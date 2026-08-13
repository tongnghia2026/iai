//! Document-wide vector style operations.

use crate::app::render::CanvasEvent;
use crate::app::state::App;
use crate::core::command::LayerStructureCommand;
use crate::core::layer::LayerType;
use crate::core::vector::color::ColorValue;
use crate::core::vector::object::VectorGeometry;
use crate::core::vector::style::{ArrowStyle, Paint, VectorStyle};
use crate::ui::intent::VectorBatchStyle;

fn style_for_scope(geometry: &VectorGeometry, spec: VectorBatchStyle) -> Option<VectorStyle> {
    match geometry {
        VectorGeometry::Primitive(shape) if spec.include_shapes => Some(shape.style),
        VectorGeometry::Path(object) => {
            let is_arrow = object.style.stroke_style.start_arrow != ArrowStyle::default()
                || object.style.stroke_style.end_arrow != ArrowStyle::default();
            if (is_arrow && spec.include_arrows) || (!is_arrow && spec.include_curves) {
                Some(object.style)
            } else {
                None
            }
        }
        _ => None,
    }
}

impl App {
    /// Change fill, outline colour and/or outline width for every vector layer
    /// in the selected classes. Shapes, arrow paths and non-arrow paths are
    /// classified independently. The derived raster cache and CMYK ink mirror
    /// are rebuilt for every changed layer, and the whole batch is one undo.
    pub fn change_document_vector_style(&mut self, spec: VectorBatchStyle) -> usize {
        if spec.is_noop() || spec.set_stroke_width.is_some_and(|w| !w.is_finite()) {
            return 0;
        }

        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let mut replacements = Vec::new();
        for (index, layer) in canvas.layer_stack.layers.iter().enumerate() {
            let LayerType::Vector(geometry) = &layer.layer_type else {
                continue;
            };
            let Some(mut style) = style_for_scope(geometry, spec) else {
                continue;
            };
            let before = style;
            if let Some(color) = spec.set_fill {
                style.fill = Paint::Solid(ColorValue::from_rgba8(color));
            }
            if let Some(color) = spec.set_stroke {
                style.stroke = Paint::Solid(ColorValue::from_rgba8(color));
            }
            if let Some(width) = spec.set_stroke_width {
                style.stroke_style.width = width.max(0.0);
            }
            if style == before {
                continue;
            }

            // Bake into a clone first so a degenerate primitive cannot leave a
            // partially-applied document if its rasterization fails.
            let mut replacement = layer.clone();
            if crate::core::command_vector::apply_style_to_layer(&mut replacement, style).is_ok() {
                replacements.push((index, replacement));
            }
        }

        if replacements.is_empty() {
            return 0;
        }

        let (cw, ch) = {
            let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
            (canvas.width, canvas.height)
        };
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let mut command = LayerStructureCommand::capture_before(
            "Format Vector Objects",
            &canvas.layer_stack,
            cw,
            ch,
        );
        let count = replacements.len();
        for (index, replacement) in replacements {
            canvas.layer_stack.layers[index] = replacement;
        }
        canvas.layer_revision += 1;
        canvas.reconcile_path_ink();
        command.capture_after(&canvas.layer_stack, cw, ch);
        canvas.record(Box::new(command));

        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::Canvas;
    use crate::core::geometry::Point;
    use crate::core::shape::{ShapeData, ShapeKind};
    use crate::core::vector::affine::AffineTransform;
    use crate::core::vector::object::VectorObjectData;
    use crate::core::vector::path::{Contour, FillRule, Node, PathData};
    use crate::core::vector::style::{ArrowHead, VectorStyle};

    fn path(points: &[(f32, f32)], closed: bool) -> PathData {
        PathData::new(
            vec![Contour::new(
                points
                    .iter()
                    .map(|&(x, y)| Node::sharp(Point::new(x, y)))
                    .collect(),
                closed,
            )],
            FillRule::NonZero,
        )
    }

    fn add_path(app: &mut App, mut style: VectorStyle, arrow: bool) -> usize {
        if arrow {
            style.stroke_style.end_arrow.kind = ArrowHead::Triangle;
        }
        let path = if arrow {
            path(&[(0.0, 0.0), (40.0, 20.0)], false)
        } else {
            path(&[(0.0, 0.0), (30.0, 0.0), (30.0, 30.0), (0.0, 30.0)], true)
        };
        let object = VectorObjectData::new(
            path,
            style,
            AffineTransform::translate(60.0, if arrow { 100.0 } else { 50.0 }),
        );
        let canvas = &mut app.docs.documents[0].canvas;
        let index = canvas.layer_stack.add_layer(canvas.width, canvas.height);
        crate::core::command_vector::apply_object_to_layer(
            &mut canvas.layer_stack.layers[index],
            object,
        );
        index
    }

    fn add_shape(app: &mut App, style: VectorStyle) -> usize {
        let (shape, offset) = ShapeData::from_canvas_span_with_style(
            ShapeKind::Rectangle,
            10.0,
            10.0,
            45.0,
            40.0,
            0.0,
            style,
        );
        let canvas = &mut app.docs.documents[0].canvas;
        let index = canvas.layer_stack.add_layer(canvas.width, canvas.height);
        let layer = &mut canvas.layer_stack.layers[index];
        layer.offset = offset;
        layer.layer_type = LayerType::Vector(VectorGeometry::Primitive(shape));
        crate::core::command_vector::apply_style_to_layer(layer, style).expect("shape raster");
        index
    }

    fn vector_style(app: &App, index: usize) -> VectorStyle {
        match &app.docs.documents[0].canvas.layer_stack.layers[index].layer_type {
            LayerType::Vector(VectorGeometry::Primitive(shape)) => shape.style,
            LayerType::Vector(VectorGeometry::Path(object)) => object.style,
            _ => panic!("expected vector layer"),
        }
    }

    fn app_with_vector_classes() -> (App, usize, usize, usize) {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(200, 200);
        let shape = add_shape(
            &mut app,
            VectorStyle::filled(ColorValue::rgb(0.1, 0.1, 0.1)),
        );
        let curve = add_path(
            &mut app,
            VectorStyle::filled(ColorValue::rgb(0.2, 0.2, 0.2)),
            false,
        );
        let arrow = add_path(
            &mut app,
            VectorStyle::stroked(ColorValue::rgb(0.3, 0.3, 0.3), 2.0),
            true,
        );
        (app, shape, curve, arrow)
    }

    #[test]
    fn batch_vector_style_changes_color_by_each_class() {
        let (mut app, shape, curve, arrow) = app_with_vector_classes();
        let red = [220, 10, 20, 255];
        assert_eq!(
            app.change_document_vector_style(VectorBatchStyle {
                include_shapes: true,
                set_fill: Some(red),
                ..Default::default()
            }),
            1
        );
        assert_eq!(
            vector_style(&app, shape).fill,
            Paint::Solid(ColorValue::from_rgba8(red))
        );

        let green = [20, 210, 30, 255];
        assert_eq!(
            app.change_document_vector_style(VectorBatchStyle {
                include_arrows: true,
                set_stroke: Some(green),
                ..Default::default()
            }),
            1
        );
        assert_eq!(
            vector_style(&app, arrow).stroke,
            Paint::Solid(ColorValue::from_rgba8(green))
        );

        let blue = [30, 40, 230, 255];
        assert_eq!(
            app.change_document_vector_style(VectorBatchStyle {
                include_curves: true,
                set_fill: Some(blue),
                ..Default::default()
            }),
            1
        );
        assert_eq!(
            vector_style(&app, curve).fill,
            Paint::Solid(ColorValue::from_rgba8(blue))
        );
    }

    #[test]
    fn batch_vector_style_skips_unchecked_classes() {
        let (mut app, shape, curve, arrow) = app_with_vector_classes();
        let before_shape = vector_style(&app, shape);
        let before_arrow = vector_style(&app, arrow);

        assert_eq!(
            app.change_document_vector_style(VectorBatchStyle {
                include_curves: true,
                set_stroke_width: Some(9.0),
                ..Default::default()
            }),
            1
        );
        assert_eq!(vector_style(&app, curve).stroke_style.width, 9.0);
        assert_eq!(vector_style(&app, shape), before_shape);
        assert_eq!(vector_style(&app, arrow), before_arrow);
    }

    #[test]
    fn batch_vector_style_is_one_undo() {
        let (mut app, shape, curve, arrow) = app_with_vector_classes();
        let before = [
            vector_style(&app, shape),
            vector_style(&app, curve),
            vector_style(&app, arrow),
        ];
        let undo_before = app.docs.documents[0].canvas.undo_count();

        assert_eq!(
            app.change_document_vector_style(VectorBatchStyle {
                include_arrows: true,
                include_shapes: true,
                include_curves: true,
                set_fill: Some([100, 110, 120, 255]),
                set_stroke: Some([10, 20, 30, 255]),
                set_stroke_width: Some(7.0),
            }),
            3
        );
        assert_eq!(app.docs.documents[0].canvas.undo_count(), undo_before + 1);

        app.docs.documents[0]
            .canvas
            .undo()
            .expect("undo vector batch");
        assert_eq!(vector_style(&app, shape), before[0]);
        assert_eq!(vector_style(&app, curve), before[1]);
        assert_eq!(vector_style(&app, arrow), before[2]);
    }
}

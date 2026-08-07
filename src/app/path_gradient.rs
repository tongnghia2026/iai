//! On-canvas transform handles for editable Path gradient fills.

use crate::app::state::{App, PathGradientDrag, PathGradientHandle};
use crate::core::geometry::Point;
use crate::core::layer::LayerType;
use crate::core::vector::affine::AffineTransform;
use crate::core::vector::object::VectorGeometry;
use crate::core::vector::style::{Gradient, GradientKind, Paint};

const HANDLE_HIT_PX: f32 = 9.0;

impl App {
    fn active_vector_gradient_layer(&self) -> Option<usize> {
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let idx = canvas.layer_stack.active_idx;
        let layer = canvas.layer_stack.layers.get(idx)?;
        (layer.visible
            && !layer.locked
            && !layer.is_background
            && layer.paint_target == crate::core::layer::PaintTarget::Pixels
            && matches!(layer.layer_type, LayerType::Vector(_)))
        .then_some(idx)
    }

    fn vector_gradient_context(
        layer: &crate::core::layer::Layer,
    ) -> Option<(Gradient, AffineTransform)> {
        match &layer.layer_type {
            LayerType::Vector(VectorGeometry::Path(object)) => {
                let Paint::Gradient(gradient) = object.style.fill else {
                    return None;
                };
                Some((gradient, object.transform))
            }
            LayerType::Vector(VectorGeometry::Primitive(shape)) => {
                let Paint::Gradient(gradient) = shape.style.fill else {
                    return None;
                };
                Some((
                    gradient,
                    AffineTransform::translate(layer.offset.0 as f32, layer.offset.1 as f32),
                ))
            }
            _ => None,
        }
    }

    /// Gradient origin, basis handles, and stop markers in canvas space.
    pub fn active_path_gradient_overlay(&self) -> Option<crate::ui::PathGradientOverlay> {
        if self.edit.tools.active_id() != crate::tools::ToolId::Gradient
            || self.edit.transform_state.is_some()
        {
            return None;
        }
        let idx = self.active_vector_gradient_layer()?;
        let layer = &self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .layers[idx];
        let (gradient, object_transform) = Self::vector_gradient_context(layer)?;
        let transform = object_transform.then(&gradient.transform);
        let center = transform.apply_point(Point::new(0.0, 0.0));
        let axis_x = transform.apply_point(Point::new(1.0, 0.0));
        let axis_y = transform.apply_point(Point::new(0.0, 1.0));
        let stops = gradient
            .active_stops()
            .iter()
            .map(|stop| transform.apply_point(Point::new(stop.offset, 0.0)))
            .map(|point| (point.x, point.y))
            .collect();
        Some(crate::ui::PathGradientOverlay {
            kind: u8::from(gradient.kind == GradientKind::Radial),
            center: (center.x, center.y),
            axis_x: (axis_x.x, axis_x.y),
            axis_y: (axis_y.x, axis_y.y),
            stops,
        })
    }

    /// Return the transform handle under a screen-space pointer.
    pub fn path_gradient_handle_at_screen(&self, sx: f32, sy: f32) -> Option<PathGradientHandle> {
        let overlay = self.active_path_gradient_overlay()?;
        let zoom = self.edit.view.zoom;
        let offset_x = self.edit.view.offset_x;
        let offset_y = self.edit.view.offset_y;
        let distance_sq = |point: (f32, f32)| {
            let dx = point.0 * zoom + offset_x - sx;
            let dy = point.1 * zoom + offset_y - sy;
            dx * dx + dy * dy
        };
        let limit = HANDLE_HIT_PX * HANDLE_HIT_PX;
        if distance_sq(overlay.axis_x) <= limit {
            return Some(PathGradientHandle::AxisX);
        }
        if overlay.kind == 1 && distance_sq(overlay.axis_y) <= limit {
            return Some(PathGradientHandle::AxisY);
        }
        (distance_sq(overlay.center) <= limit).then_some(PathGradientHandle::Center)
    }

    /// Begin a drag after normalising any residual Move-tool offset into the
    /// vector model.
    pub fn path_gradient_begin(&mut self, handle: PathGradientHandle, cx: f32, cy: f32) {
        let Some(idx) = self.active_vector_gradient_layer() else {
            return;
        };
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let layer = &mut canvas.layer_stack.layers[idx];
        if matches!(layer.layer_type, LayerType::Vector(VectorGeometry::Path(_))) {
            crate::core::command_vector::fold_offset_into_model(layer);
        }
        let layer_id = layer.id;
        let Some((gradient, object_transform)) = Self::vector_gradient_context(layer) else {
            return;
        };
        if handle == PathGradientHandle::AxisY && gradient.kind != GradientKind::Radial {
            return;
        }
        let Some(object_inverse) = object_transform.inverse() else {
            return;
        };
        self.edit.path_gradient_drag = Some(PathGradientDrag {
            layer_id,
            handle,
            original: gradient.transform,
            start_local: object_inverse.apply_point(Point::new(cx, cy)),
        });
    }

    /// Update the grabbed basis point in object-local coordinates.
    pub fn path_gradient_update(&mut self, cx: f32, cy: f32) {
        let Some(drag) = self.edit.path_gradient_drag.as_ref() else {
            return;
        };
        let layer_id = drag.layer_id;
        let handle = drag.handle;
        let original = drag.original;
        let start_local = drag.start_local;
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let Some(layer) = canvas
            .layer_stack
            .layers
            .iter()
            .find(|layer| layer.id == layer_id)
        else {
            return;
        };
        let Some((_, object_transform)) = Self::vector_gradient_context(layer) else {
            return;
        };
        let Some(object_inverse) = object_transform.inverse() else {
            return;
        };
        let current = object_inverse.apply_point(Point::new(cx, cy));
        let dx = current.x - start_local.x;
        let dy = current.y - start_local.y;
        let mut target = original;
        match handle {
            PathGradientHandle::Center => {
                target.e += dx;
                target.f += dy;
            }
            PathGradientHandle::AxisX => {
                target.a += dx;
                target.b += dy;
            }
            PathGradientHandle::AxisY => {
                target.c += dx;
                target.d += dy;
            }
        }
        if target.determinant().abs() >= 1e-4 {
            self.path_set_gradient_transform(target);
        }
    }

    /// Finish the gesture as one style-history entry.
    pub fn path_gradient_finish(&mut self) {
        if self.edit.path_gradient_drag.take().is_some() {
            self.path_style_commit();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::Canvas;
    use crate::core::command_vector::CreatePathLayer;
    use crate::core::gateway::ChangeKind;
    use crate::core::shape::{ShapeData, ShapeKind};
    use crate::core::tile::TileMap;
    use crate::core::vector::affine::AffineTransform;
    use crate::core::vector::color::ColorValue;
    use crate::core::vector::object::VectorObjectData;
    use crate::core::vector::path::{Contour, FillRule, Node, PathData};
    use crate::core::vector::style::{Gradient, VectorStyle};

    fn gradient_style() -> VectorStyle {
        let mut style = VectorStyle::default();
        style.fill = Paint::Gradient(Gradient::two_color(
            GradientKind::Linear,
            ColorValue::BLACK,
            ColorValue::WHITE,
            AffineTransform {
                a: 100.0,
                d: 100.0,
                ..AffineTransform::IDENTITY
            },
        ));
        style
    }

    fn app_with_gradient() -> (App, u32) {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(200, 200);
        app.edit.tools.select(crate::tools::ToolId::Gradient);
        let path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(100.0, 0.0)),
                    Node::sharp(Point::new(100.0, 100.0)),
                    Node::sharp(Point::new(0.0, 100.0)),
                ],
                true,
            )],
            FillRule::NonZero,
        );
        let object = VectorObjectData::new(path, gradient_style(), AffineTransform::IDENTITY);
        let id = {
            let canvas = &mut app.docs.documents[0].canvas;
            canvas
                .execute(
                    Box::new(CreatePathLayer::new(object, "Gradient")),
                    ChangeKind::LayerStructure,
                )
                .unwrap();
            let index = canvas
                .layer_stack
                .layers
                .iter()
                .position(|layer| {
                    matches!(layer.layer_type, LayerType::Vector(VectorGeometry::Path(_)))
                })
                .unwrap();
            canvas.layer_stack.active_idx = index;
            canvas.layer_stack.layers[index].id
        };
        (app, id)
    }

    fn app_with_primitive_gradient() -> (App, u32) {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(200, 200);
        app.edit.tools.select(crate::tools::ToolId::Gradient);
        let (mut shape, offset) = ShapeData::from_canvas_span_with_style(
            ShapeKind::Star,
            20.0,
            20.0,
            180.0,
            180.0,
            0.0,
            gradient_style(),
        );
        shape.sides = 5;
        shape.star_inner = 0.5;
        let raster = shape.render().expect("render star");
        let id = {
            let canvas = &mut app.docs.documents[0].canvas;
            let index = canvas.layer_stack.add_layer(canvas.width, canvas.height);
            canvas.layer_stack.active_idx = index;
            let layer = &mut canvas.layer_stack.layers[index];
            layer.tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
            layer.width = raster.width;
            layer.height = raster.height;
            layer.offset = offset;
            layer.name = "Star".to_string();
            layer.layer_type = LayerType::Vector(VectorGeometry::Primitive(shape));
            layer.id
        };
        (app, id)
    }

    fn gradient_transform(app: &App, id: u32) -> crate::core::vector::affine::AffineTransform {
        let layer = app.docs.documents[0]
            .canvas
            .layer_stack
            .layers
            .iter()
            .find(|layer| layer.id == id)
            .unwrap();
        let style = match &layer.layer_type {
            LayerType::Vector(VectorGeometry::Path(object)) => object.style,
            LayerType::Vector(VectorGeometry::Primitive(shape)) => shape.style,
            _ => panic!("not a vector"),
        };
        let Paint::Gradient(gradient) = style.fill else {
            panic!("not a gradient");
        };
        gradient.transform
    }

    #[test]
    fn dragging_gradient_center_commits_one_undo_step() {
        let (mut app, id) = app_with_gradient();
        app.path_gradient_begin(PathGradientHandle::Center, 0.0, 0.0);
        // Beginning may fold a residual raster offset into the Path model.
        // Measure after that one-time normalization; pointer updates themselves
        // must leave the cache untouched.
        let before_tiles = app.docs.documents[0]
            .canvas
            .layer_stack
            .layers
            .iter()
            .find(|layer| layer.id == id)
            .unwrap()
            .tiles
            .revision_fingerprint();
        app.path_gradient_update(20.0, 30.0);
        let during_tiles = app.docs.documents[0]
            .canvas
            .layer_stack
            .layers
            .iter()
            .find(|layer| layer.id == id)
            .unwrap()
            .tiles
            .revision_fingerprint();
        assert_eq!(
            during_tiles, before_tiles,
            "pointer moves must not rasterize the vector cache"
        );
        assert!(
            app.jobs.path_bake.is_none(),
            "gradient handle drag must not queue area-sized worker bakes"
        );
        assert!((gradient_transform(&app, id).e - 20.0).abs() < 1e-3);
        app.path_gradient_finish();
        let moved = gradient_transform(&app, id);
        assert!((moved.e - 20.0).abs() < 1e-3);
        assert!((moved.f - 30.0).abs() < 1e-3);
        let after_tiles = app.docs.documents[0]
            .canvas
            .layer_stack
            .layers
            .iter()
            .find(|layer| layer.id == id)
            .unwrap()
            .tiles
            .revision_fingerprint();
        assert_ne!(
            after_tiles, before_tiles,
            "release must rebuild the final vector cache once"
        );

        app.docs.documents[0].canvas.undo().expect("undo");
        assert_eq!(
            gradient_transform(&app, id),
            AffineTransform {
                a: 100.0,
                d: 100.0,
                ..AffineTransform::IDENTITY
            }
        );
    }

    #[test]
    fn dragging_primitive_gradient_defers_star_raster_until_release() {
        let (mut app, id) = app_with_primitive_gradient();
        app.path_gradient_begin(PathGradientHandle::Center, 20.0, 20.0);
        let before_tiles = app.docs.documents[0]
            .canvas
            .layer_stack
            .layers
            .iter()
            .find(|layer| layer.id == id)
            .unwrap()
            .tiles
            .revision_fingerprint();

        app.path_gradient_update(35.0, 45.0);
        let during_tiles = app.docs.documents[0]
            .canvas
            .layer_stack
            .layers
            .iter()
            .find(|layer| layer.id == id)
            .unwrap()
            .tiles
            .revision_fingerprint();
        assert_eq!(
            during_tiles, before_tiles,
            "Star raster must stay untouched while its gradient handle moves"
        );
        assert!((gradient_transform(&app, id).e - 15.0).abs() < 1e-3);
        assert!((gradient_transform(&app, id).f - 25.0).abs() < 1e-3);

        app.path_gradient_finish();
        let after_tiles = app.docs.documents[0]
            .canvas
            .layer_stack
            .layers
            .iter()
            .find(|layer| layer.id == id)
            .unwrap()
            .tiles
            .revision_fingerprint();
        assert_ne!(
            after_tiles, before_tiles,
            "release must rebuild the Star exactly at its final gradient"
        );
    }

    #[test]
    fn overlay_tracks_gradient_basis_and_stops() {
        let (mut app, _id) = app_with_gradient();
        let overlay = app.active_path_gradient_overlay().unwrap();
        assert_eq!(overlay.center, (0.0, 0.0));
        assert_eq!(overlay.axis_x, (100.0, 0.0));
        assert_eq!(overlay.stops, vec![(0.0, 0.0), (100.0, 0.0)]);

        app.edit.tools.select(crate::tools::ToolId::Move);
        assert!(
            app.active_path_gradient_overlay().is_none(),
            "gradient handles belong only to the Gradient Tool"
        );
    }
}

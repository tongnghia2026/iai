// Boolean / shaping commands (Phase 7): Weld / Trim / Intersect / Exclude on the
// selected vector layers. The geometry lives in `core::vector::boolean`; this is
// the App-level glue that gathers the selection, composes it in CANVAS space,
// runs the operation and replaces the selected layers with one result Path —
// all as a single undo step (mirrors `shape_ops::convert_shape_to_path`).
//
// Coordinate spaces: each vector layer is reduced to a canvas-space `PathData`
// (primitive → `from_shape`, Path → model in layer space folded with any offset
// drift). The result is stored on a new Path layer with an identity transform, so
// its raster origin (`Layer::offset`) is derived straight from the canvas-space
// geometry — the delta-0 invariant every other vector op relies on.

use super::render::CanvasEvent;
use super::state::App;
use crate::core::command::LayerStructureCommand;
use crate::core::layer::{Layer, LayerType};
use crate::core::vector::affine::AffineTransform;
use crate::core::vector::boolean::{boolean_many, BooleanOp};
use crate::core::vector::from_shape;
use crate::core::vector::object::{VectorGeometry, VectorObjectData};
use crate::core::vector::path::PathData;
use crate::core::vector::raster;
use crate::core::vector::style::VectorStyle;

/// A vector layer eligible for a boolean: selected, unlocked, not the background,
/// and holding either a parametric primitive or a Bézier path.
fn is_boolean_target(layer: &Layer) -> bool {
    layer.selected
        && !layer.locked
        && !layer.is_background
        && matches!(layer.layer_type, LayerType::Vector(_))
}

/// Reduce one vector layer to a canvas-space fill path plus its style. Returns
/// `None` for non-vector layers.
fn layer_canvas_path(layer: &Layer) -> Option<(PathData, VectorStyle)> {
    match &layer.layer_type {
        LayerType::Vector(VectorGeometry::Path(obj)) => {
            // The model lives in layer space; `Layer::offset` is normally its
            // derived raster origin (delta-0). Fold any residual drag drift so the
            // path we compose is in true canvas space.
            let mut p = obj.path_in_layer_space();
            if let Some((origin, _, _)) = raster::raster_geometry(obj) {
                let dx = (layer.offset.0 - origin.0) as f32;
                let dy = (layer.offset.1 - origin.1) as f32;
                if dx != 0.0 || dy != 0.0 {
                    p.transform(&AffineTransform::translate(dx, dy));
                }
            }
            Some((p, obj.style))
        }
        LayerType::Vector(VectorGeometry::Primitive(shape)) => {
            let (x0, y0, x1, y1) = shape.canvas_span(layer.offset);
            use crate::core::shape::ShapeKind;
            let path = match shape.kind {
                ShapeKind::Rectangle => {
                    from_shape::rect_path(x0, y0, x1, y1, shape.effective_radius())
                }
                ShapeKind::Ellipse => from_shape::ellipse_path(x0, y0, x1, y1),
                ShapeKind::Line => from_shape::line_path(x0, y0, x1, y1),
                ShapeKind::Polygon => from_shape::polygon_path(x0, y0, x1, y1, shape.sides),
                ShapeKind::Star => {
                    from_shape::star_path(x0, y0, x1, y1, shape.sides, shape.star_inner)
                }
            };
            Some((path, shape.style))
        }
        _ => None,
    }
}

impl App {
    /// Number of vector layers currently eligible for a boolean. The menu enables
    /// the shaping items only when this is at least 2.
    pub fn boolean_selection_count(&self) -> usize {
        self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .layers
            .iter()
            .filter(|l| is_boolean_target(l))
            .count()
    }

    /// Apply a boolean/shaping operation to the selected vector layers, replacing
    /// them with a single result Path layer. Returns `false` (with a status
    /// message) when there is nothing valid to do, leaving the document untouched.
    pub fn apply_boolean(&mut self, op: BooleanOp) -> bool {
        // Selected vector layers, bottom-to-top (index 0 is the bottom layer).
        let indices: Vec<usize> = {
            let stack = &self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack;
            stack
                .layers
                .iter()
                .enumerate()
                .filter(|(_, l)| is_boolean_target(l))
                .map(|(i, _)| i)
                .collect()
        };
        if indices.len() < 2 {
            self.shell.status_msg = "Chọn ít nhất 2 đối tượng vector để kết hợp hình".to_string();
            return false;
        }

        // Gather each layer's canvas-space path in z-order. For Trim the bottom
        // object is the one that survives, so it must come first.
        let paths: Vec<PathData> = {
            let stack = &self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack;
            indices
                .iter()
                .filter_map(|&i| layer_canvas_path(&stack.layers[i]).map(|(p, _)| p))
                .collect()
        };
        if paths.len() < 2 {
            self.shell.status_msg = "Không đủ đối tượng vector hợp lệ".to_string();
            return false;
        }

        // Style/visual identity of the result = the bottom-most selected object
        // (CorelDRAW's "result takes the target's properties").
        let target_idx = indices[0];
        let target_style = {
            let stack = &self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack;
            layer_canvas_path(&stack.layers[target_idx])
                .map(|(_, s)| s)
                .unwrap_or_default()
        };

        let refs: Vec<&PathData> = paths.iter().collect();
        let result = match boolean_many(&refs, op) {
            Some(r) => r,
            None => {
                self.shell.status_msg =
                    "Không thực hiện được phép kết hợp (hình quá phức tạp)".to_string();
                return false;
            }
        };
        if result.contours.is_empty() {
            self.shell.status_msg = match op {
                BooleanOp::Intersect => "Không có phần giao nhau".to_string(),
                BooleanOp::Difference => "Kết quả rỗng (bị cắt hết)".to_string(),
                _ => "Kết quả rỗng".to_string(),
            };
            return false;
        }
        let object = VectorObjectData::new(result, target_style, AffineTransform::IDENTITY);
        if object.validate().is_err() {
            self.shell.status_msg = "Kết quả không hợp lệ".to_string();
            return false;
        }

        let name = match op {
            BooleanOp::Union => "Hàn",
            BooleanOp::Intersect => "Giao",
            BooleanOp::Difference => "Cắt",
            BooleanOp::Exclude => "Loại trừ",
        };

        let (cw, ch) = {
            let d = &self.docs.documents[self.docs.active_doc_idx];
            (d.canvas.width, d.canvas.height)
        };
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let before = LayerStructureCommand::capture_before(name, &canvas.layer_stack, cw, ch);

        // Build the result layer by inheriting the target's visual properties
        // (opacity, blend, visibility, group membership), then swap in the boolean
        // geometry. Cloning is the simplest way to carry those props across.
        let new_id = canvas.layer_stack.next_id();
        canvas.layer_stack.set_next_id(new_id + 1);
        let mut new_layer = canvas.layer_stack.layers[target_idx].clone();
        new_layer.id = new_id;
        new_layer.name = name.to_string();
        new_layer.mask = None;
        new_layer.selected = true;
        crate::core::command_vector::apply_object_to_layer(&mut new_layer, object);

        // Remove the selected layers (highest index first so lower indices stay
        // valid), then insert the result where the bottom object was.
        let mut desc = indices.clone();
        desc.sort_unstable_by(|a, b| b.cmp(a));
        for &i in &desc {
            canvas.layer_stack.layers.remove(i);
        }
        let insert_at = target_idx.min(canvas.layer_stack.layers.len());
        canvas.layer_stack.layers.insert(insert_at, new_layer);
        canvas.layer_stack.active_idx = insert_at;
        for (i, l) in canvas.layer_stack.layers.iter_mut().enumerate() {
            l.selected = i == insert_at;
        }

        // CMYK: re-derive ink planes for the new Path raster from its RGB mirror.
        canvas.reconcile_path_ink();
        canvas.layer_revision += 1;

        let mut cmd = before;
        cmd.capture_after(&canvas.layer_stack, cw, ch);
        canvas.record(Box::new(cmd));

        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.apply_canvas_event(CanvasEvent::SelectionChanged);
        self.shell.status_msg = format!(
            "Đã {} {} đối tượng vector",
            name.to_lowercase(),
            indices.len()
        );
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::Canvas;
    use crate::core::shape::{ShapeData, ShapeKind};
    use crate::core::vector::flatten::flatten_path;
    use crate::core::vector::path::FillRule;

    fn rect_shape(x0: f32, y0: f32, x1: f32, y1: f32) -> ShapeData {
        ShapeData {
            kind: ShapeKind::Rectangle,
            x0,
            y0,
            x1,
            y1,
            corner_radius: 0.0,
            sides: 5,
            star_inner: 0.5,
            style: VectorStyle::from_shape_fields(true, [200, 40, 40, 255], 0.0, [0, 0, 0, 0]),
        }
    }

    /// App with two overlapping rectangle primitives selected.
    fn app_with_two_rects() -> App {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(200, 200);
        let canvas = &mut app.docs.documents[0].canvas;
        for (i, (x0, y0)) in [(20.0, 20.0), (60.0, 60.0)].into_iter().enumerate() {
            let idx = canvas.layer_stack.add_layer(200, 200);
            let layer = &mut canvas.layer_stack.layers[idx];
            layer.offset = (0, 0);
            layer.layer_type = LayerType::Vector(VectorGeometry::Primitive(rect_shape(
                x0,
                y0,
                x0 + 80.0,
                y0 + 80.0,
            )));
            layer.selected = true;
            let _ = i;
        }
        app
    }

    fn active_path_area(app: &App) -> f32 {
        let canvas = &app.docs.documents[0].canvas;
        let l = &canvas.layer_stack.layers[canvas.layer_stack.active_idx];
        let LayerType::Vector(VectorGeometry::Path(obj)) = &l.layer_type else {
            panic!("active layer is not a Path");
        };
        // Approximate filled area of the result via grid sampling of its flatten.
        let polys = flatten_path(&obj.path_in_layer_space(), 0.3);
        if polys.is_empty() {
            return 0.0;
        }
        let (mut minx, mut miny, mut maxx, mut maxy) = (
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        );
        for r in &polys {
            for p in r {
                minx = minx.min(p.x);
                miny = miny.min(p.y);
                maxx = maxx.max(p.x);
                maxy = maxy.max(p.y);
            }
        }
        // Even-odd point membership using ray casting over all rings.
        let inside = |x: f32, y: f32| {
            let mut parity = 0i32;
            for r in &polys {
                let n = r.len();
                for i in 0..n {
                    let a = r[i];
                    let b = r[(i + 1) % n];
                    let (lo, hi) = if a.y <= b.y { (a, b) } else { (b, a) };
                    if y >= lo.y && y < hi.y {
                        let t = (y - lo.y) / (hi.y - lo.y);
                        if lo.x + t * (hi.x - lo.x) > x {
                            parity ^= 1;
                        }
                    }
                }
            }
            let _ = obj.path.fill_rule == FillRule::EvenOdd;
            parity != 0
        };
        let step = 1.0;
        let mut area = 0.0;
        let mut y = miny + 0.5;
        while y < maxy {
            let mut x = minx + 0.5;
            while x < maxx {
                if inside(x, y) {
                    area += step * step;
                }
                x += step;
            }
            y += step;
        }
        area
    }

    #[test]
    fn weld_two_rects_makes_one_path() {
        let mut app = app_with_two_rects();
        let before_layers = app.docs.documents[0].canvas.layer_stack.layers.len();
        assert!(app.apply_boolean(BooleanOp::Union));
        let canvas = &app.docs.documents[0].canvas;
        // Two vector layers collapsed into one → one fewer layer.
        assert_eq!(canvas.layer_stack.layers.len(), before_layers - 1);
        // Union area = 80² + 80² − 40² overlap = 6400 + 6400 − 1600 = 11200.
        let area = active_path_area(&app);
        assert!((area - 11200.0).abs() < 400.0, "weld area {area}");
    }

    #[test]
    fn weld_then_undo_restores_both_layers() {
        let mut app = app_with_two_rects();
        let before = app.docs.documents[0].canvas.layer_stack.layers.len();
        assert!(app.apply_boolean(BooleanOp::Union));
        app.docs.documents[0].canvas.undo().expect("undo");
        assert_eq!(
            app.docs.documents[0].canvas.layer_stack.layers.len(),
            before,
            "undo brings both source layers back"
        );
    }

    #[test]
    fn intersect_keeps_only_overlap() {
        let mut app = app_with_two_rects();
        assert!(app.apply_boolean(BooleanOp::Intersect));
        // Overlap of the two 80×80 rects offset by (40,40) = 40×40 = 1600.
        let area = active_path_area(&app);
        assert!((area - 1600.0).abs() < 200.0, "intersect area {area}");
    }

    #[test]
    fn boolean_needs_two_selected() {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(100, 100);
        let canvas = &mut app.docs.documents[0].canvas;
        let idx = canvas.layer_stack.add_layer(100, 100);
        canvas.layer_stack.layers[idx].layer_type =
            LayerType::Vector(VectorGeometry::Primitive(rect_shape(0.0, 0.0, 10.0, 10.0)));
        canvas.layer_stack.layers[idx].selected = true;
        assert!(
            !app.apply_boolean(BooleanOp::Union),
            "one object is not enough"
        );
    }
}

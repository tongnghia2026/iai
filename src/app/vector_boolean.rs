// Boolean / shaping commands: Weld / Trim / Intersect / Simplify on the
// selected vector layers. The geometry lives in `core::vector::boolean`; this is
// the App-level glue that gathers the selection, composes it in CANVAS space,
// runs the operation and updates the layer stack per-op (see `apply_boolean`) —
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
use crate::core::layer::LayerType;
use crate::core::vector::affine::AffineTransform;
use crate::core::vector::boolean::{BooleanOp, boolean};
use crate::core::vector::from_shape;
use crate::core::vector::object::VectorObjectData;
use crate::core::vector::path::PathData;

use iai_canvas::shaping::{is_boolean_target, layer_canvas_object};

impl App {
    /// Number of vector layers currently eligible for a boolean. The menu enables
    /// the shaping items only when this is at least 2.
    pub fn boolean_selection_count(&self) -> usize {
        self.docs
            .active()
            .canvas
            .layer_stack
            .layers
            .iter()
            .filter(|l| is_boolean_target(l))
            .count()
    }

    /// Apply a boolean/shaping operation to the selected vector layers. Returns
    /// `false` (with a status message) when there is nothing valid to do, leaving
    /// the document untouched. The whole thing is one undo step.
    ///
    /// CorelDRAW-compatible layer policy:
    ///   Weld (Union)  → merge every source into ONE result (sources removed).
    ///   Trim (Diff)   → bottom target minus uppers; sources stay unchanged.
    ///   Intersect     → KEEP every source; add the overlap as a NEW object on top.
    ///   Simplify      → preserve the visible stack: each lower object loses the
    ///                   area hidden by selected objects above it.
    pub fn apply_boolean(&mut self, op: BooleanOp) -> bool {
        self.run_with("vector.shaping", crate::ui::shaping_args(op))
            .is_ok()
    }

    /// Whether the active layer can be trimmed by the current selection: there is
    /// an active selection AND the active layer is an unlocked, non-background
    /// vector layer. Gates the Delete-key Trim branch (kept cheap — no mask scan).
    pub fn can_trim_active_vector(&self) -> bool {
        let canvas = &self.docs.active().canvas;
        if !canvas.selection.active {
            return false;
        }
        match canvas.layer_stack.layers.get(canvas.layer_stack.active_idx) {
            Some(l) => {
                !l.locked && !l.is_background && matches!(l.layer_type, LayerType::Vector(_))
            }
            None => false,
        }
    }

    /// Trim the active vector layer with the current selection — the vector-layer
    /// meaning of Delete-with-a-selection.
    ///
    /// CLOSED (filled / outline) contours are carved by the boolean Difference
    /// against a cutter built from the selection: a solid rectangular marquee maps
    /// exactly to its bounding box, while an ellipse / lasso / wand selection is
    /// traced from its coverage mask into a smooth curved cutter. The kept shape
    /// stays an editable curve path (re-fit by the boolean engine, like the object
    /// Trim in [`App::apply_boolean`]).
    ///
    /// OPEN contours (pen lines, the Line primitive, Vector Brush centerlines) are
    /// cut where they cross the selection into separate open sub-paths; a cut
    /// Vector Brush keeps its variable width (profile re-sliced per piece), each
    /// piece on its own layer. One undo step. Returns true when it handled the key.
    pub fn trim_active_vector_by_selection(&mut self) -> bool {
        use crate::core::geometry::Point;
        use crate::core::vector::path::{Contour, FillRule};
        use crate::core::vector::segment_cut::{OpenPiece, cut_open_contour};

        let doc = self.docs.active_doc_idx;
        let target_idx = self.docs.documents[doc].canvas.layer_stack.active_idx;

        // Guard: the active layer must be an unlocked, non-background vector.
        match self.docs.documents[doc]
            .canvas
            .layer_stack
            .layers
            .get(target_idx)
        {
            Some(l) if matches!(l.layer_type, LayerType::Vector(_)) => {
                if l.locked || l.is_background {
                    self.shell.status_msg = "Layer is locked".to_string();
                    return true;
                }
            }
            _ => return false,
        }

        // Raster bounds of the active layer — a cheap overlap gate.
        let layer_bounds = {
            let l = &self.docs.documents[doc].canvas.layer_stack.layers[target_idx];
            (
                l.offset.0 as f32,
                l.offset.1 as f32,
                (l.offset.0 + l.width as i32) as f32,
                (l.offset.1 + l.height as i32) as f32,
            )
        };

        // Build the cutter from the selection: a solid rectangular marquee maps
        // exactly to its bounding box; any other shape (ellipse / lasso / wand) is
        // traced from its coverage mask into a smooth curved cutter.
        let cutter = {
            let canvas = &mut self.docs.documents[doc].canvas;
            if !canvas.selection.active {
                return false;
            }
            let (bx0, by0, bx1, by1) = canvas.selection.bounding_box();
            if bx1 - bx0 < 1.0 || by1 - by0 < 1.0 {
                return false;
            }
            // Nothing to do if the selection doesn't overlap the shape at all.
            let (lx0, ly0, lx1, ly1) = layer_bounds;
            if bx1 <= lx0 || bx0 >= lx1 || by1 <= ly0 || by0 >= ly1 {
                self.shell.status_msg = "Selection doesn't overlap the shape".to_string();
                return true;
            }
            if canvas.selection.is_solid_rect() {
                from_shape::rect_path(bx0, by0, bx1, by1, 0.0)
            } else {
                let sel = &canvas.selection;
                let off = sel.offset;
                // Selection bbox in MASK space (canvas bbox minus the mask origin).
                let scan = (
                    (bx0 as i32 - off.0).max(0) as u32,
                    (by0 as i32 - off.1).max(0) as u32,
                    (bx1 as i32 - off.0).max(0) as u32,
                    (by1 as i32 - off.1).max(0) as u32,
                );
                match crate::core::vector::mask_trace::trace_mask_to_path(
                    &sel.mask, sel.width, sel.height, off, scan, 128,
                ) {
                    Some(p) => p,
                    None => {
                        self.shell.status_msg = "Empty selection".to_string();
                        return true;
                    }
                }
            }
        };

        // The active object in canvas space: fill path, style, and (canvas-scaled)
        // brush when it is a Vector Brush stroke.
        let Some((path, style, brush)) =
            layer_canvas_object(&self.docs.documents[doc].canvas.layer_stack.layers[target_idx])
        else {
            return false;
        };

        // Partition into CLOSED contours (carved by the boolean) and OPEN contours
        // (cut into pieces where they cross the selection).
        let mut closed: Vec<Contour> = Vec::new();
        let mut open: Vec<Contour> = Vec::new();
        for c in &path.contours {
            if c.closed {
                closed.push(c.clone());
            } else {
                open.push(c.clone());
            }
        }

        // Carve the closed part with the cutter.
        let had_closed = !closed.is_empty();
        let mut closed_result: Vec<Contour> = Vec::new();
        if had_closed {
            let closed_path = PathData::new(closed, path.fill_rule);
            if let Some(r) = boolean(&closed_path, &cutter, BooleanOp::Difference)
                .filter(|r| !r.contours.is_empty())
            {
                closed_result = r.contours;
            }
        }

        // Cut the open contours against the true selection region.
        let open_pieces: Vec<OpenPiece> = {
            let sel = &self.docs.documents[doc].canvas.selection;
            let inside = |p: Point| {
                if p.x < 0.0 || p.y < 0.0 {
                    false
                } else {
                    sel.is_selected(p.x.round() as u32, p.y.round() as u32)
                }
            };
            open.iter()
                .flat_map(|c| cut_open_contour(c, &inside))
                .collect()
        };

        let (cw, ch) = {
            let d = &self.docs.documents[doc];
            (d.canvas.width, d.canvas.height)
        };

        // ── Vector Brush: each surviving piece becomes its own brush layer, so the
        //    variable width is preserved (the ribbon rasteriser is one-contour). ──
        if let Some(canvas_brush) = brush {
            let canvas = &mut self.docs.documents[doc].canvas;
            let before = LayerStructureCommand::capture_before("Trim", &canvas.layer_stack, cw, ch);
            let orig_id = canvas.layer_stack.layers[target_idx].id;
            let template = canvas.layer_stack.layers[target_idx].clone();
            let removed = open_pieces.is_empty();

            canvas.layer_stack.layers.remove(target_idx);
            for (k, piece) in open_pieces.iter().enumerate() {
                let mut nl = template.clone();
                nl.id = if k == 0 {
                    orig_id
                } else {
                    let id = canvas.layer_stack.next_id();
                    canvas.layer_stack.set_next_id(id + 1);
                    id
                };
                nl.mask = None;
                let sliced = canvas_brush.sliced(piece.arc0, piece.arc1);
                let obj = VectorObjectData::new_brush(
                    PathData::new(vec![piece.contour.clone()], FillRule::NonZero),
                    style,
                    AffineTransform::IDENTITY,
                    sliced,
                );
                crate::core::command_vector::apply_object_to_layer(&mut nl, obj);
                let at = (target_idx + k).min(canvas.layer_stack.layers.len());
                canvas.layer_stack.layers.insert(at, nl);
            }
            canvas.layer_stack.active_idx =
                target_idx.min(canvas.layer_stack.layers.len().saturating_sub(1));
            let active = canvas.layer_stack.active_idx;
            for (i, layer) in canvas.layer_stack.layers.iter_mut().enumerate() {
                layer.selected = i == active;
            }
            canvas.reconcile_path_ink();
            canvas.layer_revision += 1;
            let mut cmd = before;
            cmd.capture_after(&canvas.layer_stack, cw, ch);
            canvas.record(Box::new(cmd));

            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            self.shell.status_msg = if removed {
                "Trimmed the whole stroke away".to_string()
            } else {
                format!("Cut the stroke into {} piece(s)", open_pieces.len())
            };
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return true;
        }

        // ── Plain path: one layer holding the carved closed part plus every open
        //    stroke piece (multiple open sub-paths coexist on one layer). ──
        let mut result_contours = closed_result;
        for piece in &open_pieces {
            result_contours.push(piece.contour.clone());
        }
        // The carved closed part relies on EvenOdd (holes); open strokes don't fill,
        // so EvenOdd is safe for them too. With no closed part, keep the original.
        let result_fill = if had_closed {
            FillRule::EvenOdd
        } else {
            path.fill_rule
        };

        let object = if result_contours.is_empty() {
            None
        } else {
            let obj = VectorObjectData::new(
                PathData::new(result_contours, result_fill),
                style,
                AffineTransform::IDENTITY,
            );
            if obj.validate().is_err() {
                self.shell.status_msg = "Invalid result".to_string();
                return true;
            }
            Some(obj)
        };

        let canvas = &mut self.docs.documents[doc].canvas;
        let before = LayerStructureCommand::capture_before("Trim", &canvas.layer_stack, cw, ch);
        let removed = object.is_none();
        match object {
            Some(object) => {
                crate::core::command_vector::apply_object_to_layer(
                    &mut canvas.layer_stack.layers[target_idx],
                    object,
                );
                canvas.layer_stack.active_idx = target_idx;
            }
            None => {
                // The selection covered the whole shape — drop the now-empty layer
                // (matches the object Trim, which removes a fully-trimmed target).
                canvas.layer_stack.layers.remove(target_idx);
                canvas.layer_stack.active_idx =
                    target_idx.min(canvas.layer_stack.layers.len().saturating_sub(1));
            }
        }
        let active = canvas.layer_stack.active_idx;
        for (i, layer) in canvas.layer_stack.layers.iter_mut().enumerate() {
            layer.selected = i == active;
        }

        // CMYK: re-derive ink planes for the edited Path raster from the RGB mirror.
        canvas.reconcile_path_ink();
        canvas.layer_revision += 1;

        let mut cmd = before;
        cmd.capture_after(&canvas.layer_stack, cw, ch);
        canvas.record(Box::new(cmd));

        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.shell.status_msg = if removed {
            "Trimmed the whole shape away".to_string()
        } else {
            "Trimmed the selected region".to_string()
        };
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
    use crate::core::geometry::Point;
    use crate::core::shape::{ShapeData, ShapeKind};
    use crate::core::vector::color::ColorValue;
    use crate::core::vector::flatten::flatten_path;
    use crate::core::vector::object::VectorGeometry;
    use crate::core::vector::path::{Contour, FillRule, Node};
    use crate::core::vector::style::VectorStyle;
    use crate::core::vector::style::{LineCap, Paint};

    fn rect_shape(x0: f32, y0: f32, x1: f32, y1: f32) -> ShapeData {
        ShapeData {
            kind: ShapeKind::Rectangle,
            x0,
            y0,
            x1,
            y1,
            corner_radius: 0.0,
            corner_type: crate::core::vector::from_shape::RectCorner::Round,
            sides: 5,
            star_inner: 0.5,
            style: VectorStyle::from_shape_fields(true, [200, 40, 40, 255], 0.0, [0, 0, 0, 0]),
        }
    }

    /// App with two overlapping rectangle primitives selected.
    fn app_with_two_rects() -> App {
        app_with_rects(&[(20.0, 20.0, 100.0, 100.0), (60.0, 60.0, 140.0, 140.0)])
    }

    fn app_with_rects(rects: &[(f32, f32, f32, f32)]) -> App {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(200, 200);
        let canvas = &mut app.docs.documents[0].canvas;
        for &(x0, y0, x1, y1) in rects {
            let idx = canvas.layer_stack.add_layer(200, 200);
            let layer = &mut canvas.layer_stack.layers[idx];
            layer.offset = (0, 0);
            layer.layer_type =
                LayerType::Vector(VectorGeometry::Primitive(rect_shape(x0, y0, x1, y1)));
            layer.selected = true;
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
    fn intersect_adds_overlap_and_keeps_sources() {
        let mut app = app_with_two_rects();
        let before = app.docs.documents[0].canvas.layer_stack.layers.len();
        assert!(
            app.apply_boolean(BooleanOp::Intersect),
            "{}",
            app.shell.status_msg
        );
        // Sources are kept; the overlap is a NEW object → one more layer.
        assert_eq!(
            app.docs.documents[0].canvas.layer_stack.layers.len(),
            before + 1,
            "intersect keeps the sources and adds the overlap"
        );
        // Overlap of the two 80×80 rects offset by (40,40) = 40×40 = 1600.
        let area = active_path_area(&app);
        assert!((area - 1600.0).abs() < 200.0, "intersect area {area}");
    }

    #[test]
    fn trim_changes_only_bottom_target_and_keeps_cutters() {
        let mut app = app_with_two_rects();
        let before = app.docs.documents[0].canvas.layer_stack.layers.len();
        assert!(app.apply_boolean(BooleanOp::Difference));
        // Corel Trim changes only the bottom target; source layers remain.
        assert_eq!(
            app.docs.documents[0].canvas.layer_stack.layers.len(),
            before,
            "trim keeps every source layer"
        );
        // Bottom (80×80) minus the 40×40 overlap = 4800.
        let area = active_path_area(&app);
        assert!((area - 4800.0).abs() < 300.0, "trim area {area}");
    }

    #[test]
    fn simplify_cuts_only_the_hidden_part_of_the_lower_layer() {
        let mut app = app_with_two_rects();
        let before = app.docs.documents[0].canvas.layer_stack.layers.len();
        assert!(app.apply_boolean(BooleanOp::Exclude));
        // Both layers survive; only the lower one loses the covered area.
        assert_eq!(
            app.docs.documents[0].canvas.layer_stack.layers.len(),
            before,
            "simplify keeps each visible layer separate"
        );
        // Active is the bottom survivor = A − B = 6400 − 1600 = 4800.
        let area = active_path_area(&app);
        assert!(
            (area - 4800.0).abs() < 300.0,
            "simplified bottom area {area}"
        );
        // The lower edited layer is a Path; the untouched top stays primitive.
        let canvas = &app.docs.documents[0].canvas;
        let path_count = canvas
            .layer_stack
            .layers
            .iter()
            .filter(|l| matches!(l.layer_type, LayerType::Vector(VectorGeometry::Path(_))))
            .count();
        assert!(
            path_count >= 1,
            "the edited lower source became a Path, got {path_count}"
        );
    }

    #[test]
    fn intersect_three_rects_includes_the_three_way_overlap_once() {
        let mut app = app_with_rects(&[
            (20.0, 20.0, 100.0, 100.0),
            (40.0, 20.0, 120.0, 100.0),
            (60.0, 20.0, 140.0, 100.0),
        ]);
        assert!(
            app.apply_boolean(BooleanOp::Intersect),
            "{}",
            app.shell.status_msg
        );
        let area = active_path_area(&app);
        // Coverage >= 2 spans x=40..120. The x=60..100 three-way region is
        // included once inside that union, not emitted as a duplicate contour.
        assert!((area - 6400.0).abs() < 300.0, "multi-intersect area {area}");
    }

    #[test]
    fn intersect_three_rects_keeps_pairwise_overlaps_without_a_three_way_overlap() {
        let mut app = app_with_rects(&[
            (0.0, 20.0, 80.0, 100.0),
            (40.0, 20.0, 120.0, 100.0),
            (80.0, 20.0, 160.0, 100.0),
        ]);
        let before = app.docs.documents[0].canvas.layer_stack.layers.len();
        assert!(
            app.apply_boolean(BooleanOp::Intersect),
            "{}",
            app.shell.status_msg
        );
        assert_eq!(
            app.docs.documents[0].canvas.layer_stack.layers.len(),
            before + 1,
            "the sources stay and one overlap object is added"
        );
        let area = active_path_area(&app);
        assert!(
            (area - 6400.0).abs() < 300.0,
            "pairwise-overlap area {area}"
        );
    }

    #[test]
    fn weld_three_rects_unions_every_selected_layer() {
        let mut app = app_with_rects(&[
            (20.0, 20.0, 100.0, 100.0),
            (40.0, 20.0, 120.0, 100.0),
            (60.0, 20.0, 140.0, 100.0),
        ]);
        assert!(
            app.apply_boolean(BooleanOp::Union),
            "{}",
            app.shell.status_msg
        );
        let area = active_path_area(&app);
        assert!((area - 9600.0).abs() < 300.0, "three-way weld area {area}");
    }

    #[test]
    fn trim_three_rects_subtracts_every_upper_layer_from_the_bottom() {
        let mut app = app_with_rects(&[
            (20.0, 20.0, 100.0, 100.0),
            (40.0, 20.0, 120.0, 100.0),
            (60.0, 20.0, 140.0, 100.0),
        ]);
        assert!(
            app.apply_boolean(BooleanOp::Difference),
            "{}",
            app.shell.status_msg
        );
        let area = active_path_area(&app);
        assert!((area - 1600.0).abs() < 200.0, "three-way trim area {area}");
    }

    #[test]
    fn trim_uses_every_disjoint_cutter_above_the_target() {
        let mut app = app_with_rects(&[
            (10.0, 10.0, 190.0, 190.0),
            (30.0, 30.0, 70.0, 70.0),
            (120.0, 120.0, 160.0, 160.0),
        ]);
        let before = app.docs.documents[0].canvas.layer_stack.layers.len();
        assert!(
            app.apply_boolean(BooleanOp::Difference),
            "{}",
            app.shell.status_msg
        );
        assert_eq!(
            app.docs.documents[0].canvas.layer_stack.layers.len(),
            before,
            "both cutter layers stay in the stack"
        );
        let area = active_path_area(&app);
        assert!(
            (area - 29200.0).abs() < 500.0,
            "target minus two disjoint cutters area {area}"
        );
    }

    #[test]
    fn simplify_three_rects_preserves_the_visible_z_order_parts() {
        let mut app = app_with_rects(&[
            (20.0, 20.0, 100.0, 100.0),
            (40.0, 20.0, 120.0, 100.0),
            (60.0, 20.0, 140.0, 100.0),
        ]);
        let before = app.docs.documents[0].canvas.layer_stack.layers.len();
        assert!(
            app.apply_boolean(BooleanOp::Exclude),
            "{}",
            app.shell.status_msg
        );
        let canvas = &app.docs.documents[0].canvas;
        assert_eq!(
            canvas.layer_stack.layers.len(),
            before,
            "all three rectangles still have a visible part"
        );
        let area = active_path_area(&app);
        assert!(
            (area - 1600.0).abs() < 200.0,
            "three-way simplify bottom area {area}"
        );
    }

    /// One rectangle-primitive vector layer on a 200×200 canvas, made active,
    /// with a rectangular marquee selection already set. Returns the app and the
    /// active layer index.
    fn app_with_one_rect_and_rect_selection(
        shape: (f32, f32, f32, f32),
        sel: (u32, u32, u32, u32),
    ) -> (App, usize) {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(200, 200);
        let canvas = &mut app.docs.documents[0].canvas;
        let idx = canvas.layer_stack.add_layer(200, 200);
        let layer = &mut canvas.layer_stack.layers[idx];
        layer.offset = (0, 0);
        layer.layer_type = LayerType::Vector(VectorGeometry::Primitive(rect_shape(
            shape.0, shape.1, shape.2, shape.3,
        )));
        layer.selected = true;
        canvas.layer_stack.active_idx = idx;
        canvas.selection.select_rect(sel.0, sel.1, sel.2, sel.3);
        (app, idx)
    }

    #[test]
    fn trim_by_rect_selection_cuts_the_region_from_active_vector() {
        // 100×100 rect; marquee over the right half (x 70..120, y 20..120).
        let (mut app, idx) =
            app_with_one_rect_and_rect_selection((20.0, 20.0, 120.0, 120.0), (70, 20, 120, 120));
        let before = app.docs.documents[0].canvas.layer_stack.layers.len();
        assert!(
            app.trim_active_vector_by_selection(),
            "{}",
            app.shell.status_msg
        );
        // Partial trim keeps the layer; it is now an editable Path.
        assert_eq!(
            app.docs.documents[0].canvas.layer_stack.layers.len(),
            before
        );
        let l = &app.docs.documents[0].canvas.layer_stack.layers[idx];
        assert!(
            matches!(l.layer_type, LayerType::Vector(VectorGeometry::Path(_))),
            "trimmed primitive should become a Path"
        );
        // Remaining = left half: 50 wide × 100 tall = 5000.
        let area = active_path_area(&app);
        assert!((area - 5000.0).abs() < 400.0, "trim area {area}");
    }

    #[test]
    fn trim_covering_the_whole_shape_removes_the_layer() {
        let (mut app, _idx) =
            app_with_one_rect_and_rect_selection((20.0, 20.0, 120.0, 120.0), (0, 0, 200, 200));
        let before = app.docs.documents[0].canvas.layer_stack.layers.len();
        assert!(
            app.trim_active_vector_by_selection(),
            "{}",
            app.shell.status_msg
        );
        assert_eq!(
            app.docs.documents[0].canvas.layer_stack.layers.len(),
            before - 1,
            "a fully-covered shape is removed"
        );
    }

    #[test]
    fn trim_by_ellipse_selection_cuts_a_curved_bite() {
        // 100×100 filled rect at (20,20)-(120,120); a round selection over its
        // centre. Not a solid rectangle → the cutter is traced from the mask.
        let (mut app, idx) =
            app_with_one_rect_and_rect_selection((20.0, 20.0, 120.0, 120.0), (0, 0, 1, 1));
        {
            let sel = &mut app.docs.documents[0].canvas.selection;
            let (w, h) = (sel.width, sel.height);
            for p in sel.mask.iter_mut() {
                *p = 0;
            }
            // Disk radius 25 at (70,70) — fully inside the rectangle.
            for y in 0..h {
                for x in 0..w {
                    let dx = x as f32 + 0.5 - 70.0;
                    let dy = y as f32 + 0.5 - 70.0;
                    if dx * dx + dy * dy <= 25.0 * 25.0 {
                        sel.mask[(y * w + x) as usize] = 255;
                    }
                }
            }
            sel.active = true;
            sel.mark_bbox_dirty();
        }
        assert!(
            app.trim_active_vector_by_selection(),
            "{}",
            app.shell.status_msg
        );
        let l = &app.docs.documents[0].canvas.layer_stack.layers[idx];
        assert!(
            matches!(l.layer_type, LayerType::Vector(VectorGeometry::Path(_))),
            "trimmed shape becomes a Path"
        );
        // Remaining ≈ 10000 − π·25² ≈ 8037.
        let area = active_path_area(&app);
        let expected = 10000.0 - std::f32::consts::PI * 25.0 * 25.0;
        assert!(
            (area - expected).abs() < 500.0,
            "ellipse-trim area {area} vs {expected}"
        );
    }

    #[test]
    fn trim_then_undo_restores_the_original_primitive() {
        let (mut app, idx) =
            app_with_one_rect_and_rect_selection((20.0, 20.0, 120.0, 120.0), (70, 20, 120, 120));
        assert!(app.trim_active_vector_by_selection());
        // After the trim the layer is a Path.
        assert!(matches!(
            app.docs.documents[0].canvas.layer_stack.layers[idx].layer_type,
            LayerType::Vector(VectorGeometry::Path(_))
        ));
        app.docs.documents[0].canvas.undo().expect("undo");
        // Undo brings back the original parametric rectangle, unchanged.
        match &app.docs.documents[0].canvas.layer_stack.layers[idx].layer_type {
            LayerType::Vector(VectorGeometry::Primitive(s)) => {
                assert!(
                    (s.x0 - 20.0).abs() < 0.5
                        && (s.y0 - 20.0).abs() < 0.5
                        && (s.x1 - 120.0).abs() < 0.5
                        && (s.y1 - 120.0).abs() < 0.5,
                    "restored rect span ({},{},{},{})",
                    s.x0,
                    s.y0,
                    s.x1,
                    s.y1
                );
            }
            _ => panic!("undo should restore the original Primitive shape"),
        }
    }

    /// An open stroked line as a Path layer on a 200×200 canvas, made active.
    /// Built through `apply_object_to_layer` so `offset` matches the raster origin
    /// (the delta-0 invariant the trim relies on).
    fn app_with_open_line(a: (f32, f32), b: (f32, f32)) -> (App, usize) {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(200, 200);
        let canvas = &mut app.docs.documents[0].canvas;
        let idx = canvas.layer_stack.add_layer(200, 200);
        let path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(a.0, a.1)),
                    Node::sharp(Point::new(b.0, b.1)),
                ],
                false,
            )],
            FillRule::NonZero,
        );
        let mut style = VectorStyle::default();
        style.fill = Paint::None;
        style.stroke = Paint::Solid(ColorValue::BLACK);
        style.stroke_style.width = 3.0;
        let obj = VectorObjectData::new(path, style, AffineTransform::IDENTITY);
        crate::core::command_vector::apply_object_to_layer(
            &mut canvas.layer_stack.layers[idx],
            obj,
        );
        canvas.layer_stack.layers[idx].selected = true;
        canvas.layer_stack.active_idx = idx;
        (app, idx)
    }

    #[test]
    fn trim_open_line_splits_into_two_pieces() {
        let (mut app, idx) = app_with_open_line((20.0, 100.0), (180.0, 100.0));
        // A rectangular selection over the middle of the line.
        app.docs.documents[0]
            .canvas
            .selection
            .select_rect(90, 90, 110, 110);
        assert!(
            app.trim_active_vector_by_selection(),
            "{}",
            app.shell.status_msg
        );
        let l = &app.docs.documents[0].canvas.layer_stack.layers[idx];
        let LayerType::Vector(VectorGeometry::Path(obj)) = &l.layer_type else {
            panic!("active layer is not a Path");
        };
        assert_eq!(
            obj.path.contours.len(),
            2,
            "the open line is cut into two open sub-paths"
        );
        assert!(
            obj.path.contours.iter().all(|c| !c.closed),
            "both pieces stay open"
        );
    }

    #[test]
    fn trim_brush_stroke_cuts_into_two_layers() {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(200, 200);
        let idx = {
            let canvas = &mut app.docs.documents[0].canvas;
            let idx = canvas.layer_stack.add_layer(200, 200);
            let path = PathData::new(
                vec![Contour::new(
                    vec![
                        Node::sharp(Point::new(20.0, 100.0)),
                        Node::sharp(Point::new(180.0, 100.0)),
                    ],
                    false,
                )],
                FillRule::NonZero,
            );
            let brush = crate::core::vector::brush::BrushStroke::uniform(10.0, LineCap::Round);
            let obj = VectorObjectData::new_brush(
                path,
                VectorStyle::filled(ColorValue::BLACK),
                AffineTransform::IDENTITY,
                brush,
            );
            crate::core::command_vector::apply_object_to_layer(
                &mut canvas.layer_stack.layers[idx],
                obj,
            );
            canvas.layer_stack.layers[idx].selected = true;
            canvas.layer_stack.active_idx = idx;
            idx
        };
        let _ = idx;
        let before = app.docs.documents[0].canvas.layer_stack.layers.len();
        app.docs.documents[0]
            .canvas
            .selection
            .select_rect(90, 90, 110, 110);
        assert!(
            app.trim_active_vector_by_selection(),
            "{}",
            app.shell.status_msg
        );
        // The single stroke becomes two brush pieces → one more layer.
        assert_eq!(
            app.docs.documents[0].canvas.layer_stack.layers.len(),
            before + 1,
            "a mid cut yields two brush pieces"
        );
        let brush_layers = app.docs.documents[0]
            .canvas
            .layer_stack
            .layers
            .iter()
            .filter(|l| {
                matches!(&l.layer_type,
                    LayerType::Vector(VectorGeometry::Path(o)) if o.is_brush())
            })
            .count();
        assert!(
            brush_layers >= 2,
            "both pieces keep the brush, got {brush_layers}"
        );
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

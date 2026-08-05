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
use crate::core::layer::{Layer, LayerType};
use crate::core::vector::affine::AffineTransform;
use crate::core::vector::boolean::{boolean_many, intersect_overlaps, BooleanOp};
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
        // Selected vector layers bottom-to-top: (layer index, canvas-space path,
        // style). Index 0 is the bottom layer.
        let items: Vec<(usize, PathData, VectorStyle)> = {
            let stack = &self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack;
            stack
                .layers
                .iter()
                .enumerate()
                .filter(|(_, l)| is_boolean_target(l))
                .filter_map(|(i, l)| layer_canvas_path(l).map(|(p, s)| (i, p, s)))
                .collect()
        };
        if items.len() < 2 {
            self.shell.status_msg = "Select at least 2 vector objects to shape".to_string();
            return false;
        }
        let indices: Vec<usize> = items.iter().map(|(i, _, _)| *i).collect();
        let paths: Vec<&PathData> = items.iter().map(|(_, p, _)| p).collect();
        let target_idx = indices[0];
        // Merge results inherit the bottom (target) object's style.
        let target_style = items[0].2;

        let name = match op {
            BooleanOp::Union => "Weld",
            BooleanOp::Intersect => "Intersect",
            BooleanOp::Difference => "Trim",
            BooleanOp::Exclude => "Simplify",
        };

        // Compute the geometry up front so an empty/failed result aborts cleanly,
        // before any layer is touched.
        enum Plan {
            /// Remove all sources, insert one result at the bottom (Weld).
            Merge(VectorObjectData),
            /// Replace only the bottom target. `None` means it was fully trimmed.
            ReplaceTarget(Option<VectorObjectData>),
            /// Keep all sources, add one result on top (Intersect).
            AddOnTop(VectorObjectData),
            /// Replace each source's geometry in place; `None` = fully consumed,
            /// remove that layer (Simplify). The top selected layer is untouched.
            PerLayer(Vec<(usize, Option<VectorObjectData>)>),
        }
        let new_obj =
            |p: PathData, s: VectorStyle| VectorObjectData::new(p, s, AffineTransform::IDENTITY);

        let plan = match op {
            BooleanOp::Union => match boolean_many(&paths, op).filter(|r| !r.contours.is_empty()) {
                Some(r) => Plan::Merge(new_obj(r, target_style)),
                None => {
                    self.shell.status_msg = "Empty result".to_string();
                    return false;
                }
            },
            BooleanOp::Difference => {
                let result = boolean_many(&paths, op)
                    .filter(|r| !r.contours.is_empty())
                    .map(|r| new_obj(r, target_style));
                Plan::ReplaceTarget(result)
            }
            BooleanOp::Intersect => {
                match intersect_overlaps(&paths).filter(|r| !r.contours.is_empty()) {
                    Some(r) => Plan::AddOnTop(new_obj(r, target_style)),
                    None => {
                        self.shell.status_msg = "No intersection".to_string();
                        return false;
                    }
                }
            }
            BooleanOp::Exclude => {
                // Corel Simplify: top objects act as cutters for objects below.
                // The top selected object stays untouched.
                let mut per: Vec<(usize, Option<VectorObjectData>)> =
                    Vec::with_capacity(items.len().saturating_sub(1));
                for (k, (idx, _p, style)) in
                    items.iter().enumerate().take(items.len().saturating_sub(1))
                {
                    let mut refs: Vec<&PathData> = vec![paths[k]];
                    refs.extend(paths.iter().skip(k + 1).copied());
                    let r = boolean_many(&refs, BooleanOp::Difference)
                        .filter(|r| !r.contours.is_empty());
                    match r {
                        Some(r) => per.push((*idx, Some(new_obj(r, *style)))),
                        None => per.push((*idx, None)),
                    }
                }
                Plan::PerLayer(per)
            }
        };

        let valid = match &plan {
            Plan::Merge(o) | Plan::AddOnTop(o) => o.validate().is_ok(),
            Plan::ReplaceTarget(o) => o.as_ref().map_or(true, |o| o.validate().is_ok()),
            Plan::PerLayer(v) => v
                .iter()
                .all(|(_, o)| o.as_ref().map_or(true, |o| o.validate().is_ok())),
        };
        if !valid {
            self.shell.status_msg = "Invalid result".to_string();
            return false;
        }

        let (cw, ch) = {
            let d = &self.docs.documents[self.docs.active_doc_idx];
            (d.canvas.width, d.canvas.height)
        };
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let before = LayerStructureCommand::capture_before(name, &canvas.layer_stack, cw, ch);

        let mut removed_empty = 0usize;
        match plan {
            Plan::Merge(object) => {
                let new_id = canvas.layer_stack.next_id();
                canvas.layer_stack.set_next_id(new_id + 1);
                let mut new_layer = canvas.layer_stack.layers[target_idx].clone();
                new_layer.id = new_id;
                new_layer.name = name.to_string();
                new_layer.mask = None;
                crate::core::command_vector::apply_object_to_layer(&mut new_layer, object);
                // Remove sources highest-index-first so lower indices stay valid.
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
            }
            Plan::AddOnTop(object) => {
                let new_id = canvas.layer_stack.next_id();
                canvas.layer_stack.set_next_id(new_id + 1);
                let mut new_layer = canvas.layer_stack.layers[target_idx].clone();
                new_layer.id = new_id;
                new_layer.name = name.to_string();
                new_layer.mask = None;
                crate::core::command_vector::apply_object_to_layer(&mut new_layer, object);
                let top = *indices.last().unwrap_or(&target_idx);
                let insert_at = (top + 1).min(canvas.layer_stack.layers.len());
                canvas.layer_stack.layers.insert(insert_at, new_layer);
                canvas.layer_stack.active_idx = insert_at;
                for (i, l) in canvas.layer_stack.layers.iter_mut().enumerate() {
                    l.selected = i == insert_at;
                }
            }
            Plan::ReplaceTarget(object) => {
                if let Some(object) = object {
                    crate::core::command_vector::apply_object_to_layer(
                        &mut canvas.layer_stack.layers[target_idx],
                        object,
                    );
                    canvas.layer_stack.active_idx = target_idx;
                } else {
                    canvas.layer_stack.layers.remove(target_idx);
                    removed_empty = 1;
                    canvas.layer_stack.active_idx =
                        target_idx.min(canvas.layer_stack.layers.len().saturating_sub(1));
                }
                let active = canvas.layer_stack.active_idx;
                for (i, layer) in canvas.layer_stack.layers.iter_mut().enumerate() {
                    layer.selected = i == active;
                }
            }
            Plan::PerLayer(per) => {
                // In-place edits keep indices stable; collect the empties and drop
                // them afterwards (highest-first). Edited layers stay selected.
                let mut empties: Vec<usize> = Vec::new();
                for (idx, obj) in per {
                    match obj {
                        Some(o) => {
                            let layer = &mut canvas.layer_stack.layers[idx];
                            crate::core::command_vector::apply_object_to_layer(layer, o);
                        }
                        None => empties.push(idx),
                    }
                }
                empties.sort_unstable_by(|a, b| b.cmp(a));
                for &i in &empties {
                    canvas.layer_stack.layers.remove(i);
                }
                removed_empty = empties.len();
                if let Some(pos) = canvas.layer_stack.layers.iter().position(|l| l.selected) {
                    canvas.layer_stack.active_idx = pos;
                } else {
                    canvas.layer_stack.active_idx = canvas
                        .layer_stack
                        .active_idx
                        .min(canvas.layer_stack.layers.len().saturating_sub(1));
                }
            }
        }

        // CMYK: re-derive ink planes for the new Path raster(s) from the RGB mirror.
        canvas.reconcile_path_ink();
        canvas.layer_revision += 1;

        let mut cmd = before;
        cmd.capture_after(&canvas.layer_stack, cw, ch);
        canvas.record(Box::new(cmd));

        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.apply_canvas_event(CanvasEvent::SelectionChanged);
        self.shell.status_msg = match op {
            BooleanOp::Union => format!("Welded {} vector objects", indices.len()),
            BooleanOp::Difference => {
                if removed_empty == 0 {
                    format!(
                        "Trimmed the target with {} source(s) — sources kept",
                        indices.len() - 1
                    )
                } else {
                    "Target fully trimmed away — sources kept".to_string()
                }
            }
            BooleanOp::Intersect => "Created the intersection (originals kept)".to_string(),
            BooleanOp::Exclude => format!(
                "Simplified the overlap — {} shapes remain",
                indices.len() - removed_empty
            ),
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

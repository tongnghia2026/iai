//! Node tool (direct-selection) editing of a Path layer's anchor points
//! (Slice 3 of the vector track). Dragging an anchor moves it (its handles
//! follow); pressing on a segment first INSERTS an anchor there (De Casteljau
//! split, shape-preserving) then drags it; Delete removes the selected anchor.
//!
//! Every edit changes only `PathData` geometry — the object `transform` and
//! style are kept — and commits ONE [`ReplacePathGeometry`] so an insert+move is
//! a single undo step. Like the Move-tool transform box, the overlay follows a
//! `pending` path every frame while the fill re-raster is throttled by its
//! measured cost, so a big filled path never stalls the drag.
//!
//! Nodes are in OBJECT-LOCAL space; with position committed into the model
//! (delta-0 invariant) the object transform maps local ⇄ canvas directly.

use crate::app::render::CanvasEvent;
use crate::app::state::{App, NodeDrag, NodeDragTarget};
use crate::core::geometry::{cubic_bezier, Point};
use crate::core::layer::LayerType;
use crate::core::vector::affine::AffineTransform;
use crate::core::vector::object::VectorObjectData;
use crate::core::vector::ops::HandleSide;
use crate::core::vector::path::PathData;

/// Screen-space grab radius for an anchor.
const NODE_HIT_PX: f32 = 8.0;
/// Screen-space grab radius for a segment (insert an anchor).
const SEG_HIT_PX: f32 = 6.0;
/// Samples per segment when locating the nearest split parameter.
const SEG_SAMPLES: usize = 40;
/// Flatten tolerance (object-local units) for the overlay outline.
const OUTLINE_TOL: f32 = 0.4;

/// What the Node tool pointer is over on the active Path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NodeHit {
    /// A Bézier control handle `(contour, node, side)` of the SELECTED node —
    /// these sit on top of the anchor and are only reachable when their node is
    /// selected (that is when the overlay draws them).
    Handle(usize, usize, HandleSide),
    /// An existing anchor `(contour, node)`.
    Node(usize, usize),
    /// A point on segment `seg` of `contour` at parameter `t` — insert here.
    Segment(usize, usize, f32),
}

/// Shift a node's anchor and both handles by the same delta (a rigid move that
/// preserves local curvature).
fn shifted_node(
    base: &crate::core::vector::path::Node,
    dx: f32,
    dy: f32,
) -> crate::core::vector::path::Node {
    let shift = |p: Point| Point::new(p.x + dx, p.y + dy);
    crate::core::vector::path::Node {
        anchor: shift(base.anchor),
        in_handle: base.in_handle.map(shift),
        out_handle: base.out_handle.map(shift),
        kind: base.kind,
    }
}

/// The parameter `t ∈ (0,1)` on segment `seg` of `contour` closest to the local
/// point `p`, by uniform sampling. Clamped away from the endpoints so
/// `split_segment` accepts it.
fn nearest_segment_t(contour: &crate::core::vector::path::Contour, seg: usize, p: Point) -> f32 {
    let Some((p0, p1, p2, p3)) = contour.segment(seg) else {
        return 0.5;
    };
    let mut best_t = 0.5;
    let mut best_d = f32::INFINITY;
    for k in 0..=SEG_SAMPLES {
        let t = k as f32 / SEG_SAMPLES as f32;
        let d = cubic_bezier(p0, p1, p2, p3, t).distance_to(p);
        if d < best_d {
            best_d = d;
            best_t = t;
        }
    }
    best_t.clamp(0.02, 0.98)
}

impl App {
    /// The active Path's `(layer_id, transform, path)` for node editing, or
    /// `None` when the Node tool has no editable Path active.
    fn active_node_object(&self) -> Option<(u32, AffineTransform, PathData)> {
        let idx = self.active_path_layer()?;
        let layer = &self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .layers[idx];
        let LayerType::Path(obj) = &layer.layer_type else {
            return None;
        };
        Some((layer.id, obj.transform, obj.path.clone()))
    }

    /// The geometry the overlay/hit-test should use: the live `pending` path
    /// during a drag, else the committed model path. Plus the object transform.
    fn node_edit_geometry(&self) -> Option<(u32, AffineTransform, PathData)> {
        let (id, t, model_path) = self.active_node_object()?;
        let path = match &self.edit.node_drag {
            Some(d) if d.layer_id == id => d.pending.clone(),
            _ => model_path,
        };
        Some((id, t, path))
    }

    /// Build the Node tool overlay for the active Path (outline + anchors +
    /// selected-node handle arms), or `None` when the Node tool isn't editing a
    /// Path. Canvas-space; the UI maps to screen.
    pub fn active_node_overlay(&self) -> Option<crate::ui::NodeOverlay> {
        if self.edit.tools.active_id() != crate::tools::ToolId::Node
            || self.edit.transform_state.is_some()
        {
            return None;
        }
        let (id, t, path) = self.node_edit_geometry()?;
        let map = |p: Point| {
            let q = t.apply_point(p);
            (q.x, q.y)
        };
        let sel = self
            .edit
            .node_selected
            .and_then(|(lid, c, n)| (lid == id).then_some((c, n)));

        let mut outlines: Vec<Vec<(f32, f32)>> = Vec::new();
        for c in &path.contours {
            let poly = crate::core::vector::flatten::flatten_contour(c, OUTLINE_TOL);
            let mut line: Vec<(f32, f32)> = poly.iter().map(|p| map(*p)).collect();
            if c.closed && line.len() >= 2 {
                line.push(line[0]);
            }
            if line.len() >= 2 {
                outlines.push(line);
            }
        }

        let mut nodes: Vec<(f32, f32, bool)> = Vec::new();
        let mut handles: Vec<[f32; 4]> = Vec::new();
        for (ci, c) in path.contours.iter().enumerate() {
            for (ni, node) in c.nodes.iter().enumerate() {
                let (ax, ay) = map(node.anchor);
                let selected = sel == Some((ci, ni));
                nodes.push((ax, ay, selected));
                if selected {
                    if let Some(h) = node.in_handle {
                        let (hx, hy) = map(h);
                        handles.push([ax, ay, hx, hy]);
                    }
                    if let Some(h) = node.out_handle {
                        let (hx, hy) = map(h);
                        handles.push([ax, ay, hx, hy]);
                    }
                }
            }
        }
        Some(crate::ui::NodeOverlay {
            outlines,
            nodes,
            handles,
        })
    }

    /// Cursor hint for the Node tool at the current mouse position:
    /// `4` over a control handle (drag to reshape), `2` over an anchor (drag to
    /// move), `3` over a segment (click to insert), `0` otherwise (plain arrow).
    /// Keeps the cursor logic in `state.rs` tiny and mirrors `move_hover_hint`.
    pub fn node_cursor_hint(&self) -> u8 {
        match self.node_hit_at_screen(self.edit.input.mouse_x, self.edit.input.mouse_y) {
            Some(NodeHit::Handle(..)) => 4,
            Some(NodeHit::Node(..)) => 2,
            Some(NodeHit::Segment(..)) => 3,
            None => 0,
        }
    }

    /// Which anchor / segment of the active Path is under the screen point.
    /// Anchors win over segments.
    pub fn node_hit_at_screen(&self, sx: f32, sy: f32) -> Option<NodeHit> {
        let (id, t, path) = self.node_edit_geometry()?;
        let zoom = self.edit.view.zoom;
        let vox = self.edit.view.offset_x;
        let voy = self.edit.view.offset_y;
        let to_screen = |p: Point| {
            let q = t.apply_point(p);
            (q.x * zoom + vox, q.y * zoom + voy)
        };
        let dist2 = |(ax, ay): (f32, f32)| (ax - sx).powi(2) + (ay - sy).powi(2);

        // Handles of the SELECTED node win first — they sit on top of the path
        // and are only drawn (hence grabbable) while their node is selected.
        if let Some((lid, sc, sn)) = self.edit.node_selected {
            if lid == id {
                if let Some(node) = path.contours.get(sc).and_then(|c| c.nodes.get(sn)) {
                    for (side, h) in [
                        (HandleSide::In, node.in_handle),
                        (HandleSide::Out, node.out_handle),
                    ] {
                        if let Some(hp) = h {
                            if dist2(to_screen(hp)) <= NODE_HIT_PX * NODE_HIT_PX {
                                return Some(NodeHit::Handle(sc, sn, side));
                            }
                        }
                    }
                }
            }
        }

        // Anchors next.
        let mut best: Option<(f32, usize, usize)> = None;
        for (ci, c) in path.contours.iter().enumerate() {
            for (ni, node) in c.nodes.iter().enumerate() {
                let d = dist2(to_screen(node.anchor));
                if d <= NODE_HIT_PX * NODE_HIT_PX && best.map_or(true, |(bd, _, _)| d < bd) {
                    best = Some((d, ci, ni));
                }
            }
        }
        if let Some((_, ci, ni)) = best {
            return Some(NodeHit::Node(ci, ni));
        }

        // Then segments: sample each and take the nearest within the pick radius.
        let mut best_seg: Option<(f32, usize, usize, Point)> = None;
        for (ci, c) in path.contours.iter().enumerate() {
            for seg in 0..c.segment_count() {
                let Some((p0, p1, p2, p3)) = c.segment(seg) else {
                    continue;
                };
                for k in 1..SEG_SAMPLES {
                    let t = k as f32 / SEG_SAMPLES as f32;
                    let lp = cubic_bezier(p0, p1, p2, p3, t);
                    let d = dist2(to_screen(lp));
                    if d <= SEG_HIT_PX * SEG_HIT_PX && best_seg.map_or(true, |(bd, _, _, _)| d < bd)
                    {
                        best_seg = Some((d, ci, seg, lp));
                    }
                }
            }
        }
        if let Some((_, ci, seg, lp)) = best_seg {
            let tparam = nearest_segment_t(&path.contours[ci], seg, lp);
            return Some(NodeHit::Segment(ci, seg, tparam));
        }
        None
    }

    /// Begin a Node tool gesture at press point `(cx, cy)` (canvas space). A
    /// `Node` hit drags that anchor; a `Segment` hit inserts an anchor there and
    /// drags the new one. Returns true when a gesture started.
    pub fn node_press(&mut self, hit: NodeHit, cx: f32, cy: f32) -> bool {
        let Some((id, t, model_path)) = self.active_node_object() else {
            return false;
        };
        let Some(inv) = t.inverse() else {
            return false;
        };
        let grab_local = inv.apply_point(Point::new(cx, cy));

        let orig_path = model_path.clone();
        // (pending geometry, contour, node, drag target, geometry-already-changed?)
        let (pending, contour, node, target, changed) = match hit {
            NodeHit::Node(ci, ni) => (model_path, ci, ni, NodeDragTarget::Anchor, false),
            NodeHit::Handle(ci, ni, side) => {
                (model_path, ci, ni, NodeDragTarget::Handle(side), false)
            }
            NodeHit::Segment(ci, seg, tparam) => {
                let mut p = model_path;
                let Some(c) = p.contours.get_mut(ci) else {
                    return false;
                };
                let ni = match crate::core::vector::ops::split_segment(c, seg, tparam) {
                    Ok(i) => i,
                    Err(_) => return false,
                };
                // An insert already changed geometry; drag the fresh anchor.
                (p, ci, ni, NodeDragTarget::Anchor, true)
            }
        };
        let Some(base_node) = pending
            .contours
            .get(contour)
            .and_then(|c| c.nodes.get(node))
        else {
            return false;
        };
        let base_node = *base_node;

        self.edit.node_selected = Some((id, contour, node));
        self.edit.node_drag = Some(NodeDrag {
            layer_id: id,
            contour,
            node,
            target,
            orig_path,
            pending: pending.clone(),
            grab_local,
            base_node,
            changed,
        });
        // Show the inserted node (and the fresh selection) immediately.
        self.apply_pending_node_geometry(id, &pending);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        true
    }

    /// True while a node is being dragged.
    pub fn node_drag_active(&self) -> bool {
        self.edit.node_drag.is_some()
    }

    /// Apply the in-progress node drag to `(cx, cy)`. Updates `pending` every
    /// frame (the overlay tracks it at 60 fps); the fill re-raster runs
    /// OFF-THREAD so a big filled path never stalls the drag.
    pub fn node_drag_update(&mut self, cx: f32, cy: f32) {
        // Map the cursor to object-local space via the object transform inverse.
        let Some((_, t, _)) = self.active_node_object() else {
            return;
        };
        let Some(inv) = t.inverse() else {
            return;
        };
        let local = inv.apply_point(Point::new(cx, cy));

        let (layer_id, pending) = match self.edit.node_drag.as_mut() {
            Some(d) => {
                let dx = local.x - d.grab_local.x;
                let dy = local.y - d.grab_local.y;
                let Some(c) = d.pending.contours.get_mut(d.contour) else {
                    return;
                };
                let Some(slot) = c.nodes.get_mut(d.node) else {
                    return;
                };
                match d.target {
                    NodeDragTarget::Anchor => {
                        *slot = shifted_node(&d.base_node, dx, dy);
                    }
                    NodeDragTarget::Handle(side) => {
                        // Reset to the node as it was at press, then move the
                        // grabbed handle rigidly with the cursor; the opposite
                        // handle is coupled per the node kind in core.
                        *slot = d.base_node;
                        let base_h = match side {
                            HandleSide::In => d.base_node.in_handle,
                            HandleSide::Out => d.base_node.out_handle,
                        }
                        .unwrap_or(d.base_node.anchor);
                        let new_pos = Point::new(base_h.x + dx, base_h.y + dy);
                        crate::core::vector::ops::apply_handle_move(slot, side, new_pos);
                    }
                }
                d.changed = true;
                (d.layer_id, d.pending.clone())
            }
            None => return,
        };
        // Build the target object (style + transform kept, new geometry) and hand
        // it to the off-thread bake.
        let object = {
            let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
            match canvas.layer_stack.layers.iter().find(|l| l.id == layer_id) {
                Some(l) => match &l.layer_type {
                    LayerType::Path(o) => VectorObjectData {
                        path: pending,
                        ..o.clone()
                    },
                    _ => return,
                },
                None => return,
            }
        };
        self.request_path_bake(layer_id, object);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Re-raster the Path layer's cache from a working `path` (object transform +
    /// style kept), with dirty-rect invalidation — a live preview only.
    fn apply_pending_node_geometry(&mut self, layer_id: u32, path: &PathData) {
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let Some(idx) = canvas
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == layer_id)
        else {
            return;
        };
        let (old_off, old_w, old_h, obj) = {
            let layer = &canvas.layer_stack.layers[idx];
            let LayerType::Path(o) = &layer.layer_type else {
                return;
            };
            (layer.offset, layer.width, layer.height, o.clone())
        };
        let new_obj = VectorObjectData {
            path: path.clone(),
            ..obj
        };
        {
            let layer = &mut canvas.layer_stack.layers[idx];
            crate::core::command_vector::apply_object_to_layer(layer, new_obj);
        }
        canvas.layer_revision += 1;
        let (new_off, new_w, new_h) = {
            let l = &canvas.layer_stack.layers[idx];
            (l.offset, l.width, l.height)
        };
        canvas.mark_dirty_layer_bounds(old_off.0, old_off.1, old_w, old_h);
        canvas.mark_dirty_layer_bounds(new_off.0, new_off.1, new_w, new_h);
        self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
    }

    /// Finish the node drag: rewind the model to the pre-gesture geometry so the
    /// gateway captures the correct "before", then commit `pending` as ONE
    /// [`crate::core::command_vector::ReplacePathGeometry`].
    pub fn node_drag_finish(&mut self) {
        let Some(drag) = self.edit.node_drag.take() else {
            return;
        };
        // Abandon any in-flight worker bake — committed synchronously below.
        self.cancel_path_bake();
        if !drag.changed || drag.pending == drag.orig_path {
            // A no-op click: restore the exact baseline, record nothing.
            self.apply_pending_node_geometry(drag.layer_id, &drag.orig_path);
            return;
        }
        // Rewind the live preview to the baseline, then execute old→new.
        self.apply_pending_node_geometry(drag.layer_id, &drag.orig_path);
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let _ = canvas.execute(
            Box::new(crate::core::command_vector::ReplacePathGeometry::new(
                drag.layer_id,
                drag.pending,
            )),
            crate::core::gateway::ChangeKind::LayerStructure,
        );
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Delete the selected node (Delete key under the Node tool). Refuses to drop
    /// a contour below 2 nodes (nothing left to draw). Returns true when handled.
    pub fn node_delete_selected(&mut self) -> bool {
        let Some((id, ci, ni)) = self.edit.node_selected else {
            return false;
        };
        let Some((layer_id, _t, mut path)) = self.active_node_object() else {
            return false;
        };
        if layer_id != id {
            return false;
        }
        let Some(c) = path.contours.get_mut(ci) else {
            return false;
        };
        if c.nodes.len() <= 2 {
            self.shell.status_msg = "Không thể xoá: đường cần ít nhất 2 điểm".to_string();
            return true;
        }
        if crate::core::vector::ops::delete_node(c, ni).is_err() {
            return false;
        }
        self.edit.node_selected = None;
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let _ = canvas.execute(
            Box::new(crate::core::command_vector::ReplacePathGeometry::new(
                layer_id, path,
            )),
            crate::core::gateway::ChangeKind::LayerStructure,
        );
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.shell.status_msg = "Đã xoá điểm".to_string();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        true
    }

    /// Cycle node `(ci, ni)`'s kind Cusp → Smooth → Symmetric → Cusp (Node tool
    /// double-click on an anchor). Converting a Cusp corner to Smooth synthesizes
    /// collinear handles, so a straight primitive (rectangle/polygon) becomes a
    /// curve the user can then bend by dragging those handles. One
    /// [`crate::core::command_vector::ReplacePathGeometry`] = one undo.
    pub fn node_toggle_kind(&mut self, ci: usize, ni: usize) -> bool {
        use crate::core::vector::ops;
        use crate::core::vector::path::NodeKind;
        let Some((layer_id, _t, mut path)) = self.active_node_object() else {
            return false;
        };
        let Some(c) = path.contours.get_mut(ci) else {
            return false;
        };
        let Some(kind) = c.nodes.get(ni).map(|n| n.kind) else {
            return false;
        };
        let (res, label) = match kind {
            NodeKind::Cusp => (ops::set_node_smooth(c, ni), "Điểm trơn (Smooth)"),
            NodeKind::Smooth => (ops::set_node_symmetric(c, ni), "Điểm đối xứng (Symmetric)"),
            NodeKind::Symmetric => (ops::set_node_cusp(c, ni), "Điểm góc (Cusp)"),
        };
        if res.is_err() {
            return false;
        }
        self.edit.node_selected = Some((layer_id, ci, ni));
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let _ = canvas.execute(
            Box::new(crate::core::command_vector::ReplacePathGeometry::new(
                layer_id, path,
            )),
            crate::core::gateway::ChangeKind::LayerStructure,
        );
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.shell.status_msg = label.to_string();
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
    use crate::core::command_vector::CreatePathLayer;
    use crate::core::gateway::ChangeKind;
    use crate::core::vector::path::{Contour, FillRule, Node, PathData};
    use crate::core::vector::style::VectorStyle;

    fn rect_obj(w: f32, h: f32, tx: f32, ty: f32) -> VectorObjectData {
        let path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(w, 0.0)),
                    Node::sharp(Point::new(w, h)),
                    Node::sharp(Point::new(0.0, h)),
                ],
                true,
            )],
            FillRule::NonZero,
        );
        VectorObjectData::new(
            path,
            VectorStyle::default(),
            AffineTransform::translate(tx, ty),
        )
    }

    /// App with one active, selected Path (a 40×20 rect at (100,120)) and the
    /// Node tool live at 1:1 zoom.
    fn app_with_path() -> (App, u32) {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(300, 300);
        let id = {
            let canvas = &mut app.docs.documents[0].canvas;
            canvas
                .execute(
                    Box::new(CreatePathLayer::new(
                        rect_obj(40.0, 20.0, 100.0, 120.0),
                        "Path 1",
                    )),
                    ChangeKind::LayerStructure,
                )
                .unwrap();
            let idx = canvas
                .layer_stack
                .layers
                .iter()
                .position(|l| matches!(l.layer_type, LayerType::Path(_)))
                .unwrap();
            canvas.layer_stack.active_idx = idx;
            canvas.layer_stack.layers[idx].id
        };
        app.edit.tools.select(crate::tools::ToolId::Node);
        app.edit.view.zoom = 1.0;
        app.edit.view.offset_x = 0.0;
        app.edit.view.offset_y = 0.0;
        (app, id)
    }

    fn model_path(app: &App, id: u32) -> PathData {
        match &app.docs.documents[0]
            .canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == id)
            .unwrap()
            .layer_type
        {
            LayerType::Path(o) => o.path.clone(),
            _ => panic!("not a path"),
        }
    }

    #[test]
    fn hit_finds_anchor_then_drag_moves_it_and_undoes() {
        let (mut app, id) = app_with_path();
        // Top-left anchor sits on screen at (100,120) at 1:1.
        let hit = app.node_hit_at_screen(100.0, 120.0).expect("anchor hit");
        assert_eq!(hit, NodeHit::Node(0, 0));
        app.node_press(hit, 100.0, 120.0);
        app.node_drag_update(130.0, 150.0); // drag it +30,+30 in canvas/local
        app.node_drag_finish();

        let p = model_path(&app, id);
        let a = p.contours[0].nodes[0].anchor;
        assert!(
            (a.x - 30.0).abs() < 0.5 && (a.y - 30.0).abs() < 0.5,
            "node moved in local space, got {a:?}"
        );
        assert!(matches!(
            app.docs.documents[0].canvas.layer_stack.layers
                [app.docs.documents[0].canvas.layer_stack.active_idx]
                .layer_type,
            LayerType::Path(_)
        ));

        app.docs.documents[0].canvas.undo().expect("undo");
        let p = model_path(&app, id);
        assert!(
            p.contours[0].nodes[0]
                .anchor
                .distance_to(Point::new(0.0, 0.0))
                < 0.5
        );
    }

    #[test]
    fn segment_press_inserts_a_node_and_undo_removes_it() {
        let (mut app, id) = app_with_path();
        assert_eq!(model_path(&app, id).contours[0].nodes.len(), 4);
        // Midpoint of the top edge: canvas (120,120).
        let hit = app.node_hit_at_screen(120.0, 120.0).expect("segment hit");
        assert!(matches!(hit, NodeHit::Segment(0, 0, _)));
        app.node_press(hit, 120.0, 120.0);
        app.node_drag_finish();
        assert_eq!(
            model_path(&app, id).contours[0].nodes.len(),
            5,
            "one anchor inserted"
        );
        app.docs.documents[0].canvas.undo().expect("undo");
        assert_eq!(model_path(&app, id).contours[0].nodes.len(), 4);
    }

    fn node_at(app: &App, id: u32, ci: usize, ni: usize) -> crate::core::vector::path::Node {
        model_path(app, id).contours[ci].nodes[ni]
    }

    #[test]
    fn double_click_converts_cusp_corner_to_smooth_with_handles() {
        use crate::core::vector::path::NodeKind;
        let (mut app, id) = app_with_path();
        // A fresh rect corner is a cusp with no handles.
        let n0 = node_at(&app, id, 0, 0);
        assert_eq!(n0.kind, NodeKind::Cusp);
        assert!(n0.in_handle.is_none() && n0.out_handle.is_none());

        assert!(app.node_toggle_kind(0, 0));
        let n0 = node_at(&app, id, 0, 0);
        assert_eq!(n0.kind, NodeKind::Smooth);
        assert!(
            n0.in_handle.is_some() && n0.out_handle.is_some(),
            "handles synthesized so the corner can now be bent"
        );

        app.docs.documents[0].canvas.undo().expect("undo");
        let n0 = node_at(&app, id, 0, 0);
        assert_eq!(n0.kind, NodeKind::Cusp);
        assert!(n0.in_handle.is_none() && n0.out_handle.is_none());
    }

    #[test]
    fn drag_out_handle_reshapes_curve_couples_in_handle_and_undoes() {
        let (mut app, id) = app_with_path();
        // Give the top-left node collinear handles, then drag its OUT handle.
        assert!(app.node_toggle_kind(0, 0)); // Cusp -> Smooth, selects node (0,0)
        let before = node_at(&app, id, 0, 0);
        let oh = before.out_handle.unwrap();
        // Transform is translate(100,120) at 1:1 zoom, 0 offset => canvas = local + (100,120).
        let (cx, cy) = (oh.x + 100.0, oh.y + 120.0);
        let hit = app.node_hit_at_screen(cx, cy).expect("handle hit");
        assert!(matches!(hit, NodeHit::Handle(0, 0, HandleSide::Out)));
        assert!(app.node_press(hit, cx, cy));
        app.node_drag_update(cx + 5.0, cy - 8.0); // move the handle by (+5,-8)
        app.node_drag_finish();

        let after = node_at(&app, id, 0, 0);
        let oh2 = after.out_handle.unwrap();
        assert!(
            (oh2.x - (oh.x + 5.0)).abs() < 0.5 && (oh2.y - (oh.y - 8.0)).abs() < 0.5,
            "out handle followed the cursor, got {oh2:?}"
        );
        // Smooth coupling: the in handle stays collinear and points the other way.
        let a = after.anchor;
        let ih = after.in_handle.unwrap();
        let (vo, vi) = ((oh2.x - a.x, oh2.y - a.y), (ih.x - a.x, ih.y - a.y));
        let (no, ni) = (
            (vo.0 * vo.0 + vo.1 * vo.1).sqrt(),
            (vi.0 * vi.0 + vi.1 * vi.1).sqrt(),
        );
        let cross = (vo.0 / no) * (vi.1 / ni) - (vo.1 / no) * (vi.0 / ni);
        assert!(
            cross.abs() < 1e-2,
            "in/out handles not collinear after drag"
        );
        assert!(
            vo.0 * vi.0 + vo.1 * vi.1 < 0.0,
            "handles point opposite ways"
        );

        // One undo returns to the smoothed-but-undragged handle (single step).
        app.docs.documents[0].canvas.undo().expect("undo");
        let back = node_at(&app, id, 0, 0);
        assert!(
            back.out_handle.unwrap().distance_to(oh) < 0.5,
            "the handle drag undid in one step"
        );
    }

    #[test]
    fn delete_selected_removes_a_node_and_undoes() {
        let (mut app, id) = app_with_path();
        app.edit.node_selected = Some((id, 0, 1));
        assert!(app.node_delete_selected());
        assert_eq!(model_path(&app, id).contours[0].nodes.len(), 3);
        app.docs.documents[0].canvas.undo().expect("undo");
        assert_eq!(model_path(&app, id).contours[0].nodes.len(), 4);
    }

    #[test]
    fn delete_refuses_below_two_nodes() {
        let (mut app, _id) = app_with_path();
        // Shrink to a 2-node contour, then a delete must be refused.
        {
            let canvas = &mut app.docs.documents[0].canvas;
            let idx = canvas.layer_stack.active_idx;
            if let LayerType::Path(o) = &mut canvas.layer_stack.layers[idx].layer_type {
                o.path.contours[0].nodes.truncate(2);
                o.path.contours[0].closed = false;
            }
        }
        let id = app.docs.documents[0].canvas.layer_stack.layers
            [app.docs.documents[0].canvas.layer_stack.active_idx]
            .id;
        app.edit.node_selected = Some((id, 0, 0));
        assert!(app.node_delete_selected(), "handled (refused)");
        assert_eq!(
            model_path(&app, id).contours[0].nodes.len(),
            2,
            "not deleted"
        );
    }
}

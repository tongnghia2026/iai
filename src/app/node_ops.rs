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
use crate::core::vector::object::VectorGeometry;
use crate::core::vector::object::VectorObjectData;
use crate::core::vector::ops::{AlignRef, Axis, HandleSide};
use crate::core::vector::path::PathData;

/// Screen-space grab radius for an anchor.
const NODE_HIT_PX: f32 = 8.0;
/// Screen-space grab radius for a segment (insert an anchor).
const SEG_HIT_PX: f32 = 6.0;
/// Samples per segment when locating the nearest split parameter.
const SEG_SAMPLES: usize = 40;
/// Maximum flattening error in screen pixels for the editable outline.
///
/// Keeping this in screen space is important: a fixed object-space tolerance
/// turns into visibly angular chords when the user zooms in or when the Path's
/// object transform scales it up.
const OUTLINE_SCREEN_TOL: f32 = 0.15;

fn node_outline_tolerance(t: AffineTransform, zoom: f32) -> f32 {
    // The Frobenius norm is a safe upper bound for the linear transform's
    // largest scale factor. Dividing by it keeps the flattened curve's error
    // below OUTLINE_SCREEN_TOL after object→canvas→screen mapping.
    let transform_scale = (t.a * t.a + t.b * t.b + t.c * t.c + t.d * t.d)
        .sqrt()
        .max(1e-4);
    OUTLINE_SCREEN_TOL / (zoom.abs().max(1e-4) * transform_scale)
}

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
        let LayerType::Vector(VectorGeometry::Path(obj)) = &layer.layer_type else {
            return None;
        };
        Some((layer.id, obj.transform, obj.path.clone()))
    }

    /// Every selected node `(contour, node)` on the active Path — the primary in
    /// `node_selected` plus the `node_multi` extras — with the layer id. `None`
    /// when nothing is selected. The primary is always first.
    fn node_effective_selection(&self) -> Option<(u32, Vec<(usize, usize)>)> {
        let (lid, pc, pn) = self.edit.node_selected?;
        let mut v = vec![(pc, pn)];
        for &(c, n) in &self.edit.node_multi {
            if (c, n) != (pc, pn) {
                v.push((c, n));
            }
        }
        Some((lid, v))
    }

    /// Clear both the primary node and the multi-selection.
    pub fn clear_node_selection(&mut self) {
        self.edit.node_selected = None;
        self.edit.node_multi.clear();
    }

    /// Shift-click a node `(ci, ni)`: toggle it in the multi-selection. Adding a
    /// node makes it the new primary (the old primary joins the extras); removing
    /// the primary promotes an extra (or clears). Node coords are on the active
    /// Path layer.
    pub fn node_shift_toggle(&mut self, ci: usize, ni: usize) {
        let Some((id, _, _)) = self.active_node_object() else {
            return;
        };
        match self.edit.node_selected {
            None => self.edit.node_selected = Some((id, ci, ni)),
            Some((lid, ..)) if lid != id => {
                // Selection was on another layer — start fresh on this one.
                self.edit.node_multi.clear();
                self.edit.node_selected = Some((id, ci, ni));
            }
            Some((lid, pc, pn)) => {
                if (ci, ni) == (pc, pn) {
                    // Deselect the primary: promote an extra, else clear.
                    match self.edit.node_multi.pop() {
                        Some((mc, mn)) => self.edit.node_selected = Some((lid, mc, mn)),
                        None => self.edit.node_selected = None,
                    }
                } else if let Some(pos) = self.edit.node_multi.iter().position(|&x| x == (ci, ni)) {
                    self.edit.node_multi.remove(pos);
                } else {
                    self.edit.node_multi.push((pc, pn));
                    self.edit.node_selected = Some((lid, ci, ni));
                }
            }
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// True while a rubber-band selection is being dragged.
    pub fn node_marquee_active(&self) -> bool {
        self.edit.node_marquee.is_some()
    }

    /// Begin a rubber-band selection at a screen point (press on empty canvas).
    pub fn node_marquee_start(&mut self, sx: f32, sy: f32) {
        self.edit.node_marquee = Some((sx, sy, sx, sy));
    }

    /// Extend the rubber-band to the current screen point.
    pub fn node_marquee_update(&mut self, sx: f32, sy: f32) {
        if let Some(m) = &mut self.edit.node_marquee {
            m.2 = sx;
            m.3 = sy;
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
    }

    /// Finish the rubber-band: select every anchor of the active Path whose screen
    /// position falls inside the rect. A click-sized rect just clears the selection.
    pub fn node_marquee_finish(&mut self) {
        let Some((x0, y0, x1, y1)) = self.edit.node_marquee.take() else {
            return;
        };
        let (lo_x, hi_x) = (x0.min(x1), x0.max(x1));
        let (lo_y, hi_y) = (y0.min(y1), y0.max(y1));
        if (hi_x - lo_x) < 3.0 && (hi_y - lo_y) < 3.0 {
            self.clear_node_selection();
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return;
        }
        let Some((id, t, path)) = self.node_edit_geometry() else {
            self.clear_node_selection();
            return;
        };
        let zoom = self.edit.view.zoom;
        let vox = self.edit.view.offset_x;
        let voy = self.edit.view.offset_y;
        let mut inside: Vec<(usize, usize)> = Vec::new();
        for (ci, c) in path.contours.iter().enumerate() {
            for (ni, node) in c.nodes.iter().enumerate() {
                let q = t.apply_point(node.anchor);
                let (sx, sy) = (q.x * zoom + vox, q.y * zoom + voy);
                if sx >= lo_x && sx <= hi_x && sy >= lo_y && sy <= hi_y {
                    inside.push((ci, ni));
                }
            }
        }
        if let Some((&first, rest)) = inside.split_first() {
            self.edit.node_selected = Some((id, first.0, first.1));
            self.edit.node_multi = rest.to_vec();
        } else {
            self.clear_node_selection();
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Align the multi-selected nodes onto a shared coordinate (Node options bar).
    /// Needs ≥2 selected nodes. The target is computed GLOBALLY across the whole
    /// selection (consistent even across contours); each node's handles move with
    /// its anchor so curvature is kept. One `ReplacePathGeometry` = one undo.
    pub fn node_align(&mut self, axis: Axis, reference: AlignRef) -> bool {
        let Some((layer_id, sel)) = self.node_effective_selection() else {
            return false;
        };
        if sel.len() < 2 {
            self.shell.status_msg = "Cần chọn ít nhất 2 điểm để căn".to_string();
            return true;
        }
        let Some((_, _t, mut path)) = self.active_node_object() else {
            return false;
        };
        let coord = |p: Point| match axis {
            Axis::Horizontal => p.y,
            Axis::Vertical => p.x,
        };
        let mut vals = Vec::with_capacity(sel.len());
        for &(c, n) in &sel {
            if let Some(node) = path.contours.get(c).and_then(|c| c.nodes.get(n)) {
                vals.push(coord(node.anchor));
            }
        }
        if vals.len() < 2 {
            return false;
        }
        let target = match reference {
            AlignRef::First => vals[0],
            AlignRef::Last => *vals.last().unwrap(),
            AlignRef::Min => vals.iter().copied().fold(f32::INFINITY, f32::min),
            AlignRef::Max => vals.iter().copied().fold(f32::NEG_INFINITY, f32::max),
            AlignRef::Average => vals.iter().sum::<f32>() / vals.len() as f32,
        };
        for &(c, n) in &sel {
            if let Some(node) = path.contours.get_mut(c).and_then(|c| c.nodes.get_mut(n)) {
                let (dx, dy) = match axis {
                    Axis::Horizontal => (0.0, target - node.anchor.y),
                    Axis::Vertical => (target - node.anchor.x, 0.0),
                };
                let shift = |p: Point| Point::new(p.x + dx, p.y + dy);
                node.anchor = shift(node.anchor);
                node.in_handle = node.in_handle.map(shift);
                node.out_handle = node.out_handle.map(shift);
            }
        }
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let _ = canvas.execute(
            Box::new(crate::core::command_vector::ReplacePathGeometry::new(
                layer_id, path,
            )),
            crate::core::gateway::ChangeKind::LayerStructure,
        );
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.shell.status_msg = "Đã căn các điểm".to_string();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        true
    }

    /// Commit a whole-geometry edit as ONE `ReplacePathGeometry` (structural), set a
    /// status line, and request a redraw. Shared by break/join.
    fn commit_node_geometry(&mut self, layer_id: u32, path: PathData, msg: &str) {
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let _ = canvas.execute(
            Box::new(crate::core::command_vector::ReplacePathGeometry::new(
                layer_id, path,
            )),
            crate::core::gateway::ChangeKind::LayerStructure,
        );
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.shell.status_msg = msg.to_string();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Break the active Path at the primary selected node — reopen a closed contour
    /// there, or split an open contour into two. Rejected at an open endpoint.
    pub fn node_break_at_selected(&mut self) -> bool {
        let Some((id, ci, ni)) = self.edit.node_selected else {
            self.shell.status_msg = "Chọn một điểm để tách".to_string();
            return false;
        };
        let Some((layer_id, _t, mut path)) = self.active_node_object() else {
            return false;
        };
        if layer_id != id {
            return false;
        }
        if crate::core::vector::ops::break_at_node(&mut path, ci, ni).is_err() {
            self.shell.status_msg = "Không tách được tại điểm đầu/cuối".to_string();
            return true;
        }
        self.clear_node_selection();
        self.commit_node_geometry(layer_id, path, "Đã tách đường");
        true
    }

    /// Join the two selected endpoints — close the active contour if they are its
    /// two ends, else weld two open contours into one. Needs exactly two selected
    /// nodes, both endpoints of open contours.
    pub fn node_join_selected(&mut self) -> bool {
        let Some((id, sel)) = self.node_effective_selection() else {
            return false;
        };
        let Some((layer_id, _t, mut path)) = self.active_node_object() else {
            return false;
        };
        if layer_id != id {
            return false;
        }
        if sel.len() != 2 {
            self.shell.status_msg = "Chọn đúng 2 điểm đầu/cuối để nối".to_string();
            return true;
        }
        let (a, b) = (sel[0], sel[1]);
        let endpoint_ok = |ci: usize, ni: usize| {
            path.contours.get(ci).is_some_and(|c| {
                !c.closed && c.nodes.len() >= 2 && (ni == 0 || ni + 1 == c.nodes.len())
            })
        };
        let (a_ok, b_ok) = (endpoint_ok(a.0, a.1), endpoint_ok(b.0, b.1));
        if !a_ok || !b_ok {
            self.shell.status_msg = "Nối cần 2 điểm đầu/cuối của đường mở".to_string();
            return true;
        }
        const WELD: f32 = 6.0;
        let res = if a.0 == b.0 {
            // Two ends of the same contour → close it.
            crate::core::vector::ops::close_contour(&mut path, a.0, WELD)
        } else {
            // Two open contours → orient so a's selected end is LAST and b's is
            // FIRST, then concatenate (welding coincident endpoints).
            if a.1 == 0 {
                path.contours[a.0].reverse();
            }
            if b.1 != 0 {
                path.contours[b.0].reverse();
            }
            crate::core::vector::ops::join_contours(&mut path, a.0, b.0, WELD)
        };
        if res.is_err() {
            return false;
        }
        self.clear_node_selection();
        self.commit_node_geometry(layer_id, path, "Đã nối đường");
        true
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
        // The focused (primary) node — its Bézier arms are drawn/grabbable. The
        // extra multi-selection is only highlighted, not armed.
        let primary = self
            .edit
            .node_selected
            .and_then(|(lid, c, n)| (lid == id).then_some((c, n)));
        let multi: &[(usize, usize)] = if self.edit.node_selected.map(|(lid, ..)| lid) == Some(id) {
            &self.edit.node_multi
        } else {
            &[]
        };

        let mut outlines: Vec<Vec<(f32, f32)>> = Vec::new();
        let outline_tol = node_outline_tolerance(t, self.edit.view.zoom);
        for c in &path.contours {
            // `flatten_contour` already returns the first anchor again for a
            // closed contour, so no extra closing point is needed here.
            let poly = crate::core::vector::flatten::flatten_contour(c, outline_tol);
            let line: Vec<(f32, f32)> = poly.iter().map(|p| map(*p)).collect();
            if line.len() >= 2 {
                outlines.push(line);
            }
        }

        let mut nodes: Vec<(f32, f32, bool)> = Vec::new();
        let mut handles: Vec<[f32; 4]> = Vec::new();
        for (ci, c) in path.contours.iter().enumerate() {
            for (ni, node) in c.nodes.iter().enumerate() {
                let (ax, ay) = map(node.anchor);
                let is_primary = primary == Some((ci, ni));
                let selected = is_primary || multi.contains(&(ci, ni));
                nodes.push((ax, ay, selected));
                if is_primary {
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
            marquee: self
                .edit
                .node_marquee
                .map(|(x0, y0, x1, y1)| [x0, y0, x1, y1]),
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
    /// `Handle` hit reshapes the curve; a `Node` hit drags that anchor; a
    /// `Segment` hit inserts an anchor there and drags the new one (or, with Alt
    /// held, converts the segment line↔curve without inserting). Returns true
    /// when the press was consumed.
    pub fn node_press(&mut self, hit: NodeHit, cx: f32, cy: f32) -> bool {
        // Alt + click a segment converts it line↔curve instead of inserting.
        if let NodeHit::Segment(ci, seg, _) = hit {
            if self.edit.input.alt_held {
                return self.node_convert_segment(ci, seg);
            }
        }
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

        // If this anchor belongs to a multi-selection, the whole selection moves
        // rigidly (group drag). Otherwise the press starts a fresh single
        // selection (and drops any previous multi-selection).
        let group: Vec<(usize, usize, crate::core::vector::path::Node)> =
            if matches!(target, NodeDragTarget::Anchor) {
                match self.node_effective_selection() {
                    Some((lid, sel))
                        if lid == id && sel.len() > 1 && sel.contains(&(contour, node)) =>
                    {
                        sel.iter()
                            .filter(|&&(c, n)| (c, n) != (contour, node))
                            .filter_map(|&(c, n)| {
                                pending
                                    .contours
                                    .get(c)
                                    .and_then(|cc| cc.nodes.get(n))
                                    .map(|nd| (c, n, *nd))
                            })
                            .collect()
                    }
                    _ => Vec::new(),
                }
            } else {
                Vec::new()
            };

        if group.is_empty() {
            // Fresh single-node selection (or a handle/insert gesture).
            self.edit.node_selected = Some((id, contour, node));
            self.edit.node_multi.clear();
        }
        // else: keep the existing multi-selection for the rigid group move.
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
            group,
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
                match d.target {
                    NodeDragTarget::Anchor => {
                        // Primary node follows the cursor rigidly.
                        let Some(slot) = d
                            .pending
                            .contours
                            .get_mut(d.contour)
                            .and_then(|c| c.nodes.get_mut(d.node))
                        else {
                            return;
                        };
                        *slot = shifted_node(&d.base_node, dx, dy);
                        // Any other selected nodes (a multi-selection) move by the
                        // same delta — possibly in other contours.
                        let grp = d.group.clone();
                        for (gc, gn, gbase) in grp {
                            if let Some(slot) = d
                                .pending
                                .contours
                                .get_mut(gc)
                                .and_then(|c| c.nodes.get_mut(gn))
                            {
                                *slot = shifted_node(&gbase, dx, dy);
                            }
                        }
                    }
                    NodeDragTarget::Handle(side) => {
                        // Reset to the node as it was at press, then move the
                        // grabbed handle rigidly with the cursor; the opposite
                        // handle is coupled per the node kind in core.
                        let Some(slot) = d
                            .pending
                            .contours
                            .get_mut(d.contour)
                            .and_then(|c| c.nodes.get_mut(d.node))
                        else {
                            return;
                        };
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
                    LayerType::Vector(VectorGeometry::Path(o)) => VectorObjectData {
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
            let LayerType::Vector(VectorGeometry::Path(o)) = &layer.layer_type else {
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

    /// Delete every selected node (Delete key under the Node tool) — the primary
    /// plus any multi-selection. Deletes high→low within each contour so indices
    /// stay valid, and never drops a contour below 2 nodes. Returns true when
    /// handled.
    pub fn node_delete_selected(&mut self) -> bool {
        let Some((id, sel)) = self.node_effective_selection() else {
            return false;
        };
        let Some((layer_id, _t, mut path)) = self.active_node_object() else {
            return false;
        };
        if layer_id != id {
            return false;
        }
        // Group selected indices by contour; delete descending so earlier indices
        // remain valid, keeping every contour at ≥2 nodes.
        use std::collections::BTreeMap;
        let mut by_contour: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (c, n) in sel {
            by_contour.entry(c).or_default().push(n);
        }
        let mut deleted = 0usize;
        for (c, mut idxs) in by_contour {
            let Some(contour) = path.contours.get_mut(c) else {
                continue;
            };
            idxs.sort_unstable_by(|a, b| b.cmp(a));
            idxs.dedup();
            for idx in idxs {
                if contour.nodes.len() <= 2 {
                    break;
                }
                if crate::core::vector::ops::delete_node(contour, idx).is_ok() {
                    deleted += 1;
                }
            }
        }
        if deleted == 0 {
            self.shell.status_msg = "Không thể xoá: đường cần ít nhất 2 điểm".to_string();
            return true;
        }
        self.clear_node_selection();
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let _ = canvas.execute(
            Box::new(crate::core::command_vector::ReplacePathGeometry::new(
                layer_id, path,
            )),
            crate::core::gateway::ChangeKind::LayerStructure,
        );
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.shell.status_msg = if deleted == 1 {
            "Đã xoá điểm".to_string()
        } else {
            format!("Đã xoá {deleted} điểm")
        };
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

    /// If the canvas point `(cx, cy)` is over the fill/outline of a Path layer
    /// that is not the current edit target, make that layer active and clear the
    /// node selection — Node/Shape-tool "click any object to edit it". Returns
    /// true when it switched the active layer.
    pub fn node_click_select_path(&mut self, cx: f32, cy: f32) -> bool {
        let Some(idx) = self.path_layer_hit_at(cx, cy) else {
            return false;
        };
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        if idx == canvas.layer_stack.active_idx {
            return false;
        }
        canvas.layer_stack.active_idx = idx;
        self.clear_node_selection();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        true
    }

    /// Toggle segment `seg` of contour `ci` between straight and curved (Alt+click
    /// a segment under the Node tool). Straight → default 1/3–2/3 handles the user
    /// can then bend; curved → drops the two facing handles. One
    /// [`crate::core::command_vector::ReplacePathGeometry`] = one undo.
    fn node_convert_segment(&mut self, ci: usize, seg: usize) -> bool {
        use crate::core::vector::ops;
        let Some((layer_id, _t, mut path)) = self.active_node_object() else {
            return false;
        };
        let Some(c) = path.contours.get_mut(ci) else {
            return false;
        };
        if seg >= c.segment_count() {
            return false;
        }
        let n = c.nodes.len();
        let straight =
            c.nodes[seg].out_handle.is_none() && c.nodes[(seg + 1) % n].in_handle.is_none();
        let (res, label) = if straight {
            (ops::set_segment_curved(c, seg), "Đoạn cong (Curve)")
        } else {
            (ops::set_segment_straight(c, seg), "Đoạn thẳng (Line)")
        };
        if res.is_err() {
            return false;
        }
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

    #[test]
    fn node_outline_tolerance_tracks_zoom_and_object_scale() {
        let identity = node_outline_tolerance(AffineTransform::IDENTITY, 1.0);
        let zoomed = node_outline_tolerance(AffineTransform::IDENTITY, 4.0);
        let scaled = node_outline_tolerance(AffineTransform::scale(4.0, 4.0), 1.0);

        assert!(zoomed < identity);
        assert!(scaled < identity);
        assert!((zoomed - identity / 4.0).abs() < 1e-6);
        // The Frobenius bound for a uniform 4× transform is 4√2, so it is
        // intentionally a little more conservative than the zoom-only case.
        assert!(scaled <= identity / 4.0);
    }
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
                .position(|l| matches!(l.layer_type, LayerType::Vector(VectorGeometry::Path(_))))
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
            LayerType::Vector(VectorGeometry::Path(o)) => o.path.clone(),
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
            LayerType::Vector(VectorGeometry::Path(_))
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
    fn alt_click_segment_toggles_line_curve_without_inserting_and_undoes() {
        let (mut app, id) = app_with_path();
        app.edit.input.alt_held = true;
        // Midpoint of the top edge (canvas 120,120) hits segment 0, which is straight.
        let hit = app.node_hit_at_screen(120.0, 120.0).expect("segment hit");
        assert!(matches!(hit, NodeHit::Segment(0, 0, _)));
        assert!(app.node_press(hit, 120.0, 120.0));
        let p = model_path(&app, id);
        assert_eq!(p.contours[0].nodes.len(), 4, "no node inserted");
        assert!(
            p.contours[0].nodes[0].out_handle.is_some()
                && p.contours[0].nodes[1].in_handle.is_some(),
            "segment 0 is now a curve"
        );
        // Alt+click again straightens it back.
        let hit = app.node_hit_at_screen(120.0, 120.0).expect("segment hit");
        assert!(app.node_press(hit, 120.0, 120.0));
        let p = model_path(&app, id);
        assert!(
            p.contours[0].nodes[0].out_handle.is_none()
                && p.contours[0].nodes[1].in_handle.is_none(),
            "segment 0 is straight again"
        );
        // Each toggle is its own undo step.
        app.docs.documents[0].canvas.undo().expect("undo");
        let p = model_path(&app, id);
        assert!(p.contours[0].nodes[0].out_handle.is_some(), "back to curve");
    }

    #[test]
    fn click_selects_another_path_as_edit_target() {
        use crate::core::command_vector::CreatePathLayer;
        use crate::core::gateway::ChangeKind;
        let (mut app, first_id) = app_with_path();
        // Add a second Path far from the first (a 40×20 rect at (10,10)).
        let second_id = {
            let canvas = &mut app.docs.documents[0].canvas;
            canvas
                .execute(
                    Box::new(CreatePathLayer::new(
                        rect_obj(40.0, 20.0, 10.0, 10.0),
                        "Path 2",
                    )),
                    ChangeKind::LayerStructure,
                )
                .unwrap();
            let idx = canvas.layer_stack.layers.len() - 1;
            canvas.layer_stack.layers[idx].id
        };
        // Make the FIRST path the edit target again.
        let first_idx = {
            let canvas = &mut app.docs.documents[0].canvas;
            let idx = canvas
                .layer_stack
                .layers
                .iter()
                .position(|l| l.id == first_id)
                .unwrap();
            canvas.layer_stack.active_idx = idx;
            idx
        };
        assert_eq!(
            app.active_path_layer(),
            Some(first_idx),
            "first path is active"
        );

        // Clicking inside the SECOND path's fill (canvas ~30,20) switches to it.
        assert!(app.node_click_select_path(30.0, 20.0));
        let active = app.docs.documents[0].canvas.layer_stack.active_idx;
        assert_eq!(
            app.docs.documents[0].canvas.layer_stack.layers[active].id, second_id,
            "second path is now the edit target"
        );
        // Clicking empty space (no path) does not switch.
        assert!(!app.node_click_select_path(280.0, 280.0));
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
            if let LayerType::Vector(VectorGeometry::Path(o)) =
                &mut canvas.layer_stack.layers[idx].layer_type
            {
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

    #[test]
    fn shift_toggle_builds_then_shrinks_the_multiselection() {
        let (mut app, id) = app_with_path();
        app.edit.node_selected = Some((id, 0, 0));
        // Shift-add node 2: it becomes primary, the old primary joins the extras.
        app.node_shift_toggle(0, 2);
        assert_eq!(app.edit.node_selected, Some((id, 0, 2)));
        assert_eq!(app.edit.node_multi, vec![(0, 0)]);
        // Shift-add node 1 too.
        app.node_shift_toggle(0, 1);
        assert_eq!(app.edit.node_selected, Some((id, 0, 1)));
        assert_eq!(app.edit.node_multi, vec![(0, 0), (0, 2)]);
        // Shift-click the primary again removes it; an extra is promoted.
        app.node_shift_toggle(0, 1);
        assert_eq!(app.edit.node_selected, Some((id, 0, 2)));
        assert_eq!(app.edit.node_multi, vec![(0, 0)]);
    }

    #[test]
    fn dragging_a_multiselected_node_moves_the_whole_group() {
        let (mut app, id) = app_with_path();
        app.edit.node_selected = Some((id, 0, 0)); // local (0,0)
        app.node_shift_toggle(0, 1); // add local (40,0); primary = node 1
                                     // Drag node 1 (canvas 140,120) by (+10,+5). Both selected nodes shift.
        let hit = app.node_hit_at_screen(140.0, 120.0).expect("node 1");
        assert!(matches!(hit, NodeHit::Node(0, 1)));
        app.node_press(hit, 140.0, 120.0);
        app.node_drag_update(150.0, 125.0);
        app.node_drag_finish();

        let p = model_path(&app, id);
        let n0 = p.contours[0].nodes[0].anchor;
        let n1 = p.contours[0].nodes[1].anchor;
        let n2 = p.contours[0].nodes[2].anchor;
        assert!(
            (n0.x - 10.0).abs() < 0.5 && (n0.y - 5.0).abs() < 0.5,
            "n0 {n0:?}"
        );
        assert!(
            (n1.x - 50.0).abs() < 0.5 && (n1.y - 5.0).abs() < 0.5,
            "n1 {n1:?}"
        );
        assert!(
            (n2.x - 40.0).abs() < 0.5 && (n2.y - 20.0).abs() < 0.5,
            "n2 unmoved"
        );
        // One undo returns the whole group.
        app.docs.documents[0].canvas.undo().expect("undo");
        let p = model_path(&app, id);
        assert!(
            p.contours[0].nodes[0]
                .anchor
                .distance_to(Point::new(0.0, 0.0))
                < 0.5
        );
        assert!(
            p.contours[0].nodes[1]
                .anchor
                .distance_to(Point::new(40.0, 0.0))
                < 0.5
        );
    }

    #[test]
    fn delete_removes_every_selected_node() {
        let (mut app, id) = app_with_path();
        app.edit.node_selected = Some((id, 0, 0));
        app.node_shift_toggle(0, 2); // selection = {0, 2}
        assert!(app.node_delete_selected());
        assert_eq!(
            model_path(&app, id).contours[0].nodes.len(),
            2,
            "two removed"
        );
        assert_eq!(app.edit.node_selected, None);
        assert!(app.edit.node_multi.is_empty());
        app.docs.documents[0].canvas.undo().expect("undo");
        assert_eq!(
            model_path(&app, id).contours[0].nodes.len(),
            4,
            "both restored"
        );
    }

    #[test]
    fn align_left_snaps_selected_nodes_to_min_x_and_undoes() {
        let (mut app, id) = app_with_path();
        app.edit.node_selected = Some((id, 0, 0)); // x = 0
        app.node_shift_toggle(0, 1); // add x = 40
        assert!(app.node_align(Axis::Vertical, AlignRef::Min));
        let p = model_path(&app, id);
        assert!((p.contours[0].nodes[0].anchor.x - 0.0).abs() < 0.5);
        assert!(
            (p.contours[0].nodes[1].anchor.x - 0.0).abs() < 0.5,
            "node 1 snapped to the min x"
        );
        // The aligned axis only; y is untouched.
        assert!((p.contours[0].nodes[1].anchor.y - 0.0).abs() < 0.5);
        app.docs.documents[0].canvas.undo().expect("undo");
        assert!((model_path(&app, id).contours[0].nodes[1].anchor.x - 40.0).abs() < 0.5);
    }

    #[test]
    fn align_below_two_nodes_is_a_handled_noop() {
        let (mut app, id) = app_with_path();
        app.edit.node_selected = Some((id, 0, 1));
        let before = model_path(&app, id);
        // Returns true (handled) but changes nothing with a single node selected.
        assert!(app.node_align(Axis::Horizontal, AlignRef::Average));
        assert_eq!(model_path(&app, id), before);
    }

    #[test]
    fn marquee_selects_the_enclosed_anchors() {
        let (mut app, id) = app_with_path();
        // At 1:1 / no view offset, canvas == screen. The rect's top edge nodes are
        // at (100,120) and (140,120); the bottom edge at y=140.
        app.node_marquee_start(90.0, 110.0);
        app.node_marquee_update(150.0, 130.0);
        app.node_marquee_finish();
        assert_eq!(app.edit.node_selected, Some((id, 0, 0)));
        assert_eq!(app.edit.node_multi, vec![(0, 1)], "only the two top nodes");
    }

    #[test]
    fn marquee_click_sized_clears_the_selection() {
        let (mut app, id) = app_with_path();
        app.edit.node_selected = Some((id, 0, 0));
        app.node_marquee_start(200.0, 200.0);
        app.node_marquee_update(201.0, 201.0); // < 3px → treated as a click
        app.node_marquee_finish();
        assert_eq!(app.edit.node_selected, None);
        assert!(app.edit.node_multi.is_empty());
    }

    #[test]
    fn break_reopens_a_closed_contour_and_undoes() {
        let (mut app, id) = app_with_path();
        app.edit.node_selected = Some((id, 0, 1));
        assert!(app.node_break_at_selected());
        let p = model_path(&app, id);
        assert!(
            !p.contours[0].closed,
            "closed rect reopened at the break node"
        );
        assert_eq!(
            p.contours[0].nodes.len(),
            5,
            "break node duplicated at both ends"
        );
        app.docs.documents[0].canvas.undo().expect("undo");
        assert!(
            model_path(&app, id).contours[0].closed,
            "undo restores the ring"
        );
    }

    #[test]
    fn join_closes_an_opened_contour() {
        let (mut app, id) = app_with_path();
        // Open the rect at an interior node, then join its two ends → closed again.
        app.edit.node_selected = Some((id, 0, 1));
        assert!(app.node_break_at_selected());
        let last = model_path(&app, id).contours[0].nodes.len() - 1;
        app.edit.node_selected = Some((id, 0, 0));
        app.node_shift_toggle(0, last);
        assert!(app.node_join_selected());
        assert!(
            model_path(&app, id).contours[0].closed,
            "the two ends welded the contour closed"
        );
    }

    #[test]
    fn join_needs_two_open_endpoints() {
        let (mut app, id) = app_with_path();
        // Two adjacent nodes of a CLOSED contour are not open endpoints.
        app.edit.node_selected = Some((id, 0, 0));
        app.node_shift_toggle(0, 1);
        assert!(app.node_join_selected(), "handled");
        assert!(
            model_path(&app, id).contours[0].closed,
            "nothing joined on a closed contour"
        );
    }
}

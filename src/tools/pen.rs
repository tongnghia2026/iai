#![allow(dead_code)]
//! Pen tool — place Bézier anchor points to draw a path, edit them, then commit.
//! Gestures (standard):
//!   - Click = corner anchor; click-and-drag = smooth anchor (symmetric handles).
//!   - Click the first anchor to close the path.
//!   - Alt-click an anchor = one-sided corner (drops the outgoing handle, keeps
//!     the incoming curve) so the next segment leaves straight.
//!   - Ctrl = edit mode: drag an anchor to move it, drag a handle to reshape
//!     (smooth/mirrored; add Alt to break the joint), or click on the path
//!     between anchors to insert a new smooth node.
//!   - Ctrl+Z while drawing steps the path back one anchor (`undo_last_anchor`).
//!   - Enter commits per `PenMode` — Selection (`Canvas::select_polygon_mode`),
//!     Fill (`Canvas::fill_polygon`) or Stroke (`Canvas::stroke_polyline`).
//!   - Ctrl+Enter always commits as a selection (`commit_as_selection`); Esc clears.
//!
//! The path math (cubic Bézier flatten) lives in `core::geometry`.

use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::geometry::{cubic_bezier, Point};
use crate::core::selection::SelectionMode;
use crate::core::vector::affine::AffineTransform;
use crate::core::vector::flatten;
use crate::core::vector::object::VectorObjectData;
use crate::core::vector::path::{Contour, FillRule, Node, NodeKind, PathData};
use crate::core::vector::style::VectorStyle;

/// Grab radius (screen px) for hitting an anchor or handle dot — clicking within
/// this of the first anchor closes the path, of any other moves it; Alt-click
/// resets it to a corner; Ctrl-drag a handle reshapes the curve.
const ANCHOR_HIT_PX: f32 = 10.0;
/// Fixed subdivisions used only by the interactive "insert node on path" hit-test
/// (`insert_anchor_on_path`) to find which segment was clicked. The rendered
/// polyline no longer uses a fixed step count — see [`PenTool::flatten`].
const FLATTEN_STEPS: usize = 24;
/// Flatness (canvas px) for the shared adaptive flattening of the rendered path.
/// Sub-pixel, so the polyline/selection is at least as accurate as the old fixed
/// 24-step subdivision on every curve size (finer on large curves).
const FLATTEN_TOLERANCE_PX: f32 = 0.2;

/// What committing the path does (Pen tool option bar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PenMode {
    /// Turn the closed path into a selection (default).
    Selection,
    /// Fill the closed path with the foreground colour.
    Fill,
    /// Stroke the path outline with the foreground colour.
    Stroke,
    /// Commit as an editable vector Path layer (Bước 4 / T4.4).
    Path,
}

impl PenMode {
    pub fn to_u8(self) -> u8 {
        match self {
            PenMode::Selection => 0,
            PenMode::Fill => 1,
            PenMode::Stroke => 2,
            PenMode::Path => 3,
        }
    }
    pub fn from_u8(v: u8) -> PenMode {
        match v {
            1 => PenMode::Fill,
            2 => PenMode::Stroke,
            3 => PenMode::Path,
            _ => PenMode::Selection,
        }
    }
}

/// Which Bézier handle of an anchor (the curve arriving = In, leaving = Out).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    In,
    Out,
}

/// The active press-drag gesture.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Drag {
    /// A freshly placed (or Alt-reset) anchor whose handles follow the cursor.
    Shape(usize),
    /// An existing anchor being repositioned (its point and handles translate).
    MoveAnchor(usize),
    /// One Bézier handle dragged on its own (Ctrl) to reshape the curve.
    MoveHandle(usize, Side),
}

/// One path anchor with optional absolute Bézier handles. `out_handle` controls
/// the curve leaving this anchor, `in_handle` the curve arriving at it. `None`
/// means a corner on that side (straight segment).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PenAnchor {
    pub pt: (f32, f32),
    pub in_handle: Option<(f32, f32)>,
    pub out_handle: Option<(f32, f32)>,
}

impl PenAnchor {
    fn corner(pt: (f32, f32)) -> Self {
        Self {
            pt,
            in_handle: None,
            out_handle: None,
        }
    }

    /// Convert to a vector-core [`Node`] (adapter task T3.1). Handles map 1:1 —
    /// both models store *absolute* control points — so the drawn curve is
    /// identical. The [`NodeKind`] is inferred from the handles (Pen doesn't track
    /// it) purely as an editing hint for the future Node tool; it does not affect
    /// rendering.
    fn to_node(self) -> Node {
        Node {
            anchor: self.pt.into(),
            in_handle: self.in_handle.map(Into::into),
            out_handle: self.out_handle.map(Into::into),
            kind: infer_node_kind(self.pt, self.in_handle, self.out_handle),
        }
    }
}

/// Classify a node from its handles: no handles → corner (Cusp); two handles
/// collinear through the anchor → Symmetric if equal length else Smooth;
/// anything else → Cusp. Rendering-neutral (used only as an editing hint).
fn infer_node_kind(
    pt: (f32, f32),
    in_h: Option<(f32, f32)>,
    out_h: Option<(f32, f32)>,
) -> NodeKind {
    let (i, o) = match (in_h, out_h) {
        (Some(i), Some(o)) => (i, o),
        _ => return NodeKind::Cusp,
    };
    // Vectors pointing away from the anchor along each handle.
    let vi = (i.0 - pt.0, i.1 - pt.1);
    let vo = (o.0 - pt.0, o.1 - pt.1);
    let li = (vi.0 * vi.0 + vi.1 * vi.1).sqrt();
    let lo = (vo.0 * vo.0 + vo.1 * vo.1).sqrt();
    if li < 1e-6 || lo < 1e-6 {
        return NodeKind::Cusp;
    }
    // Smooth requires the handles to be opposite (collinear, pointing apart).
    let cross = vi.0 * vo.1 - vi.1 * vo.0;
    let dot = vi.0 * vo.0 + vi.1 * vo.1;
    let opposite = cross.abs() <= 1e-3 * li * lo && dot < 0.0;
    if !opposite {
        NodeKind::Cusp
    } else if (li - lo).abs() <= 1e-3 * li.max(lo) {
        NodeKind::Symmetric
    } else {
        NodeKind::Smooth
    }
}

/// Adapt a list of Pen anchors into the shared vector-core [`PathData`] — one
/// contour (task T3.1). Empty input yields an empty path. Free function so it can
/// be unit-tested without a live `PenTool`.
pub fn pen_anchors_to_path(anchors: &[PenAnchor], closed: bool) -> PathData {
    if anchors.is_empty() {
        return PathData::default();
    }
    let nodes = anchors.iter().map(|a| a.to_node()).collect();
    PathData::new(vec![Contour::new(nodes, closed)], FillRule::NonZero)
}

pub struct PenTool {
    pub feather: f32,
    pub anti_alias: bool,
    /// What `commit` does with the path (Selection / Fill / Stroke).
    pub mode: PenMode,
    /// Line width (canvas px) for `PenMode::Stroke`.
    pub stroke_width: f32,
    anchors: Vec<PenAnchor>,
    closed: bool,
    /// The press-drag gesture in flight, or `None` between gestures.
    dragging: Option<Drag>,
    /// The anchor whose handles are shown for editing (set by Ctrl-click). Handles
    /// are otherwise hidden while drawing; the actively-dragged anchor also shows.
    selected: Option<usize>,
}

impl PenTool {
    pub fn new() -> Self {
        Self {
            feather: 0.0,
            anti_alias: true,
            mode: PenMode::Selection,
            stroke_width: 3.0,
            anchors: Vec::new(),
            closed: false,
            dragging: None,
            selected: None,
        }
    }

    /// Index of the anchor whose point is within `ANCHOR_HIT_PX` screen px of
    /// `(cx,cy)` (canvas space), nearest wins. Used for grab / close / delete.
    fn hit_anchor(&self, cx: f32, cy: f32, zoom: f32) -> Option<usize> {
        let z = zoom.max(0.01);
        let mut best = (f32::MAX, None);
        for (i, a) in self.anchors.iter().enumerate() {
            let d = ((cx - a.pt.0) * z).hypot((cy - a.pt.1) * z);
            if d < ANCHOR_HIT_PX && d < best.0 {
                best = (d, Some(i));
            }
        }
        best.1
    }

    /// The handle endpoint within `ANCHOR_HIT_PX` screen px of `(cx,cy)`, nearest
    /// wins. Only present anchors with handles are testable. Used for Ctrl editing.
    fn hit_handle(&self, cx: f32, cy: f32, zoom: f32) -> Option<(usize, Side)> {
        let z = zoom.max(0.01);
        let mut best = (f32::MAX, None);
        for (i, a) in self.anchors.iter().enumerate() {
            for (side, h) in [(Side::In, a.in_handle), (Side::Out, a.out_handle)] {
                if let Some((hx, hy)) = h {
                    let d = ((cx - hx) * z).hypot((cy - hy) * z);
                    if d < ANCHOR_HIT_PX && d < best.0 {
                        best = (d, Some((i, side)));
                    }
                }
            }
        }
        best.1
    }

    /// Anchor→handle line segments `[ax, ay, hx, hy]` for the overlay to draw the
    /// curve "arms" and their grab dots. Only the actively-dragged anchor, or the
    /// Ctrl-selected one, shows its handles — they stay hidden while drawing.
    pub fn handle_segments(&self) -> Vec<[f32; 4]> {
        let show = self
            .dragging
            .map(|d| match d {
                Drag::Shape(i) | Drag::MoveAnchor(i) | Drag::MoveHandle(i, _) => i,
            })
            .or(self.selected);
        let mut segs = Vec::new();
        if let Some(a) = show.and_then(|i| self.anchors.get(i)) {
            if let Some((hx, hy)) = a.in_handle {
                segs.push([a.pt.0, a.pt.1, hx, hy]);
            }
            if let Some((hx, hy)) = a.out_handle {
                segs.push([a.pt.0, a.pt.1, hx, hy]);
            }
        }
        segs
    }

    /// Ctrl+click on the path between two anchors inserts a node at the nearest
    /// point, splitting that segment. A curved segment is split with De Casteljau
    /// so the **existing curvature is preserved exactly** (the neighbours' near
    /// handles are re-anchored to the two sub-curves); a straight segment stays
    /// straight by inserting a plain corner. Returns whether a node was inserted
    /// (click within ~8 screen px of the line). Samples each segment so curved
    /// parts are matched too.
    fn insert_anchor_on_path(&mut self, cx: f32, cy: f32, zoom: f32) -> bool {
        let n = self.anchors.len();
        if n < 2 {
            return false;
        }
        let thresh = 8.0 / zoom.max(0.01);
        let seg_count = if self.closed { n } else { n - 1 };
        // (dist², segment index, parameter t along that segment)
        let mut best: Option<(f32, usize, f32)> = None;
        for s in 0..seg_count {
            let a = self.anchors[s];
            let b = self.anchors[(s + 1) % n];
            let straight = a.out_handle.is_none() && b.in_handle.is_none();
            let p0: Point = a.pt.into();
            let c1: Point = a.out_handle.unwrap_or(a.pt).into();
            let c2: Point = b.in_handle.unwrap_or(b.pt).into();
            let p3: Point = b.pt.into();
            for k in 1..FLATTEN_STEPS {
                let t = k as f32 / FLATTEN_STEPS as f32;
                let pt = if straight {
                    (
                        a.pt.0 + (b.pt.0 - a.pt.0) * t,
                        a.pt.1 + (b.pt.1 - a.pt.1) * t,
                    )
                } else {
                    let p = cubic_bezier(p0, c1, c2, p3, t);
                    (p.x, p.y)
                };
                let d2 = (cx - pt.0).powi(2) + (cy - pt.1).powi(2);
                if d2 < best.map_or(f32::MAX, |bb| bb.0) {
                    best = Some((d2, s, t));
                }
            }
        }
        let (d2, s, t) = match best {
            Some(b) => b,
            None => return false,
        };
        if d2.sqrt() > thresh {
            return false;
        }

        let bi = (s + 1) % n;
        let a = self.anchors[s];
        let b = self.anchors[bi];
        let new_anchor = if a.out_handle.is_none() && b.in_handle.is_none() {
            // Straight stays straight: a plain corner on the line.
            let pt = (
                a.pt.0 + (b.pt.0 - a.pt.0) * t,
                a.pt.1 + (b.pt.1 - a.pt.1) * t,
            );
            PenAnchor::corner(pt)
        } else {
            // De Casteljau split at t: the two sub-curves together reproduce the
            // original cubic, so the curve the user already drew does not move.
            let lerp =
                |u: (f32, f32), v: (f32, f32)| (u.0 + (v.0 - u.0) * t, u.1 + (v.1 - u.1) * t);
            let p0 = a.pt;
            let p1 = a.out_handle.unwrap_or(a.pt);
            let p2 = b.in_handle.unwrap_or(b.pt);
            let p3 = b.pt;
            let p01 = lerp(p0, p1);
            let p12 = lerp(p1, p2);
            let p23 = lerp(p2, p3);
            let p012 = lerp(p01, p12);
            let p123 = lerp(p12, p23);
            let mid = lerp(p012, p123);
            // Re-anchor the neighbours' near handles to the sub-curves. Skip a side
            // that was a corner (its split handle coincides with the anchor, which
            // flatten() already reproduces) so corners stay corners.
            if a.out_handle.is_some() {
                self.anchors[s].out_handle = Some(p01);
            }
            if b.in_handle.is_some() {
                self.anchors[bi].in_handle = Some(p23);
            }
            PenAnchor {
                pt: mid,
                in_handle: Some(p012),
                out_handle: Some(p123),
            }
        };

        let idx = s + 1;
        self.anchors.insert(idx, new_anchor);
        self.selected = Some(idx);
        true
    }

    /// Step the in-progress path back one edit (Ctrl+Z while drawing): a closed
    /// path re-opens first, otherwise the last anchor is dropped. Returns whether
    /// anything changed (so the caller can skip the global undo when it did).
    pub fn undo_last_anchor(&mut self) -> bool {
        self.dragging = None;
        self.selected = None;
        if self.closed {
            self.closed = false;
            return true;
        }
        self.anchors.pop().is_some()
    }

    /// Commit the path to a selection regardless of `mode` (Ctrl+Enter, standard raster editors
    /// "load path as selection"). A ≥3-point path is treated as closed.
    pub fn commit_as_selection(&mut self, canvas: &mut crate::core::canvas::Canvas) {
        if self.anchors.len() >= 3 {
            self.closed = true;
            let pts = self.flatten();
            if pts.len() >= 3 {
                canvas.select_polygon_mode(&pts, SelectionMode::New, self.feather, self.anti_alias);
            }
        }
        self.clear();
    }

    pub fn anchors(&self) -> &[PenAnchor] {
        &self.anchors
    }
    pub fn is_closed(&self) -> bool {
        self.closed
    }
    pub fn is_empty(&self) -> bool {
        self.anchors.is_empty()
    }

    /// Adapt the current anchors into the shared vector-core [`PathData`] — one
    /// contour (task T3.1). The bridge that lets Pen reuse the vector-core
    /// flatten/hit-test instead of a second geometry model; also the basis for a
    /// future "commit as Path layer" (task T4.4).
    pub fn to_path_data(&self) -> PathData {
        pen_anchors_to_path(&self.anchors, self.closed)
    }

    /// Flatten the path to a polyline in canvas space. Straight where both
    /// adjacent handles are absent, cubic Bézier otherwise. Routed through the
    /// shared adaptive flattener (task T3.2) — no fixed step count — so the
    /// polyline (and the selection/fill built from it) is sub-pixel accurate at
    /// any curve size.
    pub fn flatten(&self) -> Vec<(f32, f32)> {
        let n = self.anchors.len();
        if n == 0 {
            return Vec::new();
        }
        if n == 1 {
            return vec![self.anchors[0].pt];
        }
        match self.to_path_data().contours.first() {
            Some(c) => flatten::flatten_contour(c, FLATTEN_TOLERANCE_PX)
                .into_iter()
                .map(|p| (p.x, p.y))
                .collect(),
            None => Vec::new(),
        }
    }

    /// Flattened points the app draws as the live path overlay — only the anchors
    /// placed so far (no segment is drawn to the cursor until the next click).
    pub fn preview_points(&self) -> Vec<(f32, f32)> {
        self.flatten()
    }

    fn close_path(&mut self) {
        if self.anchors.len() >= 3 {
            self.closed = true;
        }
    }

    /// Commit the path using the active `mode` and the foreground `color`.
    /// Selection/Fill treat a ≥3-point path as implicitly closed; Stroke follows
    /// the path as drawn (open unless closed) and needs only ≥2 points.
    pub fn commit(&mut self, canvas: &mut crate::core::canvas::Canvas, color: [u8; 4]) {
        match self.mode {
            PenMode::Stroke => {
                let pts = self.flatten();
                if pts.len() >= 2 {
                    canvas.stroke_polyline(&pts, self.closed, color, self.stroke_width.max(1.0));
                }
            }
            PenMode::Selection | PenMode::Fill => {
                if self.anchors.len() >= 3 {
                    self.closed = true;
                    let pts = self.flatten();
                    if pts.len() >= 3 {
                        if self.mode == PenMode::Fill {
                            canvas.fill_polygon(&pts, color, self.feather);
                        } else {
                            canvas.select_polygon_mode(
                                &pts,
                                SelectionMode::New,
                                self.feather,
                                self.anti_alias,
                            );
                        }
                    }
                }
            }
            // Committing a Path layer needs app-level invalidation (a new layer),
            // so the app calls `take_path_object` directly. Reaching here without
            // that routing leaves the path intact instead of dropping it.
            PenMode::Path => return,
        }
        self.clear();
    }

    /// Take the drawn path as an editable vector object for a Path-layer commit
    /// (Bước 4 / T4.4), clearing the in-progress path. `None` when there are fewer
    /// than two anchors (no segment). A ≥3-anchor path is closed like
    /// Selection/Fill so a drawn shape fills as expected. The object starts at the
    /// default style (solid black fill) and identity transform; geometry is in
    /// canvas space, which for the single implicit page equals layer space
    /// (Mục 3.4 / 3.13).
    pub fn take_path_object(&mut self) -> Option<VectorObjectData> {
        if self.anchors.len() >= 3 {
            self.closed = true;
        }
        if self.anchors.len() < 2 {
            return None;
        }
        let path = self.to_path_data();
        self.clear();
        Some(VectorObjectData::new(
            path,
            VectorStyle::default(),
            AffineTransform::IDENTITY,
        ))
    }

    pub fn clear(&mut self) {
        self.anchors.clear();
        self.closed = false;
        self.dragging = None;
        self.selected = None;
    }
}

impl Default for PenTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for PenTool {
    fn id(&self) -> &'static str {
        "pen"
    }
    fn name(&self) -> &str {
        "Pen"
    }
    fn icon(&self) -> &'static str {
        "vector"
    }
    fn shortcut(&self) -> Option<char> {
        Some('P')
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::Pen
    }

    fn on_press(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        let (cx, cy) = (event.canvas_x, event.canvas_y);

        // Ctrl = edit mode (Direct Selection): reshape a handle, move an anchor, or
        // insert a node on the path. It never extends the path (so Ctrl+click on a
        // segment adds a point instead of continuing to draw).
        if event.ctrl {
            if let Some((i, side)) = self.hit_handle(cx, cy, ctx.zoom) {
                self.selected = Some(i);
                self.dragging = Some(Drag::MoveHandle(i, side));
                return ToolResponse::redraw();
            }
            if let Some(i) = self.hit_anchor(cx, cy, ctx.zoom) {
                // Ctrl-click a node reveals its handles for adjustment.
                self.selected = Some(i);
                self.dragging = Some(Drag::MoveAnchor(i));
                return ToolResponse::redraw();
            }
            if self.insert_anchor_on_path(cx, cy, ctx.zoom) {
                self.dragging = None;
                return ToolResponse::redraw();
            }
            self.selected = None;
            self.dragging = None;
            return ToolResponse::none();
        }

        // A finished (closed) path: a new plain press starts a fresh path.
        if self.closed {
            self.clear();
        }

        // Hitting an existing anchor edits it instead of adding a new point.
        if let Some(i) = self.hit_anchor(cx, cy, ctx.zoom) {
            if event.alt {
                // Alt-click = one-sided corner: drop only the outgoing handle (the
                // un-drawn side) and keep the incoming curve, so the next segment
                // leaves straight without losing the curve already drawn.
                self.anchors[i].out_handle = None;
                self.selected = Some(i);
                self.dragging = None;
                return ToolResponse::redraw();
            }
            if i == 0 && self.anchors.len() >= 3 && !self.closed {
                // Clicking back on the first anchor closes the path.
                self.close_path();
                self.selected = None;
                self.dragging = None;
                return ToolResponse::redraw();
            }
            // Otherwise grab the anchor to reposition it.
            self.selected = Some(i);
            self.dragging = Some(Drag::MoveAnchor(i));
            return ToolResponse::redraw();
        }

        // Empty space: append a new corner anchor (a press-drag smooths it). The
        // newest anchor becomes the selected one, so its handles stay visible until
        // the next point is placed.
        self.anchors.push(PenAnchor::corner((cx, cy)));
        let last = self.anchors.len() - 1;
        self.selected = Some(last);
        self.dragging = Some(Drag::Shape(last));
        ToolResponse::redraw()
    }

    fn on_drag(
        &mut self,
        event: PointerEvent,
        _prev: &PointerEvent,
        _ctx: &mut ToolCtx,
    ) -> ToolResponse {
        let (cx, cy) = (event.canvas_x, event.canvas_y);
        match self.dragging {
            Some(Drag::Shape(i)) => {
                // Drag sets symmetric handles → a smooth anchor.
                let (ax, ay) = self.anchors[i].pt;
                self.anchors[i].out_handle = Some((cx, cy));
                self.anchors[i].in_handle = Some((2.0 * ax - cx, 2.0 * ay - cy));
                ToolResponse::redraw()
            }
            Some(Drag::MoveAnchor(i)) => {
                // Translate the anchor point and carry its handles along.
                let (ax, ay) = self.anchors[i].pt;
                let (dx, dy) = (cx - ax, cy - ay);
                self.anchors[i].pt = (cx, cy);
                if let Some(h) = self.anchors[i].in_handle {
                    self.anchors[i].in_handle = Some((h.0 + dx, h.1 + dy));
                }
                if let Some(h) = self.anchors[i].out_handle {
                    self.anchors[i].out_handle = Some((h.0 + dx, h.1 + dy));
                }
                ToolResponse::redraw()
            }
            Some(Drag::MoveHandle(i, side)) => {
                if event.alt {
                    // Ctrl+Alt: break the joint — move only the grabbed handle and
                    // leave the opposite one (and the curve it already drew) intact.
                    match side {
                        Side::In => self.anchors[i].in_handle = Some((cx, cy)),
                        Side::Out => self.anchors[i].out_handle = Some((cx, cy)),
                    }
                } else {
                    // Ctrl: smooth point — the opposite handle mirrors across the
                    // anchor so both sides of the curve update together.
                    let (ax, ay) = self.anchors[i].pt;
                    let mirror = (2.0 * ax - cx, 2.0 * ay - cy);
                    match side {
                        Side::In => {
                            self.anchors[i].in_handle = Some((cx, cy));
                            self.anchors[i].out_handle = Some(mirror);
                        }
                        Side::Out => {
                            self.anchors[i].out_handle = Some((cx, cy));
                            self.anchors[i].in_handle = Some(mirror);
                        }
                    }
                }
                ToolResponse::redraw()
            }
            None => ToolResponse::none(),
        }
    }

    fn on_release(&mut self, _event: PointerEvent, _ctx: &mut ToolCtx) -> ToolResponse {
        self.dragging = None;
        ToolResponse::redraw()
    }

    fn on_cancel(&mut self) {
        self.clear();
    }

    fn on_confirm(&mut self, ctx: &mut ToolCtx) {
        let color = ctx.fg_color;
        self.commit(ctx.canvas_mut(), color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::document::{Document, DocumentId};

    fn ctx(doc: &mut Document) -> ToolCtx<'_> {
        ToolCtx::new(doc, [0, 0, 0, 255], [255, 255, 255, 255], 1.0, 0.0, 0.0)
    }

    #[test]
    fn corner_clicks_build_straight_polyline() {
        let mut t = PenTool::new();
        let mut doc = Document::new(DocumentId(1), 200, 200);
        let mut c = ctx(&mut doc);
        for &(x, y) in &[(10.0, 10.0), (100.0, 10.0), (100.0, 100.0)] {
            let e = PointerEvent::new(x, y);
            t.on_press(e, &mut c);
            t.on_release(e, &mut c);
        }
        // straight segments → exactly the 3 corners
        assert_eq!(
            t.flatten(),
            vec![(10.0, 10.0), (100.0, 10.0), (100.0, 100.0)]
        );
    }

    #[test]
    fn drag_makes_a_smooth_anchor_with_mirrored_handles() {
        let mut t = PenTool::new();
        let mut doc = Document::new(DocumentId(1), 200, 200);
        let mut c = ctx(&mut doc);
        let e = PointerEvent::new(50.0, 50.0);
        t.on_press(e, &mut c);
        t.on_drag(PointerEvent::new(70.0, 50.0), &e, &mut c);
        t.on_release(e, &mut c);
        let a = t.anchors()[0];
        assert_eq!(a.out_handle, Some((70.0, 50.0)));
        assert_eq!(a.in_handle, Some((30.0, 50.0))); // mirror around (50,50)
    }

    #[test]
    fn curved_segment_subdivides() {
        let mut t = PenTool::new();
        t.anchors = vec![
            PenAnchor {
                pt: (0.0, 0.0),
                in_handle: None,
                out_handle: Some((0.0, 50.0)),
            },
            PenAnchor {
                pt: (100.0, 0.0),
                in_handle: Some((100.0, 50.0)),
                out_handle: None,
            },
        ];
        let pts = t.flatten();
        // Adaptive flatten (no fixed step count): a curved segment yields several
        // on-curve points — more than the 2 a straight segment produces.
        assert!(
            pts.len() > 4,
            "curve should subdivide adaptively: {}",
            pts.len()
        );
        // The curve bulges downward (positive y) between the endpoints.
        let mid = pts[pts.len() / 2];
        assert!(mid.1 > 10.0, "curve should bow out: {mid:?}");
        // Every vertex lies on the true cubic (shape unchanged by the new flatten).
        let (p0, c1, c2, p3): (Point, Point, Point, Point) = (
            (0.0, 0.0).into(),
            (0.0, 50.0).into(),
            (100.0, 50.0).into(),
            (100.0, 0.0).into(),
        );
        for &(vx, vy) in &pts {
            let d = (0..=400)
                .map(|k| {
                    let p = cubic_bezier(p0, c1, c2, p3, k as f32 / 400.0);
                    (p.x - vx).hypot(p.y - vy)
                })
                .fold(f32::MAX, f32::min);
            assert!(d < 0.5, "vertex ({vx},{vy}) drifted off curve by {d}");
        }
    }

    #[test]
    fn adapter_preserves_anchor_structure() {
        // T3.1: PenAnchor → PathData maps handles and closed 1:1.
        let anchors = vec![
            PenAnchor {
                pt: (0.0, 0.0),
                in_handle: None,
                out_handle: Some((10.0, 10.0)),
            },
            PenAnchor {
                pt: (50.0, 0.0),
                in_handle: Some((40.0, 10.0)),
                out_handle: None,
            },
        ];
        let path = pen_anchors_to_path(&anchors, true);
        assert_eq!(path.contours.len(), 1);
        let c = &path.contours[0];
        assert!(c.closed);
        assert_eq!(c.nodes.len(), 2);
        assert_eq!(c.nodes[0].anchor, Point::new(0.0, 0.0));
        assert_eq!(c.nodes[0].out_handle, Some(Point::new(10.0, 10.0)));
        assert_eq!(c.nodes[1].in_handle, Some(Point::new(40.0, 10.0)));
        // Empty input → empty path.
        assert!(pen_anchors_to_path(&[], false).contours.is_empty());
    }

    #[test]
    fn adapter_segment_matches_pen_bezier() {
        // The adapted segment's control points equal the Pen's own p0/c1/c2/p3
        // (out_handle→c1, in_handle→c2), so the drawn curve is identical.
        let anchors = vec![
            PenAnchor {
                pt: (0.0, 0.0),
                in_handle: None,
                out_handle: Some((0.0, 80.0)),
            },
            PenAnchor {
                pt: (100.0, 0.0),
                in_handle: Some((100.0, 80.0)),
                out_handle: None,
            },
        ];
        let path = pen_anchors_to_path(&anchors, false);
        let seg = path.contours[0].segment(0).unwrap();
        assert_eq!(
            seg,
            (
                Point::new(0.0, 0.0),
                Point::new(0.0, 80.0),
                Point::new(100.0, 80.0),
                Point::new(100.0, 0.0),
            )
        );
    }

    #[test]
    fn adapter_infers_smooth_kinds() {
        // Mirror handles → Symmetric; unequal collinear → Smooth; none → Cusp.
        let sym = PenAnchor {
            pt: (50.0, 50.0),
            in_handle: Some((30.0, 50.0)),
            out_handle: Some((70.0, 50.0)),
        }
        .to_node();
        assert_eq!(sym.kind, NodeKind::Symmetric);
        let smooth = PenAnchor {
            pt: (50.0, 50.0),
            in_handle: Some((40.0, 50.0)),
            out_handle: Some((70.0, 50.0)),
        }
        .to_node();
        assert_eq!(smooth.kind, NodeKind::Smooth);
        let cusp = PenAnchor::corner((10.0, 10.0)).to_node();
        assert_eq!(cusp.kind, NodeKind::Cusp);
    }

    #[test]
    fn click_near_first_anchor_closes() {
        let mut t = PenTool::new();
        let mut doc = Document::new(DocumentId(1), 200, 200);
        let mut c = ctx(&mut doc);
        for &(x, y) in &[(10.0, 10.0), (100.0, 10.0), (100.0, 100.0)] {
            let e = PointerEvent::new(x, y);
            t.on_press(e, &mut c);
            t.on_release(e, &mut c);
        }
        // press back on the first anchor
        let e = PointerEvent::new(11.0, 11.0);
        t.on_press(e, &mut c);
        assert!(t.is_closed());
    }

    #[test]
    fn commit_makes_a_selection() {
        let mut t = PenTool::new();
        let mut doc = Document::new(DocumentId(1), 200, 200);
        t.anchors = vec![
            PenAnchor::corner((20.0, 20.0)),
            PenAnchor::corner((160.0, 30.0)),
            PenAnchor::corner((140.0, 150.0)),
            PenAnchor::corner((30.0, 140.0)),
        ];
        {
            let mut c = ctx(&mut doc);
            t.on_confirm(&mut c);
        }
        assert!(doc.canvas.selection.active);
        assert!(t.is_empty()); // cleared after commit
    }

    #[test]
    fn take_path_object_builds_closed_shape_and_clears() {
        let mut t = PenTool::new();
        t.anchors = vec![
            PenAnchor::corner((20.0, 20.0)),
            PenAnchor::corner((160.0, 30.0)),
            PenAnchor::corner((140.0, 150.0)),
        ];
        let obj = t.take_path_object().expect("object");
        assert!(t.is_empty(), "pen cleared after commit");
        let c = &obj.path.contours[0];
        assert!(c.closed, "≥3-anchor path closes like Selection/Fill");
        assert_eq!(c.nodes.len(), 3);
        // Default style is the visible solid-black fill.
        assert!(obj.style.fill.is_visible());
        assert_eq!(obj.transform, AffineTransform::IDENTITY);
    }

    #[test]
    fn take_path_object_needs_two_anchors() {
        let mut t = PenTool::new();
        t.anchors = vec![PenAnchor::corner((20.0, 20.0))];
        assert!(t.take_path_object().is_none());
    }

    #[test]
    fn drag_existing_anchor_moves_it() {
        let mut t = PenTool::new();
        let mut doc = Document::new(DocumentId(1), 200, 200);
        let mut c = ctx(&mut doc);
        for &(x, y) in &[(20.0, 20.0), (120.0, 20.0), (120.0, 120.0)] {
            let e = PointerEvent::new(x, y);
            t.on_press(e, &mut c);
            t.on_release(e, &mut c);
        }
        // Grab the 2nd anchor and drag it — no new anchor is added.
        let press = PointerEvent::new(120.0, 20.0);
        t.on_press(press, &mut c);
        t.on_drag(PointerEvent::new(140.0, 60.0), &press, &mut c);
        t.on_release(PointerEvent::new(140.0, 60.0), &mut c);
        assert_eq!(t.anchors().len(), 3);
        assert_eq!(t.anchors()[1].pt, (140.0, 60.0));
    }

    #[test]
    fn alt_click_keeps_incoming_curve() {
        let mut t = PenTool::new();
        let mut doc = Document::new(DocumentId(1), 200, 200);
        let mut c = ctx(&mut doc);
        // Place a smooth anchor (press + drag gives it both handles).
        let e = PointerEvent::new(50.0, 50.0);
        t.on_press(e, &mut c);
        t.on_drag(PointerEvent::new(70.0, 50.0), &e, &mut c);
        t.on_release(e, &mut c);
        // Alt-click drops ONLY the outgoing handle; the incoming curve survives.
        let mut alt = PointerEvent::new(50.0, 50.0);
        alt.alt = true;
        t.on_press(alt, &mut c);
        t.on_release(alt, &mut c);
        assert_eq!(t.anchors().len(), 1);
        assert!(t.anchors()[0].out_handle.is_none());
        assert_eq!(t.anchors()[0].in_handle, Some((30.0, 50.0)));
    }

    #[test]
    fn latest_point_shows_handles_until_next_click() {
        let mut t = PenTool::new();
        let mut doc = Document::new(DocumentId(1), 200, 200);
        let mut c = ctx(&mut doc);
        // Draw a smooth anchor; as the latest point it keeps showing its handles.
        let e = PointerEvent::new(50.0, 50.0);
        t.on_press(e, &mut c);
        t.on_drag(PointerEvent::new(70.0, 50.0), &e, &mut c);
        t.on_release(e, &mut c);
        assert!(!t.handle_segments().is_empty());
        // Placing the next point (a corner) hides the previous node's handles.
        let e2 = PointerEvent::new(120.0, 50.0);
        t.on_press(e2, &mut c);
        t.on_release(e2, &mut c);
        assert!(t.handle_segments().is_empty());
        // Ctrl-click the first node → its handles reappear for adjustment.
        let mut ctrl = PointerEvent::new(50.0, 50.0);
        ctrl.ctrl = true;
        t.on_press(ctrl, &mut c);
        assert!(!t.handle_segments().is_empty());
    }

    #[test]
    fn ctrl_click_on_straight_inserts_corner() {
        let mut t = PenTool::new();
        let mut doc = Document::new(DocumentId(1), 200, 200);
        let mut c = ctx(&mut doc);
        t.anchors = vec![
            PenAnchor::corner((0.0, 50.0)),
            PenAnchor::corner((100.0, 50.0)),
        ];
        // Ctrl-click on the straight segment near its middle → new node.
        let mut ctrl = PointerEvent::new(50.0, 52.0);
        ctrl.ctrl = true;
        t.on_press(ctrl, &mut c);
        assert_eq!(t.anchors().len(), 3);
        let mid = t.anchors()[1];
        assert!((mid.pt.0 - 50.0).abs() < 6.0);
        // A straight segment has no curvature to preserve: the node is a corner and
        // the neighbours stay corners, so the line stays straight.
        assert!(mid.in_handle.is_none() && mid.out_handle.is_none());
        assert!(t.anchors()[0].out_handle.is_none());
        assert!(t.anchors()[2].in_handle.is_none());
        // Ctrl-click far from the path inserts nothing.
        let mut far = PointerEvent::new(50.0, 150.0);
        far.ctrl = true;
        t.on_press(far, &mut c);
        assert_eq!(t.anchors().len(), 3);
    }

    #[test]
    fn ctrl_click_on_curve_preserves_shape() {
        let mut t = PenTool::new();
        let mut doc = Document::new(DocumentId(1), 200, 200);
        let mut c = ctx(&mut doc);
        // A single curved segment bowing downward.
        t.anchors = vec![
            PenAnchor {
                pt: (0.0, 0.0),
                in_handle: None,
                out_handle: Some((0.0, 80.0)),
            },
            PenAnchor {
                pt: (100.0, 0.0),
                in_handle: Some((100.0, 80.0)),
                out_handle: None,
            },
        ];
        // Densely sample the ORIGINAL cubic for reference.
        let (p0, c1, c2, p3): (Point, Point, Point, Point) = (
            (0.0, 0.0).into(),
            (0.0, 80.0).into(),
            (100.0, 80.0).into(),
            (100.0, 0.0).into(),
        );
        let orig: Vec<(f32, f32)> = (0..=400)
            .map(|k| {
                let p = cubic_bezier(p0, c1, c2, p3, k as f32 / 400.0);
                (p.x, p.y)
            })
            .collect();

        // Insert a node by Ctrl-clicking right next to the curve's midpoint.
        let mp = cubic_bezier(p0, c1, c2, p3, 0.5);
        let mut ctrl = PointerEvent::new(mp.x, mp.y + 1.0);
        ctrl.ctrl = true;
        t.on_press(ctrl, &mut c);
        assert_eq!(t.anchors().len(), 3, "a node was inserted");

        // Every vertex of the re-split path must still lie on the original curve —
        // i.e. the existing curvature did not change.
        for v in t.flatten() {
            let d = orig
                .iter()
                .map(|o| (o.0 - v.0).hypot(o.1 - v.1))
                .fold(f32::MAX, f32::min);
            assert!(
                d < 1.0,
                "vertex {v:?} drifted off the original curve by {d}"
            );
        }
    }

    #[test]
    fn ctrl_drag_mirrors_both_handles() {
        let mut t = PenTool::new();
        let mut doc = Document::new(DocumentId(1), 200, 200);
        let mut c = ctx(&mut doc);
        // Smooth anchor at (50,50) with out_handle (70,50), in_handle (30,50).
        let e = PointerEvent::new(50.0, 50.0);
        t.on_press(e, &mut c);
        t.on_drag(PointerEvent::new(70.0, 50.0), &e, &mut c);
        t.on_release(e, &mut c);
        // Ctrl-grab the out handle and drag it; the in handle mirrors across (50,50).
        let mut ctrl = PointerEvent::new(70.0, 50.0);
        ctrl.ctrl = true;
        t.on_press(ctrl, &mut c);
        t.on_drag(PointerEvent::new(70.0, 90.0), &ctrl, &mut c);
        assert_eq!(t.anchors()[0].out_handle, Some((70.0, 90.0)));
        assert_eq!(t.anchors()[0].in_handle, Some((30.0, 10.0))); // mirror of (70,90)
    }

    #[test]
    fn ctrl_alt_drag_breaks_only_the_grabbed_handle() {
        let mut t = PenTool::new();
        let mut doc = Document::new(DocumentId(1), 200, 200);
        let mut c = ctx(&mut doc);
        // Smooth anchor at (50,50): out_handle (70,50), in_handle (30,50).
        let e = PointerEvent::new(50.0, 50.0);
        t.on_press(e, &mut c);
        t.on_drag(PointerEvent::new(70.0, 50.0), &e, &mut c);
        t.on_release(e, &mut c);
        // Ctrl grabs the out handle; Alt held during the drag breaks the joint.
        let mut ctrl = PointerEvent::new(70.0, 50.0);
        ctrl.ctrl = true;
        t.on_press(ctrl, &mut c);
        let mut ctrl_alt = PointerEvent::new(70.0, 90.0);
        ctrl_alt.ctrl = true;
        ctrl_alt.alt = true;
        t.on_drag(ctrl_alt, &ctrl, &mut c);
        assert_eq!(t.anchors()[0].out_handle, Some((70.0, 90.0)));
        assert_eq!(t.anchors()[0].in_handle, Some((30.0, 50.0))); // untouched
    }

    #[test]
    fn undo_last_anchor_steps_back() {
        let mut t = PenTool::new();
        let mut doc = Document::new(DocumentId(1), 200, 200);
        let mut c = ctx(&mut doc);
        for &(x, y) in &[(20.0, 20.0), (120.0, 20.0), (120.0, 120.0)] {
            let e = PointerEvent::new(x, y);
            t.on_press(e, &mut c);
            t.on_release(e, &mut c);
        }
        assert!(t.undo_last_anchor());
        assert_eq!(t.anchors().len(), 2);
        assert!(t.undo_last_anchor());
        assert!(t.undo_last_anchor());
        assert!(t.is_empty());
        assert!(!t.undo_last_anchor()); // nothing left
    }

    #[test]
    fn ctrl_enter_commits_selection_in_fill_mode() {
        let mut t = PenTool::new();
        t.mode = PenMode::Fill; // Ctrl+Enter overrides the mode
        let mut doc = Document::new(DocumentId(1), 200, 200);
        t.anchors = vec![
            PenAnchor::corner((20.0, 20.0)),
            PenAnchor::corner((160.0, 30.0)),
            PenAnchor::corner((140.0, 150.0)),
        ];
        t.commit_as_selection(&mut doc.canvas);
        assert!(doc.canvas.selection.active);
        assert!(t.is_empty());
    }

    #[test]
    fn fill_mode_paints_inside_polygon() {
        let mut t = PenTool::new();
        t.mode = PenMode::Fill;
        let mut doc = Document::new(DocumentId(1), 100, 100);
        t.anchors = vec![
            PenAnchor::corner((20.0, 20.0)),
            PenAnchor::corner((80.0, 20.0)),
            PenAnchor::corner((80.0, 80.0)),
            PenAnchor::corner((20.0, 80.0)),
        ];
        {
            let mut c = ctx(&mut doc); // foreground = black
            t.on_confirm(&mut c);
        }
        let tiles = doc.canvas.active_layer().get_paint_tiles().unwrap();
        assert_eq!(
            tiles.get_pixel(50, 50),
            (0, 0, 0, 255),
            "centre filled black"
        );
        assert_ne!(tiles.get_pixel(5, 5), (0, 0, 0, 255), "corner untouched");
        assert!(t.is_empty());
        assert!(!doc.canvas.selection.active, "fill leaves no selection");
    }

    #[test]
    fn fill_is_undoable() {
        let mut t = PenTool::new();
        t.mode = PenMode::Fill;
        let mut doc = Document::new(DocumentId(1), 100, 100);
        let before = doc
            .canvas
            .active_layer()
            .get_paint_tiles()
            .unwrap()
            .get_pixel(50, 50);
        t.anchors = vec![
            PenAnchor::corner((20.0, 20.0)),
            PenAnchor::corner((80.0, 20.0)),
            PenAnchor::corner((80.0, 80.0)),
            PenAnchor::corner((20.0, 80.0)),
        ];
        {
            let mut c = ctx(&mut doc);
            t.on_confirm(&mut c);
        }
        assert_ne!(
            doc.canvas
                .active_layer()
                .get_paint_tiles()
                .unwrap()
                .get_pixel(50, 50),
            before,
            "fill changed the pixel"
        );
        doc.canvas.undo();
        assert_eq!(
            doc.canvas
                .active_layer()
                .get_paint_tiles()
                .unwrap()
                .get_pixel(50, 50),
            before,
            "undo restores the original pixel"
        );
    }

    #[test]
    fn stroke_mode_paints_along_path() {
        let mut t = PenTool::new();
        t.mode = PenMode::Stroke;
        t.stroke_width = 4.0;
        let mut doc = Document::new(DocumentId(1), 100, 100);
        t.anchors = vec![
            PenAnchor::corner((10.0, 50.0)),
            PenAnchor::corner((90.0, 50.0)),
        ];
        {
            let mut c = ctx(&mut doc);
            t.on_confirm(&mut c);
        }
        let tiles = doc.canvas.active_layer().get_paint_tiles().unwrap();
        let on_line = tiles.get_pixel(50, 50);
        assert_eq!(on_line.3, 255, "line pixel fully painted");
        assert!(on_line.0 < 40, "line pixel takes the dark foreground");
        assert_ne!(
            tiles.get_pixel(50, 10),
            on_line,
            "pixel off the line differs"
        );
        assert!(t.is_empty());
    }
}

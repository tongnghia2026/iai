#![allow(dead_code)]
//! Perspective Crop — drag four corner handles over a skewed subject (a sign, a
//! page, a building) and commit to rectify that quadrilateral back to a
//! straight, axis-aligned rectangle. The warp is a unit-square→quad homography
//! (`Canvas::crop_perspective`); this file is just the interactive quad editor.

use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::canvas::Canvas;
use crate::core::units::{from_pixels, to_pixels, Unit};

/// Quad corner order: 0=top-left, 1=top-right, 2=bottom-right, 3=bottom-left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PerspHandle {
    None,
    Corner(usize),
    /// Edge index: 0=top, 1=right, 2=bottom, 3=left.
    Edge(usize),
    Move,
}

pub struct PerspectiveCropTool {
    /// The four corners in canvas space, order [TL, TR, BR, BL].
    pub corners: [(f32, f32); 4],
    /// True once all four corners are placed → the quad is shown and editable.
    pub initialized: bool,
    /// Draw a 3×3 thirds grid inside the quad.
    pub show_grid: bool,

    /// Corners placed so far while still picking them (0..4). Once it reaches four
    /// they transfer to `corners` and `initialized` flips true. Empty in edit mode.
    pub points: Vec<(f32, f32)>,
    /// True while the just-placed point is being dragged to fine-tune it.
    placing_drag: bool,
    /// Manual output size (px). `None` = derive it from the quad geometry. Survives
    /// re-placing so the size can be typed before OR after picking the corners.
    pub manual_size: Option<(u32, u32)>,
    /// Exact values typed by the user in `unit`. Pixel output is derived from
    /// these values so integer rounding never feeds back into fields such as cm.
    pub manual_input: Option<(f32, f32)>,
    /// Output resolution (px/inch) — for unit conversion in the options bar and
    /// written to the rectified image's metadata on commit.
    pub dpi: f32,
    /// Options-bar unit index (see `core::units::Unit::all`): 0=px,1=cm,2=mm,…
    pub unit: u8,

    // Rubber-band rectangle sweep (an alternative to clicking 4 corners): a single
    // press-drag-release whose first move travels past a small threshold becomes a
    // rectangle, exactly like the normal Crop tool.
    press_pos: (f32, f32),
    sweeping: bool,
    sweep_cur: (f32, f32),

    active: PerspHandle,
    is_dragging: bool,
    drag_start: (f32, f32),
    drag_start_corners: [(f32, f32); 4],
}

impl PerspectiveCropTool {
    pub fn new() -> Self {
        Self {
            corners: [(0.0, 0.0); 4],
            initialized: false,
            show_grid: true,
            points: Vec::new(),
            placing_drag: false,
            manual_size: None,
            manual_input: None,
            dpi: 300.0,
            unit: 0,
            press_pos: (0.0, 0.0),
            sweeping: false,
            sweep_cur: (0.0, 0.0),
            active: PerspHandle::None,
            is_dragging: false,
            drag_start: (0.0, 0.0),
            drag_start_corners: [(0.0, 0.0); 4],
        }
    }

    /// The 4 corners of the in-progress rubber-band sweep ([TL,TR,BR,BL]), or `None`
    /// when not sweeping. Used to preview the rectangle as it's dragged.
    pub fn sweep_preview(&self) -> Option<[(f32, f32); 4]> {
        if !self.sweeping {
            return None;
        }
        let (x0, y0) = self.press_pos;
        let (x1, y1) = self.sweep_cur;
        let (lx, rx) = (x0.min(x1), x0.max(x1));
        let (ty, by) = (y0.min(y1), y0.max(y1));
        Some([(lx, ty), (rx, ty), (rx, by), (lx, by)])
    }

    /// Enter corner-picking mode: clear any quad and placed points (but keep a
    /// manually-typed output size). Called when the tool is selected — unlike a
    /// regular crop, perspective crop shows NO frame until the user picks corners.
    pub fn begin_placing(&mut self) {
        self.initialized = false;
        self.points.clear();
        self.placing_drag = false;
        self.sweeping = false;
        self.active = PerspHandle::None;
        self.is_dragging = false;
    }

    /// Corners placed so far (0..4) while picking. Empty once the quad is set.
    pub fn placing_points(&self) -> &[(f32, f32)] {
        &self.points
    }

    /// True while still picking corners (no editable quad yet).
    pub fn is_placing(&self) -> bool {
        !self.initialized
    }

    /// True only when cancelling/exiting would discard an in-progress placement.
    /// Merely having Perspective Crop selected is idle and must not block app exit.
    pub fn has_pending_placement(&self) -> bool {
        !self.points.is_empty() || self.placing_drag || self.sweeping
    }

    /// Place the quad over the image, inset a little so the corner handles sit
    /// inside the canvas and are easy to grab.
    pub fn init_bounds(&mut self, cw: u32, ch: u32) {
        let w = cw as f32;
        let h = ch as f32;
        let ix = (w * 0.08).clamp(0.0, w * 0.4);
        let iy = (h * 0.08).clamp(0.0, h * 0.4);
        self.corners = [(ix, iy), (w - ix, iy), (w - ix, h - iy), (ix, h - iy)];
        self.initialized = true;
    }

    pub fn has_quad(&self) -> bool {
        self.initialized
    }

    /// Corner, edge, or whole-quad Move under the pointer.
    pub fn detect_handle(&self, x: f32, y: f32, zoom: f32) -> PerspHandle {
        if !self.initialized {
            return PerspHandle::None;
        }
        let r = (11.0 / zoom.max(0.01)).clamp(5.0, 32.0);
        let mut best = (f32::MAX, PerspHandle::None);
        for (i, c) in self.corners.iter().enumerate() {
            let d = (x - c.0).hypot(y - c.1);
            if d < r && d < best.0 {
                best = (d, PerspHandle::Corner(i));
            }
        }
        if best.1 != PerspHandle::None {
            return best.1;
        }

        // Corners win over edges. Edge hit-testing uses distance to the finite
        // segment so resize cursors also work on skewed quadrilaterals.
        let edge_radius = (7.0 / zoom.max(0.01)).clamp(3.0, 20.0);
        let edges = [(0usize, 1usize), (1, 2), (2, 3), (3, 0)];
        let mut nearest_edge = (f32::MAX, PerspHandle::None);
        for (edge_idx, &(a_idx, b_idx)) in edges.iter().enumerate() {
            let a = self.corners[a_idx];
            let b = self.corners[b_idx];
            let ab = (b.0 - a.0, b.1 - a.1);
            let len_sq = ab.0 * ab.0 + ab.1 * ab.1;
            if len_sq <= f32::EPSILON {
                continue;
            }
            let t = (((x - a.0) * ab.0 + (y - a.1) * ab.1) / len_sq).clamp(0.0, 1.0);
            let nearest = (a.0 + ab.0 * t, a.1 + ab.1 * t);
            let distance = (x - nearest.0).hypot(y - nearest.1);
            if distance <= edge_radius && distance < nearest_edge.0 {
                nearest_edge = (distance, PerspHandle::Edge(edge_idx));
            }
        }
        if nearest_edge.1 != PerspHandle::None {
            return nearest_edge.1;
        }

        if self.point_in_quad(x, y) {
            return PerspHandle::Move;
        }
        PerspHandle::None
    }

    /// Even-odd point-in-polygon for the (possibly non-convex) quad.
    fn point_in_quad(&self, x: f32, y: f32) -> bool {
        let mut inside = false;
        let mut j = 3usize;
        for i in 0..4 {
            let (xi, yi) = self.corners[i];
            let (xj, yj) = self.corners[j];
            if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                inside = !inside;
            }
            j = i;
        }
        inside
    }

    /// Output dimensions. A manually-typed size wins; otherwise it's the average of
    /// the two opposite edge lengths, so the rectified image keeps roughly the
    /// quad's real proportions.
    pub fn output_size(&self) -> (u32, u32) {
        if let Some((w, h)) = self.manual_size {
            return (w.max(1), h.max(1));
        }
        self.auto_output_size()
    }

    /// The geometry-derived size, ignoring any manual override (used to seed the
    /// options-bar fields and the "auto" hint).
    pub fn auto_output_size(&self) -> (u32, u32) {
        let d = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).hypot(a.1 - b.1);
        let top = d(self.corners[0], self.corners[1]);
        let bottom = d(self.corners[3], self.corners[2]);
        let left = d(self.corners[0], self.corners[3]);
        let right = d(self.corners[1], self.corners[2]);
        let w = ((top + bottom) * 0.5).round().max(1.0) as u32;
        let h = ((left + right) * 0.5).round().max(1.0) as u32;
        (w, h)
    }

    /// Size to show in the options bar: the manual override if typed, else the
    /// quad's auto size once placed, else `(0, 0)` (nothing picked yet).
    pub fn display_size(&self) -> (u32, u32) {
        if let Some(s) = self.manual_size {
            s
        } else if self.initialized {
            self.auto_output_size()
        } else {
            (0, 0)
        }
    }

    fn selected_unit(&self) -> Unit {
        Unit::all()
            .get(self.unit as usize)
            .copied()
            .unwrap_or(Unit::Pixels)
    }

    /// Values shown in the options bar. Manual source values win so a value
    /// like 10 cm is never reconstructed from its rounded pixel result.
    pub fn display_values(&self, canvas_w: f32, canvas_h: f32) -> (f32, f32) {
        if let Some(values) = self.manual_input {
            return values;
        }
        let (w, h) = self.display_size();
        let unit = self.selected_unit();
        (
            from_pixels(w as f32, unit, self.dpi, canvas_w),
            from_pixels(h as f32, unit, self.dpi, canvas_h),
        )
    }

    fn auto_input_values(&self, canvas_w: f32, canvas_h: f32) -> (f32, f32) {
        let (w, h) = self.auto_output_size();
        let unit = self.selected_unit();
        (
            from_pixels(w as f32, unit, self.dpi, canvas_w),
            from_pixels(h as f32, unit, self.dpi, canvas_h),
        )
    }

    pub fn sync_manual_pixels(&mut self, canvas_w: f32, canvas_h: f32) {
        let Some((w, h)) = self.manual_input else {
            return;
        };
        let unit = self.selected_unit();
        self.manual_size = Some((
            to_pixels(w, unit, self.dpi, canvas_w).round().max(1.0) as u32,
            to_pixels(h, unit, self.dpi, canvas_h).round().max(1.0) as u32,
        ));
    }

    pub fn set_manual_width(&mut self, width: f32, canvas_w: f32, canvas_h: f32) {
        let height = self
            .manual_input
            .map(|values| values.1)
            .unwrap_or_else(|| self.auto_input_values(canvas_w, canvas_h).1);
        self.manual_input = Some((width.max(0.0), height));
        self.sync_manual_pixels(canvas_w, canvas_h);
    }

    pub fn set_manual_height(&mut self, height: f32, canvas_w: f32, canvas_h: f32) {
        let width = self
            .manual_input
            .map(|values| values.0)
            .unwrap_or_else(|| self.auto_input_values(canvas_w, canvas_h).0);
        self.manual_input = Some((width, height.max(0.0)));
        self.sync_manual_pixels(canvas_w, canvas_h);
    }

    pub fn clear_manual_size(&mut self) {
        self.manual_size = None;
        self.manual_input = None;
    }

    /// Output size commit() would produce, or None if degenerate.
    pub fn prospective_output_size(&self) -> Option<(u32, u32)> {
        if !self.initialized {
            return None;
        }
        let (w, h) = self.output_size();
        if w < 2 || h < 2 {
            None
        } else {
            Some((w, h))
        }
    }

    pub fn commit(&mut self, canvas: &mut Canvas) {
        if !self.initialized {
            return;
        }
        let (out_w, out_h) = self.output_size();
        if out_w < 2 || out_h < 2 {
            self.cancel();
            return;
        }
        canvas.crop_perspective(self.corners, out_w, out_h, true);
        self.cancel();
    }

    pub fn cancel(&mut self) {
        self.initialized = false;
        self.points.clear();
        self.placing_drag = false;
        self.sweeping = false;
        self.active = PerspHandle::None;
        self.is_dragging = false;
    }
}

impl Default for PerspectiveCropTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for PerspectiveCropTool {
    fn id(&self) -> &'static str {
        "perspective_crop"
    }
    fn name(&self) -> &str {
        "Perspective Crop"
    }
    fn icon(&self) -> &'static str {
        "crop"
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::PerspectiveCrop
    }

    fn on_cancel(&mut self) {
        self.cancel();
    }
    fn on_confirm(&mut self, ctx: &mut ToolCtx) {
        self.commit(ctx.canvas_mut());
    }

    fn on_press(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        let (cx, cy) = (event.canvas_x, event.canvas_y);

        // Placement mode. No frame shows until the quad is defined — either by
        // clicking four corners or by sweeping one rectangle (decided on the first
        // gesture: a click starts corner-picking, a drag becomes a rectangle).
        if !self.initialized {
            self.press_pos = (cx, cy);
            self.sweep_cur = (cx, cy);
            self.sweeping = false;
            self.placing_drag = true;
            // Corners 2-4 drop immediately (drag to fine-tune); corner 1 waits until
            // release so a drag from empty can turn into a rectangle sweep instead.
            if !self.points.is_empty() {
                self.points.push((cx, cy));
            }
            return ToolResponse::redraw();
        }

        // Edit mode: grab a corner or the whole quad.
        self.active = self.detect_handle(cx, cy, ctx.zoom);
        self.is_dragging = self.active != PerspHandle::None;
        self.drag_start = (cx, cy);
        self.drag_start_corners = self.corners;
        ToolResponse::redraw()
    }

    fn on_drag(
        &mut self,
        event: PointerEvent,
        _prev: &PointerEvent,
        ctx: &mut ToolCtx,
    ) -> ToolResponse {
        let (cx, cy) = (event.canvas_x, event.canvas_y);

        if !self.initialized {
            self.sweep_cur = (cx, cy);
            if self.placing_drag {
                if self.points.is_empty() {
                    // First gesture: a move past ~8 screen px becomes a rectangle.
                    let thresh = 8.0 / ctx.zoom.max(0.01);
                    let d = (cx - self.press_pos.0).hypot(cy - self.press_pos.1);
                    if d > thresh {
                        self.sweeping = true;
                    }
                } else if let Some(last) = self.points.last_mut() {
                    *last = (cx, cy);
                }
            }
            return ToolResponse::redraw();
        }

        match self.active {
            PerspHandle::Corner(i) => {
                self.corners[i] = (cx, cy);
            }
            PerspHandle::Edge(edge_idx) => {
                let edge_corners = [(0usize, 1usize), (1, 2), (2, 3), (3, 0)];
                let (a_idx, b_idx) = edge_corners[edge_idx];
                let a = self.drag_start_corners[a_idx];
                let b = self.drag_start_corners[b_idx];
                let tangent = (b.0 - a.0, b.1 - a.1);
                let length = tangent.0.hypot(tangent.1);
                if length > f32::EPSILON {
                    let normal = (-tangent.1 / length, tangent.0 / length);
                    let pointer_delta = (cx - self.drag_start.0, cy - self.drag_start.1);
                    let offset = pointer_delta.0 * normal.0 + pointer_delta.1 * normal.1;
                    let delta = (normal.0 * offset, normal.1 * offset);
                    self.corners[a_idx] = (a.0 + delta.0, a.1 + delta.1);
                    self.corners[b_idx] = (b.0 + delta.0, b.1 + delta.1);
                }
            }
            PerspHandle::Move => {
                let dx = cx - self.drag_start.0;
                let dy = cy - self.drag_start.1;
                for i in 0..4 {
                    self.corners[i] = (
                        self.drag_start_corners[i].0 + dx,
                        self.drag_start_corners[i].1 + dy,
                    );
                }
            }
            PerspHandle::None => {}
        }
        ToolResponse::redraw()
    }

    fn on_release(&mut self, _event: PointerEvent, _ctx: &mut ToolCtx) -> ToolResponse {
        if !self.initialized {
            self.placing_drag = false;

            // Rectangle sweep → build the quad from its bounds.
            if self.sweeping {
                if let Some(q) = self.sweep_preview() {
                    let w = (q[1].0 - q[0].0).abs();
                    let h = (q[2].1 - q[1].1).abs();
                    if w >= 2.0 && h >= 2.0 {
                        self.corners = q;
                        self.initialized = true;
                        self.points.clear();
                    }
                }
                self.sweeping = false;
                return ToolResponse::redraw();
            }

            // A click placed corner 1 (which we deferred to here so a drag could sweep).
            if self.points.is_empty() {
                self.points.push(self.press_pos);
            }
            // Fourth corner placed → freeze the quad and switch to edit mode.
            if self.points.len() == 4 {
                self.corners = [
                    self.points[0],
                    self.points[1],
                    self.points[2],
                    self.points[3],
                ];
                self.initialized = true;
                self.points.clear();
            }
            return ToolResponse::redraw();
        }
        self.is_dragging = false;
        self.active = PerspHandle::None;
        ToolResponse::redraw()
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
    fn init_places_inset_quad() {
        let mut t = PerspectiveCropTool::new();
        t.init_bounds(1000, 500);
        assert!(t.initialized);
        // corners inset from the edges, in TL,TR,BR,BL order
        assert!(t.corners[0].0 > 0.0 && t.corners[0].1 > 0.0);
        assert!(t.corners[2].0 < 1000.0 && t.corners[2].1 < 500.0);
        assert!((t.corners[1].0 - t.corners[2].0).abs() < 0.01); // TR.x == BR.x
    }

    #[test]
    fn detect_corner_and_move() {
        let mut t = PerspectiveCropTool::new();
        t.init_bounds(100, 100); // corners at (8,8)(92,8)(92,92)(8,92)
        assert_eq!(t.detect_handle(8.0, 8.0, 1.0), PerspHandle::Corner(0));
        assert_eq!(t.detect_handle(92.0, 92.0, 1.0), PerspHandle::Corner(2));
        assert_eq!(t.detect_handle(50.0, 8.0, 1.0), PerspHandle::Edge(0));
        assert_eq!(t.detect_handle(92.0, 50.0, 1.0), PerspHandle::Edge(1));
        assert_eq!(t.detect_handle(50.0, 92.0, 1.0), PerspHandle::Edge(2));
        assert_eq!(t.detect_handle(8.0, 50.0, 1.0), PerspHandle::Edge(3));
        assert_eq!(t.detect_handle(50.0, 50.0, 1.0), PerspHandle::Move);
        // Outside the quad and beyond the top edge's resize hit radius.
        assert_eq!(t.detect_handle(50.0, -5.0, 1.0), PerspHandle::None);
    }

    #[test]
    fn dragging_a_corner_moves_only_it() {
        let mut t = PerspectiveCropTool::new();
        t.init_bounds(100, 100); // edit mode with corners at (8,8)(92,8)(92,92)(8,92)
        let mut doc = Document::new(DocumentId(1), 100, 100);
        let press = PointerEvent::new(8.0, 8.0);
        {
            let mut c = ctx(&mut doc);
            t.on_press(press, &mut c);
            t.on_drag(PointerEvent::new(20.0, 25.0), &press, &mut c);
        }
        assert_eq!(t.active, PerspHandle::Corner(0)); // captured at press (released later)
        assert!((t.corners[0].0 - 20.0).abs() < 0.01);
        assert!((t.corners[0].1 - 25.0).abs() < 0.01);
        // other corners untouched
        assert!((t.corners[2].0 - 92.0).abs() < 0.01);
    }

    #[test]
    fn move_translates_whole_quad() {
        let mut t = PerspectiveCropTool::new();
        t.init_bounds(100, 100);
        let mut doc = Document::new(DocumentId(1), 100, 100);
        let press = PointerEvent::new(50.0, 50.0);
        {
            let mut c = ctx(&mut doc);
            t.on_press(press, &mut c);
            t.on_drag(PointerEvent::new(60.0, 70.0), &press, &mut c);
        }
        assert!((t.corners[0].0 - 18.0).abs() < 0.01); // 8 + 10
        assert!((t.corners[0].1 - 28.0).abs() < 0.01); // 8 + 20
    }

    #[test]
    fn dragging_edge_moves_both_corners_along_its_normal() {
        let mut t = PerspectiveCropTool::new();
        t.init_bounds(100, 100);
        let mut doc = Document::new(DocumentId(1), 100, 100);
        let press = PointerEvent::new(50.0, 8.0);
        {
            let mut c = ctx(&mut doc);
            t.on_press(press, &mut c);
            t.on_drag(PointerEvent::new(65.0, 18.0), &press, &mut c);
        }

        assert_eq!(t.active, PerspHandle::Edge(0));
        assert!((t.corners[0].0 - 8.0).abs() < 0.01);
        assert!((t.corners[1].0 - 92.0).abs() < 0.01);
        assert!((t.corners[0].1 - 18.0).abs() < 0.01);
        assert!((t.corners[1].1 - 18.0).abs() < 0.01);
        assert!((t.corners[2].1 - 92.0).abs() < 0.01);
        assert!((t.corners[3].1 - 92.0).abs() < 0.01);
    }

    #[test]
    fn four_clicks_place_the_quad() {
        let mut t = PerspectiveCropTool::new();
        let mut doc = Document::new(DocumentId(1), 200, 200);
        let pts = [(10.0, 10.0), (180.0, 20.0), (190.0, 170.0), (20.0, 160.0)];
        for &(x, y) in pts.iter() {
            let mut c = ctx(&mut doc);
            let ev = PointerEvent::new(x, y);
            t.on_press(ev, &mut c);
            assert!(t.is_placing()); // no quad until the 4th release
            t.on_release(ev, &mut c);
        }
        assert!(t.initialized);
        assert!(!t.is_placing());
        assert!(t.placing_points().is_empty());
        assert!((t.corners[1].0 - 180.0).abs() < 0.01);
    }

    #[test]
    fn cancel_clears_pending_placement() {
        let mut t = PerspectiveCropTool::new();
        let mut doc = Document::new(DocumentId(1), 200, 200);
        let click = PointerEvent::new(10.0, 10.0);
        {
            let mut c = ctx(&mut doc);
            t.on_press(click, &mut c);
            t.on_release(click, &mut c);
        }
        assert!(t.has_pending_placement());

        t.cancel();

        assert!(!t.has_pending_placement());
        assert!(!t.has_quad());
    }

    #[test]
    fn drag_sweeps_a_rectangle() {
        let mut t = PerspectiveCropTool::new();
        let mut doc = Document::new(DocumentId(1), 200, 200);
        let p0 = PointerEvent::new(10.0, 10.0);
        let end = PointerEvent::new(110.0, 80.0);
        {
            let mut c = ctx(&mut doc); // zoom = 1.0 → 8px threshold
            t.on_press(p0, &mut c);
            t.on_drag(end, &p0, &mut c);
            assert!(
                t.sweep_preview().is_some(),
                "a long first drag should sweep"
            );
            t.on_release(end, &mut c);
        }
        assert!(t.initialized);
        // Rect corners in TL, TR, BR, BL order.
        assert!((t.corners[0].0 - 10.0).abs() < 0.01);
        assert!((t.corners[0].1 - 10.0).abs() < 0.01);
        assert!((t.corners[2].0 - 110.0).abs() < 0.01);
        assert!((t.corners[2].1 - 80.0).abs() < 0.01);
    }

    #[test]
    fn manual_size_overrides_geometry() {
        let mut t = PerspectiveCropTool::new();
        t.corners = [(0.0, 0.0), (200.0, 0.0), (200.0, 100.0), (0.0, 100.0)];
        t.initialized = true;
        assert_eq!(t.output_size(), (200, 100));
        t.manual_size = Some((640, 480));
        t.manual_input = Some((640.0, 480.0));
        assert_eq!(t.output_size(), (640, 480));
        assert_eq!(t.auto_output_size(), (200, 100));
    }

    #[test]
    fn physical_manual_size_stays_exact_when_dpi_changes() {
        let mut t = PerspectiveCropTool::new();
        t.unit = Unit::Centimeters as u8;
        t.dpi = 72.0;
        t.manual_input = Some((10.0, 15.0));
        t.sync_manual_pixels(1000.0, 1000.0);
        assert_eq!(t.manual_size, Some((283, 425)));
        assert_eq!(t.display_values(1000.0, 1000.0), (10.0, 15.0));

        t.dpi = 300.0;
        t.sync_manual_pixels(1000.0, 1000.0);
        assert_eq!(t.manual_size, Some((1181, 1772)));
        assert_eq!(t.display_values(1000.0, 1000.0), (10.0, 15.0));
    }

    #[test]
    fn output_size_matches_quad_extent() {
        let mut t = PerspectiveCropTool::new();
        t.corners = [(0.0, 0.0), (200.0, 0.0), (200.0, 100.0), (0.0, 100.0)];
        t.initialized = true;
        assert_eq!(t.output_size(), (200, 100));
    }

    #[test]
    fn commit_rectifies_and_resizes_canvas() {
        let mut doc = Document::new(DocumentId(1), 300, 200);
        let mut t = PerspectiveCropTool::new();
        // a skewed quad
        t.corners = [(20.0, 30.0), (260.0, 10.0), (280.0, 180.0), (10.0, 190.0)];
        t.initialized = true;
        let (ew, eh) = t.output_size();
        {
            let mut c = ctx(&mut doc);
            t.on_confirm(&mut c);
        }
        assert_eq!((doc.canvas.width, doc.canvas.height), (ew, eh));
        assert!(!t.initialized); // committed → cleared
    }
}

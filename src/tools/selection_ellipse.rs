// Elliptical marquee selection tool. Same pattern as SelectionRect.

use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::selection::SelectionSnapshot;

pub struct SelectionEllipseTool {
    pub feather: f32,
    pub anti_alias: bool,
    start_x: f32,
    start_y: f32,
    end_x: f32,
    end_y: f32,
    dragging: bool,
    base_snap: Option<SelectionSnapshot>,
    /// True when this drag began with Shift held while a selection already
    /// existed → PTS "add to selection" (free shape, not constrained circle).
    freeform_add: bool,

    is_moving_selection: bool,
    frac_x: f32,
    frac_y: f32,
    total_dx: i32,
    total_dy: i32,
}

impl SelectionEllipseTool {
    pub fn new() -> Self {
        Self {
            feather: 0.0,
            anti_alias: true,
            start_x: 0.0,
            start_y: 0.0,
            end_x: 0.0,
            end_y: 0.0,
            dragging: false,
            base_snap: None,
            freeform_add: false,
            is_moving_selection: false,
            frac_x: 0.0,
            frac_y: 0.0,
            total_dx: 0,
            total_dy: 0,
        }
    }

    /// When Shift is held, lock the drag to a 1:1 box anchored at the start
    /// point so the marquee draws a perfect circle.
    fn constrain_end(&self, ex: f32, ey: f32) -> (f32, f32) {
        let dx = ex - self.start_x;
        let dy = ey - self.start_y;
        let size = dx.abs().max(dy.abs());
        let nx = self.start_x + if dx < 0.0 { -size } else { size };
        let ny = self.start_y + if dy < 0.0 { -size } else { size };
        (nx, ny)
    }

    /// Canvas-space preview ellipse bounds during drag: (x0, y0, x1, y1).
    ///
    /// Snapped to the same integer pixel grid `apply()` commits to, so the live
    /// ellipse matches the final marching-ants edge instead of drifting ~1px.
    pub fn preview_ellipse(&self) -> Option<[f32; 4]> {
        if self.dragging {
            let x0 = self.start_x.min(self.end_x).max(0.0).floor();
            let y0 = self.start_y.min(self.end_y).max(0.0).floor();
            let x1 = self.start_x.max(self.end_x).max(0.0).floor();
            let y1 = self.start_y.max(self.end_y).max(0.0).floor();
            Some([x0, y0, x1, y1])
        } else {
            None
        }
    }

    fn apply(
        &self,
        canvas: &mut crate::core::canvas::Canvas,
        ex: f32,
        ey: f32,
        mode: crate::core::selection::SelectionMode,
    ) {
        let x0 = self.start_x.min(ex).max(0.0) as u32;
        let y0 = self.start_y.min(ey).max(0.0) as u32;
        let x1 = (self.start_x.max(ex) as u32).min(canvas.width);
        let y1 = (self.start_y.max(ey) as u32).min(canvas.height);

        if let Some(snap) = &self.base_snap {
            canvas.restore_selection_snapshot(snap);
        }
        canvas.select_ellipse_mode(x0, y0, x1, y1, mode, self.feather, self.anti_alias);
    }
}

impl Tool for SelectionEllipseTool {
    fn id(&self) -> &'static str {
        "selection_ellipse"
    }
    fn name(&self) -> &str {
        "Ellipse Selection"
    }
    fn shortcut(&self) -> Option<char> {
        Some('M')
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::SelectionEllipse
    }

    fn on_press(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        let (cx, cy) = (event.canvas_x, event.canvas_y);
        let canvas = ctx.canvas_mut();

        if !event.shift
            && !event.alt
            && canvas.selection.active
            && canvas.selection.is_selected(cx as u32, cy as u32)
        {
            self.is_moving_selection = true;
            self.frac_x = 0.0;
            self.frac_y = 0.0;
            self.total_dx = 0;
            self.total_dy = 0;
            return ToolResponse::none();
        }

        self.freeform_add = event.shift && canvas.selection.active;

        self.start_x = cx;
        self.start_y = cy;
        self.end_x = cx;
        self.end_y = cy;
        self.dragging = true;
        self.is_moving_selection = false;
        self.base_snap = Some(canvas.snapshot_selection());
        ToolResponse::none()
    }

    fn on_drag(
        &mut self,
        event: PointerEvent,
        prev: &PointerEvent,
        ctx: &mut ToolCtx,
    ) -> ToolResponse {
        if self.is_moving_selection {
            let cx = event.canvas_x;
            let cy = event.canvas_y;
            let px = prev.canvas_x;
            let py = prev.canvas_y;

            self.frac_x += cx - px;
            self.frac_y += cy - py;

            let dx = self.frac_x.trunc() as i32;
            let dy = self.frac_y.trunc() as i32;

            if dx != 0 || dy != 0 {
                self.frac_x -= dx as f32;
                self.frac_y -= dy as f32;
                self.total_dx += dx;
                self.total_dy += dy;

                let canvas = ctx.canvas_mut();
                canvas.selection.offset.0 += dx;
                canvas.selection.offset.1 += dy;
                canvas.selection.mark_bbox_dirty();
                return ToolResponse::repaint();
            }
            return ToolResponse::none();
        }

        if !self.dragging {
            return ToolResponse::none();
        }

        if event.space {
            let ddx = event.canvas_x - prev.canvas_x;
            let ddy = event.canvas_y - prev.canvas_y;
            self.start_x += ddx;
            self.start_y += ddy;
            self.end_x += ddx;
            self.end_y += ddy;
            return ToolResponse::redraw();
        }

        if event.shift && !self.freeform_add {
            let (ex, ey) = self.constrain_end(event.canvas_x, event.canvas_y);
            self.end_x = ex;
            self.end_y = ey;
        } else {
            self.end_x = event.canvas_x;
            self.end_y = event.canvas_y;
        }
        ToolResponse::redraw()
    }

    fn on_release(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        if self.is_moving_selection {
            self.is_moving_selection = false;

            if self.total_dx != 0 || self.total_dy != 0 {
                let canvas = ctx.canvas_mut();
                let cmd = crate::core::command::TranslateSelectionCommand::from_applied_move(
                    &canvas.selection,
                    self.total_dx,
                    self.total_dy,
                );
                canvas.record(Box::new(cmd));
            }
            return ToolResponse::none();
        }

        self.apply(
            ctx.canvas_mut(),
            self.end_x,
            self.end_y,
            event.selection_mode,
        );
        self.dragging = false;
        self.freeform_add = false;
        self.base_snap = None;
        ToolResponse::repaint()
    }

    fn on_cancel(&mut self) {
        self.dragging = false;
        self.is_moving_selection = false;
        self.freeform_add = false;
        self.base_snap = None;
    }
}

// Polygon lasso: click to add points; click near the start (<12px), double-click,
// or Enter closes; Esc cancels. Like the freehand lasso, the live selection is left
// untouched while placing points — the polygon is applied once, on close.

use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::selection::{SelectionMode, SelectionSnapshot};

const CLOSE_THRESHOLD_PX: f32 = 12.0;

pub struct PolygonLassoTool {
    pub feather: f32,
    pub anti_alias: bool,
    points: Vec<(f32, f32)>,
    /// Locked in at the first point so every close path uses the same mode.
    mode: SelectionMode,
    base_snap: Option<SelectionSnapshot>,

    is_moving_selection: bool,
    frac_x: f32,
    frac_y: f32,
    total_dx: i32,
    total_dy: i32,
}

impl PolygonLassoTool {
    pub fn new() -> Self {
        Self {
            feather: 0.0,
            anti_alias: true,
            points: Vec::new(),
            mode: SelectionMode::New,
            base_snap: None,
            is_moving_selection: false,
            frac_x: 0.0,
            frac_y: 0.0,
            total_dx: 0,
            total_dy: 0,
        }
    }

    /// Placed vertices; the app appends the cursor for the rubber-band overlay.
    pub fn preview_points(&self) -> &[(f32, f32)] {
        &self.points
    }

    fn close_and_commit(&mut self, canvas: &mut crate::core::canvas::Canvas) {
        if self.points.len() >= 3 {
            if let Some(snap) = &self.base_snap {
                canvas.restore_selection_snapshot(snap);
            }
            canvas.select_polygon_mode(&self.points, self.mode, self.feather, self.anti_alias);
        }
        self.points.clear();
        self.base_snap = None;
    }
}

impl Tool for PolygonLassoTool {
    fn id(&self) -> &'static str {
        "polygon_lasso"
    }
    fn name(&self) -> &str {
        "Polygon Lasso"
    }
    fn shortcut(&self) -> Option<char> {
        Some('L')
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::PolygonLasso
    }

    fn on_press(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        let (cx, cy) = (event.canvas_x, event.canvas_y);
        let canvas = ctx.canvas_mut();

        if self.points.is_empty()
            && event.selection_mode == SelectionMode::New
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

        if self.points.is_empty() {
            self.mode = event.selection_mode;
            self.base_snap = Some(canvas.snapshot_selection());
            self.points.push((cx, cy));
            return ToolResponse::repaint();
        }

        let (fx, fy) = self.points[0];
        let screen_dist = {
            let sdx = (cx - fx) * ctx.zoom;
            let sdy = (cy - fy) * ctx.zoom;
            (sdx * sdx + sdy * sdy).sqrt()
        };
        if screen_dist < CLOSE_THRESHOLD_PX && self.points.len() >= 3 {
            self.close_and_commit(ctx.canvas_mut());
        } else {
            self.points.push((cx, cy));
        }
        ToolResponse::repaint()
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
        ToolResponse::none()
    }

    fn on_release(&mut self, _event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
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
        ToolResponse::none()
    }

    fn on_cancel(&mut self) {
        self.points.clear();
        self.is_moving_selection = false;
        self.base_snap = None;
    }

    fn on_confirm(&mut self, ctx: &mut ToolCtx) {
        if self.points.len() >= 3 {
            self.close_and_commit(ctx.canvas_mut());
        } else {
            self.points.clear();
            self.base_snap = None;
        }
    }
}

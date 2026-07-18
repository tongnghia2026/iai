// Freehand lasso selection — drag to trace a shape; release to close and fill.

use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::selection::SelectionSnapshot;

pub struct LassoTool {
    pub feather: f32,
    pub anti_alias: bool,
    points: Vec<(f32, f32)>,
    dragging: bool,
    base_snap: Option<SelectionSnapshot>,

    is_moving_selection: bool,
    frac_x: f32,
    frac_y: f32,
    total_dx: i32,
    total_dy: i32,
}

impl LassoTool {
    pub fn new() -> Self {
        Self {
            feather: 0.0,
            anti_alias: true,
            points: Vec::new(),
            dragging: false,
            base_snap: None,
            is_moving_selection: false,
            frac_x: 0.0,
            frac_y: 0.0,
            total_dx: 0,
            total_dy: 0,
        }
    }

    pub fn preview_points(&self) -> &[(f32, f32)] {
        if self.dragging {
            &self.points
        } else {
            &[]
        }
    }
}

impl Tool for LassoTool {
    fn id(&self) -> &'static str {
        "lasso"
    }
    fn name(&self) -> &str {
        "Lasso"
    }
    fn shortcut(&self) -> Option<char> {
        Some('L')
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::Lasso
    }

    fn on_press(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        let (cx, cy) = (event.canvas_x, event.canvas_y);
        let canvas = ctx.canvas_mut();

        if event.selection_mode == crate::core::selection::SelectionMode::New
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

        self.points.clear();
        self.points.push((cx, cy));
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
        let (cx, cy) = (event.canvas_x, event.canvas_y);
        if let Some(&(last_x, last_y)) = self.points.last() {
            let dist = ((cx - last_x).powi(2) + (cy - last_y).powi(2)).sqrt();
            if dist >= 2.0 {
                self.points.push((cx, cy));
            }
        } else {
            self.points.push((cx, cy));
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
            return ToolResponse::repaint();
        }

        let (cx, cy) = (event.canvas_x, event.canvas_y);
        self.points.push((cx, cy));

        if self.points.len() >= 3 {
            let canvas = ctx.canvas_mut();
            if let Some(snap) = &self.base_snap {
                canvas.restore_selection_snapshot(snap);
            }
            canvas.select_polygon_mode(
                &self.points,
                event.selection_mode,
                self.feather,
                self.anti_alias,
            );
        }

        self.points.clear();
        self.dragging = false;
        self.base_snap = None;
        ToolResponse::repaint()
    }

    fn on_cancel(&mut self) {
        self.points.clear();
        self.dragging = false;
        self.is_moving_selection = false;
        self.base_snap = None;
    }
}

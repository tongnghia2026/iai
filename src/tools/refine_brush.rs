// Refine Brush — intelligent selection edge refinement for hair/fur/feathers.
//
// Used exclusively inside the "Refine Selection" workspace (show_refine_panel = true).
// Three modes:
//   Smart    — color-based alpha matting: pixels similar to FG → opaque, BG → transparent
//   Add      — force-include pixels in selection (soft-edge brush painting selection)
//   Subtract — force-exclude pixels (erase selection with soft edge)

use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::command::SelectionCommand;
use crate::core::selection::RefineBrushMode;

pub struct RefineBrushTool {
    pub size: f32,
    pub hardness: f32,
    pub mode: RefineBrushMode,

    dragging: bool,
    last_x: f32,
    last_y: f32,
    snapshot_before: Vec<u8>,
    undo_cmd: Option<SelectionCommand>,
}

impl RefineBrushTool {
    pub fn new() -> Self {
        Self {
            size: 40.0,
            hardness: 0.5,
            mode: RefineBrushMode::Smart,
            dragging: false,
            last_x: 0.0,
            last_y: 0.0,
            snapshot_before: Vec::new(),
            undo_cmd: None,
        }
    }

    fn stamp(&self, canvas: &mut crate::core::canvas::Canvas, cx: f32, cy: f32) {
        canvas.refine_edge_stamp(cx, cy, self.size, self.hardness, self.mode, true);
    }

    fn paint_segment(
        &self,
        canvas: &mut crate::core::canvas::Canvas,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
    ) {
        let dist = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
        let spacing = (self.size * 0.35).max(1.0);
        let steps = ((dist / spacing).ceil() as u32).clamp(1, 1000);
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            self.stamp(canvas, x0 + (x1 - x0) * t, y0 + (y1 - y0) * t);
        }
    }
}

impl Tool for RefineBrushTool {
    fn id(&self) -> &'static str {
        "refine_brush"
    }
    fn name(&self) -> &str {
        "Refine Brush"
    }
    fn shortcut(&self) -> Option<char> {
        None
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::RefineBrush
    }
    fn cursor_size(&self) -> f32 {
        self.size
    }

    fn on_press(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        let canvas = ctx.canvas_mut();
        if !canvas.selection.active {
            return ToolResponse::none();
        }

        self.snapshot_before = canvas.selection.mask.clone();
        self.undo_cmd = Some(SelectionCommand::capture_before(
            "Refine",
            &canvas.selection,
        ));

        self.dragging = true;
        self.last_x = event.canvas_x;
        self.last_y = event.canvas_y;
        self.stamp(canvas, event.canvas_x, event.canvas_y);

        ToolResponse::repaint()
    }

    fn on_drag(
        &mut self,
        event: PointerEvent,
        prev: &PointerEvent,
        ctx: &mut ToolCtx,
    ) -> ToolResponse {
        if !self.dragging {
            return ToolResponse::none();
        }

        let canvas = ctx.canvas_mut();
        if !canvas.selection.active {
            return ToolResponse::none();
        }

        self.paint_segment(
            canvas,
            prev.canvas_x,
            prev.canvas_y,
            event.canvas_x,
            event.canvas_y,
        );
        self.last_x = event.canvas_x;
        self.last_y = event.canvas_y;
        ToolResponse::repaint()
    }

    fn on_release(&mut self, _event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        if !self.dragging {
            return ToolResponse::none();
        }
        self.dragging = false;

        let canvas = ctx.canvas_mut();
        if let Some(mut cmd) = self.undo_cmd.take() {
            if canvas.selection.mask != self.snapshot_before {
                cmd.capture_after(&canvas.selection);
                canvas.record(Box::new(cmd));
            }
        }
        self.snapshot_before.clear();
        ToolResponse::repaint()
    }

    fn on_cancel(&mut self) {
        self.dragging = false;
        self.undo_cmd = None;
        self.snapshot_before.clear();
    }
}

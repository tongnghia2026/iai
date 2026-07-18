#![allow(dead_code)]
use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::command::SelectionCommand;
use crate::core::selection::SelectionMode;

pub struct SmartSelectTool {
    pub brush_size: f32,
    pub tolerance: u8,
    pub edge_sensitivity: u8,
    pub sample_merged: bool,
    pub anti_alias: bool,
    pub feather: f32,
    pub contiguous: bool,

    dragging: bool,
    drag_mode: SelectionMode,
    last_x: f32,
    last_y: f32,
    undo_cmd: Option<SelectionCommand>,
    snapshot_before: Vec<u8>,
}

impl SmartSelectTool {
    pub fn new() -> Self {
        Self {
            brush_size: 30.0,
            tolerance: 32,
            edge_sensitivity: 65,
            sample_merged: true,
            anti_alias: true,
            feather: 0.0,
            contiguous: true,
            dragging: false,
            drag_mode: SelectionMode::New,
            last_x: 0.0,
            last_y: 0.0,
            undo_cmd: None,
            snapshot_before: Vec::new(),
        }
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
        let spacing = (self.brush_size * 0.40).max(1.0);
        let steps = ((dist / spacing).ceil() as u32).clamp(1, 2000);
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            canvas.smart_select_brush_stamp(
                x0 + (x1 - x0) * t,
                y0 + (y1 - y0) * t,
                self.brush_size,
                self.tolerance,
                self.edge_sensitivity,
                self.drag_mode,
                self.sample_merged,
            );
        }
    }
}

impl Tool for SmartSelectTool {
    fn id(&self) -> &'static str {
        "smart_select"
    }
    fn name(&self) -> &str {
        "Smart Select"
    }
    fn shortcut(&self) -> Option<char> {
        Some('W')
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::SmartSelect
    }
    fn cursor_size(&self) -> f32 {
        self.brush_size
    }

    fn on_press(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        let canvas = ctx.canvas_mut();

        self.drag_mode = match (event.shift, event.alt) {
            (true, true) => SelectionMode::Intersect,
            (true, false) => SelectionMode::Add,
            (false, true) => SelectionMode::Subtract,
            _ => event.selection_mode,
        };

        self.snapshot_before = canvas.selection.mask.clone();
        self.undo_cmd = Some(SelectionCommand::capture_before(
            "Smart Select",
            &canvas.selection,
        ));

        if self.drag_mode == SelectionMode::New {
            canvas.selection.deselect();
            self.drag_mode = SelectionMode::Add;
        }

        canvas.smart_select_brush_stamp(
            event.canvas_x,
            event.canvas_y,
            self.brush_size,
            self.tolerance,
            self.edge_sensitivity,
            self.drag_mode,
            self.sample_merged,
        );

        self.last_x = event.canvas_x;
        self.last_y = event.canvas_y;
        self.dragging = true;
        ToolResponse::repaint()
    }

    fn on_drag(
        &mut self,
        event: PointerEvent,
        _prev: &PointerEvent,
        ctx: &mut ToolCtx,
    ) -> ToolResponse {
        if !self.dragging {
            return ToolResponse::none();
        }
        let canvas = ctx.canvas_mut();
        self.paint_segment(
            canvas,
            self.last_x,
            self.last_y,
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

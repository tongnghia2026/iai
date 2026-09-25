#![allow(dead_code)]
use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::command::SelectionCommand;
use crate::core::quick_select::{QuickSelectOp, QuickSelectStroke};
use crate::core::selection::SelectionMode;

/// Smart Select (W) — Photoshop's Quick Selection: paint over an object and the
/// selection grows out to its edges. See [`crate::core::quick_select`].
pub struct SmartSelectTool {
    pub brush_size: f32,
    pub sample_merged: bool,
    pub anti_alias: bool,
    pub feather: f32,
    pub contiguous: bool,

    dragging: bool,
    drag_mode: SelectionMode,
    stroke: Option<QuickSelectStroke>,
    /// Cursor positions not yet applied; cut once per frame.
    pending: Vec<(f32, f32)>,
    /// Intersect: the selection before the stroke, and the scratch mask the
    /// stroke paints.
    intersect: Option<(Vec<u8>, Vec<u8>)>,
    undo_cmd: Option<SelectionCommand>,
    snapshot_before: Vec<u8>,
}

impl SmartSelectTool {
    pub fn new() -> Self {
        Self {
            brush_size: 30.0,
            sample_merged: true,
            anti_alias: true,
            feather: 0.0,
            contiguous: true,
            dragging: false,
            drag_mode: SelectionMode::New,
            stroke: None,
            pending: Vec::new(),
            intersect: None,
            undo_cmd: None,
            snapshot_before: Vec::new(),
        }
    }

    /// Apply the pending cursor path to the selection.
    fn flush(&mut self, canvas: &mut crate::core::canvas::Canvas) {
        if self.pending.is_empty() {
            return;
        }
        let Some(stroke) = self.stroke.as_mut() else {
            self.pending.clear();
            return;
        };
        let points = std::mem::take(&mut self.pending);
        if let Some((base, scratch)) = self.intersect.as_mut() {
            if let Some((x0, y0, x1, y1)) =
                canvas.quick_select_extend_into(stroke, scratch, &points)
            {
                let w = canvas.width as usize;
                let sel = &mut canvas.selection;
                let mut any = false;
                for y in y0..y1 {
                    for x in x0..x1 {
                        let i = y * w + x;
                        if scratch[i] >= 128 {
                            sel.mask[i] = base[i];
                            any |= base[i] > 0;
                        }
                    }
                }
                sel.active |= any;
                sel.mask_revision += 1;
                sel.mark_bbox_dirty();
            }
        } else {
            canvas.quick_select_extend(stroke, &points);
        }
        self.pending = points;
        self.pending.clear();
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

        let at = (event.canvas_x, event.canvas_y);
        let op = if self.drag_mode == SelectionMode::Subtract {
            QuickSelectOp::Subtract
        } else {
            QuickSelectOp::Add
        };
        self.intersect = None;
        if self.drag_mode == SelectionMode::Intersect {
            // The result is the old selection limited to what this stroke
            // picks, so it starts empty and fills in as the stroke grows.
            let n = canvas.selection.mask.len();
            let base = std::mem::replace(&mut canvas.selection.mask, vec![0; n]);
            canvas.selection.active = false;
            canvas.selection.mask_revision += 1;
            canvas.selection.mark_bbox_dirty();
            let scratch = vec![0u8; n];
            self.stroke = canvas.quick_select_begin(
                op,
                self.brush_size,
                at,
                self.sample_merged,
                Some(&scratch),
            );
            self.intersect = Some((base, scratch));
        } else {
            self.stroke =
                canvas.quick_select_begin(op, self.brush_size, at, self.sample_merged, None);
        }

        self.pending.clear();
        self.pending.push(at);
        self.flush(canvas);
        self.dragging = true;
        ToolResponse::repaint()
    }

    fn on_drag(
        &mut self,
        event: PointerEvent,
        _prev: &PointerEvent,
        _ctx: &mut ToolCtx,
    ) -> ToolResponse {
        if !self.dragging {
            return ToolResponse::none();
        }
        self.pending.push((event.canvas_x, event.canvas_y));
        ToolResponse::none()
    }

    fn on_frame(&mut self, ctx: &mut ToolCtx) -> ToolResponse {
        if !self.dragging || self.pending.is_empty() {
            return ToolResponse::none();
        }
        self.flush(ctx.canvas_mut());
        ToolResponse::repaint()
    }

    fn on_release(&mut self, _event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        if !self.dragging {
            return ToolResponse::none();
        }
        let canvas = ctx.canvas_mut();
        self.flush(canvas);
        self.dragging = false;
        self.stroke = None;
        self.intersect = None;

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
        self.stroke = None;
        self.intersect = None;
        self.pending.clear();
        self.undo_cmd = None;
        self.snapshot_before.clear();
    }
}

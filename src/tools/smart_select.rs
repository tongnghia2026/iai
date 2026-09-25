#![allow(dead_code)]
use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::command::SelectionCommand;
use crate::core::quick_select::{QuickSelectOp, QuickSelectStroke};
use crate::core::selection::SelectionMode;

/// Smart Select (W) — Photoshop's Quick Selection: paint over an object and the
/// selection grows out to its edges. See [`crate::core::quick_select`].
pub struct SmartSelectTool {
    pub brush_size: f32,
    /// The tool's own New / Add / Subtract / Intersect choice, like each of
    /// Photoshop's selection tools; the marquees keep theirs.
    pub mode: SelectionMode,
    /// Smooth the stroke's rim onto image edges when the stroke ends.
    pub auto_enhance: bool,
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
    /// Pixels the current stroke changed, for Auto-Enhance.
    stroke_bounds: Option<(usize, usize, usize, usize)>,
    /// The stroke replaced the selection (New); afterwards the mode moves on
    /// to Add, as in Photoshop.
    started_new: bool,
}

impl SmartSelectTool {
    pub fn new() -> Self {
        Self {
            brush_size: 30.0,
            mode: SelectionMode::New,
            auto_enhance: true,
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
            stroke_bounds: None,
            started_new: false,
        }
    }

    fn grow_bounds(&mut self, b: (usize, usize, usize, usize)) {
        self.stroke_bounds = Some(match self.stroke_bounds {
            None => b,
            Some((x0, y0, x1, y1)) => (x0.min(b.0), y0.min(b.1), x1.max(b.2), y1.max(b.3)),
        });
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
        let mut changed = None;
        if let Some((base, scratch)) = self.intersect.as_mut() {
            if let Some((x0, y0, x1, y1)) =
                canvas.quick_select_extend_into(stroke, scratch, &points)
            {
                changed = Some((x0, y0, x1, y1));
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
            changed = canvas.quick_select_extend(stroke, &points);
        }
        if let Some(b) = changed {
            self.grow_bounds(b);
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
            _ => self.mode,
        };
        self.started_new = self.drag_mode == SelectionMode::New;
        self.stroke_bounds = None;

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
        if let Some(bounds) = self.stroke_bounds.take() {
            if self.auto_enhance {
                canvas.quick_select_auto_enhance(bounds);
            }
        }
        // Photoshop's Quick Selection moves on to Add once the first stroke
        // has made a selection, so the next stroke extends it.
        if self.started_new && self.mode == SelectionMode::New && canvas.selection.active {
            self.mode = SelectionMode::Add;
        }
        self.started_new = false;

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
        self.stroke_bounds = None;
        self.started_new = false;
        self.stroke = None;
        self.intersect = None;
        self.pending.clear();
        self.undo_cmd = None;
        self.snapshot_before.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::Canvas;
    use crate::core::document::{Document, DocumentId};

    /// Two separate red squares on a blue background.
    fn two_squares() -> Document {
        let (w, h) = (200u32, 120u32);
        let mut px = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let red =
                    (30..80).contains(&y) && ((20..70).contains(&x) || (120..170).contains(&x));
                let c = if red {
                    [210, 40, 40, 255]
                } else {
                    [40, 70, 180, 255]
                };
                px[((y * w + x) * 4) as usize..][..4].copy_from_slice(&c);
            }
        }
        Document::from_canvas(DocumentId(1), Canvas::from_rgba(px, w, h), None)
    }

    fn paint(tool: &mut SmartSelectTool, doc: &mut Document, points: &[(f32, f32)]) {
        let mut ctx = ToolCtx::new(doc, [0, 0, 0, 255], [255; 4], 1.0, 0.0, 0.0);
        let mut prev = PointerEvent::new(points[0].0, points[0].1);
        tool.on_press(prev, &mut ctx);
        for &(x, y) in &points[1..] {
            let ev = PointerEvent::new(x, y);
            tool.on_drag(ev, &prev, &mut ctx);
            prev = ev;
        }
        tool.on_frame(&mut ctx);
        tool.on_release(prev, &mut ctx);
    }

    fn selected(doc: &Document, x: u32, y: u32) -> bool {
        doc.canvas.selection.mask[(y * doc.canvas.width + x) as usize] >= 128
    }

    #[test]
    fn first_new_stroke_moves_the_tool_to_add() {
        let mut doc = two_squares();
        let mut tool = SmartSelectTool::new();
        tool.brush_size = 8.0;
        assert_eq!(tool.mode, SelectionMode::New);

        paint(&mut tool, &mut doc, &[(40.0, 55.0), (50.0, 55.0)]);
        assert!(selected(&doc, 45, 55));
        assert_eq!(
            tool.mode,
            SelectionMode::Add,
            "Quick Selection moves on to Add"
        );

        // A plain second stroke now extends the selection.
        paint(&mut tool, &mut doc, &[(140.0, 55.0), (150.0, 55.0)]);
        assert!(selected(&doc, 45, 55) && selected(&doc, 145, 55));

        // Picking New again replaces it on the next stroke.
        tool.mode = SelectionMode::New;
        paint(&mut tool, &mut doc, &[(140.0, 55.0), (150.0, 55.0)]);
        assert!(!selected(&doc, 45, 55) && selected(&doc, 145, 55));
        assert_eq!(tool.mode, SelectionMode::Add);
    }

    #[test]
    fn auto_enhance_gives_the_stroke_a_soft_rim() {
        let soft_rim = |enhance: bool| {
            let mut doc = two_squares();
            let mut tool = SmartSelectTool::new();
            tool.brush_size = 8.0;
            tool.auto_enhance = enhance;
            paint(&mut tool, &mut doc, &[(40.0, 55.0), (50.0, 55.0)]);
            doc.canvas.selection.mask.iter().any(|&v| v > 0 && v < 255)
        };
        assert!(soft_rim(true), "Auto-Enhance anti-aliases the rim");
        assert!(!soft_rim(false), "without it the cut stays binary");
    }
}

// Refine Brush — paints on the open Refine Selection session (see
// `core::refine`), never on the selection or the document history directly.
// Without one, it queues its strokes for a dialog that paints its own masks
// (`Canvas::mask_brush`, Chỉnh chân dung).
//
// Modes: Smart (colour-aware matting for hair / fur), Add, Subtract. Holding
// Alt reverses the mode (Smart → put the opening selection back), as in
// Photoshop. Strokes are undone inside the panel with Ctrl+Z.

use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::refine::{MaskBrushEvent, StampOp};
use crate::core::selection::RefineBrushMode;

pub struct RefineBrushTool {
    /// Tip diameter in canvas pixels.
    pub size: f32,
    pub hardness: f32,
    pub mode: RefineBrushMode,

    dragging: bool,
    op: StampOp,
    last: (f32, f32),
    pending: Vec<(f32, f32)>,
}

impl RefineBrushTool {
    pub fn new() -> Self {
        Self {
            size: 80.0,
            hardness: 0.5,
            mode: RefineBrushMode::Smart,
            dragging: false,
            op: StampOp::Smart,
            last: (0.0, 0.0),
            pending: Vec::new(),
        }
    }

    fn radius(&self) -> f32 {
        (self.size * 0.5).max(0.5)
    }

    /// Dab positions from the last dab along the pending cursor path.
    fn walk(&mut self) -> Vec<(f32, f32)> {
        let spacing = (self.radius() * 0.35).max(1.0);
        let mut dabs = Vec::new();
        for (x, y) in std::mem::take(&mut self.pending) {
            let (x0, y0) = self.last;
            let dist = ((x - x0).powi(2) + (y - y0).powi(2)).sqrt();
            if dist < spacing {
                continue;
            }
            let steps = ((dist / spacing).floor() as u32).clamp(1, 4000);
            for i in 1..=steps {
                let t = i as f32 * spacing / dist;
                dabs.push((x0 + (x - x0) * t, y0 + (y - y0) * t));
            }
            self.last = *dabs.last().unwrap_or(&(x, y));
        }
        dabs
    }

    fn flush(&mut self, ctx: &mut ToolCtx) {
        let dabs = self.walk();
        if !dabs.is_empty() {
            self.paint(ctx, dabs);
        }
    }

    fn paint(&self, ctx: &mut ToolCtx, points: Vec<(f32, f32)>) {
        let (radius, hardness) = (self.radius(), self.hardness);
        let canvas = ctx.canvas_mut();
        if canvas.refine.is_some() {
            canvas.refine_paint(self.op, &points, radius, hardness);
        } else if let Some(queue) = canvas.mask_brush.as_mut() {
            queue.push(MaskBrushEvent::Dabs {
                points,
                radius,
                hardness,
            });
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
        self.radius()
    }

    fn on_press(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        let canvas = ctx.canvas_mut();
        self.op = StampOp::for_mode(self.mode, event.alt);
        if canvas.refine.is_some() {
            canvas.refine_stroke_begin(self.op);
        } else if let Some(queue) = canvas.mask_brush.as_mut() {
            queue.push(MaskBrushEvent::Begin(self.op));
        } else {
            return ToolResponse::none();
        }
        let at = (event.canvas_x, event.canvas_y);
        self.paint(ctx, vec![at]);
        self.last = at;
        self.pending.clear();
        self.dragging = true;
        ToolResponse::repaint()
    }

    fn on_drag(
        &mut self,
        event: PointerEvent,
        _prev: &PointerEvent,
        _ctx: &mut ToolCtx,
    ) -> ToolResponse {
        if self.dragging {
            self.pending.push((event.canvas_x, event.canvas_y));
        }
        ToolResponse::none()
    }

    fn on_frame(&mut self, ctx: &mut ToolCtx) -> ToolResponse {
        if !self.dragging || self.pending.is_empty() {
            return ToolResponse::none();
        }
        self.flush(ctx);
        ToolResponse::repaint()
    }

    fn on_release(&mut self, _event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        if !self.dragging {
            return ToolResponse::none();
        }
        self.flush(ctx);
        self.dragging = false;
        let canvas = ctx.canvas_mut();
        if canvas.refine.is_some() {
            canvas.refine_stroke_end();
        } else if let Some(queue) = canvas.mask_brush.as_mut() {
            queue.push(MaskBrushEvent::End);
        }
        ToolResponse::repaint()
    }

    fn on_cancel(&mut self) {
        self.dragging = false;
        self.pending.clear();
    }
}

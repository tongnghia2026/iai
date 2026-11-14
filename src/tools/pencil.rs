use super::brush::{BrushSettings, BrushTool};
use super::{PointerEvent, Tool, ToolCtx, ToolResponse};

pub struct PencilTool {
    pub size: f32,
    pub opacity: f32,
    pub color: [u8; 4],
    cached_settings: BrushSettings,
    /// Distance left until the next dab, carried across drag events (the
    /// path without a stroke buffer).
    stroke_residual: f32,
    /// Per-stroke coverage buffer, as Brush and Eraser use: overlapping
    /// dabs of one stroke no longer build up past Opacity (14/11: the
    /// Pencil all but ignored it).
    stroke: Option<super::brush::StrokeBuffer>,
}

impl Default for PencilTool {
    fn default() -> Self {
        Self::new()
    }
}

impl PencilTool {
    pub fn new() -> Self {
        Self {
            size: 5.0,
            opacity: 1.0,
            color: [0, 0, 0, 255],
            cached_settings: BrushSettings {
                size: 5.0,
                hardness: 1.0,
                opacity: 1.0,
                color: [0, 0, 0, 255],
                spacing: 0.1,
                is_eraser: false,
                smoothing: 0.0,
                flow: 1.0,
            },
            stroke_residual: 0.0,
            stroke: None,
        }
    }

    fn sync_cache(&mut self) {
        self.cached_settings.size = self.size;
        self.cached_settings.hardness = 1.0;
        self.cached_settings.opacity = self.opacity;
        self.cached_settings.color = self.color;
        self.cached_settings.spacing = 0.1;
    }
}

impl Tool for PencilTool {
    fn id(&self) -> &'static str {
        "pencil"
    }
    fn name(&self) -> &str {
        "Pencil"
    }
    fn shortcut(&self) -> Option<char> {
        Some('B')
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::Pencil
    }
    fn paints(&self) -> bool {
        true
    }
    fn cursor_size(&self) -> f32 {
        self.size * 0.5
    }

    fn on_press(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        ctx.canvas_mut().begin_stroke("Pencil Stroke");
        self.sync_cache();
        let (cx, cy) = (event.canvas_x, event.canvas_y);
        let alpha_plane = matches!(
            ctx.canvas().channels.view,
            crate::core::channels::ChannelView::Alpha(_)
        );
        self.stroke = if alpha_plane {
            None
        } else {
            super::brush::StrokeBuffer::begin(ctx.canvas())
        };
        if let Some(stroke) = self.stroke.as_mut() {
            BrushTool::paint_cpu_dab_stroked(
                &self.cached_settings,
                ctx.canvas_mut(),
                cx,
                cy,
                stroke,
            );
            stroke.residual = self.cached_settings.dab_spacing();
        } else {
            BrushTool::paint_cpu_dab(&self.cached_settings, ctx.canvas_mut(), cx, cy);
            self.stroke_residual = self.cached_settings.dab_spacing();
        }
        ToolResponse::repaint()
    }

    fn on_drag(
        &mut self,
        event: PointerEvent,
        prev: &PointerEvent,
        ctx: &mut ToolCtx,
    ) -> ToolResponse {
        self.sync_cache();
        let (x0, y0) = (prev.canvas_x, prev.canvas_y);
        let (x1, y1) = (event.canvas_x, event.canvas_y);
        if let Some(stroke) = self.stroke.as_mut() {
            BrushTool::paint_cpu_stroke_segment_stroked(
                &self.cached_settings,
                ctx.canvas_mut(),
                x0,
                y0,
                x1,
                y1,
                stroke,
            );
        } else {
            BrushTool::paint_cpu_stroke_segment(
                &self.cached_settings,
                ctx.canvas_mut(),
                x0,
                y0,
                x1,
                y1,
                &mut self.stroke_residual,
            );
        }
        ToolResponse::repaint()
    }

    fn on_release(&mut self, _event: PointerEvent, _ctx: &mut ToolCtx) -> ToolResponse {
        self.stroke = None;
        ToolResponse::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::Canvas;
    use crate::core::document::{Document, DocumentId};

    /// A Pencil stroke at 50 % Opacity on white leaves mid grey where its
    /// dabs overlap, not near black (14/11).
    #[test]
    fn pencil_keeps_to_its_opacity() {
        let px = [255u8; 64 * 64 * 4].to_vec();
        let mut doc = Document::from_canvas(DocumentId(1), Canvas::from_rgba(px, 64, 64), None);
        let mut pencil = PencilTool::new();
        pencil.size = 8.0;
        pencil.opacity = 0.5;
        let mut ctx = ToolCtx::new(&mut doc, [0, 0, 0, 255], [255; 4], 1.0, 0.0, 0.0);
        let mut prev = PointerEvent::new(10.0, 32.0);
        pencil.on_press(prev, &mut ctx);
        for x in 11..54 {
            let ev = PointerEvent::new(x as f32, 32.0);
            pencil.on_drag(ev, &prev, &mut ctx);
            prev = ev;
        }
        pencil.on_release(prev, &mut ctx);
        let (r, ..) = doc
            .canvas
            .layer_stack
            .active_layer()
            .tiles
            .get_pixel(32, 32);
        assert!((118..=138).contains(&r), "got {r}");
    }
}

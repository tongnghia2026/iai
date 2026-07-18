use super::brush::{BrushSettings, BrushTool};
use super::{PointerEvent, Tool, ToolCtx, ToolResponse};

pub struct PencilTool {
    pub size: f32,
    pub opacity: f32,
    pub color: [u8; 4],
    cached_settings: BrushSettings,
    /// Distance left until the next dab, carried across drag events.
    stroke_residual: f32,
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
        BrushTool::paint_cpu_dab(&self.cached_settings, ctx.canvas_mut(), cx, cy);
        self.stroke_residual = self.cached_settings.dab_spacing();
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
        BrushTool::paint_cpu_stroke_segment(
            &self.cached_settings,
            ctx.canvas_mut(),
            x0,
            y0,
            x1,
            y1,
            &mut self.stroke_residual,
        );
        ToolResponse::repaint()
    }

    fn on_release(&mut self, _event: PointerEvent, _ctx: &mut ToolCtx) -> ToolResponse {
        ToolResponse::none()
    }
}

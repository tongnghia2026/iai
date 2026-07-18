use super::brush::BrushSettings;
use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::canvas::Canvas;

pub struct EraserTool {
    pub size: f32,
    pub hardness: f32,
    pub opacity: f32,
    pub spacing: f32,
    pub flow: f32,
    pub smoothing: f32,
    /// Index into `BRUSH_PRESETS` (eraser shares the brush tip presets) or
    /// `PRESET_CUSTOM` when the tip was hand-edited.
    pub preset_idx: usize,
    pub bg_color: [u8; 4],
    pub cached_settings: BrushSettings,
    pub pending_dabs: Vec<crate::tools::brush::BrushDab>,
    pub stroke_dabs: Vec<crate::tools::brush::BrushDab>,
    /// Per-stroke coverage buffer (PS opacity/flow model); Some while the
    /// pointer is down.
    stroke: Option<crate::tools::brush::StrokeBuffer>,
    /// Distance walker for alpha-channel plane strokes, which do not use the
    /// layer-tile stroke buffer.
    stroke_residual: f32,
}

impl EraserTool {
    pub fn new() -> Self {
        Self {
            size: 20.0,
            hardness: 0.5,
            opacity: 1.0,
            spacing: 0.25,
            flow: 1.0,
            smoothing: 0.0,
            preset_idx: crate::tools::brush::PRESET_CUSTOM,
            bg_color: [255, 255, 255, 255],
            cached_settings: BrushSettings {
                size: 20.0,
                hardness: 0.8,
                opacity: 1.0,
                color: [255, 255, 255, 255],
                spacing: 0.25,
                is_eraser: true,
                smoothing: 0.0,
                flow: 1.0,
            },
            pending_dabs: Vec::new(),
            stroke_dabs: Vec::new(),
            stroke: None,
            stroke_residual: 0.0,
        }
    }

    /// Apply a brush tip preset (eraser reuses the brush presets: hardness +
    /// spacing + flow). Size/opacity are kept.
    pub fn apply_preset(&mut self, idx: usize) {
        if let Some(p) = crate::tools::brush::BRUSH_PRESETS.get(idx) {
            self.hardness = p.hardness;
            self.spacing = p.spacing;
            self.flow = p.flow;
            self.preset_idx = idx;
        }
    }

    fn sync_cache(&mut self, canvas: &Canvas) {
        self.cached_settings.size = self.size;
        self.cached_settings.hardness = self.hardness;
        self.cached_settings.opacity = self.opacity;
        self.cached_settings.spacing = self.spacing;
        self.cached_settings.flow = self.flow;
        self.cached_settings.smoothing = self.smoothing;

        let layer = &canvas.layer_stack.layers[canvas.layer_stack.active_idx];
        if layer.is_background || layer.locked {
            self.cached_settings.is_eraser = false;
            self.cached_settings.color = self.bg_color;
        } else {
            self.cached_settings.is_eraser = true;
            // Normal (alpha) erasing never reads the colour, but the
            // Channels-panel write gate erases a single channel by painting
            // the background colour's luma into it.
            self.cached_settings.color = self.bg_color;
        }
    }
}

impl Tool for EraserTool {
    fn id(&self) -> &'static str {
        "eraser"
    }
    fn name(&self) -> &str {
        "Eraser"
    }
    fn shortcut(&self) -> Option<char> {
        Some('E')
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::Eraser
    }
    fn paints(&self) -> bool {
        true
    }
    fn cursor_size(&self) -> f32 {
        self.size * 0.5
    }

    fn on_press(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        ctx.canvas_mut().begin_stroke("Eraser Stroke");
        self.sync_cache(ctx.canvas());
        let (cx, cy) = (event.canvas_x, event.canvas_y);
        let alpha_plane = matches!(
            ctx.canvas().channels.view,
            crate::core::channels::ChannelView::Alpha(_)
        );
        if alpha_plane {
            self.stroke = None;
            crate::tools::brush::BrushTool::paint_cpu_dab(
                &self.cached_settings,
                ctx.canvas_mut(),
                cx,
                cy,
            );
            self.stroke_residual = self.cached_settings.dab_spacing();
        } else {
            self.stroke = crate::tools::brush::StrokeBuffer::begin(ctx.canvas());
        }
        if let Some(stroke) = self.stroke.as_mut() {
            crate::tools::brush::BrushTool::paint_cpu_dab_stroked(
                &self.cached_settings,
                ctx.canvas_mut(),
                cx,
                cy,
                stroke,
            );
            stroke.residual = self.cached_settings.dab_spacing();
        }
        ToolResponse::repaint()
    }

    fn on_drag(
        &mut self,
        event: PointerEvent,
        prev: &PointerEvent,
        ctx: &mut ToolCtx,
    ) -> ToolResponse {
        self.sync_cache(ctx.canvas());
        let (x0, y0) = (prev.canvas_x, prev.canvas_y);
        let (x1, y1) = (event.canvas_x, event.canvas_y);
        if let Some(stroke) = self.stroke.as_mut() {
            crate::tools::brush::BrushTool::paint_cpu_stroke_segment_stroked(
                &self.cached_settings,
                ctx.canvas_mut(),
                x0,
                y0,
                x1,
                y1,
                stroke,
            );
        } else if matches!(
            ctx.canvas().channels.view,
            crate::core::channels::ChannelView::Alpha(_)
        ) {
            crate::tools::brush::BrushTool::paint_cpu_stroke_segment(
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
        self.stroke_dabs.clear();
        self.pending_dabs.clear();
        self.stroke = None;
        self.stroke_residual = 0.0;
        ToolResponse::none()
    }
}

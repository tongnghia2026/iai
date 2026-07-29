use crate::core::canvas::Canvas;
use crate::core::document::{Document, DocumentId};
use crate::core::selection::SelectionMode;

/// Identity of every tool IAI ships. Lives beside the [`Tool`] trait that
/// returns it so this layer never depends on the concrete `tools` module.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ToolId {
    Brush,
    Eraser,
    Pencil,
    Move,
    Crop,
    Zoom,
    Hand,
    Fill,
    Gradient,
    Eyedropper,
    SelectionRect,
    SelectionEllipse,
    Lasso,
    PolygonLasso,
    SmartSelect,
    Clone,
    Transform,
    RefineBrush,
    Text,
    Shape,
    Repair,
    PerspectiveCrop,
    Pen,
    Smudge,
    Dodge,
    Burn,
    Patch,
    /// Direct-selection of a Path layer's nodes (edit anchor points). Companion
    /// to Pen (create) and Move (whole-object transform).
    Node,
    /// Freehand vector drawing (Phase 6B): a drag commits an editable
    /// variable-width Path stroke, distinct from the pixel Brush.
    VectorBrush,
}

#[derive(Debug, Clone, Copy)]
pub struct PointerEvent {
    /// Position on the canvas (zoom + pan applied).
    pub canvas_x: f32,
    pub canvas_y: f32,
    /// Position on screen (pixels).
    pub screen_x: f32,
    pub screen_y: f32,
    /// Pen-tablet pressure (0.0–1.0). Mouse = 1.0.
    pub pressure: f32,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    /// Space is held — used to reposition the marquee while drawing (by convention).
    pub space: bool,
    /// Selection mode (New / Add / Subtract / Intersect) — derived from Shift/Alt at event time.
    pub selection_mode: SelectionMode,
}

impl PointerEvent {
    pub fn new(canvas_x: f32, canvas_y: f32) -> Self {
        Self {
            canvas_x,
            canvas_y,
            screen_x: canvas_x,
            screen_y: canvas_y,
            pressure: 1.0,
            shift: false,
            ctrl: false,
            alt: false,
            space: false,
            selection_mode: SelectionMode::New,
        }
    }
}

#[derive(Debug, Clone)]
pub struct KeyEvent {
    pub key_char: Option<char>,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ToolResponse {
    pub needs_redraw: bool,
    pub needs_composite: bool,
    pub cursor: Option<ToolCursor>,
    /// When an operation is blocked (e.g. the canvas is too large for the memory limit),
    /// the tool returns a message here for the app to show on the status bar instead of
    /// doing nothing silently. `&'static str` keeps ToolResponse `Copy`.
    pub status: Option<&'static str>,
}

impl ToolResponse {
    pub fn none() -> Self {
        Self::default()
    }
    pub fn redraw() -> Self {
        Self {
            needs_redraw: true,
            needs_composite: false,
            cursor: None,
            status: None,
        }
    }
    pub fn repaint() -> Self {
        Self {
            needs_redraw: true,
            needs_composite: true,
            cursor: None,
            status: None,
        }
    }

    /// Operation blocked — does nothing but tells the user why.
    /// Requests a redraw to clear any half-finished preview.
    pub fn blocked(msg: &'static str) -> Self {
        Self {
            needs_redraw: true,
            needs_composite: false,
            cursor: None,
            status: Some(msg),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ToolCursor {
    Crosshair,
    Brush,
    Move,
    Eyedropper,
    ResizeNS,
    ResizeEW,
    None,
}

pub struct ToolCtx<'a> {
    pub document: &'a mut Document,
    pub fg_color: [u8; 4],
    pub bg_color: [u8; 4],
    pub zoom: f32,
    pub pan_x: f32,
    pub pan_y: f32,
}

impl<'a> ToolCtx<'a> {
    pub fn new(
        document: &'a mut Document,
        fg_color: [u8; 4],
        bg_color: [u8; 4],
        zoom: f32,
        pan_x: f32,
        pan_y: f32,
    ) -> Self {
        Self {
            document,
            fg_color,
            bg_color,
            zoom,
            pan_x,
            pan_y,
        }
    }

    #[inline]
    pub fn canvas(&self) -> &Canvas {
        &self.document.canvas
    }
    #[inline]
    pub fn canvas_mut(&mut self) -> &mut Canvas {
        &mut self.document.canvas
    }
    #[inline]
    pub fn doc_id(&self) -> DocumentId {
        self.document.id
    }

    pub fn screen_to_canvas(&self, sx: f32, sy: f32) -> (f32, f32) {
        ((sx - self.pan_x) / self.zoom, (sy - self.pan_y) / self.zoom)
    }

    pub fn canvas_to_screen(&self, cx: f32, cy: f32) -> (f32, f32) {
        (cx * self.zoom + self.pan_x, cy * self.zoom + self.pan_y)
    }
}

pub trait Tool: Send {
    /// Unique ID, lowercase no spaces. E.g. "brush", "eraser".
    fn id(&self) -> &'static str;
    /// Display name.
    fn name(&self) -> &str;
    /// Tabler icon name. Xem https://tabler.io/icons
    fn icon(&self) -> &'static str {
        ""
    }
    /// Keyboard shortcut
    fn shortcut(&self) -> Option<char> {
        None
    }
    /// ToolId enum cho App UI (toolbar highlight, cursor switching).
    /// Needed so the app doesn't string-match every render frame.
    fn tool_id(&self) -> ToolId;
    /// Does the tool paint onto the layer? (used to disable when the layer is locked)
    fn paints(&self) -> bool {
        false
    }
    /// Cursor ring size (pixels in canvas space, zoom not applied).
    fn cursor_size(&self) -> f32 {
        0.0
    }

    fn activate(&mut self, _ctx: &mut ToolCtx) {}
    fn deactivate(&mut self, _ctx: &mut ToolCtx) {}

    fn on_press(&mut self, _event: PointerEvent, _ctx: &mut ToolCtx) -> ToolResponse {
        ToolResponse::none()
    }
    fn on_drag(
        &mut self,
        _event: PointerEvent,
        _prev: &PointerEvent,
        _ctx: &mut ToolCtx,
    ) -> ToolResponse {
        ToolResponse::none()
    }
    fn on_release(&mut self, _event: PointerEvent, _ctx: &mut ToolCtx) -> ToolResponse {
        ToolResponse::none()
    }

    fn on_cancel(&mut self) {}
    fn on_confirm(&mut self, _ctx: &mut ToolCtx) {}

    fn on_key(&mut self, _event: KeyEvent, _ctx: &mut ToolCtx) -> ToolResponse {
        ToolResponse::none()
    }

    fn options_ui(&mut self, _ui: &mut egui::Ui) {}
    fn canvas_overlay(&self, _painter: &egui::Painter, _ctx: &ToolCtx) {}
}

//! One of the six App subsystems — see `state.rs` for the tree.

use super::state::*;

/// OS windows and the render runtime.
///
/// Owns winit windows, the GPU state, both egui runtimes, custom cursors,
/// startup progress and all compositor scheduling (recompose throttles, GPU
/// sync regions, marching-ants cadence). Invariant: everything here is
/// reconstructible — dropping the GPU or a window loses no document state,
/// which is what makes device-lost recovery possible.
pub struct WindowRuntime {
    pub(in crate::app) window: Option<Arc<Window>>,
    pub(in crate::app) window_visible: bool,
    pub(in crate::app) window_focused: bool,
    /// The Develop stage's second OS window (Track D / D1), when open. It has
    /// its own egui context + egui-winit state (VN font + theme applied on
    /// creation); its GPU surface lives in `GpuState::develop`. `None` when the
    /// Develop window is closed.
    pub(in crate::app) develop_window: Option<Arc<Window>>,
    /// A Develop window whose session is already closed but whose OS window is
    /// deliberately kept alive until the main surface has presented its final
    /// canvas frame. This prevents DWM exposing the main window's stale buffer
    /// for one frame during the transition back from Develop.
    pub(in crate::app) retiring_develop_window: Option<(Arc<Window>, u8)>,
    pub(in crate::app) develop_egui_ctx: Option<egui::Context>,
    pub(in crate::app) develop_egui_state: Option<egui_winit::State>,
    pub(in crate::app) gpu: Option<GpuState>,
    pub(in crate::app) egui_ctx: egui::Context,
    pub(in crate::app) egui_state: Option<egui_winit::State>,
    /// The installed UI fonts, kept so GPU-device recovery can rebuild the
    /// egui context with identical text rendering.
    pub(in crate::app) ui_fonts: Option<egui::FontDefinitions>,
    pub(in crate::app) startup_phase: StartupPhase,
    pub(in crate::app) startup_rx: Option<std::sync::mpsc::Receiver<StartupProgress>>,
    /// Accumulated startup log lines. Kept for diagnostics; startup no longer renders a splash.
    pub(in crate::app) startup_log: Vec<String>,
    pub(in crate::app) cursor_ring: Option<winit::window::CustomCursor>,
    pub(in crate::app) cursor_crosshair: Option<winit::window::CustomCursor>,
    pub(in crate::app) cursor_selection_crosshair: Option<winit::window::CustomCursor>,
    /// Tiny high-contrast crosshair used while placing Perspective Crop points.
    pub(in crate::app) cursor_perspective_crosshair: Option<winit::window::CustomCursor>,
    pub(in crate::app) cursor_lasso: Option<winit::window::CustomCursor>,
    pub(in crate::app) cursor_crop: Option<winit::window::CustomCursor>,
    /// Pipette cursor for the Alt-hold temporary eyedropper (paint tools).
    pub(in crate::app) cursor_eyedropper: Option<winit::window::CustomCursor>,
    pub(in crate::app) cursor_fill: Option<winit::window::CustomCursor>,
    pub(in crate::app) cursor_gradient: Option<winit::window::CustomCursor>,
    pub(in crate::app) cursor_hand: Option<winit::window::CustomCursor>,
    /// Pen-nib cursor for the Pen tool.
    pub(in crate::app) cursor_pen: Option<winit::window::CustomCursor>,
    /// Small fixed ring shown over the paint-colour dialog (peer of cursor_ring).
    pub(in crate::app) cursor_small_ring: Option<winit::window::CustomCursor>,
    /// Magnifier cursors for the Zoom tool (Windows has no native zoom cursor, so
    /// `CursorIcon::ZoomIn/Out` falls back to an arrow — use a glyph instead).
    pub(in crate::app) cursor_zoom_in: Option<winit::window::CustomCursor>,
    pub(in crate::app) cursor_zoom_out: Option<winit::window::CustomCursor>,
    pub(in crate::app) last_cursor_radius: u32,
    pub(in crate::app) start_time: Instant,
    pub(in crate::app) pending_gpu_sync: crate::core::canvas::DirtyRegion,
    /// Layer ID with unsynced GPU brush data. Skip CPU upload only when the active
    /// layer == pending_gpu_sync_layer_id, so other layers' uploads aren't blocked.
    pub(in crate::app) pending_gpu_sync_layer_id: usize,
    pub(in crate::app) last_selection_push: Instant,
    /// Timestamp of the last marching-ants frame while idle (selection active, no
    /// painting/transform). Throttles the animation to ~15fps instead of polling at
    /// 60fps+, keeping idle CPU/GPU low when a selection exists but nothing is happening.
    pub(in crate::app) last_ants_frame: Instant,
    /// Tessellated egui output from the last full UI build, reused verbatim on
    /// "ants-only" frames (selection animation tick with nothing else changing) so
    /// the whole immediate-mode UI doesn't get rebuilt + re-tessellated ~15×/sec
    /// just to advance the marching-ants. Refreshed on every full frame.
    pub(in crate::app) cached_egui_primitives: Vec<egui::ClippedPrimitive>,
    /// Set by `about_to_wait` when the pending redraw was scheduled solely by the
    /// marching-ants timer. Any other window event clears it, so the fast path is
    /// only taken when the frame truly carries no new UI state (see RedrawRequested).
    pub(in crate::app) ants_redraw_pending: bool,
    /// Last theme mode pushed into egui's global Visuals. `apply_theme` is rerun
    /// only when `ui.theme_mode` differs from this, not every frame.
    pub(in crate::app) theme_applied: Option<crate::ui::theme::ThemeMode>,
    /// Absolute time the next egui frame must render, derived from
    /// FullOutput.viewport_output[ROOT].repaint_delay after the last frame.
    /// Single source of truth for choosing ControlFlow in about_to_wait:
    ///   Some(t) in the past   → render now (animation/interacting) → Poll
    ///   Some(t) in the future → WaitUntil(t), then render
    ///   None                  → fully idle (egui returns Duration::MAX) → Wait, ~0% CPU
    /// Stores an absolute deadline (not a Duration) because WaitUntil does NOT emit
    /// RedrawRequested on expiry — about_to_wait must detect the passed deadline itself.
    pub(in crate::app) egui_repaint_deadline: Option<std::time::Instant>,
    /// Last selection texture cache key uploaded to GPU.
    /// Normal canvas keys mostly track mask_revision; large-canvas keys also include
    /// view/zoom/selection offset because the mask is baked into screen space.
    pub(in crate::app) last_uploaded_mask_key: u64,
    #[allow(dead_code)]
    pub(in crate::app) cpu_patch_buf: Vec<u8>,
    #[allow(dead_code)]
    pub(in crate::app) viewport_buf: Vec<u8>,
    #[allow(dead_code)]
    pub(in crate::app) viewport_patch_buf: Vec<u8>,
    pub(in crate::app) pending_view_change: bool,
    pub(in crate::app) view_recompose_last: Option<std::time::Instant>,
    pub(in crate::app) view_recompose_cost: std::time::Duration,
    pub(in crate::app) view_recompose_deadline: Option<std::time::Instant>,
    /// Coalesces expensive live Move/Free Transform recomposites while the mouse
    /// is producing events faster than the GPU can present them.
    pub(in crate::app) interactive_recompose_pending: bool,
    pub(in crate::app) interactive_recompose_last: Option<std::time::Instant>,
    pub(in crate::app) interactive_recompose_cost: std::time::Duration,
    /// True while rendering a frame OR while a blocking native (rfd) dialog is open.
    /// A native dialog runs a nested message loop pumping WM_PAINT → winit re-enters
    /// RedrawRequested recursively → gpu.render() re-enters and stalls on
    /// get_current_texture (frame latency 1) → deadlock, frozen dialog. This guard
    /// makes the nested RedrawRequested return immediately, breaking the deadlock.
    pub(in crate::app) rendering: bool,
}

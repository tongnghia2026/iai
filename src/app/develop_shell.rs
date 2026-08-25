//! One of the six App subsystems — see `state.rs` for the tree.

use super::state::*;

/// The active pointer tool in the Develop window's canvas viewport (D5 tool
/// rail). View state, not serialized. Middle-drag always pans regardless of the
/// tool, and an armed local mask always takes the left drag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DevelopTool {
    /// Left-drag pans the viewport (the default, matching the middle-drag pan).
    #[default]
    Hand,
    /// Left-click zooms in about the cursor; Alt+click zooms out.
    Zoom,
    /// Left-click samples the pixel and solves Temperature/Tint to neutralise it.
    WhiteBalance,
}

/// The Develop window's session state.
///
/// Owns the second OS window's view (zoom/pan/fit), preview and histogram
/// caches, the filmstrip session list and the bake pipeline state. Invariant:
/// everything here previews `develop_settings` — the only path that changes
/// document pixels is the bake commit.
pub struct DevelopShell {
    /// The Develop window's own canvas view (physical px), independent of the
    /// main window. `fit` re-fits the canvas to the viewport each frame until
    /// the user zooms/pans; scroll zooms about the cursor, middle-drag pans.
    pub(in crate::app) develop_view_zoom: f32,
    pub(in crate::app) develop_view_off: (f32, f32),
    pub(in crate::app) develop_view_fit: bool,
    /// Last cursor position in the Develop window (physical px) and the anchor
    /// of an in-progress middle-button pan.
    pub(in crate::app) develop_cursor: (f32, f32),
    pub(in crate::app) develop_pan_drag: Option<(f32, f32)>,
    /// The active canvas tool in the viewport (D5 left tool rail).
    pub(in crate::app) develop_tool: DevelopTool,
    /// Mode B only: the Develop-window view the compositor last baked, as
    /// `[off_x, off_y, zoom]` bit patterns + window `[w, h]`. Mode B composites
    /// carry the view transform, so the Develop window must recomposite when its
    /// own zoom/pan/size diverges from this. `None` = nothing baked for it yet.
    pub(in crate::app) develop_composited_view: Option<[u32; 5]>,
    pub(in crate::app) develop_preview: Option<DevelopPreviewState>,
    /// R/G/B/Luma histogram of the develop source layer through the CURRENT
    /// settings — the curve editor's backdrop, live-updated as sliders move.
    pub(in crate::app) develop_histogram: Option<std::sync::Arc<[[f32; 256]; 4]>>,
    /// Wall-clock of the last histogram re-bin + whether a newer settings state
    /// is waiting: binning costs a few ms and a drag requests it every tick, so
    /// re-bins are throttled and the resting value lands via a trailing flush.
    pub(in crate::app) develop_histogram_at: Option<std::time::Instant>,
    pub(in crate::app) develop_histogram_stale: bool,
    /// Display-post-render, pre-monitor scopes, refreshed on the same throttle
    /// as the live histogram and shared with both Develop UI hosts.
    pub(in crate::app) develop_scopes:
        Option<std::sync::Arc<crate::core::develop2::scopes::DevelopScopes>>,
    pub(in crate::app) develop_scopes_revision: u64,
    pub(in crate::app) develop_scope_visibility: u8,
    /// Display R/G/B (0–255) under the Develop-window cursor, evaluated from
    /// the scene master through the current settings each frame the cursor
    /// sits over the viewport. `None` off-viewport or without a scene.
    pub(in crate::app) develop_readout: Option<[u8; 3]>,
    /// Persisted open/closed state of the Develop panel's sections, in panel
    /// order (prefs.json `develop_sections_open`).
    pub(in crate::app) develop_sections_open: [bool; crate::ui::develop::DEV_PANEL_SECTIONS],
    /// Anchor of an in-progress local-mask placement drag, in the Develop
    /// layer's normalized mask coordinates.
    pub(in crate::app) develop_local_drag: Option<(f32, f32)>,
    /// The Develop stage's session across documents (D3 filmstrip): every
    /// document taking part, in filmstrip order. One entry for a plain menu
    /// Develop; opening N RAWs appends N transient entries. The ACTIVE entry's
    /// live settings are `ui.develop_settings`; an entry's `settings` field is
    /// its saved state while another image is active. Emptied on commit
    /// ("Open Image" bakes only the active entry and discards other transient
    /// placeholders) and on cancel (which closes every transient RAW document).
    /// `pending_develop` defers entering Develop to
    /// just after the load is attached (see `enter_pending_develop`).
    pub(in crate::app) develop_session: Vec<DevelopSessionEntry>,
    /// In-progress "Open Image" bake over the session (see [`DevelopBakeAll`]).
    pub(in crate::app) develop_bake_all: Option<DevelopBakeAll>,
    /// Open Image was requested while the interactive GPU frame was still
    /// active. Wait for the exact CPU settled frame before starting the commit
    /// queue so teardown can never expose a different set of pixels.
    pub(in crate::app) develop_commit_after_refine: bool,
    /// Filmstrip thumbnails for this Develop session (textures on the Develop
    /// window's egui context; dropped on teardown).
    pub(in crate::app) develop_thumbs: std::collections::HashMap<DocumentId, egui::TextureHandle>,
    /// RAW decodes waiting to enter the Develop stage, drained next frame by
    /// `enter_pending_develop` (after the load attaches). A queue: a multi-open
    /// batch lands one decode at a time, and every one joins the session.
    pub(in crate::app) pending_develop: Vec<DocumentId>,
    pub(in crate::app) develop_gpu_preview_dirty: bool,
    pub(in crate::app) develop_gpu_preview_immediate: bool,
    pub(in crate::app) develop_gpu_recompose_last: Option<std::time::Instant>,
    pub(in crate::app) develop_gpu_recompose_cost: std::time::Duration,
    /// Cached GPU-preview region proxies, reused across a drag while the tone stage
    /// is unchanged (see [`DevelopProxyCache`]).
    pub(in crate::app) develop_proxy_cache: Option<DevelopProxyCache>,
    /// Throttle state for the CR tone-proxy rebuild: a Light-slider drag changes the
    /// tone signature every frame, so the proxy rebuild is rate-limited to its own
    /// cost (×1.5) to keep dragging responsive (see `build_develop_gpu_preview`).
    pub(in crate::app) develop_proxy_last: Option<std::time::Instant>,
    pub(in crate::app) develop_proxy_cost: std::time::Duration,
}

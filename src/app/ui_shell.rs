//! One of the six App subsystems — see `state.rs` for the tree.

use super::state::*;

/// UI chrome the user steers: dialogs, panels, preferences, presets,
/// proof/print settings and dialog-driven preview sessions.
///
/// Invariant: this is presentation state. Closing every dialog and dropping
/// this struct must never lose document content.
pub struct UiShell {
    pub(in crate::app) ui: UiState,
    pub(in crate::app) ui_data_cache: UiDataCache,
    pub(in crate::app) status_msg: String,
    /// Exit the app after the Save (Save & Exit) dialog finishes writing. Cancelled
    /// if the user cancels the dialog. Honored in poll_file_dialog.
    pub(in crate::app) exit_requested: bool,
    /// A Save chosen by the per-tab exit sweep is waiting for its file dialog.
    pub(in crate::app) exit_save_pending: bool,
    /// Close the file after the Save (Save & Close) dialog finishes (see exit_requested).
    pub(in crate::app) close_requested: bool,
    pub(in crate::app) toolbar_w: f32,
    pub(in crate::app) panel_r_w: f32,
    pub(in crate::app) canvas_unit: crate::core::units::Unit,
    /// Soft-proof (View ▸ Proof) display state. Display-only — applied through the
    /// blit-shader 3D LUT, never altering document pixels or export.
    pub(in crate::app) proof_enabled: bool,
    pub(in crate::app) proof_target: ProofTarget,
    pub(in crate::app) proof_gamut_warn: bool,
    /// Display colour management: when on, the composite is corrected to the
    /// monitor's ICC profile in the blit shader (same 3D-LUT path as soft proof).
    pub(in crate::app) display_cms_enabled: bool,
    pub(in crate::app) display_profile: Option<Vec<u8>>,
    pub(in crate::app) display_profile_name: String,
    /// Page setup for File ▸ Print (persists across the dialog opening).
    pub(in crate::app) print_layout: crate::core::print::PrintLayout,
    pub(in crate::app) print_printers: Vec<crate::core::print::PrinterInfo>,
    pub(in crate::app) print_selected_printer: String,
    pub(in crate::app) print_copies: u32,
    /// Optional printer ICC profile for app-managed colour conversion (RGB
    /// profiles only for now). None = printer/OS manages colour (PDF is tagged
    /// with the document sRGB profile + rendering intent).
    pub(in crate::app) print_printer_profile: Option<Vec<u8>>,
    pub(in crate::app) print_printer_profile_name: String,
    pub(in crate::app) adjustment_preview: Option<AdjustmentPreviewState>,
    /// Latest adjustment params awaiting an (expensive) live-preview recompute.
    /// The dialog stores params every frame (cheap) but the full-layer apply +
    /// recomposite is throttled via [`Self::flush_pending_adjustment_preview`] so
    /// dragging stays at full FPS on large images.
    pub(in crate::app) adjustment_preview_pending: Option<crate::core::layer::AdjustmentType>,
    /// Wall-clock of the last preview recompute, and how long it took — used to
    /// adapt the throttle interval to the image size.
    pub(in crate::app) adjustment_preview_last: Option<std::time::Instant>,
    pub(in crate::app) adjustment_preview_cost: std::time::Duration,
    pub(in crate::app) filter_preview: Option<FilterPreviewSession>,
    /// Live-preview session for the "Làm sạch bản scan" dialog.
    pub(in crate::app) scan_preview: Option<ScanPreviewSession>,
    pub(in crate::app) user_presets: std::sync::Arc<Vec<crate::core::presets::SizePreset>>,
    /// Named Develop slider sets saved by the user (develop_presets.json).
    pub(in crate::app) develop_presets: std::sync::Arc<Vec<crate::core::presets::DevelopPreset>>,
    /// User-saved Levels/Curves presets (adjustment_presets.json).
    pub(in crate::app) adjustment_presets: std::sync::Arc<crate::core::presets::AdjustmentPresets>,
}

//! One of the six App subsystems — see `state.rs` for the tree.

use super::state::*;

/// UI chrome the user steers: dialogs, panels, preferences, presets,
/// proof/print settings and dialog-driven preview sessions.
///
/// Invariant: this is presentation state. Closing every dialog and dropping
/// this struct must never lose document content.
pub struct UiShell {
    pub(in crate::app) ui: UiState,
    /// Persisted Preferences (prefs.json): UI scale, default unit, autosave,
    /// snap default, AI GPU. Loaded once at startup and applied live on change.
    pub(in crate::app) settings: crate::core::settings::AppSettings,
    /// The shortcuts compiled from `settings.shortcuts`; rebuilt whenever the
    /// settings change so key handling never re-parses them.
    pub(in crate::app) keymap: crate::app::commands::KeyMap,
    pub(in crate::app) ui_data_cache: UiDataCache,
    pub(in crate::app) status_msg: String,
    /// Exit the app after the Save (Save & Exit) dialog finishes writing. Cancelled
    /// if the user cancels the dialog. Honored in poll_file_dialog.
    pub(in crate::app) exit_requested: bool,
    /// A Save chosen by the per-tab exit sweep is waiting for its file dialog.
    pub(in crate::app) exit_save_pending: bool,
    /// Close the file after the Save (Save & Close) dialog finishes (see exit_requested).
    pub(in crate::app) close_requested: bool,
    /// Where the app's self-update stood on the last frame, to tell a step
    /// that has just been taken (see `follow_update`).
    pub(in crate::app) update_seen: Option<std::mem::Discriminant<crate::update::Status>>,
    /// This is the first start of a build that replaced another: what is new
    /// in it is still to be said (see `note_build_started`).
    pub(in crate::app) updated_notice: bool,
    /// Pro on this machine: its key, its trial (see `crate::pro`).
    #[cfg(feature = "pro")]
    pub(in crate::app) pro: crate::pro::Pro,
    /// The Pro feature asked for while Pro was shut: gone through once the
    /// "Bản Pro" dialog has opened it.
    #[cfg(feature = "pro")]
    pub(in crate::app) pro_door: Option<crate::app::actions::ProDoor>,
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
    /// True only for View > Display Profile > From System. Manual profiles
    /// must never be replaced just because a window crosses monitors.
    pub(in crate::app) display_profile_from_system: bool,
    pub(in crate::app) display_profile: Option<Vec<u8>>,
    pub(in crate::app) display_profile_name: String,
    /// The Develop OS window can live on a different monitor from main, so it
    /// owns a separate monitor profile while sharing proof/document settings.
    pub(in crate::app) develop_display_profile: Option<Vec<u8>>,
    pub(in crate::app) develop_display_profile_name: String,
    /// CPU copy of the main window's display 3D LUT while it is active, so
    /// overlays drawn by egui (Clone preview) match the managed canvas.
    pub(in crate::app) display_lut: Option<std::sync::Arc<Vec<u8>>>,
    /// Page setup for File ▸ Print (persists across the dialog opening).
    pub(in crate::app) print_layout: crate::core::print::PrintLayout,
    pub(in crate::app) print_printers: Vec<crate::core::print::PrinterInfo>,
    pub(in crate::app) print_selected_printer: String,
    pub(in crate::app) print_copies: u32,
    /// Driver DEVMODE scoped to this IAI print session. It is passed directly
    /// to CreateDC and never saved as the Windows printer default.
    pub(in crate::app) print_driver_settings: Option<crate::core::print_gdi::PrinterSettings>,
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
    /// Alt held while a Levels input black (`false`) or white (`true`) point
    /// is dragged, on channel 0 (all) to 3, at a level: the preview shows what
    /// it clips (see `App::levels_clip_view`).
    pub(in crate::app) levels_clip: Option<(bool, usize, u8)>,
    pub(in crate::app) filter_preview: Option<FilterPreviewSession>,
    /// Live-preview session for the "Làm sạch bản scan" dialog.
    pub(in crate::app) scan_preview: Option<ScanPreviewSession>,
    /// Session of the "Auto retouch" dialog (analysis + live preview).
    #[cfg(feature = "pro")]
    pub(in crate::app) portrait: Option<crate::app::portrait_ops::PortraitSession>,
    /// Why the retouch asked for in the dialog could not start, for the
    /// dialog to show.
    #[cfg(feature = "pro")]
    pub(in crate::app) portrait_error: Option<String>,
    /// The last portrait analysis, kept so reopening the same photo does not
    /// run the models again.
    #[cfg(feature = "pro")]
    pub(in crate::app) portrait_cache: crate::app::portrait_ops::PortraitCache,
    /// The "Làm ảnh thẻ" job (and its wait for the background model).
    #[cfg(feature = "pro")]
    pub(in crate::app) id_photo: crate::app::id_photo_ops::IdPhotoSession,
    /// The folder of photos chosen in "Xếp ảnh in".
    #[cfg(feature = "pro")]
    pub(in crate::app) print_folder: Option<crate::app::actions::impose::PrintFolder>,
    /// AI agents on this machine may run commands in the window (Help ▸
    /// "Cho AI điều khiển iAi"); off when `None`.
    pub(in crate::app) agent_link: Option<crate::app::agent_link::AgentLink>,
    /// Why turning it on failed, for the dialog to show.
    pub(in crate::app) agent_link_error: Option<String>,
    /// The skill library as the agent dialog shows it, and when it was read.
    pub(in crate::app) agent_skills: Option<(std::time::Instant, Vec<crate::ui::SkillRow>)>,
    /// "Nhờ AI làm": errands for Gemini in Antigravity.
    pub(in crate::app) errands: crate::app::errands::Errands,
    /// "Việc nhanh": jobs iAi does on a folder by itself.
    pub(in crate::app) quick: crate::app::quick_tasks::QuickTasks,
    /// Sizes saved by the user, the Crop tool's and the New Canvas dialog's.
    pub(in crate::app) crop_presets: std::sync::Arc<Vec<crate::core::presets::SizePreset>>,
    pub(in crate::app) canvas_presets: std::sync::Arc<Vec<crate::core::presets::SizePreset>>,
    /// Named Develop slider sets saved by the user (develop_presets.json).
    pub(in crate::app) develop_presets: std::sync::Arc<Vec<crate::core::presets::DevelopPreset>>,
    /// User-saved Levels/Curves presets (adjustment_presets.json).
    pub(in crate::app) adjustment_presets: std::sync::Arc<crate::core::presets::AdjustmentPresets>,
    /// The values each kind of adjustment was last applied with, one per
    /// kind (Ctrl+Alt+L / M / U / B open with them); kept in prefs.json.
    pub(in crate::app) last_adjustments: Vec<crate::core::layer::AdjustmentType>,
}

/// Where `UiShell::last_adjustments` is kept in prefs.json.
pub(in crate::app) const LAST_ADJUSTMENTS_PREF: &str = "last_adjustments";

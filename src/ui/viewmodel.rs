//! The UI's read contract: per-feature view-models composed into [`UiData`].
//!
//! Built once per frame by `App::collect_ui_data`; every panel receives only
//! the slices it renders. UI code never sees `App` — this file and `intent.rs`
//! ARE the boundary.

use super::*;

/// The active document as the UI sees it: geometry, colour mode, view,
/// history counters, the tab strip and the PDF navigator.
pub struct DocumentViewModel {
    pub canvas_w: u32,
    pub canvas_h: u32,
    pub canvas_dpi: f32,
    pub canvas_unit: Unit,
    /// Document colour profile name for the Info panel (e.g. "sRGB IEC61966-2.1").
    pub doc_profile_name: String,
    /// Document bit depth (8 or 16) for the Info panel + Image ▸ Mode.
    pub canvas_bit_depth: u8,
    /// True when the document is in CMYK mode (Image ▸ Mode check-marks,
    /// tool/menu gating). Empty profile name when RGB.
    pub is_cmyk: bool,
    pub cmyk_profile_name: String,
    /// Whether "Embed Color Profile (ICC)" is enabled for export.
    pub export_embed_icc: bool,
    /// Named RGB/CMYK process colours stored in this document.
    pub swatches: std::sync::Arc<Vec<crate::core::palette::DocumentSwatch>>,
    pub zoom: f32,
    pub offset_x: f32,
    pub offset_y: f32,
    pub undo_count: usize,
    pub redo_count: usize,
    pub history_entries: std::sync::Arc<Vec<crate::core::command::HistoryEntry>>,
    pub current_file: Option<String>,
    pub is_modified: bool,
    pub doc_count: usize,
    pub active_doc_idx: usize,
    pub doc_titles: std::sync::Arc<Vec<String>>,
    pub doc_modified: std::sync::Arc<Vec<bool>>,
    pub doc_ai_busy: std::sync::Arc<Vec<Option<DocAiBusy>>>,
    pub has_doc: bool,
    /// Navigator strip: Some when the active document is a page of an imported PDF.
    pub pdf_nav: Option<PdfNavData>,
}
/// The layer panel: names, thumbnails, per-layer flags, grouping.
pub struct LayerViewModel {
    pub layer_count: usize,
    pub active_layer_idx: usize,
    pub layer_names: std::sync::Arc<Vec<String>>,
    pub layer_visibles: std::sync::Arc<Vec<bool>>,
    pub layer_opacities: std::sync::Arc<Vec<f32>>,
    pub layer_blend_modes: std::sync::Arc<Vec<BlendMode>>,
    pub layer_locked: std::sync::Arc<Vec<bool>>,
    pub layer_has_mask: std::sync::Arc<Vec<bool>>,
    pub layer_mask_enabled: std::sync::Arc<Vec<bool>>,
    pub layer_paint_targets: std::sync::Arc<Vec<PaintTarget>>,
    pub layer_mask_linked: std::sync::Arc<Vec<bool>>,
    pub layer_types: std::sync::Arc<Vec<String>>,
    pub layer_is_background: std::sync::Arc<Vec<bool>>,
    pub layer_lock_alpha: std::sync::Arc<Vec<bool>>,
    pub layer_selected: std::sync::Arc<Vec<bool>>,
    /// Clipped to the layer below (clipping mask / PowerClip content).
    pub layer_is_clipped: std::sync::Arc<Vec<bool>>,
    /// Base of a clipping mask (some layer clips to it) — name underlined.
    pub layer_is_clip_base: std::sync::Arc<Vec<bool>>,
    pub layer_thumbnails: std::sync::Arc<Vec<Vec<u8>>>,
    pub layer_mask_thumbnails: std::sync::Arc<Vec<Vec<u8>>>,
    /// Group nesting depth per layer (0 = top level) — panel indentation.
    pub layer_depths: std::sync::Arc<Vec<u32>>,
    /// Group folder expanded state per layer (Group layers only).
    pub layer_expanded: std::sync::Arc<Vec<bool>>,
    /// True when an ancestor folder is collapsed, so the row is hidden.
    pub layer_collapsed_hidden: std::sync::Arc<Vec<bool>>,
    /// Set for the frame after the active layer changed: the panel scrolls the
    /// active row into view (its collapsed ancestors are expanded in the same
    /// frame) so a selection made elsewhere — canvas auto-select, a layer nested
    /// in a folder — is revealed.
    pub scroll_layers_to_active: bool,
}
/// The options bar and on-canvas overlays for every tool, plus the
/// transform/text/crop session display state.
pub struct ToolViewModel {
    pub active_tool: ToolId,
    pub tool_group_preferences: std::sync::Arc<Vec<ToolId>>,
    /// A text-editing session is active → render the editing overlay.
    pub text_editing: bool,
    pub text_buffer: String,
    /// Screen-space top-left of the editing overlay.
    pub text_overlay_pos: (f32, f32),
    /// Canvas-space top-left of the editing overlay.
    pub text_overlay_origin: (i32, i32),
    pub text_font_px: f32,
    pub text_font_family: crate::core::text::TextFontFamily,
    pub text_color: [u8; 4],
    pub text_align: crate::core::text::TextAlign,
    pub text_bold: bool,
    pub text_italic: bool,
    pub text_underline: bool,
    pub text_line_height: f32,
    pub text_tracking_px: f32,
    pub text_opacity: f32,
    pub text_rotation_deg: f32,
    pub text_stretch_x: f32,
    pub text_flip_x: bool,
    pub text_flip_y: bool,
    /// Per-character colour/size of the active editing session (empty when
    /// uniform). Aligned 1:1 with `text_buffer` chars.
    pub text_glyph_styles: std::sync::Arc<Vec<crate::core::text::GlyphStyle>>,
    /// Last app-level text selection. Kept alive while toolbar/panel widgets
    /// temporarily take focus from the invisible TextEdit.
    pub text_selection: Option<std::ops::Range<usize>>,
    pub text_caret: Option<usize>,
    /// One-shot request to focus the overlay (set on session open).
    pub text_focus_pending: bool,
    /// False when no system font could be loaded (UI shows a hint).
    pub text_font_available: bool,
    /// True when the active family is installed in the egui context (fonts
    /// register on demand); false → the editing TextEdit falls back to
    /// Proportional. The canvas preview uses the font file directly either way.
    pub text_font_registered: bool,
    pub brush_size: f32,
    pub brush_hardness: f32,
    pub brush_opacity: f32,
    pub brush_spacing: f32,
    pub brush_smoothing: f32,
    pub brush_flow: f32,
    /// Active brush preset index (BRUSH_PRESETS), or PRESET_CUSTOM.
    pub brush_preset_idx: usize,
    /// Screen-space position of the right-click brush popup, None when closed.
    pub brush_popup_pos: Option<(f32, f32)>,
    pub brush_color: [u8; 4],
    /// CMYK ink split (C,M,Y,K bytes) of `brush_color` through the document's
    /// converter — `Some` only on a CMYK document (Info-panel readout).
    pub brush_ink: Option<[u8; 4]>,
    pub bg_color: [u8; 4],
    pub crop_mode: u8,
    pub crop_ratio_w: f32,
    pub crop_ratio_h: f32,
    pub crop_overlay: u8,
    pub crop_unit: u8,
    pub crop_dpi: f32,
    pub crop_w_display: f32,
    pub crop_h_display: f32,
    pub fill_tolerance: u8,
    pub fill_contiguous: bool,
    pub fill_anti_alias: bool,
    pub fill_all_layers: bool,
    /// Context-sensitive Gradient target: 0 disabled, 1 pixel, 2 vector, 3 mask.
    pub gradient_mode: u8,
    pub gradient_type: u8,
    pub gradient_opacity: f32,
    pub gradient_reverse: bool,
    pub gradient_dither: bool,
    /// The Gradient tool's editable color ramp: (position 0..1, RGBA).
    pub gradient_stops: Vec<(f32, [u8; 4])>,
    pub show_gradient_editor: bool,
    pub eyedropper_sample: u8,
    pub eyedropper_sample_merged: bool,
    /// Most recently sampled canvas colour, exposed as a Corel-style status-bar swatch.
    pub eyedropper_picked_color: Option<[u8; 4]>,
    /// Session history of sampled colours for the bottom Corel-style palette.
    pub eyedropper_picked_colors: Vec<[u8; 4]>,
    pub move_auto_select: bool,
    pub move_show_transform: bool,
    pub clone_size: f32,
    pub clone_hardness: f32,
    pub clone_opacity: f32,
    pub clone_spacing: f32,
    pub clone_aligned: bool,
    pub clone_sample_merged: bool,
    /// Repair Brush only: content-aware healing type vs. proximity match.
    pub clone_smart_fill: bool,
    /// RGBA preview of the current Clone source patch, including brush softness.
    pub clone_source_thumbnail: Option<std::sync::Arc<CloneSourcePreview>>,
    // Smudge tool.
    pub smudge_size: f32,
    pub smudge_hardness: f32,
    pub smudge_strength: f32,
    pub smudge_finger_painting: bool,
    // Dodge / Burn tools (shared options; the active one is shown).
    pub dodge_size: f32,
    pub dodge_hardness: f32,
    pub dodge_exposure: f32,
    /// Tonal range targeted (0 = Shadows, 1 = Midtones, 2 = Highlights).
    pub dodge_range: u8,
    pub dodge_protect_tones: bool,
    // Patch tool: 0 = Source, 1 = Smart.
    pub patch_mode: u8,
    pub transform_interpolation: crate::core::geometry::InterpolationMode,
    pub shape_kind: u8,
    pub shape_fill: bool,
    pub shape_fill_color: [u8; 4],
    pub shape_stroke_width: f32,
    pub shape_stroke_color: [u8; 4],
    pub shape_corner_radius: f32,
    /// Rectangle corner style: 0 Round, 1 Scallop, 2 Chamfer.
    pub shape_corner_type: u8,
    /// Polygon edge count / Star point count.
    pub shape_sides: u32,
    /// Star inner-radius fraction.
    pub shape_star_inner: f32,
    pub shape_preview: Option<[f32; 4]>,
    /// On-canvas editing overlay for the active Shape layer (Shape tool):
    /// bounding-box span + handle positions in canvas space.
    pub shape_overlay: Option<ShapeOverlay>,
    /// On-canvas editing overlay for the active Path layer (Node tool): outline
    /// + anchor points + selected-node handle arms.
    pub node_overlay: Option<super::NodeOverlay>,
    /// Transform handles for the active Path's vector gradient fill.
    pub path_gradient_overlay: Option<super::PathGradientOverlay>,
    /// Zoom-bucketed raster for the active Path. Display-only; document tiles
    /// and export continue to use the normal document-resolution cache.
    pub path_display: Option<super::PathDisplayRaster>,
    /// Fill/Outline of the active Path layer for the options-bar controls
    /// (Move / Node). `None` when the active layer isn't an editable Path.
    pub path_style: Option<super::PathStyleData>,
    /// Pen tool: commit mode (0=Selection, 1=Fill, 2=Stroke) + stroke width.
    pub pen_mode: u8,
    pub pen_stroke_width: f32,
    /// Vector Brush (Phase 6B) options-bar state.
    pub vector_brush_width: f32,
    pub vector_brush_color: [u8; 4],
    pub vector_brush_smoothing: f32,
    pub vector_brush_pressure: bool,
    pub vector_brush_velocity: bool,
    /// Live preview polyline of the in-progress Vector Brush stroke (canvas space).
    pub vector_brush_path: Vec<(f32, f32)>,
    /// True when the active layer is a Vector Brush stroke (enables Expand).
    pub vector_brush_can_expand: bool,
    pub arrow_width: f32,
    pub arrow_end: u8,
    pub arrow_route: u8,
    pub arrow_path: Vec<(f32, f32)>,
    /// Gradient tool drag guide: [start_x, start_y, end_x, end_y] in canvas space.
    pub gradient_preview: Option<[f32; 4]>,
    pub wand_brush_size: f32,
    pub wand_tolerance: u8,
    pub wand_edge_sensitivity: u8,
    #[allow(dead_code)]
    pub wand_contiguous: bool,
    #[allow(dead_code)]
    pub wand_anti_alias: bool,
    pub wand_sample_merged: bool,
    pub crop_rect: Option<[f32; 4]>,
    pub crop_rotation: f32,
    /// Perspective Crop quad corners [TL, TR, BR, BL] in canvas space.
    pub persp_crop_quad: Option<[(f32, f32); 4]>,
    /// Perspective Crop corner-picking preview: the corners placed so far with the
    /// live cursor appended as the tentative next corner. Empty unless picking.
    pub persp_crop_preview: Vec<(f32, f32)>,
    /// Perspective Crop output size shown in the options bar, in `persp_unit`
    /// (manual override or auto-derived from the quad).
    pub persp_w_display: f32,
    pub persp_h_display: f32,
    /// Options-bar unit index (0=px,1=cm,2=mm,3=in,4=pt,5=pc,6=%) and output DPI.
    pub persp_unit: u8,
    pub persp_dpi: f32,
    /// True when an explicit output size has been typed (vs auto from the quad).
    pub persp_size_manual: bool,
    /// True once the four corners are placed (enables the confirm/cancel buttons).
    pub persp_has_quad: bool,
    /// Pen tool: flattened path polyline (canvas space), last point = live cursor.
    pub pen_path: Vec<(f32, f32)>,
    /// Pen tool: anchor points (for drawing the square anchor handles).
    pub pen_anchors: Vec<(f32, f32)>,
    /// Pen tool: Bézier handle arms `[ax, ay, hx, hy]` (canvas space).
    pub pen_handles: Vec<[f32; 4]>,
    pub pen_closed: bool,
    pub crop_cursor_hint: u8,
    pub transform_overlay: Option<TransformOverlayData>,
    pub transform_scale_x: f32,
    pub transform_scale_y: f32,
    pub transform_angle: f32,
    pub transform_tx: f32,
    pub transform_ty: f32,
    pub transform_cursor_hint: u8,
    pub transform_ctx_menu_pos: Option<(f32, f32)>,
    pub selected_rect_corner_type: Option<u8>,
}
/// Selection: mode, previews, Select Subject and the Refine panel.
pub struct SelectionViewModel {
    pub selection_mode: crate::core::selection::SelectionMode,
    pub lasso_preview: Vec<(f32, f32)>,
    pub rect_sel_preview: Option<[f32; 4]>,
    pub ellipse_sel_preview: Option<[f32; 4]>,
    pub has_selection: bool,
    pub select_subject_busy: bool,
    pub select_subject_status_msg: String,
    pub select_subject_model: crate::core::select_subject::SelectSubjectModel,
    pub show_refine_panel: bool,
    pub refine_feather: f32,
    pub refine_smooth: u32,
    pub refine_smart_radius: f32,
    pub refine_shift_edge: f32,
    pub refine_contrast: f32,
    pub refine_decontaminate: bool,
    pub refine_decontaminate_amount: f32,
    pub refine_brush_size: f32,
    pub refine_brush_hardness: f32,
    pub refine_brush_mode: crate::core::selection::RefineBrushMode,
    pub refine_view_mode: crate::ui::refine_select::RefineViewMode,
    pub refine_overlay_color: [u8; 4],
    pub show_refine_color_dialog: bool,
    pub refine_color_dialog_color: [u8; 4],
    pub refine_color_dialog_original: [u8; 4],
    pub refine_color_dialog_live_preview: bool,
    pub refine_color_dialog_center_next: bool,
    pub refine_output_mode: crate::ui::refine_select::RefineOutputMode,
    /// Per-pixel overlay texture (Overlay view mode). None when not in Overlay mode or not built yet.
    pub refine_overlay_tex: Option<egui::TextureId>,
    pub selection_ctx_menu_pos: Option<(f32, f32)>,
}
/// The Develop dialog/window: settings, histogram, EXIF, local masks.
pub struct DevelopViewModel {
    pub show_develop_dialog: bool,
    /// True while the Develop stage lives in its own OS window (Track D) — the
    /// in-canvas Develop dialog is suppressed so the UI is not shown twice.
    pub develop_in_window: bool,
    pub develop_settings: DevelopSettings,
    /// R/G/B/Luma histogram of the develop source — curve editor backdrop.
    pub develop_histogram: Option<std::sync::Arc<[[f32; 256]; 4]>>,
    /// True while the Develop panel is acting as the RAW Develop stage (slice
    /// R2): the panel is titled "Develop" and commit/cancel mean open/discard.
    pub develop_mode: bool,
    /// Open/closed state of the panel sections, in panel order (persisted).
    pub develop_sections_open: [bool; develop::DEV_PANEL_SECTIONS],
    /// EXIF line for the active RAW ("ISO … · f/… · …"); hidden when None.
    pub develop_exif: Option<String>,
    /// Display R/G/B under the Develop-window cursor (histogram readout).
    pub develop_readout: Option<[u8; 3]>,
    /// Auto (exposure fit) works on scene-referred sessions only.
    pub develop_auto_available: bool,
    pub develop_presets: std::sync::Arc<Vec<crate::core::presets::DevelopPreset>>,
    /// Develop local masks: which row is selected, which placement is armed,
    /// and the selected mask's screen-space outline.
    pub develop_local_selected: Option<usize>,
    pub develop_local_arm: Option<crate::core::develop::LocalMaskKind>,
    pub develop_local_overlay: Option<DevelopLocalOverlay>,
}
/// Print and proof/colour management.
pub struct PrintViewModel {
    /// Soft-proof (View ▸ Proof) display state, for menu check-marks.
    pub proof_enabled: bool,
    pub proof_gamut_warn: bool,
    pub proof_target_label: String,
    /// Display colour management (monitor profile) state, for the View menu.
    pub display_cms_enabled: bool,
    pub display_profile_name: String,
    /// File ▸ Print dialog state + page setup.
    pub show_print_dialog: bool,
    pub print_layout: crate::core::print::PrintLayout,
    pub print_printers: std::sync::Arc<Vec<crate::core::print::PrinterInfo>>,
    pub print_selected_printer: String,
    pub print_copies: u32,
    pub print_refreshing: bool,
    pub print_preview_image: Option<std::sync::Arc<egui::ColorImage>>,
    /// Loaded printer ICC profile name for app-managed print colour ("" = printer
    /// manages colour).
    pub print_printer_profile_name: String,
}
/// Modal dialogs: which is open, plus their scratch inputs.
pub struct DialogViewModel {
    /// Ink split of the paint dialog's live colour (picker readout).
    pub paint_dialog_ink: Option<[u8; 4]>,
    pub show_new_dialog: bool,
    pub show_resize_dialog: bool,
    pub show_image_size_dialog: bool,
    pub show_rename_dialog: bool,
    pub show_export_dialog: bool,
    pub show_preferences: bool,
    pub show_adjustment_dialog: bool,
    pub adjustment_dialog: AdjustmentType,
    pub adjustment_preview_enabled: bool,
    pub adjustment_options: AdjustmentOptions,
    pub adj_eyedropper: Option<AdjEyedropperKind>,
    pub levels_histogram: std::sync::Arc<[[u32; 256]; 4]>,
    pub show_warp_dialog: bool,
    pub warp_params: crate::core::warp::WarpParams,
    pub warp_freeze: Option<std::sync::Arc<WarpFreezeView>>,
    /// True while Alt+right-dragging to resize the Warp brush. When set, the
    /// brush ring is pinned at `warp_resize_anchor` (egui points) instead of
    /// tracking the cursor.
    pub warp_resizing: bool,
    pub warp_resize_anchor: (f32, f32),
    pub show_filter_dialog: bool,
    pub filter_dialog: FilterType,
    pub filter_proxy_original: Option<std::sync::Arc<egui::ColorImage>>,
    pub filter_proxy_preview: Option<std::sync::Arc<egui::ColorImage>>,
    pub filter_preview_processing: bool,
    pub filter_preview_enabled: bool,
    pub show_smart_fill_dialog: bool,
    /// AI inpainting model state (for the Smart Fill dialog).
    pub lama_available: bool,
    pub lama_status_msg: String,
    pub show_exit_dialog: bool,
    pub show_close_dialog: bool,
    pub show_reload_file_dialog: bool,
    pub reload_file_name: String,
    pub reload_will_discard_changes: bool,
    // PDF page-selection dialog (see app/file_ops.rs).
    pub show_pdf_import_dialog: bool,
    /// Image ▸ Mode ▸ CMYK Color… convert dialog + its choices.
    pub show_cmyk_convert_dialog: bool,
    pub cmyk_convert_use_icc: bool,
    /// Display name of the browsed `.icc` (None = nothing picked yet).
    pub cmyk_convert_icc_name: Option<String>,
    pub pdf_import_file_name: String,
    /// Full path (lossy) — a stable key for the dialog's per-PDF egui temp state.
    pub pdf_import_path_key: String,
    pub pdf_import_page_count: usize,
    /// Per-page size in points, for the dialog's page list.
    pub pdf_import_page_dims: Vec<(f32, f32)>,
    pub show_feather_dialog: bool,
    pub show_modify_dialog: Option<SelectionModifyKind>,
    pub show_stroke_dialog: bool,
    /// Editable values in `new_unit`, before conversion to integer pixels.
    pub new_w_input: f32,
    pub new_h_input: f32,
    pub new_dpi: f32,
    pub new_bg_color: u8,
    pub new_name: String,
    pub new_unit: Unit,
    pub rename_idx: usize,
    pub rename_text: String,
    pub export_format: ExportFormat,
    pub show_paint_color_dialog: bool,
    pub paint_color_dialog_target: u8,
    pub paint_color_dialog_color: [u8; 4],
    pub paint_color_dialog_original: [u8; 4],
    pub paint_color_dialog_live_preview: bool,
    pub paint_color_dialog_center_next: bool,
    pub user_presets: std::sync::Arc<Vec<crate::core::presets::SizePreset>>,
    pub adjustment_presets: std::sync::Arc<crate::core::presets::AdjustmentPresets>,
    pub show_preset_dialog: bool,
    pub show_delete_preset_dialog: bool,
    pub preset_dialog_name: String,
    pub preset_dialog_w: f32,
    pub preset_dialog_h: f32,
    pub preset_dialog_unit: String,
    pub preset_dialog_dpi: f32,
}
/// Window chrome: panels, toolbox, theme, rulers/guides/snap, status.
pub struct ChromeViewModel {
    pub status_msg: String,
    pub show_welcome: bool,
    /// The Library grid browser is showing instead of the editor (Track B).
    pub show_library: bool,
    pub theme_mode: theme::ThemeMode,
    pub show_color_panel: bool,
    pub show_text_panel: bool,
    pub show_layer_panel: bool,
    pub show_history_panel: bool,
    pub show_info_panel: bool,
    pub show_channels_panel: bool,
    pub show_rulers: bool,
    pub show_guides: bool,
    pub lock_guides: bool,
    pub snap_enabled: bool,
    /// Committed guides of the active document (canvas-space).
    pub guides: Vec<crate::core::document::Guide>,
    /// In-progress guide being dragged (orientation + canvas pos), drawn as a preview.
    pub guide_preview: Option<(crate::core::document::GuideOrientation, f32)>,
    /// Index of the guide under the cursor / being dragged — drawn highlighted.
    pub hovered_guide: Option<usize>,
    /// Smart-guide lines from the Move tool's snapping (magenta = align, red = page edge).
    pub snap_guides: Vec<crate::core::snapping::SnapLine>,
    pub toolbar_w: f32,
    pub panel_r_w: f32,
    /// Strict modal lock is active (Crop/Free Transform/Text editing/dialog):
    /// the layer panel, menus and tab bar are disabled until commit/cancel.
    pub is_tool_modal: bool,
    /// Blink the active modal's Commit/Cancel controls (a blocked exit attempt
    /// is asking the user to finish or cancel the operation).
    pub modal_flash: bool,
}
/// The Channels panel.
pub struct ChannelsViewModel {
    /// Channels panel rows: write mask + viewed channel of the active document.
    pub channels_write_mask: [bool; 4],
    pub channel_view: crate::core::channels::ChannelView,
    /// Saved alpha channels of the active document: (id, name).
    pub alpha_channels: std::sync::Arc<Vec<(u32, String)>>,
    pub active_alpha_channel: Option<usize>,
    /// [composite, R, G, B] panel-sized RGBA plates, then one thumbnail per alpha
    /// channel in `alpha_thumbnails`. May be empty (stale pixels / huge doc).
    pub channel_thumbnails: std::sync::Arc<Vec<Vec<u8>>>,
    pub alpha_thumbnails: std::sync::Arc<Vec<Vec<u8>>>,
}
/// The AI panel and extension-bridge status.
pub struct AiViewModel {
    // AI Gemini panel snapshot (see ui/ai_panel.rs).
    pub show_ai_panel: bool,
    pub ai: crate::core::ai::AiPanelState,
    pub ai_status: String,
    pub ai_history: Vec<String>,
    // Browser-extension bridge (see app/ext_bridge.rs).
    pub ext_connected: bool,
    pub ext_queue_len: usize,
    pub ext_status: String,
    pub ext_log: Vec<String>,
    pub ext_token: String,
}

/// One recent-files entry for the welcome screen (Track B): its path, display
/// name, pixel size, and thumbnail texture once the cache has produced it.
pub struct RecentThumb {
    pub path: std::path::PathBuf,
    pub name: String,
    pub dims: (u32, u32),
    pub thumb: Option<(egui::TextureId, egui::Vec2)>,
}

/// The welcome screen's view model: the recent-files list.
#[derive(Default)]
pub struct WelcomeViewModel {
    pub recent: Vec<RecentThumb>,
}

/// One scanned file in the Library grid (Track B): path, display name, whether
/// it is selected, and its thumbnail texture once the cache has produced it.
pub struct LibraryEntry {
    pub path: std::path::PathBuf,
    pub name: String,
    pub selected: bool,
    pub thumb: Option<(egui::TextureId, egui::Vec2)>,
}

/// The Library grid browser's view model (Track B): the chosen folder, the
/// images found in it, and how many are selected.
#[derive(Default)]
pub struct LibraryViewModel {
    /// Display path of the chosen folder (None until one is picked).
    pub folder: Option<String>,
    pub entries: Vec<LibraryEntry>,
    pub selected_count: usize,
}

pub struct UiData {
    /// The active document as the UI sees it: geometry, colour mode, view,
    pub doc: DocumentViewModel,
    /// The layer panel: names, thumbnails, per-layer flags, grouping.
    pub layers: LayerViewModel,
    /// The options bar and on-canvas overlays for every tool, plus the
    pub tool: ToolViewModel,
    /// Selection: mode, previews, Select Subject and the Refine panel.
    pub sel: SelectionViewModel,
    /// The Develop dialog/window: settings, histogram, EXIF, local masks.
    pub develop: DevelopViewModel,
    /// Print and proof/colour management.
    pub print: PrintViewModel,
    /// Modal dialogs: which is open, plus their scratch inputs.
    pub dialogs: DialogViewModel,
    /// Window chrome: panels, toolbox, theme, rulers/guides/snap, status.
    pub chrome: ChromeViewModel,
    /// The Channels panel.
    pub channels: ChannelsViewModel,
    /// The AI panel and extension-bridge status.
    pub ai: AiViewModel,
    /// The welcome screen: recent files and their thumbnails.
    pub welcome: WelcomeViewModel,
    /// The Library grid browser: the chosen folder and its images.
    pub library: LibraryViewModel,
}

impl Default for UiData {
    fn default() -> Self {
        Self {
            doc: DocumentViewModel {
                canvas_w: 800,
                canvas_h: 600,
                canvas_dpi: 72.0,
                canvas_unit: Unit::Pixels,
                doc_profile_name: String::new(),
                canvas_bit_depth: 8,
                is_cmyk: false,
                cmyk_profile_name: String::new(),
                export_embed_icc: true,
                swatches: std::sync::Arc::new(Vec::new()),
                zoom: 1.0,
                offset_x: 0.0,
                offset_y: 0.0,
                undo_count: 0,
                redo_count: 0,
                history_entries: std::sync::Arc::new(Vec::new()),
                current_file: None,
                is_modified: false,
                doc_count: 1,
                active_doc_idx: 0,
                doc_titles: std::sync::Arc::new(vec!["Untitled".to_string()]),
                doc_modified: std::sync::Arc::new(vec![false]),
                doc_ai_busy: std::sync::Arc::new(vec![None]),
                has_doc: false,
                pdf_nav: None,
            },
            layers: LayerViewModel {
                layer_count: 1,
                active_layer_idx: 0,
                layer_names: std::sync::Arc::new(vec!["Background".to_string()]),
                layer_visibles: std::sync::Arc::new(vec![true]),
                layer_opacities: std::sync::Arc::new(vec![1.0]),
                layer_blend_modes: std::sync::Arc::new(vec![BlendMode::Normal]),
                layer_locked: std::sync::Arc::new(vec![false]),
                layer_has_mask: std::sync::Arc::new(vec![false]),
                layer_mask_enabled: std::sync::Arc::new(vec![true]),
                layer_paint_targets: std::sync::Arc::new(vec![PaintTarget::Pixels]),
                layer_mask_linked: std::sync::Arc::new(vec![true]),
                layer_types: std::sync::Arc::new(vec!["Raster".to_string()]),
                layer_is_background: std::sync::Arc::new(vec![true]),
                layer_lock_alpha: std::sync::Arc::new(vec![false]),
                layer_selected: std::sync::Arc::new(vec![true]),
                layer_is_clipped: std::sync::Arc::new(vec![false]),
                layer_is_clip_base: std::sync::Arc::new(vec![false]),
                layer_thumbnails: std::sync::Arc::new(Vec::new()),
                layer_mask_thumbnails: std::sync::Arc::new(Vec::new()),
                layer_depths: std::sync::Arc::new(vec![0]),
                layer_expanded: std::sync::Arc::new(vec![true]),
                layer_collapsed_hidden: std::sync::Arc::new(vec![false]),
                scroll_layers_to_active: false,
            },
            tool: ToolViewModel {
                active_tool: ToolId::Move,
                tool_group_preferences: std::sync::Arc::new(Vec::new()),
                text_editing: false,
                text_buffer: String::new(),
                text_overlay_pos: (0.0, 0.0),
                text_overlay_origin: (0, 0),
                text_font_px: 48.0,
                text_font_family: crate::core::text::TextFontFamily::SegoeUi,
                text_color: [0, 0, 0, 255],
                text_align: crate::core::text::TextAlign::Left,
                text_bold: false,
                text_italic: false,
                text_underline: false,
                text_line_height: 1.2,
                text_tracking_px: 0.0,
                text_opacity: 1.0,
                text_rotation_deg: 0.0,
                text_stretch_x: 1.0,
                text_flip_x: false,
                text_flip_y: false,
                text_glyph_styles: std::sync::Arc::new(Vec::new()),
                text_selection: None,
                text_caret: None,
                text_focus_pending: false,
                text_font_available: true,
                text_font_registered: false,
                brush_size: 20.0,
                brush_hardness: 0.8,
                brush_opacity: 1.0,
                brush_spacing: 0.25,
                brush_smoothing: 0.0,
                brush_flow: 1.0,
                brush_preset_idx: usize::MAX,
                brush_popup_pos: None,
                brush_color: [0, 0, 0, 255],
                brush_ink: None,
                bg_color: [255, 255, 255, 255],
                crop_mode: 0,
                crop_ratio_w: 1.0,
                crop_ratio_h: 1.0,
                crop_overlay: 0,
                crop_unit: 0,
                crop_dpi: 72.0,
                crop_w_display: 0.0,
                crop_h_display: 0.0,
                fill_tolerance: 32,
                fill_contiguous: true,
                fill_anti_alias: true,
                fill_all_layers: false,
                gradient_mode: 1,
                gradient_type: 0,
                gradient_opacity: 1.0,
                gradient_reverse: false,
                gradient_dither: true,
                gradient_stops: vec![(0.0, [0, 0, 0, 255]), (1.0, [255, 255, 255, 255])],
                show_gradient_editor: false,
                eyedropper_sample: 0,
                eyedropper_sample_merged: true,
                eyedropper_picked_color: None,
                eyedropper_picked_colors: Vec::new(),
                move_auto_select: false,
                move_show_transform: false,
                clone_size: 30.0,
                clone_hardness: 0.0,
                clone_opacity: 1.0,
                clone_spacing: 0.25,
                clone_aligned: true,
                clone_sample_merged: false,
                clone_smart_fill: false,
                clone_source_thumbnail: None,
                smudge_size: 40.0,
                smudge_hardness: 0.0,
                smudge_strength: 0.5,
                smudge_finger_painting: false,
                dodge_size: 60.0,
                dodge_hardness: 0.0,
                dodge_exposure: 0.5,
                dodge_range: 1,
                dodge_protect_tones: true,
                patch_mode: 0,
                transform_interpolation: crate::core::geometry::InterpolationMode::Bilinear,
                shape_kind: 0,
                shape_fill: true,
                shape_fill_color: [0, 0, 0, 255],
                shape_stroke_width: 2.0,
                shape_stroke_color: [0, 0, 0, 255],
                shape_corner_radius: 0.0,
                shape_corner_type: 0,
                shape_sides: 5,
                shape_star_inner: 0.5,
                shape_preview: None,
                shape_overlay: None,
                node_overlay: None,
                path_gradient_overlay: None,
                path_display: None,
                path_style: None,
                pen_mode: 0,
                pen_stroke_width: 3.0,
                vector_brush_width: 12.0,
                vector_brush_color: [0, 0, 0, 255],
                vector_brush_smoothing: 0.4,
                vector_brush_pressure: true,
                vector_brush_velocity: true,
                vector_brush_path: Vec::new(),
                vector_brush_can_expand: false,
                arrow_width: 3.0,
                arrow_end: 1,
                arrow_route: 0,
                arrow_path: Vec::new(),
                gradient_preview: None,
                wand_brush_size: 30.0,
                wand_tolerance: 32,
                wand_edge_sensitivity: 65,
                wand_contiguous: true,
                wand_anti_alias: true,
                wand_sample_merged: true,
                crop_rect: None,
                crop_rotation: 0.0,
                persp_crop_quad: None,
                persp_crop_preview: Vec::new(),
                persp_w_display: 0.0,
                persp_h_display: 0.0,
                persp_unit: 0,
                persp_dpi: 300.0,
                persp_size_manual: false,
                persp_has_quad: false,
                pen_path: Vec::new(),
                pen_anchors: Vec::new(),
                pen_handles: Vec::new(),
                pen_closed: false,
                crop_cursor_hint: 0,
                transform_overlay: None,
                transform_scale_x: 1.0,
                transform_scale_y: 1.0,
                transform_angle: 0.0,
                transform_tx: 0.0,
                transform_ty: 0.0,
                transform_cursor_hint: 0,
                transform_ctx_menu_pos: None,
                selected_rect_corner_type: None,
            },
            sel: SelectionViewModel {
                selection_mode: crate::core::selection::SelectionMode::New,
                lasso_preview: Vec::new(),
                rect_sel_preview: None,
                ellipse_sel_preview: None,
                has_selection: false,
                select_subject_busy: false,
                select_subject_status_msg: String::new(),
                select_subject_model: crate::core::select_subject::SelectSubjectModel::Rmbg14,
                show_refine_panel: false,
                refine_feather: 0.0,
                refine_smooth: 0,
                refine_smart_radius: 0.0,
                refine_shift_edge: 0.0,
                refine_contrast: 0.0,
                refine_decontaminate: false,
                refine_decontaminate_amount: 0.5,
                refine_brush_size: 40.0,
                refine_brush_hardness: 0.5,
                refine_brush_mode: crate::core::selection::RefineBrushMode::Smart,
                refine_view_mode: crate::ui::refine_select::RefineViewMode::Overlay,
                refine_overlay_color: [210, 30, 30, 190],
                show_refine_color_dialog: false,
                refine_color_dialog_color: [210, 30, 30, 190],
                refine_color_dialog_original: [210, 30, 30, 190],
                refine_color_dialog_live_preview: true,
                refine_color_dialog_center_next: false,
                refine_output_mode: crate::ui::refine_select::RefineOutputMode::Selection,
                refine_overlay_tex: None,
                selection_ctx_menu_pos: None,
            },
            develop: DevelopViewModel {
                show_develop_dialog: false,
                develop_in_window: false,
                develop_settings: DevelopSettings::default(),
                develop_histogram: None,
                develop_mode: false,
                develop_sections_open: develop::DEFAULT_SECTIONS_OPEN,
                develop_exif: None,
                develop_readout: None,
                develop_auto_available: false,
                develop_presets: std::sync::Arc::new(Vec::new()),
                develop_local_selected: None,
                develop_local_arm: None,
                develop_local_overlay: None,
            },
            print: PrintViewModel {
                proof_enabled: false,
                proof_gamut_warn: false,
                proof_target_label: String::new(),
                display_cms_enabled: false,
                display_profile_name: String::new(),
                show_print_dialog: false,
                print_layout: crate::core::print::PrintLayout::default(),
                print_printers: std::sync::Arc::new(Vec::new()),
                print_selected_printer: String::new(),
                print_copies: 1,
                print_refreshing: false,
                print_preview_image: None,
                print_printer_profile_name: String::new(),
            },
            dialogs: DialogViewModel {
                paint_dialog_ink: None,
                show_new_dialog: false,
                show_resize_dialog: false,
                show_image_size_dialog: false,
                show_rename_dialog: false,
                show_export_dialog: false,
                show_preferences: false,
                show_adjustment_dialog: false,
                adjustment_dialog: AdjustmentType::default_levels(),
                adjustment_preview_enabled: true,
                adjustment_options: AdjustmentOptions::default(),
                adj_eyedropper: None,
                levels_histogram: std::sync::Arc::new([[0; 256]; 4]),
                show_warp_dialog: false,
                warp_params: crate::core::warp::WarpParams::default(),
                warp_resizing: false,
                warp_resize_anchor: (0.0, 0.0),
                warp_freeze: None,
                show_filter_dialog: false,
                filter_dialog: FilterType::GaussianBlur { radius: 2.0 },
                filter_proxy_original: None,
                filter_proxy_preview: None,
                filter_preview_processing: false,
                filter_preview_enabled: true,
                show_smart_fill_dialog: false,
                lama_available: false,
                lama_status_msg: String::new(),
                show_exit_dialog: false,
                show_close_dialog: false,
                show_reload_file_dialog: false,
                reload_file_name: String::new(),
                reload_will_discard_changes: false,
                show_pdf_import_dialog: false,
                show_cmyk_convert_dialog: false,
                cmyk_convert_use_icc: false,
                cmyk_convert_icc_name: None,
                pdf_import_file_name: String::new(),
                pdf_import_path_key: String::new(),
                pdf_import_page_count: 0,
                pdf_import_page_dims: Vec::new(),
                show_feather_dialog: false,
                show_modify_dialog: None,
                show_stroke_dialog: false,
                new_w_input: 800.0,
                new_h_input: 600.0,
                new_dpi: 72.0,
                new_bg_color: 0,
                new_name: "Untitled".to_string(),
                new_unit: crate::core::units::Unit::Pixels,
                rename_idx: 0,
                rename_text: String::new(),
                export_format: ExportFormat::Png { compression: 6 },
                show_paint_color_dialog: false,
                paint_color_dialog_target: 0,
                paint_color_dialog_color: [0, 0, 0, 255],
                paint_color_dialog_original: [0, 0, 0, 255],
                paint_color_dialog_live_preview: true,
                paint_color_dialog_center_next: false,
                user_presets: std::sync::Arc::new(Vec::new()),
                adjustment_presets: std::sync::Arc::new(Default::default()),
                show_preset_dialog: false,
                show_delete_preset_dialog: false,
                preset_dialog_name: String::new(),
                preset_dialog_w: 0.0,
                preset_dialog_h: 0.0,
                preset_dialog_unit: "px".to_string(),
                preset_dialog_dpi: 72.0,
            },
            chrome: ChromeViewModel {
                status_msg: String::new(),
                show_welcome: true,
                show_library: false,
                theme_mode: theme::ThemeMode::Dark,
                show_color_panel: false,
                show_text_panel: false,
                show_layer_panel: true,
                show_history_panel: false,
                show_info_panel: false,
                show_channels_panel: false,
                show_rulers: true,
                show_guides: true,
                lock_guides: false,
                snap_enabled: true,
                guides: Vec::new(),
                guide_preview: None,
                hovered_guide: None,
                snap_guides: Vec::new(),
                toolbar_w: 48.0,
                panel_r_w: 300.0,
                is_tool_modal: false,
                modal_flash: false,
            },
            channels: ChannelsViewModel {
                channels_write_mask: [true; 4],
                channel_view: crate::core::channels::ChannelView::Composite,
                alpha_channels: std::sync::Arc::new(Vec::new()),
                active_alpha_channel: None,
                channel_thumbnails: std::sync::Arc::new(Vec::new()),
                alpha_thumbnails: std::sync::Arc::new(Vec::new()),
            },
            ai: AiViewModel {
                show_ai_panel: false,
                ai: crate::core::ai::AiPanelState::default(),
                ai_status: String::new(),
                ai_history: Vec::new(),
                ext_connected: false,
                ext_queue_len: 0,
                ext_status: String::new(),
                ext_log: Vec::new(),
                ext_token: String::new(),
            },
            welcome: WelcomeViewModel::default(),
            library: LibraryViewModel::default(),
        }
    }
}

//! The UI's write contract: per-feature intents composed into [`UiActions`].
//!
//! Panels record what the user asked for; the app consumes and applies them
//! through its handlers (and document mutations go through the canvas
//! gateway). UI code never mutates `App` state directly.

use super::*;

/// File/document commands: open/save/export, undo/redo, resize, tabs,
/// colour-mode conversion, view zoom, clipboard.
#[derive(Default)]
pub struct DocumentIntent {
    pub fit_to_screen: bool,
    pub zoom_in: bool,
    pub zoom_out: bool,
    pub zoom_100: bool,
    pub flip_horizontal: bool,
    pub flip_vertical: bool,
    pub rotate_cw: bool,
    pub rotate_ccw: bool,
    pub copy: bool,
    pub cut: bool,
    pub paste: bool,
    /// Edit → Fill: flood the active selection (or whole layer) with the foreground color.
    pub fill_foreground: bool,
    pub fill_background: bool,
    pub new_canvas: bool,
    pub open_file: bool,
    /// Welcome screen: open this recent file (Track B library).
    pub open_recent: Option<std::path::PathBuf>,
    pub save: bool,
    pub save_as: bool,
    /// Save from an exit/close confirmation. Multi-layer documents default to
    /// an `.iai` project; a single-layer image keeps its original format.
    pub save_project: bool,
    pub export: Option<ExportFormat>,
    pub exit: bool,
    pub exit_save_current: bool,
    pub exit_discard_current: bool,
    pub exit_cancel: bool,
    pub undo: bool,
    pub redo: bool,
    pub jump_history: Option<usize>,
    pub canvas_resize: Option<(u32, u32, i32, i32)>,
    pub image_resize: Option<(u32, u32, f32)>,
    /// "Xếp ảnh in": duplicate the active photo onto a print sheet
    /// (paper, photo kind, gap in px at sheet DPI).
    pub impose_sheet: Option<(
        crate::core::imposition::Paper,
        crate::core::imposition::PhotoKind,
        u32,
    )>,
    pub set_canvas_unit: Option<Unit>,
    /// Page ▸ Bleed: set the default page bleed of the active document, in mm
    /// (one undoable step). Drawn by the artboard sheet overlay.
    pub set_page_bleed_mm: Option<f32>,
    /// Page ▸ Safe margin: set the default page safe-margin, in mm.
    pub set_page_margin_mm: Option<f32>,
    pub reload_open_file_confirm: bool,
    pub reload_open_file_cancel: bool,
    /// PDF page-selection dialog: confirm with the 0-based page indices to open
    /// and the chosen import DPI (`None` = Auto).
    pub pdf_import_confirm: Option<(Vec<usize>, Option<f32>)>,
    pub pdf_import_cancel: bool,
    /// PDF navigator strip: jump to this page index within the active PDF group.
    pub pdf_nav_goto: Option<usize>,
    /// PDF navigator strip: export the active PDF group as a multi-page PDF.
    pub pdf_nav_export: bool,
    /// New canvas: `(name, w, h, dpi, background, unit, cmyk)`. `cmyk` starts the
    /// document in CMYK (Generic naive) instead of RGB.
    pub new_canvas_confirmed: Option<(String, u32, u32, f32, u8, Unit, bool)>,
    pub rename_confirmed: Option<(usize, String)>,
    pub export_confirmed: Option<(ExportFormat, String)>,
    /// Request the Save dialog (rfd) to pick an export path. MUST be handled in
    /// apply_ui_actions (outside the egui frame) — calling rfd in-frame deadlocks.
    pub request_export_path: Option<ExportFormat>,
    /// Toggle "Embed Color Profile (ICC)" for export.
    pub set_export_embed_icc: Option<bool>,
    /// Export output-sizing/sharpening controls.
    pub set_export_resize_enabled: Option<bool>,
    pub set_export_resize_long_edge: Option<u32>,
    pub set_export_output_sharpen: Option<u8>,
    /// PDF export press marks (crop / registration / bleed).
    pub set_export_pdf_marks: Option<crate::core::print::PrintMarks>,
    /// Image ▸ Assign Profile: re-interpret the document as this profile and bake
    /// into the sRGB working space.
    pub assign_profile: Option<crate::core::cms::WorkingProfile>,
    /// Image ▸ Color Profile ▸ Convert to Profile: transform pixels into this profile.
    pub convert_profile: Option<crate::core::cms::WorkingProfile>,
    /// Image ▸ Mode ▸ Grayscale: desaturate the whole document to luminance.
    pub convert_grayscale: bool,
    /// Image ▸ Mode ▸ CMYK Color…: open/close the convert dialog.
    pub show_cmyk_convert_dialog: Option<bool>,
    /// CMYK convert dialog: browse for a `.icc` (deferred rfd pick).
    pub browse_cmyk_icc: bool,
    /// CMYK convert dialog: radio choice (true = ICC, false = naive).
    pub set_cmyk_convert_use_icc: Option<bool>,
    /// CMYK convert dialog OK: run the conversion with the dialog's choice.
    pub convert_cmyk: bool,
    /// Image ▸ Mode ▸ RGB Color (only meaningful on a CMYK document): drop the
    /// ink planes and return to RGB editing.
    pub convert_rgb_mode: bool,
    /// Image ▸ Mode: set bit depth (true = 16 bits/channel, false = 8).
    pub set_bit_depth: Option<bool>,
    pub new_w_input: Option<f32>,
    pub new_h_input: Option<f32>,
    pub new_dpi: Option<f32>,
    pub new_bg_color: Option<u8>,
    pub new_name: Option<String>,
    pub new_unit: Option<Unit>,
    pub rename_text: Option<String>,
    pub close_file_without_saving: Option<bool>,
    pub switch_doc: Option<usize>,
    pub close_doc_tab: Option<usize>,
    pub new_doc_tab: bool,
}
/// Layer panel commands.
#[derive(Default)]
pub struct LayerIntent {
    pub align_layers: Option<LayerAlign>,
    pub distribute_layers: Option<LayerDistribute>,
    pub duplicate_selected_step: bool,
    pub repeat_duplicate_step: bool,
    pub add_layer: bool,
    pub add_adjustment_layer: Option<AdjustmentType>,
    pub duplicate_layer: Option<usize>,
    pub layer_via_copy: bool,
    pub remove_layer: Option<usize>,
    /// Bake a vector Path layer (this index) down to a plain raster layer.
    pub rasterize_layer: Option<usize>,
    /// Convert the Shape layer (this index) into an editable vector Path layer.
    pub convert_to_curves: Option<usize>,
    /// Convert the Text layer (this index) into editable vector Path layer(s).
    pub text_to_curves: Option<usize>,
    /// Weld/Trim/Intersect/Simplify the selected vector layers.
    pub boolean_op: Option<crate::core::vector::boolean::BooleanOp>,
    /// PowerClip: place the selected content inside the selected vector frame.
    pub powerclip_place: bool,
    /// PowerClip: extract the selected content from its frame.
    pub powerclip_release: bool,
    /// Clipping mask (Ctrl+Alt+G): clip the active layer to the one below, or
    /// release it. Photoshop-style; shares the PowerClip clip engine.
    pub toggle_clipping_mask: bool,
    pub select_layer: Option<(usize, bool, bool)>,
    /// Ctrl+click a layer's thumbnail → load that layer's alpha as a selection.
    pub load_layer_selection: Option<usize>,
    pub toggle_visible: Option<usize>,
    pub toggle_lock: Option<usize>,
    pub toggle_lock_alpha: Option<usize>,
    pub unlock_background_layer: Option<usize>,
    pub set_opacity: Option<(usize, f32)>,
    /// True on the frame the user RELEASES the mouse (or types a value) on the opacity
    /// slider → push a single undo entry for the whole drag, not one per frame.
    pub set_opacity_commit: bool,
    pub set_blend_mode: Option<(usize, BlendMode)>,
    pub move_layer_up: Option<usize>,
    pub move_layer_down: Option<usize>,
    pub move_layer_to: Option<(usize, usize)>,
    pub merge_down: Option<usize>,
    pub merge_selected: bool,
    pub merge_all: bool,
    /// Merge a folder (header at this index) into one raster layer.
    pub merge_group: Option<usize>,
    /// Merge Visible (Ctrl+Shift+E): flatten eye-on layers into one.
    pub merge_visible: bool,
    /// Group the selected (or active) layers into a new folder (Ctrl+G).
    pub group_selected: bool,
    /// Ungroup the folder at this index (Ctrl+Shift+G when active is a group).
    pub ungroup: Option<usize>,
    /// Toggle a group folder's expanded/collapsed state (panel triangle).
    pub toggle_group_expanded: Option<usize>,
    pub add_mask_white: Option<usize>,
    pub add_mask_black: Option<usize>,
    pub delete_mask: Option<usize>,
    pub apply_mask: Option<usize>,
    pub toggle_mask_enabled: Option<usize>,
    pub toggle_mask_link: Option<usize>,
    /// Invert (Ctrl+I): the mask if editing a mask, otherwise the layer's pixels.
    pub invert_active: Option<usize>,
    pub set_paint_target: Option<(usize, PaintTarget)>,
}
/// Tool selection, options-bar edits, transform/crop/text session commands.
#[derive(Default)]
pub struct ToolIntent {
    pub select_tool: Option<ToolId>,
    /// Align the multi-selected Path nodes (Node options bar): the axis to snap on
    /// and which reference coordinate to snap to.
    pub node_align: Option<(
        crate::core::vector::ops::Axis,
        crate::core::vector::ops::AlignRef,
    )>,
    /// Break the active Path at the primary selected node (Node options bar).
    pub node_break: bool,
    /// Join the two selected endpoints — close or weld (Node options bar).
    pub node_join: bool,
    /// New buffer contents from the editing overlay.
    pub text_buffer: Option<String>,
    pub text_cursor: Option<(Option<std::ops::Range<usize>>, Option<usize>)>,
    pub text_commit: bool,
    pub text_cancel: bool,
    pub set_text_font_px: Option<f32>,
    pub set_text_font_family: Option<crate::core::text::TextFontFamily>,
    pub preview_text_font_family: Option<crate::core::text::TextFontFamily>,
    pub cancel_text_font_family_preview: bool,
    pub set_text_color: Option<[u8; 4]>,
    pub set_text_align: Option<crate::core::text::TextAlign>,
    /// Text panel case buttons (discoverable Shift+F3): recase the selection
    /// (or whole buffer) to lowercase / UPPERCASE / Title Case.
    pub set_text_case: Option<crate::core::text::TextCase>,
    pub set_text_bold: Option<bool>,
    pub set_text_italic: Option<bool>,
    pub set_text_underline: Option<bool>,
    pub set_text_line_height: Option<f32>,
    pub set_text_tracking_px: Option<f32>,
    pub set_text_opacity: Option<f32>,
    /// Double-click a text layer in the Layers panel → re-edit.
    pub edit_text_layer: Option<usize>,
    /// Canvas click outside the text being edited. Under the strict modal
    /// lock this is refused (bell) — the session ends only via Commit/Cancel.
    pub text_canvas_click: Option<(f32, f32)>,
    /// Drag the active text overlay to a new canvas-space origin.
    pub text_move_origin: Option<(i32, i32)>,
    /// UI acknowledged the focus request → app clears its one-shot flag.
    pub consume_text_focus: bool,
    pub text_drag_handle_hovered: bool,
    pub text_panel_hovered: bool,
    pub brush_size: Option<f32>,
    pub brush_hardness: Option<f32>,
    pub brush_opacity: Option<f32>,
    pub brush_spacing: Option<f32>,
    pub brush_smoothing: Option<f32>,
    pub brush_flow: Option<f32>,
    /// Apply a built-in brush preset by index into BRUSH_PRESETS.
    pub select_brush_preset: Option<usize>,
    /// Close the right-click brush popup.
    pub brush_popup_close: bool,
    pub brush_color: Option<[u8; 4]>,
    pub bg_color: Option<[u8; 4]>,
    pub swap_colors: bool,
    pub set_crop_mode: Option<u8>,
    pub set_crop_ratio_w: Option<f32>,
    pub set_crop_ratio_h: Option<f32>,
    pub set_crop_overlay: Option<u8>,
    pub set_crop_unit: Option<u8>,
    pub set_crop_dpi: Option<f32>,
    /// Exact source values typed in the currently selected Crop unit.
    pub set_crop_w_value: Option<f32>,
    pub set_crop_h_value: Option<f32>,
    pub set_crop_w_px: Option<f32>,
    pub set_crop_h_px: Option<f32>,
    pub set_transform_interpolation: Option<crate::core::geometry::InterpolationMode>,
    pub crop_confirm: bool,
    pub crop_cancel: bool,
    /// Exact Perspective Crop source values in its selected unit.
    /// Setting either marks the output size as manual.
    pub set_persp_out_w: Option<f32>,
    pub set_persp_out_h: Option<f32>,
    /// Revert the Perspective Crop output size to auto (derive from the quad).
    pub clear_persp_size: bool,
    /// Perspective Crop options-bar unit index / output DPI.
    pub set_persp_unit: Option<u8>,
    pub set_persp_dpi: Option<f32>,
    pub swap_crop_wh: bool,
    pub set_wand_brush_size: Option<f32>,
    pub set_wand_tolerance: Option<u8>,
    pub set_wand_edge_sensitivity: Option<u8>,
    pub set_wand_contiguous: Option<bool>,
    pub set_wand_anti_alias: Option<bool>,
    pub set_wand_sample_merged: Option<bool>,
    pub set_fill_tolerance: Option<u8>,
    pub set_fill_contiguous: Option<bool>,
    pub set_fill_anti_alias: Option<bool>,
    pub set_fill_all_layers: Option<bool>,
    pub set_gradient_type: Option<u8>,
    pub set_gradient_opacity: Option<f32>,
    pub set_gradient_reverse: Option<bool>,
    pub set_gradient_dither: Option<bool>,
    /// Replace the Gradient tool's color ramp (from the gradient editor).
    pub set_gradient_stops: Option<Vec<(f32, [u8; 4])>>,
    pub toggle_gradient_editor: Option<bool>,
    pub set_eyedropper_sample: Option<u8>,
    pub set_eyedropper_sample_merged: Option<bool>,
    pub set_move_auto_select: Option<bool>,
    pub set_move_show_transform: Option<bool>,
    pub move_transform: Option<MoveTransformAction>,
    pub set_clone_size: Option<f32>,
    pub set_clone_hardness: Option<f32>,
    pub set_clone_opacity: Option<f32>,
    pub set_clone_spacing: Option<f32>,
    /// Apply a soft/hard round tip preset to Clone / Repair.
    pub set_clone_preset: Option<usize>,
    pub set_clone_aligned: Option<bool>,
    pub set_clone_sample_merged: Option<bool>,
    pub set_clone_smart_fill: Option<bool>,
    pub set_smudge_size: Option<f32>,
    pub set_smudge_hardness: Option<f32>,
    pub set_smudge_strength: Option<f32>,
    pub set_smudge_finger_painting: Option<bool>,
    /// Apply a [`BRUSH_PRESETS`] tip (Soft/Hard round …) to the Smudge tool.
    pub set_smudge_preset: Option<usize>,
    pub set_dodge_size: Option<f32>,
    pub set_dodge_hardness: Option<f32>,
    pub set_dodge_exposure: Option<f32>,
    pub set_dodge_range: Option<u8>,
    pub set_dodge_protect_tones: Option<bool>,
    /// Apply a [`BRUSH_PRESETS`] tip (Soft/Hard round …) to the active Dodge/Burn tool.
    pub set_dodge_preset: Option<usize>,
    pub set_patch_mode: Option<u8>,
    pub set_pen_mode: Option<u8>,
    pub set_pen_stroke_width: Option<f32>,
    /// Vector Brush (Phase 6B) options.
    pub set_vector_brush_width: Option<f32>,
    pub set_vector_brush_smoothing: Option<f32>,
    pub set_vector_brush_pressure: Option<bool>,
    pub set_vector_brush_velocity: Option<bool>,
    pub set_arrow_width: Option<f32>,
    pub set_arrow_end: Option<u8>,
    pub set_arrow_route: Option<u8>,
    /// Arrow tool mode: 0 single, 1 branch, 2 tree (org-chart).
    pub set_arrow_mode: Option<u8>,
    /// Number of down-arrows generated in tree mode.
    pub set_tree_count: Option<u8>,
    /// Bake the active Vector Brush stroke into a closed outline.
    pub expand_vector_brush: bool,
    pub set_shape_kind: Option<u8>,
    pub set_shape_fill: Option<bool>,
    pub set_shape_stroke_width: Option<f32>,
    pub set_shape_corner_radius: Option<f32>,
    pub set_shape_corner_type: Option<u8>,
    /// Polygon edge count / Star point count.
    pub set_shape_sides: Option<u32>,
    /// Star inner-radius fraction.
    pub set_shape_star_inner: Option<f32>,
    /// Path layer Fill/Outline (options bar under Move / Node).
    pub set_path_fill_enabled: Option<bool>,
    pub set_path_stroke_enabled: Option<bool>,
    pub set_path_fill_overprint: Option<bool>,
    pub set_path_stroke_overprint: Option<bool>,
    /// Corel-like palette gesture: primary click applies Fill, secondary click
    /// applies Outline while preserving an exact RGB/CMYK process value.
    pub apply_palette_fill: Option<crate::core::vector::color::ColorValue>,
    pub apply_palette_outline: Option<crate::core::vector::color::ColorValue>,
    pub clear_palette_fill: bool,
    pub clear_palette_outline: bool,
    pub add_document_swatch: Option<crate::core::vector::color::ColorValue>,
    /// Create a spot-ink swatch: `(plate name, base colour used as the process
    /// alternate)`.
    pub add_spot_swatch: Option<(String, crate::core::vector::color::ColorValue)>,
    pub rename_document_swatch: Option<(usize, String)>,
    pub remove_document_swatch: Option<usize>,
    /// Live preview of the outline width; `commit_path_style` finalises the scrub.
    pub set_path_stroke_width: Option<f32>,
    pub set_path_fill_kind: Option<u8>,
    pub set_path_gradient_stop_offset: Option<(u8, f32)>,
    pub add_path_gradient_stop: bool,
    pub remove_path_gradient_stop: Option<u8>,
    pub set_path_dash_kind: Option<u8>,
    /// Outline end cap (0 Butt, 1 Round, 2 Square) and corner join (0 Miter, 1
    /// Round, 2 Bevel). Each is a discrete selection recorded as one undo step.
    pub set_path_cap: Option<u8>,
    pub set_path_join: Option<u8>,
    pub set_path_arrow_start: Option<u8>,
    pub set_path_arrow_end: Option<u8>,
    pub set_path_arrow_size: Option<f32>,
    /// Live preview of the custom dash array and phase.
    pub set_path_dash_values: Option<([f32; crate::core::vector::style::MAX_DASHES], u8)>,
    pub set_path_dash_offset: Option<f32>,
    /// End of an interactive Path style edit (width scrub / colour dialog) — record
    /// the single undo step.
    pub commit_path_style: bool,
    pub transform_commit: bool,
    pub transform_cancel: bool,
    pub set_transform_scale_x: Option<f32>,
    pub set_transform_scale_y: Option<f32>,
    pub set_transform_angle: Option<f32>,
    pub set_transform_tx: Option<f32>,
    pub set_transform_ty: Option<f32>,
    pub transform_flip_h: bool,
    pub transform_flip_v: bool,
    pub transform_rot_90cw: bool,
    pub transform_rot_90ccw: bool,
    pub transform_rot_180: bool,
    /// Restore the pending Free Transform without leaving the session.
    pub transform_reset: bool,
    pub set_transform_mode: Option<crate::app::state::TransformMode>,
    pub transform_warp: bool,
    pub transform_ctx_menu_close: bool,
    /// Close the Move-tool selected-object context menu.
    pub object_ctx_menu_close: bool,
    /// Begin Free Transform (Ctrl+T) — fired from the selection context menu.
    pub start_transform: bool,
}
/// Selection commands: mode, modify, Select Subject, Refine.
#[derive(Default)]
pub struct SelectionIntent {
    pub clear_selection_pixels: bool,
    /// Select menu actions (functionality already exists via keyboard shortcuts).
    pub select_all: bool,
    pub deselect: bool,
    pub invert_selection: bool,
    pub show_feather_dialog: Option<bool>,
    /// Open (Some(true)) / close (Some(false)) the Stroke Selection dialog.
    pub show_stroke_dialog: Option<bool>,
    /// Apply a stroke to the active selection with these params.
    pub apply_stroke: Option<StrokeParams>,
    pub set_selection_mode: Option<crate::core::selection::SelectionMode>,
    pub selection_feather: Option<f32>,
    pub selection_grow: Option<u32>,
    pub selection_shrink: Option<u32>,
    pub selection_smooth: Option<f32>,
    pub selection_border: Option<u32>,
    /// Select ▸ Modify: open the shared modify dialog for this operation.
    pub open_modify_dialog: Option<SelectionModifyKind>,
    /// Close the Select ▸ Modify dialog.
    pub close_modify_dialog: bool,
    pub trigger_select_subject: bool,
    pub set_select_subject_model: Option<crate::core::select_subject::SelectSubjectModel>,
    pub open_refine_panel: bool,
    pub set_refine_feather: Option<f32>,
    pub set_refine_smooth: Option<u32>,
    pub set_refine_smart_radius: Option<f32>,
    pub set_refine_shift_edge: Option<f32>,
    pub set_refine_contrast: Option<f32>,
    pub set_refine_decontaminate: Option<bool>,
    pub set_refine_decontaminate_amount: Option<f32>,
    /// Fire apply_refine_preview() — only set on drag-release or keyboard commit, not while dragging.
    pub trigger_refine_apply: bool,
    pub refine_apply: bool,
    pub refine_cancel: bool,
    pub set_refine_brush_size: Option<f32>,
    pub set_refine_brush_hardness: Option<f32>,
    pub set_refine_brush_mode: Option<crate::core::selection::RefineBrushMode>,
    pub set_refine_view_mode: Option<crate::ui::refine_select::RefineViewMode>,
    pub set_refine_overlay_color: Option<[u8; 4]>,
    pub open_refine_color_dialog: bool,
    pub set_refine_color_dialog_color: Option<[u8; 4]>,
    pub set_refine_color_dialog_live_preview: Option<bool>,
    pub refine_color_dialog_default: bool,
    pub refine_color_dialog_ok: bool,
    pub refine_color_dialog_cancel: bool,
    pub refine_color_dialog_centered: bool,
    pub set_refine_output_mode: Option<crate::ui::refine_select::RefineOutputMode>,
    /// Close the selection-tool right-click context menu.
    pub selection_ctx_menu_close: bool,
}
/// Develop commands: scrub, apply/cancel, presets, local masks, Auto.
#[derive(Default)]
pub struct DevelopIntent {
    pub open_develop_dialog: bool,
    pub set_develop_settings: Option<DevelopSettings>,
    /// True while the primary pointer is held over an interactive Develop
    /// control. Full-resolution refinement waits for release.
    pub develop_controls_pointer_down: bool,
    pub apply_develop_dialog: bool,
    pub cancel_develop_dialog: bool,
    /// Save the current Develop settings under this name (same name overwrites).
    pub save_develop_preset: Option<String>,
    /// Delete the Develop preset at this index.
    pub delete_develop_preset: Option<usize>,
    /// Arm local-mask placement: the next canvas drag places this kind, into
    /// `locals[target]` when re-placing (None appends a new mask).
    pub arm_develop_local: Option<(crate::core::develop::LocalMaskKind, Option<usize>)>,
    pub disarm_develop_local: bool,
    /// Select (Some(idx)) or deselect (None) a local mask row in the panel.
    pub select_develop_local: Option<Option<usize>>,
    /// A section header was expanded/collapsed — persist to prefs.json.
    pub set_develop_section_open: Option<(usize, bool)>,
    /// View-only scope visibility bitmask (waveform/parade/vectorscope).
    pub set_develop_scope_visibility: Option<u8>,
    /// "Auto" clicked: fit Exposure to the fixed target brightness (D4 v1).
    pub develop_auto: bool,
}
/// Print/proof commands.
#[derive(Default)]
pub struct PrintIntent {
    /// View ▸ Proof Colors toggle.
    pub toggle_proof_colors: bool,
    /// View ▸ Gamut Warning toggle.
    pub toggle_gamut_warning: bool,
    /// View ▸ Proof Setup: pick a built-in proof target.
    pub set_proof_target: Option<crate::core::cms::ProofTarget>,
    /// View ▸ Proof Setup ▸ Load Profile… (deferred rfd file pick).
    pub load_proof_profile: bool,
    /// View ▸ Display Profile: turn display CMS off.
    pub display_cms_off: bool,
    /// View ▸ Display Profile: use the OS monitor profile (auto-detect).
    pub display_cms_from_system: bool,
    /// View ▸ Display Profile: load a monitor .icc (deferred rfd file pick).
    pub display_cms_load: bool,
    /// File ▸ Print dialog: open/close.
    pub show_print_dialog: Option<bool>,
    /// Updated page setup from the Print dialog.
    pub set_print_layout: Option<crate::core::print::PrintLayout>,
    pub set_print_printer: Option<String>,
    pub set_print_copies: Option<u32>,
    pub refresh_printers: bool,
    pub open_printer_settings: bool,
    /// Print ▸ Color Handling: load an RGB printer ICC for app-managed conversion.
    pub load_print_printer_profile: bool,
    /// Print ▸ Color Handling: back to "Printer Manages Colors".
    pub clear_print_printer_profile: bool,
    /// File ▸ Export ▸ CMYK Separations… (pick CMYK ICC + base path, write plates).
    pub export_cmyk_separations: bool,
    /// File ▸ Export ▸ PDF (multi-page)… — write all open tabs as one PDF.
    pub export_multipage_pdf: bool,
    /// File ▸ Export ▸ SVG (Vector)… — write the document as an SVG.
    pub export_svg: bool,
    /// Print dialog: send the current page to the default printer.
    pub print_send: bool,
    /// Print dialog: save the print-ready PDF (deferred rfd save).
    pub print_save_pdf: bool,
}
/// A batch text-formatting request from the "Định dạng chữ hàng loạt" dialog.
/// Every field is optional — `None` means "leave this attribute unchanged".
/// Applied across every text layer in the active document as one undo step.
#[derive(Clone, Default)]
pub struct TextBatchFormat {
    /// Scope of the font swap: `None` = all fonts, `Some(storage_name)` = only
    /// text currently using that font. Relevant only when `set_font` is set.
    pub from_font: Option<String>,
    pub set_font: Option<crate::core::text::TextFontFamily>,
    pub set_color: Option<[u8; 4]>,
    pub set_size: Option<f32>,
    pub set_bold: Option<bool>,
    pub set_italic: Option<bool>,
    pub set_underline: Option<bool>,
    pub set_case: Option<crate::core::text::TextCase>,
}

impl TextBatchFormat {
    /// True when nothing was chosen to change (Apply is then a no-op).
    pub fn is_noop(&self) -> bool {
        self.set_font.is_none()
            && self.set_color.is_none()
            && self.set_size.is_none()
            && self.set_bold.is_none()
            && self.set_italic.is_none()
            && self.set_underline.is_none()
            && self.set_case.is_none()
    }
}

/// Which layers a vector batch-style operation may change.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum VectorStyleTarget {
    #[default]
    Document,
    Selected,
}

/// Batch style change for vector layers. Scope flags classify each vector
/// layer independently; optional style fields leave the existing value alone.
#[derive(Clone, Copy, Default)]
pub struct VectorBatchStyle {
    pub target: VectorStyleTarget,
    pub include_arrows: bool,
    pub include_shapes: bool,
    pub include_curves: bool,
    pub set_fill: Option<[u8; 4]>,
    pub set_stroke: Option<[u8; 4]>,
    pub set_stroke_width: Option<f32>,
}

impl VectorBatchStyle {
    pub fn is_noop(&self) -> bool {
        (!self.include_arrows && !self.include_shapes && !self.include_curves)
            || (self.set_fill.is_none()
                && self.set_stroke.is_none()
                && self.set_stroke_width.is_none())
    }
}

/// Open/confirm/cancel for the modal dialogs and their scratch edits.
#[derive(Default)]
pub struct DialogIntent {
    pub open_paint_color_dialog: Option<u8>,
    /// Open the paint-colour dialog for `target` pre-seeded with a specific
    /// colour (the palette right-click "Adjust colour…" gesture). `original`
    /// stays the target's current colour so Cancel restores it.
    pub open_paint_color_dialog_with: Option<(u8, [u8; 4])>,
    pub set_paint_color_dialog_color: Option<[u8; 4]>,
    pub set_paint_color_dialog_live_preview: Option<bool>,
    pub paint_color_dialog_default: bool,
    pub paint_color_dialog_ok: bool,
    pub paint_color_dialog_cancel: bool,
    pub paint_color_dialog_centered: bool,
    /// True when the pointer is over the floating paint-colour dialog window
    /// (so the app can shrink the brush ring cursor to a small circle there).
    pub paint_dialog_hovered: bool,
    pub open_adjustment_dialog: Option<AdjustmentType>,
    pub apply_direct_adjustment: Option<AdjustmentType>,
    pub auto_levels: bool,
    /// Double-click an adjustment layer (idx) → open the params editor.
    pub edit_adjustment_layer: Option<usize>,
    pub set_adjustment_dialog: Option<AdjustmentType>,
    pub set_adjustment_preview_enabled: Option<bool>,
    pub set_adjustment_options: Option<AdjustmentOptions>,
    pub set_adj_eyedropper: Option<Option<AdjEyedropperKind>>,
    pub apply_adjustment_dialog: bool,
    pub cancel_adjustment_dialog: bool,
    pub open_warp_dialog: bool,
    pub set_warp_params: Option<crate::core::warp::WarpParams>,
    pub apply_warp_dialog: bool,
    pub cancel_warp_dialog: bool,
    pub warp_restore_all: bool,
    pub open_filter_dialog: Option<FilterType>,
    /// Update the filter params (cheap — keeps the slider value live) WITHOUT
    /// recomputing the (expensive) blur preview.
    pub set_filter_dialog: Option<FilterType>,
    /// Recompute the live filter preview now (debounced: only on slider release /
    /// click / Reset, not on every drag tick).
    pub apply_filter_preview: bool,
    pub set_filter_preview_enabled: Option<bool>,
    pub apply_filter_dialog: bool,
    pub cancel_filter_dialog: bool,
    /// Edit → Smart Fill: synthesise pixels inside the active selection.
    pub smart_fill_fill: bool,
    /// Smart Fill dialog → Apply with the chosen method (Some(use_ai)).
    pub apply_smart_fill_fill: Option<bool>,
    pub cancel_smart_fill_dialog: bool,
    /// Smart Fill dialog → download the AI inpainting model.
    pub download_lama_model: bool,
    pub show_new_dialog: Option<bool>,
    /// Open/close the batch "Định dạng chữ hàng loạt" dialog.
    pub show_font_change_dialog: Option<bool>,
    /// Apply a batch text-formatting change to the active document's text layers.
    pub apply_text_format: Option<TextBatchFormat>,
    /// Open/close Object ▸ Format All Vectors….
    pub show_vector_style_dialog: Option<bool>,
    /// Open the vector-style dialog for the whole document or selected layers.
    pub open_vector_style_dialog: Option<VectorStyleTarget>,
    /// Apply one batch style change to the selected vector-layer classes.
    pub apply_vector_style: Option<VectorBatchStyle>,
    /// Open/close the "Làm sạch bản scan" dialog.
    pub show_scan_cleanup_dialog: Option<bool>,
    /// Live-preview params for the open scan-cleanup dialog (current page only).
    pub set_scan_cleanup_preview: Option<crate::core::scan_cleanup::ScanCleanupParams>,
    /// Cancel/close the scan-cleanup dialog (restore the previewed layer).
    pub cancel_scan_cleanup_dialog: bool,
    /// Apply a scan-cleanup pass to the current page/image or a page range.
    pub apply_scan_cleanup: Option<crate::core::scan_cleanup::ScanCleanupRequest>,
    pub show_resize_dialog: Option<bool>,
    pub show_image_size_dialog: Option<bool>,
    pub show_rename_dialog: Option<(bool, usize)>,
    pub show_export_dialog: Option<bool>,
    pub show_preferences: Option<bool>,
    pub show_adjustment_dialog: Option<bool>,
    pub show_exit_dialog: Option<bool>,
    pub show_close_dialog: Option<bool>,
    pub save_preset: Option<(String, f32, f32, String, f32)>,
    pub delete_preset: Option<usize>,
    pub open_delete_preset_dialog: bool,
    pub close_delete_preset_dialog: bool,
    pub open_preset_dialog: Option<(f32, f32, String, f32)>,
    pub preset_dialog_name_changed: Option<String>,
    pub preset_dialog_confirm: bool,
    pub preset_dialog_cancel: bool,
    /// Save/delete user Levels/Curves presets (adjustment_presets.json).
    pub save_levels_preset: Option<(String, [crate::core::layer::LevelsParams; 4])>,
    pub delete_levels_preset: Option<usize>,
    pub save_curves_preset: Option<(String, [Vec<(f32, f32)>; 4])>,
    pub delete_curves_preset: Option<usize>,
}
/// Chrome: panel toggles, toolbox, theme, guides, window buttons.
#[derive(Default)]
pub struct ChromeIntent {
    /// A click landed on UI that is disabled by the strict modal lock (tab
    /// bar, menus, …) — the app rings the bell and flashes Commit/Cancel.
    pub modal_denied: bool,
    pub set_theme_mode: Option<theme::ThemeMode>,
    pub show_welcome: Option<bool>,
    /// Toggle the Library grid browser (Track B). `true` also leaves the welcome
    /// screen; `false` returns to the editor.
    pub show_library: Option<bool>,
    pub toggle_color_panel: bool,
    pub toggle_text_panel: bool,
    pub toggle_layer_panel: bool,
    pub show_layer_panel: Option<bool>,
    pub toggle_history_panel: bool,
    pub toggle_info_panel: bool,
    pub toggle_channels_panel: bool,
    pub show_channels_panel: Option<bool>,
    pub toggle_rulers: bool,
    /// Dragging a guide out of a ruler: (orientation, canvas pos under pointer).
    /// Set every frame the ruler drag is active.
    pub ruler_guide_drag: Option<(crate::core::document::GuideOrientation, f32)>,
    /// The ruler guide drag ended this frame — commit the in-progress guide.
    pub ruler_guide_commit: bool,
    pub toggle_show_guides: bool,
    pub toggle_lock_guides: bool,
    pub toggle_snap: bool,
    pub clear_guides: bool,
    pub show_text_panel: Option<bool>,
    pub cursor_left: bool,
    pub cursor_entered: bool,
    pub window_minimize: bool,
    pub window_maximize_toggle: bool,
    pub window_drag: bool,
}
/// How a Library grid click changes the selection.
#[derive(Clone, Copy, PartialEq)]
pub enum LibrarySelect {
    /// Plain click — select only this entry (and set it as the range anchor).
    Replace,
    /// Ctrl/Cmd click — toggle this entry in the selection (and re-anchor here).
    Toggle,
    /// Shift click — select the contiguous range from the anchor to this entry.
    Range,
}

/// Library grid commands (Track B): folder browse, selection, open.
#[derive(Default)]
pub struct LibraryIntent {
    /// Header "Choose Folder" — open the deferred folder picker.
    pub open_folder: bool,
    /// Click a grid card: (path, how the click changes the selection).
    pub select_entry: Option<(std::path::PathBuf, LibrarySelect)>,
    /// Double-click a card — open that single file into the editor.
    pub open_entry: Option<std::path::PathBuf>,
    /// "Open Selected" — open every selected file into the editor.
    pub open_selected: bool,
    /// Clear the current selection.
    pub clear_selection: bool,
    /// Paths whose cards are visible in the scroll viewport this frame — the app
    /// requests thumbnails only for these, so a large folder never floods the
    /// generator or thrashes the resident-texture cache.
    pub visible_thumbs: Vec<std::path::PathBuf>,
}
/// Channels panel commands.
#[derive(Default)]
pub struct ChannelsIntent {
    /// Channels panel row click: target view + additive (Ctrl/Shift held).
    pub select_channel_row: Option<(crate::core::channels::ChannelView, bool)>,
    /// Channels panel alpha ops (all undoable except load-as-selection,
    /// which pushes a SelectionCommand).
    pub save_selection_as_channel: bool,
    pub load_channel_as_selection: Option<(usize, crate::core::selection::MaskCombine)>,
    pub new_alpha_channel: bool,
    pub delete_alpha_channel: Option<usize>,
    pub rename_alpha_channel: Option<(usize, String)>,
    pub duplicate_alpha_channel: Option<usize>,
}
/// AI panel commands and extension-bridge runs.
#[derive(Default)]
pub struct AiIntent {
    // AI Gemini panel (see ui/ai_panel.rs).
    /// Write the edited panel buffers back to UiState.
    pub set_ai_panel: Option<crate::core::ai::AiPanelState>,
    /// Open (Some(true)) / close (Some(false)) the Gemini panel.
    pub toggle_ai_panel: Option<bool>,
    /// Save the API key currently in the panel to disk.
    pub ai_save_key: bool,
    /// Run a Gemini edit with this final prompt.
    pub ai_run: Option<String>,
    /// Run an edit via the browser extension (site comes from the selected source).
    pub ext_run: Option<String>,
    /// Cancel whichever API/bridge job belongs to the active document.
    pub ai_cancel_active: bool,
}

#[derive(Default)]
pub struct UiActions {
    /// File/document commands: open/save/export, undo/redo, resize, tabs,
    pub doc: DocumentIntent,
    /// Layer panel commands.
    pub layers: LayerIntent,
    /// Tool selection, options-bar edits, transform/crop/text session commands.
    pub tool: ToolIntent,
    /// Selection commands: mode, modify, Select Subject, Refine.
    pub sel: SelectionIntent,
    /// Develop commands: scrub, apply/cancel, presets, local masks, Auto.
    pub develop: DevelopIntent,
    /// Print/proof commands.
    pub print: PrintIntent,
    /// Open/confirm/cancel for the modal dialogs and their scratch edits.
    pub dialogs: DialogIntent,
    /// Chrome: panel toggles, toolbox, theme, guides, window buttons.
    pub chrome: ChromeIntent,
    /// Library grid commands: folder browse, selection, open.
    pub library: LibraryIntent,
    /// Channels panel commands.
    pub channels: ChannelsIntent,
    /// AI panel commands and extension-bridge runs.
    pub ai: AiIntent,
}

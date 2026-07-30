//! One of the six App subsystems — see `state.rs` for the tree.

use super::state::*;

/// Live editing state: input, view, tools and every in-flight session.
///
/// Owns the pointer/keyboard state, zoom/pan, the tool manager, colours,
/// clipboard, and the modal editing sessions (transform, text, shape, warp,
/// refine). Invariant: in gateway terms everything here is PreviewTransient
/// or SessionState — none of it is serialized, and cancelling any session
/// must leave documents untouched (commits go through the canvas gateway).
pub struct EditorInteraction {
    pub(in crate::app) tools: ToolManager,
    pub(in crate::app) fg_color: [u8; 4],
    pub(in crate::app) bg_color: [u8; 4],
    pub(in crate::app) view: ViewState,
    pub(in crate::app) input: InputState,
    pub(in crate::app) selection_mode: crate::core::selection::SelectionMode,
    pub(in crate::app) pending_stroke_inputs: Vec<PointerEvent>,
    pub(in crate::app) pending_fill: Option<[u8; 4]>,
    /// Deferred selection-stroke request (Edit ▸ Stroke), applied after the egui
    /// frame like `pending_fill`. None when nothing pending.
    pub(in crate::app) pending_stroke: Option<crate::core::canvas::StrokeParams>,
    /// In-progress guide drag (create from ruler, or move an existing guide).
    pub(in crate::app) guide_op: Option<GuideOp>,
    /// Smart-guide lines from Free Transform snapping (④); drawn like the Move
    /// tool's. Cleared when no transform handle is being dragged.
    pub(in crate::app) transform_snap_guides: Vec<crate::core::snapping::SnapLine>,
    pub(in crate::app) pending_transform_commit:
        Option<std::sync::mpsc::Receiver<Result<TransformCommitResult, String>>>,
    /// Continue into mesh Warp after an asynchronous transform bake.
    pub(in crate::app) warp_after_transform_commit: bool,
    /// Internal layer clipboard: whole cloned `Layer`s (preserving offset, opacity,
    /// blend, mask, visibility, and group structure via `parent_id`). Copying a group
    /// header stores its whole subtree; paste remaps ids so the folder + children
    /// come across intact.
    pub(in crate::app) clipboard: Option<Vec<crate::core::layer::Layer>>,
    /// Size of an OS clipboard image that populated the New Canvas dialog. Paste
    /// uses this as a short-lived hint so the follow-up Ctrl+V prefers the copied
    /// external image over any older internal layer clipboard.
    pub(in crate::app) clipboard_image_new_doc_hint: Option<(u32, u32)>,
    /// Content hash of the image iai itself last wrote to the OS clipboard (on
    /// copy, or when sending an edit to the browser extension). Paste compares
    /// the OS clipboard against this: a mismatch means another app copied
    /// something newer, which then wins over the internal layer clipboard.
    pub(in crate::app) os_clipboard_written: Option<u64>,
    /// Structure command captured when an opacity-slider drag starts; pushed once when
    /// the drag ends so the whole drag is one undo entry (avoids history spam and
    /// cloning the full layer stack every frame).
    pub(in crate::app) pending_opacity_cmd: Option<crate::core::command::LayerStructureCommand>,
    /// Active free-transform session (Ctrl+T … Enter/Esc).
    /// None when no transform is in progress.
    pub(in crate::app) transform_state: Option<TransformState>,
    pub(in crate::app) adjustment_layer_edit: Option<AdjustmentLayerEditState>,
    /// Active Warp session (Ctrl+Shift+X). None when not warping.
    pub(in crate::app) warp_state: Option<WarpState>,
    /// Active text-editing session (Text tool). See [`TextEditState`].
    pub(in crate::app) text_edit: Option<TextEditState>,
    /// Active Shape-handle drag (Shape tool). See [`ShapeDragState`].
    pub(in crate::app) shape_drag: Option<ShapeDragState>,
    /// Active on-canvas Path transform (scale/rotate) under the Move tool. See
    /// [`PathTransformDrag`]. None unless a handle is being dragged.
    pub(in crate::app) path_transform: Option<PathTransformDrag>,
    /// User-moved rotation pivot for the active single Path: `(layer_id, pivot in
    /// object-LOCAL coords)`. `None` — or an id that isn't the active layer —
    /// means the default (the box centre). CorelDRAW-style: dragging the centre
    /// marker relocates the point every rotation turns about. Kept per-layer so a
    /// different object resets to centre; a material (local) point so it stays
    /// glued to the object as it moves/scales/rotates.
    pub(in crate::app) path_pivot: Option<(u32, crate::core::geometry::Point)>,
    /// True while the rotation-pivot marker itself is being dragged (Move tool),
    /// as opposed to a scale/rotate handle.
    pub(in crate::app) path_pivot_dragging: bool,
    /// The last repeatable "duplicate + transform" step, as a CANVAS-space affine
    /// `M`: Repeat (Ctrl+D / the button) duplicates the selection and applies `M`
    /// to each copy, so a single sample fans out into a row / ring / spiral.
    /// Captured from Alt+move (translate), Alt+rotate (rotate about the pivot) and
    /// Ctrl+scale (scale about the anchor). `None` = nothing to repeat yet.
    pub(in crate::app) last_repeat_transform: Option<crate::core::vector::affine::AffineTransform>,
    /// Layer-stack snapshot captured at the START of an Alt/Ctrl duplicate-transform
    /// gesture, so the duplicate + its transform commit as ONE undo step on release.
    pub(in crate::app) path_dup_before: Option<crate::core::command::LayerStructureCommand>,
    /// Active on-canvas vector-gradient transform handle drag.
    pub(in crate::app) path_gradient_drag: Option<PathGradientDrag>,
    /// Active node-edit drag under the Node tool. See [`NodeDrag`]. None unless a
    /// node is being dragged.
    pub(in crate::app) node_drag: Option<NodeDrag>,
    /// The currently-selected Path node `(layer_id, contour, node)` for the Node
    /// tool — highlighted in the overlay and the target of the Delete key.
    pub(in crate::app) node_selected: Option<(u32, usize, usize)>,
    /// Additional selected Path nodes `(contour, node)` on the SAME layer as
    /// `node_selected`, for multi-node move / delete / align. Excludes the primary
    /// (which lives in `node_selected`). Empty for a single-node selection.
    pub(in crate::app) node_multi: Vec<(usize, usize)>,
    /// Active Node-tool rubber-band selection rect in SCREEN space
    /// `(start_x, start_y, cur_x, cur_y)`. `None` unless dragging on empty canvas.
    pub(in crate::app) node_marquee: Option<(f32, f32, f32, f32)>,
    /// Baseline `(layer_id, style)` captured when an interactive Path style edit
    /// (colour dialog / outline-width scrub) begins, so the whole interaction
    /// commits as ONE `ChangeVectorStyle` on release. None when idle.
    pub(in crate::app) pending_path_style: Option<(u32, crate::core::vector::style::VectorStyle)>,
    /// Deferred options-bar style edit (Radius/Stroke/colour scrub) for a
    /// Shape layer. See [`ShapeStylePending`].
    pub(in crate::app) shape_style_pending: Option<ShapeStylePending>,
    /// Default font size (px) for new text, editable from the options bar.
    pub(in crate::app) text_font_px: f32,
    /// While true, new text sessions auto-pick a font size that suits the
    /// canvas dimensions; cleared the first time the user sets a size
    /// explicitly (or adopts one by re-editing a text layer).
    pub(in crate::app) text_font_px_auto: bool,
    pub(in crate::app) text_font_family: crate::core::text::TextFontFamily,
    pub(in crate::app) text_color: [u8; 4],
    /// Default alignment for new text.
    pub(in crate::app) text_align: crate::core::text::TextAlign,
    pub(in crate::app) text_bold: bool,
    pub(in crate::app) text_italic: bool,
    pub(in crate::app) text_underline: bool,
    pub(in crate::app) text_line_height: f32,
    pub(in crate::app) text_tracking_px: f32,
    pub(in crate::app) text_opacity: f32,
    /// One-shot: request egui to focus the text overlay on the next frame.
    pub(in crate::app) text_focus_pending: bool,
    /// egui family names (iai_text_*) currently installed in the egui context.
    /// Only builtins load at startup; others register on demand
    /// (`ensure_text_font_registered`).
    pub(in crate::app) text_fonts_registered: std::collections::HashSet<String>,
    /// Families that failed to load/parse — cached so we don't retry disk IO
    /// every time they are referenced.
    pub(in crate::app) text_fonts_failed: std::collections::HashSet<String>,
    /// Original text style while the font picker is doing hover preview.
    pub(in crate::app) text_font_preview: Option<TextFontPreviewState>,
    /// Screen-space position where right-click happened inside the transform bbox.
    /// Set in input.rs, consumed by the UI to show a context menu popup.
    pub(in crate::app) transform_ctx_menu_pos: Option<(f32, f32)>,
    /// Screen-space position of the right-click brush settings popup (Brush /
    /// Pencil / Eraser). Set in input.rs, consumed by the UI. None when closed.
    pub(in crate::app) brush_popup_pos: Option<(f32, f32)>,
    /// Screen-space position of the right-click context menu for the selection
    /// tools (marquee / lasso / quick-select). Set in input.rs, consumed by the
    /// UI. None when closed.
    pub(in crate::app) selection_ctx_menu_pos: Option<(f32, f32)>,
    pub(in crate::app) text_drag_hovered: bool,
    pub(in crate::app) text_panel_hovered: bool,
    pub(in crate::app) show_refine_panel: bool,
    pub(in crate::app) refine_feather: f32,
    pub(in crate::app) refine_smooth: u32,
    pub(in crate::app) refine_smart_radius: f32,
    pub(in crate::app) refine_shift_edge: f32,
    pub(in crate::app) refine_contrast: f32,
    pub(in crate::app) refine_decontaminate: bool,
    pub(in crate::app) refine_decontaminate_amount: f32,
    /// Snapshot of selection mask taken when refine panel was opened.
    /// Used to restore on Cancel, and as base for preview on each change.
    pub(in crate::app) refine_snapshot: Vec<u8>,
    /// Whether the refine preview is dirty and needs re-applying.
    pub(in crate::app) refine_dirty: bool,
    /// Tool that was active before the refine panel was opened — restored on close.
    pub(in crate::app) refine_prev_tool: crate::tools::ToolId,
    /// Current view mode for the Refine Selection workspace.
    pub(in crate::app) refine_view_mode: RefineViewMode,
    /// Overlay tint color used by Refine Selection.
    pub(in crate::app) refine_overlay_color: [u8; 4],
    /// Output destination when user clicks OK in Refine Selection.
    pub(in crate::app) refine_output_mode: RefineOutputMode,
    /// Per-pixel overlay texture for Overlay view mode.
    /// RGBA image: selected pixels → transparent, unselected → red tint.
    /// Rebuilt lazily whenever `selection.mask_revision` changes.
    pub(in crate::app) refine_overlay_tex: Option<egui::TextureHandle>,
    pub(in crate::app) refine_overlay_mask_rev: u64,
}

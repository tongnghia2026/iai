pub(in crate::app) use std::path::PathBuf;
pub(in crate::app) use std::sync::Arc;

use winit::event_loop::ActiveEventLoop;
pub(in crate::app) use winit::window::{CursorIcon, Window};

use super::background_jobs::BackgroundJobs;
use super::develop_shell::DevelopShell;
use super::document_session::DocumentSession;
use super::editor_interaction::EditorInteraction;
use super::ui_shell::UiShell;
use super::window_runtime::WindowRuntime;
pub(in crate::app) use crate::core::cms::ProofTarget;
pub(in crate::app) use crate::core::document::{Document, DocumentId, GuideOrientation};
pub(in crate::app) use crate::core::geometry::InterpolationMode;
use crate::event_bus::create_bus;
pub(in crate::app) use crate::event_bus::SharedBus;
pub(in crate::app) use crate::extension::tool::PointerEvent;
pub(in crate::app) use crate::formats::FormatRegistry;
pub(in crate::app) use crate::gpu::GpuState;
pub(in crate::app) use crate::tools::{ToolId, ToolManager};
pub(in crate::app) use crate::ui::refine_select::{RefineOutputMode, RefineViewMode};
pub(in crate::app) use std::time::Instant;

/// On-screen ring radius (px) above which the brush cursor switches from a
/// custom OS cursor to the in-canvas GPU ring.
///
/// A large custom OS cursor (IAI's brush ring reached 806×806 px) makes
/// external tools that mirror the cursor — UltraViewer remote desktop, screen
/// recorders — overflow their 16-bit cursor-size arithmetic and pop a native
/// `CheckCursorChange :0 :Overflow` dialog. Larger rings are drawn by the GPU
/// shader instead (see `push_cursor_uniforms`), but the GPU ring follows the
/// pointer one frame late, so a brush above this radius feels laggy on hover.
///
/// 200 → ~406 px bitmap: brushes up to ~400 px diameter keep the instant native
/// cursor; still well under the ~806 px that tripped the UltraViewer overflow.
/// If a remote-desktop / screen-mirroring tool starts popping the overflow
/// dialog again, lower this back toward 60.
pub(crate) const MAX_NATIVE_RING_RADIUS: u32 = 200;

pub struct ViewState {
    pub offset_x: f32,
    pub offset_y: f32,
    pub zoom: f32,
}

/// Which of the 8 edge/corner handles (or center) is being dragged.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TransformHandle {
    TopLeft,
    TopCenter,
    TopRight,
    MiddleLeft,
    MiddleRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
    Center,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TransformMode {
    #[default]
    Free,
    Skew,
    Distort,
    Perspective,
}

/// Snapshot of one layer's state before a free transform.
#[derive(Clone)]
pub struct LayerOrigState {
    pub layer_id: u32,
    pub layer_idx: usize,
    pub layer_type: crate::core::layer::LayerType,
    pub tiles: crate::core::tile::TileMap,
    pub mask: Option<crate::core::layer::LayerMask>,
    pub offset: (i32, i32),
    pub width: u32,
    pub height: u32,
    pub content_offset: (i32, i32),
    pub content_w: u32,
    pub content_h: u32,
}

/// All state for an ongoing Ctrl+T free-transform session.
/// Lives in App and is None when no transform is active.
#[derive(Clone)]
pub struct TransformState {
    pub layer_states: Vec<LayerOrigState>,
    pub preview_layer_states: Vec<LayerOrigState>,
    #[allow(dead_code)]
    pub layer_idx: usize,
    #[allow(dead_code)]
    pub layer_id: u32,
    pub orig_offset: (i32, i32),
    pub orig_w: u32,
    pub orig_h: u32,
    pub scale_x: f32,
    pub scale_y: f32,
    pub angle_deg: f32,
    pub translate_x: f32,
    pub translate_y: f32,
    pub pivot_cx: f32,
    pub pivot_cy: f32,
    pub drag_handle: Option<Option<TransformHandle>>,
    pub drag_start_cx: f32,
    pub drag_start_cy: f32,
    pub drag_start_sx: f32,
    pub drag_start_sy: f32,
    pub drag_start_angle: f32,
    pub drag_start_tx: f32,
    pub drag_start_ty: f32,
    /// Destination corners in TL, TR, BR, BL order for non-affine modes.
    pub quad: Option<[(f32, f32); 4]>,
    pub drag_start_quad: [(f32, f32); 4],
    pub mode: TransformMode,
}

pub struct TransformCommitLayer {
    pub layer_id: u32,
    pub layer_type: crate::core::layer::LayerType,
    pub tiles: crate::core::tile::TileMap,
    pub mask: Option<crate::core::layer::LayerMask>,
    pub width: u32,
    pub height: u32,
    pub offset: (i32, i32),
}

pub struct TransformCommitResult {
    pub doc_id: crate::core::document::DocumentId,
    pub command: crate::core::command::FreeTransformCommand,
    pub layers: Vec<TransformCommitLayer>,
}

impl TransformState {
    /// Forward-transform a canvas point through the current transform.
    pub fn transform_point(&self, cx: f32, cy: f32) -> (f32, f32) {
        if let Some(quad) = self.quad {
            let w = self.orig_w.max(1) as f32;
            let h = self.orig_h.max(1) as f32;
            let u = (cx - self.orig_offset.0 as f32) / w;
            let v = (cy - self.orig_offset.1 as f32) / h;
            if let Some(map) = crate::core::geometry::Homography::square_to_quad(
                quad.map(|(x, y)| crate::core::geometry::Point::new(x, y)),
            ) {
                let p = map.apply(u, v);
                return (p.x, p.y);
            }
        }
        let rad = self.angle_deg.to_radians();
        let c = rad.cos();
        let s = rad.sin();
        let dx = cx - self.pivot_cx;
        let dy = cy - self.pivot_cy;
        let nx = self.pivot_cx + self.translate_x + c * self.scale_x * dx - s * self.scale_y * dy;
        let ny = self.pivot_cy + self.translate_y + s * self.scale_x * dx + c * self.scale_y * dy;
        (nx, ny)
    }

    /// Inverse map from destination canvas coordinates back to original canvas.
    pub fn inverse_canvas_point(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        if let Some(quad) = self.quad {
            let map = crate::core::geometry::Homography::square_to_quad(
                quad.map(|(x, y)| crate::core::geometry::Point::new(x, y)),
            )?;
            let uv = map.inverse()?.apply(x, y);
            return Some((
                self.orig_offset.0 as f32 + uv.x * self.orig_w as f32,
                self.orig_offset.1 as f32 + uv.y * self.orig_h as f32,
            ));
        }
        let (a, b, c, d) = self.inv_matrix();
        let dx = x - (self.pivot_cx + self.translate_x);
        let dy = y - (self.pivot_cy + self.translate_y);
        Some((
            self.pivot_cx + a * dx + b * dy,
            self.pivot_cy + c * dx + d * dy,
        ))
    }

    /// Inverse canvas-to-canvas homography used by GPU preview and CPU commit.
    pub fn inverse_homography(&self) -> Option<[f32; 9]> {
        let src = [
            crate::core::geometry::Point::new(self.orig_offset.0 as f32, self.orig_offset.1 as f32),
            crate::core::geometry::Point::new(
                self.orig_offset.0 as f32 + self.orig_w as f32,
                self.orig_offset.1 as f32,
            ),
            crate::core::geometry::Point::new(
                self.orig_offset.0 as f32 + self.orig_w as f32,
                self.orig_offset.1 as f32 + self.orig_h as f32,
            ),
            crate::core::geometry::Point::new(
                self.orig_offset.0 as f32,
                self.orig_offset.1 as f32 + self.orig_h as f32,
            ),
        ];
        let dst = if let Some(q) = self.quad {
            q.map(|(x, y)| crate::core::geometry::Point::new(x, y))
        } else {
            src.map(|p| {
                let (x, y) = self.transform_point(p.x, p.y);
                crate::core::geometry::Point::new(x, y)
            })
        };
        let unit_to_src = crate::core::geometry::Homography::square_to_quad(src)?;
        let dst_to_unit = crate::core::geometry::Homography::square_to_quad(dst)?.inverse()?;
        let a = dst_to_unit.m;
        let b = unit_to_src.m;
        let mut out = [0.0; 9];
        for r in 0..3 {
            for c in 0..3 {
                out[r * 3 + c] = (0..3).map(|k| b[r * 3 + k] * a[k * 3 + c]).sum();
            }
        }
        Some(out)
    }

    /// Inverse matrix coefficients used by the GPU shader.
    /// inv = M⁻¹ where M = [[cos*sx, -sin*sy], [sin*sx, cos*sy]].
    pub fn inv_matrix(&self) -> (f32, f32, f32, f32) {
        let rad = self.angle_deg.to_radians();
        let c = rad.cos();
        let s = rad.sin();
        let sx = if self.scale_x.abs() < 1e-6 {
            1e-6_f32
        } else {
            self.scale_x
        };
        let sy = if self.scale_y.abs() < 1e-6 {
            1e-6_f32
        } else {
            self.scale_y
        };
        (c / sx, s / sx, -s / sy, c / sy)
    }

    /// Positions of the 8 handles in canvas space.
    /// Order: TL, TC, TR, ML, MR, BL, BC, BR.
    pub fn handle_positions(&self) -> [(f32, f32); 8] {
        if let Some([tl, tr, br, bl]) = self.quad {
            let mid = |a: (f32, f32), b: (f32, f32)| ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5);
            return [
                tl,
                mid(tl, tr),
                tr,
                mid(tl, bl),
                mid(tr, br),
                bl,
                mid(bl, br),
                br,
            ];
        }
        let ox = self.orig_offset.0 as f32;
        let oy = self.orig_offset.1 as f32;
        let w = self.orig_w as f32;
        let h = self.orig_h as f32;
        [
            self.transform_point(ox, oy),
            self.transform_point(ox + w / 2.0, oy),
            self.transform_point(ox + w, oy),
            self.transform_point(ox, oy + h / 2.0),
            self.transform_point(ox + w, oy + h / 2.0),
            self.transform_point(ox, oy + h),
            self.transform_point(ox + w / 2.0, oy + h),
            self.transform_point(ox + w, oy + h),
        ]
    }

    /// Bounding box corners in canvas space (TL, TR, BL, BR).
    pub fn corners(&self) -> [(f32, f32); 4] {
        let ox = self.orig_offset.0 as f32;
        let oy = self.orig_offset.1 as f32;
        let w = self.orig_w as f32;
        let h = self.orig_h as f32;
        [
            self.transform_point(ox, oy),
            self.transform_point(ox + w, oy),
            self.transform_point(ox, oy + h),
            self.transform_point(ox + w, oy + h),
        ]
    }

    /// Local coords (relative to pivot) for each handle — used in scale math.
    pub fn handle_local(&self, handle: TransformHandle) -> (f32, f32) {
        let hw = self.orig_w as f32 / 2.0;
        let hh = self.orig_h as f32 / 2.0;
        match handle {
            TransformHandle::TopLeft => (-hw, -hh),
            TransformHandle::TopCenter => (0.0, -hh),
            TransformHandle::TopRight => (hw, -hh),
            TransformHandle::MiddleLeft => (-hw, 0.0),
            TransformHandle::MiddleRight => (hw, 0.0),
            TransformHandle::BottomLeft => (-hw, hh),
            TransformHandle::BottomCenter => (0.0, hh),
            TransformHandle::BottomRight => (hw, hh),
            TransformHandle::Center => (0.0, 0.0),
        }
    }
}

pub struct InputState {
    pub mouse_x: f32,
    pub mouse_y: f32,
    pub last_mouse_x: f64,
    pub last_mouse_y: f64,
    pub painting: bool,
    pub mid_dragging: bool,
    pub is_over_ui: bool,
    pub was_over_ui: bool,
    pub space_held: bool,
    pub space_dragging: bool,
    pub alt_held: bool,
    pub ctrl_held: bool,
    pub shift_held: bool,
    /// True while an Alt-hold temporary eyedropper drag is in progress (paint
    /// tools): each pointer move samples the canvas colour into the foreground.
    pub eyedropping: bool,
    /// True when the pointer is hovering the floating paint-colour dialog window
    /// (used to shrink the brush ring cursor while choosing a colour).
    pub paint_dialog_hovered: bool,
    pub alt_right_dragging: bool,
    pub alt_drag_start_x: f32,
    pub alt_drag_start_y: f32,
    pub alt_drag_start_size: f32,
    /// True while an Alt+left-drag is resizing the Warp brush (no warp this drag).
    pub warp_resizing: bool,
    pub zoom_dragging: bool,
    pub zoom_drag_moved: bool,
    pub zoom_drag_start_x: f32,
    pub zoom_drag_start_y: f32,
    pub zoom_drag_anchor_x: f32,
    pub zoom_drag_anchor_y: f32,
    pub zoom_drag_start_zoom: f32,
    /// Time of the last left-button release, used to detect double-clicks.
    pub last_left_release_time: Option<std::time::Instant>,
    /// Pointer is over the fixed window chrome (top bars, tool/side panels,
    /// status bar) — as opposed to the canvas area or floating egui overlays.
    pub in_ui_chrome: bool,
}

pub struct UiState {
    pub show_welcome: bool,
    /// The Library grid browser (Track B) is showing instead of the editor.
    pub show_library: bool,
    pub show_new_dialog: bool,
    pub show_resize_dialog: bool,
    pub show_image_size_dialog: bool,
    pub show_rename_dialog: bool,
    pub show_export_dialog: bool,
    pub show_print_dialog: bool,
    pub show_preferences: bool,
    pub show_adjustment_dialog: bool,
    pub adjustment_dialog: crate::core::layer::AdjustmentType,
    pub adjustment_preview_enabled: bool,
    pub adjustment_options: crate::ui::AdjustmentOptions,
    pub adj_eyedropper: Option<crate::ui::AdjEyedropperKind>,
    pub show_develop_dialog: bool,
    pub develop_settings: crate::core::develop::DevelopSettings,
    /// Which local mask row the Develop panel has selected (its sliders show).
    pub develop_local_selected: Option<usize>,
    /// Armed local-mask placement: the next canvas drag places this kind, into
    /// `locals[target]` when re-placing an existing mask (None appends).
    pub develop_local_arm: Option<(crate::core::develop::LocalMaskKind, Option<usize>)>,
    pub show_warp_dialog: bool,
    pub warp_params: crate::core::warp::WarpParams,
    pub show_filter_dialog: bool,
    pub filter_dialog: crate::core::filters::FilterType,
    pub filter_preview_enabled: bool,
    pub show_smart_fill_dialog: bool,
    /// Gradient tool → gradient editor window open.
    pub show_gradient_editor: bool,
    pub show_ai_panel: bool,
    pub ai: crate::core::ai::AiPanelState,
    pub ai_status: String,
    pub show_exit_dialog: bool,
    pub theme_mode: crate::ui::theme::ThemeMode,
    pub show_close_dialog: bool,
    pub show_feather_dialog: bool,
    pub show_modify_dialog: Option<crate::ui::SelectionModifyKind>,
    pub show_stroke_dialog: bool,
    /// Editable New Canvas dimensions in `new_unit`. Keep these separate from
    /// the rounded pixel dimensions so physical values such as 10 cm remain
    /// exactly 10 while the user changes focus, unit, or DPI.
    pub new_w_input: f32,
    pub new_h_input: f32,
    pub new_dpi: f32,
    pub new_bg_color: u8,
    pub new_name: String,
    pub new_unit: crate::core::units::Unit,
    pub rename_idx: usize,
    pub rename_text: String,
    pub export_format: crate::formats::ExportFormat,
    /// "Embed Color Profile (ICC)" toggle for export (default on).
    pub export_embed_icc: bool,
    pub transform_interpolation: InterpolationMode,
    pub show_color_panel: bool,
    pub show_text_panel: bool,
    pub show_layer_panel: bool,
    pub show_history_panel: bool,
    pub show_info_panel: bool,
    pub show_channels_panel: bool,
    pub show_rulers: bool,
    /// Show ruler guides on the canvas.
    pub show_guides: bool,
    /// When locked, guides can't be moved or deleted by dragging.
    pub lock_guides: bool,
    /// Master snapping toggle (guides ②, layer move ③, transform ④).
    pub snap_enabled: bool,
    pub show_preset_dialog: bool,
    pub show_delete_preset_dialog: bool,
    pub preset_dialog_name: String,
    pub preset_dialog_w: f32,
    pub preset_dialog_h: f32,
    pub preset_dialog_unit: String,
    pub preset_dialog_dpi: f32,
    pub show_refine_color_dialog: bool,
    pub refine_color_dialog_color: [u8; 4],
    pub refine_color_dialog_original: [u8; 4],
    pub refine_color_dialog_live_preview: bool,
    pub refine_color_dialog_center_next: bool,
    pub show_paint_color_dialog: bool,
    pub paint_color_dialog_target: u8,
    pub paint_color_dialog_color: [u8; 4],
    pub paint_color_dialog_original: [u8; 4],
    pub paint_color_dialog_live_preview: bool,
    pub paint_color_dialog_center_next: bool,
    /// While set (and in the future), the active modal's Commit/Cancel
    /// controls blink to draw attention (blocked exit attempts).
    pub modal_flash_until: Option<Instant>,
    /// Image ▸ Mode ▸ CMYK Color… convert dialog.
    pub show_cmyk_convert_dialog: bool,
    /// Dialog choice: browsed ICC space (true) or built-in naive GCR (false).
    pub cmyk_convert_use_icc: bool,
    /// The `.icc` picked for CMYK conversion: `(display name, raw bytes)`.
    /// Pre-loaded from the prefs' remembered path when the dialog opens.
    pub cmyk_convert_icc: Option<(String, Vec<u8>)>,
}

/// Audible attention chime for blocked modal actions (Windows system sound;
/// no-op on other platforms and in tests).
fn message_beep(kind: u32) {
    #[cfg(all(windows, not(test)))]
    {
        #[link(name = "user32")]
        extern "system" {
            fn MessageBeep(u_type: u32) -> i32;
        }
        unsafe {
            MessageBeep(kind);
        }
    }
    #[cfg(any(not(windows), test))]
    let _ = kind;
}

pub(crate) fn alert_beep() {
    // MB_ICONEXCLAMATION: the standard "attention" chime.
    message_beep(0x30);
}

pub(crate) fn notify_beep() {
    // MB_ICONASTERISK: the standard informational notification chime.
    message_beep(0x40);
}

pub struct AdjustmentPreviewState {
    pub doc_id: crate::core::document::DocumentId,
    pub layer_id: u32,
    pub original_tiles: crate::core::tile::TileMap,
    pub original_flat: Vec<u8>,
    pub levels_histogram: std::sync::Arc<[[u32; 256]; 4]>,
}

#[derive(Clone, PartialEq, Debug)]
pub enum StartupPhase {
    Loading(String),
    Done,
}

pub enum StartupProgress {
    Log(String),
    FontsReady(egui::FontDefinitions),
}

pub struct PendingReloadPrompt {
    pub doc_idx: usize,
    pub path: PathBuf,
}

/// A PDF is being opened and the page-selection dialog is up. Holds what the
/// dialog needs to list pages; the actual per-page selection lives in egui temp
/// state (keyed by path) and is sent back on confirm. See `file_ops.rs`.
pub struct PdfImportPrompt {
    pub path: PathBuf,
    pub page_count: usize,
    /// Per-page size in points, for display in the dialog.
    pub page_dims: Vec<(f32, f32)>,
}

/// Snapshot for a live filter preview on the active raster layer (Gaussian Blur,
/// Sharpen…). Commit/cancel reuse the generic `commit_layer_tiles_change` /
/// `restore_layer_tiles` paths shared with adjustments.
pub struct FilterPreviewSession {
    pub doc_id: crate::core::document::DocumentId,
    pub layer_id: u32,
    pub original_tiles: crate::core::tile::TileMap,
    pub job_id: u64,
    pub processing: bool,
    pub gpu_preview_active: bool,
    pub cpu_preview_active: bool,
    pub pending_filter: Option<crate::core::filters::FilterType>,
    pub last_preview_filter: Option<crate::core::filters::FilterType>,
    pub rx: Option<std::sync::mpsc::Receiver<FilterPreviewResult>>,
    pub proxy_original: Vec<u8>,
    pub proxy_original_preview: std::sync::Arc<egui::ColorImage>,
    pub proxy_w: u32,
    pub proxy_h: u32,
    pub proxy_scale: f32,
    pub proxy_preview: Option<std::sync::Arc<egui::ColorImage>>,
    pub proxy_filter: Option<crate::core::filters::FilterType>,
}

pub struct FilterPreviewResult {
    pub job_id: u64,
    pub filter: crate::core::filters::FilterType,
    pub tiles: crate::core::tile::TileMap,
}

pub struct DevelopPreviewState {
    pub doc_id: crate::core::document::DocumentId,
    pub layer_id: u32,
    pub original_tiles: crate::core::tile::TileMap,
    /// Linear scene-referred master (RAW sessions). When present, previews and
    /// bakes run the scene-referred chain (`develop_scene`) instead of the
    /// legacy display-domain engine, and `histogram_proxy` holds LINEAR samples.
    pub scene: Option<std::sync::Arc<crate::core::develop_scene::SceneSource>>,
    /// Grid-sampled source pixels for the curve-editor histogram; re-binned
    /// through the current settings on every slider change so the backdrop
    /// follows the adjustments (see `develop::histogram_rgbl`). LINEAR scene
    /// samples when `scene` is present (`develop_scene::histogram_rgbl_scene`).
    pub histogram_proxy: std::sync::Arc<Vec<[f32; 3]>>,
    pub job_id: u64,
    pub processing: bool,
    pub gpu_preview_active: bool,
    pub pending_settings: Option<crate::core::develop::DevelopSettings>,
    pub last_preview_settings: crate::core::develop::DevelopSettings,
    pub rx: Option<std::sync::mpsc::Receiver<DevelopPreviewResult>>,
    /// Debounced commit-quality CPU bake for the Detail group: the shader
    /// cannot run Sharpening/NR (full-res neighbourhood passes), so a
    /// Detail-engaged edit schedules this bake to land over the GPU preview
    /// once the sliders go quiet. Any newer edit re-schedules it.
    pub detail_refine_at: Option<std::time::Instant>,
    pub detail_refine_settings: Option<crate::core::develop::DevelopSettings>,
}

pub struct DevelopPreviewResult {
    pub job_id: u64,
    pub settings: crate::core::develop::DevelopSettings,
    pub tiles: crate::core::tile::TileMap,
}

/// Active Warp session (Filter ▸ Warp… / Ctrl+Shift+X). Modal like Free
/// Transform: pointer input warps the layer through `mesh`, the layer preview is
/// updated live, and Apply/Cancel commits or discards. `original_*` is the
/// untouched layer (the warp always gathers from here); `working_flat` is the
/// current warped RGBA, rebuilt per dab over the brush rect.
pub struct WarpState {
    pub doc_id: crate::core::document::DocumentId,
    pub layer_id: u32,
    pub layer_w: usize,
    pub layer_h: usize,
    pub layer_offset: (i32, i32),
    pub original_tiles: crate::core::tile::TileMap,
    pub original_flat: Vec<u8>,
    pub working_flat: Vec<u8>,
    pub mesh: crate::core::warp::WarpMesh,
    /// True between pointer-down and pointer-up (a single warp stroke).
    pub dragging: bool,
    /// Previous pointer position in layer-local pixels, for the per-dab delta.
    pub last_lx: f32,
    pub last_ly: f32,
}

/// Cached region proxies for the GPU Develop preview. BOTH the colour `region` base
/// and the local-tone `region_luma_base` are the tone-INDEPENDENT box-averages, so
/// they are rebuilt only when the viewport/layer changes; tone (incl. WB+Exposure)
/// is re-applied per frame from them. So ANY tone/colour drag — Exposure, Contrast,
/// Curve, Shadows, Saturation — reuses the cache and never rebuilds a full proxy
/// (and the per-pixel tone and the region proxies never drift out of sync).
pub struct DevelopProxyCache {
    pub layer_id: u32,
    /// Raw block-average (RGB) of the full image for the local-adaptation base luma;
    /// `region` holds the average, `w`/`h`/`downsample` its proxy dims. Tone-independent
    /// (built once). WB+Exposure + the guided low-pass are applied by
    /// `develop::finish_region_luma` into `region_luma` below.
    pub region_luma_base: Option<DevelopRegionCache>,
    /// Finished local-adaptation base luma, memoised on `region_luma_sig` = (temperature,
    /// tint, exposure) — its ONLY inputs (`apply_wb_ev`). So only an Exposure/WB drag
    /// recomputes it (cheap: no full-image read, just the cached base), while a
    /// Shadows/Contrast/Curve drag reuses it. Recomputed every frame during an Exposure
    /// drag (unthrottled) so it never drifts out of sync with the per-pixel tone.
    pub region_luma_sig: [u32; 3],
    pub region_luma: Option<crate::gpu::compositor::RegionLumaProxy>,
    pub color_region: Option<DevelopRegionCache>,
    pub fast_region: Option<DevelopRegionCache>,
    /// Finished colour/fast proxies (tone + guided low-pass + colour applied),
    /// memoised on the exact settings that built them: a pure zoom/pan recompose
    /// (settings unchanged) reuses them instead of re-running the per-frame
    /// tails on every view tick. Cleared whenever the bases are rebuilt.
    pub finished_color: Option<crate::gpu::compositor::ColorProxies>,
    pub finished_settings: Option<crate::core::develop::DevelopSettings>,
}

#[derive(Clone)]
pub struct DevelopRegionCache {
    pub region: std::sync::Arc<Vec<[f32; 3]>>,
    pub w: usize,
    pub h: usize,
    pub origin_x: u32,
    pub origin_y: u32,
    pub source_w: u32,
    pub source_h: u32,
    pub downsample: u32,
}

/// State for editing an adjustment layer's params via double-click (non-destructive).
/// Unlike `AdjustmentPreviewState`, which bakes into the raster layer.
pub struct AdjustmentLayerEditState {
    pub doc_id: crate::core::document::DocumentId,
    pub layer_id: u32,
    pub original_adj: crate::core::layer::AdjustmentType,
}

/// Active text-editing session (Text tool). While a session is open the layer
/// is rendered by the egui overlay (its tiles are kept empty) and rasterized
/// into the layer only on commit.
pub struct TextEditState {
    pub doc_id: crate::core::document::DocumentId,
    pub layer_id: u32,
    pub buffer: String,
    /// Canvas-space upright raster origin; rotated layers store a bbox offset.
    pub origin: (i32, i32),
    /// Rotation copied from `TextData` while editing.
    pub rotation_deg: f32,
    pub stretch_x: f32,
    pub flip_x: bool,
    pub flip_y: bool,
    /// True for a freshly created layer (discarded on cancel / empty commit).
    pub is_new: bool,
    /// Original text data, for restoring on cancel of a re-edit (None if new).
    pub orig: Option<crate::core::text::TextData>,
    /// Undo command snapshotting the stack before the edit; finalized on commit.
    pub before_cmd: Option<crate::core::command::LayerStructureCommand>,
    /// Per-character style, aligned 1:1 with `buffer` chars. Empty until
    /// the first per-glyph override; maintained across edits by diffing the
    /// buffer (see `App::update_text_buffer`).
    pub glyph_styles: Vec<crate::core::text::GlyphStyle>,
    /// Last known selection from the overlay, kept when toolbar/panel focus
    /// temporarily steals egui's TextEdit cursor state.
    pub selection: Option<std::ops::Range<usize>>,
    pub caret: Option<usize>,
    /// Style to apply to the next character typed at the caret (set when the
    /// user changes colour/size with no selection). `.0` is the caret index at
    /// which it is valid.
    pub pending_style: Option<(usize, crate::core::text::GlyphStyle)>,
}

/// An in-progress drag of a Shape layer's on-canvas handle (resize / corner
/// radius / line endpoint). Created on press over a handle, applied live on
/// drag, and finalized into an undo entry on release.
pub struct ShapeDragState {
    pub layer_id: u32,
    pub handle: crate::core::shape::ShapeHandle,
    /// Stable gesture baseline so toggling Shift/Alt mid-drag never compounds
    /// already-previewed geometry.
    pub original: (crate::core::shape::ShapeData, (i32, i32)),
    /// Undo command snapshotting the stack before the edit; finalized on release.
    pub before_cmd: Option<crate::core::command::LayerStructureCommand>,
    /// True once the drag actually changed the geometry (so a no-op click over a
    /// handle doesn't push an empty undo entry).
    pub changed: bool,
    /// Target geometry not yet baked into the layer raster. Rasterizing a big
    /// shape is O(bbox pixels) on the CPU, so bakes are throttled by measured
    /// cost; the vector overlay tracks the cursor every frame meanwhile.
    pub pending: Option<(crate::core::shape::ShapeData, (i32, i32))>,
    /// When the last bake finished + how long it took, setting the throttle.
    pub last_bake: Option<std::time::Instant>,
    pub bake_cost_secs: f32,
}

/// An in-progress on-canvas transform of a Path layer under the Move tool:
/// dragging a corner/edge handle scales, dragging the rotate ring outside a
/// corner rotates. The gesture edits the vector object's affine `transform`
/// (never bakes node coordinates); on release it records ONE
/// [`crate::core::command_vector::ChangeVectorTransform`] so the object stays
/// editable and the edit is a single undo step.
///
/// The overlay box follows `pending` every frame (smooth at 60 fps) while the
/// fill re-raster runs OFF-THREAD (see [`PathBakeInFlight`]) — rasterising a big
/// filled path on the UI thread each frame stalled the drag and made a rotation
/// look very laggy.
pub struct PathTransformDrag {
    pub layer_id: u32,
    /// The grabbed handle. `Some(h)` = scale via that handle; `None` = rotate.
    pub handle: Option<TransformHandle>,
    /// Object `transform` captured at press (after folding any pending Move
    /// drag). The undo baseline AND the frame every drag frame recomputes from.
    pub orig_transform: crate::core::vector::affine::AffineTransform,
    /// Latest target transform for this cursor position. Drives the overlay box
    /// every frame and is committed on release, independent of the throttled bake.
    pub pending: crate::core::vector::affine::AffineTransform,
    /// Fill geometry bounds in OBJECT-LOCAL space at press: its four corners map
    /// through the transform to the displayed box.
    pub local_bounds: crate::core::geometry::Rect,
    /// Canvas-space pivot (box centre) at press — the rotation centre.
    pub pivot: crate::core::geometry::Point,
    /// Cursor canvas position at press (rotation reference angle).
    pub start_cx: f32,
    pub start_cy: f32,
    /// True once the gesture actually changed the transform (so a no-op click on
    /// a handle pushes no undo entry).
    pub changed: bool,
    /// `false` (single Path): `orig_transform`/`pending` are the object's own
    /// transform and `local_bounds` is in OBJECT-LOCAL space — scaling runs in the
    /// object's local frame so it stays square to a rotated object.
    /// `true` (multi-Path union): `orig_transform` is the identity, `pending` is a
    /// CANVAS-space delta `M`, and `local_bounds` is the union AABB in CANVAS
    /// space. Each target's new transform is `M ∘ orig_i`.
    pub canvas_frame: bool,
    /// Every Path moved by this gesture: `(layer_id, transform captured at press)`.
    /// One entry for a single-Path drag; the whole selection for a union drag.
    /// Drives the per-layer GPU preview and the one-undo-group commit.
    pub targets: Vec<(u32, crate::core::vector::affine::AffineTransform)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathGradientHandle {
    Center,
    AxisX,
    AxisY,
}

/// An in-progress drag of a vector fill gradient's on-canvas transform handles.
pub struct PathGradientDrag {
    pub layer_id: u32,
    pub handle: PathGradientHandle,
    pub original: crate::core::vector::affine::AffineTransform,
    pub start_local: crate::core::geometry::Point,
}

/// What a [`NodeDrag`] is moving.
#[derive(Clone, Copy, PartialEq)]
pub enum NodeDragTarget {
    /// The node's anchor — the whole node (anchor + both handles) shifts rigidly.
    Anchor,
    /// One Bézier control handle; the opposite handle is coupled per the node's
    /// [`crate::core::vector::path::NodeKind`] (see
    /// [`crate::core::vector::ops::apply_handle_move`]).
    Handle(crate::core::vector::ops::HandleSide),
}

/// An in-progress node edit of a Path layer under the Node tool: dragging an
/// anchor moves it (handles follow), dragging a control handle reshapes the
/// curve, and a press on a segment first INSERTS an anchor then drags it. The
/// gesture edits only `PathData` geometry (object transform kept) and, on
/// release, records ONE
/// [`crate::core::command_vector::ReplacePathGeometry`] so an insert+move is a
/// single undo step. Like [`PathTransformDrag`], the overlay follows `pending`
/// every frame while the fill re-raster runs off-thread.
pub struct NodeDrag {
    pub layer_id: u32,
    /// Which contour + node index is being dragged (in `pending`).
    pub contour: usize,
    pub node: usize,
    /// Whether the gesture moves the anchor (handles follow rigidly) or one of
    /// the node's Bézier control handles (kind-coupled — see [`NodeDragTarget`]).
    pub target: NodeDragTarget,
    /// Path geometry BEFORE the whole gesture (incl. before any insert). The undo
    /// baseline and the state the model is rewound to before the commit.
    pub orig_path: crate::core::vector::path::PathData,
    /// Latest geometry for this cursor position (drives the overlay + commit).
    pub pending: crate::core::vector::path::PathData,
    /// Grab point in OBJECT-LOCAL space, and the dragged node (anchor + handles)
    /// at press, so the node — and its handles — track the cursor rigidly without
    /// jumping to it.
    pub grab_local: crate::core::geometry::Point,
    pub base_node: crate::core::vector::path::Node,
    pub changed: bool,
    /// For a multi-node move: the OTHER selected nodes `(contour, node, base)`
    /// dragged rigidly alongside the primary. Empty for a single-node drag or a
    /// handle drag.
    pub group: Vec<(usize, usize, crate::core::vector::path::Node)>,
}

/// A deferred options-bar style edit (Radius/Stroke/colour scrub) for a Shape
/// layer. Scrubbing emits a tick per frame and each tick used to re-rasterize
/// the whole shape; bakes are now throttled by measured cost (like
/// [`ShapeDragState`]) and the latest tool style is applied on the next due
/// frame. `(doc_id, layer_id)` pin the target so a stale flush can never hit
/// another document or layer.
pub struct ShapeStylePending {
    pub doc_id: crate::core::document::DocumentId,
    pub layer_id: u32,
    /// A style tick arrived since the last bake.
    pub dirty: bool,
    /// The fill controls were explicitly changed. Geometry-only edits must
    /// preserve an existing vector gradient.
    pub apply_fill: bool,
    /// The outline controls were explicitly changed.
    pub apply_stroke: bool,
    /// The corner / sides / star controls were explicitly changed. A fill or
    /// outline edit leaves this false so it never overwrites geometry the user
    /// shaped on canvas — e.g. a corner radius dragged with the shape handle.
    pub apply_corner: bool,
    pub last_bake: Option<std::time::Instant>,
    pub bake_cost_secs: f32,
}

/// An off-thread Shape rasterization (handle drag / style scrub on RGB
/// documents): the worker renders `data` and builds the `TileMap`; the UI
/// thread polls per frame (`poll_shape_bake`) and swaps the result in, so a
/// page-sized shape never stalls input. One job at a time — its completion
/// chains the next from whatever target is pending by then. `(doc_id,
/// layer_id)` pin the destination; results for anything no longer active are
/// dropped.
pub struct ShapeBakeInFlight {
    pub doc_id: crate::core::document::DocumentId,
    pub layer_id: u32,
    /// The geometry/style being rendered (applied to the layer on completion).
    pub data: crate::core::shape::ShapeData,
    pub offset: (i32, i32),
    pub started: std::time::Instant,
    #[allow(clippy::type_complexity)]
    pub rx: std::sync::mpsc::Receiver<Option<(crate::core::tile::TileMap, u32, u32)>>,
}

/// An off-thread Path rasterization (a live scale/rotate or node drag on an RGB
/// document): the worker renders the whole [`VectorObjectData`] and its tight
/// `TileMap` + placement offset; the UI thread polls per frame
/// (`poll_path_bake`) and swaps the result in, so a page-sized filled path never
/// stalls the drag. One job at a time — its completion starts whatever
/// `path_bake_next` holds by then (latest wins). `doc_id`/`layer_id` pin the
/// destination; a result for anything no longer active is dropped.
pub struct PathBakeInFlight {
    pub doc_id: crate::core::document::DocumentId,
    pub layer_id: u32,
    /// The object being rendered (its model becomes the layer's on completion).
    pub object: crate::core::vector::object::VectorObjectData,
    pub started: std::time::Instant,
    #[allow(clippy::type_complexity)]
    pub rx: std::sync::mpsc::Receiver<Option<(crate::core::tile::TileMap, u32, u32, (i32, i32))>>,
}

pub struct TextFontPreviewSession {
    pub glyph_styles: Vec<crate::core::text::GlyphStyle>,
    pub pending_style: Option<(usize, crate::core::text::GlyphStyle)>,
    pub selection: Option<std::ops::Range<usize>>,
    pub caret: Option<usize>,
}

pub struct TextFontPreviewState {
    pub font_family: crate::core::text::TextFontFamily,
    pub session: Option<TextFontPreviewSession>,
}

#[derive(Clone, PartialEq)]
pub struct PathDisplayObjectKey {
    pub layer_id: u32,
    pub layer_offset: (i32, i32),
    pub object: crate::core::vector::object::VectorObjectData,
}

#[derive(Clone, PartialEq)]
pub struct PathDisplayCacheKey {
    pub doc_id: u32,
    pub scale: u8,
    pub clip: (u32, u32, u32, u32),
    pub objects: Vec<PathDisplayObjectKey>,
}

pub struct PathDisplayCacheEntry {
    pub key: PathDisplayCacheKey,
    pub display: crate::ui::PathDisplayRaster,
}

/// Finished off-thread bake of the crisp active-Path display overlay. Built on a
/// worker so zooming across scale buckets never rasterizes on the UI thread.
pub struct DisplayBakeOutput {
    pub tiles: Vec<crate::ui::PathDisplayTile>,
    pub canvas_x: f32,
    pub canvas_y: f32,
    pub canvas_w: f32,
    pub canvas_h: f32,
    pub raster_w: u32,
    pub raster_h: u32,
}

/// In-flight off-thread bake of the crisp display overlay (mirrors
/// [`PathBakeInFlight`]). `key` pins exactly what was requested so a result for a
/// stale zoom/geometry is matched (or ignored) correctly.
pub struct DisplayBakeInFlight {
    pub keys: Vec<PathDisplayCacheKey>,
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub rx: std::sync::mpsc::Receiver<Vec<(PathDisplayCacheKey, Option<DisplayBakeOutput>)>>,
}

#[derive(Default)]
pub struct UiDataCache {
    pub history_entries: std::sync::Arc<Vec<crate::core::command::HistoryEntry>>,
    pub history_revision: u64,

    pub layer_names: std::sync::Arc<Vec<String>>,
    pub layer_visibles: std::sync::Arc<Vec<bool>>,
    pub layer_opacities: std::sync::Arc<Vec<f32>>,
    pub layer_blend_modes: std::sync::Arc<Vec<crate::core::layer::BlendMode>>,
    pub layer_locked: std::sync::Arc<Vec<bool>>,
    pub layer_has_mask: std::sync::Arc<Vec<bool>>,
    pub layer_mask_enabled: std::sync::Arc<Vec<bool>>,
    pub layer_paint_targets: std::sync::Arc<Vec<crate::core::layer::PaintTarget>>,
    pub layer_mask_linked: std::sync::Arc<Vec<bool>>,
    pub layer_types: std::sync::Arc<Vec<String>>,
    pub layer_is_background: std::sync::Arc<Vec<bool>>,
    pub layer_lock_alpha: std::sync::Arc<Vec<bool>>,
    pub layer_selected: std::sync::Arc<Vec<bool>>,
    /// True when the layer is clipped to the one below (Photoshop clipping mask /
    /// PowerClip content) — panel draws it indented with a ↓ arrow.
    pub layer_is_clipped: std::sync::Arc<Vec<bool>>,
    /// True when some other layer is clipped to this one (a clip base) — panel
    /// underlines its name.
    pub layer_is_clip_base: std::sync::Arc<Vec<bool>>,
    pub layer_thumbnails: std::sync::Arc<Vec<Vec<u8>>>,
    pub layer_mask_thumbnails: std::sync::Arc<Vec<Vec<u8>>>,
    pub print_preview_image: Option<std::sync::Arc<egui::ColorImage>>,
    pub print_preview_doc_id: u32,
    pub print_preview_layer_revision: u64,
    pub print_preview_w: u32,
    pub print_preview_h: u32,
    /// Group nesting depth per layer (0 = top level) — panel indentation.
    pub layer_depths: std::sync::Arc<Vec<u32>>,
    /// Group folder expanded state per layer (only meaningful for Group layers).
    pub layer_expanded: std::sync::Arc<Vec<bool>>,
    /// True when a layer is hidden in the panel because an ancestor folder is
    /// collapsed.
    pub layer_collapsed_hidden: std::sync::Arc<Vec<bool>>,
    pub layer_revision: u64,
    /// Document id the cache holds. layer_revision is per-canvas (starts at 1), so
    /// switching documents can collide with an old value and skip the rebuild —
    /// doc_id forces a rebuild on document change.
    pub ui_cache_doc_id: u32,
    /// `(doc_id, active layer id)` the last time the panel revealed the active
    /// layer. When the active layer changes, `collect_ui_data` expands its
    /// collapsed ancestor folders and asks the panel to scroll it into view.
    pub reveal_last_active: Option<(u32, u32)>,

    /// Channels panel thumbnails: [composite, R, G, B] or [composite, C, M, Y,
    /// K] plates (panel-sized RGBA). Rebuilt when the composite `pixels` buffer is
    /// fresh at a new layer revision, or when an alpha channel changes.
    pub channel_thumbnails: std::sync::Arc<Vec<Vec<u8>>>,
    pub alpha_thumbnails: std::sync::Arc<Vec<Vec<u8>>>,
    pub alpha_channel_names: std::sync::Arc<Vec<(u32, String)>>,
    pub channel_thumbs_layer_revision: u64,
    pub channel_thumbs_alpha_key: u64,
    pub channel_thumbs_doc_id: u32,
    /// Rebuild throttle: the colour plates force a full CPU flatten, so they
    /// refresh at most every few hundred ms while the panel is open.
    pub channel_thumbs_built_at: Option<Instant>,
    pub path_displays: Vec<PathDisplayCacheEntry>,
    pub path_display_serial: u64,
    /// Top visible vector run omitted from the coarse document composite while
    /// its supersampled display raster is drawn above it.
    pub path_display_suppressed_layers: Option<(u32, Vec<u32>)>,
}

/// In-progress guide gesture (preview is rendered until committed).
#[derive(Debug, Clone, Copy)]
pub enum GuideOp {
    /// Dragging a brand-new guide out of a ruler.
    Create {
        orientation: GuideOrientation,
        pos: f32,
    },
    /// Dragging an existing guide (index into the active document's `guides`).
    Move {
        idx: usize,
        orientation: GuideOrientation,
        pos: f32,
    },
}

/// One finished off-thread "Open Image" bake (D3): the worker applied the
/// image's Develop settings to its source at full resolution; landing it is an
/// undoable tiles change on that document.
pub struct DevelopBakeAllResult {
    pub doc: DocumentId,
    pub layer_id: u32,
    pub original_tiles: crate::core::tile::TileMap,
    pub tiles: crate::core::tile::TileMap,
}

/// Sequential off-thread bake of every session image on "Open Image" (D3).
/// One image is in flight at a time (`rx`); the Develop window stays open
/// showing progress until the queue drains, then tears down.
pub struct DevelopBakeAll {
    /// Used to suppress a one-frame progress/original-image flash for fast
    /// single-image commits. The last preview frame remains presented until
    /// this delay expires or the bake finishes.
    pub started_at: std::time::Instant,
    /// Non-neutral images still to bake, in filmstrip order.
    pub pending: std::collections::VecDeque<(DocumentId, crate::core::develop::DevelopSettings)>,
    /// Progress denominator: how many images actually bake.
    pub bake_total: usize,
    /// Every session image (including neutral ones) — for the final status.
    pub total_images: usize,
    pub done: usize,
    /// Single-image RAW session (drives the "Opened/Developed RAW" wording).
    pub single_raw: bool,
    pub rx: Option<std::sync::mpsc::Receiver<DevelopBakeAllResult>>,
}

/// One image of the Develop stage's session (a filmstrip slot).
pub struct DevelopSessionEntry {
    pub doc: DocumentId,
    /// Transient RAW import (the open → Develop flow): Cancel closes the
    /// document — the pre-editor flow where "Open Image" is what creates it.
    /// A session begun from an already-open document keeps it on cancel.
    pub transient: bool,
    /// This image's Develop settings while it is NOT the active filmstrip
    /// image (the active image lives in `ui.develop_settings` and is saved
    /// back here on switch/commit).
    pub settings: crate::core::develop::DevelopSettings,
}

pub struct App {
    /// Open documents and their lifecycle (tabs, MRU, ids, autosave anchors).
    pub(in crate::app) docs: DocumentSession,
    /// OS windows, GPU, egui runtimes, cursors, compositor scheduling.
    pub(in crate::app) win: WindowRuntime,
    /// Input, view, tools and every transient editing session.
    pub(in crate::app) edit: EditorInteraction,
    /// Background work: loads, RAW/PDF pipelines, AI, extension bridge.
    pub(in crate::app) jobs: BackgroundJobs,
    /// The Develop window's session state.
    pub(in crate::app) dev: DevelopShell,
    /// Dialogs, panels, preferences, presets, print/proof settings.
    pub(in crate::app) shell: UiShell,
    /// The lightweight library: recent-files catalog + thumbnail cache.
    pub(in crate::app) lib: crate::app::library::LibraryShell,
}

impl App {
    pub fn new() -> Self {
        Self {
            docs: DocumentSession {
                documents: vec![Document::new(
                    DocumentId(1),
                    crate::core::DEFAULT_W,
                    crate::core::DEFAULT_H,
                )],
                active_doc_idx: 0,
                doc_mru: vec![DocumentId(1)],
                current_file: None,
                next_doc_id: 2,
                pending_close_doc_idx: None,
                pending_exit_docs: std::collections::VecDeque::new(),
                next_pdf_group_id: 1,
                pdf_render_services: std::collections::HashMap::new(),
                last_autosave: std::time::Instant::now(),
                autosave_files: std::collections::HashMap::new(),
                embedded_pdf_files: std::collections::HashMap::new(),
                crash_recovery_checked: false,
            },
            win: WindowRuntime {
                window: None,
                window_visible: false,
                window_focused: false,
                startup_focus_until: None,
                window_occluded: false,
                text_input_quiet_until: None,
                develop_window: None,
                retiring_develop_window: None,
                develop_egui_ctx: None,
                develop_egui_state: None,
                gpu: None,
                egui_ctx: egui::Context::default(),
                egui_state: None,
                ui_fonts: None,
                startup_phase: StartupPhase::Loading("Đang khởi động...".to_string()),
                startup_rx: None,
                startup_log: Vec::new(),
                cursor_ring: None,
                cursor_crosshair: None,
                cursor_selection_crosshair: None,
                cursor_perspective_crosshair: None,
                cursor_lasso: None,
                cursor_crop: None,
                cursor_eyedropper: None,
                cursor_fill: None,
                cursor_gradient: None,
                cursor_hand: None,
                cursor_pen: None,
                cursor_small_ring: None,
                cursor_zoom_in: None,
                cursor_zoom_out: None,
                last_cursor_radius: 0,
                start_time: Instant::now(),
                pending_gpu_sync: crate::core::canvas::DirtyRegion::default(),
                pending_gpu_sync_layer_id: usize::MAX,
                last_selection_push: Instant::now(),
                last_ants_frame: Instant::now(),
                cached_egui_primitives: Vec::new(),
                ants_redraw_pending: false,
                theme_applied: None,
                egui_repaint_deadline: Some(Instant::now()),
                last_uploaded_mask_key: u64::MAX,
                cpu_patch_buf: Vec::new(),
                viewport_buf: Vec::new(),
                viewport_patch_buf: Vec::new(),
                pending_view_change: false,
                view_recompose_last: None,
                view_recompose_cost: std::time::Duration::ZERO,
                view_recompose_deadline: None,
                interactive_recompose_pending: false,
                interactive_recompose_last: None,
                interactive_recompose_cost: std::time::Duration::ZERO,
                rendering: false,
            },
            edit: EditorInteraction {
                tools: ToolManager::new(),
                fg_color: [0, 0, 0, 255],
                bg_color: [255, 255, 255, 255],
                view: ViewState {
                    offset_x: 0.0,
                    offset_y: 0.0,
                    zoom: 1.0,
                },
                input: InputState {
                    mouse_x: 0.0,
                    mouse_y: 0.0,
                    last_mouse_x: 0.0,
                    last_mouse_y: 0.0,
                    painting: false,
                    mid_dragging: false,
                    is_over_ui: false,
                    was_over_ui: false,
                    space_held: false,
                    space_dragging: false,
                    alt_held: false,
                    ctrl_held: false,
                    shift_held: false,
                    eyedropping: false,
                    paint_dialog_hovered: false,
                    alt_right_dragging: false,
                    warp_resizing: false,
                    alt_drag_start_x: 0.0,
                    alt_drag_start_y: 0.0,
                    alt_drag_start_size: 20.0,
                    zoom_dragging: false,
                    zoom_drag_moved: false,
                    zoom_drag_start_x: 0.0,
                    zoom_drag_start_y: 0.0,
                    zoom_drag_anchor_x: 0.0,
                    zoom_drag_anchor_y: 0.0,
                    zoom_drag_start_zoom: 1.0,
                    last_left_release_time: None,
                    in_ui_chrome: false,
                },
                selection_mode: crate::core::selection::SelectionMode::Add,
                pending_stroke_inputs: Vec::new(),
                pending_fill: None,
                pending_stroke: None,
                guide_op: None,
                transform_snap_guides: Vec::new(),
                pending_transform_commit: None,
                warp_after_transform_commit: false,
                clipboard: None,
                clipboard_image_new_doc_hint: None,
                os_clipboard_written: None,
                pending_opacity_cmd: None,
                transform_state: None,
                warp_state: None,
                adjustment_layer_edit: None,
                text_edit: None,
                shape_drag: None,
                path_transform: None,
                path_pivot: None,
                path_pivot_dragging: false,
                path_pivot_snap: None,
                last_repeat_transform: None,
                path_dup_before: None,
                path_gradient_drag: None,
                node_drag: None,
                node_selected: None,
                node_multi: Vec::new(),
                node_marquee: None,
                pending_path_style: None,
                shape_style_pending: None,
                text_font_px: 48.0,
                text_font_px_auto: true,
                text_font_family: crate::core::text::TextFontFamily::SegoeUi,
                text_color: [0, 0, 0, 255],
                text_align: crate::core::text::TextAlign::Left,
                text_bold: false,
                text_italic: false,
                text_underline: false,
                text_line_height: 1.2,
                text_tracking_px: 0.0,
                text_opacity: 1.0,
                text_focus_pending: false,
                text_fonts_registered: std::collections::HashSet::new(),
                text_fonts_failed: std::collections::HashSet::new(),
                text_font_preview: None,
                transform_ctx_menu_pos: None,
                brush_popup_pos: None,
                selection_ctx_menu_pos: None,
                text_drag_hovered: false,
                text_panel_hovered: false,
                show_refine_panel: false,
                refine_feather: 0.0,
                refine_smooth: 0,
                refine_smart_radius: 0.0,
                refine_shift_edge: 0.0,
                refine_contrast: 0.0,
                refine_decontaminate: false,
                refine_decontaminate_amount: 0.5,
                refine_snapshot: Vec::new(),
                refine_dirty: false,
                refine_prev_tool: crate::tools::ToolId::Brush,
                refine_view_mode: RefineViewMode::Overlay,
                refine_overlay_color: [210, 30, 30, 190],
                refine_output_mode: RefineOutputMode::Selection,
                refine_overlay_tex: None,
                refine_overlay_mask_rev: u64::MAX,
            },
            jobs: BackgroundJobs {
                bus: create_bus(),
                format_registry: FormatRegistry::new(),
                pending_file_dialog: None,
                pending_loads: Vec::new(),
                pending_raw_previews: Vec::new(),
                raw_preview_docs: std::collections::HashMap::new(),
                raw_preview_failures: std::collections::HashMap::new(),
                cancelled_raw_loads: std::collections::HashSet::new(),
                loading_keys: std::collections::HashSet::new(),
                load_activate_pending: false,
                pending_pdf_probe_queue: std::collections::VecDeque::new(),
                pending_pdf_probe: None,
                pending_pdf_prompt: None,
                pending_pdf_render: None,
                pdf_render_total_pages: 0,
                pdf_render_group_id: 0,
                pdf_render_source: PathBuf::new(),
                pdf_render_selected_pages: Vec::new(),
                pdf_render_target_dpi: 300.0,
                pending_pdf_page_render: None,
                pending_reload_prompt: None,
                pending_reload_job: None,
                pending_iai_projects: Vec::new(),
                pending_printer_refresh: None,
                shape_bake: None,
                path_bake: None,
                path_bake_next: None,
                display_bake: None,
                display_bake_next: None,
                select_subject: crate::core::select_subject::SelectSubjectEngine::new(),
                ai_engine: crate::core::ai::edit::AiEditEngine::new(),
                ext: crate::app::ext_bridge::ExtBridge::new(),
            },
            dev: DevelopShell {
                develop_view_zoom: 1.0,
                develop_view_off: (0.0, 0.0),
                develop_view_fit: true,
                develop_cursor: (0.0, 0.0),
                develop_pan_drag: None,
                develop_tool: crate::app::develop_shell::DevelopTool::default(),
                develop_composited_view: None,
                develop_preview: None,
                develop_histogram: None,
                develop_histogram_at: None,
                develop_histogram_stale: false,
                develop_readout: None,
                develop_sections_open: crate::ui::develop::load_sections_open(),
                develop_local_drag: None,
                develop_session: Vec::new(),
                develop_bake_all: None,
                develop_thumbs: std::collections::HashMap::new(),
                pending_develop: Vec::new(),
                develop_gpu_preview_dirty: false,
                develop_gpu_preview_immediate: false,
                develop_gpu_recompose_last: None,
                develop_gpu_recompose_cost: std::time::Duration::ZERO,
                develop_proxy_cache: None,
                develop_proxy_last: None,
                develop_proxy_cost: std::time::Duration::ZERO,
            },
            shell: UiShell {
                ui_data_cache: UiDataCache::default(),
                ui: UiState {
                    show_welcome: true,
                    show_library: false,
                    show_new_dialog: false,
                    show_resize_dialog: false,
                    show_image_size_dialog: false,
                    show_rename_dialog: false,
                    show_export_dialog: false,
                    show_print_dialog: false,
                    show_preferences: false,
                    show_adjustment_dialog: false,
                    adjustment_dialog: crate::core::layer::AdjustmentType::default_levels(),
                    adjustment_preview_enabled: true,
                    adjustment_options: crate::ui::dialogs::load_adjustment_options(),
                    adj_eyedropper: None,
                    show_develop_dialog: false,
                    develop_settings: crate::core::develop::DevelopSettings::default(),
                    develop_local_selected: None,
                    develop_local_arm: None,
                    show_warp_dialog: false,
                    warp_params: crate::core::warp::WarpParams::default(),
                    show_filter_dialog: false,
                    filter_dialog: crate::core::filters::FilterType::GaussianBlur { radius: 2.0 },
                    filter_preview_enabled: true,
                    show_smart_fill_dialog: false,
                    show_gradient_editor: false,
                    show_ai_panel: false,
                    ai: crate::core::ai::AiPanelState::default(),
                    ai_status: String::new(),
                    show_exit_dialog: false,
                    theme_mode: crate::ui::theme::load_theme_mode(),
                    show_close_dialog: false,
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
                    export_format: crate::formats::ExportFormat::Png { compression: 6 },
                    export_embed_icc: true,
                    transform_interpolation: InterpolationMode::Bilinear,
                    // Color & Brush is now a floating panel opened on demand
                    // (Window ▸ Color Panel), like the Levels dialog. Quick
                    // colours are always available in the right-edge strip.
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
                    show_preset_dialog: false,
                    show_delete_preset_dialog: false,
                    preset_dialog_name: String::new(),
                    preset_dialog_w: 0.0,
                    preset_dialog_h: 0.0,
                    preset_dialog_unit: "px".to_string(),
                    preset_dialog_dpi: 72.0,
                    show_refine_color_dialog: false,
                    refine_color_dialog_color: [210, 30, 30, 190],
                    refine_color_dialog_original: [210, 30, 30, 190],
                    refine_color_dialog_live_preview: true,
                    refine_color_dialog_center_next: false,
                    show_paint_color_dialog: false,
                    paint_color_dialog_target: 0,
                    paint_color_dialog_color: [0, 0, 0, 255],
                    paint_color_dialog_original: [0, 0, 0, 255],
                    paint_color_dialog_live_preview: true,
                    paint_color_dialog_center_next: false,
                    modal_flash_until: None,
                    show_cmyk_convert_dialog: false,
                    cmyk_convert_use_icc: false,
                    cmyk_convert_icc: None,
                },
                status_msg: String::new(),
                exit_requested: false,
                exit_save_pending: false,
                close_requested: false,
                canvas_unit: crate::core::units::Unit::Pixels,
                toolbar_w: 48.0,
                // Layer/Channels width (260) plus the Corel-style vertical
                // colour strip (VECTOR_PALETTE_STRIP_W = 40) that lives at the
                // right edge of this band. Canvas layout reserves the whole width.
                panel_r_w: 300.0,
                proof_enabled: false,
                proof_target: ProofTarget::default(),
                proof_gamut_warn: false,
                display_cms_enabled: false,
                display_profile: None,
                display_profile_name: String::new(),
                print_layout: crate::core::print::PrintLayout::default(),
                print_printers: Vec::new(),
                print_selected_printer: String::new(),
                print_copies: 1,
                print_printer_profile: None,
                print_printer_profile_name: String::new(),
                adjustment_preview: None,
                adjustment_preview_pending: None,
                adjustment_preview_last: None,
                adjustment_preview_cost: std::time::Duration::ZERO,
                filter_preview: None,
                user_presets: std::sync::Arc::new(crate::core::presets::SizePreset::load_all()),
                develop_presets: std::sync::Arc::new(
                    crate::core::presets::DevelopPreset::load_all(),
                ),
                adjustment_presets: std::sync::Arc::new(
                    crate::core::presets::AdjustmentPresets::load(),
                ),
            },
            lib: crate::app::library::LibraryShell::new(),
        }
    }

    /// One-time startup work that needs a constructed `App`. Kicks off the
    /// printer enumeration (a ~2-3s hidden PowerShell query) on its worker
    /// thread now, so the Print dialog opens with the list already loaded.
    pub fn start_background_init(&mut self) {
        self.refresh_printer_list();
    }

    /// Guide that should appear "active" (highlighted + resize cursor): the one
    /// being dragged, else the one under the cursor while the Move tool is active.
    pub(crate) fn active_hover_guide(&self) -> Option<usize> {
        if let Some(GuideOp::Move { idx, .. }) = self.edit.guide_op {
            return Some(idx);
        }
        if self.edit.tools.active_id() == ToolId::Move {
            return self.guide_at_screen();
        }
        None
    }

    /// For the borderless window: if the cursor sits in the resize border of a
    /// NON-maximized window, the system resize direction for that edge/corner.
    /// `None` when maximized or away from the edge (normal canvas/UI interaction).
    pub fn resize_direction(&self) -> Option<winit::window::ResizeDirection> {
        use winit::window::ResizeDirection;
        let win = self.win.window.as_ref()?;
        if win.is_maximized() {
            return None;
        }
        let sz = win.inner_size();
        let (w, h) = (sz.width as f32, sz.height as f32);
        let x = self.edit.input.mouse_x;
        let y = self.edit.input.mouse_y;
        const B: f32 = 6.0;
        let l = x <= B;
        let r = x >= w - B;
        let t = y <= B;
        let b = y >= h - B;
        Some(match (t, b, l, r) {
            (true, _, true, _) => ResizeDirection::NorthWest,
            (true, _, _, true) => ResizeDirection::NorthEast,
            (_, true, true, _) => ResizeDirection::SouthWest,
            (_, true, _, true) => ResizeDirection::SouthEast,
            (true, ..) => ResizeDirection::North,
            (_, true, ..) => ResizeDirection::South,
            (_, _, true, _) => ResizeDirection::West,
            (_, _, _, true) => ResizeDirection::East,
            _ => return None,
        })
    }

    /// The matching resize cursor for a `ResizeDirection`.
    pub fn resize_cursor(dir: winit::window::ResizeDirection) -> winit::window::CursorIcon {
        use winit::window::{CursorIcon, ResizeDirection};
        match dir {
            ResizeDirection::North => CursorIcon::NResize,
            ResizeDirection::South => CursorIcon::SResize,
            ResizeDirection::East => CursorIcon::EResize,
            ResizeDirection::West => CursorIcon::WResize,
            ResizeDirection::NorthEast => CursorIcon::NeResize,
            ResizeDirection::NorthWest => CursorIcon::NwResize,
            ResizeDirection::SouthEast => CursorIcon::SeResize,
            ResizeDirection::SouthWest => CursorIcon::SwResize,
        }
    }

    pub fn tool_event(&self) -> PointerEvent {
        use crate::core::selection::SelectionMode;
        let sx = self.edit.input.mouse_x;
        let sy = self.edit.input.mouse_y;
        let cx = (sx - self.edit.view.offset_x) / self.edit.view.zoom;
        let cy = (sy - self.edit.view.offset_y) / self.edit.view.zoom;
        let selection_mode = if self.edit.input.shift_held && self.edit.input.alt_held {
            SelectionMode::Intersect
        } else if self.edit.input.shift_held {
            SelectionMode::Add
        } else if self.edit.input.alt_held {
            SelectionMode::Subtract
        } else {
            self.edit.selection_mode
        };
        PointerEvent {
            canvas_x: cx,
            canvas_y: cy,
            screen_x: sx,
            screen_y: sy,
            pressure: 1.0,
            shift: self.edit.input.shift_held,
            ctrl: self.edit.input.ctrl_held,
            alt: self.edit.input.alt_held,
            space: self.edit.input.space_held,
            selection_mode,
        }
    }

    pub fn make_ring_cursor(
        event_loop: &ActiveEventLoop,
        radius: u32,
    ) -> winit::window::CustomCursor {
        let r = radius.max(2);
        let size = (r * 2 + 6) as usize;
        let cx = size as f32 / 2.0;
        let cy = size as f32 / 2.0;
        let rf = r as f32;
        let mut rgba = vec![0u8; size * size * 4];
        for y in 0..size {
            for x in 0..size {
                let dx = x as f32 + 0.5 - cx;
                let dy = y as f32 + 0.5 - cy;
                let dist = (dx * dx + dy * dy).sqrt();
                let i = (y * size + x) * 4;
                let wd = (dist - (rf - 1.5)).abs();
                if wd < 1.0 {
                    let a = ((1.0 - wd) * 180.0) as u8;
                    rgba[i] = 255;
                    rgba[i + 1] = 255;
                    rgba[i + 2] = 255;
                    rgba[i + 3] = a;
                }
                let bd = (dist - rf).abs();
                if bd < 1.2 {
                    let a = ((1.0 - bd / 1.2) * 230.0) as u8;
                    rgba[i] = 0;
                    rgba[i + 1] = 0;
                    rgba[i + 2] = 0;
                    rgba[i + 3] = a;
                }
            }
        }
        let src = winit::window::CustomCursor::from_rgba(
            rgba,
            size as u16,
            size as u16,
            size as u16 / 2,
            size as u16 / 2,
        )
        .expect("ring cursor: rgba data is always valid (constructed inline)");
        event_loop.create_custom_cursor(src)
    }

    /// Thin crosshair cursor (white with black outline, center gap) for selection/lasso tools.
    pub fn make_crosshair_cursor(event_loop: &ActiveEventLoop) -> winit::window::CustomCursor {
        let size = 32usize;
        let center = size / 2;
        let gap = 4usize;
        let mut rgba = vec![0u8; size * size * 4];

        for i in 0..size {
            let in_gap = i >= center.saturating_sub(gap) && i <= center + gap;
            if in_gap {
                continue;
            }

            for (off, col, alpha) in [
                (-2i32, 0u8, 200u8),
                (-1, 255, 230),
                (0, 255, 240),
                (1, 255, 230),
                (2, 0, 200),
            ] {
                let y = (center as i32 + off) as usize;
                if y < size {
                    let idx = (y * size + i) * 4;
                    rgba[idx] = col;
                    rgba[idx + 1] = col;
                    rgba[idx + 2] = col;
                    rgba[idx + 3] = alpha;
                }
            }

            for (off, col, alpha) in [
                (-2i32, 0u8, 200u8),
                (-1, 255, 230),
                (0, 255, 240),
                (1, 255, 230),
                (2, 0, 200),
            ] {
                let x = (center as i32 + off) as usize;
                if x < size {
                    let idx = (i * size + x) * 4;
                    rgba[idx] = col;
                    rgba[idx + 1] = col;
                    rgba[idx + 2] = col;
                    rgba[idx + 3] = alpha;
                }
            }
        }

        let src = winit::window::CustomCursor::from_rgba(
            rgba,
            size as u16,
            size as u16,
            center as u16,
            center as u16,
        )
        .expect("crosshair cursor: always valid");
        event_loop.create_custom_cursor(src)
    }

    /// can't be rasterized (should never happen — the font is embedded).
    /// Tiny marquee cursor for selection tools.
    pub fn make_selection_cursor(event_loop: &ActiveEventLoop) -> winit::window::CustomCursor {
        let size = 19usize;
        let center = size / 2;
        let gap = 2usize;
        let mut rgba = vec![0u8; size * size * 4];

        for i in 0..size {
            let in_gap = i >= center.saturating_sub(gap) && i <= center + gap;
            if in_gap {
                continue;
            }

            for (off, col, alpha) in [(-1i32, 0u8, 210u8), (0, 255, 245), (1, 0, 210)] {
                let y = center as i32 + off;
                if y >= 0 && (y as usize) < size {
                    let idx = (y as usize * size + i) * 4;
                    rgba[idx] = col;
                    rgba[idx + 1] = col;
                    rgba[idx + 2] = col;
                    rgba[idx + 3] = alpha;
                }

                let x = center as i32 + off;
                if x >= 0 && (x as usize) < size {
                    let idx = (i * size + x as usize) * 4;
                    rgba[idx] = col;
                    rgba[idx + 1] = col;
                    rgba[idx + 2] = col;
                    rgba[idx + 3] = alpha;
                }
            }
        }

        let src = winit::window::CustomCursor::from_rgba(
            rgba,
            size as u16,
            size as u16,
            center as u16,
            center as u16,
        )
        .expect("selection cursor: always valid");
        event_loop.create_custom_cursor(src)
    }

    /// A 15×15, one-pixel crosshair. Alternating dark/light pixels keep the
    /// single thin stroke visible over both bright and dark image regions.
    pub fn make_perspective_cursor(event_loop: &ActiveEventLoop) -> winit::window::CustomCursor {
        let size = 15usize;
        let center = size / 2;
        let mut rgba = vec![0u8; size * size * 4];

        for i in 0..size {
            if i.abs_diff(center) <= 1 {
                continue;
            }
            let value = if i % 2 == 0 { 20 } else { 245 };
            for (x, y) in [(i, center), (center, i)] {
                let idx = (y * size + x) * 4;
                rgba[idx] = value;
                rgba[idx + 1] = value;
                rgba[idx + 2] = value;
                rgba[idx + 3] = 255;
            }
        }

        let src = winit::window::CustomCursor::from_rgba(
            rgba,
            size as u16,
            size as u16,
            center as u16,
            center as u16,
        )
        .expect("perspective cursor: always valid");
        event_loop.create_custom_cursor(src)
    }

    /// Tiny crosshair for the gradient tool (identical to selection cursor for now, but decoupled).
    pub fn make_gradient_cursor(event_loop: &ActiveEventLoop) -> winit::window::CustomCursor {
        let size = 19usize;
        let center = size / 2;
        let gap = 2usize;
        let mut rgba = vec![0u8; size * size * 4];

        for i in 0..size {
            let in_gap = i >= center.saturating_sub(gap) && i <= center + gap;
            if in_gap {
                continue;
            }

            for (off, col, alpha) in [(-1i32, 0u8, 210u8), (0, 255, 245), (1, 0, 210)] {
                let y = center as i32 + off;
                if y >= 0 && (y as usize) < size {
                    let idx = (y as usize * size + i) * 4;
                    rgba[idx] = col;
                    rgba[idx + 1] = col;
                    rgba[idx + 2] = col;
                    rgba[idx + 3] = alpha;
                }

                let x = center as i32 + off;
                if x >= 0 && (x as usize) < size {
                    let idx = (i * size + x as usize) * 4;
                    rgba[idx] = col;
                    rgba[idx + 1] = col;
                    rgba[idx + 2] = col;
                    rgba[idx + 3] = alpha;
                }
            }
        }

        let src = winit::window::CustomCursor::from_rgba(
            rgba,
            size as u16,
            size as u16,
            center as u16,
            center as u16,
        )
        .expect("gradient cursor: always valid");
        event_loop.create_custom_cursor(src)
    }

    /// standard lasso cursor: a small arrow plus the lasso tool mark.
    pub fn make_lasso_cursor(event_loop: &ActiveEventLoop) -> winit::window::CustomCursor {
        const W: usize = 32;
        const H: usize = 32;
        let mut rgba = vec![0u8; W * H * 4];

        let arrow_outline = [(1.0, 1.0), (3.5, 13.0), (11.0, 6.0)];
        let arrow_fill = [(1.65, 1.9), (3.9, 11.4), (9.4, 6.2)];
        Self::fill_cursor_polygon(&mut rgba, W, H, &arrow_outline, [255, 255, 255, 245]);
        Self::fill_cursor_polygon(&mut rgba, W, H, &arrow_fill, [0, 0, 0, 255]);

        if Self::paint_cursor_glyph(&mut rgba, W, H, egui_phosphor::regular::LASSO, 15.0, 10, 11)
            .is_err()
        {
            return Self::make_selection_cursor(event_loop);
        }

        let src = winit::window::CustomCursor::from_rgba(rgba, W as u16, H as u16, 2, 2)
            .expect("lasso cursor: rgba data is always valid (constructed inline)");
        event_loop.create_custom_cursor(src)
    }

    fn fill_cursor_polygon(
        rgba: &mut [u8],
        w: usize,
        h: usize,
        points: &[(f32, f32)],
        color: [u8; 4],
    ) {
        for y in 0..h {
            for x in 0..w {
                if Self::cursor_point_in_polygon(x as f32 + 0.5, y as f32 + 0.5, points) {
                    Self::blend_cursor_pixel(rgba, w, x, y, color);
                }
            }
        }
    }

    fn cursor_point_in_polygon(x: f32, y: f32, points: &[(f32, f32)]) -> bool {
        let mut inside = false;
        let mut j = points.len().saturating_sub(1);
        for i in 0..points.len() {
            let (xi, yi) = points[i];
            let (xj, yj) = points[j];
            let denom = yj - yi;
            if ((yi > y) != (yj > y)) && denom.abs() > 0.0001 {
                if x < (xj - xi) * (y - yi) / denom + xi {
                    inside = !inside;
                }
            }
            j = i;
        }
        inside
    }

    fn paint_cursor_glyph(
        rgba: &mut [u8],
        w: usize,
        h: usize,
        glyph: &str,
        px: f32,
        x0: usize,
        y0: usize,
    ) -> Result<(), ()> {
        use ab_glyph::Font;

        let ch = glyph.chars().next().ok_or(())?;
        let font = ab_glyph::FontRef::try_from_slice(egui_phosphor::Variant::Regular.font_bytes())
            .map_err(|_| ())?;
        let scale = ab_glyph::PxScale::from(px);
        let outlined = font
            .outline_glyph(font.glyph_id(ch).with_scale(scale))
            .ok_or(())?;
        let bounds = outlined.px_bounds();
        let gw = bounds.width().ceil().max(1.0) as usize;
        let gh = bounds.height().ceil().max(1.0) as usize;
        let mut cov = vec![0f32; gw * gh];
        outlined.draw(|gx, gy, c| {
            let x = gx as usize;
            let y = gy as usize;
            if x < gw && y < gh {
                cov[y * gw + x] = c;
            }
        });

        for y in 0..gh {
            for x in 0..gw {
                let mut halo = 0f32;
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        let nx = x as i32 + dx;
                        let ny = y as i32 + dy;
                        if nx >= 0 && ny >= 0 && (nx as usize) < gw && (ny as usize) < gh {
                            halo = halo.max(cov[ny as usize * gw + nx as usize]);
                        }
                    }
                }
                let core = cov[y * gw + x];
                let total = halo.max(core);
                if total <= 0.0 {
                    continue;
                }
                let val = ((core / total) * 255.0).round() as u8;
                let a = (total * 235.0).round() as u8;
                let tx = x0 + x;
                let ty = y0 + y;
                if tx < w && ty < h {
                    Self::blend_cursor_pixel(rgba, w, tx, ty, [val, val, val, a]);
                }
            }
        }
        Ok(())
    }

    fn blend_cursor_pixel(rgba: &mut [u8], w: usize, x: usize, y: usize, src: [u8; 4]) {
        let i = (y * w + x) * 4;
        if i + 3 >= rgba.len() {
            return;
        }
        let sa = src[3] as f32 / 255.0;
        let da = rgba[i + 3] as f32 / 255.0;
        let out_a = sa + da * (1.0 - sa);
        if out_a <= 0.0 {
            return;
        }
        for c in 0..3 {
            let sc = src[c] as f32 / 255.0;
            let dc = rgba[i + c] as f32 / 255.0;
            rgba[i + c] = (((sc * sa + dc * da * (1.0 - sa)) / out_a) * 255.0).round() as u8;
        }
        rgba[i + 3] = (out_a * 255.0).round() as u8;
    }

    /// Crop cursor rasterized from the same Phosphor glyph used in the toolbar.
    pub fn make_crop_cursor(event_loop: &ActiveEventLoop) -> winit::window::CustomCursor {
        Self::rasterize_glyph_cursor(event_loop, egui_phosphor::regular::CROP, 20.0, false)
            .unwrap_or_else(|| Self::make_crosshair_cursor(event_loop))
    }

    /// Eyedropper cursor rasterized from the Phosphor toolbar glyph.
    pub fn make_eyedropper_cursor(event_loop: &ActiveEventLoop) -> winit::window::CustomCursor {
        Self::rasterize_glyph_cursor(event_loop, egui_phosphor::regular::EYEDROPPER, 20.0, true)
            .unwrap_or_else(|| Self::make_crosshair_cursor(event_loop))
    }

    pub fn make_fill_cursor(event_loop: &ActiveEventLoop) -> winit::window::CustomCursor {
        Self::rasterize_glyph_cursor(event_loop, egui_phosphor::regular::PAINT_BUCKET, 20.0, true)
            .unwrap_or_else(|| Self::make_crosshair_cursor(event_loop))
    }

    /// Magnifying-glass cursor for the Zoom tool (`+` for zoom-in, `−` for
    /// zoom-out). Hotspot at the glyph centre (the lens).
    pub fn make_zoom_cursor(
        event_loop: &ActiveEventLoop,
        zoom_in: bool,
    ) -> winit::window::CustomCursor {
        let glyph = if zoom_in {
            egui_phosphor::regular::MAGNIFYING_GLASS_PLUS
        } else {
            egui_phosphor::regular::MAGNIFYING_GLASS_MINUS
        };
        Self::rasterize_glyph_cursor(event_loop, glyph, 20.0, false)
            .unwrap_or_else(|| Self::make_crosshair_cursor(event_loop))
    }

    pub fn make_hand_cursor(event_loop: &ActiveEventLoop) -> winit::window::CustomCursor {
        // The HAND icon in phosphor doesn't have a sharp pointing tip at the bottom left like tools,
        // it's a hand. So we set tip_hotspot=false to use the center, or maybe tip_hotspot=false but
        // for Hand tool usually the hotspot is near the center/palm.
        Self::rasterize_glyph_cursor(event_loop, egui_phosphor::regular::HAND, 20.0, false)
            .unwrap_or_else(|| {
                // fallback to a dummy if needed, but it should not fail
                Self::make_crosshair_cursor(event_loop)
            })
    }

    /// Pen cursor: the toolbar's PEN_NIB glyph rotated 90° clockwise so the nib
    /// tip points up-left at 45° (like the Windows pointer), with the hotspot on
    /// the tip. White fill + 1-px dark halo so it reads on any background. The 15.0
    /// px scale matches the toolbar button's `RichText::size(15.0)`.
    pub fn make_pen_cursor(event_loop: &ActiveEventLoop) -> winit::window::CustomCursor {
        Self::rotated_glyph_cursor(event_loop, egui_phosphor::regular::PEN_NIB, 15.0)
            .unwrap_or_else(|| Self::make_crosshair_cursor(event_loop))
    }

    /// Rasterize a Phosphor glyph, rotate it 90° clockwise, and build a cursor
    /// whose hotspot is the up-left-most opaque pixel (the rotated tip). Same
    /// white-fill + dark-halo treatment as [`rasterize_glyph_cursor`].
    fn rotated_glyph_cursor(
        event_loop: &ActiveEventLoop,
        glyph: &str,
        px: f32,
    ) -> Option<winit::window::CustomCursor> {
        use ab_glyph::Font;

        let ch = glyph.chars().next()?;
        let font =
            ab_glyph::FontRef::try_from_slice(egui_phosphor::Variant::Regular.font_bytes()).ok()?;
        let scale = ab_glyph::PxScale::from(px);
        let outlined = font.outline_glyph(font.glyph_id(ch).with_scale(scale))?;
        let bounds = outlined.px_bounds();
        let gw = bounds.width().ceil() as usize;
        let gh = bounds.height().ceil() as usize;
        if gw == 0 || gh == 0 {
            return None;
        }

        // Tight upright coverage.
        let mut up = vec![0f32; gw * gh];
        outlined.draw(|gx, gy, c| {
            let (x, y) = (gx as usize, gy as usize);
            if x < gw && y < gh {
                up[y * gw + x] = c;
            }
        });

        // Rotate 90° clockwise: (sx, sy) -> (gh-1-sy, sx). New dims (gh × gw).
        let margin = 2usize;
        let w = gh + margin * 2;
        let h = gw + margin * 2;
        let mut cov = vec![0f32; w * h];
        for sy in 0..gh {
            for sx in 0..gw {
                let nx = gh - 1 - sy + margin;
                let ny = sx + margin;
                cov[ny * w + nx] = up[sy * gw + sx];
            }
        }

        // White fill + 1-px dark halo, tracking the up-left-most opaque pixel as
        // the tip / hotspot.
        let mut rgba = vec![0u8; w * h * 4];
        let mut hot = (margin as u16, margin as u16);
        let mut best = usize::MAX;
        for y in 0..h {
            for x in 0..w {
                let mut halo = 0f32;
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        let nx = x as i32 + dx;
                        let ny = y as i32 + dy;
                        if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h {
                            halo = halo.max(cov[ny as usize * w + nx as usize]);
                        }
                    }
                }
                let core = cov[y * w + x];
                let total = halo.max(core);
                if total <= 0.0 {
                    continue;
                }
                let val = ((core / total) * 255.0).round() as u8;
                let i = (y * w + x) * 4;
                rgba[i] = val;
                rgba[i + 1] = val;
                rgba[i + 2] = val;
                rgba[i + 3] = (total * 255.0).round() as u8;
                if total > 0.3 && x + y < best {
                    best = x + y;
                    hot = (x as u16, y as u16);
                }
            }
        }

        let src =
            winit::window::CustomCursor::from_rgba(rgba, w as u16, h as u16, hot.0, hot.1).ok()?;
        Some(event_loop.create_custom_cursor(src))
    }

    /// Rasterize a single icon-font (Phosphor) glyph into a CustomCursor: white
    /// fill with a 1-px black halo so it reads on any background. The hotspot is
    /// the bottom-left opaque pixel — i.e. the down-left-pointing dropper tip.
    fn rasterize_glyph_cursor(
        event_loop: &ActiveEventLoop,
        glyph: &str,
        px: f32,
        tip_hotspot: bool,
    ) -> Option<winit::window::CustomCursor> {
        use ab_glyph::Font;

        let ch = glyph.chars().next()?;
        let font =
            ab_glyph::FontRef::try_from_slice(egui_phosphor::Variant::Regular.font_bytes()).ok()?;
        let scale = ab_glyph::PxScale::from(px);
        let outlined = font.outline_glyph(font.glyph_id(ch).with_scale(scale))?;
        let bounds = outlined.px_bounds();

        let margin = 2usize;
        let gw = bounds.width().ceil() as usize;
        let gh = bounds.height().ceil() as usize;
        if gw == 0 || gh == 0 {
            return None;
        }
        let w = gw + margin * 2;
        let h = gh + margin * 2;

        let mut cov = vec![0f32; w * h];
        outlined.draw(|gx, gy, c| {
            let x = gx as usize + margin;
            let y = gy as usize + margin;
            if x < w && y < h {
                cov[y * w + x] = c;
            }
        });

        let mut rgba = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let mut halo = 0f32;
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        let nx = x as i32 + dx;
                        let ny = y as i32 + dy;
                        if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h {
                            halo = halo.max(cov[ny as usize * w + nx as usize]);
                        }
                    }
                }
                let core = cov[y * w + x];
                let total = halo.max(core);
                if total <= 0.0 {
                    continue;
                }
                let val = ((core / total) * 255.0).round() as u8;
                let i = (y * w + x) * 4;
                rgba[i] = val;
                rgba[i + 1] = val;
                rgba[i + 2] = val;
                rgba[i + 3] = (total * 255.0).round() as u8;
            }
        }

        let mut hot = ((w / 2) as u16, (h / 2) as u16);
        if tip_hotspot {
            hot = (margin as u16, (h - margin) as u16);
            'find: for y in (0..h).rev() {
                for x in 0..w {
                    if cov[y * w + x] > 0.3 {
                        hot = (x as u16, y as u16);
                        break 'find;
                    }
                }
            }
        }

        let src =
            winit::window::CustomCursor::from_rgba(rgba, w as u16, h as u16, hot.0, hot.1).ok()?;
        Some(event_loop.create_custom_cursor(src))
    }

    pub fn update_ring_cursor(&mut self, event_loop: &ActiveEventLoop) {
        let radius = (self.edit.tools.cursor_size() * self.edit.view.zoom)
            .round()
            .clamp(2.0, 400.0) as u32;
        if radius > MAX_NATIVE_RING_RADIUS {
            self.win.cursor_ring = None;
            self.win.last_cursor_radius = radius;
            return;
        }
        if radius != self.win.last_cursor_radius || self.win.cursor_ring.is_none() {
            self.win.cursor_ring = Some(Self::make_ring_cursor(event_loop, radius));
            self.win.last_cursor_radius = radius;
        }
    }

    pub fn is_modal_open(&self) -> bool {
        self.is_blocking_modal() || self.is_preview_dialog_open()
    }

    /// Modals that fully block canvas interaction (no zoom/pan/edit). Everything
    /// except the live-preview image dialogs (adjustment / filter).
    pub fn is_blocking_modal(&self) -> bool {
        self.shell.ui.show_welcome
            || self.shell.ui.show_new_dialog
            || self.shell.ui.show_resize_dialog
            || self.shell.ui.show_image_size_dialog
            || self.shell.ui.show_rename_dialog
            || self.shell.ui.show_export_dialog
            || self.shell.ui.show_preferences
            || self.shell.ui.show_exit_dialog
            || self.jobs.pending_reload_prompt.is_some()
            || self.jobs.pending_pdf_prompt.is_some()
            || self.jobs.pending_pdf_page_render.is_some()
            || self.shell.ui.show_refine_color_dialog
            || self.shell.ui.show_paint_color_dialog
    }

    /// True while an adjustment or filter dialog with a live canvas preview is
    /// open. These stay open while the user zooms/pans to inspect the result, so
    /// they do NOT block view navigation (only editing/painting).
    pub fn is_preview_dialog_open(&self) -> bool {
        self.shell.ui.show_adjustment_dialog
            || self.shell.ui.show_filter_dialog
            || self.shell.ui.show_develop_dialog
    }

    /// Bug 7: True when Crop (with an active selection) or Free Transform is active.
    /// All keyboard shortcuts except Escape/Enter are blocked, toolbar tool-switches
    /// are ignored, and the layer panel is disabled until the user confirms or cancels.
    pub fn is_tool_modal_active(&self) -> bool {
        self.edit.transform_state.is_some()
            || self.edit.pending_transform_commit.is_some()
            || self.edit.warp_state.is_some()
            || (self.edit.tools.active_id() == ToolId::Crop
                && self.edit.tools.crop().has_selection())
    }

    /// Strict modal lock (standard design-app behavior): while one of these
    /// operations or dialogs is open, features outside its scope are refused
    /// (see `deny_modal_action`) until the user commits or cancels. The
    /// welcome screen and the exit/close save prompts are exempt — they are
    /// themselves navigation/resolution surfaces.
    pub fn modal_lock_active(&self) -> bool {
        // The Develop stage's second OS window is modal over the main window
        // (Track D): while it is open the main window is soft-locked — canvas
        // actions are refused (bell) until Develop closes.
        self.win.develop_window.is_some()
            || self.is_tool_modal_active()
            || self.edit.text_edit.is_some()
            || self.edit.show_refine_panel
            || (self.edit.tools.active_id() == ToolId::PerspectiveCrop
                && self.edit.tools.perspective_crop().has_quad())
            || self.is_preview_dialog_open()
            || (self.is_blocking_modal()
                && !self.shell.ui.show_welcome
                && !self.shell.ui.show_exit_dialog
                && !self.shell.ui.show_close_dialog)
    }

    /// A user action outside the active modal operation's scope was refused:
    /// ring the system bell, flash the modal's Commit/Cancel controls and
    /// explain in the status bar.
    pub(crate) fn deny_modal_action(&mut self) {
        self.shell.status_msg =
            "Finish or cancel the current operation first (✓ / ✗ / Esc)".to_string();
        self.shell.ui.modal_flash_until =
            Some(Instant::now() + std::time::Duration::from_millis(1600));
        alert_beep();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Active operations that must be applied/cancelled before the app can exit.
    /// Plain tool selection is not enough to block exit; this is only for states
    /// with live edits, overlays, or modal previews that would otherwise be dropped.
    pub fn exit_blocking_operation(&self) -> Option<&'static str> {
        if self.edit.transform_state.is_some() || self.edit.pending_transform_commit.is_some() {
            return Some("Free Transform");
        }
        if self.edit.warp_state.is_some() {
            return Some("Warp");
        }
        if self.edit.show_refine_panel || self.shell.ui.show_refine_color_dialog {
            return Some("Refine Selection");
        }
        if self.edit.text_edit.is_some() {
            return Some("Text editing");
        }
        if self.is_preview_dialog_open() {
            return Some("live preview");
        }
        if self.edit.input.painting || !self.edit.pending_stroke_inputs.is_empty() {
            return Some("current stroke");
        }

        match self.edit.tools.active_id() {
            ToolId::Crop if self.edit.tools.crop().has_selection() => Some("Crop"),
            ToolId::PerspectiveCrop
                if self.edit.tools.perspective_crop().has_pending_placement()
                    || self.edit.tools.perspective_crop().has_quad() =>
            {
                Some("Perspective Crop")
            }
            ToolId::PolygonLasso
                if !self.edit.tools.polygon_lasso().preview_points().is_empty() =>
            {
                Some("Polygon Lasso")
            }
            ToolId::Pen
                if !self.edit.tools.pen().is_empty() || self.edit.tools.pen().is_closed() =>
            {
                Some("Pen path")
            }
            _ => None,
        }
    }

    pub fn block_exit_if_active_operation(&mut self) -> bool {
        let Some(name) = self.exit_blocking_operation() else {
            return false;
        };
        self.shell.status_msg = format!("Finish or cancel {name} before exiting");
        // A one-line status is easy to miss on a close gesture: also chime and
        // flash the modal's Commit/Cancel controls for a moment.
        self.shell.ui.modal_flash_until =
            Some(Instant::now() + std::time::Duration::from_millis(1600));
        alert_beep();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        true
    }

    /// Does the page the user is looking at hold unsaved changes?
    ///
    /// Scope note: this is the ACTIVE page, not the whole document — for a
    /// multi-page PDF session an edited-but-inactive page does not show here.
    /// That matches the flag this replaced (the old `App::is_modified` bool was
    /// assigned from `Document::active_is_modified`, which is why autosave had
    /// to OR it with the session's own unsaved-pages check). For the
    /// whole-document answer use [`Document::is_modified`].
    ///
    /// Derived from the active canvas's saved checkpoint. There is no
    /// `is_modified` field to keep in sync any more: the old bool lived on both
    /// `App` and `Document`, was assigned by ~100 call sites, and still reported
    /// "unsaved" after undoing back to the saved state.
    pub fn is_modified(&self) -> bool {
        self.docs
            .documents
            .get(self.docs.active_doc_idx)
            .is_some_and(|d| d.active_is_modified())
    }

    /// Anchor the active document to "clean" after a successful write.
    pub fn mark_active_saved(&mut self) {
        if let Some(doc) = self.docs.documents.get_mut(self.docs.active_doc_idx) {
            doc.mark_saved();
        }
    }

    /// Start an app exit from a user gesture (window close / File > Exit). Returns
    /// true only when the caller should exit immediately.
    pub fn request_app_exit(&mut self) -> bool {
        if self.block_exit_if_active_operation() {
            return false;
        }
        // Finalize edits before taking the snapshot of dirty tabs.
        if self.edit.text_edit.is_some() {
            self.commit_text_edit();
        }
        self.path_style_commit();
        self.docs.documents[self.docs.active_doc_idx].reconcile_pdf_page_modified();

        self.docs.pending_exit_docs = self
            .docs
            .documents
            .iter()
            .filter(|document| document.is_modified())
            .map(|document| document.id)
            .collect();
        if self.docs.pending_exit_docs.is_empty() {
            return true;
        }
        self.present_next_exit_document();
        false
    }

    /// Select and prompt the next still-dirty tab in an app-exit sweep.
    pub(crate) fn present_next_exit_document(&mut self) {
        while let Some(id) = self.docs.pending_exit_docs.front().copied() {
            let Some(idx) = self.docs.documents.iter().position(|doc| doc.id == id) else {
                self.docs.pending_exit_docs.pop_front();
                continue;
            };
            if !self.docs.documents[idx].is_modified() {
                self.docs.pending_exit_docs.pop_front();
                continue;
            }
            if idx != self.docs.active_doc_idx {
                self.switch_to_doc(idx);
            }
            self.shell.ui.show_exit_dialog = true;
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return;
        }
        self.shell.ui.show_exit_dialog = false;
        self.shell.exit_requested = true;
    }

    pub(crate) fn discard_current_exit_document(&mut self) {
        self.docs.pending_exit_docs.pop_front();
        self.shell.ui.show_exit_dialog = false;
        self.present_next_exit_document();
    }

    pub(crate) fn cancel_app_exit(&mut self) {
        self.docs.pending_exit_docs.clear();
        self.shell.exit_save_pending = false;
        self.shell.exit_requested = false;
        self.shell.ui.show_exit_dialog = false;
    }

    /// Returns a reference to the currently active document.
    /// To add tabs: change only the body of these two methods.
    #[allow(dead_code)]
    pub fn active_doc(&self) -> &crate::core::document::Document {
        &self.docs.documents[self.docs.active_doc_idx]
    }

    /// Returns a mutable reference to the currently active document.
    #[allow(dead_code)]
    pub fn active_doc_mut(&mut self) -> &mut crate::core::document::Document {
        &mut self.docs.documents[self.docs.active_doc_idx]
    }

    pub fn sync_cursor(&mut self, event_loop: &ActiveEventLoop) {
        if self.edit.input.alt_right_dragging {
            return;
        }
        // Warp is modal: the egui brush ring IS the cursor over the canvas, so hide
        // the OS pointer there — otherwise the previously-active tool's cursor (e.g. a
        // brush ring) is dragged into the Warp session.
        if self.edit.warp_state.is_some() {
            if let Some(w) = &self.win.window {
                let panning = self.edit.input.space_dragging
                    || self.edit.input.space_held
                    || self.edit.input.mid_dragging;
                if self.edit.input.was_over_ui {
                    w.set_cursor_visible(true);
                    w.set_cursor(CursorIcon::Default);
                } else if panning {
                    w.set_cursor_visible(true);
                    w.set_cursor(CursorIcon::Grab);
                } else {
                    w.set_cursor_visible(false);
                }
            }
            return;
        }

        let eyedrop_cursor = self.edit.tools.active_id() == ToolId::Eyedropper
            || (self.shell.ui.show_paint_color_dialog && !self.edit.input.was_over_ui)
            || (self.edit.input.alt_held
                && matches!(self.edit.tools.active_id(), ToolId::Brush | ToolId::Pencil));
        let small_ring_over_dialog = self.shell.ui.show_paint_color_dialog
            && self.edit.input.paint_dialog_hovered
            && matches!(
                self.edit.tools.active_id(),
                ToolId::Brush
                    | ToolId::Eraser
                    | ToolId::Pencil
                    | ToolId::Clone
                    | ToolId::Repair
                    | ToolId::SmartSelect
                    | ToolId::RefineBrush
                    | ToolId::Smudge
                    | ToolId::Dodge
                    | ToolId::Burn
            );

        let needs_native_ring = !self.edit.input.was_over_ui
            && !self.edit.input.space_dragging
            && !self.edit.input.space_held
            && !self.edit.input.mid_dragging
            && !eyedrop_cursor
            && !small_ring_over_dialog
            && matches!(
                self.edit.tools.active_id(),
                ToolId::Brush
                    | ToolId::Eraser
                    | ToolId::Pencil
                    | ToolId::Clone
                    | ToolId::Repair
                    | ToolId::SmartSelect
                    | ToolId::RefineBrush
                    | ToolId::Smudge
                    | ToolId::Dodge
                    | ToolId::Burn
                    // Vector Brush shows a size ring so the drawn width is visible.
                    | ToolId::VectorBrush
            );
        if needs_native_ring {
            self.update_ring_cursor(event_loop);
        }
        if eyedrop_cursor && self.win.cursor_eyedropper.is_none() {
            self.win.cursor_eyedropper = Some(Self::make_eyedropper_cursor(event_loop));
        }
        let needs_fill_cursor = !self.edit.input.was_over_ui
            && !self.edit.input.space_dragging
            && !self.edit.input.space_held
            && !self.edit.input.mid_dragging
            && matches!(self.edit.tools.active_id(), ToolId::Fill);
        if needs_fill_cursor && self.win.cursor_fill.is_none() {
            self.win.cursor_fill = Some(Self::make_fill_cursor(event_loop));
        }
        if small_ring_over_dialog && self.win.cursor_small_ring.is_none() {
            self.win.cursor_small_ring = Some(Self::make_ring_cursor(event_loop, 6));
        }

        let needs_selection_cursor = !self.edit.input.was_over_ui
            && !self.edit.input.space_dragging
            && !self.edit.input.space_held
            && !self.edit.input.mid_dragging
            && matches!(
                self.edit.tools.active_id(),
                ToolId::SelectionRect | ToolId::SelectionEllipse | ToolId::Arrow
            );
        if needs_selection_cursor && self.win.cursor_selection_crosshair.is_none() {
            self.win.cursor_selection_crosshair = Some(Self::make_selection_cursor(event_loop));
        }
        let needs_perspective_cursor = !self.edit.input.was_over_ui
            && !self.edit.input.space_dragging
            && !self.edit.input.space_held
            && !self.edit.input.mid_dragging
            && self.edit.tools.active_id() == ToolId::PerspectiveCrop;
        if needs_perspective_cursor && self.win.cursor_perspective_crosshair.is_none() {
            self.win.cursor_perspective_crosshair = Some(Self::make_perspective_cursor(event_loop));
        }
        let needs_gradient_cursor = !self.edit.input.was_over_ui
            && !self.edit.input.space_dragging
            && !self.edit.input.space_held
            && !self.edit.input.mid_dragging
            && matches!(self.edit.tools.active_id(), ToolId::Gradient);
        if needs_gradient_cursor && self.win.cursor_gradient.is_none() {
            self.win.cursor_gradient = Some(Self::make_gradient_cursor(event_loop));
        }

        let needs_lasso_cursor = !self.edit.input.was_over_ui
            && !self.edit.input.space_dragging
            && !self.edit.input.space_held
            && !self.edit.input.mid_dragging
            && matches!(
                self.edit.tools.active_id(),
                ToolId::Lasso | ToolId::PolygonLasso | ToolId::Patch
            );
        if needs_lasso_cursor && self.win.cursor_lasso.is_none() {
            self.win.cursor_lasso = Some(Self::make_lasso_cursor(event_loop));
        }

        let needs_crop_cursor = !self.edit.input.was_over_ui
            && !self.edit.input.space_dragging
            && !self.edit.input.space_held
            && !self.edit.input.mid_dragging
            && self.edit.tools.active_id() == ToolId::Crop;
        if needs_crop_cursor && self.win.cursor_crop.is_none() {
            self.win.cursor_crop = Some(Self::make_crop_cursor(event_loop));
        }

        let needs_crosshair = !self.edit.input.was_over_ui
            && !self.edit.input.space_dragging
            && !self.edit.input.space_held
            && !self.edit.input.mid_dragging
            && matches!(
                self.edit.tools.active_id(),
                ToolId::Eyedropper | ToolId::Shape
            );
        if needs_crosshair && self.win.cursor_crosshair.is_none() {
            self.win.cursor_crosshair = Some(Self::make_crosshair_cursor(event_loop));
        }

        let needs_zoom_cursor = !self.edit.input.was_over_ui
            && !self.edit.input.space_dragging
            && !self.edit.input.space_held
            && !self.edit.input.mid_dragging
            && self.edit.tools.active_id() == ToolId::Zoom;
        if needs_zoom_cursor {
            if self.win.cursor_zoom_in.is_none() {
                self.win.cursor_zoom_in = Some(Self::make_zoom_cursor(event_loop, true));
            }
            if self.win.cursor_zoom_out.is_none() {
                self.win.cursor_zoom_out = Some(Self::make_zoom_cursor(event_loop, false));
            }
        }

        let needs_pen_cursor = !self.edit.input.was_over_ui
            && !self.edit.input.space_dragging
            && !self.edit.input.space_held
            && !self.edit.input.mid_dragging
            && self.edit.tools.active_id() == ToolId::Pen;
        if needs_pen_cursor && self.win.cursor_pen.is_none() {
            self.win.cursor_pen = Some(Self::make_pen_cursor(event_loop));
        }

        let needs_hand_cursor = !self.edit.input.was_over_ui
            && (self.edit.input.space_dragging
                || self.edit.input.space_held
                || self.edit.tools.active_id() == ToolId::Hand
                || self.edit.input.mid_dragging);
        if needs_hand_cursor && self.win.cursor_hand.is_none() {
            self.win.cursor_hand = Some(Self::make_hand_cursor(event_loop));
        }

        if let Some(w) = &self.win.window {
            let over_fixed_chrome = self
                .ui_chrome_hit(self.edit.input.mouse_x, self.edit.input.mouse_y)
                .0;
            if over_fixed_chrome {
                w.set_cursor_visible(true);
                w.set_cursor(CursorIcon::Default);
                return;
            }

            // I-beam over the canvas/overlay while editing text, but not over
            // the window chrome — panels keep the normal arrow.
            if self.edit.text_edit.is_some()
                && self.edit.tools.active_id() == ToolId::Text
                // The colour dialog owns cursor semantics while open: normal
                // Windows/UI cursors over the dialog, eyedropper over canvas.
                && !self.shell.ui.show_paint_color_dialog
            {
                w.set_cursor_visible(true);
                if self.edit.text_panel_hovered {
                    w.set_cursor(CursorIcon::Default);
                } else if self.edit.text_drag_hovered {
                    w.set_cursor(CursorIcon::Move);
                } else {
                    w.set_cursor(CursorIcon::Text);
                }
                return;
            }

            if small_ring_over_dialog {
                if let Some(sr) = &self.win.cursor_small_ring {
                    w.set_cursor_visible(true);
                    w.set_cursor(sr.clone());
                    return;
                }
            }

            if self.edit.input.was_over_ui {
                w.set_cursor_visible(true);
                w.set_cursor(CursorIcon::Default);
                return;
            }

            if self.edit.input.space_dragging
                || self.edit.input.space_held
                || self.edit.input.mid_dragging
            {
                w.set_cursor_visible(true);
                if let Some(hc) = &self.win.cursor_hand {
                    w.set_cursor(hc.clone());
                } else {
                    w.set_cursor(CursorIcon::Grab);
                }
            } else if eyedrop_cursor {
                w.set_cursor_visible(true);
                if let Some(ed) = &self.win.cursor_eyedropper {
                    w.set_cursor(ed.clone());
                } else {
                    w.set_cursor(CursorIcon::Crosshair);
                }
            } else {
                match self.edit.tools.active_id() {
                    ToolId::Move => {
                        w.set_cursor_visible(true);
                        // Hovering / dragging a guide → axis resize cursor.
                        let guide_cursor = self.active_hover_guide().and_then(|i| {
                            self.docs.documents[self.docs.active_doc_idx]
                                .guides
                                .get(i)
                                .map(|g| match g.orientation {
                                    GuideOrientation::Horizontal => CursorIcon::NsResize,
                                    GuideOrientation::Vertical => CursorIcon::EwResize,
                                })
                        });
                        // A vector Path shows its transform box: resize/rotate over
                        // a handle, move over its fill. The native four-way Move
                        // cursor is visually heavy, so a plain layer body still uses
                        // the compact OS pointer; guide edges take priority.
                        let hint = {
                            let path = self.move_hover_hint();
                            if path != 0 {
                                path
                            } else {
                                self.move_selection_transform_cursor_hint(
                                    self.edit.input.mouse_x,
                                    self.edit.input.mouse_y,
                                )
                            }
                        };
                        let path_cursor = match hint {
                            2 => Some(CursorIcon::NwseResize),
                            3 => Some(CursorIcon::NeswResize),
                            4 => Some(CursorIcon::NsResize),
                            5 => Some(CursorIcon::EwResize),
                            6 => Some(CursorIcon::Grab),
                            1 => Some(CursorIcon::Move),
                            _ => None,
                        };
                        w.set_cursor(guide_cursor.or(path_cursor).unwrap_or(CursorIcon::Default));
                    }
                    ToolId::Hand => {
                        w.set_cursor_visible(true);
                        if let Some(hc) = &self.win.cursor_hand {
                            w.set_cursor(hc.clone());
                        } else {
                            w.set_cursor(CursorIcon::Grab);
                        }
                    }
                    ToolId::Text => {
                        w.set_cursor_visible(true);
                        w.set_cursor(CursorIcon::Text);
                    }
                    ToolId::Zoom => {
                        w.set_cursor_visible(true);
                        let want_out = self.edit.input.alt_held;
                        let custom = if want_out {
                            &self.win.cursor_zoom_out
                        } else {
                            &self.win.cursor_zoom_in
                        };
                        if let Some(c) = custom {
                            w.set_cursor(c.clone());
                        } else {
                            w.set_cursor(if want_out {
                                CursorIcon::ZoomOut
                            } else {
                                CursorIcon::ZoomIn
                            });
                        }
                    }
                    ToolId::Lasso | ToolId::PolygonLasso | ToolId::Patch => {
                        if let Some(lasso) = &self.win.cursor_lasso {
                            w.set_cursor_visible(true);
                            w.set_cursor(lasso.clone());
                        } else {
                            w.set_cursor_visible(true);
                            w.set_cursor(CursorIcon::Default);
                        }
                    }
                    ToolId::Crop => {
                        w.set_cursor_visible(true);
                        match self.crop_cursor_hint() {
                            1 => w.set_cursor(CursorIcon::Move),
                            2 => w.set_cursor(CursorIcon::NwseResize),
                            3 => w.set_cursor(CursorIcon::NeswResize),
                            4 => w.set_cursor(CursorIcon::NsResize),
                            5 => w.set_cursor(CursorIcon::EwResize),
                            0 => {
                                if let Some(crop) = &self.win.cursor_crop {
                                    w.set_cursor(crop.clone());
                                } else {
                                    w.set_cursor(CursorIcon::Default);
                                }
                            }
                            _ => {
                                if let Some(crop) = &self.win.cursor_crop {
                                    w.set_cursor(crop.clone());
                                } else {
                                    w.set_cursor(CursorIcon::Crosshair);
                                }
                            }
                        }
                    }
                    ToolId::PerspectiveCrop => {
                        w.set_cursor_visible(true);
                        match self.perspective_crop_cursor_hint() {
                            1 => w.set_cursor(CursorIcon::Move),
                            2 => w.set_cursor(CursorIcon::NsResize),
                            3 => w.set_cursor(CursorIcon::EwResize),
                            4 => w.set_cursor(CursorIcon::NwseResize),
                            5 => w.set_cursor(CursorIcon::NeswResize),
                            // Corner handles use the compact native OS arrow.
                            6 => w.set_cursor(CursorIcon::Default),
                            _ => {
                                if let Some(cursor) = &self.win.cursor_perspective_crosshair {
                                    w.set_cursor(cursor.clone());
                                } else {
                                    w.set_cursor(CursorIcon::Crosshair);
                                }
                            }
                        }
                    }
                    ToolId::Shape => {
                        w.set_cursor_visible(true);
                        match self.shape_cursor_hint() {
                            1 => w.set_cursor(CursorIcon::NwseResize),
                            2 => w.set_cursor(CursorIcon::NeswResize),
                            3 => w.set_cursor(CursorIcon::NsResize),
                            4 => w.set_cursor(CursorIcon::EwResize),
                            5 => w.set_cursor(CursorIcon::Default),
                            6 => w.set_cursor(CursorIcon::Move),
                            _ => {
                                if let Some(cc) = &self.win.cursor_selection_crosshair {
                                    w.set_cursor(cc.clone());
                                } else {
                                    w.set_cursor(CursorIcon::Crosshair);
                                }
                            }
                        }
                    }
                    ToolId::SelectionRect | ToolId::SelectionEllipse | ToolId::Arrow => {
                        if let Some(cc) = &self.win.cursor_selection_crosshair {
                            w.set_cursor_visible(true);
                            w.set_cursor(cc.clone());
                        } else {
                            w.set_cursor_visible(true);
                            w.set_cursor(CursorIcon::Crosshair);
                        }
                    }
                    ToolId::Gradient => {
                        if let Some(gc) = &self.win.cursor_gradient {
                            w.set_cursor_visible(true);
                            w.set_cursor(gc.clone());
                        } else {
                            w.set_cursor_visible(true);
                            w.set_cursor(CursorIcon::Crosshair);
                        }
                    }

                    ToolId::Fill => {
                        if let Some(fc) = &self.win.cursor_fill {
                            w.set_cursor_visible(true);
                            w.set_cursor(fc.clone());
                        } else {
                            w.set_cursor_visible(true);
                            w.set_cursor(CursorIcon::Crosshair);
                        }
                    }
                    ToolId::SmartSelect => {
                        if let Some(ring) = &self.win.cursor_ring {
                            w.set_cursor_visible(true);
                            w.set_cursor(ring.clone());
                        } else {
                            w.set_cursor_visible(false);
                        }
                    }
                    ToolId::Eyedropper => {
                        if let Some(cc) = &self.win.cursor_crosshair {
                            w.set_cursor_visible(true);
                            w.set_cursor(cc.clone());
                        } else {
                            w.set_cursor_visible(true);
                            w.set_cursor(CursorIcon::Crosshair);
                        }
                    }
                    ToolId::Pen => {
                        if self.edit.input.ctrl_held {
                            // Ctrl = edit mode → swap to an arrow so the user can see
                            // the modifier registered (Direct Selection).
                            w.set_cursor_visible(true);
                            w.set_cursor(CursorIcon::Default);
                        } else if let Some(pc) = &self.win.cursor_pen {
                            w.set_cursor_visible(true);
                            w.set_cursor(pc.clone());
                        } else {
                            w.set_cursor_visible(true);
                            w.set_cursor(CursorIcon::Crosshair);
                        }
                    }
                    ToolId::Transform => {
                        w.set_cursor_visible(true);
                        match self.transform_cursor_hint() {
                            0 => w.set_cursor(CursorIcon::Move),
                            1 => w.set_cursor(CursorIcon::Crosshair),
                            10 => w.set_cursor(CursorIcon::EwResize),
                            11 => w.set_cursor(CursorIcon::NwseResize),
                            12 => w.set_cursor(CursorIcon::NsResize),
                            13 => w.set_cursor(CursorIcon::NeswResize),
                            _ => w.set_cursor(CursorIcon::Default),
                        }
                    }
                    ToolId::RefineBrush => {
                        if let Some(ring) = &self.win.cursor_ring {
                            w.set_cursor_visible(true);
                            w.set_cursor(ring.clone());
                        } else {
                            w.set_cursor_visible(false);
                        }
                    }
                    ToolId::Node => {
                        // Direct-selection: a plain arrow, a Move cursor over an
                        // anchor, a crosshair over a segment (insert). Without this
                        // arm the Node tool fell through to the brush-ring default
                        // and HID the cursor over the canvas (nothing to test with).
                        w.set_cursor_visible(true);
                        match self.node_cursor_hint() {
                            4 => w.set_cursor(CursorIcon::Crosshair),
                            2 => w.set_cursor(CursorIcon::Move),
                            3 => w.set_cursor(CursorIcon::Crosshair),
                            _ => w.set_cursor(CursorIcon::Default),
                        }
                    }
                    _ => {
                        if let Some(ring) = &self.win.cursor_ring {
                            w.set_cursor_visible(true);
                            w.set_cursor(ring.clone());
                        } else {
                            w.set_cursor_visible(false);
                        }
                    }
                }
            }
        }
    }

    pub fn crop_cursor_hint(&self) -> u8 {
        use crate::tools::crop::CropHandle;

        let c = self.edit.tools.crop();
        if !c.has_selection() {
            return 10;
        }

        let zoom = self.edit.view.zoom;
        let vox = self.edit.view.offset_x;
        let voy = self.edit.view.offset_y;
        let mx = self.edit.input.mouse_x;
        let my = self.edit.input.mouse_y;

        let cx = (mx - vox) / zoom;
        let cy = (my - voy) / zoom;

        let handle = c.detect_handle(cx, cy, zoom);

        match handle {
            CropHandle::TopLeft | CropHandle::BottomRight => 2,
            CropHandle::TopRight | CropHandle::BottomLeft => 3,
            CropHandle::Top | CropHandle::Bottom => 4,
            CropHandle::Left | CropHandle::Right => 5,
            CropHandle::Move => 1,
            CropHandle::Rotate | CropHandle::None => 0,
        }
    }

    /// Cursor hint for the editable Perspective Crop quad.
    /// 0=crosshair, 1=move, 2=NS, 3=EW, 4=NW-SE, 5=NE-SW, 6=OS arrow.
    pub fn perspective_crop_cursor_hint(&self) -> u8 {
        use crate::tools::perspective_crop::PerspHandle;

        let tool = self.edit.tools.perspective_crop();
        if !tool.has_quad() {
            return 0;
        }

        let zoom = self.edit.view.zoom;
        let cx = (self.edit.input.mouse_x - self.edit.view.offset_x) / zoom;
        let cy = (self.edit.input.mouse_y - self.edit.view.offset_y) / zoom;
        match tool.detect_handle(cx, cy, zoom) {
            PerspHandle::Corner(_) => 6,
            PerspHandle::Move => 1,
            PerspHandle::Edge(edge_idx) => {
                let edge_corners = [(0usize, 1usize), (1, 2), (2, 3), (3, 0)];
                let (a_idx, b_idx) = edge_corners[edge_idx];
                let a = tool.corners[a_idx];
                let b = tool.corners[b_idx];
                let normal_angle = (b.0 - a.0)
                    .atan2(-(b.1 - a.1))
                    .rem_euclid(std::f32::consts::PI);
                let eighth = std::f32::consts::FRAC_PI_8;
                if normal_angle < eighth || normal_angle >= 7.0 * eighth {
                    3
                } else if normal_angle < 3.0 * eighth {
                    4
                } else if normal_angle < 5.0 * eighth {
                    2
                } else {
                    5
                }
            }
            PerspHandle::None => 0,
        }
    }
}

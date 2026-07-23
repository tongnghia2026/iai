// UI trung tam — nhan UiData tu app.rs, tra ve UiActions
// Them panel moi: tao file moi, goi trong build()
// KHONG bao gio import truc tiep tu app.rs

pub mod ai_panel;
pub mod ai_progress;
pub mod color_picker;
pub mod develop;
pub mod dialogs;
pub mod intent;
pub mod library;
pub mod menubar;
pub mod nav;
pub mod panels;
pub mod refine_select;
pub mod statusbar;
pub mod tabbar;
pub mod theme;
pub mod toolbar;
pub mod topoptions;
pub mod viewmodel;
pub mod warp;
pub mod welcome;
pub mod widgets;
pub use intent::*;
pub use viewmodel::*;

use crate::core::canvas::StrokeParams;
use crate::core::develop::DevelopSettings;
use crate::core::document::GuideOrientation;
use crate::core::filters::FilterType;
use crate::core::layer::{AdjustmentType, BlendMode, PaintTarget};
use crate::core::units::Unit;
use crate::formats::ExportFormat;
use crate::tools::ToolId;
use egui_phosphor::regular as ph;
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};

pub const CLONE_SOURCE_PREVIEW_MAX_SIZE: usize = 512;

// Canvas tool adornments must stay behind every floating window. In particular,
// keeping Crop on `Foreground` lets its border paint over dialogs that also use
// `Foreground` (Print, Levels, Curves, etc.), because same-order custom layers
// have no reliable stacking relationship with window layers.
const CANVAS_TOOL_OVERLAY_ORDER: egui::Order = egui::Order::Background;

pub(crate) fn document_side_dialog_pos(
    ctx: &egui::Context,
    data: &UiData,
    width: f32,
    y: f32,
) -> egui::Pos2 {
    let screen = ctx.content_rect();
    let left = data.chrome.toolbar_w + if data.chrome.show_rulers { 20.0 } else { 0.0 } + 12.0;
    let right = screen.max.x - data.chrome.panel_r_w - 12.0;
    let x = (right - width).clamp(left, (screen.max.x - width - 12.0).max(left));
    egui::pos2(x, y.max(screen.min.y + 48.0))
}

fn flat_context_menu_item(
    ui: &mut egui::Ui,
    width: f32,
    enabled: bool,
    label: &str,
    shortcut: Option<&str>,
) -> bool {
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 22.0), sense);
    let visuals = ui.visuals();
    if enabled && response.hovered() {
        ui.painter()
            .rect_filled(rect, 0.0, visuals.widgets.hovered.bg_fill);
    }

    let text_color = if enabled {
        visuals.text_color()
    } else {
        visuals.weak_text_color()
    };
    let shortcut_color = visuals.weak_text_color();
    let font = egui::FontId::proportional(12.0);
    ui.painter().text(
        egui::pos2(rect.left() + 10.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        font.clone(),
        text_color,
    );
    if let Some(shortcut) = shortcut {
        ui.painter().text(
            egui::pos2(rect.right() - 10.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            shortcut,
            font,
            shortcut_color,
        );
    }

    enabled && response.clicked()
}

fn flat_context_menu_separator(ui: &mut egui::Ui, width: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 7.0), egui::Sense::hover());
    let y = rect.center().y;
    ui.painter().line_segment(
        [
            egui::pos2(rect.left() + 8.0, y),
            egui::pos2(rect.right() - 8.0, y),
        ],
        egui::Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color),
    );
}

pub struct CloneSourcePreview {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

/// On-canvas editing overlay for a Shape layer. `span` is the bounding box
/// `[x0,y0,x1,y1]` and `handles` are `(handle_id, canvas_x, canvas_y)` where the
/// handle id follows `core::shape::ShapeHandle::to_u8` (8 = corner-radius node).
#[derive(Clone)]
pub struct ShapeOverlay {
    pub span: [f32; 4],
    pub kind: u8,
    /// Effective corner radius (canvas units; rectangles only, else 0).
    pub radius: f32,
    pub handles: Vec<(u8, f32, f32)>,
    /// True while a handle drag is in progress — the raster may lag behind
    /// (bakes are throttled on big shapes), so the overlay also draws the
    /// live shape outline.
    pub dragging: bool,
}

/// Current fill/outline style of the active Path layer, for the options-bar
/// Fill/Outline controls (Move / Node tools). `*_enabled` mirrors whether the
/// paint is present (`Paint::Solid`); the colour is the last solid colour (or
/// black) so a chip always shows something.
#[derive(Clone, Copy)]
pub struct PathStyleData {
    pub fill_enabled: bool,
    pub fill_color: [u8; 4],
    pub stroke_enabled: bool,
    pub stroke_color: [u8; 4],
    pub stroke_width: f32,
}

/// On-canvas editing overlay for the Node tool: the active Path's outline,
/// its anchor points, and the handle arms of the selected node. All positions
/// are in canvas space; the UI maps them to screen.
#[derive(Clone)]
pub struct NodeOverlay {
    /// Flattened contours (each a polyline) to draw as the path outline.
    pub outlines: Vec<Vec<(f32, f32)>>,
    /// Anchor points: `(canvas_x, canvas_y, selected)`.
    pub nodes: Vec<(f32, f32, bool)>,
    /// Bézier handle arms of the selected node: `[anchor_x, anchor_y, ctrl_x, ctrl_y]`.
    pub handles: Vec<[f32; 4]>,
    /// Active rubber-band selection rect in SCREEN space `[x0, y0, x1, y1]`, or
    /// `None` when not marquee-dragging.
    pub marquee: Option<[f32; 4]>,
}

#[derive(Clone)]
pub struct PathDisplayRaster {
    pub cache_key: u64,
    pub tiles: std::sync::Arc<Vec<PathDisplayTile>>,
    pub canvas_x: f32,
    pub canvas_y: f32,
    pub canvas_w: f32,
    pub canvas_h: f32,
    pub raster_w: u32,
    pub raster_h: u32,
}

#[derive(Clone)]
pub struct PathDisplayTile {
    pub rgba: std::sync::Arc<Vec<u8>>,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Warp freeze-mask snapshot for the red canvas overlay. `alpha` is the mesh
/// node grid (`gw × gh`, 0..255); the panel scales it over the layer's canvas rect.
pub struct WarpFreezeView {
    pub gw: usize,
    pub gh: usize,
    pub alpha: Vec<u8>,
    pub layer_x: f32,
    pub layer_y: f32,
    pub layer_w: f32,
    pub layer_h: f32,
}

/// Which Select ▸ Modify operation the shared modify dialog is editing.
#[derive(Clone, Copy, PartialEq)]
pub enum SelectionModifyKind {
    Expand,
    Contract,
    Smooth,
    Border,
}

impl SelectionModifyKind {
    pub fn title(self) -> &'static str {
        match self {
            SelectionModifyKind::Expand => "Expand Selection",
            SelectionModifyKind::Contract => "Contract Selection",
            SelectionModifyKind::Smooth => "Smooth Selection",
            SelectionModifyKind::Border => "Border Selection",
        }
    }
}

/// PDF navigator strip state for the active document (Some when it's a PDF page).
#[derive(Clone)]
pub struct PdfNavData {
    pub index: usize,
    pub count: usize,
    pub source_name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutoLevelsAlgorithm {
    Monochromatic,
    PerChannelContrast,
}

impl Default for AutoLevelsAlgorithm {
    fn default() -> Self {
        Self::PerChannelContrast
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AdjustmentOptions {
    pub auto_levels_algorithm: AutoLevelsAlgorithm,
    pub auto_clip_percent: f32,
}

impl Default for AdjustmentOptions {
    fn default() -> Self {
        Self {
            auto_levels_algorithm: AutoLevelsAlgorithm::PerChannelContrast,
            auto_clip_percent: 0.10,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdjEyedropperKind {
    Black,
    Gray,
    White,
}

#[derive(Clone, Debug)]
pub struct DocAiBusy {
    pub label: String,
    pub elapsed_secs: Option<u64>,
    pub queue_pos: Option<usize>,
}

/// Screen-space outline of the selected Develop local mask.
pub enum DevelopLocalOverlay {
    /// Gradient handles: full effect at `p0`, zero past `p1`.
    Linear { p0: (f32, f32), p1: (f32, f32) },
    /// Ellipse centre + screen-space radii.
    Radial { cx: f32, cy: f32, rx: f32, ry: f32 },
}

/// Draw the selected local mask's outline (shadowed white, PTS-style). Shared
/// by the main window's canvas overlay and the Develop OS window's viewport.
pub fn draw_develop_local_overlay(painter: &egui::Painter, ov: &DevelopLocalOverlay) {
    let shadow = egui::Stroke::new(3.0_f32, egui::Color32::from_black_alpha(160));
    let line = egui::Stroke::new(
        1.5_f32,
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 230),
    );
    match *ov {
        DevelopLocalOverlay::Linear { p0, p1 } => {
            let a = egui::pos2(p0.0, p0.1);
            let b = egui::pos2(p1.0, p1.1);
            let d = b - a;
            let len = d.length().max(1e-3);
            let n = egui::vec2(-d.y / len, d.x / len);
            // The two rails mark the full-effect and zero lines; long
            // enough to cross any viewport.
            let cross = 4000.0;
            for stroke in [shadow, line] {
                painter.line_segment([a, b], stroke);
                painter.line_segment([a - n * cross, a + n * cross], stroke);
                painter.line_segment([b - n * cross, b + n * cross], stroke);
            }
            painter.circle_filled(a, 4.0, egui::Color32::WHITE);
            painter.circle_stroke(a, 4.0, egui::Stroke::new(1.0_f32, egui::Color32::BLACK));
            painter.circle_filled(b, 4.0, egui::Color32::from_gray(205));
            painter.circle_stroke(b, 4.0, egui::Stroke::new(1.0_f32, egui::Color32::BLACK));
        }
        DevelopLocalOverlay::Radial { cx, cy, rx, ry } => {
            let n = 64;
            let pts: Vec<egui::Pos2> = (0..=n)
                .map(|i| {
                    let t = i as f32 / n as f32 * std::f32::consts::TAU;
                    egui::pos2(cx + rx * t.cos(), cy + ry * t.sin())
                })
                .collect();
            for stroke in [shadow, line] {
                painter.add(egui::Shape::line(pts.clone(), stroke));
            }
            let c = egui::pos2(cx, cy);
            painter.circle_filled(c, 3.5, egui::Color32::WHITE);
            painter.circle_stroke(c, 3.5, egui::Stroke::new(1.0_f32, egui::Color32::BLACK));
        }
    }
}

/// Data for the free-transform overlay drawn in canvas space.
pub struct TransformOverlayData {
    /// 4 corners in canvas space: [TL, TR, BL, BR]
    pub corners: [(f32, f32); 4],
    /// 8 handle positions in canvas space: [TL, TC, TR, ML, MR, BL, BC, BR]
    pub handles: [(f32, f32); 8],
    /// Center handle in canvas space
    pub center: (f32, f32),
}

fn text_preview_hash(td: &crate::core::text::TextData) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    td.content.hash(&mut h);
    td.font_px.to_bits().hash(&mut h);
    td.color.hash(&mut h);
    td.font_family.hash(&mut h);
    td.align.hash(&mut h);
    td.line_height.to_bits().hash(&mut h);
    td.bold.hash(&mut h);
    td.italic.hash(&mut h);
    td.underline.hash(&mut h);
    td.tracking_px.to_bits().hash(&mut h);
    td.opacity.to_bits().hash(&mut h);
    td.stretch_x.to_bits().hash(&mut h);
    td.rotation_deg.to_bits().hash(&mut h);
    td.flip_x.hash(&mut h);
    td.flip_y.hash(&mut h);
    for gs in &td.glyph_styles {
        gs.color.hash(&mut h);
        gs.font_px.to_bits().hash(&mut h);
        gs.font_family.hash(&mut h);
        gs.bold.hash(&mut h);
        gs.italic.hash(&mut h);
        gs.underline.hash(&mut h);
    }
    h.finish()
}

fn text_preview_texture(
    ctx: &egui::Context,
    td: &crate::core::text::TextData,
) -> Option<(egui::TextureHandle, u32, u32)> {
    // Cache check first: rasterizing runs every frame the overlay is open,
    // and only the miss path may pay for a full rasterization.
    let hash = text_preview_hash(td);
    let cache_id = egui::Id::new("text_overlay_raster_preview_texture");
    if let Some((cached_hash, texture, w, h)) =
        ctx.data(|data| data.get_temp::<(u64, egui::TextureHandle, u32, u32)>(cache_id))
    {
        if cached_hash == hash {
            return Some((texture, w, h));
        }
    }
    let raster = crate::core::text::rasterize_stretched(td)?;

    let image = egui::ColorImage::from_rgba_unmultiplied(
        [raster.width as usize, raster.height as usize],
        &raster.rgba,
    );
    let texture = if let Some((_, mut texture, _, _)) =
        ctx.data(|data| data.get_temp::<(u64, egui::TextureHandle, u32, u32)>(cache_id))
    {
        texture.set(image, egui::TextureOptions::NEAREST);
        texture
    } else {
        ctx.load_texture(
            "text_overlay_raster_preview",
            image,
            egui::TextureOptions::NEAREST,
        )
    };
    ctx.data_mut(|data| {
        data.insert_temp(
            cache_id,
            (hash, texture.clone(), raster.width, raster.height),
        );
    });
    Some((texture, raster.width, raster.height))
}

fn text_rotation_sin_cos(rotation_deg: f32) -> (f32, f32) {
    let rad = rotation_deg.to_radians();
    (rad.sin(), rad.cos())
}

fn text_local_to_screen(
    origin: egui::Pos2,
    x: f32,
    y: f32,
    scale: f32,
    rotation_deg: f32,
    flip_x: bool,
    flip_y: bool,
) -> egui::Pos2 {
    let (s, c) = text_rotation_sin_cos(rotation_deg);
    let sx = if flip_x { -x } else { x } * scale;
    let sy = if flip_y { -y } else { y } * scale;
    origin + egui::vec2(c * sx - s * sy, s * sx + c * sy)
}

fn text_screen_to_local(
    origin: egui::Pos2,
    pos: egui::Pos2,
    scale: f32,
    rotation_deg: f32,
    flip_x: bool,
    flip_y: bool,
) -> (f32, f32) {
    let (s, c) = text_rotation_sin_cos(rotation_deg);
    let dx = pos.x - origin.x;
    let dy = pos.y - origin.y;
    let x = (c * dx + s * dy) / scale;
    let y = (-s * dx + c * dy) / scale;
    (if flip_x { -x } else { x }, if flip_y { -y } else { y })
}

fn text_preview_corners(
    origin: egui::Pos2,
    w: u32,
    h: u32,
    scale: f32,
    rotation_deg: f32,
    flip_x: bool,
    flip_y: bool,
) -> [egui::Pos2; 4] {
    [
        text_local_to_screen(origin, 0.0, 0.0, scale, rotation_deg, flip_x, flip_y),
        text_local_to_screen(origin, w as f32, 0.0, scale, rotation_deg, flip_x, flip_y),
        text_local_to_screen(origin, 0.0, h as f32, scale, rotation_deg, flip_x, flip_y),
        text_local_to_screen(
            origin,
            w as f32,
            h as f32,
            scale,
            rotation_deg,
            flip_x,
            flip_y,
        ),
    ]
}

fn text_preview_bounds(corners: &[egui::Pos2; 4]) -> egui::Rect {
    let mut rect = egui::Rect::NOTHING;
    for p in corners {
        rect.extend_with(*p);
    }
    rect
}

fn paint_text_preview_texture(
    painter: &egui::Painter,
    texture_id: egui::TextureId,
    corners: [egui::Pos2; 4],
) {
    let mut mesh = egui::Mesh::with_texture(texture_id);
    mesh.indices.extend_from_slice(&[0, 1, 2, 2, 1, 3]);
    mesh.vertices.extend_from_slice(&[
        egui::epaint::Vertex {
            pos: corners[0],
            uv: egui::pos2(0.0, 0.0),
            color: egui::Color32::WHITE,
        },
        egui::epaint::Vertex {
            pos: corners[1],
            uv: egui::pos2(1.0, 0.0),
            color: egui::Color32::WHITE,
        },
        egui::epaint::Vertex {
            pos: corners[2],
            uv: egui::pos2(0.0, 1.0),
            color: egui::Color32::WHITE,
        },
        egui::epaint::Vertex {
            pos: corners[3],
            uv: egui::pos2(1.0, 1.0),
            color: egui::Color32::WHITE,
        },
    ]);
    painter.add(egui::Shape::mesh(mesh));
}

fn text_preview_contains(
    pos: egui::Pos2,
    origin: egui::Pos2,
    w: u32,
    h: u32,
    scale: f32,
    rotation_deg: f32,
    flip_x: bool,
    flip_y: bool,
) -> bool {
    let (tx, ty) = text_screen_to_local(origin, pos, scale, rotation_deg, flip_x, flip_y);
    tx >= 0.0 && ty >= 0.0 && tx <= w as f32 && ty <= h as f32
}

fn paint_text_selection_overlay(
    painter: &egui::Painter,
    origin: egui::Pos2,
    rects: &[crate::core::text::TextLayoutRect],
    scale: f32,
    rotation_deg: f32,
    flip_x: bool,
    flip_y: bool,
    stretch_x: f32,
    fill: egui::Color32,
) {
    let stretch_x = stretch_x.max(0.001);
    for rect in rects {
        if rect.w <= 0.0 || rect.h <= 0.0 {
            continue;
        }

        let grow_x = 1.0 / scale;
        let grow_y = 0.5 / scale;
        let x0 = rect.x * stretch_x - grow_x;
        let y0 = rect.y - grow_y;
        let x1 = (rect.x + rect.w) * stretch_x + grow_x;
        let y1 = rect.y + rect.h + grow_y;
        painter.add(egui::Shape::convex_polygon(
            vec![
                text_local_to_screen(origin, x0, y0, scale, rotation_deg, flip_x, flip_y),
                text_local_to_screen(origin, x1, y0, scale, rotation_deg, flip_x, flip_y),
                text_local_to_screen(origin, x1, y1, scale, rotation_deg, flip_x, flip_y),
                text_local_to_screen(origin, x0, y1, scale, rotation_deg, flip_x, flip_y),
            ],
            fill,
            egui::Stroke::NONE,
        ));
    }
}

/// While a refused action is flashing the active modal (strict modal lock),
/// pulse a Commit/Cancel button's fill so the user can find where to finish.
pub(crate) fn modal_flash_btn<'a>(
    btn: egui::Button<'a>,
    ui: &egui::Ui,
    data: &UiData,
) -> egui::Button<'a> {
    if !data.chrome.modal_flash {
        return btn;
    }
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(90));
    if ((ui.input(|i| i.time) * 5.0) as i64) % 2 == 0 {
        btn.fill(data.chrome.theme_mode.palette().warning)
    } else {
        btn
    }
}

fn paint_text_caret_overlay(
    painter: &egui::Painter,
    origin: egui::Pos2,
    rect: crate::core::text::TextLayoutRect,
    scale: f32,
    rotation_deg: f32,
    flip_x: bool,
    flip_y: bool,
    stretch_x: f32,
    screen_font: f32,
    time: f64,
) {
    let blink_on = ((time * 2.0).floor() as i64).rem_euclid(2) == 0;
    if !blink_on {
        return;
    }

    let center_y = rect.y + rect.h * 0.5;
    let caret_h = (rect.h * scale)
        .min(screen_font * 0.98)
        .max(screen_font * 0.72)
        .max(6.0);
    let caret_h_local = caret_h / scale;
    let top = text_local_to_screen(
        origin,
        rect.x * stretch_x.max(0.001),
        center_y - caret_h_local * 0.5,
        scale,
        rotation_deg,
        flip_x,
        flip_y,
    );
    let bottom = text_local_to_screen(
        origin,
        rect.x * stretch_x.max(0.001),
        center_y + caret_h_local * 0.5,
        scale,
        rotation_deg,
        flip_x,
        flip_y,
    );
    // Two-tone caret (dark halo + light core) stays visible over any text or
    // background colour — the text colour itself would vanish on same-colour
    // backgrounds.
    let core_w = (screen_font / 42.0).clamp(1.0, 2.2);
    painter.line_segment(
        [top, bottom],
        egui::Stroke::new(core_w + 2.0, egui::Color32::from_black_alpha(150)),
    );
    painter.line_segment(
        [top, bottom],
        egui::Stroke::new(core_w, egui::Color32::WHITE),
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerAlign {
    Left,
    HorizontalCenter,
    Right,
    Top,
    VerticalCenter,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveTransformAction {
    Rotate90Ccw,
    Rotate90Cw,
    Rotate180,
    FlipHorizontal,
    FlipVertical,
}

#[allow(deprecated)]
pub fn build(
    egui_ctx: &egui::Context,
    egui_state: &mut Option<egui_winit::State>,
    window: &winit::window::Window,
    data: &UiData,
) -> (
    Vec<egui::ClippedPrimitive>,
    egui::TexturesDelta,
    UiActions,
    std::time::Duration,
) {
    let raw_input = if let Some(state) = egui_state {
        state.take_egui_input(window)
    } else {
        egui::RawInput::default()
    };

    let mut actions = UiActions::default();

    let full_output = egui_ctx.run_ui(raw_input, |ctx| {
        // Theme is applied on-change from the app loop (see RedrawRequested), not
        // every frame — the custom chrome reads `theme_mode.palette()` directly.

        if ctx.input_mut(|i| {
            i.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::CTRL,
                egui::Key::N,
            ))
        }) {
            actions.dialogs.show_new_dialog = Some(true);
        }
        if ctx.input_mut(|i| {
            i.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers {
                    alt: true,
                    ctrl: true,
                    ..Default::default()
                },
                egui::Key::I,
            ))
        }) {
            actions.dialogs.show_image_size_dialog = Some(true);
        }

        menubar::build(ctx, data, &mut actions);

        if data.chrome.show_welcome {
            welcome::build(ctx, data, &mut actions);
            dialogs::build(ctx, data, &mut actions);
            return;
        }

        if data.chrome.show_library {
            library::build(ctx, data, &mut actions);
            dialogs::build(ctx, data, &mut actions);
            return;
        }

        topoptions::build(ctx, data, &mut actions);

        tabbar::build(ctx, data, &mut actions);

        nav::build(ctx, data, &mut actions);

        toolbar::build(ctx, data, &mut actions);

        if data.chrome.show_rulers {
            draw_rulers(ctx, data, &mut actions);
        }

        if data.sel.show_refine_panel {
            refine_select::build(ctx, data, &mut actions);
        } else {
            panels::build(ctx, data, &mut actions);
        }

        statusbar::build(ctx, data, &mut actions);

        dialogs::build(ctx, data, &mut actions);
        // Suppressed when the Develop stage is hosted in its own OS window.
        if !data.develop.develop_in_window {
            develop::build(ctx, data, &mut actions);
        }
        warp::build(ctx, data, &mut actions);
        ai_panel::build(ctx, data, &mut actions);
        ai_progress::build(ctx, data, &mut actions);
        draw_paint_color_dialog(ctx, data, &mut actions);

        let canvas_viewport = {
            let screen = ctx.content_rect();
            let ruler_off = if data.chrome.show_rulers {
                20.0_f32
            } else {
                0.0
            };
            egui::Rect::from_min_max(
                egui::pos2(
                    data.chrome.toolbar_w + ruler_off,
                    28.0 + 26.0 + 32.0 + ruler_off,
                ),
                egui::pos2(screen.max.x - data.chrome.panel_r_w, screen.max.y - 22.0),
            )
        };

        if data.sel.show_refine_panel
            && data.sel.refine_view_mode == crate::ui::refine_select::RefineViewMode::Overlay
        {
            let canvas_rect = egui::Rect::from_min_size(
                egui::pos2(data.doc.offset_x, data.doc.offset_y),
                egui::vec2(
                    data.doc.canvas_w as f32 * data.doc.zoom,
                    data.doc.canvas_h as f32 * data.doc.zoom,
                ),
            );

            let clip_rect = canvas_rect.intersect(canvas_viewport);
            if clip_rect.is_positive() {
                let painter = ctx
                    .layer_painter(egui::LayerId::new(
                        egui::Order::Background,
                        egui::Id::new("sam_overlay"),
                    ))
                    .with_clip_rect(clip_rect);

                if let Some(tex_id) = data.sel.refine_overlay_tex {
                    painter.image(
                        tex_id,
                        canvas_rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                } else {
                    let [r, g, b, a] = data.sel.refine_overlay_color;
                    painter.rect_filled(
                        canvas_rect,
                        0.0,
                        egui::Color32::from_rgba_unmultiplied(r, g, b, a),
                    );
                }
            }
        }

        let sel_stroke = egui::Stroke::new(1.0_f32, egui::Color32::WHITE);
        let zoom = data.doc.zoom;
        let ox = data.doc.offset_x;
        let oy = data.doc.offset_y;
        let to_screen_pos = |cx: f32, cy: f32| egui::pos2(cx * zoom + ox, cy * zoom + oy);

        // Ruler guides (theme guide accent) + the in-progress one being dragged (brighter).
        if data.chrome.show_guides
            && (!data.chrome.guides.is_empty() || data.chrome.guide_preview.is_some())
        {
            let pal = data.chrome.theme_mode.palette();
            let painter = ctx
                .layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("guides_overlay"),
                ))
                .with_clip_rect(canvas_viewport);
            let normal = egui::Stroke::new(1.0_f32, pal.accent_guide);
            // Highlighted (hovered / dragged): brighter + thicker so the user knows
            // it can be grabbed.
            let hot = egui::Stroke::new(2.0_f32, pal.accent_hover);
            let preview = egui::Stroke::new(1.0_f32, pal.accent_guide);
            let draw_guide =
                |orientation: GuideOrientation, pos: f32, stroke: egui::Stroke| match orientation {
                    GuideOrientation::Vertical => {
                        let sx = pos * zoom + ox;
                        painter.line_segment(
                            [
                                egui::pos2(sx, canvas_viewport.top()),
                                egui::pos2(sx, canvas_viewport.bottom()),
                            ],
                            stroke,
                        );
                    }
                    GuideOrientation::Horizontal => {
                        let sy = pos * zoom + oy;
                        painter.line_segment(
                            [
                                egui::pos2(canvas_viewport.left(), sy),
                                egui::pos2(canvas_viewport.right(), sy),
                            ],
                            stroke,
                        );
                    }
                };
            for (i, g) in data.chrome.guides.iter().enumerate() {
                let stroke = if Some(i) == data.chrome.hovered_guide {
                    hot
                } else {
                    normal
                };
                draw_guide(g.orientation, g.pos, stroke);
            }
            if let Some((orientation, pos)) = data.chrome.guide_preview {
                draw_guide(orientation, pos, preview);
            }
        }

        // Smart guides from snapping (Move tool ③): guide accent = alignment to an
        // object/center/guide, danger = flush with a canvas edge. Span the canvas.
        if !data.chrome.snap_guides.is_empty() {
            let pal = data.chrome.theme_mode.palette();
            let painter = ctx
                .layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("snap_guides_overlay"),
                ))
                .with_clip_rect(canvas_viewport);
            let align_col = pal.accent_guide;
            let edge_col = pal.danger;
            let cx0 = ox;
            let cy0 = oy;
            let cx1 = data.doc.canvas_w as f32 * zoom + ox;
            let cy1 = data.doc.canvas_h as f32 * zoom + oy;
            for line in &data.chrome.snap_guides {
                let col = if line.kind.is_canvas_edge() {
                    edge_col
                } else {
                    align_col
                };
                let stroke = egui::Stroke::new(1.0_f32, col);
                if line.vertical {
                    let sx = line.pos * zoom + ox;
                    painter.line_segment([egui::pos2(sx, cy0), egui::pos2(sx, cy1)], stroke);
                } else {
                    let sy = line.pos * zoom + oy;
                    painter.line_segment([egui::pos2(cx0, sy), egui::pos2(cx1, sy)], stroke);
                }
            }
        }

        if data.sel.lasso_preview.len() >= 2 {
            let painter = ctx
                .layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("lasso_preview"),
                ))
                .with_clip_rect(canvas_viewport);
            let pts: Vec<egui::Pos2> = data
                .sel
                .lasso_preview
                .iter()
                .map(|&(x, y)| to_screen_pos(x, y))
                .collect();
            let phase = ctx.input(|i| (i.time as f32 * 42.0) % 12.0);
            paint_contrast_polyline(&painter, &pts, phase);
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }

        // Perspective Crop quad: edges, thirds grid (bilinear across the quad) and
        // square corner handles.
        if let Some(quad) = data.tool.persp_crop_quad {
            let painter = ctx
                .layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("persp_crop_overlay"),
                ))
                .with_clip_rect(canvas_viewport);
            let sp: Vec<egui::Pos2> = quad.iter().map(|&(x, y)| to_screen_pos(x, y)).collect();
            let lerp = |a: egui::Pos2, b: egui::Pos2, t: f32| {
                egui::pos2(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
            };
            let inner = egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(105));
            for i in 1..3 {
                let t = i as f32 / 3.0;
                painter.line_segment([lerp(sp[0], sp[1], t), lerp(sp[3], sp[2], t)], inner);
                painter.line_segment([lerp(sp[0], sp[3], t), lerp(sp[1], sp[2], t)], inner);
            }
            let border_shadow = egui::Stroke::new(3.0_f32, egui::Color32::from_black_alpha(150));
            let border = egui::Stroke::new(1.4_f32, egui::Color32::WHITE);
            for stroke in [border_shadow, border] {
                painter.add(egui::Shape::line(
                    vec![sp[0], sp[1], sp[2], sp[3], sp[0]],
                    stroke,
                ));
            }
            for p in &sp {
                let r = egui::Rect::from_center_size(*p, egui::vec2(10.0, 10.0));
                painter.rect_filled(r, 1.0, egui::Color32::WHITE);
                painter.rect_stroke(
                    r,
                    1.0,
                    egui::Stroke::new(1.0_f32, egui::Color32::from_gray(40)),
                    egui::StrokeKind::Inside,
                );
            }
        }

        // Perspective Crop — corner-picking preview: placed corners as handles and
        // a rubber-band line to the live cursor. After two corners, close the live
        // triangle back to the first point and show a triangular thirds grid; after
        // three corners, show the forming quad/grid while placing the fourth.
        if !data.tool.persp_crop_preview.is_empty() {
            let painter = ctx
                .layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("persp_crop_preview"),
                ))
                .with_clip_rect(canvas_viewport);
            let sp: Vec<egui::Pos2> = data
                .tool
                .persp_crop_preview
                .iter()
                .map(|&(x, y)| to_screen_pos(x, y))
                .collect();
            let border_shadow = egui::Stroke::new(3.0_f32, egui::Color32::from_black_alpha(150));
            let border = egui::Stroke::new(1.4_f32, egui::Color32::WHITE);
            let lerp = |a: egui::Pos2, b: egui::Pos2, t: f32| {
                egui::pos2(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
            };

            if sp.len() == 4 {
                let inner = egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(105));
                for i in 1..3 {
                    let t = i as f32 / 3.0;
                    painter.line_segment([lerp(sp[0], sp[1], t), lerp(sp[3], sp[2], t)], inner);
                    painter.line_segment([lerp(sp[0], sp[3], t), lerp(sp[1], sp[2], t)], inner);
                }
                for stroke in [border_shadow, border] {
                    painter.add(egui::Shape::line(
                        vec![sp[0], sp[1], sp[2], sp[3], sp[0]],
                        stroke,
                    ));
                }
            } else if sp.len() == 3 {
                // Two fixed corners + the live cursor. Three families of thirds
                // lines form a triangular mesh while the cursor edge closes
                // directly back to the first corner.
                let inner = egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(105));
                for i in 1..3 {
                    let t = i as f32 / 3.0;
                    painter.line_segment([lerp(sp[0], sp[1], t), lerp(sp[0], sp[2], t)], inner);
                    painter.line_segment([lerp(sp[1], sp[0], t), lerp(sp[1], sp[2], t)], inner);
                    painter.line_segment([lerp(sp[2], sp[0], t), lerp(sp[2], sp[1], t)], inner);
                }
                for stroke in [border_shadow, border] {
                    painter.add(egui::Shape::line(vec![sp[0], sp[1], sp[2], sp[0]], stroke));
                }
            } else {
                for stroke in [border_shadow, border] {
                    painter.add(egui::Shape::line(sp.clone(), stroke));
                }
            }

            // Square handles on the corners already committed (all but the cursor).
            let placed = sp.len().saturating_sub(1);
            for p in sp.iter().take(placed) {
                let r = egui::Rect::from_center_size(*p, egui::vec2(10.0, 10.0));
                painter.rect_filled(r, 1.0, egui::Color32::WHITE);
                painter.rect_stroke(
                    r,
                    1.0,
                    egui::Stroke::new(1.0_f32, egui::Color32::from_gray(40)),
                    egui::StrokeKind::Inside,
                );
            }
            // A small dot marks where the next corner will land.
            if let Some(last) = sp.last() {
                painter.circle_filled(*last, 3.5, egui::Color32::WHITE);
                painter.circle_stroke(
                    *last,
                    3.5,
                    egui::Stroke::new(1.0_f32, egui::Color32::from_gray(40)),
                );
            }
        }

        // Pen tool: the path drawn as a fixed solid line (not animated marching
        // ants — a vector path is steady, like the standard), plus anchor handles.
        if data.tool.pen_path.len() >= 2 {
            let painter = ctx
                .layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("pen_path_overlay"),
                ))
                .with_clip_rect(canvas_viewport);
            let mut pts: Vec<egui::Pos2> = data
                .tool
                .pen_path
                .iter()
                .map(|&(x, y)| to_screen_pos(x, y))
                .collect();
            if data.tool.pen_closed {
                if let Some(&first) = pts.first() {
                    pts.push(first);
                }
            }
            // Thin blue line, matching the Bézier handle arms.
            painter.add(egui::Shape::line(
                pts,
                egui::Stroke::new(1.0_f32, egui::Color32::from_gray(190)),
            ));
        }
        if !data.tool.pen_handles.is_empty() {
            let painter = ctx
                .layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("pen_handles_overlay"),
                ))
                .with_clip_rect(canvas_viewport);
            let arm = egui::Stroke::new(1.0_f32, egui::Color32::from_gray(190));
            for &[ax, ay, hx, hy] in &data.tool.pen_handles {
                let a = to_screen_pos(ax, ay);
                let h = to_screen_pos(hx, hy);
                painter.line_segment([a, h], arm);
                painter.circle_filled(h, 3.0, egui::Color32::from_gray(190));
            }
        }
        if !data.tool.pen_anchors.is_empty() {
            let painter = ctx
                .layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("pen_anchors_overlay"),
                ))
                .with_clip_rect(canvas_viewport);
            for &(x, y) in &data.tool.pen_anchors {
                let r = egui::Rect::from_center_size(to_screen_pos(x, y), egui::vec2(7.0, 7.0));
                painter.rect_filled(r, 0.0, egui::Color32::WHITE);
                painter.rect_stroke(
                    r,
                    0.0,
                    egui::Stroke::new(1.0_f32, egui::Color32::from_gray(40)),
                    egui::StrokeKind::Inside,
                );
            }
        }

        if let Some([x0, y0, x1, y1]) = data.sel.rect_sel_preview {
            let painter = ctx
                .layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("rect_sel_preview"),
                ))
                .with_clip_rect(canvas_viewport);
            let tl = to_screen_pos(x0.min(x1), y0.min(y1));
            let tr = to_screen_pos(x0.max(x1), y0.min(y1));
            let br = to_screen_pos(x0.max(x1), y0.max(y1));
            let bl = to_screen_pos(x0.min(x1), y0.max(y1));
            painter.add(egui::Shape::line(vec![tl, tr, br, bl, tl], sel_stroke));
        }

        if let Some([x0, y0, x1, y1]) = data.sel.ellipse_sel_preview {
            let painter = ctx
                .layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("ellipse_sel_preview"),
                ))
                .with_clip_rect(canvas_viewport);
            let cx = (x0 + x1) * 0.5;
            let cy = (y0 + y1) * 0.5;
            let rx = (x1 - x0).abs() * 0.5;
            let ry = (y1 - y0).abs() * 0.5;
            if rx > 0.5 && ry > 0.5 {
                let n = 64usize;
                let mut pts: Vec<egui::Pos2> = (0..=n)
                    .map(|i| {
                        let angle = std::f32::consts::TAU * i as f32 / n as f32;
                        to_screen_pos(cx + rx * angle.cos(), cy + ry * angle.sin())
                    })
                    .collect();
                pts.push(pts[0]);
                painter.add(egui::Shape::line(pts, sel_stroke));
            }
        }

        if let Some([x0, y0, x1, y1]) = data.tool.shape_preview {
            let painter = ctx
                .layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("shape_preview"),
                ))
                .with_clip_rect(canvas_viewport);
            let stroke = egui::Stroke::new(1.0_f32, egui::Color32::from_gray(190));
            // canvas→screen scale (px per canvas unit), for the corner radius.
            let scale = to_screen_pos(1.0, 0.0).x - to_screen_pos(0.0, 0.0).x;
            let readout = match data.tool.shape_kind {
                2 => {
                    painter.add(egui::Shape::line(
                        vec![to_screen_pos(x0, y0), to_screen_pos(x1, y1)],
                        stroke,
                    ));
                    let len = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
                    let ang = (y1 - y0).atan2(x1 - x0).to_degrees();
                    format!("{:.0} px  {:.1}°", len, ang)
                }
                1 => {
                    let cx = (x0 + x1) * 0.5;
                    let cy = (y0 + y1) * 0.5;
                    let rx = (x1 - x0).abs() * 0.5;
                    let ry = (y1 - y0).abs() * 0.5;
                    if rx > 0.5 && ry > 0.5 {
                        let n = 64usize;
                        let mut pts: Vec<egui::Pos2> = (0..=n)
                            .map(|i| {
                                let a = std::f32::consts::TAU * i as f32 / n as f32;
                                to_screen_pos(cx + rx * a.cos(), cy + ry * a.sin())
                            })
                            .collect();
                        pts.push(pts[0]);
                        painter.add(egui::Shape::line(pts, stroke));
                    }
                    format!("{:.0} × {:.0}", (x1 - x0).abs(), (y1 - y0).abs())
                }
                3 | 4 => {
                    let cx = (x0 + x1) * 0.5;
                    let cy = (y0 + y1) * 0.5;
                    let rx = (x1 - x0).abs() * 0.5;
                    let ry = (y1 - y0).abs() * 0.5;
                    let n = data.tool.shape_sides.clamp(3, 100) as usize;
                    if rx > 0.5 && ry > 0.5 {
                        let start = -std::f32::consts::FRAC_PI_2;
                        let star = data.tool.shape_kind == 4;
                        let inner = data.tool.shape_star_inner.clamp(0.05, 0.95);
                        let count = if star { 2 * n } else { n };
                        let mut pts: Vec<egui::Pos2> = (0..count)
                            .map(|i| {
                                let (a, f) = if star {
                                    let a = start + std::f32::consts::PI * i as f32 / n as f32;
                                    (a, if i % 2 == 0 { 1.0 } else { inner })
                                } else {
                                    (start + std::f32::consts::TAU * i as f32 / n as f32, 1.0)
                                };
                                to_screen_pos(cx + rx * f * a.cos(), cy + ry * f * a.sin())
                            })
                            .collect();
                        if let Some(&first) = pts.first() {
                            pts.push(first);
                        }
                        painter.add(egui::Shape::line(pts, stroke));
                    }
                    format!("{:.0} × {:.0}", (x1 - x0).abs(), (y1 - y0).abs())
                }
                _ => {
                    let w = (x1 - x0).abs();
                    let h = (y1 - y0).abs();
                    let r = data
                        .tool
                        .shape_corner_radius
                        .min(w * 0.5)
                        .min(h * 0.5)
                        .max(0.0);
                    if r > 0.0 && scale > 0.0 {
                        let rect = egui::Rect::from_min_max(
                            to_screen_pos(x0.min(x1), y0.min(y1)),
                            to_screen_pos(x0.max(x1), y0.max(y1)),
                        );
                        painter.rect_stroke(rect, r * scale, stroke, egui::StrokeKind::Inside);
                    } else {
                        let tl = to_screen_pos(x0.min(x1), y0.min(y1));
                        let tr = to_screen_pos(x0.max(x1), y0.min(y1));
                        let br = to_screen_pos(x0.max(x1), y0.max(y1));
                        let bl = to_screen_pos(x0.min(x1), y0.max(y1));
                        painter.add(egui::Shape::line(vec![tl, tr, br, bl, tl], stroke));
                    }
                    format!("{:.0} × {:.0}", w, h)
                }
            };

            // Live size/length readout, pinned just off the drag-end cursor.
            let pos = to_screen_pos(x1, y1) + egui::vec2(12.0, -20.0);
            let galley = painter.layout_no_wrap(
                readout,
                egui::FontId::proportional(12.0),
                egui::Color32::WHITE,
            );
            let bg = egui::Rect::from_min_size(
                pos - egui::vec2(4.0, 2.0),
                galley.size() + egui::vec2(8.0, 4.0),
            );
            painter.rect_filled(bg, 3.0, egui::Color32::from_black_alpha(180));
            painter.galley(pos, galley, egui::Color32::WHITE);
        }

        // Shape layer editing overlay: bounding box + resize handles + the
        // rounded-corner radius node (Shape tool, active Shape layer).
        if let Some(overlay) = &data.tool.shape_overlay {
            let painter = ctx
                .layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("shape_overlay"),
                ))
                .with_clip_rect(canvas_viewport);
            let [x0, y0, x1, y1] = overlay.span;
            let accent = egui::Color32::from_rgb(64, 140, 240);
            // Bounding box (not drawn for lines — the endpoints imply it).
            if overlay.kind != 2 {
                let tl = to_screen_pos(x0.min(x1), y0.min(y1));
                let br = to_screen_pos(x0.max(x1), y0.max(y1));
                painter.rect_stroke(
                    egui::Rect::from_two_pos(tl, br),
                    0.0,
                    egui::Stroke::new(1.0_f32, accent),
                    egui::StrokeKind::Inside,
                );
            }
            // While dragging a handle the raster catches up on a throttle, so
            // draw the live geometry as a vector outline on top.
            if overlay.dragging {
                let stroke = egui::Stroke::new(1.0_f32, accent);
                let scale = to_screen_pos(1.0, 0.0).x - to_screen_pos(0.0, 0.0).x;
                match overlay.kind {
                    2 => {
                        painter.add(egui::Shape::line(
                            vec![to_screen_pos(x0, y0), to_screen_pos(x1, y1)],
                            stroke,
                        ));
                    }
                    1 => {
                        let cx = (x0 + x1) * 0.5;
                        let cy = (y0 + y1) * 0.5;
                        let rx = (x1 - x0).abs() * 0.5;
                        let ry = (y1 - y0).abs() * 0.5;
                        if rx > 0.5 && ry > 0.5 {
                            let n = 64usize;
                            let mut pts: Vec<egui::Pos2> = (0..=n)
                                .map(|i| {
                                    let a = std::f32::consts::TAU * i as f32 / n as f32;
                                    to_screen_pos(cx + rx * a.cos(), cy + ry * a.sin())
                                })
                                .collect();
                            pts.push(pts[0]);
                            painter.add(egui::Shape::line(pts, stroke));
                        }
                    }
                    _ => {
                        if overlay.radius > 0.0 && scale > 0.0 {
                            let rect = egui::Rect::from_min_max(
                                to_screen_pos(x0.min(x1), y0.min(y1)),
                                to_screen_pos(x0.max(x1), y0.max(y1)),
                            );
                            painter.rect_stroke(
                                rect,
                                overlay.radius * scale,
                                stroke,
                                egui::StrokeKind::Inside,
                            );
                        }
                        // Radius 0: the bounding box above already IS the outline.
                    }
                }
            }
            for &(hid, hx, hy) in &overlay.handles {
                let c = to_screen_pos(hx, hy);
                if hid == 8 {
                    // Corner-radius node: a small diamond.
                    let r = 4.5;
                    let pts = vec![
                        egui::pos2(c.x, c.y - r),
                        egui::pos2(c.x + r, c.y),
                        egui::pos2(c.x, c.y + r),
                        egui::pos2(c.x - r, c.y),
                    ];
                    painter.add(egui::Shape::convex_polygon(
                        pts,
                        egui::Color32::WHITE,
                        egui::Stroke::new(1.0_f32, accent),
                    ));
                } else {
                    let rect = egui::Rect::from_center_size(c, egui::vec2(8.0, 8.0));
                    painter.rect_filled(rect, 0.0, egui::Color32::WHITE);
                    painter.rect_stroke(
                        rect,
                        0.0,
                        egui::Stroke::new(1.0_f32, accent),
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }

        // A zoom-bucketed display raster keeps the active Path crisp without
        // changing the document-resolution atlas (and therefore without
        // softening raster/photo layers). It sits in the canvas-tool layer so
        // dialogs and panels always remain above it.
        if let Some(display) = &data.tool.path_display {
            let texture_cache_id = egui::Id::new("active_path_display_texture");
            let textures = ctx
                .data(|d| {
                    d.get_temp::<(u64, Vec<egui::TextureHandle>)>(texture_cache_id)
                        .filter(|(key, _)| *key == display.cache_key)
                        .map(|(_, textures)| textures)
                })
                .unwrap_or_else(|| {
                    let textures: Vec<_> = display
                        .tiles
                        .iter()
                        .enumerate()
                        .map(|(index, tile)| {
                            let image = egui::ColorImage::from_rgba_unmultiplied(
                                [tile.width as usize, tile.height as usize],
                                tile.rgba.as_slice(),
                            );
                            ctx.load_texture(
                                format!("active_path_display_{index}"),
                                image,
                                // Display rasters are baked at the next zoom
                                // bucket (2x/4x/8x/16x). Most actual zooms sit
                                // between buckets, so the texture is reduced
                                // slightly on screen. Linear filtering preserves
                                // the supersampled edge coverage; nearest sampling
                                // reintroduced visible stair-steps at e.g. 257%.
                                egui::TextureOptions::LINEAR,
                            )
                        })
                        .collect();
                    ctx.data_mut(|d| {
                        d.insert_temp(texture_cache_id, (display.cache_key, textures.clone()));
                    });
                    textures
                });
            let painter = ctx
                .layer_painter(egui::LayerId::new(
                    CANVAS_TOOL_OVERLAY_ORDER,
                    egui::Id::new("active_path_display"),
                ))
                .with_clip_rect(canvas_viewport);
            let px_canvas_x = display.canvas_w / display.raster_w as f32;
            let px_canvas_y = display.canvas_h / display.raster_h as f32;
            for (tile, texture) in display.tiles.iter().zip(&textures) {
                let tile_x = display.canvas_x + tile.x as f32 * px_canvas_x;
                let tile_y = display.canvas_y + tile.y as f32 * px_canvas_y;
                let rect = egui::Rect::from_min_size(
                    to_screen_pos(tile_x, tile_y),
                    egui::vec2(
                        tile.width as f32 * px_canvas_x * zoom,
                        tile.height as f32 * px_canvas_y * zoom,
                    ),
                );
                painter.image(
                    texture.id(),
                    rect,
                    egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
        }

        // Node tool overlay: the active Path's outline, its anchor points, and
        // the selected node's Bézier handle arms.
        if let Some(overlay) = &data.tool.node_overlay {
            let painter = ctx
                .layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("node_overlay"),
                ))
                .with_clip_rect(canvas_viewport);
            let accent = egui::Color32::from_rgb(64, 140, 240);
            // Path outline (steady blue line, like the Pen preview).
            for line in &overlay.outlines {
                if line.len() >= 2 {
                    let pts: Vec<egui::Pos2> =
                        line.iter().map(|&(x, y)| to_screen_pos(x, y)).collect();
                    painter.add(egui::Shape::line(pts, egui::Stroke::new(1.0_f32, accent)));
                }
            }
            // Handle arms of the selected node (thin line + round control point).
            for &[ax, ay, hx, hy] in &overlay.handles {
                let a = to_screen_pos(ax, ay);
                let h = to_screen_pos(hx, hy);
                painter.add(egui::Shape::line_segment(
                    [a, h],
                    egui::Stroke::new(1.0_f32, accent),
                ));
                painter.circle(
                    h,
                    3.0,
                    egui::Color32::WHITE,
                    egui::Stroke::new(1.0_f32, accent),
                );
            }
            // Anchor squares: hollow for normal, filled for the selected node.
            for &(x, y, selected) in &overlay.nodes {
                let c = to_screen_pos(x, y);
                let rect = egui::Rect::from_center_size(c, egui::vec2(7.0, 7.0));
                if selected {
                    painter.rect_filled(rect, 0.0, accent);
                    painter.rect_stroke(
                        rect,
                        0.0,
                        egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
                        egui::StrokeKind::Outside,
                    );
                } else {
                    painter.rect_filled(rect, 0.0, egui::Color32::WHITE);
                    painter.rect_stroke(
                        rect,
                        0.0,
                        egui::Stroke::new(1.0_f32, accent),
                        egui::StrokeKind::Inside,
                    );
                }
            }
            // Rubber-band selection rect (screen space — drawn directly).
            if let Some([x0, y0, x1, y1]) = overlay.marquee {
                let rect = egui::Rect::from_two_pos(egui::pos2(x0, y0), egui::pos2(x1, y1));
                painter.rect_filled(
                    rect,
                    0.0,
                    egui::Color32::from_rgba_unmultiplied(64, 140, 240, 40),
                );
                painter.rect_stroke(
                    rect,
                    0.0,
                    egui::Stroke::new(1.0_f32, accent),
                    egui::StrokeKind::Middle,
                );
            }
        }

        // Gradient tool direction guide: a line from the drag start to the cursor,
        // with endpoint handles (standard). Drawn black-under / white-over so
        // it stays visible on any background.
        if let Some([x0, y0, x1, y1]) = data.tool.gradient_preview {
            let painter = ctx
                .layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("gradient_preview"),
                ))
                .with_clip_rect(canvas_viewport);
            let a = to_screen_pos(x0, y0);
            let b = to_screen_pos(x1, y1);
            painter.add(egui::Shape::line_segment(
                [a, b],
                egui::Stroke::new(3.0_f32, egui::Color32::from_black_alpha(150)),
            ));
            painter.add(egui::Shape::line_segment(
                [a, b],
                egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
            ));
            for p in [a, b] {
                painter.circle(
                    p,
                    3.5,
                    egui::Color32::WHITE,
                    egui::Stroke::new(1.0_f32, egui::Color32::from_black_alpha(170)),
                );
            }
        }

        if data.dialogs.show_preset_dialog {
            let enter_pressed =
                ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
            let esc_pressed =
                ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
            if esc_pressed {
                actions.dialogs.preset_dialog_cancel = true;
            }

            egui::Window::new("Lưu Preset kích thước")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    let size_label = if data.dialogs.preset_dialog_unit == "px" {
                        format!(
                            "{:.0} × {:.0} px  @  {:.0} DPI",
                            data.dialogs.preset_dialog_w,
                            data.dialogs.preset_dialog_h,
                            data.dialogs.preset_dialog_dpi
                        )
                    } else {
                        format!(
                            "{:.2} × {:.2} {}  @  {:.0} DPI",
                            data.dialogs.preset_dialog_w,
                            data.dialogs.preset_dialog_h,
                            data.dialogs.preset_dialog_unit,
                            data.dialogs.preset_dialog_dpi
                        )
                    };
                    ui.label(egui::RichText::new(size_label).color(egui::Color32::GRAY));
                    ui.separator();
                    ui.label("Tên preset:");
                    let mut name = data.dialogs.preset_dialog_name.clone();
                    let resp = ui.text_edit_singleline(&mut name);
                    if resp.changed() {
                        actions.dialogs.preset_dialog_name_changed = Some(name.clone());
                    }
                    resp.request_focus();
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let name_ok = !data.dialogs.preset_dialog_name.trim().is_empty();
                        if ui
                            .add_enabled(
                                name_ok,
                                egui::Button::new("OK").min_size(egui::vec2(60.0, 22.0)),
                            )
                            .clicked()
                            || (enter_pressed && name_ok)
                        {
                            actions.dialogs.preset_dialog_confirm = true;
                        }
                        let cancel_btn = egui::Button::new(
                            egui::RichText::new("Hủy").color(egui::Color32::from_rgb(200, 80, 80)),
                        )
                        .min_size(egui::vec2(60.0, 22.0));
                        if ui.add(cancel_btn).clicked() {
                            actions.dialogs.preset_dialog_cancel = true;
                        }
                    });
                });
        }

        if data.dialogs.show_delete_preset_dialog {
            let esc_pressed =
                ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
            if esc_pressed {
                actions.dialogs.close_delete_preset_dialog = true;
            }

            egui::Window::new("Delete Crop Preset")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    if data.dialogs.user_presets.is_empty() {
                        ui.label(
                            egui::RichText::new("No saved presets.").color(egui::Color32::GRAY),
                        );
                    } else {
                        ui.label("Select a preset to delete:");
                        ui.separator();
                        let mut del_idx: Option<usize> = None;
                        egui::ScrollArea::vertical()
                            .max_height(240.0)
                            .show(ui, |ui| {
                                for (i, preset) in data.dialogs.user_presets.iter().enumerate() {
                                    ui.horizontal(|ui| {
                                        ui.label(&preset.name);
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                let del_btn = egui::Button::new(
                                                    egui::RichText::new("\u{00D7}").color(
                                                        egui::Color32::from_rgb(220, 80, 80),
                                                    ),
                                                )
                                                .min_size(egui::vec2(22.0, 18.0));
                                                if ui
                                                    .add(del_btn)
                                                    .on_hover_text("Delete this preset")
                                                    .clicked()
                                                {
                                                    del_idx = Some(i);
                                                }
                                            },
                                        );
                                    });
                                }
                            });
                        if let Some(i) = del_idx {
                            actions.dialogs.delete_preset = Some(i);
                        }
                    }
                    ui.separator();
                    if ui.button("Close").clicked() {
                        actions.dialogs.close_delete_preset_dialog = true;
                    }
                });
        }

        if data.tool.active_tool == ToolId::Crop && data.tool.crop_rect.is_none() {
            let painter = ctx
                .layer_painter(egui::LayerId::new(
                    CANVAS_TOOL_OVERLAY_ORDER,
                    egui::Id::new("crop_idle_border"),
                ))
                .with_clip_rect(canvas_viewport);
            let canvas_rect = egui::Rect::from_min_max(
                to_screen_pos(0.0, 0.0),
                to_screen_pos(data.doc.canvas_w as f32, data.doc.canvas_h as f32),
            );
            if canvas_rect.intersects(canvas_viewport) {
                painter.rect_stroke(
                    canvas_rect,
                    0.0,
                    egui::Stroke::new(3.0_f32, egui::Color32::from_black_alpha(150)),
                    egui::StrokeKind::Inside,
                );
                painter.rect_stroke(
                    canvas_rect,
                    0.0,
                    egui::Stroke::new(1.25_f32, egui::Color32::WHITE),
                    egui::StrokeKind::Inside,
                );
            }
        }

        if let Some([x0, y0, x1, y1]) = data.tool.crop_rect {
            let rx0 = x0.min(x1);
            let rx1 = x0.max(x1);
            let ry0 = y0.min(y1);
            let ry1 = y0.max(y1);
            let rot = data.tool.crop_rotation;
            let bcx = (rx0 + rx1) * 0.5;
            let bcy = (ry0 + ry1) * 0.5;
            let hw = (rx1 - rx0) * 0.5;
            let hh = (ry1 - ry0) * 0.5;
            let cos_r = rot.cos();
            let sin_r = rot.sin();

            let rot_sp = |lx: f32, ly: f32| -> egui::Pos2 {
                let cx = lx * cos_r - ly * sin_r + bcx;
                let cy = lx * sin_r + ly * cos_r + bcy;
                to_screen_pos(cx, cy)
            };

            let p_tl = rot_sp(-hw, -hh);
            let p_tc = rot_sp(0.0, -hh);
            let p_tr = rot_sp(hw, -hh);
            let p_ml = rot_sp(-hw, 0.0);
            let p_mr = rot_sp(hw, 0.0);
            let p_bl = rot_sp(-hw, hh);
            let p_bc = rot_sp(0.0, hh);
            let p_br = rot_sp(hw, hh);

            let painter = ctx
                .layer_painter(egui::LayerId::new(
                    CANVAS_TOOL_OVERLAY_ORDER,
                    egui::Id::new("crop_overlay"),
                ))
                .with_clip_rect(canvas_viewport);

            let canvas_screen_rect = egui::Rect::from_min_max(
                to_screen_pos(0.0, 0.0),
                to_screen_pos(data.doc.canvas_w as f32, data.doc.canvas_h as f32),
            )
            .intersect(canvas_viewport);
            let crop_screen_rect =
                egui::Rect::from_min_max(to_screen_pos(rx0, ry0), to_screen_pos(rx1, ry1))
                    .intersect(canvas_viewport);
            let shade_rect = canvas_screen_rect.union(crop_screen_rect);
            let dark = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 118);
            if rot.abs() < 0.01 {
                let sp_tl = to_screen_pos(rx0, ry0);
                let sp_br = to_screen_pos(rx1, ry1);

                let v_tl = shade_rect.min;
                let v_br = shade_rect.max;

                let clamp_tl = sp_tl.max(v_tl).min(v_br);
                let clamp_br = sp_br.max(v_tl).min(v_br);

                if shade_rect.width() > 0.0 && shade_rect.height() > 0.0 {
                    let rects = [
                        egui::Rect::from_min_max(v_tl, egui::pos2(v_br.x, clamp_tl.y)),
                        egui::Rect::from_min_max(egui::pos2(v_tl.x, clamp_br.y), v_br),
                        egui::Rect::from_min_max(
                            egui::pos2(v_tl.x, clamp_tl.y),
                            egui::pos2(clamp_tl.x, clamp_br.y),
                        ),
                        egui::Rect::from_min_max(
                            egui::pos2(clamp_br.x, clamp_tl.y),
                            egui::pos2(v_br.x, clamp_br.y),
                        ),
                    ];
                    for r in rects {
                        if r.width() > 0.0 && r.height() > 0.0 {
                            painter.rect_filled(r, 0.0, dark);
                        }
                    }
                }
            } else {
                painter.rect_filled(shade_rect, 0.0, dark);
                painter.add(egui::Shape::convex_polygon(
                    vec![p_tl, p_tr, p_br, p_bl],
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 28),
                    egui::Stroke::NONE,
                ));
            }

            let border_shadow = egui::Stroke::new(3.0_f32, egui::Color32::from_black_alpha(150));
            let border_stroke = egui::Stroke::new(1.25_f32, egui::Color32::WHITE);
            for stroke in [border_shadow, border_stroke] {
                painter.line_segment([p_tl, p_tr], stroke);
                painter.line_segment([p_tr, p_br], stroke);
                painter.line_segment([p_br, p_bl], stroke);
                painter.line_segment([p_bl, p_tl], stroke);
            }

            let inner_stroke = egui::Stroke::new(
                1.35_f32,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 112),
            );
            match data.tool.crop_overlay {
                1 => {
                    for i in 1..3 {
                        let t = i as f32 / 3.0;
                        let lx = -hw + hw * 2.0 * t;
                        let ly = -hh + hh * 2.0 * t;
                        painter.line_segment([rot_sp(lx, -hh), rot_sp(lx, hh)], inner_stroke);
                        painter.line_segment([rot_sp(-hw, ly), rot_sp(hw, ly)], inner_stroke);
                    }
                }
                2 => {
                    for i in 1..5 {
                        let t = i as f32 / 5.0;
                        let lx = -hw + hw * 2.0 * t;
                        let ly = -hh + hh * 2.0 * t;
                        painter.line_segment([rot_sp(lx, -hh), rot_sp(lx, hh)], inner_stroke);
                        painter.line_segment([rot_sp(-hw, ly), rot_sp(hw, ly)], inner_stroke);
                    }
                }
                3 => {
                    let phi = 0.618_f32;
                    for &t in &[phi, 1.0 - phi] {
                        let lx = -hw + hw * 2.0 * t;
                        let ly = -hh + hh * 2.0 * t;
                        painter.line_segment([rot_sp(lx, -hh), rot_sp(lx, hh)], inner_stroke);
                        painter.line_segment([rot_sp(-hw, ly), rot_sp(hw, ly)], inner_stroke);
                    }
                }
                _ => {}
            }

            match data.tool.crop_cursor_hint {
                1 => ctx.set_cursor_icon(egui::CursorIcon::Move),
                2 => ctx.set_cursor_icon(egui::CursorIcon::ResizeNwSe),
                3 => ctx.set_cursor_icon(egui::CursorIcon::ResizeNeSw),
                4 => ctx.set_cursor_icon(egui::CursorIcon::ResizeVertical),
                5 => ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal),
                0 => {
                    if let Some(mouse_pos) = ctx.pointer_hover_pos() {
                        let rotate_icon_pos = egui::pos2(mouse_pos.x + 14.0, mouse_pos.y - 14.0);
                        let font = egui::FontId::proportional(16.0);
                        let shadow_col = egui::Color32::from_black_alpha(200);
                        for (ox, oy) in [(-1.0_f32, 0.0), (1.0, 0.0), (0.0, -1.0_f32), (0.0, 1.0)] {
                            painter.text(
                                egui::pos2(rotate_icon_pos.x + ox, rotate_icon_pos.y + oy),
                                egui::Align2::LEFT_TOP,
                                "⟲",
                                font.clone(),
                                shadow_col,
                            );
                        }
                        painter.text(
                            rotate_icon_pos,
                            egui::Align2::LEFT_TOP,
                            "⟲",
                            font,
                            egui::Color32::WHITE,
                        );
                    }
                }
                _ => {}
            }

            let handle_fill = egui::Color32::from_rgba_unmultiplied(245, 245, 245, 245);
            let handle_border = egui::Color32::from_rgba_unmultiplied(25, 25, 25, 230);
            let hborder = egui::Stroke::new(1.0_f32, handle_border);
            let corner_pts = [p_tl, p_tr, p_bl, p_br];
            let edge_pts = [p_tc, p_ml, p_mr, p_bc];
            for hp in &corner_pts {
                let r = egui::Rect::from_center_size(*hp, egui::vec2(9.0, 9.0));
                painter.rect_filled(r, 1.0, handle_fill);
                painter.rect_stroke(r, 1.0, hborder, egui::StrokeKind::Outside);
            }
            for hp in &edge_pts {
                let r = egui::Rect::from_center_size(*hp, egui::vec2(7.0, 7.0));
                painter.rect_filled(r, 1.0, handle_fill);
                painter.rect_stroke(r, 1.0, hborder, egui::StrokeKind::Outside);
            }

            if rot.abs() > 0.02 {
                let deg = rot.to_degrees();
                let ind_color = egui::Color32::from_rgba_unmultiplied(255, 200, 50, 220);
                let center_sp = to_screen_pos(bcx, bcy);
                painter.circle_stroke(center_sp, 5.0, egui::Stroke::new(1.5_f32, ind_color));
                let label_pos = egui::pos2(p_tr.x + 6.0, p_tr.y - 14.0);
                painter.text(
                    label_pos,
                    egui::Align2::LEFT_BOTTOM,
                    format!("{:.1}°", deg),
                    egui::FontId::proportional(11.0),
                    ind_color,
                );
            }
        }

        // Selected Develop local-mask outline (screen space — see
        // App::build_develop_local_overlay).
        if let Some(ref ov) = data.develop.develop_local_overlay {
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("develop_local_overlay"),
            ));
            draw_develop_local_overlay(&painter, ov);
        }

        if let Some(ref ov) = data.tool.transform_overlay {
            // Canvas tool chrome must stay above the image but below floating
            // windows/dialogs. It also must not escape the canvas when a rotated
            // box extends into panels or modal UI.
            let canvas_screen_rect = egui::Rect::from_min_max(
                to_screen_pos(0.0, 0.0),
                to_screen_pos(data.doc.canvas_w as f32, data.doc.canvas_h as f32),
            );
            let clip_rect = canvas_screen_rect.intersect(canvas_viewport);
            let painter = ctx
                .layer_painter(egui::LayerId::new(
                    CANVAS_TOOL_OVERLAY_ORDER,
                    egui::Id::new("transform_overlay"),
                ))
                .with_clip_rect(clip_rect);

            let bbox_color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 220);
            let handle_fill = egui::Color32::from_rgba_unmultiplied(240, 240, 255, 240);
            let handle_border = egui::Color32::from_rgba_unmultiplied(60, 60, 80, 220);
            let center_fill = egui::Color32::from_rgba_unmultiplied(180, 200, 255, 200);
            let bbox_stroke = egui::Stroke::new(1.0_f32, bbox_color);
            let border_stroke = egui::Stroke::new(1.0_f32, handle_border);

            let cs = |cx: f32, cy: f32| egui::pos2(cx * zoom + ox, cy * zoom + oy);

            let c = ov.corners;
            painter.line_segment([cs(c[0].0, c[0].1), cs(c[1].0, c[1].1)], bbox_stroke);
            painter.line_segment([cs(c[1].0, c[1].1), cs(c[3].0, c[3].1)], bbox_stroke);
            painter.line_segment([cs(c[3].0, c[3].1), cs(c[2].0, c[2].1)], bbox_stroke);
            painter.line_segment([cs(c[2].0, c[2].1), cs(c[0].0, c[0].1)], bbox_stroke);

            let hs = 4.5;
            for &(hx, hy) in &ov.handles {
                let sp = cs(hx, hy);
                let rect = egui::Rect::from_center_size(sp, egui::vec2(hs * 2.0, hs * 2.0));
                painter.rect_filled(rect, 1.0, handle_fill);
                painter.rect_stroke(rect, 1.0, border_stroke, egui::StrokeKind::Outside);
            }

            let cp = cs(ov.center.0, ov.center.1);
            painter.circle_filled(cp, 5.0, center_fill);
            painter.circle_stroke(cp, 5.0, border_stroke);
            painter.line_segment(
                [egui::pos2(cp.x - 4.0, cp.y), egui::pos2(cp.x + 4.0, cp.y)],
                egui::Stroke::new(1.0_f32, handle_border),
            );
            painter.line_segment(
                [egui::pos2(cp.x, cp.y - 4.0), egui::pos2(cp.x, cp.y + 4.0)],
                egui::Stroke::new(1.0_f32, handle_border),
            );

            if data.tool.transform_cursor_hint == 1 {
                if let Some(mouse_pos) = ctx.pointer_hover_pos() {
                    let rotate_icon_pos = egui::pos2(mouse_pos.x + 14.0, mouse_pos.y - 14.0);
                    let font = egui::FontId::proportional(16.0);
                    let shadow_col = egui::Color32::from_black_alpha(200);
                    for (ox, oy) in [(-1.0_f32, 0.0), (1.0, 0.0), (0.0, -1.0_f32), (0.0, 1.0)] {
                        painter.text(
                            egui::pos2(rotate_icon_pos.x + ox, rotate_icon_pos.y + oy),
                            egui::Align2::LEFT_TOP,
                            ph::ARROW_COUNTER_CLOCKWISE,
                            font.clone(),
                            shadow_col,
                        );
                    }
                    painter.text(
                        rotate_icon_pos,
                        egui::Align2::LEFT_TOP,
                        ph::ARROW_COUNTER_CLOCKWISE,
                        font,
                        egui::Color32::WHITE,
                    );
                }
            }
        }

        if data.tool.active_tool == ToolId::SmartSelect {
            if let Some(cursor_pos) = ctx.pointer_hover_pos() {
                let (shift_held, alt_held) = ctx.input(|i| (i.modifiers.shift, i.modifiers.alt));
                let symbol: &str = match (shift_held, alt_held) {
                    (true, true) => "\u{00D7}",
                    (true, false) => "+",
                    (false, true) => "-",
                    _ => "",
                };
                if !symbol.is_empty() {
                    let painter = ctx.layer_painter(egui::LayerId::new(
                        egui::Order::Foreground,
                        egui::Id::new("wand_modifier"),
                    ));
                    let text_pos = egui::pos2(cursor_pos.x + 10.0, cursor_pos.y - 18.0);
                    let shadow_col = egui::Color32::from_black_alpha(180);
                    painter.text(
                        text_pos + egui::vec2(1.0, 1.0),
                        egui::Align2::LEFT_TOP,
                        symbol,
                        egui::FontId::monospace(14.0),
                        shadow_col,
                    );
                    let sym_col = if shift_held {
                        egui::Color32::from_rgb(80, 200, 80)
                    } else {
                        egui::Color32::from_rgb(220, 80, 80)
                    };
                    painter.text(
                        text_pos,
                        egui::Align2::LEFT_TOP,
                        symbol,
                        egui::FontId::monospace(14.0),
                        sym_col,
                    );
                }
            }
        }

        if matches!(data.tool.active_tool, ToolId::Clone | ToolId::Repair) {
            if let (Some(cursor_pos), Some(thumb)) = (
                ctx.pointer_hover_pos(),
                data.tool.clone_source_thumbnail.as_ref(),
            ) {
                paint_clone_source_thumbnail(ctx, data, cursor_pos, thumb);
            }
        }

        if let Some((mx, my)) = data.tool.transform_ctx_menu_pos {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                actions.tool.transform_ctx_menu_close = true;
            }
            let popup_id = egui::Id::new("transform_ctx_menu");
            let screen = ctx.content_rect();
            let menu_w = 210.0;
            let menu_h = 380.0;
            let menu_pos = egui::pos2(
                mx.min(screen.max.x - menu_w).max(screen.min.x),
                my.min(screen.max.y - menu_h).max(screen.min.y + 28.0),
            );
            let pal = data.chrome.theme_mode.palette();
            let resp = egui::Area::new(popup_id)
                .fixed_pos(menu_pos)
                .order(egui::Order::Tooltip)
                .show(ctx, |ui| {
                    egui::Frame::new()
                        .fill(pal.panel_bg)
                        .stroke(egui::Stroke::new(1.0_f32, pal.border_subtle))
                        .corner_radius(4.0)
                        .inner_margin(egui::Margin::same(4))
                        .show(ui, |ui| {
                            ui.set_min_width(menu_w - 8.0);
                            ui.label(egui::RichText::new("Free Transform").strong());
                            ui.separator();
                            if ui.selectable_label(false, "Reset Transform").clicked() {
                                actions.tool.transform_reset = true;
                            }
                            ui.separator();
                            if ui.selectable_label(false, "Skew").clicked() {
                                actions.tool.set_transform_mode =
                                    Some(crate::app::state::TransformMode::Skew);
                            }
                            if ui.selectable_label(false, "Distort").clicked() {
                                actions.tool.set_transform_mode =
                                    Some(crate::app::state::TransformMode::Distort);
                            }
                            if ui.selectable_label(false, "Perspective").clicked() {
                                actions.tool.set_transform_mode =
                                    Some(crate::app::state::TransformMode::Perspective);
                            }
                            if ui.selectable_label(false, "Warp…").clicked() {
                                actions.tool.transform_warp = true;
                            }
                            ui.separator();
                            if ui.selectable_label(false, "Flip Horizontal").clicked() {
                                actions.tool.transform_flip_h = true;
                            }
                            if ui.selectable_label(false, "Flip Vertical").clicked() {
                                actions.tool.transform_flip_v = true;
                            }
                            ui.separator();
                            if ui.selectable_label(false, "Rotate 90° Clockwise").clicked() {
                                actions.tool.transform_rot_90cw = true;
                            }
                            if ui
                                .selectable_label(false, "Rotate 90° Counter Clockwise")
                                .clicked()
                            {
                                actions.tool.transform_rot_90ccw = true;
                            }
                            if ui.selectable_label(false, "Rotate 180°").clicked() {
                                actions.tool.transform_rot_180 = true;
                            }
                            ui.separator();
                            if ui
                                .selectable_label(false, "Cancel Transform    Esc")
                                .clicked()
                            {
                                actions.tool.transform_cancel = true;
                            }
                            if ui
                                .selectable_label(false, "Apply Transform     Enter")
                                .clicked()
                            {
                                actions.tool.transform_commit = true;
                            }
                        });
                });
            let clicked_outside = ctx.input(|i| i.pointer.any_click()) && !resp.response.hovered();
            if clicked_outside {
                actions.tool.transform_ctx_menu_close = true;
            }
        }

        if let Some((mx, my)) = data.sel.selection_ctx_menu_pos {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                actions.sel.selection_ctx_menu_close = true;
            }

            let screen = ctx.content_rect();
            let menu_w = 220.0;
            let px = mx.min(screen.max.x - menu_w).max(0.0);
            let py = my.min(screen.max.y - 360.0).max(28.0);
            let has_sel = data.sel.has_selection;
            let has_doc = data.doc.has_doc;
            let pal = data.chrome.theme_mode.palette();

            let resp = egui::Area::new(egui::Id::new("selection_ctx_menu"))
                .fixed_pos(egui::pos2(px, py))
                .order(egui::Order::Tooltip)
                .show(ctx, |ui| {
                    egui::Frame::new()
                        .fill(pal.panel_bg)
                        .stroke(egui::Stroke::new(1.0_f32, pal.border_subtle))
                        .inner_margin(egui::Margin::symmetric(0, 4))
                        .corner_radius(0.0)
                        .show(ui, |ui| {
                            ui.set_min_width(menu_w);
                            ui.set_max_width(menu_w);
                            ui.spacing_mut().item_spacing.y = 0.0;
                            let mut clicked = false;

                            if flat_context_menu_item(
                                ui,
                                menu_w,
                                has_doc,
                                "Select All",
                                Some("Ctrl+A"),
                            ) {
                                actions.sel.select_all = true;
                                clicked = true;
                            }
                            if flat_context_menu_item(
                                ui,
                                menu_w,
                                has_sel,
                                "Deselect",
                                Some("Ctrl+D"),
                            ) {
                                actions.sel.deselect = true;
                                clicked = true;
                            }
                            if flat_context_menu_item(
                                ui,
                                menu_w,
                                has_sel,
                                "Select Inverse",
                                Some("Shift+F7"),
                            ) {
                                actions.sel.invert_selection = true;
                                clicked = true;
                            }
                            flat_context_menu_separator(ui, menu_w);
                            if flat_context_menu_item(
                                ui,
                                menu_w,
                                has_sel,
                                "Feather...",
                                Some("Shift+F6"),
                            ) {
                                actions.sel.show_feather_dialog = Some(true);
                                clicked = true;
                            }
                            if flat_context_menu_item(ui, menu_w, has_sel, "Expand 1 px", None) {
                                actions.sel.selection_grow = Some(1);
                                clicked = true;
                            }
                            if flat_context_menu_item(ui, menu_w, has_sel, "Contract 1 px", None) {
                                actions.sel.selection_shrink = Some(1);
                                clicked = true;
                            }
                            flat_context_menu_separator(ui, menu_w);
                            if flat_context_menu_item(
                                ui,
                                menu_w,
                                has_doc,
                                "Fill with Foreground",
                                None,
                            ) {
                                actions.doc.fill_foreground = true;
                                clicked = true;
                            }
                            if flat_context_menu_item(ui, menu_w, has_sel, "Stroke...", None) {
                                actions.sel.show_stroke_dialog = Some(true);
                                clicked = true;
                            }
                            if flat_context_menu_item(ui, menu_w, has_sel, "Smart Fill...", None) {
                                actions.dialogs.smart_fill_fill = true;
                                clicked = true;
                            }
                            flat_context_menu_separator(ui, menu_w);
                            if flat_context_menu_item(
                                ui,
                                menu_w,
                                has_sel,
                                "Layer via Copy",
                                Some("Ctrl+J"),
                            ) {
                                actions.layers.layer_via_copy = true;
                                clicked = true;
                            }
                            if flat_context_menu_item(ui, menu_w, has_sel, "Copy", Some("Ctrl+C")) {
                                actions.doc.copy = true;
                                clicked = true;
                            }
                            if flat_context_menu_item(ui, menu_w, has_sel, "Cut", Some("Ctrl+X")) {
                                actions.doc.cut = true;
                                clicked = true;
                            }
                            if flat_context_menu_item(ui, menu_w, has_doc, "Paste", Some("Ctrl+V"))
                            {
                                actions.doc.paste = true;
                                clicked = true;
                            }
                            flat_context_menu_separator(ui, menu_w);
                            if flat_context_menu_item(
                                ui,
                                menu_w,
                                has_doc,
                                "Free Transform",
                                Some("Ctrl+T"),
                            ) {
                                actions.tool.start_transform = true;
                                clicked = true;
                            }
                            clicked
                        })
                        .inner
                });

            if resp.inner {
                actions.sel.selection_ctx_menu_close = true;
            }
            let clicked_outside = ctx.input(|i| i.pointer.any_click()) && !resp.response.hovered();
            if clicked_outside {
                actions.sel.selection_ctx_menu_close = true;
            }
        }

        if let Some((mx, my)) = data.tool.brush_popup_pos {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                actions.tool.brush_popup_close = true;
            }

            let screen = ctx.content_rect();
            let px = mx.min(screen.max.x - 318.0).max(0.0);
            let py = my.min(screen.max.y - 330.0).max(28.0);

            let resp = egui::Area::new(egui::Id::new("brush_popup"))
                .fixed_pos(egui::pos2(px, py))
                .order(egui::Order::Tooltip)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        // Brush-tip popup for the scrub tools (Smudge / Dodge / Burn):
                        // size + hardness + the tool's own strength control. They have no
                        // preset library, so this is a compact tip editor and returns early.
                        if data.tool.active_tool == ToolId::Smudge {
                            ui.set_min_width(300.0);
                            ui.set_max_width(300.0);
                            ui.label(egui::RichText::new("Smudge Tip").strong());
                            ui.add_space(2.0);
                            let mut size = data.tool.smudge_size;
                            if widgets::dev_slider(ui, "Size", &mut size, 1.0..=1000.0) {
                                actions.tool.set_smudge_size = Some(size);
                            }
                            let mut hard = data.tool.smudge_hardness * 100.0;
                            if widgets::dev_slider(ui, "Hardness", &mut hard, 0.0..=100.0) {
                                actions.tool.set_smudge_hardness = Some(hard / 100.0);
                            }
                            let mut st = data.tool.smudge_strength * 100.0;
                            if widgets::dev_slider(ui, "Strength", &mut st, 0.0..=100.0) {
                                actions.tool.set_smudge_strength = Some(st / 100.0);
                            }
                            return;
                        }
                        if matches!(data.tool.active_tool, ToolId::Dodge | ToolId::Burn) {
                            ui.set_min_width(300.0);
                            ui.set_max_width(300.0);
                            let title = if data.tool.active_tool == ToolId::Burn {
                                "Burn Tip"
                            } else {
                                "Dodge Tip"
                            };
                            ui.label(egui::RichText::new(title).strong());
                            ui.add_space(2.0);
                            let mut size = data.tool.dodge_size;
                            if widgets::dev_slider(ui, "Size", &mut size, 1.0..=1000.0) {
                                actions.tool.set_dodge_size = Some(size);
                            }
                            let mut hard = data.tool.dodge_hardness * 100.0;
                            if widgets::dev_slider(ui, "Hardness", &mut hard, 0.0..=100.0) {
                                actions.tool.set_dodge_hardness = Some(hard / 100.0);
                            }
                            let mut exp = data.tool.dodge_exposure * 100.0;
                            if widgets::dev_slider(ui, "Exposure", &mut exp, 0.0..=100.0) {
                                actions.tool.set_dodge_exposure = Some(exp / 100.0);
                            }
                            ui.label("Range");
                            ui.horizontal(|ui| {
                                let mut range = data.tool.dodge_range;
                                for (v, name) in
                                    [(0u8, "Shadows"), (1, "Midtones"), (2, "Highlights")]
                                {
                                    if ui.selectable_value(&mut range, v, name).changed() {
                                        actions.tool.set_dodge_range = Some(range);
                                    }
                                }
                            });
                            return;
                        }
                        if matches!(data.tool.active_tool, ToolId::Clone | ToolId::Repair) {
                            ui.set_min_width(300.0);
                            ui.set_max_width(300.0);
                            ui.label(
                                egui::RichText::new(if data.tool.active_tool == ToolId::Repair {
                                    "Repair Brush Tip"
                                } else {
                                    "Clone Tip"
                                })
                                .strong(),
                            );
                            ui.add_space(2.0);
                            ui.horizontal(|ui| {
                                for (idx, label) in [(0usize, "Soft"), (1usize, "Hard")] {
                                    let Some(preset) = crate::tools::brush::BRUSH_PRESETS.get(idx)
                                    else {
                                        continue;
                                    };
                                    let selected =
                                        (data.tool.clone_hardness - preset.hardness).abs() < 0.001;
                                    let (rect, _) = ui.allocate_exact_size(
                                        egui::vec2(28.0, 28.0),
                                        egui::Sense::hover(),
                                    );
                                    draw_brush_preview(
                                        ui.painter(),
                                        rect,
                                        preset.hardness,
                                        [210, 210, 210, 255],
                                    );
                                    if ui
                                        .add_sized(
                                            egui::vec2(88.0, 28.0),
                                            egui::Button::selectable(selected, label),
                                        )
                                        .clicked()
                                    {
                                        actions.tool.set_clone_preset = Some(idx);
                                    }
                                }
                            });
                            ui.separator();
                            let mut size = data.tool.clone_size;
                            if widgets::dev_slider(ui, "Size", &mut size, 1.0..=1000.0) {
                                actions.tool.set_clone_size = Some(size);
                            }
                            let mut hard = data.tool.clone_hardness * 100.0;
                            if widgets::dev_slider(ui, "Hardness", &mut hard, 0.0..=100.0) {
                                actions.tool.set_clone_hardness = Some(hard / 100.0);
                            }
                            let mut op = data.tool.clone_opacity * 100.0;
                            if widgets::dev_slider(ui, "Opacity", &mut op, 1.0..=100.0) {
                                actions.tool.set_clone_opacity = Some(op / 100.0);
                            }
                            let mut spacing = data.tool.clone_spacing * 100.0;
                            if widgets::dev_slider(ui, "Spacing", &mut spacing, 1.0..=200.0) {
                                actions.tool.set_clone_spacing = Some(spacing / 100.0);
                            }
                            ui.horizontal(|ui| {
                                let mut aligned = data.tool.clone_aligned;
                                if ui.checkbox(&mut aligned, "Aligned").changed() {
                                    actions.tool.set_clone_aligned = Some(aligned);
                                }
                                let mut merged = data.tool.clone_sample_merged;
                                if ui.checkbox(&mut merged, "Sample Merged").changed() {
                                    actions.tool.set_clone_sample_merged = Some(merged);
                                }
                            });
                            if data.tool.active_tool == ToolId::Repair {
                                ui.separator();
                                let mut ca = data.tool.clone_smart_fill;
                                if ui.checkbox(&mut ca, "Smart").changed() {
                                    actions.tool.set_clone_smart_fill = Some(ca);
                                }
                            }
                            return;
                        }

                        let is_eraser = data.tool.active_tool == ToolId::Eraser;
                        let preview_color = if is_eraser {
                            [160, 160, 160, 255]
                        } else {
                            data.tool.brush_color
                        };
                        ui.set_min_width(300.0);
                        ui.set_max_width(300.0);
                        ui.label(
                            egui::RichText::new(if is_eraser {
                                "Eraser Presets"
                            } else {
                                "Brush Presets"
                            })
                            .strong(),
                        );
                        ui.add_space(2.0);

                        egui::ScrollArea::vertical()
                            .max_height(150.0)
                            .show(ui, |ui| {
                                for (i, p) in crate::tools::brush::BRUSH_PRESETS.iter().enumerate()
                                {
                                    let selected = data.tool.brush_preset_idx == i;
                                    let row = ui
                                        .horizontal(|ui| {
                                            let (rect, _) = ui.allocate_exact_size(
                                                egui::vec2(28.0, 28.0),
                                                egui::Sense::hover(),
                                            );
                                            draw_brush_preview(
                                                ui.painter(),
                                                rect,
                                                p.hardness,
                                                preview_color,
                                            );
                                            ui.add_sized(
                                                egui::vec2(150.0, 28.0),
                                                egui::Button::selectable(selected, p.name),
                                            )
                                        })
                                        .inner;
                                    if row.clicked() {
                                        actions.tool.select_brush_preset = Some(i);
                                    }
                                }
                            });

                        ui.separator();

                        let mut size = data.tool.brush_size;
                        if widgets::dev_slider(ui, "Size", &mut size, 1.0..=1000.0) {
                            actions.tool.brush_size = Some(size);
                        }
                        let mut hard = data.tool.brush_hardness * 100.0;
                        if widgets::dev_slider(ui, "Hardness", &mut hard, 0.0..=100.0) {
                            actions.tool.brush_hardness = Some(hard / 100.0);
                        }
                        let mut op = data.tool.brush_opacity * 100.0;
                        if widgets::dev_slider(ui, "Opacity", &mut op, 1.0..=100.0) {
                            actions.tool.brush_opacity = Some(op / 100.0);
                        }
                        let mut flow = data.tool.brush_flow * 100.0;
                        if widgets::dev_slider(ui, "Flow", &mut flow, 1.0..=100.0) {
                            actions.tool.brush_flow = Some(flow / 100.0);
                        }
                        let mut spacing = data.tool.brush_spacing * 100.0;
                        if widgets::dev_slider(ui, "Spacing", &mut spacing, 1.0..=200.0) {
                            actions.tool.brush_spacing = Some(spacing / 100.0);
                        }
                        let mut sm = data.tool.brush_smoothing * 100.0;
                        if widgets::dev_slider(ui, "Smoothing", &mut sm, 0.0..=100.0) {
                            actions.tool.brush_smoothing = Some(sm / 100.0);
                        }
                    });
                });

            let clicked_outside = ctx.input(|i| i.pointer.any_click()) && !resp.response.hovered();
            if clicked_outside {
                actions.tool.brush_popup_close = true;
            }
        }

        if data.tool.text_editing {
            let (mx, my) = data.tool.text_overlay_pos;
            let screen_font =
                (data.tool.text_font_px * zoom / ctx.pixels_per_point()).clamp(6.0, 800.0);
            let family_name = data.tool.text_font_family.egui_family_name();
            // Must key off what egui actually has installed (fonts register on
            // demand) — an unregistered FontFamily::Name panics inside egui.
            let egui_family = if data.tool.text_font_registered {
                egui::FontFamily::Name(family_name.into())
            } else {
                egui::FontFamily::Proportional
            };
            let color = egui::Color32::from_rgba_unmultiplied(
                data.tool.text_color[0],
                data.tool.text_color[1],
                data.tool.text_color[2],
                (data.tool.text_color[3] as f32 * data.tool.text_opacity.clamp(0.0, 1.0))
                    .round()
                    .clamp(0.0, 255.0) as u8,
            );
            let mut buf = data.tool.text_buffer.clone();
            let preview_td = crate::core::text::TextData {
                content: buf.clone(),
                font_px: data.tool.text_font_px,
                color: data.tool.text_color,
                font_family: data.tool.text_font_family.clone(),
                align: data.tool.text_align,
                line_height: data.tool.text_line_height,
                bold: data.tool.text_bold,
                italic: data.tool.text_italic,
                underline: data.tool.text_underline,
                tracking_px: data.tool.text_tracking_px,
                opacity: data.tool.text_opacity,
                stretch_x: data.tool.text_stretch_x,
                rotation_deg: data.tool.text_rotation_deg,
                flip_x: data.tool.text_flip_x,
                flip_y: data.tool.text_flip_y,
                // Only valid while the buffer matches; a mid-frame edit falls
                // back to uniform for that frame (re-synced next frame).
                glyph_styles: if data.tool.text_glyph_styles.len() == buf.chars().count() {
                    data.tool.text_glyph_styles.as_ref().clone()
                } else {
                    Vec::new()
                },
            };
            let te_id = egui::Id::new("text_overlay_te");
            let saved_text_range =
                egui::TextEdit::load_state(ctx, te_id).and_then(|state| state.cursor.char_range());
            let has_text_selection = saved_text_range
                .as_ref()
                .is_some_and(|range| !range.is_empty())
                || data.tool.text_selection.is_some();
            // Photoshop-style spacing shortcuts while a text range is selected.
            // Consume them before TextEdit sees the event; otherwise Alt+Arrow
            // may move/collapse its cursor instead of preserving the selection.
            if has_text_selection {
                let alt = egui::Modifiers::ALT;
                if ctx.input_mut(|i| i.consume_key(alt, egui::Key::ArrowLeft)) {
                    actions.tool.set_text_tracking_px =
                        Some((data.tool.text_tracking_px - 1.0).clamp(-200.0, 500.0));
                }
                if ctx.input_mut(|i| i.consume_key(alt, egui::Key::ArrowRight)) {
                    actions.tool.set_text_tracking_px =
                        Some((data.tool.text_tracking_px + 1.0).clamp(-200.0, 500.0));
                }
                if ctx.input_mut(|i| i.consume_key(alt, egui::Key::ArrowUp)) {
                    actions.tool.set_text_line_height =
                        Some((data.tool.text_line_height - 0.05).clamp(0.5, 4.0));
                }
                if ctx.input_mut(|i| i.consume_key(alt, egui::Key::ArrowDown)) {
                    actions.tool.set_text_line_height =
                        Some((data.tool.text_line_height + 0.05).clamp(0.5, 4.0));
                }
            }
            let text_scale = (zoom / ctx.pixels_per_point()).max(0.001);
            let text_origin = egui::pos2(mx, my);
            let preview_image =
                if let Some((texture, w, h)) = text_preview_texture(ctx, &preview_td) {
                    let corners = text_preview_corners(
                        text_origin,
                        w,
                        h,
                        text_scale,
                        data.tool.text_rotation_deg,
                        data.tool.text_flip_x,
                        data.tool.text_flip_y,
                    );
                    let bounds = text_preview_bounds(&corners);
                    Some((texture.id(), w, h, corners, bounds))
                } else {
                    None
                };
            let preview_drawn = preview_image.is_some();
            let edit_width = preview_image
                .as_ref()
                .map_or(screen_font * 8.0, |(_, w, _, _, _)| {
                    *w as f32 * text_scale + screen_font * 0.5
                })
                .clamp(screen_font * 2.0, 50000.0);
            let edit_color = if preview_drawn {
                egui::Color32::TRANSPARENT
            } else {
                color
            };
            let text_align = match data.tool.text_align {
                crate::core::text::TextAlign::Left => egui::Align::LEFT,
                crate::core::text::TextAlign::Center => egui::Align::Center,
                crate::core::text::TextAlign::Right => egui::Align::RIGHT,
            };
            let font_id = egui::FontId::new(screen_font, egui_family);
            let tracking_points = data.tool.text_tracking_px * text_scale;
            let line_height_points =
                (data.tool.text_font_px * data.tool.text_line_height.max(0.5) * text_scale)
                    .max(screen_font * 0.75);
            let underline = if data.tool.text_underline {
                egui::Stroke::new((screen_font / 16.0).max(1.0), edit_color)
            } else {
                egui::Stroke::NONE
            };
            let mut layouter = {
                let font_id = font_id.clone();
                move |ui: &egui::Ui, text: &dyn egui::TextBuffer, _wrap_width: f32| {
                    let format = egui::TextFormat {
                        font_id: font_id.clone(),
                        extra_letter_spacing: tracking_points,
                        line_height: Some(line_height_points),
                        color: edit_color,
                        italics: data.tool.text_italic,
                        underline,
                        ..Default::default()
                    };
                    let mut job =
                        egui::text::LayoutJob::simple_format(text.as_str().to_owned(), format);
                    job.wrap.max_width = f32::INFINITY;
                    job.halign = text_align;
                    ui.fonts_mut(|f| f.layout_job(job))
                }
            };
            let drag_id = egui::Id::new("text_overlay_drag_grab_offset");
            if let Some((texture_id, _, _, corners, _)) = preview_image.as_ref() {
                let preview_painter = ctx
                    .layer_painter(egui::LayerId::new(
                        egui::Order::Middle,
                        egui::Id::new("text_overlay_raster_preview_layer"),
                    ))
                    .with_clip_rect(canvas_viewport);
                paint_text_preview_texture(&preview_painter, *texture_id, *corners);
            }

            let handle_resp = egui::Area::new(egui::Id::new("text_overlay_drag_handle"))
                .fixed_pos(egui::pos2(mx - 24.0, my - 4.0))
                .order(egui::Order::Middle)
                .show(ctx, |ui| {
                    let (rect, response) = ui
                        .allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::click_and_drag());
                    let draw_rect =
                        egui::Rect::from_center_size(rect.center(), egui::vec2(18.0, 18.0));
                    let fill = if response.dragged() {
                        egui::Color32::from_gray(92)
                    } else if response.hovered() {
                        egui::Color32::from_rgb(58, 58, 58)
                    } else {
                        egui::Color32::from_rgba_unmultiplied(38, 38, 38, 210)
                    };
                    ui.painter().rect_filled(draw_rect, 2.0, fill);
                    ui.painter().rect_stroke(
                        draw_rect,
                        2.0,
                        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(120)),
                        egui::StrokeKind::Outside,
                    );
                    let center = draw_rect.center();
                    let stroke = egui::Stroke::new(1.0_f32, egui::Color32::from_gray(230));
                    ui.painter().line_segment(
                        [
                            egui::pos2(draw_rect.left() + 4.0, center.y),
                            egui::pos2(draw_rect.right() - 4.0, center.y),
                        ],
                        stroke,
                    );
                    ui.painter().line_segment(
                        [
                            egui::pos2(center.x, draw_rect.top() + 4.0),
                            egui::pos2(center.x, draw_rect.bottom() - 4.0),
                        ],
                        stroke,
                    );
                    if response.hovered() || response.dragged() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
                        actions.tool.text_drag_handle_hovered = true;
                    }
                    response.on_hover_text("Move text")
                })
                .inner;

            if handle_resp.drag_started() {
                if let Some(pos) = handle_resp.interact_pointer_pos() {
                    let pointer_canvas = ((pos.x - ox) / zoom, (pos.y - oy) / zoom);
                    let grab_offset = (
                        pointer_canvas.0 - data.tool.text_overlay_origin.0 as f32,
                        pointer_canvas.1 - data.tool.text_overlay_origin.1 as f32,
                    );
                    ctx.data_mut(|d| d.insert_temp(drag_id, grab_offset));
                }
            }
            if handle_resp.dragged() {
                if let Some(pos) = handle_resp.interact_pointer_pos() {
                    let grab_offset = ctx
                        .data(|d| d.get_temp::<(f32, f32)>(drag_id))
                        .unwrap_or((9.0 / zoom, 8.0 / zoom));
                    let cx = ((pos.x - ox) / zoom - grab_offset.0)
                        .round()
                        .clamp(0.0, data.doc.canvas_w as f32) as i32;
                    let cy = ((pos.y - oy) / zoom - grab_offset.1)
                        .round()
                        .clamp(0.0, data.doc.canvas_h as f32) as i32;
                    actions.tool.text_move_origin = Some((cx, cy));
                }
            }

            // The raster pads (font_px*0.2) around the glyphs; shift the
            // invisible TextEdit by the same amount so egui's click→caret
            // mapping lines up with the drawn text for all three alignments.
            let pad_screen = (data.tool.text_font_px * 0.2).ceil() * text_scale;
            let inner_width = (edit_width - 2.0 * pad_screen).max(screen_font);
            let text_output = egui::Area::new(egui::Id::new("text_overlay"))
                .fixed_pos(egui::pos2(mx, my))
                .order(egui::Order::Middle)
                .show(ctx, |ui| {
                    ui.set_min_width(edit_width);
                    ui.set_max_width(edit_width);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.add_space(pad_screen);
                        ui.scope(|ui| {
                            let visuals = ui.visuals_mut();
                            visuals.selection.bg_fill = egui::Color32::TRANSPARENT;
                            visuals.selection.stroke.color = egui::Color32::TRANSPARENT;
                            visuals.text_cursor.stroke.color = egui::Color32::TRANSPARENT;
                            egui::TextEdit::multiline(&mut buf)
                                .id(te_id)
                                .font(font_id)
                                .text_color(edit_color)
                                .frame(egui::Frame::NONE)
                                .desired_width(inner_width)
                                .desired_rows(1)
                                .horizontal_align(text_align)
                                .layouter(&mut layouter)
                                .hint_text("Type…")
                                .show(ui)
                        })
                        .inner
                    })
                    .inner
                })
                .inner;
            let resp = &text_output.response;
            // Caret/selection geometry follows the buffer as edited this frame;
            // `preview_td` still holds the pre-frame content.
            let mut caret_td = preview_td;
            if caret_td.content != buf {
                caret_td.content = buf.clone();
            }
            let raw_text_range = text_output
                .cursor_range
                .or_else(|| text_output.state.cursor.char_range())
                .or(saved_text_range);
            let cached_text_range = data.tool.text_selection.as_ref().and_then(|range| {
                use egui::text::{CCursor, CCursorRange};
                let n = buf.chars().count();
                let start = range.start.min(n);
                let end = range.end.min(n);
                if start >= end {
                    return None;
                }
                let caret = data.tool.text_caret.unwrap_or(end).min(n);
                let (primary, secondary) = if caret <= start {
                    (start, end)
                } else {
                    (end, start)
                };
                Some(CCursorRange::two(
                    CCursor::new(primary),
                    CCursor::new(secondary),
                ))
            });
            let mut active_text_range = match raw_text_range {
                Some(range) if !range.is_empty() => Some(range),
                Some(range) if resp.has_focus() => Some(range),
                Some(range) => cached_text_range.or(Some(range)),
                None => cached_text_range,
            };

            // Pointer→caret mapping in raster space. egui's built-in hit-test
            // runs on its own galley whose metrics drift from the drawn raster
            // (clamped screen font, kerning/padding differences), so clicks and
            // drag-selections are resolved with the rasterizer's layout math
            // and written back into the TextEdit state.
            if preview_drawn {
                let anchor_id = egui::Id::new("text_overlay_pointer_anchor");
                let primary_down =
                    ctx.input(|i| i.pointer.button_down(egui::PointerButton::Primary));
                // The drag anchor only lives for the duration of one held
                // drag; a stale anchor would make every later click anywhere
                // in the app rewrite the selection.
                if !primary_down {
                    ctx.data_mut(|d| d.remove::<usize>(anchor_id));
                }
                let mut override_range: Option<egui::text::CCursorRange> = None;
                let pointer_pos = ctx.input(|i| i.pointer.interact_pos()).filter(|pos| {
                    // Ignore the pointer while another egui layer (dialog,
                    // popup, floating window) sits on top of it, and while the
                    // colour dialog owns canvas clicks (eyedropper).
                    !data.dialogs.show_paint_color_dialog
                        && !ctx.layer_id_at(*pos).is_some_and(|l| {
                            l.order != egui::Order::Background
                                && l.id != egui::Id::new("text_overlay")
                        })
                });
                if let Some(pos) = pointer_pos {
                    let (tx, ty) = text_screen_to_local(
                        text_origin,
                        pos,
                        text_scale,
                        data.tool.text_rotation_deg,
                        data.tool.text_flip_x,
                        data.tool.text_flip_y,
                    );
                    let pointer_in_text = preview_image.as_ref().is_some_and(|(_, w, h, _, _)| {
                        tx >= 0.0 && ty >= 0.0 && tx <= *w as f32 && ty <= *h as f32
                    });
                    let primary_pressed = ctx.input(|i| i.pointer.primary_pressed());
                    let continuing_drag = ctx.data(|d| d.get_temp::<usize>(anchor_id)).is_some();
                    // The preview texture is already horizontally stretched,
                    // while layout/caret geometry stays in upright text space.
                    // Undo that stretch before resolving the character index.
                    let layout_x = tx / data.tool.text_stretch_x.max(0.001);
                    if let Some(idx) = crate::core::text::char_index_at_pos(&caret_td, layout_x, ty)
                    {
                        use egui::text::{CCursor, CCursorRange};
                        if pointer_in_text
                            && ctx.input(|i| {
                                i.pointer
                                    .button_triple_clicked(egui::PointerButton::Primary)
                            })
                        {
                            resp.request_focus();
                            let (s, e) = crate::core::text::line_char_span(&caret_td.content, idx);
                            override_range =
                                Some(CCursorRange::two(CCursor::new(s), CCursor::new(e)));
                        } else if pointer_in_text
                            && ctx.input(|i| {
                                i.pointer
                                    .button_double_clicked(egui::PointerButton::Primary)
                            })
                        {
                            resp.request_focus();
                            let (s, e) = crate::core::text::word_char_span(&caret_td.content, idx);
                            override_range =
                                Some(CCursorRange::two(CCursor::new(s), CCursor::new(e)));
                        } else if primary_down && (pointer_in_text || continuing_drag) {
                            if pointer_in_text && primary_pressed {
                                resp.request_focus();
                            }
                            let anchor = if primary_pressed {
                                let anchor = if ctx.input(|i| i.modifiers.shift) {
                                    active_text_range.map(|r| r.secondary.index).unwrap_or(idx)
                                } else {
                                    idx
                                };
                                ctx.data_mut(|d| d.insert_temp(anchor_id, anchor));
                                anchor
                            } else {
                                ctx.data(|d| d.get_temp::<usize>(anchor_id)).unwrap_or(idx)
                            };
                            override_range = Some(CCursorRange {
                                primary: CCursor::new(idx),
                                secondary: CCursor::new(anchor),
                                h_pos: None,
                            });
                        }
                    }
                }
                if let Some(range) = override_range {
                    let mut state = text_output.state.clone();
                    state.cursor.set_char_range(Some(range));
                    state.store(ctx, te_id);
                    active_text_range = Some(range);
                }
            }
            let selection_now = active_text_range.is_some_and(|range| !range.is_empty());
            if selection_now != has_text_selection {
                ctx.request_repaint();
            }
            if let Some(range) = active_text_range {
                let sorted = range.as_sorted_char_range();
                let selection = (sorted.start < sorted.end).then_some(sorted);
                if resp.has_focus() || selection.is_some() {
                    actions.tool.text_cursor = Some((selection, Some(range.primary.index)));
                }
            }
            if let Some(range) = active_text_range {
                let overlay_painter = ctx
                    .layer_painter(egui::LayerId::new(
                        egui::Order::Middle,
                        egui::Id::new("text_overlay_selection_preview"),
                    ))
                    .with_clip_rect(canvas_viewport);
                let origin = egui::pos2(mx, my);
                if range.is_empty() {
                    if resp.has_focus() {
                        ctx.request_repaint_after(std::time::Duration::from_millis(250));
                        if let Some(rect) =
                            crate::core::text::caret_rect(&caret_td, range.primary.index)
                        {
                            paint_text_caret_overlay(
                                &overlay_painter,
                                origin,
                                rect,
                                text_scale,
                                data.tool.text_rotation_deg,
                                data.tool.text_flip_x,
                                data.tool.text_flip_y,
                                data.tool.text_stretch_x,
                                screen_font,
                                ctx.input(|i| i.time),
                            );
                        }
                    }
                } else {
                    let accent = ctx.style().visuals.selection.stroke.color;
                    let fill = egui::Color32::from_rgba_unmultiplied(
                        accent.r(),
                        accent.g(),
                        accent.b(),
                        84,
                    );
                    let [start, end] = range.sorted_cursors();
                    if let Some(rects) =
                        crate::core::text::selection_rects(&caret_td, start.index..end.index)
                    {
                        paint_text_selection_overlay(
                            &overlay_painter,
                            origin,
                            &rects,
                            text_scale,
                            data.tool.text_rotation_deg,
                            data.tool.text_flip_x,
                            data.tool.text_flip_y,
                            data.tool.text_stretch_x,
                            fill,
                        );
                    }
                }
            }

            if data.tool.text_focus_pending {
                ctx.memory_mut(|m| m.request_focus(te_id));
                if !ctx.input(|i| i.pointer.button_down(egui::PointerButton::Primary)) {
                    actions.tool.consume_text_focus = true;
                }
            }
            if buf != data.tool.text_buffer {
                actions.tool.text_buffer = Some(buf);
            }
            if !data.tool.text_focus_pending
                && !data.dialogs.show_paint_color_dialog
                && ctx.input(|i| i.pointer.primary_clicked())
            {
                if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                    let canvas_rect = egui::Rect::from_min_size(
                        egui::pos2(ox, oy),
                        egui::vec2(
                            data.doc.canvas_w as f32 * zoom,
                            data.doc.canvas_h as f32 * zoom,
                        ),
                    )
                    .intersect(canvas_viewport);
                    let text_preview_hit = preview_image.as_ref().is_some_and(|(_, w, h, _, _)| {
                        text_preview_contains(
                            pos,
                            text_origin,
                            *w,
                            *h,
                            text_scale,
                            data.tool.text_rotation_deg,
                            data.tool.text_flip_x,
                            data.tool.text_flip_y,
                        )
                    });
                    // Clicks on floating egui UI (toolbox, popups) over the
                    // canvas are not canvas clicks.
                    let over_floating_ui = ctx.layer_id_at(pos).is_some_and(|l| {
                        l.order != egui::Order::Background
                            && l.id != egui::Id::new("text_overlay")
                            && l.id != egui::Id::new("text_overlay_drag_handle")
                    });
                    if canvas_rect.contains(pos)
                        && !over_floating_ui
                        && !resp.rect.contains(pos)
                        && !text_preview_hit
                        && !handle_resp.rect.contains(pos)
                    {
                        let cx = ((pos.x - ox) / zoom).clamp(0.0, data.doc.canvas_w as f32);
                        let cy = ((pos.y - oy) / zoom).clamp(0.0, data.doc.canvas_h as f32);
                        actions.tool.text_canvas_click = Some((cx, cy));
                    }
                }
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                actions.tool.text_commit = true;
            }
        }

        let pixels_per_point = ctx.pixels_per_point().max(1.0);
        let pixel_screen_size = zoom * pixels_per_point;
        if pixel_screen_size >= 8.0 {
            let screen_rect = ctx.content_rect();
            let painter = ctx
                .layer_painter(egui::LayerId::new(
                    egui::Order::Background,
                    egui::Id::new("pixel_grid"),
                ))
                .with_clip_rect(canvas_viewport);
            let grid_col = egui::Color32::from_rgba_unmultiplied(100, 100, 100, 80);
            let grid_stroke = egui::Stroke::new(1.0 / pixels_per_point, grid_col);
            let snap_line = |v: f32| ((v * pixels_per_point).round() + 0.5) / pixels_per_point;

            let cs_x0 = ox;
            let cs_y0 = oy;
            let cs_x1 = ox + data.doc.canvas_w as f32 * zoom;
            let cs_y1 = oy + data.doc.canvas_h as f32 * zoom;

            let vis_x0 = cs_x0.max(screen_rect.min.x);
            let vis_y0 = cs_y0.max(screen_rect.min.y);
            let vis_x1 = cs_x1.min(screen_rect.max.x);
            let vis_y1 = cs_y1.min(screen_rect.max.y);

            if vis_x1 > vis_x0 && vis_y1 > vis_y0 {
                let col_start = ((vis_x0 - cs_x0) / zoom).floor() as i32;
                let col_end = ((vis_x1 - cs_x0) / zoom).ceil() as i32;
                for col in col_start..=col_end {
                    let sx = snap_line(cs_x0 + col as f32 * zoom);
                    if sx < vis_x0 - 0.5 || sx > vis_x1 + 0.5 {
                        continue;
                    }
                    painter.line_segment(
                        [egui::pos2(sx, vis_y0), egui::pos2(sx, vis_y1)],
                        grid_stroke,
                    );
                }
                let row_start = ((vis_y0 - cs_y0) / zoom).floor() as i32;
                let row_end = ((vis_y1 - cs_y0) / zoom).ceil() as i32;
                for row in row_start..=row_end {
                    let sy = snap_line(cs_y0 + row as f32 * zoom);
                    if sy < vis_y0 - 0.5 || sy > vis_y1 + 0.5 {
                        continue;
                    }
                    painter.line_segment(
                        [egui::pos2(vis_x0, sy), egui::pos2(vis_x1, sy)],
                        grid_stroke,
                    );
                }
            }
        }
    });

    if let Some(state) = egui_state {
        state.handle_platform_output(window, full_output.platform_output);
    }

    if actions.chrome.window_minimize {
        window.set_minimized(true);
    }
    if actions.chrome.window_maximize_toggle {
        window.set_maximized(!window.is_maximized());
    }
    if actions.chrome.window_drag {
        let _ = window.drag_window();
    }

    let primitives = egui_ctx.tessellate(full_output.shapes, full_output.pixels_per_point);

    let repaint_delay = full_output
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|v| v.repaint_delay)
        .unwrap_or(std::time::Duration::MAX);

    (
        primitives,
        full_output.textures_delta,
        actions,
        repaint_delay,
    )
}

fn paint_contrast_polyline(painter: &egui::Painter, pts: &[egui::Pos2], phase: f32) {
    if pts.len() < 2 {
        return;
    }

    painter.add(egui::Shape::line(
        pts.to_vec(),
        egui::Stroke::new(2.2_f32, egui::Color32::from_black_alpha(210)),
    ));

    let dash = 6.0_f32;
    let mut distance = 0.0_f32;
    for segment in pts.windows(2) {
        let a = segment[0];
        let b = segment[1];
        let delta = b - a;
        let len = delta.length();
        if len <= 0.001 {
            continue;
        }
        let dir = delta / len;
        let mut at = 0.0_f32;
        while at < len {
            let global = distance + at + phase;
            let band = (global / dash).floor();
            let mut next = (((band + 1.0) * dash) - phase - distance).min(len);
            if next <= at {
                next = (at + 0.5).min(len);
            }
            let color = if band as i32 % 2 == 0 {
                egui::Color32::WHITE
            } else {
                egui::Color32::BLACK
            };
            painter.line_segment(
                [a + dir * at, a + dir * next],
                egui::Stroke::new(1.15_f32, color),
            );
            at = next;
        }
        distance += len;
    }
}

pub const RULER_SIZE: f32 = 20.0;

/// Draw a small soft/hard round brush-tip preview swatch inside `rect`.
/// Renders concentric discs (outer → inner) so the falloff mirrors the painter.
fn draw_brush_preview(painter: &egui::Painter, rect: egui::Rect, hardness: f32, _color: [u8; 4]) {
    painter.rect_filled(rect, 3.0, egui::Color32::from_gray(28));
    let center = rect.center();
    let max_r = rect.width().min(rect.height()) * 0.42;
    let [r, g, b] = [255u8, 255, 255];
    let steps = 18;
    for k in 0..=steps {
        let radius = max_r * (1.0 - k as f32 / steps as f32);
        let dist = radius.max(0.0);
        let a = crate::tools::brush::soft_round_alpha(dist * dist, max_r, hardness);
        let alpha = (a * 130.0).round().clamp(0.0, 255.0) as u8;
        painter.circle_filled(
            center,
            radius,
            egui::Color32::from_rgba_unmultiplied(r, g, b, alpha),
        );
    }
}

/// Rulers use TopBottomPanel + SidePanel — placed right after menu/topoptions,
/// NOT covering any panel.
fn paint_clone_source_thumbnail(
    ctx: &egui::Context,
    data: &UiData,
    cursor_pos: egui::Pos2,
    preview: &CloneSourcePreview,
) {
    if preview.width == 0
        || preview.height == 0
        || preview.pixels.len() != preview.width * preview.height * 4
    {
        return;
    }

    let side = (data.tool.clone_size * 2.0 * data.doc.zoom).max(1.0);
    let rect = egui::Rect::from_center_size(cursor_pos, egui::vec2(side, side));
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("clone_source_thumbnail"),
    ));
    let image =
        egui::ColorImage::from_rgba_unmultiplied([preview.width, preview.height], &preview.pixels);
    let texture = ctx.load_texture(
        "clone_source_thumbnail",
        image,
        egui::TextureOptions::LINEAR,
    );
    painter.image(
        texture.id(),
        rect,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
}

#[allow(deprecated)]
fn draw_rulers(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let pal = data.chrome.theme_mode.palette();
    let ruler_frame = egui::Frame::new()
        .fill(pal.ruler_bg)
        .inner_margin(egui::Margin::ZERO);

    egui::TopBottomPanel::top("ruler_h")
        .exact_size(RULER_SIZE)
        .frame(ruler_frame)
        .show(ctx, |ui| {
            let rect = ui.max_rect();
            let painter = ui.painter_at(rect);

            painter.line_segment(
                [rect.left_bottom(), rect.right_bottom()],
                egui::Stroke::new(1.0_f32, pal.border_subtle),
            );

            paint_h_ticks(&painter, rect, data);

            let resp = ui.interact(rect, ui.id().with("h_ruler"), egui::Sense::click_and_drag());
            // Drag from the top ruler → a horizontal guide positioned by canvas Y.
            if resp.dragged() {
                if let Some(p) = resp.interact_pointer_pos() {
                    let cy = (p.y - data.doc.offset_y) / data.doc.zoom;
                    actions.chrome.ruler_guide_drag =
                        Some((crate::core::document::GuideOrientation::Horizontal, cy));
                }
            }
            if resp.drag_stopped() {
                actions.chrome.ruler_guide_commit = true;
            }
            resp.context_menu(|ui| {
                for unit in Unit::all() {
                    let is_selected = data.doc.canvas_unit == unit;
                    if ui.radio(is_selected, unit.name()).clicked() {
                        actions.doc.set_canvas_unit = Some(unit);
                        ui.close();
                    }
                }
            });
        });

    egui::SidePanel::left("ruler_v")
        .exact_size(RULER_SIZE)
        .resizable(false)
        .frame(ruler_frame)
        .show(ctx, |ui| {
            let rect = ui.max_rect();
            let painter = ui.painter_at(rect);

            painter.line_segment(
                [rect.right_top(), rect.right_bottom()],
                egui::Stroke::new(1.0_f32, pal.border_subtle),
            );

            paint_v_ticks(&painter, rect, data);

            let resp = ui.interact(rect, ui.id().with("v_ruler"), egui::Sense::click_and_drag());
            // Drag from the left ruler → a vertical guide positioned by canvas X.
            if resp.dragged() {
                if let Some(p) = resp.interact_pointer_pos() {
                    let cx = (p.x - data.doc.offset_x) / data.doc.zoom;
                    actions.chrome.ruler_guide_drag =
                        Some((crate::core::document::GuideOrientation::Vertical, cx));
                }
            }
            if resp.drag_stopped() {
                actions.chrome.ruler_guide_commit = true;
            }
            resp.context_menu(|ui| {
                for unit in Unit::all() {
                    let is_selected = data.doc.canvas_unit == unit;
                    if ui.radio(is_selected, unit.name()).clicked() {
                        actions.doc.set_canvas_unit = Some(unit);
                        ui.close();
                    }
                }
            });
        });
}

fn tick_interval(zoom: f32, unit: crate::core::units::Unit, dpi: f32) -> f32 {
    let min_gap_px = 48.0_f32;
    let min_gap_unit = crate::core::units::from_pixels(min_gap_px / zoom, unit, dpi, 1.0);

    const STEPS: &[f32] = &[
        0.01, 0.05, 0.1, 0.2, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 25.0, 50.0, 100.0, 200.0,
        250.0, 500.0, 1000.0, 5000.0, 10000.0,
    ];
    let unit_interval = STEPS
        .iter()
        .find(|&&s| s >= min_gap_unit)
        .copied()
        .unwrap_or(50000.0);

    crate::core::units::to_pixels(unit_interval, unit, dpi, 1.0)
}

fn paint_h_ticks(painter: &egui::Painter, rect: egui::Rect, data: &UiData) {
    let pal = data.chrome.theme_mode.palette();
    let font = egui::FontId::monospace(9.0);
    let zoom = data.doc.zoom;
    let off_x = data.doc.offset_x;
    let interval = tick_interval(zoom, data.doc.canvas_unit, data.doc.canvas_dpi);

    let canvas_x0 = ((rect.min.x - off_x) / zoom).floor();
    let canvas_x1 = ((rect.max.x - off_x) / zoom).ceil();
    let first = (canvas_x0 / interval).floor() * interval;

    let mut cx = first;
    while cx <= canvas_x1 {
        let sx = off_x + cx * zoom;
        if sx >= rect.min.x && sx <= rect.max.x {
            painter.line_segment(
                [egui::pos2(sx, rect.max.y - 7.0), egui::pos2(sx, rect.max.y)],
                egui::Stroke::new(1.0_f32, pal.text_disabled),
            );

            let val = crate::core::units::from_pixels(
                cx,
                data.doc.canvas_unit,
                data.doc.canvas_dpi,
                data.doc.canvas_w as f32,
            );
            let label = crate::core::units::format_value(val, data.doc.canvas_unit);

            painter.text(
                egui::pos2(sx + 2.0, rect.min.y + 2.0),
                egui::Align2::LEFT_TOP,
                label,
                font.clone(),
                pal.text_secondary,
            );

            if cx.abs() < 1e-3 {
                painter.line_segment(
                    [egui::pos2(sx, rect.min.y), egui::pos2(sx, rect.max.y)],
                    egui::Stroke::new(1.5_f32, pal.accent_primary),
                );
            }

            let sub = interval / 4.0;
            for i in 1..4i32 {
                let mx = off_x + (cx + sub * i as f32) * zoom;
                if mx >= rect.min.x && mx <= rect.max.x {
                    painter.line_segment(
                        [egui::pos2(mx, rect.max.y - 3.0), egui::pos2(mx, rect.max.y)],
                        egui::Stroke::new(0.5_f32, pal.text_disabled),
                    );
                }
            }
        }
        cx += interval;
    }
}

fn paint_v_ticks(painter: &egui::Painter, rect: egui::Rect, data: &UiData) {
    let pal = data.chrome.theme_mode.palette();
    let font = egui::FontId::monospace(9.0);
    let zoom = data.doc.zoom;
    let off_y = data.doc.offset_y;
    let interval = tick_interval(zoom, data.doc.canvas_unit, data.doc.canvas_dpi);

    let canvas_y0 = ((rect.min.y - off_y) / zoom).floor();
    let canvas_y1 = ((rect.max.y - off_y) / zoom).ceil();
    let first = (canvas_y0 / interval).floor() * interval;

    let mut cy = first;
    while cy <= canvas_y1 {
        let sy = off_y + cy * zoom;
        if sy >= rect.min.y && sy <= rect.max.y {
            painter.line_segment(
                [egui::pos2(rect.max.x - 7.0, sy), egui::pos2(rect.max.x, sy)],
                egui::Stroke::new(1.0_f32, pal.text_disabled),
            );

            let val = crate::core::units::from_pixels(
                cy,
                data.doc.canvas_unit,
                data.doc.canvas_dpi,
                data.doc.canvas_h as f32,
            );
            let label = crate::core::units::format_value(val, data.doc.canvas_unit);

            painter.text(
                egui::pos2(rect.min.x + 2.0, sy + 2.0),
                egui::Align2::LEFT_TOP,
                label,
                font.clone(),
                pal.text_secondary,
            );

            if cy.abs() < 1e-3 {
                painter.line_segment(
                    [egui::pos2(rect.min.x, sy), egui::pos2(rect.max.x, sy)],
                    egui::Stroke::new(1.5_f32, pal.accent_primary),
                );
            }

            let sub = interval / 4.0;
            for i in 1..4i32 {
                let my = off_y + (cy + sub * i as f32) * zoom;
                if my >= rect.min.y && my <= rect.max.y {
                    painter.line_segment(
                        [egui::pos2(rect.max.x - 3.0, my), egui::pos2(rect.max.x, my)],
                        egui::Stroke::new(0.5_f32, pal.text_disabled),
                    );
                }
            }
        }
        cy += interval;
    }
}

fn draw_paint_color_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    if !data.dialogs.show_paint_color_dialog {
        return;
    }

    let esc_pressed = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    let enter_pressed = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
    if esc_pressed {
        actions.dialogs.paint_color_dialog_cancel = true;
    }
    if enter_pressed {
        actions.dialogs.paint_color_dialog_ok = true;
    }

    let title = match data.dialogs.paint_color_dialog_target {
        1 => "Background Color",
        2 => "Text Color",
        3 => "Shape Fill Color",
        4 => "Shape Stroke Color",
        _ => "Foreground Color",
    };

    let side_pos = document_side_dialog_pos(ctx, data, 340.0, 96.0);
    let mut open = data.dialogs.show_paint_color_dialog;
    let mut window = egui::Window::new(title)
        .id(egui::Id::new("paint_color_dialog"))
        .collapsible(false)
        .resizable(false)
        .movable(true)
        .order(egui::Order::Foreground)
        .default_pos(side_pos)
        .default_width(340.0)
        .open(&mut open);

    if data.dialogs.paint_color_dialog_center_next {
        window = window.current_pos(side_pos);
        actions.dialogs.paint_color_dialog_centered = true;
    }

    let dialog_resp = window.show(ctx, |ui| {
        ui.spacing_mut().slider_width = 270.0;

        let mut color = egui::Color32::from_rgba_unmultiplied(
            data.dialogs.paint_color_dialog_color[0],
            data.dialogs.paint_color_dialog_color[1],
            data.dialogs.paint_color_dialog_color[2],
            255,
        );
        if color_picker::color_picker_compact(ui, &mut color) {
            actions.dialogs.set_paint_color_dialog_color =
                Some([color.r(), color.g(), color.b(), 255]);
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            paint_color_preview(ui, "Current", data.dialogs.paint_color_dialog_original);
            ui.add_space(8.0);
            paint_color_preview(ui, "New", data.dialogs.paint_color_dialog_color);
        });

        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("Swatches")
                .small()
                .color(egui::Color32::GRAY),
        );
        ui.horizontal_wrapped(|ui| {
            let sw = [
                [0u8, 0, 0, 255],
                [255, 255, 255, 255],
                [192, 0, 0, 255],
                [255, 0, 0, 255],
                [255, 102, 0, 255],
                [255, 192, 0, 255],
                [255, 255, 0, 255],
                [146, 208, 80, 255],
                [0, 176, 80, 255],
                [0, 176, 240, 255],
                [0, 70, 127, 255],
                [112, 48, 160, 255],
            ];
            for swatch in sw {
                if ui
                    .add(
                        egui::Button::new("")
                            .fill(egui::Color32::from_rgb(swatch[0], swatch[1], swatch[2]))
                            .min_size(egui::vec2(22.0, 22.0)),
                    )
                    .clicked()
                {
                    actions.dialogs.set_paint_color_dialog_color = Some(swatch);
                }
            }
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let mut live = data.dialogs.paint_color_dialog_live_preview;
            if ui.checkbox(&mut live, "Preview").changed() {
                actions.dialogs.set_paint_color_dialog_live_preview = Some(live);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let [r, g, b, _] = data.dialogs.paint_color_dialog_color;
                ui.label(
                    egui::RichText::new(format!("#{r:02X}{g:02X}{b:02X}"))
                        .monospace()
                        .small()
                        .color(egui::Color32::GRAY),
                );
                if let Some(ink) = data.dialogs.paint_dialog_ink {
                    let pct = |v: u8| (v as f32 / 255.0 * 100.0).round() as u32;
                    ui.label(
                        egui::RichText::new(format!(
                            "C{} M{} Y{} K{} %",
                            pct(ink[0]),
                            pct(ink[1]),
                            pct(ink[2]),
                            pct(ink[3])
                        ))
                        .monospace()
                        .small()
                        .color(egui::Color32::GRAY),
                    );
                }
            });
        });

        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Default").clicked() {
                actions.dialogs.paint_color_dialog_default = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(egui::Button::new("Cancel").min_size(egui::vec2(72.0, 24.0)))
                    .clicked()
                {
                    actions.dialogs.paint_color_dialog_cancel = true;
                }
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new("OK").color(egui::Color32::WHITE))
                            .fill(egui::Color32::from_gray(82))
                            .min_size(egui::vec2(72.0, 24.0)),
                    )
                    .clicked()
                {
                    actions.dialogs.paint_color_dialog_ok = true;
                }
            });
        });
    });

    if let Some(ir) = &dialog_resp {
        actions.dialogs.paint_dialog_hovered = ctx
            .pointer_hover_pos()
            .is_some_and(|p| ir.response.rect.contains(p));
    }

    if !open {
        actions.dialogs.paint_color_dialog_cancel = true;
    }
}

fn paint_color_preview(ui: &mut egui::Ui, label: &str, color: [u8; 4]) {
    ui.vertical(|ui| {
        ui.label(
            egui::RichText::new(label)
                .small()
                .color(egui::Color32::GRAY),
        );
        let (rect, _) = ui.allocate_exact_size(egui::vec2(86.0, 30.0), egui::Sense::hover());
        ui.painter().rect_filled(
            rect,
            2.0,
            egui::Color32::from_rgb(color[0], color[1], color[2]),
        );
        ui.painter().rect_stroke(
            rect,
            2.0,
            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(90)),
            egui::StrokeKind::Outside,
        );
    });
}

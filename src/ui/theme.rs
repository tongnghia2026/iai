// App theme: a single source of truth for both egui's built-in Visuals AND the
// app's custom-painted chrome (toolbar, tab bar, tool options, panels, status
// bar, canvas backdrop). Every custom color must come from `Palette` so a new
// theme is one match arm here, not a hunt through ten files.

use egui::{Color32, Context, Shadow, Visuals};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// Dark-only. The `ThemeMode` enum is kept as the single source that feeds `Palette`
// (so chrome colours live in one place), but the Light theme was removed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum ThemeMode {
    #[default]
    Dark,
}

/// Resolved colours for the active theme. Built from `ThemeMode` — cheap, copy.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub app_bg: Color32,
    pub top_bar_bg: Color32,
    pub toolbar_bg: Color32,
    pub panel_bg: Color32,
    pub panel_header_bg: Color32,
    pub workspace_bg: Color32,
    pub ruler_bg: Color32,
    pub border_subtle: Color32,
    pub hover_bg: Color32,
    pub pressed_bg: Color32,
    pub text_primary: Color32,
    pub text_secondary: Color32,
    pub text_disabled: Color32,
    pub icon: Color32,
    pub accent_primary: Color32,
    pub accent_hover: Color32,
    pub accent_selected_bg: Color32,
    pub accent_guide: Color32,
    pub warning: Color32,
    pub danger: Color32,
    pub success: Color32,
    pub window_bg: Color32,
    /// Text-input / "extreme" background.
    pub input_bg: Color32,
    /// Inactive button / row fill.
    pub button_bg: Color32,
    /// Hovered button / row fill.
    pub button_hover: Color32,
    /// Pressed (active) button fill — a neutral press, NOT the accent.
    pub separator: Color32,
    /// Primary text / icon colour.
    pub text: Color32,
    /// Secondary (dimmed) text.
    pub text_dim: Color32,
}

impl Palette {
    /// Backdrop behind the canvas (the wgpu clear colour), normalized 0..1 RGBA.
    pub fn canvas_backdrop(self) -> [f64; 4] {
        fn srgb_to_linear(c: u8) -> f64 {
            let v = c as f64 / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        }

        [
            srgb_to_linear(self.workspace_bg.r()),
            srgb_to_linear(self.workspace_bg.g()),
            srgb_to_linear(self.workspace_bg.b()),
            1.0,
        ]
    }
}

impl ThemeMode {
    pub fn palette(self) -> Palette {
        // Deep, slightly cool charcoal with one blue accent, as in modern pro
        // photo editors: the chrome steps from darkest (canvas backdrop) to
        // lightest (panel headers), and blue marks only what is chosen.
        let app_bg = Color32::from_rgb(35, 36, 40);
        let top_bar_bg = Color32::from_rgb(30, 31, 35);
        let toolbar_bg = Color32::from_rgb(38, 39, 43);
        let panel_bg = Color32::from_rgb(43, 44, 49);
        let panel_header_bg = Color32::from_rgb(52, 54, 60);
        let workspace_bg = Color32::from_rgb(26, 27, 30);
        let ruler_bg = Color32::from_rgb(38, 39, 43);
        let border_subtle = Color32::from_rgb(60, 62, 69);
        let hover_bg = Color32::from_rgb(59, 61, 68);
        let pressed_bg = Color32::from_rgb(68, 71, 79);
        let text_primary = Color32::from_rgb(228, 229, 233);
        let text_secondary = Color32::from_rgb(160, 163, 171);
        let text_disabled = Color32::from_rgb(106, 109, 117);
        let icon = Color32::from_rgb(214, 216, 222);
        let accent_primary = Color32::from_rgb(74, 144, 240);
        let accent_hover = Color32::from_rgb(110, 168, 246);
        let accent_selected_bg = Color32::from_rgb(40, 76, 128);
        let accent_guide = Color32::from_rgb(74, 160, 255);
        let warning = Color32::from_rgb(226, 178, 92);
        let danger = Color32::from_rgb(222, 98, 104);
        let success = Color32::from_rgb(96, 196, 128);

        Palette {
            app_bg,
            top_bar_bg,
            toolbar_bg,
            panel_bg,
            panel_header_bg,
            workspace_bg,
            ruler_bg,
            border_subtle,
            hover_bg,
            pressed_bg,
            text_primary,
            text_secondary,
            text_disabled,
            icon,
            accent_primary,
            accent_hover,
            accent_selected_bg,
            accent_guide,
            warning,
            danger,
            success,
            window_bg: panel_bg,
            input_bg: Color32::from_rgb(28, 29, 33),
            button_bg: panel_header_bg,
            button_hover: hover_bg,
            separator: border_subtle,
            text: text_primary,
            text_dim: text_secondary,
        }
    }
}

/// Apply the theme to egui's global Visuals. Call only when the mode changes —
/// the custom chrome reads `palette()` directly each frame.
pub fn apply_theme(ctx: &Context, _mode: ThemeMode) {
    let pal = ThemeMode::Dark.palette();
    let mut style = (*ctx.global_style()).clone();

    let mut visuals = Visuals::dark();
    visuals.panel_fill = pal.app_bg;
    visuals.window_fill = pal.window_bg;
    visuals.window_stroke = egui::Stroke::new(1.0_f32, pal.border_subtle);
    visuals.window_corner_radius = egui::CornerRadius::same(8);
    visuals.menu_corner_radius = egui::CornerRadius::same(6);
    visuals.extreme_bg_color = pal.input_bg;
    visuals.faint_bg_color = pal.panel_header_bg;
    visuals.code_bg_color = pal.input_bg;
    visuals.warn_fg_color = pal.warning;
    visuals.error_fg_color = pal.danger;
    visuals.selection.bg_fill = pal.accent_selected_bg;
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, pal.accent_primary);
    visuals.override_text_color = Some(pal.text_primary);
    visuals.hyperlink_color = pal.accent_hover;
    // A slider shows its value as a blue run from the left end.
    visuals.slider_trailing_fill = true;

    // Flat design: a BUTTON shows no box until hovered/active. egui buttons fill
    // with `weak_bg_fill`, so making only that transparent flattens buttons WITHOUT
    // hiding `bg_fill`-drawn chrome like the slider rail / trough (the earlier bug
    // that made sliders invisible). The fill + a brighter outline appear on hover.
    visuals.widgets.noninteractive.weak_bg_fill = Color32::TRANSPARENT;
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, pal.text_secondary);
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, pal.border_subtle);
    visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    visuals.widgets.inactive.bg_fill = pal.button_bg;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, pal.border_subtle);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, pal.text_secondary);
    visuals.widgets.hovered.bg_fill = pal.hover_bg;
    visuals.widgets.hovered.weak_bg_fill = pal.hover_bg;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, pal.pressed_bg);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0_f32, pal.text_primary);
    visuals.widgets.active.bg_fill = pal.pressed_bg;
    visuals.widgets.active.weak_bg_fill = pal.pressed_bg;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, pal.accent_primary);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0_f32, pal.text_primary);
    visuals.widgets.open.bg_fill = pal.accent_selected_bg;
    visuals.widgets.open.weak_bg_fill = pal.accent_selected_bg;
    visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0_f32, pal.text_primary);
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0_f32, pal.accent_primary);

    // Softer corners on every button, field and row.
    for w in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        w.corner_radius = egui::CornerRadius::same(5);
    }

    // Sliders: flatter, rectangular handle to echo the Develop look.
    visuals.handle_shape = egui::style::HandleShape::Rect { aspect_ratio: 0.6 };

    // A soft shadow lifts menus and dialogs off the dark chrome behind them.
    visuals.window_shadow = Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(110),
    };
    visuals.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 12,
        spread: 0,
        color: Color32::from_black_alpha(90),
    };

    style.spacing.item_spacing = egui::vec2(6.0, 5.0);
    style.spacing.button_padding = egui::vec2(8.0, 4.0);
    style.spacing.menu_margin = egui::Margin::same(6);
    style.spacing.interact_size = egui::vec2(24.0, 22.0);
    style.spacing.slider_width = 126.0;
    style.spacing.indent = 14.0;

    style.visuals = visuals;
    ctx.set_global_style(style);
}

// ── Persistence ─────────────────────────────────────────────────────────────
// Remembered across sessions in %APPDATA%/IAI/prefs.json (same base dir as the
// AI settings / model cache).

pub(crate) fn prefs_path() -> PathBuf {
    crate::core::prefs_store::prefs_path()
}

/// Load the saved theme (defaults to Dark on first run / read error).
pub fn load_theme_mode() -> ThemeMode {
    crate::core::prefs_store::get("theme_mode").unwrap_or_default()
}

/// Persist the chosen theme. Best-effort — a write failure is non-fatal.
pub fn save_theme_mode(mode: ThemeMode) {
    crate::core::prefs_store::set("theme_mode", &mode);
}

/// The family of the bold UI face (group and section titles).
pub const BOLD_FAMILY: &str = "ui_bold";

/// Register the bold UI face as [`BOLD_FAMILY`], backed by the regular
/// family's fonts (icons, other scripts). On a system without a bold file
/// the family is the regular one.
pub fn add_bold_font(fonts: &mut egui::FontDefinitions) {
    let mut faces = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    for path in [
        "C:/Windows/Fonts/segoeuib.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
    ] {
        if let Ok(data) = std::fs::read(path) {
            fonts.font_data.insert(
                BOLD_FAMILY.to_owned(),
                std::sync::Arc::new(egui::FontData::from_owned(data)),
            );
            faces.insert(0, BOLD_FAMILY.to_owned());
            break;
        }
    }
    fonts
        .families
        .insert(egui::FontFamily::Name(BOLD_FAMILY.into()), faces);
}

/// The bold UI font at `size`; the regular one while the bold family is not
/// installed (before the fonts load, and in a bare test context).
pub fn bold_font(ctx: &Context, size: f32) -> egui::FontId {
    let family = egui::FontFamily::Name(BOLD_FAMILY.into());
    let bound = ctx.fonts(|f| f.definitions().families.contains_key(&family));
    let family = if bound {
        family
    } else {
        egui::FontFamily::Proportional
    };
    egui::FontId::new(size, family)
}

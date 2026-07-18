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
        let app_bg = Color32::from_rgb(57, 57, 57);
        let top_bar_bg = Color32::from_rgb(64, 64, 64);
        let toolbar_bg = Color32::from_rgb(61, 61, 61);
        let panel_bg = Color32::from_rgb(69, 69, 69);
        let panel_header_bg = Color32::from_rgb(75, 75, 75);
        let workspace_bg = Color32::from_rgb(40, 40, 40);
        let ruler_bg = Color32::from_rgb(61, 61, 61);
        let border_subtle = Color32::from_rgb(88, 88, 88);
        let hover_bg = Color32::from_rgb(82, 82, 82);
        let pressed_bg = Color32::from_rgb(90, 90, 90);
        let text_primary = Color32::from_rgb(232, 232, 232);
        let text_secondary = Color32::from_rgb(174, 174, 174);
        let text_disabled = Color32::from_rgb(126, 126, 126);
        let icon = text_primary;
        let accent_primary = Color32::from_rgb(206, 206, 206);
        let accent_hover = Color32::from_rgb(226, 226, 226);
        let accent_selected_bg = Color32::from_rgb(98, 98, 98);
        let accent_guide = Color32::from_rgb(206, 206, 206);
        let warning = Color32::from_rgb(218, 170, 92);
        let danger = Color32::from_rgb(210, 101, 106);
        let success = Color32::from_rgb(104, 190, 130);

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
            input_bg: app_bg,
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
    visuals.window_corner_radius = egui::CornerRadius::same(6);
    visuals.menu_corner_radius = egui::CornerRadius::same(4);
    visuals.extreme_bg_color = pal.input_bg;
    visuals.faint_bg_color = pal.panel_header_bg;
    visuals.code_bg_color = pal.input_bg;
    visuals.warn_fg_color = pal.warning;
    visuals.error_fg_color = pal.danger;
    visuals.selection.bg_fill = pal.accent_selected_bg;
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, pal.accent_primary);
    visuals.override_text_color = Some(pal.text_primary);
    visuals.hyperlink_color = pal.accent_hover;

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
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, pal.border_subtle);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0_f32, pal.text_primary);
    visuals.widgets.active.bg_fill = pal.pressed_bg;
    visuals.widgets.active.weak_bg_fill = pal.pressed_bg;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, pal.accent_primary);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0_f32, pal.text_primary);
    visuals.widgets.open.bg_fill = pal.accent_selected_bg;
    visuals.widgets.open.weak_bg_fill = pal.accent_selected_bg;
    visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0_f32, pal.text_primary);

    // Sliders: flatter, rectangular handle to echo the Develop look.
    visuals.handle_shape = egui::style::HandleShape::Rect { aspect_ratio: 0.6 };

    // Flat design — no drop shadows.
    visuals.window_shadow = Shadow::NONE;
    visuals.popup_shadow = Shadow::NONE;

    style.spacing.item_spacing = egui::vec2(6.0, 5.0);
    style.spacing.button_padding = egui::vec2(8.0, 3.0);
    style.spacing.interact_size = egui::vec2(24.0, 22.0);
    style.spacing.slider_width = 126.0;
    style.spacing.indent = 14.0;

    style.visuals = visuals;
    ctx.set_global_style(style);
}

// ── Persistence ─────────────────────────────────────────────────────────────
// Remembered across sessions in %APPDATA%/IAI/prefs.json (same base dir as the
// AI settings / model cache).

#[derive(Serialize, Deserialize, Default)]
struct UiPrefs {
    theme_mode: ThemeMode,
}

pub(crate) fn prefs_path() -> PathBuf {
    let base = if let Ok(appdata) = std::env::var("APPDATA") {
        PathBuf::from(appdata).join("IAI")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local").join("share").join("iai")
    } else {
        PathBuf::from(".")
    };
    base.join("prefs.json")
}

/// Load the saved theme (defaults to Dark on first run / read error).
pub fn load_theme_mode() -> ThemeMode {
    std::fs::read_to_string(prefs_path())
        .ok()
        .and_then(|s| serde_json::from_str::<UiPrefs>(&s).ok())
        .map(|p| p.theme_mode)
        .unwrap_or_default()
}

/// Persist the chosen theme. Best-effort — a write failure is non-fatal.
pub fn save_theme_mode(mode: ThemeMode) {
    let path = prefs_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
    if !value.is_object() {
        value = serde_json::Value::Object(Default::default());
    }
    if let Some(map) = value.as_object_mut() {
        if let Ok(v) = serde_json::to_value(mode) {
            map.insert("theme_mode".to_string(), v);
        }
    }
    if let Ok(json) = serde_json::to_string_pretty(&value) {
        let _ = std::fs::write(&path, json);
    }
}

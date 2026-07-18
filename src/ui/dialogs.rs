mod adjustment;
mod document;
mod filter;
mod print;
mod select_ops;
mod session;
pub(crate) use adjustment::*;
pub(crate) use document::*;
pub(crate) use filter::*;
pub(crate) use print::*;
pub(crate) use select_ops::*;
pub(crate) use session::*;

pub(crate) use super::{
    AdjEyedropperKind, AdjustmentOptions, AutoLevelsAlgorithm, UiActions, UiData,
};
pub(crate) use crate::core::filters::FilterType;
pub(crate) use crate::core::layer::{AdjustmentType, LevelsParams};
pub(crate) use crate::formats::ExportFormat;
pub(crate) use crate::ui::widgets::dev_slider;
use egui;
pub(crate) use egui_phosphor::regular as ph;
use serde::{Deserialize, Serialize};
pub(crate) use std::path::PathBuf;

pub(crate) const MODAL_BACKDROP_ORDER: egui::Order = egui::Order::Middle;
pub(crate) const DIALOG_ORDER: egui::Order = egui::Order::Foreground;

#[derive(Serialize, Deserialize, Default)]
struct AdjustmentPrefs {
    #[serde(default)]
    auto_levels_algorithm: AutoLevelsAlgorithm,
    #[serde(default = "default_auto_clip_percent")]
    auto_clip_percent: f32,
}

fn default_auto_clip_percent() -> f32 {
    0.10
}

fn prefs_path() -> PathBuf {
    let base = if let Ok(appdata) = std::env::var("APPDATA") {
        PathBuf::from(appdata).join("IAI")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local").join("share").join("iai")
    } else {
        PathBuf::from(".")
    };
    base.join("prefs.json")
}

/// Remembered `.icc` path of the last CMYK conversion (prefs.json key
/// `last_cmyk_icc_path`; merge-written so other keys survive).
pub(crate) fn load_last_cmyk_icc_path() -> Option<PathBuf> {
    let s = std::fs::read_to_string(prefs_path()).ok()?;
    let v = serde_json::from_str::<serde_json::Value>(&s).ok()?;
    v["last_cmyk_icc_path"].as_str().map(PathBuf::from)
}

pub(crate) fn save_last_cmyk_icc_path(path: &std::path::Path) {
    let file = prefs_path();
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut value = std::fs::read_to_string(&file)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
    if !value.is_object() {
        value = serde_json::Value::Object(Default::default());
    }
    if let Some(map) = value.as_object_mut() {
        map.insert(
            "last_cmyk_icc_path".to_string(),
            serde_json::json!(path.to_string_lossy()),
        );
    }
    if let Ok(json) = serde_json::to_string_pretty(&value) {
        let _ = std::fs::write(file, json);
    }
}

pub(crate) fn sanitize_adjustment_options(mut options: AdjustmentOptions) -> AdjustmentOptions {
    options.auto_clip_percent = options.auto_clip_percent.clamp(0.0, 10.0);
    options
}

pub(crate) fn load_adjustment_options() -> AdjustmentOptions {
    std::fs::read_to_string(prefs_path())
        .ok()
        .and_then(|s| serde_json::from_str::<AdjustmentPrefs>(&s).ok())
        .map(|p| {
            sanitize_adjustment_options(AdjustmentOptions {
                auto_levels_algorithm: p.auto_levels_algorithm,
                auto_clip_percent: p.auto_clip_percent,
            })
        })
        .unwrap_or_default()
}

pub(crate) fn save_adjustment_options(options: AdjustmentOptions) {
    let path = prefs_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let options = sanitize_adjustment_options(options);
    let mut value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
    if !value.is_object() {
        value = serde_json::Value::Object(Default::default());
    }
    if let Some(map) = value.as_object_mut() {
        if let Ok(v) = serde_json::to_value(options.auto_levels_algorithm) {
            map.insert("auto_levels_algorithm".to_string(), v);
        }
        map.insert(
            "auto_clip_percent".to_string(),
            serde_json::json!(options.auto_clip_percent),
        );
    }
    if let Ok(json) = serde_json::to_string_pretty(&value) {
        let _ = std::fs::write(path, json);
    }
}

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

#[allow(deprecated)]
pub fn build(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    if data.dialogs.show_new_dialog {
        new_canvas_dialog(ctx, data, actions);
    }
    if data.dialogs.show_resize_dialog {
        resize_dialog(ctx, data, actions);
    }
    if data.dialogs.show_image_size_dialog {
        image_size_dialog(ctx, data, actions);
    }
    if data.dialogs.show_rename_dialog {
        rename_dialog(ctx, data, actions);
    }
    if data.dialogs.show_export_dialog {
        export_dialog(ctx, data, actions);
    }
    if data.print.show_print_dialog {
        print_dialog(ctx, data, actions);
    }
    if data.dialogs.show_preferences {
        preferences_dialog(ctx, data, actions);
    }
    if data.dialogs.show_adjustment_dialog {
        adjustment_dialog(ctx, data, actions);
    }
    if data.dialogs.show_filter_dialog {
        filter_dialog(ctx, data, actions);
    }
    if data.dialogs.show_feather_dialog {
        feather_dialog(ctx, data, actions);
    }
    if data.dialogs.show_modify_dialog.is_some() {
        modify_dialog(ctx, data, actions);
    }
    if data.dialogs.show_stroke_dialog {
        stroke_dialog(ctx, data, actions);
    }
    if data.dialogs.show_smart_fill_dialog {
        smart_fill_dialog(ctx, data, actions);
    }
    if data.tool.show_gradient_editor {
        gradient_editor_window(ctx, data, actions);
    }
    if data.dialogs.show_exit_dialog {
        exit_dialog(ctx, data, actions);
    }
    if data.dialogs.show_close_dialog {
        close_dialog(ctx, data, actions);
    }
    if data.dialogs.show_reload_file_dialog {
        reload_file_dialog(ctx, data, actions);
    }
    if data.dialogs.show_pdf_import_dialog {
        pdf_import_dialog(ctx, data, actions);
    }
    if data.dialogs.show_cmyk_convert_dialog {
        cmyk_convert_dialog(ctx, data, actions);
    }
}

pub(crate) fn consume_dialog_enter_escape(ctx: &egui::Context) -> (bool, bool) {
    let esc_pressed = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    let enter_pressed = if ctx.egui_wants_keyboard_input() {
        false
    } else {
        ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
    };
    (enter_pressed, esc_pressed)
}

pub(crate) fn draw_checkerboard_in_rect(ui: &egui::Ui, rect: egui::Rect, cell: f32) {
    let painter = ui.painter();
    let cols = (rect.width() / cell).ceil() as i32;
    let rows = (rect.height() / cell).ceil() as i32;
    for row in 0..rows {
        for col in 0..cols {
            let x = rect.left() + col as f32 * cell;
            let y = rect.top() + row as f32 * cell;
            let r = egui::Rect::from_min_max(
                egui::pos2(x, y),
                egui::pos2((x + cell).min(rect.right()), (y + cell).min(rect.bottom())),
            );
            let color = if (row + col) & 1 == 0 {
                egui::Color32::from_gray(122)
            } else {
                egui::Color32::from_gray(156)
            };
            painter.rect_filled(r, 0.0, color);
        }
    }
}

pub(crate) fn modal_overlay(ctx: &egui::Context, id: &str) {
    let screen = ctx.content_rect();
    let overlay_layer = egui::LayerId::new(MODAL_BACKDROP_ORDER, egui::Id::new(id).with("bg"));
    ctx.layer_painter(overlay_layer)
        .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(120));

    egui::Area::new(egui::Id::new(id).with("input"))
        .fixed_pos(screen.min)
        .order(MODAL_BACKDROP_ORDER)
        .show(ctx, |ui| {
            ui.allocate_rect(screen, egui::Sense::click_and_drag());
        });
}

#[cfg(test)]
mod tests {
    use super::{
        auto_levels_channels_from_histogram, auto_levels_from_histogram_with_clip,
        builtin_curves_presets, curve_from_levels_params, new_canvas_pixels, parse_page_ranges,
    };
    use crate::core::layer::LevelsParams;
    use crate::core::units::Unit;
    use crate::ui::{AdjustmentOptions, AutoLevelsAlgorithm};

    #[test]
    fn new_canvas_physical_input_stays_source_of_truth_across_dpi_changes() {
        let width_cm = 10.0;
        let height_cm = 15.0;

        assert_eq!(new_canvas_pixels(width_cm, Unit::Centimeters, 72.0), 283);
        assert_eq!(new_canvas_pixels(height_cm, Unit::Centimeters, 72.0), 425);
        assert_eq!(new_canvas_pixels(width_cm, Unit::Centimeters, 300.0), 1181);
        assert_eq!(new_canvas_pixels(height_cm, Unit::Centimeters, 300.0), 1772);
    }

    #[test]
    fn parses_ranges_and_singletons() {
        assert_eq!(
            parse_page_ranges("1-3,5", 8),
            Some(vec![true, true, true, false, true, false, false, false])
        );
    }

    #[test]
    fn clamps_and_ignores_out_of_range_and_garbage() {
        // 0 and 99 are out of range; "abc" is garbage; 2 is valid.
        assert_eq!(
            parse_page_ranges("0, 2, 99, abc", 3),
            Some(vec![false, true, false])
        );
    }

    #[test]
    fn reversed_range_is_normalized() {
        assert_eq!(parse_page_ranges("3-1", 3), Some(vec![true, true, true]));
    }

    #[test]
    fn blank_or_all_invalid_returns_none() {
        assert_eq!(parse_page_ranges("", 4), None);
        assert_eq!(parse_page_ranges("  ,, ", 4), None);
        assert_eq!(parse_page_ranges("99-200", 4), None);
    }

    #[test]
    fn auto_levels_clip_percent_finds_black_and_white_points() {
        let mut hist = [0_u32; 256];
        hist[0] = 10;
        hist[12] = 100;
        hist[240] = 100;
        hist[255] = 10;

        assert_eq!(
            auto_levels_from_histogram_with_clip(&hist, 0.0),
            Some((0, 255))
        );
        assert_eq!(
            auto_levels_from_histogram_with_clip(&hist, 5.0),
            Some((12, 240))
        );
    }

    #[test]
    fn auto_levels_options_choose_luma_or_rgb_planes() {
        let mut hist = [[0_u32; 256]; 4];
        hist[0][10] = 1;
        hist[0][200] = 1;
        hist[1][30] = 1;
        hist[1][180] = 1;
        hist[2][40] = 1;
        hist[2][160] = 1;
        hist[3][20] = 1;
        hist[3][220] = 1;

        let mono = auto_levels_channels_from_histogram(
            &hist,
            AdjustmentOptions {
                auto_levels_algorithm: AutoLevelsAlgorithm::Monochromatic,
                auto_clip_percent: 0.0,
            },
        )
        .unwrap();
        assert_eq!((mono[0].in_black, mono[0].in_white), (20, 220));
        assert!(mono[1].is_identity() && mono[2].is_identity() && mono[3].is_identity());

        let rgb = auto_levels_channels_from_histogram(
            &hist,
            AdjustmentOptions {
                auto_levels_algorithm: AutoLevelsAlgorithm::PerChannelContrast,
                auto_clip_percent: 0.0,
            },
        )
        .unwrap();
        assert!(rgb[0].is_identity());
        assert_eq!((rgb[1].in_black, rgb[1].in_white), (10, 200));
        assert_eq!((rgb[2].in_black, rgb[2].in_white), (30, 180));
        assert_eq!((rgb[3].in_black, rgb[3].in_white), (40, 160));
    }

    #[test]
    fn curve_from_levels_params_maps_black_and_white_points() {
        let curve = curve_from_levels_params(&LevelsParams {
            in_black: 32,
            in_white: 224,
            gamma: 1.0,
            out_black: 0,
            out_white: 255,
        });
        assert_eq!(curve[0], (0.0, 0.0));
        assert_eq!(curve[1], (32.0 / 255.0, 0.0));
        assert_eq!(curve[2], (224.0 / 255.0, 1.0));
        assert_eq!(curve[3], (1.0, 1.0));
    }

    #[test]
    fn built_in_negative_curves_preset_uses_master_channel() {
        let presets = builtin_curves_presets();
        let negative = presets
            .iter()
            .find(|(name, _)| *name == "Negative")
            .map(|(_, channels)| channels)
            .unwrap();
        assert_eq!(negative[0], vec![(0.0, 1.0), (1.0, 0.0)]);
        assert!(negative[1].iter().all(|(x, y)| (x - y).abs() < 1e-6));
    }
}

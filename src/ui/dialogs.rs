mod adjustment;
mod agent;
mod ai_errand;
mod author_mail;
mod document;
mod filter;
#[cfg(feature = "pro")]
#[rustfmt::skip]
mod id_photo;
#[cfg(feature = "pro")]
#[rustfmt::skip]
mod portrait;
mod print;
#[cfg(feature = "pro")]
#[rustfmt::skip]
mod print_sheet;
#[cfg(feature = "pro")]
#[rustfmt::skip]
mod pro;
mod quick_tasks;
mod scan_cleanup;
mod select_ops;
mod session;
mod sponsors;
mod support;
mod text_font;
mod update;
mod vector_style;
pub(crate) use adjustment::*;
pub(crate) use agent::*;
pub(crate) use ai_errand::*;
pub(crate) use author_mail::*;
pub(crate) use document::*;
pub(crate) use filter::*;
#[cfg(feature = "pro")]
pub(crate) use portrait::*;
pub(crate) use print::*;
#[cfg(feature = "pro")]
pub(crate) use print_sheet::*;
#[cfg(feature = "pro")]
pub(crate) use pro::*;
pub(crate) use quick_tasks::*;
pub(crate) use scan_cleanup::*;
pub(crate) use select_ops::*;
pub(crate) use session::*;
pub(crate) use sponsors::*;
pub(crate) use support::*;
pub(crate) use text_font::*;
pub(crate) use update::*;
pub(crate) use vector_style::*;

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

/// One remembered value of prefs.json.
pub(crate) fn load_pref<T: serde::de::DeserializeOwned>(key: &str) -> Option<T> {
    crate::core::prefs_store::get(key)
}

/// Remember one value in prefs.json, keeping the others.
pub(crate) fn save_pref<T: Serialize>(key: &str, value: &T) {
    crate::core::prefs_store::set(key, value);
}

/// Remembered `.icc` path of the last CMYK conversion (prefs.json key
/// `last_cmyk_icc_path`).
pub(crate) fn load_last_cmyk_icc_path() -> Option<PathBuf> {
    load_pref::<String>("last_cmyk_icc_path").map(PathBuf::from)
}

pub(crate) fn save_last_cmyk_icc_path(path: &std::path::Path) {
    save_pref("last_cmyk_icc_path", &path.to_string_lossy());
}

pub(crate) fn sanitize_adjustment_options(mut options: AdjustmentOptions) -> AdjustmentOptions {
    options.auto_clip_percent = options.auto_clip_percent.clamp(0.0, 10.0);
    options
}

/// What `image.auto_levels` is run with: the Levels dialog's Auto options.
pub(crate) fn auto_levels_args(options: AdjustmentOptions) -> serde_json::Value {
    let options = sanitize_adjustment_options(options);
    serde_json::json!({
        "monochromatic": options.auto_levels_algorithm == AutoLevelsAlgorithm::Monochromatic,
        "clipPercent": options.auto_clip_percent,
    })
}

pub(crate) fn load_adjustment_options() -> AdjustmentOptions {
    let prefs = serde_json::Value::Object(crate::core::prefs_store::read());
    serde_json::from_value::<AdjustmentPrefs>(prefs)
        .ok()
        .map(|p| {
            sanitize_adjustment_options(AdjustmentOptions {
                auto_levels_algorithm: p.auto_levels_algorithm,
                auto_clip_percent: p.auto_clip_percent,
            })
        })
        .unwrap_or_default()
}

pub(crate) fn save_adjustment_options(options: AdjustmentOptions) {
    let options = sanitize_adjustment_options(options);
    let Ok(algorithm) = serde_json::to_value(options.auto_levels_algorithm) else {
        return;
    };
    crate::core::prefs_store::update(|prefs| {
        prefs.insert("auto_levels_algorithm".to_string(), algorithm);
        prefs.insert(
            "auto_clip_percent".to_string(),
            serde_json::json!(options.auto_clip_percent),
        );
    });
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
    if data.dialogs.show_font_change_dialog {
        text_format_dialog(ctx, data, actions);
    }
    if data.dialogs.pdf_batch_operation.is_some() {
        pdf_batch_scope_dialog(ctx, data, actions);
    }
    if data.dialogs.show_vector_style_dialog {
        vector_style_dialog(ctx, data, actions);
    }
    if data.dialogs.show_scan_cleanup_dialog {
        scan_cleanup_dialog(ctx, data, actions);
    }
    #[cfg(feature = "pro")]
    if data.dialogs.show_portrait_dialog {
        // Its window shows one tab at a time.
        if data.ai.tab == Some(crate::ui::AiTab::Online) {
            portrait_dialog_behind(ctx, data, actions);
        } else {
            portrait_dialog(ctx, data, actions);
        }
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
    if data.dialogs.page_rename.is_some() {
        page_rename_dialog(ctx, data, actions);
    }
    if data.dialogs.show_pdf_export_dialog {
        pdf_export_dialog(ctx, data, actions);
    }
    if data.dialogs.mail_merge.is_some() {
        mail_merge_dialog(ctx, data, actions);
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
    if data.dialogs.show_color_range_dialog {
        color_range_dialog(ctx, data, actions);
    }
    if data.dialogs.show_smart_fill_dialog {
        smart_fill_dialog(ctx, data, actions);
    }
    if data.tool.show_gradient_editor {
        gradient_editor_window(ctx, data, actions);
    }
    // A failed "Save & Close" brings its prompt back; the reason comes first, on
    // its own, so the two never fight over Enter and Escape.
    let save_failed = data.dialogs.save_error.is_some();
    if data.dialogs.show_exit_dialog && !save_failed {
        exit_dialog(ctx, data, actions);
    }
    if data.dialogs.show_close_dialog && !save_failed {
        close_dialog(ctx, data, actions);
    }
    if data.dialogs.document_editor_error.is_some() {
        document_editor_error_dialog(ctx, data, actions);
    }
    if save_failed {
        save_error_dialog(ctx, data, actions);
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
    // The save prompts of a closing app come before the news of a newer one,
    // and before the ask to support its author.
    let closing = data.dialogs.show_exit_dialog || data.dialogs.show_close_dialog || save_failed;
    if data.chrome.errand.open && !closing {
        errand_window(ctx, data, actions);
    }
    if data.chrome.quick.open && !closing {
        quick_window(ctx, data, actions);
    }
    if data.chrome.update.show_dialog && !closing {
        update_dialog(ctx, data, actions);
    } else if !closing {
        use crate::ui::SupportDialog;
        match data.chrome.support {
            Some(SupportDialog::Sponsors) => sponsors_dialog(ctx, data, actions),
            Some(SupportDialog::Mail(kind)) => author_mail_dialog(ctx, data, actions, kind),
            Some(SupportDialog::Support | SupportDialog::Updated) => {
                support_dialog(ctx, data, actions)
            }
            #[cfg(feature = "pro")]
            Some(SupportDialog::Pro) => pro_dialog(ctx, data, actions),
            #[cfg(feature = "pro")]
            Some(SupportDialog::ProIssue) => pro_issue_dialog(ctx, data, actions),
            #[cfg(not(feature = "pro"))]
            Some(SupportDialog::Pro | SupportDialog::ProIssue) => {}
            Some(SupportDialog::Agent) => agent_dialog(ctx, data, actions),
            None => {}
        }
    }
}

/// Open the "Delete preset" window of preset list `list` (see
/// [`preset_deleter`]).
pub(crate) fn open_preset_deleter(ctx: &egui::Context, list: &str) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(("preset_deleter_open", list)), true));
}

/// The "Delete preset" window of a list of presets (`names`), while open:
/// one is picked, then deleted by the button, so nothing is lost to a click
/// beside a name. Returns the one to delete.
pub(crate) fn preset_deleter(
    ctx: &egui::Context,
    list: &str,
    title: &str,
    names: &[&str],
) -> Option<usize> {
    let open_id = egui::Id::new(("preset_deleter_open", list));
    let picked_id = egui::Id::new(("preset_deleter_picked", list));
    if !ctx.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false) {
        return None;
    }
    let mut picked = ctx
        .data(|d| d.get_temp::<usize>(picked_id))
        .filter(|&i| i < names.len());
    let mut open = true;
    let mut deleted = None;
    egui::Window::new(title)
        .id(egui::Id::new(("preset_deleter", list)))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(egui::Order::Tooltip)
        .show(ctx, |ui| {
            ui.set_min_width(260.0);
            if names.is_empty() {
                ui.label(egui::RichText::new("No saved presets.").color(egui::Color32::GRAY));
            } else {
                ui.label("Chọn preset cần xóa:");
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(ui.ctx().content_rect().height() * 0.6)
                    .show(ui, |ui| {
                        for (i, name) in names.iter().enumerate() {
                            let row = egui::Button::selectable(picked == Some(i), *name)
                                .min_size(egui::vec2(ui.available_width(), 0.0));
                            if ui.add(row).clicked() {
                                picked = Some(i);
                            }
                        }
                    });
            }
            ui.separator();
            ui.horizontal(|ui| {
                let delete = egui::Button::new(
                    egui::RichText::new(format!("{}  Xóa", ph::TRASH))
                        .color(egui::Color32::from_rgb(220, 80, 80)),
                );
                if ui.add_enabled(picked.is_some(), delete).clicked() {
                    deleted = picked.take();
                }
                if ui.button("Close").clicked() {
                    open = false;
                }
            });
        });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        open = false;
    }
    ctx.data_mut(|d| {
        d.insert_temp(open_id, open);
        match picked.filter(|_| open) {
            Some(i) => {
                d.insert_temp(picked_id, i);
            }
            None => {
                d.remove::<usize>(picked_id);
            }
        }
    });
    deleted
}

/// What a dialog's Cancel button was pressed as (see [`cancel_or_reset`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CancelPress {
    None,
    Cancel,
    Reset,
}

/// A dialog's Cancel button, Photoshop's way: while Alt is held it reads
/// `reset` and puts back the values the dialog opened with (see
/// [`opening_values`]), the dialog staying open. `add` draws the button with
/// the text it is given.
pub(crate) fn cancel_or_reset(
    ui: &mut egui::Ui,
    cancel: &str,
    reset: &str,
    add: impl FnOnce(&mut egui::Ui, &str) -> egui::Response,
) -> CancelPress {
    let alt = ui.input(|i| i.modifiers.alt);
    let response = add(ui, if alt { reset } else { cancel });
    match (response.clicked(), alt) {
        (false, _) => CancelPress::None,
        (true, false) => CancelPress::Cancel,
        (true, true) => CancelPress::Reset,
    }
}

/// The values a dialog opened with: `now` on the first pass it is drawn,
/// kept while it is drawn pass after pass, and taken again once it was
/// closed, or when `same` says `now` belongs to another dialog than the one
/// kept (Curves opened over Levels).
pub(crate) fn opening_values<T: Clone + Send + Sync + 'static>(
    ctx: &egui::Context,
    id: egui::Id,
    now: &T,
    same: impl Fn(&T, &T) -> bool,
) -> T {
    let pass = ctx.cumulative_pass_nr();
    let key = id.with("opening_values");
    ctx.data_mut(|d| {
        let kept = d
            .get_temp::<(u64, T)>(key)
            .filter(|(last, kept)| last.saturating_add(1) >= pass && same(kept, now))
            .map(|(_, kept)| kept);
        let value = kept.unwrap_or_else(|| now.clone());
        d.insert_temp(key, (pass, value.clone()));
        value
    })
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

/// Test-only: a dialog drawn without a window, to click and type in.
#[cfg(test)]
pub(crate) mod stage {
    use super::{UiActions, UiData};

    /// A dialog on a screen of its own, frame by frame.
    pub(crate) struct Stage {
        pub(crate) ctx: egui::Context,
        pub(crate) data: UiData,
        screen: egui::Vec2,
        draw: Box<dyn Fn(&egui::Context, &UiData, &mut UiActions)>,
    }

    impl Stage {
        pub(crate) fn new(
            data: UiData,
            draw: impl Fn(&egui::Context, &UiData, &mut UiActions) + 'static,
        ) -> Self {
            Self::on(data, egui::vec2(900.0, 700.0), draw)
        }

        pub(crate) fn on(
            data: UiData,
            screen: egui::Vec2,
            draw: impl Fn(&egui::Context, &UiData, &mut UiActions) + 'static,
        ) -> Self {
            let stage = Self {
                ctx: egui::Context::default(),
                data,
                screen,
                draw: Box::new(draw),
            };
            // A window finds its size over its first frames.
            for _ in 0..3 {
                stage.frame(vec![]);
            }
            stage
        }

        pub(crate) fn frame(&self, events: Vec<egui::Event>) -> (egui::FullOutput, UiActions) {
            let mut actions = UiActions::default();
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.screen)),
                events,
                ..Default::default()
            };
            let output = self
                .ctx
                .run_ui(input, |ui| (self.draw)(ui.ctx(), &self.data, &mut actions));
            (output, actions)
        }

        /// Every piece of text drawn, and where.
        pub(crate) fn texts(&self) -> Vec<(String, egui::Pos2)> {
            self.frame(vec![])
                .0
                .shapes
                .iter()
                .filter_map(|clipped| match &clipped.shape {
                    egui::Shape::Text(text) => Some((text.galley.text().to_owned(), text.pos)),
                    _ => None,
                })
                .collect()
        }

        pub(crate) fn says(&self, what: &str) -> bool {
            self.texts().iter().any(|(text, _)| text.contains(what))
        }

        /// A click on the text ending in `label`: what its frame asked for
        /// and the links it opened.
        pub(crate) fn click(&self, label: &str) -> (UiActions, Vec<String>) {
            let at = self
                .texts()
                .iter()
                .find(|(text, _)| text.trim().ends_with(label))
                .map(|(_, pos)| *pos + egui::vec2(6.0, 5.0))
                .unwrap_or_else(|| panic!("no {label}"));
            let press = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            self.frame(vec![egui::Event::PointerMoved(at)]);
            self.frame(vec![press(true)]);
            let (output, actions) = self.frame(vec![press(false)]);
            let opened = output
                .platform_output
                .commands
                .iter()
                .filter_map(|command| match command {
                    egui::OutputCommand::OpenUrl(open) => Some(open.url.clone()),
                    _ => None,
                })
                .collect();
            (actions, opened)
        }

        pub(crate) fn key(&self, key: egui::Key) -> UiActions {
            let press = |pressed| egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            };
            let (_, actions) = self.frame(vec![press(true)]);
            self.frame(vec![press(false)]);
            actions
        }

        /// `text` typed into whatever has the keyboard.
        pub(crate) fn type_text(&self, text: &str) {
            self.frame(vec![egui::Event::Text(text.to_string())]);
        }
    }
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

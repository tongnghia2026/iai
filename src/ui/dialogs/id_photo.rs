//! The "Ảnh thẻ" side of the Auto retouch dialog: one click turns a portrait into an
//! ID photo (2×3, 3×4 or 4×6) framed from the eyes and chin, levelled, with
//! the person on its own layer over white or blue; the retouch sliders below
//! then work on it. Once it is made, changing the size, the framing or the
//! nudges frames it again at once. The options are remembered in prefs.json
//! (key `id_photo`), and the preset the sliders start from (key
//! `id_photo_preset`).

use super::*;
use crate::core::id_photo::{IdPhotoOptions, IdPhotoRequest, Nudge, DEFAULT_WIDEN, MAX_WIDEN};
use crate::core::imposition::{Backdrop, PhotoKind};
use crate::core::portrait::presets::Preset;
use crate::core::portrait::PortraitSettings;
use egui_phosphor::regular as ph;

const PREFS_KEY: &str = "id_photo";
const PRESET_KEY: &str = "id_photo_preset";
const OPTIONS_ID: &str = "id_photo_options";
const NUDGE_ID: &str = "id_photo_nudge";
/// One click of an arrow, as a fraction of the picture's height, and of a
/// turn, in degrees.
const NUDGE_STEP: f32 = 0.02;
const TURN_STEP: f32 = 0.5;

/// The sliders the Ảnh thẻ side opens with: the preset used last, if it is
/// still kept.
pub(super) fn starting_settings(presets: &[Preset]) -> PortraitSettings {
    let name: String = load_pref(PRESET_KEY).unwrap_or_default();
    presets
        .iter()
        .find(|p| p.name == name)
        .map_or_else(PortraitSettings::default, |p| p.settings)
}

/// A preset's name on its chip: "Ảnh thẻ nữ" is "Nữ" here.
fn chip_name(name: &str) -> String {
    let short = name.strip_prefix("Ảnh thẻ ").unwrap_or(name);
    let mut letters = short.chars();
    match letters.next() {
        Some(first) => first.to_uppercase().chain(letters).collect(),
        None => String::new(),
    }
}

/// A choice among a few: a framed button, filled while it is the one chosen.
fn chip(ui: &mut egui::Ui, chosen: bool, text: &str) -> bool {
    ui.add(egui::Button::new(egui::RichText::new(text).size(12.0)).selected(chosen))
        .clicked()
}

/// `chip` with a square of `colour` before its text.
fn colour_chip(ui: &mut egui::Ui, chosen: bool, text: &str, colour: [u8; 3]) -> bool {
    let response = ui.add(
        egui::Button::new(egui::RichText::new(format!("     {text}")).size(12.0)).selected(chosen),
    );
    let square = egui::Rect::from_center_size(
        egui::pos2(response.rect.left() + 12.0, response.rect.center().y),
        egui::vec2(11.0, 11.0),
    );
    ui.painter().rect(
        square,
        2.0,
        egui::Color32::from_rgb(colour[0], colour[1], colour[2]),
        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(110)),
        egui::StrokeKind::Inside,
    );
    response.clicked()
}

fn row_label(ui: &mut egui::Ui, text: &str) {
    ui.add_sized(
        [36.0, 20.0],
        egui::Label::new(
            egui::RichText::new(text)
                .size(11.5)
                .color(egui::Color32::from_gray(170)),
        ),
    );
}

/// The Ảnh thẻ side's own controls, above the retouch groups. `settings` are
/// the retouch sliders: a "Mẫu" chip loads a preset into them.
pub(super) fn id_photo_section(
    ui: &mut egui::Ui,
    data: &UiData,
    actions: &mut UiActions,
    settings: &mut PortraitSettings,
    presets: &[Preset],
) {
    let ctx = ui.ctx().clone();
    let options_id = egui::Id::new(OPTIONS_ID);
    let nudge_id = egui::Id::new(NUDGE_ID);
    let saved: IdPhotoOptions = ctx
        .data_mut(|d| d.get_temp(options_id))
        .unwrap_or_else(|| load_pref(PREFS_KEY).unwrap_or_default());
    let mut options = saved;
    let busy = data.dialogs.id_photo_busy;
    let made = data.dialogs.id_photo_made;
    // Nudges correct the photo that is there: a photo yet to be made starts
    // from the app's own framing.
    let mut nudge: Nudge = ctx
        .data_mut(|d| d.get_temp(nudge_id))
        .filter(|_| made)
        .unwrap_or_default();
    // A change that is over: a chip clicked, a slider let go, a nudge.
    let mut settled = false;

    ui.horizontal_wrapped(|ui| {
        row_label(ui, "Nền");
        for backdrop in [Backdrop::White, Backdrop::Blue] {
            let chosen = options.cut_out && options.backdrop == backdrop;
            if colour_chip(ui, chosen, backdrop.label(), backdrop.rgb()) {
                (options.cut_out, options.backdrop) = (true, backdrop);
                settled = true;
            }
        }
        if chip(ui, !options.cut_out, "Giữ nền gốc") {
            options.cut_out = false;
            settled = true;
        }
    });
    ui.horizontal_wrapped(|ui| {
        row_label(ui, "Mẫu");
        for preset in presets {
            if chip(ui, preset.settings == *settings, &chip_name(&preset.name)) {
                *settings = preset.settings;
                save_pref(PRESET_KEY, &preset.name);
            }
        }
    })
    .response
    .on_hover_text(
        "Bộ thanh kéo chỉnh chân dung dùng cho ảnh này. Sửa, lưu, xóa mẫu ở ô Chân dung",
    );
    ui.horizontal_wrapped(|ui| {
        row_label(ui, "Cỡ");
        for size in PhotoKind::ALL {
            if chip(ui, options.crop && options.size == size, size.label()) {
                (options.crop, options.size) = (true, size);
                settled = true;
            }
        }
        if chip(ui, !options.crop, "Không cắt") {
            options.crop = false;
            settled = true;
        }
    });
    ui.add_enabled_ui(options.crop, |ui| {
        if ui
            .checkbox(&mut options.straighten, "Xoay thẳng theo đường mắt")
            .changed()
        {
            settled = true;
        }
        // The app's own slider (drag, or click the box and type), in whole
        // percent.
        let mut percent = (options.widen * 100.0).round();
        let track = [70u8, 130, 200].map(egui::Color32::from_gray);
        let slider = crate::ui::widgets::dev_slider_stacked_resp(
            ui,
            "Khung rộng hơn mẫu (%)",
            &mut percent,
            0.0..=MAX_WIDEN * 100.0,
            &track,
            1.0,
        );
        options.widen = percent.round() / 100.0;
        settled |= slider.drag_stopped() || (slider.changed() && !slider.dragged());
    });

    let can_run = options.crop || options.cut_out;
    let run = if made {
        ui.add_enabled_ui(options.crop, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                row_label(ui, "Khung");
                let before = (nudge, options.widen);
                for (icon, tip, right, down, turn) in [
                    (ph::ARROW_LEFT, "Người sang trái", -1.0, 0.0, 0.0),
                    (ph::ARROW_RIGHT, "Người sang phải", 1.0, 0.0, 0.0),
                    (ph::ARROW_UP, "Người lên trên", 0.0, -1.0, 0.0),
                    (ph::ARROW_DOWN, "Người xuống dưới", 0.0, 1.0, 0.0),
                    (ph::ARROW_COUNTER_CLOCKWISE, "Xoay trái", 0.0, 0.0, -1.0),
                    (ph::ARROW_CLOCKWISE, "Xoay phải", 0.0, 0.0, 1.0),
                ] {
                    let arrow = egui::Button::new(icon).min_size(egui::vec2(26.0, 22.0));
                    if ui.add(arrow).on_hover_text(tip).clicked() {
                        nudge.right += right * NUDGE_STEP;
                        nudge.down += down * NUDGE_STEP;
                        nudge.turn += turn * TURN_STEP;
                    }
                }
                if ui
                    .add_enabled(
                        nudge != Nudge::default() || options.widen != DEFAULT_WIDEN,
                        egui::Button::new("Đặt lại"),
                    )
                    .on_hover_text("Về khung app tự chọn, rộng hơn mẫu 10%")
                    .clicked()
                {
                    nudge = Nudge::default();
                    options.widen = DEFAULT_WIDEN;
                }
                settled |= (nudge, options.widen) != before;
            });
        });
        // The photo is there: what changes frames it again.
        settled && can_run
    } else {
        let button = egui::Button::new(
            egui::RichText::new(format!("{}  Làm ảnh thẻ", ph::IDENTIFICATION_CARD)).strong(),
        );
        ui.add_enabled_ui(can_run && !busy && data.doc.has_doc, |ui| {
            ui.add_sized([ui.available_width(), 28.0], button)
                .on_hover_text(
                    "Cắt theo mắt và cằm, tách người lên nền đã chọn rồi chỉnh chân dung theo mẫu. Có vùng chọn thì chỉ lấy người trong vùng chọn",
                )
                .clicked()
        })
        .inner
    };

    let status = &data.dialogs.id_photo_status;
    if busy || !status.is_empty() {
        ui.horizontal_wrapped(|ui| {
            if busy {
                ui.spinner();
            }
            let colour = if data.dialogs.id_photo_error {
                egui::Color32::from_rgb(220, 90, 80)
            } else {
                egui::Color32::from_gray(170)
            };
            ui.label(
                egui::RichText::new(status.as_str())
                    .size(11.0)
                    .color(colour),
            );
        });
    }

    ctx.data_mut(|d| {
        d.insert_temp(options_id, options);
        d.insert_temp(nudge_id, nudge);
    });
    if settled || run {
        save_pref(PREFS_KEY, &options);
    }
    if run {
        actions.dialogs.run_id_photo = Some(IdPhotoRequest {
            options,
            nudge,
            settings: Some(*settings),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_presets_chip_drops_the_words_every_one_shares() {
        assert_eq!(chip_name("Ảnh thẻ nữ"), "Nữ");
        assert_eq!(chip_name("Ảnh thẻ nam"), "Nam");
        assert_eq!(chip_name("Trẻ em"), "Trẻ em");
        assert_eq!(chip_name(""), "");
    }

    /// One frame of the section alone, 300 px wide; returns what it asked.
    fn frame(
        ctx: &egui::Context,
        data: &UiData,
        settings: &mut PortraitSettings,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, UiActions) {
        let mut actions = UiActions::default();
        let presets = crate::core::portrait::presets::built_in();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(320.0, 400.0),
            )),
            events,
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            ui.set_max_width(300.0);
            id_photo_section(ui, data, &mut actions, settings, &presets);
        });
        (output, actions)
    }

    fn text_at(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
        output
            .shapes
            .iter()
            .find_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) if text.galley.text().trim() == label => {
                    Some(text.pos + egui::vec2(text.galley.size().x - 4.0, 5.0))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("no {label}"))
    }

    fn click(
        ctx: &egui::Context,
        data: &UiData,
        settings: &mut PortraitSettings,
        at: egui::Pos2,
    ) -> UiActions {
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(ctx, data, settings, vec![egui::Event::PointerMoved(at)]);
        frame(ctx, data, settings, vec![button(true)]);
        frame(ctx, data, settings, vec![button(false)]).1
    }

    #[test]
    fn the_photo_is_asked_for_by_its_button_and_then_by_every_change() {
        let ctx = egui::Context::default();
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(OPTIONS_ID), IdPhotoOptions::default()));
        let mut data = UiData::default();
        data.doc.has_doc = true;
        let mut settings = PortraitSettings::default();
        frame(&ctx, &data, &mut settings, vec![]);
        let (output, actions) = frame(&ctx, &data, &mut settings, vec![]);
        assert!(actions.dialogs.run_id_photo.is_none());

        // Before the photo: chips only choose, a preset loads its sliders.
        let asked = click(&ctx, &data, &mut settings, text_at(&output, "Xanh"));
        assert!(asked.dialogs.run_id_photo.is_none());
        let asked = click(&ctx, &data, &mut settings, text_at(&output, "4×6"));
        assert!(asked.dialogs.run_id_photo.is_none());
        let asked = click(&ctx, &data, &mut settings, text_at(&output, "Nam"));
        assert!(asked.dialogs.run_id_photo.is_none());
        let presets = crate::core::portrait::presets::built_in();
        let male = presets.iter().find(|p| p.name == "Ảnh thẻ nam").unwrap();
        assert_eq!(settings, male.settings);

        // The button asks for all of it.
        let button = text_at(
            &output,
            &format!("{}  Làm ảnh thẻ", ph::IDENTIFICATION_CARD),
        );
        let asked = click(&ctx, &data, &mut settings, button);
        let request = asked.dialogs.run_id_photo.expect("the photo");
        assert_eq!(
            (
                request.options.cut_out,
                request.options.backdrop,
                request.options.size,
                request.nudge,
                request.settings,
            ),
            (
                true,
                Backdrop::Blue,
                PhotoKind::Id4x6,
                Nudge::default(),
                Some(male.settings),
            )
        );

        // Once it is made the button gives way to the nudges, and a chip or
        // a nudge asks at once.
        data.dialogs.id_photo_made = true;
        frame(&ctx, &data, &mut settings, vec![]);
        let (output, actions) = frame(&ctx, &data, &mut settings, vec![]);
        assert!(actions.dialogs.run_id_photo.is_none());
        let asked = click(&ctx, &data, &mut settings, text_at(&output, "2×3"));
        let request = asked.dialogs.run_id_photo.expect("the photo again");
        assert_eq!(request.options.size, PhotoKind::Id2x3);
        let asked = click(&ctx, &data, &mut settings, text_at(&output, ph::ARROW_UP));
        let request = asked.dialogs.run_id_photo.expect("the photo nudged");
        assert_eq!(request.nudge.down, -NUDGE_STEP);
        let asked = click(&ctx, &data, &mut settings, text_at(&output, "Đặt lại"));
        let request = asked
            .dialogs
            .run_id_photo
            .expect("the photo as first framed");
        assert_eq!(request.nudge, Nudge::default());
    }
}

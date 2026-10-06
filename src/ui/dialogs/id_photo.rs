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
const MAKE_LABEL: &str = "Làm ảnh thẻ tự động";
const OPEN_SHEET_LABEL: &str = "Mở file áo";

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

const LABEL_WIDTH: f32 = 38.0;
const CHIP_HEIGHT: f32 = 28.0;
const CHIP_GAP: f32 = 6.0;
const CHIP_PADDING: f32 = 9.0;
const CHIP_TEXT: f32 = 13.5;
const SWATCH: f32 = 13.0;
/// The side of the box that shows the garment held.
const GARMENT_BOX: f32 = 54.0;
/// The blue of what is chosen and of the button that makes the photo.
const BLUE: egui::Color32 = egui::Color32::from_rgb(30, 110, 220);
const BLUE_HOVERED: egui::Color32 = egui::Color32::from_rgb(52, 130, 238);
const BLUE_PRESSED: egui::Color32 = egui::Color32::from_rgb(24, 92, 188);
const BLUE_EDGE: egui::Color32 = egui::Color32::from_rgb(120, 185, 255);

/// A choice among a few: its text, whether it is the one chosen, and the
/// colour of the square before its text, if it has one.
struct Chip<'a> {
    text: &'a str,
    chosen: bool,
    swatch: Option<[u8; 3]>,
}

impl<'a> Chip<'a> {
    fn new(text: &'a str, chosen: bool) -> Self {
        Self {
            text,
            chosen,
            swatch: None,
        }
    }
}

/// Chips of these widths in lines no wider than `width`: as few lines as
/// hold them, and those as even as they go. Each line is a range of chips.
fn chip_lines(widths: &[f32], width: f32) -> Vec<std::ops::Range<usize>> {
    let pack = |limit: f32| {
        let mut lines = Vec::new();
        let (mut start, mut used) = (0, 0.0);
        for (i, w) in widths.iter().enumerate() {
            let longer = used + CHIP_GAP + w;
            if i > start && longer > limit {
                lines.push(start..i);
                (start, used) = (i, *w);
            } else {
                used = if i == start { *w } else { longer };
            }
        }
        if start < widths.len() {
            lines.push(start..widths.len());
        }
        lines
    };
    let count = pack(width).len();
    // The narrowest limit that takes no more lines evens them out.
    let (mut low, mut high) = (0.0, width);
    for _ in 0..16 {
        let middle = (low + high) / 2.0;
        if pack(middle).len() <= count {
            high = middle;
        } else {
            low = middle;
        }
    }
    pack(high)
}

fn row_label(ui: &mut egui::Ui, text: &str) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(LABEL_WIDTH, CHIP_HEIGHT), egui::Sense::hover());
    ui.painter().text(
        rect.left_center() + egui::vec2(2.0, 0.0),
        egui::Align2::LEFT_CENTER,
        text,
        egui::FontId::proportional(12.5),
        egui::Color32::from_gray(185),
    );
}

/// One chip in `rect`: blue while it is the one chosen.
fn paint_chip(ui: &egui::Ui, rect: egui::Rect, response: &egui::Response, chip: &Chip) {
    let visuals = ui.visuals();
    let (fill, edge, colour) = if chip.chosen {
        (BLUE, BLUE_EDGE, egui::Color32::WHITE)
    } else if response.hovered() {
        (
            visuals.widgets.hovered.weak_bg_fill,
            egui::Color32::from_gray(150),
            visuals.strong_text_color(),
        )
    } else {
        (
            visuals.faint_bg_color,
            egui::Color32::from_gray(112),
            visuals.text_color(),
        )
    };
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect(
        rect,
        5.0,
        fill,
        egui::Stroke::new(1.0_f32, edge),
        egui::StrokeKind::Inside,
    );
    let font = if chip.chosen {
        crate::ui::theme::bold_font(ui.ctx(), CHIP_TEXT)
    } else {
        egui::FontId::proportional(CHIP_TEXT)
    };
    let text = painter.layout_no_wrap(chip.text.to_owned(), font, colour);
    let before = chip.swatch.map_or(0.0, |_| SWATCH + 6.0);
    let mut left = (rect.center().x - (before + text.size().x) / 2.0).max(rect.left() + 4.0);
    if let Some(c) = chip.swatch {
        let square = egui::Rect::from_min_size(
            egui::pos2(left, rect.center().y - SWATCH / 2.0),
            egui::vec2(SWATCH, SWATCH),
        );
        painter.rect(
            square,
            2.0,
            egui::Color32::from_rgb(c[0], c[1], c[2]),
            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(235)),
            egui::StrokeKind::Outside,
        );
        left += before;
    }
    let top = rect.center().y - text.size().y / 2.0;
    painter.galley(egui::pos2(left, top), text, colour);
}

/// A row of choices: its name, then its chips stretched to fill the width,
/// on as many lines as they need. Returns the chip clicked and the row.
fn chip_row(ui: &mut egui::Ui, label: &str, chips: &[Chip]) -> (Option<usize>, egui::Response) {
    let mut clicked = None;
    let row = ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(CHIP_GAP, CHIP_GAP);
        row_label(ui, label);
        let width = ui.available_width();
        // Measured in bold: choosing a chip must not move the others.
        let bold = crate::ui::theme::bold_font(ui.ctx(), CHIP_TEXT);
        let natural: Vec<f32> = chips
            .iter()
            .map(|chip| {
                let text = ui.painter().layout_no_wrap(
                    chip.text.to_owned(),
                    bold.clone(),
                    egui::Color32::WHITE,
                );
                let before = chip.swatch.map_or(0.0, |_| SWATCH + 6.0);
                (before + text.size().x + 2.0 * CHIP_PADDING).min(width)
            })
            .collect();
        ui.vertical(|ui| {
            for line in chip_lines(&natural, width) {
                let gaps = CHIP_GAP * (line.len() - 1) as f32;
                let spare = width - gaps - natural[line.clone()].iter().sum::<f32>();
                let each = (spare / line.len() as f32).max(0.0);
                ui.horizontal(|ui| {
                    for i in line {
                        let size = egui::vec2(natural[i] + each, CHIP_HEIGHT);
                        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
                        paint_chip(ui, rect, &response, &chips[i]);
                        if response
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked()
                        {
                            clicked = Some(i);
                        }
                    }
                });
            }
        });
    });
    (clicked, row.response)
}

/// The "Áo" row: the box a garment is dragged onto from the shop's sheet
/// (it shows the one it holds), what can be done with that garment, and
/// under it how another is got: the sheet's file opened, and the layer
/// picked there taken. The whole row is where a garment may be let go.
fn garment_row(
    ui: &mut egui::Ui,
    data: &UiData,
    actions: &mut UiActions,
    settings: &PortraitSettings,
) {
    let held = data.dialogs.garment_thumb;
    let busy = data.dialogs.garment_busy;
    let worn = data.dialogs.garment_worn;
    // Going to a sheet leaves the photo: not while it is being made.
    let may_leave = !busy && !data.dialogs.id_photo_busy;
    let row = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = CHIP_GAP;
        row_label(ui, "Áo");
        let side = egui::vec2(GARMENT_BOX, GARMENT_BOX);
        let (rect, _) = ui.allocate_exact_size(side, egui::Sense::hover());
        let edge = match held {
            Some(_) => BLUE_EDGE,
            None => egui::Color32::from_gray(112),
        };
        ui.painter().rect(
            rect,
            5.0,
            ui.visuals().extreme_bg_color,
            egui::Stroke::new(1.0_f32, edge),
            egui::StrokeKind::Inside,
        );
        match held {
            Some(picture) => {
                let whole = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                ui.painter()
                    .image(picture, rect.shrink(3.0), whole, egui::Color32::WHITE);
            }
            None => {
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    ph::T_SHIRT,
                    egui::FontId::proportional(24.0),
                    egui::Color32::from_gray(130),
                );
            }
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            let line = match held {
                Some(_) => "Áo này được giữ cho các ảnh sau",
                None => "Mở file áo, kéo cái áo khách chọn thả vào đây",
            };
            ui.label(
                egui::RichText::new(line)
                    .size(12.0)
                    .color(egui::Color32::from_gray(185)),
            );
            let gap = egui::vec2(4.0, 4.0);
            if held.is_some() || worn {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = gap;
                    if worn
                        && ui
                            .add_enabled(!busy, egui::Button::new("Chỉnh áo"))
                            .on_hover_text(
                                "Dời, phóng, xoay áo bằng tay như Ctrl+T; Enter để xong. Áo là một layer riêng",
                            )
                            .clicked()
                    {
                        actions.dialogs.adjust_garment = true;
                    }
                    if ui
                        .add_enabled(may_leave, egui::Button::new("Đổi áo khác"))
                        .on_hover_text(
                            "Sang tab file áo để chọn cái khác. Chọn xong bấm Lấy áo đang chọn, app tự quay về ảnh",
                        )
                        .clicked()
                    {
                        actions.dialogs.change_garment = true;
                    }
                    if held.is_some()
                        && ui
                            .add_enabled(!busy, egui::Button::new("Bỏ áo"))
                            .on_hover_text("Lấy áo khỏi ô này và khỏi ảnh đang mở")
                            .clicked()
                    {
                        actions.dialogs.remove_garment = Some(*settings);
                    }
                });
            }
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = gap;
                if ui
                    .add_enabled(
                        may_leave,
                        egui::Button::new(format!("{}  {OPEN_SHEET_LABEL}", ph::FOLDER_OPEN)),
                    )
                    .on_hover_text(
                        "Chọn file áo của tiệm để mở thành một tab. App mở sẵn thư mục của file áo dùng lần trước",
                    )
                    .clicked()
                {
                    actions.dialogs.open_garment_sheet = true;
                }
                if ui
                    .add_enabled(
                        !busy && data.doc.has_doc,
                        egui::Button::new("Lấy áo đang chọn"),
                    )
                    .on_hover_text(
                        "Dùng thay cho kéo thả: bấm vào một cái áo trong file áo rồi bấm nút này. App tự quay về ảnh vừa xem trước đó và mặc áo lên",
                    )
                    .clicked()
                {
                    actions.dialogs.take_garment = Some(*settings);
                }
            });
        });
    });
    let zone = row.response.rect;
    actions.dialogs.garment_box =
        Some(([zone.min.x, zone.min.y, zone.max.x, zone.max.y], *settings));
    let status = &data.dialogs.garment_status;
    if busy || !status.is_empty() {
        ui.horizontal_wrapped(|ui| {
            if busy {
                ui.spinner();
            }
            let colour = if data.dialogs.garment_error {
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
}

/// The button that makes the photo: the one thing to press here, in blue.
fn make_button(ui: &mut egui::Ui) -> egui::Response {
    let size = egui::vec2(ui.available_width(), 38.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let fill = if response.is_pointer_button_down_on() {
        BLUE_PRESSED
    } else if response.hovered() {
        BLUE_HOVERED
    } else {
        BLUE
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        6.0,
        fill,
        egui::Stroke::new(1.0_f32, BLUE_EDGE),
        egui::StrokeKind::Inside,
    );
    let white = egui::Color32::WHITE;
    let icon = painter.layout_no_wrap(
        ph::IDENTIFICATION_CARD.to_string(),
        egui::FontId::proportional(19.0),
        white,
    );
    let name = painter.layout_no_wrap(
        MAKE_LABEL.to_string(),
        crate::ui::theme::bold_font(ui.ctx(), 15.0),
        white,
    );
    let left = rect.center().x - (icon.size().x + 8.0 + name.size().x) / 2.0;
    let after_icon = left + icon.size().x + 8.0;
    let middle = rect.center().y;
    painter.galley(egui::pos2(left, middle - icon.size().y / 2.0), icon, white);
    painter.galley(
        egui::pos2(after_icon, middle - name.size().y / 2.0),
        name,
        white,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
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

    let backdrops = [Backdrop::White, Backdrop::Blue];
    let mut chips: Vec<Chip> = backdrops
        .iter()
        .map(|backdrop| Chip {
            text: backdrop.label(),
            chosen: options.cut_out && options.backdrop == *backdrop,
            swatch: Some(backdrop.rgb()),
        })
        .collect();
    chips.push(Chip::new("Giữ nền gốc", !options.cut_out));
    if let (Some(i), _) = chip_row(ui, "Nền", &chips) {
        match backdrops.get(i) {
            Some(backdrop) => (options.cut_out, options.backdrop) = (true, *backdrop),
            None => options.cut_out = false,
        }
        settled = true;
    }

    let names: Vec<String> = presets.iter().map(|p| chip_name(&p.name)).collect();
    let chips: Vec<Chip> = presets
        .iter()
        .zip(&names)
        .map(|(preset, name)| Chip::new(name, preset.settings == *settings))
        .collect();
    let (clicked, row) = chip_row(ui, "Mẫu", &chips);
    row.on_hover_text(
        "Bộ thanh kéo chỉnh chân dung dùng cho ảnh này. Sửa, lưu, xóa mẫu ở ô Chân dung",
    );
    if let Some(preset) = clicked.and_then(|i| presets.get(i)) {
        *settings = preset.settings;
        save_pref(PRESET_KEY, &preset.name);
    }

    let mut chips: Vec<Chip> = PhotoKind::ALL
        .iter()
        .map(|size| Chip::new(size.label(), options.crop && options.size == *size))
        .collect();
    chips.push(Chip::new("Không cắt", !options.crop));
    if let (Some(i), _) = chip_row(ui, "Cỡ", &chips) {
        match PhotoKind::ALL.get(i) {
            Some(size) => (options.crop, options.size) = (true, *size),
            None => options.crop = false,
        }
        settled = true;
    }
    garment_row(ui, data, actions, settings);
    ui.add_space(2.0);
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
                    let arrow = egui::Button::new(icon).min_size(egui::vec2(28.0, 26.0));
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
        ui.add_space(2.0);
        ui.add_enabled_ui(can_run && !busy && data.doc.has_doc, |ui| {
            make_button(ui)
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
    fn chips_take_as_few_lines_as_hold_them_and_share_them_evenly() {
        // One line while they fit, the gaps between them counted.
        assert_eq!(chip_lines(&[50.0, 50.0, 50.0], 162.0), vec![0..3]);
        assert_eq!(chip_lines(&[50.0, 50.0, 50.0], 161.0), vec![0..2, 2..3]);
        // Five that need two lines go three and two, not four and one.
        let lines = chip_lines(&[44.0, 54.0, 69.0, 76.0, 109.0], 276.0);
        assert_eq!(lines, vec![0..3, 3..5]);
        // One wider than the row has a line of its own.
        assert_eq!(chip_lines(&[300.0, 40.0], 276.0), vec![0..1, 1..2]);
        assert!(chip_lines(&[], 276.0).is_empty());
    }

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
    fn the_garment_row_shows_its_box_and_asks_what_its_buttons_say() {
        let ctx = egui::Context::default();
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(OPTIONS_ID), IdPhotoOptions::default()));
        let mut data = UiData::default();
        data.doc.has_doc = true;
        let mut settings = PortraitSettings::default();
        let written = |output: &egui::FullOutput, label: &str| {
            output.shapes.iter().any(|clipped| {
                matches!(&clipped.shape, egui::Shape::Text(text) if text.galley.text().trim() == label)
            })
        };
        frame(&ctx, &data, &mut settings, vec![]);
        let (output, actions) = frame(&ctx, &data, &mut settings, vec![]);
        // Where a garment may be let go: the whole row, told every frame.
        let (zone, sliders) = actions.dialogs.garment_box.expect("the box");
        assert_eq!(sliders, settings);
        assert!(zone[2] - zone[0] > 200.0 && zone[3] - zone[1] >= GARMENT_BOX);
        // Empty, the box only offers to get a garment: its sheet opened,
        // the layer picked there taken.
        assert!(!written(&output, "Bỏ áo") && !written(&output, "Chỉnh áo"));
        assert!(!written(&output, "Đổi áo khác"));
        let asked = click(
            &ctx,
            &data,
            &mut settings,
            text_at(&output, "Lấy áo đang chọn"),
        );
        assert_eq!(asked.dialogs.take_garment, Some(settings));
        assert!(asked.dialogs.remove_garment.is_none() && !asked.dialogs.adjust_garment);
        assert!(!asked.dialogs.open_garment_sheet && !asked.dialogs.change_garment);
        let open_sheet = format!("{}  {OPEN_SHEET_LABEL}", ph::FOLDER_OPEN);
        let asked = click(&ctx, &data, &mut settings, text_at(&output, &open_sheet));
        assert!(asked.dialogs.open_garment_sheet);
        assert!(asked.dialogs.take_garment.is_none() && !asked.dialogs.change_garment);

        // Holding a garment the photo wears, it can be adjusted, changed
        // for another or let go.
        data.dialogs.garment_thumb = Some(egui::TextureId::default());
        data.dialogs.garment_worn = true;
        frame(&ctx, &data, &mut settings, vec![]);
        let (output, _) = frame(&ctx, &data, &mut settings, vec![]);
        let asked = click(&ctx, &data, &mut settings, text_at(&output, "Chỉnh áo"));
        assert!(asked.dialogs.adjust_garment);
        let asked = click(&ctx, &data, &mut settings, text_at(&output, "Bỏ áo"));
        assert_eq!(asked.dialogs.remove_garment, Some(settings));
        let asked = click(&ctx, &data, &mut settings, text_at(&output, "Đổi áo khác"));
        assert!(asked.dialogs.change_garment && !asked.dialogs.open_garment_sheet);
        // The photo is not left while it is being made.
        data.dialogs.id_photo_busy = true;
        frame(&ctx, &data, &mut settings, vec![]);
        let (output, _) = frame(&ctx, &data, &mut settings, vec![]);
        let asked = click(&ctx, &data, &mut settings, text_at(&output, "Đổi áo khác"));
        assert!(!asked.dialogs.change_garment);
        let asked = click(&ctx, &data, &mut settings, text_at(&output, &open_sheet));
        assert!(!asked.dialogs.open_garment_sheet);
        data.dialogs.id_photo_busy = false;
        // While the garment is being put on, nothing is asked.
        data.dialogs.garment_busy = true;
        frame(&ctx, &data, &mut settings, vec![]);
        let (output, _) = frame(&ctx, &data, &mut settings, vec![]);
        let asked = click(&ctx, &data, &mut settings, text_at(&output, "Bỏ áo"));
        assert!(asked.dialogs.remove_garment.is_none());
        let asked = click(&ctx, &data, &mut settings, text_at(&output, "Đổi áo khác"));
        assert!(!asked.dialogs.change_garment);
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
        let asked = click(&ctx, &data, &mut settings, text_at(&output, MAKE_LABEL));
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

//! "Auto retouch" dialog (Image ▸ Auto retouch…): skin, blemish, under-eye,
//! eye and teeth sliders with a live canvas preview, like the Filter/Levels
//! dialogs, and a brush ("Tô vùng") to fix the skin, hair and brow areas
//! before sliding, and studio colour looks. Áp dụng adds the retouch (look
//! included) as one new layer; Hủy restores. Sets of sliders are kept by
//! name ("Công thức") in prefs.json (key `portrait_presets`).
//!
//! The dialog is a panel, not a modal one: open and idle it leaves every
//! tool and command free. Only while a retouch is previewed (or an ID photo
//! is being made) does it hold the canvas; picking a tool then applies the
//! retouch as it stands and leaves the dialog open.
//!
//! Two tiles at the top choose its side; the one used last is remembered
//! (key `portrait_side`). Opening the dialog analyses nothing. "Chân dung"
//! retouches the photo as it is once "Tự động làm đẹp" is pressed. "Ảnh
//! thẻ" (see `id_photo`) first makes the ID photo, cropped and on a plain
//! backdrop, with the presets as chips, then the same sliders work on that.

use super::*;
use crate::core::portrait::brush::MaskTarget;
use crate::core::portrait::looks::StudioLook;
use crate::core::portrait::presets::{self, Preset};
use crate::core::portrait::PortraitSettings;
use crate::core::selection::RefineBrushMode;
use egui_phosphor::regular as ph;

const PRESETS_KEY: &str = "portrait_presets";
/// Which round of built-in presets prefs.json has been given.
const BUILT_IN_KEY: &str = "portrait_presets_built_in";
const PRESET_NAME_FIELD: &str = "portrait_preset_name";
/// Whether the dialog was last on its Ảnh thẻ side.
const SIDE_KEY: &str = "portrait_side";

/// The kept presets; an install that has not had the built-in ones yet is
/// given them first.
fn load_presets() -> Vec<Preset> {
    let mut kept: Vec<Preset> = load_pref(PRESETS_KEY).unwrap_or_default();
    if load_pref::<u32>(BUILT_IN_KEY).unwrap_or(0) < presets::BUILT_IN_ROUND {
        presets::add_built_in(&mut kept);
        save_pref(PRESETS_KEY, &kept);
        save_pref(BUILT_IN_KEY, &presets::BUILT_IN_ROUND);
    }
    kept
}

/// The kept presets, read from prefs.json once and held in memory after.
pub(super) fn kept_presets(ctx: &egui::Context) -> Vec<Preset> {
    let id = egui::Id::new(PRESETS_KEY);
    if let Some(kept) = ctx.data_mut(|d| d.get_temp(id)) {
        return kept;
    }
    let kept = load_presets();
    ctx.data_mut(|d| d.insert_temp(id, kept.clone()));
    kept
}

/// The "Công thức" row: load a kept set of sliders, keep the current one
/// under a name (`naming` holds the name while it is typed; `leave` closes
/// that field), or delete the one in use. Returns whether `presets` changed.
fn preset_row(
    ui: &mut egui::Ui,
    ready: bool,
    s: &mut PortraitSettings,
    presets: &mut Vec<Preset>,
    naming: &mut Option<String>,
    leave: bool,
) -> bool {
    let mut changed = false;
    // The preset in use is the one the sliders stand at.
    let current = presets.iter().position(|p| p.settings == *s);
    ui.add_enabled_ui(ready, |ui| {
        ui.horizontal(|ui| {
            ui.label("Công thức");
            let shown = match current {
                Some(i) => presets[i].name.as_str(),
                None if presets.is_empty() => "Chưa lưu",
                None => "Chọn…",
            };
            egui::ComboBox::from_id_salt("portrait_preset")
                .selected_text(shown)
                .width(118.0)
                .show_ui(ui, |ui| {
                    for (i, preset) in presets.iter().enumerate() {
                        if ui
                            .selectable_label(Some(i) == current, &preset.name)
                            .clicked()
                        {
                            *s = preset.settings;
                        }
                    }
                })
                .response
                .on_hover_text("Nạp lại bộ thanh kéo đã lưu");
            if ui
                .button(format!("{} Lưu…", ph::FLOPPY_DISK))
                .on_hover_text(
                    "Lưu các thanh đang chỉnh thành một công thức, để dùng lại cho ảnh khác",
                )
                .clicked()
            {
                *naming = Some(current.map_or_else(String::new, |i| presets[i].name.clone()));
                ui.memory_mut(|m| m.request_focus(egui::Id::new(PRESET_NAME_FIELD)));
            }
            if ui
                .add_enabled(current.is_some(), egui::Button::new(ph::TRASH))
                .on_hover_text("Xóa công thức đang chọn")
                .clicked()
            {
                if let Some(i) = current {
                    presets.remove(i);
                    changed = true;
                }
            }
        });
    });
    if let Some(name) = naming {
        let (mut keep, mut done) = (false, leave);
        ui.horizontal(|ui| {
            let field = ui.add(
                egui::TextEdit::singleline(name)
                    .id(egui::Id::new(PRESET_NAME_FIELD))
                    .desired_width(180.0)
                    .hint_text("Tên, vd. Nữ, Nam, Trẻ em"),
            );
            keep = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            done |= field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape));
            keep |= ui
                .add_enabled(!name.trim().is_empty(), egui::Button::new("Lưu"))
                .on_hover_text("Trùng tên thì ghi đè công thức cũ")
                .clicked();
            done |= ui.button("Thôi").clicked();
        });
        if keep && !name.trim().is_empty() {
            presets::keep(presets, name, *s);
            changed = true;
            done = true;
        }
        if done {
            *naming = None;
        }
    }
    changed
}

fn slider_colors() -> [egui::Color32; 3] {
    [
        egui::Color32::from_rgb(52, 46, 44),
        egui::Color32::from_rgb(150, 118, 100),
        egui::Color32::from_rgb(246, 214, 190),
    ]
}

/// The dialog's collapsible groups; one is open at a time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Group {
    Brush,
    Skin,
    Shape,
    Body,
    Eyes,
    Nose,
    Mouth,
    Brows,
    Hair,
    Detail,
    Fix,
    Look,
    Sheet,
}

impl Group {
    fn icon(self) -> crate::ui::widgets::HeaderIcon<'static> {
        use crate::ui::widgets::HeaderIcon;
        HeaderIcon::Glyph(match self {
            Group::Brush => ph::PAINT_BRUSH,
            Group::Skin => ph::SPARKLE,
            Group::Shape => ph::SMILEY,
            Group::Body => ph::PERSON,
            Group::Eyes => ph::EYE,
            Group::Nose => ph::TRIANGLE,
            // Phosphor has no mouth.
            Group::Mouth => return HeaderIcon::Lips,
            Group::Brows => ph::RAINBOW,
            Group::Hair => ph::SCISSORS,
            Group::Detail => ph::MAGNIFYING_GLASS_PLUS,
            Group::Fix => ph::SUN,
            Group::Look => ph::PALETTE,
            Group::Sheet => ph::PRINTER,
        })
    }
}

/// One of the two tiles at the top of the dialog: an icon over a name,
/// filled while it is the side shown.
fn side_tile(
    ui: &mut egui::Ui,
    width: f32,
    icon: &str,
    name: &str,
    chosen: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 46.0), egui::Sense::click());
    let visuals = ui.visuals();
    let (fill, stroke, text) = if chosen {
        (
            visuals.selection.bg_fill,
            visuals.selection.stroke.color,
            visuals.strong_text_color(),
        )
    } else if response.hovered() {
        (
            visuals.widgets.hovered.weak_bg_fill,
            visuals.widgets.hovered.bg_stroke.color,
            visuals.text_color(),
        )
    } else {
        (
            visuals.widgets.inactive.weak_bg_fill,
            visuals.widgets.noninteractive.bg_stroke.color,
            visuals.text_color(),
        )
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        5.0,
        fill,
        egui::Stroke::new(1.0_f32, stroke),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center_top() + egui::vec2(0.0, 5.0),
        egui::Align2::CENTER_TOP,
        icon,
        egui::FontId::proportional(18.0),
        text,
    );
    painter.text(
        rect.center_bottom() - egui::vec2(0.0, 5.0),
        egui::Align2::CENTER_BOTTOM,
        name,
        crate::ui::theme::bold_font(ui.ctx(), 13.0),
        text,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// One group: its header, and its contents while it is the open group
/// (`shown`, as of the start of the frame). A click on the header opens it
/// in `next`, closing the others, or closes it.
fn group(
    ui: &mut egui::Ui,
    shown: Option<Group>,
    next: &mut Option<Group>,
    id: Group,
    title: &str,
    active: bool,
    body: impl FnOnce(&mut egui::Ui),
) {
    ui.add_space(3.0);
    let open = shown == Some(id);
    if crate::ui::widgets::section_header(ui, id.icon(), title, open, active).clicked() {
        *next = if shown == Some(id) { None } else { Some(id) };
    }
    if shown == Some(id) {
        ui.add_space(2.0);
        body(ui);
        ui.add_space(4.0);
    }
}

/// A small note inside a group: grey, or orange for a warning.
fn note_line(ui: &mut egui::Ui, note: &str, warning: bool) {
    let colour = if warning {
        egui::Color32::from_rgb(220, 150, 90)
    } else {
        egui::Color32::from_gray(150)
    };
    ui.label(egui::RichText::new(note).size(10.0).color(colour));
}

/// How a slider row reads: 0..100, two-sided -100..100, or a colour picker
/// over the hue circle (0..360) drawn as a rainbow like Hue/Saturation.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Amount,
    TwoSided,
    Hue,
}

fn rainbow() -> [egui::Color32; 7] {
    [
        egui::Color32::from_rgb(255, 0, 0),
        egui::Color32::from_rgb(255, 255, 0),
        egui::Color32::from_rgb(0, 255, 0),
        egui::Color32::from_rgb(0, 255, 255),
        egui::Color32::from_rgb(0, 0, 255),
        egui::Color32::from_rgb(255, 0, 255),
        egui::Color32::from_rgb(255, 0, 0),
    ]
}

/// A slider row: label, value, what it does, kind. The note is for the
/// reader of this file: popping up over every slider cluttered the dialog.
type Row<'a> = (&'a str, &'a mut f32, &'a str, Kind);

/// Whether any slider (not a colour picker) is away from 0.
fn at_work(items: &[Row]) -> bool {
    items
        .iter()
        .any(|(_, v, _, k)| *k != Kind::Hue && **v != 0.0)
}

/// The studio looks as chips (three skin swatches and a name), two a row,
/// then the strength slider.
fn look_section(ui: &mut egui::Ui, ready: bool, s: &mut PortraitSettings) {
    let current = StudioLook::from_index(s.look);
    let width = (ui.available_width() - 6.0) / 2.0;
    for pair in StudioLook::ALL.chunks(2) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for &look in pair {
                let (rect, response) =
                    ui.allocate_exact_size(egui::vec2(width, 26.0), egui::Sense::click());
                let visuals = ui.visuals();
                let selected = look == current;
                let fill = if selected {
                    visuals.selection.bg_fill
                } else if response.hovered() {
                    visuals.widgets.hovered.weak_bg_fill
                } else {
                    visuals.widgets.inactive.weak_bg_fill
                };
                let painter = ui.painter();
                painter.rect_filled(rect, 3.0, fill);
                let mut x = rect.left() + 6.0;
                if look != StudioLook::None {
                    for c in look.swatch() {
                        let square = egui::Rect::from_min_size(
                            egui::pos2(x, rect.center().y - 6.0),
                            egui::vec2(12.0, 12.0),
                        );
                        painter.rect_filled(square, 2.0, egui::Color32::from_rgb(c[0], c[1], c[2]));
                        x += 13.0;
                    }
                    x += 4.0;
                }
                painter.text(
                    egui::pos2(x, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    look.label(),
                    egui::FontId::proportional(12.0),
                    egui::Color32::from_rgb(220, 220, 220),
                );
                if ready
                    && response
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text(look.tip())
                        .clicked()
                {
                    s.look = look.index();
                }
            }
        });
    }
    ui.add_space(2.0);
    let strength = vec![(
        "Độ đậm",
        &mut s.look_strength,
        "Pha bộ màu vào ảnh mạnh hay nhẹ (gộp chung vào layer \"Chân dung\")",
        Kind::Amount,
    )];
    rows(ui, ready && current != StudioLook::None, strength);
}

fn rows(ui: &mut egui::Ui, enabled: bool, items: Vec<Row>) {
    for (label, value, _, kind) in items {
        let (range, colours): (_, Vec<egui::Color32>) = match kind {
            Kind::Amount => (0.0..=100.0, slider_colors().to_vec()),
            Kind::TwoSided => (-100.0..=100.0, slider_colors().to_vec()),
            Kind::Hue => (0.0..=360.0, rainbow().to_vec()),
        };
        ui.add_enabled_ui(enabled, |ui| {
            crate::ui::widgets::dev_slider_stacked_resp(ui, label, value, range, &colours, 1.0);
        });
    }
}

/// "Tô vùng": which area the brush paints, then its mode, size and hardness.
fn brush_section(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions, ready: bool) {
    let d = &data.dialogs;
    ui.add_enabled_ui(ready, |ui| {
        ui.horizontal(|ui| {
            let targets = [
                (None, "Tắt", "Không tô — bấm lên ảnh không làm gì"),
                (
                    Some(MaskTarget::Skin),
                    "Da",
                    "Tô thêm / bớt vùng da app nhận ra (hiện màu đỏ)",
                ),
                (
                    Some(MaskTarget::Hair),
                    "Tóc",
                    "Tô thêm / bớt vùng tóc app nhận ra (hiện màu tím)",
                ),
                (
                    Some(MaskTarget::Brows),
                    "Lông mày",
                    "Tô thêm / bớt vùng lông mày mà các thanh \"Lông mày\" tác động (hiện màu vàng). Vùng lông mày không bị làm mịn da",
                ),
            ];
            for (target, label, tip) in targets {
                let enabled = target != Some(MaskTarget::Hair) || d.portrait_hair;
                let response = ui
                    .add_enabled_ui(enabled, |ui| {
                        ui.selectable_label(d.portrait_brush == target, label)
                    })
                    .inner
                    .on_hover_text(tip);
                if response.clicked() {
                    actions.dialogs.set_portrait_brush = Some(target);
                    if target == Some(MaskTarget::Brows)
                        && data.sel.refine_brush_mode == RefineBrushMode::Smart
                    {
                        actions.sel.set_refine_brush_mode = Some(RefineBrushMode::Add);
                    }
                }
            }
        });
        if d.portrait_brush.is_none() {
            return;
        }
        // Brows are a soft shape around sparse hairs: painted plainly.
        let smart = d.portrait_brush != Some(MaskTarget::Brows);
        ui.horizontal(|ui| {
            let modes = [
                (
                    RefineBrushMode::Smart,
                    format!("{} Thông minh", ph::SPARKLE),
                    "Chọn theo màu: điểm giống vùng đang tô hơn nền thì được thêm, sợi mờ thêm mờ — tô lại để đậm hơn",
                ),
                (
                    RefineBrushMode::Add,
                    format!("{} Thêm", ph::PLUS),
                    "Tô thêm vào vùng",
                ),
                (
                    RefineBrushMode::Subtract,
                    format!("{} Bớt", ph::MINUS),
                    "Tô bớt khỏi vùng",
                ),
            ];
            for (mode, label, tip) in modes {
                if mode == RefineBrushMode::Smart && !smart {
                    continue;
                }
                if ui
                    .selectable_label(data.sel.refine_brush_mode == mode, label)
                    .on_hover_text(tip)
                    .clicked()
                {
                    actions.sel.set_refine_brush_mode = Some(mode);
                }
            }
        });
        let mut size = data.sel.refine_brush_size;
        if crate::ui::widgets::dev_slider_stacked_log_resp(
            ui,
            "Cỡ cọ",
            &mut size,
            1.0..=1000.0,
            &slider_colors(),
        )
        .changed()
        {
            actions.sel.set_refine_brush_size = Some(size);
        }
        let mut hardness = data.sel.refine_brush_hardness * 100.0;
        if crate::ui::widgets::dev_slider_stacked_resp(
            ui,
            "Độ cứng",
            &mut hardness,
            0.0..=100.0,
            &slider_colors(),
            1.0,
        )
        .changed()
        {
            actions.sel.set_refine_brush_hardness = Some(hardness / 100.0);
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    d.portrait_brush_undo,
                    egui::Button::new(ph::ARROW_U_UP_LEFT),
                )
                .on_hover_text("Hoàn tác nét tô (Ctrl+Z)")
                .clicked()
            {
                actions.dialogs.portrait_brush_undo = true;
            }
            if ui
                .add_enabled(
                    d.portrait_brush_redo,
                    egui::Button::new(ph::ARROW_U_UP_RIGHT),
                )
                .on_hover_text("Làm lại nét tô (Ctrl+Shift+Z)")
                .clicked()
            {
                actions.dialogs.portrait_brush_redo = true;
            }
            ui.label(
                egui::RichText::new("Alt: đảo Thêm ↔ Bớt (Thông minh: bớt phần giống nền)")
                    .size(10.0)
                    .color(egui::Color32::from_gray(150)),
            );
        });
    });
}

pub(crate) fn portrait_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let settings_id = egui::Id::new("portrait_settings");
    let faces_id = egui::Id::new("portrait_faces");
    let preview_id = egui::Id::new("portrait_preview");
    let masks_id = egui::Id::new("portrait_masks");
    let group_id = egui::Id::new("portrait_group");
    let presets_id = egui::Id::new(PRESETS_KEY);
    let naming_id = egui::Id::new("portrait_preset_naming");
    let open_id = egui::Id::new("portrait_dialog_open");
    let mut presets = kept_presets(ctx);
    let mut naming: Option<String> = ctx.data_mut(|d| d.get_temp(naming_id).unwrap_or(None));
    let mut s: PortraitSettings = ctx.data_mut(|d| d.get_temp(settings_id).unwrap_or_default());
    let mut id_side = data.dialogs.portrait_id_side;
    // The dialog opens with no retouch under way, on the side used last. The
    // Ảnh thẻ side's sliders start from the preset used last, the Chân dung
    // side's from the usual ones.
    let opened = !ctx.data_mut(|d| d.get_temp::<bool>(open_id).unwrap_or(false));
    if opened && !data.dialogs.portrait_session && !data.dialogs.id_photo_made {
        if let Some(last) = load_pref::<bool>(SIDE_KEY).filter(|&last| last != id_side) {
            id_side = last;
            actions.dialogs.set_portrait_side = Some(last);
        }
        s = if id_side {
            super::id_photo::starting_settings(&presets)
        } else {
            PortraitSettings::default()
        };
    }
    let mut preview: bool = ctx.data_mut(|d| d.get_temp(preview_id).unwrap_or(true));
    let mut masks: bool = ctx.data_mut(|d| d.get_temp(masks_id).unwrap_or(false));
    // Every group starts closed; opening one closes the rest.
    let shown: Option<Group> = ctx.data_mut(|d| d.get_temp(group_id).unwrap_or(None));
    let mut next = shown;
    let face_count = data.dialogs.portrait_faces.len();
    let mut faces: Vec<bool> = ctx.data_mut(|d| d.get_temp(faces_id).unwrap_or_default());
    // A reopened "Chân dung" layer brings back what it was made with.
    let d = &data.dialogs;
    if let Some(saved) = d.portrait_restore_settings {
        s = saved;
    }
    if let Some(saved) = &d.portrait_restore_faces {
        faces = saved.clone();
    }
    if d.portrait_restore_settings.is_some() || d.portrait_restore_faces.is_some() {
        actions.dialogs.portrait_restored = Some((
            d.portrait_restore_settings.is_some(),
            d.portrait_restore_faces.is_some(),
        ));
    }
    faces.resize(face_count, true);

    let ready = data.dialogs.portrait_ready;
    // While a value or a preset's name is being typed, Esc and Enter belong
    // to that field: Esc leaves it and the dialog, with its sliders, stays.
    let typing = crate::ui::widgets::typing_in_a_field(ctx, egui::Id::new("portrait_typing"));
    // Open and idle the dialog is a panel beside the tools: Enter and Esc
    // are theirs (a crop to commit or give up). With a retouch previewed or
    // an ID photo being made they are Áp dụng and Hủy.
    let under_way = data.dialogs.portrait_session || data.dialogs.id_photo_busy;
    let (enter_pressed, esc_pressed) = if typing || !under_way {
        (false, false)
    } else {
        consume_dialog_enter_escape(ctx)
    };
    // With the name row open, Esc closes the row, not the dialog.
    let leave_naming = esc_pressed && naming.is_some();
    let mut do_apply = enter_pressed && ready;
    let mut do_cancel = esc_pressed && !leave_naming;
    let mut open = true;
    let mut sheet = None;
    let mut folder = None;

    let default_pos = document_side_dialog_pos(ctx, data, 320.0, 96.0);
    egui::Window::new("Auto retouch")
        .collapsible(false)
        .resizable(false)
        .movable(true)
        .open(&mut open)
        .default_pos(default_pos)
        .order(DIALOG_ORDER)
        .min_width(320.0)
        .max_width(320.0)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 4.0);
            ui.horizontal(|ui| {
                let width = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
                for (side, icon, name) in [
                    (true, ph::IDENTIFICATION_CARD, "Ảnh thẻ"),
                    (false, ph::USER_CIRCLE, "Chân dung"),
                ] {
                    if side_tile(ui, width, icon, name, id_side == side).clicked()
                        && id_side != side
                    {
                        actions.dialogs.set_portrait_side = Some(side);
                        save_pref(SIDE_KEY, &side);
                    }
                }
            });
            ui.add_space(4.0);
            if id_side {
                super::id_photo::id_photo_section(ui, data, actions, &mut s, &presets);
                ui.add_space(2.0);
                ui.separator();
            }
            if data.dialogs.portrait_session {
                ui.horizontal(|ui| {
                    if !ready && !data.dialogs.portrait_status.starts_with("Không") {
                        ui.spinner();
                    }
                    ui.label(
                        egui::RichText::new(&data.dialogs.portrait_status)
                            .size(11.0)
                            .color(egui::Color32::from_gray(170)),
                    );
                });
            } else if !id_side {
                // Nothing is analysed until asked.
                let button = egui::Button::new(
                    egui::RichText::new(format!("{}  Tự động làm đẹp", ph::MAGIC_WAND)).strong(),
                );
                ui.add_enabled_ui(data.doc.has_doc && !data.dialogs.id_photo_busy, |ui| {
                    if ui
                        .add_sized([ui.available_width(), 28.0], button)
                        .on_hover_text(
                            "App nhận diện khuôn mặt rồi làm đẹp theo mức thường dùng; sau đó kéo các thanh bên dưới để chỉnh thêm",
                        )
                        .clicked()
                    {
                        actions.dialogs.start_portrait_retouch = true;
                    }
                });
                if !data.dialogs.portrait_status.is_empty() {
                    ui.label(
                        egui::RichText::new(&data.dialogs.portrait_status)
                            .size(11.0)
                            .color(egui::Color32::from_rgb(220, 150, 90)),
                    );
                }
            }
            if face_count > 1 {
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    for (i, on) in faces.iter_mut().enumerate() {
                        let mut label = format!("Mặt {}", i + 1);
                        if !data.dialogs.portrait_faces[i] {
                            label.push_str(" (mốc mặt)");
                        }
                        ui.checkbox(on, label).on_hover_text(if data.dialogs.portrait_faces[i] {
                            "Bỏ tích để không chỉnh khuôn mặt này"
                        } else {
                            "Hai model lệch nhau ở mặt này — chỉnh theo mốc mặt, mép có thể kém hơn"
                        });
                    }
                });
            } else if face_count == 1 && !data.dialogs.portrait_faces[0] {
                ui.label(
                    egui::RichText::new("Mặt này chỉnh theo mốc mặt (mép có thể kém hơn).")
                        .size(10.0)
                        .color(egui::Color32::from_rgb(220, 150, 90)),
                );
            }
            if !id_side {
                ui.add_space(4.0);
                if preset_row(ui, ready, &mut s, &mut presets, &mut naming, leave_naming) {
                    save_pref(PRESETS_KEY, &presets);
                }
            }

            // The Ảnh thẻ side's own controls take room above the groups.
            let room = ctx.content_rect().height() * 0.62 - if id_side { 230.0 } else { 0.0 };
            egui::ScrollArea::vertical()
                .max_height(room.max(120.0))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    use Kind::*;
                    let hair = data.dialogs.portrait_hair;
                    group(
                        ui,
                        shown,
                        &mut next,
                        Group::Brush,
                        "Tô vùng",
                        data.dialogs.portrait_brush_undo,
                        |ui| brush_section(ui, data, actions, ready),
                    );
                    let skin = vec![
                        ("Làm mịn da", &mut s.smooth, "Mịn da nhưng giữ vân lỗ chân lông", Amount),
                        (
                            "Tạo khối",
                            &mut s.volume,
                            "Giữ khối mặt (sống mũi, cánh mũi, nếp má, gò má) khi làm mịn mạnh — kéo cao khi da bị bệt",
                            Amount,
                        ),
                        (
                            "Vân da",
                            &mut s.texture,
                            "Thêm vân lỗ chân lông cho da bệt (ảnh điện thoại, độ phân giải thấp, làm mịn mạnh)",
                            Amount,
                        ),
                        ("Đều màu da", &mut s.even_tone, "Giảm mảng đỏ, loang màu", Amount),
                        ("Giảm bóng dầu", &mut s.shine, "Dịu các vùng bóng loáng", Amount),
                        (
                            "Sáng da",
                            &mut s.brighten,
                            "Trái: da tối hơn — phải: da sáng hơn (như thanh Midtones của Develop: tông giữa đổi nhiều, vùng rất sáng và rất tối ít đổi, giữ màu và vân da)",
                            TwoSided,
                        ),
                        (
                            "Xóa mụn",
                            &mut s.blemish,
                            "Tự tìm và xóa mụn, đốm thâm nhỏ (lấp bằng vân da lành bên cạnh)",
                            Amount,
                        ),
                        ("Quầng thâm", &mut s.dark_circles, "Làm sáng vùng dưới mắt", Amount),
                    ];
                    group(ui, shown, &mut next, Group::Skin, "Da", at_work(&skin), |ui| {
                        rows(ui, ready, skin)
                    });
                    let face = vec![
                        (
                            "Mặt thon",
                            &mut s.face_slim,
                            "Phải: hàm và má thon lại — trái: mặt đầy hơn",
                            TwoSided,
                        ),
                        (
                            "Bóp mặt",
                            &mut s.face_squeeze,
                            "Phải: cả khuôn mặt hẹp lại theo chiều ngang (mắt, mũi, miệng theo cùng tỉ lệ) — trái: rộng ra",
                            TwoSided,
                        ),
                        (
                            "Cằm",
                            &mut s.chin_length,
                            "Phải: cằm dài hơn — trái: cằm ngắn lại",
                            TwoSided,
                        ),
                        (
                            "Trán",
                            &mut s.forehead_height,
                            "Phải: trán cao hơn — trái: trán thấp lại",
                            TwoSided,
                        ),
                    ];
                    group(ui, shown, &mut next, Group::Shape, "Dáng mặt", at_work(&face), |ui| {
                        rows(ui, ready, face)
                    });
                    let body = vec![
                        (
                            "Eo thon",
                            &mut s.body_waist,
                            "Phải: eo thon lại — trái: eo đầy hơn",
                            TwoSided,
                        ),
                        (
                            "Vai",
                            &mut s.body_shoulders,
                            "Phải: vai hẹp lại — trái: vai rộng ra (tay đi theo vai)",
                            TwoSided,
                        ),
                        (
                            "Cổ",
                            &mut s.body_neck,
                            "Phải: cổ cao hơn (đầu nâng lên) — trái: cổ ngắn lại",
                            TwoSided,
                        ),
                        (
                            "Tay thon",
                            &mut s.body_arms,
                            "Phải: tay thon lại — trái: tay đầy hơn",
                            TwoSided,
                        ),
                        (
                            "Chân thon",
                            &mut s.body_legs,
                            "Phải: chân thon lại — trái: chân đầy hơn",
                            TwoSided,
                        ),
                        (
                            "Chân dài",
                            &mut s.body_leg_length,
                            "Kéo dài phần dưới hông (chỉ khi đứng, thấy cả bàn chân và còn chỗ dưới chân)",
                            Amount,
                        ),
                    ];
                    group(ui, shown, &mut next, Group::Body, "Dáng người", at_work(&body), |ui| {
                        if let Some((note, warning)) = &data.dialogs.portrait_body {
                            note_line(ui, note, *warning);
                        }
                        rows(ui, ready, body)
                    });
                    let eyes = vec![
                        (
                            "Mắt to",
                            &mut s.eye_size,
                            "Phải: mắt to hơn — trái: mắt nhỏ lại",
                            TwoSided,
                        ),
                        (
                            "Mắt nghiêng",
                            &mut s.eye_tilt,
                            "Phải: đuôi mắt xếch lên — trái: đuôi mắt cụp xuống",
                            TwoSided,
                        ),
                        ("Trắng mắt", &mut s.eye_white, "Lòng trắng mắt sáng, bớt đỏ", Amount),
                        ("Sáng tròng mắt", &mut s.iris, "Tròng mắt sáng và trong hơn", Amount),
                        (
                            "Đậm / giảm màu mắt",
                            &mut s.eye_saturation,
                            "Trái: giảm màu cả mắt, lòng trắng lẫn tròng (mắt đỏ, đau mắt đỏ, kính áp tròng màu) về màu trung tính — phải: tròng mắt đậm màu hơn",
                            TwoSided,
                        ),
                        (
                            "Màu tròng mắt",
                            &mut s.iris_hue,
                            "Chọn màu trên dải — cần kéo \"Phủ màu tròng\" để thấy",
                            Hue,
                        ),
                        ("Phủ màu tròng", &mut s.iris_tint, "0 = giữ màu mắt thật", Amount),
                    ];
                    group(ui, shown, &mut next, Group::Eyes, "Mắt", at_work(&eyes), |ui| {
                        rows(ui, ready, eyes)
                    });
                    let nose = vec![
                        (
                            "Mũi thon",
                            &mut s.nose_slim,
                            "Phải: cánh mũi hẹp lại — trái: mũi rộng hơn",
                            TwoSided,
                        ),
                        (
                            "Sống mũi cao",
                            &mut s.nose_bridge,
                            "Tạo khối: sáng dọc sống mũi, tối nhẹ hai bên",
                            Amount,
                        ),
                    ];
                    group(ui, shown, &mut next, Group::Nose, "Mũi", at_work(&nose), |ui| {
                        rows(ui, ready, nose)
                    });
                    let lips = vec![
                        (
                            "Rộng miệng",
                            &mut s.mouth_width,
                            "Phải: miệng rộng hơn — trái: miệng hẹp lại",
                            TwoSided,
                        ),
                        (
                            "Cười",
                            &mut s.smile,
                            "Phải: khóe miệng nhếch lên (cười) — trái: khóe miệng trễ xuống (mếu)",
                            TwoSided,
                        ),
                        (
                            "Môi dày",
                            &mut s.lip_fullness,
                            "Phải: môi dày hơn — trái: môi mỏng lại",
                            TwoSided,
                        ),
                        (
                            "Đậm / giảm màu môi",
                            &mut s.lip_saturation,
                            "Trái: giảm màu môi (son đậm nhạt bớt) — phải: môi đậm, tươi hơn",
                            TwoSided,
                        ),
                        (
                            "Sáng môi",
                            &mut s.lip_brightness,
                            "Trái: môi tối hơn — phải: môi sáng hơn",
                            TwoSided,
                        ),
                        (
                            "Màu môi",
                            &mut s.lip_hue,
                            "Chọn màu son trên dải — cần kéo \"Phủ màu môi\" để thấy",
                            Hue,
                        ),
                        ("Phủ màu môi", &mut s.lip_tint, "0 = giữ màu môi thật", Amount),
                        ("Trắng răng", &mut s.teeth, "Răng trắng, bớt ố vàng", Amount),
                    ];
                    group(ui, shown, &mut next, Group::Mouth, "Miệng, môi & răng", at_work(&lips), |ui| {
                        rows(ui, ready, lips)
                    });
                    let brows = vec![
                        (
                            "Đậm nhạt",
                            &mut s.brows,
                            "0 = giữ nguyên — trái: lông mày nhạt đi — phải: đậm hơn",
                            TwoSided,
                        ),
                        (
                            "Độ nét",
                            &mut s.brow_sharpen,
                            "0 = giữ nguyên — sợi lông mày rõ nét hơn",
                            Amount,
                        ),
                        (
                            "Đậm / giảm màu lông mày",
                            &mut s.brow_saturation,
                            "Trái: giảm màu lông mày (nhuộm, xăm ngả nâu đỏ) về màu trung tính — phải: màu lông mày đậm hơn",
                            TwoSided,
                        ),
                        (
                            "Màu lông mày",
                            &mut s.brow_hue,
                            "Chọn màu trên dải — cần kéo \"Phủ màu lông mày\" để thấy",
                            Hue,
                        ),
                        (
                            "Phủ màu lông mày",
                            &mut s.brow_tint,
                            "0 = giữ màu lông mày thật",
                            Amount,
                        ),
                    ];
                    group(ui, shown, &mut next, Group::Brows, "Lông mày", at_work(&brows), |ui| {
                        rows(ui, ready, brows)
                    });
                    let hair_rows = vec![
                        (
                            "Sáng tóc",
                            &mut s.hair_brightness,
                            "Trái: tóc tối hơn — phải: tóc sáng hơn (như thanh Blacks của Develop, giữ màu và vân tóc)",
                            TwoSided,
                        ),
                        (
                            "Đậm / giảm màu tóc",
                            &mut s.hair_saturation,
                            "Trái: giảm màu tóc nhuộm (vàng, đỏ, nâu) về màu trung tính, kéo thêm \"Sáng tóc\" sang trái để ra tóc đen — phải: màu tóc đậm, tươi hơn",
                            TwoSided,
                        ),
                        (
                            "Màu tóc",
                            &mut s.hair_hue,
                            "Chọn màu nhuộm trên dải — cần kéo \"Phủ màu tóc\" để thấy",
                            Hue,
                        ),
                        ("Phủ màu tóc", &mut s.hair_tint, "0 = giữ màu tóc thật", Amount),
                    ];
                    let hair_active = hair && at_work(&hair_rows);
                    group(ui, shown, &mut next, Group::Hair, "Tóc", hair_active, |ui| {
                        if !hair && ready {
                            ui.label(
                                egui::RichText::new(
                                    "Không nhận ra tóc (cần model tách vùng Sapiens2).",
                                )
                                .size(10.0)
                                .color(egui::Color32::from_rgb(220, 150, 90)),
                            );
                        }
                        rows(ui, ready && hair, hair_rows)
                    });
                    let detail = vec![
                        (
                            "Chi tiết mặt (AI)",
                            &mut s.ai_detail,
                            "AI vẽ lại chi tiết cả khuôn mặt và tóc (da, mắt, mi, lông mày, môi, sợi tóc) cho ảnh mờ, nhiễu, ảnh điện thoại, ảnh nhỏ — màu, sáng tối và dáng mặt vẫn là của ảnh. Da sau AI rất sạch: kéo thêm \"Vân da\" nếu muốn có lỗ chân lông. Ảnh vốn đã nét thì kéo thấp lại. App chạy AI vài giây cho mỗi khuôn mặt",
                            Amount,
                        ),
                        (
                            "Tăng nét",
                            &mut s.sharpen,
                            "Mắt, mi, môi nét hơn (không đụng da và lông mày)",
                            Amount,
                        ),
                    ];
                    group(ui, shown, &mut next, Group::Detail, "Chi tiết", at_work(&detail), |ui| {
                        rows(ui, ready, detail);
                        if let Some((note, warning)) = &data.dialogs.portrait_detail {
                            note_line(ui, note, *warning);
                        }
                    });
                    let fix = vec![
                        (
                            "Khử ám màu",
                            &mut s.fix_cast,
                            "Tự đo ám màu từ chính màu da khuôn mặt rồi khử cho cả ảnh (ám xanh, vàng do đèn, tường, kệ hàng xung quanh)",
                            Amount,
                        ),
                        (
                            "Ấm / lạnh",
                            &mut s.fix_warmth,
                            "Chỉnh tay thêm nếu ảnh còn ngả màu — trái: lạnh hơn, phải: ấm hơn",
                            TwoSided,
                        ),
                        (
                            "Cân sáng",
                            &mut s.fix_exposure,
                            "Đưa độ sáng da mặt về mức chuẩn: ảnh thiếu sáng sáng lên, ảnh quá sáng dịu lại",
                            Amount,
                        ),
                        (
                            "Khử đục",
                            &mut s.fix_haze,
                            "Lấy lại màu đen và độ trong cho ảnh bị đục, bạc màu, thiếu tương phản",
                            Amount,
                        ),
                        (
                            "Đều sáng da",
                            &mut s.even_light,
                            "Nâng sáng vùng da khuất đèn (dưới cằm, cổ, nửa mặt bên tối) lên gần bằng trán và má — cho ảnh đèn chiếu từ trên xuống hoặc lệch một bên",
                            Amount,
                        ),
                    ];
                    let mut auto_fix = false;
                    group(ui, shown, &mut next, Group::Fix, "Sửa màu & sáng", at_work(&fix), |ui| {
                        ui.add_enabled_ui(ready, |ui| {
                            auto_fix = ui
                                .button(format!("{} Tự động", ph::MAGIC_WAND))
                                .on_hover_text("Đặt lại các thanh về mức thường dùng cho ảnh điện thoại bị ám màu, tối, đục (mức một ảnh mới bắt đầu) — rồi chỉnh lại từng thanh nếu cần")
                                .clicked();
                        });
                        rows(ui, ready, fix)
                    });
                    if auto_fix {
                        s = s.with_auto_fix();
                    }
                    let look_on = s.look != 0;
                    group(ui, shown, &mut next, Group::Look, "Màu studio", look_on, |ui| {
                        look_section(ui, ready, &mut s)
                    });
                    group(ui, shown, &mut next, Group::Sheet, "Xếp ảnh in", false, |ui| {
                        note_line(
                            ui,
                            "Bấm một trang bên dưới: app áp dụng phần làm đẹp (nếu có) rồi xếp ảnh ra trang in mới.",
                            false,
                        );
                        // With no retouch under way the photo is laid out
                        // as it is.
                        let idle = !data.dialogs.portrait_session && !data.dialogs.id_photo_busy;
                        ui.add_enabled_ui(ready || idle, |ui| {
                            match print_sheet_section(ui, data) {
                                Some(SheetAsk::Photo(asked, options)) => {
                                    sheet = Some((asked, options));
                                }
                                // The folder is no part of this photo: its
                                // sheets are asked of the app, not the dialog.
                                asked => folder = asked,
                            }
                        });
                    });
                });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .small_button(format!("{} Mặc định", ph::ARROW_COUNTER_CLOCKWISE))
                    .clicked()
                {
                    s = PortraitSettings::default();
                }
                if ui
                    .small_button(format!("{} Về 0", ph::NUMBER_CIRCLE_ZERO))
                    .clicked()
                {
                    s = PortraitSettings::NEUTRAL;
                }
                ui.checkbox(&mut preview, "Xem trước")
                    .on_hover_text("Bỏ tích để xem ảnh gốc");
            });
            ui.add_enabled_ui(ready, |ui| {
                ui.checkbox(&mut masks, "Hiện vùng nhận diện").on_hover_text(
                    "Tô màu vùng app nhận ra: da đỏ, quầng mắt cam, lòng trắng xanh lá, tròng xanh dương, lông mày vàng, môi hồng, răng xanh ngọc, tóc tím",
                );
            });
            ui.add_space(6.0);
            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        ready,
                        egui::Button::new(format!("{}  Áp dụng", ph::CHECK)),
                    )
                    .on_hover_text(if data.dialogs.portrait_reopened {
                        "Cập nhật layer \"Chân dung\" đang chỉnh tiếp. Dùng công cụ hoặc lệnh khác lúc đang xem trước cũng áp dụng, bảng vẫn mở"
                    } else {
                        "Thêm kết quả (cả Màu studio nếu có chọn) thành layer mới \"Chân dung\". Dùng công cụ hoặc lệnh khác lúc đang xem trước cũng áp dụng, bảng vẫn mở; bấm Tự động làm đẹp để chỉnh tiếp"
                    })
                    .clicked()
                {
                    do_apply = true;
                }
                // With no retouch to give up it only closes the dialog.
                let leave = if data.dialogs.portrait_session {
                    "Hủy"
                } else {
                    "Đóng"
                };
                if ui.button(format!("{}  {leave}", ph::X)).clicked() {
                    do_cancel = true;
                }
            });
        });

    if !open {
        do_cancel = true;
    }
    do_apply |= sheet.is_some();
    ctx.data_mut(|d| {
        d.insert_temp(settings_id, s);
        d.insert_temp(faces_id, faces.clone());
        d.insert_temp(preview_id, preview);
        d.insert_temp(masks_id, masks);
        d.insert_temp(presets_id, presets);
        d.insert_temp(open_id, !(do_apply || do_cancel));
        if do_apply || do_cancel {
            d.remove_temp::<Option<Group>>(group_id);
            d.remove_temp::<Option<String>>(naming_id);
        } else {
            d.insert_temp(group_id, next);
            d.insert_temp(naming_id, naming);
        }
    });
    // The brush paints only while its group is open.
    if next != Some(Group::Brush) && data.dialogs.portrait_brush.is_some() {
        actions.dialogs.set_portrait_brush = Some(None);
    }

    match folder {
        Some(SheetAsk::PickFolder) => actions.doc.pick_print_folder = true,
        Some(SheetAsk::Folder(ask, options)) => actions.doc.impose_folder = Some((ask, options)),
        _ => {}
    }
    if do_apply {
        actions.dialogs.apply_portrait = Some((s, faces));
        actions.dialogs.portrait_sheet = sheet;
    } else if do_cancel {
        actions.dialogs.cancel_portrait_dialog = true;
    } else {
        actions.dialogs.set_portrait_preview = Some((s, faces, preview, masks));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opt-in: IAI_UI_SNAPSHOT is a folder; the dialog is drawn into
    /// `chan_dung_<group>.png` there, one group open, the pointer on a row.
    #[test]
    #[ignore]
    fn probe_dialog_snapshots() {
        let Ok(dir) = std::env::var("IAI_UI_SNAPSHOT") else {
            return;
        };
        // The Chân dung side, then the Ảnh thẻ side before its photo is
        // made and after.
        for (name, open, hover, id_side, made) in [
            ("dong", None, egui::pos2(260.0, 215.0), false, false),
            (
                "da",
                Some(Group::Skin),
                egui::pos2(260.0, 330.0),
                false,
                false,
            ),
            (
                "mieng",
                Some(Group::Mouth),
                egui::pos2(420.0, 420.0),
                false,
                false,
            ),
            ("anh_the", None, egui::pos2(260.0, 250.0), true, false),
            ("cho", None, egui::pos2(260.0, 250.0), false, false),
            (
                "anh_the_xong",
                Some(Group::Sheet),
                egui::pos2(260.0, 250.0),
                true,
                true,
            ),
        ] {
            let mut data = UiData::default();
            data.doc.has_doc = true;
            data.dialogs.portrait_id_side = id_side;
            data.dialogs.id_photo_made = made;
            if (!id_side && name != "cho") || made {
                data.dialogs.portrait_session = true;
                data.dialogs.portrait_ready = true;
                data.dialogs.portrait_status = "Đã nhận 1 khuôn mặt".to_string();
                data.dialogs.portrait_faces = vec![true];
                data.dialogs.portrait_hair = true;
            }
            if made {
                // The box as it holds a garment the photo wears (the font
                // atlas stands in for the garment's picture).
                data.dialogs.garment_thumb = Some(egui::TextureId::default());
                data.dialogs.garment_worn = true;
                data.dialogs.garment_status =
                    "Đã mặc áo \"Layer 23\". Áo là layer riêng: bấm Chỉnh áo để dời, phóng, xoay"
                        .to_string();
                data.dialogs.id_photo_status =
                    "Làm ảnh thẻ xong: 2,8×3,8 cm, 1043×1417 px, nền trắng".to_string();
                (data.doc.canvas_w, data.doc.canvas_h, data.doc.canvas_dpi) = (1043, 1417, 947.2);
            }
            let image = crate::ui::snapshot::render(460.0, 1000.0, 1.5, Some(hover), |ctx| {
                ctx.data_mut(|d| {
                    d.insert_temp(egui::Id::new(PRESETS_KEY), presets::built_in());
                    d.insert_temp(egui::Id::new("portrait_group"), open);
                });
                let mut actions = UiActions::default();
                portrait_dialog(ctx, &data, &mut actions);
            });
            image
                .save(std::path::Path::new(&dir).join(format!("chan_dung_{name}.png")))
                .unwrap();
        }
    }

    #[test]
    fn a_tile_asks_for_the_other_side_and_each_side_shows_its_own_controls() {
        let ctx = egui::Context::default();
        let mut data = UiData::default();
        data.doc.has_doc = true;
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(460.0, 1000.0));
        let draw = |data: &UiData, events: Vec<egui::Event>| {
            let mut actions = UiActions::default();
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events,
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| {
                ui.ctx().data_mut(|d| {
                    d.insert_temp(egui::Id::new(PRESETS_KEY), presets::built_in());
                });
                portrait_dialog(ui.ctx(), data, &mut actions);
            });
            (output, actions)
        };
        let find = |output: &egui::FullOutput, label: &str| -> Option<egui::Pos2> {
            output
                .shapes
                .iter()
                .find_map(|clipped| match &clipped.shape {
                    egui::Shape::Text(text) if text.galley.text() == label => {
                        Some(text.pos + egui::vec2(4.0, 5.0))
                    }
                    _ => None,
                })
        };
        let click = |data: &UiData, at: egui::Pos2| {
            let button = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            draw(data, vec![egui::Event::PointerMoved(at)]);
            draw(data, vec![button(true)]);
            draw(data, vec![button(false)]).1
        };
        let settle = |data: &UiData| {
            for _ in 0..8 {
                draw(data, vec![]);
            }
            draw(data, vec![]).0
        };

        // The Ảnh thẻ side: its chips and button, no "Công thức" row.
        data.dialogs.portrait_id_side = true;
        let output = settle(&data);
        assert!(find(&output, "Nền").is_some() && find(&output, "Cỡ").is_some());
        assert!(find(&output, "Công thức").is_none());
        // Its own tile asks for nothing, the other one for its side.
        let asked = click(&data, find(&output, "Ảnh thẻ").unwrap());
        assert!(asked.dialogs.set_portrait_side.is_none());
        let asked = click(&data, find(&output, "Chân dung").unwrap());
        assert_eq!(asked.dialogs.set_portrait_side, Some(false));

        // The Chân dung side: the row of presets, none of the ID photo's,
        // and the retouch starts only by its button.
        data.dialogs.portrait_id_side = false;
        let output = settle(&data);
        assert!(find(&output, "Công thức").is_some());
        assert!(find(&output, "Nền").is_none() && find(&output, "Cỡ").is_none());
        let button = format!("{}  Tự động làm đẹp", ph::MAGIC_WAND);
        let (_, idle) = draw(&data, vec![]);
        assert!(!idle.dialogs.start_portrait_retouch);
        let asked = click(&data, find(&output, &button).expect("the button"));
        assert!(asked.dialogs.start_portrait_retouch);
        // Under way, the button gives way to the status line.
        data.dialogs.portrait_session = true;
        data.dialogs.portrait_status = "Đang tìm khuôn mặt…".to_string();
        let output = settle(&data);
        assert!(find(&output, &button).is_none());
        assert!(find(&output, "Đang tìm khuôn mặt…").is_some());
        let asked = click(&data, find(&output, "Ảnh thẻ").unwrap());
        assert_eq!(asked.dialogs.set_portrait_side, Some(true));
    }

    #[test]
    fn a_sheet_can_be_asked_for_with_no_retouch_under_way() {
        use crate::core::imposition::{Paper, PhotoKind, Sheet, SheetOptions};
        let ctx = egui::Context::default();
        let mut data = UiData::default();
        data.doc.has_doc = true;
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(460.0, 1000.0));
        let draw = |data: &UiData, events: Vec<egui::Event>| {
            let mut actions = UiActions::default();
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events,
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| {
                ui.ctx().data_mut(|d| {
                    d.insert_temp(egui::Id::new(PRESETS_KEY), presets::built_in());
                    d.insert_temp(egui::Id::new("portrait_group"), Some(Group::Sheet));
                    d.insert_temp(
                        egui::Id::new("print_sheet_options"),
                        SheetOptions::default(),
                    );
                });
                portrait_dialog(ui.ctx(), data, &mut actions);
            });
            (output, actions)
        };
        // Whether a click on the "18 tấm" button (3×4 on 13×18) asks for it.
        let asks = |data: &UiData| {
            let mut output = draw(data, vec![]).0;
            for _ in 0..40 {
                output = draw(data, vec![]).0;
            }
            let at = output
                .shapes
                .iter()
                .find_map(|clipped| match &clipped.shape {
                    egui::Shape::Text(text) if text.galley.text() == "18 tấm" => {
                        Some(text.pos + egui::vec2(4.0, 5.0))
                    }
                    _ => None,
                })
                .expect("the 18 tấm button");
            let button = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            draw(data, vec![egui::Event::PointerMoved(at)]);
            draw(data, vec![button(true)]);
            let (_, actions) = draw(data, vec![button(false)]);
            actions.dialogs.portrait_sheet.map(|(sheet, _)| sheet)
        };
        let wanted = Some(Sheet::Grid(Paper::P13x18, PhotoKind::Id3x4));
        // Idle: the photo is laid out as it is.
        assert_eq!(asks(&data), wanted);
        // A retouch still analysing has to finish first.
        data.dialogs.portrait_session = true;
        assert_eq!(asks(&data), None);
        data.dialogs.portrait_ready = true;
        data.dialogs.portrait_faces = vec![true];
        assert_eq!(asks(&data), wanted);
    }

    #[test]
    fn open_and_idle_the_dialog_leaves_enter_and_esc_to_the_tools() {
        let ctx = egui::Context::default();
        let mut data = UiData::default();
        data.doc.has_doc = true;
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(460.0, 1000.0));
        let draw = |data: &UiData, events: Vec<egui::Event>| {
            let mut actions = UiActions::default();
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events,
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| {
                ui.ctx().data_mut(|d| {
                    d.insert_temp(egui::Id::new(PRESETS_KEY), presets::built_in());
                });
                portrait_dialog(ui.ctx(), data, &mut actions);
            });
            (output, actions)
        };
        let key = |key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let has = |output: &egui::FullOutput, label: &str| {
            output.shapes.iter().any(|clipped| {
                matches!(&clipped.shape, egui::Shape::Text(text) if text.galley.text() == label)
            })
        };
        for _ in 0..8 {
            draw(&data, vec![]);
        }
        // Idle: neither key is the dialog's, and its button only closes.
        for pressed in [egui::Key::Escape, egui::Key::Enter] {
            let (_, actions) = draw(&data, vec![key(pressed)]);
            assert!(!actions.dialogs.cancel_portrait_dialog);
            assert!(actions.dialogs.apply_portrait.is_none());
        }
        let (output, _) = draw(&data, vec![]);
        assert!(has(&output, &format!("{}  Đóng", ph::X)));

        // A retouch under way: Esc gives it up, the button says so.
        data.dialogs.portrait_session = true;
        let (output, _) = draw(&data, vec![]);
        assert!(has(&output, &format!("{}  Hủy", ph::X)));
        let (_, actions) = draw(&data, vec![key(egui::Key::Escape)]);
        assert!(actions.dialogs.cancel_portrait_dialog);
    }

    #[test]
    fn esc_and_enter_in_a_value_box_act_on_the_value_not_on_the_dialog() {
        let ctx = egui::Context::default();
        let mut data = UiData::default();
        data.dialogs.portrait_session = true;
        data.dialogs.portrait_ready = true;
        data.dialogs.portrait_faces = vec![true];
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(460.0, 860.0));
        let draw = |events: Vec<egui::Event>| {
            let mut actions = UiActions::default();
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events,
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| {
                ui.ctx().data_mut(|d| {
                    d.insert_temp(egui::Id::new(PRESETS_KEY), Vec::<Preset>::new());
                    d.insert_temp(egui::Id::new("portrait_group"), Some(Group::Skin));
                });
                portrait_dialog(ui.ctx(), &data, &mut actions);
            });
            (output, actions)
        };
        let smooth = |actions: &UiActions| {
            let (settings, ..) = actions.dialogs.set_portrait_preview.clone().unwrap();
            settings.smooth
        };
        for _ in 0..3 {
            draw(vec![]);
        }
        // "Làm mịn da" starts at 40: its value box is where that number is.
        let (output, actions) = draw(vec![]);
        assert_eq!(smooth(&actions), 40.0);
        let in_box = output
            .shapes
            .iter()
            .find_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) if text.galley.text() == "40" => {
                    Some(text.pos + egui::vec2(3.0, 5.0))
                }
                _ => None,
            })
            .expect("the value 40");
        let button = |pressed| egui::Event::PointerButton {
            pos: in_box,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let key = |key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let click = || {
            draw(vec![egui::Event::PointerMoved(in_box)]);
            draw(vec![button(true)]);
            draw(vec![button(false)])
        };

        // Typed, then given up with Esc: the value stays and so does the dialog.
        let (_, actions) = click();
        assert_eq!(smooth(&actions), 40.0, "the click dragged the slider");
        draw(vec![egui::Event::Text("77".to_string())]);
        let (_, actions) = draw(vec![key(egui::Key::Escape)]);
        assert!(!actions.dialogs.cancel_portrait_dialog);
        assert_eq!(smooth(&actions), 40.0);

        // Typed, then Enter: the value is taken and nothing is applied yet.
        click();
        draw(vec![egui::Event::Text("77".to_string())]);
        let (_, actions) = draw(vec![key(egui::Key::Enter)]);
        assert!(actions.dialogs.apply_portrait.is_none());
        assert_eq!(smooth(&actions), 77.0);

        // With no field in use, Esc closes the dialog as before.
        draw(vec![]);
        let (_, actions) = draw(vec![key(egui::Key::Escape)]);
        assert!(actions.dialogs.cancel_portrait_dialog);
    }

    #[test]
    fn the_preset_row_draws_and_keeps_everything_until_asked() {
        let ctx = egui::Context::default();
        let mut s = PortraitSettings::default();
        let mut presets = vec![Preset {
            name: "Nữ".to_string(),
            settings: s,
        }];
        let mut naming = Some("Nam".to_string());
        for _ in 0..2 {
            let _ = ctx.run_ui(Default::default(), |ui| {
                assert!(!preset_row(
                    ui,
                    true,
                    &mut s,
                    &mut presets,
                    &mut naming,
                    false
                ));
            });
        }
        assert_eq!(
            (presets.len(), naming.as_deref(), s),
            (1, Some("Nam"), PortraitSettings::default())
        );
        // Esc leaves the name field and keeps nothing.
        let _ = ctx.run_ui(Default::default(), |ui| {
            assert!(!preset_row(
                ui,
                true,
                &mut s,
                &mut presets,
                &mut naming,
                true
            ));
        });
        assert_eq!((presets.len(), naming), (1, None));
    }
}

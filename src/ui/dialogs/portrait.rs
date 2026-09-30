//! "Chỉnh chân dung" dialog (Image ▸ Chỉnh chân dung…): skin, blemish,
//! under-eye, eye and teeth sliders with a live canvas preview, like the
//! Filter/Levels dialogs. Áp dụng adds the retouch as a new layer; Hủy restores.

use super::*;
use crate::core::portrait::PortraitSettings;

fn slider_colors() -> [egui::Color32; 3] {
    [
        egui::Color32::from_rgb(52, 46, 44),
        egui::Color32::from_rgb(150, 118, 100),
        egui::Color32::from_rgb(246, 214, 190),
    ]
}

fn section_title(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(title)
            .strong()
            .size(12.0)
            .color(egui::Color32::from_rgb(210, 210, 210)),
    );
}

fn rows(ui: &mut egui::Ui, enabled: bool, items: [(&str, &mut f32, &str); 3]) {
    for (label, value, tip) in items {
        ui.add_enabled_ui(enabled, |ui| {
            crate::ui::widgets::dev_slider_stacked_resp(
                ui,
                label,
                value,
                0.0..=100.0,
                &slider_colors(),
                1.0,
            )
            .on_hover_text(tip);
        });
    }
}

pub(crate) fn portrait_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let settings_id = egui::Id::new("portrait_settings");
    let faces_id = egui::Id::new("portrait_faces");
    let preview_id = egui::Id::new("portrait_preview");
    let mut s: PortraitSettings = ctx.data_mut(|d| d.get_temp(settings_id).unwrap_or_default());
    let mut preview: bool = ctx.data_mut(|d| d.get_temp(preview_id).unwrap_or(true));
    let face_count = data.dialogs.portrait_faces.len();
    let mut faces: Vec<bool> = ctx.data_mut(|d| d.get_temp(faces_id).unwrap_or_default());
    faces.resize(face_count, true);

    let ready = data.dialogs.portrait_ready;
    let (enter_pressed, esc_pressed) = consume_dialog_enter_escape(ctx);
    let mut do_apply = enter_pressed && ready;
    let mut do_cancel = esc_pressed;
    let mut open = true;

    let default_pos = document_side_dialog_pos(ctx, data, 320.0, 96.0);
    egui::Window::new("Chỉnh chân dung")
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
                if !ready && !data.dialogs.portrait_status.starts_with("Không") {
                    ui.spinner();
                }
                ui.label(
                    egui::RichText::new(&data.dialogs.portrait_status)
                        .size(11.0)
                        .color(egui::Color32::from_gray(170)),
                );
            });
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

            section_title(ui, "Da");
            rows(
                ui,
                ready,
                [
                    (
                        "Làm mịn da",
                        &mut s.smooth,
                        "Mịn da nhưng giữ vân lỗ chân lông",
                    ),
                    ("Đều màu da", &mut s.even_tone, "Giảm mảng đỏ, loang màu"),
                    ("Giảm bóng dầu", &mut s.shine, "Dịu các vùng bóng loáng"),
                ],
            );
            rows(
                ui,
                ready,
                [
                    ("Sáng da", &mut s.brighten, "Da sáng hơn, giữ màu"),
                    ("Xóa mụn", &mut s.blemish, "Tự tìm và xóa mụn, đốm thâm nhỏ"),
                    ("Quầng thâm", &mut s.dark_circles, "Làm sáng vùng dưới mắt"),
                ],
            );
            section_title(ui, "Mắt & răng");
            rows(
                ui,
                ready,
                [
                    ("Trắng mắt", &mut s.eye_white, "Lòng trắng mắt sáng, bớt đỏ"),
                    ("Sáng tròng mắt", &mut s.iris, "Tròng mắt sáng và trong hơn"),
                    ("Trắng răng", &mut s.teeth, "Răng trắng, bớt ố vàng"),
                ],
            );

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.small_button("Mặc định").clicked() {
                    s = PortraitSettings::default();
                }
                if ui.small_button("Về 0").clicked() {
                    s = PortraitSettings::NEUTRAL;
                }
                ui.checkbox(&mut preview, "Xem trước")
                    .on_hover_text("Bỏ tích để xem ảnh gốc");
            });
            ui.add_space(6.0);
            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(ready, egui::Button::new("  Áp dụng  "))
                    .on_hover_text("Thêm kết quả thành layer mới \"Chân dung\"")
                    .clicked()
                {
                    do_apply = true;
                }
                if ui.button("Hủy").clicked() {
                    do_cancel = true;
                }
            });
        });

    if !open {
        do_cancel = true;
    }
    ctx.data_mut(|d| {
        d.insert_temp(settings_id, s);
        d.insert_temp(faces_id, faces.clone());
        d.insert_temp(preview_id, preview);
    });

    if do_apply {
        actions.dialogs.apply_portrait = Some((s, faces));
    } else if do_cancel {
        actions.dialogs.cancel_portrait_dialog = true;
    } else {
        actions.dialogs.set_portrait_preview = Some((s, faces, preview));
    }
}

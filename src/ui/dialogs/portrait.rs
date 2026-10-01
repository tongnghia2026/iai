//! "Chỉnh chân dung" dialog (Image ▸ Chỉnh chân dung…): skin, blemish,
//! under-eye, eye and teeth sliders with a live canvas preview, like the
//! Filter/Levels dialogs, and a brush ("Tô vùng") to fix the skin and hair
//! areas before sliding. Áp dụng adds the retouch as a new layer; Hủy restores.

use super::*;
use crate::core::portrait::brush::MaskTarget;
use crate::core::portrait::PortraitSettings;
use crate::core::selection::RefineBrushMode;
use egui_phosphor::regular as ph;

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

/// Slider rows: (label, value, tooltip, kind).
fn rows(ui: &mut egui::Ui, enabled: bool, items: Vec<(&str, &mut f32, &str, Kind)>) {
    for (label, value, tip, kind) in items {
        let (range, colours): (_, Vec<egui::Color32>) = match kind {
            Kind::Amount => (0.0..=100.0, slider_colors().to_vec()),
            Kind::TwoSided => (-100.0..=100.0, slider_colors().to_vec()),
            Kind::Hue => (0.0..=360.0, rainbow().to_vec()),
        };
        ui.add_enabled_ui(enabled, |ui| {
            crate::ui::widgets::dev_slider_stacked_resp(ui, label, value, range, &colours, 1.0)
                .on_hover_text(tip);
        });
    }
}

/// "Tô vùng": which area the brush paints, then its mode, size and hardness.
fn brush_section(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions, ready: bool) {
    let d = &data.dialogs;
    section_title(ui, "Tô vùng");
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
                }
            }
        });
        if d.portrait_brush.is_none() {
            return;
        }
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
        .on_hover_text("Đường kính cọ (px) — phím [ ]")
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
        .on_hover_text("0 = mép cọ mềm — Shift + [ ]")
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
    let mut s: PortraitSettings = ctx.data_mut(|d| d.get_temp(settings_id).unwrap_or_default());
    let mut preview: bool = ctx.data_mut(|d| d.get_temp(preview_id).unwrap_or(true));
    let mut masks: bool = ctx.data_mut(|d| d.get_temp(masks_id).unwrap_or(false));
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

            brush_section(ui, data, actions, ready);

            egui::ScrollArea::vertical()
                .max_height(ctx.content_rect().height() * 0.62)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    use Kind::*;
                    section_title(ui, "Da");
                    rows(
                        ui,
                        ready,
                        vec![
                            ("Làm mịn da", &mut s.smooth, "Mịn da nhưng giữ vân lỗ chân lông", Amount),
                            ("Đều màu da", &mut s.even_tone, "Giảm mảng đỏ, loang màu", Amount),
                            ("Giảm bóng dầu", &mut s.shine, "Dịu các vùng bóng loáng", Amount),
                            ("Sáng da", &mut s.brighten, "Da sáng hơn, giữ màu", Amount),
                            (
                                "Xóa mụn",
                                &mut s.blemish,
                                "Tự tìm và xóa mụn, đốm thâm nhỏ (lấp bằng vân da lành bên cạnh)",
                                Amount,
                            ),
                            ("Quầng thâm", &mut s.dark_circles, "Làm sáng vùng dưới mắt", Amount),
                            (
                                "Sống mũi cao",
                                &mut s.nose_bridge,
                                "Tạo khối: sáng dọc sống mũi, tối nhẹ hai bên",
                                Amount,
                            ),
                        ],
                    );
                    section_title(ui, "Mắt & răng");
                    rows(
                        ui,
                        ready,
                        vec![
                            ("Trắng mắt", &mut s.eye_white, "Lòng trắng mắt sáng, bớt đỏ", Amount),
                            ("Sáng tròng mắt", &mut s.iris, "Tròng mắt sáng và trong hơn", Amount),
                            (
                                "Màu tròng mắt",
                                &mut s.iris_hue,
                                "Chọn màu trên dải — cần kéo \"Phủ màu tròng\" để thấy",
                                Hue,
                            ),
                            ("Phủ màu tròng", &mut s.iris_tint, "0 = giữ màu mắt thật", Amount),
                            ("Trắng răng", &mut s.teeth, "Răng trắng, bớt ố vàng", Amount),
                        ],
                    );
                    section_title(ui, "Môi");
                    rows(
                        ui,
                        ready,
                        vec![
                            (
                                "Đậm môi",
                                &mut s.lip_saturation,
                                "Trái: môi nhạt màu — phải: môi đậm, tươi",
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
                        ],
                    );
                    section_title(ui, "Lông mày");
                    rows(
                        ui,
                        ready,
                        vec![
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
                        ],
                    );
                    section_title(ui, "Tóc");
                    if !data.dialogs.portrait_hair && ready {
                        ui.label(
                            egui::RichText::new("Không nhận ra tóc (cần model tách vùng Sapiens2).")
                                .size(10.0)
                                .color(egui::Color32::from_rgb(220, 150, 90)),
                        );
                    }
                    rows(
                        ui,
                        ready && data.dialogs.portrait_hair,
                        vec![
                            (
                                "Sáng tóc",
                                &mut s.hair_brightness,
                                "Trái: tóc tối hơn — phải: tóc sáng hơn (như thanh Blacks của Develop, giữ màu và vân tóc)",
                                TwoSided,
                            ),
                            (
                                "Màu tóc",
                                &mut s.hair_hue,
                                "Chọn màu nhuộm trên dải — cần kéo \"Phủ màu tóc\" để thấy",
                                Hue,
                            ),
                            ("Phủ màu tóc", &mut s.hair_tint, "0 = giữ màu tóc thật", Amount),
                        ],
                    );
                    section_title(ui, "Chi tiết");
                    rows(
                        ui,
                        ready,
                        vec![
                            (
                                "Tăng nét",
                                &mut s.sharpen,
                                "Mắt, mi, môi nét hơn (không đụng da và lông mày)",
                                Amount,
                            ),
                        ],
                    );
                });

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
            ui.add_enabled_ui(ready, |ui| {
                ui.checkbox(&mut masks, "Hiện vùng nhận diện").on_hover_text(
                    "Tô màu vùng app nhận ra: da đỏ, quầng mắt cam, lòng trắng xanh lá, tròng xanh dương, lông mày vàng, môi hồng, răng xanh ngọc, tóc tím",
                );
            });
            ui.add_space(6.0);
            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(ready, egui::Button::new("  Áp dụng  "))
                    .on_hover_text(if data.dialogs.portrait_reopened {
                        "Cập nhật layer \"Chân dung\" đang chỉnh tiếp"
                    } else {
                        "Thêm kết quả thành layer mới \"Chân dung\" (mở lại để chỉnh tiếp: chọn layer đó rồi vào Chỉnh chân dung)"
                    })
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
        d.insert_temp(masks_id, masks);
    });

    if do_apply {
        actions.dialogs.apply_portrait = Some((s, faces));
    } else if do_cancel {
        actions.dialogs.cancel_portrait_dialog = true;
    } else {
        actions.dialogs.set_portrait_preview = Some((s, faces, preview, masks));
    }
}

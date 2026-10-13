//! Batch "Định dạng chữ hàng loạt" dialog (Text ▸ Format All Text…): change the
//! font / colour / size / bold / italic / underline / case of every text layer
//! in the open document at once. Every control is opt-in — an untouched row is
//! left unchanged. Applied as a single undo step.

use super::*;
use crate::core::text::{TextCase, TextFontFamily};
use crate::ui::intent::TextBatchFormat;

fn tri_label(v: i8) -> &'static str {
    match v {
        1 => "Bật",
        2 => "Tắt",
        _ => "Không đổi",
    }
}

fn case_label(v: i8) -> &'static str {
    match v {
        1 => "HOA",
        2 => "thường",
        3 => "Mỗi Từ Hoa",
        _ => "Không đổi",
    }
}

pub(crate) fn text_format_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let from_id = egui::Id::new("txtfmt_from");
    let to_id = egui::Id::new("txtfmt_to");
    let to_search_id = egui::Id::new("txtfmt_to_search");
    let color_on_id = egui::Id::new("txtfmt_color_on");
    let color_id = egui::Id::new("txtfmt_color");
    let size_on_id = egui::Id::new("txtfmt_size_on");
    let size_id = egui::Id::new("txtfmt_size");
    let bold_id = egui::Id::new("txtfmt_bold");
    let italic_id = egui::Id::new("txtfmt_italic");
    let underline_id = egui::Id::new("txtfmt_underline");
    let case_id = egui::Id::new("txtfmt_case");

    // Live selections carried between frames in egui temp storage. "from" and
    // "to" are font storage names ("" = all / don't-change); the style rows use
    // a tri-state i8 (0 = keep, 1 = on, 2 = off) and case an i8 (0 = keep).
    let mut from_sel: String = ctx.data_mut(|d| d.get_temp::<String>(from_id).unwrap_or_default());
    let mut to_sel: String = ctx.data_mut(|d| d.get_temp::<String>(to_id).unwrap_or_default());
    let mut color_on: bool = ctx.data_mut(|d| d.get_temp::<bool>(color_on_id).unwrap_or(false));
    let mut color: egui::Color32 = ctx.data_mut(|d| {
        d.get_temp::<egui::Color32>(color_id)
            .unwrap_or(egui::Color32::BLACK)
    });
    let mut size_on: bool = ctx.data_mut(|d| d.get_temp::<bool>(size_on_id).unwrap_or(false));
    let mut size: f32 = ctx.data_mut(|d| d.get_temp::<f32>(size_id).unwrap_or(48.0));
    let mut bold: i8 = ctx.data_mut(|d| d.get_temp::<i8>(bold_id).unwrap_or(0));
    let mut italic: i8 = ctx.data_mut(|d| d.get_temp::<i8>(italic_id).unwrap_or(0));
    let mut underline: i8 = ctx.data_mut(|d| d.get_temp::<i8>(underline_id).unwrap_or(0));
    let mut case: i8 = ctx.data_mut(|d| d.get_temp::<i8>(case_id).unwrap_or(0));

    // A "from" font remembered from another document may not exist here.
    if !from_sel.is_empty()
        && !data
            .dialogs
            .text_fonts_in_use
            .iter()
            .any(|(s, _)| s == &from_sel)
    {
        from_sel.clear();
    }

    let (enter_pressed, esc_pressed) = consume_dialog_enter_escape(ctx);
    let mut do_apply = enter_pressed;
    let mut do_cancel = esc_pressed;

    modal_overlay(ctx, "text_format_overlay");

    let from_label = if from_sel.is_empty() {
        "Tất cả font".to_string()
    } else {
        data.dialogs
            .text_fonts_in_use
            .iter()
            .find(|(s, _)| s == &from_sel)
            .map(|(_, label)| label.clone())
            .unwrap_or_else(|| from_sel.clone())
    };
    let to_label = if to_sel.is_empty() {
        "— Không đổi —".to_string()
    } else {
        TextFontFamily::all()
            .iter()
            .find(|f| f.storage_name() == to_sel)
            .map(|f| f.name().to_string())
            .unwrap_or_else(|| to_sel.clone())
    };

    egui::Window::new("Định dạng chữ hàng loạt")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(DIALOG_ORDER)
        .min_width(360.0)
        .show(ctx, |ui| {
            ui.add_space(6.0);
            ui.label("Áp dụng cho toàn bộ chữ trong tài liệu (kể cả các trang). Mục nào để trống thì giữ nguyên.");
            ui.add_space(10.0);

            egui::Grid::new("txtfmt_grid")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    // ---- Font: from (scope) → to ----
                    ui.label("Đổi font — từ:");
                    egui::ComboBox::from_id_salt("txtfmt_from_combo")
                        .selected_text(from_label)
                        .width(220.0)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(from_sel.is_empty(), "Tất cả font")
                                .clicked()
                            {
                                from_sel.clear();
                            }
                            if !data.dialogs.text_fonts_in_use.is_empty() {
                                ui.separator();
                            }
                            for (storage, display) in &data.dialogs.text_fonts_in_use {
                                if ui.selectable_label(&from_sel == storage, display).clicked() {
                                    from_sel = storage.clone();
                                }
                            }
                        });
                    ui.end_row();

                    ui.label("            sang:");
                    egui::ComboBox::from_id_salt("txtfmt_to_combo")
                        .selected_text(to_label)
                        .width(220.0)
                        .height(440.0)
                        .show_ui(ui, |ui| {
                            ui.set_min_width(280.0);
                            if ui.selectable_label(to_sel.is_empty(), "— Không đổi —").clicked() {
                                to_sel.clear();
                                ui.data_mut(|d| d.remove::<String>(to_search_id));
                            }
                            ui.separator();
                            let had_search =
                                ui.data(|d| d.get_temp::<String>(to_search_id).is_some());
                            let mut search = ui
                                .data(|d| d.get_temp::<String>(to_search_id))
                                .unwrap_or_default();
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut search)
                                    .hint_text("Tìm font…")
                                    .desired_width(f32::INFINITY),
                            );
                            if !had_search {
                                resp.request_focus();
                            }
                            ui.data_mut(|d| d.insert_temp(to_search_id, search.clone()));
                            ui.separator();
                            let needle = search.trim().to_lowercase();
                            let mut shown = 0;
                            egui::ScrollArea::vertical()
                                .id_salt("txtfmt_to_list")
                                .max_height(360.0)
                                .show(ui, |ui| {
                                    for family in TextFontFamily::all() {
                                        if !needle.is_empty()
                                            && !family.name().to_lowercase().contains(&needle)
                                        {
                                            continue;
                                        }
                                        shown += 1;
                                        let storage = family.storage_name();
                                        if ui
                                            .selectable_label(to_sel == storage, family.name())
                                            .clicked()
                                        {
                                            to_sel = storage;
                                            ui.data_mut(|d| d.remove::<String>(to_search_id));
                                        }
                                    }
                                    if shown == 0 {
                                        ui.weak("Không có font khớp");
                                    }
                                });
                        });
                    ui.end_row();

                    // ---- Colour ----
                    ui.checkbox(&mut color_on, "Màu");
                    ui.add_enabled_ui(color_on, |ui| {
                        ui.color_edit_button_srgba(&mut color);
                    });
                    ui.end_row();

                    // ---- Size ----
                    ui.checkbox(&mut size_on, "Cỡ chữ (px)");
                    ui.add_enabled_ui(size_on, |ui| {
                        ui.add(
                            egui::DragValue::new(&mut size)
                                .range(
                                    crate::core::text::MIN_EDITABLE_FONT_PX
                                        ..=crate::core::text::MAX_EDITABLE_FONT_PX,
                                )
                                .speed(1.0),
                        );
                    });
                    ui.end_row();

                    // ---- Bold / Italic / Underline (tri-state) ----
                    ui.label("Đậm:");
                    egui::ComboBox::from_id_salt("txtfmt_bold_combo")
                        .selected_text(tri_label(bold))
                        .width(120.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut bold, 0, "Không đổi");
                            ui.selectable_value(&mut bold, 1, "Bật");
                            ui.selectable_value(&mut bold, 2, "Tắt");
                        });
                    ui.end_row();

                    ui.label("Nghiêng:");
                    egui::ComboBox::from_id_salt("txtfmt_italic_combo")
                        .selected_text(tri_label(italic))
                        .width(120.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut italic, 0, "Không đổi");
                            ui.selectable_value(&mut italic, 1, "Bật");
                            ui.selectable_value(&mut italic, 2, "Tắt");
                        });
                    ui.end_row();

                    ui.label("Gạch chân:");
                    egui::ComboBox::from_id_salt("txtfmt_underline_combo")
                        .selected_text(tri_label(underline))
                        .width(120.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut underline, 0, "Không đổi");
                            ui.selectable_value(&mut underline, 1, "Bật");
                            ui.selectable_value(&mut underline, 2, "Tắt");
                        });
                    ui.end_row();

                    // ---- Case ----
                    ui.label("Hoa/thường:");
                    egui::ComboBox::from_id_salt("txtfmt_case_combo")
                        .selected_text(case_label(case))
                        .width(150.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut case, 0, "Không đổi");
                            ui.selectable_value(&mut case, 1, "HOA");
                            ui.selectable_value(&mut case, 2, "thường");
                            ui.selectable_value(&mut case, 3, "Mỗi Từ Hoa");
                        });
                    ui.end_row();
                });

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("  Áp dụng  ").clicked() {
                    do_apply = true;
                }
                if ui.button("Hủy").clicked() {
                    do_cancel = true;
                }
            });
            ui.add_space(4.0);
        });

    // Carry the selections to the next frame.
    ctx.data_mut(|d| {
        d.insert_temp(from_id, from_sel.clone());
        d.insert_temp(to_id, to_sel.clone());
        d.insert_temp(color_on_id, color_on);
        d.insert_temp(color_id, color);
        d.insert_temp(size_on_id, size_on);
        d.insert_temp(size_id, size);
        d.insert_temp(bold_id, bold);
        d.insert_temp(italic_id, italic);
        d.insert_temp(underline_id, underline);
        d.insert_temp(case_id, case);
    });

    if do_apply {
        let tri = |v: i8| match v {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        };
        let fmt = TextBatchFormat {
            from_font: if from_sel.is_empty() {
                None
            } else {
                Some(from_sel)
            },
            set_font: if to_sel.is_empty() {
                None
            } else {
                Some(TextFontFamily::from_storage_name(&to_sel))
            },
            set_color: color_on.then(|| color.to_array()),
            set_size: size_on.then_some(size),
            set_bold: tri(bold),
            set_italic: tri(italic),
            set_underline: tri(underline),
            set_case: match case {
                1 => Some(TextCase::Upper),
                2 => Some(TextCase::Lower),
                3 => Some(TextCase::Title),
                _ => None,
            },
        };
        // Nothing chosen → just close; otherwise hand the batch to the app.
        if fmt.is_noop() {
            actions.dialogs.show_font_change_dialog = Some(false);
        } else {
            actions.dialogs.apply_text_format = Some(fmt);
        }
        ctx.data_mut(|d| d.remove::<String>(to_search_id));
    } else if do_cancel {
        actions.dialogs.show_font_change_dialog = Some(false);
        ctx.data_mut(|d| d.remove::<String>(to_search_id));
    }
}

//! Batch "Change font" dialog (Layer ▸ Change Font of All Text…): swap the font
//! of every text layer in the open document at once — either all text to one
//! font, or one specific font replaced by another. Applied as a single undo.

use super::*;
use crate::core::text::TextFontFamily;
use crate::ui::FontChangeRequest;

pub(crate) fn font_change_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let from_id = egui::Id::new("font_change_from");
    let to_id = egui::Id::new("font_change_to");
    let to_search_id = egui::Id::new("font_change_to_search");

    // Live selections, kept in egui temp storage between frames: "from" storage
    // name (empty = every font) and "to" storage name.
    let mut from_sel: String = ctx.data_mut(|d| d.get_temp::<String>(from_id).unwrap_or_default());
    let mut to_sel: String = ctx.data_mut(|d| {
        d.get_temp::<String>(to_id).unwrap_or_else(|| {
            TextFontFamily::all()
                .first()
                .map(|f| f.storage_name())
                .unwrap_or_else(|| TextFontFamily::SegoeUi.storage_name())
        })
    });
    // A "from" font remembered from another document may not exist here → fall
    // back to "all" so the label never shows a stale storage string.
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

    modal_overlay(ctx, "font_change_overlay");

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
    let to_label = TextFontFamily::all()
        .iter()
        .find(|f| f.storage_name() == to_sel)
        .map(|f| f.name().to_string())
        .unwrap_or_else(|| to_sel.clone());

    egui::Window::new("Đổi font hàng loạt")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(DIALOG_ORDER)
        .min_width(340.0)
        .show(ctx, |ui| {
            ui.add_space(6.0);
            ui.label("Đổi font cho toàn bộ chữ trong tài liệu đang mở (kể cả các trang).");
            ui.add_space(10.0);

            // ---- Source: all fonts, or one specific font in use ----
            ui.label("Đổi từ:");
            egui::ComboBox::from_id_salt("font_change_from_combo")
                .selected_text(from_label)
                .width(300.0)
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
            ui.add_space(8.0);

            // ---- Target: searchable list of every available font ----
            ui.label("Sang font:");
            egui::ComboBox::from_id_salt("font_change_to_combo")
                .selected_text(to_label)
                .width(300.0)
                .height(440.0)
                .show_ui(ui, |ui| {
                    ui.set_min_width(300.0);
                    let had_search = ui.data(|d| d.get_temp::<String>(to_search_id).is_some());
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
                        .id_salt("font_change_to_list")
                        .max_height(380.0)
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
    ctx.data_mut(|d| d.insert_temp(from_id, from_sel.clone()));
    ctx.data_mut(|d| d.insert_temp(to_id, to_sel.clone()));

    if do_apply && !to_sel.is_empty() {
        actions.dialogs.apply_font_change = Some(FontChangeRequest {
            from: if from_sel.is_empty() {
                None
            } else {
                Some(from_sel)
            },
            to: to_sel,
        });
        ctx.data_mut(|d| d.remove::<String>(to_search_id));
    } else if do_cancel {
        actions.dialogs.show_font_change_dialog = Some(false);
        ctx.data_mut(|d| d.remove::<String>(to_search_id));
    }
}

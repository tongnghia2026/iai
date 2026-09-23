//! Selection-modify dialogs: feather, modify (grow/shrink/...), stroke.

use super::*;

pub(crate) fn pdf_batch_scope_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let Some(operation) = data.dialogs.pdf_batch_operation else {
        return;
    };
    let Some(nav) = data.doc.pdf_nav.as_ref() else {
        actions.sel.cancel_pdf_batch = true;
        return;
    };
    let max_pages = nav.count.saturating_sub(nav.index).max(1);
    let mut page_count = data.dialogs.pdf_batch_page_count.clamp(1, max_pages);
    let (enter_pressed, esc_pressed) = consume_dialog_enter_escape(ctx);
    let mut apply = enter_pressed;
    let mut cancel = esc_pressed;
    let title = match operation {
        crate::ui::intent::PdfBatchOperation::Clear => "Xóa vùng hàng loạt",
        crate::ui::intent::PdfBatchOperation::Text => "Thêm văn bản hàng loạt",
    };

    modal_overlay(ctx, "pdf_batch_scope_dialog_overlay");
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .movable(true)
        .default_pos(document_side_dialog_pos(ctx, data, 390.0, 96.0))
        .default_width(390.0)
        .min_width(390.0)
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.label(
                egui::RichText::new(format!("Bắt đầu từ trang hiện tại: {}", nav.index + 1))
                    .strong(),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Số trang áp dụng:");
                let response = ui.add(
                    egui::DragValue::new(&mut page_count)
                        .range(1..=max_pages)
                        .speed(1.0),
                );
                ui.label(format!("/ {max_pages} trang còn lại"));
                if response.changed() {
                    actions.sel.set_pdf_batch_page_count = Some(page_count);
                }
            });
            let end_page = nav.index + page_count;
            ui.label(
                egui::RichText::new(format!(
                    "Phạm vi: trang {} đến trang {}. Các trang trước và sau phạm vi được giữ nguyên.",
                    nav.index + 1,
                    end_page
                ))
                .color(egui::Color32::GRAY)
                .size(11.0),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Áp dụng").clicked() {
                    apply = true;
                }
                if ui.button("Hủy").clicked() {
                    cancel = true;
                }
            });
        });

    if apply {
        actions.sel.set_pdf_batch_page_count = Some(page_count);
        actions.sel.apply_pdf_batch = true;
    } else if cancel {
        actions.sel.cancel_pdf_batch = true;
    }
}

pub(crate) fn feather_dialog(ctx: &egui::Context, _data: &UiData, actions: &mut UiActions) {
    let mut open = true;
    let (enter_pressed, esc_pressed) = consume_dialog_enter_escape(ctx);
    let mut do_feather = enter_pressed;
    let mut do_cancel = esc_pressed;

    let mut current_radius =
        ctx.data_mut(|d| *d.get_temp_mut_or_default::<f32>(egui::Id::new("feather_radius")));
    if current_radius == 0.0 {
        current_radius = 5.0;
    }

    modal_overlay(ctx, "feather_dialog_overlay");

    egui::Window::new("Feather Selection")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Feather Radius:");
                ui.add(egui::Slider::new(&mut current_radius, 0.0..=100.0).suffix(" px"));
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("OK").clicked() {
                    do_feather = true;
                }
                if ui.button("Cancel").clicked() {
                    do_cancel = true;
                }
            });
        });

    ctx.data_mut(|d| {
        d.insert_temp(egui::Id::new("feather_radius"), current_radius);
    });

    if do_feather {
        actions.sel.selection_feather = Some(current_radius);
        actions.sel.show_feather_dialog = Some(false);
    }
    if do_cancel || !open {
        actions.sel.show_feather_dialog = Some(false);
    }
}

/// Shared dialog for Select ▸ Modify ▸ Expand / Contract / Smooth / Border. The
/// active operation comes from `data.dialogs.show_modify_dialog`; the amount persists in
/// egui temp across re-opens.
pub(crate) fn modify_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    use crate::ui::SelectionModifyKind;
    let kind = match data.dialogs.show_modify_dialog {
        Some(k) => k,
        None => return,
    };
    let mut open = true;
    let (enter_pressed, esc_pressed) = consume_dialog_enter_escape(ctx);
    let mut apply = enter_pressed;
    let mut cancel = esc_pressed;

    let mut amount =
        ctx.data_mut(|d| *d.get_temp_mut_or_default::<f32>(egui::Id::new("modify_amount")));
    if amount == 0.0 {
        amount = 5.0;
    }

    modal_overlay(ctx, "modify_dialog_overlay");

    egui::Window::new(kind.title())
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            let label = match kind {
                SelectionModifyKind::Smooth => "Radius:",
                SelectionModifyKind::Border => "Width:",
                _ => "Amount:",
            };
            ui.horizontal(|ui| {
                ui.label(label);
                ui.add(egui::Slider::new(&mut amount, 1.0..=100.0).suffix(" px"));
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("OK").clicked() {
                    apply = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });

    ctx.data_mut(|d| d.insert_temp(egui::Id::new("modify_amount"), amount));

    if apply {
        match kind {
            SelectionModifyKind::Expand => actions.sel.selection_grow = Some(amount.round() as u32),
            SelectionModifyKind::Contract => {
                actions.sel.selection_shrink = Some(amount.round() as u32)
            }
            SelectionModifyKind::Smooth => actions.sel.selection_smooth = Some(amount),
            SelectionModifyKind::Border => {
                actions.sel.selection_border = Some(amount.round() as u32)
            }
        }
        actions.sel.close_modify_dialog = true;
    }
    if cancel || !open {
        actions.sel.close_modify_dialog = true;
    }
}

/// Select ▸ Color Range. A side-docked, non-modal window (like the paint-colour
/// dialog) so the canvas stays visible and clickable: clicking the image samples
/// the target colour. A Fuzziness slider widens the colour family and a
/// grayscale thumbnail previews the resulting selection (white = selected).
pub(crate) fn color_range_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let (enter_pressed, esc_pressed) = consume_dialog_enter_escape(ctx);
    let mut do_apply = enter_pressed && data.doc.has_doc;
    let mut do_cancel = esc_pressed;

    let mut open = true;
    let width = 300.0_f32;
    egui::Window::new("Color Range")
        .id(egui::Id::new("color_range_dialog"))
        .collapsible(false)
        .resizable(false)
        .movable(true)
        .order(DIALOG_ORDER)
        .open(&mut open)
        .default_pos(document_side_dialog_pos(ctx, data, width, 96.0))
        .default_width(width)
        .min_width(width)
        .show(ctx, |ui| {
            // Sampled colour read-out — a small chip, not a full picker panel.
            let c = data.dialogs.color_range_color;
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Bấm vào ảnh để lấy màu")
                        .color(egui::Color32::GRAY)
                        .size(11.0),
                );
                ui.add_space(6.0);
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(22.0, 16.0), egui::Sense::hover());
                ui.painter()
                    .rect_filled(rect, 2.0, egui::Color32::from_rgb(c[0], c[1], c[2]));
                ui.painter().rect_stroke(
                    rect,
                    2.0,
                    egui::Stroke::new(1.0_f32, egui::Color32::from_gray(120)),
                    egui::StrokeKind::Inside,
                );
                ui.label(
                    egui::RichText::new(format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2]))
                        .monospace()
                        .size(11.0)
                        .color(egui::Color32::GRAY),
                );
            });

            ui.add_space(8.0);
            let mut fuzz = data.dialogs.color_range_fuzziness as f32;
            if ui
                .add(egui::Slider::new(&mut fuzz, 0.0..=200.0).text("Fuzziness"))
                .changed()
            {
                actions.sel.set_color_range_fuzziness = Some(fuzz.round().clamp(0.0, 200.0) as u8);
            }

            ui.add_space(8.0);
            color_range_preview_ui(ui, data);

            ui.add_space(10.0);
            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        data.doc.has_doc,
                        egui::Button::new("OK").min_size(egui::vec2(72.0, 24.0)),
                    )
                    .clicked()
                {
                    do_apply = true;
                }
                if ui
                    .add(egui::Button::new("Cancel").min_size(egui::vec2(72.0, 24.0)))
                    .clicked()
                {
                    do_cancel = true;
                }
            });
        });

    if do_apply {
        actions.sel.apply_color_range = true;
        actions.sel.show_color_range_dialog = Some(false);
    } else if do_cancel || !open {
        actions.sel.show_color_range_dialog = Some(false);
    }
}

/// Grayscale thumbnail of the pending Color Range mask (white = selected). The
/// texture is cached and only re-uploaded when the mask image changes.
fn color_range_preview_ui(ui: &mut egui::Ui, data: &UiData) {
    let slot_size = egui::vec2(268.0, 168.0);
    let (slot, _) = ui.allocate_exact_size(slot_size, egui::Sense::hover());
    let painter = ui.painter_at(slot);
    painter.rect_filled(slot, 0.0, egui::Color32::from_gray(24));

    if let Some(image) = &data.dialogs.color_range_preview {
        let cache_id = egui::Id::new("color_range_preview_texture");
        let image_key = std::sync::Arc::as_ptr(image) as usize;
        let texture = ui
            .ctx()
            .data(|d| d.get_temp::<(usize, egui::TextureHandle)>(cache_id))
            .map(|(cached_key, mut texture)| {
                if cached_key != image_key {
                    texture.set((**image).clone(), egui::TextureOptions::LINEAR);
                    ui.ctx().data_mut(|store| {
                        store.insert_temp(cache_id, (image_key, texture.clone()))
                    });
                }
                texture
            })
            .unwrap_or_else(|| {
                let texture = ui.ctx().load_texture(
                    "color_range_preview",
                    (**image).clone(),
                    egui::TextureOptions::LINEAR,
                );
                ui.ctx()
                    .data_mut(|store| store.insert_temp(cache_id, (image_key, texture.clone())));
                texture
            });

        let iw = image.size[0].max(1) as f32;
        let ih = image.size[1].max(1) as f32;
        let scale = (slot.width() / iw).min(slot.height() / ih).min(1.0);
        let draw_size = egui::vec2(iw * scale, ih * scale);
        let rect = egui::Rect::from_center_size(slot.center(), draw_size);
        painter.image(
            texture.id(),
            rect,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    } else {
        painter.text(
            slot.center(),
            egui::Align2::CENTER_CENTER,
            "Xem trước vùng chọn",
            egui::FontId::proportional(12.0),
            egui::Color32::from_gray(150),
        );
    }

    painter.rect_stroke(
        slot,
        0.0,
        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(92)),
        egui::StrokeKind::Inside,
    );
}

pub(crate) fn stroke_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    use crate::core::canvas::{StrokeLocation, StrokeParams};

    let mut open = true;
    let (enter_pressed, esc_pressed) = consume_dialog_enter_escape(ctx);
    let mut do_apply = enter_pressed && data.sel.has_selection;
    let mut do_cancel = esc_pressed;

    let width_id = egui::Id::new("stroke_width");
    let loc_id = egui::Id::new("stroke_location");
    let op_id = egui::Id::new("stroke_opacity");
    let col_id = egui::Id::new("stroke_color");

    let mut width = ctx.data_mut(|d| *d.get_temp_mut_or_default::<f32>(width_id));
    if width < 1.0 {
        width = 3.0;
    }
    // Location index: 0 = Inside, 1 = Center, 2 = Outside. Inside keeps the
    // requested width visible when the selection touches the canvas edge.
    let mut loc_idx = ctx.data_mut(|d| d.get_temp::<u8>(loc_id)).unwrap_or(0u8);
    let mut opacity = ctx.data_mut(|d| *d.get_temp_mut_or_default::<f32>(op_id));
    if opacity <= 0.0 {
        opacity = 100.0;
    }
    // Stroke color seeds from the foreground color the first time the dialog opens.
    let fg = data.tool.brush_color;
    let mut color = ctx
        .data_mut(|d| d.get_temp::<egui::Color32>(col_id))
        .unwrap_or_else(|| egui::Color32::from_rgb(fg[0], fg[1], fg[2]));

    modal_overlay(ctx, "stroke_dialog_overlay");

    egui::Window::new("Stroke Selection")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .default_pos(document_side_dialog_pos(ctx, data, 340.0, 96.0))
        .default_width(340.0)
        .min_width(340.0)
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            dev_slider(ui, "Width", &mut width, 1.0..=250.0);
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_sized([84.0, 18.0], egui::Label::new("Location"));
                if ui
                    .add_sized(
                        [58.0, 22.0],
                        egui::Button::selectable(loc_idx == 0, "Inside"),
                    )
                    .clicked()
                {
                    loc_idx = 0;
                }
                if ui
                    .add_sized(
                        [58.0, 22.0],
                        egui::Button::selectable(loc_idx == 1, "Center"),
                    )
                    .clicked()
                {
                    loc_idx = 1;
                }
                if ui
                    .add_sized(
                        [62.0, 22.0],
                        egui::Button::selectable(loc_idx == 2, "Outside"),
                    )
                    .clicked()
                {
                    loc_idx = 2;
                }
            });
            ui.add_space(4.0);
            dev_slider(ui, "Opacity", &mut opacity, 0.0..=100.0);
            ui.add_space(8.0);
            ui.label("Color:");
            ui.scope(|ui| {
                ui.spacing_mut().slider_width = 270.0;
                crate::ui::color_picker::color_picker_compact(ui, &mut color);
            });

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(data.sel.has_selection, egui::Button::new("OK"))
                    .clicked()
                {
                    do_apply = true;
                }
                if ui.button("Cancel").clicked() {
                    do_cancel = true;
                }
            });
            if !data.sel.has_selection {
                ui.colored_label(
                    egui::Color32::from_rgb(200, 150, 60),
                    "No active selection to stroke.",
                );
            }
        });

    ctx.data_mut(|d| {
        d.insert_temp(width_id, width);
        d.insert_temp(loc_id, loc_idx);
        d.insert_temp(op_id, opacity);
        d.insert_temp(col_id, color);
    });

    if do_apply {
        let location = match loc_idx {
            0 => StrokeLocation::Inside,
            2 => StrokeLocation::Outside,
            _ => StrokeLocation::Center,
        };
        actions.sel.apply_stroke = Some(StrokeParams {
            color: [color.r(), color.g(), color.b(), 255],
            width: width.round().max(1.0) as u32,
            location,
            opacity: (opacity / 100.0).clamp(0.0, 1.0),
        });
        actions.sel.show_stroke_dialog = Some(false);
    }
    if do_cancel || !open {
        actions.sel.show_stroke_dialog = Some(false);
    }
}

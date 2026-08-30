//! The print dialog: preview, printer picker, settings panel.

use super::*;

pub(crate) fn print_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let enter_pressed = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
    let mut layout = data.print.print_layout;
    let mut changed = false;
    let mut do_print = false;
    let mut do_done = false;
    let mut do_cancel = false;
    let print_layout = print_layout_for_selected_printer(data, layout);
    let can_print = !data.print.print_refreshing
        && !data.print.print_settings_open
        && !data.print.print_printers.is_empty();
    let default_size = egui::vec2(980.0, 540.0);
    let screen = ctx.content_rect();
    let default_pos = egui::pos2(
        (screen.center().x - default_size.x * 0.5).max(screen.min.x + 12.0),
        (screen.center().y - default_size.y * 0.5).max(screen.min.y + 12.0),
    );

    egui::Window::new("Print Settings")
        .collapsible(false)
        .resizable(true)
        .movable(true)
        .default_pos(default_pos)
        .default_size(default_size)
        .min_size(egui::vec2(820.0, 460.0))
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.set_min_size(egui::vec2(800.0, 420.0));
            let main_height = (ui.available_height() - 42.0).max(360.0);
            let preview_size = egui::vec2(
                (ui.available_width() - 490.0).clamp(320.0, 520.0),
                main_height.clamp(360.0, 720.0),
            );
            ui.horizontal_top(|ui| {
                print_preview_panel(ui, data, &print_layout, preview_size);
                ui.add_space(10.0);
                print_settings_panel(ui, data, actions, &mut layout, &mut changed);
            });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(can_print, egui::Button::new("  Print  "))
                        .clicked()
                    {
                        do_print = true;
                    }
                    if ui.button("  Done  ").clicked() {
                        do_done = true;
                    }
                    if ui.button("  Cancel  ").clicked() {
                        do_cancel = true;
                    }
                });
            });
        });

    if enter_pressed && can_print {
        do_print = true;
    }
    if changed {
        actions.print.set_print_layout = Some(layout);
    }
    if do_print {
        actions.print.set_print_layout = Some(print_layout_for_selected_printer(data, layout));
        actions.print.print_send = true;
    }
    if do_done || do_cancel {
        actions.print.show_print_dialog = Some(false);
    }
}

pub(crate) fn print_layout_for_selected_printer(
    data: &UiData,
    layout: crate::core::print::PrintLayout,
) -> crate::core::print::PrintLayout {
    crate::core::print::layout_for_printer(
        layout,
        data.print.print_printers.as_ref(),
        &data.print.print_selected_printer,
    )
}

pub(crate) fn print_preview_panel(
    ui: &mut egui::Ui,
    data: &UiData,
    layout: &crate::core::print::PrintLayout,
    panel_size: egui::Vec2,
) {
    use crate::core::print::{document_page_points, page_points, placement, printable_area_points};

    let (panel, _) = ui.allocate_exact_size(panel_size, egui::Sense::hover());
    let painter = ui.painter_at(panel);
    painter.rect_filled(panel, 0.0, egui::Color32::from_gray(38));

    let (pw, ph) = page_points(
        layout,
        data.doc.canvas_w,
        data.doc.canvas_h,
        data.doc.canvas_dpi,
    );
    let max_w = panel.width() - 90.0;
    let max_h = panel.height() - 42.0;
    let s = (max_w / pw).min(max_h / ph).max(0.01);
    let page_size = egui::vec2(pw * s, ph * s);
    let page = egui::Rect::from_center_size(panel.center(), page_size);

    painter.rect_filled(page.expand(4.0), 0.0, egui::Color32::from_gray(18));
    painter.rect_filled(page, 0.0, egui::Color32::WHITE);
    painter.rect_stroke(
        page,
        0.0,
        egui::Stroke::new(1.0_f32, egui::Color32::BLACK),
        egui::StrokeKind::Inside,
    );

    let to_page_rect = |x: f32, y: f32, w: f32, h: f32| {
        egui::Rect::from_min_size(
            page.min + egui::vec2(x * s, (ph - (y + h)) * s),
            egui::vec2(w * s, h * s),
        )
    };
    let (print_x, print_y, print_w, print_h) = printable_area_points(
        layout,
        data.doc.canvas_w,
        data.doc.canvas_h,
        data.doc.canvas_dpi,
    );
    let printable = to_page_rect(print_x, print_y, print_w, print_h).intersect(page);

    if data.doc.canvas_w > 0 && data.doc.canvas_h > 0 {
        let (dw, dh, x, y) = placement(
            layout,
            data.doc.canvas_w,
            data.doc.canvas_h,
            data.doc.canvas_dpi,
        );
        let image = to_page_rect(x, y, dw, dh);
        // Everything past the printable area is hidden, exactly like on paper:
        // details that fall outside the red frame are lost in print.
        let page_painter = painter.with_clip_rect(printable);
        page_painter.rect_filled(image, 0.0, egui::Color32::WHITE);
        if let Some(preview) = &data.print.print_preview_image {
            let cache_id = egui::Id::new("print_preview_texture");
            let image_key = std::sync::Arc::as_ptr(preview) as usize;
            let texture = ui
                .ctx()
                .data(|data| data.get_temp::<(usize, egui::TextureHandle)>(cache_id))
                .map(|(cached_key, mut texture)| {
                    if cached_key != image_key {
                        texture.set((**preview).clone(), egui::TextureOptions::LINEAR);
                        ui.ctx().data_mut(|store| {
                            store.insert_temp(cache_id, (image_key, texture.clone()))
                        });
                    }
                    texture
                })
                .unwrap_or_else(|| {
                    let texture = ui.ctx().load_texture(
                        "print_preview",
                        (**preview).clone(),
                        egui::TextureOptions::LINEAR,
                    );
                    ui.ctx().data_mut(|store| {
                        store.insert_temp(cache_id, (image_key, texture.clone()))
                    });
                    texture
                });

            page_painter.image(
                texture.id(),
                image,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        } else {
            page_painter.rect_filled(image, 0.0, egui::Color32::from_rgb(214, 226, 238));
        }
    }

    // Red frame on top of the image: the printer's reachable area.
    painter.rect_stroke(
        printable,
        0.0,
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(220, 60, 60)),
        egui::StrokeKind::Inside,
    );

    // Crop / registration marks around the artwork, matching the exported PDF.
    if layout.marks.is_active() && data.doc.canvas_w > 0 && data.doc.canvas_h > 0 {
        let (dw, dh, ax, ay) = placement(
            layout,
            data.doc.canvas_w,
            data.doc.canvas_h,
            data.doc.canvas_dpi,
        );
        let to_screen = |px: f32, py: f32| page.min + egui::vec2(px * s, (ph - py) * s);
        let (trx0, try0, trx1, try1) =
            crate::core::print::marks_trim_box(ax, ay, dw, dh, layout.marks.bleed_mm);
        let stroke = egui::Stroke::new(1.0_f32, egui::Color32::from_gray(60));
        let seg = |x1: f32, y1: f32, x2: f32, y2: f32| {
            painter.line_segment([to_screen(x1, y1), to_screen(x2, y2)], stroke);
        };
        // The trim (cut) rectangle.
        painter.rect_stroke(
            egui::Rect::from_two_pos(to_screen(trx0, try0), to_screen(trx1, try1)),
            0.0,
            stroke,
            egui::StrokeKind::Inside,
        );
        let (l, r, b, t) = (ax, ax + dw, ay, ay + dh);
        let (gap, clen) = (3.0, 12.0);
        if layout.marks.crop_marks {
            seg(l - gap - clen, try0, l - gap, try0);
            seg(l - gap - clen, try1, l - gap, try1);
            seg(r + gap, try0, r + gap + clen, try0);
            seg(r + gap, try1, r + gap + clen, try1);
            seg(trx0, b - gap - clen, trx0, b - gap);
            seg(trx1, b - gap - clen, trx1, b - gap);
            seg(trx0, t + gap, trx0, t + gap + clen);
            seg(trx1, t + gap, trx1, t + gap + clen);
        }
        if layout.marks.registration_marks {
            let (rr, reg_off, cross) = (4.0_f32, 10.0, 7.0);
            for (cx, cy) in [
                (ax + dw * 0.5, b - reg_off),
                (ax + dw * 0.5, t + reg_off),
                (l - reg_off, ay + dh * 0.5),
                (r + reg_off, ay + dh * 0.5),
            ] {
                painter.circle_stroke(to_screen(cx, cy), rr * s, stroke);
                seg(cx - cross, cy, cx + cross, cy);
                seg(cx, cy - cross, cx, cy + cross);
            }
        }
    }

    let (iw, ih) = document_page_points(data.doc.canvas_w, data.doc.canvas_h, data.doc.canvas_dpi);
    let size_label = format!(
        "Paper {:.3} x {:.3} in   Printable {:.3} x {:.3} in   Image {:.3} x {:.3} in",
        pw / 72.0,
        ph / 72.0,
        print_w / 72.0,
        print_h / 72.0,
        iw / 72.0,
        ih / 72.0
    );
    painter.text(
        egui::pos2(page.center().x, (page.top() - 16.0).max(panel.top() + 4.0)),
        egui::Align2::CENTER_CENTER,
        size_label,
        egui::TextStyle::Small.resolve(ui.style()),
        egui::Color32::from_gray(170),
    );
}

pub(crate) fn printer_picker(
    ui: &mut egui::Ui,
    data: &UiData,
    actions: &mut UiActions,
    selected: String,
) {
    const PICKER_W: f32 = 320.0;
    const PICKER_ROW_H: f32 = 26.0;

    let picker_id = egui::Id::new("print_printer_picker_open");
    let mut open = ui
        .ctx()
        .data_mut(|d| d.get_temp::<bool>(picker_id).unwrap_or(false));
    let (picker_rect, response) = ui.allocate_exact_size(
        egui::vec2(PICKER_W, ui.spacing().interact_size.y),
        egui::Sense::click(),
    );
    let visuals = ui.style().interact(&response);
    ui.painter().rect(
        picker_rect,
        visuals.corner_radius,
        visuals.weak_bg_fill,
        visuals.bg_stroke,
        egui::StrokeKind::Inside,
    );
    let text_rect = egui::Rect::from_min_max(
        picker_rect.min + egui::vec2(8.0, 0.0),
        picker_rect.max - egui::vec2(20.0, 0.0),
    );
    let text_painter = ui.painter().with_clip_rect(text_rect);
    text_painter.text(
        egui::pos2(text_rect.left(), picker_rect.center().y),
        egui::Align2::LEFT_CENTER,
        selected,
        egui::TextStyle::Button.resolve(ui.style()),
        visuals.text_color(),
    );
    ui.painter().text(
        egui::pos2(picker_rect.right() - 8.0, picker_rect.center().y),
        egui::Align2::RIGHT_CENTER,
        "v",
        egui::TextStyle::Button.resolve(ui.style()),
        visuals.text_color(),
    );
    if response.clicked() {
        open = !open;
    }
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        open = false;
    }

    if open {
        let screen = ui.ctx().content_rect();
        let content_h = data.print.print_printers.len() as f32 * 28.0 + 10.0;
        let max_h = (screen.height() - 96.0).clamp(420.0, 900.0);
        let popup_h = content_h.min(max_h).max(220.0);
        let popup_w = response
            .rect
            .width()
            .min((screen.width() - 24.0).max(240.0));
        let x = response
            .rect
            .left()
            .min(screen.max.x - popup_w - 12.0)
            .max(12.0);
        let y = (response.rect.bottom() + 2.0)
            .min(screen.max.y - popup_h - 12.0)
            .max(screen.min.y + 48.0);

        egui::Area::new(picker_id.with("area"))
            .order(DIALOG_ORDER)
            .fixed_pos(egui::pos2(x, y))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(popup_w);
                    ui.set_max_width(popup_w);
                    egui::ScrollArea::vertical()
                        .max_height(popup_h)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for printer in data.print.print_printers.iter() {
                                let selected = data.print.print_selected_printer == printer.name;
                                let label = if printer.is_default {
                                    format!("{} (Default)", printer.name)
                                } else {
                                    printer.name.clone()
                                };
                                let (row_rect, row_response) = ui.allocate_exact_size(
                                    egui::vec2((popup_w - 12.0).max(200.0), PICKER_ROW_H),
                                    egui::Sense::click(),
                                );
                                let row_visuals =
                                    ui.style().interact_selectable(&row_response, selected);
                                if selected || row_response.hovered() || row_response.has_focus() {
                                    ui.painter().rect_filled(
                                        row_rect,
                                        row_visuals.corner_radius,
                                        row_visuals.weak_bg_fill,
                                    );
                                }
                                let label_rect = row_rect.shrink2(egui::vec2(6.0, 0.0));
                                ui.painter().with_clip_rect(label_rect).text(
                                    egui::pos2(label_rect.left(), row_rect.center().y),
                                    egui::Align2::LEFT_CENTER,
                                    label,
                                    egui::TextStyle::Button.resolve(ui.style()),
                                    row_visuals.text_color(),
                                );
                                if row_response.clicked() {
                                    actions.print.set_print_printer = Some(printer.name.clone());
                                    open = false;
                                }
                            }
                        });
                });
            });
    }

    ui.ctx().data_mut(|d| d.insert_temp(picker_id, open));
}

pub(crate) fn print_printable_mode_label(
    page: (f32, f32),
    rect: (f32, f32, f32, f32),
) -> &'static str {
    let (pw, ph) = page;
    let (x, y, w, h) = rect;
    let edge_tol = 3.0;
    let size_tol = 6.0;
    if x <= edge_tol && y <= edge_tol && (pw - w).abs() <= size_tol && (ph - h).abs() <= size_tol {
        "Borderless"
    } else {
        "Normal margins"
    }
}

pub(crate) fn print_settings_panel(
    ui: &mut egui::Ui,
    data: &UiData,
    actions: &mut UiActions,
    layout: &mut crate::core::print::PrintLayout,
    changed: &mut bool,
) {
    use crate::core::print::RenderIntent;

    ui.vertical(|ui| {
        ui.set_width(470.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label(egui::RichText::new("Printer Setup").strong());
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Printer:");
                let selected = if data.print.print_refreshing && data.print.print_printers.is_empty() {
                    "Loading printers...".to_string()
                } else if data.print.print_selected_printer.is_empty() {
                    "No printer".to_string()
                } else {
                    data.print.print_selected_printer.clone()
                };
                ui.add_enabled_ui(!data.print.print_settings_open, |ui| {
                    printer_picker(ui, data, actions, selected);
                });
                if ui
                    .add_enabled(
                        !data.print.print_refreshing && !data.print.print_settings_open,
                        egui::Button::new("Refresh"),
                    )
                    .clicked()
                {
                    actions.print.refresh_printers = true;
                }
            });
            if data.print.print_refreshing {
                ui.label(
                    egui::RichText::new("Reading printer paper settings...")
                        .color(egui::Color32::GRAY)
                        .size(10.0),
                );
            } else if !data.print.print_printers.is_empty() {
                let (paper_w, paper_h) = crate::core::print::selected_printer_page_points(
                    data.print.print_printers.as_ref(),
                    &data.print.print_selected_printer,
                );
                let printable = crate::core::print::selected_printer_printable_rect(
                    data.print.print_printers.as_ref(),
                    &data.print.print_selected_printer,
                );
                ui.label(
                    egui::RichText::new(format!(
                        "Paper: {:.3} x {:.3} in",
                        paper_w / 72.0,
                        paper_h / 72.0
                    ))
                    .color(egui::Color32::GRAY)
                    .size(10.0),
                );
                if let Some(rect) = printable {
                    let mode = print_printable_mode_label((paper_w, paper_h), rect);
                    ui.label(
                        egui::RichText::new(format!(
                            "Printable: {:.3} x {:.3} in ({mode})",
                            rect.2 / 72.0,
                            rect.3 / 72.0
                        ))
                        .color(egui::Color32::GRAY)
                        .size(10.0),
                    );
                }
            }
            ui.horizontal(|ui| {
                ui.label("Copies:");
                let mut copies = data.print.print_copies.max(1) as i32;
                if ui
                    .add(egui::DragValue::new(&mut copies).range(1..=999).speed(1.0))
                    .changed()
                {
                    actions.print.set_print_copies = Some(copies.clamp(1, 999) as u32);
                }
                if ui
                    .add_enabled(
                        !data.print.print_refreshing && !data.print.print_settings_open,
                        egui::Button::new(if data.print.print_settings_open {
                            "Printer Settings Open..."
                        } else {
                            "Print Settings..."
                        }),
                    )
                    .clicked()
                {
                    actions.print.open_printer_settings = true;
                }
            });
        });

        ui.add_space(8.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            egui::CollapsingHeader::new("Color Management")
                .default_open(true)
                .show(ui, |ui| {
                    ui.label(format!("Document Profile: {}", data.doc.doc_profile_name));
                    ui.horizontal(|ui| {
                        ui.label("Color Handling:");
                        let printer_managed = data.print.print_printer_profile_name.is_empty();
                        let handling = if printer_managed {
                            "Printer Manages Colors".to_string()
                        } else {
                            format!("Convert: {}", data.print.print_printer_profile_name)
                        };
                        egui::ComboBox::from_id_salt("print_color_handling")
                            .width(230.0)
                            .selected_text(handling)
                            .show_ui(ui, |ui| {
                                if ui
                                    .selectable_label(printer_managed, "Printer Manages Colors")
                                    .clicked()
                                {
                                    actions.print.clear_print_printer_profile = true;
                                }
                                if ui
                                    .selectable_label(
                                        !printer_managed,
                                        "Convert to Printer Profile…",
                                    )
                                    .clicked()
                                {
                                    actions.print.load_print_printer_profile = true;
                                }
                            });
                    });
                    // The intent only affects the app-managed conversion, so it
                    // stays hidden while the printer manages colors.
                    if !data.print.print_printer_profile_name.is_empty() {
                        ui.horizontal(|ui| {
                            ui.label("Rendering Intent:");
                            egui::ComboBox::from_id_salt("print_rendering_intent")
                                .selected_text(layout.intent.label())
                                .show_ui(ui, |ui| {
                                    for it in RenderIntent::all() {
                                        if ui
                                            .selectable_value(&mut layout.intent, *it, it.label())
                                            .changed()
                                        {
                                            *changed = true;
                                        }
                                    }
                                });
                        });
                        ui.label(
                            egui::RichText::new(
                                "App converts to the printer profile — set the printer driver to 'No Color Management'.",
                            )
                            .color(egui::Color32::GRAY)
                            .size(10.0),
                        );
                    }
                });
        });

        ui.add_space(8.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            egui::CollapsingHeader::new("Print Marks")
                .default_open(false)
                .show(ui, |ui| {
                    if ui
                        .checkbox(&mut layout.marks.crop_marks, "Crop marks")
                        .changed()
                    {
                        *changed = true;
                    }
                    if ui
                        .checkbox(&mut layout.marks.registration_marks, "Registration marks")
                        .changed()
                    {
                        *changed = true;
                    }
                    ui.horizontal(|ui| {
                        ui.label("Bleed:");
                        let mut bleed = layout.marks.bleed_mm;
                        if ui
                            .add(
                                egui::DragValue::new(&mut bleed)
                                    .range(0.0..=20.0)
                                    .speed(0.1)
                                    .suffix(" mm"),
                            )
                            .changed()
                        {
                            layout.marks.bleed_mm = bleed.clamp(0.0, 20.0);
                            *changed = true;
                        }
                    });
                    ui.label(
                        egui::RichText::new(
                            "Applies to Save as PDF. The document is the bleed area; the trim (cut) \
                             line is inset by the bleed. Choose a paper larger than the artwork so \
                             the marks have room.",
                        )
                        .color(egui::Color32::GRAY)
                        .size(10.0),
                    );
                });
        });
    });
}

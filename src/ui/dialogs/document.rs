//! Document geometry/file dialogs: new canvas, resize, image size, rename,
//! export, CMYK conversion.

use super::*;

/// Image ▸ Mode ▸ CMYK Color…: pick the conversion space (built-in naive GCR or
/// a browsed ICC device profile) and confirm the destructive parts.
pub(crate) fn cmyk_convert_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        actions.doc.show_cmyk_convert_dialog = Some(false);
    }
    modal_overlay(ctx, "cmyk_convert_dialog_overlay");
    egui::Window::new("Convert to CMYK")
        .order(DIALOG_ORDER)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, -40.0])
        .show(ctx, |ui| {
            ui.set_width(380.0);
            ui.label("Conversion space:");
            ui.add_space(4.0);
            if ui
                .radio(!data.dialogs.cmyk_convert_use_icc, "Generic CMYK (built-in)")
                .clicked()
            {
                actions.doc.set_cmyk_convert_use_icc = Some(false);
            }
            ui.indent("cmyk_naive_note", |ui| {
                ui.small("Use this when you do not have a CMYK profile from your printer. It is predictable for editing, but not a certified press profile.");
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui
                    .radio(data.dialogs.cmyk_convert_use_icc, "ICC profile:")
                    .clicked()
                {
                    actions.doc.set_cmyk_convert_use_icc = Some(true);
                }
                match &data.dialogs.cmyk_convert_icc_name {
                    Some(name) => {
                        ui.label(egui::RichText::new(name).strong());
                    }
                    None => {
                        ui.weak("(none selected)");
                    }
                }
                if ui.button(format!("{} Browse…", ph::FOLDER_OPEN)).clicked() {
                    actions.doc.browse_cmyk_icc = true;
                }
            });
            ui.indent("cmyk_icc_note", |ui| {
                ui.small("Choose this only if your print shop gave you a CMYK .icc or .icm profile.");
            });
            ui.add_space(8.0);
            ui.separator();
            ui.label(
                egui::RichText::new(
                    "Converting to CMYK will flatten all layers, convert the document to 8-bit, and clear the undo history.",
                )
                .color(ui.visuals().warn_fg_color),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let ready = !data.dialogs.cmyk_convert_use_icc || data.dialogs.cmyk_convert_icc_name.is_some();
                if ui
                    .add_enabled(ready, egui::Button::new("Convert"))
                    .clicked()
                {
                    actions.doc.convert_cmyk = true;
                }
                if ui.button("Cancel").clicked() {
                    actions.doc.show_cmyk_convert_dialog = Some(false);
                }
            });
        });
}

pub(crate) fn new_canvas_pixels(value: f32, unit: crate::core::units::Unit, dpi: f32) -> u32 {
    crate::core::units::to_pixels(value, unit, dpi, 0.0)
        .round()
        .max(1.0) as u32
}

pub(crate) fn new_canvas_dimension_drag<'a>(
    value: &'a mut f32,
    parsed_unit: &'a std::cell::Cell<Option<crate::core::units::Unit>>,
) -> egui::DragValue<'a> {
    egui::DragValue::new(value)
        .range(1.0..=100000.0)
        .speed(1.0)
        .custom_parser(move |text| {
            crate::core::units::parse_dimension(text).map(|(value, unit)| {
                if unit.is_some() {
                    parsed_unit.set(unit);
                }
                value as f64
            })
        })
}

pub(crate) fn new_canvas_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let mut new_name = data.dialogs.new_name.clone();
    let mut new_dpi = data.dialogs.new_dpi;
    let mut new_unit = data.dialogs.new_unit;
    // These are the user's source values, not values reconstructed from the
    // rounded integer pixel size. Reconstructing every frame turns 10 cm at
    // 72 DPI into 283 px and then back into 9.98 cm.
    let mut new_w_disp = data.dialogs.new_w_input;
    let mut new_h_disp = data.dialogs.new_h_input;

    let mut new_bg_color = data.dialogs.new_bg_color;
    // RGB vs CMYK is only consulted when the canvas is created, so it lives in the
    // dialog's transient egui store rather than in persistent app state.
    let mut cmyk_mode = ctx.data_mut(|d| {
        d.get_temp::<bool>(egui::Id::new("nd_cmyk"))
            .unwrap_or(false)
    });
    let mut do_create = false;
    let mut do_cancel = false;

    modal_overlay(ctx, "new_canvas_dialog_overlay");

    egui::Window::new("New Canvas")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .min_width(360.0)
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.add_space(8.0);

            egui::Grid::new("new_canvas_grid")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Name:");
                    if ui.text_edit_singleline(&mut new_name).changed() {
                        actions.doc.new_name = Some(new_name.clone());
                    }
                    ui.end_row();

                    ui.label("Width:");
                    let (w_response, unit_w_response) = ui
                        .horizontal(|ui| {
                            let typed_unit = std::cell::Cell::new(None);
                            let response = ui
                                .add(new_canvas_dimension_drag(&mut new_w_disp, &typed_unit))
                                .on_hover_text(
                                    "You can type a unit, for example: 10 cm, 15mm, 8 in",
                                );
                            if let Some(unit) = typed_unit.get() {
                                new_unit = unit;
                                actions.doc.new_unit = Some(unit);
                            }
                            if response.changed() || typed_unit.get().is_some() {
                                actions.doc.new_w_input = Some(new_w_disp);
                            }

                            // Unit before the combo runs, so a change can convert
                            // the typed W/H to the newly picked unit.
                            let old_unit = new_unit;
                            let unit_response = egui::ComboBox::from_id_salt("unit_select_w")
                                .selected_text(new_unit.name())
                                .width(60.0)
                                .show_ui(ui, |ui| {
                                    for unit in crate::core::units::Unit::all() {
                                        if ui
                                            .selectable_value(&mut new_unit, unit, unit.name())
                                            .changed()
                                        {
                                            // Convert W/H through pixels so the real
                                            // size is preserved when the unit changes
                                            // (e.g. 2480 px @ 300 dpi → 21.00 cm),
                                            // instead of keeping the raw number.
                                            use crate::core::units::{from_pixels, to_pixels};
                                            let w_px =
                                                to_pixels(new_w_disp, old_unit, new_dpi, 0.0);
                                            let h_px =
                                                to_pixels(new_h_disp, old_unit, new_dpi, 0.0);
                                            new_w_disp = from_pixels(w_px, unit, new_dpi, 0.0);
                                            new_h_disp = from_pixels(h_px, unit, new_dpi, 0.0);
                                            actions.doc.new_w_input = Some(new_w_disp);
                                            actions.doc.new_h_input = Some(new_h_disp);
                                            actions.doc.new_unit = Some(unit);
                                        }
                                    }
                                })
                                .response;
                            (response, unit_response)
                        })
                        .inner;
                    ui.end_row();

                    ui.label("Height:");
                    let typed_unit = std::cell::Cell::new(None);
                    let h_response = ui
                        .add(new_canvas_dimension_drag(&mut new_h_disp, &typed_unit))
                        .on_hover_text("You can type a unit, for example: 10 cm, 15mm, 8 in");
                    if let Some(unit) = typed_unit.get() {
                        new_unit = unit;
                        actions.doc.new_unit = Some(unit);
                    }
                    if h_response.changed() || typed_unit.get().is_some() {
                        actions.doc.new_h_input = Some(new_h_disp);
                    }
                    ui.end_row();

                    ui.label("Resolution:");
                    let res_response = ui.add(
                        egui::DragValue::new(&mut new_dpi)
                            .range(1.0..=1200.0)
                            .suffix(" DPI")
                            .speed(1.0),
                    );
                    if res_response.changed() {
                        actions.doc.new_dpi = Some(new_dpi);
                    }
                    ui.end_row();

                    // Tab / Shift+Tab cycle Width → Height → Resolution (skipping
                    // the unit picker), each field selected-all so typing replaces
                    // the value — matching the Crop options bar. Runs here while all
                    // three field responses are in scope.
                    let (tab, shift) = ui
                        .ctx()
                        .input(|i| (i.key_pressed(egui::Key::Tab), i.modifiers.shift));
                    if tab {
                        if !shift && (w_response.has_focus() || unit_w_response.has_focus()) {
                            crate::ui::widgets::focus_field_select_all(ui, &h_response);
                        } else if !shift && h_response.has_focus() {
                            crate::ui::widgets::focus_field_select_all(ui, &res_response);
                        } else if !shift && res_response.has_focus() {
                            crate::ui::widgets::focus_field_select_all(ui, &w_response);
                        } else if shift && res_response.has_focus() {
                            crate::ui::widgets::focus_field_select_all(ui, &h_response);
                        } else if shift && h_response.has_focus() {
                            crate::ui::widgets::focus_field_select_all(ui, &w_response);
                        } else if shift && (w_response.has_focus() || unit_w_response.has_focus()) {
                            crate::ui::widgets::focus_field_select_all(ui, &res_response);
                        }
                    }

                    ui.label("Background:");
                    egui::ComboBox::from_id_salt("bg_color")
                        .selected_text(match new_bg_color {
                            0 => "White",
                            1 => "Black",
                            _ => "Transparent",
                        })
                        .show_ui(ui, |ui| {
                            if ui.selectable_value(&mut new_bg_color, 0, "White").changed() {
                                actions.doc.new_bg_color = Some(0);
                            }
                            if ui.selectable_value(&mut new_bg_color, 1, "Black").changed() {
                                actions.doc.new_bg_color = Some(1);
                            }
                            if ui
                                .selectable_value(&mut new_bg_color, 2, "Transparent")
                                .changed()
                            {
                                actions.doc.new_bg_color = Some(2);
                            }
                        });
                    ui.end_row();

                    ui.label("Color mode:");
                    egui::ComboBox::from_id_salt("color_mode")
                        .selected_text(if cmyk_mode {
                            "CMYK (for print)"
                        } else {
                            "RGB (for screen)"
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut cmyk_mode, false, "RGB (for screen)");
                            ui.selectable_value(&mut cmyk_mode, true, "CMYK (for print)");
                        });
                    ctx.data_mut(|d| d.insert_temp(egui::Id::new("nd_cmyk"), cmyk_mode));
                    ui.end_row();

                    ui.label("Quick:");
                    egui::ComboBox::from_id_salt("preset_select")
                        .selected_text("Built-in presets...")
                        .show_ui(ui, |ui| {
                            let presets: &[(&str, u32, u32, f32)] = &[
                                ("800x600", 800, 600, 72.0),
                                ("1280x720", 1280, 720, 72.0),
                                ("1920x1080", 1920, 1080, 72.0),
                                ("2560x1440", 2560, 1440, 72.0),
                                ("4K", 3840, 2160, 72.0),
                                ("A4 72dpi", 595, 842, 72.0),
                                ("A4 150dpi", 1240, 1754, 150.0),
                                ("A4 300dpi", 2480, 3508, 300.0),
                                ("Square 1K", 1024, 1024, 72.0),
                                ("Instagram", 1080, 1080, 72.0),
                            ];
                            for (name, w, h, dpi) in presets {
                                if ui.selectable_label(false, *name).clicked() {
                                    new_w_disp = *w as f32;
                                    new_h_disp = *h as f32;
                                    new_dpi = *dpi;
                                    new_unit = crate::core::units::Unit::Pixels;
                                    actions.doc.new_w_input = Some(new_w_disp);
                                    actions.doc.new_h_input = Some(new_h_disp);
                                    actions.doc.new_dpi = Some(*dpi);
                                    actions.doc.new_unit = Some(new_unit);
                                }
                            }
                        });
                    ui.end_row();

                    ui.label("My Presets:");
                    ui.horizontal(|ui| {
                        let mut apply_idx: Option<usize> = None;
                        let mut del_idx: Option<usize> = None;
                        egui::ComboBox::from_id_salt("user_preset_select")
                            .selected_text("Saved presets...")
                            .width(160.0)
                            .show_ui(ui, |ui| {
                                for (i, preset) in data.dialogs.user_presets.iter().enumerate() {
                                    ui.horizontal(|ui| {
                                        if ui.selectable_label(false, &preset.name).clicked() {
                                            apply_idx = Some(i);
                                        }
                                        if ui
                                            .small_button(ph::X)
                                            .on_hover_text("Delete preset")
                                            .clicked()
                                        {
                                            del_idx = Some(i);
                                        }
                                    });
                                }
                            });
                        if let Some(i) = apply_idx {
                            let p = &data.dialogs.user_presets[i];
                            new_w_disp = p.width;
                            new_h_disp = p.height;
                            new_dpi = p.dpi;
                            new_unit = p.unit_enum();
                            actions.doc.new_unit = Some(new_unit);
                            actions.doc.new_dpi = Some(p.dpi);
                            actions.doc.new_w_input = Some(new_w_disp);
                            actions.doc.new_h_input = Some(new_h_disp);
                        }
                        if let Some(i) = del_idx {
                            actions.dialogs.delete_preset = Some(i);
                        }

                        let mut preset_name = ctx.data_mut(|d| {
                            d.get_temp::<String>(egui::Id::new("nd_preset_name"))
                                .unwrap_or_default()
                        });
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut preset_name)
                                .desired_width(100.0)
                                .hint_text("Preset name…"),
                        );
                        if resp.changed() {
                            ctx.data_mut(|d| {
                                d.insert_temp(egui::Id::new("nd_preset_name"), preset_name.clone())
                            });
                        }
                        if ui.button("Save").clicked() && !preset_name.trim().is_empty() {
                            let unit_str = new_unit.name().to_string();
                            actions.dialogs.save_preset = Some((
                                preset_name.trim().to_string(),
                                new_w_disp,
                                new_h_disp,
                                unit_str,
                                new_dpi,
                            ));
                            ctx.data_mut(|d| {
                                d.insert_temp(egui::Id::new("nd_preset_name"), String::new())
                            });
                        }
                    });
                    ui.end_row();
                });

            ui.add_space(12.0);

            let preview_w = new_canvas_pixels(new_w_disp, new_unit, new_dpi);
            let preview_h = new_canvas_pixels(new_h_disp, new_unit, new_dpi);
            let size_mb = (preview_w as f64 * preview_h as f64 * 4.0) / (1024.0 * 1024.0);
            ui.label(
                egui::RichText::new(format!(
                    "Memory: {:.1} MB  |  {} x {} px",
                    size_mb, preview_w, preview_h,
                ))
                .color(egui::Color32::GRAY)
                .size(11.0),
            );

            ui.add_space(12.0);

            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                do_create = true;
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                do_cancel = true;
            }

            ui.horizontal(|ui| {
                if ui.button("  Create  ").clicked() {
                    do_create = true;
                }
                if ui.button("  Cancel  ").clicked() {
                    do_cancel = true;
                }
            });
            ui.add_space(4.0);
        });

    if do_create {
        actions.doc.new_canvas_confirmed = Some((
            new_name,
            new_canvas_pixels(new_w_disp, new_unit, new_dpi),
            new_canvas_pixels(new_h_disp, new_unit, new_dpi),
            new_dpi,
            new_bg_color,
            new_unit,
            cmyk_mode,
        ));
        actions.dialogs.show_new_dialog = Some(false);
    }
    if do_cancel {
        actions.dialogs.show_new_dialog = Some(false);
    }
}

pub(crate) fn resize_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let mut new_w_px = ctx.data_mut(|d| {
        d.get_temp::<f32>(egui::Id::new("res_w"))
            .unwrap_or(data.doc.canvas_w as f32)
    });
    let mut new_h_px = ctx.data_mut(|d| {
        d.get_temp::<f32>(egui::Id::new("res_h"))
            .unwrap_or(data.doc.canvas_h as f32)
    });
    let mut anchor_x = ctx.data_mut(|d| d.get_temp::<i32>(egui::Id::new("res_ax")).unwrap_or(0i32));
    let mut anchor_y = ctx.data_mut(|d| d.get_temp::<i32>(egui::Id::new("res_ay")).unwrap_or(0i32));
    let mut res_unit = ctx.data_mut(|d| {
        d.get_temp::<crate::core::units::Unit>(egui::Id::new("res_unit"))
            .unwrap_or(crate::core::units::Unit::Pixels)
    });
    let mut res_dpi = ctx.data_mut(|d| {
        d.get_temp::<f32>(egui::Id::new("res_dpi"))
            .unwrap_or(data.doc.canvas_dpi)
    });
    let mut do_resize = false;
    let mut do_cancel = false;

    let mut w_disp = crate::core::units::from_pixels(new_w_px, res_unit, res_dpi, 0.0);
    let mut h_disp = crate::core::units::from_pixels(new_h_px, res_unit, res_dpi, 0.0);

    modal_overlay(ctx, "resize_dialog_overlay");

    egui::Window::new("Canvas Size")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .min_width(340.0)
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.add_space(8.0);

            let cur_w_disp =
                crate::core::units::from_pixels(data.doc.canvas_w as f32, res_unit, res_dpi, 0.0);
            let cur_h_disp =
                crate::core::units::from_pixels(data.doc.canvas_h as f32, res_unit, res_dpi, 0.0);
            let fmt = |v: f32| crate::core::units::format_value(v, res_unit);
            ui.label(
                egui::RichText::new(format!(
                    "Current: {}×{} {}  ({}×{} px)",
                    fmt(cur_w_disp),
                    fmt(cur_h_disp),
                    res_unit.name(),
                    data.doc.canvas_w,
                    data.doc.canvas_h
                ))
                .color(egui::Color32::GRAY)
                .size(11.0),
            );
            ui.add_space(6.0);

            egui::Grid::new("resize_grid")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Width:");
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::DragValue::new(&mut w_disp)
                                    .range(1.0..=100000.0)
                                    .speed(1.0),
                            )
                            .changed()
                        {
                            new_w_px =
                                crate::core::units::to_pixels(w_disp, res_unit, res_dpi, 0.0)
                                    .max(1.0);
                        }
                        egui::ComboBox::from_id_salt("res_unit_sel")
                            .selected_text(res_unit.name())
                            .width(60.0)
                            .show_ui(ui, |ui| {
                                for unit in crate::core::units::Unit::all() {
                                    if unit == crate::core::units::Unit::Percent {
                                        continue;
                                    }
                                    ui.selectable_value(&mut res_unit, unit, unit.name());
                                }
                            });
                    });
                    ui.end_row();

                    ui.label("Height:");
                    if ui
                        .add(
                            egui::DragValue::new(&mut h_disp)
                                .range(1.0..=100000.0)
                                .speed(1.0),
                        )
                        .changed()
                    {
                        new_h_px =
                            crate::core::units::to_pixels(h_disp, res_unit, res_dpi, 0.0).max(1.0);
                    }
                    ui.end_row();

                    ui.label("Resolution:");
                    if ui
                        .add(
                            egui::DragValue::new(&mut res_dpi)
                                .range(1.0..=1200.0)
                                .suffix(" DPI")
                                .speed(1.0),
                        )
                        .changed()
                    {}
                    ui.end_row();

                    ui.label("Offset X:");
                    ui.add(egui::DragValue::new(&mut anchor_x).suffix(" px").speed(1.0));
                    ui.end_row();

                    ui.label("Offset Y:");
                    ui.add(egui::DragValue::new(&mut anchor_y).suffix(" px").speed(1.0));
                    ui.end_row();
                });

            let size_mb = (new_w_px as f64 * new_h_px as f64 * 4.0) / (1024.0 * 1024.0);
            ui.label(
                egui::RichText::new(format!(
                    "New size: {} × {} px  |  Memory: {:.1} MB",
                    new_w_px as u32, new_h_px as u32, size_mb
                ))
                .color(egui::Color32::GRAY)
                .size(11.0),
            );

            ctx.data_mut(|d| {
                d.insert_temp(egui::Id::new("res_w"), new_w_px);
                d.insert_temp(egui::Id::new("res_h"), new_h_px);
                d.insert_temp(egui::Id::new("res_ax"), anchor_x);
                d.insert_temp(egui::Id::new("res_ay"), anchor_y);
                d.insert_temp(egui::Id::new("res_unit"), res_unit);
                d.insert_temp(egui::Id::new("res_dpi"), res_dpi);
            });

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("  Apply  ").clicked() {
                    do_resize = true;
                }
                if ui.button("  Cancel  ").clicked() {
                    do_cancel = true;
                }
            });
            ui.add_space(4.0);
        });

    if do_resize {
        actions.doc.canvas_resize = Some((new_w_px as u32, new_h_px as u32, anchor_x, anchor_y));
        actions.dialogs.show_resize_dialog = Some(false);
    }
    if do_cancel {
        actions.dialogs.show_resize_dialog = Some(false);
    }
}

pub(crate) fn image_size_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let mut new_w_px = ctx.data_mut(|d| {
        d.get_temp::<f32>(egui::Id::new("img_size_w"))
            .unwrap_or(data.doc.canvas_w as f32)
    });
    let mut new_h_px = ctx.data_mut(|d| {
        d.get_temp::<f32>(egui::Id::new("img_size_h"))
            .unwrap_or(data.doc.canvas_h as f32)
    });
    let mut res_unit = ctx.data_mut(|d| {
        d.get_temp::<crate::core::units::Unit>(egui::Id::new("img_size_unit"))
            .unwrap_or(crate::core::units::Unit::Pixels)
    });
    let mut res_dpi = ctx.data_mut(|d| {
        d.get_temp::<f32>(egui::Id::new("img_size_dpi"))
            .unwrap_or(data.doc.canvas_dpi)
    });
    let mut constrain = ctx.data_mut(|d| {
        d.get_temp::<bool>(egui::Id::new("img_size_constrain"))
            .unwrap_or(true)
    });
    if res_unit == crate::core::units::Unit::Percent {
        res_unit = crate::core::units::Unit::Pixels;
    }
    let mut do_resize = false;
    let mut do_cancel = false;

    let aspect = data.doc.canvas_w.max(1) as f32 / data.doc.canvas_h.max(1) as f32;
    let mut w_disp = crate::core::units::from_pixels(new_w_px, res_unit, res_dpi, 0.0);
    let mut h_disp = crate::core::units::from_pixels(new_h_px, res_unit, res_dpi, 0.0);

    modal_overlay(ctx, "image_size_dialog_overlay");

    egui::Window::new("Image Size")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .min_width(340.0)
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!(
                    "Current: {}×{} px, {:.0} DPI",
                    data.doc.canvas_w, data.doc.canvas_h, data.doc.canvas_dpi
                ))
                .color(egui::Color32::GRAY)
                .size(11.0),
            );
            ui.add_space(6.0);

            egui::Grid::new("image_size_grid")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Width:");
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::DragValue::new(&mut w_disp)
                                    .range(1.0..=100000.0)
                                    .speed(1.0),
                            )
                            .changed()
                        {
                            new_w_px =
                                crate::core::units::to_pixels(w_disp, res_unit, res_dpi, 0.0)
                                    .max(1.0);
                            if constrain {
                                new_h_px = (new_w_px / aspect).max(1.0);
                                h_disp = crate::core::units::from_pixels(
                                    new_h_px, res_unit, res_dpi, 0.0,
                                );
                            }
                        }
                        egui::ComboBox::from_id_salt("image_size_unit_sel")
                            .selected_text(res_unit.name())
                            .width(60.0)
                            .show_ui(ui, |ui| {
                                for unit in crate::core::units::Unit::all() {
                                    if unit == crate::core::units::Unit::Percent {
                                        continue;
                                    }
                                    ui.selectable_value(&mut res_unit, unit, unit.name());
                                }
                            });
                    });
                    ui.end_row();

                    ui.label("Height:");
                    if ui
                        .add(
                            egui::DragValue::new(&mut h_disp)
                                .range(1.0..=100000.0)
                                .speed(1.0),
                        )
                        .changed()
                    {
                        new_h_px =
                            crate::core::units::to_pixels(h_disp, res_unit, res_dpi, 0.0).max(1.0);
                        if constrain {
                            new_w_px = (new_h_px * aspect).max(1.0);
                            w_disp =
                                crate::core::units::from_pixels(new_w_px, res_unit, res_dpi, 0.0);
                        }
                    }
                    ui.end_row();

                    ui.label("Resolution:");
                    if ui
                        .add(
                            egui::DragValue::new(&mut res_dpi)
                                .range(1.0..=1200.0)
                                .suffix(" DPI")
                                .speed(1.0),
                        )
                        .changed()
                    {
                        res_dpi = res_dpi.max(1.0);
                        if res_unit != crate::core::units::Unit::Pixels {
                            new_w_px =
                                crate::core::units::to_pixels(w_disp, res_unit, res_dpi, 0.0)
                                    .max(1.0);
                            new_h_px =
                                crate::core::units::to_pixels(h_disp, res_unit, res_dpi, 0.0)
                                    .max(1.0);
                        }
                    }
                    ui.end_row();
                });

            ui.checkbox(&mut constrain, "Constrain proportions");

            let size_mb = (new_w_px as f64 * new_h_px as f64 * 4.0) / (1024.0 * 1024.0);
            ui.label(
                egui::RichText::new(format!(
                    "New image: {} × {} px  |  Memory: {:.1} MB",
                    new_w_px.round() as u32,
                    new_h_px.round() as u32,
                    size_mb
                ))
                .color(egui::Color32::GRAY)
                .size(11.0),
            );

            ctx.data_mut(|d| {
                d.insert_temp(egui::Id::new("img_size_w"), new_w_px);
                d.insert_temp(egui::Id::new("img_size_h"), new_h_px);
                d.insert_temp(egui::Id::new("img_size_unit"), res_unit);
                d.insert_temp(egui::Id::new("img_size_dpi"), res_dpi);
                d.insert_temp(egui::Id::new("img_size_constrain"), constrain);
            });

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("  Apply  ").clicked() {
                    do_resize = true;
                }
                if ui.button("  Cancel  ").clicked() {
                    do_cancel = true;
                }
            });
            ui.add_space(4.0);
        });

    if do_resize {
        actions.doc.image_resize = Some((
            new_w_px.round().max(1.0) as u32,
            new_h_px.round().max(1.0) as u32,
            res_dpi.max(1.0),
        ));
        actions.dialogs.show_image_size_dialog = Some(false);
    }
    if do_cancel {
        actions.dialogs.show_image_size_dialog = Some(false);
    }
    if do_resize || do_cancel {
        // Drop the in-session field values so the next open re-seeds from the
        // current image. Without this the temp store keeps the last numbers and
        // the dialog shows stale dimensions for a different/just-resized image.
        ctx.data_mut(|d| {
            d.remove::<f32>(egui::Id::new("img_size_w"));
            d.remove::<f32>(egui::Id::new("img_size_h"));
            d.remove::<f32>(egui::Id::new("img_size_dpi"));
        });
    }
}

pub(crate) fn rename_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let mut text = data.dialogs.rename_text.clone();
    let idx = data.dialogs.rename_idx;
    let mut do_rename = false;
    let mut do_cancel = false;

    modal_overlay(ctx, "rename_dialog_overlay");

    egui::Window::new("Rename Layer")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.add_space(8.0);
            ui.label("Layer name:");
            let resp = ui.add(
                egui::TextEdit::singleline(&mut text)
                    .desired_width(200.0)
                    .hint_text("Enter layer name..."),
            );
            resp.request_focus();

            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                do_rename = true;
            }

            if resp.changed() {
                actions.doc.rename_text = Some(text.clone());
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("  OK  ").clicked() {
                    do_rename = true;
                }
                if ui.button("Cancel").clicked() {
                    do_cancel = true;
                }
            });
            ui.add_space(4.0);
        });

    if do_rename && !text.is_empty() {
        actions.doc.rename_confirmed = Some((idx, text));
        actions.dialogs.show_rename_dialog = Some((false, idx));
    }
    if do_cancel {
        actions.dialogs.show_rename_dialog = Some((false, idx));
    }
}

pub(crate) fn export_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let (enter_pressed, esc_pressed) = consume_dialog_enter_escape(ctx);
    let mut do_export = enter_pressed;
    let mut do_cancel = esc_pressed;
    let jpeg_quality = ctx.data_mut(|d| d.get_temp::<u8>(egui::Id::new("exp_jq")).unwrap_or(90u8));
    let webp_quality = ctx.data_mut(|d| d.get_temp::<u8>(egui::Id::new("exp_wq")).unwrap_or(90u8));
    let webp_lossless =
        ctx.data_mut(|d| d.get_temp::<bool>(egui::Id::new("exp_wl")).unwrap_or(false));
    let png_compression =
        ctx.data_mut(|d| d.get_temp::<u8>(egui::Id::new("exp_pc")).unwrap_or(6u8));

    modal_overlay(ctx, "export_dialog_overlay");

    egui::Window::new("Export Image")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .min_width(300.0)
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.add_space(8.0);

            ui.label("Format:");
            ui.add_space(4.0);

            ui.horizontal_wrapped(|ui| {
                for fmt in ExportFormat::all_export() {
                    let is_selected = std::mem::discriminant(&data.dialogs.export_format)
                        == std::mem::discriminant(&fmt);
                    if ui.selectable_label(is_selected, fmt.name()).clicked() {
                        actions.doc.export = Some(fmt);
                    }
                }
            });

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);

            match &data.dialogs.export_format {
                ExportFormat::Jpeg { quality } => {
                    let mut q = *quality;
                    ui.label("Quality:");
                    if ui
                        .add(egui::Slider::new(&mut q, 1..=100).suffix("%"))
                        .changed()
                    {
                        actions.doc.export = Some(ExportFormat::Jpeg { quality: q });
                    }
                }
                ExportFormat::Webp { quality, lossless } => {
                    let mut q = *quality;
                    let mut l = *lossless;
                    if ui.checkbox(&mut l, "Lossless").changed() {
                        actions.doc.export = Some(ExportFormat::Webp {
                            quality: q,
                            lossless: l,
                        });
                    }
                    if !l {
                        ui.label("Quality:");
                        if ui
                            .add(egui::Slider::new(&mut q, 1..=100).suffix("%"))
                            .changed()
                        {
                            actions.doc.export = Some(ExportFormat::Webp {
                                quality: q,
                                lossless: l,
                            });
                        }
                    }
                }
                ExportFormat::Png { compression } => {
                    let mut c = *compression;
                    ui.label("Compression:");
                    if ui.add(egui::Slider::new(&mut c, 0..=9)).changed() {
                        actions.doc.export = Some(ExportFormat::Png { compression: c });
                    }
                    ui.label(
                        egui::RichText::new("0=Fast, 9=Small file")
                            .color(egui::Color32::GRAY)
                            .size(10.0),
                    );
                }
                ExportFormat::Iai => {
                    ui.label(
                        egui::RichText::new("iAi format preserves all layers, masks and metadata.")
                            .color(egui::Color32::GRAY)
                            .size(11.0),
                    );
                }
                _ => {}
            }

            ctx.data_mut(|d| {
                d.insert_temp(egui::Id::new("exp_jq"), jpeg_quality);
                d.insert_temp(egui::Id::new("exp_wq"), webp_quality);
                d.insert_temp(egui::Id::new("exp_wl"), webp_lossless);
                d.insert_temp(egui::Id::new("exp_pc"), png_compression);
            });

            // ICC embedding is supported for PNG / JPEG / TIFF.
            let supports_icc = matches!(
                data.dialogs.export_format,
                ExportFormat::Png { .. } | ExportFormat::Jpeg { .. } | ExportFormat::Tiff
            );
            if supports_icc {
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                let mut embed = data.doc.export_embed_icc;
                if ui
                    .checkbox(&mut embed, "Embed Color Profile (ICC)")
                    .changed()
                {
                    actions.doc.set_export_embed_icc = Some(embed);
                }
                ui.label(
                    egui::RichText::new(format!("Profile: {}", data.doc.doc_profile_name))
                        .color(egui::Color32::GRAY)
                        .size(10.0),
                );
            }

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("  Export...  ").clicked() {
                    do_export = true;
                }
                if ui.button("  Cancel  ").clicked() {
                    do_cancel = true;
                }
            });
            ui.add_space(4.0);
        });

    if do_export {
        actions.doc.request_export_path = Some(data.dialogs.export_format.clone());
        actions.dialogs.show_export_dialog = Some(false);
    }
    if do_cancel {
        actions.dialogs.show_export_dialog = Some(false);
    }
}

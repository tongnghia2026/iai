//! App/session dialogs: preferences, exit/close confirmations, reload
//! prompt, PDF import.

use super::*;

pub(crate) fn preferences_dialog(ctx: &egui::Context, _data: &UiData, actions: &mut UiActions) {
    let mut do_close = false;

    modal_overlay(ctx, "preferences_dialog_overlay");

    egui::Window::new("Preferences")
        .collapsible(false)
        .resizable(true)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .min_width(400.0)
        .min_height(300.0)
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.add_space(8.0);

            egui::CollapsingHeader::new("Performance")
                .default_open(true)
                .show(ui, |ui| {
                    ui.label("Undo history: 100 steps");
                    ui.label("GPU: Auto");
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("More performance settings coming soon")
                            .color(egui::Color32::GRAY)
                            .size(11.0),
                    );
                });

            egui::CollapsingHeader::new("Appearance")
                .default_open(true)
                .show(ui, |ui| {
                    ui.label("Theme: Dark");
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("More appearance settings coming soon")
                            .color(egui::Color32::GRAY)
                            .size(11.0),
                    );
                });

            egui::CollapsingHeader::new("Shortcuts")
                .default_open(false)
                .show(ui, |ui| {
                    let shortcuts = [
                        ("Brush", "B"),
                        ("Eraser", "E"),
                        ("Move", "V"),
                        ("Eyedropper", "I"),
                        ("Fill", "G"),
                        ("Crop", "C"),
                        ("Zoom", "Z"),
                        ("Hand", "H"),
                        ("Undo", "Ctrl+Z"),
                        ("Redo", "Ctrl+Shift+Z"),
                        ("Save", "Ctrl+S"),
                        ("Open", "Ctrl+O"),
                        ("New", "Ctrl+N"),
                        ("Close", "Ctrl+W"),
                        ("Fit Screen", "Ctrl+0"),
                        ("Zoom 100%", "Ctrl+1"),
                        ("Levels", "Ctrl+L"),
                        ("Auto Levels", "Ctrl+Shift+L"),
                        ("Color Balance", "Ctrl+B"),
                        ("Hue/Saturation", "Ctrl+U"),
                        ("Desaturate", "Ctrl+Shift+U"),
                        ("Invert", "Ctrl+I"),
                        ("Free Transform", "Ctrl+T"),
                        ("Layer via Copy", "Ctrl+J"),
                        ("Smart Fill", "Shift+F5"),
                        ("Rulers", "Ctrl+R"),
                        ("Swap Colors", "X"),
                        ("Brush Size -", "["),
                        ("Brush Size +", "]"),
                    ];

                    egui::Grid::new("shortcuts_grid")
                        .num_columns(2)
                        .striped(true)
                        .spacing([20.0, 4.0])
                        .show(ui, |ui| {
                            for (action, key) in &shortcuts {
                                ui.label(*action);
                                ui.label(
                                    egui::RichText::new(*key)
                                        .monospace()
                                        .color(egui::Color32::from_rgb(180, 180, 255)),
                                );
                                ui.end_row();
                            }
                        });
                });

            ui.add_space(12.0);
            if ui.button("  Close  ").clicked() {
                do_close = true;
            }
            ui.add_space(4.0);
        });

    if do_close {
        actions.dialogs.show_preferences = Some(false);
    }
}

pub(crate) fn exit_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let (enter_pressed, esc_pressed) = consume_dialog_enter_escape(ctx);
    let mut do_save_exit = enter_pressed;
    let mut do_exit_no_save = false;
    let mut do_cancel = esc_pressed;

    modal_overlay(ctx, "exit_dialog_overlay");

    egui::Window::new("Unsaved Changes")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.add_space(8.0);
            let title = data
                .doc
                .doc_titles
                .get(data.doc.active_doc_idx)
                .map(String::as_str)
                .unwrap_or("Untitled");
            ui.label(format!("Save changes to “{title}” before exiting?"));
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if ui.button("Save & Exit").clicked() {
                    do_save_exit = true;
                }
                if ui.button("Exit Without Saving").clicked() {
                    do_exit_no_save = true;
                }
                if ui.button("Cancel").clicked() {
                    do_cancel = true;
                }
            });
        });

    if do_save_exit {
        actions.doc.exit_save_current = true;
    }
    if do_exit_no_save {
        actions.doc.exit_discard_current = true;
    }
    if do_cancel {
        actions.doc.exit_cancel = true;
    }
}

pub(crate) fn close_dialog(ctx: &egui::Context, _data: &UiData, actions: &mut UiActions) {
    let (enter_pressed, esc_pressed) = consume_dialog_enter_escape(ctx);
    let mut do_save_close = enter_pressed;
    let mut do_close_no_save = false;
    let mut do_cancel = esc_pressed;

    modal_overlay(ctx, "close_dialog_overlay");

    egui::Window::new("Unsaved Changes (Close File)")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.add_space(8.0);
            ui.label("You have unsaved changes. Do you want to save before closing?");
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if ui.button("Save & Close").clicked() {
                    do_save_close = true;
                }
                if ui.button("Close Without Saving").clicked() {
                    do_close_no_save = true;
                }
                if ui.button("Cancel").clicked() {
                    do_cancel = true;
                }
            });
        });

    if do_save_close {
        actions.doc.save_project = true;
        actions.doc.close_file_without_saving = Some(true);
        actions.dialogs.show_close_dialog = Some(false);
    }
    if do_close_no_save {
        actions.doc.close_file_without_saving = Some(true);
        actions.dialogs.show_close_dialog = Some(false);
    }
    if do_cancel {
        actions.dialogs.show_close_dialog = Some(false);
    }
}

pub(crate) fn reload_file_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let mut do_reload = false;
    let mut do_keep = false;

    modal_overlay(ctx, "reload_file_dialog_overlay");

    egui::Window::new("File changed on disk")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.add_space(8.0);
            ui.label(format!(
                "{} da duoc cap nhat ben ngoai. Ban co muon cap nhat tab dang mo tu file nay khong?",
                data.dialogs.reload_file_name
            ));
            if data.dialogs.reload_will_discard_changes {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("Canh bao: reload se ghi de nhung thay doi chua luu trong iAi.")
                        .color(egui::Color32::from_rgb(220, 170, 80)),
                );
            }
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if ui.button("Cap nhat tu file").clicked() {
                    do_reload = true;
                }
                if ui.button("Giu ban dang mo").clicked() {
                    do_keep = true;
                }
            });
        });

    if do_reload {
        actions.doc.reload_open_file_confirm = true;
    }
    if do_keep {
        actions.doc.reload_open_file_cancel = true;
    }
}

/// Parse a page-range string like `1-3,5,8-10` into a 1-based selection mask.
/// Tokens that don't parse or fall outside `1..=count` are ignored. Returns
/// `None` when nothing valid was selected (so a stray keystroke doesn't wipe the
/// current selection).
pub(crate) fn parse_page_ranges(text: &str, count: usize) -> Option<Vec<bool>> {
    let mut selection = vec![false; count];
    let mut any = false;
    let mark = |page: usize, selection: &mut [bool], any: &mut bool| {
        if page >= 1 && page <= count {
            selection[page - 1] = true;
            *any = true;
        }
    };
    for token in text.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        if let Some((a, b)) = token.split_once('-') {
            if let (Ok(a), Ok(b)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>()) {
                let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                for page in lo..=hi {
                    mark(page, &mut selection, &mut any);
                }
            }
        } else if let Ok(page) = token.parse::<usize>() {
            mark(page, &mut selection, &mut any);
        }
    }
    any.then_some(selection)
}

/// Photoshop-style page picker shown when opening a PDF. Lists each page with a
/// checkbox + size, a Select All / Deselect toggle, and a page-range field. The
/// per-PDF selection lives in egui temp state (keyed by path) so it survives
/// across frames; confirming sends the chosen 0-based indices back to the app.
pub(crate) fn pdf_import_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let count = data.dialogs.pdf_import_page_count;
    if count == 0 {
        return;
    }
    let sel_id = egui::Id::new(("pdf_import_sel", data.dialogs.pdf_import_path_key.as_str()));
    let range_id = egui::Id::new((
        "pdf_import_range",
        data.dialogs.pdf_import_path_key.as_str(),
    ));
    let dpi_id = egui::Id::new(("pdf_import_dpi", data.dialogs.pdf_import_path_key.as_str()));

    // Default: every page selected.
    let mut selection: Vec<bool> = ctx
        .data_mut(|d| d.get_temp::<Vec<bool>>(sel_id))
        .filter(|s| s.len() == count)
        .unwrap_or_else(|| vec![true; count]);
    let mut range_text: String = ctx
        .data_mut(|d| d.get_temp::<String>(range_id))
        .unwrap_or_default();
    // Import resolution: 0=Auto, 1=150, 2=300, 3=600 DPI.
    let dpi_labels = ["Auto", "150 DPI", "300 DPI", "600 DPI"];
    let dpi_values = [None, Some(150.0_f32), Some(300.0), Some(600.0)];
    let mut dpi_idx: usize = ctx
        .data_mut(|d| d.get_temp::<usize>(dpi_id))
        .unwrap_or(0)
        .min(dpi_labels.len() - 1);

    let mut confirm = false;
    let mut cancel = false;
    let mut open = true;

    modal_overlay(ctx, "pdf_import_dialog_overlay");

    egui::Window::new("Open PDF — Select Pages")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(DIALOG_ORDER)
        .default_width(360.0)
        .show(ctx, |ui| {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!(
                    "{} — {} pages",
                    data.dialogs.pdf_import_file_name, count
                ))
                .strong(),
            );
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                if ui.button("Select All").clicked() {
                    selection = vec![true; count];
                    range_text.clear();
                }
                if ui.button("Deselect All").clicked() {
                    selection = vec![false; count];
                    range_text.clear();
                }
            });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Pages:");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut range_text)
                        .hint_text("e.g. 1-3,5")
                        .desired_width(190.0),
                );
                if resp.changed() {
                    if let Some(mask) = parse_page_ranges(&range_text, count) {
                        selection = mask;
                    }
                }
            });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Resolution:");
                egui::ComboBox::from_id_salt("pdf_import_dpi_combo")
                    .selected_text(dpi_labels[dpi_idx])
                    .show_ui(ui, |ui| {
                        for (i, label) in dpi_labels.iter().enumerate() {
                            ui.selectable_value(&mut dpi_idx, i, *label);
                        }
                    });
            });
            ui.weak("Pages load on demand; page count does not reduce resolution.");

            ui.add_space(6.0);
            egui::ScrollArea::vertical()
                .max_height(300.0)
                .auto_shrink([false, true])
                .show_rows(ui, 20.0, count, |ui, rows| {
                    for i in rows {
                        let (w, h) = data
                            .dialogs
                            .pdf_import_page_dims
                            .get(i)
                            .copied()
                            .unwrap_or((0.0, 0.0));
                        ui.horizontal(|ui| {
                            let mut on = selection[i];
                            if ui.checkbox(&mut on, format!("Page {}", i + 1)).changed() {
                                selection[i] = on;
                                // Manual edits win over the range field.
                                range_text.clear();
                            }
                            ui.add_space(6.0);
                            ui.weak(format!("{:.0} × {:.0} pt", w, h));
                        });
                    }
                });

            let selected = selection.iter().filter(|&&s| s).count();
            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(format!("{selected}/{count} selected"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    ui.add_enabled_ui(selected > 0, |ui| {
                        if ui.button(format!("Open {selected} pages")).clicked() {
                            confirm = true;
                        }
                    });
                });
            });
        });

    // Closing via the window's X counts as cancel.
    if !open {
        cancel = true;
    }

    if confirm {
        let indices: Vec<usize> = selection
            .iter()
            .enumerate()
            .filter_map(|(i, &s)| s.then_some(i))
            .collect();
        actions.doc.pdf_import_confirm = Some((indices, dpi_values[dpi_idx]));
        ctx.data_mut(|d| {
            d.remove::<Vec<bool>>(sel_id);
            d.remove::<String>(range_id);
            d.remove::<usize>(dpi_id);
        });
    } else if cancel {
        actions.doc.pdf_import_cancel = true;
        ctx.data_mut(|d| {
            d.remove::<Vec<bool>>(sel_id);
            d.remove::<String>(range_id);
            d.remove::<usize>(dpi_id);
        });
    } else {
        // Persist for the next frame.
        ctx.data_mut(|d| {
            d.insert_temp(sel_id, selection);
            d.insert_temp(range_id, range_text);
            d.insert_temp(dpi_id, dpi_idx);
        });
    }
}

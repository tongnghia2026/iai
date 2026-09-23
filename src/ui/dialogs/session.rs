//! App/session dialogs: preferences, exit/close confirmations, reload
//! prompt, PDF import.

use super::*;

/// Fixed inner width for the exit/close confirmation dialogs, so they keep a
/// constant size and centred position regardless of the document behind them.
const EXIT_DIALOG_WIDTH: f32 = 384.0;

/// The category tabs down the left edge, Photoshop-style.
const PREFERENCES_CATEGORIES: [&str; 7] = [
    "Tổng quát",
    "Giao diện",
    "Hiệu năng",
    "Tệp & Tự lưu",
    "Công cụ & Con trỏ",
    "AI",
    "Phím tắt",
];

pub(crate) fn preferences_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    // Esc = cancel (revert). Enter is swallowed so finishing a typed value never
    // dismisses the dialog by accident.
    let (_enter_pressed, esc_pressed) = consume_dialog_enter_escape(ctx);
    let mut do_ok = false;
    let mut do_cancel = esc_pressed;

    // Which category is shown, remembered across frames in egui temp state.
    let cat_id = egui::Id::new("preferences_active_category");
    let orig_id = egui::Id::new("preferences_original_settings");
    let mut category: usize = ctx.data(|d| d.get_temp::<usize>(cat_id)).unwrap_or(0);

    // Baseline captured the first frame the dialog is shown; "Hoàn tác" / Esc
    // restores it, so changes previewed live can still be undone.
    let original = ctx
        .data_mut(|d| d.get_temp::<crate::core::settings::AppSettings>(orig_id))
        .unwrap_or_else(|| data.settings.clone());
    ctx.data_mut(|d| d.insert_temp(orig_id, original.clone()));

    // Edit a working copy; emit it only if it actually differs from the live
    // settings, so the app applies + persists exactly the changed values.
    let mut settings = data.settings.clone();

    modal_overlay(ctx, "preferences_dialog_overlay");

    // Never let the window grow taller than the screen: cap the scrolling
    // content area, and keep the header, footer and buttons always on-screen.
    let screen = ctx.screen_rect();
    let max_content_h = (screen.height() - 170.0).clamp(220.0, 520.0);

    egui::Window::new("Preferences")
        .collapsible(false)
        .resizable(true)
        // Centre on first open but stay draggable (an anchored window can't move).
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(screen.center())
        .default_width(600.0)
        .min_width(520.0)
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal_top(|ui| {
                // Left: category list.
                ui.vertical(|ui| {
                    ui.set_width(150.0);
                    for (idx, name) in PREFERENCES_CATEGORIES.iter().enumerate() {
                        if ui
                            .add_sized(
                                [ui.available_width(), 24.0],
                                egui::Button::selectable(category == idx, *name),
                            )
                            .clicked()
                        {
                            category = idx;
                        }
                    }
                });

                // A gap, NOT ui.separator(): a vertical separator in a horizontal
                // layout greedily grows to the full available height, which pushed
                // the whole window (and the OK button) off-screen.
                ui.add_space(12.0);

                // Right: the selected category's content, scrolling when tall.
                ui.vertical(|ui| {
                    ui.set_min_width(340.0);
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, true])
                        .max_height(max_content_h)
                        .show(ui, |ui| match category {
                            0 => preferences_general(ui, &mut settings),
                            1 => preferences_appearance(ui),
                            2 => preferences_performance(ui),
                            3 => preferences_files(ui, &mut settings),
                            4 => preferences_tools(ui, &mut settings),
                            5 => preferences_ai(ui, &mut settings),
                            _ => preferences_shortcuts(ui),
                        });
                });
            });

            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "Lưu tại {}",
                        crate::ui::theme::prefs_path().display()
                    ))
                    .color(egui::Color32::GRAY)
                    .size(11.0),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("  OK  ").clicked() {
                        do_ok = true;
                    }
                    if ui.button("Hoàn tác").clicked() {
                        do_cancel = true;
                    }
                });
            });
            ui.add_space(2.0);
        });

    ctx.data_mut(|d| d.insert_temp(cat_id, category));

    if do_cancel {
        // Restore the baseline (the app applies + persists it) and close.
        if original != data.settings {
            actions.settings.updated = Some(original);
        }
        ctx.data_mut(|d| d.remove::<crate::core::settings::AppSettings>(orig_id));
        actions.dialogs.show_preferences = Some(false);
    } else if do_ok {
        // Keep the (already-applied) changes and close.
        if settings != data.settings {
            actions.settings.updated = Some(settings);
        }
        ctx.data_mut(|d| d.remove::<crate::core::settings::AppSettings>(orig_id));
        actions.dialogs.show_preferences = Some(false);
    } else if settings != data.settings {
        // Preview edits live while the dialog stays open.
        actions.settings.updated = Some(settings);
    }
}

fn preferences_section_title(ui: &mut egui::Ui, text: &str) {
    ui.add_space(2.0);
    ui.label(egui::RichText::new(text).strong());
    ui.add_space(4.0);
}

fn preferences_general(ui: &mut egui::Ui, settings: &mut crate::core::settings::AppSettings) {
    preferences_section_title(ui, "Đơn vị mặc định");
    ui.horizontal(|ui| {
        ui.label("Thước & hộp thoại kích thước:");
        egui::ComboBox::from_id_salt("pref_default_unit")
            .selected_text(settings.default_unit.name())
            .show_ui(ui, |ui| {
                for unit in ruler_unit_choices() {
                    ui.selectable_value(&mut settings.default_unit, unit, unit.name());
                }
            });
    });
    ui.label(
        egui::RichText::new("Áp dụng cho thước và các hộp thoại kích thước; đổi là dùng ngay.")
            .color(egui::Color32::GRAY)
            .size(11.0),
    );
}

fn preferences_appearance(ui: &mut egui::Ui) {
    preferences_section_title(ui, "Giao diện");
    ui.horizontal(|ui| {
        ui.label("Chủ đề:");
        ui.label(egui::RichText::new("Tối").strong());
    });
    ui.label(
        egui::RichText::new("iAi hiện dùng một chủ đề tối duy nhất.")
            .color(egui::Color32::GRAY)
            .size(11.0),
    );
}

fn preferences_performance(ui: &mut egui::Ui) {
    preferences_section_title(ui, "Bộ nhớ Hoàn tác (Undo)");
    let budget_mb = crate::core::hw::history_budget_bytes() / (1024 * 1024);
    ui.label(format!("Ngân sách hiện tại: {budget_mb} MB"));
    ui.label(
        egui::RichText::new(
            "Tự tính theo RAM máy; chỉ lưu phần ảnh thay đổi nên đủ cho hàng chục bước.",
        )
        .color(egui::Color32::GRAY)
        .size(11.0),
    );

    ui.add_space(12.0);
    preferences_section_title(ui, "Card đồ họa (GPU)");
    match crate::core::hw::gpu() {
        Some(info) => {
            ui.label(&info.name);
            ui.label(
                egui::RichText::new(format!("{} · {}", info.device_type, info.backend))
                    .color(egui::Color32::GRAY)
                    .size(11.0),
            );
        }
        None => {
            ui.label(egui::RichText::new("Đang dò tìm…").color(egui::Color32::GRAY));
        }
    }
}

fn preferences_files(ui: &mut egui::Ui, settings: &mut crate::core::settings::AppSettings) {
    use crate::core::settings::{AUTOSAVE_MAX_SECS, AUTOSAVE_MIN_SECS};

    preferences_section_title(ui, "Tự lưu & khôi phục");
    ui.checkbox(
        &mut settings.autosave_enabled,
        "Tự lưu bản khôi phục khi đang làm việc",
    );
    ui.add_enabled_ui(settings.autosave_enabled, |ui| {
        ui.horizontal(|ui| {
            ui.label("Chu kỳ:");
            ui.add(
                egui::DragValue::new(&mut settings.autosave_interval_secs)
                    .speed(5.0)
                    .range(AUTOSAVE_MIN_SECS..=AUTOSAVE_MAX_SECS)
                    .suffix(" giây"),
            );
        });
    });
    ui.label(
        egui::RichText::new(
            "Bản khôi phục lưu ở thư mục dữ liệu của iAi và tự xóa khi bạn lưu/đóng bình thường.",
        )
        .color(egui::Color32::GRAY)
        .size(11.0),
    );
}

fn preferences_tools(ui: &mut egui::Ui, settings: &mut crate::core::settings::AppSettings) {
    preferences_section_title(ui, "Bắt dính (Snap)");
    ui.checkbox(
        &mut settings.snap_default,
        "Bật bắt dính mặc định khi mở iAi",
    );
    ui.label(
        egui::RichText::new(
            "Vẫn có thể bật/tắt nhanh bằng nút nam châm trên thanh công cụ cho phiên hiện tại.",
        )
        .color(egui::Color32::GRAY)
        .size(11.0),
    );
}

fn preferences_ai(ui: &mut egui::Ui, settings: &mut crate::core::settings::AppSettings) {
    preferences_section_title(ui, "Tăng tốc AI");
    ui.checkbox(
        &mut settings.ai_use_gpu,
        "Dùng GPU (DirectML) cho Select Subject & Smart Fill",
    );
    let status = if crate::core::hw::ai_gpu_candidate() {
        "Máy có GPU phù hợp. Nếu một mô hình lỗi trên GPU, iAi tự lùi về CPU."
    } else {
        "Chưa phát hiện GPU phù hợp — các tính năng này sẽ chạy trên CPU."
    };
    ui.label(
        egui::RichText::new(status)
            .color(egui::Color32::GRAY)
            .size(11.0),
    );
}

fn preferences_shortcuts(ui: &mut egui::Ui) {
    preferences_section_title(ui, "Phím tắt hiện tại");
    ui.label(
        egui::RichText::new("Danh sách chỉ để xem. Đổi phím tắt sẽ mở ở bước kế tiếp.")
            .color(egui::Color32::GRAY)
            .size(11.0),
    );
    ui.add_space(6.0);
    let shortcuts = [
        ("Preferences", "Ctrl+K"),
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
}

/// Units offered as a default for rulers / size dialogs. Percent is excluded —
/// it is meaningless as a standalone default measurement.
fn ruler_unit_choices() -> [crate::core::units::Unit; 6] {
    use crate::core::units::Unit;
    [
        Unit::Pixels,
        Unit::Centimeters,
        Unit::Millimeters,
        Unit::Inches,
        Unit::Points,
        Unit::Picas,
    ]
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
            // Pin the width so the dialog is the same size and stays dead-centre
            // no matter which document (or how long its title) triggers it — the
            // title is truncated to one line so it can't stretch the window.
            ui.set_width(EXIT_DIALOG_WIDTH);
            ui.add_space(8.0);
            let title = data
                .doc
                .doc_titles
                .get(data.doc.active_doc_idx)
                .map(String::as_str)
                .unwrap_or("Untitled");
            ui.add(
                egui::Label::new(format!("Save changes to “{title}” before exiting?")).truncate(),
            );
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
            ui.set_width(EXIT_DIALOG_WIDTH);
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

pub(crate) fn document_editor_error_dialog(
    ctx: &egui::Context,
    data: &UiData,
    actions: &mut UiActions,
) {
    let (enter_pressed, esc_pressed) = consume_dialog_enter_escape(ctx);
    let mut dismiss = enter_pressed || esc_pressed;

    modal_overlay(ctx, "document_editor_error_dialog_overlay");

    egui::Window::new("Canvas Editor Error")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .min_width(440.0)
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.add_space(8.0);
            ui.label("Canvas Editor could not complete the operation.");
            ui.add_space(8.0);
            if let Some(message) = data.dialogs.document_editor_error.as_deref() {
                ui.label(egui::RichText::new(message).color(egui::Color32::from_rgb(
                    235, 170, 100,
                )));
            }
            ui.add_space(8.0);
            ui.label(
                "iAi kept the document open and did not overwrite a file. Dismiss this message, then retry the action.",
            );
            ui.add_space(16.0);
            if ui.button("  OK  ").clicked() {
                dismiss = true;
            }
        });

    if dismiss {
        actions.dialogs.dismiss_document_editor_error = true;
    }
}

pub(crate) fn reload_file_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let (enter_pressed, esc_pressed) = consume_dialog_enter_escape(ctx);
    let mut do_reload = enter_pressed;
    let mut do_keep = esc_pressed;

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

    let (enter_pressed, esc_pressed) = consume_dialog_enter_escape(ctx);
    let mut confirm = enter_pressed;
    let mut cancel = esc_pressed;
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

    if confirm && selection.iter().any(|&selected| selected) {
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

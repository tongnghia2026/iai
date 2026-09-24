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
    // Esc first dismisses an open "take this key over?" prompt; only a second
    // Esc cancels the whole dialog.
    let conflict_id = egui::Id::new(PREFS_SHORTCUT_CONFLICT_ID);
    let conflict_open = ctx.data(|d| {
        d.get_temp::<(
            crate::app::commands::Command,
            crate::app::commands::KeyChord,
            crate::app::commands::Command,
        )>(conflict_id)
            .is_some()
    });
    if esc_pressed && conflict_open {
        ctx.data_mut(|d| {
            d.remove::<(
                crate::app::commands::Command,
                crate::app::commands::KeyChord,
                crate::app::commands::Command,
            )>(conflict_id)
        });
    }
    let mut do_cancel = esc_pressed && !conflict_open;

    // Which category is shown, remembered across frames in egui temp state.
    let cat_id = egui::Id::new("preferences_active_category");
    let orig_id = egui::Id::new("preferences_original_settings");
    let mut category: usize = ctx.data(|d| d.get_temp::<usize>(cat_id)).unwrap_or(0);

    // Baseline captured the first frame the dialog is shown; "Cancel" / Esc
    // restores it, so changes previewed live can still be undone.
    let original = ctx
        .data_mut(|d| d.get_temp::<crate::core::settings::AppSettings>(orig_id))
        .unwrap_or_else(|| data.settings.clone());
    ctx.data_mut(|d| d.insert_temp(orig_id, original.clone()));

    // Edit a working copy; emit it only if it actually differs from the live
    // settings, so the app applies + persists exactly the changed values.
    let mut settings = data.settings.clone();

    // A key caught for Preferences ▸ Shortcuts (the app catches it before egui).
    if let Some((cmd, outcome)) = data.shortcut_captured {
        actions.settings.captured_taken = true;
        let mut keymap = crate::app::commands::KeyMap::from_overrides(&settings.shortcuts);
        match outcome {
            crate::ui::ShortcutCapture::Cancel => {}
            crate::ui::ShortcutCapture::Clear => {
                keymap.assign(cmd, None);
                shortcut_notice(ctx, format!("Đã bỏ phím của “{}”.", cmd.display_name()));
            }
            crate::ui::ShortcutCapture::Chord(chord) => {
                request_shortcut(ctx, &mut keymap, cmd, chord);
            }
        }
        settings.shortcuts = keymap.to_overrides();
    }

    modal_overlay(ctx, "preferences_dialog_overlay");

    // Resizable both ways but never taller than the screen, so the footer and
    // its buttons always stay reachable. The limit is on the content, so leave
    // room for the title bar and frame too.
    let screen = ctx.screen_rect();
    let max_window_h = (screen.height() - 90.0).max(300.0);
    let default_window_h = (screen.height() - 170.0)
        .clamp(380.0, 640.0)
        .min(max_window_h);

    egui::Window::new("Preferences")
        .collapsible(false)
        .resizable(true)
        // Centre on first open but stay draggable (an anchored window can't move).
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(screen.center())
        .default_size([600.0, default_window_h])
        .min_width(520.0)
        .min_height(300.0)
        .max_height(max_window_h)
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.add_space(6.0);
            // The body fills whatever height the window is dragged to (it scrolls
            // when the page is taller); the footer keeps its own strip below.
            const FOOTER_H: f32 = 40.0;
            let body_h = (ui.available_height() - FOOTER_H).max(120.0);
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
                        .auto_shrink([false, false])
                        .max_height(body_h)
                        .show(ui, |ui| match category {
                            0 => preferences_general(ui, &mut settings),
                            1 => preferences_appearance(ui),
                            2 => preferences_performance(ui, &mut settings),
                            3 => preferences_files(ui, &mut settings),
                            4 => preferences_tools(ui, &mut settings),
                            5 => preferences_ai(ui, &mut settings),
                            _ => preferences_shortcuts(ui, &mut settings, data, actions),
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
                    if ui.button("Cancel").clicked() {
                        do_cancel = true;
                    }
                });
            });
            ui.add_space(2.0);
        });

    shortcut_conflict_window(ctx, &mut settings);

    ctx.data_mut(|d| d.insert_temp(cat_id, category));

    if do_cancel {
        // Restore the baseline (the app applies + persists it) and close.
        if original != data.settings {
            actions.settings.updated = Some(original);
        }
        ctx.data_mut(|d| d.remove::<crate::core::settings::AppSettings>(orig_id));
        clear_preferences_page_state(ctx);
        actions.dialogs.show_preferences = Some(false);
    } else if do_ok {
        // Keep the (already-applied) changes and close.
        if settings != data.settings {
            actions.settings.updated = Some(settings);
        }
        ctx.data_mut(|d| d.remove::<crate::core::settings::AppSettings>(orig_id));
        clear_preferences_page_state(ctx);
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

    ui.add_space(14.0);
    preferences_section_title(ui, "Khôi phục");
    let is_default = *settings == crate::core::settings::AppSettings::default();
    if confirm_row(
        ui,
        PREFS_CONFIRM_RESET_ALL_ID,
        "Khôi phục toàn bộ cài đặt mặc định…",
        "Đưa TẤT CẢ cài đặt (kể cả phím tắt) về mặc định?",
        !is_default,
    ) {
        *settings = crate::core::settings::AppSettings::default();
    }
    if is_default {
        ui.label(
            egui::RichText::new("Mọi cài đặt đang ở mặc định.")
                .color(egui::Color32::GRAY)
                .size(11.0),
        );
    }
}

const PREFS_CONFIRM_RESET_ALL_ID: &str = "preferences_confirm_reset_all";
const PREFS_CONFIRM_RESET_KEYS_ID: &str = "preferences_confirm_reset_shortcuts";
const PREFS_SHORTCUT_NOTICE_ID: &str = "preferences_shortcut_notice";
const PREFS_SHORTCUT_CONFLICT_ID: &str = "preferences_shortcut_conflict";
const PREFS_SHORTCUT_SEARCH_ID: &str = "preferences_shortcut_search";

/// Forget the per-open page state (pending confirmations, notices, search).
fn clear_preferences_page_state(ctx: &egui::Context) {
    use crate::app::commands::{Command, KeyChord};
    ctx.data_mut(|d| {
        d.remove::<bool>(egui::Id::new(PREFS_CONFIRM_RESET_ALL_ID));
        d.remove::<bool>(egui::Id::new(PREFS_CONFIRM_RESET_KEYS_ID));
        d.remove::<String>(egui::Id::new(PREFS_SHORTCUT_NOTICE_ID));
        d.remove::<(Command, KeyChord, Command)>(egui::Id::new(PREFS_SHORTCUT_CONFLICT_ID));
        d.remove::<String>(egui::Id::new(PREFS_SHORTCUT_SEARCH_ID));
    });
}

/// A destructive button that asks once inline before acting. Returns `true`
/// on the frame the user confirms.
fn confirm_row(ui: &mut egui::Ui, id: &str, button: &str, question: &str, enabled: bool) -> bool {
    let id = egui::Id::new(id);
    let mut asking = ui.ctx().data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let mut confirmed = false;
    if asking {
        warning_frame().show(ui, |ui| {
            ui.label(
                egui::RichText::new(question)
                    .strong()
                    .color(egui::Color32::WHITE),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add(dialog_button("Khôi phục")).clicked() {
                    confirmed = true;
                    asking = false;
                }
                if ui.add(dialog_button("Hủy")).clicked() {
                    asking = false;
                }
            });
        });
    } else if ui.add_enabled(enabled, egui::Button::new(button)).clicked() {
        asking = true;
    }
    ui.ctx().data_mut(|d| d.insert_temp(id, asking));
    confirmed
}

/// Amber box that makes a pending question stand out from the page.
fn warning_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(egui::Color32::from_rgb(74, 54, 18))
        .stroke(egui::Stroke::new(
            1.5_f32,
            egui::Color32::from_rgb(230, 180, 90),
        ))
        .corner_radius(6)
        .inner_margin(egui::Margin::same(10))
}

/// A roomy button for the answers in a question box. Uses the normal dark
/// button fill: the theme accent is light grey, which washed out white text.
fn dialog_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(text.to_string()).min_size(egui::vec2(72.0, 28.0))
}

/// "This key belongs to another command — take it over?" as a small window
/// over Preferences, so it cannot scroll out of sight behind the list.
fn shortcut_conflict_window(
    ctx: &egui::Context,
    settings: &mut crate::core::settings::AppSettings,
) {
    use crate::app::commands::{Command, KeyChord, KeyMap};
    let conflict_id = egui::Id::new(PREFS_SHORTCUT_CONFLICT_ID);
    let Some((cmd, chord, other)) =
        ctx.data(|d| d.get_temp::<(Command, KeyChord, Command)>(conflict_id))
    else {
        return;
    };
    let mut close = false;
    egui::Window::new("Phím đang được dùng")
        .id(egui::Id::new("preferences_shortcut_conflict_window"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(egui::Order::Tooltip)
        .frame(warning_frame())
        .show(ctx, |ui| {
            ui.set_max_width(380.0);
            ui.label(
                egui::RichText::new(format!(
                    "{}  {} đang là phím của “{}”.",
                    egui_phosphor::regular::WARNING,
                    chord.label(),
                    other.display_name()
                ))
                .strong()
                .size(14.0)
                .color(egui::Color32::WHITE),
            );
            ui.add_space(2.0);
            ui.label(format!(
                "Gán cho “{}” thì “{}” sẽ không còn phím tắt.",
                cmd.display_name(),
                other.display_name()
            ));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.add(dialog_button("OK")).clicked() {
                    let mut keymap = KeyMap::from_overrides(&settings.shortcuts);
                    keymap.assign(cmd, Some(chord));
                    settings.shortcuts = keymap.to_overrides();
                    shortcut_notice(
                        ctx,
                        format!(
                            "Đã gán {} cho “{}”; “{}” hiện không có phím.",
                            chord.label(),
                            cmd.display_name(),
                            other.display_name()
                        ),
                    );
                    close = true;
                }
                if ui.add(dialog_button("Hủy")).clicked() {
                    close = true;
                }
            });
        });
    if close {
        ctx.data_mut(|d| d.remove::<(Command, KeyChord, Command)>(conflict_id));
    }
}

/// Show a one-line result on the Shortcuts page.
pub(crate) fn shortcut_notice(ctx: &egui::Context, text: String) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(PREFS_SHORTCUT_NOTICE_ID), text));
}

/// Try to give `chord` to `cmd`: a fixed key is refused with a notice, a key
/// another command holds waits for the user to confirm the takeover, and
/// anything else is assigned at once.
fn request_shortcut(
    ctx: &egui::Context,
    keymap: &mut crate::app::commands::KeyMap,
    cmd: crate::app::commands::Command,
    chord: crate::app::commands::KeyChord,
) {
    use crate::app::commands::ChordConflict;
    match keymap.conflict(cmd, chord) {
        Some(ChordConflict::Reserved(action)) => shortcut_notice(
            ctx,
            format!(
                "{} là phím cố định cho “{action}” — hãy chọn phím khác.",
                chord.label()
            ),
        ),
        Some(ChordConflict::Command(other)) => {
            ctx.data_mut(|d| {
                d.insert_temp(
                    egui::Id::new(PREFS_SHORTCUT_CONFLICT_ID),
                    (cmd, chord, other),
                )
            });
            ctx.data_mut(|d| d.remove::<String>(egui::Id::new(PREFS_SHORTCUT_NOTICE_ID)));
        }
        None => {
            keymap.assign(cmd, Some(chord));
            shortcut_notice(
                ctx,
                format!("Đã gán {} cho “{}”.", chord.label(), cmd.display_name()),
            );
        }
    }
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

fn preferences_performance(ui: &mut egui::Ui, settings: &mut crate::core::settings::AppSettings) {
    use crate::core::settings::{HISTORY_STEPS_MAX, HISTORY_STEPS_MIN};

    preferences_section_title(ui, "Hoàn tác (Undo)");
    ui.horizontal(|ui| {
        ui.label("Số bước Undo tối đa:");
        ui.add(
            egui::DragValue::new(&mut settings.history_steps)
                .speed(1.0)
                .range(HISTORY_STEPS_MIN..=HISTORY_STEPS_MAX)
                .suffix(" bước"),
        );
    });
    let budget_mb = crate::core::hw::history_budget_bytes() / (1024 * 1024);
    ui.label(
        egui::RichText::new(format!(
            "Áp dụng ngay cho mọi tab; giảm số bước sẽ bỏ các bước cũ nhất. \
             Bộ nhớ dành cho Undo tự tính theo RAM máy: {budget_mb} MB."
        ))
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

    use crate::core::settings::BrushCursorStyle;
    ui.add_space(14.0);
    preferences_section_title(ui, "Con trỏ cọ vẽ");
    for (style, label, hint) in [
        (
            BrushCursorStyle::Ring,
            "Vòng tròn theo cỡ cọ",
            "Thấy vùng cọ sẽ tô (mặc định).",
        ),
        (
            BrushCursorStyle::RingCrosshair,
            "Vòng tròn + chữ thập ở tâm",
            "Thêm dấu chữ thập nhỏ để đặt cọ chính xác.",
        ),
        (
            BrushCursorStyle::Precise,
            "Chữ thập chính xác",
            "Chỉ hiện chữ thập, không vẽ vòng cỡ cọ.",
        ),
    ] {
        ui.radio_value(&mut settings.brush_cursor, style, label)
            .on_hover_text(hint);
    }
    use crate::core::settings::BrushTipOutline;
    ui.add_space(6.0);
    ui.label("Vòng tròn với cọ mềm:");
    for (outline, label, hint) in [
        (
            BrushTipOutline::Normal,
            "Normal Brush Tip — như Photoshop (mặc định)",
            "Vòng nằm ở mức nét còn 50%; phần mờ của cọ mềm tô lan ra ngoài vòng.",
        ),
        (
            BrushTipOutline::FullSize,
            "Full Size Brush Tip",
            "Vòng bao hết vùng cọ chạm tới, kể cả phần mờ nhất.",
        ),
    ] {
        ui.radio_value(&mut settings.brush_tip_outline, outline, label)
            .on_hover_text(hint);
    }
    ui.label(
        egui::RichText::new(
            "Áp dụng cho Brush, Eraser, Clone, Repair, Dodge/Burn, Smudge, Smart Select.",
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

fn preferences_shortcuts(
    ui: &mut egui::Ui,
    settings: &mut crate::core::settings::AppSettings,
    data: &UiData,
    actions: &mut UiActions,
) {
    use crate::app::commands::{Command, CommandGroup, KeyChord, KeyMap};

    const KEY_COLOR: egui::Color32 = egui::Color32::from_rgb(180, 180, 255);
    const CHANGED_COLOR: egui::Color32 = egui::Color32::from_rgb(230, 180, 90);
    let ctx = ui.ctx().clone();
    let mut keymap = KeyMap::from_overrides(&settings.shortcuts);
    let before = keymap.clone();

    preferences_section_title(ui, "Phím tắt");
    ui.label(
        egui::RichText::new(
            "Bấm vào ô phím của một lệnh rồi nhấn tổ hợp phím mới. \
             Esc = hủy, Backspace = bỏ phím của lệnh đó.",
        )
        .color(egui::Color32::GRAY)
        .size(11.0),
    );
    ui.add_space(6.0);

    let conflict_id = egui::Id::new(PREFS_SHORTCUT_CONFLICT_ID);
    if let Some(notice) =
        ctx.data(|d| d.get_temp::<String>(egui::Id::new(PREFS_SHORTCUT_NOTICE_ID)))
    {
        ui.label(egui::RichText::new(notice).size(11.5).color(CHANGED_COLOR));
        ui.add_space(4.0);
    }

    let search_id = egui::Id::new(PREFS_SHORTCUT_SEARCH_ID);
    let mut query = ctx
        .data(|d| d.get_temp::<String>(search_id))
        .unwrap_or_default();
    ui.add(
        egui::TextEdit::singleline(&mut query)
            .hint_text("Tìm lệnh hoặc phím…")
            .desired_width(220.0),
    );
    ctx.data_mut(|d| d.insert_temp(search_id, query.clone()));
    let needle = query.trim().to_lowercase();

    let mut any_row = false;
    for group in CommandGroup::all() {
        let rows: Vec<Command> = Command::in_group(group)
            .filter(|cmd| {
                needle.is_empty()
                    || cmd.display_name().to_lowercase().contains(&needle)
                    || keymap.label_for(*cmd).to_lowercase().contains(&needle)
            })
            .collect();
        if rows.is_empty() {
            continue;
        }
        any_row = true;
        ui.add_space(6.0);
        ui.label(egui::RichText::new(group.title()).strong().size(12.0));
        egui::Grid::new(("shortcuts_grid", group.title()))
            .num_columns(3)
            .striped(true)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                for cmd in rows {
                    let changed = !keymap.is_default(cmd);
                    let name = egui::RichText::new(cmd.display_name());
                    ui.horizontal(|ui| {
                        if changed {
                            ui.label(name.strong());
                            ui.label(
                                egui::RichText::new("đã đổi")
                                    .size(10.5)
                                    .color(CHANGED_COLOR),
                            );
                        } else {
                            ui.label(name);
                        }
                    });

                    let capturing = data.shortcut_capture == Some(cmd);
                    let label = keymap.label_for(cmd);
                    let text = if capturing {
                        egui::RichText::new("Nhấn phím…").color(CHANGED_COLOR)
                    } else if label.is_empty() {
                        egui::RichText::new("—").color(egui::Color32::GRAY)
                    } else {
                        egui::RichText::new(label).color(KEY_COLOR)
                    };
                    let key_button = egui::Button::new(text.monospace())
                        .min_size(egui::vec2(130.0, 0.0))
                        .selected(capturing);
                    if ui
                        .add(key_button)
                        .on_hover_text("Bấm rồi nhấn tổ hợp phím mới")
                        .clicked()
                    {
                        actions.settings.capture = Some((!capturing).then_some(cmd));
                        ctx.data_mut(|d| {
                            d.remove::<String>(egui::Id::new(PREFS_SHORTCUT_NOTICE_ID));
                            d.remove::<(Command, KeyChord, Command)>(conflict_id);
                        });
                    }

                    let reset = egui::Button::new(egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE);
                    if ui
                        .add_enabled(changed, reset)
                        .on_hover_text(format!("Về mặc định ({})", cmd.default_label()))
                        .clicked()
                    {
                        request_shortcut(&ctx, &mut keymap, cmd, cmd.default_chord());
                    }
                    ui.end_row();
                }
            });
    }
    if !any_row {
        ui.label(
            egui::RichText::new("Không có lệnh nào khớp.")
                .color(egui::Color32::GRAY)
                .size(11.0),
        );
    }

    ui.add_space(10.0);
    ui.label(
        egui::RichText::new("Phím cố định (không đổi được)")
            .strong()
            .size(12.0),
    );
    egui::Grid::new("shortcuts_fixed_grid")
        .num_columns(2)
        .striped(true)
        .spacing([12.0, 3.0])
        .show(ui, |ui| {
            for (keys, action) in crate::app::commands::FIXED_SHORTCUTS {
                ui.label(egui::RichText::new(*action).color(egui::Color32::GRAY));
                ui.label(
                    egui::RichText::new(*keys)
                        .monospace()
                        .color(egui::Color32::GRAY),
                );
                ui.end_row();
            }
        });

    ui.add_space(10.0);
    ui.separator();
    ui.horizontal(|ui| {
        if ui
            .button(format!("{}  Xuất ra file…", egui_phosphor::regular::EXPORT))
            .on_hover_text("Lưu bộ phím tắt hiện tại để dùng lại hoặc chép sang máy khác")
            .clicked()
        {
            actions.settings.export_shortcuts = true;
        }
        if ui
            .button(format!(
                "{}  Nhập từ file…",
                egui_phosphor::regular::DOWNLOAD_SIMPLE
            ))
            .on_hover_text("Thay toàn bộ phím tắt bằng bộ đã xuất trước đó")
            .clicked()
        {
            actions.settings.import_shortcuts = true;
        }
    });
    ui.add_space(4.0);
    if confirm_row(
        ui,
        PREFS_CONFIRM_RESET_KEYS_ID,
        "Khôi phục phím tắt mặc định",
        "Đưa TẤT CẢ phím tắt về mặc định?",
        !settings.shortcuts.is_empty(),
    ) {
        keymap = KeyMap::default();
        shortcut_notice(&ctx, "Đã khôi phục toàn bộ phím tắt mặc định.".to_string());
        ctx.data_mut(|d| d.remove::<(Command, KeyChord, Command)>(conflict_id));
    }

    if keymap != before {
        settings.shortcuts = keymap.to_overrides();
    }
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
    // Enter takes the safe choice: it reloads only when that cannot throw away
    // unsaved edits (and their undo history); otherwise it keeps the open tab.
    let discards = data.dialogs.reload_will_discard_changes;
    let mut do_reload = enter_pressed && !discards;
    let mut do_keep = esc_pressed || (enter_pressed && discards);

    modal_overlay(ctx, "reload_file_dialog_overlay");

    egui::Window::new("File changed on disk")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.add_space(8.0);
            ui.label(format!(
                "{} đã được cập nhật bên ngoài. Bạn có muốn cập nhật tab đang mở từ file này không?",
                data.dialogs.reload_file_name
            ));
            if discards {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(
                        "Cảnh báo: cập nhật sẽ ghi đè những thay đổi chưa lưu trong iAi \
                         (nhấn Enter = giữ bản đang mở).",
                    )
                    .color(egui::Color32::from_rgb(220, 170, 80)),
                );
            }
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if ui.button("Cập nhật từ file").clicked() {
                    do_reload = true;
                }
                if ui.button("Giữ bản đang mở").clicked() {
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

#[cfg(test)]
mod reload_dialog_tests {
    use super::*;

    fn press(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn run_reload_dialog(will_discard: bool, key: egui::Key) -> UiActions {
        let mut data = UiData::default();
        data.dialogs.reload_file_name = "photo.iai".to_string();
        data.dialogs.reload_will_discard_changes = will_discard;
        let mut actions = UiActions::default();
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(
            egui::RawInput {
                events: vec![press(key)],
                ..Default::default()
            },
            |ui| reload_file_dialog(ui.ctx(), &data, &mut actions),
        );
        actions
    }

    #[test]
    fn enter_keeps_the_open_tab_when_reload_would_discard_edits() {
        let actions = run_reload_dialog(true, egui::Key::Enter);
        assert!(!actions.doc.reload_open_file_confirm);
        assert!(actions.doc.reload_open_file_cancel);
    }

    #[test]
    fn enter_reloads_a_clean_tab() {
        let actions = run_reload_dialog(false, egui::Key::Enter);
        assert!(actions.doc.reload_open_file_confirm);
        assert!(!actions.doc.reload_open_file_cancel);
    }

    #[test]
    fn escape_always_keeps_the_open_tab() {
        for will_discard in [false, true] {
            let actions = run_reload_dialog(will_discard, egui::Key::Escape);
            assert!(!actions.doc.reload_open_file_confirm);
            assert!(actions.doc.reload_open_file_cancel);
        }
    }
}

#[cfg(test)]
mod shortcut_editor_tests {
    use super::*;
    use crate::app::commands::{Command, KeyChord, KeyName};

    /// One Preferences frame with `captured` delivered, as the app would.
    fn deliver(captured: (Command, crate::ui::ShortcutCapture)) -> (UiActions, egui::Context) {
        let mut data = UiData::default();
        data.shortcut_captured = Some(captured);
        let mut actions = UiActions::default();
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            preferences_dialog(ui.ctx(), &data, &mut actions)
        });
        (actions, ctx)
    }

    fn saved(actions: &UiActions) -> Option<std::collections::BTreeMap<String, String>> {
        actions
            .settings
            .updated
            .as_ref()
            .map(|s| s.shortcuts.clone())
    }

    #[test]
    fn a_free_key_is_assigned_and_saved() {
        let chord = KeyChord::plain(KeyName::Q);
        let (actions, _) = deliver((Command::ToolBrush, crate::ui::ShortcutCapture::Chord(chord)));
        assert!(actions.settings.captured_taken);
        let saved = saved(&actions).expect("settings updated");
        assert_eq!(saved.get("tool.brush").map(String::as_str), Some("Q"));
    }

    #[test]
    fn a_key_held_by_another_command_waits_for_confirmation() {
        let chord = KeyChord::plain(KeyName::E);
        let (actions, ctx) =
            deliver((Command::ToolBrush, crate::ui::ShortcutCapture::Chord(chord)));
        assert!(actions.settings.captured_taken);
        assert_eq!(
            saved(&actions),
            None,
            "nothing may change before the user agrees"
        );
        let pending = ctx.data(|d| {
            d.get_temp::<(Command, KeyChord, Command)>(egui::Id::new(PREFS_SHORTCUT_CONFLICT_ID))
        });
        assert_eq!(
            pending,
            Some((Command::ToolBrush, chord, Command::ToolEraser))
        );
    }

    #[test]
    fn esc_first_closes_the_takeover_prompt_then_the_dialog() {
        let (_, ctx) = deliver((
            Command::ToolBrush,
            crate::ui::ShortcutCapture::Chord(KeyChord::plain(KeyName::E)),
        ));
        let esc = || egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let data = UiData::default();
        let conflict = |ctx: &egui::Context| {
            ctx.data(|d| {
                d.get_temp::<(Command, KeyChord, Command)>(egui::Id::new(
                    PREFS_SHORTCUT_CONFLICT_ID,
                ))
            })
        };

        let mut actions = UiActions::default();
        let _ = ctx.run_ui(esc(), |ui| {
            preferences_dialog(ui.ctx(), &data, &mut actions)
        });
        assert_eq!(conflict(&ctx), None, "Esc dismisses the prompt");
        assert_eq!(
            actions.dialogs.show_preferences, None,
            "…but keeps Preferences open"
        );

        let mut actions = UiActions::default();
        let _ = ctx.run_ui(esc(), |ui| {
            preferences_dialog(ui.ctx(), &data, &mut actions)
        });
        assert_eq!(actions.dialogs.show_preferences, Some(false));
    }

    #[test]
    fn a_fixed_key_is_refused() {
        let (actions, ctx) = deliver((
            Command::ToolBrush,
            crate::ui::ShortcutCapture::Chord(KeyChord::plain(KeyName::X)),
        ));
        assert_eq!(saved(&actions), None);
        let notice = ctx.data(|d| d.get_temp::<String>(egui::Id::new(PREFS_SHORTCUT_NOTICE_ID)));
        assert!(notice.unwrap_or_default().contains("Swap colours"));
    }

    #[test]
    fn backspace_removes_the_key_and_esc_changes_nothing() {
        let (actions, _) = deliver((Command::FileSave, crate::ui::ShortcutCapture::Clear));
        let saved = saved(&actions).expect("settings updated");
        assert_eq!(saved.get("file.save").map(String::as_str), Some(""));

        let (actions, _) = deliver((Command::FileSave, crate::ui::ShortcutCapture::Cancel));
        assert!(actions.settings.captured_taken);
        assert_eq!(super::shortcut_editor_tests::saved(&actions), None);
    }
}

#[cfg(test)]
mod preferences_window_tests {
    use super::*;

    const SCREEN: egui::Vec2 = egui::vec2(1280.0, 900.0);

    fn frame(ctx: &egui::Context, events: Vec<egui::Event>) {
        let data = UiData::default();
        let mut actions = UiActions::default();
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
                events,
                ..Default::default()
            },
            |ui| preferences_dialog(ui.ctx(), &data, &mut actions),
        );
    }

    fn window_rect(ctx: &egui::Context) -> egui::Rect {
        ctx.memory(|m| m.area_rect(egui::Id::new("Preferences")))
            .expect("Preferences window shown")
    }

    /// Drag the window's bottom-right corner by `delta`, in small steps.
    fn drag_corner(ctx: &egui::Context, delta: egui::Vec2) {
        let start = window_rect(ctx).right_bottom() - egui::vec2(2.0, 2.0);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(ctx, vec![egui::Event::PointerMoved(start)]);
        frame(ctx, vec![button(start, true)]);
        for step in 1..=8 {
            frame(
                ctx,
                vec![egui::Event::PointerMoved(
                    start + delta * (step as f32 / 8.0),
                )],
            );
        }
        frame(ctx, vec![button(start + delta, false)]);
        for _ in 0..3 {
            frame(ctx, vec![]);
        }
    }

    #[test]
    fn the_window_resizes_vertically_both_ways_and_stays_on_screen() {
        let ctx = egui::Context::default();
        for _ in 0..4 {
            frame(&ctx, vec![]);
        }
        let start = window_rect(&ctx);
        assert!(start.height() <= SCREEN.y, "starts on screen: {start:?}");

        drag_corner(&ctx, egui::vec2(0.0, -150.0));
        let shorter = window_rect(&ctx);
        assert!(
            shorter.height() < start.height() - 100.0,
            "dragging up must shrink it: {start:?} -> {shorter:?}"
        );

        drag_corner(&ctx, egui::vec2(0.0, 250.0));
        let taller = window_rect(&ctx);
        assert!(
            taller.height() > shorter.height() + 200.0,
            "dragging down must grow it: {shorter:?} -> {taller:?}"
        );

        drag_corner(&ctx, egui::vec2(120.0, 0.0));
        let wider = window_rect(&ctx);
        assert!(
            wider.width() > taller.width() + 80.0,
            "horizontal resizing still works: {taller:?} -> {wider:?}"
        );

        drag_corner(&ctx, egui::vec2(0.0, 2000.0));
        let huge = window_rect(&ctx);
        assert!(
            huge.height() <= SCREEN.y,
            "never taller than the screen: {huge:?}"
        );
    }
}

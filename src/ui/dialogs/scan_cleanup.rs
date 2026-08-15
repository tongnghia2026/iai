//! "Làm sạch bản scan" dialog (Image ▸ Làm sạch bản scan…): flatten an uneven,
//! greyed scan background to white and deepen text. One preset (grayscale /
//! bilevel) plus a strength slider, and — for a multi-page PDF — a page scope
//! (current / range / all).
//!
//! Non-blocking with a live canvas preview (like the Filter/Levels dialogs): the
//! window is movable and leaves the canvas navigable so the page shows behind it;
//! every frame it streams the current params so the result updates in real time.
//! OK commits undoably; Cancel restores the layer.

use super::*;
use crate::core::scan_cleanup::{
    ScanCleanScope, ScanCleanupMode, ScanCleanupParams, ScanCleanupRequest,
};

pub(crate) fn scan_cleanup_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let mode_id = egui::Id::new("scan_mode");
    let strength_id = egui::Id::new("scan_strength");
    let scope_id = egui::Id::new("scan_scope");
    let from_id = egui::Id::new("scan_from");
    let to_id = egui::Id::new("scan_to");

    let is_pdf = data.dialogs.scan_is_pdf;
    let page_count = data.dialogs.scan_page_count.max(1);
    let active_page = data.dialogs.scan_active_page.min(page_count - 1);

    // 0 = grayscale, 1 = bilevel. Scope: 0 = current, 1 = range, 2 = all.
    let mut mode: i8 = ctx.data_mut(|d| d.get_temp(mode_id).unwrap_or(0));
    let mut strength: f32 = ctx.data_mut(|d| d.get_temp(strength_id).unwrap_or(1.0));
    let mut scope: i8 = ctx.data_mut(|d| d.get_temp(scope_id).unwrap_or(0));
    let mut from: i32 = ctx.data_mut(|d| d.get_temp(from_id).unwrap_or(1));
    let mut to: i32 = ctx.data_mut(|d| d.get_temp(to_id).unwrap_or(page_count as i32));

    let (enter_pressed, esc_pressed) = consume_dialog_enter_escape(ctx);
    let mut do_apply = enter_pressed;
    let mut do_cancel = esc_pressed;
    let mut open = true;

    let default_pos = document_side_dialog_pos(ctx, data, 360.0, 96.0);
    egui::Window::new("Làm sạch bản scan")
        .collapsible(false)
        .resizable(false)
        .movable(true)
        .open(&mut open)
        .default_pos(default_pos)
        .order(DIALOG_ORDER)
        .min_width(360.0)
        .show(ctx, |ui| {
            ui.add_space(6.0);
            ui.label("Làm phẳng nền xám/tối không đều của bản scan về trắng, chữ đậm rõ hơn.");
            ui.label(
                egui::RichText::new("Xem trực tiếp trên trang; OK để áp dụng, Hủy để bỏ.")
                    .size(10.0)
                    .color(egui::Color32::from_gray(140)),
            );
            ui.add_space(10.0);

            ui.label("Kiểu:");
            ui.horizontal(|ui| {
                ui.radio_value(&mut mode, 0, "Ảnh xám đẹp");
                ui.radio_value(&mut mode, 1, "Đen trắng (để in)");
            });
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                ui.label("Mức làm sạch:");
                ui.add(egui::Slider::new(&mut strength, 0.0..=1.0).show_value(false));
                ui.label(format!("{:.0}%", strength * 100.0));
            });
            ui.add_space(10.0);

            ui.separator();
            ui.add_space(6.0);
            ui.label("Phạm vi:");
            if is_pdf {
                ui.radio_value(
                    &mut scope,
                    0,
                    format!("Chỉ trang hiện tại (trang {})", active_page + 1),
                );
                ui.horizontal(|ui| {
                    ui.radio_value(&mut scope, 1, "Từ trang");
                    ui.add_enabled_ui(scope == 1, |ui| {
                        ui.add(egui::DragValue::new(&mut from).range(1..=page_count as i32));
                        ui.label("đến");
                        ui.add(egui::DragValue::new(&mut to).range(1..=page_count as i32));
                    });
                });
                ui.radio_value(&mut scope, 2, format!("Tất cả trang ({page_count} trang)"));
                ui.weak("(Xem trực tiếp áp cho trang hiện tại; OK mới xử lý cả phạm vi.)");
                if scope == 2 && page_count > 60 {
                    ui.colored_label(
                        egui::Color32::from_rgb(200, 150, 60),
                        format!("Lưu ý: xử lý {page_count} trang có thể chậm và tốn bộ nhớ."),
                    );
                }
            } else {
                scope = 0;
                ui.weak("Ảnh đơn — áp dụng cho ảnh hiện tại.");
            }

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

    if !open {
        do_cancel = true;
    }

    // Keep the typed range ascending.
    if from > to {
        std::mem::swap(&mut from, &mut to);
    }

    ctx.data_mut(|d| {
        d.insert_temp(mode_id, mode);
        d.insert_temp(strength_id, strength);
        d.insert_temp(scope_id, scope);
        d.insert_temp(from_id, from);
        d.insert_temp(to_id, to);
    });

    let params = ScanCleanupParams {
        mode: if mode == 1 {
            ScanCleanupMode::Bilevel
        } else {
            ScanCleanupMode::Grayscale
        },
        strength,
    };

    if do_apply {
        let scope = if is_pdf {
            match scope {
                2 => ScanCleanScope::AllPages,
                1 => ScanCleanScope::Range {
                    from: from.max(1) as usize,
                    to: to.max(1) as usize,
                },
                _ => ScanCleanScope::CurrentPage,
            }
        } else {
            ScanCleanScope::CurrentPage
        };
        actions.dialogs.apply_scan_cleanup = Some(ScanCleanupRequest { params, scope });
    } else if do_cancel {
        actions.dialogs.cancel_scan_cleanup_dialog = true;
    } else {
        // Stream the current params so the canvas preview tracks the sliders.
        actions.dialogs.set_scan_cleanup_preview = Some(params);
    }
}

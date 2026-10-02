//! "Làm ảnh thẻ" dialog (Image ▸ Làm ảnh thẻ…): one click turns a portrait
//! into a 3×4 ID photo (2.8×3.8 cm, 600 ppi) framed from the eyes and chin,
//! levelled, with the person on its own layer over white. The options are
//! remembered in prefs.json (key `id_photo`).

use super::*;
use crate::core::id_photo::{IdPhotoOptions, DEFAULT_WIDEN, MAX_WIDEN};

const PREFS_KEY: &str = "id_photo";

fn load_options() -> IdPhotoOptions {
    std::fs::read_to_string(prefs_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| serde_json::from_value::<IdPhotoOptions>(v[PREFS_KEY].clone()).ok())
        .unwrap_or_default()
}

fn save_options(options: IdPhotoOptions) {
    let path = prefs_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
    if !value.is_object() {
        value = serde_json::Value::Object(Default::default());
    }
    if let (Some(map), Ok(v)) = (value.as_object_mut(), serde_json::to_value(options)) {
        map.insert(PREFS_KEY.to_string(), v);
    }
    if let Ok(json) = serde_json::to_string_pretty(&value) {
        let _ = std::fs::write(path, json);
    }
}

pub(crate) fn id_photo_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let id = egui::Id::new("id_photo_options");
    let mut options: IdPhotoOptions = ctx
        .data_mut(|d| d.get_temp(id))
        .unwrap_or_else(load_options);
    let busy = data.dialogs.id_photo_busy;
    let (enter_pressed, esc_pressed) = consume_dialog_enter_escape(ctx);
    let can_run = !busy && (options.crop || options.white_background);
    let mut run = enter_pressed && can_run;
    let mut close = esc_pressed;
    let mut open = true;

    let default_pos = document_side_dialog_pos(ctx, data, 380.0, 96.0);
    egui::Window::new("Làm ảnh thẻ")
        .collapsible(false)
        .resizable(false)
        .movable(true)
        .open(&mut open)
        .default_pos(default_pos)
        .order(DIALOG_ORDER)
        .min_width(380.0)
        .show(ctx, |ui| {
            ui.add_space(6.0);
            ui.label("Tự cắt ảnh thẻ theo mắt và cằm, tách người ra nền trắng.");
            ui.label(
                egui::RichText::new(
                    "Có vùng chọn thì chỉ lấy người trong vùng chọn. Ctrl+Z để hoàn tác.",
                )
                .size(10.0)
                .color(egui::Color32::from_gray(140)),
            );
            ui.add_space(10.0);

            ui.add_enabled_ui(!busy, |ui| {
                ui.checkbox(
                    &mut options.crop,
                    "Cắt cỡ 3×4 (2,8×3,8 cm · 600 ppi · 661×898 px)",
                );
                ui.add_enabled_ui(options.crop, |ui| {
                    ui.horizontal(|ui| {
                        ui.add_space(22.0);
                        ui.label("Khung rộng hơn mẫu:");
                        let mut percent = options.widen * 100.0;
                        ui.add(
                            egui::Slider::new(&mut percent, 0.0..=MAX_WIDEN * 100.0)
                                .step_by(1.0)
                                .show_value(false),
                        );
                        ui.label(format!("{percent:.0}%"));
                        options.widen = percent / 100.0;
                        if (options.widen - DEFAULT_WIDEN).abs() > 1e-3
                            && ui
                                .small_button("Mặc định")
                                .on_hover_text("Rộng hơn mẫu 10%")
                                .clicked()
                        {
                            options.widen = DEFAULT_WIDEN;
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.add_space(22.0);
                        ui.checkbox(&mut options.straighten, "Xoay thẳng theo đường mắt");
                    });
                });
                ui.add_space(4.0);
                ui.checkbox(
                    &mut options.white_background,
                    "Tách người ra layer riêng, nền trắng",
                )
                .on_hover_text(
                    "Background thành trắng; người nhân 2 layer: \"Người\" (đã tách nền) và bên dưới \"Ảnh gốc\" (mask đen — tô trắng để lấy lại chi tiết)",
                );
                ui.checkbox(&mut options.then_portrait, "Xong thì mở Chỉnh chân dung");
            });

            let status = &data.dialogs.id_photo_status;
            if busy || !status.is_empty() {
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    if busy {
                        ui.spinner();
                    }
                    let color = if data.dialogs.id_photo_error {
                        egui::Color32::from_rgb(220, 90, 80)
                    } else {
                        egui::Color32::from_gray(170)
                    };
                    ui.label(egui::RichText::new(status.as_str()).color(color));
                });
            }

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let can_run = !busy && (options.crop || options.white_background);
                if ui
                    .add_enabled(can_run, egui::Button::new("  Làm ảnh thẻ  "))
                    .clicked()
                {
                    run = true;
                }
                if ui.button("Đóng").clicked() {
                    close = true;
                }
            });
            ui.add_space(4.0);
        });

    if !open {
        close = true;
    }
    ctx.data_mut(|d| d.insert_temp(id, options));
    if run {
        save_options(options);
        actions.dialogs.run_id_photo = Some(options);
    } else if close {
        save_options(options);
        actions.dialogs.show_id_photo_dialog = Some(false);
    }
}

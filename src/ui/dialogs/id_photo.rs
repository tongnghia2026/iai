//! "Làm ảnh thẻ" dialog (Image ▸ Làm ảnh thẻ…): one click turns a portrait
//! into a 3×4 ID photo (2.8×3.8 cm) framed from the eyes and chin,
//! levelled, with the person on its own layer over white. The options are
//! remembered in prefs.json (key `id_photo`), and the "Công thức" Chỉnh chân
//! dung then opens with (key `id_photo_preset`).

use super::*;
use crate::core::id_photo::{IdPhotoOptions, DEFAULT_WIDEN, MAX_WIDEN, PRINT_PX};
use egui_phosphor::regular as ph;

const PREFS_KEY: &str = "id_photo";
const PRESET_KEY: &str = "id_photo_preset";

fn load_options() -> IdPhotoOptions {
    load_pref(PREFS_KEY).unwrap_or_default()
}

fn save_options(options: IdPhotoOptions) {
    save_pref(PREFS_KEY, &options);
}

pub(crate) fn id_photo_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let id = egui::Id::new("id_photo_options");
    let mut options: IdPhotoOptions = ctx
        .data_mut(|d| d.get_temp(id))
        .unwrap_or_else(load_options);
    // The preset Chỉnh chân dung opens with, by name; none = its defaults.
    let preset_id = egui::Id::new(PRESET_KEY);
    let mut preset: String = ctx
        .data_mut(|d| d.get_temp(preset_id))
        .unwrap_or_else(|| load_pref(PRESET_KEY).unwrap_or_default());
    let presets = super::portrait::kept_presets(ctx);
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
                let (w, h) = PRINT_PX;
                ui.checkbox(
                    &mut options.crop,
                    format!("Cắt cỡ 3×4 (2,8×3,8 cm · {w}×{h} px)"),
                )
                .on_hover_text(
                    "Cắt dư điểm ảnh để tấm 4×6 trên trang in cũng nét như tấm 3×4; in ra vẫn đúng 2,8×3,8 cm",
                );
                ui.add_enabled_ui(options.crop, |ui| {
                    ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        ui.vertical(|ui| {
                            // The app's own slider (drag, or click the box
                            // and type), in whole percent.
                            let mut percent = (options.widen * 100.0).round();
                            let track = [70u8, 130, 200].map(egui::Color32::from_gray);
                            crate::ui::widgets::dev_slider_stacked_resp(
                                ui,
                                "Khung rộng hơn mẫu (%)",
                                &mut percent,
                                0.0..=MAX_WIDEN * 100.0,
                                &track,
                                1.0,
                            );
                            options.widen = percent.round() / 100.0;
                            if (options.widen - DEFAULT_WIDEN).abs() > 1e-3
                                && ui
                                    .small_button(format!(
                                        "{} Mặc định",
                                        ph::ARROW_COUNTER_CLOCKWISE
                                    ))
                                    .on_hover_text("Rộng hơn mẫu 10%")
                                    .clicked()
                            {
                                options.widen = DEFAULT_WIDEN;
                            }
                        });
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
                    "Background thành trắng; người tách ra \"Layer 1\" (như Ctrl+J với vùng chọn), bên dưới là \"Ảnh gốc\" (mask đen — tô trắng để lấy lại chi tiết)",
                );
                ui.checkbox(&mut options.then_portrait, "Xong thì mở Chỉnh chân dung");
                ui.add_enabled_ui(options.then_portrait, |ui| {
                    ui.horizontal(|ui| {
                        ui.add_space(22.0);
                        ui.label("Công thức:");
                        // A deleted preset falls back to the defaults.
                        let chosen = presets.iter().find(|p| p.name == preset);
                        egui::ComboBox::from_id_salt(PRESET_KEY)
                            .selected_text(chosen.map_or("Mặc định", |p| p.name.as_str()))
                            .width(170.0)
                            .show_ui(ui, |ui| {
                                if ui.selectable_label(chosen.is_none(), "Mặc định").clicked() {
                                    preset.clear();
                                }
                                for p in &presets {
                                    if ui.selectable_label(p.name == preset, &p.name).clicked() {
                                        preset = p.name.clone();
                                    }
                                }
                            })
                            .response
                            .on_hover_text(
                                "Cắt xong, Chỉnh chân dung mở ra với sẵn công thức này. Sửa, lưu, xóa công thức trong hộp thoại Chỉnh chân dung",
                            );
                    });
                });
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
                    .add_enabled(
                        can_run,
                        egui::Button::new(format!("{}  Làm ảnh thẻ", ph::IDENTIFICATION_CARD)),
                    )
                    .clicked()
                {
                    run = true;
                }
                if ui.button(format!("{}  Đóng", ph::X)).clicked() {
                    close = true;
                }
            });
            ui.add_space(4.0);
        });

    if !open {
        close = true;
    }
    ctx.data_mut(|d| {
        d.insert_temp(id, options);
        d.insert_temp(preset_id, preset.clone());
    });
    if run || close {
        save_options(options);
        save_pref(PRESET_KEY, &preset);
    }
    if run {
        let settings = presets
            .iter()
            .find(|p| options.then_portrait && p.name == preset)
            .map(|p| p.settings);
        actions.dialogs.run_id_photo = Some((options, settings));
    } else if close {
        actions.dialogs.show_id_photo_dialog = Some(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opt-in: IAI_UI_SNAPSHOT is a folder; the dialog is drawn into
    /// `anh_the.png` there.
    #[test]
    #[ignore]
    fn probe_dialog_snapshot() {
        let Ok(dir) = std::env::var("IAI_UI_SNAPSHOT") else {
            return;
        };
        let data = UiData::default();
        let image = crate::ui::snapshot::render(520.0, 420.0, 1.5, None, |ctx| {
            ctx.data_mut(|d| {
                d.insert_temp(
                    egui::Id::new("portrait_presets"),
                    crate::core::portrait::presets::built_in(),
                );
                d.insert_temp(egui::Id::new(PRESET_KEY), "Ảnh thẻ nữ".to_string());
                d.insert_temp(egui::Id::new("id_photo_options"), IdPhotoOptions::default());
            });
            let mut actions = UiActions::default();
            id_photo_dialog(ctx, &data, &mut actions);
        });
        image
            .save(std::path::Path::new(&dir).join("anh_the.png"))
            .unwrap();
    }

    #[test]
    fn the_dialog_draws_with_its_presets_and_asks_for_nothing_untouched() {
        let ctx = egui::Context::default();
        // What the portrait dialog holds in memory; prefs.json is not written.
        ctx.data_mut(|d| {
            d.insert_temp(
                egui::Id::new("portrait_presets"),
                crate::core::portrait::presets::built_in(),
            );
            d.insert_temp(egui::Id::new(PRESET_KEY), "Ảnh thẻ nữ".to_string());
            d.insert_temp(egui::Id::new("id_photo_options"), IdPhotoOptions::default());
        });
        let data = UiData::default();
        let mut actions = UiActions::default();
        for _ in 0..2 {
            let _ = ctx.run_ui(Default::default(), |ui| {
                id_photo_dialog(ui.ctx(), &data, &mut actions);
            });
        }
        assert!(actions.dialogs.run_id_photo.is_none());
        assert!(actions.dialogs.show_id_photo_dialog.is_none());
        let kept: String = ctx
            .data_mut(|d| d.get_temp(egui::Id::new(PRESET_KEY)))
            .unwrap();
        assert_eq!(kept, "Ảnh thẻ nữ");
    }
}

//! "Model AI" (Help menu): each feature with the models it runs on, their
//! size and state on this machine ("✓ đã kiểm" / "✓ tự đặt" / "✗ thiếu"),
//! "Tải" for those the app fetches, "Mở link tải" / "Xem video" for those
//! the person gets by hand, the files that cannot be used and why, and a
//! line to send the author when every source fails (A3, A5, A6).

use egui_phosphor::regular as ph;
use serde_json::json;

use iai_ai::model_store::status::State;

use crate::ui::{UiActions, UiData};

const READY: egui::Color32 = egui::Color32::from_rgb(120, 200, 120);
const WANTING: egui::Color32 = egui::Color32::from_rgb(230, 150, 90);

/// A state as the window says it: its mark (the font has no ✓ / ✗ of the
/// model store's text) and words.
fn state_label(state: &State) -> (String, egui::Color32) {
    let (mark, words, colour) = match state {
        State::Verified => (ph::CHECK_CIRCLE, "đã kiểm".to_string(), READY),
        State::Custom => (ph::CHECK_CIRCLE, "tự đặt".to_string(), READY),
        State::Missing => (ph::X_CIRCLE, "thiếu".to_string(), WANTING),
        State::Unusable(why) => (ph::X_CIRCLE, why.clone(), WANTING),
    };
    (format!("{mark} {words}"), colour)
}

pub(crate) fn models_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let Some(view) = data.chrome.models.as_ref() else {
        return;
    };
    let busy = data.chrome.models_busy;
    let small = |text: &str| egui::RichText::new(text).size(11.0);
    let mut open = true;
    egui::Window::new("Model AI")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(560.0)
        .show(ctx, |ui| {
            ui.label(small(&format!(
                "Thư mục model chính: {}",
                view.main_folder.display()
            )));
            ui.horizontal(|ui| {
                if ui
                    .button(format!("{} Mở thư mục model", ph::FOLDER_OPEN))
                    .clicked()
                {
                    actions.run("models.reveal", json!({}));
                }
                if ui
                    .add_enabled(
                        !busy,
                        egui::Button::new(format!("{} Cài model từ file…", ph::DOWNLOAD_SIMPLE)),
                    )
                    .on_hover_text(
                        "Chọn file model .onnx (hoặc file .zip gói model): app tự nhận ra model và chép vào đúng chỗ. Kéo thả file vào cửa sổ iAi cũng được",
                    )
                    .clicked()
                {
                    actions.run("models.install", json!({}));
                }
            });
            if view.scanning {
                ui.label(small("Đang xem các thư mục model…"));
            }
            if busy {
                ui.label(small("Đang tải / cài model — tiến độ ở dòng trạng thái bên dưới"));
            }
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(440.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for feature in &view.features {
                        let (mark, colour) = if feature.ready {
                            (ph::CHECK_CIRCLE, READY)
                        } else {
                            (ph::X_CIRCLE, WANTING)
                        };
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(mark).color(colour));
                            ui.label(egui::RichText::new(feature.label).strong());
                        });
                        for row in &feature.models {
                            ui.horizontal_wrapped(|ui| {
                                ui.add_space(12.0);
                                ui.label(&row.name);
                                ui.label(small(&row.size_text));
                                let (state, colour) = state_label(&row.state);
                                ui.label(small(&state).color(colour));
                                if row.state.usable() {
                                    return;
                                }
                                match &row.download {
                                    Ok(()) => {
                                        if ui
                                            .add_enabled(!busy, egui::Button::new("Tải"))
                                            .on_hover_text(
                                                "App tự tải từ nguồn của tác giả model, không được thì từ kho dự phòng; file tải về được kiểm mã",
                                            )
                                            .clicked()
                                        {
                                            actions.run("models.fetch", json!({"id": row.id}));
                                        }
                                    }
                                    Err(why) => {
                                        ui.label(small(why));
                                    }
                                }
                                if let Some(page) = &row.page
                                    && ui.small_button("Mở link tải").clicked()
                                {
                                    ctx.open_url(egui::OpenUrl::new_tab(page));
                                }
                                if let Some(video) = &row.video
                                    && ui.small_button("Xem video").clicked()
                                {
                                    ctx.open_url(egui::OpenUrl::new_tab(video));
                                }
                                if ui
                                    .small_button("Chép lời nhắn")
                                    .on_hover_text(
                                        "Tải mọi nguồn đều hỏng: bấm để chép lời nhắn (tên model, cỡ, mã) rồi dán gửi tác giả qua Zalo / Facebook để nhận file",
                                    )
                                    .clicked()
                                {
                                    let list = iai_ai::model_store::list::current();
                                    ctx.copy_text(iai_ai::model_store::status::message_for_author(
                                        row, &list,
                                    ));
                                }
                            });
                        }
                        for custom in &feature.custom {
                            let name = custom
                                .path
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            ui.horizontal_wrapped(|ui| {
                                ui.add_space(12.0);
                                ui.label(format!("{name} ({})", custom.size_text));
                                let (state, colour) = state_label(&State::Custom);
                                ui.label(small(&state).color(colour));
                            });
                        }
                        ui.add_space(4.0);
                    }
                    if !view.problems.is_empty() {
                        ui.separator();
                        ui.label(egui::RichText::new("File trong thư mục model không dùng được").strong());
                        for (path, why) in &view.problems {
                            let name = path
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            ui.label(small(&format!("{name}: {why}")));
                        }
                    }
                });
            ui.separator();
            ui.label(small(
                "Model cùng loại tải về từ nơi khác: chép vào thư mục model (để trong thư mục con nào, đặt tên gì cũng được) — mở lại cửa sổ này là thấy.",
            ));
        });
    if !open {
        actions.chrome.close_support = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opt-in: IAI_UI_SNAPSHOT is a folder; the window, as this machine's
    /// model folders stand, is drawn into `model_ai.png` there.
    #[test]
    #[ignore]
    fn probe_models_snapshot() {
        let Ok(dir) = std::env::var("IAI_UI_SNAPSHOT") else {
            return;
        };
        iai_ai::model_store::scan_now();
        let mut data = UiData::default();
        data.chrome.models = Some(std::sync::Arc::new(iai_ai::model_store::status::overview()));
        let image = crate::ui::snapshot::render(640.0, 760.0, 1.5, None, |ctx| {
            crate::ui::theme::apply_theme(ctx, crate::ui::theme::ThemeMode::default());
            let mut actions = UiActions::default();
            models_dialog(ctx, &data, &mut actions);
        });
        let _ = std::fs::create_dir_all(&dir);
        image
            .save(std::path::Path::new(&dir).join("model_ai.png"))
            .expect("the snapshot is written");
    }
}

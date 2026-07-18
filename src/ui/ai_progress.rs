use super::{UiActions, UiData};

/// Non-modal progress card for the active document, independent of the AI panel.
pub fn build(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let Some(busy) = data
        .doc
        .doc_ai_busy
        .get(data.doc.active_doc_idx)
        .and_then(Option::as_ref)
    else {
        return;
    };

    let other_jobs = data
        .doc
        .doc_ai_busy
        .iter()
        .enumerate()
        .filter(|(index, job)| *index != data.doc.active_doc_idx && job.is_some())
        .count();

    egui::Area::new(egui::Id::new("ai_progress_overlay"))
        .order(egui::Order::Foreground)
        .anchor(
            egui::Align2::RIGHT_BOTTOM,
            egui::vec2(-(data.chrome.panel_r_w + 12.0), -34.0),
        )
        .show(ctx, |ui| {
            let visuals = ui.visuals();
            egui::Frame::new()
                .fill(visuals.panel_fill)
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    visuals.widgets.noninteractive.bg_stroke.color,
                ))
                .corner_radius(7.0)
                .inner_margin(egui::Margin::symmetric(12, 9))
                .show(ui, |ui| {
                    ui.set_max_width(330.0);
                    ui.horizontal(|ui| {
                        if let Some(pos) = busy.queue_pos {
                            ui.label(
                                egui::RichText::new(egui_phosphor::regular::CLOCK)
                                    .size(16.0)
                                    .color(ui.visuals().weak_text_color()),
                            );
                            ui.label(format!("Đang chờ đến lượt ({}) — vị trí {pos}", busy.label));
                        } else {
                            ui.add(egui::Spinner::new().size(16.0));
                            ui.label(format!(
                                "Đang xử lý ({})… {}s",
                                busy.label,
                                busy.elapsed_secs.unwrap_or(0)
                            ));
                        }

                        if ui.small_button("Hủy").clicked() {
                            actions.ai.ai_cancel_active = true;
                        }
                    });

                    if other_jobs > 0 {
                        ui.label(
                            egui::RichText::new(format!(
                                "+{other_jobs} lệnh khác đang chạy ở tab khác"
                            ))
                            .small()
                            .weak(),
                        );
                    }
                });
        });
}

// PDF navigator strip — shown below the tab bar when the active document is a
// page of an imported PDF. A whole PDF lives in one document; only the active
// page canvas is resident, so navigation stays cheap for very large files.

use super::{UiActions, UiData};
use egui;

const NAV_H: f32 = 26.0;

pub fn build(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let Some(nav) = data.doc.pdf_nav.clone() else {
        return;
    };
    let pal = data.chrome.theme_mode.palette();
    let count = nav.count.max(1);
    let index = nav.index.min(count - 1);

    // Ctrl+PageUp / Ctrl+PageDown flip pages (common PDF-reader shortcut).
    if ctx.input(|i| i.modifiers.ctrl) {
        if index + 1 < count && ctx.input(|i| i.key_pressed(egui::Key::PageDown)) {
            actions.doc.pdf_nav_goto = Some(index + 1);
        }
        if index > 0 && ctx.input(|i| i.key_pressed(egui::Key::PageUp)) {
            actions.doc.pdf_nav_goto = Some(index - 1);
        }
    }

    #[allow(deprecated)]
    egui::TopBottomPanel::top("pdf_nav")
        .exact_size(NAV_H)
        .frame(
            egui::Frame::new()
                .fill(pal.panel_bg)
                .inner_margin(egui::Margin::symmetric(8, 0)),
        )
        .show(ctx, |ui| {
            // Strict modal lock: page navigation swaps the canvas under the
            // active operation, so it is disabled until commit/cancel.
            let strip = ui.add_enabled_ui(!data.chrome.is_tool_modal, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;

                    ui.label(
                        egui::RichText::new(&nav.source_name)
                            .color(pal.text_dim)
                            .size(11.5),
                    );
                    ui.separator();

                    if ui
                        .add_enabled(index > 0, egui::Button::new("◀").small())
                        .clicked()
                    {
                        actions.doc.pdf_nav_goto = Some(index - 1);
                    }

                    let mut page_number = index + 1;
                    let response = ui.add(
                        egui::DragValue::new(&mut page_number)
                            .range(1..=count)
                            .prefix("Page ")
                            .suffix(format!("/{count}"))
                            .speed(1.0),
                    );
                    if response.changed() {
                        actions.doc.pdf_nav_goto = Some(page_number - 1);
                    }

                    if ui
                        .add_enabled(index + 1 < count, egui::Button::new("▶").small())
                        .clicked()
                    {
                        actions.doc.pdf_nav_goto = Some(index + 1);
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Export PDF").clicked() {
                            actions.doc.pdf_nav_export = true;
                        }
                    });
                });
            });
            if data.chrome.is_tool_modal
                && ui.input(|i| i.pointer.primary_clicked())
                && ui
                    .input(|i| i.pointer.interact_pos())
                    .is_some_and(|pos| strip.response.rect.contains(pos))
            {
                actions.chrome.modal_denied = true;
            }
        });
}

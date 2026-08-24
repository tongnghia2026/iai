// Artboard page-tab bar — a CorelDRAW / Excel-style strip at the bottom of the
// window (just above the status bar) for documents with more than one artboard.
// Nav arrows + "k / N" + an add button, then one tab per artboard; clicking a
// tab (or a nav arrow) frames that artboard in the view. Hidden for a plain
// single-page document so photo editing stays uncluttered.

use super::{UiActions, UiData};
use egui;

const BAR_H: f32 = 26.0;

pub fn build(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let count = data.chrome.artboards.len();
    if count <= 1 {
        return;
    }
    let pal = data.chrome.theme_mode.palette();
    let active = data.doc.active_artboard.min(count - 1);

    #[allow(deprecated)]
    egui::TopBottomPanel::bottom("artboard_tabs")
        .exact_size(BAR_H)
        .frame(
            egui::Frame::new()
                .fill(pal.panel_bg)
                .inner_margin(egui::Margin::symmetric(6, 0)),
        )
        .show(ctx, |ui| {
            // Switching the framed page during a modal op (crop/transform) would
            // move the view out from under it, so the bar is disabled until
            // commit/cancel — mirroring the PDF navigator.
            ui.add_enabled_ui(!data.chrome.is_tool_modal, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;

                    if ui
                        .add_enabled(active > 0, egui::Button::new("◀◀").small())
                        .on_hover_text("Trang đầu")
                        .clicked()
                    {
                        actions.doc.set_active_artboard = Some(0);
                    }
                    if ui
                        .add_enabled(active > 0, egui::Button::new("◀").small())
                        .on_hover_text("Trang trước")
                        .clicked()
                    {
                        actions.doc.set_active_artboard = Some(active - 1);
                    }
                    ui.label(
                        egui::RichText::new(format!("{} / {}", active + 1, count))
                            .color(pal.text_dim)
                            .size(11.5),
                    );
                    if ui
                        .add_enabled(active + 1 < count, egui::Button::new("▶").small())
                        .on_hover_text("Trang sau")
                        .clicked()
                    {
                        actions.doc.set_active_artboard = Some(active + 1);
                    }
                    if ui
                        .add_enabled(active + 1 < count, egui::Button::new("▶▶").small())
                        .on_hover_text("Trang cuối")
                        .clicked()
                    {
                        actions.doc.set_active_artboard = Some(count - 1);
                    }

                    ui.separator();
                    if ui
                        .add(egui::Button::new("+").small())
                        .on_hover_text("Thêm artboard")
                        .clicked()
                    {
                        actions.doc.add_artboard = true;
                    }
                    ui.separator();

                    // One tab per artboard. A horizontal scroll keeps a long row
                    // reachable without stealing the whole strip.
                    egui::ScrollArea::horizontal()
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 2.0;
                                for i in 0..count {
                                    let tab = ui
                                        .selectable_label(i == active, format!("Trang {}", i + 1));
                                    if tab.clicked() {
                                        actions.doc.set_active_artboard = Some(i);
                                    }
                                }
                            });
                        });
                });
            });
        });
}

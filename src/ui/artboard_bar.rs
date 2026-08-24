// Page-tab bar — a CorelDRAW / Excel-style strip at the bottom of the window
// (just above the status bar). It navigates the active document's pages:
//   • a normal document → one tab per artboard page, plus a "+" to add a page;
//   • an imported multi-page PDF → one tab per PDF page (no "+"; page count is
//     fixed), plus an "Export PDF" button — the same strip that used to be a
//     separate navigator across the top, so paging a PDF now feels identical to
//     paging artboards.
// Shown for any open document so the current page is always in view.

use super::{PdfNavData, UiActions, UiData};
use egui;

const BAR_H: f32 = 26.0;

pub fn build(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    if !data.doc.has_doc {
        return;
    }
    let pal = data.chrome.theme_mode.palette();

    // A document that is a page of an imported PDF drives this same bottom strip
    // (previously a separate top navigator), so paging a PDF matches paging
    // artboards. PDF pages are async-rendered on demand, so they keep their own
    // navigation intent rather than sharing the artboard page storage.
    if let Some(nav) = data.doc.pdf_nav.clone() {
        build_pdf(ctx, data, actions, &nav, &pal);
        return;
    }

    let count = data.doc.page_count.max(1);
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
            // commit/cancel.
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
                        .on_hover_text("Thêm trang")
                        .clicked()
                    {
                        actions.doc.add_artboard = true;
                    }
                    ui.separator();

                    // One tab per page. A horizontal scroll keeps a long row
                    // reachable without stealing the whole strip. Right-click a tab
                    // to rename, delete, or reorder it.
                    egui::ScrollArea::horizontal()
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 2.0;
                                for i in 0..count {
                                    page_tab(ui, data, actions, i, active, count);
                                }
                            });
                        });
                });
            });
        });
}

/// One artboard-page tab: a selectable label with a right-click context menu to
/// rename / delete / move the page. `count` is the current page count (guards the
/// move and delete affordances).
fn page_tab(
    ui: &mut egui::Ui,
    data: &UiData,
    actions: &mut UiActions,
    i: usize,
    active: usize,
    count: usize,
) {
    let label = data
        .doc
        .page_names
        .get(i)
        .cloned()
        .unwrap_or_else(|| format!("Trang {}", i + 1));
    let tab = ui.selectable_label(i == active, label);
    if tab.clicked() {
        actions.doc.set_active_artboard = Some(i);
    }
    tab.context_menu(|ui| {
        ui.set_min_width(150.0);
        if ui.button("Đổi tên…").clicked() {
            actions.doc.rename_page = Some(i);
            ui.close();
        }
        ui.add_enabled_ui(i > 0, |ui| {
            if ui.button("◀ Chuyển sang trái").clicked() {
                actions.doc.move_page = Some((i, i - 1));
                ui.close();
            }
        });
        ui.add_enabled_ui(i + 1 < count, |ui| {
            if ui.button("Chuyển sang phải ▶").clicked() {
                actions.doc.move_page = Some((i, i + 1));
                ui.close();
            }
        });
        ui.separator();
        ui.add_enabled_ui(count > 1, |ui| {
            if ui.button("Xoá trang").clicked() {
                actions.doc.delete_page = Some(i);
                ui.close();
            }
        });
    });
}

/// PDF navigator variant of the strip: source name, page nav, an Export button,
/// and one tab per imported page. Clicking a tab pages the PDF (async on-demand
/// render), so it routes through the PDF navigation intent, not artboard storage.
fn build_pdf(
    ctx: &egui::Context,
    data: &UiData,
    actions: &mut UiActions,
    nav: &PdfNavData,
    pal: &crate::ui::theme::Palette,
) {
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
    egui::TopBottomPanel::bottom("artboard_tabs")
        .exact_size(BAR_H)
        .frame(
            egui::Frame::new()
                .fill(pal.panel_bg)
                .inner_margin(egui::Margin::symmetric(6, 0)),
        )
        .show(ctx, |ui| {
            let strip = ui.add_enabled_ui(!data.chrome.is_tool_modal, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;

                    ui.label(
                        egui::RichText::new(&nav.source_name)
                            .color(pal.text_dim)
                            .size(11.5),
                    );
                    ui.separator();

                    if ui
                        .add_enabled(index > 0, egui::Button::new("◀◀").small())
                        .on_hover_text("Trang đầu")
                        .clicked()
                    {
                        actions.doc.pdf_nav_goto = Some(0);
                    }
                    if ui
                        .add_enabled(index > 0, egui::Button::new("◀").small())
                        .on_hover_text("Trang trước")
                        .clicked()
                    {
                        actions.doc.pdf_nav_goto = Some(index - 1);
                    }
                    ui.label(
                        egui::RichText::new(format!("{} / {}", index + 1, count))
                            .color(pal.text_dim)
                            .size(11.5),
                    );
                    if ui
                        .add_enabled(index + 1 < count, egui::Button::new("▶").small())
                        .on_hover_text("Trang sau")
                        .clicked()
                    {
                        actions.doc.pdf_nav_goto = Some(index + 1);
                    }
                    if ui
                        .add_enabled(index + 1 < count, egui::Button::new("▶▶").small())
                        .on_hover_text("Trang cuối")
                        .clicked()
                    {
                        actions.doc.pdf_nav_goto = Some(count - 1);
                    }

                    ui.separator();
                    if ui
                        .button("Xuất PDF")
                        .on_hover_text("Xuất PDF nhiều trang")
                        .clicked()
                    {
                        actions.doc.pdf_nav_export = true;
                    }
                    ui.separator();

                    // One tab per PDF page; a horizontal scroll keeps a long row
                    // reachable without stealing the whole strip.
                    egui::ScrollArea::horizontal()
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 2.0;
                                for i in 0..count {
                                    if ui
                                        .selectable_label(i == index, format!("Trang {}", i + 1))
                                        .clicked()
                                    {
                                        actions.doc.pdf_nav_goto = Some(i);
                                    }
                                }
                            });
                        });
                });
            });
            // Clicking the strip during a modal op denies it (matches the old
            // top navigator), giving the "finish the current action first" toast.
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

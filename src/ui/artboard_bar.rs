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

// The solid horizontal scrollbar needs its own row below the page tabs. The old
// 26 px strip used egui's floating scrollbar, which expanded over a tab on hover
// and intercepted both left- and right-clicks.
const BAR_H: f32 = 42.0;

fn separate_page_scrollbar(ui: &mut egui::Ui) {
    let scroll = &mut ui.style_mut().spacing.scroll;
    scroll.floating = false;
    scroll.bar_width = 6.0;
    scroll.bar_inner_margin = 6.0;
    scroll.bar_outer_margin = 1.0;
}

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
                    // Master (shared background) is a multi-page workflow, so the
                    // control only appears once there are ≥2 pages (or one exists).
                    if count > 1 || data.doc.has_master {
                        ui.separator();
                        master_controls(ui, data, actions, &pal);
                    }
                    ui.separator();

                    // One tab per page. A horizontal scroll keeps a long row
                    // reachable without stealing the whole strip. Right-click a tab
                    // to rename, delete, or reorder it.
                    ui.scope(|ui| {
                        separate_page_scrollbar(ui);
                        egui::ScrollArea::horizontal()
                            .id_salt("artboard_page_tabs_scroll")
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
        });
}

/// Master (shared background) page controls in the tab bar. Create + edit a
/// document-wide background that shows beneath every page; while editing it, a
/// prominent "done" button returns to the pages.
fn master_controls(
    ui: &mut egui::Ui,
    data: &UiData,
    actions: &mut UiActions,
    pal: &crate::ui::theme::Palette,
) {
    if data.doc.editing_master {
        if ui
            .add(egui::Button::new(egui::RichText::new("✓ Xong nền").strong()).small())
            .on_hover_text("Kết thúc chỉnh trang nền, quay lại trang")
            .clicked()
        {
            actions.doc.toggle_master_edit = true;
        }
        ui.label(
            egui::RichText::new("◆ đang chỉnh TRANG NỀN")
                .color(pal.accent_primary)
                .size(11.5),
        );
    } else if data.doc.has_master {
        if ui
            .add(egui::Button::new("Sửa nền").small())
            .on_hover_text("Chỉnh trang nền dùng chung (hiện dưới mọi trang)")
            .clicked()
        {
            actions.doc.toggle_master_edit = true;
        }
        if ui
            .add(egui::Button::new("🗑").small())
            .on_hover_text("Xoá trang nền")
            .clicked()
        {
            actions.doc.delete_master = true;
        }
    } else if ui
        .add(egui::Button::new("＋ Nền").small())
        .on_hover_text("Tạo trang nền dùng chung cho mọi trang (logo, khung, header/footer…)")
        .clicked()
    {
        actions.doc.toggle_master_edit = true;
    }
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
        if data.doc.has_master {
            ui.separator();
            let uses = data.doc.page_uses_master.get(i).copied().unwrap_or(true);
            let mut u = uses;
            if ui
                .checkbox(&mut u, "Hiện trang nền")
                .on_hover_text("Trang này có hiện nền dùng chung bên dưới không")
                .clicked()
            {
                actions.doc.set_page_use_master = Some((i, u));
                ui.close();
            }
        }
        ui.separator();
        ui.add_enabled_ui(count > 1, |ui| {
            if ui.button("Xoá trang").clicked() {
                actions.doc.delete_page = Some(i);
                ui.close();
            }
        });
    });
}

/// One imported-PDF tab. PDF page order is the compact `selected_pages` list,
/// so rename/delete/reorder remain metadata-only and preserve the source PDF.
/// Every page action lives in the right-click menu; a plain click pages the doc.
fn pdf_page_tab(
    ui: &mut egui::Ui,
    data: &UiData,
    actions: &mut UiActions,
    nav: &PdfNavData,
    i: usize,
    active: usize,
    count: usize,
) {
    let label = nav
        .page_names
        .get(i)
        .cloned()
        .unwrap_or_else(|| format!("Trang {}", i + 1));
    let tab = ui.selectable_label(i == active, label);
    if tab.clicked() {
        actions.doc.pdf_nav_goto = Some(i);
    }
    // A new page / inserted files land right after the tab the menu opened on.
    let active_is_background = data
        .layers
        .layer_is_background
        .get(data.layers.active_layer_idx)
        .copied()
        .unwrap_or(false);
    tab.context_menu(|ui| {
        ui.set_min_width(190.0);
        if ui.button("＋ Thêm trang trắng").clicked() {
            actions.doc.insert_pdf_blank = Some(i + 1);
            ui.close();
        }
        if ui.button("＋ Chèn ảnh/PDF…").clicked() {
            actions.doc.insert_pdf_files = Some(i + 1);
            ui.close();
        }
        ui.separator();
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
        if ui
            .add_enabled(
                data.sel.has_selection && active_is_background,
                egui::Button::new("Xóa vùng · mọi trang"),
            )
            .on_hover_text(
                "Dùng vùng chọn chữ nhật hiện tại để che cùng vị trí trên mọi trang (Shift+Delete)",
            )
            .clicked()
        {
            actions.sel.clear_selection_all_pdf_pages = true;
            ui.close();
        }
        if ui
            .add_enabled(
                nav.global_clear_count > 0,
                egui::Button::new("Hoàn tác xóa mọi trang"),
            )
            .clicked()
        {
            actions.sel.undo_pdf_global_clear = true;
            ui.close();
        }
        if ui
            .add_enabled(
                nav.global_clear_redo_count > 0,
                egui::Button::new("Làm lại xóa mọi trang"),
            )
            .clicked()
        {
            actions.sel.redo_pdf_global_clear = true;
            ui.close();
        }
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
                        .on_hover_text("Mở cửa sổ Xuất PDF")
                        .clicked()
                    {
                        actions.doc.show_pdf_export_dialog = Some(true);
                    }
                    // Page actions (add / insert / rename / reorder / clear /
                    // undo-redo / delete) all live in each tab's right-click menu
                    // to keep this bar lean.
                    ui.label(
                        egui::RichText::new("chuột phải vào trang ⋯")
                            .color(pal.text_dim)
                            .size(11.0),
                    )
                    .on_hover_text(
                        "Chuột phải vào một trang để: thêm trang, chèn ảnh/PDF, đổi tên, \
                         chuyển vị trí, xóa vùng mọi trang, hoàn tác/làm lại, xoá trang",
                    );
                    ui.separator();

                    // One tab per PDF page; a horizontal scroll keeps a long row
                    // reachable without stealing the whole strip.
                    ui.scope(|ui| {
                        separate_page_scrollbar(ui);
                        egui::ScrollArea::horizontal()
                            .id_salt("pdf_page_tabs_scroll")
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 2.0;
                                    for i in 0..count {
                                        pdf_page_tab(ui, data, actions, nav, i, index, count);
                                    }
                                });
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

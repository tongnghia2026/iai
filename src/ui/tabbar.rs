// Document tab bar — shown between the menu bar and the tool-options bar.
// One tab per open document; clicking switches the active document.

use super::{UiActions, UiData};
use egui;
use egui_phosphor::regular as ph;

const TAB_H: f32 = 26.0;
const TAB_MIN_W: f32 = 80.0;
const TAB_MAX_W: f32 = 180.0;
const CLOSE_W: f32 = 18.0;
const NEW_BTN_W: f32 = 28.0;
const DOC_LIST_BTN_W: f32 = 30.0;
const DOC_LIST_ROW_H: f32 = 24.0;
const DOC_LIST_MIN_W: f32 = 260.0;
const DOC_LIST_MAX_W: f32 = 360.0;

pub fn build(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let pal = data.chrome.theme_mode.palette();
    let active = data.doc.active_doc_idx;
    let doc_count = data.doc.doc_count;

    let ctrl = ctx.input(|i| i.modifiers.ctrl);
    if ctrl && ctx.input(|i| i.key_pressed(egui::Key::Tab)) {
        let shift = ctx.input(|i| i.modifiers.shift);
        if doc_count > 1 {
            actions.doc.switch_doc = Some(if shift {
                if active == 0 {
                    doc_count - 1
                } else {
                    active - 1
                }
            } else {
                (active + 1) % doc_count
            });
        }
    }

    #[allow(deprecated)]
    egui::TopBottomPanel::top("tabbar")
        .exact_size(TAB_H)
        .frame(
            egui::Frame::new()
                .fill(pal.app_bg)
                .inner_margin(egui::Margin::symmetric(2, 0)),
        )
        .show(ctx, |ui| {
            // Strict modal lock: document navigation is disabled while a
            // modal operation is open; clicking the strip rings the bell.
            let strip = ui.add_enabled_ui(!data.chrome.is_tool_modal, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.spacing_mut().button_padding = egui::vec2(0.0, 0.0);

                    // Reserve the document menu from the right before laying out
                    // tabs, so it remains reachable when the tab row overflows.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        build_document_list(ctx, ui, data, actions);

                        // Keep the document list fixed while the tabs themselves
                        // slide underneath the available strip.  A newly appended
                        // document is revealed at the right edge automatically;
                        // selecting an older document reveals that tab again.
                        let tabs_size = egui::vec2(ui.available_width(), TAB_H);
                        ui.allocate_ui_with_layout(
                            tabs_size,
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                egui::ScrollArea::horizontal()
                                    .id_salt("document_tab_scroll")
                                    .scroll_bar_visibility(
                                        egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                                    )
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| build_tabs(ui, data, actions));
                            },
                        );
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

fn build_tabs(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    let pal = data.chrome.theme_mode.palette();
    let count_key = egui::Id::new("document_tab_previous_count");
    let active_key = egui::Id::new("document_tab_previous_active");
    let previous_count = ui
        .ctx()
        .data(|d| d.get_temp::<usize>(count_key))
        .unwrap_or(0);
    let previous_active = ui
        .ctx()
        .data(|d| d.get_temp::<usize>(active_key))
        .unwrap_or(data.doc.active_doc_idx);
    let documents_appended = data.doc.doc_count > previous_count;
    let active_changed = data.doc.active_doc_idx != previous_active;

    for i in 0..data.doc.doc_count {
        let title = data
            .doc
            .doc_titles
            .get(i)
            .map(|s| s.as_str())
            .unwrap_or("Untitled");
        let dirty = data.doc.doc_modified.get(i).copied().unwrap_or(false);
        let is_active = i == data.doc.active_doc_idx;
        let ai_busy = data.doc.doc_ai_busy.get(i).and_then(Option::as_ref);

        let display = if dirty {
            format!("{}*", title)
        } else {
            title.to_string()
        };
        let label_text = truncate_title(&display, 18);
        let tab_fill = if is_active { pal.panel_bg } else { pal.app_bg };
        let text_color = if is_active { pal.text } else { pal.text_dim };
        let font_id = egui::FontId::proportional(11.5);
        let text_w = 7.0_f32 * label_text.len() as f32;
        let busy_w = if ai_busy.is_some() { 14.0 } else { 0.0 };
        let tab_w = (text_w + CLOSE_W + busy_w + 20.0).clamp(TAB_MIN_W, TAB_MAX_W);

        let (tab_rect, tab_response) =
            ui.allocate_exact_size(egui::vec2(tab_w, TAB_H), egui::Sense::click());

        // Appends take precedence so the newest filename is always visible on
        // the right.  Otherwise follow explicit tab/keyboard navigation.
        if documents_appended && i + 1 == data.doc.doc_count {
            tab_response.scroll_to_me(Some(egui::Align::Max));
        } else if !documents_appended && active_changed && is_active {
            tab_response.scroll_to_me(Some(egui::Align::Center));
        }
        let painter = ui.painter();
        painter.rect_filled(tab_rect, 0.0, tab_fill);

        if is_active {
            painter.hline(
                tab_rect.left()..=tab_rect.right(),
                tab_rect.bottom() - 2.0,
                egui::Stroke::new(2.0_f32, pal.accent_primary),
            );
        }

        if !is_active || i + 1 < data.doc.doc_count {
            painter.vline(
                tab_rect.right(),
                tab_rect.top()..=tab_rect.bottom(),
                egui::Stroke::new(1.0_f32, pal.separator),
            );
        }

        let label_rect = egui::Rect::from_min_size(
            egui::pos2(tab_rect.left() + 8.0, tab_rect.top()),
            egui::vec2(tab_w - CLOSE_W - busy_w - 10.0, TAB_H),
        );
        painter.text(
            label_rect.left_center(),
            egui::Align2::LEFT_CENTER,
            &label_text,
            font_id,
            text_color,
        );

        if let Some(busy) = ai_busy {
            let badge_rect = egui::Rect::from_min_size(
                egui::pos2(tab_rect.right() - CLOSE_W - busy_w, tab_rect.top()),
                egui::vec2(busy_w, TAB_H),
            );
            let badge_resp = ui.interact(
                badge_rect,
                egui::Id::new(("tab_ai_busy", i)),
                egui::Sense::hover(),
            );
            if let Some(pos) = busy.queue_pos {
                painter.text(
                    badge_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    ph::CLOCK,
                    egui::FontId::proportional(12.0),
                    pal.text_dim,
                );
                badge_resp.on_hover_text(format!("Trong hàng chờ (vị trí {pos})"));
            } else {
                let font = egui::FontId::proportional(12.0);
                let galley =
                    painter.layout_no_wrap(ph::CIRCLE_NOTCH.to_string(), font, pal.accent_primary);
                let pos = badge_rect.center() - galley.size() * 0.5;
                let angle = ui.ctx().input(|input| input.time as f32 * 4.0);
                painter.add(
                    egui::epaint::TextShape::new(pos, galley, pal.accent_primary)
                        .with_angle_and_anchor(angle, egui::Align2::CENTER_CENTER),
                );
                badge_resp.on_hover_text(format!(
                    "Đang xử lý ({})… {}s",
                    busy.label,
                    busy.elapsed_secs.unwrap_or(0)
                ));
            }
        }

        let close_rect = egui::Rect::from_min_size(
            egui::pos2(tab_rect.right() - CLOSE_W, tab_rect.top()),
            egui::vec2(CLOSE_W, TAB_H),
        );
        let close_id = egui::Id::new(("tab_close", i));
        let close_resp = ui.interact(close_rect, close_id, egui::Sense::click());
        let close_col = if close_resp.hovered() {
            pal.danger
        } else if is_active {
            pal.text_dim
        } else {
            pal.separator
        };
        painter.text(
            close_rect.center(),
            egui::Align2::CENTER_CENTER,
            ph::X,
            egui::FontId::proportional(13.0),
            close_col,
        );
        if close_resp.clicked() {
            actions.doc.close_doc_tab = Some(i);
        }

        if tab_response.clicked() && !close_resp.clicked() && !is_active {
            actions.doc.switch_doc = Some(i);
        }
    }

    let (new_rect, new_resp) =
        ui.allocate_exact_size(egui::vec2(NEW_BTN_W, TAB_H), egui::Sense::click());
    let new_fill = if new_resp.hovered() {
        pal.button_hover
    } else {
        pal.app_bg
    };
    ui.painter().rect_filled(new_rect, 0.0, new_fill);
    ui.painter().text(
        new_rect.center(),
        egui::Align2::CENTER_CENTER,
        ph::PLUS,
        egui::FontId::proportional(16.0),
        pal.text_dim,
    );
    if new_resp.clicked() {
        actions.doc.new_doc_tab = true;
    }

    ui.ctx().data_mut(|d| {
        d.insert_temp(count_key, data.doc.doc_count);
        d.insert_temp(active_key, data.doc.active_doc_idx);
    });
}

fn build_document_list(
    ctx: &egui::Context,
    ui: &mut egui::Ui,
    data: &UiData,
    actions: &mut UiActions,
) {
    let pal = data.chrome.theme_mode.palette();
    // The popup frame has 6 px of vertical margin on each side. Fill all
    // remaining space down to the top edge of the status bar.
    let available_height =
        (ctx.content_rect().bottom() - ui.max_rect().bottom() - super::statusbar::HEIGHT - 12.0)
            .max(DOC_LIST_ROW_H);
    let button = egui::Button::new(
        egui::RichText::new(ph::CARET_DOWN)
            .size(12.0)
            .color(pal.text_dim),
    )
    .min_size(egui::vec2(DOC_LIST_BTN_W, TAB_H))
    .fill(pal.app_bg)
    .corner_radius(0.0);

    // Scroll to the active document only on the frame the popup opens, so a
    // long list starts centered on it without fighting later user scrolling.
    let open_flag = egui::Id::new("doc_list_was_open");
    let was_open = ctx.data(|d| d.get_temp::<bool>(open_flag)).unwrap_or(false);

    let (response, menu_inner) =
        egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| {
            ui.set_min_width(DOC_LIST_MIN_W);
            ui.set_max_width(DOC_LIST_MAX_W);

            egui::ScrollArea::vertical()
                .id_salt("document_list_scroll")
                .max_height(available_height)
                .min_scrolled_height(available_height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_min_width(DOC_LIST_MIN_W);

                    for i in 0..data.doc.doc_count {
                        let title = data
                            .doc
                            .doc_titles
                            .get(i)
                            .map(|s| s.as_str())
                            .unwrap_or("Untitled");
                        let dirty = data.doc.doc_modified.get(i).copied().unwrap_or(false);
                        let is_active = i == data.doc.active_doc_idx;
                        let display = if dirty {
                            format!("{} *", title)
                        } else {
                            title.to_string()
                        };

                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let close = ui
                                        .add_sized(
                                            [DOC_LIST_ROW_H, DOC_LIST_ROW_H],
                                            egui::Button::new(ph::X).frame(false),
                                        )
                                        .on_hover_text(format!("Close {}", title));

                                    let row_width = ui.available_width();
                                    let label = truncate_title(&display, 42);
                                    let label = if is_active {
                                        egui::RichText::new(label).strong().color(pal.text)
                                    } else {
                                        egui::RichText::new(label)
                                    };
                                    let select = ui
                                        .add_sized(
                                            [row_width, DOC_LIST_ROW_H],
                                            egui::Button::new(())
                                                .left_text(label)
                                                .selected(is_active)
                                                .frame_when_inactive(false),
                                        )
                                        .on_hover_text(title);

                                    if is_active {
                                        let row = select.rect.union(close.rect);
                                        let painter = ui.painter();
                                        painter.rect_stroke(
                                            row,
                                            2.0,
                                            egui::Stroke::new(1.0_f32, pal.accent_primary),
                                            egui::StrokeKind::Inside,
                                        );
                                        painter.rect_filled(
                                            egui::Rect::from_min_max(
                                                row.min,
                                                egui::pos2(row.min.x + 3.0, row.max.y),
                                            ),
                                            0.0,
                                            pal.accent_primary,
                                        );
                                        if !was_open {
                                            select.scroll_to_me(Some(egui::Align::Center));
                                        }
                                    }

                                    if close.clicked() {
                                        actions.doc.close_doc_tab = Some(i);
                                        ui.close();
                                    } else if select.clicked() {
                                        if !is_active {
                                            actions.doc.switch_doc = Some(i);
                                        }
                                        ui.close();
                                    }
                                },
                            );
                        });
                    }
                });
        });

    ctx.data_mut(|d| d.insert_temp(open_flag, menu_inner.is_some()));

    response.on_hover_text("All open documents");
}

fn truncate_title(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = chars[..max_chars - 1].iter().collect();
        format!("{}…", truncated)
    }
}

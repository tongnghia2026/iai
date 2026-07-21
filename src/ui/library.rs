//! The Library grid browser (Track B, B3): pick a folder, scan it for images
//! (RAW + common rasters), and show a scrollable wall of thumbnails. Cards
//! single-click to select (Ctrl-click toggles, Shift-click ranges from the
//! anchor, Ctrl+A selects all), double-click to open; the header opens the
//! selection in bulk and toggles back to the editor.
//!
//! Thumbnails come from the shared `ThumbCache` (built for the welcome screen).
//! Only the cards visible in the viewport are reported back for generation, so
//! a large folder never floods the generator.

use super::{UiActions, UiData};
use egui;
use std::path::PathBuf;

const BG: egui::Color32 = egui::Color32::from_rgb(18, 20, 26);
const ACCENT: egui::Color32 = egui::Color32::from_rgb(30, 100, 215);
const CARD_W: f32 = 150.0;
const THUMB_H: f32 = 108.0;
const CARD_H: f32 = THUMB_H + 26.0;
const GAP: f32 = 12.0;

#[allow(deprecated)]
pub fn build(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    // Paths whose cards fall inside the viewport this frame → the app requests
    // their thumbnails (see App::handle_library_actions).
    let mut visible: Vec<PathBuf> = Vec::new();

    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(BG))
        .show(ctx, |ui| {
            header(ui, data, actions);

            if data.library.folder.is_none() {
                empty_prompt(ui, "Choose a folder to browse its photos", Some(actions));
            } else if data.library.entries.is_empty() {
                empty_prompt(ui, "No images in this folder", None);
            } else {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(6.0);
                        grid(ui, data, actions, &mut visible);
                        ui.add_space(GAP);
                    });
            }
        });

    actions.library.visible_thumbs = visible;
}

/// Pinned top bar: Choose Folder · folder path · Open Selected · Editor toggle.
fn header(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.spacing_mut().item_spacing.x = 8.0;

        if ui
            .add(
                egui::Button::new(egui::RichText::new("  Choose Folder  ").size(13.0))
                    .fill(egui::Color32::from_rgb(42, 44, 54))
                    .min_size(egui::vec2(0.0, 30.0)),
            )
            .clicked()
        {
            actions.library.open_folder = true;
        }

        let selected = data.library.selected_count;
        if selected > 0
            && ui
                .add(
                    egui::Button::new(
                        egui::RichText::new(format!("  Open Selected ({selected})  "))
                            .size(13.0)
                            .color(egui::Color32::WHITE),
                    )
                    .fill(ACCENT)
                    .min_size(egui::vec2(0.0, 30.0)),
                )
                .clicked()
        {
            actions.library.open_selected = true;
        }
        if selected > 0
            && ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("Deselect")
                            .size(12.0)
                            .color(egui::Color32::from_gray(170)),
                    )
                    .frame(false),
                )
                .clicked()
        {
            actions.library.clear_selection = true;
        }

        // Right-aligned "Editor" toggle, then the folder path filling the middle.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(12.0);
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("  Editor  ")
                            .size(13.0)
                            .color(egui::Color32::from_gray(210)),
                    )
                    .fill(egui::Color32::from_rgb(42, 44, 54))
                    .min_size(egui::vec2(0.0, 30.0)),
                )
                .on_hover_text("Back to the editor")
                .clicked()
            {
                actions.chrome.show_library = Some(false);
            }

            if let Some(folder) = &data.library.folder {
                ui.add_space(8.0);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(folder)
                            .size(11.5)
                            .color(egui::Color32::from_gray(120)),
                    )
                    .truncate(),
                )
                .on_hover_text(folder);
            }
        });
    });
    ui.add_space(8.0);
    ui.separator();
}

/// Centred prompt for the two empty states. When `actions` is given, a big
/// "Choose Folder" button is offered (the no-folder-yet state).
fn empty_prompt(ui: &mut egui::Ui, msg: &str, actions: Option<&mut UiActions>) {
    ui.add_space(60.0);
    ui.vertical_centered(|ui| {
        ui.label(
            egui::RichText::new(msg)
                .size(14.0)
                .color(egui::Color32::from_gray(120)),
        );
        if let Some(actions) = actions {
            ui.add_space(16.0);
            if ui
                .add(
                    egui::Button::new(egui::RichText::new("  Choose Folder  ").size(13.0))
                        .fill(ACCENT)
                        .min_size(egui::vec2(150.0, 40.0)),
                )
                .clicked()
            {
                actions.library.open_folder = true;
            }
        }
    });
}

/// Responsive thumbnail grid: as many columns as the width allows (min 2).
fn grid(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions, visible: &mut Vec<PathBuf>) {
    let avail = ui.available_width();
    let cols = (((avail + GAP) / (CARD_W + GAP)).floor() as usize).clamp(2, 12);
    // Shift = range-select from the anchor; Ctrl/Cmd = toggle; plain = replace.
    let (ctrl, shift) = ui.input(|i| (i.modifiers.command || i.modifiers.ctrl, i.modifiers.shift));
    let kind = if shift {
        crate::ui::LibrarySelect::Range
    } else if ctrl {
        crate::ui::LibrarySelect::Toggle
    } else {
        crate::ui::LibrarySelect::Replace
    };
    for row in data.library.entries.chunks(cols) {
        ui.horizontal(|ui| {
            ui.add_space(GAP);
            for (i, item) in row.iter().enumerate() {
                if i > 0 {
                    ui.add_space(GAP);
                }
                card(ui, item, kind, actions, visible);
            }
        });
        ui.add_space(GAP);
    }
}

fn card(
    ui: &mut egui::Ui,
    item: &crate::ui::LibraryEntry,
    select_kind: crate::ui::LibrarySelect,
    actions: &mut UiActions,
    visible: &mut Vec<PathBuf>,
) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(CARD_W, CARD_H), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        visible.push(item.path.clone());
    }
    // Stable, path-derived id so a press and release land on the same widget even
    // as thumbnails stream in and shift nothing (same reasoning as the welcome grid).
    let resp = ui.interact(
        rect,
        egui::Id::new(("library_card", item.path.as_path())),
        egui::Sense::click(),
    );
    let hovered = resp.hovered();
    let painter = ui.painter().clone();

    let fill = if item.selected {
        egui::Color32::from_rgb(34, 52, 84)
    } else if hovered {
        egui::Color32::from_rgb(40, 44, 56)
    } else {
        egui::Color32::from_rgb(26, 28, 36)
    };
    painter.rect_filled(rect, 6.0, fill);
    let (stroke_c, stroke_w) = if item.selected {
        (ACCENT, 2.0)
    } else {
        (egui::Color32::from_gray(if hovered { 72 } else { 38 }), 1.0)
    };
    painter.rect_stroke(
        rect,
        6.0,
        egui::Stroke::new(stroke_w, stroke_c),
        egui::StrokeKind::Inside,
    );

    let thumb_rect = egui::Rect::from_min_size(rect.min, egui::vec2(CARD_W, THUMB_H));
    match item.thumb {
        Some((id, size)) => {
            let pad = 6.0;
            let avail = egui::vec2(CARD_W - pad * 2.0, THUMB_H - pad * 2.0);
            let scale = (avail.x / size.x.max(1.0)).min(avail.y / size.y.max(1.0));
            let img_rect = egui::Rect::from_center_size(thumb_rect.center(), size * scale);
            painter.image(
                id,
                img_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        None => {
            painter.rect_filled(
                thumb_rect.shrink(6.0),
                4.0,
                egui::Color32::from_rgb(20, 22, 28),
            );
            painter.text(
                thumb_rect.center(),
                egui::Align2::CENTER_CENTER,
                "…",
                egui::FontId::proportional(18.0),
                egui::Color32::from_gray(90),
            );
        }
    }

    painter.text(
        egui::pos2(rect.center().x, thumb_rect.max.y + 13.0),
        egui::Align2::CENTER_CENTER,
        short_name(&item.name, 20),
        egui::FontId::proportional(10.5),
        egui::Color32::from_gray(if item.selected || hovered { 225 } else { 195 }),
    );

    // Double-click opens; a single click selects (plain = only this, Ctrl =
    // toggle, Shift = range from the anchor). On a double-click egui reports the
    // first click as a select and the second as the open — the desired feel.
    if resp.double_clicked() {
        actions.library.open_entry = Some(item.path.clone());
    } else if resp.clicked() {
        actions.library.select_entry = Some((item.path.clone(), select_kind));
    }
    resp.on_hover_text(item.path.to_string_lossy().to_string());
}

/// Truncate a file name to `max` characters, appending an ellipsis when cut.
fn short_name(name: &str, max: usize) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= max {
        return name.to_string();
    }
    let head: String = chars[..max.saturating_sub(1)].iter().collect();
    format!("{head}…")
}

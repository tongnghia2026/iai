use super::{UiActions, UiData};
use egui;

fn load_logo(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let cache_id = egui::Id::new("iai_welcome_logo_tex");
    let cached = ctx.data(|d| d.get_temp::<egui::TextureHandle>(cache_id));
    if let Some(tex) = cached {
        return Some(tex);
    }
    let bytes = include_bytes!("../../logo_iAi.png");
    // Clamp the logo's side to a size every GPU accepts (older cards cap texture
    // sides at 2048) before uploading it as a texture.
    let src = image::load_from_memory(bytes).ok()?;
    let src = if src.width().max(src.height()) > 512 {
        src.resize(512, 512, image::imageops::FilterType::Lanczos3)
    } else {
        src
    };
    let img = src.into_rgba8();
    let (w, h) = img.dimensions();
    let color_image =
        egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw());
    let tex = ctx.load_texture(
        "iai_welcome_logo",
        color_image,
        egui::TextureOptions::LINEAR,
    );
    ctx.data_mut(|d| d.insert_temp(cache_id, tex.clone()));
    Some(tex)
}

#[allow(deprecated)]
pub fn build(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let logo_tex = load_logo(ctx);
    let bg = egui::Color32::from_rgb(18, 20, 26);

    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(bg))
        .show(ctx, |ui| {
            // "Back to Canvas" pinned as a thin footer so the recent grid gets
            // the whole middle of the screen.
            egui::TopBottomPanel::bottom("welcome_footer")
                .frame(egui::Frame::new().fill(bg))
                .show_separator_line(false)
                .show_inside(ui, |ui| {
                    ui.add_space(5.0);
                    ui.vertical_centered(|ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Back to Canvas")
                                        .size(11.0)
                                        .color(egui::Color32::from_gray(70)),
                                )
                                .frame(false),
                            )
                            .clicked()
                        {
                            actions.chrome.show_welcome = Some(false);
                        }
                    });
                    ui.add_space(5.0);
                });

            // Compact header sitting near the top.
            ui.add_space(16.0);
            ui.vertical_centered(|ui| {
                if let Some(ref tex) = logo_tex {
                    let tex_size = tex.size_vec2();
                    let logo_h = 88.0_f32;
                    let logo_w = logo_h * tex_size.x / tex_size.y;
                    let (strip_rect, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), logo_h),
                        egui::Sense::hover(),
                    );
                    let logo_rect = egui::Rect::from_center_size(
                        strip_rect.center(),
                        egui::vec2(logo_w, logo_h),
                    );
                    ui.painter().image(
                        tex.id(),
                        logo_rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                } else {
                    ui.label(
                        egui::RichText::new("iAi")
                            .size(44.0)
                            .strong()
                            .color(egui::Color32::from_rgb(0, 140, 255)),
                    );
                }

                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(concat!(
                        "v",
                        env!("CARGO_PKG_VERSION"),
                        "  ·  Open Source Image Editor"
                    ))
                    .size(11.0)
                    .color(egui::Color32::from_gray(90)),
                );

                ui.add_space(16.0);

                ui.horizontal(|ui| {
                    let btn_w = 140.0_f32;
                    let gap = 12.0_f32;
                    let total_btn = btn_w * 3.0 + gap * 2.0;
                    let avail = ui.available_width();
                    ui.add_space(((avail - total_btn) / 2.0).max(0.0));

                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new("  New Canvas  ").size(13.0))
                                .fill(egui::Color32::from_rgb(30, 100, 215))
                                .min_size(egui::vec2(btn_w, 40.0)),
                        )
                        .clicked()
                    {
                        actions.dialogs.show_new_dialog = Some(true);
                    }

                    ui.add_space(gap);

                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("  Open File  ")
                                    .size(13.0)
                                    .color(egui::Color32::from_gray(210)),
                            )
                            .fill(egui::Color32::from_rgb(42, 44, 54))
                            .min_size(egui::vec2(btn_w, 40.0)),
                        )
                        .clicked()
                    {
                        actions.doc.open_file = true;
                    }

                    ui.add_space(gap);

                    // Browse Folder → open the folder picker; a chosen folder
                    // switches to the Library grid (Track B).
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("  Browse Folder  ")
                                    .size(13.0)
                                    .color(egui::Color32::from_gray(210)),
                            )
                            .fill(egui::Color32::from_rgb(42, 44, 54))
                            .min_size(egui::vec2(btn_w, 40.0)),
                        )
                        .on_hover_text("Open a folder of photos in the Library")
                        .clicked()
                    {
                        actions.library.open_folder = true;
                    }
                });

                ui.add_space(18.0);
                ui.label(
                    egui::RichText::new("Recent Files")
                        .size(11.0)
                        .color(egui::Color32::from_gray(75)),
                );
                ui.add_space(8.0);
            });

            // Recent grid fills the remaining height: a responsive, scrollable
            // wall of thumbnails (as many columns as the width allows).
            if data.welcome.recent.is_empty() {
                ui.add_space(18.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new("No recent files")
                            .color(egui::Color32::from_gray(55))
                            .size(12.0),
                    );
                });
            } else {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(2.0);
                        recent_grid(ui, &data.welcome.recent, actions);
                    });
            }
        });
}

/// Recent-files grid on the welcome screen (Track B): centred rows of clickable
/// thumbnail cards, as many columns as the width allows. Shows every recent
/// entry; the caller wraps it in a scroll area.
fn recent_grid(ui: &mut egui::Ui, recent: &[crate::ui::RecentThumb], actions: &mut UiActions) {
    const CARD_W: f32 = 116.0;
    const GAP: f32 = 12.0;
    // Fit as many columns as the available width allows (min 2).
    let avail = ui.available_width();
    let cols = (((avail + GAP) / (CARD_W + GAP)).floor() as usize).clamp(2, 10);
    for row in recent.chunks(cols) {
        let row_w = CARD_W * row.len() as f32 + GAP * row.len().saturating_sub(1) as f32;
        ui.horizontal(|ui| {
            ui.add_space(((avail - row_w) / 2.0).max(0.0));
            for (i, item) in row.iter().enumerate() {
                if i > 0 {
                    ui.add_space(GAP);
                }
                recent_card(ui, item, actions);
            }
        });
        ui.add_space(GAP);
    }
}

fn recent_card(ui: &mut egui::Ui, item: &crate::ui::RecentThumb, actions: &mut UiActions) {
    let card_w = 116.0_f32;
    let thumb_h = 78.0_f32;
    let card_h = thumb_h + 30.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(card_w, card_h), egui::Sense::hover());
    // Interact with a stable, path-derived id (not the positional auto-id) so a
    // press and release land on the same widget even as thumbnails stream in.
    let resp = ui.interact(
        rect,
        egui::Id::new(("welcome_recent_card", item.path.as_path())),
        egui::Sense::click(),
    );
    let hovered = resp.hovered();
    let painter = ui.painter().clone();
    painter.rect_filled(
        rect,
        6.0,
        if hovered {
            egui::Color32::from_rgb(40, 44, 56)
        } else {
            egui::Color32::from_rgb(26, 28, 36)
        },
    );
    painter.rect_stroke(
        rect,
        6.0,
        egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_gray(if hovered { 72 } else { 38 }),
        ),
        egui::StrokeKind::Inside,
    );

    let thumb_rect = egui::Rect::from_min_size(rect.min, egui::vec2(card_w, thumb_h));
    match item.thumb {
        Some((id, size)) => {
            let pad = 6.0;
            let avail = egui::vec2(card_w - pad * 2.0, thumb_h - pad * 2.0);
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
        egui::pos2(rect.center().x, thumb_rect.max.y + 9.0),
        egui::Align2::CENTER_CENTER,
        short_name(&item.name, 16),
        egui::FontId::proportional(10.5),
        egui::Color32::from_gray(if hovered { 225 } else { 200 }),
    );
    painter.text(
        egui::pos2(rect.center().x, thumb_rect.max.y + 21.0),
        egui::Align2::CENTER_CENTER,
        format!("{}×{}", item.dims.0, item.dims.1),
        egui::FontId::proportional(9.0),
        egui::Color32::from_gray(110),
    );

    if resp.clicked() {
        actions.doc.open_recent = Some(item.path.clone());
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

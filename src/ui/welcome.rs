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

    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(egui::Color32::from_rgb(18, 20, 26)))
        .show(ctx, |ui| {
            let total_h = ui.available_height();
            let top_pad = ((total_h - 500.0) / 2.0).max(20.0);
            ui.add_space(top_pad);

            ui.vertical_centered(|ui| {
                if let Some(ref tex) = logo_tex {
                    let tex_size = tex.size_vec2();
                    let logo_h = 175.0_f32;
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
                            .size(64.0)
                            .strong()
                            .color(egui::Color32::from_rgb(0, 140, 255)),
                    );
                }

                ui.add_space(14.0);

                ui.label(
                    egui::RichText::new("v0.1.0  ·  Open Source Image Editor")
                        .size(12.0)
                        .color(egui::Color32::from_gray(90)),
                );

                ui.add_space(36.0);

                ui.horizontal(|ui| {
                    let btn_w = 160.0_f32;
                    let gap = 14.0_f32;
                    let total_btn = btn_w * 2.0 + gap;
                    let avail = ui.available_width();
                    ui.add_space(((avail - total_btn) / 2.0).max(0.0));

                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new("  New Canvas  ").size(14.0))
                                .fill(egui::Color32::from_rgb(30, 100, 215))
                                .min_size(egui::vec2(btn_w, 46.0)),
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
                                    .size(14.0)
                                    .color(egui::Color32::from_gray(210)),
                            )
                            .fill(egui::Color32::from_rgb(42, 44, 54))
                            .min_size(egui::vec2(btn_w, 46.0)),
                        )
                        .clicked()
                    {
                        actions.doc.open_file = true;
                    }
                });

                ui.add_space(32.0);

                ui.label(
                    egui::RichText::new("Recent Files")
                        .size(11.0)
                        .color(egui::Color32::from_gray(75)),
                );
                ui.add_space(6.0);

                let sep_w = 340.0_f32;
                let sep_avail = ui.available_width();
                ui.horizontal(|ui| {
                    ui.add_space(((sep_avail - sep_w) / 2.0).max(0.0));
                    ui.add_sized(
                        egui::vec2(sep_w, 1.0),
                        egui::Separator::default().horizontal(),
                    );
                });

                ui.add_space(8.0);

                if data.doc.current_file.is_none() {
                    ui.label(
                        egui::RichText::new("No recent files")
                            .color(egui::Color32::from_gray(55))
                            .size(12.0),
                    );
                }

                ui.add_space(32.0);

                ui.horizontal(|ui| {
                    let card_w = 130.0_f32;
                    let card_gap = 12.0_f32;
                    let total_cards = (card_w + 24.0) * 3.0 + card_gap * 2.0;
                    let avail = ui.available_width();
                    ui.add_space(((avail - total_cards) / 2.0).max(0.0));

                    feature_card(
                        ui,
                        card_w,
                        "Open Source",
                        "Free forever\nContribute on GitHub",
                    );
                    ui.add_space(card_gap);
                    feature_card(
                        ui,
                        card_w,
                        "Cross Platform",
                        "Windows · Linux · macOS\nRust + wgpu",
                    );
                    ui.add_space(card_gap);
                    feature_card(
                        ui,
                        card_w,
                        "Extensible",
                        "Plugin API coming\nMicrokernel design",
                    );
                });

                ui.add_space(28.0);

                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Back to Canvas")
                                .size(11.0)
                                .color(egui::Color32::from_gray(65)),
                        )
                        .frame(false),
                    )
                    .clicked()
                {
                    actions.chrome.show_welcome = Some(false);
                }
            });
        });
}

fn feature_card(ui: &mut egui::Ui, width: f32, title: &str, desc: &str) {
    egui::Frame::new()
        .fill(egui::Color32::from_rgb(26, 28, 36))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .corner_radius(8.0)
        .stroke(egui::Stroke::new(1.0_f32, egui::Color32::from_gray(38)))
        .show(ui, |ui| {
            ui.set_min_width(width);
            ui.set_max_width(width);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(title)
                        .size(12.0)
                        .strong()
                        .color(egui::Color32::from_gray(210)),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(desc)
                        .size(10.0)
                        .color(egui::Color32::from_gray(95)),
                );
            });
        });
}

//! "Danh sách nhà tài trợ": the organizations and the people the author
//! thanks, as his news names them (`crate::author_news`). Nothing reaches
//! the list but through that file.

use super::*;
use crate::author_news::Logo;
use crate::ui::SupportDialog;

const SPONSORS_DIALOG_WIDTH: f32 = 480.0;
/// The side of the tile an organization's logo sits on, in points.
const LOGO_TILE: f32 = 56.0;
/// What the dialog takes of the screen's height around its list.
const SPONSORS_DIALOG_CHROME: f32 = 190.0;

pub(crate) const SPONSORS_TITLE: &str = "Danh sách nhà tài trợ";
const SPONSORS_THANKS: &str =
    "Cảm ơn những người và đơn vị đã ủng hộ để phần chính của iAi tiếp tục miễn phí.";
const NOBODY_YET: &str = "Chưa có tên nào ở đây. Bạn có thể là người đầu tiên.";
const NOT_LOADED: &str = "Chưa tải được danh sách. Hãy kiểm tra mạng rồi mở lại app.";
const ME_TOO: &str = "Tôi cũng muốn ủng hộ";

/// `logo` as a texture; made once for each copy of the news.
fn logo_texture(
    ctx: &egui::Context,
    name: &str,
    logo: &Logo,
    revision: u64,
) -> egui::TextureHandle {
    let cache = egui::Id::new(("sponsor_logo", name, revision));
    if let Some(texture) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(cache)) {
        return texture;
    }
    let image = egui::ColorImage::from_rgba_unmultiplied(logo.size, &logo.rgba);
    let texture = ctx.load_texture(
        format!("sponsor_logo_{name}"),
        image,
        egui::TextureOptions::LINEAR,
    );
    ctx.data_mut(|d| d.insert_temp(cache, texture.clone()));
    texture
}

/// How the anonymous are counted under the names, or by themselves.
fn anonymous_line(anonymous: u32, named: bool) -> Option<String> {
    match (anonymous, named) {
        (0, _) => None,
        (n, true) => Some(format!("và {n} người ẩn danh")),
        (n, false) => Some(format!("{n} người ủng hộ ẩn danh")),
    }
}

pub(crate) fn sponsors_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let mut close = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    let loaded = &data.chrome.news;
    let sponsors = &loaded.news.sponsors;
    let weak = |text: &str| egui::RichText::new(text).weak();
    let room = (ctx.content_rect().height() - SPONSORS_DIALOG_CHROME).max(120.0);

    modal_overlay(ctx, "sponsors_dialog_overlay");

    egui::Window::new(SPONSORS_TITLE)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.set_width(SPONSORS_DIALOG_WIDTH);
            ui.add_space(6.0);
            ui.label(SPONSORS_THANKS);
            egui::ScrollArea::vertical()
                .max_height(room)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    let named = !sponsors.people.is_empty();
                    let anonymous = anonymous_line(sponsors.anonymous, named);
                    if sponsors.organizations.is_empty() && !named && anonymous.is_none() {
                        ui.add_space(12.0);
                        ui.label(weak(if loaded.known { NOBODY_YET } else { NOT_LOADED }));
                    }
                    if !sponsors.organizations.is_empty() {
                        group_heading(ui, "Đơn vị tài trợ");
                    }
                    for org in &sponsors.organizations {
                        ui.horizontal(|ui| {
                            let (tile, _) = ui.allocate_exact_size(
                                egui::Vec2::splat(LOGO_TILE),
                                egui::Sense::hover(),
                            );
                            let logo = org
                                .logo
                                .as_deref()
                                .and_then(|name| Some((name, loaded.logos.get(name)?)));
                            match logo {
                                // White under it: a logo is drawn for paper.
                                Some((name, logo)) => {
                                    ui.painter().rect_filled(tile, 6.0, egui::Color32::WHITE);
                                    let texture = logo_texture(ctx, name, logo, loaded.revision);
                                    let fit = tile.shrink(4.0);
                                    let size = texture.size_vec2();
                                    let scale = (fit.width() / size.x).min(fit.height() / size.y);
                                    ui.painter().image(
                                        texture.id(),
                                        egui::Rect::from_center_size(fit.center(), size * scale),
                                        egui::Rect::from_min_max(
                                            egui::Pos2::ZERO,
                                            egui::pos2(1.0, 1.0),
                                        ),
                                        egui::Color32::WHITE,
                                    );
                                }
                                // None, or one that did not come: its initial.
                                None => {
                                    let visuals = ui.visuals();
                                    ui.painter().rect_filled(
                                        tile,
                                        6.0,
                                        visuals.widgets.inactive.bg_fill,
                                    );
                                    let initial: String = org
                                        .name
                                        .chars()
                                        .next()
                                        .map(|c| c.to_uppercase().collect())
                                        .unwrap_or_default();
                                    ui.painter().text(
                                        tile.center(),
                                        egui::Align2::CENTER_CENTER,
                                        initial,
                                        crate::ui::theme::bold_font(ctx, 24.0),
                                        visuals.strong_text_color(),
                                    );
                                }
                            }
                            ui.add_space(4.0);
                            ui.vertical(|ui| {
                                ui.add_space(if org.contact.is_empty() { 18.0 } else { 9.0 });
                                let size = egui::TextStyle::Body.resolve(ui.style()).size;
                                let font = crate::ui::theme::bold_font(ctx, size);
                                ui.label(egui::RichText::new(&org.name).font(font).strong());
                                if !org.contact.is_empty() {
                                    ui.label(weak(&org.contact));
                                }
                            });
                        });
                        ui.add_space(4.0);
                    }
                    if named || anonymous.is_some() {
                        group_heading(ui, "Cá nhân");
                    }
                    if named {
                        ui.add(egui::Label::new(sponsors.people.join("   ·   ")).wrap());
                    }
                    if let Some(anonymous) = anonymous {
                        ui.label(weak(&anonymous));
                    }
                });

            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui.button(format!("{}  {ME_TOO}", ph::HAND_HEART)).clicked() {
                    actions.chrome.open_support = Some(SupportDialog::Support);
                }
                close |= ui.button("  Đóng  ").clicked();
            });
        });

    if close {
        actions.chrome.close_support = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::author_news::{Loaded, Organization};
    use std::sync::Arc;

    /// News naming two organizations, one with its logo, people and some
    /// who gave without a name.
    fn sample() -> Loaded {
        let mut loaded = Loaded {
            known: true,
            revision: 1,
            ..Default::default()
        };
        // A logo wider than it is tall, as most are.
        let (w, h) = (96, 48);
        let rgba: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let stripe = (i % w) / 12 % 2 == 0;
                if stripe {
                    [200, 40, 40, 255]
                } else {
                    [250, 250, 250, 255]
                }
            })
            .collect();
        loaded.logos.insert(
            "hoa_mai.png".to_string(),
            Logo {
                size: [w, h],
                rgba: Arc::new(rgba),
            },
        );
        let sponsors = &mut loaded.news.sponsors;
        sponsors.organizations = vec![
            Organization {
                name: "Tiệm ảnh Hoa Mai".to_string(),
                contact: "0900 000 000 · 12 Lê Lợi, Huế".to_string(),
                logo: Some("hoa_mai.png".to_string()),
            },
            Organization {
                name: "công ty In Nhanh".to_string(),
                contact: String::new(),
                logo: Some("chua_ve.png".to_string()),
            },
        ];
        sponsors.people = ["Nguyễn Văn A", "Trần Thị B", "Lê C"]
            .map(str::to_string)
            .to_vec();
        sponsors.anonymous = 4;
        loaded
    }

    /// What the dialog draws for `loaded`, and what a click on `label` asks.
    fn shown(loaded: Loaded, click: Option<&str>) -> (Vec<String>, Option<UiActions>) {
        let mut data = UiData::default();
        data.chrome.support = Some(SupportDialog::Sponsors);
        data.chrome.news = Arc::new(loaded);
        let ctx = egui::Context::default();
        let frame = |events: Vec<egui::Event>| {
            let mut actions = UiActions::default();
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 700.0),
                )),
                events,
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| sponsors_dialog(ui.ctx(), &data, &mut actions));
            (output, actions)
        };
        for _ in 0..3 {
            frame(vec![]);
        }
        let texts: Vec<(String, egui::Pos2)> = frame(vec![])
            .0
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) => Some((text.galley.text().to_owned(), text.pos)),
                _ => None,
            })
            .collect();
        let asked = click.map(|label| {
            let at = texts
                .iter()
                .find(|(text, _)| text.trim().ends_with(label))
                .map(|(_, pos)| *pos + egui::vec2(6.0, 5.0))
                .unwrap_or_else(|| panic!("no {label}"));
            let press = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            frame(vec![egui::Event::PointerMoved(at)]);
            frame(vec![press(true)]);
            frame(vec![press(false)]).1
        });
        (texts.into_iter().map(|(text, _)| text).collect(), asked)
    }

    #[test]
    fn the_list_names_organizations_then_people_and_counts_the_anonymous() {
        let (said, _) = shown(sample(), None);
        let at = |what: &str| {
            said.iter()
                .position(|text| text.contains(what))
                .unwrap_or_else(|| panic!("no {what} in {said:?}"))
        };
        assert!(at(SPONSORS_THANKS) < at("Đơn vị tài trợ"));
        assert!(at("Đơn vị tài trợ") < at("Tiệm ảnh Hoa Mai"));
        assert!(at("Tiệm ảnh Hoa Mai") < at("0900 000 000"));
        // One whose logo did not come is listed under its initial.
        assert!(at("công ty In Nhanh") < at("Cá nhân"));
        assert!(said.iter().any(|text| text == "C"));
        assert!(at("Cá nhân") < at("Nguyễn Văn A   ·   Trần Thị B   ·   Lê C"));
        assert!(at("Lê C") < at("và 4 người ẩn danh"));
        assert!(
            !said
                .iter()
                .any(|text| text == NOBODY_YET || text == NOT_LOADED)
        );
    }

    #[test]
    fn an_empty_list_says_whether_it_is_empty_or_did_not_come() {
        let known = Loaded {
            known: true,
            ..Default::default()
        };
        let (said, _) = shown(known, None);
        assert!(said.iter().any(|text| text == NOBODY_YET));
        assert!(!said.iter().any(|text| text.contains("Cá nhân")));
        let (said, _) = shown(Loaded::default(), None);
        assert!(said.iter().any(|text| text == NOT_LOADED));

        assert_eq!(anonymous_line(0, true), None);
        assert_eq!(
            anonymous_line(2, true).as_deref(),
            Some("và 2 người ẩn danh")
        );
        assert_eq!(
            anonymous_line(2, false).as_deref(),
            Some("2 người ủng hộ ẩn danh")
        );
        // Only the anonymous: they are the list.
        let mut only = Loaded {
            known: true,
            ..Default::default()
        };
        only.news.sponsors.anonymous = 2;
        let (said, _) = shown(only, None);
        assert!(said.iter().any(|text| text == "2 người ủng hộ ẩn danh"));
        assert!(!said.iter().any(|text| text == NOBODY_YET));
    }

    #[test]
    fn the_dialog_leads_back_to_the_ways_to_support_or_closes() {
        let (_, asked) = shown(sample(), Some(ME_TOO));
        assert_eq!(
            asked.unwrap().chrome.open_support,
            Some(SupportDialog::Support)
        );
        let (_, asked) = shown(sample(), Some("Đóng"));
        assert!(asked.unwrap().chrome.close_support);
    }

    /// Opt-in: IAI_UI_SNAPSHOT is a folder; the dialog is drawn there with a
    /// sample list and with none, `nha_tai_tro_<kind>.png`.
    #[test]
    #[ignore]
    fn probe_dialog_snapshot_of_the_sponsors_dialog() {
        let Ok(dir) = std::env::var("IAI_UI_SNAPSHOT") else {
            return;
        };
        let known = Loaded {
            known: true,
            ..Default::default()
        };
        for (name, loaded) in [("mau", sample()), ("trong", known)] {
            let mut data = UiData::default();
            data.chrome.support = Some(SupportDialog::Sponsors);
            data.chrome.news = Arc::new(loaded);
            let image = crate::ui::snapshot::render(680.0, 520.0, 1.5, None, |ctx| {
                #[allow(deprecated)]
                egui::CentralPanel::default().show(ctx, |_| {});
                sponsors_dialog(ctx, &data, &mut UiActions::default());
            });
            image
                .save(std::path::Path::new(&dir).join(format!("nha_tai_tro_{name}.png")))
                .unwrap();
        }
    }
}

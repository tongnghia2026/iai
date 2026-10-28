//! The update dialog: a newer build of the app is published, is on its way,
//! has come (see `crate::update`). Also what the title bar's button says.

use super::*;
use crate::ui::{UpdateStage, UpdateView};

const UPDATE_DIALOG_WIDTH: f32 = 380.0;
/// The list of what is new scrolls past this height.
const NEWS_HEIGHT: f32 = 260.0;

/// What the update brings, entry by entry, before the person decides.
fn news_list(ui: &mut egui::Ui, news: &[crate::update::News]) {
    if news.is_empty() {
        return;
    }
    ui.add_space(8.0);
    ui.label(egui::RichText::new("Bản mới có gì").strong());
    egui::ScrollArea::vertical()
        .id_salt("update_news")
        .max_height(NEWS_HEIGHT)
        .show(ui, |ui| {
            for entry in news {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(&entry.date).small().weak());
                for point in &entry.points {
                    ui.horizontal_top(|ui| {
                        ui.label("•");
                        ui.add(egui::Label::new(point).wrap());
                    });
                }
            }
        });
}

/// What the title bar's button says while the update has something to show.
pub(crate) fn update_button_label(update: &UpdateView) -> Option<String> {
    match &update.stage {
        UpdateStage::Available => Some("Có bản mới".to_string()),
        UpdateStage::Downloading(done) => Some(format!("Đang tải bản mới {:.0}%", done * 100.0)),
        UpdateStage::Ready => Some("Khởi động lại để cập nhật".to_string()),
        UpdateStage::DownloadFailed(_) => Some("Chưa tải được bản mới".to_string()),
        // Answers to a question the user asked: said in the dialog alone.
        UpdateStage::Idle
        | UpdateStage::Checking
        | UpdateStage::Current
        | UpdateStage::CheckFailed(_) => None,
    }
}

/// The button's colour. The app is all greys, so news stands out in green
/// and a failure in amber; a download on its way stays quiet.
pub(crate) fn update_button_fill(
    update: &UpdateView,
    pal: crate::ui::theme::Palette,
) -> egui::Color32 {
    match update.stage {
        UpdateStage::DownloadFailed(_) | UpdateStage::CheckFailed(_) => pal.warning,
        UpdateStage::Downloading(_) => pal.accent_primary,
        _ => pal.success,
    }
}

/// The text on the button: dark, since every one of its fills is light.
pub(crate) const UPDATE_BUTTON_TEXT: egui::Color32 = egui::Color32::from_rgb(24, 24, 24);

pub(crate) fn update_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let update = &data.chrome.update;
    // Enter starts nothing here: the dialog can come up by itself.
    let mut close = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));

    modal_overlay(ctx, "update_dialog_overlay");

    egui::Window::new("Cập nhật iAi")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.set_width(UPDATE_DIALOG_WIDTH);
            ui.add_space(8.0);
            let weak = |text: String| egui::RichText::new(text).small().weak();
            match &update.stage {
                UpdateStage::Checking => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Đang kiểm tra bản mới…");
                    });
                }
                UpdateStage::Idle | UpdateStage::Current => {
                    ui.label(egui::RichText::new("Bạn đang dùng bản mới nhất.").strong());
                    ui.label(weak(format!("Bản đang dùng: {}", update.this_day)));
                    ui.add_space(8.0);
                    ui.label(SUPPORT_ASK);
                }
                UpdateStage::Available => {
                    ui.label(egui::RichText::new("Đã có bản iAi mới.").strong());
                    ui.label(weak(format!(
                        "Bản mới: {}   ·   Bản đang dùng: {}",
                        update.new_day, update.this_day
                    )));
                    news_list(ui, &update.news);
                    ui.add_space(8.0);
                    ui.label(format!(
                        "Bấm «Cập nhật»: app tải bản mới (khoảng {} MB) rồi tự mở lại. \
                         Ảnh chưa lưu sẽ được hỏi lưu trước khi app đóng.",
                        update.megabytes
                    ));
                }
                UpdateStage::Downloading(done) => {
                    ui.label(egui::RichText::new("Đang tải bản mới…").strong());
                    ui.add_space(4.0);
                    ui.add(egui::ProgressBar::new(*done).show_percentage());
                    ui.add_space(4.0);
                    ui.label(weak(
                        "Có thể bấm «Ẩn» và làm việc tiếp. Tải xong app sẽ hỏi lưu ảnh rồi tự mở lại."
                            .to_string(),
                    ));
                }
                UpdateStage::Ready => {
                    ui.label(egui::RichText::new("Bản mới đã tải xong.").strong());
                    ui.add_space(4.0);
                    ui.label(
                        "Khởi động lại để dùng ngay. Nếu chưa tiện, bản mới sẽ tự được dùng \
                         ở lần mở app sau.",
                    );
                    news_list(ui, &update.news);
                }
                UpdateStage::CheckFailed(why) | UpdateStage::DownloadFailed(why) => {
                    let heading = match update.stage {
                        UpdateStage::CheckFailed(_) => "Chưa kiểm tra được bản mới.",
                        _ => "Chưa tải được bản mới.",
                    };
                    ui.label(egui::RichText::new(heading).strong());
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(why).color(egui::Color32::from_rgb(235, 170, 100)),
                    );
                    ui.add_space(4.0);
                    ui.label(weak("App vẫn chạy bản đang dùng như bình thường.".to_string()));
                }
            }
            ui.add_space(16.0);
            ui.horizontal(|ui| match &update.stage {
                UpdateStage::Available => {
                    if ui.button("  Cập nhật  ").clicked() {
                        actions.chrome.update_download = true;
                    }
                    close |= ui.button("Để sau").clicked();
                }
                UpdateStage::Downloading(_) => close |= ui.button("  Ẩn  ").clicked(),
                UpdateStage::Ready => {
                    if ui.button("  Khởi động lại  ").clicked() {
                        actions.chrome.update_restart = true;
                    }
                    close |= ui.button("Để sau").clicked();
                }
                UpdateStage::CheckFailed(_) => {
                    if ui.button("  Thử lại  ").clicked() {
                        actions.chrome.update_check = true;
                    }
                    close |= ui.button("Đóng").clicked();
                }
                UpdateStage::DownloadFailed(_) => {
                    if ui.button("  Thử lại  ").clicked() {
                        actions.chrome.update_download = true;
                    }
                    close |= ui.button("Đóng").clicked();
                }
                UpdateStage::Idle | UpdateStage::Current => {
                    if ui
                        .button(format!("{}  Ủng hộ tác giả", ph::HAND_HEART))
                        .clicked()
                    {
                        actions.chrome.open_support = Some(crate::ui::SupportDialog::Support);
                    }
                    close |= ui.button("Đóng").clicked();
                }
                UpdateStage::Checking => close |= ui.button("  Đóng  ").clicked(),
            });
        });

    if close {
        actions.chrome.show_update_dialog = Some(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(stage: UpdateStage) -> UiData {
        let mut data = UiData::default();
        data.chrome.update = UpdateView {
            stage,
            show_dialog: true,
            this_day: "07/10/2026 12:53".to_string(),
            new_day: "12/10/2026 10:59".to_string(),
            megabytes: 51,
            news: vec![crate::update::News {
                date: "12/10/2026".to_string(),
                points: vec!["Xếp ảnh in có thêm cỡ 5×5".to_string()],
            }],
        };
        data
    }

    /// The texts the dialog shows for `data`.
    fn texts(data: &UiData) -> Vec<String> {
        let ctx = egui::Context::default();
        let mut shown = Vec::new();
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 700.0),
                )),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| {
                update_dialog(ui.ctx(), data, &mut UiActions::default())
            });
            shown = output
                .shapes
                .iter()
                .filter_map(|clipped| match &clipped.shape {
                    egui::Shape::Text(text) => Some(text.galley.text().to_string()),
                    _ => None,
                })
                .collect();
        }
        shown
    }

    /// The offer says what the new build brings before anything is fetched,
    /// and says nothing of it when the build on offer carries no word.
    #[test]
    fn the_offer_says_what_is_new_before_the_update_is_asked_for() {
        let offered = texts(&view(UpdateStage::Available));
        assert!(offered.iter().any(|t| t == "Bản mới có gì"), "{offered:?}");
        assert!(offered.iter().any(|t| t == "Xếp ảnh in có thêm cỡ 5×5"));
        assert!(offered.iter().any(|t| t.contains("Cập nhật")));
        let mut silent = view(UpdateStage::Available);
        silent.chrome.update.news.clear();
        assert!(!texts(&silent).iter().any(|t| t == "Bản mới có gì"));
    }

    /// The dialog drawn twice, a click on the button labelled `button` sent,
    /// and what that asked for.
    fn click(data: &UiData, button: &str) -> UiActions {
        let ctx = egui::Context::default();
        let draw = |events: Vec<egui::Event>| {
            let mut actions = UiActions::default();
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                events,
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| update_dialog(ui.ctx(), data, &mut actions));
            (output, actions)
        };
        draw(vec![]);
        let (output, _) = draw(vec![]);
        let at = output
            .shapes
            .iter()
            .find_map(|clipped| match &clipped.shape {
                // A button may lead with an icon.
                egui::Shape::Text(text) if text.galley.text().trim().ends_with(button) => {
                    Some(text.pos + egui::vec2(6.0, 5.0))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("no button {button}"));
        let press = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        draw(vec![egui::Event::PointerMoved(at)]);
        draw(vec![press(true)]);
        draw(vec![press(false)]).1
    }

    #[test]
    fn each_stage_offers_its_own_way_on_and_a_way_out() {
        let asked = click(&view(UpdateStage::Available), "Cập nhật");
        assert!(asked.chrome.update_download && asked.chrome.show_update_dialog.is_none());
        let asked = click(&view(UpdateStage::Available), "Để sau");
        assert!(!asked.chrome.update_download);
        assert_eq!(asked.chrome.show_update_dialog, Some(false));

        let asked = click(&view(UpdateStage::Downloading(0.4)), "Ẩn");
        assert_eq!(asked.chrome.show_update_dialog, Some(false));

        let asked = click(&view(UpdateStage::Ready), "Khởi động lại");
        assert!(asked.chrome.update_restart);
        let asked = click(&view(UpdateStage::Ready), "Để sau");
        assert!(!asked.chrome.update_restart);
        assert_eq!(asked.chrome.show_update_dialog, Some(false));

        // A check that failed is asked again; a download that failed is
        // taken up again.
        let unasked = view(UpdateStage::CheckFailed("Không kết nối được".to_string()));
        let asked = click(&unasked, "Thử lại");
        assert!(asked.chrome.update_check && !asked.chrome.update_download);
        let broken = view(UpdateStage::DownloadFailed(
            "Không kết nối được".to_string(),
        ));
        let asked = click(&broken, "Thử lại");
        assert!(asked.chrome.update_download && !asked.chrome.update_check);
        for failed in [unasked, broken] {
            assert_eq!(
                click(&failed, "Đóng").chrome.show_update_dialog,
                Some(false)
            );
        }
        for stage in [UpdateStage::Checking, UpdateStage::Current] {
            assert_eq!(
                click(&view(stage), "Đóng").chrome.show_update_dialog,
                Some(false)
            );
        }
        // Nothing newer: the ways to support the author are one click away.
        let asked = click(&view(UpdateStage::Current), "Ủng hộ tác giả");
        assert_eq!(
            asked.chrome.open_support,
            Some(crate::ui::SupportDialog::Support)
        );
    }

    #[test]
    fn the_title_bar_speaks_only_while_there_is_something_to_do() {
        let label = |stage| update_button_label(&view(stage).chrome.update);
        for quiet in [
            UpdateStage::Idle,
            UpdateStage::Checking,
            UpdateStage::Current,
            UpdateStage::CheckFailed(String::new()),
        ] {
            assert_eq!(label(quiet), None);
        }
        assert_eq!(label(UpdateStage::Available).as_deref(), Some("Có bản mới"));
        assert_eq!(
            label(UpdateStage::Downloading(0.456)).as_deref(),
            Some("Đang tải bản mới 46%")
        );
        assert!(label(UpdateStage::Ready).unwrap().contains("Khởi động lại"));
        assert!(label(UpdateStage::DownloadFailed(String::new())).is_some());
    }

    #[test]
    fn the_title_bar_button_can_be_read_whatever_it_says() {
        // Relative luminance and contrast as WCAG 2 define them.
        let luminance = |c: egui::Color32| {
            let linear = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(c.r()) + 0.7152 * linear(c.g()) + 0.0722 * linear(c.b())
        };
        let contrast = |a: egui::Color32, b: egui::Color32| {
            let (hi, lo) = (
                luminance(a).max(luminance(b)),
                luminance(a).min(luminance(b)),
            );
            (hi + 0.05) / (lo + 0.05)
        };
        let pal = crate::ui::theme::ThemeMode::Dark.palette();
        for stage in [
            UpdateStage::Available,
            UpdateStage::Downloading(0.5),
            UpdateStage::Ready,
            UpdateStage::DownloadFailed(String::new()),
        ] {
            let fill = update_button_fill(&view(stage.clone()).chrome.update, pal);
            // The text on its fill, and the fill on the bar it sits in.
            assert!(
                contrast(UPDATE_BUTTON_TEXT, fill) >= 7.0,
                "{stage:?}: {}",
                contrast(UPDATE_BUTTON_TEXT, fill)
            );
            assert!(contrast(fill, pal.top_bar_bg) >= 3.0, "{stage:?}");
        }
    }

    /// Opt-in: IAI_UI_SNAPSHOT is a folder; every stage of the dialog is
    /// drawn into `cap_nhat_<stage>.png` there.
    #[test]
    #[ignore]
    fn probe_dialog_snapshot_of_the_update_dialog() {
        let Ok(dir) = std::env::var("IAI_UI_SNAPSHOT") else {
            return;
        };
        for (name, stage) in [
            ("kiem_tra", UpdateStage::Checking),
            ("moi_nhat", UpdateStage::Current),
            ("co_ban_moi", UpdateStage::Available),
            ("dang_tai", UpdateStage::Downloading(0.42)),
            ("tai_xong", UpdateStage::Ready),
            (
                "loi_mang",
                UpdateStage::CheckFailed(
                    "Không kết nối được mạng. Kiểm tra mạng rồi thử lại.".to_string(),
                ),
            ),
            (
                "loi_tai",
                UpdateStage::DownloadFailed(
                    "Không kết nối được mạng. Kiểm tra mạng rồi thử lại.".to_string(),
                ),
            ),
        ] {
            let data = view(stage);
            let image = crate::ui::snapshot::render(520.0, 300.0, 1.5, None, |ctx| {
                #[allow(deprecated)]
                egui::CentralPanel::default().show(ctx, |_| {});
                update_dialog(ctx, &data, &mut UiActions::default());
            });
            image
                .save(std::path::Path::new(&dir).join(format!("cap_nhat_{name}.png")))
                .unwrap();
        }
    }
}

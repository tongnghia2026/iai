//! "Ủng hộ tác giả": the ways to support the app's author. Those that cost
//! nothing (his music and his channel on YouTube, his post to pass on), and
//! money: a bank transfer by a QR code drawn for the amount picked and the
//! name given, or PayPal. They come under what is new in this build when the
//! dialog opens after an update. Every link opens in the user's own browser;
//! the author's news (`crate::author_news`) can name newer ones.

use super::*;
use crate::author_mail::Kind;
use crate::core::vietqr;
use crate::ui::SupportDialog;
use egui::emath::GuiRounding;

const SUPPORT_DIALOG_WIDTH: f32 = 520.0;
/// What the dialog takes of the screen's height around its ways to support:
/// its title, the row of "Đóng", the margins.
const SUPPORT_DIALOG_CHROME: f32 = 120.0;
/// A module of the transfer's QR code on screen, in points (rounded to whole
/// pixels), and the modules of white a scanner wants around the code.
const QR_MODULE: f32 = 3.0;
const QR_QUIET: f32 = 4.0;

/// The author's playlist (until his news names another), his channel with
/// the subscribe prompt, and where a donation goes.
const MUSIC_URL: &str = "https://www.youtube.com/watch?v=o7Dvr1311Rg&list=PLUeJVze2qAi0&index=5";
const SUBSCRIBE_URL: &str = "https://www.youtube.com/@TongNghia3999?sub_confirmation=1";
const PAYPAL_URL: &str = "https://paypal.me/TongNghia3999";
pub(super) const BANK: &str = "Ngân hàng ACB";
/// ACB as NAPAS knows it: the id the bank's own code for this account carries.
pub(super) const BANK_ID: &str = "970416";
pub(super) const BANK_ACCOUNT: &str = "18278961";
pub(super) const BANK_HOLDER: &str = "TONG VAN NGHIA";

/// The amounts on offer, in đồng; any other is typed in.
const AMOUNTS: [u64; 3] = [20_000, 50_000, 100_000];
/// What a transfer's message opens with, and how much of the name follows.
const NOTE_LEAD: &str = "iAi ung ho";
const NOTE_NAME_MAX: usize = 30;

/// The dialog's name, on the title bar's button too.
pub(crate) const SUPPORT_TITLE: &str = "Ủng hộ tác giả";
/// The ask itself, also said where a check finds no newer build.
pub(crate) const SUPPORT_ASK: &str = "Phần chính của iAi miễn phí. Nếu app giúp được bạn, hãy ủng hộ tác giả để có kinh phí nâng cấp app.";
const NAME_HINT: &str = "Để trống là ẩn danh";
const SHARE_LABEL: &str = "Giới thiệu cho bạn bè";
const THANKS: &str = "Cảm ơn bạn đã ủng hộ tác giả!";

/// What is new, build by build, as its author wrote it for users.
const WHATS_NEW: &str = include_str!("../../../assets/whats_new.txt");

/// The newest entry of `notes`: its date, and its points.
fn newest_entry(notes: &str) -> (&str, Vec<&str>) {
    let mut lines = notes
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .skip_while(|line| line.is_empty());
    let date = lines.next().unwrap_or_default();
    let points = lines
        .take_while(|line| !line.is_empty())
        .map(|line| line.trim_start_matches('-').trim())
        .collect();
    (date, points)
}

/// The name of a group of ways, over them.
pub(super) fn group_heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(12.0);
    let size = egui::TextStyle::Body.resolve(ui.style()).size;
    let font = crate::ui::theme::bold_font(ui.ctx(), size);
    ui.label(egui::RichText::new(text).font(font).strong());
    ui.add_space(4.0);
}

/// What the user has set in the dialog; kept while the app runs.
#[derive(Clone, PartialEq, Debug)]
struct Gift {
    /// Which of `AMOUNTS`; past them, the amount typed into `other`.
    pick: usize,
    /// Digits only.
    other: String,
    /// The name to show among the sponsors; empty for none.
    name: String,
    /// A way to support was taken up since the dialog opened.
    thanked: bool,
}

impl Default for Gift {
    fn default() -> Self {
        Self {
            pick: 1,
            other: String::new(),
            name: String::new(),
            thanked: false,
        }
    }
}

impl Gift {
    fn id() -> egui::Id {
        egui::Id::new("support_gift")
    }

    /// In đồng; 0 while none is typed, and the payer's bank app asks.
    fn amount(&self) -> u64 {
        match AMOUNTS.get(self.pick) {
            Some(&amount) => amount,
            None => self.other.parse().unwrap_or(0).min(vietqr::MAX_AMOUNT),
        }
    }

    /// The transfer's message: by it the author knows the transfer is for
    /// the app, and under which name to list it.
    fn note(&self) -> String {
        let name = vietqr::plain_note(&self.name, NOTE_NAME_MAX);
        if name.is_empty() {
            NOTE_LEAD.to_string()
        } else {
            format!("{NOTE_LEAD} {name}")
        }
    }

    /// What the QR code says.
    fn code(&self) -> String {
        vietqr::payload(BANK_ID, BANK_ACCOUNT, self.amount(), &self.note())
    }
}

/// `amount` as money is written: "1.250.000 đ".
pub(super) fn dong(amount: u64) -> String {
    let digits = amount.to_string();
    let mut text = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            text.push('.');
        }
        text.push(digit);
    }
    text + " đ"
}

/// The QR code of `text`, a texel to a module; drawn anew when `text` changes.
pub(super) fn qr_texture(ctx: &egui::Context, text: &str) -> Option<egui::TextureHandle> {
    let cache = egui::Id::new("support_qr_tex");
    let drawn = ctx.data(|d| d.get_temp::<(String, egui::TextureHandle)>(cache));
    if let Some((_, texture)) = drawn.filter(|(of, _)| of == text) {
        return Some(texture);
    }
    let code = vietqr::modules(text)?;
    let gray: Vec<u8> = code
        .dark
        .iter()
        .map(|&dark| if dark { 0 } else { 255 })
        .collect();
    let image = egui::ColorImage::from_gray([code.side; 2], &gray);
    let texture = ctx.load_texture("support_qr", image, egui::TextureOptions::NEAREST);
    ctx.data_mut(|d| d.insert_temp(cache, (text.to_owned(), texture.clone())));
    Some(texture)
}

/// The side of the white square the code of `modules` a side is drawn in, and
/// of one module, in points: each module covers whole screen pixels.
pub(super) fn qr_sides(modules: f32, pixels_per_point: f32) -> (f32, f32) {
    let module = (QR_MODULE * pixels_per_point).round().max(1.0) / pixels_per_point;
    ((modules + 2.0 * QR_QUIET) * module, module)
}

/// The transfer's QR code on its white square.
pub(super) fn qr_square(ui: &mut egui::Ui, texture: &egui::TextureHandle) {
    let ppp = ui.ctx().pixels_per_point();
    let modules = texture.size()[0] as f32;
    let (side, module) = qr_sides(modules, ppp);
    let (square, _) = ui.allocate_exact_size(egui::Vec2::splat(side), egui::Sense::hover());
    ui.painter().rect_filled(square, 6.0, egui::Color32::WHITE);
    let code = egui::Rect::from_min_size(
        (square.min + egui::Vec2::splat(QR_QUIET * module)).round_to_pixels(ppp),
        egui::Vec2::splat(modules * module),
    );
    let whole = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
    ui.painter()
        .image(texture.id(), code, whole, egui::Color32::WHITE);
}

pub(crate) fn support_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let Some(kind) = data.chrome.support else {
        return;
    };
    // Esc leaves a field being typed in before it closes the dialog.
    let typing = crate::ui::widgets::typing_in_a_field(ctx, egui::Id::new("support_typing"));
    let esc = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    let mut close = esc && !typing;
    let was = ctx
        .data(|d| d.get_temp::<Gift>(Gift::id()))
        .unwrap_or_default();
    let mut gift = was.clone();
    let news = &data.chrome.news.news;
    let open = |url: &str| ctx.open_url(egui::OpenUrl::new_tab(url));
    let weak = |text: &str| egui::RichText::new(text).small().weak();
    let heading = group_heading;
    // On a low screen the ways scroll, and "Đóng" stays in reach under them.
    let room = (ctx.content_rect().height() - SUPPORT_DIALOG_CHROME).max(160.0);

    modal_overlay(ctx, "support_dialog_overlay");

    egui::Window::new(SUPPORT_TITLE)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.set_width(SUPPORT_DIALOG_WIDTH);
            ui.add_space(6.0);
            egui::ScrollArea::vertical()
                .max_height(room)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    if kind == SupportDialog::Updated {
                        let (date, points) = newest_entry(WHATS_NEW);
                        ui.label(egui::RichText::new("Đã cập nhật xong. Bản mới có:").strong());
                        ui.label(weak(&format!("Bản ngày {date}")));
                        ui.add_space(2.0);
                        for point in points {
                            ui.horizontal_top(|ui| {
                                ui.label("•");
                                ui.add(egui::Label::new(point).wrap());
                            });
                        }
                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(6.0);
                    }
                    ui.label(SUPPORT_ASK);

                    heading(ui, "Ủng hộ không tốn tiền");
                    ui.horizontal_wrapped(|ui| {
                        // The post to pass on comes with the author's news:
                        // until then there is no button for it.
                        let music = news.music_url.as_deref().unwrap_or(MUSIC_URL);
                        let ways = [
                            (ph::MUSIC_NOTES, "Nghe nhạc", Some(music)),
                            (ph::YOUTUBE_LOGO, "Đăng ký kênh", Some(SUBSCRIBE_URL)),
                            (ph::SHARE_NETWORK, SHARE_LABEL, news.share_url.as_deref()),
                        ];
                        for (icon, label, url) in ways {
                            let Some(url) = url else { continue };
                            if ui.button(format!("{icon}  {label}")).clicked() {
                                open(url);
                                gift.thanked = true;
                            }
                        }
                        let report = format!("{}  {}", ph::CHAT_TEXT, Kind::Feedback.label());
                        if ui.button(report).clicked() {
                            actions.chrome.open_support = Some(SupportDialog::Mail(Kind::Feedback));
                        }
                    });
                    ui.label(weak("Nhạc và kênh mở trong trình duyệt của bạn."));

                    heading(ui, "Ủng hộ tiền");
                    let code = gift.code();
                    let texture = qr_texture(ctx, &code);
                    let beside = texture.as_ref().map_or(0.0, |texture| {
                        qr_sides(texture.size()[0] as f32, ctx.pixels_per_point()).0 + 14.0
                    });
                    ui.horizontal_top(|ui| {
                        ui.vertical(|ui| {
                            ui.set_width(SUPPORT_DIALOG_WIDTH - beside);
                            let other = AMOUNTS.len();
                            let mut other_picked = false;
                            ui.horizontal(|ui| {
                                for (i, &amount) in AMOUNTS.iter().enumerate() {
                                    if ui.selectable_label(gift.pick == i, dong(amount)).clicked() {
                                        gift.pick = i;
                                    }
                                }
                                if ui.selectable_label(gift.pick == other, "Số khác").clicked() {
                                    gift.pick = other;
                                    other_picked = true;
                                }
                            });
                            if gift.pick == other {
                                ui.horizontal(|ui| {
                                    let field = ui.add(
                                        egui::TextEdit::singleline(&mut gift.other)
                                            .desired_width(110.0)
                                            .char_limit(9)
                                            .hint_text("Nhập số tiền"),
                                    );
                                    gift.other.retain(|c| c.is_ascii_digit());
                                    // The field is there before it asks for the
                                    // keyboard: asked for sooner, a click's frame
                                    // takes the focus back.
                                    if other_picked {
                                        field.request_focus();
                                    }
                                    ui.label("đ");
                                });
                            }
                            ui.add_space(8.0);
                            ui.label("Tên hiện trong danh sách nhà tài trợ");
                            ui.add(
                                egui::TextEdit::singleline(&mut gift.name)
                                    .desired_width(f32::INFINITY)
                                    .char_limit(40)
                                    .hint_text(NAME_HINT),
                            );
                            // An organization sends its name, a line of
                            // contact and its logo in a form of its own.
                            let sponsor = format!("{}  {}…", ph::BUILDINGS, Kind::Sponsor.label());
                            if ui.small_button(sponsor).clicked() {
                                actions.chrome.open_support =
                                    Some(SupportDialog::Mail(Kind::Sponsor));
                            }
                            ui.add_space(8.0);
                            ui.label(weak("Quét mã bằng app ngân hàng, hoặc chuyển khoản tới:"));
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(BANK_ACCOUNT).strong().size(17.0));
                                ui.label(BANK);
                            });
                            ui.label(BANK_HOLDER);
                            let amount = match gift.amount() {
                                0 => "bạn nhập trong app ngân hàng".to_string(),
                                amount => dong(amount),
                            };
                            ui.label(format!("Số tiền: {amount}"));
                            ui.label(format!("Nội dung: {}", gift.note()));
                            ui.add_space(8.0);
                            if ui.button(format!("{}  PayPal", ph::PAYPAL_LOGO)).clicked() {
                                open(PAYPAL_URL);
                                gift.thanked = true;
                            }
                        });
                        if let Some(texture) = &texture {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                                qr_square(ui, texture);
                            });
                        }
                    });

                    heading(ui, "Liên hệ");
                    ui.horizontal(|ui| {
                        let sponsors = format!("{}  {SPONSORS_TITLE}", ph::USERS_THREE);
                        if ui.button(sponsors).clicked() {
                            actions.chrome.open_support = Some(SupportDialog::Sponsors);
                        }
                        let feature = format!("{}  {}", ph::WRENCH, Kind::Feature.label());
                        if ui.button(feature).clicked() {
                            actions.chrome.open_support = Some(SupportDialog::Mail(Kind::Feature));
                        }
                    });
                });

            ui.add_space(14.0);
            ui.horizontal(|ui| {
                close |= ui.button("  Đóng  ").clicked();
                if gift.thanked {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(format!("{}  {THANKS}", ph::HEART)).strong());
                }
            });
        });

    if close {
        actions.chrome.close_support = true;
        // The thanks are for what was done while the dialog was open.
        gift.thanked = false;
    }
    if gift != was {
        ctx.data_mut(|d| d.insert_temp(Gift::id(), gift));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::dialogs::stage::Stage;

    #[test]
    fn the_newest_entry_is_the_first_block_under_the_notes_to_the_writer() {
        let notes =
            "# to the writer\n# more\n\n12/10/2026\n- one\n-  two \n\n11/10/2026\n- older\n";
        assert_eq!(newest_entry(notes), ("12/10/2026", vec!["one", "two"]));
        assert_eq!(newest_entry("# nothing yet\n"), ("", vec![]));
        // What ships has a dated entry with something in it.
        let (date, points) = newest_entry(WHATS_NEW);
        assert_eq!(date.len(), "12/10/2026".len(), "{date}");
        assert!(
            date.chars().all(|c| c.is_ascii_digit() || c == '/'),
            "{date}"
        );
        assert!(!points.is_empty() && points.iter().all(|p| !p.is_empty()));
    }

    #[test]
    fn the_code_pays_the_amount_picked_under_the_name_given() {
        let mut gift = Gift::default();
        assert_eq!(
            (gift.amount(), gift.note().as_str()),
            (50_000, "iAi ung ho")
        );
        gift.name = "Nguyễn Văn Á".to_string();
        assert_eq!(gift.note(), "iAi ung ho Nguyen Van A");
        assert_eq!(
            gift.code(),
            "00020101021238520010A000000727012200069704160108182789610208QRIBFTTA\
             53037045405500005802VN62270823iAi ung ho Nguyen Van A63044F2A"
        );
        // A name of marks and signs alone is no name.
        gift.name = " …  ".to_string();
        assert_eq!(gift.note(), "iAi ung ho");
        // A long one is cut where the message would grow past what banks take.
        gift.name = "Công ty trách nhiệm hữu hạn một thành viên in ấn".to_string();
        assert_eq!(gift.note(), "iAi ung ho Cong ty trach nhiem huu han mo");

        for (pick, amount) in [(0, 20_000), (2, 100_000)] {
            gift.pick = pick;
            assert_eq!(gift.amount(), amount);
        }
        // Another amount: none yet leaves it to the payer's bank app.
        gift.pick = AMOUNTS.len();
        gift.name.clear();
        assert_eq!(gift.amount(), 0);
        assert_eq!(
            gift.code(),
            vietqr::payload(BANK_ID, BANK_ACCOUNT, 0, "iAi ung ho")
        );
        gift.other = "1250000".to_string();
        assert_eq!(gift.amount(), 1_250_000);
        assert!(gift.code().contains("54071250000"), "{}", gift.code());
        gift.other = "999999999".to_string();
        assert_eq!(gift.amount(), vietqr::MAX_AMOUNT);

        assert_eq!(dong(0), "0 đ");
        assert_eq!(dong(999), "999 đ");
        assert_eq!(dong(20_000), "20.000 đ");
        assert_eq!(dong(1_250_000), "1.250.000 đ");
        assert_eq!(dong(vietqr::MAX_AMOUNT), "499.999.999 đ");
    }

    #[test]
    fn a_module_of_the_code_covers_whole_pixels_at_any_scale() {
        for scale in [1.0, 1.25, 1.5, 1.75, 2.0, 3.0] {
            let (side, module) = qr_sides(53.0, scale);
            let pixels = module * scale;
            assert!((pixels - pixels.round()).abs() < 1e-4, "{scale}: {pixels}");
            assert!(pixels >= 3.0, "{scale}: {pixels}");
            assert!((170.0..=205.0).contains(&side), "{scale}: {side}");
        }
    }

    /// The dialog on a screen of its own.
    fn staged(kind: Option<SupportDialog>) -> Stage {
        staged_on(kind, egui::vec2(900.0, 700.0))
    }

    fn staged_on(kind: Option<SupportDialog>, screen: egui::Vec2) -> Stage {
        let mut data = UiData::default();
        data.chrome.support = kind;
        Stage::on(data, screen, support_dialog)
    }

    #[test]
    fn the_dialog_closes_on_its_button_and_says_what_is_new_only_after_an_update() {
        // Closed: nothing is drawn.
        assert!(staged(None).texts().is_empty());

        let shown = staged(Some(SupportDialog::Support));
        assert!(shown.says(SUPPORT_ASK) && shown.says(BANK_ACCOUNT));
        assert!(!shown.says("cập nhật xong") && !shown.says("khoa học"));
        // No post to pass on yet: no button for it.
        assert!(!shown.says(SHARE_LABEL));
        assert!(shown.click("Đóng").0.chrome.close_support);
        assert!(shown.key(egui::Key::Escape).chrome.close_support);

        // What a build brings can be a long list: on a screen tall enough
        // for it, once the window has grown to it, the ask follows.
        let shown = staged_on(Some(SupportDialog::Updated), egui::vec2(900.0, 2400.0));
        for _ in 0..60 {
            shown.frame(vec![]);
        }
        assert!(shown.says("Đã cập nhật xong") && shown.says(SUPPORT_ASK));
    }

    #[test]
    fn on_a_low_screen_the_ways_scroll_and_the_close_button_stays_in_reach() {
        for kind in [SupportDialog::Support, SupportDialog::Updated] {
            let shown = staged_on(Some(kind), egui::vec2(900.0, 420.0));
            let texts = shown.texts();
            let (_, close) = texts
                .iter()
                .find(|(text, _)| text.trim() == "Đóng")
                .expect("the close button");
            assert!(close.y > 0.0 && close.y < 400.0, "{kind:?}: {close:?}");
            assert!(shown.click("Đóng").0.chrome.close_support);
        }
    }

    #[test]
    fn a_way_taken_up_opens_in_the_browser_and_is_thanked_until_the_dialog_closes() {
        let shown = staged(Some(SupportDialog::Support));
        assert!(!shown.says(THANKS));
        assert_eq!(shown.click("Nghe nhạc").1, vec![MUSIC_URL.to_string()]);
        assert!(shown.says(THANKS));
        assert_eq!(
            shown.click("Đăng ký kênh").1,
            vec![SUBSCRIBE_URL.to_string()]
        );
        assert_eq!(shown.click("PayPal").1, vec![PAYPAL_URL.to_string()]);
        shown.click("Đóng");
        assert!(!shown.says(THANKS));
    }

    #[test]
    fn the_authors_news_names_the_music_and_the_post_to_pass_on() {
        let mut shown = staged(Some(SupportDialog::Support));
        let mut loaded = crate::author_news::Loaded::default();
        loaded.news.music_url = Some("https://www.youtube.com/watch?v=new".to_string());
        loaded.news.share_url = Some("https://www.facebook.com/p/1".to_string());
        shown.data.chrome.news = std::sync::Arc::new(loaded);
        assert_eq!(
            shown.click("Nghe nhạc").1,
            vec!["https://www.youtube.com/watch?v=new".to_string()]
        );
        assert_eq!(
            shown.click(SHARE_LABEL).1,
            vec!["https://www.facebook.com/p/1".to_string()]
        );
        assert!(shown.says(THANKS));
        // The sponsors are a click away, and so is a word to the author.
        assert_eq!(
            shown.click(SPONSORS_TITLE).0.chrome.open_support,
            Some(SupportDialog::Sponsors)
        );
        for kind in [Kind::Feedback, Kind::Feature] {
            assert_eq!(
                shown.click(kind.label()).0.chrome.open_support,
                Some(SupportDialog::Mail(kind))
            );
        }
        let sponsor = format!("{}…", Kind::Sponsor.label());
        assert_eq!(
            shown.click(&sponsor).0.chrome.open_support,
            Some(SupportDialog::Mail(Kind::Sponsor))
        );
    }

    #[test]
    fn the_amount_and_the_name_set_in_the_dialog_are_what_the_code_carries() {
        let shown = staged(Some(SupportDialog::Support));
        let gift = || shown.ctx.data(|d| d.get_temp::<Gift>(Gift::id()));
        assert!(shown.says("Số tiền: 50.000 đ") && shown.says("Nội dung: iAi ung ho"));
        assert!(!shown.says("Số tiền: 100.000 đ"));

        shown.click("100.000 đ");
        assert!(shown.says("Số tiền: 100.000 đ"));

        // The name goes into the message without its marks.
        shown.click(NAME_HINT);
        shown.frame(vec![egui::Event::Text("Nguyễn Văn Á".to_string())]);
        assert!(shown.says("Nội dung: iAi ung ho Nguyen Van A"));
        // Esc leaves the field; the dialog stays, and so does what was set.
        assert!(!shown.key(egui::Key::Escape).chrome.close_support);
        assert!(shown.key(egui::Key::Escape).chrome.close_support);
        assert_eq!(
            gift().unwrap().code(),
            vietqr::payload(BANK_ID, BANK_ACCOUNT, 100_000, "iAi ung ho Nguyen Van A")
        );

        // "Số khác": its field takes the keyboard at once, and digits only.
        shown.click("Số khác");
        assert!(shown.says("Số tiền: bạn nhập trong app ngân hàng"));
        shown.frame(vec![egui::Event::Text("75.000đ".to_string())]);
        assert!(shown.says("Số tiền: 75.000 đ"));
        assert_eq!(gift().unwrap().amount(), 75_000);
        // Back to an amount on offer: the field goes.
        shown.click("20.000 đ");
        assert!(shown.says("Số tiền: 20.000 đ") && !shown.says("75.000"));
    }

    /// Opt-in: IAI_UI_SNAPSHOT is a folder; the dialog is drawn there as it
    /// opens from the Help menu, after an update, with a name and another
    /// amount set, and on a low screen, `ung_ho_<kind>.png`.
    #[test]
    #[ignore]
    fn probe_dialog_snapshot_of_the_support_dialog() {
        let Ok(dir) = std::env::var("IAI_UI_SNAPSHOT") else {
            return;
        };
        let named = Gift {
            pick: 2,
            name: "Nguyễn Văn Á".to_string(),
            thanked: true,
            ..Default::default()
        };
        let other = Gift {
            pick: AMOUNTS.len(),
            other: "250000".to_string(),
            ..Default::default()
        };
        for (name, kind, gift, height) in [
            ("tac_gia", SupportDialog::Support, Gift::default(), 640.0),
            (
                "sau_cap_nhat",
                SupportDialog::Updated,
                Gift::default(),
                640.0,
            ),
            ("co_ten", SupportDialog::Support, named, 640.0),
            ("so_khac", SupportDialog::Support, other, 640.0),
            (
                "man_hinh_thap",
                SupportDialog::Updated,
                Gift::default(),
                440.0,
            ),
        ] {
            let mut data = UiData::default();
            data.chrome.support = Some(kind);
            let image = crate::ui::snapshot::render(680.0, height, 1.5, None, |ctx| {
                ctx.data_mut(|d| d.insert_temp(Gift::id(), gift.clone()));
                #[allow(deprecated)]
                egui::CentralPanel::default().show(ctx, |_| {});
                support_dialog(ctx, &data, &mut UiActions::default());
            });
            image
                .save(std::path::Path::new(&dir).join(format!("ung_ho_{name}.png")))
                .unwrap();
        }
    }
}

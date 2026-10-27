//! A message to the app's author: "Góp ý, báo lỗi", "Cần tính năng riêng",
//! and a sponsoring organization's details for the sponsors list. This is
//! its form and its outcome; `crate::author_mail` sends what is written.

use super::*;
use crate::author_mail::{AUTHOR_EMAIL, Kind, MAX_TEXT, Outgoing, Status};

const MAIL_DIALOG_WIDTH: f32 = 460.0;
/// The colour of a reason something did not go through.
const MAIL_WARNING: egui::Color32 = egui::Color32::from_rgb(235, 170, 100);

const SEND: &str = "Gửi";
const PICK_LOGO: &str = "Chọn logo…";
const WITH_LOGS: &str = "Gửi kèm nhật ký lỗi gần nhất";
const SENT: &str = "Đã gửi tới tác giả. Cảm ơn bạn!";
const NOT_SENT: &str = "Chưa gửi được qua mạng.";
const REVEAL: &str = "Mở thư mục chứa file";

/// What is being written, kept for each kind while the app runs: a dialog
/// closed by mistake has lost nothing.
#[derive(Clone, Default, PartialEq)]
struct Draft {
    name: String,
    contact: String,
    text: String,
    /// The user agrees to send the flight recorder's logs along.
    diagnostics: bool,
}

impl Draft {
    fn id(kind: Kind) -> egui::Id {
        egui::Id::new(("author_mail_draft", kind))
    }

    /// What still has to be written before it can go, in the user's words.
    fn missing(&self, kind: Kind) -> Option<&'static str> {
        let empty = |text: &str| text.trim().is_empty();
        match kind {
            Kind::Sponsor if empty(&self.name) => Some("Hãy ghi tên công ty, tổ chức."),
            Kind::Feedback if empty(&self.text) => Some("Hãy viết điều bạn muốn nói."),
            Kind::Feature if empty(&self.text) => Some("Hãy viết việc bạn cần app làm."),
            Kind::Feature if empty(&self.contact) => Some("Hãy ghi cách liên hệ lại với bạn."),
            _ => None,
        }
    }

    /// The message as it leaves; the app adds a sponsor's logo.
    fn outgoing(&self, kind: Kind) -> Outgoing {
        Outgoing {
            kind,
            name: self.name.clone(),
            contact: self.contact.clone(),
            text: self.text.clone(),
            logo: None,
            diagnostics: kind == Kind::Feedback && self.diagnostics,
        }
    }
}

fn one_line<'a>(text: &'a mut String, hint: &str, limit: usize) -> egui::TextEdit<'a> {
    egui::TextEdit::singleline(text)
        .desired_width(f32::INFINITY)
        .char_limit(limit)
        .hint_text(hint)
}

fn lines<'a>(text: &'a mut String, hint: &str, rows: usize) -> egui::TextEdit<'a> {
    egui::TextEdit::multiline(text)
        .desired_width(f32::INFINITY)
        .desired_rows(rows)
        .char_limit(MAX_TEXT)
        .hint_text(hint)
}

/// The fields of a message of `kind`.
fn fields(
    ui: &mut egui::Ui,
    kind: Kind,
    draft: &mut Draft,
    logo: &Option<Result<String, String>>,
    actions: &mut UiActions,
) {
    let weak = |text: &str| egui::RichText::new(text).small().weak();
    const REACH_HINT: &str = "Số điện thoại, Zalo hoặc email";
    match kind {
        Kind::Feedback => {
            ui.label("Bạn gặp lỗi gì, hoặc muốn góp ý gì cho app?");
            ui.add(lines(
                &mut draft.text,
                "Lỗi xảy ra khi đang làm gì, với file nào…",
                5,
            ));
            ui.add_space(6.0);
            ui.label("Cách liên hệ lại với bạn (không bắt buộc)");
            ui.add(one_line(&mut draft.contact, REACH_HINT, 120));
            ui.add_space(6.0);
            ui.checkbox(&mut draft.diagnostics, WITH_LOGS);
            ui.label(weak(
                "Nhật ký ghi các thao tác trong app, trong đó có tên file bạn đã mở. \
                 Nó giúp tác giả tìm ra lỗi, và chỉ được gửi khi bạn đánh dấu ô này.",
            ));
            ui.add_space(6.0);
            let logs = if draft.diagnostics {
                ", nhật ký lỗi gần nhất"
            } else {
                ""
            };
            ui.label(weak(&format!(
                "Sẽ gửi tới tác giả: lời bạn viết, bản app đang chạy ({}){logs}.",
                crate::update::this_build().day()
            )));
        }
        Kind::Feature => {
            ui.label("Bạn cần app làm thêm việc gì cho công việc của mình?");
            ui.add(lines(
                &mut draft.text,
                "Việc đang phải làm tay, mong app làm giúp…",
                5,
            ));
            ui.add_space(6.0);
            ui.label("Cách liên hệ lại với bạn");
            ui.add(one_line(&mut draft.contact, REACH_HINT, 120));
            ui.add_space(6.0);
            ui.label(weak("Tác giả sẽ liên hệ lại để trao đổi."));
        }
        Kind::Sponsor => {
            ui.label("Tên công ty, tổ chức");
            ui.add(one_line(&mut draft.name, "Tên hiện trong danh sách", 80));
            ui.add_space(6.0);
            ui.label("Thông tin liên hệ muốn hiện trong danh sách");
            ui.add(one_line(
                &mut draft.contact,
                "Số điện thoại, địa chỉ hoặc trang web",
                120,
            ));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button(PICK_LOGO).clicked() {
                    actions.chrome.pick_sponsor_logo = true;
                }
                match logo {
                    None => ui.label(weak("Chưa chọn (ảnh PNG hoặc JPG, tối đa 2 MB)")),
                    Some(Ok(name)) => ui.label(name),
                    Some(Err(why)) => ui.colored_label(MAIL_WARNING, why),
                };
            });
            ui.add_space(6.0);
            ui.label("Lời nhắn cho tác giả (không bắt buộc)");
            ui.add(lines(&mut draft.text, "", 2));
            ui.add_space(6.0);
            ui.label(weak(
                "Tác giả đối chiếu với chuyển khoản rồi mới đưa tên và logo lên danh sách. \
                 Danh sách hiện công khai cho mọi người dùng app.",
            ));
        }
    }
}

pub(crate) fn author_mail_dialog(
    ctx: &egui::Context,
    data: &UiData,
    actions: &mut UiActions,
    kind: Kind,
) {
    // Esc leaves a field being typed in before it closes the dialog.
    let typing = crate::ui::widgets::typing_in_a_field(ctx, egui::Id::new("author_mail_typing"));
    let esc = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    let mut close = esc && !typing;
    let mail = &data.chrome.mail;
    let was = ctx
        .data(|d| d.get_temp::<Draft>(Draft::id(kind)))
        .unwrap_or_default();
    let mut draft = was.clone();

    modal_overlay(ctx, "author_mail_dialog_overlay");

    egui::Window::new(kind.label())
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.set_width(MAIL_DIALOG_WIDTH);
            ui.add_space(6.0);
            match &mail.status {
                Status::Idle | Status::Failed(_) => {
                    fields(ui, kind, &mut draft, &mail.logo, actions);
                    if let Status::Failed(why) = &mail.status {
                        ui.add_space(6.0);
                        ui.colored_label(MAIL_WARNING, why);
                    }
                    ui.add_space(14.0);
                    let missing = draft.missing(kind);
                    ui.horizontal(|ui| {
                        let send = egui::Button::new(format!("  {SEND}  "));
                        if ui.add_enabled(missing.is_none(), send).clicked() {
                            actions.chrome.send_to_author = Some(draft.outgoing(kind));
                        }
                        close |= ui.button("Hủy").clicked();
                        if let Some(missing) = missing {
                            ui.label(egui::RichText::new(missing).small().weak());
                        }
                    });
                }
                Status::Sending => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Đang gửi tới tác giả…");
                    });
                    ui.add_space(14.0);
                    close |= ui.button("  Đóng  ").clicked();
                }
                Status::Sent => {
                    // What went is not kept to be sent twice.
                    draft = Draft::default();
                    ui.label(egui::RichText::new(SENT).strong());
                    ui.add_space(14.0);
                    close |= ui.button("  Đóng  ").clicked();
                }
                Status::Kept(path) => {
                    draft = Draft::default();
                    ui.label(egui::RichText::new(NOT_SENT).strong());
                    ui.add_space(4.0);
                    ui.label("App đã lưu lời nhắn của bạn thành một file trên Desktop:");
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    ui.label(egui::RichText::new(name).strong());
                    ui.add_space(4.0);
                    ui.add(
                        egui::Label::new(format!(
                            "Hãy gửi file đó tới {AUTHOR_EMAIL}, hoặc gửi cho tác giả qua Zalo, \
                             Facebook."
                        ))
                        .wrap(),
                    );
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        if ui.button(REVEAL).clicked() {
                            crate::author_mail::reveal(path);
                        }
                        close |= ui.button("  Đóng  ").clicked();
                    });
                }
            }
        });

    if close {
        actions.chrome.close_support = true;
        // An outcome that was on show has been read.
        actions.chrome.author_mail_read = !matches!(mail.status, Status::Idle | Status::Sending);
    }
    if draft != was {
        ctx.data_mut(|d| d.insert_temp(Draft::id(kind), draft));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::SupportDialog;
    use crate::ui::dialogs::stage::Stage;
    use std::path::PathBuf;

    fn staged(kind: Kind, status: Status) -> Stage {
        let mut data = UiData::default();
        data.chrome.support = Some(SupportDialog::Mail(kind));
        data.chrome.mail.status = status;
        Stage::new(data, move |ctx, data, actions| {
            author_mail_dialog(ctx, data, actions, kind)
        })
    }

    #[test]
    fn a_report_goes_once_something_is_written_and_logs_only_when_ticked() {
        let shown = staged(Kind::Feedback, Status::Idle);
        // Nothing written: nothing to send.
        assert!(shown.says("Hãy viết điều bạn muốn nói."));
        assert!(shown.click(SEND).0.chrome.send_to_author.is_none());
        assert!(shown.says("Sẽ gửi tới tác giả") && !shown.says(", nhật ký lỗi gần nhất."));

        shown.click("Lỗi xảy ra khi đang làm gì, với file nào…");
        shown.type_text("Bấm in thì app đứng");
        assert!(!shown.says("Hãy viết điều bạn muốn nói."));
        let sent = shown.click(SEND).0.chrome.send_to_author.unwrap();
        assert_eq!(
            (sent.kind, sent.text.as_str(), sent.diagnostics, &sent.logo),
            (Kind::Feedback, "Bấm in thì app đứng", false, &None)
        );

        // Ticked: the dialog says the logs go too, and they do.
        shown.click(WITH_LOGS);
        assert!(shown.says(", nhật ký lỗi gần nhất."));
        assert!(
            shown
                .click(SEND)
                .0
                .chrome
                .send_to_author
                .unwrap()
                .diagnostics
        );

        // Esc leaves the field first; what was written outlives the dialog.
        shown.click("Số điện thoại, Zalo hoặc email");
        shown.type_text("0900");
        let left = shown.key(egui::Key::Escape);
        assert!(!left.chrome.close_support);
        let closed = shown.key(egui::Key::Escape);
        assert!(closed.chrome.close_support && !closed.chrome.author_mail_read);
        assert!(shown.says("Bấm in thì app đứng") && shown.says("0900"));
    }

    #[test]
    fn a_request_needs_a_way_back_and_a_sponsor_a_name() {
        let shown = staged(Kind::Feature, Status::Idle);
        shown.click("Việc đang phải làm tay, mong app làm giúp…");
        shown.type_text("Xếp ảnh thẻ theo lớp");
        assert!(shown.says("Hãy ghi cách liên hệ lại với bạn."));
        assert!(shown.click(SEND).0.chrome.send_to_author.is_none());
        shown.click("Số điện thoại, Zalo hoặc email");
        shown.type_text("0900 000 000");
        let sent = shown.click(SEND).0.chrome.send_to_author.unwrap();
        assert_eq!(
            (sent.kind, sent.contact.as_str(), sent.diagnostics),
            (Kind::Feature, "0900 000 000", false)
        );
        // The box for logs belongs to a bug report.
        assert!(!shown.says(WITH_LOGS));

        let mut shown = staged(Kind::Sponsor, Status::Idle);
        assert!(shown.says("Hãy ghi tên công ty, tổ chức."));
        assert!(shown.click(PICK_LOGO).0.chrome.pick_sponsor_logo);
        assert!(shown.says("Chưa chọn"));
        shown.data.chrome.mail.logo = Some(Ok("logo tiệm.png".to_string()));
        assert!(shown.says("logo tiệm.png") && !shown.says("Chưa chọn"));
        shown.data.chrome.mail.logo = Some(Err("Logo lớn quá 2 MB.".to_string()));
        assert!(shown.says("Logo lớn quá 2 MB."));
        shown.click("Tên hiện trong danh sách");
        shown.type_text("Tiệm ảnh Hoa Mai");
        let sent = shown.click(SEND).0.chrome.send_to_author.unwrap();
        assert_eq!(
            (sent.kind, sent.name.as_str()),
            (Kind::Sponsor, "Tiệm ảnh Hoa Mai")
        );
    }

    #[test]
    fn the_outcome_is_said_and_read_when_the_dialog_closes() {
        let shown = staged(Kind::Feedback, Status::Sending);
        assert!(shown.says("Đang gửi tới tác giả") && !shown.says(SEND));
        let closed = shown.click("Đóng").0;
        assert!(closed.chrome.close_support && !closed.chrome.author_mail_read);

        let shown = staged(Kind::Feedback, Status::Sent);
        assert!(shown.says(SENT));
        let closed = shown.click("Đóng").0;
        assert!(closed.chrome.close_support && closed.chrome.author_mail_read);

        let kept = PathBuf::from("C:/Users/x/Desktop/iAi gui tac gia 12-10-2026-21-05.zip");
        let shown = staged(Kind::Feature, Status::Kept(kept));
        assert!(shown.says(NOT_SENT) && shown.says(REVEAL));
        assert!(shown.says("iAi gui tac gia 12-10-2026-21-05.zip"));
        assert!(shown.says(AUTHOR_EMAIL));
        assert!(shown.key(egui::Key::Escape).chrome.author_mail_read);

        // Neither sent nor kept: the reason, and the form to try again.
        let failed = Status::Failed("Không gửi được.".to_string());
        let shown = staged(Kind::Feedback, failed);
        assert!(shown.says("Không gửi được.") && shown.says(SEND));
    }

    #[test]
    fn what_went_is_not_kept_to_be_sent_twice() {
        let mut shown = staged(Kind::Feedback, Status::Idle);
        shown.click("Lỗi xảy ra khi đang làm gì, với file nào…");
        shown.type_text("Một lỗi");
        shown.data.chrome.mail.status = Status::Sent;
        shown.frame(vec![]);
        shown.data.chrome.mail.status = Status::Idle;
        assert!(!shown.says("Một lỗi"));
    }

    /// Opt-in: IAI_UI_SNAPSHOT is a folder; each kind of message is drawn
    /// there, and the outcomes, `gui_tac_gia_<kind>.png`.
    #[test]
    #[ignore]
    fn probe_dialog_snapshot_of_the_author_mail_dialog() {
        let Ok(dir) = std::env::var("IAI_UI_SNAPSHOT") else {
            return;
        };
        let report = Draft {
            text: "Bấm in ảnh thẻ thì app đứng khoảng 10 giây, máy in Canon G1010.".to_string(),
            contact: "0900 000 000 (Zalo)".to_string(),
            diagnostics: true,
            ..Default::default()
        };
        let sponsor = Draft {
            name: "Tiệm ảnh Hoa Mai".to_string(),
            contact: "0900 000 000 · 12 Lê Lợi, Huế".to_string(),
            ..Default::default()
        };
        let kept = PathBuf::from("C:/Users/x/Desktop/iAi gui tac gia 12-10-2026-21-05.zip");
        for (name, kind, draft, status, logo) in [
            ("bao_loi", Kind::Feedback, report, Status::Idle, None),
            (
                "tinh_nang",
                Kind::Feature,
                Draft::default(),
                Status::Idle,
                None,
            ),
            (
                "don_vi",
                Kind::Sponsor,
                sponsor,
                Status::Idle,
                Some(Ok("logo_hoa_mai.png".to_string())),
            ),
            (
                "da_gui",
                Kind::Feedback,
                Draft::default(),
                Status::Sent,
                None,
            ),
            (
                "luu_desktop",
                Kind::Feedback,
                Draft::default(),
                Status::Kept(kept),
                None,
            ),
        ] {
            let mut data = UiData::default();
            data.chrome.support = Some(SupportDialog::Mail(kind));
            data.chrome.mail.status = status;
            data.chrome.mail.logo = logo;
            let image = crate::ui::snapshot::render(640.0, 470.0, 1.5, None, |ctx| {
                ctx.data_mut(|d| d.insert_temp(Draft::id(kind), draft.clone()));
                #[allow(deprecated)]
                egui::CentralPanel::default().show(ctx, |_| {});
                author_mail_dialog(ctx, &data, &mut UiActions::default(), kind);
            });
            image
                .save(std::path::Path::new(&dir).join(format!("gui_tac_gia_{name}.png")))
                .unwrap();
        }
    }
}

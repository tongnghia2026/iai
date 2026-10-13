//! Word to the app's author as the window sees it. Its dialogs open like the
//! others around supporting him (`update.rs`); here the logo a sponsor sends
//! is chosen, and what the user wrote is handed to `crate::author_mail`.

use crate::app::state::App;
use crate::author_mail::{Kind, Status};
use crate::ui::{MailView, SupportDialog, UiActions};
use std::path::PathBuf;

impl App {
    pub(super) fn mail_view(&self) -> MailView {
        let name = |path: &PathBuf| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        MailView {
            status: crate::author_mail::status(),
            logo: self
                .shell
                .ui
                .sponsor_logo
                .as_ref()
                .map(|logo| logo.as_ref().map(name).map_err(String::clone)),
        }
    }

    pub(super) fn handle_author_mail_actions(&mut self, actions: &mut UiActions) {
        if std::mem::take(&mut actions.chrome.pick_sponsor_logo) {
            self.pick_sponsor_logo();
        }
        if let Some(mut outgoing) = actions.chrome.send_to_author.take() {
            if outgoing.kind == Kind::Sponsor {
                outgoing.logo = self.shell.ui.sponsor_logo.clone().and_then(Result::ok);
            }
            let form = crate::author_news::current().news.form_url.clone();
            crate::author_mail::send(outgoing, form);
        }
        if std::mem::take(&mut actions.chrome.author_mail_read) {
            self.author_mail_read();
        }
        self.follow_author_mail(crate::author_mail::observe());
    }

    /// The outcome of a message has been read: the next one starts clean,
    /// and a logo that has gone is not sent again.
    fn author_mail_read(&mut self) {
        if matches!(crate::author_mail::status(), Status::Sent | Status::Kept(_)) {
            self.shell.ui.sponsor_logo = None;
        }
        crate::author_mail::reset();
    }

    /// One frame of a message, which stands at `status`; `news` when that
    /// has just changed. With its dialog closed the outcome goes to the
    /// status line.
    pub(crate) fn follow_author_mail(&mut self, (status, news): (Status, bool)) {
        if !news {
            return;
        }
        // "Bản Pro" sends the machine's code this way, and says how it went.
        let watched = matches!(
            self.shell.ui.support_dialog,
            Some(SupportDialog::Mail(_) | SupportDialog::Pro)
        );
        if !watched {
            let said = match &status {
                Status::Sent => "Đã gửi tới tác giả. Cảm ơn bạn!".to_string(),
                Status::Kept(path) => format!(
                    "Chưa gửi được qua mạng — lời nhắn đã lưu trên Desktop: {}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ),
                Status::Failed(why) => why.clone(),
                Status::Idle | Status::Sending => String::new(),
            };
            if !said.is_empty() {
                self.shell.status_msg = said;
                self.author_mail_read();
            }
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Ask for the file of a sponsor's logo. The answer comes back through
    /// `poll_file_dialog`, to [`Self::set_sponsor_logo`].
    fn pick_sponsor_logo(&mut self) {
        if self.jobs.pending_file_dialog.is_some() {
            return;
        }
        let Some(window) = self.win.window.as_ref() else {
            return;
        };
        let parent = crate::file_io::dialog_parent(window);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Some(path) = crate::file_io::dialog_pick_logo(parent) {
                let _ = tx.send(crate::file_io::FileDialogResult::PickedSponsorLogo(path));
            }
        });
        self.jobs.pending_file_dialog = Some(rx);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// The file chosen as a sponsor's logo, kept if it can be one.
    pub(crate) fn set_sponsor_logo(&mut self, path: PathBuf) {
        self.shell.ui.sponsor_logo = Some(crate::author_mail::check_logo(&path).map(|()| path));
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_logo_is_kept_only_if_it_can_be_sent_and_the_dialog_is_told_which() {
        let dir = std::env::temp_dir().join(format!("iai_mail_app_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("Logo tiệm.png");
        image::RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255]))
            .save(&good)
            .unwrap();
        let bad = dir.join("notes.png");
        std::fs::write(&bad, b"not a picture").unwrap();

        let mut app = App::new();
        assert_eq!(app.mail_view(), MailView::default());
        app.set_sponsor_logo(good.clone());
        assert_eq!(app.mail_view().logo, Some(Ok("Logo tiệm.png".to_string())));
        app.set_sponsor_logo(bad);
        let refused = app.mail_view().logo.unwrap().unwrap_err();
        assert!(refused.contains("không phải là ảnh"), "{refused}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_outcome_nobody_is_watching_goes_to_the_status_line() {
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        let kept = Status::Kept(PathBuf::from("C:/Users/x/Desktop/iAi gui tac gia.zip"));

        // The dialog is open: it says so itself.
        app.shell.ui.support_dialog = Some(SupportDialog::Mail(Kind::Feedback));
        app.shell.status_msg.clear();
        app.follow_author_mail((Status::Sent, true));
        assert!(app.shell.status_msg.is_empty());

        // Closed meanwhile: the status line does.
        app.shell.ui.support_dialog = None;
        app.follow_author_mail((Status::Sent, true));
        assert!(app.shell.status_msg.contains("Đã gửi tới tác giả"));
        app.follow_author_mail((kept.clone(), true));
        assert!(
            app.shell.status_msg.contains("iAi gui tac gia.zip"),
            "{}",
            app.shell.status_msg
        );
        // Once: nothing new, nothing said.
        app.shell.status_msg.clear();
        app.follow_author_mail((kept, false));
        app.follow_author_mail((Status::Sending, true));
        assert!(app.shell.status_msg.is_empty());
    }

    #[test]
    fn the_dialogs_of_a_message_open_and_close_like_the_others() {
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        let mut actions = UiActions::default();
        actions.chrome.open_support = Some(SupportDialog::Mail(Kind::Feature));
        app.follow_update(&crate::update::Status::Idle, &mut actions);
        assert_eq!(
            app.shell.ui.support_dialog,
            Some(SupportDialog::Mail(Kind::Feature))
        );
        assert!(app.is_blocking_modal());
        // A message asked for in a test goes nowhere.
        actions.chrome.send_to_author = Some(crate::author_mail::Outgoing {
            kind: Kind::Feature,
            name: String::new(),
            contact: "0900".to_string(),
            text: "x".to_string(),
            logo: None,
            diagnostics: false,
        });
        app.handle_author_mail_actions(&mut actions);
        assert!(actions.chrome.send_to_author.is_none());
        assert_eq!(crate::author_mail::status(), Status::Idle);
        actions.chrome.close_support = true;
        app.follow_update(&crate::update::Status::Idle, &mut actions);
        assert_eq!(app.shell.ui.support_dialog, None);
    }
}

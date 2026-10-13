//! The app's self-update as the window sees it: what `crate::update` reports
//! from its worker threads is followed up frame by frame, and what the user
//! asks of it is passed on. With it, the "Ủng hộ tác giả" dialog: asked for,
//! or brought up by the first start of a new build.

use crate::app::state::App;
use crate::ui::{SupportDialog, UiActions, UpdateStage, UpdateView};
use crate::update::Status;

/// prefs.json: the build that ran last, and when the app last asked by
/// itself for support (seconds since 1970).
const LAST_BUILD_KEY: &str = "update_last_build";
const NUDGED_KEY: &str = "support_nudged_at";
/// The app asks by itself no more often than this, however many updates come.
const NUDGE_EVERY: u64 = 7 * 24 * 3600;

/// Whether the first start of a build says what is new in it and asks for
/// support: it has replaced another, and the last ask is long enough ago.
fn notice_due(ran_before: bool, nudged_at: u64, now: u64) -> bool {
    ran_before && now.saturating_sub(nudged_at) >= NUDGE_EVERY
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// `status` as the title bar and the update dialog show it.
fn view(status: Status, show_dialog: bool) -> UpdateView {
    if status == Status::Idle && !show_dialog {
        return UpdateView::default();
    }
    let (stage, release) = match status {
        Status::Idle => (UpdateStage::Idle, None),
        Status::Checking => (UpdateStage::Checking, None),
        Status::Current => (UpdateStage::Current, None),
        Status::Available(release) => (UpdateStage::Available, Some(release)),
        Status::Downloading(release, done) => (UpdateStage::Downloading(done), Some(release)),
        Status::Ready(release) => (UpdateStage::Ready, Some(release)),
        Status::CheckFailed(why) => (UpdateStage::CheckFailed(why), None),
        Status::DownloadFailed(why) => (UpdateStage::DownloadFailed(why), None),
    };
    UpdateView {
        stage,
        show_dialog,
        this_day: crate::update::this_build().day(),
        new_day: release.as_ref().map(|r| r.day()).unwrap_or_default(),
        megabytes: release.as_ref().map_or(0, |r| r.megabytes()),
    }
}

impl App {
    pub(super) fn update_view(&self) -> UpdateView {
        view(crate::update::status(), self.shell.ui.show_update_dialog)
    }

    /// As the app starts: remember which build this is, and whether it has
    /// just taken another's place.
    pub(crate) fn note_build_started(&mut self) {
        let this = crate::update::this_build().commit;
        let last: Option<String> = crate::core::prefs_store::get(LAST_BUILD_KEY);
        if last.as_deref() == Some(this) {
            return;
        }
        crate::core::prefs_store::set(LAST_BUILD_KEY, &this);
        let nudged_at = crate::core::prefs_store::get(NUDGED_KEY).unwrap_or(0);
        self.shell.updated_notice = notice_due(last.is_some(), nudged_at, now_secs());
    }

    /// Before `actions.doc.exit` is acted on: a restart into the new build is
    /// an exit like any other, with its save prompts.
    pub(super) fn handle_update_actions(&mut self, actions: &mut UiActions) {
        if std::mem::take(&mut actions.chrome.update_check) {
            crate::update::check(true);
            self.shell.ui.show_update_dialog = true;
        }
        if std::mem::take(&mut actions.chrome.update_download) {
            crate::update::download();
        }
        self.follow_update(&crate::update::observe(), actions);
    }

    /// One frame of the update, which stands at `status`.
    pub(crate) fn follow_update(&mut self, status: &Status, actions: &mut UiActions) {
        // The title bar's buttons stay live while a tool has work under way
        // (a crop, a transform, a stroke): a dialog is refused over it like
        // any other action from outside.
        let busy = self.exit_blocking_operation().is_some();
        if let Some(show) = actions.chrome.show_update_dialog.take() {
            if show && busy {
                self.deny_modal_action();
            } else {
                self.shell.ui.show_update_dialog = show;
                if !show {
                    crate::update::dismiss();
                }
            }
        }
        // The ways to support the author take the update dialog's place.
        if let Some(kind) = actions.chrome.open_support.take() {
            if busy {
                self.deny_modal_action();
            } else {
                self.shell.ui.show_update_dialog = false;
                crate::update::dismiss();
                // A message starts clean of the last one's outcome.
                if matches!(kind, SupportDialog::Mail(_) | SupportDialog::Pro) {
                    crate::author_mail::reset();
                }
                self.forget_pro_outcome(kind);
                self.shell.ui.support_dialog = Some(kind);
            }
        }
        if std::mem::take(&mut actions.chrome.close_support) {
            self.shell.ui.support_dialog = None;
        }
        // The first start of a new build: what is new in it and the ask for
        // support, once the user is free to read them.
        if self.shell.updated_notice && self.free_for_update() {
            self.shell.updated_notice = false;
            self.shell.ui.support_dialog = Some(SupportDialog::Updated);
            crate::core::prefs_store::set(NUDGED_KEY, &now_secs());
        }
        let mut restart = std::mem::take(&mut actions.chrome.update_restart);
        let stage = std::mem::discriminant(status);
        let before = self.shell.update_seen.replace(stage);
        let arrived = before != Some(stage);
        // A download that failed and was closed leaves the build on offer:
        // that is no news.
        let failed = std::mem::discriminant(&Status::DownloadFailed(String::new()));
        match status {
            // Said once, to a user who is not in the middle of something; the
            // title bar goes on saying it.
            Status::Available(_) if arrived && before != Some(failed) && self.free_for_update() => {
                self.shell.ui.show_update_dialog = true;
            }
            // The user asked for this build a moment ago: on to the restart,
            // unless work has been taken up meanwhile.
            Status::Ready(_) if arrived => {
                self.shell.ui.show_update_dialog = false;
                if self.free_for_update() {
                    restart = true;
                } else {
                    self.shell.status_msg =
                        "Bản mới đã tải xong — xong việc đang làm thì bấm «Khởi động lại để cập nhật» trên thanh tiêu đề"
                            .to_string();
                }
            }
            _ => {}
        }
        if restart {
            self.shell.ui.show_update_dialog = false;
            crate::update::set_relaunch(true);
            actions.doc.exit = true;
        }
        if arrived {
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
    }

    /// Nothing the user is in the middle of stands in the way of a dialog
    /// coming up, or of the app closing.
    fn free_for_update(&self) -> bool {
        !self.modal_lock_active()
            && self.exit_blocking_operation().is_none()
            && !self.shell.ui.show_exit_dialog
            && !self.shell.ui.show_close_dialog
            && !self.shell.ui.show_update_dialog
            && self.shell.ui.support_dialog.is_none()
            && self.jobs.pending_file_dialog.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::{Release, ReleaseFile};

    fn release() -> Release {
        Release {
            commit: "abc123".to_string(),
            time: 1_791_777_550,
            files: vec![ReleaseFile {
                name: "iai.exe".to_string(),
                asset: "iai.exe.gz".to_string(),
                bytes: 28_521_630,
                sha256: String::new(),
            }],
        }
    }

    /// One frame at `status` in which the dialog asked for `ask`.
    fn frame(app: &mut App, status: &Status, ask: impl FnOnce(&mut UiActions)) -> UiActions {
        let mut actions = UiActions::default();
        ask(&mut actions);
        app.follow_update(status, &mut actions);
        actions
    }

    #[test]
    fn a_newer_build_is_announced_once_and_left_alone_when_put_off() {
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        let available = Status::Available(release());
        frame(&mut app, &Status::Checking, |_| {});
        assert!(!app.shell.ui.show_update_dialog);
        frame(&mut app, &available, |_| {});
        assert!(app.shell.ui.show_update_dialog);
        // While it is up the app is under the dialog, like any other.
        assert!(app.is_blocking_modal() && app.modal_lock_active());

        // "Để sau": it stays away, the title bar keeps the news.
        frame(&mut app, &available, |a| {
            a.chrome.show_update_dialog = Some(false)
        });
        for _ in 0..3 {
            let asked = frame(&mut app, &available, |_| {});
            assert!(!app.shell.ui.show_update_dialog && !asked.doc.exit);
        }
        let shown = view(available.clone(), false);
        assert_eq!(shown.stage, UpdateStage::Available);
        assert_eq!((shown.megabytes, shown.new_day.len()), (29, 16));
        // The title bar's button brings it back.
        frame(&mut app, &available, |a| {
            a.chrome.show_update_dialog = Some(true)
        });
        assert!(app.shell.ui.show_update_dialog);
    }

    #[test]
    fn a_newer_build_does_not_break_into_work_under_way() {
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        app.shell.ui.show_preferences = true;
        frame(&mut app, &Status::Available(release()), |_| {});
        assert!(!app.shell.ui.show_update_dialog);
        // Nor later, by itself, once that work is done.
        app.shell.ui.show_preferences = false;
        frame(&mut app, &Status::Available(release()), |_| {});
        assert!(!app.shell.ui.show_update_dialog);

        // A download that failed, read and closed, leaves the build on offer
        // in the title bar: the dialog does not come back for it.
        let mut app = App::new();
        app.shell.ui.show_update_dialog = true;
        let failed = Status::DownloadFailed("mạng".to_string());
        frame(&mut app, &Status::Downloading(release(), 0.3), |_| {});
        frame(&mut app, &failed, |_| {});
        assert!(app.shell.ui.show_update_dialog);
        frame(&mut app, &failed, |a| {
            a.chrome.show_update_dialog = Some(false)
        });
        frame(&mut app, &Status::Available(release()), |_| {});
        assert!(!app.shell.ui.show_update_dialog);
    }

    #[test]
    fn a_build_that_has_come_restarts_the_app_unless_work_is_in_the_way() {
        // The user watched it come: the dialog goes, the app asks to exit.
        let mut app = App::new();
        let coming = Status::Downloading(release(), 0.5);
        app.shell.ui.show_update_dialog = true;
        frame(&mut app, &coming, |_| {});
        let asked = frame(&mut app, &Status::Ready(release()), |_| {});
        assert!(asked.doc.exit && !app.shell.ui.show_update_dialog);
        // Once: an exit the user cancels is not asked for again.
        let asked = frame(&mut app, &Status::Ready(release()), |_| {});
        assert!(!asked.doc.exit);

        // Work taken up meanwhile: nothing closes, the status line says why.
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        frame(&mut app, &coming, |_| {});
        app.shell.ui.show_preferences = true;
        let asked = frame(&mut app, &Status::Ready(release()), |_| {});
        assert!(!asked.doc.exit && !app.shell.ui.show_update_dialog);
        assert!(
            app.shell.status_msg.contains("Khởi động lại để cập nhật"),
            "{}",
            app.shell.status_msg
        );
        // Then "Khởi động lại" in the dialog is the way.
        app.shell.ui.show_preferences = false;
        let asked = frame(&mut app, &Status::Ready(release()), |a| {
            a.chrome.update_restart = true
        });
        assert!(asked.doc.exit);
    }

    #[test]
    fn nothing_to_say_costs_the_window_nothing() {
        assert_eq!(view(Status::Idle, false), UpdateView::default());
        let asked = view(Status::Current, true);
        assert_eq!(asked.stage, UpdateStage::Current);
        assert!(asked.show_dialog && !asked.this_day.is_empty());
        let unasked = view(Status::CheckFailed("mạng".to_string()), true);
        assert_eq!(unasked.stage, UpdateStage::CheckFailed("mạng".to_string()));
        let broken = view(Status::DownloadFailed("mạng".to_string()), false);
        assert_eq!(
            broken.stage,
            UpdateStage::DownloadFailed("mạng".to_string())
        );
    }

    #[test]
    fn the_ways_to_support_take_the_update_dialogs_place_and_close_again() {
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        // "Ủng hộ tác giả" under "Bạn đang dùng bản mới nhất".
        app.shell.ui.show_update_dialog = true;
        frame(&mut app, &Status::Current, |a| {
            a.chrome.open_support = Some(SupportDialog::Support)
        });
        assert!(!app.shell.ui.show_update_dialog);
        assert_eq!(app.shell.ui.support_dialog, Some(SupportDialog::Support));
        // A dialog like any other while it is up.
        assert!(app.is_blocking_modal() && app.modal_lock_active());
        // News of a build does not come up over it.
        frame(&mut app, &Status::Available(release()), |_| {});
        assert!(!app.shell.ui.show_update_dialog);
        frame(&mut app, &Status::Available(release()), |a| {
            a.chrome.close_support = true
        });
        assert_eq!(app.shell.ui.support_dialog, None);
        assert!(!app.is_blocking_modal());

        // The title bar's buttons are refused over work a tool has under way.
        app.shell.ui.document_editor_error = Some("held".to_string());
        assert!(app.exit_blocking_operation().is_some());
        frame(&mut app, &Status::Available(release()), |a| {
            a.chrome.open_support = Some(SupportDialog::Support);
            a.chrome.show_update_dialog = Some(true);
        });
        assert_eq!(app.shell.ui.support_dialog, None);
        assert!(!app.shell.ui.show_update_dialog);
    }

    #[test]
    fn the_first_start_of_a_new_build_says_what_is_new_once_the_user_is_free() {
        const DAY: u64 = 24 * 3600;
        // A first install has replaced nothing; a week must pass between asks.
        assert!(!notice_due(false, 0, 100 * DAY));
        assert!(notice_due(true, 0, 100 * DAY));
        assert!(!notice_due(true, 95 * DAY, 100 * DAY));
        assert!(notice_due(true, 93 * DAY, 100 * DAY));
        // A clock set back is no reason to ask.
        assert!(!notice_due(true, 200 * DAY, 100 * DAY));

        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        app.shell.updated_notice = true;
        // Not over work under way.
        app.shell.ui.show_preferences = true;
        frame(&mut app, &Status::Idle, |_| {});
        assert_eq!(app.shell.ui.support_dialog, None);
        app.shell.ui.show_preferences = false;
        frame(&mut app, &Status::Idle, |_| {});
        assert_eq!(app.shell.ui.support_dialog, Some(SupportDialog::Updated));
        // Once.
        frame(&mut app, &Status::Idle, |a| a.chrome.close_support = true);
        frame(&mut app, &Status::Idle, |_| {});
        assert_eq!(app.shell.ui.support_dialog, None);
        assert!(!app.shell.updated_notice);
    }
}

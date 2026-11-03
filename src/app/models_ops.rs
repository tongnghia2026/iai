//! The "Model AI" window's side of the app (A3–A6 of the plan of 30/10):
//! what it shows, read again only when the model folders were looked at
//! anew, and what it asks for that needs the window — a file to pick, the
//! folder to open — or runs as a job (a fetch, an install), one at a time.

use std::path::PathBuf;
use std::sync::Arc;

use iai_ai::model_store::{self, status::Overview};
use iai_engine::{CmdError, CommandCall, JobId, Reply};
use parking_lot::Mutex;
use serde_json::json;

use super::state::App;

/// What the window shows, and the job it started.
#[derive(Default)]
pub struct ModelsWindow {
    /// The rows as last read, with the scan they came from and whether one
    /// was under way.
    shown: Mutex<Option<(u64, bool, Arc<Overview>)>>,
    /// The fetch or install under way.
    pub job: Option<JobId>,
}

impl ModelsWindow {
    /// The rows, read again when the folders were looked at anew.
    pub fn view(&self) -> Arc<Overview> {
        let (generation, scanning) = (model_store::generation(), model_store::is_scanning());
        let mut shown = self.shown.lock();
        if let Some((at, was, view)) = shown.as_ref()
            && *at == generation
            && *was == scanning
        {
            return Arc::clone(view);
        }
        let view = Arc::new(model_store::status::overview());
        *shown = Some((generation, scanning, Arc::clone(&view)));
        view
    }
}

/// A file dropped on the window that holds models rather than a picture.
pub fn holds_models(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("onnx") || e.eq_ignore_ascii_case("zip"))
}

impl App {
    /// The window opened, or the app came back to the front with it open:
    /// the folders are looked at again (a model dropped in meanwhile shows).
    pub(crate) fn models_window_shown(&mut self) {
        model_store::rescan();
    }

    /// A command the window asked for that the window itself takes care of.
    /// True when the call is dealt with.
    pub(in crate::app) fn models_window_step(&mut self, call: &CommandCall) -> bool {
        match call.command.as_str() {
            "models.reveal" => {
                let dir = model_store::main_folder();
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::process::Command::new("explorer.exe").arg(&dir).spawn();
                true
            }
            "models.install" if call.args.get("path").is_none() => {
                self.pick_model_file();
                true
            }
            "models.fetch" | "models.install" => {
                if self.shell.models.job.is_some() {
                    self.shell.status_msg =
                        "Đang tải / cài một model — đợi xong rồi làm tiếp".to_string();
                    return true;
                }
                if let Some(Reply::Started { job }) =
                    self.start_command(&call.command, call.args.clone())
                {
                    self.shell.models.job = Some(job);
                }
                true
            }
            _ => false,
        }
    }

    /// "Cài model từ file…": a model file, or a `.zip` of them, picked.
    fn pick_model_file(&mut self) {
        if self.jobs.pending_file_dialog.is_some() {
            return;
        }
        let Some(window) = self.win.window.as_ref() else {
            return;
        };
        let parent = crate::file_io::dialog_parent(window);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Some(path) = crate::file_io::dialog_pick_model(parent) {
                let _ = tx.send(crate::file_io::FileDialogResult::PickedModelFile(path));
            }
        });
        self.jobs.pending_file_dialog = Some(rx);
    }

    /// A model file picked, or dropped on the window: installed.
    pub(in crate::app) fn install_model_file(&mut self, path: PathBuf) {
        self.models_window_step(&CommandCall::new(
            "models.install",
            json!({"path": path.display().to_string()}),
        ));
    }

    /// The window's fetch or install ended: the rows are read again, and the
    /// status line says how it went.
    pub(in crate::app) fn on_models_job_finished(&mut self, job: JobId, error: Option<&CmdError>) {
        if self.shell.models.job != Some(job) {
            return;
        }
        self.shell.models.job = None;
        model_store::rescan();
        if error.is_none() {
            self.shell.status_msg =
                "Model AI: xong — cửa sổ «Model AI» đã cập nhật (Help ▸ Model AI…)".to_string();
        }
    }
}

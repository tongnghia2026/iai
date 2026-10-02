//! "Làm ảnh thẻ" (Image ▸ Làm ảnh thẻ…): runs `core::id_photo` on a worker
//! (face mesh, then BiRefNet for the white background) against what the
//! document shows, and applies the result as one undo step when it lands.
//! When the background model is missing it is downloaded first and the job
//! starts by itself once it is ready.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};

use super::render::CanvasEvent;
use super::state::App;
use crate::core::id_photo::{self, IdPhotoOptions, IdPhotoPlan};
use crate::core::select_subject::{SelectSubjectEngine, SelectSubjectModel, SubjectStatus};

const SEGMENT_MODEL: SelectSubjectModel = SelectSubjectModel::BiRefNetTiny;

pub struct IdPhotoJob {
    doc_id: crate::core::document::DocumentId,
    size: (u32, u32),
    progress: Arc<Mutex<String>>,
    rx: Receiver<Result<IdPhotoPlan, String>>,
}

/// The dialog's view: status line, whether it is an error, whether work runs.
#[derive(Clone, Debug, Default)]
pub struct IdPhotoState {
    pub status: String,
    pub error: bool,
    pub busy: bool,
}

#[derive(Default)]
pub struct IdPhotoSession {
    job: Option<IdPhotoJob>,
    /// Waiting for the background model download, then runs with these.
    waiting: Option<IdPhotoOptions>,
    state: IdPhotoState,
}

impl IdPhotoSession {
    fn set(&mut self, status: impl Into<String>, error: bool) {
        self.state = IdPhotoState {
            status: status.into(),
            error,
            busy: self.job.is_some() || self.waiting.is_some(),
        };
    }
}

impl App {
    pub(crate) fn id_photo_state(&self) -> IdPhotoState {
        let mut state = self.shell.id_photo.state.clone();
        if let Some(job) = &self.shell.id_photo.job {
            if let Ok(line) = job.progress.lock() {
                state.status = line.clone();
            }
        }
        state
    }

    pub(crate) fn close_id_photo(&mut self) {
        // A running job finishes on its own; its result is dropped.
        self.shell.id_photo = IdPhotoSession::default();
    }

    /// Start the job on the active document (or queue it behind the model
    /// download).
    pub(crate) fn run_id_photo(&mut self, options: IdPhotoOptions) {
        if self.shell.id_photo.job.is_some() {
            return;
        }
        if options.white_background && !SelectSubjectEngine::model_path_for(SEGMENT_MODEL).is_file()
        {
            self.download_segment_model();
            self.shell.id_photo.waiting = Some(options);
            self.shell.id_photo.set(self.segment_model_status(), false);
            return;
        }
        self.shell.id_photo.waiting = None;
        if let Err(e) = self.start_id_photo(options) {
            self.shell
                .id_photo
                .set(format!("Không làm được: {e}"), true);
        }
    }

    fn download_segment_model(&mut self) {
        let engine = &mut self.jobs.select_subject;
        if engine.selected_model() != SEGMENT_MODEL && !engine.set_selected_model(SEGMENT_MODEL) {
            return;
        }
        engine.download_model_async();
    }

    fn segment_model_state(&self) -> Option<SubjectStatus> {
        self.jobs
            .select_subject
            .status
            .lock()
            .ok()
            .map(|s| s.clone())
    }

    fn segment_model_status(&self) -> String {
        match self.segment_model_state() {
            Some(SubjectStatus::Downloading { progress }) => format!(
                "Đang tải model tách nền (~214 MB)… {:.0}% — tải xong sẽ tự chạy",
                progress * 100.0
            ),
            Some(SubjectStatus::Error(e)) => format!("Tải model tách nền lỗi: {e}"),
            _ => "Đang chuẩn bị tải model tách nền (~214 MB)…".to_string(),
        }
    }

    fn start_id_photo(&mut self, options: IdPhotoOptions) -> Result<(), String> {
        let idx = self.docs.active_doc_idx;
        let canvas = &mut self.docs.documents[idx].canvas;
        if canvas.is_cmyk() {
            return Err("chưa hỗ trợ ảnh CMYK".to_string());
        }
        canvas.selection.refresh_bbox();
        let (w, h) = (canvas.width, canvas.height);
        let clip = if canvas.selection.active {
            Some(
                super::portrait_ops::selection_clip(&canvas.selection, (0, 0), w, h)
                    .ok_or_else(|| "vùng chọn nằm ngoài ảnh".to_string())?,
            )
        } else {
            None
        };
        canvas.ensure_pixels();
        let pixels = canvas.pixels.clone();
        if pixels.len() != w as usize * h as usize * 4 {
            return Err("ảnh không hợp lệ".to_string());
        }
        let progress = Arc::new(Mutex::new("Đang chuẩn bị…".to_string()));
        let (tx, rx) = mpsc::channel();
        {
            let progress = Arc::clone(&progress);
            let prefer_gpu = crate::core::ai::ort_ep::prefer_gpu();
            std::thread::spawn(move || {
                let report = |line: String| {
                    if let Ok(mut slot) = progress.lock() {
                        *slot = line;
                    }
                };
                let mut segment = |px: &[u8], sw: u32, sh: u32| {
                    crate::core::select_subject::segment_blocking(
                        SEGMENT_MODEL,
                        px,
                        sw,
                        sh,
                        prefer_gpu,
                    )
                };
                let plan = id_photo::prepare(
                    &pixels,
                    w,
                    h,
                    clip.as_ref(),
                    &options,
                    &mut segment,
                    &report,
                );
                let _ = tx.send(plan);
            });
        }
        self.shell.id_photo.job = Some(IdPhotoJob {
            doc_id: self.docs.documents[idx].id,
            size: (w, h),
            progress,
            rx,
        });
        self.shell.id_photo.set("Đang chuẩn bị…", false);
        Ok(())
    }

    /// Every frame: start a job whose model has arrived, collect a finished
    /// one and apply it.
    pub(crate) fn poll_id_photo(&mut self) {
        if let Some(options) = self.shell.id_photo.waiting {
            match self.segment_model_state() {
                Some(SubjectStatus::Ready) => self.run_id_photo(options),
                Some(SubjectStatus::Error(_)) => {
                    self.shell.id_photo.waiting = None;
                    let line = self.segment_model_status();
                    self.shell.id_photo.set(line, true);
                }
                _ => {
                    let line = self.segment_model_status();
                    self.shell.id_photo.set(line, false);
                }
            }
            self.request_repaint();
            return;
        }
        let Some(job) = self.shell.id_photo.job.as_ref() else {
            return;
        };
        let result = match job.rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => {
                self.request_repaint();
                return;
            }
            Err(TryRecvError::Disconnected) => Err("xử lý dừng bất thường".to_string()),
        };
        let job = self.shell.id_photo.job.take().expect("job checked above");
        match result.and_then(|plan| self.apply_id_photo(&job, plan)) {
            Ok(message) => {
                self.shell.status_msg = message;
                self.shell.ui.show_id_photo_dialog = false;
                self.close_id_photo();
            }
            Err(e) => {
                self.shell
                    .id_photo
                    .set(format!("Không làm được: {e}"), true);
            }
        }
        self.request_repaint();
    }

    fn apply_id_photo(&mut self, job: &IdPhotoJob, plan: IdPhotoPlan) -> Result<String, String> {
        let Some(idx) = self.docs.documents.iter().position(|d| d.id == job.doc_id) else {
            return Err("tab ảnh đã đóng".to_string());
        };
        let canvas = &mut self.docs.documents[idx].canvas;
        if (canvas.width, canvas.height) != job.size {
            return Err("kích thước ảnh đã đổi trong lúc chạy — bấm chạy lại".to_string());
        }
        let cropped = plan.frame.is_some();
        let notes = plan.notes.join("; ");
        id_photo::apply(canvas, plan)?;
        // As after a Crop commit: this event owns the canvas-size bookkeeping
        // (GPU texture, refit, recomposite).
        if idx == self.docs.active_doc_idx {
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            self.apply_canvas_event(CanvasEvent::SelectionChanged);
        }
        let mut message = if cropped {
            let (w, h) = id_photo::output_size();
            format!("Làm ảnh thẻ xong: {w}×{h} px, 600 ppi (Ctrl+Z để hoàn tác)")
        } else {
            "Làm ảnh thẻ xong: đã tách người lên nền trắng (Ctrl+Z để hoàn tác)".to_string()
        };
        if !notes.is_empty() {
            message.push_str(" — ");
            message.push_str(&notes);
        }
        Ok(message)
    }

    fn request_repaint(&self) {
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
}

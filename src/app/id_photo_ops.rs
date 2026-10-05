//! "Làm ảnh thẻ" (the Ảnh thẻ side of Auto retouch): runs `core::id_photo`
//! on a worker (BiRefNet for the backdrop, then the face mesh) against what
//! the document shows, and applies the result as one undo step when it lands.
//! When the background model is missing it is downloaded first and the job
//! starts by itself once it is ready.
//!
//! What the models found is kept while the panel is open: asking again with
//! another size, framing or nudge undoes the photo made and frames it anew
//! from that, without running the models again. The retouch (the dialog's
//! sliders) then starts over on the new photo, a moment after the last
//! change so that a run of nudges analyses the face once.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::render::CanvasEvent;
use super::state::App;
use crate::core::id_photo::{self, Analysis, IdPhotoOptions, IdPhotoPlan, IdPhotoRequest, Nudge};
use crate::core::imposition::Backdrop;
use crate::core::portrait::{Clip, PortraitSettings};
use crate::core::select_subject::{SelectSubjectEngine, SelectSubjectModel, SubjectStatus};

const SEGMENT_MODEL: SelectSubjectModel = SelectSubjectModel::BiRefNetTiny;
/// How long after a photo framed again the retouch starts on it.
const RETOUCH_DELAY: Duration = Duration::from_millis(600);

/// The photo an ID photo is made from, as it was before.
#[derive(Clone)]
struct Source {
    pixels: Arc<Vec<u8>>,
    size: (u32, u32),
    clip: Option<Clip>,
}

struct Made {
    plan: IdPhotoPlan,
    found: Arc<Analysis>,
}

pub struct IdPhotoJob {
    doc_id: crate::core::document::DocumentId,
    asked: IdPhotoRequest,
    source: Source,
    /// The document holds the ID photo made before from `source`: that one
    /// is undone first.
    again: bool,
    progress: Arc<Mutex<String>>,
    rx: Receiver<Result<Made, String>>,
}

/// The ID photo the document holds, kept to frame it again.
struct Kept {
    doc_id: crate::core::document::DocumentId,
    source: Source,
    found: Arc<Analysis>,
    options: IdPhotoOptions,
    nudge: Nudge,
    /// History entries it took (the photo, then each change of backdrop),
    /// and the history's revision after the last of them.
    steps: usize,
    revision: u64,
}

/// The panel's view: status line, whether it is an error, whether work runs,
/// and whether the document holds an ID photo that can be framed again.
#[derive(Clone, Debug, Default)]
pub struct IdPhotoState {
    pub status: String,
    pub error: bool,
    pub busy: bool,
    pub made: bool,
}

#[derive(Default)]
pub struct IdPhotoSession {
    job: Option<IdPhotoJob>,
    /// Waiting for the background model download, then runs.
    waiting: Option<IdPhotoRequest>,
    /// Asked while a job ran: runs when that one lands.
    next: Option<IdPhotoRequest>,
    kept: Option<Kept>,
    /// When the retouch starts on the photo just made, and from which sliders.
    retouch_due: Option<(Instant, Option<PortraitSettings>)>,
    state: IdPhotoState,
}

impl IdPhotoSession {
    fn set(&mut self, status: impl Into<String>, error: bool) {
        self.state.status = status.into();
        self.state.error = error;
    }
}

/// "2,8×3,8 cm".
fn centimetres(size: crate::core::imposition::PhotoKind) -> String {
    let (w, h) = size.cell_cm();
    let side = |v: f32| format!("{v:.1}").replace(".0", "").replace('.', ",");
    format!("{}×{} cm", side(w), side(h))
}

fn backdrop_name(backdrop: Backdrop) -> &'static str {
    match backdrop {
        Backdrop::White => "trắng",
        Backdrop::Blue => "xanh",
    }
}

impl App {
    pub(crate) fn id_photo_state(&self) -> IdPhotoState {
        let session = &self.shell.id_photo;
        let mut state = session.state.clone();
        if let Some(job) = &session.job {
            if let Ok(line) = job.progress.lock() {
                state.status = line.clone();
            }
        }
        state.busy = self.id_photo_busy();
        state.made = self.kept_id_photo().is_some();
        state
    }

    /// Whether an ID photo is being made. Until the retouch has started on
    /// the photo just made, work is still to come.
    pub(crate) fn id_photo_busy(&self) -> bool {
        let session = &self.shell.id_photo;
        session.job.is_some() || session.waiting.is_some() || session.retouch_due.is_some()
    }

    pub(crate) fn close_id_photo(&mut self) {
        // A running job finishes on its own; its result is dropped.
        self.shell.id_photo = IdPhotoSession::default();
    }

    /// Stop the work under way. The photo already made stays, and can
    /// still be framed again.
    pub(crate) fn stop_id_photo(&mut self) {
        let session = &mut self.shell.id_photo;
        let running = session.job.take().is_some() | session.waiting.take().is_some();
        session.next = None;
        session.retouch_due = None;
        if running {
            session.set("Đã dừng làm ảnh thẻ", false);
        }
    }

    /// The ID photo kept, when the active document still is that photo.
    fn kept_id_photo(&self) -> Option<&Kept> {
        let doc = self.docs.documents.get(self.docs.active_doc_idx)?;
        self.shell
            .id_photo
            .kept
            .as_ref()
            .filter(|k| k.doc_id == doc.id && k.revision == doc.canvas.history_revision())
    }

    /// Make the ID photo of the active document, or make it again as now
    /// asked (queued behind a running job or the model download).
    pub(crate) fn run_id_photo(&mut self, asked: IdPhotoRequest) {
        if self.shell.id_photo.job.is_some() {
            self.shell.id_photo.next = Some(asked);
            return;
        }
        let kept = self
            .kept_id_photo()
            .map(|k| (k.options, k.nudge, k.found.person.is_some()));
        if let Some((options, nudge, _)) = kept {
            if (asked.options, asked.nudge) == (options, nudge) {
                return;
            }
            // Another backdrop under the same cut-out is only a fill.
            let recoloured = IdPhotoOptions {
                backdrop: asked.options.backdrop,
                ..options
            };
            if options.cut_out && recoloured == asked.options && nudge == asked.nudge {
                self.set_id_photo_backdrop(asked.options.backdrop);
                return;
            }
        }
        let masked = kept.is_some_and(|(_, _, masked)| masked);
        if asked.options.cut_out
            && !masked
            && !SelectSubjectEngine::model_path_for(SEGMENT_MODEL).is_file()
        {
            self.download_segment_model();
            self.shell.id_photo.waiting = Some(asked);
            self.shell.id_photo.set(self.segment_model_status(), false);
            return;
        }
        self.shell.id_photo.waiting = None;
        if let Err(e) = self.start_id_photo(asked) {
            self.shell
                .id_photo
                .set(format!("Không làm được: {e}"), true);
        }
    }

    fn set_id_photo_backdrop(&mut self, backdrop: Backdrop) {
        let idx = self.docs.active_doc_idx;
        let canvas = &mut self.docs.documents[idx].canvas;
        let changed = id_photo::set_backdrop(canvas, backdrop);
        let revision = canvas.history_revision();
        if let Some(kept) = &mut self.shell.id_photo.kept {
            kept.options.backdrop = backdrop;
            kept.steps += changed as usize;
            kept.revision = revision;
        }
        if changed {
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            self.shell.id_photo.set(
                format!("Đã đổi sang nền {}", backdrop_name(backdrop)),
                false,
            );
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
                "Đang tải model tách nền BiRefNet (~214 MB)… {:.0}% — tải xong sẽ tự chạy",
                progress * 100.0
            ),
            Some(SubjectStatus::Error(e)) => format!("Tải model tách nền lỗi: {e}"),
            _ => "Đang chuẩn bị tải model tách nền BiRefNet (~214 MB)…".to_string(),
        }
    }

    /// The active document as the ID photo's source: what it shows, and its
    /// selection.
    fn id_photo_source(&mut self) -> Result<Source, String> {
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
        Ok(Source {
            pixels: Arc::new(pixels),
            size: (w, h),
            clip,
        })
    }

    fn start_id_photo(&mut self, asked: IdPhotoRequest) -> Result<(), String> {
        let idx = self.docs.active_doc_idx;
        let doc_id = self.docs.documents[idx].id;
        let kept = self
            .kept_id_photo()
            .map(|k| (k.source.clone(), Arc::clone(&k.found)));
        let (source, found, again) = match kept {
            Some((source, found)) => (source, Some(found), true),
            None => {
                // Nothing kept is this photo: it is made from what the
                // document shows, without a retouch preview on it.
                self.shell.id_photo.kept = None;
                self.shell.id_photo.retouch_due = None;
                self.cancel_portrait();
                (self.id_photo_source()?, None, false)
            }
        };
        // A photo that kept its background was analysed without a mask.
        let found = found.filter(|f| f.person.is_some() || !asked.options.cut_out);
        let progress = Arc::new(Mutex::new("Đang chuẩn bị…".to_string()));
        let (tx, rx) = mpsc::channel();
        {
            let progress = Arc::clone(&progress);
            let source = source.clone();
            let prefer_gpu = crate::core::ai::ort_ep::prefer_gpu();
            let (options, nudge) = (asked.options, asked.nudge);
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
                let (w, h) = source.size;
                let made = match found {
                    Some(found) => Ok(found),
                    None => id_photo::analyse(
                        &source.pixels,
                        w,
                        h,
                        source.clip.as_ref(),
                        options.cut_out,
                        &mut segment,
                        &report,
                    )
                    .map(Arc::new),
                }
                .and_then(|found| {
                    let plan = id_photo::plan(&source.pixels, &found, &options, nudge, &report)?;
                    Ok(Made { plan, found })
                });
                let _ = tx.send(made);
            });
        }
        self.shell.id_photo.job = Some(IdPhotoJob {
            doc_id,
            asked,
            source,
            again,
            progress,
            rx,
        });
        self.shell.id_photo.set("Đang chuẩn bị…", false);
        Ok(())
    }

    /// Every frame: start a job whose model has arrived, collect a finished
    /// one and apply it, and start the retouch on a photo just made.
    pub(crate) fn poll_id_photo(&mut self) {
        if let Some(asked) = self.shell.id_photo.waiting {
            match self.segment_model_state() {
                Some(SubjectStatus::Ready) => self.run_id_photo(asked),
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
            self.start_due_retouch();
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
        match result.and_then(|made| self.apply_id_photo(&job, made)) {
            Ok(message) => {
                self.shell.id_photo.set(message.clone(), false);
                self.shell.status_msg = format!("{message} (Ctrl+Z để hoàn tác)");
                let active = self.docs.documents[self.docs.active_doc_idx].id == job.doc_id;
                if active {
                    let delay = if job.again {
                        RETOUCH_DELAY
                    } else {
                        Duration::ZERO
                    };
                    self.shell.id_photo.retouch_due =
                        Some((Instant::now() + delay, job.asked.settings));
                }
                self.garment_after_id_photo(job.doc_id, job.asked.settings);
                if let Some(next) = self.shell.id_photo.next.take() {
                    self.run_id_photo(next);
                }
            }
            Err(e) => {
                self.shell.id_photo.next = None;
                self.shell
                    .id_photo
                    .set(format!("Không làm được: {e}"), true);
            }
        }
        self.request_repaint();
    }

    /// Open the retouch on the photo just made, once its moment has come.
    fn start_due_retouch(&mut self) {
        let Some((at, settings)) = self.shell.id_photo.retouch_due else {
            return;
        };
        // The garment goes on first: the retouch is of the person dressed.
        if Instant::now() < at || self.garment_busy() {
            self.request_repaint();
            return;
        }
        self.shell.id_photo.retouch_due = None;
        if !self.shell.ui.show_portrait_dialog {
            return;
        }
        if let Err(e) = self.begin_portrait_from(settings) {
            let line = format!(
                "{} — chưa chỉnh chân dung được: {e}",
                self.shell.id_photo.state.status
            );
            self.shell.id_photo.set(line, true);
        }
        self.request_repaint();
    }

    fn apply_id_photo(&mut self, job: &IdPhotoJob, made: Made) -> Result<String, String> {
        let Some(idx) = self.docs.documents.iter().position(|d| d.id == job.doc_id) else {
            return Err("tab ảnh đã đóng".to_string());
        };
        // The retouch under way is of the photo this one replaces.
        self.cancel_portrait();
        self.shell.id_photo.retouch_due = None;
        let kept = self.shell.id_photo.kept.take();
        let canvas = &mut self.docs.documents[idx].canvas;
        if job.again {
            let steps = kept
                .filter(|k| k.doc_id == job.doc_id && k.revision == canvas.history_revision())
                .map(|k| k.steps)
                .ok_or_else(|| {
                    "ảnh đã đổi từ lúc làm ảnh thẻ — bấm Làm ảnh thẻ tự động lại".to_string()
                })?;
            for _ in 0..steps {
                canvas.undo();
            }
        }
        if (canvas.width, canvas.height) != job.source.size {
            return Err("kích thước ảnh đã đổi trong lúc chạy — bấm chạy lại".to_string());
        }
        let options = job.asked.options;
        let cropped = made.plan.frame.is_some();
        let cut_out = made.plan.cutout.is_some();
        let notes = made.plan.notes.join("; ");
        let began = Instant::now();
        let applied = id_photo::apply(canvas, made.plan);
        let pixels = began.elapsed();
        let revision = canvas.history_revision();
        // As after a Crop commit: this event owns the canvas-size bookkeeping
        // (GPU texture, refit, recomposite).
        if idx == self.docs.active_doc_idx {
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            self.apply_canvas_event(CanvasEvent::SelectionChanged);
        }
        // All of it on the UI thread, like a Crop commit: keep what it cost.
        let total = began.elapsed();
        if total.as_millis() >= 30 {
            crate::diag::note(
                "perf",
                &format!(
                    "id photo applied in {} ms (pixels {} ms, screen {} ms)",
                    total.as_millis(),
                    pixels.as_millis(),
                    (total - pixels).as_millis()
                ),
            );
        }
        applied?;
        self.shell.id_photo.kept = Some(Kept {
            doc_id: job.doc_id,
            source: job.source.clone(),
            found: made.found,
            options,
            nudge: job.asked.nudge,
            steps: 1,
            revision,
        });
        let mut message = if cropped {
            let (w, h) = id_photo::print_px(options.size);
            format!(
                "Làm ảnh thẻ xong: {}, {w}×{h} px",
                centimetres(options.size)
            )
        } else {
            "Làm ảnh thẻ xong".to_string()
        };
        if cut_out {
            message.push_str(&format!(", nền {}", backdrop_name(options.backdrop)));
        }
        if !notes.is_empty() {
            message.push_str(" — ");
            message.push_str(&notes);
        }
        Ok(message)
    }

    /// Whether the retouch is about to start on the photo just made.
    pub(crate) fn retouch_is_due(&self) -> bool {
        self.shell.id_photo.retouch_due.is_some()
    }

    /// Start the retouch over on what the document now shows, from
    /// `settings`, while the dialog is open.
    pub(crate) fn retouch_after(&mut self, settings: Option<PortraitSettings>) {
        if self.shell.ui.show_portrait_dialog {
            self.shell.id_photo.retouch_due = Some((Instant::now(), settings));
        }
    }

    /// Work on the ID photo kept took `steps` more history entries, from
    /// revision `before` to `after`: they are undone with it when it is
    /// framed again.
    pub(crate) fn id_photo_took(
        &mut self,
        doc_id: crate::core::document::DocumentId,
        before: u64,
        steps: usize,
        after: u64,
    ) {
        if let Some(kept) = &mut self.shell.id_photo.kept {
            if kept.doc_id == doc_id && kept.revision == before {
                kept.steps += steps;
                kept.revision = after;
            }
        }
    }

    fn request_repaint(&self) {
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::Canvas;
    use crate::core::imposition::PhotoKind;

    #[test]
    fn sizes_read_as_centimetres() {
        assert_eq!(centimetres(PhotoKind::Id3x4), "2,8×3,8 cm");
        assert_eq!(centimetres(PhotoKind::Id2x3), "2×3 cm");
        assert_eq!(centimetres(PhotoKind::Id4x6), "4×6 cm");
    }

    /// The portrait the retouch tests use, when it and the models are here.
    fn photo() -> Option<image::RgbaImage> {
        let path =
            std::path::Path::new("tmp/model-sources/gfpgan/inputs/whole_imgs/Blake_Lively.jpg");
        if !path.is_file()
            || crate::core::ai::face_mesh::model_path().is_none()
            || !SelectSubjectEngine::model_path_for(SEGMENT_MODEL).is_file()
        {
            return None;
        }
        Some(image::open(path).ok()?.to_rgba8())
    }

    /// Run the app's polls until `done`, for at most two minutes.
    fn wait(app: &mut App, what: &str, done: impl Fn(&App) -> bool) {
        let started = Instant::now();
        while !done(app) {
            assert!(started.elapsed().as_secs() < 120, "timed out: {what}");
            std::thread::sleep(Duration::from_millis(20));
            app.poll_id_photo();
            app.poll_portrait();
        }
    }

    /// The photo is made, framed again with a nudge and another size without
    /// the models, its backdrop changed, and each time the retouch starts
    /// over.
    #[test]
    fn an_id_photo_is_framed_again_from_what_was_kept() {
        let Some(image) = photo() else {
            return;
        };
        let (w, h) = image.dimensions();
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        app.docs.documents[0].canvas = Canvas::from_rgba(image.into_raw(), w, h);
        app.shell.ui.show_portrait_dialog = true;
        let undo_before = app.docs.documents[0].canvas.undo_count();
        let mut asked = IdPhotoRequest {
            options: IdPhotoOptions::default(),
            nudge: Nudge::default(),
            settings: Some(PortraitSettings::NEUTRAL),
        };
        app.run_id_photo(asked);
        wait(&mut app, "the photo", |app| {
            app.shell.id_photo.job.is_none() && app.shell.portrait.is_some()
        });
        assert!(!app.id_photo_state().error, "{:?}", app.id_photo_state());
        let canvas = &app.docs.documents[0].canvas;
        assert_eq!((canvas.width, canvas.height), id_photo::PRINT_PX);
        assert_eq!(canvas.undo_count(), undo_before + 1);
        assert!(app.id_photo_state().made);
        let first = canvas.flatten_for_export();

        // The person a tenth of the height lower, for a 4×6: no model runs,
        // the history still holds one step, the retouch starts again.
        asked.nudge.down = 0.1;
        asked.options.size = PhotoKind::Id4x6;
        let started = Instant::now();
        app.run_id_photo(asked);
        wait(&mut app, "the photo again", |app| {
            app.shell.id_photo.job.is_none()
        });
        let framed_again = started.elapsed();
        assert!(app.shell.portrait.is_none(), "the old retouch is over");
        wait(&mut app, "the retouch", |app| app.shell.portrait.is_some());
        let canvas = &app.docs.documents[0].canvas;
        assert_eq!((canvas.width, canvas.height), (945, 1417));
        assert_eq!(canvas.undo_count(), undo_before + 1);
        assert!(canvas.flatten_for_export() != first);
        println!("framed again in {} ms", framed_again.as_millis());

        // Blue instead of white is a step of its own, the retouch stays.
        asked.options.backdrop = Backdrop::Blue;
        app.run_id_photo(asked);
        assert!(app.shell.id_photo.job.is_none());
        let canvas = &app.docs.documents[0].canvas;
        assert_eq!(canvas.undo_count(), undo_before + 2);
        let flat = canvas.flatten_for_export();
        assert_eq!(&flat[0..3], &Backdrop::Blue.rgb());
        assert!(app.shell.portrait.is_some());

        // Framed once more, both steps are undone first.
        asked.nudge.down = 0.0;
        app.run_id_photo(asked);
        wait(&mut app, "the photo once more", |app| {
            app.shell.id_photo.job.is_none()
        });
        let canvas = &app.docs.documents[0].canvas;
        assert_eq!(canvas.undo_count(), undo_before + 1);
        assert_eq!(
            &canvas.flatten_for_export()[0..3],
            &Backdrop::Blue.rgb(),
            "{:?}",
            app.id_photo_state()
        );
    }
}

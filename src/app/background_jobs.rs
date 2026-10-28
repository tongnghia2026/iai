//! One of the six App subsystems — see `state.rs` for the tree.

use super::state::*;

/// Background work and its result mailboxes.
///
/// Owns worker channels for loads, RAW previews, the PDF probe/render
/// pipelines, project reloads, printer refresh, the AI engines and the
/// extension bridge. Invariant: results land tagged with the `DocumentId`
/// they were started for; a result whose document is gone (or whose load was
/// cancelled) must be dropped, never applied to whatever tab is active.
pub struct BackgroundJobs {
    /// When Some: a file dialog (Open/Save As/Export) is running on a worker thread;
    /// poll_file_dialog() reads the result via this channel, avoiding blocking the main loop.
    pub(in crate::app) pending_file_dialog:
        Option<std::sync::mpsc::Receiver<crate::file_io::FileDialogResult>>,
    /// A PDF export: its save dialog, then the `export.pdf` job writing it.
    /// Holds the window (modal lock) until it is written.
    pub(in crate::app) pending_pdf_export: Option<PdfExport>,
    /// Picking + decoding an image to insert into a flowing-text document. The
    /// worker runs the native dialog and decodes the file; `poll_flow_text_image`
    /// hands the result to the document editor. `Some(None)` = cancelled.
    #[allow(clippy::type_complexity)]
    pub(in crate::app) pending_flow_image:
        Option<std::sync::mpsc::Receiver<Option<(Vec<u8>, u32, u32)>>>,
    /// Mail merge: the native picker + parse of the CSV/Excel data source runs on
    /// a worker; `poll_mail_merge_data` opens the dialog with the parsed table.
    /// `None` payload = cancelled dialog.
    #[allow(clippy::type_complexity)]
    pub(in crate::app) pending_mail_merge_data: Option<
        std::sync::mpsc::Receiver<
            Option<Result<(std::path::PathBuf, crate::core::mail_merge::MergeTable), String>>,
        >,
    >,
    /// Mail merge: the batch PDF export (folder picker + one PDF per data row)
    /// streams progress and a final summary from a worker thread.
    pub(in crate::app) pending_mail_merge_export:
        Option<std::sync::mpsc::Receiver<crate::app::file_ops::mail_merge::MailMergeProgress>>,
    /// Background image-import jobs. A worker thread decodes each path off the UI
    /// thread and streams `(path, Vec<Canvas>|Err, is_last)` back; multi-page formats such
    /// as PDF produce one canvas per page. `poll_loads()` attaches each finished
    /// document so opening large / many files never blocks the window.
    #[allow(clippy::type_complexity)]
    pub(in crate::app) pending_loads: Vec<
        std::sync::mpsc::Receiver<(
            std::path::PathBuf,
            Result<Vec<crate::core::canvas::Canvas>, String>,
            bool,
        )>,
    >,
    /// Fast embedded-JPEG previews for RAW opens: attached as a placeholder tab
    /// within a fraction of a second so the image shows while the full demosaic
    /// still decodes on `pending_loads`. See `poll_raw_previews`.
    pub(in crate::app) pending_raw_previews: Vec<
        std::sync::mpsc::Receiver<(std::path::PathBuf, crate::formats::raw_preview::RawPreview)>,
    >,
    /// Normalized path key → placeholder document id for RAWs still decoding at
    /// full resolution. When the full decode lands, its canvas replaces the
    /// placeholder's in place (no new tab).
    pub(in crate::app) raw_preview_docs:
        std::collections::HashMap<String, crate::core::document::DocumentId>,
    /// Preview documents whose full RAW decoder failed. Keeping this separate
    /// prevents Develop from treating the embedded JPEG as editable RAW data.
    pub(in crate::app) raw_preview_failures:
        std::collections::HashMap<crate::core::document::DocumentId, String>,
    /// RAW decodes that are still running after the user cancelled their
    /// transient Develop session. Their eventual worker result is discarded
    /// instead of reopening the window the user just closed.
    pub(in crate::app) cancelled_raw_loads: std::collections::HashSet<String>,
    /// Path keys whose generic decode is in flight on `pending_loads`. Guards
    /// against a duplicate load of the same file before its first decode lands
    /// and gets a tab (e.g. a fast double-click on a recent-files card). Cleared
    /// as each result is drained in `poll_loads`.
    pub(in crate::app) loading_keys: std::collections::HashSet<String>,
    /// Pictures being opened by the engine's `file.open` jobs, and their files.
    pub(in crate::app) open_jobs: Vec<(iai_engine::JobId, std::path::PathBuf)>,
    /// Early reduced-size RAW renders made by the same colour chain as the full
    /// decode; each replaces its file's placeholder while the demosaic runs.
    /// See `poll_raw_drafts`.
    #[allow(clippy::type_complexity)]
    pub(in crate::app) pending_raw_drafts:
        Vec<std::sync::mpsc::Receiver<(std::path::PathBuf, crate::core::canvas::Canvas)>>,
    /// Foreground RAW decodes in flight, by normalized path key. Each carries
    /// the flag that abandons it once the user has moved to another image.
    pub(in crate::app) raw_decode_jobs: std::collections::HashMap<String, RawDecodeJob>,
    /// Background decode of a filmstrip neighbour straight into a disk spill.
    pub(in crate::app) raw_prefetch: Option<RawPrefetchJob>,
    /// Documents whose prefetch failed; not retried this session (a real
    /// decoder error surfaces when the user selects the image).
    pub(in crate::app) raw_prefetch_failed:
        std::collections::HashSet<crate::core::document::DocumentId>,
    /// Documents holding a RAW disk spill, least recently used first.
    pub(in crate::app) raw_spill_lru: Vec<crate::core::document::DocumentId>,
    /// The next finished load becomes active immediately for early feedback; once
    /// a multi-file batch finishes, its final successfully loaded document becomes
    /// active (see the `is_last` marker carried by `pending_loads`).
    pub(in crate::app) load_activate_pending: bool,
    /// PDFs opened this batch, waiting for their page-selection dialog. Handled one
    /// at a time: probe -> dialog -> render. See `maybe_start_next_pdf_probe`.
    pub(in crate::app) pending_pdf_probe_queue: std::collections::VecDeque<PathBuf>,
    /// A background PDF probe (parse + page sizes, no raster) is in flight.
    #[allow(clippy::type_complexity)]
    pub(in crate::app) pending_pdf_probe:
        Option<std::sync::mpsc::Receiver<(PathBuf, Result<crate::formats::pdf::PdfProbe, String>)>>,
    /// The page-selection dialog is showing for this PDF.
    pub(in crate::app) pending_pdf_prompt: Option<PdfImportPrompt>,
    /// Initial background render of the first selected PDF page. Remaining
    /// selections stay virtual after this result is attached.
    #[allow(clippy::type_complexity)]
    pub(in crate::app) pending_pdf_render: Option<
        std::sync::mpsc::Receiver<(
            PathBuf,
            Result<Vec<(usize, crate::core::canvas::Canvas)>, String>,
        )>,
    >,
    /// Total page count of the PDF currently rendering, for "Page X of N" labels.
    pub(in crate::app) pdf_render_total_pages: usize,
    /// Group id + source path for the PDF pages currently rendering (tag applied on attach).
    pub(in crate::app) pdf_render_group_id: u32,
    pub(in crate::app) pdf_render_source: PathBuf,
    /// Selection retained while the first page renders.
    pub(in crate::app) pdf_render_selected_pages: Vec<usize>,
    pub(in crate::app) pdf_render_target_dpi: f32,
    /// `(document, target page)` while an on-demand page render is in flight.
    pub(in crate::app) pending_pdf_page_render: Option<(crate::core::document::DocumentId, usize)>,
    /// `(document, physical source page)` to remove after an active PDF page has
    /// switched successfully. Inactive pages are removed immediately.
    pub(in crate::app) pending_pdf_page_delete: Option<(crate::core::document::DocumentId, usize)>,
    /// Image/PDF files dropped from the OS onto a PDF document, staged for one
    /// batched page insert on the next frame (winit sends one event per file).
    pub(in crate::app) dropped_pdf_page_files: Vec<PathBuf>,
    /// Decode of image/PDF files selected for insertion into an existing PDF.
    #[allow(clippy::type_complexity)]
    pub(in crate::app) pending_pdf_page_insert: Option<
        std::sync::mpsc::Receiver<(
            crate::core::document::DocumentId,
            usize,
            Result<Vec<(crate::core::canvas::Canvas, String)>, String>,
        )>,
    >,
    pub(in crate::app) pending_reload_prompt: Option<PendingReloadPrompt>,
    pub(in crate::app) pending_reload_job: Option<
        std::sync::mpsc::Receiver<(
            usize,
            std::path::PathBuf,
            Result<crate::core::canvas::Canvas, String>,
        )>,
    >,
    /// Background loads of `.iai` multi-page PDF projects. Decoded on a worker
    /// thread (link + edited pages), attached by `poll_iai_projects` which also
    /// starts the per-document render service for clean pages.
    #[allow(clippy::type_complexity)]
    pub(in crate::app) pending_iai_projects: Vec<
        std::sync::mpsc::Receiver<(
            std::path::PathBuf,
            Result<crate::formats::iai::IaiPdfProject, String>,
        )>,
    >,
    pub(in crate::app) pending_printer_refresh:
        Option<std::sync::mpsc::Receiver<Result<Vec<crate::core::print::PrinterInfo>, String>>>,
    /// A selected-printer re-read requested while another query was in flight;
    /// run once that query lands so the preview never keeps stale paper.
    pub(in crate::app) printer_refresh_queued: bool,
    /// Extension uploads being flattened + encoded off the UI thread.
    #[cfg(feature = "pro")]
    pub(in crate::app) ext_uploads: Vec<crate::app::ext_bridge::PendingUpload>,
    /// Extension result images being decoded off the UI thread.
    #[cfg(feature = "pro")]
    #[allow(clippy::type_complexity)]
    pub(in crate::app) ext_decodes: Vec<(
        crate::app::ext_bridge::EditOrigin,
        std::sync::mpsc::Receiver<Result<(Vec<u8>, u32, u32), String>>,
    )>,
    /// The native printer-driver property sheet runs on a worker thread.  Calling
    /// `DocumentPropertiesW(DM_IN_PROMPT)` from inside winit's event handler can
    /// re-enter the Windows message loop and leave the borderless main window in
    /// a permanent nested-modal spin (most often after changing Borderless).
    #[allow(clippy::type_complexity)]
    pub(in crate::app) pending_printer_settings: Option<
        std::sync::mpsc::Receiver<(
            String,
            Result<Option<crate::core::print_gdi::PrinterSettings>, String>,
        )>,
    >,
    /// A sheet on its way to the printer through GDI, off the winit thread:
    /// opening the device, the spooler's StartDoc and the band conversion take
    /// a few hundred milliseconds a sheet, more with copies.
    pub(in crate::app) pending_print: Option<PrintJob>,
    /// Sheets printed while one was on its way: sent after it, in turn (the
    /// dialog closes at Print, as Photoshop's does, and work goes on).
    pub(in crate::app) print_queue: std::collections::VecDeque<PrintTask>,
    /// Why a direct print failed in its worker; the next frame sends the page
    /// through the PDF handler instead.
    pub(in crate::app) print_gdi_failed: Option<String>,
    /// The document and page that direct print was of, for that PDF route.
    pub(in crate::app) print_failed_doc: Option<(crate::core::document::DocumentId, usize)>,
    /// Off-thread Shape rasterization in flight. See [`ShapeBakeInFlight`].
    pub(in crate::app) shape_bake: Option<ShapeBakeInFlight>,
    /// Off-thread Path (vector) rasterization in flight — used by the live
    /// scale/rotate and node drags so re-rastering a filled path never stalls
    /// the UI thread. See [`PathBakeInFlight`].
    pub(in crate::app) path_bake: Option<PathBakeInFlight>,
    /// The newest Path bake requested while one was already running (latest
    /// wins): `(layer_id, object)`. `poll_path_bake` starts it once the running
    /// job lands.
    pub(in crate::app) path_bake_next: Option<(u32, crate::core::vector::object::VectorObjectData)>,
    /// Off-thread rasterization of the crisp active-Path display overlay in flight.
    /// Zoom-bucket changes rebuild HERE instead of on the UI thread, so zooming a
    /// large Path never stalls. See [`crate::app::state::DisplayBakeInFlight`].
    pub(in crate::app) display_bake: Option<crate::app::state::DisplayBakeInFlight>,
    /// Newest per-object display batch requested while a batch ran (latest wins).
    pub(in crate::app) display_bake_next: Option<Vec<crate::app::state::PathDisplayCacheKey>>,
    pub(in crate::app) select_subject: crate::core::select_subject::SelectSubjectEngine,
    /// Smart Repair stroke being rebuilt by the AI inpainter off the UI thread.
    pub(in crate::app) repair_ai: Option<RepairAiJob>,
    /// Gemini AI image-edit engine (see core/ai/edit.rs).
    #[cfg(feature = "pro")]
    pub(in crate::app) ai_engine: crate::core::ai::edit::AiEditEngine,
    /// Browser-extension bridge: localhost WS server (see app/ext_bridge.rs).
    #[cfg(feature = "pro")]
    pub(in crate::app) ext: crate::app::ext_bridge::ExtBridge,
}

/// Everything a print worker needs of a sheet, taken when Print is pressed:
/// the page's layers (shared, not copied), its size and resolution, and the
/// print settings then. The page is composited on the worker.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(in crate::app) struct PrintTask {
    pub(in crate::app) doc_id: crate::core::document::DocumentId,
    /// Which page of it, 0 being the first.
    pub(in crate::app) page: usize,
    pub(in crate::app) doc_name: String,
    pub(in crate::app) stack: crate::core::layer::LayerStack,
    pub(in crate::app) width: u32,
    pub(in crate::app) height: u32,
    pub(in crate::app) dpi: f32,
    pub(in crate::app) layout: crate::core::print::PrintLayout,
    /// The printer's RGB profile, when the app manages colour.
    pub(in crate::app) profile: Option<Vec<u8>>,
    pub(in crate::app) printer: String,
    pub(in crate::app) settings: Option<crate::core::print_gdi::PrinterSettings>,
    pub(in crate::app) copies: u32,
}

/// A sheet handed to a print worker (see `BackgroundJobs::pending_print`).
pub(in crate::app) struct PrintJob {
    pub(in crate::app) printer: String,
    /// The document and page printed, for the PDF route if the device
    /// refuses.
    pub(in crate::app) page_of: (crate::core::document::DocumentId, usize),
    /// `Ok`, or why the device refused.
    pub(in crate::app) rx: std::sync::mpsc::Receiver<Result<(), String>>,
    /// Where the worker is, and how it hears the job is no longer wanted.
    pub(in crate::app) progress: std::sync::Arc<crate::core::print_gdi::PrintProgress>,
    /// What the spooler said of the printer as the job began.
    pub(in crate::app) printer_state:
        std::sync::Arc<std::sync::OnceLock<crate::core::print_gdi::PrinterState>>,
    pub(in crate::app) started: std::time::Instant,
    /// The worker's step count at the last look, and when it last differed.
    seen_steps: u32,
    moved_at: std::time::Instant,
    /// The user has been told the printer is not answering.
    pub(in crate::app) told_stuck: bool,
}

impl PrintJob {
    /// A worker that has stood still this long is waiting on a printer that
    /// does not answer: it is no longer waited for (a sheet takes well under
    /// two seconds to hand over, and every band sent counts as a step).
    pub(in crate::app) const STUCK_AFTER: std::time::Duration = std::time::Duration::from_secs(10);
    /// Stopping is offered only once the job has outlived an ordinary one, so
    /// a second click meant for Print cannot land on it.
    pub(in crate::app) const STOP_OFFERED_AFTER: std::time::Duration =
        std::time::Duration::from_secs(3);

    // Off Windows a print goes out as a PDF, with no worker to wait on.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub(in crate::app) fn new(
        printer: String,
        page_of: (crate::core::document::DocumentId, usize),
        rx: std::sync::mpsc::Receiver<Result<(), String>>,
        progress: std::sync::Arc<crate::core::print_gdi::PrintProgress>,
        printer_state: std::sync::Arc<std::sync::OnceLock<crate::core::print_gdi::PrinterState>>,
    ) -> Self {
        let now = std::time::Instant::now();
        Self {
            printer,
            page_of,
            rx,
            seen_steps: progress.steps(),
            progress,
            printer_state,
            started: now,
            moved_at: now,
            told_stuck: false,
        }
    }

    /// Note whether the worker has moved since the last look.
    pub(in crate::app) fn watch(&mut self) {
        let steps = self.progress.steps();
        if steps != self.seen_steps {
            self.seen_steps = steps;
            self.moved_at = std::time::Instant::now();
        }
    }

    /// How long the worker has stood still.
    pub(in crate::app) fn still_for(&self) -> std::time::Duration {
        if self.progress.steps() != self.seen_steps {
            return std::time::Duration::ZERO;
        }
        self.moved_at.elapsed()
    }

    pub(in crate::app) fn is_stuck(&self) -> bool {
        self.still_for() >= Self::STUCK_AFTER
    }

    /// What the spooler said of the printer, in brackets, when it said anything.
    pub(in crate::app) fn printer_says(&self) -> String {
        self.printer_state
            .get()
            .map(|state| state.summary())
            .filter(|summary| !summary.is_empty())
            .map(|summary| format!(" ({summary})"))
            .unwrap_or_default()
    }

    /// What the Print dialog says beside its stop button; `None` while the
    /// job is still young enough to be an ordinary one.
    pub(in crate::app) fn waiting_note(&self) -> Option<String> {
        let waited = self.started.elapsed();
        if waited < Self::STOP_OFFERED_AFTER {
            return None;
        }
        Some(if self.is_stuck() {
            format!(
                "«{}» chưa trả lời {} giây{}",
                self.printer,
                self.still_for().as_secs(),
                self.printer_says()
            )
        } else {
            format!("Đang gửi tới «{}»… {} giây", self.printer, waited.as_secs())
        })
    }

    #[cfg(test)]
    pub(in crate::app) fn stand_still_for(&mut self, time: std::time::Duration) {
        self.moved_at = std::time::Instant::now() - time;
        self.started = self.started.min(self.moved_at);
    }
}

/// A large Smart Repair stroke handed to the AI inpainter (seconds on a CPU).
/// The painted wash stays on screen until `poll_repair_ai` commits the result.
pub(in crate::app) struct RepairAiJob {
    pub(in crate::app) doc_id: u32,
    pub(in crate::app) work: crate::core::canvas::SpotHealWork,
    pub(in crate::app) overlay: Option<std::sync::Arc<crate::ui::CloneSourcePreview>>,
    pub(in crate::app) handle: std::thread::JoinHandle<Option<Vec<u8>>>,
}

/// A foreground RAW decode (see `BackgroundJobs::raw_decode_jobs`).
pub(in crate::app) struct RawDecodeJob {
    /// Target document; `None` until the first open's placeholder exists.
    pub(in crate::app) doc: Option<crate::core::document::DocumentId>,
    pub(in crate::app) cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(in crate::app) started: std::time::Instant,
}

/// A background RAW prefetch (see `BackgroundJobs::raw_prefetch`).
pub(in crate::app) struct RawPrefetchJob {
    pub(in crate::app) doc: crate::core::document::DocumentId,
    pub(in crate::app) cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    #[allow(clippy::type_complexity)]
    pub(in crate::app) rx:
        std::sync::mpsc::Receiver<Result<std::sync::Arc<crate::core::raw_spill::RawSpill>, String>>,
}

/// Where a PDF export is.
pub(in crate::app) enum PdfExport {
    /// The save dialog is open; the command's params wait for its path.
    Asking {
        rx: std::sync::mpsc::Receiver<Option<PathBuf>>,
        args: serde_json::Value,
    },
    /// `export.pdf` writes it.
    Writing(iai_engine::JobId),
}

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
    #[allow(dead_code)]
    pub(in crate::app) bus: SharedBus,
    pub(in crate::app) format_registry: FormatRegistry,
    /// When Some: a file dialog (Open/Save As/Export) is running on a worker thread;
    /// poll_file_dialog() reads the result via this channel, avoiding blocking the main loop.
    pub(in crate::app) pending_file_dialog:
        Option<std::sync::mpsc::Receiver<crate::file_io::FileDialogResult>>,
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
    /// Off-thread Shape rasterization in flight. See [`ShapeBakeInFlight`].
    pub(in crate::app) shape_bake: Option<ShapeBakeInFlight>,
    pub(in crate::app) select_subject: crate::core::select_subject::SelectSubjectEngine,
    /// Gemini AI image-edit engine (see core/ai/edit.rs).
    pub(in crate::app) ai_engine: crate::core::ai::edit::AiEditEngine,
    /// Browser-extension bridge: localhost WS server (see app/ext_bridge.rs).
    pub(in crate::app) ext: crate::app::ext_bridge::ExtBridge,
}

//! One of the six App subsystems — see `state.rs` for the tree.

use super::state::*;

/// Open documents and their lifecycle.
///
/// Owns the tab list, the active index, MRU order, document id allocation and
/// the autosave/crash-recovery anchors. Invariant: `active_doc_idx` always
/// indexes into `documents` (the app never runs with zero documents), and
/// every `DocumentId` in `doc_mru`/`autosave_files` was minted by
/// `next_doc_id`.
pub struct DocumentSession {
    pub(in crate::app) documents: Vec<Document>,
    pub(in crate::app) active_doc_idx: usize,
    /// Document ids ordered most-recently-used first. Closing the active tab
    /// returns to the front-most surviving entry instead of the positional
    /// neighbor, so "edit → copy → close" lands back on the tab the user was
    /// working in just before.
    pub(in crate::app) doc_mru: Vec<DocumentId>,
    pub(in crate::app) current_file: Option<PathBuf>,
    /// Counter for assigning unique DocumentIds to newly created documents.
    pub(in crate::app) next_doc_id: u32,
    /// When non-None, the user confirmed "close without saving" for this doc index.
    pub(in crate::app) pending_close_doc_idx: Option<usize>,
    /// Monotonic id assigned to each PDF import so its pages share a navigator group.
    pub(in crate::app) next_pdf_group_id: u32,
    /// Persistent per-document PDF render workers. Each parses its source once.
    pub(in crate::app) pdf_render_services: std::collections::HashMap<
        crate::core::document::DocumentId,
        crate::formats::pdf::PdfRenderService,
    >,
    /// Autosave/crash recovery: monotonic seconds of the last successful autosave
    /// sweep. Throttles how often dirty documents are mirrored to the recovery dir.
    pub(in crate::app) last_autosave: std::time::Instant,
    /// Per-document recovery file paths in the autosave dir. Cleared on a clean
    /// save/close and on a clean exit; a file left behind marks an unclean shutdown.
    pub(in crate::app) autosave_files:
        std::collections::HashMap<crate::core::document::DocumentId, std::path::PathBuf>,
    /// Runtime materialized embedded PDFs for self-contained `.iai` projects.
    pub(in crate::app) embedded_pdf_files:
        std::collections::HashMap<crate::core::document::DocumentId, std::path::PathBuf>,
    /// One-shot: whether the startup crash-recovery scan has run this session.
    pub(in crate::app) crash_recovery_checked: bool,
}

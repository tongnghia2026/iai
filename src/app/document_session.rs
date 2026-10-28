//! One of the six App subsystems — see `state.rs` for the tree.

use super::state::*;

/// Open documents and their lifecycle.
///
/// The tab list, the active index, MRU order and document id allocation are
/// the engine's [`Tabs`](iai_engine::Tabs) (reached through `Deref`, so
/// `docs.documents` reads as before); this adds what only the window has:
/// the autosave/crash-recovery anchors, the save jobs, the close and exit
/// questions. Invariant: `active_doc_idx` always indexes into `documents`
/// (the app never runs with zero documents), and every `DocumentId` in
/// `doc_mru`/`autosave_files` was minted by `next_doc_id`.
pub struct DocumentSession {
    pub(in crate::app) tabs: iai_engine::Tabs,
    pub(in crate::app) current_file: Option<PathBuf>,
    /// When non-None, the user confirmed "close without saving" for this doc index.
    pub(in crate::app) pending_close_doc_idx: Option<usize>,
    /// Dirty documents still requiring a per-tab decision during app shutdown.
    pub(in crate::app) pending_exit_docs: std::collections::VecDeque<DocumentId>,
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
    /// Recovery write running on a worker thread (at most one).
    pub(in crate::app) autosave_job: Option<super::autosave::AutosaveJob>,
    /// Content fingerprint last mirrored for each document, so an unchanged
    /// document is not re-encoded every period.
    pub(in crate::app) autosave_fingerprints:
        std::collections::HashMap<crate::core::document::DocumentId, u64>,
    /// Failed and skipped recovery writes, and what the user was told of them.
    pub(in crate::app) autosave_health: super::autosave::AutosaveHealth,
    /// Single images being saved or exported on worker threads.
    pub(in crate::app) save_jobs: Vec<super::file_ops::save_export::SaveJob>,
    /// Exclusive lock marking this process alive to other iAi instances, so
    /// their crash recovery never adopts this session's autosave files.
    pub(in crate::app) instance_lock: Option<std::fs::File>,
    /// Runtime materialized embedded PDFs for self-contained `.iai` projects.
    pub(in crate::app) embedded_pdf_files:
        std::collections::HashMap<crate::core::document::DocumentId, std::path::PathBuf>,
    /// One-shot: whether the startup crash-recovery scan has run this session.
    pub(in crate::app) crash_recovery_checked: bool,
}

impl std::ops::Deref for DocumentSession {
    type Target = iai_engine::Tabs;

    fn deref(&self) -> &iai_engine::Tabs {
        &self.tabs
    }
}

impl std::ops::DerefMut for DocumentSession {
    fn deref_mut(&mut self) -> &mut iai_engine::Tabs {
        &mut self.tabs
    }
}

impl DocumentSession {
    /// The document in the active tab.
    pub(in crate::app) fn active(&self) -> &Document {
        &self.documents[self.active_doc_idx]
    }

    /// The document in the active tab, to change it.
    pub(in crate::app) fn active_mut(&mut self) -> &mut Document {
        let tabs = &mut self.tabs;
        &mut tabs.documents[tabs.active_doc_idx]
    }

    /// `current_file` is kept, by hand, equal to the active tab's `path`. Until
    /// it is replaced by that path, a frame that finds the two apart leaves a
    /// line in the flight recorder (once per pair), so the cases can be found.
    pub(in crate::app) fn note_current_file_drift(&self) {
        static REPORTED: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
        let path = self.active().path.as_deref();
        if self.current_file.as_deref() == path {
            return;
        }
        let text = format!(
            "current_file is {:?} but the active tab's path is {path:?}",
            self.current_file
        );
        let mut reported = REPORTED.lock().unwrap_or_else(|e| e.into_inner());
        if *reported != text {
            crate::diag::note("drift", &text);
            *reported = text;
        }
    }
}

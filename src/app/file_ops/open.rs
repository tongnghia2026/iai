//! Opening files: dialogs, path dedupe, async decode landing, RAW previews,
//! reload-from-disk and .iai project loads. Results land tagged by DocumentId.

use super::{file_name, normalized_path_key};
use crate::app::state::App;
use crate::core::canvas::Canvas;
use crate::core::document::file_modified_at;
use crate::file_io;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Multi-RAW sessions present the embedded camera JPEG immediately, then swap
/// in iAi's scene-linear render only for the active image. The placeholder is
/// explicitly marked deferred and its controls stay locked, so it can never be
/// mistaken for the final RAW render or committed as pixels.
fn present_embedded_raw_as_canvas() -> bool {
    true
}

fn direct_decode_raw_on_open(raw_count: usize) -> bool {
    raw_count == 1
}

const RAW_SESSION_PREVIEW_MAX_DIM: u32 = 2048;

#[cfg(test)]
mod raw_transition_tests {
    use super::{
        direct_decode_raw_on_open, interactive_raw_thread_count, present_embedded_raw_as_canvas,
    };

    #[test]
    fn embedded_raw_preview_is_available_as_a_deferred_placeholder() {
        assert!(present_embedded_raw_as_canvas());
    }

    #[test]
    fn interactive_raw_decode_leaves_half_the_logical_cpus_free() {
        assert_eq!(interactive_raw_thread_count(1), 1);
        assert_eq!(interactive_raw_thread_count(8), 4);
        assert_eq!(interactive_raw_thread_count(15), 8);
    }

    #[test]
    fn multi_raw_open_is_preview_first_but_single_raw_keeps_a_decode_fallback() {
        assert!(direct_decode_raw_on_open(1));
        assert!(!direct_decode_raw_on_open(2));
        assert!(!direct_decode_raw_on_open(20));
    }
}

/// Run one import, converting a decoder panic into a normal error. A panic
/// used to kill the worker thread silently and leave the app stuck on
/// "Loading…" with no message (e.g. the old PSD ZIP-compression path).
pub(in crate::app) fn import_guarded(
    registry: &crate::formats::FormatRegistry,
    path: &Path,
) -> Result<Canvas, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| registry.import(path))).unwrap_or_else(
        |payload| {
            let detail = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            Err(if detail.is_empty() {
                "Lỗi giải mã file (decoder panic)".to_string()
            } else {
                format!("Lỗi giải mã file: {detail}")
            })
        },
    )
}

pub(in crate::app) fn import_many_guarded(
    registry: &crate::formats::FormatRegistry,
    path: &Path,
) -> Result<Vec<Canvas>, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| registry.import_many(path)))
        .unwrap_or_else(|payload| {
            let detail = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            Err(if detail.is_empty() {
                "File decoder panicked".to_string()
            } else {
                format!("File decoder panicked: {detail}")
            })
        })
}

fn interactive_raw_thread_count(logical_threads: usize) -> usize {
    logical_threads.div_ceil(2).max(1)
}

/// RAW demosaic can starve input/rendering when it occupies Rayon's global
/// pool. Interactive loads use a bounded local pool so the UI and OS retain
/// roughly half the logical CPUs.
fn import_many_guarded_interactive_raw(
    registry: &crate::formats::FormatRegistry,
    path: &Path,
) -> Result<Vec<Canvas>, String> {
    let logical = std::thread::available_parallelism().map_or(1, usize::from);
    let threads = interactive_raw_thread_count(logical);
    match rayon::ThreadPoolBuilder::new().num_threads(threads).build() {
        Ok(pool) => pool.install(|| import_many_guarded(registry, path)),
        Err(_) => import_many_guarded(registry, path),
    }
}

impl App {
    pub fn do_open(&mut self) {
        if self.jobs.pending_file_dialog.is_some() {
            return;
        }
        let Some(window) = self.win.window.as_ref() else {
            return;
        };
        let parent = file_io::dialog_parent(window);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Some(paths) = file_io::dialog_open_many(parent) {
                if !paths.is_empty() {
                    let _ = tx.send(file_io::FileDialogResult::OpenedMany(paths));
                }
            }
        });
        self.jobs.pending_file_dialog = Some(rx);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    pub(in crate::app) fn find_open_document_by_path(&self, path: &Path) -> Option<usize> {
        let key = normalized_path_key(path);
        self.docs.documents.iter().position(|doc| {
            doc.path
                .as_deref()
                .is_some_and(|open_path| normalized_path_key(open_path) == key)
        })
    }

    pub(in crate::app) fn raw_decode_in_flight_for_doc(
        &self,
        id: crate::core::document::DocumentId,
    ) -> bool {
        self.docs
            .documents
            .iter()
            .find(|doc| doc.id == id)
            .and_then(|doc| doc.path.as_ref())
            .is_some_and(|path| self.jobs.loading_keys.contains(&normalized_path_key(path)))
    }

    pub(in crate::app) fn disk_is_newer_than_document(&self, idx: usize, path: &Path) -> bool {
        let Some(disk_modified) = file_modified_at(path) else {
            return false;
        };
        let Some(doc) = self.docs.documents.get(idx) else {
            return false;
        };
        match doc.file_modified_at {
            Some(known_modified) => disk_modified
                .duration_since(known_modified)
                .is_ok_and(|delta| delta > std::time::Duration::ZERO),
            None => true,
        }
    }

    pub(in crate::app) fn focus_existing_open_path(&mut self, idx: usize, path: PathBuf) {
        if self.docs.active_doc_idx < self.docs.documents.len() {
            self.docs.documents[self.docs.active_doc_idx].reconcile_pdf_page_modified();
        }
        if idx != self.docs.active_doc_idx {
            self.switch_to_doc(idx);
        }
        // Focusing a document that is already open always leaves the welcome
        // screen — e.g. clicking a recent-files card (or re-opening via the
        // dialog) for a file that still has a tab. Without this the welcome
        // stayed up because only the fresh-load path (activate_new_document)
        // cleared it.
        self.shell.ui.show_welcome = false;

        let name = file_name(&path);
        if self.disk_is_newer_than_document(idx, &path) {
            self.jobs.pending_reload_prompt =
                Some(crate::app::state::PendingReloadPrompt { doc_idx: idx, path });
            self.shell.status_msg = format!("Already open: {name} (file changed on disk)");
        } else {
            self.shell.status_msg = format!("Already open: {name}");
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Kick off background import of `paths`. A worker thread decodes each file
    /// (the expensive step) off the UI thread and streams the result back, so
    /// opening a large image — or several at once — never freezes the window.
    /// `poll_loads()` attaches each finished document.
    pub fn start_load_paths(&mut self, paths: Vec<std::path::PathBuf>) {
        let mut queued_keys = HashSet::new();
        let mut paths_to_load: Vec<PathBuf> = Vec::new();
        let mut pdf_enqueued = false;
        for path in paths.into_iter().filter(|p| p.exists()) {
            // PDFs open through the page-selection dialog (one at a time) instead
            // of the generic loader; queue them for probing.
            if crate::formats::pdf::is_pdf_path(&path) {
                let key = normalized_path_key(&path);
                let already_queued = self
                    .jobs
                    .pending_pdf_probe_queue
                    .iter()
                    .any(|p| normalized_path_key(p) == key);
                if !already_queued && queued_keys.insert(key) {
                    self.jobs.pending_pdf_probe_queue.push_back(path);
                    pdf_enqueued = true;
                }
                continue;
            }
            // A multi-page PDF *project* `.iai` opens through its own loader, which
            // rebuilds the whole session; ordinary single-canvas `.iai` still uses
            // the generic image path below.
            let is_iai = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("iai"));
            if is_iai && crate::formats::iai::is_pdf_project(&path) {
                if let Some(idx) = self.find_open_document_by_path(&path) {
                    self.focus_existing_open_path(idx, path);
                    continue;
                }
                if queued_keys.insert(normalized_path_key(&path)) {
                    self.start_load_iai_project(path);
                }
                continue;
            }
            // A multi-page artboard document rebuilds all its pages; ordinary
            // single-canvas `.iai` falls through to the generic image path.
            if is_iai && crate::formats::iai::is_artboard_doc(&path) {
                if let Some(idx) = self.find_open_document_by_path(&path) {
                    self.focus_existing_open_path(idx, path);
                    continue;
                }
                if queued_keys.insert(normalized_path_key(&path)) {
                    self.open_artboard_doc(path);
                }
                continue;
            }
            if let Some(idx) = self.find_open_document_by_path(&path) {
                self.focus_existing_open_path(idx, path);
                continue;
            }
            let key = normalized_path_key(&path);
            // A decode of this file is already running but has not attached its
            // tab yet (e.g. a fast double-click on a recent card): skip the
            // duplicate instead of opening the same image in two tabs.
            if self.jobs.loading_keys.contains(&key) {
                continue;
            }
            if queued_keys.insert(key.clone()) {
                // A fresh open of this path overrides a stale cancel from an
                // earlier Develop session whose decode may still be in flight
                // (otherwise the leftover key would swallow the new preview).
                self.jobs.cancelled_raw_loads.remove(&key);
                paths_to_load.push(path);
            }
        }
        if !paths_to_load.is_empty() {
            let n = paths_to_load.len();
            let raw_paths: Vec<PathBuf> = paths_to_load
                .iter()
                .filter(|p| crate::formats::raw::is_raw_path(p))
                .cloned()
                .collect();
            // Embedded previews build the filmstrip/session in seconds without
            // demosaicing every selected file. They are display-only deferred
            // placeholders; selecting one calls ensure_raw_resident.
            if !raw_paths.is_empty() {
                let preview_paths = raw_paths.clone();
                // Bound decoded preview buffers so a fast extractor cannot
                // queue 20 full embedded JPEG rasters ahead of the UI thread.
                let (preview_tx, preview_rx) = std::sync::mpsc::sync_channel(2);
                std::thread::spawn(move || {
                    for path in preview_paths {
                        if let Some(preview) = crate::formats::raw_preview::extract(&path) {
                            if preview_tx.send((path, preview)).is_err() {
                                break;
                            }
                        }
                    }
                });
                self.jobs.pending_raw_previews.push(preview_rx);
            }

            // Full-decode ordinary raster files as before. A single RAW keeps
            // the direct-load fallback for cameras without an embedded JPEG;
            // a multi-RAW batch starts from previews and attach_raw_preview
            // queues only the active image for full decode.
            let direct_raw = direct_decode_raw_on_open(raw_paths.len()).then(|| &raw_paths[0]);
            let paths_to_decode: Vec<PathBuf> = paths_to_load
                .into_iter()
                .filter(|path| {
                    !crate::formats::raw::is_raw_path(path)
                        || direct_raw.is_some_and(|direct| {
                            normalized_path_key(direct) == normalized_path_key(path)
                        })
                })
                .collect();
            for path in &paths_to_decode {
                self.jobs.loading_keys.insert(normalized_path_key(path));
            }
            if !paths_to_decode.is_empty() {
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    // The registry holds only built-in importers (no runtime plugins), so a
                    // fresh one matches `self.jobs.format_registry` and needs no sharing.
                    let registry = crate::formats::FormatRegistry::new();
                    let path_count = paths_to_decode.len();
                    for (index, path) in paths_to_decode.into_iter().enumerate() {
                        let result = if crate::formats::raw::is_raw_path(&path) {
                            import_many_guarded_interactive_raw(&registry, &path)
                        } else {
                            import_many_guarded(&registry, &path)
                        };
                        if tx.send((path, result, index + 1 == path_count)).is_err() {
                            break; // UI dropped the receiver (app closing) — stop decoding.
                        }
                    }
                });
                self.jobs.pending_loads.push(rx);
            }
            self.jobs.load_activate_pending = true;
            self.shell.status_msg = if n == 1 {
                "Loading…".to_string()
            } else {
                format!("Loading {n} files…")
            };
        }
        if pdf_enqueued {
            self.maybe_start_next_pdf_probe();
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Drain any finished background imports and attach them. Called every frame
    /// while loads are in flight; keeps requesting redraws so results are picked
    /// up promptly without blocking.
    pub fn poll_loads(&mut self) {
        if self.jobs.pending_loads.is_empty() {
            return;
        }
        let mut attached_any = false;
        let mut still_pending = Vec::new();
        for rx in std::mem::take(&mut self.jobs.pending_loads) {
            let mut alive = true;
            loop {
                match rx.try_recv() {
                    Ok((path, Ok(canvases), is_last)) => {
                        self.jobs.loading_keys.remove(&normalized_path_key(&path));
                        // Record the successful open in the recent-files catalog
                        // (Track B) and prime its thumbnail.
                        let dims = canvases.first().map(|c| (c.width, c.height));
                        let page_count = canvases.len();
                        let mut last_attached = None;
                        for (page_index, canvas) in canvases.into_iter().enumerate() {
                            let page = (page_count > 1).then_some((page_index, page_count));
                            last_attached =
                                self.attach_loaded_doc(path.clone(), canvas, page, None);
                            attached_any |= last_attached.is_some();
                        }
                        if let Some((w, h)) = dims {
                            self.record_recent(&path, w, h);
                        }
                        if is_last {
                            if let Some(id) = last_attached {
                                if let Some(idx) =
                                    self.docs.documents.iter().position(|doc| doc.id == id)
                                {
                                    self.switch_to_doc(idx);
                                }
                            }
                        }
                    }
                    Ok((path, Err(e), _is_last)) => {
                        self.jobs.loading_keys.remove(&normalized_path_key(&path));
                        // A failed decode never reached the point that consumes
                        // the preview-luma cache entry — drop it here.
                        crate::formats::raw_preview::forget_cached_mean_luma(&path);
                        let path_key = normalized_path_key(&path);
                        if self.jobs.cancelled_raw_loads.remove(&path_key) {
                            continue;
                        }
                        // Drop the placeholder mapping (the preview tab, if any,
                        // stays — it's the best pixels we have for this file).
                        if let Some(id) = self.jobs.raw_preview_docs.remove(&path_key) {
                            self.jobs.raw_preview_failures.insert(id, e.clone());
                            if let Some(w) = &self.win.develop_window {
                                w.set_title(&self.develop_window_title());
                                w.request_redraw();
                            }
                        }
                        self.shell.status_msg = format!(
                            "Error opening {}: {e}",
                            path.file_name().unwrap_or_default().to_string_lossy()
                        );
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        alive = false;
                        break;
                    }
                }
            }
            if alive {
                still_pending.push(rx);
            }
        }
        self.jobs.pending_loads = still_pending;
        // Memory Milestone M1: run eviction only when a load actually landed.
        // `poll_loads` is pumped on every redraw while any worker lives; doing
        // this unconditionally made the whole document scan a per-frame path.
        if attached_any {
            self.evict_background_raws();
        }
        // A filmstrip click made a deferred RAW active while another RAW was
        // already decoding. Start the queued active image as soon as that one
        // lands; ensure_raw_resident remains a no-op if it is already resident.
        if self
            .docs
            .documents
            .get(self.docs.active_doc_idx)
            .is_some_and(|doc| doc.deferred_raw)
        {
            self.ensure_raw_resident(self.docs.active_doc_idx);
        }
        if !self.jobs.pending_loads.is_empty() {
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
    }

    /// Decode a `.iai` multi-page PDF project on a worker thread. `poll_iai_projects`
    /// rebuilds the session (edited pages + render service) once it finishes.
    pub(in crate::app) fn start_load_iai_project(&mut self, path: PathBuf) {
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_path = path.clone();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                match crate::formats::iai::load(&worker_path) {
                    Ok(crate::formats::iai::IaiLoad::PdfProject(project)) => Ok(project),
                    Ok(crate::formats::iai::IaiLoad::Canvas(_))
                    | Ok(crate::formats::iai::IaiLoad::ArtboardDoc(_)) => {
                        Err("Not a PDF project file".to_string())
                    }
                    Err(error) => Err(error),
                }
            }))
            .unwrap_or_else(|_| Err("iAi decoder panicked".to_string()));
            let _ = tx.send((worker_path, result));
        });
        self.jobs.pending_iai_projects.push(rx);
        self.jobs.load_activate_pending = true;
        self.shell.status_msg = format!("Opening project: {}", file_name(&path));
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Open a multi-page artboard `.iai` document synchronously (no PDF render
    /// worker needed — every page is a stored canvas). The active page becomes the
    /// live canvas; the rest are held in `Document.pages` for the page-tab bar.
    pub(in crate::app) fn open_artboard_doc(&mut self, path: PathBuf) {
        let loaded = match crate::formats::iai::load(&path) {
            Ok(crate::formats::iai::IaiLoad::ArtboardDoc(doc)) => doc,
            Ok(_) => {
                self.shell.status_msg = "Không phải tài liệu đa trang".to_string();
                return;
            }
            Err(error) => {
                self.shell.status_msg = format!("Lỗi mở tài liệu: {error}");
                return;
            }
        };
        self.install_artboard_doc(path, loaded, true);
    }

    /// Attach a loaded multi-page artboard document as the active tab. `mark_clean`
    /// anchors every page to "saved" for a normal open; a crash-recovered document
    /// passes `false` and latches dirty afterwards so it prompts to save. Returns
    /// the new document id.
    pub(in crate::app) fn install_artboard_doc(
        &mut self,
        path: PathBuf,
        loaded: crate::formats::iai::IaiArtboardDoc,
        mark_clean: bool,
    ) -> Option<crate::core::document::DocumentId> {
        let master = loaded.master;
        let mut slots: Vec<Option<Canvas>> = loaded.pages.into_iter().map(Some).collect();
        let active = loaded.active_page.min(slots.len().saturating_sub(1));
        let Some(active_canvas) = slots.get_mut(active).and_then(|s| s.take()) else {
            self.shell.status_msg = "Tài liệu đa trang rỗng".to_string();
            return None;
        };
        self.jobs.load_activate_pending = true;
        let id = self.attach_loaded_doc(path, active_canvas, None, None)?;
        if let Some(doc) = self.docs.documents.iter_mut().find(|d| d.id == id) {
            doc.pages = slots;
            doc.active_artboard = active;
            doc.master = master.map(Box::new);
            doc.editing_master = false;
            if mark_clean {
                doc.mark_saved();
            }
        }
        Some(id)
    }

    /// Install a crash-recovered multi-page document: attach it, point it back at
    /// its original project path (from the sidecar) when known, latch dirty, and
    /// keep updating the recovery file until the user saves for real.
    pub(crate) fn install_artboard_doc_recovered(
        &mut self,
        autosave_path: PathBuf,
        loaded: crate::formats::iai::IaiArtboardDoc,
        project_path: Option<PathBuf>,
    ) {
        let Some(id) = self.install_artboard_doc(autosave_path.clone(), loaded, false) else {
            return;
        };
        if let Some(doc) = self.docs.documents.iter_mut().find(|d| d.id == id) {
            doc.path = project_path.clone();
            doc.file_modified_at = project_path
                .as_deref()
                .and_then(crate::core::document::file_modified_at);
            // Recovered work has no command/checkpoint proving it matches a file.
            doc.canvas.mark_dirty_unconditionally();
        }
        self.docs.current_file = project_path;
        self.docs.autosave_files.insert(id, autosave_path);
    }

    /// Attach any finished `.iai` project loads.
    pub fn poll_iai_projects(&mut self) {
        if self.jobs.pending_iai_projects.is_empty() {
            return;
        }
        let mut still_pending = Vec::new();
        for rx in std::mem::take(&mut self.jobs.pending_iai_projects) {
            match rx.try_recv() {
                Ok((path, Ok(project))) => self.install_pdf_project(path, project),
                Ok((path, Err(e))) => {
                    self.shell.status_msg = format!("Error opening {}: {e}", file_name(&path));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => still_pending.push(rx),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
            }
        }
        self.jobs.pending_iai_projects = still_pending;
        if !self.jobs.pending_iai_projects.is_empty() {
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
    }

    /// Attach a decoded document. The first of a batch becomes active immediately;
    /// intermediate documents attach as background tabs, and `poll_loads` activates
    /// the final document when the batch completes. This limits GPU uploads to the
    /// first and final images instead of flashing through every opened file.
    /// Install `doc` as the active tab — replacing the welcome placeholder or
    /// pushed as a new tab — saving the outgoing view and syncing all GPU state.
    /// Returns the new active index. Caller has already cleared
    /// `load_activate_pending`. Shared by the full-decode and RAW-preview attach.
    pub(in crate::app) fn activate_new_document(
        &mut self,
        doc: crate::core::document::Document,
    ) -> usize {
        // Save the outgoing doc's view unless we're replacing the welcome tab.
        if !self.has_only_welcome_placeholder() {
            self.docs.documents[self.docs.active_doc_idx].saved_zoom = self.edit.view.zoom;
            self.docs.documents[self.docs.active_doc_idx].saved_offset_x = self.edit.view.offset_x;
            self.docs.documents[self.docs.active_doc_idx].saved_offset_y = self.edit.view.offset_y;
            self.docs.documents[self.docs.active_doc_idx].reconcile_pdf_page_modified();
        }
        let new_idx = if self.has_only_welcome_placeholder() {
            self.docs.documents[0] = doc;
            0
        } else {
            self.docs.documents.push(doc);
            self.docs.documents.len() - 1
        };
        self.shell.ui.show_welcome = false;
        self.edit.input.painting = false;
        self.edit.transform_state = None;
        self.edit.pending_stroke_inputs.clear();
        self.shell.canvas_unit = crate::core::units::Unit::Pixels;
        self.docs.active_doc_idx = new_idx;
        // Syncs view + ALL GPU state (texture size, uniforms, recomposite, mask).
        self.refresh_active_document();
        new_idx
    }

    /// Attach any finished RAW previews as placeholder tabs. Called every frame
    /// while a RAW open is in flight (before `poll_loads`) so the image appears
    /// near-instantly, ahead of the full demosaic.
    pub fn poll_raw_previews(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if self.jobs.pending_raw_previews.is_empty() {
            return;
        }
        let mut still_pending = Vec::new();
        for rx in std::mem::take(&mut self.jobs.pending_raw_previews) {
            let mut alive = true;
            loop {
                match rx.try_recv() {
                    Ok((path, preview)) => self.attach_raw_preview(event_loop, path, preview),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        alive = false;
                        break;
                    }
                }
            }
            if alive {
                still_pending.push(rx);
            }
        }
        self.jobs.pending_raw_previews = still_pending;
        if !self.jobs.pending_raw_previews.is_empty() {
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
    }

    /// Show a RAW's embedded-JPEG preview as a placeholder tab. Skipped if the
    /// full decode already produced a document for this path (it won the race).
    pub(in crate::app) fn attach_raw_preview(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        path: std::path::PathBuf,
        preview: crate::formats::raw_preview::RawPreview,
    ) {
        if !present_embedded_raw_as_canvas() {
            return;
        }
        let key = normalized_path_key(&path);
        if self.jobs.cancelled_raw_loads.contains(&key) {
            return;
        }
        if self.jobs.raw_preview_docs.contains_key(&key)
            || self.find_open_document_by_path(&path).is_some()
        {
            crate::formats::raw_preview::forget_cached_mean_luma(&path);
            return;
        }
        let longest = preview.width.max(preview.height).max(1);
        let scale = (RAW_SESSION_PREVIEW_MAX_DIM as f32 / longest as f32).min(1.0);
        let width = ((preview.width as f32 * scale).round() as u32).max(1);
        let height = ((preview.height as f32 * scale).round() as u32).max(1);
        let rgba = if (width, height) == (preview.width, preview.height) {
            preview.rgba
        } else {
            crate::core::canvas::downscale_rgba(
                &preview.rgba,
                preview.width,
                preview.height,
                width,
                height,
            )
        };
        let canvas = Canvas::from_rgba(rgba, width, height);
        let id = crate::core::document::DocumentId(self.docs.next_doc_id);
        self.docs.next_doc_id += 1;
        let mut doc = crate::core::document::Document::from_canvas(id, canvas, Some(path.clone()));
        doc.raw_exif = preview.exif;
        doc.deferred_raw = true;
        let name = file_name(&path);
        if self.jobs.load_activate_pending {
            // The first finished item (preview or full) claims the active tab.
            self.jobs.load_activate_pending = false;
            self.activate_new_document(doc);
            self.shell.status_msg = format!("Opening {name}…");
        } else {
            self.docs.documents.push(doc);
            self.shell.ui.show_welcome = false;
        }
        let Some(new_idx) = self
            .docs
            .documents
            .iter()
            .position(|document| document.id == id)
        else {
            return;
        };
        self.jobs.raw_preview_docs.insert(key, id);
        // Open the actual Develop workflow as soon as the embedded preview is
        // ready. While this id remains in raw_preview_docs the panel presents a
        // decoder state and does not expose RAW controls.
        if self.dev.develop_bake_all.is_some() || self.win.retiring_develop_window.is_some() {
            // Mid-"Open Image" bake, or during the two-phase window teardown,
            // the Develop window cannot host a new session (and switching the
            // active document would disturb the commit in progress). Queue the
            // placeholder; enter_pending_develop opens it once the transition
            // ends.
            self.dev.pending_develop.push(id);
        } else if self.dev.develop_preview.is_some() {
            self.develop_session_push(id, true);
            let title = self.develop_window_title();
            if let Some(w) = &self.win.develop_window {
                w.set_title(&title);
                w.request_redraw();
            }
        } else {
            if self.docs.active_doc_idx != new_idx {
                self.switch_to_doc(new_idx);
            }
            self.open_develop_window(event_loop);
            if self.dev.develop_preview.is_some() {
                self.develop_session_mark_transient(id);
            }
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        // The first preview is the initial active filmstrip image. Materialize
        // only that one; background previews stay light until clicked.
        if new_idx == self.docs.active_doc_idx {
            self.ensure_raw_resident(new_idx);
        }
    }

    /// Replace a RAW placeholder's low-res preview canvas with the full decode
    /// and queue the document for the Develop stage — every RAW of a multi-open
    /// batch joins the session (the first to land opens the Develop window; the
    /// rest append to its filmstrip without stealing the active image).
    pub(in crate::app) fn replace_preview_with_full(&mut self, idx: usize, canvas: Canvas) {
        let id = self.docs.documents[idx].id;
        let active = idx == self.docs.active_doc_idx;
        let in_develop = self.dev.develop_session.iter().any(|entry| entry.doc == id)
            || self
                .dev
                .develop_bake_all
                .as_ref()
                .is_some_and(|state| state.pending.iter().any(|(doc, _)| *doc == id));
        let settings = self
            .dev
            .develop_session
            .iter()
            .find(|entry| entry.doc == id)
            .map(|entry| entry.settings.clone())
            .unwrap_or_default();
        if active
            && self
                .dev
                .develop_preview
                .as_ref()
                .is_some_and(|preview| preview.doc_id == id)
        {
            self.cancel_develop_preview();
        }
        // Force a re-fit for the full-resolution canvas (the preview was lower-res).
        self.docs.documents[idx].saved_zoom = 0.0;
        self.docs.documents[idx].canvas = canvas;
        // The full-resolution buffers are back: this is no longer a Memory
        // Milestone M1 deferred/evicted placeholder.
        self.docs.documents[idx].deferred_raw = false;
        self.jobs.raw_preview_failures.remove(&id);
        self.dev.develop_thumbs.remove(&id);
        if active {
            self.refresh_active_document();
        }
        if in_develop {
            if active && self.dev.develop_bake_all.is_none() {
                self.begin_develop_preview(settings);
                self.dev.develop_view_fit = true;
                self.dev.develop_composited_view = None;
            }
            let title = self.develop_window_title();
            if let Some(w) = &self.win.develop_window {
                w.set_title(&title);
                w.request_redraw();
            }
        } else {
            self.dev.pending_develop.push(id);
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    pub(in crate::app) fn attach_loaded_doc(
        &mut self,
        path: std::path::PathBuf,
        canvas: Canvas,
        page: Option<(usize, usize)>,
        pdf_page: Option<crate::core::document::PdfPageRef>,
    ) -> Option<crate::core::document::DocumentId> {
        let path_key = normalized_path_key(&path);
        if self.jobs.cancelled_raw_loads.remove(&path_key) {
            return None;
        }
        let is_raw = crate::formats::raw::is_raw_path(&path);
        // A RAW whose fast preview is already showing: swap the full decode into
        // that placeholder tab instead of opening a second one for the same file.
        if pdf_page.is_none() && page.is_none() {
            if let Some(existing_id) = self.jobs.raw_preview_docs.remove(&path_key) {
                if let Some(idx) = self.docs.documents.iter().position(|d| d.id == existing_id) {
                    self.replace_preview_with_full(idx, canvas);
                    return Some(existing_id);
                }
                // Placeholder was closed before the decode finished — fall through
                // and attach as a fresh document.
            }
            // A cancel + immediate re-open leaves TWO decodes of the same RAW in
            // flight; the first to land filled the placeholder above, so a
            // second result for an already-open path is a duplicate — drop it.
            if is_raw && self.find_open_document_by_path(&path).is_some() {
                return None;
            }
        }
        let (w, h) = (canvas.width, canvas.height);
        let id = crate::core::document::DocumentId(self.docs.next_doc_id);
        self.docs.next_doc_id += 1;
        let is_pdf = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"));
        let document_path = (!is_pdf).then(|| path.clone());
        let mut doc = crate::core::document::Document::from_canvas(id, canvas, document_path);
        doc.pdf_page = pdf_page;
        let file_name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let name = match page {
            Some((page_index, page_count)) => {
                let title = format!("{file_name} - Page {}", page_index + 1);
                doc.title = title.clone();
                format!("{title} of {page_count}")
            }
            None => {
                if is_pdf {
                    doc.title = file_name.clone();
                }
                file_name
            }
        };

        if self.jobs.load_activate_pending {
            self.jobs.load_activate_pending = false;
            self.activate_new_document(doc);
            let viewport_streaming = self
                .win
                .gpu
                .as_ref()
                .map_or(false, |g| !g.compositor.canvas_space);
            self.shell.status_msg = if viewport_streaming {
                format!("Opened: {name} ({w}×{h}) — Viewport Streaming mode")
            } else {
                format!("Opened: {name} ({w}x{h})")
            };
            // RAW files open into the Develop stage (slice R2) rather than directly
            // as an editable document — entered next frame, after the load attaches.
            if is_raw {
                self.dev.pending_develop.push(id);
            }
        } else {
            // Background tab: CPU-only attach, no GPU work until the user selects it.
            self.docs.documents.push(doc);
            self.shell.ui.show_welcome = false;
            self.shell.status_msg = format!("Loaded: {name}");
            if let Some(win) = &self.win.window {
                win.request_redraw();
            }
            // A RAW without an extractable embedded preview must still join a
            // multi-file Develop session when its full decode completes.
            if is_raw {
                self.dev.pending_develop.push(id);
            }
        }
        Some(id)
    }

    /// Take the file-dialog result (if the worker thread is done) and run the
    /// matching action. Called every frame in RedrawRequested.
    pub fn poll_file_dialog(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        let result = match self.jobs.pending_file_dialog.as_ref() {
            Some(rx) => rx.try_recv(),
            None => return,
        };
        match result {
            Ok(result) => {
                self.jobs.pending_file_dialog = None;
                match result {
                    file_io::FileDialogResult::OpenedMany(paths) => {
                        self.start_load_paths(paths);
                    }
                    file_io::FileDialogResult::SaveAs(mut path) => {
                        if path.extension().is_none() {
                            path.set_extension("iai");
                        }
                        self.save_to(&path);
                    }
                    file_io::FileDialogResult::Export(fmt, path) => {
                        self.do_export(fmt, &path.to_string_lossy());
                    }
                    file_io::FileDialogResult::PickedFolder(dir) => {
                        // Library grid (Track B): scan the folder for images and
                        // show its grid (a picker can only be opened from Library).
                        let count = {
                            self.lib.grid.scan(dir);
                            self.lib.grid.entries.len()
                        };
                        self.shell.ui.show_library = true;
                        self.shell.ui.show_welcome = false;
                        self.shell.status_msg = format!("Library: {count} images");
                        if let Some(w) = &self.win.window {
                            w.request_redraw();
                        }
                    }
                }
                if self.shell.close_requested {
                    self.shell.close_requested = false;
                    self.execute_close();
                }
                if self.shell.exit_save_pending {
                    self.shell.exit_save_pending = false;
                    if !self.docs.documents[self.docs.active_doc_idx].is_modified() {
                        self.docs.pending_exit_docs.pop_front();
                    }
                    self.present_next_exit_document();
                }
                if self.shell.exit_requested {
                    self.shell.exit_requested = false;
                    self.clear_all_autosave();
                    event_loop.exit();
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.jobs.pending_file_dialog = None;
                if self.shell.exit_save_pending {
                    self.shell.exit_save_pending = false;
                    self.shell.ui.show_exit_dialog = true;
                }
                self.shell.exit_requested = false;
                self.shell.close_requested = false;
            }
        }
    }

    pub fn confirm_reload_open_file(&mut self) {
        if self.jobs.pending_reload_job.is_some() {
            return;
        }
        let Some(prompt) = self.jobs.pending_reload_prompt.take() else {
            return;
        };
        if prompt.doc_idx >= self.docs.documents.len() {
            return;
        }
        let idx = prompt.doc_idx;
        let path = prompt.path;
        let name = file_name(&path);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let registry = crate::formats::FormatRegistry::new();
            let result = import_guarded(&registry, &path);
            let _ = tx.send((idx, path, result));
        });
        self.jobs.pending_reload_job = Some(rx);
        self.shell.status_msg = format!("Updating from disk: {name}");
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    pub fn cancel_reload_open_file(&mut self) {
        self.jobs.pending_reload_prompt = None;
        self.shell.status_msg = "Kept the open iAi version".to_string();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    pub fn poll_reload_open_file(&mut self) {
        let result = match self.jobs.pending_reload_job.as_ref() {
            Some(rx) => rx.try_recv(),
            None => return,
        };
        match result {
            Ok((idx, path, Ok(canvas))) => {
                self.jobs.pending_reload_job = None;
                let name = file_name(&path);
                if idx >= self.docs.documents.len()
                    || self.docs.documents[idx]
                        .path
                        .as_deref()
                        .is_none_or(|open_path| {
                            normalized_path_key(open_path) != normalized_path_key(&path)
                        })
                {
                    self.shell.status_msg = format!("Reload skipped: {name}");
                    return;
                }

                let size_changed = {
                    let doc = &self.docs.documents[idx];
                    doc.canvas.width != canvas.width || doc.canvas.height != canvas.height
                };
                let doc = &mut self.docs.documents[idx];
                doc.canvas = canvas;
                doc.path = Some(path.clone());
                doc.file_modified_at = file_modified_at(&path);
                doc.mark_saved();
                doc.title = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Untitled")
                    .to_string();
                if size_changed {
                    doc.saved_zoom = 0.0;
                }

                if idx == self.docs.active_doc_idx {
                    self.docs.current_file = Some(path);
                    self.refresh_active_document();
                } else if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
                self.shell.status_msg = format!("Updated from disk: {name}");
            }
            Ok((_, path, Err(e))) => {
                self.jobs.pending_reload_job = None;
                self.shell.status_msg = format!("Error updating {}: {e}", file_name(&path));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.jobs.pending_reload_job = None;
                self.shell.status_msg = "Reload worker stopped".to_string();
            }
        }
    }

    /// Memory Milestone M1: keep the active image — plus the single
    /// most-recently-used other RAW (a one-image keep so A/B switching between
    /// two photos doesn't re-decode) — full-resolution, and evict every other
    /// still-transient RAW develop document to a thumbnail. Cheap and
    /// idempotent; already-light or non-evictable documents are skipped.
    pub(in crate::app) fn evict_background_raws(&mut self) {
        // The sequential Open Image queue deliberately rehydrates one deferred
        // RAW at a time. Evicting from poll_loads before that queue can bake it
        // creates a decode/evict loop and can pin CPU at full utilization.
        if self.dev.develop_bake_all.is_some() {
            return;
        }
        let active_id = self
            .docs
            .documents
            .get(self.docs.active_doc_idx)
            .map(|d| d.id);
        // Most-recently-used resident RAW other than the active one, kept so the
        // common two-image compare stays instant. `doc_mru` is newest-first.
        let keep_extra =
            self.docs.doc_mru.iter().copied().find(|id| {
                Some(*id) != active_id
                    && self.docs.documents.iter().any(|d| {
                        d.id == *id && !d.deferred_raw && d.canvas.develop_source.is_some()
                    })
            });
        for idx in 0..self.docs.documents.len() {
            if idx == self.docs.active_doc_idx {
                continue;
            }
            if keep_extra.is_some() && Some(self.docs.documents[idx].id) == keep_extra {
                continue;
            }
            self.evict_raw_document(idx);
        }
    }

    /// Evict one non-active RAW document's full-resolution buffers (tiles +
    /// `develop_source`), replacing its canvas with a thumbnail and marking it
    /// [`Document::deferred_raw`]. The RAW at `path` is re-decoded on demand when
    /// the user activates it. No-op unless the document is safe to throw away and
    /// rebuild: a transient (uncommitted) RAW develop image that is not active,
    /// not already light, has no decode in flight and no live preview.
    pub(in crate::app) fn evict_raw_document(&mut self, idx: usize) {
        if idx == self.docs.active_doc_idx || idx >= self.docs.documents.len() {
            return;
        }
        let doc = &self.docs.documents[idx];
        if doc.deferred_raw {
            return;
        }
        // A committed "Open Image" raster (no scene master) or an edited/raster
        // document must never be discarded. Only a live RAW render qualifies.
        if doc.canvas.develop_source.is_none() {
            return;
        }
        // Multi-page / master documents are never RAW develop docs; guard anyway.
        if !doc.pages.is_empty() || doc.master.is_some() {
            return;
        }
        let Some(path) = doc.path.clone() else {
            return;
        };
        if !crate::formats::raw::is_raw_path(&path) {
            return;
        }
        let id = doc.id;
        // Must still be a transient develop-session import (Cancel would close
        // it), so its only "edits" are Develop settings — which are persisted in
        // the session entry and re-applied on re-decode.
        let is_transient = self
            .dev
            .develop_session
            .iter()
            .any(|e| e.doc == id && e.transient);
        if !is_transient {
            return;
        }
        // A live preview or an in-flight decode means this image is really in
        // play; leave it resident.
        if self
            .dev
            .develop_preview
            .as_ref()
            .is_some_and(|p| p.doc_id == id)
        {
            return;
        }
        let key = normalized_path_key(&path);
        if self.jobs.loading_keys.contains(&key) {
            return;
        }
        let thumb = self.docs.documents[idx].canvas.downscaled_thumbnail(2048);
        self.docs.documents[idx].canvas = thumb;
        self.docs.documents[idx].deferred_raw = true;
        // A later decode of this path swaps the full image back into THIS doc
        // (attach_loaded_doc routes through raw_preview_docs).
        self.jobs.raw_preview_docs.insert(key, id);
        // The cached develop thumbnail is rebuilt from the new state on demand.
        self.dev.develop_thumbs.remove(&id);
    }

    /// Memory Milestone M1: make an evicted RAW document full-resolution again by
    /// re-decoding it off-thread. The decode lands in `poll_loads`, where
    /// `raw_preview_docs` routes it back into this document via
    /// `replace_preview_with_full` (which also re-enters the Develop preview with
    /// the entry's saved settings). No-op unless the document is deferred.
    pub(in crate::app) fn ensure_raw_resident(&mut self, idx: usize) {
        let Some(doc) = self.docs.documents.get(idx) else {
            return;
        };
        if !doc.deferred_raw {
            return;
        }
        let Some(path) = doc.path.clone() else {
            return;
        };
        let id = doc.id;
        // A user-initiated filmstrip switch has already made this document
        // active before asking for residency. The Open Image batch, however,
        // rehydrates background documents without changing the visible tab.
        let activate_on_load = idx == self.docs.active_doc_idx;
        let key = normalized_path_key(&path);
        // Already re-decoding (e.g. a double activation): nothing to do.
        if self.jobs.loading_keys.contains(&key) {
            return;
        }
        let another_raw_is_loading = self.docs.documents.iter().any(|candidate| {
            candidate.path.as_ref().is_some_and(|candidate_path| {
                crate::formats::raw::is_raw_path(candidate_path)
                    && self
                        .jobs
                        .loading_keys
                        .contains(&normalized_path_key(candidate_path))
            })
        });
        if another_raw_is_loading {
            self.shell.status_msg = format!("Đang xếp hàng RAW: {}", file_name(&path));
            return;
        }
        // Route the decode back into this exact document.
        self.jobs.raw_preview_docs.insert(key.clone(), id);
        self.jobs.cancelled_raw_loads.remove(&key);
        self.jobs.loading_keys.insert(key);
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_path = path.clone();
        std::thread::spawn(move || {
            let registry = crate::formats::FormatRegistry::new();
            let result = import_many_guarded_interactive_raw(&registry, &worker_path);
            let _ = tx.send((worker_path, result, activate_on_load));
        });
        self.jobs.pending_loads.push(rx);
        self.shell.status_msg = format!("Đang tải lại RAW: {}", file_name(&path));
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
}

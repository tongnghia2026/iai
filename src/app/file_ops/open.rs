//! Opening files: dialogs, path dedupe, async decode landing, RAW previews,
//! reload-from-disk and .iai project loads. Results land tagged by DocumentId.

use super::{file_name, normalized_path_key};
use crate::app::state::App;
use crate::core::canvas::Canvas;
use crate::core::document::{file_modified_at, DocumentId};
use crate::core::raw_spill::RawSpill;
use crate::file_io;
use crate::formats::raw::RAW_DECODE_CANCELLED;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Multi-RAW sessions present the embedded camera JPEG immediately, then swap
/// in iAi's scene-linear render only for the active image. The placeholder is
/// explicitly marked deferred and its controls stay locked, so it can never be
/// mistaken for the final RAW render or committed as pixels.
fn present_embedded_raw_as_canvas() -> bool {
    true
}

const RAW_SESSION_PREVIEW_MAX_DIM: u32 = 2048;

/// How long an embedded preview (or a filmstrip switch) waits for the
/// colour-true draft before showing what it has.
const RAW_DRAFT_WAIT: Duration = Duration::from_millis(1200);

/// Error text of a spill that could not be read back; the image is decoded
/// again instead.
const RAW_SPILL_LOST: &str = "RAW spill unavailable";

/// Disk budget for parked RAW images, and free space always left on the
/// temp volume.
const RAW_SPILL_BUDGET: u64 = 8 << 30;
const RAW_SPILL_FREE_RESERVE: u64 = 10 << 30;

/// Filmstrip neighbours prefetched in the background, in order of preference.
const RAW_PREFETCH_OFFSETS: [isize; 3] = [1, -1, 2];

/// Logical CPUs for a RAW decode the user is waiting on. The Develop panel is
/// locked meanwhile, so only the UI thread and the GPU driver need headroom.
fn foreground_raw_thread_count(logical_threads: usize) -> usize {
    logical_threads.saturating_sub(2).max(1)
}

/// Background prefetch leaves roughly half the logical CPUs free for editing.
fn background_raw_thread_count(logical_threads: usize) -> usize {
    logical_threads.div_ceil(2).max(1)
}

#[cfg(test)]
mod raw_transition_tests {
    use super::{
        background_raw_thread_count, foreground_raw_thread_count, present_embedded_raw_as_canvas,
    };

    #[test]
    fn embedded_raw_preview_is_available_as_a_deferred_placeholder() {
        assert!(present_embedded_raw_as_canvas());
    }

    #[test]
    fn foreground_raw_decode_keeps_two_logical_cpus_for_the_ui() {
        assert_eq!(foreground_raw_thread_count(1), 1);
        assert_eq!(foreground_raw_thread_count(2), 1);
        assert_eq!(foreground_raw_thread_count(16), 14);
    }

    #[test]
    fn background_raw_prefetch_leaves_half_the_logical_cpus_free() {
        assert_eq!(background_raw_thread_count(1), 1);
        assert_eq!(background_raw_thread_count(8), 4);
        assert_eq!(background_raw_thread_count(15), 8);
    }

    use crate::app::state::App;
    use crate::core::canvas::Canvas;
    use crate::core::develop_scene::{f32_to_f16_bits, SceneSource};
    use crate::core::document::{Document, DocumentId};
    use std::sync::Arc;

    /// A decoded-RAW-shaped canvas: 16-bit tiles plus an opaque scene master.
    fn raw_like_canvas(w: u32, h: u32, seed: usize) -> Canvas {
        let n = (w * h) as usize;
        let mut half = vec![0x3c00u16; n * 4];
        let mut px16 = vec![u16::MAX; n * 4];
        for i in 0..n {
            for c in 0..3 {
                half[i * 4 + c] = f32_to_f16_bits(((i + seed) * 3 + c) as f32 * 1e-4);
                px16[i * 4 + c] = ((i * 7 + c * 13 + seed) % 65536) as u16;
            }
        }
        let mut canvas = Canvas::from_rgba16(px16, w, h);
        let mut scene = SceneSource::new(w, h);
        scene.half = half;
        canvas.develop_source = Some(Arc::new(scene));
        canvas
    }

    /// Two transient RAW Develop documents; the first one is active.
    fn raw_session() -> (App, DocumentId, DocumentId) {
        let mut app = App::new();
        let mut ids = Vec::new();
        for (seed, name) in ["a.nef", "b.nef"].into_iter().enumerate() {
            let id = DocumentId(app.docs.next_doc_id);
            app.docs.next_doc_id += 1;
            let path = std::env::temp_dir()
                .join(format!("iai-no-such-dir-{seed}"))
                .join(name);
            app.docs.documents.push(Document::from_canvas(
                id,
                raw_like_canvas(96, 64, seed),
                Some(path),
            ));
            app.develop_session_push(id, true);
            ids.push(id);
        }
        let first = app
            .docs
            .documents
            .iter()
            .position(|d| d.id == ids[0])
            .unwrap();
        app.docs.active_doc_idx = first;
        (app, ids[0], ids[1])
    }

    fn doc_idx(app: &App, id: DocumentId) -> usize {
        app.docs.documents.iter().position(|d| d.id == id).unwrap()
    }

    fn fingerprint(canvas: &Canvas) -> (Vec<u16>, Vec<u16>) {
        (
            canvas.develop_source.as_ref().unwrap().half.clone(),
            canvas.layer_stack.layers[0].tiles.flatten16(),
        )
    }

    /// Wait for the single RAW job on `pending_loads` and route its result
    /// the way `poll_loads` does (without touching the recent-files catalog).
    fn land_raw_job(app: &mut App) {
        let rx = app.jobs.pending_loads.pop().expect("a RAW job was started");
        let (path, result, _) = rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the RAW job finished");
        let key = super::normalized_path_key(&path);
        app.jobs.loading_keys.remove(&key);
        app.jobs.raw_decode_jobs.remove(&key);
        let canvas = result.expect("restore succeeded").pop().unwrap();
        app.attach_loaded_doc(path, canvas, None, None);
    }

    #[test]
    fn a_parked_raw_is_spilled_and_read_back_without_decoding() {
        let (mut app, _a, b) = raw_session();
        let expected = fingerprint(&app.docs.documents[doc_idx(&app, b)].canvas);

        app.evict_raw_document(doc_idx(&app, b));
        let parked = &app.docs.documents[doc_idx(&app, b)];
        assert!(parked.deferred_raw);
        assert!(parked.canvas.develop_source.is_none(), "RAM copy released");
        assert!(parked.raw_spill.is_some(), "decode parked on disk");

        app.ensure_raw_resident(doc_idx(&app, b));
        assert!(
            app.jobs.raw_decode_jobs.is_empty(),
            "a spilled image is read back, not decoded"
        );
        land_raw_job(&mut app);

        let doc = &app.docs.documents[doc_idx(&app, b)];
        assert!(!doc.deferred_raw);
        assert_eq!(fingerprint(&doc.canvas), expected);
        // Parking it again reuses the spill instead of writing a new one.
        let spill = doc.raw_spill.clone().unwrap();
        app.evict_raw_document(doc_idx(&app, b));
        assert!(Arc::ptr_eq(
            app.docs.documents[doc_idx(&app, b)]
                .raw_spill
                .as_ref()
                .unwrap(),
            &spill
        ));
    }

    #[test]
    fn a_filmstrip_click_on_a_parked_raw_waits_for_it_then_switches() {
        let (mut app, a, b) = raw_session();
        app.evict_raw_document(doc_idx(&app, b));

        app.develop_session_activate(b);
        // The current image stays up while the target is read back.
        assert_eq!(app.docs.documents[app.docs.active_doc_idx].id, a);
        assert_eq!(app.dev.develop_switch_pending.map(|(id, _)| id), Some(b));
        app.poll_develop_switch();
        assert_eq!(app.docs.documents[app.docs.active_doc_idx].id, a);

        land_raw_job(&mut app);
        // The restored target must not be parked again before the switch.
        app.evict_background_raws();
        assert!(!app.docs.documents[doc_idx(&app, b)].deferred_raw);
        app.poll_develop_switch();
        assert_eq!(app.docs.documents[app.docs.active_doc_idx].id, b);
        assert!(app.dev.develop_switch_pending.is_none());
    }

    #[test]
    fn a_cancelled_raw_decode_leaves_the_image_parked_not_failed() {
        let (mut app, _a, b) = raw_session();
        let b_idx = doc_idx(&app, b);
        let path = app.docs.documents[b_idx].path.clone().unwrap();
        let key = super::normalized_path_key(&path);
        app.docs.documents[b_idx].deferred_raw = true;
        app.jobs.raw_preview_docs.insert(key.clone(), b);
        app.jobs.loading_keys.insert(key.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send((path, Err(super::RAW_DECODE_CANCELLED.to_string()), false))
            .unwrap();
        app.jobs.pending_loads.push(rx);

        app.poll_loads();

        assert!(!app.jobs.loading_keys.contains(&key));
        assert_eq!(app.jobs.raw_preview_docs.get(&key), Some(&b));
        assert!(!app.jobs.raw_preview_failures.contains_key(&b));
        assert!(app.docs.documents[doc_idx(&app, b)].deferred_raw);
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_default()
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
            let detail = panic_message(payload);
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
            let detail = panic_message(payload);
            Err(if detail.is_empty() {
                "File decoder panicked".to_string()
            } else {
                format!("File decoder panicked: {detail}")
            })
        })
}

/// Decode one RAW in a bounded local pool (Rayon's global pool would starve
/// input and rendering), abandoning it when `cancel` is raised and sending the
/// colour-true draft to `draft_tx`. Background work runs below normal
/// priority. A decoder panic becomes an ordinary error.
fn decode_raw_job(
    path: &Path,
    background: bool,
    cancel: &AtomicBool,
    draft_tx: Option<std::sync::mpsc::Sender<(PathBuf, Canvas)>>,
) -> Result<Vec<Canvas>, String> {
    let logical = std::thread::available_parallelism().map_or(1, usize::from);
    let mut builder = rayon::ThreadPoolBuilder::new();
    if background {
        builder = builder
            .num_threads(background_raw_thread_count(logical))
            .start_handler(|_| crate::core::hw::lower_current_thread_priority());
    } else {
        builder = builder.num_threads(foreground_raw_thread_count(logical));
    }
    let draft_path = path.to_path_buf();
    let draft_sink = draft_tx.map(|tx| {
        let tx = std::sync::Mutex::new(tx);
        move |canvas: Canvas| {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send((draft_path.clone(), canvas));
            }
        }
    });
    let run = || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let control = crate::formats::raw::RawDecodeControl {
                cancel: Some(cancel),
                draft: draft_sink
                    .as_ref()
                    .map(|sink| sink as &(dyn Fn(Canvas) + Sync)),
            };
            crate::formats::raw::decode_raw_controlled(path, control).map(|canvas| vec![canvas])
        }))
        .unwrap_or_else(|payload| {
            let detail = panic_message(payload);
            Err(if detail.is_empty() {
                "File decoder panicked".to_string()
            } else {
                format!("File decoder panicked: {detail}")
            })
        })
    };
    match builder.build() {
        Ok(pool) => pool.install(run),
        Err(_) => run(),
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
            // Flowing-text `.iai` files are model documents, not raster imports.
            // Route them before the generic importer so they never collapse into
            // the 1x1 compatibility canvas.
            if is_iai && crate::formats::iai::is_flow_text_doc(&path) {
                if let Some(idx) = self.find_open_document_by_path(&path) {
                    self.focus_existing_open_path(idx, path);
                    continue;
                }
                if queued_keys.insert(normalized_path_key(&path)) {
                    self.open_flow_text_doc(path);
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

            // Full-decode ordinary raster files as before. Of the RAWs only the
            // first (the image the session opens on) is decoded now, at full
            // speed and with an early draft; the rest stay previews until they
            // are selected or prefetched.
            let paths_to_decode: Vec<PathBuf> = paths_to_load
                .into_iter()
                .filter(|path| !crate::formats::raw::is_raw_path(path))
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
                        let result = import_many_guarded(&registry, &path);
                        if tx.send((path, result, index + 1 == path_count)).is_err() {
                            break; // UI dropped the receiver (app closing) — stop decoding.
                        }
                    }
                });
                self.jobs.pending_loads.push(rx);
            }
            if let Some(first_raw) = raw_paths.first() {
                self.start_foreground_raw_decode(first_raw.clone(), None);
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
                        self.jobs
                            .raw_decode_jobs
                            .remove(&normalized_path_key(&path));
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
                        let path_key = normalized_path_key(&path);
                        self.jobs.loading_keys.remove(&path_key);
                        self.jobs.raw_decode_jobs.remove(&path_key);
                        if e == RAW_DECODE_CANCELLED {
                            // Abandoned because the user moved on: the document
                            // stays deferred and its preview stats stay cached.
                            self.jobs.cancelled_raw_loads.remove(&path_key);
                            continue;
                        }
                        if e == RAW_SPILL_LOST {
                            // Decode it again instead (see the end of this poll).
                            if let Some(doc) =
                                self.jobs.raw_preview_docs.get(&path_key).and_then(|id| {
                                    self.docs.documents.iter_mut().find(|d| d.id == *id)
                                })
                            {
                                doc.raw_spill = None;
                            }
                            continue;
                        }
                        // A failed decode never reached the point that consumes
                        // the preview-luma cache entry — drop it here.
                        crate::formats::raw_preview::forget_cached_mean_luma(&path);
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
        // Likewise for a filmstrip image waiting to become active.
        if let Some(idx) = self.dev.develop_switch_pending.and_then(|(id, _)| {
            self.docs
                .documents
                .iter()
                .position(|doc| doc.id == id && doc.deferred_raw)
        }) {
            self.ensure_raw_resident(idx);
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
                    | Ok(crate::formats::iai::IaiLoad::ArtboardDoc(_))
                    | Ok(crate::formats::iai::IaiLoad::FlowTextDocument(_)) => {
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

    /// Open a lightweight flowing-text `.iai` as a normal application tab.
    /// The editor/layout cache is rebuilt lazily by the UI from this canonical
    /// model, so opening does not allocate page rasters or touch the compositor.
    pub(in crate::app) fn open_flow_text_doc(&mut self, path: PathBuf) {
        let loaded = match crate::formats::iai::load(&path) {
            Ok(crate::formats::iai::IaiLoad::FlowTextDocument(document)) => document,
            Ok(_) => {
                self.shell.status_msg = "Không phải tài liệu văn bản".to_string();
                return;
            }
            Err(error) => {
                self.shell.status_msg = format!("Lỗi mở văn bản: {error}");
                return;
            }
        };
        let id = crate::core::document::DocumentId(self.docs.next_doc_id);
        self.docs.next_doc_id += 1;
        let mut doc = crate::core::document::Document::new_flow_text(id);
        let mut flow_text = crate::core::document::FlowTextDocumentState::from_backing(loaded);
        flow_text.set_read_only(
            std::fs::metadata(&path)
                .map(|metadata| metadata.permissions().readonly())
                .unwrap_or(false),
        );
        doc.flow_text = Some(flow_text);
        doc.path = Some(path.clone());
        doc.file_modified_at = file_modified_at(&path);
        doc.title = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("Văn bản")
            .to_string();
        doc.mark_saved();
        self.jobs.load_activate_pending = false;
        self.activate_new_document(doc);
        self.shell.status_msg = format!("Đã mở văn bản: {}", file_name(&path));
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

    /// Install a crash-recovered single image the same way: pointed back at the
    /// file it was edited from (or its old tab title when it never had one),
    /// latched dirty, and still mirrored to the recovery file until saved.
    pub(crate) fn install_canvas_recovered(
        &mut self,
        autosave_path: PathBuf,
        canvas: Canvas,
        project_path: Option<PathBuf>,
        title: Option<String>,
    ) {
        self.jobs.load_activate_pending = true;
        let Some(id) = self.attach_loaded_doc(autosave_path.clone(), canvas, None, None) else {
            return;
        };
        if let Some(doc) = self.docs.documents.iter_mut().find(|d| d.id == id) {
            doc.path = project_path.clone();
            doc.file_modified_at = project_path
                .as_deref()
                .and_then(crate::core::document::file_modified_at);
            doc.title = title
                .filter(|t| !t.trim().is_empty())
                .or_else(|| {
                    project_path
                        .as_deref()
                        .and_then(|p| p.file_stem())
                        .map(|s| s.to_string_lossy().to_string())
                })
                .unwrap_or_else(|| "Untitled".to_string());
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
        self.shell.canvas_unit = self.shell.settings.default_unit;
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
        // Leave previews in their bounded channel (the extractor waits) while
        // the opening image's draft is due; see `raw_previews_on_hold`.
        if self.raw_previews_on_hold() {
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
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

    /// While a RAW that is being opened has no placeholder yet, its draft is
    /// expected shortly: hold every embedded preview until then (or until
    /// [`RAW_DRAFT_WAIT`]) — its own so the first paint is iAi's render rather
    /// than the camera JPEG, the others so they cannot claim the active image
    /// first.
    fn raw_previews_on_hold(&self) -> bool {
        self.jobs
            .raw_decode_jobs
            .values()
            .any(|job| job.doc.is_none() && job.started.elapsed() < RAW_DRAFT_WAIT)
    }

    /// Show a RAW's embedded-JPEG preview as a placeholder tab. Skipped if a
    /// draft or the full decode already produced a document for this path.
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
        let existing = self
            .jobs
            .raw_preview_docs
            .get(&key)
            .and_then(|id| self.docs.documents.iter().position(|d| d.id == *id))
            .or_else(|| self.find_open_document_by_path(&path));
        if let Some(idx) = existing {
            // Keep the better pixels already shown; only adopt the metadata.
            if self.docs.documents[idx].raw_exif.is_none() {
                self.docs.documents[idx].raw_exif = preview.exif;
            }
            if !self.jobs.raw_decode_jobs.contains_key(&key) {
                crate::formats::raw_preview::forget_cached_mean_luma(&path);
            }
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
        self.attach_raw_placeholder(event_loop, path, canvas, preview.exif, false);
    }

    /// Open a deferred RAW document showing `canvas` (the camera JPEG, or the
    /// colour-true draft when `draft`) and enter it into the Develop session.
    /// While its id remains in raw_preview_docs the panel presents a decoder
    /// state and does not expose RAW controls.
    fn attach_raw_placeholder(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        path: PathBuf,
        canvas: Canvas,
        exif: Option<String>,
        draft: bool,
    ) {
        let key = normalized_path_key(&path);
        let id = crate::core::document::DocumentId(self.docs.next_doc_id);
        self.docs.next_doc_id += 1;
        let mut doc = crate::core::document::Document::from_canvas(id, canvas, Some(path.clone()));
        doc.raw_exif = exif;
        doc.deferred_raw = true;
        doc.raw_draft = draft;
        let name = file_name(&path);
        if self.jobs.load_activate_pending {
            // The first finished item (preview, draft or full) claims the active tab.
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
        self.jobs.raw_preview_docs.insert(key.clone(), id);
        if let Some(job) = self.jobs.raw_decode_jobs.get_mut(&key) {
            job.doc = Some(id);
        }
        // Open the actual Develop workflow as soon as the first placeholder is
        // ready.
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
        // The first placeholder is the initial active filmstrip image. Materialize
        // only that one; background previews stay light until clicked.
        if new_idx == self.docs.active_doc_idx {
            self.ensure_raw_resident(new_idx);
        }
    }

    /// Attach finished colour-true drafts (see `RawDecodeControl::draft`).
    pub fn poll_raw_drafts(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if self.jobs.pending_raw_drafts.is_empty() {
            return;
        }
        let mut still_pending = Vec::new();
        for rx in std::mem::take(&mut self.jobs.pending_raw_drafts) {
            let mut alive = true;
            loop {
                match rx.try_recv() {
                    Ok((path, canvas)) => self.attach_raw_draft(event_loop, path, canvas),
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
        self.jobs.pending_raw_drafts = still_pending;
    }

    /// Show a RAW's colour-true draft in place of its placeholder (or as its
    /// first paint). Same crop and colour as the full decode, which later
    /// replaces it without a visible change at Fit.
    pub(in crate::app) fn attach_raw_draft(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        path: PathBuf,
        canvas: Canvas,
    ) {
        let key = normalized_path_key(&path);
        // A draft from an abandoned decode must not reopen or repaint anything.
        if self.jobs.cancelled_raw_loads.contains(&key)
            || !self.jobs.raw_decode_jobs.contains_key(&key)
            || self.dev.develop_bake_all.is_some()
        {
            return;
        }
        let existing = self
            .jobs
            .raw_preview_docs
            .get(&key)
            .and_then(|id| self.docs.documents.iter().position(|d| d.id == *id));
        match existing {
            Some(idx) => {
                if self.docs.documents[idx].deferred_raw {
                    self.replace_placeholder_canvas(idx, canvas);
                }
            }
            None => {
                if self.find_open_document_by_path(&path).is_some() {
                    return;
                }
                // The embedded preview, when it arrives, only adds its EXIF.
                self.attach_raw_placeholder(event_loop, path, canvas, None, true);
            }
        }
    }

    /// Swap a deferred document's placeholder pixels for a draft, restarting
    /// the (locked) Develop preview on it when it is the active image.
    fn replace_placeholder_canvas(&mut self, idx: usize, canvas: Canvas) {
        let id = self.docs.documents[idx].id;
        let active = idx == self.docs.active_doc_idx;
        let previewing = active
            && self
                .dev
                .develop_preview
                .as_ref()
                .is_some_and(|preview| preview.doc_id == id);
        if previewing {
            self.cancel_develop_preview();
        }
        self.docs.documents[idx].saved_zoom = 0.0;
        self.docs.documents[idx].canvas = canvas;
        self.docs.documents[idx].raw_draft = true;
        self.dev.develop_thumbs.remove(&id);
        if active {
            self.refresh_active_document();
            if previewing && self.dev.develop_bake_all.is_none() {
                let settings = self
                    .dev
                    .develop_session
                    .iter()
                    .find(|entry| entry.doc == id)
                    .map(|entry| entry.settings.clone())
                    .unwrap_or_default();
                self.begin_develop_preview(settings);
                self.dev.develop_view_fit = true;
                self.dev.develop_composited_view = None;
            }
        }
        if let Some(w) = &self.win.develop_window {
            w.request_redraw();
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
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
        self.docs.documents[idx].raw_draft = false;
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
                    file_io::FileDialogResult::InsertPdfPages {
                        document_id,
                        position,
                        paths,
                    } => {
                        self.start_pdf_page_insert(document_id, position, paths);
                    }
                }
                #[cfg(all(target_os = "windows", feature = "canvas-editor-webview"))]
                self.finish_document_webview_save_continuation();
                #[cfg(not(all(target_os = "windows", feature = "canvas-editor-webview")))]
                {
                    if self.shell.close_requested {
                        self.shell.close_requested = false;
                        if !self.docs.documents[self.docs.active_doc_idx].is_modified() {
                            self.execute_close();
                        } else {
                            self.shell.ui.show_close_dialog = true;
                        }
                    }
                    if self.shell.exit_save_pending {
                        self.shell.exit_save_pending = false;
                        if !self.docs.documents[self.docs.active_doc_idx].is_modified() {
                            self.docs.pending_exit_docs.pop_front();
                            self.present_next_exit_document();
                        } else {
                            self.shell.ui.show_exit_dialog = true;
                        }
                    }
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
                if self.shell.close_requested {
                    self.shell.close_requested = false;
                    self.shell.ui.show_close_dialog = true;
                }
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
        // A live preview, an in-flight decode or a pending filmstrip switch
        // means this image is really in play; leave it resident.
        if self
            .dev
            .develop_preview
            .as_ref()
            .is_some_and(|p| p.doc_id == id)
            || self
                .dev
                .develop_switch_pending
                .is_some_and(|(pending, _)| pending == id)
        {
            return;
        }
        let key = normalized_path_key(&path);
        if self.jobs.loading_keys.contains(&key) {
            return;
        }
        let thumb = self.docs.documents[idx].canvas.downscaled_thumbnail(2048);
        let full = std::mem::replace(&mut self.docs.documents[idx].canvas, thumb);
        self.docs.documents[idx].deferred_raw = true;
        self.docs.documents[idx].raw_draft = false;
        // Park the decode on disk (unless an earlier visit already did), so
        // coming back is a read instead of a new demosaic.
        let spilled = self.docs.documents[idx]
            .raw_spill
            .as_ref()
            .is_some_and(|spill| !spill.is_failed());
        if !spilled {
            let bytes = full.width as u64 * full.height as u64 * 12;
            if self.make_raw_spill_room(bytes, &[id]) {
                if let Some(spill) = RawSpill::spawn(full) {
                    self.docs.documents[idx].raw_spill = Some(spill);
                    self.touch_raw_spill(id);
                }
            }
        }
        // A later decode of this path swaps the full image back into THIS doc
        // (attach_loaded_doc routes through raw_preview_docs).
        self.jobs.raw_preview_docs.insert(key, id);
        // The cached develop thumbnail is rebuilt from the new state on demand.
        self.dev.develop_thumbs.remove(&id);
    }

    /// Memory Milestone M1: make an evicted RAW document full-resolution again,
    /// off-thread: read its disk spill back when it has one, otherwise decode it
    /// at full speed (sending an early draft). The result lands in `poll_loads`,
    /// where `raw_preview_docs` routes it back into this document via
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
        let key = normalized_path_key(&path);
        // Already restoring or decoding (e.g. a double activation).
        if self.jobs.loading_keys.contains(&key) {
            return;
        }
        if let Some(spill) = doc.raw_spill.clone().filter(|spill| !spill.is_failed()) {
            self.touch_raw_spill(id);
            self.jobs.raw_preview_docs.insert(key.clone(), id);
            self.jobs.cancelled_raw_loads.remove(&key);
            self.jobs.loading_keys.insert(key);
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let result = spill
                    .restore()
                    .map(|canvas| vec![canvas])
                    .map_err(|_| RAW_SPILL_LOST.to_string());
                let _ = tx.send((path, result, false));
            });
            self.jobs.pending_loads.push(rx);
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return;
        }
        // A full decode needs the CPU: stop the background prefetch, and when
        // this is the image the user wants, abandon decodes of images they
        // have already left.
        self.cancel_raw_prefetch();
        let wanted = idx == self.docs.active_doc_idx
            || self
                .dev
                .develop_switch_pending
                .is_some_and(|(pending, _)| pending == id);
        if wanted {
            for job in self.jobs.raw_decode_jobs.values() {
                if job.doc != Some(id) {
                    job.cancel.store(true, Ordering::Relaxed);
                }
            }
        }
        // One full decode at a time; the next poll_loads restarts this one when
        // the running (or cancelled) decode lands.
        if !self.jobs.raw_decode_jobs.is_empty() {
            self.shell.status_msg = format!("Đang xếp hàng RAW: {}", file_name(&path));
            return;
        }
        self.start_foreground_raw_decode(path.clone(), Some(id));
        self.shell.status_msg = format!("Đang giải mã RAW: {}", file_name(&path));
    }

    /// Decode `path` at full speed on a worker, sending its colour-true draft
    /// first. `doc` is the deferred document it fills (`None` for the first
    /// image of an open, whose placeholder does not exist yet).
    fn start_foreground_raw_decode(&mut self, path: PathBuf, doc: Option<DocumentId>) {
        let key = normalized_path_key(&path);
        if let Some(id) = doc {
            // Route the decode back into this exact document.
            self.jobs.raw_preview_docs.insert(key.clone(), id);
        }
        self.jobs.cancelled_raw_loads.remove(&key);
        self.jobs.loading_keys.insert(key.clone());
        let cancel = Arc::new(AtomicBool::new(false));
        self.jobs.raw_decode_jobs.insert(
            key,
            crate::app::background_jobs::RawDecodeJob {
                doc,
                cancel: Arc::clone(&cancel),
                started: Instant::now(),
            },
        );
        let (tx, rx) = std::sync::mpsc::channel();
        let (draft_tx, draft_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = decode_raw_job(&path, false, &cancel, Some(draft_tx));
            let _ = tx.send((path, result, false));
        });
        self.jobs.pending_loads.push(rx);
        self.jobs.pending_raw_drafts.push(draft_rx);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    fn raw_spill_bytes(&self) -> u64 {
        self.docs
            .documents
            .iter()
            .filter_map(|doc| doc.raw_spill.as_ref())
            .map(|spill| spill.bytes())
            .sum()
    }

    /// Mark `id`'s spill as the most recently used.
    fn touch_raw_spill(&mut self, id: DocumentId) {
        self.jobs.raw_spill_lru.retain(|other| *other != id);
        self.jobs.raw_spill_lru.push(id);
    }

    /// Make room on disk for a new spill of `bytes`, dropping the least recently
    /// used spills (never those of `keep`, the active image or the image being
    /// switched to). False when it cannot fit, including when the temp volume
    /// would drop below its free-space reserve.
    fn make_raw_spill_room(&mut self, bytes: u64, keep: &[DocumentId]) -> bool {
        let free = crate::core::hw::free_disk_bytes(&std::env::temp_dir()).unwrap_or(u64::MAX);
        if bytes.saturating_add(RAW_SPILL_FREE_RESERVE) > free || bytes > RAW_SPILL_BUDGET {
            return false;
        }
        let documents = &self.docs.documents;
        self.jobs.raw_spill_lru.retain(|id| {
            documents
                .iter()
                .any(|d| d.id == *id && d.raw_spill.is_some())
        });
        let active = self
            .docs
            .documents
            .get(self.docs.active_doc_idx)
            .map(|d| d.id);
        let pending = self.dev.develop_switch_pending.map(|(id, _)| id);
        while self.raw_spill_bytes() + bytes > RAW_SPILL_BUDGET {
            let Some(pos) =
                self.jobs.raw_spill_lru.iter().position(|id| {
                    !keep.contains(id) && Some(*id) != active && Some(*id) != pending
                })
            else {
                return false;
            };
            let victim = self.jobs.raw_spill_lru.remove(pos);
            if let Some(doc) = self.docs.documents.iter_mut().find(|d| d.id == victim) {
                doc.raw_spill = None;
            }
        }
        true
    }

    /// Stop the background prefetch (it aborts at its next checkpoint).
    pub(in crate::app) fn cancel_raw_prefetch(&mut self) {
        if let Some(job) = self.jobs.raw_prefetch.take() {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Land a finished prefetch and start the next one. Called every frame.
    pub fn poll_raw_prefetch(&mut self) {
        if let Some(job) = &self.jobs.raw_prefetch {
            let result = match job.rx.try_recv() {
                Ok(result) => Some(result),
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
            };
            let doc_id = job.doc;
            self.jobs.raw_prefetch = None;
            match result {
                Some(Ok(spill)) => {
                    let wanted = self
                        .docs
                        .documents
                        .iter()
                        .any(|d| d.id == doc_id && d.deferred_raw && d.raw_spill.is_none());
                    if wanted && self.make_raw_spill_room(spill.bytes(), &[doc_id]) {
                        if let Some(doc) = self.docs.documents.iter_mut().find(|d| d.id == doc_id) {
                            doc.raw_spill = Some(spill);
                        }
                        self.touch_raw_spill(doc_id);
                    }
                }
                Some(Err(e)) if e != RAW_DECODE_CANCELLED => {
                    self.jobs.raw_prefetch_failed.insert(doc_id);
                }
                _ => {}
            }
        }
        self.schedule_raw_prefetch();
    }

    /// While the user edits a resident image, decode its filmstrip neighbours
    /// in the background (one at a time, half the CPUs, below normal priority)
    /// straight into disk spills, so selecting them next is a quick read.
    fn schedule_raw_prefetch(&mut self) {
        if self.jobs.raw_prefetch.is_some()
            || !self.jobs.raw_decode_jobs.is_empty()
            || !self.jobs.loading_keys.is_empty()
            || self.win.develop_window.is_none()
            || self.win.retiring_develop_window.is_some()
            || self.dev.develop_bake_all.is_some()
            || self.dev.develop_switch_pending.is_some()
            || self.dev.develop_session.len() < 2
        {
            return;
        }
        let Some(active) = self.docs.documents.get(self.docs.active_doc_idx) else {
            return;
        };
        if active.deferred_raw {
            return;
        }
        let Some(pos) = self
            .dev
            .develop_session
            .iter()
            .position(|entry| entry.doc == active.id)
        else {
            return;
        };
        if self.raw_spill_bytes() >= RAW_SPILL_BUDGET {
            return;
        }
        for offset in RAW_PREFETCH_OFFSETS {
            let Some(entry) = pos
                .checked_add_signed(offset)
                .and_then(|i| self.dev.develop_session.get(i))
            else {
                continue;
            };
            let id = entry.doc;
            let Some(doc) = self.docs.documents.iter().find(|d| d.id == id) else {
                continue;
            };
            if !doc.deferred_raw
                || doc.raw_spill.is_some()
                || self.jobs.raw_prefetch_failed.contains(&id)
                || self.jobs.raw_preview_failures.contains_key(&id)
            {
                continue;
            }
            let Some(path) = doc.path.clone() else {
                continue;
            };
            if !crate::formats::raw::is_raw_path(&path)
                || self
                    .jobs
                    .cancelled_raw_loads
                    .contains(&normalized_path_key(&path))
            {
                continue;
            }
            let cancel = Arc::new(AtomicBool::new(false));
            let worker_cancel = Arc::clone(&cancel);
            let (tx, rx) = std::sync::mpsc::channel();
            // Wake the (possibly idle) UI so the next prefetch is scheduled.
            let window = self.win.window.clone();
            std::thread::spawn(move || {
                crate::core::hw::lower_current_thread_priority();
                let result =
                    decode_raw_job(&path, true, &worker_cancel, None).and_then(|mut canvases| {
                        let canvas = canvases.pop().ok_or("RAW decode produced no image")?;
                        RawSpill::write_now(&canvas)
                    });
                let _ = tx.send(result);
                if let Some(window) = window {
                    window.request_redraw();
                }
            });
            self.jobs.raw_prefetch = Some(crate::app::background_jobs::RawPrefetchJob {
                doc: id,
                cancel,
                rx,
            });
            return;
        }
    }
}

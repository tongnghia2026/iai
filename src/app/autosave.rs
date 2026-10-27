// Autosave + crash recovery.
//
// Dirty documents are mirrored to `%APPDATA%/IAI/autosave/` on a throttle so an
// unclean shutdown does not lose the edits. Every open tab is covered, the one
// in front first: single images, imported PDF projects and artboard documents
// are snapshotted (tiles are shared copy-on-write) and encoded on a worker
// thread so a large canvas never stalls the UI. Files are removed on a clean
// save/close and on a clean exit, so any file left behind by a process that is
// no longer running marks a crash and is offered back to the user (loaded and
// flagged unsaved).
//
// Several iAi windows can run at once, so each process holds an exclusive lock
// on `instance_{pid}.lock`. Recovery only adopts files whose owner's lock is
// free, and renames an adopted file to the adopting process.

use super::state::App;
use crate::core::canvas::Canvas;
use crate::core::document::{Document, DocumentId};
use crate::formats::Exporter;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant, SystemTime};

/// Largest single image mirrored in the background: the machine's ceiling (see
/// [`crate::core::hw::autosave_pixel_limit`]), halved for a 16-bit image because
/// its layers are materialized at 8 bytes a pixel — but never under the 100 MP
/// every machine has always been given.
fn autosave_pixel_limit(canvas: &Canvas) -> u64 {
    let limit = crate::core::hw::autosave_pixel_limit();
    if canvas.layer_stack.layers.iter().any(|l| l.tiles.has_hdr()) {
        (limit / 2).max(100_000_000)
    } else {
        limit
    }
}

/// Failed writes in a row before the user is told autosave is not working.
const FAILURES_BEFORE_WARNING: u32 = 3;

/// How autosave has fared this session.
#[derive(Default)]
pub(in crate::app) struct AutosaveHealth {
    /// Writes that failed in a row; any success clears it.
    failures: u32,
    /// The user has been told once; that is enough for a session.
    warned: bool,
    /// Documents whose write failed in the sweep under way. They wait for the
    /// next sweep, so a full disk is not hammered with one attempt after another.
    failed_this_sweep: HashSet<DocumentId>,
    /// Documents whose user has been told they are too large to mirror.
    too_large_told: HashSet<DocumentId>,
}

/// How long a clean exit waits for an in-flight background write to land so it
/// can be removed instead of being offered back as a crash.
const EXIT_WAIT_FOR_AUTOSAVE: Duration = Duration::from_secs(30);

pub(in crate::app) fn iai_data_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).or_else(|| {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
    })?;
    Some(base.join("IAI"))
}

pub(in crate::app) fn ensure_data_child_dir(name: &str) -> Option<PathBuf> {
    let dir = iai_data_dir()?.join(name);
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// The autosave directory, created on demand. `None` if no data dir is available.
fn autosave_dir() -> Option<PathBuf> {
    ensure_data_child_dir("autosave")
}

pub(crate) fn pdf_cache_dir() -> Option<PathBuf> {
    ensure_data_child_dir("pdf_cache")
}

/// Sidecar path storing the recovery target (the real project `.iai`, if any).
fn sidecar_path(autosave: &Path) -> PathBuf {
    autosave.with_extension("iai.meta")
}

fn write_sidecar(autosave: &Path, project_path: Option<&Path>, title: &str) {
    let value = serde_json::json!({
        "project_path": project_path.map(|p| p.to_string_lossy().to_string()),
        "title": title,
        "saved_at": SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    });
    let _ = std::fs::write(sidecar_path(autosave), value.to_string());
}

#[derive(Default)]
struct Sidecar {
    project_path: Option<PathBuf>,
    title: Option<String>,
}

fn read_sidecar(autosave: &Path) -> Sidecar {
    let Some(value) = std::fs::read_to_string(sidecar_path(autosave))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
    else {
        return Sidecar::default();
    };
    Sidecar {
        project_path: value["project_path"].as_str().map(PathBuf::from),
        title: value["title"].as_str().map(str::to_owned),
    }
}

fn remove_recovery_files(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(sidecar_path(path));
}

/// Owning process id encoded in a recovery file name (`recover_{pid}_…`).
fn recovery_owner_pid(file_name: &str) -> Option<u32> {
    file_name
        .strip_prefix("recover_")?
        .split('_')
        .next()?
        .parse()
        .ok()
}

fn instance_lock_name(pid: u32) -> String {
    format!("instance_{pid}.lock")
}

fn lock_owner_pid(file_name: &str) -> Option<u32> {
    file_name
        .strip_prefix("instance_")?
        .strip_suffix(".lock")?
        .parse()
        .ok()
}

/// Create and exclusively lock `instance_{pid}.lock`. The OS drops the lock when
/// the process dies, which is what tells a later instance the owner crashed.
fn lock_instance(dir: &Path, pid: u32) -> Option<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir.join(instance_lock_name(pid)))
        .ok()?;
    file.try_lock().ok()?;
    Some(file)
}

/// Whether the process that wrote files tagged `pid` is still running: its
/// instance lock is held. Unknown states count as alive so a running session's
/// work is never adopted by mistake.
fn instance_alive(dir: &Path, pid: u32) -> bool {
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join(instance_lock_name(pid)))
    {
        Ok(file) => file,
        Err(error) => return error.kind() != std::io::ErrorKind::NotFound,
    };
    !matches!(file.try_lock(), Ok(()))
}

/// Per-scan cache of which owning processes are still running.
struct OwnerLiveness<'a> {
    dir: &'a Path,
    known: std::collections::HashMap<u32, bool>,
}

impl<'a> OwnerLiveness<'a> {
    fn new(dir: &'a Path) -> Self {
        Self {
            dir,
            known: std::collections::HashMap::new(),
        }
    }

    fn alive(&mut self, pid: u32) -> bool {
        // The scan runs once at startup, before this process writes anything, so
        // files tagged with our own pid were left by an earlier process.
        if pid == std::process::id() {
            return false;
        }
        let dir = self.dir;
        *self
            .known
            .entry(pid)
            .or_insert_with(|| instance_alive(dir, pid))
    }
}

/// Move a dead session's recovery file (and sidecar) under this process's name.
/// The rename is the claim: a second instance starting at the same moment fails
/// it and leaves the file alone.
fn claim_recovery_file(dir: &Path, path: &Path) -> Option<PathBuf> {
    let pid = std::process::id();
    let target = (0..10_000u32)
        .map(|n| dir.join(format!("recover_{pid}_a{n}.iai")))
        .find(|candidate| !candidate.exists() && !sidecar_path(candidate).exists())?;
    std::fs::rename(path, &target).ok()?;
    let _ = std::fs::rename(sidecar_path(path), sidecar_path(&target));
    Some(target)
}

/// Cheap identity of a single image's saved content: its history position plus
/// the shared tile buffers of every layer, mask and alpha channel. Any edit
/// either moves the history or swaps tiles, and a canvas replaced wholesale
/// (reload, revert) carries new tiles even when its history restarts at the
/// same revision.
pub(in crate::app) fn content_fingerprint(canvas: &Canvas) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    fn tiles_sum(map: &crate::core::tile::TileMap) -> u64 {
        map.tiles.iter().fold(0u64, |sum, (pos, tile)| {
            let mut h = DefaultHasher::new();
            pos.hash(&mut h);
            (std::sync::Arc::as_ptr(tile) as usize).hash(&mut h);
            tile.revision.hash(&mut h);
            sum.wrapping_add(h.finish())
        })
    }

    let mut h = DefaultHasher::new();
    canvas.history_revision().hash(&mut h);
    (canvas.width, canvas.height).hash(&mut h);
    for layer in canvas.layer_stack.layers.iter() {
        layer.id.hash(&mut h);
        tiles_sum(&layer.tiles).hash(&mut h);
        if let Some(mask) = &layer.mask {
            tiles_sum(&mask.tiles).hash(&mut h);
        }
    }
    for channel in &canvas.channels.alpha {
        (channel.id, channel.revision).hash(&mut h);
    }
    h.finish()
}

/// [`content_fingerprint`] for a multi-page document: every page and the
/// master, plus what a PDF session keeps outside its canvases (page order and
/// names, the edits that span all pages).
fn project_fingerprint(doc: &Document) -> u64 {
    use crate::core::document::PdfGlobalEdit;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut h = DefaultHasher::new();
    content_fingerprint(&doc.canvas).hash(&mut h);
    (doc.active_artboard, doc.editing_master).hash(&mut h);
    for page in &doc.pages {
        page.as_ref().map(content_fingerprint).hash(&mut h);
    }
    doc.master.as_deref().map(content_fingerprint).hash(&mut h);
    if let Some(pdf) = &doc.pdf_document {
        (pdf.active_page, pdf.active_page_modified).hash(&mut h);
        pdf.selected_pages.hash(&mut h);
        pdf.page_names.hash(&mut h);
        let mut edited: Vec<(usize, u64)> = pdf
            .edited_pages
            .iter()
            .map(|(index, page)| (*index, content_fingerprint(&page.canvas)))
            .collect();
        edited.sort_unstable();
        edited.hash(&mut h);
        for edit in &pdf.global_edits {
            match edit {
                PdfGlobalEdit::Clear(clear) => {
                    0u8.hash(&mut h);
                    [clear.x0, clear.y0, clear.x1, clear.y1]
                        .map(f32::to_bits)
                        .hash(&mut h);
                    clear.color.hash(&mut h);
                    clear.target_pages.hash(&mut h);
                }
                PdfGlobalEdit::Stamp(stamp) => {
                    1u8.hash(&mut h);
                    [stamp.x0, stamp.y0, stamp.x1, stamp.y1]
                        .map(f32::to_bits)
                        .hash(&mut h);
                    (stamp.width, stamp.height, &stamp.rgba, &stamp.name).hash(&mut h);
                    (stamp.source_page, stamp.source_layer_id).hash(&mut h);
                    stamp.target_pages.hash(&mut h);
                }
            }
        }
    }
    h.finish()
}

/// `canvas` as the `.iai` writers need it, for a worker to encode. Layer tiles
/// are shared copy-on-write, so this costs a handful of reference counts; edits
/// made meanwhile copy only the tiles they touch.
pub(in crate::app) fn detached_canvas(canvas: &Canvas) -> Canvas {
    let mut snapshot = canvas.export_snapshot();
    snapshot.channels.alpha = canvas.channels.alpha.clone();
    snapshot
}

/// What one recovery write holds, detached from the live document.
enum RecoveryContent {
    Image(Canvas),
    Artboards {
        pages: Vec<Canvas>,
        active: usize,
        master: Option<Canvas>,
    },
    PdfProject(super::file_ops::PdfProjectSnapshot),
}

/// Worker body: write the sidecar first so the recovery file is never without
/// its target, then the archive (atomically replaced in place).
fn write_recovery(
    path: &Path,
    content: &RecoveryContent,
    project_path: Option<&Path>,
    title: &str,
) -> Result<(), String> {
    write_sidecar(path, project_path, title);
    match content {
        RecoveryContent::Image(canvas) => crate::formats::iai::IaiExporter.export(
            canvas,
            path,
            &crate::formats::ExportOptions::default(),
        ),
        RecoveryContent::Artboards {
            pages,
            active,
            master,
        } => {
            let pages: Vec<&Canvas> = pages.iter().collect();
            crate::formats::iai::write_artboard_doc(path, &pages, *active, master.as_ref())
        }
        RecoveryContent::PdfProject(project) => project.write(path).map(|_| ()),
    }
}

/// A recovery write running on a worker thread.
pub(in crate::app) struct AutosaveJob {
    doc_id: DocumentId,
    path: PathBuf,
    fingerprint: u64,
    /// Set when the document was saved or closed while the write was in flight:
    /// the file it lands must be removed, not adopted.
    discard: bool,
    done: Receiver<Result<(), String>>,
}

impl App {
    /// Periodically capture dirty documents. Images, PDF projects and artboard
    /// documents are encoded one at a time on a worker, the active tab first;
    /// Canvas Editor FlowText performs its asynchronous snapshot handshake and
    /// will gain disk recovery with `.iai` v12. Throttled to the Preferences
    /// period.
    pub fn maybe_autosave(&mut self) {
        self.poll_autosave_job();
        // Preferences ▸ Files & Autosave can turn recovery off, or change the
        // period. When off, do nothing (and leave the timer alone so re-enabling
        // resumes on the next tick).
        if !self.shell.settings.autosave_enabled {
            return;
        }
        let interval = Duration::from_secs(self.shell.settings.autosave_interval_secs as u64);
        if self.docs.last_autosave.elapsed() < interval {
            return;
        }
        // A live preview or an unfinished operation may hold pixels the user has
        // not committed; wait for it rather than mirroring a transient state.
        if self.autosave_must_wait() {
            return;
        }
        // The sweep stays open (timer not reset) until every changed image has
        // been written, one worker at a time.
        if self.docs.autosave_job.is_some() {
            return;
        }
        if let Some(dir) = autosave_dir()
            && self.start_next_recovery_write(&dir)
        {
            return;
        }
        self.autosave_active_flow_text();
    }

    /// Every document has had its turn: start the next period.
    fn finish_autosave_sweep(&mut self) {
        self.docs.last_autosave = Instant::now();
        self.docs.autosave_health.failed_this_sweep.clear();
    }

    fn autosave_must_wait(&self) -> bool {
        self.edit.input.painting
            || !self.edit.pending_stroke_inputs.is_empty()
            || self.edit.transform_state.is_some()
            || self.edit.pending_transform_commit.is_some()
            || self.edit.warp_state.is_some()
            || self.edit.text_edit.is_some()
            || self.edit.text_font_preview.is_some()
            || self.edit.show_refine_panel
            || self.shell.ui.show_refine_color_dialog
            || self.shell.adjustment_preview.is_some()
            || self.shell.filter_preview.is_some()
            || self.shell.scan_preview.is_some()
            || self.portrait_in_session()
            || self.dev.develop_preview.is_some()
            || self.win.develop_window.is_some()
            || self.is_preview_dialog_open()
    }

    /// The active tab's FlowText snapshot, which the Canvas Editor hands over by
    /// itself; every other kind of document went through the background sweep.
    fn autosave_active_flow_text(&mut self) {
        let is_flow_text = self
            .docs
            .documents
            .get(self.docs.active_doc_idx)
            .is_some_and(|doc| doc.is_flow_text());
        if is_flow_text {
            #[cfg(all(target_os = "windows", feature = "canvas-editor-webview"))]
            if self.document_webview_active_is_dirty()
                && !self.request_document_webview_autosave_snapshot()
            {
                // The editor could not take the request yet; ask again shortly.
                return;
            }
            // The legacy FlowText editor remains outside the recovery scope.
        }
        self.finish_autosave_sweep();
    }

    /// Start mirroring the next open document whose content changed since it was
    /// last written, active tab first. Returns whether a write started.
    fn start_next_recovery_write(&mut self, dir: &Path) -> bool {
        let active = self.docs.active_doc_idx;
        let order: Vec<usize> = std::iter::once(active)
            .chain((0..self.docs.documents.len()).filter(|&i| i != active))
            .collect();
        for idx in order {
            let Some(doc) = self.docs.documents.get(idx) else {
                continue;
            };
            if doc.is_flow_text() {
                continue;
            }
            let doc_id = doc.id;
            if self
                .docs
                .autosave_health
                .failed_this_sweep
                .contains(&doc_id)
            {
                continue;
            }
            if !doc.is_modified() {
                // Back at the saved state (e.g. undone): nothing left to recover.
                if self.docs.autosave_files.contains_key(&doc_id) {
                    self.clear_autosave_for(doc_id);
                }
                continue;
            }
            let multi_page = doc.pdf_document.is_some() || !doc.pages.is_empty();
            let pixels = u64::from(doc.canvas.width) * u64::from(doc.canvas.height);
            if !multi_page && pixels > autosave_pixel_limit(&doc.canvas) {
                self.tell_too_large_once(idx);
                continue;
            }
            let fingerprint = if multi_page {
                project_fingerprint(doc)
            } else {
                content_fingerprint(&doc.canvas)
            };
            if self.docs.autosave_files.contains_key(&doc_id)
                && self.docs.autosave_fingerprints.get(&doc_id) == Some(&fingerprint)
            {
                continue;
            }
            if self.spawn_recovery_write(idx, dir, fingerprint) {
                return true;
            }
        }
        false
    }

    /// An image above the machine's ceiling is not mirrored. Say so once per
    /// document, so that nobody counts on a recovery that will not be there.
    fn tell_too_large_once(&mut self, idx: usize) {
        let doc = &self.docs.documents[idx];
        if !self.docs.autosave_health.too_large_told.insert(doc.id) {
            return;
        }
        let megapixels = u64::from(doc.canvas.width) * u64::from(doc.canvas.height) / 1_000_000;
        crate::diag::note(
            "autosave",
            &format!(
                "skipped \"{}\": {megapixels} MP is above this machine's ceiling",
                doc.title
            ),
        );
        self.shell.status_msg = format!(
            "\"{}\" quá lớn so với bộ nhớ máy nên không được tự lưu — nhớ Ctrl+S",
            doc.tab_title()
        );
    }

    /// What the document at `idx` needs written, detached from the live document.
    fn recovery_content(&self, idx: usize) -> Result<RecoveryContent, String> {
        let doc = self.docs.documents.get(idx).ok_or("no document")?;
        if doc.pdf_document.is_some() {
            return self
                .pdf_project_snapshot(idx)
                .map(RecoveryContent::PdfProject);
        }
        if doc.pages.is_empty() {
            return Ok(RecoveryContent::Image(detached_canvas(&doc.canvas)));
        }
        let pages = doc.all_page_canvases();
        Ok(RecoveryContent::Artboards {
            active: doc.active_artboard.min(pages.len().saturating_sub(1)),
            pages: pages.into_iter().map(detached_canvas).collect(),
            master: doc.master_canvas().map(detached_canvas),
        })
    }

    fn spawn_recovery_write(&mut self, idx: usize, dir: &Path, fingerprint: u64) -> bool {
        self.sync_brush_gpu_to_cpu();
        let doc_id = self.docs.documents[idx].id;
        let content = match self.recovery_content(idx) {
            Ok(content) => content,
            Err(error) => {
                self.note_autosave_failure(doc_id, &error);
                return false;
            }
        };
        let doc = &self.docs.documents[idx];
        let path = self
            .docs
            .autosave_files
            .get(&doc_id)
            .cloned()
            .unwrap_or_else(|| {
                dir.join(format!("recover_{}_{}.iai", std::process::id(), doc_id.0))
            });
        let project_path = doc.path.clone().filter(|p| p != &path);
        let title = doc.title.clone();
        let worker_path = path.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("iai-autosave".to_string())
            .spawn(move || {
                let result =
                    write_recovery(&worker_path, &content, project_path.as_deref(), &title);
                let _ = tx.send(result);
            });
        if let Err(error) = spawned {
            self.note_autosave_failure(doc_id, &error.to_string());
            return false;
        }
        self.docs.autosave_job = Some(AutosaveJob {
            doc_id,
            path,
            fingerprint,
            discard: false,
            done: rx,
        });
        true
    }

    /// A recovery write did not happen. It goes in the flight recorder every
    /// time; the user hears of it once, when it keeps failing.
    fn note_autosave_failure(&mut self, doc_id: DocumentId, error: &str) {
        crate::diag::note("autosave", &format!("write failed: {error}"));
        let health = &mut self.docs.autosave_health;
        health.failed_this_sweep.insert(doc_id);
        health.failures += 1;
        if health.failures >= FAILURES_BEFORE_WARNING && !health.warned {
            health.warned = true;
            self.shell.status_msg = format!("Tự lưu đang lỗi: {error}");
        }
    }

    /// Adopt a finished background write, or remove it if its document was saved
    /// or closed meanwhile.
    fn poll_autosave_job(&mut self) {
        let result = match self
            .docs
            .autosave_job
            .as_ref()
            .map(|job| job.done.try_recv())
        {
            None | Some(Err(TryRecvError::Empty)) => return,
            Some(Ok(result)) => result,
            Some(Err(TryRecvError::Disconnected)) => Err("autosave worker stopped".to_string()),
        };
        let Some(job) = self.docs.autosave_job.take() else {
            return;
        };
        let still_open = self.docs.by_id(job.doc_id).is_some();
        if job.discard || !still_open {
            remove_recovery_files(&job.path);
            return;
        }
        match result {
            Ok(()) => {
                self.docs
                    .autosave_fingerprints
                    .insert(job.doc_id, job.fingerprint);
                self.docs.autosave_files.insert(job.doc_id, job.path);
                self.docs.autosave_health.failures = 0;
            }
            // Best effort — a failed autosave must never disrupt editing. Keep an
            // earlier good file; drop the sidecar of a first write that failed.
            Err(error) => {
                if !self.docs.autosave_files.contains_key(&job.doc_id) {
                    remove_recovery_files(&job.path);
                }
                self.note_autosave_failure(job.doc_id, &error);
            }
        }
    }

    /// Remove the autosave file (if any) for the document at `idx`. Called after a
    /// successful save and when a document is closed.
    pub fn clear_autosave(&mut self, idx: usize) {
        let Some(doc) = self.docs.documents.get(idx) else {
            return;
        };
        self.clear_autosave_for(doc.id);
    }

    pub(crate) fn clear_autosave_for(&mut self, id: DocumentId) {
        if let Some(job) = self
            .docs
            .autosave_job
            .as_mut()
            .filter(|job| job.doc_id == id)
        {
            job.discard = true;
        }
        self.docs.autosave_fingerprints.remove(&id);
        if let Some(path) = self.docs.autosave_files.remove(&id) {
            remove_recovery_files(&path);
        }
    }

    pub(crate) fn clear_embedded_pdf_for(&mut self, id: DocumentId) {
        if let Some(path) = self.docs.embedded_pdf_files.remove(&id) {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Drop every tracked autosave file and release the instance lock. Called at
    /// a clean app exit so a normal shutdown leaves nothing to recover.
    pub fn clear_all_autosave(&mut self) {
        // No exit cuts off a file the user asked to have written.
        self.finish_save_jobs();
        if let Some(job) = self.docs.autosave_job.take() {
            let _ = job.done.recv_timeout(EXIT_WAIT_FOR_AUTOSAVE);
            remove_recovery_files(&job.path);
        }
        for path in self.docs.autosave_files.values() {
            remove_recovery_files(path);
        }
        self.docs.autosave_files.clear();
        self.docs.autosave_fingerprints.clear();
        self.clear_all_embedded_pdf();
        if let Some(lock) = self.docs.instance_lock.take() {
            drop(lock);
            if let Some(dir) = autosave_dir() {
                let _ = std::fs::remove_file(dir.join(instance_lock_name(std::process::id())));
            }
        }
    }

    fn clear_all_embedded_pdf(&mut self) {
        for path in self.docs.embedded_pdf_files.values() {
            let _ = std::fs::remove_file(path);
        }
        self.docs.embedded_pdf_files.clear();
    }

    fn clear_orphaned_embedded_pdf_files(&self) {
        let Some(dir) = pdf_cache_dir() else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_pdf = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("pdf"));
            if !is_pdf {
                continue;
            }
            if self
                .docs
                .embedded_pdf_files
                .values()
                .any(|owned| owned == &path)
            {
                continue;
            }
            let _ = std::fs::remove_file(path);
        }
    }

    /// On startup, load any autosave files left by a previous unclean shutdown.
    /// Each recovered document is opened, pointed back at its original file when
    /// known, and flagged unsaved so the user is prompted to save.
    pub fn check_crash_recovery(&mut self) {
        let recovered = autosave_dir().map_or(0, |dir| self.recover_from_dir(&dir));
        self.clear_orphaned_embedded_pdf_files();
        if recovered > 0 {
            self.shell.status_msg = format!(
                "Recovered {recovered} unsaved document(s) from a previous session — \
                 save to keep them"
            );
            if let Some(window) = &self.win.window {
                window.request_redraw();
            }
        }
    }

    fn recover_from_dir(&mut self, dir: &Path) -> usize {
        if self.docs.instance_lock.is_none() {
            self.docs.instance_lock = lock_instance(dir, std::process::id());
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return 0;
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        paths.sort();
        let mut liveness = OwnerLiveness::new(dir);
        let mut recovered = 0usize;
        for path in &paths {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            // Another running iAi window owns these; leave them alone.
            if recovery_owner_pid(name).is_some_and(|pid| liveness.alive(pid)) {
                continue;
            }
            let is_iai = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("iai"));
            if !is_iai {
                // A dead session's half-written archive.
                if name.ends_with(".iai.tmp") {
                    let _ = std::fs::remove_file(path);
                }
                continue;
            }
            // Never re-adopt a file this session already owns.
            if self.docs.autosave_files.values().any(|owned| owned == path) {
                continue;
            }
            let Some(claimed) = claim_recovery_file(dir, path) else {
                continue;
            };
            let sidecar = read_sidecar(&claimed);
            match crate::formats::iai::load(&claimed) {
                Ok(crate::formats::iai::IaiLoad::PdfProject(project)) => {
                    self.install_pdf_project_recovered(claimed, project, sidecar.project_path);
                    recovered += 1;
                }
                Ok(crate::formats::iai::IaiLoad::ArtboardDoc(doc)) => {
                    self.install_artboard_doc_recovered(claimed, doc, sidecar.project_path);
                    recovered += 1;
                }
                Ok(crate::formats::iai::IaiLoad::Canvas(canvas)) => {
                    self.install_canvas_recovered(
                        claimed,
                        canvas,
                        sidecar.project_path,
                        sidecar.title,
                    );
                    recovered += 1;
                }
                _ => continue,
            }
        }
        // Leftovers of dead sessions: sidecars whose archive is gone, and locks.
        for path in &paths {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if let Some(pid) = lock_owner_pid(name) {
                if pid != std::process::id() && !liveness.alive(pid) {
                    let _ = std::fs::remove_file(path);
                }
            } else if let Some(archive) = name.strip_suffix(".meta") {
                let dead = recovery_owner_pid(name).is_none_or(|pid| !liveness.alive(pid));
                if dead && path.exists() && !dir.join(archive).exists() {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        recovered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "iai-autosave-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn wait_for_job(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while app.docs.autosave_job.is_some() {
            assert!(Instant::now() < deadline, "autosave worker did not finish");
            std::thread::sleep(Duration::from_millis(10));
            app.poll_autosave_job();
        }
    }

    fn paint_active(app: &mut App, rgba: [u8; 4]) {
        let doc = app.docs.active_mut();
        let (w, h) = (doc.canvas.width, doc.canvas.height);
        let pixels: Vec<u8> = rgba
            .iter()
            .copied()
            .cycle()
            .take(w as usize * h as usize * 4)
            .collect();
        let layer = doc.canvas.layer_stack.active_layer_mut();
        layer.tiles = crate::core::tile::TileMap::from_rgba(&pixels, w, h);
        doc.canvas.mark_dirty_unconditionally();
    }

    #[test]
    fn file_names_carry_their_owner() {
        assert_eq!(recovery_owner_pid("recover_1234_5.iai"), Some(1234));
        assert_eq!(recovery_owner_pid("recover_1234_a0.iai.meta"), Some(1234));
        assert_eq!(recovery_owner_pid("recover_x_5.iai"), None);
        assert_eq!(recovery_owner_pid("instance_1234.lock"), None);
        assert_eq!(lock_owner_pid("instance_1234.lock"), Some(1234));
        assert_eq!(lock_owner_pid("recover_1234_5.iai"), None);
    }

    #[test]
    fn a_held_instance_lock_reads_as_alive() {
        let dir = tmp_dir("lock");
        let pid = 4_000_000_001;
        assert!(!instance_alive(&dir, pid), "no lock file = owner gone");
        let lock = lock_instance(&dir, pid).expect("lock");
        assert!(instance_alive(&dir, pid));
        drop(lock);
        assert!(!instance_alive(&dir, pid), "released lock = owner gone");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn fingerprint_tracks_edits_and_replaced_canvases() {
        let mut canvas = Canvas::new(64, 64);
        let first = content_fingerprint(&canvas);
        assert_eq!(first, content_fingerprint(&canvas));
        canvas.layer_stack.active_layer_mut().tiles =
            crate::core::tile::TileMap::from_rgba(&vec![200u8; 64 * 64 * 4], 64, 64);
        assert_ne!(first, content_fingerprint(&canvas), "new tiles must count");
        let replaced = Canvas::new(64, 64);
        assert_ne!(
            content_fingerprint(&canvas),
            content_fingerprint(&replaced),
            "a fresh canvas at the same history revision still differs"
        );
    }

    #[test]
    fn a_single_image_is_mirrored_in_the_background_and_recovered() {
        let dir = tmp_dir("image");
        let mut app = App::new();
        let source = dir.join("photo.png");
        app.docs.documents[0].path = Some(source.clone());
        app.docs.documents[0].title = "photo".to_string();
        paint_active(&mut app, [10, 200, 30, 255]);
        let doc_id = app.docs.documents[0].id;

        assert!(app.start_next_recovery_write(&dir));
        wait_for_job(&mut app);
        let written = app
            .docs
            .autosave_files
            .get(&doc_id)
            .cloned()
            .expect("adopted");
        assert!(written.exists());
        assert_eq!(read_sidecar(&written).project_path, Some(source.clone()));
        assert!(
            !app.start_next_recovery_write(&dir),
            "unchanged content is not rewritten"
        );

        paint_active(&mut app, [90, 90, 90, 255]);
        assert!(app.start_next_recovery_write(&dir), "an edit is rewritten");
        wait_for_job(&mut app);

        // Simulate the crash: the session vanishes without clearing its files.
        app.docs.autosave_files.clear();
        let mut next = App::new();
        assert_eq!(next.recover_from_dir(&dir), 1);
        let doc = next.docs.active();
        assert_eq!(doc.path.as_deref(), Some(source.as_path()));
        assert!(doc.is_modified(), "recovered work must prompt to save");
        let flat = doc
            .canvas
            .layer_stack
            .flatten(doc.canvas.width, doc.canvas.height);
        assert_eq!(&flat[..4], &[90, 90, 90, 255]);
        let adopted = next
            .docs
            .autosave_files
            .get(&doc.id)
            .cloned()
            .expect("owned");
        assert!(
            adopted.exists() && !written.exists(),
            "claimed under the new session"
        );

        next.clear_autosave_for(next.docs.active().id);
        assert!(!adopted.exists() && !sidecar_path(&adopted).exists());
        drop((app, next));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_running_sessions_files_are_not_adopted() {
        let dir = tmp_dir("alive");
        let pid = 4_000_000_002;
        let lock = lock_instance(&dir, pid).expect("lock");
        let canvas = Canvas::new(8, 8);
        let file = dir.join(format!("recover_{pid}_1.iai"));
        let content = RecoveryContent::Image(detached_canvas(&canvas));
        write_recovery(&file, &content, None, "scan.pdf - Page 3").unwrap();

        let mut app = App::new();
        assert_eq!(app.recover_from_dir(&dir), 0);
        assert!(file.exists());

        drop(lock);
        let mut later = App::new();
        assert_eq!(later.recover_from_dir(&dir), 1, "the owner has exited");
        assert!(
            !dir.join(instance_lock_name(pid)).exists(),
            "stale lock swept"
        );
        let doc = later.docs.active();
        assert_eq!(doc.path, None);
        assert_eq!(
            doc.tab_title(),
            "scan.pdf - Page 3",
            "old tab name restored"
        );
        drop((app, later));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn saving_while_a_write_is_in_flight_discards_it() {
        let dir = tmp_dir("discard");
        let mut app = App::new();
        paint_active(&mut app, [1, 2, 3, 255]);
        let doc_id = app.docs.documents[0].id;
        assert!(app.start_next_recovery_write(&dir));
        let path = app.docs.autosave_job.as_ref().unwrap().path.clone();
        app.clear_autosave_for(doc_id);
        wait_for_job(&mut app);
        assert!(!path.exists() && !sidecar_path(&path).exists());
        assert!(!app.docs.autosave_files.contains_key(&doc_id));
        drop(app);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failing_write_keeps_the_earlier_file_and_is_told_once_it_persists() {
        let dir = tmp_dir("failing");
        let mut app = App::new();
        paint_active(&mut app, [10, 20, 30, 255]);
        let doc_id = app.docs.documents[0].id;
        assert!(app.start_next_recovery_write(&dir));
        wait_for_job(&mut app);
        let written = app.docs.autosave_files[&doc_id].clone();
        let good = std::fs::read(&written).unwrap();

        // A folder squatting on the archive's temporary name fails every write.
        std::fs::create_dir(written.with_extension("iai.tmp")).unwrap();
        for attempt in 1..=FAILURES_BEFORE_WARNING {
            paint_active(&mut app, [attempt as u8, 0, 0, 255]);
            assert!(app.start_next_recovery_write(&dir));
            wait_for_job(&mut app);
            assert_eq!(std::fs::read(&written).unwrap(), good, "earlier file kept");
            assert_eq!(app.docs.autosave_files.get(&doc_id), Some(&written));
            assert!(
                !app.start_next_recovery_write(&dir),
                "a failed document waits for the next sweep"
            );
            assert_eq!(
                app.shell.status_msg.starts_with("Tự lưu đang lỗi"),
                attempt == FAILURES_BEFORE_WARNING,
                "told only once it has failed {FAILURES_BEFORE_WARNING} times in a row"
            );
            app.finish_autosave_sweep();
        }

        app.shell.status_msg.clear();
        paint_active(&mut app, [200, 0, 0, 255]);
        assert!(app.start_next_recovery_write(&dir));
        wait_for_job(&mut app);
        assert!(app.shell.status_msg.is_empty(), "once a session is enough");

        std::fs::remove_dir(written.with_extension("iai.tmp")).unwrap();
        app.finish_autosave_sweep();
        assert!(app.start_next_recovery_write(&dir));
        wait_for_job(&mut app);
        assert_ne!(std::fs::read(&written).unwrap(), good, "writes again");
        assert_eq!(app.docs.autosave_health.failures, 0);
        drop(app);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_first_write_that_fails_leaves_nothing_behind() {
        let dir = tmp_dir("first-fails");
        let missing = dir.join("no-such-folder");
        let mut app = App::new();
        paint_active(&mut app, [1, 2, 3, 255]);
        let doc_id = app.docs.documents[0].id;
        assert!(app.start_next_recovery_write(&missing));
        wait_for_job(&mut app);
        assert!(!app.docs.autosave_files.contains_key(&doc_id));
        assert_eq!(app.docs.autosave_health.failures, 1);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        drop(app);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_many_page_document_in_a_background_tab_is_mirrored() {
        let dir = tmp_dir("pages");
        let mut app = App::new();
        let id = DocumentId(app.docs.next_doc_id);
        app.docs.next_doc_id += 1;
        let mut doc = Document::new(id, 16, 16);
        doc.title = "to roi".to_string();
        doc.pages = vec![None, Some(Canvas::new(16, 16))];
        doc.canvas.mark_dirty_unconditionally();
        app.docs.documents.push(doc);
        assert_eq!(app.docs.active_doc_idx, 0, "the new tab is behind");

        assert!(app.start_next_recovery_write(&dir));
        wait_for_job(&mut app);
        let written = app.docs.autosave_files[&id].clone();
        match crate::formats::iai::load(&written) {
            Ok(crate::formats::iai::IaiLoad::ArtboardDoc(loaded)) => {
                assert_eq!(loaded.pages.len(), 2)
            }
            _ => panic!("the recovery file must hold every page"),
        }
        assert!(
            !app.start_next_recovery_write(&dir),
            "an unchanged document is not rewritten"
        );

        // An edit on the page that is not in front still counts.
        let parked = app.docs.documents[1].pages[1].as_mut().unwrap();
        parked.layer_stack.active_layer_mut().tiles =
            crate::core::tile::TileMap::from_rgba(&vec![7u8; 16 * 16 * 4], 16, 16);
        assert!(app.start_next_recovery_write(&dir));
        wait_for_job(&mut app);
        drop(app);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_pdf_project_in_a_background_tab_is_mirrored_and_its_page_names_count() {
        use crate::core::document::{PdfDocumentState, PdfPageRef};

        let dir = tmp_dir("pdf");
        let source = dir.join("document.pdf");
        std::fs::write(
            &source,
            b"%PDF-1.4\n1 0 obj\n<<>>\nendobj\ntrailer\n<<>>\n%%EOF\n",
        )
        .unwrap();
        let recovery = dir.join("recovery");
        std::fs::create_dir(&recovery).unwrap();

        let mut app = App::new();
        let id = DocumentId(app.docs.next_doc_id);
        app.docs.next_doc_id += 1;
        let mut doc = Document::new(id, 4, 4);
        let mut reference = PdfPageRef {
            group_id: 1,
            source: source.clone(),
            index: 0,
            count: 2,
            requested_dpi: 300.0,
            loaded: true,
            original_width: 0,
            original_height: 0,
            original_dpi: 300.0,
            original_layer_id: 0,
            original_tiles_fingerprint: 0,
        };
        reference.record_canvas_baseline(&doc.canvas);
        doc.pdf_page = Some(reference);
        doc.pdf_document = Some(PdfDocumentState {
            source: source.clone(),
            embedded_source: None,
            page_count: 2,
            selected_pages: vec![0, 1],
            selected_pages_saved: vec![0, 1],
            page_names: Default::default(),
            page_names_saved: Default::default(),
            requested_dpi: 300.0,
            active_page: 0,
            active_page_modified: true,
            edited_pages: Default::default(),
            global_edits: Vec::new(),
            global_edits_saved: Vec::new(),
            global_edits_redo: Vec::new(),
            global_overlay_cache: None,
        });
        doc.canvas.mark_dirty_unconditionally();
        app.docs.documents.push(doc);

        assert!(app.start_next_recovery_write(&recovery));
        wait_for_job(&mut app);
        let written = app.docs.autosave_files[&id].clone();
        match crate::formats::iai::load(&written) {
            Ok(crate::formats::iai::IaiLoad::PdfProject(project)) => {
                assert_eq!(project.source, source);
                assert_eq!(project.pages.len(), 1, "the edited page");
            }
            _ => panic!("the recovery file must be the PDF project"),
        }
        assert!(!app.start_next_recovery_write(&recovery));

        // Kept outside any canvas, so no page's history moves.
        app.docs.documents[1]
            .pdf_document
            .as_mut()
            .unwrap()
            .set_page_name(0, Some("Bìa".to_string()));
        assert!(app.start_next_recovery_write(&recovery));
        wait_for_job(&mut app);
        drop(app);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_image_too_large_to_mirror_is_said_once() {
        let mut app = App::new();
        app.docs.documents[0].title = "khổ lớn".to_string();
        app.tell_too_large_once(0);
        assert!(app.shell.status_msg.contains("không được tự lưu"));
        app.shell.status_msg.clear();
        app.tell_too_large_once(0);
        assert!(app.shell.status_msg.is_empty());
    }

    #[test]
    fn a_sixteen_bit_image_gets_half_the_ceiling_but_never_under_the_old_one() {
        let machine = crate::core::hw::autosave_pixel_limit();
        assert_eq!(autosave_pixel_limit(&Canvas::new(8, 8)), machine);
        let deep = Canvas::from_rgba16(vec![0xFFFF; 8 * 8 * 4], 8, 8);
        assert_eq!(autosave_pixel_limit(&deep), (machine / 2).max(100_000_000));
    }
}

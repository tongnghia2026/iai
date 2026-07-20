// Autosave + crash recovery for multi-page PDF projects.
//
// A dirty PDF session is mirrored to `%APPDATA%/IAI/autosave/` on a throttle so
// an unclean shutdown does not lose the edited pages. Files are removed on a
// clean save/close and on a clean exit, so any file left behind at startup marks
// a crash and is offered back to the user (loaded and flagged unsaved).

use super::state::App;
use crate::core::document::DocumentId;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// How often the active dirty project is mirrored to disk.
const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(90);

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

fn write_sidecar(autosave: &Path, project_path: Option<&Path>) {
    let value = serde_json::json!({
        "project_path": project_path.map(|p| p.to_string_lossy().to_string()),
        "saved_at": SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    });
    let _ = std::fs::write(sidecar_path(autosave), value.to_string());
}

fn read_sidecar_project_path(autosave: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(sidecar_path(autosave)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value["project_path"].as_str().map(PathBuf::from)
}

impl App {
    /// Periodically mirror the active dirty PDF-project document to the autosave
    /// dir. Throttled to [`AUTOSAVE_INTERVAL`]; a no-op for clean documents and
    /// for documents that are not multi-page PDF sessions.
    pub fn maybe_autosave(&mut self) {
        if self.docs.last_autosave.elapsed() < AUTOSAVE_INTERVAL {
            return;
        }
        // Reset the timer even when nothing is written, so a clean/idle document
        // does not re-check the clock every frame.
        self.docs.last_autosave = Instant::now();

        let idx = self.docs.active_doc_idx;
        let Some(doc) = self.docs.documents.get(idx) else {
            return;
        };
        if doc.pdf_document.is_none() {
            return;
        }
        // `is_modified` already covers the active canvas and every cached page.
        if !doc.is_modified() {
            return;
        }
        self.write_autosave(idx);
    }

    fn write_autosave(&mut self, idx: usize) {
        self.sync_brush_gpu_to_cpu();
        let Some(dir) = autosave_dir() else {
            return;
        };
        let doc_id = self.docs.documents[idx].id;
        let path = self
            .docs
            .autosave_files
            .get(&doc_id)
            .cloned()
            .unwrap_or_else(|| {
                dir.join(format!("recover_{}_{}.iai", std::process::id(), doc_id.0))
            });
        match self.write_pdf_project(idx, &path) {
            Ok(_) => {
                let project_path = self.docs.documents[idx]
                    .path
                    .clone()
                    .filter(|existing| existing != &path);
                write_sidecar(&path, project_path.as_deref());
                self.docs.autosave_files.insert(doc_id, path);
            }
            Err(_) => {
                // Best effort — a failed autosave must never disrupt editing.
            }
        }
    }

    /// Remove the autosave file (if any) for the document at `idx`. Called after a
    /// successful project save and when a document is closed.
    pub fn clear_autosave(&mut self, idx: usize) {
        let Some(doc) = self.docs.documents.get(idx) else {
            return;
        };
        self.clear_autosave_for(doc.id);
    }

    pub(crate) fn clear_autosave_for(&mut self, id: DocumentId) {
        if let Some(path) = self.docs.autosave_files.remove(&id) {
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(sidecar_path(&path));
        }
    }

    pub(crate) fn clear_embedded_pdf_for(&mut self, id: DocumentId) {
        if let Some(path) = self.docs.embedded_pdf_files.remove(&id) {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Drop every tracked autosave file. Called at a clean app exit so a normal
    /// shutdown leaves nothing to recover.
    pub fn clear_all_autosave(&mut self) {
        for path in self.docs.autosave_files.values() {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(sidecar_path(path));
        }
        self.docs.autosave_files.clear();
        self.clear_all_embedded_pdf();
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
    /// Each recovered document is opened, pointed back at its original project
    /// path when known, and flagged unsaved so the user is prompted to save.
    pub fn check_crash_recovery(&mut self) {
        let mut recovered = 0usize;
        if let Some(dir) = autosave_dir() {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let is_iai = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| e.eq_ignore_ascii_case("iai"));
                    if !is_iai {
                        continue;
                    }
                    // Never re-adopt a file this session already owns.
                    if self
                        .docs
                        .autosave_files
                        .values()
                        .any(|owned| owned == &path)
                    {
                        continue;
                    }
                    if !crate::formats::iai::is_pdf_project(&path) {
                        continue;
                    }
                    let project = match crate::formats::iai::load(&path) {
                        Ok(crate::formats::iai::IaiLoad::PdfProject(project)) => project,
                        _ => continue,
                    };
                    let project_path = read_sidecar_project_path(&path);

                    self.install_pdf_project_recovered(path.clone(), project, project_path);
                    recovered += 1;
                }
            }
        }
        self.clear_orphaned_embedded_pdf_files();
        if recovered > 0 {
            self.shell.status_msg = format!(
                "Recovered {recovered} unsaved project(s) from a previous session — \
                 save to keep them"
            );
            if let Some(window) = &self.win.window {
                window.request_redraw();
            }
        }
    }
}

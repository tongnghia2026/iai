//! Saving and exporting the active document: PNG/JPEG/TIFF/.iai writes,
//! CMYK separations and multi-page PDF export.

use crate::app::autosave::content_fingerprint;
use crate::app::state::App;
use crate::core::canvas::Canvas;
use crate::core::document::{DocumentId, file_modified_at};
use crate::file_io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};

/// Whether a save holds its caller until the file is on disk.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum SaveWait {
    /// The caller goes on to act on the outcome: closing the tab, leaving the app.
    Finish,
    /// The user asked to save and keeps working; the outcome is taken in when
    /// the engine's job ends.
    Background,
}

/// A single image being written by the engine's `file.save` or
/// `file.export` job.
pub(in crate::app) struct SaveJob {
    doc_id: DocumentId,
    path: PathBuf,
    /// For a save, what the document held when it was asked for: a second
    /// Ctrl+S with nothing new is not started. `None` for an export, which
    /// leaves the document as it is.
    saved_content: Option<u64>,
    job: iai_engine::JobId,
}

/// The longest a save that holds its caller waits for the file to land.
const SAVE_PATIENCE: std::time::Duration = std::time::Duration::from_secs(600);

/// Ask where to write the PDF, with the native save dialog on a worker (it
/// must not re-enter winit/egui's UI thread). `None` when cancelled.
fn spawn_pdf_dialog(
    parent: Option<file_io::DialogParent>,
    stem: String,
) -> Receiver<Option<PathBuf>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut dialog = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name(format!("{stem}.pdf"));
        if let Some(parent) = &parent {
            dialog = dialog.set_parent(parent);
        }
        let path = dialog.save_file().map(|mut path| {
            if path.extension().is_none() {
                path.set_extension("pdf");
            }
            path
        });
        let _ = tx.send(path);
    });
    rx
}

/// One grayscale separation film, written so a failure keeps the previous film.
fn write_plate_png(path: &Path, plate: &[u8], width: u32, height: u32) -> Result<(), String> {
    crate::core::atomic_file::write_atomic(path, |tmp| {
        image::save_buffer(tmp, plate, width, height, image::ExtendedColorType::L8)
            .map_err(|e| e.to_string())
    })
}

fn requires_project_save_on_close(layer_count: usize, is_pdf_document: bool) -> bool {
    is_pdf_document || layer_count > 1
}

impl App {
    /// Save requested by an exit/close confirmation. A document with multiple
    /// layers defaults to an editable `.iai` project. A flat, single-layer
    /// document keeps the regular Save behavior and may overwrite its source
    /// PNG/JPEG/etc. An existing `.iai` project is always updated in place.
    pub fn do_save_project(&mut self) {
        if self
            .docs
            .documents
            .get(self.docs.active_doc_idx)
            .is_some_and(|doc| doc.is_flow_text())
        {
            self.do_save_with(SaveWait::Finish);
            return;
        }
        let requires_project = self
            .docs
            .documents
            .get(self.docs.active_doc_idx)
            .is_some_and(|doc| {
                requires_project_save_on_close(
                    doc.canvas.layer_stack.layers.len(),
                    doc.pdf_document.is_some(),
                )
            });
        if !requires_project {
            self.do_save_with(SaveWait::Finish);
            return;
        }

        let existing_project = self.docs.current_file.clone().filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("iai"))
        });
        if let Some(path) = existing_project {
            self.save_to(&path);
        } else {
            self.do_save_project_as();
        }
    }

    /// File ▸ Save / Ctrl+S: the user keeps working while a large image is
    /// written.
    pub fn do_save(&mut self) {
        self.do_save_with(SaveWait::Background);
    }

    fn do_save_with(&mut self, wait: SaveWait) {
        // Flowing text is an editable `.iai` document, never a flattened 1x1
        // compatibility canvas. PDF remains an explicit export operation.
        if self
            .docs
            .documents
            .get(self.docs.active_doc_idx)
            .is_some_and(|doc| doc.is_flow_text())
        {
            #[cfg(all(target_os = "windows", feature = "canvas-editor-webview"))]
            if self.request_document_webview_save_snapshot(false) {
                return;
            }
            let existing_iai = self.docs.current_file.clone().filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("iai"))
            });
            match existing_iai {
                Some(path) => {
                    self.save_flow_text_doc_to(&path);
                }
                None => self.do_save_project_as(),
            }
            return;
        }
        // Multi-page PDF sessions save as a full `.iai` project (all edited pages),
        // never as a flat single canvas — otherwise the other pages would be lost.
        if self
            .docs
            .documents
            .get(self.docs.active_doc_idx)
            .is_some_and(|doc| doc.pdf_document.is_some())
        {
            let existing_iai = self.docs.current_file.clone().filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("iai"))
            });
            match existing_iai {
                Some(path) => self.save_pdf_project_to(&path),
                None => self.do_save_as(),
            }
            return;
        }
        // A multi-page artboard document saves as a full `.iai` (every page),
        // never a flat single canvas.
        if self
            .docs
            .documents
            .get(self.docs.active_doc_idx)
            .is_some_and(|doc| !doc.pages.is_empty())
        {
            let existing_iai = self.docs.current_file.clone().filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("iai"))
            });
            match existing_iai {
                Some(path) => self.save_artboard_doc_to(&path),
                None => self.do_save_project_as(),
            }
            return;
        }
        if let Some(path) = self.docs.current_file.clone() {
            self.save_to_with(&path, wait);
        } else {
            self.do_save_as();
        }
    }

    pub fn do_save_as(&mut self) {
        if self
            .docs
            .documents
            .get(self.docs.active_doc_idx)
            .is_some_and(|doc| doc.is_flow_text())
        {
            #[cfg(all(target_os = "windows", feature = "canvas-editor-webview"))]
            if self.request_document_webview_save_snapshot(true) {
                return;
            }
            self.do_save_project_as();
            return;
        }
        self.do_save_as_with_suggestion(self.docs.current_file.clone());
    }

    /// Save the canonical flowing-text model. Layout pages, glyph caches and the
    /// 1x1 compatibility canvas are derived/session state and are intentionally
    /// excluded from the file.
    pub fn save_flow_text_doc_to(&mut self, path: &std::path::Path) -> bool {
        let idx = self.docs.active_doc_idx;
        let result = self
            .docs
            .documents
            .get(idx)
            .and_then(|doc| doc.flow_text.as_ref())
            .ok_or_else(|| "Không có tài liệu văn bản đang hoạt động".to_string())
            .and_then(|flow| match flow.backing() {
                crate::core::document::FlowTextBacking::Legacy(document) => {
                    crate::formats::iai::write_flow_text_doc(path, document.as_ref())
                }
                crate::core::document::FlowTextBacking::CanvasEditor(document) => {
                    crate::formats::iai::write_canvas_editor_doc(path, document)
                }
            });
        match result {
            Ok(()) => {
                let (_document_id, _revision) = {
                    let doc = &mut self.docs.documents[idx];
                    doc.path = Some(path.to_path_buf());
                    doc.file_modified_at = file_modified_at(path);
                    doc.mark_saved();
                    (
                        doc.id,
                        doc.flow_text.as_ref().map_or(0, |flow| flow.revision()),
                    )
                };
                #[cfg(all(target_os = "windows", feature = "canvas-editor-webview"))]
                self.mark_document_webview_saved(_document_id, _revision);
                self.docs.current_file = Some(path.to_path_buf());
                self.shell.status_msg = format!(
                    "Đã lưu văn bản: {}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                );
                self.clear_autosave(idx);
                true
            }
            Err(error) => {
                self.report_save_error(format!("Lỗi lưu văn bản: {error}"));
                false
            }
        }
    }

    /// Save a multi-page artboard document as a full `.iai` (every page). Gathers
    /// the active canvas plus the stored pages in tab order, writes them, then
    /// marks the whole document clean and remembers its path.
    pub fn save_artboard_doc_to(&mut self, path: &std::path::Path) {
        if self.edit.text_edit.is_some() {
            self.commit_text_edit();
        }
        self.sync_brush_gpu_to_cpu();
        let idx = self.docs.active_doc_idx;
        let result = {
            let doc = &self.docs.documents[idx];
            let n = doc.pages.len();
            let active = doc.active_artboard.min(n.saturating_sub(1));
            // `all_page_canvases` / `master_canvas` resolve the checked-out active
            // slot correctly even while the master is being edited.
            let refs = doc.all_page_canvases();
            let master = doc.master_canvas();
            crate::formats::iai::write_artboard_doc(path, &refs, active, master).map(|()| n.max(1))
        };
        match result {
            Ok(n) => {
                let doc = &mut self.docs.documents[idx];
                doc.path = Some(path.to_path_buf());
                doc.file_modified_at = file_modified_at(path);
                doc.mark_saved();
                self.docs.current_file = Some(path.to_path_buf());
                self.shell.status_msg = format!(
                    "Đã lưu tài liệu {n} trang: {}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                );
                // A real save supersedes any crash-recovery mirror.
                self.clear_autosave(idx);
            }
            Err(e) => self.report_save_error(format!("Lỗi lưu tài liệu: {e}")),
        }
    }

    pub(in crate::app) fn do_save_project_as(&mut self) {
        let suggestion = self.docs.current_file.clone().or_else(|| {
            self.docs
                .documents
                .get(self.docs.active_doc_idx)
                .and_then(|doc| doc.path.clone())
        });
        let suggestion = Some(
            suggestion
                .unwrap_or_else(|| std::path::PathBuf::from("untitled"))
                .with_extension("iai"),
        );
        self.do_save_as_with_suggestion(suggestion);
    }

    fn do_save_as_with_suggestion(&mut self, suggested_path: Option<std::path::PathBuf>) {
        if self.jobs.pending_file_dialog.is_some() {
            return;
        }
        let Some(window) = self.win.window.as_ref() else {
            return;
        };
        let parent = file_io::dialog_parent(window);
        // For a PDF project with no `.iai` yet, default the name to the source PDF's
        // stem so "Save" lands next to a sensible <document>.iai.
        let current = suggested_path.or_else(|| {
            self.docs
                .documents
                .get(self.docs.active_doc_idx)
                .and_then(|doc| doc.pdf_document.as_ref())
                .map(|pdf| pdf.source.with_extension("iai"))
        });
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Some(path) = file_io::dialog_save(current, parent) {
                let _ = tx.send(file_io::FileDialogResult::SaveAs(path));
            }
        });
        self.jobs.pending_file_dialog = Some(rx);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Save the active document to `path`, and return once it is on disk (or
    /// has failed).
    pub fn save_to(&mut self, path: &std::path::Path) {
        self.save_to_with(path, SaveWait::Finish);
    }

    /// Save the active document to `path`. A single image is snapshotted and
    /// written on a worker; `wait` says whether to stay for the outcome. The
    /// multi-page kinds and flowing text are written here and now either way.
    pub(in crate::app) fn save_to_with(&mut self, path: &std::path::Path, wait: SaveWait) {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if self
            .docs
            .documents
            .get(self.docs.active_doc_idx)
            .is_some_and(|doc| doc.is_flow_text())
        {
            if ext == "iai" {
                self.save_flow_text_doc_to(path);
            } else {
                self.report_save_error(
                    "Văn bản chỉnh sửa chỉ lưu được dạng .iai; dùng Xuất PDF để tạo PDF"
                        .to_string(),
                );
            }
            return;
        }
        if self.edit.text_edit.is_some() {
            self.commit_text_edit();
        }
        self.sync_brush_gpu_to_cpu();

        // A multi-page PDF session is written as a project, not a flat canvas.
        if self
            .docs
            .documents
            .get(self.docs.active_doc_idx)
            .is_some_and(|doc| doc.pdf_document.is_some())
        {
            if ext == "iai" {
                self.save_pdf_project_to(path);
            } else {
                self.report_save_error(
                    "Dự án PDF nhiều trang chỉ lưu được dạng .iai \
                     (dùng File ▸ Export để xuất PDF)"
                        .to_string(),
                );
            }
            return;
        }

        // A multi-page artboard document also writes every page as an `.iai`.
        if self
            .docs
            .documents
            .get(self.docs.active_doc_idx)
            .is_some_and(|doc| !doc.pages.is_empty())
        {
            if ext == "iai" {
                self.save_artboard_doc_to(path);
            } else {
                self.report_save_error(
                    "Tài liệu nhiều trang chỉ lưu được dạng .iai \
                     (dùng File ▸ Export để xuất ảnh)"
                        .to_string(),
                );
            }
            return;
        }

        let idx = self.docs.active_doc_idx;
        let doc_id = self.docs.documents[idx].id;
        let content = content_fingerprint(&self.docs.documents[idx].canvas);
        if let Some(under_way) = self
            .docs
            .save_jobs
            .iter()
            .find(|job| job.doc_id == doc_id && job.saved_content.is_some())
        {
            // Ctrl+S pressed again with nothing new to write.
            if wait == SaveWait::Background
                && under_way.saved_content == Some(content)
                && under_way.path == path
            {
                return;
            }
            // Two writers on one file would race; let the first one land.
            self.finish_save_jobs();
        }

        // A name with no ending is written as a PNG.
        let path = if path.extension().is_none() {
            path.with_extension("png")
        } else {
            path.to_path_buf()
        };
        let mut args = serde_json::json!({
            "doc": doc_id.0,
            "path": path.display().to_string(),
        });
        if ext != "iai" {
            self.add_image_options(&mut args);
        }
        self.start_write("file.save", args, doc_id, path, Some(content), wait);
    }

    /// The Export options the person set (Preferences), as the file commands
    /// take them.
    fn add_image_options(&self, args: &mut serde_json::Value) {
        let ui = &self.shell.ui;
        args["embedIcc"] = ui.export_embed_icc.into();
        args["sharpen"] = ui.export_output_sharpen.min(100).into();
        if ui.export_resize_enabled {
            args["longEdgePx"] = ui.export_resize_long_edge.clamp(16, 30000).into();
        }
    }

    /// Run `command` (`file.save` or `file.export`) as a job writing `path`.
    /// `saved_content` as in [`SaveJob`]; `wait` says whether to stay until the
    /// file has landed.
    fn start_write(
        &mut self,
        command: &str,
        args: serde_json::Value,
        doc_id: DocumentId,
        path: PathBuf,
        saved_content: Option<u64>,
        wait: SaveWait,
    ) {
        let job = match self.start_command(command, args) {
            Some(iai_engine::Reply::Started { job }) => job,
            // Done at once: nothing is under way.
            Some(iai_engine::Reply::Done { .. }) => return,
            None => {
                let error = self.shell.status_msg.clone();
                self.report_save_error(error);
                return;
            }
        };
        self.docs.save_jobs.push(SaveJob {
            doc_id,
            path,
            saved_content,
            job,
        });
        match wait {
            SaveWait::Finish => self.wait_for_save_job(job),
            SaveWait::Background => {
                if let Some(window) = &self.win.window {
                    window.request_redraw();
                }
            }
        }
    }

    /// Stay until the job `job` has ended and been taken in.
    fn wait_for_save_job(&mut self, job: iai_engine::JobId) {
        let started = std::time::Instant::now();
        while self.docs.save_jobs.iter().any(|s| s.job == job)
            && self.engine.busy()
            && started.elapsed() < SAVE_PATIENCE
        {
            self.poll_engine();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Stay until every save and export under way has landed. For the moments
    /// that act on the outcome: closing a tab, leaving the app.
    pub(crate) fn finish_save_jobs(&mut self) {
        let jobs: Vec<_> = self.docs.save_jobs.iter().map(|s| s.job).collect();
        for job in jobs {
            self.wait_for_save_job(job);
        }
    }

    /// Whether the document at `idx` is being saved right now.
    pub(in crate::app) fn save_under_way(&self, idx: usize) -> bool {
        self.docs.documents.get(idx).is_some_and(|doc| {
            self.docs
                .save_jobs
                .iter()
                .any(|job| job.doc_id == doc.id && job.saved_content.is_some())
        })
    }

    /// A `file.save` or `file.export` job ended. What it wrote was taken in
    /// by the engine (the document's file and saved state, the recovery copy
    /// dropped); a failure raises the save error.
    pub(in crate::app) fn on_write_finished(
        &mut self,
        job: iai_engine::JobId,
        error: Option<&iai_engine::CmdError>,
    ) {
        let Some(at) = self.docs.save_jobs.iter().position(|s| s.job == job) else {
            return;
        };
        let write = self.docs.save_jobs.remove(at);
        if let Some(e) = error {
            let what = if write.saved_content.is_some() {
                "Lỗi lưu"
            } else {
                "Lỗi xuất file"
            };
            self.report_save_error(format!("{what}: {}", crate::app::engine::error_line(e)));
        }
    }

    /// Open the native Save dialog to pick an export path, then export. MUST be called
    /// from apply_ui_actions (OUTSIDE the egui frame) — same `rendering` guard pattern
    /// as do_save_as — so rfd's modal loop doesn't re-enter a half-finished egui pass
    /// (the cause of the app + Windows dialog freezing on Export).
    pub fn do_export_browse(&mut self, format: crate::formats::ExportFormat) {
        if self.jobs.pending_file_dialog.is_some() {
            return;
        }
        let ext = format.extension().to_string();
        let name = format.name().to_string();
        let stem = self
            .docs
            .current_file
            .as_deref()
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .unwrap_or("untitled")
            .to_string();

        let Some(window) = self.win.window.as_ref() else {
            return;
        };
        let parent = file_io::dialog_parent(window);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new()
                .add_filter(&name, &[ext.as_str()])
                .set_file_name(format!("{stem}.{ext}"));
            if let Some(p) = parent {
                dialog = dialog.set_parent(&p);
            }
            if let Some(path) = dialog.save_file() {
                let _ = tx.send(file_io::FileDialogResult::Export(format, path));
            }
        });
        self.jobs.pending_file_dialog = Some(rx);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Export the active page to `path` in `format`, on a worker thread.
    pub fn do_export(&mut self, format: crate::formats::ExportFormat, path: &str) {
        let mut path_buf = std::path::PathBuf::from(path);
        path_buf.set_extension(format.extension());

        self.sync_brush_gpu_to_cpu();

        // `file.export` lays a page that shows the shared master over it for a
        // picture; a `.iai` copy keeps the page's own layers.
        let is_iai = matches!(format, crate::formats::ExportFormat::Iai);
        let doc_id = self.docs.active().id;
        let mut args = serde_json::json!({
            "doc": doc_id.0,
            "path": path_buf.display().to_string(),
        });
        if !is_iai {
            self.add_image_options(&mut args);
        }
        self.start_write(
            "file.export",
            args,
            doc_id,
            path_buf,
            None,
            SaveWait::Background,
        );
    }

    /// File ▸ Export ▸ SVG (Vector) — write the active document as an SVG: its
    /// qualifying vector objects as native `<path>` elements over an embedded PNG
    /// of the raster beneath them (a pure-vector document embeds no image). Meant
    /// for the web and cut plotters. Blocking rfd is fine here.
    pub fn export_svg(&mut self) {
        self.sync_brush_gpu_to_cpu();
        let idx = self.docs.active_doc_idx;
        let svg = match crate::formats::svg::build_svg(&self.docs.documents[idx].canvas) {
            Ok(svg) => svg,
            Err(e) => {
                self.shell.status_msg = format!("SVG export error: {e}");
                return;
            }
        };
        let Some(window) = self.win.window.as_ref() else {
            return;
        };
        let parent = file_io::dialog_parent(window);
        let mut dialog = rfd::FileDialog::new()
            .add_filter("SVG", &["svg"])
            .set_file_name("artwork.svg");
        if let Some(p) = &parent {
            dialog = dialog.set_parent(p);
        }
        let Some(mut path) = dialog.save_file() else {
            return;
        };
        path.set_extension("svg");
        match crate::core::atomic_file::write_bytes_atomic(&path, svg.as_bytes()) {
            Ok(_) => {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("artwork.svg");
                self.shell.status_msg = format!("Exported SVG: {name}");
            }
            Err(e) => self.report_save_error(format!("Lỗi xuất SVG: {e}")),
        }
    }

    /// File ▸ Export ▸ CMYK Separations… — write four grayscale ink plates
    /// (`<base>_C/_M/_Y/_K.png`, print convention: full ink = black). On a CMYK
    /// document the plates come straight from the ink planes (`flatten_ink`,
    /// falling back to the mirror through the document's own converter); on an
    /// RGB document the flattened page is converted through a chosen CMYK
    /// device profile. Blocking rfd is fine here (apply_ui_actions runs
    /// outside the egui frame).
    pub fn export_cmyk_separations(&mut self) {
        self.sync_brush_gpu_to_cpu();
        let (cw, ch) = {
            let c = &self.docs.active().canvas;
            (c.width, c.height)
        };
        if !Canvas::fits_flat_buffer(cw, ch) {
            self.shell.status_msg = "Canvas quá lớn để tách màu CMYK".to_string();
            return;
        }
        let Some(window) = self.win.window.as_ref() else {
            return;
        };
        let parent = file_io::dialog_parent(window);
        let doc_is_cmyk = self.docs.active().canvas.is_cmyk();

        // 1) Pick the CMYK device profile — only for RGB documents; a CMYK
        // document already carries its own.
        let mut icc: Option<Vec<u8>> = None;
        if !doc_is_cmyk {
            let mut d1 = rfd::FileDialog::new().add_filter("CMYK ICC Profile", &["icc", "icm"]);
            if let Some(p) = &parent {
                d1 = d1.set_parent(p);
            }
            let Some(icc_path) = d1.pick_file() else {
                return;
            };
            let bytes = match std::fs::read(&icc_path) {
                Ok(b) => b,
                Err(e) => {
                    self.shell.status_msg = format!("Không đọc được profile: {e}");
                    return;
                }
            };
            if !crate::core::cms::profile_is_cmyk(&bytes) {
                self.shell.status_msg = "Profile đã chọn không phải CMYK".to_string();
                return;
            }
            icc = Some(bytes);
        }

        // 2) Pick the output base path.
        let mut d2 = rfd::FileDialog::new()
            .add_filter("PNG", &["png"])
            .set_file_name("separation.png");
        if let Some(p) = &parent {
            d2 = d2.set_parent(p);
        }
        let Some(base) = d2.save_file() else {
            return;
        };

        let n = (cw as usize) * (ch as usize);
        let mut ink_exact = false;
        let mut cmyk: Vec<u8> = if doc_is_cmyk {
            if let Some(ink) = self.docs.active().canvas.flatten_ink() {
                ink_exact = true;
                ink
            } else {
                // Mirror fallback (groups/blend modes/masks in play): flatten
                // onto white and convert through the document's own converter.
                let Some(conv) = self.docs.active().canvas.cmyk_converter() else {
                    self.shell.status_msg = "Chuyển đổi CMYK thất bại".to_string();
                    return;
                };
                let rgba = self.docs.active().canvas.export_flat();
                let mut rgb = vec![[0u8; 3]; n];
                for i in 0..n {
                    let a = rgba[i * 4 + 3] as u16;
                    let inv = 255 - a;
                    rgb[i] = [
                        ((rgba[i * 4] as u16 * a + 255 * inv) / 255) as u8,
                        ((rgba[i * 4 + 1] as u16 * a + 255 * inv) / 255) as u8,
                        ((rgba[i * 4 + 2] as u16 * a + 255 * inv) / 255) as u8,
                    ];
                }
                let mut ink = vec![[0u8; 4]; n];
                conv.rgb_to_cmyk_slice(&rgb, &mut ink);
                bytemuck::cast_vec(ink)
            }
        } else {
            // RGB document: flatten onto white → packed RGB → chosen profile.
            let rgba = self.docs.active().canvas.export_flat();
            let mut rgb = vec![0u8; n * 3];
            for i in 0..n {
                let a = rgba[i * 4 + 3] as u16;
                let inv = 255 - a;
                rgb[i * 3] = ((rgba[i * 4] as u16 * a + 255 * inv) / 255) as u8;
                rgb[i * 3 + 1] = ((rgba[i * 4 + 1] as u16 * a + 255 * inv) / 255) as u8;
                rgb[i * 3 + 2] = ((rgba[i * 4 + 2] as u16 * a + 255 * inv) / 255) as u8;
            }
            match crate::core::cms::srgb_rgb_to_cmyk8(
                &rgb,
                icc.as_deref().unwrap_or_default(),
                crate::core::cms::DEFAULT_INTENT,
            ) {
                Some(c) => c,
                None => {
                    self.shell.status_msg = "Chuyển đổi CMYK thất bại".to_string();
                    return;
                }
            }
        };

        // Spot inks painted by promoted vector objects each get their OWN film,
        // and (unless the object overprints) knock their footprint out of the
        // process plates, so a spot area prints once — on its plate — instead of
        // twice (spot film + the CMYK approximation baked into the raster base).
        // A document with no spot inks yields no plates and a zero knockout, so
        // the C/M/Y/K plates below stay byte-identical to before.
        let (spot_seps, knockout) = {
            let canvas = &self.docs.active().canvas;
            let objects = crate::core::print::collect_pdf_vectors(canvas).objects;
            crate::core::print::spot_separations(&objects, cw, ch)
        };
        if knockout.iter().any(|&k| k > 0.0) {
            for i in 0..n {
                let keep = 1.0 - knockout[i].clamp(0.0, 1.0);
                for plane in 0..4 {
                    cmyk[i * 4 + plane] = (cmyk[i * 4 + plane] as f32 * keep).round() as u8;
                }
            }
        }

        let stem = base
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("separation")
            .to_string();
        let dir = base.parent().map(Path::to_path_buf).unwrap_or_default();
        for (idx, suffix) in ["C", "M", "Y", "K"].iter().enumerate() {
            let mut plate = vec![0u8; n];
            for i in 0..n {
                // Print-plate convention: paper (no ink) = white, full ink = black.
                plate[i] = 255 - cmyk[i * 4 + idx];
            }
            let path = dir.join(format!("{stem}_{suffix}.png"));
            if let Err(e) = write_plate_png(&path, &plate, cw, ch) {
                self.report_save_error(format!("Lỗi ghi bản {suffix}: {e}"));
                return;
            }
        }

        // One extra grayscale film per spot colorant (same print convention:
        // bare paper = white, solid ink = black), named by the ink itself.
        for sep in &spot_seps {
            let mut plate = vec![0u8; n];
            for (dst, &cov) in plate.iter_mut().zip(&sep.coverage) {
                *dst = 255 - (cov.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
            let safe = crate::core::print::sanitize_plate_name(sep.name.as_str());
            let path = dir.join(format!("{stem}_{safe}.png"));
            if let Err(e) = write_plate_png(&path, &plate, cw, ch) {
                self.report_save_error(format!("Lỗi ghi bản spot {safe}: {e}"));
                return;
            }
        }

        self.shell.status_msg = if spot_seps.is_empty() {
            if ink_exact {
                format!("Đã xuất tách màu từ ink planes: {stem}_C/M/Y/K.png")
            } else {
                format!("Đã xuất tách màu CMYK: {stem}_C/M/Y/K.png")
            }
        } else {
            format!(
                "Đã xuất tách màu CMYK + {} bản spot: {stem}_C/M/Y/K + spot .png",
                spot_seps.len()
            )
        };
    }

    /// Execute the choices from the unified "Xuất PDF" dialog. Page numbers are
    /// positions in the active document (or in the imported PDF's selected-page
    /// list), always written in document order.
    pub fn run_pdf_export(&mut self) {
        if self.jobs.pending_pdf_export.is_some() || self.jobs.pending_file_dialog.is_some() {
            self.shell.status_msg = "Một tác vụ xuất tệp khác đang chạy".to_string();
            return;
        }
        let Some((args, stem)) = self.pdf_export_args() else {
            return;
        };
        let Some(window) = self.win.window.as_ref() else {
            return;
        };
        let parent = file_io::dialog_parent(window);
        let rx = spawn_pdf_dialog(parent, stem);
        self.jobs.pending_pdf_export =
            Some(crate::app::background_jobs::PdfExport::Asking { rx, args });
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }

    /// The Export PDF dialog's choices as `export.pdf` params (all but the
    /// path), and the file name to offer. `None` when they ask for no page
    /// (the dialog is shown again, the status line says why).
    pub(in crate::app) fn pdf_export_args(&mut self) -> Option<(serde_json::Value, String)> {
        use crate::ui::intent::PdfExportScope;

        let idx = self.docs.active_doc_idx;
        let flow_text_active = self
            .docs
            .documents
            .get(idx)
            .is_some_and(|doc| doc.is_flow_text());
        if !flow_text_active && self.edit.text_edit.is_some() {
            self.commit_text_edit();
        }
        if !flow_text_active {
            self.sync_brush_gpu_to_cpu();
        }
        if !flow_text_active && idx < self.docs.documents.len() {
            self.docs.documents[idx].reconcile_pdf_page_modified();
        }
        let doc = self.docs.documents.get(idx)?;
        let marks = self.shell.ui.export_pdf_marks;
        let mut args = serde_json::json!({
            "cropMarks": marks.crop_marks,
            "registrationMarks": marks.registration_marks,
            "bleedMm": marks.bleed_mm.clamp(0.0, 20.0),
            "embedIcc": self.shell.ui.export_embed_icc,
        });
        let target_dpi = self.shell.ui.pdf_export_dpi;
        if target_dpi > 0 {
            args["dpi"] = target_dpi.clamp(36, 2400).into();
        }
        // Batch: combine every open tab into a single PDF (each tab's page(s) →
        // PDF pages, in tab order). The page scope only describes the active
        // document.
        if self.shell.ui.pdf_export_scope == PdfExportScope::AllOpenDocuments {
            args["allOpenDocuments"] = true.into();
        } else {
            let page_count = if let Some(pdf) = doc.pdf_document.as_ref() {
                pdf.selected_pages.len().max(1)
            } else {
                doc.page_count()
            };
            let current = if let Some(pdf) = doc.pdf_document.as_ref() {
                pdf.selected_pages
                    .iter()
                    .position(|&page| page == pdf.active_page)
                    .unwrap_or(0)
            } else if let Some(flow) = doc.flow_text.as_ref() {
                flow.active_page()
            } else {
                doc.active_artboard.min(page_count.saturating_sub(1))
            };
            let pages = match self.shell.ui.pdf_export_scope {
                PdfExportScope::CurrentPage => Ok(vec![current]),
                PdfExportScope::Range => crate::ui::intent::parse_pdf_page_range(
                    &self.shell.ui.pdf_export_range,
                    page_count,
                ),
                PdfExportScope::AllPages | PdfExportScope::AllOpenDocuments => {
                    Ok((0..page_count).collect())
                }
            };
            let pages = match pages {
                Ok(pages) if !pages.is_empty() => pages,
                Ok(_) => {
                    self.shell.status_msg = "Không có trang nào để xuất PDF".to_string();
                    self.shell.ui.show_pdf_export_dialog = true;
                    return None;
                }
                Err(error) => {
                    self.shell.status_msg = error;
                    self.shell.ui.show_pdf_export_dialog = true;
                    return None;
                }
            };
            args["doc"] = doc.id.0.into();
            args["pages"] = pages.into();
        }
        // The file name offered: the document's, or a neutral one.
        let stem = doc
            .pdf_document
            .as_ref()
            .map(|pdf| pdf.source.as_path())
            .or(doc.path.as_deref())
            .or(self.docs.current_file.as_deref())
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .unwrap_or(if flow_text_active {
                "tai-lieu"
            } else {
                "document"
            })
            .to_string();
        Some((args, stem))
    }

    /// Once the save dialog answers, write the PDF with `export.pdf` on a
    /// worker; never blocks the UI thread.
    pub(crate) fn poll_pdf_export(&mut self) {
        use crate::app::background_jobs::PdfExport;
        let Some(PdfExport::Asking { rx, .. }) = self.jobs.pending_pdf_export.as_ref() else {
            return;
        };
        let answer = match rx.try_recv() {
            Ok(answer) => answer,
            Err(TryRecvError::Empty) => {
                if let Some(window) = &self.win.window {
                    window.request_redraw();
                }
                return;
            }
            Err(TryRecvError::Disconnected) => {
                self.jobs.pending_pdf_export = None;
                self.shell.status_msg = "Tác vụ xuất PDF dừng ngoài dự kiến".to_string();
                return;
            }
        };
        let Some(PdfExport::Asking { mut args, .. }) = self.jobs.pending_pdf_export.take() else {
            return;
        };
        let Some(path) = answer else {
            self.shell.status_msg = "Đã hủy xuất PDF".to_string();
            return;
        };
        args["path"] = path.display().to_string().into();
        match self.start_command("export.pdf", args) {
            Some(iai_engine::Reply::Started { job }) => {
                self.jobs.pending_pdf_export = Some(PdfExport::Writing(job));
            }
            Some(iai_engine::Reply::Done { .. }) => {}
            None => {
                let error = self.shell.status_msg.clone();
                self.report_save_error(error);
            }
        }
    }

    /// `export.pdf` ended: the export no longer holds the window; a failure
    /// is shown as a save failure is.
    pub(crate) fn on_pdf_export_finished(
        &mut self,
        job: iai_engine::JobId,
        error: Option<&iai_engine::CmdError>,
    ) {
        use crate::app::background_jobs::PdfExport;
        if !matches!(self.jobs.pending_pdf_export, Some(PdfExport::Writing(j)) if j == job) {
            return;
        }
        self.jobs.pending_pdf_export = None;
        if let Some(e) = error {
            self.report_save_error(crate::app::engine::error_line(e));
        }
    }

    /// Pick a picture (native dialog on a worker thread) to insert into the
    /// active flowing-text document. The decode happens off the UI thread; the
    /// bytes reach the document editor via `poll_flow_text_image`.
    pub fn pick_flow_text_image(&mut self) {
        if self.jobs.pending_flow_image.is_some() {
            return;
        }
        let Some(window) = self.win.window.as_ref() else {
            return;
        };
        let parent = file_io::dialog_parent(window);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new()
                .add_filter(
                    "Ảnh",
                    &[
                        "png", "jpg", "jpeg", "jfif", "jpe", "bmp", "gif", "tiff", "tif", "webp",
                    ],
                )
                .set_title("Chèn ảnh");
            if let Some(parent) = &parent {
                dialog = dialog.set_parent(parent);
            }
            let result = dialog.pick_file().and_then(|path| {
                let (w, h) = image::image_dimensions(&path).ok()?;
                let bytes = std::fs::read(&path).ok()?;
                Some((bytes, w, h))
            });
            let _ = tx.send(result);
        });
        self.jobs.pending_flow_image = Some(rx);
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }

    /// Drain the image picker worker; hand a decoded picture to the document
    /// editor for insertion at the caret.
    pub(crate) fn poll_flow_text_image(&mut self) {
        let result = match self.jobs.pending_flow_image.as_ref() {
            Some(rx) => rx.try_recv(),
            None => return,
        };
        match result {
            Ok(Some((data, w, h))) => {
                self.jobs.pending_flow_image = None;
                crate::ui::document_mode::queue_image(data, w, h);
                if let Some(window) = &self.win.window {
                    window.request_redraw();
                }
            }
            Ok(None) => {
                self.jobs.pending_flow_image = None; // dialog cancelled
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                if let Some(window) = &self.win.window {
                    window.request_redraw();
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.jobs.pending_flow_image = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{App, SaveWait, requires_project_save_on_close};
    use crate::core::canvas::Canvas;
    use crate::core::print::prepare_pdf_pixels;

    /// A fresh folder per test, removed when the test ends.
    struct Folder(std::path::PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "iai_save_{name}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// An app whose one document is a small image of `rgba`, with unsaved work.
    fn app_with_image(rgba: [u8; 4]) -> App {
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        app.docs.documents[0].canvas = Canvas::new(8, 8);
        paint(&mut app, rgba);
        app
    }

    fn paint(app: &mut App, rgba: [u8; 4]) {
        let doc = &mut app.docs.documents[0];
        let pixels: Vec<u8> = rgba.iter().copied().cycle().take(8 * 8 * 4).collect();
        doc.canvas.layer_stack.active_layer_mut().tiles =
            crate::core::tile::TileMap::from_rgba(&pixels, 8, 8);
        doc.canvas.mark_dirty_unconditionally();
    }

    fn first_pixel(path: &std::path::Path) -> [u8; 4] {
        image::open(path).unwrap().to_rgba8().get_pixel(0, 0).0
    }

    #[test]
    fn a_save_in_the_background_lands_and_leaves_the_document_saved() {
        let folder = Folder::new("background");
        let path = folder.0.join("anh.png");
        let mut app = app_with_image([10, 200, 30, 255]);

        app.save_to_with(&path, SaveWait::Background);
        assert!(app.save_under_way(0));
        assert!(
            app.docs.documents[0].is_modified(),
            "not saved until the file has landed"
        );
        app.finish_save_jobs();

        assert!(!app.save_under_way(0));
        assert_eq!(first_pixel(&path), [10, 200, 30, 255]);
        let doc = &app.docs.documents[0];
        assert!(!doc.is_modified());
        assert_eq!(doc.path.as_deref(), Some(path.as_path()));
        assert_eq!(app.docs.current_file.as_deref(), Some(path.as_path()));
        assert!(app.shell.ui.save_error.is_none());
    }

    #[test]
    fn work_done_while_the_file_is_written_stays_unsaved() {
        let folder = Folder::new("edited");
        let path = folder.0.join("anh.png");
        let mut app = app_with_image([10, 200, 30, 255]);

        app.save_to_with(&path, SaveWait::Background);
        paint(&mut app, [200, 0, 0, 255]);
        app.finish_save_jobs();

        assert_eq!(
            first_pixel(&path),
            [10, 200, 30, 255],
            "the file holds what was there when save was asked"
        );
        let doc = &app.docs.documents[0];
        assert!(doc.is_modified(), "the later stroke is not in the file");
        assert_eq!(doc.path.as_deref(), Some(path.as_path()));
        assert!(app.shell.status_msg.contains("chưa nằm trong file"));
    }

    #[test]
    fn a_background_save_that_fails_keeps_the_old_file_and_raises_the_modal() {
        let folder = Folder::new("fails");
        let path = folder.0.join("anh.iai");
        std::fs::write(&path, b"the file that was there").unwrap();
        // A folder squatting on the archive's temporary name fails the write.
        std::fs::create_dir(path.with_extension("iai.tmp")).unwrap();
        let mut app = app_with_image([1, 2, 3, 255]);

        app.save_to_with(&path, SaveWait::Background);
        assert!(app.shell.ui.save_error.is_none(), "not known yet");
        app.finish_save_jobs();

        assert!(app.shell.ui.save_error.is_some());
        assert_eq!(std::fs::read(&path).unwrap(), b"the file that was there");
        let doc = &app.docs.documents[0];
        assert!(doc.is_modified());
        assert!(doc.path.is_none(), "not pointed at a file never written");
    }

    #[test]
    fn closing_the_tab_waits_for_its_save_instead_of_asking_again() {
        let folder = Folder::new("close");
        let path = folder.0.join("anh.png");
        let mut app = app_with_image([10, 200, 30, 255]);

        app.save_to_with(&path, SaveWait::Background);
        app.close_doc(0);

        assert!(
            !app.shell.ui.show_close_dialog,
            "the save had been asked for: no unsaved-changes prompt"
        );
        assert_eq!(first_pixel(&path), [10, 200, 30, 255]);
        assert!(app.docs.save_jobs.is_empty());
    }

    #[test]
    fn a_second_save_with_nothing_new_is_not_started() {
        let folder = Folder::new("twice");
        let path = folder.0.join("anh.png");
        let mut app = app_with_image([10, 200, 30, 255]);

        app.save_to_with(&path, SaveWait::Background);
        app.save_to_with(&path, SaveWait::Background);
        assert_eq!(app.docs.save_jobs.len(), 1);

        // With new work it is: the first lands, then the second is written.
        paint(&mut app, [0, 0, 250, 255]);
        app.save_to_with(&path, SaveWait::Background);
        assert_eq!(app.docs.save_jobs.len(), 1);
        app.finish_save_jobs();
        assert_eq!(first_pixel(&path), [0, 0, 250, 255]);
        assert!(!app.docs.documents[0].is_modified());
    }

    #[test]
    fn an_export_runs_in_the_background_and_leaves_the_document_as_it_is() {
        let folder = Folder::new("export");
        let path = folder.0.join("xuat.png");
        let mut app = app_with_image([40, 50, 60, 255]);

        app.do_export(
            crate::formats::ExportFormat::Png { compression: 6 },
            &path.to_string_lossy(),
        );
        assert_eq!(app.docs.save_jobs.len(), 1);
        app.finish_save_jobs();

        assert_eq!(first_pixel(&path), [40, 50, 60, 255]);
        let doc = &app.docs.documents[0];
        assert!(doc.is_modified(), "an export is not a save");
        assert!(doc.path.is_none());
    }

    #[test]
    fn close_save_keeps_source_format_for_single_layer_image() {
        assert!(!requires_project_save_on_close(1, false));
    }

    #[test]
    fn close_save_defaults_to_project_for_multiple_layers_or_pdf() {
        assert!(requires_project_save_on_close(2, false));
        assert!(requires_project_save_on_close(1, true));
    }

    #[test]
    fn pdf_dpi_downsamples_without_changing_physical_size() {
        let pixels = vec![128; 1200 * 600 * 4];
        let (_, width, height, dpi) = prepare_pdf_pixels(pixels, 1200, 600, 600.0, 300).unwrap();
        assert_eq!((width, height, dpi), (600, 300, 300.0));
    }

    #[test]
    fn a_save_that_fails_raises_the_modal_and_leaves_the_document_unsaved() {
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        app.docs.documents[0].canvas.mark_dirty_unconditionally();
        // A missing folder is made; one that is a file cannot be.
        let folder = Folder::new("not_a_folder");
        let not_a_folder = folder.0.join("a_file");
        std::fs::write(&not_a_folder, b"x").unwrap();

        for name in ["anh.png", "anh.iai"] {
            app.shell.ui.save_error = None;
            app.save_to(&not_a_folder.join(name));

            let message = app
                .shell
                .ui
                .save_error
                .clone()
                .expect("the modal is raised");
            assert_eq!(message, app.shell.status_msg, "{name}: status line kept");
            assert!(app.is_blocking_modal(), "{name}");
            let doc = &app.docs.documents[0];
            assert!(doc.is_modified(), "{name}: still unsaved");
            assert!(
                doc.path.is_none(),
                "{name}: not pointed at a file never written"
            );
        }
    }

    /// The Export PDF dialog as the window runs it: its choices, the save
    /// dialog's answer, then `export.pdf` on a worker. The window is held
    /// until the file is written; a page range picks the pages.
    fn export_pdf_to(app: &mut App, path: &std::path::Path) {
        use crate::app::background_jobs::PdfExport;
        let (args, _stem) = app.pdf_export_args().expect("pages to export");
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Some(path.to_path_buf())).unwrap();
        app.jobs.pending_pdf_export = Some(PdfExport::Asking { rx, args });
        app.poll_pdf_export();
        assert!(matches!(
            app.jobs.pending_pdf_export,
            Some(PdfExport::Writing(_))
        ));
        assert!(app.modal_lock_active(), "the export holds the window");
        app.finish_commands(std::time::Duration::from_secs(60));
        assert!(app.jobs.pending_pdf_export.is_none());
    }

    #[test]
    fn a_pdf_is_exported_through_the_command_with_the_pages_asked_for() {
        use crate::ui::intent::PdfExportScope;
        let folder = Folder::new("pdf");
        let mut app = app_with_image([10, 200, 30, 255]);
        app.add_artboard();
        app.add_artboard();
        let all = folder.0.join("ba trang.pdf");
        export_pdf_to(&mut app, &all);
        assert_eq!(app.shell.status_msg, "Đã xuất PDF 3 trang: ba trang.pdf");
        assert!(std::fs::read(&all).unwrap().starts_with(b"%PDF"));

        app.shell.ui.pdf_export_scope = PdfExportScope::Range;
        app.shell.ui.pdf_export_range = "1, 3".to_string();
        let two = folder.0.join("hai.pdf");
        export_pdf_to(&mut app, &two);
        assert_eq!(app.shell.status_msg, "Đã xuất PDF 2 trang: hai.pdf");

        app.shell.ui.pdf_export_scope = PdfExportScope::AllOpenDocuments;
        let combined = folder.0.join("gop.pdf");
        export_pdf_to(&mut app, &combined);
        assert_eq!(
            app.shell.status_msg,
            "Đã gộp 1 tài liệu (3 trang) thành PDF: gop.pdf"
        );

        // A range naming no page asks again, and starts nothing.
        app.shell.ui.pdf_export_scope = PdfExportScope::Range;
        app.shell.ui.pdf_export_range = "7".to_string();
        assert!(app.pdf_export_args().is_none());
        assert!(app.shell.ui.show_pdf_export_dialog);
    }

    #[test]
    fn a_pdf_that_cannot_be_written_raises_the_save_error() {
        let mut app = app_with_image([10, 200, 30, 255]);
        let folder = Folder::new("pdf_fail");
        // A folder where the file should be.
        let path = folder.0.join("la thu muc.pdf");
        std::fs::create_dir_all(&path).unwrap();
        export_pdf_to(&mut app, &path);
        assert!(
            app.shell.ui.save_error.is_some(),
            "{}",
            app.shell.status_msg
        );
    }

    #[test]
    fn pdf_dpi_never_upscales() {
        let pixels = vec![255; 20 * 10 * 4];
        let (_, width, height, dpi) = prepare_pdf_pixels(pixels, 20, 10, 150.0, 300).unwrap();
        assert_eq!((width, height, dpi), (20, 10, 150.0));
    }
}

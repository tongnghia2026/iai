//! Saving and exporting the active document: PNG/JPEG/TIFF/.iai writes,
//! CMYK separations and multi-page PDF export.

use crate::app::autosave::{content_fingerprint, detached_canvas};
use crate::app::state::App;
use crate::core::canvas::Canvas;
use crate::core::document::{file_modified_at, DocumentId};
use crate::file_io;
use crate::formats::ExportOptions;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};

/// Whether a save holds its caller until the file is on disk.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum SaveWait {
    /// The caller goes on to act on the outcome: closing the tab, leaving the app.
    Finish,
    /// The user asked to save and keeps working; the outcome is taken in by
    /// `poll_save_jobs`.
    Background,
}

/// A single image being written on a worker thread.
pub(in crate::app) struct SaveJob {
    doc_id: DocumentId,
    path: PathBuf,
    /// For a save, what the document held when it was snapshotted: it counts as
    /// saved only if it still holds exactly that when the file lands. `None`
    /// for an export, which leaves the document as it is.
    saved_content: Option<u64>,
    done: Receiver<Result<PathBuf, String>>,
}

/// Worker body: encode `canvas` into `path`. Returns the file written, which
/// gains `.png` when `path` named no format.
fn write_image(canvas: &Canvas, path: &Path, opts: &ExportOptions) -> Result<PathBuf, String> {
    let has_exporter = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| crate::formats::ExportFormat::from_extension(e).is_some());
    if has_exporter {
        crate::formats::FormatRegistry::new()
            .export(canvas, path, opts)
            .map(|()| path.to_path_buf())
    } else {
        file_io::save(canvas, path).map_err(|e| e.to_string())
    }
}

type PdfExportReceiver = std::sync::mpsc::Receiver<Option<Result<String, String>>>;

/// Run both the native save dialog and the expensive PDF build on a worker.
/// Keeping the dialog here also avoids re-entering winit/egui's UI thread.
fn spawn_pdf_export<F>(
    parent: Option<file_io::DialogParent>,
    stem: String,
    export: F,
) -> PdfExportReceiver
where
    F: FnOnce(PathBuf) -> Result<String, String> + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut dialog = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name(format!("{stem}.pdf"));
        if let Some(parent) = &parent {
            dialog = dialog.set_parent(parent);
        }
        let result = dialog.save_file().map(|mut path| {
            if path.extension().is_none() {
                path.set_extension("pdf");
            }
            export(path)
        });
        let _ = tx.send(result);
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

/// Downsample a four-channel page buffer for PDF output while preserving its
/// physical page size. `target_dpi == 0` means keep the source resolution;
/// export never upscales a page.
fn prepare_pdf_pixels(
    pixels: Vec<u8>,
    width: u32,
    height: u32,
    source_dpi: f32,
    target_dpi: u32,
) -> Result<(Vec<u8>, u32, u32, f32), String> {
    let source_dpi = source_dpi.max(1.0);
    if target_dpi == 0 || target_dpi as f32 >= source_dpi {
        return Ok((pixels, width, height, source_dpi));
    }
    let scale = target_dpi as f64 / source_dpi as f64;
    let output_w = ((width as f64 * scale).round() as u32).max(1);
    let output_h = ((height as f64 * scale).round() as u32).max(1);
    let source = image::RgbaImage::from_raw(width, height, pixels)
        .ok_or_else(|| "Bộ đệm trang PDF không đúng kích thước".to_string())?;
    let output = image::imageops::resize(
        &source,
        output_w,
        output_h,
        image::imageops::FilterType::Lanczos3,
    );
    Ok((output.into_raw(), output_w, output_h, target_dpi as f32))
}

/// Encode a set of page snapshots into one multi-page PDF with native vector
/// overlays, optional downsampling, press marks and an embedded ICC profile.
/// Runs on the export worker thread (no `App` access), so both the single-doc
/// page export and the "combine all tabs" export share one encoding path.
fn build_snapshots_pdf(
    snapshots: Vec<Canvas>,
    target_dpi: u32,
    marks: crate::core::print::PrintMarks,
    icc: Option<Vec<u8>>,
) -> Result<Vec<u8>, String> {
    let count = snapshots.len();
    let mut encoded = Vec::with_capacity(count);
    let mut vectors = Vec::with_capacity(count);
    for canvas in snapshots {
        let selection = crate::core::print::collect_pdf_vectors(&canvas);
        let rgba = crate::core::print::pdf_raster_base(&canvas, &selection);
        let (rgba, width, height, dpi) = prepare_pdf_pixels(
            rgba,
            canvas.width,
            canvas.height,
            canvas.metadata.resolution_ppi,
            target_dpi,
        )
        .map_err(|e| format!("Lỗi chuẩn bị trang PDF: {e}"))?;
        let page = crate::core::print::encode_pdf_page(&rgba, width, height, dpi)
            .map_err(|e| format!("Lỗi mã hoá trang PDF: {e}"))?
            .with_vector_space(canvas.width, canvas.height);
        encoded.push(page);
        vectors.push(selection.objects);
    }
    crate::core::print::build_pdf_multipage_encoded(&encoded, &vectors, marks, icc.as_deref())
        .map_err(|e| format!("Lỗi tạo PDF: {e}"))
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

        let opts = if ext == "iai" {
            ExportOptions::default()
        } else {
            ExportOptions {
                embed_icc: self.shell.ui.export_embed_icc,
                resize_long_edge: self
                    .shell
                    .ui
                    .export_resize_enabled
                    .then_some(self.shell.ui.export_resize_long_edge),
                output_sharpen: self.shell.ui.export_output_sharpen,
                ..ExportOptions::default()
            }
        };
        let canvas = detached_canvas(&self.docs.documents[idx].canvas);
        self.start_write(
            doc_id,
            canvas,
            path.to_path_buf(),
            opts,
            Some(content),
            wait,
        );
    }

    /// Write `canvas` to `path` on a worker thread. `saved_content` as in
    /// [`SaveJob`].
    fn start_write(
        &mut self,
        doc_id: DocumentId,
        canvas: Canvas,
        path: PathBuf,
        opts: ExportOptions,
        saved_content: Option<u64>,
        wait: SaveWait,
    ) {
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_path = path.clone();
        let spawned = std::thread::Builder::new()
            .name("iai-save".to_string())
            .spawn(move || {
                let _ = tx.send(write_image(&canvas, &worker_path, &opts));
            });
        if let Err(error) = spawned {
            self.report_save_error(format!("Lỗi lưu: không tạo được luồng ghi file ({error})"));
            return;
        }
        let job = SaveJob {
            doc_id,
            path,
            saved_content,
            done: rx,
        };
        match wait {
            SaveWait::Finish => self.wait_for_save_job(job),
            SaveWait::Background => {
                self.shell.status_msg = format!(
                    "Đang lưu {}…",
                    job.path.file_name().unwrap_or_default().to_string_lossy()
                );
                self.docs.save_jobs.push(job);
                if let Some(window) = &self.win.window {
                    window.request_redraw();
                }
            }
        }
    }

    fn wait_for_save_job(&mut self, job: SaveJob) {
        let result = job
            .done
            .recv()
            .unwrap_or_else(|_| Err("save worker stopped".to_string()));
        self.finish_save_job(job, result);
    }

    /// Take in the saves and exports that have finished. Called every frame.
    pub(crate) fn poll_save_jobs(&mut self) {
        let mut i = 0;
        while i < self.docs.save_jobs.len() {
            match self.docs.save_jobs[i].done.try_recv() {
                Err(TryRecvError::Empty) => i += 1,
                outcome => {
                    let job = self.docs.save_jobs.remove(i);
                    let result = outcome.unwrap_or_else(|_| Err("save worker stopped".to_string()));
                    self.finish_save_job(job, result);
                }
            }
        }
        // Nothing else wakes the loop when a worker finishes.
        if !self.docs.save_jobs.is_empty() {
            if let Some(window) = &self.win.window {
                window.request_redraw();
            }
        }
    }

    /// Stay until every save and export under way has landed. For the moments
    /// that act on the outcome: closing a tab, leaving the app.
    pub(crate) fn finish_save_jobs(&mut self) {
        for job in std::mem::take(&mut self.docs.save_jobs) {
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

    fn finish_save_job(&mut self, job: SaveJob, result: Result<PathBuf, String>) {
        let saved = match result {
            Ok(saved) => saved,
            Err(e) => {
                let what = if job.saved_content.is_some() {
                    "Lỗi lưu"
                } else {
                    "Lỗi xuất file"
                };
                self.report_save_error(format!("{what}: {e}"));
                return;
            }
        };
        let name = saved
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        self.shell.status_msg = format!("Saved: {name}");
        let Some(content) = job.saved_content else {
            return;
        };
        let Some(idx) = self.docs.index_of(job.doc_id) else {
            return;
        };
        let doc = &mut self.docs.documents[idx];
        doc.path = Some(saved.clone());
        doc.file_modified_at = file_modified_at(&saved);
        // Work done while the file was being written is not in it.
        let still_as_saved = content_fingerprint(&doc.canvas) == content;
        if still_as_saved {
            doc.mark_saved();
        }
        if idx == self.docs.active_doc_idx {
            self.docs.current_file = Some(saved);
        }
        if still_as_saved {
            self.clear_autosave(idx);
        } else {
            self.shell.status_msg =
                format!("Saved: {name} — phần sửa sau lúc bấm lưu chưa nằm trong file");
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

        let is_iai = matches!(format, crate::formats::ExportFormat::Iai);
        let doc = self.docs.active();
        // Raster export of a page that shows the shared master: build a throwaway
        // canvas with the master composited beneath and export THAT. `.iai` keeps
        // the master stored separately (via the artboard-document save), so it is
        // never merged here.
        let merged = if is_iai {
            None
        } else {
            doc.page_render_canvas(doc.active_artboard)
        };
        let canvas = merged.unwrap_or_else(|| detached_canvas(&doc.canvas));
        let doc_id = doc.id;
        let opts = ExportOptions {
            embed_icc: self.shell.ui.export_embed_icc,
            resize_long_edge: self
                .shell
                .ui
                .export_resize_enabled
                .then_some(self.shell.ui.export_resize_long_edge),
            output_sharpen: self.shell.ui.export_output_sharpen,
            pdf_marks: self.shell.ui.export_pdf_marks,
            ..ExportOptions::default()
        };
        self.start_write(doc_id, canvas, path_buf, opts, None, SaveWait::Background);
    }

    /// File ▸ Export ▸ SVG (Vector) — write the active document as an SVG: its
    /// qualifying vector objects as native `<path>` elements over an embedded PNG
    /// of the raster beneath them (a pure-vector document embeds no image). Meant
    /// for the web and cut plotters. Blocking rfd is fine here.
    pub fn export_svg(&mut self) {
        self.sync_brush_gpu_to_cpu();
        let idx = self.docs.active_doc_idx;
        let svg = match crate::core::svg::build_svg(&self.docs.documents[idx].canvas) {
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
        use crate::ui::intent::PdfExportScope;

        if self.jobs.pending_pdf_export.is_some() || self.jobs.pending_file_dialog.is_some() {
            self.shell.status_msg = "Một tác vụ xuất tệp khác đang chạy".to_string();
            return;
        }
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
        // Batch: combine every open tab into a single PDF (each tab's page(s) →
        // PDF pages, in tab order). Handled before the per-document page scope,
        // which only describes the active document.
        if self.shell.ui.pdf_export_scope == PdfExportScope::AllOpenDocuments {
            let target_dpi = self.shell.ui.pdf_export_dpi;
            self.export_all_open_documents_pdf(target_dpi);
            return;
        }
        let Some(doc) = self.docs.documents.get(idx) else {
            return;
        };
        let is_pdf = doc.pdf_document.is_some();
        let flow_text_document = doc.flow_text.as_ref().and_then(|flow| flow.document_arc());
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
            PdfExportScope::AllPages => Ok((0..page_count).collect()),
            PdfExportScope::CurrentPage => Ok(vec![current]),
            PdfExportScope::Range => {
                crate::ui::intent::parse_pdf_page_range(&self.shell.ui.pdf_export_range, page_count)
            }
            // Combining all tabs is dispatched above with an early return.
            PdfExportScope::AllOpenDocuments => {
                unreachable!("AllOpenDocuments is handled before the page-scope match")
            }
        };
        let pages = match pages {
            Ok(pages) if !pages.is_empty() => pages,
            Ok(_) => {
                self.shell.status_msg = "Không có trang nào để xuất PDF".to_string();
                self.shell.ui.show_pdf_export_dialog = true;
                return;
            }
            Err(error) => {
                self.shell.status_msg = error;
                self.shell.ui.show_pdf_export_dialog = true;
                return;
            }
        };
        let target_dpi = self.shell.ui.pdf_export_dpi;
        if let Some(document) = flow_text_document {
            self.export_flow_text_pdf_selected(document, pages);
        } else if is_pdf {
            self.export_pdf_document_selected(idx, &pages, target_dpi);
        } else {
            self.export_document_pages_pdf_selected(&pages, target_dpi);
        }
    }

    /// Export the canonical flowing-text model through the application's shared
    /// PDF job queue. The output remains vector/selectable and no canvas/GPU
    /// snapshot is created.
    fn export_flow_text_pdf_selected(
        &mut self,
        document: std::sync::Arc<crate::core::text_document::TextDocument>,
        pages: Vec<usize>,
    ) {
        let idx = self.docs.active_doc_idx;
        let stem = self
            .docs
            .documents
            .get(idx)
            .and_then(|doc| doc.path.as_deref().or(self.docs.current_file.as_deref()))
            .and_then(|path| path.file_stem())
            .and_then(|stem| stem.to_str())
            .unwrap_or("tai-lieu")
            .to_string();
        let Some(window) = self.win.window.as_ref() else {
            return;
        };
        let parent = file_io::dialog_parent(window);
        let page_total = pages.len();
        let rx = spawn_pdf_export(parent, stem, move |path| {
            let mut font_system = cosmic_text::FontSystem::new();
            let layout = crate::core::text_layout::DocumentLayout::build(
                document.as_ref(),
                96.0,
                &mut font_system,
            );
            layout.write_text_pdf_pages(&mut font_system, &path, &pages)?;
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            Ok(format!("Đã xuất PDF văn bản {page_total} trang: {name}"))
        });
        self.jobs.pending_pdf_export = Some(rx);
        self.shell.status_msg = format!("Đang xuất PDF văn bản {page_total} trang…");
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }

    /// Export selected pages of the active artboard document as one PDF, with
    /// native vector overlays, optional downsampling, ICC and press marks.
    fn export_document_pages_pdf_selected(&mut self, page_indices: &[usize], target_dpi: u32) {
        let idx = self.docs.active_doc_idx;
        let Some(doc) = self.docs.documents.get(idx) else {
            return;
        };
        let canvases = doc.all_page_canvases();
        let mut snapshots = Vec::with_capacity(page_indices.len());
        for &page_index in page_indices {
            let snapshot = if let Some(merged) = doc.page_render_canvas(page_index) {
                merged
            } else if let Some(&canvas) = canvases.get(page_index) {
                canvas.export_snapshot()
            } else {
                self.shell.status_msg = format!("Trang {} không tồn tại", page_index + 1);
                return;
            };
            snapshots.push(snapshot);
        }
        if snapshots.is_empty() {
            self.shell.status_msg = "Không có trang nào để xuất PDF".to_string();
            return;
        }

        let stem = self
            .docs
            .documents
            .get(idx)
            .and_then(|doc| doc.path.as_deref().or(self.docs.current_file.as_deref()))
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .unwrap_or("document")
            .to_string();

        let Some(window) = self.win.window.as_ref() else {
            return;
        };
        let parent = file_io::dialog_parent(window);
        let marks = self.shell.ui.export_pdf_marks;
        let icc = self
            .shell
            .ui
            .export_embed_icc
            .then(crate::core::cms::srgb_icc_bytes);
        let n = snapshots.len();
        let rx = spawn_pdf_export(parent, stem, move |path| {
            let bytes = build_snapshots_pdf(snapshots, target_dpi, marks, icc)?;
            crate::core::atomic_file::write_bytes_atomic(&path, &bytes)
                .map_err(|e| format!("Lỗi ghi PDF: {e}"))?;
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            Ok(format!("Đã xuất PDF {n} trang: {name}"))
        });
        self.jobs.pending_pdf_export = Some(rx);
        self.shell.status_msg = format!("Đang xuất PDF {n} trang…");
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }

    /// Combine every open document tab into one PDF — each tab contributes all of
    /// its page canvases, in tab order — for the "many scanned photos → one PDF"
    /// workflow. Flowing-text tabs (whose canvas is only a placeholder) are
    /// skipped. The active tab's pending edits are already committed by
    /// [`Self::run_pdf_export`] before this runs; inactive tabs are exported from
    /// their CPU-resident canvases.
    fn export_all_open_documents_pdf(&mut self, target_dpi: u32) {
        let mut snapshots: Vec<Canvas> = Vec::new();
        let mut included_docs = 0usize;
        for doc in &self.docs.documents {
            // A flowing-text tab has no raster page to place; leave it out rather
            // than inserting its 1×1 compatibility canvas as a blank page.
            if doc.is_flow_text() {
                continue;
            }
            let canvases = doc.all_page_canvases();
            let mut added_page = false;
            for page_index in 0..canvases.len() {
                let snapshot = if let Some(merged) = doc.page_render_canvas(page_index) {
                    merged
                } else if let Some(&canvas) = canvases.get(page_index) {
                    canvas.export_snapshot()
                } else {
                    continue;
                };
                snapshots.push(snapshot);
                added_page = true;
            }
            if added_page {
                included_docs += 1;
            }
        }
        if snapshots.is_empty() {
            self.shell.status_msg = "Không có tài liệu nào để gộp PDF".to_string();
            return;
        }

        // Suggest a filename from the active tab's folder/name, defaulting to a
        // neutral batch name.
        let idx = self.docs.active_doc_idx;
        let stem = self
            .docs
            .documents
            .get(idx)
            .and_then(|doc| doc.path.as_deref().or(self.docs.current_file.as_deref()))
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .unwrap_or("tai-lieu")
            .to_string();

        let Some(window) = self.win.window.as_ref() else {
            return;
        };
        let parent = file_io::dialog_parent(window);
        let marks = self.shell.ui.export_pdf_marks;
        let icc = self
            .shell
            .ui
            .export_embed_icc
            .then(crate::core::cms::srgb_icc_bytes);
        let page_total = snapshots.len();
        let doc_total = included_docs;
        let rx = spawn_pdf_export(parent, stem, move |path| {
            let bytes = build_snapshots_pdf(snapshots, target_dpi, marks, icc)?;
            crate::core::atomic_file::write_bytes_atomic(&path, &bytes)
                .map_err(|e| format!("Lỗi ghi PDF: {e}"))?;
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            Ok(format!(
                "Đã gộp {doc_total} tài liệu ({page_total} trang) thành PDF: {name}"
            ))
        });
        self.jobs.pending_pdf_export = Some(rx);
        self.shell.status_msg = format!("Đang gộp {doc_total} tài liệu thành PDF…");
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }

    fn export_pdf_document_selected(
        &mut self,
        doc_idx: usize,
        positions: &[usize],
        target_dpi: u32,
    ) -> bool {
        const PDF_EXPORT_MAX_PIXELS: u64 = 50_000_000;
        let Some(doc) = self.docs.documents.get(doc_idx) else {
            return false;
        };
        let Some(pdf) = doc.pdf_document.as_ref() else {
            return false;
        };
        let original_source = pdf.source.clone();
        let source = pdf.effective_source().to_path_buf();
        let global_edits = pdf.global_edits.clone();
        let selected_pages: Vec<usize> = positions
            .iter()
            .filter_map(|&position| pdf.selected_pages.get(position).copied())
            .collect();
        if selected_pages.len() != positions.len() || selected_pages.is_empty() {
            self.shell.status_msg = "Phạm vi trang PDF không hợp lệ".to_string();
            return true;
        }
        let all_pages_selected = selected_pages.len() == pdf.page_count
            && selected_pages.iter().copied().eq(0..pdf.page_count);
        let clean = !doc.is_modified();
        let Some(window) = self.win.window.as_ref() else {
            return true;
        };
        let parent = file_io::dialog_parent(window);
        let stem = original_source
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("document");
        let mut snapshots = Vec::with_capacity(selected_pages.len());
        for &page_index in &selected_pages {
            let mut edited = if page_index == pdf.active_page && pdf.active_page_modified {
                doc.pdf_page
                    .as_ref()
                    .map(|reference| (doc.canvas.export_snapshot(), reference.clone()))
            } else {
                pdf.edited_pages
                    .get(&page_index)
                    .map(|page| (page.canvas.export_snapshot(), page.reference.clone()))
            };
            if let Some((canvas, _)) = edited.as_mut() {
                if let Some(overlay) = crate::core::document::build_pdf_global_overlay_stack(
                    &global_edits,
                    page_index,
                    canvas.width,
                    canvas.height,
                ) {
                    canvas.layer_stack = canvas.layer_stack.with_overlay(&overlay);
                }
            }
            snapshots.push((page_index, edited));
        }
        let baked_global_pages: Vec<usize> = snapshots
            .iter()
            .filter_map(|(page_index, edited)| edited.is_some().then_some(*page_index))
            .collect();
        let stem = stem.to_string();
        let page_total = snapshots.len();
        let rx = spawn_pdf_export(parent, stem, move |path| {
            if clean && all_pages_selected && global_edits.is_empty() {
                let bytes = std::fs::read(&source).map_err(|e| format!("Lỗi đọc PDF gốc: {e}"))?;
                crate::core::atomic_file::write_bytes_atomic(&path, &bytes)
                    .map_err(|e| format!("Lỗi sao chép PDF gốc: {e}"))?;
                return Ok("Đã xuất PDF vector gốc".to_string());
            }

            let compatibility =
                crate::formats::pdf::hybrid_overlay_compatibility(&source).unwrap_or_default();
            let make_pages = |allow_overlay: bool| {
                let mut pages = Vec::with_capacity(snapshots.len());
                for (page_index, edited) in &snapshots {
                    let content = if let Some((canvas, reference)) = edited {
                        let overlay = (allow_overlay
                            && compatibility.get(*page_index).copied().unwrap_or(false))
                        .then(|| reference.safe_overlay_pdf_parts(canvas))
                        .flatten();
                        if let Some((rgba, vectors)) = overlay {
                            crate::formats::pdf::HybridPageContent::Overlay {
                                rgba,
                                vectors,
                                width: canvas.width,
                                height: canvas.height,
                                dpi: canvas.metadata.resolution_ppi,
                            }
                        } else {
                            let rgba = canvas.export_flat_up_to(PDF_EXPORT_MAX_PIXELS).ok_or_else(
                                || format!("Trang PDF {} quá lớn để xuất an toàn", page_index + 1),
                            )?;
                            let (rgba, width, height, dpi) = prepare_pdf_pixels(
                                rgba,
                                canvas.width,
                                canvas.height,
                                canvas.metadata.resolution_ppi,
                                target_dpi,
                            )?;
                            crate::formats::pdf::HybridPageContent::Raster {
                                rgba,
                                width,
                                height,
                                dpi,
                            }
                        }
                    } else {
                        crate::formats::pdf::HybridPageContent::Original
                    };
                    pages.push(crate::formats::pdf::HybridPage {
                        source_index: *page_index,
                        content,
                    });
                }
                Ok::<_, String>(pages)
            };
            let hybrid = make_pages(true)
                .and_then(|pages| {
                    crate::formats::pdf::build_hybrid_pdf_with_global_edits_baked(
                        &source,
                        &pages,
                        &global_edits,
                        &baked_global_pages,
                    )
                })
                .or_else(|_| {
                    make_pages(false).and_then(|pages| {
                        crate::formats::pdf::build_hybrid_pdf_with_global_edits_baked(
                            &source,
                            &pages,
                            &global_edits,
                            &baked_global_pages,
                        )
                    })
                })
                .map_err(|e| format!("Lỗi tạo PDF: {e}"))?;
            crate::core::atomic_file::write_bytes_atomic(&path, &hybrid.bytes)
                .map_err(|e| format!("Lỗi ghi PDF: {e}"))?;
            Ok(format!(
                "Đã xuất PDF hybrid ({} vector, {} overlay, {} raster)",
                hybrid.vector_pages, hybrid.overlay_pages, hybrid.raster_pages
            ))
        });
        self.jobs.pending_pdf_export = Some(rx);
        self.shell.status_msg = format!("Đang xuất PDF {page_total} trang…");
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
        true
    }

    /// Drain the background PDF worker without ever blocking the UI thread.
    pub(crate) fn poll_pdf_export(&mut self) {
        let result = match self.jobs.pending_pdf_export.as_ref() {
            Some(rx) => rx.try_recv(),
            None => return,
        };
        match result {
            Ok(result) => {
                self.jobs.pending_pdf_export = None;
                match result {
                    None => self.shell.status_msg = "Đã hủy xuất PDF".to_string(),
                    Some(Ok(message)) => self.shell.status_msg = message,
                    Some(Err(error)) => self.report_save_error(error),
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                if let Some(window) = &self.win.window {
                    window.request_redraw();
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.jobs.pending_pdf_export = None;
                self.shell.status_msg = "Tác vụ xuất PDF dừng ngoài dự kiến".to_string();
            }
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
    use super::{prepare_pdf_pixels, requires_project_save_on_close, App, SaveWait};
    use crate::core::canvas::Canvas;

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
        let missing_folder =
            std::env::temp_dir().join(format!("iai_save_error_{}_absent", std::process::id()));

        for name in ["anh.png", "anh.iai"] {
            app.shell.ui.save_error = None;
            app.save_to(&missing_folder.join(name));

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

    #[test]
    fn pdf_dpi_never_upscales() {
        let pixels = vec![255; 20 * 10 * 4];
        let (_, width, height, dpi) = prepare_pdf_pixels(pixels, 20, 10, 150.0, 300).unwrap();
        assert_eq!((width, height, dpi), (20, 10, 150.0));
    }
}

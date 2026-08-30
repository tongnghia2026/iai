//! Multi-page PDF sessions: probe/import pipeline, page renders and
//! switches, and the .iai PDF-project save/install path.

use super::file_name;
use crate::app::state::App;
use crate::core::document::file_modified_at;
use std::path::{Path, PathBuf};

pub(in crate::app) fn materialize_embedded_pdf(
    id: crate::core::document::DocumentId,
    bytes: &[u8],
) -> Result<PathBuf, String> {
    let dir = crate::app::autosave::pdf_cache_dir()
        .ok_or_else(|| "No app data directory for PDF cache".to_string())?;
    let path = dir.join(format!("embed_{}_{}.pdf", std::process::id(), id.0));
    std::fs::write(&path, bytes).map_err(|e| format!("write embedded PDF cache: {e}"))?;
    Ok(path)
}

/// A fresh [`PdfPageRef`](crate::core::document::PdfPageRef) for a loaded page.
/// The baseline (`original_*`) fields are filled by `record_canvas_baseline`
/// once a canvas is available.
pub(in crate::app) fn fresh_pdf_page_ref(
    group_id: u32,
    source: &Path,
    index: usize,
    count: usize,
    dpi: f32,
) -> crate::core::document::PdfPageRef {
    crate::core::document::PdfPageRef {
        group_id,
        source: source.to_path_buf(),
        index,
        count,
        requested_dpi: dpi,
        loaded: true,
        original_width: 0,
        original_height: 0,
        original_dpi: dpi,
        original_layer_id: 0,
        original_tiles_fingerprint: 0,
    }
}

impl App {
    /// Write the active document's multi-page PDF session as a full `.iai`
    /// project: the link + metadata of the source PDF plus every edited page's
    /// layers. Clean pages are omitted (re-rendered from the source on open).
    /// The write is atomic (temp file + rename).
    pub fn save_pdf_project_to(&mut self, path: &std::path::Path) {
        if self.edit.text_edit.is_some() {
            self.commit_text_edit();
        }
        self.sync_brush_gpu_to_cpu();
        let idx = self.docs.active_doc_idx;
        // Latch the sticky "page has been edited" flag before gathering pages;
        // dirty itself is derived from each page canvas's checkpoint.
        self.docs.documents[idx].reconcile_pdf_page_modified();

        match self.write_pdf_project(idx, path) {
            Ok(edited_count) => {
                let doc = &mut self.docs.documents[idx];
                doc.path = Some(path.to_path_buf());
                doc.file_modified_at = file_modified_at(path);
                doc.mark_saved();
                self.docs.current_file = Some(path.to_path_buf());
                self.clear_autosave(idx);
                self.shell.status_msg = format!(
                    "Saved project: {} ({edited_count} edited page(s))",
                    path.file_name().unwrap_or_default().to_string_lossy()
                );
            }
            Err(e) => {
                self.shell.status_msg = format!("Error saving project: {e}");
            }
        }
    }

    /// Serialize the document at `idx`'s PDF session to `path` (atomic) without
    /// mutating any document state. Returns the number of edited pages written.
    /// Shared by the explicit project save and by autosave.
    pub(crate) fn write_pdf_project(
        &self,
        idx: usize,
        path: &std::path::Path,
    ) -> Result<usize, String> {
        let doc = self
            .docs
            .documents
            .get(idx)
            .ok_or_else(|| "No document to save".to_string())?;
        let pdf = doc
            .pdf_document
            .as_ref()
            .ok_or_else(|| "Not a PDF project".to_string())?;

        let (source_len, source_modified_secs) = match std::fs::metadata(&pdf.source) {
            Ok(m) => (
                Some(m.len()),
                m.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs()),
            ),
            Err(_) => (None, None),
        };
        let meta = crate::formats::iai::IaiProjectMeta {
            source: pdf.source.clone(),
            source_len,
            source_modified_secs,
            page_count: pdf.page_count,
            selected_pages: pdf.selected_pages.clone(),
            page_names: pdf.page_names.clone(),
            requested_dpi: pdf.requested_dpi,
            active_page: pdf.active_page,
            global_clears: pdf.global_clears.clone(),
        };
        let source_pdf = std::fs::read(pdf.effective_source()).ok();

        let mut pages: Vec<crate::formats::iai::IaiProjectPageOut> = Vec::new();
        for (index, cached) in pdf.edited_pages.iter() {
            // The active page lives in doc.canvas and is written below; skip any
            // stale cache entry for it so a page index is never written twice.
            if *index == pdf.active_page {
                continue;
            }
            pages.push(crate::formats::iai::IaiProjectPageOut {
                index: *index,
                base_pristine: cached.reference.base_is_pristine(&cached.canvas),
                view: (
                    cached.saved_zoom,
                    cached.saved_offset_x,
                    cached.saved_offset_y,
                ),
                canvas: &cached.canvas,
            });
        }
        // The active page lives in doc.canvas; include it when it has edits. The
        // live `is_modified` catches edits not yet folded into active_page_modified.
        if pdf.active_page_modified || doc.canvas.is_dirty() {
            if let Some(reference) = doc.pdf_page.as_ref() {
                pages.push(crate::formats::iai::IaiProjectPageOut {
                    index: pdf.active_page,
                    base_pristine: reference.base_is_pristine(&doc.canvas),
                    view: (
                        self.edit.view.zoom,
                        self.edit.view.offset_x,
                        self.edit.view.offset_y,
                    ),
                    canvas: &doc.canvas,
                });
            }
        }
        pages.sort_by_key(|page| page.index);

        crate::formats::iai::save_pdf_project(path, &meta, &pages, source_pdf.as_deref())
            .map(|()| pages.len())
    }

    /// Start probing the next queued PDF, if one is queued and nothing is already
    /// probing or waiting on the dialog. Probing (parse + page sizes, no raster)
    /// runs on a worker thread; `poll_pdf_probe` then opens the dialog.
    pub fn maybe_start_next_pdf_probe(&mut self) {
        if self.jobs.pending_pdf_probe.is_some() || self.jobs.pending_pdf_prompt.is_some() {
            return;
        }
        let Some(path) = self.jobs.pending_pdf_probe_queue.pop_front() else {
            return;
        };
        let name = file_name(&path);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::formats::pdf::PdfImporter::probe(&path)
            }))
            .unwrap_or_else(|_| Err("PDF decoder panicked".to_string()));
            let _ = tx.send((path, result));
        });
        self.jobs.pending_pdf_probe = Some(rx);
        self.shell.status_msg = format!("Reading PDF: {name}…");
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Drain a finished PDF probe and raise the page-selection dialog.
    pub fn poll_pdf_probe(&mut self) {
        let result = match self.jobs.pending_pdf_probe.as_ref() {
            Some(rx) => rx.try_recv(),
            None => return,
        };
        match result {
            Ok((path, Ok(probe))) => {
                self.jobs.pending_pdf_probe = None;
                let name = file_name(&path);
                self.jobs.pending_pdf_prompt = Some(crate::app::state::PdfImportPrompt {
                    path,
                    page_count: probe.page_count,
                    page_dims: probe.page_dims,
                });
                self.shell.status_msg = format!("Select pages to open: {name}");
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            Ok((path, Err(e))) => {
                self.jobs.pending_pdf_probe = None;
                self.shell.status_msg = format!("Could not open {}: {e}", file_name(&path));
                self.maybe_start_next_pdf_probe();
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.jobs.pending_pdf_probe = None;
                self.maybe_start_next_pdf_probe();
            }
        }
    }

    /// The user confirmed the dialog: render the selected pages on a worker thread.
    /// `pages` are 0-based; they're normalized to ascending, unique, in-range.
    /// `target_dpi` is the dialog's resolution choice (`None` = Auto).
    pub fn confirm_pdf_import(&mut self, mut pages: Vec<usize>, target_dpi: Option<f32>) {
        let Some(prompt) = self.jobs.pending_pdf_prompt.take() else {
            return;
        };
        pages.sort_unstable();
        pages.dedup();
        pages.retain(|&p| p < prompt.page_count);
        if pages.is_empty() {
            self.shell.status_msg = "No pages selected".to_string();
            self.maybe_start_next_pdf_probe();
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return;
        }
        let path = prompt.path;
        let name = file_name(&path);
        let n = pages.len();
        let first_page = pages[0];
        self.jobs.pdf_render_total_pages = prompt.page_count;
        self.jobs.pdf_render_group_id = self.docs.next_pdf_group_id;
        self.docs.next_pdf_group_id += 1;
        self.jobs.pdf_render_source = path.clone();
        self.jobs.pdf_render_selected_pages = pages;
        self.jobs.pdf_render_target_dpi = target_dpi.unwrap_or(300.0);
        self.jobs.load_activate_pending = true;
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_path = path.clone();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::formats::pdf::PdfImporter::render_selected(
                    &worker_path,
                    &[first_page],
                    target_dpi,
                )
                .map(|canvases| {
                    canvases
                        .into_iter()
                        .map(|canvas| (first_page, canvas))
                        .collect::<Vec<_>>()
                })
            }))
            .unwrap_or_else(|_| Err("PDF decoder panicked".to_string()));
            let _ = tx.send((worker_path, result));
        });
        self.jobs.pending_pdf_render = Some(rx);
        let dpi_label = match target_dpi {
            Some(dpi) => format!("{} DPI", dpi as u32),
            None => "Auto".to_string(),
        };
        self.shell.status_msg =
            format!("Opening {n} pages from {name} ({dpi_label}); rendering first page…");
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// The user dismissed the page-selection dialog without importing.
    pub fn cancel_pdf_import(&mut self) {
        if self.jobs.pending_pdf_prompt.take().is_some() {
            self.shell.status_msg = "PDF import cancelled".to_string();
        }
        self.maybe_start_next_pdf_probe();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Attach the first rendered page as one PDF document. Other selected pages
    /// remain virtual until navigation requests them.
    pub fn poll_pdf_render(&mut self) {
        let result = match self.jobs.pending_pdf_render.as_ref() {
            Some(rx) => rx.try_recv(),
            None => return,
        };
        match result {
            Ok((path, Ok(pages))) => {
                self.jobs.pending_pdf_render = None;
                let total = self.jobs.pdf_render_total_pages;
                let group_id = self.jobs.pdf_render_group_id;
                let source = self.jobs.pdf_render_source.clone();
                let selected = std::mem::take(&mut self.jobs.pdf_render_selected_pages);
                let requested_dpi = self.jobs.pdf_render_target_dpi;
                let Some((page_index, canvas)) = pages.into_iter().next() else {
                    self.shell.status_msg = "PDF renderer returned no page".to_string();
                    self.maybe_start_next_pdf_probe();
                    return;
                };
                let actual_dpi = canvas.metadata.resolution_ppi;
                let mut pdf_page = crate::core::document::PdfPageRef {
                    group_id,
                    source: source.clone(),
                    index: page_index,
                    count: total,
                    requested_dpi,
                    loaded: true,
                    original_width: 0,
                    original_height: 0,
                    original_dpi: requested_dpi,
                    original_layer_id: 0,
                    original_tiles_fingerprint: 0,
                };
                pdf_page.record_canvas_baseline(&canvas);
                let page = (total > 1).then_some((page_index, total));
                self.attach_loaded_doc(path.clone(), canvas, page, Some(pdf_page));

                let doc = &mut self.docs.documents[self.docs.active_doc_idx];
                let document_id = doc.id;
                doc.pdf_document = Some(crate::core::document::PdfDocumentState {
                    source: source.clone(),
                    embedded_source: None,
                    page_count: total,
                    selected_pages_saved: selected.clone(),
                    selected_pages: selected,
                    page_names: std::collections::BTreeMap::new(),
                    page_names_saved: std::collections::BTreeMap::new(),
                    requested_dpi,
                    active_page: page_index,
                    active_page_modified: false,
                    edited_pages: std::collections::HashMap::new(),
                    global_clears: Vec::new(),
                    global_clears_saved: Vec::new(),
                    global_clears_redo: Vec::new(),
                    global_overlay_cache: None,
                });
                doc.rebuild_pdf_global_overlay();
                self.docs.pdf_render_services.insert(
                    document_id,
                    crate::formats::pdf::PdfRenderService::start(source),
                );
                self.shell.status_msg =
                    format!("Opened {total}-page PDF at {actual_dpi:.0} DPI in one document");
                self.maybe_start_next_pdf_probe();
            }
            Ok((path, Err(e))) => {
                self.jobs.pending_pdf_render = None;
                self.jobs.pdf_render_selected_pages.clear();
                self.shell.status_msg = format!("Error rendering {}: {e}", file_name(&path));
                self.maybe_start_next_pdf_probe();
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.jobs.pending_pdf_render = None;
                self.maybe_start_next_pdf_probe();
            }
        }
    }

    pub fn begin_pdf_page_insert_dialog(&mut self, position: usize) {
        if self.jobs.pending_file_dialog.is_some() || self.jobs.pending_pdf_page_insert.is_some() {
            self.shell.status_msg = "Một tác vụ chèn trang khác đang chạy".to_string();
            return;
        }
        let Some(doc) = self.docs.documents.get(self.docs.active_doc_idx) else {
            return;
        };
        let Some(pdf) = doc.pdf_document.as_ref() else {
            return;
        };
        let document_id = doc.id;
        let position = position.min(pdf.selected_pages.len());
        let Some(window) = self.win.window.as_ref() else {
            return;
        };
        let parent = crate::file_io::dialog_parent(window);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Some(paths) = crate::file_io::dialog_insert_pdf_pages(parent) {
                if !paths.is_empty() {
                    let _ = tx.send(crate::file_io::FileDialogResult::InsertPdfPages {
                        document_id,
                        position,
                        paths,
                    });
                }
            }
        });
        self.jobs.pending_file_dialog = Some(rx);
    }

    pub(crate) fn start_pdf_page_insert(
        &mut self,
        document_id: crate::core::document::DocumentId,
        position: usize,
        paths: Vec<PathBuf>,
    ) {
        if self.jobs.pending_pdf_page_insert.is_some() {
            self.shell.status_msg = "Một tác vụ chèn trang khác đang chạy".to_string();
            return;
        }
        let Some(doc) = self.docs.documents.iter().find(|doc| doc.id == document_id) else {
            return;
        };
        let Some(pdf) = doc.pdf_document.as_ref() else {
            return;
        };
        let dpi = pdf.requested_dpi;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = (|| {
                let registry = crate::formats::FormatRegistry::new();
                let mut inserted = Vec::new();
                for path in paths {
                    let stem = path
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .unwrap_or("Trang chèn")
                        .to_string();
                    let canvases = if crate::formats::pdf::is_pdf_path(&path) {
                        let probe = crate::formats::pdf::PdfImporter::probe(&path)?;
                        let pages: Vec<usize> = (0..probe.page_count).collect();
                        crate::formats::pdf::PdfImporter::render_selected(&path, &pages, Some(dpi))?
                    } else {
                        super::open::import_many_guarded(&registry, &path)?
                    };
                    let many = canvases.len() > 1;
                    for (index, canvas) in canvases.into_iter().enumerate() {
                        let label = if many {
                            format!("{stem} · {}", index + 1)
                        } else {
                            stem.clone()
                        };
                        inserted.push((canvas, label));
                    }
                }
                (!inserted.is_empty())
                    .then_some(inserted)
                    .ok_or_else(|| "Không có trang hợp lệ để chèn".to_string())
            })();
            let _ = tx.send((document_id, position, result));
        });
        self.jobs.pending_pdf_page_insert = Some(rx);
        self.shell.status_msg = "Đang nhập ảnh/PDF để chèn trang…".to_string();
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }

    /// Insert files dropped from the OS onto a PDF document as pages right after
    /// the current page. Batched: winit reports one drop event per file, so the
    /// whole selection lands as one contiguous block in drop order.
    pub fn flush_dropped_pdf_page_files(&mut self) {
        if self.jobs.dropped_pdf_page_files.is_empty() {
            return;
        }
        // Wait for any in-flight insert/dialog so the batch stays one block.
        if self.jobs.pending_pdf_page_insert.is_some() || self.jobs.pending_file_dialog.is_some() {
            return;
        }
        let paths = std::mem::take(&mut self.jobs.dropped_pdf_page_files);
        let Some(doc) = self.docs.documents.get(self.docs.active_doc_idx) else {
            return;
        };
        let Some(pdf) = doc.pdf_document.as_ref() else {
            return;
        };
        let document_id = doc.id;
        let after = pdf
            .selected_pages
            .iter()
            .position(|&page| page == pdf.active_page)
            .map_or(pdf.selected_pages.len(), |pos| pos + 1);
        self.start_pdf_page_insert(document_id, after, paths);
    }

    pub fn poll_pdf_page_insert(&mut self) {
        let result = match self.jobs.pending_pdf_page_insert.as_ref() {
            Some(rx) => rx.try_recv(),
            None => return,
        };
        match result {
            Ok((document_id, position, Ok(pages))) => {
                self.jobs.pending_pdf_page_insert = None;
                self.insert_materialized_pdf_pages(document_id, position, pages);
            }
            Ok((_document_id, _position, Err(error))) => {
                self.jobs.pending_pdf_page_insert = None;
                self.shell.status_msg = format!("Không thể chèn trang: {error}");
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                if let Some(window) = &self.win.window {
                    window.request_redraw();
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.jobs.pending_pdf_page_insert = None;
                self.shell.status_msg = "Tác vụ chèn trang dừng ngoài dự kiến".to_string();
            }
        }
    }

    pub fn insert_blank_pdf_page(&mut self, position: usize) {
        let doc_idx = self.docs.active_doc_idx;
        let Some(doc) = self.docs.documents.get(doc_idx) else {
            return;
        };
        if doc.pdf_document.is_none() {
            return;
        }
        let Some(len) =
            crate::core::canvas::Canvas::checked_rgba_len(doc.canvas.width, doc.canvas.height)
        else {
            self.shell.status_msg = "Trang hiện tại quá lớn để tạo trang trắng".to_string();
            return;
        };
        let mut canvas = crate::core::canvas::Canvas::from_rgba(
            vec![255; len],
            doc.canvas.width,
            doc.canvas.height,
        );
        canvas.metadata.resolution_ppi = doc.canvas.metadata.resolution_ppi;
        canvas.icc_profile = doc.canvas.icc_profile.clone();
        let document_id = doc.id;
        self.insert_materialized_pdf_pages(
            document_id,
            position,
            vec![(canvas, "Trang trắng".to_string())],
        );
    }

    fn insert_materialized_pdf_pages(
        &mut self,
        document_id: crate::core::document::DocumentId,
        position: usize,
        pages: Vec<(crate::core::canvas::Canvas, String)>,
    ) {
        let Some(doc_idx) = self
            .docs
            .documents
            .iter()
            .position(|doc| doc.id == document_id)
        else {
            return;
        };
        let activate = doc_idx == self.docs.active_doc_idx;
        if activate {
            self.sync_brush_gpu_to_cpu();
        }
        let group_id = self.docs.documents[doc_idx]
            .pdf_page
            .as_ref()
            .map_or(0, |page| page.group_id);
        let Some(pdf) = self.docs.documents[doc_idx].pdf_document.as_mut() else {
            return;
        };
        let position = position.min(pdf.selected_pages.len());
        let mut inserted_count = 0usize;
        for (offset, (mut canvas, label)) in pages.into_iter().enumerate() {
            let Some(page_id) = pdf.allocate_inserted_page_id() else {
                self.shell.status_msg = "Đã đạt giới hạn số trang có thể chèn".to_string();
                break;
            };
            canvas.mark_saved();
            let mut reference = fresh_pdf_page_ref(
                group_id,
                &pdf.source,
                page_id,
                pdf.page_count,
                canvas.metadata.resolution_ppi,
            );
            reference.record_canvas_baseline(&canvas);
            reference.mark_base_dirty();
            pdf.edited_pages.insert(
                page_id,
                crate::core::document::PdfCachedPage {
                    canvas,
                    reference,
                    saved_zoom: 0.0,
                    saved_offset_x: 0.0,
                    saved_offset_y: 0.0,
                },
            );
            pdf.selected_pages.insert(position + offset, page_id);
            if !label.trim().is_empty() {
                pdf.page_names.insert(page_id, label);
            }
            inserted_count += 1;
        }
        let total = pdf.selected_pages.len();
        if activate && inserted_count > 0 {
            self.pdf_nav_goto(position);
        }
        self.shell.status_msg = format!("Đã chèn {inserted_count} trang · tổng {total}");
    }

    /// Start rendering a lazy PDF page. The current page remains visible
    /// until this finishes; `poll_pdf_page_render` then switches atomically.
    pub(crate) fn request_pdf_page_switch(&mut self, doc_idx: usize, page_index: usize) {
        if self.jobs.pending_pdf_page_render.is_some() {
            self.shell.status_msg = "A PDF page is already rendering".to_string();
            return;
        }
        let Some(doc) = self.docs.documents.get(doc_idx) else {
            return;
        };
        let Some(pdf) = doc.pdf_document.as_ref() else {
            return;
        };
        let doc_id = doc.id;
        let dpi = pdf.requested_dpi;
        let Some(service) = self.docs.pdf_render_services.get(&doc_id) else {
            self.shell.status_msg = "PDF render service is unavailable".to_string();
            return;
        };
        if let Err(error) = service.request(doc_id, page_index, dpi) {
            self.shell.status_msg = error;
            return;
        }
        self.jobs.pending_pdf_page_render = Some((doc_id, page_index));
        self.shell.status_msg = format!("Rendering PDF page {} at {dpi:.0} DPI…", page_index + 1);
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }

    /// Complete an on-demand page render and activate it. The previous clean
    /// page is dropped, keeping full-resolution raster memory bounded.
    pub fn poll_pdf_page_render(&mut self) {
        let (pending_doc, pending_page) = match self.jobs.pending_pdf_page_render {
            Some(pending) => pending,
            None => return,
        };
        let result = match self.docs.pdf_render_services.get(&pending_doc) {
            Some(service) => service.try_recv(),
            None => {
                self.jobs.pending_pdf_page_render = None;
                self.shell.status_msg = "PDF render service is unavailable".to_string();
                return;
            }
        };
        match result {
            Ok(result) => {
                self.jobs.pending_pdf_page_render = None;
                if result.document_id != pending_doc || result.page_index != pending_page {
                    self.shell.status_msg = "PDF renderer returned an unexpected page".to_string();
                    return;
                }
                match result.canvas {
                    Ok(canvas) => {
                        let dpi = canvas.metadata.resolution_ppi;
                        let Some(idx) = self
                            .docs
                            .documents
                            .iter()
                            .position(|doc| doc.id == result.document_id)
                        else {
                            return;
                        };
                        let still_selected = self.docs.documents[idx]
                            .pdf_document
                            .as_ref()
                            .is_some_and(|pdf| pdf.selected_pages.contains(&result.page_index));
                        if !still_selected {
                            self.shell.status_msg =
                                "Đã bỏ kết quả render của trang vừa xóa".to_string();
                            return;
                        }
                        self.install_rendered_pdf_page(idx, result.page_index, canvas);
                        self.shell.status_msg = format!(
                            "Rendered PDF page {} at {dpi:.0} DPI",
                            result.page_index + 1
                        );
                        if let Some((doc_id, source_index)) =
                            self.jobs.pending_pdf_page_delete.take()
                        {
                            if doc_id == result.document_id {
                                self.finish_pdf_page_delete(doc_id, source_index);
                            }
                        }
                    }
                    Err(error) => {
                        self.jobs.pending_pdf_page_delete = None;
                        self.shell.status_msg = format!("Error rendering PDF page: {error}");
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                if let Some(window) = &self.win.window {
                    window.request_redraw();
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.jobs.pending_pdf_page_render = None;
                self.jobs.pending_pdf_page_delete = None;
                self.shell.status_msg = "PDF page renderer stopped unexpectedly".to_string();
            }
        }
    }

    /// Rebuild a multi-page PDF session from a decoded project and open it as the
    /// active tab. The active page comes from the stored edits when present, else
    /// it is rendered clean from the source; a per-document render service is
    /// started so clean pages can be paged to on demand.
    pub(in crate::app) fn install_pdf_project(
        &mut self,
        path: PathBuf,
        project: crate::formats::iai::IaiPdfProject,
    ) {
        use crate::core::document::{Document, DocumentId, PdfCachedPage, PdfDocumentState};

        let source = project.source.clone();
        let page_count = project.page_count.max(1);
        let requested_dpi = project.requested_dpi;
        let stored_page_indices: std::collections::HashSet<usize> =
            project.pages.iter().map(|page| page.index).collect();
        let mut selected_pages = project.selected_pages.clone();
        selected_pages.retain(|index| *index < page_count || stored_page_indices.contains(index));
        let mut seen_pages = std::collections::HashSet::new();
        selected_pages.retain(|index| seen_pages.insert(*index));
        if selected_pages.is_empty() {
            selected_pages = (0..page_count).collect();
        }
        let active_page = selected_pages
            .contains(&project.active_page)
            .then_some(project.active_page)
            .unwrap_or(selected_pages[0]);

        let id = DocumentId(self.docs.next_doc_id);
        self.docs.next_doc_id += 1;
        let group_id = self.docs.next_pdf_group_id;
        self.docs.next_pdf_group_id += 1;
        let embedded_source = project.embedded_pdf.as_deref().and_then(|bytes| {
            match materialize_embedded_pdf(id, bytes) {
                Ok(path) => Some(path),
                Err(err) => {
                    self.shell.status_msg = format!("Could not cache embedded PDF: {err}");
                    None
                }
            }
        });
        let effective_source = embedded_source.clone().unwrap_or_else(|| source.clone());

        let mut edited: std::collections::HashMap<usize, PdfCachedPage> =
            std::collections::HashMap::new();
        let mut active_from_stored: Option<(
            crate::core::canvas::Canvas,
            crate::core::document::PdfPageRef,
            (f32, f32, f32),
        )> = None;
        for page in project.pages {
            let mut reference =
                fresh_pdf_page_ref(group_id, &source, page.index, page_count, requested_dpi);
            reference.record_canvas_baseline(&page.canvas);
            if !page.base_pristine {
                reference.mark_base_dirty();
            }
            if page.index == active_page {
                active_from_stored = Some((page.canvas, reference, page.view));
            } else {
                edited.insert(
                    page.index,
                    PdfCachedPage {
                        canvas: page.canvas,
                        reference,
                        saved_zoom: page.view.0,
                        saved_offset_x: page.view.1,
                        saved_offset_y: page.view.2,
                    },
                );
            }
        }

        // Resolve the active page: a stored edit, a fresh clean render, or (source
        // missing) fall back to a stored page so the project still opens.
        let (active_canvas, active_ref, active_view, active_differs, active_index) =
            if let Some((canvas, reference, view)) = active_from_stored {
                (canvas, reference, view, true, active_page)
            } else {
                match crate::formats::pdf::PdfImporter::render_selected(
                    &effective_source,
                    &[active_page],
                    Some(requested_dpi),
                )
                .and_then(|mut pages| {
                    pages
                        .pop()
                        .ok_or_else(|| "PDF active page did not render".to_string())
                }) {
                    Ok(canvas) => {
                        let mut reference = fresh_pdf_page_ref(
                            group_id,
                            &source,
                            active_page,
                            page_count,
                            requested_dpi,
                        );
                        reference.record_canvas_baseline(&canvas);
                        (canvas, reference, (0.0, 0.0, 0.0), false, active_page)
                    }
                    Err(err) => {
                        let fallback = edited.keys().copied().min();
                        if let Some(idx) = fallback {
                            let cached = edited.remove(&idx).expect("key just found");
                            (
                                cached.canvas,
                                cached.reference,
                                (
                                    cached.saved_zoom,
                                    cached.saved_offset_x,
                                    cached.saved_offset_y,
                                ),
                                true,
                                idx,
                            )
                        } else {
                            if let Some(path) = embedded_source.as_ref() {
                                let _ = std::fs::remove_file(path);
                            }
                            self.shell.status_msg = format!("Could not open project: {err}");
                            return;
                        }
                    }
                }
            };

        for page in edited.values_mut() {
            page.canvas.mark_saved();
        }
        let mut doc = Document::from_canvas(id, active_canvas, Some(path.clone()));
        doc.pdf_page = Some(active_ref);
        doc.saved_zoom = active_view.0;
        doc.saved_offset_x = active_view.1;
        doc.saved_offset_y = active_view.2;
        let stem = source
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("PDF")
            .to_string();
        doc.title = format!("{stem} - Page {}", active_index + 1);
        doc.pdf_document = Some(PdfDocumentState {
            source: source.clone(),
            embedded_source: embedded_source.clone(),
            page_count,
            selected_pages_saved: selected_pages.clone(),
            selected_pages,
            page_names: project.page_names.clone(),
            page_names_saved: project.page_names,
            requested_dpi,
            active_page: active_index,
            active_page_modified: active_differs,
            edited_pages: edited,
            global_clears: project.global_clears.clone(),
            global_clears_saved: project.global_clears,
            global_clears_redo: Vec::new(),
            global_overlay_cache: None,
        });
        doc.rebuild_pdf_global_overlay();
        // Everything just came off disk — this state IS the file.
        doc.mark_saved();

        // Activate as a tab (mirrors attach_loaded_doc's activate path).
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
        self.jobs.load_activate_pending = false;

        self.docs.pdf_render_services.insert(
            id,
            crate::formats::pdf::PdfRenderService::start(effective_source),
        );
        if let Some(path) = embedded_source {
            self.docs.embedded_pdf_files.insert(id, path);
        }

        self.refresh_active_document();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        self.shell.status_msg = format!("Opened project: {name} ({page_count} pages)");
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Open a project decoded from a crash-recovery autosave file. Loads it, then
    /// points the document back at its original `.iai` (when known), flags it
    /// unsaved, and adopts the leftover autosave file so recovery has no gap.
    pub(crate) fn install_pdf_project_recovered(
        &mut self,
        autosave_path: PathBuf,
        project: crate::formats::iai::IaiPdfProject,
        project_path: Option<PathBuf>,
    ) {
        self.install_pdf_project(autosave_path.clone(), project);
        let idx = self.docs.active_doc_idx;
        let doc_id = self.docs.documents[idx].id;
        let doc = &mut self.docs.documents[idx];
        doc.path = project_path.clone();
        doc.file_modified_at = project_path
            .as_deref()
            .and_then(crate::core::document::file_modified_at);
        // Recovered work exists only in the autosave, with no command behind it:
        // there is no checkpoint that proves it matches a file, so latch dirty.
        doc.canvas.mark_dirty_unconditionally();
        doc.reconcile_pdf_page_modified();
        self.docs.current_file = project_path;
        // Keep updating the recovered file until the user saves for real.
        self.docs.autosave_files.insert(doc_id, autosave_path);
    }
}

//! Saving and exporting the active document: PNG/JPEG/TIFF/.iai writes,
//! CMYK separations and multi-page PDF export.

use crate::app::state::App;
use crate::core::canvas::Canvas;
use crate::core::document::file_modified_at;
use crate::file_io;
use crate::formats::ExportOptions;
use std::path::Path;

fn requires_project_save_on_close(layer_count: usize, is_pdf_document: bool) -> bool {
    is_pdf_document || layer_count > 1
}

impl App {
    /// Save requested by an exit/close confirmation. A document with multiple
    /// layers defaults to an editable `.iai` project. A flat, single-layer
    /// document keeps the regular Save behavior and may overwrite its source
    /// PNG/JPEG/etc. An existing `.iai` project is always updated in place.
    pub fn do_save_project(&mut self) {
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
            self.do_save();
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

    pub fn do_save(&mut self) {
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
        if let Some(path) = self.docs.current_file.clone() {
            self.save_to(&path);
        } else {
            self.do_save_as();
        }
    }

    pub fn do_save_as(&mut self) {
        self.do_save_as_with_suggestion(self.docs.current_file.clone());
    }

    fn do_save_project_as(&mut self) {
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

    pub fn save_to(&mut self, path: &std::path::Path) {
        if self.edit.text_edit.is_some() {
            self.commit_text_edit();
        }
        self.sync_brush_gpu_to_cpu();

        let is_large = self.win.gpu.as_ref().map_or(false, |g| g.is_large_canvas);
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

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
                self.shell.status_msg = "Dự án PDF nhiều trang chỉ lưu được dạng .iai \
                     (dùng File ▸ Export để xuất PDF)"
                    .to_string();
            }
            return;
        }

        if ext == "iai" {
            if !is_large {
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .ensure_pixels();
            }
            let opts = ExportOptions::default();
            match self.jobs.format_registry.export(
                &self.docs.documents[self.docs.active_doc_idx].canvas,
                path,
                &opts,
            ) {
                Ok(_) => {
                    self.shell.status_msg = format!(
                        "Saved: {}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    );
                    self.docs.current_file = Some(path.to_path_buf());
                    self.docs.documents[self.docs.active_doc_idx].path = Some(path.to_path_buf());
                    self.docs.documents[self.docs.active_doc_idx].file_modified_at =
                        file_modified_at(path);
                    self.mark_active_saved();
                }
                Err(e) => {
                    self.shell.status_msg = format!("Error: {}", e);
                }
            }
            return;
        }

        let (cw, ch) = {
            let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
            (canvas.width, canvas.height)
        };
        if !crate::core::canvas::Canvas::fits_flat_buffer(cw, ch) {
            self.shell.status_msg =
                "Loi: canvas qua lon de luu dang anh phang - hay dung .iai".to_string();
            return;
        }

        if is_large {
            self.docs.documents[self.docs.active_doc_idx].canvas.pixels = self.docs.documents
                [self.docs.active_doc_idx]
                .canvas
                .layer_stack
                .flatten(cw, ch);
            if self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .pixels
                .is_empty()
            {
                self.shell.status_msg =
                    "Lỗi: canvas quá lớn để xuất ảnh — dùng .iai để lưu".to_string();
                return;
            }
        } else {
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .ensure_pixels();
        }
        let save_result = {
            let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
            if crate::formats::ExportFormat::from_extension(&ext).is_some() {
                let opts = ExportOptions {
                    embed_icc: self.shell.ui.export_embed_icc,
                    ..ExportOptions::default()
                };
                self.jobs
                    .format_registry
                    .export(canvas, path, &opts)
                    .map(|_| path.to_path_buf())
                    .map_err(file_io::FileError::Io)
            } else {
                file_io::save(canvas, path)
            }
        };

        match save_result {
            Ok(saved) => {
                self.shell.status_msg = format!(
                    "Saved: {}",
                    saved.file_name().unwrap_or_default().to_string_lossy()
                );
                self.docs.documents[self.docs.active_doc_idx].path = Some(saved.clone());
                self.docs.documents[self.docs.active_doc_idx].file_modified_at =
                    file_modified_at(&saved);
                self.docs.current_file = Some(saved);
                self.mark_active_saved();
            }
            Err(e) => {
                self.shell.status_msg = format!("Error: {}", e);
            }
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

    pub fn do_export(&mut self, format: crate::formats::ExportFormat, path: &str) {
        let mut path_buf = std::path::PathBuf::from(path);
        path_buf.set_extension(format.extension());
        let path_str = path_buf.to_str().unwrap_or(path);

        self.sync_brush_gpu_to_cpu();

        let is_large = self.win.gpu.as_ref().map_or(false, |g| g.is_large_canvas);
        let is_iai = matches!(format, crate::formats::ExportFormat::Iai);
        let (cw, ch) = {
            let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
            (canvas.width, canvas.height)
        };
        if !is_iai && !crate::core::canvas::Canvas::fits_flat_buffer(cw, ch) {
            self.shell.status_msg =
                "Loi: canvas qua lon de export dang anh phang - hay dung .iai".to_string();
            return;
        }
        if is_large && !is_iai {
            self.docs.documents[self.docs.active_doc_idx].canvas.pixels = self.docs.documents
                [self.docs.active_doc_idx]
                .canvas
                .layer_stack
                .flatten(cw, ch);
            if self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .pixels
                .is_empty()
            {
                self.shell.status_msg = "Lỗi: canvas quá lớn để export ảnh".to_string();
                return;
            }
        } else if !is_large {
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .ensure_pixels();
        }

        let opts = ExportOptions {
            embed_icc: self.shell.ui.export_embed_icc,
            ..ExportOptions::default()
        };
        match self.jobs.format_registry.export(
            &self.docs.documents[self.docs.active_doc_idx].canvas,
            std::path::Path::new(path_str),
            &opts,
        ) {
            Ok(_) => {
                self.shell.status_msg = format!(
                    "Saved: {}",
                    path_buf
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("file")
                );
            }
            Err(e) => {
                self.shell.status_msg = format!("Export error: {}", e);
            }
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
            let c = &self.docs.documents[self.docs.active_doc_idx].canvas;
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
        let doc_is_cmyk = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .is_cmyk();

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
        let cmyk: Vec<u8> = if doc_is_cmyk {
            if let Some(ink) = self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .flatten_ink()
            {
                ink_exact = true;
                ink
            } else {
                // Mirror fallback (groups/blend modes/masks in play): flatten
                // onto white and convert through the document's own converter.
                let Some(conv) = self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .cmyk_converter()
                else {
                    self.shell.status_msg = "Chuyển đổi CMYK thất bại".to_string();
                    return;
                };
                let rgba = self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .export_flat();
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
            let rgba = self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .export_flat();
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
            if let Err(e) = image::save_buffer(&path, &plate, cw, ch, image::ExtendedColorType::L8)
            {
                self.shell.status_msg = format!("Lỗi ghi bản {suffix}: {e}");
                return;
            }
        }
        self.shell.status_msg = if ink_exact {
            format!("Đã xuất tách màu từ ink planes: {stem}_C/M/Y/K.png")
        } else {
            format!("Đã xuất tách màu CMYK: {stem}_C/M/Y/K.png")
        };
    }

    /// File -> Export -> PDF (multi-page). Untouched complete PDF groups are
    /// copied verbatim to retain vectors. Other inputs are flattened, losslessly
    /// compressed one page at a time, then assembled in tab order.
    pub fn export_multipage_pdf(&mut self, doc_indices: Vec<usize>) {
        self.sync_brush_gpu_to_cpu();
        if self.docs.active_doc_idx < self.docs.documents.len() {
            self.docs.documents[self.docs.active_doc_idx].reconcile_pdf_page_modified();
        }
        if let [doc_idx] = doc_indices.as_slice() {
            if self.export_pdf_document(*doc_idx) {
                return;
            }
        }

        // A complete PDF group in source order can retain its original page
        // objects. Clean groups are copied byte-for-byte; mixed groups use the
        // hybrid writer below.
        let pdf_group_source = (|| {
            let first_doc = self.docs.documents.get(*doc_indices.first()?)?;
            let first = first_doc.pdf_page.as_ref()?;
            if doc_indices.len() != first.count {
                return None;
            }
            for (expected_page, &idx) in doc_indices.iter().enumerate() {
                let doc = self.docs.documents.get(idx)?;
                let page = doc.pdf_page.as_ref()?;
                if page.group_id != first.group_id
                    || page.source != first.source
                    || page.index != expected_page
                {
                    return None;
                }
            }
            Some(first.source.clone())
        })();
        let original_source = pdf_group_source.clone().filter(|_| {
            doc_indices.iter().all(|&idx| {
                self.docs
                    .documents
                    .get(idx)
                    .is_some_and(|doc| !doc.is_modified())
            })
        });

        let Some(window) = self.win.window.as_ref() else {
            return;
        };
        let parent = file_io::dialog_parent(window);
        let stem = pdf_group_source
            .as_deref()
            .or(self.docs.current_file.as_deref())
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .unwrap_or("document")
            .to_string();
        let mut dialog = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name(format!("{stem}.pdf"));
        if let Some(p) = &parent {
            dialog = dialog.set_parent(p);
        }
        let Some(mut path) = dialog.save_file() else {
            return;
        };
        if path.extension().is_none() {
            path.set_extension("pdf");
        }

        if let Some(source) = original_source {
            match std::fs::read(&source).and_then(|bytes| std::fs::write(&path, bytes)) {
                Ok(()) => {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    self.shell.status_msg = format!("Exported original vector PDF: {name}");
                }
                Err(error) => {
                    self.shell.status_msg = format!("Error copying original PDF: {error}");
                }
            }
            return;
        }

        const PDF_EXPORT_MAX_PIXELS: u64 = 50_000_000;
        if let Some(source) = pdf_group_source {
            let compatibility =
                crate::formats::pdf::hybrid_overlay_compatibility(&source).unwrap_or_default();
            let make_pages =
                |allow_overlay: bool| -> Result<Vec<crate::formats::pdf::HybridPage>, String> {
                    let mut pages = Vec::with_capacity(doc_indices.len());
                    for &idx in &doc_indices {
                        let doc = self
                            .docs
                            .documents
                            .get(idx)
                            .ok_or_else(|| format!("Document {} is missing", idx + 1))?;
                        let source_index = doc
                            .pdf_page
                            .as_ref()
                            .ok_or_else(|| "Document is not a PDF page".to_string())?
                            .index;
                        let overlay = (doc.is_modified()
                            && allow_overlay
                            && compatibility.get(source_index).copied().unwrap_or(false))
                        .then(|| {
                            doc.pdf_page
                                .as_ref()
                                .and_then(|page| page.safe_overlay_pdf_parts(&doc.canvas))
                        })
                        .flatten();
                        let content = if !doc.is_modified() {
                            crate::formats::pdf::HybridPageContent::Original
                        } else if let Some((rgba, vectors)) = overlay {
                            crate::formats::pdf::HybridPageContent::Overlay {
                                rgba,
                                vectors,
                                width: doc.canvas.width,
                                height: doc.canvas.height,
                                dpi: doc.canvas.metadata.resolution_ppi,
                            }
                        } else {
                            let rgba = doc
                                .canvas
                                .export_flat_up_to(PDF_EXPORT_MAX_PIXELS)
                                .ok_or_else(|| {
                                    format!(
                                        "PDF page {} is too large to export safely",
                                        source_index + 1
                                    )
                                })?;
                            crate::formats::pdf::HybridPageContent::Raster {
                                rgba,
                                width: doc.canvas.width,
                                height: doc.canvas.height,
                                dpi: doc.canvas.metadata.resolution_ppi,
                            }
                        };
                        pages.push(crate::formats::pdf::HybridPage {
                            source_index,
                            content,
                        });
                    }
                    Ok(pages)
                };

            let hybrid = make_pages(true)
                .and_then(|pages| crate::formats::pdf::build_hybrid_pdf(&source, &pages))
                .or_else(|_| {
                    make_pages(false)
                        .and_then(|pages| crate::formats::pdf::build_hybrid_pdf(&source, &pages))
                });
            if let Ok(hybrid) = hybrid {
                match std::fs::write(&path, hybrid.bytes) {
                    Ok(()) => {
                        let name = path.file_name().unwrap_or_default().to_string_lossy();
                        self.shell.status_msg = format!(
                            "Exported hybrid PDF: {name} ({} vector, {} overlay, {} raster pages)",
                            hybrid.vector_pages, hybrid.overlay_pages, hybrid.raster_pages
                        );
                    }
                    Err(error) => {
                        self.shell.status_msg = format!("Error writing hybrid PDF: {error}");
                    }
                }
                return;
            }
            // Structural PDF parsing can fail on unusual/encrypted inputs. Fall
            // through to the existing all-raster writer rather than losing export.
        }

        let mut encoded = Vec::with_capacity(doc_indices.len());
        // Crisp vector overlay per page (empty for CMYK ink / re-rendered PDF
        // pages, which stay pure raster).
        let mut vectors: Vec<Vec<crate::core::print::PdfVectorObject>> =
            Vec::with_capacity(doc_indices.len());
        let mut ink_pages = 0usize;
        for &idx in &doc_indices {
            let Some(doc) = self.docs.documents.get(idx) else {
                continue;
            };
            let mut page_vectors: Vec<crate::core::print::PdfVectorObject> = Vec::new();
            let lazy_page = doc.pdf_page.as_ref().filter(|page| !page.loaded).cloned();
            let encoded_page = if let Some(page) = lazy_page {
                let canvas = match crate::formats::pdf::PdfImporter::render_selected(
                    &page.source,
                    &[page.index],
                    Some(page.requested_dpi),
                )
                .and_then(|mut pages| {
                    pages
                        .pop()
                        .ok_or_else(|| format!("PDF page {} did not render", page.index + 1))
                }) {
                    Ok(canvas) => canvas,
                    Err(error) => {
                        self.shell.status_msg =
                            format!("Error rendering PDF page {}: {error}", page.index + 1);
                        return;
                    }
                };
                let (w, h, dpi) = (canvas.width, canvas.height, canvas.metadata.resolution_ppi);
                let Some(rgba) = canvas.export_flat_up_to(PDF_EXPORT_MAX_PIXELS) else {
                    self.shell.status_msg =
                        format!("PDF page {} is too large to export safely", page.index + 1);
                    return;
                };
                crate::core::print::encode_pdf_page(&rgba, w, h, dpi)
            } else {
                let (w, h, dpi) = (
                    doc.canvas.width,
                    doc.canvas.height,
                    doc.canvas.metadata.resolution_ppi,
                );
                let too_large = crate::core::canvas::Canvas::pixel_count(w, h)
                    .is_none_or(|p| p > PDF_EXPORT_MAX_PIXELS);
                if too_large {
                    self.shell.status_msg =
                        format!("Document {} is too large to export safely", idx + 1);
                    return;
                }
                if doc.canvas.is_cmyk() {
                    // CMYK page: promote qualifying vectors to native DeviceCMYK
                    // paths over an ink raster base (press-ready + resolution
                    // independent). When the stack isn't ink-exact, flatten the
                    // RGB mirror instead (no native vectors) — same as before.
                    let selection = crate::core::print::collect_pdf_vectors(&doc.canvas);
                    if let Some(ink) = crate::core::print::pdf_ink_base(&doc.canvas, &selection) {
                        ink_pages += 1;
                        page_vectors = selection.objects;
                        crate::core::print::encode_pdf_page_cmyk(&ink, w, h, dpi)
                    } else {
                        let rgba = doc.canvas.export_flat();
                        crate::core::print::encode_pdf_page(&rgba, w, h, dpi)
                    }
                } else {
                    // RGB page: split native PDF paths out of the raster base so
                    // their anti-aliased cache cannot leave a jagged halo.
                    let selection = crate::core::print::collect_pdf_vectors(&doc.canvas);
                    let rgba = crate::core::print::pdf_raster_base(&doc.canvas, &selection);
                    page_vectors = selection.objects;
                    crate::core::print::encode_pdf_page(&rgba, w, h, dpi)
                }
            };
            match encoded_page {
                Ok(page) => {
                    encoded.push(page);
                    vectors.push(page_vectors);
                }
                Err(error) => {
                    self.shell.status_msg = format!("Error encoding PDF page: {error}");
                    return;
                }
            }
        }
        if encoded.is_empty() {
            self.shell.status_msg = "No valid pages to export as PDF".to_string();
            return;
        }

        let icc = self
            .shell
            .ui
            .export_embed_icc
            .then(crate::core::cms::srgb_icc_bytes);
        let n = encoded.len();
        match crate::core::print::build_pdf_multipage_encoded(&encoded, &vectors, icc.as_deref()) {
            Ok(bytes) => match std::fs::write(&path, &bytes) {
                Ok(_) => {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    self.shell.status_msg = if ink_pages > 0 {
                        format!(
                            "Exported {n}-page lossless PDF ({ink_pages} DeviceCMYK ink): {name}"
                        )
                    } else {
                        format!("Exported {n}-page lossless PDF: {name}")
                    };
                }
                Err(e) => {
                    self.shell.status_msg = format!("Error writing PDF: {e}");
                }
            },
            Err(e) => {
                self.shell.status_msg = format!("Error building PDF: {e}");
            }
        }
    }

    pub(in crate::app) fn export_pdf_document(&mut self, doc_idx: usize) -> bool {
        const PDF_EXPORT_MAX_PIXELS: u64 = 50_000_000;
        let Some(doc) = self.docs.documents.get(doc_idx) else {
            return false;
        };
        let Some(pdf) = doc.pdf_document.as_ref() else {
            return false;
        };
        let all_pages_selected = pdf.selected_pages.len() == pdf.page_count
            && pdf.selected_pages.iter().copied().eq(0..pdf.page_count);

        let original_source = pdf.source.clone();
        let source = pdf.effective_source().to_path_buf();
        let selected_pages = pdf.selected_pages.clone();
        let clean = !doc.is_modified();
        let Some(window) = self.win.window.as_ref() else {
            return true;
        };
        let parent = file_io::dialog_parent(window);
        let stem = original_source
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("document");
        let mut dialog = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name(format!("{stem}.pdf"));
        if let Some(parent) = &parent {
            dialog = dialog.set_parent(parent);
        }
        let Some(mut path) = dialog.save_file() else {
            return true;
        };
        if path.extension().is_none() {
            path.set_extension("pdf");
        }

        if clean && all_pages_selected {
            match std::fs::read(&source).and_then(|bytes| std::fs::write(&path, bytes)) {
                Ok(()) => self.shell.status_msg = "Exported original vector PDF".to_string(),
                Err(error) => {
                    self.shell.status_msg = format!("Error copying original PDF: {error}")
                }
            }
            return true;
        }

        let compatibility =
            crate::formats::pdf::hybrid_overlay_compatibility(&source).unwrap_or_default();
        let make_pages =
            |allow_overlay: bool| -> Result<Vec<crate::formats::pdf::HybridPage>, String> {
                let doc = &self.docs.documents[doc_idx];
                let pdf = doc.pdf_document.as_ref().unwrap();
                let mut pages = Vec::with_capacity(selected_pages.len());
                for &page_index in &selected_pages {
                    let edited = if page_index == pdf.active_page && pdf.active_page_modified {
                        Some((&doc.canvas, doc.pdf_page.as_ref().unwrap()))
                    } else {
                        pdf.edited_pages
                            .get(&page_index)
                            .map(|page| (&page.canvas, &page.reference))
                    };
                    let content = if let Some((canvas, reference)) = edited {
                        let overlay = (allow_overlay
                            && compatibility.get(page_index).copied().unwrap_or(false))
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
                                || {
                                    format!(
                                        "PDF page {} is too large to export safely",
                                        page_index + 1
                                    )
                                },
                            )?;
                            crate::formats::pdf::HybridPageContent::Raster {
                                rgba,
                                width: canvas.width,
                                height: canvas.height,
                                dpi: canvas.metadata.resolution_ppi,
                            }
                        }
                    } else {
                        crate::formats::pdf::HybridPageContent::Original
                    };
                    pages.push(crate::formats::pdf::HybridPage {
                        source_index: page_index,
                        content,
                    });
                }
                Ok(pages)
            };
        let hybrid = make_pages(true)
            .and_then(|pages| crate::formats::pdf::build_hybrid_pdf(&source, &pages))
            .or_else(|_| {
                make_pages(false)
                    .and_then(|pages| crate::formats::pdf::build_hybrid_pdf(&source, &pages))
            });
        match hybrid {
            Ok(hybrid) => match std::fs::write(&path, hybrid.bytes) {
                Ok(()) => {
                    self.shell.status_msg = format!(
                        "Exported hybrid PDF ({} vector, {} overlay, {} raster pages)",
                        hybrid.vector_pages, hybrid.overlay_pages, hybrid.raster_pages
                    )
                }
                Err(error) => self.shell.status_msg = format!("Error writing hybrid PDF: {error}"),
            },
            Err(error) => self.shell.status_msg = format!("Error building hybrid PDF: {error}"),
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::requires_project_save_on_close;

    #[test]
    fn close_save_keeps_source_format_for_single_layer_image() {
        assert!(!requires_project_save_on_close(1, false));
    }

    #[test]
    fn close_save_defaults_to_project_for_multiple_layers_or_pdf() {
        assert!(requires_project_save_on_close(2, false));
        assert!(requires_project_save_on_close(1, true));
    }
}

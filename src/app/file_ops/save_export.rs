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
            self.save_to(&path);
        } else {
            self.do_save_as();
        }
    }

    pub fn do_save_as(&mut self) {
        self.do_save_as_with_suggestion(self.docs.current_file.clone());
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
            Err(e) => {
                self.shell.status_msg = format!("Lỗi lưu tài liệu: {e}");
            }
        }
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
                self.shell.status_msg = "Tài liệu nhiều trang chỉ lưu được dạng .iai \
                     (dùng File ▸ Export để xuất ảnh)"
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
                    resize_long_edge: self
                        .shell
                        .ui
                        .export_resize_enabled
                        .then_some(self.shell.ui.export_resize_long_edge),
                    output_sharpen: self.shell.ui.export_output_sharpen,
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
        // Raster export of a page that shows the shared master: build a throwaway
        // canvas with the master composited beneath and export THAT. `.iai` keeps
        // the master stored separately (via the artboard-document save), so it is
        // never merged here.
        let mut merged = if is_iai {
            None
        } else {
            let active = self.docs.documents[self.docs.active_doc_idx].active_artboard;
            self.docs.documents[self.docs.active_doc_idx].page_render_canvas(active)
        };
        if let Some(m) = merged.as_mut() {
            m.pixels = m.layer_stack.flatten(cw, ch);
            if m.pixels.is_empty() {
                self.shell.status_msg = "Lỗi: không dựng được ảnh có trang nền".to_string();
                return;
            }
            m.pixels_stale = false;
            match self.jobs.format_registry.export(
                m,
                std::path::Path::new(path_str),
                &ExportOptions {
                    embed_icc: self.shell.ui.export_embed_icc,
                    resize_long_edge: self
                        .shell
                        .ui
                        .export_resize_enabled
                        .then_some(self.shell.ui.export_resize_long_edge),
                    output_sharpen: self.shell.ui.export_output_sharpen,
                    pdf_marks: self.shell.ui.export_pdf_marks,
                    ..ExportOptions::default()
                },
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
                Err(e) => self.shell.status_msg = format!("Export error: {}", e),
            }
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
            resize_long_edge: self
                .shell
                .ui
                .export_resize_enabled
                .then_some(self.shell.ui.export_resize_long_edge),
            output_sharpen: self.shell.ui.export_output_sharpen,
            pdf_marks: self.shell.ui.export_pdf_marks,
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
        match std::fs::write(&path, svg.as_bytes()) {
            Ok(_) => {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("artwork.svg");
                self.shell.status_msg = format!("Exported SVG: {name}");
            }
            Err(e) => self.shell.status_msg = format!("Error writing SVG: {e}"),
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
        let mut cmyk: Vec<u8> = if doc_is_cmyk {
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

        // Spot inks painted by promoted vector objects each get their OWN film,
        // and (unless the object overprints) knock their footprint out of the
        // process plates, so a spot area prints once — on its plate — instead of
        // twice (spot film + the CMYK approximation baked into the raster base).
        // A document with no spot inks yields no plates and a zero knockout, so
        // the C/M/Y/K plates below stay byte-identical to before.
        let (spot_seps, knockout) = {
            let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
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
            if let Err(e) = image::save_buffer(&path, &plate, cw, ch, image::ExtendedColorType::L8)
            {
                self.shell.status_msg = format!("Lỗi ghi bản {suffix}: {e}");
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
            if let Err(e) = image::save_buffer(&path, &plate, cw, ch, image::ExtendedColorType::L8)
            {
                self.shell.status_msg = format!("Lỗi ghi bản spot {safe}: {e}");
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

        if self.edit.text_edit.is_some() {
            self.commit_text_edit();
        }
        self.sync_brush_gpu_to_cpu();
        let idx = self.docs.active_doc_idx;
        if idx < self.docs.documents.len() {
            self.docs.documents[idx].reconcile_pdf_page_modified();
        }
        let Some(doc) = self.docs.documents.get(idx) else {
            return;
        };
        let is_pdf = doc.pdf_document.is_some();
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
        } else {
            doc.active_artboard.min(page_count.saturating_sub(1))
        };
        let pages = match self.shell.ui.pdf_export_scope {
            PdfExportScope::AllPages => Ok((0..page_count).collect()),
            PdfExportScope::CurrentPage => Ok(vec![current]),
            PdfExportScope::Range => {
                crate::ui::intent::parse_pdf_page_range(&self.shell.ui.pdf_export_range, page_count)
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
        if is_pdf {
            self.export_pdf_document_selected(idx, &pages, target_dpi);
        } else {
            self.export_document_pages_pdf_selected(&pages, target_dpi);
        }
    }

    /// Export selected pages of the active artboard document as one PDF, with
    /// native vector overlays, optional downsampling, ICC and press marks.
    fn export_document_pages_pdf_selected(&mut self, page_indices: &[usize], target_dpi: u32) {
        let idx = self.docs.active_doc_idx;

        // Encode each page up front while the document is borrowed; release the
        // borrow before the file dialog and status writes.
        let (encoded, vectors, encode_error) = {
            let Some(doc) = self.docs.documents.get(idx) else {
                return;
            };
            let canvases = doc.all_page_canvases();
            let mut encoded = Vec::with_capacity(page_indices.len());
            let mut vectors = Vec::with_capacity(page_indices.len());
            let mut error = None;
            for &page_index in page_indices {
                // Compose the shared master beneath the page when it uses one; the
                // merged canvas is throwaway. Falls back to the page as-is.
                let merged = doc.page_render_canvas(page_index);
                let canvas: &Canvas = match merged.as_ref() {
                    Some(c) => c,
                    None => match canvases.get(page_index) {
                        Some(&c) => c,
                        None => {
                            error = Some(format!("Trang {} không tồn tại", page_index + 1));
                            break;
                        }
                    },
                };
                // Split native PDF paths out of the raster base so their cached
                // anti-aliasing cannot leave a jagged twin (mirrors the tab path).
                let selection = crate::core::print::collect_pdf_vectors(canvas);
                let rgba = crate::core::print::pdf_raster_base(canvas, &selection);
                let prepared = prepare_pdf_pixels(
                    rgba,
                    canvas.width,
                    canvas.height,
                    canvas.metadata.resolution_ppi,
                    target_dpi,
                );
                match prepared.and_then(|(rgba, width, height, dpi)| {
                    crate::core::print::encode_pdf_page(&rgba, width, height, dpi)
                        .map(|page| page.with_vector_space(canvas.width, canvas.height))
                }) {
                    Ok(page) => {
                        encoded.push(page);
                        vectors.push(selection.objects);
                    }
                    Err(e) => {
                        error = Some(e);
                        break;
                    }
                }
            }
            (encoded, vectors, error)
        };
        if let Some(e) = encode_error {
            self.shell.status_msg = format!("Lỗi mã hoá trang PDF: {e}");
            return;
        }
        if encoded.is_empty() {
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

        let marks = self.shell.ui.export_pdf_marks;
        let icc = self
            .shell
            .ui
            .export_embed_icc
            .then(crate::core::cms::srgb_icc_bytes);
        let n = encoded.len();
        match crate::core::print::build_pdf_multipage_encoded(
            &encoded,
            &vectors,
            marks,
            icc.as_deref(),
        ) {
            Ok(bytes) => match std::fs::write(&path, &bytes) {
                Ok(()) => {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    self.shell.status_msg = format!("Đã xuất PDF {n} trang: {name}");
                }
                Err(e) => {
                    self.shell.status_msg = format!("Lỗi ghi PDF: {e}");
                }
            },
            Err(e) => {
                self.shell.status_msg = format!("Lỗi tạo PDF: {e}");
            }
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
    use super::{prepare_pdf_pixels, requires_project_save_on_close};

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
    fn pdf_dpi_never_upscales() {
        let pixels = vec![255; 20 * 10 * 4];
        let (_, width, height, dpi) = prepare_pdf_pixels(pixels, 20, 10, 150.0, 300).unwrap();
        assert_eq!((width, height, dpi), (20, 10, 150.0));
    }
}

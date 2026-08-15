//! "Làm sạch bản scan" (Image ▸ Làm sạch bản scan…): flatten an uneven, greyed
//! scan background to white and deepen the text. Works on a plain image document
//! (the active raster layer) and on a multi-page PDF session — the current page,
//! a page range, or every page. The pixel maths lives in
//! [`crate::core::scan_cleanup`]; this drives it against the app's documents.

use super::render::CanvasEvent;
use super::state::App;
use crate::core::canvas::Canvas;
use crate::core::scan_cleanup::{
    clean_scan_rgba, resolve_pages, ScanCleanScope, ScanCleanupParams, ScanCleanupRequest,
};
use crate::core::tile::TileMap;

const CLEAN_LABEL: &str = "Làm sạch bản scan";

impl App {
    /// Dialog entry point. Routes to the single-canvas path (a plain image or the
    /// current PDF page) or the multi-page PDF batch.
    pub(crate) fn apply_scan_cleanup(&mut self, req: ScanCleanupRequest) {
        let idx = self.docs.active_doc_idx;
        let is_pdf = self.docs.documents[idx].pdf_document.is_some();
        // A page range/all only means something for a PDF; otherwise collapse to
        // "clean the current image".
        let single = matches!(req.scope, ScanCleanScope::CurrentPage) || !is_pdf;
        if single {
            match self.clean_scan_active_layer(req.params) {
                Ok(()) => self.shell.status_msg = format!("{CLEAN_LABEL}: xong"),
                Err(msg) => self.shell.status_msg = msg,
            }
        } else {
            self.clean_scan_pdf_pages(req.scope, req.params);
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Clean the active document's active raster layer in place, as one undo
    /// step. Returns a localized error when the active layer can't be cleaned.
    pub(crate) fn clean_scan_active_layer(
        &mut self,
        params: ScanCleanupParams,
    ) -> Result<(), String> {
        let idx = self.docs.active_doc_idx;
        let ok = {
            let canvas = &mut self.docs.documents[idx].canvas;
            if canvas.is_cmyk() {
                return Err("Làm sạch bản scan chưa hỗ trợ chế độ CMYK".to_string());
            }
            canvas.layer_stack.normalize_active_idx();
            let (layer_id, w, h, before, rgba) = {
                let Some(layer) = canvas.layer_stack.layers.get(canvas.layer_stack.active_idx)
                else {
                    return Err("Không có layer để làm sạch".to_string());
                };
                if (!layer.is_background && layer.locked) || !layer.is_raster() {
                    return Err("Cần chọn một layer ảnh (raster) đang mở khoá".to_string());
                }
                (
                    layer.id,
                    layer.width,
                    layer.height,
                    layer.tiles.clone(),
                    layer.flatten_tiles(),
                )
            };
            let cleaned = clean_scan_rgba(&rgba, w, h, params);
            let after = TileMap::from_rgba(&cleaned, w, h);
            canvas.commit_layer_tiles_change(layer_id, before, after, CLEAN_LABEL)
        };
        if ok {
            self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
            Ok(())
        } else {
            Err("Không thể làm sạch layer này".to_string())
        }
    }

    /// Clean a set of PDF pages (current / range / all). The active page uses the
    /// undoable single-layer path; other pages are cleaned on their cached canvas
    /// or rendered clean from the source, cleaned, and cached as an edited page.
    ///
    /// Runs synchronously: every cleaned page is held in memory (as any multi-page
    /// edit is), so a large "all pages" range on a big PDF is heavy — the dialog's
    /// range picker is the control for that.
    fn clean_scan_pdf_pages(&mut self, scope: ScanCleanScope, params: ScanCleanupParams) {
        let idx = self.docs.active_doc_idx;
        let (page_count, active_page, requested_dpi, source, group_id) = {
            let doc = &self.docs.documents[idx];
            let Some(pdf) = doc.pdf_document.as_ref() else {
                return;
            };
            let group_id = doc.pdf_page.as_ref().map_or(0, |p| p.group_id);
            (
                pdf.page_count,
                pdf.active_page,
                pdf.requested_dpi,
                pdf.effective_source().to_path_buf(),
                group_id,
            )
        };
        let pages = resolve_pages(scope, page_count, active_page);
        if pages.is_empty() {
            self.shell.status_msg = "Không có trang nào để làm sạch".to_string();
            return;
        }

        let mut cleaned = 0usize;
        let mut errors = 0usize;
        for page in pages {
            if page == active_page {
                match self.clean_scan_active_layer(params) {
                    Ok(()) => cleaned += 1,
                    Err(_) => errors += 1,
                }
                continue;
            }
            let has_cached = self.docs.documents[idx]
                .pdf_document
                .as_ref()
                .is_some_and(|pdf| pdf.edited_pages.contains_key(&page));
            if has_cached {
                if self.clean_scan_cached_page(idx, page, params) {
                    cleaned += 1;
                } else {
                    errors += 1;
                }
                continue;
            }
            match crate::formats::pdf::PdfImporter::render_selected(
                &source,
                &[page],
                Some(requested_dpi),
            ) {
                Ok(mut canvases) => match canvases.pop() {
                    Some(mut canvas) => {
                        if clean_scan_canvas_base(&mut canvas, params) {
                            self.install_cleaned_pdf_page(
                                idx,
                                page,
                                group_id,
                                &source,
                                page_count,
                                requested_dpi,
                                canvas,
                            );
                            cleaned += 1;
                        } else {
                            errors += 1;
                        }
                    }
                    None => errors += 1,
                },
                Err(_) => errors += 1,
            }
        }

        self.refresh_active_document();
        self.shell.status_msg = if errors == 0 {
            format!("{CLEAN_LABEL}: đã làm sạch {cleaned} trang")
        } else {
            format!("{CLEAN_LABEL}: {cleaned} trang xong, {errors} trang lỗi")
        };
    }

    /// Clean an already-edited (cached, inactive) page's base layer in place.
    fn clean_scan_cached_page(
        &mut self,
        doc_idx: usize,
        page: usize,
        params: ScanCleanupParams,
    ) -> bool {
        let Some(pdf) = self.docs.documents[doc_idx].pdf_document.as_mut() else {
            return false;
        };
        let Some(cached) = pdf.edited_pages.get_mut(&page) else {
            return false;
        };
        if clean_scan_canvas_base(&mut cached.canvas, params) {
            cached.reference.mark_base_dirty();
            true
        } else {
            false
        }
    }

    /// Store a freshly-rendered, cleaned page as an edited page so it persists
    /// with the project and shows on navigation. The reference is marked dirty so
    /// export/save treat the base as edited (no pristine-overlay fast path).
    #[allow(clippy::too_many_arguments)]
    fn install_cleaned_pdf_page(
        &mut self,
        doc_idx: usize,
        page: usize,
        group_id: u32,
        source: &std::path::Path,
        page_count: usize,
        dpi: f32,
        canvas: Canvas,
    ) {
        let mut reference = crate::core::document::PdfPageRef {
            group_id,
            source: source.to_path_buf(),
            index: page,
            count: page_count,
            requested_dpi: dpi,
            loaded: true,
            original_width: 0,
            original_height: 0,
            original_dpi: dpi,
            original_layer_id: 0,
            original_tiles_fingerprint: 0,
        };
        reference.record_canvas_baseline(&canvas);
        reference.mark_base_dirty();
        if let Some(pdf) = self.docs.documents[doc_idx].pdf_document.as_mut() {
            pdf.edited_pages.insert(
                page,
                crate::core::document::PdfCachedPage {
                    canvas,
                    reference,
                    saved_zoom: 0.0,
                    saved_offset_x: 0.0,
                    saved_offset_y: 0.0,
                },
            );
        }
    }
}

/// Clean a canvas's base (page-0) raster layer in place, recompositing so a
/// later thumbnail/render reflects it. Returns false for a CMYK canvas or one
/// without a raster base. Not undoable on its own (used for inactive PDF pages).
fn clean_scan_canvas_base(canvas: &mut Canvas, params: ScanCleanupParams) -> bool {
    if canvas.is_cmyk() {
        return false;
    }
    let Some((w, h, rgba)) = canvas.layer_stack.layers.first().and_then(|layer| {
        layer
            .is_raster()
            .then(|| (layer.width, layer.height, layer.flatten_tiles()))
    }) else {
        return false;
    };
    let cleaned = clean_scan_rgba(&rgba, w, h, params);
    if let Some(layer) = canvas.layer_stack.layers.first_mut() {
        layer.tiles = TileMap::from_rgba(&cleaned, w, h);
    }
    canvas.layer_revision += 1;
    canvas.mark_dirty_unconditionally();
    canvas.dirty.expand_full(canvas.width, canvas.height);
    canvas.flatten_full();
    true
}

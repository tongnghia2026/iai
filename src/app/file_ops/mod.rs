pub(crate) mod mail_merge;
mod open;
mod pdf_session;
pub(crate) mod save_export;

use super::state::App;
use crate::core::canvas::{Canvas, CanvasMetadata};
use crate::core::tile::TileMap;
use std::path::Path;

pub(in crate::app) fn normalized_path_key(path: &Path) -> String {
    let normalized = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let key = normalized.to_string_lossy().to_string();
    #[cfg(windows)]
    {
        key.to_ascii_lowercase()
    }
    #[cfg(not(windows))]
    {
        key
    }
}

#[cfg(test)]
mod tests {
    use super::pdf_session::fresh_pdf_page_ref;
    use super::*;

    fn solid_canvas(color: [u8; 4], w: u32, h: u32) -> Canvas {
        Canvas::from_rgba(
            color
                .into_iter()
                .cycle()
                .take((w * h * 4) as usize)
                .collect(),
            w,
            h,
        )
    }

    #[test]
    fn saving_a_pdf_project_writes_every_edited_page_and_clears_dirty() {
        let mut app = App::new();
        let dir = std::env::temp_dir().join(format!(
            "iai-proj-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let project_path = dir.join("doc.iai");
        let source = dir.join("document.pdf");
        let source_pdf = b"%PDF-1.4\n1 0 obj\n<<>>\nendobj\ntrailer\n<<>>\n%%EOF\n".to_vec();
        std::fs::write(&source, &source_pdf).unwrap();

        // Active page 0 (edited, red) + a cached edited page 1 (blue).
        app.docs.documents[0].canvas = solid_canvas([220, 10, 10, 255], 4, 4);
        let mut active_ref = fresh_pdf_page_ref(1, &source, 0, 3, 300.0);
        active_ref.record_canvas_baseline(&app.docs.documents[0].canvas);
        app.docs.documents[0].pdf_page = Some(active_ref);

        let mut cached_ref = fresh_pdf_page_ref(1, &source, 1, 3, 300.0);
        let mut cached_canvas = solid_canvas([10, 10, 220, 255], 4, 4);
        cached_ref.record_canvas_baseline(&cached_canvas);
        // A real unsaved edit — dirty is derived, not assignable.
        cached_canvas.deselect();
        let mut edited = std::collections::HashMap::new();
        edited.insert(
            1usize,
            crate::core::document::PdfCachedPage {
                canvas: cached_canvas,
                reference: cached_ref,
                saved_zoom: 1.0,
                saved_offset_x: 0.0,
                saved_offset_y: 0.0,
            },
        );
        app.docs.documents[0].pdf_document = Some(crate::core::document::PdfDocumentState {
            source: source.clone(),
            embedded_source: None,
            page_count: 3,
            selected_pages: vec![0, 1, 2],
            selected_pages_saved: vec![0, 1, 2],
            page_names: std::collections::BTreeMap::new(),
            page_names_saved: std::collections::BTreeMap::new(),
            requested_dpi: 300.0,
            active_page: 0,
            active_page_modified: true,
            edited_pages: edited,
            global_clears: Vec::new(),
            global_clears_saved: Vec::new(),
            global_clears_redo: Vec::new(),
            global_overlay_cache: None,
        });

        app.save_pdf_project_to(&project_path);

        // Two edited pages written; the session is now clean and remembers its path.
        assert!(!app.is_modified());
        assert!(!app.docs.documents[0].is_modified());
        assert_eq!(
            app.docs.current_file.as_deref(),
            Some(project_path.as_path())
        );

        // Re-read the project off disk: link + both edited pages come back.
        match crate::formats::iai::load(&project_path).unwrap() {
            crate::formats::iai::IaiLoad::PdfProject(project) => {
                assert_eq!(project.source, source);
                assert_eq!(project.embedded_pdf, Some(source_pdf.clone()));
                assert_eq!(project.page_count, 3);
                assert_eq!(project.selected_pages, vec![0, 1, 2]);
                assert_eq!(project.active_page, 0);
                assert_eq!(project.pages.len(), 2);
                let page0 = project.pages.iter().find(|p| p.index == 0).unwrap();
                assert!(page0
                    .canvas
                    .export_flat()
                    .chunks_exact(4)
                    .all(|px| px[0] > 200 && px[2] < 20));
                let page1 = project.pages.iter().find(|p| p.index == 1).unwrap();
                assert!(page1
                    .canvas
                    .export_flat()
                    .chunks_exact(4)
                    .all(|px| px[2] > 200 && px[0] < 20));
            }
            _ => panic!("expected a PDF project"),
        }
        let zip_file = std::fs::File::open(&project_path).unwrap();
        let mut archive = zip::ZipArchive::new(zip_file).unwrap();
        assert!(archive.by_name("source.pdf").is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}

impl App {
    pub(crate) fn has_only_welcome_placeholder(&self) -> bool {
        self.shell.ui.show_welcome
            && self.docs.documents.len() == 1
            && self.docs.current_file.is_none()
            && !self.is_modified()
            && self.docs.documents[0].path.is_none()
    }

    /// Close a file/tab (split from the close_file_without_saving branch of
    /// apply_ui_actions so it can be reused after Save & Close completes).
    pub fn execute_close(&mut self) {
        if let Some(idx) = self.docs.pending_close_doc_idx {
            self.close_doc_confirmed(idx);
        } else {
            self.docs.current_file = None;
            self.do_new("Untitled".to_string(), 800, 600, 72.0, 0);
            self.shell.ui.show_welcome = true;
            self.mark_active_saved();
        }
    }

    /// Create a new blank canvas in a new tab and switch to it.
    /// Called from the New Canvas dialog (Ctrl+N confirm).
    /// Unlike `do_new`, this never overwrites an existing document.
    #[allow(clippy::too_many_arguments)]
    pub fn do_new_tab(
        &mut self,
        name: String,
        w: u32,
        h: u32,
        dpi: f32,
        bg: u8,
        unit: crate::core::units::Unit,
        cmyk: bool,
    ) {
        let w = w.max(1);
        let h = h.max(1);
        self.shell.canvas_unit = unit;

        let id = crate::core::document::DocumentId(self.docs.next_doc_id);
        self.docs.next_doc_id += 1;

        let mut canvas = Canvas::new(w, h);
        match bg {
            0 => {
                canvas.layer_stack.layers[0].is_background = true;
            }
            1 => {
                canvas.layer_stack.layers[0].tiles = crate::core::tile::TileMap::new_black(w, h);
                canvas.layer_stack.layers[0].is_background = true;
            }
            2 => {
                canvas.layer_stack.layers[0].tiles = TileMap::new(w, h);
                canvas.layer_stack.layers[0].name = "Layer 1".to_string();
                canvas.layer_stack.layers[0].is_background = false;
                canvas.layer_stack.layers[0].locked = false;
            }
            _ => {}
        }
        let mut metadata = CanvasMetadata::default();
        metadata.resolution_ppi = dpi;
        canvas.metadata = metadata;

        // A CMYK document is created as RGB then converted through the built-in
        // Generic naive space (flattens onto ink planes, clears the empty history).
        if cmyk {
            let _ = canvas.convert_to_cmyk(crate::core::canvas::CmykProfile::Naive);
        }

        let mut doc = crate::core::document::Document::new(id, w, h);
        doc.canvas = canvas;
        doc.title = name.clone();
        doc.path = None;

        if self.has_only_welcome_placeholder() {
            self.docs.documents[0] = doc;
            self.docs.active_doc_idx = 0;
        } else {
            self.docs.documents.push(doc);
            self.docs.active_doc_idx = self.docs.documents.len() - 1;
        }
        self.touch_doc_mru();

        self.docs.current_file = None;
        self.mark_active_saved();
        self.shell.ui.show_new_dialog = false;
        self.shell.ui.show_welcome = false;
        self.edit.input.painting = false;
        self.edit.transform_state = None;
        self.edit.pending_stroke_inputs.clear();

        if let Some(gpu) = &mut self.win.gpu {
            gpu.resize_canvas_texture(w, h);
            gpu.compositor.tile_atlas.clear();
            gpu.compositor.ping_initialized = false;
            gpu.compositor.last_result_is_ping = false;
        }

        let needs_viewport_mode = self.win.gpu.as_ref().map_or(false, |g| g.is_large_canvas);
        if needs_viewport_mode {
            self.shell.status_msg = format!(
                "Canvas {}×{} — Viewport Streaming mode (canvas lớn, CPU composite)",
                w, h
            );
        } else {
            self.shell.status_msg = format!("New canvas {}x{}", w, h);
        }

        self.fit_canvas_to_screen();
        self.push_canvas_uniforms();
        self.upload_full();
        self.upload_selection_mask();
    }

    pub fn do_new(&mut self, name: String, w: u32, h: u32, dpi: f32, bg: u8) {
        use crate::core::canvas::CanvasMetadata;
        let w = w.max(1);
        let h = h.max(1);
        self.shell.canvas_unit = self.shell.ui.new_unit;
        let old_id = self.docs.documents[self.docs.active_doc_idx].id;
        self.docs.pdf_render_services.remove(&old_id);
        self.clear_embedded_pdf_for(old_id);
        self.docs.documents[self.docs.active_doc_idx].pdf_page = None;
        self.docs.documents[self.docs.active_doc_idx].pdf_document = None;

        let mut metadata = CanvasMetadata::default();
        metadata.resolution_ppi = dpi;
        self.docs.documents[self.docs.active_doc_idx].canvas = Canvas::new(w, h);
        match bg {
            0 => {
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers[0]
                    .is_background = true;
            }
            1 => {
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers[0]
                    .tiles = crate::core::tile::TileMap::new_black(w, h);
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers[0]
                    .is_background = true;
            }
            2 => {
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers[0]
                    .tiles = crate::core::tile::TileMap::new(w, h);
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers[0]
                    .name = "Layer 1".to_string();
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers[0]
                    .is_background = false;
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers[0]
                    .locked = false;
            }
            _ => {}
        }
        self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .metadata = metadata;
        self.docs.documents[self.docs.active_doc_idx].path = None;
        self.docs.documents[self.docs.active_doc_idx].title = name.clone();
        self.docs.current_file = None;
        self.mark_active_saved();
        self.shell.ui.show_new_dialog = false;
        self.shell.ui.show_welcome = false;
        self.edit.input.painting = false;

        if let Some(gpu) = &mut self.win.gpu {
            gpu.resize_canvas_texture(w, h);
            gpu.compositor.tile_atlas.clear();
            gpu.compositor.ping_initialized = false;
            gpu.compositor.last_result_is_ping = false;
        }

        let needs_viewport_mode = self.win.gpu.as_ref().map_or(false, |g| g.is_large_canvas);
        if needs_viewport_mode {
            self.shell.status_msg = format!(
                "Canvas {}×{} — Viewport Streaming mode (canvas lớn, CPU composite)",
                w, h
            );
        } else {
            self.shell.status_msg = format!("New canvas {}x{}", w, h);
        }

        self.fit_canvas_to_screen();
        self.push_canvas_uniforms();
        self.upload_full();
        self.upload_selection_mask();

        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
}

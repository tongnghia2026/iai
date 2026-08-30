// Document management methods: switch_to_doc, close_doc, open_new_doc_tab.

use super::state::App;
use crate::core::document::{Document, DocumentId};

impl App {
    /// Save the current doc's view state, switch active_doc_idx to `idx`,
    /// restore the new doc's view state (fit to screen if never viewed), then
    /// re-upload the GPU canvas.
    pub fn switch_to_doc(&mut self, idx: usize) {
        if idx == self.docs.active_doc_idx || idx >= self.docs.documents.len() {
            return;
        }
        if self.jobs.pending_pdf_page_render.is_some() {
            self.shell.status_msg = "Wait for the PDF page to finish rendering".to_string();
            return;
        }
        // Finalize an open text session while its document is still active —
        // leaving it dangling across a switch would strand the layer as the
        // blank editing placeholder.
        if self.edit.text_edit.is_some() {
            self.commit_text_edit();
        }
        // Interactive vector-style previews are pinned to the current canvas.
        // Commit while that document is still active so layer-id reuse in the
        // destination document can never redirect or discard the edit.
        self.path_style_commit();
        self.docs.documents[self.docs.active_doc_idx].saved_zoom = self.edit.view.zoom;
        self.docs.documents[self.docs.active_doc_idx].saved_offset_x = self.edit.view.offset_x;
        self.docs.documents[self.docs.active_doc_idx].saved_offset_y = self.edit.view.offset_y;

        self.docs.documents[self.docs.active_doc_idx].reconcile_pdf_page_modified();

        self.docs.active_doc_idx = idx;
        self.refresh_active_document();
        // Memory Milestone M1: activating an evicted RAW re-decodes it to
        // full resolution (the thumbnail shows meanwhile; the swap-in re-enters
        // Develop with its saved settings).
        self.ensure_raw_resident(idx);
    }

    /// Move the active document to the front of the MRU list and drop entries
    /// whose documents no longer exist. Called from refresh_active_document
    /// (covers switch/open/close) and from the few paths that assign
    /// active_doc_idx directly without a refresh.
    pub(crate) fn touch_doc_mru(&mut self) {
        let Some(active_id) = self
            .docs
            .documents
            .get(self.docs.active_doc_idx)
            .map(|d| d.id)
        else {
            return;
        };
        let documents = &self.docs.documents;
        self.docs
            .doc_mru
            .retain(|id| *id != active_id && documents.iter().any(|d| d.id == *id));
        self.docs.doc_mru.insert(0, active_id);
    }

    /// Restore the active document's view and re-sync ALL GPU state (texture
    /// size, uniforms, full recomposite, selection mask) + redraw.
    ///
    /// Separate from switch_to_doc because when CLOSING a tab, active_doc_idx may keep
    /// the same value while the underlying document changed — switch_to_doc returns early
    /// (idx == active) and does NOT refresh → the screen keeps the closed doc's composite
    /// until a click/zoom. This helper always refreshes, with no guard.
    pub fn refresh_active_document(&mut self) {
        let idx = self.docs.active_doc_idx;
        self.touch_doc_mru();

        self.docs.current_file = self.docs.documents[idx].path.clone();

        if self.docs.documents[idx].saved_zoom <= 0.0 {
            self.edit.view.zoom = 1.0;
            self.fit_canvas_to_screen();
        } else {
            self.edit.view.zoom = self.docs.documents[idx].saved_zoom;
            self.edit.view.offset_x = self.docs.documents[idx].saved_offset_x;
            self.edit.view.offset_y = self.docs.documents[idx].saved_offset_y;
        }

        let (w, h) = {
            let d = &self.docs.documents[idx];
            (d.canvas.width, d.canvas.height)
        };
        if let Some(gpu) = &mut self.win.gpu {
            gpu.compositor.crop_preview = None;
            gpu.resize_canvas_texture(w, h);
            gpu.compositor.tile_atlas.clear();
            gpu.compositor.ping_initialized = false;
            gpu.compositor.last_result_is_ping = false;
        }
        // Pick Mode A/B + size the compositor for this canvas before compositing.
        self.sync_compositor_viewport();

        self.edit.transform_state = None;
        self.edit.input.painting = false;
        self.edit.pending_stroke_inputs.clear();
        self.win.pending_gpu_sync = Default::default();

        self.push_canvas_uniforms();
        self.upload_full();
        self.upload_selection_mask();

        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Close document at `idx`.
    /// - If the document has unsaved changes, record it as `pending_close_doc_idx`
    ///   and show the close dialog (the "save?" dialog is already wired in dialogs.rs).
    /// - If clean (or only one document remains and it's clean), close immediately.
    /// - If only one document would remain, replace it with a fresh blank document
    ///   rather than leaving zero docs open.
    pub fn close_doc(&mut self, idx: usize) {
        if idx >= self.docs.documents.len() {
            return;
        }
        if self.jobs.pending_pdf_page_render.is_some() {
            self.shell.status_msg = "Wait for the PDF page to finish rendering".to_string();
            return;
        }
        // Commit before the modified check so freshly typed text counts as an
        // unsaved change (and can't be lost with the closing canvas).
        if self
            .edit
            .text_edit
            .as_ref()
            .is_some_and(|s| s.doc_id == self.docs.documents[idx].id)
        {
            self.commit_text_edit();
        }
        if idx == self.docs.active_doc_idx {
            self.path_style_commit();
        }

        if self.docs.documents[idx].is_modified() {
            self.docs.pending_close_doc_idx = Some(idx);
            self.shell.ui.show_close_dialog = true;
            return;
        }

        self.close_doc_confirmed(idx);
    }

    /// Actually remove the document at `idx` without any further confirmation.
    /// Called when the user clicks "Close without saving" or when the doc is clean.
    pub fn close_doc_confirmed(&mut self, idx: usize) {
        if self.docs.documents.len() == 1 {
            let id = self.docs.documents[idx].id;
            self.jobs.ai_engine.abandon_doc_job(id.0);
            self.jobs.ext.remove_doc_jobs(id.0);
            self.docs.pdf_render_services.remove(&id);
            self.clear_autosave_for(id);
            self.clear_embedded_pdf_for(id);
            self.do_new("Untitled".to_string(), 800, 600, 72.0, 0);
            self.docs.current_file = None;
            self.mark_active_saved();
            self.shell.ui.show_welcome = true;
            self.docs.pending_close_doc_idx = None;
            return;
        }

        let removed = self.docs.documents.remove(idx);
        self.jobs.ai_engine.abandon_doc_job(removed.id.0);
        self.jobs.ext.remove_doc_jobs(removed.id.0);
        self.docs.pdf_render_services.remove(&removed.id);
        self.clear_autosave_for(removed.id);
        self.clear_embedded_pdf_for(removed.id);

        self.docs.doc_mru.retain(|id| *id != removed.id);
        // Return to the most recently used surviving document. When a
        // background tab was closed, the front of the MRU is still the active
        // document, so this also re-resolves its shifted index.
        let mru_idx = self
            .docs
            .doc_mru
            .iter()
            .find_map(|id| self.docs.documents.iter().position(|d| d.id == *id));
        if let Some(mru_idx) = mru_idx {
            self.docs.active_doc_idx = mru_idx;
        } else if self.docs.active_doc_idx >= self.docs.documents.len() {
            self.docs.active_doc_idx = self.docs.documents.len() - 1;
        } else if self.docs.active_doc_idx > idx {
            self.docs.active_doc_idx = self.docs.active_doc_idx.saturating_sub(1);
        }

        self.docs.pending_close_doc_idx = None;
        self.refresh_active_document();
    }

    /// Navigate to a selected page. The UI passes its ordinal within
    /// `selected_pages`; rendering and export use the physical source index.
    pub fn pdf_nav_goto(&mut self, selected_index: usize) {
        // Layer ids are only unique within one canvas; committing before the
        // page's canvas is swapped out keeps the session from landing on an
        // unrelated layer of the next page.
        if self.edit.text_edit.is_some() {
            self.commit_text_edit();
        }
        self.path_style_commit();
        let doc_idx = self.docs.active_doc_idx;
        let Some(pdf) = self.docs.documents[doc_idx].pdf_document.as_ref() else {
            return;
        };
        let Some(&target_index) = pdf.selected_pages.get(selected_index) else {
            return;
        };
        if pdf.active_page == target_index {
            return;
        }

        let cached = self.docs.documents[doc_idx]
            .pdf_document
            .as_mut()
            .and_then(|pdf| pdf.edited_pages.remove(&target_index));
        if let Some(cached) = cached {
            self.replace_active_pdf_page(
                doc_idx,
                target_index,
                cached.canvas,
                cached.reference,
                true,
                (
                    cached.saved_zoom,
                    cached.saved_offset_x,
                    cached.saved_offset_y,
                ),
            );
        } else {
            self.request_pdf_page_switch(doc_idx, target_index);
        }
    }

    pub(crate) fn install_rendered_pdf_page(
        &mut self,
        doc_idx: usize,
        target_index: usize,
        canvas: crate::core::canvas::Canvas,
    ) {
        let Some(pdf) = self.docs.documents[doc_idx].pdf_document.as_ref() else {
            return;
        };
        let mut reference = crate::core::document::PdfPageRef {
            group_id: self.docs.documents[doc_idx]
                .pdf_page
                .as_ref()
                .map_or(0, |page| page.group_id),
            source: pdf.source.clone(),
            index: target_index,
            count: pdf.page_count,
            requested_dpi: pdf.requested_dpi,
            loaded: true,
            original_width: 0,
            original_height: 0,
            original_dpi: pdf.requested_dpi,
            original_layer_id: 0,
            original_tiles_fingerprint: 0,
        };
        reference.record_canvas_baseline(&canvas);
        self.replace_active_pdf_page(
            doc_idx,
            target_index,
            canvas,
            reference,
            false,
            (0.0, 0.0, 0.0),
        );
    }

    fn replace_active_pdf_page(
        &mut self,
        doc_idx: usize,
        target_index: usize,
        canvas: crate::core::canvas::Canvas,
        reference: crate::core::document::PdfPageRef,
        target_differs: bool,
        target_view: (f32, f32, f32),
    ) {
        // Async page installs can also arrive mid-session; commit against the
        // outgoing canvas while the session's layer still exists.
        if self
            .edit
            .text_edit
            .as_ref()
            .is_some_and(|s| s.doc_id == self.docs.documents[doc_idx].id)
        {
            self.commit_text_edit();
        }
        let current_view = (
            self.edit.view.zoom,
            self.edit.view.offset_x,
            self.edit.view.offset_y,
        );
        let doc = &mut self.docs.documents[doc_idx];
        let old_canvas = std::mem::replace(&mut doc.canvas, canvas);
        // The outgoing page's dirt travels with its canvas: the checkpoint is
        // part of that canvas's history, so caching it cannot desync the two.
        let active_dirty = old_canvas.is_dirty();
        let old_reference = doc.pdf_page.replace(reference);
        if let (Some(pdf), Some(old_reference)) = (doc.pdf_document.as_mut(), old_reference) {
            // The outgoing page is cached whenever it differs from a clean render
            // (has edits), regardless of whether those edits were already saved —
            // otherwise navigating back would re-render it clean and lose them.
            pdf.active_page_modified |= active_dirty;
            if pdf.active_page_modified {
                pdf.edited_pages.insert(
                    old_reference.index,
                    crate::core::document::PdfCachedPage {
                        canvas: old_canvas,
                        reference: old_reference,
                        saved_zoom: current_view.0,
                        saved_offset_x: current_view.1,
                        saved_offset_y: current_view.2,
                    },
                );
            }
            pdf.active_page = target_index;
            pdf.active_page_modified = target_differs;
        }
        doc.saved_zoom = target_view.0;
        doc.saved_offset_x = target_view.1;
        doc.saved_offset_y = target_view.2;
        let file_name = doc
            .pdf_page
            .as_ref()
            .and_then(|page| page.source.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or("PDF");
        doc.title = format!("{file_name} - Page {}", target_index + 1);
        doc.rebuild_pdf_global_overlay();
        self.refresh_active_document();
    }

    /// Open a fresh blank document in a new tab and switch to it.
    pub fn open_new_doc_tab(&mut self) {
        let id = DocumentId(self.docs.next_doc_id);
        self.docs.next_doc_id += 1;
        let new_doc = Document::new(id, 800, 600);
        self.docs.documents.push(new_doc);
        let new_idx = self.docs.documents.len() - 1;
        self.switch_to_doc(new_idx);
        self.shell.ui.show_welcome = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_canvas(color: [u8; 4]) -> crate::core::canvas::Canvas {
        crate::core::canvas::Canvas::from_rgba(
            color.into_iter().cycle().take(4 * 4 * 4).collect(),
            4,
            4,
        )
    }

    #[test]
    fn closing_the_active_tab_returns_to_the_most_recently_used_tab() {
        let mut app = App::new();
        // Initial doc id=1 at idx 0; three more tabs → ids 2,3,4 at idx 1,2,3.
        app.open_new_doc_tab();
        app.open_new_doc_tab();
        app.open_new_doc_tab();
        // Paste-tab workflow: work in tab 1 (paste target), then switch to
        // tab 3 to edit another image.
        app.switch_to_doc(1);
        app.switch_to_doc(3);
        // Closing the active tab jumps back to the paste tab, not neighbor 2.
        app.close_doc(3);
        assert_eq!(app.docs.active_doc_idx, 1);
        assert_eq!(
            app.docs.documents[app.docs.active_doc_idx].id,
            DocumentId(2)
        );
        // Closing again walks further down the MRU chain: doc 3 was activated
        // when its tab opened, more recently than doc 1.
        app.close_doc(app.docs.active_doc_idx);
        assert_eq!(
            app.docs.documents[app.docs.active_doc_idx].id,
            DocumentId(3)
        );
    }

    #[test]
    fn closing_a_background_tab_keeps_the_active_document() {
        let mut app = App::new();
        app.open_new_doc_tab(); // id 2 at idx 1
        app.open_new_doc_tab(); // id 3 at idx 2, active
        app.close_doc(0);
        // The active document shifted to idx 1 and must stay active.
        assert_eq!(app.docs.active_doc_idx, 1);
        assert_eq!(
            app.docs.documents[app.docs.active_doc_idx].id,
            DocumentId(3)
        );
    }

    #[test]
    fn app_exit_prompts_each_dirty_tab_and_skips_clean_tabs() {
        let mut app = App::new();
        let first_id = app.docs.documents[0].id;
        app.docs.documents[0].canvas.deselect();

        app.open_new_doc_tab();
        let second_id = app.docs.documents[1].id;
        app.docs.documents[1].canvas.deselect();

        app.open_new_doc_tab(); // clean tab, active when exit is requested
        assert!(!app.request_app_exit());
        assert_eq!(app.docs.pending_exit_docs.len(), 2);
        assert_eq!(app.docs.pending_exit_docs.front(), Some(&first_id));
        assert_eq!(app.docs.documents[app.docs.active_doc_idx].id, first_id);

        app.discard_current_exit_document();
        assert_eq!(app.docs.pending_exit_docs.front(), Some(&second_id));
        assert_eq!(app.docs.documents[app.docs.active_doc_idx].id, second_id);
        assert!(app.shell.ui.show_exit_dialog);

        app.discard_current_exit_document();
        assert!(app.docs.pending_exit_docs.is_empty());
        assert!(app.shell.exit_requested);
        assert!(!app.shell.ui.show_exit_dialog);
    }

    #[test]
    fn cancelling_a_per_tab_exit_prompt_keeps_all_documents_open() {
        let mut app = App::new();
        app.docs.documents[0].canvas.deselect();
        app.open_new_doc_tab();
        app.docs.documents[1].canvas.deselect();

        assert!(!app.request_app_exit());
        app.cancel_app_exit();

        assert_eq!(app.docs.documents.len(), 2);
        assert!(app.docs.documents.iter().all(Document::is_modified));
        assert!(app.docs.pending_exit_docs.is_empty());
        assert!(!app.shell.exit_requested);
        assert!(!app.shell.ui.show_exit_dialog);
    }

    #[test]
    fn one_pdf_document_swaps_and_restores_an_edited_page() {
        let mut app = App::new();
        let source = std::path::PathBuf::from("many-pages.pdf");
        let mut reference = crate::core::document::PdfPageRef {
            group_id: 1,
            source: source.clone(),
            index: 0,
            count: 1_000,
            requested_dpi: 72.0,
            loaded: true,
            original_width: 0,
            original_height: 0,
            original_dpi: 72.0,
            original_layer_id: 0,
            original_tiles_fingerprint: 0,
        };
        app.docs.documents[0].canvas = solid_canvas([220, 10, 10, 255]);
        reference.record_canvas_baseline(&app.docs.documents[0].canvas);
        // Give page 0 a real unsaved edit: dirty is derived from the canvas's
        // own history checkpoint, so it cannot be staged by setting a flag.
        app.docs.documents[0].canvas.deselect();
        app.docs.documents[0].pdf_page = Some(reference);
        app.docs.documents[0].pdf_document = Some(crate::core::document::PdfDocumentState {
            source,
            embedded_source: None,
            page_count: 1_000,
            selected_pages: (0..1_000).collect(),
            selected_pages_saved: (0..1_000).collect(),
            page_names: std::collections::BTreeMap::new(),
            page_names_saved: std::collections::BTreeMap::new(),
            requested_dpi: 72.0,
            active_page: 0,
            active_page_modified: true,
            edited_pages: std::collections::HashMap::new(),
            global_clears: Vec::new(),
            global_clears_saved: Vec::new(),
            global_clears_redo: Vec::new(),
            global_overlay_cache: None,
        });

        app.install_rendered_pdf_page(0, 1, solid_canvas([10, 10, 220, 255]));
        let session = app.docs.documents[0].pdf_document.as_ref().unwrap();
        assert_eq!(app.docs.documents.len(), 1);
        assert_eq!(session.active_page, 1);
        assert!(session.edited_pages.contains_key(&0));
        // Page 0's edits were cached (unsaved); the fresh page 1 is clean, so the
        // active-page indicator is off but the document still has unsaved changes.
        assert!(!app.is_modified());
        assert!(session.edited_pages[&0].canvas.is_dirty());
        assert!(app.docs.documents[0].is_modified());

        // Closing the application from a clean page must still ask to save the
        // edited page cached elsewhere in this PDF session.
        assert!(!app.request_app_exit());
        assert!(app.shell.ui.show_exit_dialog);

        app.pdf_nav_goto(0);
        let session = app.docs.documents[0].pdf_document.as_ref().unwrap();
        assert_eq!(session.active_page, 0);
        assert!(app.is_modified());
        let pixels = app.docs.documents[0].canvas.export_flat();
        assert!(pixels
            .chunks_exact(4)
            .all(|pixel| pixel[0] > 200 && pixel[2] < 20));
    }

    #[test]
    fn blank_page_can_be_inserted_before_the_active_pdf_page() {
        let mut app = App::new();
        let source = std::path::PathBuf::from("book.pdf");
        app.docs.documents[0].canvas = solid_canvas([30, 40, 50, 255]);
        let mut reference = crate::core::document::PdfPageRef {
            group_id: 1,
            source: source.clone(),
            index: 0,
            count: 2,
            requested_dpi: 72.0,
            loaded: true,
            original_width: 0,
            original_height: 0,
            original_dpi: 72.0,
            original_layer_id: 0,
            original_tiles_fingerprint: 0,
        };
        reference.record_canvas_baseline(&app.docs.documents[0].canvas);
        app.docs.documents[0].pdf_page = Some(reference);
        app.docs.documents[0].pdf_document = Some(crate::core::document::PdfDocumentState {
            source,
            embedded_source: None,
            page_count: 2,
            selected_pages: vec![0, 1],
            selected_pages_saved: vec![0, 1],
            page_names: std::collections::BTreeMap::new(),
            page_names_saved: std::collections::BTreeMap::new(),
            requested_dpi: 72.0,
            active_page: 0,
            active_page_modified: false,
            edited_pages: std::collections::HashMap::new(),
            global_clears: Vec::new(),
            global_clears_saved: Vec::new(),
            global_clears_redo: Vec::new(),
            global_overlay_cache: None,
        });

        app.insert_blank_pdf_page(0);

        let pdf = app.docs.documents[0].pdf_document.as_ref().unwrap();
        assert_eq!(pdf.selected_pages, vec![2, 0, 1]);
        assert_eq!(pdf.active_page, 2);
        assert_eq!(
            pdf.page_names.get(&2).map(String::as_str),
            Some("Trang trắng")
        );
        assert!(app.docs.documents[0]
            .canvas
            .export_flat()
            .chunks_exact(4)
            .all(|pixel| pixel == [255, 255, 255, 255]));
        assert!(app.docs.documents[0].is_modified());
    }
}

//! apply_ui_actions handlers: view zoom, whole-image ops (flip/rotate/
//! resize), history and file I/O. Split out of actions.rs (phase 2).

use crate::app::state::App;
use crate::ui::UiActions;
use winit::event_loop::ActiveEventLoop;

impl App {
    /// Returns false when a failed resize aborts the rest of this frame's
    /// actions (mirrors the old early-return in apply_ui_actions).
    pub(super) fn handle_view_image_actions(
        &mut self,
        actions: &mut UiActions,
        event_loop: &ActiveEventLoop,
    ) -> bool {
        if actions.doc.new_flow_text_document {
            actions.doc.new_flow_text_document = false;
            self.open_new_flow_text_doc_tab();
        }
        if actions.doc.pick_flow_text_image {
            actions.doc.pick_flow_text_image = false;
            self.pick_flow_text_image();
        }
        if actions.doc.start_mail_merge {
            actions.doc.start_mail_merge = false;
            self.start_mail_merge();
        }
        if let Some(focus) = actions.doc.flow_text_focus.take() {
            let idx = self.docs.active_doc_idx;
            if let Some(doc) = self.docs.documents.get(idx)
                && doc.is_flow_text()
            {
                crate::ui::document_mode::request_focus(doc.id, focus);
                if let Some(window) = &self.win.window {
                    window.request_redraw();
                }
            }
        }
        if let Some(pattern) = actions.doc.set_mail_merge_pattern.take() {
            self.shell.ui.mail_merge_pattern = pattern;
        }
        if actions.doc.run_mail_merge {
            actions.doc.run_mail_merge = false;
            self.run_mail_merge();
        }
        if actions.doc.cancel_mail_merge {
            actions.doc.cancel_mail_merge = false;
            self.cancel_mail_merge();
        }
        if let Some((doc_id, document)) = actions.doc.replace_flow_text_document.take()
            && let Some(doc) = self.docs.by_id_mut(doc_id)
            && let Some(text) = doc.flow_text.as_mut()
        {
            text.replace_document(document);
        }
        if let Some((doc_id, page_count, active_page)) = actions.doc.set_flow_text_layout.take()
            && let Some(doc) = self.docs.by_id_mut(doc_id)
            && let Some(text) = doc.flow_text.as_mut()
        {
            text.set_layout_page_count(page_count);
            text.set_active_page(active_page);
        }
        let flow_text_active = self.docs.active().is_flow_text();
        if actions.doc.fit_to_screen {
            if flow_text_active {
                self.edit.view.zoom = 1.0;
            } else {
                self.fit_canvas_to_screen();
            }
        }
        if actions.doc.zoom_in {
            let (min_zoom, max_zoom) = if flow_text_active {
                (0.3, 4.0)
            } else {
                (0.02, 64.0)
            };
            self.edit.view.zoom = (self.edit.view.zoom * 1.25).clamp(min_zoom, max_zoom);
            if !flow_text_active {
                self.push_canvas_uniforms();
                self.on_view_changed();
            }
            self.win.last_cursor_radius = 0;
            self.sync_cursor(event_loop);
        }
        if actions.doc.zoom_out {
            let (min_zoom, max_zoom) = if flow_text_active {
                (0.3, 4.0)
            } else {
                (0.02, 64.0)
            };
            self.edit.view.zoom = (self.edit.view.zoom / 1.25).clamp(min_zoom, max_zoom);
            if !flow_text_active {
                self.push_canvas_uniforms();
                self.on_view_changed();
            }
            self.win.last_cursor_radius = 0;
            self.sync_cursor(event_loop);
        }
        if actions.doc.zoom_100 {
            self.edit.view.zoom = 1.0;
            if !flow_text_active {
                self.push_canvas_uniforms();
                self.on_view_changed();
            }
        }
        if let Some(i) = actions.doc.set_active_artboard.take() {
            let active = self.docs.active_mut();
            if let Some(text) = active.flow_text.as_mut() {
                text.set_active_page(i);
            } else {
                self.set_active_artboard(i);
            }
        }
        if let Some(i) = actions.doc.rename_page.take() {
            self.begin_page_rename(i);
        }
        if let Some(text) = actions.doc.page_rename_text.take() {
            self.shell.ui.page_rename_text = text;
        }
        if std::mem::take(&mut actions.doc.page_rename_commit) {
            self.commit_page_rename();
        }
        if std::mem::take(&mut actions.doc.page_rename_cancel) {
            self.shell.ui.page_rename_target = None;
            self.shell.ui.page_rename_text.clear();
        }
        if let Some((from, to)) = actions.doc.move_page.take() {
            self.move_pdf_page(from, to);
        }
        if let Some(i) = actions.doc.delete_page.take() {
            self.delete_pdf_page(i);
        }
        if let Some(position) = actions.doc.insert_pdf_blank.take() {
            self.insert_blank_pdf_page(position);
        }
        if let Some(position) = actions.doc.insert_pdf_files.take() {
            self.begin_pdf_page_insert_dialog(position);
        }
        if let Some(open) = actions.doc.show_pdf_export_dialog.take() {
            self.shell.ui.show_pdf_export_dialog = open;
        }
        if let Some(scope) = actions.doc.set_pdf_export_scope.take() {
            self.shell.ui.pdf_export_scope = scope;
        }
        if let Some(range) = actions.doc.set_pdf_export_range.take() {
            self.shell.ui.pdf_export_range = range;
        }
        if let Some(dpi) = actions.doc.set_pdf_export_dpi.take() {
            self.shell.ui.pdf_export_dpi = dpi;
        }
        if std::mem::take(&mut actions.doc.run_pdf_export) {
            self.run_pdf_export();
        }
        true
    }

    pub(super) fn handle_history_file_actions(
        &mut self,
        actions: &mut UiActions,
        event_loop: &ActiveEventLoop,
    ) {
        let history_nav =
            actions.doc.undo || actions.doc.redo || actions.doc.jump_history.is_some();
        if history_nav {
            // Popping history under a live session would edit the layers the
            // session snapshot still points at. Free Transform consumes Undo
            // as "revert pending"; every other modal refuses it (bell).
            if actions.doc.undo && self.transform_undo_pending() {
                actions.doc.undo = false;
            }
            if (actions.doc.undo || actions.doc.redo || actions.doc.jump_history.is_some())
                && self.modal_lock_active()
            {
                self.deny_modal_action();
                actions.doc.undo = false;
                actions.doc.redo = false;
                actions.doc.jump_history = None;
            }
        }
        // Undo, Redo and a step picked in History: the commands an agent
        // runs.
        if actions.doc.undo {
            let _ = self.run_now("edit.undo");
        }
        if actions.doc.redo {
            let _ = self.run_now("edit.redo");
        }
        if let Some(target_idx) = actions.doc.jump_history.take() {
            let current_idx = self.docs.active().canvas.undo_count();
            let (id, steps) = if target_idx < current_idx {
                ("edit.undo", current_idx - target_idx)
            } else {
                ("edit.redo", target_idx.saturating_sub(current_idx))
            };
            if steps > 0 {
                let _ = self.run_with(id, serde_json::json!({"steps": steps.min(1000)}));
            }
        }

        if actions.doc.new_canvas {
            self.shell.ui.show_new_dialog = true;
            let canvas = &self.docs.active().canvas;
            self.shell.ui.new_dpi = canvas.metadata.resolution_ppi;
            self.shell.ui.new_w_input = crate::core::units::from_pixels(
                canvas.width as f32,
                self.shell.ui.new_unit,
                self.shell.ui.new_dpi,
                0.0,
            );
            self.shell.ui.new_h_input = crate::core::units::from_pixels(
                canvas.height as f32,
                self.shell.ui.new_unit,
                self.shell.ui.new_dpi,
                0.0,
            );
        }
        if actions.doc.open_file {
            self.do_open();
        }
        if let Some(path) = actions.doc.open_recent.take() {
            if path.exists() {
                self.start_load_paths(vec![path]);
            } else {
                self.shell.status_msg =
                    format!("Recent file not found: {}", path.to_string_lossy());
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
        }
        if actions.doc.save {
            self.do_save();
        }
        if actions.doc.save_as {
            self.do_save_as();
        }
        if actions.doc.save_project {
            self.do_save_project();
        }

        if self.shell.exit_requested && self.jobs.pending_file_dialog.is_none() {
            self.shell.exit_requested = false;
            self.clear_all_autosave();
            event_loop.exit();
        }

        if let Some((format, path)) = actions.doc.export_confirmed.take() {
            self.do_export(format, &path);
        }
        if let Some(format) = actions.doc.request_export_path.take() {
            self.do_export_browse(format);
        }
        if let Some(v) = actions.doc.set_export_embed_icc.take() {
            self.shell.ui.export_embed_icc = v;
        }
        if let Some(v) = actions.doc.set_export_resize_enabled.take() {
            self.shell.ui.export_resize_enabled = v;
        }
        if let Some(v) = actions.doc.set_export_resize_long_edge.take() {
            self.shell.ui.export_resize_long_edge = v.clamp(16, 60000);
        }
        if let Some(v) = actions.doc.set_export_output_sharpen.take() {
            self.shell.ui.export_output_sharpen = v.min(100);
        }
        if let Some(mut v) = actions.doc.set_export_pdf_marks.take() {
            v.bleed_mm = v.bleed_mm.clamp(0.0, 20.0);
            self.shell.ui.export_pdf_marks = v;
        }
        self.handle_cmyk_mode_actions(actions);

        // Soft proof (View > Proof) is handled in a separate pass.
    }

    /// Add a new page to the active document (Corel/Excel-style): an independent
    /// blank canvas the same size / DPI / bleed as the current page, made active.
    /// A plain one-page document becomes a two-page one. Pages are separate
    /// canvases shown one at a time, so a document scales to many pages without a
    /// giant side-by-side buffer. A document-structure change (like a PDF
    /// project's pages), not part of the per-page canvas undo history.
    pub fn add_artboard(&mut self) {
        // The page shown changes: full GPU/view resync and a fresh frame (the
        // engine's `PageShown`), the status line from the command.
        let _ = self.run_now("page.add");
    }

    /// Switch the page-tab bar / canvas to page `index`: flush pending edits into
    /// the outgoing page, swap it for the target, resync and reframe. No-op when
    /// already active or out of range.
    pub fn set_active_artboard(&mut self, index: usize) {
        let idx = self.docs.active_doc_idx;
        if index >= self.docs.documents[idx].page_count()
            || index == self.docs.documents[idx].active_artboard
        {
            return;
        }
        let _ = idx;
        // Swapped the active canvas → full GPU/view resync and a fresh frame
        // (the engine's `PageShown`).
        let _ = self.run_with("page.select", serde_json::json!({"page": index}));
    }

    /// Open the page-tab rename dialog for page `index`, pre-filling its current
    /// display name so the user edits rather than retypes.
    pub fn begin_page_rename(&mut self, index: usize) {
        let idx = self.docs.active_doc_idx;
        let Some(doc) = self.docs.documents.get(idx) else {
            return;
        };
        if let Some(pdf) = doc.pdf_document.as_ref() {
            if index >= pdf.selected_pages.len() {
                return;
            }
            self.shell.ui.page_rename_text = pdf.page_display_name(index);
            self.shell.ui.page_rename_target = Some(index);
            return;
        }
        if index >= doc.page_count() {
            return;
        }
        self.shell.ui.page_rename_text = doc.page_display_name(index);
        self.shell.ui.page_rename_target = Some(index);
    }

    /// Apply the page-tab rename dialog's text to its target page, then close it.
    /// Blank text clears the custom name (reverts to the "Trang N" label).
    pub fn commit_page_rename(&mut self) {
        let Some(index) = self.shell.ui.page_rename_target.take() else {
            return;
        };
        let name = self.shell.ui.page_rename_text.trim().to_string();
        self.shell.ui.page_rename_text.clear();
        if let Some(pdf) = self.docs.active_mut().pdf_document.as_mut() {
            let default_label = format!("Trang {}", index + 1);
            let custom = (!name.is_empty() && name != default_label).then_some(name);
            pdf.set_page_name(index, custom);
        } else {
            let _ = self.run_with(
                "page.rename",
                serde_json::json!({"page": index, "name": name}),
            );
        }
    }

    /// Reorder an opened PDF's page (tab context menu "move left / right"):
    /// only the tab order changes, so no view resync is needed.
    fn move_pdf_page(&mut self, from: usize, to: usize) {
        if let Some(pdf) = self.docs.active_mut().pdf_document.as_mut() {
            pdf.move_page(from, to);
            self.shell.status_msg = format!("Đã chuyển trang {}", to + 1);
        }
    }

    fn delete_pdf_page(&mut self, position: usize) {
        if self.jobs.pending_pdf_page_delete.is_some() {
            self.shell.status_msg = "Đang chuyển trang trước khi xóa".to_string();
            return;
        }
        let idx = self.docs.active_doc_idx;
        let Some(pdf) = self.docs.documents[idx].pdf_document.as_ref() else {
            return;
        };
        if pdf.selected_pages.len() <= 1 || position >= pdf.selected_pages.len() {
            return;
        }
        let source_index = pdf.selected_pages[position];
        let active_page = pdf.active_page;
        let doc_id = self.docs.documents[idx].id;

        if source_index != active_page {
            self.finish_pdf_page_delete(doc_id, source_index);
            return;
        }

        // Keep displaying a valid page at every instant. Cached neighbours swap
        // synchronously; clean neighbours render asynchronously and finalize the
        // deletion only after that render succeeds.
        let target_position = if position + 1 < pdf.selected_pages.len() {
            position + 1
        } else {
            position - 1
        };
        self.pdf_nav_goto(target_position);
        let switched = self.docs.documents[idx]
            .pdf_document
            .as_ref()
            .is_some_and(|pdf| pdf.active_page != source_index);
        if switched {
            self.finish_pdf_page_delete(doc_id, source_index);
        } else if self
            .jobs
            .pending_pdf_page_render
            .is_some_and(|(pending_doc, _)| pending_doc == doc_id)
        {
            self.jobs.pending_pdf_page_delete = Some((doc_id, source_index));
            self.shell.status_msg = format!("Đang chuyển khỏi trang {} để xóa…", position + 1);
        }
    }

    pub(crate) fn finish_pdf_page_delete(
        &mut self,
        doc_id: crate::core::document::DocumentId,
        source_index: usize,
    ) {
        let Some(doc_idx) = self.docs.index_of(doc_id) else {
            return;
        };
        let Some(pdf) = self.docs.documents[doc_idx].pdf_document.as_mut() else {
            return;
        };
        if pdf.active_page == source_index {
            return;
        }
        let Some(position) = pdf
            .selected_pages
            .iter()
            .position(|&page| page == source_index)
        else {
            return;
        };
        if pdf.remove_page(position).is_none() {
            return;
        }
        self.shell.ui.page_rename_target = None;
        self.shell.ui.page_rename_text.clear();
        self.shell.status_msg = format!("Đã xoá trang · còn {}", pdf.selected_pages.len());
    }

    /// Image ▸ Mode CMYK actions: open/close the convert dialog (pre-loading the
    /// remembered ICC), pick an ICC, run the RGB→CMYK conversion, or drop back
    /// to RGB. The conversion is destructive (flatten + history clear); the
    /// dialog shows that warning before `convert_cmyk` arrives here.
    fn handle_cmyk_mode_actions(&mut self, actions: &mut UiActions) {
        if let Some(open) = actions.doc.show_cmyk_convert_dialog.take() {
            self.shell.ui.show_cmyk_convert_dialog = open;
            if open && self.shell.ui.cmyk_convert_icc.is_none() {
                // Offer the last-used ICC as the pre-selected profile.
                if let Some(path) = crate::ui::dialogs::load_last_cmyk_icc_path()
                    && let Ok(bytes) = std::fs::read(&path)
                    && crate::core::cms::profile_is_cmyk(&bytes)
                {
                    let name = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("CMYK profile")
                        .to_string();
                    self.shell.ui.cmyk_convert_icc = Some((name, bytes));
                    self.shell.ui.cmyk_convert_icc_path = Some(path.clone());
                    self.shell.ui.cmyk_convert_use_icc = true;
                }
            }
        }

        if let Some(use_icc) = actions.doc.set_cmyk_convert_use_icc.take() {
            self.shell.ui.cmyk_convert_use_icc = use_icc;
        }

        if actions.doc.browse_cmyk_icc
            && let Some(window) = self.win.window.as_ref()
        {
            let parent = crate::file_io::dialog_parent(window);
            let mut dialog = rfd::FileDialog::new().add_filter("ICC Profile", &["icc", "icm"]);
            if let Some(p) = parent {
                dialog = dialog.set_parent(&p);
            }
            if let Some(path) = dialog.pick_file() {
                match std::fs::read(&path) {
                    Ok(bytes) if crate::core::cms::profile_is_cmyk(&bytes) => {
                        let name = path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("CMYK profile")
                            .to_string();
                        self.shell.ui.cmyk_convert_icc = Some((name, bytes));
                        self.shell.ui.cmyk_convert_icc_path = Some(path.clone());
                        self.shell.ui.cmyk_convert_use_icc = true;
                        crate::ui::dialogs::save_last_cmyk_icc_path(&path);
                    }
                    Ok(_) => {
                        self.shell.status_msg =
                            "The selected ICC file is not a CMYK profile".to_string();
                    }
                    Err(e) => {
                        self.shell.status_msg = format!("Could not read ICC file: {e}");
                    }
                }
            }
        }

        if actions.doc.convert_cmyk {
            // The command, with the printer's profile the dialog picked.
            let mut args = serde_json::json!({"color": "cmyk"});
            if let (true, Some(path), Some(fields)) = (
                self.shell.ui.cmyk_convert_use_icc,
                self.shell.ui.cmyk_convert_icc_path.as_ref(),
                args.as_object_mut(),
            ) {
                fields.insert(
                    "icc".to_string(),
                    serde_json::json!({"path": path.display().to_string()}),
                );
            }
            if self.run_with("image.mode", args).is_ok() {
                self.shell.ui.show_channels_panel = true;
                self.shell.ui.show_layer_panel = false;
            }
            self.shell.ui.show_cmyk_convert_dialog = false;
        }
    }
}

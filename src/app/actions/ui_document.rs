//! apply_ui_actions handlers: view zoom, whole-image ops (flip/rotate/
//! resize), history and file I/O. Split out of actions.rs (phase 2).

use crate::app::render::CanvasEvent;
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
            if let Some(doc) = self.docs.documents.get(idx) {
                if doc.is_flow_text() {
                    crate::ui::document_mode::request_focus(doc.id, focus);
                    if let Some(window) = &self.win.window {
                        window.request_redraw();
                    }
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
        if let Some((doc_id, document)) = actions.doc.replace_flow_text_document.take() {
            if let Some(doc) = self.docs.documents.iter_mut().find(|doc| doc.id == doc_id) {
                if let Some(text) = doc.flow_text.as_mut() {
                    text.replace_document(document);
                }
            }
        }
        if let Some((doc_id, page_count, active_page)) = actions.doc.set_flow_text_layout.take() {
            if let Some(doc) = self.docs.documents.iter_mut().find(|doc| doc.id == doc_id) {
                if let Some(text) = doc.flow_text.as_mut() {
                    text.set_layout_page_count(page_count);
                    text.set_active_page(active_page);
                }
            }
        }
        let flow_text_active = self.docs.documents[self.docs.active_doc_idx].is_flow_text();
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
        // Flip and 90° rotate are tile-native (per-layer TileMap::flip_*/rotate_*
        // + offset/mask/selection remap), and flatten_full skips the flat buffer on
        // large canvases — so they run under Viewport Streaming with no >25M px gate.
        if actions.doc.flip_horizontal {
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .flip_horizontal();
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            self.apply_proof_settings();
            self.apply_canvas_event(CanvasEvent::SelectionChanged);
        }
        if actions.doc.flip_vertical {
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .flip_vertical();
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            self.apply_proof_settings();
            self.apply_canvas_event(CanvasEvent::SelectionChanged);
        }
        if actions.doc.rotate_cw {
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .rotate_90_cw();
            if let Some(gpu) = &mut self.win.gpu {
                let w = self.docs.documents[self.docs.active_doc_idx].canvas.width;
                let h = self.docs.documents[self.docs.active_doc_idx].canvas.height;
                gpu.resize_canvas_texture(w, h);
            }
            self.push_canvas_uniforms();
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            self.apply_canvas_event(CanvasEvent::SelectionChanged);
            self.fit_canvas_to_screen();
        }
        if actions.doc.rotate_ccw {
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .rotate_90_ccw();
            if let Some(gpu) = &mut self.win.gpu {
                let w = self.docs.documents[self.docs.active_doc_idx].canvas.width;
                let h = self.docs.documents[self.docs.active_doc_idx].canvas.height;
                gpu.resize_canvas_texture(w, h);
            }
            self.push_canvas_uniforms();
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            self.apply_canvas_event(CanvasEvent::SelectionChanged);
            self.fit_canvas_to_screen();
        }
        if let Some((w, h, _ox, _oy)) = actions.doc.canvas_resize.take() {
            let w = w.max(1);
            let h = h.max(1);
            // Canvas Resize is tile-native (per-layer chunked blit), so it runs
            // under Viewport Streaming with no >25M px gate.
            let resized = self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .resize(w, h);
            if !resized {
                self.shell.status_msg = "Resize bi huy: kich thuoc vuot gioi han".to_string();
                return false;
            }
            if let Some(gpu) = &mut self.win.gpu {
                gpu.resize_canvas_texture(w, h);
            }
            self.push_canvas_uniforms();
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            self.apply_canvas_event(CanvasEvent::SelectionChanged);
            self.fit_canvas_to_screen();
        }
        if let Some(mm) = actions.doc.set_page_bleed_mm.take() {
            self.set_page_setup(Some(mm), None);
        }
        if let Some(mm) = actions.doc.set_page_margin_mm.take() {
            self.set_page_setup(None, Some(mm));
        }
        if actions.doc.add_artboard {
            actions.doc.add_artboard = false;
            self.add_artboard();
        }
        if let Some(i) = actions.doc.set_active_artboard.take() {
            let active = &mut self.docs.documents[self.docs.active_doc_idx];
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
            self.move_page(from, to);
        }
        if let Some(i) = actions.doc.delete_page.take() {
            self.delete_page(i);
        }
        if let Some(position) = actions.doc.insert_pdf_blank.take() {
            self.insert_blank_pdf_page(position);
        }
        if let Some(position) = actions.doc.insert_pdf_files.take() {
            self.begin_pdf_page_insert_dialog(position);
        }
        if std::mem::take(&mut actions.doc.toggle_master_edit) {
            self.toggle_master_edit();
        }
        if std::mem::take(&mut actions.doc.delete_master) {
            self.delete_master();
        }
        if let Some((i, on)) = actions.doc.set_page_use_master.take() {
            self.set_page_use_master(i, on);
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
        if let Some((w, h, dpi)) = actions.doc.image_resize.take() {
            let w = w.max(1);
            let h = h.max(1);
            // Image Size is tile-native (per-layer chunked resample), so it runs
            // under Viewport Streaming with no >25M px gate.
            let resized = self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .resize_image(w, h, dpi);
            if !resized {
                self.shell.status_msg = "Image Size bi huy: kich thuoc vuot gioi han".to_string();
                return false;
            }
            if let Some(gpu) = &mut self.win.gpu {
                gpu.resize_canvas_texture(w, h);
            }
            self.push_canvas_uniforms();
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            self.apply_canvas_event(CanvasEvent::SelectionChanged);
            self.fit_canvas_to_screen();
            self.shell.status_msg = format!("Image Size: {}x{} px", w, h);
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
        if actions.doc.undo {
            self.sync_brush_gpu_to_cpu();
            self.docs.documents[self.docs.active_doc_idx].canvas.undo();
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            self.apply_canvas_event(CanvasEvent::SelectionChanged);
        }
        if actions.doc.redo {
            self.sync_brush_gpu_to_cpu();
            self.docs.documents[self.docs.active_doc_idx].canvas.redo();
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            self.apply_canvas_event(CanvasEvent::SelectionChanged);
        }
        if let Some(target_idx) = actions.doc.jump_history.take() {
            let current_idx = self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .undo_count();
            if target_idx < current_idx {
                self.sync_brush_gpu_to_cpu();
                for _ in 0..(current_idx - target_idx) {
                    self.docs.documents[self.docs.active_doc_idx].canvas.undo();
                }
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
                self.apply_canvas_event(CanvasEvent::SelectionChanged);
            } else if target_idx > current_idx {
                self.sync_brush_gpu_to_cpu();
                for _ in 0..(target_idx - current_idx) {
                    self.docs.documents[self.docs.active_doc_idx].canvas.redo();
                }
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
                self.apply_canvas_event(CanvasEvent::SelectionChanged);
            }
        }

        if actions.doc.new_canvas {
            self.shell.ui.show_new_dialog = true;
            let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
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
        if let Some(profile) = actions.doc.assign_profile.take() {
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .assign_profile(profile);
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            // Assign changes the document tag/encoding. Rebuild the display LUT
            // immediately; otherwise the next frame can interpret the new pixel
            // values through the previous document profile.
            self.apply_proof_settings();
            self.shell.status_msg = format!("Assigned profile: {}", profile.name());
        }
        if let Some(profile) = actions.doc.convert_profile.take() {
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .convert_to_profile(profile);
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            // Conversion stores pixels encoded in the destination profile, so
            // the document-to-display LUT must change in the same UI action.
            self.apply_proof_settings();
            self.shell.status_msg = format!("Converted to profile: {}", profile.name());
        }
        if actions.doc.convert_grayscale {
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .convert_to_grayscale();
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            self.shell.status_msg = "Converted to Grayscale".to_string();
        }
        self.handle_cmyk_mode_actions(actions);
        if let Some(sixteen) = actions.doc.set_bit_depth.take() {
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .set_bit_depth(sixteen);
            self.shell.status_msg = if sixteen {
                "Mode: 16 Bits/Channel".to_string()
            } else {
                "Mode: 8 Bits/Channel".to_string()
            };
        }

        // Soft proof (View > Proof) is handled in a separate pass.
    }

    /// Set the page bleed / safe-margin for the active document as one undoable
    /// step. Each argument is millimetres (`None` leaves that one unchanged);
    /// values convert to document units through the canvas DPI. Recorded as a
    /// [`crate::core::command::PageSetupCommand`] on the canvas history gate, so it
    /// undoes and marks the document dirty. No-op when nothing actually changes.
    pub fn set_page_setup(&mut self, bleed_mm: Option<f32>, margin_mm: Option<f32>) {
        let idx = self.docs.active_doc_idx;
        let dpi = self.docs.documents[idx]
            .canvas
            .metadata
            .resolution_ppi
            .max(1.0);
        let mm_to_px = |mm: f32| mm.max(0.0) / 25.4 * dpi;
        let (cur_bleed, cur_margin, artboards) = {
            let m = &self.docs.documents[idx].canvas.metadata;
            (m.page_bleed_px, m.page_margin_px, m.artboards.clone())
        };
        let bleed_px = bleed_mm.map_or(cur_bleed, mm_to_px);
        let margin_px = margin_mm.map_or(cur_margin, mm_to_px);
        if (bleed_px - cur_bleed).abs() < 1e-3 && (margin_px - cur_margin).abs() < 1e-3 {
            return;
        }
        let _ = self.docs.documents[idx].canvas.execute(
            Box::new(crate::core::command::PageSetupCommand::new(
                "Page setup",
                bleed_px,
                margin_px,
                artboards,
            )),
            crate::core::gateway::ChangeKind::LayerStructure,
        );
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.shell.status_msg = format!(
            "Trang: bleed {:.0}mm · lề an toàn {:.0}mm",
            bleed_px / dpi * 25.4,
            margin_px / dpi * 25.4
        );
    }

    /// Add a new page to the active document (Corel/Excel-style): an independent
    /// blank canvas the same size / DPI / bleed as the current page, made active.
    /// A plain one-page document becomes a two-page one. Pages are separate
    /// canvases shown one at a time, so a document scales to many pages without a
    /// giant side-by-side buffer. A document-structure change (like a PDF
    /// project's pages), not part of the per-page canvas undo history.
    pub fn add_artboard(&mut self) {
        self.sync_brush_gpu_to_cpu();
        let idx = self.docs.active_doc_idx;
        let new_index = self.docs.documents[idx].add_blank_page();
        let count = self.docs.documents[idx].page_count();
        // Swapped the active canvas → full GPU/view resync (same path as a tab or
        // PDF-page switch), then frame the fresh page.
        self.refresh_active_document();
        self.fit_canvas_to_screen();
        self.shell.status_msg = format!("Đã thêm trang {} / {count}", new_index + 1);
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
        self.sync_brush_gpu_to_cpu();
        self.docs.documents[idx].switch_page(index);
        // Swapped the active canvas → full GPU/view resync (same path as a tab or
        // PDF-page switch), then frame the page.
        self.refresh_active_document();
        self.fit_canvas_to_screen();
        self.shell.status_msg = format!("Trang {}", index + 1);
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
        let idx = self.docs.active_doc_idx;
        if let Some(doc) = self.docs.documents.get_mut(idx) {
            let default_label = format!("Trang {}", index + 1);
            let custom = (!name.is_empty() && name != default_label).then_some(name);
            if let Some(pdf) = doc.pdf_document.as_mut() {
                pdf.set_page_name(index, custom);
            } else {
                doc.set_page_name(index, custom);
            }
        }
    }

    /// Reorder a page (tab context menu "move left / right"): keeps the same
    /// active content, only tab order changes, so no view resync is needed.
    pub fn move_page(&mut self, from: usize, to: usize) {
        let idx = self.docs.active_doc_idx;
        if let Some(doc) = self.docs.documents.get_mut(idx) {
            if let Some(pdf) = doc.pdf_document.as_mut() {
                pdf.move_page(from, to);
                self.shell.status_msg = format!("Đã chuyển trang {}", to + 1);
            } else {
                doc.move_page(from, to);
            }
        }
    }

    /// Delete a page (tab context menu). Keeps at least one page; when the active
    /// page is removed a neighbour takes over, so resync the GPU/view like a page
    /// switch. Collapsing back to a single page hides the extra tab.
    pub fn delete_page(&mut self, index: usize) {
        let idx = self.docs.active_doc_idx;
        if self.docs.documents[idx].pdf_document.is_some() {
            self.delete_pdf_page(index);
            return;
        }
        let Some(doc) = self.docs.documents.get_mut(idx) else {
            return;
        };
        if doc.page_count() <= 1 || index >= doc.page_count() {
            return;
        }
        // If the rename dialog targets this or a later page, its index is stale —
        // close it rather than risk renaming the wrong page.
        self.shell.ui.page_rename_target = None;
        self.sync_brush_gpu_to_cpu();
        self.docs.documents[idx].remove_page(index);
        let count = self.docs.documents[idx].page_count();
        // The checked-out canvas may have changed → full GPU/view resync + reframe.
        self.refresh_active_document();
        self.fit_canvas_to_screen();
        self.shell.status_msg = format!("Đã xoá trang · còn {count}");
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
        let Some(doc_idx) = self.docs.documents.iter().position(|doc| doc.id == doc_id) else {
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

    /// Page ▸ Master: enter or leave master-editing. On first use it creates the
    /// shared master page (matching the current page) and checks it out into the
    /// canvas; called again it finishes editing and restores the active page.
    /// Either way the checked-out canvas changes → full GPU/view resync + reframe.
    pub fn toggle_master_edit(&mut self) {
        self.sync_brush_gpu_to_cpu();
        let idx = self.docs.active_doc_idx;
        let Some(doc) = self.docs.documents.get_mut(idx) else {
            return;
        };
        // A PDF-import document is paged by the source, not by artboards — the
        // master feature belongs to true multi-page artboard documents only.
        if doc.pdf_document.is_some() {
            self.shell.status_msg = "Trang nền chỉ dùng cho tài liệu đa trang".to_string();
            return;
        }
        if doc.editing_master {
            doc.exit_master_edit();
            self.refresh_active_document();
            self.fit_canvas_to_screen();
            self.shell.status_msg = "Đã xong trang nền".to_string();
        } else {
            doc.ensure_master();
            doc.enter_master_edit();
            self.refresh_active_document();
            self.fit_canvas_to_screen();
            self.shell.status_msg =
                "Đang chỉnh TRANG NỀN (hiện dưới mọi trang) — bấm lại để xong".to_string();
        }
    }

    /// Page ▸ Master: delete the shared master page. Exits master-editing first, so
    /// the active page is restored and the view resynced.
    pub fn delete_master(&mut self) {
        let idx = self.docs.active_doc_idx;
        let Some(doc) = self.docs.documents.get_mut(idx) else {
            return;
        };
        if !doc.has_master() {
            return;
        }
        let was_editing = doc.editing_master;
        doc.remove_master();
        if was_editing {
            self.refresh_active_document();
            self.fit_canvas_to_screen();
        } else {
            // The master vanished from beneath every page → recomposite.
            self.recomposite();
        }
        self.shell.status_msg = "Đã xoá trang nền".to_string();
    }

    /// Page-tab context menu: toggle whether page `index` shows the master beneath
    /// it. Recomposites when the active page's own setting changed.
    pub fn set_page_use_master(&mut self, index: usize, on: bool) {
        let idx = self.docs.active_doc_idx;
        let Some(doc) = self.docs.documents.get_mut(idx) else {
            return;
        };
        let affects_active = index == doc.active_artboard && !doc.editing_master;
        doc.set_page_use_master(index, on);
        if affects_active {
            self.recomposite();
        }
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
                if let Some(path) = crate::ui::dialogs::load_last_cmyk_icc_path() {
                    if let Ok(bytes) = std::fs::read(&path) {
                        if crate::core::cms::profile_is_cmyk(&bytes) {
                            let name = path
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .unwrap_or("CMYK profile")
                                .to_string();
                            self.shell.ui.cmyk_convert_icc = Some((name, bytes));
                            self.shell.ui.cmyk_convert_use_icc = true;
                        }
                    }
                }
            }
        }

        if let Some(use_icc) = actions.doc.set_cmyk_convert_use_icc.take() {
            self.shell.ui.cmyk_convert_use_icc = use_icc;
        }

        if actions.doc.browse_cmyk_icc {
            if let Some(window) = self.win.window.as_ref() {
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
        }

        if actions.doc.convert_cmyk {
            let profile = if self.shell.ui.cmyk_convert_use_icc {
                match &self.shell.ui.cmyk_convert_icc {
                    Some((name, data)) => crate::core::canvas::CmykProfile::Icc {
                        name: name.clone(),
                        data: data.clone(),
                    },
                    None => crate::core::canvas::CmykProfile::Naive,
                }
            } else {
                crate::core::canvas::CmykProfile::Naive
            };
            match self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .convert_to_cmyk(profile)
            {
                Ok(()) => {
                    self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
                    self.shell.ui.show_channels_panel = true;
                    self.shell.ui.show_layer_panel = false;
                    let name = match &self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .color_mode
                    {
                        crate::core::canvas::ColorMode::Cmyk(p) => p.display_name().to_string(),
                        crate::core::canvas::ColorMode::Rgb => String::new(),
                    };
                    self.shell.status_msg = format!("Mode: CMYK — {name}");
                }
                Err(e) => {
                    self.shell.status_msg = format!("CMYK conversion failed: {e}");
                }
            }
            self.shell.ui.show_cmyk_convert_dialog = false;
        }

        if actions.doc.convert_rgb_mode {
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .convert_to_rgb_mode();
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            self.shell.status_msg = "Mode: RGB".to_string();
        }
    }
}

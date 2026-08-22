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
        if actions.doc.fit_to_screen {
            self.fit_canvas_to_screen();
        }
        if actions.doc.zoom_in {
            self.edit.view.zoom = (self.edit.view.zoom * 1.25).clamp(0.02, 64.0);
            self.push_canvas_uniforms();
            self.on_view_changed();
            self.win.last_cursor_radius = 0;
            self.sync_cursor(event_loop);
        }
        if actions.doc.zoom_out {
            self.edit.view.zoom = (self.edit.view.zoom / 1.25).clamp(0.02, 64.0);
            self.push_canvas_uniforms();
            self.on_view_changed();
            self.win.last_cursor_radius = 0;
            self.sync_cursor(event_loop);
        }
        if actions.doc.zoom_100 {
            self.edit.view.zoom = 1.0;
            self.push_canvas_uniforms();
            self.on_view_changed();
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

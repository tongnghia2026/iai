//! apply_ui_actions handlers: modal dialogs — adjustments, Develop, Warp,
//! filters, Smart Fill, AI panels, plus the New/preset/show_* dialog
//! bookkeeping. Split out of actions.rs (phase 2).

use crate::app::state::App;
use crate::ui::UiActions;

impl App {
    pub(super) fn handle_dialog_actions(
        &mut self,
        actions: &mut UiActions,
        event_loop: &winit::event_loop::ActiveEventLoop,
    ) {
        if let Some(adj) = actions.dialogs.open_adjustment_dialog.take() {
            let cmyk_blocked = !adj.is_ink_native()
                && self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .is_cmyk();
            if !self.begin_adjustment_preview(adj) {
                self.shell.status_msg = if cmyk_blocked {
                    "Điều chỉnh này chưa dùng được ở chế độ CMYK (chỉ Levels/Curves)".to_string()
                } else {
                    "Adjustment requires an unlocked raster layer".to_string()
                };
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
        }
        if let Some(idx) = actions.dialogs.edit_adjustment_layer.take() {
            self.begin_adjustment_layer_edit(idx);
        }
        if let Some(adj) = actions.dialogs.set_adjustment_dialog.take() {
            // Store params immediately (cheap → the dialog/handles stay real-time),
            // but defer the expensive full-layer apply + recomposite to a throttled
            // flush so dragging never stalls on large images.
            self.shell.ui.adjustment_dialog = adj.clone();
            if self.shell.ui.show_adjustment_dialog
                && self.shell.ui.adjustment_preview_enabled
                && (self.shell.adjustment_preview.is_some()
                    || self.edit.adjustment_layer_edit.is_some())
            {
                self.shell.adjustment_preview_pending = Some(adj);
            }
        }
        if let Some(options) = actions.dialogs.set_adjustment_options.take() {
            let options = crate::ui::dialogs::sanitize_adjustment_options(options);
            self.shell.ui.adjustment_options = options;
            crate::ui::dialogs::save_adjustment_options(options);
        }
        if let Some(enabled) = actions.dialogs.set_adjustment_preview_enabled.take() {
            self.set_adjustment_preview_enabled(enabled);
        }
        if let Some(eyedropper) = actions.dialogs.set_adj_eyedropper.take() {
            self.shell.ui.adj_eyedropper = eyedropper;
        }
        if actions.dialogs.apply_adjustment_dialog {
            // Make sure the committed pixels reflect the very latest params, not a
            // throttled-stale preview.
            if let Some(adj) = self.shell.adjustment_preview_pending.take() {
                if self.shell.adjustment_preview.is_some() {
                    self.update_adjustment_preview(adj);
                } else if self.edit.adjustment_layer_edit.is_some() {
                    self.update_adjustment_layer_edit(adj);
                }
            }
            let adj = self.shell.ui.adjustment_dialog.clone();
            self.shell.ui.show_adjustment_dialog = false;
            self.shell.ui.adj_eyedropper = None;
            if self.edit.adjustment_layer_edit.is_some() {
                if self.commit_adjustment_layer_edit(&adj) {
                    self.shell.status_msg = format!("Edited {}", adj.name());
                }
            } else if self.commit_adjustment_preview(&adj) {
                self.shell.status_msg = format!("Applied {}", adj.name());
            } else {
                self.shell.status_msg = "No adjustment changes applied".to_string();
            }
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if actions.dialogs.cancel_adjustment_dialog {
            self.shell.ui.show_adjustment_dialog = false;
            self.shell.ui.adj_eyedropper = None;
            self.shell.adjustment_preview_pending = None;
            if self.edit.adjustment_layer_edit.is_some() {
                self.cancel_adjustment_layer_edit();
            } else {
                self.cancel_adjustment_preview();
            }
        }
        if actions.develop.open_develop_dialog {
            self.open_develop_window(event_loop);
        }
        self.set_develop_controls_pointer_down(actions.develop.develop_controls_pointer_down);
        if let Some(settings) = actions.develop.set_develop_settings.take() {
            self.shell.ui.develop_settings = settings.clone();
            if self.shell.ui.show_develop_dialog && self.dev.develop_preview.is_some() {
                self.update_develop_preview(settings);
            }
        }
        self.apply_develop_panel_actions(actions);
        if actions.develop.apply_develop_dialog {
            let active_id = self.docs.documents[self.docs.active_doc_idx].id;
            if self
                .jobs
                .raw_preview_docs
                .values()
                .any(|id| *id == active_id)
                || self.jobs.raw_preview_failures.contains_key(&active_id)
            {
                actions.develop.apply_develop_dialog = false;
                self.shell.status_msg =
                    "Wait for the RAW decoder to finish, or cancel the failed import".to_string();
            }
        }
        if actions.develop.apply_develop_dialog {
            // (in-canvas fallback dialog only; the Develop window commits via
            // commit_develop_window)
            let settings = self.shell.ui.develop_settings.clone();
            self.shell.ui.show_develop_dialog = false;
            // In the Develop stage, Open commits the document either way (a neutral
            // develop just opens the decoded RAW); outside it, neutral is a no-op.
            // (Fallback in-canvas dialog: single-image session.)
            let active_id = self.docs.documents[self.docs.active_doc_idx].id;
            let developing = self
                .dev
                .develop_session
                .iter()
                .any(|e| e.doc == active_id && e.transient);
            self.dev.develop_session.clear();
            if settings.is_neutral() {
                self.cancel_develop_preview();
                self.shell.status_msg = if developing {
                    "Opened RAW".to_string()
                } else {
                    "No Develop changes applied".to_string()
                };
            } else {
                self.apply_develop_settings_sync(settings);
                if self.commit_develop_preview() {
                    self.shell.status_msg = if developing {
                        "Developed RAW".to_string()
                    } else {
                        "Applied Develop".to_string()
                    };
                } else {
                    self.shell.status_msg = "No Develop changes applied".to_string();
                }
            }
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if actions.develop.cancel_develop_dialog {
            self.shell.ui.show_develop_dialog = false;
            self.cancel_develop_preview();
            // Cancelling the Develop stage discards the transient RAW documents.
            self.discard_develop_session_docs();
        }
        if actions.dialogs.open_warp_dialog {
            if self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .is_cmyk()
            {
                self.shell.status_msg = "Warp chưa dùng được ở chế độ CMYK".to_string();
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            } else if !self.begin_warp() {
                self.shell.status_msg = "Warp requires an unlocked raster layer".to_string();
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
        }
        if let Some(params) = actions.dialogs.set_warp_params.take() {
            self.shell.ui.warp_params = params;
        }
        if actions.dialogs.warp_restore_all {
            self.warp_restore_all();
        }
        if actions.dialogs.apply_warp_dialog {
            self.commit_warp();
        }
        if actions.dialogs.cancel_warp_dialog {
            self.cancel_warp();
        }
        if let Some(filter) = actions.dialogs.open_filter_dialog.take() {
            if self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .is_cmyk()
            {
                self.shell.status_msg = "Bộ lọc chưa dùng được ở chế độ CMYK".to_string();
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            } else if !self.begin_filter_preview(filter) {
                self.shell.status_msg = "Filter requires an unlocked raster layer".to_string();
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
        }
        if let Some(filter) = actions.dialogs.set_filter_dialog.take() {
            self.shell.ui.filter_dialog = filter;
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if let Some(enabled) = actions.dialogs.set_filter_preview_enabled.take() {
            self.set_filter_preview_enabled(enabled);
        }
        if actions.dialogs.apply_filter_preview
            && self.shell.ui.show_filter_dialog
            && self.shell.filter_preview.is_some()
        {
            let filter = self.shell.ui.filter_dialog;
            let proxy_changed = self.update_filter_proxy_preview(filter.clone());
            let canvas_changed = if self.shell.ui.filter_preview_enabled {
                self.update_filter_canvas_preview(filter)
            } else {
                false
            };
            if (proxy_changed || canvas_changed) && self.win.window.is_some() {
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
        }
        if actions.dialogs.apply_filter_dialog {
            let filter = self.shell.ui.filter_dialog;
            self.shell.ui.show_filter_dialog = false;
            if self.commit_filter_preview(&filter) {
                self.shell.status_msg = format!("Applied {}", filter.name());
            } else {
                self.shell.status_msg = "No filter changes applied".to_string();
            }
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if actions.dialogs.cancel_filter_dialog {
            self.shell.ui.show_filter_dialog = false;
            self.cancel_filter_preview();
        }
        if actions.dialogs.smart_fill_fill {
            self.request_smart_fill_fill();
        }
        if let Some(use_ai) = actions.dialogs.apply_smart_fill_fill.take() {
            self.do_smart_fill_fill(use_ai);
        }
        if actions.dialogs.cancel_smart_fill_dialog {
            self.shell.ui.show_smart_fill_dialog = false;
        }
        if actions.dialogs.download_lama_model {
            crate::core::lama::ensure_downloading();
        }
        if let Some(state) = actions.ai.set_ai_panel.take() {
            self.shell.ui.ai = state;
        }
        if let Some(open) = actions.ai.toggle_ai_panel.take() {
            self.shell.ui.show_ai_panel = open;
            if open {
                // Pre-fill the panel from the keys/provider saved on disk.
                if self.shell.ui.ai.api_key.trim().is_empty() {
                    self.shell.ui.ai.api_key = self.jobs.ai_engine.settings.api_key.clone();
                }
                if self.shell.ui.ai.openai_api_key.trim().is_empty() {
                    self.shell.ui.ai.openai_api_key =
                        self.jobs.ai_engine.settings.openai_api_key.clone();
                }
                self.shell.ui.ai.provider = self.jobs.ai_engine.settings.provider;
            }
        }
        if actions.ai.ai_save_key {
            self.jobs.ai_engine.settings.api_key = self.shell.ui.ai.api_key.trim().to_string();
            self.jobs.ai_engine.settings.openai_api_key =
                self.shell.ui.ai.openai_api_key.trim().to_string();
            self.jobs.ai_engine.settings.provider = self.shell.ui.ai.provider;
            self.shell.ui.ai_status = match self.jobs.ai_engine.settings.save() {
                Ok(()) => "Đã lưu API key".to_string(),
                Err(e) => format!("Lỗi lưu key: {e}"),
            };
        }
        if let Some(prompt) = actions.ai.ai_run.take() {
            self.do_ai_edit(prompt);
        }
        if let Some(prompt) = actions.ai.ext_run.take() {
            self.do_ext_edit(prompt);
        }
        if actions.ai.ai_cancel_active {
            self.cancel_active_ai();
        }
        if let Some((paper, kind, gap)) = actions.doc.impose_sheet.take() {
            self.do_impose_sheet(paper, kind, gap);
        }
    }

    /// Develop-panel outputs shared by both hosts of the panel UI: the main
    /// window's fallback in-canvas dialog (via `handle_dialog_actions`) and the
    /// Develop OS window (via `redraw_develop_window`) — presets and local-mask
    /// arm/select bookkeeping.
    pub(crate) fn apply_develop_panel_actions(&mut self, actions: &mut UiActions) {
        if let Some(name) = actions.develop.save_develop_preset.take() {
            let name = name.trim().to_string();
            if !name.is_empty() {
                let preset = crate::core::presets::DevelopPreset {
                    name: name.clone(),
                    settings: self.shell.ui.develop_settings.clone(),
                };
                let list = std::sync::Arc::make_mut(&mut self.shell.develop_presets);
                // Same name overwrites: re-saving a tweaked look must not duplicate.
                if let Some(existing) = list.iter_mut().find(|p| p.name == name) {
                    *existing = preset;
                } else {
                    list.push(preset);
                }
                crate::core::presets::DevelopPreset::save_all(&self.shell.develop_presets);
            }
        }
        if let Some(idx) = actions.develop.delete_develop_preset.take() {
            if idx < self.shell.develop_presets.len() {
                std::sync::Arc::make_mut(&mut self.shell.develop_presets).remove(idx);
                crate::core::presets::DevelopPreset::save_all(&self.shell.develop_presets);
            }
        }
        if let Some(arm) = actions.develop.arm_develop_local.take() {
            self.shell.ui.develop_local_arm = Some(arm);
            self.dev.develop_local_drag = None;
        }
        if actions.develop.disarm_develop_local {
            self.shell.ui.develop_local_arm = None;
            self.dev.develop_local_drag = None;
        }
        if let Some(sel) = actions.develop.select_develop_local.take() {
            self.shell.ui.develop_local_selected = sel;
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if let Some((idx, open)) = actions.develop.set_develop_section_open.take() {
            if idx < crate::ui::develop::DEV_PANEL_SECTIONS
                && self.dev.develop_sections_open[idx] != open
            {
                self.dev.develop_sections_open[idx] = open;
                crate::ui::develop::save_sections_open(&self.dev.develop_sections_open);
            }
        }
        if actions.develop.develop_auto {
            actions.develop.develop_auto = false;
            self.apply_develop_auto();
        }
    }

    /// "Auto" (D4 v1): fit Exposure so the scene's default render lands at a
    /// fixed target brightness — the same bisection that anchors a fresh RAW
    /// render to its embedded preview, here aimed at a canonical "well
    /// exposed" mean instead.
    fn apply_develop_auto(&mut self) {
        // ACR's Auto lands renders in this bright-but-unclipped neighbourhood.
        const DEVELOP_AUTO_TARGET: f32 = 0.45;
        let Some(preview) = &self.dev.develop_preview else {
            return;
        };
        if preview.scene.is_none() || preview.histogram_proxy.is_empty() {
            self.shell.status_msg = "Auto needs a scene-referred session".to_string();
            return;
        }
        let gain = crate::core::develop_scene::baseline_exposure_gain(
            &preview.histogram_proxy,
            DEVELOP_AUTO_TARGET,
        );
        let mut settings = self.shell.ui.develop_settings.clone();
        // The gain is measured from the untouched scene through the DEFAULT
        // tone transform, so it replaces (not offsets) the Exposure slider.
        settings.exposure = gain.log2();
        self.shell.ui.develop_settings = settings.clone();
        self.shell.status_msg = format!("Auto exposure: {:+.2} EV", settings.exposure);
        self.update_develop_preview(settings);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        if let Some(w) = &self.win.develop_window {
            w.request_redraw();
        }
    }

    pub(super) fn handle_misc_dialog_actions(&mut self, actions: &mut UiActions) {
        let created_new_canvas = if let Some((name, w, h, dpi, bg, unit, cmyk)) =
            actions.doc.new_canvas_confirmed.take()
        {
            self.do_new_tab(name, w, h, dpi, bg, unit, cmyk);
            true
        } else {
            false
        };

        if let Some(v) = actions.dialogs.show_new_dialog.take() {
            if v {
                self.open_new_canvas_dialog_with_clipboard_hint();
            } else {
                self.shell.ui.show_new_dialog = false;
                if !created_new_canvas {
                    self.edit.clipboard_image_new_doc_hint = None;
                }
            }
        }
        if let Some(v) = actions.dialogs.show_resize_dialog.take() {
            self.shell.ui.show_resize_dialog = v;
        }
        if let Some(v) = actions.dialogs.show_image_size_dialog.take() {
            self.shell.ui.show_image_size_dialog = v;
        }
        if let Some(v) = actions.dialogs.show_export_dialog.take() {
            self.shell.ui.show_export_dialog = v;
        }
        if let Some(v) = actions.dialogs.show_preferences.take() {
            self.shell.ui.show_preferences = v;
        }
        if let Some(v) = actions.dialogs.show_adjustment_dialog.take() {
            self.shell.ui.show_adjustment_dialog = v;
        }
        if actions.doc.exit_cancel {
            self.cancel_app_exit();
        }
        if actions.doc.exit_discard_current {
            self.discard_current_exit_document();
        }
        if actions.doc.exit_save_current {
            self.shell.ui.show_exit_dialog = false;
            self.do_save_project();
            if self.jobs.pending_file_dialog.is_some() {
                self.shell.exit_save_pending = true;
            } else if !self.docs.documents[self.docs.active_doc_idx].is_modified() {
                self.docs.pending_exit_docs.pop_front();
                self.present_next_exit_document();
            } else {
                // A synchronous write failed; keep this tab in the sweep.
                self.shell.ui.show_exit_dialog = true;
            }
        }
        if let Some(v) = actions.dialogs.show_exit_dialog.take() {
            if !v || !self.block_exit_if_active_operation() {
                self.shell.ui.show_exit_dialog = v;
            }
        }
        if let Some(v) = actions.dialogs.show_close_dialog.take() {
            self.shell.ui.show_close_dialog = v;
        }
        if actions.doc.reload_open_file_confirm {
            self.confirm_reload_open_file();
        }
        if actions.doc.reload_open_file_cancel {
            self.cancel_reload_open_file();
        }
        if let Some((indices, dpi)) = actions.doc.pdf_import_confirm.take() {
            self.confirm_pdf_import(indices, dpi);
        }
        if actions.doc.pdf_import_cancel {
            self.cancel_pdf_import();
        }
        if let Some(target) = actions.doc.pdf_nav_goto.take() {
            self.pdf_nav_goto(target);
        }
        if actions.doc.pdf_nav_export {
            self.pdf_nav_export();
        }
        if let Some(v) = actions.sel.show_feather_dialog.take() {
            self.shell.ui.show_feather_dialog = v;
        }
        if let Some(v) = actions.sel.show_stroke_dialog.take() {
            self.shell.ui.show_stroke_dialog = v;
        }
        if let Some(params) = actions.sel.apply_stroke.take() {
            self.edit.pending_stroke = Some(params);
        }

        if let Some(true) = actions.doc.close_file_without_saving.take() {
            // Save & Close runs the save first (handled earlier); if that opened a
            // dialog (e.g. first project save), defer the close until it finishes.
            if self.jobs.pending_file_dialog.is_some() {
                self.shell.close_requested = true;
            } else {
                self.execute_close();
            }
        }
        if let Some(v) = actions.chrome.show_welcome.take() {
            self.shell.ui.show_welcome = v;
            // The welcome screen and the Library grid are mutually exclusive.
            if v {
                self.shell.ui.show_library = false;
            }
        }
        if let Some((v, idx)) = actions.dialogs.show_rename_dialog.take() {
            self.shell.ui.show_rename_dialog = v;
            if v {
                self.shell.ui.rename_idx = idx;
                self.shell.ui.rename_text = self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers
                    .get(idx)
                    .map(|l| l.name.clone())
                    .unwrap_or_default();
            }
        }
        if let Some(v) = actions.doc.new_w_input.take() {
            self.shell.ui.new_w_input = v;
        }
        if let Some(v) = actions.doc.new_h_input.take() {
            self.shell.ui.new_h_input = v;
        }
        if let Some(v) = actions.doc.new_dpi.take() {
            self.shell.ui.new_dpi = v;
        }
        if let Some(v) = actions.doc.new_bg_color.take() {
            self.shell.ui.new_bg_color = v;
        }
        if let Some(v) = actions.doc.new_name.take() {
            self.shell.ui.new_name = v;
        }
        if let Some(v) = actions.doc.new_unit.take() {
            self.shell.ui.new_unit = v;
        }
        if let Some(v) = actions.doc.set_canvas_unit.take() {
            self.shell.canvas_unit = v;
        }
        if let Some(t) = actions.doc.rename_text.take() {
            self.shell.ui.rename_text = t;
        }
        if let Some(f) = actions.doc.export.take() {
            self.shell.ui.export_format = f;
        }
        if let Some(interp) = actions.tool.set_transform_interpolation.take() {
            self.shell.ui.transform_interpolation = interp;
        }

        if let Some((name, width, height, unit, dpi)) = actions.dialogs.save_preset.take() {
            let preset = crate::core::presets::SizePreset {
                name,
                width,
                height,
                unit,
                dpi,
            };
            std::sync::Arc::make_mut(&mut self.shell.user_presets).push(preset);
            crate::core::presets::SizePreset::save_all(&self.shell.user_presets);
        }
        if actions.dialogs.open_delete_preset_dialog {
            self.shell.ui.show_delete_preset_dialog = true;
        }
        if actions.dialogs.close_delete_preset_dialog {
            self.shell.ui.show_delete_preset_dialog = false;
        }
        if let Some(idx) = actions.dialogs.delete_preset.take() {
            if idx < self.shell.user_presets.len() {
                std::sync::Arc::make_mut(&mut self.shell.user_presets).remove(idx);
                crate::core::presets::SizePreset::save_all(&self.shell.user_presets);
            }
            if self.shell.user_presets.is_empty() {
                self.shell.ui.show_delete_preset_dialog = false;
            }
        }
        if let Some((w, h, unit, dpi)) = actions.dialogs.open_preset_dialog.take() {
            let auto_name = if unit == "px" {
                format!("{:.0}x{:.0}px @{:.0}DPI", w, h, dpi)
            } else {
                format!("{:.2}x{:.2}{} @{:.0}DPI", w, h, unit, dpi)
            };
            self.shell.ui.show_preset_dialog = true;
            self.shell.ui.preset_dialog_name = auto_name;
            self.shell.ui.preset_dialog_w = w;
            self.shell.ui.preset_dialog_h = h;
            self.shell.ui.preset_dialog_unit = unit;
            self.shell.ui.preset_dialog_dpi = dpi;
        }
        if let Some(name) = actions.dialogs.preset_dialog_name_changed.take() {
            self.shell.ui.preset_dialog_name = name;
        }
        if actions.dialogs.preset_dialog_confirm {
            let name = self.shell.ui.preset_dialog_name.trim().to_string();
            if !name.is_empty() {
                let preset = crate::core::presets::SizePreset {
                    name,
                    width: self.shell.ui.preset_dialog_w,
                    height: self.shell.ui.preset_dialog_h,
                    unit: self.shell.ui.preset_dialog_unit.clone(),
                    dpi: self.shell.ui.preset_dialog_dpi,
                };
                std::sync::Arc::make_mut(&mut self.shell.user_presets).push(preset);
                crate::core::presets::SizePreset::save_all(&self.shell.user_presets);
            }
            self.shell.ui.show_preset_dialog = false;
        }
        if actions.dialogs.preset_dialog_cancel {
            self.shell.ui.show_preset_dialog = false;
        }

        if let Some((name, channels)) = actions.dialogs.save_levels_preset.take() {
            let presets = std::sync::Arc::make_mut(&mut self.shell.adjustment_presets);
            presets.upsert_levels(name, channels);
            presets.save();
        }
        if let Some(idx) = actions.dialogs.delete_levels_preset.take() {
            let presets = std::sync::Arc::make_mut(&mut self.shell.adjustment_presets);
            if idx < presets.levels.len() {
                presets.levels.remove(idx);
                presets.save();
            }
        }
        if let Some((name, channels)) = actions.dialogs.save_curves_preset.take() {
            let presets = std::sync::Arc::make_mut(&mut self.shell.adjustment_presets);
            presets.upsert_curves(name, channels);
            presets.save();
        }
        if let Some(idx) = actions.dialogs.delete_curves_preset.take() {
            let presets = std::sync::Arc::make_mut(&mut self.shell.adjustment_presets);
            if idx < presets.curves.len() {
                presets.curves.remove(idx);
                presets.save();
            }
        }
    }
}

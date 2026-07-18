use super::state::App;
use crate::ui::UiActions;
use winit::event_loop::ActiveEventLoop;
use winit::window::CursorIcon;

mod adjustments;
mod ai;
mod channels;
mod clipboard;
mod develop;
mod edit_ops;
mod filters;
mod impose;
mod print;
mod refine;
mod ui_chrome;
mod ui_color_print;
mod ui_data;
mod ui_dialogs;
mod ui_document;
mod ui_layers;
mod ui_selection;
mod ui_tools;

impl App {
    fn set_refine_overlay_tint(&mut self, color: [u8; 4]) {
        self.edit.refine_overlay_color = color;
        self.edit.refine_overlay_tex = None;
        self.edit.refine_overlay_mask_rev = u64::MAX;
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    fn set_paint_color(&mut self, target: u8, color: [u8; 4]) {
        match target {
            1 => {
                self.edit.bg_color = color;
                self.edit.tools.eraser_mut().bg_color = color;
            }
            2 => {
                // While editing, colour applies to the current selection (or
                // pending caret) before updating the default so unselected
                // characters keep their prior colour.
                if self.edit.text_edit.is_some() {
                    self.apply_text_style_to_selection(Some(color), None, None, None, None, None);
                }
                self.edit.text_color = color;
                self.edit.fg_color = color;
            }
            3 => {
                self.edit.tools.shape_mut().fill_color = color;
                self.update_selected_shape_style();
            }
            4 => {
                self.edit.tools.shape_mut().stroke_color = color;
                self.update_selected_shape_style();
            }
            _ => {
                self.edit.tools.brush_mut().settings.color = color;
                self.edit.tools.fill_mut().color = color;
                self.edit.fg_color = color;
            }
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Sample the canvas colour at the given canvas-space point and set it as the
    /// foreground colour. Used by the Alt-hold temporary eyedropper while a paint
    /// tool is active (reuses the Eyedropper tool's sample settings).
    pub fn pick_color_at(&mut self, cx: f32, cy: f32) {
        let cw = self.docs.documents[self.docs.active_doc_idx].canvas.width as f32;
        let ch = self.docs.documents[self.docs.active_doc_idx].canvas.height as f32;
        if cx < 0.0 || cy < 0.0 || cx >= cw || cy >= ch {
            return;
        }
        self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .ensure_pixels();
        let color = self.edit.tools.eyedropper().sample(
            &self.docs.documents[self.docs.active_doc_idx].canvas,
            cx as u32,
            cy as u32,
        );
        if self.shell.ui.show_paint_color_dialog {
            self.shell.ui.paint_color_dialog_color = color;
            if self.shell.ui.paint_color_dialog_live_preview {
                self.set_paint_color(self.shell.ui.paint_color_dialog_target, color);
            }
        } else {
            self.set_paint_color(0, color);
        }
        self.shell.status_msg = format!("Picked #{:02X}{:02X}{:02X}", color[0], color[1], color[2]);
    }

    pub fn apply_ui_actions(&mut self, mut actions: UiActions, event_loop: &ActiveEventLoop) {
        self.edit.input.paint_dialog_hovered = actions.dialogs.paint_dialog_hovered;
        self.poll_filter_preview();
        self.poll_develop_preview();
        // Off-thread Shape bakes: land a finished worker raster, then bake any
        // deferred style scrub (Radius/Stroke) so the last tick always lands
        // even after the pointer goes idle.
        self.poll_shape_bake();
        self.flush_pending_shape_style();

        // Strict modal lock (standard design-app behavior): while a modal
        // operation or dialog is open, features outside its scope are refused
        // with the system bell until the user commits or cancels.
        if self.modal_lock_active() {
            let mut denied = actions.chrome.modal_denied;
            denied |= actions.doc.switch_doc.take().is_some();
            denied |= actions.doc.close_doc_tab.take().is_some();
            denied |= std::mem::take(&mut actions.doc.new_doc_tab);
            denied |= std::mem::take(&mut actions.doc.open_file);
            denied |= std::mem::take(&mut actions.tool.start_transform);
            denied |= actions.tool.select_tool.take().is_some();
            denied |= actions.tool.edit_text_layer.take().is_some();
            denied |= actions.tool.text_canvas_click.take().is_some();
            denied |= actions.doc.pdf_nav_goto.take().is_some();
            denied |= std::mem::take(&mut actions.doc.pdf_nav_export);
            if actions.dialogs.show_new_dialog == Some(true) {
                actions.dialogs.show_new_dialog = None;
                denied = true;
            }
            if actions.chrome.show_welcome == Some(true) {
                actions.chrome.show_welcome = None;
                denied = true;
            }
            if denied {
                self.deny_modal_action();
            }
        }

        self.handle_chrome_actions(&mut actions);
        self.handle_text_actions(&mut actions);
        self.handle_transform_crop_actions(&mut actions, event_loop);
        self.handle_tool_color_actions(&mut actions, event_loop);
        self.handle_dialog_actions(&mut actions, event_loop);
        self.handle_direct_adjustment_actions(&mut actions);
        self.handle_tool_options_actions(&mut actions, event_loop);

        if actions.doc.exit && !self.block_exit_if_active_operation() {
            self.shell.exit_requested = true;
        }

        self.handle_layer_actions(&mut actions);
        if !self.handle_view_image_actions(&mut actions, event_loop) {
            return;
        }
        self.handle_history_file_actions(&mut actions, event_loop);
        self.handle_color_print_actions(&mut actions);
        self.handle_misc_dialog_actions(&mut actions);
        self.handle_panel_guide_actions(&mut actions);
        self.handle_channel_actions(&mut actions);
        self.handle_selection_refine_actions(&mut actions);

        if actions.chrome.cursor_left {
            if let Some(w) = &self.win.window {
                w.set_cursor_visible(true);
                w.set_cursor(CursorIcon::Default);
            }
        }
        if actions.chrome.cursor_entered {
            self.sync_cursor(event_loop);
        }

        if let Some(idx) = actions.doc.switch_doc {
            self.switch_to_doc(idx);
        }
        if let Some(idx) = actions.doc.close_doc_tab {
            self.close_doc(idx);
        }
        if actions.doc.new_doc_tab {
            self.open_new_doc_tab();
        }

        // Dirty itself is derived from each document's saved checkpoint. Only the
        // sticky "this PDF page has been edited" flag needs latching, because it
        // survives a save and so cannot be derived from one.
        if !self.docs.documents.is_empty() {
            self.docs.documents[self.docs.active_doc_idx].reconcile_pdf_page_modified();
        }
    }
}

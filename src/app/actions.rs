use super::state::App;
use crate::ui::UiActions;
use winit::event_loop::ActiveEventLoop;

#[cfg(feature = "pro")]
pub(crate) use pro::ProDoor;

mod adjustments;
mod ai;
#[cfg(feature = "pro")]
#[rustfmt::skip]
mod ai_studio;
#[cfg(feature = "pro")]
#[rustfmt::skip]
mod ai_window;
mod author_mail;
mod channels;
mod clipboard;
mod develop;
mod edit_ops;
mod filters;
#[cfg(feature = "pro")]
#[rustfmt::skip]
pub(in crate::app) mod impose;
mod print;
#[cfg(feature = "pro")]
#[rustfmt::skip]
mod pro;
mod refine;
mod ui_chrome;
mod ui_color_print;
mod ui_data;
#[cfg(feature = "pro")]
pub(in crate::app) use ui_data::premultiply_for_linear_target;
mod ui_dialogs;
mod ui_document;
mod ui_layers;
mod ui_selection;
mod ui_tools;
mod update;

/// Status line when Merge Down / Merge Selected is refused.
pub(crate) const MERGE_REFUSED_MSG: &str =
    "Không gộp được: không có layer phù hợp bên dưới (nhóm, adjustment hoặc đang khóa)";

fn should_ring_modal_denial(denied: bool, text_input_frame: bool) -> bool {
    denied && !text_input_frame
}

/// True while a keyboard/IME edit is recent enough that any incidental modal
/// denial this frame belongs to that edit and must stay silent. A time window
/// (rather than a one-shot per-frame flag) because the denial can surface a
/// frame or two after the input event.
fn within_text_input_window(until: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    until.is_some_and(|until| now < until)
}

impl App {
    fn set_refine_overlay_tint(&mut self, color: [u8; 4]) {
        self.edit.refine_overlay_color = color;
        self.invalidate_refine_overlay();
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
                self.update_selected_shape_style(true, false, false);
            }
            4 => {
                self.edit.tools.shape_mut().stroke_color = color;
                self.update_selected_shape_style(false, true, false);
            }
            5 => self.path_set_fill_color(color),
            6 => self.path_set_stroke_color(color),
            7 => self.path_set_fill_end_color(color),
            8..=15 => self.path_set_gradient_stop_color(target - 8, color),
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
        let cw = self.docs.active().canvas.width as f32;
        let ch = self.docs.active().canvas.height as f32;
        if cx < 0.0 || cy < 0.0 || cx >= cw || cy >= ch {
            return;
        }
        self.docs.active_mut().canvas.ensure_pixels();
        let color =
            self.edit
                .tools
                .eyedropper()
                .sample(&self.docs.active().canvas, cx as u32, cy as u32);
        if self.shell.ui.show_paint_color_dialog {
            self.shell.ui.paint_color_dialog_color = color;
            if self.shell.ui.paint_color_dialog_live_preview {
                self.set_paint_color(self.shell.ui.paint_color_dialog_target, color);
            }
        } else {
            self.edit.tools.eyedropper_mut().remember_color(color);
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
        // Off-thread Path bakes (live scale/rotate & node drags).
        self.poll_path_bake();
        // Off-thread crisp-Path display overlay bakes (zoom-bucket changes).
        self.poll_display_bake();

        // A tool picked while Auto retouch is open. The view tools leave a
        // retouch under way alone; any other has it applied as it stands
        // first (the dialog stays open), so the tool works on what shows.
        if self.shell.ui.show_portrait_dialog {
            use crate::tools::ToolId;
            match actions.tool.select_tool {
                Some(id @ (ToolId::Hand | ToolId::Zoom)) => {
                    if self.portrait_under_way() {
                        actions.tool.select_tool = None;
                        self.edit.tools.select(id);
                        self.edit.arrow_multi_layer = None;
                        self.sync_cursor(event_loop);
                    }
                }
                Some(id) => self.portrait_tool_picked(id == ToolId::Crop),
                None => {}
            }
        }
        // So has any other command from outside the dialog.
        self.portrait_gives_way(&mut actions);

        // Strict modal lock (standard design-app behavior): while a modal
        // operation or dialog is open, features outside its scope are refused
        // with the system bell until the user commits or cancels.
        let text_input_frame =
            within_text_input_window(self.win.text_input_quiet_until, std::time::Instant::now());
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
            if actions.dialogs.show_new_dialog == Some(true) {
                actions.dialogs.show_new_dialog = None;
                denied = true;
            }
            if actions.chrome.show_welcome == Some(true) {
                actions.chrome.show_welcome = None;
                denied = true;
            }
            if actions.chrome.show_library == Some(true) {
                actions.chrome.show_library = None;
                denied = true;
            }
            // Keyboard/IME events owned by the live TextEdit can make egui emit
            // incidental UI bookkeeping in the same frame. They are not an
            // attempted action outside the modal and must never ring Windows'
            // attention bell. Pointer/menu/tab denials arrive in other frames;
            // Ctrl+N is intercepted before egui and keeps its explicit denial.
            if should_ring_modal_denial(denied, text_input_frame) {
                self.deny_modal_action();
            }
        }

        self.handle_chrome_actions(&mut actions);
        self.handle_library_actions(&mut actions);
        self.handle_text_actions(&mut actions);
        self.handle_transform_crop_actions(&mut actions, event_loop);
        self.handle_tool_color_actions(&mut actions, event_loop);
        self.handle_dialog_actions(&mut actions, event_loop);
        self.handle_direct_adjustment_actions(&mut actions);
        self.handle_tool_options_actions(&mut actions, event_loop);

        self.handle_pro_actions(&mut actions);
        self.handle_update_actions(&mut actions);
        self.handle_author_mail_actions(&mut actions);
        if actions.doc.exit && self.request_app_exit() {
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

        if actions.chrome.cursor_left || actions.chrome.cursor_entered {
            self.refresh_pointer_ui_state(self.edit.input.mouse_x, self.edit.input.mouse_y);
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
        if let Some(doc) = actions
            .doc
            .hovered_doc
            .and_then(|idx| self.docs.documents.get(idx))
        {
            if self.shell.ui_data_cache.doc_thumbs.request(doc) {
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
        }

        // Dirty itself is derived from each document's saved checkpoint. Only the
        // sticky "this PDF page has been edited" flag needs latching, because it
        // survives a save and so cannot be derived from one.
        if !self.docs.documents.is_empty() {
            self.docs.active_mut().reconcile_pdf_page_modified();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{should_ring_modal_denial, within_text_input_window};
    use std::time::{Duration, Instant};

    #[test]
    fn text_input_frames_never_ring_the_modal_bell() {
        assert!(!should_ring_modal_denial(true, true));
        assert!(should_ring_modal_denial(true, false));
        assert!(!should_ring_modal_denial(false, false));
    }

    #[test]
    fn text_input_window_covers_frames_just_after_a_keystroke() {
        let now = Instant::now();
        // No recent text input: denials ring.
        assert!(!within_text_input_window(None, now));
        // A keystroke set the window 250ms ahead; a denial one frame later
        // (still inside the window) stays silent, unlike the old one-shot flag.
        let until = now + Duration::from_millis(250);
        assert!(within_text_input_window(
            Some(until),
            now + Duration::from_millis(30)
        ));
        // Well past the window (a deliberate click after typing stopped): rings.
        assert!(!within_text_input_window(
            Some(until),
            now + Duration::from_millis(400)
        ));
    }
}

/// Menu actions that now run the command an agent runs: the same change,
/// one undo step, as before.
#[cfg(test)]
mod menu_runs_commands {
    use crate::app::state::App;
    use crate::core::canvas::Canvas;
    use crate::ui::UiActions;

    /// A 40 × 20 page, dark grey at the left, light grey at the right.
    fn two_greys() -> App {
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        let (w, h) = (40u32, 20u32);
        let rgba: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let v = if i % w < w / 2 { 64 } else { 160 };
                [v, v, v, 255]
            })
            .collect();
        app.docs.documents[0].canvas = Canvas::from_rgba(rgba, w, h);
        app
    }

    fn red_at(app: &App, x: u32, y: u32) -> u8 {
        let canvas = &app.docs.active().canvas;
        canvas.flatten_for_export()[((y * canvas.width + x) * 4) as usize]
    }

    #[test]
    fn select_modify_auto_levels_and_flatten_from_the_menu() {
        let mut app = two_greys();
        app.run_with(
            "select.rect",
            serde_json::json!({"rect": {"x": 0, "y": 0, "w": 10, "h": 10}}),
        )
        .unwrap();
        let mut actions = UiActions::default();
        actions.sel.selection_grow = Some(3);
        actions.sel.selection_feather = Some(0.0);
        app.handle_selection_refine_actions(&mut actions);
        let (x0, _, x1, _) = app.docs.active_mut().canvas.selection.bounding_box();
        assert_eq!((x0, x1), (0.0, 13.0));

        app.run_now("select.none").unwrap();
        app.do_auto_levels();
        assert_eq!(
            (red_at(&app, 5, 15), red_at(&app, 35, 15)),
            (0, 255),
            "{}",
            app.shell.status_msg
        );
        assert!(app.docs.active_mut().canvas.can_undo());

        app.run_with("layer.new", serde_json::json!({})).unwrap();
        assert_eq!(app.docs.active().canvas.layer_stack.layers.len(), 2);
        let mut actions = UiActions::default();
        actions.layers.merge_all = true;
        app.handle_layer_actions(&mut actions);
        assert_eq!(app.docs.active().canvas.layer_stack.layers.len(), 1);
        assert_eq!(app.shell.status_msg, "Đã gộp thành một lớp");
    }
}

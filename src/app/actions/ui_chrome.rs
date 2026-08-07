//! apply_ui_actions handlers: window chrome — theme, toolbox, panel and
//! ruler/guide toggles. Split out of actions.rs (phase 2).

use crate::app::state::{App, GuideOp};
use crate::core::document::{Guide, GuideOrientation};
use crate::ui::UiActions;

impl App {
    pub(super) fn handle_chrome_actions(&mut self, actions: &mut UiActions) {
        if let Some(mode) = actions.chrome.set_theme_mode.take() {
            if self.shell.ui.theme_mode != mode {
                self.shell.ui.theme_mode = mode;
                crate::ui::theme::save_theme_mode(mode);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
        }
    }

    pub(super) fn handle_panel_guide_actions(&mut self, actions: &mut UiActions) {
        if actions.chrome.toggle_color_panel {
            self.shell.ui.show_color_panel = !self.shell.ui.show_color_panel;
        }
        if actions.chrome.toggle_text_panel {
            self.shell.ui.show_text_panel = !self.shell.ui.show_text_panel;
        }
        if let Some(show) = actions.chrome.show_text_panel.take() {
            self.shell.ui.show_text_panel = show;
        }
        if actions.chrome.toggle_layer_panel {
            if self.shell.ui.show_layer_panel {
                self.shell.ui.show_layer_panel = false;
            } else {
                self.shell.ui.show_layer_panel = true;
                self.shell.ui.show_channels_panel = false;
            }
        }
        if let Some(show) = actions.chrome.show_layer_panel.take() {
            self.shell.ui.show_layer_panel = show;
            if show {
                self.shell.ui.show_channels_panel = false;
            }
        }
        if actions.chrome.toggle_history_panel {
            self.shell.ui.show_history_panel = !self.shell.ui.show_history_panel;
        }
        if actions.chrome.toggle_info_panel {
            self.shell.ui.show_info_panel = !self.shell.ui.show_info_panel;
        }
        if actions.chrome.toggle_channels_panel {
            if self.shell.ui.show_channels_panel {
                self.shell.ui.show_channels_panel = false;
            } else {
                self.shell.ui.show_channels_panel = true;
                self.shell.ui.show_layer_panel = false;
            }
        }
        if let Some(show) = actions.chrome.show_channels_panel.take() {
            self.shell.ui.show_channels_panel = show;
            if show {
                self.shell.ui.show_layer_panel = false;
            }
        }
        if actions.chrome.toggle_rulers {
            self.shell.ui.show_rulers = !self.shell.ui.show_rulers;
        }
        if actions.chrome.toggle_show_guides {
            self.shell.ui.show_guides = !self.shell.ui.show_guides;
        }
        if actions.chrome.toggle_lock_guides {
            self.shell.ui.lock_guides = !self.shell.ui.lock_guides;
        }
        if actions.chrome.toggle_snap {
            self.shell.ui.snap_enabled = !self.shell.ui.snap_enabled;
            let on = self.shell.ui.snap_enabled;
            self.edit.tools.move_tool_mut().snap_enabled = on;
            // The vector drawing tools share the same master Snap toggle so an
            // arrow / line joins other objects at their corners and edges.
            self.edit.tools.arrow_mut().snap_enabled = on;
            self.edit.tools.pen_mut().snap_enabled = on;
        }
        if actions.chrome.clear_guides {
            self.docs.documents[self.docs.active_doc_idx].guides.clear();
            self.edit.guide_op = None;
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        // Dragging a new guide out of a ruler: keep a snapped preview alive.
        if let Some((orientation, pos)) = actions.chrome.ruler_guide_drag.take() {
            let snapped = self.snap_guide_pos(orientation, pos, None);
            self.edit.guide_op = Some(GuideOp::Create {
                orientation,
                pos: snapped,
            });
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        // Ruler drag released → commit the guide if it landed on the canvas.
        if actions.chrome.ruler_guide_commit {
            if let Some(GuideOp::Create { orientation, pos }) = self.edit.guide_op {
                let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
                let dim = match orientation {
                    GuideOrientation::Horizontal => canvas.height as f32,
                    GuideOrientation::Vertical => canvas.width as f32,
                };
                if pos >= 0.0 && pos <= dim {
                    self.docs.documents[self.docs.active_doc_idx]
                        .guides
                        .push(Guide { orientation, pos });
                }
                self.edit.guide_op = None;
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
        }
    }
}

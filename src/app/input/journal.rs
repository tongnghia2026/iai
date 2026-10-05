//! What the flight recorder's journal is told about the user's input and the
//! state it lands on (see `crate::diag`).

use crate::app::state::App;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

/// A press on the canvas was journalled, so its release is too.
static CANVAS_PRESS: AtomicBool = AtomicBool::new(false);

fn is_modifier(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::ShiftLeft
            | KeyCode::ShiftRight
            | KeyCode::ControlLeft
            | KeyCode::ControlRight
            | KeyCode::AltLeft
            | KeyCode::AltRight
            | KeyCode::SuperLeft
            | KeyCode::SuperRight
    )
}

/// Keys that say something even while the user is typing into a field.
fn speaks_while_typing(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Enter
            | KeyCode::NumpadEnter
            | KeyCode::Escape
            | KeyCode::Tab
            | KeyCode::F1
            | KeyCode::F2
            | KeyCode::F3
            | KeyCode::F4
            | KeyCode::F5
            | KeyCode::F6
            | KeyCode::F7
            | KeyCode::F8
            | KeyCode::F9
            | KeyCode::F10
            | KeyCode::F11
            | KeyCode::F12
    )
}

fn chord(ctrl: bool, shift: bool, alt: bool, code: KeyCode) -> String {
    let name = format!("{code:?}");
    let name = name
        .strip_prefix("Key")
        .or_else(|| name.strip_prefix("Digit"))
        .unwrap_or(&name);
    format!(
        "{}{}{}{name}",
        if ctrl { "Ctrl+" } else { "" },
        if shift { "Shift+" } else { "" },
        if alt { "Alt+" } else { "" },
    )
}

impl App {
    /// Ctrl+Shift+F12: the user's "it just went wrong here" mark in the
    /// journal. Taken before egui and every modal lock, so it works whatever
    /// is on screen.
    pub(super) fn journal_mark_key(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    physical_key: PhysicalKey::Code(KeyCode::F12),
                    state,
                    repeat,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        if !(self.edit.input.ctrl_held && self.edit.input.shift_held) {
            return false;
        }
        if *state == ElementState::Pressed && !repeat {
            self.journal_mark();
        }
        true
    }

    /// Write the user's mark into the journal and say so in the status bar.
    pub(in crate::app) fn journal_mark(&mut self) {
        self.shell.status_msg = match crate::diag::mark() {
            Some(at) => format!("Đã đánh dấu lỗi vào nhật ký lúc {at}"),
            None => "Nhật ký lỗi đang tắt".to_string(),
        };
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// One journal line for the input events worth retelling.
    pub(super) fn journal_input(&self, event: &WindowEvent) {
        let input = &self.edit.input;
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        state: ElementState::Pressed,
                        repeat: false,
                        ..
                    },
                ..
            } => {
                if is_modifier(*code) {
                    return;
                }
                // Text being typed is the user's own business; only chords and
                // the keys that end an edit are of interest.
                let typing =
                    self.edit.text_edit.is_some() || self.win.egui_ctx.egui_wants_keyboard_input();
                if typing && !input.ctrl_held && !input.alt_held && !speaks_while_typing(*code) {
                    return;
                }
                crate::diag::note(
                    "key",
                    &chord(input.ctrl_held, input.shift_held, input.alt_held, *code),
                );
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = *state == ElementState::Pressed;
                if pressed && input.was_over_ui {
                    crate::diag::note(
                        "mouse",
                        &format!(
                            "{button:?} down on the UI at ({:.0}, {:.0})",
                            input.mouse_x, input.mouse_y
                        ),
                    );
                    return;
                }
                if pressed {
                    CANVAS_PRESS.store(true, Ordering::Relaxed);
                } else if !CANVAS_PRESS.swap(false, Ordering::Relaxed) {
                    return;
                }
                let zoom = self.edit.view.zoom.max(1e-6);
                let tool = self.edit.tools.active_id();
                crate::diag::note(
                    "mouse",
                    &format!(
                        "{button:?} {} on the canvas at ({:.0}, {:.0}), tool {}, zoom {:.0}%",
                        if pressed { "down" } else { "up" },
                        (input.mouse_x - self.edit.view.offset_x) / zoom,
                        (input.mouse_y - self.edit.view.offset_y) / zoom,
                        tool.name(),
                        zoom * 100.0,
                    ),
                );
            }
            WindowEvent::DroppedFile(path) => {
                crate::diag::note("drop", &path.display().to_string());
            }
            WindowEvent::CloseRequested => crate::diag::note("window", "close requested"),
            WindowEvent::Focused(focused) => {
                crate::diag::note("window", if *focused { "focused" } else { "lost focus" });
            }
            WindowEvent::Resized(size) => {
                crate::diag::note_change(
                    "window",
                    "resized to",
                    &format!("{}x{}", size.width, size.height),
                );
            }
            _ => {}
        }
    }

    /// Once a frame: the status line, and what the user has in front of them.
    /// Each reaches the journal only when it has changed.
    pub(super) fn journal_frame_state(&self) {
        crate::diag::status(&self.shell.status_msg);
        let doc = &self.docs.documents[self.docs.active_doc_idx];
        let tool = self.edit.tools.active_id();
        let layers = doc.canvas.layer_stack.layers.len();
        let transforming = self.edit.transform_state.is_some();
        let editing_text = self.edit.text_edit.is_some();
        let developing = self.win.develop_window.is_some();
        let dialog = self.is_modal_open();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (
            tool.name(),
            self.docs.active_doc_idx,
            self.docs.documents.len(),
            doc.canvas.width,
            doc.canvas.height,
            layers,
            (transforming, editing_text, developing, dialog),
        )
            .hash(&mut hasher);
        // Never zero, the journal's "nothing written yet".
        crate::diag::context(hasher.finish() | 1, || {
            let mut text = format!(
                "tool {}; tab {} of {} \"{}\" {}x{}, {layers} layer(s)",
                tool.name(),
                self.docs.active_doc_idx + 1,
                self.docs.documents.len(),
                doc.title,
                doc.canvas.width,
                doc.canvas.height,
            );
            for (on, what) in [
                (transforming, "free transform"),
                (editing_text, "text edit"),
                (developing, "Develop window"),
                (dialog, "a dialog"),
            ] {
                if on {
                    text.push_str("; open: ");
                    text.push_str(what);
                }
            }
            text
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_read_the_way_the_menus_print_them() {
        assert_eq!(chord(true, true, false, KeyCode::KeyT), "Ctrl+Shift+T");
        assert_eq!(chord(false, false, true, KeyCode::Digit1), "Alt+1");
        assert_eq!(chord(false, false, false, KeyCode::Enter), "Enter");
    }

    #[test]
    fn typed_characters_stay_out_of_the_journal_but_enter_and_escape_do_not() {
        assert!(!speaks_while_typing(KeyCode::KeyA));
        assert!(!speaks_while_typing(KeyCode::Backspace));
        assert!(speaks_while_typing(KeyCode::Enter));
        assert!(speaks_while_typing(KeyCode::Escape));
        assert!(is_modifier(KeyCode::ControlLeft));
    }
}

//! Keyboard input for the main window, extracted verbatim from the
//! window_event match arm.
//!
//! Re-bindable commands (see `app::commands`) keep their original key arms
//! below, each gated by `run_default` so it only fires while the command still
//! has its built-in key. A binding the user changed in Preferences is routed
//! first, through `custom_command_for` → `run_command`, which holds the same
//! command bodies.

use crate::app::commands::{Command, KeyChord, KeyName};
use crate::app::state::{App, InputState};
use crate::extension::tool::ToolCtx;
use crate::tools::ToolId;
use winit::{
    event::ElementState,
    event_loop::ActiveEventLoop,
    keyboard::{KeyCode, PhysicalKey},
};

/// An arrow key's step in page pixels: one, or ten with Shift.
fn arrow_step(key: PhysicalKey, shift: bool) -> Option<(f32, f32)> {
    let step = if shift { 10.0 } else { 1.0 };
    match key {
        PhysicalKey::Code(KeyCode::ArrowLeft) => Some((-step, 0.0)),
        PhysicalKey::Code(KeyCode::ArrowRight) => Some((step, 0.0)),
        PhysicalKey::Code(KeyCode::ArrowUp) => Some((0.0, -step)),
        PhysicalKey::Code(KeyCode::ArrowDown) => Some((0.0, step)),
        _ => None,
    }
}

/// `[` / `]` step for the Clone / Repair tip, coarser as the tip grows
/// (Photoshop's brush-size stepping).
fn clone_bracket_step(size: f32, grow: bool) -> f32 {
    let s = if grow { size } else { size - 0.5 };
    if s < 10.0 {
        1.0
    } else if s < 100.0 {
        5.0
    } else if s < 200.0 {
        10.0
    } else if s < 500.0 {
        25.0
    } else {
        50.0
    }
}

impl InputState {
    /// Follow Alt / Ctrl / Shift / Space as the keyboard has them, before egui,
    /// a dialog or the tool router can swallow the key: egui-winit consumes
    /// every key event, releases too, while one of its text fields has focus,
    /// and a release lost that way left Alt held (every marquee subtracting,
    /// so a drag drew nothing) or Space held (every drag panning). A Space
    /// press only pans when the canvas gets it, so presses are left to the
    /// router; its release always lets go. Returns whether a flag changed.
    pub(in crate::app) fn note_key(&mut self, key: PhysicalKey, pressed: bool) -> bool {
        let flag = match key {
            PhysicalKey::Code(KeyCode::AltLeft | KeyCode::AltRight) => {
                if !pressed {
                    self.alt_right_dragging = false;
                }
                &mut self.alt_held
            }
            PhysicalKey::Code(KeyCode::ControlLeft | KeyCode::ControlRight) => &mut self.ctrl_held,
            PhysicalKey::Code(KeyCode::ShiftLeft | KeyCode::ShiftRight) => &mut self.shift_held,
            PhysicalKey::Code(KeyCode::Space) if !pressed => {
                self.space_dragging = false;
                &mut self.space_held
            }
            _ => return false,
        };
        std::mem::replace(flag, pressed) != pressed
    }
}

impl App {
    /// The main window's KeyboardInput arm, verbatim. Shortcuts, tool keys,
    /// nudges, undo/redo — everything the keyboard drives outside egui.
    pub(in crate::app) fn on_main_keyboard_input(
        &mut self,
        event_loop: &ActiveEventLoop,
        physical_key: PhysicalKey,
        state: ElementState,
        repeat: bool,
    ) {
        let pressed = state == ElementState::Pressed;
        match physical_key {
            PhysicalKey::Code(KeyCode::AltLeft) | PhysicalKey::Code(KeyCode::AltRight) => {
                self.edit.input.alt_held = pressed;
                if !pressed {
                    self.edit.input.alt_right_dragging = false;
                }
            }
            PhysicalKey::Code(KeyCode::ControlLeft) | PhysicalKey::Code(KeyCode::ControlRight) => {
                self.edit.input.ctrl_held = pressed;
                // Pen tool: Ctrl swaps the cursor to an arrow (edit mode).
                if self.edit.tools.active_id() == ToolId::Pen {
                    self.sync_cursor(event_loop);
                }
            }
            PhysicalKey::Code(KeyCode::ShiftLeft) | PhysicalKey::Code(KeyCode::ShiftRight) => {
                self.edit.input.shift_held = pressed;
            }
            _ => {}
        }
        // A flowing-text document gives editing keys to egui/cosmic-text. Keep
        // only application-lifecycle shortcuts here; allowing the normal tool
        // router to continue would nudge layers, switch tools or undo the dormant
        // 1x1 compatibility canvas underneath the document surface.
        if self.docs.active().is_flow_text() {
            if pressed {
                match self.custom_command_for(physical_key) {
                    Some(Command::FileSave) => self.do_save(),
                    Some(Command::FileOpen) => self.do_open(),
                    Some(Command::FileClose) => self.close_doc(self.docs.active_doc_idx),
                    Some(_) => {}
                    None if self.edit.input.ctrl_held => match physical_key {
                        PhysicalKey::Code(KeyCode::KeyS)
                            if self.shell.keymap.is_default(Command::FileSave) =>
                        {
                            self.do_save()
                        }
                        PhysicalKey::Code(KeyCode::KeyO)
                            if self.shell.keymap.is_default(Command::FileOpen) =>
                        {
                            self.do_open()
                        }
                        PhysicalKey::Code(KeyCode::KeyW)
                            if self.shell.keymap.is_default(Command::FileClose) =>
                        {
                            self.close_doc(self.docs.active_doc_idx)
                        }
                        _ => {}
                    },
                    None => {}
                }
            }
            return;
        }
        let open_key = match self.custom_command_for(physical_key) {
            Some(cmd) => cmd == Command::FileOpen,
            None => {
                self.shell.keymap.is_default(Command::FileOpen)
                    && self.edit.input.ctrl_held
                    && matches!(physical_key, PhysicalKey::Code(KeyCode::KeyO))
            }
        };
        if self.shell.ui.show_welcome && !self.shell.ui.show_new_dialog && pressed && open_key {
            self.do_open();
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return;
        }
        // Ctrl+A in the Library grid selects every thumbnail (not the hidden
        // document's pixels). Intercept before the editor's Select All below.
        let select_all_key = match self.custom_command_for(physical_key) {
            Some(cmd) => cmd == Command::SelectAll,
            None => {
                self.shell.keymap.is_default(Command::SelectAll)
                    && self.edit.input.ctrl_held
                    && !self.edit.input.shift_held
                    && matches!(physical_key, PhysicalKey::Code(KeyCode::KeyA))
            }
        };
        if self.shell.ui.show_library && pressed && select_all_key {
            self.lib.grid.select_all();
            let n = self.lib.grid.selected.len();
            self.shell.status_msg = format!("Selected all ({n})");
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return;
        }
        if self.is_blocking_modal() {
            return;
        }
        if self.edit.show_refine_panel
            && self.refine_panel_key(event_loop, physical_key, pressed, repeat)
        {
            return;
        }
        if self.portrait_painting() && self.portrait_brush_key(event_loop, physical_key, pressed) {
            return;
        }
        // A tool's key with Auto retouch open: the view tools are taken up
        // as they are; any other first has a retouch under way applied (see
        // `portrait_tool_picked`). The key then takes up its tool below.
        let mut view_tool_key = false;
        if pressed && !repeat && self.shell.ui.show_portrait_dialog {
            let command = self
                .pressed_chord(physical_key)
                .and_then(|chord| self.shell.keymap.command_for(chord));
            match command {
                Some(Command::ToolHand | Command::ToolZoom) => view_tool_key = true,
                Some(command) if command.id().starts_with("tool.") => {
                    self.portrait_tool_picked(command == Command::ToolCrop);
                }
                _ => {}
            }
        }
        if pressed && self.is_preview_dialog_open() {
            let is_view_shortcut = view_tool_key
                || matches!(
                    physical_key,
                    PhysicalKey::Code(KeyCode::Equal)
                        | PhysicalKey::Code(KeyCode::Minus)
                        | PhysicalKey::Code(KeyCode::NumpadAdd)
                        | PhysicalKey::Code(KeyCode::NumpadSubtract)
                        | PhysicalKey::Code(KeyCode::Digit0)
                        | PhysicalKey::Code(KeyCode::Numpad0)
                        | PhysicalKey::Code(KeyCode::Digit1)
                        | PhysicalKey::Code(KeyCode::Numpad1)
                        | PhysicalKey::Code(KeyCode::Space)
                        | PhysicalKey::Code(KeyCode::ControlLeft)
                        | PhysicalKey::Code(KeyCode::ControlRight)
                        | PhysicalKey::Code(KeyCode::ShiftLeft)
                        | PhysicalKey::Code(KeyCode::ShiftRight)
                        | PhysicalKey::Code(KeyCode::AltLeft)
                        | PhysicalKey::Code(KeyCode::AltRight)
                )
                || matches!(
                    self.custom_command_for(physical_key),
                    Some(Command::FitScreen | Command::ZoomActual)
                );
            // The other previews keep every key but the view ones; a retouch
            // previewed by Auto retouch gives way to a command.
            let passes = if self.portrait_yields() {
                self.portrait_key_passes(physical_key, is_view_shortcut)
            } else {
                is_view_shortcut
            };
            if !passes {
                return;
            }
        }

        // The Crop frame up: the arrow keys move it (Shift: ten pixels).
        if pressed
            && let Some((dx, dy)) = arrow_step(physical_key, self.edit.input.shift_held)
            && self.edit.tools.active_id() == ToolId::Crop
            && self.edit.tools.crop().has_selection()
            && !self.edit.input.ctrl_held
            && !self.edit.input.alt_held
        {
            if self.edit.pending_crop_job.is_none() && self.edit.tools.crop_mut().nudge(dx, dy) {
                self.update_crop_preview();
                self.redraw_main();
            }
            return;
        }
        if pressed && self.is_tool_modal_active() {
            let is_allowed = matches!(
                physical_key,
                PhysicalKey::Code(KeyCode::Escape)
                    | PhysicalKey::Code(KeyCode::Enter)
                    | PhysicalKey::Code(KeyCode::NumpadEnter)
                    | PhysicalKey::Code(KeyCode::Equal)
                    | PhysicalKey::Code(KeyCode::Minus)
                    | PhysicalKey::Code(KeyCode::NumpadAdd)
                    | PhysicalKey::Code(KeyCode::NumpadSubtract)
                    | PhysicalKey::Code(KeyCode::Digit0)
                    | PhysicalKey::Code(KeyCode::Numpad0)
                    | PhysicalKey::Code(KeyCode::Digit1)
                    | PhysicalKey::Code(KeyCode::Numpad1)
                    | PhysicalKey::Code(KeyCode::Space)
                    | PhysicalKey::Code(KeyCode::AltLeft)
                    | PhysicalKey::Code(KeyCode::AltRight)
                    | PhysicalKey::Code(KeyCode::ControlLeft)
                    | PhysicalKey::Code(KeyCode::ControlRight)
                    | PhysicalKey::Code(KeyCode::ShiftLeft)
                    | PhysicalKey::Code(KeyCode::ShiftRight)
                    | PhysicalKey::Code(KeyCode::KeyZ)
                    | PhysicalKey::Code(KeyCode::KeyH)
            ) || matches!(
                self.custom_command_for(physical_key),
                Some(
                    Command::ToolZoom
                        | Command::ToolHand
                        | Command::EditUndo
                        | Command::EditRedo
                        | Command::FitScreen
                        | Command::ZoomActual
                )
            );
            if !is_allowed {
                return;
            }
        }

        // A shortcut the user re-bound in Preferences runs here; everything
        // still on its built-in key goes through the original arms below.
        if pressed && let Some(cmd) = self.custom_command_for(physical_key) {
            self.run_command(cmd, event_loop, repeat);
            return;
        }
        // The commands with no key arm below run from the keymap on their
        // built-in key too (Ctrl+Alt+L, Shift+Ctrl+B, Ctrl+[ …): before the
        // arms, which would take those keys for something else.
        if pressed
            && let Some(cmd) = self
                .pressed_chord(physical_key)
                .and_then(|chord| self.shell.keymap.command_for(chord))
                .filter(|cmd| cmd.runs_from_the_keymap())
        {
            self.run_command(cmd, event_loop, repeat);
            return;
        }

        match physical_key {
            // Move tool: arrow keys nudge the selected layer(s) — 1px, or
            // 10px with Shift (by convention). Repeats while held.
            PhysicalKey::Code(KeyCode::ArrowUp)
            | PhysicalKey::Code(KeyCode::ArrowDown)
            | PhysicalKey::Code(KeyCode::ArrowLeft)
            | PhysicalKey::Code(KeyCode::ArrowRight)
                if pressed
                    && self.edit.tools.active_id() == ToolId::Move
                    && self.edit.text_edit.is_none()
                    && self.edit.transform_state.is_none() =>
            {
                let step = if self.edit.input.shift_held { 10 } else { 1 };
                let (dx, dy) = match physical_key {
                    PhysicalKey::Code(KeyCode::ArrowUp) => (0, -step),
                    PhysicalKey::Code(KeyCode::ArrowDown) => (0, step),
                    PhysicalKey::Code(KeyCode::ArrowLeft) => (-step, 0),
                    PhysicalKey::Code(KeyCode::ArrowRight) => (step, 0),
                    _ => (0, 0),
                };
                if self.docs.active_mut().canvas.nudge_selected_layers(dx, dy) {
                    self.flush_canvas();
                    if let Some(w) = &self.win.window {
                        w.request_redraw();
                    }
                }
            }
            PhysicalKey::Code(KeyCode::AltLeft) | PhysicalKey::Code(KeyCode::AltRight) => {
                self.edit.input.alt_held = pressed;
                if !pressed {
                    self.edit.input.alt_right_dragging = false;
                }
            }
            PhysicalKey::Code(KeyCode::ControlLeft) | PhysicalKey::Code(KeyCode::ControlRight) => {
                self.edit.input.ctrl_held = pressed;
                // Pen tool: Ctrl swaps the cursor to an arrow (edit mode).
                if self.edit.tools.active_id() == ToolId::Pen {
                    self.sync_cursor(event_loop);
                }
            }
            PhysicalKey::Code(KeyCode::ShiftLeft) | PhysicalKey::Code(KeyCode::ShiftRight) => {
                self.edit.input.shift_held = pressed;
            }
            PhysicalKey::Code(KeyCode::Space) => {
                self.edit.input.space_held = pressed;
                if !pressed {
                    self.edit.input.space_dragging = false;
                }
                self.sync_cursor(event_loop);
            }
            PhysicalKey::Code(KeyCode::KeyM) if pressed => {
                let cmd = if self.edit.input.ctrl_held {
                    Command::Curves
                } else {
                    Command::ToolMarquee
                };
                self.run_default(cmd, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyL) if pressed => {
                let cmd = if self.edit.input.ctrl_held && self.edit.input.shift_held {
                    Command::AutoLevels
                } else if self.edit.input.ctrl_held {
                    Command::Levels
                } else {
                    Command::ToolLasso
                };
                self.run_default(cmd, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyW) if pressed && self.edit.input.ctrl_held => {
                self.run_default(Command::FileClose, event_loop, repeat);
            }
            // A guide picked with the Move tool: Delete removes the guide,
            // never the layer or the pixels it runs over.
            PhysicalKey::Code(KeyCode::Delete) | PhysicalKey::Code(KeyCode::Backspace)
                if pressed
                    && !repeat
                    && !self.edit.input.alt_held
                    && !self.edit.input.ctrl_held
                    && self.edit.text_edit.is_none()
                    && self.edit.transform_state.is_none()
                    && self.selected_guide().is_some() =>
            {
                self.delete_selected_guide();
            }
            // Multi-page PDF: Shift+Delete covers the same hard rectangular
            // selection on every page without rendering/caching every page.
            PhysicalKey::Code(KeyCode::Delete) | PhysicalKey::Code(KeyCode::Backspace)
                if pressed
                    && !repeat
                    && self.edit.input.shift_held
                    && !self.edit.input.alt_held
                    && !self.edit.input.ctrl_held
                    && self.docs.active().pdf_document.is_some()
                    && self.docs.active().canvas.selection.active =>
            {
                self.open_pdf_batch_dialog(crate::ui::intent::PdfBatchOperation::Clear);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            // Text layer selected + Alt/Ctrl+Delete: recolour the type to the
            // foreground (Alt) or background (Ctrl) colour — Photoshop's fill
            // shortcut applied to a Text layer — instead of filling raster pixels.
            PhysicalKey::Code(KeyCode::Delete) | PhysicalKey::Code(KeyCode::Backspace)
                if pressed
                    && !repeat
                    && self.edit.text_edit.is_none()
                    && (self.edit.input.alt_held || self.edit.input.ctrl_held)
                    && self.active_layer_is_text() =>
            {
                let color = if self.edit.input.alt_held {
                    self.edit.tools.brush().settings.color
                } else {
                    self.edit.bg_color
                };
                let idx = self.docs.active().canvas.layer_stack.active_idx;
                // No-op if it returns false (layer locked, or already that
                // colour) — never fall through to a raster fill, which would
                // paint the whole layer box solid over the glyphs.
                self.recolor_text_layer(idx, color);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::Delete) | PhysicalKey::Code(KeyCode::Backspace)
                if pressed
                    && !repeat
                    && self.edit.tools.active_id() == ToolId::Node
                    && self.edit.node_selected.is_some()
                    && !self.edit.input.alt_held
                    && !self.edit.input.ctrl_held =>
            {
                // Node tool: Delete removes the selected anchor (not the layer).
                self.node_delete_selected();
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::Delete) | PhysicalKey::Code(KeyCode::Backspace)
                if pressed
                    && !repeat
                    && !self.edit.input.alt_held
                    && !self.edit.input.ctrl_held
                    && self.can_trim_active_vector() =>
            {
                // Vector layer + active selection: Delete TRIMS the selected
                // region out of the shape (reuses the boolean Trim engine) rather
                // than clearing raster pixels, which is a no-op on vector layers.
                self.trim_active_vector_by_selection();
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::Delete) | PhysicalKey::Code(KeyCode::Backspace)
                if pressed && !repeat =>
            {
                if self.edit.input.alt_held || self.edit.input.ctrl_held {
                    // Alt+Delete fills with the foreground, Ctrl+Delete with
                    // the background colour: the command an agent runs.
                    let colour = if self.edit.input.alt_held {
                        self.edit.tools.brush().settings.color
                    } else {
                        self.edit.bg_color
                    };
                    let colour = crate::ui::color_hex(colour);
                    let _ = self.run_with("edit.fill", serde_json::json!({"color": colour}));
                } else {
                    // Plain Delete. Over the CANVAS with an active selection,
                    // clear only that region (transparent on a normal layer, or
                    // bg-filled on the opaque Background). Over a PANEL — the user
                    // is on the Layers panel — or with no selection at all, Delete
                    // removes the whole layer, matching the right-click "Delete
                    // layer". This is Photoshop's split: canvas Delete clears
                    // pixels, Layers-panel Delete removes the layer, so an active
                    // marquee no longer blocks deleting a layer from the panel.
                    let over_panel = self.edit.input.is_over_ui;
                    let canvas = &self.docs.active().canvas;
                    let has_sel = canvas.selection.active;
                    let is_bg = !canvas.layer_stack.layers.is_empty()
                        && canvas.active_layer().is_background;
                    if has_sel && !over_panel {
                        // Cleared, or the Background filled with the
                        // background colour.
                        let colour = crate::ui::color_hex(self.edit.bg_color);
                        let _ =
                            self.run_with("edit.clear", serde_json::json!({"background": colour}));
                        let _ = is_bg;
                    } else if self.docs.active_mut().canvas.remove_active_layer() {
                        self.apply_canvas_event(
                            crate::app::render::CanvasEvent::LayerStructureChanged,
                        );
                    }
                }
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyD) if pressed && self.edit.input.ctrl_held => {
                let has_selection = self.docs.active().canvas.selection.active;
                if self.edit.input.shift_held {
                    // Ctrl+Shift+D: always Repeat (explicit).
                    self.repeat_last_step();
                } else if has_selection {
                    // Ctrl+D with an active pixel selection = Deselect (Photoshop).
                    let _ = self.run_now("select.none");
                } else {
                    // Ctrl+D with no selection = Repeat the last duplicate step.
                    self.repeat_last_step();
                }
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            // "+" duplicates the selection in place-ish (Corel-style), recording it
            // as the repeatable step. NumpadAdd, or Shift+"=" on the main row.
            PhysicalKey::Code(KeyCode::NumpadAdd)
                if pressed && !self.edit.input.ctrl_held && !self.is_tool_modal_active() =>
            {
                self.duplicate_selected_with_step((0, 0));
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::Equal)
                if pressed
                    && self.edit.input.shift_held
                    && !self.edit.input.ctrl_held
                    && !self.is_tool_modal_active() =>
            {
                self.duplicate_selected_with_step((0, 0));
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::F7) if pressed && self.edit.input.shift_held => {
                let _ = self.run_now("select.invert");
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::F6) if pressed && self.edit.input.shift_held => {
                self.shell.ui.show_feather_dialog = true;
            }
            PhysicalKey::Code(KeyCode::F5) if pressed && self.edit.input.shift_held => {
                self.request_smart_fill_fill();
            }
            PhysicalKey::Code(KeyCode::KeyW) if pressed && !self.edit.input.ctrl_held => {
                self.run_default(Command::ToolQuickSelect, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyB) if pressed => {
                let cmd = if self.edit.input.ctrl_held {
                    Command::ColorBalance
                } else {
                    Command::ToolBrush
                };
                self.run_default(cmd, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyE) if pressed && !self.edit.input.ctrl_held => {
                self.run_default(Command::ToolEraser, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyV) if pressed && !self.edit.input.ctrl_held => {
                self.run_default(Command::ToolMove, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyI)
                if pressed
                    && self.edit.input.ctrl_held
                    && !self.edit.input.shift_held
                    && !self.edit.input.alt_held =>
            {
                self.run_default(Command::Invert, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyI) if pressed && !self.edit.input.ctrl_held => {
                self.run_default(Command::ToolEyedropper, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyG) if pressed && !self.edit.input.ctrl_held => {
                self.run_default(Command::ToolFill, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyC) if pressed && !self.edit.input.ctrl_held => {
                self.run_default(Command::ToolCrop, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyP) if pressed && !self.edit.input.ctrl_held => {
                self.run_default(Command::ToolPen, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyA) if pressed && !self.edit.input.ctrl_held => {
                self.run_default(Command::ToolNode, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::Equal) | PhysicalKey::Code(KeyCode::NumpadAdd)
                if pressed && self.edit.input.ctrl_held =>
            {
                self.edit.view.zoom = (self.edit.view.zoom * 1.25).clamp(0.02, 64.0);
                self.push_canvas_uniforms();
                self.win.pending_view_change = true;
                self.win.last_cursor_radius = 0;
                self.sync_cursor(event_loop);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::Minus) | PhysicalKey::Code(KeyCode::NumpadSubtract)
                if pressed && self.edit.input.ctrl_held =>
            {
                self.edit.view.zoom = (self.edit.view.zoom / 1.25).clamp(0.02, 64.0);
                self.push_canvas_uniforms();
                self.win.pending_view_change = true;
                self.win.last_cursor_radius = 0;
                self.sync_cursor(event_loop);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyZ) if pressed && !self.edit.input.ctrl_held => {
                self.run_default(Command::ToolZoom, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyH) if pressed && !self.edit.input.ctrl_held => {
                self.run_default(Command::ToolHand, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyR)
                if pressed && self.edit.input.ctrl_held && !self.edit.input.alt_held =>
            {
                self.run_default(Command::ToggleRulers, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyT) if pressed && !self.edit.input.ctrl_held => {
                self.run_default(Command::ToolText, event_loop, repeat);
            }

            PhysicalKey::Code(KeyCode::KeyX)
                if pressed
                    && !repeat
                    && self.edit.input.ctrl_held
                    && !self.edit.input.shift_held =>
            {
                self.run_default(Command::EditCut, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyX)
                if pressed && !repeat && !self.edit.input.ctrl_held =>
            {
                std::mem::swap(
                    &mut self.edit.tools.brush_mut().settings.color,
                    &mut self.edit.bg_color,
                );
                self.edit.tools.fill_mut().color = self.edit.tools.brush().settings.color;
                self.edit.tools.eraser_mut().bg_color = self.edit.bg_color;
                self.edit.fg_color = self.edit.tools.brush().settings.color;
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyD)
                if pressed && !repeat && !self.edit.input.ctrl_held =>
            {
                self.edit.bg_color = [255, 255, 255, 255];
                self.edit.tools.brush_mut().settings.color = [0, 0, 0, 255];
                self.edit.tools.fill_mut().color = self.edit.tools.brush().settings.color;
                self.edit.tools.eraser_mut().bg_color = self.edit.bg_color;
                self.edit.fg_color = self.edit.tools.brush().settings.color;
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyS) if pressed && !self.edit.input.ctrl_held => {
                self.run_default(Command::ToolClone, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyS)
                if pressed && self.edit.input.ctrl_held && !self.edit.input.shift_held =>
            {
                self.run_default(Command::FileSave, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyS)
                if pressed && self.edit.input.ctrl_held && self.edit.input.shift_held =>
            {
                self.run_default(Command::FileSaveAs, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyO) if pressed && self.edit.input.ctrl_held => {
                self.run_default(Command::FileOpen, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyO) if pressed && !self.edit.input.ctrl_held => {
                self.run_default(Command::ToolDodge, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyN) if pressed && self.edit.input.ctrl_held => {
                self.run_default(Command::FileNew, event_loop, repeat);
            }
            // Preferences: Ctrl+K (Photoshop).
            PhysicalKey::Code(KeyCode::KeyK) if pressed && self.edit.input.ctrl_held => {
                self.run_default(Command::Preferences, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::Digit0) | PhysicalKey::Code(KeyCode::Numpad0)
                if pressed && self.edit.input.ctrl_held =>
            {
                self.run_default(Command::FitScreen, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::Digit1) if pressed && self.edit.input.ctrl_held => {
                self.run_default(Command::ZoomActual, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyZ)
                if pressed && self.edit.input.ctrl_held && !self.edit.input.shift_held =>
            {
                self.run_default(Command::EditUndo, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyZ)
                if pressed && self.edit.input.ctrl_held && self.edit.input.shift_held =>
            {
                self.run_default(Command::EditRedo, event_loop, repeat);
            }
            // Ctrl+P → Print dialog.
            PhysicalKey::Code(KeyCode::KeyP) if pressed && self.edit.input.ctrl_held => {
                self.run_default(Command::FilePrint, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyA)
                if pressed && self.edit.input.ctrl_held && self.edit.input.shift_held =>
            {
                self.run_default(Command::OpenDevelop, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyA) if pressed && self.edit.input.ctrl_held => {
                self.run_default(Command::SelectAll, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyC) if pressed && self.edit.input.ctrl_held => {
                self.run_default(Command::EditCopy, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyV) if pressed && self.edit.input.ctrl_held => {
                self.run_default(Command::EditPaste, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyT) if pressed && self.edit.input.ctrl_held => {
                self.run_default(Command::FreeTransform, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyU)
                if pressed && self.edit.input.ctrl_held && self.edit.input.shift_held =>
            {
                self.run_default(Command::Desaturate, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyU) if pressed && self.edit.input.ctrl_held => {
                self.run_default(Command::HueSaturation, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyU) if pressed && !self.edit.input.ctrl_held => {
                self.run_default(Command::ToolShape, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyJ) if pressed && !repeat && self.edit.input.ctrl_held => {
                self.run_default(Command::LayerViaCopy, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyJ) if pressed && !self.edit.input.ctrl_held => {
                self.run_default(Command::ToolRepair, event_loop, repeat);
            }
            PhysicalKey::Code(code @ (KeyCode::BracketLeft | KeyCode::BracketRight)) if pressed => {
                let grow = code == KeyCode::BracketRight;
                if self.edit.input.shift_held {
                    self.bracket_tip_hardness(grow);
                } else {
                    self.bracket_tip_size(grow);
                }
                // A soft tip's ring follows its hardness (Normal Brush Tip).
                self.win.last_cursor_radius = 0;
                self.sync_cursor(event_loop);
            }
            PhysicalKey::Code(KeyCode::ArrowUp)
                if pressed && self.docs.active().canvas.selection.active =>
            {
                self.docs.active_mut().canvas.selection.offset.1 -= 1;
                self.docs.active_mut().canvas.selection.mark_bbox_dirty();
                let cmd = crate::core::command::TranslateSelectionCommand::from_applied_move(
                    &self.docs.active().canvas.selection,
                    0,
                    -1,
                );
                self.docs.active_mut().canvas.record(Box::new(cmd));
                self.upload_selection_mask();
                self.push_selection_uniforms();
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::ArrowDown)
                if pressed && self.docs.active().canvas.selection.active =>
            {
                self.docs.active_mut().canvas.selection.offset.1 += 1;
                self.docs.active_mut().canvas.selection.mark_bbox_dirty();
                let cmd = crate::core::command::TranslateSelectionCommand::from_applied_move(
                    &self.docs.active().canvas.selection,
                    0,
                    1,
                );
                self.docs.active_mut().canvas.record(Box::new(cmd));
                self.upload_selection_mask();
                self.push_selection_uniforms();
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::ArrowLeft)
                if pressed && self.docs.active().canvas.selection.active =>
            {
                self.docs.active_mut().canvas.selection.offset.0 -= 1;
                self.docs.active_mut().canvas.selection.mark_bbox_dirty();
                let cmd = crate::core::command::TranslateSelectionCommand::from_applied_move(
                    &self.docs.active().canvas.selection,
                    -1,
                    0,
                );
                self.docs.active_mut().canvas.record(Box::new(cmd));
                self.upload_selection_mask();
                self.push_selection_uniforms();
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::ArrowRight)
                if pressed && self.docs.active().canvas.selection.active =>
            {
                self.docs.active_mut().canvas.selection.offset.0 += 1;
                self.docs.active_mut().canvas.selection.mark_bbox_dirty();
                let cmd = crate::core::command::TranslateSelectionCommand::from_applied_move(
                    &self.docs.active().canvas.selection,
                    1,
                    0,
                );
                self.docs.active_mut().canvas.record(Box::new(cmd));
                self.upload_selection_mask();
                self.push_selection_uniforms();
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::Escape) if pressed && !repeat => {
                if self.shell.ui.develop_local_arm.is_some() {
                    self.shell.ui.develop_local_arm = None;
                    self.dev.develop_local_drag = None;
                    if let Some(w) = &self.win.window {
                        w.request_redraw();
                    }
                } else if self.edit.warp_state.is_some() {
                    self.cancel_warp();
                    self.sync_cursor(event_loop);
                } else if self.edit.transform_state.is_some() {
                    self.cancel_transform();
                    self.sync_cursor(event_loop);
                } else if matches!(
                    self.edit.tools.active_id(),
                    ToolId::Crop | ToolId::PerspectiveCrop
                ) {
                    self.cancel_crop_job();
                    self.edit.tools.active_on_cancel();
                    self.update_crop_preview();
                    if let Some(w) = &self.win.window {
                        w.request_redraw();
                    }
                } else if matches!(
                    self.edit.tools.active_id(),
                    ToolId::PolygonLasso | ToolId::Pen
                ) {
                    self.edit.tools.active_on_cancel();
                    if let Some(w) = &self.win.window {
                        w.request_redraw();
                    }
                } else if self.edit.tools.active_id() == ToolId::Arrow {
                    // Finish the current branch-arrow group: the next drag starts a
                    // fresh trunk instead of adding to this object.
                    self.edit.arrow_multi_layer = None;
                    self.edit.tools.active_on_cancel();
                    if let Some(w) = &self.win.window {
                        w.request_redraw();
                    }
                }
            }
            PhysicalKey::Code(KeyCode::Enter) | PhysicalKey::Code(KeyCode::NumpadEnter)
                if pressed && !repeat =>
            {
                if self.edit.warp_state.is_some() {
                    self.commit_warp();
                    self.sync_cursor(event_loop);
                } else if self.edit.transform_state.is_some() {
                    self.commit_transform();
                    self.sync_cursor(event_loop);
                } else if self.edit.tools.active_id() == ToolId::Crop {
                    self.commit_active_crop();
                } else if self.edit.tools.active_id() == ToolId::PerspectiveCrop {
                    self.commit_active_perspective_crop();
                } else if self.edit.tools.active_id() == ToolId::Arrow {
                    // Finish the current branch-arrow group; the next drag starts a
                    // new trunk. (Each drag already committed its own segment.)
                    self.edit.arrow_multi_layer = None;
                } else if matches!(
                    self.edit.tools.active_id(),
                    ToolId::PolygonLasso | ToolId::Pen
                ) {
                    // Pen in Path mode commits an editable vector layer, which
                    // needs app-level (layer-structure) invalidation — handle it
                    // before building the raster ToolCtx. Ctrl+Enter still forces
                    // a selection regardless of mode.
                    if self.edit.tools.active_id() == ToolId::Pen
                        && !self.edit.input.ctrl_held
                        && self.edit.tools.pen().mode == crate::tools::pen::PenMode::Path
                    {
                        self.commit_pen_as_path();
                    } else {
                        {
                            let mut ctx = ToolCtx::new(
                                self.docs.active_mut(),
                                self.edit.fg_color,
                                self.edit.bg_color,
                                self.edit.view.zoom,
                                self.edit.view.offset_x,
                                self.edit.view.offset_y,
                            );
                            // Ctrl+Enter forces a Pen path to a selection,
                            // regardless of its Selection/Fill/Stroke mode.
                            if self.edit.input.ctrl_held
                                && self.edit.tools.active_id() == ToolId::Pen
                            {
                                self.edit
                                    .tools
                                    .pen_mut()
                                    .commit_as_selection(ctx.canvas_mut());
                            } else {
                                self.edit.tools.active_on_confirm(&mut ctx);
                            }
                        }
                        self.docs.active_mut().canvas.selection.refresh_bbox();
                        self.upload_selection_mask();
                        self.push_selection_uniforms();
                        if let Some(w) = &self.win.window {
                            w.request_redraw();
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// The bindable key a physical key stands for. Numpad digits count as the
/// main-row digits, as the built-in Ctrl+0 already accepted Numpad 0.
pub(in crate::app) fn key_name(code: KeyCode) -> Option<KeyName> {
    use KeyName as K;
    Some(match code {
        KeyCode::KeyA => K::A,
        KeyCode::KeyB => K::B,
        KeyCode::KeyC => K::C,
        KeyCode::KeyD => K::D,
        KeyCode::KeyE => K::E,
        KeyCode::KeyF => K::F,
        KeyCode::KeyG => K::G,
        KeyCode::KeyH => K::H,
        KeyCode::KeyI => K::I,
        KeyCode::KeyJ => K::J,
        KeyCode::KeyK => K::K,
        KeyCode::KeyL => K::L,
        KeyCode::KeyM => K::M,
        KeyCode::KeyN => K::N,
        KeyCode::KeyO => K::O,
        KeyCode::KeyP => K::P,
        KeyCode::KeyQ => K::Q,
        KeyCode::KeyR => K::R,
        KeyCode::KeyS => K::S,
        KeyCode::KeyT => K::T,
        KeyCode::KeyU => K::U,
        KeyCode::KeyV => K::V,
        KeyCode::KeyW => K::W,
        KeyCode::KeyX => K::X,
        KeyCode::KeyY => K::Y,
        KeyCode::KeyZ => K::Z,
        KeyCode::Digit0 | KeyCode::Numpad0 => K::Digit0,
        KeyCode::Digit1 | KeyCode::Numpad1 => K::Digit1,
        KeyCode::Digit2 | KeyCode::Numpad2 => K::Digit2,
        KeyCode::Digit3 | KeyCode::Numpad3 => K::Digit3,
        KeyCode::Digit4 | KeyCode::Numpad4 => K::Digit4,
        KeyCode::Digit5 | KeyCode::Numpad5 => K::Digit5,
        KeyCode::Digit6 | KeyCode::Numpad6 => K::Digit6,
        KeyCode::Digit7 | KeyCode::Numpad7 => K::Digit7,
        KeyCode::Digit8 | KeyCode::Numpad8 => K::Digit8,
        KeyCode::Digit9 | KeyCode::Numpad9 => K::Digit9,
        KeyCode::F1 => K::F1,
        KeyCode::F2 => K::F2,
        KeyCode::F3 => K::F3,
        KeyCode::F4 => K::F4,
        KeyCode::F5 => K::F5,
        KeyCode::F6 => K::F6,
        KeyCode::F7 => K::F7,
        KeyCode::F8 => K::F8,
        KeyCode::F9 => K::F9,
        KeyCode::F10 => K::F10,
        KeyCode::F11 => K::F11,
        KeyCode::F12 => K::F12,
        KeyCode::Comma => K::Comma,
        KeyCode::Period => K::Period,
        KeyCode::Slash => K::Slash,
        KeyCode::Semicolon => K::Semicolon,
        KeyCode::Quote => K::Quote,
        KeyCode::Backquote => K::Backquote,
        KeyCode::Backslash => K::Backslash,
        KeyCode::BracketLeft => K::BracketLeft,
        KeyCode::BracketRight => K::BracketRight,
        _ => return None,
    })
}

impl App {
    /// `[` / `]`: resize the tip of the tool in hand.
    fn bracket_tip_size(&mut self, grow: bool) {
        let moved = |size: f32, step: f32, max: f32| {
            if grow {
                (size + step).min(max)
            } else {
                (size - step).max(1.0)
            }
        };
        let stepped = |size: f32, max: f32| moved(size, clone_bracket_step(size, grow), max);
        let nudged = |size: f32, max: f32| moved(size, 2.0, max);
        let tools = &mut self.edit.tools;
        if self.edit.show_refine_panel {
            let tool = tools.refine_brush_mut();
            tool.size = stepped(tool.size, 1000.0);
            return;
        }
        match tools.active_id() {
            ToolId::Eraser => {
                let tool = tools.eraser_mut();
                tool.size = nudged(tool.size, 5000.0);
            }
            ToolId::Clone | ToolId::Repair => {
                let tool = tools.clone_like_mut();
                tool.size = stepped(tool.size, 5000.0);
            }
            ToolId::Smudge => {
                let tool = tools.smudge_mut();
                tool.size = stepped(tool.size, 5000.0);
            }
            ToolId::Dodge | ToolId::Burn => {
                let tool = tools.dodge_burn_mut();
                tool.size = stepped(tool.size, 5000.0);
            }
            ToolId::SmartSelect => {
                let tool = tools.wand_mut();
                tool.brush_size = stepped(tool.brush_size, 2000.0);
            }
            _ => {
                let settings = &mut tools.brush_mut().settings;
                settings.size = nudged(settings.size, 300.0);
            }
        }
    }

    /// `Shift+[` / `Shift+]`: soften / harden the tip of the tool in hand.
    fn bracket_tip_hardness(&mut self, grow: bool) {
        let moved =
            |hardness: f32, step: f32| (hardness + if grow { step } else { -step }).clamp(0.0, 1.0);
        let tools = &mut self.edit.tools;
        if self.edit.show_refine_panel {
            let tool = tools.refine_brush_mut();
            tool.hardness = moved(tool.hardness, 0.1);
            return;
        }
        match tools.active_id() {
            ToolId::Eraser => {
                let tool = tools.eraser_mut();
                tool.hardness = moved(tool.hardness, 0.1);
            }
            ToolId::Clone | ToolId::Repair => {
                let tool = tools.clone_like_mut();
                tool.hardness = moved(tool.hardness, 0.25);
            }
            ToolId::Smudge => {
                let tool = tools.smudge_mut();
                tool.hardness = moved(tool.hardness, 0.25);
            }
            ToolId::Dodge | ToolId::Burn => {
                let tool = tools.dodge_burn_mut();
                tool.hardness = moved(tool.hardness, 0.25);
            }
            // The Quick Selection brush has no hardness.
            ToolId::SmartSelect => {}
            _ => {
                let settings = &mut tools.brush_mut().settings;
                settings.hardness = moved(settings.hardness, 0.1);
            }
        }
    }

    fn redraw_main(&self) {
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// The chord a key press makes with the modifiers currently held.
    pub(in crate::app) fn pressed_chord(&self, physical_key: PhysicalKey) -> Option<KeyChord> {
        let PhysicalKey::Code(code) = physical_key else {
            return None;
        };
        Some(KeyChord::new(
            self.edit.input.ctrl_held,
            self.edit.input.shift_held,
            self.edit.input.alt_held,
            key_name(code)?,
        ))
    }

    /// The command this key press triggers through a binding the user set in
    /// Preferences. `None` for keys still on their built-in binding.
    pub(in crate::app) fn custom_command_for(&self, physical_key: PhysicalKey) -> Option<Command> {
        self.shell
            .keymap
            .custom_command_for(self.pressed_chord(physical_key)?)
    }

    /// Run `cmd` from its original key arm only while it still has its
    /// built-in key; once re-bound, that key no longer triggers it.
    fn run_default(&mut self, cmd: Command, event_loop: &ActiveEventLoop, repeat: bool) {
        if self.shell.keymap.is_default(cmd) {
            self.run_command(cmd, event_loop, repeat);
        }
    }

    /// Preferences ▸ Shortcuts is waiting for a key: record the next key press
    /// for the dialog. Returns `true` when the event was taken (it must then
    /// reach neither egui nor any shortcut).
    pub(in crate::app) fn capture_shortcut_key(
        &mut self,
        physical_key: PhysicalKey,
        pressed: bool,
    ) -> bool {
        let Some(cmd) = self.shell.ui.shortcut_capture else {
            return false;
        };
        let PhysicalKey::Code(code) = physical_key else {
            return true;
        };
        match code {
            KeyCode::ControlLeft | KeyCode::ControlRight => {
                self.edit.input.ctrl_held = pressed;
                return true;
            }
            KeyCode::ShiftLeft | KeyCode::ShiftRight => {
                self.edit.input.shift_held = pressed;
                return true;
            }
            KeyCode::AltLeft | KeyCode::AltRight => {
                self.edit.input.alt_held = pressed;
                return true;
            }
            _ => {}
        }
        if !pressed {
            return true;
        }
        let outcome = match code {
            KeyCode::Escape => crate::ui::ShortcutCapture::Cancel,
            KeyCode::Backspace | KeyCode::Delete => crate::ui::ShortcutCapture::Clear,
            _ => match self.pressed_chord(physical_key) {
                Some(chord) => crate::ui::ShortcutCapture::Chord(chord),
                // Keys that cannot be bound (Enter, Space, arrows…): keep waiting.
                None => return true,
            },
        };
        self.shell.ui.shortcut_capture = None;
        self.shell.ui.shortcut_captured = Some((cmd, outcome));
        self.redraw_main();
        true
    }

    /// Keys while the Refine Selection panel is open. Enter / Esc commit /
    /// cancel, Ctrl+Z steps the brush strokes, F cycles the view and X shows
    /// the original; modifiers, Space, Ctrl+zoom and `[ ]` go on to the normal
    /// arms. Everything else is refused like any modal operation. Returns true
    /// when the key was taken here.
    /// Keys while the Chỉnh chân dung brush paints: Ctrl+Z / Ctrl+Shift+Z
    /// step its strokes, [ ] size the brush and Shift+[ ] its hardness.
    fn portrait_brush_key(
        &mut self,
        event_loop: &ActiveEventLoop,
        physical_key: PhysicalKey,
        pressed: bool,
    ) -> bool {
        let PhysicalKey::Code(code) = physical_key else {
            return false;
        };
        if !pressed {
            return false;
        }
        let command = self
            .pressed_chord(physical_key)
            .and_then(|c| self.shell.keymap.command_for(c));
        match (code, command) {
            (_, Some(Command::EditUndo)) => self.portrait_brush_step(false),
            (_, Some(Command::EditRedo)) => self.portrait_brush_step(true),
            (KeyCode::BracketLeft | KeyCode::BracketRight, _) => {
                let grow = code == KeyCode::BracketRight;
                let shift = self.edit.input.shift_held;
                let tool = self.edit.tools.refine_brush_mut();
                if shift {
                    tool.hardness = (tool.hardness + if grow { 0.1 } else { -0.1 }).clamp(0.0, 1.0);
                } else if grow {
                    tool.size = (tool.size + clone_bracket_step(tool.size, true)).min(1000.0);
                } else {
                    tool.size = (tool.size - clone_bracket_step(tool.size, false)).max(1.0);
                }
                self.win.last_cursor_radius = 0;
                self.sync_cursor(event_loop);
            }
            _ => return false,
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        true
    }

    /// A key pressed while only a retouch under way holds the canvas (see
    /// `portrait_yields`), `view` when it is a view shortcut. Returns whether
    /// it goes on to what it is bound to: a key that edits does once the
    /// retouch is applied (`yield_portrait`), one that only looks does as it
    /// is. Ctrl+Z gives the preview up instead; Enter and Esc are the
    /// dialog's.
    pub(in crate::app) fn portrait_key_passes(
        &mut self,
        physical_key: PhysicalKey,
        view: bool,
    ) -> bool {
        let PhysicalKey::Code(code) = physical_key else {
            return view;
        };
        // Typed into a field (a value, a name, a prompt), Ctrl+A or Ctrl+V
        // is that field's and no command.
        if self.win.egui_ctx.egui_wants_keyboard_input() {
            return view;
        }
        let input = &self.edit.input;
        let (ctrl, shift) = (input.ctrl_held, input.shift_held);
        let command = self
            .pressed_chord(physical_key)
            .and_then(|c| self.shell.keymap.command_for(c));
        // An ID photo being made holds only this tab: opening another photo
        // lets it go on.
        if self.photo_only_holds_tab() && command == Some(Command::FileOpen) {
            return true;
        }
        let edits = match command {
            Some(Command::EditUndo) => {
                self.drop_portrait_preview();
                self.redraw_main();
                return false;
            }
            Some(Command::EditRedo) => return false,
            Some(
                Command::EditCopy
                | Command::Preferences
                | Command::ToggleRulers
                | Command::FitScreen
                | Command::ZoomActual
                | Command::ToolHand
                | Command::ToolZoom
                | Command::ProofColors
                | Command::GamutWarning,
            ) => return true,
            Some(_) => true,
            // The keys that edit without being a command of the keymap.
            None => match code {
                KeyCode::Delete | KeyCode::Backspace => true,
                KeyCode::ArrowUp
                | KeyCode::ArrowDown
                | KeyCode::ArrowLeft
                | KeyCode::ArrowRight => {
                    let doc = self.docs.active();
                    self.edit.tools.active_id() == ToolId::Move || doc.canvas.selection.active
                }
                KeyCode::NumpadAdd => !ctrl,
                KeyCode::Equal => shift && !ctrl,
                KeyCode::F5 | KeyCode::F6 | KeyCode::F7 => shift,
                KeyCode::KeyD => ctrl,
                _ => false,
            },
        };
        if edits {
            self.yield_portrait();
            self.redraw_main();
            return true;
        }
        // Tip size, the paint colours and the proof view change no pixel.
        view || matches!(
            code,
            KeyCode::BracketLeft | KeyCode::BracketRight | KeyCode::KeyX | KeyCode::KeyD
        )
    }

    fn refine_panel_key(
        &mut self,
        event_loop: &ActiveEventLoop,
        physical_key: PhysicalKey,
        pressed: bool,
        repeat: bool,
    ) -> bool {
        let PhysicalKey::Code(code) = physical_key else {
            return true;
        };
        let ctrl = self.edit.input.ctrl_held;
        let plain = !ctrl && !self.edit.input.alt_held && !self.edit.input.shift_held;
        let passes = match code {
            KeyCode::ControlLeft
            | KeyCode::ControlRight
            | KeyCode::ShiftLeft
            | KeyCode::ShiftRight
            | KeyCode::AltLeft
            | KeyCode::AltRight
            | KeyCode::Space
            | KeyCode::BracketLeft
            | KeyCode::BracketRight => true,
            KeyCode::Equal
            | KeyCode::Minus
            | KeyCode::NumpadAdd
            | KeyCode::NumpadSubtract
            | KeyCode::Digit0
            | KeyCode::Numpad0
            | KeyCode::Digit1
            | KeyCode::Numpad1 => ctrl,
            _ => false,
        };
        if passes {
            return false;
        }
        if !pressed {
            return true;
        }
        let command = self
            .pressed_chord(physical_key)
            .and_then(|c| self.shell.keymap.command_for(c));
        match (code, command) {
            (_, Some(Command::EditUndo)) => self.refine_history_step(false),
            (_, Some(Command::EditRedo)) => self.refine_history_step(true),
            (_, Some(Command::FitScreen | Command::ZoomActual)) => return false,
            (KeyCode::Escape, _) if !repeat => {
                self.cancel_refine_panel();
                self.sync_cursor(event_loop);
            }
            (KeyCode::Enter | KeyCode::NumpadEnter, _) if !repeat => {
                self.commit_refine_panel();
                self.sync_cursor(event_loop);
            }
            (KeyCode::KeyF, _) if plain && !repeat => {
                self.edit.refine_view_mode = self.edit.refine_view_mode.next();
                self.edit.refine_show_original = false;
                self.invalidate_refine_overlay();
            }
            (KeyCode::KeyX, _) if plain && !repeat => {
                self.edit.refine_show_original = !self.edit.refine_show_original;
                self.invalidate_refine_overlay();
            }
            (_, Some(_)) if !repeat => self.deny_modal_action(),
            _ => {}
        }
        self.redraw_main();
        true
    }

    fn key_select_tool(&mut self, tool: ToolId, event_loop: &ActiveEventLoop) {
        self.edit.tools.select(tool);
        self.sync_cursor(event_loop);
        self.redraw_main();
    }

    fn key_select_group(&mut self, group: &[ToolId], event_loop: &ActiveEventLoop) {
        self.edit.tools.select_group(group);
        self.sync_cursor(event_loop);
        self.redraw_main();
    }

    fn key_adjustment(&mut self, adjustment: crate::core::layer::AdjustmentType) {
        if !self.begin_adjustment_preview(adjustment) {
            self.shell.status_msg = "Adjustment requires an unlocked raster layer".to_string();
        }
        self.redraw_main();
    }

    /// Ctrl+[ / Ctrl+] and with Shift: the active layer one step down or up,
    /// or to the bottom or top.
    fn key_move_layer(&mut self, to: &str) {
        let stack = &self.docs.active().canvas.layer_stack;
        let Some(id) = stack.layers.get(stack.active_idx).map(|l| l.id) else {
            return;
        };
        let _ = self.run_with("layer.move", serde_json::json!({"layer": id, "to": to}));
        self.redraw_main();
    }

    /// Alt+[ / Alt+]: the layer below or above the active one becomes active.
    fn key_pick_neighbour_layer(&mut self, above: bool) {
        let stack = &self.docs.active().canvas.layer_stack;
        let i = stack.active_idx;
        let next = if above {
            i.checked_add(1)
        } else {
            i.checked_sub(1)
        };
        let Some(id) = next.and_then(|j| stack.layers.get(j)).map(|l| l.id) else {
            return;
        };
        let _ = self.run_with(
            "layer.set",
            serde_json::json!({"layer": id, "activate": true}),
        );
        self.redraw_main();
    }

    /// Ctrl+Shift+E merges the visible layers into one; with Alt (`stamp`)
    /// they are stamped into a new one on top (Photoshop's).
    fn key_merge_visible(&mut self, stamp: bool) {
        let (what, name) = if stamp {
            ("visible", "Stamp Visible")
        } else {
            ("merge_visible", "Merge Visible")
        };
        if self.win.gpu.as_ref().is_some_and(|g| g.is_large_canvas) {
            self.shell.status_msg =
                format!("{name} không hỗ trợ canvas > 25M pixels (Viewport Streaming mode)");
        } else {
            let _ = self.run_with("layer.merge", serde_json::json!({"what": what}));
        }
        self.redraw_main();
    }

    /// Convert to Curves (Corel's Ctrl+Q): a parametric Shape becomes a
    /// path, a Text layer curves; both keep their own undo.
    fn key_convert_to_curves(&mut self) {
        use crate::core::layer::LayerType;
        use crate::core::vector::object::VectorGeometry;
        let idx = self.docs.active().canvas.layer_stack.active_idx;
        match self
            .docs
            .active()
            .canvas
            .layer_stack
            .layers
            .get(idx)
            .map(|l| &l.layer_type)
        {
            Some(LayerType::Vector(VectorGeometry::Primitive(_))) => {
                self.convert_shape_to_path(idx);
            }
            Some(LayerType::Text(_)) => {
                self.text_to_curves(idx);
            }
            _ => {
                self.shell.status_msg =
                    "Convert to Curves: chọn một Shape hoặc Text trước".to_string();
                self.redraw_main();
            }
        }
    }

    /// Run a re-bindable command. Each body is the one its built-in key arm
    /// ran, so a command behaves the same whichever key triggers it.
    pub(in crate::app) fn run_command(
        &mut self,
        cmd: Command,
        event_loop: &ActiveEventLoop,
        repeat: bool,
    ) {
        if let Some((id, args)) = cmd.engine_command() {
            // A held key runs it once.
            if !repeat {
                let _ = self.run_with(id, args);
                self.redraw_main();
            }
            return;
        }
        match cmd {
            Command::ToolMove => self.key_select_tool(ToolId::Move, event_loop),
            Command::ToolMarquee => self.key_select_group(crate::tools::SEL_GROUP, event_loop),
            Command::ToolLasso => self.key_select_group(crate::tools::LASSO_GROUP, event_loop),
            Command::ToolQuickSelect => self.key_select_tool(ToolId::SmartSelect, event_loop),
            Command::ToolCrop => {
                self.edit.tools.select_group(crate::tools::CROP_GROUP);
                let canvas = &self.docs.active().canvas;
                let cw = canvas.width;
                let ch = canvas.height;
                let dpi = canvas.metadata.resolution_ppi;
                match self.edit.tools.active_id() {
                    ToolId::Crop => {
                        // Same guard as the toolbar path: a fixed-size preset
                        // (ID photo at 600 ppi) or a hand-typed resolution must
                        // not be clobbered by the document's 72 ppi default.
                        let c = self.edit.tools.crop_mut();
                        c.sync_dpi_on_activate(dpi);
                    }
                    ToolId::PerspectiveCrop => {
                        let t = self.edit.tools.perspective_crop_mut();
                        t.sync_manual_pixels(cw as f32, ch as f32);
                        t.begin_placing();
                    }

                    _ => {}
                }
                self.sync_cursor(event_loop);
                self.redraw_main();
            }
            Command::ToolEyedropper => self.key_select_tool(ToolId::Eyedropper, event_loop),
            Command::ToolBrush => self.key_select_group(crate::tools::BRUSH_GROUP, event_loop),
            Command::ToolClone => self.key_select_tool(ToolId::Clone, event_loop),
            Command::ToolEraser => self.key_select_tool(ToolId::Eraser, event_loop),
            Command::ToolFill => self.key_select_group(crate::tools::FILL_GROUP, event_loop),
            Command::ToolDodge => self.key_select_group(crate::tools::DODGE_GROUP, event_loop),
            Command::ToolPen => self.key_select_tool(ToolId::Pen, event_loop),
            // Direct-selection: edit a Path layer's anchor points.
            Command::ToolNode => self.key_select_tool(ToolId::Node, event_loop),
            Command::ToolText => self.key_select_tool(ToolId::Text, event_loop),
            // Selects the shape group (keeps the last-used shape); the shape kind
            // is chosen by right-clicking the toolbar Shape tool. No key cycling —
            // that would jump shapes when re-selecting mid-edit.
            Command::ToolShape => self.key_select_group(crate::tools::SHAPE_GROUP, event_loop),
            Command::ToolRepair => self.key_select_tool(ToolId::Repair, event_loop),
            Command::ToolZoom => self.key_select_tool(ToolId::Zoom, event_loop),
            Command::ToolHand => self.key_select_tool(ToolId::Hand, event_loop),
            Command::FileNew => self.open_new_canvas_dialog_with_clipboard_hint(),
            Command::FileOpen => self.do_open(),
            Command::FileSave => self.do_save(),
            Command::FileSaveAs => self.do_save_as(),
            Command::FileClose => {
                self.close_doc(self.docs.active_doc_idx);
                self.redraw_main();
            }
            Command::FilePrint => {
                if !self.docs.documents.is_empty() {
                    self.open_print_dialog();
                }
            }
            Command::Preferences => {
                self.shell.ui.show_preferences = true;
                self.redraw_main();
            }
            Command::EditUndo => {
                // Free Transform owns Ctrl+Z while live: it reverts the
                // pending transform, never the history underneath it.
                if self.transform_undo_pending() {
                    self.redraw_main();
                    return;
                }
                // Warp (Liquify) owns Ctrl+Z while its modal is open: step back
                // one warp stroke instead of ringing the modal-lock bell.
                if self.edit.warp_state.is_some() {
                    self.warp_undo_stroke();
                    self.redraw_main();
                    return;
                }
                if self.modal_lock_active() {
                    self.deny_modal_action();
                    return;
                }
                // Pen tool: while a path is in progress, Ctrl+Z steps it back
                // one anchor instead of popping the document history.
                if self.edit.tools.active_id() == ToolId::Pen
                    && (!self.edit.tools.pen().is_empty() || self.edit.tools.pen().is_closed())
                    && self.edit.tools.pen_mut().undo_last_anchor()
                {
                    self.redraw_main();
                    return;
                }
                // The command an agent runs, as the menu's Undo: its events
                // carry the canvas-size bookkeeping (GPU texture, refit).
                let _ = self.run_now("edit.undo");
            }
            Command::EditRedo => {
                if self.modal_lock_active() {
                    self.deny_modal_action();
                    return;
                }
                let _ = self.run_now("edit.redo");
            }
            Command::EditCut => {
                if !repeat {
                    self.do_cut();
                }
            }
            Command::EditCopy => self.do_copy(),
            Command::EditPaste => self.do_paste(),
            Command::FreeTransform => {
                self.begin_transform();
                self.sync_cursor(event_loop);
            }
            Command::Levels => {
                self.key_adjustment(crate::core::layer::AdjustmentType::default_levels())
            }
            Command::AutoLevels => {
                self.do_auto_levels();
                self.redraw_main();
            }
            Command::Curves => {
                self.key_adjustment(crate::core::layer::AdjustmentType::default_curves())
            }
            Command::ColorBalance => {
                self.key_adjustment(crate::core::layer::AdjustmentType::ColorBalance {
                    shadows: [0.0; 3],
                    midtones: [0.0; 3],
                    highlights: [0.0; 3],
                    preserve_luminosity: true,
                })
            }
            Command::HueSaturation => self.key_adjustment(
                crate::core::layer::AdjustmentType::hue_saturation(0.0, 0.0, 0.0),
            ),
            // Engine commands, run above.
            Command::SelectAll
            | Command::LayerViaCopy
            | Command::Desaturate
            | Command::Invert
            | Command::SelectInverse => {}
            Command::LevelsLast => {
                let adj =
                    self.last_adjustment_like(crate::core::layer::AdjustmentType::default_levels());
                self.key_adjustment(adj)
            }
            Command::CurvesLast => {
                let adj =
                    self.last_adjustment_like(crate::core::layer::AdjustmentType::default_curves());
                self.key_adjustment(adj)
            }
            Command::HueSaturationLast => {
                let adj = self.last_adjustment_like(
                    crate::core::layer::AdjustmentType::hue_saturation(0.0, 0.0, 0.0),
                );
                self.key_adjustment(adj)
            }
            Command::ColorBalanceLast => {
                let adj =
                    self.last_adjustment_like(crate::core::layer::AdjustmentType::ColorBalance {
                        shadows: [0.0; 3],
                        midtones: [0.0; 3],
                        highlights: [0.0; 3],
                        preserve_luminosity: true,
                    });
                self.key_adjustment(adj)
            }
            Command::AutoContrast | Command::AutoColor if !repeat => {
                self.do_auto_contrast_or_color(cmd == Command::AutoColor);
            }
            Command::AutoContrast | Command::AutoColor => {}
            Command::BlackAndWhite => {
                self.key_adjustment(crate::core::layer::AdjustmentType::BlackAndWhite {
                    r: 40.0,
                    y: 60.0,
                    g: 40.0,
                    c: 60.0,
                    b: 20.0,
                    m: 80.0,
                })
            }
            Command::CanvasSize => {
                self.shell.ui.show_resize_dialog = true;
                self.redraw_main();
            }
            Command::BringForward if !repeat => self.key_move_layer("up"),
            Command::SendBackward if !repeat => self.key_move_layer("down"),
            Command::BringToFront if !repeat => self.key_move_layer("top"),
            Command::SendToBack if !repeat => self.key_move_layer("bottom"),
            Command::BringForward
            | Command::SendBackward
            | Command::BringToFront
            | Command::SendToBack => {}
            Command::SelectLayerAbove => self.key_pick_neighbour_layer(true),
            Command::SelectLayerBelow => self.key_pick_neighbour_layer(false),
            Command::Group if !repeat => self.do_group_selected(),
            Command::Ungroup if !repeat => {
                let idx = self.docs.active().canvas.layer_stack.active_idx;
                self.do_ungroup(idx);
            }
            // Photoshop clipping mask: clip the active layer to the one below.
            Command::ClippingMask if !repeat => {
                self.toggle_clipping_mask();
            }
            Command::MergeLayers if !repeat => {
                // Tile-native (256-px chunks): no large-canvas gate needed.
                let _ = self.run_with("layer.merge", serde_json::json!({"what": "picked"}));
                self.redraw_main();
            }
            Command::MergeVisible | Command::StampVisible if !repeat => {
                self.key_merge_visible(cmd == Command::StampVisible)
            }
            Command::ConvertToCurves if !repeat => self.key_convert_to_curves(),
            Command::ImageSize => {
                self.shell.ui.show_image_size_dialog = true;
                self.redraw_main();
            }
            Command::Warp if !repeat => {
                if !self.begin_warp() {
                    self.shell.status_msg = "Warp requires an unlocked raster layer".to_string();
                }
                self.sync_cursor(event_loop);
                self.redraw_main();
            }
            Command::RefineSelection if !repeat => {
                self.open_refine_panel();
                self.sync_cursor(event_loop);
            }
            Command::ProofColors if !repeat => {
                self.toggle_proof_colors();
                self.redraw_main();
            }
            Command::GamutWarning if !repeat => {
                self.toggle_gamut_warning();
                self.redraw_main();
            }
            Command::Group
            | Command::Ungroup
            | Command::ClippingMask
            | Command::MergeLayers
            | Command::MergeVisible
            | Command::StampVisible
            | Command::ConvertToCurves
            | Command::Warp
            | Command::RefineSelection
            | Command::ProofColors
            | Command::GamutWarning => {}
            Command::ToggleRulers => {
                self.shell.ui.show_rulers = !self.shell.ui.show_rulers;
                self.redraw_main();
            }
            Command::FitScreen => {
                self.fit_canvas_to_screen();
                self.redraw_main();
            }
            Command::ZoomActual => {
                self.edit.view.zoom = 1.0;
                self.push_canvas_uniforms();
                self.win.pending_view_change = true;
                self.redraw_main();
            }
            Command::OpenDevelop => {
                self.open_develop_window(event_loop);
                self.redraw_main();
            }
            // Run above, as the engine command it is.
            Command::Menu(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_release_egui_swallows_still_lets_go_of_the_held_keys() {
        // 19/10: a release egui consumed (a text field had focus) or a dialog
        // turned away never reached the router, so Alt stayed held — every
        // marquee / lasso subtracted and drew nothing — or Space did, and
        // every drag panned. `note_key` runs before either can swallow it.
        let mut app = App::new();
        let input = &mut app.edit.input;
        for key in [KeyCode::AltLeft, KeyCode::ShiftRight, KeyCode::ControlLeft] {
            assert!(input.note_key(PhysicalKey::Code(key), true));
        }
        // The router took the Space press and an Alt+right-drag began.
        input.space_held = true;
        input.space_dragging = true;
        input.alt_right_dragging = true;

        for key in [
            KeyCode::AltLeft,
            KeyCode::ShiftRight,
            KeyCode::ControlLeft,
            KeyCode::Space,
        ] {
            assert!(input.note_key(PhysicalKey::Code(key), false), "{key:?}");
        }
        assert!(!input.alt_held && !input.shift_held && !input.ctrl_held);
        assert!(!input.space_held && !input.space_dragging && !input.alt_right_dragging);
        assert!(!input.note_key(PhysicalKey::Code(KeyCode::Space), false));
    }

    #[test]
    fn a_space_typed_into_a_field_does_not_arm_panning() {
        let mut app = App::new();
        let input = &mut app.edit.input;
        assert!(!input.note_key(PhysicalKey::Code(KeyCode::Space), true));
        assert!(!input.space_held);
        assert!(!input.note_key(PhysicalKey::Code(KeyCode::KeyA), true));
    }

    #[test]
    fn every_bindable_key_has_a_physical_key() {
        let codes = [
            KeyCode::KeyA,
            KeyCode::KeyB,
            KeyCode::KeyC,
            KeyCode::KeyD,
            KeyCode::KeyE,
            KeyCode::KeyF,
            KeyCode::KeyG,
            KeyCode::KeyH,
            KeyCode::KeyI,
            KeyCode::KeyJ,
            KeyCode::KeyK,
            KeyCode::KeyL,
            KeyCode::KeyM,
            KeyCode::KeyN,
            KeyCode::KeyO,
            KeyCode::KeyP,
            KeyCode::KeyQ,
            KeyCode::KeyR,
            KeyCode::KeyS,
            KeyCode::KeyT,
            KeyCode::KeyU,
            KeyCode::KeyV,
            KeyCode::KeyW,
            KeyCode::KeyX,
            KeyCode::KeyY,
            KeyCode::KeyZ,
            KeyCode::Digit0,
            KeyCode::Digit1,
            KeyCode::Digit2,
            KeyCode::Digit3,
            KeyCode::Digit4,
            KeyCode::Digit5,
            KeyCode::Digit6,
            KeyCode::Digit7,
            KeyCode::Digit8,
            KeyCode::Digit9,
            KeyCode::F1,
            KeyCode::F2,
            KeyCode::F3,
            KeyCode::F4,
            KeyCode::F5,
            KeyCode::F6,
            KeyCode::F7,
            KeyCode::F8,
            KeyCode::F9,
            KeyCode::F10,
            KeyCode::F11,
            KeyCode::F12,
            KeyCode::Comma,
            KeyCode::Period,
            KeyCode::Slash,
            KeyCode::Semicolon,
            KeyCode::Quote,
            KeyCode::Backquote,
            KeyCode::Backslash,
            KeyCode::BracketLeft,
            KeyCode::BracketRight,
        ];
        let mapped: std::collections::HashSet<KeyName> =
            codes.into_iter().filter_map(key_name).collect();
        for key in KeyName::ALL {
            assert!(mapped.contains(&key), "{key:?} cannot be pressed");
        }
    }

    #[test]
    fn numpad_digits_count_as_digits_and_context_keys_are_not_bindable() {
        assert_eq!(key_name(KeyCode::Numpad0), Some(KeyName::Digit0));
        assert_eq!(key_name(KeyCode::Numpad7), Some(KeyName::Digit7));
        for code in [
            KeyCode::Enter,
            KeyCode::Escape,
            KeyCode::Space,
            KeyCode::Delete,
            KeyCode::ArrowUp,
            KeyCode::Equal,
            KeyCode::Minus,
            KeyCode::Tab,
        ] {
            assert_eq!(key_name(code), None, "{code:?}");
        }
        // [ and ] take a key with Ctrl or Alt (layer order, as Photoshop);
        // alone or with Shift they stay the tip's size and hardness.
        use crate::app::commands::{KeyChord, reserved_action};
        for key in [KeyName::BracketLeft, KeyName::BracketRight] {
            assert!(reserved_action(KeyChord::plain(key)).is_some());
            assert!(reserved_action(KeyChord::new(false, true, false, key)).is_some());
            assert!(reserved_action(KeyChord::ctrl(key)).is_none());
        }
    }

    #[test]
    fn brackets_resize_the_tool_in_hand_not_the_brush() {
        let mut app = App::new();
        let settings = &app.edit.tools.brush().settings;
        let brush = (settings.size, settings.hardness);
        let tools = &mut app.edit.tools;
        tools.smudge_mut().size = 40.0;
        tools.smudge_mut().hardness = 0.5;
        tools.wand_mut().brush_size = 40.0;

        app.edit.tools.select(ToolId::Smudge);
        app.bracket_tip_size(true);
        app.bracket_tip_hardness(false);
        let smudge = app.edit.tools.smudge();
        assert_eq!((smudge.size, smudge.hardness), (45.0, 0.25));

        for (tool, grow, size, hardness) in [
            (ToolId::Dodge, false, 35.0, 0.75),
            (ToolId::Burn, true, 45.0, 0.75),
        ] {
            // Dodge and Burn each keep their own tip.
            app.edit.tools.select(tool);
            app.edit.tools.dodge_burn_mut().size = 40.0;
            app.edit.tools.dodge_burn_mut().hardness = 0.5;
            app.bracket_tip_size(grow);
            app.bracket_tip_hardness(true);
            let t = app.edit.tools.dodge_burn();
            assert_eq!((t.size, t.hardness), (size, hardness), "{tool:?}");
        }

        app.edit.tools.select(ToolId::SmartSelect);
        app.bracket_tip_size(true);
        app.bracket_tip_hardness(true);
        assert_eq!(app.edit.tools.wand().brush_size, 45.0);

        let after = &app.edit.tools.brush().settings;
        assert_eq!((after.size, after.hardness), brush);

        // The Brush itself, and a size past the keys' ceiling only comes down.
        app.edit.tools.select(ToolId::Brush);
        app.bracket_tip_size(true);
        assert_eq!(app.edit.tools.brush().settings.size, brush.0 + 2.0);
        app.edit.tools.brush_mut().settings.size = 800.0;
        app.bracket_tip_size(false);
        assert_eq!(app.edit.tools.brush().settings.size, 798.0);
    }

    #[test]
    fn pencil_draws_with_the_brush_size_and_colour() {
        use crate::tools::{PointerEvent, ToolCtx};
        let mut app = App::new();
        let (w, h) = (120u32, 120u32);
        app.docs.documents[0].canvas =
            crate::core::canvas::Canvas::from_rgba(vec![255; (w * h * 4) as usize], w, h);
        app.edit.tools.select(ToolId::Pencil);
        app.edit.tools.brush_mut().settings.size = 40.0;
        app.edit.tools.brush_mut().settings.color = [200, 30, 30, 255];
        assert_eq!(app.edit.tools.cursor_size(), 20.0);
        // The keys resize what the Pencil draws with.
        app.bracket_tip_size(true);
        assert_eq!(app.edit.tools.cursor_size(), 21.0);
        {
            let (fg, bg) = ([200, 30, 30, 255], [255; 4]);
            let mut ctx = ToolCtx::new(&mut app.docs.documents[0], fg, bg, 1.0, 0.0, 0.0);
            app.edit
                .tools
                .on_press(PointerEvent::new(60.0, 60.0), &mut ctx);
            app.edit
                .tools
                .on_release(PointerEvent::new(60.0, 60.0), &mut ctx);
            ctx.canvas_mut().end_stroke();
        }
        let tiles = &app.docs.documents[0].canvas.active_layer().tiles;
        assert_eq!(tiles.get_pixel(60, 60), (200, 30, 30, 255));
        assert_eq!(tiles.get_pixel(75, 60), (200, 30, 30, 255));
        assert_eq!(tiles.get_pixel(95, 60), (255, 255, 255, 255));
    }
}

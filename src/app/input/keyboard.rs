//! Keyboard input for the main window, extracted verbatim from the
//! window_event match arm.
//!
//! Re-bindable commands (see `app::commands`) keep their original key arms
//! below, each gated by `run_default` so it only fires while the command still
//! has its built-in key. A binding the user changed in Preferences is routed
//! first, through `custom_command_for` → `run_command`, which holds the same
//! command bodies.

use crate::app::commands::{Command, KeyChord, KeyName};
use crate::app::state::App;
use crate::extension::tool::ToolCtx;
use crate::tools::ToolId;
use winit::{
    event::ElementState,
    event_loop::ActiveEventLoop,
    keyboard::{KeyCode, PhysicalKey},
};

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
        if self.docs.documents[self.docs.active_doc_idx].is_flow_text() {
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
        if pressed && self.is_preview_dialog_open() {
            let is_view_shortcut = matches!(
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
            ) || matches!(
                self.custom_command_for(physical_key),
                Some(Command::FitScreen | Command::ZoomActual)
            );
            if !is_view_shortcut {
                return;
            }
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
        if pressed {
            if let Some(cmd) = self.custom_command_for(physical_key) {
                self.run_command(cmd, event_loop, repeat);
                return;
            }
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
                if self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .nudge_selected_layers(dx, dy)
                {
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
            // Multi-page PDF: Shift+Delete covers the same hard rectangular
            // selection on every page without rendering/caching every page.
            PhysicalKey::Code(KeyCode::Delete) | PhysicalKey::Code(KeyCode::Backspace)
                if pressed
                    && !repeat
                    && self.edit.input.shift_held
                    && !self.edit.input.alt_held
                    && !self.edit.input.ctrl_held
                    && self.docs.documents[self.docs.active_doc_idx]
                        .pdf_document
                        .is_some()
                    && self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .selection
                        .active =>
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
                let idx = self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .active_idx;
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
                if self.edit.input.alt_held {
                    self.edit.pending_fill = Some(self.edit.tools.brush().settings.color);
                } else if self.edit.input.ctrl_held {
                    self.edit.pending_fill = Some(self.edit.bg_color);
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
                    let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
                    let has_sel = canvas.selection.active;
                    let is_bg = !canvas.layer_stack.layers.is_empty()
                        && canvas.active_layer().is_background;
                    if has_sel && !over_panel && is_bg {
                        self.edit.pending_fill = Some(self.edit.bg_color);
                    } else if has_sel && !over_panel {
                        if self.docs.documents[self.docs.active_doc_idx]
                            .canvas
                            .clear_selection()
                        {
                            self.apply_canvas_event(
                                crate::app::render::CanvasEvent::LayerPixelsChanged,
                            );
                        }
                    } else if self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .remove_active_layer()
                    {
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
                let has_selection = self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .selection
                    .active;
                if self.edit.input.shift_held {
                    // Ctrl+Shift+D: always Repeat (explicit).
                    self.repeat_last_step();
                } else if has_selection {
                    // Ctrl+D with an active pixel selection = Deselect (Photoshop).
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .deselect();
                    self.upload_selection_mask();
                    self.push_selection_uniforms();
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
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .invert_selection();
                self.upload_selection_mask();
                self.push_selection_uniforms();
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
            PhysicalKey::Code(KeyCode::KeyE)
                if pressed && self.edit.input.ctrl_held && self.edit.input.shift_held =>
            {
                let is_large = self.win.gpu.as_ref().map_or(false, |g| g.is_large_canvas);
                if is_large {
                    self.shell.status_msg =
                        "Merge Visible không hỗ trợ canvas > 25M pixels (Viewport Streaming mode)"
                            .to_string();
                } else if self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .merge_visible()
                {
                    self.apply_canvas_event(crate::app::render::CanvasEvent::LayerStructureChanged);
                }
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyE) if pressed && self.edit.input.ctrl_held => {
                let is_large = self.win.gpu.as_ref().map_or(false, |g| g.is_large_canvas);
                if is_large {
                    self.shell.status_msg =
                        "Merge không hỗ trợ canvas > 25M pixels (Viewport Streaming mode)"
                            .to_string();
                    if let Some(w) = &self.win.window {
                        w.request_redraw();
                    }
                } else if self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .merge_selected()
                {
                    self.apply_canvas_event(crate::app::render::CanvasEvent::LayerStructureChanged);
                }
            }
            PhysicalKey::Code(KeyCode::KeyV) if pressed && !self.edit.input.ctrl_held => {
                self.run_default(Command::ToolMove, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyI) if pressed && self.edit.input.ctrl_held => {
                self.run_default(Command::Invert, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyI) if pressed => {
                self.run_default(Command::ToolEyedropper, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyG)
                if pressed && self.edit.input.ctrl_held && self.edit.input.shift_held =>
            {
                let idx = self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .active_idx;
                self.do_ungroup(idx);
            }
            PhysicalKey::Code(KeyCode::KeyG)
                if pressed
                    && self.edit.input.ctrl_held
                    && self.edit.input.alt_held
                    && !self.edit.input.shift_held =>
            {
                // Photoshop clipping mask: clip the active layer to the one below.
                self.toggle_clipping_mask();
            }
            PhysicalKey::Code(KeyCode::KeyG)
                if pressed && self.edit.input.ctrl_held && !self.edit.input.alt_held =>
            {
                self.do_group_selected();
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
            PhysicalKey::Code(KeyCode::KeyR) if pressed && self.edit.input.ctrl_held => {
                self.run_default(Command::ToggleRulers, event_loop, repeat);
            }
            PhysicalKey::Code(KeyCode::KeyT) if pressed && !self.edit.input.ctrl_held => {
                self.run_default(Command::ToolText, event_loop, repeat);
            }

            PhysicalKey::Code(KeyCode::KeyX)
                if pressed
                    && !repeat
                    && self.edit.input.ctrl_held
                    && self.edit.input.shift_held =>
            {
                if !self.begin_warp() {
                    self.shell.status_msg = "Warp requires an unlocked raster layer".to_string();
                }
                self.sync_cursor(event_loop);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyX) if pressed && !repeat && self.edit.input.ctrl_held => {
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
            // Ctrl+Y → Proof Colors (redo lives on Ctrl+Shift+Z).
            PhysicalKey::Code(KeyCode::KeyY)
                if pressed && self.edit.input.ctrl_held && !self.edit.input.shift_held =>
            {
                self.shell.proof_enabled = !self.shell.proof_enabled;
                self.apply_proof_settings();
                self.shell.status_msg = if self.shell.proof_enabled {
                    format!("Proof Colors on — {}", self.shell.proof_target.label())
                } else {
                    "Proof Colors off".to_string()
                };
            }
            // Ctrl+Shift+Y → Gamut Warning.
            PhysicalKey::Code(KeyCode::KeyY)
                if pressed && self.edit.input.ctrl_held && self.edit.input.shift_held =>
            {
                self.shell.proof_gamut_warn = !self.shell.proof_gamut_warn;
                if self.shell.proof_gamut_warn {
                    self.shell.proof_enabled = true;
                }
                self.apply_proof_settings();
                self.shell.status_msg = if self.shell.proof_gamut_warn {
                    "Gamut Warning on".to_string()
                } else {
                    "Gamut Warning off".to_string()
                };
            }
            // Ctrl+P → Print dialog.
            PhysicalKey::Code(KeyCode::KeyP) if pressed && self.edit.input.ctrl_held => {
                self.run_default(Command::FilePrint, event_loop, repeat);
            }
            // Ctrl+Q → Convert to Curves (Corel muscle memory). Routes to the
            // shape→path conversion for a parametric Shape, or text→curves for a
            // Text layer; both already handle their own undo + invalidation.
            PhysicalKey::Code(KeyCode::KeyQ) if pressed && !repeat && self.edit.input.ctrl_held => {
                use crate::core::layer::LayerType;
                use crate::core::vector::object::VectorGeometry;
                let idx = self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .active_idx;
                let kind = self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers
                    .get(idx)
                    .map(|l| match &l.layer_type {
                        LayerType::Vector(VectorGeometry::Primitive(_)) => 1u8,
                        LayerType::Text(_) => 2,
                        _ => 0,
                    })
                    .unwrap_or(0);
                match kind {
                    1 => {
                        self.convert_shape_to_path(idx);
                    }
                    2 => {
                        self.text_to_curves(idx);
                    }
                    _ => {
                        self.shell.status_msg =
                            "Convert to Curves: chọn một Shape hoặc Text trước".to_string();
                        if let Some(w) = &self.win.window {
                            w.request_redraw();
                        }
                    }
                }
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
            PhysicalKey::Code(KeyCode::BracketLeft) if pressed && !self.edit.input.shift_held => {
                if self.edit.show_refine_panel {
                    self.edit.tools.refine_brush_mut().size =
                        (self.edit.tools.refine_brush().size - 2.0).max(1.0);
                } else if self.edit.tools.active_id() == ToolId::Eraser {
                    self.edit.tools.eraser_mut().size =
                        (self.edit.tools.eraser().size - 2.0).max(1.0);
                } else if matches!(self.edit.tools.active_id(), ToolId::Clone | ToolId::Repair) {
                    let size = self.edit.tools.clone_like().size;
                    self.edit.tools.clone_like_mut().size =
                        (size - clone_bracket_step(size, false)).max(1.0);
                } else {
                    self.edit.tools.brush_mut().settings.size =
                        (self.edit.tools.brush().settings.size - 2.0).max(1.0);
                }
                self.win.last_cursor_radius = 0;
                self.sync_cursor(event_loop);
            }
            PhysicalKey::Code(KeyCode::BracketRight) if pressed && !self.edit.input.shift_held => {
                if self.edit.show_refine_panel {
                    self.edit.tools.refine_brush_mut().size =
                        (self.edit.tools.refine_brush().size + 2.0).min(500.0);
                } else if self.edit.tools.active_id() == ToolId::Eraser {
                    self.edit.tools.eraser_mut().size =
                        (self.edit.tools.eraser().size + 2.0).min(5000.0);
                } else if matches!(self.edit.tools.active_id(), ToolId::Clone | ToolId::Repair) {
                    let size = self.edit.tools.clone_like().size;
                    self.edit.tools.clone_like_mut().size =
                        (size + clone_bracket_step(size, true)).min(5000.0);
                } else {
                    self.edit.tools.brush_mut().settings.size =
                        (self.edit.tools.brush().settings.size + 2.0).min(300.0);
                }
                self.win.last_cursor_radius = 0;
                self.sync_cursor(event_loop);
            }
            PhysicalKey::Code(KeyCode::BracketLeft) if pressed && self.edit.input.shift_held => {
                if self.edit.show_refine_panel {
                    self.edit.tools.refine_brush_mut().hardness =
                        (self.edit.tools.refine_brush().hardness - 0.1).max(0.0);
                } else if self.edit.tools.active_id() == ToolId::Eraser {
                    self.edit.tools.eraser_mut().hardness =
                        (self.edit.tools.eraser().hardness - 0.1).max(0.0);
                } else if matches!(self.edit.tools.active_id(), ToolId::Clone | ToolId::Repair) {
                    let t = self.edit.tools.clone_like_mut();
                    t.hardness = (t.hardness - 0.25).max(0.0);
                } else {
                    self.edit.tools.brush_mut().settings.hardness =
                        (self.edit.tools.brush().settings.hardness - 0.1).max(0.0);
                }
            }
            PhysicalKey::Code(KeyCode::BracketRight) if pressed && self.edit.input.shift_held => {
                if self.edit.show_refine_panel {
                    self.edit.tools.refine_brush_mut().hardness =
                        (self.edit.tools.refine_brush().hardness + 0.1).min(1.0);
                } else if self.edit.tools.active_id() == ToolId::Eraser {
                    self.edit.tools.eraser_mut().hardness =
                        (self.edit.tools.eraser().hardness + 0.1).min(1.0);
                } else if matches!(self.edit.tools.active_id(), ToolId::Clone | ToolId::Repair) {
                    let t = self.edit.tools.clone_like_mut();
                    t.hardness = (t.hardness + 0.25).min(1.0);
                } else {
                    self.edit.tools.brush_mut().settings.hardness =
                        (self.edit.tools.brush().settings.hardness + 0.1).min(1.0);
                }
            }
            PhysicalKey::Code(KeyCode::ArrowUp)
                if pressed
                    && self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .selection
                        .active =>
            {
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .selection
                    .offset
                    .1 -= 1;
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .selection
                    .mark_bbox_dirty();
                let cmd = crate::core::command::TranslateSelectionCommand::from_applied_move(
                    &self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .selection,
                    0,
                    -1,
                );
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .record(Box::new(cmd));
                self.upload_selection_mask();
                self.push_selection_uniforms();
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::ArrowDown)
                if pressed
                    && self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .selection
                        .active =>
            {
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .selection
                    .offset
                    .1 += 1;
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .selection
                    .mark_bbox_dirty();
                let cmd = crate::core::command::TranslateSelectionCommand::from_applied_move(
                    &self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .selection,
                    0,
                    1,
                );
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .record(Box::new(cmd));
                self.upload_selection_mask();
                self.push_selection_uniforms();
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::ArrowLeft)
                if pressed
                    && self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .selection
                        .active =>
            {
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .selection
                    .offset
                    .0 -= 1;
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .selection
                    .mark_bbox_dirty();
                let cmd = crate::core::command::TranslateSelectionCommand::from_applied_move(
                    &self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .selection,
                    -1,
                    0,
                );
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .record(Box::new(cmd));
                self.upload_selection_mask();
                self.push_selection_uniforms();
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::ArrowRight)
                if pressed
                    && self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .selection
                        .active =>
            {
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .selection
                    .offset
                    .0 += 1;
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .selection
                    .mark_bbox_dirty();
                let cmd = crate::core::command::TranslateSelectionCommand::from_applied_move(
                    &self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .selection,
                    1,
                    0,
                );
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .record(Box::new(cmd));
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
                                &mut self.docs.documents[self.docs.active_doc_idx],
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
                        self.docs.documents[self.docs.active_doc_idx]
                            .canvas
                            .selection
                            .refresh_bbox();
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
        _ => return None,
    })
}

impl App {
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

    /// Run a re-bindable command. Each body is the one its built-in key arm
    /// ran, so a command behaves the same whichever key triggers it.
    pub(in crate::app) fn run_command(
        &mut self,
        cmd: Command,
        event_loop: &ActiveEventLoop,
        repeat: bool,
    ) {
        match cmd {
            Command::ToolMove => self.key_select_tool(ToolId::Move, event_loop),
            Command::ToolMarquee => self.key_select_group(crate::tools::SEL_GROUP, event_loop),
            Command::ToolLasso => self.key_select_group(crate::tools::LASSO_GROUP, event_loop),
            Command::ToolQuickSelect => self.key_select_tool(ToolId::SmartSelect, event_loop),
            Command::ToolCrop => {
                self.edit.tools.select_group(crate::tools::CROP_GROUP);
                let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
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
                        // Freeze DPI once a size is typed (see actions.rs) so
                        // cm/inch values stay put across different-DPI images.
                        if t.manual_size.is_none() {
                            t.dpi = dpi;
                        }
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
                // Same event path as the menu/panel Undo — it carries
                // the full canvas-size bookkeeping (GPU texture resize,
                // refit, flatten) the old inline copy here skipped.
                self.sync_brush_gpu_to_cpu();
                self.docs.documents[self.docs.active_doc_idx].canvas.undo();
                self.apply_canvas_event(crate::app::render::CanvasEvent::LayerStructureChanged);
                self.apply_canvas_event(crate::app::render::CanvasEvent::SelectionChanged);
            }
            Command::EditRedo => {
                if self.modal_lock_active() {
                    self.deny_modal_action();
                    return;
                }
                self.sync_brush_gpu_to_cpu();
                self.docs.documents[self.docs.active_doc_idx].canvas.redo();
                self.apply_canvas_event(crate::app::render::CanvasEvent::LayerStructureChanged);
                self.apply_canvas_event(crate::app::render::CanvasEvent::SelectionChanged);
            }
            Command::EditCut => {
                if !repeat {
                    self.do_cut();
                }
            }
            Command::EditCopy => self.do_copy(),
            Command::EditPaste => self.do_paste(),
            Command::SelectAll => {
                let (cw, ch) = {
                    let d = &self.docs.documents[self.docs.active_doc_idx];
                    (d.canvas.width, d.canvas.height)
                };
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .selection
                    .select_all();
                self.apply_canvas_event(crate::app::render::CanvasEvent::SelectionChanged);
                self.shell.status_msg = format!("Selected all ({}×{})", cw, ch);
                self.redraw_main();
            }
            Command::FreeTransform => {
                self.begin_transform();
                self.sync_cursor(event_loop);
            }
            Command::LayerViaCopy => {
                if repeat {
                    return;
                }
                if self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .duplicate_active_group()
                {
                    self.apply_canvas_event(crate::app::render::CanvasEvent::LayerStructureChanged);
                    self.redraw_main();
                } else {
                    let had_selection = self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .selection
                        .active;
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .begin_undo_group("Layer via Copy");
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_via_copy();
                    if had_selection {
                        self.docs.documents[self.docs.active_doc_idx]
                            .canvas
                            .deselect();
                    }
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .end_undo_group();
                    self.apply_canvas_event(crate::app::render::CanvasEvent::LayerStructureChanged);
                    if had_selection {
                        self.apply_canvas_event(crate::app::render::CanvasEvent::SelectionChanged);
                    }
                }
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
            Command::HueSaturation => {
                self.key_adjustment(crate::core::layer::AdjustmentType::HueSaturation {
                    hue: 0.0,
                    saturation: 0.0,
                    lightness: 0.0,
                })
            }
            Command::Desaturate => {
                let ok = self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .apply_adjustment_to_active_layer(
                        crate::core::layer::AdjustmentType::Desaturate,
                    );
                if ok {
                    self.apply_canvas_event(crate::app::render::CanvasEvent::LayerPixelsChanged);
                } else {
                    self.shell.status_msg =
                        "Desaturate requires an unlocked raster layer".to_string();
                }
                self.redraw_main();
            }
            Command::Invert => {
                let idx = self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .active_idx;
                self.do_invert_active(idx);
                self.redraw_main();
            }
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            KeyCode::BracketLeft,
            KeyCode::Equal,
            KeyCode::Minus,
            KeyCode::Tab,
        ] {
            assert_eq!(key_name(code), None, "{code:?}");
        }
    }
}

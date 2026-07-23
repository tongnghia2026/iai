//! Keyboard input for the main window, extracted verbatim from the
//! window_event match arm.

use crate::app::state::App;
use crate::extension::tool::ToolCtx;
use crate::tools::ToolId;
use winit::{
    event::ElementState,
    event_loop::ActiveEventLoop,
    keyboard::{KeyCode, PhysicalKey},
};

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
        if self.shell.ui.show_welcome
            && !self.shell.ui.show_new_dialog
            && pressed
            && self.edit.input.ctrl_held
            && matches!(physical_key, PhysicalKey::Code(KeyCode::KeyO))
        {
            self.do_open();
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return;
        }
        // Ctrl+A in the Library grid selects every thumbnail (not the hidden
        // document's pixels). Intercept before the editor's Select All below.
        if self.shell.ui.show_library
            && pressed
            && self.edit.input.ctrl_held
            && !self.edit.input.shift_held
            && matches!(physical_key, PhysicalKey::Code(KeyCode::KeyA))
        {
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
            );
            if !is_allowed {
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
                if self.edit.input.ctrl_held {
                    if !self.begin_adjustment_preview(
                        crate::core::layer::AdjustmentType::default_curves(),
                    ) {
                        self.shell.status_msg =
                            "Adjustment requires an unlocked raster layer".to_string();
                    }
                } else {
                    self.edit.tools.select_group(crate::tools::SEL_GROUP);
                    self.sync_cursor(event_loop);
                }
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyL) if pressed => {
                if self.edit.input.ctrl_held && self.edit.input.shift_held {
                    self.do_auto_levels();
                } else if self.edit.input.ctrl_held {
                    if !self.begin_adjustment_preview(
                        crate::core::layer::AdjustmentType::default_levels(),
                    ) {
                        self.shell.status_msg =
                            "Adjustment requires an unlocked raster layer".to_string();
                    }
                } else {
                    self.edit.tools.select_group(crate::tools::LASSO_GROUP);
                    self.sync_cursor(event_loop);
                }
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyW) if pressed && self.edit.input.ctrl_held => {
                self.close_doc(self.docs.active_doc_idx);
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
                if pressed && !repeat =>
            {
                if self.edit.input.alt_held {
                    self.edit.pending_fill = Some(self.edit.tools.brush().settings.color);
                } else if self.edit.input.ctrl_held {
                    self.edit.pending_fill = Some(self.edit.bg_color);
                } else {
                    // Plain Delete: with an active selection, clear ONLY that
                    // region — transparent on a normal layer, or bg-filled on the
                    // opaque Background (it can't hold transparency). Only with no
                    // selection does Delete remove the whole layer.
                    let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
                    let has_sel = canvas.selection.active;
                    let is_bg = !canvas.layer_stack.layers.is_empty()
                        && canvas.active_layer().is_background;
                    if has_sel && is_bg {
                        self.edit.pending_fill = Some(self.edit.bg_color);
                    } else if has_sel {
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
                if self.edit.input.shift_held {
                    if let Some(delta) = self.edit.tools.move_tool().last_duplicate_delta {
                        self.duplicate_selected_with_step(delta);
                    } else {
                        self.shell.status_msg = "No duplicate step to repeat".to_string();
                    }
                } else {
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .deselect();
                    self.upload_selection_mask();
                    self.push_selection_uniforms();
                }
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
                self.edit.tools.select(ToolId::SmartSelect);
                self.sync_cursor(event_loop);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyB) if pressed => {
                if self.edit.input.ctrl_held {
                    if !self.begin_adjustment_preview(
                        crate::core::layer::AdjustmentType::ColorBalance {
                            shadows: [0.0; 3],
                            midtones: [0.0; 3],
                            highlights: [0.0; 3],
                            preserve_luminosity: true,
                        },
                    ) {
                        self.shell.status_msg =
                            "Adjustment requires an unlocked raster layer".to_string();
                    }
                } else {
                    self.edit.tools.select_group(crate::tools::BRUSH_GROUP);
                    self.sync_cursor(event_loop);
                }
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyE) if pressed && !self.edit.input.ctrl_held => {
                self.edit.tools.select(ToolId::Eraser);
                self.sync_cursor(event_loop);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
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
                self.edit.tools.select(ToolId::Move);
                self.sync_cursor(event_loop);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyI) if pressed && self.edit.input.ctrl_held => {
                let idx = self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .active_idx;
                self.do_invert_active(idx);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyI) if pressed => {
                self.edit.tools.select(ToolId::Eyedropper);
                self.sync_cursor(event_loop);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
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
            PhysicalKey::Code(KeyCode::KeyG) if pressed && self.edit.input.ctrl_held => {
                self.do_group_selected();
            }
            PhysicalKey::Code(KeyCode::KeyG) if pressed && !self.edit.input.ctrl_held => {
                self.edit.tools.select_group(crate::tools::FILL_GROUP);
                self.sync_cursor(event_loop);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyC) if pressed && !self.edit.input.ctrl_held => {
                self.edit.tools.select_group(crate::tools::CROP_GROUP);
                let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
                let cw = canvas.width;
                let ch = canvas.height;
                let dpi = canvas.metadata.resolution_ppi;
                match self.edit.tools.active_id() {
                    ToolId::Crop => {
                        let c = self.edit.tools.crop_mut();
                        c.dpi = dpi;
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
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyP) if pressed && !self.edit.input.ctrl_held => {
                self.edit.tools.select(ToolId::Pen);
                self.sync_cursor(event_loop);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyA) if pressed && !self.edit.input.ctrl_held => {
                // Direct-selection: edit a Path layer's anchor points.
                self.edit.tools.select(ToolId::Node);
                self.sync_cursor(event_loop);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
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
                self.edit.tools.select(ToolId::Zoom);
                self.sync_cursor(event_loop);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyH) if pressed && !self.edit.input.ctrl_held => {
                self.edit.tools.select(ToolId::Hand);
                self.sync_cursor(event_loop);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyR) if pressed && self.edit.input.ctrl_held => {
                self.shell.ui.show_rulers = !self.shell.ui.show_rulers;
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyT) if pressed && !self.edit.input.ctrl_held => {
                self.edit.tools.select(ToolId::Text);
                self.shell.ui.show_text_panel = true;
                self.sync_cursor(event_loop);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
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
                self.do_cut();
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
                self.edit.tools.select(ToolId::Clone);
                self.sync_cursor(event_loop);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyS)
                if pressed && self.edit.input.ctrl_held && !self.edit.input.shift_held =>
            {
                self.do_save();
            }
            PhysicalKey::Code(KeyCode::KeyS)
                if pressed && self.edit.input.ctrl_held && self.edit.input.shift_held =>
            {
                self.do_save_as();
            }
            PhysicalKey::Code(KeyCode::KeyO) if pressed && self.edit.input.ctrl_held => {
                self.do_open();
            }
            PhysicalKey::Code(KeyCode::KeyO) if pressed && !self.edit.input.ctrl_held => {
                self.edit.tools.select_group(crate::tools::DODGE_GROUP);
                self.sync_cursor(event_loop);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyN) if pressed && self.edit.input.ctrl_held => {
                self.open_new_canvas_dialog_with_clipboard_hint();
            }
            PhysicalKey::Code(KeyCode::Digit0) if pressed && self.edit.input.ctrl_held => {
                self.fit_canvas_to_screen();
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::Numpad0) if pressed && self.edit.input.ctrl_held => {
                self.fit_canvas_to_screen();
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::Digit1) if pressed && self.edit.input.ctrl_held => {
                self.edit.view.zoom = 1.0;
                self.push_canvas_uniforms();
                self.win.pending_view_change = true;
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyZ)
                if pressed && self.edit.input.ctrl_held && !self.edit.input.shift_held =>
            {
                // Free Transform owns Ctrl+Z while live: it reverts the
                // pending transform, never the history underneath it.
                if self.transform_undo_pending() {
                    if let Some(w) = &self.win.window {
                        w.request_redraw();
                    }
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
                {
                    if self.edit.tools.pen_mut().undo_last_anchor() {
                        if let Some(w) = &self.win.window {
                            w.request_redraw();
                        }
                        return;
                    }
                }
                // Same event path as the menu/panel Undo — it carries
                // the full canvas-size bookkeeping (GPU texture resize,
                // refit, flatten) the old inline copy here skipped.
                self.sync_brush_gpu_to_cpu();
                self.docs.documents[self.docs.active_doc_idx].canvas.undo();
                self.apply_canvas_event(crate::app::render::CanvasEvent::LayerStructureChanged);
                self.apply_canvas_event(crate::app::render::CanvasEvent::SelectionChanged);
            }
            PhysicalKey::Code(KeyCode::KeyZ)
                if pressed && self.edit.input.ctrl_held && self.edit.input.shift_held =>
            {
                if self.modal_lock_active() {
                    self.deny_modal_action();
                    return;
                }
                self.sync_brush_gpu_to_cpu();
                self.docs.documents[self.docs.active_doc_idx].canvas.redo();
                self.apply_canvas_event(crate::app::render::CanvasEvent::LayerStructureChanged);
                self.apply_canvas_event(crate::app::render::CanvasEvent::SelectionChanged);
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
                if !self.docs.documents.is_empty() {
                    self.open_print_dialog();
                }
            }
            PhysicalKey::Code(KeyCode::KeyA)
                if pressed && self.edit.input.ctrl_held && self.edit.input.shift_held =>
            {
                self.open_develop_window(event_loop);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyA) if pressed && self.edit.input.ctrl_held => {
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
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyC) if pressed && self.edit.input.ctrl_held => {
                self.do_copy();
            }
            PhysicalKey::Code(KeyCode::KeyV) if pressed && self.edit.input.ctrl_held => {
                self.do_paste();
            }
            PhysicalKey::Code(KeyCode::KeyT) if pressed && self.edit.input.ctrl_held => {
                self.begin_transform();
                self.sync_cursor(event_loop);
            }
            PhysicalKey::Code(KeyCode::KeyU)
                if pressed && self.edit.input.ctrl_held && self.edit.input.shift_held =>
            {
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
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyU) if pressed && self.edit.input.ctrl_held => {
                if !self.begin_adjustment_preview(
                    crate::core::layer::AdjustmentType::HueSaturation {
                        hue: 0.0,
                        saturation: 0.0,
                        lightness: 0.0,
                    },
                ) {
                    self.shell.status_msg =
                        "Adjustment requires an unlocked raster layer".to_string();
                }
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyU) if pressed && !self.edit.input.ctrl_held => {
                self.edit.tools.select(ToolId::Shape);
                self.sync_cursor(event_loop);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::KeyJ) if pressed && !repeat && self.edit.input.ctrl_held => {
                if self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .duplicate_active_group()
                {
                    self.apply_canvas_event(crate::app::render::CanvasEvent::LayerStructureChanged);
                    if let Some(w) = &self.win.window {
                        w.request_redraw();
                    }
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
            PhysicalKey::Code(KeyCode::KeyJ) if pressed && !self.edit.input.ctrl_held => {
                self.edit.tools.select(ToolId::Repair);
                self.sync_cursor(event_loop);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            PhysicalKey::Code(KeyCode::BracketLeft) if pressed && !self.edit.input.shift_held => {
                if self.edit.show_refine_panel {
                    self.edit.tools.refine_brush_mut().size =
                        (self.edit.tools.refine_brush().size - 2.0).max(1.0);
                } else if self.edit.tools.active_id() == ToolId::Eraser {
                    self.edit.tools.eraser_mut().size =
                        (self.edit.tools.eraser().size - 2.0).max(1.0);
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

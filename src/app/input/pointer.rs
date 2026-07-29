//! Pointer input for the main window (mouse buttons + cursor movement),
//! extracted verbatim from the window_event match arms.

use crate::app::state::App;
use crate::extension::tool::ToolCtx;
use crate::tools::ToolId;
use winit::{
    event::{ElementState, MouseButton},
    event_loop::ActiveEventLoop,
};

impl App {
    /// Present a vector gradient created by GradientTool::on_release.
    ///
    /// The Tool owns the command and marks the old/new layer bounds dirty; the
    /// App owns the separate crisp-vector overlay and GPU compositor. Both must
    /// be invalidated together in the same release event, otherwise the model is
    /// committed but the old overlay remains visible until another input.
    fn finish_vector_gradient_release(&mut self) {
        self.invalidate_vector_display();
        self.apply_canvas_event(crate::app::render::CanvasEvent::LayerPixelsChanged);
    }

    /// The main window's MouseInput arm, verbatim: press/release routing to
    /// tools, guides, transform/text/shape sessions and window chrome.
    pub(in crate::app) fn on_main_mouse_input(
        &mut self,
        event_loop: &ActiveEventLoop,
        state: ElementState,
        button: MouseButton,
    ) {
        let pressed = state == ElementState::Pressed;
        if self.shell.ui.show_paint_color_dialog {
            if button == MouseButton::Left {
                if pressed && !self.edit.input.is_over_ui {
                    let ev = self.tool_event();
                    self.pick_color_at(ev.canvas_x, ev.canvas_y);
                    self.edit.input.eyedropping = true;
                    if let Some(w) = &self.win.window {
                        w.request_redraw();
                    }
                } else if !pressed {
                    self.edit.input.eyedropping = false;
                }
            }
            return;
        }
        if self.shell.ui.show_adjustment_dialog && self.shell.ui.adj_eyedropper.is_some() {
            if button == MouseButton::Left {
                if pressed && !self.edit.input.is_over_ui {
                    if let Some(kind) = self.shell.ui.adj_eyedropper {
                        let ev = self.tool_event();
                        self.apply_adjustment_eyedropper_at(kind, ev.canvas_x, ev.canvas_y);
                    }
                }
                return;
            }
        }
        if self.is_blocking_modal() {
            return;
        }
        // Warp is modal: route left-drag to the warp brush, keep Middle /
        // Space pan working, and swallow everything else (no tool dispatch).
        if self.edit.warp_state.is_some() {
            match button {
                MouseButton::Left if !self.edit.input.was_over_ui => {
                    if self.edit.input.space_held {
                        self.edit.input.space_dragging = pressed;
                        self.sync_cursor(event_loop);
                    } else if pressed {
                        self.warp_pointer_down();
                    } else {
                        self.warp_pointer_up();
                    }
                }
                MouseButton::Right if !self.edit.input.was_over_ui => {
                    if pressed && self.edit.input.alt_held {
                        // Alt+right drag resizes the brush instead of warping.
                        // Pin the ring at the press point (like Brush/Eraser) so
                        // it doesn't slide away with the cursor.
                        self.edit.input.warp_resizing = true;
                        self.edit.input.alt_drag_start_x = self.edit.input.mouse_x;
                        self.edit.input.alt_drag_start_y = self.edit.input.mouse_y;
                        self.edit.input.alt_drag_start_size = self.shell.ui.warp_params.size;
                    } else if !pressed {
                        // Warp the (hidden) OS cursor back to the start so the
                        // ring resumes exactly where it was pinned.
                        if self.edit.input.warp_resizing {
                            if let Some(win) = &self.win.window {
                                let _ = win.set_cursor_position(winit::dpi::PhysicalPosition::new(
                                    self.edit.input.alt_drag_start_x as f64,
                                    self.edit.input.alt_drag_start_y as f64,
                                ));
                            }
                        }
                        self.edit.input.warp_resizing = false;
                    }
                }
                MouseButton::Middle => {
                    self.edit.input.mid_dragging = pressed;
                    self.sync_cursor(event_loop);
                }
                _ => {}
            }
            return;
        }
        // Develop local-mask placement: an armed mask captures the left
        // drag; Middle/Space pan keeps working; the rest falls through.
        if self.shell.ui.show_develop_dialog && self.shell.ui.develop_local_arm.is_some() {
            match button {
                MouseButton::Left if !self.edit.input.was_over_ui => {
                    if self.edit.input.space_held {
                        self.edit.input.space_dragging = pressed;
                        self.sync_cursor(event_loop);
                    } else if pressed {
                        self.develop_local_pointer_down();
                    } else {
                        self.develop_local_pointer_up();
                    }
                }
                MouseButton::Middle => {
                    self.edit.input.mid_dragging = pressed;
                    self.sync_cursor(event_loop);
                }
                _ => {}
            }
            return;
        }
        // Borderless-window edge resize: a left-press in the outer resize
        // border hands off to the system resize loop (no OS title bar to do
        // it for us). Only fires when restored (not maximized) and near an edge.
        if pressed && button == MouseButton::Left {
            if let Some(dir) = self.resize_direction() {
                if let Some(win) = &self.win.window {
                    let _ = win.drag_resize_window(dir);
                }
                return;
            }
        }
        match button {
            MouseButton::Middle => {
                self.edit.input.mid_dragging = pressed;
                self.sync_cursor(event_loop);
            }
            MouseButton::Right
                if pressed
                    && self.edit.transform_state.is_some()
                    && !self.edit.input.was_over_ui =>
            {
                self.edit.transform_ctx_menu_pos =
                    Some((self.edit.input.mouse_x, self.edit.input.mouse_y));
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            MouseButton::Right
                if pressed
                    && !self.edit.input.alt_held
                    && !self.edit.input.was_over_ui
                    && self.edit.transform_state.is_none()
                    && matches!(
                        self.edit.tools.active_id(),
                        ToolId::Brush
                            | ToolId::Eraser
                            | ToolId::Clone
                            | ToolId::Repair
                            | ToolId::Smudge
                            | ToolId::Dodge
                            | ToolId::Burn
                    ) =>
            {
                self.edit.brush_popup_pos =
                    Some((self.edit.input.mouse_x, self.edit.input.mouse_y));
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            MouseButton::Right
                if pressed
                    && !self.edit.input.alt_held
                    && !self.edit.input.was_over_ui
                    && self.edit.transform_state.is_none()
                    && self.is_selection_tool_active() =>
            {
                self.edit.selection_ctx_menu_pos =
                    Some((self.edit.input.mouse_x, self.edit.input.mouse_y));
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            MouseButton::Right if self.edit.input.alt_held => {
                if pressed {
                    self.edit.input.alt_right_dragging = true;
                    self.edit.input.alt_drag_start_x = self.edit.input.mouse_x;
                    self.edit.input.alt_drag_start_y = self.edit.input.mouse_y;
                    self.edit.input.alt_drag_start_size = if self.edit.show_refine_panel {
                        self.edit.tools.refine_brush().size
                    } else if self.edit.tools.active_id() == crate::tools::ToolId::SmartSelect {
                        self.edit.tools.wand().brush_size
                    } else if matches!(
                        self.edit.tools.active_id(),
                        crate::tools::ToolId::Clone | crate::tools::ToolId::Repair
                    ) {
                        self.edit.tools.clone_like().size
                    } else if self.edit.tools.active_id() == crate::tools::ToolId::Smudge {
                        self.edit.tools.smudge().size
                    } else if matches!(
                        self.edit.tools.active_id(),
                        crate::tools::ToolId::Dodge | crate::tools::ToolId::Burn
                    ) {
                        self.edit.tools.dodge_burn().size
                    } else if self.edit.tools.active_id() == crate::tools::ToolId::Eraser {
                        self.edit.tools.eraser().size
                    } else {
                        self.edit.tools.brush().settings.size
                    };
                    if let Some(win) = &self.win.window {
                        win.set_cursor_visible(false);
                    }
                } else {
                    self.edit.input.alt_right_dragging = false;
                    if let Some(win) = &self.win.window {
                        let _ = win.set_cursor_position(winit::dpi::PhysicalPosition::new(
                            self.edit.input.alt_drag_start_x as f64,
                            self.edit.input.alt_drag_start_y as f64,
                        ));
                    }
                    self.sync_cursor(event_loop);
                }
            }
            MouseButton::Left => {
                if self.edit.input.space_held {
                    self.edit.input.space_dragging = pressed;
                    self.sync_cursor(event_loop);
                } else {
                    if pressed {
                        if !self.edit.input.was_over_ui {
                            let active_tool = self.edit.tools.active_id();
                            if !self.alpha_view_allows_tool(active_tool) {
                                self.deny_alpha_view_tool(active_tool);
                                return;
                            }
                            if !self.cmyk_allows_tool(active_tool) {
                                self.deny_cmyk_tool(active_tool);
                                return;
                            }

                            // Grab an existing guide with the Move tool (before any
                            // tool press): drag to reposition, drop off-canvas to delete.
                            if self.edit.tools.active_id() == ToolId::Move
                                && self.edit.transform_state.is_none()
                            {
                                if let Some(gi) = self.guide_at_screen() {
                                    let g =
                                        self.docs.documents[self.docs.active_doc_idx].guides[gi];
                                    let ev = self.tool_event();
                                    let pos = match g.orientation {
                                        crate::core::document::GuideOrientation::Horizontal => {
                                            ev.canvas_y
                                        }
                                        crate::core::document::GuideOrientation::Vertical => {
                                            ev.canvas_x
                                        }
                                    };
                                    self.edit.guide_op = Some(crate::app::state::GuideOp::Move {
                                        idx: gi,
                                        orientation: g.orientation,
                                        pos,
                                    });
                                    if let Some(w) = &self.win.window {
                                        w.request_redraw();
                                    }
                                    return;
                                }
                            }

                            let is_double_click = self
                                .edit
                                .input
                                .last_left_release_time
                                .map(|t| t.elapsed().as_millis() < 400)
                                .unwrap_or(false);
                            if is_double_click
                                && self.edit.tools.active_id() == ToolId::Move
                                && self.edit.transform_state.is_none()
                            {
                                let ev = self.tool_event();
                                if let Some(idx) = self.text_layer_at(ev.canvas_x, ev.canvas_y) {
                                    self.begin_edit_text_layer(idx);
                                    self.edit.input.last_left_release_time = None;
                                    if let Some(win) = &self.win.window {
                                        win.request_redraw();
                                    }
                                    return;
                                }
                            }

                            if is_double_click
                                && self.edit.tools.active_id() == ToolId::PolygonLasso
                                && self.edit.transform_state.is_none()
                            {
                                {
                                    let mut ctx = ToolCtx::new(
                                        &mut self.docs.documents[self.docs.active_doc_idx],
                                        self.edit.fg_color,
                                        self.edit.bg_color,
                                        self.edit.view.zoom,
                                        self.edit.view.offset_x,
                                        self.edit.view.offset_y,
                                    );
                                    self.edit.tools.active_on_confirm(&mut ctx);
                                }
                                self.docs.documents[self.docs.active_doc_idx]
                                    .canvas
                                    .selection
                                    .refresh_bbox();
                                self.upload_selection_mask();
                                self.push_selection_uniforms();
                                self.edit.input.last_left_release_time = None;
                                self.edit.input.painting = false;
                                if let Some(win) = &self.win.window {
                                    win.request_redraw();
                                }
                                return;
                            }

                            if self.edit.tools.active_id() == ToolId::Text
                                && self.edit.transform_state.is_none()
                            {
                                // While a session is open the overlay owns all
                                // pointer interaction; outside clicks are refused
                                // by the action gate (bell) — not here, to avoid
                                // double-chiming the same click.
                                if self.edit.text_edit.is_none() {
                                    let ev = self.tool_event();
                                    if let Some(idx) = self.text_layer_at(ev.canvas_x, ev.canvas_y)
                                    {
                                        self.begin_edit_text_layer(idx);
                                    } else {
                                        self.text_tool_click(ev.canvas_x, ev.canvas_y);
                                    }
                                }
                                self.edit.input.last_left_release_time = None;
                                if let Some(win) = &self.win.window {
                                    win.request_redraw();
                                }
                                return;
                            }

                            // Shape tool: grabbing a handle of the active
                            // Shape layer starts a resize/edit drag;
                            // otherwise the press falls through to start a
                            // rubber-band for a brand-new shape.
                            if self.edit.tools.active_id() == ToolId::Shape
                                && self.edit.transform_state.is_none()
                            {
                                let ev = self.tool_event();
                                if let Some(handle) = self.shape_handle_at(ev.canvas_x, ev.canvas_y)
                                {
                                    self.shape_begin_handle_drag(handle);
                                    self.edit.input.painting = true;
                                    if let Some(w) = &self.win.window {
                                        w.request_redraw();
                                    }
                                    return;
                                }
                            }

                            // Gradient transform handles are interactive only
                            // while the Gradient Tool is active.
                            if self.edit.tools.active_id() == ToolId::Gradient
                                && self.edit.transform_state.is_none()
                            {
                                let (msx, msy) = (self.edit.input.mouse_x, self.edit.input.mouse_y);
                                if let Some(handle) = self.path_gradient_handle_at_screen(msx, msy)
                                {
                                    let ev = self.tool_event();
                                    self.path_gradient_begin(handle, ev.canvas_x, ev.canvas_y);
                                    self.edit.input.painting = true;
                                    if let Some(w) = &self.win.window {
                                        w.request_redraw();
                                    }
                                    return;
                                }
                            }

                            // Move tool: grabbing a transform handle (or the
                            // rotate ring) of the active Path layer starts an
                            // on-canvas scale/rotate that keeps the object
                            // editable; otherwise the press falls through to the
                            // normal Move (select / drag / marquee).
                            if self.edit.tools.active_id() == ToolId::Move
                                && self.edit.transform_state.is_none()
                            {
                                let (msx, msy) = (self.edit.input.mouse_x, self.edit.input.mouse_y);
                                if let Some(hit) = self.path_box_hit_at_screen(msx, msy) {
                                    let ev = self.tool_event();
                                    self.path_transform_begin(hit, ev.canvas_x, ev.canvas_y);
                                    self.edit.input.painting = true;
                                    if let Some(w) = &self.win.window {
                                        w.request_redraw();
                                    }
                                    return;
                                }
                            }

                            // Node tool: double-click an anchor cycles its kind
                            // (Cusp→Smooth→Symmetric); a single press on a handle
                            // reshapes the curve, on an anchor drags it, on a
                            // segment inserts an anchor there and drags it, and on
                            // empty space deselects. All routed to node_ops.
                            if self.edit.tools.active_id() == ToolId::Node
                                && self.edit.transform_state.is_none()
                            {
                                let (msx, msy) = (self.edit.input.mouse_x, self.edit.input.mouse_y);
                                let is_double_click = self
                                    .edit
                                    .input
                                    .last_left_release_time
                                    .map(|t| t.elapsed().as_millis() < 400)
                                    .unwrap_or(false);
                                if is_double_click {
                                    if let Some(crate::app::node_ops::NodeHit::Node(ci, ni)) =
                                        self.node_hit_at_screen(msx, msy)
                                    {
                                        if self.node_toggle_kind(ci, ni) {
                                            self.edit.input.last_left_release_time = None;
                                            self.edit.input.painting = true;
                                            return;
                                        }
                                    }
                                }
                                let ev = self.tool_event();
                                if let Some(hit) = self.node_hit_at_screen(msx, msy) {
                                    // Shift+click an anchor toggles it in the
                                    // multi-selection instead of starting a drag.
                                    if self.edit.input.shift_held {
                                        if let crate::app::node_ops::NodeHit::Node(ci, ni) = hit {
                                            self.node_shift_toggle(ci, ni);
                                            self.edit.input.painting = true;
                                            return;
                                        }
                                    }
                                    if self.node_press(hit, ev.canvas_x, ev.canvas_y) {
                                        self.edit.input.painting = true;
                                        return;
                                    }
                                } else if self.node_click_select_path(ev.canvas_x, ev.canvas_y) {
                                    // Clicked another Path's body — make it the edit
                                    // target (Shape-tool object switching), then grab
                                    // a node of it if the click also landed on one.
                                    if let Some(hit) = self.node_hit_at_screen(msx, msy) {
                                        self.node_press(hit, ev.canvas_x, ev.canvas_y);
                                    }
                                    self.edit.input.painting = true;
                                    return;
                                } else {
                                    // Empty canvas: begin a rubber-band selection.
                                    // Release decides — select the enclosed anchors,
                                    // or clear if it was really just a click.
                                    self.node_marquee_start(msx, msy);
                                }
                                // Swallow the press so it never falls through to a
                                // pixel tool on the (vector) Path layer.
                                self.edit.input.painting = true;
                                return;
                            }

                            if matches!(self.edit.tools.active_id(), ToolId::Clone | ToolId::Repair)
                                && self.edit.input.alt_held
                                && self.edit.transform_state.is_none()
                            {
                                let event = self.tool_event();
                                {
                                    let mut ctx = ToolCtx::new(
                                        &mut self.docs.documents[self.docs.active_doc_idx],
                                        self.edit.fg_color,
                                        self.edit.bg_color,
                                        self.edit.view.zoom,
                                        self.edit.view.offset_x,
                                        self.edit.view.offset_y,
                                    );
                                    let _ = self.edit.tools.on_press(event, &mut ctx);
                                }
                                if let Some(w) = &self.win.window {
                                    w.request_redraw();
                                }
                                return;
                            }

                            let alt_paint_pick = matches!(
                                self.edit.tools.active_id(),
                                ToolId::Brush | ToolId::Pencil
                            ) && self.edit.input.alt_held;
                            if (self.edit.tools.active_id() == ToolId::Eyedropper || alt_paint_pick)
                                && self.edit.transform_state.is_none()
                            {
                                let ev = self.tool_event();
                                self.pick_color_at(ev.canvas_x, ev.canvas_y);
                                self.edit.input.eyedropping = true;
                                if let Some(w) = &self.win.window {
                                    w.request_redraw();
                                }
                                return;
                            }

                            // Zoom tool: press starts scrubby zoom. If the
                            // user releases without dragging, it falls back
                            // to the classic click zoom.
                            if self.edit.tools.active_id() == ToolId::Zoom
                                && self.edit.transform_state.is_none()
                            {
                                self.edit.input.zoom_dragging = true;
                                self.edit.input.zoom_drag_moved = false;
                                self.edit.input.zoom_drag_start_x = self.edit.input.mouse_x;
                                self.edit.input.zoom_drag_start_y = self.edit.input.mouse_y;
                                self.edit.input.zoom_drag_anchor_x = self.edit.input.mouse_x;
                                self.edit.input.zoom_drag_anchor_y = self.edit.input.mouse_y;
                                self.edit.input.zoom_drag_start_zoom = self.edit.view.zoom;
                                return;
                            }

                            self.edit.input.painting = true;
                            if self.edit.transform_state.is_some() {
                                let ev = self.tool_event();
                                let sx = self.edit.input.mouse_x;
                                let sy = self.edit.input.mouse_y;
                                self.transform_on_press(ev.canvas_x, ev.canvas_y, sx, sy);
                                self.win.interactive_recompose_last = None;
                                self.win.interactive_recompose_cost = std::time::Duration::ZERO;
                                self.request_interactive_recompose();
                                if let Some(w) = &self.win.window {
                                    w.request_redraw();
                                }
                            } else {
                                let event = self.tool_event();
                                let active_tool = self.edit.tools.active_id();
                                let tool_resp = {
                                    let mut ctx = ToolCtx::new(
                                        &mut self.docs.documents[self.docs.active_doc_idx],
                                        self.edit.fg_color,
                                        self.edit.bg_color,
                                        self.edit.view.zoom,
                                        self.edit.view.offset_x,
                                        self.edit.view.offset_y,
                                    );
                                    self.edit.tools.on_press(event, &mut ctx)
                                };
                                if let Some(msg) = tool_resp.status {
                                    self.shell.status_msg = msg.to_string();
                                }
                                let mut request_redraw = tool_resp.needs_redraw;
                                if matches!(
                                    active_tool,
                                    ToolId::Move
                                        | ToolId::Gradient
                                        | ToolId::Crop
                                        | ToolId::PerspectiveCrop
                                ) {
                                    // Plain mouse-down on these tools should be cheap.
                                    // Move recomposes on first drag; Gradient only draws
                                    // a UI direction guide until mouse-up bakes pixels.
                                } else {
                                    self.flush_canvas();
                                    self.docs.documents[self.docs.active_doc_idx]
                                        .canvas
                                        .selection
                                        .refresh_bbox();
                                    self.upload_selection_mask();
                                    self.push_selection_uniforms();
                                    request_redraw = true;
                                }
                                if request_redraw {
                                    if let Some(w) = &self.win.window {
                                        w.request_redraw();
                                    }
                                }
                            }
                        }
                    } else {
                        self.edit.input.eyedropping = false;
                        if self.edit.input.zoom_dragging {
                            let clicked = !self.edit.input.zoom_drag_moved;
                            self.edit.input.zoom_dragging = false;
                            self.edit.input.zoom_drag_moved = false;
                            self.edit.input.last_left_release_time =
                                Some(std::time::Instant::now());
                            if clicked {
                                self.zoom_at_cursor(!self.edit.input.alt_held);
                            } else {
                                self.sync_cursor(event_loop);
                                if let Some(w) = &self.win.window {
                                    w.request_redraw();
                                }
                            }
                            return;
                        }
                        // Finish moving an existing guide: drop off-canvas → delete it.
                        // NB: only consume guide_op for a Move. A Create (ruler drag) is
                        // committed by the egui side via `ruler_guide_commit`, which runs
                        // a frame later — taking it here would lose the new guide.
                        if let Some(crate::app::state::GuideOp::Move {
                            idx,
                            orientation,
                            pos,
                        }) = self.edit.guide_op
                        {
                            self.edit.guide_op = None;
                            let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
                            let dim = match orientation {
                                crate::core::document::GuideOrientation::Horizontal => {
                                    canvas.height as f32
                                }
                                crate::core::document::GuideOrientation::Vertical => {
                                    canvas.width as f32
                                }
                            };
                            if (pos < 0.0 || pos > dim)
                                && idx < self.docs.documents[self.docs.active_doc_idx].guides.len()
                            {
                                self.docs.documents[self.docs.active_doc_idx]
                                    .guides
                                    .remove(idx);
                            }
                            self.edit.input.last_left_release_time =
                                Some(std::time::Instant::now());
                            if let Some(w) = &self.win.window {
                                w.request_redraw();
                            }
                            return;
                        }
                        let releasing_move = self.edit.input.painting
                            && self.edit.transform_state.is_none()
                            && self.edit.path_transform.is_none()
                            && self.edit.path_gradient_drag.is_none()
                            && self.edit.tools.active_id() == ToolId::Move;
                        if self.edit.path_gradient_drag.is_some() {
                            // Commit from a settled input state. This makes the
                            // final full composite take the normal (non-drag)
                            // path and prevents any interactive cache policy
                            // from surviving the release frame.
                            self.edit.input.painting = false;
                            self.path_gradient_finish();
                            self.edit.input.last_left_release_time =
                                Some(std::time::Instant::now());
                            if let Some(w) = &self.win.window {
                                w.request_redraw();
                            }
                            return;
                        }
                        if self.edit.path_transform.is_some() {
                            // Finish an on-canvas Path scale/rotate (records one
                            // ChangeVectorTransform). Bypasses the Move tool's
                            // own release path — the gesture never touched it.
                            self.path_transform_finish();
                            self.edit.input.last_left_release_time =
                                Some(std::time::Instant::now());
                            self.edit.input.painting = false;
                            if let Some(w) = &self.win.window {
                                w.request_redraw();
                            }
                            return;
                        }
                        if self.edit.tools.active_id() == ToolId::Node
                            && self.edit.transform_state.is_none()
                        {
                            // Finish a Node tool drag (records one
                            // ReplacePathGeometry), a rubber-band selection, or just
                            // release a no-hit press.
                            if self.edit.node_drag.is_some() {
                                self.node_drag_finish();
                            } else if self.node_marquee_active() {
                                self.node_marquee_finish();
                            }
                            self.edit.input.last_left_release_time =
                                Some(std::time::Instant::now());
                            self.edit.input.painting = false;
                            if let Some(w) = &self.win.window {
                                w.request_redraw();
                            }
                            return;
                        }
                        if self.edit.input.painting {
                            if self.edit.transform_state.is_some() {
                                self.transform_on_release();
                                if let Some(w) = &self.win.window {
                                    w.request_redraw();
                                }
                            } else if self.edit.tools.active_id() == ToolId::Shape {
                                if self.shape_drag_active() {
                                    // Finish resizing / editing a handle.
                                    self.shape_drag_finish();
                                } else {
                                    // The rubber-band drag becomes a new,
                                    // editable Shape layer (handled at App
                                    // level so layer structure is undoable).
                                    let ev = self.tool_event();
                                    let span = self.edit.tools.shape_mut().finish_span(
                                        ev.canvas_x,
                                        ev.canvas_y,
                                        ev.shift,
                                        ev.alt,
                                    );
                                    if let Some((x0, y0, x1, y1)) = span {
                                        self.begin_new_shape(x0, y0, x1, y1);
                                    }
                                }
                                self.edit.input.painting = false;
                                if let Some(w) = &self.win.window {
                                    w.request_redraw();
                                }
                            } else if self.edit.tools.active_id() == ToolId::VectorBrush {
                                // A freehand drag becomes one editable Path stroke
                                // (handled at App level so the new layer is undoable).
                                let event = self.tool_event();
                                {
                                    let mut ctx = ToolCtx::new(
                                        &mut self.docs.documents[self.docs.active_doc_idx],
                                        self.edit.fg_color,
                                        self.edit.bg_color,
                                        self.edit.view.zoom,
                                        self.edit.view.offset_x,
                                        self.edit.view.offset_y,
                                    );
                                    // Let the tool catch its stabilizer up to the
                                    // release point before we read the stroke.
                                    let _ = self.edit.tools.on_release(event, &mut ctx);
                                }
                                self.commit_vector_brush_stroke();
                                self.edit.input.painting = false;
                                if let Some(w) = &self.win.window {
                                    w.request_redraw();
                                }
                            } else {
                                if !self.edit.pending_stroke_inputs.is_empty() {
                                    let events: Vec<_> =
                                        self.edit.pending_stroke_inputs.drain(..).collect();
                                    for event in events {
                                        let mut ctx = ToolCtx::new(
                                            &mut self.docs.documents[self.docs.active_doc_idx],
                                            self.edit.fg_color,
                                            self.edit.bg_color,
                                            self.edit.view.zoom,
                                            self.edit.view.offset_x,
                                            self.edit.view.offset_y,
                                        );
                                        let _ = self.edit.tools.on_drag(event, &mut ctx);
                                    }
                                    self.flush_canvas();
                                }

                                let event = self.tool_event();
                                let active_tool = self.edit.tools.active_id();
                                let releasing_vector_gradient = active_tool == ToolId::Gradient
                                    && self.active_gradient_mode() == 2;
                                let tool_resp = {
                                    let mut ctx = ToolCtx::new(
                                        &mut self.docs.documents[self.docs.active_doc_idx],
                                        self.edit.fg_color,
                                        self.edit.bg_color,
                                        self.edit.view.zoom,
                                        self.edit.view.offset_x,
                                        self.edit.view.offset_y,
                                    );
                                    self.edit.tools.on_release(event, &mut ctx)
                                };
                                if let Some(msg) = tool_resp.status {
                                    self.shell.status_msg = msg.to_string();
                                }
                                let move_layer_release = matches!(
                                    active_tool,
                                    ToolId::Move | ToolId::Crop | ToolId::PerspectiveCrop
                                ) && !tool_resp.needs_composite;
                                if releasing_vector_gradient {
                                    self.finish_vector_gradient_release();
                                } else if !move_layer_release {
                                    self.flush_canvas();
                                }
                                if active_tool == ToolId::Crop {
                                    self.update_crop_preview();
                                }

                                if self.edit.tools.active_id() == crate::tools::ToolId::Repair {
                                    if let Some((mask, _lw, _lh)) =
                                        self.edit.tools.healing_mut().0.take_pending_ca()
                                    {
                                        let ok = self.docs.documents[self.docs.active_doc_idx]
                                            .canvas
                                            .heal_skin(mask);
                                        if ok {
                                            self.apply_canvas_event(
                                                crate::app::render::CanvasEvent::LayerPixelsChanged,
                                            );
                                            self.shell.status_msg = "Smart Heal".to_string();
                                        } else {
                                            self.shell.status_msg =
                                                "Smart Heal cần một layer raster mở khoá"
                                                    .to_string();
                                        }
                                    }
                                }

                                if self.edit.tools.active_id() == crate::tools::ToolId::Patch {
                                    use crate::tools::patch::PatchPending;
                                    if let Some(pending) =
                                        self.edit.tools.patch_mut().take_pending()
                                    {
                                        let canvas = &mut self.docs.documents
                                            [self.docs.active_doc_idx]
                                            .canvas;
                                        let ok = match pending {
                                            PatchPending::Clone { dx, dy } => {
                                                canvas.patch_clone(dx, dy)
                                            }
                                            PatchPending::Fill => canvas.smart_fill_fill(false),
                                        };
                                        if ok {
                                            self.apply_canvas_event(
                                                crate::app::render::CanvasEvent::LayerPixelsChanged,
                                            );
                                            self.shell.status_msg = "Patch".to_string();
                                        } else {
                                            self.shell.status_msg =
                                                "Patch cần một layer raster mở khoá".to_string();
                                        }
                                    }
                                }

                                if matches!(
                                    self.edit.tools.active_id(),
                                    crate::tools::ToolId::Brush | crate::tools::ToolId::Eraser
                                ) {
                                    self.win.pending_gpu_sync = self.docs.documents
                                        [self.docs.active_doc_idx]
                                        .canvas
                                        .stroke_dirty
                                        .clone();
                                    self.win.pending_gpu_sync_layer_id =
                                        self.docs.documents[self.docs.active_doc_idx]
                                            .canvas
                                            .active_layer()
                                            .id as usize;
                                    self.sync_brush_gpu_to_cpu();
                                }

                                if !move_layer_release {
                                    self.docs.documents[self.docs.active_doc_idx]
                                        .canvas
                                        .selection
                                        .refresh_bbox();
                                    self.upload_selection_mask();
                                    self.push_selection_uniforms();
                                }

                                if self.edit.show_refine_panel
                                    && self.edit.tools.active_id()
                                        == crate::tools::ToolId::RefineBrush
                                {
                                    self.edit.refine_snapshot = self.docs.documents
                                        [self.docs.active_doc_idx]
                                        .canvas
                                        .selection
                                        .mask
                                        .clone();
                                    self.edit.refine_feather = 0.0;
                                    self.edit.refine_smooth = 0;
                                    self.edit.refine_smart_radius = 0.0;
                                    self.edit.refine_shift_edge = 0.0;
                                    self.edit.refine_contrast = 0.0;
                                }

                                if !move_layer_release {
                                    self.docs.documents[self.docs.active_doc_idx]
                                        .canvas
                                        .end_stroke();
                                }
                                if let Some(w) = &self.win.window {
                                    w.request_redraw();
                                }
                            }
                        }
                        self.edit.input.last_left_release_time = Some(std::time::Instant::now());
                        self.edit.input.painting = false;
                        if releasing_move {
                            self.win.interactive_recompose_pending = false;
                            // The clipping-mask / PowerClip re-fit is skipped every
                            // frame during a Move drag (a full-content mask re-bake +
                            // GPU re-upload per frame made large clipped images lag).
                            // A plain Move release takes the `move_layer_release` fast
                            // path and skips flush_canvas, so nothing would re-pin the
                            // clip. Re-pin ONCE now that the move settled, so clipped
                            // content snaps back onto its frame instead of being left
                            // showing the dragged (stale) mask until an unrelated
                            // recomposite. Fingerprint-gated inside refresh_clip_masks,
                            // so a document with no clip that actually moved does no
                            // recomposite work.
                            if self.docs.documents[self.docs.active_doc_idx]
                                .canvas
                                .has_clip_content()
                            {
                                // Defer the CPU flatten: a normal Move release skips
                                // flush_canvas entirely, but we must flush here to
                                // re-bake the clip. Marking pixels stale keeps the
                                // flush to just the mask re-bake + GPU recomposite
                                // (the CPU flatten rebuilds lazily when actually
                                // needed), so a large clipped image doesn't stall the
                                // next drag by a beat.
                                self.docs.documents[self.docs.active_doc_idx]
                                    .canvas
                                    .pixels_stale = true;
                                self.flush_canvas();
                                if let Some(w) = &self.win.window {
                                    w.request_redraw();
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    /// The main window's CursorMoved arm, verbatim: hover states, drags,
    /// guides, painting and the resize-affordance cursor.
    pub(in crate::app) fn on_main_cursor_moved(
        &mut self,
        event_loop: &ActiveEventLoop,
        position: winit::dpi::PhysicalPosition<f64>,
    ) {
        let dx = (position.x - self.edit.input.last_mouse_x) as f32;
        let dy = (position.y - self.edit.input.last_mouse_y) as f32;
        self.edit.input.mouse_x = position.x as f32;
        self.edit.input.mouse_y = position.y as f32;

        // Show the resize cursor while idle-hovering the borderless window's
        // edge border, so the resize affordance is discoverable.
        if !self.edit.input.painting
            && !self.edit.input.mid_dragging
            && !self.edit.input.space_dragging
            && self.edit.guide_op.is_none()
            && self.edit.transform_state.is_none()
        {
            if let Some(dir) = self.resize_direction() {
                if let Some(w) = &self.win.window {
                    w.set_cursor(Self::resize_cursor(dir));
                }
                self.edit.input.last_mouse_x = position.x;
                self.edit.input.last_mouse_y = position.y;
                return;
            }
        }

        // Live-drag an existing guide (Move tool grabbed it on press).
        if let Some(crate::app::state::GuideOp::Move {
            idx, orientation, ..
        }) = self.edit.guide_op
        {
            let ev = self.tool_event();
            let raw = match orientation {
                crate::core::document::GuideOrientation::Horizontal => ev.canvas_y,
                crate::core::document::GuideOrientation::Vertical => ev.canvas_x,
            };
            let snapped = self.snap_guide_pos(orientation, raw, Some(idx));
            if let Some(g) = self.docs.documents[self.docs.active_doc_idx]
                .guides
                .get_mut(idx)
            {
                g.pos = snapped;
            }
            self.edit.guide_op = Some(crate::app::state::GuideOp::Move {
                idx,
                orientation,
                pos: snapped,
            });
            self.edit.input.last_mouse_x = position.x;
            self.edit.input.last_mouse_y = position.y;
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return;
        }

        // Warp: Alt+drag resizes the brush; a normal drag warps; hovering /
        // panning falls through so the OS cursor + pan still work.
        if self.edit.warp_state.is_some() {
            if self.edit.input.warp_resizing {
                let delta = self.edit.input.mouse_x - self.edit.input.alt_drag_start_x;
                let new_size = (self.edit.input.alt_drag_start_size + delta / self.edit.view.zoom)
                    .clamp(10.0, 1000.0);
                self.shell.ui.warp_params.size = new_size;
                self.edit.input.last_mouse_x = position.x;
                self.edit.input.last_mouse_y = position.y;
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
                return;
            }
            let dragging = self
                .edit
                .warp_state
                .as_ref()
                .map(|s| s.dragging)
                .unwrap_or(false);
            if dragging && !self.edit.input.space_dragging {
                self.warp_pointer_drag();
                self.edit.input.last_mouse_x = position.x;
                self.edit.input.last_mouse_y = position.y;
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
                return;
            }
        }

        // Live-track a Develop local-mask placement drag.
        if self.dev.develop_local_drag.is_some() && !self.edit.input.space_dragging {
            self.develop_local_pointer_drag();
            self.edit.input.last_mouse_x = position.x;
            self.edit.input.last_mouse_y = position.y;
            return;
        }

        if self.edit.input.eyedropping
            && (self.shell.ui.show_paint_color_dialog || !self.is_blocking_modal())
        {
            let ev = self.tool_event();
            self.pick_color_at(ev.canvas_x, ev.canvas_y);
            self.edit.input.last_mouse_x = position.x;
            self.edit.input.last_mouse_y = position.y;
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return;
        }

        if self.edit.input.zoom_dragging && !self.is_blocking_modal() {
            let drag_x = self.edit.input.mouse_x - self.edit.input.zoom_drag_start_x;
            let drag_y = self.edit.input.mouse_y - self.edit.input.zoom_drag_start_y;
            let mut delta = if drag_x.abs() >= drag_y.abs() {
                drag_x
            } else {
                drag_y
            };
            if self.edit.input.alt_held {
                delta = -delta;
            }

            if delta.abs() >= 4.0 {
                self.edit.input.zoom_drag_moved = true;
                let factor = (delta / 180.0).exp();
                let new_zoom = self.edit.input.zoom_drag_start_zoom * factor;
                self.set_zoom_at_screen(
                    self.edit.input.zoom_drag_anchor_x,
                    self.edit.input.zoom_drag_anchor_y,
                    new_zoom,
                );
                self.sync_cursor(event_loop);
            }

            self.edit.input.last_mouse_x = position.x;
            self.edit.input.last_mouse_y = position.y;
            self.push_cursor_uniforms();
            return;
        }

        if self.is_blocking_modal() {
        } else if self.edit.input.alt_right_dragging {
            let delta_x = self.edit.input.mouse_x - self.edit.input.alt_drag_start_x;
            let new_size = (self.edit.input.alt_drag_start_size + delta_x / self.edit.view.zoom)
                .clamp(1.0, 5000.0);
            if self.edit.show_refine_panel {
                self.edit.tools.refine_brush_mut().size = new_size;
            } else if self.edit.tools.active_id() == crate::tools::ToolId::SmartSelect {
                self.edit.tools.wand_mut().brush_size = new_size;
            } else if matches!(
                self.edit.tools.active_id(),
                crate::tools::ToolId::Clone | crate::tools::ToolId::Repair
            ) {
                self.edit.tools.clone_like_mut().size = new_size;
            } else if self.edit.tools.active_id() == crate::tools::ToolId::Smudge {
                self.edit.tools.smudge_mut().size = new_size;
            } else if matches!(
                self.edit.tools.active_id(),
                crate::tools::ToolId::Dodge | crate::tools::ToolId::Burn
            ) {
                self.edit.tools.dodge_burn_mut().size = new_size;
            } else if self.edit.tools.active_id() == crate::tools::ToolId::Eraser {
                self.edit.tools.eraser_mut().size = new_size;
            } else {
                self.edit.tools.brush_mut().settings.size = new_size;
            }
            self.win.last_cursor_radius = 0;
            self.sync_cursor(event_loop);
        } else if self.edit.input.mid_dragging || self.edit.input.space_dragging {
            self.edit.view.offset_x += dx;
            self.edit.view.offset_y += dy;
            self.constrain_pan();
            self.push_canvas_uniforms();
            self.win.pending_view_change = true;
        } else if self.edit.input.painting
            && !self.edit.input.was_over_ui
            && self.edit.path_gradient_drag.is_some()
        {
            let ev = self.tool_event();
            self.path_gradient_update(ev.canvas_x, ev.canvas_y);
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        } else if self.edit.input.painting
            && !self.edit.input.was_over_ui
            && self.edit.path_transform.is_some()
        {
            // Live on-canvas Path scale/rotate.
            let ev = self.tool_event();
            self.path_transform_update(
                ev.canvas_x,
                ev.canvas_y,
                self.edit.input.shift_held,
                self.edit.input.alt_held,
            );
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        } else if self.edit.input.painting
            && !self.edit.input.was_over_ui
            && self.edit.node_drag.is_some()
        {
            // Live Node tool drag (move / place an anchor).
            let ev = self.tool_event();
            self.node_drag_update(ev.canvas_x, ev.canvas_y);
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        } else if self.edit.input.painting
            && !self.edit.input.was_over_ui
            && self.node_marquee_active()
        {
            // Extend the Node-tool rubber-band (screen space).
            self.node_marquee_update(self.edit.input.mouse_x, self.edit.input.mouse_y);
        } else if self.edit.input.painting && !self.edit.input.was_over_ui {
            if self.edit.transform_state.is_some() {
                let ev = self.tool_event();
                self.transform_on_drag(
                    ev.canvas_x,
                    ev.canvas_y,
                    self.edit.input.shift_held,
                    self.edit.input.alt_held,
                );
            } else {
                match self.edit.tools.active_id() {
                    ToolId::Hand => {
                        self.edit.view.offset_x += dx;
                        self.edit.view.offset_y += dy;
                        self.constrain_pan();
                        self.push_canvas_uniforms();
                        self.win.pending_view_change = true;
                    }
                    ToolId::Gradient => {
                        let event = self.tool_event();
                        let tool_resp = {
                            let mut ctx = ToolCtx::new(
                                &mut self.docs.documents[self.docs.active_doc_idx],
                                self.edit.fg_color,
                                self.edit.bg_color,
                                self.edit.view.zoom,
                                self.edit.view.offset_x,
                                self.edit.view.offset_y,
                            );
                            self.edit.tools.on_drag(event, &mut ctx)
                        };
                        if let Some(msg) = tool_resp.status {
                            self.shell.status_msg = msg.to_string();
                        }
                        if let Some(w) = &self.win.window {
                            w.request_redraw();
                        }
                    }
                    ToolId::Crop => {
                        let event = self.tool_event();
                        let tool_resp = {
                            let mut ctx = ToolCtx::new(
                                &mut self.docs.documents[self.docs.active_doc_idx],
                                self.edit.fg_color,
                                self.edit.bg_color,
                                self.edit.view.zoom,
                                self.edit.view.offset_x,
                                self.edit.view.offset_y,
                            );
                            self.edit.tools.on_drag(event, &mut ctx)
                        };
                        if let Some(msg) = tool_resp.status {
                            self.shell.status_msg = msg.to_string();
                        }
                        self.update_crop_preview();
                        if let Some(w) = &self.win.window {
                            w.request_redraw();
                        }
                    }
                    ToolId::PerspectiveCrop => {
                        let event = self.tool_event();
                        let tool_resp = {
                            let mut ctx = ToolCtx::new(
                                &mut self.docs.documents[self.docs.active_doc_idx],
                                self.edit.fg_color,
                                self.edit.bg_color,
                                self.edit.view.zoom,
                                self.edit.view.offset_x,
                                self.edit.view.offset_y,
                            );
                            self.edit.tools.on_drag(event, &mut ctx)
                        };
                        if let Some(msg) = tool_resp.status {
                            self.shell.status_msg = msg.to_string();
                        }
                        if let Some(w) = &self.win.window {
                            w.request_redraw();
                        }
                    }
                    ToolId::Shape => {
                        let ev = self.tool_event();
                        if self.shape_drag_active() {
                            // Live resize / corner-radius / endpoint edit.
                            self.shape_drag_update(ev.canvas_x, ev.canvas_y);
                        } else {
                            // Immediate rubber-band preview (no throttle).
                            let mut ctx = ToolCtx::new(
                                &mut self.docs.documents[self.docs.active_doc_idx],
                                self.edit.fg_color,
                                self.edit.bg_color,
                                self.edit.view.zoom,
                                self.edit.view.offset_x,
                                self.edit.view.offset_y,
                            );
                            let _ = self.edit.tools.on_drag(ev, &mut ctx);
                        }
                        if let Some(w) = &self.win.window {
                            w.request_redraw();
                        }
                    }
                    ToolId::Node => {
                        // A live node drag is handled above (node_drag branch); a
                        // press that hit nothing must NOT queue a pixel-tool stroke
                        // on the vector Path.
                    }
                    ToolId::VectorBrush => {
                        // Capture the freehand stroke live so the preview grows as
                        // the pointer moves (the pixel-tool `pending_stroke_inputs`
                        // batch only flushes on release, which would drop samples).
                        let event = self.tool_event();
                        let mut ctx = ToolCtx::new(
                            &mut self.docs.documents[self.docs.active_doc_idx],
                            self.edit.fg_color,
                            self.edit.bg_color,
                            self.edit.view.zoom,
                            self.edit.view.offset_x,
                            self.edit.view.offset_y,
                        );
                        let _ = self.edit.tools.on_drag(event, &mut ctx);
                        if let Some(w) = &self.win.window {
                            w.request_redraw();
                        }
                    }
                    _ => {
                        let event = self.tool_event();
                        self.edit.pending_stroke_inputs.push(event);
                    }
                }
            }
        }

        self.edit.input.last_mouse_x = position.x;
        self.edit.input.last_mouse_y = position.y;
        self.push_cursor_uniforms();
        let active = self.edit.tools.active_id();
        // Move tool refreshes the cursor on hover so it can switch to the
        // guide resize cursor when passing over a guide. Warp refreshes so
        // the OS cursor hides over the canvas (egui ring shows) and reappears
        // over the panel.
        if active == ToolId::Crop
            || active == ToolId::PerspectiveCrop
            || active == ToolId::Transform
            || active == ToolId::Move
            || active == ToolId::Text
            || self.edit.warp_state.is_some()
        {
            self.sync_cursor(event_loop);
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::Canvas;
    use crate::core::layer::LayerType;
    use crate::core::shape::{ShapeData, ShapeKind};
    use crate::core::tile::TileMap;
    use crate::core::vector::object::VectorGeometry;
    use crate::extension::tool::PointerEvent;

    #[test]
    fn vector_gradient_release_updates_flat_canvas_without_followup_input() {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(160, 120);
        let index = app.docs.documents[0].canvas.layer_stack.add_layer(160, 120);
        let (shape, offset) = ShapeData::from_canvas_span(
            ShapeKind::Star,
            20.0,
            20.0,
            120.0,
            100.0,
            0.0,
            true,
            [220, 30, 20, 255],
            2.0,
            [0, 0, 0, 255],
        );
        let raster = shape.render().expect("render star");
        {
            let canvas = &mut app.docs.documents[0].canvas;
            let layer = &mut canvas.layer_stack.layers[index];
            layer.offset = offset;
            layer.width = raster.width;
            layer.height = raster.height;
            layer.tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
            layer.layer_type = LayerType::Vector(VectorGeometry::Primitive(shape));
            canvas.layer_stack.active_idx = index;
            canvas.flatten_full();
            canvas.ensure_pixels();
            canvas.dirty.clear();
        }
        let before = app.docs.documents[0].canvas.pixels.clone();
        app.edit.tools.select(ToolId::Gradient);

        let fg = app.edit.fg_color;
        let bg = app.edit.bg_color;
        let zoom = app.edit.view.zoom;
        let pan_x = app.edit.view.offset_x;
        let pan_y = app.edit.view.offset_y;
        {
            let mut ctx = ToolCtx::new(&mut app.docs.documents[0], fg, bg, zoom, pan_x, pan_y);
            let _ = app
                .edit
                .tools
                .on_press(PointerEvent::new(25.0, 30.0), &mut ctx);
            let _ = app
                .edit
                .tools
                .on_drag(PointerEvent::new(115.0, 90.0), &mut ctx);
            let _ = app
                .edit
                .tools
                .on_release(PointerEvent::new(115.0, 90.0), &mut ctx);
        }
        assert!(
            app.docs.documents[0].canvas.dirty.active,
            "tool release must mark the vector bounds dirty"
        );

        // This is the exact App-side release hook. No zoom, Move, layer click,
        // or other synthetic follow-up event is allowed before the assertion.
        app.finish_vector_gradient_release();

        let canvas = &app.docs.documents[0].canvas;
        assert!(!canvas.dirty.active);
        assert!(
            canvas.pixels != before,
            "release hook must flatten the changed vector bounds immediately"
        );
        let LayerType::Vector(VectorGeometry::Primitive(shape)) =
            &canvas.layer_stack.layers[index].layer_type
        else {
            panic!("gradient must keep the Star primitive");
        };
        assert!(matches!(
            shape.style.fill,
            crate::core::vector::style::Paint::Gradient(_)
        ));
    }
}

//! View maths applied to the window: fit, pan clamping, cursor-anchored zoom.

use crate::app::state::App;

impl App {
    /// Height of the bottom chrome the canvas viewport must clear: the status bar
    /// (22 px), plus the artboard page-tab bar (26 px) when the active document has
    /// more than one artboard. Keeps fit/pan from tucking the page under the tabs.
    fn bottom_chrome_h(&self) -> f32 {
        // The page-tab bar shows for any open document (see ui::artboard_bar).
        let has_tabs = !self.has_only_welcome_placeholder();
        22.0 + if has_tabs { 26.0 } else { 0.0 }
    }

    pub fn fit_canvas_to_screen(&mut self) {
        if let Some(win) = &self.win.window {
            let sz = win.inner_size();
            let ruler_size = if self.shell.ui.show_rulers { 20.0 } else { 0.0 };
            let top_ui = 28.0 + 26.0 + 32.0 + ruler_size;
            let bottom_ui = self.bottom_chrome_h();
            let left_ui = self.shell.toolbar_w + ruler_size;
            let right_ui = self.shell.panel_r_w;

            let sw = sz.width as f32 - left_ui - right_ui;
            let sh = sz.height as f32 - top_ui - bottom_ui;
            let cw = self.docs.documents[self.docs.active_doc_idx].canvas.width as f32;
            let ch = self.docs.documents[self.docs.active_doc_idx].canvas.height as f32;
            let padding = 24.0;
            let zoom_x = (sw - padding * 2.0) / cw;
            let zoom_y = (sh - padding * 2.0) / ch;
            self.edit.view.zoom = zoom_x.min(zoom_y).clamp(0.01, 64.0);
            self.constrain_pan();
            self.push_canvas_uniforms();
            self.shell.status_msg = format!("Fit: {:.0}%", self.edit.view.zoom * 100.0);
        }
        self.on_view_changed();
    }

    pub fn constrain_pan(&mut self) {
        if let Some(win) = &self.win.window {
            let sz = win.inner_size();
            let ruler_size = if self.shell.ui.show_rulers { 20.0 } else { 0.0 };
            let top_ui = 28.0 + 26.0 + 32.0 + ruler_size;
            let bottom_ui = self.bottom_chrome_h();
            let left_ui = self.shell.toolbar_w + ruler_size;
            let right_ui = self.shell.panel_r_w;

            let view_w = sz.width as f32 - left_ui - right_ui;
            let view_h = sz.height as f32 - top_ui - bottom_ui;
            let cw = self.docs.documents[self.docs.active_doc_idx].canvas.width as f32
                * self.edit.view.zoom;
            let ch = self.docs.documents[self.docs.active_doc_idx].canvas.height as f32
                * self.edit.view.zoom;

            // While the whole canvas fits in the viewport (not zoomed in), keep it
            // centered — there's nothing to pan to. Once it overflows on EITHER
            // axis (zoomed in enough to pan) allow free panning in all directions:
            // both axes may overscroll, leaving the canvas partly outside the
            // viewport, as long as at least `keep` px of it stays visible (enough
            // to drag any corner to the middle of the screen). This unlocks the
            // axis that still fits instead of snapping it back to center.
            let pannable = cw > view_w || ch > view_h;
            if !pannable {
                self.edit.view.offset_x = left_ui + (view_w - cw) / 2.0;
                self.edit.view.offset_y = top_ui + (view_h - ch) / 2.0;
            } else {
                let keep_x = (view_w * 0.5).min(cw);
                let keep_y = (view_h * 0.5).min(ch);
                self.edit.view.offset_x = self
                    .edit
                    .view
                    .offset_x
                    .clamp(left_ui + keep_x - cw, left_ui + view_w - keep_x);
                self.edit.view.offset_y = self
                    .edit
                    .view
                    .offset_y
                    .clamp(top_ui + keep_y - ch, top_ui + view_h - keep_y);
            }

            // Pin the canvas origin to a whole device pixel. Fractional offsets make
            // the marching-ants edge shimmer and render 0–2px thick (the shader
            // samples `(frag - offset)/zoom`). Snapping at this single funnel keeps
            // the rendered image and hit-testing (which reads the same offset)
            // consistent, so click→pixel mapping doesn't drift.
            self.edit.view.offset_x = self.edit.view.offset_x.round();
            self.edit.view.offset_y = self.edit.view.offset_y.round();
        }
    }

    /// Zoom Tool click: scale the view around the cursor (keeps the point under
    /// the pointer fixed), mirroring the Alt+scroll zoom math.
    pub fn zoom_at_cursor(&mut self, zoom_in: bool) {
        let f = if zoom_in { 1.5_f32 } else { 1.0 / 1.5 };
        let old_zoom = self.edit.view.zoom;
        let new_zoom = (old_zoom * f).clamp(0.02, 64.0);
        self.set_zoom_at_screen(self.edit.input.mouse_x, self.edit.input.mouse_y, new_zoom);
    }

    pub fn set_zoom_at_screen(&mut self, screen_x: f32, screen_y: f32, new_zoom: f32) {
        let old_zoom = self.edit.view.zoom;
        let new_zoom = new_zoom.clamp(0.02, 64.0);
        if (new_zoom - old_zoom).abs() < 1e-6 {
            return;
        }
        let actual_f = new_zoom / old_zoom;
        self.edit.view.offset_x = screen_x - (screen_x - self.edit.view.offset_x) * actual_f;
        self.edit.view.offset_y = screen_y - (screen_y - self.edit.view.offset_y) * actual_f;
        self.edit.view.zoom = new_zoom;
        self.constrain_pan();
        self.push_canvas_uniforms();
        self.win.pending_view_change = true;
        self.win.last_cursor_radius = 0;
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
}

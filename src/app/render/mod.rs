mod composite;
mod develop_preview;
mod uniforms;
mod view;

use crate::app::state::App;

/// Event kinds that require GPU state to be updated.
///
/// Use `App::apply_canvas_event()` instead of calling `push_*/upload_*` directly
/// to avoid missing a step (stale-uniform bugs from skipping one of several).
#[derive(Debug, Clone, Copy)]
pub enum CanvasEvent {
    /// Layer pixel data changed (brush stroke, fill, erase).
    /// → flush the dirty region to the GPU compositor.
    LayerPixelsChanged,

    /// Selection mask or offset changed (grow, shrink, feather, move, arrow key).
    /// → refresh bbox cache + upload mask texture + push shader uniforms + redraw.
    SelectionChanged,

    /// Layer structure changed (add/remove/reorder/opacity/blend/visibility, undo/redo).
    /// → full GPU recomposite + sync selection uniforms if a selection is active.
    LayerStructureChanged,
}

impl App {
    /// Handle a CanvasEvent by calling the right set of push_*/upload_* functions.
    /// The single source of truth for "what GPU state to update after operation X".
    pub fn apply_canvas_event(&mut self, event: CanvasEvent) {
        match event {
            CanvasEvent::LayerPixelsChanged => {
                self.flush_canvas();
            }
            CanvasEvent::SelectionChanged => {
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .selection
                    .refresh_bbox();
                self.upload_selection_mask();
                self.push_selection_uniforms();
            }
            CanvasEvent::LayerStructureChanged => {
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_revision += 1;
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .flatten_full();
                // Canvas-size bookkeeping is centralised HERE: crop/resize/
                // rotate AND their undo/redo all arrive through this event.
                // The undo paths used to skip the GPU-side resize and the view
                // refit, leaving the blit mapping and the selection texture at
                // the old size — the "crop, then Ctrl+Z shows the image
                // shifted with garbage strips" bug.
                let cw = self.docs.documents[self.docs.active_doc_idx].canvas.width;
                let ch = self.docs.documents[self.docs.active_doc_idx].canvas.height;
                let size_changed = self
                    .win
                    .gpu
                    .as_ref()
                    .is_some_and(|g| g.canvas_texture_size() != (cw, ch));
                if size_changed {
                    if let Some(gpu) = &mut self.win.gpu {
                        gpu.resize_canvas_texture(cw, ch);
                        // Force a full recomposite: in Mode A (canvas-space) a
                        // same-window-size resize can leave the compositor
                        // "initialized", so `on_view_changed` below would skip
                        // the recompose and the screen would keep the old
                        // canvas — the "2nd crop doesn't update until I switch
                        // tools" bug. Invalidating here guarantees the visible
                        // texture is rebuilt from the new pixels.
                        gpu.compositor.ping_initialized = false;
                    }
                    // The mask texture was recreated empty — the identical
                    // memo key must not skip the re-upload below.
                    self.win.last_uploaded_mask_key = u64::MAX;
                }
                // Re-pick Mode A/B + resize the compositor before recompositing
                // (a no-op when the size is unchanged, e.g. add/remove layer).
                self.sync_compositor_viewport();
                self.upload_full();
                if self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .selection
                    .active
                {
                    self.upload_selection_mask();
                    self.push_selection_uniforms();
                }
                if size_changed {
                    // Refit like a fresh crop commit does — the old view
                    // mapping points at pasteboard that no longer exists.
                    self.fit_canvas_to_screen();
                }
            }
        }
        // Every canvas event mutates GPU state that must be presented. Scheduling
        // the redraw here (for ALL variants) means callers can't forget it — the
        // old code requested a redraw only for two of three variants, so a
        // `LayerPixelsChanged` from outside a render frame left the screen stale
        // until the next input (the "must scroll to refresh" bug).
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        // …and guarantee a FULL UI rebuild follows: when this event fires from
        // inside `apply_ui_actions`, the frame being processed already built its
        // panels from pre-event data, and `request_redraw` alone can be
        // coalesced away by the platform. The immediate deadline makes
        // `about_to_wait` force the next frame, so e.g. the History panel shows
        // a crop the moment it commits instead of on the next input.
        let now = std::time::Instant::now();
        self.win.egui_repaint_deadline =
            Some(self.win.egui_repaint_deadline.map_or(now, |d| d.min(now)));
        self.refresh_channel_view_display();
    }
}

impl App {
    pub fn sync_brush_gpu_to_cpu(&mut self) {
        self.win.pending_gpu_sync.clear();
        self.win.pending_gpu_sync_layer_id = usize::MAX;
    }

    /// Rebuild the whole GPU context after the device reported itself lost
    /// (driver reset / TDR — typically right after a heavy AI inference or an
    /// idle GPU power cycle). Every wgpu resource and every egui texture lived
    /// on the dead device, so both the GPU state AND the egui context are
    /// recreated from the CPU-side truth; without this the app freezes on an
    /// endless stream of "resource is invalid" validation errors and dies.
    pub fn recover_gpu_device(&mut self) {
        let Some(window) = self.win.window.clone() else {
            return;
        };
        crate::log_crash("GPU device lost — rebuilding the GPU context");
        self.shell.status_msg = "GPU bị reset (driver) — đang khôi phục…".to_string();
        // Drop every device-bound resource before creating the replacement.
        self.win.gpu = None;

        self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .ensure_pixels();
        let (cw, ch) = (
            self.docs.documents[self.docs.active_doc_idx].canvas.width,
            self.docs.documents[self.docs.active_doc_idx].canvas.height,
        );
        // The driver may still be mid-reset; give it a few tries before giving up.
        for attempt in 0..8u32 {
            match crate::gpu::GpuState::new(
                std::sync::Arc::clone(&window),
                &self.docs.documents[self.docs.active_doc_idx].canvas.pixels,
                cw,
                ch,
            ) {
                Ok(g) => {
                    self.win.gpu = Some(g);
                    break;
                }
                Err(e) => {
                    crate::log_crash(&format!("GPU recovery attempt {attempt} failed: {e}"));
                    std::thread::sleep(std::time::Duration::from_millis(
                        150 + attempt as u64 * 100,
                    ));
                }
            }
        }
        if self.win.gpu.is_none() {
            crate::log_crash("GPU recovery failed after all attempts");
            self.shell.status_msg =
                "Không khôi phục được GPU — hãy lưu file và khởi động lại iAi.".to_string();
            return;
        }

        // Fresh egui context: the old one's textures (font atlas, cached logos,
        // panel thumbnails) lived on the dead device. All of them are cached in
        // egui memory / rebuilt lazily, so a clean context re-emits everything.
        self.win.egui_ctx = egui::Context::default();
        if let Some(fonts) = self.win.ui_fonts.clone() {
            self.win.egui_ctx.set_fonts(fonts);
        }
        self.win.theme_applied = None;
        self.win.egui_state = Some(egui_winit::State::new(
            self.win.egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &*window,
            None,
            None,
            // The recovered adapter's real texture ceiling — the egui default
            // (2048) would panic on big textures and shrink the font atlas.
            self.win
                .gpu
                .as_ref()
                .map(|g| g.max_texture_dimension as usize),
        ));
        self.win.cached_egui_primitives = Vec::new();
        self.edit.refine_overlay_tex = None;
        self.edit.refine_overlay_mask_rev = u64::MAX;
        self.win.last_uploaded_mask_key = u64::MAX;

        // Re-derive the GPU state from the CPU-side truth, exactly like a
        // fresh open: composite, selection, proof LUT, uniforms.
        self.sync_compositor_viewport();
        self.upload_full();
        if self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .selection
            .active
        {
            self.upload_selection_mask();
            self.push_selection_uniforms();
        }
        self.apply_proof_settings();
        self.push_cursor_uniforms();
        self.dev.develop_gpu_preview_dirty = true;

        // The Develop second window (if open) had its surface AND egui context on
        // the dead device too — rebuild both so it keeps rendering, or close it
        // cleanly if the replacement surface can't be created.
        if let Some(dev_window) = self.win.develop_window.clone() {
            let rebuilt = self
                .win
                .gpu
                .as_mut()
                .map(|g| {
                    g.open_develop_surface(std::sync::Arc::clone(&dev_window))
                        .is_ok()
                })
                .unwrap_or(false);
            if rebuilt {
                let ctx = egui::Context::default();
                if let Some(fonts) = self.win.ui_fonts.clone() {
                    ctx.set_fonts(fonts);
                }
                crate::ui::theme::apply_theme(&ctx, self.shell.ui.theme_mode);
                self.win.develop_egui_state = Some(egui_winit::State::new(
                    ctx.clone(),
                    egui::ViewportId::ROOT,
                    &*dev_window,
                    None,
                    None,
                    self.win
                        .gpu
                        .as_ref()
                        .map(|g| g.max_texture_dimension as usize),
                ));
                self.win.develop_egui_ctx = Some(ctx);
                dev_window.request_redraw();
            } else {
                self.close_develop_window();
            }
        }

        self.shell.status_msg = "Đã khôi phục GPU sau sự cố driver.".to_string();
        window.request_redraw();
    }
}

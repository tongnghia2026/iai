//! Composite scheduling: dirty-region flushes, full/visible recomposites,
//! and the interactive/view-change throttles that keep drags smooth.

use crate::app::state::App;

impl App {
    /// Mark every PowerClip / clipping-mask content layer's canvas region dirty,
    /// so a re-fit is recomposited even when the base only changed coverage
    /// (nothing moved to mark it). Called after a clip re-bake changed something.
    fn mark_clip_content_dirty(&mut self) {
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let bounds: Vec<(i32, i32, u32, u32)> = canvas
            .layer_stack
            .layers
            .iter()
            .filter(|l| l.clip_parent_id.is_some())
            .map(|l| (l.offset.0, l.offset.1, l.width, l.height))
            .collect();
        for (x, y, w, h) in bounds {
            canvas.mark_dirty_layer_bounds(x, y, w, h);
        }
    }

    pub fn flush_canvas(&mut self) {
        // Re-pin clipping-mask / PowerClip content to its base — but NOT while a
        // Move drag is in flight. Re-baking the clip is O(content area) on the CPU
        // plus a GPU re-upload; doing that every frame made dragging a large
        // clipped image unusably slow. While dragging, the content moves with its
        // existing mask (as cheap as any layer move); on release — and on any
        // other recomposite — we re-bake once so the clip snaps back onto the base.
        // Fingerprint-gated (Canvas::clip_fp): no clip, or unchanged geometry, is a
        // cheap hash.
        let interactive_move =
            self.edit.input.painting && self.edit.tools.active_id() == crate::tools::ToolId::Move;
        if !interactive_move
            && self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .refresh_clip_masks()
        {
            // The clip moved: mark its content's region dirty so this flush
            // actually recomposites it (a move already marks dirty, but a base
            // edit that only changed coverage might not).
            self.mark_clip_content_dirty();
        }
        if self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .plane_dirty
            .active
        {
            let dirty = self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .plane_dirty
                .to_rect();
            self.upload_viewed_channel_plane(dirty);
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }

        if self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .dirty
            .active
        {
            let is_large = self.win.gpu.as_ref().map_or(false, |g| g.is_large_canvas);
            let interactive_move = self.edit.input.painting
                && self.edit.tools.active_id() == crate::tools::ToolId::Move;
            // A live Shape edit (handle drag, worker bake landing, or a dirty
            // style scrub) re-renders the layer repeatedly; defer the CPU
            // flatten and recomposite only the dirty rect, exactly like a
            // brush stroke (the full sync happens once in shape_drag_finish).
            let shape_dragging = self.edit.shape_drag.is_some()
                || self.jobs.shape_bake.is_some()
                || self.shape_style_scrub_active();
            let defer_flatten = shape_dragging
                || self.edit.input.painting
                    && (self.edit.transform_state.is_some()
                        || matches!(
                            self.edit.tools.active_id(),
                            crate::tools::ToolId::Brush
                                | crate::tools::ToolId::Eraser
                                | crate::tools::ToolId::Pencil
                                | crate::tools::ToolId::Move
                        ));
            if !is_large
                && !self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .pixels_stale
                && !self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .pixels
                    .is_empty()
            {
                if defer_flatten {
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .pixels_stale = true;
                } else {
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .flatten_dirty();
                }
            }

            if self.is_interactive_edit() {
                self.sync_compositor_viewport();
            }

            let paint_partial_ok = shape_dragging
                || self.edit.input.painting
                    && matches!(
                        self.edit.tools.active_id(),
                        crate::tools::ToolId::Brush
                            | crate::tools::ToolId::Eraser
                            | crate::tools::ToolId::Pencil
                            | crate::tools::ToolId::Clone
                            | crate::tools::ToolId::Smudge
                            | crate::tools::ToolId::Dodge
                            | crate::tools::ToolId::Burn
                            | crate::tools::ToolId::Patch
                            | crate::tools::ToolId::Move
                    );
            let canvas_space = self
                .win
                .gpu
                .as_ref()
                .map_or(false, |g| g.compositor.canvas_space);
            let partial_dirty: Option<(u32, u32, u32, u32)> = if !paint_partial_ok {
                None
            } else if canvas_space {
                // Mode A scissors the canvas-space texture directly in canvas pixels.
                let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
                let d = &canvas.dirty;
                let x0 = d.x0.min(canvas.width);
                let y0 = d.y0.min(canvas.height);
                let x1 = d.x1.min(canvas.width);
                let y1 = d.y1.min(canvas.height);
                (x1 > x0 && y1 > y0).then_some((x0, y0, x1 - x0, y1 - y0))
            } else {
                self.win.window.as_ref().and_then(|win| {
                    let sz = win.inner_size();
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .dirty
                        .as_screen_rect(
                            self.edit.view.offset_x,
                            self.edit.view.offset_y,
                            self.edit.view.zoom,
                            sz.width,
                            sz.height,
                        )
                })
            };

            let recomposed = if interactive_move {
                self.try_recompose_interactive(partial_dirty)
            } else {
                self.recomposite_with_dirty(partial_dirty);
                true
            };

            if recomposed {
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .dirty
                    .clear();
            }
        }
    }

    pub fn recomposite(&mut self) {
        self.recomposite_with_dirty(None);
    }

    /// Canvas-pixel rectangle currently visible in the window (clamped to the
    /// canvas). Over-estimates slightly (ignores UI panels) but stays bounded by
    /// the viewport — used to cap Mode-A interactive recomposites.
    pub(in crate::app) fn visible_canvas_rect(&self) -> Option<(u32, u32, u32, u32)> {
        let win = self.win.window.as_ref()?;
        let sz = win.inner_size();
        let z = self.edit.view.zoom;
        if z <= 0.0 {
            return None;
        }
        let cw = self.docs.documents[self.docs.active_doc_idx].canvas.width as f32;
        let ch = self.docs.documents[self.docs.active_doc_idx].canvas.height as f32;
        let cx0 = ((-self.edit.view.offset_x) / z).floor().clamp(0.0, cw);
        let cy0 = ((-self.edit.view.offset_y) / z).floor().clamp(0.0, ch);
        let cx1 = ((sz.width as f32 - self.edit.view.offset_x) / z)
            .ceil()
            .clamp(0.0, cw);
        let cy1 = ((sz.height as f32 - self.edit.view.offset_y) / z)
            .ceil()
            .clamp(0.0, ch);
        ((cx1 > cx0) && (cy1 > cy0)).then(|| {
            (
                cx0 as u32,
                cy0 as u32,
                (cx1 - cx0) as u32,
                (cy1 - cy0) as u32,
            )
        })
    }

    /// Recomposite for an interactive drag (e.g. Free Transform). In Mode A this
    /// scissors to the visible canvas region so the per-frame cost is bounded by
    /// the viewport, not the full canvas — otherwise transforming an 8000×6000
    /// canvas re-composites all 48M px every frame and freezes. Off-screen stays
    /// cached and is reconciled by the full recomposite on commit. Mode B already
    /// composites only the viewport.
    pub fn recomposite_visible(&mut self) {
        let canvas_space = self
            .win
            .gpu
            .as_ref()
            .map_or(false, |g| g.compositor.canvas_space);
        if canvas_space {
            if let Some(rect) = self.visible_canvas_rect() {
                self.recomposite_with_dirty(Some(rect));
                return;
            }
        }
        self.recomposite();
    }

    pub(in crate::app) fn schedule_interactive_recompose(&mut self, delay: std::time::Duration) {
        self.win.interactive_recompose_pending = true;
        let deadline = std::time::Instant::now() + delay;
        self.win.egui_repaint_deadline = Some(
            self.win
                .egui_repaint_deadline
                .map_or(deadline, |current| current.min(deadline)),
        );
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    pub fn request_interactive_recompose(&mut self) {
        self.schedule_interactive_recompose(std::time::Duration::ZERO);
    }

    /// Recompose a live Move/Free Transform preview at a sustainable cadence.
    /// Mouse events can arrive much faster than the compositor can process a large
    /// layer stack; this keeps the latest state and drops redundant intermediate
    /// composites instead of queueing work the user will never see.
    pub fn try_recompose_interactive(&mut self, dirty_rect: Option<(u32, u32, u32, u32)>) -> bool {
        if !self.is_interactive_edit() {
            self.win.interactive_recompose_pending = false;
            self.recomposite_with_dirty(dirty_rect);
            return true;
        }

        let now = std::time::Instant::now();
        if let Some(last) = self.win.interactive_recompose_last {
            let interval = self.win.interactive_recompose_cost.mul_f32(0.75).clamp(
                std::time::Duration::from_millis(4),
                std::time::Duration::from_millis(24),
            );
            let elapsed = now.duration_since(last);
            if elapsed < interval {
                self.schedule_interactive_recompose(interval - elapsed);
                return false;
            }
        }

        self.win.interactive_recompose_pending = false;
        let start = std::time::Instant::now();
        match dirty_rect {
            Some(rect) => self.recomposite_with_dirty(Some(rect)),
            None => self.recomposite_visible(),
        }
        let end = std::time::Instant::now();
        self.win.interactive_recompose_cost = end.duration_since(start);
        self.win.interactive_recompose_last = Some(end);
        true
    }

    pub fn flush_pending_interactive_recompose(&mut self) {
        if !self.win.interactive_recompose_pending {
            return;
        }
        if !self.is_interactive_edit() {
            self.win.interactive_recompose_pending = false;
            return;
        }

        if self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .dirty
            .active
        {
            self.flush_canvas();
        } else {
            let _ = self.try_recompose_interactive(None);
        }
    }

    /// Full recomposite when `dirty_rect = None`; partial scissored composite when `Some`.
    /// The compositor guards correctness via `ping_initialized` — callers can freely
    /// pass `Some` and the compositor will fall back to full clear if needed.
    pub fn recomposite_with_dirty(&mut self, dirty_rect: Option<(u32, u32, u32, u32)>) {
        // Live clip move: the backdrop cache freezes the layers below the active
        // one, but a dragged clip child is pinned to a FIXED frame that its moving
        // dirty rect doesn't track, so the frozen base could show through stale —
        // the "bóng ma" that doubled the layer below. Disable the backdrop for this
        // case so the base is recomposited fresh inside the (cheap) partial scissor,
        // which the moved content bounds already cover (the frame sits inside them).
        // NB: keep the partial scissor — a FULL viewport recompose every frame gets
        // throttled, which left newly-revealed frame regions showing the base until
        // release (base "covers" the image mid-drag).
        let clip_live_move = self.is_interactive_edit()
            && self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .has_clip_content();
        // Pick Mode A/B (Mode B while interactively moving/transforming) and size
        // the compositor before computing the composite.
        self.sync_compositor_viewport();
        // Mode B bakes a view transform into the composite. With the Develop
        // window open that must be ITS view (the compositor serves only it);
        // resolve a pending fit first so the bake and the blit agree.
        let canvas_space = self
            .win
            .gpu
            .as_ref()
            .map_or(true, |g| g.compositor.canvas_space);
        let develop_view = if !canvas_space && self.win.develop_window.is_some() {
            self.develop_resolve_fit();
            Some((self.dev.develop_view_off, self.dev.develop_view_zoom))
        } else {
            None
        };
        let dev_preview = self.build_develop_gpu_preview();
        if let Some(gpu) = &mut self.win.gpu {
            // Mode A composites in canvas-space (identity view); the dirty rect is
            // already in canvas pixels. Mode B bakes the view transform, so the
            // dirty rect (from flush_canvas) is in screen pixels.
            let canvas_space = gpu.compositor.canvas_space;
            let (view_off, view_zoom) = match develop_view {
                Some((off, zoom)) => (off, zoom),
                None => (
                    (self.edit.view.offset_x, self.edit.view.offset_y),
                    self.edit.view.zoom,
                ),
            };
            let (comp_off_x, comp_off_y, comp_zoom) = if canvas_space {
                (0.0, 0.0, 1.0)
            } else {
                let inv_zoom = if view_zoom > 0.0 {
                    1.0 / view_zoom
                } else {
                    1.0
                };
                (-view_off.0 * inv_zoom, -view_off.1 * inv_zoom, view_zoom)
            };
            let comp_inv_zoom = if comp_zoom > 0.0 {
                1.0 / comp_zoom
            } else {
                1.0
            };
            // While a Shape layer is interactively re-baked its content
            // fingerprint changes on every bake — a LOD-proxy downsample
            // spawned per bake is stale before it lands. Suspend proxy builds
            // for that layer; the first recompose after the edit settles
            // builds one for keeps.
            gpu.compositor.proxy_build_suspend = self
                .edit
                .shape_drag
                .as_ref()
                .map(|d| d.layer_id)
                .or_else(|| self.jobs.shape_bake.as_ref().map(|j| j.layer_id))
                .or_else(|| {
                    self.edit
                        .shape_style_pending
                        .as_ref()
                        .filter(|p| p.dirty)
                        .map(|p| p.layer_id)
                });
            let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
            gpu.compositor.develop_preview = dev_preview;
            let has_groups = canvas.layer_stack.has_effected_groups();
            let effective_dirty = if has_groups {
                dirty_rect.filter(|_| gpu.compositor.ping_initialized)
            } else {
                dirty_rect
            };

            let render_stack_owned;
            let render_stack_ref = if has_groups {
                let (sx, sy, sw, sh) = effective_dirty.unwrap_or((
                    0,
                    0,
                    gpu.compositor.viewport_w,
                    gpu.compositor.viewport_h,
                ));
                let cx0 = ((sx as f32 * comp_inv_zoom + comp_off_x).floor() as i32)
                    .clamp(0, canvas.width as i32) as u32;
                let cy0 = ((sy as f32 * comp_inv_zoom + comp_off_y).floor() as i32)
                    .clamp(0, canvas.height as i32) as u32;
                let cx1 = (((sx + sw) as f32 * comp_inv_zoom + comp_off_x).ceil() as i32)
                    .clamp(0, canvas.width as i32) as u32;
                let cy1 = (((sy + sh) as f32 * comp_inv_zoom + comp_off_y).ceil() as i32)
                    .clamp(0, canvas.height as i32) as u32;
                let stack = canvas.layer_stack.to_render_stack_region(
                    canvas.width,
                    canvas.height,
                    cx0,
                    cy0,
                    cx1.saturating_sub(cx0),
                    cy1.saturating_sub(cy0),
                );
                render_stack_owned = Some(stack);
                render_stack_owned.as_ref().unwrap()
            } else {
                &canvas.layer_stack
            };

            // The top visible vector run is drawn separately from one
            // zoom-bucketed supersampled raster. Remove every corresponding
            // document-resolution cache so coarse edge pixels cannot show
            // through underneath the smoother overlay.
            let display_stack_owned = self
                .shell
                .ui_data_cache
                .path_display_suppressed_layers
                .as_ref()
                .filter(|(doc_id, _)| *doc_id == self.docs.documents[self.docs.active_doc_idx].id.0)
                .map(|(_, layer_ids)| {
                    let mut stack = render_stack_ref.clone();
                    for layer in &mut stack.layers {
                        if layer_ids.contains(&layer.id) {
                            layer.visible = false;
                        }
                    }
                    stack
                });
            let render_stack_ref = display_stack_owned.as_ref().unwrap_or(render_stack_ref);

            gpu.composite_layers(
                render_stack_ref,
                comp_off_x,
                comp_off_y,
                comp_zoom,
                effective_dirty,
                // Backdrop cache. Enabled in both modes: Mode A composites
                // view-independently, and Mode B (viewport-streaming) bakes the
                // view in but the view transform is part of the snapshot key
                // (`vp_sig`), so a zoom/pan simply misses and recomposites. Only
                // disabled for the synthetic group-isolation stack (whose indices
                // don't match the real stack the cut is computed from) and for a
                // live clip move (the frozen base would ghost — see above).
                !has_groups && !clip_live_move,
            );
            // Remember which Develop-window view this Mode B composite baked, so
            // the Develop window recomposites only when its view actually moves.
            self.dev.develop_composited_view = develop_view.map(|(off, zoom)| {
                [
                    off.0.to_bits(),
                    off.1.to_bits(),
                    zoom.to_bits(),
                    gpu.compositor.viewport_w,
                    gpu.compositor.viewport_h,
                ]
            });
            // A large zoomed-out layer builds its LOD proxy on a worker thread and
            // renders full-res meanwhile; keep repainting until it lands so the
            // proxy engages even if the zoom gesture has already stopped.
            if gpu.compositor.has_pending_proxy_builds() {
                self.win.egui_ctx.request_repaint();
            }
        }
    }

    pub fn upload_full(&mut self) {
        self.recomposite();
        self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .dirty
            .clear();
        self.refresh_channel_view_display();
    }

    /// Pick the compositing space for the active canvas and size the compositor's
    /// ping/pong accordingly: canvas-sized for Mode A (fits in a texture → cheap
    /// zoom/pan), viewport-sized for Mode B (large/streaming). `resize_viewport`
    /// is a no-op when the size is unchanged (so this is cheap to call per view
    /// change) and clears `ping_initialized` when it does resize.
    pub fn sync_compositor_viewport(&mut self) {
        let Some(win) = self.win.window.as_ref() else {
            return;
        };
        // While the Develop window is open the compositor serves IT exclusively
        // (the main window is soft-modal locked), so Mode B's viewport must be
        // the Develop window's size, not the main window's.
        let win_sz = match &self.win.develop_window {
            Some(dev) => dev.inner_size(),
            None => win.inner_size(),
        };
        let cw = self.docs.documents[self.docs.active_doc_idx].canvas.width;
        let ch = self.docs.documents[self.docs.active_doc_idx].canvas.height;
        // During an interactive move/transform, force Mode B (viewport-sized) so
        // each per-frame recomposite only fills the VIEWPORT (~1-2M px), not the
        // full canvas resolution (e.g. 17.5M px) Mode A would composite. Zoom/pan
        // (non-interactive) keeps Mode A's cached, recomposite-free re-blit.
        //
        // Exception: an on-canvas Path scale/rotate previews entirely through the
        // GPU transform slots, whose math is canvas-space (mode-agnostic — see the
        // `xform_active` branch in compositor.wgsl) and which Mode A composites
        // fine through its scissored partial path. Forcing Mode B for it would flip
        // the ping/pong textures (canvas-size ↔ window-size) on gesture start,
        // reallocating them and clearing `ping_initialized` — so the FIRST frame of
        // the first such gesture pays a full-viewport recomposite plus a cold
        // texture allocation, the visible one-beat hitch users hit when they first
        // grab a rotate/scale handle. Staying in Mode A keeps `ping_initialized`
        // and the textures, so the first frame is a cheap partial composite. A
        // genuinely large / streaming canvas still switches to Mode B below via
        // `is_large_canvas` / `viewport_streaming`.
        let interactive = self.is_interactive_edit() && self.edit.path_transform.is_none();
        let mut mode_changed = false;
        if let Some(gpu) = &mut self.win.gpu {
            let canvas_pixels = (cw as u64).saturating_mul(ch as u64);
            let viewport_streaming = canvas_pixels > 12_000_000;
            let canvas_space = !gpu.is_large_canvas && !viewport_streaming && !interactive;
            if gpu.compositor.canvas_space != canvas_space {
                mode_changed = true;
            }
            gpu.compositor.canvas_space = canvas_space;
            let (vw, vh) = if canvas_space {
                (cw, ch)
            } else {
                (win_sz.width.max(1), win_sz.height.max(1))
            };
            // Interactive move/transform still uses Mode B (viewport-sized) to
            // avoid full-canvas compositing, but keep it at native viewport
            // resolution. The lower-res proxy was fast but visibly destroyed image
            // detail during drag.
            let scale = 1;
            gpu.compositor
                .configure_viewport(&gpu.device, vw, vh, scale);
        }
        // The blit's `vp_mode` must track the mode (Mode A positions a canvas-sized
        // texture by offset+zoom; Mode B is a fullscreen blit). Re-push when it flips.
        if mode_changed {
            self.push_canvas_uniforms();
        }
    }

    /// True while dragging a Free Transform, moving layers, or dragging a Shape
    /// handle — these recomposite a large region every frame, so they run in
    /// Mode B (viewport resolution).
    pub(in crate::app) fn is_interactive_edit(&self) -> bool {
        self.edit.transform_state.is_some()
            || (self.edit.input.painting
                && self.edit.tools.active_id() == crate::tools::ToolId::Move)
            || self.edit.shape_drag.is_some()
    }

    pub fn on_view_changed(&mut self) {
        self.sync_compositor_viewport();
        if self.active_view_uses_canvas_override() {
            self.upload_viewed_channel_plane(None);
            if self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .selection
                .active
            {
                let canvas_space = self
                    .win
                    .gpu
                    .as_ref()
                    .map_or(false, |g| g.compositor.canvas_space);
                if !canvas_space {
                    self.upload_selection_mask();
                }
                self.push_selection_uniforms();
            }
            return;
        }

        let canvas_space = self
            .win
            .gpu
            .as_ref()
            .map_or(false, |g| g.compositor.canvas_space);
        if canvas_space {
            // The canvas-space composite is view-independent. Recompose only if it's
            // stale (mode just activated / canvas resized → ping reset); otherwise a
            // zoom/pan is just a re-blit with new uniforms — no layer compositing.
            let stale = self
                .win
                .gpu
                .as_ref()
                .map_or(true, |g| !g.compositor.ping_initialized);
            if stale {
                self.recomposite();
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .dirty
                    .clear();
            } else {
                self.push_canvas_uniforms();
            }
        } else {
            self.recomposite();
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .dirty
                .clear();
        }
        if self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .selection
            .active
        {
            // The screen-space (large-canvas) mask must be re-rasterized for the new
            // view; the canvas-space mask is view-independent — just re-map it.
            if !canvas_space {
                self.upload_selection_mask();
            }
            self.push_selection_uniforms();
        }
    }

    pub fn flush_pending_view_change(&mut self) -> bool {
        if !self.win.pending_view_change {
            return false;
        }

        let large_view = self
            .win
            .gpu
            .as_ref()
            .map_or(false, |g| g.is_large_canvas || !g.compositor.canvas_space);
        let should_throttle = large_view && !self.is_interactive_edit();
        let now = std::time::Instant::now();

        if should_throttle {
            if let Some(deadline) = self.win.view_recompose_deadline {
                if now < deadline {
                    self.win.egui_repaint_deadline = Some(
                        self.win
                            .egui_repaint_deadline
                            .map_or(deadline, |current| current.min(deadline)),
                    );
                    return false;
                }
            } else if let Some(last) = self.win.view_recompose_last {
                let interval = self.win.view_recompose_cost.mul_f32(1.0).clamp(
                    std::time::Duration::from_millis(6),
                    std::time::Duration::from_millis(28),
                );
                let elapsed = now.duration_since(last);
                if elapsed < interval {
                    let deadline = now + (interval - elapsed);
                    self.win.view_recompose_deadline = Some(deadline);
                    self.win.egui_repaint_deadline = Some(
                        self.win
                            .egui_repaint_deadline
                            .map_or(deadline, |current| current.min(deadline)),
                    );
                    return false;
                }
            }
        }

        self.win.pending_view_change = false;
        self.win.view_recompose_deadline = None;
        let start = std::time::Instant::now();
        self.on_view_changed();
        let end = std::time::Instant::now();
        if should_throttle {
            self.win.view_recompose_cost = end.duration_since(start);
            self.win.view_recompose_last = Some(end);
        }
        true
    }
}

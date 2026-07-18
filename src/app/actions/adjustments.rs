//! Adjustment previews (Levels/Curves dialogs), auto-levels and
//! adjustment-layer editing. Split out of app/actions.rs.

use crate::app::render::CanvasEvent;
use crate::app::state::App;
use crate::ui::UiActions;

impl App {
    pub(super) fn handle_direct_adjustment_actions(&mut self, actions: &mut UiActions) {
        if actions.dialogs.auto_levels {
            self.do_auto_levels();
        }

        if let Some(adj) = actions.dialogs.apply_direct_adjustment.take() {
            let name = adj.name().to_string();
            if self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .apply_adjustment_to_active_layer(adj)
            {
                self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
                self.shell.status_msg = name;
            } else {
                self.shell.status_msg = format!("{name} requires an unlocked raster layer");
            }
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
    }

    pub(crate) fn begin_adjustment_preview(
        &mut self,
        adj: crate::core::layer::AdjustmentType,
    ) -> bool {
        self.cancel_adjustment_preview();
        self.shell.ui.show_filter_dialog = false;
        self.cancel_filter_preview();
        self.abandon_develop_session();

        let doc_id = self.docs.documents[self.docs.active_doc_idx].id;
        let preview = {
            let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
            canvas.layer_stack.normalize_active_idx();
            if canvas.layer_stack.layers.is_empty() {
                return false;
            }
            // CMYK docs only take ink-native adjustments (Levels/Curves per
            // ink); anything else would corrupt the ink ground truth.
            if canvas.is_cmyk() && !adj.is_ink_native() {
                return false;
            }

            let idx = canvas.layer_stack.active_idx;
            let layer = &canvas.layer_stack.layers[idx];
            if (!layer.is_background && layer.locked) || !layer.is_raster() {
                return false;
            }

            let original_tiles = layer.tiles.clone();
            let original_flat = original_tiles.flatten();
            let levels_histogram = if canvas.is_cmyk() {
                levels_histogram_from_ink(&original_tiles)
            } else {
                levels_histogram_from_rgba(&original_flat)
            };
            crate::app::state::AdjustmentPreviewState {
                doc_id,
                layer_id: layer.id,
                original_tiles,
                original_flat,
                levels_histogram,
            }
        };

        self.shell.adjustment_preview = Some(preview);
        self.shell.ui.adjustment_dialog = adj.clone();
        self.shell.ui.adjustment_preview_enabled = true;
        self.shell.ui.adj_eyedropper = None;
        self.shell.ui.show_adjustment_dialog = true;
        self.update_adjustment_preview(adj)
    }

    pub(crate) fn update_adjustment_preview(
        &mut self,
        adj: crate::core::layer::AdjustmentType,
    ) -> bool {
        // Take the preview out so we can borrow `original_tiles` while mutably
        // borrowing the canvas — avoids cloning the whole layer tilemap every
        // drag step, which is the dominant cost of live preview on large images.
        let Some(preview) = self.shell.adjustment_preview.take() else {
            return false;
        };
        if self.docs.documents[self.docs.active_doc_idx].id != preview.doc_id {
            self.shell.adjustment_preview = Some(preview);
            return false;
        }

        let layer_id = preview.layer_id;
        // A layer inside an isolated group (opacity/blend/mask) is pre-flattened on
        // the CPU for rendering, so the shader-overlay preview keyed to its id never
        // shows. Route it through the CPU tile bake below, which the group flatten
        // reads — the "color/light drag doesn't preview inside a group" fix.
        let in_isolated_group = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .layer_in_isolated_group(layer_id);

        if !self.shell.ui.adjustment_preview_enabled {
            if let Some(gpu) = &mut self.win.gpu {
                gpu.compositor.preview_adj = None;
            }
            let restored = self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .restore_layer_tiles(layer_id, preview.original_tiles.clone());
            self.shell.adjustment_preview = Some(preview);
            if restored {
                self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
            } else {
                self.recomposite_visible();
            }
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return true;
        }

        // A CMYK doc must go through the CPU ink bake below — the shader overlay
        // is an RGB LUT on the mirror and can't reproduce a per-ink LUT.
        let is_cmyk = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .is_cmyk();

        // GPU path: apply the adjustment in the compositor shader on the layer's
        // own pixels and just recomposite — no per-tick CPU bake/flatten of the
        // whole layer (that full-image work was the source of the Ctrl+L/M lag on
        // large canvases). The layer tiles stay pristine; commit bakes them. Skipped
        // when the layer sits in an isolated group (the overlay would never show).
        if self.win.gpu.is_some() && !in_isolated_group && !is_cmyk {
            if let Some(gpu) = &mut self.win.gpu {
                gpu.compositor.preview_adj = Some((layer_id, adj));
            }
            self.shell.adjustment_preview = Some(preview);
            // Bound to the visible region in Mode A (same reason as Free Transform):
            // a per-tick full recomposite of a huge canvas would lag the slider.
            self.recomposite_visible();
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return true;
        }

        // CPU bake path: no GPU, or the layer is inside an isolated group. Drop any
        // stale shader overlay, then bake the adjustment into the layer tiles (from
        // the pristine snapshot) so the group flatten / CPU composite shows it.
        if let Some(gpu) = &mut self.win.gpu {
            gpu.compositor.preview_adj = None;
        }
        let ok = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .preview_adjustment_on_layer(layer_id, &preview.original_tiles, &adj);
        self.shell.adjustment_preview = Some(preview);
        if ok {
            self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        ok
    }

    /// Throttled driver for the live adjustment preview. The dialog stores the
    /// latest params every frame (cheap, keeps sliders/gradient handles real-time)
    /// and this recomputes the expensive full-layer apply + recomposite no faster
    /// than the previous recompute could afford — so editing stays responsive on
    /// large images. Call once per frame; a trailing repaint guarantees the final
    /// value previews after the user stops.
    pub(crate) fn flush_pending_adjustment_preview(&mut self) {
        if self.shell.adjustment_preview_pending.is_none() {
            return;
        }
        if !self.shell.ui.show_adjustment_dialog {
            self.shell.adjustment_preview_pending = None;
            return;
        }
        // Interval adapts to the last recompute's cost: cheap adjustments on small
        // images stay at ~frame rate; expensive ones on large images back off so the
        // UI thread isn't saturated and dragging stays responsive.
        let interval = self.shell.adjustment_preview_cost.mul_f32(1.5).clamp(
            std::time::Duration::from_millis(16),
            std::time::Duration::from_millis(250),
        );
        let now = std::time::Instant::now();
        let elapsed = self
            .shell
            .adjustment_preview_last
            .map_or(interval, |t| now.duration_since(t));
        if elapsed < interval {
            // Not yet — schedule a trailing frame via the app's own redraw clock so
            // the final value still previews after the user stops. (egui's
            // `request_repaint_after` is set too late here: `egui_repaint_deadline`
            // was already computed from this frame's egui output, so we must lower it
            // directly or about_to_wait parks on `Wait` and the preview freezes.)
            // Lower the app's redraw clock so about_to_wait parks on WaitUntil(deadline)
            // and wakes to flush the final value — do NOT request_redraw here, that
            // forces an immediate frame and busy-spins until the interval elapses.
            let deadline = now + (interval - elapsed);
            self.win.egui_repaint_deadline = Some(
                self.win
                    .egui_repaint_deadline
                    .map_or(deadline, |d| d.min(deadline)),
            );
            return;
        }
        let Some(adj) = self.shell.adjustment_preview_pending.take() else {
            return;
        };
        let start = std::time::Instant::now();
        if self.shell.adjustment_preview.is_some() {
            self.update_adjustment_preview(adj);
        } else if self.edit.adjustment_layer_edit.is_some() {
            self.update_adjustment_layer_edit(adj);
        }
        self.shell.adjustment_preview_cost = start.elapsed();
        self.shell.adjustment_preview_last = Some(std::time::Instant::now());
    }

    pub(crate) fn commit_adjustment_preview(
        &mut self,
        adj: &crate::core::layer::AdjustmentType,
    ) -> bool {
        let Some(preview) = self.shell.adjustment_preview.take() else {
            return false;
        };
        // Drop the GPU preview overlay; the result is baked into tiles below.
        if let Some(gpu) = &mut self.win.gpu {
            gpu.compositor.preview_adj = None;
        }
        if self.docs.documents[self.docs.active_doc_idx].id != preview.doc_id {
            return false;
        }

        let (ok, changed) = {
            let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
            let Some(idx) = canvas
                .layer_stack
                .layers
                .iter()
                .position(|l| l.id == preview.layer_id)
            else {
                return false;
            };

            // Materialize the destructive result from the pristine snapshot — the
            // GPU preview path leaves the live tiles untouched, so bake here.
            canvas.preview_adjustment_on_layer(preview.layer_id, &preview.original_tiles, adj);
            let after_tiles = canvas.layer_stack.layers[idx].tiles.clone();
            let changed = after_tiles.flatten() != preview.original_flat;
            if changed {
                (
                    canvas.commit_layer_tiles_change(
                        preview.layer_id,
                        preview.original_tiles,
                        after_tiles,
                        adj.name(),
                    ),
                    true,
                )
            } else {
                (
                    canvas.restore_layer_tiles(preview.layer_id, preview.original_tiles),
                    false,
                )
            }
        };

        if ok {
            self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
        }
        ok && changed
    }

    /// Auto Levels (Ctrl+Shift+L): stretch the active layer's tonal range using the
    /// black/white points the Levels dialog's "Auto" button derives, applied directly
    /// without opening a dialog.
    pub(crate) fn do_auto_levels(&mut self) {
        let idx = self.docs.active_doc_idx;
        self.docs.documents[idx]
            .canvas
            .layer_stack
            .normalize_active_idx();
        if self.docs.documents[idx]
            .canvas
            .layer_stack
            .layers
            .is_empty()
        {
            return;
        }
        let bad = {
            let l = self.docs.documents[idx].canvas.active_layer();
            (!l.is_background && l.locked) || !l.is_raster()
        };
        if bad {
            self.shell.status_msg = "Auto Levels requires an unlocked raster layer".to_string();
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return;
        }
        // Auto Levels derives its stretch from RGB/luma histograms — no ink
        // meaning, and its [master,r,g,b] result would misread as [C,M,Y,K].
        if self.docs.documents[idx].canvas.is_cmyk() {
            self.shell.status_msg = "Auto Levels chưa hỗ trợ ở chế độ CMYK".to_string();
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return;
        }
        let pixels = self.docs.documents[idx]
            .canvas
            .active_layer()
            .flatten_tiles();
        let hist = levels_histogram_from_rgba(&pixels);
        let Some(channels) = crate::ui::dialogs::auto_levels_channels_from_histogram(
            &hist,
            self.shell.ui.adjustment_options,
        ) else {
            self.shell.status_msg = "Auto Levels: not enough tonal range".to_string();
            return;
        };
        let ok = self.docs.documents[idx]
            .canvas
            .apply_adjustment_to_active_layer(crate::core::layer::AdjustmentType::Levels {
                channels,
            });
        if ok {
            self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
            self.shell.status_msg = "Auto Levels".to_string();
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Open the editor for an adjustment layer (double-click). Non-destructive:
    /// only edits the `AdjustmentType` params stored in the layer.
    pub(crate) fn begin_adjustment_layer_edit(&mut self, idx: usize) {
        use crate::core::layer::LayerType;
        self.cancel_adjustment_preview();
        self.abandon_develop_session();

        let doc_id = self.docs.documents[self.docs.active_doc_idx].id;
        let (layer_id, adj) = {
            let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
            let Some(layer) = canvas.layer_stack.layers.get(idx) else {
                return;
            };
            let LayerType::Adjustment(adj) = &layer.layer_type else {
                self.shell.status_msg = "Layer này không phải adjustment layer".to_string();
                return;
            };
            (layer.id, adj.clone())
        };

        self.edit.adjustment_layer_edit = Some(crate::app::state::AdjustmentLayerEditState {
            doc_id,
            layer_id,
            original_adj: adj.clone(),
        });
        self.shell.ui.adjustment_dialog = adj;
        self.shell.ui.adjustment_preview_enabled = true;
        self.shell.ui.adj_eyedropper = None;
        self.shell.ui.show_adjustment_dialog = true;
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Update an adjustment layer's params while dragging a slider (live, no undo yet).
    pub(super) fn set_adjustment_layer_type(
        &mut self,
        adj: crate::core::layer::AdjustmentType,
    ) -> bool {
        use crate::core::layer::LayerType;
        let Some(edit) = &self.edit.adjustment_layer_edit else {
            return false;
        };
        if self.docs.documents[self.docs.active_doc_idx].id != edit.doc_id {
            return false;
        }
        let layer_id = edit.layer_id;
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let Some(layer) = canvas
            .layer_stack
            .layers
            .iter_mut()
            .find(|l| l.id == layer_id)
        else {
            return false;
        };
        layer.layer_type = LayerType::Adjustment(adj);
        canvas.layer_revision += 1;
        true
    }

    pub(crate) fn update_adjustment_layer_edit(&mut self, adj: crate::core::layer::AdjustmentType) {
        if self.set_adjustment_layer_type(adj) {
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
    }

    pub(crate) fn commit_adjustment_layer_edit(
        &mut self,
        adj: &crate::core::layer::AdjustmentType,
    ) -> bool {
        use crate::core::layer::LayerType;
        let Some(edit) = self.edit.adjustment_layer_edit.take() else {
            return false;
        };
        if self.docs.documents[self.docs.active_doc_idx].id != edit.doc_id {
            return false;
        }
        if &edit.original_adj == adj {
            self.set_adjustment_layer_type(adj.clone());
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            return false;
        }

        let (cw, ch) = {
            let d = &self.docs.documents[self.docs.active_doc_idx];
            (d.canvas.width, d.canvas.height)
        };
        let layer_id = edit.layer_id;
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let Some(idx) = canvas
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == layer_id)
        else {
            return false;
        };
        canvas.layer_stack.layers[idx].layer_type =
            LayerType::Adjustment(edit.original_adj.clone());
        let mut cmd = crate::core::command::LayerStructureCommand::capture_before(
            adj.name(),
            &canvas.layer_stack,
            cw,
            ch,
        );
        canvas.layer_stack.layers[idx].layer_type = LayerType::Adjustment(adj.clone());
        cmd.capture_after(&canvas.layer_stack, cw, ch);
        canvas.record(Box::new(cmd));
        canvas.layer_revision += 1;

        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        true
    }

    pub(crate) fn cancel_adjustment_layer_edit(&mut self) {
        let Some(edit) = self.edit.adjustment_layer_edit.take() else {
            return;
        };
        if self.docs.documents[self.docs.active_doc_idx].id != edit.doc_id {
            return;
        }
        use crate::core::layer::LayerType;
        let layer_id = edit.layer_id;
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        if let Some(layer) = canvas
            .layer_stack
            .layers
            .iter_mut()
            .find(|l| l.id == layer_id)
        {
            layer.layer_type = LayerType::Adjustment(edit.original_adj);
            canvas.layer_revision += 1;
        }
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    pub(crate) fn set_adjustment_preview_enabled(&mut self, enabled: bool) {
        self.shell.ui.adjustment_preview_enabled = enabled;
        self.shell.adjustment_preview_pending = None;

        if enabled {
            let adj = self.shell.ui.adjustment_dialog.clone();
            if self.shell.adjustment_preview.is_some() {
                self.update_adjustment_preview(adj);
            } else if self.edit.adjustment_layer_edit.is_some() {
                self.update_adjustment_layer_edit(adj);
            }
            return;
        }

        if let Some(gpu) = &mut self.win.gpu {
            gpu.compositor.preview_adj = None;
        }

        let mut event = None;
        if let Some(preview) = &self.shell.adjustment_preview {
            if self.docs.documents[self.docs.active_doc_idx].id == preview.doc_id {
                if self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .restore_layer_tiles(preview.layer_id, preview.original_tiles.clone())
                {
                    event = Some(CanvasEvent::LayerPixelsChanged);
                }
            }
        } else if let Some(edit) = &self.edit.adjustment_layer_edit {
            if self.docs.documents[self.docs.active_doc_idx].id == edit.doc_id {
                use crate::core::layer::LayerType;
                let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
                if let Some(layer) = canvas
                    .layer_stack
                    .layers
                    .iter_mut()
                    .find(|l| l.id == edit.layer_id)
                {
                    layer.layer_type = LayerType::Adjustment(edit.original_adj.clone());
                    canvas.layer_revision += 1;
                    event = Some(CanvasEvent::LayerStructureChanged);
                }
            }
        }

        if let Some(event) = event {
            self.apply_canvas_event(event);
        } else {
            self.recomposite_visible();
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    pub(crate) fn apply_adjustment_eyedropper_at(
        &mut self,
        kind: crate::ui::AdjEyedropperKind,
        cx: f32,
        cy: f32,
    ) -> bool {
        let cw = self.docs.documents[self.docs.active_doc_idx].canvas.width as f32;
        let ch = self.docs.documents[self.docs.active_doc_idx].canvas.height as f32;
        if cx < 0.0 || cy < 0.0 || cx >= cw || cy >= ch {
            return false;
        }

        self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .ensure_pixels();
        let color = self.edit.tools.eyedropper().sample(
            &self.docs.documents[self.docs.active_doc_idx].canvas,
            cx as u32,
            cy as u32,
        );

        let mut adj = self.shell.ui.adjustment_dialog.clone();
        if !apply_adjustment_eyedropper_to_params(&mut adj, kind, color) {
            return false;
        }
        self.shell.ui.adjustment_dialog = adj.clone();
        self.shell.ui.adj_eyedropper = None;
        self.shell.adjustment_preview_pending = None;

        if self.shell.ui.adjustment_preview_enabled {
            if self.shell.adjustment_preview.is_some() {
                self.update_adjustment_preview(adj);
            } else if self.edit.adjustment_layer_edit.is_some() {
                self.update_adjustment_layer_edit(adj);
            }
        }
        self.shell.status_msg = match kind {
            crate::ui::AdjEyedropperKind::Black => "Set black point".to_string(),
            crate::ui::AdjEyedropperKind::Gray => "Set gray point".to_string(),
            crate::ui::AdjEyedropperKind::White => "Set white point".to_string(),
        };
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        true
    }

    pub(crate) fn cancel_adjustment_preview(&mut self) {
        // Drop the GPU preview overlay first so the recomposite below shows the
        // original (also covers the no-preview-state case).
        if let Some(gpu) = &mut self.win.gpu {
            gpu.compositor.preview_adj = None;
        }
        let Some(preview) = self.shell.adjustment_preview.take() else {
            return;
        };
        if self.docs.documents[self.docs.active_doc_idx].id != preview.doc_id {
            return;
        }

        let restored = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .restore_layer_tiles(preview.layer_id, preview.original_tiles);
        if restored {
            self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
    }
}

fn levels_histogram_from_rgba(pixels: &[u8]) -> std::sync::Arc<[[u32; 256]; 4]> {
    let mut histogram = [[0_u32; 256]; 4];
    for px in pixels.chunks_exact(4) {
        if px[3] == 0 {
            continue;
        }
        histogram[0][px[0] as usize] += 1;
        histogram[1][px[1] as usize] += 1;
        histogram[2][px[2] as usize] += 1;
        let luma = (px[0] as u32 * 77 + px[1] as u32 * 150 + px[2] as u32 * 29) >> 8;
        histogram[3][luma.min(255) as usize] += 1;
    }
    std::sync::Arc::new(histogram)
}

/// Ink-coverage histograms `[C, M, Y, K]` for the CMYK Levels/Curves dialogs,
/// counting only pixels visible in the mirror (alpha > 0).
fn levels_histogram_from_ink(
    tiles: &crate::core::tile::TileMap,
) -> std::sync::Arc<[[u32; 256]; 4]> {
    let mut histogram = [[0_u32; 256]; 4];
    for tile in tiles.tiles.values() {
        let Some(ink) = tile.ink.as_ref() else {
            continue;
        };
        for (px, ip) in tile.pixels.chunks_exact(4).zip(ink.chunks_exact(4)) {
            if px[3] == 0 {
                continue;
            }
            for c in 0..4 {
                histogram[c][ip[c] as usize] += 1;
            }
        }
    }
    std::sync::Arc::new(histogram)
}

fn apply_adjustment_eyedropper_to_params(
    adj: &mut crate::core::layer::AdjustmentType,
    kind: crate::ui::AdjEyedropperKind,
    color: [u8; 4],
) -> bool {
    match adj {
        crate::core::layer::AdjustmentType::Levels { channels } => {
            for ch in 0..3 {
                let params = &mut channels[ch + 1];
                match kind {
                    crate::ui::AdjEyedropperKind::Black => {
                        params.in_black = color[ch].min(254);
                        if params.in_white <= params.in_black {
                            params.in_white = params.in_black.saturating_add(1).max(1);
                        }
                    }
                    crate::ui::AdjEyedropperKind::White => {
                        params.in_white = color[ch].max(1);
                        if params.in_black >= params.in_white {
                            params.in_black = params.in_white.saturating_sub(1);
                        }
                    }
                    crate::ui::AdjEyedropperKind::Gray => {
                        let avg = ((color[0] as f32 + color[1] as f32 + color[2] as f32)
                            / (3.0 * 255.0))
                            .clamp(0.01, 0.99);
                        let v = (color[ch] as f32 / 255.0).clamp(0.01, 0.99);
                        params.gamma = (v.ln() / avg.ln()).clamp(0.10, 9.99);
                    }
                }
            }
            true
        }
        crate::core::layer::AdjustmentType::Curves { channels } => {
            channels[0] = crate::core::layer::identity_curve();
            let avg = ((color[0] as f32 + color[1] as f32 + color[2] as f32) / (3.0 * 255.0))
                .clamp(0.0, 1.0);
            for ch in 0..3 {
                let v = (color[ch] as f32 / 255.0).clamp(0.0, 1.0);
                channels[ch + 1] = match kind {
                    crate::ui::AdjEyedropperKind::Black => curve_points_with_middle(v, 0.0),
                    crate::ui::AdjEyedropperKind::White => curve_points_with_middle(v, 1.0),
                    crate::ui::AdjEyedropperKind::Gray => curve_points_with_middle(v, avg),
                };
            }
            true
        }
        _ => false,
    }
}

fn curve_points_with_middle(x: f32, y: f32) -> Vec<(f32, f32)> {
    let mut points = Vec::with_capacity(3);
    push_curve_point(&mut points, 0.0, 0.0);
    push_curve_point(&mut points, x, y);
    push_curve_point(&mut points, 1.0, 1.0);
    points
}

fn push_curve_point(points: &mut Vec<(f32, f32)>, x: f32, y: f32) {
    let x = x.clamp(0.0, 1.0);
    let y = y.clamp(0.0, 1.0);
    if let Some(last) = points.last_mut() {
        if (last.0 - x).abs() < 1e-6 {
            last.1 = y;
            return;
        }
    }
    points.push((x, y));
}

#[cfg(test)]
mod tests {
    use super::{apply_adjustment_eyedropper_to_params, levels_histogram_from_rgba};
    use crate::core::layer::{curve_is_identity, AdjustmentType, LevelsParams};
    use crate::ui::AdjEyedropperKind;

    #[test]
    fn levels_histogram_tracks_rgb_and_luma_planes() {
        let pixels = [
            10, 20, 30, 255, //
            200, 100, 50, 255, //
            255, 255, 255, 0,
        ];

        let hist = levels_histogram_from_rgba(&pixels);

        assert_eq!(hist[0][10], 1);
        assert_eq!(hist[0][200], 1);
        assert_eq!(hist[1][20], 1);
        assert_eq!(hist[1][100], 1);
        assert_eq!(hist[2][30], 1);
        assert_eq!(hist[2][50], 1);
        assert_eq!(hist[0][255], 0, "transparent pixels are ignored");

        let luma0 = ((10_u32 * 77 + 20_u32 * 150 + 30_u32 * 29) >> 8) as usize;
        let luma1 = ((200_u32 * 77 + 100_u32 * 150 + 50_u32 * 29) >> 8) as usize;
        assert_eq!(hist[3][luma0], 1);
        assert_eq!(hist[3][luma1], 1);
    }

    #[test]
    fn levels_eyedropper_sets_rgb_channel_points() {
        let mut adj = AdjustmentType::Levels {
            channels: [LevelsParams::default(); 4],
        };
        assert!(apply_adjustment_eyedropper_to_params(
            &mut adj,
            AdjEyedropperKind::Black,
            [12, 34, 56, 255],
        ));
        let AdjustmentType::Levels { channels } = adj else {
            unreachable!();
        };
        assert!(channels[0].is_identity());
        assert_eq!(channels[1].in_black, 12);
        assert_eq!(channels[2].in_black, 34);
        assert_eq!(channels[3].in_black, 56);
    }

    #[test]
    fn curves_gray_eyedropper_adds_rgb_midpoints() {
        let mut adj = AdjustmentType::default_curves();
        assert!(apply_adjustment_eyedropper_to_params(
            &mut adj,
            AdjEyedropperKind::Gray,
            [64, 128, 192, 255],
        ));
        let AdjustmentType::Curves { channels } = adj else {
            unreachable!();
        };
        assert!(curve_is_identity(&channels[0]));
        assert_eq!(channels[1][1].0, 64.0 / 255.0);
        assert_eq!(channels[2][1].0, 128.0 / 255.0);
        assert_eq!(channels[3][1].0, 192.0 / 255.0);
        let avg = (64.0 + 128.0 + 192.0) / (3.0 * 255.0);
        assert!((channels[1][1].1 - avg).abs() < 1e-6);
    }
}

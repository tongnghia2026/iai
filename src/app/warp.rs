//! Warp modal session (Filter ▸ Warp… / Ctrl+Shift+X).
//!
//! Modal like Free Transform: while `warp_state` is `Some`, canvas pointer
//! input warps the active layer through an accumulated displacement mesh
//! ([`crate::core::warp::WarpMesh`]) and the layer preview is rebuilt live.
//! Apply commits one undoable tiles change; Cancel restores the original layer.
//!
//! v1 path favours correctness over speed: each dab warps only the brush rect of
//! `working_flat` (cheap, local) but pushes the whole layer to the GPU via the
//! proven `preview_layer_tiles` path. A region-only upload / GPU warp shader is a
//! future optimisation for very large images.

use super::render::CanvasEvent;
use super::state::{App, WarpState};
use crate::core::tile::TileMap;
use crate::core::warp::{WarpMesh, WarpMode, DEFAULT_CELL};

impl App {
    /// Enter Warp on the active raster layer. Returns false (with no state
    /// change) when the layer is locked / non-raster / missing.
    pub(crate) fn begin_warp(&mut self) -> bool {
        // A live preview from another modal would fight ours — close them first.
        self.cancel_warp();
        self.abandon_develop_session();
        self.shell.ui.show_adjustment_dialog = false;
        self.cancel_adjustment_preview();
        self.shell.ui.show_filter_dialog = false;
        self.cancel_filter_preview();

        let doc_id = self.docs.documents[self.docs.active_doc_idx].id;
        let state = {
            let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
            canvas.layer_stack.normalize_active_idx();
            if canvas.layer_stack.layers.is_empty() {
                return false;
            }
            let idx = canvas.layer_stack.active_idx;
            let layer = &canvas.layer_stack.layers[idx];
            if (!layer.is_background && layer.locked) || !layer.is_raster() {
                return false;
            }
            let layer_w = layer.width as usize;
            let layer_h = layer.height as usize;
            if layer_w == 0 || layer_h == 0 {
                return false;
            }
            let original_tiles = layer.tiles.clone();
            let original_flat = original_tiles.flatten();
            WarpState {
                doc_id,
                layer_id: layer.id,
                layer_w,
                layer_h,
                layer_offset: layer.offset,
                working_flat: original_flat.clone(),
                original_flat,
                original_tiles,
                mesh: WarpMesh::new(layer_w, layer_h, DEFAULT_CELL),
                dragging: false,
                last_lx: 0.0,
                last_ly: 0.0,
            }
        };

        self.edit.warp_state = Some(state);
        self.edit.input.warp_resizing = false;
        self.shell.ui.show_warp_dialog = true;
        self.shell.status_msg = "Warp: drag to warp, Enter to apply, Esc to cancel".to_string();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        true
    }

    /// Index of the document this session is bound to (resolved by id, so a
    /// mid-session tab switch can't push the warp into the wrong document).
    fn warp_doc_idx(&self) -> Option<usize> {
        let id = self.edit.warp_state.as_ref()?.doc_id;
        self.docs.documents.iter().position(|d| d.id == id)
    }

    /// Current pointer position in layer-local pixels.
    fn warp_layer_pos(&self) -> Option<(f32, f32)> {
        let state = self.edit.warp_state.as_ref()?;
        let ev = self.tool_event();
        Some((
            ev.canvas_x - state.layer_offset.0 as f32,
            ev.canvas_y - state.layer_offset.1 as f32,
        ))
    }

    pub(crate) fn warp_pointer_down(&mut self) {
        let Some((lx, ly)) = self.warp_layer_pos() else {
            return;
        };
        if let Some(state) = self.edit.warp_state.as_mut() {
            state.dragging = true;
            state.last_lx = lx;
            state.last_ly = ly;
        }
        // Radial / rotational / mask brushes act on a stationary press; Forward Warp
        // and Push Left need a drag delta, so a bare click is a no-op for them.
        if self.shell.ui.warp_params.mode.acts_on_press() {
            self.warp_apply_dab(lx, ly, 0.0, 0.0);
        }
    }

    pub(crate) fn warp_pointer_drag(&mut self) {
        let dragging = self
            .edit
            .warp_state
            .as_ref()
            .map(|s| s.dragging)
            .unwrap_or(false);
        if !dragging {
            return;
        }
        let Some((lx, ly)) = self.warp_layer_pos() else {
            return;
        };
        let (last_lx, last_ly) = {
            let s = self.edit.warp_state.as_ref().unwrap();
            (s.last_lx, s.last_ly)
        };
        let mvx = lx - last_lx;
        let mvy = ly - last_ly;
        if let Some(s) = self.edit.warp_state.as_mut() {
            s.last_lx = lx;
            s.last_ly = ly;
        }
        self.warp_apply_dab(lx, ly, mvx, mvy);
    }

    pub(crate) fn warp_pointer_up(&mut self) {
        if let Some(state) = self.edit.warp_state.as_mut() {
            state.dragging = false;
        }
    }

    /// Apply one brush dab at layer pixel `(lx, ly)` with pointer delta `(mvx, mvy)`,
    /// then push the re-warped rect to the live preview.
    fn warp_apply_dab(&mut self, lx: f32, ly: f32, mvx: f32, mvy: f32) {
        let params = self.shell.ui.warp_params;
        let radius = (params.size * 0.5).max(1.0);
        let p = params.pressure;
        let rect = {
            let Some(state) = self.edit.warp_state.as_mut() else {
                return;
            };
            match params.mode {
                WarpMode::ForwardWarp => state.mesh.forward_warp(lx, ly, mvx, mvy, radius, p),
                WarpMode::PushLeft => state.mesh.push_left(lx, ly, mvx, mvy, radius, p),
                WarpMode::Pucker => state.mesh.pucker(lx, ly, radius, p),
                WarpMode::Bloat => state.mesh.bloat(lx, ly, radius, p),
                WarpMode::Twirl => state.mesh.twirl(lx, ly, radius, p),
                WarpMode::Reconstruct => state.mesh.reconstruct(lx, ly, radius, p),
                WarpMode::Freeze => state.mesh.paint_freeze(lx, ly, radius, p, false),
                WarpMode::Thaw => state.mesh.paint_freeze(lx, ly, radius, p, true),
            }
            if params.mode.is_mask() {
                // Mask brushes change no pixels; the red overlay refreshes on redraw.
                None
            } else {
                let (rx, ry, rw, rh) = state.mesh.dab_rect(lx, ly, radius);
                if rw == 0 || rh == 0 {
                    None
                } else {
                    state.mesh.warp_region_into(
                        &state.original_flat,
                        &mut state.working_flat,
                        rx,
                        ry,
                        rw,
                        rh,
                    );
                    Some((rx, ry, rw, rh))
                }
            }
        };
        match rect {
            Some((rx, ry, rw, rh)) => self.warp_push_region(rx, ry, rw, rh),
            None => {
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
        }
    }

    /// Optimised live preview: write only the warped dab rect into the layer's live
    /// tiles (bumps just those tiles' revisions → partial atlas upload) and
    /// recomposite only the matching screen rect. No full-layer rebuild per dab.
    fn warp_push_region(&mut self, rx: u32, ry: u32, rw: u32, rh: u32) {
        let Some(doc_idx) = self.warp_doc_idx() else {
            return;
        };
        let (layer_id, ox, oy, tight) = {
            let Some(s) = self.edit.warp_state.as_ref() else {
                return;
            };
            let lw = s.layer_w;
            let rwb = (rw * 4) as usize;
            let mut tight = vec![0u8; rwb * rh as usize];
            for row in 0..rh as usize {
                let src = ((ry as usize + row) * lw + rx as usize) * 4;
                tight[row * rwb..(row + 1) * rwb].copy_from_slice(&s.working_flat[src..src + rwb]);
            }
            (s.layer_id, s.layer_offset.0, s.layer_offset.1, tight)
        };

        let canvas = &mut self.docs.documents[doc_idx].canvas;
        let Some(idx) = canvas
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == layer_id)
        else {
            return;
        };
        canvas.layer_stack.layers[idx]
            .tiles
            .write_region(rx, ry, rw, rh, &tight);
        canvas.layer_revision += 1;
        canvas.pixels_stale = true;

        // Recomposite just the affected canvas rect (canvas-space = layer-local + offset).
        let cx0 = (rx as i32 + ox).max(0) as f32;
        let cy0 = (ry as i32 + oy).max(0) as f32;
        self.warp_recomposite_canvas_rect(cx0, cy0, rw as f32, rh as f32);
    }

    /// Partial-recomposite the affected rect. The compositor has two dirty-rect
    /// coordinate spaces: Mode A uses canvas pixels, Mode B uses physical screen
    /// pixels. Keep the rect in the mode's native space so live Warp previews
    /// refresh the dab region instead of a shifted/clamped area.
    fn warp_recomposite_canvas_rect(&mut self, cx0: f32, cy0: f32, cw: f32, ch: f32) {
        self.sync_compositor_viewport();
        let canvas_space = self
            .win
            .gpu
            .as_ref()
            .map_or(false, |g| g.compositor.canvas_space);

        let rect = if canvas_space {
            let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
            let x0 = (cx0.floor() - 1.0).clamp(0.0, canvas.width as f32);
            let y0 = (cy0.floor() - 1.0).clamp(0.0, canvas.height as f32);
            let x1 = ((cx0 + cw).ceil() + 1.0).clamp(0.0, canvas.width as f32);
            let y1 = ((cy0 + ch).ceil() + 1.0).clamp(0.0, canvas.height as f32);
            if x1 <= x0 || y1 <= y0 {
                return;
            }
            (x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32)
        } else {
            let Some(win) = self.win.window.as_ref() else {
                return;
            };
            let sz = win.inner_size();
            let zoom = self.edit.view.zoom;
            let sx0 = ((cx0 * zoom + self.edit.view.offset_x).floor() - 1.0).max(0.0);
            let sy0 = ((cy0 * zoom + self.edit.view.offset_y).floor() - 1.0).max(0.0);
            let sx1 =
                (((cx0 + cw) * zoom + self.edit.view.offset_x).ceil() + 1.0).min(sz.width as f32);
            let sy1 =
                (((cy0 + ch) * zoom + self.edit.view.offset_y).ceil() + 1.0).min(sz.height as f32);
            if sx1 <= sx0 || sy1 <= sy0 {
                return;
            }
            (
                sx0 as u32,
                sy0 as u32,
                (sx1 - sx0) as u32,
                (sy1 - sy0) as u32,
            )
        };
        self.recomposite_with_dirty(Some(rect));
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Full-layer preview rebuild (used by Restore All, where the whole layer reverts).
    fn warp_push_preview(&mut self) {
        let Some(doc_idx) = self.warp_doc_idx() else {
            return;
        };
        let (layer_id, tiles) = {
            let Some(s) = self.edit.warp_state.as_ref() else {
                return;
            };
            (
                s.layer_id,
                TileMap::from_rgba(&s.working_flat, s.layer_w as u32, s.layer_h as u32),
            )
        };
        self.docs.documents[doc_idx]
            .canvas
            .preview_layer_tiles(layer_id, tiles);
        self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
    }

    /// Apply the warp: commit one undoable tiles change (original → warped).
    pub(crate) fn commit_warp(&mut self) {
        let Some(state) = self.edit.warp_state.take() else {
            return;
        };
        self.shell.ui.show_warp_dialog = false;
        let Some(doc_idx) = self
            .docs
            .documents
            .iter()
            .position(|d| d.id == state.doc_id)
        else {
            return;
        };
        if !state.mesh.touched {
            // Nothing was warped — restore the untouched layer, no history entry.
            self.docs.documents[doc_idx]
                .canvas
                .restore_layer_tiles(state.layer_id, state.original_tiles);
            self.shell.status_msg = "No Warp changes applied".to_string();
            self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
            return;
        }
        let after = TileMap::from_rgba(
            &state.working_flat,
            state.layer_w as u32,
            state.layer_h as u32,
        );
        let committed = self.docs.documents[doc_idx]
            .canvas
            .commit_layer_tiles_change(state.layer_id, state.original_tiles, after, "Warp");
        if committed {
            self.shell.status_msg = "Applied Warp".to_string();
        }
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
    }

    /// Discard the warp and restore the original layer.
    pub(crate) fn cancel_warp(&mut self) {
        let Some(state) = self.edit.warp_state.take() else {
            return;
        };
        self.shell.ui.show_warp_dialog = false;
        if let Some(doc_idx) = self
            .docs
            .documents
            .iter()
            .position(|d| d.id == state.doc_id)
        {
            self.docs.documents[doc_idx]
                .canvas
                .restore_layer_tiles(state.layer_id, state.original_tiles);
            self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
        }
    }

    /// Restore All: reset the mesh to identity and refresh the preview.
    pub(crate) fn warp_restore_all(&mut self) {
        let Some(state) = self.edit.warp_state.as_mut() else {
            return;
        };
        state.mesh.clear();
        state.working_flat.copy_from_slice(&state.original_flat);
        self.warp_push_preview();
    }
}

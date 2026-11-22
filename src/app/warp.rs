//! Warp modal session (Filter ▸ Liquify… / Ctrl+Shift+X).
//!
//! Modal like Free Transform: while `warp_state` is `Some`, canvas pointer
//! input warps the active layer through an accumulated displacement mesh
//! ([`crate::core::warp::WarpMesh`]) and the layer preview is rebuilt live.
//! Apply commits one undoable tiles change; Cancel restores the original layer.
//!
//! A dab only moves the mesh and notes the part of the layer it moved; once a
//! frame that part is drawn again into `working_flat` (a draft as fine as the
//! zoom shows while the pointer is down, finely when it lets go) and only its
//! tiles go to the GPU (16/11: drawing and uploading at every mouse move
//! lagged, the more so with a big brush).

use super::render::CanvasEvent;
use super::state::{App, WarpState};
use crate::core::tile::TileMap;
use crate::core::warp::{DEFAULT_CELL, WarpMesh, WarpMode};

/// How many warp strokes Ctrl+Z can step back inside the modal. Each snapshot is
/// the full displacement mesh (a few MB on a large layer), so the depth is
/// bounded; the whole stack is freed the moment the session applies or cancels.
const WARP_UNDO_DEPTH: usize = 16;

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

        let doc_id = self.docs.active().id;
        let state = {
            let canvas = &mut self.docs.active_mut().canvas;
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
                undo_stack: std::collections::VecDeque::new(),
                stroke_snapshot: None,
                stroke_dirty: false,
                pending: None,
                rough: None,
                freeze_view: std::cell::RefCell::new(None),
            }
        };

        self.edit.warp_state = Some(state);
        self.edit.input.warp_resizing = false;
        self.shell.ui.show_warp_dialog = true;
        self.shell.status_msg = "Liquify: drag to warp, Enter to apply, Esc to cancel".to_string();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        true
    }

    /// The biggest brush for the layer being warped.
    pub(crate) fn warp_size_max(&self) -> f32 {
        self.edit
            .warp_state
            .as_ref()
            .map_or(crate::core::warp::BRUSH_MAX_FLOOR, |s| {
                crate::core::warp::brush_max(s.layer_w, s.layer_h)
            })
    }

    /// Index of the document this session is bound to (resolved by id, so a
    /// mid-session tab switch can't push the warp into the wrong document).
    fn warp_doc_idx(&self) -> Option<usize> {
        let id = self.edit.warp_state.as_ref()?.doc_id;
        self.docs.index_of(id)
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
            // Snapshot the pre-stroke mesh so Ctrl+Z can step this stroke back.
            // Promoted to the undo stack on pointer-up only if a dab changed it.
            state.stroke_snapshot = Some(state.mesh.clone());
            state.stroke_dirty = false;
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
            // What the stroke drew as a draft is drawn finely next frame.
            if let Some(rough) = state.rough.take() {
                state.pending = Some(super::grid_warp::union(
                    state.pending.unwrap_or((0, 0, 0, 0)),
                    rough,
                ));
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            // Commit the stroke's pre-image to the undo stack, but only if the
            // stroke actually warped something — a bare click leaves no step.
            if let Some(snapshot) = state.stroke_snapshot.take()
                && state.stroke_dirty
            {
                if state.undo_stack.len() >= WARP_UNDO_DEPTH {
                    state.undo_stack.pop_front();
                }
                state.undo_stack.push_back(snapshot);
            }
            state.stroke_dirty = false;
        }
    }

    /// Ctrl+Z inside the Warp modal: step back one stroke (like Photoshop
    /// Liquify) instead of ringing the modal-lock bell. Returns true when the
    /// warp session consumed the undo (always, while a session is open).
    pub(crate) fn warp_undo_stroke(&mut self) -> bool {
        let Some(state) = self.edit.warp_state.as_mut() else {
            return false;
        };
        let Some(prev_mesh) = state.undo_stack.pop_back() else {
            self.shell.status_msg = "Liquify: nothing to undo".to_string();
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return true;
        };
        // Restore the mesh and rebuild the whole working buffer from the
        // untouched original through it, then refresh the live preview.
        state.mesh = prev_mesh;
        state.stroke_snapshot = None;
        state.stroke_dirty = false;
        state.pending = None;
        state.rough = None;
        let (lw, lh) = (state.layer_w, state.layer_h);
        state.mesh.warp_region_into(
            &state.original_flat,
            &mut state.working_flat,
            0,
            0,
            lw as u32,
            lh as u32,
        );
        self.warp_push_preview();
        self.shell.status_msg = "Liquify: undid one stroke".to_string();
        true
    }

    /// Apply one brush dab at layer pixel `(lx, ly)` with pointer delta `(mvx, mvy)`,
    /// then push the re-warped rect to the live preview.
    fn warp_apply_dab(&mut self, lx: f32, ly: f32, mvx: f32, mvy: f32) {
        let params = self.shell.ui.warp_params;
        let radius = (params.size * 0.5).max(1.0);
        let p = params.pressure;
        {
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
                // Mask brushes change no pixels but do mutate the freeze field —
                // count them as a stroke so Ctrl+Z can undo a freeze/thaw pass.
                state.stroke_dirty = true;
                // Mask brushes change no pixels; the red overlay refreshes on redraw.
            } else {
                let (rx, ry, rw, rh) = state.mesh.dab_rect(lx, ly, radius);
                if rw > 0 && rh > 0 {
                    state.stroke_dirty = true;
                    // Drawn once this frame with the other dabs (`warp_flush`).
                    state.pending = Some(super::grid_warp::union(
                        state.pending.unwrap_or((0, 0, 0, 0)),
                        (rx, ry, rx + rw, ry + rh),
                    ));
                }
            }
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Draw the part of the layer the dabs since the last frame moved, once
    /// a frame: a draft while the stroke goes on, finely after.
    pub(crate) fn warp_flush(&mut self) {
        let step = crate::core::grid_warp::draft_step(self.edit.view.zoom);
        let Some(state) = self.edit.warp_state.as_mut() else {
            return;
        };
        let Some((x0, y0, x1, y1)) = state.pending.take() else {
            return;
        };
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        if state.dragging {
            state.mesh.warp_region_draft_into(
                &state.original_flat,
                &mut state.working_flat,
                x0,
                y0,
                x1 - x0,
                y1 - y0,
                step,
            );
            state.rough = Some(super::grid_warp::union(
                state.rough.unwrap_or((0, 0, 0, 0)),
                (x0, y0, x1, y1),
            ));
        } else {
            state.mesh.warp_region_into(
                &state.original_flat,
                &mut state.working_flat,
                x0,
                y0,
                x1 - x0,
                y1 - y0,
            );
        }
        self.warp_push_region(x0, y0, x1 - x0, y1 - y0);
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
    pub(in crate::app) fn warp_recomposite_canvas_rect(
        &mut self,
        cx0: f32,
        cy0: f32,
        cw: f32,
        ch: f32,
    ) {
        self.sync_compositor_viewport();
        let canvas_space = self
            .win
            .gpu
            .as_ref()
            .is_some_and(|g| g.compositor.canvas_space);

        let rect = if canvas_space {
            let canvas = &self.docs.active().canvas;
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
        let Some(mut state) = self.edit.warp_state.take() else {
            return;
        };
        // What is still a draft, or not drawn yet, is drawn finely first.
        let left = [state.pending.take(), state.rough.take()]
            .into_iter()
            .flatten()
            .fold((0, 0, 0, 0), super::grid_warp::union);
        if left.2 > left.0 && left.3 > left.1 {
            state.mesh.warp_region_into(
                &state.original_flat,
                &mut state.working_flat,
                left.0,
                left.1,
                left.2 - left.0,
                left.3 - left.1,
            );
        }
        self.shell.ui.show_warp_dialog = false;
        let Some(doc_idx) = self.docs.index_of(state.doc_id) else {
            return;
        };
        if !state.mesh.touched {
            // Nothing was warped — restore the untouched layer, no history entry.
            self.docs.documents[doc_idx]
                .canvas
                .restore_layer_tiles(state.layer_id, state.original_tiles);
            self.shell.status_msg = "No Liquify changes applied".to_string();
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
            .commit_layer_tiles_change(state.layer_id, state.original_tiles, after, "Liquify");
        if committed {
            self.shell.status_msg = "Applied Liquify".to_string();
        }
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
    }

    /// Discard the warp and restore the original layer.
    pub(crate) fn cancel_warp(&mut self) {
        let Some(state) = self.edit.warp_state.take() else {
            return;
        };
        self.shell.ui.show_warp_dialog = false;
        if let Some(doc_idx) = self.docs.index_of(state.doc_id) {
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
        // Make Restore All a single undoable step: snapshot the field first, so
        // Ctrl+Z brings the whole warp back rather than losing it silently.
        if state.mesh.touched || state.mesh.any_frozen {
            if state.undo_stack.len() >= WARP_UNDO_DEPTH {
                state.undo_stack.pop_front();
            }
            state.undo_stack.push_back(state.mesh.clone());
        }
        state.mesh.clear();
        state.pending = None;
        state.rough = None;
        state.working_flat.copy_from_slice(&state.original_flat);
        self.warp_push_preview();
    }
}

#[cfg(test)]
mod tests {
    use crate::app::state::App;

    /// 16/11: dabs only note what they moved; it is drawn once a frame, a
    /// draft while the stroke goes on and finely when it ends, and the
    /// layer then holds exactly the fine warp.
    #[test]
    fn a_stroke_drawn_once_a_frame_ends_as_the_fine_warp() {
        // Zoomed out (a sparse draft) and at 100 % (a draft of every pixel).
        for zoom in [0.2, 1.0] {
            stroke_at_zoom(zoom);
        }
    }

    fn stroke_at_zoom(zoom: f32) {
        let mut app: App = crate::app::grid_warp::tests::patterned_app();
        app.edit.view.zoom = zoom;
        assert!(app.begin_warp());
        app.shell.ui.warp_params.size = 120.0;
        let (x0, y0) = (300.0 + 200.0 * zoom, 300.0 + 150.0 * zoom);
        app.edit.input.mouse_x = x0;
        app.edit.input.mouse_y = y0;
        app.warp_pointer_down();
        for k in 1..=6 {
            app.edit.input.mouse_x = x0 + 3.0 * k as f32;
            app.edit.input.mouse_y = y0 + 1.0 * k as f32;
            app.warp_pointer_drag();
            if k % 2 == 0 {
                app.warp_flush();
            }
        }
        app.warp_flush();
        assert!(app.edit.warp_state.as_ref().expect("open").rough.is_some());
        app.warp_pointer_up();
        app.warp_flush();
        let state = app.edit.warp_state.as_ref().expect("open");
        assert!(state.pending.is_none() && state.rough.is_none());
        let mut fine = state.original_flat.clone();
        state.mesh.warp_region_into(
            &state.original_flat,
            &mut fine,
            0,
            0,
            state.layer_w as u32,
            state.layer_h as u32,
        );
        assert!(state.mesh.touched);
        let off: Vec<usize> = (0..fine.len() / 4)
            .filter(|&i| state.working_flat[i * 4..i * 4 + 4] != fine[i * 4..i * 4 + 4])
            .collect();
        let w = state.layer_w;
        assert!(
            off.is_empty(),
            "zoom {zoom}: {} pixels differ, first at {:?}, last at {:?}",
            off.len(),
            off.first().map(|i| (i % w, i / w)),
            off.last().map(|i| (i % w, i / w))
        );
        let canvas = &app.docs.active().canvas;
        let layer = &canvas.layer_stack.layers[canvas.layer_stack.active_idx];
        assert!(
            layer.tiles.flatten() == fine,
            "the layer shows the fine warp"
        );
        app.commit_warp();
        assert!(app.edit.warp_state.is_none());
    }
}

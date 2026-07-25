//! Cached screen-resolution display raster for the top visible vector run.

use crate::core::layer::{BlendMode, LayerType};
use crate::core::shape::ShapeData;
use crate::core::vector::object::{VectorGeometry, VectorObjectData};
use crate::tools::ToolId;

use super::state::{PathDisplayCacheEntry, PathDisplayCacheKey, PathDisplayObjectKey};
use super::App;

impl App {
    /// Drop every screen-resolution derivative before committing a new vector
    /// appearance. An in-flight worker owns a snapshot of the old model, while
    /// `path_display_suppressed_layers` may still be hiding the document raster
    /// underneath that old overlay. Clearing both before the final cache rebuild
    /// guarantees the compositor presents the new document tiles first; a fresh
    /// crisp overlay is requested from the committed model on the next frame.
    pub(in crate::app) fn invalidate_vector_display(&mut self) {
        self.jobs.display_bake = None;
        self.jobs.display_bake_next = None;
        self.shell.ui_data_cache.path_display = None;
        self.shell.ui_data_cache.path_display_suppressed_layers = None;
        if let Some(gpu) = &mut self.win.gpu {
            gpu.compositor.ping_initialized = false;
        }
        let now = std::time::Instant::now();
        self.win.egui_repaint_deadline = Some(
            self.win
                .egui_repaint_deadline
                .map_or(now, |due| due.min(now)),
        );
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }

    pub(in crate::app) fn active_path_display(&mut self) -> Option<crate::ui::PathDisplayRaster> {
        let display = self.active_vector_display_inner();
        let suppressed = display.as_ref().and_then(|_| {
            self.shell.ui_data_cache.path_display.as_ref().map(|entry| {
                (
                    entry.key.doc_id,
                    entry
                        .key
                        .objects
                        .iter()
                        .map(|object| object.layer_id)
                        .collect(),
                )
            })
        });
        if self.shell.ui_data_cache.path_display_suppressed_layers != suppressed {
            self.shell.ui_data_cache.path_display_suppressed_layers = suppressed;
            // The coarse atlas copy must enter/leave the composite in lock-step
            // with the supersampled overlay. Otherwise its larger pixels protrude
            // around the smooth edge (or the object stays missing after switching
            // tools/layers).
            self.recomposite();
        }
        display
    }

    fn active_vector_display_inner(&mut self) -> Option<crate::ui::PathDisplayRaster> {
        let tool = self.edit.tools.active_id();
        if self.edit.transform_state.is_some()
            || self.edit.path_transform.is_some()
            || self.edit.path_gradient_drag.is_some()
        {
            return None;
        }
        // A live Move drag is previewed through `Layer::offset` in the document
        // composite. The supersampled overlay is baked asynchronously from the
        // vector model, so keeping it active here can briefly put an older bake
        // on top of the current drag position (most visible above 100% zoom).
        // Let the composite own the preview until release, then rebuild the
        // crisp overlay from the committed transform.
        if live_move_uses_composite_preview(tool, self.edit.input.painting) {
            return None;
        }
        let scale = crate::core::vector::display::zoom_bucket(self.edit.view.zoom)?;
        let visible = self.visible_canvas_rect()?;
        let doc = &self.docs.documents[self.docs.active_doc_idx];
        let stack = &doc.canvas.layer_stack;
        let mut objects = Vec::new();
        // Only the uninterrupted top run can be overlaid without changing
        // z-order relative to raster/text/adjustment content.
        for (index, layer) in stack.layers.iter().enumerate().rev() {
            if !stack.is_effectively_visible(index) || layer.is_group() {
                continue;
            }
            let object = match &layer.layer_type {
                LayerType::Vector(VectorGeometry::Path(object)) => object.clone(),
                LayerType::Vector(VectorGeometry::Primitive(shape)) => {
                    shape_display_object(shape, layer.offset)
                }
                _ => break,
            };
            let mut parent_id = layer.parent_id;
            let mut ancestors_are_plain = true;
            while let Some(id) = parent_id {
                let Some(parent) = stack.layers.iter().find(|candidate| candidate.id == id) else {
                    ancestors_are_plain = false;
                    break;
                };
                if !parent.visible
                    || parent.mask.is_some()
                    || (parent.opacity - 1.0).abs() > 1e-3
                    || parent.blend_mode != BlendMode::Normal
                {
                    ancestors_are_plain = false;
                    break;
                }
                parent_id = parent.parent_id;
            }
            if layer.mask.is_some()
                || (layer.opacity - 1.0).abs() > 1e-3
                || layer.blend_mode != BlendMode::Normal
                || (object.style.opacity - 1.0).abs() > 1e-3
                || !ancestors_are_plain
            {
                break;
            }
            objects.push(PathDisplayObjectKey {
                layer_id: layer.id,
                layer_offset: layer.offset,
                object,
            });
        }
        objects.reverse();
        if objects.is_empty() {
            return None;
        }
        // Bake a padded, screen-sized canvas region. The padding is quantized
        // to about 256 screen pixels, so normal panning reuses the same raster
        // instead of launching a worker on every pointer move.
        let chunk = (256u32 / scale as u32).max(1);
        let x0 = visible.0.saturating_sub(chunk) / chunk * chunk;
        let y0 = visible.1.saturating_sub(chunk) / chunk * chunk;
        let x1 = visible
            .0
            .saturating_add(visible.2)
            .saturating_add(chunk)
            .div_ceil(chunk)
            .saturating_mul(chunk)
            .min(doc.canvas.width);
        let y1 = visible
            .1
            .saturating_add(visible.3)
            .saturating_add(chunk)
            .div_ceil(chunk)
            .saturating_mul(chunk)
            .min(doc.canvas.height);
        let key = PathDisplayCacheKey {
            doc_id: doc.id.0,
            scale,
            clip: (x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0)),
            objects,
        };
        // Cache hit: the crisp raster for this exact (object, scale, offset) is
        // ready.
        if self
            .shell
            .ui_data_cache
            .path_display
            .as_ref()
            .is_some_and(|entry| entry.key == key)
        {
            return self
                .shell
                .ui_data_cache
                .path_display
                .as_ref()
                .map(|entry| entry.display.clone());
        }
        // Cache miss. NEVER rasterize on the UI thread here — a big path at a high
        // zoom bucket is what made zooming lag. Skip entirely during a node drag
        // (the tile preview covers the edit); otherwise kick an OFF-THREAD bake.
        if self.edit.node_drag.is_some()
            || (tool == ToolId::Shape
                && (self.shape_drag_active() || self.shape_style_scrub_active()))
        {
            return None;
        }
        self.request_display_bake(key.clone());
        // While the bake runs: if ONLY the zoom bucket changed (same object,
        // offset, layer, doc) reuse the previous crisp raster scaled — smooth zoom.
        // If the geometry/style changed, show nothing (the document atlas) so an
        // edited path never briefly ghosts its old shape.
        if let Some(entry) = self.shell.ui_data_cache.path_display.as_ref() {
            if entry.key.doc_id == key.doc_id
                && entry.key.clip == key.clip
                && entry.key.objects == key.objects
            {
                return Some(entry.display.clone());
            }
        }
        None
    }

    /// Request an off-thread bake of the crisp display raster for `key`. Coalesces
    /// to the newest key while one runs (latest wins); a no-op if the running bake
    /// already targets this exact key.
    fn request_display_bake(&mut self, key: PathDisplayCacheKey) {
        match &self.jobs.display_bake {
            Some(job) if job.key == key => {}
            Some(_) => {
                self.jobs.display_bake_next = Some(key);
                self.arm_display_bake_poll();
            }
            None => self.spawn_display_bake(key),
        }
    }

    /// Kick a worker thread that rasterizes the display overlay for `key`. Picked
    /// up by `poll_display_bake` on a later frame.
    fn spawn_display_bake(&mut self, key: PathDisplayCacheKey) {
        let (tx, rx) = std::sync::mpsc::channel();
        let objects = key
            .objects
            .iter()
            .map(|entry| entry.object.clone())
            .collect::<Vec<_>>();
        let scale = key.scale;
        let clip = key.clip;
        std::thread::spawn(move || {
            let out = crate::core::vector::display::rasterize_stack_for_display_clipped(
                &objects,
                scale,
                crate::core::geometry::Rect::new(
                    clip.0 as f32,
                    clip.1 as f32,
                    clip.2 as f32,
                    clip.3 as f32,
                ),
            )
            .map(|r| {
                let inv = 1.0 / scale as f32;
                let tiles =
                    crate::core::vector::display::split_display_tiles(&r.rgba, r.width, r.height)
                        .into_iter()
                        .map(|tile| crate::ui::PathDisplayTile {
                            rgba: std::sync::Arc::new(tile.rgba),
                            x: tile.x,
                            y: tile.y,
                            width: tile.width,
                            height: tile.height,
                        })
                        .collect();
                // `rasterize_for_display` bakes the object transform, so offset
                // ÷ scale already IS the canvas top-left (do NOT add
                // layer.offset — that double-counts and ghosts a copy).
                crate::app::state::DisplayBakeOutput {
                    tiles,
                    canvas_x: r.offset.0 as f32 * inv,
                    canvas_y: r.offset.1 as f32 * inv,
                    canvas_w: r.width as f32 * inv,
                    canvas_h: r.height as f32 * inv,
                    raster_w: r.width,
                    raster_h: r.height,
                }
            });
            let _ = tx.send(out);
        });
        self.jobs.display_bake = Some(crate::app::state::DisplayBakeInFlight { key, rx });
        self.arm_display_bake_poll();
    }

    /// Guarantee an egui frame soon while a display bake is in flight so its result
    /// lands even when the pointer goes idle.
    fn arm_display_bake_poll(&mut self) {
        let due = std::time::Instant::now() + std::time::Duration::from_millis(16);
        self.win.egui_repaint_deadline =
            Some(self.win.egui_repaint_deadline.map_or(due, |d| d.min(due)));
    }

    /// Poll the in-flight display bake (once per UI frame): store a finished raster
    /// in the cache and start the newest queued key, if any.
    pub fn poll_display_bake(&mut self) {
        let received = match &self.jobs.display_bake {
            Some(job) => match job.rx.try_recv() {
                Ok(out) => out,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.arm_display_bake_poll();
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
            },
            None => return,
        };
        if let Some(out) = received {
            let key = self
                .jobs
                .display_bake
                .as_ref()
                .expect("checked above")
                .key
                .clone();
            self.shell.ui_data_cache.path_display_serial =
                self.shell.ui_data_cache.path_display_serial.wrapping_add(1);
            let display = crate::ui::PathDisplayRaster {
                cache_key: self.shell.ui_data_cache.path_display_serial,
                tiles: std::sync::Arc::new(out.tiles),
                canvas_x: out.canvas_x,
                canvas_y: out.canvas_y,
                canvas_w: out.canvas_w,
                canvas_h: out.canvas_h,
                raster_w: out.raster_w,
                raster_h: out.raster_h,
            };
            self.shell.ui_data_cache.path_display = Some(PathDisplayCacheEntry { key, display });
            // `poll_display_bake` runs after the current frame's UI data was
            // collected. A small Shape can finish inside that same frame, and
            // on Windows a lone `request_redraw` issued from RedrawRequested
            // may be coalesced away. Force one more event-loop frame so the
            // newly cached gradient/geometry is actually presented without
            // requiring another click or tool switch.
            let now = std::time::Instant::now();
            self.win.egui_repaint_deadline = Some(
                self.win
                    .egui_repaint_deadline
                    .map_or(now, |due| due.min(now)),
            );
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        self.jobs.display_bake = None;
        if let Some(next) = self.jobs.display_bake_next.take() {
            self.spawn_display_bake(next);
        }
    }
}

fn live_move_uses_composite_preview(tool: ToolId, painting: bool) -> bool {
    tool == ToolId::Move && painting
}

/// Adapt an editable Shape to the same transient vector object used by the
/// high-zoom Path overlay. The Shape model remains untouched; this is display
/// geometry only.
fn shape_display_object(shape: &ShapeData, layer_offset: (i32, i32)) -> VectorObjectData {
    shape.to_vector_object(layer_offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::shape::ShapeKind;

    #[test]
    fn completed_display_bake_forces_a_followup_frame() {
        let mut app = App::new();
        app.win.egui_repaint_deadline = None;
        let key = crate::app::state::PathDisplayCacheKey {
            doc_id: app.docs.documents[0].id.0,
            scale: 2,
            clip: (0, 0, 1, 1),
            objects: Vec::new(),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Some(crate::app::state::DisplayBakeOutput {
            tiles: Vec::new(),
            canvas_x: 0.0,
            canvas_y: 0.0,
            canvas_w: 1.0,
            canvas_h: 1.0,
            raster_w: 1,
            raster_h: 1,
        }))
        .unwrap();
        app.jobs.display_bake = Some(crate::app::state::DisplayBakeInFlight { key, rx });

        app.poll_display_bake();

        assert!(app.jobs.display_bake.is_none());
        assert!(app.shell.ui_data_cache.path_display.is_some());
        assert!(
            app.win
                .egui_repaint_deadline
                .is_some_and(|due| due <= std::time::Instant::now()),
            "a result landed after UI collection must force the next frame"
        );
    }

    #[test]
    fn vector_commit_drops_stale_display_and_suppression() {
        let mut app = App::new();
        app.shell.ui_data_cache.path_display_suppressed_layers =
            Some((app.docs.documents[0].id.0, vec![7]));
        app.jobs.display_bake_next = Some(crate::app::state::PathDisplayCacheKey {
            doc_id: app.docs.documents[0].id.0,
            scale: 2,
            clip: (0, 0, 1, 1),
            objects: Vec::new(),
        });
        app.win.egui_repaint_deadline = None;

        app.invalidate_vector_display();

        assert!(app.jobs.display_bake.is_none());
        assert!(app.jobs.display_bake_next.is_none());
        assert!(app.shell.ui_data_cache.path_display.is_none());
        assert!(app
            .shell
            .ui_data_cache
            .path_display_suppressed_layers
            .is_none());
        assert!(app
            .win
            .egui_repaint_deadline
            .is_some_and(|due| due <= std::time::Instant::now()));
    }

    #[test]
    fn live_move_suspends_async_vector_overlay_until_pointer_release() {
        assert!(live_move_uses_composite_preview(ToolId::Move, true));
        assert!(!live_move_uses_composite_preview(ToolId::Move, false));
        assert!(!live_move_uses_composite_preview(ToolId::Node, true));
    }

    #[test]
    fn star_display_adapter_keeps_canvas_position_and_points() {
        let (mut shape, offset) = ShapeData::from_canvas_span(
            ShapeKind::Star,
            100.0,
            80.0,
            180.0,
            160.0,
            0.0,
            true,
            [0, 0, 0, 255],
            2.0,
            [0, 0, 0, 255],
        );
        shape.sides = 7;
        shape.star_inner = 0.4;
        let object = shape_display_object(&shape, offset);
        assert_eq!(object.path.contours.len(), 1);
        assert_eq!(object.path.contours[0].nodes.len(), 14);
        let anchors = object.path.contours[0]
            .nodes
            .iter()
            .map(|node| object.transform.apply_point(node.anchor))
            .collect::<Vec<_>>();
        assert!(anchors
            .iter()
            .all(|p| { p.x >= 99.0 && p.y >= 79.0 && p.x <= 181.0 && p.y <= 161.0 }));
    }
}

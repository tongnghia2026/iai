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
        let active_layer_id = self
            .docs
            .documents
            .get(self.docs.active_doc_idx)
            .and_then(|doc| {
                doc.canvas
                    .layer_stack
                    .layers
                    .get(doc.canvas.layer_stack.active_idx)
            })
            .map(|layer| layer.id);
        if let Some(job) = &self.jobs.display_bake {
            job.cancel.store(true, std::sync::atomic::Ordering::Release);
        }
        self.jobs.display_bake = None;
        self.jobs.display_bake_next = None;
        match active_layer_id {
            Some(id) => self
                .shell
                .ui_data_cache
                .path_displays
                .retain(|entry| !entry.key.objects.iter().any(|object| object.layer_id == id)),
            None => self.shell.ui_data_cache.path_displays.clear(),
        }
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
            let first = self.shell.ui_data_cache.path_displays.first()?;
            Some((
                first.key.doc_id,
                self.shell
                    .ui_data_cache
                    .path_displays
                    .iter()
                    .flat_map(|entry| entry.key.objects.iter().map(|object| object.layer_id))
                    .collect(),
            ))
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
        let allow_active_gpu_vector = self.active_vector_gpu_idle();
        let doc = &self.docs.documents[self.docs.active_doc_idx];
        let stack = &doc.canvas.layer_stack;
        let mut objects = Vec::new();
        // Only the uninterrupted top run can be overlaid without changing
        // z-order relative to raster/text/adjustment content.
        for (index, layer) in stack.layers.iter().enumerate().rev() {
            if !stack.is_effectively_visible(index) || layer.is_group() {
                continue;
            }
            // Ask the compositor's current-frame policy directly. Using the ids
            // drawn by the previous frame is racy: this overlay suppresses its
            // raster twins, which can make that next frame's id set temporarily
            // empty and enqueue the entire Repeat stack for a fresh CPU bake.
            if self.win.gpu.as_ref().is_some_and(|gpu| {
                gpu.compositor.will_draw_vector_layer_on_gpu(
                    layer,
                    stack,
                    index,
                    stack.active_idx,
                    allow_active_gpu_vector,
                )
            }) {
                break;
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
        let clip = (x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0));
        let keys: Vec<_> = objects
            .into_iter()
            .map(|object| PathDisplayCacheKey {
                doc_id: doc.id.0,
                scale,
                clip,
                objects: vec![object],
            })
            .collect();
        self.shell
            .ui_data_cache
            .path_displays
            .retain(|entry| keys.iter().any(|key| *key == entry.key));
        // Cache miss. NEVER rasterize on the UI thread here — a big path at a high
        // zoom bucket is what made zooming lag. Skip entirely during a node drag
        // (the tile preview covers the edit); otherwise kick an OFF-THREAD bake.
        let allow_bake = self.edit.node_drag.is_none()
            && !(tool == ToolId::Shape
                && (self.shape_drag_active() || self.shape_style_scrub_active()));
        let missing: Vec<_> = keys
            .iter()
            .filter(|key| {
                !self
                    .shell
                    .ui_data_cache
                    .path_displays
                    .iter()
                    .any(|entry| entry.key == **key)
            })
            .cloned()
            .collect();
        if allow_bake && !missing.is_empty() {
            self.request_display_bake(missing);
        }
        // While the bake runs: if ONLY the zoom bucket changed (same object,
        // offset, layer, doc) reuse the previous crisp raster scaled — smooth zoom.
        // If the geometry/style changed, show nothing (the document atlas) so an
        // edited path never briefly ghosts its old shape.
        let ready: Vec<_> = keys
            .iter()
            .filter_map(|key| {
                self.shell
                    .ui_data_cache
                    .path_displays
                    .iter()
                    .find(|entry| entry.key == *key)
            })
            .collect();
        combine_displays(&ready, scale)
    }

    /// Request an off-thread bake of the crisp display raster for `key`. Coalesces
    /// to the newest key while one runs (latest wins); a no-op if the running bake
    /// already targets this exact key.
    fn request_display_bake(&mut self, keys: Vec<PathDisplayCacheKey>) {
        crate::gpu::vector::telemetry::request(self.jobs.display_bake.is_some());
        match &self.jobs.display_bake {
            Some(job) if job.keys == keys => {}
            Some(_) => {
                if let Some(job) = &self.jobs.display_bake {
                    job.cancel.store(true, std::sync::atomic::Ordering::Release);
                }
                self.jobs.display_bake_next = Some(keys);
                self.arm_display_bake_poll();
            }
            None => self.spawn_display_bake(keys),
        }
    }

    /// Kick a worker thread that rasterizes the display overlay for `key`. Picked
    /// up by `poll_display_bake` on a later frame.
    fn spawn_display_bake(&mut self, keys: Vec<PathDisplayCacheKey>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let Some(key) = keys.first().cloned() else {
            return;
        };
        let objects = key
            .objects
            .iter()
            .map(|entry| entry.object.clone())
            .collect::<Vec<_>>();
        let scale = key.scale;
        let clip = key.clip;
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_cancel = std::sync::Arc::clone(&cancel);
        std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let out =
                crate::core::vector::display::rasterize_stack_for_display_clipped_cancellable(
                    &objects,
                    scale,
                    crate::core::geometry::Rect::new(
                        clip.0 as f32,
                        clip.1 as f32,
                        clip.2 as f32,
                        clip.3 as f32,
                    ),
                    &worker_cancel,
                )
                .map(|r| {
                    let inv = 1.0 / scale as f32;
                    let tiles = crate::core::vector::display::split_display_tiles(
                        &r.rgba, r.width, r.height,
                    )
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
            let pixels = out
                .as_ref()
                .map_or(0, |r| u64::from(r.raster_w) * u64::from(r.raster_h));
            crate::gpu::vector::telemetry::complete(
                objects.len(),
                pixels,
                started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
            );
            let _ = tx.send(vec![(key, out)]);
        });
        self.jobs.display_bake = Some(crate::app::state::DisplayBakeInFlight { keys, cancel, rx });
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
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Vec::new(),
            },
            None => return,
        };
        for (key, out) in received {
            let Some(out) = out else {
                continue;
            };
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
            self.shell
                .ui_data_cache
                .path_displays
                .retain(|entry| entry.key != key);
            self.shell
                .ui_data_cache
                .path_displays
                .push(PathDisplayCacheEntry { key, display });
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

fn combine_displays(
    entries: &[&PathDisplayCacheEntry],
    scale: u8,
) -> Option<crate::ui::PathDisplayRaster> {
    let first = entries.first()?.display.clone();
    let factor = scale as f32;
    let origin = |display: &crate::ui::PathDisplayRaster| {
        (
            (display.canvas_x * factor).round() as i32,
            (display.canvas_y * factor).round() as i32,
        )
    };
    let (mut x0, mut y0) = origin(&first);
    let mut x1 = x0.saturating_add(first.raster_w as i32);
    let mut y1 = y0.saturating_add(first.raster_h as i32);
    for entry in &entries[1..] {
        let (x, y) = origin(&entry.display);
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x.saturating_add(entry.display.raster_w as i32));
        y1 = y1.max(y.saturating_add(entry.display.raster_h as i32));
    }
    let mut tiles = Vec::new();
    let mut cache_key = 0xcbf2_9ce4_8422_2325u64;
    for entry in entries {
        cache_key ^= entry.display.cache_key;
        cache_key = cache_key.wrapping_mul(0x100_0000_01b3);
        let (x, y) = origin(&entry.display);
        let dx = x.saturating_sub(x0) as u32;
        let dy = y.saturating_sub(y0) as u32;
        tiles.extend(entry.display.tiles.iter().cloned().map(|mut tile| {
            tile.x = tile.x.saturating_add(dx);
            tile.y = tile.y.saturating_add(dy);
            tile
        }));
    }
    let raster_w = x1.saturating_sub(x0) as u32;
    let raster_h = y1.saturating_sub(y0) as u32;
    Some(crate::ui::PathDisplayRaster {
        cache_key,
        tiles: std::sync::Arc::new(tiles),
        canvas_x: x0 as f32 / factor,
        canvas_y: y0 as f32 / factor,
        canvas_w: raster_w as f32 / factor,
        canvas_h: raster_h as f32 / factor,
        raster_w,
        raster_h,
    })
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
        tx.send(vec![(
            key.clone(),
            Some(crate::app::state::DisplayBakeOutput {
                tiles: Vec::new(),
                canvas_x: 0.0,
                canvas_y: 0.0,
                canvas_w: 1.0,
                canvas_h: 1.0,
                raster_w: 1,
                raster_h: 1,
            }),
        )])
        .unwrap();
        app.jobs.display_bake = Some(crate::app::state::DisplayBakeInFlight {
            keys: vec![key],
            cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            rx,
        });

        app.poll_display_bake();

        assert!(app.jobs.display_bake.is_none());
        assert_eq!(app.shell.ui_data_cache.path_displays.len(), 1);
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
        app.jobs.display_bake_next = Some(vec![crate::app::state::PathDisplayCacheKey {
            doc_id: app.docs.documents[0].id.0,
            scale: 2,
            clip: (0, 0, 1, 1),
            objects: Vec::new(),
        }]);
        app.win.egui_repaint_deadline = None;

        app.invalidate_vector_display();

        assert!(app.jobs.display_bake.is_none());
        assert!(app.jobs.display_bake_next.is_none());
        assert!(app.shell.ui_data_cache.path_displays.is_empty());
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
    fn vector_commit_keeps_other_layers_display_cache() {
        let mut app = App::new();
        let active_id = app.docs.documents[0].canvas.layer_stack.layers
            [app.docs.documents[0].canvas.layer_stack.active_idx]
            .id;
        let doc_id = app.docs.documents[0].id.0;
        let entry = |layer_id| PathDisplayCacheEntry {
            key: PathDisplayCacheKey {
                doc_id,
                scale: 4,
                clip: (0, 0, 16, 16),
                objects: vec![PathDisplayObjectKey {
                    layer_id,
                    layer_offset: (0, 0),
                    object: VectorObjectData::default(),
                }],
            },
            display: crate::ui::PathDisplayRaster {
                cache_key: layer_id as u64,
                tiles: std::sync::Arc::new(Vec::new()),
                canvas_x: 0.0,
                canvas_y: 0.0,
                canvas_w: 1.0,
                canvas_h: 1.0,
                raster_w: 4,
                raster_h: 4,
            },
        };
        app.shell.ui_data_cache.path_displays = vec![entry(active_id), entry(active_id + 99)];

        app.invalidate_vector_display();

        assert_eq!(app.shell.ui_data_cache.path_displays.len(), 1);
        assert_eq!(
            app.shell.ui_data_cache.path_displays[0].key.objects[0].layer_id,
            active_id + 99
        );
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

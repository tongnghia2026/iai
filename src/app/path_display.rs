//! Cached screen-resolution display raster for the active editable vector object.

use crate::core::layer::{BlendMode, LayerType};
use crate::core::shape::{ShapeData, ShapeKind};
use crate::core::vector::affine::AffineTransform;
use crate::core::vector::from_shape;
use crate::core::vector::object::VectorObjectData;
use crate::core::vector::style::VectorStyle;
use crate::tools::ToolId;

use super::state::{PathDisplayCacheEntry, PathDisplayCacheKey};
use super::App;

impl App {
    pub(in crate::app) fn active_path_display(&mut self) -> Option<crate::ui::PathDisplayRaster> {
        let tool = self.edit.tools.active_id();
        if !matches!(
            tool,
            ToolId::Move | ToolId::Node | ToolId::Pen | ToolId::Shape
        ) || (tool == ToolId::Pen && !self.edit.tools.pen().is_empty())
            || self.edit.transform_state.is_some()
            || self.edit.path_transform.is_some()
        {
            return None;
        }
        let scale = crate::core::vector::display::zoom_bucket(self.edit.view.zoom)?;
        let doc = &self.docs.documents[self.docs.active_doc_idx];
        let stack = &doc.canvas.layer_stack;
        let active_idx = stack.active_idx;
        let layer = stack.layers.get(active_idx)?;
        let object = match &layer.layer_type {
            LayerType::Path(object) => object.clone(),
            LayerType::Shape(shape) if matches!(tool, ToolId::Move | ToolId::Shape) => {
                shape_display_object(shape, layer.offset)
            }
            _ => return None,
        };
        let ancestors_are_plain = {
            let mut parent_id = layer.parent_id;
            let mut plain = true;
            while let Some(id) = parent_id {
                let Some(parent) = stack.layers.iter().find(|candidate| candidate.id == id) else {
                    plain = false;
                    break;
                };
                if !parent.visible
                    || parent.mask.is_some()
                    || (parent.opacity - 1.0).abs() > 1e-3
                    || parent.blend_mode != BlendMode::Normal
                {
                    plain = false;
                    break;
                }
                parent_id = parent.parent_id;
            }
            plain
        };
        let painted_layer_above = stack
            .layers
            .iter()
            .enumerate()
            .skip(active_idx + 1)
            .any(|(idx, candidate)| stack.is_effectively_visible(idx) && !candidate.is_group());
        if !stack.is_effectively_visible(active_idx)
            || painted_layer_above
            || layer.mask.is_some()
            || (layer.opacity - 1.0).abs() > 1e-3
            || layer.blend_mode != BlendMode::Normal
            || (object.style.opacity - 1.0).abs() > 1e-3
            || !ancestors_are_plain
        {
            return None;
        }
        let key = PathDisplayCacheKey {
            doc_id: doc.id.0,
            layer_id: layer.id,
            scale,
            layer_offset: layer.offset,
            object,
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
                && entry.key.layer_id == key.layer_id
                && entry.key.layer_offset == key.layer_offset
                && entry.key.object == key.object
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
        let object = key.object.clone();
        let scale = key.scale;
        std::thread::spawn(move || {
            let out =
                crate::core::vector::display::rasterize_for_display(&object, scale).map(|r| {
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

/// Adapt an editable Shape to the same transient vector object used by the
/// high-zoom Path overlay. The Shape model remains untouched; this is display
/// geometry only.
fn shape_display_object(shape: &ShapeData, layer_offset: (i32, i32)) -> VectorObjectData {
    let (x0, y0, x1, y1) = shape.canvas_span(layer_offset);
    let path = match shape.kind {
        ShapeKind::Rectangle => from_shape::rect_path(x0, y0, x1, y1, shape.effective_radius()),
        ShapeKind::Ellipse => from_shape::ellipse_path(x0, y0, x1, y1),
        ShapeKind::Line => from_shape::line_path(x0, y0, x1, y1),
        ShapeKind::Polygon => from_shape::polygon_path(x0, y0, x1, y1, shape.sides),
        ShapeKind::Star => from_shape::star_path(x0, y0, x1, y1, shape.sides, shape.star_inner),
    };
    let style = VectorStyle::from_shape_fields(
        shape.fill,
        shape.fill_color,
        shape.stroke_width,
        shape.stroke_color,
    );
    VectorObjectData::new(path, style, AffineTransform::IDENTITY)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            .map(|node| node.anchor)
            .collect::<Vec<_>>();
        assert!(anchors
            .iter()
            .all(|p| { p.x >= 99.0 && p.y >= 79.0 && p.x <= 181.0 && p.y <= 161.0 }));
    }
}

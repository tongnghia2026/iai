//! Off-thread rasterization of a Path layer's vector model, shared by the live
//! scale/rotate (Move) and node (Node) drags. Rasterizing a filled path is
//! O(area) on the CPU; doing it synchronously each mouse-move stalled the drag
//! and made scale/rotate look very laggy. This mirrors the Shape tool's worker
//! (`shape_ops`): one bake runs at a time, the newest request while it runs is
//! queued (latest wins), and `poll_path_bake` (called once per UI frame) swaps
//! the finished tiles in. The overlay meanwhile tracks the gesture's `pending`
//! geometry every frame, so the box/nodes stay glued to the cursor.

use crate::app::render::CanvasEvent;
use crate::app::state::{App, PathBakeInFlight};
use crate::core::layer::LayerType;
use crate::core::tile::TileMap;
use crate::core::vector::object::VectorObjectData;
use crate::core::vector::raster;

impl App {
    /// Request a live re-raster of Path `layer_id` to `object`. On an RGB
    /// document this runs on a worker thread (coalescing to the newest request);
    /// on CMYK it falls back to a synchronous bake so the ink planes stay in
    /// sync (worker tiles carry no ink).
    pub(in crate::app) fn request_path_bake(&mut self, layer_id: u32, object: VectorObjectData) {
        if self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .is_cmyk()
        {
            self.sync_bake_path(layer_id, object);
            return;
        }
        if self.jobs.path_bake.is_none() {
            let doc_id = self.docs.documents[self.docs.active_doc_idx].id;
            self.spawn_path_bake(doc_id, layer_id, object);
        } else {
            // A bake is already running; keep only the newest target.
            self.jobs.path_bake_next = Some((layer_id, object));
            self.arm_path_bake_poll();
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
    }

    /// Synchronous re-raster (CMYK fallback / small paths): render on the UI
    /// thread and reconcile ink. Dirty-rect invalidation only (a preview).
    fn sync_bake_path(&mut self, layer_id: u32, object: VectorObjectData) {
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let Some(idx) = canvas
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == layer_id)
        else {
            return;
        };
        let (old_off, old_w, old_h) = {
            let l = &canvas.layer_stack.layers[idx];
            (l.offset, l.width, l.height)
        };
        {
            let layer = &mut canvas.layer_stack.layers[idx];
            crate::core::command_vector::apply_object_to_layer(layer, object);
        }
        canvas.layer_revision += 1;
        let (new_off, new_w, new_h) = {
            let l = &canvas.layer_stack.layers[idx];
            (l.offset, l.width, l.height)
        };
        canvas.mark_dirty_layer_bounds(old_off.0, old_off.1, old_w, old_h);
        canvas.mark_dirty_layer_bounds(new_off.0, new_off.1, new_w, new_h);
        self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
    }

    /// Kick a worker thread that rasterizes `object`. Picked up by
    /// `poll_path_bake` on a later frame.
    fn spawn_path_bake(
        &mut self,
        doc_id: crate::core::document::DocumentId,
        layer_id: u32,
        object: VectorObjectData,
    ) {
        let (tx, rx) = std::sync::mpsc::channel();
        let job = object.clone();
        std::thread::spawn(move || {
            let out = raster::rasterize(&job).map(|r| {
                let tiles = TileMap::from_rgba(&r.rgba, r.width, r.height);
                (tiles, r.width, r.height, r.offset)
            });
            let _ = tx.send(out);
        });
        self.jobs.path_bake = Some(PathBakeInFlight {
            doc_id,
            layer_id,
            object,
            started: std::time::Instant::now(),
            rx,
        });
        self.arm_path_bake_poll();
    }

    /// Guarantee an egui frame soon while a worker bake is in flight, so its
    /// result lands even when the pointer goes idle mid-drag.
    fn arm_path_bake_poll(&mut self) {
        let due = std::time::Instant::now() + std::time::Duration::from_millis(16);
        self.win.egui_repaint_deadline =
            Some(self.win.egui_repaint_deadline.map_or(due, |d| d.min(due)));
    }

    /// Poll the in-flight worker bake (once per UI frame): swap a finished
    /// raster into its layer and start the newest queued bake, if any.
    pub fn poll_path_bake(&mut self) {
        let received = match &self.jobs.path_bake {
            Some(job) => match job.rx.try_recv() {
                Ok(r) => r,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.arm_path_bake_poll();
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
            },
            None => return,
        };
        if let Some((tiles, w, h, offset)) = received {
            let (doc_id, layer_id, object) = {
                let j = self.jobs.path_bake.as_ref().expect("checked above");
                (j.doc_id, j.layer_id, j.object.clone())
            };
            self.apply_path_bake_result(doc_id, layer_id, object, tiles, w, h, offset);
        }
        self.jobs.path_bake = None;
        // Start the newest target queued while this one ran.
        if let Some((layer_id, object)) = self.jobs.path_bake_next.take() {
            if !self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .is_cmyk()
            {
                let doc_id = self.docs.documents[self.docs.active_doc_idx].id;
                self.spawn_path_bake(doc_id, layer_id, object);
            }
        }
    }

    /// Swap a finished worker raster into its Path layer (tiles + model). Dropped
    /// when the target is no longer the active document's layer, or the document
    /// turned CMYK (worker tiles carry no ink).
    #[allow(clippy::too_many_arguments)]
    fn apply_path_bake_result(
        &mut self,
        doc_id: crate::core::document::DocumentId,
        layer_id: u32,
        object: VectorObjectData,
        tiles: TileMap,
        w: u32,
        h: u32,
        offset: (i32, i32),
    ) {
        if self.docs.documents[self.docs.active_doc_idx].id != doc_id {
            return;
        }
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        if canvas.is_cmyk() {
            return;
        }
        let Some(idx) = canvas
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == layer_id)
        else {
            return;
        };
        let (old_off, old_w, old_h) = {
            let l = &canvas.layer_stack.layers[idx];
            if !matches!(l.layer_type, LayerType::Path(_)) {
                return;
            }
            (l.offset, l.width, l.height)
        };
        let layer = &mut canvas.layer_stack.layers[idx];
        layer.tiles = tiles;
        layer.width = w;
        layer.height = h;
        layer.offset = offset;
        layer.layer_type = LayerType::Path(object);
        canvas.layer_revision += 1;
        canvas.mark_dirty_layer_bounds(old_off.0, old_off.1, old_w, old_h);
        canvas.mark_dirty_layer_bounds(offset.0, offset.1, w, h);
        self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
    }

    /// Abandon any in-flight / queued Path bake (called when a drag commits, so
    /// a late worker result can't overwrite the freshly-committed geometry).
    pub(in crate::app) fn cancel_path_bake(&mut self) {
        self.jobs.path_bake = None;
        self.jobs.path_bake_next = None;
    }
}

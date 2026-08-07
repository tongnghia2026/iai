//! Off-thread rasterization of a Path layer's vector model, shared by the live
//! scale/rotate (Move) and node (Node) drags. Rasterizing a filled path is
//! O(area) on the CPU; doing it synchronously each mouse-move stalled the drag
//! and made scale/rotate look very laggy. This mirrors the Shape tool's worker
//! (`shape_ops`): one bake runs at a time, the newest request while it runs is
//! queued (latest wins), and `poll_path_bake` (called once per UI frame) swaps
//! the finished tiles in. The overlay meanwhile tracks the gesture's `pending`
//! geometry every frame, so the box/nodes stay glued to the cursor.
//!
//! CMYK note: a CMYK document's tiles carry ink planes as ground truth. The
//! worker builds the ICC converter on its own thread (lcms2 `Transform`s are not
//! shared across threads — see [`crate::core::cms::CmykConverter`]) and encodes
//! each tile's ink from the freshly-rasterized mirror, so the swapped-in preview
//! is ink-exact. This replaced an earlier CMYK fallback that re-rasterized on the
//! UI thread on *every* pointer event (the reported CMYK drag lag) and left the
//! preview tiles ink-less.

use crate::app::render::CanvasEvent;
use crate::app::state::{App, PathBakeInFlight};
use crate::core::canvas::{CmykProfile, ColorMode};
use crate::core::layer::LayerType;
use crate::core::tile::TileMap;
use crate::core::vector::object::{VectorGeometry, VectorObjectData};
use crate::core::vector::raster;

/// Rasterize `object` into a tight `TileMap` (+ size/offset), encoding CMYK ink
/// planes from the mirror when `profile` is `Some`. Runs on the bake worker
/// thread; also exercised directly by tests. Returns `None` for a path with no
/// visible fill/outline (nothing to draw). The ICC converter is built here, on
/// the calling thread, so no lcms2 `Transform` ever crosses a thread boundary.
pub(in crate::app) fn bake_object_tiles(
    object: &VectorObjectData,
    profile: Option<&CmykProfile>,
) -> Option<(TileMap, u32, u32, (i32, i32))> {
    let r = raster::rasterize(object)?;
    let mut tiles = TileMap::from_rgba(&r.rgba, r.width, r.height);
    if let Some(profile) = profile {
        // A corrupt/non-CMYK ICC yields no converter; leave the tiles ink-less
        // and let the UI-thread guard drop the result rather than land ink-less
        // tiles on a CMYK layer (which would break separations/export).
        if let Some(conv) = profile.converter() {
            tiles.encode_ink_from_mirror(&conv);
        }
    }
    Some((tiles, r.width, r.height, r.offset))
}

impl App {
    /// The active document's CMYK profile, if it is a CMYK document. `None` in
    /// RGB mode — the worker then skips ink encoding entirely.
    fn active_cmyk_profile(&self) -> Option<CmykProfile> {
        match &self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .color_mode
        {
            ColorMode::Cmyk(profile) => Some(profile.clone()),
            ColorMode::Rgb => None,
        }
    }

    /// Request a live re-raster of Path `layer_id` to `object`. Always runs on a
    /// worker thread (coalescing to the newest request); on a CMYK document the
    /// worker encodes the ink planes too, so the drag never rasterizes on the UI
    /// thread and the preview stays ink-exact.
    pub(in crate::app) fn request_path_bake(&mut self, layer_id: u32, object: VectorObjectData) {
        if self.jobs.path_bake.is_none() {
            let doc_id = self.docs.documents[self.docs.active_doc_idx].id;
            let profile = self.active_cmyk_profile();
            self.spawn_path_bake(doc_id, layer_id, object, profile);
        } else {
            // A bake is already running; keep only the newest target. The colour
            // mode is re-read when this queued target is finally spawned.
            self.jobs.path_bake_next = Some((layer_id, object));
            self.arm_path_bake_poll();
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
    }

    /// Kick a worker thread that rasterizes `object` (and, on CMYK, encodes ink).
    /// Picked up by `poll_path_bake` on a later frame.
    fn spawn_path_bake(
        &mut self,
        doc_id: crate::core::document::DocumentId,
        layer_id: u32,
        object: VectorObjectData,
        profile: Option<CmykProfile>,
    ) {
        let (tx, rx) = std::sync::mpsc::channel();
        let job = object.clone();
        let is_cmyk = profile.is_some();
        std::thread::spawn(move || {
            let out = bake_object_tiles(&job, profile.as_ref());
            let _ = tx.send(out);
        });
        self.jobs.path_bake = Some(PathBakeInFlight {
            doc_id,
            layer_id,
            object,
            is_cmyk,
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
            let (doc_id, layer_id, object, is_cmyk) = {
                let j = self.jobs.path_bake.as_ref().expect("checked above");
                (j.doc_id, j.layer_id, j.object.clone(), j.is_cmyk)
            };
            self.apply_path_bake_result(doc_id, layer_id, object, is_cmyk, tiles, w, h, offset);
        }
        self.jobs.path_bake = None;
        // Start the newest target queued while this one ran (re-reading the
        // current colour mode, since a CMYK worker must encode ink).
        if let Some((layer_id, object)) = self.jobs.path_bake_next.take() {
            let doc_id = self.docs.documents[self.docs.active_doc_idx].id;
            let profile = self.active_cmyk_profile();
            self.spawn_path_bake(doc_id, layer_id, object, profile);
        }
    }

    /// Swap a finished worker raster into its Path layer (tiles + model). Dropped
    /// when the target is no longer the active document's layer, or the document's
    /// colour mode changed since the bake was spawned (a CMYK result carries ink /
    /// an ICC-projected mirror; an RGB result carries neither), or a CMYK bake
    /// failed to produce ink (corrupt profile) — never overwrite good inked tiles
    /// with ink-less ones.
    #[allow(clippy::too_many_arguments)]
    fn apply_path_bake_result(
        &mut self,
        doc_id: crate::core::document::DocumentId,
        layer_id: u32,
        object: VectorObjectData,
        is_cmyk: bool,
        tiles: TileMap,
        w: u32,
        h: u32,
        offset: (i32, i32),
    ) {
        if self.docs.documents[self.docs.active_doc_idx].id != doc_id {
            return;
        }
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        if canvas.is_cmyk() != is_cmyk {
            // Colour mode flipped mid-flight — this result is for the old mode.
            return;
        }
        if canvas.is_cmyk() && !tiles.has_any_ink() {
            // CMYK bake produced no ink (corrupt profile): keep the layer's
            // existing valid ink rather than stripping it.
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
            if !matches!(l.layer_type, LayerType::Vector(VectorGeometry::Path(_))) {
                return;
            }
            (l.offset, l.width, l.height)
        };
        let layer = &mut canvas.layer_stack.layers[idx];
        layer.tiles = tiles;
        layer.width = w;
        layer.height = h;
        layer.offset = offset;
        layer.layer_type = LayerType::Vector(VectorGeometry::Path(object));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::Canvas;
    use crate::core::gateway::ChangeKind;
    use crate::core::geometry::Point;
    use crate::core::vector::affine::AffineTransform;
    use crate::core::vector::color::ColorValue;
    use crate::core::vector::path::{Contour, FillRule, Node, PathData};
    use crate::core::vector::style::VectorStyle;

    fn square(side: f32, at: (f32, f32)) -> VectorObjectData {
        let path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(side, 0.0)),
                    Node::sharp(Point::new(side, side)),
                    Node::sharp(Point::new(0.0, side)),
                ],
                true,
            )],
            FillRule::NonZero,
        );
        VectorObjectData::new(
            path,
            VectorStyle::filled(ColorValue::rgb(1.0, 0.0, 0.0)),
            AffineTransform::translate(at.0, at.1),
        )
    }

    /// Build a CMYK app holding one filled Path layer; returns (app, layer_id).
    fn cmyk_app_with_path() -> (App, u32) {
        let mut app = App::new();
        let mut canvas = Canvas::new(300, 300);
        canvas
            .convert_to_cmyk(CmykProfile::Naive)
            .expect("convert to CMYK");
        canvas
            .execute(
                Box::new(crate::core::command_vector::CreatePathLayer::new(
                    square(60.0, (40.0, 40.0)),
                    "Path 1",
                )),
                ChangeKind::LayerStructure,
            )
            .expect("create path");
        let id = canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| matches!(l.layer_type, LayerType::Vector(VectorGeometry::Path(_))))
            .unwrap()
            .id;
        app.docs.documents[0].canvas = canvas;
        (app, id)
    }

    fn layer_fingerprint(app: &App, id: u32) -> u64 {
        app.docs.documents[0]
            .canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == id)
            .unwrap()
            .tiles
            .revision_fingerprint()
    }

    fn layer_has_ink(app: &App, id: u32) -> bool {
        app.docs.documents[0]
            .canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == id)
            .unwrap()
            .tiles
            .has_any_ink()
    }

    /// Spin `poll_path_bake` until the worker result lands and the queue drains.
    fn drain_path_bake(app: &mut App) {
        for _ in 0..2000 {
            app.poll_path_bake();
            if app.jobs.path_bake.is_none() && app.jobs.path_bake_next.is_none() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("path bake did not drain");
    }

    /// The worker helper itself: a CMYK bake encodes ink; an RGB bake does not.
    #[test]
    fn worker_bake_encodes_ink_only_for_cmyk() {
        let obj = square(50.0, (10.0, 10.0));
        let (rgb_tiles, ..) = bake_object_tiles(&obj, None).expect("rgb raster");
        assert!(
            !rgb_tiles.has_any_ink(),
            "an RGB bake must not fabricate ink planes"
        );
        let (cmyk_tiles, ..) =
            bake_object_tiles(&obj, Some(&CmykProfile::Naive)).expect("cmyk raster");
        assert!(
            cmyk_tiles.has_any_ink(),
            "a CMYK bake must carry ink planes so the preview stays ink-exact"
        );
    }

    /// A CMYK path bake request must NOT rasterize on the calling thread: right
    /// after the call the layer's tiles are unchanged and a worker job is queued.
    /// This is the anti-regression for the synchronous per-event CMYK bake.
    #[test]
    fn cmyk_request_path_bake_is_asynchronous() {
        let (mut app, id) = cmyk_app_with_path();
        let before = layer_fingerprint(&app, id);
        assert!(layer_has_ink(&app, id), "path starts ink-exact");

        // Reshape the path (bigger square, moved) and request a live bake.
        let moved = square(90.0, (80.0, 60.0));
        app.request_path_bake(id, moved);

        // Nothing rasterized on this thread: the layer is untouched and a worker
        // job is in flight (the old CMYK path applied synchronously here).
        assert_eq!(
            layer_fingerprint(&app, id),
            before,
            "request must not rasterize synchronously on the UI thread"
        );
        assert!(
            app.jobs.path_bake.is_some(),
            "a worker bake must be in flight"
        );
        assert!(
            app.jobs.path_bake.as_ref().unwrap().is_cmyk,
            "the in-flight job is flagged CMYK so it encodes ink"
        );

        // Once the worker lands, the tiles change AND still carry ink.
        drain_path_bake(&mut app);
        assert_ne!(
            layer_fingerprint(&app, id),
            before,
            "worker result eventually swaps in"
        );
        assert!(
            layer_has_ink(&app, id),
            "CMYK preview tiles keep their ink planes"
        );
    }

    /// Many rapid pointer events coalesce to one in-flight bake + one queued
    /// target (latest wins) — per-event work is an O(1) enqueue, not an O(area)
    /// synchronous raster.
    #[test]
    fn cmyk_path_bake_coalesces_rapid_events() {
        let (mut app, id) = cmyk_app_with_path();
        let before = layer_fingerprint(&app, id);

        for i in 0..40u32 {
            let side = 60.0 + i as f32;
            app.request_path_bake(id, square(side, (40.0, 40.0)));
        }
        // Exactly one worker in flight; the other 39 collapsed into the newest
        // queued target. No synchronous rasterization touched the layer yet.
        assert!(app.jobs.path_bake.is_some(), "one bake in flight");
        assert!(
            app.jobs.path_bake_next.is_some(),
            "extra events coalesce into a single queued target"
        );
        assert_eq!(
            layer_fingerprint(&app, id),
            before,
            "no per-event synchronous raster on the UI thread"
        );

        drain_path_bake(&mut app);
        assert!(layer_has_ink(&app, id), "final CMYK tiles carry ink");
    }
}

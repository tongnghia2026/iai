//! On-canvas transform handles for a Path layer under the Move tool (Slice 2 of
//! the Pick↔Move unification). A selected editable Path shows an oriented
//! bounding box; dragging a corner/edge handle SCALES it and dragging the ring
//! just outside a corner ROTATES it. Both edit the vector object's affine
//! `transform` only — node coordinates are never baked — and each gesture
//! commits ONE [`ChangeVectorTransform`] so the object stays editable and the
//! whole drag is a single undo step (Mục 3.2 / 3.4).
//!
//! Coordinate model: with position committed into the model (the Move tool folds
//! its drag into `transform` on release), a Path layer keeps `Layer::offset ==
//! model raster origin`, so object LAYER space equals CANVAS space. The box is
//! therefore `transform.apply(local_corner)` directly, and the scale is done in
//! the object's LOCAL frame so it stays aligned with a rotated object's own axes.

use crate::app::render::CanvasEvent;
use crate::app::state::{App, PathTransformDrag, TransformHandle};
use crate::core::geometry::{Point, Rect};
use crate::core::layer::LayerType;
use crate::core::vector::affine::AffineTransform;
use crate::core::vector::object::VectorObjectData;

/// Screen-space grab radius for a box handle.
const HANDLE_HIT_PX: f32 = 9.0;
/// Screen-space outer radius of the rotate ring just past each corner.
const ROTATE_RING_PX: f32 = 26.0;
/// Flatten tolerance (object-local units) for the box geometry.
const BOX_TOL: f32 = 0.25;
/// Smallest |scale factor| a drag may collapse to (keeps the affine invertible).
const MIN_SCALE: f32 = 0.01;

/// What the pointer is over on the active Path's transform box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathBoxHit {
    /// A corner/edge handle → scale about the opposite handle (or centre on Alt).
    Handle(TransformHandle),
    /// The ring just outside a corner → rotate about the box centre.
    Rotate,
}

/// The active Path's oriented transform box in CANVAS space, ready for the
/// shared transform overlay: 4 corners `[TL, TR, BL, BR]`, 8 handles
/// `[TL, TC, TR, ML, MR, BL, BC, BR]`, and the centre.
pub struct PathBox {
    pub corners: [(f32, f32); 4],
    pub handles: [(f32, f32); 8],
    pub center: (f32, f32),
}

/// The 8 scale handles, in the order the overlay expects.
const HANDLE_ORDER: [TransformHandle; 8] = [
    TransformHandle::TopLeft,
    TransformHandle::TopCenter,
    TransformHandle::TopRight,
    TransformHandle::MiddleLeft,
    TransformHandle::MiddleRight,
    TransformHandle::BottomLeft,
    TransformHandle::BottomCenter,
    TransformHandle::BottomRight,
];

/// Object-local position of a handle on the local bounds rectangle.
fn local_handle_point(b: Rect, h: TransformHandle) -> Point {
    let (cx, cy) = (b.x + b.w * 0.5, b.y + b.h * 0.5);
    let (x0, y0, x1, y1) = (b.x, b.y, b.x + b.w, b.y + b.h);
    match h {
        TransformHandle::TopLeft => Point::new(x0, y0),
        TransformHandle::TopCenter => Point::new(cx, y0),
        TransformHandle::TopRight => Point::new(x1, y0),
        TransformHandle::MiddleLeft => Point::new(x0, cy),
        TransformHandle::MiddleRight => Point::new(x1, cy),
        TransformHandle::BottomLeft => Point::new(x0, y1),
        TransformHandle::BottomCenter => Point::new(cx, y1),
        TransformHandle::BottomRight => Point::new(x1, y1),
        TransformHandle::Center => Point::new(cx, cy),
    }
}

/// The handle diagonally opposite `h` (the fixed anchor for a non-Alt scale).
fn opposite_handle(h: TransformHandle) -> TransformHandle {
    match h {
        TransformHandle::TopLeft => TransformHandle::BottomRight,
        TransformHandle::TopCenter => TransformHandle::BottomCenter,
        TransformHandle::TopRight => TransformHandle::BottomLeft,
        TransformHandle::MiddleLeft => TransformHandle::MiddleRight,
        TransformHandle::MiddleRight => TransformHandle::MiddleLeft,
        TransformHandle::BottomLeft => TransformHandle::TopRight,
        TransformHandle::BottomCenter => TransformHandle::TopCenter,
        TransformHandle::BottomRight => TransformHandle::TopLeft,
        TransformHandle::Center => TransformHandle::Center,
    }
}

/// `(is_corner, scales_x, scales_y)` for a handle.
fn handle_axes(h: TransformHandle) -> (bool, bool, bool) {
    match h {
        TransformHandle::TopLeft
        | TransformHandle::TopRight
        | TransformHandle::BottomLeft
        | TransformHandle::BottomRight => (true, true, true),
        TransformHandle::TopCenter | TransformHandle::BottomCenter => (false, false, true),
        TransformHandle::MiddleLeft | TransformHandle::MiddleRight => (false, true, false),
        TransformHandle::Center => (false, false, false),
    }
}

/// Keep the drag from collapsing an axis to (near) zero, which would make the
/// object's transform singular and un-invertible. Sign is preserved so flips
/// (negative scale) still work.
fn clamp_scale(s: f32) -> f32 {
    if !s.is_finite() {
        return 1.0;
    }
    if s.abs() < MIN_SCALE {
        if s < 0.0 {
            -MIN_SCALE
        } else {
            MIN_SCALE
        }
    } else {
        s
    }
}

impl App {
    /// Active layer index if it is an on-canvas-transformable Path (visible,
    /// unlocked, not the background). The box shows for this layer under Move;
    /// the Node tool reuses it for the layer it edits.
    pub(crate) fn active_path_layer(&self) -> Option<usize> {
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let idx = canvas.layer_stack.active_idx;
        let layer = canvas.layer_stack.layers.get(idx)?;
        if matches!(layer.layer_type, LayerType::Path(_))
            && layer.visible
            && !layer.locked
            && !layer.is_background
        {
            Some(idx)
        } else {
            None
        }
    }

    /// The active Path's object transform + local bounds, or `None`.
    fn active_path_object(&self) -> Option<(u32, AffineTransform, Rect)> {
        let idx = self.active_path_layer()?;
        let layer = &self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .layers[idx];
        let LayerType::Path(obj) = &layer.layer_type else {
            return None;
        };
        let lb = obj.local_bounds(BOX_TOL)?;
        Some((layer.id, obj.transform, lb))
    }

    /// The active Path's oriented transform box, or `None` when the Move tool is
    /// not showing one (wrong tool, no editable Path active, a modal transform,
    /// or a plain layer-move drag in progress). Consumed by the overlay VM and by
    /// handle hit-testing, so both agree on where the handles are.
    pub fn active_path_transform_box(&self) -> Option<PathBox> {
        if self.edit.tools.active_id() != crate::tools::ToolId::Move
            || self.edit.transform_state.is_some()
        {
            return None;
        }
        // Hide the box during a plain layer-move drag (it would lag the cursor);
        // keep it while a handle drag is live so it tracks the gesture.
        if self.edit.input.painting && self.edit.path_transform.is_none() {
            return None;
        }
        let (id, model_t, b) = self.active_path_object()?;
        // During a handle drag the box tracks `pending` (updated every frame),
        // not the throttled re-rastered model, so it stays glued to the cursor.
        let t = match &self.edit.path_transform {
            Some(d) if d.layer_id == id => d.pending,
            _ => model_t,
        };
        let map = |p: Point| {
            let q = t.apply_point(p);
            (q.x, q.y)
        };
        let corners = [
            map(local_handle_point(b, TransformHandle::TopLeft)),
            map(local_handle_point(b, TransformHandle::TopRight)),
            map(local_handle_point(b, TransformHandle::BottomLeft)),
            map(local_handle_point(b, TransformHandle::BottomRight)),
        ];
        let mut handles = [(0.0f32, 0.0f32); 8];
        for (i, h) in HANDLE_ORDER.iter().enumerate() {
            handles[i] = map(local_handle_point(b, *h));
        }
        let center = map(local_handle_point(b, TransformHandle::Center));
        Some(PathBox {
            corners,
            handles,
            center,
        })
    }

    /// Which part of the active Path's box is under the screen point, if any.
    /// Handles win over the rotate ring; the ring only fires just OUTSIDE a
    /// corner (radially past it) so it never shadows a corner handle.
    pub fn path_box_hit_at_screen(&self, sx: f32, sy: f32) -> Option<PathBoxHit> {
        let bx = self.active_path_transform_box()?;
        let zoom = self.edit.view.zoom;
        let vox = self.edit.view.offset_x;
        let voy = self.edit.view.offset_y;
        let c2s = |(cx, cy): (f32, f32)| (cx * zoom + vox, cy * zoom + voy);
        let dist = |(ax, ay): (f32, f32), (bx, by): (f32, f32)| {
            ((ax - bx).powi(2) + (ay - by).powi(2)).sqrt()
        };

        let mut best: Option<(f32, TransformHandle)> = None;
        for (i, h) in HANDLE_ORDER.iter().enumerate() {
            let d = dist((sx, sy), c2s(bx.handles[i]));
            if d <= HANDLE_HIT_PX && best.map_or(true, |(bd, _)| d < bd) {
                best = Some((d, *h));
            }
        }
        if let Some((_, h)) = best {
            return Some(PathBoxHit::Handle(h));
        }

        let center_s = c2s(bx.center);
        for &corner in &bx.corners {
            let cs = c2s(corner);
            let d = dist((sx, sy), cs);
            let corner_r = dist(cs, center_s);
            if d > HANDLE_HIT_PX
                && d <= ROTATE_RING_PX
                && dist((sx, sy), center_s) >= corner_r - 2.0
            {
                return Some(PathBoxHit::Rotate);
            }
        }
        None
    }

    /// Topmost visible Path layer whose FILL or outline contains the canvas point,
    /// tested against the vector MODEL (honours fill rule and holes — a click in a
    /// donut's hole misses). Used for the Move tool's Pick cursor; layer
    /// selection itself already picks a Path by its raster alpha, which is the same
    /// fill. The delta-0 invariant (position committed into the model) makes the
    /// object transform inverse map canvas → object-local directly.
    pub fn path_layer_hit_at(&self, cx: f32, cy: f32) -> Option<usize> {
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let tol = (2.0 / self.edit.view.zoom.max(1e-4)).clamp(0.25, 8.0);
        for (idx, layer) in canvas.layer_stack.layers.iter().enumerate().rev() {
            if !layer.visible {
                continue;
            }
            let LayerType::Path(obj) = &layer.layer_type else {
                continue;
            };
            let Some(inv) = obj.transform.inverse() else {
                continue;
            };
            let p = inv.apply_point(Point::new(cx, cy));
            let hit_fill = obj.style.fill.is_visible()
                && crate::core::vector::hittest::fill_contains(&obj.path, p, tol);
            let half = obj.style.effective_stroke_width() * 0.5;
            let hit_stroke = obj.style.stroke.is_visible()
                && half > 0.0
                && crate::core::vector::hittest::stroke_hit(&obj.path, p, half, tol);
            if hit_fill || hit_stroke {
                return Some(idx);
            }
        }
        None
    }

    /// Cursor hint for the Move tool at the current mouse position:
    /// `0` none, `1` move (over a Path fill/outline), `2` NWSE-resize,
    /// `3` NESW-resize, `4` NS-resize, `5` EW-resize, `6` rotate. Resize direction
    /// is derived from the handle's SCREEN offset from the box centre, so it stays
    /// correct for a rotated object.
    pub fn move_hover_hint(&self) -> u8 {
        let (sx, sy) = (self.edit.input.mouse_x, self.edit.input.mouse_y);
        if let Some(hit) = self.path_box_hit_at_screen(sx, sy) {
            match hit {
                PathBoxHit::Rotate => return 6,
                PathBoxHit::Handle(h) => {
                    if let Some(bx) = self.active_path_transform_box() {
                        let zoom = self.edit.view.zoom;
                        let vox = self.edit.view.offset_x;
                        let voy = self.edit.view.offset_y;
                        if let Some(i) = HANDLE_ORDER.iter().position(|x| *x == h) {
                            let (hx, hy) = bx.handles[i];
                            let dx = (hx * zoom + vox) - (bx.center.0 * zoom + vox);
                            let dy = (hy * zoom + voy) - (bx.center.1 * zoom + voy);
                            let deg = dy.atan2(dx).to_degrees().rem_euclid(180.0);
                            return if !(22.5..157.5).contains(&deg) {
                                5 // horizontal → EW
                            } else if (67.5..112.5).contains(&deg) {
                                4 // vertical → NS
                            } else if (22.5..67.5).contains(&deg) {
                                2 // "\" diagonal → NWSE
                            } else {
                                3 // "/" diagonal → NESW
                            };
                        }
                    }
                    return 0;
                }
            }
        }
        let ev = self.tool_event();
        if self.path_layer_hit_at(ev.canvas_x, ev.canvas_y).is_some() {
            return 1;
        }
        0
    }

    /// Begin an on-canvas Path transform gesture. `(cx, cy)` is the press point in
    /// canvas space (the rotation reference). Folds any pending Move drag into the
    /// model first so the box pivot is the displayed centre and `orig_transform`
    /// is the true undo baseline.
    pub fn path_transform_begin(&mut self, hit: PathBoxHit, cx: f32, cy: f32) {
        let Some(idx) = self.active_path_layer() else {
            return;
        };
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let layer = &mut canvas.layer_stack.layers[idx];
        // Normalise any residual offset↔model drift (no-op in the common case).
        crate::core::command_vector::fold_offset_into_model(layer);
        let layer_id = layer.id;
        let LayerType::Path(obj) = &layer.layer_type else {
            return;
        };
        let orig = obj.transform;
        let Some(lb) = obj.local_bounds(BOX_TOL) else {
            return;
        };
        let pivot = orig.apply_point(Point::new(lb.x + lb.w * 0.5, lb.y + lb.h * 0.5));
        let handle = match hit {
            PathBoxHit::Handle(h) => Some(h),
            PathBoxHit::Rotate => None,
        };
        self.edit.path_transform = Some(PathTransformDrag {
            layer_id,
            handle,
            orig_transform: orig,
            pending: orig,
            local_bounds: lb,
            pivot,
            start_cx: cx,
            start_cy: cy,
            changed: false,
        });
    }

    /// True while a Path transform handle is being dragged.
    pub fn path_transform_active(&self) -> bool {
        self.edit.path_transform.is_some()
    }

    /// Compute the target transform for the in-progress gesture at cursor
    /// `(cx, cy)` in canvas space. Scales are done in the object's LOCAL frame so
    /// they stay square to a rotated object; rotation is a canvas-space turn about
    /// the box centre. Returns `None` for a degenerate (non-finite / singular)
    /// result, which the caller ignores.
    fn path_transform_target(
        drag: &PathTransformDrag,
        cx: f32,
        cy: f32,
        shift: bool,
        alt: bool,
    ) -> Option<AffineTransform> {
        let orig = drag.orig_transform;
        match drag.handle {
            // Rotate about the box centre (canvas space). Shift snaps to 15°.
            None => {
                let a0 = (drag.start_cy - drag.pivot.y).atan2(drag.start_cx - drag.pivot.x);
                let a1 = (cy - drag.pivot.y).atan2(cx - drag.pivot.x);
                let mut dth = a1 - a0;
                if shift {
                    let step = std::f32::consts::FRAC_PI_2 / 6.0; // 15°
                    dth = (dth / step).round() * step;
                }
                let m = AffineTransform::translate(drag.pivot.x, drag.pivot.y)
                    .then(&AffineTransform::rotate(dth))
                    .then(&AffineTransform::translate(-drag.pivot.x, -drag.pivot.y));
                let new = m.then(&orig);
                new.is_finite().then_some(new)
            }
            // Scale about the opposite handle (or the centre with Alt).
            Some(h) => {
                let inv = orig.inverse()?;
                let cl = inv.apply_point(Point::new(cx, cy));
                let b = drag.local_bounds;
                let moving = local_handle_point(b, h);
                let anchor = if alt {
                    Point::new(b.x + b.w * 0.5, b.y + b.h * 0.5)
                } else {
                    local_handle_point(b, opposite_handle(h))
                };
                let (is_corner, use_x, use_y) = handle_axes(h);
                let (mut sx, mut sy) = (1.0f32, 1.0f32);
                let (vx, vy) = (moving.x - anchor.x, moving.y - anchor.y);
                let (ux, uy) = (cl.x - anchor.x, cl.y - anchor.y);
                if is_corner {
                    if shift {
                        // Non-uniform: each axis follows the cursor independently.
                        if vx.abs() > 1e-3 {
                            sx = ux / vx;
                        }
                        if vy.abs() > 1e-3 {
                            sy = uy / vy;
                        }
                    } else {
                        // Uniform: project the cursor onto the box diagonal.
                        let denom = vx * vx + vy * vy;
                        let s = if denom > 1e-6 {
                            (ux * vx + uy * vy) / denom
                        } else {
                            1.0
                        };
                        sx = s;
                        sy = s;
                    }
                } else {
                    if use_x && vx.abs() > 1e-3 {
                        sx = ux / vx;
                    }
                    if use_y && vy.abs() > 1e-3 {
                        sy = uy / vy;
                    }
                }
                sx = clamp_scale(sx);
                sy = clamp_scale(sy);
                let sl = AffineTransform::translate(anchor.x, anchor.y)
                    .then(&AffineTransform::scale(sx, sy))
                    .then(&AffineTransform::translate(-anchor.x, -anchor.y));
                let new = orig.then(&sl);
                (new.is_finite() && new.determinant().abs() > 1e-6).then_some(new)
            }
        }
    }

    /// Apply the in-progress gesture to `(cx, cy)`. The target transform is stored
    /// as `pending` every frame (the overlay box follows it smoothly at 60 fps);
    /// the fill re-raster is handed to the OFF-THREAD Path bake so a big filled
    /// path never stalls the drag. The undo entry is recorded once on release.
    pub fn path_transform_update(&mut self, cx: f32, cy: f32, shift: bool, alt: bool) {
        let (new_t, layer_id) = match self.edit.path_transform.as_mut() {
            Some(d) => match Self::path_transform_target(d, cx, cy, shift, alt) {
                Some(t) => {
                    d.pending = t;
                    d.changed = true;
                    (t, d.layer_id)
                }
                None => return,
            },
            None => return,
        };
        // Build the target object (path + style kept, new transform) and request
        // an off-thread bake; the overlay already tracks `pending` this frame.
        let object = {
            let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
            match canvas.layer_stack.layers.iter().find(|l| l.id == layer_id) {
                Some(l) => match &l.layer_type {
                    LayerType::Path(o) if o.transform != new_t => VectorObjectData {
                        transform: new_t,
                        ..o.clone()
                    },
                    _ => return,
                },
                None => return,
            }
        };
        self.request_path_bake(layer_id, object);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Finish the gesture: restore the pre-gesture transform so the gateway
    /// captures the correct "before", then commit the final transform as ONE
    /// [`crate::core::command_vector::ChangeVectorTransform`] (a single undo
    /// step). A no-op drag (a click on a handle) records nothing.
    pub fn path_transform_finish(&mut self) {
        let Some(drag) = self.edit.path_transform.take() else {
            return;
        };
        // Abandon any in-flight worker bake — the final geometry is committed
        // synchronously below, so a late result would be stale.
        self.cancel_path_bake();
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let Some(idx) = canvas
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == drag.layer_id)
        else {
            return;
        };
        if !matches!(
            canvas.layer_stack.layers[idx].layer_type,
            LayerType::Path(_)
        ) {
            return;
        }
        // Commit `pending` — the true target at release, which the throttled live
        // re-raster may not have caught up to in the model yet.
        let final_t = drag.pending;
        if !drag.changed || final_t == drag.orig_transform {
            // Nothing moved — make sure the model is exactly the baseline.
            let model_t = match &canvas.layer_stack.layers[idx].layer_type {
                LayerType::Path(o) => o.transform,
                _ => return,
            };
            if model_t != drag.orig_transform {
                let layer = &mut canvas.layer_stack.layers[idx];
                if let LayerType::Path(o) = &layer.layer_type {
                    let mut o = o.clone();
                    o.transform = drag.orig_transform;
                    crate::core::command_vector::apply_object_to_layer(layer, o);
                }
                self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
            }
            return;
        }

        // Rewind the live preview to the baseline so `execute` records old→new.
        {
            let layer = &mut canvas.layer_stack.layers[idx];
            if let LayerType::Path(o) = &layer.layer_type {
                let mut o = o.clone();
                o.transform = drag.orig_transform;
                crate::core::command_vector::apply_object_to_layer(layer, o);
            }
        }
        let _ = canvas.execute(
            Box::new(crate::core::command_vector::ChangeVectorTransform::new(
                drag.layer_id,
                final_t,
            )),
            crate::core::gateway::ChangeKind::LayerStructure,
        );
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drag(handle: Option<TransformHandle>) -> PathTransformDrag {
        PathTransformDrag {
            layer_id: 1,
            handle,
            orig_transform: AffineTransform::IDENTITY,
            pending: AffineTransform::IDENTITY,
            local_bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
            pivot: Point::new(50.0, 50.0),
            start_cx: 100.0,
            start_cy: 50.0,
            changed: false,
        }
    }

    fn close(a: Point, b: Point) -> bool {
        (a.x - b.x).abs() < 1e-2 && (a.y - b.y).abs() < 1e-2
    }

    #[test]
    fn corner_scale_keeps_opposite_corner_fixed() {
        // Drag BR to (200,200): the box doubles about the fixed TL corner.
        let d = drag(Some(TransformHandle::BottomRight));
        let t = App::path_transform_target(&d, 200.0, 200.0, false, false).unwrap();
        assert!(close(
            t.apply_point(Point::new(0.0, 0.0)),
            Point::new(0.0, 0.0)
        ));
        assert!(close(
            t.apply_point(Point::new(100.0, 100.0)),
            Point::new(200.0, 200.0)
        ));
        // Uniform: the mid of the top edge tracks the doubling too.
        assert!(close(
            t.apply_point(Point::new(50.0, 0.0)),
            Point::new(100.0, 0.0)
        ));
    }

    #[test]
    fn edge_handle_scales_single_axis() {
        // MiddleRight drag scales X only; Y is untouched.
        let d = drag(Some(TransformHandle::MiddleRight));
        let t = App::path_transform_target(&d, 150.0, 999.0, false, false).unwrap();
        assert!(close(
            t.apply_point(Point::new(100.0, 50.0)),
            Point::new(150.0, 50.0)
        ));
        assert!(close(
            t.apply_point(Point::new(0.0, 100.0)),
            Point::new(0.0, 100.0)
        ));
    }

    #[test]
    fn alt_scales_about_centre() {
        // Alt: MiddleRight scales about the centre, so the left edge moves out too.
        let d = drag(Some(TransformHandle::MiddleRight));
        let t = App::path_transform_target(&d, 100.0, 50.0, false, true).unwrap();
        // Right edge x=100 was 50 from centre; cursor 100 → 50 from centre → s=1.
        // Push further: cursor at 150 → right edge 150, left edge mirrors to -50.
        let t2 = App::path_transform_target(&d, 150.0, 50.0, false, true).unwrap();
        assert!(close(
            t2.apply_point(Point::new(100.0, 50.0)),
            Point::new(150.0, 50.0)
        ));
        assert!(close(
            t2.apply_point(Point::new(0.0, 50.0)),
            Point::new(-50.0, 50.0)
        ));
        let _ = t;
    }

    #[test]
    fn rotate_ninety_maps_right_to_bottom() {
        // Rotate: press at the right-mid (start), drag to below the pivot → +90°.
        let d = drag(None);
        let t = App::path_transform_target(&d, 50.0, 100.0, false, false).unwrap();
        // Right-mid (100,50) rotates about (50,50) to the bottom-mid (50,100).
        assert!(close(
            t.apply_point(Point::new(100.0, 50.0)),
            Point::new(50.0, 100.0)
        ));
        // The centre is the pivot — unmoved.
        assert!(close(
            t.apply_point(Point::new(50.0, 50.0)),
            Point::new(50.0, 50.0)
        ));
    }

    #[test]
    fn shift_rotate_snaps_to_fifteen_degrees() {
        // A ~5° drag snaps to 0 (nearest 15° step is 0).
        let d = drag(None);
        let ang = 5f32.to_radians();
        let (cx, cy) = (50.0 + 50.0 * ang.cos(), 50.0 + 50.0 * ang.sin());
        let t = App::path_transform_target(&d, cx, cy, true, false).unwrap();
        // Snapped to 0° ⇒ identity-ish: the right-mid stays put.
        assert!(close(
            t.apply_point(Point::new(100.0, 50.0)),
            Point::new(100.0, 50.0)
        ));
    }

    #[test]
    fn degenerate_scale_is_clamped_not_singular() {
        // Dragging the BR handle back onto the TL anchor must not produce a
        // singular (zero-area) transform.
        let d = drag(Some(TransformHandle::BottomRight));
        let t = App::path_transform_target(&d, 0.0, 0.0, false, false).unwrap();
        assert!(t.determinant().abs() > 1e-9, "transform stays invertible");
    }

    fn rect_path_object(
        w: f32,
        h: f32,
        tx: f32,
        ty: f32,
    ) -> crate::core::vector::object::VectorObjectData {
        use crate::core::vector::path::{Contour, FillRule, Node, PathData};
        use crate::core::vector::style::VectorStyle;
        let path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(w, 0.0)),
                    Node::sharp(Point::new(w, h)),
                    Node::sharp(Point::new(0.0, h)),
                ],
                true,
            )],
            FillRule::NonZero,
        );
        crate::core::vector::object::VectorObjectData::new(
            path,
            VectorStyle::default(),
            AffineTransform::translate(tx, ty),
        )
    }

    /// Build an App with one active, selected Path layer and the Move tool live.
    fn app_with_active_path() -> (App, u32) {
        use crate::core::canvas::Canvas;
        use crate::core::command_vector::CreatePathLayer;
        use crate::core::gateway::ChangeKind;

        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(300, 300);
        let id = {
            let canvas = &mut app.docs.documents[0].canvas;
            canvas
                .execute(
                    Box::new(CreatePathLayer::new(
                        rect_path_object(40.0, 20.0, 100.0, 120.0),
                        "Path 1",
                    )),
                    ChangeKind::LayerStructure,
                )
                .unwrap();
            let idx = canvas
                .layer_stack
                .layers
                .iter()
                .position(|l| matches!(l.layer_type, LayerType::Path(_)))
                .unwrap();
            canvas.layer_stack.layers[idx].selected = true;
            canvas.layer_stack.active_idx = idx;
            canvas.layer_stack.layers[idx].id
        };
        app.edit.tools.select(crate::tools::ToolId::Move);
        app.edit.view.zoom = 1.0;
        app.edit.view.offset_x = 0.0;
        app.edit.view.offset_y = 0.0;
        (app, id)
    }

    fn find_layer(app: &App, id: u32) -> &crate::core::layer::Layer {
        app.docs.documents[0]
            .canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == id)
            .unwrap()
    }

    #[test]
    fn box_handles_track_the_object() {
        let (app, _id) = app_with_active_path();
        let bx = app
            .active_path_transform_box()
            .expect("box shown for active path");
        // Object at translate(100,120), size 40×20 ⇒ TL≈(100,120), BR≈(140,140).
        assert!((bx.corners[0].0 - 100.0).abs() < 1.0 && (bx.corners[0].1 - 120.0).abs() < 1.0);
        assert!((bx.corners[3].0 - 140.0).abs() < 1.0 && (bx.corners[3].1 - 140.0).abs() < 1.0);
        // Bottom-right handle is the last of the 8.
        assert!((bx.handles[7].0 - 140.0).abs() < 1.0 && (bx.handles[7].1 - 140.0).abs() < 1.0);
    }

    #[test]
    fn drag_handle_scales_stays_editable_and_undoes() {
        let (mut app, id) = app_with_active_path();
        let (w0, h0) = {
            let l = find_layer(&app, id);
            (l.width, l.height)
        };
        let (hx, hy) = {
            let bx = app.active_path_transform_box().unwrap();
            bx.handles[7] // bottom-right
        };
        app.path_transform_begin(PathBoxHit::Handle(TransformHandle::BottomRight), hx, hy);
        // Drag it out well past the object to enlarge.
        app.path_transform_update(220.0, 200.0, false, false);
        app.path_transform_finish();

        let l = find_layer(&app, id);
        assert!(
            matches!(l.layer_type, LayerType::Path(_)),
            "scaled Path stays an editable vector (not baked to Raster)"
        );
        assert!(
            l.width > w0 && l.height > h0,
            "raster grew: {}x{} was {w0}x{h0}",
            l.width,
            l.height
        );

        // One undo step restores the original size and keeps it a Path.
        app.docs.documents[0].canvas.undo().expect("undo");
        let l = find_layer(&app, id);
        assert!(matches!(l.layer_type, LayerType::Path(_)));
        assert_eq!((l.width, l.height), (w0, h0), "undo restored the size");
    }
}

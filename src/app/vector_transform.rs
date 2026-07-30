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
use crate::core::vector::object::VectorGeometry;
use crate::gpu::compositor::TransformPreviewUniform;

/// Screen-space grab radius for a box handle.
const HANDLE_HIT_PX: f32 = 9.0;
/// Screen-space outer radius of the rotate ring just past each corner.
const ROTATE_RING_PX: f32 = 26.0;
/// Flatten tolerance (object-local units) for the box geometry.
const BOX_TOL: f32 = 0.25;
/// Smallest |scale factor| a drag may collapse to (keeps the affine invertible).
const MIN_SCALE: f32 = 0.01;

fn path_preview_inverse(original: AffineTransform, pending: AffineTransform) -> Option<[f32; 9]> {
    let inverse = original.then(&pending.inverse()?);
    Some([
        inverse.a, inverse.c, inverse.e, inverse.b, inverse.d, inverse.f, 0.0, 0.0, 1.0,
    ])
}

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

/// Cursor hint for one oriented-box resize handle.
///
/// Unit box axes make corner directions independent of aspect ratio. Using the
/// raw centre-to-corner vector made tall, narrow text boxes report an edge
/// cursor because that vector is almost vertical.
fn path_handle_cursor_hint(bx: &PathBox, h: TransformHandle) -> u8 {
    let unit = |x: f32, y: f32| {
        let len = (x * x + y * y).sqrt().max(1e-6);
        (x / len, y / len)
    };
    let (cx, cy) = bx.center;
    let (ux, uy) = unit(bx.handles[4].0 - cx, bx.handles[4].1 - cy);
    let (vx, vy) = unit(bx.handles[6].0 - cx, bx.handles[6].1 - cy);
    let (dx, dy) = match h {
        TransformHandle::TopLeft => (-ux - vx, -uy - vy),
        TransformHandle::TopCenter => (-vx, -vy),
        TransformHandle::TopRight => (ux - vx, uy - vy),
        TransformHandle::MiddleLeft | TransformHandle::MiddleRight => (ux, uy),
        TransformHandle::BottomLeft => (-ux + vx, -uy + vy),
        TransformHandle::BottomCenter => (vx, vy),
        TransformHandle::BottomRight => (ux + vx, uy + vy),
        TransformHandle::Center => return 0,
    };
    let deg = dy.atan2(dx).to_degrees().rem_euclid(180.0);
    if !(22.5..157.5).contains(&deg) {
        5
    } else if (67.5..112.5).contains(&deg) {
        4
    } else if (22.5..67.5).contains(&deg) {
        2
    } else {
        3
    }
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

/// Build the oriented transform box for the local-bounds rectangle `b` mapped
/// through `t`. Used both for a single Path (t = the object transform, b =
/// object-local bounds — a box that stays glued to a rotated object) and for a
/// multi-Path union drag (t = the canvas-space delta `M`, b = the union AABB in
/// canvas space — the whole selection scales/rotates as one rigid box).
fn box_from(t: AffineTransform, b: Rect) -> PathBox {
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
    PathBox {
        corners,
        handles,
        center,
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
        if matches!(layer.layer_type, LayerType::Vector(VectorGeometry::Path(_)))
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
        let LayerType::Vector(VectorGeometry::Path(obj)) = &layer.layer_type else {
            return None;
        };
        let lb = obj.local_bounds(BOX_TOL)?;
        Some((layer.id, obj.transform, lb))
    }

    /// Selected, visible, unlocked, non-background Path layer indices — the set an
    /// on-canvas transform gesture would move together.
    fn selected_path_layers(&self) -> Vec<usize> {
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        canvas
            .layer_stack
            .layers
            .iter()
            .enumerate()
            .filter(|(_, l)| {
                l.selected
                    && l.visible
                    && !l.locked
                    && !l.is_background
                    && matches!(l.layer_type, LayerType::Vector(VectorGeometry::Path(_)))
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// The clean multi-Path selection to transform together as a single union
    /// box, or `None`. Requires at least two eligible Paths and NO other selected
    /// non-background layer — a mixed selection (a Path plus a raster, say) can't
    /// be faithfully scaled/rotated by the affine-only on-canvas box, so it falls
    /// back to the single active-Path box instead of silently moving only some.
    fn multi_path_targets(&self) -> Option<Vec<usize>> {
        let paths = self.selected_path_layers();
        if paths.len() < 2 {
            return None;
        }
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let clean = !canvas
            .layer_stack
            .layers
            .iter()
            .enumerate()
            .any(|(i, l)| l.selected && !l.is_background && !paths.contains(&i));
        clean.then_some(paths)
    }

    /// Canvas-space AABB of one Path layer's fill geometry (object-local bounds
    /// mapped through the object transform), or `None` for a degenerate path.
    fn path_canvas_bounds(&self, idx: usize) -> Option<Rect> {
        let layer = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .layers
            .get(idx)?;
        let LayerType::Vector(VectorGeometry::Path(obj)) = &layer.layer_type else {
            return None;
        };
        let b = obj.local_bounds(BOX_TOL)?;
        let pts = [
            obj.transform.apply_point(Point::new(b.x, b.y)),
            obj.transform.apply_point(Point::new(b.x + b.w, b.y)),
            obj.transform.apply_point(Point::new(b.x, b.y + b.h)),
            obj.transform.apply_point(Point::new(b.x + b.w, b.y + b.h)),
        ];
        let mut x0 = f32::INFINITY;
        let mut y0 = f32::INFINITY;
        let mut x1 = f32::NEG_INFINITY;
        let mut y1 = f32::NEG_INFINITY;
        for p in pts {
            x0 = x0.min(p.x);
            y0 = y0.min(p.y);
            x1 = x1.max(p.x);
            y1 = y1.max(p.y);
        }
        (x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
    }

    /// Union of every target Path's canvas-space AABB — the multi-select box.
    fn union_canvas_bounds(&self, paths: &[usize]) -> Option<Rect> {
        let mut x0 = f32::INFINITY;
        let mut y0 = f32::INFINITY;
        let mut x1 = f32::NEG_INFINITY;
        let mut y1 = f32::NEG_INFINITY;
        let mut any = false;
        for &idx in paths {
            if let Some(b) = self.path_canvas_bounds(idx) {
                x0 = x0.min(b.x);
                y0 = y0.min(b.y);
                x1 = x1.max(b.x + b.w);
                y1 = y1.max(b.y + b.h);
                any = true;
            }
        }
        (any && x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
    }

    /// The on-canvas transform box, or `None` when the Move tool is not showing
    /// one (wrong tool, nothing suitable selected, a modal transform, or a plain
    /// layer-move drag in progress). Shows a single oriented box for one selected
    /// Path or a union box around a clean multi-Path selection. Consumed by the
    /// overlay VM and by handle hit-testing, so both agree on where the handles
    /// are.
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
        // Live gesture: the box tracks `pending` every frame (the single object
        // target, or the multi-Path canvas-space delta) so it stays glued to the
        // cursor rather than the throttled, re-rastered model.
        if let Some(d) = &self.edit.path_transform {
            return Some(box_from(d.pending, d.local_bounds));
        }
        // Idle: a clean multi-Path selection shows one union box around them all
        // (the reported "frame only wraps one layer" bug).
        if let Some(paths) = self.multi_path_targets() {
            let b = self.union_canvas_bounds(&paths)?;
            return Some(box_from(AffineTransform::IDENTITY, b));
        }
        // Idle single: show the active Path's oriented box, but ONLY while it is
        // SELECTED. Clicking empty canvas deselects every layer (Move tool) yet
        // leaves `active_idx` pointing here; without this gate the box would
        // linger and read as "still selected" (the reported empty-click bug).
        let idx = self.active_path_layer()?;
        if !self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .layers[idx]
            .selected
        {
            return None;
        }
        let (_, model_t, b) = self.active_path_object()?;
        Some(box_from(model_t, b))
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
            let LayerType::Vector(VectorGeometry::Path(obj)) = &layer.layer_type else {
                continue;
            };
            let Some(inv) = obj.transform.inverse() else {
                continue;
            };
            let p = inv.apply_point(Point::new(cx, cy));
            // A Vector Brush ribbon is an open centerline: hit it as a stroke of
            // its widest half-width, not a filled polygon (whose "inside" of an
            // open path is a near-zero sliver).
            if let Some(brush) = &obj.brush {
                let half = brush.max_half_width();
                if brush.is_visible()
                    && obj.style.fill.is_visible()
                    && crate::core::vector::hittest::stroke_hit(&obj.path, p, half, tol)
                {
                    return Some(idx);
                }
                continue;
            }
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
                        return path_handle_cursor_hint(&bx, h);
                    }
                    return 0;
                }
            }
        }
        let ev = self.tool_event();
        if self.path_layer_hit_at(ev.canvas_x, ev.canvas_y).is_some() {
            return 1;
        }
        // Corel "treat as filled": a selected vector object is grabbable from
        // anywhere inside its bounding box (see `selected_vector_bbox_hit`), so
        // show the Move cursor there too — matching what a press will do.
        if self.selected_vector_bbox_hit(ev.canvas_x, ev.canvas_y) {
            return 1;
        }
        0
    }

    /// True when `(cx,cy)` (canvas space) lies inside the raster bounding box of a
    /// selected, movable Vector layer. Mirrors the Move tool's "treat a selected
    /// vector as filled" grab (see `MoveTool::on_press`) so the Vector Brush — a
    /// thin stroke inside a wide box — can be dragged from anywhere in that box.
    pub fn selected_vector_bbox_hit(&self, cx: f32, cy: f32) -> bool {
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let (x, y) = (cx as i32, cy as i32);
        canvas.layer_stack.layers.iter().any(|layer| {
            layer.selected
                && layer.visible
                && !layer.locked
                && !layer.is_background
                && matches!(layer.layer_type, LayerType::Vector(_))
                && {
                    let lx = x - layer.offset.0;
                    let ly = y - layer.offset.1;
                    lx >= 0 && ly >= 0 && (lx as u32) < layer.width && (ly as u32) < layer.height
                }
        })
    }

    /// Begin an on-canvas Path transform gesture. `(cx, cy)` is the press point in
    /// canvas space (the rotation reference). Folds any pending Move drag into the
    /// model first so the box pivot is the displayed centre and `orig_transform`
    /// is the true undo baseline.
    pub fn path_transform_begin(&mut self, hit: PathBoxHit, cx: f32, cy: f32) {
        // A clean multi-Path selection scales/rotates as one union box.
        if let Some(paths) = self.multi_path_targets() {
            self.begin_union_path_transform(hit, cx, cy, &paths);
            return;
        }
        let Some(idx) = self.active_path_layer() else {
            return;
        };
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let layer = &mut canvas.layer_stack.layers[idx];
        // Normalise any residual offset↔model drift (no-op in the common case).
        crate::core::command_vector::fold_offset_into_model(layer);
        let layer_id = layer.id;
        let LayerType::Vector(VectorGeometry::Path(obj)) = &layer.layer_type else {
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
            canvas_frame: false,
            targets: vec![(layer_id, orig)],
        });
    }

    /// Begin a union scale/rotate over several selected Paths. The gesture builds
    /// a CANVAS-space delta `M` about the union box (identity `orig_transform`,
    /// union AABB as `local_bounds`); every target's new transform is `M ∘ orig_i`
    /// so they move together and each stays an editable vector.
    fn begin_union_path_transform(&mut self, hit: PathBoxHit, cx: f32, cy: f32, paths: &[usize]) {
        let mut targets: Vec<(u32, AffineTransform)> = Vec::with_capacity(paths.len());
        {
            let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
            for &idx in paths {
                let Some(layer) = canvas.layer_stack.layers.get_mut(idx) else {
                    continue;
                };
                // Normalise any residual offset↔model drift so the union box and
                // the committed transforms agree.
                crate::core::command_vector::fold_offset_into_model(layer);
                if let LayerType::Vector(VectorGeometry::Path(obj)) = &layer.layer_type {
                    targets.push((layer.id, obj.transform));
                }
            }
        }
        if targets.len() < 2 {
            return;
        }
        let Some(b) = self.union_canvas_bounds(paths) else {
            return;
        };
        let pivot = Point::new(b.x + b.w * 0.5, b.y + b.h * 0.5);
        let handle = match hit {
            PathBoxHit::Handle(h) => Some(h),
            PathBoxHit::Rotate => None,
        };
        let layer_id = targets[0].0;
        self.edit.path_transform = Some(PathTransformDrag {
            layer_id,
            handle,
            orig_transform: AffineTransform::IDENTITY,
            pending: AffineTransform::IDENTITY,
            local_bounds: b,
            pivot,
            start_cx: cx,
            start_cy: cy,
            changed: false,
            canvas_frame: true,
            targets,
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

    /// Apply the in-progress gesture to `(cx, cy)`. Both the overlay and the GPU
    /// preview use `pending` in the same frame. The cached Path raster remains
    /// untouched until release, avoiding an O(area) CPU raster on every pointer
    /// event; release commits and rasterizes the final editable vector once.
    pub fn path_transform_update(&mut self, cx: f32, cy: f32, shift: bool, alt: bool) {
        // Recompute the gesture target and derive each moved Path's (id, orig,
        // new) transform pair. A single drag has one entry; a union drag maps its
        // canvas-space delta `M` onto every target as `M ∘ orig_i`.
        let moves: Vec<(u32, AffineTransform, AffineTransform)> =
            match self.edit.path_transform.as_mut() {
                Some(d) => match Self::path_transform_target(d, cx, cy, shift, alt) {
                    Some(t) => {
                        d.pending = t;
                        d.changed = true;
                        if d.canvas_frame {
                            d.targets
                                .iter()
                                .map(|(id, orig)| (*id, *orig, t.then(orig)))
                                .collect()
                        } else {
                            // Single: `pending` already IS the object's new transform.
                            vec![(d.layer_id, d.orig_transform, t)]
                        }
                    }
                    None => return,
                },
                None => return,
            };
        // Build one GPU transform preview per moved Path. Destination canvas ->
        // source canvas: a source point displayed under `orig` moves to `new`, so
        // sampling applies `orig * inverse(new)` to the destination coordinate.
        let mut previews: Vec<TransformPreviewUniform> = Vec::with_capacity(moves.len());
        {
            let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
            for (id, orig_t, new_t) in &moves {
                let Some(l) = canvas.layer_stack.layers.iter().find(|l| l.id == *id) else {
                    continue;
                };
                if !matches!(l.layer_type, LayerType::Vector(VectorGeometry::Path(_))) {
                    continue;
                }
                let Some(inv_m) = path_preview_inverse(*orig_t, *new_t) else {
                    continue;
                };
                previews.push(TransformPreviewUniform {
                    layer_id: *id,
                    inv_m,
                    orig_ox: l.offset.0 as f32,
                    orig_oy: l.offset.1 as f32,
                });
            }
        }
        if let Some(gpu) = &mut self.win.gpu {
            gpu.compositor.transform_previews = previews;
        }
        self.request_interactive_recompose();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Finish the gesture: restore the pre-gesture transform(s) so the gateway
    /// captures the correct "before", then commit the final transform as ONE
    /// [`crate::core::command_vector::ChangeVectorTransform`] (a single undo step)
    /// for a single Path, or every target's transform in ONE undo group for a
    /// union drag. A no-op drag (a click on a handle) records nothing.
    pub fn path_transform_finish(&mut self) {
        let Some(drag) = self.edit.path_transform.take() else {
            return;
        };
        if let Some(gpu) = &mut self.win.gpu {
            gpu.compositor.transform_previews.clear();
        }
        // The last composed frame still contains the GPU-transformed cache.
        // Rebuild once from the model even for a no-op release or early exit.
        self.recomposite_visible();
        // Abandon any in-flight worker bake — the final geometry is committed
        // synchronously below, so a late result would be stale.
        self.cancel_path_bake();

        // Multi-Path union: commit every target's `M ∘ orig_i` in one undo group.
        if drag.canvas_frame {
            let finals: Vec<(u32, AffineTransform, AffineTransform)> = drag
                .targets
                .iter()
                .map(|(id, orig)| (*id, *orig, drag.pending.then(orig)))
                .collect();
            let no_op = !drag.changed || finals.iter().all(|(_, orig, fin)| fin == orig);
            let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
            if no_op {
                // Nothing moved — make sure every target's model is its baseline.
                let mut touched = false;
                for (id, orig, _) in &finals {
                    let Some(idx) = canvas.layer_stack.layers.iter().position(|l| l.id == *id)
                    else {
                        continue;
                    };
                    let layer = &mut canvas.layer_stack.layers[idx];
                    if let LayerType::Vector(VectorGeometry::Path(o)) = &layer.layer_type {
                        if o.transform != *orig {
                            let mut o = o.clone();
                            o.transform = *orig;
                            crate::core::command_vector::apply_object_to_layer(layer, o);
                            touched = true;
                        }
                    }
                }
                if touched {
                    self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
                }
                return;
            }
            canvas.begin_undo_group("Transform Paths");
            for (id, orig, fin) in &finals {
                let Some(idx) = canvas.layer_stack.layers.iter().position(|l| l.id == *id) else {
                    continue;
                };
                if !matches!(
                    canvas.layer_stack.layers[idx].layer_type,
                    LayerType::Vector(VectorGeometry::Path(_))
                ) {
                    continue;
                }
                // Rewind this target to its baseline so `execute` records old→new.
                {
                    let layer = &mut canvas.layer_stack.layers[idx];
                    if let LayerType::Vector(VectorGeometry::Path(o)) = &layer.layer_type {
                        let mut o = o.clone();
                        o.transform = *orig;
                        crate::core::command_vector::apply_object_to_layer(layer, o);
                    }
                }
                let _ = canvas.execute(
                    Box::new(crate::core::command_vector::ChangeVectorTransform::new(
                        *id, *fin,
                    )),
                    crate::core::gateway::ChangeKind::LayerStructure,
                );
            }
            canvas.end_undo_group();
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return;
        }

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
            LayerType::Vector(VectorGeometry::Path(_))
        ) {
            return;
        }
        // Commit `pending` — the true target at release, which the throttled live
        // re-raster may not have caught up to in the model yet.
        let final_t = drag.pending;
        if !drag.changed || final_t == drag.orig_transform {
            // Nothing moved — make sure the model is exactly the baseline.
            let model_t = match &canvas.layer_stack.layers[idx].layer_type {
                LayerType::Vector(VectorGeometry::Path(o)) => o.transform,
                _ => return,
            };
            if model_t != drag.orig_transform {
                let layer = &mut canvas.layer_stack.layers[idx];
                if let LayerType::Vector(VectorGeometry::Path(o)) = &layer.layer_type {
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
            if let LayerType::Vector(VectorGeometry::Path(o)) = &layer.layer_type {
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

    #[test]
    fn tall_path_box_corner_cursors_stay_diagonal() {
        let bx = PathBox {
            corners: [(0.0, 0.0), (10.0, 0.0), (0.0, 200.0), (10.0, 200.0)],
            handles: [
                (0.0, 0.0),
                (5.0, 0.0),
                (10.0, 0.0),
                (0.0, 100.0),
                (10.0, 100.0),
                (0.0, 200.0),
                (5.0, 200.0),
                (10.0, 200.0),
            ],
            center: (5.0, 100.0),
        };

        assert_eq!(path_handle_cursor_hint(&bx, TransformHandle::TopLeft), 2);
        assert_eq!(
            path_handle_cursor_hint(&bx, TransformHandle::BottomRight),
            2
        );
        assert_eq!(path_handle_cursor_hint(&bx, TransformHandle::TopRight), 3);
        assert_eq!(path_handle_cursor_hint(&bx, TransformHandle::BottomLeft), 3);
        assert_eq!(
            path_handle_cursor_hint(&bx, TransformHandle::MiddleRight),
            5
        );
        assert_eq!(
            path_handle_cursor_hint(&bx, TransformHandle::BottomCenter),
            4
        );
    }

    #[test]
    fn gpu_path_preview_maps_pending_destination_back_to_original_canvas() {
        let original = AffineTransform::translate(30.0, 20.0).then(&AffineTransform::rotate(0.2));
        let pending = AffineTransform::translate(80.0, 45.0)
            .then(&AffineTransform::rotate(0.8))
            .then(&AffineTransform::scale(1.7, 0.6));
        let m = path_preview_inverse(original, pending).expect("invertible preview");
        let local = Point::new(12.0, 9.0);
        let destination = pending.apply_point(local);
        let source = Point::new(
            m[0] * destination.x + m[1] * destination.y + m[2],
            m[3] * destination.x + m[4] * destination.y + m[5],
        );
        let expected = original.apply_point(local);
        assert!((source.x - expected.x).abs() < 1e-3);
        assert!((source.y - expected.y).abs() < 1e-3);
    }

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
            canvas_frame: false,
            targets: vec![(1, AffineTransform::IDENTITY)],
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
                .position(|l| matches!(l.layer_type, LayerType::Vector(VectorGeometry::Path(_))))
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
        assert!(
            app.jobs.path_bake.is_none() && app.jobs.path_bake_next.is_none(),
            "live Path transform must queue no CPU raster bake"
        );
        app.path_transform_finish();

        let l = find_layer(&app, id);
        assert!(
            matches!(l.layer_type, LayerType::Vector(VectorGeometry::Path(_))),
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
        assert!(matches!(
            l.layer_type,
            LayerType::Vector(VectorGeometry::Path(_))
        ));
        assert_eq!((l.width, l.height), (w0, h0), "undo restored the size");
    }

    /// Deselecting the active Path (e.g. clicking empty canvas) must hide the box
    /// even though `active_idx` still points at it.
    #[test]
    fn deselecting_active_path_hides_box() {
        let (mut app, id) = app_with_active_path();
        assert!(
            app.active_path_transform_box().is_some(),
            "box shows for the selected active path"
        );
        for l in &mut app.docs.documents[0].canvas.layer_stack.layers {
            if l.id == id {
                l.selected = false;
            }
        }
        assert!(
            app.active_path_transform_box().is_none(),
            "clearing the selection hides the on-canvas box"
        );
    }

    /// Build an App with two selected Path layers under the Move tool.
    fn app_with_two_paths() -> (App, u32, u32) {
        use crate::core::canvas::Canvas;
        use crate::core::command_vector::CreatePathLayer;
        use crate::core::gateway::ChangeKind;

        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(400, 400);
        let (id_a, id_b) = {
            let canvas = &mut app.docs.documents[0].canvas;
            canvas
                .execute(
                    Box::new(CreatePathLayer::new(
                        rect_path_object(40.0, 20.0, 100.0, 120.0),
                        "Path A",
                    )),
                    ChangeKind::LayerStructure,
                )
                .unwrap();
            canvas
                .execute(
                    Box::new(CreatePathLayer::new(
                        rect_path_object(40.0, 20.0, 200.0, 200.0),
                        "Path B",
                    )),
                    ChangeKind::LayerStructure,
                )
                .unwrap();
            let paths: Vec<usize> = canvas
                .layer_stack
                .layers
                .iter()
                .enumerate()
                .filter(|(_, l)| matches!(l.layer_type, LayerType::Vector(VectorGeometry::Path(_))))
                .map(|(i, _)| i)
                .collect();
            assert_eq!(paths.len(), 2);
            for l in &mut canvas.layer_stack.layers {
                l.selected = false;
            }
            for &idx in &paths {
                canvas.layer_stack.layers[idx].selected = true;
            }
            canvas.layer_stack.active_idx = paths[0];
            (
                canvas.layer_stack.layers[paths[0]].id,
                canvas.layer_stack.layers[paths[1]].id,
            )
        };
        app.edit.tools.select(crate::tools::ToolId::Move);
        app.edit.view.zoom = 1.0;
        app.edit.view.offset_x = 0.0;
        app.edit.view.offset_y = 0.0;
        (app, id_a, id_b)
    }

    #[test]
    fn union_box_wraps_all_selected_paths() {
        let (app, _a, _b) = app_with_two_paths();
        let bx = app
            .active_path_transform_box()
            .expect("union box shown for a clean multi-Path selection");
        // Paths span (100,120)-(140,140) and (200,200)-(240,220) ⇒ union
        // (100,120)-(240,220).
        assert!((bx.corners[0].0 - 100.0).abs() < 1.0 && (bx.corners[0].1 - 120.0).abs() < 1.0);
        assert!((bx.corners[3].0 - 240.0).abs() < 1.0 && (bx.corners[3].1 - 220.0).abs() < 1.0);
    }

    #[test]
    fn union_drag_scales_every_path_in_one_undo() {
        let (mut app, id_a, id_b) = app_with_two_paths();
        let (wa0, wb0) = (find_layer(&app, id_a).width, find_layer(&app, id_b).width);
        // Grab the union bottom-right handle and drag it out to double the box
        // about the fixed top-left corner (100,120).
        let (hx, hy) = {
            let bx = app.active_path_transform_box().unwrap();
            bx.handles[7]
        };
        app.path_transform_begin(PathBoxHit::Handle(TransformHandle::BottomRight), hx, hy);
        app.path_transform_update(380.0, 320.0, false, false);
        app.path_transform_finish();

        let la = find_layer(&app, id_a);
        let lb = find_layer(&app, id_b);
        assert!(
            matches!(la.layer_type, LayerType::Vector(VectorGeometry::Path(_)))
                && matches!(lb.layer_type, LayerType::Vector(VectorGeometry::Path(_))),
            "both stay editable vectors"
        );
        assert!(
            la.width > wa0 && lb.width > wb0,
            "both paths scaled up: A {}→{}, B {}→{}",
            wa0,
            la.width,
            wb0,
            lb.width
        );

        // A SINGLE undo reverts the whole union transform (one undo group).
        app.docs.documents[0].canvas.undo().expect("undo");
        assert_eq!(find_layer(&app, id_a).width, wa0, "undo restored path A");
        assert_eq!(find_layer(&app, id_b).width, wb0, "undo restored path B");
    }

    #[test]
    fn mixed_selection_falls_back_to_single_box() {
        let (mut app, _a, id_b) = app_with_two_paths();
        // Add a plain raster layer and select it alongside the paths → no longer a
        // clean Path-only selection, so the union box must not engage.
        {
            let canvas = &mut app.docs.documents[0].canvas;
            let idx = canvas.layer_stack.add_layer(400, 400);
            canvas.layer_stack.layers[idx].selected = true;
            // Keep an active Path so the single-box path still has a candidate.
            let b_idx = canvas
                .layer_stack
                .layers
                .iter()
                .position(|l| l.id == id_b)
                .unwrap();
            canvas.layer_stack.active_idx = b_idx;
        }
        assert!(
            app.multi_path_targets().is_none(),
            "a mixed selection is not a clean multi-Path target set"
        );
    }
}

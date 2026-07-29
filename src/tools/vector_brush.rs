#![allow(dead_code)]
//! Vector Brush — freehand drawing that commits an EDITABLE vector stroke, not
//! pixels (Phase 6B, plan GIAI ĐOẠN 6B).
//!
//! This is a distinct tool from the pixel Brush (`tools::brush`): a drag captures
//! a pressure-aware centerline which, on release, becomes a Path layer carrying a
//! [`BrushStroke`](crate::core::vector::brush::BrushStroke) appearance — an open
//! Bézier centerline plus a width profile sampled along arc length. The Node tool
//! then edits the centerline and Expand Stroke bakes it to a closed outline.
//!
//! Gestures:
//!   - Press + drag = draw. A stabilizer (position smoothing) is applied live so
//!     the preview is steady; the geometry is committed once, on release.
//!   - Release commits the stroke (handled at App level so the new layer is
//!     undoable — see `App::commit_vector_brush_stroke`).
//!   - Esc / `on_cancel` discards the in-progress stroke.
//!
//! Width per sample folds in pen pressure and, optionally, drawing speed (faster
//! ⇒ thinner), giving a calligraphic feel even with a mouse. A minimum ratio
//! keeps thin sections from vanishing.

use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::geometry::Point;
use crate::core::vector::affine::AffineTransform;
use crate::core::vector::brush::{self, BrushStroke, WidthStop};
use crate::core::vector::color::ColorValue;
use crate::core::vector::object::VectorObjectData;
use crate::core::vector::style::{LineCap, VectorStyle};

/// Minimum spacing (canvas px) between kept raw samples — denser input is merged.
const MIN_POINT_DIST: f32 = 1.5;
/// RDP simplify tolerance (canvas px) for the committed centerline.
const SIMPLIFY_TOL: f32 = 0.6;
/// Reference speed (canvas px / event) at which the velocity taper reaches its
/// thinnest. Tuned for a natural feel at typical drawing rates.
const SPEED_REF: f32 = 22.0;

pub struct VectorBrushTool {
    /// Base (widest) stroke width, canvas px.
    pub width: f32,
    /// Position stabilizer strength in `[0,1]`: 0 = raw, higher = steadier lag.
    pub smoothing: f32,
    /// Map pen pressure to width.
    pub pressure: bool,
    /// Thin the stroke where the pointer moves fast (calligraphic taper).
    pub velocity: bool,
    /// Thinnest width as a fraction of `width` (keeps thin runs from vanishing).
    pub min_ratio: f32,
    /// End-cap for Expand Stroke (the live ribbon is always round).
    pub cap: LineCap,

    /// Captured `(canvas point, width ratio)` samples for the active stroke.
    samples: Vec<(Point, f32)>,
    /// Stabilizer state: the smoothed "pen" position that lags the cursor.
    pen: Point,
    /// Smoothed speed (canvas px/event) for the velocity taper.
    speed: f32,
    /// Whether a stroke is currently being drawn.
    drawing: bool,
}

impl VectorBrushTool {
    pub fn new() -> Self {
        Self {
            width: 12.0,
            smoothing: 0.4,
            pressure: true,
            velocity: true,
            min_ratio: 0.15,
            cap: LineCap::Round,
            samples: Vec::new(),
            pen: Point::new(0.0, 0.0),
            speed: 0.0,
            drawing: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn is_drawing(&self) -> bool {
        self.drawing
    }

    /// Flattened centerline points (canvas space) for the live overlay.
    pub fn preview_points(&self) -> Vec<(f32, f32)> {
        self.samples.iter().map(|(p, _)| (p.x, p.y)).collect()
    }

    /// Width ratio in `[min_ratio, 1]` for a sample, folding in pressure and the
    /// current smoothed speed per the tool's toggles.
    fn ratio(&self, pressure: f32) -> f32 {
        let mut raw = 1.0;
        if self.pressure {
            raw *= pressure.clamp(0.0, 1.0);
        }
        if self.velocity {
            // Fast → thin: falls from 1 toward ~0 as speed passes SPEED_REF.
            let v = (self.speed / SPEED_REF).clamp(0.0, 1.0);
            raw *= 1.0 - 0.85 * v;
        }
        self.min_ratio + (1.0 - self.min_ratio) * raw.clamp(0.0, 1.0)
    }

    fn begin(&mut self, p: Point, pressure: f32) {
        self.samples.clear();
        self.pen = p;
        self.speed = 0.0;
        self.drawing = true;
        let r = self.ratio(pressure);
        self.samples.push((p, r));
    }

    fn extend(&mut self, target: Point, pressure: f32) {
        // Stabilizer: the pen chases the cursor, lagging by `smoothing`.
        let follow = (1.0 - self.smoothing.clamp(0.0, 0.95)).max(0.05);
        let stepped = Point::new(
            self.pen.x + (target.x - self.pen.x) * follow,
            self.pen.y + (target.y - self.pen.y) * follow,
        );
        // Smoothed speed drives the velocity taper.
        let inst = self.pen.distance_to(stepped);
        self.speed = self.speed * 0.6 + inst * 0.4;
        self.pen = stepped;
        let r = self.ratio(pressure);
        // Merge samples closer than the capture spacing (ratio already blended in
        // the model, but this keeps the vector small).
        if let Some(&(last, _)) = self.samples.last() {
            if last.distance_to(self.pen) < MIN_POINT_DIST {
                if let Some(slot) = self.samples.last_mut() {
                    slot.1 = 0.5 * (slot.1 + r);
                }
                return;
            }
        }
        self.samples.push((self.pen, r));
    }

    /// Catch the stabilizer up to the final cursor position so a heavily-smoothed
    /// stroke still reaches where the user released.
    fn finish(&mut self, target: Point, pressure: f32) {
        if self.drawing {
            let r = self.ratio(pressure);
            if self
                .samples
                .last()
                .map(|&(p, _)| p.distance_to(target) >= MIN_POINT_DIST)
                .unwrap_or(true)
            {
                self.samples.push((target, r));
            }
        }
        self.drawing = false;
    }

    /// Build the editable vector object from the captured stroke, clearing it.
    /// `None` when the stroke is too short to make a segment. Geometry is in
    /// canvas space = layer space for the single implicit page (Mục 3.4 / 3.13),
    /// so the object starts at the identity transform, matching the Pen tool.
    pub fn take_stroke_object(&mut self, fg_color: [u8; 4]) -> Option<VectorObjectData> {
        let samples = std::mem::take(&mut self.samples);
        self.drawing = false;
        let (path, profile) = brush::build_centerline(&samples, MIN_POINT_DIST, SIMPLIFY_TOL)?;
        let profile = simplify_uniform(profile);
        let brush = BrushStroke {
            base_width: self.width.max(0.1),
            profile,
            cap: self.cap,
        };
        let style = VectorStyle::filled(ColorValue::from_rgba8(fg_color));
        Some(VectorObjectData::new_brush(
            path,
            style,
            AffineTransform::IDENTITY,
            brush,
        ))
    }

    pub fn clear(&mut self) {
        self.samples.clear();
        self.drawing = false;
        self.speed = 0.0;
    }
}

/// Collapse a profile whose widths are all effectively equal into an empty
/// (uniform) profile — a constant-width stroke needs no stored samples and
/// rasterizes on the fast path.
fn simplify_uniform(profile: Vec<WidthStop>) -> Vec<WidthStop> {
    if profile.is_empty() {
        return profile;
    }
    let first = profile[0].width;
    if profile.iter().all(|s| (s.width - first).abs() < 1e-3) {
        Vec::new()
    } else {
        profile
    }
}

impl Default for VectorBrushTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for VectorBrushTool {
    fn id(&self) -> &'static str {
        "vector_brush"
    }
    fn name(&self) -> &str {
        "Vector Brush"
    }
    fn icon(&self) -> &'static str {
        "vector"
    }
    fn shortcut(&self) -> Option<char> {
        // Shares the 'N' key group with nothing else; the toolbar tooltip owns it.
        None
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::VectorBrush
    }
    fn cursor_size(&self) -> f32 {
        self.width
    }

    fn on_press(&mut self, event: PointerEvent, _ctx: &mut ToolCtx) -> ToolResponse {
        self.begin(Point::new(event.canvas_x, event.canvas_y), event.pressure);
        ToolResponse::redraw()
    }

    fn on_drag(
        &mut self,
        event: PointerEvent,
        _prev: &PointerEvent,
        _ctx: &mut ToolCtx,
    ) -> ToolResponse {
        if self.drawing {
            self.extend(Point::new(event.canvas_x, event.canvas_y), event.pressure);
        }
        ToolResponse::redraw()
    }

    fn on_release(&mut self, event: PointerEvent, _ctx: &mut ToolCtx) -> ToolResponse {
        // The App intercepts release to commit the stroke as a Path layer; here we
        // only catch the stabilizer up to the final cursor position.
        self.finish(Point::new(event.canvas_x, event.canvas_y), event.pressure);
        ToolResponse::redraw()
    }

    fn on_cancel(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::document::{Document, DocumentId};

    fn ctx(doc: &mut Document) -> ToolCtx<'_> {
        ToolCtx::new(doc, [0, 0, 0, 255], [255, 255, 255, 255], 1.0, 0.0, 0.0)
    }

    fn drag(t: &mut VectorBrushTool, c: &mut ToolCtx, pts: &[(f32, f32)]) {
        let first = PointerEvent::new(pts[0].0, pts[0].1);
        t.on_press(first, c);
        let mut prev = first;
        for &(x, y) in &pts[1..] {
            let e = PointerEvent::new(x, y);
            t.on_drag(e, &prev, c);
            prev = e;
        }
        t.on_release(prev, c);
    }

    #[test]
    fn stroke_commits_a_brush_object() {
        let mut t = VectorBrushTool::new();
        // No smoothing so the samples land where drawn (deterministic test).
        t.smoothing = 0.0;
        t.pressure = false;
        t.velocity = false;
        let mut doc = Document::new(DocumentId(1), 200, 200);
        {
            let mut c = ctx(&mut doc);
            drag(
                &mut t,
                &mut c,
                &[(10.0, 100.0), (60.0, 60.0), (110.0, 100.0), (160.0, 60.0)],
            );
        }
        let obj = t.take_stroke_object([20, 40, 200, 255]).expect("object");
        assert!(obj.is_brush(), "commits a Vector Brush object");
        assert!(!obj.path.contours[0].closed, "centerline is open");
        assert!(obj.style.fill.is_visible(), "painted with the fg colour");
        // Uniform (pressure/velocity off) → empty profile.
        assert!(obj.brush.as_ref().unwrap().profile.is_empty());
        assert!(t.is_empty(), "cleared after commit");
    }

    #[test]
    fn pressure_produces_a_width_profile() {
        let mut t = VectorBrushTool::new();
        t.smoothing = 0.0;
        t.pressure = true;
        t.velocity = false;
        let mut doc = Document::new(DocumentId(1), 200, 200);
        // Feed varying pressure across the stroke.
        let press = PointerEvent {
            pressure: 0.2,
            ..PointerEvent::new(10.0, 50.0)
        };
        t.on_press(press, &mut ctx(&mut doc));
        let mut prev = press;
        for (i, x) in (30..=150).step_by(20).enumerate() {
            let e = PointerEvent {
                pressure: 0.2 + 0.1 * i as f32,
                ..PointerEvent::new(x as f32, 50.0)
            };
            t.on_drag(e, &prev, &mut ctx(&mut doc));
            prev = e;
        }
        t.on_release(prev, &mut ctx(&mut doc));
        let obj = t.take_stroke_object([0, 0, 0, 255]).expect("object");
        assert!(
            !obj.brush.as_ref().unwrap().profile.is_empty(),
            "varying pressure keeps a non-trivial width profile"
        );
    }

    #[test]
    fn short_tap_makes_no_object() {
        let mut t = VectorBrushTool::new();
        let mut doc = Document::new(DocumentId(1), 100, 100);
        let e = PointerEvent::new(50.0, 50.0);
        t.on_press(e, &mut ctx(&mut doc));
        t.on_release(e, &mut ctx(&mut doc));
        assert!(t.take_stroke_object([0, 0, 0, 255]).is_none());
    }

    #[test]
    fn cancel_discards_stroke() {
        let mut t = VectorBrushTool::new();
        let mut doc = Document::new(DocumentId(1), 100, 100);
        drag(
            &mut t,
            &mut ctx(&mut doc),
            &[(10.0, 10.0), (40.0, 40.0), (70.0, 10.0)],
        );
        assert!(!t.is_empty());
        t.on_cancel();
        assert!(t.is_empty());
    }
}

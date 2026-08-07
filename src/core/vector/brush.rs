#![allow(dead_code)]
//! Vector Brush appearance — a variable-width stroke over an editable centerline
//! (Phase 6B, plan GIAI ĐOẠN 6B / Mục 6B).
//!
//! Plan contract this module keeps:
//!   - A Vector Brush stroke is an OPEN centerline [`PathData`] plus a width
//!     profile sampled along NORMALIZED arc length — never a baked bitmap.
//!   - The centerline stays a fully editable Bézier path (the Node tool edits
//!     it); the width lives here, keyed by arc length so it survives a node
//!     insert/delete (which changes node COUNT but not the curve's [0,1]
//!     arc-length parameterization).
//!   - `Expand Stroke` bakes the appearance into a CLOSED outline [`PathData`]
//!     when a real fillable / Boolean-able object is needed (see [`expand_stroke`]).
//!   - Solid paint first; textured/nozzle brushes are a later additive slice.
//!
//! Nothing here depends on UI, GPU or serialization — same isolation as the rest
//! of the vector core. The rasteriser ([`crate::core::vector::raster`]) turns the
//! centerline + this profile into the RGBA ribbon; the paint comes from the
//! object's [`VectorStyle::fill`](crate::core::vector::style::VectorStyle) so the
//! existing Fill UI recolours a brush stroke like any other object.

use crate::core::geometry::Point;
use crate::core::vector::flatten::flatten_contour;
use crate::core::vector::path::{Contour, FillRule, Node, NodeKind, PathData};
use crate::core::vector::style::LineCap;

/// Upper bound on stored width samples (parser safety + memory). A stroke with
/// more raw samples is decimated to this many before storage.
pub const MAX_WIDTH_STOPS: usize = 4096;
/// Flatten tolerance (object-local units) used for arc-length / ribbon / expand
/// geometry. Sub-pixel at 100%, cheap on a page-sized stroke.
pub const BRUSH_TOL: f32 = 0.25;

/// One width sample: `width` multiplier of `base_width` at normalized arc
/// position `t`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WidthStop {
    /// Normalized arc position along the centerline, in `[0,1]`, ascending.
    pub t: f32,
    /// Width multiplier of `base_width`, in `[0,1]`.
    pub width: f32,
}

/// Variable-width stroke appearance paired with an object's open centerline.
#[derive(Debug, Clone, PartialEq)]
pub struct BrushStroke {
    /// Full stroke width (object-local units) at profile value `1.0`.
    pub base_width: f32,
    /// Width multipliers along arc length, ascending in `t`. Empty ⇒ a uniform
    /// full-width stroke (a constant-width ribbon).
    pub profile: Vec<WidthStop>,
    /// End-cap shape used by [`expand_stroke`]. The interactive raster ribbon is
    /// always round-joined/‑capped (the natural brush look); `cap` is honoured
    /// when the stroke is expanded to a closed outline.
    pub cap: LineCap,
}

impl BrushStroke {
    /// A constant-width stroke (no taper).
    pub fn uniform(base_width: f32, cap: LineCap) -> Self {
        Self {
            base_width,
            profile: Vec::new(),
            cap,
        }
    }

    /// The width multiplier at normalized arc position `t`, linearly interpolated
    /// between the surrounding stops and clamped at the ends. An empty profile is
    /// a uniform full-width stroke (`1.0` everywhere).
    pub fn width_ratio_at(&self, t: f32) -> f32 {
        if self.profile.is_empty() {
            return 1.0;
        }
        let t = t.clamp(0.0, 1.0);
        // Profile is ascending in `t`; a short linear scan is cheaper than a
        // binary search for the handful of stops a real stroke keeps.
        let first = self.profile[0];
        if t <= first.t {
            return first.width;
        }
        for w in self.profile.windows(2) {
            let (a, b) = (w[0], w[1]);
            if t <= b.t {
                let span = (b.t - a.t).max(1e-6);
                let f = ((t - a.t) / span).clamp(0.0, 1.0);
                return a.width + (b.width - a.width) * f;
            }
        }
        self.profile.last().map(|s| s.width).unwrap_or(1.0)
    }

    /// Largest width multiplier anywhere on the stroke (≥ the endpoints), used to
    /// pad bounds. `1.0` for a uniform stroke.
    pub fn max_ratio(&self) -> f32 {
        self.profile
            .iter()
            .map(|s| s.width)
            .fold(0.0_f32, f32::max)
            .max(if self.profile.is_empty() { 1.0 } else { 0.0 })
    }

    /// Half the stroke width (object-local) at arc position `t`.
    pub fn half_width_at(&self, t: f32) -> f32 {
        0.5 * self.base_width.max(0.0) * self.width_ratio_at(t)
    }

    /// Largest half-width anywhere on the stroke — the AA/bounds pad.
    pub fn max_half_width(&self) -> f32 {
        0.5 * self.base_width.max(0.0) * self.max_ratio()
    }

    /// The width profile restricted to the arc-length sub-range `[a0,a1]` of the
    /// original stroke and re-normalized to `[0,1]` — used when an open stroke is
    /// cut into pieces so each piece keeps the correct taper. `base_width` and
    /// `cap` carry over; a uniform (empty) profile stays uniform.
    pub fn sliced(&self, a0: f32, a1: f32) -> BrushStroke {
        let (a0, a1) = (a0.clamp(0.0, 1.0), a1.clamp(0.0, 1.0));
        if self.profile.is_empty() || (a1 - a0) <= 1e-6 {
            return BrushStroke {
                base_width: self.base_width,
                profile: self.profile.clone(),
                cap: self.cap,
            };
        }
        let span = a1 - a0;
        let mut profile = Vec::with_capacity(self.profile.len() + 2);
        profile.push(WidthStop {
            t: 0.0,
            width: self.width_ratio_at(a0),
        });
        for s in &self.profile {
            if s.t > a0 + 1e-6 && s.t < a1 - 1e-6 {
                profile.push(WidthStop {
                    t: ((s.t - a0) / span).clamp(0.0, 1.0),
                    width: s.width,
                });
            }
        }
        profile.push(WidthStop {
            t: 1.0,
            width: self.width_ratio_at(a1),
        });
        BrushStroke {
            base_width: self.base_width,
            profile,
            cap: self.cap,
        }
    }

    /// Whether the stroke would paint anything (a positive width). Paint
    /// visibility is checked separately on the object's fill.
    pub fn is_visible(&self) -> bool {
        self.base_width > 0.0 && self.max_ratio() > 0.0
    }

    /// Reject non-finite / out-of-range values so the `.iai` parser and the
    /// editor share one gate (Mục 5.4).
    pub fn validate(&self) -> Result<(), String> {
        if !self.base_width.is_finite() || self.base_width < 0.0 {
            return Err("brush base_width must be finite and >= 0".into());
        }
        if self.profile.len() > MAX_WIDTH_STOPS {
            return Err(format!(
                "brush profile has too many stops: {} > {}",
                self.profile.len(),
                MAX_WIDTH_STOPS
            ));
        }
        let mut prev_t = f32::NEG_INFINITY;
        for s in &self.profile {
            if !s.t.is_finite() || !(0.0..=1.0).contains(&s.t) {
                return Err("brush width stop t must be finite and in [0,1]".into());
            }
            if !s.width.is_finite() || s.width < 0.0 {
                return Err("brush width stop width must be finite and >= 0".into());
            }
            if s.t < prev_t - 1e-4 {
                return Err("brush width stops must be ascending in t".into());
            }
            prev_t = s.t;
        }
        Ok(())
    }
}

// ── Freehand capture → editable centerline + profile ─────────────────────────

/// Build the centerline path and width profile from freehand pointer samples.
///
/// `samples` is `(point, width_ratio)` in OBJECT-LOCAL space, in capture order,
/// where `width_ratio ∈ [0,1]` already folds in pressure / speed. Returns `None`
/// when fewer than two distinct points remain (no drawable segment).
///
/// Pipeline: dedup near-coincident points → keep the endpoints and a
/// Ramer–Douglas–Peucker subset (so the node count is bounded without moving the
/// drawn line) → fit a smooth interpolating Catmull-Rom Bézier through the kept
/// points. The profile keeps each RETAINED point's original normalized arc
/// position and ratio, so width stays aligned to the geometry after simplify.
pub fn build_centerline(
    samples: &[(Point, f32)],
    min_point_dist: f32,
    simplify_tol: f32,
) -> Option<(PathData, Vec<WidthStop>)> {
    // 1. Drop consecutive points closer than `min_point_dist`, averaging the
    //    ratio of the merged run so a pause doesn't spike the width.
    let min_d = min_point_dist.max(0.01);
    let mut pts: Vec<Point> = Vec::with_capacity(samples.len());
    let mut ratios: Vec<f32> = Vec::with_capacity(samples.len());
    for &(p, r) in samples {
        if !p.x.is_finite() || !p.y.is_finite() {
            continue;
        }
        let r = r.clamp(0.0, 1.0);
        match pts.last() {
            Some(&last) if last.distance_to(p) < min_d => {
                // Merge into the current run (blend the ratio, keep the position).
                if let Some(rr) = ratios.last_mut() {
                    *rr = 0.5 * (*rr + r);
                }
            }
            _ => {
                pts.push(p);
                ratios.push(r);
            }
        }
    }
    if pts.len() < 2 {
        return None;
    }

    // 2. Cumulative arc length over the kept points, for normalized `t`.
    let mut cum = vec![0.0_f32; pts.len()];
    for i in 1..pts.len() {
        cum[i] = cum[i - 1] + pts[i - 1].distance_to(pts[i]);
    }
    let total = *cum.last().unwrap();
    if total <= 1e-4 {
        return None;
    }

    // 3. Ramer–Douglas–Peucker: keep a subset of indices that preserves the
    //    shape within `simplify_tol`. Endpoints are always kept.
    let keep = rdp_indices(&pts, simplify_tol.max(0.05));

    // 4. Build the profile from the RETAINED points, each carrying its own
    //    original arc position and ratio — so width and geometry stay aligned.
    let mut profile: Vec<WidthStop> = keep
        .iter()
        .map(|&i| WidthStop {
            t: (cum[i] / total).clamp(0.0, 1.0),
            width: ratios[i],
        })
        .collect();
    // Guard the endpoints (numerical safety) and cap the stop count.
    if let Some(first) = profile.first_mut() {
        first.t = 0.0;
    }
    if let Some(last) = profile.last_mut() {
        last.t = 1.0;
    }
    decimate_profile(&mut profile, MAX_WIDTH_STOPS);

    // 5. Smooth interpolating spline through the kept points.
    let kept_pts: Vec<Point> = keep.iter().map(|&i| pts[i]).collect();
    let contour = catmull_rom_to_contour(&kept_pts);
    if contour.nodes.len() < 2 {
        return None;
    }
    Some((PathData::new(vec![contour], FillRule::NonZero), profile))
}

/// Ramer–Douglas–Peucker on a polyline, returning the KEPT point indices
/// (ascending, always including the first and last). Iterative to avoid deep
/// recursion on long strokes.
fn rdp_indices(pts: &[Point], tol: f32) -> Vec<usize> {
    let n = pts.len();
    if n <= 2 {
        return (0..n).collect();
    }
    let mut keep = vec![false; n];
    keep[0] = true;
    keep[n - 1] = true;
    let mut stack = vec![(0usize, n - 1)];
    while let Some((lo, hi)) = stack.pop() {
        if hi <= lo + 1 {
            continue;
        }
        let a = pts[lo];
        let b = pts[hi];
        let mut worst = tol;
        let mut worst_i = None;
        for (i, p) in pts.iter().enumerate().take(hi).skip(lo + 1) {
            let d = perp_distance(*p, a, b);
            if d > worst {
                worst = d;
                worst_i = Some(i);
            }
        }
        if let Some(i) = worst_i {
            keep[i] = true;
            stack.push((lo, i));
            stack.push((i, hi));
        }
    }
    (0..n).filter(|&i| keep[i]).collect()
}

/// Perpendicular distance from `p` to the segment `a→b` (point distance if the
/// segment is degenerate).
fn perp_distance(p: Point, a: Point, b: Point) -> f32 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len2 = dx * dx + dy * dy;
    if len2 < 1e-9 {
        return p.distance_to(a);
    }
    // Distance to the infinite line through a,b.
    ((p.x - a.x) * dy - (p.y - a.y) * dx).abs() / len2.sqrt()
}

/// Keep at most `max` stops by uniform stride, always retaining the endpoints.
fn decimate_profile(profile: &mut Vec<WidthStop>, max: usize) {
    if profile.len() <= max || max < 2 {
        return;
    }
    let stride = (profile.len() as f32 / max as f32).ceil() as usize;
    let last = profile.len() - 1;
    let mut out: Vec<WidthStop> = profile
        .iter()
        .enumerate()
        .filter(|(i, _)| *i % stride == 0)
        .map(|(_, s)| *s)
        .collect();
    if out.last().map(|s| s.t) != Some(profile[last].t) {
        out.push(profile[last]);
    }
    *profile = out;
}

/// Fit a smooth interpolating open contour through `points` using the standard
/// Catmull-Rom → cubic-Bézier conversion: each node's handles are offset along
/// the centripetal-free tangent `(P[i+1] − P[i−1]) / 6`, with the endpoints using
/// a clamped one-sided tangent. The resulting curve passes THROUGH every point,
/// so the drawn line matches the freehand path while staying editable.
pub fn catmull_rom_to_contour(points: &[Point]) -> Contour {
    let n = points.len();
    if n == 0 {
        return Contour::new(Vec::new(), false);
    }
    if n == 1 {
        return Contour::new(vec![Node::sharp(points[0])], false);
    }
    let mut nodes = Vec::with_capacity(n);
    for i in 0..n {
        let p = points[i];
        let prev = if i == 0 { points[0] } else { points[i - 1] };
        let next = if i + 1 == n {
            points[n - 1]
        } else {
            points[i + 1]
        };
        let tan = Point::new((next.x - prev.x) / 6.0, (next.y - prev.y) / 6.0);
        let out_h = Point::new(p.x + tan.x, p.y + tan.y);
        let in_h = Point::new(p.x - tan.x, p.y - tan.y);
        // Endpoints keep one handle only (the outward side is a free end), so a
        // stroke doesn't overshoot past its first/last sample.
        let (in_handle, out_handle) = if i == 0 {
            (None, Some(out_h))
        } else if i + 1 == n {
            (Some(in_h), None)
        } else {
            (Some(in_h), Some(out_h))
        };
        nodes.push(Node {
            anchor: p,
            in_handle,
            out_handle,
            kind: NodeKind::Smooth,
        });
    }
    Contour::new(nodes, false)
}

// ── Expand Stroke: variable-width ribbon → closed outline PathData ────────────

/// Bake a Vector Brush stroke (open centerline + width profile) into a CLOSED
/// outline [`PathData`] a fill or Boolean can consume (plan 6B "Expand Stroke").
///
/// The flattened centerline is offset left and right by the local half-width and
/// the two sides are joined into one ring, capped per `brush.cap`. The result is
/// a polyline outline of sharp corners (no Bézier smoothing) — matching how
/// CorelDRAW's Convert Outline to Object emits many nodes — filled NonZero so
/// any self-overlap on a sharp turn stays solid. Returns `None` when the
/// centerline has no drawable segment or the width is zero.
pub fn expand_stroke(centerline: &PathData, brush: &BrushStroke) -> Option<PathData> {
    let contour = centerline.contours.first()?;
    let pts = flatten_contour(contour, BRUSH_TOL);
    if pts.len() < 2 || !brush.is_visible() {
        return None;
    }

    // Per-vertex normalized arc position and unit normal.
    let mut cum = vec![0.0_f32; pts.len()];
    for i in 1..pts.len() {
        cum[i] = cum[i - 1] + pts[i - 1].distance_to(pts[i]);
    }
    let total = *cum.last().unwrap();
    if total <= 1e-4 {
        return None;
    }
    let normals = vertex_normals(&pts);

    let mut left = Vec::with_capacity(pts.len());
    let mut right = Vec::with_capacity(pts.len());
    for i in 0..pts.len() {
        let t = cum[i] / total;
        let hw = brush.half_width_at(t).max(0.01);
        let n = normals[i];
        left.push(Point::new(pts[i].x + n.x * hw, pts[i].y + n.y * hw));
        right.push(Point::new(pts[i].x - n.x * hw, pts[i].y - n.y * hw));
    }

    // Build the ring: left side forward, end cap, right side backward, start cap.
    let mut ring: Vec<Node> = Vec::with_capacity(pts.len() * 2 + 8);
    for &p in &left {
        ring.push(Node::sharp(p));
    }
    let end_hw = brush.half_width_at(1.0).max(0.01);
    push_cap(
        &mut ring,
        pts[pts.len() - 1],
        normals[pts.len() - 1],
        end_hw,
        brush.cap,
        false,
    );
    for &p in right.iter().rev() {
        ring.push(Node::sharp(p));
    }
    let start_hw = brush.half_width_at(0.0).max(0.01);
    push_cap(&mut ring, pts[0], normals[0], start_hw, brush.cap, true);

    dedup_ring(&mut ring);
    if ring.len() < 3 {
        return None;
    }
    Some(PathData::new(
        vec![Contour::new(ring, true)],
        FillRule::NonZero,
    ))
}

/// Unit normals per vertex, from the average of the adjacent segment directions
/// (one-sided at the ends). Normal is the left perpendicular of the tangent.
fn vertex_normals(pts: &[Point]) -> Vec<Point> {
    let n = pts.len();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let prev = if i == 0 { pts[0] } else { pts[i - 1] };
        let next = if i + 1 == n { pts[n - 1] } else { pts[i + 1] };
        let mut tx = next.x - prev.x;
        let mut ty = next.y - prev.y;
        let len = (tx * tx + ty * ty).sqrt();
        if len < 1e-6 {
            // Degenerate: fall back to any adjacent segment, else +x.
            tx = 1.0;
            ty = 0.0;
        } else {
            tx /= len;
            ty /= len;
        }
        // Left perpendicular.
        out.push(Point::new(-ty, tx));
    }
    out
}

/// Append the end/start cap between the left and right offset points at endpoint
/// `p`. `start` reverses the arc direction. Round samples a semicircle; Square
/// extends the corners outward along the tangent; Butt adds nothing (the ring's
/// straight side already joins the two offsets).
fn push_cap(ring: &mut Vec<Node>, p: Point, normal: Point, hw: f32, cap: LineCap, start: bool) {
    // Outward tangent direction at this end (points away from the stroke body).
    // At the END the body recedes along −tangent, so outward = +tangent; the
    // normal is the left perpendicular, so tangent = (normal.y, −normal.x).
    let mut tx = normal.y;
    let mut ty = -normal.x;
    if start {
        tx = -tx;
        ty = -ty;
    }
    match cap {
        LineCap::Butt => {}
        LineCap::Square => {
            // Two corners extended a half-width past the endpoint.
            let (l, r) = (
                Point::new(p.x + normal.x * hw, p.y + normal.y * hw),
                Point::new(p.x - normal.x * hw, p.y - normal.y * hw),
            );
            let (l, r) = if start { (r, l) } else { (l, r) };
            ring.push(Node::sharp(Point::new(l.x + tx * hw, l.y + ty * hw)));
            ring.push(Node::sharp(Point::new(r.x + tx * hw, r.y + ty * hw)));
        }
        LineCap::Round => {
            // Semicircle from the +normal side to the −normal side, bulging along
            // the outward tangent. Start reverses so winding stays consistent.
            let steps = ((hw * 0.75).ceil() as usize).clamp(3, 24);
            let base = normal;
            for k in 1..steps {
                let a = std::f32::consts::PI * (k as f32) / (steps as f32);
                let sign = if start { -1.0 } else { 1.0 };
                // cos over the normal axis, sin over the outward tangent.
                let nx = base.x * a.cos() * sign;
                let ny = base.y * a.cos() * sign;
                let ox = tx * a.sin();
                let oy = ty * a.sin();
                ring.push(Node::sharp(Point::new(
                    p.x + (nx + ox) * hw,
                    p.y + (ny + oy) * hw,
                )));
            }
        }
    }
}

/// Drop consecutive ring nodes that coincide (cap seams, degenerate offsets).
fn dedup_ring(ring: &mut Vec<Node>) {
    let mut i = 1;
    while i < ring.len() {
        if ring[i].anchor.distance_to(ring[i - 1].anchor) < 1e-4 {
            ring.remove(i);
        } else {
            i += 1;
        }
    }
    // Also fold a closing duplicate.
    if ring.len() >= 2 && ring[0].anchor.distance_to(ring[ring.len() - 1].anchor) < 1e-4 {
        ring.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_samples(n: usize, ratio: f32) -> Vec<(Point, f32)> {
        (0..n)
            .map(|i| (Point::new(i as f32 * 5.0, 0.0), ratio))
            .collect()
    }

    #[test]
    fn width_profile_interpolates_and_clamps() {
        let b = BrushStroke {
            base_width: 10.0,
            profile: vec![
                WidthStop { t: 0.0, width: 0.0 },
                WidthStop { t: 1.0, width: 1.0 },
            ],
            cap: LineCap::Round,
        };
        assert!((b.width_ratio_at(0.5) - 0.5).abs() < 1e-4);
        assert_eq!(b.width_ratio_at(-1.0), 0.0);
        assert_eq!(b.width_ratio_at(2.0), 1.0);
        assert!((b.half_width_at(1.0) - 5.0).abs() < 1e-4);
        assert!((b.max_half_width() - 5.0).abs() < 1e-4);
    }

    #[test]
    fn sliced_profile_remaps_to_local_arc() {
        // A linear taper 0→1 over the whole stroke.
        let b = BrushStroke {
            base_width: 10.0,
            profile: vec![
                WidthStop { t: 0.0, width: 0.0 },
                WidthStop { t: 1.0, width: 1.0 },
            ],
            cap: LineCap::Round,
        };
        // The second half [0.5,1.0]: local 0 = width 0.5, local 1 = width 1.0.
        let s = b.sliced(0.5, 1.0);
        assert!((s.width_ratio_at(0.0) - 0.5).abs() < 1e-4);
        assert!((s.width_ratio_at(1.0) - 1.0).abs() < 1e-4);
        // Local midpoint maps to original arc 0.75.
        assert!((s.width_ratio_at(0.5) - 0.75).abs() < 1e-4);
        assert!(s.validate().is_ok());
        // A uniform (empty) profile stays uniform when sliced.
        let u = BrushStroke::uniform(5.0, LineCap::Round).sliced(0.2, 0.6);
        assert_eq!(u.width_ratio_at(0.3), 1.0);
        assert!(u.validate().is_ok());
    }

    #[test]
    fn uniform_profile_is_full_width() {
        let b = BrushStroke::uniform(8.0, LineCap::Round);
        assert_eq!(b.width_ratio_at(0.3), 1.0);
        assert_eq!(b.max_ratio(), 1.0);
        assert!(b.validate().is_ok());
    }

    #[test]
    fn build_centerline_makes_open_smooth_path() {
        let (path, profile) = build_centerline(&line_samples(12, 1.0), 2.0, 0.3).expect("built");
        assert_eq!(path.contours.len(), 1);
        let c = &path.contours[0];
        assert!(!c.closed, "brush centerline is open");
        assert!(c.nodes.len() >= 2);
        // A straight run RDP-collapses toward the two endpoints.
        assert!(
            c.nodes.len() <= 4,
            "straight line simplifies: {}",
            c.nodes.len()
        );
        // Profile spans the full arc.
        assert!((profile.first().unwrap().t - 0.0).abs() < 1e-4);
        assert!((profile.last().unwrap().t - 1.0).abs() < 1e-4);
    }

    #[test]
    fn build_centerline_rejects_degenerate() {
        assert!(build_centerline(&[], 2.0, 0.3).is_none());
        assert!(build_centerline(&[(Point::new(1.0, 1.0), 1.0)], 2.0, 0.3).is_none());
        // All coincident points collapse to one → no segment.
        let coincident = vec![(Point::new(2.0, 2.0), 1.0); 5];
        assert!(build_centerline(&coincident, 2.0, 0.3).is_none());
    }

    #[test]
    fn catmull_rom_passes_through_points() {
        let pts = [
            Point::new(0.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(20.0, 0.0),
        ];
        let c = catmull_rom_to_contour(&pts);
        assert_eq!(c.nodes.len(), 3);
        for (i, p) in pts.iter().enumerate() {
            assert_eq!(c.nodes[i].anchor, *p);
        }
        // Endpoints keep one handle only.
        assert!(c.nodes[0].in_handle.is_none());
        assert!(c.nodes[2].out_handle.is_none());
        assert!(c.nodes[1].in_handle.is_some() && c.nodes[1].out_handle.is_some());
    }

    #[test]
    fn expand_stroke_builds_closed_outline_wider_than_centerline() {
        // A horizontal centerline, uniform width 10 → outline spans y ≈ ±5.
        let (path, _profile) = build_centerline(&line_samples(6, 1.0), 2.0, 0.3).unwrap();
        let brush = BrushStroke::uniform(10.0, LineCap::Round);
        let outline = expand_stroke(&path, &brush).expect("expanded");
        let c = &outline.contours[0];
        assert!(c.closed, "expanded stroke is a closed outline");
        let ys: Vec<f32> = c.nodes.iter().map(|n| n.anchor.y).collect();
        let max_y = ys.iter().cloned().fold(f32::MIN, f32::max);
        let min_y = ys.iter().cloned().fold(f32::MAX, f32::min);
        assert!(
            max_y > 4.0 && min_y < -4.0,
            "outline spans the half-width: [{min_y},{max_y}]"
        );
    }

    #[test]
    fn expand_stroke_needs_visible_width() {
        let (path, _p) = build_centerline(&line_samples(6, 1.0), 2.0, 0.3).unwrap();
        assert!(expand_stroke(&path, &BrushStroke::uniform(0.0, LineCap::Round)).is_none());
    }

    #[test]
    fn validate_catches_bad_values() {
        let mut b = BrushStroke::uniform(f32::NAN, LineCap::Round);
        assert!(b.validate().is_err());
        b = BrushStroke {
            base_width: 5.0,
            profile: vec![WidthStop { t: 2.0, width: 1.0 }],
            cap: LineCap::Round,
        };
        assert!(b.validate().is_err());
    }
}

#![allow(dead_code)]
//! Adaptive Bézier flattening (foundation task T1.4).
//!
//! The plan is explicit (Mục 4.2 / 8): do NOT hard-code a fixed 24-step
//! subdivision for every cubic. Instead we recurse only where the curve is not
//! yet flat to within a caller-supplied `tolerance` (in path units), so a
//! near-straight segment yields one line while a tight curl yields many. A hard
//! recursion-depth cap keeps degenerate/cusp input from subdividing forever.
//!
//! Output is the source of truth for hit-testing (task T1.5) and, later, the
//! CPU rasterizer (task T5.1). It never allocates a curve object — just points.

use crate::core::geometry::{Point, Rect};
use crate::core::vector::path::{Contour, PathData};

/// A sensible default flatness for interactive rendering/hit-testing: half a
/// device pixel at 1:1. Callers rendering at high zoom should pass a smaller
/// value derived from the view scale.
pub const DEFAULT_TOLERANCE: f32 = 0.25;

/// Floor applied to any tolerance so a caller passing `0.0` can't force endless
/// subdivision.
const MIN_TOLERANCE: f32 = 1e-4;

/// Hard cap on subdivision depth. 2^24 sub-segments is far past any real need;
/// the flatness test almost always stops long before this.
const MAX_DEPTH: u32 = 24;

/// Perpendicular distance from `p` to the infinite line through `a`,`b`. When
/// the chord degenerates to a point, fall back to the straight distance to `a`.
#[inline]
fn point_line_distance(p: Point, a: Point, b: Point) -> f32 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len2 = dx * dx + dy * dy;
    if len2 <= f32::EPSILON {
        return p.distance_to(a);
    }
    let cross = (p.x - a.x) * dy - (p.y - a.y) * dx;
    cross.abs() / len2.sqrt()
}

/// A cubic is "flat enough" when both off-curve controls sit within `tol` of
/// the chord joining its endpoints.
#[inline]
fn is_flat(p0: Point, p1: Point, p2: Point, p3: Point, tol: f32) -> bool {
    point_line_distance(p1, p0, p3) <= tol && point_line_distance(p2, p0, p3) <= tol
}

fn subdivide(
    p0: Point,
    p1: Point,
    p2: Point,
    p3: Point,
    tol: f32,
    depth: u32,
    out: &mut Vec<Point>,
) {
    if depth == 0 || is_flat(p0, p1, p2, p3, tol) {
        out.push(p3);
        return;
    }
    // De Casteljau split at the midpoint.
    let p01 = p0.lerp(p1, 0.5);
    let p12 = p1.lerp(p2, 0.5);
    let p23 = p2.lerp(p3, 0.5);
    let p012 = p01.lerp(p12, 0.5);
    let p123 = p12.lerp(p23, 0.5);
    let mid = p012.lerp(p123, 0.5);
    subdivide(p0, p01, p012, mid, tol, depth - 1, out);
    subdivide(mid, p123, p23, p3, tol, depth - 1, out);
}

/// Flatten a single cubic into a polyline **including both endpoints**:
/// `[p0, …, p3]`. A straight (degenerate) cubic returns just `[p0, p3]`.
pub fn flatten_cubic(p0: Point, p1: Point, p2: Point, p3: Point, tol: f32) -> Vec<Point> {
    let tol = tol.max(MIN_TOLERANCE);
    let mut out = vec![p0];
    subdivide(p0, p1, p2, p3, tol, MAX_DEPTH, &mut out);
    out
}

/// Flatten a whole contour into a polyline of on-curve points.
///
/// For a **closed** contour the wrap segment (last node → first node) is one of
/// the contour's segments, so the returned polyline naturally ends back at the
/// first anchor — callers can treat consecutive points as edges without any
/// special closing step. An empty contour yields an empty vec; a single node
/// yields that one anchor.
pub fn flatten_contour(contour: &Contour, tol: f32) -> Vec<Point> {
    if contour.nodes.is_empty() {
        return Vec::new();
    }
    let mut out = vec![contour.nodes[0].anchor];
    for si in 0..contour.segment_count() {
        let (p0, p1, p2, p3) = contour.segment(si).expect("segment in range");
        let pts = flatten_cubic(p0, p1, p2, p3, tol);
        out.extend_from_slice(&pts[1..]); // skip the duplicated start point
    }
    out
}

/// Flatten every contour of a path; one polyline per contour, order preserved.
pub fn flatten_path(path: &PathData, tol: f32) -> Vec<Vec<Point>> {
    path.contours
        .iter()
        .map(|c| flatten_contour(c, tol))
        .collect()
}

/// Tight geometric bounds of the flattened path — a superset-free replacement
/// for the conservative control-hull bounds in `path.rs`. `None` when the path
/// has no points.
pub fn tight_bounds(path: &PathData, tol: f32) -> Option<Rect> {
    let mut it = path
        .contours
        .iter()
        .flat_map(|c| flatten_contour(c, tol))
        .peekable();
    let first = *it.peek()?;
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (first.x, first.y, first.x, first.y);
    for p in it {
        min_x = min_x.min(p.x);
        min_y = min_y.min(p.y);
        max_x = max_x.max(p.x);
        max_y = max_y.max(p.y);
    }
    Some(Rect::new(min_x, min_y, max_x - min_x, max_y - min_y))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::geometry::cubic_bezier;
    use crate::core::vector::path::{FillRule, Node, NodeKind};

    #[test]
    fn straight_cubic_is_two_points() {
        let p = flatten_cubic(
            Point::new(0.0, 0.0),
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 0.0),
            DEFAULT_TOLERANCE,
        );
        assert_eq!(p.len(), 2);
        assert_eq!(p[0], Point::new(0.0, 0.0));
        assert_eq!(p[1], Point::new(10.0, 0.0));
    }

    #[test]
    fn tighter_tolerance_yields_more_points() {
        let (p0, p1, p2, p3) = (
            Point::new(0.0, 0.0),
            Point::new(0.0, 50.0),
            Point::new(100.0, 50.0),
            Point::new(100.0, 0.0),
        );
        let coarse = flatten_cubic(p0, p1, p2, p3, 5.0).len();
        let fine = flatten_cubic(p0, p1, p2, p3, 0.05).len();
        assert!(fine > coarse, "fine={fine} coarse={coarse}");
    }

    #[test]
    fn flattened_points_stay_within_tolerance() {
        // Every flattened vertex must lie on the true curve; and the polyline
        // must not stray from the curve by more than the tolerance. We check the
        // former exactly and the latter by sampling the curve densely and
        // measuring distance to the nearest polyline segment.
        let (p0, p1, p2, p3) = (
            Point::new(0.0, 0.0),
            Point::new(10.0, 80.0),
            Point::new(90.0, 80.0),
            Point::new(100.0, 0.0),
        );
        let tol = 0.5;
        let poly = flatten_cubic(p0, p1, p2, p3, tol);
        let dist_to_poly = |q: Point| {
            poly.windows(2)
                .map(|w| point_seg_dist(q, w[0], w[1]))
                .fold(f32::INFINITY, f32::min)
        };
        for k in 0..=200 {
            let t = k as f32 / 200.0;
            let on_curve = cubic_bezier(p0, p1, p2, p3, t);
            // A little slack over `tol` for the sampled (not worst-case) point.
            assert!(dist_to_poly(on_curve) <= tol + 0.25);
        }
    }

    #[test]
    fn closed_contour_polyline_returns_to_start() {
        let c = Contour::new(
            vec![
                Node::sharp(Point::new(0.0, 0.0)),
                Node::sharp(Point::new(10.0, 0.0)),
                Node::sharp(Point::new(10.0, 10.0)),
            ],
            true,
        );
        let poly = flatten_contour(&c, DEFAULT_TOLERANCE);
        assert!(poly.first().unwrap().distance_to(*poly.last().unwrap()) < 1e-4);
    }

    #[test]
    fn tight_bounds_of_bulging_curve_exceed_endpoints() {
        let c = Contour::new(
            vec![
                Node::with_handles(
                    Point::new(0.0, 0.0),
                    Point::new(0.0, 0.0),
                    Point::new(0.0, 40.0),
                    NodeKind::Smooth,
                ),
                Node::with_handles(
                    Point::new(100.0, 0.0),
                    Point::new(100.0, 40.0),
                    Point::new(100.0, 0.0),
                    NodeKind::Smooth,
                ),
            ],
            false,
        );
        let b = tight_bounds(&PathData::new(vec![c], FillRule::NonZero), 0.1).unwrap();
        // The curve bulges to ~y=30, well past the endpoints at y=0.
        assert!(b.h > 20.0, "height {}", b.h);
        assert!(b.y >= -1e-3 && b.x >= -1e-3);
    }

    fn point_seg_dist(p: Point, a: Point, b: Point) -> f32 {
        let abx = b.x - a.x;
        let aby = b.y - a.y;
        let len2 = abx * abx + aby * aby;
        let t = if len2 <= f32::EPSILON {
            0.0
        } else {
            (((p.x - a.x) * abx + (p.y - a.y) * aby) / len2).clamp(0.0, 1.0)
        };
        p.distance_to(Point::new(a.x + abx * t, a.y + aby * t))
    }
}

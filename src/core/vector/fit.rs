#![allow(dead_code)]
//! Curve fitting: turn a dense polyline back into a compact cubic-Bézier
//! [`Contour`], preserving genuine corners.
//!
//! This is a post-process for [`crate::core::vector::boolean`]: shaping topology
//! is computed on flattened polygons (robust), then the resulting boundary is
//! re-fitted here so the user gets a clean, editable curve with a handful of
//! nodes instead of hundreds of sharp ones. It never changes the boolean
//! *topology* — only how the resulting boundary is represented — and the caller
//! falls back to the raw polygon if fitting fails.
//!
//! Algorithm: Schneider, "An Algorithm for Automatically Fitting Digitized
//! Curves" (Graphics Gems, 1990) — least-squares cubic fit with fixed endpoint
//! tangents, Newton-Raphson reparameterisation, and recursive subdivision at the
//! point of maximum error. Sharp corners are detected first and the ring is split
//! there so they stay as cusps (a welded rectangle keeps its square corners).

use crate::core::geometry::{cubic_bezier, Point};
use crate::core::vector::path::{Contour, Node, NodeKind};

/// A turn sharper than this (angle between the incoming and outgoing edge
/// direction) forces a cusp and splits the fit there. 60° keeps obvious corners
/// (rectangles at 90°, star points) sharp while letting gentle arcs smooth out.
const CORNER_TURN_RAD: f32 = std::f32::consts::PI / 3.0;

/// Newton-Raphson reparameterisation passes when a fit is close but not yet
/// within tolerance.
const MAX_REPARAM_ITERS: usize = 4;

/// Recursion cap so a pathological run can never subdivide forever.
const MAX_DEPTH: usize = 16;

// ── small 2-D vector helpers (Point carries no arithmetic of its own) ─────────

#[inline]
fn sub(a: Point, b: Point) -> Point {
    Point::new(a.x - b.x, a.y - b.y)
}
#[inline]
fn add(a: Point, b: Point) -> Point {
    Point::new(a.x + b.x, a.y + b.y)
}
#[inline]
fn mul(a: Point, s: f32) -> Point {
    Point::new(a.x * s, a.y * s)
}
#[inline]
fn dot(a: Point, b: Point) -> f32 {
    a.x * b.x + a.y * b.y
}
#[inline]
fn length(a: Point) -> f32 {
    (a.x * a.x + a.y * a.y).sqrt()
}
#[inline]
fn normalize(a: Point) -> Point {
    let l = length(a);
    if l > 1e-12 {
        Point::new(a.x / l, a.y / l)
    } else {
        Point::new(0.0, 0.0)
    }
}

type Cubic = [Point; 4];

/// Fit a closed ring of sampled points into a smooth cubic-Bézier [`Contour`],
/// preserving sharp corners. `tol` is the maximum allowed distance (path units)
/// between the fitted curve and the source polyline. Returns `None` on a
/// degenerate ring or if the fit produced a non-finite control point, so the
/// caller can fall back to the raw polygon.
pub fn fit_closed_ring(points: &[Point], tol: f32) -> Option<Contour> {
    let n = points.len();
    if n < 3 {
        return None;
    }
    let tol = tol.max(1e-3);

    let corners = detect_corners(points);
    let mut cubics: Vec<Cubic> = Vec::new();
    // Which cubic starts at a genuine corner (→ cusp node).
    let mut corner_start: Vec<bool> = Vec::new();

    if corners.is_empty() {
        // A smooth closed loop (e.g. a circle): pick the sharpest vertex as the
        // seam and fit one run all the way around, closing it smoothly by sharing
        // the seam tangent between the first and last cubic.
        let seam = sharpest_vertex(points);
        let run: Vec<Point> = (0..=n).map(|k| points[(seam + k) % n]).collect();
        let prev = points[(seam + n - 1) % n];
        let next = points[(seam + 1) % n];
        let seam_tan = normalize(sub(next, prev));
        let left_t = seam_tan;
        let right_t = mul(seam_tan, -1.0);
        fit_run(&run, left_t, right_t, tol, &mut cubics);
        corner_start = vec![false; cubics.len()];
    } else {
        let c = corners.len();
        for ci in 0..c {
            let start = corners[ci];
            let end = corners[(ci + 1) % c];
            // Collect the run start..=end, wrapping around the ring.
            let mut run: Vec<Point> = Vec::new();
            let mut k = start;
            loop {
                run.push(points[k]);
                if k == end {
                    break;
                }
                k = (k + 1) % n;
            }
            if run.len() < 2 {
                continue;
            }
            let left_t = normalize(sub(run[1], run[0]));
            let right_t = normalize(sub(run[run.len() - 2], run[run.len() - 1]));
            let before = cubics.len();
            fit_run(&run, left_t, right_t, tol, &mut cubics);
            // Every cubic added by this run; only the first begins at a corner.
            for i in before..cubics.len() {
                corner_start.push(i == before);
            }
        }
    }

    build_closed_contour(&cubics, &corner_start)
}

// ── corner detection ─────────────────────────────────────────────────────────

/// Indices of vertices whose turn angle exceeds [`CORNER_TURN_RAD`].
fn detect_corners(points: &[Point]) -> Vec<usize> {
    let n = points.len();
    let mut corners = Vec::new();
    for i in 0..n {
        if turn_angle(points, i) > CORNER_TURN_RAD {
            corners.push(i);
        }
    }
    corners
}

/// Turn angle at vertex `i` of a closed ring: the angle between the direction of
/// the edge arriving at `i` and the edge leaving it. 0 = straight, π = a spike.
fn turn_angle(points: &[Point], i: usize) -> f32 {
    let n = points.len();
    let prev = points[(i + n - 1) % n];
    let cur = points[i];
    let next = points[(i + 1) % n];
    let v_in = normalize(sub(cur, prev));
    let v_out = normalize(sub(next, cur));
    if (v_in.x == 0.0 && v_in.y == 0.0) || (v_out.x == 0.0 && v_out.y == 0.0) {
        return 0.0;
    }
    dot(v_in, v_out).clamp(-1.0, 1.0).acos()
}

/// The vertex with the largest turn angle — used as a seam for a corner-free ring
/// so the fit starts somewhere stable.
fn sharpest_vertex(points: &[Point]) -> usize {
    let n = points.len();
    let mut best = 0usize;
    let mut best_turn = -1.0f32;
    for i in 0..n {
        let t = turn_angle(points, i);
        if t > best_turn {
            best_turn = t;
            best = i;
        }
    }
    best
}

// ── Schneider fit ────────────────────────────────────────────────────────────

/// Fit an open run of points (tangents point *into* the curve at each end),
/// appending one or more cubics to `out`.
fn fit_run(pts: &[Point], left_t: Point, right_t: Point, tol: f32, out: &mut Vec<Cubic>) {
    if pts.len() < 2 {
        return;
    }
    fit_cubic(pts, left_t, right_t, tol, 0, out);
}

fn fit_cubic(
    pts: &[Point],
    left_t: Point,
    right_t: Point,
    tol: f32,
    depth: usize,
    out: &mut Vec<Cubic>,
) {
    let n = pts.len();
    let p0 = pts[0];
    let p3 = pts[n - 1];

    // A short run, or one that is straight within tolerance, becomes a plain line
    // segment (handles collapse onto the anchors → no Bézier handles).
    if n == 2 || is_straight(pts, tol) {
        out.push([p0, p0, p3, p3]);
        return;
    }

    let mut u = chord_length_param(pts);
    let mut bez = generate_bezier(pts, &u, left_t, right_t);
    let (mut max_err, mut split) = max_error(pts, &bez, &u);

    if max_err < tol {
        out.push(bez);
        return;
    }

    // Close enough to try improving the parameterisation before giving up.
    if max_err < tol * tol {
        for _ in 0..MAX_REPARAM_ITERS {
            reparameterize(pts, &bez, &mut u);
            bez = generate_bezier(pts, &u, left_t, right_t);
            let (e, s) = max_error(pts, &bez, &u);
            max_err = e;
            split = s;
            if max_err < tol {
                out.push(bez);
                return;
            }
        }
    }

    if depth >= MAX_DEPTH {
        // Stop subdividing: accept the current fit rather than recurse forever.
        out.push(bez);
        return;
    }

    // Subdivide at the point of maximum error and recurse with a shared tangent.
    if split == 0 || split >= n - 1 {
        split = n / 2;
    }
    let center = center_tangent(pts, split);
    fit_cubic(&pts[..=split], left_t, center, tol, depth + 1, out);
    fit_cubic(
        &pts[split..],
        mul(center, -1.0),
        right_t,
        tol,
        depth + 1,
        out,
    );
}

/// True when every interior point lies within `tol` of the chord `p0→pN`.
fn is_straight(pts: &[Point], tol: f32) -> bool {
    let n = pts.len();
    let a = pts[0];
    let b = pts[n - 1];
    let ab = sub(b, a);
    let len = length(ab);
    if len < 1e-9 {
        // Degenerate closed run: treat as straight only if all points coincide.
        return pts.iter().all(|&p| p.distance_to(a) < tol);
    }
    let inv = 1.0 / len;
    let dir = Point::new(ab.x * inv, ab.y * inv);
    for &p in &pts[1..n - 1] {
        let ap = sub(p, a);
        // Perpendicular distance to the infinite line through a→b.
        let perp = (ap.x * dir.y - ap.y * dir.x).abs();
        if perp > tol {
            return false;
        }
    }
    true
}

/// Cumulative chord length, normalised to `[0,1]`.
fn chord_length_param(pts: &[Point]) -> Vec<f32> {
    let n = pts.len();
    let mut u = vec![0.0f32; n];
    for i in 1..n {
        u[i] = u[i - 1] + pts[i].distance_to(pts[i - 1]);
    }
    let total = u[n - 1];
    if total > 1e-12 {
        for v in u.iter_mut() {
            *v /= total;
        }
    }
    u
}

/// Least-squares fit of a single cubic to `pts` with the endpoints and endpoint
/// tangents fixed (Graphics Gems). Returns control points `[p0,p1,p2,p3]`.
fn generate_bezier(pts: &[Point], u: &[f32], left_t: Point, right_t: Point) -> Cubic {
    let n = pts.len();
    let p0 = pts[0];
    let p3 = pts[n - 1];

    let mut c = [[0.0f32; 2]; 2];
    let mut x = [0.0f32; 2];

    for i in 0..n {
        let ui = u[i];
        let om = 1.0 - ui;
        let b0 = om * om * om;
        let b1 = 3.0 * om * om * ui;
        let b2 = 3.0 * om * ui * ui;
        let b3 = ui * ui * ui;

        let a0 = mul(left_t, b1);
        let a1 = mul(right_t, b2);

        c[0][0] += dot(a0, a0);
        c[0][1] += dot(a0, a1);
        c[1][0] += dot(a0, a1);
        c[1][1] += dot(a1, a1);

        let base = add(mul(p0, b0 + b1), mul(p3, b2 + b3));
        let tmp = sub(pts[i], base);
        x[0] += dot(a0, tmp);
        x[1] += dot(a1, tmp);
    }

    let det_c0_c1 = c[0][0] * c[1][1] - c[1][0] * c[0][1];
    let det_c0_x = c[0][0] * x[1] - c[1][0] * x[0];
    let det_x_c1 = x[0] * c[1][1] - x[1] * c[0][1];

    let (mut alpha_l, mut alpha_r) = if det_c0_c1.abs() < 1e-12 {
        (0.0, 0.0)
    } else {
        (det_x_c1 / det_c0_c1, det_c0_x / det_c0_c1)
    };

    // Fall back to the Wu/Barsky heuristic (a third of the chord) when the fit is
    // degenerate or the tangent lengths came out non-positive.
    let seg_len = p0.distance_to(p3);
    let epsilon = 1e-6 * seg_len.max(1e-6);
    if alpha_l < epsilon || alpha_r < epsilon {
        let third = seg_len / 3.0;
        alpha_l = third;
        alpha_r = third;
    }

    let p1 = add(p0, mul(left_t, alpha_l));
    let p2 = add(p3, mul(right_t, alpha_r));
    [p0, p1, p2, p3]
}

/// Maximum distance between the source points and the fitted cubic, plus the
/// index of the worst point (a subdivision candidate).
fn max_error(pts: &[Point], bez: &Cubic, u: &[f32]) -> (f32, usize) {
    let n = pts.len();
    let mut max = 0.0f32;
    let mut split = n / 2;
    for i in 1..n - 1 {
        let q = cubic_bezier(bez[0], bez[1], bez[2], bez[3], u[i]);
        let d = q.distance_to(pts[i]);
        if d >= max {
            max = d;
            split = i;
        }
    }
    (max, split)
}

/// One Newton-Raphson step per point, moving each `u[i]` toward the parameter of
/// the nearest point on the fitted curve.
fn reparameterize(pts: &[Point], bez: &Cubic, u: &mut [f32]) {
    for i in 0..pts.len() {
        u[i] = newton_step(bez, pts[i], u[i]);
    }
}

fn newton_step(bez: &Cubic, p: Point, t: f32) -> f32 {
    // First-derivative control points (degree-2 Bézier): 3·(bez[i+1]-bez[i]).
    let q1 = [
        mul(sub(bez[1], bez[0]), 3.0),
        mul(sub(bez[2], bez[1]), 3.0),
        mul(sub(bez[3], bez[2]), 3.0),
    ];
    // Second-derivative control points (degree-1 Bézier): 2·(q1[i+1]-q1[i]).
    let q2 = [mul(sub(q1[1], q1[0]), 2.0), mul(sub(q1[2], q1[1]), 2.0)];

    let qt = cubic_bezier(bez[0], bez[1], bez[2], bez[3], t);
    let q1t = quad_bezier(q1[0], q1[1], q1[2], t);
    let q2t = q2[0].lerp(q2[1], t);

    let num = dot(sub(qt, p), q1t);
    let den = dot(q1t, q1t) + dot(sub(qt, p), q2t);
    if den.abs() < 1e-12 {
        return t;
    }
    let nt = t - num / den;
    if nt.is_finite() {
        nt.clamp(0.0, 1.0)
    } else {
        t
    }
}

#[inline]
fn quad_bezier(a: Point, b: Point, c: Point, t: f32) -> Point {
    let om = 1.0 - t;
    let w0 = om * om;
    let w1 = 2.0 * om * t;
    let w2 = t * t;
    Point::new(
        a.x * w0 + b.x * w1 + c.x * w2,
        a.y * w0 + b.y * w1 + c.y * w2,
    )
}

/// Tangent at an interior split point, pointing back toward decreasing index
/// (Graphics Gems `ComputeCenterTangent`).
fn center_tangent(pts: &[Point], i: usize) -> Point {
    let v1 = sub(pts[i - 1], pts[i]);
    let v2 = sub(pts[i], pts[i + 1]);
    normalize(mul(add(v1, v2), 0.5))
}

// ── cubic list → Contour ─────────────────────────────────────────────────────

/// Distance below which a handle sits on its anchor and is dropped (→ straight).
const HANDLE_EPS: f32 = 1e-4;
/// Handles this collinear (dot of the two unit directions) count as `Smooth`.
const SMOOTH_DOT: f32 = 0.985;

fn build_closed_contour(cubics: &[Cubic], corner_start: &[bool]) -> Option<Contour> {
    let m = cubics.len();
    if m == 0 {
        return None;
    }
    let mut nodes = Vec::with_capacity(m);
    for k in 0..m {
        let cur = cubics[k];
        let prev = cubics[(k + m - 1) % m];
        let anchor = cur[0];
        let out_raw = cur[1];
        let in_raw = prev[2];

        for pt in [anchor, out_raw, in_raw] {
            if !pt.x.is_finite() || !pt.y.is_finite() {
                return None;
            }
        }

        let out_handle = (out_raw.distance_to(anchor) > HANDLE_EPS).then_some(out_raw);
        let in_handle = (in_raw.distance_to(anchor) > HANDLE_EPS).then_some(in_raw);

        let is_corner = corner_start.get(k).copied().unwrap_or(true);
        let kind = if is_corner {
            NodeKind::Cusp
        } else {
            match (in_handle, out_handle) {
                (Some(ih), Some(oh)) => {
                    let vi = normalize(sub(anchor, ih));
                    let vo = normalize(sub(oh, anchor));
                    if dot(vi, vo) > SMOOTH_DOT {
                        NodeKind::Smooth
                    } else {
                        NodeKind::Cusp
                    }
                }
                _ => NodeKind::Cusp,
            }
        };

        nodes.push(Node {
            anchor,
            in_handle,
            out_handle,
            kind,
        });
    }
    Some(Contour::new(nodes, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::vector::flatten::flatten_contour;

    /// Sample a circle into `n` points (closed ring, no duplicate endpoint).
    fn circle_ring(cx: f32, cy: f32, r: f32, n: usize) -> Vec<Point> {
        (0..n)
            .map(|i| {
                let a = i as f32 / n as f32 * std::f32::consts::TAU;
                Point::new(cx + r * a.cos(), cy + r * a.sin())
            })
            .collect()
    }

    fn ring_signed_area(pts: &[Point]) -> f32 {
        let n = pts.len();
        let mut a = 0.0;
        for i in 0..n {
            let p = pts[i];
            let q = pts[(i + 1) % n];
            a += p.x * q.y - q.x * p.y;
        }
        a * 0.5
    }

    /// Densely sample a fitted contour back to a polyline for area/shape checks.
    fn contour_to_polyline(c: &Contour) -> Vec<Point> {
        let mut poly = flatten_contour(c, 0.05);
        if poly.len() >= 2 && poly[0].distance_to(poly[poly.len() - 1]) < 1e-4 {
            poly.pop();
        }
        poly
    }

    #[test]
    fn square_keeps_four_sharp_corners() {
        // A dense square outline: sampled along each edge so corner detection has
        // to find exactly the four 90° turns.
        let mut pts = Vec::new();
        let side = 100.0;
        let steps = 20;
        for i in 0..steps {
            pts.push(Point::new(i as f32 / steps as f32 * side, 0.0));
        }
        for i in 0..steps {
            pts.push(Point::new(side, i as f32 / steps as f32 * side));
        }
        for i in 0..steps {
            pts.push(Point::new(side - i as f32 / steps as f32 * side, side));
        }
        for i in 0..steps {
            pts.push(Point::new(0.0, side - i as f32 / steps as f32 * side));
        }

        let contour = fit_closed_ring(&pts, 0.5).expect("fit square");
        // Four corner nodes, each a cusp with no handles (straight edges).
        assert_eq!(contour.nodes.len(), 4, "square should fit to four nodes");
        for node in &contour.nodes {
            assert_eq!(node.kind, NodeKind::Cusp);
            assert!(node.in_handle.is_none() && node.out_handle.is_none());
        }
        // Area preserved (100×100 = 10 000).
        let area = ring_signed_area(&contour_to_polyline(&contour)).abs();
        assert!((area - 10_000.0).abs() < 50.0, "square area {area}");
    }

    #[test]
    fn circle_fits_to_few_smooth_nodes() {
        let ring = circle_ring(0.0, 0.0, 50.0, 128);
        let contour = fit_closed_ring(&ring, 0.4).expect("fit circle");
        // A circle needs only a handful of cubic segments, not 128 nodes.
        assert!(
            contour.nodes.len() <= 10,
            "circle fit used {} nodes",
            contour.nodes.len()
        );
        assert!(contour.nodes.len() >= 3);
        // No forced corners → every node smooth with handles.
        assert!(contour.nodes.iter().all(|n| n.kind == NodeKind::Smooth));
        assert!(contour
            .nodes
            .iter()
            .all(|n| n.in_handle.is_some() && n.out_handle.is_some()));
        // Area preserved within tolerance (π·50² ≈ 7854).
        let area = ring_signed_area(&contour_to_polyline(&contour)).abs();
        let expected = std::f32::consts::PI * 50.0 * 50.0;
        assert!(
            (area - expected).abs() / expected < 0.01,
            "circle area {area}"
        );
    }

    #[test]
    fn fitted_circle_stays_close_to_source() {
        let ring = circle_ring(10.0, -5.0, 30.0, 96);
        let contour = fit_closed_ring(&ring, 0.4).expect("fit");
        // Every source point is within a small distance of the fitted outline.
        let poly = contour_to_polyline(&contour);
        for &src in &ring {
            let nearest = poly
                .iter()
                .map(|&q| q.distance_to(src))
                .fold(f32::INFINITY, f32::min);
            assert!(nearest < 1.0, "source point {src:?} drifted {nearest}");
        }
    }

    #[test]
    fn degenerate_input_returns_none() {
        assert!(fit_closed_ring(&[], 0.5).is_none());
        assert!(fit_closed_ring(&[Point::new(0.0, 0.0), Point::new(1.0, 1.0)], 0.5).is_none());
    }

    #[test]
    fn fit_produces_valid_finite_geometry() {
        let ring = circle_ring(0.0, 0.0, 12.0, 40);
        let contour = fit_closed_ring(&ring, 0.4).expect("fit");
        let path = crate::core::vector::path::PathData::new(
            vec![contour],
            crate::core::vector::path::FillRule::EvenOdd,
        );
        assert!(path.validate().is_ok());
    }
}

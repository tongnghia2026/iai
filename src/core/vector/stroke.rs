//! Stroke → filled outline contours: the single shared CPU/GPU reference.
//!
//! Turns a flattened centreline into the *filled outline* of its stroke,
//! honouring cap and join style. Both the CPU rasteriser
//! ([`crate::core::vector::raster`]) and the GPU tessellator
//! ([`crate::gpu::vector::mesh`]) fill these contours, so a stroke renders the
//! same shape on either path and the GPU flag toggles cleanly.
//!
//! Every contour is emitted with the same orientation (positive signed area) so
//! a NonZero fill unions them: overlapping segment quads and join/cap wedges add
//! winding, and the hollow interior of a closed outline stays at winding zero.
//! Nothing here allocates GPU state or depends on UI.

use crate::core::geometry::Point;
use crate::core::vector::style::{ArrowHead, ArrowStyle, LineCap, LineJoin};

/// Points closer than this (in the target space) are treated as coincident.
const EPS: f32 = 1e-4;

/// Build the filled outline contours for stroking `polylines` at `half`-width.
///
/// `polylines`/`closed` are the flattened centrelines and their closed flags in
/// the target space; `tolerance` bounds the flattening error of round caps and
/// joins (pass the same value used to flatten the path). A dashed stroke passes
/// one open polyline per dash (`closed == false`), so each dash gets its caps.
///
/// The miter limit follows the SVG convention: a miter is clipped to a bevel when
/// the apex-to-vertex distance exceeds `miter_limit × half`. CPU and GPU share
/// this exact geometry, so they agree regardless of the reference definition.
#[allow(clippy::too_many_arguments)]
pub fn stroke_outline_contours(
    polylines: &[Vec<Point>],
    closed: &[bool],
    half: f32,
    cap: LineCap,
    join: LineJoin,
    miter_limit: f32,
    tolerance: f32,
) -> Vec<Vec<Point>> {
    let mut out = Vec::new();
    if !(half > 0.0) {
        return out;
    }
    let tol = tolerance.max(1e-3).min(half);
    for (ci, pl) in polylines.iter().enumerate() {
        let is_closed = closed.get(ci).copied().unwrap_or(false);
        let pts = clean(pl, is_closed);
        stroke_one(&pts, is_closed, half, cap, join, miter_limit, tol, &mut out);
    }
    out
}

/// Build filled arrowhead rings for the free ends of open centrelines.
pub fn arrowhead_contours(
    polylines: &[Vec<Point>],
    closed: &[bool],
    half: f32,
    start: ArrowStyle,
    end: ArrowStyle,
    tolerance: f32,
) -> Vec<Vec<Point>> {
    let mut out = Vec::new();
    if !(half > 0.0) {
        return out;
    }
    for (index, pl) in polylines.iter().enumerate() {
        if closed.get(index).copied().unwrap_or(false) || pl.len() < 2 {
            continue;
        }
        if let Some((p, u)) = endpoint_direction(pl, true) {
            add_arrowhead(p, u, half, start, tolerance, &mut out);
        }
        if let Some((p, u)) = endpoint_direction(pl, false) {
            add_arrowhead(p, u, half, end, tolerance, &mut out);
        }
    }
    out
}

fn endpoint_direction(pl: &[Point], start: bool) -> Option<(Point, (f32, f32))> {
    let p = if start { pl[0] } else { *pl.last()? };
    let candidates: Box<dyn Iterator<Item = &Point> + '_> = if start {
        Box::new(pl[1..].iter())
    } else {
        Box::new(pl[..pl.len() - 1].iter().rev())
    };
    candidates
        .filter_map(|neighbour| norm(sub(p, *neighbour)))
        .next()
        .map(|u| (p, u))
}

fn add_arrowhead(
    p: Point,
    u: (f32, f32),
    half: f32,
    style: ArrowStyle,
    tolerance: f32,
    out: &mut Vec<Vec<Point>>,
) {
    if style.kind == ArrowHead::None || !(style.size > 0.0) {
        return;
    }
    let length = style.size * (2.0 * half);
    let nm = perp(u);
    let wing = (length * 0.42).max(2.0 * half);
    let left = along(p, nm, wing);
    let right = along(p, nm, -wing);
    let tip = along(p, u, length);
    let ring = match style.kind {
        ArrowHead::None => return,
        ArrowHead::Triangle => vec![left, tip, right],
        ArrowHead::Stealth => vec![left, tip, right, along(p, u, length * 0.4)],
        ArrowHead::Diamond => vec![
            p,
            along(along(p, u, length * 0.5), nm, wing),
            tip,
            along(along(p, u, length * 0.5), nm, -wing),
        ],
        ArrowHead::Circle => circle(along(p, u, length * 0.5), length * 0.5, tolerance),
    };
    push_ring(out, ring);
}

/// Drop consecutive coincident points; for a closed ring also drop the flattener's
/// repeated start-at-end point so the wrap segment is not zero length.
fn clean(pl: &[Point], is_closed: bool) -> Vec<Point> {
    let mut pts: Vec<Point> = Vec::with_capacity(pl.len());
    for &p in pl {
        if pts.last().map_or(true, |q: &Point| dist(*q, p) > EPS) {
            pts.push(p);
        }
    }
    if is_closed {
        while pts.len() >= 2 && dist(pts[0], pts[pts.len() - 1]) <= EPS {
            pts.pop();
        }
    }
    pts
}

#[allow(clippy::too_many_arguments)]
fn stroke_one(
    pts: &[Point],
    is_closed: bool,
    half: f32,
    cap: LineCap,
    join: LineJoin,
    miter_limit: f32,
    tol: f32,
    out: &mut Vec<Vec<Point>>,
) {
    let n = pts.len();
    if n == 0 {
        return;
    }
    if n == 1 {
        // A lone point only shows for round/square caps (a butt cap is empty).
        match cap {
            LineCap::Round => push_ring(out, circle(pts[0], half, tol)),
            LineCap::Square => push_ring(out, axis_square(pts[0], half)),
            LineCap::Butt => {}
        }
        return;
    }

    // Body: one offset quad per segment.
    let seg_count = if is_closed { n } else { n - 1 };
    for i in 0..seg_count {
        let a = pts[i];
        let b = pts[(i + 1) % n];
        if let Some(dir) = norm(sub(b, a)) {
            let nm = perp(dir);
            push_ring(
                out,
                vec![
                    along(a, nm, half),
                    along(b, nm, half),
                    along(b, nm, -half),
                    along(a, nm, -half),
                ],
            );
        }
    }

    // Joins: every vertex for a closed ring, interior vertices for an open one.
    if is_closed {
        for j in 0..n {
            add_join(
                pts[(j + n - 1) % n],
                pts[j],
                pts[(j + 1) % n],
                half,
                join,
                miter_limit,
                tol,
                out,
            );
        }
    } else {
        for j in 1..n - 1 {
            add_join(
                pts[j - 1],
                pts[j],
                pts[j + 1],
                half,
                join,
                miter_limit,
                tol,
                out,
            );
        }
        // Caps at the two free ends (outward = away from the neighbour).
        add_cap(pts[0], pts[1], half, cap, tol, out);
        add_cap(pts[n - 1], pts[n - 2], half, cap, tol, out);
    }
}

/// Fill the outer wedge where two segments meet at `v`.
#[allow(clippy::too_many_arguments)]
fn add_join(
    prev: Point,
    v: Point,
    next: Point,
    half: f32,
    join: LineJoin,
    miter_limit: f32,
    tol: f32,
    out: &mut Vec<Vec<Point>>,
) {
    let (Some(d0), Some(d1)) = (norm(sub(v, prev)), norm(sub(next, v))) else {
        return;
    };
    let cross = d0.0 * d1.1 - d0.1 * d1.0;
    if cross.abs() < EPS {
        return; // collinear or exact reversal: the segment quads already meet
    }
    let n0 = perp(d0);
    let n1 = perp(d1);
    // Outer side of the turn: a left turn (cross > 0) bulges on the right (−normal).
    let s = if cross > 0.0 { -half } else { half };
    let p0 = along(v, n0, s);
    let p1 = along(v, n1, s);
    match join {
        LineJoin::Bevel => push_ring(out, vec![v, p0, p1]),
        LineJoin::Round => {
            let mut ring = vec![v];
            arc(v, p0, p1, half, tol, &mut ring);
            push_ring(out, ring);
        }
        LineJoin::Miter => {
            if let Some(m) = intersect(p0, d0, p1, d1) {
                if dist(v, m) <= miter_limit * half {
                    push_ring(out, vec![v, p0, m, p1]);
                    return;
                }
            }
            push_ring(out, vec![v, p0, p1]); // beyond the miter limit → bevel
        }
    }
}

/// Cap the free `end` of an open contour; `neighbour` is the adjacent vertex, so
/// the outward direction is `end − neighbour`.
fn add_cap(
    end: Point,
    neighbour: Point,
    half: f32,
    cap: LineCap,
    tol: f32,
    out: &mut Vec<Vec<Point>>,
) {
    let Some(u) = norm(sub(end, neighbour)) else {
        return;
    };
    let nm = perp(u);
    match cap {
        LineCap::Butt => {}
        LineCap::Square => {
            let base_l = along(end, nm, half);
            let base_r = along(end, nm, -half);
            push_ring(
                out,
                vec![
                    base_l,
                    along(base_l, u, half),
                    along(base_r, u, half),
                    base_r,
                ],
            );
        }
        LineCap::Round => {
            // Semicircle: point(t) = end + half·(u·cos t + n·sin t), t ∈ [−π/2, π/2],
            // so the diameter ends land exactly on the segment quad's end corners.
            let steps = arc_steps(half, std::f32::consts::PI, tol);
            let mut ring = Vec::with_capacity(steps + 1);
            for k in 0..=steps {
                let t =
                    -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * (k as f32 / steps as f32);
                let (c, s) = (t.cos(), t.sin());
                ring.push(Point::new(
                    end.x + half * (u.0 * c + nm.0 * s),
                    end.y + half * (u.1 * c + nm.1 * s),
                ));
            }
            push_ring(out, ring);
        }
    }
}

/// Append the short arc from `from` to `to` about `centre` (radius `radius`).
fn arc(centre: Point, from: Point, to: Point, radius: f32, tol: f32, ring: &mut Vec<Point>) {
    let a0 = (from.y - centre.y).atan2(from.x - centre.x);
    let a1 = (to.y - centre.y).atan2(to.x - centre.x);
    let mut delta = a1 - a0;
    while delta <= -std::f32::consts::PI {
        delta += std::f32::consts::TAU;
    }
    while delta > std::f32::consts::PI {
        delta -= std::f32::consts::TAU;
    }
    let steps = arc_steps(radius, delta.abs(), tol);
    for k in 0..=steps {
        let a = a0 + delta * (k as f32 / steps as f32);
        ring.push(Point::new(
            centre.x + radius * a.cos(),
            centre.y + radius * a.sin(),
        ));
    }
}

/// Number of chord segments to approximate `sweep` radians at `radius` within `tol`.
fn arc_steps(radius: f32, sweep: f32, tol: f32) -> usize {
    let r = radius.max(tol);
    let max_step = 2.0 * (1.0 - tol / r).clamp(-1.0, 1.0).acos();
    if max_step <= EPS {
        return 1;
    }
    ((sweep / max_step).ceil() as usize).max(1)
}

fn circle(centre: Point, radius: f32, tol: f32) -> Vec<Point> {
    let steps = arc_steps(radius, std::f32::consts::TAU, tol).max(3);
    (0..steps)
        .map(|k| {
            let a = std::f32::consts::TAU * (k as f32 / steps as f32);
            Point::new(centre.x + radius * a.cos(), centre.y + radius * a.sin())
        })
        .collect()
}

fn axis_square(centre: Point, half: f32) -> Vec<Point> {
    vec![
        Point::new(centre.x - half, centre.y - half),
        Point::new(centre.x + half, centre.y - half),
        Point::new(centre.x + half, centre.y + half),
        Point::new(centre.x - half, centre.y + half),
    ]
}

/// Push a ring, normalising it to positive signed area so every emitted contour
/// shares one winding direction (required for the NonZero union).
fn push_ring(out: &mut Vec<Vec<Point>>, mut ring: Vec<Point>) {
    if ring.len() < 3 {
        return;
    }
    if signed_area(&ring) < 0.0 {
        ring.reverse();
    }
    out.push(ring);
}

fn signed_area(ring: &[Point]) -> f32 {
    let mut a = 0.0;
    for i in 0..ring.len() {
        let p = ring[i];
        let q = ring[(i + 1) % ring.len()];
        a += p.x * q.y - q.x * p.y;
    }
    a * 0.5
}

fn dist(a: Point, b: Point) -> f32 {
    (a.x - b.x).hypot(a.y - b.y)
}

fn sub(a: Point, b: Point) -> (f32, f32) {
    (a.x - b.x, a.y - b.y)
}

fn norm(v: (f32, f32)) -> Option<(f32, f32)> {
    let l = (v.0 * v.0 + v.1 * v.1).sqrt();
    if l <= EPS {
        None
    } else {
        Some((v.0 / l, v.1 / l))
    }
}

/// Left normal (+90° rotation).
fn perp(d: (f32, f32)) -> (f32, f32) {
    (-d.1, d.0)
}

fn along(p: Point, d: (f32, f32), t: f32) -> Point {
    Point::new(p.x + d.0 * t, p.y + d.1 * t)
}

/// Intersection of line `p0 + s·d0` with line `p1 + t·d1`, or `None` if parallel.
fn intersect(p0: Point, d0: (f32, f32), p1: Point, d1: (f32, f32)) -> Option<Point> {
    let denom = d0.0 * d1.1 - d0.1 * d1.0;
    if denom.abs() < EPS {
        return None;
    }
    let (rx, ry) = (p1.x - p0.x, p1.y - p0.y);
    let s = (rx * d1.1 - ry * d1.0) / denom;
    Some(Point::new(p0.x + d0.0 * s, p0.y + d0.1 * s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(rings: &[Vec<Point>]) -> (f32, f32, f32, f32) {
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        );
        for ring in rings {
            for p in ring {
                min_x = min_x.min(p.x);
                min_y = min_y.min(p.y);
                max_x = max_x.max(p.x);
                max_y = max_y.max(p.y);
            }
        }
        (min_x, min_y, max_x, max_y)
    }

    /// True if `pt` is inside the NonZero union of `rings` (ray-cast winding).
    fn inside(rings: &[Vec<Point>], pt: Point) -> bool {
        let mut wind = 0i32;
        for ring in rings {
            let n = ring.len();
            for i in 0..n {
                let a = ring[i];
                let b = ring[(i + 1) % n];
                let (y0, y1) = (a.y, b.y);
                if (y0 <= pt.y) != (y1 <= pt.y) {
                    let t = (pt.y - y0) / (y1 - y0);
                    let x = a.x + t * (b.x - a.x);
                    if x > pt.x {
                        wind += if y1 > y0 { 1 } else { -1 };
                    }
                }
            }
        }
        wind != 0
    }

    fn line(a: (f32, f32), b: (f32, f32)) -> Vec<Vec<Point>> {
        vec![vec![Point::new(a.0, a.1), Point::new(b.0, b.1)]]
    }

    #[test]
    fn butt_cap_does_not_extend_past_the_endpoint() {
        let pl = line((10.0, 10.0), (90.0, 10.0));
        let rings = stroke_outline_contours(
            &pl,
            &[false],
            5.0,
            LineCap::Butt,
            LineJoin::Miter,
            4.0,
            0.25,
        );
        let (min_x, _, max_x, _) = bounds(&rings);
        assert!(min_x >= 10.0 - EPS, "butt start flush at 10, got {min_x}");
        assert!(max_x <= 90.0 + EPS, "butt end flush at 90, got {max_x}");
        assert!(inside(&rings, Point::new(50.0, 10.0)), "body covered");
        assert!(
            !inside(&rings, Point::new(95.0, 10.0)),
            "nothing past the end"
        );
    }

    #[test]
    fn square_cap_projects_half_width_beyond_the_endpoint() {
        let pl = line((10.0, 10.0), (90.0, 10.0));
        let rings = stroke_outline_contours(
            &pl,
            &[false],
            5.0,
            LineCap::Square,
            LineJoin::Miter,
            4.0,
            0.25,
        );
        let (min_x, _, max_x, _) = bounds(&rings);
        assert!(
            (min_x - 5.0).abs() < 0.5,
            "square start reaches 5, got {min_x}"
        );
        assert!(
            (max_x - 95.0).abs() < 0.5,
            "square end reaches 95, got {max_x}"
        );
        assert!(
            inside(&rings, Point::new(93.0, 10.0)),
            "projecting cap covers 93"
        );
    }

    #[test]
    fn round_cap_is_a_half_disc_but_not_the_square_corners() {
        let pl = line((10.0, 10.0), (90.0, 10.0));
        let rings = stroke_outline_contours(
            &pl,
            &[false],
            10.0,
            LineCap::Round,
            LineJoin::Round,
            4.0,
            0.1,
        );
        // On-axis it reaches ~half beyond the end; the far corner does not.
        assert!(
            inside(&rings, Point::new(98.0, 10.0)),
            "round cap bulges on axis"
        );
        assert!(
            !inside(&rings, Point::new(99.0, 19.0)),
            "round cap has no square corner"
        );
    }

    #[test]
    fn miter_join_reaches_the_apex_and_bevel_does_not() {
        // A right-angle corner at (50,50): miter apex sticks out past the corner.
        let pl = vec![vec![
            Point::new(10.0, 50.0),
            Point::new(50.0, 50.0),
            Point::new(50.0, 90.0),
        ]];
        let miter = stroke_outline_contours(
            &pl,
            &[false],
            8.0,
            LineCap::Butt,
            LineJoin::Miter,
            4.0,
            0.25,
        );
        let bevel = stroke_outline_contours(
            &pl,
            &[false],
            8.0,
            LineCap::Butt,
            LineJoin::Bevel,
            4.0,
            0.25,
        );
        // The convex outer corner of this elbow is up-RIGHT of the vertex; the
        // miter apex sits near (58,42). A point just inside it is covered by the
        // miter quad but falls in the corner the bevel cuts off.
        let apex = Point::new(56.0, 44.0);
        assert!(inside(&miter, apex), "miter fills the apex");
        assert!(!inside(&bevel, apex), "bevel cuts the apex off");
    }

    #[test]
    fn miter_limit_falls_back_to_bevel_on_a_sharp_spike() {
        // A very sharp turn: the true miter is far out, so a limit of 2 clips it.
        let pl = vec![vec![
            Point::new(0.0, 0.0),
            Point::new(100.0, 0.0),
            Point::new(0.0, 6.0),
        ]];
        let sharp = stroke_outline_contours(
            &pl,
            &[false],
            4.0,
            LineCap::Butt,
            LineJoin::Miter,
            2.0,
            0.25,
        );
        // Well past where a 2× miter could reach — must be clipped away.
        assert!(
            !inside(&sharp, Point::new(140.0, 3.0)),
            "miter limit clips the runaway spike"
        );
    }

    #[test]
    fn closed_ring_is_hollow_and_has_no_caps() {
        // A 40×40 square outline (closed), stroke width 4 → half 2.
        let pl = vec![vec![
            Point::new(20.0, 20.0),
            Point::new(60.0, 20.0),
            Point::new(60.0, 60.0),
            Point::new(20.0, 60.0),
            Point::new(20.0, 20.0), // flattener repeats the start
        ]];
        let rings =
            stroke_outline_contours(&pl, &[true], 2.0, LineCap::Butt, LineJoin::Miter, 4.0, 0.25);
        assert!(inside(&rings, Point::new(20.0, 40.0)), "left edge painted");
        assert!(!inside(&rings, Point::new(40.0, 40.0)), "interior hollow");
        assert!(inside(&rings, Point::new(60.0, 40.0)), "right edge painted");
    }

    #[test]
    fn arrowheads_extend_open_path_and_keep_positive_winding() {
        let lines = vec![vec![Point::new(0.0, 0.0), Point::new(10.0, 0.0)]];
        let rings = arrowhead_contours(
            &lines,
            &[false],
            1.0,
            ArrowStyle::default(),
            ArrowStyle {
                kind: ArrowHead::Triangle,
                size: 3.0,
            },
            0.25,
        );
        assert_eq!(rings.len(), 1);
        assert!(rings[0].iter().any(|point| point.x > 10.0));
        assert!(signed_area(&rings[0]) > 0.0);
    }

    #[test]
    fn arrowheads_ignore_closed_paths_and_find_noncoincident_tangent() {
        let repeated = vec![vec![
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 0.0),
        ]];
        let arrow = ArrowStyle {
            kind: ArrowHead::Diamond,
            size: 3.0,
        };
        assert!(
            !arrowhead_contours(&repeated, &[false], 1.0, ArrowStyle::default(), arrow, 0.25)
                .is_empty()
        );
        assert!(arrowhead_contours(&repeated, &[true], 1.0, arrow, arrow, 0.25).is_empty());
    }
}

#![allow(dead_code)]
//! Boolean / shaping operations on vector paths (Phase 7 — Weld/Trim/Intersect/
//! Exclude). UI-free, like the rest of `core::vector`.
//!
//! Approach (MVP, honest about its limits):
//!   1. Flatten every input path to closed polygons (`flatten`, coarse tolerance
//!      to keep node counts sane — the result is re-rasterised for display anyway).
//!   2. Run a self-contained Greiner–Hormann polygon clip on the flattened rings.
//!      Both polygons live in ONE vertex arena so traversal across them is uniform.
//!   3. Coincident/degenerate edges (two rectangles sharing a side — very common
//!      when users snap shapes) break classic GH; we detect that and retry after
//!      nudging the clip polygon by a sub-pixel offset, which is invisible once the
//!      result is re-rasterised.
//!   4. The result is a *polygonal* [`PathData`] (straight segments) with the
//!      EvenOdd fill rule, so nested holes render correctly without orientation
//!      bookkeeping.
//!
//! LIMITATION: curves become line segments and open contours are treated as
//! closed. Curve-preserving boolean is future work; this is enough for the target
//! print/advertising shapes (rectangles, ellipses, polygons, stars, pen paths).

use crate::core::geometry::Point;
use crate::core::vector::flatten::flatten_path;
use crate::core::vector::path::{Contour, FillRule, Node, PathData};

/// The four shaping operations, in CorelDRAW vocabulary.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BooleanOp {
    /// Weld — union of all inputs.
    Union,
    /// Intersect — the region common to all inputs.
    Intersect,
    /// Trim — the first input with all later inputs subtracted.
    Difference,
    /// Exclude — symmetric difference (XOR).
    Exclude,
}

/// Flatten tolerance for boolean input. Coarser than the render default so a
/// welded circle does not explode into thousands of nodes; still well under a
/// pixel of error for on-screen shapes.
const BOOL_FLATTEN_TOL: f32 = 0.4;

/// Curve-fit tolerance for the boolean *output*. The result polygon is re-fitted
/// to clean cubic curves (see [`crate::core::vector::fit`]); this bounds how far
/// the smoothed boundary may sit from that polygon. Kept near the flatten
/// tolerance so combined shapes stay faithful while collapsing to few nodes.
const BOOL_FIT_TOL: f32 = 0.5;

/// Points closer than this (in path units) are treated as coincident.
const EPS: f32 = 1e-4;

/// Sub-pixel nudges applied to the clip polygon on successive retries when a
/// degenerate (coincident) intersection is detected. Small enough to be
/// invisible after re-rasterisation, irrational-ish so they don't re-align.
const JITTERS: [(f32, f32); 4] = [
    (0.0031, 0.0017),
    (-0.0023, 0.0041),
    (0.0052, -0.0029),
    (-0.0047, -0.0013),
];

type Ring = Vec<Point>;

/// Boolean of exactly two paths. `None` only on invalid input; an empty result
/// (e.g. intersection of disjoint shapes) is `Some` with no contours.
pub fn boolean(a: &PathData, b: &PathData, op: BooleanOp) -> Option<PathData> {
    let ar = to_rings(a, BOOL_FLATTEN_TOL);
    let br = to_rings(b, BOOL_FLATTEN_TOL);
    let out = match op {
        BooleanOp::Exclude => {
            // A ⊕ B = (A − B) ∪ (B − A). The two differences are AREA-disjoint —
            // they abut only along the old overlap boundary — so a boolean union
            // would just fight that fully-shared edge (every intersection
            // degenerate) and often fail. Concatenating the rings under EvenOdd is
            // both correct (a point is filled iff it lies in exactly one input)
            // and robust.
            let mut d1 =
                clip_with_retry(&ar, a.fill_rule, &br, b.fill_rule, BooleanOp::Difference)?;
            let d2 = clip_with_retry(&br, b.fill_rule, &ar, a.fill_rule, BooleanOp::Difference)?;
            d1.extend(d2);
            d1
        }
        _ => clip_with_retry(&ar, a.fill_rule, &br, b.fill_rule, op)?,
    };
    Some(rings_to_path(out))
}

/// Fold a boolean over many paths, left to right. Weld/Intersect/Exclude are
/// associative; Difference means "first minus all the rest". `paths` must be
/// non-empty.
pub fn boolean_many(paths: &[&PathData], op: BooleanOp) -> Option<PathData> {
    let mut it = paths.iter();
    let mut acc = (*it.next()?).clone();
    for p in it {
        acc = boolean(&acc, p, op)?;
        // Intersect/Difference can only shrink; once empty they stay empty.
        if acc.contours.is_empty() && matches!(op, BooleanOp::Intersect | BooleanOp::Difference) {
            break;
        }
    }
    Some(acc)
}

// ── flatten <-> ring conversion ──────────────────────────────────────────────

/// Flatten a path to closed rings (dropping the duplicated closing point and any
/// ring with fewer than 3 distinct points). Open contours are closed implicitly.
fn to_rings(path: &PathData, tol: f32) -> Vec<Ring> {
    let mut rings = Vec::new();
    for poly in flatten_path(path, tol) {
        let mut r = poly;
        // flatten_contour on a closed contour ends back at the start point.
        if r.len() >= 2 && r[0].distance_to(r[r.len() - 1]) < EPS {
            r.pop();
        }
        dedupe_consecutive(&mut r);
        if r.len() >= 3 {
            rings.push(r);
        }
    }
    rings
}

fn dedupe_consecutive(r: &mut Ring) {
    if r.is_empty() {
        return;
    }
    let mut out: Ring = Vec::with_capacity(r.len());
    for &p in r.iter() {
        if out.last().map_or(true, |&q| q.distance_to(p) >= EPS) {
            out.push(p);
        }
    }
    // A closed ring: also drop a last point coincident with the first.
    if out.len() >= 2 && out[0].distance_to(out[out.len() - 1]) < EPS {
        out.pop();
    }
    *r = out;
}

fn rings_to_path(rings: Vec<Ring>) -> PathData {
    let mut contours = Vec::new();
    for mut r in rings {
        simplify_collinear(&mut r);
        if r.len() < 3 || signed_area(&r).abs() < 1e-3 {
            continue;
        }
        contours.push(fit_ring_to_contour(r));
    }
    PathData::new(contours, FillRule::EvenOdd)
}

/// Re-fit a boolean-result ring to a clean cubic-Bézier contour so the user edits
/// a handful of nodes instead of the raw flattened polygon. Sharp corners survive
/// as cusps; on any failure (or a fit that would not reduce the node count) the
/// raw sharp polygon is kept — the boolean *topology* is never affected.
fn fit_ring_to_contour(r: Ring) -> Contour {
    if let Some(contour) = crate::core::vector::fit::fit_closed_ring(&r, BOOL_FIT_TOL) {
        if contour.nodes.len() >= 3 && contour.nodes.len() <= r.len() {
            return contour;
        }
    }
    let nodes = r.into_iter().map(Node::sharp).collect();
    Contour::new(nodes, true)
}

/// Drop a vertex that is collinear with its neighbours (within a small angular
/// tolerance) — flattening a long straight run can leave redundant points, and
/// boolean traversal copies original vertices verbatim.
fn simplify_collinear(r: &mut Ring) {
    if r.len() < 3 {
        return;
    }
    let n = r.len();
    let mut keep = vec![true; n];
    for i in 0..n {
        let a = r[(i + n - 1) % n];
        let b = r[i];
        let c = r[(i + 1) % n];
        let cross = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
        let scale = (b.distance_to(a) + c.distance_to(b)).max(1.0);
        if cross.abs() / scale < 1e-3 {
            keep[i] = false;
        }
    }
    let out: Ring = (0..n).filter(|&i| keep[i]).map(|i| r[i]).collect();
    if out.len() >= 3 {
        *r = out;
    }
}

// ── Greiner–Hormann ──────────────────────────────────────────────────────────

#[derive(Clone)]
struct Vert {
    p: Point,
    next: usize,
    prev: usize,
    is_int: bool,
    alpha: f32,
    neighbor: usize,
    entry: bool,
    visited: bool,
}

/// Run the clip, retrying with a nudged clip polygon if a degeneracy is hit.
fn clip_with_retry(
    subject: &[Ring],
    sub_fill: FillRule,
    clip: &[Ring],
    clip_fill: FillRule,
    op: BooleanOp,
) -> Option<Vec<Ring>> {
    if subject.is_empty() {
        // Nothing to subtract from / intersect / weld against.
        return Some(match op {
            BooleanOp::Union => clip.to_vec(),
            _ => Vec::new(),
        });
    }
    if clip.is_empty() {
        return Some(match op {
            BooleanOp::Intersect => Vec::new(),
            _ => subject.to_vec(), // Union & Difference keep the subject as-is.
        });
    }
    // Attempt 0 uses the clip verbatim; later attempts nudge it.
    for attempt in 0..=JITTERS.len() {
        let clip_rings: Vec<Ring> = if attempt == 0 {
            clip.to_vec()
        } else {
            let (dx, dy) = JITTERS[attempt - 1];
            clip.iter()
                .map(|r| r.iter().map(|p| Point::new(p.x + dx, p.y + dy)).collect())
                .collect()
        };
        match try_clip(subject, sub_fill, &clip_rings, clip_fill, op) {
            ClipResult::Ok(rings) => return Some(rings),
            ClipResult::Degenerate => continue,
        }
    }
    // Every nudge still hit a degeneracy — extremely unlikely for real shapes.
    None
}

enum ClipResult {
    Ok(Vec<Ring>),
    Degenerate,
}

fn try_clip(
    subject: &[Ring],
    sub_fill: FillRule,
    clip: &[Ring],
    clip_fill: FillRule,
    op: BooleanOp,
) -> ClipResult {
    let mut verts: Vec<Vert> = Vec::new();
    // Ring-start indices per polygon, plus the [start,end) vertex span of the
    // originals so we can walk each ring after splicing.
    let mut sub_ring_starts: Vec<usize> = Vec::new();
    let mut clip_ring_starts: Vec<usize> = Vec::new();

    build_poly(subject, &mut verts, &mut sub_ring_starts);
    let sub_orig_end = verts.len();
    build_poly(clip, &mut verts, &mut clip_ring_starts);
    let clip_orig_end = verts.len();

    // Original directed edges (a -> a.next) captured before any splicing.
    let sub_edges: Vec<(usize, usize)> = (0..sub_orig_end).map(|i| (i, verts[i].next)).collect();
    let clip_edges: Vec<(usize, usize)> = (sub_orig_end..clip_orig_end)
        .map(|i| (i, verts[i].next))
        .collect();

    // Intersection points, grouped for splicing after the arena stops moving.
    let mut sub_splices: Vec<(usize, f32, usize)> = Vec::new(); // (edge_a, alpha, vert)
    let mut clip_splices: Vec<(usize, f32, usize)> = Vec::new();

    for &(a0, a1) in &sub_edges {
        for &(b0, b1) in &clip_edges {
            match seg_intersect(verts[a0].p, verts[a1].p, verts[b0].p, verts[b1].p) {
                Cross::Proper { t, u, p } => {
                    let si = verts.len();
                    let ci = si + 1;
                    verts.push(Vert {
                        p,
                        next: 0,
                        prev: 0,
                        is_int: true,
                        alpha: t,
                        neighbor: ci,
                        entry: false,
                        visited: false,
                    });
                    verts.push(Vert {
                        p,
                        next: 0,
                        prev: 0,
                        is_int: true,
                        alpha: u,
                        neighbor: si,
                        entry: false,
                        visited: false,
                    });
                    sub_splices.push((a0, t, si));
                    clip_splices.push((b0, u, ci));
                }
                Cross::Degenerate => return ClipResult::Degenerate,
                Cross::None => {}
            }
        }
    }

    if sub_splices.is_empty() {
        // No crossings: the result is decided purely by containment.
        return ClipResult::Ok(no_crossing_result(subject, sub_fill, clip, clip_fill, op));
    }

    splice_edges(&mut verts, &sub_edges, &mut sub_splices);
    splice_edges(&mut verts, &clip_edges, &mut clip_splices);

    // Entry/exit flags. Raw flag = "this crossing enters the OTHER polygon";
    // the op then decides whether to keep or invert (per polygon).
    let (inv_sub, inv_clip) = match op {
        BooleanOp::Union => (true, true),
        BooleanOp::Intersect => (false, false),
        BooleanOp::Difference => (false, true),
        BooleanOp::Exclude => (false, false), // never reached (handled by boolean()).
    };
    mark_entries(&mut verts, &sub_ring_starts, clip, clip_fill, inv_sub);
    mark_entries(&mut verts, &clip_ring_starts, subject, sub_fill, inv_clip);

    ClipResult::Ok(trace_result(&mut verts, sub_orig_end))
}

fn build_poly(rings: &[Ring], verts: &mut Vec<Vert>, ring_starts: &mut Vec<usize>) {
    for ring in rings {
        if ring.len() < 3 {
            continue;
        }
        let base = verts.len();
        ring_starts.push(base);
        let n = ring.len();
        for (i, p) in ring.iter().enumerate() {
            verts.push(Vert {
                p: *p,
                next: base + (i + 1) % n,
                prev: base + (i + n - 1) % n,
                is_int: false,
                alpha: 0.0,
                neighbor: 0,
                entry: false,
                visited: false,
            });
        }
    }
}

/// Splice intersection vertices into each original edge, ordered along it.
fn splice_edges(
    verts: &mut [Vert],
    edges: &[(usize, usize)],
    splices: &mut Vec<(usize, f32, usize)>,
) {
    for &(a0, a1) in edges {
        let mut ins: Vec<(f32, usize)> = splices
            .iter()
            .filter(|(e, _, _)| *e == a0)
            .map(|(_, alpha, v)| (*alpha, *v))
            .collect();
        if ins.is_empty() {
            continue;
        }
        ins.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut prev = a0;
        for (_, v) in ins {
            verts[prev].next = v;
            verts[v].prev = prev;
            prev = v;
        }
        verts[prev].next = a1;
        verts[a1].prev = prev;
    }
}

/// Walk each ring from its start, toggling inside/outside status of the OTHER
/// polygon at every intersection to label it entry/exit, then apply the op's
/// inversion.
fn mark_entries(
    verts: &mut [Vert],
    ring_starts: &[usize],
    other: &[Ring],
    other_fill: FillRule,
    invert: bool,
) {
    for &start in ring_starts {
        let mut inside = point_inside(other, verts[start].p, other_fill);
        let mut cur = verts[start].next;
        // Guard against a malformed cycle.
        let mut guard = 0usize;
        let limit = verts.len() * 4 + 16;
        while cur != start && guard < limit {
            if verts[cur].is_int {
                // Crossing from `inside` flips it; entry when we were outside.
                let entry = !inside;
                verts[cur].entry = entry ^ invert;
                inside = !inside;
            }
            cur = verts[cur].next;
            guard += 1;
        }
    }
}

/// Trace result rings by alternating between the two polygons at intersections.
fn trace_result(verts: &mut [Vert], _sub_orig_end: usize) -> Vec<Ring> {
    let mut rings = Vec::new();
    loop {
        // Find an unvisited intersection to start from.
        let Some(start) = verts.iter().position(|v| v.is_int && !v.visited) else {
            break;
        };
        let mut ring: Ring = Vec::new();
        let mut cur = start;
        let mut guard = 0usize;
        let limit = verts.len() * 4 + 16;
        loop {
            verts[cur].visited = true;
            let nb = verts[cur].neighbor;
            verts[nb].visited = true;
            let forward = verts[cur].entry;
            // Walk along this polygon until the next intersection.
            loop {
                cur = if forward {
                    verts[cur].next
                } else {
                    verts[cur].prev
                };
                ring.push(verts[cur].p);
                guard += 1;
                if verts[cur].is_int || guard > limit {
                    break;
                }
            }
            if guard > limit {
                break;
            }
            // Switch to the paired vertex on the other polygon.
            cur = verts[cur].neighbor;
            if cur == start || verts[cur].visited {
                break;
            }
        }
        if ring.len() >= 3 {
            rings.push(ring);
        }
    }
    rings
}

/// Result when the two polygons have no proper crossings — decided by which
/// contains which.
fn no_crossing_result(
    subject: &[Ring],
    sub_fill: FillRule,
    clip: &[Ring],
    clip_fill: FillRule,
    op: BooleanOp,
) -> Vec<Ring> {
    // With no boundary crossings the shapes are nested or disjoint, so a single
    // BOUNDARY vertex is a valid witness: a vertex of A is inside B iff A ⊆ B
    // (an interior point could sit in the mere overlap region and mislead).
    let sub_in_clip = subject
        .iter()
        .all(|r| r.first().is_some_and(|&p| point_inside(clip, p, clip_fill)));
    let clip_in_sub = clip.iter().all(|r| {
        r.first()
            .is_some_and(|&p| point_inside(subject, p, sub_fill))
    });

    match op {
        BooleanOp::Union => {
            if sub_in_clip {
                clip.to_vec()
            } else if clip_in_sub {
                subject.to_vec()
            } else {
                let mut v = subject.to_vec();
                v.extend(clip.iter().cloned());
                v
            }
        }
        BooleanOp::Intersect => {
            if sub_in_clip {
                subject.to_vec()
            } else if clip_in_sub {
                clip.to_vec()
            } else {
                Vec::new()
            }
        }
        BooleanOp::Difference => {
            if sub_in_clip {
                Vec::new()
            } else if clip_in_sub {
                // Clip carves a hole: subject outer + clip as a nested ring
                // (EvenOdd renders the hole regardless of orientation).
                let mut v = subject.to_vec();
                v.extend(clip.iter().cloned());
                v
            } else {
                subject.to_vec()
            }
        }
        BooleanOp::Exclude => {
            let mut v = subject.to_vec();
            v.extend(clip.iter().cloned());
            v
        }
    }
}

// ── geometry helpers ─────────────────────────────────────────────────────────

enum Cross {
    Proper { t: f32, u: f32, p: Point },
    Degenerate,
    None,
}

/// Intersection of segment a0a1 with b0b1. A crossing strictly interior to both
/// segments is `Proper`; one that lands on an endpoint (or a collinear overlap)
/// is `Degenerate` so the caller can nudge and retry.
fn seg_intersect(a0: Point, a1: Point, b0: Point, b1: Point) -> Cross {
    let rx = a1.x - a0.x;
    let ry = a1.y - a0.y;
    let sx = b1.x - b0.x;
    let sy = b1.y - b0.y;
    let denom = rx * sy - ry * sx;
    let qpx = b0.x - a0.x;
    let qpy = b0.y - a0.y;
    if denom.abs() < 1e-9 {
        // Parallel. Collinear + overlapping is a degeneracy; otherwise no hit.
        let collinear = (qpx * ry - qpy * rx).abs() < 1e-6;
        if collinear && overlap_1d(a0, a1, b0, b1) {
            return Cross::Degenerate;
        }
        return Cross::None;
    }
    let t = (qpx * sy - qpy * sx) / denom;
    let u = (qpx * ry - qpy * rx) / denom;
    let on_a = (-EPS..=1.0 + EPS).contains(&t);
    let on_b = (-EPS..=1.0 + EPS).contains(&u);
    if !on_a || !on_b {
        return Cross::None;
    }
    // Interior to both? Otherwise it touches an endpoint → degenerate.
    if t <= EPS || t >= 1.0 - EPS || u <= EPS || u >= 1.0 - EPS {
        return Cross::Degenerate;
    }
    Cross::Proper {
        t,
        u,
        p: Point::new(a0.x + t * rx, a0.y + t * ry),
    }
}

fn overlap_1d(a0: Point, a1: Point, b0: Point, b1: Point) -> bool {
    // Project onto the dominant axis and test 1-D interval overlap.
    let horiz = (a1.x - a0.x).abs() >= (a1.y - a0.y).abs();
    let (mut a_lo, mut a_hi, mut b_lo, mut b_hi) = if horiz {
        (a0.x, a1.x, b0.x, b1.x)
    } else {
        (a0.y, a1.y, b0.y, b1.y)
    };
    if a_lo > a_hi {
        std::mem::swap(&mut a_lo, &mut a_hi);
    }
    if b_lo > b_hi {
        std::mem::swap(&mut b_lo, &mut b_hi);
    }
    a_lo < b_hi - EPS && b_lo < a_hi - EPS
}

/// Point-in-polygon over a set of rings, honouring the fill rule. Ray cast to
/// +x counting crossings (parity) and signed windings (nonzero).
fn point_inside(rings: &[Ring], p: Point, fill: FillRule) -> bool {
    let mut parity = 0i32;
    let mut winding = 0i32;
    for r in rings {
        let n = r.len();
        for i in 0..n {
            let a = r[i];
            let b = r[(i + 1) % n];
            // Does the horizontal ray at p.y cross edge a-b?
            let (lo, hi, dir) = if a.y <= b.y { (a, b, 1) } else { (b, a, -1) };
            if p.y >= lo.y && p.y < hi.y {
                // x of the edge at height p.y.
                let t = (p.y - lo.y) / (hi.y - lo.y);
                let x = lo.x + t * (hi.x - lo.x);
                if x > p.x {
                    parity ^= 1;
                    winding += dir;
                }
            }
        }
    }
    match fill {
        FillRule::NonZero => winding != 0,
        FillRule::EvenOdd => parity != 0,
    }
}

fn signed_area(r: &Ring) -> f32 {
    let n = r.len();
    let mut a = 0.0;
    for i in 0..n {
        let p = r[i];
        let q = r[(i + 1) % n];
        a += p.x * q.y - q.x * p.y;
    }
    a * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::vector::flatten::flatten_path;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> PathData {
        PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(x, y)),
                    Node::sharp(Point::new(x + w, y)),
                    Node::sharp(Point::new(x + w, y + h)),
                    Node::sharp(Point::new(x, y + h)),
                ],
                true,
            )],
            FillRule::NonZero,
        )
    }

    /// Area covered by a path's fill, measured on the flattened rings with its
    /// declared fill rule (so a hole subtracts). Sampled independent of the
    /// boolean code so it is a real cross-check.
    fn filled_area(path: &PathData) -> f32 {
        let rings = to_rings(path, 0.1);
        if rings.is_empty() {
            return 0.0;
        }
        // Bounding box.
        let mut minx = f32::INFINITY;
        let mut miny = f32::INFINITY;
        let mut maxx = f32::NEG_INFINITY;
        let mut maxy = f32::NEG_INFINITY;
        for r in &rings {
            for p in r {
                minx = minx.min(p.x);
                miny = miny.min(p.y);
                maxx = maxx.max(p.x);
                maxy = maxy.max(p.y);
            }
        }
        // Monte-Carlo-free: dense grid sampling.
        let step = 0.5f32;
        let mut area = 0.0;
        let mut y = miny + step * 0.5;
        while y < maxy {
            let mut x = minx + step * 0.5;
            while x < maxx {
                if point_inside(&rings, Point::new(x, y), path.fill_rule) {
                    area += step * step;
                }
                x += step;
            }
            y += step;
        }
        area
    }

    #[test]
    fn union_of_overlapping_squares() {
        // Two 10×10 squares overlapping in a 5×5 corner.
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(5.0, 5.0, 10.0, 10.0);
        let u = boolean(&a, &b, BooleanOp::Union).unwrap();
        // Area = 100 + 100 − 25 = 175.
        let area = filled_area(&u);
        assert!((area - 175.0).abs() < 6.0, "union area {area}");
    }

    #[test]
    fn intersect_of_overlapping_squares() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(5.0, 5.0, 10.0, 10.0);
        let i = boolean(&a, &b, BooleanOp::Intersect).unwrap();
        // Overlap is the 5×5 corner = 25.
        let area = filled_area(&i);
        assert!((area - 25.0).abs() < 4.0, "intersect area {area}");
    }

    #[test]
    fn difference_of_overlapping_squares() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(5.0, 5.0, 10.0, 10.0);
        let d = boolean(&a, &b, BooleanOp::Difference).unwrap();
        // A minus the 5×5 overlap = 75 (an L shape).
        let area = filled_area(&d);
        assert!((area - 75.0).abs() < 5.0, "difference area {area}");
    }

    #[test]
    fn difference_makes_a_hole() {
        // Small square fully inside a big one → big with a hole.
        let big = rect(0.0, 0.0, 20.0, 20.0);
        let small = rect(5.0, 5.0, 10.0, 10.0);
        let d = boolean(&big, &small, BooleanOp::Difference).unwrap();
        // 400 − 100 = 300.
        let area = filled_area(&d);
        assert!((area - 300.0).abs() < 6.0, "holed area {area}");
        // A point in the hole centre must be OUTSIDE the filled result.
        let rings = to_rings(&d, 0.1);
        assert!(
            !point_inside(&rings, Point::new(10.0, 10.0), d.fill_rule),
            "hole centre should be empty"
        );
    }

    #[test]
    fn intersect_disjoint_is_empty() {
        let a = rect(0.0, 0.0, 5.0, 5.0);
        let b = rect(100.0, 100.0, 5.0, 5.0);
        let i = boolean(&a, &b, BooleanOp::Intersect).unwrap();
        assert!(i.contours.is_empty(), "disjoint intersection is empty");
    }

    #[test]
    fn union_disjoint_keeps_both() {
        let a = rect(0.0, 0.0, 5.0, 5.0);
        let b = rect(100.0, 100.0, 5.0, 5.0);
        let u = boolean(&a, &b, BooleanOp::Union).unwrap();
        assert_eq!(u.contours.len(), 2, "disjoint union has two rings");
        assert!((filled_area(&u) - 50.0).abs() < 3.0);
    }

    #[test]
    fn shared_edge_union_is_robust() {
        // Two squares sharing a full vertical edge — a classic GH degeneracy that
        // the nudge-and-retry path must resolve. Combined area 200.
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(10.0, 0.0, 10.0, 10.0);
        let u = boolean(&a, &b, BooleanOp::Union).unwrap();
        let area = filled_area(&u);
        assert!((area - 200.0).abs() < 8.0, "shared-edge union area {area}");
    }

    #[test]
    fn exclude_of_overlapping_squares() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(5.0, 5.0, 10.0, 10.0);
        let x = boolean(&a, &b, BooleanOp::Exclude).unwrap();
        // XOR = union − intersection = 175 − 25 = 150.
        let area = filled_area(&x);
        assert!((area - 150.0).abs() < 8.0, "exclude area {area}");
        // The shared corner must be empty.
        let rings = to_rings(&x, 0.1);
        assert!(!point_inside(&rings, Point::new(7.5, 7.5), x.fill_rule));
    }

    #[test]
    fn boolean_many_welds_three() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(8.0, 0.0, 10.0, 10.0);
        let c = rect(16.0, 0.0, 10.0, 10.0);
        let u = boolean_many(&[&a, &b, &c], BooleanOp::Union).unwrap();
        // 300 − 2 overlaps of 2×10 = 300 − 40 = 260.
        let area = filled_area(&u);
        assert!((area - 260.0).abs() < 10.0, "three-way weld area {area}");
    }

    #[test]
    fn result_is_valid_pathdata() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(5.0, 5.0, 10.0, 10.0);
        for op in [
            BooleanOp::Union,
            BooleanOp::Intersect,
            BooleanOp::Difference,
            BooleanOp::Exclude,
        ] {
            let r = boolean(&a, &b, op).unwrap();
            assert!(r.validate().is_ok(), "op {op:?} produced invalid path");
            // Every result contour flattens without panicking.
            let _ = flatten_path(&r, 0.5);
        }
    }
}

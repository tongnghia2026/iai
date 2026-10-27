//! Boolean / shaping operations on vector paths (Phase 7 — Weld/Trim/Intersect/
//! Exclude). UI-free, like the rest of `core::vector`.
//!
//! Approach:
//!   1. Flatten every input path to closed polygons (`flatten`, coarse tolerance
//!      to keep node counts sane — the result is re-rasterised for display anyway).
//!   2. Run the integer-backed `i_overlay` sweep-line engine on the flattened
//!      rings. It handles coincident edges and repeated multi-object folds without
//!      perturbing geometry between pairs.
//!   3. The result is a *polygonal* [`PathData`] (straight segments) with the
//!      EvenOdd fill rule, so nested holes render correctly without orientation
//!      bookkeeping.
//!
//! LIMITATION: curves become line segments and open contours are treated as
//! closed. Curve-preserving boolean is future work; this is enough for the target
//! print/advertising shapes (rectangles, ellipses, polygons, stars, pen paths).

use crate::vector::flatten::flatten_path;
use crate::vector::path::{Contour, FillRule, Node, PathData};
use i_overlay::core::fill_rule::FillRule as OverlayFillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::single::SingleFloatOverlay;
use iai_geom::geometry::Point;

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

type Ring = Vec<Point>;

/// Boolean of exactly two paths. `None` only on invalid input; an empty result
/// (e.g. intersection of disjoint shapes) is `Some` with no contours.
pub fn boolean(a: &PathData, b: &PathData, op: BooleanOp) -> Option<PathData> {
    let ar = to_rings(a, BOOL_FLATTEN_TOL);
    let br = to_rings(b, BOOL_FLATTEN_TOL);
    let out = boolean_rings(&ar, a.fill_rule, &br, b.fill_rule, op)?;
    Some(rings_to_path(out))
}

fn boolean_rings(
    subject: &[Ring],
    _subject_fill: FillRule,
    clip: &[Ring],
    _clip_fill: FillRule,
    op: BooleanOp,
) -> Option<Vec<Ring>> {
    let subject: Vec<Vec<[f32; 2]>> = subject
        .iter()
        .map(|ring| ring.iter().map(|p| [p.x, p.y]).collect())
        .collect();
    let clip: Vec<Vec<[f32; 2]>> = clip
        .iter()
        .map(|ring| ring.iter().map(|p| [p.x, p.y]).collect())
        .collect();
    let rule = match op {
        BooleanOp::Union => OverlayRule::Union,
        BooleanOp::Intersect => OverlayRule::Intersect,
        BooleanOp::Difference => OverlayRule::Difference,
        BooleanOp::Exclude => OverlayRule::Xor,
    };
    let shapes = subject.overlay_as::<i64>(&clip, rule, OverlayFillRule::EvenOdd);
    Some(
        shapes
            .into_iter()
            .flatten()
            .map(|ring| ring.into_iter().map(|p| Point::new(p[0], p[1])).collect())
            .collect(),
    )
}

/// Fold a boolean over many paths, left to right. Weld/Intersect/Exclude are
/// associative; Difference means "first minus all the rest". `paths` must be
/// non-empty.
pub fn boolean_many(paths: &[&PathData], op: BooleanOp) -> Option<PathData> {
    let mut it = paths.iter();
    let first = *it.next()?;
    let mut acc = to_rings(first, BOOL_FLATTEN_TOL);
    let mut acc_fill = first.fill_rule;
    for p in it {
        let next = to_rings(p, BOOL_FLATTEN_TOL);
        acc = boolean_rings(&acc, acc_fill, &next, p.fill_rule, op)?;
        // Do not curve-fit an intermediate pair: fitting perturbs the boundary
        // before the next selected layer is folded in and can lose crossings.
        acc_fill = FillRule::EvenOdd;
        // Intersect/Difference can only shrink; once empty they stay empty.
        if acc.is_empty() && matches!(op, BooleanOp::Intersect | BooleanOp::Difference) {
            break;
        }
    }
    Some(rings_to_path(acc))
}

/// CorelDRAW-style multi-object Intersect: return every region covered by at
/// least two inputs. Pairwise intersections are unioned, so a three-way overlap
/// appears once while chain overlaps survive even when no point belongs to all
/// selected objects.
pub fn intersect_overlaps(paths: &[&PathData]) -> Option<PathData> {
    if paths.len() < 2 {
        return None;
    }

    let rings: Vec<Vec<Ring>> = paths
        .iter()
        .map(|path| to_rings(path, BOOL_FLATTEN_TOL))
        .collect();
    let mut overlap: Vec<Ring> = Vec::new();

    for i in 0..rings.len() {
        for j in (i + 1)..rings.len() {
            let pair = boolean_rings(
                &rings[i],
                paths[i].fill_rule,
                &rings[j],
                paths[j].fill_rule,
                BooleanOp::Intersect,
            )?;
            if pair.is_empty() {
                continue;
            }
            overlap = if overlap.is_empty() {
                pair
            } else {
                boolean_rings(
                    &overlap,
                    FillRule::EvenOdd,
                    &pair,
                    FillRule::EvenOdd,
                    BooleanOp::Union,
                )?
            };
        }
    }

    Some(rings_to_path(overlap))
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
        if out.last().is_none_or(|&q| q.distance_to(p) >= EPS) {
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
    if let Some(contour) = crate::vector::fit::fit_closed_ring(&r, BOOL_FIT_TOL)
        && contour.nodes.len() >= 3
        && contour.nodes.len() <= r.len()
    {
        return contour;
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

// ── geometry helpers ─────────────────────────────────────────────────────────

/// Point-in-polygon over a set of rings, honouring the fill rule. Ray cast to
/// +x counting crossings (parity) and signed windings (nonzero).
#[cfg(test)]
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
    use crate::vector::flatten::flatten_path;

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
    fn multi_intersect_keeps_coverage_of_two_without_common_coverage_of_three() {
        let a = rect(0.0, 0.0, 80.0, 80.0);
        let b = rect(40.0, 0.0, 80.0, 80.0);
        let c = rect(80.0, 0.0, 80.0, 80.0);
        let overlap = intersect_overlaps(&[&a, &b, &c]).unwrap();
        let area = filled_area(&overlap);
        assert!((area - 6400.0).abs() < 20.0, "coverage>=2 area {area}");
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

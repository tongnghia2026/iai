//! The designer's path tools that are not booleans: Contour / Offset Path
//! (a parallel outline at a distance, outward or inward), Convert Outline to
//! Object (a stroke turned into the filled shape it paints), Combine and
//! Break Apart (contours gathered into one path, or split into their parts
//! with each part's holes kept).
//!
//! Like [`crate::vector::boolean`], the geometry is worked on flattened rings
//! by the `i_overlay` engine and the result is re-fitted to clean curves.
//! Paths are in canvas space; styles are the caller's.

use crate::vector::boolean::{rings_to_path, rings_to_sharp_path, to_rings};
use crate::vector::flatten::flatten_path;
use crate::vector::path::{FillRule, PathData};
use crate::vector::stroke::stroke_outline_contours;
use crate::vector::style::{LineCap, LineJoin};
use i_overlay::core::fill_rule::FillRule as OverlayFillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::single::SingleFloatOverlay;
use iai_geom::geometry::Point;

/// Flattening for these tools: fine enough that an offset of a small letter
/// keeps its shape.
const TOL: f32 = 0.2;

type Ring = Vec<Point>;

fn to_xy(rings: &[Ring]) -> Vec<Vec<[f32; 2]>> {
    rings
        .iter()
        .map(|r| r.iter().map(|p| [p.x, p.y]).collect())
        .collect()
}

fn from_xy(shapes: Vec<Vec<Vec<[f32; 2]>>>) -> Vec<Ring> {
    shapes
        .into_iter()
        .flatten()
        .map(|r| r.into_iter().map(|p| Point::new(p[0], p[1])).collect())
        .collect()
}

/// `a` op `b`, `a` read with `a_rule` and `b` with `b_rule` folded in by a
/// first pass (an overlay only takes one fill rule).
fn overlay(a: &[Ring], b: &[Ring], rule: OverlayRule, fill: OverlayFillRule) -> Vec<Ring> {
    from_xy(to_xy(a).overlay_as::<i64>(&to_xy(b), rule, fill))
}

/// The area `rings` cover by the non-zero rule, as clean rings.
fn nonzero(rings: &[Ring]) -> Vec<Ring> {
    overlay(rings, &[], OverlayRule::Subject, OverlayFillRule::NonZero)
}

/// The area a path fills, as clean rings (its own fill rule honoured).
fn filled(path: &PathData) -> Vec<Ring> {
    let rings = to_rings(path, TOL);
    let fill = match path.fill_rule {
        FillRule::NonZero => OverlayFillRule::NonZero,
        FillRule::EvenOdd => OverlayFillRule::EvenOdd,
    };
    overlay(&rings, &[], OverlayRule::Subject, fill)
}

/// The band `half` either side of every edge of `rings`.
fn band(rings: &[Ring], half: f32, join: LineJoin, miter: f32) -> Vec<Ring> {
    let closed = vec![true; rings.len()];
    let strokes = stroke_outline_contours(rings, &closed, half, LineCap::Butt, join, miter, TOL);
    nonzero(&strokes)
}

/// Contour / Offset Path: the outline of `path`'s filled area moved
/// `distance` outward (positive) or inward (negative), its corners `join`
/// (round, sharp up to `miter`, or cut). `None` when nothing is left (an
/// inward offset past the middle) or the path fills nothing.
pub fn offset_path(path: &PathData, distance: f32, join: LineJoin, miter: f32) -> Option<PathData> {
    let area = filled(path);
    if area.is_empty() || !distance.is_finite() {
        return None;
    }
    if distance.abs() < 1e-3 {
        return Some(rings_to_path(area));
    }
    let edge = band(&area, distance.abs(), join, miter.max(1.0));
    let out = if distance > 0.0 {
        overlay(&area, &edge, OverlayRule::Union, OverlayFillRule::EvenOdd)
    } else {
        overlay(
            &area,
            &edge,
            OverlayRule::Difference,
            OverlayFillRule::EvenOdd,
        )
    };
    let out = rings_to_path(out);
    (!out.contours.is_empty()).then_some(out)
}

/// Convert Outline to Object: the area a stroke of `width` paints along
/// `path` (open ends get `cap`), as a filled path. `None` for no stroke.
pub fn outline_to_path(
    path: &PathData,
    width: f32,
    cap: LineCap,
    join: LineJoin,
    miter: f32,
) -> Option<PathData> {
    if width.is_nan() || width <= 0.0 {
        return None;
    }
    let lines = flatten_path(path, TOL);
    let closed: Vec<bool> = path.contours.iter().map(|c| c.closed).collect();
    let strokes = stroke_outline_contours(&lines, &closed, width * 0.5, cap, join, miter, TOL);
    let out = rings_to_path(nonzero(&strokes));
    (!out.contours.is_empty()).then_some(out)
}

/// Combine: every contour of `paths` in one path. Overlaps cut holes (even
/// odd), as in CorelDRAW; the letters of a word stay one object.
pub fn combine(paths: &[&PathData]) -> PathData {
    let contours = paths
        .iter()
        .flat_map(|p| p.contours.iter().cloned())
        .collect();
    PathData::new(contours, FillRule::EvenOdd)
}

/// Break Apart: each separate piece of `path` on its own, its holes kept
/// with it (the counter of an «o» stays a hole, not a black dot). Pieces in
/// reading order: top to bottom, then left to right.
pub fn break_apart(path: &PathData) -> Vec<PathData> {
    let n = path.contours.len();
    if n <= 1 {
        return vec![path.clone()];
    }
    let rings: Vec<Ring> = path
        .contours
        .iter()
        .map(|c| {
            let one = PathData::new(vec![c.clone()], FillRule::NonZero);
            flatten_path(&one, TOL)
                .into_iter()
                .next()
                .unwrap_or_default()
        })
        .collect();
    // How many other contours each lies inside: even = a piece, odd = a hole.
    let inside = |k: usize, of: usize| -> bool {
        k != of
            && rings[k].first().is_some_and(|p| contains(&rings[of], *p))
            && area(&rings[of]).abs() > area(&rings[k]).abs()
    };
    let depth: Vec<usize> = (0..n)
        .map(|k| (0..n).filter(|&of| inside(k, of)).count())
        .collect();
    let mut parts: Vec<(usize, Vec<usize>)> = (0..n)
        .filter(|&k| depth[k].is_multiple_of(2) && path.contours[k].closed)
        .map(|k| (k, vec![k]))
        .collect();
    // Open contours stand alone.
    for k in 0..n {
        if !path.contours[k].closed {
            parts.push((k, vec![k]));
        }
    }
    for k in 0..n {
        if depth[k] % 2 == 1 && path.contours[k].closed {
            // The smallest piece around it is its owner.
            let owner = parts
                .iter_mut()
                .filter(|(o, _)| {
                    path.contours[*o].closed && inside(k, *o) && depth[*o] + 1 == depth[k]
                })
                .min_by(|a, b| area(&rings[a.0]).abs().total_cmp(&area(&rings[b.0]).abs()));
            match owner {
                Some((_, members)) => members.push(k),
                None => parts.push((k, vec![k])),
            }
        }
    }
    let top_left = |k: usize| -> (f32, f32) {
        rings[k]
            .iter()
            .fold((f32::MAX, f32::MAX), |(y, x), p| (y.min(p.y), x.min(p.x)))
    };
    parts.sort_by(|a, b| {
        let (ay, ax) = top_left(a.0);
        let (by, bx) = top_left(b.0);
        // Same row when the tops are close.
        if (ay - by).abs() > 2.0 {
            ay.total_cmp(&by)
        } else {
            ax.total_cmp(&bx)
        }
    });
    parts
        .into_iter()
        .map(|(_, members)| {
            PathData::new(
                members.iter().map(|&k| path.contours[k].clone()).collect(),
                FillRule::EvenOdd,
            )
        })
        .collect()
}

fn area(r: &Ring) -> f32 {
    let n = r.len();
    (0..n)
        .map(|i| {
            let (p, q) = (r[i], r[(i + 1) % n]);
            p.x * q.y - q.x * p.y
        })
        .sum::<f32>()
        * 0.5
}

fn contains(r: &Ring, p: Point) -> bool {
    let n = r.len();
    let mut inside = false;
    for i in 0..n {
        let (a, b) = (r[i], r[(i + 1) % n]);
        if (a.y > p.y) != (b.y > p.y) {
            let x = a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x);
            if x > p.x {
                inside = !inside;
            }
        }
    }
    inside
}

/// Extrude (CorelDRAW) / the side of a 3D letter: the area `path`'s filled
/// shape sweeps moving straight by (dx, dy) — the shape, its moved copy and
/// every edge's band between them, as one filled path. `None` when the
/// path fills nothing.
pub fn sweep(path: &PathData, dx: f32, dy: f32) -> Option<PathData> {
    let area = filled(path);
    if area.is_empty() || !(dx.is_finite() && dy.is_finite()) {
        return None;
    }
    let mut rings: Vec<Ring> = Vec::new();
    for r in &area {
        let n = r.len();
        // Each ring and its copy, the same way round so non-zero adds them.
        let (r, moved): (Ring, Ring) = if area_of(r) < 0.0 {
            let back: Ring = r.iter().rev().copied().collect();
            let moved = back
                .iter()
                .map(|p| Point::new(p.x + dx, p.y + dy))
                .collect();
            (back, moved)
        } else {
            let moved = r.iter().map(|p| Point::new(p.x + dx, p.y + dy)).collect();
            (r.clone(), moved)
        };
        for i in 0..n {
            let (a, b) = (r[i], r[(i + 1) % n]);
            let quad = vec![
                a,
                b,
                Point::new(b.x + dx, b.y + dy),
                Point::new(a.x + dx, a.y + dy),
            ];
            // Every band counter-clockwise too.
            rings.push(if area_of(&quad) < 0.0 {
                quad.into_iter().rev().collect()
            } else {
                quad
            });
        }
        rings.push(r);
        rings.push(moved);
    }
    // Holes are covered by the sweep's bands: only the outside shows.
    let out = rings_to_sharp_path(nonzero(&rings));
    (!out.contours.is_empty()).then_some(out)
}

/// `ring` walked into `n` points evenly spaced along it (closed), starting
/// at its first point.
fn resample(ring: &[Point], n: usize) -> Ring {
    let m = ring.len();
    if m == 0 || n == 0 {
        return Vec::new();
    }
    let mut at = Vec::with_capacity(m + 1);
    let mut len = 0.0f32;
    at.push(0.0);
    for i in 0..m {
        len += ring[i].distance_to(ring[(i + 1) % m]);
        at.push(len);
    }
    let len = len.max(1e-6);
    let mut out = Vec::with_capacity(n);
    let mut seg = 0usize;
    for k in 0..n {
        let s = len * k as f32 / n as f32;
        while seg + 1 < m && at[seg + 1] < s {
            seg += 1;
        }
        let (a, b) = (ring[seg], ring[(seg + 1) % m]);
        let d = (at[seg + 1] - at[seg]).max(1e-6);
        let t = ((s - at[seg]) / d).clamp(0.0, 1.0);
        out.push(Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t));
    }
    out
}

/// Blend (CorelDRAW) / Illustrator's Blend: `steps` shapes turning `a`
/// into `b` (their outer outlines), evenly between them, as smooth closed
/// paths, `a`'s end first.
pub fn blend(a: &PathData, b: &PathData, steps: usize) -> Vec<PathData> {
    const N: usize = 160;
    let outer = |p: &PathData| -> Option<Ring> {
        filled(p)
            .into_iter()
            .max_by(|x, y| area_of(x).abs().total_cmp(&area_of(y).abs()))
    };
    let (Some(ra), Some(rb)) = (outer(a), outer(b)) else {
        return Vec::new();
    };
    // Both counter-clockwise, then b's start turned to fit a's best.
    let ccw = |r: Ring| {
        if area_of(&r) < 0.0 {
            r.into_iter().rev().collect()
        } else {
            r
        }
    };
    let ra = resample(&ccw(ra), N);
    let rb = resample(&ccw(rb), N);
    let (ca, cb) = (centre(&ra), centre(&rb));
    let shift = (0..N)
        .min_by(|&s, &t| {
            let cost = |k: usize| -> f32 {
                (0..N)
                    .step_by(4)
                    .map(|i| {
                        let p = ra[i];
                        let q = rb[(i + k) % N];
                        (p.x - ca.x - (q.x - cb.x)).hypot(p.y - ca.y - (q.y - cb.y))
                    })
                    .sum()
            };
            cost(s).total_cmp(&cost(t))
        })
        .unwrap_or(0);
    (1..=steps)
        .filter_map(|k| {
            let t = k as f32 / (steps + 1) as f32;
            let ring: Ring = (0..N)
                .map(|i| {
                    let (p, q) = (ra[i], rb[(i + shift) % N]);
                    Point::new(p.x + (q.x - p.x) * t, p.y + (q.y - p.y) * t)
                })
                .collect();
            let out = rings_to_path(vec![ring]);
            (!out.contours.is_empty()).then_some(out)
        })
        .collect()
}

fn centre(r: &[Point]) -> Point {
    let n = r.len().max(1) as f32;
    let (x, y) = r.iter().fold((0.0, 0.0), |(x, y), p| (x + p.x, y + p.y));
    Point::new(x / n, y / n)
}

fn area_of(r: &[Point]) -> f32 {
    let n = r.len();
    (0..n)
        .map(|i| {
            let (p, q) = (r[i], r[(i + 1) % n]);
            p.x * q.y - q.x * p.y
        })
        .sum::<f32>()
        * 0.5
}

#[cfg(test)]
mod more_tests {
    use super::*;
    use crate::vector::from_shape;

    fn bounds(p: &PathData) -> (f32, f32, f32, f32) {
        flatten_path(p, 0.1).iter().flatten().fold(
            (f32::MAX, f32::MAX, f32::MIN, f32::MIN),
            |(a, b, c, d), q| (a.min(q.x), b.min(q.y), c.max(q.x), d.max(q.y)),
        )
    }

    #[test]
    fn a_square_swept_down_right_covers_the_whole_trail() {
        let sq = from_shape::rect_path(0.0, 0.0, 50.0, 50.0, 0.0);
        let s = sweep(&sq, 20.0, 10.0).expect("sweep");
        let (x0, y0, x1, y1) = bounds(&s);
        assert!(
            x0.abs() < 0.6 && y0.abs() < 0.6 && (x1 - 70.0).abs() < 0.6 && (y1 - 60.0).abs() < 0.6,
            "{x0} {y0} {x1} {y1}"
        );
        // One piece: 50² + the two bands (50·20 + 50·10).
        let a: f32 = filled(&s).iter().map(|r| area_of(r)).sum::<f32>().abs();
        assert!((a - (2500.0 + 1000.0 + 500.0)).abs() < 40.0, "{a}");
    }

    #[test]
    fn a_circle_blends_into_a_square_step_by_step() {
        let c = from_shape::ellipse_path(0.0, 0.0, 40.0, 40.0);
        let s = from_shape::rect_path(200.0, 0.0, 260.0, 60.0, 0.0);
        let steps = blend(&c, &s, 3);
        assert_eq!(steps.len(), 3);
        let mids: Vec<f32> = steps
            .iter()
            .map(|p| {
                let b = bounds(p);
                (b.0 + b.2) * 0.5
            })
            .collect();
        assert!(mids[0] < mids[1] && mids[1] < mids[2], "{mids:?}");
        let w = |p: &PathData| {
            let b = bounds(p);
            b.2 - b.0
        };
        assert!(w(&steps[0]) > 40.0 && w(&steps[2]) < 60.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::path::{Contour, Node};

    fn square(x: f32, y: f32, s: f32) -> Contour {
        Contour::new(
            vec![
                Node::sharp(Point::new(x, y)),
                Node::sharp(Point::new(x + s, y)),
                Node::sharp(Point::new(x + s, y + s)),
                Node::sharp(Point::new(x, y + s)),
            ],
            true,
        )
    }

    fn bounds(p: &PathData) -> (f32, f32, f32, f32) {
        flatten_path(p, 0.1).iter().flatten().fold(
            (f32::MAX, f32::MAX, f32::MIN, f32::MIN),
            |(a, b, c, d), q| (a.min(q.x), b.min(q.y), c.max(q.x), d.max(q.y)),
        )
    }

    fn filled_area(p: &PathData) -> f32 {
        filled(p).iter().map(|r| area(r)).sum::<f32>().abs()
    }

    #[test]
    fn a_square_grows_and_shrinks_by_the_distance() {
        let sq = PathData::new(vec![square(0.0, 0.0, 100.0)], FillRule::NonZero);
        let out = offset_path(&sq, 10.0, LineJoin::Miter, 4.0).expect("out");
        let (x0, y0, x1, y1) = bounds(&out);
        assert!(
            (x0 + 10.0).abs() < 0.6 && (y0 + 10.0).abs() < 0.6,
            "{x0} {y0}"
        );
        assert!(
            (x1 - 110.0).abs() < 0.6 && (y1 - 110.0).abs() < 0.6,
            "{x1} {y1}"
        );
        let round = offset_path(&sq, 10.0, LineJoin::Round, 4.0).expect("round");
        // Round corners: a little less than the sharp 120².
        let a = filled_area(&round);
        assert!(
            a < 120.0 * 120.0 - 50.0 && a > 100.0 * 100.0 + 4.0 * 1000.0,
            "{a}"
        );
        let inner = offset_path(&sq, -20.0, LineJoin::Miter, 4.0).expect("in");
        let (x0, _, x1, _) = bounds(&inner);
        assert!(
            (x0 - 20.0).abs() < 0.6 && (x1 - 80.0).abs() < 0.6,
            "{x0} {x1}"
        );
        assert!(offset_path(&sq, -60.0, LineJoin::Miter, 4.0).is_none());
    }

    #[test]
    fn a_line_becomes_the_shape_its_stroke_paints() {
        let line = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(100.0, 0.0)),
                ],
                false,
            )],
            FillRule::NonZero,
        );
        let out = outline_to_path(&line, 10.0, LineCap::Butt, LineJoin::Miter, 4.0).expect("o");
        assert!(
            (filled_area(&out) - 1000.0).abs() < 20.0,
            "{}",
            filled_area(&out)
        );
        assert!(out.contours.iter().all(|c| c.closed));
    }

    #[test]
    fn breaking_apart_keeps_each_hole_with_its_piece() {
        // An «o» (ring) and a dot beside it.
        let mut hole = square(20.0, 20.0, 60.0);
        hole.nodes.reverse();
        let path = PathData::new(
            vec![square(0.0, 0.0, 100.0), hole, square(150.0, 40.0, 20.0)],
            FillRule::EvenOdd,
        );
        let parts = break_apart(&path);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].contours.len(), 2, "the o keeps its counter");
        assert!((filled_area(&parts[0]) - (10000.0 - 3600.0)).abs() < 30.0);
        assert_eq!(parts[1].contours.len(), 1);
        let back = combine(&[&parts[0], &parts[1]]);
        assert_eq!(back.contours.len(), 3);
    }
}

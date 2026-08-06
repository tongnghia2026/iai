#![allow(dead_code)]
//! Cut an OPEN contour by a region predicate — the geometry behind trimming an
//! open stroke/line where it crosses a selection, leaving the outside parts as
//! separate open sub-paths (see `App::trim_active_vector_by_selection`).
//!
//! The boolean engine can't do this: it is fill-based and treats an open contour
//! as closed. Here we instead walk each cubic segment, find where the inside/
//! outside predicate flips (fine sampling + bisection), split there with exact
//! De Casteljau subdivision so the surviving curve is geometrically unchanged,
//! and regroup the runs that lie OUTSIDE the region into open [`Contour`]s.
//!
//! Each returned [`OpenPiece`] also carries the normalized arc-length range
//! `[arc0, arc1]` it occupies on the original contour, so a Vector Brush's
//! width profile can be re-sliced per piece (via [`BrushStroke::sliced`]).
//!
//! Pure geometry: the predicate injects the region test, so nothing here touches
//! `Selection`, UI or GPU.

use crate::core::geometry::{cubic_bezier, Point};
use crate::core::vector::path::{Contour, Node, NodeKind};

/// One surviving (outside-the-region) run of an open contour.
pub struct OpenPiece {
    /// The piece as an open contour, in the same space as the input.
    pub contour: Contour,
    /// Normalized arc-length start on the ORIGINAL contour, in `[0,1]`.
    pub arc0: f32,
    /// Normalized arc-length end on the original contour, in `[0,1]`.
    pub arc1: f32,
}

/// Absolute cubic control points `[p0, c1, c2, p3]`. A straight segment has its
/// handles collapsed onto the anchors.
type Cubic = [Point; 4];

/// Cut open `contour` by `inside`: returns the parts lying OUTSIDE the region as
/// separate open pieces, in order along the contour. An empty result means the
/// whole contour was inside (fully removed); a single full-range piece means the
/// region did not touch it.
pub fn cut_open_contour(contour: &Contour, inside: &dyn Fn(Point) -> bool) -> Vec<OpenPiece> {
    let seg_count = contour.segment_count();
    if contour.closed || seg_count == 0 {
        return Vec::new();
    }

    // Split every segment at its inside/outside crossings into sub-cubics, each
    // tagged inside/outside and measured (arc length), concatenated in order.
    let mut subs: Vec<(Cubic, bool, f32)> = Vec::new();
    for s in 0..seg_count {
        let Some((p0, p1, p2, p3)) = contour.segment(s) else {
            continue;
        };
        let cubic = [p0, p1, p2, p3];
        let mut params = vec![0.0f32];
        params.extend(crossings_in_segment(&cubic, inside));
        params.push(1.0);
        for w in params.windows(2) {
            let (t0, t1) = (w[0], w[1]);
            if t1 - t0 < 1e-5 {
                continue;
            }
            let sub = sub_cubic(&cubic, t0, t1);
            let mid = cubic_bezier(p0, p1, p2, p3, 0.5 * (t0 + t1));
            subs.push((sub, inside(mid), arc_len(&sub)));
        }
    }
    if subs.is_empty() {
        return Vec::new();
    }

    // Prefix arc lengths for normalized [arc0,arc1] per piece.
    let total: f32 = subs.iter().map(|(_, _, a)| *a).sum::<f32>().max(1e-6);
    let mut cum = vec![0.0f32; subs.len() + 1];
    for (i, (_, _, a)) in subs.iter().enumerate() {
        cum[i + 1] = cum[i] + a;
    }

    // Group contiguous OUTSIDE runs into pieces.
    let mut pieces: Vec<OpenPiece> = Vec::new();
    let mut i = 0;
    while i < subs.len() {
        if subs[i].1 {
            i += 1;
            continue; // inside → dropped
        }
        let start = i;
        while i < subs.len() && !subs[i].1 {
            i += 1;
        }
        let run: Vec<Cubic> = subs[start..i].iter().map(|(c, _, _)| *c).collect();
        if let Some(contour) = cubics_to_open_contour(&run) {
            pieces.push(OpenPiece {
                contour,
                arc0: (cum[start] / total).clamp(0.0, 1.0),
                arc1: (cum[i] / total).clamp(0.0, 1.0),
            });
        }
    }
    pieces
}

/// Parameters in `(0,1)` where `inside` flips along one cubic, ascending. Coarse
/// sampling (≈1/px) locates each flip, then bisection refines it to ~1e-3.
fn crossings_in_segment(c: &Cubic, inside: &dyn Fn(Point) -> bool) -> Vec<f32> {
    let approx_len = c[0]
        .distance_to(c[3])
        .max(c[0].distance_to(c[1]) + c[1].distance_to(c[2]) + c[2].distance_to(c[3]));
    let samples = (approx_len.ceil() as usize).clamp(24, 400);
    let mut out = Vec::new();
    let at = |t: f32| inside(cubic_bezier(c[0], c[1], c[2], c[3], t));
    let mut prev_t = 0.0f32;
    let mut prev_in = at(0.0);
    for k in 1..=samples {
        let t = k as f32 / samples as f32;
        let cur_in = at(t);
        if cur_in != prev_in {
            out.push(bisect_crossing(c, prev_t, t, prev_in, inside));
        }
        prev_t = t;
        prev_in = cur_in;
    }
    out
}

/// Bisection for the parameter where `inside` changes between `ta` (state
/// `ina`) and `tb`.
fn bisect_crossing(c: &Cubic, ta: f32, tb: f32, ina: bool, inside: &dyn Fn(Point) -> bool) -> f32 {
    let (mut lo, mut hi) = (ta, tb);
    for _ in 0..16 {
        let mid = 0.5 * (lo + hi);
        let mid_in = inside(cubic_bezier(c[0], c[1], c[2], c[3], mid));
        if mid_in == ina {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// De Casteljau split of a cubic at `t`, returning `(left, right)` sub-cubics.
fn split_at(c: &Cubic, t: f32) -> (Cubic, Cubic) {
    let a = c[0].lerp(c[1], t);
    let b = c[1].lerp(c[2], t);
    let cc = c[2].lerp(c[3], t);
    let d = a.lerp(b, t);
    let e = b.lerp(cc, t);
    let m = d.lerp(e, t);
    ([c[0], a, d, m], [m, e, cc, c[3]])
}

/// Exact sub-cubic covering the parameter range `[t0,t1]` of `c`.
fn sub_cubic(c: &Cubic, t0: f32, t1: f32) -> Cubic {
    let (_, right) = split_at(c, t0);
    let tt = if 1.0 - t0 > 1e-6 {
        ((t1 - t0) / (1.0 - t0)).clamp(0.0, 1.0)
    } else {
        1.0
    };
    let (left, _) = split_at(&right, tt);
    left
}

/// Arc length of a cubic by chord summation over a fixed sample count.
fn arc_len(c: &Cubic) -> f32 {
    const K: usize = 16;
    let mut prev = c[0];
    let mut len = 0.0;
    for k in 1..=K {
        let p = cubic_bezier(c[0], c[1], c[2], c[3], k as f32 / K as f32);
        len += prev.distance_to(p);
        prev = p;
    }
    len
}

/// Build one OPEN contour from a chain of cubics whose ends coincide. Handles
/// that sit on their anchor are dropped so a straight run stays a polyline.
fn cubics_to_open_contour(cubics: &[Cubic]) -> Option<Contour> {
    if cubics.is_empty() {
        return None;
    }
    let handle = |h: Point, anchor: Point| (h.distance_to(anchor) > 1e-4).then_some(h);
    let kind = |i: Option<Point>, o: Option<Point>| {
        if i.is_some() && o.is_some() {
            NodeKind::Smooth
        } else {
            NodeKind::Cusp
        }
    };

    let m = cubics.len();
    let mut nodes: Vec<Node> = Vec::with_capacity(m + 1);

    // First node: no incoming handle.
    let first_out = handle(cubics[0][1], cubics[0][0]);
    nodes.push(Node {
        anchor: cubics[0][0],
        in_handle: None,
        out_handle: first_out,
        kind: kind(None, first_out),
    });
    // Interior joins.
    for k in 1..m {
        let anchor = cubics[k][0];
        let in_h = handle(cubics[k - 1][2], anchor);
        let out_h = handle(cubics[k][1], anchor);
        nodes.push(Node {
            anchor,
            in_handle: in_h,
            out_handle: out_h,
            kind: kind(in_h, out_h),
        });
    }
    // Last node: no outgoing handle.
    let last = cubics[m - 1];
    let last_in = handle(last[2], last[3]);
    nodes.push(Node {
        anchor: last[3],
        in_handle: last_in,
        out_handle: None,
        kind: kind(last_in, None),
    });

    for n in &nodes {
        if !n.anchor.x.is_finite() || !n.anchor.y.is_finite() {
            return None;
        }
    }
    Some(Contour::new(nodes, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_line(a: Point, b: Point) -> Contour {
        Contour::new(vec![Node::sharp(a), Node::sharp(b)], false)
    }

    /// Polyline length of a contour by flattening its segments coarsely.
    fn contour_len(c: &Contour) -> f32 {
        let mut len = 0.0;
        for s in 0..c.segment_count() {
            let (p0, p1, p2, p3) = c.segment(s).unwrap();
            len += arc_len(&[p0, p1, p2, p3]);
        }
        len
    }

    #[test]
    fn middle_cut_splits_a_line_into_two() {
        // Horizontal line 0..100 at y=0; region is the band x∈[40,60].
        let line = open_line(Point::new(0.0, 0.0), Point::new(100.0, 0.0));
        let inside = |p: Point| p.x >= 40.0 && p.x <= 60.0;
        let pieces = cut_open_contour(&line, &inside);
        assert_eq!(pieces.len(), 2, "one gap → two pieces");
        // Left piece 0..40, right piece 60..100.
        assert!((pieces[0].contour.nodes[0].anchor.x - 0.0).abs() < 0.5);
        assert!((pieces[0].contour.nodes.last().unwrap().anchor.x - 40.0).abs() < 0.5);
        assert!((pieces[1].contour.nodes[0].anchor.x - 60.0).abs() < 0.5);
        assert!((pieces[1].contour.nodes.last().unwrap().anchor.x - 100.0).abs() < 0.5);
        // Both pieces are open.
        assert!(pieces.iter().all(|p| !p.contour.closed));
    }

    #[test]
    fn region_not_touching_returns_the_whole_line() {
        let line = open_line(Point::new(0.0, 0.0), Point::new(100.0, 0.0));
        let inside = |p: Point| p.y > 50.0; // never true on the line
        let pieces = cut_open_contour(&line, &inside);
        assert_eq!(pieces.len(), 1);
        assert!((pieces[0].arc0 - 0.0).abs() < 1e-3 && (pieces[0].arc1 - 1.0).abs() < 1e-3);
        assert!((contour_len(&pieces[0].contour) - 100.0).abs() < 1.0);
    }

    #[test]
    fn line_fully_inside_is_removed() {
        let line = open_line(Point::new(10.0, 0.0), Point::new(20.0, 0.0));
        let inside = |_p: Point| true;
        assert!(cut_open_contour(&line, &inside).is_empty());
    }

    #[test]
    fn cutting_an_end_leaves_one_piece_with_correct_arc() {
        // Cut off the right third (x>66).
        let line = open_line(Point::new(0.0, 0.0), Point::new(99.0, 0.0));
        let inside = |p: Point| p.x > 66.0;
        let pieces = cut_open_contour(&line, &inside);
        assert_eq!(pieces.len(), 1);
        let p = &pieces[0];
        assert!((p.arc0 - 0.0).abs() < 1e-2);
        assert!((p.arc1 - 66.0 / 99.0).abs() < 0.02, "arc1 {}", p.arc1);
        assert!((p.contour.nodes.last().unwrap().anchor.x - 66.0).abs() < 1.0);
    }

    #[test]
    fn curved_segment_split_keeps_shape() {
        // A single cubic bulging upward; cut the middle. The kept ends must still
        // lie on the original curve.
        let c = Contour::new(
            vec![
                Node::with_handles(
                    Point::new(0.0, 0.0),
                    Point::new(-1.0, 0.0),
                    Point::new(0.0, 30.0),
                    NodeKind::Smooth,
                ),
                Node::with_handles(
                    Point::new(90.0, 0.0),
                    Point::new(90.0, 30.0),
                    Point::new(91.0, 0.0),
                    NodeKind::Smooth,
                ),
            ],
            false,
        );
        let (p0, p1, p2, p3) = c.segment(0).unwrap();
        // Region: points whose y is high (near the top of the arc, mid-parameter).
        let inside = |p: Point| p.y > 20.0;
        let pieces = cut_open_contour(&c, &inside);
        assert_eq!(pieces.len(), 2, "cutting the crest leaves two ends");
        // Each surviving endpoint near the cut lies on the original curve.
        for piece in &pieces {
            for node in &piece.contour.nodes {
                // Find nearest point on the original curve.
                let mut best = f32::INFINITY;
                for k in 0..=200 {
                    let q = cubic_bezier(p0, p1, p2, p3, k as f32 / 200.0);
                    best = best.min(q.distance_to(node.anchor));
                }
                assert!(
                    best < 0.6,
                    "node {:?} drifted {best} off the curve",
                    node.anchor
                );
            }
        }
    }
}

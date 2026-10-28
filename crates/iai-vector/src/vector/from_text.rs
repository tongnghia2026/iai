//! Build editable vector [`Contour`]s from a font glyph's outline — the geometry
//! half of "Convert Text to Curves". Pure and font-free, mirroring how
//! [`crate::core::vector::from_shape`] takes raw coordinates rather than a
//! `ShapeData`: it consumes a flat list of [`GlyphSegment`]s whose points the
//! caller ([`crate::core::text::text_to_curves`]) has ALREADY mapped into the
//! target (canvas) space. All the font/layout/placement math lives in the caller;
//! here we only split the flat curve stream into closed contours and turn each
//! line/quad/cubic segment into [`Node`]s with absolute cubic handles.
//!
//! A glyph outline (from `ab_glyph`/ttf-parser) is a run of sub-contours
//! concatenated into one list: each closed sub-contour's segments chain
//! end-to-start, and a `move_to` starting the next sub-contour breaks the chain.
//! `close()` appends a straight `Line` back to the sub-contour's first point, so
//! the ring is continuous and the final wrap segment is that closing line.

use crate::vector::path::{Contour, Node, NodeKind};
use iai_geom::geometry::Point;

/// One outline segment of a glyph, endpoints and controls already in target
/// space. Quadratics are elevated to cubics when building nodes — exact, because
/// control-point elevation is a fixed affine combination and the caller's mapping
/// is itself affine, so elevating before or after the mapping gives the same
/// result.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GlyphSegment {
    /// Straight line `start → end`.
    Line(Point, Point),
    /// Quadratic Bézier `start → end` with control `.1`.
    Quad(Point, Point, Point),
    /// Cubic Bézier `start → end` with controls `.1` (at start) and `.2` (at end).
    Cubic(Point, Point, Point, Point),
}

impl GlyphSegment {
    fn start(self) -> Point {
        match self {
            GlyphSegment::Line(a, _)
            | GlyphSegment::Quad(a, _, _)
            | GlyphSegment::Cubic(a, _, _, _) => a,
        }
    }

    fn end(self) -> Point {
        match self {
            GlyphSegment::Line(_, b) => b,
            GlyphSegment::Quad(_, _, b) => b,
            GlyphSegment::Cubic(_, _, _, b) => b,
        }
    }

    /// Absolute out-handle at the segment's START anchor (`None` ⇒ straight).
    fn out_handle(self) -> Option<Point> {
        match self {
            GlyphSegment::Line(..) => None,
            GlyphSegment::Quad(a, ctrl, _) => Some(elevate(a, ctrl)),
            GlyphSegment::Cubic(_, c1, _, _) => Some(c1),
        }
    }

    /// Absolute in-handle at the segment's END anchor (`None` ⇒ straight).
    fn in_handle(self) -> Option<Point> {
        match self {
            GlyphSegment::Line(..) => None,
            GlyphSegment::Quad(_, ctrl, b) => Some(elevate(b, ctrl)),
            GlyphSegment::Cubic(_, _, c2, _) => Some(c2),
        }
    }

    /// Every point through `f` (exact for an affine `f`).
    pub fn map(self, f: impl Fn(Point) -> Point) -> Self {
        match self {
            GlyphSegment::Line(a, b) => GlyphSegment::Line(f(a), f(b)),
            GlyphSegment::Quad(a, c, b) => GlyphSegment::Quad(f(a), f(c), f(b)),
            GlyphSegment::Cubic(a, c1, c2, b) => GlyphSegment::Cubic(f(a), f(c1), f(c2), f(b)),
        }
    }

    /// The control polygon's points, start to end.
    fn points(self) -> impl Iterator<Item = Point> {
        let (n, pts) = match self {
            GlyphSegment::Line(a, b) => (2, [a, b, b, b]),
            GlyphSegment::Quad(a, c, b) => (3, [a, c, b, b]),
            GlyphSegment::Cubic(a, c1, c2, b) => (4, [a, c1, c2, b]),
        };
        pts.into_iter().take(n)
    }

    /// The segment cut where it turns in y, so each piece only rises or only
    /// falls.
    fn split_at_y_turns(self) -> Vec<GlyphSegment> {
        match self {
            GlyphSegment::Line(..) => vec![self],
            GlyphSegment::Quad(a, c, b) => {
                let denom = a.y - 2.0 * c.y + b.y;
                let t = if denom.abs() > 1e-9 {
                    (a.y - c.y) / denom
                } else {
                    -1.0
                };
                if t > TURN_EPS && t < 1.0 - TURN_EPS {
                    let (m1, m2) = (lerp(a, c, t), lerp(c, b, t));
                    let p = lerp(m1, m2, t);
                    vec![GlyphSegment::Quad(a, m1, p), GlyphSegment::Quad(p, m2, b)]
                } else {
                    vec![self]
                }
            }
            GlyphSegment::Cubic(a, c1, c2, b) => {
                // y'(t) / 3 = qa·t² + qb·t + qc.
                let (d0, d1, d2) = (c1.y - a.y, c2.y - c1.y, b.y - c2.y);
                let (qa, qb, qc) = (d0 - 2.0 * d1 + d2, 2.0 * (d1 - d0), d0);
                let mut ts: Vec<f32> = if qa.abs() > 1e-9 {
                    let disc = qb * qb - 4.0 * qa * qc;
                    if disc < 0.0 {
                        Vec::new()
                    } else {
                        let r = disc.sqrt();
                        vec![(-qb - r) / (2.0 * qa), (-qb + r) / (2.0 * qa)]
                    }
                } else if qb.abs() > 1e-9 {
                    vec![-qc / qb]
                } else {
                    Vec::new()
                };
                ts.retain(|t| *t > TURN_EPS && *t < 1.0 - TURN_EPS);
                ts.sort_by(f32::total_cmp);
                ts.dedup_by(|x, y| (*x - *y).abs() < TURN_EPS);
                let mut out = Vec::with_capacity(ts.len() + 1);
                let (mut rest, mut done) = ([a, c1, c2, b], 0.0);
                for t in ts {
                    let local = (t - done) / (1.0 - done);
                    let [p0, p1, p2, p3] = rest;
                    let (q1, q2, q3) = (
                        lerp(p0, p1, local),
                        lerp(p1, p2, local),
                        lerp(p2, p3, local),
                    );
                    let (r1, r2) = (lerp(q1, q2, local), lerp(q2, q3, local));
                    let mid = lerp(r1, r2, local);
                    out.push(GlyphSegment::Cubic(p0, q1, r1, mid));
                    rest = [mid, r2, q3, p3];
                    done = t;
                }
                let [p0, p1, p2, p3] = rest;
                out.push(GlyphSegment::Cubic(p0, p1, p2, p3));
                out
            }
        }
    }

    fn shifted(self, dx: f32) -> Self {
        self.map(|p| Point::new(p.x + dx, p.y))
    }
}

/// How close to a segment's ends a y turn may sit and still cut it.
const TURN_EPS: f32 = 1e-4;

fn lerp(a: Point, b: Point, t: f32) -> Point {
    Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

/// Twice the signed area under a run of segments (control polygons: enough to
/// tell which way a glyph's rings wind).
fn ring_area2(ring: &[GlyphSegment]) -> f32 {
    let mut pts = ring.iter().flat_map(|s| s.points());
    let Some(first) = pts.next() else {
        return 0.0;
    };
    let (mut area, mut prev) = (0.0, first);
    for p in pts.chain(std::iter::once(first)) {
        area += prev.x * p.y - p.x * prev.y;
        prev = p;
    }
    area
}

/// The outline of a glyph dragged `dx` along +x — the area a faux bold paints
/// by striking the glyph at 0 and at `dx` — as one outline. The two overlapping
/// copies a double strike would give cross in small steps at stroke ends and
/// kinks at the tops and bottoms of curves, which a PDF shows when zoomed in.
/// Each ring is cut where it turns in y; runs whose outside faces +x move by
/// `dx`, and flat joins bridge them to the runs that stay. Rings are split as
/// [`glyph_contours`] splits them.
pub fn glyph_swept_right(segments: &[GlyphSegment], dx: f32) -> Vec<GlyphSegment> {
    if !(dx.is_finite() && dx > 0.0) {
        return segments.to_vec();
    }
    let mut rings: Vec<&[GlyphSegment]> = Vec::new();
    let mut start = 0;
    for i in 1..=segments.len() {
        if i == segments.len() || segments[i].start() != segments[i - 1].end() {
            rings.push(&segments[start..i]);
            start = i;
        }
    }
    // The ink lies on the same side of travel on every ring of a glyph (holes
    // wind the other way), so the glyph's total signed area says which side.
    let area: f32 = rings.iter().map(|r| ring_area2(r)).sum();
    if area.abs() < 1e-6 {
        return segments.to_vec();
    }
    let mut out = Vec::with_capacity(segments.len() * 2);
    for ring in rings {
        let pieces: Vec<GlyphSegment> = ring.iter().flat_map(|s| s.split_at_y_turns()).collect();
        // Whether a piece moves; a flat piece goes with the one before it.
        let moves: Vec<Option<bool>> = pieces
            .iter()
            .map(|p| {
                let dy = p.end().y - p.start().y;
                (dy.abs() > MERGE_EPS).then_some((dy > 0.0) == (area > 0.0))
            })
            .collect();
        let Some(mut side) = moves.iter().flatten().next_back().copied() else {
            out.extend_from_slice(ring);
            continue;
        };
        let sides: Vec<bool> = moves
            .iter()
            .map(|m| {
                side = m.unwrap_or(side);
                side
            })
            .collect();
        let shift = |p: Point, moved: bool| if moved { Point::new(p.x + dx, p.y) } else { p };
        for (i, piece) in pieces.iter().enumerate() {
            let prev = if i == 0 { pieces.len() - 1 } else { i - 1 };
            if sides[prev] != sides[i] {
                out.push(GlyphSegment::Line(
                    shift(pieces[prev].end(), sides[prev]),
                    shift(piece.start(), sides[i]),
                ));
            }
            out.push(if sides[i] { piece.shifted(dx) } else { *piece });
        }
    }
    out
}

/// Cubic control point equivalent to a quadratic control `ctrl` at anchor `a`:
/// `a + 2/3·(ctrl − a)`. Elevating a quadratic to a cubic this way is exact.
fn elevate(a: Point, ctrl: Point) -> Point {
    Point::new(
        a.x + 2.0 / 3.0 * (ctrl.x - a.x),
        a.y + 2.0 / 3.0 * (ctrl.y - a.y),
    )
}

/// Tolerance (target-space units, i.e. canvas pixels) for treating two points as
/// the same — only used to drop a zero-length closing line.
const MERGE_EPS: f32 = 1e-4;

fn coincident(a: Point, b: Point) -> bool {
    (a.x - b.x).abs() <= MERGE_EPS && (a.y - b.y).abs() <= MERGE_EPS
}

/// Convert a glyph's flat outline (one or more closed sub-contours, concatenated)
/// into vector [`Contour`]s. Sub-contours split where a segment's start does not
/// exactly continue the previous segment's end (a `move_to` in the source). Holes
/// stay separate contours, filled by whatever [`crate::core::vector::path::FillRule`]
/// the caller puts on the `PathData` (Text uses NonZero, preserving each glyph's
/// native contour winding so counters render as holes).
pub fn glyph_contours(segments: &[GlyphSegment]) -> Vec<Contour> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < segments.len() {
        // One continuous run == one sub-contour. Endpoints shared between
        // consecutive segments are the same source point copied through the
        // caller's deterministic map, so exact equality is the right split test.
        let start = i;
        i += 1;
        while i < segments.len() && segments[i].start() == segments[i - 1].end() {
            i += 1;
        }
        if let Some(c) = build_contour(&segments[start..i]) {
            out.push(c);
        }
    }
    out
}

/// Turn one continuous sub-contour of segments into a closed [`Contour`]. Node
/// `j` sits at `seg[j].start()`; its out-handle comes from `seg[j]` and its
/// in-handle from the previous segment (wrapping to the last, which for a closed
/// ring is the straight closing line — hence a straight wrap segment).
fn build_contour(seg: &[GlyphSegment]) -> Option<Contour> {
    let k = seg.len();
    if k == 0 {
        return None;
    }
    let mut nodes: Vec<Node> = Vec::with_capacity(k);
    for j in 0..k {
        let prev = if j == 0 { k - 1 } else { j - 1 };
        nodes.push(Node {
            anchor: seg[j].start(),
            in_handle: seg[prev].in_handle(),
            out_handle: seg[j].out_handle(),
            kind: NodeKind::Cusp,
        });
    }
    // A degenerate zero-length closing line leaves the last node coincident with
    // the first; fold its incoming handle onto the first node and drop it.
    if nodes.len() >= 2 {
        let last = nodes.len() - 1;
        if coincident(nodes[last].anchor, nodes[0].anchor) {
            nodes[0].in_handle = nodes[last].in_handle;
            nodes.pop();
        }
    }
    if nodes.len() < 2 {
        return None;
    }
    Some(Contour::new(nodes, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f32, y: f32) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn straight_triangle_becomes_three_sharp_nodes() {
        // move (0,0) → (10,0) → (5,10) → close.
        let segs = [
            GlyphSegment::Line(p(0.0, 0.0), p(10.0, 0.0)),
            GlyphSegment::Line(p(10.0, 0.0), p(5.0, 10.0)),
            GlyphSegment::Line(p(5.0, 10.0), p(0.0, 0.0)), // closing line back to start
        ];
        let cs = glyph_contours(&segs);
        assert_eq!(cs.len(), 1);
        let c = &cs[0];
        assert!(c.closed);
        // Closing line ends exactly at node 0 → dedup drops it, leaving 3 nodes.
        assert_eq!(c.nodes.len(), 3);
        assert_eq!(c.nodes[0].anchor, p(0.0, 0.0));
        assert_eq!(c.nodes[1].anchor, p(10.0, 0.0));
        assert_eq!(c.nodes[2].anchor, p(5.0, 10.0));
        assert!(
            c.nodes
                .iter()
                .all(|n| n.in_handle.is_none() && n.out_handle.is_none())
        );
    }

    #[test]
    fn nondegenerate_closing_line_keeps_all_nodes() {
        // Last drawn point (5,10) is NOT the start, so close() adds a real
        // straight segment (5,10)→(0,0); that becomes the wrap segment and both
        // its ends stay distinct nodes.
        let segs = [
            GlyphSegment::Line(p(0.0, 0.0), p(10.0, 0.0)),
            GlyphSegment::Line(p(10.0, 0.0), p(5.0, 10.0)),
            // no explicit closing line in the list: the ring closes via wrap.
        ];
        let cs = glyph_contours(&segs);
        assert_eq!(cs.len(), 1);
        let c = &cs[0];
        assert_eq!(c.nodes.len(), 2);
        // Wrap segment node[1] → node[0] is straight (both handles absent there).
        assert!(c.nodes[0].in_handle.is_none() && c.nodes[1].out_handle.is_none());
    }

    #[test]
    fn quadratic_is_elevated_to_a_cubic_handle() {
        // One quad from (0,0) to (12,0), control (6,9). close.
        let segs = [
            GlyphSegment::Quad(p(0.0, 0.0), p(6.0, 9.0), p(12.0, 0.0)),
            GlyphSegment::Line(p(12.0, 0.0), p(0.0, 0.0)),
        ];
        let cs = glyph_contours(&segs);
        let c = &cs[0];
        assert_eq!(c.nodes.len(), 2);
        // out-handle of node 0 = (0,0) + 2/3·((6,9)-(0,0)) = (4,6).
        assert_eq!(c.nodes[0].out_handle, Some(p(4.0, 6.0)));
        // in-handle of node 1 = (12,0) + 2/3·((6,9)-(12,0)) = (8,6).
        assert_eq!(c.nodes[1].in_handle, Some(p(8.0, 6.0)));
    }

    #[test]
    fn cubic_handles_pass_through() {
        let segs = [
            GlyphSegment::Cubic(p(0.0, 0.0), p(1.0, 5.0), p(9.0, 5.0), p(10.0, 0.0)),
            GlyphSegment::Line(p(10.0, 0.0), p(0.0, 0.0)),
        ];
        let c = &glyph_contours(&segs)[0];
        assert_eq!(c.nodes[0].out_handle, Some(p(1.0, 5.0)));
        assert_eq!(c.nodes[1].in_handle, Some(p(9.0, 5.0)));
    }

    #[test]
    fn two_subcontours_split_at_the_move() {
        // Outer square + inner (hole) square. The inner run starts at a point that
        // does NOT continue the outer's end → new contour.
        let segs = [
            // outer
            GlyphSegment::Line(p(0.0, 0.0), p(10.0, 0.0)),
            GlyphSegment::Line(p(10.0, 0.0), p(10.0, 10.0)),
            GlyphSegment::Line(p(10.0, 10.0), p(0.0, 10.0)),
            GlyphSegment::Line(p(0.0, 10.0), p(0.0, 0.0)),
            // inner (hole)
            GlyphSegment::Line(p(3.0, 3.0), p(7.0, 3.0)),
            GlyphSegment::Line(p(7.0, 3.0), p(7.0, 7.0)),
            GlyphSegment::Line(p(7.0, 7.0), p(3.0, 7.0)),
            GlyphSegment::Line(p(3.0, 7.0), p(3.0, 3.0)),
        ];
        let cs = glyph_contours(&segs);
        assert_eq!(cs.len(), 2, "outer + hole");
        assert_eq!(cs[0].nodes.len(), 4);
        assert_eq!(cs[1].nodes.len(), 4);
        assert!(cs[0].closed && cs[1].closed);
    }

    #[test]
    fn zero_length_closing_line_folds_incoming_handle() {
        // Two cubics forming a petal that returns to the start, then a zero-length
        // close(): the closing line must be dropped and node 0 must inherit the
        // last cubic's incoming handle so the ring stays one smooth 2-node loop.
        let segs = [
            GlyphSegment::Cubic(p(0.0, 0.0), p(3.0, 5.0), p(7.0, 5.0), p(10.0, 0.0)),
            GlyphSegment::Cubic(p(10.0, 0.0), p(7.0, -5.0), p(3.0, -5.0), p(0.0, 0.0)),
            GlyphSegment::Line(p(0.0, 0.0), p(0.0, 0.0)), // zero-length close
        ];
        let cs = glyph_contours(&segs);
        assert_eq!(cs.len(), 1);
        let c = &cs[0];
        assert_eq!(
            c.nodes.len(),
            2,
            "closing line dropped, petal keeps 2 anchors"
        );
        assert_eq!(c.nodes[0].anchor, p(0.0, 0.0));
        assert_eq!(c.nodes[1].anchor, p(10.0, 0.0));
        // node 0 folds in the second cubic's end-handle; keeps its own out-handle.
        assert_eq!(c.nodes[0].in_handle, Some(p(3.0, -5.0)));
        assert_eq!(c.nodes[0].out_handle, Some(p(3.0, 5.0)));
    }

    #[test]
    fn single_anchor_loop_is_dropped() {
        // A lone cubic that starts and ends on the same point degenerates to one
        // node after dedup; a <2-node contour draws nothing, so it is dropped.
        let segs = [
            GlyphSegment::Cubic(p(0.0, 0.0), p(2.0, 8.0), p(8.0, 8.0), p(0.0, 0.0)),
            GlyphSegment::Line(p(0.0, 0.0), p(0.0, 0.0)),
        ];
        assert!(glyph_contours(&segs).is_empty());
    }

    #[test]
    fn empty_input_yields_no_contours() {
        assert!(glyph_contours(&[]).is_empty());
    }

    fn eval(s: GlyphSegment, t: f32) -> Point {
        match s {
            GlyphSegment::Line(a, b) => lerp(a, b, t),
            GlyphSegment::Quad(a, c, b) => lerp(lerp(a, c, t), lerp(c, b, t), t),
            GlyphSegment::Cubic(a, c1, c2, b) => {
                let (q1, q2, q3) = (lerp(a, c1, t), lerp(c1, c2, t), lerp(c2, b, t));
                lerp(lerp(q1, q2, t), lerp(q2, q3, t), t)
            }
        }
    }

    /// The filled area, curves flattened finely, rings split as
    /// `glyph_contours` splits them, holes counted against their outers.
    fn area_of(segs: &[GlyphSegment]) -> f32 {
        let mut total = 0.0;
        let mut ring: Vec<Point> = Vec::new();
        let close = |ring: &mut Vec<Point>| {
            let n = ring.len();
            let a: f32 = (0..n)
                .map(|i| {
                    let (p, q) = (ring[i], ring[(i + 1) % n]);
                    p.x * q.y - q.x * p.y
                })
                .sum();
            ring.clear();
            a / 2.0
        };
        for (i, s) in segs.iter().enumerate() {
            if i > 0 && s.start() != segs[i - 1].end() {
                total += close(&mut ring);
            }
            for k in 0..64 {
                ring.push(eval(*s, k as f32 / 64.0));
            }
        }
        (total + close(&mut ring)).abs()
    }

    fn lines(points: &[(f32, f32)]) -> Vec<GlyphSegment> {
        (0..points.len())
            .map(|i| {
                let (a, b) = (points[i], points[(i + 1) % points.len()]);
                GlyphSegment::Line(p(a.0, a.1), p(b.0, b.1))
            })
            .collect()
    }

    #[test]
    fn a_swept_square_is_one_sweep_wider() {
        let square = lines(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]);
        let swept = glyph_swept_right(&square, 1.0);
        assert!((area_of(&swept) - 110.0).abs() < 1e-3);
        assert_eq!(glyph_contours(&swept).len(), 1);
        // Wound the other way (a CFF font), the same.
        let mut back = lines(&[(0.0, 0.0), (0.0, 10.0), (10.0, 10.0), (10.0, 0.0)]);
        back = glyph_swept_right(&back, 1.0);
        assert!((area_of(&back) - 110.0).abs() < 1e-3);
    }

    #[test]
    fn a_swept_ring_thickens_round_its_hole() {
        let mut ring = lines(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]);
        ring.extend(lines(&[(3.0, 3.0), (3.0, 7.0), (7.0, 7.0), (7.0, 3.0)]));
        let swept = glyph_swept_right(&ring, 1.0);
        // 11×10 outside, a 3×4 hole.
        assert!((area_of(&swept) - 98.0).abs() < 1e-3);
        assert_eq!(glyph_contours(&swept).len(), 2);
    }

    #[test]
    fn a_swept_point_gets_a_flat_end() {
        // A triangle pointing down: its tip becomes a 2-wide flat.
        let tri = lines(&[(0.0, 0.0), (10.0, 0.0), (5.0, 10.0)]);
        let swept = glyph_swept_right(&tri, 2.0);
        assert!((area_of(&swept) - 70.0).abs() < 1e-3);
    }

    #[test]
    fn a_swept_circle_is_one_smooth_ring() {
        // Four cubics, radius 10: the sweep adds a 20×1 band and keeps the
        // round tops round (flat joins where the tangent is flat anyway).
        let k = 10.0 * 0.552_284_8;
        let circle = [
            GlyphSegment::Cubic(p(10.0, 0.0), p(10.0, k), p(k, 10.0), p(0.0, 10.0)),
            GlyphSegment::Cubic(p(0.0, 10.0), p(-k, 10.0), p(-10.0, k), p(-10.0, 0.0)),
            GlyphSegment::Cubic(p(-10.0, 0.0), p(-10.0, -k), p(-k, -10.0), p(0.0, -10.0)),
            GlyphSegment::Cubic(p(0.0, -10.0), p(k, -10.0), p(10.0, -k), p(10.0, 0.0)),
        ];
        let swept = glyph_swept_right(&circle, 1.0);
        assert!((area_of(&swept) - area_of(&circle) - 20.0).abs() < 0.05);
        assert_eq!(glyph_contours(&swept).len(), 1);
        // Every bridge is flat and one sweep long.
        let bridges: Vec<_> = swept
            .iter()
            .filter_map(|s| match *s {
                GlyphSegment::Line(a, b) => Some((a, b)),
                _ => None,
            })
            .collect();
        assert_eq!(bridges.len(), 2);
        assert!(
            bridges
                .iter()
                .all(|(a, b)| a.y == b.y && ((b.x - a.x).abs() - 1.0).abs() < 1e-5)
        );
    }

    #[test]
    fn a_swept_curve_turning_mid_segment_is_cut_there() {
        // One cubic arch over a flat base: the arch turns at its top, inside
        // the segment, and the sweep still adds exactly height × sweep.
        let arch = [
            GlyphSegment::Cubic(p(0.0, 0.0), p(0.0, -8.0), p(10.0, -8.0), p(10.0, 0.0)),
            GlyphSegment::Line(p(10.0, 0.0), p(0.0, 0.0)),
        ];
        let swept = glyph_swept_right(&arch, 1.0);
        assert!((area_of(&swept) - area_of(&arch) - 6.0).abs() < 0.02);
    }
}

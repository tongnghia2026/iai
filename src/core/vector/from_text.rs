#![allow(dead_code)]
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

use crate::core::geometry::Point;
use crate::core::vector::path::{Contour, Node, NodeKind};

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
        assert!(c
            .nodes
            .iter()
            .all(|n| n.in_handle.is_none() && n.out_handle.is_none()));
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
}

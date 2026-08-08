#![allow(dead_code)]
//! Node/segment editing operations on [`PathData`] (foundation task T1.3).
//!
//! These are the pure-geometry primitives the Shape/Node tool (Phase 3) will
//! drive through commands; nothing here touches UI, selection state or history.
//! Contour-level `reverse` and `set_closed` already live on [`Contour`]
//! (path.rs); this module adds the operations that change node *count* or
//! *topology*: split, delete, join, break, line<->curve conversion and align.
//!
//! Every operation preserves the drawn shape unless its whole purpose is to
//! change it (delete, convert). Splitting in particular uses De Casteljau
//! subdivision so the curve is geometrically identical before and after.

use crate::core::geometry::Point;
use crate::core::vector::path::{Contour, Node, NodeKind, PathData};

/// Result alias for the fallible editing ops. The `Err` string is stable enough
/// to surface to the user or a test.
type OpResult = Result<(), String>;

// ---------------------------------------------------------------------------
// Split / insert
// ---------------------------------------------------------------------------

/// De Casteljau subdivision of one cubic at parameter `t`, returning the five
/// new control points that are not already anchors:
/// `(left_out, mid_in, mid_anchor, mid_out, right_in)`.
fn split_cubic(
    p0: Point,
    p1: Point,
    p2: Point,
    p3: Point,
    t: f32,
) -> (Point, Point, Point, Point, Point) {
    let a = p0.lerp(p1, t);
    let b = p1.lerp(p2, t);
    let c = p2.lerp(p3, t);
    let d = a.lerp(b, t);
    let e = b.lerp(c, t);
    let m = d.lerp(e, t);
    (a, d, m, e, c)
}

/// Insert a node by splitting segment `seg` of `contour` at parameter
/// `t ∈ (0,1)`. Returns the index of the freshly inserted node.
///
/// A straight segment (both facing handles absent) splits linearly and stays a
/// polyline; a curved segment splits with De Casteljau so the outline is
/// unchanged and the new node comes out [`NodeKind::Smooth`] (its handles are
/// collinear through the anchor by construction).
pub fn split_segment(contour: &mut Contour, seg: usize, t: f32) -> Result<usize, String> {
    if seg >= contour.segment_count() {
        return Err(format!(
            "segment {seg} out of range (have {})",
            contour.segment_count()
        ));
    }
    if !t.is_finite() || t <= 0.0 || t >= 1.0 {
        return Err(format!("split parameter {t} must be in (0,1)"));
    }
    let n = contour.nodes.len();
    let i = seg;
    let j = (seg + 1) % n;
    let (p0, p1, p2, p3) = contour.segment(seg).expect("segment in range");
    let straight = contour.nodes[i].out_handle.is_none() && contour.nodes[j].in_handle.is_none();

    let new_node = if straight {
        Node::sharp(p0.lerp(p3, t))
    } else {
        let (a, d, m, e, c) = split_cubic(p0, p1, p2, p3, t);
        contour.nodes[i].out_handle = Some(a);
        contour.nodes[j].in_handle = Some(c);
        Node::with_handles(m, d, e, NodeKind::Smooth)
    };

    let at = i + 1; // for the wrap segment (j == 0) this pushes at the end
    contour.nodes.insert(at, new_node);
    Ok(at)
}

/// Remove node `index`. The two neighbours then connect directly using their
/// existing handles; the outline may change (this is a destructive edit, unlike
/// [`split_segment`]).
pub fn delete_node(contour: &mut Contour, index: usize) -> OpResult {
    if index >= contour.nodes.len() {
        return Err(format!(
            "node {index} out of range (have {})",
            contour.nodes.len()
        ));
    }
    contour.nodes.remove(index);
    Ok(())
}

// ---------------------------------------------------------------------------
// Line <-> curve conversion
// ---------------------------------------------------------------------------

/// Turn segment `seg` into a straight line by dropping the two facing handles.
pub fn set_segment_straight(contour: &mut Contour, seg: usize) -> OpResult {
    if seg >= contour.segment_count() {
        return Err(format!("segment {seg} out of range"));
    }
    let n = contour.nodes.len();
    contour.nodes[seg].out_handle = None;
    contour.nodes[(seg + 1) % n].in_handle = None;
    Ok(())
}

/// Turn segment `seg` into a curve by planting default handles at the 1/3 and
/// 2/3 points of the chord (a straight-looking cubic the user can then bend).
pub fn set_segment_curved(contour: &mut Contour, seg: usize) -> OpResult {
    if seg >= contour.segment_count() {
        return Err(format!("segment {seg} out of range"));
    }
    let n = contour.nodes.len();
    let i = seg;
    let j = (seg + 1) % n;
    let (p0, _, _, p3) = contour.segment(seg).expect("segment in range");
    contour.nodes[i].out_handle = Some(p0.lerp(p3, 1.0 / 3.0));
    contour.nodes[j].in_handle = Some(p0.lerp(p3, 2.0 / 3.0));
    Ok(())
}

// ---------------------------------------------------------------------------
// Control handles / node kind
// ---------------------------------------------------------------------------

/// Which of a node's two control handles an edit refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleSide {
    /// The handle for the segment arriving at the node ([`Node::in_handle`]).
    In,
    /// The handle for the segment leaving the node ([`Node::out_handle`]).
    Out,
}

/// Move one control handle of `node` to `new_pos`, keeping the node's kind
/// contract (Mục 2.2 / Giai đoạn 3):
///
/// * [`NodeKind::Cusp`] — the handle moves alone.
/// * [`NodeKind::Smooth`] — the opposite handle stays collinear through the
///   anchor, its own length preserved.
/// * [`NodeKind::Symmetric`] — the opposite handle mirrors to the same length.
///
/// The opposite handle is only touched when it already exists (an open endpoint
/// with a single handle just moves that one). A zero-length drag onto the anchor
/// leaves the opposite handle alone (no meaningful direction to mirror).
pub fn apply_handle_move(node: &mut Node, side: HandleSide, new_pos: Point) {
    match side {
        HandleSide::In => node.in_handle = Some(new_pos),
        HandleSide::Out => node.out_handle = Some(new_pos),
    }
    if node.kind == NodeKind::Cusp {
        return;
    }
    let a = node.anchor;
    let (dx, dy) = (new_pos.x - a.x, new_pos.y - a.y);
    let len = (dx * dx + dy * dy).sqrt();
    if len <= f32::EPSILON {
        return;
    }
    let (ux, uy) = (dx / len, dy / len);
    let kind = node.kind;
    let opp = match side {
        HandleSide::In => &mut node.out_handle,
        HandleSide::Out => &mut node.in_handle,
    };
    if let Some(op) = opp {
        let opp_len = if kind == NodeKind::Symmetric {
            len
        } else {
            ((op.x - a.x).powi(2) + (op.y - a.y).powi(2)).sqrt()
        };
        // Opposite handle points the other way along the shared tangent.
        *op = Point::new(a.x - ux * opp_len, a.y - uy * opp_len);
    }
}

/// Anchors of the nodes adjacent to `ni`, honoring open vs closed contours.
fn neighbor_anchors(c: &Contour, ni: usize) -> (Option<Point>, Option<Point>) {
    let n = c.nodes.len();
    if ni >= n {
        return (None, None);
    }
    let prev = if ni > 0 {
        Some(c.nodes[ni - 1].anchor)
    } else if c.closed && n >= 2 {
        Some(c.nodes[n - 1].anchor)
    } else {
        None
    };
    let next = if ni + 1 < n {
        Some(c.nodes[ni + 1].anchor)
    } else if c.closed && n >= 2 {
        Some(c.nodes[0].anchor)
    } else {
        None
    };
    (prev, next)
}

/// Unit tangent for a smooth node: along the chord `prev → next` when both
/// neighbours exist, else along whichever neighbour is present.
fn smooth_tangent(anchor: Point, prev: Option<Point>, next: Option<Point>) -> (f32, f32) {
    let (dx, dy) = match (prev, next) {
        (Some(p), Some(q)) => (q.x - p.x, q.y - p.y),
        (Some(p), None) => (anchor.x - p.x, anchor.y - p.y),
        (None, Some(q)) => (q.x - anchor.x, q.y - anchor.y),
        (None, None) => (1.0, 0.0),
    };
    let len = (dx * dx + dy * dy).sqrt();
    if len <= f32::EPSILON {
        (1.0, 0.0)
    } else {
        (dx / len, dy / len)
    }
}

/// Give node `ni` collinear handles through a smooth tangent, synthesizing a
/// handle where one is missing and otherwise preserving each existing handle's
/// length; sets [`NodeKind::Smooth`]. A handle is only placed on a side that has
/// an adjacent segment, so open endpoints stay clean. Converting a Cusp corner
/// this way is how a straight primitive becomes a curve the user can bend.
pub fn set_node_smooth(contour: &mut Contour, ni: usize) -> OpResult {
    make_node_collinear(contour, ni, false)
}

/// Like [`set_node_smooth`] but forces both handles to equal length
/// ([`NodeKind::Symmetric`] — a true mirror).
pub fn set_node_symmetric(contour: &mut Contour, ni: usize) -> OpResult {
    make_node_collinear(contour, ni, true)
}

/// Make node `ni` a corner ([`NodeKind::Cusp`]): its handles keep their current
/// positions but become independent. Handles are left as-is.
pub fn set_node_cusp(contour: &mut Contour, ni: usize) -> OpResult {
    let n = contour.nodes.len();
    let node = contour
        .nodes
        .get_mut(ni)
        .ok_or_else(|| format!("node {ni} out of range (have {n})"))?;
    node.kind = NodeKind::Cusp;
    Ok(())
}

fn make_node_collinear(contour: &mut Contour, ni: usize, symmetric: bool) -> OpResult {
    let n = contour.nodes.len();
    if ni >= n {
        return Err(format!("node {ni} out of range (have {n})"));
    }
    let (prev, next) = neighbor_anchors(contour, ni);
    let has_in = ni > 0 || contour.closed; // a segment arrives here
    let has_out = ni + 1 < n || contour.closed; // a segment leaves here
    let node = &mut contour.nodes[ni];
    let a = node.anchor;
    let (ux, uy) = smooth_tangent(a, prev, next);
    // Keep synthesised handles within the shorter adjacent edge. Giving each
    // side one third of its own edge lets the long-side handle cross the short
    // neighbouring segment at uneven corners, creating tiny self-intersections
    // that show up as white slits in a filled path after smoothing.
    let prev_len = prev.map(|p| a.distance_to(p));
    let next_len = next.map(|q| a.distance_to(q));
    let shared_len = match (prev_len, next_len) {
        (Some(p), Some(q)) => p.min(q) / 3.0,
        (Some(p), None) => p / 3.0,
        (None, Some(q)) => q / 3.0,
        (None, None) => 0.0,
    };
    let def_in = prev_len.map_or(0.0, |_| shared_len);
    let def_out = next_len.map_or(0.0, |_| shared_len);
    let cur_in = node.in_handle.map(|h| a.distance_to(h));
    let cur_out = node.out_handle.map(|h| a.distance_to(h));
    let (len_in, len_out) = if symmetric {
        let l = match (cur_in, cur_out) {
            (Some(i), Some(o)) => (i + o) / 2.0,
            (Some(i), None) => i,
            (None, Some(o)) => o,
            (None, None) => (def_in + def_out) / 2.0,
        };
        let l = if l <= f32::EPSILON {
            def_in.max(def_out).max(1.0)
        } else {
            l
        };
        (l, l)
    } else {
        (cur_in.unwrap_or(def_in), cur_out.unwrap_or(def_out))
    };
    node.in_handle = has_in.then(|| {
        let l = if len_in <= f32::EPSILON {
            def_in.max(1.0)
        } else {
            len_in
        };
        Point::new(a.x - ux * l, a.y - uy * l)
    });
    node.out_handle = has_out.then(|| {
        let l = if len_out <= f32::EPSILON {
            def_out.max(1.0)
        } else {
            len_out
        };
        Point::new(a.x + ux * l, a.y + uy * l)
    });
    node.kind = if symmetric {
        NodeKind::Symmetric
    } else {
        NodeKind::Smooth
    };
    Ok(())
}

// ---------------------------------------------------------------------------
// Break / join
// ---------------------------------------------------------------------------

/// Break the path at node `ni` of contour `ci`.
///
/// * A **closed** contour reopens: the ring is re-rooted at `ni` so the gap sits
///   there, leaving one open contour with a duplicated node at both ends.
/// * An **open** contour splits into two open contours sharing a coincident
///   node; breaking at an existing endpoint is rejected (nothing to break).
pub fn break_at_node(path: &mut PathData, ci: usize, ni: usize) -> OpResult {
    let n = path
        .contours
        .get(ci)
        .ok_or_else(|| format!("contour {ci} out of range"))?
        .nodes
        .len();
    if ni >= n {
        return Err(format!("node {ni} out of range (have {n})"));
    }

    if path.contours[ci].closed {
        let c = &mut path.contours[ci];
        c.nodes.rotate_left(ni);
        let first = c.nodes[0];
        c.nodes.push(first);
        c.closed = false;
        let last = c.nodes.len() - 1;
        // The two coincident endpoints only keep the handle facing into the path.
        c.nodes[0].in_handle = None;
        c.nodes[last].out_handle = None;
        return Ok(());
    }

    if ni == 0 || ni == n - 1 {
        return Err("cannot break an open contour at an endpoint".into());
    }
    let tail_nodes = {
        let c = &mut path.contours[ci];
        let tail = c.nodes.split_off(ni); // c.nodes is now [0..ni)
        let split_node = tail[0];
        c.nodes.push(split_node); // duplicate the break node at the head's end
        let hl = c.nodes.len() - 1;
        c.nodes[hl].out_handle = None;
        tail
    };
    let mut tail = Contour::new(tail_nodes, false);
    tail.nodes[0].in_handle = None;
    path.contours.insert(ci + 1, tail);
    Ok(())
}

/// Append open contour `b` onto the end of open contour `a`, then remove `b`.
/// If a's last anchor coincides with b's first anchor within `weld_eps`, the
/// duplicate node is merged (a's endpoint adopts b's outgoing handle).
pub fn join_contours(path: &mut PathData, a: usize, b: usize, weld_eps: f32) -> OpResult {
    if a == b {
        return Err("cannot join a contour to itself; use close_contour".into());
    }
    let len = path.contours.len();
    if a >= len || b >= len {
        return Err("contour index out of range".into());
    }
    if path.contours[a].closed || path.contours[b].closed {
        return Err("both contours must be open to join".into());
    }
    let b_nodes = path.contours[b].nodes.clone();
    {
        let ca = &mut path.contours[a];
        match (ca.nodes.last().copied(), b_nodes.first().copied()) {
            (Some(last), Some(first)) if last.anchor.distance_to(first.anchor) <= weld_eps => {
                if let Some(l) = ca.nodes.last_mut() {
                    l.out_handle = first.out_handle;
                }
                ca.nodes.extend_from_slice(&b_nodes[1..]);
            }
            _ => ca.nodes.extend_from_slice(&b_nodes),
        }
    }
    path.contours.remove(b);
    Ok(())
}

/// Close contour `ci`. If its first and last anchors coincide within `weld_eps`
/// the redundant last node is merged into the first (mirroring how the Pen tool
/// closes a path); otherwise a straight wrap segment is implied by `closed`.
pub fn close_contour(path: &mut PathData, ci: usize, weld_eps: f32) -> OpResult {
    let c = path
        .contours
        .get_mut(ci)
        .ok_or_else(|| format!("contour {ci} out of range"))?;
    if c.closed {
        return Ok(());
    }
    if c.nodes.len() >= 2 {
        let first = c.nodes[0];
        let last = c.nodes[c.nodes.len() - 1];
        if first.anchor.distance_to(last.anchor) <= weld_eps {
            c.nodes[0].in_handle = last.in_handle;
            c.nodes.pop();
        }
    }
    c.closed = true;
    Ok(())
}

// ---------------------------------------------------------------------------
// Align
// ---------------------------------------------------------------------------

/// Axis a set of nodes is aligned onto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Share a Y coordinate (the nodes line up on a horizontal line).
    Horizontal,
    /// Share an X coordinate (the nodes line up on a vertical line).
    Vertical,
}

/// Which value the aligned nodes snap to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignRef {
    First,
    Last,
    Min,
    Max,
    Average,
}

/// Align the anchors of `indices` in `contour` onto a shared coordinate. Each
/// node's handles move by the same delta as its anchor, so local curvature is
/// preserved. An empty selection is a no-op.
pub fn align_nodes(
    contour: &mut Contour,
    indices: &[usize],
    axis: Axis,
    reference: AlignRef,
) -> OpResult {
    if indices.is_empty() {
        return Ok(());
    }
    for &i in indices {
        if i >= contour.nodes.len() {
            return Err(format!("node {i} out of range"));
        }
    }
    let coord = |p: Point| match axis {
        Axis::Horizontal => p.y,
        Axis::Vertical => p.x,
    };
    let vals: Vec<f32> = indices
        .iter()
        .map(|&i| coord(contour.nodes[i].anchor))
        .collect();
    let target = match reference {
        AlignRef::First => vals[0],
        AlignRef::Last => *vals.last().unwrap(),
        AlignRef::Min => vals.iter().copied().fold(f32::INFINITY, f32::min),
        AlignRef::Max => vals.iter().copied().fold(f32::NEG_INFINITY, f32::max),
        AlignRef::Average => vals.iter().sum::<f32>() / vals.len() as f32,
    };
    for &i in indices {
        let node = &mut contour.nodes[i];
        let (dx, dy) = match axis {
            Axis::Horizontal => (0.0, target - node.anchor.y),
            Axis::Vertical => (target - node.anchor.x, 0.0),
        };
        let shift = |p: Point| Point::new(p.x + dx, p.y + dy);
        node.anchor = shift(node.anchor);
        node.in_handle = node.in_handle.map(shift);
        node.out_handle = node.out_handle.map(shift);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::geometry::cubic_bezier;
    use crate::core::vector::path::FillRule;

    fn close(a: Point, b: Point) -> bool {
        a.distance_to(b) < 1e-3
    }

    fn open_line() -> Contour {
        Contour::new(
            vec![
                Node::sharp(Point::new(0.0, 0.0)),
                Node::sharp(Point::new(10.0, 0.0)),
            ],
            false,
        )
    }

    fn one_cubic() -> Contour {
        // A single curved segment bulging upward.
        Contour::new(
            vec![
                Node::with_handles(
                    Point::new(0.0, 0.0),
                    Point::new(-1.0, 0.0),
                    Point::new(0.0, 10.0),
                    NodeKind::Smooth,
                ),
                Node::with_handles(
                    Point::new(30.0, 0.0),
                    Point::new(30.0, 10.0),
                    Point::new(31.0, 0.0),
                    NodeKind::Smooth,
                ),
            ],
            false,
        )
    }

    #[test]
    fn split_straight_inserts_midpoint() {
        let mut c = open_line();
        let at = split_segment(&mut c, 0, 0.5).unwrap();
        assert_eq!(at, 1);
        assert_eq!(c.nodes.len(), 3);
        assert!(close(c.nodes[1].anchor, Point::new(5.0, 0.0)));
        assert!(c.nodes[1].in_handle.is_none() && c.nodes[1].out_handle.is_none());
    }

    #[test]
    fn split_curve_preserves_shape() {
        let c = one_cubic();
        let (p0, p1, p2, p3) = c.segment(0).unwrap();
        // Reference points on the original curve.
        let refs: Vec<Point> = (0..=10)
            .map(|k| cubic_bezier(p0, p1, p2, p3, k as f32 / 10.0))
            .collect();

        let mut split = c.clone();
        let at = split_segment(&mut split, 0, 0.5).unwrap();
        assert_eq!(at, 1);
        assert_eq!(split.nodes.len(), 3);
        // The new anchor sits exactly at t=0.5 of the original curve.
        assert!(close(
            split.nodes[1].anchor,
            cubic_bezier(p0, p1, p2, p3, 0.5)
        ));

        // Sampling both halves reproduces the original curve.
        for (k, want) in refs.iter().enumerate() {
            let global = k as f32 / 10.0;
            let got = if global <= 0.5 {
                let (a0, a1, a2, a3) = split.segment(0).unwrap();
                cubic_bezier(a0, a1, a2, a3, global / 0.5)
            } else {
                let (b0, b1, b2, b3) = split.segment(1).unwrap();
                cubic_bezier(b0, b1, b2, b3, (global - 0.5) / 0.5)
            };
            assert!(
                close(got, *want),
                "mismatch at {global}: {got:?} vs {want:?}"
            );
        }
    }

    #[test]
    fn split_rejects_bad_parameter() {
        let mut c = open_line();
        assert!(split_segment(&mut c, 0, 0.0).is_err());
        assert!(split_segment(&mut c, 0, 1.0).is_err());
        assert!(split_segment(&mut c, 5, 0.5).is_err());
    }

    #[test]
    fn delete_node_shrinks() {
        let mut c = open_line();
        delete_node(&mut c, 0).unwrap();
        assert_eq!(c.nodes.len(), 1);
        assert!(delete_node(&mut c, 9).is_err());
    }

    #[test]
    fn straight_curve_conversion_roundtrip() {
        let mut c = open_line();
        set_segment_curved(&mut c, 0).unwrap();
        assert!(c.nodes[0].out_handle.is_some() && c.nodes[1].in_handle.is_some());
        set_segment_straight(&mut c, 0).unwrap();
        assert!(c.nodes[0].out_handle.is_none() && c.nodes[1].in_handle.is_none());
    }

    #[test]
    fn break_closed_reopens_with_duplicate() {
        let mut path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(10.0, 0.0)),
                    Node::sharp(Point::new(10.0, 10.0)),
                    Node::sharp(Point::new(0.0, 10.0)),
                ],
                true,
            )],
            FillRule::NonZero,
        );
        break_at_node(&mut path, 0, 2).unwrap();
        assert_eq!(path.contours.len(), 1);
        let c = &path.contours[0];
        assert!(!c.closed);
        assert_eq!(c.nodes.len(), 5); // 4 + duplicated break node
        assert_eq!(c.nodes[0].anchor, Point::new(10.0, 10.0));
        assert_eq!(c.nodes[4].anchor, Point::new(10.0, 10.0));
    }

    #[test]
    fn break_open_splits_into_two() {
        let mut path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(10.0, 0.0)),
                    Node::sharp(Point::new(20.0, 0.0)),
                ],
                false,
            )],
            FillRule::NonZero,
        );
        break_at_node(&mut path, 0, 1).unwrap();
        assert_eq!(path.contours.len(), 2);
        assert_eq!(path.contours[0].nodes.len(), 2);
        assert_eq!(path.contours[1].nodes.len(), 2);
        assert_eq!(path.contours[0].nodes[1].anchor, Point::new(10.0, 0.0));
        assert_eq!(path.contours[1].nodes[0].anchor, Point::new(10.0, 0.0));
        // Cannot break at an endpoint.
        assert!(break_at_node(&mut path, 0, 0).is_err());
    }

    #[test]
    fn join_welds_coincident_endpoints() {
        let mut path = PathData::new(
            vec![
                Contour::new(
                    vec![
                        Node::sharp(Point::new(0.0, 0.0)),
                        Node::sharp(Point::new(10.0, 0.0)),
                    ],
                    false,
                ),
                Contour::new(
                    vec![
                        Node::sharp(Point::new(10.0, 0.0)),
                        Node::sharp(Point::new(20.0, 0.0)),
                    ],
                    false,
                ),
            ],
            FillRule::NonZero,
        );
        join_contours(&mut path, 0, 1, 1e-3).unwrap();
        assert_eq!(path.contours.len(), 1);
        // 2 + 2 minus the welded duplicate = 3.
        assert_eq!(path.contours[0].nodes.len(), 3);
        assert_eq!(path.contours[0].nodes[2].anchor, Point::new(20.0, 0.0));
        assert!(join_contours(&mut path, 0, 0, 1e-3).is_err());
    }

    #[test]
    fn cusp_handle_moves_alone() {
        let mut node = Node::with_handles(
            Point::new(0.0, 0.0),
            Point::new(-2.0, 0.0),
            Point::new(2.0, 0.0),
            NodeKind::Cusp,
        );
        apply_handle_move(&mut node, HandleSide::Out, Point::new(3.0, 4.0));
        assert_eq!(node.out_handle, Some(Point::new(3.0, 4.0)));
        // The in handle is untouched for a cusp.
        assert_eq!(node.in_handle, Some(Point::new(-2.0, 0.0)));
    }

    #[test]
    fn smooth_handle_keeps_opposite_collinear_preserving_its_length() {
        // In handle length 5 (pointing left); dragging the out handle to (0,3)
        // must swing the in handle to (0,-5): collinear, opposite direction,
        // length preserved.
        let mut node = Node::with_handles(
            Point::new(0.0, 0.0),
            Point::new(-5.0, 0.0),
            Point::new(4.0, 0.0),
            NodeKind::Smooth,
        );
        apply_handle_move(&mut node, HandleSide::Out, Point::new(0.0, 3.0));
        let ih = node.in_handle.unwrap();
        assert!(close(ih, Point::new(0.0, -5.0)), "got {ih:?}");
        // The dragged handle is exactly where asked.
        assert_eq!(node.out_handle, Some(Point::new(0.0, 3.0)));
    }

    #[test]
    fn symmetric_handle_mirrors_to_equal_length() {
        let mut node = Node::with_handles(
            Point::new(1.0, 1.0),
            Point::new(-3.0, 1.0),
            Point::new(5.0, 1.0),
            NodeKind::Symmetric,
        );
        apply_handle_move(&mut node, HandleSide::Out, Point::new(4.0, 1.0));
        // Out at (4,1) => length 3 from anchor along +x, in mirrors to (-2,1).
        assert!(close(node.in_handle.unwrap(), Point::new(-2.0, 1.0)));
    }

    #[test]
    fn set_node_smooth_adds_collinear_handles_to_a_corner() {
        // A cusp corner of a closed triangle gets collinear handles.
        let mut path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(10.0, 0.0)),
                    Node::sharp(Point::new(5.0, 10.0)),
                ],
                true,
            )],
            FillRule::NonZero,
        );
        set_node_smooth(&mut path.contours[0], 1).unwrap();
        let n = path.contours[0].nodes[1];
        assert_eq!(n.kind, NodeKind::Smooth);
        let (ih, oh) = (n.in_handle.unwrap(), n.out_handle.unwrap());
        let a = n.anchor;
        // Collinear through the anchor: (ih-a) and (oh-a) are antiparallel.
        let v1 = (ih.x - a.x, ih.y - a.y);
        let v2 = (oh.x - a.x, oh.y - a.y);
        let cross = v1.0 * v2.1 - v1.1 * v2.0;
        let dot = v1.0 * v2.0 + v1.1 * v2.1;
        assert!(cross.abs() < 1e-3, "handles not collinear: {v1:?} {v2:?}");
        assert!(dot < 0.0, "handles must point opposite ways");
    }

    #[test]
    fn smooth_corner_limits_both_new_handles_to_the_shorter_edge() {
        let mut contour = Contour::new(
            vec![
                Node::sharp(Point::new(0.0, 0.0)),
                Node::sharp(Point::new(1000.0, 0.0)),
                Node::sharp(Point::new(1000.0, 30.0)),
            ],
            false,
        );
        set_node_smooth(&mut contour, 1).unwrap();
        let node = contour.nodes[1];
        let expected = 10.0; // one third of the 30-unit short edge
        assert!((node.anchor.distance_to(node.in_handle.unwrap()) - expected).abs() < 1e-4);
        assert!((node.anchor.distance_to(node.out_handle.unwrap()) - expected).abs() < 1e-4);
    }

    #[test]
    fn node_kind_setters_reject_out_of_range() {
        let mut c = open_line();
        assert!(set_node_smooth(&mut c, 9).is_err());
        assert!(set_node_symmetric(&mut c, 9).is_err());
        assert!(set_node_cusp(&mut c, 9).is_err());
    }

    #[test]
    fn align_shares_coordinate_and_moves_handles() {
        let mut c = Contour::new(
            vec![
                Node::with_handles(
                    Point::new(0.0, 0.0),
                    Point::new(-1.0, -1.0),
                    Point::new(1.0, 1.0),
                    NodeKind::Smooth,
                ),
                Node::sharp(Point::new(10.0, 8.0)),
            ],
            false,
        );
        align_nodes(&mut c, &[0, 1], Axis::Horizontal, AlignRef::First).unwrap();
        // Both anchors now share node 0's Y (0.0).
        assert!((c.nodes[0].anchor.y - 0.0).abs() < 1e-6);
        assert!((c.nodes[1].anchor.y - 0.0).abs() < 1e-6);
        // Node 0 did not move, so its handles are untouched.
        assert_eq!(c.nodes[0].out_handle, Some(Point::new(1.0, 1.0)));
    }
}

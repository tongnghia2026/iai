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

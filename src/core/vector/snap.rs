//! Vertex / edge snapping for the vector drawing tools (Arrow / Connector, Pen).
//!
//! The drawing tools place endpoints in *canvas/page* space. To let one line or
//! arrow *connect* to another object — the CorelDRAW "snap to objects" behaviour
//! the target users expect — an endpoint about to be placed is pulled to the
//! nearest existing anchor (corner / endpoint) or, failing that, to the nearest
//! point on an existing outline (an edge).
//!
//! This module is deliberately pure: it snaps a query point against a set of
//! candidate [`PathData`]s that the caller has already mapped into the SAME
//! coordinate space as the query. Gathering the document's vector layers into
//! that space (and turning a screen-pixel radius into canvas units via the zoom)
//! is the caller's job — see `ToolCtx::snap_vector_point` — which keeps the
//! vector core free of any dependency on layers, the canvas or the view.
//!
//! Priority is anchors before edges so two segments meeting at a corner weld to
//! the exact same point rather than one landing a hair off along the other's
//! outline.

use crate::core::geometry::Point;
use crate::core::vector::hittest::{nearest_node, nearest_point_on_path};
use crate::core::vector::path::PathData;

/// What a snap landed on — lets the caller colour the on-canvas feedback marker
/// (a filled dot for a hard corner/endpoint hit, a hollow one for a soft edge
/// hit) and, later, record a real connector relationship if desired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapKind {
    /// An existing anchor: a corner, or the endpoint of an open contour.
    Node,
    /// A point along an outline segment (not one of its anchors).
    Edge,
}

/// A successful snap: the point the endpoint should move to, and what it hit.
#[derive(Debug, Clone, Copy)]
pub struct SnapHit {
    pub point: Point,
    pub kind: SnapKind,
}

/// Snap `query` to the nearest node anchor (priority) or, failing that, the
/// nearest outline point across every path in `paths`, within `threshold`
/// (canvas units). Returns `None` when nothing is close enough.
///
/// `paths` must already be in the same coordinate space as `query`. Anchors are
/// preferred so meeting segments share an exact corner; an edge hit is only used
/// when no anchor is within range, and uses a slightly tighter radius so an edge
/// never wins over a corner the user was clearly aiming at.
pub fn snap_to_paths(query: Point, paths: &[PathData], threshold: f32) -> Option<SnapHit> {
    if threshold <= 0.0 {
        return None;
    }

    // 1) Corners / endpoints win. Take the globally nearest anchor in range.
    let mut best_node: Option<(f32, Point)> = None;
    for path in paths {
        if let Some(hit) = nearest_node(path, query) {
            if hit.distance <= threshold {
                let anchor = path.contours[hit.contour].nodes[hit.node].anchor;
                if best_node.map_or(true, |(d, _)| hit.distance < d) {
                    best_node = Some((hit.distance, anchor));
                }
            }
        }
    }
    if let Some((_, point)) = best_node {
        return Some(SnapHit {
            point,
            kind: SnapKind::Node,
        });
    }

    // 2) Otherwise land on the nearest outline. A tighter radius keeps an edge
    //    from grabbing the cursor from as far away as a corner would.
    let edge_threshold = threshold * 0.6;
    let flat_tol = (edge_threshold * 0.25).max(0.05);
    let mut best_edge: Option<(f32, Point)> = None;
    for path in paths {
        if let Some(hit) = nearest_point_on_path(path, query, flat_tol) {
            if hit.distance <= edge_threshold && best_edge.map_or(true, |(d, _)| hit.distance < d) {
                best_edge = Some((hit.distance, hit.point));
            }
        }
    }
    best_edge.map(|(_, point)| SnapHit {
        point,
        kind: SnapKind::Edge,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::vector::path::{Contour, FillRule, Node};

    fn open_line(a: Point, b: Point) -> PathData {
        PathData::new(
            vec![Contour::new(vec![Node::sharp(a), Node::sharp(b)], false)],
            FillRule::NonZero,
        )
    }

    #[test]
    fn snaps_to_a_nearby_endpoint() {
        let paths = [open_line(Point::new(0.0, 0.0), Point::new(100.0, 0.0))];
        // Query just past the (100,0) endpoint.
        let hit = snap_to_paths(Point::new(103.0, 2.0), &paths, 8.0).unwrap();
        assert_eq!(hit.kind, SnapKind::Node);
        assert_eq!(hit.point, Point::new(100.0, 0.0));
    }

    #[test]
    fn corner_beats_edge_when_both_in_range() {
        // A right-angle: horizontal + vertical meeting at (100,0).
        let paths = [PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(100.0, 0.0)),
                    Node::sharp(Point::new(100.0, 100.0)),
                ],
                false,
            )],
            FillRule::NonZero,
        )];
        // Near the corner but also near both edges — the anchor must win.
        let hit = snap_to_paths(Point::new(97.0, 3.0), &paths, 8.0).unwrap();
        assert_eq!(hit.kind, SnapKind::Node);
        assert_eq!(hit.point, Point::new(100.0, 0.0));
    }

    #[test]
    fn falls_back_to_edge_away_from_any_anchor() {
        let paths = [open_line(Point::new(0.0, 0.0), Point::new(100.0, 0.0))];
        // Above the middle of the segment, far from both endpoints.
        let hit = snap_to_paths(Point::new(50.0, 2.0), &paths, 8.0).unwrap();
        assert_eq!(hit.kind, SnapKind::Edge);
        assert!((hit.point.x - 50.0).abs() < 0.5);
        assert!(hit.point.y.abs() < 0.5);
    }

    #[test]
    fn nothing_in_range_returns_none() {
        let paths = [open_line(Point::new(0.0, 0.0), Point::new(100.0, 0.0))];
        assert!(snap_to_paths(Point::new(50.0, 40.0), &paths, 8.0).is_none());
        // A zero/negative radius disables snapping entirely.
        assert!(snap_to_paths(Point::new(100.0, 0.0), &paths, 0.0).is_none());
    }
}

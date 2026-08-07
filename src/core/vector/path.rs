#![allow(dead_code)]
//! Vector path data model (foundation task T1.2).
//!
//! This is the *source of truth* for a Path layer, per the plan (Mục 3.2): the
//! TileMap raster is only a cache/fallback derived from this. Nothing here
//! depends on UI, GPU, the app event loop or serialization — that isolation is
//! the T1 Definition of Done.
//!
//! A [`PathData`] is a list of [`Contour`]s (compound paths / holes live here,
//! NOT in separate hidden layers — Mục 3.3). Each contour is a ring of
//! [`Node`]s joined by cubic Bézier segments; a segment degenerates to a
//! straight line when both of its handles are absent. Handles are stored as
//! *absolute* control points so an affine transform maps every point uniformly.

use crate::core::geometry::{cubic_bezier, Point, Rect};
use crate::core::vector::affine::AffineTransform;

/// Parser-safety limits (Mục 5.4). Enforced by [`PathData::validate`]; the .iai
/// reader (task T4.3) will reuse these before trusting untrusted input.
pub const MAX_CONTOURS: usize = 65_536;
pub const MAX_NODES_PER_CONTOUR: usize = 1_000_000;
pub const MAX_TOTAL_NODES: usize = 4_000_000;

/// How the two handles of a node relate. Matches the CorelDRAW Shape-tool
/// vocabulary the target users know (Mục 2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NodeKind {
    /// Independent handles; a corner.
    #[default]
    Cusp,
    /// Handles collinear through the anchor, lengths may differ.
    Smooth,
    /// Handles collinear and equal length (mirror).
    Symmetric,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

impl Default for FillRule {
    fn default() -> Self {
        FillRule::NonZero
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Node {
    pub anchor: Point,
    /// Absolute control point for the segment arriving at this node.
    pub in_handle: Option<Point>,
    /// Absolute control point for the segment leaving this node.
    pub out_handle: Option<Point>,
    pub kind: NodeKind,
}

impl Node {
    /// A corner node with no handles (straight segments on both sides).
    pub fn sharp(anchor: Point) -> Self {
        Self {
            anchor,
            in_handle: None,
            out_handle: None,
            kind: NodeKind::Cusp,
        }
    }

    pub fn with_handles(
        anchor: Point,
        in_handle: Point,
        out_handle: Point,
        kind: NodeKind,
    ) -> Self {
        Self {
            anchor,
            in_handle: Some(in_handle),
            out_handle: Some(out_handle),
            kind,
        }
    }

    fn is_finite(&self) -> bool {
        let ok = |p: Point| p.x.is_finite() && p.y.is_finite();
        ok(self.anchor) && self.in_handle.map_or(true, ok) && self.out_handle.map_or(true, ok)
    }

    fn transform(&mut self, t: &AffineTransform) {
        self.anchor = t.apply_point(self.anchor);
        self.in_handle = self.in_handle.map(|p| t.apply_point(p));
        self.out_handle = self.out_handle.map(|p| t.apply_point(p));
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Contour {
    pub nodes: Vec<Node>,
    pub closed: bool,
}

impl Contour {
    pub fn new(nodes: Vec<Node>, closed: bool) -> Self {
        Self { nodes, closed }
    }

    /// Number of cubic segments: `n-1` for an open contour, `n` for a closed one
    /// (the wrap-around segment back to the first node).
    pub fn segment_count(&self) -> usize {
        let n = self.nodes.len();
        if n < 2 {
            0
        } else if self.closed {
            n
        } else {
            n - 1
        }
    }

    /// Control points `(p0,p1,p2,p3)` of segment `i`. A missing handle collapses
    /// to its anchor, turning the cubic into a straight line.
    pub fn segment(&self, i: usize) -> Option<(Point, Point, Point, Point)> {
        if i >= self.segment_count() {
            return None;
        }
        let a = &self.nodes[i];
        let b = &self.nodes[(i + 1) % self.nodes.len()];
        let p0 = a.anchor;
        let p3 = b.anchor;
        let p1 = a.out_handle.unwrap_or(p0);
        let p2 = b.in_handle.unwrap_or(p3);
        Some((p0, p1, p2, p3))
    }

    pub fn set_closed(&mut self, closed: bool) {
        self.closed = closed;
    }

    /// Reverse traversal direction: node order flips and each node's in/out
    /// handles swap, so the drawn shape is identical.
    pub fn reverse(&mut self) {
        self.nodes.reverse();
        for n in &mut self.nodes {
            std::mem::swap(&mut n.in_handle, &mut n.out_handle);
        }
    }

    /// Bounding box of the control hull (anchors + handles). A conservative
    /// superset of the true curve bounds — cheap and correct for invalidation;
    /// tight curve bounds come later with the flatten pass (task T1.4).
    pub fn control_bounds(&self) -> Option<Rect> {
        let mut it = self.nodes.iter().flat_map(|n| {
            std::iter::once(n.anchor)
                .chain(n.in_handle)
                .chain(n.out_handle)
        });
        let first = it.next()?;
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (first.x, first.y, first.x, first.y);
        for p in it {
            min_x = min_x.min(p.x);
            min_y = min_y.min(p.y);
            max_x = max_x.max(p.x);
            max_y = max_y.max(p.y);
        }
        Some(Rect::new(min_x, min_y, max_x - min_x, max_y - min_y))
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PathData {
    pub contours: Vec<Contour>,
    pub fill_rule: FillRule,
}

impl PathData {
    pub fn new(contours: Vec<Contour>, fill_rule: FillRule) -> Self {
        Self {
            contours,
            fill_rule,
        }
    }

    pub fn total_nodes(&self) -> usize {
        self.contours.iter().map(|c| c.nodes.len()).sum()
    }

    /// Reject non-finite coordinates and over-limit sizes. Returns a stable
    /// error string so both the editor and the .iai parser can surface it.
    pub fn validate(&self) -> Result<(), String> {
        if self.contours.len() > MAX_CONTOURS {
            return Err(format!(
                "too many contours: {} > {}",
                self.contours.len(),
                MAX_CONTOURS
            ));
        }
        let mut total = 0usize;
        for (ci, c) in self.contours.iter().enumerate() {
            if c.nodes.len() > MAX_NODES_PER_CONTOUR {
                return Err(format!(
                    "contour {} has too many nodes: {} > {}",
                    ci,
                    c.nodes.len(),
                    MAX_NODES_PER_CONTOUR
                ));
            }
            total += c.nodes.len();
            for (ni, n) in c.nodes.iter().enumerate() {
                if !n.is_finite() {
                    return Err(format!("non-finite coordinate at contour {ci}, node {ni}"));
                }
            }
        }
        if total > MAX_TOTAL_NODES {
            return Err(format!(
                "too many nodes overall: {total} > {MAX_TOTAL_NODES}"
            ));
        }
        Ok(())
    }

    /// Apply an affine transform to every anchor and handle in place. Because
    /// handles are absolute, the whole path maps uniformly.
    pub fn transform(&mut self, t: &AffineTransform) {
        for c in &mut self.contours {
            for n in &mut c.nodes {
                n.transform(t);
            }
        }
    }

    /// Union of every contour's control-hull bounds (see [`Contour::control_bounds`]).
    pub fn control_bounds(&self) -> Option<Rect> {
        self.contours
            .iter()
            .filter_map(|c| c.control_bounds())
            .reduce(|a, b| a.union(b))
    }

    /// Sample point at parameter `t ∈ [0,1]` within segment `i` of contour `ci`.
    /// Thin wrapper over [`cubic_bezier`] shared with the Pen tool.
    pub fn sample(&self, ci: usize, i: usize, t: f32) -> Option<Point> {
        let (p0, p1, p2, p3) = self.contours.get(ci)?.segment(i)?;
        Some(cubic_bezier(p0, p1, p2, p3, t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sq() -> Contour {
        // Unit square as four sharp corners.
        Contour::new(
            vec![
                Node::sharp(Point::new(0.0, 0.0)),
                Node::sharp(Point::new(10.0, 0.0)),
                Node::sharp(Point::new(10.0, 10.0)),
                Node::sharp(Point::new(0.0, 10.0)),
            ],
            true,
        )
    }

    #[test]
    fn segment_counts_open_vs_closed() {
        let mut c = sq();
        assert_eq!(c.segment_count(), 4);
        c.set_closed(false);
        assert_eq!(c.segment_count(), 3);
    }

    #[test]
    fn straight_segment_when_handles_absent() {
        let c = sq();
        // Midpoint of the first (horizontal) edge.
        let (p0, p1, p2, p3) = c.segment(0).unwrap();
        let mid = cubic_bezier(p0, p1, p2, p3, 0.5);
        assert!((mid.x - 5.0).abs() < 1e-3 && mid.y.abs() < 1e-3);
    }

    #[test]
    fn control_bounds_of_square() {
        let b = sq().control_bounds().unwrap();
        assert_eq!((b.x, b.y, b.w, b.h), (0.0, 0.0, 10.0, 10.0));
    }

    #[test]
    fn transform_moves_bounds() {
        let mut p = PathData::new(vec![sq()], FillRule::NonZero);
        p.transform(&AffineTransform::translate(100.0, 5.0));
        let b = p.control_bounds().unwrap();
        assert_eq!((b.x, b.y), (100.0, 5.0));
    }

    #[test]
    fn reverse_keeps_shape_and_swaps_handles() {
        let mut c = Contour::new(
            vec![
                Node::with_handles(
                    Point::new(0.0, 0.0),
                    Point::new(-1.0, 0.0),
                    Point::new(1.0, 0.0),
                    NodeKind::Smooth,
                ),
                Node::sharp(Point::new(10.0, 0.0)),
            ],
            false,
        );
        c.reverse();
        assert_eq!(c.nodes[0].anchor, Point::new(10.0, 0.0));
        // The former first node is now last with its handles swapped.
        assert_eq!(c.nodes[1].in_handle, Some(Point::new(1.0, 0.0)));
        assert_eq!(c.nodes[1].out_handle, Some(Point::new(-1.0, 0.0)));
    }

    #[test]
    fn validate_rejects_non_finite() {
        let bad = PathData::new(
            vec![Contour::new(
                vec![Node::sharp(Point::new(f32::NAN, 0.0))],
                false,
            )],
            FillRule::NonZero,
        );
        assert!(bad.validate().is_err());
        assert!(PathData::new(vec![sq()], FillRule::NonZero)
            .validate()
            .is_ok());
    }
}

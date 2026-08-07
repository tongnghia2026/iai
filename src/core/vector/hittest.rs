#![allow(dead_code)]
//! Hit-testing on flattened paths (foundation task T1.5).
//!
//! The Pick tool (Phase 3) selects objects by fill/outline; the Node tool grabs
//! the nearest node or clicks a segment to insert one. All four queries below
//! run against the adaptive flattening from `flatten.rs`, so accuracy tracks the
//! caller's tolerance and there is no second geometry engine.
//!
//! Coordinates here are object-local path units (Mục 3.4). Turning a screen-pixel
//! threshold into a local tolerance is the caller's job — that keeps the model
//! independent of zoom, as the plan requires.

use crate::core::geometry::Point;
use crate::core::vector::flatten::{flatten_contour, flatten_path, DEFAULT_TOLERANCE};
use crate::core::vector::path::{FillRule, PathData};

/// Closest node found by [`nearest_node`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeHit {
    pub contour: usize,
    pub node: usize,
    pub distance: f32,
}

/// Closest point on the outline found by [`nearest_point_on_path`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentHit {
    pub contour: usize,
    pub segment: usize,
    pub point: Point,
    pub distance: f32,
}

/// Signed area term: `> 0` when `p` is left of the directed edge `a→b`.
#[inline]
fn is_left(a: Point, b: Point, p: Point) -> f32 {
    (b.x - a.x) * (p.y - a.y) - (p.x - a.x) * (b.y - a.y)
}

/// Winding number of `p` about one closed polyline (last point implicitly joins
/// the first). Nonzero ⇒ inside under the non-zero rule.
fn contour_winding(poly: &[Point], p: Point) -> i32 {
    let n = poly.len();
    if n < 2 {
        return 0;
    }
    let mut wn = 0i32;
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        if a.y <= p.y {
            if b.y > p.y && is_left(a, b, p) > 0.0 {
                wn += 1;
            }
        } else if b.y <= p.y && is_left(a, b, p) < 0.0 {
            wn -= 1;
        }
    }
    wn
}

/// Even-odd crossing parity of `p` for one closed polyline.
fn contour_parity(poly: &[Point], p: Point) -> bool {
    let n = poly.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let a = poly[i];
        let b = poly[j];
        if ((a.y > p.y) != (b.y > p.y)) && (p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Distance from `p` to segment `a→b` and the closest point on it.
#[inline]
fn point_seg(p: Point, a: Point, b: Point) -> (Point, f32) {
    let abx = b.x - a.x;
    let aby = b.y - a.y;
    let len2 = abx * abx + aby * aby;
    let t = if len2 <= f32::EPSILON {
        0.0
    } else {
        (((p.x - a.x) * abx + (p.y - a.y) * aby) / len2).clamp(0.0, 1.0)
    };
    let c = Point::new(a.x + abx * t, a.y + aby * t);
    (c, p.distance_to(c))
}

/// True when `p` lies inside the filled region, honouring the path's
/// [`FillRule`]. Open contours are treated as implicitly closed (as fill does).
pub fn fill_contains(path: &PathData, p: Point, tol: f32) -> bool {
    let polys = flatten_path(path, tol);
    match path.fill_rule {
        FillRule::NonZero => {
            polys
                .iter()
                .map(|poly| contour_winding(poly, p))
                .sum::<i32>()
                != 0
        }
        FillRule::EvenOdd => polys
            .iter()
            .fold(false, |acc, poly| acc ^ contour_parity(poly, p)),
    }
}

/// True when `p` is within `half_width + tol` of any outline segment. `tol` is
/// the pick slop; the flattening tolerance is derived from it so a fat pick
/// radius doesn't over-tessellate.
pub fn stroke_hit(path: &PathData, p: Point, half_width: f32, tol: f32) -> bool {
    let thresh = half_width + tol;
    let flat_tol = tol.min(DEFAULT_TOLERANCE).max(1e-3);
    for c in &path.contours {
        let poly = flatten_contour(c, flat_tol);
        match poly.len() {
            0 => continue,
            1 => {
                if poly[0].distance_to(p) <= thresh {
                    return true;
                }
            }
            _ => {
                for w in poly.windows(2) {
                    if point_seg(p, w[0], w[1]).1 <= thresh {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Nearest anchor to `p` across all contours, or `None` for an empty path.
pub fn nearest_node(path: &PathData, p: Point) -> Option<NodeHit> {
    let mut best: Option<NodeHit> = None;
    for (ci, c) in path.contours.iter().enumerate() {
        for (ni, node) in c.nodes.iter().enumerate() {
            let d = node.anchor.distance_to(p);
            if best.map_or(true, |b| d < b.distance) {
                best = Some(NodeHit {
                    contour: ci,
                    node: ni,
                    distance: d,
                });
            }
        }
    }
    best
}

/// Nearest point on the outline to `p`, with the originating contour/segment so
/// the Node tool can insert a node there. Flattens each segment on its own to
/// keep the segment index meaningful.
pub fn nearest_point_on_path(path: &PathData, p: Point, tol: f32) -> Option<SegmentHit> {
    let flat_tol = tol.max(1e-3);
    let mut best: Option<SegmentHit> = None;
    for (ci, c) in path.contours.iter().enumerate() {
        for si in 0..c.segment_count() {
            let (p0, p1, p2, p3) = c.segment(si).expect("segment in range");
            let poly = crate::core::vector::flatten::flatten_cubic(p0, p1, p2, p3, flat_tol);
            for w in poly.windows(2) {
                let (cp, d) = point_seg(p, w[0], w[1]);
                if best.map_or(true, |b| d < b.distance) {
                    best = Some(SegmentHit {
                        contour: ci,
                        segment: si,
                        point: cp,
                        distance: d,
                    });
                }
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::vector::path::{Contour, Node};

    fn square(closed: bool) -> Contour {
        Contour::new(
            vec![
                Node::sharp(Point::new(0.0, 0.0)),
                Node::sharp(Point::new(10.0, 0.0)),
                Node::sharp(Point::new(10.0, 10.0)),
                Node::sharp(Point::new(0.0, 10.0)),
            ],
            closed,
        )
    }

    #[test]
    fn fill_inside_and_outside_square() {
        let path = PathData::new(vec![square(true)], FillRule::NonZero);
        assert!(fill_contains(
            &path,
            Point::new(5.0, 5.0),
            DEFAULT_TOLERANCE
        ));
        assert!(!fill_contains(
            &path,
            Point::new(15.0, 5.0),
            DEFAULT_TOLERANCE
        ));
        assert!(!fill_contains(
            &path,
            Point::new(-1.0, -1.0),
            DEFAULT_TOLERANCE
        ));
    }

    #[test]
    fn even_odd_hole_is_outside() {
        // Outer 0..20 square with an inner 5..15 square hole.
        let outer = Contour::new(
            vec![
                Node::sharp(Point::new(0.0, 0.0)),
                Node::sharp(Point::new(20.0, 0.0)),
                Node::sharp(Point::new(20.0, 20.0)),
                Node::sharp(Point::new(0.0, 20.0)),
            ],
            true,
        );
        let inner = Contour::new(
            vec![
                Node::sharp(Point::new(5.0, 5.0)),
                Node::sharp(Point::new(15.0, 5.0)),
                Node::sharp(Point::new(15.0, 15.0)),
                Node::sharp(Point::new(5.0, 15.0)),
            ],
            true,
        );
        let path = PathData::new(vec![outer, inner], FillRule::EvenOdd);
        // Between the two squares: filled. In the middle hole: not filled.
        assert!(fill_contains(&path, Point::new(2.0, 2.0), 0.1));
        assert!(!fill_contains(&path, Point::new(10.0, 10.0), 0.1));
    }

    #[test]
    fn stroke_hit_near_edge_only() {
        let path = PathData::new(vec![square(true)], FillRule::NonZero);
        // Just outside the top edge, within a 1px pick radius.
        assert!(stroke_hit(&path, Point::new(5.0, 0.4), 0.5, 0.5));
        // Deep inside — on no edge.
        assert!(!stroke_hit(&path, Point::new(5.0, 5.0), 0.5, 0.5));
    }

    #[test]
    fn nearest_node_picks_closest_corner() {
        let path = PathData::new(vec![square(true)], FillRule::NonZero);
        let hit = nearest_node(&path, Point::new(9.0, 1.0)).unwrap();
        assert_eq!(hit.node, 1); // (10,0)
        assert!(hit.distance < 2.0);
        assert!(nearest_node(&PathData::default(), Point::new(0.0, 0.0)).is_none());
    }

    #[test]
    fn nearest_point_lands_on_edge() {
        let path = PathData::new(vec![square(false)], FillRule::NonZero);
        // Click above the middle of the top edge.
        let hit = nearest_point_on_path(&path, Point::new(5.0, -3.0), 0.1).unwrap();
        assert_eq!(hit.segment, 0);
        assert!(hit.point.distance_to(Point::new(5.0, 0.0)) < 0.5);
        assert!((hit.distance - 3.0).abs() < 0.5);
    }
}

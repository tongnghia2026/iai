#![allow(dead_code)]
//! Build editable vector [`PathData`] from primitive geometry — the geometry half
//! of "Convert Shape to Curves" (Giai đoạn 4). Pure and independent of `shape.rs`:
//! the functions take raw coordinates (not `ShapeData`), mirroring how
//! [`crate::core::vector::style::VectorStyle::from_shape_fields`] keeps the style
//! adapter decoupled. All coordinates are whatever space the caller passes in
//! (the app converts a shape's layer-local box to canvas space first).

use crate::core::geometry::Point;
use crate::core::vector::path::{Contour, FillRule, Node, NodeKind, PathData};

/// Cubic control-arm fraction for a 90° circular arc — the classic kappa,
/// `4/3·(√2−1)`. A quarter arc of radius `r` uses handles `KAPPA·r` long.
const KAPPA: f32 = 0.552_284_75;

fn corner_node(anchor: Point, in_handle: Option<Point>, out_handle: Option<Point>) -> Node {
    Node {
        anchor,
        in_handle,
        out_handle,
        kind: NodeKind::Cusp,
    }
}

/// A rectangle (or rounded rectangle when `radius > 0`) inscribed in the box with
/// opposite corners `(x0,y0)`–`(x1,y1)`, as one closed contour. Sharp corners for
/// radius 0; otherwise each corner is a cubic quarter-arc and the sides are
/// straight. The radius is clamped to half the shorter side.
pub fn rect_path(x0: f32, y0: f32, x1: f32, y1: f32, radius: f32) -> PathData {
    let (lx, rx) = (x0.min(x1), x0.max(x1));
    let (ty, by) = (y0.min(y1), y0.max(y1));
    let (w, h) = (rx - lx, by - ty);
    let r = radius.max(0.0).min(w * 0.5).min(h * 0.5);

    let contour = if r <= f32::EPSILON {
        Contour::new(
            vec![
                Node::sharp(Point::new(lx, ty)),
                Node::sharp(Point::new(rx, ty)),
                Node::sharp(Point::new(rx, by)),
                Node::sharp(Point::new(lx, by)),
            ],
            true,
        )
    } else {
        let k = KAPPA * r;
        // Eight nodes, clockwise from the start of the top edge. Each corner node
        // carries a handle only on its arc side (the straight side has none).
        let nodes = vec![
            // top edge start (end of the top-left arc)
            corner_node(
                Point::new(lx + r, ty),
                Some(Point::new(lx + r - k, ty)),
                None,
            ),
            // top edge end → top-right arc
            corner_node(
                Point::new(rx - r, ty),
                None,
                Some(Point::new(rx - r + k, ty)),
            ),
            // right edge start (end of the top-right arc)
            corner_node(
                Point::new(rx, ty + r),
                Some(Point::new(rx, ty + r - k)),
                None,
            ),
            // right edge end → bottom-right arc
            corner_node(
                Point::new(rx, by - r),
                None,
                Some(Point::new(rx, by - r + k)),
            ),
            // bottom edge start (end of the bottom-right arc)
            corner_node(
                Point::new(rx - r, by),
                Some(Point::new(rx - r + k, by)),
                None,
            ),
            // bottom edge end → bottom-left arc
            corner_node(
                Point::new(lx + r, by),
                None,
                Some(Point::new(lx + r - k, by)),
            ),
            // left edge start (end of the bottom-left arc)
            corner_node(
                Point::new(lx, by - r),
                Some(Point::new(lx, by - r + k)),
                None,
            ),
            // left edge end → top-left arc
            corner_node(
                Point::new(lx, ty + r),
                None,
                Some(Point::new(lx, ty + r - k)),
            ),
        ];
        Contour::new(nodes, true)
    };
    PathData::new(vec![contour], FillRule::NonZero)
}

/// An axis-aligned ellipse inscribed in the box `(x0,y0)`–`(x1,y1)`, as a closed
/// four-node cubic approximation (Smooth nodes at top/right/bottom/left).
pub fn ellipse_path(x0: f32, y0: f32, x1: f32, y1: f32) -> PathData {
    let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
    let rx = (x1 - x0).abs() * 0.5;
    let ry = (y1 - y0).abs() * 0.5;
    let (kx, ky) = (KAPPA * rx, KAPPA * ry);

    let smooth = |anchor: Point, in_h: Point, out_h: Point| Node {
        anchor,
        in_handle: Some(in_h),
        out_handle: Some(out_h),
        kind: NodeKind::Smooth,
    };
    let nodes = vec![
        // top
        smooth(
            Point::new(cx, cy - ry),
            Point::new(cx - kx, cy - ry),
            Point::new(cx + kx, cy - ry),
        ),
        // right
        smooth(
            Point::new(cx + rx, cy),
            Point::new(cx + rx, cy - ky),
            Point::new(cx + rx, cy + ky),
        ),
        // bottom
        smooth(
            Point::new(cx, cy + ry),
            Point::new(cx + kx, cy + ry),
            Point::new(cx - kx, cy + ry),
        ),
        // left
        smooth(
            Point::new(cx - rx, cy),
            Point::new(cx - rx, cy + ky),
            Point::new(cx - rx, cy - ky),
        ),
    ];
    PathData::new(vec![Contour::new(nodes, true)], FillRule::NonZero)
}

/// A straight line from `(x0,y0)` to `(x1,y1)`, as a two-node OPEN contour.
/// Orientation is preserved (no normalization).
pub fn line_path(x0: f32, y0: f32, x1: f32, y1: f32) -> PathData {
    PathData::new(
        vec![Contour::new(
            vec![
                Node::sharp(Point::new(x0, y0)),
                Node::sharp(Point::new(x1, y1)),
            ],
            false,
        )],
        FillRule::NonZero,
    )
}

/// A regular polygon with `sides` edges inscribed in the box `(x0,y0)`–`(x1,y1)`,
/// as one closed contour of sharp nodes (first vertex points up). `sides` clamped
/// to `[3, 100]`. Matches [`crate::core::shape::ShapeData::polygon_vertices`].
pub fn polygon_path(x0: f32, y0: f32, x1: f32, y1: f32, sides: u32) -> PathData {
    let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
    let rx = (x1 - x0).abs() * 0.5;
    let ry = (y1 - y0).abs() * 0.5;
    let n = sides.clamp(3, 100);
    let start = -std::f32::consts::FRAC_PI_2;
    let nodes = (0..n)
        .map(|i| {
            let a = start + std::f32::consts::TAU * i as f32 / n as f32;
            Node::sharp(Point::new(cx + rx * a.cos(), cy + ry * a.sin()))
        })
        .collect();
    PathData::new(vec![Contour::new(nodes, true)], FillRule::NonZero)
}

/// An N-pointed star inscribed in the box, as one closed contour of `2·points`
/// sharp nodes alternating the outer radius and `inner`×outer (first point up).
/// `points` clamped to `[3, 100]`, `inner` to `[0.05, 0.95]`.
pub fn star_path(x0: f32, y0: f32, x1: f32, y1: f32, points: u32, inner: f32) -> PathData {
    let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
    let rx = (x1 - x0).abs() * 0.5;
    let ry = (y1 - y0).abs() * 0.5;
    let n = points.clamp(3, 100);
    let f_inner = inner.clamp(0.05, 0.95);
    let start = -std::f32::consts::FRAC_PI_2;
    let nodes = (0..2 * n)
        .map(|i| {
            let a = start + std::f32::consts::PI * i as f32 / n as f32;
            let f = if i % 2 == 0 { 1.0 } else { f_inner };
            Node::sharp(Point::new(cx + rx * f * a.cos(), cy + ry * f * a.sin()))
        })
        .collect();
    PathData::new(vec![Contour::new(nodes, true)], FillRule::NonZero)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::geometry::cubic_bezier;

    #[test]
    fn sharp_rect_is_four_corners() {
        let p = rect_path(10.0, 20.0, 50.0, 40.0, 0.0);
        assert_eq!(p.contours.len(), 1);
        let c = &p.contours[0];
        assert!(c.closed);
        assert_eq!(c.nodes.len(), 4);
        assert_eq!(c.nodes[0].anchor, Point::new(10.0, 20.0));
        assert_eq!(c.nodes[2].anchor, Point::new(50.0, 40.0));
        assert!(c
            .nodes
            .iter()
            .all(|n| n.in_handle.is_none() && n.out_handle.is_none()));
    }

    #[test]
    fn rect_normalizes_reversed_corners() {
        // Passing corners in the "wrong" order still yields the same box.
        let a = rect_path(50.0, 40.0, 10.0, 20.0, 0.0);
        let b = rect_path(10.0, 20.0, 50.0, 40.0, 0.0);
        assert_eq!(a.control_bounds(), b.control_bounds());
    }

    #[test]
    fn rounded_rect_has_eight_nodes_and_stays_inside_the_box() {
        let p = rect_path(0.0, 0.0, 100.0, 60.0, 10.0);
        let c = &p.contours[0];
        assert_eq!(c.nodes.len(), 8);
        // Every anchor sits on the box boundary (rounded corners never bulge out).
        for n in &c.nodes {
            assert!(n.anchor.x >= -0.01 && n.anchor.x <= 100.01);
            assert!(n.anchor.y >= -0.01 && n.anchor.y <= 60.01);
        }
        // The mid-top edge point is on the top edge (straight segment there).
        let (p0, p1, p2, p3) = c.segment(0).unwrap();
        let mid = cubic_bezier(p0, p1, p2, p3, 0.5);
        assert!(
            (mid.y - 0.0).abs() < 1e-3,
            "top edge is straight, got {mid:?}"
        );
    }

    #[test]
    fn rounded_rect_clamps_radius_to_half_the_short_side() {
        // Radius 999 on a 40-tall box clamps to 20 — corners meet at the mid-edges.
        let p = rect_path(0.0, 0.0, 200.0, 40.0, 999.0);
        let c = &p.contours[0];
        // On the short (vertical) axis the two right-side nodes coincide at cy=20.
        let ys: Vec<f32> = c.nodes.iter().map(|n| n.anchor.y).collect();
        assert!(ys.iter().any(|y| (*y - 20.0).abs() < 0.01));
    }

    #[test]
    fn ellipse_samples_lie_on_the_ellipse() {
        // Unit-ish ellipse: rx=30, ry=20, centred at (40,25).
        let (cx, cy, rx, ry) = (40.0f32, 25.0f32, 30.0f32, 20.0f32);
        let p = ellipse_path(cx - rx, cy - ry, cx + rx, cy + ry);
        let c = &p.contours[0];
        assert_eq!(c.nodes.len(), 4);
        assert!(c.closed);
        // Sample the whole outline; each point should satisfy (x/rx)²+(y/ry)² ≈ 1.
        let mut max_err = 0.0f32;
        for seg in 0..c.segment_count() {
            let (a0, a1, a2, a3) = c.segment(seg).unwrap();
            for k in 0..=8 {
                let pt = cubic_bezier(a0, a1, a2, a3, k as f32 / 8.0);
                let e = (((pt.x - cx) / rx).powi(2) + ((pt.y - cy) / ry).powi(2) - 1.0).abs();
                max_err = max_err.max(e);
            }
        }
        // The 4-cubic circle approximation is accurate to well under 1%.
        assert!(max_err < 0.01, "ellipse approximation error {max_err}");
    }

    #[test]
    fn line_is_two_open_nodes() {
        let p = line_path(5.0, 6.0, 70.0, 80.0);
        let c = &p.contours[0];
        assert!(!c.closed);
        assert_eq!(c.nodes.len(), 2);
        assert_eq!(c.nodes[0].anchor, Point::new(5.0, 6.0));
        assert_eq!(c.nodes[1].anchor, Point::new(70.0, 80.0));
    }

    #[test]
    fn polygon_has_n_sharp_nodes_first_up() {
        let p = polygon_path(0.0, 0.0, 100.0, 100.0, 6);
        let c = &p.contours[0];
        assert!(c.closed);
        assert_eq!(c.nodes.len(), 6);
        assert!(c
            .nodes
            .iter()
            .all(|n| n.in_handle.is_none() && n.out_handle.is_none()));
        assert!((c.nodes[0].anchor.x - 50.0).abs() < 0.01 && c.nodes[0].anchor.y.abs() < 0.01);
    }

    #[test]
    fn star_alternates_outer_and_inner_radii() {
        let p = star_path(0.0, 0.0, 100.0, 100.0, 5, 0.5);
        let c = &p.contours[0];
        assert!(c.closed);
        assert_eq!(c.nodes.len(), 10);
        let d = |i: usize| {
            ((c.nodes[i].anchor.x - 50.0).powi(2) + (c.nodes[i].anchor.y - 50.0).powi(2)).sqrt()
        };
        assert!((d(0) - 50.0).abs() < 0.5, "outer ~50, got {}", d(0));
        assert!((d(1) - 25.0).abs() < 0.5, "inner ~25, got {}", d(1));
    }
}

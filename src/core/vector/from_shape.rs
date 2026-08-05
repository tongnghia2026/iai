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

/// Rectangle corner style, CorelDRAW-like. `Round` reproduces the convex
/// quarter-circle of [`rect_path`]; `Scallop` is a concave quarter-arc biting
/// into the corner (a circle centred ON the box vertex); `Chamfer` cuts the
/// corner off with a straight diagonal. Serialized as a small u8 tag so new
/// variants stay additive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RectCorner {
    #[default]
    Round,
    Scallop,
    Chamfer,
}

impl RectCorner {
    pub fn from_u8(v: u8) -> RectCorner {
        match v {
            1 => RectCorner::Scallop,
            2 => RectCorner::Chamfer,
            _ => RectCorner::Round,
        }
    }
    pub fn to_u8(self) -> u8 {
        match self {
            RectCorner::Round => 0,
            RectCorner::Scallop => 1,
            RectCorner::Chamfer => 2,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            RectCorner::Round => "Round",
            RectCorner::Scallop => "Scallop",
            RectCorner::Chamfer => "Chamfer",
        }
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

/// A rectangle with a per-corner `radius` and [`RectCorner`] style (corners in
/// clockwise order: top-left, top-right, bottom-right, bottom-left). Each corner
/// emits a single sharp node when its clamped radius is ~0, otherwise two Cusp
/// nodes joined by the corner arc/cut; straight sides between corners. This is a
/// superset of [`rect_path`] (which stays the uniform-`Round` fast path), used
/// for Scallop/Chamfer and any mixed-corner rectangle.
pub fn rect_path_corners(
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    radii: [f32; 4],
    corners: [RectCorner; 4],
) -> PathData {
    let (lx, rx) = (x0.min(x1), x0.max(x1));
    let (ty, by) = (y0.min(y1), y0.max(y1));
    // Clamp every corner to half the short side. Because each radius is capped at
    // half the short side, two corners sharing an edge sum to at most the short
    // side (≤ that edge), so they can never overlap even with per-corner radii.
    let half_short = (rx - lx).min(by - ty) * 0.5;
    // Corner vertex `v` plus the unit edge directions from `v` toward the arc
    // start `s` and end `e`, clockwise (TL, TR, BR, BL).
    let geom = [
        (Point::new(lx, ty), (0.0f32, 1.0f32), (1.0f32, 0.0f32)),
        (Point::new(rx, ty), (-1.0, 0.0), (0.0, 1.0)),
        (Point::new(rx, by), (0.0, -1.0), (-1.0, 0.0)),
        (Point::new(lx, by), (1.0, 0.0), (0.0, -1.0)),
    ];
    let mut nodes: Vec<Node> = Vec::with_capacity(8);
    for i in 0..4 {
        let (v, a, b) = geom[i];
        let rc = radii[i].max(0.0).min(half_short);
        if rc <= f32::EPSILON {
            nodes.push(Node::sharp(v));
            continue;
        }
        let k = KAPPA * rc;
        let s = Point::new(v.x + rc * a.0, v.y + rc * a.1);
        let e = Point::new(v.x + rc * b.0, v.y + rc * b.1);
        let (s_out, e_in) = match corners[i] {
            // Handles run along the edges toward the vertex → convex quarter circle.
            RectCorner::Round => (
                Some(Point::new(v.x + (rc - k) * a.0, v.y + (rc - k) * a.1)),
                Some(Point::new(v.x + (rc - k) * b.0, v.y + (rc - k) * b.1)),
            ),
            // Handles pushed perpendicular-into the interior → concave quarter arc
            // (a circle centred on the vertex biting into the corner).
            RectCorner::Scallop => (
                Some(Point::new(
                    v.x + rc * a.0 + k * b.0,
                    v.y + rc * a.1 + k * b.1,
                )),
                Some(Point::new(
                    v.x + rc * b.0 + k * a.0,
                    v.y + rc * b.1 + k * a.1,
                )),
            ),
            // No handles → the segment collapses to a straight diagonal cut.
            RectCorner::Chamfer => (None, None),
        };
        nodes.push(corner_node(s, None, s_out));
        nodes.push(corner_node(e, e_in, None));
    }
    PathData::new(vec![Contour::new(nodes, true)], FillRule::NonZero)
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
    fn corners_round_matches_uniform_bounds_and_node_count() {
        // A uniform-Round rect_path_corners is geometrically the same box as
        // rect_path (only the node rotation differs), so bounds/area match.
        let a = rect_path_corners(0.0, 0.0, 100.0, 60.0, [10.0; 4], [RectCorner::Round; 4]);
        let b = rect_path(0.0, 0.0, 100.0, 60.0, 10.0);
        assert_eq!(a.contours[0].nodes.len(), 8);
        assert_eq!(a.control_bounds(), b.control_bounds());
    }

    #[test]
    fn chamfer_cuts_corners_with_straight_diagonals() {
        let p = rect_path_corners(0.0, 0.0, 100.0, 60.0, [10.0; 4], [RectCorner::Chamfer; 4]);
        let c = &p.contours[0];
        assert_eq!(c.nodes.len(), 8);
        // No handles anywhere → every segment is straight, including the cuts.
        assert!(c
            .nodes
            .iter()
            .all(|n| n.in_handle.is_none() && n.out_handle.is_none()));
        // The top-left cut (segment 0: S_TL→E_TL) is the straight diagonal; its
        // midpoint sits off both edges, inside the box, near the corner.
        let (p0, p1, p2, p3) = c.segment(0).unwrap();
        let mid = cubic_bezier(p0, p1, p2, p3, 0.5);
        assert!(
            (mid.x - 5.0).abs() < 1e-3 && (mid.y - 5.0).abs() < 1e-3,
            "{mid:?}"
        );
    }

    #[test]
    fn scallop_bites_a_concave_arc_toward_the_vertex() {
        let r = 12.0f32;
        let p = rect_path_corners(0.0, 0.0, 100.0, 60.0, [r; 4], [RectCorner::Scallop; 4]);
        let c = &p.contours[0];
        assert_eq!(c.nodes.len(), 8);
        // The top-left corner arc (segment 0) is the circle of radius r centred on
        // the box vertex (0,0): its midpoint lies ~r away from the vertex.
        let (p0, p1, p2, p3) = c.segment(0).unwrap();
        let mid = cubic_bezier(p0, p1, p2, p3, 0.5);
        let d = (mid.x * mid.x + mid.y * mid.y).sqrt();
        assert!((d - r).abs() < 0.05, "scallop arc radius {d}, want {r}");
    }

    #[test]
    fn zero_radius_corner_is_a_single_sharp_node() {
        // A mixed rect: TL sharp (r=0), others rounded → 1 + 2·3 = 7 nodes.
        let p = rect_path_corners(
            0.0,
            0.0,
            100.0,
            60.0,
            [0.0, 10.0, 10.0, 10.0],
            [RectCorner::Round; 4],
        );
        assert_eq!(p.contours[0].nodes.len(), 7);
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

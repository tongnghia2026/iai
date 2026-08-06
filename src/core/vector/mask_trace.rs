#![allow(dead_code)]
//! Trace a raster selection mask into vector contours — the bridge that lets a
//! freeform (ellipse / lasso / magic-wand) selection act as a cutter for the
//! vector Trim (see `App::trim_active_vector_by_selection`).
//!
//! A rectangular marquee converts exactly from its bounding box, so it never
//! comes here. Everything else is a per-pixel `u8` coverage mask, which this
//! module turns into one or more closed [`Contour`]s via marching squares:
//!
//!   1. Marching squares over the mask (iso = `threshold`) emits boundary
//!      segments keyed by the grid edge they cross, so adjacent cells share
//!      endpoints EXACTLY (integer edge ids — no float welding).
//!   2. Segments are linked into closed loops (the graph is degree-2 by
//!      construction; saddles are resolved deterministically).
//!   3. Each loop is lightly smoothed (Chaikin) to shed the single-pixel
//!      staircase, then refitted to clean cubic Béziers by [`fit::fit_closed_ring`]
//!      — the same fitter the boolean output uses — so an ellipse cut comes out
//!      curved, not jagged.
//!
//! The result is a polygonal/curved [`PathData`] with the EvenOdd fill rule, so
//! a selection with a hole (annulus) subtracts correctly. UI-free, like the rest
//! of `core::vector`: it takes the raw mask, never the `Selection` type.

use crate::core::geometry::Point;
use crate::core::vector::fit::fit_closed_ring;
use crate::core::vector::path::{Contour, FillRule, Node, PathData};
use std::collections::HashMap;

/// Curve-fit tolerance for a traced loop (canvas units). A touch looser than the
/// boolean's so pixel-scale staircase noise smooths away while the boundary stays
/// within ~1px of the drawn selection.
const TRACE_FIT_TOL: f32 = 1.0;

/// Chaikin corner-cutting passes applied before fitting. Two passes reliably
/// dissolve a 1px binary staircase (hard-edged mask) into a smooth polyline; on
/// an already anti-aliased mask they barely move the boundary. The mild (<0.5px)
/// inward shrink is imperceptible for a cut.
const CHAIKIN_PASSES: usize = 2;

/// A grid-edge id: `(orient, i, j)` where `orient` 0 = horizontal edge between
/// corners `(i,j)`–`(i+1,j)`, 1 = vertical edge between `(i,j)`–`(i,j+1)`. Two
/// cells sharing an edge produce the identical id, so loop linking is exact.
type EdgeKey = (u8, i32, i32);

#[derive(Clone, Copy)]
enum Edge {
    Top,
    Right,
    Bottom,
    Left,
}

/// Trace the thresholded region of `mask` (a `w`×`h` coverage buffer) into vector
/// contours in CANVAS space. `offset` is the mask's canvas-space origin (a mask
/// pixel `(i,j)` is canvas point `(i+offset.0, j+offset.1)`, matching
/// `Selection::is_selected`). `scan` limits the work to `(x0,y0,x1,y1)` in MASK
/// space (typically the selection bbox); it is padded by one cell so a region
/// touching the scan edge still closes. Returns `None` when nothing traces to a
/// usable loop.
pub fn trace_mask_to_path(
    mask: &[u8],
    w: u32,
    h: u32,
    offset: (i32, i32),
    scan: (u32, u32, u32, u32),
    threshold: u8,
) -> Option<PathData> {
    if w == 0 || h == 0 || mask.len() < (w as usize) * (h as usize) {
        return None;
    }
    let wi = w as i32;
    let hi = h as i32;
    let iso = threshold as f32 + 0.5;

    // Sample the coverage field, treating everything outside the mask as 0 so a
    // region touching the border is closed by the surrounding "outside".
    let sample = |i: i32, j: i32| -> f32 {
        if i < 0 || j < 0 || i >= wi || j >= hi {
            0.0
        } else {
            mask[(j as usize) * (w as usize) + (i as usize)] as f32
        }
    };
    let inside = |v: f32| v >= iso;
    // Linear crossing position between two corner values on `[0,1]`.
    let interp = |a: f32, b: f32| -> f32 {
        let d = b - a;
        if d.abs() < 1e-6 {
            0.5
        } else {
            ((iso - a) / d).clamp(0.0, 1.0)
        }
    };

    // Cell range: pad the scan rect by one so border-touching regions close. Cells
    // are indexed by their top-left corner and run one short of the sample grid.
    let (sx0, sy0, sx1, sy1) = scan;
    let cx0 = (sx0 as i32 - 1).max(-1);
    let cy0 = (sy0 as i32 - 1).max(-1);
    let cx1 = (sx1 as i32).min(wi); // exclusive corner upper bound → cells < this
    let cy1 = (sy1 as i32).min(hi);

    let mut point_of: HashMap<EdgeKey, Point> = HashMap::new();
    let mut adj: HashMap<EdgeKey, Vec<EdgeKey>> = HashMap::new();

    let edge_key = |i: i32, j: i32, e: Edge| -> EdgeKey {
        match e {
            Edge::Top => (0, i, j),
            Edge::Bottom => (0, i, j + 1),
            Edge::Left => (1, i, j),
            Edge::Right => (1, i + 1, j),
        }
    };
    let edge_point = |i: i32, j: i32, e: Edge, tl: f32, tr: f32, br: f32, bl: f32| -> Point {
        match e {
            Edge::Top => Point::new(i as f32 + interp(tl, tr), j as f32),
            Edge::Bottom => Point::new(i as f32 + interp(bl, br), (j + 1) as f32),
            Edge::Left => Point::new(i as f32, j as f32 + interp(tl, bl)),
            Edge::Right => Point::new((i + 1) as f32, j as f32 + interp(tr, br)),
        }
    };

    for j in cy0..cy1 {
        for i in cx0..cx1 {
            let tl = sample(i, j);
            let tr = sample(i + 1, j);
            let br = sample(i + 1, j + 1);
            let bl = sample(i, j + 1);
            let case = (inside(tl) as u8)
                | ((inside(tr) as u8) << 1)
                | ((inside(br) as u8) << 2)
                | ((inside(bl) as u8) << 3);

            // Which edge pair(s) the boundary connects in this cell. Saddles (5,
            // 10) are split so each inside corner keeps its own two edges.
            let conns: &[(Edge, Edge)] = match case {
                1 | 14 => &[(Edge::Top, Edge::Left)],
                2 | 13 => &[(Edge::Top, Edge::Right)],
                3 | 12 => &[(Edge::Left, Edge::Right)],
                4 | 11 => &[(Edge::Right, Edge::Bottom)],
                6 | 9 => &[(Edge::Top, Edge::Bottom)],
                7 | 8 => &[(Edge::Left, Edge::Bottom)],
                5 => &[(Edge::Top, Edge::Left), (Edge::Right, Edge::Bottom)],
                10 => &[(Edge::Top, Edge::Right), (Edge::Left, Edge::Bottom)],
                _ => &[], // 0, 15
            };

            for &(e1, e2) in conns {
                let k1 = edge_key(i, j, e1);
                let k2 = edge_key(i, j, e2);
                point_of
                    .entry(k1)
                    .or_insert_with(|| edge_point(i, j, e1, tl, tr, br, bl));
                point_of
                    .entry(k2)
                    .or_insert_with(|| edge_point(i, j, e2, tl, tr, br, bl));
                adj.entry(k1).or_default().push(k2);
                adj.entry(k2).or_default().push(k1);
            }
        }
    }

    if adj.is_empty() {
        return None;
    }

    // Link segments into closed loops. Every node has degree 2, so following the
    // unused neighbour walks one loop; consumed connections are removed from both
    // endpoints. A guard cap prevents any pathological infinite walk.
    let mut contours: Vec<Contour> = Vec::new();
    let keys: Vec<EdgeKey> = adj.keys().copied().collect();
    for start in keys {
        if adj.get(&start).map_or(true, |v| v.is_empty()) {
            continue;
        }
        let mut loop_pts: Vec<Point> = Vec::new();
        let mut cur = start;
        let cap = point_of.len() + 1;
        for _ in 0..cap {
            loop_pts.push(point_of[&cur]);
            // Take one unused neighbour of `cur`.
            let next = match adj.get_mut(&cur).and_then(|v| v.pop()) {
                Some(n) => n,
                None => break,
            };
            // Remove the reciprocal edge so we don't walk back along it.
            if let Some(v) = adj.get_mut(&next) {
                if let Some(pos) = v.iter().position(|&k| k == cur) {
                    v.swap_remove(pos);
                }
            }
            if next == start {
                break;
            }
            cur = next;
        }
        if let Some(contour) = loop_to_contour(&loop_pts, offset) {
            contours.push(contour);
        }
    }

    if contours.is_empty() {
        return None;
    }
    Some(PathData::new(contours, FillRule::EvenOdd))
}

/// Smooth (Chaikin) then curve-fit one raw loop into a closed [`Contour`],
/// translating from mask space to canvas space. Falls back to a sharp-node
/// polygon if the fit is degenerate. `None` for a loop too small to matter.
fn loop_to_contour(pts: &[Point], offset: (i32, i32)) -> Option<Contour> {
    if pts.len() < 3 {
        return None;
    }
    // To canvas space.
    let mut ring: Vec<Point> = pts
        .iter()
        .map(|p| Point::new(p.x + offset.0 as f32, p.y + offset.1 as f32))
        .collect();
    // Drop a closing duplicate if the walk returned to the start.
    if ring.len() >= 2 && ring[0].distance_to(ring[ring.len() - 1]) < 1e-4 {
        ring.pop();
    }
    if ring.len() < 3 {
        return None;
    }
    if ring_area(&ring).abs() < 1.0 {
        return None; // sliver / single-pixel speck
    }
    let smooth = chaikin_closed(&ring, CHAIKIN_PASSES);
    if let Some(c) = fit_closed_ring(&smooth, TRACE_FIT_TOL) {
        return Some(c);
    }
    // Fallback: a plain polygon of sharp nodes (still a valid cutter).
    Some(Contour::new(
        smooth.iter().map(|&p| Node::sharp(p)).collect(),
        true,
    ))
}

/// One or more Chaikin corner-cutting passes on a CLOSED ring (no duplicate
/// endpoint). Each pass replaces every vertex with the 1/4 and 3/4 points of its
/// outgoing edge, rounding off single-pixel staircases.
fn chaikin_closed(pts: &[Point], passes: usize) -> Vec<Point> {
    let mut cur = pts.to_vec();
    for _ in 0..passes {
        let n = cur.len();
        if n < 3 {
            break;
        }
        let mut next = Vec::with_capacity(n * 2);
        for i in 0..n {
            let p = cur[i];
            let q = cur[(i + 1) % n];
            next.push(Point::new(0.75 * p.x + 0.25 * q.x, 0.75 * p.y + 0.25 * q.y));
            next.push(Point::new(0.25 * p.x + 0.75 * q.x, 0.25 * p.y + 0.75 * q.y));
        }
        cur = next;
    }
    cur
}

/// Signed area of a closed ring (shoelace) — used only to reject specks.
fn ring_area(pts: &[Point]) -> f32 {
    let n = pts.len();
    let mut a = 0.0;
    for i in 0..n {
        let p = pts[i];
        let q = pts[(i + 1) % n];
        a += p.x * q.y - q.x * p.y;
    }
    a * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::vector::flatten::flatten_path;

    /// A `w`×`h` mask with a filled disk (hard edge) centred at `(cx,cy)`.
    fn disk_mask(w: u32, h: u32, cx: f32, cy: f32, r: f32) -> Vec<u8> {
        let mut m = vec![0u8; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let dx = x as f32 + 0.5 - cx;
                let dy = y as f32 + 0.5 - cy;
                if dx * dx + dy * dy <= r * r {
                    m[(y * w + x) as usize] = 255;
                }
            }
        }
        m
    }

    /// Even-odd filled area of a PathData via grid sampling of its flatten.
    fn path_area(p: &PathData) -> f32 {
        let polys = flatten_path(p, 0.3);
        if polys.is_empty() {
            return 0.0;
        }
        let (mut minx, mut miny, mut maxx, mut maxy) = (
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        );
        for r in &polys {
            for pt in r {
                minx = minx.min(pt.x);
                miny = miny.min(pt.y);
                maxx = maxx.max(pt.x);
                maxy = maxy.max(pt.y);
            }
        }
        let inside = |x: f32, y: f32| {
            let mut parity = 0i32;
            for r in &polys {
                let n = r.len();
                for i in 0..n {
                    let a = r[i];
                    let b = r[(i + 1) % n];
                    let (lo, hi) = if a.y <= b.y { (a, b) } else { (b, a) };
                    if y >= lo.y && y < hi.y {
                        let t = (y - lo.y) / (hi.y - lo.y);
                        if lo.x + t * (hi.x - lo.x) > x {
                            parity ^= 1;
                        }
                    }
                }
            }
            parity != 0
        };
        let mut area = 0.0;
        let mut y = miny + 0.5;
        while y < maxy {
            let mut x = minx + 0.5;
            while x < maxx {
                if inside(x, y) {
                    area += 1.0;
                }
                x += 1.0;
            }
            y += 1.0;
        }
        area
    }

    #[test]
    fn traces_a_disk_to_one_round_contour() {
        let (w, h) = (120, 120);
        let mask = disk_mask(w, h, 60.0, 60.0, 40.0);
        let path = trace_mask_to_path(&mask, w, h, (0, 0), (0, 0, w, h), 128).expect("traced");
        assert_eq!(path.contours.len(), 1, "a disk is one loop");
        assert!(path.validate().is_ok());
        // Fitted disk stays close to π·40² ≈ 5027 (allow ~5% for pixel + smoothing).
        let area = path_area(&path);
        let expected = std::f32::consts::PI * 40.0 * 40.0;
        assert!(
            (area - expected).abs() / expected < 0.06,
            "disk area {area} vs {expected}"
        );
    }

    #[test]
    fn traces_an_annulus_to_two_contours() {
        // A ring: outer disk minus an inner disk → EvenOdd hole.
        let (w, h) = (140, 140);
        let mut mask = disk_mask(w, h, 70.0, 70.0, 50.0);
        for y in 0..h {
            for x in 0..w {
                let dx = x as f32 + 0.5 - 70.0;
                let dy = y as f32 + 0.5 - 70.0;
                if dx * dx + dy * dy <= 25.0 * 25.0 {
                    mask[(y * w + x) as usize] = 0;
                }
            }
        }
        let path = trace_mask_to_path(&mask, w, h, (0, 0), (0, 0, w, h), 128).expect("traced");
        assert_eq!(path.contours.len(), 2, "annulus is outer + hole");
        // EvenOdd net area ≈ π(50² − 25²) ≈ 5890.
        let area = path_area(&path);
        let expected = std::f32::consts::PI * (50.0 * 50.0 - 25.0 * 25.0);
        assert!(
            (area - expected).abs() / expected < 0.08,
            "annulus area {area} vs {expected}"
        );
    }

    #[test]
    fn region_touching_the_border_still_closes() {
        // A filled rectangle flush against the mask's left/top edges.
        let (w, h) = (60, 60);
        let mut mask = vec![0u8; (w * h) as usize];
        for y in 0..30 {
            for x in 0..30 {
                mask[(y * w + x) as usize] = 255;
            }
        }
        let path = trace_mask_to_path(&mask, w, h, (0, 0), (0, 0, w, h), 128).expect("traced");
        assert_eq!(path.contours.len(), 1);
        let area = path_area(&path);
        assert!(
            (area - 900.0).abs() / 900.0 < 0.08,
            "corner rect area {area}"
        );
    }

    #[test]
    fn empty_mask_traces_nothing() {
        let mask = vec![0u8; 32 * 32];
        assert!(trace_mask_to_path(&mask, 32, 32, (0, 0), (0, 0, 32, 32), 128).is_none());
    }

    #[test]
    fn offset_shifts_into_canvas_space() {
        let (w, h) = (40, 40);
        let mask = disk_mask(w, h, 20.0, 20.0, 12.0);
        let path = trace_mask_to_path(&mask, w, h, (100, 50), (0, 0, w, h), 128).expect("traced");
        // Centre of mass roughly at the mask centre + offset = (120, 70).
        let polys = flatten_path(&path, 0.3);
        let (mut sx, mut sy, mut n) = (0.0f32, 0.0f32, 0.0f32);
        for r in &polys {
            for p in r {
                sx += p.x;
                sy += p.y;
                n += 1.0;
            }
        }
        let (cx, cy) = (sx / n, sy / n);
        assert!(
            (cx - 120.0).abs() < 3.0 && (cy - 70.0).abs() < 3.0,
            "centre ({cx},{cy})"
        );
    }
}

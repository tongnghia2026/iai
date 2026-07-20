#![allow(dead_code)]

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    #[inline]
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub fn distance_to(self, other: Point) -> f32 {
        ((self.x - other.x).powi(2) + (self.y - other.y).powi(2)).sqrt()
    }

    pub fn lerp(self, other: Point, t: f32) -> Point {
        Point::new(
            self.x + (other.x - self.x) * t,
            self.y + (other.y - self.y) * t,
        )
    }
}

impl From<(f32, f32)> for Point {
    fn from((x, y): (f32, f32)) -> Self {
        Self::new(x, y)
    }
}

/// Axis-aligned float rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    #[inline]
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// Build from any two points (auto-sorted).
    pub fn from_points(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        let (lx, rx) = if x0 <= x1 { (x0, x1) } else { (x1, x0) };
        let (ty, by) = if y0 <= y1 { (y0, y1) } else { (y1, y0) };
        Self::new(lx, ty, rx - lx, by - ty)
    }

    pub fn is_empty(self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }

    pub fn contains(self, p: Point) -> bool {
        p.x >= self.x && p.x < self.x + self.w && p.y >= self.y && p.y < self.y + self.h
    }

    /// Union of two rects (bounding box).
    pub fn union(self, other: Rect) -> Rect {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = (self.x + self.w).max(other.x + other.w);
        let y1 = (self.y + self.h).max(other.y + other.h);
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// Intersection of two rects.
    pub fn intersect(self, other: Rect) -> Option<Rect> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = (self.x + self.w).min(other.x + other.w);
        let y1 = (self.y + self.h).min(other.y + other.h);
        if x1 > x0 && y1 > y0 {
            Some(Rect::new(x0, y0, x1 - x0, y1 - y0))
        } else {
            None
        }
    }

    pub fn expand(self, amount: f32) -> Self {
        Rect::new(
            self.x - amount,
            self.y - amount,
            self.w + amount * 2.0,
            self.h + amount * 2.0,
        )
    }

    /// Convert to pixel bounds (x0, y0, x1, y1), clamped to canvas size.
    pub fn to_pixel_bounds(self, canvas_w: u32, canvas_h: u32) -> (u32, u32, u32, u32) {
        let x0 = self.x.max(0.0) as u32;
        let y0 = self.y.max(0.0) as u32;
        let x1 = (self.x + self.w).ceil().clamp(0.0, canvas_w as f32) as u32;
        let y1 = (self.y + self.h).ceil().clamp(0.0, canvas_h as f32) as u32;
        (x0, y0, x1, y1)
    }

    pub fn right(self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(self) -> f32 {
        self.y + self.h
    }
}

/// Pixel-space integer rectangle. Used for dirty regions, layer bounds, GPU uploads.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl PixelRect {
    pub fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }
    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }
    pub fn x1(&self) -> u32 {
        self.x + self.w
    }
    pub fn y1(&self) -> u32 {
        self.y + self.h
    }

    pub fn union(self, other: PixelRect) -> PixelRect {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = self.x1().max(other.x1());
        let y1 = self.y1().max(other.y1());
        PixelRect::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// Clamp the rect to within canvas bounds.
    pub fn clamp_to_canvas(self, cw: u32, ch: u32) -> PixelRect {
        let x0 = self.x.min(cw);
        let y0 = self.y.min(ch);
        let x1 = self.x1().min(cw);
        let y1 = self.y1().min(ch);
        PixelRect::new(x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
    }
}

impl From<(u32, u32, u32, u32)> for PixelRect {
    fn from((x, y, w, h): (u32, u32, u32, u32)) -> Self {
        Self::new(x, y, w, h)
    }
}

/// A 3×3 projective transform (homography), row-major `[a b c; d e f; g h i]`
/// with `i` normalised to 1. Used by the Perspective Crop tool to rectify a
/// dragged quadrilateral back to an axis-aligned rectangle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Homography {
    pub m: [f32; 9],
}

impl Homography {
    /// Build the homography that maps the unit square corners
    /// `(0,0) (1,0) (1,1) (0,1)` onto the quad `[p0, p1, p2, p3]` (same order —
    /// e.g. top-left, top-right, bottom-right, bottom-left). Closed form
    /// (Heckbert): exact for any convex/concave non-degenerate quad. Returns
    /// `None` if the quad is degenerate (collinear / zero-area), where no
    /// homography exists.
    pub fn square_to_quad(quad: [Point; 4]) -> Option<Homography> {
        let [p0, p1, p2, p3] = quad;
        let sx = p0.x - p1.x + p2.x - p3.x;
        let sy = p0.y - p1.y + p2.y - p3.y;

        // Parallelogram → the map is affine (g = h = 0).
        if sx.abs() < 1e-5 && sy.abs() < 1e-5 {
            let m = [
                p1.x - p0.x,
                p3.x - p0.x,
                p0.x,
                p1.y - p0.y,
                p3.y - p0.y,
                p0.y,
                0.0,
                0.0,
                1.0,
            ];
            // Reject a collapsed parallelogram (zero area).
            let area = m[0] * m[4] - m[1] * m[3];
            return if area.abs() < 1e-6 {
                None
            } else {
                Some(Homography { m })
            };
        }

        let dx1 = p1.x - p2.x;
        let dx2 = p3.x - p2.x;
        let dy1 = p1.y - p2.y;
        let dy2 = p3.y - p2.y;
        let denom = dx1 * dy2 - dx2 * dy1;
        if denom.abs() < 1e-9 {
            return None;
        }
        let g = (sx * dy2 - sy * dx2) / denom;
        let h = (dx1 * sy - dy1 * sx) / denom;
        let m = [
            p1.x - p0.x + g * p1.x,
            p3.x - p0.x + h * p3.x,
            p0.x,
            p1.y - p0.y + g * p1.y,
            p3.y - p0.y + h * p3.y,
            p0.y,
            g,
            h,
            1.0,
        ];
        Some(Homography { m })
    }

    /// Map a point `(u, v)` through the homography (with the projective divide).
    #[inline]
    pub fn apply(&self, u: f32, v: f32) -> Point {
        let m = &self.m;
        let w = m[6] * u + m[7] * v + m[8];
        let inv = if w.abs() < 1e-12 { 0.0 } else { 1.0 / w };
        Point::new(
            (m[0] * u + m[1] * v + m[2]) * inv,
            (m[3] * u + m[4] * v + m[5]) * inv,
        )
    }

    /// Return the inverse projective mapping, or `None` for a singular matrix.
    pub fn inverse(&self) -> Option<Homography> {
        let m = self.m;
        let c00 = m[4] * m[8] - m[5] * m[7];
        let c01 = m[2] * m[7] - m[1] * m[8];
        let c02 = m[1] * m[5] - m[2] * m[4];
        let c10 = m[5] * m[6] - m[3] * m[8];
        let c11 = m[0] * m[8] - m[2] * m[6];
        let c12 = m[2] * m[3] - m[0] * m[5];
        let c20 = m[3] * m[7] - m[4] * m[6];
        let c21 = m[1] * m[6] - m[0] * m[7];
        let c22 = m[0] * m[4] - m[1] * m[3];
        let det = m[0] * c00 + m[1] * c10 + m[2] * c20;
        if !det.is_finite() || det.abs() < 1e-9 {
            return None;
        }
        let inv_det = 1.0 / det;
        Some(Homography {
            m: [
                c00 * inv_det,
                c01 * inv_det,
                c02 * inv_det,
                c10 * inv_det,
                c11 * inv_det,
                c12 * inv_det,
                c20 * inv_det,
                c21 * inv_det,
                c22 * inv_det,
            ],
        })
    }
}

/// Evaluate a cubic Bézier at parameter `t ∈ [0,1]` given the four control
/// points. Used by the Pen tool to flatten path segments to polylines.
#[inline]
pub fn cubic_bezier(p0: Point, p1: Point, p2: Point, p3: Point, t: f32) -> Point {
    let u = 1.0 - t;
    let w0 = u * u * u;
    let w1 = 3.0 * u * u * t;
    let w2 = 3.0 * u * t * t;
    let w3 = t * t * t;
    Point::new(
        w0 * p0.x + w1 * p1.x + w2 * p2.x + w3 * p3.x,
        w0 * p0.y + w1 * p1.y + w2 * p2.y + w3 * p3.y,
    )
}

#[cfg(test)]
mod geometry_tests {
    use super::*;

    fn close(a: Point, b: Point) -> bool {
        (a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3
    }

    #[test]
    fn square_to_quad_maps_corners_exactly() {
        let quad = [
            Point::new(10.0, 20.0),
            Point::new(110.0, 5.0),
            Point::new(130.0, 90.0),
            Point::new(0.0, 100.0),
        ];
        let h = Homography::square_to_quad(quad).unwrap();
        assert!(close(h.apply(0.0, 0.0), quad[0]));
        assert!(close(h.apply(1.0, 0.0), quad[1]));
        assert!(close(h.apply(1.0, 1.0), quad[2]));
        assert!(close(h.apply(0.0, 1.0), quad[3]));
    }

    #[test]
    fn square_to_quad_affine_rectangle() {
        // Axis-aligned rectangle (parallelogram path).
        let quad = [
            Point::new(0.0, 0.0),
            Point::new(200.0, 0.0),
            Point::new(200.0, 100.0),
            Point::new(0.0, 100.0),
        ];
        let h = Homography::square_to_quad(quad).unwrap();
        assert!(close(h.apply(0.5, 0.5), Point::new(100.0, 50.0)));
    }

    #[test]
    fn square_to_quad_rejects_degenerate() {
        let collinear = [
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(20.0, 0.0),
            Point::new(30.0, 0.0),
        ];
        assert!(Homography::square_to_quad(collinear).is_none());
    }

    #[test]
    fn homography_inverse_round_trips_points() {
        let h = Homography::square_to_quad([
            Point::new(10.0, 20.0),
            Point::new(120.0, 5.0),
            Point::new(90.0, 130.0),
            Point::new(-15.0, 80.0),
        ])
        .unwrap();
        let inv = h.inverse().unwrap();
        let p = h.apply(0.27, 0.63);
        assert!(close(inv.apply(p.x, p.y), Point::new(0.27, 0.63)));
    }

    #[test]
    fn cubic_bezier_endpoints_and_straight_line() {
        let p0 = Point::new(0.0, 0.0);
        let p3 = Point::new(30.0, 0.0);
        let p1 = Point::new(10.0, 0.0);
        let p2 = Point::new(20.0, 0.0);
        assert!(close(cubic_bezier(p0, p1, p2, p3, 0.0), p0));
        assert!(close(cubic_bezier(p0, p1, p2, p3, 1.0), p3));
        // Evenly-spaced controls on a line → midpoint at t=0.5.
        assert!(close(
            cubic_bezier(p0, p1, p2, p3, 0.5),
            Point::new(15.0, 0.0)
        ));
    }
}

/// Resampling policy for geometric transforms (scale/rotate/free transform).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InterpolationMode {
    Bilinear,
    NearestNeighbor,
}

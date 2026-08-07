#![allow(dead_code)]
//! 2D affine transform for the vector core (foundation task T1.1).
//!
//! Deliberately separate from [`crate::core::geometry::Homography`]: that is a
//! projective 3×3 used only by Perspective Crop, and the plan (Mục 3.4/3.9)
//! forbids reusing it as the general vector transform. Vector objects are
//! affine-only at MVP; projective distortion must rasterize explicitly.
//!
//! Storage is a row-major 2×3 with an implicit `[0 0 1]` bottom row:
//!   x' = a*x + c*y + e
//!   y' = b*x + d*y + f
//! so `(a,b)` is the image of the x-axis, `(c,d)` the image of the y-axis, and
//! `(e,f)` the translation. This matches the cairo/skia matrix convention.

use crate::core::geometry::{Point, Rect};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AffineTransform {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Default for AffineTransform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl AffineTransform {
    pub const IDENTITY: AffineTransform = AffineTransform {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    #[inline]
    pub fn translate(tx: f32, ty: f32) -> Self {
        Self {
            e: tx,
            f: ty,
            ..Self::IDENTITY
        }
    }

    #[inline]
    pub fn scale(sx: f32, sy: f32) -> Self {
        Self {
            a: sx,
            d: sy,
            ..Self::IDENTITY
        }
    }

    /// Rotation by `radians` about the origin.
    #[inline]
    pub fn rotate(radians: f32) -> Self {
        let (s, co) = radians.sin_cos();
        Self {
            a: co,
            b: s,
            c: -s,
            d: co,
            e: 0.0,
            f: 0.0,
        }
    }

    /// True when every coefficient is finite.
    #[inline]
    pub fn is_finite(&self) -> bool {
        self.a.is_finite()
            && self.b.is_finite()
            && self.c.is_finite()
            && self.d.is_finite()
            && self.e.is_finite()
            && self.f.is_finite()
    }

    /// Determinant of the linear part. Zero ⇒ singular (non-invertible).
    #[inline]
    pub fn determinant(&self) -> f32 {
        self.a * self.d - self.b * self.c
    }

    /// `self ∘ other`: the returned transform applies `other` first, then `self`.
    #[must_use]
    pub fn then(&self, other: &AffineTransform) -> AffineTransform {
        // out = self * other  (column-vector convention)
        AffineTransform {
            a: self.a * other.a + self.c * other.b,
            b: self.b * other.a + self.d * other.b,
            c: self.a * other.c + self.c * other.d,
            d: self.b * other.c + self.d * other.d,
            e: self.a * other.e + self.c * other.f + self.e,
            f: self.b * other.e + self.d * other.f + self.f,
        }
    }

    /// Inverse transform, or `None` when singular / non-finite.
    #[must_use]
    pub fn inverse(&self) -> Option<AffineTransform> {
        let det = self.determinant();
        if !det.is_finite() || det.abs() < 1e-12 {
            return None;
        }
        let inv = 1.0 / det;
        let ia = self.d * inv;
        let ib = -self.b * inv;
        let ic = -self.c * inv;
        let id = self.a * inv;
        // translation⁻¹ = -M⁻¹ · t
        let ie = -(ia * self.e + ic * self.f);
        let if_ = -(ib * self.e + id * self.f);
        Some(AffineTransform {
            a: ia,
            b: ib,
            c: ic,
            d: id,
            e: ie,
            f: if_,
        })
    }

    /// Map a position (translation applies).
    #[inline]
    pub fn apply_point(&self, p: Point) -> Point {
        Point::new(
            self.a * p.x + self.c * p.y + self.e,
            self.b * p.x + self.d * p.y + self.f,
        )
    }

    /// Map a direction/offset (translation does NOT apply).
    #[inline]
    pub fn apply_vector(&self, p: Point) -> Point {
        Point::new(self.a * p.x + self.c * p.y, self.b * p.x + self.d * p.y)
    }

    /// Axis-aligned bounding box of `rect` after transforming its four corners.
    pub fn transform_bounds(&self, rect: Rect) -> Rect {
        let corners = [
            self.apply_point(Point::new(rect.x, rect.y)),
            self.apply_point(Point::new(rect.right(), rect.y)),
            self.apply_point(Point::new(rect.right(), rect.bottom())),
            self.apply_point(Point::new(rect.x, rect.bottom())),
        ];
        let mut min_x = corners[0].x;
        let mut min_y = corners[0].y;
        let mut max_x = corners[0].x;
        let mut max_y = corners[0].y;
        for p in &corners[1..] {
            min_x = min_x.min(p.x);
            min_y = min_y.min(p.y);
            max_x = max_x.max(p.x);
            max_y = max_y.max(p.y);
        }
        Rect::new(min_x, min_y, max_x - min_x, max_y - min_y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Point, b: Point) -> bool {
        (a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3
    }

    #[test]
    fn identity_is_a_no_op() {
        let p = Point::new(3.0, -7.0);
        assert!(close(AffineTransform::IDENTITY.apply_point(p), p));
    }

    #[test]
    fn translate_and_scale() {
        let t = AffineTransform::translate(10.0, 20.0);
        assert!(close(
            t.apply_point(Point::new(1.0, 2.0)),
            Point::new(11.0, 22.0)
        ));
        let s = AffineTransform::scale(2.0, 3.0);
        assert!(close(
            s.apply_point(Point::new(4.0, 5.0)),
            Point::new(8.0, 15.0)
        ));
        // Scale ignores translation on a vector, translate leaves a vector be.
        assert!(close(
            t.apply_vector(Point::new(1.0, 2.0)),
            Point::new(1.0, 2.0)
        ));
    }

    #[test]
    fn rotate_ninety_degrees() {
        let r = AffineTransform::rotate(std::f32::consts::FRAC_PI_2);
        // (1,0) → (0,1) under a CCW-in-screen-space rotation with this convention.
        assert!(close(
            r.apply_point(Point::new(1.0, 0.0)),
            Point::new(0.0, 1.0)
        ));
    }

    #[test]
    fn compose_applies_right_first() {
        let scale = AffineTransform::scale(2.0, 2.0);
        let translate = AffineTransform::translate(5.0, 0.0);
        // scale.then(translate): translate first (+5), then scale (×2) ⇒ (1+5)*2 = 12.
        let m = scale.then(&translate);
        assert!(close(
            m.apply_point(Point::new(1.0, 0.0)),
            Point::new(12.0, 0.0)
        ));
        // translate.then(scale): scale first (×2), then translate (+5) ⇒ 1*2+5 = 7.
        let n = translate.then(&scale);
        assert!(close(
            n.apply_point(Point::new(1.0, 0.0)),
            Point::new(7.0, 0.0)
        ));
    }

    #[test]
    fn inverse_round_trips() {
        let m = AffineTransform::translate(30.0, -12.0)
            .then(&AffineTransform::rotate(0.7))
            .then(&AffineTransform::scale(1.5, 0.4));
        let inv = m.inverse().unwrap();
        let p = Point::new(9.0, -4.0);
        assert!(close(inv.apply_point(m.apply_point(p)), p));
    }

    #[test]
    fn singular_transform_has_no_inverse() {
        let flat = AffineTransform::scale(0.0, 1.0);
        assert!(flat.inverse().is_none());
    }

    #[test]
    fn transform_bounds_of_rotated_square() {
        let r = AffineTransform::rotate(std::f32::consts::FRAC_PI_4);
        let b = r.transform_bounds(Rect::new(0.0, 0.0, 2.0, 2.0));
        // A unit-ish square rotated 45° widens to side*√2 ≈ 2.828, centered on origin.
        assert!((b.w - 2.0 * std::f32::consts::SQRT_2).abs() < 1e-2);
        assert!((b.h - 2.0 * std::f32::consts::SQRT_2).abs() < 1e-2);
    }
}

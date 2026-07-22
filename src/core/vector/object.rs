#![allow(dead_code)]
//! What a Path layer holds (foundation, Bước 4 / T4.1).
//!
//! A [`VectorObjectData`] is ONE editable vector object — the MVP rule of one
//! object per layer (Mục 3.3). It bundles the three frozen-foundation pieces:
//! geometry ([`PathData`], contract #1), appearance ([`VectorStyle`], contract
//! #3) and an object-local [`AffineTransform`] (contract #4).
//!
//! Coordinate spaces (Mục 3.4 / 3.13):
//!   - `path` nodes are in OBJECT-LOCAL space.
//!   - `transform` maps object-local → LAYER space.
//!   - `Layer::offset` then places the layer raster on the canvas / PAGE.
//! Coordinates are PAGE-RELATIVE. At MVP there is exactly one implicit page, so
//! page-space == canvas-space and nothing here is tied to a specific canvas size
//! (the geometry is abstract `f32`). A `page_id` can therefore be ADDED later
//! without migrating any object — the whole point of locking the contract now.
//!
//! The raster in `Layer::tiles` is a CACHE derived from this model, never the
//! source of truth (Mục 3.2). See [`crate::core::vector::raster`].

use crate::core::geometry::Rect;
use crate::core::vector::affine::AffineTransform;
use crate::core::vector::path::PathData;
use crate::core::vector::style::VectorStyle;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct VectorObjectData {
    /// Geometry in object-local space.
    pub path: PathData,
    /// Fill / outline / opacity.
    pub style: VectorStyle,
    /// Object-local → layer space. Kept separate from `Layer::offset` so an
    /// affine edit (move/scale/rotate) never has to bake node coordinates.
    pub transform: AffineTransform,
}

impl VectorObjectData {
    pub fn new(path: PathData, style: VectorStyle, transform: AffineTransform) -> Self {
        Self {
            path,
            style,
            transform,
        }
    }

    /// A solid-black-filled object at the identity transform — the freshly-drawn
    /// default (matches [`VectorStyle::default`]).
    pub fn from_path(path: PathData) -> Self {
        Self {
            path,
            style: VectorStyle::default(),
            transform: AffineTransform::IDENTITY,
        }
    }

    /// The path in LAYER space (object transform applied). This is what the
    /// rasteriser, bounds and hit-testing consume; the stored `path` stays in the
    /// stable local frame so an affine edit only touches `transform`.
    pub fn path_in_layer_space(&self) -> PathData {
        let mut p = self.path.clone();
        p.transform(&self.transform);
        p
    }

    /// Reject non-finite geometry/transform and out-of-range style. Reused by the
    /// .iai parser (T4.3) so untrusted files hit the same limits.
    pub fn validate(&self) -> Result<(), String> {
        if !self.transform.is_finite() {
            return Err("vector object transform is non-finite".into());
        }
        self.path.validate()?;
        self.style.validate()?;
        Ok(())
    }

    /// Tight bounds of the outline (fill geometry plus half the stroke width) in
    /// LAYER space, or `None` for an empty path. `tol` is the flatten tolerance.
    pub fn layer_bounds(&self, tol: f32) -> Option<Rect> {
        let lpath = self.path_in_layer_space();
        let core = crate::core::vector::flatten::tight_bounds(&lpath, tol)?;
        let pad = self.style.effective_stroke_width() * 0.5;
        if pad > 0.0 {
            Some(Rect::new(
                core.x - pad,
                core.y - pad,
                core.w + 2.0 * pad,
                core.h + 2.0 * pad,
            ))
        } else {
            Some(core)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::geometry::Point;
    use crate::core::vector::color::ColorValue;
    use crate::core::vector::path::{Contour, FillRule, Node};

    fn square(side: f32) -> PathData {
        PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(side, 0.0)),
                    Node::sharp(Point::new(side, side)),
                    Node::sharp(Point::new(0.0, side)),
                ],
                true,
            )],
            FillRule::NonZero,
        )
    }

    #[test]
    fn transform_maps_into_layer_space() {
        let obj = VectorObjectData::new(
            square(10.0),
            VectorStyle::default(),
            AffineTransform::translate(100.0, 50.0),
        );
        let b = obj.layer_bounds(0.1).unwrap();
        assert!((b.x - 100.0).abs() < 1e-3 && (b.y - 50.0).abs() < 1e-3);
        assert!((b.w - 10.0).abs() < 1e-3 && (b.h - 10.0).abs() < 1e-3);
    }

    #[test]
    fn bounds_include_stroke_pad() {
        let obj = VectorObjectData::new(
            square(10.0),
            VectorStyle::stroked(ColorValue::BLACK, 4.0),
            AffineTransform::IDENTITY,
        );
        let b = obj.layer_bounds(0.1).unwrap();
        // Half the 4px stroke expands each side by 2.
        assert!((b.x - -2.0).abs() < 1e-3 && (b.w - 14.0).abs() < 1e-3);
    }

    #[test]
    fn validate_rejects_non_finite_transform() {
        let mut obj = VectorObjectData::from_path(square(10.0));
        obj.transform.a = f32::NAN;
        assert!(obj.validate().is_err());
        assert!(VectorObjectData::from_path(square(10.0)).validate().is_ok());
    }
}

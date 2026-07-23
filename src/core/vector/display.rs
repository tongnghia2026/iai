//! Screen-resolution raster support for editable Path display.
//!
//! The compositor's document-resolution tile cache deliberately stays nearest-
//! sampled.  This helper produces a separate, bounded raster at a zoom bucket
//! for the active Path only; it never changes document tiles or export output.

use super::{affine::AffineTransform, object::VectorObjectData, raster::PathRaster};

/// Pick the smallest power-of-two display scale that is not below `zoom`.
/// The 16x ceiling covers the editor's 1600% inspection range while the
/// rasterizer's own pixel ceiling remains the final memory guard.
pub fn zoom_bucket(zoom: f32) -> Option<u8> {
    if !zoom.is_finite() || zoom <= 1.0 {
        return None;
    }
    let mut bucket = 2u8;
    while (bucket as f32) < zoom && bucket < 16 {
        bucket *= 2;
    }
    Some(bucket)
}

/// Rasterize `object` in scaled layer coordinates. The returned offset and
/// dimensions are also scaled; callers divide them by `scale` to place the
/// image back in canvas space.
pub fn rasterize_for_display(object: &VectorObjectData, scale: u8) -> Option<PathRaster> {
    if scale <= 1 {
        return None;
    }
    let mut scaled = object.clone();
    scaled.transform = AffineTransform::scale(scale as f32, scale as f32).then(&object.transform);
    // Stroke width is expressed in layer units, so it must follow the display
    // scale just like the transformed geometry.
    scaled.style.stroke_style.width *= scale as f32;
    super::raster::rasterize(&scaled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::geometry::Point;
    use crate::core::vector::path::{Contour, FillRule, Node, PathData};

    fn square(side: f32) -> VectorObjectData {
        VectorObjectData::from_path(PathData::new(
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
        ))
    }

    #[test]
    fn buckets_cover_zoom_without_rebuilding_for_small_changes() {
        assert_eq!(zoom_bucket(1.0), None);
        assert_eq!(zoom_bucket(1.01), Some(2));
        assert_eq!(zoom_bucket(3.9), Some(4));
        assert_eq!(zoom_bucket(4.0), Some(4));
        assert_eq!(zoom_bucket(4.1), Some(8));
        assert_eq!(zoom_bucket(16.0), Some(16));
    }

    #[test]
    fn display_raster_scales_geometry() {
        let base = super::super::raster::rasterize(&square(20.0)).unwrap();
        let hi = rasterize_for_display(&square(20.0), 4).unwrap();
        assert!(hi.width >= base.width * 3);
        assert!(hi.height >= base.height * 3);
        assert!(hi.width <= base.width * 5);
        assert!(hi.height <= base.height * 5);
    }
}

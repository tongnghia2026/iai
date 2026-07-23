//! Screen-resolution raster support for editable Path display.
//!
//! The compositor's document-resolution tile cache deliberately stays nearest-
//! sampled.  This helper produces a separate, bounded raster at a zoom bucket
//! for the active Path only; it never changes document tiles or export output.

use super::{affine::AffineTransform, object::VectorObjectData, raster::PathRaster};

/// Conservative texture edge accepted by older/low-limit adapters. The path
/// display is tiled before egui uploads it, so no individual texture can trip
/// wgpu's `max_texture_dimension_2d` validation.
pub const DISPLAY_TILE_EDGE: u32 = 1024;

pub struct DisplayRasterTile {
    pub rgba: Vec<u8>,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

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

/// Split a tight RGBA raster into upload-safe texture tiles. Tiles contain no
/// document state; they are only the screen-resolution display derivative.
pub fn split_display_tiles(rgba: &[u8], width: u32, height: u32) -> Vec<DisplayRasterTile> {
    if width == 0 || height == 0 || rgba.len() < width as usize * height as usize * 4 {
        return Vec::new();
    }
    let mut tiles = Vec::new();
    for y in (0..height).step_by(DISPLAY_TILE_EDGE as usize) {
        for x in (0..width).step_by(DISPLAY_TILE_EDGE as usize) {
            let tile_w = DISPLAY_TILE_EDGE.min(width - x);
            let tile_h = DISPLAY_TILE_EDGE.min(height - y);
            let mut pixels = vec![0; tile_w as usize * tile_h as usize * 4];
            for row in 0..tile_h {
                let src = ((y + row) as usize * width as usize + x as usize) * 4;
                let dst = row as usize * tile_w as usize * 4;
                let len = tile_w as usize * 4;
                pixels[dst..dst + len].copy_from_slice(&rgba[src..src + len]);
            }
            tiles.push(DisplayRasterTile {
                rgba: pixels,
                x,
                y,
                width: tile_w,
                height: tile_h,
            });
        }
    }
    tiles
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
    fn display_offset_over_scale_is_the_layer_offset_not_double() {
        use crate::core::vector::style::VectorStyle;
        // A path far from the origin. `rasterize_for_display` already bakes the
        // object transform, so its offset ÷ scale IS the canvas top-left — the same
        // place the document-resolution tiles sit (`layer.offset`). The overlay must
        // therefore be placed at `rd.offset / scale` ALONE; adding `layer.offset`
        // double-counts and ghosts a second copy (bug fixed in path_display.rs).
        let path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(40.0, 0.0)),
                    Node::sharp(Point::new(40.0, 40.0)),
                    Node::sharp(Point::new(0.0, 40.0)),
                ],
                true,
            )],
            FillRule::NonZero,
        );
        let obj = VectorObjectData::new(
            path,
            VectorStyle::default(),
            AffineTransform::translate(100.0, 80.0),
        );
        let r = super::super::raster::rasterize(&obj).unwrap();
        let rd = rasterize_for_display(&obj, 4).unwrap();
        let inv = 1.0 / 4.0;
        assert!(
            (rd.offset.0 as f32 * inv - r.offset.0 as f32).abs() <= 1.0
                && (rd.offset.1 as f32 * inv - r.offset.1 as f32).abs() <= 1.0,
            "display offset/scale {:?} must map back onto the layer offset {:?}",
            (rd.offset.0 as f32 * inv, rd.offset.1 as f32 * inv),
            r.offset,
        );
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

    #[test]
    fn oversized_display_raster_is_split_below_gpu_limit() {
        // Exact dimensions from the reported crash: one 2247x2256 upload used
        // to exceed a 2048px adapter limit.
        let (w, h) = (2247, 2256);
        let rgba = vec![17; w * h * 4];
        let tiles = split_display_tiles(&rgba, w as u32, h as u32);
        assert_eq!(tiles.len(), 9);
        assert!(tiles
            .iter()
            .all(|t| t.width <= DISPLAY_TILE_EDGE && t.height <= DISPLAY_TILE_EDGE));
        let covered: u64 = tiles.iter().map(|t| t.width as u64 * t.height as u64).sum();
        assert_eq!(covered, w as u64 * h as u64);
        let bottom_right = tiles.last().unwrap();
        assert_eq!(
            (bottom_right.x, bottom_right.y),
            (2 * DISPLAY_TILE_EDGE, 2 * DISPLAY_TILE_EDGE)
        );
    }
}

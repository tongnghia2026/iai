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
/// The editor permits 6400%, so the display derivative must cover 64x too.
/// High buckets are viewport-clipped by [`rasterize_for_display_clipped`].
pub fn zoom_bucket(zoom: f32) -> Option<u8> {
    if !zoom.is_finite() || zoom <= 1.0 {
        return None;
    }
    let mut bucket = 2u8;
    while (bucket as f32) < zoom && bucket < 64 {
        bucket *= 2;
    }
    Some(bucket)
}

/// Rasterize `object` in scaled layer coordinates. The returned offset and
/// dimensions are also scaled; callers divide them by `scale` to place the
/// image back in canvas space.
pub fn rasterize_for_display(object: &VectorObjectData, scale: u8) -> Option<PathRaster> {
    rasterize_for_display_inner(object, scale, None)
}

pub fn rasterize_for_display_clipped(
    object: &VectorObjectData,
    scale: u8,
    clip: crate::core::geometry::Rect,
) -> Option<PathRaster> {
    rasterize_for_display_inner(object, scale, Some(clip))
}

/// Rasterize and composite a bottom-to-top run of vector objects into one
/// screen-resolution overlay. A single combined image preserves their z-order
/// while allowing the compositor to hide every coarse document-resolution twin.
pub fn rasterize_stack_for_display_clipped(
    objects: &[VectorObjectData],
    scale: u8,
    clip: crate::core::geometry::Rect,
) -> Option<PathRaster> {
    let rasters = objects
        .iter()
        .filter_map(|object| rasterize_for_display_clipped(object, scale, clip))
        .collect::<Vec<_>>();
    let first = rasters.first()?;
    let mut x0 = first.offset.0;
    let mut y0 = first.offset.1;
    let mut x1 = x0.saturating_add(first.width as i32);
    let mut y1 = y0.saturating_add(first.height as i32);
    for raster in &rasters[1..] {
        x0 = x0.min(raster.offset.0);
        y0 = y0.min(raster.offset.1);
        x1 = x1.max(raster.offset.0.saturating_add(raster.width as i32));
        y1 = y1.max(raster.offset.1.saturating_add(raster.height as i32));
    }
    let width = x1.saturating_sub(x0) as u32;
    let height = y1.saturating_sub(y0) as u32;
    let pixels = (width as usize).checked_mul(height as usize)?;
    if pixels > 64_000_000 {
        return None;
    }
    let mut rgba = vec![0u8; pixels.checked_mul(4)?];
    for raster in rasters {
        let dx = raster.offset.0.saturating_sub(x0) as usize;
        let dy = raster.offset.1.saturating_sub(y0) as usize;
        for row in 0..raster.height as usize {
            for col in 0..raster.width as usize {
                let src_index = (row * raster.width as usize + col) * 4;
                let dst_index = ((dy + row) * width as usize + dx + col) * 4;
                let src = &raster.rgba[src_index..src_index + 4];
                let sa = src[3] as f32 / 255.0;
                if sa <= 0.0 {
                    continue;
                }
                let dst = &mut rgba[dst_index..dst_index + 4];
                let da = dst[3] as f32 / 255.0;
                let out_a = sa + da * (1.0 - sa);
                for channel in 0..3 {
                    let premultiplied =
                        src[channel] as f32 * sa + dst[channel] as f32 * da * (1.0 - sa);
                    dst[channel] =
                        (premultiplied / out_a.max(1e-6)).round().clamp(0.0, 255.0) as u8;
                }
                dst[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    Some(PathRaster {
        rgba,
        width,
        height,
        offset: (x0, y0),
    })
}

fn rasterize_for_display_inner(
    object: &VectorObjectData,
    scale: u8,
    clip: Option<crate::core::geometry::Rect>,
) -> Option<PathRaster> {
    if scale <= 1 {
        return None;
    }
    let mut scaled = object.clone();
    let factor = scale as f32;
    scaled.transform = AffineTransform::scale(factor, factor).then(&object.transform);
    // Stroke width is expressed in layer units, so it must follow the display
    // scale just like the transformed geometry.
    scaled.style.stroke_style.width *= factor;
    let dash_len = scaled
        .style
        .stroke_style
        .dash
        .len
        .min(crate::core::vector::style::MAX_DASHES as u8) as usize;
    for value in &mut scaled.style.stroke_style.dash.values[..dash_len] {
        *value *= factor;
    }
    scaled.style.stroke_style.dash.offset *= factor;
    match clip {
        Some(clip) => {
            let scaled_clip = crate::core::geometry::Rect::new(
                clip.x * factor,
                clip.y * factor,
                clip.w * factor,
                clip.h * factor,
            );
            super::raster::rasterize_clipped(&scaled, scaled_clip)
        }
        None => super::raster::rasterize(&scaled),
    }
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
        assert_eq!(zoom_bucket(16.1), Some(32));
        assert_eq!(zoom_bucket(64.0), Some(64));
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
    fn maximum_zoom_rasterizes_only_the_visible_clip() {
        let hi = rasterize_for_display_clipped(
            &square(2_000.0),
            64,
            crate::core::geometry::Rect::new(900.0, 700.0, 40.0, 30.0),
        )
        .expect("visible portion");
        assert!(hi.width <= 40 * 64 + 2);
        assert!(hi.height <= 30 * 64 + 2);
        assert!(hi.width > 2_000 && hi.height > 1_500);
    }

    #[test]
    fn visible_vector_run_composites_bottom_to_top() {
        use crate::core::vector::color::ColorValue;
        use crate::core::vector::style::VectorStyle;
        let mut bottom = square(20.0);
        bottom.style = VectorStyle::filled(ColorValue::rgb(1.0, 0.0, 0.0));
        bottom.transform = AffineTransform::translate(5.0, 5.0);
        let mut top = square(20.0);
        top.style = VectorStyle::filled(ColorValue::rgb(0.0, 0.0, 1.0));
        top.transform = AffineTransform::translate(15.0, 5.0);
        let raster = rasterize_stack_for_display_clipped(
            &[bottom, top],
            4,
            crate::core::geometry::Rect::new(0.0, 0.0, 40.0, 40.0),
        )
        .expect("combined display");
        let sample = |canvas_x: i32, canvas_y: i32| {
            let x = canvas_x * 4 - raster.offset.0;
            let y = canvas_y * 4 - raster.offset.1;
            let index = (y as usize * raster.width as usize + x as usize) * 4;
            [
                raster.rgba[index],
                raster.rgba[index + 1],
                raster.rgba[index + 2],
                raster.rgba[index + 3],
            ]
        };
        assert_eq!(sample(10, 10), [255, 0, 0, 255]);
        assert_eq!(sample(20, 10), [0, 0, 255, 255]);
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

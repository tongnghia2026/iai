//! The dialog's "Tô vùng" brush: edits one face's skin or hair mask with the
//! Refine Brush's own stamps — Add, Subtract, Smart (colour-aware matting that
//! follows hair and skin edges) and, with Alt on Smart, putting the analysis
//! mask back.

use crate::core::refine::{restore_stamp, Rect, StampOp};
use crate::core::selection::{compute_sobel, pixels_to_lab, refine_edge_stamp, RefineBrushMode};

use super::geometry::Region;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MaskTarget {
    Skin,
    Hair,
}

/// A mask being painted, in its own region of the analysed image.
pub struct MaskPaint {
    pub region: Region,
    pub mask: Vec<u8>,
    /// The analysis's mask, for Alt + Smart.
    original: Vec<u8>,
    /// Lab colour and edge strength of the region, built on the first Smart dab.
    edges: Option<(Vec<[f32; 3]>, Vec<f32>)>,
}

impl MaskPaint {
    pub fn new(region: Region, mask: Vec<u8>, original: Vec<u8>) -> Self {
        Self {
            region,
            mask,
            original,
            edges: None,
        }
    }

    /// Whether image point (x, y) lies in the region.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        let r = self.region;
        x >= r.x as f32 && y >= r.y as f32 && x < (r.x + r.w) as f32 && y < (r.y + r.h) as f32
    }

    /// Stamp one dab at image point (x, y); `rgba` is the analysed image of
    /// `width` pixels a row. Returns the touched area in region pixels.
    #[allow(clippy::too_many_arguments)]
    pub fn stamp(
        &mut self,
        rgba: &[u8],
        width: u32,
        op: StampOp,
        x: f32,
        y: f32,
        radius: f32,
        hardness: f32,
    ) -> Option<Rect> {
        let (w, h) = (self.region.w as usize, self.region.h as usize);
        let (cx, cy) = (x - self.region.x as f32, y - self.region.y as f32);
        let touched = Rect::around(cx, cy, radius + 1.0, w, h)?;
        match op {
            StampOp::Smart => {
                let region = self.region;
                let (lab, sobel) = self
                    .edges
                    .get_or_insert_with(|| edges_of(rgba, width, region));
                refine_edge_stamp(
                    lab,
                    sobel,
                    &mut self.mask,
                    w as u32,
                    h as u32,
                    cx,
                    cy,
                    radius,
                    hardness,
                    RefineBrushMode::Smart,
                );
            }
            StampOp::Add | StampOp::Subtract => {
                let mode = if op == StampOp::Add {
                    RefineBrushMode::Add
                } else {
                    RefineBrushMode::Subtract
                };
                refine_edge_stamp(
                    &[],
                    &[],
                    &mut self.mask,
                    w as u32,
                    h as u32,
                    cx,
                    cy,
                    radius,
                    hardness,
                    mode,
                );
            }
            StampOp::Restore => {
                restore_stamp(
                    &mut self.mask,
                    &self.original,
                    w,
                    h,
                    cx,
                    cy,
                    radius,
                    hardness,
                );
            }
        }
        Some(touched)
    }

    /// The mask inside `rect` (region pixels), row by row.
    pub fn read(&self, rect: Rect) -> Vec<u8> {
        let w = self.region.w as usize;
        (rect.y0..rect.y1)
            .flat_map(|y| self.mask[y * w + rect.x0..y * w + rect.x1].iter().copied())
            .collect()
    }

    /// Put `data` (from [`read`](Self::read)) back inside `rect`.
    pub fn write(&mut self, rect: Rect, data: &[u8]) {
        let (w, rw) = (self.region.w as usize, rect.width());
        for (row, y) in (rect.y0..rect.y1).enumerate() {
            self.mask[y * w + rect.x0..y * w + rect.x1]
                .copy_from_slice(&data[row * rw..(row + 1) * rw]);
        }
    }
}

/// Lab colour and edge strength of image `region`.
fn edges_of(rgba: &[u8], width: u32, r: Region) -> (Vec<[f32; 3]>, Vec<f32>) {
    let row = r.w as usize * 4;
    let mut pixels = vec![0u8; row * r.h as usize];
    for (y, line) in pixels.chunks_exact_mut(row).enumerate() {
        let o = ((r.y as usize + y) * width as usize + r.x as usize) * 4;
        line.copy_from_slice(&rgba[o..o + row]);
    }
    (
        pixels_to_lab(&pixels, r.w, r.h),
        compute_sobel(&pixels, r.w, r.h),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paint() -> (MaskPaint, Vec<u8>) {
        let region = Region {
            x: 10,
            y: 20,
            w: 40,
            h: 30,
        };
        let width = 64u32;
        let rgba = vec![128u8; width as usize * 64 * 4];
        let original = vec![0u8; 40 * 30];
        (MaskPaint::new(region, original.clone(), original), rgba)
    }

    #[test]
    fn add_then_subtract_and_restore_in_image_coordinates() {
        let (mut p, rgba) = paint();
        let at = (30.0, 35.0);
        let rect = p
            .stamp(&rgba, 64, StampOp::Add, at.0, at.1, 4.0, 1.0)
            .unwrap();
        assert_eq!(
            p.mask[15 * 40 + 20],
            255,
            "centre of the dab, region pixels"
        );
        assert!(rect.x0 <= 20 && rect.x1 > 20 && rect.y0 <= 15 && rect.y1 > 15);
        let before = p.read(rect);
        p.stamp(&rgba, 64, StampOp::Subtract, at.0, at.1, 4.0, 1.0);
        assert_eq!(p.mask[15 * 40 + 20], 0);
        p.write(rect, &before);
        assert_eq!(p.mask[15 * 40 + 20], 255);
        p.stamp(&rgba, 64, StampOp::Restore, at.0, at.1, 4.0, 1.0);
        assert_eq!(
            p.mask[15 * 40 + 20],
            0,
            "Alt + Smart puts the analysis back"
        );
        assert!(!p.contains(5.0, 25.0) && p.contains(11.0, 21.0));
        assert!(p
            .stamp(&rgba, 64, StampOp::Add, -50.0, -50.0, 4.0, 1.0)
            .is_none());
    }
}

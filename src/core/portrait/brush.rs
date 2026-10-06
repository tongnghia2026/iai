//! The dialog's "Tô vùng" brush: edits one face's skin, hair, brow or
//! clothes mask.
//! Add and Subtract paint plainly; Smart grades each pixel by colour against
//! the area being painted and the rest around the brush
//! (`core::smart_brush`).

use crate::core::refine::{Rect, StampOp};
use crate::core::selection::rgb_to_lab;
use crate::core::smart_brush::{self, Colours};

use super::geometry::Region;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MaskTarget {
    Skin,
    Hair,
    Brows,
    Clothes,
}

/// A mask being painted, in its own region of the analysed image.
pub struct MaskPaint {
    pub region: Region,
    pub mask: Vec<u8>,
    /// The mask when the current stroke began: Smart samples it and builds
    /// on it, so its own dabs neither feed its samples nor stack up.
    base: Option<Vec<u8>>,
}

/// The analysed image (`width` pixels a row) under a mask region.
struct RegionColours<'a> {
    rgba: &'a [u8],
    width: usize,
    region: Region,
}

impl RegionColours<'_> {
    fn rgb(&self, i: usize) -> [u8; 3] {
        let w = self.region.w as usize;
        let o =
            ((self.region.y as usize + i / w) * self.width + self.region.x as usize + i % w) * 4;
        [self.rgba[o], self.rgba[o + 1], self.rgba[o + 2]]
    }
}

impl Colours for RegionColours<'_> {
    fn lab(&self, i: usize) -> [f32; 3] {
        let [r, g, b] = self.rgb(i);
        rgb_to_lab(r, g, b)
    }

    fn tone(&self, i: usize) -> f32 {
        let [r, g, b] = self.rgb(i);
        0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32
    }
}

impl MaskPaint {
    pub fn new(region: Region, mask: Vec<u8>) -> Self {
        Self {
            region,
            mask,
            base: None,
        }
    }

    pub fn begin_stroke(&mut self) {
        self.base = Some(self.mask.clone());
    }

    pub fn end_stroke(&mut self) {
        self.base = None;
    }

    /// Stamp one dab at image point (x, y); `rgba` is the analysed image of
    /// `width` pixels a row. Returns the touched area in region pixels. Smart
    /// (and SmartOut, Alt) is the shared [`smart_brush`].
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
        let r = self.region;
        let image = RegionColours {
            rgba,
            width: width as usize,
            region: r,
        };
        let (w, h) = (r.w as usize, r.h as usize);
        let (cx, cy) = (x - r.x as f32, y - r.y as f32);
        match &self.base {
            Some(base) => smart_brush::stamp(
                &mut self.mask,
                base,
                w,
                h,
                &image,
                op,
                cx,
                cy,
                radius,
                hardness,
            ),
            None => {
                let start = self.mask.clone();
                smart_brush::stamp(
                    &mut self.mask,
                    &start,
                    w,
                    h,
                    &image,
                    op,
                    cx,
                    cy,
                    radius,
                    hardness,
                )
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A 60 x 40 image: dark "hair" on the left half, light "wall" on the
    /// right, and a faint strand (a quarter hair) down column 45.
    fn photo() -> (Vec<u8>, u32, MaskPaint) {
        let (w, h) = (60u32, 40u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let v: u8 = if x < 30 {
                    40
                } else if x == 45 {
                    175
                } else {
                    220
                };
                let o = ((y * w + x) * 4) as usize;
                rgba[o..o + 4].copy_from_slice(&[v, v - v / 8, v - v / 4, 255]);
            }
        }
        let region = Region { x: 0, y: 0, w, h };
        let mask = (0..w * h)
            .map(|i| if i % w < 30 { 255 } else { 0 })
            .collect();
        (rgba, w, MaskPaint::new(region, mask))
    }

    #[test]
    fn add_subtract_and_undo_patches_in_image_coordinates() {
        let (rgba, width, mut p) = photo();
        let rect = p
            .stamp(&rgba, width, StampOp::Add, 50.0, 20.0, 4.0, 1.0)
            .unwrap();
        assert_eq!(p.mask[20 * 60 + 50], 255);
        let before = p.read(rect);
        p.stamp(&rgba, width, StampOp::Subtract, 50.0, 20.0, 4.0, 1.0);
        assert_eq!(p.mask[20 * 60 + 50], 0);
        p.write(rect, &before);
        assert_eq!(p.mask[20 * 60 + 50], 255);
        assert!(p
            .stamp(&rgba, width, StampOp::Add, -50.0, -50.0, 4.0, 1.0)
            .is_none());
    }

    #[test]
    fn smart_takes_a_faint_strand_faintly_and_leaves_the_wall() {
        let (rgba, width, mut p) = photo();
        p.begin_stroke();
        p.stamp(&rgba, width, StampOp::Smart, 45.0, 20.0, 8.0, 1.0);
        p.end_stroke();
        let (strand, wall) = (p.mask[20 * 60 + 45], p.mask[20 * 60 + 41]);
        assert!(strand > 40 && strand < 200, "faint strand {strand}");
        assert_eq!(wall, 0);
        // Painting again strengthens it; the wall stays clear.
        p.begin_stroke();
        p.stamp(&rgba, width, StampOp::Smart, 45.0, 20.0, 8.0, 1.0);
        p.end_stroke();
        assert!(p.mask[20 * 60 + 45] > strand);
        assert_eq!(p.mask[20 * 60 + 41], 0);
        // Alt + Smart takes out what looks like the wall, not the hair.
        p.mask[20 * 60 + 41] = 200;
        p.begin_stroke();
        p.stamp(&rgba, width, StampOp::SmartOut, 38.0, 20.0, 10.0, 1.0);
        p.end_stroke();
        assert!(p.mask[20 * 60 + 41] < 20);
        assert_eq!(p.mask[20 * 60 + 29], 255);
    }

    /// Opt-in: IAI_BRUSH_PROBE is a folder with photos and `strokes.txt`
    /// (`name.jpg radius x,y x,y …` per line). Each stroke paints Smart over
    /// the analysed hair mask, once and twice; writes overlay sheets.
    #[test]
    #[ignore]
    fn probe_brush() {
        let Ok(dir) = std::env::var("IAI_BRUSH_PROBE") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let list = std::fs::read_to_string(dir.join("strokes.txt")).unwrap();
        let mut cache: Option<(String, Vec<u8>, u32, super::super::PortraitModel)> = None;
        for (n, line) in list.lines().filter(|l| !l.trim().is_empty()).enumerate() {
            let mut parts = line.split_whitespace();
            let name = parts.next().unwrap().to_string();
            let radius: f32 = parts.next().unwrap().parse().unwrap();
            let path: Vec<(f32, f32)> = parts
                .map(|p| {
                    let (x, y) = p.split_once(',').unwrap();
                    (x.parse().unwrap(), y.parse().unwrap())
                })
                .collect();
            if cache.as_ref().map(|c| &c.0) != Some(&name) {
                let image = image::open(dir.join(&name)).unwrap().to_rgba8();
                let (width, height) = image.dimensions();
                let rgba = image.into_raw();
                let model =
                    super::super::analyze(&rgba, width, height, false, None, &|_| {}).unwrap();
                cache = Some((name.clone(), rgba, width, model));
            }
            let (_, rgba, width, model) = cache.as_ref().unwrap();
            let face = &model.faces[0];
            let mut dabs = vec![path[0]];
            for pair in path.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                let steps = ((b.0 - a.0).hypot(b.1 - a.1) / (radius * 0.35))
                    .ceil()
                    .max(1.0) as usize;
                for i in 1..=steps {
                    let t = i as f32 / steps as f32;
                    dabs.push((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t));
                }
            }
            let mut p = MaskPaint::new(face.hair_region, face.hair.clone());
            let mut passes = Vec::new();
            for _ in 0..2 {
                p.begin_stroke();
                for &(x, y) in &dabs {
                    p.stamp(rgba, *width, StampOp::Smart, x, y, radius, 0.5);
                }
                p.end_stroke();
                passes.push(p.mask.clone());
            }
            let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for &(x, y) in &path {
                x0 = x0.min(x - radius * 1.6);
                y0 = y0.min(y - radius * 1.6);
                x1 = x1.max(x + radius * 1.6);
                y1 = y1.max(y + radius * 1.6);
            }
            let hr = face.hair_region;
            let (x0, y0) = (x0.max(hr.x as f32) as u32, y0.max(hr.y as f32) as u32);
            let (x1, y1) = ((x1 as u32).min(hr.x + hr.w), (y1 as u32).min(hr.y + hr.h));
            let (cw, ch) = (x1 - x0, y1 - y0);
            let tile = |mask: Option<&[u8]>| {
                image::RgbImage::from_fn(cw, ch, |x, y| {
                    let (ix, iy) = (x0 + x, y0 + y);
                    let o = ((iy * width + ix) * 4) as usize;
                    let c = [rgba[o] as f32, rgba[o + 1] as f32, rgba[o + 2] as f32];
                    let m = mask.map_or(0.0, |m| {
                        m[((iy - hr.y) * hr.w + ix - hr.x) as usize] as f32 / 255.0 * 0.75
                    });
                    let tint = [150.0, 60.0, 255.0];
                    image::Rgb(std::array::from_fn(|k| {
                        (c[k] * (1.0 - m) + tint[k] * m) as u8
                    }))
                })
            };
            let tiles = [
                tile(None),
                tile(Some(&face.hair)),
                tile(Some(&passes[0])),
                tile(Some(&passes[1])),
            ];
            let mut sheet =
                image::RgbImage::from_pixel(cw * 4 + 18, ch, image::Rgb([255, 255, 255]));
            for (k, t) in tiles.iter().enumerate() {
                image::imageops::replace(&mut sheet, t, ((cw + 6) * k as u32) as i64, 0);
            }
            for (k, m) in passes.iter().enumerate() {
                let added: f64 = m
                    .iter()
                    .zip(&face.hair)
                    .map(|(&a, &b)| (a as f64 - b as f64).max(0.0) / 255.0)
                    .sum();
                println!("{name} stroke {n} pass {}: adds {added:.0} px", k + 1);
            }
            sheet
                .save(dir.join(format!("pb_{n}_{}.png", name.trim_end_matches(".jpg"))))
                .unwrap();
        }
    }
}

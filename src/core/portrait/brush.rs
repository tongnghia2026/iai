//! The dialog's "Tô vùng" brush: edits one face's skin or hair mask. Add and
//! Subtract paint plainly; Smart grades each pixel by colour against the area
//! being painted and the rest around the brush (see [`MaskPaint::stamp`]).

use rayon::prelude::*;

use crate::core::refine::{Rect, StampOp};
use crate::core::selection::{lab_dist, rgb_to_lab};

use super::geometry::Region;

/// Colour samples taken from each side around a dab.
const MAX_SAMPLES: usize = 96;
/// Lab distance under which a sample could be either side.
const AMBIGUOUS: f32 = 10.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MaskTarget {
    Skin,
    Hair,
}

/// A mask being painted, in its own region of the analysed image.
pub struct MaskPaint {
    pub region: Region,
    pub mask: Vec<u8>,
    /// The mask when the current stroke began: Smart samples it and builds
    /// on it, so its own dabs neither feed its samples nor stack up.
    base: Option<Vec<u8>>,
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
    /// `width` pixels a row. Returns the touched area in region pixels.
    ///
    /// Smart is Select ▸ Color Range graded against both sides: colours are
    /// sampled around the brush from the area being painted (mask near full)
    /// and from the rest (mask near empty, flat pixels only, so a loose strand
    /// is not taken for backdrop), samples the two share are dropped, and each
    /// pixel under the brush takes its share of the painted side by Lab
    /// distance to the nearest sample of each. A faint strand comes in faint,
    /// the backdrop stays out, and colours like neither side (skin beside
    /// hair) stay out too. It only adds, on top of the mask the stroke began
    /// with, so painting again strengthens faint strands; Restore (Alt) takes
    /// out what resembles the rest instead.
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
        let (w, h) = (r.w as usize, r.h as usize);
        let (cx, cy) = (x - r.x as f32, y - r.y as f32);
        let touched = Rect::around(cx, cy, radius + 1.0, w, h)?;
        let mut disc = Vec::new();
        for py in touched.y0..touched.y1 {
            for px in touched.x0..touched.x1 {
                let d = (px as f32 + 0.5 - cx).hypot(py as f32 + 0.5 - cy);
                if d <= radius {
                    disc.push((py * w + px, falloff(d, radius, hardness)));
                }
            }
        }
        let grade: Vec<f32> = match op {
            StampOp::Add | StampOp::Subtract => vec![1.0; disc.len()],
            StampOp::Smart | StampOp::Restore => {
                self.smart_grade(rgba, width, op == StampOp::Restore, cx, cy, radius, &disc)
            }
        };
        let base = self.base.as_deref().unwrap_or(&self.mask);
        let fresh: Vec<u8> = disc
            .iter()
            .zip(&grade)
            .map(|(&(i, weight), &k)| {
                let (m, start) = (self.mask[i] as f32 / 255.0, base[i] as f32 / 255.0);
                let k = k * weight;
                let v = match op {
                    StampOp::Add => m.max(k),
                    StampOp::Smart => m.max(start + (1.0 - start) * k),
                    StampOp::Subtract | StampOp::Restore => m * (1.0 - k),
                };
                (v * 255.0).round() as u8
            })
            .collect();
        for ((i, _), v) in disc.into_iter().zip(fresh) {
            self.mask[i] = v;
        }
        Some(touched)
    }

    /// Smart's share (0..1) of the painted side for each `disc` pixel, or of
    /// the rest with `remove`.
    #[allow(clippy::too_many_arguments)]
    fn smart_grade(
        &self,
        rgba: &[u8],
        width: u32,
        remove: bool,
        cx: f32,
        cy: f32,
        radius: f32,
        disc: &[(usize, f32)],
    ) -> Vec<f32> {
        let r = self.region;
        let (w, h) = (r.w as usize, r.h as usize);
        let rgb = |i: usize| {
            let o = ((r.y as usize + i / w) * width as usize + r.x as usize + i % w) * 4;
            [rgba[o], rgba[o + 1], rgba[o + 2]]
        };
        let lab = |i: usize| {
            let [cr, cg, cb] = rgb(i);
            rgb_to_lab(cr, cg, cb)
        };
        let luma = |i: usize| {
            let [cr, cg, cb] = rgb(i);
            0.299 * cr as f32 + 0.587 * cg as f32 + 0.114 * cb as f32
        };
        let mask = self.base.as_deref().unwrap_or(&self.mask);

        let reach = radius * 2.5;
        let (mut fg, mut bg) = (Vec::new(), Vec::new());
        if let Some(ring) = Rect::around(cx, cy, reach, w, h) {
            let area = ring.width() * ring.height();
            let stride = ((area as f32 / (8 * MAX_SAMPLES) as f32).sqrt().ceil() as usize).max(1);
            for py in (ring.y0..ring.y1).step_by(stride) {
                for px in (ring.x0..ring.x1).step_by(stride) {
                    if (px as f32 + 0.5 - cx).hypot(py as f32 + 0.5 - cy) > reach {
                        continue;
                    }
                    let i = py * w + px;
                    if mask[i] >= 230 {
                        fg.push(lab(i));
                    } else if mask[i] <= 25 && px > 0 && py > 0 && px + 1 < w && py + 1 < h {
                        // Largest step to a neighbour: a strand one pixel wide
                        // stands out from both sides.
                        let l = luma(i);
                        let step = [i - 1, i + 1, i - w, i + w]
                            .iter()
                            .map(|&j| (luma(j) - l).abs())
                            .fold(0.0, f32::max);
                        bg.push((step, lab(i)));
                    }
                }
            }
        }
        bg.sort_by(|a, b| a.0.total_cmp(&b.0));
        bg.truncate((bg.len() * 6).div_ceil(10));
        let thin = |v: Vec<[f32; 3]>| -> Vec<[f32; 3]> {
            let step = v.len().div_ceil(MAX_SAMPLES).max(1);
            v.into_iter().step_by(step).collect()
        };
        let nearest = |set: &[[f32; 3]], c: [f32; 3]| {
            set.iter().map(|&s| lab_dist(s, c)).fold(f32::MAX, f32::min)
        };
        // Backdrop showing between strands inside the mask, a strand lying in
        // the backdrop: samples either side could hold.
        let keep = |own: Vec<[f32; 3]>, other: &[[f32; 3]]| -> Vec<[f32; 3]> {
            let kept: Vec<[f32; 3]> = own
                .iter()
                .copied()
                .filter(|&c| nearest(other, c) > AMBIGUOUS)
                .collect();
            if kept.is_empty() {
                own
            } else {
                kept
            }
        };
        let (fg, bg) = (thin(fg), thin(bg.into_iter().map(|(_, c)| c).collect()));
        let fg = keep(fg, &bg);
        let bg = keep(bg, &fg);
        // How far the painted side's colours typically sit from the rest.
        let span = if fg.is_empty() || bg.is_empty() {
            0.0
        } else {
            let mut d: Vec<f32> = fg.iter().map(|&c| nearest(&bg, c)).collect();
            d.sort_by(f32::total_cmp);
            d[d.len() / 2]
        };
        disc.par_iter()
            .map(|&(i, _)| {
                let c = lab(i);
                if bg.is_empty() {
                    // Nothing but the painted side around.
                    return if remove { 0.0 } else { 1.0 };
                }
                if fg.is_empty() {
                    // Only backdrop around: how far from it, up to a solid
                    // strand's distance.
                    let share = ((nearest(&bg, c) - 6.0) / 24.0).clamp(0.0, 1.0);
                    return if remove { 1.0 - share } else { share };
                }
                let (to_fg, to_bg) = (nearest(&fg, c), nearest(&bg, c));
                let total = (to_fg + to_bg).max(1e-3);
                let like = if remove { to_fg } else { to_bg };
                let share = ((like / total - 0.1) / 0.8).clamp(0.0, 1.0).powf(0.7);
                share * (1.0 - smoothstep(1.3, 1.8, total / span.max(1.0)))
            })
            .collect()
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

fn smoothstep(lo: f32, hi: f32, v: f32) -> f32 {
    let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Brush tip: 1 at the centre, 0 at the rim; harder tips hold 1 further out.
fn falloff(d: f32, radius: f32, hardness: f32) -> f32 {
    let t = (d / radius).clamp(0.0, 1.0);
    let soft = 1.0 - hardness.clamp(0.0, 1.0);
    if soft < 0.01 || t <= 1.0 - soft {
        1.0
    } else {
        let f = (t - (1.0 - soft)) / soft;
        1.0 - f * f
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
        p.stamp(&rgba, width, StampOp::Restore, 38.0, 20.0, 10.0, 1.0);
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
                let model = super::super::analyze(&rgba, width, height, false, &|_| {}).unwrap();
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

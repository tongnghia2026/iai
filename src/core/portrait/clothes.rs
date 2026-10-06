//! The clothes a person wears in the photo: where they lie, how the light
//! falls across them, and their own picture drawn sharp.
//!
//! [`ClothesArea`] is the clothes' mask, read from the part labels (and
//! mended with the "Tô vùng" brush), with the slope of the light over them:
//! "Đều sáng áo" takes that slope out, "Sáng áo" makes them lighter or
//! darker.
//!
//! [`ClothesDetail`] is "Nét áo": the clothes drawn sharp again by an
//! upscaling model (Real-ESRGAN), for a soft phone photo or an old print.
//! The model is shown the clothes at the size where what blurs them is finer
//! than a pixel: smaller than they lie in a photo that was enlarged or is
//! soft, as they lie in one that holds all its pixels can (shown smaller,
//! such a photo would lose its small print). Its picture is laid back at the
//! photo's size, and only what is finer than the cloth's shading is taken
//! from it: tone and colour stay the photo's.
//!
//! A garment laid on from a shop's sheet is a layer of its own and never
//! comes here.

use image::imageops::{resize, FilterType};
use rayon::prelude::*;

use super::analysis::{smoothstep, Clip, FaceModel, PortraitModel};
use super::blur::blur4;
use super::geometry::Region;
use crate::core::ai::body_parts::{BodyLabels, Segmenter};
use crate::core::ai::retouch::Upscaler;
use crate::core::develop::{linear_to_srgb, srgb_to_linear};

/// Pixels the model is shown at most: a quarter of a minute on the CPU.
const SHOWN_PIXELS: f32 = 600_000.0;
/// The clothes are never shown smaller than this share of their size in the
/// photo, however soft they read.
const SMALLEST: f32 = 0.3;
/// The blur, in pixels, of the edges of a photo that is sharp at its size.
const CRISP: f32 = 1.0;
/// The two blurs an edge is measured between (three box passes of radius 1
/// and of radius 3) and their variances, each with that of the central
/// difference the gradient is read by.
const MEASURE: (f32, f32) = (1.0, 3.0);
const MEASURE_VARIANCE: (f32, f32) = (2.0 + 1.0 / 3.0, 12.0 + 1.0 / 3.0);
/// The share of the gradients near the clothes that the strongest edges are
/// read above, and the fewest pixels and the least gradient worth reading.
const STRONGEST: f32 = 0.997;
const FEWEST_NEAR: usize = 500;
const FAINTEST_EDGE: f32 = 1.0;
/// Photo pixels shown around the clothes each way, so that their edge is not
/// the picture's; and how far around them, in face extents, the model draws,
/// for clothes the brush adds beside those found.
const CONTEXT: f32 = 24.0;
const DRAWN_AROUND: f32 = 0.25;
/// The blur, in face extents, whose result stays the photo's: the cloth's
/// shading and colour.
const TONE_SIGMA: f32 = 1.0 / 40.0;
/// Sapiens2 classes that are worn: apparel, shoes, socks, lower and upper
/// clothing.
const WORN: [u8; 7] = [1, 9, 10, 13, 18, 19, 23];
/// How sure the labels must be of something worn: where the mask starts and
/// where it is full.
const SURE: (f32, f32) = (0.4, 0.75);
/// Mask levels (0..255) that count as clothes when their bounds are taken,
/// and as solid when there are too few of them to be a garment.
const FAINT: u8 = 4;
const SOLID: u8 = 128;
const FEWEST: usize = 400;
/// The light's slope is read on a grid this many cells along the clothes'
/// longer side, between cells this far apart, where a cell is this full of
/// clothes; fewer readings than this are none.
const LIGHT_GRID: u32 = 256;
const LIGHT_STEP: usize = 2;
const LIGHT_FULL: f32 = 0.9;
const LIGHT_FEWEST: usize = 50;
/// The clothes count as well lit above this share of their light.
const LIT_SHARE: f32 = 0.75;
/// At 100, how much of the shade below the well lit part "Đều sáng áo"
/// lifts, how much of what is brighter it eases, and the most it changes
/// the light, in ln units.
const EVEN_LIFT: f32 = 0.9;
const EVEN_EASE: f32 = 0.35;
const EVEN_LIMIT: f32 = 0.9;
/// Stops of light "Sáng áo" moves at either end.
const BRIGHTNESS_STOPS: f32 = 1.25;

/// How the light falls across the clothes: the slope of the ln of their
/// linear brightness per pixel each way, and that plane's level where they
/// are well lit.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Light {
    slope: [f32; 2],
    lit: f32,
}

impl Light {
    /// What "Đều sáng áo" at `amount` (0..1) multiplies the linear light of
    /// image pixel (x, y) by: shade lifted toward the well lit part, what
    /// is brighter eased a little.
    fn evened(&self, amount: f32, x: f32, y: f32) -> f32 {
        let shade = self.lit - (self.slope[0] * x + self.slope[1] * y);
        let share = if shade > 0.0 { EVEN_LIFT } else { EVEN_EASE };
        (amount * share * shade)
            .clamp(-EVEN_LIMIT, EVEN_LIMIT)
            .exp()
    }
}

/// The clothes sliders as the retouch reads them: 0..1, brightness -1..1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ClothesLook {
    pub sharpen: f32,
    pub even: f32,
    pub brightness: f32,
}

impl ClothesLook {
    pub(super) fn at_rest(&self) -> bool {
        self.sharpen <= 0.0 && self.even <= 0.0 && self.brightness == 0.0
    }
}

/// A colour (0..1) with its linear light multiplied by `gain`, then made
/// `stops` lighter or darker. Darker scales the light; lighter raises it
/// toward white along 1 − (1 − y)^(2^stops), which a white shirt's folds
/// survive. The colour keeps its hue.
fn relit(c: [f32; 3], gain: f32, stops: f32) -> [f32; 3] {
    let mut light = c.map(|v| srgb_to_linear(v.clamp(0.0, 1.0)) * gain);
    if stops < 0.0 {
        let scale = stops.exp2();
        light = light.map(|v| v * scale);
    } else if stops > 0.0 {
        let y = (0.2126 * light[0] + 0.7152 * light[1] + 0.0722 * light[2]).min(1.0);
        if y > 1e-5 {
            let lifted = 1.0 - (1.0 - y).powf(stops.exp2());
            light = light.map(|v| v * lifted / y);
        }
    }
    light.map(|v| linear_to_srgb(v.clamp(0.0, 1.0)))
}

/// The smallest region of `region` holding the mask's clothes; empty when it
/// has none.
fn bounds_of(mask: &[u8], region: Region) -> Region {
    let w = region.w as usize;
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for (i, &m) in mask.iter().enumerate() {
        if m >= FAINT {
            let (x, y) = ((i % w) as u32, (i / w) as u32);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
    }
    if x0 > x1 {
        return Region {
            x: region.x,
            y: region.y,
            w: 0,
            h: 0,
        };
    }
    Region {
        x: region.x + x0,
        y: region.y + y0,
        w: x1 - x0 + 1,
        h: y1 - y0 + 1,
    }
}

/// The light over the clothes (`mask` over `region`, within `bounds`) of the
/// image `rgba`, `width` pixels a row: the median slope of the ln of their
/// brightness between neighbouring cells of a coarse grid. The cloth's own
/// edges (a jacket over a shirt, a print) are few among those readings and
/// folds lean both ways, so what is left is the light's.
fn light_of(rgba: &[u8], width: u32, region: Region, mask: &[u8], bounds: Region) -> Light {
    if bounds.is_empty() {
        return Light::default();
    }
    let cell = bounds.w.max(bounds.h).div_ceil(LIGHT_GRID).max(1) as usize;
    let (gw, gh) = (
        (bounds.w as usize).div_ceil(cell),
        (bounds.h as usize).div_ceil(cell),
    );
    let linear: Vec<f32> = (0..256).map(|v| srgb_to_linear(v as f32 / 255.0)).collect();
    // Per cell: the clothes' share of it and the mean ln of their light.
    let cells: Vec<(f32, f32)> = (0..gw * gh)
        .into_par_iter()
        .map(|c| {
            let (x0, y0) = ((c % gw) * cell, (c / gw) * cell);
            let (x1, y1) = (
                (x0 + cell).min(bounds.w as usize),
                (y0 + cell).min(bounds.h as usize),
            );
            let (mut weight, mut sum) = (0.0f32, 0.0f32);
            for y in y0..y1 {
                let (iy, my) = (bounds.y as usize + y, (bounds.y - region.y) as usize + y);
                for x in x0..x1 {
                    let m = mask[my * region.w as usize + (bounds.x - region.x) as usize + x];
                    if m == 0 {
                        continue;
                    }
                    let o = (iy * width as usize + bounds.x as usize + x) * 4;
                    let light = 0.2126 * linear[rgba[o] as usize]
                        + 0.7152 * linear[rgba[o + 1] as usize]
                        + 0.0722 * linear[rgba[o + 2] as usize];
                    let m = m as f32 / 255.0;
                    weight += m;
                    sum += m * light.max(1e-3).ln();
                }
            }
            let area = ((x1 - x0) * (y1 - y0)).max(1) as f32;
            (weight / area, sum / weight.max(1e-6))
        })
        .collect();
    let full = |c: usize| cells[c].0 > LIGHT_FULL;
    let d = LIGHT_STEP;
    let (mut across, mut down) = (Vec::new(), Vec::new());
    for gy in 0..gh {
        for gx in 0..gw {
            let c = gy * gw + gx;
            if !full(c) {
                continue;
            }
            if gx >= d && gx + d < gw && full(c - d) && full(c + d) {
                across.push((cells[c + d].1 - cells[c - d].1) / (2 * d * cell) as f32);
            }
            if gy >= d && gy + d < gh && full(c - d * gw) && full(c + d * gw) {
                down.push((cells[c + d * gw].1 - cells[c - d * gw].1) / (2 * d * cell) as f32);
            }
        }
    }
    let median = |values: &mut Vec<f32>| {
        if values.len() < LIGHT_FEWEST {
            return 0.0;
        }
        let k = values.len() / 2;
        *values.select_nth_unstable_by(k, f32::total_cmp).1
    };
    let slope = [median(&mut across), median(&mut down)];
    // The plane's level over the cells that hold clothes.
    let mut levels: Vec<f32> = (0..gw * gh)
        .filter(|&c| cells[c].0 > 0.5)
        .map(|c| {
            let x = bounds.x as f32 + ((c % gw) * cell) as f32 + cell as f32 * 0.5;
            let y = bounds.y as f32 + ((c / gw) * cell) as f32 + cell as f32 * 0.5;
            slope[0] * x + slope[1] * y
        })
        .collect();
    if levels.is_empty() {
        return Light::default();
    }
    let k = ((levels.len() - 1) as f32 * LIT_SHARE) as usize;
    let lit = *levels.select_nth_unstable_by(k, f32::total_cmp).1;
    Light { slope, lit }
}

/// One person's clothes: how much of each pixel around them is clothes, and
/// the light across them.
#[derive(Clone)]
pub struct ClothesArea {
    /// Everything the labels saw around the person: where the mask lies and
    /// the brush may paint.
    pub region: Region,
    pub(super) mask: Vec<u8>,
    /// The part of `region` that holds clothes.
    pub(super) bounds: Region,
    pub(super) light: Light,
}

impl ClothesArea {
    fn new(rgba: &[u8], width: u32, region: Region, mask: Vec<u8>) -> Self {
        let bounds = bounds_of(&mask, region);
        let light = light_of(rgba, width, region, &mask, bounds);
        Self {
            region,
            mask,
            bounds,
            light,
        }
    }

    /// How much each pixel of `region` is clothes.
    pub fn mask(&self) -> &[u8] {
        &self.mask
    }

    /// The part of `region` that holds clothes.
    pub fn bounds(&self) -> Region {
        self.bounds
    }

    /// This area with `mask` (painted with the brush) in place of its own:
    /// where the clothes lie and the light across them are read again from
    /// the image `rgba`, `width` pixels a row.
    pub fn with_mask(&self, rgba: &[u8], width: u32, mask: Vec<u8>) -> Self {
        if mask.len() != self.region.len() {
            return self.clone();
        }
        Self::new(rgba, width, self.region, mask)
    }

    /// Add to `delta` (over `union`) what `look` changes of the clothes:
    /// the model's picture `sharp` in place of the photo's as far as "Nét
    /// áo" goes, then the light evened and made lighter or darker. `pixel`
    /// reads the photo at image coordinates.
    pub(super) fn lay(
        &self,
        delta: &mut [[f32; 3]],
        union: Region,
        look: &ClothesLook,
        sharp: Option<&ClothesDetail>,
        pixel: &(dyn Fn(usize, usize) -> [f32; 3] + Sync),
    ) {
        let r = self.bounds.intersect(union);
        if r.is_empty() || look.at_rest() {
            return;
        }
        let uw = union.w as usize;
        let (dx, dy) = ((r.x - union.x) as usize, (r.y - union.y) as usize);
        let sharp = sharp.filter(|_| look.sharpen > 0.0);
        let relight = look.even > 0.0 || look.brightness != 0.0;
        let stops = look.brightness * BRIGHTNESS_STOPS;
        delta
            .par_chunks_mut(uw)
            .skip(dy)
            .take(r.h as usize)
            .enumerate()
            .for_each(|(row, line)| {
                let y = r.y + row as u32;
                for col in 0..r.w as usize {
                    let x = r.x + col as u32;
                    let Some(k) = self.region.index_at(x, y) else {
                        continue;
                    };
                    let weight = self.mask[k] as f32 / 255.0;
                    if weight <= 0.0 {
                        continue;
                    }
                    let src = pixel(x as usize, y as usize);
                    let mut c = src;
                    if let Some(drawn) = sharp.and_then(|s| s.at(x, y)) {
                        for k in 0..3 {
                            c[k] += (drawn[k] - src[k]) * look.sharpen;
                        }
                    }
                    if relight {
                        let gain = self.light.evened(look.even, x as f32 + 0.5, y as f32 + 0.5);
                        c = relit(c, gain, stops);
                    }
                    let cell = &mut line[dx + col];
                    for k in 0..3 {
                        cell[k] += (c[k] - src[k]) * weight;
                    }
                }
            });
    }
}

/// The upscaling model's picture of one person's clothes, over the part of
/// the photo it was shown.
pub struct ClothesDetail {
    pub region: Region,
    restored: Vec<[u8; 3]>,
}

impl ClothesDetail {
    /// The model's colour (0..1) at image pixel (x, y), where it drew.
    fn at(&self, x: u32, y: u32) -> Option<[f32; 3]> {
        let k = self.region.index_at(x, y)?;
        Some(self.restored[k].map(|v| v as f32 / 255.0))
    }
}

/// The clothes in `worn` (how far each pixel of `seen` is something worn,
/// 0..1) less what `taken` says is skin or hair at an image pixel, as a mask
/// over `seen`. None when there is no garment.
fn mask_of(
    worn: &[f32],
    seen: Region,
    taken: &(dyn Fn(u32, u32) -> f32 + Sync),
) -> Option<Vec<u8>> {
    let sw = seen.w as usize;
    let mask: Vec<u8> = worn
        .par_iter()
        .enumerate()
        .map(|(i, &worn)| {
            let sure = smoothstep(SURE.0, SURE.1, worn);
            if sure <= 0.0 {
                return 0;
            }
            let (x, y) = (seen.x + (i % sw) as u32, seen.y + (i / sw) as u32);
            (sure * (1.0 - taken(x, y).clamp(0.0, 1.0)) * 255.0).round() as u8
        })
        .collect();
    let solid = mask.par_iter().filter(|&&m| m >= SOLID).count();
    (solid >= FEWEST).then_some(mask)
}

/// The clothes of the person of `face` (face `index` of `owners`: each
/// face's centre and extent), from the labels kept around the face and, past
/// what those saw, from `body`'s: the region seen and their mask over it.
fn clothes_mask(
    face: &FaceModel,
    body: Option<&BodyLabels>,
    owners: &[([f32; 2], f32)],
    index: usize,
    width: u32,
    height: u32,
) -> Option<(Region, Vec<u8>)> {
    let parts = face.parts.as_ref()?;
    let [x0, y0, x1, y1] = parts.bounds();
    let mut corners = vec![[x0, y0], [x1, y1]];
    if let Some(body) = body {
        let (columns, rows) = BodyLabels::SIZE;
        corners.push(body.to_image(0.0, 0.0));
        corners.push(body.to_image(columns as f32, rows as f32));
    }
    let seen = Region::around(corners.into_iter(), [0.0; 4], width, height);
    if seen.is_empty() {
        return None;
    }
    let (sw, own) = (seen.w as usize, owners[index]);
    let mut worn: Vec<[f32; 4]> = (0..seen.len())
        .into_par_iter()
        .map(|i| {
            let x = seen.x as f32 + (i % sw) as f32 + 0.5;
            let y = seen.y as f32 + (i / sw) as f32 + 0.5;
            // Pixels nearer another face (in face sizes) are that person's.
            let near = (x - own.0[0]).hypot(y - own.0[1]) / own.1;
            if owners
                .iter()
                .enumerate()
                .any(|(j, (c, e))| j != index && (x - c[0]).hypot(y - c[1]) / e < near)
            {
                return [0.0; 4];
            }
            let level = if parts.covers(x, y) {
                parts.worn_at(x, y)
            } else {
                body.and_then(|body| body.label_at(x, y))
                    .map_or(0.0, |label| f32::from(WORN.contains(&label)))
            };
            [level, 0.0, 0.0, 0.0]
        })
        .collect();
    if let Some(body) = body {
        // The body's labels are a grid of cells: their steps are smoothed away.
        blur4(
            &mut worn,
            sw,
            seen.h as usize,
            (body.scale() * 0.5).max(1.0),
        );
    }
    let worn: Vec<f32> = worn.into_par_iter().map(|w| w[0]).collect();
    let level = |layer: &[u8], region: Region, x: u32, y: u32| {
        region
            .index_at(x, y)
            .and_then(|k| layer.get(k))
            .map_or(0.0, |&v| v as f32 / 255.0)
    };
    let mask = mask_of(&worn, seen, &|x, y| {
        level(&face.skin.mask, face.skin.region, x, y).max(level(
            &face.hair,
            face.hair_region,
            x,
            y,
        ))
    })?;
    Some((seen, mask))
}

/// How wide the edges in and around the clothes are blurred: the sigma, in
/// pixels, of the blur that would soften a clean edge to the strongest ones
/// there. `photo` is the `shown` part of the image. Where there is no edge
/// to read, the photo passes for sharp.
fn edge_blur(photo: &image::RgbImage, shown: Region, area: &ClothesArea) -> f32 {
    let (w, h) = (shown.w as usize, shown.h as usize);
    if w < 8 || h < 8 {
        return CRISP;
    }
    let mut fine: Vec<[f32; 4]> = photo
        .pixels()
        .map(|p| {
            let [r, g, b] = p.0.map(|v| v as f32);
            [0.299 * r + 0.587 * g + 0.114 * b, 0.0, 0.0, 0.0]
        })
        .collect();
    let mut coarse = fine.clone();
    // The clothes and a band around them: their outline against the skin and
    // the backdrop is as good an edge as any on the cloth.
    let mut near: Vec<[f32; 4]> = (0..w * h)
        .map(|i| {
            let (x, y) = (shown.x + (i % w) as u32, shown.y + (i / w) as u32);
            let level = area.region.index_at(x, y).map_or(0, |k| area.mask[k]);
            [level as f32 / 255.0, 0.0, 0.0, 0.0]
        })
        .collect();
    blur4(&mut fine, w, h, MEASURE.0);
    blur4(&mut coarse, w, h, MEASURE.1);
    blur4(&mut near, w, h, 8.0);
    let gradient = |plane: &[[f32; 4]], i: usize| {
        let dx = (plane[i + 1][0] - plane[i - 1][0]) * 0.5;
        let dy = (plane[i + w][0] - plane[i - w][0]) * 0.5;
        dx.hypot(dy)
    };
    let (mut sharp, mut soft) = (Vec::new(), Vec::new());
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let i = y * w + x;
            if near[i][0] > 0.05 {
                sharp.push(gradient(&fine, i));
                soft.push(gradient(&coarse, i));
            }
        }
    }
    if sharp.len() < FEWEST_NEAR {
        return CRISP;
    }
    let strongest = |values: &mut Vec<f32>| {
        let k = ((values.len() - 1) as f32 * STRONGEST) as usize;
        *values.select_nth_unstable_by(k, f32::total_cmp).1
    };
    let (sharp, soft) = (strongest(&mut sharp), strongest(&mut soft));
    if soft < FAINTEST_EDGE {
        return CRISP;
    }
    // A clean edge blurred by b reads 1 / sqrt(b² + v) through a blur of
    // variance v: the two readings give b.
    let ratio = ((sharp / soft) * (sharp / soft)).max(1.0001);
    let (near_v, far_v) = MEASURE_VARIANCE;
    ((far_v - ratio * near_v) / (ratio - 1.0)).max(0.0).sqrt()
}

/// The share of their size in the photo the model is shown the clothes at:
/// all of it when the photo holds what its pixels can, less when it was
/// enlarged `enlarged` times to this size or its edges are blurred `blur`
/// pixels wide.
fn shown_scale(enlarged: f32, blur: f32) -> f32 {
    (1.0 / enlarged.max(1.0))
        .min(CRISP / blur.max(CRISP))
        .max(SMALLEST)
}

/// The size the model is shown a region of the photo at, for `scale` of its
/// size there.
fn shown_size(shown: Region, scale: f32) -> (u32, u32) {
    let scale = scale.min((SHOWN_PIXELS / shown.len().max(1) as f32).sqrt());
    let side = |v: u32| ((v as f32 * scale).round() as u32).clamp(1, v.max(1));
    (side(shown.w), side(shown.h))
}

/// What `upscale` makes of `region` of a photo enlarged `enlarged` times
/// from what was shot (1 when it was not, or it is not known), at the
/// photo's size and with the photo's tone at the scale of `tone_sigma`
/// pixels. `area` is the clothes there, by whose edges the photo's blur is
/// read. `upscale` draws an RGB picture (0..1, width x height) larger and
/// says how many times.
#[allow(clippy::too_many_arguments)]
fn restore(
    rgba: &[u8],
    width: u32,
    height: u32,
    region: Region,
    area: &ClothesArea,
    enlarged: f32,
    tone_sigma: f32,
    upscale: &mut dyn FnMut(&[[f32; 3]], usize, usize) -> Result<(Vec<[f32; 3]>, usize), String>,
) -> Result<Vec<[u8; 3]>, String> {
    let corners = [
        [region.x as f32, region.y as f32],
        [(region.x + region.w) as f32, (region.y + region.h) as f32],
    ];
    let shown = Region::around(corners.into_iter(), [CONTEXT; 4], width, height);
    if shown.is_empty() || rgba.len() != width as usize * height as usize * 4 {
        return Err("vùng áo không hợp lệ".to_string());
    }
    let photo = image::RgbImage::from_fn(shown.w, shown.h, |x, y| {
        let o = ((shown.y + y) as usize * width as usize + (shown.x + x) as usize) * 4;
        image::Rgb([rgba[o], rgba[o + 1], rgba[o + 2]])
    });
    let scale = shown_scale(enlarged, edge_blur(&photo, shown, area));
    let (small_w, small_h) = shown_size(shown, scale);
    let small = resize(&photo, small_w, small_h, FilterType::CatmullRom);
    let unit: Vec<[f32; 3]> = small
        .pixels()
        .map(|p| p.0.map(|v| v as f32 / 255.0))
        .collect();
    let (drawn, times) = upscale(&unit, small_w as usize, small_h as usize)?;
    let (drawn_w, drawn_h) = (small_w * times as u32, small_h * times as u32);
    if times == 0 || drawn.len() != drawn_w as usize * drawn_h as usize {
        return Err("model làm nét trả về ảnh sai cỡ".to_string());
    }
    let drawn = image::RgbImage::from_fn(drawn_w, drawn_h, |x, y| {
        image::Rgb(
            drawn[(y * drawn_w + x) as usize].map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8),
        )
    });
    let laid = resize(&drawn, shown.w, shown.h, FilterType::CatmullRom);
    // What the model shifted of the photo's tone goes back.
    let mut shift: Vec<[f32; 4]> = photo
        .pixels()
        .zip(laid.pixels())
        .map(|(p, l)| {
            [
                p.0[0] as f32 - l.0[0] as f32,
                p.0[1] as f32 - l.0[1] as f32,
                p.0[2] as f32 - l.0[2] as f32,
                0.0,
            ]
        })
        .collect();
    blur4(
        &mut shift,
        shown.w as usize,
        shown.h as usize,
        tone_sigma.max(2.0),
    );
    let laid = laid.as_raw();
    Ok((0..region.len())
        .into_par_iter()
        .map(|i| {
            let k = shown.index_of(region, i);
            std::array::from_fn(|c| {
                (laid[k * 3 + c] as f32 + shift[k][c])
                    .round()
                    .clamp(0.0, 255.0) as u8
            })
        })
        .collect())
}

/// Each face of `model` that is `wanted`, with its index.
fn wanted_faces<'a>(model: &'a PortraitModel, wanted: &[bool]) -> Vec<(usize, &'a FaceModel)> {
    model
        .faces
        .iter()
        .enumerate()
        .zip(wanted.iter().chain(std::iter::repeat(&true)))
        .filter(|(_, &on)| on)
        .map(|(face, _)| face)
        .collect()
}

/// Find the clothes of each face of `model` that is `wanted` and has none
/// yet (at once, or a few seconds when they run past what was seen around
/// the face); the result is kept on the face.
pub fn analyze_clothes_areas(
    rgba: &[u8],
    model: &PortraitModel,
    wanted: &[bool],
    prefer_gpu: bool,
) {
    let owners: Vec<([f32; 2], f32)> = model
        .faces
        .iter()
        .map(|face| {
            let (centre, extent, _) = face.mesh.frame();
            (centre, extent)
        })
        .collect();
    let (width, height) = (model.width, model.height);
    let frame = model.clip.as_ref().map(Clip::bounds);
    let mut segmenter: Option<Segmenter> = None;
    for (index, face) in wanted_faces(model, wanted) {
        face.clothes_area.get_or_init(|| {
            let parts = face.parts.as_ref().ok_or_else(|| {
                "chưa nhận ra áo (cần model tách vùng models\\sapiens2-seg)".to_string()
            })?;
            // Clothes that run past what was seen around the face: one look
            // at the whole body. Its failing leaves the clothes seen so far.
            let body = parts
                .worn_cut_off(width, height)
                .then(|| {
                    if segmenter.is_none() {
                        segmenter = Segmenter::load(prefer_gpu).ok();
                    }
                    segmenter
                        .as_mut()?
                        .segment_body(rgba, width, height, &face.mesh, frame, 1)
                        .ok()?
                        .into_iter()
                        .next()
                })
                .flatten();
            let (region, mask) = clothes_mask(face, body.as_ref(), &owners, index, width, height)
                .ok_or_else(|| "không nhận ra áo trong ảnh".to_string())?;
            Ok(ClothesArea::new(rgba, width, region, mask))
        });
    }
}

/// Find the clothes of each face of `model` that is `wanted`, and have the
/// upscaling model draw those it has not drawn yet (a few seconds a person
/// on the CPU); the result is kept on the face. `enlarged` is how many times
/// the photo was enlarged from what was shot to this size, when the caller
/// knows (an ID photo cropped from a small one), else 1.
pub fn analyze_clothes(
    rgba: &[u8],
    model: &PortraitModel,
    wanted: &[bool],
    prefer_gpu: bool,
    enlarged: f32,
) {
    analyze_clothes_areas(rgba, model, wanted, prefer_gpu);
    let (width, height) = (model.width, model.height);
    let mut upscaler: Option<Result<Upscaler, String>> = None;
    for (_, face) in wanted_faces(model, wanted) {
        face.clothes.get_or_init(|| {
            let area = match face.clothes_area.get() {
                Some(Ok(area)) => area,
                Some(Err(error)) => return Err(error.clone()),
                None => return Err("chưa tìm áo".to_string()),
            };
            let upscaler = upscaler
                .get_or_insert_with(Upscaler::load)
                .as_mut()
                .map_err(|error| error.clone())?;
            // Somewhat past the clothes found: the brush may add to them.
            let b = area.bounds;
            let corners = [
                [b.x as f32, b.y as f32],
                [(b.x + b.w) as f32, (b.y + b.h) as f32],
            ];
            let around = [DRAWN_AROUND * face.extent; 4];
            let region =
                Region::around(corners.into_iter(), around, width, height).intersect(area.region);
            let restored = restore(
                rgba,
                width,
                height,
                region,
                area,
                enlarged,
                TONE_SIGMA * face.extent,
                &mut |rgb, w, h| upscaler.run(rgb, w, h),
            )?;
            Ok(ClothesDetail { region, restored })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A photo `w` x `h`: a soft ramp left to right, dark to light.
    fn ramp(w: u32, h: u32) -> Vec<u8> {
        (0..w * h)
            .flat_map(|i| {
                let v = (60 + (i % w) * 120 / w) as u8;
                [v, v, v, 255]
            })
            .collect()
    }

    fn whole(w: u32, h: u32) -> Region {
        Region { x: 0, y: 0, w, h }
    }

    /// Clothes all over a photo `w` x `h` painted by `colour` at (x, y).
    fn cloth(w: u32, h: u32, colour: impl Fn(u32, u32) -> [u8; 3]) -> (Vec<u8>, ClothesArea) {
        let rgba: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let [r, g, b] = colour(i % w, i / w);
                [r, g, b, 255]
            })
            .collect();
        let area = ClothesArea::new(&rgba, w, whole(w, h), vec![255; (w * h) as usize]);
        (rgba, area)
    }

    #[test]
    fn clothes_are_what_is_worn_less_skin_and_hair_and_a_few_pixels_are_none() {
        let seen = Region {
            x: 10,
            y: 20,
            w: 100,
            h: 80,
        };
        // Worn from row 30 of the region down; skin over image columns 50..70.
        let worn: Vec<f32> = (0..seen.len())
            .map(|i| if i / 100 >= 30 { 0.95 } else { 0.05 })
            .collect();
        let taken = |x: u32, _: u32| if (50..70).contains(&x) { 1.0 } else { 0.0 };
        let mask = mask_of(&worn, seen, &taken).unwrap();
        let at = |x: u32, y: u32| mask[seen.index_at(x, y).unwrap()];
        assert_eq!(at(20, 60), 255, "clothes");
        assert_eq!(at(55, 60), 0, "the skin in their opening");
        assert_eq!(at(20, 40), 0, "above them");
        // Their bounds leave out the rows above and nothing else.
        assert_eq!(
            bounds_of(&mask, seen),
            Region {
                x: 10,
                y: 50,
                w: 100,
                h: 50
            }
        );
        assert!(bounds_of(&vec![0; seen.len()], seen).is_empty());
        // A scrap of something worn is no garment.
        let scrap: Vec<f32> = (0..seen.len())
            .map(|i| if i < 300 { 1.0 } else { 0.0 })
            .collect();
        assert!(mask_of(&scrap, seen, &|_, _| 0.0).is_none());
    }

    #[test]
    fn the_lights_slope_is_read_past_the_cloths_own_edges_and_evened_out() {
        let (w, h) = (300u32, 200u32);
        // Light falling off to the right by a stop and a half over the
        // garment, on a pale shirt under a dark jacket (left of column 90).
        let lit = |x: u32| 0.9f32 * (-1.04 * x as f32 / w as f32).exp();
        let shade = |x: u32, _: u32| {
            let cloth = if x < 90 { 0.12 } else { 1.0 };
            [(linear_to_srgb(lit(x) * cloth) * 255.0).round() as u8; 3]
        };
        let (rgba, area) = cloth(w, h, shade);
        let slope = area.light.slope;
        assert!((slope[0] * w as f32 + 1.04).abs() < 0.2, "{slope:?}");
        assert!((slope[1] * h as f32).abs() < 0.05, "{slope:?}");

        // "Đều sáng áo" at 100 brings the shirt's far side most of the way
        // to its near side, and leaves the jacket a jacket.
        let look = ClothesLook {
            sharpen: 0.0,
            even: 1.0,
            brightness: 0.0,
        };
        let pixel = |x: usize, y: usize| {
            let o = (y * w as usize + x) * 4;
            [rgba[o], rgba[o + 1], rgba[o + 2]].map(|v| v as f32 / 255.0)
        };
        let mut delta = vec![[0.0f32; 3]; (w * h) as usize];
        area.lay(&mut delta, whole(w, h), &look, None, &pixel);
        let shown = |x: usize| pixel(x, 100)[0] + delta[100 * w as usize + x][0];
        let before = pixel(120, 100)[0] - pixel(280, 100)[0];
        let after = shown(120) - shown(280);
        assert!(
            before > 0.15 && after.abs() < 0.35 * before,
            "{before} {after}"
        );
        assert!(shown(40) < 0.6 * shown(120), "the jacket {}", shown(40));

        // Even light has no slope: nothing to even.
        let (_, flat) = cloth(w, h, |x, _| if x < 90 { [40; 3] } else { [200; 3] });
        assert_eq!(flat.light.slope, [0.0, 0.0]);
    }

    #[test]
    fn lighter_never_clips_and_darker_scales_the_light() {
        let white = relit([0.9; 3], 1.0, 1.25)[0];
        let fold = relit([0.8; 3], 1.0, 1.25)[0];
        assert!(white > 0.9 && white < 1.0 && fold < white, "{white} {fold}");
        // A stop darker halves the linear light.
        let darker = relit([0.9; 3], 1.0, -1.0)[0];
        assert!((srgb_to_linear(darker) - 0.5 * srgb_to_linear(0.9)).abs() < 2e-3);
        // The hue stays: the channels keep their ratio in linear light.
        let teal = relit([0.2, 0.6, 0.5], 1.0, 0.8);
        let ratio = |c: [f32; 3]| srgb_to_linear(c[0]) / srgb_to_linear(c[1]);
        assert!((ratio(teal) - ratio([0.2, 0.6, 0.5])).abs() < 0.01);
        let same = relit([0.3, 0.5, 0.7], 1.0, 0.0);
        assert!((0..3).all(|k| (same[k] - [0.3, 0.5, 0.7][k]).abs() < 1e-4));
    }

    #[test]
    fn the_model_is_shown_the_clothes_as_small_as_the_photo_is_soft_within_its_budget() {
        let near = |a: f32, b: f32| (a - b).abs() < 1e-3;
        // A sharp photo at its own size is shown whole.
        assert!(near(shown_scale(1.0, 0.6), 1.0));
        // One enlarged, or blurred, by as much as that.
        assert!(near(shown_scale(1.6, 0.6), 0.625));
        assert!(near(shown_scale(1.0, 2.0), 0.5));
        assert!(near(shown_scale(2.0, 4.0), SMALLEST));
        assert!(near(shown_scale(0.5, 0.0), 1.0));

        let size = |w, h, scale| shown_size(whole(w, h), scale);
        assert_eq!(size(600, 400, 0.5), (300, 200));
        let (w, h) = size(3000, 2000, 1.0);
        assert!((w * h) as f32 <= SHOWN_PIXELS * 1.01, "{w} x {h}");
        assert!((w as f32 / h as f32 - 1.5).abs() < 0.01);
        assert_eq!(size(1, 1, 0.5), (1, 1));
    }

    #[test]
    fn an_edges_blur_is_read_from_the_photo() {
        let (w, h) = (160u32, 120u32);
        let region = whole(w, h);
        // A light cloth against a dark ground, the edge blurred `sigma` wide.
        let read = |sigma: f32| {
            let mut plane: Vec<[f32; 4]> = (0..region.len())
                .map(|i| [if i as u32 % w < 80 { 60.0 } else { 180.0 }, 0.0, 0.0, 0.0])
                .collect();
            if sigma > 0.0 {
                blur4(&mut plane, w as usize, h as usize, sigma);
            }
            let photo = image::RgbImage::from_fn(w, h, |x, y| {
                image::Rgb([plane[(y * w + x) as usize][0].round() as u8; 3])
            });
            let (_, area) = cloth(w, h, |_, _| [120; 3]);
            edge_blur(&photo, region, &area)
        };
        assert!(read(0.0) < 0.6, "a clean edge {}", read(0.0));
        // Three box passes of radius 2 and 3: sigmas of about 2.4 and 3.5.
        assert!((read(2.0) - 2.45).abs() < 0.4, "{}", read(2.0));
        assert!((read(3.0) - 3.46).abs() < 0.5, "{}", read(3.0));
        // Plain cloth has no edge to read: it passes for sharp.
        let plain = image::RgbImage::from_pixel(w, h, image::Rgb([120; 3]));
        let (_, area) = cloth(w, h, |_, _| [120; 3]);
        assert_eq!(edge_blur(&plain, region, &area), CRISP);
    }

    #[test]
    fn the_models_picture_is_laid_back_with_its_detail_and_the_photos_tone() {
        let (w, h) = (240u32, 200u32);
        let photo = ramp(w, h);
        let region = Region {
            x: 40,
            y: 50,
            w: 160,
            h: 100,
        };
        let area = ClothesArea::new(&photo, w, whole(w, h), vec![255; (w * h) as usize]);
        let mut shown = (0, 0);
        // A model that doubles the picture, darkens it and draws fine stripes.
        let mut upscale = |rgb: &[[f32; 3]], sw: usize, sh: usize| {
            shown = (sw, sh);
            let out = (0..sw * 2 * sh * 2)
                .map(|i| {
                    let (x, y) = (i % (sw * 2), i / (sw * 2));
                    let stripe = if (x / 2) % 2 == 0 { 0.08 } else { -0.08 };
                    rgb[(y / 2) * sw + x / 2].map(|v| v - 0.2 + stripe)
                })
                .collect();
            Ok((out, 2))
        };
        let restored = restore(&photo, w, h, region, &area, 2.0, 6.0, &mut upscale).unwrap();
        // The region and its surroundings, at half size: the photo was
        // enlarged twice.
        assert_eq!(shown, (104, 74));
        assert_eq!(restored.len(), region.len());
        let (mut tone, mut stripes) = (0.0f64, 0.0f64);
        let rows = 40..60usize;
        for y in rows.clone() {
            for x in 40..120usize {
                let k = y * region.w as usize + x;
                let own = photo[((region.y as usize + y) * w as usize + region.x as usize + x) * 4];
                tone += restored[k][0] as f64 - own as f64;
                stripes += (restored[k][0] as f64 - restored[k + 2][0] as f64).abs();
            }
        }
        let n = (rows.len() * 80) as f64;
        assert!((tone / n).abs() < 2.0, "tone moved {}", tone / n);
        assert!(stripes / n > 15.0, "stripes {}", stripes / n);
    }

    #[test]
    fn laying_the_clothes_changes_only_their_pixels_by_the_sliders_share() {
        let union = whole(8, 4);
        let grey = [102u8, 102, 102, 255].repeat(32);
        // Clothes over columns 2..6 of rows 1 and 2, half there at column 4.
        let mut mask = vec![0u8; 32];
        for row in 1..3 {
            mask[row * 8 + 2..row * 8 + 6].copy_from_slice(&[255, 255, 128, 0]);
        }
        let area = ClothesArea::new(&grey, 8, union, mask);
        assert_eq!(
            area.bounds(),
            Region {
                x: 2,
                y: 1,
                w: 3,
                h: 2
            }
        );
        let drawn = ClothesDetail {
            region: Region {
                x: 2,
                y: 1,
                w: 4,
                h: 2,
            },
            restored: vec![[204, 204, 204]; 8],
        };
        let pixel = |_: usize, _: usize| [0.4f32; 3];
        let lay = |sharpen: f32, brightness: f32| {
            let look = ClothesLook {
                sharpen,
                even: 0.0,
                brightness,
            };
            let mut delta = vec![[0.0f32; 3]; 32];
            area.lay(&mut delta, union, &look, Some(&drawn), &pixel);
            delta
        };
        let delta = lay(0.5, 0.0);
        let at = |delta: &[[f32; 3]], x: usize, y: usize| delta[y * 8 + x][0];
        assert!(
            (at(&delta, 2, 1) - 0.2).abs() < 1e-3,
            "{}",
            at(&delta, 2, 1)
        );
        assert!(
            (at(&delta, 4, 2) - 0.1).abs() < 2e-3,
            "{}",
            at(&delta, 4, 2)
        );
        assert_eq!(at(&delta, 5, 1), 0.0, "not clothes");
        assert_eq!(at(&delta, 1, 1), 0.0, "beside them");
        assert_eq!(at(&delta, 2, 0), 0.0);
        assert!(lay(0.0, 0.0).iter().all(|d| d[0] == 0.0));
        // "Sáng áo" alone: lighter to the right, darker to the left, and
        // only on the clothes.
        let (lighter, darker) = (lay(0.0, 1.0), lay(0.0, -1.0));
        assert!(at(&lighter, 2, 1) > 0.05 && at(&darker, 2, 1) < -0.05);
        assert_eq!(at(&lighter, 5, 1), 0.0);
        // Where the model drew nothing the photo is relit as it is.
        let bare = ClothesLook {
            sharpen: 1.0,
            even: 0.0,
            brightness: 0.0,
        };
        let mut none = vec![[0.0f32; 3]; 32];
        area.lay(&mut none, union, &bare, None, &pixel);
        assert!(none.iter().all(|d| d[0] == 0.0));
    }

    /// Opt-in: set IAI_PORTRAIT_CLOTHES_PROBE to a folder of photos; writes
    /// `<name>.png` (the photo, its clothes tinted, "Nét áo" at 100) and
    /// `<name>_sang.png` (the photo, "Đều sáng áo" at 100, "Sáng áo" at -60
    /// and +60) into its `net-ao` subfolder and prints what finding and
    /// drawing them took. A photo named `..._x<times>` (`the_x1.6.png`) was
    /// enlarged that much.
    #[test]
    #[ignore]
    fn probe_clothes() {
        use super::super::{analyze, render, render_masks, PortraitSettings};

        let Ok(dir) = std::env::var("IAI_PORTRAIT_CLOTHES_PROBE") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let out = dir.join("net-ao");
        std::fs::create_dir_all(&out).unwrap();
        let mut names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                ["jpg", "jpeg", "png"].contains(&ext.to_ascii_lowercase().as_str())
            })
            .collect();
        names.sort();
        for path in names {
            let name = path.file_stem().unwrap().to_string_lossy().to_string();
            let image = image::open(&path).unwrap().to_rgba8();
            let (w, h) = image.dimensions();
            let rgba = image.into_raw();
            let model = match analyze(&rgba, w, h, false, None, &|_| {}) {
                Ok(model) => model,
                Err(error) => {
                    println!("{name}: {error}");
                    continue;
                }
            };
            let enlarged = name
                .rsplit_once("_x")
                .and_then(|(_, times)| times.parse().ok())
                .unwrap_or(1.0);
            let began = std::time::Instant::now();
            analyze_clothes_areas(&rgba, &model, &[], false);
            let found = began.elapsed().as_millis();
            analyze_clothes(&rgba, &model, &[], false, enlarged);
            let took = began.elapsed().as_millis();
            for (k, face) in model.faces.iter().enumerate() {
                match (face.clothes_area.get().unwrap(), face.clothes.get().unwrap()) {
                    (Ok(area), Ok(drawn)) => println!(
                        "{name}: {w}x{h}, face {k}: clothes {:?} light {:?}, found in {found} ms, drawn {:?} by {took} ms",
                        area.bounds, area.light, drawn.region
                    ),
                    (Err(error), _) | (_, Err(error)) => {
                        println!("{name}: face {k}: {error} ({took} ms)")
                    }
                }
            }
            let pasted = |part: Option<(Region, Vec<u8>)>| {
                let mut whole = rgba.clone();
                if let Some((r, pixels)) = part {
                    for row in 0..r.h as usize {
                        let o = ((r.y as usize + row) * w as usize + r.x as usize) * 4;
                        let line = r.w as usize * 4;
                        whole[o..o + line].copy_from_slice(&pixels[row * line..(row + 1) * line]);
                    }
                }
                image::RgbaImage::from_raw(w, h, whole).unwrap()
            };
            let with = |sharpen: f32, even: f32, brightness: f32| {
                let settings = PortraitSettings {
                    clothes_sharpen: sharpen,
                    clothes_even: even,
                    clothes_brightness: brightness,
                    ..PortraitSettings::NEUTRAL
                };
                pasted(render(&rgba, &model, &settings, &[], &[]))
            };
            let sheet = |panels: &[image::RgbaImage], file: String| {
                let mut sheet = image::RgbaImage::new(w * panels.len() as u32, h);
                for (k, panel) in panels.iter().enumerate() {
                    image::imageops::replace(&mut sheet, panel, (k as u32 * w) as i64, 0);
                }
                sheet.save(out.join(file)).unwrap();
            };
            sheet(
                &[
                    pasted(None),
                    pasted(render_masks(
                        &rgba,
                        &model,
                        &PortraitSettings::NEUTRAL,
                        &[],
                        &[],
                    )),
                    with(100.0, 0.0, 0.0),
                ],
                format!("{name}.png"),
            );
            sheet(
                &[
                    pasted(None),
                    with(0.0, 100.0, 0.0),
                    with(0.0, 0.0, -60.0),
                    with(0.0, 0.0, 60.0),
                ],
                format!("{name}_sang.png"),
            );
        }
    }
}

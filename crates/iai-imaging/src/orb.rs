//! ORB features (Rublee, Rabaud, Konolige and Bradski, "ORB: an efficient
//! alternative to SIFT or SURF", 2011): FAST corners over a pyramid, the
//! strongest by the Harris measure, each turned by its intensity centroid
//! and described by 256 comparisons of pixel pairs turned with it, so the
//! same spot of a picture is known again at another size and angle.

use std::sync::OnceLock;

use rayon::prelude::*;

/// Levels of the pyramid, each this much smaller than the one before.
const LEVELS: usize = 8;
const STEP: f32 = 1.2;
/// How much brighter or darker than the middle the ring must be.
const FAST_T: i16 = 10;
/// Kept clear at the edges of each level.
const BORDER: usize = 16;
/// The patch a corner is turned and described on: a circle this wide.
const HALF: i32 = 15;

/// A grey picture, a byte a pixel.
#[derive(Clone, Debug)]
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Gray {
    /// `rgba` (`width` × `height`, 4 bytes a pixel) made grey and
    /// box-filtered down so its longer side is `longest` at most; with how
    /// many of its pixels one of the copy's spans.
    pub fn of_rgba(rgba: &[u8], width: u32, height: u32, longest: usize) -> Option<(Self, f32)> {
        let (w, h) = (width as usize, height as usize);
        if w < 8 || h < 8 || rgba.len() < w * h * 4 {
            return None;
        }
        let scale = (w.max(h) as f32 / longest.max(8) as f32).max(1.0);
        let (sw, sh) = (
            ((w as f32 / scale).round() as usize).max(1),
            ((h as f32 / scale).round() as usize).max(1),
        );
        let px = (0..sh)
            .into_par_iter()
            .flat_map_iter(|y| {
                let y0 = ((y as f32 * scale) as usize).min(h - 1);
                let y1 = (((y + 1) as f32 * scale) as usize).clamp(y0 + 1, h);
                (0..sw).map(move |x| {
                    let x0 = ((x as f32 * scale) as usize).min(w - 1);
                    let x1 = (((x + 1) as f32 * scale) as usize).clamp(x0 + 1, w);
                    let mut sum = 0u32;
                    for yy in y0..y1 {
                        for xx in x0..x1 {
                            let o = (yy * w + xx) * 4;
                            // Rec. 601 luma, in 1/1024ths.
                            sum += (306 * rgba[o] as u32
                                + 601 * rgba[o + 1] as u32
                                + 117 * rgba[o + 2] as u32)
                                >> 10;
                        }
                    }
                    (sum / ((y1 - y0) * (x1 - x0)) as u32) as u8
                })
            })
            .collect();
        Some((Self { w: sw, h: sh, px }, scale))
    }

    fn at(&self, x: isize, y: isize) -> u8 {
        let x = x.clamp(0, self.w as isize - 1) as usize;
        let y = y.clamp(0, self.h as isize - 1) as usize;
        self.px[y * self.w + x]
    }

    /// Resampled to `w` × `h`, between pixel centres.
    fn resized(&self, w: usize, h: usize) -> Self {
        let (fx, fy) = (self.w as f32 / w as f32, self.h as f32 / h as f32);
        let px = (0..h)
            .into_par_iter()
            .flat_map_iter(|y| {
                let sy = ((y as f32 + 0.5) * fy - 0.5).max(0.0);
                let (y0, ty) = (sy.floor() as isize, sy.fract());
                (0..w).map(move |x| {
                    let sx = ((x as f32 + 0.5) * fx - 0.5).max(0.0);
                    let (x0, tx) = (sx.floor() as isize, sx.fract());
                    let p = |dx: isize, dy: isize| self.at(x0 + dx, y0 + dy) as f32;
                    let top = p(0, 0) + (p(1, 0) - p(0, 0)) * tx;
                    let bottom = p(0, 1) + (p(1, 1) - p(0, 1)) * tx;
                    (top + (bottom - top) * ty).round() as u8
                })
            })
            .collect();
        Self { w, h, px }
    }

    /// Smoothed by a 7-tap Gaussian of σ 2 each way, as the comparisons are
    /// made on.
    fn blurred(&self) -> Self {
        const K: [u32; 7] = [7, 26, 57, 76, 57, 26, 7];
        let (w, h) = (self.w, self.h);
        let across: Vec<u8> = (0..h)
            .into_par_iter()
            .flat_map_iter(|y| {
                (0..w).map(move |x| {
                    let s: u32 = (0..7)
                        .map(|k| K[k] * self.at(x as isize + k as isize - 3, y as isize) as u32)
                        .sum();
                    ((s + 128) >> 8) as u8
                })
            })
            .collect();
        let across = Self { w, h, px: across };
        let px = (0..h)
            .into_par_iter()
            .flat_map_iter(|y| {
                let across = &across;
                (0..w).map(move |x| {
                    let s: u32 = (0..7)
                        .map(|k| K[k] * across.at(x as isize, y as isize + k as isize - 3) as u32)
                        .sum();
                    ((s + 128) >> 8) as u8
                })
            })
            .collect();
        Self { w, h, px }
    }
}

/// A corner found: where (in the pixels of the picture it was found in),
/// how it is turned, and its 256 comparisons.
#[derive(Clone, Copy, Debug)]
pub struct Feature {
    pub x: f32,
    pub y: f32,
    pub angle: f32,
    pub bits: [u64; 4],
}

/// How many of their comparisons two features answer differently.
pub fn distance(a: &[u64; 4], b: &[u64; 4]) -> u32 {
    (0..4).map(|i| (a[i] ^ b[i]).count_ones()).sum()
}

/// The ring of 16 pixels three away from a corner, round from the top.
const RING: [(isize, isize); 16] = [
    (0, -3),
    (1, -3),
    (2, -2),
    (3, -1),
    (3, 0),
    (3, 1),
    (2, 2),
    (1, 3),
    (0, 3),
    (-1, 3),
    (-2, 2),
    (-3, 1),
    (-3, 0),
    (-3, -1),
    (-2, -2),
    (-1, -3),
];

/// The 256 pixel pairs compared, within the patch: drawn once from a
/// Gaussian of σ a fifth of the patch (Calonder et al., BRIEF, 2010,
/// "G II"), always the same.
fn pairs() -> &'static [[i8; 4]; 256] {
    static PAIRS: OnceLock<[[i8; 4]; 256]> = OnceLock::new();
    PAIRS.get_or_init(|| {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut uniform = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            ((state >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        };
        let sigma = (2 * HALF + 1) as f64 / 5.0;
        let mut out = [[0i8; 4]; 256];
        for pair in &mut out {
            for v in pair.iter_mut() {
                // Box–Muller, kept inside the patch.
                let (a, b) = (uniform(), uniform());
                let g = (-2.0 * a.ln()).sqrt() * (std::f64::consts::TAU * b).cos();
                *v = (g * sigma).round().clamp(-13.0, 13.0) as i8;
            }
        }
        out
    })
}

/// Half the width of the circular patch at each row from its middle.
fn circle() -> [i32; HALF as usize + 1] {
    let mut out = [0; HALF as usize + 1];
    for (dy, v) in out.iter_mut().enumerate() {
        let r = HALF as f32 + 0.5;
        *v = (r * r - (dy * dy) as f32).max(0.0).sqrt().floor() as i32;
    }
    out
}

/// FAST-9 corners of `img` that `keep` (in its own pixels) allows, the
/// strongest in their 3 × 3 neighbourhood, with their strength.
fn fast(img: &Gray, keep: &(dyn Fn(f32, f32) -> bool + Sync)) -> Vec<(usize, usize)> {
    let (w, h) = (img.w, img.h);
    if w <= 2 * BORDER || h <= 2 * BORDER {
        return Vec::new();
    }
    let score: Vec<u16> = (0..h)
        .into_par_iter()
        .flat_map_iter(|y| {
            (0..w).map(move |x| {
                if x < BORDER || y < BORDER || x + BORDER >= w || y + BORDER >= h {
                    return 0;
                }
                let p = img.px[y * w + x] as i16;
                let ring = |i: usize| {
                    let (dx, dy) = RING[i];
                    img.px[(y as isize + dy) as usize * w + (x as isize + dx) as usize] as i16
                };
                // Nine in a row of sixteen take in two of the four compass points.
                let (mut up, mut down) = (0, 0);
                for i in [0, 4, 8, 12] {
                    let v = ring(i);
                    up += usize::from(v > p + FAST_T);
                    down += usize::from(v < p - FAST_T);
                }
                if up < 2 && down < 2 {
                    return 0;
                }
                let (mut brighter, mut darker) = (0u32, 0u32);
                let (mut sum_up, mut sum_down) = (0u16, 0u16);
                for i in 0..16 {
                    let d = ring(i) - p;
                    if d > FAST_T {
                        brighter |= 1 << i;
                        sum_up += (d - FAST_T) as u16;
                    } else if d < -FAST_T {
                        darker |= 1 << i;
                        sum_down += (-d - FAST_T) as u16;
                    }
                }
                let nine = |m: u32| {
                    let m = m | (m << 16);
                    (0..9).fold(m, |acc, k| acc & (m >> k)) != 0
                };
                match (nine(brighter), nine(darker)) {
                    (true, _) => sum_up.max(1),
                    (_, true) => sum_down.max(1),
                    _ => 0,
                }
            })
        })
        .collect();
    (BORDER..h - BORDER)
        .into_par_iter()
        .flat_map_iter(|y| {
            let score = &score;
            (BORDER..w - BORDER).filter_map(move |x| {
                let s = score[y * w + x];
                if s == 0 || !keep(x as f32, y as f32) {
                    return None;
                }
                // Ties go to the first in reading order.
                for dy in -1..=1isize {
                    for dx in -1..=1isize {
                        let o = score[(y as isize + dy) as usize * w + (x as isize + dx) as usize];
                        let before = dy < 0 || (dy == 0 && dx < 0);
                        if o > s || (o == s && before && (dx, dy) != (0, 0)) {
                            return None;
                        }
                    }
                }
                Some((x, y))
            })
        })
        .collect()
}

/// The Harris measure at `(x, y)` over a 7 × 7 block.
fn harris(img: &Gray, x: usize, y: usize) -> f32 {
    let (mut xx, mut yy, mut xy) = (0.0f32, 0.0f32, 0.0f32);
    for dy in -3..=3isize {
        for dx in -3..=3isize {
            let (cx, cy) = (x as isize + dx, y as isize + dy);
            let p = |a: isize, b: isize| img.at(cx + a, cy + b) as f32;
            let gx = (p(1, -1) + 2.0 * p(1, 0) + p(1, 1)) - (p(-1, -1) + 2.0 * p(-1, 0) + p(-1, 1));
            let gy = (p(-1, 1) + 2.0 * p(0, 1) + p(1, 1)) - (p(-1, -1) + 2.0 * p(0, -1) + p(1, -1));
            xx += gx * gx;
            yy += gy * gy;
            xy += gx * gy;
        }
    }
    xx * yy - xy * xy - 0.04 * (xx + yy) * (xx + yy)
}

/// Up to `wanted` features of `gray`, where `keep` (in its pixels) allows.
pub fn features(
    gray: &Gray,
    wanted: usize,
    keep: &(dyn Fn(f32, f32) -> bool + Sync),
) -> Vec<Feature> {
    let circle = circle();
    let pairs = pairs();
    let f = 1.0 / STEP;
    let share = wanted as f32 * (1.0 - f) / (1.0 - f.powi(LEVELS as i32));
    let mut level = gray.clone();
    let mut out = Vec::new();
    for k in 0..LEVELS {
        if k > 0 {
            let s = STEP.powi(k as i32);
            let (w, h) = (
                (gray.w as f32 / s).round() as usize,
                (gray.h as f32 / s).round() as usize,
            );
            if w <= 2 * BORDER + 8 || h <= 2 * BORDER + 8 {
                break;
            }
            // Each from the one before: a step at a time leaves nothing out.
            level = level.resized(w, h);
        }
        let (sx, sy) = (
            gray.w as f32 / level.w as f32,
            gray.h as f32 / level.h as f32,
        );
        let to_gray = |x: f32, y: f32| ((x + 0.5) * sx - 0.5, (y + 0.5) * sy - 0.5);
        let here = |x: f32, y: f32| {
            let (gx, gy) = to_gray(x, y);
            keep(gx, gy)
        };
        let mut corners: Vec<(f32, usize, usize)> = fast(&level, &here)
            .into_par_iter()
            .map(|(x, y)| (harris(&level, x, y), x, y))
            .collect();
        corners.sort_by(|a, b| b.0.total_cmp(&a.0));
        corners.truncate((share * f.powi(k as i32)).round() as usize);
        let smooth = level.blurred();
        let found: Vec<Feature> = corners
            .into_par_iter()
            .map(|(_, x, y)| {
                let (mut m10, mut m01) = (0.0f32, 0.0f32);
                for dy in -HALF..=HALF {
                    let r = circle[dy.unsigned_abs() as usize];
                    for dx in -r..=r {
                        let v = level.at(x as isize + dx as isize, y as isize + dy as isize) as f32;
                        m10 += dx as f32 * v;
                        m01 += dy as f32 * v;
                    }
                }
                let angle = m01.atan2(m10);
                let (sin, cos) = angle.sin_cos();
                let at = |u: i8, v: i8| {
                    let (u, v) = (u as f32, v as f32);
                    let dx = (u * cos - v * sin).round() as isize;
                    let dy = (u * sin + v * cos).round() as isize;
                    smooth.at(x as isize + dx, y as isize + dy)
                };
                let mut bits = [0u64; 4];
                for (i, p) in pairs.iter().enumerate() {
                    if at(p[0], p[1]) < at(p[2], p[3]) {
                        bits[i / 64] |= 1 << (i % 64);
                    }
                }
                let (gx, gy) = to_gray(x as f32, y as f32);
                Feature {
                    x: gx,
                    y: gy,
                    angle,
                    bits,
                }
            })
            .collect();
        out.extend(found);
    }
    out
}

/// For each of `query`, the one of `train` it is like when it is clearly
/// more like that one than any other: its nearest is nearer than `ratio`
/// times the second nearest (Lowe's test). Pairs of indices.
pub fn matches(query: &[Feature], train: &[Feature], ratio: f32) -> Vec<(usize, usize)> {
    query
        .par_iter()
        .enumerate()
        .filter_map(|(i, q)| {
            let (mut best, mut second, mut at) = (u32::MAX, u32::MAX, 0);
            for (j, t) in train.iter().enumerate() {
                let d = distance(&q.bits, &t.bits);
                if d < best {
                    second = best;
                    best = d;
                    at = j;
                } else if d < second {
                    second = d;
                }
            }
            (second != u32::MAX && (best as f32) < ratio * second as f32).then_some((i, at))
        })
        .collect()
}

#[cfg(test)]
mod tests;

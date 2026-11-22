//! Skin Smoothing: an edge-aware softening limited to skin-coloured areas.
//! No machine learning.
//!
//! 1. A fast guided filter (He & Sun, 2015), each channel guiding itself,
//!    smooths flat areas and leaves strong edges (eyes, lips, hair) alone.
//!    Its coefficients are found on a copy shrunk with the radius, so a large
//!    radius costs no more than a small one.
//! 2. A soft skin mask from the colour (YCbCr): grey, blue, green and
//!    saturated red stay out; the mask is blurred with the coefficients, so
//!    it has no hard border.
//! 3. Detail: the fine part of what the smoothing removed (pores, texture
//!    finer than ~a quarter of the radius) is put back by that fraction, so
//!    the skin does not turn to plastic; blemishes and blotches stay gone.

use rayon::prelude::*;

use crate::filters::gaussian_blur_premul;

/// A channel value the filter reads and writes (8- or 16-bit).
trait Channel: Copy + Send + Sync + Into<f32> {
    const MAX: f32;
    fn from_unit(v: f32) -> Self;
}

impl Channel for u8 {
    const MAX: f32 = 255.0;
    fn from_unit(v: f32) -> Self {
        iai_raster::fast_math::round_u8(v * 255.0)
    }
}

impl Channel for u16 {
    const MAX: f32 = 65535.0;
    fn from_unit(v: f32) -> Self {
        (v * 65535.0).round().clamp(0.0, 65535.0) as u16
    }
}

/// Skin Smoothing on straight 8-bit RGBA. `amount` and `detail` are 0 to 100
/// (percent), `radius` is in pixels. Alpha is kept.
pub fn skin_smoothing(
    src: &[u8],
    w: u32,
    h: u32,
    amount: f32,
    radius: f32,
    detail: f32,
) -> Vec<u8> {
    smooth(src, w, h, amount, radius, detail)
}

/// 16-bit counterpart of [`skin_smoothing`].
pub fn skin_smoothing16(
    src: &[u16],
    w: u32,
    h: u32,
    amount: f32,
    radius: f32,
    detail: f32,
) -> Vec<u16> {
    smooth(src, w, h, amount, radius, detail)
}

/// How much of a colour (straight RGB in [0,1]) counts as skin, 0 to 1.
/// A soft ellipse around the skin cluster in BT.601 Cb/Cr, faded out in the
/// deep shadows where chroma says little.
pub fn skin_likelihood(r: f32, g: f32, b: f32) -> f32 {
    let y = 0.299 * r + 0.587 * g + 0.114 * b;
    let cb = 0.5 - 0.168_736 * r - 0.331_264 * g + 0.5 * b;
    let cr = 0.5 + 0.5 * r - 0.418_688 * g - 0.081_312 * b;
    let dcb = (cb - 0.40) / 0.11;
    let dcr = (cr - 0.60) / 0.09;
    let d = (dcb * dcb + dcr * dcr).sqrt();
    let chroma = 1.0 - smoothstep(0.8, 1.3, d);
    let light = smoothstep(0.08, 0.20, y);
    chroma * light
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Guided-filter coefficients on the shrunk grid: per pixel `a` (RGB, plus
/// the skin mask in the fourth slot) and `b` (RGB), both already averaged.
struct Coeffs {
    a: Vec<f32>,
    b: Vec<f32>,
    lw: usize,
    lh: usize,
    step: usize,
}

impl Coeffs {
    /// Bilinear look-up of the coefficients at full-resolution pixel (x, y):
    /// `(a, b)` with the mask in `a[3]`.
    fn at(&self, x: usize, y: usize) -> ([f32; 4], [f32; 4]) {
        let s = self.step as f32;
        let fx = ((x as f32 + 0.5) / s - 0.5).clamp(0.0, (self.lw - 1) as f32);
        let fy = ((y as f32 + 0.5) / s - 0.5).clamp(0.0, (self.lh - 1) as f32);
        let x0 = fx.floor() as usize;
        let y0 = fy.floor() as usize;
        let x1 = (x0 + 1).min(self.lw - 1);
        let y1 = (y0 + 1).min(self.lh - 1);
        let tx = fx - x0 as f32;
        let ty = fy - y0 as f32;
        let w00 = (1.0 - tx) * (1.0 - ty);
        let w10 = tx * (1.0 - ty);
        let w01 = (1.0 - tx) * ty;
        let w11 = tx * ty;
        let i00 = (y0 * self.lw + x0) * 4;
        let i10 = (y0 * self.lw + x1) * 4;
        let i01 = (y1 * self.lw + x0) * 4;
        let i11 = (y1 * self.lw + x1) * 4;
        let mut a = [0.0f32; 4];
        let mut b = [0.0f32; 4];
        for c in 0..4 {
            a[c] = self.a[i00 + c] * w00
                + self.a[i10 + c] * w10
                + self.a[i01 + c] * w01
                + self.a[i11 + c] * w11;
            b[c] = self.b[i00 + c] * w00
                + self.b[i10 + c] * w10
                + self.b[i01 + c] * w01
                + self.b[i11 + c] * w11;
        }
        (a, b)
    }
}

/// The shrunk copy (alpha-weighted box average of `step`×`step` blocks) and
/// the guided-filter maths on it.
fn coefficients<T: Channel>(src: &[T], wi: usize, hi: usize, radius: f32, eps: f32) -> Coeffs {
    let step = ((radius / 3.0).floor() as usize).clamp(1, 16);
    let lw = wi.div_ceil(step);
    let lh = hi.div_ceil(step);
    let n = lw * lh * 4;
    let norm = 1.0 / T::MAX;

    let mut mean = vec![0f32; n];
    mean.par_chunks_mut(lw * 4)
        .enumerate()
        .for_each(|(ly, row)| {
            let y0 = ly * step;
            let y1 = (y0 + step).min(hi);
            for lx in 0..lw {
                let x0 = lx * step;
                let x1 = (x0 + step).min(wi);
                let mut sum = [0f32; 3];
                let mut wsum = 0f32;
                for y in y0..y1 {
                    for x in x0..x1 {
                        let i = (y * wi + x) * 4;
                        let a = src[i + 3].into() * norm;
                        for c in 0..3 {
                            sum[c] += src[i + c].into() * norm * a;
                        }
                        wsum += a;
                    }
                }
                let o = lx * 4;
                if wsum > 1e-6 {
                    for c in 0..3 {
                        row[o + c] = sum[c] / wsum;
                    }
                }
            }
        });

    let mut corr: Vec<f32> = mean.par_iter().map(|v| v * v).collect();
    let mut tmp = vec![0f32; n];
    let sigma = (radius / step as f32).max(0.5);
    gaussian_blur_premul(&mut mean, &mut tmp, lw, lh, sigma);
    gaussian_blur_premul(&mut corr, &mut tmp, lw, lh, sigma);

    // a = var / (var + eps), b = mean - a * mean, per channel; the skin mask
    // of the local mean rides in a[3] and is averaged with a.
    let mut a = corr;
    let mut b = vec![0f32; n];
    a.par_chunks_mut(4)
        .zip(b.par_chunks_mut(4))
        .zip(mean.par_chunks(4))
        .for_each(|((ap, bp), mp)| {
            for c in 0..3 {
                let var = (ap[c] - mp[c] * mp[c]).max(0.0);
                let k = var / (var + eps);
                ap[c] = k;
                bp[c] = mp[c] - k * mp[c];
            }
            ap[3] = skin_likelihood(mp[0], mp[1], mp[2]);
            bp[3] = 0.0;
        });
    gaussian_blur_premul(&mut a, &mut tmp, lw, lh, sigma);
    gaussian_blur_premul(&mut b, &mut tmp, lw, lh, sigma);
    Coeffs { a, b, lw, lh, step }
}

fn smooth<T: Channel>(src: &[T], w: u32, h: u32, amount: f32, radius: f32, detail: f32) -> Vec<T> {
    let wi = w as usize;
    let hi = h as usize;
    let s = (amount / 100.0).clamp(0.0, 1.0);
    let d = (detail / 100.0).clamp(0.0, 1.0);
    if s <= 0.001 || radius < 0.5 || wi == 0 || hi == 0 || src.len() < wi * hi * 4 {
        return src.to_vec();
    }
    let norm = 1.0 / T::MAX;
    // A stronger amount also lets larger steps of tone count as "flat".
    let tol = 0.02 + 0.05 * s;
    let co = coefficients(src, wi, hi, radius, tol * tol);

    // Residual r = I - q of the guided filter, then its low band G(r).
    let mut low = vec![0f32; wi * hi * 4];
    low.par_chunks_mut(wi * 4).enumerate().for_each(|(y, row)| {
        for x in 0..wi {
            let i = (y * wi + x) * 4;
            let (a, b) = co.at(x, y);
            for c in 0..3 {
                let v = src[i + c].into() * norm;
                row[x * 4 + c] = v - (a[c] * v + b[c]);
            }
        }
    });
    let mut tmp = vec![0f32; wi * hi * 4];
    let fine_sigma = (radius * 0.25).clamp(0.8, 3.0);
    gaussian_blur_premul(&mut low, &mut tmp, wi, hi, fine_sigma);
    drop(tmp);

    // out = I + k * (smoothed - I), smoothed = q + d * (r - G(r))
    //     = I - k * ((1 - d) * r + d * G(r)), k = amount * skin mask.
    let mut out = src.to_vec();
    out.par_chunks_mut(wi * 4).enumerate().for_each(|(y, row)| {
        for x in 0..wi {
            let i = (y * wi + x) * 4;
            let (a, b) = co.at(x, y);
            let k = s * a[3];
            if k <= 1e-4 {
                continue;
            }
            for c in 0..3 {
                let v = src[i + c].into() * norm;
                let r = v - (a[c] * v + b[c]);
                let g = low[i + c];
                let o = v - k * ((1.0 - d) * r + d * g);
                row[x * 4 + c] = T::from_unit(o.clamp(0.0, 1.0));
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SKIN: [f32; 3] = [224.0, 172.0, 140.0];
    const HAIR: [f32; 3] = [40.0, 28.0, 22.0];

    fn noise(x: u32, y: u32, c: u32) -> f32 {
        let mut h = x
            .wrapping_mul(0x9e37_79b9)
            .wrapping_add(y.wrapping_mul(0x85eb_ca6b))
            .wrapping_add(c.wrapping_mul(0xc2b2_ae35));
        h ^= h >> 15;
        h = h.wrapping_mul(0x2c1b_3c6d);
        h ^= h >> 13;
        (h as f32 / u32::MAX as f32 - 0.5) * 2.0
    }

    /// A `w`×`h` image of `base(x)` with ±`amp` grain on every channel.
    fn noisy(w: u32, h: u32, amp: f32, base: impl Fn(u32) -> [f32; 3]) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let px = base(x);
                for c in 0..3 {
                    let n = noise(x, y, c as u32) * amp;
                    v.push((px[c] + n).round().clamp(0.0, 255.0) as u8);
                }
                v.push(255);
            }
        }
        v
    }

    /// Standard deviation of channel `c` over the columns `x0..x1`.
    fn spread(px: &[u8], w: u32, x0: u32, x1: u32, c: usize) -> f32 {
        let vals: Vec<f32> = px
            .chunks(4)
            .enumerate()
            .filter(|(i, _)| {
                let x = *i as u32 % w;
                x >= x0 && x < x1
            })
            .map(|(_, p)| p[c] as f32)
            .collect();
        let m = vals.iter().sum::<f32>() / vals.len() as f32;
        (vals.iter().map(|v| (v - m) * (v - m)).sum::<f32>() / vals.len() as f32).sqrt()
    }

    #[test]
    fn skin_colours_count_and_others_do_not() {
        let u = |c: [f32; 3]| skin_likelihood(c[0] / 255.0, c[1] / 255.0, c[2] / 255.0);
        assert!(u(SKIN) > 0.95);
        assert!(u([141.0, 85.0, 36.0]) > 0.9, "dark skin");
        assert!(u([128.0, 128.0, 128.0]) < 0.01, "grey");
        assert!(u([40.0, 90.0, 200.0]) < 0.01, "blue");
        assert!(u([60.0, 170.0, 70.0]) < 0.01, "green");
        assert!(u([10.0, 8.0, 6.0]) < 0.01, "black");
    }

    #[test]
    fn noise_on_skin_is_reduced() {
        let (w, h) = (64u32, 64u32);
        let src = noisy(w, h, 12.0, |_| SKIN);
        let out = skin_smoothing(&src, w, h, 80.0, 6.0, 0.0);
        for c in 0..3 {
            let before = spread(&src, w, 0, w, c);
            let after = spread(&out, w, 0, w, c);
            assert!(after < before * 0.5, "channel {c}: {before} -> {after}");
        }
    }

    #[test]
    fn detail_keeps_some_texture() {
        let (w, h) = (64u32, 64u32);
        let src = noisy(w, h, 12.0, |_| SKIN);
        let flat = skin_smoothing(&src, w, h, 80.0, 6.0, 0.0);
        let kept = skin_smoothing(&src, w, h, 80.0, 6.0, 100.0);
        assert!(spread(&kept, w, 0, w, 0) > spread(&flat, w, 0, w, 0) * 1.5);
    }

    #[test]
    fn non_skin_is_left_alone() {
        let (w, h) = (48u32, 48u32);
        for base in [
            [40.0, 90.0, 200.0],
            [128.0, 128.0, 128.0],
            [60.0, 170.0, 70.0],
        ] {
            let src = noisy(w, h, 12.0, |_| base);
            let out = skin_smoothing(&src, w, h, 100.0, 8.0, 30.0);
            let worst = src
                .iter()
                .zip(&out)
                .map(|(a, b)| (*a as i32 - *b as i32).abs())
                .max()
                .unwrap_or(0);
            assert!(worst <= 1, "{base:?}: moved by {worst}");
        }
    }

    #[test]
    fn a_strong_edge_stays_sharp() {
        let (w, h) = (64u32, 32u32);
        let edge = |x: u32| if x < w / 2 { SKIN } else { HAIR };
        let src = noisy(w, h, 6.0, edge);
        let out = skin_smoothing(&src, w, h, 100.0, 8.0, 0.0);
        let y = h / 2;
        let at = |px: &[u8], x: u32| px[((y * w + x) * 4) as usize] as f32;
        // Two pixels either side of the edge keep nearly the full step.
        let step_before = at(&src, w / 2 - 2) - at(&src, w / 2 + 1);
        let step_after = at(&out, w / 2 - 2) - at(&out, w / 2 + 1);
        assert!(
            step_after > step_before * 0.85,
            "edge {step_before} -> {step_after}"
        );
        // The skin side is still smoothed.
        assert!(spread(&out, w, 4, w / 2 - 6, 0) < spread(&src, w, 4, w / 2 - 6, 0) * 0.6);
    }

    #[test]
    fn alpha_is_kept_and_zero_amount_is_a_no_op() {
        let (w, h) = (16u32, 16u32);
        let mut src = noisy(w, h, 10.0, |_| SKIN);
        for (i, p) in src.chunks_mut(4).enumerate() {
            p[3] = (i * 7 % 256) as u8;
        }
        let out = skin_smoothing(&src, w, h, 100.0, 5.0, 30.0);
        assert!(src.chunks(4).zip(out.chunks(4)).all(|(a, b)| a[3] == b[3]));
        assert_eq!(skin_smoothing(&src, w, h, 0.0, 5.0, 30.0), src);
    }

    #[test]
    fn sixteen_bit_matches_eight_bit() {
        let (w, h) = (32u32, 32u32);
        let src = noisy(w, h, 10.0, |_| SKIN);
        let src16: Vec<u16> = src.iter().map(|&v| v as u16 * 257).collect();
        let out8 = skin_smoothing(&src, w, h, 70.0, 5.0, 30.0);
        let out16 = skin_smoothing16(&src16, w, h, 70.0, 5.0, 30.0);
        for (a, b) in out8.iter().zip(&out16) {
            assert!((*a as f32 - *b as f32 / 257.0).abs() <= 1.0);
        }
    }

    #[test]
    fn tiny_images_do_not_panic() {
        for (w, h) in [(1u32, 1u32), (1, 7), (5, 1), (3, 3)] {
            let src = noisy(w, h, 10.0, |_| SKIN);
            let out = skin_smoothing(&src, w, h, 100.0, 50.0, 30.0);
            assert_eq!(out.len(), src.len());
        }
    }
}

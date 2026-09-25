//! Camera Raw–style Detail core: Sharpening, Luminance and Colour Noise
//! Reduction on a gamma-encoded luma/chroma split.
//!
//! Tuned against Camera Raw 16 renders of a synthetic chart (step edges,
//! gratings, luma/chroma noise at several tones and grain sizes), so each
//! slider answers like its Camera Raw namesake:
//!   • Sharpening — difference-of-Gaussians unsharp mask. Radius sets both
//!     Gaussians, Detail below 25 pulls the result back inside the local
//!     min/max (halo suppression) and above 25 adds a fine-band boost; Masking
//!     gates on the gradient of a softened luma; shadows and near-white fade
//!     the effect out; a tanh limit bounds extreme overshoot.
//!   • Noise Reduction — edge-aware à-trous shrink. The amount blends every
//!     scale toward its smoothed value (no hard threshold, so texture fades
//!     gradually instead of falling off a cliff); Detail sets the range sigma
//!     of the guide (how much contrast counts as structure); Contrast restores
//!     the coarser scales.
//!   • Colour Noise Reduction — joint-chroma edge-aware à-trous. Fine speckle
//!     goes at any non-zero amount, the amount widens the reach into coarser
//!     blotches, Smoothness pushes the coarsest scales, Detail keeps a share of
//!     the fine chroma, and strong chroma features are protected.
//!
//! [`DetailPlan`] folds the sliders once; `gpu/detail.wgsl` consumes the same
//! plan so the live preview matches the commit.

use super::*;
use rayon::prelude::*;

/// Wavelet depth for both noise reducers (scales 1, 2, 4, 8, 16 px).
pub(crate) const DETAIL_LEVELS: usize = 5;
/// Widest dependency of the whole Detail chain: à-trous reach
/// ±2·(1+2+4+8+16) = ±62 px plus the sharpening blur and mask gradient.
pub(crate) const DETAIL_HALO: usize = 72;

// Sharpening.
const SH_K0: f32 = 0.542;
const SH_KP: f32 = 1.382;
const SH_KR: f32 = -0.487;
const SH_SA0: f32 = 0.224;
const SH_SAP: f32 = 0.988;
const SH_SB0: f32 = 0.879;
const SH_SBP: f32 = 0.700;
const SH_FINE0: f32 = 1.944;
const SH_FINE_POW: f32 = 0.843;
const SH_FINE_SIGMA: f32 = 0.531;
pub(crate) const SH_LIMIT: f32 = 0.12;
pub(crate) const SH_SHADOW_KNEE: f32 = 0.276;
pub(crate) const SH_SHADOW_POW: f32 = 1.51;
pub(crate) const SH_HIGHLIGHT_CUT: f32 = 0.505;
const SH_HALO_POW: f32 = 0.923;
pub(crate) const SH_HALO_MARGIN: f32 = 0.090;
const SH_MASK_T0: f32 = 0.0012;
const SH_MASK_RATE: f32 = 0.0415;
const SH_MASK_SIGMA: f32 = 1.259;
const SH_MASK_SOFT: f32 = 0.99;

// Luminance noise reduction.
const NR_W: [f32; DETAIL_LEVELS] = [1.0, 1.0, 0.448, 1.078, 0.641];
const NR_GROW_POW: f32 = 0.855;
const NR_CONTRAST: f32 = 0.776;
const NR_RANGE50: f32 = 0.0263;
const NR_RANGE_EXP: f32 = 5.14;
const NR_RANGE_LEVEL: f32 = 0.5485;
pub(crate) const NR_HIGHLIGHT_CUT: f32 = 0.5;

// Colour noise reduction.
const CNR_FINE_RAMP: f32 = 5.0;
const CNR_LAMBDA: [f32; 3] = [8.0, 12.0, 14.0];
const CNR_SAT: [f32; 3] = [0.9, 1.0, 0.95];
const CNR_FINE_KEEP_L2: f32 = 0.5;
const CNR_SMOOTH_LO: f32 = 0.4;
const CNR_SMOOTH_HI: f32 = 1.6;
const CNR_TAU50: f32 = 0.13;
const CNR_TAU_AMOUNT: f32 = 0.5;
const CNR_TAU_LEVEL: f32 = 0.7;
const CNR_RANGE0: f32 = 0.12;
const CNR_RANGE: f32 = 0.07;
pub(crate) const CNR_HIGHLIGHT_CUT: f32 = 0.25;
/// Isolated single-pixel chroma outliers (a pixel unlike all 8 neighbours
/// while they agree) are replaced by the neighbour mean: Camera Raw removes
/// such specks at any strength yet keeps 2 px colour lines and small dots.
pub(crate) const CNR_SPECK_SPREAD: f32 = 1.5;
pub(crate) const CNR_SPECK_FLOOR: f32 = 0.01;
pub(crate) const CNR_SPECK_WIDTH: f32 = 0.03;
/// Share of fine chroma kept at Detail 0/25/50/75/100.
const CNR_KEEP: [f32; 5] = [0.13, 0.16, 0.21, 0.41, 0.74];

pub(crate) const B3: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];

/// Every slider folded to working numbers. At `scale == 1` (every
/// full-resolution path and the GPU preview) the numbers are exactly the
/// tuned ones; a reduced live-preview proxy passes its downsample so radii and
/// wavelet scales are re-expressed in proxy pixels.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DetailPlan {
    pub sharpen: bool,
    pub k: f32,
    pub k_fine: f32,
    pub sigma_a: f32,
    pub sigma_b: f32,
    pub sigma_fine: f32,
    pub mask: bool,
    pub mask_sigma: f32,
    pub mask_lo: f32,
    pub mask_hi: f32,
    pub halo: bool,
    pub halo_r: u32,
    pub halo_h: f32,

    pub lnr: bool,
    pub lnr_alpha: f32,
    pub lnr_w: [f32; DETAIL_LEVELS],
    pub lnr_sigma: [f32; DETAIL_LEVELS],

    pub cnr: bool,
    pub cnr_speck: f32,
    pub cnr_a: [f32; DETAIL_LEVELS],
    pub cnr_tau: [f32; DETAIL_LEVELS],
    pub cnr_sigma: [f32; DETAIL_LEVELS],

    pub defringe: f32,
}

#[inline]
pub(crate) fn smooth01(x: f32) -> f32 {
    let t = x.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Sample a per-level table at a fractional level, fading to `beyond` past
/// the last level.
fn level_lerp(table: &[f32; DETAIL_LEVELS], level: f32, beyond: f32) -> f32 {
    if level <= 0.0 {
        return table[0];
    }
    let i = level.floor() as usize;
    let f = level - i as f32;
    let at = |j: usize| if j < DETAIL_LEVELS { table[j] } else { beyond };
    at(i) + (at(i + 1) - at(i)) * f
}

impl DetailPlan {
    pub(crate) fn new(settings: &DevelopSettings, scale: f32) -> Self {
        let s = scale.max(1.0);
        let shift = s.log2();

        // Sharpening.
        let amount = settings.sharpening.clamp(0.0, 150.0);
        let radius = settings.sharpen_radius.clamp(0.5, 3.0);
        let detail = settings.sharpen_detail.clamp(0.0, 100.0) / 100.0;
        let masking = settings.sharpen_masking.clamp(0.0, 100.0);
        let sharpen = amount > 0.001;
        let a25 = (amount / 25.0).powf(SH_KP);
        let k = SH_K0 * a25 * radius.powf(SH_KR);
        let k_fine = SH_FINE0 * a25 * (detail - 0.25).max(0.0).powf(SH_FINE_POW);
        let mask = masking > 0.001;
        let th = SH_MASK_T0 * (SH_MASK_RATE * masking).exp() * s;

        // Luminance NR.
        let nr = settings.noise_reduction.clamp(0.0, 100.0);
        let lnr = nr > 0.001;
        let lnr_alpha = if nr <= 25.0 {
            nr / 50.0
        } else {
            0.5 + (nr - 25.0) / 150.0
        };
        let grow = lnr_alpha.min(1.0).powf(NR_GROW_POW);
        let contrast = settings.noise_reduction_contrast.clamp(0.0, 100.0) / 100.0;
        let mut w_src = NR_W;
        for (j, w) in w_src.iter_mut().enumerate() {
            if j >= 3 {
                *w *= grow;
            }
            if j >= 2 {
                *w *= 1.0 - NR_CONTRAST * contrast;
            }
        }
        let nr_detail = settings.noise_reduction_detail.clamp(0.0, 100.0) / 100.0;
        let range = NR_RANGE50 * (NR_RANGE_EXP * (0.5 - nr_detail)).exp();
        let lnr_w = std::array::from_fn(|j| level_lerp(&w_src, j as f32 + shift, 0.0));
        let lnr_sigma = std::array::from_fn(|j| range * NR_RANGE_LEVEL.powf(j as f32 + shift));

        // Colour NR.
        let cn = settings.color_noise_reduction.clamp(0.0, 100.0);
        let cnr = cn > 0.001;
        let cdetail = settings.color_noise_detail.clamp(0.0, 100.0);
        let keep = {
            let x = cdetail / 25.0;
            let i = (x.floor() as usize).min(3);
            let f = (x - i as f32).clamp(0.0, 1.0);
            CNR_KEEP[i] + (CNR_KEEP[i + 1] - CNR_KEEP[i]) * f
        };
        let hill = |lam: f32| {
            let x = (cn / lam).powi(2);
            x / (1.0 + x)
        };
        let fine = (cn / CNR_FINE_RAMP).min(1.0) * (1.0 - keep);
        let sm = CNR_SMOOTH_LO
            + (CNR_SMOOTH_HI - CNR_SMOOTH_LO) * settings.color_noise_smoothness.clamp(0.0, 100.0)
                / 100.0;
        let a_src: [f32; DETAIL_LEVELS] = [
            fine,
            fine,
            CNR_SAT[0] * hill(CNR_LAMBDA[0]) * (1.0 - CNR_FINE_KEEP_L2 * keep),
            (CNR_SAT[1] * hill(CNR_LAMBDA[1]) * (0.5 + 0.5 * sm)).min(1.0),
            (CNR_SAT[2] * hill(CNR_LAMBDA[2]) * sm).min(1.0),
        ];
        let tau = CNR_TAU50 * (1.0 + CNR_TAU_AMOUNT * cn / 100.0);
        let sigma_src: [f32; DETAIL_LEVELS] =
            [CNR_RANGE0, CNR_RANGE, CNR_RANGE, CNR_RANGE, CNR_RANGE];
        let cnr_a = std::array::from_fn(|j| level_lerp(&a_src, j as f32 + shift, 0.0));
        let cnr_tau = std::array::from_fn(|j| tau * CNR_TAU_LEVEL.powf(j as f32 + shift));
        let cnr_sigma =
            std::array::from_fn(|j| level_lerp(&sigma_src, j as f32 + shift, CNR_RANGE));

        Self {
            sharpen,
            k,
            k_fine,
            sigma_a: SH_SA0 * radius.powf(SH_SAP) / s,
            sigma_b: SH_SB0 * radius.powf(SH_SBP) / s,
            sigma_fine: SH_FINE_SIGMA / s,
            mask,
            mask_sigma: SH_MASK_SIGMA / s,
            mask_lo: th * (1.0 - SH_MASK_SOFT),
            mask_hi: th * (1.0 + SH_MASK_SOFT),
            halo: detail < 0.25,
            halo_r: ((radius / s).round() as u32).max(1),
            halo_h: (detail / 0.25).clamp(0.0, 1.0).powf(SH_HALO_POW),
            lnr,
            lnr_alpha,
            lnr_w,
            lnr_sigma,
            cnr,
            // Specks are sub-pixel on a coarse proxy; despeckling there would
            // eat small real features instead.
            cnr_speck: (cn / CNR_FINE_RAMP).min(1.0) * (2.0 - s).clamp(0.0, 1.0),
            cnr_a,
            cnr_tau,
            cnr_sigma,
            defringe: (settings.defringe / 100.0).clamp(0.0, 1.0),
        }
    }
}

/// Normalised Gaussian taps, radius ⌊3σ + ½⌋ (a single tap when σ is tiny).
pub(crate) fn gauss_taps(sigma: f32) -> Vec<f32> {
    let r = (3.0 * sigma + 0.5).floor().max(0.0) as i32;
    if r == 0 || sigma <= 1e-6 {
        return vec![1.0];
    }
    let mut taps: Vec<f32> = (-r..=r)
        .map(|x| (-0.5 * (x * x) as f32 / (sigma * sigma)).exp())
        .collect();
    let sum: f32 = taps.iter().sum();
    for t in &mut taps {
        *t /= sum;
    }
    taps
}

/// Separable edge-clamped Gaussian blur (vertical pass, then horizontal).
pub(crate) fn gauss_blur(src: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    let taps = gauss_taps(sigma);
    if taps.len() == 1 {
        return src.to_vec();
    }
    let r = (taps.len() / 2) as i64;
    let mut tmp = vec![0.0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let mut acc = 0.0;
            for (t, &k) in taps.iter().enumerate() {
                let sy = (y as i64 + t as i64 - r).clamp(0, h as i64 - 1) as usize;
                acc += src[sy * w + x] * k;
            }
            *o = acc;
        }
    });
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let line = &tmp[y * w..(y + 1) * w];
        for (x, o) in row.iter_mut().enumerate() {
            let mut acc = 0.0;
            for (t, &k) in taps.iter().enumerate() {
                let sx = (x as i64 + t as i64 - r).clamp(0, w as i64 - 1) as usize;
                acc += line[sx] * k;
            }
            *o = acc;
        }
    });
    out
}

/// Square-window local minimum and maximum (radius `r`, edge-clamped).
fn local_min_max(src: &[f32], w: usize, h: usize, r: usize) -> (Vec<f32>, Vec<f32>) {
    let r = r as i64;
    let mut tlo = vec![0.0f32; w * h];
    let mut thi = vec![0.0f32; w * h];
    tlo.par_chunks_mut(w)
        .zip(thi.par_chunks_mut(w))
        .enumerate()
        .for_each(|(y, (lo_row, hi_row))| {
            let line = &src[y * w..(y + 1) * w];
            for x in 0..w {
                let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
                for o in -r..=r {
                    let v = line[(x as i64 + o).clamp(0, w as i64 - 1) as usize];
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
                lo_row[x] = lo;
                hi_row[x] = hi;
            }
        });
    let mut lo = vec![0.0f32; w * h];
    let mut hi = vec![0.0f32; w * h];
    lo.par_chunks_mut(w)
        .zip(hi.par_chunks_mut(w))
        .enumerate()
        .for_each(|(y, (lo_row, hi_row))| {
            for x in 0..w {
                let (mut a, mut b) = (f32::INFINITY, f32::NEG_INFINITY);
                for o in -r..=r {
                    let sy = (y as i64 + o).clamp(0, h as i64 - 1) as usize;
                    a = a.min(tlo[sy * w + x]);
                    b = b.max(thi[sy * w + x]);
                }
                lo_row[x] = a;
                hi_row[x] = b;
            }
        });
    (lo, hi)
}

/// One guided à-trous pass at hole spacing `1 << level`: the signal is
/// smoothed with B3 taps range-weighted by the guide's difference from the
/// centre (Gaussian, `sigma`), while the guide itself smooths plainly so the
/// next level compares like with like. Horizontal, then vertical.
fn guided_atrous(
    src: &[f32],
    guide: &[f32],
    w: usize,
    h: usize,
    level: usize,
    sigma: f32,
) -> (Vec<f32>, Vec<f32>) {
    let step = 1i64 << level;
    let inv = -0.5 / (sigma * sigma);
    let pass = |sig: &[f32], gd: &[f32], horizontal: bool| -> (Vec<f32>, Vec<f32>) {
        let mut out = vec![0.0f32; w * h];
        let mut gout = vec![0.0f32; w * h];
        out.par_chunks_mut(w)
            .zip(gout.par_chunks_mut(w))
            .enumerate()
            .for_each(|(y, (orow, grow))| {
                for x in 0..w {
                    let i = y * w + x;
                    let gc = gd[i];
                    let (mut acc, mut ws, mut gacc) = (0.0f32, 0.0f32, 0.0f32);
                    for (t, &kv) in B3.iter().enumerate() {
                        let o = (t as i64 - 2) * step;
                        let j = if horizontal {
                            y * w + (x as i64 + o).clamp(0, w as i64 - 1) as usize
                        } else {
                            (y as i64 + o).clamp(0, h as i64 - 1) as usize * w + x
                        };
                        let d = gd[j] - gc;
                        let wt = kv * (d * d * inv).exp();
                        acc += sig[j] * wt;
                        ws += wt;
                        gacc += gd[j] * kv;
                    }
                    orow[x] = acc / ws.max(1e-12);
                    grow[x] = gacc;
                }
            });
        (out, gout)
    };
    let (s1, g1) = pass(src, guide, true);
    pass(&s1, &g1, false)
}

/// One joint-chroma à-trous pass: taps are range-weighted by the Euclidean
/// distance of their chroma vector from the centre's.
fn chroma_atrous(src: &[[f32; 3]], w: usize, h: usize, level: usize, sigma: f32) -> Vec<[f32; 3]> {
    let step = 1i64 << level;
    let inv = -0.5 / (sigma * sigma);
    let pass = |inp: &[[f32; 3]], horizontal: bool| -> Vec<[f32; 3]> {
        let mut out = vec![[0.0f32; 3]; w * h];
        out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for x in 0..w {
                let c = inp[y * w + x];
                let (mut acc, mut ws) = ([0.0f32; 3], 0.0f32);
                for (t, &kv) in B3.iter().enumerate() {
                    let o = (t as i64 - 2) * step;
                    let j = if horizontal {
                        y * w + (x as i64 + o).clamp(0, w as i64 - 1) as usize
                    } else {
                        (y as i64 + o).clamp(0, h as i64 - 1) as usize * w + x
                    };
                    let v = inp[j];
                    let d2 = (v[0] - c[0]).powi(2) + (v[1] - c[1]).powi(2) + (v[2] - c[2]).powi(2);
                    let wt = kv * (d2 * inv).exp();
                    acc[0] += v[0] * wt;
                    acc[1] += v[1] * wt;
                    acc[2] += v[2] * wt;
                    ws += wt;
                }
                let ws = ws.max(1e-12);
                row[x] = [acc[0] / ws, acc[1] / ws, acc[2] / ws];
            }
        });
        out
    };
    let tmp = pass(src, true);
    pass(&tmp, false)
}

/// Replace isolated chroma outliers by their 8-neighbour mean (see
/// `CNR_SPECK_*`), weighted by `amount`.
fn despeckle(src: &[[f32; 3]], w: usize, h: usize, amount: f32) -> Vec<[f32; 3]> {
    let mut out = vec![[0.0f32; 3]; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let c = src[y * w + x];
            let mut nb = [[0.0f32; 3]; 8];
            let mut k = 0;
            for dy in -1i64..=1 {
                for dx in -1i64..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let sy = (y as i64 + dy).clamp(0, h as i64 - 1) as usize;
                    let sx = (x as i64 + dx).clamp(0, w as i64 - 1) as usize;
                    nb[k] = src[sy * w + sx];
                    k += 1;
                }
            }
            let mut mean = [0.0f32; 3];
            for v in &nb {
                for ch in 0..3 {
                    mean[ch] += v[ch] / 8.0;
                }
            }
            let (mut spread, mut dmin) = (0.0f32, f32::INFINITY);
            for v in &nb {
                let dm =
                    (v[0] - mean[0]).powi(2) + (v[1] - mean[1]).powi(2) + (v[2] - mean[2]).powi(2);
                spread += dm / 8.0;
                let dc = (v[0] - c[0]).powi(2) + (v[1] - c[1]).powi(2) + (v[2] - c[2]).powi(2);
                dmin = dmin.min(dc);
            }
            let excess = dmin.sqrt() - CNR_SPECK_SPREAD * spread.sqrt() - CNR_SPECK_FLOOR;
            let t = amount * smooth01(excess / CNR_SPECK_WIDTH);
            *o = [
                c[0] + t * (mean[0] - c[0]),
                c[1] + t * (mean[1] - c[1]),
                c[2] + t * (mean[2] - c[2]),
            ];
        }
    });
    out
}

/// Colour noise reduction over the chroma offsets; `luma` (pre-NR) drives the
/// near-white taper.
pub(crate) fn colour_nr(chroma: &mut [[f32; 3]], luma: &[f32], w: usize, h: usize, p: &DetailPlan) {
    let mut cur = if p.cnr_speck > 0.0 {
        despeckle(chroma, w, h, p.cnr_speck)
    } else {
        chroma.to_vec()
    };
    // The caller's buffer doubles as the accumulator: `cur` holds the signal.
    let acc = chroma;
    acc.par_iter_mut().for_each(|o| *o = [0.0; 3]);
    for lev in 0..DETAIL_LEVELS {
        let a = p.cnr_a[lev];
        let next = chroma_atrous(&cur, w, h, lev, p.cnr_sigma[lev]);
        let tau = p.cnr_tau[lev].max(1e-6);
        acc.par_iter_mut().enumerate().for_each(|(i, o)| {
            let d = [
                cur[i][0] - next[i][0],
                cur[i][1] - next[i][1],
                cur[i][2] - next[i][2],
            ];
            let keep = if a > 0.0 {
                let m = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() / tau;
                let m2 = m * m;
                let prot = 1.0 / (1.0 + m2 * m2);
                let taper = 1.0 - CNR_HIGHLIGHT_CUT * smooth01((luma[i] - 0.55) / 0.4);
                1.0 - (a * prot * taper).min(1.0)
            } else {
                1.0
            };
            o[0] += d[0] * keep;
            o[1] += d[1] * keep;
            o[2] += d[2] * keep;
        });
        cur = next;
    }
    acc.par_iter_mut().enumerate().for_each(|(i, c)| {
        c[0] += cur[i][0];
        c[1] += cur[i][1];
        c[2] += cur[i][2];
    });
}

/// Luminance noise reduction; `luma` is replaced in place.
pub(crate) fn luma_nr(luma: &mut [f32], w: usize, h: usize, p: &DetailPlan) {
    let orig = luma.to_vec();
    let mut cur = luma.to_vec();
    let mut guide = luma.to_vec();
    let mut acc = vec![0.0f32; w * h];
    for lev in 0..DETAIL_LEVELS {
        let (next, gnext) = guided_atrous(&cur, &guide, w, h, lev, p.lnr_sigma[lev].max(1e-6));
        let wl = p.lnr_w[lev] * p.lnr_alpha;
        acc.par_iter_mut().enumerate().for_each(|(i, o)| {
            let taper = 1.0 - NR_HIGHLIGHT_CUT * smooth01((orig[i] - 0.75) / 0.15);
            *o += (cur[i] - next[i]) * (1.0 - (wl * taper).min(1.0));
        });
        cur = next;
        guide = gnext;
    }
    luma.par_iter_mut()
        .enumerate()
        .for_each(|(i, l)| *l = cur[i] + acc[i]);
}

/// Sharpening on luma; returns the new plane.
pub(crate) fn sharpen_luma(luma: &[f32], w: usize, h: usize, p: &DetailPlan) -> Vec<f32> {
    let ga = gauss_blur(luma, w, h, p.sigma_a);
    let gb = gauss_blur(luma, w, h, p.sigma_b);
    let gf = (p.k_fine > 0.0).then(|| gauss_blur(luma, w, h, p.sigma_fine));
    let gm = p.mask.then(|| gauss_blur(luma, w, h, p.mask_sigma));
    let minmax = p.halo.then(|| local_min_max(luma, w, h, p.halo_r as usize));
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let i = y * w + x;
            let l = luma[i];
            let mut delta = p.k * (ga[i] - gb[i]);
            if let Some(gf) = &gf {
                delta += p.k_fine * (l - gf[i]);
            }
            let tone = (l / SH_SHADOW_KNEE).clamp(0.0, 1.0).powf(SH_SHADOW_POW)
                * (1.0 - SH_HIGHLIGHT_CUT * smooth01((l - 0.75) / 0.30));
            delta *= tone;
            if let Some(gm) = &gm {
                let gx = if w < 2 {
                    0.0
                } else if x == 0 {
                    gm[i + 1] - gm[i]
                } else if x == w - 1 {
                    gm[i] - gm[i - 1]
                } else {
                    (gm[i + 1] - gm[i - 1]) * 0.5
                };
                let gy = if h < 2 {
                    0.0
                } else if y == 0 {
                    gm[i + w] - gm[i]
                } else if y == h - 1 {
                    gm[i] - gm[i - w]
                } else {
                    (gm[i + w] - gm[i - w]) * 0.5
                };
                let g = (gx * gx + gy * gy).sqrt();
                delta *= smooth01((g - p.mask_lo) / (p.mask_hi - p.mask_lo).max(1e-9));
            }
            delta = SH_LIMIT * (delta / SH_LIMIT).tanh();
            let mut u = l + delta;
            if let Some((lo, hi)) = &minmax {
                let m = SH_HALO_MARGIN * (hi[i] - lo[i]);
                let c = u.clamp(lo[i] - m, hi[i] + m);
                u = c + p.halo_h * (u - c);
            }
            *o = u;
        }
    });
    out
}

/// sRGB transfer, odd-extended and valid above 1 (scene/HDR working values).
#[inline]
pub(crate) fn encode_channel(v: f32) -> f32 {
    let a = v.abs();
    let e = if a <= 0.003_130_8 {
        12.92 * a
    } else {
        1.055 * a.powf(1.0 / 2.4) - 0.055
    };
    e.copysign(v)
}

#[inline]
pub(crate) fn decode_channel(e: f32) -> f32 {
    let a = e.abs();
    let v = if a <= 0.040_45 {
        a / 12.92
    } else {
        ((a + 0.055) / 1.055).powf(2.4)
    };
    v.copysign(e)
}

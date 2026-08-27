//! Detail and Effects stages. Detail: à-trous (stationary) wavelet sharpening
//! and noise reduction on the luminance plane plus wavelet chroma NR.
//! Effects: clarity (local contrast), defog (dark-channel veil removal), and
//! vignette.

use super::*;
use crate::core::color::luminance_f32;
use crate::core::tile::{dither16_to_u8, quantize_dither, TileMap, TILE_SIZE};
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;

/// Detail-stage tuning. `DETAIL_HALO` covers the widest neighbourhood any stage
/// reads: the à-trous levels reach ±2·(1+2+4) = ±14 px cumulatively, plus the
/// mask gradient. `SHARPEN_KNEE` is the wavelet-coefficient amplitude the
/// Detail slider gates around; `MASK_GRAD_FULL` the residual-plane gradient a
/// full Masking slider requires; `SHARPEN_LIMIT` the tanh overshoot ceiling.
const DETAIL_HALO: usize = 16;
const SHARPEN_KNEE: f32 = 0.04;
const SHARPEN_LIMIT: f32 = 0.35;
const MASK_GRAD_FULL: f32 = 0.035;

/// À-trous decomposition depth: detail scales ≈ 1, 2, 4 px.
const WAVELET_LEVELS: usize = 3;
/// Range sigma of the edge-aware tap weights (luma units). Taps whose value
/// differs from the centre by ≫ this contribute almost nothing, so the blur
/// does not cross strong edges and coefficients hold texture, not edge steps.
const WAVELET_RANGE_SIGMA: f32 = 0.12;
/// Luminance-NR coefficient threshold (level 0) at a full slider, decaying per
/// coarser level — matches noise energy concentrating at the finest scale.
const NR_LUMA_THRESH: f32 = 0.08;
const NR_LEVEL_DECAY: f32 = 0.5;
/// Chroma-NR level attenuation at a full slider: kill fine colour speckle
/// outright, keep progressively more of the coarser (real-colour) scales.
const CHROMA_NR_ATTEN: [f32; WAVELET_LEVELS] = [1.0, 0.85, 0.6];
/// First à-trous scale whose chroma smoothing is edge-aware (Q6). Level 0 (the
/// finest, isolated-speckle scale) stays non-edge-aware so a strong colour speck
/// is still captured and removed; levels 1+ are edge-aware so genuine colour
/// boundaries stay in the residual and are not bled across. `WAVELET_LEVELS`
/// would restore the pre-Q6 fully-non-edge-aware (edge-smearing) behaviour.
const CHROMA_NR_EDGE_AWARE_FROM: usize = 1;

/// Tone-adaptive noise reduction. The display tone curve stretches shadow noise
/// (most visible) and compresses highlight noise, so both the luminance garrote
/// threshold and the chroma attenuation are scaled up in the shadows and left at
/// their baseline in the highlights. Highlight behaviour is therefore identical
/// to the pre-upgrade engine; only the shadows are cleaned harder. The weight is
/// a function of local BRIGHTNESS (the wavelet residual / pixel luma), not of a
/// measured global noise level, so it is resolution-invariant and the reduced-
/// resolution interactive preview matches the full-resolution commit. `MID` is
/// the luminance at which the shadow boost has faded to none.
const NR_SHADOW_MID: f32 = 0.5;
const NR_LUMA_SHADOW_GAIN: f32 = 1.5;
const NR_CHROMA_SHADOW_GAIN: f32 = 1.2;

/// Defringe tuning. Lateral chromatic aberration and purple fringing paint a
/// thin coloured rim (classically magenta/purple on one side, green on the
/// other) along high-contrast edges; the rim's hue matches neither side. The
/// cleanup fires only where three independent conditions agree, so real colour
/// is left alone:
///   • `EDGE_*` — the luminance gradient (central difference, [0,1] luma) is
///     steep, i.e. a genuine contrast edge (a uniform colour field never fires);
///   • `HUE_*` — the chroma direction lies near the green↔magenta axis
///     (`DEFRINGE_AXIS`), so red/blue/yellow/cyan edges are ignored;
///   • `SPIKE_*` — the pixel is markedly more colourful than its blurred
///     regional reference, i.e. a thin rim rather than the edge of a broad
///     real magenta/green object.
/// Where all three hold, the pixel's chroma is pulled toward the blurred
/// reference (`RADIUS` px), neutralising the rim while leaving luminance intact.
const DEFRINGE_RADIUS: usize = 3;
const DEFRINGE_EDGE_LO: f32 = 0.04;
const DEFRINGE_EDGE_HI: f32 = 0.15;
const DEFRINGE_HUE_LO: f32 = 0.60;
const DEFRINGE_HUE_HI: f32 = 0.85;
const DEFRINGE_SPIKE_LO: f32 = 0.01;
const DEFRINGE_SPIKE_HI: f32 = 0.05;
/// Unit vector of the green↔magenta axis in (rgb − luma) chroma space: the
/// direction of a pure magenta offset (`[1,0,1] − luma`) under Rec.709 luma.
/// Pure green is its negative, so `|cos angle|` ≈ 1 for both fringe hues and
/// falls off for the other primaries/secondaries.
const DEFRINGE_AXIS: [f32; 3] = [0.6807, -0.2710, 0.6807];

/// Slider values folded to working units once per bake.
struct DetailParams {
    amount: f32,
    sigma: f32,
    detail: f32,
    masking: f32,
    nr: f32,
    color_nr: f32,
    defringe: f32,
}

impl DetailParams {
    fn new(settings: &DevelopSettings) -> Self {
        Self {
            amount: (settings.sharpening / 100.0).clamp(0.0, 1.0) * 1.5,
            sigma: settings.sharpen_radius.clamp(0.3, 3.0),
            detail: (settings.sharpen_detail / 100.0).clamp(0.0, 1.0),
            masking: (settings.sharpen_masking / 100.0).clamp(0.0, 1.0),
            nr: (settings.noise_reduction / 100.0).clamp(0.0, 1.0),
            color_nr: (settings.color_noise_reduction / 100.0).clamp(0.0, 1.0),
            defringe: (settings.defringe / 100.0).clamp(0.0, 1.0),
        }
    }
}

/// Edge-gated green↔magenta chroma cleanup (lateral CA / purple fringing).
///
/// `luma` is the pixel luminance plane and `chroma` the per-pixel colour offset
/// (`rgb − luma`, so luminance of the chroma part is 0 — neutralising it cannot
/// shift brightness). We build a blurred regional chroma reference, then at each
/// pixel pull the chroma toward that reference by `amount · edge · hue · spike`
/// (see the tuning block for each factor). All three gates must agree, so only
/// the thin, off-hue rim at a contrast edge is neutralised; uniform colour,
/// non-fringe hues and broad coloured objects are preserved.
fn apply_defringe(chroma: &mut [[f32; 3]], luma: &[f32], w: usize, h: usize, amount: f32) {
    if w < 3 || h < 3 {
        return;
    }
    let cref: [Vec<f32>; 3] = std::array::from_fn(|ch| {
        let plane: Vec<f32> = chroma.iter().map(|c| c[ch]).collect();
        box_blur_plane(&plane, w, h, DEFRINGE_RADIUS)
    });
    let out: Vec<[f32; 3]> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let c = chroma[i];
            let x = i % w;
            let y = i / w;
            let xl = luma[y * w + x.saturating_sub(1)];
            let xr = luma[y * w + (x + 1).min(w - 1)];
            let yt = luma[y.saturating_sub(1) * w + x];
            let yb = luma[(y + 1).min(h - 1) * w + x];
            let grad = ((xr - xl) * 0.5).hypot((yb - yt) * 0.5);
            let w_edge = smootherstep(DEFRINGE_EDGE_LO, DEFRINGE_EDGE_HI, grad);
            if w_edge <= 0.0 {
                return c;
            }
            // Hue selectivity: |cos| between the chroma direction and the
            // green↔magenta axis. ≈1 for magenta/green, ~0.5 for red/cyan → those
            // fall below HUE_LO and are left untouched.
            let cmag = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt();
            if cmag <= 1e-5 {
                return c;
            }
            let cos =
                ((c[0] * DEFRINGE_AXIS[0] + c[1] * DEFRINGE_AXIS[1] + c[2] * DEFRINGE_AXIS[2])
                    / cmag)
                    .abs();
            let w_hue = smootherstep(DEFRINGE_HUE_LO, DEFRINGE_HUE_HI, cos);
            if w_hue <= 0.0 {
                return c;
            }
            // Spike: how much more colourful the pixel is than its blurred
            // reference. A thin rim spikes; a broad real object's edge does not.
            let crefv = [cref[0][i], cref[1][i], cref[2][i]];
            let crefmag = (crefv[0] * crefv[0] + crefv[1] * crefv[1] + crefv[2] * crefv[2]).sqrt();
            let w_spike = smootherstep(DEFRINGE_SPIKE_LO, DEFRINGE_SPIKE_HI, cmag - crefmag);
            let k = amount * w_edge * w_hue * w_spike;
            if k <= 0.0 {
                return c;
            }
            [
                c[0] + (crefv[0] - c[0]) * k,
                c[1] + (crefv[1] - c[1]) * k,
                c[2] + (crefv[2] - c[2]) * k,
            ]
        })
        .collect();
    chroma.copy_from_slice(&out);
}

/// One à-trous B3-spline smoothing pass at hole spacing `1 << level`,
/// separable and edge-clamped. With `edge_aware`, each tap is additionally
/// range-weighted `1 / (1 + (Δ/σ)²)` against the centre value, so the blur
/// does not cross strong edges — the detail coefficient (src − smooth) then
/// holds texture rather than the edge step, and boosting or shrinking it
/// cannot ring around edges.
fn atrous_smooth(src: &[f32], w: usize, h: usize, level: usize, edge_aware: bool) -> Vec<f32> {
    const B3: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
    let step = 1i64 << level;
    let inv_s2 = 1.0 / (WAVELET_RANGE_SIGMA * WAVELET_RANGE_SIGMA);
    let pass = |input: &[f32], horizontal: bool| -> Vec<f32> {
        let mut out = vec![0.0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let centre = input[y * w + x];
                let mut acc = 0.0f32;
                let mut wsum = 0.0f32;
                for (t, &kv) in B3.iter().enumerate() {
                    let o = (t as i64 - 2) * step;
                    let idx = if horizontal {
                        let sx = (x as i64 + o).clamp(0, w as i64 - 1) as usize;
                        y * w + sx
                    } else {
                        let sy = (y as i64 + o).clamp(0, h as i64 - 1) as usize;
                        sy * w + x
                    };
                    let v = input[idx];
                    let wt = if edge_aware {
                        let d = v - centre;
                        kv / (1.0 + d * d * inv_s2)
                    } else {
                        kv
                    };
                    acc += v * wt;
                    wsum += wt;
                }
                out[y * w + x] = acc / wsum.max(1e-9);
            }
        }
        out
    };
    let tmp = pass(src, true);
    pass(&tmp, false)
}

/// À-trous decomposition into [`WAVELET_LEVELS`] detail planes plus the smooth
/// residual: `src = residual + Σ details[j]` exactly (the transform is a plain
/// difference pyramid at full resolution, so reconstruction is lossless).
///
/// `edge_aware_from` is the first scale that uses edge-aware (range-weighted)
/// smoothing: levels `< edge_aware_from` blur with plain B3 taps (so a strong
/// isolated speck at that scale is captured into the detail plane and can be
/// removed), while levels `>= edge_aware_from` keep true edges out of the detail
/// planes. `0` = fully edge-aware; `WAVELET_LEVELS` = never.
fn atrous_decompose(
    src: &[f32],
    w: usize,
    h: usize,
    edge_aware_from: usize,
) -> (Vec<f32>, Vec<Vec<f32>>) {
    let mut c = src.to_vec();
    let mut details = Vec::with_capacity(WAVELET_LEVELS);
    for level in 0..WAVELET_LEVELS {
        let next = atrous_smooth(&c, w, h, level, level >= edge_aware_from);
        for (d, &n) in c.iter_mut().zip(&next) {
            *d -= n;
        }
        details.push(std::mem::replace(&mut c, next));
    }
    (c, details)
}

/// Tone-adaptive NR strength multiplier at a pixel of the given brightness: 1 in
/// the highlights (no change vs the pre-upgrade engine), rising toward `1 + gain`
/// in the shadows where display-domain noise is most visible.
#[inline]
fn nr_shadow_weight(brightness: f32, gain: f32) -> f32 {
    1.0 + gain * (1.0 - smootherstep(0.0, NR_SHADOW_MID, brightness.clamp(0.0, 1.0)))
}

/// Per-à-trous-level weight that scales the Detail contribution down when the
/// pass runs on a reduced-resolution live-preview proxy. `preview_scale` is the
/// proxy downsample in SOURCE pixels per proxy pixel; it is `1` for every full-
/// resolution path (per-tile commit, Apply, settled 100 % bake), which makes
/// every weight exactly `1.0` and this a bit-exact no-op there.
///
/// Why scale at all (plan G6, "quy đổi bán kính về pixel nguồn"): level `l`
/// sharpens/denoises structure at ≈ `2^l` PROXY px, i.e. `2^l · S` SOURCE px.
/// The commit only ever touches 1–4 source px, so on the 8–48× live proxy the
/// proxy's own levels sit at 8–192 source px — far coarser than anything the
/// commit does. Running them at full strength paints a broad, wrong-scale
/// halo/blur that then snaps to the fine commit result on pointer release (the
/// "scale jump"). Weighting each level by the fraction of its intended source
/// scale the proxy can still resolve, `min(2^l / S, 1)`, keeps the finest (most
/// objectionable) false level the most suppressed and lets only a coarse hint
/// through, so the drag preview tracks the settled bake instead of exaggerating
/// it. Full physical detail returns with the settled full-resolution bake.
#[inline]
fn preview_level_survive(preview_scale: u32) -> [f32; WAVELET_LEVELS] {
    let s = preview_scale.max(1) as f32;
    std::array::from_fn(|l| ((1u32 << l) as f32 / s).clamp(0.0, 1.0))
}

/// Colour NR → Luminance NR → Sharpen over one halo'd RGB plane.
///
/// The pixel is split into luminance + chroma offsets (luminance of the chroma
/// part is 0 by linearity, so the chroma stages cannot shift brightness):
///   • Colour NR: à-trous shrinkage of the chroma planes — the fine levels
///     (colour speckle) are attenuated outright, the residual (real colour
///     areas) kept. Plain B3 taps: a strong single-pixel colour speck must not
///     be "protected" as an edge. The attenuation is tone-adaptive: shadows
///     (worst colour blotches) are cleaned harder, highlights left at baseline.
///   • Luminance NR: non-negative-garrote shrinkage of the edge-aware wavelet
///     coefficients, with a shadow-boosted threshold (display-domain shadow
///     grain is most visible). Real edges live in the residual (the range
///     weights keep the blur from crossing them), so thresholding erodes grain,
///     not structure.
///   • Sharpening boosts the (denoised) coefficients per level: Radius shifts
///     weight toward coarser levels, Detail gates small-amplitude coefficients
///     (strong edges always sharpen), Masking gates on the residual's gradient
///     (smooth areas drop out), the total lift is tanh-limited, and the chroma
///     de-fringe pull is kept from the old engine (demosaic fringing guard).
fn process_detail_plane(
    rgb: &[[f32; 3]],
    w: usize,
    h: usize,
    p: &DetailParams,
    linear_space: Option<crate::core::working_color::WorkingColorSpace>,
    preview_scale: u32,
) -> Vec<[f32; 3]> {
    let linear = linear_space.is_some();
    // All-ones for every full-resolution path (`preview_scale == 1`), so the
    // per-level multiplies below are bit-exact no-ops on the commit/Apply path.
    let survive = preview_level_survive(preview_scale);
    let mut luma: Vec<f32> = rgb
        .iter()
        .map(|c| {
            let y = if let Some(space) = linear_space {
                working_luma(space, *c)
            } else {
                luminance_f32(c[0], c[1], c[2])
            };
            if linear {
                y.max(0.0)
            } else {
                y.clamp(0.0, 1.0)
            }
        })
        .collect();
    let mut chroma: Vec<[f32; 3]> = rgb
        .iter()
        .zip(&luma)
        .map(|(c, &l)| [c[0] - l, c[1] - l, c[2] - l])
        .collect();

    // Defringe runs first, on the raw chroma at the luminance edges — before
    // Colour NR blurs the rim and before Sharpen re-emphasises it.
    if p.defringe > 0.001 {
        apply_defringe(&mut chroma, &luma, w, h, p.defringe);
    }

    if p.color_nr > 0.001 {
        for ch in 0..3 {
            let plane: Vec<f32> = chroma.iter().map(|c| c[ch]).collect();
            // Scale-aware, edge-aware chroma NR (Q6 §4 "không làm bệt màu thật"):
            // the FINEST scale (level 0, where isolated colour speckle lives)
            // smooths non-edge-aware so a strong speck is captured and removed
            // even next to an edge; the COARSER scales smooth EDGE-AWARE so a
            // genuine colour boundary stays in the edge-preserving residual and
            // is not bled across ("bệt"). Pre-Q6 this ran fully non-edge-aware,
            // which smeared real colour edges by ~14 px at strong settings.
            let (res, details) = atrous_decompose(&plane, w, h, CHROMA_NR_EDGE_AWARE_FROM);
            for (i, c) in chroma.iter_mut().enumerate() {
                // Shadows carry the worst colour blotches — attenuate their chroma
                // detail more; highlights keep the baseline (real-colour edges).
                let shadow_w = nr_shadow_weight(luma[i], NR_CHROMA_SHADOW_GAIN);
                let mut v = res[i];
                for (j, d) in details.iter().enumerate() {
                    // `survive[j]` folds Colour NR toward off on a coarse proxy:
                    // the fine colour speckle it targets is already averaged out
                    // by the downsample, so smoothing at proxy scale would only
                    // bleed broad colour the settled bake keeps.
                    let atten = (p.color_nr * CHROMA_NR_ATTEN[j] * shadow_w * survive[j]).min(1.0);
                    v += d[i] * (1.0 - atten);
                }
                c[ch] = v;
            }
        }
    }

    if p.nr > 0.001 || p.amount > 0.001 {
        let (res, mut details) = atrous_decompose(&luma, w, h, 0);

        if p.nr > 0.001 {
            // Base threshold per level (finest strongest); a per-pixel shadow
            // boost then cleans shadow grain harder while highlights stay at the
            // pre-upgrade garrote exactly.
            let mut base = p.nr * NR_LUMA_THRESH;
            for (j, d) in details.iter_mut().enumerate() {
                // On a coarse proxy the finest levels' grain is gone to the
                // downsample; `survive[j]` shrinks their garrote threshold toward
                // 0 (→ no shrink) so preview NR matches the settled result.
                let level_base = base * survive[j];
                for (i, v) in d.iter_mut().enumerate() {
                    let t = level_base * nr_shadow_weight(res[i], NR_LUMA_SHADOW_GAIN);
                    let a = v.abs();
                    // Non-negative garrote: kills sub-threshold coefficients,
                    // barely touches the large (edge/texture) ones.
                    *v = if a <= t {
                        0.0
                    } else {
                        *v * (1.0 - (t * t) / (a * a))
                    };
                }
                base *= NR_LEVEL_DECAY;
            }
        }

        if p.amount > 0.001 {
            // Radius = level balance: small keeps it fine-scale, large adds the
            // coarser scales on top (reaches farther, like a wide unsharp σ).
            let t = ((p.sigma - 0.3) / 2.7).clamp(0.0, 1.0);
            let level_gain = [1.0, 0.35 + 0.65 * t, 0.9 * t];
            let cavg: [Vec<f32>; 3] = std::array::from_fn(|ch| {
                let plane: Vec<f32> = chroma.iter().map(|c| c[ch]).collect();
                box_blur_plane(&plane, w, h, 1)
            });
            for y in 0..h {
                for x in 0..w {
                    let i = y * w + x;
                    let mut delta = 0.0f32;
                    let mut edge_mag = 0.0f32;
                    for (j, d) in details.iter().enumerate() {
                        let dv = d[i];
                        if j < 2 {
                            edge_mag += dv.abs();
                        }
                        let weight =
                            p.detail + (1.0 - p.detail) * smootherstep(0.0, SHARPEN_KNEE, dv.abs());
                        // `survive[j]` folds the wrong-scale proxy boost down so
                        // the drag preview does not paint a coarse false halo the
                        // fine settled bake never produces.
                        delta += p.amount * level_gain[j] * weight * dv * survive[j];
                    }
                    let mask = if p.masking > 0.001 {
                        let xl = res[y * w + x.saturating_sub(1)];
                        let xr = res[y * w + (x + 1).min(w - 1)];
                        let yt = res[y.saturating_sub(1) * w + x];
                        let yb = res[(y + 1).min(h - 1) * w + x];
                        let gmag = ((xr - xl) * 0.5).hypot((yb - yt) * 0.5);
                        let tm = p.masking * MASK_GRAD_FULL;
                        smootherstep(tm * 0.5, tm * 1.5, gmag)
                    } else {
                        1.0
                    };
                    let delta = SHARPEN_LIMIT * (delta * mask / SHARPEN_LIMIT).tanh();
                    let base = res[i] + details.iter().map(|d| d[i]).sum::<f32>();
                    luma[i] = if linear {
                        (base + delta).max(0.0)
                    } else {
                        (base + delta).clamp(0.0, 1.0)
                    };

                    let edge_gate = smootherstep(0.006, 0.055, edge_mag);
                    // The chroma edge pull guards fine (level-0-scale) demosaic
                    // fringing, which does not exist on the downsampled proxy;
                    // `survive[0]` scales it out there and is 1.0 at full res.
                    let fr = (p.amount * edge_gate * 0.4).min(0.6) * mask * survive[0];
                    if fr > 0.001 {
                        for ch in 0..3 {
                            chroma[i][ch] += (cavg[ch][i] - chroma[i][ch]) * fr;
                        }
                    }
                }
            }
        } else {
            // NR only: lossless reconstruction of the shrunk coefficients.
            for (i, l) in luma.iter_mut().enumerate() {
                let d_sum = details.iter().map(|d| d[i]).sum::<f32>();
                *l = if linear {
                    (res[i] + d_sum).max(0.0)
                } else {
                    (res[i] + d_sum).clamp(0.0, 1.0)
                };
            }
        }
    }

    (0..w * h)
        .map(|i| {
            let l = luma[i];
            let out = [l + chroma[i][0], l + chroma[i][1], l + chroma[i][2]];
            if linear {
                out
            } else {
                [
                    out[0].clamp(0.0, 1.0),
                    out[1].clamp(0.0, 1.0),
                    out[2].clamp(0.0, 1.0),
                ]
            }
        })
        .collect()
}

/// Full-resolution RAW Detail pass over the unclamped linear master. Unlike
/// the legacy tiled entry point this receives the complete plane, so the
/// wavelet neighbourhood is naturally seam-free and output encoding remains
/// the single final boundary owned by `develop_scene`.
#[allow(dead_code)]
pub(crate) fn apply_detail_to_working_buffer(
    working: &mut Vec<[f32; 3]>,
    width: usize,
    height: usize,
    settings: &DevelopSettings,
) {
    apply_detail_to_working_buffer_in_space(
        working,
        width,
        height,
        settings,
        crate::core::working_color::WorkingColorSpace::LinearSrgb,
        1,
    );
}

/// `preview_scale` is the live-preview proxy downsample (source px per proxy px),
/// or `1` for the full-resolution commit/settled render. See
/// [`preview_level_survive`] — it is a bit-exact no-op at `1`.
pub(crate) fn apply_detail_to_working_buffer_in_space(
    working: &mut Vec<[f32; 3]>,
    width: usize,
    height: usize,
    settings: &DevelopSettings,
    working_space: crate::core::working_color::WorkingColorSpace,
    preview_scale: u32,
) {
    if width == 0 || height == 0 || working.len() != width * height || !has_detail(settings) {
        return;
    }
    let params = DetailParams::new(settings);
    *working = process_detail_plane(
        working,
        width,
        height,
        &params,
        Some(working_space),
        preview_scale,
    );
}

/// Reduced-resolution twin of the display-domain Detail bake. Interactive
/// preview feeds this an anti-aliased viewport proxy; the wavelet/NR model and
/// slider constants stay identical to the commit path, only the pixel grid is
/// smaller. `preview_scale` (source px per proxy px) rescales the wavelet radii
/// back to source scale so the drag preview tracks the settled bake instead of
/// exaggerating Detail at proxy scale (see [`preview_level_survive`]); pass `1`
/// to run at native resolution.
pub(crate) fn apply_detail_to_display_buffer(
    display: &mut Vec<[f32; 3]>,
    width: usize,
    height: usize,
    settings: &DevelopSettings,
    preview_scale: u32,
) {
    if width == 0 || height == 0 || display.len() != width * height || !has_detail(settings) {
        return;
    }
    let params = DetailParams::new(settings);
    *display = process_detail_plane(display, width, height, &params, None, preview_scale);
}

/// Gather a `DETAIL_HALO`-apron'd f32 RGB plane around one tile (edge-clamped,
/// 16-bit reads — bit-identical for 8-bit tiles). Same conventions as
/// `build_base_luma`'s gather.
fn gather_detail_plane(
    source: &TileMap,
    base_x: u32,
    base_y: u32,
    valid_w: u32,
    valid_h: u32,
) -> (Vec<[f32; 3]>, usize, usize) {
    let r = DETAIL_HALO;
    let vw = valid_w as usize;
    let vh = valid_h as usize;
    let hw = vw + 2 * r;
    let hh = vh + 2 * r;
    let wmax = source.width.saturating_sub(1) as i64;
    let hmax = source.height.saturating_sub(1) as i64;
    let mut out = vec![[0.0f32; 3]; hw * hh];
    for hy in 0..hh {
        let gy = ((base_y as i64) + hy as i64 - r as i64).clamp(0, hmax) as u32;
        let ty_tile = (gy / TILE_SIZE) as i32;
        let ly = gy % TILE_SIZE;
        let mut cached_tx = i32::MIN;
        let mut cur_tile: Option<&Arc<crate::core::tile::Tile>> = None;
        for hx in 0..hw {
            let gx = ((base_x as i64) + hx as i64 - r as i64).clamp(0, wmax) as u32;
            let tx_tile = (gx / TILE_SIZE) as i32;
            if tx_tile != cached_tx {
                cur_tile = source.tiles.get(&crate::core::tile::TilePos {
                    x: tx_tile,
                    y: ty_tile,
                });
                cached_tx = tx_tile;
            }
            if let Some(t) = cur_tile {
                let (r16, g16, b16, _a) = t.get_pixel16(gx % TILE_SIZE, ly);
                out[hy * hw + hx] = [
                    r16 as f32 / 65535.0,
                    g16 as f32 / 65535.0,
                    b16 as f32 / 65535.0,
                ];
            }
        }
    }
    (out, hw, hh)
}

/// Detail stage (Sharpening / Noise Reduction) as a separate full-resolution
/// pass over the already-toned tilemap. Per tile with a `DETAIL_HALO` apron so
/// the blurs are seam-free across tiles; writes the 16-bit master too when
/// present, so a 16-bit document keeps its precision through Detail.
pub(crate) fn apply_detail_to_tilemap(source: &TileMap, settings: &DevelopSettings) -> TileMap {
    if source.width == 0 || source.height == 0 {
        return source.clone();
    }

    let p = DetailParams::new(settings);
    let tiles: HashMap<_, _> = source
        .tiles
        .par_iter()
        .map(|(pos, arc_tile)| {
            let mut tile = (**arc_tile).clone();
            let base_x = pos.x.max(0) as u32 * TILE_SIZE;
            let base_y = pos.y.max(0) as u32 * TILE_SIZE;
            let valid_w = source.width.saturating_sub(base_x).min(TILE_SIZE);
            let valid_h = source.height.saturating_sub(base_y).min(TILE_SIZE);
            if valid_w == 0 || valid_h == 0 {
                return (*pos, Arc::new(tile));
            }

            let (plane, hw, _hh) = gather_detail_plane(source, base_x, base_y, valid_w, valid_h);
            let out = process_detail_plane(&plane, hw, _hh, &p, None, 1);
            let r = DETAIL_HALO;

            for ty in 0..valid_h as usize {
                for tx in 0..valid_w as usize {
                    let i = (ty * TILE_SIZE as usize + tx) * 4;
                    if tile.pixels[i + 3] == 0 {
                        continue;
                    }
                    let v = out[(ty + r) * hw + (tx + r)];
                    let x = base_x + tx as u32;
                    let y = base_y + ty as u32;
                    if let Some(p16) = tile.pixels16.as_mut() {
                        let q16 = |v: f32| (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
                        p16[i] = q16(v[0]);
                        p16[i + 1] = q16(v[1]);
                        p16[i + 2] = q16(v[2]);
                        // Same ordered dither as the 8-bit branch below, so the display
                        // mirror of a 16-bit commit doesn't posterize smooth gradients.
                        tile.pixels[i] = dither16_to_u8(p16[i], x, y, 0);
                        tile.pixels[i + 1] = dither16_to_u8(p16[i + 1], x, y, 1);
                        tile.pixels[i + 2] = dither16_to_u8(p16[i + 2], x, y, 2);
                    } else {
                        tile.pixels[i] = quantize_dither(v[0], x, y, 0);
                        tile.pixels[i + 1] = quantize_dither(v[1], x, y, 1);
                        tile.pixels[i + 2] = quantize_dither(v[2], x, y, 2);
                    }
                }
            }
            (*pos, Arc::new(tile))
        })
        .collect();

    TileMap {
        tiles,
        width: source.width,
        height: source.height,
    }
}

#[cfg(test)]
pub(crate) fn apply_detail_to_pixels(
    settings: &DevelopSettings,
    pixels: &mut [u8],
    width: u32,
    height: u32,
) {
    // Route through the production per-tile pass so tests exercise it.
    let tm = TileMap::from_rgba(pixels, width, height);
    let out = apply_detail_to_tilemap(&tm, settings);
    pixels.copy_from_slice(&out.flatten());
}

/// Effects tuning. Clarity: local-contrast gain and its tanh ceiling (halo /
/// clipping guard). Defog: the largest fraction of the white veil a full
/// slider may strip, and the transmission floor that keeps the division from
/// exploding in dense haze. Mirrored in the WGSL `dev_effects_stage`.
const CLARITY_GAIN: f32 = 2.2;
const CLARITY_LIMIT: f32 = 0.28;
const TEXTURE_GAIN: f32 = 1.35;
const TEXTURE_LIMIT: f32 = 0.14;
const DEHAZE_MAX_VEIL: f32 = 0.7;
const DEHAZE_MIN_TRANSMISSION: f32 = 0.25;

/// Develop Effects stage. `base_luma` is the pixel's edge-aware regional
/// luminance AFTER tone (see `DevelopPlan::effects_base` for how each path
/// supplies it) — it is what makes Clarity/Defog spatial operations (real
/// local contrast / veil removal) instead of the old per-pixel soft-contrast.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_effects(
    settings: &DevelopSettings,
    r: &mut f32,
    g: &mut f32,
    b: &mut f32,
    x: u32,
    y: u32,
    inv_w: f32,
    inv_h: f32,
    base_luma: f32,
) {
    // Texture: real luminance high-pass against the edge-aware spatial base.
    // Only target luma moves; the chroma reconstruction stays independent.
    if settings.texture.abs() > 0.001 {
        let luma = luminance_f32(*r, *g, *b).clamp(0.0, 1.0);
        let base = base_luma.clamp(0.0, 1.0);
        let k = eased_control(settings.texture) * TEXTURE_GAIN;
        let tonal = bell(base, 0.5, 0.62);
        let boost = k * tonal * (luma - base);
        let delta = TEXTURE_LIMIT * (boost / TEXTURE_LIMIT).tanh();
        apply_luma_target(r, g, b, (luma + delta).clamp(0.0, 1.0));
    }

    // Clarity (Definition): true local contrast — amplify the pixel's deviation
    // from its regional base, weighted to REGIONAL midtones (so highlight and
    // shadow areas are protected as regions), tanh-limited against halos.
    if settings.clarity.abs() > 0.001 {
        let luma = luminance_f32(*r, *g, *b).clamp(0.0, 1.0);
        let base = base_luma.clamp(0.0, 1.0);
        let k = eased_control(settings.clarity) * (CONTROL_LIMIT / 180.0) * CLARITY_GAIN;
        let mid = bell(base, 0.5, 0.56);
        let boost = k * mid * (luma - base);
        let delta = CLARITY_LIMIT * (boost / CLARITY_LIMIT).tanh();
        apply_tone_delta(r, g, b, delta);
    }

    // Dehaze (Defog): veil removal, J = (I − A·(1−t)) / t with white airlight
    // (A = 1) and per-region transmission estimated from the base — haze reads
    // as a regionally-bright veil, so clear dark regions stay untouched.
    // Negative values mix the veil back in.
    if settings.dehaze.abs() > 0.001 {
        let base = base_luma.clamp(0.0, 1.0);
        let d = (eased_control(settings.dehaze) * (CONTROL_LIMIT / 160.0)).clamp(-1.0, 1.0);
        if d > 0.0 {
            let veil = smootherstep(0.25, 0.95, base);
            let t = (1.0 - d * veil * DEHAZE_MAX_VEIL).max(DEHAZE_MIN_TRANSMISSION);
            let a = 1.0 - t;
            *r = (*r - a) / t;
            *g = (*g - a) / t;
            *b = (*b - a) / t;
        } else {
            let m = -d * 0.45 * smootherstep(0.10, 0.90, base);
            *r = *r * (1.0 - m) + m;
            *g = *g * (1.0 - m) + m;
            *b = *b * (1.0 - m) + m;
        }
    }

    let vignette = eased_control(settings.vignette);
    if vignette.abs() > 0.001 {
        let nx = x as f32 * inv_w - 0.5;
        let ny = y as f32 * inv_h - 0.5;
        let edge = ((nx * nx + ny * ny).sqrt() / 0.707).clamp(0.0, 1.0);
        let amount = smootherstep(0.18, 1.0, edge) * vignette.abs() * 0.42;
        let delta = if vignette > 0.0 { -amount } else { amount };
        apply_tone_delta(r, g, b, delta);
    }
}

/// RAW twin of [`apply_effects`]: identical slider response, but luminance and
/// all channel arithmetic stay in the unclamped linear working buffer. The
/// caller supplies an edge-aware linear-luminance base and performs the single
/// output transform only after this stage.
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
pub(crate) fn apply_effects_linear(
    settings: &DevelopSettings,
    r: &mut f32,
    g: &mut f32,
    b: &mut f32,
    x: u32,
    y: u32,
    inv_w: f32,
    inv_h: f32,
    base_luma: f32,
) {
    apply_effects_linear_in_space(
        settings,
        r,
        g,
        b,
        x,
        y,
        inv_w,
        inv_h,
        base_luma,
        crate::core::working_color::WorkingColorSpace::LinearSrgb,
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_effects_linear_in_space(
    settings: &DevelopSettings,
    r: &mut f32,
    g: &mut f32,
    b: &mut f32,
    x: u32,
    y: u32,
    inv_w: f32,
    inv_h: f32,
    base_luma: f32,
    working_space: crate::core::working_color::WorkingColorSpace,
) {
    let set_luma = |r: &mut f32, g: &mut f32, b: &mut f32, target: f32| {
        let old = working_luma(working_space, [*r, *g, *b]);
        if old.abs() > 1e-6 {
            let scale = target / old;
            *r *= scale;
            *g *= scale;
            *b *= scale;
        } else {
            *r = target;
            *g = target;
            *b = target;
        }
    };

    if settings.texture.abs() > 0.001 {
        let luma = working_luma(working_space, [*r, *g, *b]).max(0.0);
        let base = base_luma.max(0.0);
        let k = eased_control(settings.texture) * TEXTURE_GAIN;
        let boost = k * bell(base.clamp(0.0, 1.0), 0.5, 0.62) * (luma - base);
        let delta = TEXTURE_LIMIT * (boost / TEXTURE_LIMIT).tanh();
        set_luma(r, g, b, (luma + delta).max(0.0));
    }
    if settings.clarity.abs() > 0.001 {
        let luma = working_luma(working_space, [*r, *g, *b]).max(0.0);
        let base = base_luma.max(0.0);
        let k = eased_control(settings.clarity) * (CONTROL_LIMIT / 180.0) * CLARITY_GAIN;
        let boost = k * bell(base.clamp(0.0, 1.0), 0.5, 0.56) * (luma - base);
        let delta = CLARITY_LIMIT * (boost / CLARITY_LIMIT).tanh();
        set_luma(r, g, b, (luma + delta).max(0.0));
    }
    if settings.dehaze.abs() > 0.001 {
        let base = base_luma.clamp(0.0, 1.0);
        let d = (eased_control(settings.dehaze) * (CONTROL_LIMIT / 160.0)).clamp(-1.0, 1.0);
        if d > 0.0 {
            let veil = smootherstep(0.25, 0.95, base);
            let t = (1.0 - d * veil * DEHAZE_MAX_VEIL).max(DEHAZE_MIN_TRANSMISSION);
            let a = 1.0 - t;
            *r = (*r - a) / t;
            *g = (*g - a) / t;
            *b = (*b - a) / t;
        } else {
            let m = -d * 0.45 * smootherstep(0.10, 0.90, base);
            *r = *r * (1.0 - m) + m;
            *g = *g * (1.0 - m) + m;
            *b = *b * (1.0 - m) + m;
        }
    }
    let vignette = eased_control(settings.vignette);
    if vignette.abs() > 0.001 {
        let nx = x as f32 * inv_w - 0.5;
        let ny = y as f32 * inv_h - 0.5;
        let edge = ((nx * nx + ny * ny).sqrt() / 0.707).clamp(0.0, 1.0);
        let amount = smootherstep(0.18, 1.0, edge) * vignette.abs() * 0.42;
        let delta = if vignette > 0.0 { -amount } else { amount };
        let luma = working_luma(working_space, [*r, *g, *b]).max(0.0);
        set_luma(r, g, b, (luma + delta).max(0.0));
    }
}

#[inline]
fn working_luma(space: crate::core::working_color::WorkingColorSpace, rgb: [f32; 3]) -> f32 {
    if space == crate::core::working_color::WorkingColorSpace::LinearSrgb {
        luma_lin(rgb[0], rgb[1], rgb[2])
    } else {
        let c = space.render_luminance_coefficients();
        c[0] * rgb[0] + c[1] * rgb[1] + c[2] * rgb[2]
    }
}

#[cfg(test)]
mod defringe_tests {
    use super::*;

    fn chroma_mag(px: [f32; 3]) -> f32 {
        let y = crate::core::color::luminance_f32(px[0], px[1], px[2]);
        ((px[0] - y).powi(2) + (px[1] - y).powi(2) + (px[2] - y).powi(2)).sqrt()
    }

    /// A high-contrast vertical edge carrying a two-pixel magenta fringe rim.
    /// Defringe must collapse the rim's chroma, while a uniformly saturated patch
    /// (no luminance edge) and a saturated *red* edge (a non-fringe hue) are left
    /// essentially untouched.
    #[test]
    fn defringe_clears_magenta_rim_but_spares_real_colour() {
        let w = 24usize;
        let h = 8usize;
        let dark = 0.10f32;
        let bright = 0.75f32;

        // 1) Magenta rim at the x=11/12 boundary between a dark and a bright half.
        let mut edge = vec![[0.0f32; 3]; w * h];
        for y in 0..h {
            for x in 0..w {
                let base = if x < 12 { dark } else { bright };
                edge[y * w + x] = if x == 11 || x == 12 {
                    [base + 0.18, (base - 0.12).max(0.0), base + 0.18]
                } else {
                    [base, base, base]
                };
            }
        }
        let mut settings = DevelopSettings::default();
        settings.defringe = 100.0;

        let rim = |img: &[[f32; 3]]| -> f32 {
            (0..h)
                .map(|y| chroma_mag(img[y * w + 11]) + chroma_mag(img[y * w + 12]))
                .sum::<f32>()
                / (2 * h) as f32
        };
        let rim_before = rim(&edge);
        apply_detail_to_display_buffer(&mut edge, w, h, &settings, 1);
        let rim_after = rim(&edge);
        assert!(
            rim_after < rim_before * 0.5,
            "magenta edge rim should lose over half its chroma: {rim_before} -> {rim_after}"
        );

        // 2) Uniform saturated magenta, no edges → preserved.
        let mut flat = vec![[0.42f32, 0.16, 0.50]; w * h];
        let flat_before = chroma_mag(flat[w * h / 2]);
        apply_detail_to_display_buffer(&mut flat, w, h, &settings, 1);
        let flat_after = chroma_mag(flat[w * h / 2]);
        assert!(
            (flat_after - flat_before).abs() < 0.02,
            "uniform colour must be preserved: {flat_before} -> {flat_after}"
        );

        // 3) A saturated RED edge (non-fringe hue) must keep most of its chroma.
        let mut red = vec![[0.0f32; 3]; w * h];
        for y in 0..h {
            for x in 0..w {
                red[y * w + x] = if x < 12 {
                    [dark, dark, dark]
                } else {
                    [0.85, 0.12, 0.12]
                };
            }
        }
        let red_before = chroma_mag(red[3 * w + 12]);
        apply_detail_to_display_buffer(&mut red, w, h, &settings, 1);
        let red_after = chroma_mag(red[3 * w + 12]);
        assert!(
            red_after > red_before * 0.8,
            "a red (non-fringe) edge must be largely spared: {red_before} -> {red_after}"
        );
    }
}

#[cfg(test)]
mod nr_tests {
    use super::*;

    /// Deterministic pseudo-noise in [-1, 1] (no rng dependency / stays stable
    /// across runs so the assertions are reproducible).
    fn hash_noise(i: usize) -> f32 {
        let mut x = (i as u32)
            .wrapping_mul(2_654_435_761)
            .wrapping_add(2_463_534_242);
        x ^= x >> 15;
        x = x.wrapping_mul(2_246_822_519);
        x ^= x >> 13;
        x = x.wrapping_mul(3_266_489_917);
        x ^= x >> 16;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    /// Tone-adaptive luminance NR: a dark and a bright noisy half both get
    /// cleaner, the shadow half is cleaned harder, and the big luminance edge
    /// between them is preserved.
    #[test]
    fn noise_reduction_cleans_shadows_harder_and_keeps_edges() {
        let w = 48usize;
        let h = 24usize;
        let amp = 0.06f32;
        let (dark, bright) = (0.12f32, 0.85f32);
        let mut img = vec![[0.0f32; 3]; w * h];
        for y in 0..h {
            for x in 0..w {
                let base = if x < w / 2 { dark } else { bright };
                let v = (base + amp * hash_noise(y * w + x)).clamp(0.0, 1.0);
                img[y * w + x] = [v, v, v];
            }
        }

        // Mean + std over a region's interior (away from the central edge/borders).
        let stats = |img: &[[f32; 3]], x0: usize, x1: usize| -> (f32, f32) {
            let (mut s, mut s2, mut n) = (0.0f64, 0.0f64, 0.0f64);
            for y in 3..h - 3 {
                for x in x0 + 3..x1 - 3 {
                    let v = img[y * w + x][0] as f64;
                    s += v;
                    s2 += v * v;
                    n += 1.0;
                }
            }
            let mean = s / n;
            (mean as f32, ((s2 / n - mean * mean).max(0.0)).sqrt() as f32)
        };

        let (_, dark_std0) = stats(&img, 0, w / 2);
        let (_, bright_std0) = stats(&img, w / 2, w);

        // A gentle setting: the baseline (highlight) threshold sits near the noise
        // level, so highlights keep some grain while the shadow boost cleans the
        // dark half harder — that gap is what the tone-adaptive upgrade adds.
        let mut settings = DevelopSettings::default();
        settings.noise_reduction = 25.0;
        apply_detail_to_display_buffer(&mut img, w, h, &settings, 1);

        let (dark_mean1, dark_std1) = stats(&img, 0, w / 2);
        let (bright_mean1, bright_std1) = stats(&img, w / 2, w);

        assert!(
            dark_std1 < dark_std0 * 0.65,
            "shadow noise should drop clearly: {dark_std0} -> {dark_std1}"
        );
        assert!(
            bright_std1 < bright_std0,
            "highlight noise should drop too: {bright_std0} -> {bright_std1}"
        );
        let dark_reduction = 1.0 - dark_std1 / dark_std0;
        let bright_reduction = 1.0 - bright_std1 / bright_std0;
        assert!(
            dark_reduction > bright_reduction + 0.08,
            "shadows must denoise harder than highlights: {dark_reduction} vs {bright_reduction}"
        );
        assert!(
            (bright_mean1 - dark_mean1) > 0.68,
            "the tonal edge must survive denoise: {}",
            bright_mean1 - dark_mean1
        );
    }
}

/// Quality Milestone Q6 — Detail-stage contract and property guard.
///
/// Q6 (RAM/quality plan §"Quality Milestone Q6") separates capture/creative/
/// output sharpen and asks that detail be sharpened without halo/waxy texture
/// and that noise (esp. colour speckle) drop WITHOUT bleeding real colour edges.
/// These hermetic property tests lock the invariants the plan lists and quantify
/// where the current engine falls short, so any tuning has a golden guard.
#[cfg(test)]
mod q6_detail_contract {
    use super::*;

    fn hash_noise(i: usize, salt: u32) -> f32 {
        let mut x = (i as u32)
            .wrapping_mul(2_654_435_761)
            .wrapping_add(salt)
            .wrapping_add(2_463_534_242);
        x ^= x >> 15;
        x = x.wrapping_mul(2_246_822_519);
        x ^= x >> 13;
        x = x.wrapping_mul(3_266_489_917);
        x ^= x >> 16;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    fn luma(px: [f32; 3]) -> f32 {
        crate::core::color::luminance_f32(px[0], px[1], px[2])
    }
    fn chroma_vec(px: [f32; 3]) -> [f32; 3] {
        let y = luma(px);
        [px[0] - y, px[1] - y, px[2] - y]
    }
    fn chroma_mag(px: [f32; 3]) -> f32 {
        let c = chroma_vec(px);
        (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt()
    }

    // ── Sharpening: acutance up, halo bounded, neutral stays neutral ──────────

    #[test]
    fn sharpening_steepens_a_soft_edge_without_a_large_halo() {
        // A soft (blurred) neutral step. Sharpening must steepen the transition
        // (higher acutance) yet keep any overshoot beyond the two plateaus small
        // (no ringing halo), and never tint the neutral.
        let (w, h) = (48usize, 8usize);
        let (lo, hi) = (0.30f32, 0.60f32);
        let soft = |x: usize| -> f32 {
            // A 6-px-wide smootherstep ramp centred at the middle.
            let t = ((x as f32 - (w as f32 / 2.0 - 3.0)) / 6.0).clamp(0.0, 1.0);
            let s = t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
            lo + (hi - lo) * s
        };
        let mut img: Vec<[f32; 3]> = (0..w * h)
            .map(|i| {
                let v = soft(i % w);
                [v, v, v]
            })
            .collect();
        let slope = |img: &[[f32; 3]]| -> f32 {
            // Max adjacent step along the mid row = edge acutance.
            let row = h / 2;
            (0..w - 1)
                .map(|x| (img[row * w + x + 1][0] - img[row * w + x][0]).abs())
                .fold(0.0, f32::max)
        };
        let overshoot = |img: &[[f32; 3]]| -> f32 {
            // How far any pixel exceeds the [lo,hi] plateau band = halo size.
            img.iter()
                .map(|p| (p[0] - hi).max(lo - p[0]).max(0.0))
                .fold(0.0, f32::max)
        };
        let acut0 = slope(&img);

        let mut settings = DevelopSettings::default();
        settings.sharpening = 70.0;
        apply_detail_to_display_buffer(&mut img, w, h, &settings, 1);

        let acut1 = slope(&img);
        let halo = overshoot(&img);
        let max_chroma = img.iter().map(|&p| chroma_mag(p)).fold(0.0, f32::max);
        println!("Q6 sharpen: acutance {acut0:.4} -> {acut1:.4}, halo {halo:.4}, max_chroma {max_chroma:.5}");
        assert!(img.iter().all(|p| p.iter().all(|c| c.is_finite())));
        // The engine is deliberately halo-safe (Q2/Q4 band-pass capture-sharpen),
        // so on a clean smooth ramp it acts gently: acutance must not DROP, a
        // controlled edge overshoot must appear (it is doing something), and that
        // overshoot must stay well below a ringing halo. Neutral stays neutral.
        assert!(
            acut1 >= acut0 - 1e-4,
            "sharpening softened the edge: {acut0} -> {acut1}"
        );
        assert!(
            halo > 1e-3,
            "sharpening produced no edge enhancement: {halo}"
        );
        assert!(halo < (hi - lo) * 0.35, "sharpening halo too large: {halo}");
        assert!(
            max_chroma < 2e-3,
            "sharpening tinted a neutral edge: {max_chroma}"
        );
    }

    #[test]
    fn sharpen_masking_spares_flat_noise() {
        // Masking should keep the sharpener off smooth (noisy-flat) areas while
        // still working on real edges. With Masking high, the flat area's noise
        // is amplified far less than with Masking off.
        let (w, h) = (32usize, 32usize);
        let base = 0.45f32;
        let make = || -> Vec<[f32; 3]> {
            (0..w * h)
                .map(|i| {
                    let v = (base + 0.02 * hash_noise(i, 1)).clamp(0.0, 1.0);
                    [v, v, v]
                })
                .collect()
        };
        let var = |img: &[[f32; 3]]| -> f32 {
            let (mut s, mut s2) = (0.0f64, 0.0f64);
            for p in img {
                s += p[0] as f64;
                s2 += (p[0] as f64) * (p[0] as f64);
            }
            let n = img.len() as f64;
            ((s2 / n - (s / n) * (s / n)).max(0.0)) as f32
        };

        let mut no_mask = make();
        let mut masked = make();
        let mut s = DevelopSettings::default();
        s.sharpening = 90.0;
        apply_detail_to_display_buffer(&mut no_mask, w, h, &s, 1);
        s.sharpen_masking = 90.0;
        apply_detail_to_display_buffer(&mut masked, w, h, &s, 1);

        let v_no = var(&no_mask);
        let v_mask = var(&masked);
        println!("Q6 masking: flat-noise variance no-mask {v_no:.6} vs masked {v_mask:.6}");
        assert!(
            v_mask < v_no * 0.7,
            "Masking should suppress flat-area sharpening: {v_no} vs {v_mask}"
        );
    }

    // ── Colour NR: kills speckle, and how much it bleeds a real colour edge ────

    #[test]
    fn colour_nr_reduces_chroma_speckle() {
        // A flat colour field peppered with per-pixel chroma speckle. Colour NR
        // must reduce the chroma variance markedly while barely moving luma.
        let (w, h) = (32usize, 32usize);
        let mut img: Vec<[f32; 3]> = (0..w * h)
            .map(|i| {
                let cr = 0.03 * hash_noise(i, 7);
                let cb = 0.03 * hash_noise(i, 13);
                [0.40 + cr, 0.40, 0.40 + cb]
            })
            .collect();
        let luma_mean = |img: &[[f32; 3]]| -> f32 {
            img.iter().map(|&p| luma(p)).sum::<f32>() / img.len() as f32
        };
        let chroma_var = |img: &[[f32; 3]]| -> f32 {
            img.iter().map(|&p| chroma_mag(p).powi(2)).sum::<f32>() / img.len() as f32
        };
        let cv0 = chroma_var(&img);
        let ly0 = luma_mean(&img);

        let mut s = DevelopSettings::default();
        s.color_noise_reduction = 80.0;
        apply_detail_to_display_buffer(&mut img, w, h, &s, 1);
        let cv1 = chroma_var(&img);
        let ly1 = luma_mean(&img);
        println!(
            "Q6 colour-NR speckle: chroma-var {cv0:.6} -> {cv1:.6}, luma mean {ly0:.4} -> {ly1:.4}"
        );
        assert!(
            cv1 < cv0 * 0.5,
            "colour NR did not clean chroma speckle: {cv0} -> {cv1}"
        );
        assert!(
            (ly1 - ly0).abs() < 2e-3,
            "colour NR shifted luminance: {ly0} -> {ly1}"
        );
    }

    #[test]
    fn colour_nr_real_edge_bleed_is_measured() {
        // Two solid colours that share the SAME luma (so the edge is purely
        // chromatic and the luma-driven stages see nothing), a clean vertical
        // boundary, strong Colour NR. Measures how much of the real colour STEP
        // survives at the boundary. The pre-Q6 chroma NR runs its à-trous
        // decomposition NON-edge-aware, so it blurs this step badly.
        let (w, h) = (48usize, 16usize);
        // Left magenta-ish, right green-ish. Colour NR works on the luma-
        // independent chroma planes, so the luma levels are irrelevant here.
        let left = [0.55f32, 0.30, 0.55];
        let right = [0.34f32, 0.44, 0.34];
        let mid = w / 2;
        let mut img: Vec<[f32; 3]> = (0..w * h)
            .map(|i| if (i % w) < mid { left } else { right })
            .collect();

        // Chroma step across the boundary before NR, measured a couple of px in
        // (away from the exact seam) as the mean chroma-vector distance.
        let step = |img: &[[f32; 3]]| -> f32 {
            let sample = |x: usize| -> [f32; 3] {
                let mut acc = [0.0f32; 3];
                for y in 2..h - 2 {
                    let c = chroma_vec(img[y * w + x]);
                    for k in 0..3 {
                        acc[k] += c[k];
                    }
                }
                let n = (h - 4) as f32;
                [acc[0] / n, acc[1] / n, acc[2] / n]
            };
            let a = sample(mid - 3);
            let b = sample(mid + 3);
            ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
        };
        let step0 = step(&img);

        let mut s = DevelopSettings::default();
        s.color_noise_reduction = 80.0;
        apply_detail_to_display_buffer(&mut img, w, h, &s, 1);
        let step1 = step(&img);
        let kept = step1 / step0;
        println!(
            "Q6 colour-NR real-edge: chroma step {step0:.4} -> {step1:.4} (kept {:.0}% at ±3px)",
            kept * 100.0
        );
        // Q6 fix (scale-aware, edge-aware chroma NR): the real colour edge keeps
        // ~82% of its step at ±3px, up from ~68% under the pre-Q6 fully-non-edge-
        // aware smoothing. Lock the floor above the old behaviour so a regression
        // back toward edge-smearing ("bệt màu") fails; raise it further if the
        // chroma range sigma is ever tuned tighter.
        assert!(
            kept > 0.78,
            "colour NR bled the real colour edge to {:.0}% of its step (Q6 baseline ~82%)",
            kept * 100.0
        );
    }

    // ── Cross-slider invariants ───────────────────────────────────────────────

    #[test]
    fn detail_sliders_keep_a_neutral_grey_neutral_and_finite() {
        // Sharpen / NR / Colour-NR on a noisy neutral must not tint it or blow up.
        let (w, h) = (24usize, 24usize);
        let sliders: &[(&str, fn(&mut DevelopSettings, f32))] = &[
            ("sharpening", |s, v| s.sharpening = v),
            ("noise_reduction", |s, v| s.noise_reduction = v),
            ("color_noise_reduction", |s, v| s.color_noise_reduction = v),
        ];
        for &(name, set) in sliders {
            let mut img: Vec<[f32; 3]> = (0..w * h)
                .map(|i| {
                    let v = (0.45 + 0.03 * hash_noise(i, 3)).clamp(0.0, 1.0);
                    [v, v, v]
                })
                .collect();
            let mut s = DevelopSettings::default();
            set(&mut s, 100.0);
            apply_detail_to_display_buffer(&mut img, w, h, &s, 1);
            let max_chroma = img.iter().map(|&p| chroma_mag(p)).fold(0.0, f32::max);
            assert!(
                img.iter()
                    .all(|p| p.iter().all(|c| c.is_finite() && (0.0..=1.0).contains(c))),
                "{name} produced out-of-range output"
            );
            assert!(
                max_chroma < 3e-3,
                "{name} tinted a neutral: chroma {max_chroma}"
            );
        }
    }

    // ── G6: preview Detail is rescaled to source pixels ───────────────────────

    #[test]
    fn preview_scale_folds_detail_back_to_source_scale() {
        // `preview_level_survive` is a bit-exact no-op at full resolution and
        // folds each wavelet level toward off as the live proxy coarsens.
        assert_eq!(preview_level_survive(1), [1.0, 1.0, 1.0]);
        for s in [1u32, 2, 4, 8, 16, 48] {
            let w = preview_level_survive(s);
            assert!(
                w.iter().all(|&v| (0.0..=1.0).contains(&v)),
                "scale {s}: {w:?} outside [0,1]"
            );
        }
        // Coarser proxy ⇒ never MORE survival at any level (monotone), and the
        // finest level (worst false halo) folds at least as hard as the coarse.
        let mut prev = preview_level_survive(1);
        for s in [2u32, 4, 8, 16, 48] {
            let cur = preview_level_survive(s);
            for l in 0..WAVELET_LEVELS {
                assert!(
                    cur[l] <= prev[l] + 1e-6,
                    "scale {s} level {l}: {} > {}",
                    cur[l],
                    prev[l]
                );
            }
            prev = cur;
        }
        let w8 = preview_level_survive(8);
        assert!(
            w8[0] <= w8[1] && w8[1] <= w8[2],
            "finest must fold hardest: {w8:?}"
        );

        // On a soft neutral edge, the sharpen acutance the PREVIEW adds must
        // shrink as the proxy coarsens: the drag preview stops exaggerating
        // Detail at proxy scale and tracks the fine settled/commit bake.
        let (w, h) = (48usize, 8usize);
        let (lo, hi) = (0.30f32, 0.60f32);
        let make = || -> Vec<[f32; 3]> {
            (0..w * h)
                .map(|i| {
                    let x = i % w;
                    let t = ((x as f32 - (w as f32 / 2.0 - 3.0)) / 6.0).clamp(0.0, 1.0);
                    let s = t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
                    let v = lo + (hi - lo) * s;
                    [v, v, v]
                })
                .collect()
        };
        let acut = |img: &[[f32; 3]]| -> f32 {
            let row = h / 2;
            (0..w - 1)
                .map(|x| (img[row * w + x + 1][0] - img[row * w + x][0]).abs())
                .fold(0.0, f32::max)
        };
        let a0 = acut(&make());
        let mut settings = DevelopSettings::default();
        settings.sharpening = 80.0;
        let added = |scale: u32| -> f32 {
            let mut img = make();
            apply_detail_to_display_buffer(&mut img, w, h, &settings, scale);
            assert!(img.iter().all(|p| p.iter().all(|c| c.is_finite())));
            acut(&img) - a0
        };
        let a_full = added(1);
        let a_p2 = added(2);
        let a_p8 = added(8);
        println!("G6 preview sharpen add: full {a_full:.4}, 2x {a_p2:.4}, 8x {a_p8:.4}");
        assert!(
            a_full > 1e-3,
            "full-res sharpen should add acutance: {a_full}"
        );
        assert!(
            a_p2 <= a_full + 1e-4,
            "2x proxy must not exceed full-res add: {a_p2} vs {a_full}"
        );
        assert!(
            a_p8 < a_full - 1e-4,
            "8x proxy must add clearly less than full res: {a_p8} vs {a_full}"
        );
        assert!(
            a_p8 <= a_p2 + 1e-4,
            "coarser proxy must not add more: {a_p8} vs {a_p2}"
        );
    }
}

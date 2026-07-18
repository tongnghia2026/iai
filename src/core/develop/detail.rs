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

/// Slider values folded to working units once per bake.
struct DetailParams {
    amount: f32,
    sigma: f32,
    detail: f32,
    masking: f32,
    nr: f32,
    color_nr: f32,
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
        }
    }
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
fn atrous_decompose(
    src: &[f32],
    w: usize,
    h: usize,
    edge_aware: bool,
) -> (Vec<f32>, Vec<Vec<f32>>) {
    let mut c = src.to_vec();
    let mut details = Vec::with_capacity(WAVELET_LEVELS);
    for level in 0..WAVELET_LEVELS {
        let next = atrous_smooth(&c, w, h, level, edge_aware);
        for (d, &n) in c.iter_mut().zip(&next) {
            *d -= n;
        }
        details.push(std::mem::replace(&mut c, next));
    }
    (c, details)
}

/// Colour NR → Luminance NR → Sharpen over one halo'd RGB plane.
///
/// The pixel is split into luminance + chroma offsets (luminance of the chroma
/// part is 0 by linearity, so the chroma stages cannot shift brightness):
///   • Colour NR: à-trous shrinkage of the chroma planes — the fine levels
///     (colour speckle) are attenuated outright, the residual (real colour
///     areas) kept. Plain B3 taps: a strong single-pixel colour speck must not
///     be "protected" as an edge.
///   • Luminance NR: non-negative-garrote shrinkage of the edge-aware wavelet
///     coefficients. Real edges live in the residual (the range weights keep
///     the blur from crossing them), so thresholding erodes grain, not
///     structure.
///   • Sharpening boosts the (denoised) coefficients per level: Radius shifts
///     weight toward coarser levels, Detail gates small-amplitude coefficients
///     (strong edges always sharpen), Masking gates on the residual's gradient
///     (smooth areas drop out), the total lift is tanh-limited, and the chroma
///     de-fringe pull is kept from the old engine (demosaic fringing guard).
fn process_detail_plane(rgb: &[[f32; 3]], w: usize, h: usize, p: &DetailParams) -> Vec<[f32; 3]> {
    let mut luma: Vec<f32> = rgb
        .iter()
        .map(|c| luminance_f32(c[0], c[1], c[2]).clamp(0.0, 1.0))
        .collect();
    let mut chroma: Vec<[f32; 3]> = rgb
        .iter()
        .zip(&luma)
        .map(|(c, &l)| [c[0] - l, c[1] - l, c[2] - l])
        .collect();

    if p.color_nr > 0.001 {
        for ch in 0..3 {
            let plane: Vec<f32> = chroma.iter().map(|c| c[ch]).collect();
            let (res, details) = atrous_decompose(&plane, w, h, false);
            for (i, c) in chroma.iter_mut().enumerate() {
                let mut v = res[i];
                for (j, d) in details.iter().enumerate() {
                    v += d[i] * (1.0 - p.color_nr * CHROMA_NR_ATTEN[j]);
                }
                c[ch] = v;
            }
        }
    }

    if p.nr > 0.001 || p.amount > 0.001 {
        let (res, mut details) = atrous_decompose(&luma, w, h, true);

        if p.nr > 0.001 {
            let mut t = p.nr * NR_LUMA_THRESH;
            for d in details.iter_mut() {
                let t2 = t * t;
                for v in d.iter_mut() {
                    let a = v.abs();
                    // Non-negative garrote: kills sub-threshold coefficients,
                    // barely touches the large (edge/texture) ones.
                    *v = if a <= t {
                        0.0
                    } else {
                        *v * (1.0 - t2 / (a * a))
                    };
                }
                t *= NR_LEVEL_DECAY;
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
                        delta += p.amount * level_gain[j] * weight * dv;
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
                    luma[i] = (base + delta).clamp(0.0, 1.0);

                    let edge_gate = smootherstep(0.006, 0.055, edge_mag);
                    let fr = (p.amount * edge_gate * 0.4).min(0.6) * mask;
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
                *l = (res[i] + d_sum).clamp(0.0, 1.0);
            }
        }
    }

    (0..w * h)
        .map(|i| {
            let l = luma[i];
            [
                (l + chroma[i][0]).clamp(0.0, 1.0),
                (l + chroma[i][1]).clamp(0.0, 1.0),
                (l + chroma[i][2]).clamp(0.0, 1.0),
            ]
        })
        .collect()
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
            let out = process_detail_plane(&plane, hw, _hh, &p);
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
    // Texture: fine-detail soft contrast. Still a point-op — a dedicated
    // fine-scale base is a follow-up; Clarity/Defog below are spatial.
    if settings.texture.abs() > 0.001 {
        let luma = luminance_f32(*r, *g, *b).clamp(0.0, 1.0);
        let mid = bell(luma, 0.5, 0.56);
        let texture = eased_control(settings.texture) * (CONTROL_LIMIT / 300.0);
        apply_soft_contrast(r, g, b, (texture * mid * 0.55).clamp(-0.62, 0.78), 0.85);
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

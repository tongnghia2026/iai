//! Spatial colour machinery: the edge-aware guided filter and the low-pass /
//! proxy pyramid behind the regional colour stage and the fast live preview.
//! Work runs on a downsampled proxy (guided-lowpass then bilinear upsample) so
//! region edits stay soft across colour patches while holding object edges.

use super::*;
use crate::core::color::luminance_f32;
use crate::core::tile::{TileMap, TILE_SIZE};
use rayon::prelude::*;
use std::sync::Arc;

/// Build, for one tile, the three buffers the colour stage needs (each
/// `valid_w * valid_h`): the tone-mapped pixels (`toned`), their edge-aware
/// regional low-pass (`region`), and the colour-adjusted region (`adjusted`).
///
/// The expensive, smooth part — the guided filter plus the per-pixel oklab/HSL
/// colour transform — is computed on a `COLOR_DOWNSAMPLE`× proxy and bilinear
/// upsampled, so the live preview is cheap and patch boundaries stay soft. A
/// `COLOR_REGION_RADIUS` halo is read from the *source* tilemap (edge-clamped,
/// tile lookups hoisted per span) so the result is seam-free across tiles.
///
/// `base_luma` (when the tone is local, i.e. H/S/W/B engaged) is the inner
/// region's regional luminance from `build_base_luma`. The `region`/`adjusted`
/// colour proxies stay built on GLOBAL tone (only their DIFFERENCE — the colour
/// change — is used, so the tone base cancels), but the per-pixel `toned` base
/// is rebuilt with the REGIONAL local-adaptation (`apply_local`) so Shadows/
/// Blacks lift as a whole exactly like the tone-only path and the GPU preview —
/// no jump when the Mixer engages.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_color_lowpass(
    source: &TileMap,
    tone: &Option<ToneData>,
    settings: &DevelopSettings,
    base_x: u32,
    base_y: u32,
    valid_w: u32,
    valid_h: u32,
    interp_tone: bool,
    base_luma: Option<&[f32]>,
) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<[f32; 3]>) {
    let r = COLOR_REGION_RADIUS;
    let vw = valid_w as usize;
    let vh = valid_h as usize;
    let hw = vw + 2 * r;
    let hh = vh + 2 * r;
    let wmax = source.width.saturating_sub(1) as i64;
    let hmax = source.height.saturating_sub(1) as i64;

    let mut halo = vec![[0.0f32; 3]; hw * hh];
    for hy in 0..hh {
        let gy = ((base_y as i64) + hy as i64 - r as i64).clamp(0, hmax) as u32;
        let ty_tile = (gy / TILE_SIZE) as i32;
        let ly = gy % TILE_SIZE;
        // gx grows monotonically across the row (clamped at both ends), so the
        // overlapped source tile changes only a couple of times — cache it.
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
            let [mut rf, mut gf, mut bf] = match cur_tile {
                Some(t) => {
                    // get_pixel16/65535 == get_pixel/255 for 8-bit tiles; preserves
                    // full precision for 16-bit masters.
                    let (r16, g16, b16, _a) = t.get_pixel16(gx % TILE_SIZE, ly);
                    [
                        r16 as f32 / 65535.0,
                        g16 as f32 / 65535.0,
                        b16 as f32 / 65535.0,
                    ]
                }
                None => [0.0, 0.0, 0.0],
            };
            if let Some(t) = tone {
                if interp_tone {
                    t.apply_interp(&mut rf, &mut gf, &mut bf);
                } else {
                    t.apply(&mut rf, &mut gf, &mut bf);
                }
            }
            halo[hy * hw + hx] = [rf.clamp(0.0, 1.0), gf.clamp(0.0, 1.0), bf.clamp(0.0, 1.0)];
        }
    }

    // Region-aware adjustment on a downscaled proxy: guided filter + apply_color.
    let s = COLOR_DOWNSAMPLE.max(1);
    let (low, lw, lh) = downsample_box(&halo, hw, hh, s);
    let region_low = guided_lowpass(&low, lw, lh, (r / s).max(1), COLOR_GUIDED_EPS);
    let adjusted_low = apply_color_to_region(&region_low, settings, lw, lh);
    let region_full = upsample_bilinear(&region_low, lw, lh, hw, hh, s);
    let adjusted_full = upsample_bilinear(&adjusted_low, lw, lh, hw, hh, s);

    let mut toned_inner = vec![[0.0f32; 3]; vw * vh];
    let mut region_inner = vec![[0.0f32; 3]; vw * vh];
    let mut adjusted_inner = vec![[0.0f32; 3]; vw * vh];
    // Regional local-adaptation applies to the per-pixel `toned` base when
    // H/S/W/B are engaged (matches apply_pixel's local branch and the GPU
    // shader's local tone stage). Falls back to the global-toned halo otherwise.
    let local = base_luma.filter(|_| tone.as_ref().is_some_and(|t| t.is_local));
    for y in 0..vh {
        for x in 0..vw {
            let hi = (y + r) * hw + (x + r);
            let oi = y * vw + x;
            toned_inner[oi] = match (local, tone.as_ref()) {
                (Some(bl), Some(t)) => {
                    let (r16, g16, b16, _a) =
                        source.get_pixel16(base_x + x as u32, base_y + y as u32);
                    let mut rf = r16 as f32 / 65535.0;
                    let mut gf = g16 as f32 / 65535.0;
                    let mut bf = b16 as f32 / 65535.0;
                    if interp_tone {
                        t.apply_local_interp(&mut rf, &mut gf, &mut bf, bl[oi]);
                    } else {
                        t.apply_local(&mut rf, &mut gf, &mut bf, bl[oi]);
                    }
                    [rf.clamp(0.0, 1.0), gf.clamp(0.0, 1.0), bf.clamp(0.0, 1.0)]
                }
                _ => halo[hi],
            };
            region_inner[oi] = region_full[hi];
            adjusted_inner[oi] = adjusted_full[hi];
        }
    }
    (toned_inner, region_inner, adjusted_inner)
}

/// Build the per-tile regional base luminance for the Shadows/Highlights local
/// adaptation: WB+Exposure luma over a `TONE_REGION_RADIUS` halo (read from the
/// source, edge-clamped, tile lookups hoisted), edge-aware blurred (guided) so a
/// dark region averages out its texture without haloing across strong edges.
/// Returns `valid_w * valid_h`.
pub(crate) fn build_base_luma(
    source: &TileMap,
    tone: Option<&ToneData>,
    base_x: u32,
    base_y: u32,
    valid_w: u32,
    valid_h: u32,
) -> Vec<f32> {
    let r = TONE_REGION_RADIUS;
    let vw = valid_w as usize;
    let vh = valid_h as usize;
    let hw = vw + 2 * r;
    let hh = vh + 2 * r;
    let wmax = source.width.saturating_sub(1) as i64;
    let hmax = source.height.saturating_sub(1) as i64;

    let mut luma = vec![0.0f32; hw * hh];
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
            let [mut rf, mut gf, mut bf] = match cur_tile {
                Some(t) => {
                    // get_pixel16/65535 == get_pixel/255 for 8-bit tiles; preserves
                    // full precision for 16-bit masters.
                    let (r16, g16, b16, _a) = t.get_pixel16(gx % TILE_SIZE, ly);
                    [
                        r16 as f32 / 65535.0,
                        g16 as f32 / 65535.0,
                        b16 as f32 / 65535.0,
                    ]
                }
                None => [0.0, 0.0, 0.0],
            };
            if let Some(t) = tone {
                t.apply_wb_ev(&mut rf, &mut gf, &mut bf);
            }
            luma[hy * hw + hx] = luminance_f32(rf, gf, bf).clamp(0.0, 1.0);
        }
    }

    // Edge-aware regional luma on a downscaled proxy: guided filter on 1/s, then
    // bilinear upsample. The signal is already low-frequency (24px blur) so the
    // proxy barely changes the result while cutting the box-blur passes by ~s².
    let s = TONE_DOWNSAMPLE.max(1);
    let (low, lw, lh) = downsample_box_plane(&luma, hw, hh, s);
    let region_low = guided_lowpass_plane(&low, lw, lh, (r / s).max(1), TONE_GUIDED_EPS);
    let base = upsample_bilinear_plane(&region_low, lw, lh, hw, hh, s);
    let mut inner = vec![0.0f32; vw * vh];
    for y in 0..vh {
        for x in 0..vw {
            inner[y * vw + x] = base[(y + r) * hw + (x + r)];
        }
    }
    inner
}

/// Average-pool a scalar plane by factor `s` (edge blocks pool what is available).
/// The single-channel sibling of `downsample_box`.
fn downsample_box_plane(buf: &[f32], w: usize, h: usize, s: usize) -> (Vec<f32>, usize, usize) {
    let lw = w.div_ceil(s);
    let lh = h.div_ceil(s);
    let mut out = vec![0.0f32; lw * lh];
    for ly in 0..lh {
        let y0 = ly * s;
        let y1 = (y0 + s).min(h);
        for lx in 0..lw {
            let x0 = lx * s;
            let x1 = (x0 + s).min(w);
            let mut acc = 0.0f32;
            for y in y0..y1 {
                for x in x0..x1 {
                    acc += buf[y * w + x];
                }
            }
            out[ly * lw + lx] = acc / (((x1 - x0) * (y1 - y0)) as f32);
        }
    }
    (out, lw, lh)
}

/// Bilinear upsample a scalar proxy back to `out_w × out_h` (pixel-centre
/// aligned). The single-channel sibling of `upsample_bilinear`.
fn upsample_bilinear_plane(
    low: &[f32],
    lw: usize,
    lh: usize,
    out_w: usize,
    out_h: usize,
    s: usize,
) -> Vec<f32> {
    let mut out = vec![0.0f32; out_w * out_h];
    let sf = s as f32;
    for y in 0..out_h {
        let fy = (((y as f32 + 0.5) / sf) - 0.5).clamp(0.0, (lh - 1) as f32);
        let y0 = fy.floor() as usize;
        let y1 = (y0 + 1).min(lh - 1);
        let wy = fy - y0 as f32;
        for x in 0..out_w {
            let fx = (((x as f32 + 0.5) / sf) - 0.5).clamp(0.0, (lw - 1) as f32);
            let x0 = fx.floor() as usize;
            let x1 = (x0 + 1).min(lw - 1);
            let wx = fx - x0 as f32;
            let top = low[y0 * lw + x0] + (low[y0 * lw + x1] - low[y0 * lw + x0]) * wx;
            let bot = low[y1 * lw + x0] + (low[y1 * lw + x1] - low[y1 * lw + x0]) * wx;
            out[y * out_w + x] = top + (bot - top) * wy;
        }
    }
    out
}

/// Full-layer regional base luminance for the GPU local-tone preview (Shadows/
/// Highlights/Whites/Blacks). WB+Exposure luma is box-pooled by `s` over the whole
/// layer, then edge-aware (guided) blurred at `TONE_REGION_RADIUS / s`. The shader
/// samples this proxy (bilinear) per pixel and runs the same `apply_local` math the
/// CPU bake runs — so the live GPU preview matches the committed pixels.
///
/// This mirrors the per-tile `build_base_luma` the CPU bake uses; both are a 24px
/// guided blur of the same WB+EV luma field, so swapping one for the other is
/// perceptually a no-op (no pop when OK bakes). Returns the proxy plus its
/// dimensions. Live preview splits this into cached [`build_region_luma_base`] +
/// per-frame [`finish_region_luma`]; this combined form is the test reference.
#[cfg(test)]
pub(crate) fn build_region_luma_proxy(
    source: &TileMap,
    tone: &ToneData,
    s: usize,
) -> (Vec<f32>, usize, usize) {
    let (base, pw, ph) = build_region_luma_base(source, s);
    (finish_region_luma(&base, pw, ph, tone, s.max(1)), pw, ph)
}

/// Tone-INDEPENDENT half of the local-adaptation base luma: box-AVERAGE the raw RGB
/// over each s×s block of the FULL image. This is the expensive full-image read, so
/// the live preview caches it — only a layer change invalidates it, NOT an
/// Exposure/WB drag. WB+Exposure + the guided low-pass are applied per frame by
/// [`finish_region_luma`] on this small averaged buffer, so dragging Exposure no
/// longer rebuilds (and no longer leaves the local base on a stale exposure, which
/// snapped against the fresh per-pixel tone = an erratic jitter). Tile
/// boundaries (256) are multiples of `s`, so a block never straddles two tiles.
pub(crate) fn build_region_luma_base(source: &TileMap, s: usize) -> (Vec<[f32; 3]>, usize, usize) {
    let w = source.width as usize;
    let h = source.height as usize;
    let s = s.max(1);
    if w == 0 || h == 0 {
        return (Vec::new(), 0, 0);
    }
    let pw = w.div_ceil(s);
    let ph = h.div_ceil(s);
    let tsize = TILE_SIZE as usize;
    let mut low = vec![[0.0f32; 3]; pw * ph];
    low.par_chunks_mut(pw).enumerate().for_each(|(py, row)| {
        let sy0 = py * s;
        let sy1 = (sy0 + s).min(h);
        for (px, slot) in row.iter_mut().enumerate() {
            let sx0 = px * s;
            let sx1 = (sx0 + s).min(w);
            let mut acc = [0.0f32; 3];
            let n = ((sx1 - sx0) * (sy1 - sy0)) as f32;
            for yy in sy0..sy1 {
                let ty_tile = (yy / tsize) as i32;
                let ly = (yy % tsize) as u32;
                let mut cached_tx = i32::MIN;
                let mut cur_tile: Option<&Arc<crate::core::tile::Tile>> = None;
                for xx in sx0..sx1 {
                    let tx_tile = (xx / tsize) as i32;
                    if tx_tile != cached_tx {
                        cur_tile = source.tiles.get(&crate::core::tile::TilePos {
                            x: tx_tile,
                            y: ty_tile,
                        });
                        cached_tx = tx_tile;
                    }
                    if let Some(t) = cur_tile {
                        let (r8, g8, b8, _a) = t.get_pixel((xx % tsize) as u32, ly);
                        acc[0] += r8 as f32 / 255.0;
                        acc[1] += g8 as f32 / 255.0;
                        acc[2] += b8 as f32 / 255.0;
                    }
                }
            }
            *slot = if n > 0.0 {
                [acc[0] / n, acc[1] / n, acc[2] / n]
            } else {
                [0.0; 3]
            };
        }
    });
    (low, pw, ph)
}

/// Tone-DEPENDENT tail: WB+Exposure on the cached raw block averages → luminance →
/// edge-aware (guided) low-pass. Cheap (O(proxy px)), run every frame so the
/// local-adaptation base luma tracks the live Exposure/WB.
pub(crate) fn finish_region_luma(
    base: &[[f32; 3]],
    pw: usize,
    ph: usize,
    tone: &ToneData,
    s: usize,
) -> Vec<f32> {
    let low: Vec<f32> = base
        .par_iter()
        .map(|p| {
            let (mut rf, mut gf, mut bf) = (p[0], p[1], p[2]);
            tone.apply_wb_ev(&mut rf, &mut gf, &mut bf);
            luminance_f32(rf, gf, bf).clamp(0.0, 1.0)
        })
        .collect();
    let r = (TONE_REGION_RADIUS / s.max(1)).max(1);
    guided_lowpass_plane(&low, pw, ph, r, TONE_GUIDED_EPS)
}

/// Bilinear sample a scalar proxy of size `pw × ph` at fractional proxy coords
/// (`fx`, `fy`). Edge-clamped. The CPU twin of the shader's region-texture lookup,
/// so both read the proxy identically.
pub(crate) fn sample_plane_bilinear(plane: &[f32], pw: usize, ph: usize, fx: f32, fy: f32) -> f32 {
    if pw == 0 || ph == 0 {
        return 0.0;
    }
    let fx = fx.clamp(0.0, (pw - 1) as f32);
    let fy = fy.clamp(0.0, (ph - 1) as f32);
    let x0 = fx.floor() as usize;
    let y0 = fy.floor() as usize;
    let x1 = (x0 + 1).min(pw - 1);
    let y1 = (y0 + 1).min(ph - 1);
    let wx = fx - x0 as f32;
    let wy = fy - y0 as f32;
    let top = plane[y0 * pw + x0] + (plane[y0 * pw + x1] - plane[y0 * pw + x0]) * wx;
    let bot = plane[y1 * pw + x0] + (plane[y1 * pw + x1] - plane[y1 * pw + x0]) * wx;
    top + (bot - top) * wy
}

/// Full-layer colour-stage proxies for the GPU preview of Vibrance/Saturation/
/// Color Mixer. The tone-mapped layer is box-pooled by `s`, edge-aware (guided)
/// blurred into a `region`, and `apply_color` is run on the region to make
/// `adjusted`. The shader samples both (bilinear) and runs the same
/// `finish_colored_pixel` recombination the CPU bake runs — so the de-blocked live
/// preview matches the committed pixels (no pop). Built once per settings change.
///
/// Returns `(region, adjusted, pw, ph)`, each `pw*ph` RGB. Mirrors the per-tile
/// `build_color_lowpass` (both feed `finish_colored_pixel`); the full-layer proxy
/// is seam-free by construction. Production builds the two halves separately (the
/// `region` is cached across a colour drag); this convenience wrapper is for tests.
#[cfg(test)]
pub(crate) fn build_color_proxies(
    source: &TileMap,
    tone: &Option<ToneData>,
    settings: &DevelopSettings,
    s: usize,
) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, usize, usize) {
    let (region, pw, ph) = build_color_region(source, tone, s);
    let adjusted = apply_color_to_region(&region, settings, pw, ph);
    (region, adjusted, pw, ph)
}

/// The cacheable half of `build_color_proxies`: the tone-mapped layer box-pooled by
/// `s` and edge-aware (guided) blurred into `region`. Depends only on the tone
/// stage (not the colour sliders), so it can be reused across a colour drag — this
/// is the expensive part (it reads every source pixel). Returns `(region, pw, ph)`.
#[cfg(test)]
pub(crate) fn build_color_region(
    source: &TileMap,
    tone: &Option<ToneData>,
    s: usize,
) -> (Vec<[f32; 3]>, usize, usize) {
    let w = source.width as usize;
    let h = source.height as usize;
    let s = s.max(1);
    if w == 0 || h == 0 {
        return (Vec::new(), 0, 0);
    }
    let pw = w.div_ceil(s);
    let ph = h.div_ceil(s);

    // Box-average the TONE-MAPPED source over each s×s block (parallel over rows).
    // The tile is resolved PER PIXEL (cached across the inner span) because
    // COLOR_DOWNSAMPLE need not divide TILE_SIZE — an s×s block can straddle a tile
    // boundary, and a single cached tile would misread `xx % TILE_SIZE` on the far
    // side, painting wrong-colour stripes at every seam (the preview-only banding
    // the bake's per-pixel lookup never had).
    let tsize = TILE_SIZE as usize;
    let mut toned_low = vec![[0.0f32; 3]; pw * ph];
    toned_low
        .par_chunks_mut(pw)
        .enumerate()
        .for_each(|(py, row)| {
            let sy0 = py * s;
            let sy1 = (sy0 + s).min(h);
            for (px, slot) in row.iter_mut().enumerate() {
                let sx0 = px * s;
                let sx1 = (sx0 + s).min(w);
                let mut acc = [0.0f32; 3];
                let n = ((sx1 - sx0) * (sy1 - sy0)) as f32;
                for yy in sy0..sy1 {
                    let ty_tile = (yy / tsize) as i32;
                    let ly = (yy % tsize) as u32;
                    let mut cached_tx = i32::MIN;
                    let mut cur_tile: Option<&Arc<crate::core::tile::Tile>> = None;
                    for xx in sx0..sx1 {
                        let tx_tile = (xx / tsize) as i32;
                        if tx_tile != cached_tx {
                            cur_tile = source.tiles.get(&crate::core::tile::TilePos {
                                x: tx_tile,
                                y: ty_tile,
                            });
                            cached_tx = tx_tile;
                        }
                        if let Some(t) = cur_tile {
                            let (r8, g8, b8, _a) = t.get_pixel((xx % tsize) as u32, ly);
                            let (mut rf, mut gf, mut bf) =
                                (r8 as f32 / 255.0, g8 as f32 / 255.0, b8 as f32 / 255.0);
                            if let Some(tone) = tone {
                                tone.apply(&mut rf, &mut gf, &mut bf);
                            }
                            acc[0] += rf.clamp(0.0, 1.0);
                            acc[1] += gf.clamp(0.0, 1.0);
                            acc[2] += bf.clamp(0.0, 1.0);
                        }
                    }
                }
                *slot = if n > 0.0 {
                    [acc[0] / n, acc[1] / n, acc[2] / n]
                } else {
                    [0.0; 3]
                };
            }
        });

    let r = (COLOR_REGION_RADIUS / s).max(1);
    let region = guided_lowpass(&toned_low, pw, ph, r, COLOR_GUIDED_EPS);
    (region, pw, ph)
}

/// The cheap, per-frame half: run `apply_color` on a cached `region`. Parallel,
/// since it is recomputed on every colour-slider tick.
pub fn apply_color_to_region(
    region: &[[f32; 3]],
    settings: &DevelopSettings,
    pw: usize,
    ph: usize,
) -> Vec<[f32; 3]> {
    let curves = build_mixer_curves_opt(settings);
    let guided_controls = guided_mixer_controls(region, settings, pw, ph);
    let mut adjusted: Vec<[f32; 3]> = if let Some(controls) = guided_controls {
        region
            .par_iter()
            .zip(controls.par_iter())
            .map(|(p, controls)| {
                let (mut r0, mut g0, mut b0) = (p[0], p[1], p[2]);
                apply_color_with_mixer_controls(settings, *controls, &mut r0, &mut g0, &mut b0);
                [r0.clamp(0.0, 1.0), g0.clamp(0.0, 1.0), b0.clamp(0.0, 1.0)]
            })
            .collect()
    } else {
        region
            .par_iter()
            .map(|p| {
                let (mut r0, mut g0, mut b0) = (p[0], p[1], p[2]);
                apply_color(settings, curves.as_ref(), &mut r0, &mut g0, &mut b0);
                [r0.clamp(0.0, 1.0), g0.clamp(0.0, 1.0), b0.clamp(0.0, 1.0)]
            })
            .collect()
    };
    if should_suppress_color_edges(settings) {
        suppress_edge_correction(region, &mut adjusted, pw, ph);
    }
    adjusted
}

/// Build the three spatially coherent V2 mixer control planes packed as RGB.
/// `None` means the caller should retain the legacy/direct colour path.
pub fn guided_mixer_controls(
    samples: &[[f32; 3]],
    settings: &DevelopSettings,
    pw: usize,
    ph: usize,
) -> Option<Vec<[f32; 3]>> {
    let curves = build_mixer_curves_opt(settings)?;
    if settings.develop_engine_version != DevelopEngineVersion::Develop3
        || curves.algorithm != ColorMixerAlgorithm::V2
        || pw <= 1
        || ph <= 1
        || samples.len() != pw * ph
    {
        return None;
    }
    let guide: Vec<f32> = samples
        .par_iter()
        .map(|p| luminance_f32(p[0], p[1], p[2]).clamp(0.0, 1.0))
        .collect();
    let controls: Vec<[f32; 3]> = samples
        .par_iter()
        .map(|p| {
            let lab = crate::core::perceptual_color::linear_srgb_to_oklab([
                srgb_to_linear(p[0]),
                srgb_to_linear(p[1]),
                srgb_to_linear(p[2]),
            ]);
            let c = crate::core::perceptual_color::PerceptualColor::from_oklab(lab);
            let (h, s, l) = mixer_adjustments_for_perceptual(&curves, c);
            [h, s, l]
        })
        .collect();
    let hue: Vec<f32> = controls.iter().map(|c| c[0]).collect();
    let sat: Vec<f32> = controls.iter().map(|c| c[1]).collect();
    let lum: Vec<f32> = controls.iter().map(|c| c[2]).collect();
    // Hue/Saturation follow small colour regions; Luminance deliberately
    // sees a broader lighting neighbourhood. Radii are clamped for tiny
    // test/preview grids and are independent of the source RGB blur.
    let short_r = 4usize
        .min(pw.saturating_sub(1))
        .min(ph.saturating_sub(1))
        .max(1);
    let long_r = 12usize
        .min(pw.saturating_sub(1))
        .min(ph.saturating_sub(1))
        .max(1);
    let hue = guided_filter_plane(&guide, &hue, pw, ph, short_r, 0.0025);
    let sat = guided_filter_plane(&guide, &sat, pw, ph, short_r, 0.0025);
    let lum = guided_filter_plane(&guide, &lum, pw, ph, long_r, 0.0025);
    Some(
        (0..samples.len())
            .into_par_iter()
            .map(|i| [hue[i], sat[i], lum[i]])
            .collect(),
    )
}

/// Luminance-band edits are already confined by the full-resolution mixer gate.
/// Applying the generic proxy edge fade as well creates an under-corrected strip
/// on the edited side (the doubled jaw/cheek contour).  This predicate is shared
/// by both the live full-layer proxy and the CPU tile/bake path so they cannot
/// drift again.
fn should_suppress_color_edges(settings: &DevelopSettings) -> bool {
    !settings.mixer_luminance.iter().any(|v| v.abs() > 0.001)
}

/// Edge/halo suppression of the low-res colour correction: a gradient weight
/// `gradient_weight = 1 - CLIP(gradient)`. Where the correction has a
/// strong local gradient (a real edge: hair↔skin, subject↔background) the
/// adjusted proxy is pulled back toward the unadjusted `region`, so the colour/
/// brightness edit FADES over the proxy's resolution as it approaches the
/// boundary instead of holding full strength up to a hard line. Only the smooth
/// low-res correction is shaped — the per-pixel detail re-added in
/// `finish_colored_pixel_f32` keeps interior sharpness — and a uniform region
/// has no internal gradient, so an object's interior takes its full edit. The
/// GPU reads this exact `adjusted` proxy, so preview and commit stay identical.
///
/// The gradient combines TWO signals (whichever suppresses more): the region's
/// own luma gradient (a pre-existing object edge, suppressed even under a uniform
/// edit) AND the gradient of the correction's LUMA. The second is what a Luminance
/// Red/Orange boost needs: it lifts warm skin but leaves the neutral background,
/// and skin can sit at the SAME luma as the background before the edit — so the
/// region-luma term alone measures ~0 at that boundary and the boost held to a
/// hard, haloed rim. The Δbrightness gradient is large exactly where the edit
/// starts/stops (skin↔background), so the boost now tapers into a soft "stop".
///
/// The combined suppression is floored at `EDGE_FADE_FLOOR` so the taper softens
/// the boundary without eating a dark rim deep into the colour being brightened.
pub(crate) fn suppress_edge_correction(
    region: &[[f32; 3]],
    adjusted: &mut [[f32; 3]],
    pw: usize,
    ph: usize,
) {
    if pw < 3 || ph < 3 || region.len() != pw * ph || adjusted.len() != pw * ph {
        return;
    }
    let lum = |p: [f32; 3]| luminance_f32(p[0], p[1], p[2]);
    // Δbrightness per texel = luma(adjusted) − luma(region); read before the
    // mutation pass below. Its spatial gradient marks a brightness boundary the
    // EDIT creates, even at constant region luma (skin lifted, grey background
    // left). ~0 for Saturation/Hue edits, so those stay unshaped here.
    let corr_luma: Vec<f32> = adjusted
        .iter()
        .zip(region.iter())
        .map(|(a, r)| lum(*a) - lum(*r))
        .collect();
    let mut weight = vec![0.0f32; pw * ph];
    weight.par_chunks_mut(pw).enumerate().for_each(|(y, row)| {
        let y0 = y.saturating_sub(1);
        let y1 = (y + 1).min(ph - 1);
        for (x, w) in row.iter_mut().enumerate() {
            let x0 = x.saturating_sub(1);
            let x1 = (x + 1).min(pw - 1);
            let gx = lum(region[y * pw + x1]) - lum(region[y * pw + x0]);
            let gy = lum(region[y1 * pw + x]) - lum(region[y0 * pw + x]);
            let g_region = (gx * gx + gy * gy).sqrt();
            // Gradient of the correction's luma: catches an edit-created
            // brightness edge the region-luma term misses.
            let cgx = corr_luma[y * pw + x1] - corr_luma[y * pw + x0];
            let cgy = corr_luma[y1 * pw + x] - corr_luma[y0 * pw + x];
            let g_corr = (cgx * cgx + cgy * cgy).sqrt();
            // Take whichever signal suppresses MORE (min weight): a pre-existing
            // object edge (region luma) or an edit-created brightness edge, each
            // with its own knees.
            let wr = 1.0 - smootherstep(EDGE_SUPPRESS_LO, EDGE_SUPPRESS_HI, g_region);
            let wc = 1.0 - smootherstep(EDIT_SUPPRESS_LO, EDIT_SUPPRESS_HI, g_corr);
            // Lift into [EDGE_FADE_FLOOR, 1]: soften the boundary without carving
            // the boost out of the colour interior.
            *w = EDGE_FADE_FLOOR + (1.0 - EDGE_FADE_FLOOR) * wr.min(wc);
        }
    });
    // Blur the weight so the fade band is a few texels wide — a gradual taper
    // approaching the edge, not a 1-texel notch right at it.
    let weight = box_blur_plane(&weight, pw, ph, 1);
    adjusted
        .par_iter_mut()
        .zip(region.par_iter())
        .zip(weight.par_iter())
        .for_each(|((a, r), &w)| {
            a[0] = r[0] + (a[0] - r[0]) * w;
            a[1] = r[1] + (a[1] - r[1]) * w;
            a[2] = r[2] + (a[2] - r[2]) * w;
        });
}

pub fn fast_preview_downsample(width: u32, height: u32) -> usize {
    fast_preview_downsample_for_target(
        width,
        height,
        crate::core::hw::fast_preview_target_pixels(),
        FAST_PREVIEW_MIN_DOWNSAMPLE,
    )
}

/// Detail's wavelet/NR tail is heavier than Effects/Local point operations.
/// Keep its live proxy within two thirds of the normal tier budget so the same
/// model stays under the slider-to-frame latency gate without weakening the
/// commit kernel.
///
/// Unlike the general fast preview, Detail is allowed all the way down to a 1×
/// (native) proxy. The pixel-count budget already caps per-frame cost, so on the
/// small visible crop of a zoomed-in view the proxy becomes fine enough to show
/// the real, source-scale sharpening/NR live (paired with G6's source-scale
/// rescale it then matches the settled bake) — instead of the old 8× floor that
/// forced every Detail preview coarse even at 100 %, where fine detail lives.
pub(crate) fn detail_preview_downsample(width: u32, height: u32) -> usize {
    let target = (crate::core::hw::fast_preview_target_pixels() * 2 / 3).max(1);
    fast_preview_downsample_for_target(width, height, target, 1)
}

fn fast_preview_downsample_for_target(width: u32, height: u32, target: usize, min: usize) -> usize {
    let pixels = (width as usize).saturating_mul(height as usize).max(1);
    let s = ((pixels as f32 / target as f32).sqrt().ceil() as usize)
        .clamp(min.max(1), FAST_PREVIEW_MAX_DOWNSAMPLE);
    s.max(1)
}

/// Full colour-stage live-preview region for a viewport box = raw box-AVERAGE
/// ([`build_color_base_box`]) + tone/guided low-pass ([`tone_lowpass_color_region`]),
/// the SAME de-blocking low-pass the commit builds in [`build_color_lowpass`]. The
/// live preview splits these two so the (expensive) average is cached while tone is
/// re-applied per frame; this convenience wrapper (test only) keeps the combined
/// reference for `color_region_box_deblocks_like_commit`.
#[cfg(test)]
pub(crate) fn build_color_region_box(
    source: &TileMap,
    tone: &Option<ToneData>,
    origin_x: u32,
    origin_y: u32,
    region_w: u32,
    region_h: u32,
    s: usize,
) -> (Vec<[f32; 3]>, usize, usize) {
    let (base, pw, ph) = build_color_base_box(source, origin_x, origin_y, region_w, region_h, s);
    (
        tone_lowpass_color_region(&base, pw, ph, tone, s.max(1)),
        pw,
        ph,
    )
}

/// The tone-INDEPENDENT half of the colour region: box-AVERAGE the raw source over
/// each `s`×`s` block of the viewport box. This is the expensive part (reads every
/// source pixel in the box), so the live preview caches it across a Develop
/// session — only a viewport/layer change invalidates it. Tone (Contrast, Curve,
/// Shadows/Blacks…) is NOT applied here; it is applied per frame by
/// [`tone_lowpass_color_region`] on this small averaged buffer, so a Tone/Curve drag
/// no longer rebuilds this average (and no longer leaves the colour base on a stale
/// tone — the old "colour preview ≠ commit" jump). The tile is resolved per pixel
/// (cached across the inner span) because `s` need not divide `TILE_SIZE`.
pub(crate) fn build_color_base_box(
    source: &TileMap,
    origin_x: u32,
    origin_y: u32,
    region_w: u32,
    region_h: u32,
    s: usize,
) -> (Vec<[f32; 3]>, usize, usize) {
    let w = source.width as usize;
    let h = source.height as usize;
    let s = s.max(1);
    if w == 0 || h == 0 || region_w == 0 || region_h == 0 {
        return (Vec::new(), 0, 0);
    }
    let ox = (origin_x as usize).min(w - 1);
    let oy = (origin_y as usize).min(h - 1);
    let rw = (region_w as usize).min(w - ox).max(1);
    let rh = (region_h as usize).min(h - oy).max(1);
    let pw = rw.div_ceil(s);
    let ph = rh.div_ceil(s);
    let tsize = TILE_SIZE as usize;

    let mut low = vec![[0.0f32; 3]; pw * ph];
    low.par_chunks_mut(pw).enumerate().for_each(|(py, row)| {
        let sy0 = oy + py * s;
        let sy1 = (sy0 + s).min(oy + rh).min(h);
        for (px, slot) in row.iter_mut().enumerate() {
            let sx0 = ox + px * s;
            let sx1 = (sx0 + s).min(ox + rw).min(w);
            let mut acc = [0.0f32; 3];
            let n = (((sx1 - sx0) * (sy1 - sy0)) as f32).max(1.0);
            for yy in sy0..sy1 {
                let ty_tile = (yy / tsize) as i32;
                let ly = (yy % tsize) as u32;
                let mut cached_tx = i32::MIN;
                let mut cur_tile: Option<&Arc<crate::core::tile::Tile>> = None;
                for xx in sx0..sx1 {
                    let tx_tile = (xx / tsize) as i32;
                    if tx_tile != cached_tx {
                        cur_tile = source.tiles.get(&crate::core::tile::TilePos {
                            x: tx_tile,
                            y: ty_tile,
                        });
                        cached_tx = tx_tile;
                    }
                    if let Some(t) = cur_tile {
                        let (r8, g8, b8, _a) = t.get_pixel((xx % tsize) as u32, ly);
                        acc[0] += r8 as f32 / 255.0;
                        acc[1] += g8 as f32 / 255.0;
                        acc[2] += b8 as f32 / 255.0;
                    }
                }
            }
            *slot = [acc[0] / n, acc[1] / n, acc[2] / n];
        }
    });
    (low, pw, ph)
}

/// The tone-DEPENDENT tail of the colour region: apply the tone stage to a cached raw
/// [`build_color_base_box`] average, then the edge-aware (guided) low-pass. Cheap
/// (O(proxy pixels)), so it runs every frame — the colour preview's tone base always
/// matches the shader's per-pixel tone (`d = toned - region` in `dev_finish_colored`
/// stays a true high-frequency detail term, so there is no stale-tone colour shift
/// during a Tone/Curve drag).
pub(crate) fn tone_lowpass_color_region(
    base: &[[f32; 3]],
    pw: usize,
    ph: usize,
    tone: &Option<ToneData>,
    s: usize,
) -> Vec<[f32; 3]> {
    let mut toned = base.to_vec();
    if let Some(t) = tone {
        toned.par_iter_mut().for_each(|p| {
            let (mut rf, mut gf, mut bf) = (p[0], p[1], p[2]);
            t.apply(&mut rf, &mut gf, &mut bf);
            *p = [rf.clamp(0.0, 1.0), gf.clamp(0.0, 1.0), bf.clamp(0.0, 1.0)];
        });
    }
    let r = (COLOR_REGION_RADIUS / s.max(1)).max(1);
    guided_lowpass(&toned, pw, ph, r, COLOR_GUIDED_EPS)
}

/// Ultra-cheap live-preview base for the spatial chain. Effects/Local use one
/// centre sample per block; Detail requests a five-tap corners+centre average
/// to prefilter single-pixel colour aliases without turning this into an
/// O(full-image) box average.
pub(crate) fn build_fast_preview_region(
    source: &TileMap,
    tone: &Option<ToneData>,
    origin_x: u32,
    origin_y: u32,
    region_w: u32,
    region_h: u32,
    s: usize,
    antialias: bool,
) -> (Vec<[f32; 3]>, usize, usize) {
    let w = source.width as usize;
    let h = source.height as usize;
    let s = s.max(1);
    if w == 0 || h == 0 || region_w == 0 || region_h == 0 {
        return (Vec::new(), 0, 0);
    }
    let ox = (origin_x as usize).min(w - 1);
    let oy = (origin_y as usize).min(h - 1);
    let rw = (region_w as usize).min(w.saturating_sub(ox)).max(1);
    let rh = (region_h as usize).min(h.saturating_sub(oy)).max(1);
    let pw = rw.div_ceil(s);
    let ph = rh.div_ceil(s);
    let tsize = TILE_SIZE as usize;
    let mut out = vec![[0.0f32; 3]; pw * ph];
    out.par_chunks_mut(pw).enumerate().for_each(|(py, row)| {
        let block_y0 = oy + py * s;
        let block_y1 = (block_y0 + s).min(oy + rh).min(h);
        let span_y = block_y1.saturating_sub(block_y0).max(1);
        let ys = if antialias {
            [block_y0, block_y0 + span_y - 1]
        } else {
            [block_y0 + span_y / 2, block_y0 + span_y / 2]
        };
        for (px, slot) in row.iter_mut().enumerate() {
            let block_x0 = ox + px * s;
            let block_x1 = (block_x0 + s).min(ox + rw).min(w);
            let span_x = block_x1.saturating_sub(block_x0).max(1);
            let xs = if antialias {
                [block_x0, block_x0 + span_x - 1]
            } else {
                [block_x0 + span_x / 2, block_x0 + span_x / 2]
            };
            let taps = if antialias { 5.0 } else { 1.0 };
            let mut acc = [0.0f32; 3];
            for (yi, &sy) in ys.iter().enumerate() {
                for (xi, &sx) in xs.iter().enumerate() {
                    if !antialias && (yi != 0 || xi != 0) {
                        continue;
                    }
                    let ty_tile = (sy / tsize) as i32;
                    let tx_tile = (sx / tsize) as i32;
                    if let Some(t) = source.tiles.get(&crate::core::tile::TilePos {
                        x: tx_tile,
                        y: ty_tile,
                    }) {
                        let (r, g, b, _a) = t.get_pixel((sx % tsize) as u32, (sy % tsize) as u32);
                        let (mut rf, mut gf, mut bf) =
                            (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
                        if let Some(tone) = tone {
                            tone.apply(&mut rf, &mut gf, &mut bf);
                            clamp_unit(&mut rf, &mut gf, &mut bf);
                        }
                        acc[0] += rf;
                        acc[1] += gf;
                        acc[2] += bf;
                    }
                }
            }
            if antialias {
                let sx = block_x0 + (span_x - 1) / 2;
                let sy = block_y0 + (span_y - 1) / 2;
                let ty_tile = (sy / tsize) as i32;
                let tx_tile = (sx / tsize) as i32;
                if let Some(t) = source.tiles.get(&crate::core::tile::TilePos {
                    x: tx_tile,
                    y: ty_tile,
                }) {
                    let (r, g, b, _a) = t.get_pixel((sx % tsize) as u32, (sy % tsize) as u32);
                    let (mut rf, mut gf, mut bf) =
                        (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
                    if let Some(tone) = tone {
                        tone.apply(&mut rf, &mut gf, &mut bf);
                        clamp_unit(&mut rf, &mut gf, &mut bf);
                    }
                    acc[0] += rf;
                    acc[1] += gf;
                    acc[2] += bf;
                }
            }
            *slot = [acc[0] / taps, acc[1] / taps, acc[2] / taps];
        }
    });
    (out, pw, ph)
}

/// Fast live-preview half for non-Colour Develop groups. It uses the same
/// low-res proxy idea as Color Mixer: keep a cached regional RGB proxy, apply the
/// current tone/effects sliders only on that small buffer, then let the GPU
/// upsample and re-add full-res detail. Commit still uses the full CPU path.
pub(crate) fn apply_fast_preview_to_region(
    region: &[[f32; 3]],
    settings: &DevelopSettings,
    regional_luma: Option<(&[f32], usize, usize, u32)>,
    w: usize,
    h: usize,
    origin_x: u32,
    origin_y: u32,
    source_w: u32,
    source_h: u32,
    downsample: u32,
) -> Vec<[f32; 3]> {
    apply_fast_preview_to_region_with_detail(
        region,
        settings,
        regional_luma,
        w,
        h,
        origin_x,
        origin_y,
        source_w,
        source_h,
        downsample,
        |out| apply_detail_to_display_buffer(out, w, h, settings, downsample.max(1)),
    )
}

/// Live-compositor variant that injects GPU Detail after the legacy display
/// chain's Local stage, matching the order used by the destructive commit.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_fast_preview_to_region_with_detail<F>(
    region: &[[f32; 3]],
    settings: &DevelopSettings,
    regional_luma: Option<(&[f32], usize, usize, u32)>,
    w: usize,
    h: usize,
    origin_x: u32,
    origin_y: u32,
    source_w: u32,
    source_h: u32,
    downsample: u32,
    apply_detail: F,
) -> Vec<[f32; 3]>
where
    F: FnOnce(&mut Vec<[f32; 3]>),
{
    let tone = tone_is_active(settings).then(|| build_tone_data(settings));
    let use_color = has_color(settings);
    let curves = build_mixer_curves_opt(settings);
    let inv_w = if source_w > 1 {
        1.0 / (source_w - 1) as f32
    } else {
        0.0
    };
    let inv_h = if source_h > 1 {
        1.0 / (source_h - 1) as f32
    } else {
        0.0
    };
    let sample_step = downsample.max(1);

    // Tone (and colour) first over the whole proxy, so the spatial effects can
    // take an edge-aware regional base from the TONED buffer below — the same
    // "base after tone" definition the bake and the colour path use.
    let toned: Vec<[f32; 3]> = region
        .par_iter()
        .enumerate()
        .map(|(idx, p)| {
            let mut r = p[0];
            let mut g = p[1];
            let mut b = p[2];
            if let Some(tone) = &tone {
                if tone.is_local {
                    if let Some((plane, pw, ph, plane_step)) = regional_luma {
                        let px = (idx % w) as u32;
                        let py = (idx / w) as u32;
                        let x = origin_x
                            .saturating_add(px.saturating_mul(sample_step))
                            .saturating_add(sample_step / 2)
                            .min(source_w.saturating_sub(1));
                        let y = origin_y
                            .saturating_add(py.saturating_mul(sample_step))
                            .saturating_add(sample_step / 2)
                            .min(source_h.saturating_sub(1));
                        let s = plane_step.max(1) as f32;
                        let base = sample_plane_bilinear(
                            plane,
                            pw,
                            ph,
                            (x as f32 + 0.5) / s - 0.5,
                            (y as f32 + 0.5) / s - 0.5,
                        );
                        tone.apply_local(&mut r, &mut g, &mut b, base);
                    } else {
                        tone.apply(&mut r, &mut g, &mut b);
                    }
                } else {
                    tone.apply(&mut r, &mut g, &mut b);
                }
                clamp_unit(&mut r, &mut g, &mut b);
            }
            if use_color {
                apply_color(settings, curves.as_ref(), &mut r, &mut g, &mut b);
                clamp_unit(&mut r, &mut g, &mut b);
            }
            [r, g, b]
        })
        .collect();
    if w == 0 || h == 0 || toned.is_empty() {
        return toned;
    }

    // Regional base for Clarity/Defog on the proxy grid: the guided radius is
    // the bake's full-res radius divided by the proxy downsample, so preview
    // and commit look at (approximately) the same neighbourhood size.
    let base_plane = if settings.has_spatial_effects() {
        let h = toned.len() / w;
        let luma: Vec<f32> = toned
            .iter()
            .map(|p| luminance_f32(p[0], p[1], p[2]).clamp(0.0, 1.0))
            .collect();
        let r = (TONE_REGION_RADIUS / sample_step as usize).max(1);
        Some(guided_lowpass_plane(&luma, w, h, r, TONE_GUIDED_EPS))
    } else {
        None
    };

    let mut out: Vec<[f32; 3]> = toned
        .par_iter()
        .enumerate()
        .map(|(idx, p)| {
            let mut r = p[0];
            let mut g = p[1];
            let mut b = p[2];
            {
                let px = (idx % w) as u32;
                let py = (idx / w) as u32;
                let x = origin_x
                    .saturating_add(px.saturating_mul(sample_step))
                    .saturating_add(sample_step / 2)
                    .min(source_w.saturating_sub(1));
                let y = origin_y
                    .saturating_add(py.saturating_mul(sample_step))
                    .saturating_add(sample_step / 2)
                    .min(source_h.saturating_sub(1));
                let base = base_plane
                    .as_ref()
                    .map(|bp| bp[idx])
                    .unwrap_or_else(|| luminance_f32(r, g, b).clamp(0.0, 1.0));
                apply_effects(settings, &mut r, &mut g, &mut b, x, y, inv_w, inv_h, base);
                clamp_unit(&mut r, &mut g, &mut b);
            }
            [r, g, b]
        })
        .collect();

    // Legacy commit applies Local before its separate Detail pass. Reuse the
    // exact precomputed Local plans and evaluate masks in source-image space so
    // viewport origin/downsample cannot move a gradient or radial mask.
    if settings.has_locals() {
        let plan = DevelopPlan::new(settings, source_w, source_h);
        out.par_iter_mut().enumerate().for_each(|(idx, p)| {
            let px = (idx % w) as u32;
            let py = (idx / w) as u32;
            let x = origin_x
                .saturating_add(px.saturating_mul(sample_step))
                .saturating_add(sample_step / 2)
                .min(source_w.saturating_sub(1));
            let y = origin_y
                .saturating_add(py.saturating_mul(sample_step))
                .saturating_add(sample_step / 2)
                .min(source_h.saturating_sub(1));
            let [r, g, b] = p;
            plan.apply_locals(r, g, b, x, y);
        });
    }
    // `sample_step` = proxy downsample, so Detail's wavelet radii are rescaled
    // back to source pixels and the drag preview matches the settled bake.
    apply_detail(&mut out);
    out
}

/// Average-pool an RGB buffer by factor `s` (edge blocks pool what is available).
/// Returns the proxy plus its dimensions.
fn downsample_box(buf: &[[f32; 3]], w: usize, h: usize, s: usize) -> (Vec<[f32; 3]>, usize, usize) {
    let lw = w.div_ceil(s);
    let lh = h.div_ceil(s);
    let mut out = vec![[0.0f32; 3]; lw * lh];
    for ly in 0..lh {
        let y0 = ly * s;
        let y1 = (y0 + s).min(h);
        for lx in 0..lw {
            let x0 = lx * s;
            let x1 = (x0 + s).min(w);
            let mut acc = [0.0f32; 3];
            let n = ((x1 - x0) * (y1 - y0)) as f32;
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = buf[y * w + x];
                    acc[0] += p[0];
                    acc[1] += p[1];
                    acc[2] += p[2];
                }
            }
            out[ly * lw + lx] = [acc[0] / n, acc[1] / n, acc[2] / n];
        }
    }
    (out, lw, lh)
}

/// Bilinear upsample an RGB proxy back to `out_w × out_h` (pixel-centre aligned),
/// which smoothly feathers the adjustment so patch boundaries are not hard-edged.
fn upsample_bilinear(
    low: &[[f32; 3]],
    lw: usize,
    lh: usize,
    out_w: usize,
    out_h: usize,
    s: usize,
) -> Vec<[f32; 3]> {
    let mut out = vec![[0.0f32; 3]; out_w * out_h];
    let sf = s as f32;
    for y in 0..out_h {
        let fy = (((y as f32 + 0.5) / sf) - 0.5).clamp(0.0, (lh - 1) as f32);
        let y0 = fy.floor() as usize;
        let y1 = (y0 + 1).min(lh - 1);
        let wy = fy - y0 as f32;
        for x in 0..out_w {
            let fx = (((x as f32 + 0.5) / sf) - 0.5).clamp(0.0, (lw - 1) as f32);
            let x0 = fx.floor() as usize;
            let x1 = (x0 + 1).min(lw - 1);
            let wx = fx - x0 as f32;
            let p00 = low[y0 * lw + x0];
            let p01 = low[y0 * lw + x1];
            let p10 = low[y1 * lw + x0];
            let p11 = low[y1 * lw + x1];
            let mut px = [0.0f32; 3];
            for c in 0..3 {
                let top = p00[c] + (p01[c] - p00[c]) * wx;
                let bot = p10[c] + (p11[c] - p10[c]) * wx;
                px[c] = top + (bot - top) * wy;
            }
            out[y * out_w + x] = px;
        }
    }
    out
}

/// Separable box blur via row/column prefix sums (edge-clamped). O(n) regardless
/// of radius — the speed floor that lets the colour stage run live.
pub(crate) fn box_blur_plane(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    if w == 0 || h == 0 {
        return Vec::new();
    }
    // Both passes are parallel sliding windows (f64 accumulators so the
    // add/remove drift stays far below f32 output precision). This runs per
    // frame during a Develop drag on planes north of a million texels, where
    // the old sequential prefix-sum version alone cost tens of ms.
    let mut tmp = vec![0.0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let s = &src[y * w..y * w + w];
        let mut acc: f64 = s[..=r.min(w - 1)].iter().map(|v| *v as f64).sum();
        for x in 0..w {
            let x0 = x.saturating_sub(r);
            let x1 = (x + r).min(w - 1);
            row[x] = (acc / (x1 - x0 + 1) as f64) as f32;
            if x + r + 1 < w {
                acc += s[x + r + 1] as f64;
            }
            if x >= r {
                acc -= s[x - r] as f64;
            }
        }
    });
    let mut out = vec![0.0f32; w * h];
    // Column pass in row-stripes: each stripe seeds per-column window sums for
    // its first row (O(w·(2r+1)), noise next to the main loop) then slides the
    // window down, so writes stay contiguous and rows vectorize.
    let stripe_rows = h.div_ceil(rayon::current_num_threads().max(1)).max(1);
    out.par_chunks_mut(w * stripe_rows)
        .enumerate()
        .for_each(|(si, stripe)| {
            let ys = si * stripe_rows;
            let mut acc = vec![0.0f64; w];
            for y in ys.saturating_sub(r)..=(ys + r).min(h - 1) {
                let row = &tmp[y * w..(y + 1) * w];
                for (a, v) in acc.iter_mut().zip(row) {
                    *a += *v as f64;
                }
            }
            for (dy, orow) in stripe.chunks_mut(w).enumerate() {
                let y = ys + dy;
                let y0 = y.saturating_sub(r);
                let y1 = (y + r).min(h - 1);
                let inv = 1.0 / (y1 - y0 + 1) as f64;
                for (o, a) in orow.iter_mut().zip(&acc) {
                    *o = (*a * inv) as f32;
                }
                if y + r + 1 < h {
                    let row = &tmp[(y + r + 1) * w..(y + r + 2) * w];
                    for (a, v) in acc.iter_mut().zip(row) {
                        *a += *v as f64;
                    }
                }
                if y >= r {
                    let row = &tmp[(y - r) * w..(y - r + 1) * w];
                    for (a, v) in acc.iter_mut().zip(row) {
                        *a -= *v as f64;
                    }
                }
            }
        });
    out
}

/// Self-guided filter (per channel) — an edge-preserving smooth. Flat windows
/// (variance < eps) collapse to their local mean so an off-hue speck inherits its
/// region; high-variance windows (real edges) keep the original, so there are no
/// halos. All work is O(n) box blurs.
pub(crate) fn guided_lowpass(
    halo: &[[f32; 3]],
    w: usize,
    h: usize,
    r: usize,
    eps: f32,
) -> Vec<[f32; 3]> {
    let n = w * h;
    let mut out = vec![[0.0f32; 3]; n];
    for c in 0..3 {
        let i: Vec<f32> = halo.par_iter().map(|p| p[c]).collect();
        let q = guided_lowpass_plane(&i, w, h, r, eps);
        out.par_iter_mut().zip(q.par_iter()).for_each(|(o, v)| {
            o[c] = *v;
        });
    }
    out
}

/// Single-channel self-guided filter (the scalar core of `guided_lowpass`). Flat
/// windows collapse to their local mean (so a dark region averages out its
/// texture) while real edges are preserved (no halo across a bright↔dark border).
/// Used to build the Shadows/Highlights local-adaptation base luminance.
pub(crate) fn guided_lowpass_plane(i: &[f32], w: usize, h: usize, r: usize, eps: f32) -> Vec<f32> {
    let n = w * h;
    let ii: Vec<f32> = i.par_iter().map(|v| v * v).collect();
    let mean_i = box_blur_plane(i, w, h, r);
    let mean_ii = box_blur_plane(&ii, w, h, r);
    let mut a = vec![0.0f32; n];
    let mut b = vec![0.0f32; n];
    a.par_iter_mut()
        .zip(b.par_iter_mut())
        .enumerate()
        .for_each(|(k, (ak_slot, bk_slot))| {
            let var = (mean_ii[k] - mean_i[k] * mean_i[k]).max(0.0);
            let ak = var / (var + eps);
            *ak_slot = ak;
            *bk_slot = (1.0 - ak) * mean_i[k];
        });
    let mean_a = box_blur_plane(&a, w, h, r);
    let mean_b = box_blur_plane(&b, w, h, r);
    (0..n)
        .into_par_iter()
        .map(|k| mean_a[k] * i[k] + mean_b[k])
        .collect()
}

/// Guided filter of an arbitrary scalar adjustment plane `p` using luminance
/// `guide` as the edge signal. Unlike `guided_lowpass_plane` this smooths the
/// edit amount itself: hue noise inside one lighting region converges, while a
/// real luminance edge prevents the correction from bleeding across objects.
pub(crate) fn guided_filter_plane(
    guide: &[f32],
    p: &[f32],
    w: usize,
    h: usize,
    r: usize,
    eps: f32,
) -> Vec<f32> {
    let n = w.saturating_mul(h);
    if n == 0 || guide.len() != n || p.len() != n {
        return Vec::new();
    }
    let ii: Vec<f32> = guide.par_iter().map(|v| v * v).collect();
    let ip: Vec<f32> = guide
        .par_iter()
        .zip(p.par_iter())
        .map(|(i, p)| i * p)
        .collect();
    let mean_i = box_blur_plane(guide, w, h, r);
    let mean_p = box_blur_plane(p, w, h, r);
    let mean_ii = box_blur_plane(&ii, w, h, r);
    let mean_ip = box_blur_plane(&ip, w, h, r);
    let mut a = vec![0.0f32; n];
    let mut b = vec![0.0f32; n];
    a.par_iter_mut()
        .zip(b.par_iter_mut())
        .enumerate()
        .for_each(|(k, (ak, bk))| {
            let variance = (mean_ii[k] - mean_i[k] * mean_i[k]).max(0.0);
            let covariance = mean_ip[k] - mean_i[k] * mean_p[k];
            *ak = covariance / (variance + eps);
            *bk = mean_p[k] - *ak * mean_i[k];
        });
    let mean_a = box_blur_plane(&a, w, h, r);
    let mean_b = box_blur_plane(&b, w, h, r);
    (0..n)
        .into_par_iter()
        .map(|k| mean_a[k] * guide[k] + mean_b[k])
        .collect()
}

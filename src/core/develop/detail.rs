//! Detail and Effects stages. Detail (Sharpening, Noise Reduction, Colour
//! Noise Reduction, Defringe) runs the Camera Raw–style core in
//! `detail_core` on a gamma-encoded luma/chroma split. Effects: clarity (local
//! contrast), defog (dark-channel veil removal), and vignette.

use super::*;
use crate::core::color::luminance_f32;
use crate::core::tile::{dither16_to_u8, quantize_dither, TileMap, TILE_SIZE};
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;

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

const REC709: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// Colour NR → Luminance NR → Sharpening over one RGB plane.
///
/// The pixel is gamma-encoded (the linear scene/working path encodes with the
/// odd-extended sRGB curve; display data already is) and split into luma plus
/// chroma offsets, so every stage acts on perceptual steps the same way in the
/// shadows and the highlights. Defringe runs first on the raw chroma, then
/// Colour NR on the chroma, Luminance NR and Sharpening on the luma.
pub(crate) fn process_detail_plane_with_plan(
    rgb: &[[f32; 3]],
    w: usize,
    h: usize,
    p: &DetailPlan,
    linear_space: Option<crate::core::working_color::WorkingColorSpace>,
) -> Vec<[f32; 3]> {
    process_detail_banded(rgb, w, h, p, linear_space, DETAIL_BAND_ROWS)
}

/// Rows per band when a plane is large enough to bound the scratch memory.
const DETAIL_BAND_ROWS: usize = 768;
const DETAIL_BAND_MIN_PIXELS: usize = 4_000_000;

/// Large planes run as full-width horizontal bands with a `DETAIL_HALO` apron
/// (clipped at the image edge), which reproduces the whole-plane result
/// exactly while keeping the per-band scratch small.
fn process_detail_banded(
    rgb: &[[f32; 3]],
    w: usize,
    h: usize,
    p: &DetailPlan,
    linear_space: Option<crate::core::working_color::WorkingColorSpace>,
    band_rows: usize,
) -> Vec<[f32; 3]> {
    if w * h < DETAIL_BAND_MIN_PIXELS && band_rows == DETAIL_BAND_ROWS
        || h <= band_rows + 2 * DETAIL_HALO
    {
        return process_detail_band(rgb, w, h, p, linear_space);
    }
    let mut out = vec![[0.0f32; 3]; w * h];
    for y0 in (0..h).step_by(band_rows) {
        let y1 = (y0 + band_rows).min(h);
        let a0 = y0.saturating_sub(DETAIL_HALO);
        let a1 = (y1 + DETAIL_HALO).min(h);
        let band = process_detail_band(&rgb[a0 * w..a1 * w], w, a1 - a0, p, linear_space);
        out[y0 * w..y1 * w].copy_from_slice(&band[(y0 - a0) * w..(y1 - a0) * w]);
    }
    out
}

fn process_detail_band(
    rgb: &[[f32; 3]],
    w: usize,
    h: usize,
    p: &DetailPlan,
    linear_space: Option<crate::core::working_color::WorkingColorSpace>,
) -> Vec<[f32; 3]> {
    let linear = linear_space.is_some();
    let coeff = linear_space.map_or(REC709, |s| s.render_luminance_coefficients());
    let encoded: Vec<[f32; 3]> = if linear {
        rgb.par_iter()
            .map(|c| {
                [
                    encode_channel(c[0]),
                    encode_channel(c[1]),
                    encode_channel(c[2]),
                ]
            })
            .collect()
    } else {
        rgb.to_vec()
    };
    let mut luma: Vec<f32> = encoded
        .par_iter()
        .map(|c| coeff[0] * c[0] + coeff[1] * c[1] + coeff[2] * c[2])
        .collect();
    let mut chroma: Vec<[f32; 3]> = encoded
        .par_iter()
        .zip(luma.par_iter())
        .map(|(c, &l)| [c[0] - l, c[1] - l, c[2] - l])
        .collect();
    drop(encoded);

    if p.defringe > 0.001 {
        apply_defringe(&mut chroma, &luma, w, h, p.defringe);
    }
    if p.cnr {
        colour_nr(&mut chroma, &luma, w, h, p);
    }
    if p.lnr {
        luma_nr(&mut luma, w, h, p);
    }
    if p.sharpen {
        luma = sharpen_luma(&luma, w, h, p);
    }

    luma.par_iter()
        .zip(chroma.par_iter())
        .map(|(&l, c)| {
            if linear {
                let l = l.max(0.0);
                [
                    decode_channel(l + c[0]),
                    decode_channel(l + c[1]),
                    decode_channel(l + c[2]),
                ]
            } else {
                [
                    (l + c[0]).clamp(0.0, 1.0),
                    (l + c[1]).clamp(0.0, 1.0),
                    (l + c[2]).clamp(0.0, 1.0),
                ]
            }
        })
        .collect()
}

fn process_detail_plane(
    rgb: &[[f32; 3]],
    w: usize,
    h: usize,
    settings: &DevelopSettings,
    linear_space: Option<crate::core::working_color::WorkingColorSpace>,
    preview_scale: u32,
) -> Vec<[f32; 3]> {
    let plan = DetailPlan::new(settings, preview_scale.max(1) as f32);
    process_detail_plane_with_plan(rgb, w, h, &plan, linear_space)
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

/// `preview_scale` is the live-preview proxy downsample (source px per proxy
/// px), or `1` for the full-resolution commit/settled render; the plan
/// re-expresses radii and wavelet scales in proxy pixels.
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
    *working = process_detail_plane(
        working,
        width,
        height,
        settings,
        Some(working_space),
        preview_scale,
    );
}

/// Display-domain Detail (gamma values in [0,1]). Interactive preview feeds an
/// anti-aliased viewport proxy with its downsample as `preview_scale`; pass
/// `1` to run at native resolution.
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
    *display = process_detail_plane(display, width, height, settings, None, preview_scale);
}

/// Gather a `DETAIL_HALO`-apron'd f32 RGB plane around one tile (apron clipped
/// at the image edge, 16-bit reads — bit-identical for 8-bit tiles). Returns
/// the plane, its size and the tile's offset inside it.
fn gather_detail_plane(
    source: &TileMap,
    base_x: u32,
    base_y: u32,
    valid_w: u32,
    valid_h: u32,
) -> (Vec<[f32; 3]>, usize, usize, usize, usize) {
    // The apron stops at the image edge instead of replicating it, so every
    // wavelet level clamps at the real border exactly as a whole-image pass.
    let r = DETAIL_HALO as u32;
    let x0 = base_x.saturating_sub(r);
    let y0 = base_y.saturating_sub(r);
    let x1 = (base_x + valid_w + r).min(source.width);
    let y1 = (base_y + valid_h + r).min(source.height);
    let hw = (x1 - x0) as usize;
    let hh = (y1 - y0) as usize;
    let mut out = vec![[0.0f32; 3]; hw * hh];
    for hy in 0..hh {
        let gy = y0 + hy as u32;
        let ty_tile = (gy / TILE_SIZE) as i32;
        let ly = gy % TILE_SIZE;
        let mut cached_tx = i32::MIN;
        let mut cur_tile: Option<&Arc<crate::core::tile::Tile>> = None;
        for hx in 0..hw {
            let gx = x0 + hx as u32;
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
    (out, hw, hh, (base_x - x0) as usize, (base_y - y0) as usize)
}

/// Detail stage (Sharpening / Noise Reduction) as a separate full-resolution
/// pass over the already-toned tilemap. Per tile with a `DETAIL_HALO` apron so
/// the blurs are seam-free across tiles; writes the 16-bit master too when
/// present, so a 16-bit document keeps its precision through Detail.
pub(crate) fn apply_detail_to_tilemap(source: &TileMap, settings: &DevelopSettings) -> TileMap {
    if source.width == 0 || source.height == 0 {
        return source.clone();
    }

    let p = DetailPlan::new(settings, 1.0);
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

            let (plane, hw, hh, off_x, off_y) =
                gather_detail_plane(source, base_x, base_y, valid_w, valid_h);
            let out = process_detail_plane_with_plan(&plane, hw, hh, &p, None);

            for ty in 0..valid_h as usize {
                for tx in 0..valid_w as usize {
                    let i = (ty * TILE_SIZE as usize + tx) * 4;
                    if tile.pixels[i + 3] == 0 {
                        continue;
                    }
                    let v = out[(ty + off_y) * hw + (tx + off_x)];
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
mod camera_raw_contract {
    //! Behaviour locked against Camera Raw 16 measurements of a synthetic
    //! chart (see `tests/detail_pts_probe.rs`): each assertion names the
    //! Camera Raw figure it tracks and the old engine's figure it rejects.
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
        luminance_f32(px[0], px[1], px[2])
    }
    fn chroma_vec(px: [f32; 3]) -> [f32; 3] {
        let y = luma(px);
        [px[0] - y, px[1] - y, px[2] - y]
    }
    fn chroma_mag(px: [f32; 3]) -> f32 {
        let c = chroma_vec(px);
        (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt()
    }
    fn grey(v: f32) -> [f32; 3] {
        [v, v, v]
    }
    fn run(img: &mut Vec<[f32; 3]>, w: usize, h: usize, s: &DevelopSettings) {
        apply_detail_to_display_buffer(img, w, h, s, 1);
    }

    fn erf(x: f32) -> f32 {
        // Abramowitz–Stegun 7.1.26 — plenty for a test ramp.
        let t = 1.0 / (1.0 + 0.327_591_1 * x.abs());
        let poly = ((((1.061_405_4 * t - 1.453_152_1) * t + 1.421_413_7) * t - 0.284_496_74) * t
            + 0.254_829_6)
            * t;
        (1.0 - poly * (-x * x).exp()).copysign(x)
    }

    /// Soft (Gaussian σ = 1) vertical step, like a lens-blurred edge.
    fn soft_edge(w: usize, h: usize, lo: f32, hi: f32) -> Vec<[f32; 3]> {
        (0..w * h)
            .map(|i| {
                let x = (i % w) as f32 + 0.5 - w as f32 / 2.0;
                let t = 0.5 * (1.0 + erf(x / std::f32::consts::SQRT_2));
                grey(lo + (hi - lo) * t)
            })
            .collect()
    }

    /// 10–90 % rise distance and the largest excursion past the plateaus.
    fn edge_stats(img: &[[f32; 3]], w: usize, h: usize, lo: f32, hi: f32) -> (f32, f32) {
        let row: Vec<f32> = (0..w).map(|x| img[(h / 2) * w + x][0]).collect();
        let cross = |t: f32| -> f32 {
            for x in 0..w - 1 {
                if (row[x] - t) * (row[x + 1] - t) <= 0.0 && row[x] != row[x + 1] {
                    return x as f32 + (t - row[x]) / (row[x + 1] - row[x]);
                }
            }
            f32::NAN
        };
        let rise = cross(lo + 0.9 * (hi - lo)) - cross(lo + 0.1 * (hi - lo));
        let over = row.iter().fold(0.0f32, |m, &v| m.max(v - hi).max(lo - v));
        (rise, over)
    }

    fn grating(w: usize, h: usize, base: f32, period: f32, amp: f32) -> Vec<[f32; 3]> {
        (0..w * h)
            .map(|i| grey(base + amp * (std::f32::consts::TAU * (i % w) as f32 / period).sin()))
            .collect()
    }

    /// Interior standard deviation of luma (8 px margin).
    fn inner_std(img: &[[f32; 3]], w: usize, h: usize) -> f32 {
        let (mut s, mut s2, mut n) = (0.0f64, 0.0f64, 0.0f64);
        for y in 8..h - 8 {
            for x in 8..w - 8 {
                let v = luma(img[y * w + x]) as f64;
                s += v;
                s2 += v * v;
                n += 1.0;
            }
        }
        ((s2 / n - (s / n).powi(2)).max(0.0)).sqrt() as f32
    }

    fn chroma_std(img: &[[f32; 3]], w: usize, h: usize) -> f32 {
        let (mut s2, mut n) = (0.0f64, 0.0f64);
        for y in 8..h - 8 {
            for x in 8..w - 8 {
                let c = chroma_vec(img[y * w + x]);
                s2 += (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]) as f64;
                n += 1.0;
            }
        }
        (s2 / n).sqrt() as f32
    }

    #[test]
    fn sharpening_crisps_edges_like_camera_raw() {
        let (w, h) = (64usize, 8usize);
        let (lo, hi) = (0.35f32, 0.65f32);
        let (rise0, _) = edge_stats(&soft_edge(w, h, lo, hi), w, h, lo, hi);
        // Camera Raw: rise 2.66 -> 2.03 (40) / 1.55 (70), overshoot 0.007 /
        // 0.025; the old engine only reached 2.56 / 2.49.
        for (amount, max_rise, over_lo, over_hi) in
            [(40.0, 2.35, 0.002, 0.02), (70.0, 1.85, 0.015, 0.04)]
        {
            let mut img = soft_edge(w, h, lo, hi);
            let s = DevelopSettings {
                sharpening: amount,
                ..Default::default()
            };
            run(&mut img, w, h, &s);
            let (rise, over) = edge_stats(&img, w, h, lo, hi);
            println!("sharpen {amount}: rise {rise0:.2} -> {rise:.2}, overshoot {over:.4}");
            assert!(rise < max_rise, "edge not crisp enough at {amount}: {rise}");
            assert!(
                over > over_lo && over < over_hi,
                "overshoot {over} outside Camera Raw's band at {amount}"
            );
            assert!(
                img.iter().all(|p| chroma_mag(*p) < 1e-4),
                "tinted a neutral edge"
            );
        }
    }

    #[test]
    fn low_sharpen_detail_suppresses_halos() {
        let (w, h) = (64usize, 8usize);
        let (lo, hi) = (0.35f32, 0.65f32);
        let stats = |detail: f32| {
            let mut img = soft_edge(w, h, lo, hi);
            let s = DevelopSettings {
                sharpening: 70.0,
                sharpen_detail: detail,
                ..Default::default()
            };
            run(&mut img, w, h, &s);
            edge_stats(&img, w, h, lo, hi)
        };
        let (rise0, over0) = stats(0.0);
        let (_, over25) = stats(25.0);
        let (_, over100) = stats(100.0);
        println!("halo: detail 0 {over0:.4} (rise {rise0:.2}), 25 {over25:.4}, 100 {over100:.4}");
        // Camera Raw: 0.004 / 0.025 / 0.046, and Detail 0 still steepens (1.97).
        assert!(over0 < over25 * 0.5);
        assert!(over100 > over25);
        assert!(
            rise0 < 2.3,
            "Detail 0 should still steepen the edge: {rise0}"
        );
    }

    #[test]
    fn sharpening_fades_in_deep_shadows() {
        let (w, h) = (96usize, 24usize);
        let gain = |base: f32| {
            let src = grating(w, h, base, 3.0, 0.02);
            let mut img = src.clone();
            let s = DevelopSettings {
                sharpening: 70.0,
                ..Default::default()
            };
            run(&mut img, w, h, &s);
            inner_std(&img, w, h) / inner_std(&src, w, h)
        };
        let (deep, shadow, mid) = (gain(0.06), gain(0.15), gain(0.50));
        println!("sharpen gain: 0.06 {deep:.2}, 0.15 {shadow:.2}, 0.50 {mid:.2}");
        // Camera Raw: 1.17 / 1.75 / 2.75.
        assert!(mid > 2.4 && mid < 3.1, "mid-tone texture gain {mid}");
        assert!(shadow < mid - 0.6 && deep < shadow);
    }

    #[test]
    fn sharpen_masking_spares_flat_noise_but_keeps_edges() {
        let (w, h) = (48usize, 48usize);
        let flat = |masking: f32| {
            let mut img: Vec<[f32; 3]> = (0..w * h)
                .map(|i| grey((0.45 + 0.02 * hash_noise(i, 1)).clamp(0.0, 1.0)))
                .collect();
            let s = DevelopSettings {
                sharpening: 90.0,
                sharpen_masking: masking,
                ..Default::default()
            };
            run(&mut img, w, h, &s);
            inner_std(&img, w, h)
        };
        let (open, masked) = (flat(0.0), flat(60.0));
        println!("masking: flat noise std {open:.4} -> {masked:.4}");
        assert!(masked < open * 0.7);

        let (ew, eh) = (64usize, 8usize);
        let over = |masking: f32| {
            let mut img = soft_edge(ew, eh, 0.35, 0.65);
            let s = DevelopSettings {
                sharpening: 70.0,
                sharpen_masking: masking,
                ..Default::default()
            };
            run(&mut img, ew, eh, &s);
            edge_stats(&img, ew, eh, 0.35, 0.65).1
        };
        // Camera Raw keeps 0.021 of the 0.025 overshoot at Masking 50.
        assert!(
            over(50.0) > over(0.0) * 0.6,
            "masking must not switch off real edges"
        );
    }

    #[test]
    fn noise_reduction_is_gradual_even_across_tones_and_keeps_edges() {
        let (w, h) = (96usize, 24usize);
        let keep = |amount: f32, base: f32| {
            let src = grating(w, h, base, 3.0, 0.02);
            let mut img = src.clone();
            let s = DevelopSettings {
                noise_reduction: amount,
                ..Default::default()
            };
            run(&mut img, w, h, &s);
            inner_std(&img, w, h) / inner_std(&src, w, h)
        };
        // Camera Raw keeps 0.80 / 0.50 / 0.335 of fine texture at 10 / 25 / 50,
        // equally in the shadows; the old engine kept 0.19 at 25 and ~0 in the
        // shadows.
        for (amount, target) in [(10.0, 0.80), (25.0, 0.50), (50.0, 0.335)] {
            let (mid, dark) = (keep(amount, 0.45), keep(amount, 0.12));
            println!("NR {amount}: fine texture kept mid {mid:.3} shadow {dark:.3}");
            assert!((mid - target).abs() < 0.06, "NR {amount} mid keep {mid}");
            assert!(
                (dark - mid).abs() < 0.06,
                "NR {amount} shadows differ: {dark}"
            );
        }

        let (ew, eh) = (64usize, 8usize);
        let mut img = soft_edge(ew, eh, 0.35, 0.65);
        let s = DevelopSettings {
            noise_reduction: 100.0,
            ..Default::default()
        };
        run(&mut img, ew, eh, &s);
        let (rise, _) = edge_stats(&img, ew, eh, 0.35, 0.65);
        // Camera Raw 2.79 at 100; the old engine smeared it to 6.5.
        assert!(rise < 3.2, "NR blurred a real edge: rise {rise}");
    }

    #[test]
    fn colour_nr_removes_speckle_early_and_widens_to_blotches() {
        let (w, h) = (96usize, 96usize);
        let offsets: Vec<[f32; 3]> = (0..w * h)
            .map(|i| {
                let (a, b) = (0.03 * hash_noise(i, 7), 0.03 * hash_noise(i, 13));
                [a, -0.3 * (a + b), b]
            })
            .collect();
        let field = |offs: &[[f32; 3]]| -> Vec<[f32; 3]> {
            offs.iter()
                .map(|o| [0.45 + o[0], 0.45 + o[1], 0.45 + o[2]])
                .collect()
        };
        let speckle = field(&offsets);
        let blotch = {
            let planes: Vec<Vec<f32>> = (0..3)
                .map(|c| {
                    let p: Vec<f32> = offsets.iter().map(|v| v[c]).collect();
                    box_blur_plane(&box_blur_plane(&p, w, h, 4), w, h, 4)
                })
                .collect();
            let offs: Vec<[f32; 3]> = (0..w * h)
                .map(|i| [planes[0][i], planes[1][i], planes[2][i]])
                .collect();
            let k = chroma_std(&speckle, w, h) / chroma_std(&field(&offs), w, h);
            let scaled: Vec<[f32; 3]> = offs.iter().map(|o| o.map(|v| v * k)).collect();
            field(&scaled)
        };
        let kept = |src: &Vec<[f32; 3]>, amount: f32| {
            let mut img = src.clone();
            let s = DevelopSettings {
                color_noise_reduction: amount,
                ..Default::default()
            };
            run(&mut img, w, h, &s);
            chroma_std(&img, w, h) / chroma_std(src, w, h)
        };
        let (s10, b10, b50) = (
            kept(&speckle, 10.0),
            kept(&blotch, 10.0),
            kept(&blotch, 50.0),
        );
        println!("colour NR: speckle@10 {s10:.2}, blotch@10 {b10:.2}, blotch@50 {b50:.2}");
        // Camera Raw: 0.21 / 0.68 / 0.21; the old engine 0.90 / 0.97 / 0.87.
        assert!(s10 < 0.35, "fine speckle should go at low amounts: {s10}");
        assert!(b10 > b50 + 0.2, "amount should widen reach into blotches");
        assert!(b50 < 0.4, "blotches should mostly go at 50: {b50}");
    }

    #[test]
    fn colour_nr_keeps_real_colour_detail() {
        // Thin iso-luminant red lines every 16 px on grey: Camera Raw keeps 97 %
        // of their colour at 25; the old engine 83 %.
        let (w, h) = (96usize, 32usize);
        let red = {
            let r = [0.70f32, 0.36, 0.36];
            let d = luma(r) - 0.45;
            [r[0] - d, r[1] - d, r[2] - d]
        };
        let src: Vec<[f32; 3]> = (0..w * h)
            .map(|i| if (i % w) % 16 < 2 { red } else { grey(0.45) })
            .collect();
        let line_chroma = |img: &[[f32; 3]]| -> f32 {
            (0..w)
                .step_by(16)
                .map(|x| chroma_mag(img[(h / 2) * w + x]))
                .sum::<f32>()
        };
        let mut img = src.clone();
        let s = DevelopSettings {
            color_noise_reduction: 25.0,
            ..Default::default()
        };
        run(&mut img, w, h, &s);
        let kept = line_chroma(&img) / line_chroma(&src);
        println!("colour NR keeps {kept:.2} of thin red lines");
        assert!(
            kept > 0.85,
            "colour NR washed out real colour detail: {kept}"
        );
    }

    #[test]
    fn colour_nr_real_edge_does_not_bleed() {
        let (w, h) = (48usize, 16usize);
        let (left, right) = ([0.55f32, 0.30, 0.55], [0.34f32, 0.44, 0.34]);
        let mid = w / 2;
        let mut img: Vec<[f32; 3]> = (0..w * h)
            .map(|i| if (i % w) < mid { left } else { right })
            .collect();
        let step = |img: &[[f32; 3]]| -> f32 {
            let a = chroma_vec(img[(h / 2) * w + mid - 3]);
            let b = chroma_vec(img[(h / 2) * w + mid + 3]);
            ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
        };
        let step0 = step(&img);
        let s = DevelopSettings {
            color_noise_reduction: 100.0,
            ..Default::default()
        };
        run(&mut img, w, h, &s);
        let kept = step(&img) / step0;
        println!(
            "colour NR keeps {:.0}% of a real colour edge at ±3 px",
            kept * 100.0
        );
        assert!(kept > 0.9, "colour NR bled a real colour edge: {kept}");
    }

    #[test]
    fn detail_sliders_keep_a_neutral_grey_neutral_and_finite() {
        let (w, h) = (32usize, 32usize);
        let sliders: &[(&str, fn(&mut DevelopSettings))] = &[
            ("sharpening", |s| s.sharpening = 150.0),
            ("sharpen_detail", |s| {
                s.sharpening = 150.0;
                s.sharpen_detail = 100.0;
            }),
            ("noise_reduction", |s| s.noise_reduction = 100.0),
            ("color_noise_reduction", |s| s.color_noise_reduction = 100.0),
        ];
        for &(name, set) in sliders {
            let mut img: Vec<[f32; 3]> = (0..w * h)
                .map(|i| grey((0.45 + 0.03 * hash_noise(i, 3)).clamp(0.0, 1.0)))
                .collect();
            let mut s = DevelopSettings::default();
            set(&mut s);
            run(&mut img, w, h, &s);
            assert!(
                img.iter()
                    .all(|p| p.iter().all(|c| c.is_finite() && (0.0..=1.0).contains(c))),
                "{name} produced out-of-range output"
            );
            let max_chroma = img.iter().map(|&p| chroma_mag(p)).fold(0.0, f32::max);
            assert!(max_chroma < 1e-4, "{name} tinted a neutral: {max_chroma}");
        }
    }

    #[test]
    fn linear_working_path_matches_the_display_path() {
        // The scene path encodes linear light before the split, so Detail on a
        // linear buffer equals Detail on the same pixels in display gamma.
        let (w, h) = (48usize, 24usize);
        let display: Vec<[f32; 3]> = (0..w * h)
            .map(|i| {
                let base = if i % w > w / 2 { 0.62 } else { 0.18 };
                let v = base + 0.03 * hash_noise(i, 5);
                [v + 0.02 * hash_noise(i, 9), v, v - 0.02 * hash_noise(i, 11)]
            })
            .collect();
        let s = DevelopSettings {
            sharpening: 60.0,
            noise_reduction: 30.0,
            color_noise_reduction: 40.0,
            ..Default::default()
        };
        let mut a = display.clone();
        run(&mut a, w, h, &s);
        let mut b: Vec<[f32; 3]> = display.iter().map(|p| p.map(decode_channel)).collect();
        apply_detail_to_working_buffer_in_space(
            &mut b,
            w,
            h,
            &s,
            crate::core::working_color::WorkingColorSpace::LinearSrgb,
            1,
        );
        let d = a
            .iter()
            .zip(&b)
            .flat_map(|(p, q)| {
                (0..3).map(move |c| (p[c] - encode_channel(q[c]).clamp(0.0, 1.0)).abs())
            })
            .fold(0.0f32, f32::max);
        assert!(d < 2e-3, "linear and display Detail disagree: {d}");
    }

    #[test]
    fn banded_processing_matches_the_whole_plane_exactly() {
        let (w, h) = (40usize, 400usize);
        let img: Vec<[f32; 3]> = (0..w * h)
            .map(|i| {
                let v = if (i / w) % 97 < 40 { 0.25 } else { 0.7 } + 0.03 * hash_noise(i, 4);
                [v + 0.02 * hash_noise(i, 8), v, v - 0.02 * hash_noise(i, 12)]
            })
            .collect();
        let s = DevelopSettings {
            sharpening: 90.0,
            sharpen_masking: 20.0,
            sharpen_detail: 10.0,
            noise_reduction: 40.0,
            color_noise_reduction: 50.0,
            ..Default::default()
        };
        let p = DetailPlan::new(&s, 1.0);
        let whole = process_detail_band(&img, w, h, &p, None);
        let banded = process_detail_banded(&img, w, h, &p, None, 50);
        assert!(
            whole == banded,
            "banded Detail differs from the whole plane"
        );
    }

    #[test]
    fn preview_scale_folds_sharpening_toward_the_settled_look() {
        let (w, h) = (64usize, 8usize);
        let a0 = edge_stats(&soft_edge(w, h, 0.30, 0.60), w, h, 0.30, 0.60).1;
        let s = DevelopSettings {
            sharpening: 80.0,
            ..Default::default()
        };
        let added = |scale: u32| {
            let mut img = soft_edge(w, h, 0.30, 0.60);
            apply_detail_to_display_buffer(&mut img, w, h, &s, scale);
            assert!(img.iter().all(|p| p.iter().all(|c| c.is_finite())));
            edge_stats(&img, w, h, 0.30, 0.60).1 - a0
        };
        let (full, p2, p8) = (added(1), added(2), added(8));
        println!("preview sharpen overshoot: full {full:.4}, 2x {p2:.4}, 8x {p8:.4}");
        assert!(full > 1e-3);
        assert!(p2 <= full + 1e-4 && p8 <= p2 + 1e-4 && p8 < full);
    }
}

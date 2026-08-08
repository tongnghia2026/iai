//! Scene-referred Develop core (the linear-light rewrite).
//!
//! A RAW decode now stops at *linear scene-referred RGB* (sRGB primaries,
//! UNCLAMPED — values above 1.0 keep the sensor's highlight headroom, values
//! below 0.0 keep out-of-gamut camera colours) stored as an f16 [`SceneSource`].
//! The Light half of Develop is rebuilt on top of it as a physical chain:
//!
//! ```text
//! scene RGB → CAT16 white balance (one 3×3 matrix)
//!           → exposure ×2^EV (pure multiply — no easing, no rolloff hack)
//!           → tone equalizer: rgb ×2^offset(E_region)   (H/S/W/B as EV zones)
//!           → sigmoid tone map (per-channel ⊕ max-RGB ratio, hue-preserve blend)
//!           → gamut soft-clip → sRGB encode → display curves (curve_* / points)
//! ```
//!
//! Everything monotone collapses into 256-entry LUTs indexed in **log2 (EV)**
//! space over [`SCENE_EV_MIN`, `SCENE_EV_MAX`] — the same LUT-plumbing budget the
//! old gamma-domain engine used, so the GPU preview mirrors the chain exactly.
//! The Colour/Effects/Detail/Locals stages are unchanged: they run on the
//! display-referred output of this chain (see [`apply_scene_to_tilemap`]), which
//! is what they always consumed.
//!
//! Non-RAW sessions run the SAME chain through [`BaseLook::Identity`]: the
//! layer is linearized into a `SceneSource` on session open, the tone map is
//! the identity (so neutral stays an exact no-op on already-rendered images)
//! and Contrast moves into the display curve. The legacy display-domain engine
//! (`develop.rs`) remains only for oversized layers that cannot live in a GPU
//! texture.

use crate::core::cat16;
use crate::core::color::luminance_f32;
use crate::core::develop::{
    apply_color, apply_luma_target, apply_point_curve_outer, bell, contrast_curve, control_to_unit,
    curve_is_identity, curve_shadows_mask, darks_mask, eased_control, guided_lowpass_plane,
    linear_to_srgb, luma_lin, lut_lerp, rgb_curve_luts, sample_plane_bilinear, smootherstep,
    srgb_to_linear, DevelopSettings, EXPOSURE_LIMIT, TONE_DOWNSAMPLE, TONE_REGION_RADIUS,
};
use crate::core::tile::TileMap;
use rayon::prelude::*;

// ── LUT domain ───────────────────────────────────────────────────────────────

/// Scene-exposure axis of every scene LUT, in EV (log2 of linear value).
/// −14 EV is far below sensor noise; +6 EV (linear 64) is far above any real
/// highlight after decode normalisation, so the axis brackets the data.
pub const SCENE_EV_MIN: f32 = -14.0;
pub const SCENE_EV_MAX: f32 = 6.0;
/// Middle grey anchor: scene 0.1845 maps to display-linear 0.1845 (the
/// filmic convention — 18% grey stays 18% grey at neutral).
pub const SCENE_MID_GRAY: f32 = 0.1845;

/// Sigmoid steepness at neutral Contrast, and how far one full slider unit
/// scales it (C = BASE · 2^(RANGE·u)). Taste knobs for the default look.
const SIGMOID_BASE_C: f32 = 1.7;
const SIGMOID_C_RANGE: f32 = 0.7;
/// Blend from the per-channel sigmoid toward the max-RGB ratio path. Preserve
/// hue strongly through shadows/midtones, then let clipped highlights converge
/// naturally toward white. These taste knobs are mirrored in compositor.wgsl.
const HUE_PRESERVE_LOW: f32 = 0.90;
const HUE_PRESERVE_HIGH: f32 = 0.20;

#[inline]
fn hue_preserve_blend(mapped_n: f32) -> f32 {
    let hw = smootherstep(0.5, 1.0, mapped_n);
    HUE_PRESERVE_LOW + (HUE_PRESERVE_HIGH - HUE_PRESERVE_LOW) * hw
}

// ── Tone-equalizer zones (H/S/W/B as EV offsets) ────────────────────────────
// Each Light slider owns a gaussian zone on the EV axis relative to middle
// grey (Er = E − log2(SCENE_MID_GRAY)). A full slider (±200) moves its zone by
// ±gain EV; the total offset is clamped so extreme combinations stay sane.
// Multiplying a *regional* 2^offset in linear light preserves the pixel's
// log-domain texture by construction — that is the tone-equalizer trick, and
// why the old `local_detail_boost` compensation is gone.
// Blacks used to sit at −5.5 EV (≈0.004 linear) — so deep that a normal image
// has almost no pixels there, so the slider felt inert; Shadows at gain 2.0 was
// the opposite, too strong. Blacks now grips higher (−4.6 EV) with a wider zone
// and a bigger gain so it actually moves the darkest tones, and Shadows is
// eased so the two feel balanced (user taste, 2026-07-08).
const TONE_EQ_SHADOWS: (f32, f32, f32) = (-3.0, 1.6, 1.4); // (centre EV, width, gain)
const TONE_EQ_BLACKS: (f32, f32, f32) = (-4.6, 1.5, 2.6);
const TONE_EQ_HIGHLIGHTS: (f32, f32, f32) = (2.5, 1.6, 2.0);
const TONE_EQ_WHITES: (f32, f32, f32) = (4.5, 1.3, 1.5);
const TONE_EQ_CLAMP_EV: f32 = 4.0;
/// Guided-filter regularisation for the regional-E plane, in EV² (variance).
/// Windows varying less than ~0.5 EV read as one lighting region and smooth
/// out; larger swings are real light↔dark edges and are preserved (no halo).
const SCENE_TONE_GUIDED_EPS: f32 = 0.25;

// ── f16 (IEEE 754 half) helpers — no external dependency ────────────────────

/// f32 → half bits, round-to-nearest-even. Overflow → ±Inf, NaN preserved.
#[inline]
pub fn f32_to_f16_bits(v: f32) -> u16 {
    let bits = v.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let man = bits & 0x007f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if man != 0 { 0x0200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = man | 0x0080_0000;
        let shift = (14 - e) as u32;
        let half = m >> shift;
        let round = 1u32 << (shift - 1);
        let sticky = m & (round - 1);
        if (m & round) != 0 && (sticky != 0 || (half & 1) != 0) {
            return sign | (half + 1) as u16;
        }
        return sign | half as u16;
    }
    let half = ((e as u32) << 10) | (man >> 13);
    let round = man & 0x1fff;
    if round > 0x1000 || (round == 0x1000 && (half & 1) != 0) {
        // Carry may roll into the exponent — that is the correct round-to-Inf.
        return sign | (half + 1) as u16;
    }
    sign | half as u16
}

/// Half bits → f32 (exact).
#[inline]
pub fn f16_bits_to_f32(h: u16) -> f32 {
    let sign = ((h & 0x8000) as u32) << 16;
    let exp = ((h >> 10) & 0x1f) as u32;
    let man = (h & 0x03ff) as u32;
    let bits = if exp == 0 {
        if man == 0 {
            sign
        } else {
            let mut e = 127 - 15 + 1;
            let mut m = man;
            while m & 0x0400 == 0 {
                m <<= 1;
                e -= 1;
            }
            sign | ((e as u32) << 23) | ((m & 0x03ff) << 13)
        }
    } else if exp == 0x1f {
        sign | 0x7f80_0000 | (man << 13)
    } else {
        sign | ((exp + 127 - 15) << 23) | (man << 13)
    };
    f32::from_bits(bits)
}

// ── SceneSource ──────────────────────────────────────────────────────────────

/// The linear scene-referred master of a Develop session. RGBA half floats,
/// row-major — kept RGBA so the buffer uploads directly as an `Rgba16Float`
/// texture for the GPU preview. RAW masters are opaque. Identity masters keep
/// an exact UNORM16 alpha plane when the source layer has transparency; f16
/// cannot represent every 16-bit alpha value exactly, and a Develop commit
/// must never change layer transparency.
/// What the scene chain renders at NEUTRAL settings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BaseLook {
    /// RAW master: neutral renders the anchored sigmoid default look.
    Raw,
    /// Display-referred source (a JPEG/PNG/TIFF layer linearized into the
    /// scene): the tone map is the identity on [0,1] (hard white above), so
    /// neutral reproduces the source exactly, and Contrast lives in the
    /// display curve instead of the sigmoid steepness.
    Identity,
}

pub struct SceneSource {
    pub width: u32,
    pub height: u32,
    pub half: Vec<u16>,
    /// Exact source alpha for Identity scenes that contain transparency.
    /// `None` means fully opaque (the normal RAW and opaque-image case).
    pub alpha: Option<Vec<u16>>,
    pub look: BaseLook,
}

#[allow(dead_code)] // constructors exercised by unit tests; production fills `half` directly
impl SceneSource {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            half: vec![0u16; width as usize * height as usize * 4],
            alpha: None,
            look: BaseLook::Raw,
        }
    }

    /// Linearize a display-referred layer (gamma sRGB, 8- or 16-bit tiles)
    /// into an Identity-look scene, so non-RAW Develop sessions run the same
    /// linear-light chain as RAW — CAT16 white balance, ×2^EV exposure and the
    /// EV-domain tone equalizer — instead of the legacy gamma-domain engine
    /// (whose additive luma masks are why Light drags on a JPEG cut harsh,
    /// non-smooth transitions where ACR stays soft).
    ///
    /// The 8-bit path is DITHERED: a JPEG has only 256 levels per channel, so a
    /// hard shadow/exposure lift spreads the gaps between neighbouring levels
    /// into visible banding that the 8-bit OUTPUT dither cannot fill (it only
    /// spreads ±½ level, not a 5–7 level hole). Adding a sub-LSB ordered dither
    /// to the source value BEFORE linearizing turns the 256 discrete levels
    /// into a continuous distribution, so a stretched gradient stays smooth —
    /// the same reason RAW (smooth 16-bit) develops cleanly and a raw JPEG did
    /// not. At neutral settings the dither is imperceptible and rounds back to
    /// the source (PTS semantics hold to within sub-LSB noise). 16-bit masters
    /// have ample levels and are converted directly.
    pub fn from_display_tiles(tiles: &TileMap) -> SceneSource {
        let w = tiles.width;
        let h = tiles.height;
        let mut half = vec![0u16; w as usize * h as usize * 4];
        let mut alpha = vec![u16::MAX; w as usize * h as usize];
        // Fine linear table (sub-8-bit resolution) so a dithered 8-bit value
        // maps to a continuous linear value: index = (value + dither)·SCALE.
        const FINE: usize = 8192;
        let mut fine = vec![0u16; FINE + 1];
        for (i, slot) in fine.iter_mut().enumerate() {
            *slot = f32_to_f16_bits(crate::core::develop::srgb_to_linear(i as f32 / FINE as f32));
        }
        // Tile-span walk (one hash resolve per 256-px run, not per pixel — a
        // 24MP layer linearizes in tens of ms instead of seconds).
        use crate::core::tile::{bayer8, TilePos, TILE_SIZE};
        let scale = FINE as f32 / 255.0;
        half.par_chunks_mut(w as usize * 4)
            .zip(alpha.par_chunks_mut(w as usize))
            .enumerate()
            .for_each(|(y, (row, alpha_row))| {
                let ly = (y as u32 % TILE_SIZE) as usize;
                let yy = y as u32;
                let mut x0 = 0u32;
                while x0 < w {
                    let x1 = ((x0 / TILE_SIZE + 1) * TILE_SIZE).min(w);
                    let span = &mut row[x0 as usize * 4..x1 as usize * 4];
                    let alpha_span = &mut alpha_row[x0 as usize..x1 as usize];
                    match tiles.tiles.get(&TilePos::from_pixel(x0, yy)) {
                        Some(tile) => {
                            let base = ly * TILE_SIZE as usize * 4;
                            let n = (x1 - x0) as usize * 4;
                            if let Some(p16) = &tile.pixels16 {
                                let src = &p16[base..base + n];
                                for ((dst, alpha), s) in span
                                    .chunks_exact_mut(4)
                                    .zip(alpha_span.iter_mut())
                                    .zip(src.chunks_exact(4))
                                {
                                    dst[0] = f32_to_f16_bits(srgb_to_linear(s[0] as f32 / 65535.0));
                                    dst[1] = f32_to_f16_bits(srgb_to_linear(s[1] as f32 / 65535.0));
                                    dst[2] = f32_to_f16_bits(srgb_to_linear(s[2] as f32 / 65535.0));
                                    dst[3] = f32_to_f16_bits(s[3] as f32 / 65535.0);
                                    *alpha = s[3];
                                }
                            } else {
                                let src = &tile.pixels[base..base + n];
                                for (i, ((dst, alpha), s)) in span
                                    .chunks_exact_mut(4)
                                    .zip(alpha_span.iter_mut())
                                    .zip(src.chunks_exact(4))
                                    .enumerate()
                                {
                                    let xx = x0 + i as u32;
                                    // Per-channel dither offset (staggered so R/G/B
                                    // don't share one pattern → chroma stays neutral).
                                    let d0 = bayer8(xx, yy) - 0.5;
                                    let d1 = bayer8(xx + 3, yy + 5) - 0.5;
                                    let d2 = bayer8(xx + 5, yy + 3) - 0.5;
                                    let idx = |v: u8, d: f32| -> usize {
                                        (((v as f32 + d) * scale).round() as i32)
                                            .clamp(0, FINE as i32)
                                            as usize
                                    };
                                    dst[0] = fine[idx(s[0], d0)];
                                    dst[1] = fine[idx(s[1], d1)];
                                    dst[2] = fine[idx(s[2], d2)];
                                    dst[3] = f32_to_f16_bits(s[3] as f32 / 255.0);
                                    *alpha = s[3] as u16 * 257;
                                }
                            }
                        }
                        None => {
                            for dst in span.chunks_exact_mut(4) {
                                dst[0] = 0;
                                dst[1] = 0;
                                dst[2] = 0;
                                dst[3] = 0;
                            }
                            alpha_span.fill(0);
                        }
                    }
                    x0 = x1;
                }
            });
        let alpha = alpha
            .iter()
            .any(|&value| value != u16::MAX)
            .then_some(alpha);
        SceneSource {
            width: w,
            height: h,
            half,
            alpha,
            look: BaseLook::Identity,
        }
    }

    #[inline]
    pub fn set_rgb(&mut self, x: u32, y: u32, rgb: [f32; 3]) {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        self.half[i] = f32_to_f16_bits(rgb[0]);
        self.half[i + 1] = f32_to_f16_bits(rgb[1]);
        self.half[i + 2] = f32_to_f16_bits(rgb[2]);
        self.half[i + 3] = 0x3c00; // 1.0
    }

    #[inline]
    pub fn get_rgb(&self, x: u32, y: u32) -> [f32; 3] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [
            f16_bits_to_f32(self.half[i]),
            f16_bits_to_f32(self.half[i + 1]),
            f16_bits_to_f32(self.half[i + 2]),
        ]
    }

    #[inline]
    fn alpha16_at_index(&self, pixel_index: usize) -> u16 {
        self.alpha
            .as_ref()
            .map_or(u16::MAX, |alpha| alpha[pixel_index])
    }

    #[inline]
    fn alpha_at_index(&self, pixel_index: usize) -> f32 {
        self.alpha16_at_index(pixel_index) as f32 / 65535.0
    }
}

// ── Sigmoid tone map ─────────────────────────────────────────────────────────

/// Solved sigmoid parameters: `S(v) = n / (1 + (a/v)^c)`, with `S(mid)=mid`
/// (grey anchor) and `S(2^SCENE_EV_MAX)=1` (axis top maps to display white).
#[derive(Clone, Copy, Debug)]
pub struct SigmoidParams {
    pub a: f32,
    pub c: f32,
    pub n: f32,
}

/// Solve the anchor/normalisation pair for a Contrast slider value (±200).
pub fn sigmoid_params(contrast: f32) -> SigmoidParams {
    let u = control_to_unit(contrast);
    let c = SIGMOID_BASE_C * (SIGMOID_C_RANGE * u).exp2();
    let top = SCENE_EV_MAX.exp2();
    let mut n = 1.0f32;
    let mut a = SCENE_MID_GRAY;
    // Fixed point: (a/top)^c is tiny, so this converges in a couple of steps.
    for _ in 0..3 {
        a = SCENE_MID_GRAY * (n / SCENE_MID_GRAY - 1.0).powf(1.0 / c);
        n = 1.0 + (a / top).powf(c);
    }
    SigmoidParams { a, c, n }
}

/// The log-logistic sigmoid itself (display-linear output in [0,1]).
/// Monotone in `v`; 0 at and below 0 (negative = out-of-gamut, resolved by the
/// ratio path + gamut clip, not by the per-channel curve).
#[inline]
pub fn sigmoid_eval(p: &SigmoidParams, v: f32) -> f32 {
    if v <= 0.0 {
        return 0.0;
    }
    (p.n / (1.0 + (p.a / v).powf(p.c))).clamp(0.0, 1.0)
}

/// LUT index (0..1) for a linear scene value on the shared EV axis.
#[inline]
pub fn scene_lut_t(v: f32) -> f32 {
    let ev = v.max(SCENE_EV_MIN.exp2()).log2();
    ((ev - SCENE_EV_MIN) / (SCENE_EV_MAX - SCENE_EV_MIN)).clamp(0.0, 1.0)
}

/// Auto baseline-exposure gain (linear multiplier) that makes the RAW default
/// render land at `target_display_mean` — used to align the scene render with
/// the brightness of the camera's embedded JPEG preview, so opening a RAW isn't
/// darker/flatter than the preview the user just saw. `scene_rgb` is a subsample
/// of the scene-linear master; the gain is bisected against the REAL default
/// tone transform (`scene_to_display` at default settings — the same one
/// `render_default_look` runs), which is monotone in the gain, so a handful of
/// geometric (EV-linear) steps converge. Clamped to ±3 EV.
pub fn baseline_exposure_gain(scene_rgb: &[[f32; 3]], target_display_mean: f32) -> f32 {
    if scene_rgb.is_empty() || !target_display_mean.is_finite() {
        return 1.0;
    }
    let target = target_display_mean.clamp(0.0, 1.0);
    let tone = build_scene_tone(&DevelopSettings::default());
    let rendered_mean = |k: f32| -> f32 {
        let sum: f32 = scene_rgb
            .iter()
            .map(|&p| {
                let d = tone.scene_to_display([p[0] * k, p[1] * k, p[2] * k], None);
                0.2126 * d[0] + 0.7152 * d[1] + 0.0722 * d[2]
            })
            .sum();
        sum / scene_rgb.len() as f32
    };
    let (mut lo, mut hi) = (0.125f32, 8.0f32); // ±3 EV
    if rendered_mean(hi) <= target {
        return hi;
    }
    if rendered_mean(lo) >= target {
        return lo;
    }
    for _ in 0..18 {
        let mid = (lo * hi).sqrt(); // geometric midpoint = EV-linear
        if rendered_mean(mid) < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo * hi).sqrt()
}

/// Signed tone-equalizer offset (EV) for a regional exposure `e` (EV, absolute)
/// under the four Light sliders. Direct evaluation — the LUT bakes this.
pub fn tone_eq_offset_ev(settings: &DevelopSettings, e: f32) -> f32 {
    let er = e - SCENE_MID_GRAY.log2();
    let g = |zone: (f32, f32, f32), u: f32| -> f32 {
        let d = (er - zone.0) / zone.1;
        zone.2 * u * (-0.5 * d * d).exp()
    };
    let off = g(TONE_EQ_SHADOWS, control_to_unit(settings.shadows))
        + g(TONE_EQ_BLACKS, control_to_unit(settings.blacks))
        + g(TONE_EQ_HIGHLIGHTS, control_to_unit(settings.highlights))
        + g(TONE_EQ_WHITES, control_to_unit(settings.whites));
    off.clamp(-TONE_EQ_CLAMP_EV, TONE_EQ_CLAMP_EV)
}

// ── SceneToneData: the per-edit precomputed chain ───────────────────────────

/// Precomputed scene tone stage — the single source of truth shared by the CPU
/// bake, the histogram, the proxies and the GPU preview (which receives the
/// same matrix + LUTs). Built once per slider change, never per pixel.
#[derive(Clone)]
pub struct SceneToneData {
    /// CAT16 white balance composed with the exposure multiplier: one matrix
    /// applied to the linear scene value. Identity·1 at neutral.
    pub wb_ev: [[f32; 3]; 3],
    /// Sigmoid over the EV axis: `lut[i] = S(2^EV_i)`, display-linear [0,1].
    pub lut: [f32; 256],
    /// Tone-equalizer EV offsets over the same axis (all zero when inactive).
    pub tone_eq: [f32; 256],
    pub tone_eq_active: bool,
    /// Raw-look perceptual shadow-chroma compensation. Identity sources bypass
    /// it so tone-neutral non-RAW pixels remain bit-stable.
    pub shadow_chroma_active: bool,
    /// Display-domain (gamma) luma curve: parametric curve_* sliders + the
    /// luminance point curve. `None` when identity.
    pub display: Option<Box<[f32; 256]>>,
    /// Per-channel R/G/B point curves (shared builder with the legacy engine).
    pub rgb: Option<Box<[[f32; 256]; 3]>>,
    /// Direct sigmoid parameters for exact (non-LUT) evaluation in tests.
    #[allow(dead_code)]
    pub sigmoid: SigmoidParams,
}

/// Build the scene tone stage from the Develop settings.
pub fn build_scene_tone(settings: &DevelopSettings) -> SceneToneData {
    build_scene_tone_for(settings, BaseLook::Raw)
}

/// Build the scene tone stage for a given base look. Both looks share the
/// whole chain structure (CAT16·2^EV matrix, EV-domain tone equalizer, the
/// log2-indexed tone LUT the CPU and GPU consume, display curves) — only the
/// LUT CONTENT and where Contrast lives differ, so the WGSL chain needs no
/// flag: it renders whatever tables it is given.
pub fn build_scene_tone_for(settings: &DevelopSettings, look: BaseLook) -> SceneToneData {
    let wb = cat16::wb_matrix(settings.temperature, settings.tint);
    let ev = exposure_multiplier(settings.exposure);
    let mut wb_ev = wb;
    for row in &mut wb_ev {
        for v in row {
            *v *= ev;
        }
    }
    let sigmoid;
    let mut lut = [0.0f32; 256];
    match look {
        BaseLook::Raw => {
            sigmoid = sigmoid_params(settings.contrast);
            for (i, slot) in lut.iter_mut().enumerate() {
                let e = SCENE_EV_MIN + (SCENE_EV_MAX - SCENE_EV_MIN) * i as f32 / 255.0;
                *slot = sigmoid_eval(&sigmoid, e.exp2());
            }
        }
        BaseLook::Identity => {
            // Identity map (a linearized display source reproduces itself
            // bit-stably at tone-neutral settings — colour-only edits must
            // not shift tone); values pushed past 1 clip to hard white at the
            // chain's gamut-clip + encode, exactly like ACR's Exposure on a
            // JPEG (no headroom to recover — highlight compression is the
            // tone equalizer's job, pulling down in EV BEFORE the clip).
            // The LUT stays UNCLAMPED on purpose: baking min(v,1) into it
            // puts a kink inside one log2 cell and the interpolation then
            // dips ~0.6% just below white — a pure exponential interpolates
            // to ~4e-4 everywhere, and the downstream clamp is exact.
            for (i, slot) in lut.iter_mut().enumerate() {
                let e = SCENE_EV_MIN + (SCENE_EV_MAX - SCENE_EV_MIN) * i as f32 / 255.0;
                *slot = e.exp2();
            }
            // Unused by the identity chain (the LUT above drives tone_map);
            // kept neutral so any consumer sees a sane anchored curve.
            sigmoid = sigmoid_params(0.0);
        }
    }
    let tone_eq_active = settings.has_local_tone();
    let mut tone_eq = [0.0f32; 256];
    if tone_eq_active {
        for (i, slot) in tone_eq.iter_mut().enumerate() {
            let e = SCENE_EV_MIN + (SCENE_EV_MAX - SCENE_EV_MIN) * i as f32 / 255.0;
            *slot = tone_eq_offset_ev(settings, e);
        }
    }
    SceneToneData {
        wb_ev,
        lut,
        tone_eq,
        tone_eq_active,
        shadow_chroma_active: look == BaseLook::Raw,
        display: build_display_lut_for(settings, look),
        rgb: rgb_curve_luts(settings),
        sigmoid,
    }
}

/// Exposure slider (±`EXPOSURE_LIMIT`) → linear multiplier, exactly linear in
/// EV (±5 EV). The old powf(1.7) easing is gone: a physical multiply plus the
/// sigmoid's shoulder already keeps small drags gentle.
pub fn exposure_multiplier(exposure: f32) -> f32 {
    const MAX_EV: f32 = 5.0;
    ((exposure / EXPOSURE_LIMIT).clamp(-1.0, 1.0) * MAX_EV).exp2()
}

/// Display-domain luma curve (parametric curve_* + luminance point curve), or
/// None at identity. For the Raw look, Contrast and H/S/W/B are NOT here —
/// they live in the sigmoid and the tone equalizer. For the Identity look,
/// Contrast seeds this curve with the legacy tanh S-curve (same order the old
/// engine used: contrast first, then the curve_* offsets, then the point
/// curve) — the identity tone map upstream has no steepness to carry it.
/// Region masks match the legacy engine so the curve panel feels unchanged.
fn build_display_lut_for(settings: &DevelopSettings, look: BaseLook) -> Option<Box<[f32; 256]>> {
    let contrast = match look {
        BaseLook::Raw => 0.0,
        BaseLook::Identity => eased_control(settings.contrast),
    };
    let ch = eased_control(settings.curve_highlights);
    let cl = eased_control(settings.curve_lights);
    let cd = eased_control(settings.curve_darks);
    let cs = eased_control(settings.curve_shadows);
    if contrast.abs() < 1e-4
        && ch.abs() < 1e-4
        && cl.abs() < 1e-4
        && cd.abs() < 1e-4
        && cs.abs() < 1e-4
        && curve_is_identity(&settings.curve_points)
    {
        return None;
    }
    let mut lut = [0.0f32; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let mut l = contrast_curve(i as f32 / 255.0, contrast * 1.05);
        l += ch * 0.18 * smootherstep(0.64, 1.0, l);
        l += cl * 0.18 * bell(l, 0.62, 0.34);
        l += cd * 0.16 * darks_mask(l);
        l += cs * 0.14 * curve_shadows_mask(l);
        *slot = l.clamp(0.0, 1.0);
    }
    for i in 1..lut.len() {
        if lut[i] < lut[i - 1] {
            lut[i] = lut[i - 1];
        }
    }
    apply_point_curve_outer(&mut lut, settings);
    Some(Box::new(lut))
}

/// Soft gamut resolution in display-linear space: pull the chroma vector back
/// toward the (clamped) luminance just far enough that every channel fits
/// [0,1]. Luminance and the RGB hue direction are preserved exactly; only
/// colours outside the cube move at all.
#[inline]
pub fn gamut_clip_chroma(c: [f32; 3]) -> [f32; 3] {
    let y = luma_lin(c[0], c[1], c[2]).clamp(0.0, 1.0);
    let d = [c[0] - y, c[1] - y, c[2] - y];
    let mut f = 1.0f32;
    for &dc in &d {
        if dc > 1e-6 {
            f = f.min((1.0 - y) / dc);
        } else if dc < -1e-6 {
            f = f.min(y / -dc);
        }
    }
    let f = f.clamp(0.0, 1.0);
    [
        (y + d[0] * f).clamp(0.0, 1.0),
        (y + d[1] * f).clamp(0.0, 1.0),
        (y + d[2] * f).clamp(0.0, 1.0),
    ]
}

/// Film-like RGB ceiling: keep the darkest channel fixed, put the brightest
/// at white, and interpolate the middle channel by its original rank. Unlike
/// independent channel clipping this retains the highlight's hue trajectory.
#[inline]
fn filmlike_clip(c: [f32; 3]) -> [f32; 3] {
    let lo = c[0].min(c[1]).min(c[2]);
    let hi = c[0].max(c[1]).max(c[2]);
    if hi <= 1.0 {
        return c;
    }
    if lo >= 1.0 || hi - lo <= 1e-8 {
        return [1.0; 3];
    }
    let lo = lo.max(0.0);
    let scale = (1.0 - lo) / (hi - lo).max(1e-8);
    [
        lo + (c[0] - lo) * scale,
        lo + (c[1] - lo) * scale,
        lo + (c[2] - lo) * scale,
    ]
}

/// Bleach only highlights whose scene value is being compressed by the tone
/// map. More compression and greater displayed brightness both increase the
/// pull toward white; uncompressed pixels are exact no-ops.
#[inline]
fn compress_highlight_chroma(c: [f32; 3], mapped_n: f32, scene_n: f32) -> [f32; 3] {
    let ratio = mapped_n / scene_n.max(1e-8);
    if ratio >= 1.0 {
        return c;
    }
    let compression = smootherstep(0.02, 0.80, 1.0 - ratio);
    let highlight = smootherstep(0.55, 1.0, mapped_n);
    let amount = compression * highlight * 0.65;
    [
        c[0] + (mapped_n - c[0]) * amount,
        c[1] + (mapped_n - c[1]) * amount,
        c[2] + (mapped_n - c[2]) * amount,
    ]
}

/// Perceptual tone curves lose colour in the toe. Restore at most 20% chroma
/// through the coloured-shadow band, leaving black, midtones and neutrals alone.
#[inline]
fn restore_shadow_chroma(c: [f32; 3]) -> [f32; 3] {
    let y = luma_lin(c[0], c[1], c[2]).clamp(0.0, 1.0);
    let d = [c[0] - y, c[1] - y, c[2] - y];
    let chroma = d[0].max(d[1]).max(d[2]) - d[0].min(d[1]).min(d[2]);
    let tonal = smootherstep(0.08, 0.18, y) * (1.0 - smootherstep(0.38, 0.55, y));
    let colored = smootherstep(0.015, 0.10, chroma);
    let scale = 1.0 + 0.20 * tonal * colored;
    [y + d[0] * scale, y + d[1] * scale, y + d[2] * scale]
}

impl SceneToneData {
    /// Absolute exposure (EV) of a scene value after WB+EV — the signal the
    /// tone-equalizer zones read, and what the regional-E proxy stores.
    #[inline]
    pub fn own_e(&self, rgb_after_wb_ev: [f32; 3]) -> f32 {
        luma_lin(rgb_after_wb_ev[0], rgb_after_wb_ev[1], rgb_after_wb_ev[2])
            .max(SCENE_EV_MIN.exp2())
            .log2()
    }

    /// Tone-equalizer offset (EV) at an absolute exposure `e`, via the LUT.
    #[inline]
    pub fn tone_eq_at(&self, e: f32) -> f32 {
        let t = ((e - SCENE_EV_MIN) / (SCENE_EV_MAX - SCENE_EV_MIN)).clamp(0.0, 1.0);
        lut_lerp(&self.tone_eq, t)
    }

    /// Sigmoid through the LUT (display-linear output).
    #[inline]
    fn tone_map(&self, v: f32) -> f32 {
        if v <= 0.0 {
            return 0.0;
        }
        lut_lerp(&self.lut, scene_lut_t(v))
    }

    /// The full scene chain for one pixel: linear scene RGB → display-referred
    /// gamma sRGB in [0,1]. `region_e` is the regional exposure from the
    /// guided proxy; None falls back to the pixel's own exposure (histogram /
    /// proxy-free paths — identical for smooth areas by construction).
    pub fn scene_to_display(&self, rgb: [f32; 3], region_e: Option<f32>) -> [f32; 3] {
        let mut v = cat16::mat_apply(&self.wb_ev, rgb);
        if self.tone_eq_active {
            let e = region_e.unwrap_or_else(|| self.own_e(v));
            let gain = self.tone_eq_at(e).exp2();
            v = [v[0] * gain, v[1] * gain, v[2] * gain];
        }
        // Per-channel sigmoid (film-like hue skew)…
        let pc = [
            self.tone_map(v[0]),
            self.tone_map(v[1]),
            self.tone_map(v[2]),
        ];
        // …blended toward the max-RGB ratio path (spectral hue preserved).
        let n = v[0].max(v[1]).max(v[2]);
        let out = if n > 1e-8 {
            let mapped_n = self.tone_map(n);
            let s = mapped_n / n;
            let blend = hue_preserve_blend(mapped_n);
            let mixed = [
                pc[0] + (v[0] * s - pc[0]) * blend,
                pc[1] + (v[1] * s - pc[1]) * blend,
                pc[2] + (v[2] * s - pc[2]) * blend,
            ];
            compress_highlight_chroma(mixed, mapped_n, n)
        } else {
            pc
        };
        let out = if self.shadow_chroma_active {
            restore_shadow_chroma(out)
        } else {
            out
        };
        let clipped = gamut_clip_chroma(filmlike_clip(out));
        let mut r = linear_to_srgb(clipped[0]);
        let mut g = linear_to_srgb(clipped[1]);
        let mut b = linear_to_srgb(clipped[2]);
        self.apply_display_curves(&mut r, &mut g, &mut b);
        [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)]
    }

    /// Display-domain curves (gamma space): luminance curve then R/G/B point
    /// curves — the same semantics the legacy engine gave them.
    #[inline]
    pub fn apply_display_curves(&self, r: &mut f32, g: &mut f32, b: &mut f32) {
        if let Some(lut) = &self.display {
            let l = luminance_f32(*r, *g, *b).clamp(0.0, 1.0);
            let target = lut_lerp(lut, l);
            apply_luma_target(r, g, b, target);
        }
        if let Some(luts) = &self.rgb {
            *r = lut_lerp(&luts[0], r.clamp(0.0, 1.0));
            *g = lut_lerp(&luts[1], g.clamp(0.0, 1.0));
            *b = lut_lerp(&luts[2], b.clamp(0.0, 1.0));
        }
    }

    /// Display-domain (gamma) luma a neutral grey of absolute exposure `e`
    /// renders at — used to map the regional-E proxy into the display domain
    /// for the spatial effects' base.
    #[allow(dead_code)] // wired when the effects stage reads the E-plane directly
    pub fn display_luma_for_e(&self, e: f32) -> f32 {
        let v = e.exp2();
        let toned = if self.tone_eq_active {
            self.tone_map(v * self.tone_eq_at(e).exp2())
        } else {
            self.tone_map(v)
        };
        let mut r = linear_to_srgb(toned);
        let mut g = r;
        let mut b = r;
        self.apply_display_curves(&mut r, &mut g, &mut b);
        luminance_f32(r, g, b).clamp(0.0, 1.0)
    }
}

// ── Regional-E proxy (tone equalizer's guided mask) ─────────────────────────

/// Tone-independent base for the regional exposure plane: box-average the
/// LINEAR scene RGB over s×s blocks. Cacheable across a whole session — only
/// the layer changes invalidate it, not a slider drag.
pub fn build_scene_region_base(scene: &SceneSource, s: usize) -> (Vec<[f32; 3]>, usize, usize) {
    let w = scene.width as usize;
    let h = scene.height as usize;
    let s = s.max(1);
    if w == 0 || h == 0 {
        return (Vec::new(), 0, 0);
    }
    let pw = w.div_ceil(s);
    let ph = h.div_ceil(s);
    let mut low = vec![[0.0f32; 3]; pw * ph];
    low.par_chunks_mut(pw).enumerate().for_each(|(py, row)| {
        let sy0 = py * s;
        let sy1 = (sy0 + s).min(h);
        for (px, slot) in row.iter_mut().enumerate() {
            let sx0 = px * s;
            let sx1 = (sx0 + s).min(w);
            let mut acc = [0.0f32; 3];
            let mut weight = 0.0f32;
            for yy in sy0..sy1 {
                let base = (yy * w) * 4;
                for xx in sx0..sx1 {
                    let i = base + xx * 4;
                    let a = scene.alpha_at_index(yy * w + xx);
                    acc[0] += f16_bits_to_f32(scene.half[i]) * a;
                    acc[1] += f16_bits_to_f32(scene.half[i + 1]) * a;
                    acc[2] += f16_bits_to_f32(scene.half[i + 2]) * a;
                    weight += a;
                }
            }
            if weight > 1e-6 {
                *slot = [acc[0] / weight, acc[1] / weight, acc[2] / weight];
            }
        }
    });
    (low, pw, ph)
}

/// Per-frame tail: WB+EV on the cached block averages → absolute exposure E =
/// log2(luma) → edge-aware guided low-pass ON THE EV PLANE. Working in EV makes
/// the smoothing exposure-independent (the same blur in shadows and highlights
/// — the exposure-independent guided-filter insight) and hands the tone equalizer the signal its
/// zones are defined on.
pub fn finish_region_e(
    base: &[[f32; 3]],
    pw: usize,
    ph: usize,
    tone: &SceneToneData,
    s: usize,
) -> Vec<f32> {
    let floor = SCENE_EV_MIN.exp2();
    let e: Vec<f32> = base
        .par_iter()
        .map(|p| {
            let v = cat16::mat_apply(&tone.wb_ev, *p);
            luma_lin(v[0], v[1], v[2]).max(floor).log2()
        })
        .collect();
    let r = (TONE_REGION_RADIUS / s.max(1)).max(1);
    guided_lowpass_plane(&e, pw, ph, r, SCENE_TONE_GUIDED_EPS)
}

// ── GPU-preview proxy builders (scene twins of the legacy box builders) ────

/// Tone-independent colour-stage base for a viewport box: box-average the
/// LINEAR scene RGB over s×s blocks of the box. Cacheable across a session;
/// the per-frame tail is [`tone_lowpass_scene_region`].
pub fn build_scene_color_base_box(
    scene: &SceneSource,
    origin_x: u32,
    origin_y: u32,
    region_w: u32,
    region_h: u32,
    s: usize,
) -> (Vec<[f32; 3]>, usize, usize) {
    let w = scene.width as usize;
    let h = scene.height as usize;
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
    let mut low = vec![[0.0f32; 3]; pw * ph];
    low.par_chunks_mut(pw).enumerate().for_each(|(py, row)| {
        let sy0 = oy + py * s;
        let sy1 = (sy0 + s).min(oy + rh).min(h);
        for (px, slot) in row.iter_mut().enumerate() {
            let sx0 = ox + px * s;
            let sx1 = (sx0 + s).min(ox + rw).min(w);
            let mut acc = [0.0f32; 3];
            let mut weight = 0.0f32;
            for yy in sy0..sy1 {
                let base = (yy * w) * 4;
                for xx in sx0..sx1 {
                    let i = base + xx * 4;
                    let a = scene.alpha_at_index(yy * w + xx);
                    acc[0] += f16_bits_to_f32(scene.half[i]) * a;
                    acc[1] += f16_bits_to_f32(scene.half[i + 1]) * a;
                    acc[2] += f16_bits_to_f32(scene.half[i + 2]) * a;
                    weight += a;
                }
            }
            if weight > 1e-6 {
                *slot = [acc[0] / weight, acc[1] / weight, acc[2] / weight];
            }
        }
    });
    (low, pw, ph)
}

/// Per-frame colour-region tail for scene sessions: run the scene chain on the
/// cached linear base (own-exposure tone-eq — the base is already regional),
/// then the same edge-aware guided low-pass the commit's colour stage applies
/// on the display image. Output is display-domain, so `apply_color_to_region`
/// and the shader's `dev_finish_colored` consume it exactly like the legacy path.
pub fn tone_lowpass_scene_region(
    base: &[[f32; 3]],
    pw: usize,
    ph: usize,
    tone: &SceneToneData,
    s: usize,
) -> Vec<[f32; 3]> {
    let toned: Vec<[f32; 3]> = base
        .par_iter()
        .map(|p| tone.scene_to_display(*p, None))
        .collect();
    let r = (crate::core::develop::COLOR_REGION_RADIUS / s.max(1)).max(1);
    crate::core::develop::guided_lowpass(&toned, pw, ph, r, crate::core::develop::COLOR_GUIDED_EPS)
}

/// Point-sampled scene base for the fast (Effects) preview proxy — linear
/// scene RGB, one sample per s×s block. Cacheable; the per-frame tail applies
/// the scene chain + the leftover (stripped) stages.
pub fn build_scene_fast_base(
    scene: &SceneSource,
    origin_x: u32,
    origin_y: u32,
    region_w: u32,
    region_h: u32,
    s: usize,
) -> (Vec<[f32; 3]>, usize, usize) {
    let w = scene.width as usize;
    let h = scene.height as usize;
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
    let mut out = vec![[0.0f32; 3]; pw * ph];
    out.par_chunks_mut(pw).enumerate().for_each(|(py, row)| {
        let sy = (oy + (py * s) + s / 2).min(oy + rh - 1).min(h - 1);
        for (px, slot) in row.iter_mut().enumerate() {
            let sx = (ox + (px * s) + s / 2).min(ox + rw - 1).min(w - 1);
            let i = (sy * w + sx) * 4;
            *slot = [
                f16_bits_to_f32(scene.half[i]),
                f16_bits_to_f32(scene.half[i + 1]),
                f16_bits_to_f32(scene.half[i + 2]),
            ];
        }
    });
    (out, pw, ph)
}

/// Per-frame fast-preview tail: scene chain per texel → display domain.
/// The caller then runs `apply_fast_preview_to_region` with the STRIPPED
/// settings so only the leftover stages (Colour/Effects) re-apply.
pub fn scene_fast_region_display(base: &[[f32; 3]], tone: &SceneToneData) -> Vec<[f32; 3]> {
    base.par_iter()
        .map(|p| tone.scene_to_display(*p, None))
        .collect()
}

// ── Full renders ─────────────────────────────────────────────────────────────

/// Render the scene at the current settings into a display-referred RGBA16
/// (gamma sRGB) buffer — the Light half only (Colour/Effects/Detail/Locals run
/// on top via the legacy stages in [`apply_scene_to_tilemap`]).
fn render_scene_display(scene: &SceneSource, tone: &SceneToneData) -> Vec<u16> {
    let w = scene.width as usize;
    let h = scene.height as usize;
    let region = if tone.tone_eq_active {
        let (base, pw, ph) = build_scene_region_base(scene, TONE_DOWNSAMPLE);
        Some((
            finish_region_e(&base, pw, ph, tone, TONE_DOWNSAMPLE),
            pw,
            ph,
        ))
    } else {
        None
    };
    let s = TONE_DOWNSAMPLE as f32;
    let mut out = vec![0u16; w * h * 4];
    out.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        let fy = (y as f32 + 0.5) / s - 0.5;
        for x in 0..w {
            let i = (y * w + x) * 4;
            let rgb = [
                f16_bits_to_f32(scene.half[i]),
                f16_bits_to_f32(scene.half[i + 1]),
                f16_bits_to_f32(scene.half[i + 2]),
            ];
            let e = region.as_ref().map(|(plane, pw, ph)| {
                sample_plane_bilinear(plane, *pw, *ph, (x as f32 + 0.5) / s - 0.5, fy)
            });
            let d = tone.scene_to_display(rgb, e);
            let o = x * 4;
            row[o] = (d[0] * 65535.0 + 0.5) as u16;
            row[o + 1] = (d[1] * 65535.0 + 0.5) as u16;
            row[o + 2] = (d[2] * 65535.0 + 0.5) as u16;
            row[o + 3] = scene.alpha16_at_index(y * w + x);
        }
    });
    out
}

/// The default (neutral-settings) render — what the RAW decode bakes into the
/// document tiles, so opening the Develop panel at neutral shows exactly the
/// pixels already on screen (zero-work preview, no pop) and the "Open Image"
/// neutral shortcut is exact.
pub fn render_default_look(scene: &SceneSource) -> Vec<u16> {
    render_scene_display(scene, &build_scene_tone(&DevelopSettings::default()))
}

/// Strip every slider the scene chain already applied, leaving the stages the
/// legacy engine still owns (Colour, Effects, Detail, Locals). Public because
/// the live fast-preview path re-applies the leftover stages on a proxy.
pub fn strip_scene_handled(settings: &DevelopSettings) -> DevelopSettings {
    DevelopSettings {
        exposure: 0.0,
        contrast: 0.0,
        highlights: 0.0,
        shadows: 0.0,
        whites: 0.0,
        blacks: 0.0,
        temperature: 0.0,
        tint: 0.0,
        curve_highlights: 0.0,
        curve_lights: 0.0,
        curve_darks: 0.0,
        curve_shadows: 0.0,
        curve_points: crate::core::develop::identity_curve(),
        curve_points_r: crate::core::develop::identity_curve(),
        curve_points_g: crate::core::develop::identity_curve(),
        curve_points_b: crate::core::develop::identity_curve(),
        ..settings.clone()
    }
}

/// Commit bake for a scene-referred (RAW) Develop session: the scene chain
/// renders the Light half at f32 → 16-bit display tiles, then the untouched
/// legacy stages (Colour / Effects / Detail / Locals) run on that exactly as
/// they always ran on a toned document. Selections never occur in the RAW
/// pre-editor, so the scene half applies unconditionally.
pub fn apply_scene_to_tilemap(
    scene: &SceneSource,
    settings: &DevelopSettings,
    selection: Option<crate::core::develop::DevelopSelection>,
) -> TileMap {
    let tone = build_scene_tone_for(settings, scene.look);
    let px16 = render_scene_display(scene, &tone);
    let display = TileMap::from_rgba16(&px16, scene.width, scene.height);
    drop(px16);
    let rest = strip_scene_handled(settings);
    if rest.is_neutral() {
        let mut display = display;
        display.bump_all_revisions();
        return display;
    }
    crate::core::develop::apply_to_tilemap_direct(&display, &rest, selection)
}

// ── Histogram ────────────────────────────────────────────────────────────────

/// Pixel budget mirrors the legacy histogram proxy.
const HISTOGRAM_PROXY_PIXELS: u64 = 60_000;

/// Grid-sampled LINEAR scene pixels, cached for the session; the histogram is
/// re-binned from this through the live scene chain on every slider tick.
pub fn build_scene_histogram_proxy(scene: &SceneSource) -> Vec<[f32; 3]> {
    let (w, h) = (scene.width, scene.height);
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let total = w as u64 * h as u64;
    let step = (((total / HISTOGRAM_PROXY_PIXELS).max(1) as f64)
        .sqrt()
        .ceil() as u32)
        .max(1);
    let mut out = Vec::with_capacity((total / (step as u64 * step as u64) + 1) as usize);
    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            let pixel_index = y as usize * w as usize + x as usize;
            if scene.alpha16_at_index(pixel_index) > 0 {
                out.push(scene.get_rgb(x, y));
            }
            x += step;
        }
        y += step;
    }
    out
}

/// R/G/B/Luma histogram of the linear proxy AFTER the scene chain + colour
/// stage, each normalised to its own peak — the scene twin of
/// `develop::histogram_rgbl`. Tone-equalizer offsets use each sample's own
/// exposure (no spatial neighbourhood on a scatter of samples).
pub fn histogram_rgbl_scene(
    proxy: &[[f32; 3]],
    settings: &DevelopSettings,
    look: BaseLook,
) -> [[f32; 256]; 4] {
    let tone = build_scene_tone_for(settings, look);
    let use_color = settings.has_color();
    let curves = crate::core::develop::build_mixer_curves_opt(settings);
    let counts = proxy
        .par_chunks(4096)
        .map(|chunk| {
            let mut counts = [[0u32; 256]; 4];
            for p in chunk {
                let d = tone.scene_to_display(*p, None);
                let (mut r, mut g, mut b) = (d[0], d[1], d[2]);
                if use_color {
                    apply_color(settings, curves.as_ref(), &mut r, &mut g, &mut b);
                    r = r.clamp(0.0, 1.0);
                    g = g.clamp(0.0, 1.0);
                    b = b.clamp(0.0, 1.0);
                }
                let l = luminance_f32(r, g, b).clamp(0.0, 1.0);
                counts[0][((r * 255.0 + 0.5) as usize).min(255)] += 1;
                counts[1][((g * 255.0 + 0.5) as usize).min(255)] += 1;
                counts[2][((b * 255.0 + 0.5) as usize).min(255)] += 1;
                counts[3][((l * 255.0 + 0.5) as usize).min(255)] += 1;
            }
            counts
        })
        .reduce(
            || [[0u32; 256]; 4],
            |mut a, b| {
                for ch in 0..4 {
                    for i in 0..256 {
                        a[ch][i] += b[ch][i];
                    }
                }
                a
            },
        );
    let mut out = [[0.0f32; 256]; 4];
    for ch in 0..4 {
        let max = counts[ch].iter().copied().max().unwrap_or(0).max(1) as f32;
        for i in 0..256 {
            out[ch][i] = counts[ch][i] as f32 / max;
        }
    }
    out
}

/// Display-referred RGB (each 0–1) of ONE scene-linear sample under `settings`
/// — exactly the per-sample chain `histogram_rgbl_scene` runs (tone map +
/// colour stage; no spatial stages), so a cursor readout agrees with the
/// histogram it sits under. Build cost is one tone solve per call: fine for a
/// per-frame single-pixel readout, wrong for bulk work (use the tilemap paths).
pub fn eval_scene_pixel(rgb: [f32; 3], settings: &DevelopSettings, look: BaseLook) -> [f32; 3] {
    let tone = build_scene_tone_for(settings, look);
    let d = tone.scene_to_display(rgb, None);
    let (mut r, mut g, mut b) = (d[0], d[1], d[2]);
    if settings.has_color() {
        let curves = crate::core::develop::build_mixer_curves_opt(settings);
        apply_color(settings, curves.as_ref(), &mut r, &mut g, &mut b);
    }
    [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> DevelopSettings {
        DevelopSettings::default()
    }

    #[test]
    fn baseline_gain_matches_target_brightness() {
        // A dark-ish grey scene: its default render mean should be liftable to a
        // brighter target and pushable to a darker one.
        let samples = vec![[0.05f32, 0.05, 0.05]; 200];
        let tone = build_scene_tone(&DevelopSettings::default());
        let render_mean = |k: f32| -> f32 {
            let sum: f32 = samples
                .iter()
                .map(|&p| {
                    let d = tone.scene_to_display([p[0] * k, p[1] * k, p[2] * k], None);
                    0.2126 * d[0] + 0.7152 * d[1] + 0.0722 * d[2]
                })
                .sum();
            sum / samples.len() as f32
        };

        // Empty / non-finite inputs are no-ops.
        assert_eq!(baseline_exposure_gain(&[], 0.5), 1.0);
        assert_eq!(baseline_exposure_gain(&samples, f32::NAN), 1.0);

        // For a reachable target the fit lands close and beats the unlifted mean.
        for &target in &[0.35f32, 0.55, 0.7] {
            let g = baseline_exposure_gain(&samples, target);
            assert!((0.125..=8.0).contains(&g), "gain {g} out of clamp");
            let fit_err = (render_mean(g) - target).abs();
            let unit_err = (render_mean(1.0) - target).abs();
            assert!(fit_err < 0.02, "target {target}: fit_err {fit_err}");
            assert!(fit_err <= unit_err + 1e-4);
        }
    }

    #[test]
    fn f16_roundtrip_covers_photographic_range() {
        for &v in &[
            0.0f32, 1.0, 0.5, 0.1845, 0.0001, 0.000061, 4.0, 64.0, -0.25, 1e-6, 100.0,
        ] {
            let back = f16_bits_to_f32(f32_to_f16_bits(v));
            let tol = (v.abs() * 1e-3).max(1e-7);
            assert!((back - v).abs() <= tol, "f16 roundtrip {v} -> {back}");
        }
        assert_eq!(f16_bits_to_f32(0x3c00), 1.0);
    }

    #[test]
    fn sigmoid_hue_preservation_releases_only_near_white() {
        assert!((hue_preserve_blend(0.25) - 0.90).abs() < 1e-7);
        assert!((hue_preserve_blend(0.50) - 0.90).abs() < 1e-7);
        assert!((hue_preserve_blend(1.00) - 0.20).abs() < 1e-7);

        let mid_highlight = hue_preserve_blend(0.75);
        assert!(
            (0.20..0.90).contains(&mid_highlight),
            "highlight transition must be smooth: {mid_highlight}"
        );
        assert!(hue_preserve_blend(0.65) > hue_preserve_blend(0.85));
    }

    #[test]
    fn compressed_highlights_fade_smoothly_toward_white() {
        let color = [0.80, 0.32, 0.12];
        let uncompressed = compress_highlight_chroma(color, 0.80, 0.80);
        assert_eq!(uncompressed, color);

        let mild = compress_highlight_chroma(color, 0.80, 1.0);
        let strong = compress_highlight_chroma(color, 0.80, 4.0);
        let chroma = |c: [f32; 3]| c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2]);
        assert!(chroma(strong) < chroma(mild));
        assert!(chroma(mild) < chroma(color));
        assert!((strong[0] - 0.80).abs() < 1e-7);
    }

    #[test]
    fn filmlike_clip_interpolates_the_middle_channel() {
        let out = filmlike_clip([1.40, 0.80, 0.20]);
        assert!((out[0] - 1.0).abs() < 1e-6, "max={}", out[0]);
        assert!((out[2] - 0.20).abs() < 1e-6);
        assert!((out[1] - 0.60).abs() < 1e-6, "middle={}", out[1]);
        assert_eq!(filmlike_clip([0.20, 0.40, 0.60]), [0.20, 0.40, 0.60]);
    }

    #[test]
    fn shadow_chroma_restore_preserves_luma_and_neutrals() {
        let color = [0.30, 0.16, 0.10];
        let out = restore_shadow_chroma(color);
        let chroma = |c: [f32; 3]| c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2]);
        assert!(chroma(out) > chroma(color));
        assert!(chroma(out) <= chroma(color) * 1.201);
        assert!(
            (luma_lin(out[0], out[1], out[2]) - luma_lin(color[0], color[1], color[2])).abs()
                < 1e-6
        );
        assert_eq!(restore_shadow_chroma([0.20; 3]), [0.20; 3]);
    }

    #[test]
    fn identity_look_does_not_restore_shadow_chroma() {
        let tone = build_scene_tone_for(&settings(), BaseLook::Identity);
        let srgb = [0.42f32, 0.24, 0.15];
        let input = [
            srgb_to_linear(srgb[0]),
            srgb_to_linear(srgb[1]),
            srgb_to_linear(srgb[2]),
        ];
        let out = tone.scene_to_display(input, None);
        for i in 0..3 {
            assert!((out[i] - srgb[i]).abs() < 2e-3, "channel {i}: {}", out[i]);
        }
    }

    #[test]
    fn identity_look_reproduces_the_source_exactly_at_tone_neutral() {
        // A linearized display source must come back byte-stable through the
        // identity chain whenever the Light half is untouched (colour-only
        // edits must not shift tone) — the JPEG twin of `is_neutral` no-op.
        let mut s = settings();
        s.mixer_saturation[0] = 80.0; // colour engaged, tone neutral
        let tone = build_scene_tone_for(&s, BaseLook::Identity);
        for i in 0..=100 {
            let srgb = i as f32 / 100.0;
            let v = srgb_to_linear(srgb);
            let out = tone.scene_to_display([v, v, v], None);
            assert!(
                (out[0] - srgb).abs() < 2e-3,
                "identity broke at {srgb}: {}",
                out[0]
            );
        }
    }

    #[test]
    fn identity_look_exposure_is_a_pure_gain_with_hard_white() {
        let plus1 = DevelopSettings {
            exposure: 10.0, // +1 EV
            ..settings()
        };
        let tone = build_scene_tone_for(&plus1, BaseLook::Identity);
        let mid = tone.scene_to_display([0.1, 0.1, 0.1], None);
        assert!(
            (mid[0] - linear_to_srgb(0.2)).abs() < 2e-3,
            "+1 EV must double linear: {}",
            mid[0]
        );
        // A JPEG has no highlight headroom: pushed past 1 clips to white.
        let bright = tone.scene_to_display([0.9, 0.9, 0.9], None);
        assert!(
            (bright[0] - 1.0).abs() < 1e-3,
            "expected white: {}",
            bright[0]
        );
    }

    #[test]
    fn identity_look_contrast_lives_in_the_display_curve() {
        let s = DevelopSettings {
            contrast: 80.0,
            ..settings()
        };
        let t = build_scene_tone_for(&s, BaseLook::Identity);
        let display = t.display.as_ref().expect("contrast builds a display curve");
        // Legacy tanh seed: mid anchored, S-shaped around it.
        assert!((lut_lerp(display, 0.5) - 0.5).abs() < 0.02);
        assert!(lut_lerp(display, 0.7) > 0.7 && lut_lerp(display, 0.3) < 0.3);
        // The scene tone LUT itself stays the identity map (no sigmoid
        // steepness — that is the Raw look's job).
        assert!((t.tone_map(0.5) - 0.5).abs() < 2e-3);
        // End-to-end the chain applies it.
        let hi = srgb_to_linear(0.75);
        let out = t.scene_to_display([hi, hi, hi], None);
        assert!(
            out[0] > 0.76,
            "contrast did not lift highlights: {}",
            out[0]
        );
    }

    #[test]
    fn from_display_tiles_roundtrips_every_8bit_value_within_dither() {
        // Neutral reproduces the source to within the sub-LSB input dither: an
        // 8-bit value comes back exact or ±1 (the dither is imperceptible at
        // neutral and only matters once a lift stretches the tones). A large
        // block (not a 1px column) so the ordered pattern is exercised, and the
        // MEAN over the block must land back on the source value exactly — the
        // dither is zero-mean, so it adds no bias, only breaks the levels up.
        let (w, h) = (64u32, 64u32);
        for &v in &[0u8, 1, 5, 17, 60, 128, 200, 254, 255] {
            let mut px = vec![0u8; (w * h * 4) as usize];
            for pixel in px.chunks_exact_mut(4) {
                pixel[0] = v;
                pixel[1] = v;
                pixel[2] = v;
                pixel[3] = u8::MAX;
            }
            let tiles = TileMap::from_rgba(&px, w, h);
            let scene = SceneSource::from_display_tiles(&tiles);
            assert_eq!(scene.look, BaseLook::Identity);
            assert!(
                scene.alpha.is_none(),
                "opaque input should not retain an alpha plane"
            );
            let tone = build_scene_tone_for(&settings(), BaseLook::Identity);
            let mut sum = 0.0f64;
            for y in 0..h {
                for x in 0..w {
                    let out = tone.scene_to_display(scene.get_rgb(x, y), None);
                    let back = (out[0] * 255.0).round() as i32;
                    assert!(
                        (back - v as i32).abs() <= 1,
                        "u8 {v} drifted to {back} at ({x},{y})"
                    );
                    sum += out[0] as f64 * 255.0;
                }
            }
            let mean = sum / (w * h) as f64;
            assert!(
                (mean - v as f64).abs() < 0.35,
                "dither biased {v}: mean {mean}"
            );
        }
    }

    #[test]
    fn identity_scene_commit_preserves_cutout_alpha_exactly() {
        // Layer-via-Copy leaves the source RGB under alpha=0. Develop must adjust
        // colour without making that hidden background visible again on commit.
        let alpha = [u16::MAX, 0, 12_345];
        let px16 = [
            12_000, 16_000, 20_000, alpha[0], 2_000, 50_000, 3_000, alpha[1], 24_000, 18_000,
            12_000, alpha[2],
        ];
        let source = TileMap::from_rgba16(&px16, 3, 1);
        let scene = SceneSource::from_display_tiles(&source);
        assert!(
            scene.alpha.is_some(),
            "transparent input needs exact alpha storage"
        );

        let out = apply_scene_to_tilemap(
            &scene,
            &DevelopSettings {
                exposure: 20.0,
                ..settings()
            },
            None,
        );

        for (x, expected) in alpha.into_iter().enumerate() {
            assert_eq!(
                out.get_pixel16(x as u32, 0).3,
                expected,
                "Develop changed alpha at pixel {x}"
            );
        }
        assert_eq!(
            out.get_pixel(1, 0).3,
            0,
            "hidden green background resurfaced"
        );
        assert_ne!(
            out.get_pixel16(0, 0).0,
            source.get_pixel16(0, 0).0,
            "test must exercise a non-neutral Develop bake"
        );
    }

    #[test]
    fn identity_scene_histogram_and_region_ignore_transparent_rgb() {
        let tiles = TileMap::from_rgba(&[200, 20, 10, 255, 0, 255, 0, 0], 2, 1);
        let scene = SceneSource::from_display_tiles(&tiles);

        let histogram = build_scene_histogram_proxy(&scene);
        assert_eq!(
            histogram.len(),
            1,
            "transparent pixels must not enter histogram"
        );

        let (region, pw, ph) = build_scene_region_base(&scene, 2);
        assert_eq!((pw, ph), (1, 1));
        assert!(
            region[0][0] > region[0][1] * 5.0,
            "hidden green RGB contaminated the alpha-weighted region: {:?}",
            region[0]
        );
    }

    #[test]
    fn sigmoid_hits_anchor_endpoints_and_stays_monotone() {
        for contrast in [-200.0f32, -80.0, 0.0, 90.0, 200.0] {
            let p = sigmoid_params(contrast);
            let mid = sigmoid_eval(&p, SCENE_MID_GRAY);
            assert!(
                (mid - SCENE_MID_GRAY).abs() < 1e-3,
                "mid-grey anchor broken at contrast {contrast}: {mid}"
            );
            let top = sigmoid_eval(&p, SCENE_EV_MAX.exp2());
            assert!((top - 1.0).abs() < 1e-3, "top {top} at contrast {contrast}");
            assert!(sigmoid_eval(&p, 0.0) == 0.0);
            let mut last = -1.0f32;
            for i in 0..400 {
                let v = (SCENE_EV_MIN + 20.0 * i as f32 / 399.0).exp2();
                let s = sigmoid_eval(&p, v);
                assert!(s >= last - 1e-6, "sigmoid dipped at {v}");
                last = s;
            }
        }
    }

    #[test]
    fn scene_lut_matches_direct_eval_within_tolerance() {
        let s = DevelopSettings {
            contrast: 60.0,
            ..settings()
        };
        let tone = build_scene_tone(&s);
        for i in 0..1000 {
            let ev = SCENE_EV_MIN + (SCENE_EV_MAX - SCENE_EV_MIN) * i as f32 / 999.0;
            let v = ev.exp2();
            let direct = sigmoid_eval(&tone.sigmoid, v);
            let via_lut = lut_lerp(&tone.lut, scene_lut_t(v));
            assert!(
                (direct - via_lut).abs() < 1e-3,
                "LUT error {} at EV {ev}",
                (direct - via_lut).abs()
            );
        }
    }

    #[test]
    fn exposure_is_an_exact_ev_multiply() {
        // +1 EV on v must equal 0 EV on 2v through the whole scene chain.
        let one_ev = DevelopSettings {
            exposure: 10.0,
            ..settings()
        };
        let t1 = build_scene_tone(&one_ev);
        let t0 = build_scene_tone(&settings());
        for &v in &[0.01f32, 0.1845, 0.5, 1.0, 3.0] {
            let a = t1.scene_to_display([v, v, v], None);
            let b = t0.scene_to_display([2.0 * v, 2.0 * v, 2.0 * v], None);
            assert!(
                (a[1] - b[1]).abs() < 2e-3,
                "EV multiply broken at {v}: {} vs {}",
                a[1],
                b[1]
            );
        }
        assert!((exposure_multiplier(50.0) - 32.0).abs() < 1e-3);
        assert!((exposure_multiplier(-50.0) - 1.0 / 32.0).abs() < 1e-4);
    }

    #[test]
    fn hdr_headroom_recovers_under_negative_exposure() {
        // THE capability this rewrite exists for: scene 4.0 (a blown highlight
        // in the default render) must come back with texture at −2 EV.
        let t0 = build_scene_tone(&settings());
        let d0a = t0.scene_to_display([3.0, 3.0, 3.0], None)[1];
        let d0b = t0.scene_to_display([4.0, 4.0, 4.0], None)[1];
        assert!(d0b > 0.95, "default render should be near white: {d0b}");

        let minus2 = DevelopSettings {
            exposure: -20.0,
            ..settings()
        };
        let t2 = build_scene_tone(&minus2);
        let a = t2.scene_to_display([3.0, 3.0, 3.0], None)[1];
        let b = t2.scene_to_display([4.0, 4.0, 4.0], None)[1];
        assert!(
            b < d0b - 0.06,
            "-2 EV must pull the highlight visibly off white: {b} (was {d0b})"
        );
        assert!(
            b - a > 0.03,
            "texture above display white must separate after recovery: {a} vs {b} (was {d0a} vs {d0b})"
        );
    }

    #[test]
    fn tone_eq_sliders_move_only_their_zone() {
        let zones = [
            (
                "shadows",
                TONE_EQ_SHADOWS,
                DevelopSettings {
                    shadows: 200.0,
                    ..settings()
                },
            ),
            (
                "blacks",
                TONE_EQ_BLACKS,
                DevelopSettings {
                    blacks: 200.0,
                    ..settings()
                },
            ),
            (
                "highlights",
                TONE_EQ_HIGHLIGHTS,
                DevelopSettings {
                    highlights: 200.0,
                    ..settings()
                },
            ),
            (
                "whites",
                TONE_EQ_WHITES,
                DevelopSettings {
                    whites: 200.0,
                    ..settings()
                },
            ),
        ];
        let mid_ev = SCENE_MID_GRAY.log2();
        for (name, zone, s) in &zones {
            let at_centre = tone_eq_offset_ev(s, mid_ev + zone.0);
            assert!(
                (at_centre - zone.2).abs() < 0.05,
                "{name}: full slider at its centre should be ≈{} EV, got {at_centre}",
                zone.2
            );
            // 5+ widths away the zone must be essentially silent.
            let far = tone_eq_offset_ev(s, mid_ev + zone.0 + 5.5 * zone.1);
            assert!(far.abs() < 0.02, "{name} leaked {far} EV far from its zone");
            for (other_name, other_zone, _) in &zones {
                if other_name == name || (zone.0 - other_zone.0).abs() < 3.0 {
                    continue;
                }
                let leak = tone_eq_offset_ev(s, mid_ev + other_zone.0);
                assert!(
                    leak.abs() < 0.15,
                    "{name} leaked {leak} EV into {other_name}'s centre"
                );
            }
        }
        // Neutral settings → identically zero.
        let t = build_scene_tone(&settings());
        assert!(!t.tone_eq_active);
        assert!(t.tone_eq.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn tone_eq_regional_gain_preserves_local_texture_ratio() {
        // Two pixels in one region (same regional E) get the SAME linear gain,
        // so their scene-linear ratio — the texture — is untouched.
        let s = DevelopSettings {
            shadows: 150.0,
            ..settings()
        };
        let tone = build_scene_tone(&s);
        let region_e = (0.02f32).log2();
        let a = tone.scene_to_display([0.018, 0.018, 0.018], Some(region_e));
        let b = tone.scene_to_display([0.026, 0.026, 0.026], Some(region_e));
        let neutral = build_scene_tone(&settings());
        let a0 = neutral.scene_to_display([0.018, 0.018, 0.018], Some(region_e));
        let b0 = neutral.scene_to_display([0.026, 0.026, 0.026], Some(region_e));
        assert!(a[1] > a0[1] + 0.02, "shadows lift must brighten the region");
        let sep0 = b0[1] - a0[1];
        let sep1 = b[1] - a[1];
        assert!(
            sep1 > sep0 * 0.9,
            "lift must not flatten texture: {sep0} -> {sep1}"
        );
    }

    #[test]
    fn gamut_clip_preserves_luma_and_contains_channels() {
        for c in [
            [1.4f32, 0.2, 0.1],
            [-0.2, 0.5, 0.3],
            [0.9, 1.2, -0.1],
            [0.5, 0.5, 0.5],
        ] {
            let out = gamut_clip_chroma(c);
            for v in out {
                assert!((0.0..=1.0).contains(&v), "channel {v} escaped");
            }
            let y_in = luma_lin(c[0], c[1], c[2]).clamp(0.0, 1.0);
            let y_out = luma_lin(out[0], out[1], out[2]);
            assert!((y_in - y_out).abs() < 1e-3, "luma drift {y_in} -> {y_out}");
        }
        // In-gamut input is untouched.
        let same = gamut_clip_chroma([0.2, 0.4, 0.6]);
        assert!((same[0] - 0.2).abs() < 1e-6 && (same[2] - 0.6).abs() < 1e-6);
    }

    #[test]
    fn cat16_wb_shifts_grey_but_keeps_its_brightness_through_the_chain() {
        let warm = DevelopSettings {
            temperature: 120.0,
            ..settings()
        };
        let t = build_scene_tone(&warm);
        let d = t.scene_to_display([0.1845, 0.1845, 0.1845], None);
        assert!(
            d[0] > d[2] + 0.01,
            "warm WB must tip grey toward amber: {d:?}"
        );
        let l = luminance_f32(d[0], d[1], d[2]);
        let neutral = build_scene_tone(&settings()).scene_to_display([0.1845; 3], None);
        let l0 = luminance_f32(neutral[0], neutral[1], neutral[2]);
        assert!(
            (l - l0).abs() < 0.02,
            "WB should not move brightness: {l0} -> {l}"
        );
    }

    #[test]
    fn default_look_anchors_mid_grey_and_keeps_headroom_shape() {
        let mut scene = SceneSource::new(3, 1);
        scene.set_rgb(0, 0, [SCENE_MID_GRAY; 3]);
        scene.set_rgb(1, 0, [1.0, 1.0, 1.0]);
        scene.set_rgb(2, 0, [0.01, 0.01, 0.01]);
        let out = render_default_look(&scene);
        // Mid grey → its gamma encoding (≈0.4855 in sRGB), the PTS-like anchor.
        let mid = out[0] as f32 / 65535.0;
        let expect = linear_to_srgb(SCENE_MID_GRAY);
        assert!(
            (mid - expect).abs() < 0.02,
            "default mid-grey {mid} should sit near {expect}"
        );
        // Scene 1.0 is bright but NOT clipped flat (the sigmoid shoulder).
        let white = out[4] as f32 / 65535.0;
        assert!(white > 0.85 && white < 1.0, "scene white rendered {white}");
        assert_eq!(out[3], u16::MAX, "alpha must be opaque");
    }

    #[test]
    fn neutral_commit_equals_default_render() {
        let mut scene = SceneSource::new(8, 8);
        for y in 0..8 {
            for x in 0..8 {
                let v = 0.02 + (x as f32 + y as f32 * 8.0) * 0.05;
                scene.set_rgb(x, y, [v, v * 0.8, v * 1.2]);
            }
        }
        let tiles = apply_scene_to_tilemap(&scene, &settings(), None);
        let direct = render_default_look(&scene);
        for y in 0..8u32 {
            for x in 0..8u32 {
                let (r16, g16, b16, _a) = tiles.get_pixel16(x, y);
                let i = ((y * 8 + x) * 4) as usize;
                assert_eq!(
                    (r16, g16, b16),
                    (direct[i], direct[i + 1], direct[i + 2]),
                    "neutral commit diverged at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn scene_commit_runs_legacy_color_stage_on_top() {
        let mut scene = SceneSource::new(4, 4);
        for y in 0..4 {
            for x in 0..4 {
                scene.set_rgb(x, y, [0.30, 0.08, 0.06]); // a saturated red patch
            }
        }
        let plain = apply_scene_to_tilemap(&scene, &settings(), None);
        let saturated = apply_scene_to_tilemap(
            &scene,
            &DevelopSettings {
                saturation: 150.0,
                ..settings()
            },
            None,
        );
        let (r0, g0, _b0, _) = plain.get_pixel16(1, 1);
        let (r1, g1, _b1, _) = saturated.get_pixel16(1, 1);
        let c0 = r0 as i32 - g0 as i32;
        let c1 = r1 as i32 - g1 as i32;
        assert!(c1 > c0, "saturation must widen chroma: {c0} -> {c1}");
    }

    #[test]
    fn region_e_proxy_tracks_exposure_shift_exactly() {
        let mut scene = SceneSource::new(16, 16);
        for y in 0..16 {
            for x in 0..16 {
                scene.set_rgb(x, y, [0.05, 0.05, 0.05]);
            }
        }
        let (base, pw, ph) = build_scene_region_base(&scene, 4);
        let t0 = build_scene_tone(&settings());
        let e0 = finish_region_e(&base, pw, ph, &t0, 4);
        let t1 = build_scene_tone(&DevelopSettings {
            exposure: 20.0, // +2 EV
            ..settings()
        });
        let e1 = finish_region_e(&base, pw, ph, &t1, 4);
        for (a, b) in e0.iter().zip(e1.iter()) {
            assert!(
                ((b - a) - 2.0).abs() < 0.05,
                "region E must move by exactly the exposure EV: {a} -> {b}"
            );
        }
    }

    #[test]
    fn wgsl_scene_chain_mirrors_cpu_constants() {
        // The GPU preview must run the identical scene chain. Pin the
        // load-bearing constants and entry points in the WGSL so an edit on
        // either side that forgets its twin fails here, not on screen.
        let shader = crate::gpu::compositor::COMPOSITOR_SHADER;
        assert_eq!(SCENE_EV_MIN, -14.0, "EV axis changed — update the WGSL");
        assert_eq!(SCENE_EV_MAX, 6.0, "EV axis changed — update the WGSL");
        for needle in [
            "fn dev_scene_display(",
            "fn dev_scene_lut(",
            "fn dev_tone_eq_at(",
            "fn dev_gamut_clip_chroma(",
            "fn dev_filmlike_clip(",
            "fn dev_compress_highlight_chroma(",
            "fn dev_restore_shadow_chroma(",
            "dev_smootherstep(0.02, 0.80, 1.0 - ratio)",
            "dev_smootherstep(0.55, 1.0, mapped_n)",
            "(ev + 14.0) / 20.0",
            "(e + 14.0) / 20.0",
            "6.1035156e-5",
            "dev_effects[16]",
            "dev_effects[25]",
            "dev_smootherstep(0.5, 1.0, mapped_n)",
            "let blend = 0.90 + (0.20 - 0.90) * hw",
            "dev_effects[26]",
            "dev_rgb_curve[769u + i0]",
            "u.adj_pad_c == 1u",
            "@group(0) @binding(2) var dev_scene_tex",
        ] {
            assert!(
                shader.contains(needle),
                "WGSL lost the scene mirror: {needle}"
            );
        }
    }

    #[test]
    fn histogram_follows_scene_settings() {
        let mut scene = SceneSource::new(64, 64);
        for y in 0..64 {
            for x in 0..64 {
                scene.set_rgb(x, y, [0.1845, 0.1845, 0.1845]);
            }
        }
        let proxy = build_scene_histogram_proxy(&scene);
        assert!(!proxy.is_empty());
        let h0 = histogram_rgbl_scene(&proxy, &settings(), BaseLook::Raw);
        let peak0 = h0[3]
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0;
        let h1 = histogram_rgbl_scene(
            &proxy,
            &DevelopSettings {
                exposure: 20.0,
                ..settings()
            },
            BaseLook::Raw,
        );
        let peak1 = h1[3]
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0;
        assert!(
            peak1 > peak0 + 20,
            "exposure must shift the luma peak right: {peak0} -> {peak1}"
        );
    }
}

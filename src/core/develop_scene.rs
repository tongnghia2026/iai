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
    apply_color, apply_color_linear, apply_detail_to_working_buffer, apply_effects_linear,
    apply_luma_target, apply_point_curve_outer, bell, contrast_curve, control_to_unit,
    curve_is_identity, curve_shadows_mask, darks_mask, eased_control, guided_lowpass_plane,
    linear_to_srgb, luma_lin, lut_lerp, rgb_curve_luts, sample_plane_bilinear, smootherstep,
    srgb_to_linear, DevelopSettings, EXPOSURE_LIMIT, TONE_DOWNSAMPLE, TONE_REGION_RADIUS,
};
use crate::core::tile::TileMap;
use rayon::prelude::*;
use std::sync::OnceLock;

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
    /// Robust black/grey/white anchors of the immutable RAW master. Computed
    /// lazily from a small grid and then reused by every preview/commit build.
    pub auto_tone: OnceLock<AutoToneRange>,
}

#[derive(Clone, Copy, Debug)]
pub struct AutoToneRange {
    pub black_ev: f32,
    pub grey_ev: f32,
    pub white_ev: f32,
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
            auto_tone: OnceLock::new(),
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
            auto_tone: OnceLock::new(),
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
pub fn sigmoid_params(_contrast: f32) -> SigmoidParams {
    // The shoulder is part of the base rendering transform, not the Contrast
    // control. Contrast is a separate pivoted luma curve after tone mapping.
    let c = SIGMOID_BASE_C;
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

/// Camera-preview baseline fit with chromaticity as well as brightness. The
/// embedded JPEG is the only available as-shot colour reference when no DCP is
/// installed. Fit three bounded linear gains through the real default tone
/// transform; damping prevents a camera picture style from becoming an extreme
/// correction. Returns at most +/-1 EV per channel.
pub fn baseline_rgb_gains(scene_rgb: &[[f32; 3]], target: [f32; 3]) -> [f32; 3] {
    if scene_rgb.is_empty() || target.iter().any(|v| !v.is_finite()) {
        return [1.0; 3];
    }
    let target = target.map(|v| v.clamp(0.01, 0.99));
    let tone = build_scene_tone(&DevelopSettings::default());
    let mut gain = [1.0f32; 3];
    for _ in 0..10 {
        let mut sum = [0.0f64; 3];
        for p in scene_rgb {
            let d = tone.scene_to_display([p[0] * gain[0], p[1] * gain[1], p[2] * gain[2]], None);
            for c in 0..3 {
                sum[c] += d[c] as f64;
            }
        }
        for c in 0..3 {
            let mean = (sum[c] / scene_rgb.len() as f64) as f32;
            // Encoded output responds sub-linearly to a linear gain. A damped
            // 1.4 exponent converges quickly without oscillating through gamut.
            let correction = (target[c] / mean.max(0.005)).powf(1.4);
            gain[c] = (gain[c] * correction).clamp(0.5, 2.0);
        }
    }
    gain
}

/// Estimate image-specific scene anchors on a bounded grid. Transparent and
/// non-positive samples are ignored; small percentile trims keep letterbox
/// black and isolated sensor highlights from defining the whole curve.
fn estimate_auto_tone(scene: &SceneSource) -> AutoToneRange {
    const BUDGET: u64 = 60_000;
    let total = scene.width as u64 * scene.height as u64;
    let step = (((total / BUDGET).max(1) as f64).sqrt().ceil() as u32).max(1);
    let mut evs = Vec::with_capacity((total / (step as u64 * step as u64) + 1) as usize);
    let mut y = 0;
    while y < scene.height {
        let mut x = 0;
        while x < scene.width {
            let i = y as usize * scene.width as usize + x as usize;
            if scene.alpha16_at_index(i) > 0 {
                let p = scene.get_rgb(x, y);
                let l = luma_lin(p[0], p[1], p[2]);
                if l.is_finite() && l > 1e-7 {
                    evs.push(l.log2());
                }
            }
            x += step;
        }
        y += step;
    }
    if evs.len() < 32 {
        return AutoToneRange {
            black_ev: SCENE_EV_MIN,
            grey_ev: SCENE_MID_GRAY.log2(),
            white_ev: SCENE_EV_MAX,
        };
    }
    evs.sort_unstable_by(f32::total_cmp);
    let at = |q: f32| evs[((evs.len() - 1) as f32 * q).round() as usize];
    // ART's vmin*0.5 / vmax*1.5 expressed in EV, with robust 0.2/99.8%
    // endpoints so a dead pixel or specular pinprick cannot collapse the DR.
    let black_ev = (at(0.002) - 1.0).clamp(SCENE_EV_MIN, SCENE_EV_MAX - 3.0);
    let white_ev = (at(0.998) + 1.5f32.log2()).clamp(black_ev + 3.0, SCENE_EV_MAX + 4.0);
    // Mean only through the adaptive central window, excluding the outer 10%
    // of the measured DR. Averaging in linear light matches sourceGray.
    let lo = black_ev + (white_ev - black_ev) * 0.10;
    let hi = white_ev - (white_ev - black_ev) * 0.10;
    let (sum, count) = evs
        .iter()
        .filter(|&&e| e >= lo && e <= hi)
        .fold((0.0f64, 0usize), |(sum, n), &e| {
            (sum + 2.0f64.powf(e as f64), n + 1)
        });
    let grey_ev = if count == 0 {
        (black_ev + white_ev) * 0.5
    } else {
        (sum / count as f64).log2() as f32
    }
    .clamp(black_ev + 1.0, white_ev - 1.0);
    AutoToneRange {
        black_ev,
        grey_ev,
        white_ev,
    }
}

pub fn scene_auto_tone(scene: &SceneSource) -> Option<AutoToneRange> {
    (scene.look == BaseLook::Raw)
        .then(|| *scene.auto_tone.get_or_init(|| estimate_auto_tone(scene)))
}

/// Conservative image-range warp. The camera-preview baseline has already set
/// overall brightness, so source grey must remain fixed. Black/white statistics
/// may only adjust the two slopes gently; unconstrained endpoint fitting can
/// turn a normal 5-stop portrait into a 20-stop curve with crushed blacks and
/// neon skin.
#[inline]
fn adaptive_map_ev(e: f32, a: AutoToneRange) -> f32 {
    let desired = if e <= a.grey_ev {
        (a.grey_ev - SCENE_EV_MIN) / (a.grey_ev - a.black_ev).max(1.0)
    } else {
        (SCENE_EV_MAX - a.grey_ev) / (a.white_ev - a.grey_ev).max(1.0)
    };
    // Half-blend a bounded 0.8..1.2 fit -> effective slope 0.9..1.1.
    let slope = 1.0 + (desired.clamp(0.8, 1.2) - 1.0) * 0.5;
    a.grey_ev + (e - a.grey_ev) * slope
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
    /// Zero-luminance RGB offsets for scene-linear split grading.
    pub grade_shadow: [f32; 3],
    pub grade_highlight: [f32; 3],
    /// Power of the post-tone, pivoted RAW contrast curve (1 = identity).
    pub scene_contrast_gamma: f32,
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

/// Per-image variant used by every real scene preview and bake.
pub fn build_scene_tone_for_scene(
    settings: &DevelopSettings,
    scene: &SceneSource,
) -> SceneToneData {
    build_scene_tone_impl(settings, scene.look, scene_auto_tone(scene))
}

/// Build the scene tone stage for a given base look. Both looks share the
/// whole chain structure (CAT16·2^EV matrix, EV-domain tone equalizer, the
/// log2-indexed tone LUT the CPU and GPU consume, display curves) — only the
/// LUT CONTENT and where Contrast lives differ, so the WGSL chain needs no
/// flag: it renders whatever tables it is given.
pub fn build_scene_tone_for(settings: &DevelopSettings, look: BaseLook) -> SceneToneData {
    build_scene_tone_impl(settings, look, None)
}

fn build_scene_tone_impl(
    settings: &DevelopSettings,
    look: BaseLook,
    auto: Option<AutoToneRange>,
) -> SceneToneData {
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
                let mapped_e = auto.map_or(e, |a| adaptive_map_ev(e, a));
                *slot = sigmoid_eval(&sigmoid, mapped_e.exp2());
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
        grade_shadow: grade_vector(settings.grade_shadow_hue, settings.grade_shadow_strength),
        grade_highlight: grade_vector(
            settings.grade_highlight_hue,
            settings.grade_highlight_strength,
        ),
        scene_contrast_gamma: if look == BaseLook::Raw {
            (0.75 * control_to_unit(settings.contrast)).exp2()
        } else {
            1.0
        },
        display: build_display_lut_for(settings, look),
        rgb: rgb_curve_luts(settings),
        sigmoid,
    }
}

/// Hue/strength control to a small, zero-luminance linear-RGB offset. Removing
/// the direction's Rec.709 luminance makes grading change colour contrast
/// without lifting or lowering the zone as a side effect.
fn grade_vector(hue_deg: f32, strength: f32) -> [f32; 3] {
    let h = hue_deg.rem_euclid(360.0) / 60.0;
    let x = 1.0 - ((h % 2.0) - 1.0).abs();
    let rgb = match h as i32 {
        0 => [1.0, x, 0.0],
        1 => [x, 1.0, 0.0],
        2 => [0.0, 1.0, x],
        3 => [0.0, x, 1.0],
        4 => [x, 0.0, 1.0],
        _ => [1.0, 0.0, x],
    };
    let y = luma_lin(rgb[0], rgb[1], rgb[2]);
    let mut d = [rgb[0] - y, rgb[1] - y, rgb[2] - y];
    let norm = d[0].abs().max(d[1].abs()).max(d[2].abs()).max(1e-6);
    let scale = control_to_unit(strength).max(0.0) * 0.12 / norm;
    for v in &mut d {
        *v *= scale;
    }
    d
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
    #[inline]
    fn apply_scene_contrast(&self, c: [f32; 3]) -> [f32; 3] {
        if (self.scene_contrast_gamma - 1.0).abs() < 1e-5 {
            return c;
        }
        let y = luma_lin(c[0], c[1], c[2]).clamp(0.0, 1.0);
        let p = SCENE_MID_GRAY;
        let target = if y <= p {
            p * (y / p).powf(self.scene_contrast_gamma)
        } else {
            1.0 - (1.0 - p) * ((1.0 - y) / (1.0 - p)).powf(self.scene_contrast_gamma)
        };
        let (mut r, mut g, mut b) = (c[0], c[1], c[2]);
        apply_luma_target(&mut r, &mut g, &mut b, target);
        [r, g, b]
    }

    #[inline]
    fn apply_scene_grade(&self, mut v: [f32; 3]) -> [f32; 3] {
        if self.grade_shadow == [0.0; 3] && self.grade_highlight == [0.0; 3] {
            return v;
        }
        let y = luma_lin(v[0], v[1], v[2]).max(0.0);
        let er = y.max(SCENE_EV_MIN.exp2()).log2() - SCENE_MID_GRAY.log2();
        let gaussian = |center: f32, width: f32| {
            let d = (er - center) / width;
            (-0.5 * d * d).exp()
        };
        let sw = gaussian(-2.0, 1.8);
        let hw = gaussian(2.0, 1.8);
        let amplitude = y.sqrt();
        for i in 0..3 {
            v[i] += amplitude * (self.grade_shadow[i] * sw + self.grade_highlight[i] * hw);
        }
        v
    }

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
    pub(crate) fn scene_to_working(&self, rgb: [f32; 3], region_e: Option<f32>) -> [f32; 3] {
        let mut v = cat16::mat_apply(&self.wb_ev, rgb);
        if self.tone_eq_active {
            let e = region_e.unwrap_or_else(|| self.own_e(v));
            let gain = self.tone_eq_at(e).exp2();
            v = [v[0] * gain, v[1] * gain, v[2] * gain];
        }
        v = self.apply_scene_grade(v);
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
        let out = self.apply_scene_contrast(out);
        let out = if self.shadow_chroma_active {
            restore_shadow_chroma(out)
        } else {
            out
        };
        out
    }

    /// Final output boundary: gamut-map and encode a linear working pixel once.
    pub(crate) fn working_to_display(&self, working: [f32; 3]) -> [f32; 3] {
        let clipped = gamut_clip_chroma(filmlike_clip(working));
        let mut r = linear_to_srgb(clipped[0]);
        let mut g = linear_to_srgb(clipped[1]);
        let mut b = linear_to_srgb(clipped[2]);
        self.apply_display_curves(&mut r, &mut g, &mut b);
        [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)]
    }

    /// Compatibility composition while downstream stages migrate to the
    /// unclamped working-space boundary above.
    pub fn scene_to_display(&self, rgb: [f32; 3], region_e: Option<f32>) -> [f32; 3] {
        self.working_to_display(self.scene_to_working(rgb, region_e))
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

#[allow(clippy::too_many_arguments)]
pub fn scene_fast_region_develop(
    base: &[[f32; 3]],
    tone: &SceneToneData,
    settings: &DevelopSettings,
    w: usize,
    h: usize,
    origin_x: u32,
    origin_y: u32,
    source_w: u32,
    source_h: u32,
    downsample: u32,
) -> (Vec<[f32; 3]>, Vec<[f32; 3]>) {
    let curves = crate::core::develop::build_mixer_curves_opt(settings);
    let mut working: Vec<[f32; 3]> = base
        .par_iter()
        .map(|p| {
            let [mut r, mut g, mut b] = tone.scene_to_working(*p, None);
            apply_color_linear(settings, curves.as_ref(), &mut r, &mut g, &mut b);
            [r, g, b]
        })
        .collect();
    let region: Vec<[f32; 3]> = working
        .par_iter()
        .map(|p| tone.working_to_display(*p))
        .collect();
    if !(settings.has_spatial_effects() || settings.vignette.abs() > 0.001) || working.is_empty() {
        return (region.clone(), region);
    }
    let luma: Vec<f32> = working
        .par_iter()
        .map(|p| luma_lin(p[0], p[1], p[2]).max(0.0))
        .collect();
    let step = downsample.max(1);
    let spatial_base = if settings.has_spatial_effects() {
        guided_lowpass_plane(
            &luma,
            w,
            h,
            (TONE_REGION_RADIUS / step as usize).max(1),
            0.05,
        )
    } else {
        luma
    };
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
    working.par_iter_mut().enumerate().for_each(|(i, p)| {
        let px = (i % w) as u32;
        let py = (i / w) as u32;
        let x = origin_x
            .saturating_add(px.saturating_mul(step))
            .saturating_add(step / 2)
            .min(source_w.saturating_sub(1));
        let y = origin_y
            .saturating_add(py.saturating_mul(step))
            .saturating_add(step / 2)
            .min(source_h.saturating_sub(1));
        let [r, g, b] = p;
        apply_effects_linear(settings, r, g, b, x, y, inv_w, inv_h, spatial_base[i]);
    });
    let adjusted = working
        .par_iter()
        .map(|p| tone.working_to_display(*p))
        .collect();
    (region, adjusted)
}

// ── Full renders ─────────────────────────────────────────────────────────────

/// Render the scene at the current settings into a display-referred RGBA16
/// (gamma sRGB) buffer — the Light half only (Colour/Effects/Detail/Locals run
/// on top via the legacy stages in [`apply_scene_to_tilemap`]).
fn apply_scene_locals_linear(
    working: &mut [[f32; 3]],
    width: usize,
    height: usize,
    settings: &DevelopSettings,
) {
    let locals: Vec<_> = settings
        .locals
        .iter()
        .filter(|local| !local.settings.is_neutral())
        .map(|local| {
            let s = local.settings.to_develop_settings();
            let tone = build_scene_tone_for(&s, BaseLook::Identity);
            (local.shape, s, tone)
        })
        .collect();
    if locals.is_empty() {
        return;
    }
    let inv_w = if width > 1 {
        1.0 / (width - 1) as f32
    } else {
        0.0
    };
    let inv_h = if height > 1 {
        1.0 / (height - 1) as f32
    } else {
        0.0
    };
    working.par_iter_mut().enumerate().for_each(|(i, pixel)| {
        let nx = (i % width) as f32 * inv_w;
        let ny = (i / width) as f32 * inv_h;
        for (shape, local, tone) in &locals {
            let weight = shape.weight(nx, ny);
            if weight <= 0.003 {
                continue;
            }
            let mut adjusted = tone.scene_to_working(*pixel, None);
            if local.contrast.abs() > 0.001 {
                let gamma = (0.75 * control_to_unit(local.contrast)).exp2();
                let y = luma_lin(adjusted[0], adjusted[1], adjusted[2]).clamp(0.0, 1.0);
                let target = if y <= SCENE_MID_GRAY {
                    SCENE_MID_GRAY * (y / SCENE_MID_GRAY).powf(gamma)
                } else {
                    1.0 - (1.0 - SCENE_MID_GRAY) * ((1.0 - y) / (1.0 - SCENE_MID_GRAY)).powf(gamma)
                };
                let [r, g, b] = &mut adjusted;
                apply_luma_target(r, g, b, target);
            }
            let [r, g, b] = &mut adjusted;
            apply_color_linear(local, None, r, g, b);
            for channel in 0..3 {
                pixel[channel] += (adjusted[channel] - pixel[channel]) * weight;
            }
        }
    });
}

fn render_scene_display_inner(
    scene: &SceneSource,
    tone: &SceneToneData,
    develop: Option<(&DevelopSettings, Option<&crate::core::develop::MixerCurves>)>,
) -> Vec<u16> {
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
    let mut working = vec![[0.0f32; 3]; w * h];
    working.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let fy = (y as f32 + 0.5) / s - 0.5;
        for (x, slot) in row.iter_mut().enumerate() {
            let i = (y * w + x) * 4;
            let rgb = [
                f16_bits_to_f32(scene.half[i]),
                f16_bits_to_f32(scene.half[i + 1]),
                f16_bits_to_f32(scene.half[i + 2]),
            ];
            let e = region.as_ref().map(|(plane, pw, ph)| {
                sample_plane_bilinear(plane, *pw, *ph, (x as f32 + 0.5) / s - 0.5, fy)
            });
            let [mut r, mut g, mut b] = tone.scene_to_working(rgb, e);
            if let Some((settings, curves)) = develop {
                apply_color_linear(settings, curves, &mut r, &mut g, &mut b);
            }
            *slot = [r, g, b];
        }
    });

    if let Some((settings, _)) = develop
        .filter(|(settings, _)| settings.has_spatial_effects() || settings.vignette.abs() > 0.001)
    {
        let luma: Vec<f32> = working
            .par_iter()
            .map(|p| luma_lin(p[0], p[1], p[2]).max(0.0))
            .collect();
        let base = if settings.has_spatial_effects() {
            guided_lowpass_plane(&luma, w, h, TONE_REGION_RADIUS, 0.05)
        } else {
            luma
        };
        let inv_w = if w > 1 { 1.0 / (w - 1) as f32 } else { 0.0 };
        let inv_h = if h > 1 { 1.0 / (h - 1) as f32 } else { 0.0 };
        working.par_iter_mut().enumerate().for_each(|(i, p)| {
            let [r, g, b] = p;
            apply_effects_linear(
                settings,
                r,
                g,
                b,
                (i % w) as u32,
                (i / w) as u32,
                inv_w,
                inv_h,
                base[i],
            );
        });
    }

    if let Some((settings, _)) = develop.filter(|(settings, _)| settings.has_detail()) {
        apply_detail_to_working_buffer(&mut working, w, h, settings);
    }

    if let Some((settings, _)) = develop.filter(|(settings, _)| settings.has_locals()) {
        apply_scene_locals_linear(&mut working, w, h, settings);
    }

    let mut out = vec![0u16; w * h * 4];
    out.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let d = tone.working_to_display(working[y * w + x]);
            let o = x * 4;
            row[o] = (d[0] * 65535.0 + 0.5) as u16;
            row[o + 1] = (d[1] * 65535.0 + 0.5) as u16;
            row[o + 2] = (d[2] * 65535.0 + 0.5) as u16;
            row[o + 3] = scene.alpha16_at_index(y * w + x);
        }
    });
    out
}

fn render_scene_display(scene: &SceneSource, tone: &SceneToneData) -> Vec<u16> {
    render_scene_display_inner(scene, tone, None)
}

/// The default (neutral-settings) render — what the RAW decode bakes into the
/// document tiles, so opening the Develop panel at neutral shows exactly the
/// pixels already on screen (zero-work preview, no pop) and the "Open Image"
/// neutral shortcut is exact.
pub fn render_default_look(scene: &SceneSource) -> Vec<u16> {
    render_scene_display(
        scene,
        &build_scene_tone_for_scene(&DevelopSettings::default(), scene),
    )
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
    let tone = build_scene_tone_for_scene(settings, scene);
    let linear_develop = scene.look == BaseLook::Raw
        && (settings.has_color()
            || settings.has_spatial_effects()
            || settings.has_detail()
            || settings.has_locals()
            || settings.vignette.abs() > 0.001);
    let curves = crate::core::develop::build_mixer_curves_opt(settings);
    let px16 = render_scene_display_inner(
        scene,
        &tone,
        linear_develop.then_some((settings, curves.as_ref())),
    );
    let display = TileMap::from_rgba16(&px16, scene.width, scene.height);
    drop(px16);
    let mut rest = strip_scene_handled(settings);
    if linear_develop {
        rest.saturation = 0.0;
        rest.vibrance = 0.0;
        rest.mixer_hue = [0.0; crate::core::develop::MIXER_BANDS];
        rest.mixer_saturation = [0.0; crate::core::develop::MIXER_BANDS];
        rest.mixer_luminance = [0.0; crate::core::develop::MIXER_BANDS];
        rest.texture = 0.0;
        rest.clarity = 0.0;
        rest.dehaze = 0.0;
        rest.vignette = 0.0;
        rest.sharpening = 0.0;
        rest.noise_reduction = 0.0;
        rest.color_noise_reduction = 0.0;
        rest.locals.clear();
    }
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
    fn baseline_rgb_fit_matches_preview_colour_without_extreme_gains() {
        let samples = vec![[0.12f32, 0.09, 0.07]; 256];
        let target = [0.52, 0.48, 0.46];
        let gain = baseline_rgb_gains(&samples, target);
        assert!(gain.iter().all(|g| (0.5..=2.0).contains(g)));
        let tone = build_scene_tone(&settings());
        let before = tone.scene_to_display(samples[0], None);
        let out = tone.scene_to_display(
            [
                samples[0][0] * gain[0],
                samples[0][1] * gain[1],
                samples[0][2] * gain[2],
            ],
            None,
        );
        let error = |rgb: [f32; 3]| {
            (rgb[0] - target[0]).abs() + (rgb[1] - target[1]).abs() + (rgb[2] - target[2]).abs()
        };
        assert!(
            error(out) < error(before) * 0.35,
            "preview colour fit did not improve enough: {before:?} -> {out:?}"
        );
    }

    #[test]
    fn auto_tone_gently_shapes_range_without_reexposing_grey() {
        let mut scene = SceneSource::new(256, 1);
        for x in 0..256 {
            let ev = -9.0 + 11.0 * x as f32 / 255.0;
            let v = ev.exp2();
            scene.set_rgb(x, 0, [v, v, v]);
        }
        let anchors = scene_auto_tone(&scene).expect("RAW has automatic anchors");
        assert!(anchors.black_ev < anchors.grey_ev);
        assert!(anchors.grey_ev < anchors.white_ev);

        assert_eq!(adaptive_map_ev(anchors.grey_ev, anchors), anchors.grey_ev);
        for e in [
            anchors.black_ev,
            anchors.grey_ev - 2.0,
            anchors.grey_ev + 2.0,
            anchors.white_ev,
        ] {
            let mapped = adaptive_map_ev(e, anchors);
            let distance = (e - anchors.grey_ev).abs();
            assert!(
                (mapped - e).abs() <= distance * 0.101 + 1e-5,
                "adaptive warp moved {e} too far: {mapped}"
            );
        }

        let tone = build_scene_tone_for_scene(&settings(), &scene);
        let fixed = build_scene_tone(&settings());
        let grey = anchors.grey_ev.exp2();
        let grey_error = (tone.tone_map(grey) - fixed.tone_map(grey)).abs();
        assert!(
            grey_error < 0.005,
            "grey LUT interpolation drifted by {grey_error}"
        );
        assert!(tone.lut.windows(2).all(|w| w[1] >= w[0]));
    }

    #[test]
    fn auto_tone_is_disabled_for_identity_sources() {
        let tiles = TileMap::new(2, 2);
        let scene = SceneSource::from_display_tiles(&tiles);
        assert!(scene_auto_tone(&scene).is_none());
        let adaptive = build_scene_tone_for_scene(&settings(), &scene);
        let fixed = build_scene_tone_for(&settings(), BaseLook::Identity);
        assert_eq!(adaptive.lut, fixed.lut);
    }

    #[test]
    fn auto_tone_preserves_raw_baseline_exposure() {
        let mut normal = SceneSource::new(128, 1);
        let mut lifted = SceneSource::new(128, 1);
        for x in 0..128 {
            let v = (-7.0 + 7.0 * x as f32 / 127.0).exp2();
            normal.set_rgb(x, 0, [v; 3]);
            lifted.set_rgb(x, 0, [v * 2.0; 3]);
        }
        let tn = build_scene_tone_for_scene(&settings(), &normal);
        let tl = build_scene_tone_for_scene(&settings(), &lifted);
        let a = tn.scene_to_display(normal.get_rgb(64, 0), None)[0];
        let b = tl.scene_to_display(lifted.get_rgb(64, 0), None)[0];
        assert!(b > a + 0.08, "baseline exposure was cancelled: {a} -> {b}");
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
    fn split_grade_targets_opposite_ev_zones_without_moving_luma() {
        let s = DevelopSettings {
            grade_shadow_hue: 240.0,
            grade_shadow_strength: 200.0,
            grade_highlight_hue: 30.0,
            grade_highlight_strength: 200.0,
            ..settings()
        };
        let tone = build_scene_tone(&s);
        let shadow_y = SCENE_MID_GRAY / 4.0;
        let highlight_y = SCENE_MID_GRAY * 4.0;
        let shadow = tone.apply_scene_grade([shadow_y; 3]);
        let highlight = tone.apply_scene_grade([highlight_y; 3]);
        assert!(shadow[2] > shadow[0], "shadow not cooled: {shadow:?}");
        assert!(
            highlight[0] > highlight[2],
            "highlight not warmed: {highlight:?}"
        );
        for (before, after) in [(shadow_y, shadow), (highlight_y, highlight)] {
            assert!((luma_lin(after[0], after[1], after[2]) - before).abs() < 1e-6);
        }
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
    fn contrast_is_pivoted_after_a_fixed_sigmoid_shoulder() {
        let low = build_scene_tone(&DevelopSettings {
            contrast: -200.0,
            ..settings()
        });
        let neutral = build_scene_tone(&settings());
        let high = build_scene_tone(&DevelopSettings {
            contrast: 200.0,
            ..settings()
        });
        assert!((low.sigmoid.c - neutral.sigmoid.c).abs() < 1e-7);
        assert!((high.sigmoid.c - neutral.sigmoid.c).abs() < 1e-7);

        let pivot = [SCENE_MID_GRAY; 3];
        let fixed = high.apply_scene_contrast(pivot);
        assert!((fixed[0] - SCENE_MID_GRAY).abs() < 1e-6);
        assert!(high.apply_scene_contrast([0.08; 3])[0] < 0.08);
        assert!(high.apply_scene_contrast([0.70; 3])[0] > 0.70);
        let h0 = high.apply_scene_contrast([0.85; 3])[0];
        let h1 = high.apply_scene_contrast([0.90; 3])[0];
        assert!(h1 > h0, "highlight gradation flattened: {h0} -> {h1}");
        assert_eq!(neutral.apply_scene_contrast([0.37; 3]), [0.37; 3]);
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
    fn raw_texture_is_a_linear_noop_on_a_flat_field() {
        let mut scene = SceneSource::new(12, 12);
        for y in 0..12 {
            for x in 0..12 {
                scene.set_rgb(x, y, [0.32, 0.12, 0.06]);
            }
        }
        let plain = apply_scene_to_tilemap(&scene, &settings(), None);
        let textured = apply_scene_to_tilemap(
            &scene,
            &DevelopSettings {
                texture: 200.0,
                ..settings()
            },
            None,
        );
        assert_eq!(
            plain.get_pixel16(6, 6),
            textured.get_pixel16(6, 6),
            "linear texture must not move a spatially flat colour"
        );
    }

    #[test]
    fn raw_vignette_and_color_compose_before_the_single_output_boundary() {
        let mut scene = SceneSource::new(16, 16);
        for y in 0..16 {
            for x in 0..16 {
                scene.set_rgb(x, y, [0.28, 0.08, 0.04]);
            }
        }
        let out = apply_scene_to_tilemap(
            &scene,
            &DevelopSettings {
                saturation: 120.0,
                vignette: 100.0,
                ..settings()
            },
            None,
        );
        let corner = out.get_pixel16(0, 0);
        let centre = out.get_pixel16(8, 8);
        assert!(centre.0 > corner.0, "vignette must darken the RAW corner");
        assert!(
            centre.0.saturating_sub(centre.1) > centre.1.saturating_sub(centre.2),
            "linear saturation must remain present under Effects"
        );
    }

    #[test]
    fn raw_sharpening_amplifies_linear_detail_before_encoding() {
        let mut scene = SceneSource::new(24, 24);
        for y in 0..24 {
            for x in 0..24 {
                let ripple = if (x + y) % 2 == 0 { -0.012 } else { 0.012 };
                let v = 0.18 + ripple;
                scene.set_rgb(x, y, [v, v * 0.72, v * 0.48]);
            }
        }
        let plain = apply_scene_to_tilemap(&scene, &settings(), None);
        let sharp = apply_scene_to_tilemap(
            &scene,
            &DevelopSettings {
                sharpening: 100.0,
                sharpen_detail: 100.0,
                ..settings()
            },
            None,
        );
        let spread = |tm: &TileMap| {
            let a = tm.get_pixel16(10, 10).0 as i32;
            let b = tm.get_pixel16(11, 10).0 as i32;
            (a - b).abs()
        };
        assert!(
            spread(&sharp) > spread(&plain),
            "linear sharpening must widen fine RAW detail: {} -> {}",
            spread(&plain),
            spread(&sharp)
        );
    }

    #[test]
    fn raw_local_mask_blends_linear_adjustment_before_encoding() {
        let mut scene = SceneSource::new(20, 20);
        for y in 0..20 {
            for x in 0..20 {
                scene.set_rgb(x, y, [0.12, 0.07, 0.04]);
            }
        }
        let mut local = settings();
        local.locals.push(crate::core::develop::LocalAdjustment {
            shape: crate::core::develop::LocalMaskShape::Radial {
                cx: 0.5,
                cy: 0.5,
                rx: 0.28,
                ry: 0.28,
                feather: 0.35,
                invert: false,
            },
            settings: crate::core::develop::LocalSettings {
                exposure: 20.0,
                saturation: 80.0,
                ..Default::default()
            },
        });
        let plain = apply_scene_to_tilemap(&scene, &settings(), None);
        let masked = apply_scene_to_tilemap(&scene, &local, None);
        let p_center = plain.get_pixel16(10, 10);
        let m_center = masked.get_pixel16(10, 10);
        let p_corner = plain.get_pixel16(0, 0);
        let m_corner = masked.get_pixel16(0, 0);
        assert!(
            m_center.0 > p_center.0,
            "local exposure must lift its centre"
        );
        assert_eq!(m_corner, p_corner, "outside a local mask must remain exact");
        assert!(
            m_center.0.saturating_sub(m_center.1) > p_center.0.saturating_sub(p_center.1),
            "local saturation must compose in the same linear adjustment"
        );
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
            "dev_effects[27]",
            "dev_effects[32]",
            "let sw = exp(-0.5 * sd * sd)",
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

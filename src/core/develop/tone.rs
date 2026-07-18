//! Tone/light math: perceptual luminance masks (blacks / shadows / highlights /
//! whites), the light-zone luma mapping, the contrast S-curve, and the
//! luma-target chroma-preserving apply. Also hosts the shared smoothstep / bell /
//! range easing helpers the masks are built from (re-exported for other stages).

use super::*;
use crate::core::color::luminance_f32;

pub(crate) fn smootherstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

pub(crate) fn bell(x: f32, center: f32, radius: f32) -> f32 {
    let t = (1.0 - ((x - center).abs() / radius)).clamp(0.0, 1.0);
    smootherstep(0.0, 1.0, t)
}

fn range_mask(x: f32, low: f32, low_full: f32, high_full: f32, high: f32) -> f32 {
    smootherstep(low, low_full, x) * (1.0 - smootherstep(high_full, high, x))
}

fn tone_response(amount: f32) -> f32 {
    amount.signum() * amount.abs().clamp(0.0, 1.0).powf(0.72)
}

fn black_floor_gate(luma: f32) -> f32 {
    smootherstep(0.004, 0.055, luma)
}

pub(crate) fn black_mask(luma: f32) -> f32 {
    1.0 - smootherstep(0.18, 0.46, luma)
}

pub(crate) fn shadow_mask(luma: f32) -> f32 {
    range_mask(luma, 0.010, 0.080, 0.42, 0.70)
}

pub(crate) fn highlight_mask(luma: f32) -> f32 {
    smootherstep(0.42, 0.86, luma)
}

pub(crate) fn white_mask(luma: f32) -> f32 {
    smootherstep(0.72, 0.98, luma)
}

pub(crate) fn apply_light_luma(
    luma: f32,
    highlights: f32,
    shadows: f32,
    whites: f32,
    blacks: f32,
) -> f32 {
    let l = luma.clamp(0.0, 1.0);
    let hi = tone_response(highlights);
    let sh = tone_response(shadows);
    let wh = tone_response(whites);
    let bl = tone_response(blacks);
    let mut d = 0.0;

    if hi >= 0.0 {
        d += hi * 0.20 * highlight_mask(l) * (1.0 - l).powf(0.65);
    } else {
        d -= (-hi) * 0.30 * highlight_mask(l) * l.powf(0.70);
    }

    if sh >= 0.0 {
        d += sh * 0.28 * shadow_mask(l) * black_floor_gate(l) * (1.0 - l).powf(0.55);
    } else {
        d -= (-sh) * 0.28 * shadow_mask(l) * l.powf(0.72);
    }

    if wh >= 0.0 {
        d += wh * 0.15 * white_mask(l) * (1.0 - l).powf(0.75);
    } else {
        d -= (-wh) * 0.20 * white_mask(l) * l.powf(0.65);
    }

    if bl >= 0.0 {
        d += bl * 0.12 * black_mask(l) * (1.0 - smootherstep(0.28, 0.58, l));
    } else {
        d -= (-bl) * 0.45 * black_mask(l) * l.powf(0.80);
    }

    (l + d).clamp(0.0, 1.0)
}

/// Gentle filmic shoulder above a knee so a pushed exposure compresses into
/// [0,1] rather than clipping flat. Below the knee it is the identity, and it is
/// C1-continuous at the knee (slope 1) so there is no visible seam. The
/// reciprocal curve `KNEE + (1−KNEE)·t/(1+t)` maps x=1 → 1 exactly and
/// converges to 1 as x → ∞, compressing over-exposed values smoothly.
pub(crate) fn highlight_rolloff(x: f32) -> f32 {
    const KNEE: f32 = 0.75;
    if x <= KNEE {
        x
    } else {
        let t = (x - KNEE) / (1.0 - KNEE);
        KNEE + (1.0 - KNEE) * t / (1.0 + t)
    }
}

pub(crate) fn darks_mask(luma: f32) -> f32 {
    bell(luma, 0.36, 0.30) * smootherstep(0.075, 0.18, luma)
}

pub(crate) fn curve_shadows_mask(luma: f32) -> f32 {
    range_mask(luma, 0.025, 0.12, 0.30, 0.54)
}

/// Sample a 256-entry LUT at normalized `t` in [0,1] with linear interpolation
/// between the bracketing entries (continuous output for high-bit-depth input).
pub(crate) fn lut_lerp(lut: &[f32; 256], t: f32) -> f32 {
    let p = t.clamp(0.0, 1.0) * 255.0;
    let i = p.floor() as usize;
    let f = p - i as f32;
    let a = lut[i.min(255)];
    let b = lut[(i + 1).min(255)];
    a + (b - a) * f
}

pub(crate) fn luma_target_chroma_preserve_weight(luma: f32, chroma: f32, target_luma: f32) -> f32 {
    let signal = smootherstep(0.018, 0.075, luma);
    let color = smootherstep(0.030, 0.105, chroma);
    let highlight_room = 1.0 - smootherstep(0.88, 0.98, target_luma);
    signal * color * highlight_room * 0.88
}

pub(crate) fn apply_luma_target(r: &mut f32, g: &mut f32, b: &mut f32, target_luma: f32) {
    let luma = luminance_f32(*r, *g, *b).clamp(0.0, 1.0);
    let target = target_luma.clamp(0.0, 1.0);
    let lift = target - luma;
    let chroma = rgb_chroma(*r, *g, *b);
    let chroma_gate = smootherstep(0.04, 0.22, chroma);
    let tone_gate = smootherstep(0.025, 0.16, luma) * (1.0 - smootherstep(0.86, 0.98, target));
    let chroma_factor = if lift > 0.0 {
        1.0 + lift * 1.20 * chroma_gate * tone_gate
    } else {
        1.0
    };
    let ar = target + (*r - luma) * chroma_factor;
    let ag = target + (*g - luma) * chroma_factor;
    let ab = target + (*b - luma) * chroma_factor;

    // Meaningfully coloured shadows should brighten as the same colour, not as
    // grey with a small old chroma offset. Blend into an RGB-ratio reconstruction
    // only when chroma and signal are stable; neutrals and crushed blacks keep the
    // additive/neutral behaviour and never invent colour.
    let preserve = luma_target_chroma_preserve_weight(luma, chroma, target);
    let scale = if luma > 1e-5 { target / luma } else { 0.0 };
    let sr = *r * scale;
    let sg = *g * scale;
    let sb = *b * scale;

    let nr = ar + (sr - ar) * preserve;
    let ng = ag + (sg - ag) * preserve;
    let nb = ab + (sb - ab) * preserve;
    set_rgb_preserving_luma(r, g, b, nr, ng, nb, target);
}

pub(crate) fn apply_tone_delta(r: &mut f32, g: &mut f32, b: &mut f32, delta: f32) {
    let luma = luminance_f32(*r, *g, *b).clamp(0.0, 1.0);
    let delta = delta.clamp(-0.85, 0.85);
    if delta >= 0.0 {
        let amount = delta * (1.0 - luma).powf(0.45);
        *r = shift_channel(*r, amount);
        *g = shift_channel(*g, amount);
        *b = shift_channel(*b, amount);
    } else {
        let amount = -delta * luma.powf(0.45);
        *r = shift_channel(*r, -amount);
        *g = shift_channel(*g, -amount);
        *b = shift_channel(*b, -amount);
    }
}

/// Contrast strength at `amount` = ±1. Moderate so even full strength stays a
/// gentle S (a punchy but non-clipping curve), not a hard clip.
const CONTRAST_K: f32 = 3.0;

/// Monotonic contrast curve pivoting at mid-grey. `amount` in [-1,1]: positive
/// steepens the midtones (more contrast), negative flattens them (less). It is a
/// normalised `tanh` sigmoid through (0,0), (0.5,0.5), (1,1) — strictly increasing,
/// only ever reaching 0/1 at the exact endpoints — so it can NEVER crush a band of
/// dark (or light) tones to a single value. The old polynomial `x + mid*zone*amount`
/// overshot below 0 and clamped: at max contrast every input under ~0.21 luma
/// mapped to pure black, wiping out all shadow detail (colour and brightness).
/// Reducing contrast uses the exact inverse of the same sigmoid (spreads midtones),
/// because a normalised tanh is symmetric in the sign of `amount` and would
/// otherwise *increase* contrast on both sides.
pub(crate) fn contrast_curve(x: f32, amount: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    let a = amount.clamp(-1.0, 1.0);
    if a.abs() < 1e-4 {
        return x;
    }
    let k = a.abs() * CONTRAST_K;
    let d = (k * 0.5).tanh();
    let out = if a > 0.0 {
        0.5 + (k * (x - 0.5)).tanh() / (2.0 * d)
    } else {
        0.5 + ((2.0 * x - 1.0) * d).atanh() / k
    };
    out.clamp(0.0, 1.0)
}

pub(crate) fn apply_soft_contrast(
    r: &mut f32,
    g: &mut f32,
    b: &mut f32,
    amount: f32,
    strength: f32,
) {
    let amount = (amount * strength).clamp(-0.88, 0.96);
    if amount.abs() <= 0.0001 {
        return;
    }
    let luma = luminance_f32(*r, *g, *b).clamp(0.0, 1.0);
    let target_luma = contrast_curve(luma, amount);
    let chroma_factor = (1.0 + amount * 0.34).clamp(0.66, 1.34);

    *r = (luma + (*r - luma) * chroma_factor).clamp(0.0, 1.0);
    *g = (luma + (*g - luma) * chroma_factor).clamp(0.0, 1.0);
    *b = (luma + (*b - luma) * chroma_factor).clamp(0.0, 1.0);
    apply_luma_target(r, g, b, target_luma);
}

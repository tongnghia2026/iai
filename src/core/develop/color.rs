//! Per-pixel colour stage: applies global saturation / vibrance and the
//! colour-mixer band edits (hue rotation, saturation, ratio-preserving
//! brightness) to a single pixel, preserving luminance where required.

use super::*;
use crate::core::color::luminance_f32;

/// Colour stage for one pixel. `curves` are the interpolated mixer curves for
/// THESE settings (`build_mixer_curves_opt`) — callers build them once per
/// batch outside the pixel loop; `None` skips the mixer.
pub(crate) fn apply_color(
    settings: &DevelopSettings,
    curves: Option<&MixerCurves>,
    r: &mut f32,
    g: &mut f32,
    b: &mut f32,
) {
    if settings.saturation.abs() <= 0.001 && settings.vibrance.abs() <= 0.001 && curves.is_none() {
        return;
    }
    let (mut rl, mut gl, mut bl) = (srgb_to_linear(*r), srgb_to_linear(*g), srgb_to_linear(*b));
    apply_color_linear(settings, curves, &mut rl, &mut gl, &mut bl);
    *r = linear_to_srgb(rl).clamp(0.0, 1.0);
    *g = linear_to_srgb(gl).clamp(0.0, 1.0);
    *b = linear_to_srgb(bl).clamp(0.0, 1.0);
}

/// Linear working-space colour stage. Classification still samples a bounded
/// display encoding (the existing UCS/HSV slider semantics), while every actual
/// correction stays linear and unclamped. This is the RAW path's #10 building
/// block; [`apply_color`] remains the bit-compatible Identity/PTS wrapper.
pub(crate) fn apply_color_linear(
    settings: &DevelopSettings,
    curves: Option<&MixerCurves>,
    r: &mut f32,
    g: &mut f32,
    b: &mut f32,
) {
    let has_global_saturation =
        settings.saturation.abs() > 0.001 || settings.vibrance.abs() > 0.001;
    if !has_global_saturation && curves.is_none() {
        return;
    }

    let (sr, sg, sb) = (
        linear_to_srgb(*r).clamp(0.0, 1.0),
        linear_to_srgb(*g).clamp(0.0, 1.0),
        linear_to_srgb(*b).clamp(0.0, 1.0),
    );
    let luma = luminance_f32(sr, sg, sb).clamp(0.0, 1.0);
    let (mixer_hue, mixer_sat, mixer_lum) = match curves {
        Some(c) => mixer_adjustments_for_color(c, sr, sg, sb, luma),
        None => (0.0, 0.0, 0.0),
    };

    if mixer_hue.abs() > 0.001 {
        // Rotate hue in Oklab — the SAME perceptual space band membership is
        // computed in — so the shift is even and correctly-directed (the old HSL
        // rotation shifted in a different space than the selection). `mixer_hue`
        // already carries the single chroma/luma gate, and the rotation is a
        // no-op on near-neutral pixels, so no extra guard/blend is needed.
        let shift_deg = eased_control(mixer_hue) * MIXER_HUE_SHIFT_MAX_DEG;
        let (nr, ng, nb) = crate::core::color::rotate_oklab_hue_linear(*r, *g, *b, shift_deg);
        *r = nr;
        *g = ng;
        *b = nb;
    }

    let global_sat_delta = eased_control(settings.saturation);
    let mixer_sat_delta = eased_control(mixer_sat);
    let vibrance = eased_control(settings.vibrance);
    // Low-chroma priority (colour-balance-rgb style): vibrance pours into pale,
    // muted colours and leaves already-vivid ones (chroma ≥ 0.35) untouched —
    // the old HSL-saturation shaping still fed vivid colours a residual boost,
    // which is Saturation's job, not Vividness's.
    let vib_w = 1.0 - smootherstep(0.10, 0.35, rgb_chroma(*r, *g, *b));
    let vib_delta = vibrance * vib_w * 0.88;
    let factor = saturation_factor(global_sat_delta, mixer_sat_delta, vib_delta);
    if (factor - 1.0).abs() > 0.001 {
        scale_linear_chroma_around_luma(r, g, b, factor);
    }

    let lum_delta = eased_control(mixer_lum);
    if lum_delta.abs() > 0.001 {
        apply_mixer_brightness(r, g, b, lum_delta);
    }
}

/// Mixer Luminance = a RATIO-PRESERVING brightness gain (an HSB-style
/// brightness correction): scale the pixel's RGB by one factor so its hue AND
/// saturation are unchanged — brightening orange gives a brighter ORANGE, not a
/// white wash. Only near the gamut ceiling does the brightest channel roll off
/// (a filmic shoulder), so the colour survives as far as the gamut allows
/// before it must compress; it never falls back to an additive lift toward
/// white (the old `apply_luma_target` desaturated once the target passed ~0.88).
fn apply_mixer_brightness(r: &mut f32, g: &mut f32, b: &mut f32, lum_delta: f32) {
    let luma = luma_lin(*r, *g, *b).clamp(0.0, 1.0);
    // Leave less headroom the closer a pixel already is to the ceiling (bright
    // pixels move less) / the floor (dark pixels darken less), matching the old
    // response envelope so the slider feel is unchanged.
    let tonal_room = if lum_delta >= 0.0 {
        (1.0 - luma).powf(0.42)
    } else {
        luma.powf(0.42)
    };
    let gain = (1.0 + lum_delta * 0.62 * tonal_room).max(0.0);
    if (gain - 1.0).abs() < 1e-4 {
        return;
    }
    let mx = r.max(*g).max(*b);
    if mx < 1e-6 {
        return;
    }
    let want = mx * gain;
    // Ratio-preserving scale. A SOFT shoulder only very near the ceiling
    // (BRIGHT_KNEE 0.90) rolls an over-gamut brightening toward 1.0, and
    // brightening is clamped to never go BELOW the original max — so a bright
    // highlight still LIGHTENS. The old exposure knee at 0.75 compressed the
    // whole [0.75, 1] range and pulled highlights below their own value, which
    // greyed them (blocky, because it happened in the low-res colour proxy).
    // Darkening (gain < 1, want < mx) is an exact linear scale-down.
    const BRIGHT_KNEE: f32 = 0.90;
    let new_mx = if gain <= 1.0 || want <= BRIGHT_KNEE {
        want
    } else {
        let t = (want - BRIGHT_KNEE) / (1.0 - BRIGHT_KNEE);
        (BRIGHT_KNEE + (1.0 - BRIGHT_KNEE) * t / (1.0 + t)).max(mx)
    };
    let s = new_mx / mx;
    *r *= s;
    *g *= s;
    *b *= s;
}

/// ART-style saturation in working linear RGB: `Y + sat * (RGB - Y)`.
/// The positive branch keeps the existing smooth gamut knee, but its anchor is
/// true linear Rec.709 luminance. Protection thresholds are the linear-light
/// equivalents of the old display-domain masks, preserving slider feel.
fn scale_linear_chroma_around_luma(r: &mut f32, g: &mut f32, b: &mut f32, factor: f32) {
    let y = luma_lin(*r, *g, *b).clamp(0.0, 1.0);
    let protect = smootherstep(0.0027, 0.0174, y) * (1.0 - smootherstep(0.7874, 0.9774, y));
    let req = (factor.clamp(0.0, 3.20) - 1.0) * protect;
    let d = [*r - y, *g - y, *b - y];
    let mut room = f32::INFINITY;
    for dc in d {
        if dc > 1e-6 {
            room = room.min((1.0 - y) / dc - 1.0);
        } else if dc < -1e-6 {
            room = room.min(y / -dc - 1.0);
        }
    }
    let scale = if req > 0.0 {
        let room = if room.is_finite() { room.max(0.0) } else { 0.0 };
        if room > 1e-4 {
            1.0 + room * (req / room).tanh()
        } else {
            1.0
        }
    } else {
        1.0 + req
    };
    *r = y + d[0] * scale;
    *g = y + d[1] * scale;
    *b = y + d[2] * scale;
}

fn saturation_factor(global_sat_delta: f32, mixer_sat_delta: f32, vib_delta: f32) -> f32 {
    let color_delta = (global_sat_delta + mixer_sat_delta + vib_delta).clamp(-1.0, 1.0);
    if color_delta.abs() <= 0.001 {
        return 1.0;
    }
    if color_delta < 0.0 {
        return 1.0 + color_delta * SAT_NEGATIVE_SCALE;
    }

    let positive_total = global_sat_delta.max(0.0) + mixer_sat_delta.max(0.0) + vib_delta.max(0.0);
    let mixer_share = if positive_total > 1e-6 {
        (mixer_sat_delta.max(0.0) / positive_total).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let slope = SAT_POSITIVE_SCALE + mixer_share * (MIXER_SAT_POSITIVE_SCALE - SAT_POSITIVE_SCALE);
    1.0 + color_delta * slope
}

pub(crate) fn set_rgb_preserving_luma(
    r: &mut f32,
    g: &mut f32,
    b: &mut f32,
    nr: f32,
    ng: f32,
    nb: f32,
    target_luma: f32,
) {
    let target_luma = target_luma.clamp(0.0, 1.0);
    let (mut nr, mut ng, mut nb) = (nr, ng, nb);
    let min_c = nr.min(ng).min(nb);
    if min_c < 0.0 {
        let anchor = target_luma.max(0.035);
        let scale = (anchor / (anchor - min_c).max(0.00001)).clamp(0.0, 1.0);
        nr = target_luma + (nr - target_luma) * scale;
        ng = target_luma + (ng - target_luma) * scale;
        nb = target_luma + (nb - target_luma) * scale;
    }

    let max_c = nr.max(ng).max(nb);
    if max_c > 1.0 {
        let scale = ((1.0 - target_luma) / (max_c - target_luma).max(0.00001)).clamp(0.0, 1.0);
        nr = target_luma + (nr - target_luma) * scale;
        ng = target_luma + (ng - target_luma) * scale;
        nb = target_luma + (nb - target_luma) * scale;
    }
    *r = nr.clamp(0.0, 1.0);
    *g = ng.clamp(0.0, 1.0);
    *b = nb.clamp(0.0, 1.0);
}

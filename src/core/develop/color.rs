//! Per-pixel colour stage: applies global saturation / vibrance and the
//! colour-mixer band edits (hue rotation, saturation, ratio-preserving
//! brightness) to a single pixel, preserving luminance where required.

use super::*;
use crate::core::color::luminance_f32;
use crate::core::working_color::WorkingColorSpace;

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
    // Raster/PTS wrapper: the result is hard-clamped to [0,1] just below, so
    // there is no downstream OKLCh output boundary to compress an over-gamut
    // push — keep the bit-exact sRGB-hull saturation knee.
    apply_color_linear(settings, curves, false, &mut rl, &mut gl, &mut bl);
    *r = linear_to_srgb(rl).clamp(0.0, 1.0);
    *g = linear_to_srgb(gl).clamp(0.0, 1.0);
    *b = linear_to_srgb(bl).clamp(0.0, 1.0);
}

/// Display-domain proxy twin of the V2 mixer path with externally supplied
/// (spatially guided) Hue/Saturation/Luminance controls. Global Saturation and
/// Vibrance still run through the normal colour stage after the selective edit.
pub(crate) fn apply_color_with_mixer_controls(
    settings: &DevelopSettings,
    controls: [f32; 3],
    r: &mut f32,
    g: &mut f32,
    b: &mut f32,
) {
    let (mut rl, mut gl, mut bl) = (srgb_to_linear(*r), srgb_to_linear(*g), srgb_to_linear(*b));
    apply_color_linear_with_mixer_controls_in_space(
        settings,
        controls,
        false,
        WorkingColorSpace::LinearSrgb,
        &mut rl,
        &mut gl,
        &mut bl,
    );
    *r = linear_to_srgb(rl).clamp(0.0, 1.0);
    *g = linear_to_srgb(gl).clamp(0.0, 1.0);
    *b = linear_to_srgb(bl).clamp(0.0, 1.0);
}

pub(crate) fn apply_color_linear_with_mixer_controls_in_space(
    settings: &DevelopSettings,
    controls: [f32; 3],
    boundary_managed: bool,
    working_space: WorkingColorSpace,
    r: &mut f32,
    g: &mut f32,
    b: &mut f32,
) {
    let mut color =
        crate::core::perceptual_color::working_rgb_to_perceptual([*r, *g, *b], working_space);
    color.hue = (color.hue + (eased_control(controls[0]) * MIXER_HUE_SHIFT_MAX_DEG).to_radians())
        .rem_euclid(std::f32::consts::TAU);
    let sat_delta = eased_control(controls[1]);
    let response = if sat_delta > 0.0 {
        1.35 - 0.73 * smootherstep(0.05, 0.30, color.chroma)
    } else {
        1.0
    };
    let chroma_scale = if sat_delta >= 0.0 {
        1.0 + 1.15 * sat_delta * response
    } else {
        1.0 + 0.95 * sat_delta
    };
    color.chroma *= chroma_scale.max(0.0);
    let light_delta = eased_control(controls[2]);
    let room = if light_delta >= 0.0 {
        (1.0 - color.lightness).max(0.0)
    } else {
        color.lightness.max(0.0)
    };
    color.lightness += light_delta * 0.32 * room;
    [*r, *g, *b] = crate::core::perceptual_color::perceptual_to_working_rgb(color, working_space);
    // Curves=None means only global Saturation/Vibrance are applied here; the
    // mixer controls above are therefore neither reclassified nor doubled.
    apply_color_linear_in_space(settings, None, boundary_managed, working_space, r, g, b);
}

/// Linear working-space colour stage. Mixer V2 classifies and edits one
/// unclamped working-space OKLCh value; Legacy keeps its bounded display-domain
/// UCS/HSV semantics. This is the RAW path's #10 building block;
/// [`apply_color`] remains the bit-compatible Identity/PTS wrapper.
pub(crate) fn apply_color_linear(
    settings: &DevelopSettings,
    curves: Option<&MixerCurves>,
    boundary_managed: bool,
    r: &mut f32,
    g: &mut f32,
    b: &mut f32,
) {
    apply_color_linear_in_space(
        settings,
        curves,
        boundary_managed,
        WorkingColorSpace::LinearSrgb,
        r,
        g,
        b,
    );
}

pub(crate) fn apply_color_linear_in_space(
    settings: &DevelopSettings,
    curves: Option<&MixerCurves>,
    boundary_managed: bool,
    working_space: WorkingColorSpace,
    r: &mut f32,
    g: &mut f32,
    b: &mut f32,
) {
    apply_color_linear_classified_in_space(
        settings,
        curves,
        None,
        boundary_managed,
        working_space,
        r,
        g,
        b,
    );
}

/// Apply linear-light colour corrections while optionally supplying the
/// camera-look display colour used by the Legacy mixer. Mixer V2 deliberately
/// ignores that bounded proxy and classifies in the working space.
///
/// `boundary_managed` marks callers whose output passes through the single
/// hue-preserving OKLCh gamut compression at the scene boundary
/// (`working_to_display`). Those may push chroma past the sRGB hull and let the
/// boundary map it back once; the clamped raster wrapper passes `false`.
#[allow(dead_code)]
pub(crate) fn apply_color_linear_classified(
    settings: &DevelopSettings,
    curves: Option<&MixerCurves>,
    classification: Option<[f32; 3]>,
    boundary_managed: bool,
    r: &mut f32,
    g: &mut f32,
    b: &mut f32,
) {
    apply_color_linear_classified_in_space(
        settings,
        curves,
        classification,
        boundary_managed,
        WorkingColorSpace::LinearSrgb,
        r,
        g,
        b,
    );
}

pub(crate) fn apply_color_linear_classified_in_space(
    settings: &DevelopSettings,
    curves: Option<&MixerCurves>,
    classification: Option<[f32; 3]>,
    boundary_managed: bool,
    working_space: WorkingColorSpace,
    r: &mut f32,
    g: &mut f32,
    b: &mut f32,
) {
    let has_global_saturation =
        settings.saturation.abs() > 0.001 || settings.vibrance.abs() > 0.001;
    if !has_global_saturation && curves.is_none() {
        return;
    }

    let mut v2_color = curves
        .filter(|c| c.algorithm == ColorMixerAlgorithm::V2)
        .map(|_| {
            crate::core::perceptual_color::working_rgb_to_perceptual([*r, *g, *b], working_space)
        });
    let (mut mixer_hue, mut mixer_sat, mut mixer_lum) = match (curves, v2_color) {
        (Some(c), Some(color)) => mixer_adjustments_for_perceptual(c, color),
        (Some(c), None) => {
            let [sr, sg, sb] = classification.unwrap_or_else(|| {
                let output = working_space.to_linear_srgb([*r, *g, *b]);
                [
                    linear_to_srgb(output[0]).clamp(0.0, 1.0),
                    linear_to_srgb(output[1]).clamp(0.0, 1.0),
                    linear_to_srgb(output[2]).clamp(0.0, 1.0),
                ]
            });
            let luma = luminance_f32(sr, sg, sb).clamp(0.0, 1.0);
            mixer_adjustments_for_color(c, sr, sg, sb, luma)
        }
        (None, _) => (0.0, 0.0, 0.0),
    };

    if let Some(mut color) = v2_color.take() {
        color.hue = (color.hue + (eased_control(mixer_hue) * MIXER_HUE_SHIFT_MAX_DEG).to_radians())
            .rem_euclid(std::f32::consts::TAU);
        let sat_delta = eased_control(mixer_sat);
        let response = if settings.develop_engine_version == DevelopEngineVersion::Develop3
            && sat_delta > 0.0
        {
            // Develop3 mixer Saturation is intentionally strongest on muted
            // colour and tapers on already-vivid colour. The chroma-confidence
            // gate in mixer.rs still protects the neutral axis; this shapes
            // only the response after a pixel has a trustworthy hue.
            1.35 - 0.73 * smootherstep(0.05, 0.30, color.chroma)
        } else {
            1.0
        };
        let chroma_scale = if sat_delta >= 0.0 {
            1.0 + 1.15 * sat_delta * response
        } else {
            1.0 + 0.95 * sat_delta
        };
        color.chroma *= chroma_scale.max(0.0);
        let light_delta = eased_control(mixer_lum);
        let room = if light_delta >= 0.0 {
            (1.0 - color.lightness).max(0.0)
        } else {
            color.lightness.max(0.0)
        };
        color.lightness += light_delta * 0.32 * room;
        let converted =
            crate::core::perceptual_color::perceptual_to_working_rgb(color, working_space);
        *r = converted[0];
        *g = converted[1];
        *b = converted[2];
        mixer_hue = 0.0;
        mixer_sat = 0.0;
        mixer_lum = 0.0;
    }

    if mixer_hue.abs() > 0.001 {
        // Rotate hue in Oklab — the SAME perceptual space band membership is
        // computed in — so the shift is even and correctly-directed (the old HSL
        // rotation shifted in a different space than the selection). `mixer_hue`
        // already carries the single chroma/luma gate, and the rotation is a
        // no-op on near-neutral pixels, so no extra guard/blend is needed.
        let mut color =
            crate::core::perceptual_color::working_rgb_to_perceptual([*r, *g, *b], working_space);
        color.hue = (color.hue + (eased_control(mixer_hue) * MIXER_HUE_SHIFT_MAX_DEG).to_radians())
            .rem_euclid(std::f32::consts::TAU);
        [*r, *g, *b] =
            crate::core::perceptual_color::perceptual_to_working_rgb(color, working_space);
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
        scale_linear_chroma_around_luma(r, g, b, factor, boundary_managed, working_space);
    }

    let lum_delta = eased_control(mixer_lum);
    if lum_delta.abs() > 0.001 {
        apply_mixer_brightness(r, g, b, lum_delta, working_space);
    }
}

/// Mixer Luminance = a RATIO-PRESERVING brightness gain (an HSB-style
/// brightness correction): scale the pixel's RGB by one factor so its hue AND
/// saturation are unchanged — brightening orange gives a brighter ORANGE, not a
/// white wash. Only near the gamut ceiling does the brightest channel roll off
/// (a filmic shoulder), so the colour survives as far as the gamut allows
/// before it must compress; it never falls back to an additive lift toward
/// white (the old `apply_luma_target` desaturated once the target passed ~0.88).
fn apply_mixer_brightness(
    r: &mut f32,
    g: &mut f32,
    b: &mut f32,
    lum_delta: f32,
    working_space: WorkingColorSpace,
) {
    let luma = working_luma(working_space, [*r, *g, *b]).clamp(0.0, 1.0);
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

/// ART-style saturation in working linear RGB: `Y + sat * (RGB - Y)`, anchored
/// on true linear Rec.709 luminance. Protection thresholds are the linear-light
/// equivalents of the old display-domain masks, preserving slider feel.
///
/// `boundary_managed` selects how a POSITIVE push resolves gamut:
/// - `true` (RAW scene path): scale chroma along a constant OKLCh hue+lightness
///   line (Q5 gamut-aware Saturation). The scene has one hue-preserving OKLCh
///   gamut clamp at its output boundary (`working_to_display` →
///   `map_to_output_gamut`), which fits any excursion back to the MAX in-gamut
///   chroma at the same hue/lightness. Scaling in OKLCh (not linear RGB) means a
///   strong push saturates up to the hull instead of overshooting in linear RGB
///   and folding back to a duller, hue-rotated colour — the pre-Q5 defect where
///   a full Saturation push turned a saturated red into a duller orange.
/// - `false` (raster/PTS wrapper, hard-clamped right after): cap chroma at the
///   sRGB gamut hull with the smooth linear-RGB knee — bit-identical to before.
///
/// Desaturation (`req <= 0`) only shrinks chroma toward luma and is always in
/// gamut, so both paths share the exact linear `1 + req` scale there.
fn scale_linear_chroma_around_luma(
    r: &mut f32,
    g: &mut f32,
    b: &mut f32,
    factor: f32,
    boundary_managed: bool,
    working_space: WorkingColorSpace,
) {
    let y = working_luma(working_space, [*r, *g, *b]).clamp(0.0, 1.0);
    let protect = smootherstep(0.0027, 0.0174, y) * (1.0 - smootherstep(0.7874, 0.9774, y));
    let req = (factor.clamp(0.0, 3.20) - 1.0) * protect;
    // Q5 gamut-aware positive push (RAW scene path): perceptual OKLCh chroma
    // scale — hue and lightness stay fixed, the output boundary fits chroma to
    // the hull. See the doc comment above for why this replaces the old linear
    // radial scale for `boundary_managed` boosts.
    if boundary_managed && req > 0.0 {
        let mut color =
            crate::core::perceptual_color::working_rgb_to_perceptual([*r, *g, *b], working_space);
        color.chroma *= 1.0 + req;
        let out = crate::core::perceptual_color::perceptual_to_working_rgb(color, working_space);
        *r = out[0];
        *g = out[1];
        *b = out[2];
        return;
    }
    let d = [*r - y, *g - y, *b - y];
    let scale = if boundary_managed || req <= 0.0 {
        1.0 + req
    } else {
        let mut room = f32::INFINITY;
        for dc in d {
            if dc > 1e-6 {
                room = room.min((1.0 - y) / dc - 1.0);
            } else if dc < -1e-6 {
                room = room.min(y / -dc - 1.0);
            }
        }
        let room = if room.is_finite() { room.max(0.0) } else { 0.0 };
        if room > 1e-4 {
            1.0 + room * (req / room).tanh()
        } else {
            1.0
        }
    };
    *r = y + d[0] * scale;
    *g = y + d[1] * scale;
    *b = y + d[2] * scale;
}

#[inline]
fn working_luma(space: WorkingColorSpace, rgb: [f32; 3]) -> f32 {
    if space == WorkingColorSpace::LinearSrgb {
        luma_lin(rgb[0], rgb[1], rgb[2])
    } else {
        let c = space.render_luminance_coefficients();
        c[0] * rgb[0] + c[1] * rgb[1] + c[2] * rgb[2]
    }
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

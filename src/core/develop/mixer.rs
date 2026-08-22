//! Colour-mixer selection model: the periodic-RBF Lagrange basis that maps the
//! 8 band sliders to a smooth per-hue curve, the saturation-confidence weighting,
//! and the per-pixel band adjustments. Legacy selects by display UCS/HSV; V2
//! selects and edits one working-space OKLCh value with continuous neutral
//! protection.

use super::*;

/// Static interpolation basis for the mixer curves: the periodic-RBF Lagrange
/// matrix mapping the 8 node VALUES to `MIXER_CURVE_RES` curve samples —
/// `curve[i] = Σ_k lagrange[i][k] · node_k`, with nodes at the UCS hue of the
/// band swatches. Row-of-unit-vector k is exactly the "band k edited alone"
/// gate shape. Node positions never change, so this is built once.
struct MixerBasis {
    lagrange: Vec<[f32; MIXER_BANDS]>,
}

struct MixerBasisV2 {
    weights: Vec<[f32; MIXER_BANDS]>,
}

/// Periodic RBF kernel (positive-definite on the circle): a truncated cosine
/// series in the exponent, matching the reference module's interpolation.
fn rbf_kernel(d: f32) -> f32 {
    let mut s = 0.0f32;
    for l in 0..MIXER_RBF_TERMS {
        let lf = l as f32;
        s += (-(lf * lf) / MIXER_RBF_SMOOTHING).exp() * (lf * d).cos();
    }
    s.exp()
}

/// Hue of LUT entry `i`, radians in [−π, π) — the layout `curve_sample` indexes.
fn curve_entry_hue(i: usize) -> f32 {
    (i as f32) * std::f32::consts::TAU / (MIXER_CURVE_RES as f32) - std::f32::consts::PI
}

/// Narrow only the warm edge of Reds. The generic periodic RBF is deliberately
/// soft, but after moving the mixer to linear light its Red/Orange overlap
/// became visually too strong. Keep the Magenta->Red edge untouched and fade
/// Red earlier on the shortest Red->Orange arc.
fn red_to_orange_falloff(hue: f32, red: f32, orange: f32) -> f32 {
    let signed_delta = |from: f32, to: f32| {
        (to - from + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
    };
    let arc = signed_delta(red, orange);
    let t = signed_delta(red, hue) / arc;
    if !(0.0..=1.0).contains(&t) {
        return 1.0;
    }
    1.0 - smootherstep(0.20, 0.68, t)
}

fn mixer_basis() -> &'static MixerBasis {
    static BASIS: std::sync::OnceLock<MixerBasis> = std::sync::OnceLock::new();
    BASIS.get_or_init(|| {
        let node_hues: Vec<f32> = MIXER_COLORS
            .iter()
            .map(|c| {
                crate::core::ucs::ucs_hue_rad(
                    c[0] as f32 / 255.0,
                    c[1] as f32 / 255.0,
                    c[2] as f32 / 255.0,
                )
            })
            .collect();
        // K[i][j] = kernel(node_i − node_j), then invert (f64 Gauss-Jordan with
        // partial pivoting — K is symmetric positive-definite on distinct nodes).
        let mut a = [[0.0f64; MIXER_BANDS * 2]; MIXER_BANDS];
        for i in 0..MIXER_BANDS {
            for j in 0..MIXER_BANDS {
                a[i][j] = rbf_kernel(node_hues[i] - node_hues[j]) as f64;
            }
            a[i][MIXER_BANDS + i] = 1.0;
        }
        for col in 0..MIXER_BANDS {
            let piv = (col..MIXER_BANDS)
                .max_by(|&x, &y| a[x][col].abs().partial_cmp(&a[y][col].abs()).unwrap())
                .unwrap();
            a.swap(col, piv);
            let p = a[col][col];
            for v in a[col].iter_mut() {
                *v /= p;
            }
            for row in 0..MIXER_BANDS {
                if row != col {
                    let f = a[row][col];
                    for k in 0..MIXER_BANDS * 2 {
                        a[row][k] -= f * a[col][k];
                    }
                }
            }
        }
        let red_hue = node_hues[0];
        let orange_hue = node_hues[1];
        let lagrange = (0..MIXER_CURVE_RES)
            .map(|i| {
                let hue = curve_entry_hue(i);
                let mut row = [0.0f32; MIXER_BANDS];
                for (k, slot) in row.iter_mut().enumerate() {
                    let mut v = 0.0f64;
                    for (j, &nh) in node_hues.iter().enumerate() {
                        v += rbf_kernel(hue - nh) as f64 * a[j][MIXER_BANDS + k];
                    }
                    *slot = v as f32;
                }
                row[0] *= red_to_orange_falloff(hue, red_hue, orange_hue);
                row
            })
            .collect();
        MixerBasis { lagrange }
    })
}

/// Periodic raised-cosine crossfade between adjacent OKLCh band centres. Each
/// band owns an inner plateau and transitions with zero slope at both ends.
/// Only two adjacent bands overlap and their weights sum to one, eliminating
/// RBF overshoot, sign clamps and the old Red→Orange exception.
fn mixer_basis_v2() -> &'static MixerBasisV2 {
    static BASIS: std::sync::OnceLock<MixerBasisV2> = std::sync::OnceLock::new();
    BASIS.get_or_init(|| {
        let mut centers = [0.0f32; MIXER_BANDS];
        for (i, c) in MIXER_COLORS.iter().enumerate() {
            let lin = [
                srgb_to_linear(c[0] as f32 / 255.0),
                srgb_to_linear(c[1] as f32 / 255.0),
                srgb_to_linear(c[2] as f32 / 255.0),
            ];
            centers[i] = crate::core::perceptual_color::PerceptualColor::from_oklab(
                crate::core::perceptual_color::linear_srgb_to_oklab(lin),
            )
            .hue;
        }
        // Preserve UI band order while unwrapping once around the hue circle.
        for i in 1..MIXER_BANDS {
            while centers[i] <= centers[i - 1] {
                centers[i] += std::f32::consts::TAU;
            }
        }
        const INNER: f32 = 0.18;
        let weights = (0..MIXER_CURVE_RES)
            .map(|sample| {
                let mut hue = curve_entry_hue(sample).rem_euclid(std::f32::consts::TAU);
                while hue < centers[0] {
                    hue += std::f32::consts::TAU;
                }
                while hue >= centers[0] + std::f32::consts::TAU {
                    hue -= std::f32::consts::TAU;
                }
                let mut row = [0.0; MIXER_BANDS];
                for i in 0..MIXER_BANDS {
                    let j = (i + 1) % MIXER_BANDS;
                    let left = centers[i];
                    let right = if j == 0 {
                        centers[0] + std::f32::consts::TAU
                    } else {
                        centers[j]
                    };
                    let mut h = hue;
                    if i == MIXER_BANDS - 1 && h < left {
                        h += std::f32::consts::TAU;
                    }
                    if h >= left && h <= right {
                        let t = ((h - left) / (right - left)).clamp(0.0, 1.0);
                        let blend = if t <= INNER {
                            0.0
                        } else if t >= 1.0 - INNER {
                            1.0
                        } else {
                            let u = (t - INNER) / (1.0 - 2.0 * INNER);
                            0.5 - 0.5 * (std::f32::consts::PI * u).cos()
                        };
                        row[i] = 1.0 - blend;
                        row[j] = blend;
                        break;
                    }
                }
                row
            })
            .collect();
        MixerBasisV2 { weights }
    })
}

/// The three interpolated slider curves over UCS hue, plus the re-gate LUT.
pub(crate) struct MixerCurves {
    pub(crate) hue: Vec<f32>,
    pub(crate) sat: Vec<f32>,
    pub(crate) lum: Vec<f32>,
    /// 0..1 membership of the EDITED bands' hues for the full-res anti-bleed
    /// re-gate — the WGSL twin samples this exact table (uploaded per frame).
    pub(crate) gate: Vec<f32>,
    pub(crate) algorithm: ColorMixerAlgorithm,
}

/// Interpolate the band sliders into smooth periodic curves; `None` when no
/// band slider is engaged (callers then skip the mixer entirely).
pub(crate) fn build_mixer_curves_opt(settings: &DevelopSettings) -> Option<MixerCurves> {
    let any = settings.mixer_hue.iter().any(|v| v.abs() > 0.001)
        || settings.mixer_saturation.iter().any(|v| v.abs() > 0.001)
        || settings.mixer_luminance.iter().any(|v| v.abs() > 0.001);
    if !any {
        return None;
    }
    let legacy_basis = (settings.mixer_algorithm == ColorMixerAlgorithm::Legacy).then(mixer_basis);
    let v2_basis = (settings.mixer_algorithm == ColorMixerAlgorithm::V2).then(mixer_basis_v2);
    let edited = mixer_edit_mask(settings);
    let mut hue = vec![0.0f32; MIXER_CURVE_RES];
    let mut sat = vec![0.0f32; MIXER_CURVE_RES];
    let mut lum = vec![0.0f32; MIXER_CURVE_RES];
    let mut gate = vec![0.0f32; MIXER_CURVE_RES];
    for i in 0..MIXER_CURVE_RES {
        let l = if let Some(basis) = legacy_basis {
            &basis.lagrange[i]
        } else {
            &v2_basis.expect("V2 basis").weights[i]
        };
        let mut h = 0.0f32;
        let mut s = 0.0f32;
        let mut b = 0.0f32;
        let mut g = 0.0f32;
        for k in 0..MIXER_BANDS {
            h += l[k] * settings.mixer_hue[k];
            s += l[k] * settings.mixer_saturation[k];
            b += l[k] * settings.mixer_luminance[k];
            if edited.is_some_and(|e| e[k]) {
                g += l[k];
            }
        }
        hue[i] = h;
        sat[i] = s;
        lum[i] = b;
        gate[i] = g.clamp(0.0, 1.0);
    }
    // Clamp the Saturation and Luminance curves to the SPAN of their slider
    // node values (the Sat/Lum curves are clipped to their node span). The periodic-RBF
    // interpolation overshoots/undershoots between nodes, so a single positive
    // node (e.g. Red +Lum) dips slightly NEGATIVE at a neighbouring hue —
    // which silently DARKENS/greys colours the user never touched (warm
    // highlights under a red edit went grey). Clamping to [min_node, max_node]
    // kills the sign-flip while leaving the intended in-span shape. Hue is left
    // free (an offset, not a gain — it is left unclipped).
    if settings.mixer_algorithm == ColorMixerAlgorithm::Legacy {
        clamp_curve_to_node_span(&mut sat, &settings.mixer_saturation);
        clamp_curve_to_node_span(&mut lum, &settings.mixer_luminance);
    }
    Some(MixerCurves {
        hue,
        sat,
        lum,
        gate,
        algorithm: settings.mixer_algorithm,
    })
}

/// Clamp an interpolated mixer curve to the `[min, max]` span of the slider
/// values that built it, so RBF over/undershoot cannot flip the correction's
/// sign at hues between the edited nodes.
fn clamp_curve_to_node_span(curve: &mut [f32], nodes: &[f32; MIXER_BANDS]) {
    let mut lo = 0.0f32;
    let mut hi = 0.0f32;
    for &n in nodes {
        lo = lo.min(n);
        hi = hi.max(n);
    }
    for v in curve.iter_mut() {
        *v = v.clamp(lo, hi);
    }
}

/// Sample a periodic curve LUT at hue `h` (radians, any range): linear
/// interpolation with wrap-around, mirrored by the WGSL gate-LUT sampler.
pub(crate) fn curve_sample(lut: &[f32], h: f32) -> f32 {
    let n = lut.len();
    let t = (h + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU
        * n as f32;
    let i = (t as usize).min(n - 1);
    let f = t - i as f32;
    let a = lut[i];
    let b = lut[(i + 1) % n];
    a + (b - a) * f
}

/// HSV saturation (delta/max): luma-normalised colour confidence — high for
/// dark saturated colours, ≈0 for greys of any lightness.
pub(crate) fn hsv_saturation(r: f32, g: f32, b: f32) -> f32 {
    let max = r.max(g).max(b);
    let delta = max - r.min(g).min(b);
    if max > 1e-4 && delta > 1e-6 {
        delta / max
    } else {
        0.0
    }
}

/// Logistic saturation weight (0..1) around a shifted midpoint. Mirrored by
/// WGSL `dev_satweight`.
fn mixer_satweight(x: f32) -> f32 {
    1.0 / (1.0 + (-MIXER_SAT_STEEP * x).exp())
}

/// Full colour-confidence weight for one pixel: the logistic HSV-saturation
/// weight (shifted midpoint per edit type) × the absolute-delta gate (see
/// `MIXER_DELTA_LO`). Mirrored by WGSL `dev_mixer_weight`.
pub(crate) fn mixer_weight(r: f32, g: f32, b: f32, shift: f32) -> f32 {
    let delta = r.max(g).max(b) - r.min(g).min(b);
    mixer_satweight(hsv_saturation(r, g, b) - shift)
        * smootherstep(MIXER_DELTA_LO, MIXER_DELTA_HI, delta)
}

/// Lower confidence floor for negative Saturation. A pale colour cast still
/// has a meaningful hue and must be removable, while exact/near greys remain
/// protected by the absolute-delta gate.
pub(crate) fn mixer_desat_weight(r: f32, g: f32, b: f32) -> f32 {
    let delta = r.max(g).max(b) - r.min(g).min(b);
    mixer_satweight(hsv_saturation(r, g, b) - 0.01) * smootherstep(0.002, 0.025, delta)
}

/// Per-pixel selective-colour mixer contribution for a bounded display RGB.
/// Legacy samples UCS/HSV directly. This remains a compatibility wrapper for
/// V2 display-domain callers; the RAW working path calls
/// [`mixer_adjustments_for_perceptual`] with its unclamped working OKLCh.
pub(crate) fn mixer_adjustments_for_color(
    curves: &MixerCurves,
    r: f32,
    g: f32,
    b: f32,
    luma: f32,
) -> (f32, f32, f32) {
    if curves.algorithm == ColorMixerAlgorithm::V2 {
        let lab = crate::core::perceptual_color::linear_srgb_to_oklab([
            srgb_to_linear(r),
            srgb_to_linear(g),
            srgb_to_linear(b),
        ]);
        let perceptual = crate::core::perceptual_color::PerceptualColor::from_oklab(lab);
        return mixer_adjustments_for_perceptual(curves, perceptual);
    }
    let h = crate::core::ucs::ucs_hue_rad(r, g, b);
    let w = mixer_weight(r, g, b, MIXER_SAT_SHIFT);
    let lum_guard = smootherstep(LUM_BLACK_LO, LUM_BLACK_HI, luma)
        * (1.0 - smootherstep(LUM_WHITE_LO, LUM_WHITE_HI, luma));
    let wl = mixer_weight(r, g, b, MIXER_BRIGHT_SHIFT) * lum_guard;
    let sat = curve_sample(&curves.sat, h);
    let sat_w = if sat < 0.0 {
        mixer_desat_weight(r, g, b)
    } else {
        w
    };
    (
        (curve_sample(&curves.hue, h) * w).clamp(-CONTROL_LIMIT, CONTROL_LIMIT),
        (sat * sat_w).clamp(-CONTROL_LIMIT, CONTROL_LIMIT),
        (curve_sample(&curves.lum, h) * wl).clamp(-CONTROL_LIMIT, CONTROL_LIMIT),
    )
}

/// V2 mixer controls from the same working-space OKLCh value the edit mutates.
/// Keeping classification and correction on one value prevents an out-of-sRGB
/// scene colour from being assigned to a different band by the output preview.
pub(crate) fn mixer_adjustments_for_perceptual(
    curves: &MixerCurves,
    perceptual: crate::core::perceptual_color::PerceptualColor,
) -> (f32, f32, f32) {
    debug_assert_eq!(curves.algorithm, ColorMixerAlgorithm::V2);
    // Continuous chroma confidence protects the neutral axis without a hard
    // hue gate. Negative saturation gets a lower floor so weak casts remain
    // removable while exact neutrals stay fixed.
    let normal_w = smootherstep(0.008, 0.055, perceptual.chroma);
    let desat_w = smootherstep(0.002, 0.025, perceptual.chroma);
    let hue = perceptual.hue;
    let sat = curve_sample(&curves.sat, hue);
    let lum_guard = smootherstep(0.015, 0.09, perceptual.lightness)
        * (1.0 - smootherstep(0.90, 1.02, perceptual.lightness));
    (
        (curve_sample(&curves.hue, hue) * normal_w).clamp(-CONTROL_LIMIT, CONTROL_LIMIT),
        (sat * if sat < 0.0 { desat_w } else { normal_w }).clamp(-CONTROL_LIMIT, CONTROL_LIMIT),
        (curve_sample(&curves.lum, hue) * normal_w * lum_guard)
            .clamp(-CONTROL_LIMIT, CONTROL_LIMIT),
    )
}

/// Test-only view of the per-band membership: the Lagrange basis (the shape a
/// lone edit of band k takes) sampled at the pixel's UCS hue, times the colour
/// confidence weight — i.e. "how much of band k's edit would this pixel take".
#[cfg(test)]
pub(crate) fn mixer_band_memberships(r: f32, g: f32, b: f32) -> [f32; MIXER_BANDS] {
    let basis = mixer_basis();
    let h = crate::core::ucs::ucs_hue_rad(r, g, b);
    let w = mixer_weight(r, g, b, MIXER_SAT_SHIFT);
    let n = MIXER_CURVE_RES;
    let t = (h + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU
        * n as f32;
    let i = (t as usize).min(n - 1);
    let f = t - i as f32;
    let mut out = [0.0f32; MIXER_BANDS];
    for (k, slot) in out.iter_mut().enumerate() {
        let a = basis.lagrange[i][k];
        let bb = basis.lagrange[(i + 1) % n][k];
        *slot = ((a + (bb - a) * f) * w).clamp(0.0, 1.0);
    }
    out
}

/// Test-only view of the (hue/sat, luminance) edit weights of one pixel.
#[cfg(test)]
pub(crate) fn mixer_edit_weights(r: f32, g: f32, b: f32, luma: f32) -> (f32, f32) {
    let w = mixer_weight(r, g, b, MIXER_SAT_SHIFT);
    let lum_guard = smootherstep(LUM_BLACK_LO, LUM_BLACK_HI, luma)
        * (1.0 - smootherstep(LUM_WHITE_LO, LUM_WHITE_HI, luma));
    (w, mixer_weight(r, g, b, MIXER_BRIGHT_SHIFT) * lum_guard)
}

pub(crate) fn rgb_chroma(r: f32, g: f32, b: f32) -> f32 {
    r.max(g).max(b) - r.min(g).min(b)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MixerTarget {
    pub band: usize,
    pub hue_radians: f32,
    pub confidence: f32,
}

/// Engine API for a future targeted-adjustment eyedropper. Keeping this out of
/// the main UI preserves the existing Photoshop-style panel until Phase 7.
pub fn mixer_target_from_srgb(rgb: [f32; 3]) -> MixerTarget {
    let lab = crate::core::perceptual_color::linear_srgb_to_oklab([
        srgb_to_linear(rgb[0]),
        srgb_to_linear(rgb[1]),
        srgb_to_linear(rgb[2]),
    ]);
    let p = crate::core::perceptual_color::PerceptualColor::from_oklab(lab);
    let basis = mixer_basis_v2();
    let sample = (((p.hue + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
        / std::f32::consts::TAU)
        * MIXER_CURVE_RES as f32) as usize
        % MIXER_CURVE_RES;
    let (band, _) = basis.weights[sample]
        .iter()
        .copied()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap();
    MixerTarget {
        band,
        hue_radians: p.hue,
        confidence: smootherstep(0.008, 0.055, p.chroma),
    }
}

/// Per-pixel grayscale mask value for debug/targeted preview.
pub fn mixer_mask_preview(settings: &DevelopSettings, rgb: [f32; 3]) -> f32 {
    build_mixer_curves_opt(settings)
        .map(|curves| band_affinity(&curves, rgb[0], rgb[1], rgb[2]))
        .unwrap_or(0.0)
}

#[cfg(test)]
mod v2_tests {
    use super::*;
    use crate::core::perceptual_color::{
        linear_srgb_to_oklab, perceptual_to_working_rgb, working_rgb_to_perceptual, PerceptualColor,
    };
    use crate::core::working_color::WorkingColorSpace;

    fn circular_error(a: f32, b: f32) -> f32 {
        let d = (a - b).abs().rem_euclid(std::f32::consts::TAU);
        d.min(std::f32::consts::TAU - d)
    }

    /// Periodic Catmull-Rom candidate evaluated for kernel selection. It is not
    /// used in production because its outer lobes become negative.
    fn cubic_candidate_weights(t: f32) -> [f32; 4] {
        let t2 = t * t;
        let t3 = t2 * t;
        [
            -0.5 * t + t2 - 0.5 * t3,
            1.0 - 2.5 * t2 + 1.5 * t3,
            0.5 * t + 2.0 * t2 - 1.5 * t3,
            -0.5 * t2 + 0.5 * t3,
        ]
    }

    #[test]
    fn raised_cosine_wins_kernel_measurement_over_periodic_cubic() {
        let mut cubic_negative = 0;
        let mut cubic_sum_error = 0.0f32;
        for i in 0..=256 {
            let w = cubic_candidate_weights(i as f32 / 256.0);
            cubic_negative += w.iter().filter(|&&v| v < -1.0e-7).count();
            cubic_sum_error = cubic_sum_error.max((w.iter().sum::<f32>() - 1.0).abs());
        }
        assert!(
            cubic_negative > 0,
            "cubic candidate unexpectedly has no negative lobes"
        );
        assert!(cubic_sum_error < 2.0e-6);
        assert!(mixer_basis_v2().weights.iter().flatten().all(|&v| v >= 0.0));
    }

    #[test]
    fn raised_cosine_is_normalized_nonnegative_and_periodic() {
        let basis = mixer_basis_v2();
        for row in &basis.weights {
            assert!(row.iter().all(|&w| (0.0..=1.0).contains(&w)));
            assert!((row.iter().sum::<f32>() - 1.0).abs() < 2.0e-6);
            assert!(row.iter().filter(|&&w| w > 1.0e-6).count() <= 2);
        }
        for band in 0..MIXER_BANDS {
            for opposite in 0..MIXER_BANDS {
                if (band + 4) % MIXER_BANDS == opposite {
                    let max_joint = basis
                        .weights
                        .iter()
                        .map(|w| w[band].min(w[opposite]))
                        .fold(0.0, f32::max);
                    assert!(max_joint <= 1.0e-6);
                }
            }
        }
        let seam_delta: f32 = basis.weights[0]
            .iter()
            .zip(&basis.weights[MIXER_CURVE_RES - 1])
            .map(|(a, b)| (a - b).abs())
            .sum();
        assert!(seam_delta < 0.08, "0/360 seam delta {seam_delta}");
    }

    #[test]
    fn v2_axes_preserve_the_other_oklch_components() {
        let input = [0.42, 0.13, 0.07];
        let encoded = [
            linear_to_srgb(input[0]),
            linear_to_srgb(input[1]),
            linear_to_srgb(input[2]),
        ];
        let before = PerceptualColor::from_oklab(linear_srgb_to_oklab(input));
        for axis in 0..3 {
            let mut settings = DevelopSettings::default();
            let band = 1; // orange
            match axis {
                0 => settings.mixer_hue[band] = 80.0,
                1 => settings.mixer_saturation[band] = 80.0,
                _ => settings.mixer_luminance[band] = 80.0,
            }
            let curves = build_mixer_curves_opt(&settings).unwrap();
            let [mut r, mut g, mut b] = input;
            super::super::apply_color_linear_classified(
                &settings,
                Some(&curves),
                Some(encoded),
                true,
                &mut r,
                &mut g,
                &mut b,
            );
            let after = PerceptualColor::from_oklab(linear_srgb_to_oklab([r, g, b]));
            match axis {
                0 => {
                    assert!((after.lightness - before.lightness).abs() < 2.0e-5);
                    assert!((after.chroma - before.chroma).abs() < 2.0e-5);
                }
                1 => {
                    assert!((after.lightness - before.lightness).abs() < 2.0e-5);
                    assert!(circular_error(after.hue, before.hue) < 2.0e-5);
                }
                _ => {
                    assert!((after.chroma - before.chroma).abs() < 2.0e-5);
                    assert!(circular_error(after.hue, before.hue) < 2.0e-5);
                }
            }
        }
    }

    #[test]
    fn v2_classifies_and_rotates_one_wide_working_oklch_value() {
        let band = 3; // green
        let swatch = MIXER_COLORS[band];
        let swatch_lab = linear_srgb_to_oklab([
            srgb_to_linear(swatch[0] as f32 / 255.0),
            srgb_to_linear(swatch[1] as f32 / 255.0),
            srgb_to_linear(swatch[2] as f32 / 255.0),
        ]);
        let input_color = PerceptualColor {
            lightness: 0.62,
            chroma: 0.24,
            hue: PerceptualColor::from_oklab(swatch_lab).hue,
        };
        let space = WorkingColorSpace::LinearProPhoto;
        let input = perceptual_to_working_rgb(input_color, space);

        let mut settings = DevelopSettings::default();
        settings.mixer_hue[band] = 120.0;
        let curves = build_mixer_curves_opt(&settings).unwrap();
        let apply = |classification: [f32; 3]| {
            let [mut r, mut g, mut b] = input;
            super::super::apply_color_linear_classified_in_space(
                &settings,
                Some(&curves),
                Some(classification),
                true,
                space,
                &mut r,
                &mut g,
                &mut b,
            );
            working_rgb_to_perceptual([r, g, b], space)
        };
        // Deliberately conflicting bounded display proxies must not change V2.
        let green_proxy = apply([0.0, 1.0, 0.0]);
        let magenta_proxy = apply([1.0, 0.0, 1.0]);
        assert!(circular_error(green_proxy.hue, input_color.hue) > 0.05);
        assert!(circular_error(green_proxy.hue, magenta_proxy.hue) < 2.0e-5);
        assert!((green_proxy.lightness - input_color.lightness).abs() < 2.0e-5);
        assert!((green_proxy.chroma - input_color.chroma).abs() < 2.0e-5);
    }

    #[test]
    fn v2_controls_are_periodic_and_neutral_axis_is_fixed() {
        let mut settings = DevelopSettings::default();
        settings.mixer_hue = [120.0, -80.0, 35.0, 90.0, -45.0, 65.0, -110.0, 20.0];
        settings.mixer_saturation = [-200.0; MIXER_BANDS];
        settings.mixer_luminance = [40.0, -35.0, 25.0, -15.0, 10.0, -5.0, 30.0, -20.0];
        let curves = build_mixer_curves_opt(&settings).unwrap();
        let at = |hue| {
            mixer_adjustments_for_perceptual(
                &curves,
                PerceptualColor {
                    lightness: 0.58,
                    chroma: 0.16,
                    hue,
                },
            )
        };
        let zero = at(0.0);
        let turn = at(std::f32::consts::TAU);
        for (a, b) in [zero.0, zero.1, zero.2]
            .into_iter()
            .zip([turn.0, turn.1, turn.2])
        {
            assert!((a - b).abs() < 1.0e-4, "periodic seam {a} vs {b}");
        }

        let neutral = mixer_adjustments_for_perceptual(
            &curves,
            PerceptualColor {
                lightness: 0.58,
                chroma: 0.0,
                hue: 5.1,
            },
        );
        assert_eq!(neutral, (0.0, 0.0, 0.0));
    }

    #[test]
    fn v2_zero_chroma_endpoint_is_even_across_hue() {
        let mut settings = DevelopSettings::default();
        settings.mixer_saturation = [-CONTROL_LIMIT; MIXER_BANDS];
        let curves = build_mixer_curves_opt(&settings).unwrap();
        let space = WorkingColorSpace::LinearProPhoto;
        let mut ratios = Vec::new();
        for step in 0..24 {
            let before = PerceptualColor {
                lightness: 0.62,
                chroma: 0.16,
                hue: step as f32 * std::f32::consts::TAU / 24.0,
            };
            let [mut r, mut g, mut b] = perceptual_to_working_rgb(before, space);
            super::super::apply_color_linear_classified_in_space(
                &settings,
                Some(&curves),
                None,
                true,
                space,
                &mut r,
                &mut g,
                &mut b,
            );
            let after = working_rgb_to_perceptual([r, g, b], space);
            assert!((after.lightness - before.lightness).abs() < 2.0e-5);
            let hue_error = circular_error(after.hue, before.hue);
            assert!(
                hue_error < 2.0e-4,
                "hue {step} drifted {hue_error}: {before:?} -> {after:?}"
            );
            ratios.push(after.chroma / before.chroma);
        }
        let lo = ratios.iter().copied().fold(f32::INFINITY, f32::min);
        let hi = ratios.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert!(hi - lo < 2.0e-5, "desaturation varies by hue: {lo}..{hi}");
        assert!(hi < 0.051, "maximum desaturation is too weak: {hi}");
    }

    #[test]
    fn v2_skin_hue_response_is_stable_across_tone_levels() {
        let band = 1; // orange / skin neighbourhood
        let swatch = MIXER_COLORS[band];
        let hue = PerceptualColor::from_oklab(linear_srgb_to_oklab([
            srgb_to_linear(swatch[0] as f32 / 255.0),
            srgb_to_linear(swatch[1] as f32 / 255.0),
            srgb_to_linear(swatch[2] as f32 / 255.0),
        ]))
        .hue;
        let mut settings = DevelopSettings::default();
        settings.mixer_hue[band] = 90.0;
        let curves = build_mixer_curves_opt(&settings).unwrap();
        let space = WorkingColorSpace::LinearProPhoto;
        let hue_shift = |lightness: f32, chroma: f32| {
            let before = PerceptualColor {
                lightness,
                chroma,
                hue,
            };
            let [mut r, mut g, mut b] = perceptual_to_working_rgb(before, space);
            super::super::apply_color_linear_classified_in_space(
                &settings,
                Some(&curves),
                None,
                true,
                space,
                &mut r,
                &mut g,
                &mut b,
            );
            let after = working_rgb_to_perceptual([r, g, b], space);
            (after.hue - before.hue).rem_euclid(std::f32::consts::TAU)
        };
        // Two proportional OKLCh levels model the same skin chromaticity after
        // different exposure/contrast placements. Both are above the smooth
        // chroma-confidence knee, so their selective hue response must agree.
        let shadow = hue_shift(0.43, 0.09);
        let highlight = hue_shift(0.76, 0.16);
        assert!(
            (shadow - highlight).abs() < 2.0e-5,
            "{shadow} vs {highlight}"
        );
    }

    #[test]
    fn missing_algorithm_deserializes_legacy_while_new_defaults_use_v2() {
        let old = serde_json::from_str::<DevelopSettings>("{}").unwrap();
        assert_eq!(old.mixer_algorithm, ColorMixerAlgorithm::Legacy);
        assert_eq!(
            DevelopSettings::default().mixer_algorithm,
            ColorMixerAlgorithm::V2
        );
    }

    #[test]
    fn targeted_api_returns_band_and_continuous_mask() {
        let target = mixer_target_from_srgb([0.82, 0.46, 0.22]);
        assert!(target.band < MIXER_BANDS && target.confidence > 0.9);
        let mut settings = DevelopSettings::default();
        settings.mixer_saturation[target.band] = 100.0;
        let selected = mixer_mask_preview(&settings, [0.82, 0.46, 0.22]);
        let neutral = mixer_mask_preview(&settings, [0.5, 0.5, 0.5]);
        assert!(
            selected > neutral + 0.5,
            "selected={selected}, neutral={neutral}"
        );
    }
}

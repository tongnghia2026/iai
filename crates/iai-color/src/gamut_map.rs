//! Hue-preserving output gamut mapping in OKLCh.
//!
//! The mapper is an identity for colours already inside the selected output
//! RGB cube. Out-of-gamut colours keep perceptual lightness and hue while a
//! a shared profile-specific cusp LUT finds the largest reproducible chroma.
//! A binary search remains test-only as the quality reference.

use super::perceptual_color::{linear_srgb_to_oklab, oklab_to_linear_srgb, PerceptualColor};
use super::working_color::{apply_matrix, OutputColorSpace, WorkingColorSpace};
use std::sync::OnceLock;

/// Shared CPU/GPU cusp-table schema. The shader receives these dimensions and
/// the buffer offset from the CPU payload rather than carrying an independent
/// copy of the constants.
pub const CUSP_HUE_SAMPLES: usize = 1024;
pub const CUSP_LIGHTNESS_SAMPLES: usize = 256;
pub const CUSP_LUT_LEN: usize = CUSP_HUE_SAMPLES * CUSP_LIGHTNESS_SAMPLES;
const CUSP_SAFETY: f32 = 0.999;
pub const CUSP_FALLBACK_STEPS: usize = 6;

const SRGB_TO_P3: [[f32; 3]; 3] = [
    [0.822_592_9, 0.177_533_9, 0.0],
    [0.033_199_5, 0.966_783_5, 0.0],
    [0.017_085_3, 0.072_395_7, 0.910_301_5],
];

#[inline]
fn target_rgb(rgb: [f32; 3], output: OutputColorSpace) -> [f32; 3] {
    match output {
        OutputColorSpace::Srgb => rgb,
        OutputColorSpace::DisplayP3 => apply_matrix(&SRGB_TO_P3, rgb),
    }
}

#[inline]
pub fn is_in_gamut(rgb: [f32; 3], output: OutputColorSpace) -> bool {
    target_rgb(rgb, output)
        .into_iter()
        .all(|v| (-1.0e-6..=1.0 + 1.0e-6).contains(&v))
}

#[cfg(test)]
fn map_to_output_gamut_reference(rgb: [f32; 3], output: OutputColorSpace) -> [f32; 3] {
    let target = target_rgb(rgb, output);
    if rgb.into_iter().all(f32::is_finite) && target.into_iter().all(|v| (0.0..=1.0).contains(&v)) {
        return rgb;
    }
    if !rgb.into_iter().all(f32::is_finite) {
        return [0.0; 3];
    }
    let mut color = PerceptualColor::from_oklab(linear_srgb_to_oklab(rgb));
    color.lightness = color.lightness.clamp(0.0, 1.0);
    if color.chroma <= 1.0e-7 {
        return oklab_to_linear_srgb(color.to_oklab()).map(|v| v.clamp(0.0, 1.0));
    }
    let original_chroma = color.chroma;
    let mut low = 0.0f32;
    let mut high = original_chroma;
    for _ in 0..18 {
        let mid = 0.5 * (low + high);
        color.chroma = mid;
        if is_in_gamut(oklab_to_linear_srgb(color.to_oklab()), output) {
            low = mid;
        } else {
            high = mid;
        }
    }
    color.chroma = low;
    let mapped = oklab_to_linear_srgb(color.to_oklab());
    match output {
        OutputColorSpace::Srgb => mapped.map(|v| v.clamp(0.0, 1.0)),
        // Representation remains linear sRGB; valid P3 primaries may therefore
        // legitimately contain negative or >1 sRGB components.
        OutputColorSpace::DisplayP3 => mapped,
    }
}

fn build_cusp_lut(output: OutputColorSpace) -> Vec<f32> {
    let mut table = vec![0.0; CUSP_LUT_LEN];
    for lightness_index in 0..CUSP_LIGHTNESS_SAMPLES {
        let lightness = lightness_index as f32 / (CUSP_LIGHTNESS_SAMPLES - 1) as f32;
        for hue_index in 0..CUSP_HUE_SAMPLES {
            let hue = std::f32::consts::TAU * hue_index as f32 / CUSP_HUE_SAMPLES as f32;
            let mut low = 0.0f32;
            let mut high = 1.0f32;
            for _ in 0..20 {
                let mid = 0.5 * (low + high);
                let candidate = oklab_to_linear_srgb(
                    PerceptualColor {
                        lightness,
                        chroma: mid,
                        hue,
                    }
                    .to_oklab(),
                );
                if is_in_gamut(candidate, output) {
                    low = mid;
                } else {
                    high = mid;
                }
            }
            table[lightness_index * CUSP_HUE_SAMPLES + hue_index] = low * CUSP_SAFETY;
        }
    }
    table
}

/// Profile-specific OKLCh cusp table uploaded verbatim to the GPU preview.
/// Generation is deterministic and lazy; no proprietary profile/LUT resource
/// is embedded in the application.
pub fn cusp_lut(output: OutputColorSpace) -> &'static [f32] {
    static SRGB: OnceLock<Vec<f32>> = OnceLock::new();
    static DISPLAY_P3: OnceLock<Vec<f32>> = OnceLock::new();
    match output {
        OutputColorSpace::Srgb => SRGB.get_or_init(|| build_cusp_lut(output)),
        OutputColorSpace::DisplayP3 => DISPLAY_P3.get_or_init(|| build_cusp_lut(output)),
    }
}

fn sample_cusp(lightness: f32, hue: f32, output: OutputColorSpace) -> f32 {
    let table = cusp_lut(output);
    let hue_position =
        hue.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU * CUSP_HUE_SAMPLES as f32;
    let hue0 = hue_position.floor() as usize % CUSP_HUE_SAMPLES;
    let hue1 = (hue0 + 1) % CUSP_HUE_SAMPLES;
    let hue_mix = hue_position - hue_position.floor();
    let lightness_position = lightness.clamp(0.0, 1.0) * (CUSP_LIGHTNESS_SAMPLES - 1) as f32;
    let lightness0 = lightness_position.floor() as usize;
    let lightness1 = (lightness0 + 1).min(CUSP_LIGHTNESS_SAMPLES - 1);
    let lightness_mix = lightness_position - lightness_position.floor();
    let at = |l: usize, h: usize| table[l * CUSP_HUE_SAMPLES + h];
    let low = at(lightness0, hue0) + (at(lightness0, hue1) - at(lightness0, hue0)) * hue_mix;
    let high = at(lightness1, hue0) + (at(lightness1, hue1) - at(lightness1, hue0)) * hue_mix;
    low + (high - low) * lightness_mix
}

/// Map a display-linear sRGB/D65 colour to the selected output gamut. The
/// returned values remain represented in linear sRGB for the existing output
/// encoder/monitor-transform boundary. A shared cusp LUT replaces the old
/// eighteen-step search; the short fallback only runs when interpolation lands
/// microscopically outside the target hull.
pub fn map_to_output_gamut(rgb: [f32; 3], output: OutputColorSpace) -> [f32; 3] {
    let target = target_rgb(rgb, output);
    if rgb.into_iter().all(f32::is_finite) && target.into_iter().all(|v| (0.0..=1.0).contains(&v)) {
        return rgb;
    }
    if !rgb.into_iter().all(f32::is_finite) {
        return [0.0; 3];
    }
    let mut color = PerceptualColor::from_oklab(linear_srgb_to_oklab(rgb));
    color.lightness = color.lightness.clamp(0.0, 1.0);
    if color.chroma <= 1.0e-7 {
        return oklab_to_linear_srgb(color.to_oklab()).map(|v| v.clamp(0.0, 1.0));
    }
    color.chroma = color
        .chroma
        .min(sample_cusp(color.lightness, color.hue, output));
    let mut mapped = oklab_to_linear_srgb(color.to_oklab());
    if !is_in_gamut(mapped, output) {
        let mut high = color.chroma;
        let mut low = high * 0.95;
        color.chroma = low;
        if !is_in_gamut(oklab_to_linear_srgb(color.to_oklab()), output) {
            low = 0.0;
        }
        for _ in 0..CUSP_FALLBACK_STEPS {
            let mid = 0.5 * (low + high);
            color.chroma = mid;
            if is_in_gamut(oklab_to_linear_srgb(color.to_oklab()), output) {
                low = mid;
            } else {
                high = mid;
            }
        }
        color.chroma = low;
        mapped = oklab_to_linear_srgb(color.to_oklab());
    }
    match output {
        OutputColorSpace::Srgb => mapped.map(|v| v.clamp(0.0, 1.0)),
        OutputColorSpace::DisplayP3 => mapped,
    }
}

/// The single scene-working -> output boundary. All creative nodes keep their
/// native wide-gamut RGB values; only this function converts to the existing
/// linear-sRGB output representation and compresses against the selected
/// output profile gamut.
pub fn map_working_to_output_gamut(
    rgb: [f32; 3],
    working: WorkingColorSpace,
    output: OutputColorSpace,
) -> [f32; 3] {
    map_to_output_gamut(working.to_linear_srgb(rgb), output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn hue_error(a: PerceptualColor, b: PerceptualColor) -> f32 {
        let d = (a.hue - b.hue).abs().rem_euclid(std::f32::consts::TAU);
        d.min(std::f32::consts::TAU - d)
    }

    #[test]
    fn in_gamut_is_bit_identity() {
        for rgb in [
            [0.0; 3],
            [0.18; 3],
            [1.0; 3],
            [0.7, 0.2, 0.4],
            [0.05, 0.8, 0.2],
        ] {
            assert_eq!(map_to_output_gamut(rgb, OutputColorSpace::Srgb), rgb);
        }
    }

    #[test]
    fn working_boundary_matches_one_unclamped_conversion_then_map() {
        for working in [WorkingColorSpace::AcesCg, WorkingColorSpace::LinearProPhoto] {
            for rgb in [[0.18; 3], [1.2, -0.1, 0.3], [-0.2, 0.7, 2.0]] {
                assert_eq!(
                    map_working_to_output_gamut(rgb, working, OutputColorSpace::Srgb),
                    map_to_output_gamut(working.to_linear_srgb(rgb), OutputColorSpace::Srgb)
                );
            }
        }
    }

    #[test]
    fn out_of_gamut_reduces_chroma_and_preserves_hue() {
        for rgb in [
            [1.8, -0.2, 0.1],
            [-0.4, 1.5, 0.2],
            [0.1, -0.3, 2.2],
            [2.0, 0.0, 2.0],
        ] {
            let before = PerceptualColor::from_oklab(linear_srgb_to_oklab(rgb));
            let mapped = map_to_output_gamut(rgb, OutputColorSpace::Srgb);
            let after = PerceptualColor::from_oklab(linear_srgb_to_oklab(mapped));
            assert!(
                is_in_gamut(mapped, OutputColorSpace::Srgb),
                "{rgb:?} -> {mapped:?}"
            );
            assert!(after.chroma <= before.chroma + 2.0e-5);
            assert!(hue_error(before, after) < 2.0e-4, "{before:?} -> {after:?}");
        }
    }

    #[test]
    fn saturation_ramp_is_monotone_until_the_cusp() {
        let base = PerceptualColor {
            lightness: 0.62,
            chroma: 0.0,
            hue: 4.2,
        };
        let mut last = -1.0;
        for i in 0..=100 {
            let mut p = base;
            p.chroma = i as f32 * 0.01;
            let mapped =
                map_to_output_gamut(oklab_to_linear_srgb(p.to_oklab()), OutputColorSpace::Srgb);
            let c = PerceptualColor::from_oklab(linear_srgb_to_oklab(mapped)).chroma;
            assert!(c + 5.0e-6 >= last, "chroma fell at {i}: {last} -> {c}");
            last = c;
        }
    }

    #[test]
    fn hue_is_continuous_at_red_wrap_and_blue_cyan_cusp() {
        for center in [0.0, std::f32::consts::TAU, 3.7, 4.2] {
            let mut previous = None;
            for offset in -20..=20 {
                let p = PerceptualColor {
                    lightness: 0.62,
                    chroma: 0.48,
                    hue: center + offset as f32 * 0.001,
                };
                let mapped =
                    map_to_output_gamut(oklab_to_linear_srgb(p.to_oklab()), OutputColorSpace::Srgb);
                let out = PerceptualColor::from_oklab(linear_srgb_to_oklab(mapped));
                assert!(hue_error(p, out) < 3.0e-4, "{p:?} -> {out:?}");
                if let Some(last) = previous {
                    assert!(
                        hue_error(last, out) < 0.002,
                        "hue seam: {last:?} -> {out:?}"
                    );
                }
                previous = Some(out);
            }
        }
    }

    #[test]
    fn p3_retains_at_least_as_much_chroma_as_srgb() {
        let p = PerceptualColor {
            lightness: 0.65,
            chroma: 0.4,
            hue: 2.5,
        };
        let rgb = oklab_to_linear_srgb(p.to_oklab());
        let chroma = |out| PerceptualColor::from_oklab(linear_srgb_to_oklab(out)).chroma;
        assert!(
            chroma(map_to_output_gamut(rgb, OutputColorSpace::DisplayP3)) + 2.0e-5
                >= chroma(map_to_output_gamut(rgb, OutputColorSpace::Srgb))
        );
    }

    #[test]
    fn mapper_eliminates_out_of_gamut_samples() {
        let mut before = 0usize;
        let mut after = 0usize;
        for h in 0..360 {
            for c in 0..=20 {
                let p = PerceptualColor {
                    lightness: 0.62,
                    chroma: c as f32 * 0.025,
                    hue: (h as f32).to_radians(),
                };
                let rgb = oklab_to_linear_srgb(p.to_oklab());
                before += usize::from(!is_in_gamut(rgb, OutputColorSpace::Srgb));
                after += usize::from(!is_in_gamut(
                    map_to_output_gamut(rgb, OutputColorSpace::Srgb),
                    OutputColorSpace::Srgb,
                ));
            }
        }
        assert!(before > 0);
        assert_eq!(after, 0, "{after}/{before} mapped samples escaped");
    }

    #[test]
    fn cusp_lut_tracks_binary_reference_with_bounded_error() {
        for output in [OutputColorSpace::Srgb, OutputColorSpace::DisplayP3] {
            let mut max_chroma_error = 0.0f32;
            let mut max_rgb_error = 0.0f32;
            let mut worst = (0.0f32, 0usize, [0.0; 3], [0.0; 3]);
            for lightness_index in 1..64 {
                let lightness = lightness_index as f32 / 64.0;
                for hue_index in 0..360 {
                    let hue = (hue_index as f32).to_radians();
                    let input = oklab_to_linear_srgb(
                        PerceptualColor {
                            lightness,
                            chroma: 0.65,
                            hue,
                        }
                        .to_oklab(),
                    );
                    let reference = map_to_output_gamut_reference(input, output);
                    let actual = map_to_output_gamut(input, output);
                    assert!(is_in_gamut(actual, output), "{output:?}: {actual:?}");
                    let reference_perceptual =
                        PerceptualColor::from_oklab(linear_srgb_to_oklab(reference));
                    let actual_perceptual =
                        PerceptualColor::from_oklab(linear_srgb_to_oklab(actual));
                    let chroma_error =
                        (actual_perceptual.chroma - reference_perceptual.chroma).abs();
                    if chroma_error > max_chroma_error {
                        max_chroma_error = chroma_error;
                        worst = (lightness, hue_index, actual, reference);
                    }
                    for channel in 0..3 {
                        max_rgb_error =
                            max_rgb_error.max((actual[channel] - reference[channel]).abs());
                    }
                }
            }
            assert!(
                max_chroma_error <= 0.003,
                "{output:?} cusp LUT chroma error {max_chroma_error}, RGB {max_rgb_error}, worst {worst:?}"
            );
            assert!(
                max_rgb_error <= 0.02,
                "{output:?} cusp LUT RGB error {max_rgb_error}, chroma {max_chroma_error}"
            );
        }
    }

    #[test]
    fn cusp_lut_schema_has_the_declared_profile_grid() {
        assert_eq!(cusp_lut(OutputColorSpace::Srgb).len(), CUSP_LUT_LEN);
        assert_eq!(CUSP_LUT_LEN, CUSP_HUE_SAMPLES * CUSP_LIGHTNESS_SAMPLES);
        assert!(cusp_lut(OutputColorSpace::Srgb)
            .iter()
            .all(|value| value.is_finite() && *value >= 0.0));
    }

    /// Engineering comparison requested by Phase 6. The binary mapper remains
    /// the quality reference; the production cusp mapper is the timed path.
    #[test]
    #[ignore = "microbenchmark; run explicitly in release mode"]
    fn benchmark_binary_search_against_cusp_lut_and_rgb_analytic() {
        // Startup owns deterministic LUT construction; time only the hot
        // per-pixel lookup that replaces the old per-fragment search.
        let _ = cusp_lut(OutputColorSpace::Srgb);
        let vectors: Vec<_> = (0..3600)
            .map(|i| {
                let p = PerceptualColor {
                    lightness: 0.1 + 0.8 * ((i % 97) as f32 / 96.0),
                    chroma: 0.5,
                    hue: (i as f32 * 0.1).to_radians(),
                };
                oklab_to_linear_srgb(p.to_oklab())
            })
            .collect();
        let started = Instant::now();
        let binary: Vec<_> = vectors
            .iter()
            .map(|&v| map_to_output_gamut_reference(v, OutputColorSpace::Srgb))
            .collect();
        let binary_time = started.elapsed();
        let started = Instant::now();
        let lut: Vec<_> = vectors
            .iter()
            .map(|&v| map_to_output_gamut(v, OutputColorSpace::Srgb))
            .collect();
        let lut_time = started.elapsed();
        let max_err = binary
            .iter()
            .zip(&lut)
            .flat_map(|(a, b)| (0..3).map(move |c| (a[c] - b[c]).abs()))
            .fold(0.0f32, f32::max);
        eprintln!(
            "binary={binary_time:?} cusp_lookup={lut_time:?} cusp_max_rgb_error={max_err:.6}"
        );
        assert!(max_err.is_finite());
    }
}

//! Hue-preserving output gamut mapping in OKLCh.
//!
//! The mapper is an identity for colours already inside the selected output
//! RGB cube. Out-of-gamut colours keep perceptual lightness and hue while a
//! bounded binary search finds the largest reproducible chroma.

use super::perceptual_color::{linear_srgb_to_oklab, oklab_to_linear_srgb, PerceptualColor};
use super::working_color::{apply_matrix, OutputColorSpace};

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

/// Map a display-linear sRGB/D65 colour to the selected output gamut. The
/// returned values remain represented in linear sRGB for the existing output
/// encoder/monitor-transform boundary.
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
    let original_chroma = color.chroma;
    let mut low = 0.0f32;
    let mut high = original_chroma;
    // Keep this count in sync with `dev_gamut_clip_chroma`. Eighteen steps
    // keep adjacent saturation samples monotone to well below a 16-bit code
    // value without requiring a profile LUT yet.
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

    /// Engineering comparison requested by Phase 4. Binary search is the
    /// quality reference; a future profile-hash cusp LUT may replace it after
    /// interpolation error and GPU texture cost are accepted.
    #[test]
    #[ignore = "microbenchmark; run explicitly in release mode"]
    fn benchmark_binary_search_against_cusp_lut_and_rgb_analytic() {
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
            .map(|&v| map_to_output_gamut(v, OutputColorSpace::Srgb))
            .collect();
        let binary_time = started.elapsed();

        // Coarse 1-degree/0.01-L cusp table prototype, nearest lookup. This is
        // intentionally not production code: it quantifies the contour risk.
        let mut cusp = vec![0.0f32; 101 * 360];
        for li in 0..=100 {
            for hi in 0..360 {
                let p = PerceptualColor {
                    lightness: li as f32 / 100.0,
                    chroma: 0.5,
                    hue: (hi as f32).to_radians(),
                };
                let out =
                    map_to_output_gamut(oklab_to_linear_srgb(p.to_oklab()), OutputColorSpace::Srgb);
                cusp[li * 360 + hi] = PerceptualColor::from_oklab(linear_srgb_to_oklab(out)).chroma;
            }
        }
        let started = Instant::now();
        let lut: Vec<_> = vectors
            .iter()
            .map(|&v| {
                let mut p = PerceptualColor::from_oklab(linear_srgb_to_oklab(v));
                let li = (p.lightness.clamp(0.0, 1.0) * 100.0).round() as usize;
                let hi = p.hue.to_degrees().rem_euclid(360.0).round() as usize % 360;
                p.chroma = p.chroma.min(cusp[li * 360 + hi]);
                oklab_to_linear_srgb(p.to_oklab())
            })
            .collect();
        let lut_time = started.elapsed();
        let started = Instant::now();
        let analytic: Vec<_> = vectors
            .iter()
            .map(|&v| {
                let y = (0.2126 * v[0] + 0.7152 * v[1] + 0.0722 * v[2]).clamp(0.0, 1.0);
                let d = [v[0] - y, v[1] - y, v[2] - y];
                let mut scale = 1.0f32;
                for delta in d {
                    if delta > 1.0e-6 {
                        scale = scale.min((1.0 - y) / delta);
                    } else if delta < -1.0e-6 {
                        scale = scale.min(y / -delta);
                    }
                }
                d.map(|delta| (y + delta * scale.clamp(0.0, 1.0)).clamp(0.0, 1.0))
            })
            .collect();
        let analytic_time = started.elapsed();
        let max_err = binary
            .iter()
            .zip(&lut)
            .flat_map(|(a, b)| (0..3).map(move |c| (a[c] - b[c]).abs()))
            .fold(0.0f32, f32::max);
        let analytic_max_hue_error = binary
            .iter()
            .zip(&analytic)
            .map(|(a, b)| {
                hue_error(
                    PerceptualColor::from_oklab(linear_srgb_to_oklab(*a)),
                    PerceptualColor::from_oklab(linear_srgb_to_oklab(*b)),
                )
            })
            .fold(0.0f32, f32::max);
        eprintln!(
            "binary={binary_time:?} cusp_lookup={lut_time:?} cusp_max_rgb_error={max_err:.6} \
             rgb_analytic={analytic_time:?} analytic_max_hue_error_deg={:.3}",
            analytic_max_hue_error.to_degrees()
        );
        assert!(max_err.is_finite() && analytic_max_hue_error.is_finite());
    }
}

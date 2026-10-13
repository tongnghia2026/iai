//! Reproducible colour-reference and image-quality measurements.
//!
//! This module is instrumentation: it is not called by the render path and
//! cannot change pixels.  CIE L*a*b* values below are the pre-November-2014
//! ColorChecker Classic values published by X-Rite and distributed by the
//! Colour Science project under BSD-3-Clause.  See
//! `docs/color-pipeline/COLOR_ENGINE_REFERENCE_PROVENANCE.md` for the exact
//! source and the external image-corpus provenance.

use super::perceptual_color::linear_srgb_to_oklab;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CieLab {
    pub l: f64,
    pub a: f64,
    pub b: f64,
}

impl CieLab {
    pub const fn new(l: f64, a: f64, b: f64) -> Self {
        Self { l, a, b }
    }

    pub fn chroma(self) -> f64 {
        self.a.hypot(self.b)
    }

    pub fn hue_degrees(self) -> f64 {
        self.b.atan2(self.a).to_degrees().rem_euclid(360.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorCheckerPatch {
    pub name: &'static str,
    pub lab_d50: CieLab,
}

macro_rules! patch {
    ($name:literal, $l:literal, $a:literal, $b:literal) => {
        ColorCheckerPatch {
            name: $name,
            lab_d50: CieLab::new($l, $a, $b),
        }
    };
}

/// ColorChecker Classic 24, row-major (six columns by four rows), under ICC D50.
pub const COLORCHECKER_CLASSIC_D50: [ColorCheckerPatch; 24] = [
    patch!("dark skin", 37.986, 13.555, 14.059),
    patch!("light skin", 65.711, 18.130, 17.810),
    patch!("blue sky", 49.927, -4.880, -21.905),
    patch!("foliage", 43.139, -13.095, 21.905),
    patch!("blue flower", 55.112, 8.844, -25.399),
    patch!("bluish green", 70.719, -33.397, -0.199),
    patch!("orange", 62.661, 36.067, 57.096),
    patch!("purplish blue", 40.020, 10.410, -45.964),
    patch!("moderate red", 51.124, 48.239, 16.248),
    patch!("purple", 30.325, 22.976, -21.587),
    patch!("yellow green", 72.532, -23.709, 57.255),
    patch!("orange yellow", 71.941, 19.363, 67.857),
    patch!("blue", 28.778, 14.179, -50.297),
    patch!("green", 55.261, -38.342, 31.370),
    patch!("red", 42.101, 53.378, 28.190),
    patch!("yellow", 81.733, 4.039, 79.819),
    patch!("magenta", 51.935, 49.986, -14.574),
    patch!("cyan", 51.038, -28.631, -28.638),
    patch!("white 9.5", 96.539, -0.425, 1.186),
    patch!("neutral 8", 81.257, -0.638, -0.335),
    patch!("neutral 6.5", 66.766, -0.734, -0.504),
    patch!("neutral 5", 50.867, -0.153, -0.270),
    patch!("neutral 3.5", 35.656, -0.421, -1.231),
    patch!("black 2", 20.461, -0.079, -0.973),
];

const SRGB_TO_XYZ_D65: [[f64; 3]; 3] = [
    [0.412_456_4, 0.357_576_1, 0.180_437_5],
    [0.212_672_9, 0.715_152_2, 0.072_175_0],
    [0.019_333_9, 0.119_192_0, 0.950_304_1],
];
const XYZ_D65_TO_SRGB: [[f64; 3]; 3] = [
    [3.240_454_2, -1.537_138_5, -0.498_531_4],
    [-0.969_266_0, 1.876_010_8, 0.041_556_0],
    [0.055_643_4, -0.204_025_9, 1.057_225_2],
];
const BRADFORD_D65_TO_D50: [[f64; 3]; 3] = [
    [1.047_811_2, 0.022_886_6, -0.050_127_0],
    [0.029_542_4, 0.990_484_4, -0.017_049_1],
    [-0.009_234_5, 0.015_043_6, 0.752_131_6],
];
const BRADFORD_D50_TO_D65: [[f64; 3]; 3] = [
    [0.955_576_6, -0.023_039_3, 0.063_163_6],
    [-0.028_289_5, 1.009_941_6, 0.021_007_7],
    [0.012_298_2, -0.020_483_0, 1.329_909_8],
];
const D50_XYZ: [f64; 3] = [0.964_22, 1.0, 0.825_21];

#[inline]
fn mat_vec(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

pub fn linear_srgb_to_lab_d50(rgb: [f32; 3]) -> CieLab {
    let xyz_d65 = mat_vec(
        &SRGB_TO_XYZ_D65,
        [rgb[0] as f64, rgb[1] as f64, rgb[2] as f64],
    );
    xyz_d50_to_lab(mat_vec(&BRADFORD_D65_TO_D50, xyz_d65))
}

pub fn encoded_srgb_to_lab_d50(rgb: [f32; 3]) -> CieLab {
    linear_srgb_to_lab_d50(rgb.map(super::develop::srgb_to_linear))
}

pub fn lab_d50_to_linear_srgb(lab: CieLab) -> [f32; 3] {
    let xyz_d65 = mat_vec(&BRADFORD_D50_TO_D65, lab_to_xyz_d50(lab));
    let rgb = mat_vec(&XYZ_D65_TO_SRGB, xyz_d65);
    [rgb[0] as f32, rgb[1] as f32, rgb[2] as f32]
}

fn xyz_d50_to_lab(xyz: [f64; 3]) -> CieLab {
    const EPSILON: f64 = 216.0 / 24_389.0;
    const KAPPA: f64 = 24_389.0 / 27.0;
    let f = |t: f64| {
        if t > EPSILON {
            t.cbrt()
        } else {
            (KAPPA * t + 16.0) / 116.0
        }
    };
    let fx = f(xyz[0] / D50_XYZ[0]);
    let fy = f(xyz[1] / D50_XYZ[1]);
    let fz = f(xyz[2] / D50_XYZ[2]);
    CieLab::new(116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))
}

fn lab_to_xyz_d50(lab: CieLab) -> [f64; 3] {
    const EPSILON: f64 = 216.0 / 24_389.0;
    const KAPPA: f64 = 24_389.0 / 27.0;
    let fy = (lab.l + 16.0) / 116.0;
    let fx = fy + lab.a / 500.0;
    let fz = fy - lab.b / 200.0;
    let inverse = |f: f64| {
        let cube = f * f * f;
        if cube > EPSILON {
            cube
        } else {
            (116.0 * f - 16.0) / KAPPA
        }
    };
    [
        D50_XYZ[0] * inverse(fx),
        D50_XYZ[1] * inverse(fy),
        D50_XYZ[2] * inverse(fz),
    ]
}

/// CIEDE2000 colour difference with unit weighting factors.
pub fn delta_e_2000(first: CieLab, second: CieLab) -> f64 {
    let c1 = first.chroma();
    let c2 = second.chroma();
    let c_bar = (c1 + c2) * 0.5;
    let c7 = c_bar.powi(7);
    let g = 0.5 * (1.0 - (c7 / (c7 + 25.0_f64.powi(7))).sqrt());
    let a1p = (1.0 + g) * first.a;
    let a2p = (1.0 + g) * second.a;
    let c1p = a1p.hypot(first.b);
    let c2p = a2p.hypot(second.b);
    let hp = |a: f64, b: f64| {
        if a == 0.0 && b == 0.0 {
            0.0
        } else {
            b.atan2(a).to_degrees().rem_euclid(360.0)
        }
    };
    let h1p = hp(a1p, first.b);
    let h2p = hp(a2p, second.b);
    let delta_lp = second.l - first.l;
    let delta_cp = c2p - c1p;
    let dh = if c1p * c2p == 0.0 {
        0.0
    } else if (h2p - h1p).abs() <= 180.0 {
        h2p - h1p
    } else if h2p <= h1p {
        h2p - h1p + 360.0
    } else {
        h2p - h1p - 360.0
    };
    let delta_hp = 2.0 * (c1p * c2p).sqrt() * (0.5 * dh.to_radians()).sin();
    let l_bar = (first.l + second.l) * 0.5;
    let c_bar_p = (c1p + c2p) * 0.5;
    let h_bar = if c1p * c2p == 0.0 {
        h1p + h2p
    } else if (h1p - h2p).abs() <= 180.0 {
        (h1p + h2p) * 0.5
    } else if h1p + h2p < 360.0 {
        (h1p + h2p + 360.0) * 0.5
    } else {
        (h1p + h2p - 360.0) * 0.5
    };
    let t = 1.0 - 0.17 * (h_bar - 30.0).to_radians().cos()
        + 0.24 * (2.0 * h_bar).to_radians().cos()
        + 0.32 * (3.0 * h_bar + 6.0).to_radians().cos()
        - 0.20 * (4.0 * h_bar - 63.0).to_radians().cos();
    let delta_theta = 30.0 * (-((h_bar - 275.0) / 25.0).powi(2)).exp();
    let c_bar_p7 = c_bar_p.powi(7);
    let rc = 2.0 * (c_bar_p7 / (c_bar_p7 + 25.0_f64.powi(7))).sqrt();
    let sl = 1.0 + 0.015 * (l_bar - 50.0).powi(2) / (20.0 + (l_bar - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * c_bar_p;
    let sh = 1.0 + 0.015 * c_bar_p * t;
    let rt = -rc * (2.0 * delta_theta.to_radians()).sin();
    let dl = delta_lp / sl;
    let dc = delta_cp / sc;
    let dh = delta_hp / sh;
    (dl * dl + dc * dc + dh * dh + rt * dc * dh).max(0.0).sqrt()
}

#[derive(Clone, Debug, PartialEq)]
pub struct PatchMetric {
    pub name: &'static str,
    pub measured_lab_d50: CieLab,
    pub delta_e_2000: f64,
    /// Euclidean OKLab distance. Values are on OKLab's native 0..1 scale.
    pub delta_e_ok: f64,
    pub hue_error_degrees: f64,
    pub chroma_drift: f64,
    pub lightness_drift: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ColorCheckerSummary {
    pub exposure_scale: f32,
    pub mean_delta_e_2000: f64,
    pub p95_delta_e_2000: f64,
    pub max_delta_e_2000: f64,
    pub mean_delta_e_ok: f64,
    pub p95_delta_e_ok: f64,
    pub mean_hue_error_degrees: f64,
    pub mean_chroma_drift: f64,
    pub mean_lightness_drift: f64,
    /// Fraction of exposure-normalized patch means outside the nominal sRGB
    /// cube. This is an out-of-gamut diagnostic, not source-pixel clipping.
    pub out_of_range_patch_mean_fraction: f64,
    pub patches: Vec<PatchMetric>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReferenceError {
    PatchCount { expected: usize, actual: usize },
    NonFinite,
}

/// Fit only a scalar scene exposure from the six neutral patches.  No channel
/// gains, matrix, curve, or chroma fit is permitted: colour error remains visible.
pub fn neutral_exposure_scale(measured_linear_srgb: &[[f32; 3]]) -> Result<f32, ReferenceError> {
    if measured_linear_srgb.len() != COLORCHECKER_CLASSIC_D50.len() {
        return Err(ReferenceError::PatchCount {
            expected: COLORCHECKER_CLASSIC_D50.len(),
            actual: measured_linear_srgb.len(),
        });
    }
    let mut ratios = Vec::with_capacity(6);
    for (rgb, reference) in measured_linear_srgb[18..]
        .iter()
        .zip(&COLORCHECKER_CLASSIC_D50[18..])
    {
        let y = SRGB_TO_XYZ_D65[1][0] * rgb[0] as f64
            + SRGB_TO_XYZ_D65[1][1] * rgb[1] as f64
            + SRGB_TO_XYZ_D65[1][2] * rgb[2] as f64;
        let reference_rgb = lab_d50_to_linear_srgb(reference.lab_d50);
        let target_y = SRGB_TO_XYZ_D65[1][0] * reference_rgb[0] as f64
            + SRGB_TO_XYZ_D65[1][1] * reference_rgb[1] as f64
            + SRGB_TO_XYZ_D65[1][2] * reference_rgb[2] as f64;
        if y.is_finite() && y > 1.0e-9 {
            ratios.push(target_y / y);
        }
    }
    if ratios.is_empty() || ratios.iter().any(|v| !v.is_finite()) {
        return Err(ReferenceError::NonFinite);
    }
    ratios.sort_by(f64::total_cmp);
    let middle = ratios.len() / 2;
    let scale = if ratios.len() % 2 == 0 {
        (ratios[middle - 1] + ratios[middle]) * 0.5
    } else {
        ratios[middle]
    };
    Ok(scale as f32)
}

pub fn evaluate_colorchecker_linear(
    measured_linear_srgb: &[[f32; 3]],
    normalize_exposure: bool,
) -> Result<ColorCheckerSummary, ReferenceError> {
    if measured_linear_srgb.len() != COLORCHECKER_CLASSIC_D50.len() {
        return Err(ReferenceError::PatchCount {
            expected: COLORCHECKER_CLASSIC_D50.len(),
            actual: measured_linear_srgb.len(),
        });
    }
    if measured_linear_srgb
        .iter()
        .flatten()
        .any(|v| !v.is_finite())
    {
        return Err(ReferenceError::NonFinite);
    }
    let exposure_scale = if normalize_exposure {
        neutral_exposure_scale(measured_linear_srgb)?
    } else {
        1.0
    };
    let mut patches = Vec::with_capacity(24);
    let mut clipped = 0usize;
    for (&rgb, reference) in measured_linear_srgb.iter().zip(COLORCHECKER_CLASSIC_D50) {
        let rgb = rgb.map(|v| v * exposure_scale);
        if rgb.iter().any(|&v| !(0.0..=1.0).contains(&v)) {
            clipped += 1;
        }
        let measured = linear_srgb_to_lab_d50(rgb);
        let measured_ok = linear_srgb_to_oklab(rgb);
        let reference_rgb = lab_d50_to_linear_srgb(reference.lab_d50);
        let reference_ok = linear_srgb_to_oklab(reference_rgb);
        let delta_e_ok = ((measured_ok.l - reference_ok.l).powi(2)
            + (measured_ok.a - reference_ok.a).powi(2)
            + (measured_ok.b - reference_ok.b).powi(2))
        .sqrt() as f64;
        let hue_error_degrees = if reference.lab_d50.chroma() < 2.0 {
            0.0
        } else {
            let gap = (measured.hue_degrees() - reference.lab_d50.hue_degrees()).abs();
            gap.min(360.0 - gap)
        };
        patches.push(PatchMetric {
            name: reference.name,
            measured_lab_d50: measured,
            delta_e_2000: delta_e_2000(measured, reference.lab_d50),
            delta_e_ok,
            hue_error_degrees,
            chroma_drift: measured.chroma() - reference.lab_d50.chroma(),
            lightness_drift: measured.l - reference.lab_d50.l,
        });
    }
    let mean =
        |f: fn(&PatchMetric) -> f64| patches.iter().map(f).sum::<f64>() / patches.len() as f64;
    let percentile = |f: fn(&PatchMetric) -> f64, fraction: f64| {
        let mut values = patches.iter().map(f).collect::<Vec<_>>();
        values.sort_by(f64::total_cmp);
        let index = ((values.len() as f64 * fraction).ceil() as usize)
            .saturating_sub(1)
            .min(values.len() - 1);
        values[index]
    };
    Ok(ColorCheckerSummary {
        exposure_scale,
        mean_delta_e_2000: mean(|p| p.delta_e_2000),
        p95_delta_e_2000: percentile(|p| p.delta_e_2000, 0.95),
        max_delta_e_2000: patches.iter().map(|p| p.delta_e_2000).fold(0.0, f64::max),
        mean_delta_e_ok: mean(|p| p.delta_e_ok),
        p95_delta_e_ok: percentile(|p| p.delta_e_ok, 0.95),
        mean_hue_error_degrees: patches[..18]
            .iter()
            .map(|p| p.hue_error_degrees)
            .sum::<f64>()
            / 18.0,
        mean_chroma_drift: mean(|p| p.chroma_drift),
        mean_lightness_drift: mean(|p| p.lightness_drift),
        out_of_range_patch_mean_fraction: clipped as f64 / patches.len() as f64,
        patches,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RampMetrics {
    pub distinct_quantized_levels: usize,
    pub mean_step: f64,
    pub step_variance: f64,
    pub max_abs_step: f64,
    pub reversals: usize,
}

pub fn analyze_ramp(samples: &[f32], quantization_levels: u32) -> RampMetrics {
    if samples.is_empty() {
        return RampMetrics::default();
    }
    let mut codes = samples
        .iter()
        .map(|&v| (v.clamp(0.0, 1.0) * quantization_levels as f32).round() as u32)
        .collect::<Vec<_>>();
    codes.sort_unstable();
    codes.dedup();
    let steps = samples
        .windows(2)
        .map(|p| (p[1] - p[0]) as f64)
        .collect::<Vec<_>>();
    if steps.is_empty() {
        return RampMetrics {
            distinct_quantized_levels: codes.len(),
            ..Default::default()
        };
    }
    let mean_step = steps.iter().sum::<f64>() / steps.len() as f64;
    RampMetrics {
        distinct_quantized_levels: codes.len(),
        mean_step,
        step_variance: steps.iter().map(|v| (v - mean_step).powi(2)).sum::<f64>()
            / steps.len() as f64,
        max_abs_step: steps.iter().copied().map(f64::abs).fold(0.0, f64::max),
        reversals: steps.iter().filter(|&&v| v < -1.0e-9).count(),
    }
}

/// Variance of the four-neighbour Laplacian, a deterministic acutance proxy.
pub fn laplacian_acutance(luma: &[f32], width: usize, height: usize) -> Option<f64> {
    if width < 3 || height < 3 || luma.len() != width * height {
        return None;
    }
    let mut sum = 0.0f64;
    let mut sum_sq = 0.0f64;
    let mut count = 0usize;
    for y in 1..height - 1 {
        for x in 1..width - 1 {
            let i = y * width + x;
            let lap = 4.0 * luma[i] - luma[i - 1] - luma[i + 1] - luma[i - width] - luma[i + width];
            let lap = lap as f64;
            sum += lap;
            sum_sq += lap * lap;
            count += 1;
        }
    }
    let mean = sum / count as f64;
    Some((sum_sq / count as f64 - mean * mean).max(0.0))
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ImageQualitySummary {
    pub mean_oklab_lightness: f64,
    pub mean_oklab_chroma: f64,
    pub shadow_chroma: f64,
    pub midtone_chroma: f64,
    pub highlight_chroma: f64,
    pub clipped_pixel_fraction: f64,
    pub laplacian_acutance: f64,
}

/// Summarize an opaque encoded-sRGB RGBA16 image without subject registration.
/// This is a no-reference diagnostic, not a likeness or colour-accuracy score.
/// Acutance uses perceptual OKLab L at a matched output size. Transparent input
/// is rejected because alpha boundaries would create false Laplacian edges.
pub fn summarize_encoded_rgba16(
    rgba: &[u16],
    width: usize,
    height: usize,
) -> Option<ImageQualitySummary> {
    if width < 3
        || height < 3
        || rgba.len() != width * height * 4
        || rgba.chunks_exact(4).any(|pixel| pixel[3] != u16::MAX)
    {
        return None;
    }
    let mut luma = vec![0.0f32; width * height];
    let mut sum_l = 0.0f64;
    let mut sum_c = 0.0f64;
    let mut bin_sum = [0.0f64; 3];
    let mut bin_count = [0usize; 3];
    let mut clipped = 0usize;
    let mut count = 0usize;
    for (index, pixel) in rgba.chunks_exact(4).enumerate() {
        let encoded = [
            pixel[0] as f32 / 65_535.0,
            pixel[1] as f32 / 65_535.0,
            pixel[2] as f32 / 65_535.0,
        ];
        if pixel[..3]
            .iter()
            .any(|&channel| channel <= 1 || channel >= 65_534)
        {
            clipped += 1;
        }
        let lab = linear_srgb_to_oklab(encoded.map(super::develop::srgb_to_linear));
        let chroma = lab.a.hypot(lab.b);
        luma[index] = lab.l;
        sum_l += lab.l as f64;
        sum_c += chroma as f64;
        let bin = if lab.l < 0.25 {
            0
        } else if lab.l <= 0.80 {
            1
        } else {
            2
        };
        bin_sum[bin] += chroma as f64;
        bin_count[bin] += 1;
        count += 1;
    }
    if count == 0 {
        return None;
    }
    let bin_mean = |index: usize| {
        if bin_count[index] == 0 {
            0.0
        } else {
            bin_sum[index] / bin_count[index] as f64
        }
    };
    Some(ImageQualitySummary {
        mean_oklab_lightness: sum_l / count as f64,
        mean_oklab_chroma: sum_c / count as f64,
        shadow_chroma: bin_mean(0),
        midtone_chroma: bin_mean(1),
        highlight_chroma: bin_mean(2),
        clipped_pixel_fraction: clipped as f64 / count as f64,
        laplacian_acutance: laplacian_acutance(&luma, width, height).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ciede2000_matches_all_published_sharma_vectors() {
        // G. Sharma, W. Wu, E. Dalal, supplementary CIEDE2000 test data:
        // https://hajim.rochester.edu/ece/sites/gsharma/ciede2000/
        let vectors = [
            (50.0000, 2.6772, -79.7751, 50.0000, 0.0000, -82.7485, 2.0425),
            (50.0000, 3.1571, -77.2803, 50.0000, 0.0000, -82.7485, 2.8615),
            (50.0000, 2.8361, -74.0200, 50.0000, 0.0000, -82.7485, 3.4412),
            (
                50.0000, -1.3802, -84.2814, 50.0000, 0.0000, -82.7485, 1.0000,
            ),
            (
                50.0000, -1.1848, -84.8006, 50.0000, 0.0000, -82.7485, 1.0000,
            ),
            (
                50.0000, -0.9009, -85.5211, 50.0000, 0.0000, -82.7485, 1.0000,
            ),
            (50.0000, 0.0000, 0.0000, 50.0000, -1.0000, 2.0000, 2.3669),
            (50.0000, -1.0000, 2.0000, 50.0000, 0.0000, 0.0000, 2.3669),
            (50.0000, 2.4900, -0.0010, 50.0000, -2.4900, 0.0009, 7.1792),
            (50.0000, 2.4900, -0.0010, 50.0000, -2.4900, 0.0010, 7.1792),
            (50.0000, 2.4900, -0.0010, 50.0000, -2.4900, 0.0011, 7.2195),
            (50.0000, 2.4900, -0.0010, 50.0000, -2.4900, 0.0012, 7.2195),
            (50.0000, -0.0010, 2.4900, 50.0000, 0.0009, -2.4900, 4.8045),
            (50.0000, -0.0010, 2.4900, 50.0000, 0.0010, -2.4900, 4.8045),
            (50.0000, -0.0010, 2.4900, 50.0000, 0.0011, -2.4900, 4.7461),
            (50.0000, 2.5000, 0.0000, 50.0000, 0.0000, -2.5000, 4.3065),
            (50.0000, 2.5000, 0.0000, 73.0000, 25.0000, -18.0000, 27.1492),
            (50.0000, 2.5000, 0.0000, 61.0000, -5.0000, 29.0000, 22.8977),
            (50.0000, 2.5000, 0.0000, 56.0000, -27.0000, -3.0000, 31.9030),
            (50.0000, 2.5000, 0.0000, 58.0000, 24.0000, 15.0000, 19.4535),
            (50.0000, 2.5000, 0.0000, 50.0000, 3.1736, 0.5854, 1.0000),
            (50.0000, 2.5000, 0.0000, 50.0000, 3.2972, 0.0000, 1.0000),
            (50.0000, 2.5000, 0.0000, 50.0000, 1.8634, 0.5757, 1.0000),
            (50.0000, 2.5000, 0.0000, 50.0000, 3.2592, 0.3350, 1.0000),
            (
                60.2574, -34.0099, 36.2677, 60.4626, -34.1751, 39.4387, 1.2644,
            ),
            (
                63.0109, -31.0961, -5.8663, 62.8187, -29.7946, -4.0864, 1.2630,
            ),
            (61.2901, 3.7196, -5.3901, 61.4292, 2.2480, -4.9620, 1.8731),
            (35.0831, -44.1164, 3.7933, 35.0232, -40.0716, 1.5901, 1.8645),
            (
                22.7233, 20.0904, -46.6940, 23.0331, 14.9730, -42.5619, 2.0373,
            ),
            (36.4612, 47.8580, 18.3852, 36.2715, 50.5065, 21.2231, 1.4146),
            (90.8027, -2.0831, 1.4410, 91.1528, -1.6435, 0.0447, 1.4441),
            (90.9257, -0.5406, -0.9208, 88.6381, -0.8985, -0.7239, 1.5381),
            (6.7747, -0.2908, -2.4247, 5.8714, -0.0985, -2.2286, 0.6377),
            (2.0776, 0.0795, -1.1350, 0.9033, -0.0636, -0.5514, 0.9082),
        ];
        for &(l1, a1, b1, l2, a2, b2, expected) in &vectors {
            let actual = delta_e_2000(CieLab::new(l1, a1, b1), CieLab::new(l2, a2, b2));
            assert!(
                (actual - expected).abs() < 1.0e-4,
                "expected {expected}, got {actual} for ({l1}, {a1}, {b1}) / ({l2}, {a2}, {b2})"
            );
        }
    }

    #[test]
    fn colorchecker_reference_roundtrip_is_near_zero() {
        let rgb = COLORCHECKER_CLASSIC_D50
            .iter()
            .map(|patch| lab_d50_to_linear_srgb(patch.lab_d50))
            .collect::<Vec<_>>();
        let report = evaluate_colorchecker_linear(&rgb, false).unwrap();
        assert!(report.max_delta_e_2000 < 2.0e-4, "{report:?}");
        assert!(report.p95_delta_e_ok < 2.0e-6, "{report:?}");
    }

    #[test]
    fn neutral_fit_recovers_scalar_exposure_without_fitting_color() {
        let rgb = COLORCHECKER_CLASSIC_D50
            .iter()
            .map(|patch| lab_d50_to_linear_srgb(patch.lab_d50).map(|v| v * 0.25))
            .collect::<Vec<_>>();
        let report = evaluate_colorchecker_linear(&rgb, true).unwrap();
        assert!((report.exposure_scale - 4.0).abs() < 1.0e-3);
        assert!(report.max_delta_e_2000 < 2.0e-3);
    }

    #[test]
    fn ramp_and_acutance_metrics_detect_regressions() {
        let ramp = (0..1024).map(|i| i as f32 / 1023.0).collect::<Vec<_>>();
        let metrics = analyze_ramp(&ramp, 65_535);
        assert_eq!(metrics.distinct_quantized_levels, 1024);
        assert_eq!(metrics.reversals, 0);
        assert!(metrics.step_variance < 1.0e-12);

        let flat = vec![0.5f32; 25];
        let mut edge = flat.clone();
        for y in 0..5 {
            for x in 3..5 {
                edge[y * 5 + x] = 1.0;
            }
        }
        assert_eq!(laplacian_acutance(&flat, 5, 5), Some(0.0));
        assert!(laplacian_acutance(&edge, 5, 5).unwrap() > 0.0);
    }

    #[test]
    fn image_summary_separates_chroma_and_detail() {
        let mut flat = vec![0u16; 5 * 5 * 4];
        for (index, pixel) in flat.chunks_exact_mut(4).enumerate() {
            let value = if index % 5 < 3 { 16_000 } else { 48_000 };
            pixel.copy_from_slice(&[value, value, value, u16::MAX]);
        }
        let neutral = summarize_encoded_rgba16(&flat, 5, 5).unwrap();
        assert!(neutral.mean_oklab_chroma < 1.0e-5);
        assert!(neutral.laplacian_acutance > 0.0);

        for pixel in flat.chunks_exact_mut(4) {
            pixel[0] = pixel[0].saturating_add(4_000);
        }
        let colored = summarize_encoded_rgba16(&flat, 5, 5).unwrap();
        assert!(colored.mean_oklab_chroma > neutral.mean_oklab_chroma + 0.005);

        flat[3] = 0;
        assert!(summarize_encoded_rgba16(&flat, 5, 5).is_none());
    }
}

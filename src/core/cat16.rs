//! CAT16 chromatic adaptation for the Develop white-balance stage.
//!
//! The old white balance was three ad-hoc channel gains in sRGB. A real WB move
//! is a change of assumed illuminant: the Temperature/Tint sliders pick an
//! illuminant on a Planckian/daylight locus (± a Duv offset toward
//! green/magenta) and
//! the pixel is adapted from that illuminant to D65 with the CAT16 transform —
//! the standard CAT16 chromatic-adaptation model. Everything composes into
//! ONE 3×3 matrix over linear-sRGB, built once per slider change, so the
//! per-pixel cost is a single matrix multiply on both CPU and GPU.
//!
//! Slider mapping:
//! Temperature ±200 traverses 1800–25000 K in piecewise-linear mired space
//! around D65 (mired keeps the perceptual step per slider tick roughly
//! constant while retaining both tungsten and cool-daylight endpoints);
//! Tint ±200 → ∓`TINT_DUV_RANGE` Duv (perpendicular to the locus).
//! (0, 0) is EXACTLY the identity matrix: the source illuminant is computed
//! through the same locus code as the D65 reference, so the gains cancel and a
//! neutral setting is a true no-op (`is_neutral` semantics).

use crate::core::develop::{control_to_unit, CONTROL_LIMIT};

/// D65 correlated colour temperature (K). The locus is evaluated here for the
/// reference white so temp = 0 cancels exactly.
pub const D65_CCT_KELVIN: f32 = 6503.6;
pub const MIN_CCT_KELVIN: f32 = 1800.0;
pub const MAX_CCT_KELVIN: f32 = 25_000.0;
/// Tint slider range as a CIE-1960 Duv offset (±). 0.05 spans a strong
/// green↔magenta correction (fluorescent-grade).
const TINT_DUV_RANGE: f32 = 0.05;
/// Reachable Temperature-slider endpoints. The physical clamps above stay wide
/// so the eyedropper and Kelvin readout can express any recovered as-shot white,
/// but the ±200 control only travels between these: a linear-sRGB von-Kries
/// adaptation toward candlelight explodes the blue channel gain (>7× at 1800 K),
/// driving red negative and clipping the frame to a flat purple (the reported
/// bug). Tungsten (~2850 K, peak element ≈2.9) is the usable warm limit; the cool
/// end stays gentle (peak ≈1.08) so it keeps the full clamp.
const SLIDER_WARM_CCT: f32 = 2850.0;
const SLIDER_COOL_CCT: f32 = MAX_CCT_KELVIN;

/// Absolute white-balance coordinates independent of the ±200 UI transport.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WhiteBalance {
    pub cct_kelvin: f32,
    pub duv: f32,
}

/// CAM16 cone-response matrix (XYZ → RGB_c).
const M16: [[f32; 3]; 3] = [
    [0.401288, 0.650173, -0.051461],
    [-0.250268, 1.204414, 0.045854],
    [-0.002079, 0.048952, 0.953127],
];
/// Inverse of `M16`.
const M16_INV: [[f32; 3]; 3] = [
    [1.862068, -1.011255, 0.149187],
    [0.387527, 0.621447, -0.008974],
    [-0.015841, -0.034123, 1.049964],
];

/// Linear sRGB (D65) → XYZ.
const XYZ_FROM_SRGB: [[f32; 3]; 3] = [
    [0.4124564, 0.3575761, 0.1804375],
    [0.2126729, 0.7151522, 0.0721750],
    [0.0193339, 0.1191920, 0.9503041],
];
/// XYZ → linear sRGB (D65).
const SRGB_FROM_XYZ: [[f32; 3]; 3] = [
    [3.2404542, -1.5371385, -0.4985314],
    [-0.9692660, 1.8760108, 0.0415560],
    [0.0556434, -0.2040259, 1.0572252],
];

fn mat_mul(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = [[0.0f32; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    out
}

fn mat_inverse(m: &[[f32; 3]; 3]) -> Option<[[f32; 3]; 3]> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if !det.is_finite() || det.abs() < 1.0e-8 {
        return None;
    }
    let d = 1.0 / det;
    Some([
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * d,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * d,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * d,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * d,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * d,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * d,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * d,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * d,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * d,
        ],
    ])
}

#[inline]
pub fn mat_apply(m: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// CIE daylight-locus chromaticity for a CCT in [4000, 25000] K. The slider
/// range stays well inside that validity window.
fn daylight_xy(cct: f32) -> (f32, f32) {
    let t = cct.clamp(4000.0, 25000.0) as f64;
    let x = if t <= 7000.0 {
        -4.607e9 / (t * t * t) + 2.9678e6 / (t * t) + 0.09911e3 / t + 0.244063
    } else {
        -2.0064e9 / (t * t * t) + 1.9018e6 / (t * t) + 0.24748e3 / t + 0.237040
    };
    let y = -3.000 * x * x + 2.870 * x - 0.275;
    (x as f32, y as f32)
}

/// Smooth analytic approximation of the CIE 1931 2° colour matching
/// functions. It is evaluated at 1 nm intervals below, avoiding an embedded
/// proprietary/table resource while retaining an actual spectral integration.
fn cie_1931_xyz_bar(wavelength_nm: f64) -> [f64; 3] {
    let gaussian = |center: f64, left: f64, right: f64| {
        let scale = if wavelength_nm < center { left } else { right };
        let t = (wavelength_nm - center) * scale;
        (-0.5 * t * t).exp()
    };
    [
        0.362 * gaussian(442.0, 0.0624, 0.0374) + 1.056 * gaussian(599.8, 0.0264, 0.0323)
            - 0.065 * gaussian(501.1, 0.0490, 0.0382),
        0.821 * gaussian(568.8, 0.0213, 0.0247) + 0.286 * gaussian(530.9, 0.0613, 0.0322),
        1.217 * gaussian(437.0, 0.0845, 0.0278) + 0.681 * gaussian(459.0, 0.0385, 0.0725),
    ]
}

/// Planckian-locus chromaticity from spectral radiance integrated against the
/// CIE 1931 2° observer. Only relative radiance matters after XYZ
/// normalisation, so the leading Planck constant cancels.
fn planckian_xy(cct: f32) -> (f32, f32) {
    const C2: f64 = 1.438_776_877e-2;
    let temperature = cct.clamp(MIN_CCT_KELVIN, 5000.0) as f64;
    let mut xyz = [0.0f64; 3];
    for wavelength_nm in 360..=830 {
        let wavelength_m = wavelength_nm as f64 * 1.0e-9;
        let exponent = C2 / (wavelength_m * temperature);
        let spectral = 1.0 / (wavelength_m.powi(5) * exponent.exp_m1());
        let observer = cie_1931_xyz_bar(wavelength_nm as f64);
        for channel in 0..3 {
            xyz[channel] += spectral * observer[channel];
        }
    }
    let sum = xyz.iter().sum::<f64>().max(f64::MIN_POSITIVE);
    ((xyz[0] / sum) as f32, (xyz[1] / sum) as f32)
}

/// xy → CIE 1960 uv (the space Duv is defined in).
fn xy_to_uv(x: f32, y: f32) -> (f32, f32) {
    let d = -2.0 * x + 12.0 * y + 3.0;
    (4.0 * x / d, 6.0 * y / d)
}

/// CIE 1960 uv → xy.
fn uv_to_xy(u: f32, v: f32) -> (f32, f32) {
    let d = 2.0 * u - 8.0 * v + 4.0;
    (3.0 * u / d, 2.0 * v / d)
}

/// Warm illuminants follow the physical blackbody locus; daylight illuminants
/// follow the CIE daylight locus. Blending in uniform uv over 3900–4100 K keeps
/// both value and tangent continuous at the model boundary.
fn locus_xy(cct: f32) -> (f32, f32) {
    let cct = cct.clamp(MIN_CCT_KELVIN, MAX_CCT_KELVIN);
    if cct <= 3900.0 {
        return planckian_xy(cct);
    }
    if cct >= 4100.0 {
        return daylight_xy(cct);
    }
    let (pu, pv) = {
        let (x, y) = planckian_xy(cct);
        xy_to_uv(x, y)
    };
    let (du, dv) = {
        let (x, y) = daylight_xy(cct);
        xy_to_uv(x, y)
    };
    let x = ((cct - 3900.0) / 200.0).clamp(0.0, 1.0);
    let mix = x * x * (3.0 - 2.0 * x);
    uv_to_xy(pu + (du - pu) * mix, pv + (dv - pv) * mix)
}

/// Illuminant chromaticity for a CCT plus a Duv offset perpendicular to the
/// locus (positive Duv = toward green, the CIE convention).
fn illuminant_xy(cct: f32, duv: f32) -> (f32, f32) {
    let cct = cct.clamp(MIN_CCT_KELVIN, MAX_CCT_KELVIN);
    let (x0, y0) = locus_xy(cct);
    if duv.abs() < 1e-7 {
        return (x0, y0);
    }
    let (u0, v0) = xy_to_uv(x0, y0);
    // Locus tangent from a small CCT step; the normal (rotated +90°) points
    // toward green above the locus.
    let (x1, y1) = locus_xy((cct * 1.002).min(MAX_CCT_KELVIN));
    let (u1, v1) = xy_to_uv(x1, y1);
    let (mut tu, mut tv) = (u1 - u0, v1 - v0);
    let len = (tu * tu + tv * tv).sqrt().max(1e-9);
    tu /= len;
    tv /= len;
    // The locus tangent toward higher CCT points down-left in uv; rotating it
    // −90° gives the normal on the GREEN side (+v), the CIE +Duv convention.
    uv_to_xy(u0 + tv * duv, v0 - tu * duv)
}

/// Historical daylight-only locus used by Scene1 projects. Keep its tangent
/// step and clamp untouched so saved non-zero slider values remain colour
/// compatible after Develop2 adopts the Planckian model.
fn legacy_illuminant_xy(cct: f32, duv: f32) -> (f32, f32) {
    let (x0, y0) = daylight_xy(cct);
    if duv.abs() < 1e-7 {
        return (x0, y0);
    }
    let (u0, v0) = xy_to_uv(x0, y0);
    let (x1, y1) = daylight_xy(cct * 1.01);
    let (u1, v1) = xy_to_uv(x1, y1);
    let (mut tu, mut tv) = (u1 - u0, v1 - v0);
    let len = (tu * tu + tv * tv).sqrt().max(1e-9);
    tu /= len;
    tv /= len;
    uv_to_xy(u0 + tv * duv, v0 - tu * duv)
}

fn xy_to_xyz(x: f32, y: f32) -> [f32; 3] {
    let y = y.max(1e-6);
    [x / y, 1.0, (1.0 - x - y) / y]
}

/// Rec.709 luma of the image a matrix produces for white input, used to
/// normalise so a neutral grey keeps its brightness (only the cast moves).
fn white_luma(m: &[[f32; 3]; 3]) -> f32 {
    let w = mat_apply(m, [1.0, 1.0, 1.0]);
    0.2126 * w[0] + 0.7152 * w[1] + 0.0722 * w[2]
}

/// Normalise a white-balance matrix so a neutral grey keeps its brightness
/// (only the colour cast moves). Returns identity·luma untouched at neutral.
fn normalize_wb_luma(mut m: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let luma = white_luma(&m);
    if luma > 1e-6 {
        for row in &mut m {
            for value in row {
                *value /= luma;
            }
        }
    }
    m
}

/// Convert the existing ±200 controls to absolute CCT/Duv coordinates around
/// a concrete scene's as-shot source white.
pub fn sliders_to_white_balance_from_base(
    base: WhiteBalance,
    temperature: f32,
    tint: f32,
) -> WhiteBalance {
    // Mired math runs in f64: the warm endpoint (tungsten) can sit within a few
    // mired of an already-warm as-shot base, and an f32 subtraction there loses
    // enough precision to break the slider↔white round-trip.
    let unit = control_to_unit(temperature) as f64;
    let base_cct = base.cct_kelvin.clamp(MIN_CCT_KELVIN, MAX_CCT_KELVIN) as f64;
    let base_mired = 1.0e6 / base_cct;
    let mired = if unit >= 0.0 {
        let cold_limit = 1.0e6 / SLIDER_COOL_CCT as f64;
        base_mired - unit * (base_mired - cold_limit)
    } else {
        let warm_limit = 1.0e6 / SLIDER_WARM_CCT as f64;
        base_mired + -unit * (warm_limit - base_mired)
    };
    // Tint is a SYMMETRIC Duv offset around the scene's as-shot green/magenta
    // cast. The previous `base.duv + unit*(RANGE - base.duv)` mapped to the
    // absolute ±RANGE endpoints, so a camera whose as-shot neutral already sat
    // near +RANGE lost all +tint travel (the reported "tint does nothing").
    let tint_unit = control_to_unit(tint);
    let duv =
        (base.duv + tint_unit * TINT_DUV_RANGE).clamp(-2.0 * TINT_DUV_RANGE, 2.0 * TINT_DUV_RANGE);
    WhiteBalance {
        cct_kelvin: (1.0e6 / mired as f32).clamp(MIN_CCT_KELVIN, MAX_CCT_KELVIN),
        duv,
    }
}

/// Inverse of [`sliders_to_white_balance_from_base`].
pub fn white_balance_to_sliders_from_base(base: WhiteBalance, white: WhiteBalance) -> (f32, f32) {
    let cct = white.cct_kelvin.clamp(MIN_CCT_KELVIN, MAX_CCT_KELVIN) as f64;
    let mired = 1.0e6 / cct;
    let base_mired = 1.0e6 / base.cct_kelvin.clamp(MIN_CCT_KELVIN, MAX_CCT_KELVIN) as f64;
    let temperature_unit = if mired <= base_mired {
        let cold_limit = 1.0e6 / SLIDER_COOL_CCT as f64;
        (base_mired - mired) / (base_mired - cold_limit).max(1.0e-6)
    } else {
        let warm_limit = 1.0e6 / SLIDER_WARM_CCT as f64;
        -(mired - base_mired) / (warm_limit - base_mired).max(1.0e-6)
    };
    // Inverse of the symmetric tint offset above.
    let tint_unit = (white.duv - base.duv) / TINT_DUV_RANGE;
    (
        (temperature_unit as f32 * CONTROL_LIMIT).clamp(-CONTROL_LIMIT, CONTROL_LIMIT),
        (tint_unit * CONTROL_LIMIT).clamp(-CONTROL_LIMIT, CONTROL_LIMIT),
    )
}

/// D65-based convenience mapping for non-RAW/synthetic scenes.
pub fn sliders_to_white_balance(temperature: f32, tint: f32) -> WhiteBalance {
    sliders_to_white_balance_from_base(
        WhiteBalance {
            cct_kelvin: D65_CCT_KELVIN,
            duv: 0.0,
        },
        temperature,
        tint,
    )
}

pub fn white_balance_to_sliders(white: WhiteBalance) -> (f32, f32) {
    white_balance_to_sliders_from_base(
        WhiteBalance {
            cct_kelvin: D65_CCT_KELVIN,
            duv: 0.0,
        },
        white,
    )
}

/// Recover the nearest locus CCT and signed Duv from a source-white XYZ.
pub fn white_balance_from_xyz(xyz: [f32; 3]) -> Option<WhiteBalance> {
    if xyz.iter().any(|v| !v.is_finite()) || xyz[1] <= 0.0 {
        return None;
    }
    let sum = xyz.iter().sum::<f32>();
    if sum <= 1.0e-8 {
        return None;
    }
    let target_uv = xy_to_uv(xyz[0] / sum, xyz[1] / sum);
    let min_mired = 1.0e6 / MAX_CCT_KELVIN;
    let max_mired = 1.0e6 / MIN_CCT_KELVIN;
    let distance = |mired: f32| {
        let (x, y) = locus_xy(1.0e6 / mired);
        let uv = xy_to_uv(x, y);
        (uv.0 - target_uv.0).powi(2) + (uv.1 - target_uv.1).powi(2)
    };
    let mut best = min_mired;
    let mut best_distance = f32::INFINITY;
    const STEPS: usize = 256;
    for i in 0..=STEPS {
        let mired = min_mired + (max_mired - min_mired) * i as f32 / STEPS as f32;
        let d = distance(mired);
        if d < best_distance {
            best = mired;
            best_distance = d;
        }
    }
    let step = (max_mired - min_mired) / STEPS as f32;
    let mut low = (best - step).max(min_mired);
    let mut high = (best + step).min(max_mired);
    for _ in 0..20 {
        let a = low + (high - low) / 3.0;
        let b = high - (high - low) / 3.0;
        if distance(a) <= distance(b) {
            high = b;
        } else {
            low = a;
        }
    }
    let cct = 1.0e6 / ((low + high) * 0.5);
    let (x0, y0) = locus_xy(cct);
    let (u0, v0) = xy_to_uv(x0, y0);
    let (x1, y1) = locus_xy((cct * 1.002).min(MAX_CCT_KELVIN));
    let (u1, v1) = xy_to_uv(x1, y1);
    let (tu, tv) = (u1 - u0, v1 - v0);
    let length = (tu * tu + tv * tv).sqrt().max(1.0e-9);
    let normal = (tv / length, -tu / length);
    let duv = (target_uv.0 - u0) * normal.0 + (target_uv.1 - v0) * normal.1;
    Some(WhiteBalance {
        cct_kelvin: cct.clamp(MIN_CCT_KELVIN, MAX_CCT_KELVIN),
        duv,
    })
}

/// Build a CAT16 adaptation matrix directly from absolute CCT/Duv.
pub fn wb_matrix_at_white_balance(white: WhiteBalance) -> [[f32; 3]; 3] {
    let (sx, sy) = illuminant_xy(
        white.cct_kelvin.clamp(MIN_CCT_KELVIN, MAX_CCT_KELVIN),
        white.duv.clamp(-TINT_DUV_RANGE, TINT_DUV_RANGE),
    );
    let (dx, dy) = illuminant_xy(D65_CCT_KELVIN, 0.0);
    let src_w = mat_apply(&M16, xy_to_xyz(sx, sy));
    let dst_w = mat_apply(&M16, xy_to_xyz(dx, dy));
    let gains = [
        [dst_w[0] / src_w[0].max(1e-6), 0.0, 0.0],
        [0.0, dst_w[1] / src_w[1].max(1e-6), 0.0],
        [0.0, 0.0, dst_w[2] / src_w[2].max(1e-6)],
    ];
    let cat_xyz = mat_mul(&M16_INV, &mat_mul(&gains, &M16));
    let matrix = mat_mul(&SRGB_FROM_XYZ, &mat_mul(&cat_xyz, &XYZ_FROM_SRGB));
    normalize_wb_luma(matrix)
}

/// Relative correction from a scene's already-applied as-shot white to a
/// requested absolute white. This is identity when target equals base.
pub fn wb_matrix_between(base: WhiteBalance, target: WhiteBalance) -> [[f32; 3]; 3] {
    if (base.cct_kelvin - target.cct_kelvin).abs() < 1.0e-4
        && (base.duv - target.duv).abs() < 1.0e-7
    {
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    }
    let base_matrix = wb_matrix_at_white_balance(base);
    let Some(base_inverse) = mat_inverse(&base_matrix) else {
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    };
    let relative = mat_mul(&wb_matrix_at_white_balance(target), &base_inverse);
    normalize_wb_luma(relative)
}

pub fn wb_matrix_from_base(base: WhiteBalance, temperature: f32, tint: f32) -> [[f32; 3]; 3] {
    if temperature.abs() < 1.0e-6 && tint.abs() < 1.0e-6 {
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    }
    // The camera already balanced the scene, so the as-shot white IS the neutral
    // reference: Temperature travels along the locus and Tint is a symmetric
    // offset from neutral. Only the as-shot CCT sets the pivot; its recovered
    // Duv is display metadata, NOT fed into the CAT. Carrying a large sensor Duv
    // (e.g. 0.047 on a D810 neutral) into the pivot pushed a warm target far off
    // the locus and exploded the channel gains (>2500×) into a flat magenta.
    let pivot = WhiteBalance {
        cct_kelvin: base.cct_kelvin,
        duv: 0.0,
    };
    wb_matrix_between(
        pivot,
        sliders_to_white_balance_from_base(pivot, temperature, tint),
    )
}

/// Build the composed linear-sRGB → linear-sRGB white-balance matrix for the
/// Develop Temperature/Tint sliders (each ±200). (0, 0) returns the exact
/// identity. Temperature > 0 warms the image (the scene illuminant is assumed
/// cooler than D65, so adapting to D65 pushes the render toward amber);
/// Tint > 0 pushes magenta (assumed greener illuminant), matching the old
/// slider directions and Lightroom convention.
pub fn wb_matrix(temperature: f32, tint: f32) -> [[f32; 3]; 3] {
    let ut = control_to_unit(temperature);
    let ug = control_to_unit(tint);
    if ut.abs() < 1e-6 && ug.abs() < 1e-6 {
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    }
    wb_matrix_at_white_balance(sliders_to_white_balance(temperature, tint))
}

/// Scene1 compatibility mapping: daylight-only, symmetric ±80 mired around
/// D65. This is the exact pre-Develop2 formula and must not be retuned.
pub fn wb_matrix_legacy(temperature: f32, tint: f32) -> [[f32; 3]; 3] {
    const TEMP_MIRED_RANGE: f32 = 80.0;
    let temperature_unit = control_to_unit(temperature);
    let tint_unit = control_to_unit(tint);
    if temperature_unit.abs() < 1e-6 && tint_unit.abs() < 1e-6 {
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    }
    let mired_d65 = 1.0e6 / D65_CCT_KELVIN;
    let source_cct = 1.0e6 / (mired_d65 - temperature_unit * TEMP_MIRED_RANGE);
    let source_duv = tint_unit * TINT_DUV_RANGE;
    let (sx, sy) = legacy_illuminant_xy(source_cct, source_duv);
    let (dx, dy) = legacy_illuminant_xy(D65_CCT_KELVIN, 0.0);
    let src_w = mat_apply(&M16, xy_to_xyz(sx, sy));
    let dst_w = mat_apply(&M16, xy_to_xyz(dx, dy));
    let gains = [
        [dst_w[0] / src_w[0].max(1e-6), 0.0, 0.0],
        [0.0, dst_w[1] / src_w[1].max(1e-6), 0.0],
        [0.0, 0.0, dst_w[2] / src_w[2].max(1e-6)],
    ];
    let cat_xyz = mat_mul(&M16_INV, &mat_mul(&gains, &M16));
    let mut matrix = mat_mul(&SRGB_FROM_XYZ, &mat_mul(&cat_xyz, &XYZ_FROM_SRGB));
    let luma = white_luma(&matrix);
    if luma > 1e-6 {
        for row in &mut matrix {
            for value in row {
                *value /= luma;
            }
        }
    }
    matrix
}

/// Solve the Temperature/Tint slider pair (each ±`CONTROL_LIMIT`) whose
/// [`wb_matrix`] maps a linear-sRGB sample closest to neutral grey — the
/// white-balance eyedropper (D5). The sample is the scene value the user
/// clicked, taken BEFORE white balance, so the result is an absolute setting
/// rather than a delta on the current one.
///
/// The balanced pixel `wb·p` is neutral exactly when its two chroma residuals
/// `(r−g, g−b)` vanish — a smooth two-in / two-out system in (temp, tint), so a
/// handful of Gauss-Newton steps from the neutral start converge. The result is
/// clamped to the slider range: a sample too saturated to neutralise within
/// range yields the best in-range correction instead of running away.
pub fn neutralize(pixel: [f32; 3]) -> (f32, f32) {
    let sum = pixel[0] + pixel[1] + pixel[2];
    if !sum.is_finite() || sum <= 1e-6 {
        return (0.0, 0.0);
    }
    // Chromaticity only: the overall scale never moves the root, and unit-sum
    // keeps the residual magnitudes well-conditioned across bright/dark samples.
    let p = [pixel[0] / sum, pixel[1] / sum, pixel[2] / sum];
    let residual = |temp: f32, tint: f32| {
        let w = mat_apply(&wb_matrix(temp, tint), p);
        [w[0] - w[1], w[1] - w[2]]
    };
    let (mut temp, mut tint) = (0.0f32, 0.0f32);
    for _ in 0..30 {
        let r = residual(temp, tint);
        if r[0].abs() + r[1].abs() < 1e-6 {
            break;
        }
        // One-sided finite-difference Jacobian — the slider→matrix map is smooth
        // enough over this range that a forward difference tracks it well.
        let h = 2.0;
        let rt = residual(temp + h, tint);
        let rn = residual(temp, tint + h);
        let a = (rt[0] - r[0]) / h; // ∂(r−g)/∂temp
        let b = (rn[0] - r[0]) / h; // ∂(r−g)/∂tint
        let c = (rt[1] - r[1]) / h; // ∂(g−b)/∂temp
        let d = (rn[1] - r[1]) / h; // ∂(g−b)/∂tint
        let det = a * d - b * c;
        if det.abs() < 1e-9 {
            break; // At a slider clamp the matrix stops moving — take the best so far.
        }
        // Δ = −J⁻¹·r.
        let mut d_temp = (b * r[1] - d * r[0]) / det;
        let mut d_tint = (c * r[0] - a * r[1]) / det;
        // Damp a runaway step so a far-off sample cannot overshoot the smooth
        // region before the range clamp catches it.
        let step = (d_temp * d_temp + d_tint * d_tint).sqrt();
        let cap = 2.0 * CONTROL_LIMIT;
        if step > cap {
            let s = cap / step;
            d_temp *= s;
            d_tint *= s;
        }
        temp = (temp + d_temp).clamp(-CONTROL_LIMIT, CONTROL_LIMIT);
        tint = (tint + d_tint).clamp(-CONTROL_LIMIT, CONTROL_LIMIT);
        if d_temp.abs() + d_tint.abs() < 1e-3 {
            break;
        }
    }
    (temp, tint)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    /// Chroma spread of a linear-sRGB pixel, normalised by its brightness.
    fn chroma_spread(w: [f32; 3]) -> f32 {
        let max = w[0].max(w[1]).max(w[2]);
        let min = w[0].min(w[1]).min(w[2]);
        (max - min) / max.max(1e-4)
    }

    #[test]
    fn eyedropper_neutralises_a_colour_cast() {
        // Mild warm / cool / green / magenta casts on a mid-grey card — all
        // comfortably inside the slider range.
        for p in [
            [0.55f32, 0.50, 0.42], // warm
            [0.42, 0.50, 0.55],    // cool
            [0.48, 0.56, 0.48],    // green
            [0.55, 0.47, 0.55],    // magenta
        ] {
            let (t, g) = neutralize(p);
            assert!(t.abs() <= CONTROL_LIMIT && g.abs() <= CONTROL_LIMIT);
            let balanced = mat_apply(&wb_matrix(t, g), p);
            assert!(
                chroma_spread(balanced) < 0.01,
                "cast not removed for {p:?}: balanced {balanced:?} (t={t}, g={g})"
            );
        }
    }

    #[test]
    fn eyedropper_on_neutral_is_a_no_op() {
        let (t, g) = neutralize([0.5, 0.5, 0.5]);
        assert!(approx(t, 0.0, 1e-3) && approx(g, 0.0, 1e-3), "t={t}, g={g}");
    }

    #[test]
    fn eyedropper_reduces_even_a_strong_cast() {
        // A saturated sample may not neutralise fully in range, but the result
        // must still cut the cast, never worsen it.
        let p = [0.7f32, 0.45, 0.25];
        let (t, g) = neutralize(p);
        let balanced = mat_apply(&wb_matrix(t, g), p);
        assert!(
            chroma_spread(balanced) < chroma_spread(p),
            "did not reduce cast: {p:?} -> {balanced:?} (t={t}, g={g})"
        );
    }

    #[test]
    fn eyedropper_ignores_black() {
        assert_eq!(neutralize([0.0, 0.0, 0.0]), (0.0, 0.0));
    }

    #[test]
    fn neutral_sliders_are_exact_identity() {
        let m = wb_matrix(0.0, 0.0);
        for (i, row) in m.iter().enumerate() {
            for (j, &v) in row.iter().enumerate() {
                let want = if i == j { 1.0 } else { 0.0 };
                assert!(approx(v, want, 1e-6), "m[{i}][{j}] = {v}");
            }
        }
    }

    #[test]
    fn warm_slider_raises_red_over_blue_and_preserves_neutral_luma() {
        for temp in [40.0f32, 120.0, 200.0] {
            let m = wb_matrix(temp, 0.0);
            let w = mat_apply(&m, [0.5, 0.5, 0.5]);
            assert!(
                w[0] > w[2],
                "temp {temp}: r {} should exceed b {}",
                w[0],
                w[2]
            );
            let l = 0.2126 * w[0] + 0.7152 * w[1] + 0.0722 * w[2];
            assert!(
                approx(l, 0.5, 2e-3),
                "temp {temp}: neutral luma drifted to {l}"
            );
        }
        let m = wb_matrix(-150.0, 0.0);
        let w = mat_apply(&m, [0.5, 0.5, 0.5]);
        assert!(w[2] > w[0], "cool slider should raise blue over red");
    }

    #[test]
    fn tint_slider_moves_green_magenta_axis() {
        let mag = mat_apply(&wb_matrix(0.0, 150.0), [0.5, 0.5, 0.5]);
        assert!(
            mag[1] < (mag[0] + mag[2]) * 0.5,
            "positive tint should suppress green vs r/b: {mag:?}"
        );
        let grn = mat_apply(&wb_matrix(0.0, -150.0), [0.5, 0.5, 0.5]);
        assert!(
            grn[1] > (grn[0] + grn[2]) * 0.5,
            "negative tint should favour green: {grn:?}"
        );
    }

    #[test]
    fn known_planckian_pair_adapts_in_the_right_direction_and_magnitude() {
        // Full warm slider assumes a 25000 K source; the D65 white point pushed
        // through the adaptation must land clearly amber but still plausible
        // (bounded gains — CAT16 does not explode like naive XYZ scaling).
        let m = wb_matrix(200.0, 0.0);
        let w = mat_apply(&m, [1.0, 1.0, 1.0]);
        assert!(w[0] / w[2] > 1.15, "full warm too weak: {w:?}");
        assert!(w[0] / w[2] < 3.0, "full warm implausibly strong: {w:?}");
    }

    #[test]
    fn matrix_is_monotone_in_temperature() {
        let mut last_ratio = 0.0f32;
        for i in 0..=20 {
            let t = -200.0 + i as f32 * 20.0;
            let w = mat_apply(&wb_matrix(t, 0.0), [1.0, 1.0, 1.0]);
            let ratio = w[0] / w[2].max(1e-6);
            assert!(
                ratio > last_ratio || i == 0,
                "warmth must rise with the slider (t={t}, ratio={ratio})"
            );
            last_ratio = ratio;
        }
    }

    #[test]
    fn daylight_locus_hits_reference_chromaticities() {
        // CIE daylight locus: D65 ≈ (0.3127, 0.3290), D50 region and 10000 K
        // sanity bounds (locus is only used inside [4278, 13600] K here).
        let (x, y) = daylight_xy(6503.6);
        assert!(
            approx(x, 0.3127, 2e-3) && approx(y, 0.3290, 2e-3),
            "D65 ({x},{y})"
        );
        let (x5, _) = daylight_xy(5000.0);
        assert!(x5 > x, "5000K must be warmer (larger x) than D65");
        let (x10, _) = daylight_xy(10000.0);
        assert!(x10 < x, "10000K must be cooler (smaller x) than D65");
    }

    #[test]
    fn planckian_integration_tracks_standard_illuminant_a() {
        let (x, y) = planckian_xy(2856.0);
        assert!(
            approx(x, 0.4476, 0.006) && approx(y, 0.4074, 0.006),
            "illuminant A: ({x}, {y})"
        );
    }

    #[test]
    fn hybrid_locus_is_continuous_through_the_model_join() {
        let mut previous = locus_xy(3800.0);
        for cct in (3810..=4200).step_by(10) {
            let current = locus_xy(cct as f32);
            let (pu, pv) = xy_to_uv(previous.0, previous.1);
            let (cu, cv) = xy_to_uv(current.0, current.1);
            let jump = ((cu - pu).powi(2) + (cv - pv).powi(2)).sqrt();
            assert!(jump < 0.001, "locus jump at {cct} K: {jump}");
            previous = current;
        }
    }

    #[test]
    fn slider_and_absolute_white_balance_round_trip() {
        for temperature in [-200.0, -137.0, -40.0, 0.0, 65.0, 141.0, 200.0] {
            for tint in [-200.0, -73.0, 0.0, 54.0, 200.0] {
                let absolute = sliders_to_white_balance(temperature, tint);
                let (actual_temperature, actual_tint) = white_balance_to_sliders(absolute);
                assert!(
                    approx(actual_temperature, temperature, 2.0e-4),
                    "temperature {temperature} -> {absolute:?} -> {actual_temperature}"
                );
                assert!(approx(actual_tint, tint, 2.0e-4));
            }
        }
    }

    #[test]
    fn as_shot_relative_controls_round_trip_and_zero_is_identity() {
        // Representative daylight as-shot base. (A base within a few Kelvin of
        // the tungsten warm endpoint has a warm-slider span of ~1 mired, which
        // the f32 `cct_kelvin` field cannot resolve finely enough to round-trip
        // at 3e-4 — a storage-precision edge, not a mapping error.)
        let base = WhiteBalance {
            cct_kelvin: 5200.0,
            duv: 0.009,
        };
        assert_eq!(wb_matrix_from_base(base, 0.0, 0.0), wb_matrix(0.0, 0.0));
        for temperature in [-200.0, -91.0, 0.0, 74.0, 200.0] {
            for tint in [-200.0, -60.0, 0.0, 83.0, 200.0] {
                let target = sliders_to_white_balance_from_base(base, temperature, tint);
                let actual = white_balance_to_sliders_from_base(base, target);
                assert!(approx(actual.0, temperature, 3.0e-4), "{target:?}");
                assert!(approx(actual.1, tint, 3.0e-4), "{target:?}");
            }
        }
    }

    #[test]
    fn relative_cat_composes_base_adaptation_into_target() {
        let base = WhiteBalance {
            cct_kelvin: 2856.0,
            duv: 0.006,
        };
        let target = WhiteBalance {
            cct_kelvin: 7200.0,
            duv: -0.004,
        };
        let mut composed = mat_mul(
            &wb_matrix_between(base, target),
            &wb_matrix_at_white_balance(base),
        );
        let scale = white_luma(&composed);
        for row in &mut composed {
            for value in row {
                *value /= scale;
            }
        }
        let expected = wb_matrix_at_white_balance(target);
        for row in 0..3 {
            for column in 0..3 {
                assert!(
                    approx(composed[row][column], expected[row][column], 3.0e-4),
                    "[{row}][{column}] {} vs {}",
                    composed[row][column],
                    expected[row][column]
                );
            }
        }
    }

    #[test]
    fn xyz_inverse_recovers_cct_and_duv() {
        for expected in [
            WhiteBalance {
                cct_kelvin: 2400.0,
                duv: -0.012,
            },
            WhiteBalance {
                cct_kelvin: 4000.0,
                duv: 0.0,
            },
            WhiteBalance {
                cct_kelvin: 6503.6,
                duv: 0.008,
            },
            WhiteBalance {
                cct_kelvin: 12_000.0,
                duv: -0.005,
            },
        ] {
            let (x, y) = illuminant_xy(expected.cct_kelvin, expected.duv);
            let actual = white_balance_from_xyz(xy_to_xyz(x, y)).unwrap();
            assert!(
                (actual.cct_kelvin - expected.cct_kelvin).abs() < 25.0,
                "{expected:?} -> {actual:?}"
            );
            assert!(
                (actual.duv - expected.duv).abs() < 2.0e-4,
                "{expected:?} -> {actual:?}"
            );
        }
    }

    #[test]
    fn absolute_matrix_matches_slider_matrix() {
        for (temperature, tint) in [(-180.0, -40.0), (-60.0, 25.0), (80.0, 70.0)] {
            let slider = wb_matrix(temperature, tint);
            let absolute = wb_matrix_at_white_balance(sliders_to_white_balance(temperature, tint));
            assert_eq!(slider, absolute);
        }
    }

    #[test]
    fn legacy_mapping_retains_its_historical_range() {
        assert_eq!(wb_matrix_legacy(0.0, 0.0), wb_matrix(0.0, 0.0));
        let legacy_tungsten_end = 1.0e6 / (1.0e6 / D65_CCT_KELVIN + 80.0);
        assert!(approx(legacy_tungsten_end, 4278.0, 2.0));
        assert_eq!(
            wb_matrix_legacy(-123.0, 47.0),
            [
                [0.991_395_4, -0.037_559_81, -0.017_671_25],
                [0.001_646_964_1, 1.006_689_9, -0.060_024_89],
                [0.017_785_104, 0.094_071_78, 1.588_126_1],
            ]
        );
        assert_ne!(wb_matrix_legacy(-200.0, 0.0), wb_matrix(-200.0, 0.0));
    }

    #[test]
    fn extreme_absolute_inputs_stay_finite_and_bounded() {
        for white in [
            WhiteBalance {
                cct_kelvin: 100.0,
                duv: -1.0,
            },
            WhiteBalance {
                cct_kelvin: 100_000.0,
                duv: 1.0,
            },
        ] {
            let matrix = wb_matrix_at_white_balance(white);
            assert!(matrix.iter().flatten().all(|value| value.is_finite()));
            let rendered_white = mat_apply(&matrix, [1.0; 3]);
            assert!(
                rendered_white.iter().all(|value| value.abs() <= 4.0),
                "{white:?} -> {rendered_white:?}"
            );
            assert!(approx(
                0.2126 * rendered_white[0]
                    + 0.7152 * rendered_white[1]
                    + 0.0722 * rendered_white[2],
                1.0,
                2.0e-4
            ));
        }
    }
}

//! CAT16 chromatic adaptation for the Develop white-balance stage.
//!
//! The old white balance was three ad-hoc channel gains in sRGB. A real WB move
//! is a change of assumed illuminant: the Temperature/Tint sliders pick an
//! illuminant on the daylight locus (± a Duv offset toward green/magenta) and
//! the pixel is adapted from that illuminant to D65 with the CAT16 transform —
//! the standard CAT16 chromatic-adaptation model. Everything composes into
//! ONE 3×3 matrix over linear-sRGB, built once per slider change, so the
//! per-pixel cost is a single matrix multiply on both CPU and GPU.
//!
//! Slider mapping (P1 stopgap, full CCT/Duv locus math lands in P2):
//! Temperature ±200 → ±`TEMP_MIRED_RANGE` mired around D65 (mired keeps the
//! perceptual step per slider tick roughly constant across the range);
//! Tint ±200 → ∓`TINT_DUV_RANGE` Duv (perpendicular to the locus).
//! (0, 0) is EXACTLY the identity matrix: the source illuminant is computed
//! through the same locus code as the D65 reference, so the gains cancel and a
//! neutral setting is a true no-op (`is_neutral` semantics).

use crate::core::develop::{control_to_unit, CONTROL_LIMIT};

/// D65 correlated colour temperature (K). The locus is evaluated here for the
/// reference white so temp = 0 cancels exactly.
const D65_CCT: f32 = 6503.6;
/// Temperature slider range in mired (±). 80 mired below D65 ≈ 13600 K (cool),
/// 80 above ≈ 4280 K (warm) — the useful correction band for as-shot RAW.
const TEMP_MIRED_RANGE: f32 = 80.0;
/// Tint slider range as a CIE-1960 Duv offset (±). 0.05 spans a strong
/// green↔magenta correction (fluorescent-grade).
const TINT_DUV_RANGE: f32 = 0.05;

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

/// Illuminant chromaticity for a CCT plus a Duv offset perpendicular to the
/// locus (positive Duv = toward green, the CIE convention).
fn illuminant_xy(cct: f32, duv: f32) -> (f32, f32) {
    let (x0, y0) = daylight_xy(cct);
    if duv.abs() < 1e-7 {
        return (x0, y0);
    }
    let (u0, v0) = xy_to_uv(x0, y0);
    // Locus tangent from a small CCT step; the normal (rotated +90°) points
    // toward green above the locus.
    let (x1, y1) = daylight_xy(cct * 1.01);
    let (u1, v1) = xy_to_uv(x1, y1);
    let (mut tu, mut tv) = (u1 - u0, v1 - v0);
    let len = (tu * tu + tv * tv).sqrt().max(1e-9);
    tu /= len;
    tv /= len;
    // The locus tangent toward higher CCT points down-left in uv; rotating it
    // −90° gives the normal on the GREEN side (+v), the CIE +Duv convention.
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
    // Warmer output ⇐ assume a cooler (higher-CCT, lower-mired) source.
    let mired_d65 = 1.0e6 / D65_CCT;
    let src_cct = 1.0e6 / (mired_d65 - ut * TEMP_MIRED_RANGE);
    // Tint > 0 → magenta output ⇐ assume a greener source (positive Duv).
    let src_duv = ug * TINT_DUV_RANGE;

    let (sx, sy) = illuminant_xy(src_cct, src_duv);
    let (dx, dy) = illuminant_xy(D65_CCT, 0.0);
    let src_w = mat_apply(&M16, xy_to_xyz(sx, sy));
    let dst_w = mat_apply(&M16, xy_to_xyz(dx, dy));

    // Full-adaptation (D = 1) von-Kries scaling in CAT16 cone space.
    let gains = [
        [dst_w[0] / src_w[0].max(1e-6), 0.0, 0.0],
        [0.0, dst_w[1] / src_w[1].max(1e-6), 0.0],
        [0.0, 0.0, dst_w[2] / src_w[2].max(1e-6)],
    ];
    let cat_xyz = mat_mul(&M16_INV, &mat_mul(&gains, &M16));
    let mut m = mat_mul(&SRGB_FROM_XYZ, &mat_mul(&cat_xyz, &XYZ_FROM_SRGB));

    // Luma-preserving normalisation (same contract as the old `wb_gains`).
    let l = white_luma(&m);
    if l > 1e-6 {
        for row in &mut m {
            for v in row {
                *v /= l;
            }
        }
    }
    m
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
        // Full warm slider assumes a ~13600 K source; the D65 white point pushed
        // through the adaptation must land clearly amber but still plausible
        // (bounded gains — CAT16 does not explode like naive XYZ scaling).
        let m = wb_matrix(200.0, 0.0);
        let w = mat_apply(&m, [1.0, 1.0, 1.0]);
        assert!(w[0] / w[2] > 1.15, "full warm too weak: {w:?}");
        assert!(w[0] / w[2] < 2.5, "full warm implausibly strong: {w:?}");
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
}

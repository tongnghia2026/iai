//! Clean-room colorimetric foundation for Develop Engine 2's profile-aware
//! input/scene boundary.
//!
//! Everything here is derived from published CIE 1931 colorimetry and the
//! standard construction of an RGB-primaries → CIE XYZ matrix (the linear
//! algebra behind SMPTE RP 177: solve for the per-primary luminance scalars so
//! the adopted white maps to XYZ). Chromatic adaptation uses the published
//! Bradford cone-response matrix. No third-party imaging engine source was
//! consulted, and none of these values are copied from one.
//!
//! To guarantee this general path stays consistent with the color already
//! shipping in `working_color.rs`, the unit tests cross-check the derived
//! sRGB→ProPhoto and sRGB→ACEScg matrices against the hand-tabulated matrices
//! that the current renderer uses. That keeps the contract layer honest without
//! introducing a second, divergent set of color constants.

/// A 3×3 matrix in double precision. Colorimetric fitting is done in f64 and
/// only quantized to f32 at the boundary the render path consumes.
pub type Mat3 = [[f64; 3]; 3];

/// CIE 1931 xy chromaticity coordinate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chromaticity {
    pub x: f64,
    pub y: f64,
}

impl Chromaticity {
    const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// Standard 2° observer illuminant white points.
pub const D65: Chromaticity = Chromaticity::new(0.312_7, 0.329_0);
pub const D50: Chromaticity = Chromaticity::new(0.345_7, 0.358_5);
/// ACES adopted white (~D60).
pub const ACES_WHITE: Chromaticity = Chromaticity::new(0.321_68, 0.337_67);

/// The three additive primaries and adopted white of an RGB color space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RgbPrimaries {
    pub red: Chromaticity,
    pub green: Chromaticity,
    pub blue: Chromaticity,
    pub white: Chromaticity,
}

/// IEC 61966-2-1 sRGB / ITU-R BT.709 primaries (D65).
pub const SRGB: RgbPrimaries = RgbPrimaries {
    red: Chromaticity::new(0.640, 0.330),
    green: Chromaticity::new(0.300, 0.600),
    blue: Chromaticity::new(0.150, 0.060),
    white: D65,
};

/// ROMM RGB (ProPhoto) primaries (D50).
pub const PROPHOTO: RgbPrimaries = RgbPrimaries {
    red: Chromaticity::new(0.734_7, 0.265_3),
    green: Chromaticity::new(0.159_6, 0.840_4),
    blue: Chromaticity::new(0.036_6, 0.000_1),
    white: D50,
};

/// ACEScg (AP1) primaries (ACES white).
pub const ACESCG: RgbPrimaries = RgbPrimaries {
    red: Chromaticity::new(0.713, 0.293),
    green: Chromaticity::new(0.165, 0.830),
    blue: Chromaticity::new(0.128, 0.044),
    white: ACES_WHITE,
};

/// Display P3 primaries (D65). Retained for future output-gamut work.
pub const DISPLAY_P3: RgbPrimaries = RgbPrimaries {
    red: Chromaticity::new(0.680, 0.320),
    green: Chromaticity::new(0.265, 0.690),
    blue: Chromaticity::new(0.150, 0.060),
    white: D65,
};

/// ITU-R BT.2020 primaries (D65). Retained for future output-gamut work.
pub const REC2020: RgbPrimaries = RgbPrimaries {
    red: Chromaticity::new(0.708, 0.292),
    green: Chromaticity::new(0.170, 0.797),
    blue: Chromaticity::new(0.131, 0.046),
    white: D65,
};

/// Published Bradford cone-response transform (spectrally sharpened LMS).
const BRADFORD: Mat3 = [
    [0.895_1, 0.266_4, -0.161_4],
    [-0.750_2, 1.713_5, 0.036_7],
    [0.038_9, -0.068_5, 1.029_6],
];

/// CIE XYZ of a chromaticity normalized to Y = 1.
pub fn xyz_from_xy(c: Chromaticity) -> [f64; 3] {
    [c.x / c.y, 1.0, (1.0 - c.x - c.y) / c.y]
}

fn mat_mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut out = [[0.0; 3]; 3];
    for (r, out_row) in out.iter_mut().enumerate() {
        for (col, slot) in out_row.iter_mut().enumerate() {
            *slot = a[r][0] * b[0][col] + a[r][1] * b[1][col] + a[r][2] * b[2][col];
        }
    }
    out
}

fn mat_vec(a: &Mat3, v: [f64; 3]) -> [f64; 3] {
    [
        a[0][0] * v[0] + a[0][1] * v[1] + a[0][2] * v[2],
        a[1][0] * v[0] + a[1][1] * v[1] + a[1][2] * v[2],
        a[2][0] * v[0] + a[2][1] * v[1] + a[2][2] * v[2],
    ]
}

/// Invert a 3×3 matrix by cofactor expansion. `None` if singular.
pub fn invert3(m: &Mat3) -> Option<Mat3> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det.abs() < 1.0e-12 {
        return None;
    }
    let inv_det = 1.0 / det;
    Some([
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * inv_det,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * inv_det,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * inv_det,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * inv_det,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * inv_det,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * inv_det,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * inv_det,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * inv_det,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * inv_det,
        ],
    ])
}

/// Linear RGB → CIE XYZ matrix (in the space's own adopted white).
pub fn rgb_to_xyz(primaries: RgbPrimaries) -> Mat3 {
    let r = xyz_from_xy(primaries.red);
    let g = xyz_from_xy(primaries.green);
    let b = xyz_from_xy(primaries.blue);
    let primary_matrix = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let white = xyz_from_xy(primaries.white);
    // Per-primary luminance scalars so that RGB (1,1,1) maps to the white XYZ.
    let inv = invert3(&primary_matrix).expect("RGB primaries are linearly independent");
    let scale = mat_vec(&inv, white);
    [
        [
            primary_matrix[0][0] * scale[0],
            primary_matrix[0][1] * scale[1],
            primary_matrix[0][2] * scale[2],
        ],
        [
            primary_matrix[1][0] * scale[0],
            primary_matrix[1][1] * scale[1],
            primary_matrix[1][2] * scale[2],
        ],
        [
            primary_matrix[2][0] * scale[0],
            primary_matrix[2][1] * scale[1],
            primary_matrix[2][2] * scale[2],
        ],
    ]
}

/// Bradford chromatic-adaptation matrix from one white to another (both XYZ).
pub fn bradford(src_white: [f64; 3], dst_white: [f64; 3]) -> Mat3 {
    let ma_inv = invert3(&BRADFORD).expect("Bradford matrix is non-singular");
    let src_cone = mat_vec(&BRADFORD, src_white);
    let dst_cone = mat_vec(&BRADFORD, dst_white);
    let diag = [
        [dst_cone[0] / src_cone[0], 0.0, 0.0],
        [0.0, dst_cone[1] / src_cone[1], 0.0],
        [0.0, 0.0, dst_cone[2] / src_cone[2]],
    ];
    mat_mul(&ma_inv, &mat_mul(&diag, &BRADFORD))
}

/// Linear RGB (source space) → linear RGB (destination space), including
/// Bradford adaptation between the two adopted whites.
pub fn rgb_to_rgb(src: RgbPrimaries, dst: RgbPrimaries) -> Mat3 {
    let src_to_xyz = rgb_to_xyz(src);
    let adapt = bradford(xyz_from_xy(src.white), xyz_from_xy(dst.white));
    let xyz_to_dst = invert3(&rgb_to_xyz(dst)).expect("destination primaries are invertible");
    mat_mul(&xyz_to_dst, &mat_mul(&adapt, &src_to_xyz))
}

/// Linear RGB → CIE XYZ adapted to the D50 profile-connection white.
/// This is the profile-independent boundary a canonical Develop input node targets.
pub fn rgb_to_pcs_xyz_d50(primaries: RgbPrimaries) -> Mat3 {
    let to_xyz = rgb_to_xyz(primaries);
    let adapt = bradford(xyz_from_xy(primaries.white), xyz_from_xy(D50));
    mat_mul(&adapt, &to_xyz)
}

/// Quantize a colorimetric matrix to the f32 the render path consumes.
pub fn to_f32(m: &Mat3) -> [[f32; 3]; 3] {
    [
        [m[0][0] as f32, m[0][1] as f32, m[0][2] as f32],
        [m[1][0] as f32, m[1][1] as f32, m[1][2] as f32],
        [m[2][0] as f32, m[2][1] as f32, m[2][2] as f32],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::working_color::WorkingColorSpace;

    fn max_diff(a: &Mat3, b: &[[f32; 3]; 3]) -> f64 {
        let mut worst = 0.0f64;
        for r in 0..3 {
            for c in 0..3 {
                worst = worst.max((a[r][c] - b[r][c] as f64).abs());
            }
        }
        worst
    }

    #[test]
    fn white_maps_to_adopted_white_xyz() {
        // sRGB (1,1,1) → its own D65 white in XYZ.
        let white = mat_vec(&rgb_to_xyz(SRGB), [1.0, 1.0, 1.0]);
        let expect = xyz_from_xy(D65);
        for i in 0..3 {
            assert!(
                (white[i] - expect[i]).abs() < 1.0e-6,
                "{white:?} vs {expect:?}"
            );
        }
    }

    #[test]
    fn xyz_roundtrip_is_identity() {
        for prim in [SRGB, PROPHOTO, ACESCG, DISPLAY_P3, REC2020] {
            let fwd = rgb_to_xyz(prim);
            let inv = invert3(&fwd).unwrap();
            let round = mat_mul(&inv, &fwd);
            for r in 0..3 {
                for c in 0..3 {
                    let expect = if r == c { 1.0 } else { 0.0 };
                    assert!((round[r][c] - expect).abs() < 1.0e-9);
                }
            }
        }
    }

    #[test]
    fn derived_prophoto_matches_shipping_matrix() {
        // Cross-check the general derivation against the matrix the renderer
        // currently uses, so the contract layer cannot silently drift.
        let derived = rgb_to_rgb(SRGB, PROPHOTO);
        let reference = WorkingColorSpace::LinearProPhoto.from_linear_srgb_matrix();
        assert!(max_diff(&derived, reference) < 2.0e-3);
    }

    #[test]
    fn derived_acescg_matches_shipping_matrix() {
        let derived = rgb_to_rgb(SRGB, ACESCG);
        let reference = WorkingColorSpace::AcesCg.from_linear_srgb_matrix();
        assert!(max_diff(&derived, reference) < 2.0e-3);
    }

    #[test]
    fn pcs_transform_lands_on_d50_white() {
        let target = xyz_from_xy(D50);
        for prim in [SRGB, PROPHOTO, ACESCG] {
            let m = rgb_to_pcs_xyz_d50(prim);
            let white = mat_vec(&m, [1.0, 1.0, 1.0]);
            for i in 0..3 {
                assert!(
                    (white[i] - target[i]).abs() < 1.0e-4,
                    "{prim:?}: {white:?} vs {target:?}"
                );
            }
        }
    }
}

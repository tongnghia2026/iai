//! Shared perceptual-colour core for scene/display colour operations.
//!
//! OKLab is defined from linear-light sRGB/D65. Inputs in another scene working
//! space are transformed at the boundary, never clamped, so signed and HDR
//! values remain representable. The signed cube root makes the extension to
//! negative opponent/LMS values finite and reversible.

use super::working_color::WorkingColorSpace;

const OKLAB_LMS_FROM_SRGB: [[f32; 3]; 3] = [
    [0.412_221_46, 0.536_332_55, 0.051_445_995],
    [0.211_903_5, 0.680_699_5, 0.107_396_96],
    [0.088_302_46, 0.281_718_85, 0.629_978_7],
];
const SRGB_FROM_OKLAB_LMS: [[f32; 3]; 3] = [
    [4.076_741_7, -3.307_711_6, 0.230_969_94],
    [-1.268_438, 2.609_757_4, -0.341_319_4],
    [-0.004_196_086_3, -0.703_418_6, 1.707_614_7],
];

// OKLab's linear LMS input composed directly with each working-space-to-sRGB
// transform. The transforms already include their adopted-white adaptation, so
// this is the same colorimetry as `working -> linear sRGB -> OKLab` without an
// intermediate RGB representation or an opportunity for a gamut clamp.
const OKLAB_LMS_FROM_PROPHOTO: [[f32; 3]; 3] = [
    [0.715_330_96, 0.352_908_94, -0.068_239_8],
    [0.274_355_92, 0.667_852_34, 0.057_791_825],
    [0.109_757_565, 0.186_217_46, 0.704_025_6],
];
const PROPHOTO_FROM_OKLAB_LMS: [[f32; 3]; 3] = [
    [1.738_739_8, -0.988_400_76, 0.249_660_88],
    [-0.707_003_9, 1.934_316_8, -0.227_312_77],
    [-0.084_064_75, -0.357_515_5, 1.441_580_3],
];
const OKLAB_LMS_FROM_ACESCG: [[f32; 3]; 3] = [
    [0.631_782_05, 0.348_893_73, 0.019_316_588],
    [0.270_148_63, 0.630_902_8, 0.098_990_716],
    [0.098_801_255, 0.185_215_88, 0.716_043_83],
];
const ACESCG_FROM_OKLAB_LMS: [[f32; 3]; 3] = [
    [2.068_700_6, -1.175_307_2, 0.106_693_7],
    [-0.876_566_35, 2.150_152_7, -0.273_616_43],
    [-0.058_707_546, -0.393_958_27, 1.452_613_6],
];

pub fn oklab_lms_from_working_matrix(space: WorkingColorSpace) -> &'static [[f32; 3]; 3] {
    match space {
        WorkingColorSpace::LinearSrgb => &OKLAB_LMS_FROM_SRGB,
        WorkingColorSpace::AcesCg => &OKLAB_LMS_FROM_ACESCG,
        WorkingColorSpace::LinearProPhoto => &OKLAB_LMS_FROM_PROPHOTO,
    }
}

pub fn working_from_oklab_lms_matrix(space: WorkingColorSpace) -> &'static [[f32; 3]; 3] {
    match space {
        WorkingColorSpace::LinearSrgb => &SRGB_FROM_OKLAB_LMS,
        WorkingColorSpace::AcesCg => &ACESCG_FROM_OKLAB_LMS,
        WorkingColorSpace::LinearProPhoto => &PROPHOTO_FROM_OKLAB_LMS,
    }
}

// Wide-gamut adaptation matrices are rounded to f32, leaving neutral-axis
// opponent noise around 1e-5. Keep that numerical noise hue-less.
pub const ACHROMATIC_EPSILON: f32 = 2.0e-5;
pub const OKLAB_F32_ROUNDTRIP_TOLERANCE: f32 = 1.0e-4;
pub const OKLAB_GPU_PARITY_TOLERANCE: f32 = 2.0e-5;
pub const OKLAB_F16_ROUNDTRIP_TOLERANCE: f32 = 3.0e-3;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Oklab {
    pub l: f32,
    pub a: f32,
    pub b: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PerceptualColor {
    pub lightness: f32,
    pub chroma: f32,
    /// Hue in radians, normalized to `[0, TAU)`. Achromatic colours use zero.
    pub hue: f32,
}

impl PerceptualColor {
    pub fn from_oklab(lab: Oklab) -> Self {
        let chroma = lab.a.hypot(lab.b);
        let hue = if chroma <= ACHROMATIC_EPSILON {
            0.0
        } else {
            lab.b.atan2(lab.a).rem_euclid(std::f32::consts::TAU)
        };
        Self {
            lightness: lab.l,
            chroma,
            hue,
        }
    }

    pub fn to_oklab(self) -> Oklab {
        let (sin_h, cos_h) = self.hue.sin_cos();
        Oklab {
            l: self.lightness,
            a: self.chroma * cos_h,
            b: self.chroma * sin_h,
        }
    }
}

#[inline]
pub fn linear_srgb_to_oklab(rgb: [f32; 3]) -> Oklab {
    // Use the shader-representable signed pow form instead of libm cbrt so
    // hue rotations remain within the CPU/GPU output budget.
    let signed_cbrt = |x: f32| x.signum() * x.abs().powf(1.0 / 3.0);
    let l = signed_cbrt(0.412_221_46 * rgb[0] + 0.536_332_55 * rgb[1] + 0.051_445_995 * rgb[2]);
    let m = signed_cbrt(0.211_903_5 * rgb[0] + 0.680_699_5 * rgb[1] + 0.107_396_96 * rgb[2]);
    let s = signed_cbrt(0.088_302_46 * rgb[0] + 0.281_718_85 * rgb[1] + 0.629_978_7 * rgb[2]);
    Oklab {
        l: 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        a: 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        b: 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    }
}

#[inline]
pub fn oklab_to_linear_srgb(lab: Oklab) -> [f32; 3] {
    let l = lab.l + 0.396_337_78 * lab.a + 0.215_803_76 * lab.b;
    let m = lab.l - 0.105_561_346 * lab.a - 0.063_854_17 * lab.b;
    let s = lab.l - 0.089_484_18 * lab.a - 1.291_485_5 * lab.b;
    let (l, m, s) = (l * l * l, m * m * m, s * s * s);
    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_4 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
}

#[inline]
fn working_rgb_to_oklab_direct(rgb: [f32; 3], lms_from_working: &[[f32; 3]; 3]) -> Oklab {
    let signed_cbrt = |x: f32| x.signum() * x.abs().powf(1.0 / 3.0);
    let lms = super::working_color::apply_matrix(lms_from_working, rgb);
    let l = signed_cbrt(lms[0]);
    let m = signed_cbrt(lms[1]);
    let s = signed_cbrt(lms[2]);
    Oklab {
        l: 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        a: 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        b: 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    }
}

#[inline]
fn oklab_to_working_rgb_direct(lab: Oklab, working_from_lms: &[[f32; 3]; 3]) -> [f32; 3] {
    let l = lab.l + 0.396_337_78 * lab.a + 0.215_803_76 * lab.b;
    let m = lab.l - 0.105_561_346 * lab.a - 0.063_854_17 * lab.b;
    let s = lab.l - 0.089_484_18 * lab.a - 1.291_485_5 * lab.b;
    super::working_color::apply_matrix(working_from_lms, [l * l * l, m * m * m, s * s * s])
}

#[inline]
pub fn working_rgb_to_oklab(rgb: [f32; 3], space: WorkingColorSpace) -> Oklab {
    match space {
        WorkingColorSpace::LinearSrgb => linear_srgb_to_oklab(rgb),
        WorkingColorSpace::AcesCg => working_rgb_to_oklab_direct(rgb, &OKLAB_LMS_FROM_ACESCG),
        WorkingColorSpace::LinearProPhoto => {
            working_rgb_to_oklab_direct(rgb, &OKLAB_LMS_FROM_PROPHOTO)
        }
    }
}

#[inline]
pub fn oklab_to_working_rgb(lab: Oklab, space: WorkingColorSpace) -> [f32; 3] {
    match space {
        WorkingColorSpace::LinearSrgb => oklab_to_linear_srgb(lab),
        WorkingColorSpace::AcesCg => oklab_to_working_rgb_direct(lab, &ACESCG_FROM_OKLAB_LMS),
        WorkingColorSpace::LinearProPhoto => {
            oklab_to_working_rgb_direct(lab, &PROPHOTO_FROM_OKLAB_LMS)
        }
    }
}

#[inline]
pub fn working_rgb_to_perceptual(rgb: [f32; 3], space: WorkingColorSpace) -> PerceptualColor {
    PerceptualColor::from_oklab(working_rgb_to_oklab(rgb, space))
}

#[inline]
pub fn perceptual_to_working_rgb(color: PerceptualColor, space: WorkingColorSpace) -> [f32; 3] {
    oklab_to_working_rgb(color.to_oklab(), space)
}

/// HDR/WCG perceptual space used by the Develop3 highlight-chroma recipe.
///
/// The transform follows Safdar et al. (2017) and interprets display-linear
/// `1.0` as a 100 cd/m² SDR diffuse white. The signed PQ extension keeps
/// diagnostic/wide-gamut excursions finite and reversible; ordinary physical
/// colours stay on the standard positive branch.
pub mod jzazbz {
    use super::WorkingColorSpace;

    const DISPLAY_WHITE_NITS: f32 = 100.0;
    const PQ_N: f32 = 0.159_301_76;
    const PQ_P: f32 = 134.034_38;
    const PQ_C1: f32 = 0.835_937_5;
    const PQ_C2: f32 = 18.851_563;
    const PQ_C3: f32 = 18.687_5;
    const D: f32 = -0.56;
    const D0: f32 = 1.629_549_9e-11;

    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    pub struct JzAzBz {
        pub jz: f32,
        pub az: f32,
        pub bz: f32,
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    pub struct JzCzHz {
        pub jz: f32,
        pub cz: f32,
        pub hz: f32,
    }

    #[inline]
    fn pq(v: f32) -> f32 {
        let sign = v.signum();
        let vn = (v.abs() / 10_000.0).powf(PQ_N);
        sign * ((PQ_C1 + PQ_C2 * vn) / (1.0 + PQ_C3 * vn)).powf(PQ_P)
    }

    #[inline]
    fn inverse_pq(v: f32) -> f32 {
        let sign = v.signum();
        let vp = v.abs().powf(1.0 / PQ_P);
        let base = ((vp - PQ_C1) / (PQ_C2 - PQ_C3 * vp)).max(0.0);
        sign * 10_000.0 * base.powf(1.0 / PQ_N)
    }

    /// Convert scene working RGB to Jzazbz via D65 XYZ. This function never
    /// clamps: wide-gamut colours retain their signed matrix excursions.
    pub fn from_working_rgb(rgb: [f32; 3], space: WorkingColorSpace) -> JzAzBz {
        let rgb = space.to_linear_srgb(rgb);
        let x = DISPLAY_WHITE_NITS
            * (0.412_456_4 * rgb[0] + 0.357_576_1 * rgb[1] + 0.180_437_5 * rgb[2]);
        let y =
            DISPLAY_WHITE_NITS * (0.212_672_9 * rgb[0] + 0.715_152_2 * rgb[1] + 0.072_175 * rgb[2]);
        let z =
            DISPLAY_WHITE_NITS * (0.019_333_9 * rgb[0] + 0.119_192 * rgb[1] + 0.950_304_1 * rgb[2]);
        let xp = 1.15 * x - 0.15 * z;
        let yp = 0.66 * y + 0.34 * x;
        let l = 0.414_789_7 * xp + 0.579_999 * yp + 0.014_648 * z;
        let m = -0.201_51 * xp + 1.120_649 * yp + 0.053_100_8 * z;
        let s = -0.016_600_8 * xp + 0.264_8 * yp + 0.668_479_9 * z;
        let (lp, mp, sp) = (pq(l), pq(m), pq(s));
        let iz = 0.5 * (lp + mp);
        JzAzBz {
            jz: (1.0 + D) * iz / (1.0 + D * iz) - D0,
            az: 3.524 * lp - 4.066_708 * mp + 0.542_708 * sp,
            bz: 0.199_076 * lp + 1.096_799 * mp - 1.295_875 * sp,
        }
    }

    /// Inverse Jzazbz transform back to the requested scene working space.
    pub fn to_working_rgb(value: JzAzBz, space: WorkingColorSpace) -> [f32; 3] {
        let q = value.jz + D0;
        let iz = q / (1.0 + D - D * q);
        let lp = iz + 0.138_605_04 * value.az + 0.058_047_317 * value.bz;
        let mp = iz - 0.138_605_04 * value.az - 0.058_047_317 * value.bz;
        let sp = iz - 0.096_019_24 * value.az - 0.811_891_9 * value.bz;
        let l = inverse_pq(lp);
        let m = inverse_pq(mp);
        let s = inverse_pq(sp);

        let xp = 1.924_226_4 * l - 1.004_792_3 * m + 0.037_651_405 * s;
        let yp = 0.350_316_76 * l + 0.726_481_2 * m - 0.065_384_425 * s;
        let z = -0.090_982_81 * l - 0.312_728_3 * m + 1.522_766_6 * s;
        let x = (xp + 0.15 * z) / 1.15;
        let y = (yp - 0.34 * x) / 0.66;
        let scale = 1.0 / DISPLAY_WHITE_NITS;
        let xyz = [x * scale, y * scale, z * scale];
        let linear_srgb = [
            3.240_454_2 * xyz[0] - 1.537_138_5 * xyz[1] - 0.498_531_4 * xyz[2],
            -0.969_266 * xyz[0] + 1.876_010_8 * xyz[1] + 0.041_556 * xyz[2],
            0.055_643_4 * xyz[0] - 0.204_025_9 * xyz[1] + 1.057_225_2 * xyz[2],
        ];
        space.from_linear_srgb(linear_srgb)
    }

    impl JzAzBz {
        pub fn is_finite(self) -> bool {
            [self.jz, self.az, self.bz].into_iter().all(f32::is_finite)
        }

        /// Scale perceptual chroma while preserving Jz and hue exactly.
        pub fn scale_chroma(self, factor: f32) -> Self {
            Self {
                jz: self.jz,
                az: self.az * factor,
                bz: self.bz * factor,
            }
        }
    }

    impl From<JzAzBz> for JzCzHz {
        fn from(value: JzAzBz) -> Self {
            let cz = value.az.hypot(value.bz);
            Self {
                jz: value.jz,
                cz,
                hz: if cz <= super::ACHROMATIC_EPSILON {
                    0.0
                } else {
                    value.bz.atan2(value.az).rem_euclid(std::f32::consts::TAU)
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VECTORS: [[f32; 3]; 9] = [
        [0.0, 0.0, 0.0],
        [0.18, 0.18, 0.18],
        [1.0, 1.0, 1.0],
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
        [-0.25, 0.4, 2.0],
        [8.0, -1.0, 0.5],
        [-100.0, 40.0, 250.0],
    ];

    #[test]
    fn signed_hdr_roundtrip_is_finite() {
        for space in [
            WorkingColorSpace::LinearSrgb,
            WorkingColorSpace::AcesCg,
            WorkingColorSpace::LinearProPhoto,
        ] {
            for rgb in VECTORS {
                let lab = working_rgb_to_oklab(rgb, space);
                let back = oklab_to_working_rgb(lab, space);
                assert!([lab.l, lab.a, lab.b]
                    .into_iter()
                    .chain(back)
                    .all(f32::is_finite));
                let scale = rgb.into_iter().map(f32::abs).fold(1.0, f32::max);
                for i in 0..3 {
                    assert!(
                        (back[i] - rgb[i]).abs() <= OKLAB_F32_ROUNDTRIP_TOLERANCE * scale,
                        "{space:?} {rgb:?} -> {lab:?} -> {back:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn neutral_axis_stays_neutral_and_hueless() {
        for v in [-2.0, -0.1, 0.0, 0.18, 1.0, 16.0] {
            let rgb = WorkingColorSpace::LinearProPhoto.from_linear_srgb([v; 3]);
            let p = working_rgb_to_perceptual(rgb, WorkingColorSpace::LinearProPhoto);
            assert!(p.chroma < 2.0e-5, "neutral {v} -> {p:?}");
            assert_eq!(p.hue, 0.0);
        }
    }

    #[test]
    fn direct_working_oklab_matches_unclamped_composed_reference() {
        for space in [WorkingColorSpace::AcesCg, WorkingColorSpace::LinearProPhoto] {
            for rgb in VECTORS {
                let direct = working_rgb_to_oklab(rgb, space);
                let composed = linear_srgb_to_oklab(space.to_linear_srgb(rgb));
                for (a, b) in [direct.l, direct.a, direct.b]
                    .into_iter()
                    .zip([composed.l, composed.a, composed.b])
                {
                    assert!((a - b).abs() < 3.0e-6, "{space:?}: {rgb:?}");
                }
            }
        }
    }

    #[test]
    fn cylindrical_roundtrip_preserves_hue() {
        for rgb in VECTORS.into_iter().skip(3) {
            let p = working_rgb_to_perceptual(rgb, WorkingColorSpace::LinearSrgb);
            let q = PerceptualColor::from_oklab(p.to_oklab());
            let hue_error = (q.hue - p.hue)
                .abs()
                .min(std::f32::consts::TAU - (q.hue - p.hue).abs());
            assert!(hue_error < 2.0e-6 && (q.chroma - p.chroma).abs() < 2.0e-6);
        }
    }

    #[test]
    fn jzazbz_is_finite_for_signed_hdr() {
        for rgb in VECTORS {
            let j = jzazbz::from_working_rgb(rgb, WorkingColorSpace::LinearProPhoto);
            assert!(
                [j.jz, j.az, j.bz].into_iter().all(f32::is_finite),
                "{rgb:?} -> {j:?}"
            );
            let c = jzazbz::JzCzHz::from(j);
            assert!([c.jz, c.cz, c.hz].into_iter().all(f32::is_finite));
        }
    }

    #[test]
    fn jzazbz_roundtrips_display_linear_working_spaces() {
        for space in [
            WorkingColorSpace::LinearSrgb,
            WorkingColorSpace::AcesCg,
            WorkingColorSpace::LinearProPhoto,
        ] {
            for linear_srgb in [
                [0.0, 0.0, 0.0],
                [0.18, 0.18, 0.18],
                [1.0, 1.0, 1.0],
                [0.80, 0.32, 0.12],
                [0.08, 0.65, 0.92],
                [1.25, 0.45, 0.10],
            ] {
                let rgb = space.from_linear_srgb(linear_srgb);
                let encoded = jzazbz::from_working_rgb(rgb, space);
                let back = jzazbz::to_working_rgb(encoded, space);
                for channel in 0..3 {
                    assert!(
                        (back[channel] - rgb[channel]).abs() < 2.5e-3,
                        "{space:?} {rgb:?} -> {encoded:?} -> {back:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn jzazbz_chroma_scale_preserves_lightness_and_hue() {
        let input = [0.80, 0.32, 0.12];
        let before = jzazbz::from_working_rgb(input, WorkingColorSpace::LinearSrgb);
        let after = before.scale_chroma(0.6);
        let roundtrip = jzazbz::from_working_rgb(
            jzazbz::to_working_rgb(after, WorkingColorSpace::LinearSrgb),
            WorkingColorSpace::LinearSrgb,
        );
        let p = jzazbz::JzCzHz::from(before);
        let q = jzazbz::JzCzHz::from(roundtrip);
        let hue_error = (q.hz - p.hz)
            .abs()
            .min(std::f32::consts::TAU - (q.hz - p.hz).abs());
        assert!((q.jz - p.jz).abs() < 2.0e-5, "Jz drift: {p:?} -> {q:?}");
        assert!(
            (q.cz - p.cz * 0.6).abs() < 2.0e-5,
            "Cz drift: {p:?} -> {q:?}"
        );
        assert!(hue_error < 8.0e-5, "hue drift: {p:?} -> {q:?}");
    }
}

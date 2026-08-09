//! Shared perceptual-colour core for scene/display colour operations.
//!
//! OKLab is defined from linear-light sRGB/D65. Inputs in another scene working
//! space are transformed at the boundary, never clamped, so signed and HDR
//! values remain representable. The signed cube root makes the extension to
//! negative opponent/LMS values finite and reversible.

use super::working_color::WorkingColorSpace;

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
pub fn working_rgb_to_oklab(rgb: [f32; 3], space: WorkingColorSpace) -> Oklab {
    linear_srgb_to_oklab(space.to_linear_srgb(rgb))
}

#[inline]
pub fn oklab_to_working_rgb(lab: Oklab, space: WorkingColorSpace) -> [f32; 3] {
    space.from_linear_srgb(oklab_to_linear_srgb(lab))
}

#[inline]
pub fn working_rgb_to_perceptual(rgb: [f32; 3], space: WorkingColorSpace) -> PerceptualColor {
    PerceptualColor::from_oklab(working_rgb_to_oklab(rgb, space))
}

#[inline]
pub fn perceptual_to_working_rgb(color: PerceptualColor, space: WorkingColorSpace) -> [f32; 3] {
    oklab_to_working_rgb(color.to_oklab(), space)
}

/// Experimental HDR comparison space. Kept behind `jzazbz-prototype`; it is
/// not used by the production mixer until measurements justify the complexity.
#[cfg(feature = "jzazbz-prototype")]
pub mod jzazbz {
    use super::WorkingColorSpace;

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

    /// Forward Safdar JzAzBz prototype. Signed PQ keeps diagnostic negative RGB
    /// finite; production use will require an explicit mastering luminance.
    pub fn from_working_rgb(rgb: [f32; 3], space: WorkingColorSpace) -> JzAzBz {
        let rgb = space.to_linear_srgb(rgb);
        let x = 0.412_456_4 * rgb[0] + 0.357_576_1 * rgb[1] + 0.180_437_5 * rgb[2];
        let y = 0.212_672_9 * rgb[0] + 0.715_152_2 * rgb[1] + 0.072_175 * rgb[2];
        let z = 0.019_333_9 * rgb[0] + 0.119_192 * rgb[1] + 0.950_304_1 * rgb[2];
        let xp = 1.15 * x - 0.15 * z;
        let yp = 0.66 * y + 0.34 * x;
        let l = 0.414_789_7 * xp + 0.579_999 * yp + 0.014_648 * z;
        let m = -0.201_51 * xp + 1.120_649 * yp + 0.053_100_8 * z;
        let s = -0.016_600_8 * xp + 0.264_8 * yp + 0.668_479_9 * z;
        let pq = |v: f32| {
            const N: f32 = 0.159_301_76;
            const P: f32 = 134.034_38;
            const C1: f32 = 0.835_937_5;
            const C2: f32 = 18.851_563;
            const C3: f32 = 18.687_5;
            let sign = v.signum();
            let vn = (v.abs() / 10_000.0).powf(N);
            sign * ((C1 + C2 * vn) / (1.0 + C3 * vn)).powf(P)
        };
        let (lp, mp, sp) = (pq(l), pq(m), pq(s));
        let iz = 0.5 * (lp + mp);
        JzAzBz {
            jz: 1.629_549_9 * iz / (1.0 + 0.629_549_9 * iz) - 1.629_549_9e-11,
            az: 3.524 * lp - 4.066_708 * mp + 0.542_708 * sp,
            bz: 0.199_076 * lp + 1.096_799 * mp - 1.295_875 * sp,
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

    #[cfg(feature = "jzazbz-prototype")]
    #[test]
    fn jzazbz_prototype_is_finite_for_signed_hdr() {
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
}

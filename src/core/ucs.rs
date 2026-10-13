//! UCS 22 — the perceptually-uniform colour space the Colour Mixer's
//! selection runs in (hue only; corrections still apply in Oklab/HSL).
//!
//! UCS 22 was fitted on Munsell renotation data specifically
//! for hue/saturation *grading*: its hue angle tracks perceived hue more evenly
//! than Oklab's around the orange–yellow and blue–purple arcs (where our old
//! banded selection kept catching the wrong colours), and its colourfulness
//! axis accounts for the Helmholtz–Kohlrausch effect. Implemented from the
//! published space definition; constants are the fit parameters.
//!
//! Only the forward direction to the hue plane is needed here: display sRGB →
//! linear → XYZ (D65) → xyY → UV* prime (a rational chromaticity warp centred
//! on D65) → hue = atan2. The full JCH is exposed for tests/tuning.

/// sRGB (D65) → CIE XYZ, standard primaries.
const SRGB_TO_XYZ: [[f32; 3]; 3] = [
    [0.412_456_4, 0.357_576_1, 0.180_437_5],
    [0.212_672_9, 0.715_152_2, 0.072_175_0],
    [0.019_333_9, 0.119_192_0, 0.950_304_1],
];

#[inline]
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[inline]
fn srgb_to_xyz(r: f32, g: f32, b: f32) -> [f32; 3] {
    let lin = [srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b)];
    let row = |m: &[f32; 3]| m[0] * lin[0] + m[1] * lin[1] + m[2] * lin[2];
    [
        row(&SRGB_TO_XYZ[0]),
        row(&SRGB_TO_XYZ[1]),
        row(&SRGB_TO_XYZ[2]),
    ]
}

/// xy chromaticity → UCS 22 UV* prime. D65 (0.3127, 0.3290) maps to (0, 0) —
/// achromatic colours sit at the origin, so hue/colourfulness fall out of the
/// polar form directly. Mirrored (in f32) by the WGSL `dev_ucs_hue`.
#[inline]
fn xy_to_uv_star_prime(x: f32, y: f32) -> [f32; 2] {
    let x = x as f64;
    let y = y as f64;
    let mut d = 0.318_707_282_433_486 * x + 2.167_436_927_321_58 * y + 0.291_320_554_395_942;
    if d.abs() < 1e-12 {
        d = if d < 0.0 { -1e-12 } else { 1e-12 };
    }
    let u = (-0.783_941_002_840_055 * x + 0.277_512_987_809_202 * y + 0.153_836_578_598_858) / d;
    let v = (0.745_273_540_913_283 * x - 0.205_375_866_083_878 * y - 0.165_478_376_301_988) / d;
    let u_star = 1.396_562_256_67 * u / (u.abs() + 1.492_173_529_29);
    let v_star = 1.451_395_428_7 * v / (v.abs() + 1.524_886_379_14);
    [
        (-1.124_983_854_323_892 * u_star - 0.980_483_721_769_325 * v_star) as f32,
        (1.863_233_150_986_72 * u_star + 1.971_853_092_390_862 * v_star) as f32,
    ]
}

/// UCS 22 hue angle (radians, `(-π, π]`) of a display sRGB colour. Achromatic
/// input lands at the origin where the angle is meaningless — callers weight by
/// saturation, so the returned 0 is never visible.
pub fn ucs_hue_rad(r: f32, g: f32, b: f32) -> f32 {
    let xyz = srgb_to_xyz(r, g, b);
    let sum = (xyz[0] + xyz[1] + xyz[2]).max(1e-10);
    let uv = xy_to_uv_star_prime(xyz[0] / sum, xyz[1] / sum);
    uv[1].atan2(uv[0])
}

/// Full UCS 22 JCH (lightness J, colourfulness-based chroma C, hue H in
/// radians) of a display sRGB colour, normalised to diffuse white Y = 1.
/// Test/tuning aid — the per-pixel mixer path only needs [`ucs_hue_rad`].
#[cfg_attr(not(test), allow(dead_code))]
pub fn srgb_to_jch(r: f32, g: f32, b: f32) -> [f32; 3] {
    let xyz = srgb_to_xyz(r, g, b);
    let sum = (xyz[0] + xyz[1] + xyz[2]).max(1e-10);
    let uv = xy_to_uv_star_prime(xyz[0] / sum, xyz[1] / sum);
    let l_star = y_to_l_star(xyz[1].max(0.0));
    let l_white = y_to_l_star(1.0);
    let m2 = uv[0] * uv[0] + uv[1] * uv[1];
    [
        l_star / l_white,
        15.932_994 * l_star.powf(0.652_399_75) * m2.powf(0.600_755_7) / l_white,
        uv[1].atan2(uv[0]),
    ]
}

/// UCS 22 lightness from relative luminance Y.
#[inline]
fn y_to_l_star(y: f32) -> f32 {
    let y_hat = y.powf(0.631_651_34);
    2.098_883_8 * y_hat / (y_hat + 1.124_267_7)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deg(rad: f32) -> f32 {
        rad.to_degrees().rem_euclid(360.0)
    }

    #[test]
    fn d65_neutrals_sit_at_the_origin() {
        // The chromaticity warp is calibrated so D65 grey has (near-)zero
        // colourfulness at any lightness — the whole point of the space.
        for v in [0.05, 0.18, 0.5, 0.8, 1.0] {
            let jch = srgb_to_jch(v, v, v);
            assert!(jch[1] < 1e-3, "grey {v} has chroma {}", jch[1]);
        }
    }

    #[test]
    fn lightness_is_monotone_and_normalised() {
        let mut last = -1.0f32;
        for i in 0..=20 {
            let v = i as f32 / 20.0;
            let j = srgb_to_jch(v, v, v)[0];
            assert!(j > last, "J not monotone at {v}");
            last = j;
        }
        assert!((srgb_to_jch(1.0, 1.0, 1.0)[0] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn hue_orders_the_rainbow_consistently() {
        // The band swatch colours (R, O, Y, G, A, B, P, M) must advance
        // monotonically around the UCS hue circle — the mixer's node layout
        // depends on it. Direction (sign) doesn't matter, ordering does.
        let hues: Vec<f32> = crate::core::develop::MIXER_COLORS
            .iter()
            .map(|c| {
                ucs_hue_rad(
                    c[0] as f32 / 255.0,
                    c[1] as f32 / 255.0,
                    c[2] as f32 / 255.0,
                )
            })
            .collect();
        // Unwrap the circle starting at Reds and require strictly one direction.
        let mut unwrapped = vec![hues[0]];
        for &h in &hues[1..] {
            let prev = *unwrapped.last().unwrap();
            let mut d = h - prev;
            while d <= -std::f32::consts::PI {
                d += std::f32::consts::TAU;
            }
            while d > std::f32::consts::PI {
                d -= std::f32::consts::TAU;
            }
            unwrapped.push(prev + d);
        }
        let dir = (unwrapped[1] - unwrapped[0]).signum();
        let mut total = 0.0;
        for w in unwrapped.windows(2) {
            let step = (w[1] - w[0]) * dir;
            assert!(step > 0.0, "band hues not monotone: {unwrapped:?}");
            total += step;
        }
        assert!(
            total < std::f32::consts::TAU,
            "bands wind more than one turn: {total}"
        );
    }

    #[test]
    fn primaries_land_on_distinct_hues() {
        let r = deg(ucs_hue_rad(1.0, 0.0, 0.0));
        let g = deg(ucs_hue_rad(0.0, 1.0, 0.0));
        let b = deg(ucs_hue_rad(0.0, 0.0, 1.0));
        let sep = |a: f32, c: f32| {
            let d = (a - c).abs();
            d.min(360.0 - d)
        };
        assert!(sep(r, g) > 60.0, "R/G too close: {r} vs {g}");
        assert!(sep(g, b) > 60.0, "G/B too close: {g} vs {b}");
        assert!(sep(b, r) > 60.0, "B/R too close: {b} vs {r}");
    }

    #[test]
    fn chroma_grows_with_saturation_at_fixed_hue() {
        let jch_pale = srgb_to_jch(0.7, 0.5, 0.5);
        let jch_mid = srgb_to_jch(0.8, 0.4, 0.4);
        let jch_vivid = srgb_to_jch(0.9, 0.2, 0.2);
        assert!(jch_pale[1] < jch_mid[1] && jch_mid[1] < jch_vivid[1]);
    }
}

//! Fast scalar `log2` / `exp2` for the per-pixel scene tone chain, where the
//! platform libm versions dominated the Develop live preview (millions of
//! calls per frame). Both stay within a few f32 ulps, far below one 16-bit
//! code value, so CPU results and GPU parity are unaffected.

use std::f32::consts::SQRT_2;

/// Exponent and mantissa log of a positive normal finite `x`:
/// `log2(x) = e + p`, with `p ∈ [-0.5, 0.5]`.
#[inline]
fn log2_parts(x: f32) -> (i32, f32) {
    let bits = x.to_bits();
    let mut e = ((bits >> 23) & 0xff) as i32 - 127;
    let mut m = f32::from_bits((bits & 0x007f_ffff) | 0x3f80_0000);
    if m > SQRT_2 {
        m *= 0.5;
        e += 1;
    }
    // log2(m) = 2/ln2 · atanh(t), t = (m-1)/(m+1) ∈ [-0.172, 0.172].
    let t = (m - 1.0) / (m + 1.0);
    let t2 = t * t;
    let p = t
        * (2.885_390_1
            + t2 * (0.961_796_7 + t2 * (0.577_078_03 + t2 * (0.412_198_58 + t2 * 0.320_598_9))));
    (e, p)
}

#[inline]
fn is_positive_normal(x: f32) -> bool {
    (f32::MIN_POSITIVE..f32::INFINITY).contains(&x)
}

/// `log2(x)`; libm fallback outside positive normal finite inputs.
#[inline]
pub fn fast_log2(x: f32) -> f32 {
    if !is_positive_normal(x) {
        return x.log2();
    }
    let (e, p) = log2_parts(x);
    e as f32 + p
}

/// `2^f` for `f ∈ [-0.5, 0.5]`.
#[inline]
fn exp2_fraction(f: f32) -> f32 {
    1.0 + f
        * (std::f32::consts::LN_2
            + f * (0.240_226_5
                + f * (0.055_504_11
                    + f * (0.009_618_129
                        + f * (0.001_333_355_8 + f * (0.000_154_035_3 + f * 0.000_015_252_73))))))
}

/// `2^z` with `z` kept in f64 so a large integer part does not cost the
/// fraction its precision; `None` outside the normal f32 result range.
#[inline]
fn exp2_split(z: f64) -> Option<f32> {
    if !(z > -126.0 && z < 127.0) {
        return None;
    }
    // Round to nearest by adding 1.5·2^52: plain SSE2 adds, where `round()`
    // is a libm call on the baseline x86-64 target.
    const ROUND: f64 = 6_755_399_441_055_744.0;
    let n = (z + ROUND) - ROUND;
    let scale = f32::from_bits(((n as i32 + 127) as u32) << 23);
    Some(exp2_fraction((z - n) as f32) * scale)
}

/// `2^x`; libm fallback outside the normal-result range.
#[inline]
pub fn fast_exp2(x: f32) -> f32 {
    exp2_split(x as f64).unwrap_or_else(|| x.exp2())
}

/// Exactly `v.round().clamp(0.0, 255.0) as u8` (NaN gives 0), with no libm
/// `roundf` call: per-pixel loops that round three or four channels spent
/// most of their time there. The truncated part is exact, so the half test is.
#[inline]
pub fn round_u8(v: f32) -> u8 {
    let v = v.clamp(0.0, 255.0);
    let t = v as u8;
    if v - t as f32 >= 0.5 { t + 1 } else { t }
}

/// Exactly `v.round().clamp(0.0, 65535.0) as u16` (NaN gives 0); see [`round_u8`].
#[inline]
pub fn round_u16(v: f32) -> u16 {
    let v = v.clamp(0.0, 65535.0);
    let t = v as u16;
    if v - t as f32 >= 0.5 { t + 1 } else { t }
}

/// Exactly `v.floor() as i32` (saturating, NaN gives 0), with no libm call.
#[inline]
pub fn floor_i32(v: f32) -> i32 {
    let t = v as i32;
    if t as f32 > v { t.saturating_sub(1) } else { t }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples() -> impl Iterator<Item = f32> {
        // Dense log-spaced sweep over the ranges the colour chains feed in.
        (0..200_000).map(|i| (-40.0 + 60.0 * i as f32 / 200_000.0).exp2())
    }

    #[test]
    fn log2_matches_libm() {
        let mut worst = 0.0f32;
        for x in samples() {
            worst = worst.max((fast_log2(x) - x.log2()).abs());
        }
        assert!(worst < 4.0e-6, "log2 abs error {worst}");
        assert_eq!(fast_log2(1.0), 0.0);
        assert_eq!(fast_log2(8.0), 3.0);
        assert!(fast_log2(0.0) == f32::NEG_INFINITY && fast_log2(-1.0).is_nan());
    }

    #[test]
    fn exp2_matches_libm() {
        let mut worst = 0.0f32;
        for i in 0..200_000 {
            let x = -60.0 + 120.0 * i as f32 / 200_000.0;
            let exact = x.exp2();
            worst = worst.max(((fast_exp2(x) - exact) / exact).abs());
        }
        assert!(worst < 4.0e-7, "exp2 rel error {worst}");
        assert_eq!(fast_exp2(0.0), 1.0);
        assert_eq!(fast_exp2(-200.0), 0.0);
    }

    #[test]
    fn rounding_and_floor_match_libm_bit_for_bit() {
        let mut edges = vec![
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            -0.0,
            0.0,
            0.5,
            0.499_999_97,
            254.5,
            255.0,
            255.5,
            65_534.5,
            65_535.4,
            1.0e10,
            -1.0e10,
            2_147_483_520.0,
            -2_147_483_648.0,
        ];
        // Every f32 step around each half and integer in the 8-bit range, and
        // a sweep across the 16-bit range.
        for k in 0..=512 {
            let mid = k as f32 * 0.5;
            let mut v = mid;
            for _ in 0..4 {
                v = v.next_down();
            }
            for _ in 0..9 {
                edges.push(v);
                edges.push(-v);
                v = v.next_up();
            }
        }
        edges.extend((0..400_000).map(|i| -100.0 + 66_000.0 * i as f32 / 400_000.0));
        for v in edges {
            assert_eq!(
                round_u8(v),
                v.round().clamp(0.0, 255.0) as u8,
                "round_u8({v})"
            );
            assert_eq!(
                round_u16(v),
                v.round().clamp(0.0, 65535.0) as u16,
                "round_u16({v})"
            );
            assert_eq!(floor_i32(v), v.floor() as i32, "floor_i32({v})");
            assert_eq!(
                floor_i32(v * 0.01),
                (v * 0.01).floor() as i32,
                "floor_i32({v}/100)"
            );
        }
    }
}

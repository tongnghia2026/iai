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
    x >= f32::MIN_POSITIVE && x < f32::INFINITY
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
        * (0.693_147_2
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
}

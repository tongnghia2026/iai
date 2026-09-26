//! Leaf numeric and colour-space primitives shared by every Develop stage.
//!
//! Pure functions with no dependencies on the rest of the module — the base of
//! the dependency graph.

use super::CONTROL_LIMIT;

pub fn control_to_unit(value: f32) -> f32 {
    (value / CONTROL_LIMIT).clamp(-1.0, 1.0)
}

pub(crate) fn eased_control(value: f32) -> f32 {
    control_to_unit(value)
}

pub(crate) fn shift_channel(v: f32, delta: f32) -> f32 {
    let delta = delta.clamp(-1.0, 1.0);
    if delta >= 0.0 {
        lerp(v, 1.0, delta)
    } else {
        lerp(v, 0.0, -delta)
    }
}

pub(crate) fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

pub(crate) fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else if c <= 1.0 {
        let t = c * (TRANSFER_LUT - 1) as f32;
        lut_at(&TRANSFER_LUTS.to_linear, t)
    } else {
        srgb_to_linear_exact(c)
    }
}

pub(crate) fn linear_to_srgb(c: f32) -> f32 {
    let c = c.max(0.0);
    if c <= 0.0031308 {
        c * 12.92
    } else if c <= 1.0 {
        // Indexed by sqrt(c): the curve is far flatter there, so linear
        // interpolation stays within ~3e-8.
        let t = c.sqrt() * (TRANSFER_LUT - 1) as f32;
        lut_at(&TRANSFER_LUTS.to_srgb, t)
    } else {
        linear_to_srgb_exact(c)
    }
}

fn srgb_to_linear_exact(c: f32) -> f32 {
    ((c + 0.055) / 1.055).powf(2.4)
}

fn linear_to_srgb_exact(c: f32) -> f32 {
    1.055 * c.powf(1.0 / 2.4) - 0.055
}

/// Entries per sRGB transfer table; the per-pixel colour chains call these
/// millions of times per frame, where libm `powf` dominated.
const TRANSFER_LUT: usize = 4096;

struct TransferLuts {
    to_linear: Box<[f32]>,
    to_srgb: Box<[f32]>,
}

static TRANSFER_LUTS: std::sync::LazyLock<TransferLuts> = std::sync::LazyLock::new(|| {
    let step = 1.0 / (TRANSFER_LUT - 1) as f64;
    // Power segment only: the tables serve inputs past the linear toe, and a
    // toe entry would bend the cell that straddles the joint.
    let to_linear = (0..TRANSFER_LUT)
        .map(|i| ((i as f64 * step + 0.055) / 1.055).powf(2.4) as f32)
        .collect();
    let to_srgb = (0..TRANSFER_LUT)
        .map(|i| (1.055 * (i as f64 * step).powf(2.0 / 2.4) - 0.055) as f32)
        .collect();
    TransferLuts { to_linear, to_srgb }
});

#[inline]
fn lut_at(lut: &[f32], t: f32) -> f32 {
    let i = (t as usize).min(lut.len() - 2);
    let f = t - i as f32;
    lut[i] + (lut[i + 1] - lut[i]) * f
}

/// Rec.709 luminance on LINEAR-light values (used by the white-balance and
/// exposure stages, which run in linear). The gamma-space stages keep using
/// `luminance_f32` (Rec.601 on sRGB) for their perceptual tone masks.
pub(crate) fn luma_lin(r: f32, g: f32, b: f32) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

pub(crate) fn fit_linear_rgb_to_luma(mut c: [f32; 3], target_luma: f32) -> [f32; 3] {
    let target = target_luma.clamp(0.0, 1.0);
    let mn = c[0].min(c[1]).min(c[2]);
    if mn < 0.0 {
        let anchor = target.max(0.0001);
        let scale = (anchor / (anchor - mn).max(0.00001)).clamp(0.0, 1.0);
        for v in &mut c {
            *v = target + (*v - target) * scale;
        }
    }
    let mx = c[0].max(c[1]).max(c[2]);
    if mx > 1.0 {
        let scale = ((1.0 - target) / (mx - target).max(0.00001)).clamp(0.0, 1.0);
        for v in &mut c {
            *v = target + (*v - target) * scale;
        }
    }
    [
        c[0].clamp(0.0, 1.0),
        c[1].clamp(0.0, 1.0),
        c[2].clamp(0.0, 1.0),
    ]
}

pub(crate) fn clamp_unit(r: &mut f32, g: &mut f32, b: &mut f32) {
    *r = r.clamp(0.0, 1.0);
    *g = g.clamp(0.0, 1.0);
    *b = b.clamp(0.0, 1.0);
}

#[cfg(test)]
mod transfer_lut_tests {
    use super::*;

    #[test]
    fn transfer_luts_match_the_exact_curves() {
        let exact_to_linear = |c: f32| {
            if c <= 0.04045 {
                c / 12.92
            } else {
                srgb_to_linear_exact(c)
            }
        };
        let exact_to_srgb = |c: f32| {
            if c <= 0.0031308 {
                c * 12.92
            } else {
                linear_to_srgb_exact(c)
            }
        };
        let (mut worst_lin, mut worst_srgb) = (0.0f32, 0.0f32);
        for i in 0..=1_000_000 {
            let c = i as f32 / 1_000_000.0;
            worst_lin = worst_lin.max((srgb_to_linear(c) - exact_to_linear(c)).abs());
            worst_srgb = worst_srgb.max((linear_to_srgb(c) - exact_to_srgb(c)).abs());
        }
        assert!(worst_lin < 3.0e-7, "srgb_to_linear error {worst_lin}");
        assert!(worst_srgb < 3.0e-7, "linear_to_srgb error {worst_srgb}");
        assert_eq!(srgb_to_linear(1.0), 1.0);
        assert_eq!(linear_to_srgb(1.0), 1.0);
        assert!((linear_to_srgb(1.5) - exact_to_srgb(1.5)).abs() < 1e-6);
        assert_eq!(linear_to_srgb(-0.2), 0.0);
    }
}

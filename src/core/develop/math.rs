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
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

pub(crate) fn linear_to_srgb(c: f32) -> f32 {
    let c = c.max(0.0);
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
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

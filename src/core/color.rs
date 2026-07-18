#![allow(dead_code)]

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const BLACK: Color = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };
    pub const WHITE: Color = Color {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };
    pub const TRANSPARENT: Color = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };

    #[inline]
    pub fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
    #[inline]
    pub fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::new(r, g, b, 255)
    }

    pub fn from_array(arr: [u8; 4]) -> Self {
        Self {
            r: arr[0],
            g: arr[1],
            b: arr[2],
            a: arr[3],
        }
    }
    pub fn to_array(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }

    /// Convert to a float tuple [0..1].
    #[inline]
    pub fn to_f32(self) -> (f32, f32, f32, f32) {
        (
            self.r as f32 / 255.0,
            self.g as f32 / 255.0,
            self.b as f32 / 255.0,
            self.a as f32 / 255.0,
        )
    }

    /// Create a Color from float [0..1].
    #[inline]
    pub fn from_f32(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self::new(
            (r.clamp(0.0, 1.0) * 255.0) as u8,
            (g.clamp(0.0, 1.0) * 255.0) as u8,
            (b.clamp(0.0, 1.0) * 255.0) as u8,
            (a.clamp(0.0, 1.0) * 255.0) as u8,
        )
    }

    pub fn luminance(self) -> f32 {
        let (r, g, b, _) = self.to_f32();
        0.299 * r + 0.587 * g + 0.114 * b
    }

    /// Alpha compositing: self over dst
    pub fn over(self, dst: Color) -> Color {
        let (sr, sg, sb, sa) = self.to_f32();
        let (dr, dg, db, da) = dst.to_f32();
        let out_a = sa + da * (1.0 - sa);
        if out_a < 0.001 {
            return Color::TRANSPARENT;
        }
        Color::from_f32(
            (sr * sa + dr * da * (1.0 - sa)) / out_a,
            (sg * sa + dg * da * (1.0 - sa)) / out_a,
            (sb * sa + db * da * (1.0 - sa)) / out_a,
            out_a,
        )
    }
}

impl From<[u8; 4]> for Color {
    fn from(arr: [u8; 4]) -> Self {
        Self::from_array(arr)
    }
}
impl From<Color> for [u8; 4] {
    fn from(c: Color) -> Self {
        c.to_array()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct HslColor {
    pub h: f32,
    pub s: f32,
    pub l: f32,
    pub a: f32,
}

impl HslColor {
    pub fn new(h: f32, s: f32, l: f32, a: f32) -> Self {
        Self { h, s, l, a }
    }

    pub fn to_color(self) -> Color {
        let (r, g, b) = hsl_to_rgb(self.h, self.s, self.l);
        Color::from_f32(r, g, b, self.a)
    }

    /// Hue trong degrees [0..360]
    pub fn hue_degrees(self) -> f32 {
        self.h * 360.0
    }

    pub fn from_color(c: Color) -> Self {
        let (r, g, b, a) = c.to_f32();
        let (h, s, l) = rgb_to_hsl(r, g, b);
        Self { h, s, l, a }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct HsvColor {
    pub h: f32,
    pub s: f32,
    pub v: f32,
    pub a: f32,
}

impl HsvColor {
    pub fn new(h: f32, s: f32, v: f32, a: f32) -> Self {
        Self { h, s, v, a }
    }

    pub fn to_color(self) -> Color {
        let (r, g, b) = hsv_to_rgb(self.h, self.s, self.v);
        Color::from_f32(r, g, b, self.a)
    }

    pub fn from_color(c: Color) -> Self {
        let (r, g, b, a) = c.to_f32();
        let (h, s, v) = rgb_to_hsv(r, g, b);
        Self { h, s, v, a }
    }
}

/// RGB [0..1] → HSL [0..1]
pub fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;

    if (max - min) < 0.0001 {
        return (0.0, 0.0, l);
    }

    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };

    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } / 6.0;

    (h, s, l)
}

/// HSL [0..1] → RGB [0..1]
pub fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    if s < 0.0001 {
        return (l, l, l);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let hue = |mut t: f32| -> f32 {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            return p + (q - p) * 6.0 * t;
        }
        if t < 0.5 {
            return q;
        }
        if t < 2.0 / 3.0 {
            return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
        }
        p
    };
    (hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0))
}

/// RGB [0..1] → HSV — returns a tuple (h, s, v) [0..1].
pub fn rgb_to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let v = max;
    let s = if max < 0.0001 { 0.0 } else { (max - min) / max };
    let h = if (max - min) < 0.0001 {
        0.0
    } else if max == r {
        ((g - b) / (max - min) + if g < b { 6.0 } else { 0.0 }) / 6.0
    } else if max == g {
        ((b - r) / (max - min) + 2.0) / 6.0
    } else {
        ((r - g) / (max - min) + 4.0) / 6.0
    };
    (h, s, v)
}

/// HSV [0..1] → RGB [0..1]
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    if s < 0.0001 {
        return (v, v, v);
    }
    let h6 = h * 6.0;
    let i = h6.floor() as u32 % 6;
    let f = h6 - h6.floor();
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    match i {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}

/// Perceptual luminance (Rec. 709 weights, matching `luma_lin` in `develop.rs`).
/// Used on gamma-encoded sRGB values for the tone-curve and colour-mask stages.
#[inline]
pub fn luminance_f32(r: f32, g: f32, b: f32) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// Sample a color-stop gradient at `t` (0..1). `stops` are `(position, rgb)`,
/// sorted ascending by position; values of `t` outside the stop range clamp to
/// the nearest endpoint. Linear interpolation in sRGB. Shared single source of
/// truth for the Gradient Map adjustment (`apply_pixel`) and its editor UI so the
/// drawn bar matches the applied result.
pub fn sample_gradient_stops(stops: &[(f32, [u8; 3])], t: f32) -> [u8; 3] {
    match stops {
        [] => [0, 0, 0],
        [one] => one.1,
        _ => {
            let t = t.clamp(0.0, 1.0);
            let last = stops.len() - 1;
            if t <= stops[0].0 {
                return stops[0].1;
            }
            if t >= stops[last].0 {
                return stops[last].1;
            }
            for i in 0..last {
                let (p0, c0) = stops[i];
                let (p1, c1) = stops[i + 1];
                if t >= p0 && t <= p1 {
                    let range = p1 - p0;
                    if range < 1e-6 {
                        return c1;
                    }
                    let lt = (t - p0) / range;
                    let inv = 1.0 - lt;
                    return [
                        (c0[0] as f32 * inv + c1[0] as f32 * lt).round() as u8,
                        (c0[1] as f32 * inv + c1[1] as f32 * lt).round() as u8,
                        (c0[2] as f32 * inv + c1[2] as f32 * lt).round() as u8,
                    ];
                }
            }
            stops[last].1
        }
    }
}

// ── Perceptual selective-colour weighting (Oklab / Oklch hue) ────────────────
//
// Selective-colour bands (the Hue / Saturation / Luminance "Color Mixer": Reds,
// Oranges, Yellows … Magentas) decide which pixels to push by their HUE. Doing
// that classification in HSL hue is the root of the harsh, banded transitions:
// equal HSL-degree steps are NOT equal perceived-hue steps, so a smooth ramp in
// HSL degrees ramps unevenly through the red–orange–yellow / skin-tone arc and
// reads as hard edges. Classifying by Oklab/Oklch hue — which is perceptually
// near-uniform — makes the band edges fall where the eye expects them. Oklab is
// chosen over CIELAB because its hue lines stay straight (no blue-shift) and over
// HSV/HSL because those are not perceptual at all; only the hue ANGLE is needed,
// so the cost is one linear-light transform + three cbrt + an atan2 per pixel,
// cheap enough for the real-time tile bake and matched exactly by the GPU shader.

/// Selective-colour band count: Reds, Oranges, Yellows, Greens, Aquas, Blues,
/// Purples, Magentas.
pub const SELECTIVE_BANDS: usize = 8;

/// Band centres on the Oklab/Oklch hue circle (degrees). Mostly the perceptual
/// hue angles of the canonical band colours — the same reference hues the legacy
/// HSL centres (0/30/60/120/180/240/285/320) stood for — so a pixel on a band's
/// reference colour is "pure": only that band is active there. Exception: Yellows
/// is pulled down from pure yellow (≈109.7°) toward orange so the Yellow slider
/// grips the orange-yellow / skin-tone range real photos contain.
/// MUST match `dev_band_center` in the compositor WGSL.
pub const SELECTIVE_HUE_CENTERS: [f32; SELECTIVE_BANDS] =
    [27.80, 59.03, 90.00, 142.63, 194.83, 267.88, 314.93, 346.74];

#[inline]
fn srgb_to_linear_scalar(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[inline]
fn linear_to_srgb_scalar(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Rotate a colour's **Oklab hue** by `deg` degrees, preserving its Oklab
/// lightness and chroma. The Develop Color Mixer selects bands by Oklab hue
/// ([`oklab_hue_deg`]), so shifting hue in the SAME perceptual space keeps the
/// move even and correctly-directed (the old HSL rotation shifted in a different
/// space than the selection, which felt uneven). An achromatic input is a no-op
/// (chroma ≈ 0 → rotation moves nothing).
pub fn rotate_oklab_hue(r: f32, g: f32, b: f32, deg: f32) -> (f32, f32, f32) {
    if deg.abs() < 1e-4 {
        return (r, g, b);
    }
    let lr = srgb_to_linear_scalar(r);
    let lg = srgb_to_linear_scalar(g);
    let lb = srgb_to_linear_scalar(b);
    let l = 0.4122214708 * lr + 0.5363325363 * lg + 0.0514459929 * lb;
    let m = 0.2119034982 * lr + 0.6806995451 * lg + 0.1073969566 * lb;
    let s = 0.0883024619 * lr + 0.2817188376 * lg + 0.6299787005 * lb;
    let l_ = l.cbrt();
    let m_ = m.cbrt();
    let s_ = s.cbrt();
    let big_l = 0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_;
    let aa = 1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_;
    let bb = 0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_;

    let rad = deg.to_radians();
    let (sn, cs) = rad.sin_cos();
    let na = aa * cs - bb * sn;
    let nb = aa * sn + bb * cs;

    let l2_ = big_l + 0.3963377774 * na + 0.2158037573 * nb;
    let m2_ = big_l - 0.1055613458 * na - 0.0638541728 * nb;
    let s2_ = big_l - 0.0894841775 * na - 1.2914855480 * nb;
    let l2 = l2_ * l2_ * l2_;
    let m2 = m2_ * m2_ * m2_;
    let s2 = s2_ * s2_ * s2_;
    let lr2 = 4.0767416621 * l2 - 3.3077115913 * m2 + 0.2309699292 * s2;
    let lg2 = -1.2684380046 * l2 + 2.6097574011 * m2 - 0.3413193965 * s2;
    let lb2 = -0.0041960863 * l2 - 0.7034186147 * m2 + 1.7076147010 * s2;
    (
        linear_to_srgb_scalar(lr2),
        linear_to_srgb_scalar(lg2),
        linear_to_srgb_scalar(lb2),
    )
}

/// Oklab/Oklch hue of an sRGB colour (channels in [0,1]) as degrees in [0,360).
/// Returns 0.0 for an achromatic input, where hue is undefined — the caller's
/// chroma gate fades those pixels out anyway. Kept in f32; the GPU `dev_oklab_hue`
/// mirrors this exactly so preview and bake agree.
#[inline]
pub fn oklab_hue_deg(r: f32, g: f32, b: f32) -> f32 {
    let lr = srgb_to_linear_scalar(r);
    let lg = srgb_to_linear_scalar(g);
    let lb = srgb_to_linear_scalar(b);
    let l = 0.4122214708 * lr + 0.5363325363 * lg + 0.0514459929 * lb;
    let m = 0.2119034982 * lr + 0.6806995451 * lg + 0.1073969566 * lb;
    let s = 0.0883024619 * lr + 0.2817188376 * lg + 0.6299787005 * lb;
    let l_ = l.cbrt();
    let m_ = m.cbrt();
    let s_ = s.cbrt();
    let a = 1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_;
    let bb = 0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_;
    if a.abs() < 1e-7 && bb.abs() < 1e-7 {
        return 0.0;
    }
    let deg = bb.atan2(a).to_degrees();
    if deg < 0.0 {
        deg + 360.0
    } else {
        deg
    }
}

/// Smooth partition-of-unity band weights for a perceptual hue (degrees). Exactly
/// the two bands bracketing the hue are non-zero and their weights sum to 1, so
/// the *total* selective-colour strength is constant around the whole hue circle —
/// this is what removes the pulsing/banding. Adjacent centres are blended with
/// smootherstep (C1-continuous, zero slope at every centre, so a band's influence
/// peaks cleanly at its centre and fades to zero at its neighbours), and the
/// red↔magenta wrap is handled circularly. Single source of truth shared by the
/// CPU camera-raw bake and the WGSL preview shader.
#[inline]
pub fn selective_hue_weights(hue_deg: f32) -> [f32; SELECTIVE_BANDS] {
    let centers = SELECTIVE_HUE_CENTERS;
    let deg = hue_deg.rem_euclid(360.0);

    // Largest centre <= deg. If deg is below the first centre it falls in the wrap
    // segment between the last and first centres (lo stays at the last band).
    let mut lo = SELECTIVE_BANDS - 1;
    for i in 0..SELECTIVE_BANDS {
        if deg >= centers[i] {
            lo = i;
        } else {
            break;
        }
    }
    let hi = (lo + 1) % SELECTIVE_BANDS;
    let c_lo = centers[lo];
    let c_hi = if hi == 0 {
        centers[hi] + 360.0
    } else {
        centers[hi]
    };
    let d = if deg < c_lo { deg + 360.0 } else { deg };
    let frac = ((d - c_lo) / (c_hi - c_lo).max(1.0)).clamp(0.0, 1.0);
    let w_hi = smoothstep01(frac);

    let mut w = [0.0f32; SELECTIVE_BANDS];
    w[lo] = 1.0 - w_hi;
    w[hi] = w_hi;
    w
}

/// Quintic smootherstep on a pre-normalised t in [0,1] (Perlin's
/// 6t⁵−15t⁴+10t³): zero first *and* second derivative at both ends.
#[inline]
fn smoothstep01(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

#[inline]
fn smootherstep_edges(edge0: f32, edge1: f32, x: f32) -> f32 {
    smoothstep01((x - edge0) / (edge1 - edge0))
}

/// Largest extra chroma scale (`f − 1`) that keeps `luma + d·f` inside [0,1] for
/// every channel — the distance from the colour to the RGB-cube gamut hull along
/// its constant-luma chroma direction. 0 when the colour already sits on the hull.
#[inline]
fn chroma_headroom(dr: f32, dg: f32, db: f32, luma: f32) -> f32 {
    let mut room = f32::INFINITY;
    for d in [dr, dg, db] {
        if d > 1e-6 {
            room = room.min((1.0 - luma) / d - 1.0);
        } else if d < -1e-6 {
            room = room.min(luma / -d - 1.0);
        }
    }
    if room.is_finite() {
        room.max(0.0)
    } else {
        0.0
    }
}

/// Scale colourfulness by moving the RGB chroma vector around a *fixed* luminance.
/// `factor > 1` saturates, `< 1` desaturates. A perceptual `protect` mask eases
/// the effect out of deep shadows and near-white so those don't posterise.
///
/// The important part is the **gamut-aware soft knee** on the way up: instead of
/// scaling past the RGB cube and hard-clipping — which pins channels flat and
/// makes the two-stage rescale shift hue, i.e. the blocky over-saturated speckle
/// and neon blow-out when a colour band is pushed to the maximum — a positive
/// request is compressed with `tanh` into the pixel's actual headroom, so it
/// asymptotes smoothly to the gamut hull. The whole chroma vector is scaled by one
/// factor, so hue is preserved exactly and luminance is left untouched. Single
/// source of truth for the Develop mixer/saturation/vibrance and the
/// Hue/Saturation adjustment; the WGSL shaders mirror this math.
#[inline]
pub fn saturate_around_luma(r: f32, g: f32, b: f32, factor: f32) -> (f32, f32, f32) {
    let luma = luminance_f32(r, g, b).clamp(0.0, 1.0);
    let protect =
        smootherstep_edges(0.035, 0.14, luma) * (1.0 - smootherstep_edges(0.90, 0.99, luma));
    let req = (factor.clamp(0.0, 2.35) - 1.0) * protect;
    let dr = r - luma;
    let dg = g - luma;
    let db = b - luma;
    let f = if req > 0.0 {
        let room = chroma_headroom(dr, dg, db, luma);
        if room > 1e-4 {
            1.0 + room * (req / room).tanh()
        } else {
            1.0
        }
    } else {
        // Desaturation only shrinks chroma toward luma — always in gamut.
        1.0 + req
    };
    (
        (luma + dr * f).clamp(0.0, 1.0),
        (luma + dg * f).clamp(0.0, 1.0),
        (luma + db * f).clamp(0.0, 1.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsl_roundtrip() {
        let cases = [
            (1.0f32, 0.0, 0.0),
            (0.0, 1.0, 0.0),
            (0.0, 0.0, 1.0),
            (1.0, 1.0, 0.0),
            (0.5, 0.5, 0.5),
        ];
        for (r, g, b) in cases {
            let (h, s, l) = rgb_to_hsl(r, g, b);
            let (r2, g2, b2) = hsl_to_rgb(h, s, l);
            assert!((r - r2).abs() < 0.001, "r fail: {r} → {r2}");
            assert!((g - g2).abs() < 0.001, "g fail: {g} → {g2}");
            assert!((b - b2).abs() < 0.001, "b fail: {b} → {b2}");
        }
    }

    #[test]
    fn hsv_roundtrip() {
        for &(r, g, b) in &[(1.0f32, 0.0, 0.0), (0.0, 1.0, 0.0), (0.5, 0.3, 0.8)] {
            let (h, s, v) = rgb_to_hsv(r, g, b);
            let (r2, g2, b2) = hsv_to_rgb(h, s, v);
            assert!((r - r2).abs() < 0.001 && (g - g2).abs() < 0.001 && (b - b2).abs() < 0.001);
        }
    }

    #[test]
    fn color_over_compositing() {
        let red = Color::new(255, 0, 0, 128);
        let blue = Color::new(0, 0, 255, 255);
        let result = red.over(blue);
        assert!(result.a == 255, "alpha should be 255");
        assert!(
            result.r > 0 && result.b > 0,
            "should be mix of red and blue"
        );
    }

    #[test]
    fn selective_weights_are_partition_of_unity_all_around_the_circle() {
        // For every hue the active band weights must sum to 1 (constant total
        // strength → no pulsing/banding) and never go negative.
        let mut deg = 0.0f32;
        while deg < 360.0 {
            let w = selective_hue_weights(deg);
            let sum: f32 = w.iter().sum();
            assert!((sum - 1.0).abs() < 1e-4, "sum at {deg}° = {sum}");
            assert!(w.iter().all(|&x| x >= -1e-6 && x <= 1.0 + 1e-6));
            deg += 0.37;
        }
    }

    #[test]
    fn selective_weights_are_pure_at_each_centre() {
        // On a band's own centre only that band is active.
        for (i, &c) in SELECTIVE_HUE_CENTERS.iter().enumerate() {
            let w = selective_hue_weights(c);
            assert!((w[i] - 1.0).abs() < 1e-3, "centre {i} weight {}", w[i]);
            for (j, &wj) in w.iter().enumerate() {
                if j != i {
                    assert!(wj < 1e-3, "band {j} leaked {wj} at centre {i}");
                }
            }
        }
    }

    #[test]
    fn selective_weights_are_continuous_across_the_red_magenta_wrap() {
        // No discontinuity stepping across 0°/360° (the red↔magenta seam).
        let a = selective_hue_weights(359.9);
        let b = selective_hue_weights(0.1);
        let max_jump = a
            .iter()
            .zip(b.iter())
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max);
        assert!(max_jump < 0.05, "wrap discontinuity {max_jump}");
    }

    #[test]
    fn saturate_preserves_luma_and_never_clips_out_of_gamut() {
        let samples = [
            (0.90, 0.71, 0.63), // skin
            (0.82, 0.27, 0.27), // lips
            (0.20, 0.12, 0.08), // dark warm
            (0.50, 0.50, 0.50), // neutral
            (0.99, 0.98, 0.97), // near white
        ];
        for &(r, g, b) in &samples {
            for &factor in &[2.35f32, 1.6, 0.4] {
                let (nr, ng, nb) = saturate_around_luma(r, g, b, factor);
                // Stays inside the RGB cube (no hard clip needed).
                for v in [nr, ng, nb] {
                    assert!((0.0..=1.0).contains(&v), "out of gamut {v} @ f={factor}");
                }
                // Luminance is preserved (chroma scaled around luma).
                let l0 = luminance_f32(r, g, b);
                let l1 = luminance_f32(nr, ng, nb);
                assert!((l0 - l1).abs() < 1e-3, "luma drift {l0}->{l1}");
            }
        }
    }

    #[test]
    fn max_saturation_keeps_neighbouring_skin_pixels_distinct() {
        // The blocky speckle bug was channels pinning flat at the clip so adjacent
        // skin values collapsed to the same colour. At max boost they must stay
        // separated (monotonic), not merge.
        let a = saturate_around_luma(0.894, 0.714, 0.635, 2.35);
        let b = saturate_around_luma(0.906, 0.722, 0.627, 2.35);
        let diff = (a.0 - b.0).abs() + (a.1 - b.1).abs() + (a.2 - b.2).abs();
        assert!(
            diff > 0.01,
            "neighbouring skin pixels collapsed: {a:?} vs {b:?}"
        );
    }

    #[test]
    fn oklab_hue_matches_known_anchors() {
        // Pure-ish primaries land near their expected perceptual angles.
        let red = oklab_hue_deg(1.0, 0.0, 0.0);
        assert!((20.0..40.0).contains(&red), "red hue {red}");
        let green = oklab_hue_deg(0.0, 1.0, 0.0);
        assert!((130.0..150.0).contains(&green), "green hue {green}");
        let blue = oklab_hue_deg(0.0, 0.0, 1.0);
        assert!((250.0..280.0).contains(&blue), "blue hue {blue}");
        // Achromatic input is reported as 0 (undefined hue), not NaN.
        assert_eq!(oklab_hue_deg(0.5, 0.5, 0.5), 0.0);
    }

    #[test]
    fn rotate_oklab_hue_advances_hue_and_noop_on_gray() {
        // A mild red (stays in gamut after a moderate rotation).
        let (r, g, b) = (0.60, 0.38, 0.38);
        let h0 = oklab_hue_deg(r, g, b);
        let (nr, ng, nb) = rotate_oklab_hue(r, g, b, 40.0);
        let h1 = oklab_hue_deg(nr, ng, nb);
        let dh = ((h1 - h0 + 540.0) % 360.0) - 180.0; // signed shortest delta
        assert!(dh > 20.0 && dh < 60.0, "hue should advance ~40° (got {dh})");
        // Lightness/luma roughly preserved (Oklab rotation keeps L).
        assert!(
            (luminance_f32(nr, ng, nb) - luminance_f32(r, g, b)).abs() < 0.06,
            "luma should be roughly preserved"
        );
        // Gray → hue undefined → rotation is a no-op.
        let (gr, gg, gb) = rotate_oklab_hue(0.5, 0.5, 0.5, 90.0);
        assert!((gr - 0.5).abs() < 0.01 && (gg - 0.5).abs() < 0.01 && (gb - 0.5).abs() < 0.01);
    }
}

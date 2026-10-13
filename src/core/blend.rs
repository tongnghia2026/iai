// BlendMode enum and all pixel-level blend operations.
// The only file with blend math — layer.rs calls into it.
//
// To add a blend mode:
//   1. Add a variant to BlendMode.
//   2. Add a case in blend_pixel().
//   3. Add a name in BlendMode::name().
//   4. Add it to BlendMode::all().

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum BlendMode {
    #[default]
    Normal,
    Dissolve,
    Multiply,
    ColorBurn,
    Darken,
    Screen,
    ColorDodge,
    Lighten,
    Overlay,
    SoftLight,
    HardLight,
    LinearLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl BlendMode {
    pub fn name(self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Dissolve => "Dissolve",
            BlendMode::Multiply => "Multiply",
            BlendMode::Screen => "Screen",
            BlendMode::Overlay => "Overlay",
            BlendMode::Darken => "Darken",
            BlendMode::Lighten => "Lighten",
            BlendMode::ColorDodge => "Color Dodge",
            BlendMode::ColorBurn => "Color Burn",
            BlendMode::HardLight => "Hard Light",
            BlendMode::SoftLight => "Soft Light",
            BlendMode::LinearLight => "Linear Light",
            BlendMode::Difference => "Difference",
            BlendMode::Exclusion => "Exclusion",
            BlendMode::Hue => "Hue",
            BlendMode::Saturation => "Saturation",
            BlendMode::Color => "Color",
            BlendMode::Luminosity => "Luminosity",
        }
    }

    pub fn all() -> &'static [BlendMode] {
        &[
            BlendMode::Normal,
            BlendMode::Dissolve,
            BlendMode::Multiply,
            BlendMode::ColorBurn,
            BlendMode::Darken,
            BlendMode::Screen,
            BlendMode::ColorDodge,
            BlendMode::Lighten,
            BlendMode::Overlay,
            BlendMode::SoftLight,
            BlendMode::HardLight,
            BlendMode::LinearLight,
            BlendMode::Difference,
            BlendMode::Exclusion,
            BlendMode::Hue,
            BlendMode::Saturation,
            BlendMode::Color,
            BlendMode::Luminosity,
        ]
    }
}

/// Deterministic 2D hash cho Dissolve blend mode.
/// The same (x, y) always gives the same result → stable pattern when unchanged.
#[inline]
pub fn dissolve_threshold(x: u32, y: u32) -> f32 {
    let mut h = x
        .wrapping_mul(0x9e3779b9)
        .wrapping_add(y.wrapping_mul(0x517cc1b7));
    h ^= h >> 16;
    h = h.wrapping_mul(0x45d9f3b7);
    h ^= h >> 16;
    (h as f32) / (u32::MAX as f32 + 1.0)
}

// The non-separable blend helpers below follow the PDF/W3C compositing
// definition used by Adobe-style RGB blend modes. This is intentionally not
// an HSL conversion: the blend-space luminosity is the legacy 0.30/0.59/0.11
// weighted sum and saturation is max(R,G,B) - min(R,G,B).
#[inline]
fn blend_lum(c: [f32; 3]) -> f32 {
    0.30 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

#[inline]
fn blend_sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

#[inline]
fn clip_blend_color(mut c: [f32; 3]) -> [f32; 3] {
    let l = blend_lum(c);
    let min_c = c[0].min(c[1]).min(c[2]);
    let max_c = c[0].max(c[1]).max(c[2]);

    if min_c < 0.0 {
        let scale = l / (l - min_c);
        for channel in &mut c {
            *channel = l + (*channel - l) * scale;
        }
    }
    if max_c > 1.0 {
        let scale = (1.0 - l) / (max_c - l);
        for channel in &mut c {
            *channel = l + (*channel - l) * scale;
        }
    }
    c
}

#[inline]
fn set_blend_lum(mut c: [f32; 3], l: f32) -> [f32; 3] {
    let delta = l - blend_lum(c);
    for channel in &mut c {
        *channel += delta;
    }
    clip_blend_color(c)
}

#[inline]
fn set_blend_sat(c: [f32; 3], saturation: f32) -> [f32; 3] {
    let range = blend_sat(c);
    let mut out = [0.0; 3];
    if range <= 0.0 {
        return out;
    }

    // A fixed three-channel sorting network avoids allocating or invoking a
    // comparator in this per-pixel hot path.
    if c[0] <= c[1] {
        if c[1] <= c[2] {
            out[1] = (c[1] - c[0]) * saturation / range;
            out[2] = saturation;
        } else if c[0] <= c[2] {
            out[2] = (c[2] - c[0]) * saturation / range;
            out[1] = saturation;
        } else {
            out[0] = (c[0] - c[2]) * saturation / range;
            out[1] = saturation;
        }
    } else if c[0] <= c[2] {
        out[0] = (c[0] - c[1]) * saturation / range;
        out[2] = saturation;
    } else if c[1] <= c[2] {
        out[2] = (c[2] - c[1]) * saturation / range;
        out[0] = saturation;
    } else {
        out[1] = (c[1] - c[2]) * saturation / range;
        out[0] = saturation;
    }
    out
}

/// Blend src over dst per mode, returning rgb [0..1].
/// NOTE: alpha isn't handled here — the caller composites alpha.
/// NOTE: Dissolve is handled in blend_onto_row, not here.
#[inline]
pub fn blend_pixel(
    mode: BlendMode,
    sr: f32,
    sg: f32,
    sb: f32,
    dr: f32,
    dg: f32,
    db: f32,
) -> (f32, f32, f32) {
    match mode {
        BlendMode::Normal | BlendMode::Dissolve => (sr, sg, sb),

        BlendMode::Multiply => (sr * dr, sg * dg, sb * db),

        BlendMode::Screen => (
            1.0 - (1.0 - sr) * (1.0 - dr),
            1.0 - (1.0 - sg) * (1.0 - dg),
            1.0 - (1.0 - sb) * (1.0 - db),
        ),

        BlendMode::Overlay => (
            if dr < 0.5 {
                2.0 * sr * dr
            } else {
                1.0 - 2.0 * (1.0 - sr) * (1.0 - dr)
            },
            if dg < 0.5 {
                2.0 * sg * dg
            } else {
                1.0 - 2.0 * (1.0 - sg) * (1.0 - dg)
            },
            if db < 0.5 {
                2.0 * sb * db
            } else {
                1.0 - 2.0 * (1.0 - sb) * (1.0 - db)
            },
        ),

        BlendMode::Darken => (sr.min(dr), sg.min(dg), sb.min(db)),
        BlendMode::Lighten => (sr.max(dr), sg.max(dg), sb.max(db)),

        BlendMode::ColorDodge => (
            if dr <= 0.0 {
                0.0
            } else if sr >= 1.0 {
                1.0
            } else {
                (dr / (1.0 - sr)).min(1.0)
            },
            if dg <= 0.0 {
                0.0
            } else if sg >= 1.0 {
                1.0
            } else {
                (dg / (1.0 - sg)).min(1.0)
            },
            if db <= 0.0 {
                0.0
            } else if sb >= 1.0 {
                1.0
            } else {
                (db / (1.0 - sb)).min(1.0)
            },
        ),

        BlendMode::ColorBurn => (
            if dr >= 1.0 {
                1.0
            } else if sr <= 0.0 {
                0.0
            } else {
                (1.0 - (1.0 - dr) / sr).max(0.0)
            },
            if dg >= 1.0 {
                1.0
            } else if sg <= 0.0 {
                0.0
            } else {
                (1.0 - (1.0 - dg) / sg).max(0.0)
            },
            if db >= 1.0 {
                1.0
            } else if sb <= 0.0 {
                0.0
            } else {
                (1.0 - (1.0 - db) / sb).max(0.0)
            },
        ),

        BlendMode::HardLight => (
            if sr < 0.5 {
                2.0 * sr * dr
            } else {
                1.0 - 2.0 * (1.0 - sr) * (1.0 - dr)
            },
            if sg < 0.5 {
                2.0 * sg * dg
            } else {
                1.0 - 2.0 * (1.0 - sg) * (1.0 - dg)
            },
            if sb < 0.5 {
                2.0 * sb * db
            } else {
                1.0 - 2.0 * (1.0 - sb) * (1.0 - db)
            },
        ),

        BlendMode::SoftLight => {
            let soft = |s: f32, d: f32| -> f32 {
                if s < 0.5 {
                    d - (1.0 - 2.0 * s) * d * (1.0 - d)
                } else {
                    let g = if d < 0.25 {
                        ((16.0 * d - 12.0) * d + 4.0) * d
                    } else {
                        d.sqrt()
                    };
                    d + (2.0 * s - 1.0) * (g - d)
                }
            };
            (soft(sr, dr), soft(sg, dg), soft(sb, db))
        }

        BlendMode::LinearLight => {
            let ll = |s: f32, d: f32| (d + 2.0 * s - 1.0).clamp(0.0, 1.0);
            (ll(sr, dr), ll(sg, dg), ll(sb, db))
        }

        BlendMode::Difference => ((sr - dr).abs(), (sg - dg).abs(), (sb - db).abs()),

        BlendMode::Exclusion => (
            sr + dr - 2.0 * sr * dr,
            sg + dg - 2.0 * sg * dg,
            sb + db - 2.0 * sb * db,
        ),

        BlendMode::Hue => {
            let src = [sr, sg, sb];
            let dst = [dr, dg, db];
            let out = set_blend_lum(set_blend_sat(src, blend_sat(dst)), blend_lum(dst));
            (out[0], out[1], out[2])
        }

        BlendMode::Saturation => {
            let src = [sr, sg, sb];
            let dst = [dr, dg, db];
            let out = set_blend_lum(set_blend_sat(dst, blend_sat(src)), blend_lum(dst));
            (out[0], out[1], out[2])
        }

        BlendMode::Color => {
            let out = set_blend_lum([sr, sg, sb], blend_lum([dr, dg, db]));
            (out[0], out[1], out[2])
        }

        BlendMode::Luminosity => {
            let out = set_blend_lum([dr, dg, db], blend_lum([sr, sg, sb]));
            (out[0], out[1], out[2])
        }
    }
}

/// Porter-Duff src-over compositing cho 1 pixel.
/// Returns (out_r, out_g, out_b, out_a) as f32 [0..1].
#[inline]
pub fn alpha_composite(
    br: f32,
    bg: f32,
    bb: f32,
    sr: f32,
    sg: f32,
    sb: f32,
    sa: f32,
    dr: f32,
    dg: f32,
    db: f32,
    da: f32,
) -> (f32, f32, f32, f32) {
    let out_a = sa + da * (1.0 - sa);
    if out_a < 0.001 {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let out_r = (sa * da * br + sa * (1.0 - da) * sr + da * (1.0 - sa) * dr) / out_a;
    let out_g = (sa * da * bg + sa * (1.0 - da) * sg + da * (1.0 - sa) * dg) / out_a;
    let out_b = (sa * da * bb + sa * (1.0 - da) * sb + da * (1.0 - sa) * db) / out_a;
    (out_r, out_g, out_b, out_a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_blend_is_identity_for_opaque() {
        let (r, g, b) = blend_pixel(BlendMode::Normal, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0);
        assert!((r - 1.0).abs() < 0.001 && g.abs() < 0.001 && b.abs() < 0.001);
    }

    #[test]
    fn multiply_black_gives_black() {
        let (r, g, b) = blend_pixel(BlendMode::Multiply, 1.0, 0.5, 0.3, 0.0, 0.0, 0.0);
        assert!(r.abs() < 0.001 && g.abs() < 0.001 && b.abs() < 0.001);
    }

    #[test]
    fn linear_light_math() {
        let (r, _, _) = blend_pixel(BlendMode::LinearLight, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5);
        assert!((r - 0.5).abs() < 0.001, "mid was {r}");
        let (r, _, _) = blend_pixel(BlendMode::LinearLight, 1.0, 1.0, 1.0, 0.3, 0.3, 0.3);
        assert!((r - 1.0).abs() < 0.001, "white blend was {r}");
        let (r, _, _) = blend_pixel(BlendMode::LinearLight, 0.0, 0.0, 0.0, 0.7, 0.7, 0.7);
        assert!(r.abs() < 0.001, "black blend was {r}");
    }

    #[test]
    fn dodge_and_burn_preserve_black_and_white_boundaries() {
        let dodge = blend_pixel(BlendMode::ColorDodge, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0);
        assert_eq!(dodge, (0.0, 0.0, 0.0));

        let burn = blend_pixel(BlendMode::ColorBurn, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0);
        assert_eq!(burn, (1.0, 1.0, 1.0));
    }

    #[test]
    fn non_separable_modes_preserve_the_spec_components() {
        let src = [0.82, 0.21, 0.37];
        let dst = [0.12, 0.68, 0.44];

        let hue = blend_pixel(
            BlendMode::Hue,
            src[0],
            src[1],
            src[2],
            dst[0],
            dst[1],
            dst[2],
        );
        let hue = [hue.0, hue.1, hue.2];
        assert!((blend_lum(hue) - blend_lum(dst)).abs() < 1e-5);
        assert!((blend_sat(hue) - blend_sat(dst)).abs() < 1e-5);

        let color = blend_pixel(
            BlendMode::Color,
            src[0],
            src[1],
            src[2],
            dst[0],
            dst[1],
            dst[2],
        );
        assert!((blend_lum([color.0, color.1, color.2]) - blend_lum(dst)).abs() < 1e-5);

        let luminosity = blend_pixel(
            BlendMode::Luminosity,
            src[0],
            src[1],
            src[2],
            dst[0],
            dst[1],
            dst[2],
        );
        assert!(
            (blend_lum([luminosity.0, luminosity.1, luminosity.2]) - blend_lum(src)).abs() < 1e-5
        );
    }

    #[test]
    fn dissolve_threshold_deterministic() {
        let t1 = dissolve_threshold(100, 200);
        let t2 = dissolve_threshold(100, 200);
        assert_eq!(t1, t2);
        assert!(t1 >= 0.0 && t1 < 1.0);
    }

    #[test]
    fn dissolve_threshold_varies_with_position() {
        let t1 = dissolve_threshold(0, 0);
        let t2 = dissolve_threshold(1, 0);
        let t3 = dissolve_threshold(0, 1);
        assert!(!(t1 == t2 && t2 == t3));
    }
}

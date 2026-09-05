// Layer, LayerStack, blend operations.
//
//   - BlendMode is DEFINED in blend.rs, re-exported here for backward compat
//   - blend_pixel() comes from blend.rs
//   - blend_onto / blend_onto_region: rayon parallel, Dissolve FIX
//   - apply_pixel: handles all adjustments (Curves, ColorBalance, GradientMap, PhotoFilter, BlackAndWhite)
//   - rgb_to_hsl / hsl_to_rgb: come from color.rs

use rayon::prelude::*;

use crate::core::blend::{alpha_composite, blend_pixel, dissolve_threshold};
use crate::core::color::{hsl_to_rgb, luminance_f32, rgb_to_hsl};
use crate::core::tile::TileMap;

pub use crate::core::blend::BlendMode;

#[derive(Debug, Clone, PartialEq)]
pub enum AdjustmentType {
    BrightnessContrast {
        brightness: f32,
        contrast: f32,
    },
    HueSaturation {
        hue: f32,
        saturation: f32,
        lightness: f32,
    },
    Levels {
        /// Per-channel parameters: `[master, red, green, blue]`. The master
        /// mapping is applied after the individual channel: `master(channel(v))`.
        channels: [LevelsParams; 4],
    },
    Curves {
        /// Per-channel control points: `[master, red, green, blue]`, each
        /// sorted ascending by x. Master applies after the individual channel.
        channels: [Vec<(f32, f32)>; 4],
    },
    ColorBalance {
        shadows: [f32; 3],
        midtones: [f32; 3],
        highlights: [f32; 3],
        preserve_luminosity: bool,
    },
    Vibrance {
        vibrance: f32,
        saturation: f32,
    },
    Exposure {
        exposure: f32,
        offset: f32,
        gamma: f32,
    },
    Invert,
    Threshold {
        value: u8,
    },
    Posterize {
        levels: u8,
    },
    BlackAndWhite {
        r: f32,
        y: f32,
        g: f32,
        c: f32,
        b: f32,
        m: f32,
    },
    PhotoFilter {
        color: [u8; 3],
        density: f32,
        luminosity: bool,
    },
    GradientMap {
        /// Color stops sorted ascending by position (0..1). The image's tonal
        /// range (shadows → highlights) is mapped across them. Always ≥ 2 stops.
        stops: Vec<(f32, [u8; 3])>,
        reverse: bool,
        /// Add per-pixel jitter to the tonal lookup to soften banding.
        dither: bool,
    },
    Desaturate,
    ChannelMixer {
        red: [f32; 3],
        green: [f32; 3],
        blue: [f32; 3],
        monochrome: bool,
    },
}

/// One channel's Levels mapping: input black/white points, gamma, output range.
/// Serde is only used by the user preset store (adjustment_presets.json) — the
/// .iai manifest writes these fields by hand.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LevelsParams {
    pub in_black: u8,
    pub in_white: u8,
    pub gamma: f32,
    pub out_black: u8,
    pub out_white: u8,
}

impl Default for LevelsParams {
    fn default() -> Self {
        Self {
            in_black: 0,
            in_white: 255,
            gamma: 1.0,
            out_black: 0,
            out_white: 255,
        }
    }
}

impl LevelsParams {
    pub fn is_identity(&self) -> bool {
        self.in_black == 0
            && self.in_white == 255
            && (self.gamma - 1.0).abs() < 1e-6
            && self.out_black == 0
            && self.out_white == 255
    }
}

/// Evaluate one channel's Levels mapping at `v` (input in [0,1]).
///
/// Single source of truth shared by the CPU pixel apply, the GPU LUT bake and
/// the Levels dialog preview.
pub fn levels_eval(p: &LevelsParams, v: f32) -> f32 {
    if p.is_identity() {
        return v;
    }
    let ib = p.in_black as f32 / 255.0;
    let iw = p.in_white as f32 / 255.0;
    let ob = p.out_black as f32 / 255.0;
    let ow = p.out_white as f32 / 255.0;
    let v = ((v - ib) / (iw - ib).max(0.001)).clamp(0.0, 1.0);
    let v = v.powf(1.0 / p.gamma.max(0.01));
    (ob + v * (ow - ob)).clamp(0.0, 1.0)
}

/// The identity curve every new Curves channel starts from.
pub fn identity_curve() -> Vec<(f32, f32)> {
    vec![(0.0, 0.0), (1.0, 1.0)]
}

/// True when the curve maps every input to itself. Fewer than 2 points is
/// identity by convention (`curves_eval` returns the input unchanged), and a
/// monotone Hermite spline through collinear diagonal points IS the diagonal.
pub fn curve_is_identity(points: &[(f32, f32)]) -> bool {
    points.len() < 2 || points.iter().all(|(x, y)| (x - y).abs() < 1e-6)
}

/// Evaluate a tone curve at `v` (input in [0,1]) through `points`
/// (**sorted ascending by x**, each in [0,1]).
///
/// Uses a monotone cubic Hermite spline (Fritsch–Carlson tangents) so the curve
/// is smooth and rounded by convention — no sharp corners at control points —
/// while never overshooting past the data (important for a tone curve). With
/// fewer than 2 points it returns `v` unchanged; outside the point range it
/// clamps to the nearest endpoint's output.
///
/// This is the single source of truth shared by the CPU pixel apply
/// (`apply_pixel`) and the Curves editor widget so the drawn curve matches the
/// applied result exactly.
pub fn curves_eval(points: &[(f32, f32)], v: f32) -> f32 {
    let n = points.len();
    if n < 2 {
        return v;
    }
    if v <= points[0].0 {
        return points[0].1.clamp(0.0, 1.0);
    }
    if v >= points[n - 1].0 {
        return points[n - 1].1.clamp(0.0, 1.0);
    }

    let secant = |k: usize| -> f32 {
        let h = points[k + 1].0 - points[k].0;
        if h.abs() < 1e-6 {
            0.0
        } else {
            (points[k + 1].1 - points[k].1) / h
        }
    };

    let tangent = |k: usize| -> f32 {
        if k == 0 {
            secant(0)
        } else if k == n - 1 {
            secant(n - 2)
        } else {
            let d_prev = secant(k - 1);
            let d_next = secant(k);
            if d_prev * d_next <= 0.0 {
                0.0
            } else {
                let h_prev = points[k].0 - points[k - 1].0;
                let h_next = points[k + 1].0 - points[k].0;
                let w1 = 2.0 * h_next + h_prev;
                let w2 = h_next + 2.0 * h_prev;
                (w1 + w2) / (w1 / d_prev + w2 / d_next)
            }
        }
    };

    let mut i = 0;
    while i + 1 < n - 1 && v > points[i + 1].0 {
        i += 1;
    }
    let (x0, y0) = points[i];
    let (x1, y1) = points[i + 1];
    let h = x1 - x0;
    if h.abs() < 1e-6 {
        return y1.clamp(0.0, 1.0);
    }

    let m0 = tangent(i);
    let m1 = tangent(i + 1);

    let t = (v - x0) / h;
    let t2 = t * t;
    let t3 = t2 * t;
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    (h00 * y0 + h10 * h * m0 + h01 * y1 + h11 * h * m1).clamp(0.0, 1.0)
}

fn rgb_chroma_f32(r: f32, g: f32, b: f32) -> f32 {
    r.max(g).max(b) - r.min(g).min(b)
}

fn smootherstep_f32(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0).max(0.00001)).clamp(0.0, 1.0);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

fn scale_chroma_around_luma_f32(r: &mut f32, g: &mut f32, b: &mut f32, factor: f32) {
    // Shared gamut-aware soft-knee saturation (no hard clip → no blocky
    // over-saturation/hue shift); see core::color::saturate_around_luma.
    let (nr, ng, nb) = crate::core::color::saturate_around_luma(*r, *g, *b, factor);
    *r = nr;
    *g = ng;
    *b = nb;
}

impl AdjustmentType {
    /// Default Levels: identity on master and all three channels. The single
    /// source of truth for every "new Levels" entry point.
    pub fn default_levels() -> Self {
        AdjustmentType::Levels {
            channels: [LevelsParams::default(); 4],
        }
    }

    /// Default Curves: the identity curve on master and all three channels.
    pub fn default_curves() -> Self {
        AdjustmentType::Curves {
            channels: std::array::from_fn(|_| identity_curve()),
        }
    }

    /// Whether this adjustment can run natively on CMYK ink planes (per-ink
    /// LUT). Everything else is RGB-space math with no ink meaning and must be
    /// refused on a CMYK document instead of corrupting the ink ground truth.
    pub fn is_ink_native(&self) -> bool {
        matches!(
            self,
            AdjustmentType::Levels { .. } | AdjustmentType::Curves { .. }
        )
    }

    /// Per-ink 8-bit LUTs for applying this adjustment to CMYK ink planes. The
    /// channel slots follow the CMYK dialog convention `[C, M, Y, K]` — there is
    /// no master pass; the RGB `[master, r, g, b]` compose does not apply to
    /// ink. `None` for adjustments with no ink-native meaning.
    pub fn ink_luts(&self) -> Option<[[u8; 256]; 4]> {
        let mut luts = [[0u8; 256]; 4];
        match self {
            AdjustmentType::Levels { channels } => {
                for (i, lut) in luts.iter_mut().enumerate() {
                    for (v, out) in lut.iter_mut().enumerate() {
                        let f = levels_eval(&channels[i], v as f32 / 255.0);
                        *out = (f * 255.0).round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
            AdjustmentType::Curves { channels } => {
                for (i, lut) in luts.iter_mut().enumerate() {
                    if curve_is_identity(&channels[i]) {
                        for (v, out) in lut.iter_mut().enumerate() {
                            *out = v as u8;
                        }
                        continue;
                    }
                    for (v, out) in lut.iter_mut().enumerate() {
                        let f = curves_eval(&channels[i], v as f32 / 255.0);
                        *out = (f * 255.0).round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
            _ => return None,
        }
        Some(luts)
    }

    /// Default Gradient Map: a black → white ramp, no reverse, no dither.
    /// The single source of truth for every "new Gradient Map" entry point.
    pub fn default_gradient_map() -> Self {
        AdjustmentType::GradientMap {
            stops: vec![(0.0, [0, 0, 0]), (1.0, [255, 255, 255])],
            reverse: false,
            dither: false,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            AdjustmentType::BrightnessContrast { .. } => "Brightness/Contrast",
            AdjustmentType::HueSaturation { .. } => "Hue/Saturation",
            AdjustmentType::Levels { .. } => "Levels",
            AdjustmentType::Curves { .. } => "Curves",
            AdjustmentType::ColorBalance { .. } => "Color Balance",
            AdjustmentType::Vibrance { .. } => "Vividness",
            AdjustmentType::Exposure { .. } => "Exposure",
            AdjustmentType::Invert => "Invert",
            AdjustmentType::Threshold { .. } => "Threshold",
            AdjustmentType::Posterize { .. } => "Posterize",
            AdjustmentType::BlackAndWhite { .. } => "Black & White",
            AdjustmentType::PhotoFilter { .. } => "Photo Filter",
            AdjustmentType::GradientMap { .. } => "Gradient Map",
            AdjustmentType::Desaturate => "Desaturate",
            AdjustmentType::ChannelMixer { .. } => "Channel Mixer",
        }
    }

    /// Apply an adjustment to one pixel. Returns RGBA.
    /// Core adjustment math in normalized [0,1] f32 — shared by the 8-bit and
    /// 16-bit pixel wrappers. `seed` only drives the Gradient Map dither jitter.
    fn apply_pixel_norm(&self, rf: f32, gf: f32, bf: f32, seed: u32) -> (f32, f32, f32) {
        let (nr, ng, nb) = match self {
            AdjustmentType::BrightnessContrast {
                brightness,
                contrast,
            } => {
                let bv = brightness / 255.0;
                let cv = (contrast + 100.0) / 100.0;
                let apply = |v: f32| ((v + bv - 0.5) * cv + 0.5).clamp(0.0, 1.0);
                (apply(rf), apply(gf), apply(bf))
            }

            AdjustmentType::HueSaturation {
                hue,
                saturation,
                lightness,
            } => {
                let (h, s, l) = rgb_to_hsl(rf, gf, bf);
                let nh = (h + hue / 360.0).rem_euclid(1.0);
                let nl = (l + lightness / 100.0).clamp(0.0, 1.0);
                let (mut r2, mut g2, mut b2) = hsl_to_rgb(nh, s, nl);
                let sat_amount = saturation / 100.0;
                if sat_amount.abs() > 0.001 {
                    let chroma = rgb_chroma_f32(r2, g2, b2);
                    let factor = if sat_amount >= 0.0 {
                        let chroma_gate = smootherstep_f32(0.035, 0.20, chroma);
                        1.0 + sat_amount * 1.35 * chroma_gate
                    } else {
                        1.0 + sat_amount
                    };
                    scale_chroma_around_luma_f32(&mut r2, &mut g2, &mut b2, factor);
                }
                (r2, g2, b2)
            }

            AdjustmentType::Levels { channels } => {
                // Per-channel first, then master (GIMP/Krita convention).
                // Swap the compose order here AND in the GPU LUT bake
                // (`adjustment_to_gpu`) if a Photoshop comparison disagrees.
                let apply =
                    |v: f32, ch: usize| levels_eval(&channels[0], levels_eval(&channels[ch], v));
                (apply(rf, 1), apply(gf, 2), apply(bf, 3))
            }

            AdjustmentType::Curves { channels } => {
                let id: [bool; 4] = std::array::from_fn(|i| curve_is_identity(&channels[i]));
                let apply = |v: f32, ch: usize| {
                    let v = if id[ch] {
                        v
                    } else {
                        curves_eval(&channels[ch], v)
                    };
                    if id[0] {
                        v
                    } else {
                        curves_eval(&channels[0], v)
                    }
                };
                (apply(rf, 1), apply(gf, 2), apply(bf, 3))
            }

            AdjustmentType::ColorBalance {
                shadows,
                midtones,
                highlights,
                preserve_luminosity,
            } => {
                let orig_lum = luminance_f32(rf, gf, bf);

                let lum = orig_lum;
                let sw = (1.0 - 2.0 * lum).max(0.0);
                let hw = (2.0 * lum - 1.0).max(0.0);
                let mw = 1.0 - sw - hw;

                let cr = (rf
                    + sw * shadows[0] / 100.0
                    + mw * midtones[0] / 100.0
                    + hw * highlights[0] / 100.0)
                    .clamp(0.0, 1.0);
                let cg = (gf
                    + sw * shadows[1] / 100.0
                    + mw * midtones[1] / 100.0
                    + hw * highlights[1] / 100.0)
                    .clamp(0.0, 1.0);
                let cb = (bf
                    + sw * shadows[2] / 100.0
                    + mw * midtones[2] / 100.0
                    + hw * highlights[2] / 100.0)
                    .clamp(0.0, 1.0);

                if *preserve_luminosity {
                    let new_lum = luminance_f32(cr, cg, cb);
                    if new_lum > 0.001 {
                        let scale = orig_lum / new_lum;
                        (
                            (cr * scale).clamp(0.0, 1.0),
                            (cg * scale).clamp(0.0, 1.0),
                            (cb * scale).clamp(0.0, 1.0),
                        )
                    } else {
                        (cr, cg, cb)
                    }
                } else {
                    (cr, cg, cb)
                }
            }

            AdjustmentType::Vibrance {
                vibrance,
                saturation,
            } => {
                let (h, s, l) = rgb_to_hsl(rf, gf, bf);
                let sat_boost = vibrance / 100.0 * (1.0 - s);
                let ns = (s + sat_boost + saturation / 100.0).clamp(0.0, 1.0);
                let (r2, g2, b2) = hsl_to_rgb(h, ns, l);
                (r2, g2, b2)
            }

            AdjustmentType::Exposure {
                exposure,
                offset,
                gamma,
            } => {
                let apply = |v: f32| -> f32 {
                    let v = v * (2.0f32).powf(*exposure);
                    let v = v + offset;
                    v.powf(1.0 / gamma.max(0.01)).clamp(0.0, 1.0)
                };
                (apply(rf), apply(gf), apply(bf))
            }

            AdjustmentType::Invert => (1.0 - rf, 1.0 - gf, 1.0 - bf),
            AdjustmentType::Desaturate => {
                let lum = luminance_f32(rf, gf, bf);
                (lum, lum, lum)
            }

            AdjustmentType::Threshold { value } => {
                let lum = luminance_f32(rf, gf, bf);
                let v = if lum >= *value as f32 / 255.0 {
                    1.0
                } else {
                    0.0
                };
                (v, v, v)
            }

            AdjustmentType::Posterize { levels } => {
                let l = (*levels).max(2) as f32;
                let apply = |v: f32| (v * l).floor() / (l - 1.0);
                (apply(rf), apply(gf), apply(bf))
            }

            AdjustmentType::BlackAndWhite {
                r: rv,
                y,
                g: gv,
                c,
                b: bv,
                m,
            } => {
                let (h, s, l) = rgb_to_hsl(rf, gf, bf);

                let sliders = [*rv, *y, *gv, *c, *bv, *m];
                let weight = if s < 0.05 {
                    sliders.iter().sum::<f32>() / 600.0
                } else {
                    let zone = h * 6.0;
                    let i = zone.floor() as usize % 6;
                    let frac = zone - zone.floor();
                    let w0 = sliders[i] / 100.0;
                    let w1 = sliders[(i + 1) % 6] / 100.0;
                    w0 + (w1 - w0) * frac
                };

                let new_l = (l * (1.0 + (weight - 1.0) * s)).clamp(0.0, 1.0);
                (new_l, new_l, new_l)
            }

            AdjustmentType::PhotoFilter {
                color,
                density,
                luminosity,
            } => {
                let orig_lum = luminance_f32(rf, gf, bf);
                let fr = color[0] as f32 / 255.0;
                let fg = color[1] as f32 / 255.0;
                let fb = color[2] as f32 / 255.0;
                let d = density.clamp(0.0, 1.0);

                let nr = (rf * fr * d + rf * (1.0 - d)).clamp(0.0, 1.0);
                let ng = (gf * fg * d + gf * (1.0 - d)).clamp(0.0, 1.0);
                let nb = (bf * fb * d + bf * (1.0 - d)).clamp(0.0, 1.0);

                if *luminosity {
                    let new_lum = luminance_f32(nr, ng, nb);
                    if new_lum > 0.001 {
                        let scale = orig_lum / new_lum;
                        (
                            (nr * scale).clamp(0.0, 1.0),
                            (ng * scale).clamp(0.0, 1.0),
                            (nb * scale).clamp(0.0, 1.0),
                        )
                    } else {
                        (nr, ng, nb)
                    }
                } else {
                    (nr, ng, nb)
                }
            }

            AdjustmentType::GradientMap {
                stops,
                reverse,
                dither,
            } => {
                let lum = luminance_f32(rf, gf, bf);
                let mut t = if *reverse { 1.0 - lum } else { lum };
                if *dither {
                    // No pixel coords here, so jitter is seeded from the source
                    // pixel (passed in). This breaks banding on photographic content
                    // (where neighbouring pixels differ); a perfectly flat synthetic
                    // ramp won't dither — an accepted limitation of the per-pixel API.
                    let mut h = seed.wrapping_mul(2654435761);
                    h ^= h >> 15;
                    h = h.wrapping_mul(2246822519);
                    h ^= h >> 13;
                    let jitter = (h as f32 / u32::MAX as f32 - 0.5) * (1.0 / 48.0);
                    t = (t + jitter).clamp(0.0, 1.0);
                }
                let c = crate::core::color::sample_gradient_stops(stops, t);
                (
                    c[0] as f32 / 255.0,
                    c[1] as f32 / 255.0,
                    c[2] as f32 / 255.0,
                )
            }

            AdjustmentType::ChannelMixer {
                red,
                green,
                blue,
                monochrome,
            } => {
                let nr = (rf * red[0] + gf * red[1] + bf * red[2]).clamp(0.0, 1.0);
                let ng = (rf * green[0] + gf * green[1] + bf * green[2]).clamp(0.0, 1.0);
                let nb = (rf * blue[0] + gf * blue[1] + bf * blue[2]).clamp(0.0, 1.0);
                if *monochrome {
                    let lum = luminance_f32(nr, ng, nb);
                    (lum, lum, lum)
                } else {
                    (nr, ng, nb)
                }
            }
        };

        (nr, ng, nb)
    }

    /// 8-bit adjustment: convert to f32, run the shared math, quantize to u8.
    /// Alpha is passed through. Output is bit-identical to the pre-refactor code.
    pub fn apply_pixel(&self, r: u8, g: u8, b: u8, a: u8) -> (u8, u8, u8, u8) {
        let seed = (r as u32) | ((g as u32) << 8) | ((b as u32) << 16) | ((a as u32) << 24);
        let (nr, ng, nb) =
            self.apply_pixel_norm(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, seed);
        (
            (nr * 255.0).round().clamp(0.0, 255.0) as u8,
            (ng * 255.0).round().clamp(0.0, 255.0) as u8,
            (nb * 255.0).round().clamp(0.0, 255.0) as u8,
            a,
        )
    }

    /// 16-bit adjustment: the same math at full 16-bit precision (no 8-bit
    /// quantization of the input), so Levels/Curves/etc. on a 16-bit document
    /// don't band. Alpha is passed through.
    pub fn apply_pixel16(&self, r: u16, g: u16, b: u16, a: u16) -> (u16, u16, u16, u16) {
        let seed = (r as u32) ^ ((g as u32) << 11) ^ ((b as u32) << 22) ^ ((a as u32) << 5);
        let (nr, ng, nb) = self.apply_pixel_norm(
            r as f32 / 65535.0,
            g as f32 / 65535.0,
            b as f32 / 65535.0,
            seed,
        );
        (
            (nr * 65535.0).round().clamp(0.0, 65535.0) as u16,
            (ng * 65535.0).round().clamp(0.0, 65535.0) as u16,
            (nb * 65535.0).round().clamp(0.0, 65535.0) as u16,
            a,
        )
    }

    /// Apply an adjustment to a whole 16-bit RGBA buffer (rayon parallel).
    pub fn apply_to_pixels16(&self, pixels: &mut [u16]) {
        pixels.par_chunks_mut(4).for_each(|px| {
            if px.len() == 4 {
                let (nr, ng, nb, na) = self.apply_pixel16(px[0], px[1], px[2], px[3]);
                px[0] = nr;
                px[1] = ng;
                px[2] = nb;
                px[3] = na;
            }
        });
    }

    /// Apply an adjustment to the whole pixel buffer (rayon parallel).
    pub fn apply_to_pixels(&self, pixels: &mut Vec<u8>) {
        pixels.par_chunks_mut(4).for_each(|px| {
            if px.len() == 4 {
                let (nr, ng, nb, na) =
                    AdjustmentType::apply_pixel_static(self, px[0], px[1], px[2], px[3]);
                px[0] = nr;
                px[1] = ng;
                px[2] = nb;
                px[3] = na;
            }
        });
    }

    fn apply_pixel_static(&self, r: u8, g: u8, b: u8, a: u8) -> (u8, u8, u8, u8) {
        self.apply_pixel(r, g, b, a)
    }
}

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum LayerType {
    Raster,
    Adjustment(AdjustmentType),
    /// A layer group (folder). Children are not stored here — membership lives
    /// on each child's `Layer::parent_id`, and the group's contiguous run in the
    /// flat stack is derived via `LayerStack::group_range`. Keeping the variant
    /// payload-free avoids stale child indices.
    Group,
    Text(crate::core::text::TextData),
    /// An editable vector Path object (Bước 4). Holds the source-of-truth model
    /// (geometry + fill/outline + object transform, Mục 3.2); `Layer::tiles` is
    /// a raster cache derived from it, and `Layer::offset` places that raster on
    /// the canvas/page. One object per layer at MVP (Mục 3.3).
    Vector(crate::core::vector::object::VectorGeometry),
    SmartObject,
}

impl Default for LayerType {
    fn default() -> Self {
        LayerType::Raster
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaintTarget {
    Pixels,
    Mask,
}

impl Default for PaintTarget {
    fn default() -> Self {
        PaintTarget::Pixels
    }
}

#[derive(Clone)]
pub struct LayerMask {
    pub tiles: TileMap,
    pub width: u32,
    pub height: u32,
    pub enabled: bool,
    pub inverted: bool,
    /// Canvas offset of the owning (content) layer when this mask was baked, and
    /// (`bake_frame_offset`) the clip frame's offset at that same moment. Only
    /// meaningful for a PowerClip / clipping-mask content layer: while either the
    /// content or its frame is dragged, the clip re-bake is skipped (too slow per
    /// frame), so the GPU pins the clip by sampling this unmoved mask at
    /// `layer_local + shift`, where `shift = (content.offset − bake_offset) −
    /// (frame.offset − bake_frame_offset)`. That keeps the image clipped inside
    /// the frame live, with no per-frame re-bake. Both `(0, 0)` for every ordinary
    /// mask (which simply move with their layer, delta 0).
    pub bake_offset: (i32, i32),
    pub bake_frame_offset: (i32, i32),
}

#[allow(dead_code)]
impl LayerMask {
    pub fn new_white(width: u32, height: u32) -> Self {
        Self {
            tiles: TileMap::new_white(width, height),
            width,
            height,
            enabled: true,
            inverted: false,
            bake_offset: (0, 0),
            bake_frame_offset: (0, 0),
        }
    }

    pub fn new_black(width: u32, height: u32) -> Self {
        Self {
            tiles: TileMap::new(width, height),
            width,
            height,
            enabled: true,
            inverted: false,
            bake_offset: (0, 0),
            bake_frame_offset: (0, 0),
        }
    }

    /// Invert the mask's pixels directly on the tile (Ctrl+I). Doesn't touch the
    /// `inverted` flag (which only affects CPU sampling); the GPU compositor reads pixels directly.
    pub fn invert_tiles(&mut self) {
        for tile in self.tiles.tiles.values_mut() {
            let t = std::sync::Arc::make_mut(tile);
            t.revision += 1;
            for i in (0..t.pixels.len()).step_by(4) {
                t.pixels[i] = 255 - t.pixels[i];
                t.pixels[i + 1] = 255 - t.pixels[i + 1];
                t.pixels[i + 2] = 255 - t.pixels[i + 2];
            }
        }
    }

    /// Clamp-to-edge sampling: coordinates outside the mask take the nearest
    /// edge value, so a mask smaller than its layer never hard-reveals the
    /// overflow. Mirrored by `sample_mask_nearest_i` in the GPU compositor.
    #[inline]
    pub fn sample(&self, x: u32, y: u32) -> f32 {
        if self.width == 0 || self.height == 0 {
            return 1.0;
        }
        let x = x.min(self.width - 1);
        let y = y.min(self.height - 1);
        let (r, _, _, _) = self.tiles.get_pixel(x, y);
        let v = r as f32 / 255.0;
        if self.inverted {
            1.0 - v
        } else {
            v
        }
    }

    /// Resize the mask canvas keeping content at (0,0); new area is filled
    /// white (revealed) like Photoshop's canvas-resize behaviour. Used when
    /// the owning layer's width/height change so mask and layer stay aligned.
    pub fn resize_to(&mut self, new_w: u32, new_h: u32) {
        if (self.width == new_w && self.height == new_h) || new_w == 0 || new_h == 0 {
            return;
        }
        let copy_w = self.width.min(new_w);
        let copy_h = self.height.min(new_h);
        // Tile-native (no canvas-sized buffer, works on Viewport-Streaming masks):
        // start fully-revealed (white) so any extended area reveals the layer, then
        // copy the overlapping old region. Mask tiles are solid-initialised (opaque
        // alpha everywhere), so the gray value in R survives the copy.
        let mut new_tiles = TileMap::new_white(new_w, new_h);
        if copy_w > 0 && copy_h > 0 {
            new_tiles.blit_region_from(&self.tiles, 0, 0, 0, 0, copy_w, copy_h);
        }
        self.tiles = new_tiles;
        self.width = new_w;
        self.height = new_h;
    }

    /// Apply the mask to the layer (flatten mask → layer alpha), then remove the mask.
    pub fn apply_to_layer(self, layer: &mut Layer) {
        let mask_tiles = self.tiles;
        let inverted = self.inverted;

        let old_layer_tiles = std::mem::take(&mut layer.tiles.tiles);
        for (pos, tile) in old_layer_tiles {
            if let Some(mask_tile) = mask_tiles.tiles.get(&pos) {
                let dest =
                    layer.tiles.tiles.entry(pos).or_insert_with(|| {
                        std::sync::Arc::new(crate::core::tile::Tile::new_empty())
                    });
                let t = std::sync::Arc::make_mut(dest);
                t.revision += 1;
                t.pixels.copy_from_slice(&tile.pixels);
                for i in (0..t.pixels.len()).step_by(4) {
                    let la = t.pixels[i + 3] as f32 / 255.0;
                    let mr = mask_tile.pixels[i];
                    let mask_a = if inverted { 255 - mr } else { mr };
                    t.pixels[i + 3] = (la * mask_a as f32).clamp(0.0, 255.0) as u8;
                }
            } else {
                if inverted {
                    layer.tiles.tiles.insert(pos, tile);
                }
            }
        }
        layer.mask = None;
        layer.mask_active = false;
    }

    /// Bake this (group) mask into a child layer's pixel alpha. A group folder
    /// holds no pixels of its own, so "Apply Layer Mask" on a folder can't bake
    /// into the header — instead the mask (canvas-space) is multiplied into each
    /// raster child's alpha, sampling at the child's canvas position so child
    /// offsets are respected. Pixels outside the mask/canvas are left untouched
    /// (the group content is clipped to the canvas anyway), and tiles that end up
    /// fully transparent are dropped so the atlas isn't left with phantom tiles.
    pub fn bake_into_child_alpha(&self, layer: &mut Layer) {
        use crate::core::tile::{TilePos, TILE_SIZE};
        let (ox, oy) = layer.offset;
        let mw = self.width as i32;
        let mh = self.height as i32;
        let mut drop_tiles: Vec<TilePos> = Vec::new();
        for (pos, tile) in layer.tiles.tiles.iter_mut() {
            let base_x = pos.x * TILE_SIZE as i32 + ox;
            let base_y = pos.y * TILE_SIZE as i32 + oy;
            let t = std::sync::Arc::make_mut(tile);
            t.revision += 1;
            let mut any_visible = false;
            for ty in 0..TILE_SIZE {
                let cy = base_y + ty as i32;
                let in_y = cy >= 0 && cy < mh;
                for tx in 0..TILE_SIZE {
                    let i = ((ty * TILE_SIZE + tx) * 4) as usize;
                    let a = t.pixels[i + 3];
                    if a == 0 {
                        continue;
                    }
                    let cx = base_x + tx as i32;
                    let m = if in_y && cx >= 0 && cx < mw {
                        self.sample(cx as u32, cy as u32)
                    } else {
                        1.0
                    };
                    let na = (a as f32 * m).round().clamp(0.0, 255.0) as u8;
                    t.pixels[i + 3] = na;
                    if na > 0 {
                        any_visible = true;
                    }
                }
            }
            if !any_visible {
                drop_tiles.push(*pos);
            }
        }
        for pos in drop_tiles {
            layer.tiles.tiles.remove(&pos);
        }
    }
}

#[derive(Clone)]
pub struct Layer {
    pub id: u32,
    pub name: String,
    pub tiles: TileMap,
    pub width: u32,
    pub height: u32,
    pub opacity: f32,
    pub visible: bool,
    pub locked: bool,
    pub blend_mode: BlendMode,
    pub mask: Option<LayerMask>,
    pub mask_active: bool,
    /// Mask is "linked" to the layer pixels (they move together, by convention).
    /// Defaults to true on mask creation. Currently stores the state + shows the chain icon.
    pub mask_linked: bool,
    pub paint_target: PaintTarget,
    pub layer_type: LayerType,
    pub is_background: bool,
    pub lock_alpha: bool,
    pub offset: (i32, i32),
    pub selected: bool,
    /// Id of the containing group layer, or `None` for a top-level layer.
    /// Membership is stored here (not in `LayerType::Group`) so reorders never
    /// invalidate stale child indices.
    pub parent_id: Option<u32>,
    /// Independent PowerClip relation: this layer's pixels are content clipped
    /// by the referenced frame layer. Deliberately separate from `parent_id`,
    /// which means group membership only.
    pub clip_parent_id: Option<u32>,
    /// Which page / artboard this layer belongs to (foundation contract #10). At
    /// MVP every layer is on [`crate::core::page::PageId::IMPLICIT`], so behaviour
    /// is unchanged; the field is here so multi-page support is purely additive.
    pub page_id: crate::core::page::PageId,
    /// Group-only: whether the folder is expanded in the panel. Ignored for
    /// non-group layers. Defaults to `true`.
    pub expanded: bool,
    /// Dynamic-connector attachment: set on an arrow/connector Path layer whose
    /// end(s) stick to other layers, so it re-routes when they move. `None` for
    /// every ordinary layer — the common case pays nothing.
    pub connector: Option<crate::core::connector::ConnectorBinding>,
}

impl Layer {
    /// The layer's opaque content rectangle in canvas space (`x, y, w, h`), or
    /// `None` when it has no ink. Used to resolve a connector anchor against the
    /// shape it sticks to, and to hit-test which shape an endpoint landed on.
    pub fn canvas_content_rect(&self) -> Option<(f32, f32, f32, f32)> {
        let (x0, y0, x1, y1) = self.tiles.content_bounds()?;
        let (w, h) = ((x1 - x0) as f32, (y1 - y0) as f32);
        if w <= 0.0 || h <= 0.0 {
            return None;
        }
        Some((
            self.offset.0 as f32 + x0 as f32,
            self.offset.1 as f32 + y0 as f32,
            w,
            h,
        ))
    }
}

#[allow(dead_code)]
impl Layer {
    pub fn new(id: u32, name: &str, width: u32, height: u32) -> Self {
        Self {
            id,
            name: name.to_string(),
            tiles: TileMap::new(width, height),
            width,
            height,
            opacity: 1.0,
            visible: true,
            locked: false,
            blend_mode: BlendMode::Normal,
            mask: None,
            mask_active: false,
            mask_linked: true,
            paint_target: PaintTarget::Pixels,
            layer_type: LayerType::Raster,
            is_background: false,
            lock_alpha: false,
            offset: (0, 0),
            selected: false,
            parent_id: None,
            clip_parent_id: None,
            page_id: crate::core::page::PageId::IMPLICIT,
            expanded: true,
            connector: None,
        }
    }

    pub fn new_white(id: u32, name: &str, width: u32, height: u32) -> Self {
        let mut l = Self::new(id, name, width, height);
        l.tiles = TileMap::new_white(width, height);
        l.is_background = true;
        l
    }

    pub fn from_rgba(id: u32, name: &str, pixels: Vec<u8>, width: u32, height: u32) -> Self {
        Self {
            id,
            name: name.to_string(),
            tiles: TileMap::from_rgba(&pixels, width, height),
            width,
            height,
            opacity: 1.0,
            visible: true,
            locked: name == "Background",
            blend_mode: BlendMode::Normal,
            mask: None,
            mask_active: false,
            mask_linked: true,
            paint_target: PaintTarget::Pixels,
            layer_type: LayerType::Raster,
            is_background: name == "Background",
            lock_alpha: false,
            offset: (0, 0),
            selected: false,
            parent_id: None,
            clip_parent_id: None,
            page_id: crate::core::page::PageId::IMPLICIT,
            expanded: true,
            connector: None,
        }
    }

    pub fn new_adjustment(id: u32, adj: AdjustmentType, width: u32, height: u32) -> Self {
        let name = adj.name().to_string();
        Self {
            id,
            name,
            tiles: TileMap::new(width, height),
            width,
            height,
            opacity: 1.0,
            visible: true,
            locked: false,
            blend_mode: BlendMode::Normal,
            mask: None,
            mask_active: false,
            mask_linked: true,
            paint_target: PaintTarget::Pixels,
            layer_type: LayerType::Adjustment(adj),
            is_background: false,
            lock_alpha: false,
            offset: (0, 0),
            selected: false,
            parent_id: None,
            clip_parent_id: None,
            page_id: crate::core::page::PageId::IMPLICIT,
            expanded: true,
            connector: None,
        }
    }

    /// A folder/group layer. Holds no pixels (its tiles stay empty — TileMap is
    /// sparse, so canvas-sized costs nothing); sized to the canvas so a group
    /// mask is canvas-sized and paintable. Membership is tracked via each child's
    /// `parent_id`, collapse state via `expanded`.
    pub fn new_group(id: u32, name: &str, width: u32, height: u32) -> Self {
        let mut l = Self::new(id, name, width, height);
        l.layer_type = LayerType::Group;
        l.expanded = true;
        l
    }

    /// Whether this layer is a group/folder header.
    pub fn is_group(&self) -> bool {
        matches!(self.layer_type, LayerType::Group)
    }

    /// Whether drawing this layer can change the composed image.
    ///
    /// Fresh raster/text/shape layers are often canvas-sized but tile-empty. On
    /// large documents, treating those as drawable forces a fullscreen compositor
    /// pass on every zoom/pan/move even though they contribute no pixels.
    pub fn has_renderable_content(&self) -> bool {
        match &self.layer_type {
            LayerType::Group => false,
            LayerType::Adjustment(_) => true,
            _ => !self.tiles.tiles.is_empty(),
        }
    }

    pub fn duplicate(&self, new_id: u32) -> Self {
        Self {
            id: new_id,
            name: format!("{} copy", self.name),
            tiles: self.tiles.clone(),
            width: self.width,
            height: self.height,
            opacity: self.opacity,
            visible: self.visible,
            locked: false,
            blend_mode: self.blend_mode,
            mask: self.mask.clone(),
            mask_active: self.mask_active,
            mask_linked: self.mask_linked,
            paint_target: self.paint_target,
            layer_type: self.layer_type.clone(),
            is_background: false,
            lock_alpha: self.lock_alpha,
            offset: self.offset,
            selected: self.selected,
            parent_id: self.parent_id,
            clip_parent_id: self.clip_parent_id,
            // A duplicate stays on the same page as its source (page membership is
            // copied verbatim, unlike the clip relation which is remapped).
            page_id: self.page_id,
            expanded: self.expanded,
            // A duplicated connector keeps its bindings (same targets).
            connector: self.connector,
        }
    }

    pub fn add_mask(&mut self, white: bool) {
        self.mask = Some(if white {
            LayerMask::new_white(self.width, self.height)
        } else {
            LayerMask::new_black(self.width, self.height)
        });
        self.mask_active = true;
        self.mask_linked = true;
        self.paint_target = PaintTarget::Mask;
    }

    /// Invert the layer's RGB pixels (Ctrl+I when the paint target = Pixels).
    /// Only flips existing tiles — empty (transparent) areas don't need inverting.
    pub fn invert_pixels(&mut self) {
        for tile in self.tiles.tiles.values_mut() {
            let t = std::sync::Arc::make_mut(tile);
            t.revision += 1;
            for i in (0..t.pixels.len()).step_by(4) {
                t.pixels[i] = 255 - t.pixels[i];
                t.pixels[i + 1] = 255 - t.pixels[i + 1];
                t.pixels[i + 2] = 255 - t.pixels[i + 2];
            }
        }
    }

    pub fn delete_mask(&mut self) {
        self.mask = None;
        self.mask_active = false;
        self.paint_target = PaintTarget::Pixels;
    }

    pub fn apply_mask(&mut self) {
        if let Some(mask) = self.mask.take() {
            mask.apply_to_layer(self);
        }
        self.mask_active = false;
        self.paint_target = PaintTarget::Pixels;
    }

    pub fn is_raster(&self) -> bool {
        matches!(self.layer_type, LayerType::Raster)
    }
    pub fn is_adjustment(&self) -> bool {
        matches!(self.layer_type, LayerType::Adjustment(_))
    }

    pub fn get_paint_tiles_mut(&mut self) -> Option<&mut TileMap> {
        match self.paint_target {
            PaintTarget::Pixels => {
                if self.is_raster() {
                    Some(&mut self.tiles)
                } else {
                    None
                }
            }
            PaintTarget::Mask => {
                if let Some(ref mut mask) = self.mask {
                    Some(&mut mask.tiles)
                } else {
                    None
                }
            }
        }
    }

    pub fn get_paint_tiles(&self) -> Option<&TileMap> {
        match self.paint_target {
            PaintTarget::Pixels => {
                if self.is_raster() {
                    Some(&self.tiles)
                } else {
                    None
                }
            }
            PaintTarget::Mask => {
                if let Some(ref mask) = self.mask {
                    Some(&mask.tiles)
                } else {
                    None
                }
            }
        }
    }

    pub fn blend_onto(&self, dst: &mut [u8], dst_w: u32) {
        self.blend_onto_region(dst, dst_w, 0, 0, dst_w, dst.len() as u32 / dst_w / 4);
    }

    pub fn blend_onto_region(
        &self,
        dst: &mut [u8],
        dst_w: u32,
        rx: u32,
        ry: u32,
        rw: u32,
        rh: u32,
    ) {
        if !self.visible || self.is_group() {
            return;
        }
        let opacity = self.opacity.clamp(0.0, 1.0);
        if opacity < 0.001 {
            return;
        }

        let mode = self.blend_mode;
        let has_mask = self.mask.as_ref().map(|m| m.enabled).unwrap_or(false);

        let end_y = (ry + rh).min(dst.len() as u32 / dst_w / 4);
        let end_x = (rx + rw).min(dst_w);

        if let LayerType::Adjustment(ref adj) = self.layer_type {
            let dst_w_u = dst_w as usize;
            let row_bytes = dst_w_u * 4;
            // Adjustment masks live in canvas space (adjustment offset is 0 in
            // normal use). Under the chunked offset-shift composite the layer is
            // temporarily shifted by (−chunk_x, −chunk_y), so map the chunk-local
            // (x, y) back through the offset to sample the mask at its true
            // canvas position; a zero offset keeps this an exact no-op.
            let (ox, oy) = self.offset;

            dst[..((end_y * dst_w) * 4) as usize]
                .par_chunks_mut(row_bytes)
                .skip(ry as usize)
                .take((end_y - ry) as usize)
                .enumerate()
                .for_each(|(row_offset, dst_row)| {
                    let y = ry + row_offset as u32;
                    for x in rx..end_x {
                        let di = (x * 4) as usize;
                        if di + 3 >= dst_row.len() {
                            continue;
                        }

                        let mut sa = opacity;
                        if has_mask {
                            if let Some(ref mask) = self.mask {
                                let mx = (x as i32 - ox).max(0) as u32;
                                let my = (y as i32 - oy).max(0) as u32;
                                sa *= mask.sample(mx, my);
                            }
                        }
                        if sa < 0.001 {
                            continue;
                        }

                        let (nr, ng, nb, na) = adj.apply_pixel(
                            dst_row[di],
                            dst_row[di + 1],
                            dst_row[di + 2],
                            dst_row[di + 3],
                        );
                        let inv = 1.0 - sa;
                        dst_row[di] = (nr as f32 * sa + dst_row[di] as f32 * inv) as u8;
                        dst_row[di + 1] = (ng as f32 * sa + dst_row[di + 1] as f32 * inv) as u8;
                        dst_row[di + 2] = (nb as f32 * sa + dst_row[di + 2] as f32 * inv) as u8;
                        dst_row[di + 3] = na;
                    }
                });
            return;
        }

        let dst_w_u = dst_w as usize;
        let row_bytes = dst_w_u * 4;
        let layer_tiles = &self.tiles;

        let ox = self.offset.0;
        let oy = self.offset.1;

        let bound_x0 = rx.max(ox.max(0) as u32);
        let bound_y0 = ry.max(oy.max(0) as u32);
        let bound_x1 = end_x.min((ox + self.width as i32).max(0) as u32);
        let bound_y1 = end_y.min((oy + self.height as i32).max(0) as u32);

        if bound_x1 <= bound_x0 || bound_y1 <= bound_y0 {
            return;
        }

        dst[..((bound_y1 * dst_w) * 4) as usize]
            .par_chunks_mut(row_bytes)
            .skip(bound_y0 as usize)
            .take((bound_y1 - bound_y0) as usize)
            .enumerate()
            .for_each(|(row_offset, dst_row)| {
                let y = bound_y0 + row_offset as u32;
                let layer_y = (y as i32 - oy) as u32;
                let ty = layer_y / crate::core::tile::TILE_SIZE;
                let ty_rem = layer_y % crate::core::tile::TILE_SIZE;

                let mut x = bound_x0;
                while x < bound_x1 {
                    let layer_x = (x as i32 - ox) as u32;
                    let tx = layer_x / crate::core::tile::TILE_SIZE;
                    let tx_rem = layer_x % crate::core::tile::TILE_SIZE;
                    let tile_w = crate::core::tile::TILE_SIZE - tx_rem;
                    let copy_w = tile_w.min(bound_x1 - x);

                    let pos = crate::core::tile::TilePos {
                        x: tx as i32,
                        y: ty as i32,
                    };
                    if let Some(tile) = layer_tiles.tiles.get(&pos) {
                        let tile_row_offset =
                            ((ty_rem * crate::core::tile::TILE_SIZE) * 4) as usize;

                        for i in 0..copy_w {
                            let px = x + i;
                            let di = (px * 4) as usize;
                            if di + 3 >= dst_row.len() {
                                continue;
                            }

                            let src_i = tile_row_offset + ((tx_rem + i) * 4) as usize;
                            if src_i + 3 >= tile.pixels.len() {
                                continue;
                            }

                            let sr8 = tile.pixels[src_i];
                            let sg8 = tile.pixels[src_i + 1];
                            let sb8 = tile.pixels[src_i + 2];
                            let sa8 = tile.pixels[src_i + 3];

                            let src_alpha = sa8 as f32 / 255.0;
                            let mut sa = src_alpha * opacity;
                            if has_mask {
                                if let Some(ref mask) = self.mask {
                                    let layer_px = (px as i32 - ox) as u32;
                                    sa *= mask.sample(layer_px, layer_y);
                                }
                            }
                            if mode == BlendMode::Dissolve {
                                let threshold = dissolve_threshold(px, y);
                                if threshold >= sa {
                                    continue;
                                }
                                sa = 1.0;
                            }
                            if sa < 0.001 {
                                continue;
                            }

                            let sr = sr8 as f32 / 255.0;
                            let sg = sg8 as f32 / 255.0;
                            let sb = sb8 as f32 / 255.0;
                            let dr = dst_row[di] as f32 / 255.0;
                            let dg = dst_row[di + 1] as f32 / 255.0;
                            let db = dst_row[di + 2] as f32 / 255.0;
                            let da = dst_row[di + 3] as f32 / 255.0;

                            let (br, bg, bb) = blend_pixel(mode, sr, sg, sb, dr, dg, db);
                            let (out_r, out_g, out_b, out_a) =
                                alpha_composite(br, bg, bb, sr, sg, sb, sa, dr, dg, db, da);

                            dst_row[di] = (out_r * 255.0).clamp(0.0, 255.0) as u8;
                            dst_row[di + 1] = (out_g * 255.0).clamp(0.0, 255.0) as u8;
                            dst_row[di + 2] = (out_b * 255.0).clamp(0.0, 255.0) as u8;
                            dst_row[di + 3] = (out_a * 255.0).clamp(0.0, 255.0) as u8;
                        }
                    }
                    x += copy_w;
                }
            });
    }

    /// Full-canvas composite of this layer onto an f32 straight-alpha buffer
    /// (`dst`, normalized RGBA in [0,1], row-major `dst_w`×`dst_h`). The 16-bit
    /// counterpart of [`Self::blend_onto_region`]: identical blend maths (the
    /// shared `blend_pixel`/`alpha_composite` operate on normalized f32) but the
    /// source is read at 16-bit and the accumulator stays f32 so a multi-layer
    /// 16-bit stack composites without banding. Used by [`LayerStack::flatten16`].
    pub fn blend_onto_f32(&self, dst: &mut [f32], dst_w: u32, dst_h: u32) {
        if !self.visible || self.is_group() {
            return;
        }
        let opacity = self.opacity.clamp(0.0, 1.0);
        if opacity < 0.001 {
            return;
        }
        let mode = self.blend_mode;
        let has_mask = self.mask.as_ref().map(|m| m.enabled).unwrap_or(false);
        let row_len = dst_w as usize * 4;

        if let LayerType::Adjustment(ref adj) = self.layer_type {
            dst.par_chunks_mut(row_len)
                .take(dst_h as usize)
                .enumerate()
                .for_each(|(y, row)| {
                    let y = y as u32;
                    for x in 0..dst_w {
                        let di = (x * 4) as usize;
                        let mut sa = opacity;
                        if has_mask {
                            if let Some(ref mask) = self.mask {
                                sa *= mask.sample(x, y);
                            }
                        }
                        if sa < 0.001 {
                            continue;
                        }
                        let to16 = |v: f32| (v * 65535.0).round().clamp(0.0, 65535.0) as u16;
                        let (nr, ng, nb, na) = adj.apply_pixel16(
                            to16(row[di]),
                            to16(row[di + 1]),
                            to16(row[di + 2]),
                            to16(row[di + 3]),
                        );
                        let inv = 1.0 - sa;
                        row[di] = (nr as f32 / 65535.0) * sa + row[di] * inv;
                        row[di + 1] = (ng as f32 / 65535.0) * sa + row[di + 1] * inv;
                        row[di + 2] = (nb as f32 / 65535.0) * sa + row[di + 2] * inv;
                        row[di + 3] = na as f32 / 65535.0;
                    }
                });
            return;
        }

        let ox = self.offset.0;
        let oy = self.offset.1;
        let layer_w = self.width as i32;
        let layer_h = self.height as i32;

        dst.par_chunks_mut(row_len)
            .take(dst_h as usize)
            .enumerate()
            .for_each(|(y, row)| {
                let ly = y as i32 - oy;
                if ly < 0 || ly >= layer_h {
                    return;
                }
                let ly = ly as u32;
                for x in 0..dst_w {
                    let lx = x as i32 - ox;
                    if lx < 0 || lx >= layer_w {
                        continue;
                    }
                    let lx = lx as u32;
                    let (sr16, sg16, sb16, sa16) = self.tiles.get_pixel16(lx, ly);
                    let mut sa = (sa16 as f32 / 65535.0) * opacity;
                    if has_mask {
                        if let Some(ref mask) = self.mask {
                            sa *= mask.sample(lx, ly);
                        }
                    }
                    if mode == BlendMode::Dissolve {
                        let threshold = dissolve_threshold(x, y as u32);
                        if threshold >= sa {
                            continue;
                        }
                        sa = 1.0;
                    }
                    if sa < 0.001 {
                        continue;
                    }
                    let sr = sr16 as f32 / 65535.0;
                    let sg = sg16 as f32 / 65535.0;
                    let sb = sb16 as f32 / 65535.0;
                    let di = (x * 4) as usize;
                    let dr = row[di];
                    let dg = row[di + 1];
                    let db = row[di + 2];
                    let da = row[di + 3];
                    let (br, bg, bb) = blend_pixel(mode, sr, sg, sb, dr, dg, db);
                    let (out_r, out_g, out_b, out_a) =
                        alpha_composite(br, bg, bb, sr, sg, sb, sa, dr, dg, db, da);
                    row[di] = out_r;
                    row[di + 1] = out_g;
                    row[di + 2] = out_b;
                    row[di + 3] = out_a;
                }
            });
    }

    pub fn flatten_tiles(&self) -> Vec<u8> {
        self.tiles.flatten()
    }

    pub fn flatten_tiles_region(&self, rx: u32, ry: u32, rw: u32, rh: u32) -> Vec<u8> {
        let Some(len) = (rw as u64)
            .checked_mul(rh as u64)
            .and_then(|n| n.checked_mul(4))
            .and_then(|n| usize::try_from(n).ok())
        else {
            return Vec::new();
        };
        let mut out = vec![0u8; len];
        if rw == 0 || rh == 0 {
            return out;
        }
        self.flatten_tiles_region_into(rx, ry, rw, rh, &mut out);
        out
    }

    /// Variant that writes into an existing buffer instead of allocating a new Vec each time.
    /// The caller ensures buf.len() >= rw as usize * rh as usize * 4.
    pub fn flatten_tiles_region_into(&self, rx: u32, ry: u32, rw: u32, rh: u32, buf: &mut [u8]) {
        if rw == 0 || rh == 0 {
            return;
        }
        let Some(needed) = (rw as u64)
            .checked_mul(rh as u64)
            .and_then(|n| n.checked_mul(4))
            .and_then(|n| usize::try_from(n).ok())
        else {
            return;
        };
        if buf.len() < needed {
            return;
        }
        for b in buf[..needed].iter_mut() {
            *b = 0;
        }
        let end_y = ry + rh;
        let end_x = rx + rw;
        let tiles = &self.tiles;

        for y in ry..end_y {
            let ty = y / crate::core::tile::TILE_SIZE;
            let ty_rem = y % crate::core::tile::TILE_SIZE;

            let mut x = rx;
            while x < end_x {
                let tx = x / crate::core::tile::TILE_SIZE;
                let tx_rem = x % crate::core::tile::TILE_SIZE;
                let tile_w = crate::core::tile::TILE_SIZE - tx_rem;
                let copy_w = tile_w.min(end_x - x);

                let pos = crate::core::tile::TilePos {
                    x: tx as i32,
                    y: ty as i32,
                };
                if let Some(tile) = tiles.tiles.get(&pos) {
                    let src_i = ((ty_rem * crate::core::tile::TILE_SIZE + tx_rem) * 4) as usize;
                    let dst_i = (((y - ry) * rw + (x - rx)) * 4) as usize;
                    let bytes = (copy_w * 4) as usize;

                    if src_i + bytes <= tile.pixels.len() && dst_i + bytes <= buf.len() {
                        buf[dst_i..dst_i + bytes]
                            .copy_from_slice(&tile.pixels[src_i..src_i + bytes]);
                    }
                }
                x += copy_w;
            }
        }
    }

    pub fn update_tiles_region(&mut self, rx: u32, ry: u32, rw: u32, rh: u32, pixels: &[u8]) {
        if (self.locked && !self.is_background) || rw == 0 || rh == 0 {
            return;
        }
        if let Some(tiles) = self.get_paint_tiles_mut() {
            tiles.write_region(rx, ry, rw, rh, pixels);
        }
    }
}

#[derive(Clone)]
pub struct LayerStack {
    pub layers: Vec<Layer>,
    pub active_idx: usize,
    next_id: u32,
}

#[allow(dead_code)]
impl LayerStack {
    pub fn new(width: u32, height: u32) -> Self {
        let bg = Layer::new_white(0, "Background", width, height);
        Self {
            layers: vec![bg],
            active_idx: 0,
            next_id: 1,
        }
    }

    pub fn next_id(&self) -> u32 {
        self.next_id
    }
    pub fn set_next_id(&mut self, id: u32) {
        self.next_id = id;
    }

    pub fn repair_next_id(&mut self) {
        self.next_id = self
            .layers
            .iter()
            .map(|l| l.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
    }

    fn reserve_id(&mut self) -> u32 {
        while self.layers.iter().any(|l| l.id == self.next_id) {
            self.next_id = self.next_id.saturating_add(1);
        }
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    fn numbered_name(&self, prefix: &str, first: u32) -> String {
        let mut n = first;
        loop {
            let name = format!("{prefix} {n}");
            if !self.layers.iter().any(|l| l.name == name) {
                return name;
            }
            n = n.saturating_add(1);
        }
    }

    fn copy_name(&self, source_name: &str) -> String {
        self.copy_name_with_reserved(source_name, &mut Vec::new())
    }

    fn copy_name_with_reserved(&self, source_name: &str, reserved: &mut Vec<String>) -> String {
        let base = format!("{source_name} copy");
        let name_taken = |name: &str, reserved: &[String]| {
            self.layers.iter().any(|l| l.name == name) || reserved.iter().any(|n| n == name)
        };
        if !name_taken(&base, reserved) {
            reserved.push(base.clone());
            return base;
        }
        let mut n = 2;
        loop {
            let name = format!("{base} {n}");
            if !name_taken(&name, reserved) {
                reserved.push(name.clone());
                return name;
            }
            n += 1;
        }
    }

    pub fn normalize_active_idx(&mut self) {
        if self.active_idx >= self.layers.len() {
            self.active_idx = self.layers.len().saturating_sub(1);
        }
    }

    pub fn add_layer(&mut self, width: u32, height: u32) -> usize {
        let id = self.reserve_id();
        let name = self.numbered_name("Layer", 1);
        let layer = Layer::new(id, &name, width, height);
        self.normalize_active_idx();
        let idx = (self.active_idx + 1).min(self.layers.len());
        self.layers.insert(idx, layer);
        self.active_idx = idx;
        idx
    }

    pub fn add_adjustment_layer(&mut self, adj: AdjustmentType, width: u32, height: u32) -> usize {
        let id = self.reserve_id();
        let mut layer = Layer::new_adjustment(id, adj, width, height);
        layer.add_mask(true);
        layer.paint_target = PaintTarget::Pixels;
        self.normalize_active_idx();
        let idx = (self.active_idx + 1).min(self.layers.len());
        self.layers.insert(idx, layer);
        self.active_idx = idx;
        idx
    }

    /// Nesting depth of a layer (0 = top level), by walking the `parent_id`
    /// chain. Used for panel indentation.
    pub fn depth_of(&self, idx: usize) -> u32 {
        let mut depth = 0u32;
        let mut parent = self.layers.get(idx).and_then(|l| l.parent_id);
        let mut guard = 0;
        while let Some(pid) = parent {
            depth += 1;
            guard += 1;
            if guard > self.layers.len() {
                break;
            }
            parent = self
                .layers
                .iter()
                .find(|l| l.id == pid)
                .and_then(|l| l.parent_id);
        }
        depth
    }

    /// Whether `idx` should be hidden in the panel because some ancestor folder
    /// is collapsed (`expanded == false`). Walks the full parent chain so a
    /// collapsed grandparent hides everything beneath it.
    pub fn is_collapsed_hidden(&self, idx: usize) -> bool {
        let mut parent = self.layers.get(idx).and_then(|l| l.parent_id);
        let mut guard = 0;
        while let Some(pid) = parent {
            let Some(p) = self.layers.iter().find(|l| l.id == pid) else {
                break;
            };
            if p.is_group() && !p.expanded {
                return true;
            }
            parent = p.parent_id;
            guard += 1;
            if guard > self.layers.len() {
                break;
            }
        }
        false
    }

    /// Expand every collapsed ancestor folder of `idx` so its panel row becomes
    /// visible (used to reveal a newly-selected nested layer). Returns whether any
    /// folder's `expanded` flag actually changed. Walks the full parent chain,
    /// mirroring `is_collapsed_hidden`.
    pub fn expand_collapsed_ancestors(&mut self, idx: usize) -> bool {
        let mut parent = self.layers.get(idx).and_then(|l| l.parent_id);
        let mut changed = false;
        let mut guard = 0;
        while let Some(pid) = parent {
            let Some(p_idx) = self.layers.iter().position(|l| l.id == pid) else {
                break;
            };
            let p = &mut self.layers[p_idx];
            if p.is_group() && !p.expanded {
                p.expanded = true;
                changed = true;
            }
            parent = self.layers[p_idx].parent_id;
            guard += 1;
            if guard > self.layers.len() {
                break;
            }
        }
        changed
    }

    /// Effective visibility: a layer renders only if it is visible AND every
    /// ancestor folder is visible. This is how a group's eye hides its whole
    /// subtree without mutating each child's own `visible` flag (standard,
    /// non-destructive). Group headers themselves never paint (caller skips them).
    pub fn is_effectively_visible(&self, idx: usize) -> bool {
        let Some(l) = self.layers.get(idx) else {
            return false;
        };
        if !l.visible {
            return false;
        }
        let mut parent = l.parent_id;
        let mut guard = 0;
        while let Some(pid) = parent {
            let Some(p) = self.layers.iter().find(|x| x.id == pid) else {
                break;
            };
            if !p.visible {
                return false;
            }
            parent = p.parent_id;
            guard += 1;
            if guard > self.layers.len() {
                break;
            }
        }
        true
    }

    /// Direct children indices of a group id, in stack order (bottom→top).
    pub fn children_of(&self, group_id: u32) -> Vec<usize> {
        self.layers
            .iter()
            .enumerate()
            .filter(|(_, l)| l.parent_id == Some(group_id))
            .map(|(i, _)| i)
            .collect()
    }

    /// Whether `idx`'s parent chain passes through group id `gid`.
    fn is_descendant_of(&self, idx: usize, gid: u32) -> bool {
        let mut parent = self.layers.get(idx).and_then(|l| l.parent_id);
        let mut guard = 0;
        while let Some(pid) = parent {
            if pid == gid {
                return true;
            }
            guard += 1;
            if guard > self.layers.len() {
                break;
            }
            parent = self
                .layers
                .iter()
                .find(|l| l.id == pid)
                .and_then(|l| l.parent_id);
        }
        false
    }

    /// Members of the group whose header is at `group_idx`, as the half-open
    /// range `start..group_idx` (the header itself is at `group_idx`). Members
    /// are the maximal contiguous block immediately below the header belonging
    /// (directly or transitively) to this group. Non-group `group_idx` → empty.
    pub fn group_member_range(&self, group_idx: usize) -> std::ops::Range<usize> {
        match self.layers.get(group_idx) {
            Some(l) if l.is_group() => {}
            _ => return group_idx..group_idx,
        }
        let gid = self.layers[group_idx].id;
        let mut start = group_idx;
        while start > 0 && self.is_descendant_of(start - 1, gid) {
            start -= 1;
        }
        start..group_idx
    }

    /// Wrap the selected (or active) layers in a new group folder. Pulls the
    /// chosen layers into a contiguous run, inserts a `Group` header above them,
    /// and re-parents them to it. Returns the new header's index, or `None` when
    /// there is nothing groupable (e.g. only the background is selected).
    pub fn create_group_from_selected(&mut self, canvas_w: u32, canvas_h: u32) -> Option<usize> {
        let mut members: Vec<usize> = self
            .layers
            .iter()
            .enumerate()
            .filter(|(_, l)| l.selected && !l.is_background)
            .map(|(i, _)| i)
            .collect();
        if members.is_empty() {
            match self.layers.get(self.active_idx) {
                Some(l) if !l.is_background => members.push(self.active_idx),
                _ => return None,
            }
        }
        members.sort_unstable();

        let group_id = self.reserve_id();

        let mut grabbed: Vec<Layer> = Vec::with_capacity(members.len());
        for &i in members.iter().rev() {
            grabbed.push(self.layers.remove(i));
        }
        grabbed.reverse();

        let member_ids: std::collections::HashSet<u32> = grabbed.iter().map(|l| l.id).collect();
        for l in grabbed.iter_mut() {
            if l.parent_id.map_or(true, |p| !member_ids.contains(&p)) {
                l.parent_id = Some(group_id);
            }
            l.selected = false;
        }

        let at = members[0];
        let n = grabbed.len();
        for (k, l) in grabbed.into_iter().enumerate() {
            self.layers.insert(at + k, l);
        }

        let mut group = Layer::new_group(group_id, "Group", canvas_w, canvas_h);
        group.selected = true;
        group.expanded = false;
        let header_idx = at + n;
        self.layers.insert(header_idx, group);
        self.active_idx = header_idx;
        Some(header_idx)
    }

    /// Dissolve the group whose header is at `group_idx`: direct children are
    /// re-parented to the group's own parent, then the header is removed. The
    /// children keep their positions (they were already contiguous below the
    /// header). Returns false if `group_idx` is not a group.
    pub fn ungroup(&mut self, group_idx: usize) -> bool {
        let Some(header) = self.layers.get(group_idx) else {
            return false;
        };
        if !header.is_group() {
            return false;
        }
        let gid = header.id;
        let outer = header.parent_id;
        for l in self.layers.iter_mut() {
            if l.parent_id == Some(gid) {
                l.parent_id = outer;
            }
        }
        self.layers.remove(group_idx);
        self.normalize_active_idx();
        true
    }

    pub fn duplicate_layer(&mut self, idx: usize) -> usize {
        if idx >= self.layers.len() {
            return idx;
        }
        let id = self.reserve_id();
        let source_name = self.layers[idx].name.clone();
        let mut new_layer = self.layers[idx].duplicate(id);
        new_layer.name = self.copy_name(&source_name);
        let insert = idx + 1;
        self.layers.insert(insert, new_layer);
        self.active_idx = insert;
        insert
    }

    pub fn translate_active_layer(&mut self, dx: i32, dy: i32) {
        if self.active_idx >= self.layers.len() {
            return;
        }
        let layer = &mut self.layers[self.active_idx];
        if layer.locked || layer.is_background {
            return;
        }
        layer.tiles.translate(dx, dy);
    }

    pub fn remove_layer(&mut self, idx: usize) -> bool {
        if self.layers.len() <= 1 {
            return false;
        }
        if idx >= self.layers.len() {
            return false;
        }
        let removed_id = self.layers[idx].id;
        self.layers.remove(idx);
        for layer in &mut self.layers {
            if layer.clip_parent_id == Some(removed_id) {
                layer.clip_parent_id = None;
            }
        }
        self.repair_clip_relations();
        if idx < self.active_idx {
            self.active_idx -= 1;
        } else if self.active_idx >= self.layers.len() {
            self.active_idx = self.layers.len().saturating_sub(1);
        }
        true
    }

    pub fn rename_layer(&mut self, idx: usize, name: &str) {
        if idx < self.layers.len() {
            self.layers[idx].name = name.to_string();
        }
    }

    pub fn unlock_background_layer(&mut self, idx: usize) -> bool {
        let Some(layer) = self.layers.get_mut(idx) else {
            return false;
        };
        if !layer.is_background {
            return false;
        }
        layer.is_background = false;
        layer.locked = false;
        layer.name = "Layer 0".to_string();
        layer.selected = true;
        self.active_idx = idx;
        true
    }

    pub fn merge_down(&mut self, idx: usize) -> bool {
        if idx == 0 || idx >= self.layers.len() {
            return false;
        }
        let mut top = self.layers[idx].clone();
        // Visibility priority: a hidden layer contributes nothing to the merge,
        // and the result is visible when either input was — so merging a visible
        // top onto a hidden bottom keeps the top, not the hidden bottom.
        let top_visible = top.visible;
        let bottom_visible = self.layers[idx - 1].visible;
        let bottom = &mut self.layers[idx - 1];
        // Bake the bottom mask into its alpha first: the merged pixels are a
        // new image, so keeping the old mask would cut the freshly blended
        // content (the top layer's own mask is applied by blend_onto_region).
        if let Some(mask) = bottom.mask.take() {
            if mask.enabled {
                mask.apply_to_layer(bottom);
            } else {
                bottom.mask_active = false;
            }
        }
        let w = bottom.width;
        let h = bottom.height;

        let chunk_size = 256;
        let mut patch = vec![0u8; chunk_size * chunk_size * 4];

        for cy in (0..h).step_by(chunk_size) {
            for cx in (0..w).step_by(chunk_size) {
                let cw = chunk_size.min((w - cx) as usize) as u32;
                let ch = chunk_size.min((h - cy) as usize) as u32;
                let needed = (cw * ch * 4) as usize;

                if bottom_visible {
                    bottom
                        .tiles
                        .flatten_tiles_region_into(cx, cy, cw, ch, &mut patch[..needed]);
                } else {
                    // Hidden bottom: start from transparent so only the visible
                    // top contributes (blend_onto_region no-ops a hidden top).
                    for b in patch[..needed].iter_mut() {
                        *b = 0;
                    }
                }
                let saved = top.offset;
                top.offset = (saved.0 - cx as i32, saved.1 - cy as i32);
                top.blend_onto_region(&mut patch[..needed], cw, 0, 0, cw, ch);
                top.offset = saved;
                bottom.tiles.write_region(cx, cy, cw, ch, &patch[..needed]);
            }
        }

        bottom.blend_mode = BlendMode::Normal;
        bottom.opacity = 1.0;
        bottom.layer_type = LayerType::Raster;
        bottom.visible = top_visible || bottom_visible;
        self.layers.remove(idx);
        if self.active_idx >= self.layers.len() {
            self.active_idx = self.layers.len() - 1;
        }
        true
    }

    /// Duplicate a folder (header + members) as a new block placed directly above
    /// the original, with fresh ids and child→group links remapped. Returns the
    /// new header index. Single-level (nested folders are Phase 3).
    pub fn duplicate_group(&mut self, header_idx: usize) -> usize {
        if header_idx >= self.layers.len() || !self.layers[header_idx].is_group() {
            return header_idx;
        }
        let start = self.group_member_range(header_idx).start;
        let old_gid = self.layers[header_idx].id;
        let new_gid = self.reserve_id();
        let mut id_map = std::collections::HashMap::new();
        id_map.insert(old_gid, new_gid);
        for i in start..header_idx {
            id_map.insert(self.layers[i].id, self.reserve_id());
        }

        let mut block: Vec<Layer> = Vec::with_capacity(header_idx - start + 1);
        let mut generated_names = Vec::with_capacity(header_idx - start + 1);
        for i in start..=header_idx {
            let nid = id_map[&self.layers[i].id];
            let source_name = self.layers[i].name.clone();
            let mut d = self.layers[i].duplicate(nid);
            d.name = self.copy_name_with_reserved(&source_name, &mut generated_names);
            d.parent_id = self.layers[i]
                .parent_id
                .map(|parent| id_map.get(&parent).copied().unwrap_or(parent));
            d.clip_parent_id = self.layers[i]
                .clip_parent_id
                .map(|parent| id_map.get(&parent).copied().unwrap_or(parent));
            d.selected = false;
            block.push(d);
        }
        let insert_at = header_idx + 1;
        let block_len = block.len();
        for (k, d) in block.into_iter().enumerate() {
            self.layers.insert(insert_at + k, d);
        }
        let new_header = insert_at + block_len - 1;
        self.active_idx = new_header;
        new_header
    }

    /// Paste a block of cloned layers (from the clipboard) on top of the stack,
    /// remapping ids and internal `parent_id` links so a copied group folder and its
    /// children come across intact and never collide with existing ids. `block` is in
    /// bottom→top stack order (a group's members precede its header); the pasted
    /// layers become the selection and the top one becomes active. Returns the new
    /// active index (or the current one if the block is empty).
    pub fn paste_layers(&mut self, block: &[Layer]) -> usize {
        if block.is_empty() {
            return self.active_idx;
        }
        for l in self.layers.iter_mut() {
            l.selected = false;
        }
        let mut id_map: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        for src in block {
            id_map.insert(src.id, self.reserve_id());
        }
        for src in block {
            let mut l = src.clone();
            l.id = id_map[&src.id];
            // Keep membership only for a parent that was copied too; an outer folder
            // left behind becomes a top-level paste.
            l.parent_id = src.parent_id.and_then(|p| id_map.get(&p).copied());
            // A copied clip relation survives only when its frame was copied in
            // the same block. Never leave a pasted child pointing into the
            // source document/block.
            l.clip_parent_id = src.clip_parent_id.and_then(|p| id_map.get(&p).copied());
            l.is_background = false;
            l.locked = false;
            l.selected = true;
            self.layers.push(l);
        }
        self.active_idx = self.layers.len() - 1;
        self.active_idx
    }

    /// Merge a folder into one raster layer: the subtree is flattened, the group
    /// opacity + mask baked into the pixels, and the group's blend mode kept on
    /// the result so it still blends with layers below as the folder did.
    /// Single-level (nested folders are Phase 3).
    /// Bake a group folder's opacity and (enabled) mask into the alpha of an
    /// already-composited region buffer `buf` (straight-alpha RGBA, `cw`×`ch`,
    /// covering the canvas region starting at `(cx, cy)`). Equivalent to
    /// compositing the subtree through the group's opacity+mask with Normal blend,
    /// which only scale the source alpha (RGB unchanged); the caller applies the
    /// group's own blend mode separately.
    fn bake_opacity_mask_region(buf: &mut [u8], cw: u32, ch: u32, cx: u32, cy: u32, group: &Layer) {
        let opacity = group.opacity.clamp(0.0, 1.0);
        let mask = group.mask.as_ref().filter(|m| m.enabled);
        if opacity >= 0.999 && mask.is_none() {
            return;
        }
        for py in 0..ch {
            for px in 0..cw {
                let i = ((py * cw + px) * 4 + 3) as usize;
                let mut a = buf[i] as f32 / 255.0 * opacity;
                if let Some(m) = mask {
                    a *= m.sample(cx + px, cy + py);
                }
                buf[i] = (a * 255.0).clamp(0.0, 255.0) as u8;
            }
        }
    }

    pub fn merge_group(&mut self, header_idx: usize, width: u32, height: u32) -> bool {
        if header_idx >= self.layers.len() || !self.layers[header_idx].is_group() {
            return false;
        }
        let start = self.group_member_range(header_idx).start;

        let (gid, name, parent, group_blend) = {
            let g = &self.layers[header_idx];
            (g.id, g.name.clone(), g.parent_id, g.blend_mode)
        };

        let new_tiles = if crate::core::canvas::Canvas::fits_flat_buffer(width, height) {
            // Small canvas: exact original path (full-canvas subtree + synth bake).
            let subtree = self.flatten_group_subtree_region(header_idx, 0, 0, width, height);
            let group = &self.layers[header_idx];
            let mut synth = Layer::from_rgba(0, "", subtree, width, height);
            synth.opacity = group.opacity;
            synth.blend_mode = BlendMode::Normal;
            synth.mask = group.mask.clone();
            let mut baked = vec![0u8; width as usize * height as usize * 4];
            synth.blend_onto(&mut baked, width);
            crate::core::tile::TileMap::from_rgba(&baked, width, height)
        } else {
            // Large canvas: composite the subtree and bake opacity+mask one 256-px
            // chunk at a time (no canvas-sized buffer) so Merge Group runs under
            // Viewport Streaming. flatten_group_subtree_region is already region-based.
            let mut new_tiles = crate::core::tile::TileMap::new(width, height);
            let chunk = 256u32;
            let mut cy = 0;
            while cy < height {
                let ch = chunk.min(height - cy);
                let mut cx = 0;
                while cx < width {
                    let cw = chunk.min(width - cx);
                    let mut buf = self.flatten_group_subtree_region(header_idx, cx, cy, cw, ch);
                    Self::bake_opacity_mask_region(
                        &mut buf,
                        cw,
                        ch,
                        cx,
                        cy,
                        &self.layers[header_idx],
                    );
                    new_tiles.write_region(cx, cy, cw, ch, &buf);
                    cx += cw;
                }
                cy += ch;
            }
            new_tiles
        };

        // The group's blend mode stays on the merged layer (applied when it is later
        // composited); opacity/mask are already baked in above.
        let mut merged = Layer::new(gid, &name, width, height);
        merged.tiles = new_tiles;
        merged.blend_mode = group_blend;
        merged.parent_id = parent;

        self.layers.drain(start..=header_idx);
        self.layers.insert(start, merged);
        self.active_idx = start;
        true
    }

    /// Apply (bake + remove) the mask on the layer at `idx`. A regular layer bakes
    /// the mask into its own pixels; a group folder holds no pixels of its own, so
    /// its mask is baked into each raster child's alpha instead and the header just
    /// loses the mask. Routing both cases through here fixes "Apply Mask" on a
    /// group silently dropping the mask and un-clipping the whole subtree.
    pub fn apply_layer_mask(&mut self, idx: usize) {
        if idx >= self.layers.len() {
            return;
        }
        if self.layers[idx].is_group() {
            self.apply_group_mask(idx);
        } else {
            self.layers[idx].apply_mask();
        }
    }

    /// Bake a group folder's mask into its direct raster children, then drop the
    /// mask from the header. A disabled mask isn't clipping anything on screen, so
    /// it's removed without touching pixels. Non-raster children (adjustments,
    /// nested folders) need no explicit clip: wherever the mask hides the group,
    /// every raster child becomes transparent, so the group's isolated buffer is
    /// empty there and an adjustment over it has nothing to show.
    fn apply_group_mask(&mut self, header_idx: usize) {
        let gid = self.layers[header_idx].id;
        let mask = self.layers[header_idx].mask.take();
        self.layers[header_idx].mask_active = false;
        self.layers[header_idx].paint_target = PaintTarget::Pixels;
        let Some(mask) = mask else {
            return;
        };
        if !mask.enabled {
            return;
        }
        for child in self.layers.iter_mut() {
            if child.parent_id == Some(gid) && child.is_raster() {
                mask.bake_into_child_alpha(child);
            }
        }
    }

    /// Stamp Visible (Ctrl+Shift+E): flatten every eye-on layer into ONE new
    /// raster layer placed on TOP of the stack, keeping all originals (the standard
    /// "Stamp Visible" snapshot). Returns false when nothing is visible.
    pub fn merge_visible(&mut self, width: u32, height: u32) -> bool {
        let any_visible = (0..self.layers.len()).any(|i| self.is_effectively_visible(i));
        if !any_visible {
            return false;
        }
        let tiles = if crate::core::canvas::Canvas::fits_flat_buffer(width, height) {
            // Small canvas: exact original path (full-canvas flatten).
            crate::core::tile::TileMap::from_rgba(&self.flatten(width, height), width, height)
        } else {
            self.flatten_into_tiles(width, height)
        };
        let id = self.reserve_id();
        let mut merged = Layer::new(id, "Merged", width, height);
        merged.tiles = tiles;
        self.layers.push(merged);
        self.active_idx = self.layers.len() - 1;
        true
    }

    /// 16-bit Stamp Visible: composite the visible stack at full precision and
    /// push it as a new 16-bit "Merged" layer on top.
    pub fn merge_visible16(&mut self, width: u32, height: u32) -> bool {
        let any_visible = (0..self.layers.len()).any(|i| self.is_effectively_visible(i));
        if !any_visible {
            return false;
        }
        let tiles = if crate::core::canvas::Canvas::fits_flat_buffer(width, height) {
            // Small canvas: exact original path (full-canvas flatten16).
            let flat16 = self.flatten16(width, height);
            crate::core::tile::TileMap::from_rgba16(&flat16, width, height)
        } else {
            self.flatten16_into_tiles(width, height)
        };
        let id = self.reserve_id();
        let mut merged = Layer::new(id, "Merged", width, height);
        merged.tiles = tiles;
        self.layers.push(merged);
        self.active_idx = self.layers.len() - 1;
        true
    }

    /// Per-layer precomputation for the chunked composite: effective visibility
    /// plus each non-group child's isolated-group target id (None if it
    /// composites straight onto the result). Precomputed so the chunk loop can
    /// take `&mut self`.
    fn flatten_chunk_plan(&self) -> (Vec<bool>, Vec<Option<u32>>) {
        let eff: Vec<bool> = (0..self.layers.len())
            .map(|i| self.is_effectively_visible(i))
            .collect();
        let targets: Vec<Option<u32>> = self
            .layers
            .iter()
            .map(|layer| {
                if layer.is_group() {
                    return None;
                }
                layer.parent_id.and_then(|pid| {
                    self.layers
                        .iter()
                        .find(|g| g.id == pid && g.is_group() && Self::group_needs_isolation(g))
                        .map(|g| g.id)
                })
            })
            .collect();
        (eff, targets)
    }

    /// Composite the whole visible stack (isolated groups included) for ONE
    /// rectangular chunk of the canvas into a chunk-sized RGBA buffer, via the
    /// offset-shift trick (each layer is repositioned so the chunk falls at the
    /// buffer's origin, blended, then restored). `eff`/`targets` come from
    /// [`Self::flatten_chunk_plan`].
    fn flatten_chunk(
        &mut self,
        eff: &[bool],
        targets: &[Option<u32>],
        cx: u32,
        cy: u32,
        cw: u32,
        ch: u32,
    ) -> Vec<u8> {
        use std::collections::HashMap;
        let n4 = (cw as usize) * (ch as usize) * 4;
        let mut out = vec![0u8; n4];
        let mut group_bufs: HashMap<u32, Vec<u8>> = HashMap::new();

        for i in 0..self.layers.len() {
            if self.layers[i].is_group() {
                if eff[i] && Self::group_needs_isolation(&self.layers[i]) {
                    if let Some(mut buf) = group_bufs.remove(&self.layers[i].id) {
                        Self::bake_opacity_mask_region(&mut buf, cw, ch, cx, cy, &self.layers[i]);
                        let mut synth = Layer::from_rgba(0, "", buf, cw, ch);
                        synth.blend_mode = self.layers[i].blend_mode;
                        synth.blend_onto_region(&mut out, cw, 0, 0, cw, ch);
                    }
                }
                continue;
            }
            if !eff[i] {
                continue;
            }
            let layer = &mut self.layers[i];
            let saved = layer.offset;
            layer.offset = (saved.0 - cx as i32, saved.1 - cy as i32);
            match targets[i] {
                Some(gid) => {
                    let buf = group_bufs.entry(gid).or_insert_with(|| vec![0u8; n4]);
                    layer.blend_onto_region(buf, cw, 0, 0, cw, ch);
                }
                None => layer.blend_onto_region(&mut out, cw, 0, 0, cw, ch),
            }
            self.layers[i].offset = saved;
        }
        out
    }

    /// Composite the visible stack for one full-width row band, top-to-bottom
    /// streaming order — no canvas-sized buffer is ever built. Feeds the
    /// Viewport-Streaming print/PDF path (and any other consumer that can eat
    /// the canvas in horizontal slices).
    pub fn flatten_band(&mut self, width: u32, height: u32, y0: u32, band_h: u32) -> Vec<u8> {
        let bh = band_h.min(height.saturating_sub(y0));
        if width == 0 || bh == 0 {
            return Vec::new();
        }
        let (eff, targets) = self.flatten_chunk_plan();
        self.flatten_chunk(&eff, &targets, 0, y0, width, bh)
    }

    /// Tile-native counterpart of [`Self::flatten`]: composite the whole visible
    /// stack (isolated groups included) into a fresh TileMap in 256-px chunks with
    /// no canvas-sized buffer, so Stamp Visible runs under Viewport Streaming. Used
    /// only for large canvases; the common path keeps the exact `flatten`.
    fn flatten_into_tiles(&mut self, width: u32, height: u32) -> crate::core::tile::TileMap {
        let (eff, targets) = self.flatten_chunk_plan();
        let mut new_tiles = crate::core::tile::TileMap::new(width, height);
        let chunk = 256u32;
        let mut cy = 0;
        while cy < height {
            let ch = chunk.min(height - cy);
            let mut cx = 0;
            while cx < width {
                let cw = chunk.min(width - cx);
                let out = self.flatten_chunk(&eff, &targets, cx, cy, cw, ch);
                new_tiles.write_region(cx, cy, cw, ch, &out);
                cx += cw;
            }
            cy += ch;
        }
        new_tiles
    }

    /// Tile-native counterpart of [`Self::flatten16`]: composite the visible stack
    /// into a fresh 16-bit TileMap in 256-px chunks (f32 accumulator), no
    /// canvas-sized buffer. Effected groups fall back to the 8-bit chunked
    /// [`Self::flatten_into_tiles`] promoted to 16-bit — matching flatten16, which
    /// also drops to 8-bit precision for isolated groups.
    fn flatten16_into_tiles(&mut self, width: u32, height: u32) -> crate::core::tile::TileMap {
        if self.has_effected_groups() {
            let mut t = self.flatten_into_tiles(width, height);
            t.promote_to_hdr();
            return t;
        }
        let eff: Vec<bool> = (0..self.layers.len())
            .map(|i| self.is_effectively_visible(i))
            .collect();
        let mut new_tiles = crate::core::tile::TileMap::new(width, height);
        let chunk = 256u32;
        let mut acc = vec![0f32; (chunk * chunk * 4) as usize];
        let mut buf16 = vec![0u16; (chunk * chunk * 4) as usize];
        let mut cy = 0;
        while cy < height {
            let ch = chunk.min(height - cy);
            let mut cx = 0;
            while cx < width {
                let cw = chunk.min(width - cx);
                let n4 = (cw * ch * 4) as usize;
                acc[..n4].fill(0.0);
                for (i, layer) in self.layers.iter_mut().enumerate() {
                    if eff[i] && !layer.is_group() {
                        let saved = layer.offset;
                        layer.offset = (saved.0 - cx as i32, saved.1 - cy as i32);
                        layer.blend_onto_f32(&mut acc[..n4], cw, ch);
                        layer.offset = saved;
                    }
                }
                // Straight-alpha quantize (Stamp Visible keeps transparency).
                for (o, &v) in buf16[..n4].iter_mut().zip(acc[..n4].iter()) {
                    *o = (v * 65535.0).round().clamp(0.0, 65535.0) as u16;
                }
                new_tiles.write_region16(cx, cy, cw, ch, &buf16[..n4]);
                cx += cw;
            }
            cy += ch;
        }
        new_tiles
    }

    pub fn merge_all(&mut self, width: u32, height: u32) {
        let mut new_tiles = crate::core::tile::TileMap::new_white(width, height);
        let chunk_size = 256;
        let mut patch = vec![0u8; chunk_size * chunk_size * 4];

        let eff: Vec<bool> = (0..self.layers.len())
            .map(|i| self.is_effectively_visible(i))
            .collect();

        for cy in (0..height).step_by(chunk_size) {
            for cx in (0..width).step_by(chunk_size) {
                let cw = chunk_size.min((width - cx) as usize) as u32;
                let ch = chunk_size.min((height - cy) as usize) as u32;
                let needed = (cw * ch * 4) as usize;

                new_tiles.flatten_tiles_region_into(cx, cy, cw, ch, &mut patch[..needed]);
                for (i, layer) in self.layers.iter_mut().enumerate() {
                    if eff[i] {
                        let saved = layer.offset;
                        layer.offset = (saved.0 - cx as i32, saved.1 - cy as i32);
                        layer.blend_onto_region(&mut patch[..needed], cw, 0, 0, cw, ch);
                        layer.offset = saved;
                    }
                }
                new_tiles.write_region(cx, cy, cw, ch, &patch[..needed]);
            }
        }

        self.layers.clear();
        self.next_id = 1;
        let mut bg = Layer::new_white(0, "Background", width, height);
        bg.tiles = new_tiles;
        self.layers.push(bg);
        self.active_idx = 0;
    }

    /// 16-bit Flatten Image: composite the visible stack at full precision, bake
    /// it over an opaque white background and replace the stack with one 16-bit
    /// Background layer.
    pub fn merge_all16(&mut self, width: u32, height: u32) {
        // Chunked (256-px) so a >25M px 16-bit doc (a large RAW) flattens without a
        // canvas-sized f32/u16 buffer. Mirrors flatten16's pass-through-group blend
        // via blend_onto_f32 with the offset-shift trick, over opaque white, into
        // 16-bit tiles.
        let mut new_tiles = crate::core::tile::TileMap::new(width, height);
        let eff: Vec<bool> = (0..self.layers.len())
            .map(|i| self.is_effectively_visible(i))
            .collect();
        let chunk = 256u32;
        let mut acc = vec![0f32; (chunk * chunk * 4) as usize];
        let mut buf16 = vec![0u16; (chunk * chunk * 4) as usize];
        let mut cy = 0;
        while cy < height {
            let ch = chunk.min(height - cy);
            let mut cx = 0;
            while cx < width {
                let cw = chunk.min(width - cx);
                let n4 = (cw * ch * 4) as usize;
                acc[..n4].fill(0.0);
                for (i, layer) in self.layers.iter_mut().enumerate() {
                    if eff[i] && !layer.is_group() {
                        let saved = layer.offset;
                        layer.offset = (saved.0 - cx as i32, saved.1 - cy as i32);
                        layer.blend_onto_f32(&mut acc[..n4], cw, ch);
                        layer.offset = saved;
                    }
                }
                // Straight-alpha result over opaque white (Flatten discards
                // transparency), then quantize to 16-bit.
                for (o, px) in buf16[..n4]
                    .iter_mut()
                    .zip(acc[..n4].chunks_exact(4).flat_map(|p| {
                        let a = p[3].clamp(0.0, 1.0);
                        [
                            (p[0] * a + (1.0 - a)),
                            (p[1] * a + (1.0 - a)),
                            (p[2] * a + (1.0 - a)),
                            1.0,
                        ]
                    }))
                {
                    *o = (px * 65535.0).round().clamp(0.0, 65535.0) as u16;
                }
                new_tiles.write_region16(cx, cy, cw, ch, &buf16[..n4]);
                cx += cw;
            }
            cy += ch;
        }
        self.layers.clear();
        self.next_id = 1;
        let mut bg = Layer::new_white(0, "Background", width, height);
        bg.tiles = new_tiles;
        self.layers.push(bg);
        self.active_idx = 0;
    }

    /// Merge all selected (selected=true) layers into one at the lowest position.
    /// If only one layer is selected → merge_down at active_idx.
    /// Returns true if the merge succeeded.
    pub fn merge_selected(&mut self, canvas_width: u32, canvas_height: u32) -> bool {
        let selected_idxs: Vec<usize> = self
            .layers
            .iter()
            .enumerate()
            .filter(|(_, l)| {
                l.selected
                    && !l.locked
                    && !l.is_background
                    && matches!(l.layer_type, LayerType::Raster)
            })
            .map(|(i, _)| i)
            .collect();

        if selected_idxs.len() < 2 {
            return self.merge_down(self.active_idx);
        }

        // Visibility priority: hidden selected layers are skipped by
        // blend_onto_region, so the composite already excludes them; the result
        // must stay visible when any input was (not inherit a hidden bottom).
        let any_visible = selected_idxs.iter().any(|&i| self.layers[i].visible);

        let min_ox = selected_idxs
            .iter()
            .map(|&i| self.layers[i].offset.0)
            .min()
            .unwrap_or(0);
        let min_oy = selected_idxs
            .iter()
            .map(|&i| self.layers[i].offset.1)
            .min()
            .unwrap_or(0);
        let max_ex = selected_idxs
            .iter()
            .map(|&i| self.layers[i].offset.0 + self.layers[i].width as i32)
            .max()
            .unwrap_or(0);
        let max_ey = selected_idxs
            .iter()
            .map(|&i| self.layers[i].offset.1 + self.layers[i].height as i32)
            .max()
            .unwrap_or(0);

        let ox = min_ox.max(0) as u32;
        let oy = min_oy.max(0) as u32;
        let ex = (max_ex.max(0) as u32).min(canvas_width);
        let ey = (max_ey.max(0) as u32).min(canvas_height);
        if ex <= ox || ey <= oy {
            return false;
        }
        let w = ex - ox;
        let h = ey - oy;

        let chunk_size = 256usize;
        let Some(merged_len) = (w as u64)
            .checked_mul(h as u64)
            .and_then(|n| n.checked_mul(4))
            .and_then(|n| usize::try_from(n).ok())
        else {
            return false;
        };
        let mut merged_pixels = vec![0u8; merged_len];
        let mut patch = vec![0u8; chunk_size * chunk_size * 4];

        for cy in (0..h as usize).step_by(chunk_size) {
            for cx in (0..w as usize).step_by(chunk_size) {
                let cw = chunk_size.min(w as usize - cx) as u32;
                let ch = chunk_size.min(h as usize - cy) as u32;
                let needed = (cw * ch * 4) as usize;
                let patch_slice = &mut patch[..needed];

                for b in patch_slice.iter_mut() {
                    *b = 0;
                }

                for &idx in &selected_idxs {
                    let saved = self.layers[idx].offset;
                    self.layers[idx].offset = (
                        saved.0 - (ox + cx as u32) as i32,
                        saved.1 - (oy + cy as u32) as i32,
                    );
                    self.layers[idx].blend_onto_region(patch_slice, cw, 0, 0, cw, ch);
                    self.layers[idx].offset = saved;
                }

                for py in 0..ch {
                    let src_start = (py * cw * 4) as usize;
                    let dst_start = (((cy as u32 + py) * w + cx as u32) * 4) as usize;
                    let row_len = (cw * 4) as usize;
                    merged_pixels[dst_start..dst_start + row_len]
                        .copy_from_slice(&patch_slice[src_start..src_start + row_len]);
                }
            }
        }

        let merged_tiles = crate::core::tile::TileMap::from_rgba(&merged_pixels, w, h);

        let bottom_idx = selected_idxs[0];
        self.layers[bottom_idx].tiles = merged_tiles;
        self.layers[bottom_idx].width = w;
        self.layers[bottom_idx].height = h;
        self.layers[bottom_idx].offset = (ox as i32, oy as i32);
        self.layers[bottom_idx].blend_mode = BlendMode::Normal;
        self.layers[bottom_idx].opacity = 1.0;
        self.layers[bottom_idx].selected = false;
        self.layers[bottom_idx].name = "Merged".to_string();
        self.layers[bottom_idx].layer_type = LayerType::Raster;
        self.layers[bottom_idx].mask = None;
        self.layers[bottom_idx].visible = any_visible;

        for &idx in selected_idxs[1..].iter().rev() {
            self.layers.remove(idx);
        }

        if self.active_idx >= self.layers.len() {
            self.active_idx = self.layers.len().saturating_sub(1);
        }
        self.active_idx = bottom_idx.min(self.layers.len().saturating_sub(1));

        true
    }

    pub fn move_layer_up(&mut self, idx: usize) -> bool {
        if idx + 1 >= self.layers.len() {
            return false;
        }
        self.layers.swap(idx, idx + 1);
        if self.active_idx == idx {
            self.active_idx += 1;
        } else if self.active_idx == idx + 1 {
            self.active_idx -= 1;
        }
        true
    }

    pub fn move_layer_down(&mut self, idx: usize) -> bool {
        if idx == 0 {
            return false;
        }
        self.layers.swap(idx, idx - 1);
        if self.active_idx == idx {
            self.active_idx -= 1;
        } else if self.active_idx == idx - 1 {
            self.active_idx += 1;
        }
        true
    }

    pub fn move_layer_to(&mut self, src: usize, dst: usize) -> bool {
        if src >= self.layers.len() || dst > self.layers.len() || src == dst {
            return false;
        }
        self.normalize_active_idx();
        let active_id = self.layers[self.active_idx].id;
        let layer = self.layers.remove(src);

        let insert_idx = if src < dst { dst - 1 } else { dst };
        self.layers.insert(insert_idx, layer);

        self.active_idx = self
            .layers
            .iter()
            .position(|l| l.id == active_id)
            .unwrap_or(0);
        true
    }

    /// Infer the `parent_id` for a layer that has just landed at index `i`:
    /// dropped directly under an EXPANDED folder header → first child of it;
    /// otherwise adopt the group of the row below (a child → same group; a
    /// top-level row or a folder header → that row's own parent level).
    fn infer_parent_at(&self, i: usize) -> Option<u32> {
        if let Some(above) = self.layers.get(i + 1) {
            if above.is_group() && above.expanded {
                return Some(above.id);
            }
        }
        if i > 0 {
            if let Some(below) = self.layers.get(i - 1) {
                return below.parent_id;
            }
        }
        None
    }

    /// Move a whole folder (header + its members) as one block to `dst`. The
    /// header is re-leveled to top-level (nested groups are Phase 3); members
    /// keep their parent so the run stays intact.
    fn move_group_block(&mut self, header_idx: usize, dst: usize) {
        let range = self.group_member_range(header_idx);
        let block_start = range.start;
        let block_len = header_idx - block_start + 1;
        if dst >= block_start && dst <= header_idx + 1 {
            return;
        }
        let block: Vec<Layer> = self.layers.drain(block_start..=header_idx).collect();
        let mut insert_at = if dst > header_idx {
            dst - block_len
        } else {
            dst.min(block_start)
        };
        insert_at = insert_at.min(self.layers.len());
        for (k, l) in block.into_iter().enumerate() {
            self.layers.insert(insert_at + k, l);
        }
        let header_new = insert_at + block_len - 1;
        if let Some(h) = self.layers.get_mut(header_new) {
            h.parent_id = None;
        }
    }

    /// Drag-and-drop reorder from the layer panel, group-aware (C-2):
    /// dragging a folder header moves the whole folder; any other layer is
    /// re-parented based on where it lands (into a folder, or out of one).
    /// Maintains the contiguous-run invariant. Returns false on a no-op.
    pub fn drag_layer_to(&mut self, src: usize, dst: usize) -> bool {
        let n = self.layers.len();
        if src >= n || dst > n || src == dst {
            return false;
        }
        self.normalize_active_idx();
        let active_id = self.layers.get(self.active_idx).map(|l| l.id);

        if self.layers[src].is_group() {
            self.move_group_block(src, dst);
        } else {
            let moved_id = self.layers[src].id;
            self.move_layer_to(src, dst);
            if let Some(i) = self.layers.iter().position(|l| l.id == moved_id) {
                self.layers[i].parent_id = self.infer_parent_at(i);
            }
        }

        if let Some(aid) = active_id {
            self.active_idx = self.layers.iter().position(|l| l.id == aid).unwrap_or(0);
        }
        true
    }

    pub fn active_layer(&self) -> &Layer {
        let idx = self.active_idx.min(self.layers.len().saturating_sub(1));
        &self.layers[idx]
    }

    pub fn active_layer_mut(&mut self) -> &mut Layer {
        self.normalize_active_idx();
        &mut self.layers[self.active_idx]
    }

    /// Whether a group folder must be composited in isolation: its subtree is
    /// flattened on its own, then blended with the group's opacity/blend mode.
    /// A pass-through group (Normal, 100%) is identical to compositing its
    /// children inline, so we skip the extra buffer in that case.
    fn group_needs_isolation(group: &Layer) -> bool {
        group.opacity < 0.999
            || group.blend_mode != BlendMode::Normal
            || group.mask.as_ref().map_or(false, |m| m.enabled)
    }

    /// Flatten all layers into one buffer over a transparent background.
    /// If the document has a real Background layer, it provides the white/opaque base.
    ///
    /// Group folders with opacity < 100% or blend ≠ Normal are composited ISOLATED
    /// (Phase 2): child layers merge into their own buffer (transparent background), then that
    /// buffer is blended onto the result using the group's opacity/blend. Single-level; nested
    /// effected groups are Phase 3.
    pub fn flatten(&self, width: u32, height: u32) -> Vec<u8> {
        let mut out = vec![0u8; (width as u64 * height as u64 * 4) as usize];
        let mut group_bufs: std::collections::HashMap<u32, Vec<u8>> =
            std::collections::HashMap::new();

        for (i, layer) in self.layers.iter().enumerate() {
            if layer.is_group() {
                if self.is_effectively_visible(i) && Self::group_needs_isolation(layer) {
                    if let Some(buf) = group_bufs.remove(&layer.id) {
                        let mut synth = Layer::from_rgba(0, "", buf, width, height);
                        synth.opacity = layer.opacity;
                        synth.blend_mode = layer.blend_mode;
                        synth.mask = layer.mask.clone();
                        synth.blend_onto(&mut out, width);
                    }
                }
                continue;
            }
            if !self.is_effectively_visible(i) {
                continue;
            }
            let target = layer.parent_id.and_then(|pid| {
                self.layers
                    .iter()
                    .find(|g| g.id == pid && g.is_group() && Self::group_needs_isolation(g))
                    .map(|g| g.id)
            });
            match target {
                Some(gid) => {
                    let buf = group_bufs
                        .entry(gid)
                        .or_insert_with(|| vec![0u8; out.len()]);
                    layer.blend_onto(buf, width);
                }
                None => layer.blend_onto(&mut out, width),
            }
        }
        out
    }

    /// 16-bit counterpart of [`Self::flatten`]: composite the visible stack into
    /// a `width*height*4` u16 buffer at full precision (f32 accumulator). Used by
    /// 16-bit export / Flatten Image / Stamp Visible. Effected groups (isolated
    /// opacity/blend) are rare in 16-bit photo work and still composite at 8-bit
    /// precision via the up-converted `flatten` fallback — a 16-bit isolated-group
    /// path is future work.
    pub fn flatten16(&self, width: u32, height: u32) -> Vec<u16> {
        if self.has_effected_groups() {
            return self
                .flatten(width, height)
                .iter()
                .map(|&v| v as u16 * 257)
                .collect();
        }
        let n = (width as u64 * height as u64 * 4) as usize;
        let mut acc = vec![0f32; n];
        for (i, layer) in self.layers.iter().enumerate() {
            if layer.is_group() || !self.is_effectively_visible(i) {
                continue;
            }
            layer.blend_onto_f32(&mut acc, width, height);
        }
        let mut out = vec![0u16; n];
        out.par_iter_mut().zip(acc.par_iter()).for_each(|(o, &v)| {
            *o = (v * 65535.0).round().clamp(0.0, 65535.0) as u16;
        });
        out
    }

    /// Any visible group folder that needs isolated compositing (opacity/blend).
    /// When false, the GPU can draw the real stack directly (fast path).
    pub fn has_effected_groups(&self) -> bool {
        self.layers.iter().enumerate().any(|(i, l)| {
            l.is_group() && self.is_effectively_visible(i) && Self::group_needs_isolation(l)
        })
    }

    /// Whether the layer `layer_id` is a direct child of a visible group that is
    /// composited in isolation (opacity < 100% / non-Normal blend / mask). Such a
    /// child is pre-flattened on the CPU into a single synthetic group layer for
    /// rendering (see `to_render_stack_region`), so a GPU-shader live preview keyed
    /// to the child's id never reaches the screen. Callers route those previews
    /// (Develop, Levels/Curves, …) through the CPU tile bake instead, which the
    /// group flatten reads. Single-level, matching the flatten's own parent check.
    pub fn layer_in_isolated_group(&self, layer_id: u32) -> bool {
        let Some(pid) = self
            .layers
            .iter()
            .find(|l| l.id == layer_id)
            .and_then(|l| l.parent_id)
        else {
            return false;
        };
        self.layers.iter().enumerate().any(|(i, g)| {
            g.id == pid
                && g.is_group()
                && self.is_effectively_visible(i)
                && Self::group_needs_isolation(g)
        })
    }

    /// Composite the direct children of the group at `group_idx` into a
    /// transparent buffer covering only the region `(rx,ry,rw,rh)` (each child's
    /// own blend mode applied). Single-level; nested children are Phase 3.
    fn flatten_group_subtree_region(
        &self,
        group_idx: usize,
        rx: u32,
        ry: u32,
        rw: u32,
        rh: u32,
    ) -> Vec<u8> {
        let gid = self.layers[group_idx].id;
        let Some(len) = (rw as u64)
            .checked_mul(rh as u64)
            .and_then(|n| n.checked_mul(4))
            .and_then(|n| usize::try_from(n).ok())
        else {
            return Vec::new();
        };
        let mut buf = vec![0u8; len];
        if rw == 0 || rh == 0 {
            return buf;
        }

        for layer in self.layers.iter() {
            if layer.parent_id == Some(gid) && !layer.is_group() && layer.visible {
                let mut shifted = layer.clone();
                shifted.offset.0 -= rx as i32;
                shifted.offset.1 -= ry as i32;
                shifted.blend_onto_region(&mut buf, rw, 0, 0, rw, rh);
            }
        }
        buf
    }

    fn region_to_synthetic_group_layer(
        group: &Layer,
        width: u32,
        height: u32,
        rx: u32,
        ry: u32,
        rw: u32,
        rh: u32,
        pixels: &[u8],
    ) -> Layer {
        let mut synth = Layer::new(group.id, &group.name, width, height);
        synth.tiles = TileMap::new(width, height);
        if rw > 0 && rh > 0 && !pixels.is_empty() {
            synth.tiles.write_region(rx, ry, rw, rh, pixels);
            synth.tiles.bump_all_revisions();
        }
        synth.opacity = group.opacity;
        synth.blend_mode = group.blend_mode;
        synth.mask = group.mask.clone();
        synth.offset = (0, 0);
        synth.parent_id = None;
        synth
    }

    /// Build the flat layer list the GPU compositor should draw so that group
    /// opacity/blend/mask show live, reusing the existing single-pass compositor:
    /// each effected group becomes ONE raster layer (subtree pre-flattened +
    /// group opacity/blend/mask), its children omitted; pass-through group headers
    /// are dropped (children render inline); hidden layers dropped; the hierarchy
    /// is flattened (parent_id cleared). Only the effected group's subtree is
    /// materialised, and only inside `(rx,ry,rw,rh)` — so a brush dab or a zoom
    /// rebuilds just the dirty/visible region instead of the whole canvas. Used
    /// only when `has_effected_groups()`; the common case keeps the real stack.
    /// Build a render-only stack that draws `backdrop` (a shared master / paper
    /// page) beneath `self` (a document page). Used to composite a master under a
    /// page without touching either editable stack — the result is throwaway,
    /// meant only to be composited or flattened, never edited or saved.
    ///
    /// - Backdrop layer ids, and their internal `parent_id` / `clip_parent_id`
    ///   references, are shifted above every id in `self`, so nothing collides.
    /// - `self`'s bottom `is_background` layer is hidden in the result so the
    ///   master shows as the page's paper (the master carries the shared
    ///   background); the layer is kept, just made invisible, so indices and
    ///   `active_idx` stay aligned with the page's real stack.
    /// - `active_idx` is offset by the backdrop length to keep pointing at the
    ///   same foreground layer.
    pub fn with_backdrop(&self, backdrop: &LayerStack) -> LayerStack {
        let offset = self
            .layers
            .iter()
            .map(|l| l.id)
            .max()
            .map_or(0, |m| m.saturating_add(1));
        let mut layers: Vec<Layer> = Vec::with_capacity(backdrop.layers.len() + self.layers.len());
        for layer in &backdrop.layers {
            let mut l = layer.clone();
            l.id = l.id.saturating_add(offset);
            l.parent_id = l.parent_id.map(|p| p.saturating_add(offset));
            l.clip_parent_id = l.clip_parent_id.map(|p| p.saturating_add(offset));
            layers.push(l);
        }
        for (i, layer) in self.layers.iter().enumerate() {
            let mut l = layer.clone();
            if i == 0 && l.is_background {
                // Master replaces the page's own paper.
                l.visible = false;
            }
            layers.push(l);
        }
        let next_id = layers
            .iter()
            .map(|l| l.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        LayerStack {
            active_idx: backdrop.layers.len() + self.active_idx,
            layers,
            next_id,
        }
    }

    /// Build a render-only stack with `overlay` directly above the Background
    /// and below editable foreground layers. Used by document-level PDF edits:
    /// a shared clear behaves like erasing the imported page, while text/images
    /// added afterward remain visible above it.
    pub fn with_overlay(&self, overlay: &LayerStack) -> LayerStack {
        let offset = self
            .layers
            .iter()
            .map(|layer| layer.id)
            .max()
            .map_or(0, |id| id.saturating_add(1));
        let insert_at = self
            .layers
            .iter()
            .position(|layer| layer.is_background)
            .map_or(0, |index| index + 1);
        let mut overlay_layers = Vec::with_capacity(overlay.layers.len());
        for layer in &overlay.layers {
            let mut layer = layer.clone();
            layer.id = layer.id.saturating_add(offset);
            layer.parent_id = layer.parent_id.map(|id| id.saturating_add(offset));
            layer.clip_parent_id = layer.clip_parent_id.map(|id| id.saturating_add(offset));
            overlay_layers.push(layer);
        }
        let overlay_len = overlay_layers.len();
        let mut layers = self.layers.clone();
        layers.splice(insert_at..insert_at, overlay_layers);
        let next_id = layers
            .iter()
            .map(|layer| layer.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        LayerStack {
            layers,
            active_idx: self.active_idx + usize::from(self.active_idx >= insert_at) * overlay_len,
            next_id,
        }
    }

    pub fn to_render_stack_region(
        &self,
        width: u32,
        height: u32,
        rx: u32,
        ry: u32,
        rw: u32,
        rh: u32,
    ) -> LayerStack {
        let mut layers: Vec<Layer> = Vec::with_capacity(self.layers.len());
        let rx = rx.min(width);
        let ry = ry.min(height);
        let rw = rw.min(width.saturating_sub(rx));
        let rh = rh.min(height.saturating_sub(ry));

        for (i, layer) in self.layers.iter().enumerate() {
            if layer.is_group() {
                if self.is_effectively_visible(i) && Self::group_needs_isolation(layer) {
                    let buf = self.flatten_group_subtree_region(i, rx, ry, rw, rh);
                    layers.push(Self::region_to_synthetic_group_layer(
                        layer, width, height, rx, ry, rw, rh, &buf,
                    ));
                }
                continue;
            }
            if let Some(pid) = layer.parent_id {
                if self
                    .layers
                    .iter()
                    .any(|g| g.id == pid && g.is_group() && Self::group_needs_isolation(g))
                {
                    continue;
                }
            }
            if self.is_effectively_visible(i) {
                let mut l = layer.clone();
                l.parent_id = None;
                layers.push(l);
            }
        }
        LayerStack {
            layers,
            active_idx: 0,
            next_id: self.next_id,
        }
    }

    pub fn flatten_region(
        &self,
        out: &mut [u8],
        width: u32,
        height: u32,
        rx: u32,
        ry: u32,
        rw: u32,
        rh: u32,
    ) {
        let end_y = (ry + rh).min(height);
        let end_x = (rx + rw).min(width);
        for y in ry..end_y {
            for x in rx..end_x {
                let di = ((y * width + x) * 4) as usize;
                if di + 3 < out.len() {
                    out[di] = 0;
                    out[di + 1] = 0;
                    out[di + 2] = 0;
                    out[di + 3] = 0;
                }
            }
        }
        for (i, layer) in self.layers.iter().enumerate() {
            if self.is_effectively_visible(i) {
                layer.blend_onto_region(out, width, rx, ry, rw, rh);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        curve_is_identity, identity_curve, levels_eval, AdjustmentType, BlendMode, Layer,
        LayerMask, LayerStack, LevelsParams,
    };

    #[test]
    fn with_backdrop_stacks_master_beneath_and_hides_page_paper() {
        // A page (its own white background + one content layer) over a master
        // (paper + a logo). The result must draw the master first, keep the page's
        // content on top, hide the page's own background, and never collide ids.
        let mut master = LayerStack::new(8, 8); // bg id 0
        master.add_layer(8, 8); // "logo"
        let mut page = LayerStack::new(8, 8); // bg id 0
        page.add_layer(8, 8); // "photo"
        page.active_idx = 1;

        let combined = page.with_backdrop(&master);
        assert_eq!(combined.layers.len(), 4, "master(2) + page(2)");
        // active_idx points at the same foreground layer, shifted by the backdrop.
        assert_eq!(combined.active_idx, master.layers.len() + 1);
        // All ids are distinct (master ids were shifted above the page's).
        let ids: std::collections::HashSet<u32> = combined.layers.iter().map(|l| l.id).collect();
        assert_eq!(ids.len(), 4, "no id collisions between master and page");
        // Bottom two are the master (in order); the page's background (index 2)
        // is hidden so the master shows as the paper.
        assert!(
            combined.layers[0].is_background,
            "master paper is the bottom"
        );
        assert!(
            !combined.layers[2].visible,
            "the page's own background is suppressed under a master"
        );
        assert!(combined.layers[3].visible, "the page content stays visible");
    }

    #[test]
    fn adjustment16_matches_8bit_at_endpoints() {
        // The 16-bit path must agree with the 8-bit path on exact 8-bit values
        // (no behaviour drift for the common case).
        let adj = AdjustmentType::Invert;
        for v in [0u8, 64, 128, 200, 255] {
            let (r8, _, _, _) = adj.apply_pixel(v, v, v, 255);
            let v16 = (v as u16) * 257;
            let (r16, _, _, a16) = adj.apply_pixel16(v16, v16, v16, 65535);
            assert_eq!(r16 >> 8, r8 as u16, "invert mismatch at {v}");
            assert_eq!(a16, 65535);
        }
    }

    #[test]
    fn levels_at_16bit_beats_8bit_banding() {
        // A steep Levels stretch over a smooth 16-bit ramp must keep far more than
        // 256 distinct output levels — the whole point of 16-bit editing.
        let mut channels = [LevelsParams::default(); 4];
        channels[0].in_black = 51; // 0.2
        channels[0].in_white = 204; // 0.8
        let adj = AdjustmentType::Levels { channels };
        let n = 2000u32;
        let lo = (0.2 * 65535.0) as u32;
        let hi = (0.8 * 65535.0) as u32;

        let mut out16 = std::collections::BTreeSet::new();
        let mut out8 = std::collections::BTreeSet::new();
        for i in 0..n {
            let v = (lo + (hi - lo) * i / (n - 1)) as u16;
            out16.insert(adj.apply_pixel16(v, v, v, 65535).0);
            out8.insert(
                adj.apply_pixel((v >> 8) as u8, (v >> 8) as u8, (v >> 8) as u8, 255)
                    .0,
            );
        }
        assert!(out8.len() <= 256, "8-bit can't exceed 256 levels");
        assert!(
            out16.len() > 1000,
            "16-bit levels should be finely stepped, got {}",
            out16.len()
        );
    }

    #[test]
    fn levels_per_channel_touches_only_that_channel() {
        let mut channels = [LevelsParams::default(); 4];
        channels[1].in_black = 64; // raise red's black point → red darkens
        let adj = AdjustmentType::Levels { channels };
        let (r, g, b, a) = adj.apply_pixel(128, 128, 128, 255);
        assert!(r < 128, "red must darken, got {r}");
        assert_eq!((g, b, a), (128, 128, 255), "green/blue/alpha untouched");
    }

    #[test]
    fn levels_master_composes_after_channel() {
        // channel red: in_black 64 → v=128 maps to (128-64)/(255-64) ≈ 0.335
        // master: out_white 128 → halves the result.
        let mut channels = [LevelsParams::default(); 4];
        channels[1].in_black = 64;
        channels[0].out_white = 128;
        let adj = AdjustmentType::Levels { channels };

        let ch_only = levels_eval(&channels[1], 128.0 / 255.0);
        let expect = levels_eval(&channels[0], ch_only);
        let (r, _, _, _) = adj.apply_pixel(128, 128, 128, 255);
        assert_eq!(r, (expect * 255.0).round() as u8, "master(channel(v))");
    }

    #[test]
    fn curves_per_channel_touches_only_that_channel() {
        let mut channels: [Vec<(f32, f32)>; 4] = std::array::from_fn(|_| identity_curve());
        channels[3] = vec![(0.0, 0.0), (0.5, 0.8), (1.0, 1.0)]; // lift blue
        let adj = AdjustmentType::Curves { channels };
        let (r, g, b, a) = adj.apply_pixel(128, 128, 128, 200);
        assert_eq!((r, g, a), (128, 128, 200), "red/green/alpha untouched");
        assert!(b > 180, "blue must lift strongly, got {b}");
    }

    #[test]
    fn curve_identity_detection() {
        assert!(curve_is_identity(&[]));
        assert!(
            curve_is_identity(&[(0.3, 0.7)]),
            "n<2 is identity by convention"
        );
        assert!(curve_is_identity(&identity_curve()));
        assert!(curve_is_identity(&[(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)]));
        assert!(!curve_is_identity(&[(0.0, 0.0), (0.5, 0.4), (1.0, 1.0)]));
    }

    #[test]
    fn levels_identity_eval_is_exact_passthrough() {
        let p = LevelsParams::default();
        for i in 0..=255u32 {
            let v = i as f32 / 255.0;
            assert_eq!(levels_eval(&p, v), v);
        }
    }

    #[test]
    fn flatten_without_background_preserves_transparency() {
        let stack = LayerStack {
            layers: vec![Layer::new(0, "Layer 1", 2, 2)],
            active_idx: 0,
            next_id: 1,
        };
        assert_eq!(stack.flatten(2, 2), vec![0u8; 16]);
    }

    #[test]
    fn flatten_with_background_stays_opaque_white() {
        let stack = LayerStack::new(1, 1);
        assert_eq!(stack.flatten(1, 1), vec![255u8; 4]);
    }

    #[test]
    fn unlock_background_layer_makes_layer_zero() {
        let mut stack = LayerStack::new(2, 2);
        stack.layers[0].locked = true;

        assert!(stack.unlock_background_layer(0));

        assert_eq!(stack.layers[0].name, "Layer 0");
        assert!(!stack.layers[0].is_background);
        assert!(!stack.layers[0].locked);
        assert!(stack.layers[0].selected);
        assert_eq!(stack.active_idx, 0);
    }

    #[test]
    fn update_tiles_region_allows_locked_background_only() {
        let mut bg = Layer::from_rgba(0, "Background", vec![255, 255, 255, 255], 1, 1);
        assert!(bg.locked);
        assert!(bg.is_background);
        bg.update_tiles_region(0, 0, 1, 1, &[10, 20, 30, 255]);
        assert_eq!(bg.tiles.extract_region(0, 0, 1, 1), vec![10, 20, 30, 255]);

        let mut layer = Layer::from_rgba(1, "Layer 1", vec![255, 255, 255, 255], 1, 1);
        layer.locked = true;
        layer.update_tiles_region(0, 0, 1, 1, &[10, 20, 30, 255]);
        assert_eq!(
            layer.tiles.extract_region(0, 0, 1, 1),
            vec![255, 255, 255, 255]
        );
    }

    #[test]
    fn add_layer_names_use_visible_sequence_not_internal_id() {
        let mut stack = LayerStack::new(2, 2);
        stack.set_next_id(42);

        let idx = stack.add_layer(2, 2);

        assert_eq!(stack.layers[idx].id, 42);
        assert_eq!(stack.layers[idx].name, "Layer 1");
    }

    #[test]
    fn add_layer_inserts_directly_above_active_layer() {
        let mut stack = LayerStack::new(2, 2);
        let first = stack.add_layer(2, 2);
        stack.active_idx = 0;

        let second = stack.add_layer(2, 2);

        assert_eq!(second, 1);
        assert_eq!(stack.layers[second].name, "Layer 2");
        assert_eq!(stack.layers[first + 1].name, "Layer 1");
    }

    #[test]
    fn add_layer_reuses_lowest_free_layer_number() {
        let mut stack = LayerStack::new(2, 2);
        let first = stack.add_layer(2, 2);
        stack.add_layer(2, 2);
        stack.remove_layer(first);

        let idx = stack.add_layer(2, 2);

        assert_eq!(stack.layers[idx].name, "Layer 1");
    }

    #[test]
    fn duplicate_layer_names_are_unique() {
        let mut stack = LayerStack::new(2, 2);
        let idx = stack.add_layer(2, 2);

        stack.duplicate_layer(idx);
        stack.duplicate_layer(idx);
        let names: Vec<&str> = stack.layers.iter().map(|l| l.name.as_str()).collect();

        assert!(names.contains(&"Layer 1 copy"));
        assert!(names.contains(&"Layer 1 copy 2"));
    }

    #[test]
    fn merge_down_bakes_bottom_mask_instead_of_keeping_it() {
        let mut stack = LayerStack::new(2, 2);
        let bottom = stack.add_layer(2, 2);
        stack.layers[bottom].tiles.set_pixel(0, 0, 255, 0, 0, 255);
        stack.layers[bottom].mask = Some(LayerMask::new_black(2, 2));
        let top = stack.add_layer(2, 2);
        stack.layers[top].tiles.set_pixel(1, 1, 0, 0, 255, 255);

        assert!(stack.merge_down(top));

        let merged = &stack.layers[bottom];
        assert!(
            merged.mask.is_none(),
            "old bottom mask must not cut the merged content"
        );
        // Bottom pixel was hidden by its black mask -> baked to transparent.
        assert_eq!(merged.tiles.get_pixel(0, 0).3, 0);
        // Top pixel had no mask -> survives the merge.
        assert_eq!(merged.tiles.get_pixel(1, 1), (0, 0, 255, 255));
    }

    #[test]
    fn merge_down_drops_a_hidden_bottom_and_stays_visible() {
        let mut stack = LayerStack::new(2, 2);
        let bottom = stack.add_layer(2, 2);
        stack.layers[bottom].tiles.set_pixel(0, 0, 255, 0, 0, 255); // red
        stack.layers[bottom].visible = false; // eye off on the lower layer
        let top = stack.add_layer(2, 2);
        stack.layers[top].tiles.set_pixel(1, 1, 0, 0, 255, 255); // blue

        assert!(stack.merge_down(top));

        let merged = &stack.layers[bottom];
        assert!(
            merged.visible,
            "a visible top makes the merged layer visible"
        );
        // The hidden bottom contributes nothing.
        assert_eq!(
            merged.tiles.get_pixel(0, 0).3,
            0,
            "hidden bottom pixel dropped"
        );
        // The visible top survives.
        assert_eq!(merged.tiles.get_pixel(1, 1), (0, 0, 255, 255));
    }

    #[test]
    fn merge_selected_keeps_visible_top_over_hidden_lower() {
        let mut stack = LayerStack::new(2, 2);
        let lower = stack.add_layer(2, 2);
        stack.layers[lower].tiles.set_pixel(0, 0, 255, 0, 0, 255); // red
        stack.layers[lower].visible = false;
        stack.layers[lower].selected = true;
        let upper = stack.add_layer(2, 2);
        stack.layers[upper].tiles.set_pixel(1, 1, 0, 0, 255, 255); // blue
        stack.layers[upper].selected = true;

        assert!(stack.merge_selected(2, 2));

        // Result lands on the lower slot; it must be visible and hold only the
        // visible top's pixel, not the hidden lower's.
        let merged = &stack.layers[lower];
        assert!(merged.visible, "merged result must be visible");
        assert_eq!(
            merged.tiles.get_pixel(0, 0).3,
            0,
            "hidden lower pixel dropped"
        );
        assert_eq!(merged.tiles.get_pixel(1, 1), (0, 0, 255, 255));
    }

    #[test]
    fn merge_selected_bakes_a_layer_mask() {
        let mut stack = LayerStack::new(2, 2);
        let lower = stack.add_layer(2, 2);
        for (x, y) in [(0u32, 0u32), (1, 0), (0, 1), (1, 1)] {
            stack.layers[lower].tiles.set_pixel(x, y, 0, 255, 0, 255); // green
        }
        stack.layers[lower].selected = true;
        let upper = stack.add_layer(2, 2);
        stack.layers[upper].tiles.set_pixel(0, 0, 0, 0, 255, 255); // blue
        stack.layers[upper].mask = Some(LayerMask::new_black(2, 2)); // hides the top
        stack.layers[upper].selected = true;

        assert!(stack.merge_selected(2, 2));

        let merged = &stack.layers[lower];
        assert!(merged.mask.is_none(), "mask is baked in, not kept");
        // The top was fully masked out, so its blue must not paint over the green.
        assert_eq!(
            merged.tiles.get_pixel(0, 0),
            (0, 255, 0, 255),
            "masked-out top did not paint"
        );
    }

    #[test]
    fn mask_sample_clamps_to_edge() {
        let mut mask = LayerMask::new_white(2, 2);
        mask.tiles.set_pixel(1, 1, 40, 40, 40, 255);

        assert_eq!(mask.sample(5, 5), mask.sample(1, 1));
        assert_eq!(mask.sample(0, 9), mask.sample(0, 1));
    }

    #[test]
    fn mask_resize_keeps_content_and_extends_white() {
        let mut mask = LayerMask::new_black(2, 2);
        mask.resize_to(4, 4);

        assert_eq!((mask.width, mask.height), (4, 4));
        assert_eq!(mask.sample(0, 0), 0.0, "old area stays black");
        assert_eq!(mask.sample(3, 3), 1.0, "new area is revealed");
    }

    #[test]
    fn curves_identity_is_noop() {
        let adj = AdjustmentType::default_curves();
        for &v in &[0u8, 1, 64, 128, 200, 254, 255] {
            let (r, g, b, a) = adj.apply_pixel(v, v, v, 200);
            assert_eq!((r, g, b, a), (v, v, v, 200), "identity curve changed {v}");
        }
    }

    #[test]
    fn curves_darkening_midpoint_pulls_down() {
        let mut channels: [Vec<(f32, f32)>; 4] = std::array::from_fn(|_| identity_curve());
        channels[0] = vec![(0.0, 0.0), (0.5, 0.25), (1.0, 1.0)];
        let adj = AdjustmentType::Curves { channels };
        let (mid, _, _, _) = adj.apply_pixel(128, 128, 128, 255);
        assert!(mid < 110, "expected mid-grey to darken, got {mid}");
        assert_eq!(adj.apply_pixel(0, 0, 0, 255).0, 0);
        assert_eq!(adj.apply_pixel(255, 255, 255, 255).0, 255);
    }

    #[test]
    fn curves_degenerate_single_point_is_identity() {
        let mut channels: [Vec<(f32, f32)>; 4] = std::array::from_fn(|_| identity_curve());
        channels[0] = vec![(0.3, 0.7)];
        let adj = AdjustmentType::Curves { channels };
        assert_eq!(adj.apply_pixel(128, 64, 200, 255), (128, 64, 200, 255));
    }

    #[test]
    fn hue_saturation_boost_protects_near_neutral_pixels() {
        let adj = AdjustmentType::HueSaturation {
            hue: 0.0,
            saturation: 40.0,
            lightness: 0.0,
        };

        let neutral_in = (178u8, 181u8, 184u8, 255u8);
        let neutral_out = adj.apply_pixel(neutral_in.0, neutral_in.1, neutral_in.2, neutral_in.3);
        let neutral_delta = max_rgb_delta(neutral_in, neutral_out);

        let red_in = (204u8, 82u8, 50u8, 255u8);
        let red_out = adj.apply_pixel(red_in.0, red_in.1, red_in.2, red_in.3);
        let red_chroma_gain = rgb_chroma_u8(red_out) as i16 - rgb_chroma_u8(red_in) as i16;

        assert!(
            neutral_delta <= 3,
            "near-neutral pixel shifted too much: {neutral_in:?} -> {neutral_out:?}"
        );
        assert!(
            red_chroma_gain > 35,
            "saturated red/orange should gain chroma, got {red_in:?} -> {red_out:?}"
        );
    }

    fn max_rgb_delta(a: (u8, u8, u8, u8), b: (u8, u8, u8, u8)) -> u8 {
        let dr = a.0.abs_diff(b.0);
        let dg = a.1.abs_diff(b.1);
        let db = a.2.abs_diff(b.2);
        dr.max(dg).max(db)
    }

    fn rgb_chroma_u8(c: (u8, u8, u8, u8)) -> u8 {
        c.0.max(c.1).max(c.2) - c.0.min(c.1).min(c.2)
    }

    fn stack_of(n: u32) -> LayerStack {
        let layers: Vec<Layer> = (0..n)
            .map(|i| Layer::new(i, &format!("L{i}"), 2, 2))
            .collect();
        LayerStack {
            layers,
            active_idx: 0,
            next_id: n,
        }
    }

    #[test]
    fn render_overlay_sits_above_background_below_page_content() {
        let mut page = stack_of(2);
        page.layers[0].is_background = true;
        page.active_idx = 1;
        let overlay = stack_of(1);
        let combined = page.with_overlay(&overlay);
        assert_eq!(combined.layers.len(), 3);
        assert_eq!(combined.active_idx, 2);
        assert_eq!(combined.layers[0].name, "L0");
        assert_eq!(combined.layers[1].name, "L0");
        assert_eq!(combined.layers[2].name, "L1");
        let ids: std::collections::HashSet<_> =
            combined.layers.iter().map(|layer| layer.id).collect();
        assert_eq!(ids.len(), combined.layers.len());
    }

    #[test]
    fn foreground_text_like_layer_is_not_hidden_by_virtual_clear() {
        let mut page = LayerStack::new(1, 1);
        page.layers
            .push(Layer::from_rgba(1, "Text", vec![220, 20, 30, 255], 1, 1));
        page.active_idx = 1;
        let mut clear = LayerStack::new(1, 1);
        clear.layers = vec![Layer::from_rgba(
            0,
            "Xóa vùng hàng loạt PDF",
            vec![255, 255, 255, 255],
            1,
            1,
        )];
        let combined = page.with_overlay(&clear);
        assert_eq!(combined.flatten(1, 1), vec![220, 20, 30, 255]);
        assert_eq!(combined.active_idx, 2);
    }

    #[test]
    fn group_selected_wraps_and_reparents() {
        let mut s = stack_of(4);
        s.layers[1].selected = true;
        s.layers[2].selected = true;

        let header_idx = s.create_group_from_selected(2, 2).expect("should group");
        assert!(s.layers[header_idx].is_group());
        assert!(
            !s.layers[header_idx].expanded,
            "new groups should start collapsed"
        );
        let gid = s.layers[header_idx].id;

        let members = s.group_member_range(header_idx);
        assert_eq!(members.len(), 2, "group should hold 2 members");
        for i in members {
            assert_eq!(s.layers[i].parent_id, Some(gid));
            assert_eq!(s.depth_of(i), 1);
        }
        assert_eq!(s.layers[header_idx].parent_id, None);
        assert_eq!(s.active_idx, header_idx);
        assert_eq!(s.children_of(gid).len(), 2);
    }

    #[test]
    fn layer_in_isolated_group_tracks_group_effects() {
        let mut s = stack_of(3);
        s.layers[1].selected = true;
        s.layers[2].selected = true;
        let g = s.create_group_from_selected(2, 2).unwrap();
        let child_id = 1; // L1, now a member of the group

        // Plain group (Normal / 100% / no mask) needs no isolation → the child
        // renders inline, so the GPU preview reaches it.
        assert!(!s.layer_in_isolated_group(child_id));

        // Reduced opacity, a non-Normal blend, or a mask each isolate the group,
        // which pre-flattens the child on the CPU (the preview must bake instead).
        s.layers[g].opacity = 0.5;
        assert!(s.layer_in_isolated_group(child_id));
        s.layers[g].opacity = 1.0;
        s.layers[g].blend_mode = BlendMode::Multiply;
        assert!(s.layer_in_isolated_group(child_id));
        s.layers[g].blend_mode = BlendMode::Normal;
        assert!(!s.layer_in_isolated_group(child_id));

        // A hidden isolated group drops out of the render entirely, so its child
        // isn't on a live-preview path either.
        s.layers[g].opacity = 0.5;
        s.layers[g].visible = false;
        assert!(!s.layer_in_isolated_group(child_id));

        // A top-level layer (no parent) is never "in a group".
        let top_id = s.layers[g].id;
        assert!(!s.layer_in_isolated_group(top_id));
    }

    #[test]
    fn expand_collapsed_ancestors_reveals_nested_layer() {
        let mut s = stack_of(3);
        s.layers[1].selected = true;
        s.layers[2].selected = true;
        let g = s.create_group_from_selected(2, 2).unwrap();
        let child_idx = s.group_member_range(g).start;

        // New groups start collapsed, hiding their members.
        assert!(!s.layers[g].expanded);
        assert!(s.is_collapsed_hidden(child_idx));

        // Expanding reveals the child and reports the change; then it's idempotent.
        assert!(s.expand_collapsed_ancestors(child_idx));
        assert!(s.layers[g].expanded);
        assert!(!s.is_collapsed_hidden(child_idx));
        assert!(!s.expand_collapsed_ancestors(child_idx));
    }

    #[test]
    fn ungroup_dissolves_and_reparents_to_outer() {
        let mut s = stack_of(3);
        s.layers[1].selected = true;
        let header_idx = s.create_group_from_selected(2, 2).unwrap();
        let n_before = s.layers.len();

        assert!(s.ungroup(header_idx), "ungroup a real group");
        assert_eq!(s.layers.len(), n_before - 1, "header removed");
        assert!(s.layers.iter().all(|l| l.parent_id.is_none()));
        assert!(s.layers.iter().all(|l| !l.is_group()));
    }

    #[test]
    fn group_member_range_empty_for_non_group() {
        let s = stack_of(2);
        assert_eq!(s.group_member_range(0), 0..0);
    }

    #[test]
    fn paste_layers_carries_group_and_children() {
        // Source: a folder with children A and B (the "copy group" case).
        let mut src = LayerStack::new(2, 2);
        src.layers.push(Layer::new(1, "A", 2, 2));
        src.layers.push(Layer::new(2, "B", 2, 2));
        src.next_id = 3;
        src.layers[1].selected = true;
        src.layers[2].selected = true;
        let g = src.create_group_from_selected(2, 2).unwrap();
        // Clipboard block = the folder's members + header (bottom→top).
        let block: Vec<Layer> = src.layers[src.group_member_range(g).start..=g]
            .iter()
            .cloned()
            .collect();
        assert_eq!(block.len(), 3, "header + 2 children");

        // Paste into a fresh document — folder + children must survive with remapped ids.
        let mut dst = LayerStack::new(2, 2);
        let before = dst.layers.len();
        dst.paste_layers(&block);
        assert_eq!(dst.layers.len(), before + 3, "folder + 2 children pasted");

        let header = dst
            .layers
            .iter()
            .find(|l| l.is_group())
            .expect("group folder pasted, not lost as an empty folder");
        let new_gid = header.id;
        let children: Vec<&Layer> = dst
            .layers
            .iter()
            .filter(|l| l.parent_id == Some(new_gid))
            .collect();
        assert_eq!(
            children.len(),
            2,
            "children re-parented to the pasted folder"
        );
        assert!(children.iter().any(|l| l.name == "A"));
        assert!(children.iter().any(|l| l.name == "B"));
        // No id collisions after the remap.
        let ids: Vec<u32> = dst.layers.iter().map(|l| l.id).collect();
        let uniq: std::collections::HashSet<u32> = ids.iter().copied().collect();
        assert_eq!(ids.len(), uniq.len(), "pasted ids are unique in the doc");
    }

    /// [bg(top), A(top), B(child of a folder), G(folder header)].
    fn group_with_outsider() -> (LayerStack, u32) {
        let mut s = LayerStack::new(2, 2);
        s.layers.push(Layer::new(1, "A", 2, 2));
        s.layers.push(Layer::new(2, "B", 2, 2));
        s.next_id = 3;
        s.layers[2].selected = true;
        let g = s.create_group_from_selected(2, 2).unwrap();
        let gid = s.layers[g].id;
        (s, gid)
    }

    #[test]
    fn drag_into_group_sets_parent() {
        let (mut s, gid) = group_with_outsider();
        let header = s.layers.iter().position(|l| l.is_group()).unwrap();
        let a = s.layers.iter().position(|l| l.name == "A").unwrap();
        assert!(s.drag_layer_to(a, header));
        let a2 = s.layers.iter().position(|l| l.name == "A").unwrap();
        assert_eq!(
            s.layers[a2].parent_id,
            Some(gid),
            "A should be in the folder"
        );
    }

    #[test]
    fn drag_out_of_group_clears_parent() {
        let (mut s, _gid) = group_with_outsider();
        let b = s.layers.iter().position(|l| l.name == "B").unwrap();
        assert!(s.drag_layer_to(b, 1));
        let b2 = s.layers.iter().position(|l| l.name == "B").unwrap();
        assert_eq!(s.layers[b2].parent_id, None, "B should be top-level");
    }

    #[test]
    fn duplicate_group_copies_children_with_new_ids() {
        let (mut s, gid) = group_with_outsider();
        let header = s.layers.iter().position(|l| l.is_group()).unwrap();
        let before = s.layers.len();
        let new_header = s.duplicate_group(header);
        assert_eq!(s.layers.len(), before + 2, "header + 1 child duplicated");
        let new_gid = s.layers[new_header].id;
        assert_ne!(new_gid, gid, "new folder has a fresh id");
        assert_eq!(s.children_of(new_gid).len(), 1);
        assert_eq!(s.children_of(gid).len(), 1, "original folder untouched");
    }

    #[test]
    fn merge_group_bakes_opacity_into_one_layer() {
        let mut s = LayerStack::new(1, 1);
        s.layers
            .push(Layer::from_rgba(1, "red", vec![255, 0, 0, 255], 1, 1));
        s.next_id = 2;
        s.layers[1].selected = true;
        let header = s.create_group_from_selected(1, 1).unwrap();
        s.layers[header].opacity = 0.5;
        let before = s.layers.len();
        assert!(s.merge_group(header, 1, 1));
        assert_eq!(s.layers.len(), before - 1, "header + child → one layer");
        assert!(!s.layers.iter().any(|l| l.is_group()), "folder gone");
        let merged = &s.layers[1];
        let px = merged.tiles.flatten();
        assert_eq!(px[0], 255);
        assert!((px[3] as i32 - 127).abs() <= 2, "alpha was {}", px[3]);
    }

    #[test]
    fn stamp_visible_adds_top_layer_keeps_originals() {
        let mut s = LayerStack::new(1, 1);
        s.layers
            .push(Layer::from_rgba(1, "a", vec![0, 0, 0, 255], 1, 1));
        s.layers
            .push(Layer::from_rgba(2, "b", vec![0, 0, 0, 255], 1, 1));
        s.next_id = 3;
        s.layers[2].visible = false;
        let before = s.layers.len();
        assert!(s.merge_visible(1, 1));
        assert_eq!(s.layers.len(), before + 1, "one new layer, originals kept");
        assert_eq!(s.layers.last().unwrap().name, "Merged", "stamp sits on top");
        assert!(s.layers.iter().any(|l| l.name == "a"));
        assert!(s.layers.iter().any(|l| l.name == "b"));
    }

    #[test]
    fn merge_group_large_canvas_bakes_opacity_chunked() {
        // > 25M px forces the tile-native chunked merge_group path (no canvas-sized
        // buffer); the group's opacity must still bake into the merged layer's alpha.
        let (w, h) = (5001u32, 5001u32);
        assert!(!crate::core::canvas::Canvas::fits_flat_buffer(w, h));
        let mut s = LayerStack::new(w, h);
        let mut child = Layer::new(1, "red", w, h);
        child.tiles.set_pixel(300, 300, 255, 0, 0, 255);
        s.layers.push(child);
        s.next_id = 2;
        s.layers[1].selected = true;
        let header = s.create_group_from_selected(w, h).unwrap();
        let gid = s.layers[header].id;
        s.layers[header].opacity = 0.5;

        let before = s.layers.len();
        assert!(s.merge_group(header, w, h));
        assert_eq!(s.layers.len(), before - 1, "header + child → one layer");
        assert!(!s.layers.iter().any(|l| l.is_group()), "folder gone");
        let merged = s.layers.iter().find(|l| l.id == gid).unwrap();
        let (r, _g, _b, a) = merged.tiles.get_pixel(300, 300);
        assert_eq!(r, 255);
        assert!((a as i32 - 127).abs() <= 2, "opacity baked, alpha={a}");
        assert_eq!(
            merged.tiles.get_pixel(4000, 4000).3,
            0,
            "empty subtree region stays transparent"
        );
    }

    #[test]
    fn merge_visible_large_canvas_composites_isolated_group_chunked() {
        // > 25M px forces the chunked flatten_into_tiles path; an isolated (50%)
        // group over the white background must composite correctly per chunk.
        let (w, h) = (5001u32, 5001u32);
        assert!(!crate::core::canvas::Canvas::fits_flat_buffer(w, h));
        let mut s = LayerStack::new(w, h); // white Background at index 0
        let mut child = Layer::new(1, "green", w, h);
        child.tiles.set_pixel(300, 300, 0, 255, 0, 255);
        s.layers.push(child);
        s.next_id = 2;
        s.layers[1].selected = true;
        let header = s.create_group_from_selected(w, h).unwrap();
        s.layers[header].opacity = 0.5;

        let before = s.layers.len();
        assert!(s.merge_visible(w, h));
        assert_eq!(s.layers.len(), before + 1, "merged added, originals kept");
        let merged = s.layers.last().unwrap();
        assert_eq!(merged.name, "Merged");
        // green (0,255,0) at 50% group opacity over white → ~(128,255,128), opaque.
        let (r, g, b, a) = merged.tiles.get_pixel(300, 300);
        assert_eq!(a, 255, "over opaque background → opaque");
        assert!(
            g > 240 && (100..=160).contains(&r) && (100..=160).contains(&b),
            "green over white at 50% group opacity: ({r},{g},{b})"
        );
        assert_eq!(
            merged.tiles.get_pixel(4000, 4000),
            (255, 255, 255, 255),
            "background elsewhere"
        );
    }

    #[test]
    fn drag_group_header_moves_whole_block() {
        let (mut s, gid) = group_with_outsider();
        let header = s.layers.iter().position(|l| l.is_group()).unwrap();
        assert!(s.drag_layer_to(header, 1));
        let new_header = s.layers.iter().position(|l| l.is_group()).unwrap();
        let b = s.layers.iter().position(|l| l.name == "B").unwrap();
        assert_eq!(s.layers[b].parent_id, Some(gid), "B stays a child");
        assert_eq!(new_header, b + 1, "header sits directly above its child");
        assert_eq!(s.layers[new_header].parent_id, None, "folder is top-level");
    }

    /// White background + one opaque red child grouped into a folder. Returns the
    /// flattened top-left pixel for a given group opacity.
    fn flatten_red_group_at_opacity(opacity: f32) -> [u8; 4] {
        let mut s = LayerStack::new(1, 1);
        s.layers
            .push(Layer::from_rgba(1, "red", vec![255, 0, 0, 255], 1, 1));
        s.next_id = 2;
        s.layers[1].selected = true;
        let hidx = s.create_group_from_selected(1, 1).expect("group");
        s.layers[hidx].opacity = opacity;
        let out = s.flatten(1, 1);
        [out[0], out[1], out[2], out[3]]
    }

    #[test]
    fn group_opacity_blends_subtree() {
        let px = flatten_red_group_at_opacity(0.5);
        assert_eq!(px[0], 255);
        assert!((px[1] as i32 - 127).abs() <= 2, "G was {}", px[1]);
        assert!((px[2] as i32 - 127).abs() <= 2, "B was {}", px[2]);
        assert_eq!(px[3], 255);
    }

    #[test]
    fn passthrough_group_equals_ungrouped() {
        let grouped = flatten_red_group_at_opacity(1.0);
        let mut plain = LayerStack::new(1, 1);
        plain
            .layers
            .push(Layer::from_rgba(1, "red", vec![255, 0, 0, 255], 1, 1));
        let ungrouped = plain.flatten(1, 1);
        assert_eq!(
            grouped,
            [ungrouped[0], ungrouped[1], ungrouped[2], ungrouped[3]]
        );
        assert_eq!(grouped, [255, 0, 0, 255]);
    }

    #[test]
    fn flatten_band_matches_flatten_with_groups_and_masked_adjustment() {
        // A stack exercising every chunked-composite branch: offset raster layer
        // with alpha, isolated group (opacity < 1), and a masked adjustment.
        // Band assembly must match the single-pass flatten to within 1 LSB (the
        // chunk path bakes group opacity into u8 alpha; flatten applies it in
        // f32 at blend time) — in particular the adjustment's mask must be
        // sampled at CANVAS coords under the offset-shift trick (any band with
        // y0 > 0 regressed by far more than 1 LSB before the coordinate fix).
        let (w, h) = (40u32, 33u32);
        let mut s = LayerStack::new(w, h);

        let (lw, lh) = (21u32, 17u32);
        let mut px = Vec::with_capacity((lw * lh * 4) as usize);
        for y in 0..lh {
            for x in 0..lw {
                px.extend_from_slice(&[
                    (x * 12) as u8,
                    (y * 14) as u8,
                    200,
                    if (x + y) % 3 == 0 { 128 } else { 255 },
                ]);
            }
        }
        let mut moved = Layer::from_rgba(1, "moved", px, lw, lh);
        moved.offset = (5, 9);
        s.layers.push(moved);

        let mut red = Layer::from_rgba(2, "red", vec![255u8, 0, 0, 255].repeat(64), 8, 8);
        red.offset = (20, 3);
        s.layers.push(red);
        let red_idx = s.layers.len() - 1;
        s.next_id = 3;
        for l in s.layers.iter_mut() {
            l.selected = false;
        }
        s.layers[red_idx].selected = true;
        let gidx = s.create_group_from_selected(w, h).unwrap();
        s.layers[gidx].opacity = 0.5;

        // Vertical gradient mask: sampling it at chunk-local Y instead of
        // canvas Y shifts the adjustment strength visibly in every band.
        let mut adj = Layer::new_adjustment(90, super::AdjustmentType::Invert, w, h);
        let mut mpx = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for _x in 0..w {
                let v = (y * 255 / (h - 1)) as u8;
                mpx.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let mut mask = LayerMask::new_white(w, h);
        mask.tiles = crate::core::tile::TileMap::from_rgba(&mpx, w, h);
        adj.mask = Some(mask);
        s.layers.push(adj);

        let flat = s.flatten(w, h);
        let mut banded = vec![0u8; flat.len()];
        let band_h = 7u32; // not a divisor of h → bands straddle uneven splits
        let mut y = 0u32;
        while y < h {
            let bh = band_h.min(h - y);
            let band = s.flatten_band(w, h, y, bh);
            let start = ((y * w) * 4) as usize;
            banded[start..start + band.len()].copy_from_slice(&band);
            y += bh;
        }
        let max_diff = flat
            .iter()
            .zip(&banded)
            .map(|(a, b)| (*a as i32 - *b as i32).abs())
            .max()
            .unwrap_or(0);
        assert!(
            max_diff <= 1,
            "band assembly must match single-pass flatten within quantization (max diff {max_diff})"
        );
    }

    #[test]
    fn group_black_mask_hides_subtree() {
        let mut s = LayerStack::new(1, 1);
        s.layers
            .push(Layer::from_rgba(1, "red", vec![255, 0, 0, 255], 1, 1));
        s.next_id = 2;
        s.layers[1].selected = true;
        let hidx = s.create_group_from_selected(1, 1).unwrap();
        s.layers[hidx].mask = Some(LayerMask::new_black(1, 1));
        let out = s.flatten(1, 1);
        assert_eq!([out[0], out[1], out[2], out[3]], [255, 255, 255, 255]);
    }

    #[test]
    fn group_white_mask_shows_subtree() {
        let mut s = LayerStack::new(1, 1);
        s.layers
            .push(Layer::from_rgba(1, "red", vec![255, 0, 0, 255], 1, 1));
        s.next_id = 2;
        s.layers[1].selected = true;
        let hidx = s.create_group_from_selected(1, 1).unwrap();
        s.layers[hidx].mask = Some(LayerMask::new_white(1, 1));
        let out = s.flatten(1, 1);
        assert_eq!([out[0], out[1], out[2], out[3]], [255, 0, 0, 255]);
    }

    #[test]
    fn apply_group_black_mask_bakes_clip_into_children() {
        let mut s = LayerStack::new(1, 1);
        s.layers
            .push(Layer::from_rgba(1, "red", vec![255, 0, 0, 255], 1, 1));
        s.next_id = 2;
        s.layers[1].selected = true;
        let hidx = s.create_group_from_selected(1, 1).unwrap();
        s.layers[hidx].mask = Some(LayerMask::new_black(1, 1));
        let before = s.flatten(1, 1);
        assert_eq!(
            [before[0], before[1], before[2], before[3]],
            [255, 255, 255, 255]
        );

        s.apply_layer_mask(hidx);
        assert!(
            s.layers[hidx].mask.is_none(),
            "group mask removed after apply"
        );
        let after = s.flatten(1, 1);
        assert_eq!(
            [after[0], after[1], after[2], after[3]],
            [255, 255, 255, 255]
        );
    }

    #[test]
    fn apply_group_white_mask_keeps_subtree() {
        let mut s = LayerStack::new(1, 1);
        s.layers
            .push(Layer::from_rgba(1, "red", vec![255, 0, 0, 255], 1, 1));
        s.next_id = 2;
        s.layers[1].selected = true;
        let hidx = s.create_group_from_selected(1, 1).unwrap();
        s.layers[hidx].mask = Some(LayerMask::new_white(1, 1));
        s.apply_layer_mask(hidx);
        assert!(s.layers[hidx].mask.is_none());
        let out = s.flatten(1, 1);
        assert_eq!([out[0], out[1], out[2], out[3]], [255, 0, 0, 255]);
    }

    #[test]
    fn hidden_group_renders_nothing_extra() {
        let mut s = LayerStack::new(1, 1);
        s.layers
            .push(Layer::from_rgba(1, "red", vec![255, 0, 0, 255], 1, 1));
        s.next_id = 2;
        s.layers[1].selected = true;
        let hidx = s.create_group_from_selected(1, 1).unwrap();
        s.layers[hidx].opacity = 0.5;
        s.layers[hidx].visible = false;
        let out = s.flatten(1, 1);
        assert_eq!([out[0], out[1], out[2], out[3]], [255, 255, 255, 255]);
    }
}

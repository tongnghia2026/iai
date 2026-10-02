//! "Màu studio": finished colour looks for portraits (Chỉnh chân dung). Each
//! look is a fixed grade — white balance and exposure in linear light, then
//! a skin-tone tweak, saturation, a tone curve and split toning — baked into
//! a 3D LUT. Applied on its own layer, its strength is that layer's opacity.

use rayon::prelude::*;

use super::geometry::Region;
use crate::core::develop::srgb_to_linear;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StudioLook {
    None,
    Clear,
    Rosy,
    Fair,
    Warm,
    Natural,
    SoftFilm,
}

/// One look's grade at full strength.
struct Grade {
    /// Exposure in stops.
    ev: f32,
    /// −1 cool … 1 warm, and −1 green … 1 magenta.
    temp: f32,
    tint: f32,
    /// S-curve strength (negative flattens) and black lift (matte).
    contrast: f32,
    lift: f32,
    saturation: f32,
    /// Extra saturation for muted colours only.
    vibrance: f32,
    /// Skin tones: hue shift in degrees (negative turns pinker), saturation
    /// factor and lightening toward white.
    skin_hue: f32,
    skin_saturation: f32,
    skin_light: f32,
    /// Colour added to the shadows / highlights.
    shadows: [f32; 3],
    highlights: [f32; 3],
}

impl StudioLook {
    pub const ALL: [StudioLook; 7] = [
        StudioLook::None,
        StudioLook::Clear,
        StudioLook::Rosy,
        StudioLook::Fair,
        StudioLook::Warm,
        StudioLook::Natural,
        StudioLook::SoftFilm,
    ];

    pub fn from_index(index: u8) -> Self {
        Self::ALL
            .get(index as usize)
            .copied()
            .unwrap_or(StudioLook::None)
    }

    pub fn index(self) -> u8 {
        Self::ALL.iter().position(|&l| l == self).unwrap_or(0) as u8
    }

    pub fn label(self) -> &'static str {
        match self {
            StudioLook::None => "Không",
            StudioLook::Clear => "Trong trẻo",
            StudioLook::Rosy => "Hồng hào",
            StudioLook::Fair => "Trắng sáng",
            StudioLook::Warm => "Ấm áp",
            StudioLook::Natural => "Tự nhiên",
            StudioLook::SoftFilm => "Film nhẹ",
        }
    }

    pub fn tip(self) -> &'static str {
        match self {
            StudioLook::None => "Giữ màu ảnh như hiện tại",
            StudioLook::Clear => "Sáng, sạch, hơi mát — da trong, trắng nền tinh",
            StudioLook::Rosy => "Da hồng hào tươi tắn, má có sức sống",
            StudioLook::Fair => "Kiểu Hàn: da trắng sáng, mềm, bớt vàng",
            StudioLook::Warm => "Ấm kiểu studio: da vàng mật ong, đậm đà",
            StudioLook::Natural => "Giữ màu thật, chỉ tươi và rõ khối hơn",
            StudioLook::SoftFilm => "Màu phim nhẹ: đen mờ, sáng ấm, tối xanh",
        }
    }

    fn grade(self) -> Option<Grade> {
        let g = match self {
            StudioLook::None => return None,
            StudioLook::Clear => Grade {
                ev: 0.15,
                temp: -0.25,
                tint: 0.10,
                contrast: 0.15,
                lift: 0.0,
                saturation: 1.0,
                vibrance: 0.15,
                skin_hue: -3.0,
                skin_saturation: 0.92,
                skin_light: 0.06,
                shadows: [0.0, 0.0, 0.010],
                highlights: [0.0, 0.004, 0.012],
            },
            StudioLook::Rosy => Grade {
                ev: 0.12,
                temp: 0.05,
                tint: 0.35,
                contrast: 0.10,
                lift: 0.0,
                saturation: 1.0,
                vibrance: 0.12,
                skin_hue: -7.0,
                skin_saturation: 1.06,
                skin_light: 0.05,
                shadows: [0.0; 3],
                highlights: [0.018, 0.0, 0.008],
            },
            StudioLook::Fair => Grade {
                ev: 0.30,
                temp: -0.10,
                tint: 0.20,
                contrast: -0.08,
                lift: 0.025,
                saturation: 0.92,
                vibrance: 0.0,
                skin_hue: -5.0,
                skin_saturation: 0.78,
                skin_light: 0.10,
                shadows: [0.0; 3],
                highlights: [0.012, 0.004, 0.012],
            },
            StudioLook::Warm => Grade {
                ev: 0.05,
                temp: 0.45,
                tint: 0.05,
                contrast: 0.15,
                lift: 0.0,
                saturation: 1.04,
                vibrance: 0.12,
                skin_hue: 2.0,
                skin_saturation: 1.05,
                skin_light: 0.02,
                shadows: [0.015, 0.008, -0.005],
                highlights: [0.0; 3],
            },
            StudioLook::Natural => Grade {
                ev: 0.08,
                temp: 0.0,
                tint: 0.0,
                contrast: 0.10,
                lift: 0.0,
                saturation: 1.0,
                vibrance: 0.22,
                skin_hue: 0.0,
                skin_saturation: 1.0,
                skin_light: 0.03,
                shadows: [0.0; 3],
                highlights: [0.0; 3],
            },
            StudioLook::SoftFilm => Grade {
                ev: 0.05,
                temp: 0.12,
                tint: -0.05,
                contrast: 0.18,
                lift: 0.05,
                saturation: 0.85,
                vibrance: 0.0,
                skin_hue: -2.0,
                skin_saturation: 0.95,
                skin_light: 0.02,
                shadows: [-0.012, 0.006, 0.020],
                highlights: [0.025, 0.012, -0.012],
            },
        };
        Some(g)
    }

    /// Three sample skin tones (shadow, mid, light) after the look, for the
    /// dialog's swatches.
    pub fn swatch(self) -> [[u8; 3]; 3] {
        const SKIN: [[u8; 3]; 3] = [[150, 98, 74], [214, 160, 130], [243, 206, 184]];
        let Some(grade) = self.grade() else {
            return SKIN;
        };
        SKIN.map(|c| {
            let out = grade.apply([
                c[0] as f32 / 255.0,
                c[1] as f32 / 255.0,
                c[2] as f32 / 255.0,
            ]);
            out.map(|v| (v * 255.0).round().clamp(0.0, 255.0) as u8)
        })
    }
}

fn linear_to_srgb(v: f32) -> f32 {
    let v = v.max(0.0);
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

fn smoothstep(lo: f32, hi: f32, v: f32) -> f32 {
    let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn rgb_to_hsv(c: [f32; 3]) -> [f32; 3] {
    let max = c[0].max(c[1]).max(c[2]);
    let min = c[0].min(c[1]).min(c[2]);
    let d = max - min;
    let h = if d <= 1e-6 {
        0.0
    } else if max == c[0] {
        60.0 * ((c[1] - c[2]) / d).rem_euclid(6.0)
    } else if max == c[1] {
        60.0 * ((c[2] - c[0]) / d + 2.0)
    } else {
        60.0 * ((c[0] - c[1]) / d + 4.0)
    };
    let s = if max <= 1e-6 { 0.0 } else { d / max };
    [h, s, max]
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r + m, g + m, b + m]
}

/// How much a colour reads as skin: orange hues, moderately saturated, not
/// too dark.
fn skin_weight(hsv: [f32; 3]) -> f32 {
    let d = (hsv[0] - 25.0).abs().min(360.0 - (hsv[0] - 25.0).abs());
    if d >= 24.0 {
        return 0.0;
    }
    let hue = (std::f32::consts::FRAC_PI_2 * d / 24.0).cos().powi(2);
    let sat = smoothstep(0.06, 0.18, hsv[1]) * (1.0 - smoothstep(0.65, 0.9, hsv[1]));
    hue * sat * smoothstep(0.12, 0.3, hsv[2])
}

impl Grade {
    /// sRGB 0..1 in and out.
    fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        // White balance (luminance kept) and exposure, in linear light, with
        // a soft shoulder so brightened skin never clips flat.
        let wb = [
            1.0 + 0.10 * self.temp + 0.03 * self.tint,
            1.0 - 0.06 * self.tint,
            1.0 - 0.10 * self.temp + 0.03 * self.tint,
        ];
        let norm = luma(wb);
        let gain = 2f32.powf(self.ev);
        let knee = 0.8;
        let mut rgb = [0.0f32; 3];
        for i in 0..3 {
            let mut v = srgb_to_linear(c[i]) * wb[i] / norm * gain;
            if v > knee {
                v = knee + (1.0 - knee) * ((v - knee) / (1.0 - knee)).tanh();
            }
            rgb[i] = linear_to_srgb(v).min(1.0);
        }

        let hsv = rgb_to_hsv(rgb);
        let skin = skin_weight(hsv);
        if skin > 0.0 {
            let s = (hsv[1] * (1.0 + skin * (self.skin_saturation - 1.0))).clamp(0.0, 1.0);
            let v = hsv[2] + skin * self.skin_light * (1.0 - hsv[2]);
            rgb = hsv_to_rgb(hsv[0] + skin * self.skin_hue, s, v.min(1.0));
        }

        let y = luma(rgb);
        let chroma = rgb_to_hsv(rgb)[1];
        let factor = self.saturation * (1.0 + self.vibrance * (1.0 - chroma).powi(2));
        for v in &mut rgb {
            *v = y + (*v - y) * factor;
        }

        for v in &mut rgb {
            let x = v.clamp(0.0, 1.0);
            let curved = x + self.contrast * x * (1.0 - x) * (2.0 * x - 1.0);
            *v = self.lift + curved * (1.0 - self.lift);
        }

        let y = luma(rgb).clamp(0.0, 1.0);
        let (dark, light) = ((1.0 - y).powi(2), y * y);
        for i in 0..3 {
            rgb[i] = (rgb[i] + self.shadows[i] * dark + self.highlights[i] * light).clamp(0.0, 1.0);
        }

        // Whites stay white (a white shirt must not turn pink or cream): bright,
        // near-neutral colours keep their own tint at the graded brightness.
        let original = rgb_to_hsv(c);
        let protect = smoothstep(0.62, 0.85, luma(c)) * (1.0 - smoothstep(0.06, 0.16, original[1]));
        if protect > 0.0 {
            let (y_out, y_in) = (luma(rgb), luma(c));
            for i in 0..3 {
                let neutral = (y_out + (c[i] - y_in)).clamp(0.0, 1.0);
                rgb[i] += (neutral - rgb[i]) * protect;
            }
        }
        rgb
    }
}

/// A look baked into a 33³ LUT over sRGB.
pub struct LookLut {
    data: Vec<[f32; 3]>,
}

const LUT_SIDE: usize = 33;

impl LookLut {
    /// `None` for [`StudioLook::None`].
    pub fn new(look: StudioLook) -> Option<Self> {
        let grade = look.grade()?;
        let n = LUT_SIDE;
        let step = 1.0 / (n - 1) as f32;
        let data = (0..n * n * n)
            .into_par_iter()
            .map(|i| {
                let (r, g, b) = (i % n, i / n % n, i / (n * n));
                grade
                    .apply([r as f32 * step, g as f32 * step, b as f32 * step])
                    .map(|v| v * 255.0)
            })
            .collect();
        Some(Self { data })
    }

    /// Trilinear lookup; 0..255 in and out.
    pub fn map(&self, c: [u8; 3]) -> [f32; 3] {
        let n = LUT_SIDE;
        let scale = (n - 1) as f32 / 255.0;
        let pos = c.map(|v| v as f32 * scale);
        let base = pos.map(|p| (p as usize).min(n - 2));
        let t = [
            pos[0] - base[0] as f32,
            pos[1] - base[1] as f32,
            pos[2] - base[2] as f32,
        ];
        let at = |r: usize, g: usize, b: usize| self.data[(b * n + g) * n + r];
        let mut out = [0.0f32; 3];
        for (k, value) in out.iter_mut().enumerate() {
            let c00 = at(base[0], base[1], base[2])[k] * (1.0 - t[0])
                + at(base[0] + 1, base[1], base[2])[k] * t[0];
            let c10 = at(base[0], base[1] + 1, base[2])[k] * (1.0 - t[0])
                + at(base[0] + 1, base[1] + 1, base[2])[k] * t[0];
            let c01 = at(base[0], base[1], base[2] + 1)[k] * (1.0 - t[0])
                + at(base[0] + 1, base[1], base[2] + 1)[k] * t[0];
            let c11 = at(base[0], base[1] + 1, base[2] + 1)[k] * (1.0 - t[0])
                + at(base[0] + 1, base[1] + 1, base[2] + 1)[k] * t[0];
            let c0 = c00 * (1.0 - t[1]) + c10 * t[1];
            let c1 = c01 * (1.0 - t[1]) + c11 * t[1];
            *value = c0 * (1.0 - t[2]) + c1 * t[2];
        }
        out
    }

    /// Grade straight RGBA in place, mixing `strength` (0..1) of the look
    /// in; alpha is kept.
    pub fn apply(&self, rgba: &mut [u8], strength: f32) {
        let strength = strength.clamp(0.0, 1.0);
        rgba.par_chunks_exact_mut(4).for_each(|px| {
            if px[3] == 0 {
                return;
            }
            let graded = self.map([px[0], px[1], px[2]]);
            for c in 0..3 {
                let v = px[c] as f32 + (graded[c] - px[c] as f32) * strength;
                px[c] = v.round().clamp(0.0, 255.0) as u8;
            }
        });
    }
}

/// The whole layer `src` (`width × height`) with the retouch `rendered`
/// written in and `look` mixed in at `strength` (0..1): what the preview
/// shows. `rendered` unchanged when there is no look.
pub fn preview_with_look(
    src: &[u8],
    width: u32,
    height: u32,
    rendered: Option<(Region, Vec<u8>)>,
    look: StudioLook,
    strength: f32,
) -> Option<(Region, Vec<u8>)> {
    let Some(lut) = LookLut::new(look).filter(|_| strength > 0.0) else {
        return rendered;
    };
    let mut full = with_retouch(src, width, rendered);
    lut.apply(&mut full, strength);
    Some((
        Region {
            x: 0,
            y: 0,
            w: width,
            h: height,
        },
        full,
    ))
}

/// `src` with the retouched region written over it.
pub fn with_retouch(src: &[u8], width: u32, rendered: Option<(Region, Vec<u8>)>) -> Vec<u8> {
    let mut full = src.to_vec();
    if let Some((region, pixels)) = rendered {
        let row = region.w as usize * 4;
        for y in 0..region.h as usize {
            let o = ((region.y as usize + y) * width as usize + region.x as usize) * 4;
            full[o..o + row].copy_from_slice(&pixels[y * row..(y + 1) * row]);
        }
    }
    full
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_has_no_lut_and_leaves_the_preview_alone() {
        assert!(LookLut::new(StudioLook::None).is_none());
        let src = vec![10u8, 20, 30, 255];
        assert!(preview_with_look(&src, 1, 1, None, StudioLook::None, 1.0).is_none());
    }

    #[test]
    fn index_round_trips() {
        for look in StudioLook::ALL {
            assert_eq!(StudioLook::from_index(look.index()), look);
        }
        assert_eq!(StudioLook::from_index(200), StudioLook::None);
    }

    #[test]
    fn lut_matches_the_grade() {
        for look in StudioLook::ALL.into_iter().skip(1) {
            let lut = LookLut::new(look).unwrap();
            let grade = look.grade().unwrap();
            for c in [
                [214u8, 160, 130],
                [30, 40, 50],
                [250, 250, 250],
                [90, 140, 60],
            ] {
                let direct = grade.apply(c.map(|v| v as f32 / 255.0));
                let got = lut.map(c);
                for k in 0..3 {
                    assert!(
                        (got[k] - direct[k] * 255.0).abs() < 3.0,
                        "{look:?} {c:?}: {got:?} vs {direct:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn looks_move_skin_their_own_way() {
        let skin = [214u8, 160, 130];
        let graded = |look: StudioLook| LookLut::new(look).unwrap().map(skin);
        let warmth = |c: [f32; 3]| c[0] - c[2];
        let base = warmth(skin.map(|v| v as f32));
        assert!(warmth(graded(StudioLook::Warm)) > base + 5.0);
        assert!(warmth(graded(StudioLook::Clear)) < base);
        // Rosy turns skin pinker: less green against red.
        let rosy = graded(StudioLook::Rosy);
        assert!(rosy[0] - rosy[1] > (skin[0] - skin[1]) as f32);
        // Fair brightens.
        assert!(luma(graded(StudioLook::Fair)) > luma(skin.map(|v| v as f32)) + 8.0);
    }

    #[test]
    fn a_white_shirt_stays_white() {
        for look in StudioLook::ALL.into_iter().skip(1) {
            let out = LookLut::new(look).unwrap().map([238, 238, 236]);
            let spread = out.iter().cloned().fold(f32::MIN, f32::max)
                - out.iter().cloned().fold(f32::MAX, f32::min);
            assert!(spread < 6.0, "{look:?}: {out:?}");
        }
    }

    #[test]
    fn strength_mixes_and_alpha_stays() {
        let lut = LookLut::new(StudioLook::Warm).unwrap();
        let src = [180u8, 140, 120, 128, 50, 60, 70, 0];
        let mut half = src;
        lut.apply(&mut half, 0.5);
        let mut full = src;
        lut.apply(&mut full, 1.0);
        assert_eq!(half[3], 128);
        assert_eq!(&half[4..], &src[4..], "transparent pixels untouched");
        for c in 0..3 {
            let mid = (src[c] as f32 + full[c] as f32) * 0.5;
            assert!((half[c] as f32 - mid).abs() <= 1.0);
        }
    }

    #[test]
    fn preview_writes_the_retouch_then_the_look_over_the_whole_layer() {
        let src = vec![200u8; 4 * 4 * 4];
        let region = Region {
            x: 1,
            y: 1,
            w: 2,
            h: 1,
        };
        let rendered = Some((region, vec![10u8; 2 * 4]));
        let (out_region, out) =
            preview_with_look(&src, 4, 4, rendered, StudioLook::Natural, 1.0).unwrap();
        assert_eq!((out_region.w, out_region.h), (4, 4));
        assert!(out[(4 + 1) * 4] < 40, "retouched pixel kept under the look");
        assert!(out[0] > 150);
    }

    /// Opt-in visual probe: IAI_LOOK_PROBE names an image; every look at
    /// IAI_LOOK_STRENGTH (default 70%) is written beside it as `look_<n>.png`.
    #[test]
    #[ignore]
    fn probe_looks() {
        let Ok(path) = std::env::var("IAI_LOOK_PROBE") else {
            return;
        };
        let strength: f32 = std::env::var("IAI_LOOK_STRENGTH")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.7);
        let path = std::path::PathBuf::from(path);
        let image = image::open(&path).unwrap().to_rgba8();
        let (w, h) = image.dimensions();
        for look in StudioLook::ALL.into_iter().skip(1) {
            let mut px = image.as_raw().clone();
            LookLut::new(look).unwrap().apply(&mut px, strength);
            image::RgbaImage::from_raw(w, h, px)
                .unwrap()
                .save(path.with_file_name(format!("look_{}.png", look.index())))
                .unwrap();
        }
    }
}

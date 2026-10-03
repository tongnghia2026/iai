//! "Sửa màu & sáng": corrections for portraits shot on a phone in poor
//! light, before any look goes on. The colour cast is read off the face's own
//! skin (a shop's green shelves or yellow lamps fool any measure taken from
//! the scene), the exposure off the skin's brightness, and the haze off the
//! photo's darkest pixels; all three are baked into one LUT for the whole
//! layer.

use rayon::prelude::*;

use super::analysis::FaceModel;
use super::looks::{linear_to_srgb, LookLut};
use crate::core::develop::srgb_to_linear;

/// Skin of every tone lies near one ray of (ln R/G, ln B/G) from neutral:
/// B/G falls as R/G rises. Across the ray is cast (green, yellow, blue,
/// magenta); along it, skin of more or less colour or a warm or cool cast.
const SKIN_RAY: [f32; 2] = [0.849_9, -0.527_0];
/// Where average skin sits on the ray.
const SKIN_ON_RAY: f32 = 0.62;
/// How much of skin lying off that average along the ray counts as cast.
const WARM_SHARE: f32 = 0.4;
/// Casts beyond this (in ln units) are not chased: the skin was misread.
const CAST_LIMIT: f32 = 0.6;
/// Linear luminance well-lit skin is brought to.
const SKIN_LUMINANCE: f32 = 0.42;
const EV_RANGE: (f32, f32) = (-0.6, 2.0);
/// The darkest share of the photo that holds its veil, and the most veil
/// (linear) taken off.
const VEIL_SHARE: f32 = 0.01;
const VEIL_LIMIT: f32 = 0.04;
/// How much of the veil "Khử đục" at 100 removes, and the contrast it adds.
const VEIL_REMOVED: f32 = 0.9;
const HAZE_CONTRAST: f32 = 0.25;
/// "Ấm / lạnh" at ±100, in ln units of R against B.
const WARMTH_RANGE: f32 = 0.22;
/// Above this (linear) a brightened channel is eased toward white.
const KNEE: f32 = 0.8;

fn luminance(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// What the corrections are measured from, read once from the photo.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightStats {
    /// Mean colour of the faces' skin, linear RGB; `None` without any.
    pub skin: Option<[f32; 3]>,
    /// Mean colour of the darkest pixels, linear RGB.
    pub veil: [f32; 3],
}

impl LightStats {
    /// Measure `rgba` (straight alpha, `width` wide) with its analysed
    /// `faces`. Transparent pixels (a cut-out's background) are left out.
    pub fn measure(rgba: &[u8], width: u32, faces: &[FaceModel]) -> Self {
        let linear = |o: usize| -> [f32; 3] {
            [rgba[o], rgba[o + 1], rgba[o + 2]].map(|v| srgb_to_linear(v as f32 / 255.0))
        };
        // Skin well inside its outline, neither in deep shade nor blown.
        let (mut sum, mut weight) = ([0.0f64; 3], 0.0f64);
        for face in faces {
            let (skin, region) = (&face.skin, face.region);
            for k in 0..region.len() {
                let i = skin.region.index_of(region, k);
                let inside = skin.interior[i] as f32 / 255.0;
                if inside <= 0.0 {
                    continue;
                }
                let (x, y) = (
                    region.x as usize + k % region.w as usize,
                    region.y as usize + k / region.w as usize,
                );
                let c = linear((y * width as usize + x) * 4);
                let l = luminance(c);
                if !(0.03..=0.85).contains(&l) {
                    continue;
                }
                weight += inside as f64;
                for (s, v) in sum.iter_mut().zip(c) {
                    *s += (v * inside) as f64;
                }
            }
        }
        let skin = (weight > 50.0).then(|| sum.map(|s| (s / weight) as f32));

        // The darkest pixels, from a sample of the opaque ones.
        let pixels = rgba.len() / 4;
        let step = (pixels / 400_000).max(1);
        let sample: Vec<(u8, [f32; 3])> = (0..pixels)
            .into_par_iter()
            .step_by(step)
            .filter(|&p| rgba[p * 4 + 3] >= 128)
            .map(|p| {
                let c = linear(p * 4);
                let level = (linear_to_srgb(luminance(c)) * 255.0).round() as u8;
                (level, c)
            })
            .collect();
        let mut histogram = [0usize; 256];
        for (level, _) in &sample {
            histogram[*level as usize] += 1;
        }
        let wanted = ((sample.len() as f32 * VEIL_SHARE) as usize).max(1);
        let mut seen = 0;
        let cut = histogram
            .iter()
            .position(|&n| {
                seen += n;
                seen >= wanted
            })
            .unwrap_or(0) as u8;
        let (mut dark, mut count) = ([0.0f64; 3], 0usize);
        for (level, c) in &sample {
            if *level <= cut {
                count += 1;
                for (s, v) in dark.iter_mut().zip(c) {
                    *s += *v as f64;
                }
            }
        }
        let veil = if count == 0 {
            [0.0; 3]
        } else {
            dark.map(|s| ((s / count as f64) as f32).min(VEIL_LIMIT))
        };
        Self { skin, veil }
    }

    /// The colour cast as (ln R/G, ln B/G), read off the skin: all of what
    /// lies across the skin ray, and part of what lies along it.
    pub fn cast(&self) -> [f32; 2] {
        let Some(skin) = self.skin else {
            return [0.0; 2];
        };
        if skin.iter().any(|&v| v <= 1e-4) {
            return [0.0; 2];
        }
        let p = [(skin[0] / skin[1]).ln(), (skin[2] / skin[1]).ln()];
        let [dx, dy] = SKIN_RAY;
        let along = p[0] * dx + p[1] * dy;
        let across = -p[0] * dy + p[1] * dx;
        let warm = (along - SKIN_ON_RAY) * WARM_SHARE;
        let cast = [across * -dy + warm * dx, across * dx + warm * dy];
        let size = cast[0].hypot(cast[1]);
        if size > CAST_LIMIT {
            cast.map(|v| v * CAST_LIMIT / size)
        } else {
            cast
        }
    }
}

/// The correction sliders, 0..1 (`warmth` −1..1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fixes {
    /// "Khử ám màu": how much of the measured cast is taken out.
    pub cast: f32,
    /// "Ấm / lạnh": a manual trim on top, cooler (negative) or warmer.
    pub warmth: f32,
    /// "Cân sáng": how far the skin is brought to a well-lit brightness.
    pub exposure: f32,
    /// "Khử đục": how much of the veil over the blacks is removed.
    pub haze: f32,
}

impl Fixes {
    pub fn is_neutral(&self) -> bool {
        self.cast <= 0.0 && self.warmth == 0.0 && self.exposure <= 0.0 && self.haze <= 0.0
    }
}

/// One colour through the corrections, linear RGB in, sRGB out.
struct Correction {
    veil: [f32; 3],
    /// Per channel: white balance and exposure together.
    gain: [f32; 3],
    contrast: f32,
}

impl Correction {
    fn new(stats: &LightStats, fixes: &Fixes) -> Self {
        let veil = stats
            .veil
            .map(|v| v * VEIL_REMOVED * fixes.haze.clamp(0.0, 1.0));
        let cast = stats.cast().map(|v| v * fixes.cast.clamp(0.0, 1.0));
        let warmth = fixes.warmth.clamp(-1.0, 1.0) * WARMTH_RANGE;
        let balance = [(warmth - cast[0]).exp(), 1.0, (-warmth - cast[1]).exp()];
        // White balance keeps a grey's brightness.
        let norm = luminance(balance);
        let balance = balance.map(|g| g / norm);
        let ev = stats.skin.map_or(0.0, |skin| {
            let lit: [f32; 3] = std::array::from_fn(|c| {
                ((skin[c] - veil[c]) / (1.0 - veil[c])).max(0.0) * balance[c]
            });
            let ev = (SKIN_LUMINANCE / luminance(lit).max(1e-3)).log2();
            ev.clamp(EV_RANGE.0, EV_RANGE.1) * fixes.exposure.clamp(0.0, 1.0)
        });
        Self {
            veil,
            gain: balance.map(|g| g * 2f32.powf(ev)),
            contrast: HAZE_CONTRAST * fixes.haze.clamp(0.0, 1.0),
        }
    }

    /// sRGB 0..1 in and out.
    fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        std::array::from_fn(|i| {
            let v = ((srgb_to_linear(c[i]) - self.veil[i]) / (1.0 - self.veil[i])).max(0.0);
            // A brightened channel eases into white instead of clipping; a
            // blown white stays white whatever the balance.
            let top = self.gain[i];
            let mut v = v * top;
            if top > 1.0 && v > KNEE {
                let over = v - KNEE;
                let k = 1.0 / (1.0 - KNEE) - 1.0 / (top - KNEE);
                v = KNEE + over / (1.0 + k * over);
            }
            let x = linear_to_srgb(v).clamp(0.0, 1.0);
            x + self.contrast * x * (1.0 - x) * (2.0 * x - 1.0)
        })
    }
}

/// The corrections baked into a LUT; `None` when every slider is at rest.
pub fn fix_lut(stats: &LightStats, fixes: &Fixes) -> Option<LookLut> {
    if fixes.is_neutral() {
        return None;
    }
    let correction = Correction::new(stats, fixes);
    Some(LookLut::from_fn(|c| correction.apply(c)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_FIX: Fixes = Fixes {
        cast: 0.0,
        warmth: 0.0,
        exposure: 0.0,
        haze: 0.0,
    };

    /// Average skin (linear) seen through per-channel `cast` gains.
    fn stats(cast: [f32; 3]) -> LightStats {
        let skin = [0.50f32, 0.30, 0.21];
        LightStats {
            skin: Some(std::array::from_fn(|c| skin[c] * cast[c])),
            veil: [0.0; 3],
        }
    }

    #[test]
    fn skin_on_the_ray_reads_as_no_cast_whatever_its_tone() {
        // Pale, average and tanned skin: none is far off neutral.
        for skin in [
            [0.87f32, 0.64, 0.55],
            [0.75, 0.41, 0.29],
            [0.35, 0.156, 0.091],
        ] {
            let cast = LightStats {
                skin: Some(skin),
                veil: [0.0; 3],
            }
            .cast();
            assert!(cast[0].hypot(cast[1]) < 0.16, "{skin:?}: {cast:?}");
        }
        assert_eq!(
            LightStats {
                skin: None,
                veil: [0.0; 3]
            }
            .cast(),
            [0.0; 2]
        );
    }

    #[test]
    fn green_and_yellow_casts_are_read_off_the_skin_and_taken_out() {
        let clean = stats([1.0; 3]).cast();
        for (name, tint) in [("green", [0.85f32, 1.0, 0.85]), ("yellow", [1.0, 1.0, 0.7])] {
            let stats = stats(tint);
            let cast = stats.cast();
            // Most of the cast laid on the skin is found.
            let laid = [(tint[0] / tint[1]).ln(), (tint[2] / tint[1]).ln()];
            let found = [cast[0] - clean[0], cast[1] - clean[1]];
            let share =
                (found[0] * laid[0] + found[1] * laid[1]) / (laid[0].powi(2) + laid[1].powi(2));
            assert!((0.7..=1.05).contains(&share), "{name}: found {share} of it");

            // Corrected, a grey seen through the cast is close to grey again.
            let fixes = Fixes {
                cast: 1.0,
                ..NO_FIX
            };
            let lut = fix_lut(&stats, &fixes).unwrap();
            let grey: [u8; 3] =
                std::array::from_fn(|c| (linear_to_srgb(0.2 * tint[c]) * 255.0).round() as u8);
            let before = grey.iter().max().unwrap() - grey.iter().min().unwrap();
            let out = lut.map(grey);
            let after = out.iter().cloned().fold(f32::MIN, f32::max)
                - out.iter().cloned().fold(f32::MAX, f32::min);
            assert!(after < 0.6 * before as f32, "{name}: {grey:?} -> {out:?}");
        }
    }

    #[test]
    fn exposure_brings_dim_skin_up_and_keeps_white_from_clipping() {
        let dim = LightStats {
            skin: Some([0.20, 0.12, 0.085]),
            veil: [0.0; 3],
        };
        let fixes = Fixes {
            exposure: 1.0,
            ..NO_FIX
        };
        let lut = fix_lut(&dim, &fixes).unwrap();
        let skin = [0.20f32, 0.12, 0.085].map(|v| (linear_to_srgb(v) * 255.0).round() as u8);
        let out = lut.map(skin).map(|v| srgb_to_linear(v / 255.0));
        assert!((luminance(out) - SKIN_LUMINANCE).abs() < 0.03, "{out:?}");
        // Brighter values keep their order and white stays white.
        let (light, white) = (lut.map([230; 3]), lut.map([255; 3]));
        assert!(light[1] > lut.map([200; 3])[1] && light[1] < white[1]);
        assert!(white.iter().all(|&v| v > 253.0), "{white:?}");
        // Skin already bright enough is left alone.
        let lit = LightStats {
            skin: Some([0.60, 0.38, 0.28]),
            veil: [0.0; 3],
        };
        let out = fix_lut(&lit, &fixes).unwrap().map([128; 3]);
        assert!((out[1] - 128.0).abs() < 6.0, "{out:?}");
    }

    #[test]
    fn haze_takes_the_veil_off_the_blacks_and_leaves_white() {
        let hazy = LightStats {
            skin: None,
            veil: [0.02, 0.03, 0.025],
        };
        let fixes = Fixes {
            haze: 1.0,
            ..NO_FIX
        };
        let lut = fix_lut(&hazy, &fixes).unwrap();
        let veil = [0.02f32, 0.03, 0.025].map(|v| (linear_to_srgb(v) * 255.0).round() as u8);
        let black = lut.map(veil);
        assert!(black.iter().all(|&v| v < 25.0), "{veil:?} -> {black:?}");
        assert!(lut.map([255; 3]).iter().all(|&v| v > 253.0));
        assert!(fix_lut(&hazy, &NO_FIX).is_none());
    }

    #[test]
    fn warmth_trims_by_hand() {
        let lut = |warmth: f32| {
            fix_lut(&stats([1.0; 3]), &Fixes { warmth, ..NO_FIX })
                .unwrap()
                .map([128; 3])
        };
        let (warm, cool) = (lut(1.0), lut(-1.0));
        assert!(warm[0] > 134.0 && warm[2] < 122.0, "{warm:?}");
        assert!(cool[0] < 122.0 && cool[2] > 134.0, "{cool:?}");
    }
}

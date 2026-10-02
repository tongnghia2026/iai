//! Slider settings and the cheap per-pixel recombination that turns a
//! [`PortraitModel`] into retouched pixels.

use std::sync::Arc;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::analysis::{luma, BrowLayers, FaceModel, PortraitModel, SkinLayers, BLEMISH_SCALE};
use super::body::BodySliders;
use super::geometry::Region;
use super::reshape::{reshape, FaceShape};
use crate::core::color::luminance_f32;
use crate::core::develop::{
    apply_light_luma, apply_luma_target, local_detail_boost, srgb_to_linear, DevelopEngineVersion,
    DevelopSettings, CONTROL_LIMIT,
};
use crate::core::develop_scene::{build_scene_tone_for, BaseLook, SceneToneData, SCENE_EV_MIN};

/// Sliders run 0..100, the two-sided ones (lip saturation and brightness,
/// brows, hair brightness) -100..100, and the colour pickers (`*_hue`) are
/// target hues in degrees, 0..360, applied by the matching `*_tint` amount.
/// Brows have only their own sliders, all 0 by default: they stay as shot.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PortraitSettings {
    pub smooth: f32,
    pub even_tone: f32,
    pub shine: f32,
    pub brighten: f32,
    pub blemish: f32,
    pub dark_circles: f32,
    pub eye_white: f32,
    pub iris: f32,
    pub teeth: f32,
    pub lip_saturation: f32,
    pub lip_hue: f32,
    pub lip_brightness: f32,
    pub sharpen: f32,
    pub brows: f32,
    pub nose_bridge: f32,
    pub iris_hue: f32,
    pub iris_tint: f32,
    pub lip_tint: f32,
    pub hair_brightness: f32,
    pub hair_hue: f32,
    pub hair_tint: f32,
    pub brow_sharpen: f32,
    pub brow_hue: f32,
    pub brow_tint: f32,
    /// Face shape, -100..100 (0 = as shot); see [`FaceShape`].
    pub face_slim: f32,
    pub chin_length: f32,
    pub eye_size: f32,
    pub nose_slim: f32,
    pub mouth_width: f32,
    pub forehead_height: f32,
    pub smile: f32,
    pub lip_fullness: f32,
    pub eye_tilt: f32,
    pub face_squeeze: f32,
    /// Body shape, -100..100 (leg length 0..100; 0 = as shot); see
    /// [`BodySliders`].
    pub body_waist: f32,
    pub body_shoulders: f32,
    pub body_neck: f32,
    pub body_arms: f32,
    pub body_legs: f32,
    pub body_leg_length: f32,
}

impl Default for PortraitSettings {
    fn default() -> Self {
        Self {
            smooth: 40.0,
            even_tone: 25.0,
            shine: 20.0,
            brighten: 0.0,
            blemish: 60.0,
            dark_circles: 30.0,
            eye_white: 25.0,
            iris: 15.0,
            teeth: 25.0,
            lip_saturation: 0.0,
            lip_hue: 350.0,
            lip_brightness: 0.0,
            sharpen: 20.0,
            brows: 0.0,
            nose_bridge: 0.0,
            iris_hue: 200.0,
            iris_tint: 0.0,
            lip_tint: 0.0,
            hair_brightness: 0.0,
            hair_hue: 25.0,
            hair_tint: 0.0,
            brow_sharpen: 0.0,
            brow_hue: 25.0,
            brow_tint: 0.0,
            face_slim: 0.0,
            chin_length: 0.0,
            eye_size: 0.0,
            nose_slim: 0.0,
            mouth_width: 0.0,
            forehead_height: 0.0,
            smile: 0.0,
            lip_fullness: 0.0,
            eye_tilt: 0.0,
            face_squeeze: 0.0,
            body_waist: 0.0,
            body_shoulders: 0.0,
            body_neck: 0.0,
            body_arms: 0.0,
            body_legs: 0.0,
            body_leg_length: 0.0,
        }
    }
}

impl PortraitSettings {
    pub const NEUTRAL: Self = Self {
        smooth: 0.0,
        even_tone: 0.0,
        shine: 0.0,
        brighten: 0.0,
        blemish: 0.0,
        dark_circles: 0.0,
        eye_white: 0.0,
        iris: 0.0,
        teeth: 0.0,
        lip_saturation: 0.0,
        lip_hue: 350.0,
        lip_brightness: 0.0,
        sharpen: 0.0,
        brows: 0.0,
        nose_bridge: 0.0,
        iris_hue: 200.0,
        iris_tint: 0.0,
        lip_tint: 0.0,
        hair_brightness: 0.0,
        hair_hue: 25.0,
        hair_tint: 0.0,
        brow_sharpen: 0.0,
        brow_hue: 25.0,
        brow_tint: 0.0,
        face_slim: 0.0,
        chin_length: 0.0,
        eye_size: 0.0,
        nose_slim: 0.0,
        mouth_width: 0.0,
        forehead_height: 0.0,
        smile: 0.0,
        lip_fullness: 0.0,
        eye_tilt: 0.0,
        face_squeeze: 0.0,
        body_waist: 0.0,
        body_shoulders: 0.0,
        body_neck: 0.0,
        body_arms: 0.0,
        body_legs: 0.0,
        body_leg_length: 0.0,
    };

    fn unit(&self) -> Self {
        let u = |v: f32| (v / 100.0).clamp(0.0, 1.0);
        let both = |v: f32| (v / 100.0).clamp(-1.0, 1.0);
        Self {
            smooth: u(self.smooth),
            even_tone: u(self.even_tone),
            shine: u(self.shine),
            brighten: u(self.brighten),
            blemish: u(self.blemish),
            dark_circles: u(self.dark_circles),
            eye_white: u(self.eye_white),
            iris: u(self.iris),
            teeth: u(self.teeth),
            lip_saturation: both(self.lip_saturation),
            lip_hue: self.lip_hue.rem_euclid(360.0),
            lip_brightness: both(self.lip_brightness),
            sharpen: u(self.sharpen),
            brows: both(self.brows),
            nose_bridge: u(self.nose_bridge),
            iris_hue: self.iris_hue.rem_euclid(360.0),
            iris_tint: u(self.iris_tint),
            lip_tint: u(self.lip_tint),
            hair_brightness: both(self.hair_brightness),
            hair_hue: self.hair_hue.rem_euclid(360.0),
            hair_tint: u(self.hair_tint),
            brow_sharpen: u(self.brow_sharpen),
            brow_hue: self.brow_hue.rem_euclid(360.0),
            brow_tint: u(self.brow_tint),
            ..*self
        }
    }

    /// The face shape sliders.
    pub fn face_shape(&self) -> FaceShape {
        FaceShape {
            slim: self.face_slim,
            chin: self.chin_length,
            eyes: self.eye_size,
            nose: self.nose_slim,
            mouth: self.mouth_width,
            forehead: self.forehead_height,
            smile: self.smile,
            lips: self.lip_fullness,
            eye_tilt: self.eye_tilt,
            squeeze: self.face_squeeze,
        }
    }

    /// The body shape sliders.
    pub fn body_shape(&self) -> BodySliders {
        BodySliders {
            waist: self.body_waist,
            shoulders: self.body_shoulders,
            neck: self.body_neck,
            arms: self.body_arms,
            legs: self.body_legs,
            leg_length: self.body_leg_length,
        }
    }

    /// These settings with the face and body shape as shot.
    pub fn without_shape(&self) -> Self {
        Self {
            face_slim: 0.0,
            chin_length: 0.0,
            eye_size: 0.0,
            nose_slim: 0.0,
            mouth_width: 0.0,
            forehead_height: 0.0,
            smile: 0.0,
            lip_fullness: 0.0,
            eye_tilt: 0.0,
            face_squeeze: 0.0,
            body_waist: 0.0,
            body_shoulders: 0.0,
            body_neck: 0.0,
            body_arms: 0.0,
            body_legs: 0.0,
            body_leg_length: 0.0,
            ..*self
        }
    }

    fn hair_active(&self) -> bool {
        self.hair_brightness != 0.0 || self.hair_tint > 0.0
    }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn from_u16(c: [u16; 3]) -> [f32; 3] {
    c.map(|v| v as f32 / 65535.0)
}

fn split(c: [f32; 3]) -> (f32, [f32; 3]) {
    let y = luma(c);
    (y, c.map(|v| v - y))
}

fn join(y: f32, chroma: [f32; 3]) -> [f32; 3] {
    chroma.map(|v| v + y)
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// Skin retouch of skin-region pixel `i` from its bands: the photo `src`, the
/// fine-split low `l1` and the mid-split low `l2`; `under` is its under-eye
/// weight (0..1).
#[allow(clippy::too_many_arguments)]
fn skin_result(
    skin: &SkinLayers,
    s: &PortraitSettings,
    i: usize,
    under: f32,
    src: [f32; 3],
    l1: [f32; 3],
    l2: [f32; 3],
    inside: f32,
) -> [f32; 3] {
    let fine = sub(src, l1);
    let mid = sub(l1, l2);

    let (mut low_y, mut low_c) = split(l2);
    let (_, mean_c) = split(skin.mean);
    // Even tone steers the hue toward the face's average while keeping most
    // of the local saturation, so skin evens out without going grey.
    let magnitude = |c: [f32; 3]| (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt();
    let (local, average) = (magnitude(low_c), magnitude(mean_c).max(1e-4));
    let target_mag = local + (average - local) * 0.3;
    let target = mean_c.map(|v| v / average * target_mag);
    for k in 0..3 {
        low_c[k] += (target[k] - low_c[k]) * 0.6 * s.even_tone;
    }
    let under = under * s.dark_circles;
    if under > 0.0 {
        low_y += under * (skin.cheek_luma - low_y).max(0.0) * 0.85;
        for k in 0..3 {
            low_c[k] += (mean_c[k] - low_c[k]) * under * 0.5;
        }
    }

    let keep_mid = 1.0 - 0.85 * s.smooth * inside;
    let keep_fine = 1.0 - 0.15 * s.smooth * inside;
    let (mid_y, mid_c) = split(mid);
    let low = join(low_y, low_c);
    let mut r = [0.0f32; 3];
    for k in 0..3 {
        r[k] = low[k]
            + mid_y * keep_mid
            + mid_c[k] * keep_mid * (1.0 - 0.6 * s.even_tone * inside)
            + fine[k] * keep_fine;
    }

    let y = luma(r);
    let lift = y - skin.broad[i] as f32 / 65535.0;
    let shine = s.shine * smoothstep(0.03, 0.18, lift) * smoothstep(0.45, 0.8, y);
    if shine > 0.0 {
        for v in r.iter_mut() {
            *v -= shine * lift * 0.75;
        }
    }
    if s.brighten > 0.0 {
        let power = 1.0 + 0.5 * s.brighten;
        for v in r.iter_mut() {
            *v = 1.0 - (1.0 - v.clamp(0.0, 1.0)).powf(power);
        }
    }
    r
}

fn rgb_to_hsl(c: [f32; 3]) -> [f32; 3] {
    let (max, min) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
    let l = (max + min) * 0.5;
    let d = max - min;
    if d <= 1e-6 {
        return [0.0, 0.0, l];
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs()).max(1e-6);
    let h = if max == c[0] {
        ((c[1] - c[2]) / d).rem_euclid(6.0)
    } else if max == c[1] {
        (c[2] - c[0]) / d + 2.0
    } else {
        (c[0] - c[1]) / d + 4.0
    };
    [h * 60.0, s.min(1.0), l]
}

fn hsl_to_rgb(hsl: [f32; 3]) -> [f32; 3] {
    let [h, s, l] = hsl;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match (h.rem_euclid(360.0) / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c * 0.5;
    [r + m, g + m, b + m]
}

/// Recolour to `hue` (degrees) keeping lightness, with at least `saturation`
/// so even grey-brown features take the colour.
fn colourise(c: [f32; 3], hue: f32, saturation: f32) -> [f32; 3] {
    let [_, s, l] = rgb_to_hsl(c.map(|v| v.clamp(0.0, 1.0)));
    hsl_to_rgb([hue, s.max(saturation), l])
}

/// The retouched colour of skin-region pixel `i`, which is pixel `f` of the
/// face region when it lies there. `src` is the photo in 0..1; `fetch(dx,
/// dy)` reads the photo at an offset from this pixel.
#[allow(clippy::too_many_arguments)]
fn retouch_pixel(
    face: &FaceModel,
    skin: &SkinLayers,
    brows: &BrowLayers,
    s: &PortraitSettings,
    i: usize,
    f: Option<usize>,
    src: [f32; 3],
    fetch: &dyn Fn(isize, isize) -> [f32; 3],
) -> [f32; 3] {
    let m = skin.mask[i] as f32 / 255.0;
    let mut out = src;
    if m > 0.0 {
        let inside = skin.interior[i] as f32 / 255.0;
        let l1 = from_u16(skin.low1[i]);
        let l2 = from_u16(skin.low2[i]);
        let under = f.map_or(0.0, |f| skin.under_eye[f] as f32 / 255.0);
        let mut r = skin_result(skin, s, i, under, src, l1, l2, inside);
        let cover = f.map_or(0.0, |f| face.spot_cover[f] as f32 / 255.0);
        if let Some(f) = f.filter(|_| s.blemish > 0.0 && cover > 0.0) {
            let score = face.spot_score[f] as f32 / BLEMISH_SCALE;
            let threshold = 1.5 - 1.15 * s.blemish;
            let spot = smoothstep(threshold * 0.85, threshold * 1.15, score) * cover * inside;
            if spot > 0.0 {
                // Heal like a healing brush: borrow the texture of nearby clean
                // skin, shifted to the colour around this spot.
                let here = from_u16(face.heal_base[f]);
                let [dx, dy] = face.donor[f];
                let (healed, healed_l1) = if dx == 0 && dy == 0 {
                    (here, here)
                } else {
                    let (dx, dy) = (dx as isize, dy as isize);
                    let qf = (f as isize + dy * face.region.w as isize + dx) as usize;
                    let qi = (i as isize + dy * skin.region.w as isize + dx) as usize;
                    let shift = sub(here, from_u16(face.heal_base[qf]));
                    (
                        add(fetch(dx, dy), shift),
                        add(from_u16(skin.low1[qi]), shift),
                    )
                };
                let fixed = skin_result(skin, s, i, under, healed, healed_l1, l2, inside);
                for k in 0..3 {
                    r[k] += (fixed[k] - r[k]) * spot;
                }
            }
        }
        let contour = f.map_or(0.0, |f| face.nose[f] as f32 / 127.0 * s.nose_bridge);
        if contour != 0.0 {
            // Shade lightly: the sides only need to hint at depth.
            let gain = if contour > 0.0 {
                0.18 * contour
            } else {
                0.03 * contour
            };
            r = r.map(|v| v * (1.0 + gain));
        }
        for k in 0..3 {
            out[k] = src[k] + m * (r[k] - src[k]);
        }
    }
    // The features lie in the face region.
    let Some(f) = f else {
        return out;
    };
    let white = face.eye_white[f] as f32 / 255.0 * s.eye_white;
    if white > 0.0 {
        let (y, c) = split(out);
        out = join(
            y * (1.0 + 0.15 * white),
            c.map(|v| v * (1.0 - 0.75 * white)),
        );
    }
    let iris = face.iris[f] as f32 / 255.0 * s.iris;
    if iris > 0.0 {
        let (y, c) = split(out);
        out = join(y * (1.0 + 0.18 * iris), c.map(|v| v * (1.0 + 0.35 * iris)));
    }
    let iris_tint = face.iris[f] as f32 / 255.0 * s.iris_tint;
    if iris_tint > 0.0 {
        let tinted = colourise(out, s.iris_hue, 0.5);
        for k in 0..3 {
            out[k] += (tinted[k] - out[k]) * iris_tint;
        }
    }
    let teeth = face.teeth[f] as f32 / 255.0 * s.teeth;
    if teeth > 0.0 {
        let (y, c) = split(out);
        out = join(y * (1.0 + 0.1 * teeth), c.map(|v| v * (1.0 - 0.8 * teeth)));
    }
    let lips = face.lips[f] as f32 / 255.0;
    if lips > 0.0 && (s.lip_saturation != 0.0 || s.lip_tint > 0.0 || s.lip_brightness != 0.0) {
        let tinted = colourise(out, s.lip_hue, 0.45);
        let mut lip = out;
        for k in 0..3 {
            lip[k] += (tinted[k] - lip[k]) * s.lip_tint;
        }
        let (y, c) = split(lip);
        let coloured = join(
            y * (1.0 + 0.3 * s.lip_brightness),
            c.map(|v| v * (1.0 + 0.8 * s.lip_saturation).max(0.0)),
        );
        for k in 0..3 {
            out[k] += (coloured[k] - out[k]) * lips;
        }
    }
    if let Some(b) = brow_index(face.region, brows, f) {
        out = retouch_brow(brows, s, b, src, out, from_u16(face.soft[f]));
    }
    let crisp = face.detail[f] as f32 / 255.0 * s.sharpen;
    if crisp > 0.0 {
        let soft = from_u16(face.soft[f]);
        for k in 0..3 {
            out[k] += 1.5 * crisp * (src[k] - soft[k]);
        }
    }
    out
}

/// Index in the brow layers of pixel `f` of the face region, if it lies
/// there.
fn brow_index(face: Region, brows: &BrowLayers, f: usize) -> Option<usize> {
    let w = face.w as usize;
    brows
        .region
        .index_at(face.x + (f % w) as u32, face.y + (f / w) as u32)
}

/// Brow sliders at brow pixel `b`: dye the hairs, darken them (and fill the
/// gaps between them a little, like brow powder) or lighten the brow toward
/// the skin beneath while its hairs still show, and crisp the whole brow
/// shape. `soft` is the photo's small blur there.
fn retouch_brow(
    brows: &BrowLayers,
    s: &PortraitSettings,
    b: usize,
    src: [f32; 3],
    mut out: [f32; 3],
    soft: [f32; 3],
) -> [f32; 3] {
    let (hair, area) = (brows.hair[b] as f32 / 255.0, brows.area[b] as f32 / 255.0);
    if area <= 0.0 {
        return out;
    }
    if s.brow_tint > 0.0 && hair > 0.0 {
        let dyed = colourise(out, s.brow_hue, 0.3);
        for k in 0..3 {
            out[k] += (dyed[k] - out[k]) * s.brow_tint * hair;
        }
    }
    if s.brows > 0.0 {
        let fill = 0.25 * area + 0.75 * hair;
        out = out.map(|v| v * (1.0 - 0.5 * s.brows * fill));
    } else if s.brows < 0.0 {
        let (skin, mean) = (from_u16(brows.skin[b]), from_u16(brows.mean[b]));
        let fade = -s.brows * area;
        for k in 0..3 {
            let toned = mean[k] + (skin[k] - mean[k]) * 0.9 * fade;
            out[k] = toned + (out[k] - mean[k]) * (1.0 - 0.5 * fade);
        }
    }
    if s.brow_sharpen > 0.0 {
        for k in 0..3 {
            out[k] += 2.0 * s.brow_sharpen * area * (src[k] - soft[k]);
        }
    }
    out
}

/// Develop's tone stage on a photo with only Blacks set, to the hair's lift
/// (`amount` 0..1 of the slider is Blacks 0..+200); `None` unless lifting.
fn hair_lift(amount: f32) -> Option<SceneToneData> {
    (amount > 0.0).then(|| {
        let develop = DevelopSettings {
            develop_engine_version: DevelopEngineVersion::Develop3,
            blacks: amount * CONTROL_LIMIT,
            ..DevelopSettings::default()
        };
        build_scene_tone_for(&develop, BaseLook::Identity)
    })
}

/// Hair lighter with Develop's own Blacks (`lift`, from [`hair_lift`]): the
/// photo's linear light scaled by the gain at the pixel's regional tone
/// `base`, as Develop reads it, so strands keep their texture and colour.
/// Darker with Develop's display Shadows and Blacks, also read at `base`.
/// Then dyed toward `hair_hue`.
fn recolour_hair(
    src: [f32; 3],
    base: f32,
    lift: Option<&SceneToneData>,
    s: &PortraitSettings,
) -> [f32; 3] {
    let [mut r, mut g, mut b] = src.map(|v| v.clamp(0.0, 1.0));
    if let Some(tone) = lift {
        let region = srgb_to_linear(base).max(SCENE_EV_MIN.exp2()).log2();
        [r, g, b] = tone.scene_to_display([r, g, b].map(srgb_to_linear), Some(region));
    } else if s.hair_brightness != 0.0 {
        let l = luminance_f32(r, g, b).clamp(0.0, 1.0);
        let amount = s.hair_brightness;
        let offset = apply_light_luma(base, 0.0, amount, 0.0, 0.5 * amount) - base;
        let target = (l + offset + local_detail_boost(l, base, offset)).clamp(0.0, 1.0);
        apply_luma_target(&mut r, &mut g, &mut b, target);
    }
    let mut out = [r, g, b];
    if s.hair_tint > 0.0 {
        let dyed = colourise(out, s.hair_hue, 0.35);
        for k in 0..3 {
            out[k] += (dyed[k] - out[k]) * s.hair_tint;
        }
    }
    out
}

/// Brush edits of one face's masks, used in place of the analysis's own.
/// `skin_paint` is the skin mask as painted, before the brows' share moved
/// to an edited brow area (`skin` is built from both).
#[derive(Clone, Default)]
pub struct FaceEdits {
    pub skin: Option<Arc<SkinLayers>>,
    pub skin_paint: Option<Arc<Vec<u8>>>,
    pub hair: Option<Arc<Vec<u8>>>,
    pub brows: Option<Arc<BrowLayers>>,
}

fn skin_of<'a>(face: &'a FaceModel, edit: Option<&'a FaceEdits>) -> &'a SkinLayers {
    edit.and_then(|e| e.skin.as_deref()).unwrap_or(&face.skin)
}

fn brows_of<'a>(face: &'a FaceModel, edit: Option<&'a FaceEdits>) -> &'a BrowLayers {
    edit.and_then(|e| e.brows.as_deref())
        .unwrap_or(face.brow_layers())
}

fn hair_of<'a>(face: &'a FaceModel, edit: Option<&'a FaceEdits>) -> &'a [u8] {
    edit.and_then(|e| e.hair.as_deref())
        .map_or(&face.hair[..], |h| &h[..])
}

/// The smallest rectangle holding every enabled face's skin region (which
/// holds its face region), and their hair regions when `hair` is set.
pub fn union_region(model: &PortraitModel, enabled: &[bool], hair: bool) -> Option<Region> {
    model
        .faces
        .iter()
        .zip(enabled.iter().chain(std::iter::repeat(&true)))
        .filter(|(face, &on)| on && !face.region.is_empty())
        .map(|(face, _)| {
            let r = face.skin.region;
            if hair {
                r.union(face.hair_region)
            } else {
                r
            }
        })
        .reduce(|a, b| a.union(b))
}

/// Retouch every enabled face of `rgba` (the analysed image), then reshape
/// it, and return the changed region with its new RGBA pixels.
pub fn render(
    rgba: &[u8],
    model: &PortraitModel,
    settings: &PortraitSettings,
    enabled: &[bool],
    edits: &[FaceEdits],
) -> Option<(Region, Vec<u8>)> {
    let retouched = retouch(rgba, model, settings, enabled, edits);
    reshape(
        rgba,
        model,
        &settings.face_shape(),
        &settings.body_shape(),
        enabled,
        retouched,
    )
}

/// The retouch alone: the union region of the enabled faces with its new
/// RGBA pixels. Faces add their own changes, so overlapping regions compose.
fn retouch(
    rgba: &[u8],
    model: &PortraitModel,
    settings: &PortraitSettings,
    enabled: &[bool],
    edits: &[FaceEdits],
) -> Option<(Region, Vec<u8>)> {
    let s = settings.unit();
    let union = union_region(model, enabled, s.hair_active())?;
    let width = model.width as usize;
    let (uw, uh) = (union.w as usize, union.h as usize);
    let mut out = vec![0u8; uw * uh * 4];
    out.par_chunks_mut(uw * 4)
        .enumerate()
        .for_each(|(row, line)| {
            let o = ((union.y as usize + row) * width + union.x as usize) * 4;
            line.copy_from_slice(&rgba[o..o + uw * 4]);
        });
    let pixel = |x: usize, y: usize| {
        let o = (y * width + x) * 4;
        [
            rgba[o] as f32 / 255.0,
            rgba[o + 1] as f32 / 255.0,
            rgba[o + 2] as f32 / 255.0,
        ]
    };
    let mut delta = vec![[0.0f32; 3]; uw * uh];
    for (index, (face, _)) in model
        .faces
        .iter()
        .zip(enabled.iter().chain(std::iter::repeat(&true)))
        .enumerate()
        .filter(|(_, (_, &on))| on)
    {
        let skin = skin_of(face, edits.get(index));
        let brows = brows_of(face, edits.get(index));
        let r = skin.region;
        let (sw, sx, sy) = (
            r.w as usize,
            (r.x - union.x) as usize,
            (r.y - union.y) as usize,
        );
        delta
            .par_chunks_mut(uw)
            .enumerate()
            .skip(sy)
            .take(r.h as usize)
            .for_each(|(urow, line)| {
                let row = urow - sy;
                for col in 0..sw {
                    let i = row * sw + col;
                    let (x, y) = (r.x as usize + col, r.y as usize + row);
                    let f = face.region.index_at(x as u32, y as u32);
                    if f.is_none() && skin.mask[i] == 0 {
                        continue;
                    }
                    let src = pixel(x, y);
                    let fetch = |dx: isize, dy: isize| {
                        pixel((x as isize + dx) as usize, (y as isize + dy) as usize)
                    };
                    let res = retouch_pixel(face, skin, brows, &s, i, f, src, &fetch);
                    let cell = &mut line[sx + col];
                    for k in 0..3 {
                        cell[k] += res[k] - src[k];
                    }
                }
            });
    }
    if s.hair_active() {
        let lift = hair_lift(s.hair_brightness);
        for (index, (face, _)) in model
            .faces
            .iter()
            .zip(enabled.iter().chain(std::iter::repeat(&true)))
            .enumerate()
            .filter(|(_, (face, &on))| on && !face.hair.is_empty())
        {
            let hair = hair_of(face, edits.get(index));
            let r = face.hair_region;
            let (hw, hx, hy) = (
                r.w as usize,
                (r.x - union.x) as usize,
                (r.y - union.y) as usize,
            );
            delta
                .par_chunks_mut(uw)
                .enumerate()
                .skip(hy)
                .take(r.h as usize)
                .for_each(|(urow, line)| {
                    let row = urow - hy;
                    for col in 0..hw {
                        let k = row * hw + col;
                        let weight = hair[k] as f32 / 255.0;
                        if weight <= 0.0 {
                            continue;
                        }
                        let src = pixel(r.x as usize + col, r.y as usize + row);
                        let base = face.hair_base[k] as f32 / 65535.0;
                        let res = recolour_hair(src, base, lift.as_ref(), &s);
                        let cell = &mut line[hx + col];
                        for k in 0..3 {
                            cell[k] += (res[k] - src[k]) * weight;
                        }
                    }
                });
        }
    }
    let clip = model.clip.as_ref();
    out.par_chunks_mut(4)
        .zip(delta.par_iter())
        .enumerate()
        .for_each(|(i, (px, d))| {
            let a = clip.map_or(1.0, |c| {
                c.at(union.x + (i % uw) as u32, union.y + (i / uw) as u32)
            });
            for k in 0..3 {
                px[k] = (px[k] as f32 + d[k] * 255.0 * a).round().clamp(0.0, 255.0) as u8;
            }
        });
    Some((union, out))
}

/// The photo with each detected area tinted (skin red, under-eye orange,
/// eye whites green, irises blue, brows yellow, lips pink, teeth cyan), so the
/// user can see where every slider acts; reshaped like the retouch.
pub fn render_masks(
    rgba: &[u8],
    model: &PortraitModel,
    settings: &PortraitSettings,
    enabled: &[bool],
    edits: &[FaceEdits],
) -> Option<(Region, Vec<u8>)> {
    let tinted = tint_masks(rgba, model, enabled, edits);
    reshape(
        rgba,
        model,
        &settings.face_shape(),
        &settings.body_shape(),
        enabled,
        tinted,
    )
}

fn tint_masks(
    rgba: &[u8],
    model: &PortraitModel,
    enabled: &[bool],
    edits: &[FaceEdits],
) -> Option<(Region, Vec<u8>)> {
    let union = union_region(model, enabled, true)?;
    let width = model.width as usize;
    let uw = union.w as usize;
    let mut out = vec![0u8; uw * union.h as usize * 4];
    out.par_chunks_mut(uw * 4)
        .enumerate()
        .for_each(|(row, line)| {
            let o = ((union.y as usize + row) * width + union.x as usize) * 4;
            line.copy_from_slice(&rgba[o..o + uw * 4]);
        });
    for (index, (face, _)) in model
        .faces
        .iter()
        .zip(enabled.iter().chain(std::iter::repeat(&true)))
        .enumerate()
        .filter(|(_, (_, &on))| on)
    {
        let (skin, hair, brows) = (
            skin_of(face, edits.get(index)),
            hair_of(face, edits.get(index)),
            brows_of(face, edits.get(index)),
        );
        let hr = face.hair_region;
        if !hair.is_empty() {
            let (hx, hy) = ((hr.x - union.x) as usize, (hr.y - union.y) as usize);
            out.par_chunks_mut(uw * 4)
                .enumerate()
                .skip(hy)
                .take(hr.h as usize)
                .for_each(|(urow, line)| {
                    let row = urow - hy;
                    for col in 0..hr.w as usize {
                        let inside = model
                            .clip
                            .as_ref()
                            .map_or(1.0, |c| c.at(hr.x + col as u32, hr.y + row as u32));
                        let a = hair[row * hr.w as usize + col] as f32 / 255.0 * 0.55 * inside;
                        if a > 0.0 {
                            let px = &mut line[(hx + col) * 4..(hx + col) * 4 + 3];
                            for (k, colour) in [150.0f32, 60.0, 255.0].iter().enumerate() {
                                px[k] = (px[k] as f32 * (1.0 - a) + colour * a).round() as u8;
                            }
                        }
                    }
                });
        }
        let r = skin.region;
        let (sx, sy) = ((r.x - union.x) as usize, (r.y - union.y) as usize);
        out.par_chunks_mut(uw * 4)
            .enumerate()
            .skip(sy)
            .take(r.h as usize)
            .for_each(|(urow, line)| {
                let row = urow - sy;
                for col in 0..r.w as usize {
                    let i = row * r.w as usize + col;
                    let f = face.region.index_at(r.x + col as u32, r.y + row as u32);
                    let feature = |layer: &[u8]| f.map_or(0, |f| layer[f]);
                    let tints: [(u8, [f32; 3]); 7] = [
                        (skin.mask[i], [255.0, 40.0, 40.0]),
                        (feature(&skin.under_eye), [255.0, 150.0, 0.0]),
                        (feature(&face.eye_white), [0.0, 255.0, 60.0]),
                        (feature(&face.iris), [40.0, 110.0, 255.0]),
                        (
                            f.and_then(|f| brow_index(face.region, brows, f))
                                .map_or(0, |b| brows.area[b]),
                            [255.0, 230.0, 0.0],
                        ),
                        (feature(&face.lips), [255.0, 0.0, 200.0]),
                        (feature(&face.teeth), [0.0, 230.0, 255.0]),
                    ];
                    let px = &mut line[(sx + col) * 4..(sx + col) * 4 + 4];
                    let inside = model
                        .clip
                        .as_ref()
                        .map_or(1.0, |c| c.at(r.x + col as u32, r.y + row as u32));
                    for (weight, colour) in tints {
                        let a = weight as f32 / 255.0 * 0.55 * inside;
                        if a > 0.0 {
                            for k in 0..3 {
                                px[k] = (px[k] as f32 * (1.0 - a) + colour[k] * a).round() as u8;
                            }
                        }
                    }
                }
            });
    }
    Some((union, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brows_stay_as_shot_unless_their_own_sliders_move() {
        for s in [PortraitSettings::default(), PortraitSettings::NEUTRAL] {
            assert_eq!((s.brows, s.brow_sharpen, s.brow_tint), (0.0, 0.0, 0.0));
        }
    }

    #[test]
    fn hair_tone_moves_dark_strands_and_spares_skin_tones() {
        let tone = |c: [f32; 3]| luminance_f32(c[0], c[1], c[2]);
        let strand = [0.22f32, 0.16, 0.12];
        let skin = [0.86f32, 0.68, 0.58];
        let darker = PortraitSettings {
            hair_brightness: -1.0,
            ..PortraitSettings::NEUTRAL
        };
        let dark = recolour_hair(strand, tone(strand), None, &darker);
        assert!(tone(dark) < tone(strand) - 0.05, "{dark:?}");
        let kept = recolour_hair(skin, tone(skin), None, &darker);
        assert!((tone(kept) - tone(skin)).abs() < 0.01, "{kept:?}");
        let none = recolour_hair(strand, tone(strand), None, &PortraitSettings::NEUTRAL);
        assert_eq!(none, strand);

        // Lighter is Develop's Blacks: deep strands lift, keeping their hue;
        // skin tones stay put.
        let lighter = PortraitSettings {
            hair_brightness: 1.0,
            ..PortraitSettings::NEUTRAL
        };
        let lift = hair_lift(1.0);
        let black = [0.1f32, 0.08, 0.07];
        let lit = recolour_hair(black, tone(black), lift.as_ref(), &lighter);
        assert!(tone(lit) > tone(black) + 0.05, "{lit:?}");
        assert!(lit[0] > lit[1] && lit[1] > lit[2], "hue kept: {lit:?}");
        let kept = recolour_hair(skin, tone(skin), lift.as_ref(), &lighter);
        assert!((tone(kept) - tone(skin)).abs() < 0.01, "{kept:?}");
        assert!(hair_lift(0.0).is_none() && hair_lift(-0.5).is_none());
    }

    #[test]
    fn hsl_round_trips_and_colourise_keeps_lightness() {
        for c in [[0.8f32, 0.3, 0.3], [0.2, 0.5, 0.9], [0.4, 0.4, 0.4]] {
            let back = hsl_to_rgb(rgb_to_hsl(c));
            for k in 0..3 {
                assert!((back[k] - c[k]).abs() < 1e-4, "{c:?} -> {back:?}");
            }
        }
        let brown = [0.35f32, 0.22, 0.12];
        let blue = colourise(brown, 220.0, 0.5);
        assert!(blue[2] > blue[0], "turned blue: {blue:?}");
        assert!((rgb_to_hsl(blue)[2] - rgb_to_hsl(brown)[2]).abs() < 1e-4);
    }
}

//! Slider settings and the cheap per-pixel recombination that turns a
//! [`PortraitModel`] into retouched pixels.

use std::sync::Arc;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::ai_detail::DetailAt;
use super::analysis::{luma, BrowLayers, FaceModel, PortraitModel, SkinLayers, BLEMISH_SCALE};
use super::body::BodySliders;
use super::clothes::{ClothesArea, ClothesDetail, ClothesLook};
use super::correct::{grey_axis, Fixes};
use super::geometry::Region;
use super::looks::StudioLook;
use super::reshape::{reshape, FaceShape};
use crate::core::color::luminance_f32;
use crate::core::develop::{
    apply_light_luma, apply_luma_target, local_detail_boost, srgb_to_linear, DevelopEngineVersion,
    DevelopSettings, CONTROL_LIMIT,
};
use crate::core::develop_scene::{build_scene_tone_for, BaseLook, SceneToneData, SCENE_EV_MIN};

/// Sliders run 0..100, the two-sided ones (the `*_saturation`s, skin, lip and
/// hair brightness, brows) -100..100, and the colour pickers (`*_hue`) are
/// target hues in degrees, 0..360, applied by the matching `*_tint` amount.
/// Brows have only their own sliders, all 0 by default: they stay as shot.
/// A new photo starts with the corrections of "Sửa màu & sáng", the "Trong
/// trẻo" look and the AI detail on.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PortraitSettings {
    pub smooth: f32,
    /// "Tạo khối": keep the face's shape (nose, folds, eye sockets) that
    /// strong smoothing flattens, and deepen its big forms a little. Layers
    /// saved before it existed read 0, as they were made.
    #[serde(default)]
    pub volume: f32,
    /// "Vân da": synthetic pore texture for flat, low-resolution skin.
    #[serde(default)]
    pub texture: f32,
    /// "Chi tiết mặt (AI)": how far the face's fine detail is the one a
    /// face restore model draws rather than the photo's own (see
    /// [`super::ai_detail`]).
    #[serde(default)]
    pub ai_detail: f32,
    /// "Sửa màu & sáng", the corrections of [`super::correct`]: how much of
    /// the measured cast, dimness and haze is taken out (0..100) and a
    /// manual cooler / warmer trim (-100..100).
    #[serde(default)]
    pub fix_cast: f32,
    #[serde(default)]
    pub fix_warmth: f32,
    #[serde(default)]
    pub fix_exposure: f32,
    #[serde(default)]
    pub fix_haze: f32,
    /// "Đều sáng da": how far skin in shade (under the chin, the neck, the
    /// side away from the lamp) is lifted toward the face's lit skin.
    #[serde(default)]
    pub even_light: f32,
    pub even_tone: f32,
    pub shine: f32,
    /// "Sáng da", -100..100: Develop's Midtones on the skin (see
    /// [`skin_tone`]).
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
    /// "Đậm / giảm màu", -100..100: left takes colour out of the hair, the
    /// eyes (whites and irises: sore red eyes, coloured lenses) and the brows
    /// before any of their own tints goes on; right deepens it (of the eyes,
    /// the irises only).
    #[serde(default)]
    pub hair_saturation: f32,
    #[serde(default)]
    pub eye_saturation: f32,
    #[serde(default)]
    pub brow_saturation: f32,
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
    /// "Màu studio": the look (index into [`StudioLook::ALL`], 0 = none) and
    /// how much of it is mixed in, 0..100. A layer saved before looks
    /// existed has none.
    #[serde(default)]
    pub look: u8,
    pub look_strength: f32,
    /// "Nét áo": how far the clothes worn in the photo are the ones an
    /// upscaling model draws sharp (see [`super::clothes`]). Left at 0 for a
    /// photo whose clothes are sharp as shot.
    #[serde(default)]
    pub clothes_sharpen: f32,
    /// "Sáng áo", -100..100: the clothes darker or lighter, and "Đều sáng
    /// áo": how far the slope of the light across them is taken out.
    #[serde(default)]
    pub clothes_brightness: f32,
    #[serde(default)]
    pub clothes_even: f32,
}

impl Default for PortraitSettings {
    fn default() -> Self {
        Self {
            smooth: 40.0,
            volume: 30.0,
            texture: 0.0,
            ai_detail: 60.0,
            fix_cast: AUTO_FIX.cast,
            fix_warmth: 0.0,
            fix_exposure: AUTO_FIX.exposure,
            fix_haze: AUTO_FIX.haze,
            even_light: AUTO_FIX.even_light,
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
            hair_saturation: 0.0,
            eye_saturation: 0.0,
            brow_saturation: 0.0,
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
            look: StudioLook::Clear.index(),
            look_strength: DEFAULT_LOOK_STRENGTH,
            clothes_sharpen: 0.0,
            clothes_brightness: 0.0,
            clothes_even: 0.0,
        }
    }
}

pub const DEFAULT_LOOK_STRENGTH: f32 = 70.0;

/// What "Tự động" sets the "Sửa màu & sáng" sliders to: the usual amounts
/// for a phone photo, and where a new photo starts.
pub struct AutoFix {
    pub cast: f32,
    pub exposure: f32,
    pub haze: f32,
    pub even_light: f32,
}

pub const AUTO_FIX: AutoFix = AutoFix {
    cast: 100.0,
    exposure: 80.0,
    haze: 60.0,
    even_light: 60.0,
};

impl PortraitSettings {
    pub const NEUTRAL: Self = Self {
        smooth: 0.0,
        volume: 0.0,
        texture: 0.0,
        ai_detail: 0.0,
        fix_cast: 0.0,
        fix_warmth: 0.0,
        fix_exposure: 0.0,
        fix_haze: 0.0,
        even_light: 0.0,
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
        hair_saturation: 0.0,
        eye_saturation: 0.0,
        brow_saturation: 0.0,
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
        look: 0,
        look_strength: DEFAULT_LOOK_STRENGTH,
        clothes_sharpen: 0.0,
        clothes_brightness: 0.0,
        clothes_even: 0.0,
    };

    /// The settings a layer was saved with. The one-sided "Giảm màu" sliders
    /// of an earlier build (`*_fade`, 0..100) read as the left half of
    /// today's two-sided ones.
    pub fn from_saved(mut saved: serde_json::Value) -> Option<Self> {
        if let Some(map) = saved.as_object_mut() {
            for (old, new) in [
                ("hair_fade", "hair_saturation"),
                ("iris_fade", "eye_saturation"),
                ("brow_fade", "brow_saturation"),
            ] {
                let fade = map.remove(old).and_then(|v| v.as_f64());
                if let Some(fade) = fade.filter(|_| !map.contains_key(new)) {
                    map.insert(new.to_string(), (-fade).into());
                }
            }
        }
        serde_json::from_value(saved).ok()
    }

    fn unit(&self) -> Self {
        let u = |v: f32| (v / 100.0).clamp(0.0, 1.0);
        let both = |v: f32| (v / 100.0).clamp(-1.0, 1.0);
        Self {
            smooth: u(self.smooth),
            volume: u(self.volume),
            texture: u(self.texture),
            ai_detail: u(self.ai_detail),
            even_light: u(self.even_light),
            even_tone: u(self.even_tone),
            shine: u(self.shine),
            brighten: both(self.brighten),
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
            hair_saturation: both(self.hair_saturation),
            eye_saturation: both(self.eye_saturation),
            brow_saturation: both(self.brow_saturation),
            clothes_sharpen: u(self.clothes_sharpen),
            clothes_brightness: both(self.clothes_brightness),
            clothes_even: u(self.clothes_even),
            ..*self
        }
    }

    /// Whether a clothes slider is away from rest: the clothes must be found.
    pub fn clothes_active(&self) -> bool {
        self.clothes_sharpen > 0.0 || self.clothes_even > 0.0 || self.clothes_brightness != 0.0
    }

    /// The chosen studio look and its strength 0..1, when one is on.
    pub fn studio_look(&self) -> Option<(StudioLook, f32)> {
        let look = StudioLook::from_index(self.look);
        let strength = (self.look_strength / 100.0).clamp(0.0, 1.0);
        (look != StudioLook::None && strength > 0.0).then_some((look, strength))
    }

    /// The corrections of "Sửa màu & sáng" that are on, if any.
    pub fn fixes(&self) -> Option<Fixes> {
        let fixes = Fixes {
            cast: (self.fix_cast / 100.0).clamp(0.0, 1.0),
            warmth: (self.fix_warmth / 100.0).clamp(-1.0, 1.0),
            exposure: (self.fix_exposure / 100.0).clamp(0.0, 1.0),
            haze: (self.fix_haze / 100.0).clamp(0.0, 1.0),
        };
        (!fixes.is_neutral()).then_some(fixes)
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
        self.hair_brightness != 0.0 || self.hair_tint > 0.0 || self.hair_saturation != 0.0
    }

    /// The "Sửa màu & sáng" sliders at their usual amounts ("Tự động").
    pub fn with_auto_fix(self) -> Self {
        Self {
            fix_cast: AUTO_FIX.cast,
            fix_exposure: AUTO_FIX.exposure,
            fix_haze: AUTO_FIX.haze,
            even_light: AUTO_FIX.even_light,
            ..self
        }
    }
}

/// How far "Tạo khối" at 100 deepens the big forms' light and shade.
const VOLUME_GAIN: f32 = 0.45;
/// Luminance swing of "Vân da" at 100.
const TEXTURE_GAIN: f32 = 0.10;

/// What smoothing leaves of the skin's mid band.
fn keep_mid(s: &PortraitSettings, inside: f32) -> f32 {
    1.0 - 0.85 * s.smooth * inside
}

/// What smoothing leaves of the finest grain: near the top of the slider it
/// goes too (phone noise); "Vân da" and "Chi tiết mặt (AI)" can lay clean
/// texture back.
fn keep_fine(s: &PortraitSettings, inside: f32) -> f32 {
    1.0 - (0.15 * s.smooth + 0.45 * s.smooth.powi(3)) * inside
}

/// What "Chi tiết mặt (AI)" adds to a pixel still holding `kept` of the
/// photo's own detail: the model's detail in its place, as far as the slider
/// goes.
fn ai_detail_swap(s: &PortraitSettings, detail: &DetailAt, kept: f32) -> [f32; 3] {
    std::array::from_fn(|k| s.ai_detail * (detail.model[k] - kept * detail.photo[k]))
}

/// Shade this shallow (ln of brightness) is the face's own modelling (eye
/// sockets, the sides of the nose) and is left alone by "Đều sáng da".
const EVEN_TOLERANCE: f32 = 0.12;
/// At 100, how much of the shade beyond that is lifted, and how much of
/// what is brighter than the lit skin is eased.
const EVEN_LIFT: f32 = 0.9;
const EVEN_EASE: f32 = 0.35;
/// The most it changes brightness, in ln units.
const EVEN_LIMIT: f32 = 1.2;

/// What "Đều sáng da" multiplies pixel `i` of the skin region by: skin in
/// shade (under the chin, the neck, the side away from the lamp) is lifted
/// toward the face's lit skin, and brighter skin eased a little toward it.
/// It follows the skin's broad brightness, so texture and edges keep their
/// contrast.
fn even_light_gain(skin: &SkinLayers, s: &PortraitSettings, i: usize) -> f32 {
    if s.even_light <= 0.0 || skin.lit <= 0.0 {
        return 1.0;
    }
    let broad = (skin.broad[i] as f32 / 65535.0).max(0.02);
    let shade = (skin.lit / broad).ln();
    let beyond = shade - EVEN_TOLERANCE * (shade / EVEN_TOLERANCE).tanh();
    let share = if beyond > 0.0 { EVEN_LIFT } else { EVEN_EASE };
    (s.even_light * share * beyond)
        .clamp(-EVEN_LIMIT, EVEN_LIMIT)
        .exp()
}

/// Pore spacing for a face `extent` pixels from forehead to chin: about 1/350
/// of it, never finer than a pixel.
fn pore_period(extent: f32) -> f32 {
    (extent / 350.0).max(1.0)
}

/// A fixed pseudo-random value in 0..1 for lattice point (x, y).
fn lattice(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343)
        ^ (y as u32).wrapping_mul(0xd816_3841)
        ^ seed.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0xff_ffff) as f32 / 0x100_0000 as f32
}

/// Smooth value noise in −1..1 with features `period` pixels apart.
fn value_noise(x: f32, y: f32, period: f32, seed: u32) -> f32 {
    // Off the lattice even at a 1-pixel period, where every sample would
    // otherwise land on a lattice point.
    let (u, v) = (x / period + 0.37, y / period + 0.61);
    let (x0, y0) = (u.floor(), v.floor());
    let ease = |t: f32| t * t * (3.0 - 2.0 * t);
    let (tx, ty) = (ease(u - x0), ease(v - y0));
    let (x0, y0) = (x0 as i32, y0 as i32);
    let a = lattice(x0, y0, seed) * (1.0 - tx) + lattice(x0 + 1, y0, seed) * tx;
    let b = lattice(x0, y0 + 1, seed) * (1.0 - tx) + lattice(x0 + 1, y0 + 1, seed) * tx;
    (a * (1.0 - ty) + b * ty) * 2.0 - 1.0
}

/// Skin texture in about −1..1 at image pixel (x, y): pore dots and the
/// lighter ridges between them, a finer grain and a faint larger mottling.
/// Odd in every noise, so it averages to zero at any scale (the skin keeps its
/// brightness), and fixed per position, so the preview and the applied layer
/// match.
fn pores(x: f32, y: f32, period: f32) -> f32 {
    let dots = value_noise(x, y, period, 1);
    let grain = value_noise(x, y, period * 0.5, 2);
    let mottle = value_noise(x, y, period * 2.3, 3);
    -1.4 * dots * dots.abs() + 0.35 * grain + 0.2 * mottle
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

    if s.volume > 0.0 {
        // The big forms (cheekbones, brow, jaw) a little deeper, from the
        // photo's own light.
        let broad = skin.broad[i] as f32 / 65535.0;
        let huge = skin.huge[i] as f32 / 65535.0;
        low_y += s.volume * VOLUME_GAIN * (broad - huge) * inside;
    }

    let keep_mid = keep_mid(s, inside);
    let keep_fine = keep_fine(s, inside);
    let (mid_y, mid_c) = split(mid);
    // The coarser half of the mid band's light is the face's shape (nose,
    // folds, eye sockets); "Tạo khối" keeps it while the grain still goes.
    let shape_y = skin.form[i] as f32 / 65535.0 - luma(l2);
    let keep_shape = keep_mid + (1.0 - keep_mid) * s.volume;
    let low = join(low_y, low_c);
    let mut r = [0.0f32; 3];
    for k in 0..3 {
        r[k] = low[k]
            + (mid_y - shape_y) * keep_mid
            + shape_y * keep_shape
            + mid_c[k] * keep_mid * (1.0 - 0.6 * s.even_tone * inside)
            + fine[k] * keep_fine;
    }

    if s.texture > 0.0 {
        let w = skin.region.w as usize;
        let (x, y) = (
            (skin.region.x as usize + i % w) as f32,
            (skin.region.y as usize + i / w) as f32,
        );
        let y_now = luma(r).clamp(0.0, 1.0);
        // Pores show in the midtones, not in deep shade or bright highlights.
        let amount = s.texture * TEXTURE_GAIN * inside * 4.0 * y_now * (1.0 - y_now);
        let grain = 1.0 + amount * pores(x, y, pore_period(skin.extent));
        r = r.map(|v| v * grain);
    }

    let y = luma(r);
    let lift = y - skin.broad[i] as f32 / 65535.0;
    let shine = s.shine * smoothstep(0.03, 0.18, lift) * smoothstep(0.45, 0.8, y);
    if shine > 0.0 {
        for v in r.iter_mut() {
            *v -= shine * lift * 0.75;
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

/// How far "Đậm / giảm màu" at +100 deepens a colour.
const SATURATION_GAIN: f32 = 0.8;

/// `c` at the same brightness with its colour scaled by a two-sided
/// `amount`: -1 takes all of it out, +1 deepens it by [`SATURATION_GAIN`].
fn saturated(c: [f32; 3], amount: f32) -> [f32; 3] {
    if amount == 0.0 {
        return c;
    }
    let gain = if amount < 0.0 {
        1.0 + amount.max(-1.0)
    } else {
        1.0 + SATURATION_GAIN * amount.min(1.0)
    };
    let (y, chroma) = split(c);
    join(y, chroma.map(|v| v * gain))
}

/// `change` made to `c` as the corrections will balance it: `grey` is the
/// photo's colour for neutral ([`grey_axis`]), so colour taken out leaves
/// that, not a grey the white balance then tints.
fn balanced(c: [f32; 3], grey: [f32; 3], change: impl Fn([f32; 3]) -> [f32; 3]) -> [f32; 3] {
    let out = change(std::array::from_fn(|k| c[k] / grey[k]));
    std::array::from_fn(|k| out[k] * grey[k])
}

/// A white feature (the white of an eye, teeth) brighter by `lift` with
/// `fade` of its colour taken out.
fn bleached(c: [f32; 3], lift: f32, fade: f32) -> [f32; 3] {
    let (y, chroma) = split(c);
    join(y * (1.0 + lift), chroma.map(|v| v * (1.0 - fade)))
}

/// How far a reddened eye white is lifted from its luma toward its brightest
/// channel as its colour goes.
const WHITE_LIFT: f32 = 0.75;

/// The white of an eye with `amount` (0..1) of its colour taken out. Blood
/// (veins, a sore red eye) dims green and blue and hardly red, so the white
/// beneath is brighter than the luma: a plain fade would leave grey patches.
fn whitened(c: [f32; 3], amount: f32) -> [f32; 3] {
    let y = luma(c);
    let level = y + WHITE_LIFT * (c[0].max(c[1]).max(c[2]) - y);
    c.map(|v| v + (level - v) * amount.clamp(0.0, 1.0))
}

/// Recolour to `hue` (degrees) keeping lightness, with at least `saturation`
/// so even grey-brown features take the colour.
fn colourise(c: [f32; 3], hue: f32, saturation: f32) -> [f32; 3] {
    let [_, s, l] = rgb_to_hsl(c.map(|v| v.clamp(0.0, 1.0)));
    hsl_to_rgb([hue, s.max(saturation), l])
}

/// What a render works out once from the sliders and the photo's light.
struct Shared {
    /// The photo's neutral ([`grey_axis`]).
    grey: [f32; 3],
    /// Develop's Midtones for "Sáng da", unless it is at rest.
    skin_tone: Option<SceneToneData>,
}

/// The retouched colour of skin-region pixel `i`, which is pixel `f` of the
/// face region when it lies there. `src` is the photo in 0..1; `fetch(dx,
/// dy)` reads the photo at an offset from this pixel; `ai` is the restore
/// model's detail here, once made.
#[allow(clippy::too_many_arguments)]
fn retouch_pixel(
    face: &FaceModel,
    skin: &SkinLayers,
    brows: &BrowLayers,
    s: &PortraitSettings,
    shared: &Shared,
    i: usize,
    f: Option<usize>,
    src: [f32; 3],
    fetch: &dyn Fn(isize, isize) -> [f32; 3],
    ai: Option<DetailAt>,
) -> [f32; 3] {
    let m = skin.mask[i] as f32 / 255.0;
    let ai = ai.filter(|_| s.ai_detail > 0.0);
    let mut out = src;
    let l2 = from_u16(skin.low2[i]);
    if m > 0.0 {
        let inside = skin.interior[i] as f32 / 255.0;
        let l1 = from_u16(skin.low1[i]);
        let under = f.map_or(0.0, |f| skin.under_eye[f] as f32 / 255.0);
        let mut r = skin_result(skin, s, i, under, src, l1, l2, inside);
        let cover = f.map_or(0.0, |f| face.spot_cover[f] as f32 / 255.0);
        let mut healed = 0.0;
        if let Some(f) = f.filter(|_| s.blemish > 0.0 && cover > 0.0) {
            let score = face.spot_score[f] as f32 / BLEMISH_SCALE;
            let threshold = 1.5 - 1.15 * s.blemish;
            let spot = smoothstep(threshold * 0.85, threshold * 1.15, score) * cover * inside;
            healed = spot;
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
        let light = even_light_gain(skin, s, i);
        if light != 1.0 {
            r = r.map(|v| v * light);
        }
        if let Some(detail) = &ai {
            // Smoothing has taken part of the photo's detail already. A
            // healed spot has its donor's instead: no swap there.
            let kept = 0.5 * (keep_mid(s, inside) + keep_fine(s, inside));
            let add = ai_detail_swap(s, detail, kept);
            for k in 0..3 {
                r[k] += add[k] * (1.0 - healed);
            }
        }
        if let Some(tone) = &shared.skin_tone {
            // Read at the skin's tone around, as the evened light left it.
            r = midtoned(r, l2.map(|v| v * light), tone);
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
    if let Some(detail) = &ai {
        // What is not skin inside the face outline: eyes, brows, lips,
        // glasses, beard.
        let share = (detail.face - m).max(0.0);
        if share > 0.0 {
            let add = ai_detail_swap(s, detail, 1.0);
            for k in 0..3 {
                out[k] += add[k] * share;
            }
        }
    }
    // The features lie in the face region.
    let Some(f) = f else {
        return out;
    };
    let grey = shared.grey;
    let brow = brow_index(face.region, brows, f);
    let light = even_light_gain(skin, s, i);
    // Eyes, brows and lips take the evened light with the skin around.
    let feature = face.detail[f].max(brow.map_or(0, |b| brows.area[b]));
    if light != 1.0 {
        let share = (feature as f32 / 255.0 - m).max(0.0);
        out = out.map(|v| v * (1.0 + (light - 1.0) * share));
    }
    if let Some(tone) = &shared.skin_tone {
        // And the skin's tone, the nostrils too: left as shot they would
        // stand out of skin made darker. Each by its own tone, so lashes,
        // irises and hair over an eye stay dark.
        let share = (feature.max(face.nostrils[f]) as f32 / 255.0).min(1.0 - m);
        if share > 0.0 {
            let around = from_u16(face.soft[f]).map(|v| v * light);
            let toned = midtoned(out, around, tone);
            for k in 0..3 {
                out[k] += (toned[k] - out[k]) * share;
            }
        }
    }
    let sclera = face.sclera[f] as f32 / 255.0;
    if s.eye_saturation < 0.0 && sclera > 0.0 {
        // The whole eye loses colour, its white by its own rule and before
        // "Trắng mắt" takes some of the red that rule reads.
        out = balanced(out, grey, |c| whitened(c, -s.eye_saturation * sclera));
    }
    let white = face.eye_white[f] as f32 / 255.0 * s.eye_white;
    if white > 0.0 {
        out = balanced(out, grey, |c| bleached(c, 0.15 * white, 0.75 * white));
    }
    let iris = face.iris[f] as f32 / 255.0 * s.iris;
    if iris > 0.0 {
        let (y, c) = split(out);
        out = join(y * (1.0 + 0.18 * iris), c.map(|v| v * (1.0 + 0.35 * iris)));
    }
    let iris_colour = s.eye_saturation * face.iris[f] as f32 / 255.0;
    if iris_colour != 0.0 {
        out = balanced(out, grey, |c| saturated(c, iris_colour));
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
        out = balanced(out, grey, |c| bleached(c, 0.1 * teeth, 0.8 * teeth));
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
    if let Some(b) = brow {
        out = retouch_brow(brows, s, grey, b, src, out, from_u16(face.soft[f]));
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
    grey: [f32; 3],
    b: usize,
    src: [f32; 3],
    mut out: [f32; 3],
    soft: [f32; 3],
) -> [f32; 3] {
    let (hair, area) = (brows.hair[b] as f32 / 255.0, brows.area[b] as f32 / 255.0);
    if area <= 0.0 {
        return out;
    }
    // The hairs fully, the skin between them a little.
    let colour = s.brow_saturation * (0.25 * area + 0.75 * hair);
    if colour != 0.0 {
        out = balanced(out, grey, |c| saturated(c, colour));
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

/// Develop's tone stage on a photo with only Midtones set, for "Sáng da"
/// (`amount` -1..1 of the slider is Midtones -200..+200); `None` at rest.
fn skin_tone(amount: f32) -> Option<SceneToneData> {
    (amount != 0.0).then(|| {
        let develop = DevelopSettings {
            develop_engine_version: DevelopEngineVersion::Develop3,
            midtones: amount * CONTROL_LIMIT,
            ..DevelopSettings::default()
        };
        build_scene_tone_for(&develop, BaseLook::Identity)
    })
}

/// Skin lighter or darker with Develop's own Midtones (`tone`, from
/// [`skin_tone`]): the linear light of `c` scaled by the gain at the tone of
/// the skin around it, `region`, as Develop reads it. Mid tones move most,
/// deep shade and highlights hardly, and pores and lines keep their contrast.
fn midtoned(c: [f32; 3], region: [f32; 3], tone: &SceneToneData) -> [f32; 3] {
    let linear = |c: [f32; 3]| c.map(|v| srgb_to_linear(v.clamp(0.0, 1.0)));
    tone.scene_to_display(linear(c), Some(tone.own_e(linear(region))))
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
/// Then its colour faded toward `grey` ([`grey_axis`]) or deepened, and
/// dyed toward `hair_hue`.
fn recolour_hair(
    src: [f32; 3],
    base: f32,
    lift: Option<&SceneToneData>,
    s: &PortraitSettings,
    grey: [f32; 3],
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
    if s.hair_saturation != 0.0 {
        out = balanced(out, grey, |c| saturated(c, s.hair_saturation));
    }
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
    pub clothes: Option<Arc<ClothesArea>>,
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

/// Where the clothes of `face` lie, as painted or else as found, once found.
fn clothes_of<'a>(face: &'a FaceModel, edit: Option<&'a FaceEdits>) -> Option<&'a ClothesArea> {
    edit.and_then(|e| e.clothes.as_deref())
        .or_else(|| face.clothes_area.get().and_then(|c| c.as_ref().ok()))
}

/// The clothes of `face` as the upscaling model drew them, once made.
fn drawn_clothes(face: &FaceModel) -> Option<&ClothesDetail> {
    face.clothes.get().and_then(|c| c.as_ref().ok())
}

/// The smallest rectangle holding every enabled face's skin region (which
/// holds its face region), their hair regions when `hair` is set and their
/// clothes when `clothes` is.
pub fn union_region(
    model: &PortraitModel,
    enabled: &[bool],
    edits: &[FaceEdits],
    hair: bool,
    clothes: bool,
) -> Option<Region> {
    model
        .faces
        .iter()
        .enumerate()
        .zip(enabled.iter().chain(std::iter::repeat(&true)))
        .filter(|((_, face), &on)| on && !face.region.is_empty())
        .map(|((index, face), _)| {
            let mut r = face.skin.region;
            if hair {
                r = r.union(face.hair_region);
            }
            match clothes_of(face, edits.get(index)).filter(|_| clothes) {
                Some(worn) => r.union(worn.bounds()),
                None => r,
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
    let shared = Shared {
        grey: grey_axis(&model.light, settings.fixes().as_ref()),
        skin_tone: skin_tone(s.brighten),
    };
    // Hair takes its own sliders, and the AI detail where the model saw it.
    let hair_detail = s.ai_detail > 0.0
        && model
            .faces
            .iter()
            .any(|f| !f.hair.is_empty() && matches!(f.ai_detail.get(), Some(Ok(_))));
    let hair_pass = s.hair_active() || hair_detail;
    let clothes = ClothesLook {
        sharpen: s.clothes_sharpen,
        even: s.clothes_even,
        brightness: s.clothes_brightness,
    };
    let union = union_region(model, enabled, edits, hair_pass, !clothes.at_rest())?;
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
        let detail = face.ai_detail.get().and_then(|d| d.as_ref().ok());
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
                    let ai = detail.and_then(|d| d.at(x as u32, y as u32));
                    let res = retouch_pixel(face, skin, brows, &s, &shared, i, f, src, &fetch, ai);
                    let cell = &mut line[sx + col];
                    for k in 0..3 {
                        cell[k] += res[k] - src[k];
                    }
                }
            });
    }
    if hair_pass {
        let lift = hair_lift(s.hair_brightness);
        let recolour = s.hair_active();
        for (index, (face, _)) in model
            .faces
            .iter()
            .zip(enabled.iter().chain(std::iter::repeat(&true)))
            .enumerate()
            .filter(|(_, (face, &on))| on && !face.hair.is_empty())
        {
            let hair = hair_of(face, edits.get(index));
            let skin = skin_of(face, edits.get(index));
            let detail = face
                .ai_detail
                .get()
                .and_then(|d| d.as_ref().ok())
                .filter(|_| s.ai_detail > 0.0);
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
                        let (x, y) = (r.x as usize + col, r.y as usize + row);
                        let cell = &mut line[hx + col];
                        if recolour {
                            let src = pixel(x, y);
                            let base = face.hair_base[k] as f32 / 65535.0;
                            let res = recolour_hair(src, base, lift.as_ref(), &s, shared.grey);
                            for k in 0..3 {
                                cell[k] += (res[k] - src[k]) * weight;
                            }
                        }
                        if let Some(d) = detail.and_then(|d| d.hair_at(x as u32, y as u32)) {
                            // The skin pass has swapped the skin's share and
                            // what lies inside the face outline.
                            let m = skin
                                .region
                                .index_at(x as u32, y as u32)
                                .map_or(0.0, |i| skin.mask[i] as f32 / 255.0);
                            let share = (weight - m.max(d.face)).max(0.0);
                            if share > 0.0 {
                                let add = ai_detail_swap(&s, &d, 1.0);
                                for k in 0..3 {
                                    cell[k] += add[k] * share;
                                }
                            }
                        }
                    }
                });
        }
    }
    if !clothes.at_rest() {
        for (index, (face, _)) in model
            .faces
            .iter()
            .zip(enabled.iter().chain(std::iter::repeat(&true)))
            .enumerate()
            .filter(|(_, (_, &on))| on)
        {
            if let Some(worn) = clothes_of(face, edits.get(index)) {
                worn.lay(&mut delta, union, &clothes, drawn_clothes(face), &pixel);
            }
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
/// eye whites green, irises blue, brows yellow, lips pink, teeth cyan, hair
/// violet, and the clothes teal once they are found), so the user
/// can see where every slider acts; reshaped like the retouch.
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
    let union = union_region(model, enabled, edits, true, true)?;
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
        if let Some(clothes) = clothes_of(face, edits.get(index)) {
            let r = clothes.bounds();
            let (cx, cy) = ((r.x - union.x) as usize, (r.y - union.y) as usize);
            out.par_chunks_mut(uw * 4)
                .skip(cy)
                .take(r.h as usize)
                .enumerate()
                .for_each(|(row, line)| {
                    for col in 0..r.w as usize {
                        let (x, y) = (r.x + col as u32, r.y + row as u32);
                        let inside = model.clip.as_ref().map_or(1.0, |c| c.at(x, y));
                        let level = clothes
                            .region
                            .index_at(x, y)
                            .map_or(0, |k| clothes.mask()[k]);
                        let a = level as f32 / 255.0 * 0.55 * inside;
                        if a > 0.0 {
                            let px = &mut line[(cx + col) * 4..(cx + col) * 4 + 3];
                            for (k, colour) in [0.0f32, 150.0, 130.0].iter().enumerate() {
                                px[k] = (px[k] as f32 * (1.0 - a) + colour * a).round() as u8;
                            }
                        }
                    }
                });
        }
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

    /// One skin pixel whose mid band holds 0.08 of shape and 0.02 of grain.
    fn one_pixel_skin() -> SkinLayers {
        let u16v = |v: f32| (v * 65535.0).round() as u16;
        SkinLayers {
            region: Region {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
            mask: vec![255],
            interior: vec![255],
            under_eye: vec![0],
            low1: vec![[u16v(0.5); 3]],
            low2: vec![[u16v(0.4); 3]],
            broad: vec![u16v(0.4)],
            form: vec![u16v(0.48)],
            huge: vec![u16v(0.4)],
            mean: [0.4; 3],
            cheek_luma: 0.4,
            extent: 300.0,
            lit: 0.0,
        }
    }

    #[test]
    fn volume_keeps_the_shape_that_strong_smoothing_removes() {
        let skin = one_pixel_skin();
        let src = [0.5f32; 3];
        let (l1, l2) = ([0.5f32; 3], [0.4f32; 3]);
        let result = |volume: f32| {
            let s = PortraitSettings {
                smooth: 100.0,
                volume,
                ..PortraitSettings::NEUTRAL
            }
            .unit();
            luma(skin_result(&skin, &s, 0, 0.0, src, l1, l2, 1.0))
        };
        // Smoothing alone keeps 15% of the whole mid band.
        assert!((result(0.0) - 0.415).abs() < 2e-3, "{}", result(0.0));
        // "Tạo khối" 100 keeps all the shape, still drops 85% of the grain.
        assert!((result(100.0) - 0.483).abs() < 2e-3, "{}", result(100.0));
    }

    #[test]
    fn settings_saved_before_volume_and_texture_read_them_as_zero() {
        let mut old = serde_json::to_value(PortraitSettings::default()).unwrap();
        let map = old.as_object_mut().unwrap();
        for later in [
            "volume",
            "texture",
            "ai_detail",
            "fix_cast",
            "fix_exposure",
            "fix_haze",
            "even_light",
            "look",
            "hair_saturation",
        ] {
            map.remove(later);
        }
        let read = PortraitSettings::from_saved(old).unwrap();
        assert_eq!((read.volume, read.texture, read.ai_detail), (0.0, 0.0, 0.0));
        assert!(read.fixes().is_none() && read.studio_look().is_none());
        assert_eq!((read.even_light, read.hair_saturation), (0.0, 0.0));
        assert_eq!(read.smooth, PortraitSettings::default().smooth);
    }

    #[test]
    fn one_sided_fades_read_as_the_left_half_of_the_two_sided_sliders() {
        let mut old = serde_json::to_value(PortraitSettings::NEUTRAL).unwrap();
        let map = old.as_object_mut().unwrap();
        for now in ["hair_saturation", "eye_saturation", "brow_saturation"] {
            map.remove(now);
        }
        map.insert("hair_fade".into(), 80.0.into());
        map.insert("iris_fade".into(), 30.0.into());
        map.insert("brow_fade".into(), 0.0.into());
        let read = PortraitSettings::from_saved(old).unwrap();
        assert_eq!(
            (
                read.hair_saturation,
                read.eye_saturation,
                read.brow_saturation
            ),
            (-80.0, -30.0, 0.0)
        );
        // What is saved today reads back as it is.
        let now = PortraitSettings {
            hair_saturation: 40.0,
            eye_saturation: -100.0,
            ..PortraitSettings::default()
        };
        let read = PortraitSettings::from_saved(serde_json::to_value(now).unwrap());
        assert_eq!(read, Some(now));
    }

    #[test]
    fn ai_detail_swaps_the_photos_detail_for_the_models() {
        let detail = DetailAt {
            model: [0.05, 0.04, 0.03],
            photo: [-0.02, 0.01, 0.0],
            face: 1.0,
        };
        let at = |amount: f32| {
            PortraitSettings {
                ai_detail: amount,
                ..PortraitSettings::NEUTRAL
            }
            .unit()
        };
        // At 100 a pixel holding the photo's detail ends up with the model's.
        let full = ai_detail_swap(&at(100.0), &detail, 1.0);
        for k in 0..3 {
            let after = detail.photo[k] + full[k];
            assert!((after - detail.model[k]).abs() < 1e-6);
        }
        // Where smoothing left a quarter of the photo's detail, only that
        // quarter is taken back out.
        let smoothed = ai_detail_swap(&at(100.0), &detail, 0.25);
        assert!((smoothed[0] - (0.05 + 0.25 * 0.02)).abs() < 1e-6);
        // Halfway is half of it; 0 is nothing.
        let half = ai_detail_swap(&at(50.0), &detail, 1.0);
        assert!((half[0] - 0.5 * full[0]).abs() < 1e-6);
        assert_eq!(ai_detail_swap(&at(0.0), &detail, 1.0), [0.0; 3]);
    }

    /// Opt-in visual probe: IAI_PORTRAIT_DETAIL_PROBE is a folder of photos;
    /// each gets `detail_<name>.png`, face 0 at least 420 pixels wide: as
    /// shot | the default retouch | + Chi tiết mặt (AI) 50 | + 100 |
    /// + 100 and Vân da 50 | AI 100 alone.
    #[test]
    #[ignore]
    fn probe_ai_detail() {
        let Ok(dir) = std::env::var("IAI_PORTRAIT_DETAIL_PROBE") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if name.starts_with("detail_") || !(name.ends_with(".jpg") || name.ends_with(".png")) {
                continue;
            }
            let rgba = image::open(&path).unwrap().to_rgba8();
            let (w, h) = rgba.dimensions();
            let rgba = rgba.into_raw();
            let model = super::super::analyze(&rgba, w, h, false, None, &|_| {}).unwrap();
            if model.faces.is_empty() {
                println!("{name}: no face");
                continue;
            }
            let enabled = vec![true; model.faces.len()];
            let base = PortraitSettings::default();
            let half = PortraitSettings {
                ai_detail: 50.0,
                ..base
            };
            let full_ai = PortraitSettings {
                ai_detail: 100.0,
                ..base
            };
            let pores = PortraitSettings {
                texture: 50.0,
                ..full_ai
            };
            let alone = PortraitSettings {
                ai_detail: 100.0,
                ..PortraitSettings::NEUTRAL
            };
            let full = |settings: &PortraitSettings| {
                let mut out = rgba.clone();
                if let Some((r, px)) = render(&rgba, &model, settings, &enabled, &[]) {
                    for y in 0..r.h as usize {
                        let o = ((r.y as usize + y) * w as usize + r.x as usize) * 4;
                        out[o..o + r.w as usize * 4]
                            .copy_from_slice(&px[y * r.w as usize * 4..(y + 1) * r.w as usize * 4]);
                    }
                }
                out
            };
            // Before the model's layer exists the slider changes nothing.
            let before = full(&full_ai);
            assert!(before == full(&base), "{name}: detail before analysis");
            let started = std::time::Instant::now();
            super::super::ai_detail::analyze_details(&rgba, &model, &enabled);
            let seconds = started.elapsed().as_secs_f32();
            if let Some(Err(error)) = model.faces[0].ai_detail.get() {
                println!("{name}: {error}");
                continue;
            }
            let views = [
                rgba.clone(),
                full(&base),
                full(&half),
                full(&full_ai),
                full(&pores),
                full(&alone),
            ];
            let r = model.faces[0].region;
            let zoom = (420.0 / r.w as f32).ceil().max(1.0) as u32;
            let mut sheet =
                image::RgbaImage::new((r.w * zoom + 8) * views.len() as u32, r.h * zoom);
            for (k, view) in views.iter().enumerate() {
                for y in 0..r.h * zoom {
                    for x in 0..r.w * zoom {
                        let o = (((r.y + y / zoom) * w + r.x + x / zoom) * 4) as usize;
                        sheet.put_pixel(
                            k as u32 * (r.w * zoom + 8) + x,
                            y,
                            image::Rgba([view[o], view[o + 1], view[o + 2], 255]),
                        );
                    }
                }
            }
            sheet.save(dir.join(format!("detail_{name}.png"))).unwrap();
            println!(
                "{name}: face {}x{}, extent {:.0}, {} face(s) in {seconds:.1} s",
                r.w,
                r.h,
                model.faces[0].extent,
                model.faces.len()
            );
        }
    }

    #[test]
    fn pore_texture_neither_brightens_nor_darkens_the_skin() {
        for period in [1.0f32, 1.7, 2.5, 4.0] {
            let n = 400;
            let values: Vec<f32> = (0..n * n)
                .map(|k| pores((k % n) as f32, (k / n) as f32, period))
                .collect();
            let mean = values.iter().sum::<f32>() / values.len() as f32;
            let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / values.len() as f32;
            assert!(mean.abs() < 0.03, "period {period}: mean {mean}");
            assert!(
                var.sqrt() > 0.15 && var.sqrt() < 0.6,
                "period {period}: sd {}",
                var.sqrt()
            );
            assert!(values.iter().all(|v| v.abs() <= 2.0));
        }
        assert_eq!(pores(10.0, 20.0, 2.0), pores(10.0, 20.0, 2.0));
    }

    /// Opt-in visual probe: IAI_PORTRAIT_FORM_PROBE is a folder of photos;
    /// each gets `form_<name>.png`: as shot | smooth 100 | + Tạo khối 60 |
    /// + Vân da 60, cropped to face 0.
    #[test]
    #[ignore]
    fn probe_form_and_texture() {
        let Ok(dir) = std::env::var("IAI_PORTRAIT_FORM_PROBE") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let volume: f32 = std::env::var("IAI_FORM_VOLUME")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60.0);
        let texture: f32 = std::env::var("IAI_FORM_TEXTURE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60.0);
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if name.starts_with("form_") || !(name.ends_with(".jpg") || name.ends_with(".png")) {
                continue;
            }
            let rgba = image::open(&path).unwrap().to_rgba8();
            let (w, h) = rgba.dimensions();
            let rgba = rgba.into_raw();
            let model = super::super::analyze(&rgba, w, h, false, None, &|_| {}).unwrap();
            let enabled = vec![true; model.faces.len()];
            let smooth = PortraitSettings {
                smooth: 100.0,
                ..PortraitSettings::NEUTRAL
            };
            let shaped = PortraitSettings { volume, ..smooth };
            let textured = PortraitSettings { texture, ..shaped };
            let full = |settings: &PortraitSettings| {
                let mut out = rgba.clone();
                if let Some((r, px)) = render(&rgba, &model, settings, &enabled, &[]) {
                    for y in 0..r.h as usize {
                        let o = ((r.y as usize + y) * w as usize + r.x as usize) * 4;
                        out[o..o + r.w as usize * 4]
                            .copy_from_slice(&px[y * r.w as usize * 4..(y + 1) * r.w as usize * 4]);
                    }
                }
                out
            };
            let views = [rgba.clone(), full(&smooth), full(&shaped), full(&textured)];
            let r = model.faces[0].region;
            let mut sheet = image::RgbaImage::new(r.w * 4 + 30, r.h);
            for (k, view) in views.iter().enumerate() {
                for y in 0..r.h {
                    for x in 0..r.w {
                        let o = (((r.y + y) * w + r.x + x) * 4) as usize;
                        sheet.put_pixel(
                            k as u32 * (r.w + 10) + x,
                            y,
                            image::Rgba([view[o], view[o + 1], view[o + 2], 255]),
                        );
                    }
                }
            }
            sheet.save(dir.join(format!("form_{name}.png"))).unwrap();
            println!(
                "{name}: face {}x{}, extent {:.0}",
                r.w, r.h, model.faces[0].extent
            );
        }
    }

    #[test]
    fn even_light_lifts_shaded_skin_toward_the_lit_skin() {
        let u16v = |v: f32| (v * 65535.0).round() as u16;
        // Lit skin at 0.6; a cheek at it, an eye socket a little darker, the
        // neck far darker, a hot forehead brighter.
        let skin = SkinLayers {
            region: Region {
                x: 0,
                y: 0,
                w: 4,
                h: 1,
            },
            broad: vec![u16v(0.6), u16v(0.56), u16v(0.3), u16v(0.75)],
            lit: 0.6,
            ..one_pixel_skin()
        };
        let s = PortraitSettings {
            even_light: 100.0,
            ..PortraitSettings::NEUTRAL
        }
        .unit();
        let gain = |i: usize| even_light_gain(&skin, &s, i);
        assert!((gain(0) - 1.0).abs() < 1e-4);
        assert!(gain(1) < 1.01, "the face's own shading stays: {}", gain(1));
        // The neck comes most of the way up to the lit skin, not past it.
        assert!(0.3 * gain(2) > 0.5 && 0.3 * gain(2) < 0.6, "{}", gain(2));
        assert!(gain(3) < 1.0 && gain(3) > 0.9, "{}", gain(3));
        // Half the slider, about half the lift; none without a reading.
        let half = PortraitSettings {
            even_light: 50.0,
            ..PortraitSettings::NEUTRAL
        }
        .unit();
        let part = even_light_gain(&skin, &half, 2);
        assert!((part.ln() - 0.5 * gain(2).ln()).abs() < 1e-4);
        let unread = SkinLayers { lit: 0.0, ..skin };
        assert_eq!(even_light_gain(&unread, &s, 2), 1.0);
    }

    #[test]
    fn a_new_photo_starts_corrected_with_the_clear_look_and_ai_detail() {
        let new = PortraitSettings::default();
        let fixes = new.fixes().expect("corrections on");
        assert_eq!((fixes.cast, fixes.exposure, fixes.haze), (1.0, 0.8, 0.6));
        assert_eq!((new.even_light, new.ai_detail), (60.0, 60.0));
        assert_eq!(new.studio_look(), Some((StudioLook::Clear, 0.7)));
        // "Tự động" sets the same corrections on a photo that had none.
        let auto = PortraitSettings::NEUTRAL.with_auto_fix();
        assert_eq!(
            (auto.fixes(), auto.even_light),
            (new.fixes(), new.even_light)
        );
    }

    #[test]
    fn corrections_are_off_until_a_slider_moves() {
        assert!(PortraitSettings::NEUTRAL.fixes().is_none());
        assert!(PortraitSettings::NEUTRAL.studio_look().is_none());
        let fixes = PortraitSettings {
            fix_cast: 80.0,
            fix_warmth: -50.0,
            ..PortraitSettings::NEUTRAL
        }
        .fixes()
        .unwrap();
        assert_eq!((fixes.cast, fixes.warmth, fixes.haze), (0.8, -0.5, 0.0));
    }

    /// Opt-in visual probe: IAI_PORTRAIT_FIX_PROBE is a folder of photos;
    /// each gets `fix_<name>.png`, the whole photo: as shot | Khử ám màu
    /// 100 | + Cân sáng 80 | + Khử đục 60 | + Đều sáng da 60 and the
    /// default retouch | + Chi tiết mặt (AI) 100.
    #[test]
    #[ignore]
    fn probe_fix() {
        use super::super::correct::fix_lut;
        use super::super::looks::preview_graded;
        let Ok(dir) = std::env::var("IAI_PORTRAIT_FIX_PROBE") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if name.starts_with("fix_") || name.starts_with("py_") || !name.ends_with(".jpg") {
                continue;
            }
            let rgba = image::open(&path).unwrap().to_rgba8();
            let (w, h) = rgba.dimensions();
            let rgba = rgba.into_raw();
            let model = super::super::analyze(&rgba, w, h, false, None, &|_| {}).unwrap();
            let enabled = vec![true; model.faces.len()];
            let stats = model.light;
            println!(
                "{name}: skin {:?}, cast {:?}, veil {:?}, lit {:?}",
                stats.skin,
                stats.cast(),
                stats.veil,
                model.faces[0].skin.lit
            );
            super::super::ai_detail::analyze_details(&rgba, &model, &enabled);
            let cast = PortraitSettings {
                fix_cast: 100.0,
                ..PortraitSettings::NEUTRAL
            };
            let lit = PortraitSettings {
                fix_exposure: 80.0,
                ..cast
            };
            let clear = PortraitSettings {
                fix_haze: 60.0,
                ..lit
            };
            let d = PortraitSettings::default();
            let even = PortraitSettings {
                fix_cast: 100.0,
                fix_exposure: 80.0,
                fix_haze: 60.0,
                even_light: 60.0,
                ..d
            };
            let ai = PortraitSettings {
                ai_detail: 100.0,
                ..even
            };
            let full = |settings: &PortraitSettings| {
                let retouched = render(&rgba, &model, settings, &enabled, &[]);
                let fix = settings.fixes().and_then(|f| fix_lut(&stats, &f));
                match preview_graded(&rgba, w, h, retouched, fix.as_ref(), None, None) {
                    Some((r, px)) if r.w == w && r.h == h => px,
                    Some((r, px)) => {
                        let mut out = rgba.clone();
                        for y in 0..r.h as usize {
                            let o = ((r.y as usize + y) * w as usize + r.x as usize) * 4;
                            out[o..o + r.w as usize * 4].copy_from_slice(
                                &px[y * r.w as usize * 4..(y + 1) * r.w as usize * 4],
                            );
                        }
                        out
                    }
                    None => rgba.clone(),
                }
            };
            let views = [
                rgba.clone(),
                full(&cast),
                full(&lit),
                full(&clear),
                full(&even),
                full(&ai),
            ];
            let mut sheet = image::RgbaImage::new((w + 8) * 3, (h + 8) * 2);
            for (k, view) in views.iter().enumerate() {
                let tile = image::RgbaImage::from_raw(w, h, view.clone()).unwrap();
                image::imageops::replace(
                    &mut sheet,
                    &tile,
                    ((k as u32 % 3) * (w + 8)) as i64,
                    ((k as u32 / 3) * (h + 8)) as i64,
                );
            }
            sheet.save(dir.join(format!("fix_{name}.png"))).unwrap();
        }
    }

    #[test]
    fn brows_stay_as_shot_unless_their_own_sliders_move() {
        for s in [PortraitSettings::default(), PortraitSettings::NEUTRAL] {
            assert_eq!((s.brows, s.brow_sharpen, s.brow_tint), (0.0, 0.0, 0.0));
            assert_eq!(
                (s.brow_saturation, s.hair_saturation, s.eye_saturation),
                (0.0, 0.0, 0.0)
            );
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
        let dark = recolour_hair(strand, tone(strand), None, &darker, [1.0; 3]);
        assert!(tone(dark) < tone(strand) - 0.05, "{dark:?}");
        let kept = recolour_hair(skin, tone(skin), None, &darker, [1.0; 3]);
        assert!((tone(kept) - tone(skin)).abs() < 0.01, "{kept:?}");
        let none = recolour_hair(
            strand,
            tone(strand),
            None,
            &PortraitSettings::NEUTRAL,
            [1.0; 3],
        );
        assert_eq!(none, strand);

        // Lighter is Develop's Blacks: deep strands lift, keeping their hue;
        // skin tones stay put.
        let lighter = PortraitSettings {
            hair_brightness: 1.0,
            ..PortraitSettings::NEUTRAL
        };
        let lift = hair_lift(1.0);
        let black = [0.1f32, 0.08, 0.07];
        let lit = recolour_hair(black, tone(black), lift.as_ref(), &lighter, [1.0; 3]);
        assert!(tone(lit) > tone(black) + 0.05, "{lit:?}");
        assert!(lit[0] > lit[1] && lit[1] > lit[2], "hue kept: {lit:?}");
        let kept = recolour_hair(skin, tone(skin), lift.as_ref(), &lighter, [1.0; 3]);
        assert!((tone(kept) - tone(skin)).abs() < 0.01, "{kept:?}");
        assert!(hair_lift(0.0).is_none() && hair_lift(-0.5).is_none());
    }

    #[test]
    fn saturation_fades_or_deepens_the_colour_and_keeps_the_brightness() {
        let dyed = [0.55f32, 0.25, 0.15];
        let grey = saturated(dyed, -1.0);
        assert!((grey[0] - grey[1]).abs() < 1e-6 && (grey[1] - grey[2]).abs() < 1e-6);
        assert!((luma(grey) - luma(dyed)).abs() < 1e-6);
        let half = saturated(dyed, -0.5);
        assert!((half[0] - (dyed[0] + grey[0]) * 0.5).abs() < 1e-6);
        assert_eq!(saturated(dyed, 0.0), dyed);
        let deep = saturated(dyed, 1.0);
        assert!((luma(deep) - luma(dyed)).abs() < 1e-6);
        let spread = |c: [f32; 3]| c[0] - c[2];
        assert!((spread(deep) - (1.0 + SATURATION_GAIN) * spread(dyed)).abs() < 1e-6);

        // Dyed hair loses its colour before a new tint goes on.
        let fade = PortraitSettings {
            hair_saturation: -100.0,
            ..PortraitSettings::NEUTRAL
        }
        .unit();
        assert!(fade.hair_active());
        let out = recolour_hair(dyed, luma(dyed), None, &fade, [1.0; 3]);
        assert!((out[0] - out[2]).abs() < 1e-5, "{out:?}");
        let vivid = PortraitSettings {
            hair_saturation: 100.0,
            ..PortraitSettings::NEUTRAL
        }
        .unit();
        assert!(vivid.hair_active());
        let out = recolour_hair(dyed, luma(dyed), None, &vivid, [1.0; 3]);
        assert!(spread(out) > spread(dyed) + 0.1, "{out:?}");
    }

    #[test]
    fn a_sore_eye_white_loses_its_red_without_turning_grey() {
        let (clear, sore) = ([0.82f32, 0.78, 0.75], [0.8f32, 0.5, 0.5]);
        let out = whitened(sore, 1.0);
        assert!(
            (out[0] - out[1]).abs() < 1e-6 && (out[1] - out[2]).abs() < 1e-6,
            "{out:?}"
        );
        // Brighter than its own luma, not brighter than the white beneath.
        assert!(out[0] > luma(sore) + 0.05 && out[0] <= sore[0], "{out:?}");
        // A clear white hardly moves.
        let out = whitened(clear, 1.0);
        assert!((luma(out) - luma(clear)).abs() < 0.03, "{out:?}");
        assert_eq!(whitened(sore, 0.0), sore);
    }

    #[test]
    fn eye_whites_and_teeth_whiten_toward_the_photos_grey() {
        let stained = [0.80f32, 0.74, 0.52];
        // Without a cast, as before: brighter, most of the colour gone.
        let plain = bleached(stained, 0.1, 0.8);
        assert!((luma(plain) - 1.1 * luma(stained)).abs() < 1e-6);
        assert!((plain[0] - plain[2] - 0.2 * (stained[0] - stained[2])).abs() < 1e-6);
        assert_eq!(
            balanced(stained, [1.0; 3], |c| bleached(c, 0.1, 0.8)),
            plain
        );
        // Under a yellow cast the photo's grey is yellow: the tooth keeps
        // that much yellow, which the white balance then takes out, instead
        // of going past it to blue.
        let grey = [1.05f32, 1.0, 0.8];
        let cast = balanced(stained, grey, |c| bleached(c, 0.1, 1.0));
        assert!(
            (cast[0] / grey[0] - cast[2] / grey[2]).abs() < 1e-6,
            "{cast:?}"
        );
        assert!(cast[2] < cast[0], "{cast:?}");
    }

    #[test]
    fn colour_fades_to_the_photos_grey_not_to_equal_channels() {
        // Under a yellow cast the photo's grey is yellow.
        let grey = [1.05f32, 1.0, 0.8];
        let dyed = [0.55f32, 0.25, 0.15];
        let out = balanced(dyed, grey, |c| saturated(c, -1.0));
        assert!(
            (out[0] / grey[0] - out[1] / grey[1]).abs() < 1e-6,
            "{out:?}"
        );
        assert!(
            (out[2] / grey[2] - out[1] / grey[1]).abs() < 1e-6,
            "{out:?}"
        );
        let fade = PortraitSettings {
            hair_saturation: -100.0,
            ..PortraitSettings::NEUTRAL
        }
        .unit();
        assert_eq!(recolour_hair(dyed, luma(dyed), None, &fade, grey), out);
        // Without a cast it is plain grey.
        let plain = balanced(dyed, [1.0; 3], |c| saturated(c, -1.0));
        assert_eq!(plain, saturated(dyed, -1.0));
    }

    #[test]
    fn skin_brightness_is_develops_midtones_and_keeps_the_texture() {
        let lin = |c: [f32; 3]| luminance_f32(c[0], c[1], c[2]);
        let (up, down) = (skin_tone(1.0).unwrap(), skin_tone(-1.0).unwrap());
        assert!(skin_tone(0.0).is_none());
        let skin = [0.72f32, 0.55, 0.47];
        let (lit, dim) = (midtoned(skin, skin, &up), midtoned(skin, skin, &down));
        assert!(lin(lit) > lin(skin) + 0.05, "{lit:?}");
        assert!(lin(dim) < lin(skin) - 0.05, "{dim:?}");
        assert!(lit[0] > lit[1] && lit[1] > lit[2], "hue kept: {lit:?}");
        // Deep shade and highlights move far less than the mid tones.
        let moved = |c: [f32; 3]| lin(midtoned(c, c, &up)) - lin(c);
        assert!(moved([0.06, 0.05, 0.04]) < 0.25 * moved(skin));
        assert!(moved([0.97, 0.95, 0.93]) < 0.25 * moved(skin));
        // A pore keeps its contrast against the skin around it: both take the
        // gain of the skin around.
        let pore = skin.map(|v| v * 0.9);
        let ratio = |a: [f32; 3], b: [f32; 3]| srgb_to_linear(a[1]) / srgb_to_linear(b[1]);
        let after = ratio(midtoned(pore, skin, &up), lit);
        assert!((after - ratio(pore, skin)).abs() < 0.01, "{after}");
        // Half the slider is well under half the lift, as in Develop.
        let half = PortraitSettings {
            brighten: 50.0,
            ..PortraitSettings::NEUTRAL
        }
        .unit();
        let tone = skin_tone(half.brighten).unwrap();
        let part = lin(midtoned(skin, skin, &tone)) - lin(skin);
        assert!(part > 0.0 && part < 0.5 * (lin(lit) - lin(skin)), "{part}");
        assert_eq!(
            PortraitSettings {
                brighten: -40.0,
                ..PortraitSettings::NEUTRAL
            }
            .unit()
            .brighten,
            -0.4
        );
    }

    /// Opt-in visual probe: IAI_PORTRAIT_SKIN_TONE_PROBE is a folder of
    /// photos; each gets `tone_<name>.png`, face 0 with "Sáng da" at 0 | 50 |
    /// 100 | -50 | -100 and nothing else on.
    #[test]
    #[ignore]
    fn probe_skin_brightness() {
        let Ok(dir) = std::env::var("IAI_PORTRAIT_SKIN_TONE_PROBE") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if !name.ends_with(".jpg") {
                continue;
            }
            let photo = image::open(&path).unwrap().to_rgba8();
            let (w, h) = photo.dimensions();
            let photo = photo.into_raw();
            let model = super::super::analyze(&photo, w, h, false, None, &|_| {}).unwrap();
            let Some(face) = model.faces.first() else {
                continue;
            };
            let enabled = vec![true; model.faces.len()];
            let r = face.skin.region;
            let scale = (480.0 / r.w as f32).min(1.0);
            let (tw, th) = ((r.w as f32 * scale) as u32, (r.h as f32 * scale) as u32);
            let amounts = [0.0f32, 50.0, 100.0, -50.0, -100.0];
            let mut sheet = image::RgbaImage::new((tw + 8) * amounts.len() as u32, th);
            for (k, &brighten) in amounts.iter().enumerate() {
                let settings = PortraitSettings {
                    brighten,
                    ..PortraitSettings::NEUTRAL
                };
                let mut full = image::RgbaImage::from_raw(w, h, photo.clone()).unwrap();
                if let Some((u, px)) = render(&photo, &model, &settings, &enabled, &[]) {
                    let part = image::RgbaImage::from_raw(u.w, u.h, px).unwrap();
                    image::imageops::replace(&mut full, &part, u.x as i64, u.y as i64);
                }
                let crop = image::imageops::crop_imm(&full, r.x, r.y, r.w, r.h).to_image();
                let tile =
                    image::imageops::resize(&crop, tw, th, image::imageops::FilterType::Triangle);
                image::imageops::replace(&mut sheet, &tile, (k as u32 * (tw + 8)) as i64, 0);
            }
            sheet.save(dir.join(format!("tone_{name}.png"))).unwrap();
        }
    }

    /// Opt-in visual probe: IAI_PORTRAIT_EYE_PROBE is a folder of photos;
    /// each gets `eye_<name>.png`, the eyes of face 0 enlarged, under the
    /// default retouch and corrections. Top row, the photo: "Đậm / giảm màu
    /// mắt" 0 | -100 | +100. Bottom row, the same eyes made sore (whites
    /// reddened in patches): 0 | -50 | -100 | -100 faded to the photo's own
    /// grey, which the white balance then tints.
    #[test]
    #[ignore]
    fn probe_eye_colour() {
        use super::super::correct::fix_lut;
        use super::super::looks::preview_graded;
        let Ok(dir) = std::env::var("IAI_PORTRAIT_EYE_PROBE") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if name.starts_with("eye_") || !name.ends_with(".jpg") {
                continue;
            }
            let photo = image::open(&path).unwrap().to_rgba8();
            let (w, h) = photo.dimensions();
            let photo = photo.into_raw();
            let analyse =
                |rgba: &[u8]| super::super::analyze(rgba, w, h, false, None, &|_| {}).unwrap();
            let model = analyse(&photo);
            let Some(face) = model.faces.first() else {
                continue;
            };
            let r = face.region;
            let at = |f: usize| {
                (
                    r.x as usize + f % r.w as usize,
                    r.y as usize + f / r.w as usize,
                )
            };
            let mut sore = photo.clone();
            let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
            for f in 0..r.len() {
                let (x, y) = at(f);
                if face.sclera[f].max(face.iris[f]) > 0 {
                    (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                }
                let patch = 0.75 + 0.25 * ((x as f32 * 0.9).sin() * (y as f32 * 1.3).cos());
                let red = 0.45 * patch * face.sclera[f] as f32 / 255.0;
                let o = (y * w as usize + x) * 4;
                for k in 1..3 {
                    sore[o + k] = (sore[o + k] as f32 * (1.0 - red)).round() as u8;
                }
            }
            let pad = (x1 - x0) / 10;
            let (x0, y0) = (x0.saturating_sub(pad), y0.saturating_sub(pad));
            let (x1, y1) = (
                (x1 + pad).min(w as usize - 1),
                (y1 + pad).min(h as usize - 1),
            );
            let (cw, ch) = ((x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32);
            let eyes = |amount: f32| PortraitSettings {
                eye_saturation: amount,
                ..PortraitSettings::default()
            };
            // The retouch of `retouch`, then the corrections of `fixes`.
            let rendered = |rgba: &[u8],
                            model: &PortraitModel,
                            retouch: &PortraitSettings,
                            fixes: &PortraitSettings| {
                let enabled = vec![true; model.faces.len()];
                let retouched = render(rgba, model, retouch, &enabled, &[]);
                let fix = fixes.fixes().and_then(|f| fix_lut(&model.light, &f));
                let mut out = rgba.to_vec();
                if let Some((u, px)) =
                    preview_graded(rgba, w, h, retouched, fix.as_ref(), None, None)
                {
                    for y in 0..u.h as usize {
                        let o = ((u.y as usize + y) * w as usize + u.x as usize) * 4;
                        out[o..o + u.w as usize * 4]
                            .copy_from_slice(&px[y * u.w as usize * 4..(y + 1) * u.w as usize * 4]);
                    }
                }
                out
            };
            let view = |rgba: &[u8], model: &PortraitModel, amount: f32| {
                rendered(rgba, model, &eyes(amount), &eyes(amount))
            };
            let sore_model = analyse(&sore);
            let unbalanced = PortraitSettings {
                fix_cast: 0.0,
                fix_warmth: 0.0,
                ..eyes(-100.0)
            };
            println!(
                "{name}: grey {:?}",
                grey_axis(&sore_model.light, eyes(0.0).fixes().as_ref())
            );
            let rows = [
                vec![
                    view(&photo, &model, 0.0),
                    view(&photo, &model, -100.0),
                    view(&photo, &model, 100.0),
                ],
                vec![
                    view(&sore, &sore_model, 0.0),
                    view(&sore, &sore_model, -50.0),
                    view(&sore, &sore_model, -100.0),
                    rendered(&sore, &sore_model, &unbalanced, &eyes(-100.0)),
                ],
            ];
            let zoom = (900 / cw).clamp(1, 6);
            let (tw, th) = (cw * zoom, ch * zoom);
            let mut sheet = image::RgbaImage::new((tw + 8) * 4, (th + 8) * 2);
            for (row, views) in rows.iter().enumerate() {
                for (col, view) in views.iter().enumerate() {
                    let full = image::RgbaImage::from_raw(w, h, view.clone()).unwrap();
                    let crop =
                        image::imageops::crop_imm(&full, x0 as u32, y0 as u32, cw, ch).to_image();
                    let tile = image::imageops::resize(
                        &crop,
                        tw,
                        th,
                        image::imageops::FilterType::CatmullRom,
                    );
                    image::imageops::replace(
                        &mut sheet,
                        &tile,
                        (col as u32 * (tw + 8)) as i64,
                        (row as u32 * (th + 8)) as i64,
                    );
                }
            }
            sheet.save(dir.join(format!("eye_{name}.png"))).unwrap();
        }
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

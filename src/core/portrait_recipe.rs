//! What a "Chân dung" layer keeps of the retouch that made it, so the
//! retouch can be reopened: the sliders, which faces were on, the masks
//! painted over them, the selection it kept to. It is data only, and part of
//! the `.iai` format (`formats::iai`). What reads a photo and retouches it
//! is the Pro part (`core::portrait`), absent from a build without it; such
//! a build still opens these layers and saves them whole.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// A rectangle of layer pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Region {
    pub fn len(&self) -> usize {
        self.w as usize * self.h as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Index of image pixel (x, y) in this region, if it lies inside.
    pub fn index_at(&self, x: u32, y: u32) -> Option<usize> {
        (x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h)
            .then(|| ((y - self.y) * self.w + x - self.x) as usize)
    }

    /// Index in this region of pixel `i` of `inner`, a region inside it.
    pub fn index_of(&self, inner: Region, i: usize) -> usize {
        let (x, y) = (i as u32 % inner.w, i as u32 / inner.w);
        ((inner.y - self.y + y) * self.w + inner.x - self.x + x) as usize
    }

    /// The smallest region holding both.
    pub fn union(&self, other: Region) -> Region {
        if other.is_empty() {
            return *self;
        }
        if self.is_empty() {
            return other;
        }
        let (x0, y0) = (self.x.min(other.x), self.y.min(other.y));
        let x1 = (self.x + self.w).max(other.x + other.w);
        let y1 = (self.y + self.h).max(other.y + other.h);
        Region {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        }
    }

    /// The part of this region inside `other` (empty when they do not meet).
    pub fn intersect(&self, other: Region) -> Region {
        let (x0, y0) = (self.x.max(other.x), self.y.max(other.y));
        let x1 = (self.x + self.w).min(other.x + other.w);
        let y1 = (self.y + self.h).min(other.y + other.h);
        Region {
            x: x0,
            y: y0,
            w: x1.saturating_sub(x0),
            h: y1.saturating_sub(y0),
        }
    }

    /// The values of `inner`, a region inside this one, from this region's
    /// `values`.
    pub fn crop<T: Copy + Send + Sync>(&self, values: &[T], inner: Region) -> Vec<T> {
        (0..inner.len())
            .into_par_iter()
            .map(|i| values[self.index_of(inner, i)])
            .collect()
    }

    /// Bounds of `points` grown by the given margins and clipped to the image.
    pub fn around(
        points: impl Iterator<Item = [f32; 2]>,
        grow: [f32; 4],
        width: u32,
        height: u32,
    ) -> Self {
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for [x, y] in points {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        let [left, top, right, bottom] = grow;
        let x0 = (x0 - left).floor().clamp(0.0, width as f32) as u32;
        let y0 = (y0 - top).floor().clamp(0.0, height as f32) as u32;
        let x1 = (x1 + right).ceil().clamp(0.0, width as f32) as u32;
        let y1 = (y1 + bottom).ceil().clamp(0.0, height as f32) as u32;
        Self {
            x: x0,
            y: y0,
            w: x1.saturating_sub(x0),
            h: y1.saturating_sub(y0),
        }
    }
}

/// The selection the dialog opened with, in image pixels: its bounds and its
/// soft mask there. Faces are looked for inside it, the part model looks at
/// it alone when that is closer than its own crop, and the retouch fades out
/// along its edge.
#[derive(Clone, Debug)]
pub struct Clip {
    pub region: Region,
    pub mask: Vec<u8>,
}

impl Clip {
    /// Selection alpha (0..1) at image pixel (x, y).
    pub fn at(&self, x: u32, y: u32) -> f32 {
        let r = self.region;
        if x < r.x || y < r.y || x >= r.x + r.w || y >= r.y + r.h {
            return 0.0;
        }
        self.mask[((y - r.y) * r.w + x - r.x) as usize] as f32 / 255.0
    }

    /// [x0, y0, x1, y1] in image pixels.
    pub fn bounds(&self) -> [f32; 4] {
        let r = self.region;
        [
            r.x as f32,
            r.y as f32,
            (r.x + r.w) as f32,
            (r.y + r.h) as f32,
        ]
    }
}

/// Where the "Trong trẻo" look stands among the studio looks
/// (`core::portrait::looks::StudioLook::Clear`): the look a new photo starts
/// with.
pub const DEFAULT_LOOK: u8 = 1;

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
    /// "Da cổ": how far the neck's skin takes the detail the face restore
    /// model draws there, and has its tone and light evened (see
    /// [`super::neck`]). At 0 the neck stays as it is.
    #[serde(default)]
    pub neck: f32,
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
    /// "Khớp áo với ảnh", 0..100: how much of the photo's own light (its
    /// cast, its exposure, the veil over its blacks) a garment laid on from
    /// a sheet takes. The clothes shot with the photo have it all.
    #[serde(default)]
    pub clothes_match: f32,
}

impl Default for PortraitSettings {
    fn default() -> Self {
        Self {
            smooth: 40.0,
            volume: 30.0,
            texture: 0.0,
            ai_detail: 60.0,
            neck: 0.0,
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
            nose_bridge: 30.0,
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
            look: DEFAULT_LOOK,
            look_strength: DEFAULT_LOOK_STRENGTH,
            clothes_sharpen: 0.0,
            clothes_brightness: 0.0,
            clothes_even: 0.0,
            clothes_match: 0.0,
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
        neck: 0.0,
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
        clothes_match: 0.0,
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
}

/// A painted mask over its region of the image.
#[derive(Clone, Debug)]
pub struct SavedMask {
    pub region: Region,
    pub mask: Vec<u8>,
}

impl SavedMask {
    /// This mask laid over `analysed` (the mask of `region`): painted values
    /// where the two regions meet, the analysis elsewhere.
    pub fn onto(&self, region: Region, analysed: &[u8]) -> Vec<u8> {
        let mut out = analysed.to_vec();
        let (s, r) = (self.region, region);
        if self.mask.len() != s.len() || out.len() != r.len() {
            return out;
        }
        let (x0, y0) = (s.x.max(r.x), s.y.max(r.y));
        let (x1, y1) = ((s.x + s.w).min(r.x + r.w), (s.y + s.h).min(r.y + r.h));
        if x1 <= x0 || y1 <= y0 {
            return out;
        }
        let n = (x1 - x0) as usize;
        for y in y0..y1 {
            let from = ((y - s.y) * s.w + x0 - s.x) as usize;
            let to = ((y - r.y) * r.w + x0 - r.x) as usize;
            out[to..to + n].copy_from_slice(&self.mask[from..from + n]);
        }
        out
    }
}

/// One face as it was applied: where it sat (to find it again), whether it
/// was on, and its painted masks, if any.
#[derive(Clone, Debug)]
pub struct SavedFace {
    pub centre: [f32; 2],
    pub extent: f32,
    pub enabled: bool,
    pub skin: Option<SavedMask>,
    pub hair: Option<SavedMask>,
    pub brows: Option<SavedMask>,
    pub clothes: Option<SavedMask>,
    /// The neck's skin for "Da cổ" as painted, over the skin region.
    pub neck: Option<SavedMask>,
}

#[derive(Clone, Debug)]
pub struct PortraitRecipe {
    /// Id of the photo layer that was analysed, and its size.
    pub source: u32,
    pub source_size: (u32, u32),
    pub settings: PortraitSettings,
    pub clip: Option<Clip>,
    pub faces: Vec<SavedFace>,
    /// Identity of the layer's pixels as the retouch left them
    /// (`TileMap::content_hash`): a layer worked on since is no longer what
    /// the recipe makes. `None` in a recipe saved before this was kept.
    pub made: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_saved_mask_overrides_the_analysis_only_where_regions_meet() {
        let saved = SavedMask {
            region: Region {
                x: 2,
                y: 1,
                w: 3,
                h: 2,
            },
            mask: vec![9; 6],
        };
        let region = Region {
            x: 0,
            y: 0,
            w: 4,
            h: 3,
        };
        let out = saved.onto(region, &[1; 12]);
        #[rustfmt::skip]
        assert_eq!(out, vec![
            1, 1, 1, 1,
            1, 1, 9, 9,
            1, 1, 9, 9,
        ]);
        // A mask of the wrong size is ignored.
        let broken = SavedMask {
            region: saved.region,
            mask: vec![9; 5],
        };
        assert_eq!(broken.onto(region, &[1; 12]), vec![1; 12]);
    }
}

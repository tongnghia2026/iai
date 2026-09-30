//! Slider settings and the cheap per-pixel recombination that turns a
//! [`PortraitModel`] into retouched pixels.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::analysis::{luma, FaceModel, PortraitModel, BLEMISH_SCALE};
use super::geometry::Region;

/// Sliders run 0..100, except the two-sided lip and brow ones (-100..100).
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
            lip_hue: 0.0,
            lip_brightness: 0.0,
            sharpen: 20.0,
            brows: 0.0,
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
        lip_hue: 0.0,
        lip_brightness: 0.0,
        sharpen: 0.0,
        brows: 0.0,
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
            lip_hue: both(self.lip_hue),
            lip_brightness: both(self.lip_brightness),
            sharpen: u(self.sharpen),
            brows: both(self.brows),
        }
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

/// Skin retouch of one pixel from its bands: the photo `src`, the fine-split
/// low `l1` and the mid-split low `l2`.
fn skin_result(
    face: &FaceModel,
    s: &PortraitSettings,
    i: usize,
    src: [f32; 3],
    l1: [f32; 3],
    l2: [f32; 3],
    inside: f32,
) -> [f32; 3] {
    let fine = sub(src, l1);
    let mid = sub(l1, l2);

    let (mut low_y, mut low_c) = split(l2);
    let (_, mean_c) = split(face.skin_mean);
    // Even tone steers the hue toward the face's average while keeping most
    // of the local saturation, so skin evens out without going grey.
    let magnitude = |c: [f32; 3]| (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt();
    let (local, average) = (magnitude(low_c), magnitude(mean_c).max(1e-4));
    let target_mag = local + (average - local) * 0.3;
    let target = mean_c.map(|v| v / average * target_mag);
    for k in 0..3 {
        low_c[k] += (target[k] - low_c[k]) * 0.6 * s.even_tone;
    }
    let under = face.under_eye[i] as f32 / 255.0 * s.dark_circles;
    if under > 0.0 {
        low_y += under * (face.cheek_luma - low_y).max(0.0) * 0.85;
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
    let lift = y - face.broad[i] as f32 / 65535.0;
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

/// Rotate a colour's hue around the grey axis by `angle` radians; positive
/// turns red toward yellow.
fn rotate_hue(c: [f32; 3], angle: f32) -> [f32; 3] {
    let mean = (c[0] + c[1] + c[2]) / 3.0;
    let d = c.map(|v| v - mean);
    let k = 1.0 / 3.0f32.sqrt();
    let cross = [k * (d[2] - d[1]), k * (d[0] - d[2]), k * (d[1] - d[0])];
    let (sin, cos) = angle.sin_cos();
    [
        mean + d[0] * cos + cross[0] * sin,
        mean + d[1] * cos + cross[1] * sin,
        mean + d[2] * cos + cross[2] * sin,
    ]
}

/// The retouched colour of one region pixel. `src` is the photo in 0..1;
/// `fetch(dx, dy)` reads the photo at an offset from this pixel.
fn retouch_pixel(
    face: &FaceModel,
    s: &PortraitSettings,
    i: usize,
    src: [f32; 3],
    fetch: &dyn Fn(isize, isize) -> [f32; 3],
) -> [f32; 3] {
    let m = face.skin[i] as f32 / 255.0;
    let mut out = src;
    if m > 0.0 {
        let inside = face.interior[i] as f32 / 255.0;
        let l1 = from_u16(face.low1[i]);
        let l2 = from_u16(face.low2[i]);
        let mut r = skin_result(face, s, i, src, l1, l2, inside);
        let cover = face.spot_cover[i] as f32 / 255.0;
        if s.blemish > 0.0 && cover > 0.0 {
            let score = face.spot_score[i] as f32 / BLEMISH_SCALE;
            let threshold = 1.5 - 1.15 * s.blemish;
            let spot = smoothstep(threshold * 0.85, threshold * 1.15, score) * cover * inside;
            if spot > 0.0 {
                // Heal like a healing brush: borrow the texture of nearby clean
                // skin, shifted to the colour around this spot.
                let here = from_u16(face.heal_base[i]);
                let [dx, dy] = face.donor[i];
                let (healed, healed_l1) = if dx == 0 && dy == 0 {
                    (here, here)
                } else {
                    let q =
                        (i as isize + dy as isize * face.region.w as isize + dx as isize) as usize;
                    let shift = sub(here, from_u16(face.heal_base[q]));
                    (
                        add(fetch(dx as isize, dy as isize), shift),
                        add(from_u16(face.low1[q]), shift),
                    )
                };
                let fixed = skin_result(face, s, i, healed, healed_l1, l2, inside);
                for k in 0..3 {
                    r[k] += (fixed[k] - r[k]) * spot;
                }
            }
        }
        for k in 0..3 {
            out[k] = src[k] + m * (r[k] - src[k]);
        }
    }

    let white = face.eye_white[i] as f32 / 255.0 * s.eye_white;
    if white > 0.0 {
        let (y, c) = split(out);
        out = join(
            y * (1.0 + 0.15 * white),
            c.map(|v| v * (1.0 - 0.75 * white)),
        );
    }
    let iris = face.iris[i] as f32 / 255.0 * s.iris;
    if iris > 0.0 {
        let (y, c) = split(out);
        out = join(y * (1.0 + 0.18 * iris), c.map(|v| v * (1.0 + 0.35 * iris)));
    }
    let teeth = face.teeth[i] as f32 / 255.0 * s.teeth;
    if teeth > 0.0 {
        let (y, c) = split(out);
        out = join(y * (1.0 + 0.1 * teeth), c.map(|v| v * (1.0 - 0.8 * teeth)));
    }
    let lips = face.lips[i] as f32 / 255.0;
    if lips > 0.0 && (s.lip_saturation != 0.0 || s.lip_hue != 0.0 || s.lip_brightness != 0.0) {
        // Positive hue turns the lips pinker (toward berry), negative toward coral.
        let turned = rotate_hue(out, -s.lip_hue * 0.35);
        let (y, c) = split(turned);
        let coloured = join(
            y * (1.0 + 0.3 * s.lip_brightness),
            c.map(|v| v * (1.0 + 0.8 * s.lip_saturation).max(0.0)),
        );
        for k in 0..3 {
            out[k] += (coloured[k] - out[k]) * lips;
        }
    }
    let brow = face.brows[i] as f32 / 255.0 * s.brows.abs();
    if brow > 0.0 {
        if s.brows > 0.0 {
            out = out.map(|v| v * (1.0 - 0.4 * brow));
        } else {
            let skin = from_u16(face.low2[i]);
            for k in 0..3 {
                out[k] += (skin[k] - out[k]) * 0.7 * brow;
            }
        }
    }
    let crisp = face.detail[i] as f32 / 255.0 * s.sharpen;
    if crisp > 0.0 {
        let soft = from_u16(face.soft[i]);
        for k in 0..3 {
            out[k] += 1.5 * crisp * (src[k] - soft[k]);
        }
    }
    out
}

/// The smallest rectangle holding every enabled face's region.
pub fn union_region(model: &PortraitModel, enabled: &[bool]) -> Option<Region> {
    let mut bounds: Option<(u32, u32, u32, u32)> = None;
    for (face, _) in model
        .faces
        .iter()
        .zip(enabled.iter().chain(std::iter::repeat(&true)))
        .filter(|(face, &on)| on && !face.region.is_empty())
    {
        let r = face.region;
        let (x1, y1) = (r.x + r.w, r.y + r.h);
        bounds = Some(match bounds {
            None => (r.x, r.y, x1, y1),
            Some((a, b, c, d)) => (a.min(r.x), b.min(r.y), c.max(x1), d.max(y1)),
        });
    }
    bounds.map(|(x0, y0, x1, y1)| Region {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    })
}

/// Retouch every enabled face of `rgba` (the analysed image) and return the
/// union region with its new RGBA pixels. Faces add their own changes, so
/// overlapping regions compose.
pub fn render(
    rgba: &[u8],
    model: &PortraitModel,
    settings: &PortraitSettings,
    enabled: &[bool],
) -> Option<(Region, Vec<u8>)> {
    let union = union_region(model, enabled)?;
    let width = model.width as usize;
    let (uw, uh) = (union.w as usize, union.h as usize);
    let mut out = vec![0u8; uw * uh * 4];
    out.par_chunks_mut(uw * 4)
        .enumerate()
        .for_each(|(row, line)| {
            let o = ((union.y as usize + row) * width + union.x as usize) * 4;
            line.copy_from_slice(&rgba[o..o + uw * 4]);
        });
    let s = settings.unit();
    let pixel = |x: usize, y: usize| {
        let o = (y * width + x) * 4;
        [
            rgba[o] as f32 / 255.0,
            rgba[o + 1] as f32 / 255.0,
            rgba[o + 2] as f32 / 255.0,
        ]
    };
    let mut delta = vec![[0.0f32; 3]; uw * uh];
    for (face, _) in model
        .faces
        .iter()
        .zip(enabled.iter().chain(std::iter::repeat(&true)))
        .filter(|(_, &on)| on)
    {
        let r = face.region;
        let (fw, fx, fy) = (
            r.w as usize,
            (r.x - union.x) as usize,
            (r.y - union.y) as usize,
        );
        delta
            .par_chunks_mut(uw)
            .enumerate()
            .skip(fy)
            .take(r.h as usize)
            .for_each(|(urow, line)| {
                let row = urow - fy;
                for col in 0..fw {
                    let i = row * fw + col;
                    let (x, y) = (r.x as usize + col, r.y as usize + row);
                    let src = pixel(x, y);
                    let fetch = |dx: isize, dy: isize| {
                        pixel((x as isize + dx) as usize, (y as isize + dy) as usize)
                    };
                    let res = retouch_pixel(face, &s, i, src, &fetch);
                    let cell = &mut line[fx + col];
                    for k in 0..3 {
                        cell[k] += res[k] - src[k];
                    }
                }
            });
    }
    out.par_chunks_mut(4)
        .zip(delta.par_iter())
        .for_each(|(px, d)| {
            for k in 0..3 {
                px[k] = (px[k] as f32 + d[k] * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        });
    Some((union, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hue_rotation_keeps_grey_and_turns_red_toward_yellow() {
        let grey = rotate_hue([0.5, 0.5, 0.5], 0.4);
        assert!(grey.iter().all(|v| (v - 0.5).abs() < 1e-6));
        let red = rotate_hue([0.8, 0.3, 0.3], 0.3);
        assert!(
            red[1] > red[2],
            "positive angle moves red toward yellow: {red:?}"
        );
    }
}

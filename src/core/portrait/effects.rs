//! Slider settings and the cheap per-pixel recombination that turns a
//! [`PortraitModel`] into retouched pixels.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::analysis::{luma, FaceModel, PortraitModel, BLEMISH_SCALE};
use super::geometry::Region;

/// Every slider runs 0..100.
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
    };

    fn unit(&self) -> Self {
        let u = |v: f32| (v / 100.0).clamp(0.0, 1.0);
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

/// The retouched colour of one region pixel, `src` in 0..1.
fn retouch_pixel(face: &FaceModel, s: &PortraitSettings, i: usize, src: [f32; 3]) -> [f32; 3] {
    let m = face.skin[i] as f32 / 255.0;
    let mut out = src;
    if m > 0.0 {
        let l1 = from_u16(face.low1[i]);
        let l2 = from_u16(face.low2[i]);
        let fine = [src[0] - l1[0], src[1] - l1[1], src[2] - l1[2]];
        let mid = [l1[0] - l2[0], l1[1] - l2[1], l1[2] - l2[2]];

        let (mut low_y, mut low_c) = split(l2);
        let (_, mean_c) = split(face.skin_mean);
        // Even tone steers the hue toward the face's average while keeping
        // most of the local saturation, so skin evens out without going grey.
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

        let score = face.blemish[i] as f32 / BLEMISH_SCALE;
        let spot = if s.blemish > 0.0 {
            let threshold = 1.5 - 1.15 * s.blemish;
            smoothstep(threshold * 0.85, threshold * 1.15, score)
        } else {
            0.0
        };
        let inside = face.interior[i] as f32 / 255.0;
        let keep_mid = 1.0 - (1.0 - (1.0 - 0.85 * s.smooth) * (1.0 - spot)) * inside;
        let (mid_y, mid_c) = split(mid);
        let keep_fine = 1.0 - 0.15 * s.smooth * inside;
        // On a spot, drop the fine band's darkness and colour but keep its
        // lighter grain, so the patch keeps a skin texture instead of going flat.
        let (fine_y, fine_c) = split(fine);
        let spot_fine = spot * inside;
        let fine_y = fine_y - spot_fine * fine_y.min(0.0);
        let fine = join(fine_y, fine_c.map(|v| v * (1.0 - 0.7 * spot_fine)));

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
                    let o = (y * width + x) * 4;
                    let src = [
                        rgba[o] as f32 / 255.0,
                        rgba[o + 1] as f32 / 255.0,
                        rgba[o + 2] as f32 / 255.0,
                    ];
                    let res = retouch_pixel(face, &s, i, src);
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

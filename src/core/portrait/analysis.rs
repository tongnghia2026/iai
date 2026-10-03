//! One-off analysis of a photo for portrait retouching: find faces, fit the
//! face mesh, segment parts when the Sapiens2 model is present, then precompute
//! each face's masks and frequency split so slider changes only recombine.

use rayon::prelude::*;

use super::blur::{blur4, masked_blur};
use super::geometry::*;
use super::skin_mask::{self, SkinInputs};
use crate::core::ai::body_parts::{self, PartLabels, Segmenter};
use crate::core::ai::face_mesh::{self, FaceMesh};
use crate::core::color::luminance_f32;

/// Share of mesh landmarks that must land on face-like part classes before the
/// part masks are trusted over the mesh-only fallback.
pub const TRUSTED_AGREEMENT: f32 = 0.8;
/// Fixed-point scale of the stored blemish score.
pub(super) const BLEMISH_SCALE: f32 = 32.0;
/// How far below the chin (in face sizes) the face's own skin tone is read.
const TONE_BELOW_CHIN: f32 = 0.45;

/// The face region grown over all the skin the part model may see on this
/// person (neck, shoulders, arms, a hand at the face) where the photo has
/// skin's colour, plus a margin past the skin mask's fence, so the mask is
/// not cut off by the face's own box. Points nearer another face are that
/// face's.
fn skin_region(
    rgba: &[u8],
    parts: &PartLabels,
    face: Region,
    owners: &[([f32; 2], f32)],
    index: usize,
    width: u32,
    height: u32,
) -> Region {
    let (own_centre, e) = owners[index];
    let step = (e / 50.0).max(2.0);
    let [x0, y0, x1, y1] = parts.bounds();
    let (x0, y0) = (x0.max(0.0), y0.max(0.0));
    let cols = ((x1.min(width as f32) - x0) / step).ceil().max(0.0) as usize;
    let rows = ((y1.min(height as f32) - y0) / step).ceil().max(0.0) as usize;
    let seen: Vec<[f32; 2]> = (0..cols * rows)
        .into_par_iter()
        .filter_map(|c| {
            let x = x0 + ((c % cols) as f32 + 0.5) * step;
            let y = y0 + ((c / cols) as f32 + 0.5) * step;
            let g = parts.groups_at(x, y);
            if g[body_parts::GROUP_FACE_SKIN] + g[body_parts::GROUP_BODY_SKIN]
                <= skin_mask::SEEN_SKIN
            {
                return None;
            }
            let own = (x - own_centre[0]).hypot(y - own_centre[1]) / e;
            let foreign = owners
                .iter()
                .enumerate()
                .any(|(j, (c, s))| j != index && (x - c[0]).hypot(y - c[1]) / s < own);
            let colour = body_parts::colour_near(rgba, width, height, x, y, step);
            (!foreign && parts.skin_coloured(colour)).then_some([x, y])
        })
        .collect();
    if seen.is_empty() {
        return face;
    }
    let margin = 0.1 * e + step;
    face.union(Region::around(seen.into_iter(), [margin; 4], width, height))
}

/// Rows of `region` from its top down to just below the chin: where the
/// face's skin tone is read, so neck and chest skin do not shift it.
fn tone_rows(points: &[[f32; 3]], region: Region, e: f32) -> usize {
    let chin = points.iter().map(|p| p[1]).fold(f32::MIN, f32::max);
    ((chin + TONE_BELOW_CHIN * e).ceil() - region.y as f32).clamp(1.0, region.h.max(1) as f32)
        as usize
}

pub struct FaceModel {
    pub mesh: FaceMesh,
    /// Mesh/part agreement; `None` when the part model was not run.
    pub agreement: Option<f32>,
    pub region: Region,
    pub extent: f32,
    pub skin: SkinLayers,
    /// Under-eye bands below each lower lid, before the skin mask cuts them.
    pub(super) under_band: Vec<u8>,
    /// The eye openings outside the irises, whatever their brightness, and
    /// their bright part: the whites as "Trắng mắt" sees them.
    pub(super) sclera: Vec<u8>,
    pub(super) eye_white: Vec<u8>,
    pub(super) iris: Vec<u8>,
    pub(super) teeth: Vec<u8>,
    /// Spot strength (score, fixed point) and soft disc coverage.
    pub(super) spot_score: Vec<u8>,
    pub(super) spot_cover: Vec<u8>,
    /// Offset (dx, dy) to clean skin whose texture heals each spot pixel, and
    /// the local skin colour around spots used to match that texture.
    pub(super) donor: Vec<[i16; 2]>,
    pub(super) heal_base: Vec<[u16; 3]>,
    pub(super) lips: Vec<u8>,
    pub(super) brows: BrowLayers,
    /// Nose contour: +127 lights the bridge, -127 shades its sides.
    pub(super) nose: Vec<i8>,
    /// Hair lies outside the face region, so it has its own (head and
    /// shoulders) region; empty without the part model. `hair` is how much
    /// each pixel takes the hair change, `hair_base` its regional luminance
    /// (Rec. 709, 0..65535) for Develop-style Shadows/Blacks.
    pub hair_region: Region,
    pub(super) hair: Vec<u8>,
    pub(super) hair_base: Vec<u16>,
    /// Eyes, lashes and lips: where sharpening applies.
    pub(super) detail: Vec<u8>,
    /// Plain small blur of the photo, the sharpening reference.
    pub(super) soft: Vec<[u16; 3]>,
    /// The restore model's detail for this face, made once "Chi tiết mặt
    /// (AI)" is first used (see [`super::ai_detail::analyze_details`]).
    pub ai_detail: std::sync::OnceLock<Result<super::ai_detail::FaceDetail, String>>,
}

impl FaceModel {
    pub fn trusted(&self) -> bool {
        self.agreement.is_some_and(|a| a >= TRUSTED_AGREEMENT)
    }

    /// How much each hair-region pixel takes the hair change.
    pub fn hair_mask(&self) -> &[u8] {
        &self.hair
    }

    /// The brows as analysed.
    pub fn brow_layers(&self) -> &BrowLayers {
        &self.brows
    }

    /// The skin mask `skin` (of the skin region, as the analysis or the
    /// brush left it) with the brows' share moved to brow `area` (of the
    /// brow region): brow takes precedence over skin.
    pub fn skin_with_brows(&self, skin: &[u8], area: &[u8]) -> Vec<u8> {
        let mut out = skin.to_vec();
        let b = &self.brows;
        if area.len() != b.region.len() || skin.len() != self.skin.region.len() {
            return out;
        }
        for (k, &a1) in area.iter().enumerate() {
            let i = self.skin.region.index_of(b.region, k);
            out[i] = self.skin_beside_brow_at(k, skin[i], a1);
        }
        out
    }

    /// [`skin_with_brows`](Self::skin_with_brows) at image pixel (x, y) only.
    pub fn skin_with_brow_at(&self, area: &[u8], x: u32, y: u32, skin: u8) -> u8 {
        match self.brows.region.index_at(x, y) {
            Some(k) if area.len() == self.brows.region.len() => {
                self.skin_beside_brow_at(k, skin, area[k])
            }
            _ => skin,
        }
    }

    fn skin_beside_brow_at(&self, k: usize, skin: u8, a1: u8) -> u8 {
        let b = &self.brows;
        let unit = |v: u8| v as f32 / 255.0;
        to_u8(skin_beside_brow(
            unit(skin),
            unit(b.area[k]),
            unit(a1),
            unit(b.uncut[k]),
        ))
    }
}

/// The brows over their own region (inside the face's): the smooth brow
/// shape (`area`: what the brow sliders act on, kept out of the skin retouch
/// so brows stay as shot until their own sliders move), how much each pixel
/// in it is hair, the brow's local mean colour, and the skin colour beneath,
/// which a fading brow moves toward. The brush repaints `area`; what the rest
/// is rebuilt from rides along: how much each pixel looks like a brow hair
/// wherever it lies (`strand`), the weights the skin colour beneath is read
/// with, and the skin mask before the brows were cut out of it.
#[derive(Clone)]
pub struct BrowLayers {
    pub(super) region: Region,
    pub(super) hair: Vec<u8>,
    pub(super) area: Vec<u8>,
    pub(super) mean: Vec<[u16; 3]>,
    pub(super) skin: Vec<[u16; 3]>,
    strand: Vec<u8>,
    beneath_weight: Vec<u8>,
    uncut: Vec<u8>,
}

impl BrowLayers {
    /// Where the brows' layers lie in the image.
    pub fn region(&self) -> Region {
        self.region
    }

    /// The brow shape the sliders act on.
    pub fn area(&self) -> &[u8] {
        &self.area
    }
}

/// Skin coverage at a brow pixel whose brow area changed from the
/// analysis's `a0` to `a1`, given the skin mask `s` there: more brow takes
/// skin away in proportion, less gives back the skin found there before the
/// brows were cut out (`uncut`), never less than `s`.
fn skin_beside_brow(s: f32, a0: f32, a1: f32, uncut: f32) -> f32 {
    if a1 > a0 {
        s * (1.0 - a1) / (1.0 - a0)
    } else if a1 < a0 {
        s.max(uncut * (1.0 - a1))
    } else {
        s
    }
}

/// Colour distance that counts a change of hue or saturation fully and one
/// of brightness a little less (creases are shading, brow hairs are both).
fn brow_distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (ya, yb) = (luma(a), luma(b));
    let du = (a[2] - ya) - (b[2] - yb);
    let dv = (a[0] - ya) - (b[0] - yb);
    ((0.7 * (ya - yb)).powi(2) + du * du + dv * dv).sqrt()
}

/// Nose contour over `region` (-127..127): light down the bridge, from
/// between the brows onto the tip, and a light shade along both sides of
/// it. Both fall off as bell curves across the bridge and fade in and out
/// along it, so the change has no edge anywhere. Between the eyes, where the
/// face is flat and broad, the light stays narrow and gentle.
fn nose_contour(points: &[[f32; 3]], region: Region, e: f32) -> Vec<i8> {
    let line: Vec<[f32; 2]> = loop_points(points, &NOSE_BRIDGE);
    let lengths: Vec<f32> = line
        .windows(2)
        .map(|s| (s[1][0] - s[0][0]).hypot(s[1][1] - s[0][1]))
        .collect();
    let total = lengths.iter().sum::<f32>().max(1.0);
    // Distance from the bridge line and how far down it (0 at the top,
    // 1 at the tip) the nearest point lies.
    let nearest = |x: f32, y: f32| {
        let (mut best, mut along, mut start) = (f32::MAX, 0.0f32, 0.0f32);
        for (s, &length) in line.windows(2).zip(&lengths) {
            let (dx, dy) = (s[1][0] - s[0][0], s[1][1] - s[0][1]);
            let u = if length > 0.0 {
                (((x - s[0][0]) * dx + (y - s[0][1]) * dy) / (length * length)).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let d = (x - s[0][0] - u * dx).hypot(y - s[0][1] - u * dy);
            if d < best {
                best = d;
                along = (start + u * length) / total;
            }
            start += length;
        }
        (best, along)
    };
    let (light_width, shade_offset, shade_width) = (0.026 * e, 0.08 * e, 0.035 * e);
    let top_width = 0.015 * e;
    let bounds = Region::around(
        line.iter().copied(),
        [shade_offset + 3.0 * shade_width; 4],
        region.x + region.w,
        region.y + region.h,
    )
    .intersect(region);
    let w = region.w as usize;
    (0..region.len())
        .into_par_iter()
        .map(|i| {
            let (x, y) = (region.x + (i % w) as u32, region.y + (i / w) as u32);
            if bounds.index_at(x, y).is_none() {
                return 0;
            }
            let (d, t) = nearest(x as f32 + 0.5, y as f32 + 0.5);
            let bell = |offset: f32, width: f32| (-0.5 * ((d - offset) / width).powi(2)).exp();
            // In from brow level, about half between the eyes, full and
            // widest down the middle of the bridge, a third left on the tip;
            // the shade only beside the bony bridge.
            let width = top_width + (light_width - top_width) * smoothstep(0.2, 0.55, t);
            let light = bell(0.0, width)
                * smoothstep(0.05, 0.5, t)
                * (1.0 - 0.7 * smoothstep(0.75, 1.0, t));
            let shade = bell(shade_offset, shade_width)
                * smoothstep(0.15, 0.4, t)
                * (1.0 - smoothstep(0.6, 0.85, t));
            ((light - shade).clamp(-1.0, 1.0) * 127.0).round() as i8
        })
        .collect()
}

/// Value at quantile `q` (0..1) of `values`.
fn quantile(mut values: Vec<f32>, q: f32) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    let k = ((values.len() - 1) as f32 * q) as usize;
    values.select_nth_unstable_by(k, f32::total_cmp);
    Some(values[k])
}

/// Where to look for one brow's hairs: the mesh's brow outline (lower edge
/// outer to inner, then upper edge inner to outer) moved out where hairs
/// often reach past it: up, and on past the tail, but hardly down, where
/// eye make-up lies, nor in past the head, where frown lines are.
fn brow_search(outline: &[[f32; 2]], down: [f32; 2], e: f32) -> Vec<[f32; 2]> {
    let half = outline.len() / 2;
    let mid = |a: usize, b: usize| {
        [
            (outline[a][0] + outline[b][0]) * 0.5,
            (outline[a][1] + outline[b][1]) * 0.5,
        ]
    };
    let (outer, inner) = (mid(0, outline.len() - 1), mid(half - 1, half));
    let length = (outer[0] - inner[0]).hypot(outer[1] - inner[1]).max(1.0);
    let along = [
        (outer[0] - inner[0]) / length,
        (outer[1] - inner[1]) / length,
    ];
    outline
        .iter()
        .enumerate()
        .map(|(k, p)| {
            let normal = if k < half { 0.012 * e } else { -0.025 * e };
            let tail = k == 0 || k == outline.len() - 1;
            let end = if tail { 0.02 * e } else { 0.0 };
            [
                p[0] + down[0] * normal + along[0] * end,
                p[1] + down[1] * normal + along[1] * end,
            ]
        })
        .collect()
}

/// The part model's hair outline is coarse and can stop short of a fringe's
/// edge, leaving a band of scalp hair beside a brow's tail. Carry the scalp
/// on from where the model is sure of hair through pixels coloured like that
/// hair (rather than like the skin `beneath`), a short way only, so a brow
/// touching the fringe loses at most its very end. Returns how much each
/// pixel is such hair (0..1).
#[allow(clippy::too_many_arguments)]
fn fringe_edge(
    colours: &[[f32; 4]],
    beneath: &[[f32; 3]],
    odds: &[f32],
    side: &[usize],
    middle: &[bool],
    w: usize,
    h: usize,
    e: f32,
) -> Vec<f32> {
    let n = w * h;
    let seed: Vec<bool> = odds.iter().map(|&o| o >= 0.5).collect();
    let seeds = seed.iter().filter(|&&s| s).count();
    if seeds == 0 {
        return vec![0.0; n];
    }
    // Where a pixel's colour sits from the skin beneath (0) to `hair` (1).
    let share_of = |i: usize, hair: [f32; 3]| {
        let (c, skin) = (colours[i], beneath[i]);
        let gap = [hair[0] - skin[0], hair[1] - skin[1], hair[2] - skin[2]];
        let gap2 = gap.iter().map(|v| v * v).sum::<f32>();
        if gap2 < 0.12 * 0.12 {
            return 0.0;
        }
        (0..3).map(|k| (c[k] - skin[k]) * gap[k]).sum::<f32>() / gap2
    };
    // A brow whose own hairs look like the scalp's cannot be told from a
    // fringe touching it by colour: leave that side alone. Otherwise only
    // pixels clearly more like the scalp than the brow carry it on.
    let scalp_colour = {
        let mut sum = [0.0f32; 3];
        for i in (0..n).filter(|&i| seed[i]) {
            for k in 0..3 {
                sum[k] += colours[i][k];
            }
        }
        sum.map(|v| v / seeds as f32)
    };
    let mut needed = [f32::INFINITY; 2];
    for (s, need) in needed.iter_mut().enumerate() {
        let brow: Vec<f32> = (0..n)
            .filter(|&i| side[i] == s && middle[i])
            .map(|i| share_of(i, scalp_colour))
            .collect();
        if let Some(like) = quantile(brow, 0.75).filter(|&like| like < 0.5) {
            *need = (like + 0.25).max(0.6);
        }
    }
    if needed.iter().all(|v| v.is_infinite()) {
        return vec![0.0; n];
    }
    let mut hair: Vec<[f32; 4]> = (0..n)
        .map(|i| {
            let (c, s) = (colours[i], f32::from(u8::from(seed[i])));
            [c[0] * s, c[1] * s, c[2] * s, s]
        })
        .collect();
    blur4(&mut hair, w, h, (e / 40.0).max(2.0));
    let share: Vec<f32> = (0..n)
        .map(|i| {
            let [r, g, b, s] = hair[i];
            if s < 0.05 || needed[side[i]].is_infinite() {
                return 0.0;
            }
            share_of(i, [r / s, g / s, b / s])
        })
        .collect();
    let reach = (0.04 * e).max(2.0) as u32;
    let mut steps = vec![u32::MAX; n];
    let mut queue = std::collections::VecDeque::new();
    for i in (0..n).filter(|&i| seed[i]) {
        steps[i] = 0;
        queue.push_back(i);
    }
    // Through hair-coloured pixels, then on down the soft edge of the hair
    // (never back up it, onto a brow hair beyond).
    let blend = (e / 60.0).max(2.0) as u32;
    let mut edge = vec![0u32; n];
    while let Some(i) = queue.pop_front() {
        let next = steps[i] + 1;
        if next > reach + blend {
            continue;
        }
        let (x, y) = (i % w, i / w);
        let around = [
            (x > 0).then(|| i - 1),
            (x + 1 < w).then(|| i + 1),
            (y > 0).then(|| i - w),
            (y + 1 < h).then(|| i + w),
        ];
        for j in around.into_iter().flatten() {
            if steps[j] != u32::MAX {
                continue;
            }
            let blended = share[j] < needed[side[j]];
            let into_edge = edge[i] + u32::from(blended);
            let downhill = !blended || share[j] <= share[i] + 0.03;
            if share[j] >= 0.1 && downhill && into_edge <= blend && (into_edge > 0 || next <= reach)
            {
                steps[j] = next;
                edge[j] = into_edge;
                queue.push_back(j);
            }
        }
    }
    (0..n)
        .map(|i| {
            if steps[i] == u32::MAX || seed[i] {
                0.0
            } else {
                smoothstep(0.1, 0.25, share[i])
            }
        })
        .collect()
}

/// Find the brows around the mesh's brow outlines. Each pixel's colour is
/// measured against the skin around the brows carried across them; each
/// brow learns how far its own hairs and the plain skin beside it stray from
/// that, so dark, grey and fair brows all register. The brow shape is where
/// those hairs gather (not single hairs), so its edge is soft and the
/// sliders do not paint a hard outline.
#[allow(clippy::too_many_arguments)]
fn brow_layers(
    src: &[[f32; 3]],
    face: Region,
    skin: &[f32],
    skin_region: Region,
    points: &[[f32; 3]],
    parts: Option<&PartLabels>,
    down: [f32; 2],
    e: f32,
) -> BrowLayers {
    let outlines = [
        loop_points(points, &RIGHT_BROW),
        loop_points(points, &LEFT_BROW),
    ];
    let r = Region::around(
        outlines.iter().flatten().copied(),
        [0.12 * e; 4],
        face.x + face.w,
        face.y + face.h,
    )
    .intersect(face);
    let (w, h, n) = (r.w as usize, r.h as usize, r.len());
    let colours = face.crop(src, r);
    if n == 0 {
        return BrowLayers {
            region: r,
            hair: Vec::new(),
            area: Vec::new(),
            mean: Vec::new(),
            skin: Vec::new(),
            strand: Vec::new(),
            beneath_weight: Vec::new(),
            uncut: Vec::new(),
        };
    }
    let skin_here: Vec<f32> = (0..n).map(|i| skin[skin_region.index_of(r, i)]).collect();
    let at = |i: usize| {
        (
            r.x as f32 + (i % w) as f32 + 0.5,
            r.y as f32 + (i / w) as f32 + 0.5,
        )
    };
    // Scalp hair (a fringe, a strand at the temple) is no brow: the part
    // model reads brows as face skin, so even faint odds of hair rule out.
    let odds: Vec<f32> = (0..n)
        .map(|i| {
            parts.map_or(0.0, |p| {
                let (x, y) = at(i);
                p.groups_at(x, y)[body_parts::GROUP_HAIR]
            })
        })
        .collect();
    let mut scalp: Vec<f32> = odds.iter().map(|&o| smoothstep(0.25, 0.55, o)).collect();
    let mut zone = vec![0.0f32; n];
    let mut core = vec![0.0f32; n];
    let mut near = vec![0.0f32; n];
    let mut eyes = vec![0.0f32; n];
    for outline in &outlines {
        stamp_polygon(&mut core, r, outline, 0.0, 0.01 * e);
        stamp_polygon(&mut zone, r, &brow_search(outline, down, e), 0.0, 0.035 * e);
        stamp_polygon(&mut near, r, outline, 0.08 * e, 0.02 * e);
    }
    for eye in [&RIGHT_EYE[..], &LEFT_EYE[..]] {
        stamp_polygon(&mut eyes, r, &loop_points(points, eye), 0.03 * e, 0.02 * e);
    }
    for i in 0..n {
        zone[i] *= (1.0 - eyes[i]) * (1.0 - scalp[i]);
    }
    // The skin around the brows, carried across them.
    let weight: Vec<f32> = (0..n)
        .map(|i| skin_here[i] * (1.0 - zone[i]) * (1.0 - eyes[i]) * (1.0 - scalp[i]))
        .collect();
    let beneath = masked_blur(&colours, &weight, w, h, (e / 18.0).max(2.0));
    let mut smooth: Vec<[f32; 4]> = colours.iter().map(|c| [c[0], c[1], c[2], 0.0]).collect();
    blur4(&mut smooth, w, h, (e / 900.0).round());
    let centres: Vec<[f32; 2]> = outlines
        .iter()
        .map(|o| {
            let k = o.len() as f32;
            [
                o.iter().map(|p| p[0]).sum::<f32>() / k,
                o.iter().map(|p| p[1]).sum::<f32>() / k,
            ]
        })
        .collect();
    let side: Vec<usize> = (0..n)
        .map(|i| {
            let (x, y) = at(i);
            let d = |c: [f32; 2]| (x - c[0]).hypot(y - c[1]);
            usize::from(d(centres[1]) < d(centres[0]))
        })
        .collect();
    // The middle of each brow (well clear of a fringe at either end), to
    // tell whether its hairs look like the scalp's.
    let middle: Vec<bool> = (0..n)
        .map(|i| {
            let (x, y) = at(i);
            let c = centres[side[i]];
            core[i] > 0.7 && odds[i] < 0.25 && (x - c[0]).hypot(y - c[1]) < 0.1 * e
        })
        .collect();
    let fringe = fringe_edge(&smooth, &beneath, &odds, &side, &middle, w, h, e);
    for i in 0..n {
        scalp[i] = scalp[i].max(fringe[i]);
        zone[i] *= 1.0 - fringe[i];
    }
    let distance: Vec<f32> = (0..n)
        .map(|i| {
            let c = smooth[i];
            brow_distance([c[0], c[1], c[2]], beneath[i])
        })
        .collect();
    // Each brow's own levels: how far the plain skin beside it strays
    // (texture, creases) and how far its hairs do.
    let mut levels = [(0.03f32, 0.1f32); 2];
    for (s, level) in levels.iter_mut().enumerate() {
        let plain: Vec<f32> = (0..n)
            .filter(|&i| {
                side[i] == s
                    && near[i] > 0.5
                    && zone[i] < 0.05
                    && eyes[i] < 0.1
                    && skin_here[i] > 0.5
            })
            .map(|i| distance[i])
            .collect();
        let hairs: Vec<f32> = (0..n)
            .filter(|&i| side[i] == s && core[i] > 0.7 && scalp[i] < 0.5)
            .map(|i| distance[i])
            .collect();
        let noise = if plain.len() >= 50 {
            quantile(plain, 0.9).unwrap_or(0.03)
        } else {
            0.03
        };
        let brow = quantile(hairs, 0.75).unwrap_or(noise);
        *level = (noise, noise + (brow - noise).max(0.02));
    }
    let anywhere: Vec<f32> = (0..n)
        .map(|i| {
            let (lo, hi) = levels[side[i]];
            smoothstep(lo, hi, distance[i]) * (1.0 - eyes[i])
        })
        .collect();
    let strand: Vec<f32> = (0..n).map(|i| anywhere[i] * zone[i]).collect();
    // The brow shape: where hairs gather, relative to each brow's core.
    let mut spread: Vec<[f32; 4]> = strand.iter().map(|&v| [v, 0.0, 0.0, 0.0]).collect();
    blur4(&mut spread, w, h, (e / 45.0).max(1.5));
    let mut gathered = [1.0f32; 2];
    for (s, g) in gathered.iter_mut().enumerate() {
        let inside: Vec<f32> = (0..n)
            .filter(|&i| side[i] == s && core[i] > 0.7 && scalp[i] < 0.5)
            .map(|i| spread[i][0])
            .collect();
        *g = quantile(inside, 0.75).unwrap_or(1.0).max(0.05);
    }
    let area: Vec<f32> = (0..n)
        .map(|i| smoothstep(0.12, 0.45, spread[i][0] / gathered[side[i]]) * zone[i])
        .collect();
    let mean = masked_blur(&colours, &area, w, h, (e / 50.0).max(1.5));
    BrowLayers {
        region: r,
        hair: (0..n).map(|i| to_u8(strand[i] * area[i])).collect(),
        area: area.into_iter().map(to_u8).collect(),
        mean: mean.into_iter().map(to_u16).collect(),
        skin: beneath.into_iter().map(to_u16).collect(),
        strand: anywhere.into_iter().map(to_u8).collect(),
        beneath_weight: weight.into_iter().map(to_u8).collect(),
        uncut: skin_here.into_iter().map(to_u8).collect(),
    }
}

/// What the skin sliders read that follows from the skin mask: the mask, how
/// deep inside it each pixel sits, the under-eye bands within it, the
/// skin-weighted frequency split and the face's skin tone. Rebuilt whole when
/// the brush edits the mask. Skin reaches past the face (neck, shoulders,
/// arms), so it has its own region holding the face's; only `under_eye` lies
/// in the face region.
#[derive(Clone)]
pub struct SkinLayers {
    pub(super) region: Region,
    pub(super) mask: Vec<u8>,
    pub(super) interior: Vec<u8>,
    pub(super) under_eye: Vec<u8>,
    pub(super) low1: Vec<[u16; 3]>,
    pub(super) low2: Vec<[u16; 3]>,
    pub(super) broad: Vec<u16>,
    /// Luma between the fine and mid splits: what of the mid band is shape
    /// (nose, folds, eye sockets) rather than grain and noise.
    pub(super) form: Vec<u16>,
    /// Luma at the scale of the face's big forms (cheekbones, brow, jaw).
    pub(super) huge: Vec<u16>,
    pub(super) mean: [f32; 3],
    pub(super) cheek_luma: f32,
    /// Forehead-to-chin extent of the face, in pixels.
    pub(super) extent: f32,
    /// Broad brightness (0..1) of the face's well-lit skin, which shaded
    /// skin is measured against; 0 when it could not be read.
    pub(super) lit: f32,
}

impl SkinLayers {
    pub fn mask(&self) -> &[u8] {
        &self.mask
    }

    /// Where `mask` lies in the image.
    pub fn region(&self) -> Region {
        self.region
    }
}

/// [`SkinLayers`] with the float lows and interior of the face region, which
/// the blemish search reads.
struct SkinSplit {
    low1: Vec<[f32; 3]>,
    low2: Vec<[f32; 3]>,
    interior: Vec<f32>,
    layers: SkinLayers,
}

/// Split the photo `src` of a skin `region` by its skin mask (0..1), weighted
/// to skin so hair and features do not bleed in. `under_band` lies in the
/// `face` region; `fallback` is the skin colour when the mask is empty.
#[allow(clippy::too_many_arguments)]
fn split_skin(
    src: &[[f32; 3]],
    skin: &[f32],
    under_band: &[u8],
    region: Region,
    face: Region,
    e: f32,
    points: &[[f32; 3]],
    fallback: [f32; 3],
) -> SkinSplit {
    let (w, h) = (region.w as usize, region.h as usize);
    let r1 = (e / 220.0).max(1.0);
    let r2 = (e / 28.0).max(3.0);
    let r3 = (e / 7.0).max(6.0);
    let r4 = (e / 2.5).max(12.0);
    let low1 = masked_blur(src, skin, w, h, r1);
    let low2 = masked_blur(src, skin, w, h, r2);
    let luma_band = |r: f32| -> Vec<u16> {
        masked_blur(src, skin, w, h, r)
            .into_par_iter()
            .map(|c| (luma(c).clamp(0.0, 1.0) * 65535.0).round() as u16)
            .collect()
    };
    let broad = luma_band(r3);
    let form = luma_band((e / 80.0).max(2.0));
    let huge = luma_band(r4);

    // The face's own tone: within its region, down to just below the chin.
    let (mut weight, mut mean) = (0.0f64, [0.0f64; 3]);
    for k in 0..tone_rows(points, face, e) * face.w as usize {
        let i = region.index_of(face, k);
        let (c, m) = (low2[i], skin[i]);
        weight += m as f64;
        for k in 0..3 {
            mean[k] += c[k] as f64 * m as f64;
        }
    }
    let mean = if weight > 1.0 {
        mean.map(|v| (v / weight) as f32)
    } else {
        fallback
    };
    let cheek_values: Vec<f32> = CHEEKS
        .iter()
        .filter_map(|&k| {
            let p = points[k as usize];
            let (x, y) = (p[0] - region.x as f32, p[1] - region.y as f32);
            (x >= 0.0 && y >= 0.0 && (x as usize) < w && (y as usize) < h)
                .then(|| luma(low2[y as usize * w + x as usize]))
        })
        .collect();
    let cheek_luma = if cheek_values.is_empty() {
        luma(mean)
    } else {
        cheek_values.iter().sum::<f32>() / cheek_values.len() as f32
    };

    // How deep inside the skin each pixel sits: 1 well inside, falling to 0 at
    // the outline, so smoothing fades out before it can halo the edges.
    let mut spread_mask: Vec<[f32; 4]> = skin.iter().map(|&m| [m, 0.0, 0.0, 0.0]).collect();
    blur4(&mut spread_mask, w, h, r2);
    let interior: Vec<f32> = spread_mask
        .par_iter()
        .zip(skin.par_iter())
        .map(|(b, &m)| smoothstep(0.55, 0.92, b[0]) * m)
        .collect();

    let lit = lit_skin(&broad, &interior, region, face, tone_rows(points, face, e));
    let layers = SkinLayers {
        region,
        mask: skin.par_iter().map(|&m| to_u8(m)).collect(),
        interior: interior.par_iter().map(|&v| to_u8(v)).collect(),
        under_eye: under_band
            .par_iter()
            .enumerate()
            .map(|(k, &b)| to_u8(b as f32 / 255.0 * skin[region.index_of(face, k)]))
            .collect(),
        low1: low1.par_iter().map(|&c| to_u16(c)).collect(),
        low2: low2.par_iter().map(|&c| to_u16(c)).collect(),
        broad,
        form,
        huge,
        mean,
        cheek_luma,
        extent: e,
        lit,
    };
    SkinSplit {
        low1: region.crop(&low1, face),
        low2: region.crop(&low2, face),
        interior: region.crop(&interior, face),
        layers,
    }
}

/// The brightness of the face's well-lit skin: the 70th percentile of the
/// skin's broad brightness (`broad`, over the skin `region`), well inside
/// the skin and within the first `rows` rows of the `face` region. Shaded
/// skin (under the chin, the neck, the side away from the lamp) is measured
/// against it. 0 without enough skin.
fn lit_skin(broad: &[u16], interior: &[f32], region: Region, face: Region, rows: usize) -> f32 {
    let mut histogram = [0.0f64; 256];
    let mut total = 0.0f64;
    for k in 0..rows * face.w as usize {
        let i = region.index_of(face, k);
        let weight = interior[i] as f64;
        if weight > 0.0 {
            histogram[(broad[i] >> 8) as usize] += weight;
            total += weight;
        }
    }
    if total < 50.0 {
        return 0.0;
    }
    let mut seen = 0.0;
    let level = histogram
        .iter()
        .position(|&w| {
            seen += w;
            seen >= 0.7 * total
        })
        .unwrap_or(255);
    (level as f32 + 0.5) / 256.0
}

impl PortraitModel {
    /// Brow layers of face `index` for a painted brow `area`: the hairs in
    /// it, its mean colour and the skin colour beneath it. `rgba` is the
    /// analysed image.
    pub fn brow_layers_from(&self, rgba: &[u8], index: usize, area: &[u8]) -> BrowLayers {
        let face = &self.faces[index];
        let b = &face.brows;
        let r = b.region;
        if area.len() != r.len() {
            return b.clone();
        }
        let (w, h, e) = (r.w as usize, r.h as usize, face.extent);
        let colours = read_region(rgba, self.width, r);
        let unit = |v: u8| v as f32 / 255.0;
        let shape: Vec<f32> = area.iter().map(|&a| unit(a)).collect();
        let weight: Vec<f32> = (0..r.len())
            .map(|i| unit(b.beneath_weight[i]) * (1.0 - shape[i]))
            .collect();
        let beneath = masked_blur(&colours, &weight, w, h, (e / 18.0).max(2.0));
        let mean = masked_blur(&colours, &shape, w, h, (e / 50.0).max(1.5));
        BrowLayers {
            region: r,
            hair: (0..r.len())
                .map(|i| to_u8(unit(b.strand[i]) * shape[i]))
                .collect(),
            area: area.to_vec(),
            mean: mean.into_iter().map(to_u16).collect(),
            skin: beneath.into_iter().map(to_u16).collect(),
            strand: b.strand.clone(),
            beneath_weight: b.beneath_weight.clone(),
            uncut: b.uncut.clone(),
        }
    }

    /// Skin layers of face `index` for an edited skin mask, built the way the
    /// analysis builds them. `rgba` is the analysed image.
    pub fn skin_layers_from(&self, rgba: &[u8], index: usize, mask: &[u8]) -> SkinLayers {
        let face = &self.faces[index];
        let r = face.skin.region;
        let src = read_region(rgba, self.width, r);
        let skin: Vec<f32> = mask.par_iter().map(|&m| m as f32 / 255.0).collect();
        split_skin(
            &src,
            &skin,
            &face.under_band,
            r,
            face.region,
            face.extent,
            &face.mesh.points,
            face.skin.mean,
        )
        .layers
    }
}

/// The pixels of `region` of an RGBA image `width` wide, as RGB in 0..1.
fn read_region(rgba: &[u8], width: u32, region: Region) -> Vec<[f32; 3]> {
    let w = region.w as usize;
    (0..region.len())
        .into_par_iter()
        .map(|i| {
            let o = ((region.y as usize + i / w) * width as usize + region.x as usize + i % w) * 4;
            [
                rgba[o] as f32 / 255.0,
                rgba[o + 1] as f32 / 255.0,
                rgba[o + 2] as f32 / 255.0,
            ]
        })
        .collect()
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

pub struct PortraitModel {
    pub width: u32,
    pub height: u32,
    pub faces: Vec<FaceModel>,
    /// The selection the analysis kept to, if any.
    pub clip: Option<Clip>,
    /// Whether the Sapiens2 part model ran (else every face uses mesh masks).
    pub parts_used: bool,
    pub parts_on_gpu: bool,
    /// Why the part model was skipped, if it was.
    pub parts_note: Option<String>,
    /// Milliseconds spent finding faces, loading and running the part model,
    /// and preparing the faces.
    pub timings: [u128; 4],
    /// What "Sửa màu & sáng" reads the photo's cast, exposure and haze from.
    pub light: super::correct::LightStats,
    /// The bodies below the faces, analysed once a body slider is first
    /// used (see [`super::body::analyze_bodies`]).
    pub bodies: std::sync::OnceLock<Result<Vec<Option<super::body::BodyModel>>, String>>,
}

pub(super) fn luma(c: [f32; 3]) -> f32 {
    0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
}

pub(super) fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn to_u16(c: [f32; 3]) -> [u16; 3] {
    c.map(|v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16)
}

fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Largest value within `radius` pixels along each axis (a square dilation).
fn max_filter(values: &[f32], width: usize, height: usize, radius: usize) -> Vec<f32> {
    if radius == 0 {
        return values.to_vec();
    }
    let mut rows = vec![0.0f32; values.len()];
    rows.par_chunks_mut(width)
        .enumerate()
        .for_each(|(y, line)| {
            let source = &values[y * width..(y + 1) * width];
            for (x, cell) in line.iter_mut().enumerate() {
                let lo = x.saturating_sub(radius);
                let hi = (x + radius + 1).min(width);
                *cell = source[lo..hi].iter().copied().fold(0.0, f32::max);
            }
        });
    let mut out = vec![0.0f32; values.len()];
    out.par_chunks_mut(width).enumerate().for_each(|(y, line)| {
        let lo = y.saturating_sub(radius);
        let hi = (y + radius + 1).min(height);
        for (x, cell) in line.iter_mut().enumerate() {
            *cell = (lo..hi).map(|yy| rows[yy * width + x]).fold(0.0, f32::max);
        }
    });
    out
}

/// Bilinear read at pixel (x, y) of a `gw` x `gh` grid of `cell`-pixel blocks.
fn grid_at(grid: &[[f32; 4]], gw: usize, gh: usize, cell: usize, x: usize, y: usize) -> [f32; 4] {
    let u = ((x as f32 + 0.5) / cell as f32 - 0.5).clamp(0.0, (gw - 1) as f32);
    let v = ((y as f32 + 0.5) / cell as f32 - 0.5).clamp(0.0, (gh - 1) as f32);
    let (x0, y0) = (u as usize, v as usize);
    let (x1, y1) = ((x0 + 1).min(gw - 1), (y0 + 1).min(gh - 1));
    let (fx, fy) = (u - x0 as f32, v - y0 as f32);
    let (a, b) = (grid[y0 * gw + x0], grid[y0 * gw + x1]);
    let (c, d) = (grid[y1 * gw + x0], grid[y1 * gw + x1]);
    std::array::from_fn(|k| {
        (a[k] * (1.0 - fx) + b[k] * fx) * (1.0 - fy) + (c[k] * (1.0 - fx) + d[k] * fx) * fy
    })
}

/// Guided filter of `mask` with a grey `guide`: snaps a soft mask onto the
/// photo's own edges. Never grows the mask where it had nothing nearby.
pub(super) fn guided(
    mask: &[f32],
    guide: &[f32],
    w: usize,
    h: usize,
    radius: f32,
    eps: f32,
) -> Vec<f32> {
    let mut stats: Vec<[f32; 4]> = mask
        .par_iter()
        .zip(guide.par_iter())
        .map(|(&p, &g)| [g, p, g * g, g * p])
        .collect();
    blur4(&mut stats, w, h, radius);
    let mut coefficients: Vec<[f32; 4]> = stats
        .par_iter()
        .map(|m| {
            let variance = (m[2] - m[0] * m[0]).max(0.0);
            let covariance = m[3] - m[0] * m[1];
            let a = covariance / (variance + eps);
            [a, m[1] - a * m[0], m[1], 0.0]
        })
        .collect();
    blur4(&mut coefficients, w, h, radius);
    coefficients
        .par_iter()
        .zip(guide.par_iter())
        .map(|(c, &g)| ((c[0] * g + c[1]).clamp(0.0, 1.0)) * smoothstep(0.0, 0.05, c[2]))
        .collect()
}

/// Edge-aware smoothing of a grey image (a guided filter guided by itself):
/// the regional tone Develop's Shadows/Blacks read, flat within a region yet
/// not bleeding across strong edges such as hair against skin.
fn edge_aware_base(values: &[f32], w: usize, h: usize, radius: f32, eps: f32) -> Vec<f32> {
    let mut stats: Vec<[f32; 4]> = values.par_iter().map(|&v| [v, v * v, 0.0, 0.0]).collect();
    blur4(&mut stats, w, h, radius);
    let mut coefficients: Vec<[f32; 4]> = stats
        .par_iter()
        .map(|m| {
            let variance = (m[1] - m[0] * m[0]).max(0.0);
            let a = variance / (variance + eps);
            [a, m[0] - a * m[0], 0.0, 0.0]
        })
        .collect();
    drop(stats);
    blur4(&mut coefficients, w, h, radius);
    coefficients
        .par_iter()
        .zip(values.par_iter())
        .map(|(c, &v)| c[0] * v + c[1])
        .collect()
}

/// Fade toward the region sides flagged in `open_sides` (left, top, right,
/// bottom): those sides lie inside the image, so an effect must not end there
/// in a hard line.
pub(super) fn side_fade(i: usize, w: usize, h: usize, open_sides: [bool; 4], fade: f32) -> f32 {
    let (x, y) = ((i % w) as f32 + 0.5, (i / w) as f32 + 0.5);
    let distances = [x, y, w as f32 - x, h as f32 - y];
    open_sides
        .iter()
        .zip(distances)
        .filter(|(open, _)| **open)
        .map(|(_, d)| smoothstep(0.0, fade, d))
        .product()
}

/// Near a mask's outline, keep only pixels whose colour matches the mask's
/// solid core: a chroma model (mean and covariance of two colour-difference
/// axes) from pixels deep inside, then a Mahalanobis cut. The core itself is
/// left alone, so off-colour spots inside (a red pimple) still count. Chroma
/// is read slightly blurred: JPEG stores it in coarse blocks, which would
/// otherwise cut the outline into squares.
fn gate_by_colour(mask: &[f32], colours: &[[f32; 3]], w: usize, h: usize, radius: f32) -> Vec<f32> {
    let n = w * h;
    let mut core: Vec<[f32; 4]> = mask.iter().map(|&m| [m, 0.0, 0.0, 0.0]).collect();
    blur4(&mut core, w, h, radius);
    let core: Vec<f32> = core
        .par_iter()
        .map(|c| smoothstep(0.7, 0.95, c[0]))
        .collect();
    let mut chroma: Vec<[f32; 4]> = colours
        .par_iter()
        .map(|&c| {
            let y = luma(c);
            [c[2] - y, c[0] - y, 0.0, 0.0]
        })
        .collect();
    blur4(&mut chroma, w, h, radius / 6.0);
    let (mut total, mut mean, mut cov) = (0.0f64, [0.0f64; 2], [0.0f64; 3]);
    for i in 0..n {
        if core[i] > 0.9 && mask[i] > 0.9 {
            let [u, v, _, _] = chroma[i];
            total += 1.0;
            mean[0] += u as f64;
            mean[1] += v as f64;
            cov[0] += (u * u) as f64;
            cov[1] += (u * v) as f64;
            cov[2] += (v * v) as f64;
        }
    }
    if total <= 200.0 {
        return mask.to_vec();
    }
    let (mu, mv) = (mean[0] / total, mean[1] / total);
    let floor = 1e-5;
    let (suu, suv, svv) = (
        (cov[0] / total - mu * mu).max(floor) as f32,
        (cov[1] / total - mu * mv) as f32,
        (cov[2] / total - mv * mv).max(floor) as f32,
    );
    let (mu, mv) = (mu as f32, mv as f32);
    let det = (suu * svv - suv * suv).max(1e-12);
    (0..n)
        .into_par_iter()
        .map(|i| {
            let [u, v, _, _] = chroma[i];
            let (du, dv) = (u - mu, v - mv);
            let d2 = (svv * du * du - 2.0 * suv * du * dv + suu * dv * dv) / det;
            let gate = 1.0 - smoothstep(9.0, 25.0, d2);
            mask[i] * (core[i] + (1.0 - core[i]) * gate)
        })
        .collect()
}

/// Tighten the soft, low-resolution skin mask onto the photo: a guided filter
/// (luma guide) snaps its outline to real edges such as the hairline, pixels
/// near the outline must also have the face's skin colour, and the mask fades
/// out toward region sides inside the image.
fn refine_skin(
    mask: Vec<f32>,
    src: &[[f32; 3]],
    w: usize,
    h: usize,
    e: f32,
    open_sides: [bool; 4],
) -> Vec<f32> {
    let n = w * h;
    let guide: Vec<f32> = src.par_iter().map(|&c| luma(c)).collect();
    let snapped = guided(&mask, &guide, w, h, (e / 80.0).max(2.0), 0.0015);
    let gated = gate_by_colour(&snapped, src, w, h, (e / 28.0).max(3.0));
    let fade = (0.06 * e).max(4.0);
    (0..n)
        .into_par_iter()
        .map(|i| gated[i] * side_fade(i, w, h, open_sides, fade))
        .collect()
}

/// A blemish found by the ring test, in region pixels.
struct Spot {
    x: f32,
    y: f32,
    score: f32,
    radius: f32,
}

/// Visit the pixels within `reach` of (cx, cy) with their distance.
fn for_disc(w: usize, h: usize, cx: f32, cy: f32, reach: f32, mut visit: impl FnMut(usize, f32)) {
    let x0 = (cx - reach).floor().max(0.0) as usize;
    let y0 = (cy - reach).floor().max(0.0) as usize;
    let x1 = ((cx + reach).ceil() as usize).min(w);
    let y1 = ((cy + reach).ceil() as usize).min(h);
    for y in y0..y1 {
        for x in x0..x1 {
            let d = (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - cy);
            if d <= reach {
                visit(y * w + x, d);
            }
        }
    }
}

/// Ring-test peaks become spots. Peaks packed densely (stubble, nose pores)
/// are skin texture rather than blemishes, so they are damped.
fn find_spots(ring: &[(f32, usize)], radii: &[f32; 2], w: usize, h: usize, e: f32) -> Vec<Spot> {
    // The most permissive slider threshold; weaker peaks never matter.
    const FLOOR: f32 = 0.3;
    let raw: Vec<f32> = ring.iter().map(|r| r.0).collect();
    let local = max_filter(&raw, w, h, 2);
    let mut peaks: Vec<Spot> = (0..raw.len())
        .filter(|&i| raw[i] >= FLOOR && raw[i] >= local[i])
        .map(|i| Spot {
            x: (i % w) as f32 + 0.5,
            y: (i / w) as f32 + 0.5,
            score: raw[i],
            radius: radii[ring[i].1] * 0.9,
        })
        .collect();
    peaks.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Spot> = Vec::new();
    for p in peaks {
        if kept.len() >= 3000 {
            break;
        }
        if kept
            .iter()
            .all(|k| (k.x - p.x).hypot(k.y - p.y) > k.radius.max(p.radius) * 0.8)
        {
            kept.push(p);
        }
    }
    let crowd = e / 12.0;
    let neighbours: Vec<usize> = kept
        .iter()
        .map(|a| {
            kept.iter()
                .filter(|b| (a.x - b.x).hypot(a.y - b.y) < crowd)
                .count()
                - 1
        })
        .collect();
    for (spot, count) in kept.iter_mut().zip(neighbours) {
        spot.score *= (1.0 - count.saturating_sub(5) as f32 * 0.12).clamp(0.2, 1.0);
    }
    kept
}

/// Per spot, pick nearby clean skin to borrow texture from (toward the face
/// centre first), and paint soft discs of spot strength, coverage and donor
/// offset. Also the local skin colour around spots, measured without them.
#[allow(clippy::type_complexity)]
fn heal_spots(
    spots: &[Spot],
    src: &[[f32; 3]],
    interior: &[f32],
    w: usize,
    h: usize,
    centre: [f32; 2],
    small_radius: f32,
) -> (Vec<u8>, Vec<u8>, Vec<[i16; 2]>, Vec<[u16; 3]>) {
    let n = w * h;
    let mut score = vec![0u8; n];
    let mut cover = vec![0u8; n];
    if spots.is_empty() {
        return (score, cover, Vec::new(), Vec::new());
    }
    let mut taken = vec![0.0f32; n];
    for s in spots {
        for_disc(w, h, s.x, s.y, s.radius * 1.4, |i, _| taken[i] = 1.0);
    }
    let clean = |x: f32, y: f32| {
        if x < 0.0 || y < 0.0 || x >= w as f32 || y >= h as f32 {
            return false;
        }
        let i = y as usize * w + x as usize;
        interior[i] >= 0.8 && taken[i] < 0.5
    };
    let mut donor = vec![[0i16; 2]; n];
    let mut owner = vec![0.0f32; n];
    for s in spots {
        let (mut bx, mut by) = (centre[0] - s.x, centre[1] - s.y);
        let length = bx.hypot(by);
        if length > 1e-3 {
            bx /= length;
            by /= length;
        } else {
            (bx, by) = (1.0, 0.0);
        }
        let reach = s.radius * 2.5 + 2.0;
        let mut offset = [0i16; 2];
        for degrees in [0.0f32, 60.0, -60.0, 120.0, -120.0, 180.0] {
            let (sin, cos) = degrees.to_radians().sin_cos();
            let (dx, dy) = ((bx * cos - by * sin) * reach, (bx * sin + by * cos) * reach);
            let (qx, qy) = (s.x + dx, s.y + dy);
            let r = s.radius;
            if [(0.0, 0.0), (r, 0.0), (-r, 0.0), (0.0, r), (0.0, -r)]
                .iter()
                .all(|(ox, oy)| clean(qx + ox, qy + oy))
            {
                offset = [dx.round() as i16, dy.round() as i16];
                break;
            }
        }
        let feather = (s.radius * 0.35).max(1.5);
        for_disc(w, h, s.x, s.y, s.radius + feather, |i, d| {
            let c = smoothstep(s.radius + feather, s.radius - feather, d);
            if c <= 0.0 {
                return;
            }
            cover[i] = cover[i].max(to_u8(c));
            if s.score > owner[i] {
                owner[i] = s.score;
                score[i] = (s.score * BLEMISH_SCALE).round().clamp(0.0, 255.0) as u8;
                donor[i] = offset;
            }
        });
    }
    let mut colour: Vec<[f32; 4]> = (0..n)
        .into_par_iter()
        .map(|i| {
            let weight = interior[i] * (1.0 - taken[i]) + 1e-4;
            let c = src[i];
            [c[0] * weight, c[1] * weight, c[2] * weight, weight]
        })
        .collect();
    blur4(&mut colour, w, h, (small_radius * 1.5).max(2.0));
    let heal_base = colour
        .into_par_iter()
        .map(|a| to_u16([a[0] / a[3], a[1] / a[3], a[2] / a[3]]))
        .collect();
    (score, cover, donor, heal_base)
}

/// Faces in `clip` and a margin around it (a close crop finds small faces in
/// a group photo more surely), kept when centred in the selection.
pub fn faces_in(
    rgba: &[u8],
    width: u32,
    height: u32,
    clip: &Clip,
) -> Result<Vec<FaceMesh>, String> {
    let r = clip.region;
    let area = Region::around(
        [
            [r.x as f32, r.y as f32],
            [(r.x + r.w) as f32, (r.y + r.h) as f32],
        ]
        .into_iter(),
        [
            0.25 * r.w as f32,
            0.25 * r.h as f32,
            0.25 * r.w as f32,
            0.25 * r.h as f32,
        ],
        width,
        height,
    );
    let row = area.w as usize * 4;
    let mut crop = vec![0u8; row * area.h as usize];
    for (y, line) in crop.chunks_exact_mut(row).enumerate() {
        let o = ((area.y as usize + y) * width as usize + area.x as usize) * 4;
        line.copy_from_slice(&rgba[o..o + row]);
    }
    let mut meshes = face_mesh::detect(&crop, area.w, area.h)?;
    for mesh in &mut meshes {
        for p in &mut mesh.points {
            p[0] += area.x as f32;
            p[1] += area.y as f32;
        }
    }
    meshes.retain(|mesh| {
        let (c, _, _) = mesh.frame();
        c[0] >= 0.0 && c[1] >= 0.0 && clip.at(c[0] as u32, c[1] as u32) >= 0.5
    });
    Ok(meshes)
}

/// Analyse `rgba` (straight RGBA, `width * height`), keeping to `clip` when
/// given. `progress` receives short Vietnamese status lines for the dialog.
pub fn analyze(
    rgba: &[u8],
    width: u32,
    height: u32,
    prefer_gpu: bool,
    clip: Option<Clip>,
    progress: &(dyn Fn(String) + Sync),
) -> Result<PortraitModel, String> {
    progress("Đang tìm khuôn mặt…".to_string());
    let mut timings = [0u128; 4];
    let started = std::time::Instant::now();
    let meshes = match &clip {
        Some(c) => faces_in(rgba, width, height, c)?,
        None => face_mesh::detect(rgba, width, height)?,
    };
    timings[0] = started.elapsed().as_millis();
    if meshes.is_empty() {
        return Err(if clip.is_some() {
            "không tìm thấy khuôn mặt nào trong vùng chọn".to_string()
        } else {
            "không tìm thấy khuôn mặt nào".to_string()
        });
    }
    let frame = clip.as_ref().map(Clip::bounds);
    let mut parts: Vec<Option<PartLabels>> = (0..meshes.len()).map(|_| None).collect();
    let mut parts_on_gpu = false;
    let mut parts_note = None;
    if body_parts::model_path().is_some() {
        progress("Đang nạp model tách vùng…".to_string());
        let started = std::time::Instant::now();
        match Segmenter::load(prefer_gpu) {
            Ok(mut segmenter) => {
                timings[1] = started.elapsed().as_millis();
                let started = std::time::Instant::now();
                for (i, mesh) in meshes.iter().enumerate() {
                    progress(format!("Đang tách vùng mặt {}/{}…", i + 1, meshes.len()));
                    match segmenter.segment_face_within(rgba, width, height, mesh, frame) {
                        Ok(labels) => parts[i] = Some(labels),
                        Err(error) => parts_note = Some(error),
                    }
                }
                parts_on_gpu = segmenter.on_gpu;
                timings[2] = started.elapsed().as_millis();
            }
            Err(error) => parts_note = Some(error),
        }
    } else {
        parts_note = Some("chưa có model tách vùng Sapiens2 — dùng mốc mặt".to_string());
    }
    let parts_used = parts.iter().any(Option::is_some);
    let owners: Vec<([f32; 2], f32)> = meshes
        .iter()
        .map(|mesh| {
            let (centre, extent, _) = mesh.frame();
            (centre, extent)
        })
        .collect();
    let started = std::time::Instant::now();
    let mut faces = Vec::with_capacity(meshes.len());
    let count = meshes.len();
    for (i, (mesh, part)) in meshes.into_iter().zip(parts).enumerate() {
        progress(format!("Đang chuẩn bị mặt {}/{}…", i + 1, count));
        faces.push(build_face(
            rgba,
            width,
            height,
            mesh,
            part.as_ref(),
            i,
            &owners,
        ));
    }
    timings[3] = started.elapsed().as_millis();
    let light = super::correct::LightStats::measure(rgba, width, &faces);
    Ok(PortraitModel {
        width,
        height,
        light,
        faces,
        clip,
        parts_used,
        parts_on_gpu,
        parts_note,
        timings,
        bodies: std::sync::OnceLock::new(),
    })
}

fn build_face(
    rgba: &[u8],
    width: u32,
    height: u32,
    mesh: FaceMesh,
    parts: Option<&PartLabels>,
    index: usize,
    owners: &[([f32; 2], f32)],
) -> FaceModel {
    let (_, extent, angle) = mesh.frame();
    let e = extent;
    let agreement = parts.map(|p| p.agreement);
    let trusted = agreement.is_some_and(|a| a >= TRUSTED_AGREEMENT);
    // The features down to just below the chin; skin, with the part model,
    // wherever it sees this person's skin.
    let region = Region::around(
        mesh.points.iter().map(|p| [p[0], p[1]]),
        [0.15 * e, 0.15 * e, 0.15 * e, TONE_BELOW_CHIN * e],
        width,
        height,
    );
    let skin_region = match parts {
        Some(p) if trusted => skin_region(rgba, p, region, owners, index, width, height),
        _ => region,
    };
    let (w, h) = (region.w as usize, region.h as usize);
    let n = region.len();
    let pixel_xy = |i: usize| {
        (
            region.x as f32 + (i % w) as f32 + 0.5,
            region.y as f32 + (i / w) as f32 + 0.5,
        )
    };
    let (sw, sn) = (skin_region.w as usize, skin_region.len());
    let skin_xy = |i: usize| {
        (
            skin_region.x + (i % sw) as u32,
            skin_region.y + (i / sw) as u32,
        )
    };
    let skin_src = read_region(rgba, width, skin_region);
    let src = skin_region.crop(&skin_src, region);
    let points = &mesh.points;

    // Features that are never skin.
    let mut exclude = vec![0.0f32; n];
    for eye in [&RIGHT_EYE[..], &LEFT_EYE[..]] {
        stamp_polygon(
            &mut exclude,
            region,
            &loop_points(points, eye),
            0.02 * e,
            0.02 * e,
        );
    }
    for brow in [&RIGHT_BROW[..], &LEFT_BROW[..]] {
        stamp_polygon(
            &mut exclude,
            region,
            &loop_points(points, brow),
            0.015 * e,
            0.02 * e,
        );
    }
    let wing_a = points[NOSE_WINGS[0] as usize];
    let wing_b = points[NOSE_WINGS[1] as usize];
    let below = points[SUBNASALE as usize];
    let nostril_centre = [
        (wing_a[0] + wing_b[0]) * 0.5 * 0.6 + below[0] * 0.4,
        (wing_a[1] + wing_b[1]) * 0.5 * 0.6 + below[1] * 0.4,
    ];
    let half_width = ((wing_b[0] - wing_a[0]).hypot(wing_b[1] - wing_a[1]) * 0.5).max(1.0);
    let (sin, cos) = angle.sin_cos();
    let nostrils: Vec<[f32; 2]> = (0..24)
        .map(|k| {
            let t = k as f32 / 24.0 * std::f32::consts::TAU;
            let (u, v) = (t.cos() * half_width * 0.95, t.sin() * half_width * 0.42);
            [
                nostril_centre[0] + cos * u - sin * v,
                nostril_centre[1] + sin * u + cos * v,
            ]
        })
        .collect();
    let mut nostril_area = vec![0.0f32; n];
    stamp_polygon(&mut nostril_area, region, &nostrils, 0.0, 0.03 * e);

    // Cheek reference colour from the source.
    let cheek_samples: Vec<[f32; 3]> = CHEEKS
        .iter()
        .filter_map(|&k| {
            let p = points[k as usize];
            let (x, y) = (p[0] - region.x as f32, p[1] - region.y as f32);
            (x >= 0.0 && y >= 0.0 && (x as usize) < w && (y as usize) < h)
                .then(|| src[y as usize * w + x as usize])
        })
        .collect();
    let cheek_colour = if cheek_samples.is_empty() {
        [0.8, 0.6, 0.5]
    } else {
        let mut sum = [0.0f32; 3];
        for c in &cheek_samples {
            for k in 0..3 {
                sum[k] += c[k] / cheek_samples.len() as f32;
            }
        }
        sum
    };

    let open_sides = [
        skin_region.x > 0,
        skin_region.y > 0,
        skin_region.x + skin_region.w < width,
        skin_region.y + skin_region.h < height,
    ];
    // Skin from the photo's own colours; the part model's mask (or, without
    // it, the face outline limited to cheek-coloured pixels) when the photo
    // has too little colour to go on.
    let coloured = if skin_mask::legacy() {
        None
    } else {
        skin_mask::skin_mask(&SkinInputs {
            src: &skin_src,
            region: skin_region,
            face: region,
            extent: e,
            points,
            parts: parts.filter(|_| trusted),
            owners,
            index,
            open_sides,
        })
    };
    let refined = match coloured {
        Some(mask) => {
            exclude.fill(0.0);
            mask
        }
        None => {
            let mut oval = vec![0.0f32; sn];
            if !trusted {
                stamp_polygon(
                    &mut oval,
                    skin_region,
                    &loop_points(points, &FACE_OVAL),
                    -0.01 * e,
                    0.05 * e,
                );
                stamp_polygon(
                    &mut exclude,
                    region,
                    &loop_points(points, &LIPS_OUTER),
                    0.01 * e,
                    0.02 * e,
                );
            }
            let cheek_chroma = {
                let y = luma(cheek_colour);
                cheek_colour.map(|v| v - y)
            };
            let raw_skin: Vec<f32> = (0..sn)
                .into_par_iter()
                .map(|i| {
                    let (x, y) = skin_xy(i);
                    match parts {
                        Some(p) if trusted => {
                            let g = p.groups_at(x as f32 + 0.5, y as f32 + 0.5);
                            (g[body_parts::GROUP_FACE_SKIN] + g[body_parts::GROUP_BODY_SKIN])
                                .min(1.0)
                        }
                        _ => {
                            let c = skin_src[i];
                            let yl = luma(c);
                            let distance = (0..3)
                                .map(|k| (c[k] - yl - cheek_chroma[k]).powi(2))
                                .sum::<f32>()
                                .sqrt();
                            oval[i] * (1.0 - smoothstep(0.05, 0.12, distance))
                        }
                    }
                })
                .collect();
            refine_skin(
                raw_skin,
                &skin_src,
                sw,
                skin_region.h as usize,
                e,
                open_sides,
            )
        }
    };
    let cheek_luma_src = luma(cheek_colour).max(0.05);
    let own_centre = owners[index];
    let skin: Vec<f32> = (0..sn)
        .into_par_iter()
        .map(|i| {
            let (px, py) = skin_xy(i);
            let (x, y) = (px as f32 + 0.5, py as f32 + 0.5);
            // Pixels nearer another face (in face sizes) belong to that face.
            let own = ((x - own_centre.0[0]).hypot(y - own_centre.0[1])) / own_centre.1;
            if owners
                .iter()
                .enumerate()
                .any(|(j, (c, s))| j != index && (x - c[0]).hypot(y - c[1]) / s < own)
            {
                return 0.0;
            }
            let Some(f) = region.index_at(px, py) else {
                return refined[i];
            };
            // Only the dark nostril holes leave the skin, not the nose around them.
            let hole =
                nostril_area[f] * (1.0 - smoothstep(0.6, 0.85, luma(src[f]) / cheek_luma_src));
            refined[i] * (1.0 - exclude[f]) * (1.0 - hole)
        })
        .collect();
    drop(refined);
    let brows = brow_layers(
        &src,
        region,
        &skin,
        skin_region,
        points,
        parts.filter(|_| trusted),
        [-sin, cos],
        e,
    );
    // Brows stay as shot under the skin retouch.
    let mut skin = skin;
    for (k, &a) in brows.area.iter().enumerate() {
        skin[skin_region.index_of(brows.region, k)] *= 1.0 - a as f32 / 255.0;
    }

    // Eyes: whites and irises.
    let mut eye_area = vec![0.0f32; n];
    let mut irises = vec![0.0f32; n];
    for (eye, (centre, rim)) in [(&RIGHT_EYE[..], RIGHT_IRIS), (&LEFT_EYE[..], LEFT_IRIS)] {
        stamp_polygon(
            &mut eye_area,
            region,
            &loop_points(points, eye),
            -0.004 * e,
            0.01 * e,
        );
        let c = points[centre as usize];
        let radius = rim
            .iter()
            .map(|&k| {
                let p = points[k as usize];
                (p[0] - c[0]).hypot(p[1] - c[1])
            })
            .sum::<f32>()
            / 4.0;
        stamp_disc(&mut irises, region, [c[0], c[1]], radius * 1.05, 0.008 * e);
    }
    let sclera: Vec<u8> = (0..n)
        .into_par_iter()
        .map(|i| to_u8(eye_area[i] * (1.0 - irises[i])))
        .collect();
    let eye_white: Vec<u8> = (0..n)
        .into_par_iter()
        .map(|i| {
            let y = luma(src[i]);
            to_u8(eye_area[i] * (1.0 - irises[i]) * smoothstep(0.28, 0.5, y))
        })
        .collect();
    let mut eye_lids = vec![0.0f32; n];
    for eye in [&RIGHT_EYE[..], &LEFT_EYE[..]] {
        stamp_polygon(
            &mut eye_lids,
            region,
            &loop_points(points, eye),
            0.0,
            0.01 * e,
        );
    }
    let iris: Vec<u8> = (0..n)
        .into_par_iter()
        .map(|i| to_u8(irises[i] * eye_lids[i]))
        .collect();

    // Under-eye bands hanging below each lower lid.
    let down = [-sin, cos];
    let mut bands = vec![0.0f32; n];
    for lower in [&RIGHT_EYE_LOWER[..], &LEFT_EYE_LOWER[..]] {
        let lid = loop_points(points, lower);
        let last = (lid.len() - 1) as f32;
        let mut band: Vec<[f32; 2]> = lid
            .iter()
            .map(|p| [p[0] + down[0] * 0.015 * e, p[1] + down[1] * 0.015 * e])
            .collect();
        for (k, p) in lid.iter().enumerate().rev() {
            let t = k as f32 / last * 2.0 - 1.0;
            let depth = 0.015 * e + 0.11 * e * (1.0 - 0.45 * t * t);
            band.push([p[0] + down[0] * depth, p[1] + down[1] * depth]);
        }
        stamp_polygon(&mut bands, region, &band, 0.0, 0.05 * e);
    }
    let under_band: Vec<u8> = bands.into_par_iter().map(to_u8).collect();

    // Teeth: bright pixels inside the mouth.
    let mut mouth = vec![0.0f32; n];
    if !trusted {
        stamp_polygon(
            &mut mouth,
            region,
            &loop_points(points, &MOUTH_INNER),
            0.0,
            0.01 * e,
        );
    }
    let teeth: Vec<u8> = (0..n)
        .into_par_iter()
        .map(|i| {
            let c = src[i];
            let y = luma(c);
            let (x, py) = pixel_xy(i);
            let base = match parts {
                Some(p) if trusted => p.groups_at(x, py)[body_parts::GROUP_TEETH],
                _ => {
                    let max = c[0].max(c[1]).max(c[2]);
                    let min = c[0].min(c[1]).min(c[2]);
                    let saturation = if max > 0.0 { (max - min) / max } else { 0.0 };
                    mouth[i] * (1.0 - smoothstep(0.2, 0.45, saturation))
                }
            };
            to_u8(base * smoothstep(0.25, 0.5, y))
        })
        .collect();

    let SkinSplit {
        low1,
        low2,
        interior,
        layers: skin_layers,
    } = split_skin(
        &skin_src,
        &skin,
        &under_band,
        skin_region,
        region,
        e,
        points,
        cheek_colour,
    );
    drop(skin_src);
    let cheek_luma = skin_layers.cheek_luma;

    // Blemishes: compact spots darker or redder than a ring around them in
    // every direction (creases and outlines fail the ring test along their
    // length), in units of the face's own skin texture spread. Only inside the
    // face outline (not ears or the jaw edge) and away from the nose wings.
    let (mut spread, mut spread_weight) = (0.0f64, 0.0f64);
    for i in 0..n {
        let m = interior[i] as f64;
        let d = luma(low1[i]) - luma(low2[i]);
        spread += (d * d) as f64 * m;
        spread_weight += m;
    }
    let sigma = ((spread / spread_weight.max(1.0)).sqrt() as f32).max(1.0 / 255.0);
    let tone: Vec<[f32; 2]> = low1
        .par_iter()
        .map(|c| [luma(*c), c[0] - (c[1] + c[2]) * 0.5])
        .collect();
    let mut zone = vec![0.0f32; n];
    stamp_polygon(
        &mut zone,
        region,
        &loop_points(points, &FACE_OVAL),
        -0.04 * e,
        0.02 * e,
    );
    let mut wings = vec![0.0f32; n];
    for &k in &NOSE_WINGS {
        let p = points[k as usize];
        stamp_disc(&mut wings, region, [p[0], p[1]], 0.06 * e, 0.02 * e);
    }
    let radii = [(e / 110.0).max(2.0), (e / 55.0).max(3.0)];
    let directions: Vec<[f32; 2]> = (0..8)
        .map(|k| {
            let a = k as f32 * std::f32::consts::FRAC_PI_4;
            [a.cos(), a.sin()]
        })
        .collect();
    let ring: Vec<(f32, usize)> = (0..n)
        .into_par_iter()
        .map(|i| {
            if interior[i] < 0.8 || zone[i] * (1.0 - wings[i]) < 0.5 {
                return (0.0, 0);
            }
            let (x, y) = ((i % w) as f32, (i / w) as f32);
            let centre = tone[i];
            let (mut best, mut which) = (0.0f32, 0usize);
            for (k, radius) in radii.iter().enumerate() {
                let (mut dark, mut red, mut brightest) = (f32::MAX, f32::MAX, f32::MIN);
                for d in &directions {
                    let (sx, sy) = (x + d[0] * radius, y + d[1] * radius);
                    if sx < 0.0 || sy < 0.0 || sx >= w as f32 || sy >= h as f32 {
                        return (0.0, 0);
                    }
                    let ring = tone[sy as usize * w + sx as usize];
                    dark = dark.min(ring[0] - centre[0]);
                    red = red.min(centre[1] - ring[1]);
                    brightest = brightest.max(ring[0] - centre[0]);
                }
                // A spot sits in even skin; one beside a crease or shadow edge
                // sees a lopsided ring, and healing it would lift the crease.
                let depth = dark.max(0.0);
                let even = depth / (depth + (brightest - dark) + 1e-6);
                let score = (depth + 0.5 * red.max(0.0)) / sigma * smoothstep(0.3, 0.55, even);
                if score > best {
                    best = score;
                    which = k;
                }
            }
            (best, which)
        })
        .collect();
    let spots = find_spots(&ring, &radii, w, h, e);
    let own = owners[index].0;
    let (spot_score, spot_cover, donor, heal_base) = heal_spots(
        &spots,
        &src,
        &interior,
        w,
        h,
        [own[0] - region.x as f32, own[1] - region.y as f32],
        radii[0],
    );

    // Lips and the sharpening zone.
    let mut lip_shape = vec![0.0f32; n];
    let mut mouth_hole = vec![0.0f32; n];
    if !trusted {
        stamp_polygon(
            &mut lip_shape,
            region,
            &loop_points(points, &LIPS_OUTER),
            0.0,
            0.012 * e,
        );
        stamp_polygon(
            &mut mouth_hole,
            region,
            &loop_points(points, &MOUTH_INNER),
            0.0,
            0.01 * e,
        );
    }
    let lips: Vec<u8> = (0..n)
        .into_par_iter()
        .map(|i| {
            let (x, y) = pixel_xy(i);
            match parts {
                Some(p) if trusted => to_u8(p.groups_at(x, y)[body_parts::GROUP_LIPS]),
                _ => to_u8(lip_shape[i] * (1.0 - mouth_hole[i])),
            }
        })
        .collect();
    let mut detail_shape = vec![0.0f32; n];
    for eye in [&RIGHT_EYE[..], &LEFT_EYE[..]] {
        stamp_polygon(
            &mut detail_shape,
            region,
            &loop_points(points, eye),
            0.035 * e,
            0.02 * e,
        );
    }
    stamp_polygon(
        &mut detail_shape,
        region,
        &loop_points(points, &LIPS_OUTER),
        0.01 * e,
        0.015 * e,
    );
    let detail: Vec<u8> = detail_shape.into_par_iter().map(to_u8).collect();
    let mut soft4: Vec<[f32; 4]> = src.iter().map(|c| [c[0], c[1], c[2], 0.0]).collect();
    blur4(&mut soft4, w, h, (e / 350.0).max(1.0));
    let soft: Vec<[u16; 3]> = soft4
        .into_par_iter()
        .map(|c| to_u16([c[0], c[1], c[2]]))
        .collect();

    let nose = nose_contour(points, region, e);

    // Hair: a broad soft zone over the part model's whole head-and-shoulders
    // crop, with no outline cut. As with Develop's Shadows/Blacks, each
    // pixel's own tone decides how much it changes: fully where the model is
    // sure of hair, elsewhere by how much darker it is than the skin next to
    // it (as a ratio, so sideburns beside a shaded temple count), so strands
    // over the forehead change and the skin between them not. Against the
    // backdrop, each pixel's share of hair is where its colour sits between
    // the backdrop's and the hair's there, as Refine Edge does, so stray
    // strands past the model's coarse outline count and backdrop the model
    // took for hair does not.
    let (hair_region, hair, hair_base) = match parts {
        Some(p) if trusted => {
            let [x0, y0, x1, y1] = p.head_bounds();
            let hr = Region::around([[x0, y0], [x1, y1]].into_iter(), [0.0; 4], width, height);
            let (hw, hh) = (hr.w as usize, hr.h as usize);
            let at = |i: usize| {
                (
                    hr.x as f32 + (i % hw) as f32 + 0.5,
                    hr.y as f32 + (i / hw) as f32 + 0.5,
                )
            };
            let colours: Vec<[f32; 3]> = (0..hr.len())
                .into_par_iter()
                .map(|i| {
                    let (x, y) = at(i);
                    let o = (y as usize * width as usize + x as usize) * 4;
                    [
                        rgba[o] as f32 / 255.0,
                        rgba[o + 1] as f32 / 255.0,
                        rgba[o + 2] as f32 / 255.0,
                    ]
                })
                .collect();
            let own_centre = owners[index];
            let colour_skin_at =
                |x: f32, y: f32| skin_region.index_at(x as u32, y as u32).map(|i| skin[i]);
            // Neighbourhood sums on a grid of `cell`-pixel blocks (the part
            // model is no finer), per block: sure hair (never where the model
            // sees skin: faint hair odds spread over blond or grey-haired
            // foreheads), skin-weighted brightness and skin weight for the
            // local skin tone, sure backdrop with its colour and squared
            // colour, hair-weighted colour, and the pixel count.
            let cell = ((e / 150.0).round() as usize).max(1);
            let (gw, gh) = (hw.div_ceil(cell), hh.div_ceil(cell));
            // Per pixel: whether the model sees this person at all (not
            // backdrop), whether it may be a strand against the backdrop (not
            // skin or clothes), the colour skin mask, and whether the model is
            // certain of hair (a strand across a brow end).
            let mut weights = vec![[0u8; 4]; hr.len()];
            let sums: Vec<[f32; 12]> = weights
                .par_chunks_mut(cell * hw)
                .enumerate()
                .map(|(gy, rows)| {
                    let mut line = vec![[0.0f32; 12]; gw];
                    for (j, out) in rows.iter_mut().enumerate() {
                        let i = gy * cell * hw + j;
                        let (x, y) = at(i);
                        let sum = &mut line[(j % hw) / cell];
                        sum[11] += 1.0;
                        let own = (x - own_centre.0[0]).hypot(y - own_centre.0[1]) / own_centre.1;
                        if owners
                            .iter()
                            .enumerate()
                            .any(|(k, (c, s))| k != index && (x - c[0]).hypot(y - c[1]) / s < own)
                        {
                            continue;
                        }
                        let g = p.groups_at(x, y);
                        let part_skin =
                            g[body_parts::GROUP_FACE_SKIN] + g[body_parts::GROUP_BODY_SKIN];
                        let hair = g[body_parts::GROUP_HAIR];
                        let not_skin = 1.0 - smoothstep(0.2, 0.5, part_skin);
                        let sure = smoothstep(0.35, 0.65, hair) * not_skin;
                        let colour_skin = colour_skin_at(x, y);
                        let tone_weight = colour_skin.unwrap_or(part_skin);
                        let back = smoothstep(0.5, 0.9, g[body_parts::GROUP_BACKDROP]);
                        let c = colours[i];
                        let add = [
                            sure,
                            tone_weight * luma(c),
                            tone_weight,
                            back * (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]),
                            back,
                            back * c[0],
                            back * c[1],
                            back * c[2],
                            sure * c[0],
                            sure * c[1],
                            sure * c[2],
                        ];
                        for (total, value) in sum.iter_mut().zip(add) {
                            *total += value;
                        }
                        let colour_skin = colour_skin.unwrap_or(0.0);
                        *out = [
                            to_u8(smoothstep(0.3, 0.7, hair + part_skin)),
                            to_u8(
                                smoothstep(0.6, 0.85, hair + g[body_parts::GROUP_BACKDROP])
                                    * not_skin
                                    * (1.0 - colour_skin),
                            ),
                            to_u8(colour_skin),
                            to_u8(smoothstep(0.6, 0.9, hair) * not_skin),
                        ];
                    }
                    line
                })
                .collect::<Vec<_>>()
                .concat();
            let spread = |mut grid: Vec<[f32; 4]>, sigma: f32| {
                blur4(&mut grid, gw, gh, sigma / cell as f32);
                grid
            };
            let mean = |s: &[f32; 12], k: usize| s[k] / s[11].max(1.0);
            let near = spread(
                sums.iter()
                    .map(|s| [mean(s, 0), mean(s, 1), mean(s, 2), mean(s, 3)])
                    .collect(),
                (e / 40.0).max(3.0),
            );
            let backdrop = spread(
                sums.iter()
                    .map(|s| [mean(s, 4), mean(s, 5), mean(s, 6), mean(s, 7)])
                    .collect(),
                (e / 40.0).max(3.0),
            );
            // The hair's own colour from well inside the model's hair only:
            // its outline can run past the real hair onto the backdrop.
            let strands = spread(
                sums.iter()
                    .zip(&near)
                    .map(|(s, n)| {
                        let inside = smoothstep(0.8, 0.97, n[0]);
                        [
                            mean(s, 0) * inside,
                            mean(s, 8) * inside,
                            mean(s, 9) * inside,
                            mean(s, 10) * inside,
                        ]
                    })
                    .collect(),
                (e / 20.0).max(3.0),
            );
            drop(sums);
            // Brows and eyes stay out under a fringe, unless the model is
            // certain of hair there.
            let mut features = vec![0.0f32; hr.len()];
            for brow in [&RIGHT_BROW[..], &LEFT_BROW[..]] {
                stamp_polygon(
                    &mut features,
                    hr,
                    &loop_points(points, brow),
                    0.005 * e,
                    0.015 * e,
                );
            }
            for eye in [&RIGHT_EYE[..], &LEFT_EYE[..]] {
                stamp_polygon(
                    &mut features,
                    hr,
                    &loop_points(points, eye),
                    0.01 * e,
                    0.015 * e,
                );
            }
            let open = [
                hr.x > 0,
                hr.y > 0,
                hr.x + hr.w < width,
                hr.y + hr.h < height,
            ];
            let fade = (0.05 * e).max(4.0);
            let hair: Vec<u8> = (0..hr.len())
                .into_par_iter()
                .map(|i| {
                    let [person, strand, colour_skin, certain] =
                        weights[i].map(|v| v as f32 / 255.0);
                    if person == 0.0 && strand == 0.0 {
                        return 0;
                    }
                    let (x, y) = (i % hw, i / hw);
                    let c = colours[i];
                    let [b, skin_luma, skin, back_sq] = grid_at(&near, gw, gh, cell, x, y);
                    // Against a plain backdrop whose colour stands well apart
                    // from the hair's (not a busy one such as a bookshelf,
                    // which would pass for strands), the pixel's share of hair
                    // by where its colour sits between the two.
                    let [back_w, br, bg, bb] = grid_at(&backdrop, gw, gh, cell, x, y);
                    let [fore_w, fr, fg, fb] = grid_at(&strands, gw, gh, cell, x, y);
                    let (share, clear) = if back_w > 0.02 && fore_w > 0.02 {
                        let back = [br / back_w, bg / back_w, bb / back_w];
                        let fore = [fr / fore_w, fg / fore_w, fb / fore_w];
                        let gap: [f32; 3] = std::array::from_fn(|k| back[k] - fore[k]);
                        let gap2 = gap.iter().map(|v| v * v).sum::<f32>();
                        let busy = (back_sq / back_w - back.iter().map(|v| v * v).sum::<f32>())
                            .max(0.0)
                            .sqrt();
                        let clear = smoothstep(0.12, 0.25, gap2.sqrt())
                            * smoothstep(2.0, 4.0, gap2.sqrt() / busy.max(1e-3))
                            * smoothstep(0.02, 0.1, back_w);
                        if clear > 0.0 {
                            let along = (0..3).map(|k| (back[k] - c[k]) * gap[k]).sum::<f32>();
                            (smoothstep(0.1, 0.9, along / gap2), clear)
                        } else {
                            (0.0, 0.0)
                        }
                    } else {
                        (0.0, 0.0)
                    };
                    let zone = smoothstep(0.0, 0.5, b) * person;
                    let core = smoothstep(0.75, 0.97, b);
                    let reference = if skin > 0.02 {
                        skin_luma / skin
                    } else {
                        cheek_luma
                    };
                    let dark = 1.0 - smoothstep(0.55 * reference, 0.85 * reference, luma(c));
                    let tone = dark + (share - dark) * clear;
                    let body = zone * (core + (1.0 - core) * tone);
                    let edge = share * clear * smoothstep(0.02, 0.12, b) * strand;
                    let keep =
                        (1.0 - features[i] * (1.0 - certain)) * side_fade(i, hw, hh, open, fade);
                    to_u8(body.max(edge) * (1.0 - colour_skin) * keep)
                })
                .collect();
            drop(weights);
            // Develop's regional luminance: Shadows/Blacks move each strand
            // by its neighbourhood's tone, so hair texture survives a lift.
            let tone: Vec<f32> = colours
                .par_iter()
                .map(|c| luminance_f32(c[0], c[1], c[2]).clamp(0.0, 1.0))
                .collect();
            drop(colours);
            let base = edge_aware_base(&tone, hw, hh, (e / 100.0).max(2.0), 0.01)
                .into_par_iter()
                .map(|v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16)
                .collect();
            (hr, hair, base)
        }
        _ => (
            Region {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
            Vec::new(),
            Vec::new(),
        ),
    };

    FaceModel {
        mesh,
        agreement,
        region,
        extent,
        skin: skin_layers,
        under_band,
        sclera,
        eye_white,
        iris,
        teeth,
        spot_score,
        spot_cover,
        donor,
        heal_base,
        lips,
        brows,
        nose,
        hair_region,
        hair,
        hair_base,
        detail,
        soft,
        ai_detail: std::sync::OnceLock::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brow_search_reaches_up_and_past_the_tail_but_hardly_down() {
        // A level brow: lower edge outer to inner, upper edge inner to outer.
        let outline: Vec<[f32; 2]> = [0.0f32, 25.0, 50.0, 75.0, 100.0]
            .iter()
            .map(|&x| [x, 110.0])
            .chain(
                [100.0f32, 75.0, 50.0, 25.0, 0.0]
                    .iter()
                    .map(|&x| [x, 100.0]),
            )
            .collect();
        let e = 1000.0;
        let search = brow_search(&outline, [0.0, 1.0], e);
        let mid_lower = search[2][1] - outline[2][1];
        let mid_upper = outline[7][1] - search[7][1];
        assert!(
            mid_lower > 0.0 && mid_upper > 2.0 * mid_lower,
            "{mid_lower} {mid_upper}"
        );
        // The outer end (x = 0) moves out, the inner end (x = 100) stays.
        assert!(search[0][0] < -15.0, "tail {:?}", search[0]);
        assert_eq!(search[4][0], 100.0, "head {:?}", search[4]);
    }

    /// A fringe edge the part model stopped short of (hair-coloured x 20..28,
    /// a soft ramp to x 31), with a brow starting at x 32 just past it.
    fn fringe_scene(brow: [f32; 4]) -> Vec<f32> {
        let (w, h, e) = (80usize, 40usize, 300.0);
        let (skin, hair) = ([0.8f32, 0.6, 0.5, 0.0], [0.15f32, 0.1, 0.08, 0.0]);
        let mix = |t: f32| std::array::from_fn(|k| skin[k] + (hair[k] - skin[k]) * t);
        let colours: Vec<[f32; 4]> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                match x {
                    _ if x < 28 => hair,
                    28..=31 => mix(0.8 - 0.2 * (x - 28) as f32),
                    _ if (32..70).contains(&x) && (18..22).contains(&y) => brow,
                    _ => skin,
                }
            })
            .collect();
        let beneath = vec![[skin[0], skin[1], skin[2]]; w * h];
        let odds: Vec<f32> = (0..w * h)
            .map(|i| if i % w < 20 { 0.9 } else { 0.0 })
            .collect();
        let middle: Vec<bool> = (0..w * h)
            .map(|i| (45..60).contains(&(i % w)) && (18..22).contains(&(i / w)))
            .collect();
        fringe_edge(&colours, &beneath, &odds, &vec![0; w * h], &middle, w, h, e)
    }

    #[test]
    fn fringe_edge_takes_the_hair_past_the_model_but_not_a_light_brow() {
        let fringe = fringe_scene([0.55, 0.4, 0.33, 0.0]);
        let at = |x: usize, y: usize| fringe[y * 80 + x];
        assert!(at(24, 10) > 0.95, "hair past the model {}", at(24, 10));
        assert!(at(29, 20) > 0.95, "soft hair edge {}", at(29, 20));
        assert_eq!(at(10, 10), 0.0, "the model's own hair stays its own");
        for x in 32..70 {
            assert_eq!(at(x, 20), 0.0, "brow at x {x}");
        }
        assert_eq!(at(50, 5), 0.0, "skin");
    }

    #[test]
    fn fringe_edge_leaves_a_brow_as_dark_as_the_hair_alone() {
        let fringe = fringe_scene([0.15, 0.1, 0.08, 0.0]);
        assert!(fringe.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn brow_takes_skin_in_proportion_and_gives_back_the_uncut_skin() {
        let near = |a: f32, b: f32| (a - b).abs() < 1e-6;
        assert!(near(skin_beside_brow(0.6, 0.25, 0.625, 0.8), 0.3));
        assert!(near(skin_beside_brow(0.6, 0.25, 1.0, 0.8), 0.0));
        assert!(near(skin_beside_brow(0.1, 0.8, 0.2, 0.9), 0.72));
        // Skin painted in past the brow stays.
        assert!(near(skin_beside_brow(0.9, 0.8, 0.2, 0.5), 0.9));
        assert!(near(skin_beside_brow(0.4, 0.5, 0.5, 1.0), 0.4));
        // As analysed (skin = uncut cut by the brow), there and back.
        let (uncut, a0, a1) = (0.9f32, 0.3, 0.7);
        let more = skin_beside_brow(uncut * (1.0 - a0), a0, a1, uncut);
        assert!(near(more, uncut * (1.0 - a1)));
    }

    #[test]
    fn quantile_picks_the_ranked_value() {
        let values: Vec<f32> = (0..101).map(|v| v as f32).collect();
        assert_eq!(quantile(values.clone(), 0.75), Some(75.0));
        assert_eq!(quantile(values, 0.0), Some(0.0));
        assert_eq!(quantile(Vec::new(), 0.5), None);
    }
}

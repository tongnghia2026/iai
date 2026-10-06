//! Dress an ID photo in a garment cut out on a transparent ground (a shop's
//! "phôi áo"): find the collar's two points on the garment and the two sides
//! of the neck on the person, lay the one on the other, take the old clothes
//! away and keep the hair that falls over them.
//!
//! Nothing here is generated: the garment is the shop's own picture, moved,
//! scaled and turned. The person is read from what the retouch already finds
//! (the matte, the face's landmarks, the hair and skin labels).

use rayon::prelude::*;

use super::ai::body_parts::{PartLabels, GROUP_BODY_SKIN, GROUP_FACE_SKIN, GROUP_HAIR};
use super::id_photo::FaceMarks;

const SOLID: u8 = 128;
/// The collar may lean this far with the shoulders (radians).
const MAX_LEAN: f32 = 0.07;
/// A garment too short for the photo is drawn this much taller under its
/// collar at most; past that its last row runs on.
const MAX_STRETCH: f32 = 1.25;
/// Half the neck as a share of the face's width: the least and most believed
/// of a measurement, and what stands in for one.
const NECK_HALF: (f32, f32, f32) = (0.27, 0.50, 0.36);
/// How much wider than the neck the outline must grow to count as shoulder
/// (or as the collar worn in the photo): the new collar sits where that starts.
const SHOULDER_GROWTH: f32 = 1.06;
/// A garment still short at `MAX_STRETCH` is laid this much larger than the
/// neck asks at most; its opening is then wider than the neck, and skin is
/// laid in the gap.
const MAX_OVERSIZE: f32 = 1.35;
/// The skin laid in the collar's opening darkens this much beside the
/// garment's edge, where the collar shades it.
const RIM_SHADE: f32 = 0.25;
/// Garment pixels the opening's skin runs on under the garment's edge.
const OPENING_LAP: usize = 3;
/// How far around the opening, in eye-to-chin lengths, the person has skin
/// under the garment.
const UNDERLAY: f32 = 0.25;
/// Photo pixels over the collar's line from which hair counts as falling
/// over the garment.
const HAIR_RISE: f32 = 8.0;
/// Pixels of hair on the garment below which no hair layer is made.
const HAIR_MEETS: usize = 64;
/// How sure the labels must be of clothes for them to count as clothes.
const CLOTHES: u8 = 77;
/// The neck widens to a collar's point no farther from its edge than this,
/// in eye-to-chin lengths, and by this many pixels for each pixel of height.
const FLARE_REACH: f32 = 0.15;
const FLARE_RUN: f32 = 0.7;

/// A garment as the shop keeps it: straight-alpha RGBA, the neck cut away.
#[derive(Clone)]
pub struct Garment {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Where a garment takes the neck, in its own pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Collar {
    /// The collar's top beside the neck, on the picture's left and right.
    pub left: [f32; 2],
    pub right: [f32; 2],
    /// The bottom of the opening between them.
    pub notch: [f32; 2],
}

impl Garment {
    /// The first solid row of each column; the height where a column is empty.
    fn top_edge(&self) -> Vec<usize> {
        let (w, h) = (self.width as usize, self.height as usize);
        (0..w)
            .map(|x| {
                (0..h)
                    .find(|y| self.rgba[(y * w + x) * 4 + 3] >= SOLID)
                    .unwrap_or(h)
            })
            .collect()
    }

    /// The collar, read from the garment's outline: its two highest points
    /// with the deepest dip of the middle between them. `None` for a picture
    /// with no such opening.
    pub fn collar(&self) -> Option<Collar> {
        let (w, h) = (self.width as usize, self.height as usize);
        if self.rgba.len() != w * h * 4 {
            return None;
        }
        let top = self.top_edge();
        let first = top.iter().position(|&t| t < h)?;
        let last = top.iter().rposition(|&t| t < h)?;
        let span = last - first + 1;
        if span < 16 {
            return None;
        }
        // The highest point at or left of each column, and at or right of it.
        let mut from_left = top.clone();
        for x in first + 1..=last {
            from_left[x] = from_left[x].min(from_left[x - 1]);
        }
        let mut from_right = top.clone();
        for x in (first..last).rev() {
            from_right[x] = from_right[x].min(from_right[x + 1]);
        }
        let depth = |x: usize| top[x] as i64 - from_left[x].max(from_right[x]) as i64;
        let middle = (first + last) / 2;
        let dip = (first + span / 4..=last - span / 4)
            .max_by_key(|&x| (depth(x), -(x as i64 - middle as i64).abs()))?;
        let deep = depth(dip);
        if deep < (span as i64 / 40).max(3) {
            return None;
        }
        // A flat-topped collar touches the neck at its inner end.
        let slack = (deep / 25).max(1) as usize;
        let left = (first..=dip).rfind(|&x| top[x] <= from_left[dip] + slack)?;
        let right = (dip..=last).find(|&x| top[x] <= from_right[dip] + slack)?;
        let between = right - left;
        if between * 100 < span * 6 || between * 100 > span * 80 {
            return None;
        }
        // The opening's bottom: the middle of its deepest run.
        let floor = (left..=right).map(|x| top[x]).max()?;
        let run: Vec<usize> = (left..=right)
            .filter(|&x| top[x] + slack >= floor)
            .collect();
        let notch = run[run.len() / 2];
        let at = |x: usize| [x as f32 + 0.5, top[x] as f32];
        Some(Collar {
            left: at(left),
            right: at(right),
            notch: at(notch),
        })
    }

    /// The collar's opening as a plane of the garment: what lies under the
    /// line between the collar's points and over the garment itself, and
    /// `OPENING_LAP` pixels on under the garment's edge and over the line,
    /// so no seam shows where the two meet.
    fn opening(&self, collar: &Collar) -> Vec<u8> {
        let (w, h) = (self.width as usize, self.height as usize);
        let top = self.top_edge();
        let mut plane = vec![0u8; w * h];
        let (x0, x1) = (
            collar.left[0] as usize,
            (collar.right[0] as usize).min(w - 1),
        );
        let line = collar.top().round() as usize;
        for x in x0.saturating_sub(OPENING_LAP)..=(x1 + OPENING_LAP).min(w - 1) {
            // Beside the collar's points only the lap under the garment's
            // edge counts: a collar that falls away from its point leaves the
            // photo bare there.
            let from = if (x0..=x1).contains(&x) {
                line.saturating_sub(OPENING_LAP)
            } else {
                line.max(top[x])
            };
            for y in from..(top[x] + OPENING_LAP).min(h) {
                plane[y * w + x] = 255;
            }
        }
        plane
    }
}

impl Collar {
    /// The collar's line: the row of its higher point. A collar drawn with
    /// one point lower is not a garment drawn askew, so the line stays level.
    fn top(&self) -> f32 {
        self.left[1].min(self.right[1])
    }
}

/// What the fit reads of the person, each a plane of the photo, 0..255.
pub struct Figure {
    pub width: u32,
    pub height: u32,
    /// The person against the background.
    pub matte: Vec<u8>,
    pub hair: Vec<u8>,
    /// Bare skin: face, neck and body.
    pub skin: Vec<u8>,
    /// What is none of the groups the labels tell apart: clothes and what
    /// is worn.
    pub other: Vec<u8>,
}

impl Figure {
    pub fn read(labels: &PartLabels, matte: &[u8], width: u32, height: u32) -> Self {
        let w = width as usize;
        let mut hair = vec![0u8; matte.len()];
        let mut skin = vec![0u8; matte.len()];
        let mut other = vec![0u8; matte.len()];
        hair.par_chunks_mut(w)
            .zip(skin.par_chunks_mut(w))
            .zip(other.par_chunks_mut(w))
            .enumerate()
            .for_each(|(y, ((hair_row, skin_row), other_row))| {
                for x in 0..w {
                    let odds = labels.groups_at(x as f32 + 0.5, y as f32 + 0.5);
                    let level = |odds: f32| (odds.clamp(0.0, 1.0) * 255.0).round() as u8;
                    hair_row[x] = level(odds[GROUP_HAIR]);
                    skin_row[x] = level(odds[GROUP_FACE_SKIN] + odds[GROUP_BODY_SKIN]);
                    other_row[x] = level(1.0 - odds.iter().sum::<f32>());
                }
            });
        Self {
            width,
            height,
            matte: matte.to_vec(),
            hair,
            skin,
            other,
        }
    }

    fn body(&self, x: i32, y: i32) -> bool {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return false;
        }
        let i = y as usize * self.width as usize + x as usize;
        self.matte[i] >= SOLID && self.hair[i] < SOLID
    }

    fn hair_at(&self, x: i32, y: i32) -> bool {
        x >= 0
            && y >= 0
            && x < self.width as i32
            && y < self.height as i32
            && self.hair[y as usize * self.width as usize + x as usize] >= SOLID
    }

    /// How far the body runs from `axis` along row `y` towards `step` (-1 or
    /// 1), and whether hair is what ends it. Gaps up to `gap` are stepped over.
    fn reach(&self, axis: i32, y: i32, step: i32, gap: i32) -> Option<(f32, bool)> {
        if !self.body(axis, y) {
            return None;
        }
        let (mut x, mut end, mut missed) = (axis, axis, 0);
        while missed <= gap {
            x += step;
            if x < 0 || x >= self.width as i32 {
                break;
            }
            if self.body(x, y) {
                (end, missed) = (x, 0);
            } else {
                missed += 1;
            }
        }
        let hair = (1..=gap + 2).any(|d| self.hair_at(end + step * d, y));
        Some(((end - axis).abs() as f32 + 0.5, hair))
    }
}

/// The two sides of the neck at its base: where a collar's points sit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Neck {
    pub left: [f32; 2],
    pub right: [f32; 2],
}

/// One side of the neck as the outline shows it.
#[derive(Clone, Copy, Debug)]
struct Side {
    /// Half the neck's width where it is narrowest.
    half: f32,
    /// The row where the outline turns out into the shoulder (or the old
    /// collar).
    base: f32,
}

/// Follow the body's outline down one side from the jaw: the narrowest row is
/// the neck, and where it then grows is the neck's base. `None` when hair
/// hides that side.
fn neck_side(figure: &Figure, face: &FaceMarks, unit: f32, step: i32) -> Option<Side> {
    let axis = face.chin[0].round() as i32;
    let gap = (unit * 0.02).max(2.0) as i32;
    let from = (face.chin[1] - unit * 0.45).round() as i32;
    let to = ((face.chin[1] + unit).round() as i32).min(figure.height as i32 - 1);
    let rows: Vec<Option<(f32, bool)>> = (from..=to)
        .map(|y| figure.reach(axis, y, step, gap))
        .collect();
    // A median of five rows: one stray row is no neck.
    let smooth = |i: usize| -> Option<f32> {
        let mut near: Vec<f32> = (i.saturating_sub(2)..(i + 3).min(rows.len()))
            .filter_map(|j| rows[j].map(|r| r.0))
            .collect();
        if near.is_empty() || rows[i].is_none() {
            return None;
        }
        near.sort_by(f32::total_cmp);
        Some(near[near.len() / 2])
    };
    let search = ((unit * 1.15) as usize).min(rows.len());
    let (narrowest, half) = (0..search)
        .filter_map(|i| smooth(i).map(|h| (i, h)))
        .min_by(|a, b| a.1.total_cmp(&b.1))?;
    if rows[narrowest].is_some_and(|r| r.1) {
        return None;
    }
    let grown = half * SHOULDER_GROWTH + 2.0;
    let base = (narrowest..rows.len())
        .find(|&i| smooth(i).is_some_and(|h| h >= grown))
        .unwrap_or(narrowest + (unit * 0.2) as usize);
    Some(Side {
        half,
        base: (from + base as i32) as f32,
    })
}

/// Where the collar's points go on the person. A side hidden by hair takes
/// the other side's measure; with both hidden the face's own proportions
/// stand in (the collar's points are then under the hair anyway).
pub fn neck_of(figure: &Figure, face: &FaceMarks) -> Neck {
    let unit = (face.chin[0] - face.eyes[0])
        .hypot(face.chin[1] - face.eyes[1])
        .max(1.0);
    let left = neck_side(figure, face, unit, -1);
    let right = neck_side(figure, face, unit, 1);
    let stand_in = Side {
        half: face.width * NECK_HALF.2,
        base: face.chin[1] + unit * 0.12,
    };
    let (left, right, centre) = match (left, right) {
        (Some(l), Some(r)) => (l, r, face.chin[0] + (r.half - l.half) * 0.5),
        (Some(s), None) | (None, Some(s)) => (s, s, face.chin[0]),
        (None, None) => (stand_in, stand_in, face.chin[0]),
    };
    let half =
        ((left.half + right.half) * 0.5).clamp(face.width * NECK_HALF.0, face.width * NECK_HALF.1);
    let row = |side: Side| {
        side.base
            .clamp(face.chin[1] - unit * 0.35, face.chin[1] + unit * 0.55)
    };
    Neck {
        left: [centre - half, row(left)],
        right: [centre + half, row(right)],
    }
}

/// How a garment lies on the photo: the garment's point `g`, its rows under
/// `stretch_from` drawn `stretch` times taller, shows at
/// `origin + scale * turn(angle) * (g - pivot)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub scale: f32,
    /// Clockwise, radians.
    pub angle: f32,
    pub pivot: [f32; 2],
    pub origin: [f32; 2],
    pub stretch_from: f32,
    pub stretch: f32,
}

impl Placement {
    /// Lay the collar between the neck's sides, leaning with the shoulders
    /// no more than `MAX_LEAN`, and reach the bottom of the photo (`photo`
    /// is its width and height).
    pub fn fit(collar: &Collar, garment_height: u32, neck: &Neck, photo: (u32, u32)) -> Self {
        let on_person = [neck.right[0] - neck.left[0], neck.right[1] - neck.left[1]];
        let scale =
            on_person[0].hypot(on_person[1]).max(1.0) / (collar.right[0] - collar.left[0]).max(1.0);
        let angle = on_person[1].atan2(on_person[0]).clamp(-MAX_LEAN, MAX_LEAN);
        let middle = |a: [f32; 2], b: [f32; 2]| [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
        let pivot = middle(collar.left, collar.right);
        let origin = middle(neck.left, neck.right);
        // Rows of the photo under the collar (a leaning garment's low corner
        // rises), and rows of the garment there when those under its notch
        // are drawn `stretch` times taller.
        let stretch_from = collar.notch[1];
        let need =
            (photo.1 as f32 - origin[1]).max(0.0) + angle.sin().abs() * photo.0 as f32 * 0.5 + 1.0;
        let above = stretch_from - pivot[1];
        let under = (garment_height as f32 - stretch_from).max(1.0);
        let stretch = ((need / scale - above) / under).clamp(1.0, MAX_STRETCH);
        let reach = (above + under * stretch) * scale;
        let scale = if reach < need {
            (scale * need / reach).min(scale * MAX_OVERSIZE)
        } else {
            scale
        };
        Self {
            scale,
            angle,
            pivot,
            origin,
            stretch_from,
            stretch,
        }
    }

    pub fn to_photo(&self, g: [f32; 2]) -> [f32; 2] {
        let y = if g[1] > self.stretch_from {
            self.stretch_from + (g[1] - self.stretch_from) * self.stretch
        } else {
            g[1]
        };
        let (dx, dy) = (g[0] - self.pivot[0], y - self.pivot[1]);
        let (sin, cos) = self.angle.sin_cos();
        [
            self.origin[0] + self.scale * (cos * dx - sin * dy),
            self.origin[1] + self.scale * (sin * dx + cos * dy),
        ]
    }

    pub fn to_garment(&self, p: [f32; 2]) -> [f32; 2] {
        let (dx, dy) = (
            (p[0] - self.origin[0]) / self.scale,
            (p[1] - self.origin[1]) / self.scale,
        );
        let (sin, cos) = self.angle.sin_cos();
        let (x, y) = (
            self.pivot[0] + cos * dx + sin * dy,
            self.pivot[1] - sin * dx + cos * dy,
        );
        let y = if y > self.stretch_from {
            self.stretch_from + (y - self.stretch_from) / self.stretch
        } else {
            y
        };
        [x, y]
    }
}

/// Catmull-Rom weights for the four samples around a point `t` past the second.
fn cubic(t: f32) -> [f32; 4] {
    let (t2, t3) = (t * t, t * t * t);
    [
        -0.5 * t3 + t2 - 0.5 * t,
        1.5 * t3 - 2.5 * t2 + 1.0,
        -1.5 * t3 + 2.0 * t2 + 0.5 * t,
        0.5 * t3 - 0.5 * t2,
    ]
}

/// A garment made ready to be drawn: premultiplied colour and its opening.
struct Cloth {
    width: usize,
    height: usize,
    colour: Vec<[f32; 4]>,
    opening: Vec<u8>,
}

impl Cloth {
    fn of(garment: &Garment, collar: &Collar) -> Self {
        let (w, h) = (garment.width as usize, garment.height as usize);
        let mut colour: Vec<[f32; 4]> = garment
            .rgba
            .chunks_exact(4)
            .map(|px| {
                let a = px[3] as f32 / 255.0;
                [px[0] as f32 * a, px[1] as f32 * a, px[2] as f32 * a, a]
            })
            .collect();
        // A garment cut a little askew ends some columns short of its last
        // row: those run on to it, as the last row runs on under it.
        for x in 0..w {
            let solid = |y: &usize| garment.rgba[(y * w + x) * 4 + 3] >= SOLID;
            if let Some(last) = (h - h / 8..h).rfind(solid) {
                for y in last + 1..h {
                    colour[y * w + x] = colour[last * w + x];
                }
            }
        }
        Self {
            width: garment.width as usize,
            height: garment.height as usize,
            colour,
            opening: garment.opening(collar),
        }
    }

    /// Premultiplied colour at a point of the garment, bicubic; clear outside
    /// it, but under its last row that row runs on.
    fn colour_at(&self, x: f32, y: f32) -> [f32; 4] {
        let (w, h) = (self.width as i32, self.height as i32);
        if x < -1.0 || x > w as f32 + 1.0 || y < -1.0 {
            return [0.0; 4];
        }
        let (fx, fy) = (x - 0.5, (y - 0.5).min(h as f32 - 1.0));
        let (ix, iy) = (fx.floor() as i32, fy.floor() as i32);
        let (wx, wy) = (cubic(fx - ix as f32), cubic(fy - iy as f32));
        let mut sum = [0.0f32; 4];
        for (j, wy) in wy.iter().enumerate() {
            let sy = (iy + j as i32 - 1).min(h - 1);
            if sy < 0 {
                continue;
            }
            for (i, wx) in wx.iter().enumerate() {
                let sx = ix + i as i32 - 1;
                if sx < 0 || sx >= w {
                    continue;
                }
                let px = self.colour[(sy * w + sx) as usize];
                let k = wx * wy;
                for c in 0..4 {
                    sum[c] += px[c] * k;
                }
            }
        }
        let a = sum[3].clamp(0.0, 1.0);
        [
            sum[0].clamp(0.0, 255.0 * a),
            sum[1].clamp(0.0, 255.0 * a),
            sum[2].clamp(0.0, 255.0 * a),
            a,
        ]
    }

    /// The whole garment as it lies on a photo of `photo` (width, height):
    /// a piece as large as the garment laid, reaching the photo's bottom
    /// where the garment is short of it.
    fn laid(&self, placement: &Placement, photo: (usize, usize)) -> Piece {
        let (gw, gh) = (self.width as f32, self.height as f32);
        let corners = [[0.0, 0.0], [gw, 0.0], [0.0, gh], [gw, gh]].map(|g| placement.to_photo(g));
        let least = |axis: usize| corners.iter().map(|c| c[axis]).fold(f32::MAX, f32::min);
        let most = |axis: usize| corners.iter().map(|c| c[axis]).fold(f32::MIN, f32::max);
        // No wider than three photos: a garment laid absurdly large is cut.
        let (limit_w, limit_h) = (photo.0 as f32 * 3.0, photo.1 as f32 * 3.0);
        let x0 = least(0).floor().max(-limit_w) - 1.0;
        let y0 = least(1).floor().max(-limit_h) - 1.0;
        let x1 = most(0).ceil().min(limit_w * 2.0) + 1.0;
        let y1 = most(1).ceil().max(photo.1 as f32).min(limit_h * 2.0) + 1.0;
        let (width, height) = ((x1 - x0).max(1.0) as usize, (y1 - y0).max(1.0) as usize);
        let mut rgba = vec![0u8; width * height * 4];
        rgba.par_chunks_mut(width * 4)
            .enumerate()
            .for_each(|(y, row)| {
                for x in 0..width {
                    let at = [x0 + x as f32 + 0.5, y0 + y as f32 + 0.5];
                    let [gx, gy] = placement.to_garment(at);
                    // Under the photo nothing runs on.
                    if at[1] > photo.1 as f32 && gy > self.height as f32 {
                        continue;
                    }
                    let [r, g, b, a] = self.colour_at(gx, gy);
                    if a > 0.0 {
                        row[x * 4] = (r / a).round() as u8;
                        row[x * 4 + 1] = (g / a).round() as u8;
                        row[x * 4 + 2] = (b / a).round() as u8;
                        row[x * 4 + 3] = (a * 255.0).round() as u8;
                    }
                }
            });
        Piece {
            rgba,
            width: width as u32,
            height: height as u32,
            offset: (x0 as i32, y0 as i32),
        }
    }

    /// How far inside the opening a point of the garment is, 0..1.
    fn opening_at(&self, x: f32, y: f32) -> f32 {
        let (w, h) = (self.width as i32, self.height as i32);
        let (fx, fy) = (x - 0.5, y - 0.5);
        let (ix, iy) = (fx.floor() as i32, fy.floor() as i32);
        let (tx, ty) = (fx - ix as f32, fy - iy as f32);
        let at = |sx: i32, sy: i32| {
            if sx < 0 || sy < 0 || sx >= w || sy >= h {
                0.0
            } else {
                self.opening[(sy * w + sx) as usize] as f32 / 255.0
            }
        };
        let upper = at(ix, iy) * (1.0 - tx) + at(ix + 1, iy) * tx;
        let lower = at(ix, iy + 1) * (1.0 - tx) + at(ix + 1, iy + 1) * tx;
        upper * (1.0 - ty) + lower * ty
    }
}

/// Spread the colours of weighted pixels over the weightless ones, smoothly:
/// each level of a pyramid fills what the finer one lacks.
fn spread(colour: &mut [[f32; 3]], weight: &[f32], width: usize, height: usize) {
    if width * height <= 1 {
        return;
    }
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let mut coarse = vec![[0.0f32; 3]; cw * ch];
    let mut coarse_weight = vec![0.0f32; cw * ch];
    for y in 0..height {
        for x in 0..width {
            let (i, o) = (y * width + x, (y / 2) * cw + x / 2);
            for c in 0..3 {
                coarse[o][c] += colour[i][c] * weight[i];
            }
            coarse_weight[o] += weight[i];
        }
    }
    for (px, w) in coarse.iter_mut().zip(coarse_weight.iter_mut()) {
        if *w > 0.0 {
            for c in px.iter_mut() {
                *c /= *w;
            }
        }
        *w = w.min(1.0);
    }
    spread(&mut coarse, &coarse_weight, cw, ch);
    for y in 0..height {
        for x in 0..width {
            let i = y * width + x;
            if weight[i] >= 1.0 {
                continue;
            }
            // The coarse level read between its cells.
            let (fx, fy) = ((x as f32 - 0.5) * 0.5, (y as f32 - 0.5) * 0.5);
            let (x0, y0) = (fx.floor().max(0.0) as usize, fy.floor().max(0.0) as usize);
            let (x1, y1) = ((x0 + 1).min(cw - 1), (y0 + 1).min(ch - 1));
            let (tx, ty) = (
                (fx - x0 as f32).clamp(0.0, 1.0),
                (fy - y0 as f32).clamp(0.0, 1.0),
            );
            for c in 0..3 {
                let upper = coarse[y0 * cw + x0][c] * (1.0 - tx) + coarse[y0 * cw + x1][c] * tx;
                let lower = coarse[y1 * cw + x0][c] * (1.0 - tx) + coarse[y1 * cw + x1][c] * tx;
                let filled = upper * (1.0 - ty) + lower * ty;
                colour[i][c] = filled * (1.0 - weight[i]) + colour[i][c] * weight[i];
            }
        }
    }
}

fn smoothstep(from: f32, to: f32, x: f32) -> f32 {
    let t = ((x - from) / (to - from)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A layer that need not be the photo's size: straight-alpha RGBA with its
/// corner at `offset` on the photo.
pub struct Piece {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub offset: (i32, i32),
}

impl Piece {
    /// The piece as it lies on a photo `width` by `height`: the photo's
    /// straight-alpha RGBA.
    pub fn on_photo(&self, width: u32, height: u32) -> Vec<u8> {
        let (w, h) = (width as i64, height as i64);
        let (pw, ph) = (self.width as i64, self.height as i64);
        let (ox, oy) = (self.offset.0 as i64, self.offset.1 as i64);
        let mut out = vec![0u8; (w * h * 4) as usize];
        let (x0, x1) = (ox.max(0), (ox + pw).min(w));
        if x0 >= x1 {
            return out;
        }
        for y in oy.max(0)..(oy + ph).min(h) {
            let from = (((y - oy) * pw + x0 - ox) * 4) as usize;
            let to = ((y * w + x0) * 4) as usize;
            let run = ((x1 - x0) * 4) as usize;
            out[to..to + run].copy_from_slice(&self.rgba[from..from + run]);
        }
        out
    }
}

/// The dressed photo's layers, bottom to top. The garment is whole and on
/// its own so it can be moved, scaled and turned by hand afterwards: the
/// person has skin a little way on under it, and the hair that falls over it
/// lies over it as a layer of its own.
pub struct Dressed {
    /// The person, the photo's size: the old clothes gone, skin laid in the
    /// collar's opening.
    pub person: Vec<u8>,
    pub garment: Piece,
    /// The shade the garment and the person cast on each other, the photo's
    /// size; `None` when there is none.
    pub shade: Option<Vec<u8>>,
    /// The hair from the collar's line down, the photo's size; `None` when
    /// none of it falls over the garment.
    pub hair: Option<Vec<u8>>,
}

/// Dress `person` (the cut-out of the photo `figure` was read from) in
/// `garment`, lying as `placement` says.
pub fn dress(
    person: &[u8],
    figure: &Figure,
    face: &FaceMarks,
    garment: &Garment,
    collar: &Collar,
    placement: &Placement,
) -> Dressed {
    let (w, h) = (figure.width as usize, figure.height as usize);
    let cloth = Cloth::of(garment, collar);
    let unit = (face.chin[0] - face.eyes[0])
        .hypot(face.chin[1] - face.eyes[1])
        .max(1.0);
    // How far under the line through the laid collar's points a pixel of the
    // photo is, in its own pixels: nothing over that line is touched.
    let under_line = |x: usize, y: usize| {
        let [_, gy] = placement.to_garment([x as f32 + 0.5, y as f32 + 0.5]);
        (gy - collar.top()) * placement.scale
    };
    // Whether a pixel of the photo lies in the columns between the collar's
    // points: the neck's own.
    let in_neck = |x: usize, y: usize| {
        let [gx, _] = placement.to_garment([x as f32 + 0.5, y as f32 + 0.5]);
        gx >= collar.left[0] && gx <= collar.right[0]
    };
    let hair = hair_matte(person, figure, unit, &under_line);
    // Skin is believed only well inside what the labels call skin: their
    // edge runs a few pixels over the clothes beside it.
    let labelled: Vec<f32> = (0..w * h)
        .map(|i| {
            figure.skin[i] as f32 / 255.0 * (person[i * 4 + 3] as f32 / 255.0) * (1.0 - hair[i])
        })
        .collect();
    let within = box_blur(&labelled, w, h, (unit * 0.04).max(6.0) as usize);
    let sure_skin = |i: usize| smoothstep(0.75, 0.99, within[i]);
    let risen = risen_clothes(figure, face.chin[1] - unit * 0.6, &under_line);
    // The neck's bare skin along each row of the photo: its first and last
    // column between the collar's points.
    let bare: Vec<Option<(usize, usize)>> = (0..h)
        .into_par_iter()
        .map(|y| {
            let skin = |x: &usize| {
                let i = y * w + x;
                figure.skin[i] >= 230
                    && person[i * 4 + 3] >= SOLID
                    && hair[i] < 0.5
                    && in_neck(*x, y)
            };
            Some(((0..w).find(skin)?, (0..w).rfind(skin)?))
        })
        .collect();
    // Where clothes hide the whole neck, it runs on as wide as it last showed.
    let mut last_shown = None;
    let covered: Vec<Option<(usize, usize)>> = bare
        .iter()
        .map(|row| {
            if row.is_some() {
                last_shown = *row;
            }
            last_shown
        })
        .collect();
    // Old clothes over the line give way to skin where they lie on the neck:
    // between its bare skin, or across it where none shows.
    let on_neck = |x: usize, y: usize| match (bare[y], covered[y]) {
        (Some((first, last)), _) => x > first && x < last,
        (None, Some((first, last))) => x >= first && x <= last,
        (None, None) => in_neck(x, y),
    };

    // Where the garment covers the photo, and its opening there.
    let mut cover = vec![0.0f32; w * h];
    let mut opening = vec![0.0f32; w * h];
    cover
        .par_chunks_mut(w)
        .zip(opening.par_chunks_mut(w))
        .enumerate()
        .for_each(|(y, (cover_row, open_row))| {
            for x in 0..w {
                let [gx, gy] = placement.to_garment([x as f32 + 0.5, y as f32 + 0.5]);
                cover_row[x] = cloth.colour_at(gx, gy)[3];
                open_row[x] = cloth.opening_at(gx, gy);
            }
        });
    // How deep inside the opening each pixel lies: 1 well inside, about
    // half at its rim.
    let deep = box_blur(&opening, w, h, ((unit * 0.05) as usize).max(2));
    // Skin runs on under the garment around the opening, so a garment moved
    // by hand uncovers skin, not a hole.
    // (Any of the opening inside the blur's square leaves a little in it.)
    let around = box_blur(&opening, w, h, ((unit * UNDERLAY) as usize).max(2));
    let laid_at = |i: usize| opening[i].max(smoothstep(0.0003, 0.004, around[i]) * cover[i]);

    // Skin for the opening, and for the neck where old clothes rose over
    // it: spread from the neck's own over the box that holds both.
    let mut bounds: Option<[usize; 4]> = None;
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if laid_at(i) > 0.0 || (risen[i] > 0.0 && on_neck(x, y)) {
                let b = bounds.get_or_insert([x, y, x, y]);
                *b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
            }
        }
    }
    let skin_box = bounds.map(|[x0, y0, x1, y1]| {
        let grow = (unit * 0.3) as usize;
        [
            x0.saturating_sub(grow),
            y0.saturating_sub(grow),
            (x1 + grow).min(w - 1),
            y1,
        ]
    });
    let filled = skin_box.map(|area| spread_in(person, w, area, &sure_skin));
    let skin_at = |x: usize, y: usize| -> Option<([f32; 3], f32)> {
        let ((fill, weight), [x0, y0, x1, y1]) = (filled.as_ref()?, skin_box?);
        (x >= x0 && x <= x1 && y >= y0 && y <= y1).then(|| {
            let o = (y - y0) * (x1 - x0 + 1) + (x - x0);
            (fill[o], weight[o])
        })
    };

    // Hair's own colour for its soft edge under the line: the old clothes
    // that showed through there must not show on the new.
    let sure_hair = |i: usize| smoothstep(0.9, 0.99, hair[i]) * (person[i * 4 + 3] as f32 / 255.0);
    let mut edge: Option<[usize; 4]> = None;
    for y in 0..h {
        for x in 0..w {
            let hair = hair[y * w + x];
            if hair > 0.0 && hair < 1.0 && under_line(x, y) > -HAIR_RISE {
                let b = edge.get_or_insert([x, y, x, y]);
                *b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
            }
        }
    }
    let hair_box = edge.map(|[x0, y0, x1, y1]| {
        let grow = (unit * 0.1) as usize + 4;
        [
            x0.saturating_sub(grow),
            y0.saturating_sub(grow),
            (x1 + grow).min(w - 1),
            (y1 + grow).min(h - 1),
        ]
    });
    let strands = hair_box.map(|area| spread_in(person, w, area, &sure_hair));
    let strand_at = |x: usize, y: usize| -> Option<[f32; 3]> {
        let ((colour, _), [x0, y0, x1, y1]) = (strands.as_ref()?, hair_box?);
        (x >= x0 && x <= x1 && y >= y0 && y <= y1)
            .then(|| colour[(y - y0) * (x1 - x0 + 1) + (x - x0)])
    };

    let mut out = person.to_vec();
    let mut over = vec![0u8; w * h * 4];
    out.par_chunks_mut(w * 4)
        .zip(over.par_chunks_mut(w * 4))
        .enumerate()
        .for_each(|(y, (row, over_row))| {
            for x in 0..w {
                let i = y * w + x;
                let under = under_line(x, y);
                let below = smoothstep(-1.0, 1.0, under);
                // Old clothes that rose are gone through the line's own rows
                // too: fading there against `below` would leave a line of them.
                let gone = risen[i] * (1.0 - smoothstep(1.0, 2.0, under));
                let strand = hair[i] * smoothstep(-HAIR_RISE, 0.0, under);
                if below <= 0.0 && gone <= 0.0 && strand <= 0.0 {
                    continue;
                }
                let hair = hair[i];
                let inside = laid_at(i);
                let px = &mut row[x * 4..x * 4 + 4];
                let alpha = px[3] as f32 / 255.0;
                let mut colour = [px[0] as f32, px[1] as f32, px[2] as f32];
                if let (true, Some(strand)) = (hair > 0.0 && hair < 1.0, strand_at(x, y)) {
                    let own = sure_hair(i);
                    for c in 0..3 {
                        colour[c] = strand[c] * (1.0 - own) + colour[c] * own;
                    }
                }
                // The hair as it lies over the garment.
                if strand > 0.0 {
                    for c in 0..3 {
                        over_row[x * 4 + c] = colour[c].round().clamp(0.0, 255.0) as u8;
                    }
                    over_row[x * 4 + 3] = (strand * alpha * 255.0).round() as u8;
                }
                if below <= 0.0 && gone <= 0.0 {
                    continue;
                }
                // Under the line, outside the opening only hair stays; inside
                // it skin is laid where the person shows none.
                let mut kept = alpha * hair;
                if let (true, Some((fill, weight))) = (inside > 0.0, skin_at(x, y)) {
                    let own = weight.max(hair * alpha);
                    let rim = ((1.0 - deep[i]) * 2.0).clamp(0.0, 1.0);
                    let shade = 1.0 - RIM_SHADE * rim;
                    for c in 0..3 {
                        let laid_in = fill[c] * shade * (1.0 - own) + colour[c] * own;
                        colour[c] += (laid_in - colour[c]) * inside;
                    }
                    kept += (1.0 - kept) * inside;
                }
                // What is left of the pixel as it was, and what is laid on
                // it. The old clothes that rose are gone: skin takes their
                // place on the neck, nothing beside it.
                let on_the_neck = if gone > 0.0 && on_neck(x, y) {
                    skin_at(x, y)
                } else {
                    None
                };
                let mut left = alpha * (1.0 - below);
                if on_the_neck.is_none() {
                    left *= 1.0 - gone;
                }
                let laid = kept * below;
                let mut after = left + laid;
                let mut shown = [px[0] as f32, px[1] as f32, px[2] as f32];
                if after > 0.0 {
                    for c in 0..3 {
                        shown[c] = (shown[c] * left + colour[c] * laid) / after;
                    }
                }
                if let Some((fill, _)) = on_the_neck {
                    for c in 0..3 {
                        shown[c] += (fill[c] - shown[c]) * gone;
                    }
                    after += (1.0 - after) * gone;
                }
                for c in 0..3 {
                    px[c] = shown[c].round().clamp(0.0, 255.0) as u8;
                }
                px[3] = (after * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        });
    // The neck widens to the collar's points: skin behind what is there.
    // What is there by now is the person without the old clothes.
    let stays = |i: usize| out[i * 4 + 3] >= SOLID;
    let line = collar.top();
    let points = [collar.left[0], collar.right[0]].map(|x| placement.to_photo([x, line]));
    let flare = neck_flare(&stays, (w, h), points, unit, &under_line);
    for (i, cover) in flare {
        let Some((fill, _)) = skin_at(i % w, i / w) else {
            continue;
        };
        let px = &mut out[i * 4..i * 4 + 4];
        let own = px[3] as f32 / 255.0;
        let laid = (1.0 - own) * cover;
        let after = own + laid;
        if laid <= 0.0 {
            continue;
        }
        for c in 0..3 {
            px[c] = ((px[c] as f32 * own + fill[c] * laid) / after)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
        px[3] = (after * 255.0).round() as u8;
    }
    // Hair that nowhere meets the garment needs no layer over it.
    let meets = over
        .chunks_exact(4)
        .zip(&cover)
        .filter(|(px, cover)| px[3] >= SOLID && **cover >= 0.5)
        .count();

    let garment = cloth.laid(placement, (w, h));
    let hair = (meets >= HAIR_MEETS).then_some(over);
    let shade = super::seam::shade(
        &out,
        &garment.on_photo(figure.width, figure.height),
        hair.as_deref(),
        figure.width,
        figure.height,
    );
    Dressed {
        person: out,
        garment,
        shade,
        hair,
    }
}

/// Where the neck widens to the collar's points over the collar's line: the
/// wedge from each point (`points`, on the photo: left, right) up to the edge
/// of what `stays` of the person just over the line. Each pixel of it with
/// how much of it the wedge covers. A side whose edge is at the point, or
/// too far from it to be the neck's, has none.
fn neck_flare(
    stays: &dyn Fn(usize) -> bool,
    (w, h): (usize, usize),
    points: [[f32; 2]; 2],
    unit: f32,
    under_line: &dyn Fn(usize, usize) -> f32,
) -> Vec<(usize, f32)> {
    let reach = (unit * FLARE_REACH) as i32;
    let mut wedge = Vec::new();
    for (point, inward) in points.into_iter().zip([1i32, -1]) {
        let (px, py) = (point[0].floor() as i32, point[1].round() as i32 - 2);
        if py < 0 || py >= h as i32 {
            continue;
        }
        let edge = (0..=reach).find(|d| {
            let x = px + d * inward;
            x >= 0 && x < w as i32 && stays(py as usize * w + x as usize)
        });
        let Some(span) = edge.filter(|span| *span >= 2) else {
            continue;
        };
        let rise = (span as f32 / FLARE_RUN).ceil() as i32 + 2;
        for y in (py + 2 - rise).max(0)..(py + 4).min(h as i32) {
            for d in 0..=span + 1 {
                let x = px + d * inward;
                if x < 0 || x >= w as i32 {
                    continue;
                }
                let (x, y) = (x as usize, y as usize);
                let under = under_line(x, y);
                if under > 1.0 {
                    continue;
                }
                // How far past the wedge's slanted side the pixel lies.
                let along = (x as f32 + 0.5 - point[0]) * inward as f32;
                let cover = (along + under.min(0.0) * FLARE_RUN + 0.5).clamp(0.0, 1.0);
                if cover > 0.0 {
                    wedge.push((y * w + x, cover));
                }
            }
        }
    }
    wedge
}

/// A plane blurred over squares `2 * radius + 1` across, as 0..1.
fn box_blur(plane: &[f32], width: usize, height: usize, radius: usize) -> Vec<f32> {
    let pass = |source: &[f32], length: usize, lines: usize, at: &dyn Fn(usize, usize) -> usize| {
        let mut out = vec![0.0f32; source.len()];
        for line in 0..lines {
            let mut sum = 0.0;
            let mut count = 0usize;
            for k in 0..(radius + 1).min(length) {
                sum += source[at(line, k)];
                count += 1;
            }
            for k in 0..length {
                out[at(line, k)] = sum / count as f32;
                if k + radius + 1 < length {
                    sum += source[at(line, k + radius + 1)];
                    count += 1;
                }
                if k >= radius {
                    sum -= source[at(line, k - radius)];
                    count -= 1;
                }
            }
        }
        out
    };
    let rows = pass(plane, width, height, &|y, x| y * width + x);
    pass(&rows, height, width, &|x, y| y * width + x)
}

/// How much of each pixel is hair, 0..1. The labels are coarse: near their
/// edge, from the collar's line down, the pixel's own colour decides, between
/// the hair's colour and that of what lies beside the hair.
fn hair_matte(
    person: &[u8],
    figure: &Figure,
    unit: f32,
    under_line: &dyn Fn(usize, usize) -> f32,
) -> Vec<f32> {
    let (w, h) = (figure.width as usize, figure.height as usize);
    let alpha = |i: usize| figure.matte[i] as f32 / 255.0;
    let mut matte: Vec<f32> = (0..w * h)
        .map(|i| smoothstep(0.4, 0.6, figure.hair[i] as f32 / 255.0) * alpha(i))
        .collect();
    // How far the labels' edge may be off: a few of their cells.
    let radius = (unit * 0.04).max(6.0) as usize;
    let labelled: Vec<f32> = (0..w * h)
        .map(|i| figure.hair[i] as f32 / 255.0 * alpha(i))
        .collect();
    let near = box_blur(&labelled, w, h, radius);
    let mut band: Option<[usize; 4]> = None;
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if near[i] > 0.02 && near[i] < 0.98 && under_line(x, y) > -(radius as f32) {
                let b = band.get_or_insert([x, y, x, y]);
                *b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
            }
        }
    }
    let Some([x0, y0, x1, y1]) = band else {
        return matte;
    };
    let area = [
        x0.saturating_sub(radius * 2),
        y0.saturating_sub(radius * 2),
        (x1 + radius * 2).min(w - 1),
        (y1 + radius * 2).min(h - 1),
    ];
    let (strand, _) = spread_in(person, w, area, &|i| {
        if near[i] >= 0.98 {
            alpha(i)
        } else {
            0.0
        }
    });
    let (beside, _) = spread_in(person, w, area, &|i| {
        if near[i] <= 0.02 && figure.matte[i] >= 250 {
            1.0
        } else {
            0.0
        }
    });
    let across = area[2] - area[0] + 1;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let i = y * w + x;
            if !(near[i] > 0.02 && near[i] < 0.98) || under_line(x, y) <= -(radius as f32) {
                continue;
            }
            let o = (y - area[1]) * across + (x - area[0]);
            let (hair, other) = (strand[o], beside[o]);
            let apart = [hair[0] - other[0], hair[1] - other[1], hair[2] - other[2]];
            let distance = apart.iter().map(|d| d * d).sum::<f32>();
            // Colours too alike tell nothing: the labels stand.
            if distance < 40.0 * 40.0 {
                continue;
            }
            let along: f32 = (0..3)
                .map(|c| (person[i * 4 + c] as f32 - other[c]) * apart[c])
                .sum::<f32>()
                / distance;
            // The colour decides between what the labels are sure of.
            let (least, most) = (
                smoothstep(0.85, 0.99, near[i]),
                smoothstep(0.02, 0.3, near[i]),
            );
            matte[i] = smoothstep(0.15, 0.85, along).clamp(least, most) * alpha(i);
        }
    }
    matte
}

/// The colours of `area` ([x0, y0, x1, y1]) of an RGBA picture `width` wide,
/// spread from the pixels `certain` weighs over the others; with them, the
/// weights.
fn spread_in(
    rgba: &[u8],
    width: usize,
    [x0, y0, x1, y1]: [usize; 4],
    certain: &dyn Fn(usize) -> f32,
) -> (Vec<[f32; 3]>, Vec<f32>) {
    let (bw, bh) = (x1 - x0 + 1, y1 - y0 + 1);
    let mut colour = Vec::with_capacity(bw * bh);
    let mut weight = Vec::with_capacity(bw * bh);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let i = y * width + x;
            colour.push([
                rgba[i * 4] as f32,
                rgba[i * 4 + 1] as f32,
                rgba[i * 4 + 2] as f32,
            ]);
            weight.push(certain(i));
        }
    }
    spread(&mut colour, &weight, bw, bh);
    (colour, weight)
}

/// Old clothes that rise over the collar's line (a collar's points, a hood's
/// rim): what is neither skin nor hair there and joins the clothes under the
/// line, no higher than row `ceiling`. 0..1 per pixel, soft at its rim.
fn risen_clothes(
    figure: &Figure,
    ceiling: f32,
    under_line: &dyn Fn(usize, usize) -> f32,
) -> Vec<f32> {
    let (w, h) = (figure.width as usize, figure.height as usize);
    let clothes = |i: usize| figure.other[i] >= CLOTHES && figure.matte[i] >= SOLID / 2;
    let top = (ceiling.max(0.0) as usize).min(h);
    let mut found = vec![false; w * h];
    let mut queue = Vec::new();
    for y in top..h {
        for x in 0..w {
            let i = y * w + x;
            if clothes(i) && (0.0..2.0).contains(&under_line(x, y)) {
                found[i] = true;
                queue.push(i);
            }
        }
    }
    while let Some(i) = queue.pop() {
        let (x, y) = (i % w, i / w);
        let around = [
            (x.wrapping_sub(1), y),
            (x + 1, y),
            (x, y.wrapping_sub(1)),
            (x, y + 1),
        ];
        for (nx, ny) in around {
            if nx >= w || ny >= h || ny < top {
                continue;
            }
            let j = ny * w + nx;
            if !found[j] && clothes(j) && under_line(nx, ny) < 2.0 {
                found[j] = true;
                queue.push(j);
            }
        }
    }
    let mut soft = vec![0.0f32; w * h];
    for y in top..h {
        for x in 0..w {
            let mut sum = 0.0;
            for ny in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    sum += found[ny * w + nx] as u8 as f32;
                }
            }
            soft[y * w + x] = sum / 9.0;
        }
    }
    soft
}

impl Garment {
    /// A garment from a layer's pixels: the picture cut to what it holds.
    /// `None` when it holds nothing.
    pub fn trimmed(rgba: &[u8], width: u32, height: u32) -> Option<Self> {
        let (w, h) = (width as usize, height as usize);
        if w == 0 || rgba.len() != w * h * 4 {
            return None;
        }
        let shows = |x: usize, y: usize| rgba[(y * w + x) * 4 + 3] >= 8;
        let top = (0..h).find(|&y| (0..w).any(|x| shows(x, y)))?;
        let bottom = (top..h).rfind(|&y| (0..w).any(|x| shows(x, y)))?;
        let left = (0..w).find(|&x| (top..=bottom).any(|y| shows(x, y)))?;
        let right = (left..w).rfind(|&x| (top..=bottom).any(|y| shows(x, y)))?;
        let mut cut = Vec::with_capacity((right - left + 1) * (bottom - top + 1) * 4);
        for y in top..=bottom {
            cut.extend_from_slice(&rgba[(y * w + left) * 4..(y * w + right + 1) * 4]);
        }
        Some(Self {
            rgba: cut,
            width: (right - left + 1) as u32,
            height: (bottom - top + 1) as u32,
        })
    }
}

/// What dressing a photo needs of its person, read once by the models: kept,
/// it lays another garment at once.
pub struct Fitting {
    pub figure: Figure,
    pub face: FaceMarks,
    pub neck: Neck,
}

impl Fitting {
    /// Read the person of a cut-out photo: `person` is its straight-alpha
    /// RGBA, shown to the models over `backdrop`. `progress` gets short
    /// status lines.
    pub fn read(
        person: &[u8],
        width: u32,
        height: u32,
        backdrop: [u8; 3],
        prefer_gpu: bool,
        progress: &dyn Fn(String),
    ) -> Result<Self, String> {
        use super::ai::{body_parts::Segmenter, face_mesh};

        if width == 0 || person.len() != width as usize * height as usize * 4 {
            return Err("ảnh không hợp lệ".to_string());
        }
        let shown = super::imposition::over_backdrop(person, backdrop);
        progress("Đang tìm khuôn mặt…".to_string());
        let meshes = face_mesh::detect(&shown, width, height)?;
        let mesh = meshes
            .iter()
            .max_by(|a, b| a.frame().1.total_cmp(&b.frame().1))
            .ok_or_else(|| "không tìm thấy khuôn mặt nào".to_string())?;
        progress("Đang tìm cổ, tóc và áo…".to_string());
        let labels = Segmenter::load(prefer_gpu)?.segment_face(&shown, width, height, mesh)?;
        let matte: Vec<u8> = person.chunks_exact(4).map(|px| px[3]).collect();
        let figure = Figure::read(&labels, &matte, width, height);
        let face = FaceMarks::from_mesh(mesh);
        let neck = neck_of(&figure, &face);
        Ok(Self { figure, face, neck })
    }

    /// The photo's layers with `person` (now, as the photo shows it) dressed
    /// in `garment`.
    pub fn dress(&self, person: &[u8], garment: &Garment, collar: &Collar) -> Dressed {
        let photo = (self.figure.width, self.figure.height);
        let placement = Placement::fit(collar, garment.height, &self.neck, photo);
        dress(
            person,
            &self.figure,
            &self.face,
            garment,
            collar,
            &placement,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shirt 200 wide: shoulders from row 60, a collar rising to row 10 at
    /// columns 80 and 120, open between them down to row 50 at the middle.
    fn shirt() -> Garment {
        let (w, h) = (200usize, 120usize);
        let mut rgba = vec![0u8; w * h * 4];
        for x in 0..w {
            let from_middle = (x as i32 - 100).unsigned_abs() as usize;
            let top = if from_middle <= 20 {
                // The opening: 50 at the middle up to 10 at the collar's points.
                50 - from_middle * 2
            } else {
                // The collar falling away into the shoulder.
                (10 + (from_middle - 20)).min(60)
            };
            for y in top..h {
                rgba[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[240, 240, 250, 255]);
            }
        }
        Garment {
            rgba,
            width: w as u32,
            height: h as u32,
        }
    }

    #[test]
    fn a_collar_is_its_two_points_and_the_dip_between_them() {
        let collar = shirt().collar().expect("a collar");
        assert_eq!(collar.left, [80.5, 10.0]);
        assert_eq!(collar.right, [120.5, 10.0]);
        assert_eq!(collar.notch, [100.5, 50.0]);
        // A plain block has no opening to lay a neck in.
        let block = Garment {
            rgba: vec![255u8; 60 * 40 * 4],
            width: 60,
            height: 40,
        };
        assert_eq!(block.collar(), None);
    }

    #[test]
    fn the_opening_is_what_lies_between_the_collars_points_over_the_garment() {
        let shirt = shirt();
        let collar = shirt.collar().unwrap();
        let plane = shirt.opening(&collar);
        let at = |x: usize, y: usize| plane[y * 200 + x];
        assert_eq!(at(100, 30), 255, "under the line, over the notch");
        assert_eq!(at(100, 55), 0, "the garment itself");
        assert_eq!(at(100, 5), 0, "over the collar's line");
        assert_eq!(at(60, 30), 0, "beside the collar");
        // Beside a point the collar falls away from, only what lies under
        // the garment's edge: two columns out the edge is at row 12.
        assert_eq!(at(78, 11), 0, "bare beside the point");
        assert_eq!(at(78, 13), 255, "under the edge beside the point");
    }

    /// A photo 400x600 of a head (rows 100..300), a neck 80 wide to row 360
    /// and shoulders under it; hair down the left when `long_hair`.
    fn figure(long_hair: bool) -> (Figure, FaceMarks) {
        let (w, h) = (400usize, 600usize);
        let mut matte = vec![0u8; w * h];
        let mut hair = vec![0u8; w * h];
        let mut skin = vec![0u8; w * h];
        let mut other = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                let from_axis = (x as i32 - 200).abs();
                let i = y * w + x;
                let (head, neck, shoulders) = (
                    (100..300).contains(&y) && from_axis < 70,
                    (300..360).contains(&y) && from_axis < 40,
                    y >= 360 && from_axis < 40 + (y as i32 - 360) * 3,
                );
                if head || neck || shoulders {
                    matte[i] = 255;
                }
                if head || neck {
                    skin[i] = 255;
                }
                if long_hair && (100..520).contains(&y) && (110..165).contains(&x) {
                    matte[i] = 255;
                    hair[i] = 255;
                    skin[i] = 0;
                }
                if matte[i] > 0 && hair[i] == 0 && skin[i] == 0 {
                    other[i] = 255;
                }
            }
        }
        let face = FaceMarks {
            eyes: [200.0, 190.0],
            chin: [200.0, 300.0],
            cheeks: [200.0, 220.0],
            nose: [200.0, 230.0],
            tilt: 0.0,
            width: 140.0,
        };
        (
            Figure {
                width: w as u32,
                height: h as u32,
                matte,
                hair,
                skin,
                other,
            },
            face,
        )
    }

    #[test]
    fn the_neck_ends_where_the_outline_turns_into_shoulder() {
        let (figure, face) = figure(false);
        let neck = neck_of(&figure, &face);
        // Half the neck is 40; the shoulder is a fifth wider four rows on.
        assert!((neck.left[0] - 160.0).abs() <= 1.0, "{neck:?}");
        assert!((neck.right[0] - 240.0).abs() <= 1.0, "{neck:?}");
        for side in [neck.left, neck.right] {
            assert!((360.0..=366.0).contains(&side[1]), "{neck:?}");
        }
    }

    #[test]
    fn a_side_under_hair_takes_the_other_sides_measure() {
        let (figure, face) = figure(true);
        let neck = neck_of(&figure, &face);
        assert!((neck.left[0] - 160.0).abs() <= 1.0, "{neck:?}");
        assert!((neck.right[0] - 240.0).abs() <= 1.0, "{neck:?}");
        assert_eq!(neck.left[1], neck.right[1]);
    }

    #[test]
    fn the_collars_points_land_on_the_neck_and_the_garment_reaches_the_bottom() {
        let shirt = shirt();
        let collar = shirt.collar().unwrap();
        let (figure, face) = figure(false);
        let neck = neck_of(&figure, &face);
        let placement = Placement::fit(&collar, shirt.height, &neck, (figure.width, figure.height));
        let near = |a: [f32; 2], b: [f32; 2]| (a[0] - b[0]).hypot(a[1] - b[1]) < 0.01;
        assert!(near(placement.to_photo(collar.left), neck.left));
        assert!(near(placement.to_photo(collar.right), neck.right));
        // There and back, above the stretch and under it.
        for g in [[30.0, 20.0], [150.0, 110.0]] {
            assert!(near(placement.to_garment(placement.to_photo(g)), g));
        }
        // Twice the size, the shirt ends at row 580 of 600: its rows under
        // the notch are drawn taller to reach.
        assert!((placement.scale - 2.0).abs() < 0.05, "{placement:?}");
        assert!(placement.stretch > 1.0 && placement.stretch <= MAX_STRETCH);
        let bottom = placement.to_photo([100.0, shirt.height as f32]);
        assert!(bottom[1] >= figure.height as f32 - 0.5, "{bottom:?}");
    }

    #[test]
    fn dressing_takes_the_old_clothes_keeps_the_hair_and_lays_skin_in_the_opening() {
        let shirt = shirt();
        let collar = shirt.collar().unwrap();
        let (figure, face) = figure(true);
        let w = figure.width as usize;
        let person = painted(&figure);
        let neck = neck_of(&figure, &face);
        let placement = Placement::fit(&collar, shirt.height, &neck, (figure.width, figure.height));
        let dressed = dress(&person, &figure, &face, &shirt, &collar, &placement);
        let px = |buffer: &[u8], x: usize, y: usize| {
            let i = (y * w + x) * 4;
            [buffer[i], buffer[i + 1], buffer[i + 2], buffer[i + 3]]
        };
        // The head and the neck are as they were.
        assert_eq!(px(&dressed.person, 200, 200), px(&person, 200, 200));
        assert_eq!(px(&dressed.person, 200, 340), px(&person, 200, 340));
        // The red top is gone from under the collar's line, beside the
        // garment and under it.
        assert_eq!(px(&dressed.person, 300, 450)[3], 0);
        assert_eq!(px(&dressed.person, 330, 590)[3], 0);
        // In the opening, where the top was, there is skin now.
        let laid = px(&dressed.person, 200, 400);
        assert_eq!(laid[3], 255);
        assert!(laid[0] > 150 && laid[1] > 110 && laid[1] < 200, "{laid:?}");
        // The hair that falls over the shoulder stays, and lies over the
        // garment as a layer of its own; over the collar's line that layer
        // is clear.
        let hair = dressed.hair.as_deref().expect("hair over the garment");
        assert_eq!(px(&dressed.person, 135, 480), [10, 10, 10, 255]);
        assert_eq!(px(hair, 135, 480), [10, 10, 10, 255]);
        assert_eq!(px(hair, 135, 300)[3], 0);
        // The garment is whole: it covers the shoulder, lies under the hair
        // and reaches the photo's bottom.
        assert_eq!(piece_px(&dressed.garment, 280, 520), [240, 240, 250, 255]);
        assert_eq!(piece_px(&dressed.garment, 135, 480), [240, 240, 250, 255]);
        assert_eq!(piece_px(&dressed.garment, 200, 599), [240, 240, 250, 255]);
        assert!(
            dressed.garment.offset.0 <= 5,
            "{:?}",
            dressed.garment.offset
        );
        // Around the opening the person has skin under the garment.
        let under = px(&dressed.person, 200, 455);
        assert_eq!(under[3], 255);
        assert!(under[0] > 120 && under[2] < 160, "{under:?}");
    }

    /// The alpha of a piece at a point of the photo.
    fn piece_px(piece: &Piece, x: usize, y: usize) -> [u8; 4] {
        let (px, py) = (x as i32 - piece.offset.0, y as i32 - piece.offset.1);
        if px < 0 || py < 0 || px >= piece.width as i32 || py >= piece.height as i32 {
            return [0; 4];
        }
        let i = (py as usize * piece.width as usize + px as usize) * 4;
        [
            piece.rgba[i],
            piece.rgba[i + 1],
            piece.rgba[i + 2],
            piece.rgba[i + 3],
        ]
    }

    /// The person of `figure`: skin where skin is, black hair, a red top.
    fn painted(figure: &Figure) -> Vec<u8> {
        let mut person = vec![0u8; figure.matte.len() * 4];
        for i in 0..figure.matte.len() {
            let colour = if figure.hair[i] > 0 {
                [10, 10, 10]
            } else if figure.skin[i] > 0 {
                [220, 170, 140]
            } else {
                [200, 0, 0]
            };
            person[i * 4..i * 4 + 3].copy_from_slice(&colour);
            person[i * 4 + 3] = figure.matte[i];
        }
        person
    }

    #[test]
    fn old_clothes_that_rise_over_the_collars_line_go_too() {
        let shirt = shirt();
        let collar = shirt.collar().unwrap();
        let (mut figure, face) = figure(false);
        let w = figure.width as usize;
        let neck = neck_of(&figure, &face);
        let placement = Placement::fit(&collar, shirt.height, &neck, (figure.width, figure.height));
        // The old top's collar stands 20 rows over the line: a point beside
        // the neck, and a band across its front.
        for y in 340..360 {
            for x in (156..166).chain(190..210) {
                let i = y * w + x;
                (figure.matte[i], figure.skin[i], figure.other[i]) = (255, 0, 255);
            }
        }
        let person = painted(&figure);
        let dressed = dress(&person, &figure, &face, &shirt, &collar, &placement);
        let px = |x: usize, y: usize| {
            let i = (y * w + x) * 4;
            [
                dressed.person[i],
                dressed.person[i + 1],
                dressed.person[i + 2],
                dressed.person[i + 3],
            ]
        };
        // Beside the neck nothing is left; across it there is skin.
        assert_eq!(px(158, 350)[3], 0);
        assert_eq!(px(200, 350), [220, 170, 140, 255]);
        // The neck's own skin beside the band is as it was.
        assert_eq!(px(175, 350), [220, 170, 140, 255]);
    }

    #[test]
    fn the_neck_widens_to_the_points_of_a_collar_a_little_wider_than_it() {
        let shirt = shirt();
        let collar = shirt.collar().unwrap();
        let (figure, face) = figure(false);
        let w = figure.width as usize;
        let person = painted(&figure);
        let alpha = |buffer: &[u8], x: usize, y: usize| buffer[(y * w + x) * 4 + 3];
        let laid = |left: f32, right: f32| {
            let neck = Neck {
                left: [left, 350.0],
                right: [right, 350.0],
            };
            let placement =
                Placement::fit(&collar, shirt.height, &neck, (figure.width, figure.height));
            dress(&person, &figure, &face, &shirt, &collar, &placement).person
        };
        // The neck's edge is at 160; the collar's point lies 10 further out,
        // on row 350. The wedge between them holds skin, up to the slant
        // from the point; over the slant the photo is bare as before.
        let wide = laid(150.0, 250.0);
        assert_eq!(alpha(&person, 156, 346), 0);
        assert_eq!(alpha(&wide, 156, 346), 255);
        assert_eq!(alpha(&wide, 243, 346), 255);
        let skin = &wide[(346 * w + 156) * 4..(346 * w + 156) * 4 + 3];
        assert!(skin[0] > 150 && skin[2] < 190, "{skin:?}");
        assert_eq!(alpha(&wide, 152, 340), 0);
        assert_eq!(alpha(&wide, 156, 330), 0);
        // No row of the line is left half bare under the wedge.
        for y in 348..353 {
            assert_eq!(alpha(&wide, 157, y), 255, "row {y}");
        }
        // A collar far wider than the neck is not the neck's to reach.
        let far = laid(120.0, 280.0);
        assert_eq!(alpha(&far, 156, 346), 0);
        assert_eq!(alpha(&far, 126, 346), 0);
    }

    #[test]
    fn old_clothes_leave_no_line_where_the_collars_line_cuts_them() {
        let shirt = shirt();
        let collar = shirt.collar().unwrap();
        let (figure, face) = figure(false);
        let w = figure.width as usize;
        let person = painted(&figure);
        let neck = neck_of(&figure, &face);
        let placement = Placement::fit(&collar, shirt.height, &neck, (figure.width, figure.height));
        let dressed = dress(&person, &figure, &face, &shirt, &collar, &placement);
        let cover = dressed.garment.on_photo(figure.width, figure.height);
        // The old top crosses the line beside the neck. Around the line,
        // what is not under the garment there is skin, whole, or as good as
        // nothing: no row of the top is left at a quarter of itself, and
        // none of its red in the skin.
        let line = neck.left[1].round() as usize;
        for y in line - 3..=line + 3 {
            for x in (140..159).chain(242..260) {
                let i = (y * w + x) * 4;
                if cover[i + 3] > 0 {
                    continue;
                }
                let px = &dressed.person[i..i + 4];
                assert!(
                    px[3] < 16 || (px[3] == 255 && px[1] > 120),
                    "({x}, {y}): {px:?}"
                );
            }
        }
    }

    #[test]
    fn hair_is_told_from_the_clothes_beside_it_by_its_colour() {
        let shirt = shirt();
        let collar = shirt.collar().unwrap();
        let (mut figure, face) = figure(true);
        let w = figure.width as usize;
        let person = painted(&figure);
        // The labels run four pixels over the red top beside the hair.
        for y in 100..520 {
            for x in 165..169 {
                let i = y * w + x;
                if figure.matte[i] > 0 {
                    (figure.hair[i], figure.skin[i], figure.other[i]) = (255, 0, 0);
                }
            }
        }
        let neck = neck_of(&figure, &face);
        let placement = Placement::fit(&collar, shirt.height, &neck, (figure.width, figure.height));
        let dressed = dress(&person, &figure, &face, &shirt, &collar, &placement);
        let alpha = |buffer: &[u8], x: usize, y: usize| buffer[(y * w + x) * 4 + 3];
        // The top the labels took for hair is gone, and is no hair to lie
        // over the garment; the hair itself is.
        let hair = dressed.hair.expect("hair over the garment");
        assert_eq!(alpha(&dressed.person, 166, 500), 0);
        assert_eq!(alpha(&hair, 166, 500), 0);
        assert_eq!(alpha(&dressed.person, 160, 500), 255);
        assert_eq!(alpha(&hair, 160, 500), 255);
        // The garment is whole under both.
        assert_eq!(piece_px(&dressed.garment, 166, 500)[3], 255);
        assert_eq!(piece_px(&dressed.garment, 160, 500)[3], 255);
    }

    fn over(base: &mut [u8], layer: &[u8]) {
        for (under, px) in base.chunks_exact_mut(3).zip(layer.chunks_exact(4)) {
            let a = px[3] as f32 / 255.0;
            for c in 0..3 {
                under[c] = (px[c] as f32 * a + under[c] as f32 * (1.0 - a)).round() as u8;
            }
        }
    }

    fn multiply(base: &mut [u8], layer: &[u8]) {
        for (under, px) in base.chunks_exact_mut(3).zip(layer.chunks_exact(4)) {
            let a = px[3] as f32 / 255.0;
            for c in 0..3 {
                let left = 1.0 - a * (1.0 - px[c] as f32 / 255.0);
                under[c] = (under[c] as f32 * left).round() as u8;
            }
        }
    }

    /// Opt-in visual probe. IAI_GARMENT_PROBE is a folder of ID photos,
    /// IAI_GARMENT_PSD the shop's garment files (`;` between them),
    /// IAI_GARMENT_OUT where the results go: each photo dressed in
    /// IAI_GARMENT_PICK garments of every file (4 unless given) without its
    /// shade (`..jpg`) and with it (`..__vien.jpg`), and a sheet
    /// `_<photo>.jpg` of the photo beside them. With IAI_GARMENT_LAYERS the
    /// layers of each are kept too: the person's (`..__nguoi.png`, and
    /// `..__nguoi_da_co.png` as a retouch with "Da cổ" at IAI_GARMENT_NECK
    /// leaves it, the whole photo then being `..__da_co_vien.jpg`), the
    /// garment's (`..__ao.png`; `..__ao_anh.png` as it lies on the photo)
    /// and the hair's over it (`..__toc.png`). A garment as wide as its
    /// sheet is taken only with IAI_GARMENT_WIDE.
    #[test]
    #[ignore]
    fn probe_dressed_photos() {
        use crate::core::ai::{body_parts::Segmenter, face_mesh};
        use crate::core::id_photo::{self, IdPhotoOptions};
        use crate::formats::Importer;

        let (Ok(photos), Ok(files), Ok(out)) = (
            std::env::var("IAI_GARMENT_PROBE"),
            std::env::var("IAI_GARMENT_PSD"),
            std::env::var("IAI_GARMENT_OUT"),
        ) else {
            return;
        };
        let pick: usize = std::env::var("IAI_GARMENT_PICK")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4);
        let out = std::path::PathBuf::from(out);
        std::fs::create_dir_all(&out).unwrap();

        let mut garments: Vec<(String, Garment, Collar)> = Vec::new();
        for file in files.split(';') {
            let path = std::path::Path::new(file);
            let stem = path.file_stem().unwrap().to_string_lossy().to_string();
            let canvas = crate::formats::psd::PsdImporter.import(path).unwrap();
            let found: Vec<(usize, Garment, Collar)> = canvas
                .layer_stack
                .layers
                .iter()
                .enumerate()
                .filter(|(_, layer)| {
                    layer.width < canvas.width * 9 / 10 || std::env::var("IAI_GARMENT_WIDE").is_ok()
                })
                .filter_map(|(i, layer)| {
                    let garment = Garment {
                        rgba: layer.tiles.flatten(),
                        width: layer.width,
                        height: layer.height,
                    };
                    let collar = garment.collar()?;
                    Some((i, garment, collar))
                })
                .collect();
            println!(
                "{stem}: {} layers, a collar found on {}",
                canvas.layer_stack.layers.len(),
                found.len()
            );
            let step = (found.len() / pick.max(1)).max(1);
            for (i, garment, collar) in found.into_iter().step_by(step).take(pick) {
                garments.push((format!("{stem}_{i}"), garment, collar));
            }
        }

        let mut segmenter = Segmenter::load(false).unwrap();
        for entry in std::fs::read_dir(&photos).unwrap() {
            let path = entry.unwrap().path();
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if !["jpg", "jpeg", "png"].contains(&ext.as_str()) {
                continue;
            }
            let name = path.file_stem().unwrap().to_string_lossy().to_string();
            let name: String = name.chars().take(24).collect();
            let image = image::open(&path).unwrap().to_rgba8();
            let (photo_w, photo_h) = image.dimensions();
            let started = std::time::Instant::now();
            let mut segment = |px: &[u8], sw: u32, sh: u32| {
                crate::core::select_subject::segment_blocking(
                    crate::core::select_subject::SelectSubjectModel::BiRefNetTiny,
                    px,
                    sw,
                    sh,
                    false,
                )
            };
            // The ID photo as "Làm ảnh thẻ tự động" makes it, on the colour
            // the photo came on.
            let corner = image.get_pixel(2, 2).0;
            let options = IdPhotoOptions {
                backdrop: if corner[2] > corner[0].saturating_add(40) {
                    crate::core::imposition::Backdrop::Blue
                } else {
                    crate::core::imposition::Backdrop::White
                },
                ..Default::default()
            };
            let plan = match id_photo::prepare(
                image.as_raw(),
                photo_w,
                photo_h,
                None,
                &options,
                &mut segment,
                &|_| {},
            ) {
                Ok(plan) => plan,
                Err(e) => {
                    println!("{name}: {e}");
                    continue;
                }
            };
            let notes = plan.notes.join("; ");
            let mut canvas =
                crate::core::canvas::Canvas::from_rgba(image.into_raw(), photo_w, photo_h);
            id_photo::apply(&mut canvas, plan).unwrap();
            canvas.ensure_pixels();
            let (w, h) = (canvas.width, canvas.height);
            let made = canvas.pixels.clone();
            // The cut-out person over the whole photo.
            let layer = canvas
                .layer_stack
                .layers
                .iter()
                .rfind(|l| l.visible && !l.is_background)
                .unwrap();
            let own = layer.tiles.flatten();
            let mut person = vec![0u8; (w * h * 4) as usize];
            for ly in 0..layer.height as i32 {
                for lx in 0..layer.width as i32 {
                    let (x, y) = (lx + layer.offset.0, ly + layer.offset.1);
                    if x >= 0 && y >= 0 && x < w as i32 && y < h as i32 {
                        let (from, to) = (
                            ((ly as u32 * layer.width + lx as u32) * 4) as usize,
                            ((y as u32 * w + x as u32) * 4) as usize,
                        );
                        person[to..to + 4].copy_from_slice(&own[from..from + 4]);
                    }
                }
            }
            let matte: Vec<u8> = person.chunks_exact(4).map(|px| px[3]).collect();
            let meshes = face_mesh::detect(&made, w, h).unwrap();
            let Some(mesh) = meshes
                .iter()
                .max_by(|a, b| a.frame().1.total_cmp(&b.frame().1))
            else {
                println!("{name}: no face");
                continue;
            };
            let face = FaceMarks::from_mesh(mesh);
            let labels = segmenter.segment_face(&made, w, h, mesh).unwrap();
            let figure = Figure::read(&labels, &matte, w, h);
            let neck = neck_of(&figure, &face);
            println!(
                "{name}: {photo_w}x{photo_h} -> {w}x{h}, read in {} ms; chin {:?}, neck {:?} {notes}",
                started.elapsed().as_millis(),
                face.chin,
                neck
            );

            let plain: Vec<u8> = options.backdrop.rgb().repeat((w * h) as usize);
            let made_rgb: Vec<u8> = made
                .chunks_exact(4)
                .flat_map(|px| [px[0], px[1], px[2]])
                .collect();
            let mut panels = vec![image::RgbImage::from_raw(w, h, made_rgb).unwrap()];
            for (label, garment, collar) in &garments {
                let started = std::time::Instant::now();
                let placement = Placement::fit(collar, garment.height, &neck, (w, h));
                let dressed = dress(&person, &figure, &face, garment, collar, &placement);
                let mut flat = plain.clone();
                over(&mut flat, &dressed.person);
                let laid: Vec<u8> = (0..h as usize)
                    .flat_map(|y| (0..w as usize).map(move |x| (x, y)))
                    .flat_map(|(x, y)| piece_px(&dressed.garment, x, y))
                    .collect();
                over(&mut flat, &laid);
                if let Some(hair) = &dressed.hair {
                    over(&mut flat, hair);
                }
                println!(
                    "    {label}: x{:.2}, stretch {:.2}, {} ms",
                    placement.scale,
                    placement.stretch,
                    started.elapsed().as_millis()
                );
                let panel = image::RgbImage::from_raw(w, h, flat).unwrap();
                panel
                    .save(out.join(format!("{name}__{label}.jpg")))
                    .unwrap();
                // The same with the shade between the two.
                if let Some(shade) = &dressed.shade {
                    let mut flat = plain.clone();
                    over(&mut flat, &dressed.person);
                    over(&mut flat, &laid);
                    multiply(&mut flat, shade);
                    if let Some(hair) = &dressed.hair {
                        over(&mut flat, hair);
                    }
                    image::RgbImage::from_raw(w, h, flat)
                        .unwrap()
                        .save(out.join(format!("{name}__{label}__vien.jpg")))
                        .unwrap();
                }
                if std::env::var("IAI_GARMENT_LAYERS").is_ok() {
                    // The person's layer as the retouch is given it, and the
                    // garment's as its light sliders are.
                    image::RgbaImage::from_raw(w, h, dressed.person.clone())
                        .unwrap()
                        .save(out.join(format!("{name}__{label}__nguoi.png")))
                        .unwrap();
                    let piece = &dressed.garment;
                    image::RgbaImage::from_raw(piece.width, piece.height, piece.rgba.clone())
                        .unwrap()
                        .save(out.join(format!("{name}__{label}__ao.png")))
                        .unwrap();
                    // The garment and the hair over it as they lie on the
                    // photo, and where the face is.
                    image::RgbaImage::from_raw(w, h, laid.clone())
                        .unwrap()
                        .save(out.join(format!("{name}__{label}__ao_anh.png")))
                        .unwrap();
                    if let Some(hair) = &dressed.hair {
                        image::RgbaImage::from_raw(w, h, hair.clone())
                            .unwrap()
                            .save(out.join(format!("{name}__{label}__toc.png")))
                            .unwrap();
                    }
                    let [r, g, b] = options.backdrop.rgb();
                    std::fs::write(
                        out.join(format!("{name}__{label}.txt")),
                        format!(
                            "eyes {} {}\nchin {} {}\nwidth {}\nbackdrop {r} {g} {b}\n",
                            face.eyes[0], face.eyes[1], face.chin[0], face.chin[1], face.width
                        ),
                    )
                    .unwrap();
                    // The person as the retouch leaves the neck.
                    if let Some(neck) = std::env::var("IAI_GARMENT_NECK")
                        .ok()
                        .and_then(|v| v.parse::<f32>().ok())
                    {
                        use crate::core::portrait::{analyze, neck::analyze_necks, render};
                        let model = analyze(&dressed.person, w, h, false, None, &|_| {}).unwrap();
                        let enabled = vec![true; model.faces.len()];
                        analyze_necks(&dressed.person, &model, &enabled);
                        let settings = crate::core::portrait::PortraitSettings {
                            neck,
                            ..crate::core::portrait::PortraitSettings::NEUTRAL
                        };
                        let mut out_px = dressed.person.clone();
                        if let Some((u, px)) =
                            render(&dressed.person, &model, &settings, &enabled, &[])
                        {
                            let (uw, ww) = (u.w as usize * 4, w as usize * 4);
                            for y in 0..u.h as usize {
                                let o = (u.y as usize + y) * ww + u.x as usize * 4;
                                out_px[o..o + uw].copy_from_slice(&px[y * uw..(y + 1) * uw]);
                            }
                        }
                        // The photo as the owner sees it after that retouch.
                        let mut flat = plain.clone();
                        over(&mut flat, &out_px);
                        over(&mut flat, &laid);
                        if let Some(shade) = &dressed.shade {
                            multiply(&mut flat, shade);
                        }
                        if let Some(hair) = &dressed.hair {
                            over(&mut flat, hair);
                        }
                        image::RgbImage::from_raw(w, h, flat)
                            .unwrap()
                            .save(out.join(format!("{name}__{label}__da_co_vien.jpg")))
                            .unwrap();
                        image::RgbaImage::from_raw(w, h, out_px)
                            .unwrap()
                            .save(out.join(format!("{name}__{label}__nguoi_da_co.png")))
                            .unwrap();
                    }
                }
                panels.push(panel);
            }
            // The photo and every result side by side, 640 tall.
            let tall = 640u32;
            let wide = w * tall / h;
            let mut sheet = image::RgbImage::new(wide * panels.len() as u32, tall);
            for (i, panel) in panels.iter().enumerate() {
                let small = image::imageops::resize(
                    panel,
                    wide,
                    tall,
                    image::imageops::FilterType::Triangle,
                );
                image::imageops::replace(&mut sheet, &small, (wide * i as u32) as i64, 0);
            }
            sheet.save(out.join(format!("_{name}.jpg"))).unwrap();
        }
    }

    #[test]
    fn spreading_fills_the_weightless_from_the_weighted() {
        let (w, h) = (8usize, 8usize);
        let mut colour = vec![[0.0f32; 3]; w * h];
        let mut weight = vec![0.0f32; w * h];
        for y in 0..h {
            (colour[y * w], weight[y * w]) = ([200.0, 100.0, 50.0], 1.0);
        }
        spread(&mut colour, &weight, w, h);
        for px in &colour {
            assert!(
                (px[0] - 200.0).abs() < 1.0 && (px[2] - 50.0).abs() < 1.0,
                "{px:?}"
            );
        }
    }
}

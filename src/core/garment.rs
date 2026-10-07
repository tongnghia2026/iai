//! Lay a garment cut out on a transparent ground (a shop's "phôi áo") on an
//! ID photo: find the collar's two points on the garment and the two sides
//! of the neck on the person, and lay the one on the other.
//!
//! Nothing here is generated, and nothing of the photo is touched: the
//! garment is the shop's own picture, moved, scaled and turned, a piece to
//! lie over the photo. The person is read from what the retouch already
//! finds (the matte, the face's landmarks, the hair labels).

use rayon::prelude::*;

use super::ai::body_parts::{PartLabels, GROUP_HAIR};
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
/// neck asks at most; its opening is then wider than the neck.
const MAX_OVERSIZE: f32 = 1.35;
/// Alpha from which a pixel is the cloth whole. A pixel's colour is the
/// cloth's own when every pixel `FRINGE` around it is whole; a pixel of the
/// garment's edge takes the colour of such pixels up to `FRINGE_REACH` away.
const WHOLE: u8 = 250;
const FRINGE: i32 = 1;
const FRINGE_REACH: i32 = 3;
/// The edge of a garment laid larger is drawn this many times steeper at
/// most. An edge is where the alpha varies within `EDGE_NEAR` pixels of the
/// garment: by how much (0..1) for it to begin to count as one, and to count
/// as one fully.
const MAX_CRISP: f32 = 4.0;
const EDGE_NEAR: usize = 3;
const EDGE_SPAN: (f32, f32) = (0.5, 0.9);

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
}

/// What the fit reads of the person, each a plane of the photo, 0..255.
pub struct Figure {
    pub width: u32,
    pub height: u32,
    /// The person against the background.
    pub matte: Vec<u8>,
    pub hair: Vec<u8>,
}

impl Figure {
    pub fn read(labels: &PartLabels, matte: &[u8], width: u32, height: u32) -> Self {
        let w = width as usize;
        let mut hair = vec![0u8; matte.len()];
        hair.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, hair) in row.iter_mut().enumerate() {
                let odds = labels.groups_at(x as f32 + 0.5, y as f32 + 0.5);
                *hair = (odds[GROUP_HAIR].clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        });
        Self {
            width,
            height,
            matte: matte.to_vec(),
            hair,
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

/// The garment's colours with those of its edge taken from just inside it.
/// A cut-out's edge pixels hold some of the ground it was cut from, which
/// shows as a rim around the garment once it is laid larger.
fn without_fringe(garment: &Garment) -> Vec<[f32; 3]> {
    let (w, h) = (garment.width as i32, garment.height as i32);
    let rgba = &garment.rgba;
    // The picture's sides cut the garment: past them it counts as whole.
    let whole = |x: i32, y: i32| {
        x < 0 || y < 0 || x >= w || y >= h || rgba[((y * w + x) * 4 + 3) as usize] >= WHOLE
    };
    let deep: Vec<bool> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (i % w, i / w);
            (-FRINGE..=FRINGE).all(|dy| (-FRINGE..=FRINGE).all(|dx| whole(x + dx, y + dy)))
        })
        .collect();
    (0..w * h)
        .into_par_iter()
        .map(|i| {
            let o = i as usize * 4;
            let own = [rgba[o] as f32, rgba[o + 1] as f32, rgba[o + 2] as f32];
            if deep[i as usize] || rgba[o + 3] == 0 {
                return own;
            }
            let (x, y) = (i % w, i / w);
            let (mut sum, mut weight) = ([0.0f32; 3], 0.0f32);
            for ny in (y - FRINGE_REACH).max(0)..=(y + FRINGE_REACH).min(h - 1) {
                for nx in (x - FRINGE_REACH).max(0)..=(x + FRINGE_REACH).min(w - 1) {
                    let n = (ny * w + nx) as usize;
                    if !deep[n] {
                        continue;
                    }
                    // The nearest count most.
                    let k = 1.0 / ((nx - x).pow(2) + (ny - y).pow(2)) as f32;
                    for c in 0..3 {
                        sum[c] += rgba[n * 4 + c] as f32 * k;
                    }
                    weight += k;
                }
            }
            if weight > 0.0 {
                sum.map(|v| v / weight)
            } else {
                own
            }
        })
        .collect()
}

/// How much each pixel of the garment lies at an edge of it, 0..1: how far
/// the alpha varies within `EDGE_NEAR` pixels around it. Past the picture's
/// sides and top the garment is clear; past its bottom it runs on.
fn edges_of(garment: &Garment) -> Vec<f32> {
    let (w, h) = (garment.width as usize, garment.height as usize);
    let alpha: Vec<u8> = garment.rgba.chunks_exact(4).map(|px| px[3]).collect();
    // The least and the most alpha along each row, then down each column.
    let mut along = vec![(0u8, 0u8); w * h];
    along.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let line = &alpha[y * w..(y + 1) * w];
        for (x, cell) in row.iter_mut().enumerate() {
            let near = &line[x.saturating_sub(EDGE_NEAR)..(x + EDGE_NEAR + 1).min(w)];
            let cut = x < EDGE_NEAR || x + EDGE_NEAR >= w;
            let least = if cut { 0 } else { *near.iter().min().unwrap() };
            *cell = (least, *near.iter().max().unwrap());
        }
    });
    (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let rows = y.saturating_sub(EDGE_NEAR)..(y + EDGE_NEAR + 1).min(h);
            let (mut least, mut most) = (if y < EDGE_NEAR { 0 } else { 255 }, 0u8);
            for ny in rows {
                let (low, high) = along[ny * w + x];
                (least, most) = (least.min(low), most.max(high));
            }
            let span = most.saturating_sub(least) as f32 / 255.0;
            let t = ((span - EDGE_SPAN.0) / (EDGE_SPAN.1 - EDGE_SPAN.0)).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        })
        .collect()
}

/// The alpha of an edge enlarged `gain` times, drawn that much steeper about
/// its middle: the edge is then as wide as the garment's own was, and lies
/// where it lay. `edge` (0..1) is how much of an edge it is.
fn crisper(alpha: f32, edge: f32, gain: f32) -> f32 {
    if gain <= 1.0 || edge <= 0.0 {
        return alpha;
    }
    let steep = (0.5 + (alpha - 0.5) * gain).clamp(0.0, 1.0);
    alpha + (steep - alpha) * edge
}

/// A garment made ready to be drawn: its colour, premultiplied, and where
/// its edges are.
struct Cloth {
    width: usize,
    height: usize,
    colour: Vec<[f32; 4]>,
    edge: Vec<f32>,
}

impl Cloth {
    fn of(garment: &Garment) -> Self {
        let (w, h) = (garment.width as usize, garment.height as usize);
        let mut colour: Vec<[f32; 4]> = without_fringe(garment)
            .into_iter()
            .zip(garment.rgba.chunks_exact(4))
            .map(|(c, px)| {
                let a = px[3] as f32 / 255.0;
                [c[0] * a, c[1] * a, c[2] * a, a]
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
            edge: edges_of(garment),
        }
    }

    /// How much of an edge the garment has at a point of it, 0..1.
    fn edge_at(&self, x: f32, y: f32) -> f32 {
        let (w, h) = (self.width as i32, self.height as i32);
        let (ix, iy) = (x.floor() as i32, y.floor() as i32);
        self.edge[(iy.clamp(0, h - 1) * w + ix.clamp(0, w - 1)) as usize]
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
    /// where the garment is short of it. Laid larger than it is, its edge
    /// is kept as crisp as it was.
    fn laid(&self, placement: &Placement, photo: (usize, usize)) -> Piece {
        let crisp = placement.scale.clamp(1.0, MAX_CRISP);
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
                    if a <= 0.0 {
                        continue;
                    }
                    let shows = crisper(a, self.edge_at(gx, gy), crisp);
                    let shows = (shows * 255.0).round() as u8;
                    if shows > 0 {
                        row[x * 4] = (r / a).round() as u8;
                        row[x * 4 + 1] = (g / a).round() as u8;
                        row[x * 4 + 2] = (b / a).round() as u8;
                        row[x * 4 + 3] = shows;
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

/// What laying a garment on a photo needs of its person, read once by the
/// models: kept, it lays another garment at once.
pub struct Fitting {
    /// The photo's width and height.
    pub photo: (u32, u32),
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
        progress("Đang tìm cổ và tóc…".to_string());
        let labels = Segmenter::load(prefer_gpu)?.segment_face(&shown, width, height, mesh)?;
        let matte: Vec<u8> = person.chunks_exact(4).map(|px| px[3]).collect();
        let figure = Figure::read(&labels, &matte, width, height);
        let neck = neck_of(&figure, &FaceMarks::from_mesh(mesh));
        Ok(Self {
            photo: (width, height),
            neck,
        })
    }

    /// `garment` as it lies on the person: moved, scaled and turned to their
    /// neck, and nothing of the photo touched.
    pub fn lay(&self, garment: &Garment, collar: &Collar) -> Piece {
        let photo = self.photo;
        let placement = Placement::fit(collar, garment.height, &self.neck, photo);
        Cloth::of(garment).laid(&placement, (photo.0 as usize, photo.1 as usize))
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

    /// A photo 400x600 of a head (rows 100..300), a neck 80 wide to row 360
    /// and shoulders under it; hair down the left when `long_hair`.
    fn figure(long_hair: bool) -> (Figure, FaceMarks) {
        let (w, h) = (400usize, 600usize);
        let mut matte = vec![0u8; w * h];
        let mut hair = vec![0u8; w * h];
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
                if long_hair && (100..520).contains(&y) && (110..165).contains(&x) {
                    matte[i] = 255;
                    hair[i] = 255;
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

    /// A piece's pixel at a point of the photo.
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

    #[test]
    fn a_garment_is_laid_whole_from_the_neck_to_the_photos_bottom() {
        let shirt = shirt();
        let collar = shirt.collar().unwrap();
        let (figure, face) = figure(true);
        let fitting = Fitting {
            photo: (figure.width, figure.height),
            neck: neck_of(&figure, &face),
        };
        let piece = fitting.lay(&shirt, &collar);
        // It covers the shoulder and what hair falls there, and reaches the
        // photo's bottom; the neck in its opening and the photo over its
        // collar stay bare.
        assert_eq!(piece_px(&piece, 280, 520), [240, 240, 250, 255]);
        assert_eq!(piece_px(&piece, 135, 480), [240, 240, 250, 255]);
        assert_eq!(piece_px(&piece, 200, 599), [240, 240, 250, 255]);
        assert_eq!(piece_px(&piece, 200, 400)[3], 0);
        assert_eq!(piece_px(&piece, 200, 340)[3], 0);
        assert!(piece.offset.0 <= 5, "{:?}", piece.offset);
        // On the photo it is the same picture, cut to the photo.
        let on_photo = piece.on_photo(figure.width, figure.height);
        let i = (520 * figure.width as usize + 280) * 4;
        assert_eq!(on_photo[i..i + 4], [240, 240, 250, 255]);
        assert_eq!(on_photo.len(), 400 * 600 * 4);
    }

    /// A garment 60x40, whole from column 20 on, whose edge is two pixels
    /// of `rim` at a third and two thirds of full alpha.
    fn cut_out(cloth: [u8; 3], rim: [u8; 3]) -> Garment {
        let (w, h) = (60usize, 40usize);
        let mut rgba = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 18..w {
                let px = match x {
                    18 => [rim[0], rim[1], rim[2], 85],
                    19 => [rim[0], rim[1], rim[2], 170],
                    _ => [cloth[0], cloth[1], cloth[2], 255],
                };
                rgba[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&px);
            }
        }
        Garment {
            rgba,
            width: w as u32,
            height: h as u32,
        }
    }

    /// The garment's own pixels drawn `scale` times larger from the photo's
    /// corner.
    fn enlarged(scale: f32) -> Placement {
        Placement {
            scale,
            angle: 0.0,
            pivot: [0.0, 0.0],
            origin: [0.0, 0.0],
            stretch_from: f32::MAX,
            stretch: 1.0,
        }
    }

    #[test]
    fn a_garment_laid_larger_keeps_its_edge_as_crisp_as_it_was() {
        let garment = cut_out([20, 30, 40], [20, 30, 40]);
        let row = |scale: f32| -> Vec<u8> {
            let piece = Cloth::of(&garment).laid(&enlarged(scale), (400, 300));
            let (w, y) = (piece.width as usize, (20.0 * scale) as usize);
            (0..w).map(|x| piece.rgba[(y * w + x) * 4 + 3]).collect()
        };
        let soft = |row: &[u8]| row.iter().filter(|a| (13..243).contains(*a)).count();
        // Two pixels of edge in the garment are no more than three laid three
        // times larger; and the edge's middle lies where it lay.
        let (same, larger) = (row(1.0), row(3.0));
        assert_eq!(soft(&same), 2, "{same:?}");
        assert!(soft(&larger) <= 3, "{larger:?}");
        let middle = |row: &[u8]| row.iter().position(|a| *a >= 128).unwrap() as f32;
        assert!((middle(&larger) - middle(&same) * 3.0).abs() <= 2.0);
        // Inside it the garment is whole.
        assert!(larger[100..150].iter().all(|a| *a == 255));
    }

    #[test]
    fn a_cut_outs_rim_takes_the_colour_of_the_cloth_inside_it() {
        // A dark jacket cut from a light ground: its edge is light.
        let garment = cut_out([20, 30, 40], [200, 210, 220]);
        let piece = Cloth::of(&garment).laid(&enlarged(3.0), (400, 300));
        let lightest = piece
            .rgba
            .chunks_exact(4)
            .filter(|px| px[3] > 0)
            .map(|px| px[1])
            .max()
            .unwrap();
        assert!(lightest <= 40, "a rim as light as {lightest} is left");
        // A garment of one colour is as it was.
        let plain = cut_out([20, 30, 40], [20, 30, 40]);
        let clean = without_fringe(&plain);
        assert!(clean
            .iter()
            .zip(plain.rgba.chunks_exact(4))
            .all(|(c, px)| px[3] == 0 || (c[0] - 20.0).abs() + (c[2] - 40.0).abs() < 0.01));
    }

    #[test]
    fn a_sheer_part_of_a_garment_stays_sheer_when_laid_larger() {
        let mut garment = cut_out([20, 30, 40], [20, 30, 40]);
        // Half-clear from column 30 on.
        for y in 0..40 {
            for x in 30..60 {
                garment.rgba[(y * 60 + x) * 4 + 3] = 128;
            }
        }
        let piece = Cloth::of(&garment).laid(&enlarged(3.0), (400, 300));
        let at = |x: usize, y: usize| piece_px(&piece, x, y)[3];
        assert_eq!(at(140, 60), 128);
        assert_eq!(at(70, 60), 255);
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
    /// IAI_GARMENT_OUT where the results go: each photo wearing
    /// IAI_GARMENT_PICK garments of every file (4 unless given) as the app
    /// lays them (`..jpg`), then as "Chạy lại da cổ, viền áo" leaves them:
    /// with the shade (`..__vien.jpg`) and, the garment in the photo's
    /// light at IAI_GARMENT_MATCH (0..100) and the neck retouched at
    /// IAI_GARMENT_NECK, whichever are given, `..__xong.jpg`. A sheet
    /// `_<photo>.jpg` shows the photo beside the last of those for every
    /// garment. With IAI_GARMENT_LAYERS the layers are kept too: the
    /// person's (`<photo>__nguoi.png`) and the garment's as laid
    /// (`..__ao.png`). A garment as wide as its sheet is taken only with
    /// IAI_GARMENT_WIDE.
    #[test]
    #[ignore]
    fn probe_dressed_photos() {
        use crate::core::ai::{body_parts::Segmenter, face_mesh};
        use crate::core::id_photo::{self, IdPhotoOptions};
        use crate::core::portrait::{
            analyze, neck::analyze_necks, render, FaceEdits, GarmentLook, LaidGarment, PhotoLight,
            PortraitSettings,
        };
        use crate::formats::Importer;

        let (Ok(photos), Ok(files), Ok(out)) = (
            std::env::var("IAI_GARMENT_PROBE"),
            std::env::var("IAI_GARMENT_PSD"),
            std::env::var("IAI_GARMENT_OUT"),
        ) else {
            return;
        };
        let number = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<f32>().ok());
        let pick = number("IAI_GARMENT_PICK").map_or(4, |n| n as usize);
        let (matched, neck) = (number("IAI_GARMENT_MATCH"), number("IAI_GARMENT_NECK"));
        let keep_layers = std::env::var("IAI_GARMENT_LAYERS").is_ok();
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
                    let garment =
                        Garment::trimmed(&layer.tiles.flatten(), layer.width, layer.height)?;
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
            let prepared = id_photo::prepare(
                image.as_raw(),
                photo_w,
                photo_h,
                None,
                &options,
                &mut segment,
                &|_| {},
            );
            let plan = match prepared {
                Ok(plan) => plan,
                Err(e) => {
                    println!("{name}: {e}");
                    continue;
                }
            };
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
            let person = Piece {
                rgba: layer.tiles.flatten(),
                width: layer.width,
                height: layer.height,
                offset: layer.offset,
            }
            .on_photo(w, h);
            let matte: Vec<u8> = person.chunks_exact(4).map(|px| px[3]).collect();
            let meshes = face_mesh::detect(&made, w, h).unwrap();
            let Some(mesh) = meshes
                .iter()
                .max_by(|a, b| a.frame().1.total_cmp(&b.frame().1))
            else {
                println!("{name}: no face");
                continue;
            };
            let labels = segmenter.segment_face(&made, w, h, mesh).unwrap();
            let figure = Figure::read(&labels, &matte, w, h);
            let fitting = Fitting {
                photo: (w, h),
                neck: neck_of(&figure, &FaceMarks::from_mesh(mesh)),
            };
            if keep_layers {
                image::RgbaImage::from_raw(w, h, person.clone())
                    .unwrap()
                    .save(out.join(format!("{name}__nguoi.png")))
                    .unwrap();
            }
            // What the retouch reads of the person, for the photo's light
            // and the neck.
            let model = (matched.is_some() || neck.is_some())
                .then(|| analyze(&person, w, h, false, None, &|_| {}).unwrap());
            if let Some(model) = &model {
                println!(
                    "{name}: {photo_w}x{photo_h} -> {w}x{h}; skin {:?}, cast {:?}, veil {:?}",
                    model.light.skin,
                    model.light.cast(),
                    model.light.veil
                );
            }

            let plain: Vec<u8> = options.backdrop.rgb().repeat((w * h) as usize);
            let flat = |layers: &[&[u8]], shade: Option<&[u8]>| {
                let mut flat = plain.clone();
                for layer in layers {
                    over(&mut flat, layer);
                }
                if let Some(shade) = shade {
                    multiply(&mut flat, shade);
                }
                image::RgbImage::from_raw(w, h, flat).unwrap()
            };
            let mut panels = vec![flat(&[&person], None)];
            for (label, garment, collar) in &garments {
                let started = std::time::Instant::now();
                let piece = fitting.lay(garment, collar);
                let took = started.elapsed().as_millis();
                let laid = piece.on_photo(w, h);
                let worn = Garment {
                    rgba: laid.clone(),
                    width: w,
                    height: h,
                };
                let (unit, side) = crate::core::seam::lit_from(&person, &worn);
                let scale = piece.width as f32 / garment.width as f32;
                println!("    {label}: x{scale:.2} in {took} ms; unit {unit:.0}, lit from {side}");
                let mut panel = flat(&[&person, &laid], None);
                panel
                    .save(out.join(format!("{name}__{label}.jpg")))
                    .unwrap();
                if keep_layers {
                    image::RgbaImage::from_raw(piece.width, piece.height, piece.rgba.clone())
                        .unwrap()
                        .save(out.join(format!("{name}__{label}__ao.png")))
                        .unwrap();
                }
                let shade = crate::core::seam::shade(&person, &laid, None, w, h);
                if let Some(shade) = &shade {
                    panel = flat(&[&person, &laid], Some(shade));
                    panel
                        .save(out.join(format!("{name}__{label}__vien.jpg")))
                        .unwrap();
                }
                if let Some(model) = &model {
                    // The garment in the photo's light.
                    let light = PhotoLight::left_by(&model.light, None);
                    let settings = PortraitSettings {
                        neck: neck.unwrap_or(0.0),
                        clothes_match: matched.unwrap_or(0.0),
                        ..PortraitSettings::NEUTRAL
                    };
                    let look = GarmentLook::of(&settings, &light);
                    let relit =
                        LaidGarment::read(&laid, w, h).relit(&laid, w, &look, &PhotoLight::NONE);
                    // The neck, reaching the garment's edge.
                    let enabled = vec![true; model.faces.len()];
                    let mut mended = person.clone();
                    if neck.is_some() {
                        analyze_necks(&person, model, &enabled);
                        let edits: Vec<FaceEdits> = model
                            .faces
                            .iter()
                            .map(|face| {
                                let mut edit = FaceEdits::default();
                                if let Some(Ok(neck)) = face.neck.get() {
                                    let r = face.skin.region();
                                    let cover: Vec<u8> = (r.y..r.y + r.h)
                                        .flat_map(|y| (r.x..r.x + r.w).map(move |x| (x, y)))
                                        .map(|(x, y)| laid[((y * w + x) * 4 + 3) as usize])
                                        .collect();
                                    edit.neck = Some(std::sync::Arc::new(
                                        neck.mask_beside(&face.skin, &cover),
                                    ));
                                }
                                edit
                            })
                            .collect();
                        if let Some((u, px)) = render(&person, model, &settings, &enabled, &edits) {
                            let (uw, ww) = (u.w as usize * 4, w as usize * 4);
                            for y in 0..u.h as usize {
                                let o = (u.y as usize + y) * ww + u.x as usize * 4;
                                mended[o..o + uw].copy_from_slice(&px[y * uw..(y + 1) * uw]);
                            }
                        }
                    }
                    panel = flat(&[&mended, &relit], shade.as_deref());
                    panel
                        .save(out.join(format!("{name}__{label}__xong.jpg")))
                        .unwrap();
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
}

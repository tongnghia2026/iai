//! Finding a card by what is printed on it. A picture of a blank card of
//! each kind is given; the spots of its print are matched in the photo
//! (ORB), the perspective that carries the one onto the other is worked out
//! from the matches that agree, and the card's corners are carried with it,
//! then moved onto the card's edges in the photo. What lies around the card
//! does not matter (a striped cloth, a sleeve, a hand), nor whether the
//! whole of it is in the photo, and the corners come out as the card reads,
//! top-left first, however it lies.

use rayon::prelude::*;

use super::{
    FitLine, Photo, choose, convex, fit_side, fitted, off_line, seen_aspect, shoelace,
    square_enough,
};
use crate::orb::{self, Feature, Gray};

/// The longer side of the copy of a photo its features are found on.
const WORK: usize = 1280;
/// Features looked for in a photo, and in a card's picture.
const PHOTO_FEATURES: usize = 8000;
const PRINT_FEATURES: usize = 1500;
/// How much nearer a match must be than the next best.
const RATIO: f32 = 0.8;
/// Matches that must agree on one perspective for the card to count as found.
const AGREE: usize = 20;
/// How far off (in pixels of the working copy) a match may be and agree.
const TOLERANCE: f64 = 4.0;
/// Left out round the parts of a card's picture that differ card to card,
/// in its pixels: a feature looks this far about it.
const SKIP_MARGIN: f32 = 12.0;

/// A kind of card known by its print.
pub struct CardPrint {
    w: f32,
    h: f32,
    features: Vec<Feature>,
}

impl CardPrint {
    /// `gray` (`width` × `height`, a byte a pixel) is the card lying flat
    /// and upright, edge to edge; `skip` are the rectangles of it
    /// (`[x0, y0, x1, y1]` in its pixels) that differ from card to card (the
    /// holder's photo, name, numbers…), whose spots are not matched.
    pub fn new(gray: Vec<u8>, width: u32, height: u32, skip: &[[f32; 4]]) -> Option<Self> {
        let (w, h) = (width as usize, height as usize);
        if w < 64 || h < 64 || gray.len() < w * h {
            return None;
        }
        let picture = Gray { w, h, px: gray };
        let keep = |x: f32, y: f32| {
            !skip.iter().any(|r| {
                x >= r[0] - SKIP_MARGIN
                    && x <= r[2] + SKIP_MARGIN
                    && y >= r[1] - SKIP_MARGIN
                    && y <= r[3] + SKIP_MARGIN
            })
        };
        let features = orb::features(&picture, PRINT_FEATURES, &keep);
        Some(Self {
            w: width as f32,
            h: height as f32,
            features,
        })
    }

    /// Its corners, top-left round to bottom-left, in its pixels.
    fn corners(&self) -> [(f64, f64); 4] {
        let (w, h) = (self.w as f64, self.h as f64);
        [
            (-0.5, -0.5),
            (w - 0.5, -0.5),
            (w - 0.5, h - 0.5),
            (-0.5, h - 0.5),
        ]
    }

    fn aspect(&self) -> f32 {
        self.w.max(self.h) / self.w.min(self.h)
    }
}

/// A card found by its print.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PrintFound {
    /// In the photo's pixels: top-left, top-right, bottom-right,
    /// bottom-left of the card as it reads.
    pub corners: [(f32, f32); 4],
    /// Which of the prints it is.
    pub print: usize,
    /// How sure, 0 to 1.
    pub score: f32,
}

/// A point of a card's picture and the point of the photo it was matched to.
type Pair = ((f64, f64), (f64, f64));

/// The photo, and its features matched to each print's.
struct Seen<'a> {
    full: Photo<'a>,
    /// Photo pixels per pixel of the working copy.
    scale: f64,
    pairs: Vec<Vec<Pair>>,
}

impl<'a> Seen<'a> {
    fn of(rgba: &'a [u8], width: u32, height: u32, prints: &[CardPrint]) -> Option<Self> {
        let full = Photo::new(rgba, width, height)?;
        let (gray, scale) = Gray::of_rgba(rgba, width, height, WORK)?;
        let found = orb::features(&gray, PHOTO_FEATURES, &|_, _| true);
        let photo = |f: &Feature| {
            (
                ((f.x + 0.5) * scale - 0.5) as f64,
                ((f.y + 0.5) * scale - 0.5) as f64,
            )
        };
        let pairs = prints
            .iter()
            .map(|p| {
                orb::matches(&p.features, &found, RATIO)
                    .into_iter()
                    .map(|(i, j)| {
                        let a = &p.features[i];
                        ((a.x as f64, a.y as f64), photo(&found[j]))
                    })
                    .collect()
            })
            .collect();
        Some(Self {
            full,
            scale: scale as f64,
            pairs,
        })
    }

    /// How many of print `print`'s matches put it at `corners`.
    fn support(
        &self,
        prints: &[CardPrint],
        print: usize,
        corners: &[(f32, f32); 4],
        tol: f64,
    ) -> usize {
        let to = corners.map(|(x, y)| (x as f64, y as f64));
        let Some(h) = Projective::fit(&prints[print].corners(), &to) else {
            return 0;
        };
        agreeing(&h, &self.pairs[print], tol * tol)
    }
}

/// The card in `rgba` (`width` × `height`, 4 bytes a pixel) of one of the
/// kinds `prints` shows; none when no print is seen clearly enough.
pub fn find_by_print(
    rgba: &[u8],
    width: u32,
    height: u32,
    prints: &[CardPrint],
) -> Option<PrintFound> {
    let seen = Seen::of(rgba, width, height, prints)?;
    let tol = TOLERANCE * seen.scale;
    let size = (width as f32, height as f32);
    let (print, h, agree) = seen
        .pairs
        .par_iter()
        .enumerate()
        .filter_map(|(i, pairs)| {
            let (h, n) = ransac(pairs, tol)?;
            let carried = carry(&h, &prints[i])?;
            (n >= AGREE && plausible(&carried, prints[i].aspect(), size)).then_some((i, h, n))
        })
        .max_by_key(|&(_, _, n)| n)?;
    let carried = carry(&h, &prints[print])?;
    let aspect = prints[print].aspect();
    let diagonal = (carried[0].0 - carried[2].0).hypot(carried[0].1 - carried[2].1);
    // The print says which card it is, which way up and roughly where; the
    // photo's edges say where exactly. Cards are not all printed in the
    // same place on the plastic (a back may sit 2 or 3 mm off from another
    // of its kind), and where the print has most to match (on a back, about
    // the chip and the seal) the corners carried out from there may be
    // further off still. So of the shapes the edges make, those the print
    // agrees with to within a few millimetres and of the card's shape are
    // taken, as the edge finder takes from all of them: the clearest, or the
    // card a little way in from its sleeve.
    let wide = (0.03 * diagonal as f64).max(tol);
    let own = seen.support(prints, print, &carried, wide);
    let agreed: Vec<(f32, [(f32, f32); 4])> = fitted(&seen.full, aspect)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(score, mut c)| {
            // Round the same way as the print's corners, then each way up.
            if shoelace(&c) < 0.0 {
                c.reverse();
            }
            let (n, c) = (0..4)
                .map(|turn| {
                    let c = [
                        c[turn],
                        c[(turn + 1) % 4],
                        c[(turn + 2) % 4],
                        c[(turn + 3) % 4],
                    ];
                    (seen.support(prints, print, &c, wide), c)
                })
                .max_by_key(|&(n, _)| n)?;
            (n * 5 >= own * 4 && shaped(&c, aspect, size, 0.04) && plausible(&c, aspect, size))
                .then_some((score, c))
        })
        .collect();
    let corners = match choose(&agreed) {
        Some((_, c)) => c,
        // None of them: the card's own edges looked for where the print
        // puts them, closely.
        None => {
            let c = onto_edges(&seen.full, carried);
            if plausible(&c, aspect, size) {
                c
            } else {
                carried
            }
        }
    };
    Some(PrintFound {
        corners,
        print,
        score: (agree as f32 / 80.0).min(1.0),
    })
}

/// Whether `c`, its slant undone as seen from the middle of a photo of
/// `size`, is within `share` of `aspect`.
fn shaped(c: &[(f32, f32); 4], aspect: f32, size: (f32, f32), share: f32) -> bool {
    let centre = (size.0 / 2.0, size.1 / 2.0);
    (seen_aspect(c, centre) / aspect - 1.0).abs() <= share
}

/// `carried` with each side moved onto the card's edge in the photo where
/// one runs close beside it, along its whole length. A side with no edge
/// there stays where the print puts it: the card against something as pale
/// as itself (a stronger edge a little way off is the cloth's, not the
/// card's), or off the photo.
fn onto_edges(photo: &Photo<'_>, carried: [(f32, f32); 4]) -> [(f32, f32); 4] {
    let diagonal = (carried[0].0 - carried[2].0).hypot(carried[0].1 - carried[2].1);
    let band = (0.012 * diagonal).max(3.0);
    let sides: Vec<FitLine> = (0..4)
        .map(|k| {
            let (a, b) = (carried[k], carried[(k + 1) % 4]);
            let fitted = fit_side(photo, a, b, 1.5 * band);
            if off_line(&fitted, a) <= band && off_line(&fitted, b) <= band {
                fitted
            } else {
                let length = (b.0 - a.0).hypot(b.1 - a.1).max(1e-6);
                FitLine {
                    point: a,
                    dir: ((b.0 - a.0) / length, (b.1 - a.1) / length),
                }
            }
        })
        .collect();
    let mut corners = carried;
    for (k, corner) in corners.iter_mut().enumerate() {
        if let Some(p) = sides[(k + 3) % 4].meet(&sides[k])
            && (p.0 - corner.0).hypot(p.1 - corner.1) <= 2.0 * band
        {
            *corner = p;
        }
    }
    corners
}

/// `corners` (a card already found, its sides on its edges, in any order)
/// put in the order the card reads, when its print is seen.
pub fn upright(
    rgba: &[u8],
    width: u32,
    height: u32,
    corners: [(f32, f32); 4],
    prints: &[CardPrint],
) -> Option<PrintFound> {
    let seen = Seen::of(rgba, width, height, prints)?;
    let mut corners = corners;
    if shoelace(&corners) < 0.0 {
        corners.reverse();
    }
    let diagonal = (corners[0].0 - corners[2].0).hypot(corners[0].1 - corners[2].1);
    // As loosely as the print may sit on the plastic: only which way up is
    // asked, and the wrong ways are far off.
    let wide = (0.03 * diagonal as f64).max(TOLERANCE * seen.scale);
    let mut best: Option<(usize, usize, [(f32, f32); 4])> = None;
    for print in 0..prints.len() {
        for turn in 0..4 {
            let c = [
                corners[turn],
                corners[(turn + 1) % 4],
                corners[(turn + 2) % 4],
                corners[(turn + 3) % 4],
            ];
            let n = seen.support(prints, print, &c, wide);
            if best.is_none_or(|(most, _, _)| n > most) {
                best = Some((n, print, c));
            }
        }
    }
    let (agree, print, corners) = best?;
    (agree >= AGREE).then_some(PrintFound {
        corners,
        print,
        score: (agree as f32 / 80.0).min(1.0),
    })
}

/// The print's corners carried into the photo by `h`.
fn carry(h: &Projective, print: &CardPrint) -> Option<[(f32, f32); 4]> {
    let c = print.corners();
    let mut out = [(0.0f32, 0.0f32); 4];
    for (o, p) in out.iter_mut().zip(c) {
        let (x, y) = h.map(p)?;
        *o = (x as f32, y as f32);
    }
    Some(out)
}

/// Whether `c` could be a card of shape `aspect` in a photo of `size`: the
/// right way round (not mirrored), square enough at the corners, of about
/// the card's shape once its slant is undone, and not a speck.
fn plausible(c: &[(f32, f32); 4], aspect: f32, size: (f32, f32)) -> bool {
    shoelace(c) > 0.0
        && convex(c)
        && square_enough(c)
        && shaped(c, aspect, size, 0.15)
        && shoelace(c).abs() / 2.0 >= 0.01 * size.0 * size.1
}

/// How many of `pairs` `h` carries to within `tol2` (squared) of their match.
fn agreeing(h: &Projective, pairs: &[Pair], tol2: f64) -> usize {
    pairs
        .iter()
        .filter(|(a, b)| {
            h.map(*a)
                .is_some_and(|m| (m.0 - b.0).powi(2) + (m.1 - b.1).powi(2) <= tol2)
        })
        .count()
}

/// The perspective most of `pairs` agree on, to within `tol` pixels, and how
/// many do (Fischler and Bolles' RANSAC, refitted on those that agree).
fn ransac(pairs: &[Pair], tol: f64) -> Option<(Projective, usize)> {
    if pairs.len() < 8 {
        return None;
    }
    let tol2 = tol * tol;
    let mut state = 0x9e37_79b9_7f4a_7c15u64 ^ pairs.len() as u64;
    let mut next = move |n: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % n as u64) as usize
    };
    let mut best: Option<(Projective, usize)> = None;
    let mut rounds = 2000usize;
    let mut round = 0;
    while round < rounds {
        round += 1;
        let mut pick = [0usize; 4];
        for k in 0..4 {
            let mut i = next(pairs.len());
            while pick[..k].contains(&i) {
                i = next(pairs.len());
            }
            pick[k] = i;
        }
        let from = pick.map(|i| pairs[i].0);
        let to = pick.map(|i| pairs[i].1);
        if !spread(&from) || !spread(&to) {
            continue;
        }
        let Some(h) = Projective::fit(&from, &to) else {
            continue;
        };
        let n = agreeing(&h, pairs, tol2);
        if best.as_ref().is_none_or(|b| n > b.1) {
            best = Some((h, n));
            // Rounds enough to draw four that agree, 99 times in 100.
            let share = n as f64 / pairs.len() as f64;
            let miss = 1.0 - share.powi(4);
            if miss <= 0.0 {
                break;
            }
            let wanted = (0.01f64.ln() / miss.ln()).ceil();
            if wanted.is_finite() {
                rounds = rounds.min(wanted.max(50.0) as usize);
            }
        }
    }
    let (mut h, mut n) = best?;
    for _ in 0..3 {
        let (from, to): (Vec<_>, Vec<_>) = pairs
            .iter()
            .filter(|(a, b)| {
                h.map(*a)
                    .is_some_and(|m| (m.0 - b.0).powi(2) + (m.1 - b.1).powi(2) <= tol2)
            })
            .copied()
            .unzip();
        let Some(refit) = Projective::fit(&from, &to) else {
            break;
        };
        let m = agreeing(&refit, pairs, tol2);
        if m < n {
            break;
        }
        (h, n) = (refit, m);
    }
    Some((h, n))
}

/// Whether no three of four points are nearly in a line.
fn spread(p: &[(f64, f64); 4]) -> bool {
    let twice = |a: (f64, f64), b: (f64, f64), c: (f64, f64)| {
        ((b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)).abs()
    };
    let span = (0..4)
        .flat_map(|i| (0..4).map(move |j| (i, j)))
        .map(|(i, j)| (p[i].0 - p[j].0).hypot(p[i].1 - p[j].1))
        .fold(0.0, f64::max);
    let least = span * span * 0.01;
    twice(p[0], p[1], p[2]) > least
        && twice(p[0], p[1], p[3]) > least
        && twice(p[0], p[2], p[3]) > least
        && twice(p[1], p[2], p[3]) > least
}

/// A perspective map of the plane, row by row, its last entry 1.
#[derive(Clone, Copy, Debug)]
struct Projective([f64; 9]);

impl Projective {
    /// The map taking each of `from` nearest to the one of `to` beside it:
    /// least squares, exact for four (Hartley's normalised direct linear
    /// transform).
    fn fit(from: &[(f64, f64)], to: &[(f64, f64)]) -> Option<Self> {
        if from.len() < 4 || from.len() != to.len() {
            return None;
        }
        let (a, fa) = Normal::of(from)?;
        let (b, fb) = Normal::of(to)?;
        let mut m = [[0.0f64; 9]; 8];
        for (p, q) in fa.iter().zip(&fb) {
            let (x, y, u, v) = (p.0, p.1, q.0, q.1);
            let rows = [
                ([x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y], u),
                ([0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y], v),
            ];
            for (r, rhs) in rows {
                for i in 0..8 {
                    for j in 0..8 {
                        m[i][j] += r[i] * r[j];
                    }
                    m[i][8] += r[i] * rhs;
                }
            }
        }
        let h = solve(m)?;
        let n = [h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], 1.0];
        // Undo the normalising: b⁻¹ · n · a.
        let full = mul(&mul(&b.inverse(), &n), &a.forward());
        let last = full[8];
        if !last.is_finite() || last.abs() < 1e-12 {
            return None;
        }
        Some(Self(full.map(|v| v / last)))
    }

    fn map(&self, (x, y): (f64, f64)) -> Option<(f64, f64)> {
        let m = &self.0;
        let w = m[6] * x + m[7] * y + m[8];
        if w.abs() < 1e-12 {
            return None;
        }
        Some((
            (m[0] * x + m[1] * y + m[2]) / w,
            (m[3] * x + m[4] * y + m[5]) / w,
        ))
    }
}

/// Points moved to have their middle at 0 and lie √2 from it on average.
struct Normal {
    cx: f64,
    cy: f64,
    s: f64,
}

impl Normal {
    fn of(points: &[(f64, f64)]) -> Option<(Self, Vec<(f64, f64)>)> {
        let n = points.len() as f64;
        let (cx, cy) = points
            .iter()
            .fold((0.0, 0.0), |(x, y), p| (x + p.0 / n, y + p.1 / n));
        let mean = points
            .iter()
            .map(|p| (p.0 - cx).hypot(p.1 - cy))
            .sum::<f64>()
            / n;
        if mean.is_nan() || mean <= 1e-9 {
            return None;
        }
        let s = std::f64::consts::SQRT_2 / mean;
        let moved = points
            .iter()
            .map(|p| ((p.0 - cx) * s, (p.1 - cy) * s))
            .collect();
        Some((Self { cx, cy, s }, moved))
    }

    fn forward(&self) -> [f64; 9] {
        [
            self.s,
            0.0,
            -self.s * self.cx,
            0.0,
            self.s,
            -self.s * self.cy,
            0.0,
            0.0,
            1.0,
        ]
    }

    fn inverse(&self) -> [f64; 9] {
        [
            1.0 / self.s,
            0.0,
            self.cx,
            0.0,
            1.0 / self.s,
            self.cy,
            0.0,
            0.0,
            1.0,
        ]
    }
}

fn mul(a: &[f64; 9], b: &[f64; 9]) -> [f64; 9] {
    let mut out = [0.0; 9];
    for r in 0..3 {
        for c in 0..3 {
            out[r * 3 + c] = (0..3).map(|k| a[r * 3 + k] * b[k * 3 + c]).sum();
        }
    }
    out
}

/// The 8 unknowns of `m` (8 equations, the right-hand side last), by
/// elimination with the largest pivot.
fn solve(mut m: [[f64; 9]; 8]) -> Option<[f64; 8]> {
    for col in 0..8 {
        let pivot = (col..8).max_by(|&a, &b| m[a][col].abs().total_cmp(&m[b][col].abs()))?;
        if m[pivot][col].abs() < 1e-12 {
            return None;
        }
        m.swap(col, pivot);
        for row in 0..8 {
            if row != col {
                let f = m[row][col] / m[col][col];
                if f != 0.0 {
                    for k in col..9 {
                        m[row][k] -= f * m[col][k];
                    }
                }
            }
        }
    }
    let mut out = [0.0; 8];
    for (i, v) in out.iter_mut().enumerate() {
        *v = m[i][8] / m[i][i];
        if !v.is_finite() {
            return None;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests;

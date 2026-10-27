//! Finding a card in a photo: the four sides of an ID card (or any
//! rectangle of a known shape) photographed at a slant, for a perspective
//! crop. Straight edges are found on a small copy of the photo, the
//! quadrilateral that looks most like the card is chosen, and its sides are
//! then fitted to the edges of the full photo.

use rayon::prelude::*;

/// The long side of an ID card over its short side (85.6 × 53.98 mm).
pub const ID_CARD_ASPECT: f32 = 85.6 / 53.98;

/// A card found: its corners in the photo's pixels, top-left, top-right,
/// bottom-right, bottom-left (its long sides are the top and the bottom),
/// and how much it looks like one, 0 to 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Found {
    pub corners: [(f32, f32); 4],
    pub score: f32,
}

/// The longer side of the copy lines are looked for on.
const WORK_EDGE: usize = 512;
/// Lines kept from the vote, and of them, at most so many within 10° of
/// one another.
const MAX_LINES: usize = 40;
const PER_DIRECTION: usize = 6;
/// Sides of a quadrilateral that may be opposite: no further from parallel.
const MAX_SKEW_DEG: f32 = 30.0;
/// Candidates fitted to the full photo before one is chosen.
const LEADING: usize = 16;

/// The card in `rgba` (`width` × `height`, 4 bytes a pixel) whose long side
/// is `aspect` times its short side; none when nothing card-like is there.
pub fn find_card(rgba: &[u8], width: u32, height: u32, aspect: f32) -> Option<Found> {
    let full = Photo::new(rgba, width, height)?;
    let small = Small::of(&full)?;
    let edges = Edges::of(&small);
    let quads = scored_quads(&edges, &edges.lines(), aspect);
    let scale = small.scale;
    let reach = 2.5 * scale + 3.0;
    let centre = (full.w as f32 / 2.0, full.h as f32 / 2.0);
    // The leading few fitted to the full photo, where the shape is measured
    // closely enough to tell a card from its sleeve or from a line printed
    // across it.
    let mut fitted: Vec<(f32, [(f32, f32); 4])> = Vec::new();
    let mut tried: Vec<[(f32, f32); 4]> = Vec::new();
    for q in &quads {
        if tried.len() >= LEADING {
            break;
        }
        if tried.iter().any(|t| same_corners(t, &q.corners, 3.0)) {
            continue;
        }
        tried.push(q.corners);
        let rough = q
            .corners
            .map(|(x, y)| ((x + 0.5) * scale - 0.5, (y + 0.5) * scale - 0.5));
        let corners = fit_sides(&full, rough, reach);
        let shape = (seen_aspect(&corners, centre) / aspect).ln() / 0.06;
        fitted.push((q.base * (-shape * shape).exp(), corners));
    }
    fitted.sort_by(|a, b| b.0.total_cmp(&a.0));
    let &(score, outer) = fitted.first()?;
    // A card in a clear sleeve: the sleeve's rim is as clear and of much the
    // same shape; the card is the one a little way in from it, on two sides
    // at least (where the card lies against the sleeve, the two share a side).
    let inner = fitted
        .iter()
        .skip(1)
        .find(|(s, c)| *s >= score * 0.75 && sleeved(&outer, c));
    let (score, corners) = *inner.unwrap_or(&(score, outer));
    (score >= 0.08).then(|| Found {
        corners: order(corners),
        score: score.clamp(0.0, 1.0),
    })
}

/// Whether `inner` lies in `outer` as a card in its sleeve: each side of it
/// on a side of `outer` or a little way in from one, at least two of them in.
fn sleeved(outer: &[(f32, f32); 4], inner: &[(f32, f32); 4]) -> bool {
    let len = |c: &[(f32, f32); 4], i: usize| {
        let (a, b) = (c[i], c[(i + 1) % 4]);
        (b.0 - a.0).hypot(b.1 - a.1)
    };
    let short = (0..4).map(|i| len(outer, i)).fold(f32::MAX, f32::min);
    let mut moved_in = 0;
    for i in 0..4 {
        let (a, b) = (inner[i], inner[(i + 1) % 4]);
        let mid = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
        // How far in from `outer`'s sides, the nearest first; out is below 0.
        let mut gaps: Vec<f32> = (0..4)
            .map(|k| {
                let (p, q) = (outer[k], outer[(k + 1) % 4]);
                let cross = (q.0 - p.0) * (mid.1 - p.1) - (q.1 - p.1) * (mid.0 - p.0);
                let sign = if shoelace(outer) >= 0.0 { 1.0 } else { -1.0 };
                sign * cross / len(outer, k).max(1e-6)
            })
            .collect();
        gaps.sort_by(|x, y| x.abs().total_cmp(&y.abs()));
        let gap = gaps[0] / short;
        if !(-0.01..=0.09).contains(&gap) {
            return false;
        }
        moved_in += usize::from(gap > 0.01);
    }
    moved_in >= 2
}

/// `rough` corners (in any order, e.g. read off a preview)
/// moved onto the card's edges, ordered as [`Found::corners`].
pub fn snap_corners(
    rgba: &[u8],
    width: u32,
    height: u32,
    rough: [(f32, f32); 4],
) -> [(f32, f32); 4] {
    // Any order: round their middle first.
    let (mx, my) = rough
        .iter()
        .fold((0.0, 0.0), |(x, y), p| (x + p.0 / 4.0, y + p.1 / 4.0));
    let mut rough = rough;
    rough.sort_by(|a, b| {
        (a.1 - my)
            .atan2(a.0 - mx)
            .total_cmp(&(b.1 - my).atan2(b.0 - mx))
    });
    let Some(full) = Photo::new(rgba, width, height) else {
        return order(rough);
    };
    let around = order(rough);
    let diagonal = (width as f32).hypot(height as f32);
    order(fit_sides(&full, around, (0.025 * diagonal).max(6.0)))
}

/// The photo, read as it is.
struct Photo<'a> {
    rgba: &'a [u8],
    w: usize,
    h: usize,
}

impl<'a> Photo<'a> {
    fn new(rgba: &'a [u8], width: u32, height: u32) -> Option<Self> {
        let (w, h) = (width as usize, height as usize);
        (w >= 32 && h >= 32 && rgba.len() >= w * h * 4).then_some(Self { rgba, w, h })
    }

    fn px(&self, x: usize, y: usize) -> [f32; 3] {
        let o = (y.min(self.h - 1) * self.w + x.min(self.w - 1)) * 4;
        match self.rgba.get(o..o + 3) {
            Some(p) => [p[0] as f32, p[1] as f32, p[2] as f32],
            None => [0.0; 3],
        }
    }

    /// The colour at `(x, y)`, between pixel centres.
    fn at(&self, x: f32, y: f32) -> [f32; 3] {
        let x = x.clamp(0.0, (self.w - 1) as f32);
        let y = y.clamp(0.0, (self.h - 1) as f32);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let (a, b) = (self.px(x0, y0), self.px(x0 + 1, y0));
        let (c, d) = (self.px(x0, y0 + 1), self.px(x0 + 1, y0 + 1));
        let mut out = [0.0; 3];
        for (i, v) in out.iter_mut().enumerate() {
            let top = a[i] + (b[i] - a[i]) * fx;
            let bottom = c[i] + (d[i] - c[i]) * fx;
            *v = top + (bottom - top) * fy;
        }
        out
    }
}

/// A box-filtered copy of the photo, its longer side [`WORK_EDGE`] at most.
struct Small {
    w: usize,
    h: usize,
    rgb: Vec<[f32; 3]>,
    /// Photo pixels per copy pixel.
    scale: f32,
}

impl Small {
    fn of(photo: &Photo<'_>) -> Option<Self> {
        let longest = photo.w.max(photo.h);
        let scale = (longest as f32 / WORK_EDGE as f32).max(1.0);
        let w = ((photo.w as f32 / scale).round() as usize).max(1);
        let h = ((photo.h as f32 / scale).round() as usize).max(1);
        if w < 24 || h < 24 {
            return None;
        }
        let rgb = (0..h)
            .into_par_iter()
            .flat_map_iter(|y| {
                let y0 = ((y as f32 * scale) as usize).min(photo.h - 1);
                let y1 = (((y + 1) as f32 * scale) as usize).clamp(y0 + 1, photo.h);
                (0..w).map(move |x| {
                    let x0 = ((x as f32 * scale) as usize).min(photo.w - 1);
                    let x1 = (((x + 1) as f32 * scale) as usize).clamp(x0 + 1, photo.w);
                    let mut sum = [0.0f32; 3];
                    for yy in y0..y1 {
                        for xx in x0..x1 {
                            let p = photo.px(xx, yy);
                            sum[0] += p[0];
                            sum[1] += p[1];
                            sum[2] += p[2];
                        }
                    }
                    let n = ((y1 - y0) * (x1 - x0)) as f32;
                    [sum[0] / n, sum[1] / n, sum[2] / n]
                })
            })
            .collect();
        Some(Self { w, h, rgb, scale })
    }
}

/// How strongly the colour changes at each pixel of the copy, and across
/// which direction.
struct Edges {
    w: usize,
    h: usize,
    rgb: Vec<[f32; 3]>,
    /// Of the colour channel that changes most.
    magnitude: Vec<f32>,
    /// The direction of that change (across the edge), in radians.
    angle: Vec<f32>,
    /// What counts as a clear edge.
    strong: f32,
}

impl Edges {
    fn of(small: &Small) -> Self {
        let (w, h) = (small.w, small.h);
        let at = |x: usize, y: usize| small.rgb[y * w + x];
        let cells: Vec<(f32, f32)> = (0..h)
            .into_par_iter()
            .flat_map_iter(|y| {
                (0..w).map(move |x| {
                    if x == 0 || y == 0 || x + 1 >= w || y + 1 >= h {
                        return (0.0, 0.0);
                    }
                    let mut best = (0.0f32, 0.0f32, 0.0f32);
                    for c in 0..3 {
                        let p = |dx: isize, dy: isize| {
                            at((x as isize + dx) as usize, (y as isize + dy) as usize)[c]
                        };
                        let gx = (p(1, -1) + 2.0 * p(1, 0) + p(1, 1))
                            - (p(-1, -1) + 2.0 * p(-1, 0) + p(-1, 1));
                        let gy = (p(-1, 1) + 2.0 * p(0, 1) + p(1, 1))
                            - (p(-1, -1) + 2.0 * p(0, -1) + p(1, -1));
                        let m = gx.hypot(gy);
                        if m > best.0 {
                            best = (m, gx, gy);
                        }
                    }
                    (best.0, best.2.atan2(best.1))
                })
            })
            .collect();
        let (magnitude, angle): (Vec<f32>, Vec<f32>) = cells.into_iter().unzip();
        // A card's edge: some 40 levels of contrast at least.
        let strong = percentile(&magnitude, 0.97).max(160.0);
        Self {
            w,
            h,
            rgb: small.rgb.clone(),
            magnitude,
            angle,
            strong,
        }
    }

    /// The straight lines the strongest edges lie on.
    fn lines(&self) -> Vec<Line> {
        const THETAS: usize = 180;
        let diagonal = (self.w as f32).hypot(self.h as f32).ceil() as usize;
        let rhos = 2 * diagonal + 1;
        let vote_from = percentile(&self.magnitude, 0.80).max(self.strong * 0.2);
        let (cos, sin): (Vec<f32>, Vec<f32>) = (0..THETAS)
            .map(|t| (t as f32).to_radians())
            .map(|a| (a.cos(), a.sin()))
            .unzip();
        let mut votes = vec![0.0f32; THETAS * rhos];
        for y in 1..self.h.saturating_sub(1) {
            for x in 1..self.w.saturating_sub(1) {
                let i = y * self.w + x;
                let m = self.magnitude[i];
                if m < vote_from {
                    continue;
                }
                let normal = self.angle[i].to_degrees().rem_euclid(180.0);
                let centre = normal.round() as isize;
                for dt in -6..=6isize {
                    let t = (centre + dt).rem_euclid(THETAS as isize) as usize;
                    let rho = x as f32 * cos[t] + y as f32 * sin[t];
                    let r = (rho.round() as isize + diagonal as isize) as usize;
                    if let Some(v) = votes.get_mut(t * rhos + r) {
                        *v += m.min(self.strong * 2.0);
                    }
                }
            }
        }
        let mut peaks: Vec<(f32, usize, usize)> = Vec::new();
        for t in 0..THETAS {
            for r in 1..rhos - 1 {
                let v = votes[t * rhos + r];
                if v <= 0.0 {
                    continue;
                }
                let around = [
                    votes[t * rhos + r - 1],
                    votes[t * rhos + r + 1],
                    votes[((t + 1) % THETAS) * rhos + r],
                    votes[((t + THETAS - 1) % THETAS) * rhos + r],
                ];
                if around.iter().all(|&a| v >= a) {
                    peaks.push((v, t, r));
                }
            }
        }
        peaks.sort_by(|a, b| b.0.total_cmp(&a.0));
        let top = peaks.first().map_or(0.0, |p| p.0);
        // A few lines of each direction: the many lines of print on a card
        // run one way and would crowd out its short sides.
        let mut per_direction = [0usize; THETAS / 10];
        let mut lines: Vec<Line> = Vec::new();
        for (v, t, r) in peaks {
            if v < top * 0.04 || lines.len() >= MAX_LINES {
                break;
            }
            let line = Line {
                theta: (t as f32).to_radians(),
                rho: r as f32 - diagonal as f32,
            };
            let Some(taken) = per_direction.get_mut(t / 10) else {
                continue;
            };
            if *taken < PER_DIRECTION && lines.iter().all(|l| !l.same_as(&line)) {
                *taken += 1;
                lines.push(line);
            }
        }
        lines
    }

    /// How well the segment `a`–`b` lies on an edge: 0 to 1, and the share
    /// of it that does.
    fn support(&self, a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let length = dx.hypot(dy);
        if length < 4.0 {
            return (0.0, 0.0);
        }
        let normal = (-dy / length, dx / length);
        let samples = ((length * 0.8 / 1.5) as usize).max(8);
        let (mut sum, mut held) = (0.0f32, 0usize);
        for i in 0..samples {
            let t = 0.1 + 0.8 * (i as f32 + 0.5) / samples as f32;
            let (px, py) = (a.0 + dx * t, a.1 + dy * t);
            let mut best = 0.0f32;
            for k in -1..=1 {
                let x = (px + normal.0 * k as f32).round();
                let y = (py + normal.1 * k as f32).round();
                if x < 0.0 || y < 0.0 || x >= self.w as f32 || y >= self.h as f32 {
                    continue;
                }
                let i = y as usize * self.w + x as usize;
                let along = (self.angle[i].cos() * normal.0 + self.angle[i].sin() * normal.1).abs();
                if along > 0.9 {
                    best = best.max(self.magnitude[i] * along);
                }
            }
            let s = (best / self.strong).min(1.0);
            sum += s;
            held += usize::from(s >= 0.3);
        }
        (sum / samples as f32, held as f32 / samples as f32)
    }
}

impl Edges {
    /// How different the photo is on the two sides of the segment `a`–`b`,
    /// a little way off it: 0 (alike) to 1 (black against white).
    fn contrast(&self, a: (f32, f32), b: (f32, f32)) -> f32 {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let length = dx.hypot(dy);
        if length < 4.0 {
            return 0.0;
        }
        let normal = (-dy / length, dx / length);
        let colour = |x: f32, y: f32| {
            let (x, y) = (x.round(), y.round());
            if x < 0.0 || y < 0.0 || x >= self.w as f32 || y >= self.h as f32 {
                return None;
            }
            self.rgb.get(y as usize * self.w + x as usize).copied()
        };
        let samples = ((length * 0.8 / 2.0) as usize).max(8);
        let (mut sum, mut n) = (0.0f32, 0usize);
        for i in 0..samples {
            let t = 0.1 + 0.8 * (i as f32 + 0.5) / samples as f32;
            let (px, py) = (a.0 + dx * t, a.1 + dy * t);
            let one = colour(px + normal.0 * 3.0, py + normal.1 * 3.0);
            let other = colour(px - normal.0 * 3.0, py - normal.1 * 3.0);
            if let (Some(p), Some(q)) = (one, other) {
                let d =
                    ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt();
                sum += (d / 255.0).min(1.0);
                n += 1;
            }
        }
        if n == 0 { 0.0 } else { sum / n as f32 }
    }
}

/// The value below which `share` of `values` lie.
fn percentile(values: &[f32], share: f32) -> f32 {
    let mut sorted: Vec<f32> = values.iter().copied().filter(|v| *v > 0.0).collect();
    if sorted.is_empty() {
        return 0.0;
    }
    let k = ((sorted.len() - 1) as f32 * share) as usize;
    let (_, v, _) = sorted.select_nth_unstable_by(k, f32::total_cmp);
    *v
}

/// A line `x cos θ + y sin θ = ρ`.
#[derive(Clone, Copy, Debug)]
struct Line {
    theta: f32,
    rho: f32,
}

impl Line {
    /// The angle between two lines, 0 to 90°.
    fn skew(&self, other: &Line) -> f32 {
        let d = (self.theta - other.theta).abs().to_degrees() % 180.0;
        d.min(180.0 - d)
    }

    fn same_as(&self, other: &Line) -> bool {
        let skew = self.skew(other);
        let flipped = (self.theta - other.theta).abs().to_degrees() > 90.0;
        let rho = if flipped { -other.rho } else { other.rho };
        skew <= 5.0 && (self.rho - rho).abs() <= 8.0
    }

    fn meet(&self, other: &Line) -> Option<(f32, f32)> {
        let (c1, s1) = (self.theta.cos(), self.theta.sin());
        let (c2, s2) = (other.theta.cos(), other.theta.sin());
        let det = c1 * s2 - s1 * c2;
        if det.abs() < 0.17 {
            return None;
        }
        Some((
            (self.rho * s2 - other.rho * s1) / det,
            (c1 * other.rho - c2 * self.rho) / det,
        ))
    }
}

/// A quadrilateral on the copy and how card-like it is.
#[derive(Clone, Copy, Debug)]
struct Quad {
    corners: [(f32, f32); 4],
    /// Leaving its shape aside.
    base: f32,
    /// With its shape as the copy shows it.
    score: f32,
    /// Its third side (corners 2 to 3) was not seen but drawn where the
    /// card's shape puts it: the card runs off the photo, or into something
    /// as pale as itself.
    drawn: bool,
}

/// A card with one side not seen counts for this much of one seen whole.
const DRAWN: f32 = 0.75;

/// Every quadrilateral the lines make that could be a card, the most
/// card-like first.
fn scored_quads(edges: &Edges, lines: &[Line], aspect: f32) -> Vec<Quad> {
    let (w, h) = (edges.w as f32, edges.h as f32);
    let short = w.min(h);
    let centre = (w / 2.0, h / 2.0);
    let mut pairs = Vec::new();
    for (i, a) in lines.iter().enumerate() {
        for b in lines.iter().skip(i + 1) {
            if a.skew(b) > MAX_SKEW_DEG {
                continue;
            }
            // Apart by more than a sliver of the photo, near its middle.
            let off = distance(a, centre);
            let foot = (
                centre.0 - off * a.theta.cos(),
                centre.1 - off * a.theta.sin(),
            );
            if distance(b, foot).abs() >= 0.1 * short {
                pairs.push((*a, *b));
            }
        }
    }
    let fits = |corners: &[(f32, f32); 4]| {
        let margin = 0.03 * short;
        let inside = corners
            .iter()
            .all(|&(x, y)| x >= -margin && y >= -margin && x <= w + margin && y <= h + margin);
        let share = shoelace(corners).abs() / 2.0 / (w * h);
        inside && convex(corners) && square_enough(corners) && (0.03..=0.97).contains(&share)
    };
    let quad = |corners, drawn| Quad {
        corners,
        base: 0.0,
        score: 0.0,
        drawn,
    };
    let mut quads: Vec<Quad> = Vec::new();
    for (i, &(a, b)) in pairs.iter().enumerate() {
        for &(c, d) in pairs.iter().skip(i + 1) {
            let cross = a.skew(&c).min(a.skew(&d)).min(b.skew(&c)).min(b.skew(&d));
            if cross < 50.0 {
                continue;
            }
            if let (Some(p), Some(q), Some(r), Some(s)) =
                (a.meet(&c), c.meet(&b), b.meet(&d), d.meet(&a))
            {
                let corners = [p, q, r, s];
                if fits(&corners) {
                    quads.push(quad(corners, false));
                }
            }
        }
        // Three sides seen: the fourth drawn opposite the third.
        for c in lines {
            if a.skew(c).min(b.skew(c)) < 50.0 {
                continue;
            }
            for corners in drawn_fourth(&a, &b, c, aspect, centre) {
                if fits(&corners) {
                    quads.push(quad(corners, true));
                }
            }
        }
    }
    quads.par_iter_mut().for_each(|q| {
        let seen = if q.drawn { 3 } else { 4 };
        let (mut sum, mut least, mut contrast, mut onward) = (0.0, 1.0f32, 0.0, 0.0);
        for k in [0, 1, 3, 2].into_iter().take(seen) {
            let (a, b) = (q.corners[k], q.corners[(k + 1) % 4]);
            let (s, held) = edges.support(a, b);
            sum += s;
            least = least.min(held);
            contrast += edges.contrast(a, b);
            // A side that runs on past its corners is part of something
            // larger: the card, when this is a piece of it.
            let past = |from: (f32, f32), to: (f32, f32)| {
                let (dx, dy) = (to.0 - from.0, to.1 - from.1);
                let start = (to.0 + dx * 0.03, to.1 + dy * 0.03);
                let end = (to.0 + dx * 0.25, to.1 + dy * 0.25);
                edges.support(start, end).0
            };
            onward += past(a, b).max(past(b, a));
        }
        let n = seen as f32;
        let share = shoelace(&q.corners).abs() / 2.0 / (w * h);
        // Clear edges all round that end at the corners, a card that stands
        // out from what it lies on (a sleeve's rim stands out less), and not
        // a speck.
        q.base = sum / n
            * (0.4 + 0.6 * least)
            * (1.0 - onward / n)
            * share.powf(0.15)
            * (0.4 + contrast / n).min(1.0)
            * if q.drawn { DRAWN } else { 1.0 };
        let shape = (seen_aspect(&q.corners, centre) / aspect).ln() / 0.2;
        q.score = q.base * (-shape * shape).exp();
    });
    quads.sort_by(|a, b| b.score.total_cmp(&a.score));
    quads
}

/// The quadrilaterals with sides `c`, `a`, `b` and a fourth side across `a`
/// and `b`, parallel to `c`, placed where the whole has the card's shape:
/// one with `c` a long side, one with `c` a short one, on either side of
/// `c`. Corners go `c`'s two, then along `b`, the drawn side, along `a`.
fn drawn_fourth(
    a: &Line,
    b: &Line,
    c: &Line,
    aspect: f32,
    centre: (f32, f32),
) -> Vec<[(f32, f32); 4]> {
    let (Some(p1), Some(p2)) = (a.meet(c), b.meet(c)) else {
        return Vec::new();
    };
    let across = (p2.0 - p1.0).hypot(p2.1 - p1.1);
    if across < 8.0 {
        return Vec::new();
    }
    let normal = (c.theta.cos(), c.theta.sin());
    let mut out = Vec::new();
    for sign in [1.0f32, -1.0] {
        let ua = (-a.theta.sin() * sign, a.theta.cos() * sign);
        let mut best: [(f32, Option<[(f32, f32); 4]>); 2] = [(f32::MAX, None), (f32::MAX, None)];
        for step in 0..48 {
            let length = across * 0.35 * (2.6f32 / 0.35).powf(step as f32 / 47.0);
            let q1 = (p1.0 + ua.0 * length, p1.1 + ua.1 * length);
            let d = Line {
                theta: c.theta,
                rho: q1.0 * normal.0 + q1.1 * normal.1,
            };
            let Some(q2) = b.meet(&d) else { continue };
            let corners = [p1, p2, q2, q1];
            let off = (seen_aspect(&corners, centre) / aspect).ln().abs();
            let slot = usize::from(length > across);
            if let Some(kept) = best.get_mut(slot)
                && off < kept.0
            {
                *kept = (off, Some(corners));
            }
        }
        for (off, corners) in best {
            if let (true, Some(corners)) = (off < 0.06, corners) {
                out.push(corners);
            }
        }
    }
    out
}

/// Whether `one` and `other` have their corners in the same places.
fn same_corners(one: &[(f32, f32); 4], other: &[(f32, f32); 4], within: f32) -> bool {
    one.iter()
        .all(|p| other.iter().any(|q| (p.0 - q.0).hypot(p.1 - q.1) <= within))
}

fn distance(line: &Line, (x, y): (f32, f32)) -> f32 {
    x * line.theta.cos() + y * line.theta.sin() - line.rho
}

fn shoelace(c: &[(f32, f32); 4]) -> f32 {
    (0..4)
        .map(|i| {
            let (a, b) = (c[i], c[(i + 1) % 4]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum()
}

fn convex(c: &[(f32, f32); 4]) -> bool {
    let turn = |i: usize| {
        let (a, b, d) = (c[i], c[(i + 1) % 4], c[(i + 2) % 4]);
        (b.0 - a.0) * (d.1 - b.1) - (b.1 - a.1) * (d.0 - b.0)
    };
    let turns = [turn(0), turn(1), turn(2), turn(3)];
    turns.iter().all(|&t| t > 0.0) || turns.iter().all(|&t| t < 0.0)
}

/// Every corner between 50° and 130°: a card seen at a slant, not a sliver.
fn square_enough(c: &[(f32, f32); 4]) -> bool {
    (0..4).all(|i| {
        let (p, q, r) = (c[(i + 3) % 4], c[i], c[(i + 1) % 4]);
        let (ux, uy) = (p.0 - q.0, p.1 - q.1);
        let (vx, vy) = (r.0 - q.0, r.1 - q.1);
        let cos = (ux * vx + uy * vy) / (ux.hypot(uy) * vx.hypot(vy)).max(1e-6);
        cos.abs() <= 0.643
    })
}

/// The long side over the short one of the rectangle that, photographed
/// by a camera looking through `centre`, shows as `c`: its slant undone
/// (Zhang and He, "Whiteboard scanning and image enhancement", 2007).
fn seen_aspect(c: &[(f32, f32); 4], centre: (f32, f32)) -> f32 {
    let at = |i: usize| {
        let (x, y) = c[i];
        [(x - centre.0) as f64, (y - centre.1) as f64, 1.0]
    };
    // m1 top-left, m2 top-right, m3 bottom-left, m4 bottom-right.
    let (m1, m2, m3, m4) = (at(0), at(1), at(3), at(2));
    let cross = |a: [f64; 3], b: [f64; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let k2 = dot(cross(m1, m4), m3) / dot(cross(m2, m4), m3);
    let k3 = dot(cross(m1, m4), m2) / dot(cross(m3, m4), m2);
    let n2 = [k2 * m2[0] - m1[0], k2 * m2[1] - m1[1], k2 * m2[2] - m1[2]];
    let n3 = [k3 * m3[0] - m1[0], k3 * m3[1] - m1[1], k3 * m3[2] - m1[2]];
    let flat2 = n2[0] * n2[0] + n2[1] * n2[1];
    let flat3 = n3[0] * n3[0] + n3[1] * n3[1];
    let depth = n2[2] * n3[2];
    let focal2 = -(n2[0] * n3[0] + n2[1] * n3[1]) / depth;
    let ratio2 = if depth.abs() > 1e-9 && focal2.is_finite() && focal2 > 0.0 {
        (flat2 / focal2 + n2[2] * n2[2]) / (flat3 / focal2 + n3[2] * n3[2])
    } else {
        // Seen square on: no slant to undo.
        flat2 / flat3
    };
    let ratio = ratio2.sqrt() as f32;
    if !ratio.is_finite() || ratio <= 0.0 {
        return 1.0;
    }
    ratio.max(1.0 / ratio)
}

/// The sides of `rough` fitted to the edges of the full photo, each looked
/// for within `reach` pixels of where it was; its corners where the fitted
/// sides meet.
fn fit_sides(photo: &Photo<'_>, rough: [(f32, f32); 4], reach: f32) -> [(f32, f32); 4] {
    let sides: Vec<FitLine> = (0..4)
        .map(|k| fit_side(photo, rough[k], rough[(k + 1) % 4], reach))
        .collect();
    let mut corners = rough;
    for (k, corner) in corners.iter_mut().enumerate() {
        let before = &sides[(k + 3) % 4];
        let after = &sides[k];
        if let Some(p) = before.meet(after)
            && (p.0 - corner.0).hypot(p.1 - corner.1) <= reach * 3.0
        {
            *corner = p;
        }
    }
    corners
}

/// A line through `point` along `dir` (unit length).
#[derive(Clone, Copy, Debug)]
struct FitLine {
    point: (f32, f32),
    dir: (f32, f32),
}

impl FitLine {
    fn meet(&self, other: &FitLine) -> Option<(f32, f32)> {
        let det = self.dir.0 * other.dir.1 - self.dir.1 * other.dir.0;
        if det.abs() < 0.17 {
            return None;
        }
        let (dx, dy) = (other.point.0 - self.point.0, other.point.1 - self.point.1);
        let t = (dx * other.dir.1 - dy * other.dir.0) / det;
        Some((self.point.0 + self.dir.0 * t, self.point.1 + self.dir.1 * t))
    }
}

fn fit_side(photo: &Photo<'_>, a: (f32, f32), b: (f32, f32), reach: f32) -> FitLine {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length = dx.hypot(dy).max(1e-6);
    let dir = (dx / length, dy / length);
    let normal = (-dir.1, dir.0);
    let rough = FitLine { point: a, dir };
    let samples = ((length / 3.0) as usize).clamp(12, 160);
    let steps = (reach * 2.0).ceil() as i32;
    let found: Vec<((f32, f32), f32)> = (0..samples)
        .into_par_iter()
        .filter_map(|i| {
            let t = 0.12 + 0.76 * (i as f32 + 0.5) / samples as f32;
            let (px, py) = (a.0 + dx * t, a.1 + dy * t);
            let change = |s: f32| {
                let before = photo.at(px + normal.0 * (s - 1.0), py + normal.1 * (s - 1.0));
                let after = photo.at(px + normal.0 * (s + 1.0), py + normal.1 * (s + 1.0));
                (0..3).map(|c| (after[c] - before[c]).abs()).sum::<f32>()
            };
            // The edge nearest where the side was, before a stronger one
            // further off (a line of print on the card).
            let near = reach * 0.6;
            let profile: Vec<f32> = (-steps..=steps)
                .map(|k| {
                    let s = k as f32 * 0.5;
                    change(s) * (-(s / near) * (s / near)).exp()
                })
                .collect();
            let (k, &peak) = profile
                .iter()
                .enumerate()
                .max_by(|x, y| x.1.total_cmp(y.1))?;
            if change((k as i32 - steps) as f32 * 0.5) < 24.0 {
                return None;
            }
            // Between samples: the top of the parabola through the peak.
            let left = if k > 0 { profile[k - 1] } else { peak };
            let right = profile.get(k + 1).copied().unwrap_or(peak);
            let bend = left - 2.0 * peak + right;
            let shift = if bend < 0.0 {
                (0.5 * (left - right) / bend).clamp(-0.5, 0.5)
            } else {
                0.0
            };
            let s = ((k as i32 - steps) as f32 + shift) * 0.5;
            Some(((px + normal.0 * s, py + normal.1 * s), peak))
        })
        .collect();
    if found.len() < samples / 4 {
        return rough;
    }
    let mut keep: Vec<(f32, f32)> = found.iter().map(|f| f.0).collect();
    let mut line = through(&keep).unwrap_or(rough);
    for _ in 0..4 {
        let off: Vec<f32> = keep.iter().map(|&p| off_line(&line, p)).collect();
        let mut sorted = off.clone();
        sorted.sort_by(f32::total_cmp);
        let median = sorted[sorted.len() / 2];
        let tolerance = (median * 2.5).max(1.0);
        let next: Vec<(f32, f32)> = keep
            .iter()
            .zip(&off)
            .filter(|&(_, &d)| d <= tolerance)
            .map(|(&p, _)| p)
            .collect();
        if next.len() < samples / 4 {
            break;
        }
        keep = next;
        match through(&keep) {
            Some(fitted) => line = fitted,
            None => break,
        }
    }
    // A fit that turned far from the rough side found something else.
    if (line.dir.0 * dir.0 + line.dir.1 * dir.1).abs() < 0.985 {
        return rough;
    }
    line
}

fn off_line(line: &FitLine, p: (f32, f32)) -> f32 {
    ((p.0 - line.point.0) * line.dir.1 - (p.1 - line.point.1) * line.dir.0).abs()
}

/// The line closest to `points` (least squares across the line).
fn through(points: &[(f32, f32)]) -> Option<FitLine> {
    if points.len() < 2 {
        return None;
    }
    let n = points.len() as f32;
    let (mx, my) = points
        .iter()
        .fold((0.0, 0.0), |(x, y), p| (x + p.0, y + p.1));
    let (mx, my) = (mx / n, my / n);
    let (mut xx, mut xy, mut yy) = (0.0f32, 0.0f32, 0.0f32);
    for &(x, y) in points {
        let (dx, dy) = (x - mx, y - my);
        xx += dx * dx;
        xy += dx * dy;
        yy += dy * dy;
    }
    let angle = 0.5 * (2.0 * xy).atan2(xx - yy);
    Some(FitLine {
        point: (mx, my),
        dir: (angle.cos(), angle.sin()),
    })
}

/// Corners going clockwise from the top-left, a long side on top: the upper
/// long side when the card lies flat, the left one when it stands on end.
fn order(c: [(f32, f32); 4]) -> [(f32, f32); 4] {
    let mut c = c;
    if shoelace(&c) < 0.0 {
        c.reverse();
    }
    let len = |c: &[(f32, f32); 4], i: usize| {
        let (a, b) = (c[i], c[(i + 1) % 4]);
        (b.0 - a.0).hypot(b.1 - a.1)
    };
    let long = if len(&c, 0) + len(&c, 2) >= len(&c, 1) + len(&c, 3) {
        0
    } else {
        1
    };
    let mid = |i: usize| {
        let (a, b) = (c[i], c[(i + 1) % 4]);
        ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0)
    };
    let (one, other) = (mid(long), mid(long + 2));
    let short = len(&c, (long + 1) % 4).min(len(&c, (long + 3) % 4));
    let top = if (one.1 - other.1).abs() > 0.25 * short {
        if one.1 < other.1 { long } else { long + 2 }
    } else if one.0 <= other.0 {
        long
    } else {
        long + 2
    };
    [
        c[top % 4],
        c[(top + 1) % 4],
        c[(top + 2) % 4],
        c[(top + 3) % 4],
    ]
}

#[cfg(test)]
mod tests;

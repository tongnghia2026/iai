//! Quick Select engine behind the Smart Select tool (W).
//!
//! Photoshop's Quick Selection is a progressive *local graph cut* ("Paint
//! Selection", Liu, Sun & Shum, SIGGRAPH 2009): pixels under the brush centre
//! are hard foreground, a colour model of the brushed pixels competes with a
//! model of the rest of the image, and a contrast-sensitive min-cut places the
//! boundary in a region around the newest brush segment (widened while the
//! result keeps running into its edge; solved coarse-to-fine when large). New
//! pixels must stay connected to the brush, so a stroke only ever grows
//! outward from where it is painted — it never jumps to a distant,
//! similar-coloured area.

use std::collections::VecDeque;

use super::selection::EdgeCache;

/// Smoothness weight: how strongly the cut prefers short boundaries that run
/// along colour edges (nats per pixel of boundary).
const LAMBDA: f32 = 24.0;
/// Float energy -> integer capacity.
const SCALE: f32 = 64.0;
const HARD: i32 = 1 << 26;
/// Per-pixel colour cost cap, so a single odd pixel cannot dominate.
const MAX_COST: f32 = 48.0;
/// Extra cost (nats) for leaving a pixel right under the brush unselected;
/// fades to zero at the brush rim.
const BRUSH_PRIOR: f32 = 3.0;
/// Lab distance within which a pixel near the brush centre counts as the
/// colour being painted (feeds the brush colour model).
const COLOR_GATE: f32 = 18.0;
/// Distance from the brush (in brush radii) that a stroke reaches freely.
const REACH: f32 = 4.0;
/// Extra cost (nats) per further `REACH` of distance for joining the stroke.
const LOCALITY: f32 = 1.5;
const OTHER_SAMPLES: usize = 2400;
const OTHER_CLUSTERS: usize = 10;
const BRUSH_CLUSTERS: usize = 5;
const BRUSH_SAMPLES: usize = 900;
/// Colour noise floor (Lab variance) for the Gaussian clusters.
const VAR_FLOOR: f32 = 9.0;
/// Largest grid solved in one level; bigger regions start on a coarse grid
/// and are refined along the boundary level by level.
const MAX_LEVEL_NODES: usize = 16_000;
/// Cells of the coarser level on each side of its boundary that the next
/// level re-solves.
const BAND: usize = 2;
/// Largest region examined for one update (about 2000 x 2000 px).
const MAX_ROI_PIXELS: usize = 4_000_000;

/// Cell states for one cut: solved, or fixed to either label (the selection
/// being grown, or the coarser level's answer away from its boundary).
const FREE: u8 = 0;
const FIX_TARGET: u8 = 1;
const FIX_OTHER: u8 = 2;

const DIRS: [(i32, i32); 8] = [
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
    (0, -1),
    (1, -1),
];

#[inline]
fn opp(d: usize) -> usize {
    (d + 4) & 7
}

#[inline]
fn dist2(a: [f32; 3], b: [f32; 3]) -> f32 {
    let d0 = a[0] - b[0];
    let d1 = a[1] - b[1];
    let d2 = a[2] - b[2];
    d0 * d0 + d1 * d1 + d2 * d2
}

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn unit(&mut self) -> f32 {
        (self.next() & 0xFF_FFFF) as f32 / 16_777_216.0
    }
}

// ─── Boykov–Kolmogorov max-flow on an 8-connected pixel graph ───────────────

const NO_NODE: u32 = u32::MAX;
const P_TERMINAL: u8 = 8;
const P_ORPHAN: u8 = 9;
const P_NONE: u8 = 10;

struct Maxflow {
    nbr: Vec<[u32; 8]>,
    cap: Vec<[i32; 8]>,
    /// Residual terminal capacity: > 0 towards the source, < 0 towards the sink.
    tr: Vec<i32>,
    /// Direction of the arc to the search-tree parent, or a `P_*` marker.
    parent: Vec<u8>,
    sink: Vec<bool>,
    ts: Vec<u32>,
    dist: Vec<u32>,
    queued: Vec<bool>,
    active: VecDeque<u32>,
    orphans: VecDeque<u32>,
    time: u32,
}

impl Maxflow {
    fn new(n: usize) -> Self {
        Self {
            nbr: vec![[NO_NODE; 8]; n],
            cap: vec![[0; 8]; n],
            tr: vec![0; n],
            parent: vec![P_NONE; n],
            sink: vec![false; n],
            ts: vec![0; n],
            dist: vec![0; n],
            queued: vec![false; n],
            active: VecDeque::new(),
            orphans: VecDeque::new(),
            time: 0,
        }
    }

    fn link(&mut self, i: usize, d: usize, j: usize, c: i32) {
        self.nbr[i][d] = j as u32;
        self.nbr[j][opp(d)] = i as u32;
        self.cap[i][d] = c;
        self.cap[j][opp(d)] = c;
    }

    fn set_active(&mut self, i: usize) {
        if !self.queued[i] {
            self.queued[i] = true;
            self.active.push_back(i as u32);
        }
    }

    fn next_active(&mut self) -> Option<usize> {
        while let Some(i) = self.active.pop_front() {
            let i = i as usize;
            self.queued[i] = false;
            if self.parent[i] != P_NONE {
                return Some(i);
            }
        }
        None
    }

    fn orphan_front(&mut self, i: usize) {
        self.parent[i] = P_ORPHAN;
        self.orphans.push_front(i as u32);
    }

    fn solve(&mut self) {
        for i in 0..self.tr.len() {
            if self.tr[i] != 0 {
                self.parent[i] = P_TERMINAL;
                self.sink[i] = self.tr[i] < 0;
                self.ts[i] = 0;
                self.dist[i] = 1;
                self.set_active(i);
            } else {
                self.parent[i] = P_NONE;
            }
        }

        let mut current: Option<usize> = None;
        loop {
            let i = match current.take() {
                Some(i) => {
                    self.queued[i] = false;
                    if self.parent[i] != P_NONE {
                        i
                    } else {
                        match self.next_active() {
                            Some(i) => i,
                            None => break,
                        }
                    }
                }
                None => match self.next_active() {
                    Some(i) => i,
                    None => break,
                },
            };

            // Grow the tree `i` belongs to until it meets the other one.
            let mut hit: Option<(usize, usize)> = None;
            if !self.sink[i] {
                for d in 0..8 {
                    let j = self.nbr[i][d];
                    if j == NO_NODE || self.cap[i][d] == 0 {
                        continue;
                    }
                    let j = j as usize;
                    if self.parent[j] == P_NONE {
                        self.sink[j] = false;
                        self.parent[j] = opp(d) as u8;
                        self.ts[j] = self.ts[i];
                        self.dist[j] = self.dist[i] + 1;
                        self.set_active(j);
                    } else if self.sink[j] {
                        hit = Some((i, d));
                        break;
                    } else if self.ts[j] <= self.ts[i] && self.dist[j] > self.dist[i] {
                        self.parent[j] = opp(d) as u8;
                        self.ts[j] = self.ts[i];
                        self.dist[j] = self.dist[i] + 1;
                    }
                }
            } else {
                for d in 0..8 {
                    let j = self.nbr[i][d];
                    if j == NO_NODE {
                        continue;
                    }
                    let j = j as usize;
                    if self.cap[j][opp(d)] == 0 {
                        continue;
                    }
                    if self.parent[j] == P_NONE {
                        self.sink[j] = true;
                        self.parent[j] = opp(d) as u8;
                        self.ts[j] = self.ts[i];
                        self.dist[j] = self.dist[i] + 1;
                        self.set_active(j);
                    } else if !self.sink[j] {
                        hit = Some((j, opp(d)));
                        break;
                    } else if self.ts[j] <= self.ts[i] && self.dist[j] > self.dist[i] {
                        self.parent[j] = opp(d) as u8;
                        self.ts[j] = self.ts[i];
                        self.dist[j] = self.dist[i] + 1;
                    }
                }
            }

            self.time = self.time.wrapping_add(1);
            if let Some((a, d)) = hit {
                // `i` may have more arcs to grow through: keep it current.
                self.queued[i] = true;
                current = Some(i);
                self.augment(a, d);
                self.adopt();
            }
        }
    }

    /// Push flow along source-root … a → nbr(a, d) … sink-root.
    fn augment(&mut self, a: usize, d: usize) {
        let b = self.nbr[a][d] as usize;

        let mut bottleneck = self.cap[a][d];
        let mut k = a;
        loop {
            let p = self.parent[k];
            if p == P_TERMINAL {
                break;
            }
            let m = self.nbr[k][p as usize] as usize;
            bottleneck = bottleneck.min(self.cap[m][opp(p as usize)]);
            k = m;
        }
        bottleneck = bottleneck.min(self.tr[k]);
        let mut k = b;
        loop {
            let p = self.parent[k];
            if p == P_TERMINAL {
                break;
            }
            bottleneck = bottleneck.min(self.cap[k][p as usize]);
            k = self.nbr[k][p as usize] as usize;
        }
        bottleneck = bottleneck.min(-self.tr[k]);

        self.cap[a][d] -= bottleneck;
        self.cap[b][opp(d)] += bottleneck;

        let mut k = a;
        loop {
            let p = self.parent[k];
            if p == P_TERMINAL {
                self.tr[k] -= bottleneck;
                if self.tr[k] == 0 {
                    self.orphan_front(k);
                }
                break;
            }
            let p = p as usize;
            let m = self.nbr[k][p] as usize;
            self.cap[m][opp(p)] -= bottleneck;
            self.cap[k][p] += bottleneck;
            if self.cap[m][opp(p)] == 0 {
                self.orphan_front(k);
            }
            k = m;
        }
        let mut k = b;
        loop {
            let p = self.parent[k];
            if p == P_TERMINAL {
                self.tr[k] += bottleneck;
                if self.tr[k] == 0 {
                    self.orphan_front(k);
                }
                break;
            }
            let p = p as usize;
            let m = self.nbr[k][p] as usize;
            self.cap[k][p] -= bottleneck;
            self.cap[m][opp(p)] += bottleneck;
            if self.cap[k][p] == 0 {
                self.orphan_front(k);
            }
            k = m;
        }
    }

    fn adopt(&mut self) {
        while let Some(i) = self.orphans.pop_front() {
            let i = i as usize;
            let in_sink = self.sink[i];
            self.process_orphan(i, in_sink);
        }
    }

    fn process_orphan(&mut self, i: usize, in_sink: bool) {
        let mut best_dir = P_NONE;
        let mut best_dist = u32::MAX;
        for d in 0..8 {
            let j = self.nbr[i][d];
            if j == NO_NODE {
                continue;
            }
            let j = j as usize;
            let residual = if in_sink {
                self.cap[i][d]
            } else {
                self.cap[j][opp(d)]
            };
            if residual == 0 || self.parent[j] == P_NONE || self.sink[j] != in_sink {
                continue;
            }
            // Does j still reach a terminal?
            let mut k = j;
            let mut dd: u32 = 0;
            loop {
                if self.ts[k] == self.time {
                    dd = dd.saturating_add(self.dist[k]);
                    break;
                }
                let p = self.parent[k];
                dd += 1;
                if p == P_TERMINAL {
                    self.ts[k] = self.time;
                    self.dist[k] = 1;
                    break;
                }
                if p == P_ORPHAN {
                    dd = u32::MAX;
                    break;
                }
                k = self.nbr[k][p as usize] as usize;
            }
            if dd == u32::MAX {
                continue;
            }
            if dd < best_dist {
                best_dir = d as u8;
                best_dist = dd;
            }
            let mut k = j;
            let mut dk = dd;
            while self.ts[k] != self.time {
                self.ts[k] = self.time;
                self.dist[k] = dk;
                dk = dk.saturating_sub(1);
                k = self.nbr[k][self.parent[k] as usize] as usize;
            }
        }

        if best_dir != P_NONE {
            self.parent[i] = best_dir;
            self.ts[i] = self.time;
            self.dist[i] = best_dist + 1;
            return;
        }

        self.parent[i] = P_NONE;
        for d in 0..8 {
            let j = self.nbr[i][d];
            if j == NO_NODE {
                continue;
            }
            let j = j as usize;
            if self.parent[j] == P_NONE || self.sink[j] != in_sink {
                continue;
            }
            let residual = if in_sink {
                self.cap[i][d]
            } else {
                self.cap[j][opp(d)]
            };
            if residual > 0 {
                self.set_active(j);
            }
            let p = self.parent[j];
            if p != P_TERMINAL && p != P_ORPHAN && self.nbr[j][p as usize] as usize == i {
                self.parent[j] = P_ORPHAN;
                self.orphans.push_back(j as u32);
            }
        }
    }

    fn is_source(&self, i: usize) -> bool {
        self.parent[i] != P_NONE && !self.sink[i]
    }
}

// ─── Colour models ─────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct Gaussian {
    mean: [f32; 3],
    inv2var: f32,
    bias: f32,
}

/// Isotropic Gaussian mixture in Lab, evaluated with the best component
/// (negative log-likelihood).
#[derive(Clone)]
struct ColorModel {
    parts: Vec<Gaussian>,
}

impl ColorModel {
    fn fit(samples: &[[f32; 3]], k: usize, seed: u64) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        let k = k.clamp(1, samples.len());
        let mut rng = Lcg(seed ^ 0x9E37_79B9_7F4A_7C15);

        // k-means++ seeding.
        let mut centers: Vec<[f32; 3]> = vec![samples[rng.below(samples.len())]];
        let mut near: Vec<f32> = samples.iter().map(|s| dist2(*s, centers[0])).collect();
        while centers.len() < k {
            let total: f32 = near.iter().sum();
            if total <= 1e-3 {
                break;
            }
            let mut t = rng.unit() * total;
            let mut pick = samples.len() - 1;
            for (i, &v) in near.iter().enumerate() {
                t -= v;
                if t <= 0.0 {
                    pick = i;
                    break;
                }
            }
            let c = samples[pick];
            centers.push(c);
            for (i, s) in samples.iter().enumerate() {
                near[i] = near[i].min(dist2(*s, c));
            }
        }

        let k = centers.len();
        let nearest = |s: [f32; 3], centers: &[[f32; 3]]| -> (usize, f32) {
            let mut best = (0, f32::MAX);
            for (c, m) in centers.iter().enumerate() {
                let d = dist2(s, *m);
                if d < best.1 {
                    best = (c, d);
                }
            }
            best
        };
        for _ in 0..8 {
            let mut sum = vec![[0f32; 3]; k];
            let mut cnt = vec![0usize; k];
            for s in samples {
                let (c, _) = nearest(*s, &centers);
                cnt[c] += 1;
                for ch in 0..3 {
                    sum[c][ch] += s[ch];
                }
            }
            for c in 0..k {
                if cnt[c] > 0 {
                    for ch in 0..3 {
                        centers[c][ch] = sum[c][ch] / cnt[c] as f32;
                    }
                }
            }
        }

        let mut var = vec![0f32; k];
        let mut cnt = vec![0usize; k];
        for s in samples {
            let (c, d) = nearest(*s, &centers);
            cnt[c] += 1;
            var[c] += d;
        }
        let n = samples.len() as f32;
        let parts = (0..k)
            .filter(|&c| cnt[c] > 0)
            .map(|c| {
                let v = (var[c] / (3.0 * cnt[c] as f32)).max(VAR_FLOOR);
                Gaussian {
                    mean: centers[c],
                    inv2var: 0.5 / v,
                    bias: -(cnt[c] as f32 / n).ln() + 1.5 * (2.0 * std::f32::consts::PI * v).ln(),
                }
            })
            .collect();
        Some(Self { parts })
    }

    fn cost(&self, c: [f32; 3]) -> f32 {
        let mut best = f32::MAX;
        for g in &self.parts {
            let v = dist2(c, g.mean) * g.inv2var + g.bias;
            if v < best {
                best = v;
            }
        }
        best.min(MAX_COST)
    }
}

// ─── Brush geometry ────────────────────────────────────────────────────────

/// Polyline of cursor positions painted since the previous update.
struct BrushPath {
    points: Vec<(f32, f32)>,
    radius: f32,
    /// Hard-seed radius around the cursor centre line.
    core: f32,
    bbox: (f32, f32, f32, f32),
}

impl BrushPath {
    fn new(points: Vec<(f32, f32)>, radius: f32) -> Self {
        let mut bbox = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &(x, y) in &points {
            bbox.0 = bbox.0.min(x - radius);
            bbox.1 = bbox.1.min(y - radius);
            bbox.2 = bbox.2.max(x + radius);
            bbox.3 = bbox.3.max(y + radius);
        }
        Self {
            points,
            radius,
            core: (radius * 0.2).max(1.0),
            bbox,
        }
    }

    /// Whether pixel centre `(x, y)` lies under the brush.
    fn covers(&self, x: f32, y: f32) -> bool {
        x >= self.bbox.0
            && y >= self.bbox.1
            && x <= self.bbox.2
            && y <= self.bbox.3
            && self.dist(x, y) <= self.radius
    }

    /// Distance from pixel centre `(x, y)` to the cursor path.
    fn dist(&self, x: f32, y: f32) -> f32 {
        let mut best = f32::INFINITY;
        if self.points.len() == 1 {
            let (px, py) = self.points[0];
            return ((x - px).powi(2) + (y - py).powi(2)).sqrt();
        }
        for seg in self.points.windows(2) {
            let (ax, ay) = seg[0];
            let (bx, by) = seg[1];
            let (vx, vy) = (bx - ax, by - ay);
            let len2 = vx * vx + vy * vy;
            let t = if len2 > 1e-6 {
                (((x - ax) * vx + (y - ay) * vy) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let dx = x - (ax + vx * t);
            let dy = y - (ay + vy * t);
            best = best.min(dx * dx + dy * dy);
        }
        best.sqrt()
    }
}

/// Cells within `BAND` cells of a label change.
fn near_boundary(labels: &[bool], gw: usize, gh: usize) -> Vec<bool> {
    let mut band = vec![false; gw * gh];
    for y in 0..gh {
        for x in 0..gw {
            let l = labels[y * gw + x];
            let edge = (x + 1 < gw && labels[y * gw + x + 1] != l)
                || (y + 1 < gh && labels[(y + 1) * gw + x] != l);
            if !edge {
                continue;
            }
            for by in y.saturating_sub(BAND)..=(y + 1 + BAND).min(gh - 1) {
                for bx in x.saturating_sub(BAND)..=(x + 1 + BAND).min(gw - 1) {
                    band[by * gw + bx] = true;
                }
            }
        }
    }
    band
}

// ─── Stroke ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuickSelectOp {
    /// Grow the selection (New / Add).
    Add,
    /// Shrink it (Alt).
    Subtract,
}

/// State of one Quick Select drag.
pub struct QuickSelectStroke {
    op: QuickSelectOp,
    radius: f32,
    /// Colour model of what the stroke must *not* take: the unselected image
    /// when adding, the selection when subtracting.
    other: Option<ColorModel>,
    beta: f32,
    last: Option<(f32, f32)>,
    seed: u64,
}

impl QuickSelectStroke {
    pub fn begin(
        cache: &EdgeCache,
        mask: &[u8],
        op: QuickSelectOp,
        radius: f32,
        at: (f32, f32),
    ) -> Self {
        let w = cache.width as usize;
        let h = cache.height as usize;
        let radius = radius.max(1.0);
        let seed = (at.0.to_bits() as u64) << 32 ^ at.1.to_bits() as u64 ^ (w * h) as u64;
        let mut rng = Lcg(seed);
        let target = |m: u8| (m >= 128) == (op == QuickSelectOp::Add);

        let mut other = None;
        let mut beta = 1.0 / 16.0;
        if w > 0 && h > 0 && mask.len() == w * h && cache.lab.len() == w * h {
            let keep_out = (radius * 1.5).powi(2);
            let mut samples = Vec::with_capacity(OTHER_SAMPLES);
            let mut tries = 0;
            while samples.len() < OTHER_SAMPLES && tries < OTHER_SAMPLES * 16 {
                tries += 1;
                let i = rng.below(w * h);
                if target(mask[i]) {
                    continue;
                }
                let dx = (i % w) as f32 + 0.5 - at.0;
                let dy = (i / w) as f32 + 0.5 - at.1;
                if dx * dx + dy * dy < keep_out {
                    continue;
                }
                samples.push(cache.lab[i]);
            }
            if samples.len() >= 24 {
                other = ColorModel::fit(&samples, OTHER_CLUSTERS, seed);
            }

            // Contrast normalisation (GrabCut's β) from the whole image.
            let mut sum = 0.0f64;
            let mut count = 0usize;
            for _ in 0..8000 {
                let x = rng.below(w.saturating_sub(1).max(1));
                let y = rng.below(h.saturating_sub(1).max(1));
                let i = y * w + x;
                if x + 1 < w {
                    sum += dist2(cache.lab[i], cache.lab[i + 1]) as f64;
                    count += 1;
                }
                if y + 1 < h {
                    sum += dist2(cache.lab[i], cache.lab[i + w]) as f64;
                    count += 1;
                }
            }
            let mean = if count > 0 { sum / count as f64 } else { 16.0 };
            beta = (0.5 / mean.max(4.0)) as f32;
        }

        Self {
            op,
            radius,
            other,
            beta,
            last: None,
            seed,
        }
    }

    pub fn op(&self) -> QuickSelectOp {
        self.op
    }

    /// Continue the stroke through `points` (cursor positions, canvas px).
    /// Returns the bounding box `(x0, y0, x1, y1)` of changed mask pixels.
    pub fn extend(
        &mut self,
        cache: &EdgeCache,
        mask: &mut [u8],
        points: &[(f32, f32)],
    ) -> Option<(usize, usize, usize, usize)> {
        let w = cache.width as usize;
        let h = cache.height as usize;
        if w == 0 || h == 0 || mask.len() != w * h || cache.lab.len() != w * h {
            return None;
        }
        let clamp = |(x, y): (f32, f32)| (x.clamp(0.0, w as f32), y.clamp(0.0, h as f32));
        let continuing = self.last.is_some();
        let mut path: Vec<(f32, f32)> = Vec::with_capacity(points.len() + 1);
        if let Some(last) = self.last {
            path.push(last);
        }
        for &p in points {
            let p = clamp(p);
            if path
                .last()
                .is_none_or(|q| (q.0 - p.0).abs() + (q.1 - p.1).abs() >= 0.5)
            {
                path.push(p);
            }
        }
        let &end = path.last()?;
        self.last = Some(end);
        // The cursor has not moved since the last update.
        if continuing && path.len() == 1 {
            return None;
        }

        let brush = BrushPath::new(path, self.radius);
        let target_model = self.brush_model(cache, &brush)?;
        let mut margin = (2.0 * self.radius).max(24.0);
        loop {
            let roi = (
                (brush.bbox.0 - margin).floor().max(0.0) as usize,
                (brush.bbox.1 - margin).floor().max(0.0) as usize,
                ((brush.bbox.2 + margin).ceil().max(0.0) as usize).min(w),
                ((brush.bbox.3 + margin).ceil().max(0.0) as usize).min(h),
            );
            if roi.2 <= roi.0 || roi.3 <= roi.1 {
                return None;
            }
            let labels = self.cut_roi(cache, mask, &brush, &target_model, roi);
            let (keep, open) = self.connected_to_brush(cache, mask, &brush, roi, &labels);
            // The new region runs off the examined area: look further, so a
            // uniform area is taken whole instead of cut off square.
            let room = (roi.2 - roi.0) * (roi.3 - roi.1) < MAX_ROI_PIXELS;
            if open && room && roi != (0, 0, w, h) {
                margin *= 2.0;
                continue;
            }
            return self.write(cache, mask, roi, &keep);
        }
    }

    fn is_target(&self, m: u8) -> bool {
        (m >= 128) == (self.op == QuickSelectOp::Add)
    }

    /// Colour model of the pixels being painted: the brush core, limited to
    /// colours close to what is under the cursor centre line.
    fn brush_model(&self, cache: &EdgeCache, brush: &BrushPath) -> Option<ColorModel> {
        let w = cache.width as usize;
        let h = cache.height as usize;
        let lab_at = |x: usize, y: usize| cache.lab[y * w + x];

        // Reference colours along the centre line (3×3 means).
        let mut refs: Vec<[f32; 3]> = Vec::new();
        let total_len: f32 = brush
            .points
            .windows(2)
            .map(|s| ((s[1].0 - s[0].0).powi(2) + (s[1].1 - s[0].1).powi(2)).sqrt())
            .sum();
        let step = (total_len / 24.0).max(1.5);
        let mut sample_ref = |x: f32, y: f32| {
            let cx = (x.floor().max(0.0) as usize).min(w - 1);
            let cy = (y.floor().max(0.0) as usize).min(h - 1);
            let mut acc = [0f32; 3];
            let mut n = 0.0;
            for yy in cy.saturating_sub(1)..=(cy + 1).min(h - 1) {
                for xx in cx.saturating_sub(1)..=(cx + 1).min(w - 1) {
                    let l = lab_at(xx, yy);
                    acc[0] += l[0];
                    acc[1] += l[1];
                    acc[2] += l[2];
                    n += 1.0;
                }
            }
            refs.push([acc[0] / n, acc[1] / n, acc[2] / n]);
        };
        sample_ref(brush.points[0].0, brush.points[0].1);
        for seg in brush.points.windows(2) {
            let (ax, ay) = seg[0];
            let (bx, by) = seg[1];
            let len = ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt();
            let n = (len / step).ceil().max(1.0) as usize;
            for i in 1..=n {
                let t = i as f32 / n as f32;
                sample_ref(ax + (bx - ax) * t, ay + (by - ay) * t);
            }
        }

        // Core pixels whose colour matches the centre line.
        let reach = (self.radius * 0.5).max(brush.core);
        let x0 = (brush.bbox.0 + self.radius - reach).floor().max(0.0) as usize;
        let y0 = (brush.bbox.1 + self.radius - reach).floor().max(0.0) as usize;
        let x1 = ((brush.bbox.2 - self.radius + reach).ceil().max(0.0) as usize).min(w);
        let y1 = ((brush.bbox.3 - self.radius + reach).ceil().max(0.0) as usize).min(h);
        let area = (x1.saturating_sub(x0) * y1.saturating_sub(y0)).max(1);
        let stride = ((area as f32 / (BRUSH_SAMPLES as f32 * 3.0)).sqrt().floor() as usize).max(1);
        let gate2 = COLOR_GATE * COLOR_GATE;
        let mut samples = refs.clone();
        let mut y = y0;
        while y < y1 {
            let mut x = x0;
            while x < x1 {
                let d = brush.dist(x as f32 + 0.5, y as f32 + 0.5);
                if d <= reach {
                    let lab = lab_at(x, y);
                    if d <= brush.core || refs.iter().any(|r| dist2(*r, lab) <= gate2) {
                        samples.push(lab);
                    }
                }
                x += stride;
            }
            y += stride;
        }
        if samples.len() > BRUSH_SAMPLES {
            let keep = samples.len() as f32 / BRUSH_SAMPLES as f32;
            samples = (0..BRUSH_SAMPLES)
                .map(|i| samples[((i as f32 * keep) as usize).min(samples.len() - 1)])
                .collect();
        }
        ColorModel::fit(&samples, BRUSH_CLUSTERS, self.seed ^ samples.len() as u64)
    }

    /// Data costs (target, other) for one pixel or coarse cell.
    fn unary(&self, target: &ColorModel, lab: [f32; 3], d: f32) -> (f32, f32) {
        let mut ct = target.cost(lab);
        let mut co = self.other.as_ref().map_or(MAX_COST * 0.5, |m| m.cost(lab));
        if d < self.radius {
            co += BRUSH_PRIOR * (1.0 - d / self.radius);
        }
        // Far from the brush a pixel needs ever clearer evidence, so a click
        // takes the area around it and painting further extends it.
        let reach = (self.radius * REACH).max(64.0);
        if d > reach {
            ct += LOCALITY * (d - reach) / reach;
        }
        (ct, co)
    }

    fn pair_weight(&self, a: [f32; 3], b: [f32; 3], diagonal: bool) -> f32 {
        let wgt = LAMBDA * (-self.beta * dist2(a, b)).exp();
        if diagonal {
            wgt * std::f32::consts::FRAC_1_SQRT_2
        } else {
            wgt
        }
    }

    /// Cut the region `roi`: coarse-to-fine, each finer level re-solving only
    /// a band around the previous level's boundary.
    fn cut_roi(
        &self,
        cache: &EdgeCache,
        mask: &[u8],
        brush: &BrushPath,
        target_model: &ColorModel,
        roi: (usize, usize, usize, usize),
    ) -> Vec<bool> {
        let w = cache.width as usize;
        let (x0, y0, x1, y1) = roi;
        let rw = x1 - x0;
        let rh = y1 - y0;

        let mut k = ((rw * rh) as f32 / MAX_LEVEL_NODES as f32)
            .sqrt()
            .ceil()
            .max(1.0) as usize;
        // (cell size, grid width, labels, near-boundary flags) of the
        // previous, coarser level.
        let mut prev: Option<(usize, usize, Vec<bool>, Vec<bool>)> = None;
        loop {
            let cw = rw.div_ceil(k);
            let ch = rh.div_ceil(k);
            let coarse = k > 1;
            let mut state = vec![FREE; cw * ch];
            let mut cell_lab = vec![[0f32; 3]; if coarse { cw * ch } else { 0 }];
            let mut cell_area = vec![1f32; if coarse { cw * ch } else { 0 }];
            for cy in 0..ch {
                for cx in 0..cw {
                    let c = cy * cw + cx;
                    let bx0 = x0 + cx * k;
                    let by0 = y0 + cy * k;
                    let bx1 = (bx0 + k).min(x1);
                    let by1 = (by0 + k).min(y1);
                    let m = (bx1 - bx0) * (by1 - by0);
                    let mut targets = 0usize;
                    let mut acc = [0f32; 3];
                    for y in by0..by1 {
                        for x in bx0..bx1 {
                            let i = y * w + x;
                            if self.is_target(mask[i]) {
                                targets += 1;
                            }
                            if coarse {
                                let l = cache.lab[i];
                                acc[0] += l[0];
                                acc[1] += l[1];
                                acc[2] += l[2];
                            }
                        }
                    }
                    if coarse {
                        let mf = m as f32;
                        cell_lab[c] = [acc[0] / mf, acc[1] / mf, acc[2] / mf];
                        cell_area[c] = mf;
                    }
                    state[c] = if targets == m {
                        FIX_TARGET
                    } else if let Some((pk, pw, labels, band)) = &prev {
                        let pc = ((by0 + by1) / 2 - y0) / pk * pw + ((bx0 + bx1) / 2 - x0) / pk;
                        if band[pc] {
                            FREE
                        } else if labels[pc] {
                            FIX_TARGET
                        } else {
                            FIX_OTHER
                        }
                    } else {
                        FREE
                    };
                }
            }

            let kf = k as f32;
            let labels = if coarse {
                self.solve_grid(
                    cw,
                    ch,
                    &state,
                    |c| cell_lab[c],
                    |c| {
                        let px = x0 as f32 + ((c % cw) as f32 + 0.5) * kf;
                        let py = y0 as f32 + ((c / cw) as f32 + 0.5) * kf;
                        brush.dist(px, py)
                    },
                    |c| cell_area[c],
                    kf,
                    target_model,
                    brush.core.max(kf * 0.75),
                )
            } else {
                self.solve_grid(
                    cw,
                    ch,
                    &state,
                    |q| cache.lab[(y0 + q / cw) * w + x0 + q % cw],
                    |q| brush.dist((x0 + q % cw) as f32 + 0.5, (y0 + q / cw) as f32 + 0.5),
                    |_| 1.0,
                    1.0,
                    target_model,
                    brush.core,
                )
            };
            if !coarse {
                return labels;
            }
            let band = near_boundary(&labels, cw, ch);
            prev = Some((k, cw, labels, band));
            k = k.div_ceil(2);
        }
    }

    /// Min-cut over the `FREE` cells of a grid; fixed cells act as terminals
    /// through their smoothness links. Returns the label (target?) per cell.
    #[allow(clippy::too_many_arguments)]
    fn solve_grid(
        &self,
        gw: usize,
        gh: usize,
        state: &[u8],
        lab: impl Fn(usize) -> [f32; 3],
        brush_dist: impl Fn(usize) -> f32,
        area: impl Fn(usize) -> f32,
        edge_len: f32,
        target_model: &ColorModel,
        hard_radius: f32,
    ) -> Vec<bool> {
        let n = gw * gh;
        let mut node = vec![NO_NODE; n];
        let mut cells: Vec<u32> = Vec::new();
        for (c, &s) in state.iter().enumerate() {
            if s == FREE {
                node[c] = cells.len() as u32;
                cells.push(c as u32);
            }
        }
        let mut labels: Vec<bool> = state.iter().map(|&s| s == FIX_TARGET).collect();
        if cells.is_empty() {
            return labels;
        }

        let mut mf = Maxflow::new(cells.len());
        for (v, &c) in cells.iter().enumerate() {
            let c = c as usize;
            let (cx, cy) = ((c % gw) as i32, (c / gw) as i32);
            let lab_c = lab(c);
            let d = brush_dist(c);
            if d <= hard_radius {
                mf.tr[v] = HARD;
            } else {
                let (ct, co) = self.unary(target_model, lab_c, d);
                let limit = (HARD / 4) as f32;
                mf.tr[v] += ((co - ct) * area(c) * SCALE).clamp(-limit, limit) as i32;
            }
            for (dir, &(dx, dy)) in DIRS.iter().enumerate() {
                let nx = cx + dx;
                let ny = cy + dy;
                if nx < 0 || ny < 0 || nx >= gw as i32 || ny >= gh as i32 {
                    continue;
                }
                let nc = ny as usize * gw + nx as usize;
                let wgt = self.pair_weight(lab_c, lab(nc), dir & 1 == 1) * edge_len * SCALE;
                let wgt = wgt.max(1.0) as i32;
                match state[nc] {
                    FREE => {
                        if dir < 4 {
                            mf.link(v, dir, node[nc] as usize, wgt);
                        }
                    }
                    FIX_TARGET => mf.tr[v] = mf.tr[v].saturating_add(wgt),
                    _ => mf.tr[v] = mf.tr[v].saturating_sub(wgt),
                }
            }
        }
        mf.solve();
        for (v, &c) in cells.iter().enumerate() {
            labels[c as usize] = mf.is_source(v);
        }
        labels
    }

    /// New pixels of the cut that connect to the brush — through other new
    /// pixels, or through selected ones under the brush — so the stroke never
    /// jumps to a separate area. Also reports whether that region runs into a
    /// side of `roi` that is not the image edge.
    fn connected_to_brush(
        &self,
        cache: &EdgeCache,
        mask: &[u8],
        brush: &BrushPath,
        roi: (usize, usize, usize, usize),
        labels: &[bool],
    ) -> (Vec<bool>, bool) {
        let w = cache.width as usize;
        let h = cache.height as usize;
        let (x0, y0, x1, y1) = roi;
        let rw = x1 - x0;
        let rh = y1 - y0;
        let pix = |q: usize| (y0 + q / rw) * w + x0 + q % rw;
        let d_at = |q: usize| brush.dist((x0 + q % rw) as f32 + 0.5, (y0 + q / rw) as f32 + 0.5);
        let under_brush =
            |q: usize| brush.covers((x0 + q % rw) as f32 + 0.5, (y0 + q / rw) as f32 + 0.5);
        let passable = |q: usize| labels[q] && (!self.is_target(mask[pix(q)]) || under_brush(q));

        let mut seen = vec![false; rw * rh];
        let mut queue: VecDeque<u32> = VecDeque::new();
        let bx0 = (brush.bbox.0.floor().max(x0 as f32) as usize).min(x1);
        let by0 = (brush.bbox.1.floor().max(y0 as f32) as usize).min(y1);
        let bx1 = (brush.bbox.2.ceil().max(0.0) as usize).clamp(x0, x1);
        let by1 = (brush.bbox.3.ceil().max(0.0) as usize).clamp(y0, y1);
        for y in by0..by1 {
            for x in bx0..bx1 {
                let q = (y - y0) * rw + (x - x0);
                if !labels[q] {
                    continue;
                }
                let d = d_at(q);
                if d <= brush.core || (self.is_target(mask[pix(q)]) && d <= brush.radius) {
                    seen[q] = true;
                    queue.push_back(q as u32);
                }
            }
        }

        let mut open = false;
        while let Some(q) = queue.pop_front() {
            let q = q as usize;
            let (qx, qy) = (q % rw, q / rw);
            if !open && !self.is_target(mask[pix(q)]) {
                open = (qx == 0 && x0 > 0)
                    || (qx + 1 == rw && x1 < w)
                    || (qy == 0 && y0 > 0)
                    || (qy + 1 == rh && y1 < h);
            }
            for &(dx, dy) in &DIRS {
                let nx = qx as i32 + dx;
                let ny = qy as i32 + dy;
                if nx < 0 || ny < 0 || nx >= rw as i32 || ny >= rh as i32 {
                    continue;
                }
                let nq = ny as usize * rw + nx as usize;
                if !seen[nq] && passable(nq) {
                    seen[nq] = true;
                    queue.push_back(nq as u32);
                }
            }
        }
        (seen, open)
    }

    /// Write the kept pixels; returns the changed bounds.
    fn write(
        &self,
        cache: &EdgeCache,
        mask: &mut [u8],
        roi: (usize, usize, usize, usize),
        keep: &[bool],
    ) -> Option<(usize, usize, usize, usize)> {
        let w = cache.width as usize;
        let (x0, y0, x1, _) = roi;
        let rw = x1 - x0;
        let value = if self.op == QuickSelectOp::Add {
            255
        } else {
            0
        };
        let mut changed: Option<(usize, usize, usize, usize)> = None;
        for (q, _) in keep.iter().enumerate().filter(|(_, &k)| k) {
            let (x, y) = (x0 + q % rw, y0 + q / rw);
            let i = y * w + x;
            if self.is_target(mask[i]) {
                continue;
            }
            mask[i] = value;
            changed = Some(match changed {
                None => (x, y, x + 1, y + 1),
                Some((a, b, c, d)) => (a.min(x), b.min(y), c.max(x + 1), d.max(y + 1)),
            });
        }
        changed
    }
}

/// Auto-Enhance (the Quick Selection option): tidy the boundary a stroke
/// left — the cut's staircase steps and single-pixel jaggies are smoothed
/// away, and the rim settles onto the image edge with a soft, anti-aliased
/// falloff (an edge-aware guided filter on the luminance). Works on `bounds`
/// (the pixels the stroke changed) plus a small margin and returns the box
/// it rewrote, or None when nothing changed.
pub fn auto_enhance(
    mask: &mut [u8],
    cache: &EdgeCache,
    bounds: (usize, usize, usize, usize),
) -> Option<(usize, usize, usize, usize)> {
    /// Smoothing kernel radius (Gaussian, sigma 1.2).
    const SMOOTH_R: usize = 3;
    /// Guided-filter window radius and regulariser (luminance in 0..1).
    const GUIDE_R: usize = 2;
    const GUIDE_EPS: f32 = 0.0008;
    /// Pixels outside the stroke's box that may still change (its rim).
    const WRITE_PAD: usize = 4;
    /// Work margin beyond that, so the filters see real neighbours.
    const WORK_PAD: usize = WRITE_PAD + SMOOTH_R + 2 * GUIDE_R + 2;

    let (w, h) = (cache.width as usize, cache.height as usize);
    let (bx0, by0, bx1, by1) = bounds;
    if w == 0 || h == 0 || bx1 <= bx0 || by1 <= by0 || mask.len() < w * h {
        return None;
    }
    let (x0, y0) = (bx0.saturating_sub(WORK_PAD), by0.saturating_sub(WORK_PAD));
    let (x1, y1) = ((bx1 + WORK_PAD).min(w), (by1 + WORK_PAD).min(h));
    let (rw, rh) = (x1 - x0, y1 - y0);
    let n = rw * rh;

    // 1. Smooth the binary cut and re-threshold: rounds off steps and jaggies.
    let binary: Vec<f32> = (0..n)
        .map(|q| {
            let i = (y0 + q / rw) * w + x0 + q % rw;
            if mask[i] >= 128 {
                1.0
            } else {
                0.0
            }
        })
        .collect();
    let kernel: Vec<f32> = {
        let k: Vec<f32> = (0..=2 * SMOOTH_R)
            .map(|i| {
                let d = i as f32 - SMOOTH_R as f32;
                (-d * d / (2.0 * 1.2 * 1.2)).exp()
            })
            .collect();
        let sum: f32 = k.iter().sum();
        k.into_iter().map(|v| v / sum).collect()
    };
    let smoothed = separable_filter(&binary, rw, rh, &kernel);
    let p: Vec<f32> = smoothed
        .iter()
        .map(|&v| if v >= 0.5 { 1.0 } else { 0.0 })
        .collect();

    // 2. Guided filter with the luminance as guide: the rim follows edges in
    //    the photo and gets a natural anti-aliased falloff.
    let guide: Vec<f32> = (0..n)
        .map(|q| cache.lab[(y0 + q / rw) * w + x0 + q % rw][0] / 100.0)
        .collect();
    let mean_i = box_mean(&guide, rw, rh, GUIDE_R);
    let mean_p = box_mean(&p, rw, rh, GUIDE_R);
    let ii: Vec<f32> = guide.iter().map(|&v| v * v).collect();
    let ip: Vec<f32> = guide.iter().zip(&p).map(|(&g, &v)| g * v).collect();
    let corr_ii = box_mean(&ii, rw, rh, GUIDE_R);
    let corr_ip = box_mean(&ip, rw, rh, GUIDE_R);
    let mut a = vec![0f32; n];
    let mut b = vec![0f32; n];
    for q in 0..n {
        let var = corr_ii[q] - mean_i[q] * mean_i[q];
        let cov = corr_ip[q] - mean_i[q] * mean_p[q];
        a[q] = cov / (var + GUIDE_EPS);
        b[q] = mean_p[q] - a[q] * mean_i[q];
    }
    let mean_a = box_mean(&a, rw, rh, GUIDE_R);
    let mean_b = box_mean(&b, rw, rh, GUIDE_R);

    // 3. Firm the result up (Photoshop's Contrast) and write back the rim.
    let (wx0, wy0) = (bx0.saturating_sub(WRITE_PAD), by0.saturating_sub(WRITE_PAD));
    let (wx1, wy1) = ((bx1 + WRITE_PAD).min(w), (by1 + WRITE_PAD).min(h));
    let mut changed: Option<(usize, usize, usize, usize)> = None;
    for y in wy0..wy1 {
        for x in wx0..wx1 {
            let q = (y - y0) * rw + (x - x0);
            let alpha = (mean_a[q] * guide[q] + mean_b[q]).clamp(0.0, 1.0);
            let t = ((alpha - 0.15) / 0.7).clamp(0.0, 1.0);
            let v = (t * t * (3.0 - 2.0 * t) * 255.0).round() as u8;
            let i = y * w + x;
            if mask[i] != v {
                mask[i] = v;
                changed = Some(match changed {
                    None => (x, y, x + 1, y + 1),
                    Some((a0, b0, a1, b1)) => (a0.min(x), b0.min(y), a1.max(x + 1), b1.max(y + 1)),
                });
            }
        }
    }
    changed
}

/// Convolve rows then columns with a symmetric kernel (edges clamped).
fn separable_filter(src: &[f32], w: usize, h: usize, kernel: &[f32]) -> Vec<f32> {
    let r = kernel.len() / 2;
    let mut tmp = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (k, &kv) in kernel.iter().enumerate() {
                let xx = (x + k).saturating_sub(r).min(w - 1);
                acc += src[y * w + xx] * kv;
            }
            tmp[y * w + x] = acc;
        }
    }
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (k, &kv) in kernel.iter().enumerate() {
                let yy = (y + k).saturating_sub(r).min(h - 1);
                acc += tmp[yy * w + x] * kv;
            }
            out[y * w + x] = acc;
        }
    }
    out
}

/// Mean over the `(2r+1)²` window around each pixel, windows clipped to the
/// image (summed-area table).
fn box_mean(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let iw = w + 1;
    let mut sat = vec![0f64; iw * (h + 1)];
    for y in 0..h {
        let mut row = 0f64;
        for x in 0..w {
            row += src[y * w + x] as f64;
            sat[(y + 1) * iw + x + 1] = sat[y * iw + x + 1] + row;
        }
    }
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        let (ya, yb) = (y.saturating_sub(r), (y + r + 1).min(h));
        for x in 0..w {
            let (xa, xb) = (x.saturating_sub(r), (x + r + 1).min(w));
            let sum = sat[yb * iw + xb] - sat[ya * iw + xb] - sat[yb * iw + xa] + sat[ya * iw + xa];
            out[y * w + x] = (sum / ((xb - xa) * (yb - ya)) as f64) as f32;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::selection::{compute_sobel, pixels_to_lab};

    fn picture(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> EdgeCache {
        let mut px = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let c = f(x, y);
                px[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
        EdgeCache {
            lab: pixels_to_lab(&px, w, h),
            sobel: compute_sobel(&px, w, h),
            width: w,
            height: h,
            layer_idx: 0,
            layer_revision: 0,
            sample_merged: true,
        }
    }

    /// One stroke through `points` (first = press), two points per update.
    fn stroke(
        cache: &EdgeCache,
        mask: &mut [u8],
        op: QuickSelectOp,
        radius: f32,
        points: &[(f32, f32)],
    ) {
        let mut s = QuickSelectStroke::begin(cache, mask, op, radius, points[0]);
        s.extend(cache, mask, &points[..1]);
        for chunk in points[1..].chunks(2) {
            s.extend(cache, mask, chunk);
        }
    }

    fn at(mask: &[u8], w: u32, x: u32, y: u32) -> bool {
        mask[(y * w + x) as usize] >= 128
    }

    const RED: [u8; 3] = [210, 40, 35];
    const BLUE: [u8; 3] = [30, 90, 210];

    #[test]
    fn maxflow_matches_brute_force_min_cut() {
        let mut rng = Lcg(7);
        for trial in 0..300 {
            let (gw, gh) = if trial % 2 == 0 { (3, 3) } else { (4, 3) };
            let n = gw * gh;
            let mut mf = Maxflow::new(n);
            let mut edges = Vec::new();
            for v in 0..n {
                mf.tr[v] = rng.below(41) as i32 - 20;
                let (x, y) = ((v % gw) as i32, (v / gw) as i32);
                for (d, &(dx, dy)) in DIRS.iter().enumerate().take(4) {
                    let (nx, ny) = (x + dx, y + dy);
                    if nx < 0 || ny < 0 || nx >= gw as i32 || ny >= gh as i32 {
                        continue;
                    }
                    let u = ny as usize * gw + nx as usize;
                    let c = rng.below(16) as i32;
                    mf.link(v, d, u, c);
                    edges.push((v, u, c));
                }
            }
            let tr = mf.tr.clone();
            let energy = |src: &dyn Fn(usize) -> bool| -> i64 {
                let mut e = 0i64;
                for (v, &t) in tr.iter().enumerate() {
                    if src(v) && t < 0 {
                        e += -t as i64;
                    }
                    if !src(v) && t > 0 {
                        e += t as i64;
                    }
                }
                for &(a, b, c) in &edges {
                    if src(a) != src(b) {
                        e += c as i64;
                    }
                }
                e
            };
            let best = (0u32..1 << n)
                .map(|bits| energy(&|v| bits >> v & 1 == 1))
                .min()
                .unwrap();
            mf.solve();
            let got = energy(&|v| mf.is_source(v));
            assert_eq!(got, best, "trial {trial}");
        }
    }

    #[test]
    fn click_selects_the_region_up_to_its_edge() {
        let (w, h) = (90u32, 70u32);
        let cache = picture(w, h, |x, y| {
            if (20..70).contains(&x) && (15..55).contains(&y) {
                RED
            } else {
                BLUE
            }
        });
        let mut mask = vec![0u8; (w * h) as usize];
        stroke(&cache, &mut mask, QuickSelectOp::Add, 6.0, &[(44.0, 35.0)]);
        assert!(at(&mask, w, 21, 16) && at(&mask, w, 68, 53), "whole square");
        assert!(!at(&mask, w, 18, 35) && !at(&mask, w, 72, 35), "no spill");
    }

    #[test]
    fn never_jumps_to_a_separate_region_of_the_same_colour() {
        let (w, h) = (120u32, 60u32);
        let cache = picture(w, h, |x, y| {
            let left = (10..50).contains(&x) && (10..50).contains(&y);
            let right = (58..98).contains(&x) && (10..50).contains(&y);
            if left || right {
                RED
            } else {
                BLUE
            }
        });
        let mut mask = vec![0u8; (w * h) as usize];
        stroke(
            &cache,
            &mut mask,
            QuickSelectOp::Add,
            8.0,
            &[(20.0, 30.0), (30.0, 28.0), (40.0, 32.0)],
        );
        assert!(at(&mask, w, 12, 12) && at(&mask, w, 47, 47));
        assert!(
            (58..98).all(|x| !at(&mask, w, x, 30)),
            "the right square was never painted"
        );
    }

    #[test]
    fn rejects_background_inside_a_large_brush() {
        let (w, h) = (80u32, 50u32);
        let cache = picture(w, h, |x, y| {
            if (20..=60).contains(&x) && (10..=40).contains(&y) {
                RED
            } else {
                BLUE
            }
        });
        let mut mask = vec![0u8; (w * h) as usize];
        stroke(&cache, &mut mask, QuickSelectOp::Add, 14.0, &[(23.0, 25.0)]);
        assert!(at(&mask, w, 40, 25));
        assert!(!at(&mask, w, 12, 25), "background under the brush rim");
        assert!(!at(&mask, w, 5, 25));
    }

    #[test]
    fn stops_at_a_same_luma_colour_edge() {
        let (w, h) = (60u32, 36u32);
        let cache = picture(
            w,
            h,
            |x, _| {
                if x < 30 {
                    [180, 50, 50]
                } else {
                    [50, 115, 50]
                }
            },
        );
        let mut mask = vec![0u8; (w * h) as usize];
        stroke(&cache, &mut mask, QuickSelectOp::Add, 12.0, &[(18.0, 18.0)]);
        assert!(at(&mask, w, 18, 18) && at(&mask, w, 28, 18));
        assert!(!at(&mask, w, 32, 18) && !at(&mask, w, 42, 18));
    }

    #[test]
    fn fills_small_features_inside_the_painted_area() {
        let (w, h) = (96u32, 72u32);
        let cache = picture(w, h, |x, y| {
            let face = (24..=72).contains(&x) && (10..=62).contains(&y);
            let eye = ((34..=42).contains(&x) || (54..=62).contains(&x)) && (28..=34).contains(&y);
            let mouth = (40..=56).contains(&x) && (48..=53).contains(&y);
            if eye || mouth {
                [35, 22, 18]
            } else if face {
                [190, 118, 82]
            } else {
                [35, 95, 210]
            }
        });
        let mut mask = vec![0u8; (w * h) as usize];
        stroke(&cache, &mut mask, QuickSelectOp::Add, 22.0, &[(48.0, 36.0)]);
        assert!(at(&mask, w, 38, 31) && at(&mask, w, 48, 50));
        assert!(!at(&mask, w, 8, 36));
    }

    #[test]
    fn does_not_take_white_background_next_to_dark_hair() {
        let (w, h) = (96u32, 72u32);
        let cache = picture(w, h, |x, y| {
            let hair = (34..=58).contains(&x) && (5..=66).contains(&y);
            let skin = x < 34 && (8..=66).contains(&y);
            if hair {
                [18, 17, 20]
            } else if skin {
                [218, 156, 132]
            } else {
                [248, 248, 246]
            }
        });
        let mut mask = vec![0u8; (w * h) as usize];
        stroke(&cache, &mut mask, QuickSelectOp::Add, 18.0, &[(54.0, 34.0)]);
        assert!(at(&mask, w, 48, 34));
        assert!(!at(&mask, w, 62, 34) && !at(&mask, w, 72, 34));
    }

    #[test]
    fn subtract_follows_edges_too() {
        let (w, h) = (90u32, 70u32);
        let cache = picture(w, h, |x, y| {
            if (20..70).contains(&x) && (15..55).contains(&y) {
                RED
            } else {
                BLUE
            }
        });
        let mut mask = vec![255u8; (w * h) as usize];
        stroke(
            &cache,
            &mut mask,
            QuickSelectOp::Subtract,
            6.0,
            &[(44.0, 35.0)],
        );
        assert!(
            !at(&mask, w, 21, 16) && !at(&mask, w, 68, 53),
            "square removed"
        );
        assert!(
            at(&mask, w, 18, 35) && at(&mask, w, 5, 5),
            "background kept"
        );
    }

    #[test]
    fn a_drag_extends_the_selection_along_the_object() {
        let (w, h) = (400u32, 80u32);
        let cache = picture(w, h, |x, y| {
            if (10..390).contains(&x) && (30..50).contains(&y) {
                RED
            } else {
                BLUE
            }
        });
        let mut mask = vec![0u8; (w * h) as usize];
        let path: Vec<(f32, f32)> = (0..=30).map(|i| (20.0 + i as f32 * 12.0, 40.0)).collect();
        stroke(&cache, &mut mask, QuickSelectOp::Add, 5.0, &path);
        assert!((12..388).all(|x| at(&mask, w, x, 31) && at(&mask, w, x, 48)));
        assert!((0..w).all(|x| !at(&mask, w, x, 27) && !at(&mask, w, x, 52)));
    }

    #[test]
    fn large_regions_are_cut_coarse_then_refined_at_full_resolution() {
        let (w, h) = (900u32, 700u32);
        let cache = picture(w, h, |x, y| {
            if (200..700).contains(&x) && (150..550).contains(&y) {
                RED
            } else {
                BLUE
            }
        });
        let mut mask = vec![0u8; (w * h) as usize];
        stroke(
            &cache,
            &mut mask,
            QuickSelectOp::Add,
            120.0,
            &[(450.0, 350.0)],
        );
        for &(x, y) in &[(200, 350), (699, 350), (450, 150), (450, 549), (201, 151)] {
            assert!(at(&mask, w, x, y), "inside edge at ({x},{y})");
        }
        for &(x, y) in &[(199, 350), (700, 350), (450, 149), (450, 550)] {
            assert!(!at(&mask, w, x, y), "outside edge at ({x},{y})");
        }
    }
    #[test]
    fn auto_enhance_snaps_a_jagged_rim_onto_the_image_edge() {
        // Dark left half, bright right half; the edge is at x = 60.
        let (w, h) = (120u32, 80u32);
        let cache = picture(w, h, |x, _| {
            if x < 60 {
                [40, 40, 40]
            } else {
                [220, 220, 220]
            }
        });
        // A cut of the dark half with a ragged 2-3 px staircase rim.
        let mut mask = vec![0u8; (w * h) as usize];
        for y in 0..h {
            let rim = 57 + (y % 5) as u32;
            for x in 0..rim {
                mask[(y * w + x) as usize] = 255;
            }
        }
        let changed = auto_enhance(&mut mask, &cache, (0, 0, w as usize, h as usize));
        assert!(changed.is_some());
        for y in 10..h - 10 {
            // Solidly inside and outside stay put…
            assert_eq!(mask[(y * w + 50) as usize], 255, "row {y}");
            assert_eq!(mask[(y * w + 70) as usize], 0, "row {y}");
            // …and the 50 % contour now sits on the edge, straight.
            let rim = (0..w).find(|&x| mask[(y * w + x) as usize] < 128).unwrap();
            assert!((59..=61).contains(&rim), "row {y}: rim at {rim}");
        }
    }

    #[test]
    fn auto_enhance_is_stable_on_a_clean_selection() {
        let (w, h) = (90u32, 90u32);
        let cache = picture(w, h, |x, y| {
            let d = ((x as f32 - 45.0).powi(2) + (y as f32 - 45.0).powi(2)).sqrt();
            if d < 25.0 {
                [200, 60, 60]
            } else {
                [40, 90, 160]
            }
        });
        let mut mask = vec![0u8; (w * h) as usize];
        stroke(
            &cache,
            &mut mask,
            QuickSelectOp::Add,
            6.0,
            &[(45.0, 45.0), (50.0, 45.0)],
        );
        let bounds = (0, 0, w as usize, h as usize);
        auto_enhance(&mut mask, &cache, bounds);
        let once = mask.clone();
        auto_enhance(&mut mask, &cache, bounds);
        let drift = once
            .iter()
            .zip(&mask)
            .filter(|(a, b)| (**a as i32 - **b as i32).abs() > 8)
            .count();
        assert!(drift < 10, "a second pass moved {drift} px");
        // The disc is still selected and its surroundings are not.
        assert!(at(&mask, w, 45, 45) && !at(&mask, w, 5, 5));
    }
}

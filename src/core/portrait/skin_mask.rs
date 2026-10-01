//! Skin mask from the photo's own colours. Skin sampled around face-mesh
//! landmarks (cheeks, nose, chin, a bare forehead) trains a colour model over
//! Lab a*b*, where skin keeps nearly the same hue and chroma from shadow to
//! shine; a contrast-sensitive graph cut (the Smart Select max-flow), solved
//! coarse to fine, then takes the skin connected to those samples, and single
//! brow hairs and lashes leave it afterwards. The part model only fences the
//! area in and tips undecided pixels, so a forehead it reads as hair under a
//! fringe is still skin when it has skin's colour.

use std::collections::VecDeque;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;

use super::analysis::{guided, luma, side_fade, smoothstep};
use super::blur::blur4;
use super::geometry::*;
use crate::core::ai::body_parts::{self, PartLabels};
use crate::core::quick_select::{Maxflow, DIRS};

/// Scale of L* against a* and b* in the non-skin model.
const LUMA_WEIGHT: f32 = 0.5;
/// Largest colour evidence (nats) either way for one pixel.
const MAX_EVIDENCE: f32 = 8.0;
/// Pull (nats) toward the part model's reading at full confidence.
const PRIOR: f32 = 1.5;
/// Smoothness weight: nats per pixel of boundary between like colours.
const LAMBDA: f32 = 6.0;
const SCALE: f32 = 64.0;
const HARD: i32 = 1 << 26;
/// Part-model skin odds (face plus body) above which the colour cut may take
/// skin there; the fence grows this by about e/25.
pub(super) const SEEN_SKIN: f32 = 0.35;
/// Part-model skin odds that keep skin apart from the face's (past a strap).
const SURE_SKIN: f32 = 0.8;
/// Most cells in the first, coarsest cut.
const TOP_CELLS: usize = 60_000;
/// Cells on each side of a coarser boundary that the next level re-solves.
const BAND: usize = 2;
/// Chroma noise floor (standard deviation, a*b* units) of the skin model.
const SKIN_FLOOR: f32 = 2.0;
/// Extra spread along each skin cluster's own chroma direction, as a share
/// of its chroma: skin keeps its hue but loses some chroma in shine, and much
/// more in deep shadow (below L* 15..35).
const CHROMA_SLACK_LIT: f32 = 0.2;
const CHROMA_SLACK_DARK: f32 = 0.45;
/// Feature noise floor of the non-skin clusters.
const OTHER_FLOOR: f32 = 3.0;
const OTHER_CLUSTERS: usize = 10;
const CLASS_SAMPLES: usize = 1500;
/// -2 ln density of a colour no model explains (uniform over the feature box
/// a* -60..80, b* -60..90, weighted L* 0..50, weight 0.1): the non-skin
/// side's floor.
const OUTLIER_NLL: f32 = 32.3;

const FREE: u8 = 0;
const FIX_SKIN: u8 = 1;
const FIX_OTHER: u8 = 2;

/// Bare-skin landmarks on nearly every face: cheeks, nose bridge, chin, with
/// sample radii in face extents.
const SEEDS: [(u16, f32); 9] = [
    (50, 0.045),
    (280, 0.045),
    (205, 0.03),
    (425, 0.03),
    (123, 0.03),
    (352, 0.03),
    (197, 0.015),
    (195, 0.015),
    (199, 0.022),
];
/// Forehead samples, used only where no fringe covers them.
const FOREHEAD: [(u16, f32); 4] = [(151, 0.04), (108, 0.03), (337, 0.03), (9, 0.018)];

#[cfg(test)]
static LEGACY: AtomicBool = AtomicBool::new(false);

/// Probe switch (tests only): the part model's own skin mask instead, for
/// comparison.
#[cfg(test)]
pub(super) fn set_legacy(on: bool) {
    LEGACY.store(on, Ordering::Relaxed);
}

pub(super) fn legacy() -> bool {
    #[cfg(test)]
    return LEGACY.load(Ordering::Relaxed);
    #[cfg(not(test))]
    false
}

pub(super) struct SkinInputs<'a> {
    /// The skin region of the photo, 0..1.
    pub src: &'a [[f32; 3]],
    pub region: Region,
    /// The face's own region inside it (eyes, brows, mouth).
    pub face: Region,
    pub extent: f32,
    pub points: &'a [[f32; 3]],
    /// Part labels, only when they agree with the mesh.
    pub parts: Option<&'a PartLabels>,
    /// Every face's centre and extent; pixels nearer another face are its.
    pub owners: &'a [([f32; 2], f32)],
    pub index: usize,
    pub open_sides: [bool; 4],
}

/// sRGB (0..1) to [a*, b*, L*] (D65).
fn features(c: [f32; 3]) -> [f32; 3] {
    let linear = c.map(|v| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    });
    let f = |t: f32| {
        if t > 0.008856 {
            t.cbrt()
        } else {
            7.787 * t + 16.0 / 116.0
        }
    };
    let [r, g, b] = linear;
    let fx = f((0.4124564 * r + 0.3575761 * g + 0.1804375 * b) / 0.95047);
    let fy = f(0.2126729 * r + 0.7151522 * g + 0.0721750 * b);
    let fz = f((0.0193339 * r + 0.1191920 * g + 0.9503041 * b) / 1.08883);
    [500.0 * (fx - fy), 200.0 * (fy - fz), 116.0 * fy - 16.0]
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

    fn unit(&mut self) -> f32 {
        (self.next() & 0xFF_FFFF) as f32 / 16_777_216.0
    }
}

fn dist2(a: [f32; 3], b: [f32; 3]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

/// k-means with k-means++ seeding; returns the centres and each sample's
/// cluster.
fn kmeans(samples: &[[f32; 3]], k: usize) -> (Vec<[f32; 3]>, Vec<usize>) {
    let mut rng = Lcg(0x5EED_5EED);
    let mut centres = vec![samples[samples.len() / 2]];
    let mut near: Vec<f32> = samples.iter().map(|s| dist2(*s, centres[0])).collect();
    while centres.len() < k.min(samples.len()) {
        let total: f32 = near.iter().sum();
        if total <= 1e-9 {
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
        centres.push(samples[pick]);
        for (i, s) in samples.iter().enumerate() {
            near[i] = near[i].min(dist2(*s, samples[pick]));
        }
    }
    let nearest = |s: [f32; 3], centres: &[[f32; 3]]| {
        (0..centres.len())
            .min_by(|&a, &b| dist2(s, centres[a]).total_cmp(&dist2(s, centres[b])))
            .unwrap_or(0)
    };
    let mut owner = vec![0usize; samples.len()];
    for _ in 0..10 {
        for (o, s) in owner.iter_mut().zip(samples) {
            *o = nearest(*s, &centres);
        }
        let mut sum = vec![[0.0f32; 4]; centres.len()];
        for (&o, s) in owner.iter().zip(samples) {
            for c in 0..3 {
                sum[o][c] += s[c];
            }
            sum[o][3] += 1.0;
        }
        for (centre, s) in centres.iter_mut().zip(&sum) {
            if s[3] > 0.0 {
                *centre = [s[0] / s[3], s[1] / s[3], s[2] / s[3]];
            }
        }
    }
    (centres, owner)
}

/// One skin chroma cluster in its own frame: `dir` points along the mean
/// chroma, `cov` is (along², along·across, across²) and `cost` the -2 ln
/// density terms besides the covariance.
struct Chroma {
    mean: [f32; 2],
    dir: [f32; 2],
    chroma: f32,
    cov: [f32; 3],
    cost: f32,
}

impl Chroma {
    /// Squared Mahalanobis distance and ln det of the covariance, which
    /// widens along the mean chroma toward deep shadow.
    fn distance(&self, f: [f32; 3]) -> (f32, f32) {
        let (da, db) = (f[0] - self.mean[0], f[1] - self.mean[1]);
        let along = da * self.dir[0] + db * self.dir[1];
        let across = db * self.dir[0] - da * self.dir[1];
        let share = CHROMA_SLACK_LIT
            + (CHROMA_SLACK_DARK - CHROMA_SLACK_LIT) * (1.0 - smoothstep(15.0, 35.0, f[2]));
        let aa = self.cov[0] + (share * self.chroma).powi(2);
        let det = (aa * self.cov[2] - self.cov[1] * self.cov[1]).max(1e-6);
        let d2 = (self.cov[2] * along * along - 2.0 * self.cov[1] * along * across
            + aa * across * across)
            / det;
        (d2, det.ln())
    }
}

/// Skin colour: a full-covariance Gaussian mixture over a*b*, with any
/// brightness from deep shadow up allowed.
struct SkinModel {
    parts: Vec<Chroma>,
    /// Darkest L* still fully skin; darker fades out.
    low: f32,
    /// L* most skin samples are brighter than.
    typical: f32,
    /// -2 ln of the flat luma density.
    spread: f32,
}

impl SkinModel {
    fn fit(samples: &[[f32; 3]]) -> Option<Self> {
        if samples.len() < 40 {
            return None;
        }
        let n = samples.len();
        let k = if n >= 600 {
            3
        } else if n >= 150 {
            2
        } else {
            1
        };
        let chroma: Vec<[f32; 3]> = samples.iter().map(|s| [s[0], s[1], 0.0]).collect();
        let (centres, owner) = kmeans(&chroma, k);
        let k = centres.len();
        let floor = SKIN_FLOOR * SKIN_FLOOR;
        // (weight, mean, covariance [uu, uv, vv]) per cluster.
        let mut comps: Vec<(f32, [f32; 2], [f32; 3])> = (0..k)
            .map(|j| {
                let members: Vec<&[f32; 3]> = chroma
                    .iter()
                    .zip(&owner)
                    .filter(|(_, &o)| o == j)
                    .map(|(s, _)| s)
                    .collect();
                let m = members.len().max(1) as f32;
                let mean = [
                    members.iter().map(|s| s[0]).sum::<f32>() / m,
                    members.iter().map(|s| s[1]).sum::<f32>() / m,
                ];
                let cov = [
                    members
                        .iter()
                        .map(|s| (s[0] - mean[0]).powi(2))
                        .sum::<f32>()
                        / m
                        + floor,
                    members
                        .iter()
                        .map(|s| (s[0] - mean[0]) * (s[1] - mean[1]))
                        .sum::<f32>()
                        / m,
                    members
                        .iter()
                        .map(|s| (s[1] - mean[1]).powi(2))
                        .sum::<f32>()
                        / m
                        + floor,
                ];
                (members.len() as f32 / n as f32, mean, cov)
            })
            .filter(|c| c.0 > 0.0)
            .collect();
        let log_density = |c: &(f32, [f32; 2], [f32; 3]), s: &[f32; 3]| {
            let det = (c.2[0] * c.2[2] - c.2[1] * c.2[1]).max(1e-12);
            let (du, dv) = (s[0] - c.1[0], s[1] - c.1[1]);
            let d2 = (c.2[2] * du * du - 2.0 * c.2[1] * du * dv + c.2[0] * dv * dv) / det;
            c.0.max(1e-6).ln() - 0.5 * (d2 + det.ln())
        };
        for _ in 0..12 {
            let mut acc = vec![[0.0f64; 6]; comps.len()];
            for s in &chroma {
                let logs: Vec<f32> = comps.iter().map(|c| log_density(c, s)).collect();
                let top = logs.iter().copied().fold(f32::MIN, f32::max);
                let total: f32 = logs.iter().map(|l| (l - top).exp()).sum();
                for (j, l) in logs.iter().enumerate() {
                    let r = ((l - top).exp() / total) as f64;
                    let (u, v) = (s[0] as f64, s[1] as f64);
                    let a = &mut acc[j];
                    a[0] += r;
                    a[1] += r * u;
                    a[2] += r * v;
                    a[3] += r * u * u;
                    a[4] += r * u * v;
                    a[5] += r * v * v;
                }
            }
            for (c, a) in comps.iter_mut().zip(&acc) {
                if a[0] < 1e-3 {
                    c.0 = 0.0;
                    continue;
                }
                let (mu, mv) = (a[1] / a[0], a[2] / a[0]);
                *c = (
                    (a[0] / n as f64) as f32,
                    [mu as f32, mv as f32],
                    [
                        (a[3] / a[0] - mu * mu) as f32 + floor,
                        (a[4] / a[0] - mu * mv) as f32,
                        (a[5] / a[0] - mv * mv) as f32 + floor,
                    ],
                );
            }
            comps.retain(|c| c.0 > 0.0);
        }
        let parts = comps
            .iter()
            .map(|(weight, mean, cov)| {
                let chroma = mean[0].hypot(mean[1]).max(1e-3);
                let dir = [mean[0] / chroma, mean[1] / chroma];
                let across = [-dir[1], dir[0]];
                let form = |x: [f32; 2], y: [f32; 2]| {
                    x[0] * (cov[0] * y[0] + cov[1] * y[1]) + x[1] * (cov[1] * y[0] + cov[2] * y[1])
                };
                Chroma {
                    mean: *mean,
                    dir,
                    chroma,
                    cov: [form(dir, dir), form(dir, across), form(across, across)],
                    cost: 2.0 * std::f32::consts::TAU.ln() - 2.0 * weight.max(0.02).ln(),
                }
            })
            .collect();
        let mut lumas: Vec<f32> = samples.iter().map(|s| s[2]).collect();
        lumas.sort_by(f32::total_cmp);
        let low = 0.5 * lumas[lumas.len() / 20];
        Some(Self {
            parts,
            low,
            typical: lumas[lumas.len() / 10],
            spread: 2.0 * (LUMA_WEIGHT * (100.0 - low).max(10.0)).ln(),
        })
    }

    /// Squared Mahalanobis distance to the nearest cluster.
    fn chroma_distance(&self, f: [f32; 3]) -> f32 {
        self.parts
            .iter()
            .map(|g| g.distance(f).0)
            .fold(f32::MAX, f32::min)
    }

    fn shade_penalty(&self, l: f32) -> f32 {
        if l < self.low {
            ((self.low - l) / (0.3 * self.low.max(1e-3))).powi(2)
        } else {
            0.0
        }
    }

    /// Whether a colour could pass for this skin (within three deviations).
    fn resembles(&self, f: [f32; 3]) -> bool {
        self.chroma_distance(f) + self.shade_penalty(f[2]) < 9.0
    }

    /// Whether a colour is plainly this skin, lit like the samples (within
    /// two deviations, not darker than most of them): dark brown hair has
    /// shaded skin's hue.
    fn matches(&self, f: [f32; 3]) -> bool {
        self.chroma_distance(f) < 4.0 && f[2] >= self.typical
    }

    /// -2 ln density.
    fn nll(&self, f: [f32; 3]) -> f32 {
        let best = self
            .parts
            .iter()
            .map(|g| {
                let (d2, log_det) = g.distance(f);
                d2 + log_det + g.cost
            })
            .fold(f32::MAX, f32::min);
        best + self.shade_penalty(f[2]) + self.spread
    }
}

struct Blob {
    mean: [f32; 3],
    inv_var: f32,
    cost: f32,
}

/// Everything that is not skin (hair, backdrop, clothes, eyes, lips): an
/// isotropic Gaussian mixture over chroma and weighted luma.
struct OtherModel {
    parts: Vec<Blob>,
}

impl OtherModel {
    fn fit(samples: &[[f32; 3]], clusters: usize) -> Self {
        if samples.is_empty() {
            return Self { parts: Vec::new() };
        }
        let (centres, owner) = kmeans(samples, clusters);
        let n = samples.len() as f32;
        let parts = centres
            .iter()
            .enumerate()
            .filter_map(|(j, centre)| {
                let members: Vec<&[f32; 3]> = samples
                    .iter()
                    .zip(&owner)
                    .filter(|(_, &o)| o == j)
                    .map(|(s, _)| s)
                    .collect();
                if members.is_empty() {
                    return None;
                }
                let var = members.iter().map(|s| dist2(**s, *centre)).sum::<f32>()
                    / (3.0 * members.len() as f32)
                    + OTHER_FLOOR * OTHER_FLOOR;
                let weight = (members.len() as f32 / n).max(0.02);
                Some(Blob {
                    mean: *centre,
                    inv_var: 1.0 / var,
                    cost: 3.0 * (std::f32::consts::TAU * var).ln() - 2.0 * weight.ln(),
                })
            })
            .collect();
        Self { parts }
    }

    /// -2 ln density, floored by the outlier level.
    fn nll(&self, f: [f32; 3]) -> f32 {
        let g = [f[0], f[1], f[2] * LUMA_WEIGHT];
        self.parts
            .iter()
            .map(|b| dist2(g, b.mean) * b.inv_var + b.cost)
            .fold(OUTLIER_NLL, f32::min)
    }
}

fn weighted(f: [f32; 3]) -> [f32; 3] {
    [f[0], f[1], f[2] * LUMA_WEIGHT]
}

/// A mesh outline with its bounds, for quick inside/outside tests.
struct Shape {
    points: Vec<[f32; 2]>,
    bounds: [f32; 4],
}

impl Shape {
    fn new(points: Vec<[f32; 2]>) -> Self {
        let mut bounds = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for p in &points {
            bounds[0] = bounds[0].min(p[0]);
            bounds[1] = bounds[1].min(p[1]);
            bounds[2] = bounds[2].max(p[0]);
            bounds[3] = bounds[3].max(p[1]);
        }
        Self { points, bounds }
    }

    /// Signed distance, positive inside; points well outside the bounds
    /// only get the (smaller) distance to the bounds, below `-margin`.
    fn depth(&self, x: f32, y: f32, margin: f32) -> f32 {
        let dx = (self.bounds[0] - x).max(x - self.bounds[2]).max(0.0);
        let dy = (self.bounds[1] - y).max(y - self.bounds[3]).max(0.0);
        let outside = dx.hypot(dy);
        if outside > margin {
            return -outside;
        }
        signed_distance(&self.points, x, y)
    }
}

/// Why a pixel can never be skin.
#[derive(Clone, Copy, PartialEq)]
enum Barrier {
    /// Nearer another face.
    Foreign,
    /// Outside the area around the face and neck.
    Outside,
    /// Inside an eye, the mouth or the core of the lips.
    Feature,
    /// Hair the part model is sure of, off the face.
    Hair,
}

struct Scene<'a> {
    input: &'a SkinInputs<'a>,
    e: f32,
    oval: Shape,
    eyes: [Shape; 2],
    brows: [Shape; 2],
    lips: Shape,
    mouth: Shape,
    /// Sample discs: centre, radius, on the forehead.
    seeds: Vec<([f32; 2], f32, bool)>,
    /// The part model's face, neck and body skin grown by about e/15, on a
    /// grid of `fence_cell` pixels over the region.
    fence: Vec<bool>,
    fence_cell: f32,
    fence_w: usize,
}

/// What the part model says at a point: skin probability, hair, lips,
/// mouth inside (teeth, tongue).
#[derive(Clone, Copy, Default)]
struct Reading {
    skin: f32,
    glasses: f32,
    hair: f32,
    lips: f32,
    mouth: f32,
    other: f32,
}

impl<'a> Scene<'a> {
    fn new(input: &'a SkinInputs<'a>) -> Self {
        let e = input.extent;
        let r = input.region;
        let shape = |ring: &[u16]| Shape::new(loop_points(input.points, ring));
        let mut seeds: Vec<([f32; 2], f32, bool)> = SEEDS
            .iter()
            .map(|&(k, radius)| (k, radius, false))
            .chain(FOREHEAD.iter().map(|&(k, radius)| (k, radius, true)))
            .map(|(k, radius, forehead)| {
                let p = input.points[k as usize];
                ([p[0], p[1]], radius * e, forehead)
            })
            .collect();
        seeds.retain(|(c, _, _)| {
            c[0] >= r.x as f32
                && c[1] >= r.y as f32
                && c[0] < (r.x + r.w) as f32
                && c[1] < (r.y + r.h) as f32
        });
        let fence_cell = (e / 100.0).max(2.0);
        let fence_w = (r.w as f32 / fence_cell).ceil() as usize;
        let fence_h = (r.h as f32 / fence_cell).ceil() as usize;
        let mut fence = vec![false; fence_w * fence_h];
        if let Some(parts) = input.parts {
            let marked: Vec<bool> = (0..fence_w * fence_h)
                .into_par_iter()
                .map(|c| {
                    let x = r.x as f32 + ((c % fence_w) as f32 + 0.5) * fence_cell;
                    let y = r.y as f32 + ((c / fence_w) as f32 + 0.5) * fence_cell;
                    let g = parts.groups_at(x, y);
                    g[body_parts::GROUP_FACE_SKIN] + g[body_parts::GROUP_BODY_SKIN] > SEEN_SKIN
                })
                .collect();
            let reach = ((e / 25.0) / fence_cell).ceil() as isize;
            fence.par_iter_mut().enumerate().for_each(|(c, cell)| {
                let (cx, cy) = ((c % fence_w) as isize, (c / fence_w) as isize);
                *cell = (-reach..=reach).any(|dy| {
                    let y = cy + dy;
                    y >= 0
                        && (y as usize) < fence_h
                        && (-reach..=reach).any(|dx| {
                            let x = cx + dx;
                            x >= 0
                                && (x as usize) < fence_w
                                && marked[y as usize * fence_w + x as usize]
                        })
                });
            });
        }
        Self {
            input,
            e,
            oval: shape(&FACE_OVAL),
            eyes: [shape(&RIGHT_EYE), shape(&LEFT_EYE)],
            brows: [shape(&RIGHT_BROW), shape(&LEFT_BROW)],
            lips: shape(&LIPS_OUTER),
            mouth: shape(&MOUTH_INNER),
            seeds,
            fence,
            fence_cell,
            fence_w,
        }
    }

    fn read(&self, x: f32, y: f32) -> Option<Reading> {
        self.input.parts.map(|p| {
            let g = p.groups_at(x, y);
            let skin = (g[body_parts::GROUP_FACE_SKIN] + g[body_parts::GROUP_BODY_SKIN]).min(1.0);
            let total: f32 = g.iter().sum();
            Reading {
                skin,
                glasses: g[body_parts::GROUP_GLASSES],
                hair: g[body_parts::GROUP_HAIR],
                lips: g[body_parts::GROUP_LIPS],
                mouth: g[body_parts::GROUP_TEETH] + g[body_parts::GROUP_TONGUE],
                other: (1.0 - total).max(0.0),
            }
        })
    }

    fn barrier(&self, x: f32, y: f32, reading: Option<Reading>) -> Option<Barrier> {
        let e = self.e;
        let input = self.input;
        let (own_centre, own_size) = input.owners[input.index];
        let own = (x - own_centre[0]).hypot(y - own_centre[1]) / own_size;
        if input
            .owners
            .iter()
            .enumerate()
            .any(|(j, (c, s))| j != input.index && (x - c[0]).hypot(y - c[1]) / s < own)
        {
            return Some(Barrier::Foreign);
        }
        let oval = self.oval.depth(x, y, 0.1 * e);
        let fx = ((x - input.region.x as f32) / self.fence_cell) as usize;
        let fy = ((y - input.region.y as f32) / self.fence_cell) as usize;
        let fenced = self
            .fence
            .get(fy * self.fence_w + fx.min(self.fence_w - 1))
            .copied()
            .unwrap_or(false);
        if !fenced && oval < -0.06 * e {
            return Some(Barrier::Outside);
        }
        if self.eyes.iter().any(|s| s.depth(x, y, 0.1 * e) > 0.003 * e)
            || self.mouth.depth(x, y, 0.1 * e) > 0.004 * e
            || self.lips.depth(x, y, 0.1 * e) > 0.012 * e
        {
            return Some(Barrier::Feature);
        }
        if let Some(r) = reading {
            if r.lips > 0.85 || r.mouth > 0.6 {
                return Some(Barrier::Feature);
            }
            if r.hair > 0.7 && oval < -0.03 * e {
                return Some(Barrier::Hair);
            }
        }
        None
    }

    /// Whether a point lies around the mouth, where lip colours compete
    /// with skin. (Eyes are fenced off and brows and lashes left to the
    /// final hair test: brow colour is often the skin's own, only darker.)
    fn at_mouth(&self, x: f32, y: f32) -> bool {
        self.lips.depth(x, y, 0.1 * self.e) > -0.03 * self.e
    }

    /// The sample disc a point lies in, if any: Some(on the forehead).
    fn seed(&self, x: f32, y: f32) -> Option<bool> {
        self.seeds
            .iter()
            .find(|(c, radius, _)| (x - c[0]).hypot(y - c[1]) <= *radius)
            .map(|s| s.2)
    }
}

/// Mean colour pyramid of the region: level j averages 2^j x 2^j pixels.
struct Pyramid<'a> {
    src: &'a [[f32; 3]],
    w: usize,
    h: usize,
    /// Levels 1.. as (width, height, rgb + pixel count).
    levels: Vec<(usize, usize, Vec<[f32; 4]>)>,
}

impl<'a> Pyramid<'a> {
    fn new(src: &'a [[f32; 3]], w: usize, h: usize, top: usize) -> Self {
        let mut levels: Vec<(usize, usize, Vec<[f32; 4]>)> = Vec::new();
        for j in 1..=top {
            let (pw, ph) = if j == 1 {
                (w, h)
            } else {
                (levels[j - 2].0, levels[j - 2].1)
            };
            let (lw, lh) = (pw.div_ceil(2), ph.div_ceil(2));
            let previous = levels.last().map(|l| &l.2);
            let data: Vec<[f32; 4]> = (0..lw * lh)
                .into_par_iter()
                .map(|c| {
                    let (cx, cy) = (c % lw, c / lw);
                    let mut acc = [0.0f32; 4];
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let (x, y) = (cx * 2 + dx, cy * 2 + dy);
                        if x >= pw || y >= ph {
                            continue;
                        }
                        let v = match previous {
                            None => {
                                let s = src[y * w + x];
                                [s[0], s[1], s[2], 1.0]
                            }
                            Some(p) => {
                                let v = p[y * pw + x];
                                [v[0] * v[3], v[1] * v[3], v[2] * v[3], v[3]]
                            }
                        };
                        for k in 0..4 {
                            acc[k] += v[k];
                        }
                    }
                    [acc[0] / acc[3], acc[1] / acc[3], acc[2] / acc[3], acc[3]]
                })
                .collect();
            levels.push((lw, lh, data));
        }
        Self { src, w, h, levels }
    }

    fn dims(&self, j: usize) -> (usize, usize) {
        if j == 0 {
            (self.w, self.h)
        } else {
            (self.levels[j - 1].0, self.levels[j - 1].1)
        }
    }

    fn colour(&self, j: usize, c: usize) -> [f32; 3] {
        if j == 0 {
            self.src[c]
        } else {
            let v = self.levels[j - 1].2[c];
            [v[0], v[1], v[2]]
        }
    }

    /// Colour for the colour models: at full resolution a 3x3 mean, since
    /// single pixels carry JPEG chroma noise.
    fn model_colour(&self, j: usize, c: usize) -> [f32; 3] {
        if j > 0 {
            return self.colour(j, c);
        }
        let (x, y) = ((c % self.w) as isize, (c / self.w) as isize);
        let mut acc = [0.0f32; 4];
        for dy in -1..=1 {
            for dx in -1..=1 {
                let (xx, yy) = (x + dx, y + dy);
                if xx < 0 || yy < 0 || xx >= self.w as isize || yy >= self.h as isize {
                    continue;
                }
                let s = self.src[yy as usize * self.w + xx as usize];
                acc[0] += s[0];
                acc[1] += s[1];
                acc[2] += s[2];
                acc[3] += 1.0;
            }
        }
        [acc[0] / acc[3], acc[1] / acc[3], acc[2] / acc[3]]
    }

    /// Pixel bounds [x0, y0, x1, y1) of cell `c` on level `j`.
    fn cell(&self, j: usize, c: usize) -> [usize; 4] {
        let (gw, _) = self.dims(j);
        let k = 1usize << j;
        let (x0, y0) = ((c % gw) * k, (c / gw) * k);
        [x0, y0, (x0 + k).min(self.w), (y0 + k).min(self.h)]
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

/// Keep only the skin connected to the `starts`.
fn connected(labels: &[bool], gw: usize, gh: usize, starts: &[usize]) -> Vec<bool> {
    let mut seen = vec![false; labels.len()];
    let mut queue: VecDeque<u32> = VecDeque::new();
    for &s in starts {
        if labels[s] && !seen[s] {
            seen[s] = true;
            queue.push_back(s as u32);
        }
    }
    while let Some(q) = queue.pop_front() {
        let (qx, qy) = ((q as usize % gw) as i32, (q as usize / gw) as i32);
        for &(dx, dy) in &DIRS {
            let (nx, ny) = (qx + dx, qy + dy);
            if nx < 0 || ny < 0 || nx >= gw as i32 || ny >= gh as i32 {
                continue;
            }
            let nq = ny as usize * gw + nx as usize;
            if labels[nq] && !seen[nq] {
                seen[nq] = true;
                queue.push_back(nq as u32);
            }
        }
    }
    seen
}

struct Models {
    skin: SkinModel,
    /// Hair, backdrop, clothes.
    other: OtherModel,
    /// Lips, teeth and mouth, only around the mouth: elsewhere a flushed
    /// cheek could pass for lips.
    mouth: OtherModel,
}

impl Models {
    /// Colour evidence plus the part model's pull, in nats; > 0 favours skin.
    fn evidence(&self, colour: [f32; 3], reading: Option<Reading>, at_mouth: bool) -> f32 {
        let f = features(colour);
        let mut other = self.other.nll(f);
        if at_mouth {
            other = other.min(self.mouth.nll(f));
        }
        let evidence = (0.5 * (other - self.skin.nll(f))).clamp(-MAX_EVIDENCE, MAX_EVIDENCE);
        // Near black or blown out, colour cannot be read; the part model
        // decides there.
        let legible = smoothstep(3.0, 15.0, f[2]) * (1.0 - smoothstep(94.0, 99.0, f[2]));
        // Skin seen through lenses is the part model's "glasses": undecided.
        evidence * legible
            + reading.map_or(0.0, |r| {
                PRIOR * (2.0 * (r.skin + 0.5 * r.glasses).min(1.0) - 1.0)
            })
    }
}

/// Train the colour models on the level-`j` cells: skin in the sample discs
/// (trimmed of odd colours such as shine, moles or stubble), everything else
/// from sure hair, the backdrop, eyes, mouth, lips and brow hairs.
fn train(scene: &Scene, pyramid: &Pyramid, j: usize) -> Option<Models> {
    let (gw, gh) = pyramid.dims(j);
    let centre = |c: usize| {
        let b = pyramid.cell(j, c);
        let r = scene.input.region;
        (
            r.x as f32 + (b[0] + b[2]) as f32 * 0.5,
            r.y as f32 + (b[1] + b[3]) as f32 * 0.5,
        )
    };
    let skin: Vec<[f32; 3]> = (0..gw * gh)
        .into_par_iter()
        .filter_map(|c| {
            let (x, y) = centre(c);
            let forehead = scene.seed(x, y)?;
            let reading = scene.read(x, y);
            if scene.barrier(x, y, reading).is_some() {
                return None;
            }
            if let Some(r) = reading {
                if (forehead && r.hair > 0.3) || r.skin < 0.4 {
                    return None;
                }
            }
            Some(features(pyramid.model_colour(j, c)))
        })
        .collect();
    if skin.len() < 40 {
        return None;
    }
    let median = |values: &mut Vec<f32>| {
        values.sort_by(f32::total_cmp);
        values[values.len() / 2]
    };
    let mut l: Vec<f32> = skin.iter().map(|f| f[2]).collect();
    let mut u: Vec<f32> = skin.iter().map(|f| f[0]).collect();
    let mut v: Vec<f32> = skin.iter().map(|f| f[1]).collect();
    let (ml, mu, mv) = (median(&mut l), median(&mut u), median(&mut v));
    let mut du: Vec<f32> = skin.iter().map(|f| (f[0] - mu).abs()).collect();
    let mut dv: Vec<f32> = skin.iter().map(|f| (f[1] - mv).abs()).collect();
    let (su, sv) = (median(&mut du) * 1.5 + 1.0, median(&mut dv) * 1.5 + 1.0);
    // Moles, stubble and shine out; shade stays in.
    let kept: Vec<[f32; 3]> = skin
        .into_iter()
        .filter(|f| {
            f[2] > 0.5 * ml
                && f[2] < 97.0
                && (f[0] - mu).abs() < 3.0 * su
                && (f[1] - mv).abs() < 3.0 * sv
        })
        .collect();
    // Hardly any colour (a black-and-white photo): nothing to go on.
    if mu.hypot(mv) < 5.0 {
        return None;
    }
    let skin = SkinModel::fit(&kept)?;

    // Mouth; hair, beyond the fence, other part-model classes.
    let mut classes: [Vec<[f32; 3]>; 4] = Default::default();
    let picked: Vec<(usize, [f32; 3])> = (0..gw * gh)
        .into_par_iter()
        .filter_map(|c| {
            let (x, y) = centre(c);
            let f = features(pyramid.model_colour(j, c));
            let reading = scene.read(x, y);
            let e = scene.e;
            match scene.barrier(x, y, reading) {
                Some(Barrier::Feature) => {
                    let eye = scene.eyes.iter().any(|s| s.depth(x, y, 0.1 * e) > 0.0);
                    return (!eye).then_some((0, f));
                }
                Some(Barrier::Hair) if !skin.matches(f) => return Some((1, f)),
                Some(Barrier::Outside) if !skin.matches(f) => return Some((2, f)),
                Some(_) => return None,
                None => {}
            }
            let r = reading?;
            if skin.matches(f) {
                None
            } else if r.hair > 0.8 {
                Some((1, f))
            } else if r.other > 0.8 {
                Some((3, f))
            } else {
                None
            }
        })
        .collect();
    for (class, f) in picked {
        classes[class].push(weighted(f));
    }
    let thin = |class: &[[f32; 3]]| -> Vec<[f32; 3]> {
        let stride = class.len().div_ceil(CLASS_SAMPLES).max(1);
        class.iter().step_by(stride).copied().collect()
    };
    let other: Vec<[f32; 3]> = classes[1..].iter().flat_map(|c| thin(c)).collect();
    Some(Models {
        skin,
        other: OtherModel::fit(&other, OTHER_CLUSTERS),
        mouth: OtherModel::fit(&thin(&classes[0]), 4),
    })
}

/// Skin mask of the face region (0..1 per pixel), or None when the photo
/// gives too little colour to model (the caller then keeps the part model's
/// mask).
pub(super) fn skin_mask(input: &SkinInputs) -> Option<Vec<f32>> {
    let r = input.region;
    let (w, h) = (r.w as usize, r.h as usize);
    let e = input.extent;
    if w < 8 || h < 8 {
        return None;
    }
    let scene = Scene::new(input);
    let mut top = 0usize;
    while w.div_ceil(1 << top) * h.div_ceil(1 << top) > TOP_CELLS {
        top += 1;
    }
    let pyramid = Pyramid::new(input.src, w, h, top);
    let sample_level = ((e / 300.0).max(1.0).log2().floor() as usize).min(top);
    let models = train(&scene, &pyramid, sample_level)?;

    let centre = |j: usize, c: usize| {
        let b = pyramid.cell(j, c);
        (
            r.x as f32 + (b[0] + b[2]) as f32 * 0.5,
            r.y as f32 + (b[1] + b[3]) as f32 * 0.5,
        )
    };
    let mut starts: Vec<usize> = Vec::new();
    // (grid width, labels, band) of the coarser level.
    let mut previous: Option<(usize, Vec<bool>, Vec<bool>)> = None;
    let mut labels = Vec::new();
    for j in (0..=top).rev() {
        let (gw, gh) = pyramid.dims(j);
        let state: Vec<u8> = (0..gw * gh)
            .into_par_iter()
            .map(|c| {
                if let Some((pw, plabels, pband)) = &previous {
                    let p = (c / gw / 2) * pw + (c % gw) / 2;
                    if !pband[p] {
                        return if plabels[p] { FIX_SKIN } else { FIX_OTHER };
                    }
                }
                let (x, y) = centre(j, c);
                let reading = scene.read(x, y);
                if scene.barrier(x, y, reading).is_some() {
                    return FIX_OTHER;
                }
                if scene.seed(x, y).is_some() {
                    let f = features(pyramid.model_colour(j, c));
                    if models.skin.resembles(f) && f[2] > 1.2 * models.skin.low {
                        return FIX_SKIN;
                    }
                }
                FREE
            })
            .collect();
        if previous.is_none() {
            starts = state
                .iter()
                .enumerate()
                .filter(|(_, &s)| s == FIX_SKIN)
                .map(|(c, _)| c)
                .collect();
            if starts.is_empty() {
                return None;
            }
        }
        labels = solve_level(&scene, &pyramid, &models, j, &state, |c| centre(j, c));
        if previous.is_none() {
            // Skin the part model is sure of stays even where clothes part it
            // from the face (an arm past a strap).
            let sure: Vec<usize> = (0..gw * gh)
                .into_par_iter()
                .filter(|&c| {
                    let (x, y) = centre(j, c);
                    labels[c] && scene.read(x, y).is_some_and(|r| r.skin > SURE_SKIN)
                })
                .collect();
            starts.extend(sure);
            labels = connected(&labels, gw, gh, &starts);
            // Start cells for the final connectivity pass, at full resolution.
            starts = starts
                .iter()
                .map(|&c| {
                    let b = pyramid.cell(j, c);
                    ((b[1] + b[3]) / 2) * w + (b[0] + b[2]) / 2
                })
                .collect();
        }
        if j == 0 {
            break;
        }
        let band = near_boundary(&labels, gw, gh);
        previous = Some((gw, labels.clone(), band));
    }
    let mut labels = if top > 0 {
        connected(&labels, w, h, &starts)
    } else {
        labels
    };
    fill_holes(&mut labels, &scene, (0.02 * e * e) as usize);

    // A soft edge that stays crisp on the photo's own edges (a guided
    // filter on luma) and does not wander off along bright strands; then
    // single brow hairs and lashes (thin lines darker than their immediate
    // surroundings) leave the mask, and a thin guard keeps it off the eyes.
    let guide: Vec<f32> = input.src.par_iter().map(|&c| luma(c)).collect();
    let hard: Vec<f32> = labels.par_iter().map(|&l| l as u8 as f32).collect();
    let soft = guided(&hard, &guide, w, h, (e / 100.0).max(2.0), 0.001);
    let mut local: Vec<[f32; 4]> = guide
        .par_iter()
        .zip(hard.par_iter())
        .map(|(&g, &m)| [g, m, 0.0, 0.0])
        .collect();
    blur4(&mut local, w, h, (e / 150.0).max(2.0));
    // Brows and eyes lie in the face region; their zones are kept that size.
    let face = input.face;
    let mut brow_zone = vec![0.0f32; face.len()];
    for ring in [&RIGHT_BROW[..], &LEFT_BROW[..]] {
        stamp_polygon(
            &mut brow_zone,
            face,
            &loop_points(input.points, ring),
            0.02 * e,
            0.02 * e,
        );
    }
    let rim = brow_rim(&guide, &hard, &brow_zone, r, face, e);
    drop(hard);
    let mut lash_zone = vec![0.0f32; face.len()];
    let mut eye_guard = vec![0.0f32; face.len()];
    for ring in [&RIGHT_EYE[..], &LEFT_EYE[..]] {
        let outline = loop_points(input.points, ring);
        stamp_polygon(&mut lash_zone, face, &outline, 0.012 * e, 0.01 * e);
        stamp_polygon(&mut eye_guard, face, &outline, 0.005 * e, 0.01 * e);
    }
    let fade = (0.06 * e).max(4.0);
    Some(
        (0..w * h)
            .into_par_iter()
            .map(|i| {
                let [around, near, _, _] = local[i];
                let m = soft[i] * smoothstep(0.02, 0.3, near);
                if m <= 0.0 {
                    return 0.0;
                }
                let m = m * side_fade(i, w, h, input.open_sides, fade);
                let Some(f) = face.index_at(r.x + (i % w) as u32, r.y + (i / w) as u32) else {
                    return m;
                };
                let darker = around - guide[i];
                let hairs = smoothstep(0.03, 0.12, darker).max(
                    rim.get(&i)
                        .map_or(0.0, |&skin| smoothstep(0.03, 0.1, skin - guide[i])),
                );
                let strand =
                    (brow_zone[f] * hairs).max(lash_zone[f] * smoothstep(0.04, 0.15, darker));
                m * (1.0 - strand) * (1.0 - eye_guard[f])
            })
            .collect(),
    )
}

/// Luma of the skin around the brows (not the brows themselves), for each
/// brow-zone pixel: brow hairs are what is clearly darker than it, however
/// dense the brow.
/// `guide` and `skin` lie in `region`, `zone` in the `face` region inside it;
/// the result is keyed by `region` pixel.
fn brow_rim(
    guide: &[f32],
    skin: &[f32],
    zone: &[f32],
    region: Region,
    face: Region,
    e: f32,
) -> std::collections::HashMap<usize, f32> {
    let (fw, fh) = (face.w as usize, face.h as usize);
    let (mut x0, mut y0, mut x1, mut y1) = (fw, fh, 0, 0);
    for (i, &z) in zone.iter().enumerate() {
        if z > 0.0 {
            let (x, y) = (i % fw, i / fw);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + 1);
            y1 = y1.max(y + 1);
        }
    }
    if x0 >= x1 {
        return Default::default();
    }
    let pad = (e / 12.0) as usize;
    let (x0, y0) = (x0.saturating_sub(pad), y0.saturating_sub(pad));
    let (x1, y1) = ((x1 + pad).min(fw), (y1 + pad).min(fh));
    let (cw, ch) = (x1 - x0, y1 - y0);
    let at = |k: usize| {
        let f = (y0 + k / cw) * fw + x0 + k % cw;
        (f, region.index_of(face, f))
    };
    let mut sums: Vec<[f32; 4]> = (0..cw * ch)
        .into_par_iter()
        .map(|k| {
            let (f, i) = at(k);
            let weight = skin[i] * (1.0 - zone[f]);
            [guide[i] * weight, weight, 0.0, 0.0]
        })
        .collect();
    blur4(&mut sums, cw, ch, (e / 30.0).max(3.0));
    (0..cw * ch)
        .filter_map(|k| {
            let (f, i) = at(k);
            (zone[f] > 0.0 && sums[k][1] > 1e-3).then(|| (i, sums[k][0] / sums[k][1]))
        })
        .collect()
}

/// Enclosed gaps in the skin that are no eye, brow or mouth (a colour cast
/// such as a blue shirt reflected under the chin, a mole) join the skin when
/// the part model reads skin there too, or when there is no part model.
fn fill_holes(labels: &mut [bool], scene: &Scene, max_area: usize) {
    let r = scene.input.region;
    let (w, h) = (r.w as usize, r.h as usize);
    let e = scene.e;
    let mut seen = vec![false; w * h];
    let mut queue: VecDeque<u32> = VecDeque::new();
    let mut gap: Vec<u32> = Vec::new();
    for start in 0..w * h {
        if labels[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        queue.push_back(start as u32);
        gap.clear();
        let mut open = false;
        while let Some(q) = queue.pop_front() {
            let (x, y) = (q as usize % w, q as usize / w);
            open |= x == 0 || y == 0 || x + 1 == w || y + 1 == h;
            if gap.len() <= max_area {
                gap.push(q);
            }
            for (dx, dy) in [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)] {
                let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                    continue;
                }
                let n = ny as usize * w + nx as usize;
                if !labels[n] && !seen[n] {
                    seen[n] = true;
                    queue.push_back(n as u32);
                }
            }
        }
        if open || gap.len() > max_area {
            continue;
        }
        let (mut skin, mut count) = (0.0f32, 0usize);
        let mut feature = false;
        for &q in gap.iter().step_by(4) {
            let x = r.x as f32 + (q as usize % w) as f32 + 0.5;
            let y = r.y as f32 + (q as usize / w) as f32 + 0.5;
            let reading = scene.read(x, y);
            feature |= scene.barrier(x, y, reading) == Some(Barrier::Feature)
                || scene
                    .brows
                    .iter()
                    .any(|s| s.depth(x, y, 0.1 * e) > -0.005 * e);
            skin += reading.map_or(1.0, |q| q.skin);
            count += 1;
        }
        if !feature && skin > 0.6 * count as f32 {
            for &q in &gap {
                labels[q as usize] = true;
            }
        }
    }
}

/// Min-cut of one level: free cells get colour evidence (times their area)
/// and contrast-sensitive links; fixed cells act through their links.
fn solve_level(
    scene: &Scene,
    pyramid: &Pyramid,
    models: &Models,
    j: usize,
    state: &[u8],
    centre: impl Fn(usize) -> (f32, f32) + Sync,
) -> Vec<bool> {
    let (gw, gh) = pyramid.dims(j);
    let mut labels: Vec<bool> = state.iter().map(|&s| s == FIX_SKIN).collect();
    let cells: Vec<u32> = state
        .iter()
        .enumerate()
        .filter(|(_, &s)| s == FREE)
        .map(|(c, _)| c as u32)
        .collect();
    if cells.is_empty() {
        return labels;
    }
    let mut node = vec![u32::MAX; gw * gh];
    for (v, &c) in cells.iter().enumerate() {
        node[c as usize] = v as u32;
    }
    // Contrast scale from the level's typical neighbour difference.
    let stride = (gw * gh / 40_000).max(1);
    let (sum, count) = (0..gw * gh)
        .step_by(stride)
        .filter(|&c| c % gw + 1 < gw && c / gw + 1 < gh)
        .fold((0.0f64, 0usize), |(s, n), c| {
            let a = pyramid.colour(j, c);
            let right = dist2(a, pyramid.colour(j, c + 1));
            let down = dist2(a, pyramid.colour(j, c + gw));
            (s + (right + down) as f64, n + 2)
        });
    let beta = (0.5 / (sum / count.max(1) as f64).max(1e-6)) as f32;
    let k = (1usize << j) as f32;
    let terminals: Vec<(i32, [i32; 8])> = cells
        .par_iter()
        .map(|&c| {
            let c = c as usize;
            let (x, y) = centre(c);
            let b = pyramid.cell(j, c);
            let area = ((b[2] - b[0]) * (b[3] - b[1])) as f32;
            let evidence = models.evidence(
                pyramid.model_colour(j, c),
                scene.read(x, y),
                scene.at_mouth(x, y),
            );
            let limit = (HARD / 4) as f32;
            let tr = (evidence * area * SCALE).clamp(-limit, limit) as i32;
            let colour = pyramid.colour(j, c);
            let (cx, cy) = ((c % gw) as i32, (c / gw) as i32);
            let mut links = [0i32; 8];
            for (d, &(dx, dy)) in DIRS.iter().enumerate() {
                let (nx, ny) = (cx + dx, cy + dy);
                if nx < 0 || ny < 0 || nx >= gw as i32 || ny >= gh as i32 {
                    continue;
                }
                let nc = ny as usize * gw + nx as usize;
                let diagonal = if d & 1 == 1 {
                    std::f32::consts::FRAC_1_SQRT_2
                } else {
                    1.0
                };
                let weight = LAMBDA
                    * (-beta * dist2(colour, pyramid.colour(j, nc))).exp()
                    * diagonal
                    * k
                    * SCALE;
                links[d] = weight.max(1.0) as i32;
            }
            (tr, links)
        })
        .collect();
    let mut flow = Maxflow::new(cells.len());
    for (v, &c) in cells.iter().enumerate() {
        let c = c as usize;
        let (tr, links) = terminals[v];
        flow.add_terminal(v, tr);
        let (cx, cy) = ((c % gw) as i32, (c / gw) as i32);
        for (d, &(dx, dy)) in DIRS.iter().enumerate() {
            if links[d] == 0 {
                continue;
            }
            let nc = (cy + dy) as usize * gw + (cx + dx) as usize;
            match state[nc] {
                FREE => {
                    if d < 4 {
                        flow.link(v, d, node[nc] as usize, links[d]);
                    }
                }
                FIX_SKIN => flow.add_terminal(v, links[d]),
                _ => flow.add_terminal(v, -links[d]),
            }
        }
    }
    flow.solve();
    for (v, &c) in cells.iter().enumerate() {
        labels[c as usize] = flow.is_source(v);
    }
    labels
}

/// Probe measures of a skin mask (u8 per region pixel): the share of
/// skin-coloured face pixels (inside the outline, away from eyes, brows,
/// lips and nostrils) left out, and the share of the mask on sure hair,
/// backdrop or clothes that is not skin-coloured.
#[cfg(test)]
pub(super) fn audit(input: &SkinInputs, mask: &[u8]) -> Option<(f32, f32)> {
    let r = input.region;
    let (w, h) = (r.w as usize, r.h as usize);
    let e = input.extent;
    let scene = Scene::new(input);
    let mut top = 0usize;
    while w.div_ceil(1 << top) * h.div_ceil(1 << top) > TOP_CELLS {
        top += 1;
    }
    let pyramid = Pyramid::new(input.src, w, h, top);
    let sample_level = ((e / 300.0).max(1.0).log2().floor() as usize).min(top);
    let models = train(&scene, &pyramid, sample_level)?;
    let nose: Vec<[f32; 2]> = [NOSE_WINGS[0], NOSE_WINGS[1], SUBNASALE]
        .iter()
        .map(|&k| [input.points[k as usize][0], input.points[k as usize][1]])
        .collect();
    let counts = (0..w * h)
        .into_par_iter()
        .filter(|i| (i % w) % 2 == 0 && (i / w) % 2 == 0)
        .map(|i| {
            let (x, y) = (
                r.x as f32 + (i % w) as f32 + 0.5,
                r.y as f32 + (i / w) as f32 + 0.5,
            );
            let f = features(pyramid.model_colour(0, i));
            let like = models.skin.resembles(f);
            let on = mask[i] >= 128;
            let face = scene.oval.depth(x, y, 0.1 * e) > 0.03 * e
                && scene
                    .eyes
                    .iter()
                    .all(|s| s.depth(x, y, 0.1 * e) < -0.02 * e)
                && scene
                    .brows
                    .iter()
                    .all(|s| s.depth(x, y, 0.1 * e) < -0.02 * e)
                && scene.lips.depth(x, y, 0.1 * e) < -0.012 * e
                && nose.iter().all(|p| (x - p[0]).hypot(y - p[1]) > 0.04 * e);
            let foreign = scene
                .read(x, y)
                .is_some_and(|q| q.hair > 0.7 || q.other > 0.7);
            [
                (face && like) as u64,
                (face && like && !on) as u64,
                on as u64,
                (on && !like && foreign) as u64,
            ]
        })
        .reduce(
            || [0; 4],
            |a, b| [a[0] + b[0], a[1] + b[1], a[2] + b[2], a[3] + b[3]],
        );
    Some((
        counts[1] as f32 / counts[0].max(1) as f32,
        counts[3] as f32 / counts[2].max(1) as f32,
    ))
}

/// Probe view of the colour evidence on the sampling level: grey = evidence
/// (brighter favours skin, mid grey undecided), blue = barrier, green =
/// skin seeds. Also a one-line summary of the skin model.
#[cfg(test)]
pub(super) fn evidence_map(input: &SkinInputs) -> Option<(usize, usize, Vec<u8>, String)> {
    let r = input.region;
    let (w, h) = (r.w as usize, r.h as usize);
    let e = input.extent;
    let scene = Scene::new(input);
    let mut top = 0usize;
    while w.div_ceil(1 << top) * h.div_ceil(1 << top) > TOP_CELLS {
        top += 1;
    }
    let pyramid = Pyramid::new(input.src, w, h, top);
    let j = ((e / 300.0).max(1.0).log2().floor() as usize).min(top);
    let models = train(&scene, &pyramid, j)?;
    let j = std::env::var("IAI_SKIN_EV_LEVEL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(j)
        .min(top);
    let (gw, gh) = pyramid.dims(j);
    let pixels: Vec<[u8; 3]> = (0..gw * gh)
        .into_par_iter()
        .map(|c| {
            let b = pyramid.cell(j, c);
            let (x, y) = (
                r.x as f32 + (b[0] + b[2]) as f32 * 0.5,
                r.y as f32 + (b[1] + b[3]) as f32 * 0.5,
            );
            let reading = scene.read(x, y);
            let colour = pyramid.model_colour(j, c);
            let evidence = models.evidence(colour, reading, scene.at_mouth(x, y));
            let grey = (128.0 + 15.0 * evidence).clamp(0.0, 255.0) as u8;
            if scene.barrier(x, y, reading).is_some() {
                [grey / 3, grey / 3, 160]
            } else if scene.seed(x, y).is_some()
                && models.skin.resembles(features(colour))
                && features(colour)[2] > 1.2 * models.skin.low
            {
                [grey / 3, 200, grey / 3]
            } else {
                [grey; 3]
            }
        })
        .collect();
    let summary = format!(
        "skin low {:.2} comps {:?}; other comps {}",
        models.skin.low,
        models
            .skin
            .parts
            .iter()
            .map(|g| {
                [g.mean[0], g.mean[1], g.cov[0].sqrt(), g.cov[2].sqrt()]
                    .map(|v| (v * 10.0).round() / 10.0)
            })
            .collect::<Vec<_>>(),
        models.other.parts.len()
    );
    Some((gw, gh, pixels.into_iter().flatten().collect(), summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 200;
    const H: u32 = 260;
    const SKIN: [f32; 3] = [0.85, 0.65, 0.55];

    /// Place `ring` evenly on an ellipse, clockwise from the top.
    fn ring(points: &mut [[f32; 3]], ring: &[u16], centre: [f32; 2], radius: [f32; 2]) {
        for (k, &i) in ring.iter().enumerate() {
            let a = k as f32 / ring.len() as f32 * std::f32::consts::TAU;
            points[i as usize] = [
                centre[0] + radius[0] * a.sin(),
                centre[1] - radius[1] * a.cos(),
                0.0,
            ];
        }
    }

    /// A synthetic face: skin oval on a blue backdrop, a dark fringe over the
    /// top of the forehead with bare forehead below it, sparse brows of 2 px
    /// hairs, lips, and the right half of the face in shade.
    fn face() -> (Vec<[f32; 3]>, Vec<[f32; 3]>) {
        let mut points = vec![[100.0, 130.0, 0.0]; 478];
        ring(&mut points, &FACE_OVAL, [100.0, 130.0], [70.0, 95.0]);
        ring(&mut points, &RIGHT_EYE, [70.0, 118.0], [12.0, 5.0]);
        ring(&mut points, &LEFT_EYE, [130.0, 118.0], [12.0, 5.0]);
        ring(&mut points, &RIGHT_BROW, [70.0, 101.0], [17.0, 4.0]);
        ring(&mut points, &LEFT_BROW, [130.0, 101.0], [17.0, 4.0]);
        ring(&mut points, &LIPS_OUTER, [100.0, 185.0], [22.0, 8.0]);
        ring(&mut points, &MOUTH_INNER, [100.0, 185.0], [18.0, 1.0]);
        for (i, p) in [
            (50, [65.0, 150.0]),
            (205, [75.0, 162.0]),
            (123, [55.0, 140.0]),
            (280, [135.0, 150.0]),
            (425, [125.0, 162.0]),
            (352, [145.0, 140.0]),
            (197, [100.0, 125.0]),
            (195, [100.0, 140.0]),
            (199, [100.0, 208.0]),
            (151, [100.0, 60.0]),
            (108, [82.0, 62.0]),
            (337, [118.0, 62.0]),
            (9, [100.0, 88.0]),
            (98, [90.0, 160.0]),
            (327, [110.0, 160.0]),
            (2, [100.0, 166.0]),
        ] {
            points[i] = [p[0], p[1], 0.0];
        }
        let mut noise = Lcg(7);
        let src = (0..W * H)
            .map(|i| {
                let (x, y) = ((i % W) as f32 + 0.5, (i / W) as f32 + 0.5);
                let in_oval = ((x - 100.0) / 70.0).powi(2) + ((y - 130.0) / 95.0).powi(2) < 1.0;
                let in_head = ((x - 100.0) / 86.0).powi(2) + ((y - 126.0) / 110.0).powi(2) < 1.0;
                let brow = (98.0..104.0).contains(&y)
                    && ((55.0..86.0).contains(&x) || (115.0..146.0).contains(&x))
                    && (x as u32) % 6 < 2;
                let lips = ((x - 100.0) / 20.0).powi(2) + ((y - 185.0) / 6.0).powi(2) < 1.0;
                let mut c = if in_head && y < 55.0 {
                    [0.2, 0.13, 0.1]
                } else if !in_oval {
                    [0.35, 0.45, 0.6]
                } else if brow {
                    [0.32, 0.22, 0.17]
                } else if lips {
                    [0.72, 0.38, 0.4]
                } else {
                    SKIN
                };
                if in_oval && x > 110.0 && y >= 55.0 {
                    c = c.map(|v| v * 0.55);
                }
                c.map(|v| (v + (noise.unit() - 0.5) * 0.03).clamp(0.0, 1.0))
            })
            .collect();
        (src, points)
    }

    fn inputs<'a>(
        src: &'a [[f32; 3]],
        points: &'a [[f32; 3]],
        owners: &'a [([f32; 2], f32)],
    ) -> SkinInputs<'a> {
        let region = Region {
            x: 0,
            y: 0,
            w: W,
            h: H,
        };
        SkinInputs {
            src,
            region,
            face: region,
            extent: 190.0,
            points,
            parts: None,
            owners,
            index: 0,
            open_sides: [false; 4],
        }
    }

    #[test]
    fn lab_features_of_white_and_skin() {
        let white = features([1.0, 1.0, 1.0]);
        assert!((white[2] - 100.0).abs() < 0.5 && white[0].abs() < 0.5 && white[1].abs() < 0.5);
        let skin = features(SKIN);
        assert!(skin[0] > 5.0 && skin[1] > 5.0, "{skin:?}");
    }

    #[test]
    fn takes_bare_forehead_and_shade_but_not_fringe_brows_or_lips() {
        let (src, points) = face();
        let owners = [([100.0, 130.0], 190.0)];
        let mask = skin_mask(&inputs(&src, &points, &owners)).expect("mask");
        let at = |x: u32, y: u32| mask[(y * W + x) as usize];
        assert!(
            at(100, 70) > 0.8,
            "forehead under the fringe {}",
            at(100, 70)
        );
        assert!(at(100, 45) < 0.2, "fringe {}", at(100, 45));
        assert!(at(8, 8) < 0.01, "backdrop {}", at(8, 8));
        assert!(at(65, 150) > 0.9, "lit cheek {}", at(65, 150));
        assert!(at(140, 160) > 0.8, "shaded cheek {}", at(140, 160));
        assert!(at(100, 185) < 0.1, "lips {}", at(100, 185));
        assert!(
            at(100, 196) > 0.7,
            "skin just below the lips {}",
            at(100, 196)
        );
        // Brow hairs sit at x % 6 < 2 on y 98..104.
        assert!(at(66, 100) < 0.4, "brow hair {}", at(66, 100));
        assert!(at(69, 100) > 0.5, "skin between brow hairs {}", at(69, 100));
        assert!(
            at(70, 108) > 0.8,
            "skin just below the brow {}",
            at(70, 108)
        );
    }

    #[test]
    fn a_grey_photo_keeps_the_part_model_mask() {
        let (src, points) = face();
        let grey: Vec<[f32; 3]> = src.iter().map(|&c| [luma(c); 3]).collect();
        let owners = [([100.0, 130.0], 190.0)];
        assert!(skin_mask(&inputs(&grey, &points, &owners)).is_none());
    }
}

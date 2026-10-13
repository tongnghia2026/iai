//! The Smart brush shared by Refine Selection and Chỉnh chân dung's "Tô
//! vùng": Select ▸ Color Range graded against both sides of a mask. Colours
//! are sampled around the brush from the area being painted (mask near full)
//! and from the rest (mask near empty, flat pixels only, so a loose strand is
//! not taken for backdrop), samples the two share are dropped, and each pixel
//! under the brush takes its share of the painted side by Lab distance to the
//! nearest sample of each. A faint strand comes in faint, the backdrop stays
//! out, and colours like neither side (skin beside hair) stay out too. It only
//! adds, on top of the mask the stroke began with, so painting again
//! strengthens faint strands; [`StampOp::SmartOut`] (Alt) takes out what
//! resembles the rest instead.

use rayon::prelude::*;

use super::refine::{Rect, StampOp};
use super::selection::lab_dist;

/// Colour samples taken from each side around a dab.
const MAX_SAMPLES: usize = 96;
/// Lab distance under which a sample could be either side.
const AMBIGUOUS: f32 = 10.0;

/// The photo under a mask, pixel for pixel (index `y * w + x` of the mask).
pub trait Colours: Sync {
    fn lab(&self, i: usize) -> [f32; 3];
    /// Any brightness scale; only compared between neighbours.
    fn tone(&self, i: usize) -> f32;
}

/// Lab colours already worked out for every mask pixel.
pub struct LabColours<'a>(pub &'a [[f32; 3]]);

impl Colours for LabColours<'_> {
    fn lab(&self, i: usize) -> [f32; 3] {
        self.0[i]
    }

    fn tone(&self, i: usize) -> f32 {
        self.0[i][0]
    }
}

/// Stamp one dab of `op` at mask point (cx, cy) on the `w` x `h` `mask`;
/// `start` is the mask when the stroke began (Smart samples it and builds on
/// it, so a stroke's own dabs neither feed its samples nor stack up). Returns
/// the touched area.
#[allow(clippy::too_many_arguments)]
pub fn stamp(
    mask: &mut [u8],
    start: &[u8],
    w: usize,
    h: usize,
    image: &impl Colours,
    op: StampOp,
    cx: f32,
    cy: f32,
    radius: f32,
    hardness: f32,
) -> Option<Rect> {
    if mask.len() < w * h || start.len() < w * h {
        return None;
    }
    let touched = Rect::around(cx, cy, radius + 1.0, w, h)?;
    let mut disc = Vec::new();
    for py in touched.y0..touched.y1 {
        for px in touched.x0..touched.x1 {
            let d = (px as f32 + 0.5 - cx).hypot(py as f32 + 0.5 - cy);
            if d <= radius {
                disc.push((py * w + px, falloff(d, radius, hardness)));
            }
        }
    }
    let grade: Vec<f32> = match op {
        StampOp::Add | StampOp::Subtract => vec![1.0; disc.len()],
        StampOp::Smart | StampOp::SmartOut => smart_grade(
            start,
            w,
            h,
            image,
            op == StampOp::SmartOut,
            cx,
            cy,
            radius,
            &disc,
        ),
    };
    for (&(i, weight), &k) in disc.iter().zip(&grade) {
        let (m, from) = (mask[i] as f32 / 255.0, start[i] as f32 / 255.0);
        let k = k * weight;
        let v = match op {
            StampOp::Add => m.max(k),
            StampOp::Smart => m.max(from + (1.0 - from) * k),
            StampOp::Subtract | StampOp::SmartOut => m * (1.0 - k),
        };
        mask[i] = (v * 255.0).round() as u8;
    }
    Some(touched)
}

/// Smart's share (0..1) of the painted side for each `disc` pixel, or of the
/// rest with `remove`, sampling `mask` (the stroke's start).
#[allow(clippy::too_many_arguments)]
fn smart_grade(
    mask: &[u8],
    w: usize,
    h: usize,
    image: &impl Colours,
    remove: bool,
    cx: f32,
    cy: f32,
    radius: f32,
    disc: &[(usize, f32)],
) -> Vec<f32> {
    let reach = radius * 2.5;
    let (mut fg, mut bg) = (Vec::new(), Vec::new());
    if let Some(ring) = Rect::around(cx, cy, reach, w, h) {
        let area = ring.width() * ring.height();
        let stride = ((area as f32 / (8 * MAX_SAMPLES) as f32).sqrt().ceil() as usize).max(1);
        for py in (ring.y0..ring.y1).step_by(stride) {
            for px in (ring.x0..ring.x1).step_by(stride) {
                if (px as f32 + 0.5 - cx).hypot(py as f32 + 0.5 - cy) > reach {
                    continue;
                }
                let i = py * w + px;
                if mask[i] >= 230 {
                    fg.push(image.lab(i));
                } else if mask[i] <= 25 && px > 0 && py > 0 && px + 1 < w && py + 1 < h {
                    // Largest step to a neighbour: a strand one pixel wide
                    // stands out from both sides.
                    let t = image.tone(i);
                    let step = [i - 1, i + 1, i - w, i + w]
                        .iter()
                        .map(|&j| (image.tone(j) - t).abs())
                        .fold(0.0, f32::max);
                    bg.push((step, image.lab(i)));
                }
            }
        }
    }
    bg.sort_by(|a, b| a.0.total_cmp(&b.0));
    bg.truncate((bg.len() * 6).div_ceil(10));
    let thin = |v: Vec<[f32; 3]>| -> Vec<[f32; 3]> {
        let step = v.len().div_ceil(MAX_SAMPLES).max(1);
        v.into_iter().step_by(step).collect()
    };
    let nearest = |set: &[[f32; 3]], c: [f32; 3]| {
        set.iter().map(|&s| lab_dist(s, c)).fold(f32::MAX, f32::min)
    };
    // Backdrop showing between strands inside the mask, a strand lying in the
    // backdrop: samples either side could hold.
    let keep = |own: Vec<[f32; 3]>, other: &[[f32; 3]]| -> Vec<[f32; 3]> {
        let kept: Vec<[f32; 3]> = own
            .iter()
            .copied()
            .filter(|&c| nearest(other, c) > AMBIGUOUS)
            .collect();
        if kept.is_empty() {
            own
        } else {
            kept
        }
    };
    let (fg, bg) = (thin(fg), thin(bg.into_iter().map(|(_, c)| c).collect()));
    let fg = keep(fg, &bg);
    let bg = keep(bg, &fg);
    // How far the painted side's colours typically sit from the rest.
    let span = if fg.is_empty() || bg.is_empty() {
        0.0
    } else {
        let mut d: Vec<f32> = fg.iter().map(|&c| nearest(&bg, c)).collect();
        d.sort_by(f32::total_cmp);
        d[d.len() / 2]
    };
    disc.par_iter()
        .map(|&(i, _)| {
            let c = image.lab(i);
            if bg.is_empty() {
                // Nothing but the painted side around.
                return if remove { 0.0 } else { 1.0 };
            }
            if fg.is_empty() {
                // Only backdrop around: how far from it, up to a solid
                // strand's distance.
                let share = ((nearest(&bg, c) - 6.0) / 24.0).clamp(0.0, 1.0);
                return if remove { 1.0 - share } else { share };
            }
            let (to_fg, to_bg) = (nearest(&fg, c), nearest(&bg, c));
            let total = (to_fg + to_bg).max(1e-3);
            let like = if remove { to_fg } else { to_bg };
            let share = ((like / total - 0.1) / 0.8).clamp(0.0, 1.0).powf(0.7);
            share * (1.0 - smoothstep(1.3, 1.8, total / span.max(1.0)))
        })
        .collect()
}

fn smoothstep(lo: f32, hi: f32, v: f32) -> f32 {
    let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Brush tip: 1 at the centre, 0 at the rim; harder tips hold 1 further out.
fn falloff(d: f32, radius: f32, hardness: f32) -> f32 {
    let t = (d / radius).clamp(0.0, 1.0);
    let soft = 1.0 - hardness.clamp(0.0, 1.0);
    if soft < 0.01 || t <= 1.0 - soft {
        1.0
    } else {
        let f = (t - (1.0 - soft)) / soft;
        1.0 - f * f
    }
}

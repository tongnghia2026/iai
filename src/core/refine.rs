//! Refine Selection — the edge engine and the editing session behind the
//! Refine Selection panel (Photoshop's Select and Mask).
//!
//! The panel never edits the selection it was opened on. A session keeps
//! `original` (for Cancel, and as the "before" of the single undo step the
//! panel records) and `base` (that mask plus the Refine Brush strokes, each
//! one undoable inside the panel). The live selection is always
//! `refine(base)`: Radius (edge detection) → Smooth → Feather → Contrast →
//! Shift Edge, Photoshop's order — so a brush stroke never wipes the slider
//! settings and moving a slider never wipes a stroke.

use super::selection::{refine_edge_stamp, EdgeCache, RefineBrushMode, Selection};
use rayon::prelude::*;
use std::time::{Duration, Instant};

/// Slider settings of the Refine Selection panel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RefineParams {
    /// Edge Detection radius (px): the band around the edge that is re-matted
    /// from the photo's colours.
    pub radius: f32,
    /// Keep the band narrow on crisp photo edges, wide on soft ones (hair).
    pub smart_radius: bool,
    /// 0–100: rounds jagged outlines.
    pub smooth: f32,
    /// Blur of the edge (px).
    pub feather: f32,
    /// 0–100 %: sharpens soft edges; 100 % gives a hard edge.
    pub contrast: f32,
    /// −100–100 %: moves soft edges inward / outward.
    pub shift_edge: f32,
}

impl Default for RefineParams {
    fn default() -> Self {
        Self {
            radius: 0.0,
            smart_radius: false,
            smooth: 0.0,
            feather: 0.0,
            contrast: 0.0,
            shift_edge: 0.0,
        }
    }
}

impl RefineParams {
    pub const MAX_RADIUS: f32 = 250.0;
    pub const MAX_FEATHER: f32 = 250.0;

    fn has_radius(&self) -> bool {
        self.radius >= 0.5
    }

    /// Grid cell size and averaging window (in cells) of the local
    /// foreground / background colours: the window spans the whole band, so
    /// every pixel in it sees trusted colour on both sides.
    fn sampling(&self) -> (usize, usize) {
        let (band, fade) = self.band_radii();
        let window = 2 * band + 3 * fade + 2;
        let s = window.div_ceil(12).max(4);
        (s, window.div_ceil(s))
    }

    fn smooth_radius(&self) -> usize {
        if self.smooth < 0.5 {
            0
        } else {
            ((self.smooth / 5.0).round() as usize).max(1)
        }
    }

    fn feather_radius(&self) -> usize {
        if self.feather < 0.5 {
            0
        } else {
            self.feather.round() as usize
        }
    }

    /// Band dilation and its soft fall-off (see `matte_region`).
    fn band_radii(&self) -> (usize, usize) {
        (
            ((0.75 * self.radius).round() as usize).max(1),
            (0.25 * self.radius).round() as usize,
        )
    }

    /// How far a change of `base` can move the matte.
    fn matte_reach(&self) -> usize {
        if !self.has_radius() {
            return 0;
        }
        let (band, fade) = self.band_radii();
        let (s, rc) = self.sampling();
        (band + 3 * fade + 2).max((rc + 2) * s + band + 2 * GUIDE_R + 2) + 4
    }

    /// How far a change of the matte can move the final mask.
    fn finish_reach(&self) -> usize {
        let sr = self.smooth_radius();
        3 * sr + if sr > 0 { DENSITY_R } else { 0 } + 3 * self.feather_radius() + 1
    }

    /// Contrast then Shift Edge — both act on each pixel alone.
    fn point_lut(&self) -> Option<[u8; 256]> {
        let c = self.contrast.clamp(0.0, 100.0);
        let s = self.shift_edge.clamp(-100.0, 100.0) / 100.0;
        if c < 0.5 && s.abs() < 0.005 {
            return None;
        }
        let gain = 1.0 / (1.0 - 0.98 * c / 100.0);
        let mut lut = [0u8; 256];
        for (v, out) in lut.iter_mut().enumerate() {
            let mut a = v as f32 / 255.0;
            if c >= 0.5 {
                a = (0.5 + (a - 0.5) * gain).clamp(0.0, 1.0);
            }
            if s > 0.0 {
                a = (a / (1.0 - 0.99 * s)).min(1.0);
            } else if s < 0.0 {
                let t = 0.99 * -s;
                a = ((a - t) / (1.0 - t)).max(0.0);
            }
            *out = (a * 255.0).round() as u8;
        }
        Some(lut)
    }
}

/// Mask values trusted as selected / unselected colour samples.
const KNOWN_FG: u8 = 250;
const KNOWN_BG: u8 = 5;
/// Guided-filter window that cleans the per-pixel matte.
const GUIDE_R: usize = 2;
const GUIDE_EPS: f32 = 0.001;
/// Squared Lab distance (/100) at which local foreground and background are
/// told apart with full confidence.
const SEPARATION: f32 = 0.012;
/// Smart Radius: an edge counts as crisp when its Sobel peak is at least this
/// and the texture around it stays under `CRISP_TEXTURE` of the peak.
const CRISP_PEAK: f32 = 150.0;
const CRISP_TEXTURE: f32 = 0.3;
/// Band kept around crisp edges under Smart Radius.
const CRISP_BAND: usize = 2;
/// Smooth leaves soft matte (hair) alone: partial-pixel density window.
const DENSITY_R: usize = 2;
const BLOCK: usize = 160;
/// Byte budget of the in-panel stroke history.
const UNDO_BUDGET: usize = 512 << 20;

/// Pixel rectangle `[x0, x1) × [y0, y1)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
}

impl Rect {
    pub fn full(w: usize, h: usize) -> Self {
        Self {
            x0: 0,
            y0: 0,
            x1: w,
            y1: h,
        }
    }

    /// Bounds of a disc, clipped to the canvas; `None` when fully outside.
    pub fn around(cx: f32, cy: f32, r: f32, w: usize, h: usize) -> Option<Self> {
        let x0 = (cx - r).floor().max(0.0) as usize;
        let y0 = (cy - r).floor().max(0.0) as usize;
        let x1 = ((cx + r).ceil() + 1.0).clamp(0.0, w as f32) as usize;
        let y1 = ((cy + r).ceil() + 1.0).clamp(0.0, h as f32) as usize;
        (x1 > x0 && y1 > y0).then_some(Self { x0, y0, x1, y1 })
    }

    pub fn expand(self, m: usize, w: usize, h: usize) -> Self {
        Self {
            x0: self.x0.saturating_sub(m),
            y0: self.y0.saturating_sub(m),
            x1: (self.x1 + m).min(w),
            y1: (self.y1 + m).min(h),
        }
    }

    pub fn union(self, o: Self) -> Self {
        Self {
            x0: self.x0.min(o.x0),
            y0: self.y0.min(o.y0),
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
        }
    }

    pub fn width(&self) -> usize {
        self.x1 - self.x0
    }

    pub fn height(&self) -> usize {
        self.y1 - self.y0
    }

    fn area(&self) -> usize {
        self.width() * self.height()
    }
}

fn union_opt(a: Option<Rect>, b: Rect) -> Rect {
    a.map_or(b, |a| a.union(b))
}

fn extract<T: Copy + Send + Sync>(src: &[T], w: usize, r: Rect) -> Vec<T> {
    let mut out = Vec::with_capacity(r.area());
    for y in r.y0..r.y1 {
        out.extend_from_slice(&src[y * w + r.x0..y * w + r.x1]);
    }
    out
}

fn write_rect(dst: &mut [u8], w: usize, r: Rect, src: &[u8]) {
    let rw = r.width();
    for (ry, y) in (r.y0..r.y1).enumerate() {
        dst[y * w + r.x0..y * w + r.x1].copy_from_slice(&src[ry * rw..(ry + 1) * rw]);
    }
}

#[inline]
fn smoothstep(lo: f32, hi: f32, v: f32) -> f32 {
    let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

// ── Box filters ────────────────────────────────────────────────────────────

/// Row pass of a `(2r+1)` window clipped to the row: `finish(sum, count)`.
fn box_rows(
    src: &[u8],
    dst: &mut [u8],
    w: usize,
    r: usize,
    finish: impl Fn(u32, u32) -> u8 + Sync,
) {
    dst.par_chunks_mut(w)
        .zip(src.par_chunks(w))
        .for_each(|(d, s)| {
            let (mut a, mut b, mut sum) = (0usize, 0usize, 0u32);
            for (x, out) in d.iter_mut().enumerate() {
                let (na, nb) = (x.saturating_sub(r), (x + r + 1).min(w));
                while b < nb {
                    sum += s[b] as u32;
                    b += 1;
                }
                while a < na {
                    sum -= s[a] as u32;
                    a += 1;
                }
                *out = finish(sum, (b - a) as u32);
            }
        });
}

/// Column pass, run in parallel bands of rows with running column sums.
fn box_cols(
    src: &[u8],
    dst: &mut [u8],
    w: usize,
    h: usize,
    r: usize,
    finish: impl Fn(u32, u32) -> u8 + Sync,
) {
    let band = (h / (rayon::current_num_threads() * 4).max(1)).max(16);
    dst.par_chunks_mut(w * band)
        .enumerate()
        .for_each(|(bi, out)| {
            let y_start = bi * band;
            let rows = out.len() / w;
            let mut sums = vec![0u32; w];
            let mut a = y_start.saturating_sub(r);
            let mut b = a;
            for yy in 0..rows {
                let y = y_start + yy;
                let (na, nb) = (y.saturating_sub(r), (y + r + 1).min(h));
                while b < nb {
                    for (s, &v) in sums.iter_mut().zip(&src[b * w..(b + 1) * w]) {
                        *s += v as u32;
                    }
                    b += 1;
                }
                while a < na {
                    for (s, &v) in sums.iter_mut().zip(&src[a * w..(a + 1) * w]) {
                        *s -= v as u32;
                    }
                    a += 1;
                }
                let cnt = (b - a) as u32;
                for (o, &s) in out[yy * w..(yy + 1) * w].iter_mut().zip(&sums) {
                    *o = finish(s, cnt);
                }
            }
        });
}

/// `passes` rounds of a `(2r+1)²` box blur (three ≈ a Gaussian with σ ≈ r),
/// windows clipped at the buffer edge. O(1) per pixel and parallel.
pub fn box_blur(mask: &mut [u8], w: usize, h: usize, r: usize, passes: usize) {
    if r == 0 || w == 0 || h == 0 || mask.len() < w * h {
        return;
    }
    let mask = &mut mask[..w * h];
    let mut tmp = vec![0u8; w * h];
    let mean = |s: u32, c: u32| (s / c) as u8;
    for _ in 0..passes {
        box_rows(mask, &mut tmp, w, r, mean);
        box_cols(&tmp, mask, w, h, r, mean);
    }
}

/// 1 where any non-zero pixel lies within the `(2r+1)²` square.
fn dilate(src: &[u8], w: usize, h: usize, r: usize) -> Vec<u8> {
    let set: Vec<u8> = src.par_iter().map(|&v| (v > 0) as u8).collect();
    if r == 0 {
        return set;
    }
    let any = |s: u32, _: u32| (s > 0) as u8;
    let mut tmp = vec![0u8; w * h];
    box_rows(&set, &mut tmp, w, r, any);
    let mut out = vec![0u8; w * h];
    box_cols(&tmp, &mut out, w, h, r, any);
    out
}

/// Mean over the `(2r+1)²` window clipped to the buffer (running sums).
fn box_mean(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut tmp = vec![0f32; w * h];
    for y in 0..h {
        let s = &src[y * w..(y + 1) * w];
        let (mut a, mut b, mut sum) = (0usize, 0usize, 0f64);
        for x in 0..w {
            let (na, nb) = (x.saturating_sub(r), (x + r + 1).min(w));
            while b < nb {
                sum += s[b] as f64;
                b += 1;
            }
            while a < na {
                sum -= s[a] as f64;
                a += 1;
            }
            tmp[y * w + x] = (sum / (b - a) as f64) as f32;
        }
    }
    let mut out = vec![0f32; w * h];
    let mut sums = vec![0f64; w];
    let (mut a, mut b) = (0usize, 0usize);
    for y in 0..h {
        let (na, nb) = (y.saturating_sub(r), (y + r + 1).min(h));
        while b < nb {
            for (s, &v) in sums.iter_mut().zip(&tmp[b * w..(b + 1) * w]) {
                *s += v as f64;
            }
            b += 1;
        }
        while a < na {
            for (s, &v) in sums.iter_mut().zip(&tmp[a * w..(a + 1) * w]) {
                *s -= v as f64;
            }
            a += 1;
        }
        let cnt = (b - a) as f64;
        for (o, &s) in out[y * w..(y + 1) * w].iter_mut().zip(&sums) {
            *o = (s / cnt) as f32;
        }
    }
    out
}

/// Guided filter (He, Sun & Tang) of `p` with a 3-channel guide.
fn guided_color(guide: &[[f32; 3]], p: &[f32], w: usize, h: usize, r: usize, eps: f32) -> Vec<f32> {
    let n = w * h;
    let chan = |c: usize| -> Vec<f32> { guide.iter().map(|g| g[c]).collect() };
    let i = [chan(0), chan(1), chan(2)];
    let prod = |a: &[f32], b: &[f32]| -> Vec<f32> { a.iter().zip(b).map(|(x, y)| x * y).collect() };
    let m = [
        box_mean(&i[0], w, h, r),
        box_mean(&i[1], w, h, r),
        box_mean(&i[2], w, h, r),
    ];
    let mp = box_mean(p, w, h, r);
    let c = |a: usize, b: usize| box_mean(&prod(&i[a], &i[b]), w, h, r);
    let (c00, c01, c02, c11, c12, c22) = (c(0, 0), c(0, 1), c(0, 2), c(1, 1), c(1, 2), c(2, 2));
    let cp = [
        box_mean(&prod(&i[0], p), w, h, r),
        box_mean(&prod(&i[1], p), w, h, r),
        box_mean(&prod(&i[2], p), w, h, r),
    ];
    let mut a = [vec![0f32; n], vec![0f32; n], vec![0f32; n]];
    let mut b = vec![0f32; n];
    for k in 0..n {
        let (m0, m1, m2) = (m[0][k], m[1][k], m[2][k]);
        let s00 = c00[k] - m0 * m0 + eps;
        let s01 = c01[k] - m0 * m1;
        let s02 = c02[k] - m0 * m2;
        let s11 = c11[k] - m1 * m1 + eps;
        let s12 = c12[k] - m1 * m2;
        let s22 = c22[k] - m2 * m2 + eps;
        let v0 = cp[0][k] - m0 * mp[k];
        let v1 = cp[1][k] - m1 * mp[k];
        let v2 = cp[2][k] - m2 * mp[k];
        let i00 = s11 * s22 - s12 * s12;
        let i01 = s02 * s12 - s01 * s22;
        let i02 = s01 * s12 - s02 * s11;
        let i11 = s00 * s22 - s02 * s02;
        let i12 = s01 * s02 - s00 * s12;
        let i22 = s00 * s11 - s01 * s01;
        let det = s00 * i00 + s01 * i01 + s02 * i02;
        if det.abs() < 1e-12 {
            b[k] = mp[k];
            continue;
        }
        let a0 = (i00 * v0 + i01 * v1 + i02 * v2) / det;
        let a1 = (i01 * v0 + i11 * v1 + i12 * v2) / det;
        let a2 = (i02 * v0 + i12 * v1 + i22 * v2) / det;
        a[0][k] = a0;
        a[1][k] = a1;
        a[2][k] = a2;
        b[k] = mp[k] - a0 * m0 - a1 * m1 - a2 * m2;
    }
    let ma = [
        box_mean(&a[0], w, h, r),
        box_mean(&a[1], w, h, r),
        box_mean(&a[2], w, h, r),
    ];
    let mb = box_mean(&b, w, h, r);
    (0..n)
        .map(|k| ma[0][k] * i[0][k] + ma[1][k] * i[1][k] + ma[2][k] * i[2][k] + mb[k])
        .collect()
}

// ── Stage 1: Radius (edge detection) ───────────────────────────────────────

/// Re-matte `roi` of the matte from `base`. Inside a band around the mask's
/// edge each pixel's alpha is read from the photo: its colour is projected on
/// the line between the local foreground and background colours (averaged
/// over the confidently selected / unselected pixels around the band, on a
/// coarse grid), then cleaned with a small guided filter. Outside the band
/// the matte is `base`.
fn matte_region(
    base: &[u8],
    w: usize,
    h: usize,
    cache: &EdgeCache,
    p: &RefineParams,
    roi: Rect,
    matte: &mut [u8],
) {
    let (band_r, fade_r) = p.band_radii();
    let area = roi.expand(p.matte_reach(), w, h);
    let (aw, ah) = (area.width(), area.height());
    let widx = |x: usize, y: usize| (y - area.y0) * aw + (x - area.x0);

    // 1. The mask's edge pixels.
    let mut edge = vec![0u8; aw * ah];
    edge.par_chunks_mut(aw).enumerate().for_each(|(ry, row)| {
        let y = area.y0 + ry;
        for (rx, out) in row.iter_mut().enumerate() {
            let x = area.x0 + rx;
            let i = y * w + x;
            let v = base[i];
            let differs = |j: usize| base[j].abs_diff(v) > 8;
            *out = ((v > 0 && v < 255)
                || (x > 0 && differs(i - 1))
                || (x + 1 < w && differs(i + 1))
                || (y > 0 && differs(i - w))
                || (y + 1 < h && differs(i + w))) as u8;
        }
    });

    // 2. Smart Radius: crisp photo edges get a narrow band.
    let mut crisp = vec![0u8; aw * ah];
    if p.smart_radius {
        let tr = ((p.radius * 0.5).round() as usize).max(2);
        let tex_area = area.expand(tr, w, h);
        let tw = tex_area.width();
        let tex = box_mean(
            &extract(&cache.sobel, w, tex_area),
            tw,
            tex_area.height(),
            tr,
        );
        crisp
            .par_chunks_mut(aw)
            .zip(edge.par_chunks_mut(aw))
            .enumerate()
            .for_each(|(ry, (crow, erow))| {
                let y = area.y0 + ry;
                for rx in 0..aw {
                    if erow[rx] == 0 {
                        continue;
                    }
                    let x = area.x0 + rx;
                    let mut peak = 0f32;
                    for yy in y.saturating_sub(2)..(y + 3).min(h) {
                        for xx in x.saturating_sub(2)..(x + 3).min(w) {
                            peak = peak.max(cache.sobel[yy * w + xx]);
                        }
                    }
                    let t = tex[(y - tex_area.y0) * tw + (x - tex_area.x0)];
                    if peak >= CRISP_PEAK && t < CRISP_TEXTURE * peak {
                        crow[rx] = 1;
                        erow[rx] = 0;
                    }
                }
            });
    }

    // 3. Band: 1 near the edge, fading out by ~Radius. `core` is where the
    //    mask is not trusted as a colour sample.
    let mut core = dilate(&edge, aw, ah, band_r);
    let mut weight: Vec<u8> = core.par_iter().map(|&v| v * 255).collect();
    box_blur(&mut weight, aw, ah, fade_r, 3);
    if p.smart_radius {
        let near = dilate(&crisp, aw, ah, CRISP_BAND.min(band_r));
        weight
            .par_iter_mut()
            .zip(core.par_iter_mut())
            .zip(near.par_iter())
            .for_each(|((wv, c), &n)| {
                *wv = (*wv).max(n * 255);
                *c |= n;
            });
    }

    // 4. Local foreground / background colours: known pixels summed into
    //    grid cells (aligned to the canvas, so any region gives the same
    //    numbers), then averaged over a window that spans the band.
    let (s, rc) = p.sampling();
    let (gx0, gy0) = (area.x0 / s, area.y0 / s);
    let (gx1, gy1) = (area.x1.div_ceil(s), area.y1.div_ceil(s));
    let (gw, gh) = (gx1 - gx0, gy1 - gy0);
    let mut cells = vec![[0f32; 8]; gw * gh];
    cells.par_chunks_mut(gw).enumerate().for_each(|(cy, row)| {
        let ya = ((gy0 + cy) * s).max(area.y0);
        let yb = ((gy0 + cy + 1) * s).min(area.y1);
        for (cx, cell) in row.iter_mut().enumerate() {
            let xa = ((gx0 + cx) * s).max(area.x0);
            let xb = ((gx0 + cx + 1) * s).min(area.x1);
            for y in ya..yb {
                for x in xa..xb {
                    if core[widx(x, y)] != 0 {
                        continue;
                    }
                    let v = base[y * w + x];
                    let k = if v >= KNOWN_FG {
                        0
                    } else if v <= KNOWN_BG {
                        4
                    } else {
                        continue;
                    };
                    let l = cache.lab[y * w + x];
                    cell[k] += l[0] / 100.0;
                    cell[k + 1] += l[1] / 100.0;
                    cell[k + 2] += l[2] / 100.0;
                    cell[k + 3] += 1.0;
                }
            }
        }
    });
    let fields: Vec<Vec<f32>> = (0..8)
        .into_par_iter()
        .map(|k| {
            let ch: Vec<f32> = cells.iter().map(|c| c[k]).collect();
            box_mean(&ch, gw, gh, rc)
        })
        .collect();
    let sample = |x: usize, y: usize| -> [f32; 8] {
        let fx = ((x as f32 + 0.5) / s as f32 - 0.5 - gx0 as f32).clamp(0.0, (gw - 1) as f32);
        let fy = ((y as f32 + 0.5) / s as f32 - 0.5 - gy0 as f32).clamp(0.0, (gh - 1) as f32);
        let (i0, j0) = (fx.floor() as usize, fy.floor() as usize);
        let (i1, j1) = ((i0 + 1).min(gw - 1), (j0 + 1).min(gh - 1));
        let (tx, ty) = (fx - i0 as f32, fy - j0 as f32);
        let mut v = [0f32; 8];
        for (k, f) in fields.iter().enumerate() {
            let top = f[j0 * gw + i0] * (1.0 - tx) + f[j0 * gw + i1] * tx;
            let bot = f[j1 * gw + i0] * (1.0 - tx) + f[j1 * gw + i1] * tx;
            v[k] = top * (1.0 - ty) + bot * ty;
        }
        v
    };

    // 5. Matte the band block by block.
    let mut blocks = Vec::new();
    let mut by = roi.y0;
    while by < roi.y1 {
        let mut bx = roi.x0;
        while bx < roi.x1 {
            blocks.push(Rect {
                x0: bx,
                y0: by,
                x1: (bx + BLOCK).min(roi.x1),
                y1: (by + BLOCK).min(roi.y1),
            });
            bx += BLOCK;
        }
        by += BLOCK;
    }
    let results: Vec<(Rect, Vec<u8>)> = blocks
        .par_iter()
        .filter_map(|&blk| {
            let touched =
                (blk.y0..blk.y1).any(|y| (blk.x0..blk.x1).any(|x| weight[widx(x, y)] > 0));
            if !touched {
                return None;
            }
            let sub = blk.expand(2 * GUIDE_R + 1, w, h);
            let sw = sub.width();
            let guide: Vec<[f32; 3]> = extract(&cache.lab, w, sub)
                .into_iter()
                .map(|l| [l[0] / 100.0, l[1] / 100.0, l[2] / 100.0])
                .collect();
            let mut alpha = Vec::with_capacity(sub.area());
            for y in sub.y0..sub.y1 {
                for x in sub.x0..sub.x1 {
                    let pv = base[y * w + x] as f32 / 255.0;
                    let v = sample(x, y);
                    if v[3] <= 1e-6 || v[7] <= 1e-6 {
                        alpha.push(pv);
                        continue;
                    }
                    let f = [v[0] / v[3], v[1] / v[3], v[2] / v[3]];
                    let b = [v[4] / v[7], v[5] / v[7], v[6] / v[7]];
                    let d = [f[0] - b[0], f[1] - b[1], f[2] - b[2]];
                    let dd = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
                    let g = guide[alpha.len()];
                    let proj = ((g[0] - b[0]) * d[0] + (g[1] - b[1]) * d[1] + (g[2] - b[2]) * d[2])
                        / dd.max(1e-6);
                    let conf = (dd / SEPARATION).min(1.0);
                    alpha.push(pv + (proj.clamp(0.0, 1.0) - pv) * conf);
                }
            }
            let clean = guided_color(&guide, &alpha, sw, sub.height(), GUIDE_R, GUIDE_EPS);

            let mut out = Vec::with_capacity(blk.area());
            for y in blk.y0..blk.y1 {
                for x in blk.x0..blk.x1 {
                    let b = base[y * w + x];
                    let wv = weight[widx(x, y)] as f32 / 255.0;
                    if wv <= 0.0 {
                        out.push(b);
                        continue;
                    }
                    let a = smoothstep(0.04, 0.96, clean[(y - sub.y0) * sw + (x - sub.x0)]);
                    let v = b as f32 / 255.0 + (a - b as f32 / 255.0) * wv;
                    out.push((v * 255.0).round().clamp(0.0, 255.0) as u8);
                }
            }
            Some((blk, out))
        })
        .collect();

    for y in roi.y0..roi.y1 {
        matte[y * w + roi.x0..y * w + roi.x1]
            .copy_from_slice(&base[y * w + roi.x0..y * w + roi.x1]);
    }
    for (blk, px) in results {
        write_rect(matte, w, blk, &px);
    }
}

// ── Stage 2: Smooth, Feather, Contrast, Shift Edge ─────────────────────────

fn finish_region(matte: &[u8], w: usize, h: usize, p: &RefineParams, roi: Rect, out: &mut [u8]) {
    let sr = p.smooth_radius();
    let fr = p.feather_radius();
    let lut = p.point_lut();
    if sr == 0 && fr == 0 && lut.is_none() {
        for y in roi.y0..roi.y1 {
            out[y * w + roi.x0..y * w + roi.x1]
                .copy_from_slice(&matte[y * w + roi.x0..y * w + roi.x1]);
        }
        return;
    }
    let sub = roi.expand(p.finish_reach(), w, h);
    let (sw, sh) = (sub.width(), sub.height());
    let mut buf = extract(matte, w, sub);

    if sr > 0 {
        // Blur, then pull the edge back to a crisp one: the outline gets
        // rounded without going soft. Soft matte (hair) is kept as it is.
        let orig = buf.clone();
        let mut density: Vec<u8> = orig
            .par_iter()
            .map(|&v| if v > 0 && v < 255 { 255 } else { 0 })
            .collect();
        box_blur(&mut density, sw, sh, DENSITY_R, 1);
        box_blur(&mut buf, sw, sh, sr, 3);
        let gain = 2.5 * sr as f32;
        buf.par_iter_mut()
            .zip(orig.par_iter().zip(density.par_iter()))
            .for_each(|(b, (&o, &d))| {
                let s = (0.5 + (*b as f32 / 255.0 - 0.5) * gain).clamp(0.0, 1.0);
                let keep = smoothstep(0.35, 0.75, d as f32 / 255.0);
                let v = s + (o as f32 / 255.0 - s) * keep;
                *b = (v * 255.0).round() as u8;
            });
    }
    if fr > 0 {
        box_blur(&mut buf, sw, sh, fr, 3);
    }
    if let Some(lut) = lut {
        buf.par_iter_mut().for_each(|v| *v = lut[*v as usize]);
    }
    for y in roi.y0..roi.y1 {
        let s = (y - sub.y0) * sw + (roi.x0 - sub.x0);
        out[y * w + roi.x0..y * w + roi.x1].copy_from_slice(&buf[s..s + roi.width()]);
    }
}

// ── Brush ──────────────────────────────────────────────────────────────────

/// What one Refine Brush dab does to the session's base mask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StampOp {
    /// Colour-aware matting under the brush (hair / fur).
    Smart,
    Add,
    Subtract,
    /// Put back the selection the panel was opened with (Alt + Smart).
    Restore,
}

impl StampOp {
    /// The brush mode, reversed while Alt is held (Photoshop).
    pub fn for_mode(mode: RefineBrushMode, alt: bool) -> Self {
        match (mode, alt) {
            (RefineBrushMode::Smart, false) => StampOp::Smart,
            (RefineBrushMode::Smart, true) => StampOp::Restore,
            (RefineBrushMode::Add, false) | (RefineBrushMode::Subtract, true) => StampOp::Add,
            (RefineBrushMode::Subtract, false) | (RefineBrushMode::Add, true) => StampOp::Subtract,
        }
    }
}

fn restore_stamp(
    base: &mut [u8],
    start: &[u8],
    w: usize,
    h: usize,
    cx: f32,
    cy: f32,
    radius: f32,
    hardness: f32,
) {
    let Some(r) = Rect::around(cx, cy, radius, w, h) else {
        return;
    };
    for y in r.y0..r.y1 {
        for x in r.x0..r.x1 {
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            if d > radius {
                continue;
            }
            let t = (d / radius).clamp(0.0, 1.0);
            let soft = 1.0 - hardness;
            let k = if soft < 0.01 || t <= 1.0 - soft {
                1.0
            } else {
                let f = (t - (1.0 - soft)) / soft;
                1.0 - f * f
            };
            let i = y * w + x;
            let v = base[i] as f32 + (start[i] as f32 - base[i] as f32) * k;
            base[i] = v.round() as u8;
        }
    }
}

// ── Session ────────────────────────────────────────────────────────────────

struct BasePatch {
    rect: Rect,
    before: Vec<u8>,
    after: Vec<u8>,
}

/// One open Refine Selection panel (see the module docs).
pub struct RefineSession {
    /// The selection as it was when the panel opened.
    pub original: Selection,
    /// `original`'s mask with its move offset baked in, when it had one.
    start: Option<Vec<u8>>,
    base: Vec<u8>,
    matte: Vec<u8>,
    params: RefineParams,
    width: usize,
    height: usize,
    stroke_before: Option<Vec<u8>>,
    stroke_rect: Option<Rect>,
    undo: Vec<BasePatch>,
    redo: Vec<BasePatch>,
    /// Area of the live mask changed since the overlay last read it.
    display_dirty: Option<Rect>,
    last_full_render: Duration,
}

impl RefineSession {
    /// `baked` is `original`'s mask with its offset applied.
    pub fn new(original: Selection, baked: Vec<u8>) -> Self {
        let (width, height) = (original.width as usize, original.height as usize);
        let start = (original.offset != (0, 0)).then(|| baked.clone());
        Self {
            original,
            start,
            matte: baked.clone(),
            base: baked,
            params: RefineParams::default(),
            width,
            height,
            stroke_before: None,
            stroke_rect: None,
            undo: Vec::new(),
            redo: Vec::new(),
            display_dirty: None,
            last_full_render: Duration::ZERO,
        }
    }

    fn start_mask(&self) -> &[u8] {
        self.start.as_deref().unwrap_or(&self.original.mask)
    }

    pub fn params(&self) -> RefineParams {
        self.params
    }

    /// Returns whether anything changed (a full render is then due).
    pub fn set_params(&mut self, p: RefineParams) -> bool {
        let changed = self.params != p;
        self.params = p;
        changed
    }

    pub fn needs_edge_cache(&self) -> bool {
        self.params.has_radius()
    }

    /// Time the last full render took (for live slider previews).
    pub fn last_full_render(&self) -> Duration {
        self.last_full_render
    }

    /// Render the live mask into `out` — everywhere, or only what a change of
    /// `base` inside `dirty` can reach. Returns the rewritten area.
    pub fn render(
        &mut self,
        cache: Option<&EdgeCache>,
        out: &mut [u8],
        dirty: Option<Rect>,
    ) -> Rect {
        let started = Instant::now();
        let (w, h) = (self.width, self.height);
        let full = Rect::full(w, h);
        if out.len() < w * h || self.base.len() < w * h {
            return full;
        }
        let p = self.params;
        let cache = cache.filter(|c| {
            p.has_radius()
                && c.width as usize == w
                && c.height as usize == h
                && c.lab.len() >= w * h
                && c.sobel.len() >= w * h
        });
        let matte_roi = match dirty {
            None => full,
            Some(d) => d.expand(if cache.is_some() { p.matte_reach() } else { 0 }, w, h),
        };
        match cache {
            Some(c) => matte_region(&self.base, w, h, c, &p, matte_roi, &mut self.matte),
            None => {
                for y in matte_roi.y0..matte_roi.y1 {
                    self.matte[y * w + matte_roi.x0..y * w + matte_roi.x1]
                        .copy_from_slice(&self.base[y * w + matte_roi.x0..y * w + matte_roi.x1]);
                }
            }
        }
        let fin_roi = match dirty {
            None => full,
            Some(_) => matte_roi.expand(p.finish_reach(), w, h),
        };
        finish_region(&self.matte, w, h, &p, fin_roi, out);
        self.display_dirty = Some(union_opt(self.display_dirty, fin_roi));
        if dirty.is_none() {
            self.last_full_render = started.elapsed();
        }
        fin_roi
    }

    /// Area of the live mask changed since the last call.
    pub fn take_display_dirty(&mut self) -> Option<Rect> {
        self.display_dirty.take()
    }

    pub fn begin_stroke(&mut self) {
        self.stroke_before = Some(self.base.clone());
        self.stroke_rect = None;
    }

    /// Stamp dabs of the given radius at `points`; returns the touched area.
    pub fn paint(
        &mut self,
        cache: Option<&EdgeCache>,
        op: StampOp,
        points: &[(f32, f32)],
        radius: f32,
        hardness: f32,
    ) -> Option<Rect> {
        let (w, h) = (self.width, self.height);
        let mut touched = None;
        for &(x, y) in points {
            let Some(r) = Rect::around(x, y, radius + 1.0, w, h) else {
                continue;
            };
            match op {
                StampOp::Smart => {
                    let Some(c) = cache else {
                        continue;
                    };
                    refine_edge_stamp(
                        &c.lab,
                        &c.sobel,
                        &mut self.base,
                        w as u32,
                        h as u32,
                        x,
                        y,
                        radius,
                        hardness,
                        RefineBrushMode::Smart,
                    );
                }
                StampOp::Add | StampOp::Subtract => {
                    let mode = if op == StampOp::Add {
                        RefineBrushMode::Add
                    } else {
                        RefineBrushMode::Subtract
                    };
                    refine_edge_stamp(
                        &[],
                        &[],
                        &mut self.base,
                        w as u32,
                        h as u32,
                        x,
                        y,
                        radius,
                        hardness,
                        mode,
                    );
                }
                StampOp::Restore => {
                    let mut base = std::mem::take(&mut self.base);
                    restore_stamp(&mut base, self.start_mask(), w, h, x, y, radius, hardness);
                    self.base = base;
                }
            }
            touched = Some(union_opt(touched, r));
        }
        if let Some(t) = touched {
            self.stroke_rect = Some(union_opt(self.stroke_rect, t));
        }
        touched
    }

    /// Close the stroke as one in-panel undo step. Returns whether it changed
    /// anything.
    pub fn end_stroke(&mut self) -> bool {
        let (Some(before), Some(rect)) = (self.stroke_before.take(), self.stroke_rect.take())
        else {
            return false;
        };
        let old = extract(&before, self.width, rect);
        let new = extract(&self.base, self.width, rect);
        if old == new {
            return false;
        }
        self.push_undo(BasePatch {
            rect,
            before: old,
            after: new,
        });
        true
    }

    fn push_undo(&mut self, patch: BasePatch) {
        self.redo.clear();
        self.undo.push(patch);
        let bytes = |p: &BasePatch| p.before.len() + p.after.len();
        let mut total: usize = self.undo.iter().map(bytes).sum();
        while total > UNDO_BUDGET && self.undo.len() > 1 {
            total -= bytes(&self.undo.remove(0));
        }
    }

    /// Replace the whole base mask as one undo step (Clear / Invert).
    pub fn edit_base(&mut self, f: impl FnOnce(&mut [u8])) -> bool {
        let before = self.base.clone();
        f(&mut self.base);
        if before == self.base {
            return false;
        }
        let rect = Rect::full(self.width, self.height);
        let after = self.base.clone();
        self.push_undo(BasePatch {
            rect,
            before,
            after,
        });
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Step the brush history back; returns the area to re-render.
    pub fn undo(&mut self) -> Option<Rect> {
        let patch = self.undo.pop()?;
        write_rect(&mut self.base, self.width, patch.rect, &patch.before);
        let rect = patch.rect;
        self.redo.push(patch);
        Some(rect)
    }

    pub fn redo(&mut self) -> Option<Rect> {
        let patch = self.redo.pop()?;
        write_rect(&mut self.base, self.width, patch.rect, &patch.after);
        let rect = patch.rect;
        self.undo.push(patch);
        Some(rect)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::selection::{compute_sobel, pixels_to_lab};

    fn cache_of(w: usize, h: usize, f: impl Fn(usize, usize) -> [u8; 3]) -> EdgeCache {
        let mut px = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let c = f(x, y);
                px[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
        EdgeCache {
            lab: pixels_to_lab(&px, w as u32, h as u32),
            sobel: compute_sobel(&px, w as u32, h as u32),
            width: w as u32,
            height: h as u32,
            layer_idx: 0,
            layer_revision: 0,
            sample_merged: true,
        }
    }

    fn session(w: usize, h: usize, mask: Vec<u8>) -> RefineSession {
        let mut sel = Selection::new(w as u32, h as u32);
        sel.mask = mask.clone();
        sel.active = true;
        RefineSession::new(sel, mask)
    }

    fn render(s: &mut RefineSession, cache: Option<&EdgeCache>) -> Vec<u8> {
        let mut out = vec![0u8; s.width * s.height];
        s.render(cache, &mut out, None);
        out
    }

    /// The old per-window blur, as the reference.
    fn naive_blur(mask: &mut [u8], w: usize, h: usize, r: usize) {
        for _ in 0..3 {
            let src = mask.to_vec();
            for y in 0..h {
                for x in 0..w {
                    let (x0, x1) = (x.saturating_sub(r), (x + r + 1).min(w));
                    let s: u32 = src[y * w + x0..y * w + x1].iter().map(|&v| v as u32).sum();
                    mask[y * w + x] = (s / (x1 - x0) as u32) as u8;
                }
            }
            let src = mask.to_vec();
            for x in 0..w {
                for y in 0..h {
                    let (y0, y1) = (y.saturating_sub(r), (y + r + 1).min(h));
                    let s: u32 = (y0..y1).map(|yy| src[yy * w + x] as u32).sum();
                    mask[y * w + x] = (s / (y1 - y0) as u32) as u8;
                }
            }
        }
    }

    #[test]
    fn box_blur_matches_the_per_window_sum() {
        let (w, h) = (53, 71);
        let src: Vec<u8> = (0..w * h).map(|i| ((i * 7919) % 256) as u8).collect();
        for r in [1, 3, 9, 40, 90] {
            let mut a = src.clone();
            let mut b = src.clone();
            box_blur(&mut a, w, h, r, 3);
            naive_blur(&mut b, w, h, r);
            assert_eq!(a, b, "r={r}");
        }
    }

    #[test]
    fn dilate_marks_the_square_around_set_pixels() {
        let (w, h) = (20, 15);
        let mut src = vec![0u8; w * h];
        src[7 * w + 9] = 200;
        let d = dilate(&src, w, h, 3);
        for y in 0..h {
            for x in 0..w {
                let inside = x.abs_diff(9) <= 3 && y.abs_diff(7) <= 3;
                assert_eq!(d[y * w + x], inside as u8, "({x},{y})");
            }
        }
    }

    /// Dark hair on a white wall; the rough selection stops short of the loose
    /// strands and takes in some wall between them.
    fn hair() -> (usize, usize, EdgeCache, Vec<u8>) {
        let (w, h) = (200, 120);
        let is_hair = |x: usize, y: usize| {
            (20..=60).contains(&x) || ((20..100).contains(&y) && (x == 70 || x == 76 || x == 82))
        };
        let cache = cache_of(w, h, |x, y| {
            if is_hair(x, y) {
                [24, 20, 18]
            } else {
                [246, 244, 240]
            }
        });
        let mut mask = vec![0u8; w * h];
        for y in 0..h {
            for x in 20..67 {
                mask[y * w + x] = 255;
            }
        }
        (w, h, cache, mask)
    }

    #[test]
    fn radius_pulls_in_loose_strands_and_drops_the_wall_between() {
        let (w, h, cache, mask) = hair();
        let mut s = session(w, h, mask);
        s.set_params(RefineParams {
            radius: 24.0,
            ..Default::default()
        });
        let out = render(&mut s, Some(&cache));
        let at = |x: usize, y: usize| out[y * w + x];
        assert!(
            at(70, 60) > 200,
            "strand outside the selection: {}",
            at(70, 60)
        );
        assert!(at(76, 60) > 200, "second strand: {}", at(76, 60));
        assert!(at(64, 60) < 40, "wall inside the selection: {}", at(64, 60));
        assert!(at(73, 60) < 40, "wall between strands: {}", at(73, 60));
        assert_eq!(at(40, 60), 255, "solid hair stays selected");
        assert_eq!(at(150, 60), 0, "far wall stays unselected");
        assert_eq!(at(10, 60), 0, "wall on the other side stays unselected");
    }

    #[test]
    fn zero_radius_leaves_the_mask_alone() {
        let (w, h, cache, mask) = hair();
        let mut s = session(w, h, mask.clone());
        assert_eq!(render(&mut s, Some(&cache)), mask);
    }

    #[test]
    fn smart_radius_keeps_crisp_edges_tight() {
        // A crisp object edge, with a dark stripe in the backdrop close by.
        let (w, h) = (160, 80);
        let cache = cache_of(w, h, |x, _| {
            if x < 60 || (74..78).contains(&x) {
                [20, 20, 20]
            } else {
                [235, 235, 235]
            }
        });
        let mut mask = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..60 {
                mask[y * w + x] = 255;
            }
        }
        let p = RefineParams {
            radius: 30.0,
            ..Default::default()
        };
        let mut wide = session(w, h, mask.clone());
        wide.set_params(p);
        let wide_out = render(&mut wide, Some(&cache));
        let mut smart = session(w, h, mask.clone());
        smart.set_params(RefineParams {
            smart_radius: true,
            ..p
        });
        let smart_out = render(&mut smart, Some(&cache));
        for y in 0..h {
            for x in 0..w {
                if x.abs_diff(60) > CRISP_BAND + 1 {
                    assert_eq!(smart_out[y * w + x], mask[y * w + x], "({x},{y})");
                }
            }
        }
        assert!(wide_out[40 * w + 75] > smart_out[40 * w + 75]);
    }

    fn disc(w: usize, h: usize, r: f32) -> Vec<u8> {
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                if ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() <= r {
                    255
                } else {
                    0
                }
            })
            .collect()
    }

    fn area(m: &[u8]) -> f32 {
        m.iter().map(|&v| v as f32 / 255.0).sum()
    }

    #[test]
    fn contrast_hardens_and_shift_edge_moves_a_soft_edge() {
        let (w, h) = (120, 120);
        let mut s = session(w, h, disc(w, h, 40.0));
        let feathered = RefineParams {
            feather: 6.0,
            ..Default::default()
        };
        s.set_params(feathered);
        let soft = render(&mut s, None);
        let partial = |m: &[u8]| m.iter().filter(|&&v| v > 5 && v < 250).count();
        s.set_params(RefineParams {
            contrast: 100.0,
            ..feathered
        });
        let hard = render(&mut s, None);
        assert!(
            partial(&hard) * 10 < partial(&soft),
            "{} vs {}",
            partial(&hard),
            partial(&soft)
        );
        s.set_params(RefineParams {
            shift_edge: 50.0,
            ..feathered
        });
        let out = render(&mut s, None);
        s.set_params(RefineParams {
            shift_edge: -50.0,
            ..feathered
        });
        let inn = render(&mut s, None);
        assert!(area(&out) > area(&soft) + 100.0);
        assert!(area(&inn) < area(&soft) - 100.0);
        // Continuous: a small shift moves a little.
        s.set_params(RefineParams {
            shift_edge: 3.0,
            ..feathered
        });
        let tiny = render(&mut s, None);
        assert!(area(&tiny) > area(&soft) && area(&tiny) < area(&out));
    }

    #[test]
    fn smooth_rounds_a_staircase_without_softening_it() {
        let (w, h) = (100, 100);
        let mut mask = vec![0u8; w * h];
        for y in 20..80 {
            let x1 = 50 + if (y / 4) % 2 == 0 { 4 } else { 0 };
            for x in 20..x1 {
                mask[y * w + x] = 255;
            }
        }
        let mut s = session(w, h, mask.clone());
        s.set_params(RefineParams {
            smooth: 20.0,
            ..Default::default()
        });
        let out = render(&mut s, None);
        // A plain blur of that size leaves thousands of soft pixels; the
        // outline (~190 px long) keeps about one per step.
        let partial = out.iter().filter(|&&v| v > 5 && v < 250).count();
        assert!(partial < 450, "edge stays crisp: {partial}");
        // The notches are gone: column 52 is now uniform along the edge.
        let col: Vec<u8> = (30..70).map(|y| out[y * w + 52]).collect();
        let spread = col.iter().max().unwrap() - col.iter().min().unwrap();
        assert!(spread < 140, "staircase flattened: {col:?}");
        assert!((area(&out) - area(&mask)).abs() < 150.0);
    }

    #[test]
    fn a_partial_render_matches_a_full_one() {
        let (w, h, cache, mask) = hair();
        let p = RefineParams {
            radius: 12.0,
            smart_radius: true,
            smooth: 10.0,
            feather: 2.0,
            contrast: 20.0,
            shift_edge: -10.0,
        };
        let mut s = session(w, h, mask);
        s.set_params(p);
        let mut live = render(&mut s, Some(&cache));
        s.begin_stroke();
        let touched = s
            .paint(
                Some(&cache),
                StampOp::Add,
                &[(120.0, 60.0), (126.0, 64.0)],
                9.0,
                0.5,
            )
            .unwrap();
        s.end_stroke();
        s.render(Some(&cache), &mut live, Some(touched));
        let full = render(&mut s, Some(&cache));
        assert_eq!(live, full);
    }

    #[test]
    fn strokes_undo_and_redo_inside_the_panel() {
        let (w, h) = (80, 60);
        let mask = disc(w, h, 15.0);
        let mut s = session(w, h, mask.clone());
        s.begin_stroke();
        s.paint(None, StampOp::Add, &[(60.0, 30.0)], 6.0, 1.0);
        assert!(s.end_stroke());
        let painted = s.base.clone();
        assert_ne!(painted, mask);
        assert!(s.undo().is_some());
        assert_eq!(s.base, mask);
        assert!(s.redo().is_some());
        assert_eq!(s.base, painted);
        // Alt + Smart puts the opening selection back.
        s.begin_stroke();
        s.paint(None, StampOp::Restore, &[(60.0, 30.0)], 10.0, 1.0);
        s.end_stroke();
        assert_eq!(s.base, mask);
    }
}

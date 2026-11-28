//! A signature (or a stamp) lifted off its paper: the paper turns clear, the
//! ink keeps its own colour, the picture is cut down to the ink. For a
//! signature photographed or scanned on paper, to be placed on a document.
//!
//! The paper is found region by region (a bright percentile of each block,
//! spread smoothly between blocks), so a phone photo darker on one side, or
//! a slightly yellow sheet, still goes clear. How much of a pixel is ink is
//! how much darker than its paper it is; its colour is what the ink must be
//! to look like that over that paper, so the edges carry no pale halo.
//!
//! A picture that is clear already (a PNG of the signature on nothing, a
//! white one made for dark photos among them) has no paper to lift: it is
//! kept as it is and only cut down to what shows.

use rayon::prelude::*;

/// The ink of a picture, on nothing: straight RGBA, `width`×`height`, cut
/// out of the source at (`x`, `y`).
#[derive(Clone, Debug, PartialEq)]
pub struct Ink {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub x: u32,
    pub y: u32,
}

/// Darker than its paper by less than this (0..1) is paper: grain, noise.
const PAPER_NOISE: f32 = 0.06;
/// A picture whose darkest strokes are no darker than this holds no ink.
const LEAST_INK: f32 = 0.1;
/// Ink at least this opaque counts when the picture is cut down to it.
const KEPT_ALPHA: u8 = 24;
/// Clear pixels kept around the ink.
const PAD: u32 = 4;
/// A picture with at least this share of (mostly) clear pixels is clear
/// already: a signature on nothing, not on paper.
const CLEAR_SHARE: f32 = 0.02;

/// Lift the ink of `src` (straight RGBA, `w`×`h`) off its paper. `None` when
/// there is no ink in it (a blank sheet) or the size is wrong.
pub fn lift_ink(src: &[u8], w: u32, h: u32) -> Option<Ink> {
    let (wi, hi) = (w as usize, h as usize);
    if wi == 0 || hi == 0 || src.len() < wi * hi * 4 {
        return None;
    }
    let src = &src[..wi * hi * 4];
    if is_clear_already(src) {
        return trim(src, wi, hi);
    }
    let paper = Paper::of(src, wi, hi);

    // How much darker than its paper each pixel is.
    let mut depth = vec![0f32; wi * hi];
    depth.par_chunks_mut(wi).enumerate().for_each(|(y, row)| {
        for (x, d) in row.iter_mut().enumerate() {
            let i = (y * wi + x) * 4;
            let bg = paper.at(x, y);
            let a = src[i + 3] as f32 / 255.0;
            *d = (luma(bg) - luma(px(&src[i..i + 3]))).max(0.0) * a;
        }
    });

    // Full ink: about as dark as the darkest strokes. A pixel half as dark
    // is ink half covering its paper (a stroke's soft edge).
    let deepest = percentile(&depth, 0.995);
    if deepest < LEAST_INK {
        return None;
    }
    let solid = 0.85 * deepest;

    let mut out = vec![0u8; wi * hi * 4];
    out.par_chunks_mut(wi * 4).enumerate().for_each(|(y, row)| {
        for x in 0..wi {
            let i = y * wi + x;
            // How much of the pixel the ink covers; the faintest, which is
            // the paper's own grain, fades out to nothing.
            let cover = (depth[i] / solid).clamp(0.0, 1.0);
            let t = ((depth[i] - PAPER_NOISE) / PAPER_NOISE).clamp(0.0, 1.0);
            let alpha = cover * t * t * (3.0 - 2.0 * t);
            if alpha <= 0.0 {
                continue;
            }
            let bg = paper.at(x, y);
            let seen = px(&src[i * 4..i * 4 + 3]);
            let o = &mut row[x * 4..x * 4 + 4];
            for c in 0..3 {
                // seen = cover·ink + (1 − cover)·paper, solved for ink.
                let ink = (seen[c] - (1.0 - cover) * bg[c]) / cover;
                o[c] = (ink * 255.0).round().clamp(0.0, 255.0) as u8;
            }
            o[3] = (alpha * 255.0).round() as u8;
        }
    });

    trim(&out, wi, hi)
}

/// The paper's colour around each pixel: a bright percentile of each block,
/// spread bilinearly between block centres.
struct Paper {
    grid: Vec<[f32; 3]>,
    gw: usize,
    gh: usize,
    block: usize,
}

impl Paper {
    fn of(src: &[u8], wi: usize, hi: usize) -> Self {
        let block = (wi.max(hi) / 16).clamp(16, 128);
        let (gw, gh) = (wi.div_ceil(block), hi.div_ceil(block));
        let grid: Vec<[f32; 3]> = (0..gw * gh)
            .into_par_iter()
            .map(|b| {
                let (bx, by) = (b % gw, b / gw);
                let mut hist = [[0u32; 256]; 3];
                let mut n = 0u32;
                for y in by * block..((by + 1) * block).min(hi) {
                    for x in bx * block..((bx + 1) * block).min(wi) {
                        let i = (y * wi + x) * 4;
                        if src[i + 3] < 128 {
                            continue;
                        }
                        for (c, h) in hist.iter_mut().enumerate() {
                            h[src[i + c] as usize] += 1;
                        }
                        n += 1;
                    }
                }
                if n == 0 {
                    return [1.0; 3];
                }
                // Ink rarely covers a tenth of a block: the brightest tenth
                // is paper.
                let want = (n as f32 * 0.9).ceil() as u32;
                let mut colour = [1.0f32; 3];
                for (c, h) in hist.iter().enumerate() {
                    let mut seen = 0;
                    for (v, count) in h.iter().enumerate() {
                        seen += count;
                        if seen >= want {
                            colour[c] = v as f32 / 255.0;
                            break;
                        }
                    }
                }
                colour
            })
            .collect();
        Self {
            grid,
            gw,
            gh,
            block,
        }
    }

    fn at(&self, x: usize, y: usize) -> [f32; 3] {
        let b = self.block as f32;
        let fx = ((x as f32 + 0.5) / b - 0.5).clamp(0.0, (self.gw - 1) as f32);
        let fy = ((y as f32 + 0.5) / b - 0.5).clamp(0.0, (self.gh - 1) as f32);
        let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(self.gw - 1), (y0 + 1).min(self.gh - 1));
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let g = |x: usize, y: usize| self.grid[y * self.gw + x];
        let (a, b2, c, d) = (g(x0, y0), g(x1, y0), g(x0, y1), g(x1, y1));
        let mut out = [0.0; 3];
        for k in 0..3 {
            let top = a[k] + (b2[k] - a[k]) * tx;
            let bottom = c[k] + (d[k] - c[k]) * tx;
            out[k] = top + (bottom - top) * ty;
        }
        out
    }
}

/// Whether the picture has no paper: enough of it is clear already.
fn is_clear_already(src: &[u8]) -> bool {
    let clear = src.par_chunks(4).filter(|pixel| pixel[3] < 128).count();
    clear as f32 >= CLEAR_SHARE * (src.len() / 4) as f32
}

fn px(rgb: &[u8]) -> [f32; 3] {
    [
        rgb[0] as f32 / 255.0,
        rgb[1] as f32 / 255.0,
        rgb[2] as f32 / 255.0,
    ]
}

fn luma(c: [f32; 3]) -> f32 {
    0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
}

/// The `q` quantile (0..1) of `values`, from a 1024-step histogram of 0..1.
fn percentile(values: &[f32], q: f32) -> f32 {
    const STEPS: usize = 1024;
    let hist = values
        .par_chunks(1 << 16)
        .map(|chunk| {
            let mut h = vec![0u32; STEPS];
            for &v in chunk {
                h[((v.clamp(0.0, 1.0) * (STEPS - 1) as f32) as usize).min(STEPS - 1)] += 1;
            }
            h
        })
        .reduce(
            || vec![0u32; STEPS],
            |mut a, b| {
                for (x, y) in a.iter_mut().zip(b) {
                    *x += y;
                }
                a
            },
        );
    let want = (values.len() as f32 * q).ceil() as u64;
    let mut seen = 0u64;
    for (i, count) in hist.iter().enumerate() {
        seen += *count as u64;
        if seen >= want {
            return i as f32 / (STEPS - 1) as f32;
        }
    }
    1.0
}

/// `rgba` cut down to its ink and a few clear pixels around it.
fn trim(rgba: &[u8], wi: usize, hi: usize) -> Option<Ink> {
    let (mut x0, mut y0, mut x1, mut y1) = (wi, hi, 0usize, 0usize);
    for y in 0..hi {
        for x in 0..wi {
            if rgba[(y * wi + x) * 4 + 3] >= KEPT_ALPHA {
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
        }
    }
    if x0 > x1 || y0 > y1 {
        return None;
    }
    let pad = PAD as usize;
    let (x0, y0) = (x0.saturating_sub(pad), y0.saturating_sub(pad));
    let (x1, y1) = ((x1 + pad).min(wi - 1), (y1 + pad).min(hi - 1));
    let (cw, ch) = (x1 - x0 + 1, y1 - y0 + 1);
    let mut out = Vec::with_capacity(cw * ch * 4);
    for y in y0..=y1 {
        out.extend_from_slice(&rgba[(y * wi + x0) * 4..(y * wi + x1 + 1) * 4]);
    }
    Some(Ink {
        rgba: out,
        width: cw as u32,
        height: ch as u32,
        x: x0 as u32,
        y: y0 as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `w`×`h` sheet of `paper(x)` with `ink` drawn where `stroke(x, y)`.
    fn sheet(
        w: u32,
        h: u32,
        paper: impl Fn(u32) -> [u8; 3],
        ink: [u8; 3],
        stroke: impl Fn(u32, u32) -> bool,
    ) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let c = if stroke(x, y) { ink } else { paper(x) };
                v.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
        v
    }

    fn at(ink: &Ink, x: u32, y: u32) -> [u8; 4] {
        let i = ((y - ink.y) * ink.width + (x - ink.x)) as usize * 4;
        [
            ink.rgba[i],
            ink.rgba[i + 1],
            ink.rgba[i + 2],
            ink.rgba[i + 3],
        ]
    }

    #[test]
    fn the_paper_goes_clear_and_the_ink_stays() {
        let (w, h) = (200, 100);
        // A stroke from (40, 48) to (160, 52).
        let stroke = |x: u32, y: u32| (40..160).contains(&x) && (48..52).contains(&y);
        let src = sheet(w, h, |_| [250, 250, 248], [20, 20, 25], stroke);
        let ink = lift_ink(&src, w, h).expect("ink");
        // Cut down to the stroke and a few pixels around it.
        assert_eq!((ink.x, ink.y), (36, 44));
        assert_eq!((ink.width, ink.height), (128, 12));
        let mid = at(&ink, 100, 50);
        assert!(mid[3] > 240 && mid[0] < 40, "{mid:?}");
        assert_eq!(at(&ink, 38, 46)[3], 0, "paper beside the stroke");
    }

    #[test]
    fn a_shaded_photo_of_the_paper_still_goes_clear() {
        let (w, h) = (400, 120);
        // Bright on the left, in shadow on the right.
        let paper = |x: u32| {
            let v = (250.0 - 110.0 * x as f32 / w as f32) as u8;
            [v, v, v.saturating_sub(4)]
        };
        let stroke = |x: u32, y: u32| {
            ((30..90).contains(&x) || (310..370).contains(&x)) && (58..62).contains(&y)
        };
        let src = sheet(w, h, paper, [15, 15, 20], stroke);
        let ink = lift_ink(&src, w, h).expect("ink");
        for y in ink.y..ink.y + ink.height {
            for x in ink.x..ink.x + ink.width {
                if !stroke(x, y) {
                    assert!(at(&ink, x, y)[3] < 16, "paper at ({x}, {y})");
                }
            }
        }
        assert!(at(&ink, 60, 60)[3] > 240);
        assert!(at(&ink, 340, 60)[3] > 240, "the stroke in the shadow");
    }

    #[test]
    fn blue_ink_stays_blue() {
        let (w, h) = (120, 60);
        let stroke = |x: u32, y: u32| (20..100).contains(&x) && (28..33).contains(&y);
        let src = sheet(w, h, |_| [252, 252, 252], [30, 60, 190], stroke);
        let ink = lift_ink(&src, w, h).expect("ink");
        let c = at(&ink, 60, 30);
        assert!(c[3] > 240 && c[2] > c[0] + 100, "{c:?}");
    }

    #[test]
    fn a_soft_edge_is_ink_seen_through_not_a_pale_halo() {
        // Black ink, white paper, and a pixel halfway between: half ink.
        let (w, h) = (80, 40);
        let mut src = sheet(
            w,
            h,
            |_| [255, 255, 255],
            [0, 0, 0],
            |x, y| (20..60).contains(&x) && (18..22).contains(&y),
        );
        for x in 20..60 {
            let i = ((17 * w + x) * 4) as usize;
            src[i..i + 3].copy_from_slice(&[180, 180, 180]);
        }
        let ink = lift_ink(&src, w, h).expect("ink");
        let edge = at(&ink, 40, 17);
        assert!(edge[3] > 40 && edge[3] < 250, "{edge:?}");
        assert!(
            edge[0] < 60,
            "the edge is dark ink, not grey paper: {edge:?}"
        );
    }

    #[test]
    fn a_signature_on_nothing_is_kept_as_it_is_and_cut_down() {
        // White ink on a clear picture: a signature made for dark photos.
        let (w, h) = (100u32, 50u32);
        let mut src = vec![0u8; (w * h * 4) as usize];
        for y in 20..30 {
            for x in 10..90 {
                let i = ((y * w + x) * 4) as usize;
                src[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        let ink = lift_ink(&src, w, h).expect("the white signature");
        assert_eq!((ink.x, ink.y, ink.width, ink.height), (6, 16, 88, 18));
        assert_eq!(at(&ink, 50, 25), [255, 255, 255, 255]);
        assert_eq!(at(&ink, 50, 17)[3], 0);
        // Nothing showing at all: nothing to place.
        assert_eq!(lift_ink(&vec![0u8; (w * h * 4) as usize], w, h), None);
    }

    #[test]
    fn a_blank_sheet_has_no_ink() {
        let src = sheet(
            64,
            64,
            |x| [240, 240, (230 + x % 3) as u8],
            [0, 0, 0],
            |_, _| false,
        );
        assert_eq!(lift_ink(&src, 64, 64), None);
        assert_eq!(lift_ink(&[], 0, 0), None);
        assert_eq!(lift_ink(&[0; 8], 4, 4), None);
    }
}

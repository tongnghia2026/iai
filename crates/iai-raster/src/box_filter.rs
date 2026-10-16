//! Box filters over 8-bit masks: a row pass and a column pass with running
//! sums, O(1) per pixel and parallel.

use rayon::prelude::*;

/// Row pass of a `(2r+1)` window clipped to the row: `finish(sum, count)`.
pub fn box_rows(
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
pub fn box_cols(
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

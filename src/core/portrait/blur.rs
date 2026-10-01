//! Fast approximate Gaussian blur (three box passes each way) for the
//! frequency split, with a mask-normalised variant that keeps non-skin colour
//! from bleeding into skin.

use rayon::prelude::*;

fn box_pass_rows(data: &mut [[f32; 4]], width: usize, radius: usize) {
    if radius == 0 || width == 0 {
        return;
    }
    let norm = 1.0 / (2 * radius + 1) as f32;
    data.par_chunks_mut(width).for_each(|row| {
        let source = row.to_vec();
        let at = |i: isize| source[i.clamp(0, width as isize - 1) as usize];
        let mut sum = [0.0f32; 4];
        for i in -(radius as isize)..=radius as isize {
            let v = at(i);
            for c in 0..4 {
                sum[c] += v[c];
            }
        }
        for x in 0..width {
            for c in 0..4 {
                row[x][c] = sum[c] * norm;
            }
            let add = at(x as isize + radius as isize + 1);
            let remove = at(x as isize - radius as isize);
            for c in 0..4 {
                sum[c] += add[c] - remove[c];
            }
        }
    });
}

fn transpose(data: &[[f32; 4]], width: usize, height: usize) -> Vec<[f32; 4]> {
    let mut out = vec![[0.0f32; 4]; data.len()];
    out.par_chunks_mut(height)
        .enumerate()
        .for_each(|(x, column)| {
            for (y, cell) in column.iter_mut().enumerate() {
                *cell = data[y * width + x];
            }
        });
    out
}

/// Blur four channels in place; `sigma` in pixels. A wide blur runs on a grid
/// of blocks up to 8 pixels across and is read back bilinearly: its result is
/// smooth on that scale anyway.
pub fn blur4(data: &mut Vec<[f32; 4]>, width: usize, height: usize, sigma: f32) {
    if width == 0 || height == 0 {
        return;
    }
    let mut factor = 1usize;
    while factor < 8 && sigma / (factor * 2) as f32 >= 10.0 {
        factor *= 2;
    }
    if factor == 1 {
        box_blur(data, width, height, sigma);
        return;
    }
    let (cw, ch) = (width.div_ceil(factor), height.div_ceil(factor));
    let mut coarse: Vec<[f32; 4]> = (0..cw * ch)
        .into_par_iter()
        .map(|c| {
            let (x0, y0) = ((c % cw) * factor, (c / cw) * factor);
            let (x1, y1) = ((x0 + factor).min(width), (y0 + factor).min(height));
            let mut sum = [0.0f32; 4];
            for y in y0..y1 {
                for v in &data[y * width + x0..y * width + x1] {
                    for k in 0..4 {
                        sum[k] += v[k];
                    }
                }
            }
            let n = ((x1 - x0) * (y1 - y0)) as f32;
            sum.map(|s| s / n)
        })
        .collect();
    // The radius whose three passes spread as far as the full blur's would.
    let radius = sigma.round();
    let spread = (radius * (radius + 1.0)).sqrt() / factor as f32;
    let coarse_radius = ((1.0 + 4.0 * spread * spread).sqrt() - 1.0) * 0.5;
    box_blur(&mut coarse, cw, ch, coarse_radius);
    let at = |p: usize, cells: usize| {
        let t = ((p as f32 + 0.5) / factor as f32 - 0.5).clamp(0.0, (cells - 1) as f32);
        let lo = t as usize;
        (lo, (lo + 1).min(cells - 1), t - lo as f32)
    };
    data.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let (y0, y1, fy) = at(y, ch);
        for (x, cell) in row.iter_mut().enumerate() {
            let (x0, x1, fx) = at(x, cw);
            let (a, b) = (coarse[y0 * cw + x0], coarse[y0 * cw + x1]);
            let (c, d) = (coarse[y1 * cw + x0], coarse[y1 * cw + x1]);
            for k in 0..4 {
                let top = a[k] + (b[k] - a[k]) * fx;
                let bottom = c[k] + (d[k] - c[k]) * fx;
                cell[k] = top + (bottom - top) * fy;
            }
        }
    });
}

fn box_blur(data: &mut Vec<[f32; 4]>, width: usize, height: usize, sigma: f32) {
    // Three box passes of radius r give a Gaussian of sigma ~ r.
    let radius = sigma.round().max(0.0) as usize;
    if radius == 0 {
        return;
    }
    for _ in 0..3 {
        box_pass_rows(data, width, radius);
    }
    let mut columns = transpose(data, width, height);
    for _ in 0..3 {
        box_pass_rows(&mut columns, height, radius);
    }
    *data = transpose(&columns, height, width);
}

/// RGB blurred with each pixel weighted by `mask`, then divided by the blurred
/// mask; pixels with no nearby weight keep their own colour.
pub fn masked_blur(
    rgb: &[[f32; 3]],
    mask: &[f32],
    width: usize,
    height: usize,
    sigma: f32,
) -> Vec<[f32; 3]> {
    let mut weighted: Vec<[f32; 4]> = rgb
        .par_iter()
        .zip(mask.par_iter())
        .map(|(c, &m)| {
            let m = m.max(1e-3);
            [c[0] * m, c[1] * m, c[2] * m, m]
        })
        .collect();
    blur4(&mut weighted, width, height, sigma);
    weighted
        .par_iter()
        .zip(rgb.par_iter())
        .map(|(w, c)| {
            if w[3] > 1e-4 {
                [w[0] / w[3], w[1] / w[3], w[2] / w[3]]
            } else {
                *c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blur_keeps_flat_fields_and_spreads_a_spike() {
        let (w, h) = (31, 21);
        let mut data = vec![[0.5f32, 0.5, 0.5, 1.0]; w * h];
        data[10 * w + 15] = [10.5, 0.5, 0.5, 1.0];
        blur4(&mut data, w, h, 3.0);
        assert!((data[0][1] - 0.5).abs() < 1e-5);
        assert!(data[10 * w + 15][0] < 1.0);
        let total: f32 = data.iter().map(|p| p[0] - 0.5).sum();
        assert!((total - 10.0).abs() < 0.05, "mass {total}");
    }

    #[test]
    fn wide_blur_on_blocks_matches_the_full_one() {
        for sigma in [24.0f32, 50.0, 90.0] {
            let (w, h) = ((sigma * 12.0) as usize, (sigma * 9.0) as usize);
            let field: Vec<[f32; 4]> = (0..w * h)
                .map(|i| {
                    let (x, y) = ((i % w) as f32 / sigma, (i / w) as f32 / sigma);
                    let v = if (x - 6.0).hypot(y - 4.5) < 2.5 {
                        1.0
                    } else {
                        0.0
                    };
                    [v, (x * 2.4).sin(), 0.5, 0.0]
                })
                .collect();
            let mut full = field.clone();
            box_blur(&mut full, w, h, sigma);
            let mut fast = field;
            blur4(&mut fast, w, h, sigma);
            // Edges replicate a block's mean rather than one pixel, so only
            // the inside is compared.
            let m = (2.0 * sigma) as usize;
            let worst = (0..w * h)
                .filter(|i| (m..w - m).contains(&(i % w)) && (m..h - m).contains(&(i / w)))
                .map(|i| {
                    (0..3)
                        .map(|k| (full[i][k] - fast[i][k]).abs())
                        .fold(0.0, f32::max)
                })
                .fold(0.0, f32::max);
            assert!(worst < 0.02, "sigma {sigma}: largest difference {worst}");
        }
    }
}

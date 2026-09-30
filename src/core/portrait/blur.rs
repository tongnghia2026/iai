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

/// Blur four channels in place; `sigma` in pixels.
pub fn blur4(data: &mut Vec<[f32; 4]>, width: usize, height: usize, sigma: f32) {
    if width == 0 || height == 0 {
        return;
    }
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
}

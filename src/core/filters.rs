use rayon::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FilterType {
    GaussianBlur { radius: f32 },
    Sharpen { amount: f32, radius: f32 },
    HighPass { radius: f32 },
    AddNoise { amount: f32, monochromatic: bool },
    Pixelate { cell: f32 },
    ReduceNoise { strength: f32 },
}

impl FilterType {
    pub fn name(&self) -> &'static str {
        match self {
            FilterType::GaussianBlur { .. } => "Gaussian Blur",
            FilterType::Sharpen { .. } => "Sharpen",
            FilterType::HighPass { .. } => "High Pass",
            FilterType::AddNoise { .. } => "Add Noise",
            FilterType::Pixelate { .. } => "Pixelate",
            FilterType::ReduceNoise { .. } => "Reduce Noise",
        }
    }

    pub fn apply(&self, pixels: &mut Vec<u8>, w: u32, h: u32) {
        match self {
            FilterType::GaussianBlur { radius } => {
                if *radius > 0.01 {
                    *pixels = gaussian_blur_rgba(pixels, w, h, *radius);
                }
            }
            FilterType::Sharpen { amount, radius } => {
                if *amount > 0.001 && *radius > 0.01 {
                    *pixels = unsharp_mask(pixels, w, h, *radius, *amount);
                }
            }
            FilterType::HighPass { radius } => {
                if *radius > 0.01 {
                    *pixels = high_pass(pixels, w, h, *radius);
                }
            }
            FilterType::AddNoise {
                amount,
                monochromatic,
            } => {
                if *amount > 0.001 {
                    *pixels = add_noise(pixels, w, h, *amount, *monochromatic);
                }
            }
            FilterType::Pixelate { cell } => {
                let c = cell.round().max(1.0) as u32;
                if c > 1 {
                    *pixels = pixelate(pixels, w, h, c);
                }
            }
            FilterType::ReduceNoise { strength } => {
                if *strength > 0.1 {
                    *pixels = reduce_noise(pixels, w, h, *strength);
                }
            }
        }
    }

    /// 16-bit counterpart of [`Self::apply`]: same maths at full precision so a
    /// 16-bit document keeps its 16-bit master through a destructive filter.
    pub fn apply16(&self, pixels: &mut Vec<u16>, w: u32, h: u32) {
        match self {
            FilterType::GaussianBlur { radius } => {
                if *radius > 0.01 {
                    *pixels = gaussian_blur_rgba16(pixels, w, h, *radius);
                }
            }
            FilterType::Sharpen { amount, radius } => {
                if *amount > 0.001 && *radius > 0.01 {
                    *pixels = unsharp_mask16(pixels, w, h, *radius, *amount);
                }
            }
            FilterType::HighPass { radius } => {
                if *radius > 0.01 {
                    *pixels = high_pass16(pixels, w, h, *radius);
                }
            }
            FilterType::AddNoise {
                amount,
                monochromatic,
            } => {
                if *amount > 0.001 {
                    *pixels = add_noise16(pixels, w, h, *amount, *monochromatic);
                }
            }
            FilterType::Pixelate { cell } => {
                let c = cell.round().max(1.0) as u32;
                if c > 1 {
                    *pixels = pixelate16(pixels, w, h, c);
                }
            }
            FilterType::ReduceNoise { strength } => {
                if *strength > 0.1 {
                    *pixels = reduce_noise16(pixels, w, h, *strength);
                }
            }
        }
    }
}

fn boxes_for_gauss(sigma: f32, n: usize) -> Vec<usize> {
    let n_f = n as f32;
    let s2 = sigma * sigma;
    let w_ideal = (12.0 * s2 / n_f + 1.0).sqrt();
    let mut wl = w_ideal.floor() as i32;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wl = wl.max(1);
    let wu = wl + 2;
    let m_ideal = (12.0 * s2 - n_f * (wl * wl) as f32 - 4.0 * n_f * wl as f32 - 3.0 * n_f)
        / (-4.0 * wl as f32 - 4.0);
    let m = m_ideal.round() as i32;
    (0..n)
        .map(|i| {
            if (i as i32) < m {
                wl as usize
            } else {
                wu as usize
            }
        })
        .collect()
}

fn box_blur_h(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize) {
    let r = r.min(w.saturating_sub(1) / 2);
    if r == 0 || h == 0 {
        dst.copy_from_slice(src);
        return;
    }
    let norm = 1.0 / (2 * r + 1) as f32;
    dst.par_chunks_mut(w * 4)
        .zip(src.par_chunks(w * 4))
        .for_each(|(drow, srow)| {
            for c in 0..4 {
                let fv = srow[c];
                let lv = srow[(w - 1) * 4 + c];
                let mut val = fv * (r as f32 + 1.0);
                for j in 0..r {
                    val += srow[j * 4 + c];
                }
                for j in 0..=r {
                    val += srow[(j + r) * 4 + c] - fv;
                    drow[j * 4 + c] = val * norm;
                }
                for j in (r + 1)..(w - r) {
                    val += srow[(j + r) * 4 + c] - srow[(j - r - 1) * 4 + c];
                    drow[j * 4 + c] = val * norm;
                }
                for j in (w - r)..w {
                    val += lv - srow[(j - r - 1) * 4 + c];
                    drow[j * 4 + c] = val * norm;
                }
            }
        });
}

fn box_blur_v(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize) {
    let r = r.min(h.saturating_sub(1) / 2);
    if r == 0 || w == 0 {
        dst.copy_from_slice(src);
        return;
    }
    let norm = 1.0 / (2 * r + 1) as f32;
    let dst_addr = dst.as_mut_ptr() as usize;
    let total = w * h * 4;
    (0..w).into_par_iter().for_each(|x| {
        let dst = unsafe { std::slice::from_raw_parts_mut(dst_addr as *mut f32, total) };
        for c in 0..4 {
            let at = |y: usize| (y * w + x) * 4 + c;
            let fv = src[at(0)];
            let lv = src[at(h - 1)];
            let mut val = fv * (r as f32 + 1.0);
            for j in 0..r {
                val += src[at(j)];
            }
            for j in 0..=r {
                val += src[at(j + r)] - fv;
                dst[at(j)] = val * norm;
            }
            for j in (r + 1)..(h - r) {
                val += src[at(j + r)] - src[at(j - r - 1)];
                dst[at(j)] = val * norm;
            }
            for j in (h - r)..h {
                val += lv - src[at(j - r - 1)];
                dst[at(j)] = val * norm;
            }
        }
    });
}

/// Premultiplied, normalized-linear box-blur core shared by the 8- and 16-bit
/// entry points. `buf` holds premultiplied RGBA in [0,1] and is blurred in
/// place (three box passes approximate a Gaussian). `tmp` is scratch of the
/// same length.
fn gaussian_blur_premul(buf: &mut [f32], tmp: &mut [f32], wi: usize, hi: usize, sigma: f32) {
    for bw in boxes_for_gauss(sigma, 3) {
        let r = bw.saturating_sub(1) / 2;
        if r == 0 {
            continue;
        }
        box_blur_h(buf, tmp, wi, hi, r);
        box_blur_v(tmp, buf, wi, hi, r);
    }
}

pub fn gaussian_blur_rgba(src: &[u8], w: u32, h: u32, sigma: f32) -> Vec<u8> {
    let wi = w as usize;
    let hi = h as usize;
    if sigma <= 0.01 || wi == 0 || hi == 0 || src.len() < wi * hi * 4 {
        return src.to_vec();
    }

    let mut buf = vec![0f32; wi * hi * 4];
    buf.par_chunks_mut(4)
        .zip(src.par_chunks(4))
        .for_each(|(d, s)| {
            let a = s[3] as f32 / 255.0;
            d[0] = s[0] as f32 / 255.0 * a;
            d[1] = s[1] as f32 / 255.0 * a;
            d[2] = s[2] as f32 / 255.0 * a;
            d[3] = a;
        });

    let mut tmp = vec![0f32; wi * hi * 4];
    gaussian_blur_premul(&mut buf, &mut tmp, wi, hi, sigma);

    let mut out = vec![0u8; wi * hi * 4];
    out.par_chunks_mut(4)
        .zip(buf.par_chunks(4))
        .for_each(|(o, p)| {
            let a = p[3];
            if a > 0.0001 {
                o[0] = ((p[0] / a) * 255.0).round().clamp(0.0, 255.0) as u8;
                o[1] = ((p[1] / a) * 255.0).round().clamp(0.0, 255.0) as u8;
                o[2] = ((p[2] / a) * 255.0).round().clamp(0.0, 255.0) as u8;
                o[3] = (a * 255.0).round().clamp(0.0, 255.0) as u8;
            } else {
                o[0] = 0;
                o[1] = 0;
                o[2] = 0;
                o[3] = 0;
            }
        });
    out
}

pub fn gaussian_blur_rgba16(src: &[u16], w: u32, h: u32, sigma: f32) -> Vec<u16> {
    let wi = w as usize;
    let hi = h as usize;
    if sigma <= 0.01 || wi == 0 || hi == 0 || src.len() < wi * hi * 4 {
        return src.to_vec();
    }

    let mut buf = vec![0f32; wi * hi * 4];
    buf.par_chunks_mut(4)
        .zip(src.par_chunks(4))
        .for_each(|(d, s)| {
            let a = s[3] as f32 / 65535.0;
            d[0] = s[0] as f32 / 65535.0 * a;
            d[1] = s[1] as f32 / 65535.0 * a;
            d[2] = s[2] as f32 / 65535.0 * a;
            d[3] = a;
        });

    let mut tmp = vec![0f32; wi * hi * 4];
    gaussian_blur_premul(&mut buf, &mut tmp, wi, hi, sigma);

    let mut out = vec![0u16; wi * hi * 4];
    out.par_chunks_mut(4)
        .zip(buf.par_chunks(4))
        .for_each(|(o, p)| {
            let a = p[3];
            if a > 0.0001 {
                o[0] = ((p[0] / a) * 65535.0).round().clamp(0.0, 65535.0) as u16;
                o[1] = ((p[1] / a) * 65535.0).round().clamp(0.0, 65535.0) as u16;
                o[2] = ((p[2] / a) * 65535.0).round().clamp(0.0, 65535.0) as u16;
                o[3] = (a * 65535.0).round().clamp(0.0, 65535.0) as u16;
            } else {
                o[0] = 0;
                o[1] = 0;
                o[2] = 0;
                o[3] = 0;
            }
        });
    out
}

pub fn unsharp_mask(src: &[u8], w: u32, h: u32, radius: f32, amount: f32) -> Vec<u8> {
    let blurred = gaussian_blur_rgba(src, w, h, radius);
    let mut out = src.to_vec();
    out.par_chunks_mut(4)
        .zip(src.par_chunks(4))
        .zip(blurred.par_chunks(4))
        .for_each(|((o, s), b)| {
            for c in 0..3 {
                let orig = s[c] as f32;
                let blur = b[c] as f32;
                o[c] = (orig + amount * (orig - blur)).round().clamp(0.0, 255.0) as u8;
            }
            o[3] = s[3];
        });
    out
}

pub fn high_pass(src: &[u8], w: u32, h: u32, radius: f32) -> Vec<u8> {
    let blurred = gaussian_blur_rgba(src, w, h, radius);
    let mut out = src.to_vec();
    out.par_chunks_mut(4)
        .zip(src.par_chunks(4))
        .zip(blurred.par_chunks(4))
        .for_each(|((o, s), b)| {
            for c in 0..3 {
                let orig = s[c] as f32;
                let blur = b[c] as f32;
                o[c] = (orig - blur + 128.0).round().clamp(0.0, 255.0) as u8;
            }
            o[3] = s[3];
        });
    out
}

#[inline]
fn noise_hash(x: u32, y: u32, c: u32) -> f32 {
    let mut h = x
        .wrapping_mul(0x9e37_79b9)
        .wrapping_add(y.wrapping_mul(0x85eb_ca6b))
        .wrapping_add(c.wrapping_mul(0xc2b2_ae35))
        .wrapping_add(0x27d4_eb2f);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a_2d39);
    h ^= h >> 15;
    (h as f32) / (u32::MAX as f32 + 1.0)
}

pub fn add_noise(src: &[u8], w: u32, h: u32, amount: f32, monochromatic: bool) -> Vec<u8> {
    let wi = w as usize;
    let hi = h as usize;
    if amount <= 0.001 || wi == 0 || hi == 0 || src.len() < wi * hi * 4 {
        return src.to_vec();
    }
    let mag = amount * 2.55;
    let mut out = src.to_vec();
    out.par_chunks_mut(4).enumerate().for_each(|(p, o)| {
        let x = (p % wi) as u32;
        let y = (p / wi) as u32;
        if monochromatic {
            let n = (noise_hash(x, y, 0) - 0.5) * 2.0 * mag;
            for c in 0..3 {
                o[c] = (o[c] as f32 + n).round().clamp(0.0, 255.0) as u8;
            }
        } else {
            for c in 0..3 {
                let n = (noise_hash(x, y, c as u32) - 0.5) * 2.0 * mag;
                o[c] = (o[c] as f32 + n).round().clamp(0.0, 255.0) as u8;
            }
        }
    });
    out
}

pub fn pixelate(src: &[u8], w: u32, h: u32, cell: u32) -> Vec<u8> {
    let wi = w as usize;
    let hi = h as usize;
    let c = cell.max(1) as usize;
    if c <= 1 || wi == 0 || hi == 0 || src.len() < wi * hi * 4 {
        return src.to_vec();
    }
    let cells_x = (wi + c - 1) / c;
    let cells_y = (hi + c - 1) / c;

    let mut avg = vec![[0u8; 4]; cells_x * cells_y];
    avg.par_iter_mut().enumerate().for_each(|(ci, a)| {
        let (cx, cy) = (ci % cells_x, ci / cells_x);
        let (x0, y0) = (cx * c, cy * c);
        let (x1, y1) = ((x0 + c).min(wi), (y0 + c).min(hi));
        let (mut sr, mut sg, mut sb, mut sa, mut n) = (0f64, 0f64, 0f64, 0f64, 0f64);
        for y in y0..y1 {
            for x in x0..x1 {
                let i = (y * wi + x) * 4;
                let af = src[i + 3] as f64 / 255.0;
                sr += src[i] as f64 * af;
                sg += src[i + 1] as f64 * af;
                sb += src[i + 2] as f64 * af;
                sa += af;
                n += 1.0;
            }
        }
        if n > 0.0 {
            if sa > 1e-6 {
                a[0] = (sr / sa).round().clamp(0.0, 255.0) as u8;
                a[1] = (sg / sa).round().clamp(0.0, 255.0) as u8;
                a[2] = (sb / sa).round().clamp(0.0, 255.0) as u8;
            }
            a[3] = ((sa / n) * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    });

    let mut out = vec![0u8; wi * hi * 4];
    out.par_chunks_mut(wi * 4).enumerate().for_each(|(y, row)| {
        let cy = y / c;
        for x in 0..wi {
            let a = avg[cy * cells_x + (x / c)];
            let i = x * 4;
            row[i..i + 4].copy_from_slice(&a);
        }
    });
    out
}

pub fn reduce_noise(src: &[u8], w: u32, h: u32, strength: f32) -> Vec<u8> {
    let wi = w as usize;
    let hi = h as usize;
    let s = (strength / 100.0).clamp(0.0, 1.0);
    if s <= 0.001 || wi == 0 || hi == 0 || src.len() < wi * hi * 4 {
        return src.to_vec();
    }

    let sigma = 0.8 + s * 2.2;
    let threshold = 0.10;
    let blurred = gaussian_blur_rgba(src, w, h, sigma);

    let mut out = src.to_vec();
    out.par_chunks_mut(4)
        .zip(src.par_chunks(4))
        .zip(blurred.par_chunks(4))
        .for_each(|((o, sp), b)| {
            for c in 0..3 {
                let orig = sp[c] as f32 / 255.0;
                let blur = b[c] as f32 / 255.0;
                let edge = ((orig - blur).abs() / threshold).min(1.0);
                let w_blur = s * (1.0 - edge);
                let v = orig * (1.0 - w_blur) + blur * w_blur;
                o[c] = (v * 255.0).round().clamp(0.0, 255.0) as u8;
            }
            o[3] = sp[3];
        });
    out
}

pub fn unsharp_mask16(src: &[u16], w: u32, h: u32, radius: f32, amount: f32) -> Vec<u16> {
    let blurred = gaussian_blur_rgba16(src, w, h, radius);
    let mut out = src.to_vec();
    out.par_chunks_mut(4)
        .zip(src.par_chunks(4))
        .zip(blurred.par_chunks(4))
        .for_each(|((o, s), b)| {
            for c in 0..3 {
                let orig = s[c] as f32;
                let blur = b[c] as f32;
                o[c] = (orig + amount * (orig - blur)).round().clamp(0.0, 65535.0) as u16;
            }
            o[3] = s[3];
        });
    out
}

pub fn high_pass16(src: &[u16], w: u32, h: u32, radius: f32) -> Vec<u16> {
    let blurred = gaussian_blur_rgba16(src, w, h, radius);
    let mut out = src.to_vec();
    out.par_chunks_mut(4)
        .zip(src.par_chunks(4))
        .zip(blurred.par_chunks(4))
        .for_each(|((o, s), b)| {
            for c in 0..3 {
                let orig = s[c] as f32;
                let blur = b[c] as f32;
                o[c] = (orig - blur + 32768.0).round().clamp(0.0, 65535.0) as u16;
            }
            o[3] = s[3];
        });
    out
}

pub fn add_noise16(src: &[u16], w: u32, h: u32, amount: f32, monochromatic: bool) -> Vec<u16> {
    let wi = w as usize;
    let hi = h as usize;
    if amount <= 0.001 || wi == 0 || hi == 0 || src.len() < wi * hi * 4 {
        return src.to_vec();
    }
    let mag = amount * 655.35;
    let mut out = src.to_vec();
    out.par_chunks_mut(4).enumerate().for_each(|(p, o)| {
        let x = (p % wi) as u32;
        let y = (p / wi) as u32;
        if monochromatic {
            let n = (noise_hash(x, y, 0) - 0.5) * 2.0 * mag;
            for c in 0..3 {
                o[c] = (o[c] as f32 + n).round().clamp(0.0, 65535.0) as u16;
            }
        } else {
            for c in 0..3 {
                let n = (noise_hash(x, y, c as u32) - 0.5) * 2.0 * mag;
                o[c] = (o[c] as f32 + n).round().clamp(0.0, 65535.0) as u16;
            }
        }
    });
    out
}

pub fn pixelate16(src: &[u16], w: u32, h: u32, cell: u32) -> Vec<u16> {
    let wi = w as usize;
    let hi = h as usize;
    let c = cell.max(1) as usize;
    if c <= 1 || wi == 0 || hi == 0 || src.len() < wi * hi * 4 {
        return src.to_vec();
    }
    let cells_x = (wi + c - 1) / c;
    let cells_y = (hi + c - 1) / c;

    let mut avg = vec![[0u16; 4]; cells_x * cells_y];
    avg.par_iter_mut().enumerate().for_each(|(ci, a)| {
        let (cx, cy) = (ci % cells_x, ci / cells_x);
        let (x0, y0) = (cx * c, cy * c);
        let (x1, y1) = ((x0 + c).min(wi), (y0 + c).min(hi));
        let (mut sr, mut sg, mut sb, mut sa, mut n) = (0f64, 0f64, 0f64, 0f64, 0f64);
        for y in y0..y1 {
            for x in x0..x1 {
                let i = (y * wi + x) * 4;
                let af = src[i + 3] as f64 / 65535.0;
                sr += src[i] as f64 * af;
                sg += src[i + 1] as f64 * af;
                sb += src[i + 2] as f64 * af;
                sa += af;
                n += 1.0;
            }
        }
        if n > 0.0 {
            if sa > 1e-6 {
                a[0] = (sr / sa).round().clamp(0.0, 65535.0) as u16;
                a[1] = (sg / sa).round().clamp(0.0, 65535.0) as u16;
                a[2] = (sb / sa).round().clamp(0.0, 65535.0) as u16;
            }
            a[3] = ((sa / n) * 65535.0).round().clamp(0.0, 65535.0) as u16;
        }
    });

    let mut out = vec![0u16; wi * hi * 4];
    out.par_chunks_mut(wi * 4).enumerate().for_each(|(y, row)| {
        let cy = y / c;
        for x in 0..wi {
            let a = avg[cy * cells_x + (x / c)];
            let i = x * 4;
            row[i..i + 4].copy_from_slice(&a);
        }
    });
    out
}

pub fn reduce_noise16(src: &[u16], w: u32, h: u32, strength: f32) -> Vec<u16> {
    let wi = w as usize;
    let hi = h as usize;
    let s = (strength / 100.0).clamp(0.0, 1.0);
    if s <= 0.001 || wi == 0 || hi == 0 || src.len() < wi * hi * 4 {
        return src.to_vec();
    }

    let sigma = 0.8 + s * 2.2;
    let threshold = 0.10;
    let blurred = gaussian_blur_rgba16(src, w, h, sigma);

    let mut out = src.to_vec();
    out.par_chunks_mut(4)
        .zip(src.par_chunks(4))
        .zip(blurred.par_chunks(4))
        .for_each(|((o, sp), b)| {
            for c in 0..3 {
                let orig = sp[c] as f32 / 65535.0;
                let blur = b[c] as f32 / 65535.0;
                let edge = ((orig - blur).abs() / threshold).min(1.0);
                let w_blur = s * (1.0 - edge);
                let v = orig * (1.0 - w_blur) + blur * w_blur;
                o[c] = (v * 65535.0).round().clamp(0.0, 65535.0) as u16;
            }
            o[3] = sp[3];
        });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blur_preserves_flat_color() {
        let (w, h) = (8u32, 8u32);
        let src: Vec<u8> = (0..w * h).flat_map(|_| [120u8, 80, 200, 255]).collect();
        let out = gaussian_blur_rgba(&src, w, h, 2.0);
        assert_eq!(out, src);
    }

    #[test]
    fn blur_zero_radius_is_identity() {
        let (w, h) = (4u32, 4u32);
        let src: Vec<u8> = (0..w * h).flat_map(|i| [i as u8, 0, 0, 255]).collect();
        let out = gaussian_blur_rgba(&src, w, h, 0.0);
        assert_eq!(out, src);
    }

    #[test]
    fn blur_softens_an_edge() {
        let (w, h) = (8u32, 4u32);
        let mut src = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let v = if x < w / 2 { 0 } else { 255 };
                src[i] = v;
                src[i + 1] = v;
                src[i + 2] = v;
                src[i + 3] = 255;
            }
        }
        let out = gaussian_blur_rgba(&src, w, h, 2.0);
        let left = out[((w / 2 - 1) * 4) as usize];
        let right = out[((w / 2) * 4) as usize];
        assert!(left > 0 && left < 255);
        assert!(right > 0 && right < 255);
        assert!(left < right);
    }

    #[test]
    fn blur_large_sigma_small_image_no_panic() {
        let (w, h) = (4u32, 4u32);
        let src: Vec<u8> = (0..w * h)
            .flat_map(|i| [(i * 16) as u8, 0, 0, 255])
            .collect();
        let out = gaussian_blur_rgba(&src, w, h, 50.0);
        assert_eq!(out.len(), src.len());
        for px in out.chunks(4) {
            assert_eq!(px[3], 255);
        }
    }

    #[test]
    fn high_pass_flattens_uniform_to_grey() {
        let (w, h) = (8u32, 8u32);
        let src: Vec<u8> = (0..w * h).flat_map(|_| [40u8, 200, 90, 255]).collect();
        let out = high_pass(&src, w, h, 3.0);
        for px in out.chunks(4) {
            assert_eq!(px[0], 128);
            assert_eq!(px[1], 128);
            assert_eq!(px[2], 128);
            assert_eq!(px[3], 255);
        }
    }

    #[test]
    fn high_pass_keeps_edge_detail() {
        let (w, h) = (8u32, 4u32);
        let mut src = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let v = if x < w / 2 { 0 } else { 255 };
                src[i] = v;
                src[i + 1] = v;
                src[i + 2] = v;
                src[i + 3] = 255;
            }
        }
        let out = high_pass(&src, w, h, 2.0);
        let left = out[((3 * 4) as usize)..][0];
        let right = out[((4 * 4) as usize)..][0];
        assert!(left < 128);
        assert!(right > 128);
    }

    #[test]
    fn noise_zero_amount_is_identity() {
        let (w, h) = (4u32, 4u32);
        let src: Vec<u8> = (0..w * h).flat_map(|i| [i as u8, 50, 90, 255]).collect();
        assert_eq!(add_noise(&src, w, h, 0.0, false), src);
    }

    #[test]
    fn noise_is_deterministic_and_keeps_alpha() {
        let (w, h) = (8u32, 8u32);
        let src: Vec<u8> = (0..w * h).flat_map(|_| [128u8, 128, 128, 200]).collect();
        let a = add_noise(&src, w, h, 40.0, false);
        let b = add_noise(&src, w, h, 40.0, false);
        assert_eq!(a, b);
        assert_ne!(a, src);
        for px in a.chunks(4) {
            assert_eq!(px[3], 200);
        }
    }

    #[test]
    fn pixelate_cell_one_is_identity() {
        let (w, h) = (5u32, 3u32);
        let src: Vec<u8> = (0..w * h).flat_map(|i| [i as u8, 0, 0, 255]).collect();
        assert_eq!(pixelate(&src, w, h, 1), src);
    }

    #[test]
    fn pixelate_block_is_uniform() {
        let (w, h) = (4u32, 4u32);
        let src: Vec<u8> = (0..w * h)
            .flat_map(|i| [(i * 10) as u8, 0, 0, 255])
            .collect();
        let out = pixelate(&src, w, h, 4);
        let first = out[0];
        for px in out.chunks(4) {
            assert_eq!(px[0], first);
        }
    }

    #[test]
    fn reduce_noise_flat_color_is_identity() {
        let (w, h) = (6u32, 6u32);
        let src: Vec<u8> = (0..w * h).flat_map(|_| [70u8, 130, 200, 255]).collect();
        assert_eq!(reduce_noise(&src, w, h, 80.0), src);
    }

    #[test]
    fn reduce_noise_zero_strength_is_identity() {
        let (w, h) = (4u32, 4u32);
        let src: Vec<u8> = (0..w * h).flat_map(|i| [i as u8, 20, 200, 255]).collect();
        assert_eq!(reduce_noise(&src, w, h, 0.0), src);
    }

    #[test]
    fn blur16_preserves_flat_color() {
        let (w, h) = (8u32, 8u32);
        let src: Vec<u16> = (0..w * h)
            .flat_map(|_| [30000u16, 20000, 50000, 65535])
            .collect();
        let out = gaussian_blur_rgba16(&src, w, h, 2.0);
        assert_eq!(out, src);
    }

    #[test]
    fn blur16_keeps_sub_8bit_precision() {
        // An edge between two 16-bit levels that are NOT multiples of 257 (i.e.
        // not representable as v*257 from an 8-bit source). The blurred midtones
        // must carry low-byte detail an 8-bit pass would have banded away.
        let (w, h) = (8u32, 4u32);
        let mut src = vec![0u16; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let v: u16 = if x < w / 2 { 10000 } else { 10300 };
                src[i] = v;
                src[i + 1] = v;
                src[i + 2] = v;
                src[i + 3] = 65535;
            }
        }
        let out = gaussian_blur_rgba16(&src, w, h, 2.0);
        let has_fine = out.chunks(4).any(|px| {
            let v = px[0];
            v > 10000 && v < 10300 && v % 257 != 0
        });
        assert!(has_fine, "16-bit blur must retain sub-8-bit tonal steps");
    }

    #[test]
    fn high_pass16_flattens_uniform_to_mid() {
        let (w, h) = (8u32, 8u32);
        let src: Vec<u16> = (0..w * h)
            .flat_map(|_| [12000u16, 48000, 30000, 65535])
            .collect();
        let out = high_pass16(&src, w, h, 3.0);
        for px in out.chunks(4) {
            assert_eq!(px[0], 32768);
            assert_eq!(px[1], 32768);
            assert_eq!(px[2], 32768);
            assert_eq!(px[3], 65535);
        }
    }

    #[test]
    fn noise16_zero_amount_is_identity_else_changes_keeping_alpha() {
        let (w, h) = (8u32, 8u32);
        let src: Vec<u16> = (0..w * h)
            .flat_map(|_| [32000u16, 32000, 32000, 40000])
            .collect();
        assert_eq!(add_noise16(&src, w, h, 0.0, false), src);
        let noisy = add_noise16(&src, w, h, 40.0, false);
        assert_ne!(noisy, src);
        for px in noisy.chunks(4) {
            assert_eq!(px[3], 40000);
        }
    }

    #[test]
    fn pixelate16_block_is_uniform_and_precise() {
        let (w, h) = (4u32, 4u32);
        let src: Vec<u16> = (0..w * h)
            .flat_map(|i| [(i as u16) * 700 + 13, 0, 0, 65535])
            .collect();
        let out = pixelate16(&src, w, h, 4);
        let first = out[0];
        for px in out.chunks(4) {
            assert_eq!(px[0], first);
        }
    }

    #[test]
    fn reduce_noise_softens_low_amplitude_noise_keeps_alpha() {
        let (w, h) = (6u32, 6u32);
        let mut src = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let v = if (x + y) % 2 == 0 { 90u8 } else { 80u8 };
                src[i] = v;
                src[i + 1] = v;
                src[i + 2] = v;
                src[i + 3] = 200;
            }
        }
        let out = reduce_noise(&src, w, h, 100.0);
        let hi = ((2 * w + 2) * 4) as usize;
        assert_eq!(src[hi], 90);
        assert!(out[hi] < 90);
        for px in out.chunks(4) {
            assert_eq!(px[3], 200);
        }
    }
}

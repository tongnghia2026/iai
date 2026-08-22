//! Output sharpening for export.
//!
//! Downscaling for a delivery size softens an image: the resampler averages
//! detail away, so a 24 MP frame fit to 2000 px looks noticeably less crisp than
//! the original did on screen. Output sharpening compensates by adding a small,
//! bounded luminance high-pass AFTER the resize, at the final pixel grid — the
//! only place it can restore acutance the downscale removed. It is a display-
//! referred, final-output operation and runs on the already-encoded 8-bit RGBA
//! export buffer, not on the scene/Develop pipeline.
//!
//! The kernel is a luminance unsharp mask: blur the luma plane, take the
//! high-pass, tanh-limit it so overshoot never clips hard or rings, and add that
//! single delta to R, G and B equally (achromatic — no hue/saturation shift and
//! no divide-by-luminance instability in dark colours). Flat areas have a zero
//! high-pass and are left untouched, so noise and smooth gradients are not
//! amplified.

/// Unsharp radius. A small sigma (sub-pixel to ~1 px) is what output sharpening
/// wants — it re-crisps the edge the resampler softened without haloing.
const OUTPUT_SHARPEN_SIGMA: f32 = 0.8;
/// Strength at a full (100) slider. The delta is `amount·highpass`, so this is
/// the high-pass gain a full slider applies before the limiter.
const OUTPUT_SHARPEN_MAX_GAIN: f32 = 1.4;
/// tanh ceiling on the per-pixel luminance delta (in [0,1]). Bounds overshoot so
/// a hard edge cannot blow to pure black/white or ring.
const OUTPUT_SHARPEN_LIMIT: f32 = 0.18;

/// Apply output sharpening in place to an 8-bit RGBA buffer. `amount` is 0..=100
/// (0 = no-op). Alpha is left untouched.
pub fn apply_output_sharpen(rgba: &mut [u8], width: usize, height: usize, amount: u8) {
    let n = width.saturating_mul(height);
    if amount == 0 || n == 0 || rgba.len() < n * 4 || width < 3 || height < 3 {
        return;
    }
    let gain = (amount as f32 / 100.0).clamp(0.0, 1.0) * OUTPUT_SHARPEN_MAX_GAIN;
    if gain <= 0.0 {
        return;
    }

    // Luma in the display (gamma) domain — the correct domain for output
    // sharpening of an already-encoded export buffer.
    let luma: Vec<f32> = (0..n)
        .map(|i| {
            let r = rgba[i * 4] as f32 / 255.0;
            let g = rgba[i * 4 + 1] as f32 / 255.0;
            let b = rgba[i * 4 + 2] as f32 / 255.0;
            0.2126 * r + 0.7152 * g + 0.0722 * b
        })
        .collect();
    let blur = gaussian_blur_plane(&luma, width, height, OUTPUT_SHARPEN_SIGMA);

    for i in 0..n {
        let hp = luma[i] - blur[i];
        let delta = OUTPUT_SHARPEN_LIMIT * (gain * hp / OUTPUT_SHARPEN_LIMIT).tanh();
        if delta == 0.0 {
            continue;
        }
        for c in 0..3 {
            let v = rgba[i * 4 + c] as f32 / 255.0 + delta;
            rgba[i * 4 + c] = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
}

/// Separable Gaussian blur of a single plane, edge-clamped. Radius = ⌈3σ⌉.
fn gaussian_blur_plane(src: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    let radius = (sigma * 3.0).ceil().max(1.0) as isize;
    let inv = 1.0 / (2.0 * sigma * sigma);
    let mut kernel: Vec<f32> = (-radius..=radius)
        .map(|d| (-(d * d) as f32 * inv).exp())
        .collect();
    let sum: f32 = kernel.iter().sum();
    for k in kernel.iter_mut() {
        *k /= sum;
    }

    let sample = |plane: &[f32], x: isize, y: isize| -> f32 {
        let xs = x.clamp(0, w as isize - 1) as usize;
        let ys = y.clamp(0, h as isize - 1) as usize;
        plane[ys * w + xs]
    };

    let mut tmp = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (k, &kv) in kernel.iter().enumerate() {
                let o = k as isize - radius;
                acc += sample(src, x as isize + o, y as isize) * kv;
            }
            tmp[y * w + x] = acc;
        }
    }
    let mut out = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (k, &kv) in kernel.iter().enumerate() {
                let o = k as isize - radius;
                acc += sample(&tmp, x as isize, y as isize + o) * kv;
            }
            out[y * w + x] = acc;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mean absolute horizontal luminance gradient — a proxy for edge acutance.
    fn acutance(rgba: &[u8], w: usize, h: usize) -> f32 {
        let mut sum = 0.0f64;
        let mut count = 0u64;
        for y in 0..h {
            for x in 1..w {
                let l = |xx: usize| {
                    let i = (y * w + xx) * 4;
                    0.2126 * rgba[i] as f32
                        + 0.7152 * rgba[i + 1] as f32
                        + 0.0722 * rgba[i + 2] as f32
                };
                sum += (l(x) - l(x - 1)).abs() as f64;
                count += 1;
            }
        }
        (sum / count as f64) as f32
    }

    /// A softened vertical edge must gain acutance after sharpening, and a flat
    /// field must be left untouched (no noise amplification / no drift).
    #[test]
    fn output_sharpen_recrisps_soft_edge_and_spares_flat() {
        let w = 32usize;
        let h = 16usize;
        // Soft edge: a 4-px linear ramp from dark to light across the centre.
        let mut soft = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let t = ((x as f32 - 14.0) / 4.0).clamp(0.0, 1.0);
                let v = (40.0 + t * (215.0 - 40.0)).round() as u8;
                let i = (y * w + x) * 4;
                soft[i] = v;
                soft[i + 1] = v;
                soft[i + 2] = v;
                soft[i + 3] = 255;
            }
        }
        let before = acutance(&soft, w, h);
        apply_output_sharpen(&mut soft, w, h, 100);
        let after = acutance(&soft, w, h);
        assert!(
            after > before * 1.15,
            "sharpening should raise edge acutance: {before} -> {after}"
        );
        // Overshoot is bounded: no channel is pushed to a hard 0/255 clip that a
        // soft mid-grey edge (40..215) had no business reaching.
        assert!(
            soft.chunks_exact(4).all(|p| p[0] > 0 && p[0] < 255),
            "bounded overshoot must not hard-clip a mid-contrast edge"
        );

        // Flat mid-grey: high-pass is zero everywhere → pixels unchanged.
        let mut flat = vec![128u8; w * h * 4];
        for p in flat.chunks_exact_mut(4) {
            p[3] = 255;
        }
        let flat_ref = flat.clone();
        apply_output_sharpen(&mut flat, w, h, 100);
        assert_eq!(flat, flat_ref, "a flat field must be left untouched");
    }

    #[test]
    fn amount_zero_is_noop() {
        let mut img = vec![0u8; 8 * 8 * 4];
        for (i, p) in img.iter_mut().enumerate() {
            *p = (i % 251) as u8;
        }
        let reference = img.clone();
        apply_output_sharpen(&mut img, 8, 8, 0);
        assert_eq!(img, reference);
    }
}

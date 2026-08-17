//! "Làm sạch bản scan": flatten an uneven, greyed scan background to white and
//! deepen the text, for cleaning up book/scanner captures whose paper reads grey
//! and whose gutter is shadowed.
//!
//! Method — divide by a **morphological-closing** background estimate
//! (rolling-ball style), which beats a plain blur (or a high-pass/subtract):
//!  1. Estimate the local paper level by grayscale *closing* (dilate then erode)
//!     of a downscaled luma image. Closing erases the thin dark strokes of text
//!     and leaves only the smooth paper illumination — and, unlike a blur, it
//!     tracks the sharp gutter shadow without bleeding brightness across it.
//!  2. Divide each pixel's luma by that background (illumination is multiplicative,
//!     so a divide — not a subtract — flattens uneven shading to a uniform white).
//!  3. Clamp with a white/black point so faint grey snaps to paper and text darkens.
//!
//! Output is neutral grey (paper→white, text→black), which removes any uneven
//! colour cast from the scan; `Bilevel` thresholds that to pure black-on-white
//! for the cleanest print. Runs on an 8-bit RGBA buffer; alpha is preserved.

/// Output character of a cleanup pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanCleanupMode {
    /// Flatten to a clean neutral grayscale (paper white, text black).
    Grayscale,
    /// Hard two-level output (black text on white) for the cleanest print.
    Bilevel,
}

/// Tunable parameters for one cleanup pass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScanCleanupParams {
    pub mode: ScanCleanupMode,
    /// 0 = original, 1 = full effect. Blends the cleaned result over the input.
    pub strength: f32,
}

impl Default for ScanCleanupParams {
    fn default() -> Self {
        Self {
            mode: ScanCleanupMode::Grayscale,
            strength: 1.0,
        }
    }
}

/// Which pages a batch cleanup touches. `Range` bounds are 1-based inclusive
/// (as typed in the dialog) and clamped to the document on apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanCleanScope {
    /// Just the page/image currently on screen.
    CurrentPage,
    /// A 1-based inclusive page range.
    Range { from: usize, to: usize },
    /// Every page of the PDF.
    AllPages,
}

/// A full cleanup request from the dialog: how to clean, and what to clean.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScanCleanupRequest {
    pub params: ScanCleanupParams,
    pub scope: ScanCleanScope,
}

/// After dividing by the background, paper ≈ 1.0. Pixels at/above this become
/// pure white; at/below `BLACK_POINT` become pure black; linear between. The
/// window is deliberately narrow so the flattened grey gains contrast (text
/// deepens, paper whitens) rather than reading flat.
const WHITE_POINT: f32 = 0.86;
const BLACK_POINT: f32 = 0.26;
/// Bilevel split on the raw background-normalised level (paper ≈ 1.0): pixels
/// darker than this fraction of the local paper become black. Higher keeps more
/// (and thinner) strokes intact; lower is cleaner but drops faint text.
const BILEVEL_NORM_THRESHOLD: f32 = 0.72;
/// Long edge of the working image the background is estimated on. Downscaling
/// keeps the (radius-heavy) morphology cheap and doubles as a first smoothing.
const BG_WORK: usize = 1000;

/// Per-pixel luma (0..1) of an 8-bit RGBA buffer.
pub fn luma_of(src: &[u8]) -> Vec<f32> {
    src.chunks_exact(4)
        .map(|px| (0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32) / 255.0)
        .collect()
}

/// Clean an 8-bit RGBA image. Returns a new buffer of the same length; the
/// input is returned unchanged for a degenerate size or zero strength.
///
/// This computes `luma` + the rolling-ball `background` from scratch. A live
/// preview should instead cache those once (they don't depend on the params)
/// and call [`apply_with_background`] on every slider change — see the
/// scan-cleanup preview session.
pub fn clean_scan_rgba(src: &[u8], w: u32, h: u32, params: ScanCleanupParams) -> Vec<u8> {
    let (wu, hu) = (w as usize, h as usize);
    let strength = params.strength.clamp(0.0, 1.0);
    if wu == 0 || hu == 0 || src.len() < wu * hu * 4 || strength <= 0.0 {
        return src.to_vec();
    }
    let luma = luma_of(src);
    let bg = estimate_background(&luma, wu, hu);
    apply_with_background(src, &bg, &luma, params)
}

/// The cheap final pass: divide each pixel's luma by the pre-computed background,
/// clamp with the white/black point, and blend the neutral result over the input
/// by `strength`. `bg` and `luma` are per-pixel (as from [`estimate_background`]
/// and [`luma_of`] of the same image). Fast enough to rerun every frame.
pub fn apply_with_background(
    src: &[u8],
    bg: &[f32],
    luma: &[f32],
    params: ScanCleanupParams,
) -> Vec<u8> {
    let n = src.len() / 4;
    let strength = params.strength.clamp(0.0, 1.0);
    if strength <= 0.0 || bg.len() < n || luma.len() < n {
        return src.to_vec();
    }
    let inv_span = 1.0 / (WHITE_POINT - BLACK_POINT);
    let mut out = src.to_vec();
    for (i, chunk) in out.chunks_exact_mut(4).enumerate() {
        let bg_level = bg[i].max(1e-3);
        // paper → ≈1.0, ink → <1.
        let norm = (luma[i] / bg_level).min(1.3);
        // Neutral grey (grayscale) or a hard threshold (bilevel). Either way the
        // target is achromatic, which erases any uneven colour cast from the scan.
        let target = match params.mode {
            ScanCleanupMode::Grayscale => {
                (((norm - BLACK_POINT) * inv_span).clamp(0.0, 1.0)) * 255.0
            }
            // Threshold on the paper-relative level so faint strokes survive.
            ScanCleanupMode::Bilevel => {
                if norm >= BILEVEL_NORM_THRESHOLD {
                    255.0
                } else {
                    0.0
                }
            }
        };
        for c in chunk.iter_mut().take(3) {
            let orig = *c as f32;
            *c = (orig * (1.0 - strength) + target * strength)
                .clamp(0.0, 255.0)
                .round() as u8;
        }
    }
    out
}

/// Estimate the local background (paper) luma for every pixel by grayscale
/// morphological closing of a downscaled copy: dilate (local max) then erode
/// (local min) erases the thin dark strokes of text, leaving smooth paper that
/// tracks the shading — including a sharp gutter shadow — far better than a blur.
/// A light box blur removes residual texture; the result is bilinearly upsampled.
pub fn estimate_background(luma: &[f32], w: usize, h: usize) -> Vec<f32> {
    // Downscale so the long edge is ~BG_WORK (cheap morphology + first smoothing).
    let ds = w.max(h).div_ceil(BG_WORK).max(1);
    let sw = w.div_ceil(ds);
    let sh = h.div_ceil(ds);
    let mut small = vec![0f32; sw * sh];
    for sy in 0..sh {
        let y0 = sy * ds;
        let y1 = (y0 + ds).min(h);
        for sx in 0..sw {
            let x0 = sx * ds;
            let x1 = (x0 + ds).min(w);
            let mut sum = 0f32;
            let mut n = 0f32;
            for y in y0..y1 {
                let row = y * w;
                for x in x0..x1 {
                    sum += luma[row + x];
                    n += 1.0;
                }
            }
            small[sy * sw + sx] = if n > 0.0 { sum / n } else { 0.0 };
        }
    }

    // Closing radius (in working pixels): big enough to swallow text strokes,
    // small enough to follow the illumination. Then a light blur to de-texture.
    let radius = (sw.max(sh) / 80).max(4);
    let dilated = morph(&small, sw, sh, radius, true);
    let closed = morph(&dilated, sw, sh, radius, false);
    let bg_small = box_blur_grid(&closed, sw, sh, (radius / 2).max(1));

    // Bilinear upsample to full resolution.
    let mut bg = vec![1f32; w * h];
    let ds_f = ds as f32;
    for (y, bg_row) in bg.chunks_exact_mut(w).enumerate() {
        let fy = ((y as f32 + 0.5) / ds_f - 0.5).clamp(0.0, (sh - 1) as f32);
        let gy0 = fy.floor() as usize;
        let gy1 = (gy0 + 1).min(sh - 1);
        let ty = fy - gy0 as f32;
        for (x, dst) in bg_row.iter_mut().enumerate() {
            let fx = ((x as f32 + 0.5) / ds_f - 0.5).clamp(0.0, (sw - 1) as f32);
            let gx0 = fx.floor() as usize;
            let gx1 = (gx0 + 1).min(sw - 1);
            let tx = fx - gx0 as f32;
            let a = bg_small[gy0 * sw + gx0];
            let b = bg_small[gy0 * sw + gx1];
            let c = bg_small[gy1 * sw + gx0];
            let d = bg_small[gy1 * sw + gx1];
            let top = a + (b - a) * tx;
            let bot = c + (d - c) * tx;
            *dst = top + (bot - top) * ty;
        }
    }
    bg
}

/// Separable grayscale morphology (dilation when `take_max`, erosion otherwise)
/// with a square structuring element of the given radius. Windows shrink at the
/// edges rather than wrapping.
fn morph(src: &[f32], w: usize, h: usize, radius: usize, take_max: bool) -> Vec<f32> {
    let pick = |a: f32, b: f32| if take_max { a.max(b) } else { a.min(b) };
    // Horizontal pass.
    let mut tmp = vec![0f32; w * h];
    for y in 0..h {
        let row = y * w;
        for x in 0..w {
            let x0 = x.saturating_sub(radius);
            let x1 = (x + radius).min(w - 1);
            let mut v = src[row + x0];
            for xx in (x0 + 1)..=x1 {
                v = pick(v, src[row + xx]);
            }
            tmp[row + x] = v;
        }
    }
    // Vertical pass.
    let mut out = vec![0f32; w * h];
    for x in 0..w {
        for y in 0..h {
            let y0 = y.saturating_sub(radius);
            let y1 = (y + radius).min(h - 1);
            let mut v = tmp[y0 * w + x];
            for yy in (y0 + 1)..=y1 {
                v = pick(v, tmp[yy * w + x]);
            }
            out[y * w + x] = v;
        }
    }
    out
}

/// Separable box blur over a small grid. Edges shrink the window rather than
/// wrapping or clamping to a constant.
fn box_blur_grid(src: &[f32], gw: usize, gh: usize, radius: usize) -> Vec<f32> {
    if radius == 0 || gw == 0 || gh == 0 {
        return src.to_vec();
    }
    let mut tmp = vec![0f32; gw * gh];
    for y in 0..gh {
        for x in 0..gw {
            let x0 = x.saturating_sub(radius);
            let x1 = (x + radius).min(gw - 1);
            let mut sum = 0f32;
            for xx in x0..=x1 {
                sum += src[y * gw + xx];
            }
            tmp[y * gw + x] = sum / (x1 - x0 + 1) as f32;
        }
    }
    let mut out = vec![0f32; gw * gh];
    for y in 0..gh {
        let y0 = y.saturating_sub(radius);
        let y1 = (y + radius).min(gh - 1);
        for x in 0..gw {
            let mut sum = 0f32;
            for yy in y0..=y1 {
                sum += tmp[yy * gw + x];
            }
            out[y * gw + x] = sum / (y1 - y0 + 1) as f32;
        }
    }
    out
}

/// Resolve a batch scope to the concrete 0-based page indices to clean, in
/// ascending order. `active_page` is 0-based.
pub fn resolve_pages(scope: ScanCleanScope, page_count: usize, active_page: usize) -> Vec<usize> {
    match scope {
        ScanCleanScope::CurrentPage => {
            if active_page < page_count {
                vec![active_page]
            } else {
                Vec::new()
            }
        }
        ScanCleanScope::AllPages => (0..page_count).collect(),
        ScanCleanScope::Range { from, to } => {
            if page_count == 0 {
                return Vec::new();
            }
            let lo = from.max(1);
            let hi = to.max(lo);
            let start = (lo - 1).min(page_count - 1);
            let end = (hi - 1).min(page_count - 1);
            (start..=end).collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A white page darkened toward the right edge (a gutter shadow) with thin
    /// dark text strokes (as real glyphs are — not a solid block), so we can
    /// check the shadow flattens while the strokes stay dark.
    fn shaded_page(w: u32, h: u32) -> Vec<u8> {
        let mut px = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                // ~235 on the left fading to ~155 on the right.
                let shade = 235u32.saturating_sub(x as u32 * 80 / w as u32) as u8;
                // 2px-wide vertical strokes with 2px paper gaps (thin text).
                let stroke =
                    (18..46).contains(&y) && matches!(x % 4, 0 | 1) && (18..32).contains(&x);
                let v = if stroke { 20 } else { shade };
                px[i] = v;
                px[i + 1] = v;
                px[i + 2] = v;
                px[i + 3] = 255;
            }
        }
        px
    }

    #[test]
    fn flattens_uneven_background_to_white_keeps_text_dark() {
        let (w, h) = (128u32, 128u32);
        let px = shaded_page(w, h);
        let out = clean_scan_rgba(
            &px,
            w,
            h,
            ScanCleanupParams {
                mode: ScanCleanupMode::Grayscale,
                strength: 1.0,
            },
        );
        // A shadowed right-edge background pixel is pushed to near-white.
        let bg = ((10 * w + (w - 3)) * 4) as usize;
        assert!(
            out[bg] > 235,
            "shadowed background should whiten, got {}",
            out[bg]
        );
        // A stroke pixel (x=20, y=30) stays dark.
        let t = ((30 * w + 20) * 4) as usize;
        assert!(out[t] < 90, "text should stay dark, got {}", out[t]);
        // Output is neutral grey.
        assert_eq!(out[bg + 1], out[bg]);
        assert_eq!(out[bg + 3], 255);
    }

    #[test]
    fn bilevel_is_pure_black_and_white() {
        let (w, h) = (128u32, 128u32);
        let px = shaded_page(w, h);
        let out = clean_scan_rgba(
            &px,
            w,
            h,
            ScanCleanupParams {
                mode: ScanCleanupMode::Bilevel,
                strength: 1.0,
            },
        );
        for chunk in out.chunks_exact(4) {
            assert!(
                chunk[0] == 0 || chunk[0] == 255,
                "bilevel output must be 0 or 255, got {}",
                chunk[0]
            );
        }
    }

    #[test]
    fn zero_strength_is_identity() {
        let (w, h) = (32u32, 32u32);
        let px = shaded_page(w, h);
        let out = clean_scan_rgba(
            &px,
            w,
            h,
            ScanCleanupParams {
                mode: ScanCleanupMode::Grayscale,
                strength: 0.0,
            },
        );
        assert_eq!(out, px);
    }

    #[test]
    fn degenerate_size_returns_copy() {
        assert_eq!(
            clean_scan_rgba(&[], 0, 0, ScanCleanupParams::default()),
            Vec::<u8>::new()
        );
    }

    #[test]
    fn resolve_pages_covers_scopes() {
        assert_eq!(resolve_pages(ScanCleanScope::CurrentPage, 10, 3), vec![3]);
        assert_eq!(resolve_pages(ScanCleanScope::AllPages, 3, 0), vec![0, 1, 2]);
        // 1-based inclusive range, clamped.
        assert_eq!(
            resolve_pages(ScanCleanScope::Range { from: 2, to: 4 }, 10, 0),
            vec![1, 2, 3]
        );
        assert_eq!(
            resolve_pages(ScanCleanScope::Range { from: 8, to: 100 }, 10, 0),
            vec![7, 8, 9]
        );
        // Degenerate inputs don't panic.
        assert!(resolve_pages(ScanCleanScope::AllPages, 0, 0).is_empty());
        assert!(resolve_pages(ScanCleanScope::CurrentPage, 0, 0).is_empty());
    }
}

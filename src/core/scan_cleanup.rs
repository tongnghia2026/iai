//! "Làm sạch bản scan": flatten an uneven, greyed scan background to white and
//! deepen the text, for cleaning up book/scanner captures whose paper reads grey
//! and whose gutter is shadowed.
//!
//! The maths is illumination normalisation: estimate the local paper level
//! (background) on a coarse grid, divide each pixel by it so uneven shading
//! flattens to a uniform white, then clamp with a white/black point so faint
//! grey snaps to paper-white and text darkens. `Grayscale` mode keeps tone and
//! colour (scaling each channel by the same factor); `Bilevel` produces a hard
//! black-on-white result for the cleanest print. Everything runs on an 8-bit
//! RGBA buffer in linear index order; alpha is preserved.

/// Output character of a cleanup pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanCleanupMode {
    /// Keep tone/colour — just flatten the background to white and deepen text.
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

/// Pixels at or above this normalised level (paper ≈ 1.0) become pure white.
const WHITE_POINT: f32 = 0.85;
/// Pixels at or below this normalised level become pure black.
const BLACK_POINT: f32 = 0.20;
/// Bilevel split on the normalised level.
const BILEVEL_THRESHOLD: f32 = 0.60;
/// Roughly how many background samples span the long edge. Larger cells are
/// more likely to contain paper (not just ink) and smooth the gutter shadow;
/// the estimate is upsampled bilinearly so cell size doesn't band the output.
const BG_CELLS: usize = 24;
/// Never let a background cell shrink below this many pixels, so a cell always
/// straddles some paper even on small inputs.
const BG_CELL_MIN: usize = 16;

/// Clean an 8-bit RGBA image. Returns a new buffer of the same length; the
/// input is returned unchanged for a degenerate size or zero strength.
pub fn clean_scan_rgba(src: &[u8], w: u32, h: u32, params: ScanCleanupParams) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let strength = params.strength.clamp(0.0, 1.0);
    if w == 0 || h == 0 || src.len() < w * h * 4 || strength <= 0.0 {
        return src.to_vec();
    }

    // Per-pixel luma (0..1).
    let mut luma = vec![0f32; w * h];
    for (l, px) in luma.iter_mut().zip(src.chunks_exact(4)) {
        *l = (0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32) / 255.0;
    }

    // Local paper level, full-resolution.
    let bg = estimate_background(&luma, w, h);

    let inv_span = 1.0 / (WHITE_POINT - BLACK_POINT);
    let mut out = src.to_vec();
    for (i, chunk) in out.chunks_exact_mut(4).enumerate() {
        let bg_level = bg[i].max(1e-3);
        // paper → ≈1.0, ink → <1. Cap so a slightly-brighter-than-estimate paper
        // pixel doesn't explode the per-channel scale.
        let norm = (luma[i] / bg_level).min(1.5);
        match params.mode {
            ScanCleanupMode::Grayscale => {
                let cleaned_l = ((norm - BLACK_POINT) * inv_span).clamp(0.0, 1.0);
                // Scale every channel by the same factor so colour is preserved
                // (a near-neutral scan simply whitens; text goes to black).
                let cur = luma[i].max(1e-3);
                let scale = cleaned_l / cur;
                for c in chunk.iter_mut().take(3) {
                    let orig = *c as f32;
                    let v = orig * scale;
                    *c = (orig * (1.0 - strength) + v * strength)
                        .clamp(0.0, 255.0)
                        .round() as u8;
                }
            }
            ScanCleanupMode::Bilevel => {
                let v = if norm >= BILEVEL_THRESHOLD {
                    255.0
                } else {
                    0.0
                };
                for c in chunk.iter_mut().take(3) {
                    let orig = *c as f32;
                    *c = (orig * (1.0 - strength) + v * strength)
                        .clamp(0.0, 255.0)
                        .round() as u8;
                }
            }
        }
    }
    out
}

/// Estimate the local background (paper) luma for every pixel: take the
/// brightest sample per coarse cell (paper is the brightest thing locally, ink
/// is darker), smooth the coarse grid, then bilinearly upsample to full size.
fn estimate_background(luma: &[f32], w: usize, h: usize) -> Vec<f32> {
    let cell = (w.max(h) / BG_CELLS).max(BG_CELL_MIN);
    let gw = w.div_ceil(cell);
    let gh = h.div_ceil(cell);

    let mut grid = vec![0f32; gw * gh];
    for gy in 0..gh {
        let y0 = gy * cell;
        let y1 = (y0 + cell).min(h);
        for gx in 0..gw {
            let x0 = gx * cell;
            let x1 = (x0 + cell).min(w);
            let mut m = 0f32;
            for y in y0..y1 {
                let row = y * w;
                for x in x0..x1 {
                    let v = luma[row + x];
                    if v > m {
                        m = v;
                    }
                }
            }
            grid[gy * gw + gx] = m;
        }
    }

    // Smooth cell seams (two light box passes).
    let grid = box_blur_grid(&grid, gw, gh, 1);
    let grid = box_blur_grid(&grid, gw, gh, 1);

    let mut bg = vec![1f32; w * h];
    let cell_f = cell as f32;
    for (y, bg_row) in bg.chunks_exact_mut(w).enumerate() {
        let fy = ((y as f32 + 0.5) / cell_f - 0.5).clamp(0.0, (gh - 1) as f32);
        let gy0 = fy.floor() as usize;
        let gy1 = (gy0 + 1).min(gh - 1);
        let ty = fy - gy0 as f32;
        for (x, dst) in bg_row.iter_mut().enumerate() {
            let fx = ((x as f32 + 0.5) / cell_f - 0.5).clamp(0.0, (gw - 1) as f32);
            let gx0 = fx.floor() as usize;
            let gx1 = (gx0 + 1).min(gw - 1);
            let tx = fx - gx0 as f32;
            let a = grid[gy0 * gw + gx0];
            let b = grid[gy0 * gw + gx1];
            let c = grid[gy1 * gw + gx0];
            let d = grid[gy1 * gw + gx1];
            let top = a + (b - a) * tx;
            let bot = c + (d - c) * tx;
            *dst = top + (bot - top) * ty;
        }
    }
    bg
}

/// Separable box blur over a small grid (radius in cells). Edges shrink the
/// window rather than wrapping or clamping to a constant.
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

    /// Build a white page darkened toward the right edge (a gutter shadow) with
    /// a dark text block, so we can check the shadow flattens and text stays.
    fn shaded_page(w: u32, h: u32) -> Vec<u8> {
        let mut px = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                // ~235 on the left fading to ~155 on the right.
                let shade = 235u32.saturating_sub(x as u32 * 80 / w as u32) as u8;
                let text = (18..30).contains(&x) && (18..46).contains(&y);
                let v = if text { 20 } else { shade };
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
        let (w, h) = (64u32, 64u32);
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
        // A text pixel stays dark.
        let t = ((30 * w + 22) * 4) as usize;
        assert!(out[t] < 90, "text should stay dark, got {}", out[t]);
        // Alpha untouched.
        assert_eq!(out[bg + 1], out[bg]); // grey stays neutral
        assert_eq!(out[bg + 3], 255);
    }

    #[test]
    fn bilevel_is_pure_black_and_white() {
        let (w, h) = (64u32, 64u32);
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
        let (w, h) = (16u32, 16u32);
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

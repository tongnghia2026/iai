//! Point (tone) curves and the live histogram: curve evaluation, the
//! per-channel RGB curve LUTs, and the downsampled histogram proxy the panel
//! reads.

use super::*;
use crate::core::color::luminance_f32;
use crate::core::tile::TileMap;
use rayon::prelude::*;

pub fn identity_curve() -> Vec<[f32; 2]> {
    vec![[0.0, 0.0], [1.0, 1.0]]
}

/// A point curve is identity when every control point sits on the diagonal —
/// the monotone Hermite spline through diagonal points IS the diagonal.
pub fn curve_is_identity(points: &[[f32; 2]]) -> bool {
    points.iter().all(|p| (p[1] - p[0]).abs() <= 0.001)
}

/// Evaluate the point curve at `x` with a monotone cubic Hermite spline
/// (Fritsch–Carlson tangents): passes exactly through the control points and
/// never overshoots between two points that rise or fall together. Outside the
/// first/last point the curve is clamped flat. `points` must be sorted by x.
pub fn eval_point_curve(points: &[[f32; 2]], x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    let n = points.len();
    if n == 0 {
        return x;
    }
    if n == 1 {
        return points[0][1].clamp(0.0, 1.0);
    }
    if x <= points[0][0] {
        return points[0][1].clamp(0.0, 1.0);
    }
    if x >= points[n - 1][0] {
        return points[n - 1][1].clamp(0.0, 1.0);
    }

    // Secant slopes and Fritsch–Carlson-limited tangents.
    let mut d = vec![0.0f32; n - 1];
    for i in 0..n - 1 {
        let dx = (points[i + 1][0] - points[i][0]).max(1e-6);
        d[i] = (points[i + 1][1] - points[i][1]) / dx;
    }
    let mut m = vec![0.0f32; n];
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for i in 1..n - 1 {
        m[i] = if d[i - 1] * d[i] <= 0.0 {
            0.0
        } else {
            (d[i - 1] + d[i]) * 0.5
        };
    }
    for i in 0..n - 1 {
        if d[i].abs() < 1e-9 {
            m[i] = 0.0;
            m[i + 1] = 0.0;
        } else {
            let a = m[i] / d[i];
            let b = m[i + 1] / d[i];
            let s = a * a + b * b;
            if s > 9.0 {
                let t = 3.0 / s.sqrt();
                m[i] = t * a * d[i];
                m[i + 1] = t * b * d[i];
            }
        }
    }

    let mut i = 0;
    while i < n - 2 && x > points[i + 1][0] {
        i += 1;
    }
    let h = (points[i + 1][0] - points[i][0]).max(1e-6);
    let t = ((x - points[i][0]) / h).clamp(0.0, 1.0);
    let h00 = (1.0 + 2.0 * t) * (1.0 - t) * (1.0 - t);
    let h10 = t * (1.0 - t) * (1.0 - t);
    let h01 = t * t * (3.0 - 2.0 * t);
    let h11 = t * t * (t - 1.0);
    (h00 * points[i][1] + h10 * h * m[i] + h01 * points[i + 1][1] + h11 * h * m[i + 1])
        .clamp(0.0, 1.0)
}

/// Bake a point curve to a 256-entry LUT (no monotone enforcement — an
/// intentionally inverting point curve is legal, unlike the parametric curve).
pub(crate) fn point_curve_lut(points: &[[f32; 2]]) -> [f32; 256] {
    let mut lut = [0.0f32; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        *slot = eval_point_curve(points, i as f32 / 255.0);
    }
    lut
}

/// Map a finished tone LUT through the luminance point curve (outermost stage,
/// AFTER the parametric monotone pass — an intentionally inverting point curve
/// must survive, so it cannot go through that pass itself).
pub(crate) fn apply_point_curve_outer(lut: &mut [f32; 256], settings: &DevelopSettings) {
    if curve_is_identity(&settings.curve_points) {
        return;
    }
    let pc = point_curve_lut(&settings.curve_points);
    for slot in lut.iter_mut() {
        *slot = lut_lerp(&pc, *slot);
    }
}

/// Pixel budget for the live-histogram source proxy: enough samples for a
/// smooth 256-bin backdrop, small enough that re-binning through the
/// tone+colour stages on every slider tick stays negligible.
const HISTOGRAM_PROXY_PIXELS: u64 = 60_000;

/// Coordinate-preserving twin of the histogram proxy for waveform/parade.
/// Transparent samples remain in the raster but are excluded by the scope
/// analyzer, so their neighbours never slide into the wrong x column.
pub(crate) fn build_scope_source_proxy(
    tiles: &TileMap,
) -> crate::core::develop2::scopes::ScopeSourceProxy {
    crate::core::develop2::scopes::ScopeSourceProxy::sample(
        tiles.width,
        tiles.height,
        HISTOGRAM_PROXY_PIXELS,
        |x, y| {
            let (r, g, b, a) = tiles.get_pixel(x, y);
            (
                [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0],
                a > 0,
            )
        },
    )
}

/// Render cached display-domain source samples through the current legacy
/// settings. The returned pixels are encoded sRGB (`DISPLAY_SINK`).
pub(crate) fn render_scope_source_proxy(
    proxy: &crate::core::develop2::scopes::ScopeSourceProxy,
    settings: &DevelopSettings,
) -> Vec<[f32; 3]> {
    let tone = tone_is_active(settings).then(|| build_tone_data(settings));
    let use_color = has_color(settings);
    let curves = build_mixer_curves_opt(settings);
    proxy
        .pixels
        .iter()
        .map(|p| {
            let (mut r, mut g, mut b) = (p[0], p[1], p[2]);
            if let Some(tone) = &tone {
                tone.apply(&mut r, &mut g, &mut b);
                clamp_unit(&mut r, &mut g, &mut b);
            }
            if use_color {
                apply_color(settings, curves.as_ref(), &mut r, &mut g, &mut b);
                clamp_unit(&mut r, &mut g, &mut b);
            }
            [r, g, b]
        })
        .collect()
}

/// Grid-sampled source pixels (transparent skipped), cached for the Develop
/// session. The curve-editor histogram must show the image AFTER the current
/// settings, so it is re-binned from this proxy through the tone+colour
/// stages on every change instead of being scanned once from the full image.
pub fn build_histogram_proxy(tiles: &TileMap) -> Vec<[f32; 3]> {
    let (w, h) = (tiles.width, tiles.height);
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let total = w as u64 * h as u64;
    let step = (((total / HISTOGRAM_PROXY_PIXELS).max(1) as f64)
        .sqrt()
        .ceil() as u32)
        .max(1);
    let mut out = Vec::with_capacity((total / (step as u64 * step as u64) + 1) as usize);
    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            let (r, g, b, a) = tiles.get_pixel(x, y);
            if a > 0 {
                out.push([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0]);
            }
            x += step;
        }
        y += step;
    }
    out
}

/// R/G/B/Luma histogram of the proxy AFTER the current tone+colour stages,
/// each of the four normalised to its own peak (0..1), for the curve editor
/// backdrop. Detail and the spatial half of the effects are skipped (they
/// need full-res neighbourhoods and barely move the distribution), so this
/// stays cheap enough to run on every slider tick.
pub fn histogram_rgbl(proxy: &[[f32; 3]], settings: &DevelopSettings) -> [[f32; 256]; 4] {
    let tone = tone_is_active(settings).then(|| build_tone_data(settings));
    let use_color = has_color(settings);
    let curves = build_mixer_curves_opt(settings);
    let counts = proxy
        .par_chunks(4096)
        .map(|chunk| {
            let mut counts = [[0u32; 256]; 4];
            for p in chunk {
                let (mut r, mut g, mut b) = (p[0], p[1], p[2]);
                if let Some(t) = &tone {
                    t.apply(&mut r, &mut g, &mut b);
                    clamp_unit(&mut r, &mut g, &mut b);
                }
                if use_color {
                    apply_color(settings, curves.as_ref(), &mut r, &mut g, &mut b);
                    clamp_unit(&mut r, &mut g, &mut b);
                }
                let l = luminance_f32(r, g, b).clamp(0.0, 1.0);
                counts[0][((r * 255.0 + 0.5) as usize).min(255)] += 1;
                counts[1][((g * 255.0 + 0.5) as usize).min(255)] += 1;
                counts[2][((b * 255.0 + 0.5) as usize).min(255)] += 1;
                counts[3][((l * 255.0 + 0.5) as usize).min(255)] += 1;
            }
            counts
        })
        .reduce(
            || [[0u32; 256]; 4],
            |mut a, b| {
                for ch in 0..4 {
                    for i in 0..256 {
                        a[ch][i] += b[ch][i];
                    }
                }
                a
            },
        );
    let mut out = [[0.0f32; 256]; 4];
    for ch in 0..4 {
        let max = counts[ch].iter().copied().max().unwrap_or(0).max(1) as f32;
        for i in 0..256 {
            out[ch][i] = counts[ch][i] as f32 / max;
        }
    }
    out
}

/// Per-channel R/G/B point-curve LUTs, or None when all three are identity.
/// Shared by the CPU tone stage and the GPU preview upload so both sides read
/// the exact same tables.
pub(crate) fn rgb_curve_luts(settings: &DevelopSettings) -> Option<Box<[[f32; 256]; 3]>> {
    if curve_is_identity(&settings.curve_points_r)
        && curve_is_identity(&settings.curve_points_g)
        && curve_is_identity(&settings.curve_points_b)
    {
        return None;
    }
    Some(Box::new([
        point_curve_lut(&settings.curve_points_r),
        point_curve_lut(&settings.curve_points_g),
        point_curve_lut(&settings.curve_points_b),
    ]))
}

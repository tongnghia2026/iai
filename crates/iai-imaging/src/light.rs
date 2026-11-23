//! Light filters (Photoshop's Render > Lens Flare, Render > Lighting Effects,
//! and Outer / Inner Glow baked into the layer). They work on straight RGBA
//! as 0..1 floats, so the 8-bit and 16-bit paths share the maths.

use rayon::prelude::*;

/// A sun with its rays, halo and the lens' ghosts along the line from the
/// sun through the picture's centre, screened over the picture.
/// `(cx, cy)` in pixels, `brightness` 1 = 100 %.
pub fn lens_flare(px: &mut [f32], w: u32, h: u32, cx: f32, cy: f32, brightness: f32) {
    let (wf, hf) = (w as f32, h as f32);
    let diag = (wf * wf + hf * hf).sqrt().max(1.0);
    let b = brightness.max(0.0);
    // Ghosts: (where along sun→centre→beyond, radius, colour, strength).
    let (mx, my) = (wf * 0.5, hf * 0.5);
    let ghosts: [(f32, f32, [f32; 3], f32); 5] = [
        (0.55, 0.018, [0.6, 1.0, 0.6], 0.20),
        (1.25, 0.045, [0.5, 0.7, 1.0], 0.12),
        (1.6, 0.025, [1.0, 0.6, 0.3], 0.16),
        (1.9, 0.07, [0.8, 0.5, 1.0], 0.08),
        (2.3, 0.035, [0.4, 1.0, 0.9], 0.12),
    ];
    let ghosts = ghosts.map(|(t, r, c, s)| {
        let gx = cx + (mx - cx) * t;
        let gy = cy + (my - cy) * t;
        (gx, gy, r * diag, c, s)
    });
    let (core_r, glow_r) = (0.02 * diag, 0.09 * diag);
    let (ring_r, ring_w) = (0.11 * diag, 0.012 * diag);
    let ray_reach = 0.3 * diag;
    let row_len = (w.max(1) * 4) as usize;
    px.par_chunks_mut(row_len).enumerate().for_each(|(y, row)| {
        for (x, p) in row.chunks_exact_mut(4).enumerate() {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let (dx, dy) = (fx - cx, fy - cy);
            let r = (dx * dx + dy * dy).sqrt();
            let core = (-(r / core_r).powi(2)).exp() + 0.35 * (-r / glow_r).exp();
            let ring = 0.22 * (-((r - ring_r) / ring_w).powi(2)).exp();
            let angle = dy.atan2(dx);
            let rays = 0.3 * (6.0 * angle).cos().abs().powi(60) * (-r / ray_reach).exp();
            let mut add = [
                core + rays + ring,
                0.95 * (core + rays) + 0.75 * ring,
                0.85 * (core + rays) + 0.55 * ring,
            ];
            for &(gx, gy, gr, c, s) in &ghosts {
                let d = ((fx - gx).powi(2) + (fy - gy).powi(2)).sqrt();
                // A soft-edged disc, brighter at its rim, as a lens ghost is.
                let disc = smoothstep(gr, gr * 0.8, d) * (0.6 + 0.4 * (d / gr).min(1.0));
                for (a, c) in add.iter_mut().zip(c) {
                    *a += s * disc * c;
                }
            }
            screen_light(p, add.map(|v| (v * b).min(1.0)));
        }
    });
}

/// An omni light (Lighting Effects).
#[derive(Clone, Copy, Debug)]
pub struct Light {
    /// Where it shines from, px.
    pub centre: (f32, f32),
    /// How far it reaches, px.
    pub radius: f32,
    /// 1 = 100 %.
    pub intensity: f32,
    /// How much of the picture stays lit far from it, 0..1.
    pub ambient: f32,
    /// 0..1 per channel.
    pub color: [f32; 3],
}

/// The picture lit by `light`: bright under it, down to the ambient away
/// from it. Alpha is left alone.
pub fn lighting(px: &mut [f32], w: u32, light: &Light) {
    let radius = light.radius.max(1.0);
    let (cx, cy) = light.centre;
    let w = w.max(1) as usize;
    px.par_chunks_mut(4).enumerate().for_each(|(n, p)| {
        let (x, y) = ((n % w) as f32 + 0.5, (n / w) as f32 + 0.5);
        let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
        let fall = (1.0 - (d / radius).powi(2)).max(0.0).powi(2);
        for (v, c) in p.iter_mut().zip(light.color) {
            let m = light.ambient + light.intensity * fall * c;
            *v = (*v * m).clamp(0.0, 1.0);
        }
    });
}

/// A glow of `color` spreading `size` px out from the layer's content,
/// under it (Outer Glow); `opacity` 0..1.
pub fn outer_glow(px: &mut [f32], w: u32, h: u32, size: f32, opacity: f32, color: [f32; 3]) {
    let alpha: Vec<f32> = px.chunks_exact(4).map(|p| p[3]).collect();
    let spread = soft_alpha(&alpha, w as usize, h as usize, size);
    for (p, g) in px.chunks_exact_mut(4).zip(spread) {
        let a = p[3];
        let g = (g * GLOW_GAIN).min(1.0) * opacity.clamp(0.0, 1.0) * (1.0 - a);
        let out = a + g;
        if out > 0.0 {
            for (v, c) in p.iter_mut().zip(color) {
                *v = (*v * a + c * g) / out;
            }
        }
        p[3] = out.min(1.0);
    }
}

/// A glow of `color` reaching `size` px in from the content's edges, over
/// it (Inner Glow); `opacity` 0..1. The layer's own edges count as edges.
pub fn inner_glow(px: &mut [f32], w: u32, h: u32, size: f32, opacity: f32, color: [f32; 3]) {
    let alpha: Vec<f32> = px.chunks_exact(4).map(|p| p[3]).collect();
    let spread = soft_alpha(&alpha, w as usize, h as usize, size);
    for (p, inside) in px.chunks_exact_mut(4).zip(spread) {
        let g = ((1.0 - inside) * GLOW_GAIN).clamp(0.0, 1.0) * opacity.clamp(0.0, 1.0);
        for (v, c) in p.iter_mut().zip(color) {
            *v = *v * (1.0 - g) + c * g;
        }
    }
}

/// How strongly a blurred edge turns into glow (a plain blur fades to half
/// right at the edge; the glow should be full there).
const GLOW_GAIN: f32 = 1.6;

/// Light added the way Screen adds it, in premultiplied terms, so light
/// falling on transparency makes it show.
fn screen_light(p: &mut [f32], light: [f32; 3]) {
    let a = p[3];
    let lmax = light.iter().fold(0.0f32, |m, &v| m.max(v));
    let out_a = 1.0 - (1.0 - a) * (1.0 - lmax);
    if out_a <= 0.0 {
        return;
    }
    for (v, l) in p.iter_mut().zip(light) {
        let lit = 1.0 - (1.0 - *v * a) * (1.0 - l);
        *v = (lit / out_a).clamp(0.0, 1.0);
    }
    p[3] = out_a.clamp(0.0, 1.0);
}

/// 0 at `edge0`, 1 at `edge1`, smooth between (either order).
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// `alpha` softened over about `size` px by three box blurs (close to a
/// Gaussian), with nothing (0) past the edges.
fn soft_alpha(alpha: &[f32], w: usize, h: usize, size: f32) -> Vec<f32> {
    let r = ((size.max(0.0) / 3.0).round() as usize).max(1);
    let mut a = alpha.to_vec();
    let mut tmp = vec![0.0; a.len()];
    for _ in 0..3 {
        box_pass(&a, &mut tmp, w, h, r, true);
        box_pass(&tmp, &mut a, w, h, r, false);
    }
    a
}

/// One box blur of radius `r` along rows (`horizontal`) or columns, with
/// zero padding.
fn box_pass(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize, horizontal: bool) {
    let (lines, len) = if horizontal { (h, w) } else { (w, h) };
    let at = |line: usize, k: usize| {
        if horizontal {
            line * w + k
        } else {
            k * w + line
        }
    };
    let norm = 1.0 / (2 * r + 1) as f32;
    for line in 0..lines {
        let mut sum: f32 = (0..=r.min(len.saturating_sub(1)))
            .map(|k| src[at(line, k)])
            .sum();
        for k in 0..len {
            dst[at(line, k)] = sum * norm;
            if k + r + 1 < len {
                sum += src[at(line, k + r + 1)];
            }
            if k >= r {
                sum -= src[at(line, k - r)];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, rgba: [f32; 4]) -> Vec<f32> {
        (0..w * h).flat_map(|_| rgba).collect()
    }

    fn at(px: &[f32], w: u32, x: u32, y: u32) -> [f32; 4] {
        let i = ((y * w + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    }

    #[test]
    fn the_flare_is_brightest_at_the_sun_and_only_ever_lightens() {
        let (w, h) = (120, 80);
        let before = solid(w, h, [0.2, 0.3, 0.4, 1.0]);
        let mut px = before.clone();
        lens_flare(&mut px, w, h, 30.0, 20.0, 1.0);
        let sun = at(&px, w, 30, 20);
        assert!(sun[0] > 0.95, "the sun is white hot: {sun:?}");
        let far = at(&px, w, 115, 75);
        assert!(far[0] < sun[0]);
        for (a, b) in px.iter().zip(&before) {
            assert!(*a >= *b - 1e-6, "screen never darkens");
        }
    }

    #[test]
    fn no_brightness_leaves_the_picture_as_it_was() {
        let before = solid(40, 30, [0.5, 0.1, 0.9, 1.0]);
        let mut px = before.clone();
        lens_flare(&mut px, 40, 30, 10.0, 10.0, 0.0);
        for (a, b) in px.iter().zip(&before) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn flare_light_shows_on_transparency() {
        let mut px = solid(40, 40, [0.0, 0.0, 0.0, 0.0]);
        lens_flare(&mut px, 40, 40, 20.0, 20.0, 1.0);
        assert!(at(&px, 40, 20, 20)[3] > 0.9);
    }

    #[test]
    fn the_light_falls_off_to_the_ambient() {
        let (w, h) = (100, 100);
        let mut px = solid(w, h, [0.5, 0.5, 0.5, 1.0]);
        let light = Light {
            centre: (50.0, 50.0),
            radius: 30.0,
            intensity: 1.0,
            ambient: 0.2,
            color: [1.0, 1.0, 1.0],
        };
        lighting(&mut px, w, &light);
        let centre = at(&px, w, 50, 50);
        let edge = at(&px, w, 2, 2);
        assert!(centre[0] > 0.55, "lit under the light: {centre:?}");
        assert!(
            (edge[0] - 0.1).abs() < 1e-4,
            "ambient only far off: {edge:?}"
        );
        assert_eq!(centre[3], 1.0, "alpha untouched");
    }

    #[test]
    fn a_coloured_light_tints_the_lit_part() {
        let mut px = solid(20, 20, [0.5, 0.5, 0.5, 1.0]);
        let light = Light {
            centre: (10.0, 10.0),
            radius: 50.0,
            intensity: 1.0,
            ambient: 0.0,
            color: [1.0, 0.5, 0.0],
        };
        lighting(&mut px, 20, &light);
        let c = at(&px, 20, 10, 10);
        assert!(c[0] > c[1] && c[1] > c[2], "warm light: {c:?}");
    }

    /// A 60×60 transparent layer with an opaque white 20×20 square in the
    /// middle.
    fn square() -> Vec<f32> {
        let mut px = solid(60, 60, [0.0, 0.0, 0.0, 0.0]);
        for y in 20..40 {
            for x in 20..40 {
                let i = ((y * 60 + x) * 4) as usize;
                px[i..i + 4].copy_from_slice(&[1.0, 1.0, 1.0, 1.0]);
            }
        }
        px
    }

    #[test]
    fn outer_glow_spreads_round_the_content_and_fades_out() {
        let mut px = square();
        outer_glow(&mut px, 60, 60, 12.0, 1.0, [1.0, 0.8, 0.0]);
        let inside = at(&px, 60, 30, 30);
        assert_eq!(inside, [1.0, 1.0, 1.0, 1.0], "the content stays on top");
        let near = at(&px, 60, 18, 30);
        assert!(near[3] > 0.5, "glow next to the edge: {near:?}");
        assert!((near[2]).abs() < 1e-4, "in the glow colour: {near:?}");
        let mid = at(&px, 60, 12, 30);
        assert!(mid[3] < near[3], "fading out");
        assert!(at(&px, 60, 0, 0)[3] < 1e-3, "nothing far off");
    }

    #[test]
    fn inner_glow_lights_the_inside_edges_only() {
        let mut px = square();
        inner_glow(&mut px, 60, 60, 6.0, 1.0, [1.0, 0.0, 0.0]);
        let edge = at(&px, 60, 20, 30);
        let centre = at(&px, 60, 30, 30);
        assert!(edge[1] < 0.6, "red near the edge: {edge:?}");
        assert!(centre[1] > edge[1], "less in the middle");
        assert_eq!(at(&px, 60, 5, 5)[3], 0.0, "outside stays transparent");
    }

    #[test]
    fn the_blur_keeps_a_flat_field_flat_and_pads_with_nothing() {
        let a = vec![1.0; 30 * 30];
        let s = soft_alpha(&a, 30, 30, 6.0);
        assert!((s[15 * 30 + 15] - 1.0).abs() < 1e-4);
        assert!(s[0] < 0.5, "the edge sees transparency past it");
    }
}

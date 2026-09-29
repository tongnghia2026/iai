//! Zoomed-out canvas display: every screen pixel shows the area average of its
//! footprint, read through the tile atlas mip chain. The GPU result is checked
//! against a CPU model of the same filter (per-tile 2×2 mips clipped to the
//! layer, then area weights), including partial edge tiles, alpha and a mask.

use iai::core::layer::{Layer, LayerMask, LayerStack};
use iai::gpu::compositor::CompositorState;

const TILE: u32 = 256;
const LEVELS: u32 = 6;

fn hash01(x: u32, y: u32, salt: u32) -> f32 {
    let mut v = x
        .wrapping_mul(2_654_435_761)
        .wrapping_add(y.wrapping_mul(2_246_822_519))
        .wrapping_add(salt.wrapping_mul(3_266_489_917))
        .wrapping_add(2_463_534_242);
    v ^= v >> 15;
    v = v.wrapping_mul(2_246_822_519);
    v ^= v >> 13;
    v as f32 / u32::MAX as f32
}

/// Fine speckle on a slow gradient: the texture a zoomed-out view aliases.
fn test_pixels(w: u32, h: u32, with_alpha: bool) -> Vec<u8> {
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let speck = if hash01(x, y, 1) > 0.8 { 110.0 } else { 0.0 };
            px[i] = (40.0 + 120.0 * x as f32 / w as f32 + speck).min(255.0) as u8;
            px[i + 1] = (30.0 + 150.0 * hash01(x, y, 2)) as u8;
            px[i + 2] = (200.0 - 150.0 * y as f32 / h as f32) as u8;
            px[i + 3] = if with_alpha {
                (255.0 * hash01(x / 3, y / 3, 4)) as u8
            } else {
                255
            };
        }
    }
    px
}

/// Global level-`k` image (tile-aligned layout, byte values) built the way the
/// GPU mip pass builds it: each texel averages its in-layer 2×2 children,
/// colour alpha-weighted (masks plain), rounded half to even per level.
fn build_mips(px: &[u8], w: u32, h: u32, is_mask: bool) -> Vec<Vec<[f32; 4]>> {
    let tiles_w = w.div_ceil(TILE);
    let tiles_h = h.div_ceil(TILE);
    let gw = (tiles_w * TILE) as usize;
    let gh = (tiles_h * TILE) as usize;
    let mut level0 = vec![[0.0f32; 4]; gw * gh];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let i = (y * w as usize + x) * 4;
            level0[y * gw + x] = [
                px[i] as f32,
                px[i + 1] as f32,
                px[i + 2] as f32,
                px[i + 3] as f32,
            ];
        }
    }
    let mut levels = vec![level0];
    for k in 1..LEVELS {
        let prev = &levels[(k - 1) as usize];
        let pw = gw >> (k - 1);
        let t = (TILE >> k) as usize;
        let lw = gw >> k;
        let lh = gh >> k;
        let mut out = vec![[0.0f32; 4]; lw * lh];
        for ty in 0..tiles_h {
            for tx in 0..tiles_w {
                let vw0 = (w - tx * TILE).min(TILE);
                let vh0 = (h - ty * TILE).min(TILE);
                let vw = vw0.div_ceil(1 << (k - 1)) as usize;
                let vh = vh0.div_ceil(1 << (k - 1)) as usize;
                for ly in 0..t {
                    for lx in 0..t {
                        let (mut plain, mut weighted, mut a, mut n) =
                            ([0.0f32; 3], [0.0f32; 3], 0.0f32, 0.0f32);
                        for dy in 0..2 {
                            for dx in 0..2 {
                                let cx = lx * 2 + dx;
                                let cy = ly * 2 + dy;
                                if cx < vw && cy < vh {
                                    let gx = tx as usize * t * 2 + cx;
                                    let gy = ty as usize * t * 2 + cy;
                                    let c = prev[gy * pw + gx];
                                    for ch in 0..3 {
                                        plain[ch] += c[ch];
                                        weighted[ch] += c[ch] * c[3];
                                    }
                                    a += c[3];
                                    n += 1.0;
                                }
                            }
                        }
                        let mut v = [0.0f32; 4];
                        if n > 0.0 {
                            let use_plain = is_mask || a == 255.0 * n;
                            for ch in 0..3 {
                                v[ch] = if use_plain {
                                    plain[ch] / n
                                } else if a > 0.0 {
                                    weighted[ch] / a
                                } else {
                                    0.0
                                };
                            }
                            v[3] = a / n;
                        }
                        let q = |x: f32| x.round_ties_even();
                        let gx = tx as usize * t + lx;
                        let gy = ty as usize * t + ly;
                        out[gy * lw + gx] = [q(v[0]), q(v[1]), q(v[2]), q(v[3])];
                    }
                }
            }
        }
        levels.push(out);
    }
    levels
}

/// CPU twin of `area_sample`: (straight sRGB rgb, coverage) or None when the
/// footprint holds no coverage.
fn model_pixel(
    color: &[Vec<[f32; 4]>],
    mask: Option<&[Vec<[f32; 4]>]>,
    w: u32,
    h: u32,
    lx: f32,
    ly: f32,
    footprint: f32,
) -> Option<([f32; 3], f32)> {
    let lv = ((footprint * 0.5).log2().floor()).clamp(0.0, (LEVELS - 1) as f32) as u32;
    let s = (1u32 << lv) as f32;
    let gw = (w.div_ceil(TILE) * TILE >> lv) as usize;
    let half = 0.5 * footprint;
    let x0 = (lx - half).max(0.0) / s;
    let x1 = (lx + half).min(w as f32) / s;
    let y0 = (ly - half).max(0.0) / s;
    let y1 = (ly + half).min(h as f32) / s;
    let (mut acc, mut cov, mut area) = ([0.0f32; 3], 0.0f32, 0.0f32);
    let mut ty = y0.floor() as u32;
    while (ty as f32) < y1 {
        let wy = y1.min(ty as f32 + 1.0) - y0.max(ty as f32);
        let mut tx = x0.floor() as u32;
        while (tx as f32) < x1 {
            let wx = x1.min(tx as f32 + 1.0) - x0.max(tx as f32);
            let wgt = wx * wy;
            let c = color[lv as usize][ty as usize * gw + tx as usize];
            let m = mask.map_or(255.0, |m| m[lv as usize][ty as usize * gw + tx as usize][0]);
            let a = c[3] / 255.0 * (m / 255.0);
            for ch in 0..3 {
                acc[ch] += c[ch] / 255.0 * a * wgt;
            }
            cov += a * wgt;
            area += wgt;
            tx += 1;
        }
        ty += 1;
    }
    (cov > 1e-5 && area > 0.0).then(|| ([acc[0] / cov, acc[1] / cov, acc[2] / cov], cov / area))
}

struct Case {
    label: &'static str,
    with_alpha: bool,
    with_mask: bool,
}

#[test]
fn headless_zoomed_out_display_is_the_footprint_area_average() {
    if std::env::var_os("CI").is_some() {
        eprintln!("headless GPU pixel test is local real-GPU only; skipped on CI");
        return;
    }
    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no headless GPU adapter; skipped");
        return;
    };
    // Not tile multiples: the right/bottom tiles are partial.
    let (w, h) = (600u32, 300u32);
    let max_texture = device.limits().max_texture_dimension_2d;
    let cases = [
        Case {
            label: "opaque",
            with_alpha: false,
            with_mask: false,
        },
        Case {
            label: "alpha",
            with_alpha: true,
            with_mask: false,
        },
        Case {
            label: "mask",
            with_alpha: false,
            with_mask: true,
        },
    ];
    let mut compositor = CompositorState::new(&device, 1, 1, max_texture);
    for case in &cases {
        let px = test_pixels(w, h, case.with_alpha);
        let mut layer = Layer::from_rgba(0, "Layer", px.clone(), w, h);
        let mut mask_px = Vec::new();
        if case.with_mask {
            let mut mask = LayerMask::new_white(w, h);
            mask_px = vec![0u8; (w * h * 4) as usize];
            for y in 0..h {
                for x in 0..w {
                    let v = (255.0 * (x as f32 / w as f32) * (0.6 + 0.4 * hash01(x, y, 9))) as u8;
                    mask.tiles.set_pixel(x, y, v, v, v, 255);
                    let i = ((y * w + x) * 4) as usize;
                    mask_px[i..i + 4].copy_from_slice(&[v, v, v, 255]);
                }
            }
            layer.mask = Some(mask);
        }
        let mut stack = LayerStack::new(w, h);
        stack.layers[0] = layer;
        let color_mips = build_mips(&px, w, h, false);
        let mask_mips = case.with_mask.then(|| build_mips(&mask_px, w, h, true));

        for zoom in [1.0f32, 0.75, 0.4, 0.2, 0.1, 0.03] {
            let vw = ((w as f32 * zoom).ceil() as u32).max(1);
            let vh = ((h as f32 * zoom).ceil() as u32).max(1);
            compositor.configure_viewport(&device, vw, vh, 1);
            let is_ping = compositor
                .composite_layers(&device, &queue, &stack, 0.0, 0.0, zoom, None, false, false);
            let gpu = compositor.readback_rgba8(&device, &queue, is_ping);
            let footprint = 1.0 / zoom;
            let (mut max_rgb, mut max_a, mut checked) = (0.0f32, 0.0f32, 0u32);
            let (mut sum_rgb, mut n_rgb, mut worst, mut signed) =
                (0.0f64, 0u32, (0u32, 0u32), [0.0f64; 3]);
            for sy in 0..vh {
                for sx in 0..vw {
                    let lx = (sx as f32 + 0.5) / zoom;
                    let ly = (sy as f32 + 0.5) / zoom;
                    if lx >= w as f32 || ly >= h as f32 {
                        continue;
                    }
                    let o = ((sy * vw + sx) * 4) as usize;
                    let got = &gpu[o..o + 4];
                    if zoom >= 1.0 {
                        let i = ((ly as u32 * w + lx as u32) * 4) as usize;
                        let m = if case.with_mask {
                            mask_px[i] as f32 / 255.0
                        } else {
                            1.0
                        };
                        let a = px[i + 3] as f32 / 255.0 * m;
                        max_a = max_a.max((got[3] as f32 - a * 255.0).abs());
                        if a >= 0.25 {
                            for ch in 0..3 {
                                max_rgb = max_rgb.max((got[ch] as f32 - px[i + ch] as f32).abs());
                            }
                        }
                        checked += 1;
                        continue;
                    }
                    let Some((rgb, cov)) =
                        model_pixel(&color_mips, mask_mips.as_deref(), w, h, lx, ly, footprint)
                    else {
                        continue;
                    };
                    max_a = max_a.max((got[3] as f32 - cov * 255.0).abs());
                    if cov >= 0.25 {
                        for ch in 0..3 {
                            signed[ch] += (got[ch] as f32 - rgb[ch] * 255.0) as f64;
                            let d = (got[ch] as f32 - rgb[ch] * 255.0).abs();
                            if d > max_rgb {
                                worst = (sx, sy);
                            }
                            max_rgb = max_rgb.max(d);
                            sum_rgb += d as f64;
                            n_rgb += 1;
                        }
                    }
                    checked += 1;
                }
            }
            eprintln!(
                "{:>6} zoom {:>5.1}%: {checked} px ({vw}x{vh}), max |rgb| {max_rgb:.2} at {worst:?}, mean {:.3}, signed {:?}, max |alpha| {max_a:.2}",
                case.label,
                zoom * 100.0,
                sum_rgb / n_rgb.max(1) as f64,
                signed.map(|v| (v / (n_rgb.max(1) / 3).max(1) as f64 * 100.0).round() / 100.0)
            );
            assert!(checked > 0, "{} zoom {zoom}: nothing compared", case.label);
            // Output rounding is ≤ 0.5; opaque blocks go through the hardware
            // bilinear filter (8-bit sub-texel weights) and mixed-alpha mips
            // may differ by one step where GPU division rounds a tie the other
            // way. No drift allowed.
            let mean = sum_rgb / n_rgb.max(1) as f64;
            let drift = signed
                .iter()
                .map(|v| (v / (n_rgb.max(1) / 3).max(1) as f64).abs())
                .fold(0.0, f64::max);
            assert!(
                max_rgb <= 2.5 && max_a <= 1.0 && mean <= 0.4 && drift <= 0.15,
                "{} zoom {zoom}: GPU differs from the area-filter model \
                 (max rgb {max_rgb}, alpha {max_a}, mean {mean}, drift {drift})",
                case.label
            );
        }
    }
}

//! Manual probe: timings of the Refine Selection engine on a 12 MP canvas,
//! and optional before/after PNGs on a real photo.
//!
//! Run: `cargo test --release --test refine_probe -- --ignored --nocapture`
//! (set `REFINE_PROBE_OUT=<dir>` to also write the photo PNGs).

use iai::core::refine::{RefineParams, RefineSession};
use iai::core::selection::{compute_sobel, pixels_to_lab, EdgeCache, Selection};
use std::time::Instant;

fn disc(w: usize, h: usize, cx: f32, cy: f32, r: f32) -> Vec<u8> {
    (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as f32, (i / w) as f32);
            if ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() <= r {
                255
            } else {
                0
            }
        })
        .collect()
}

fn session(w: usize, h: usize, mask: Vec<u8>) -> RefineSession {
    let mut sel = Selection::new(w as u32, h as u32);
    sel.mask = mask.clone();
    sel.active = true;
    RefineSession::new(sel, mask)
}

#[test]
#[ignore]
fn refine_render_timings_12mp() {
    let (w, h) = (4000usize, 3000usize);
    // A busy picture: stripes and noise so the matting has real work.
    let mut px = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let n = ((x * 7 + y * 13) % 29) as u8;
            let v = if (x / 9 + y / 13) % 3 == 0 {
                40 + n
            } else {
                200 - n
            };
            px[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[
                v,
                v / 2 + 60,
                255 - v,
                255,
            ]);
        }
    }
    let t = Instant::now();
    let cache = EdgeCache {
        lab: pixels_to_lab(&px, w as u32, h as u32),
        sobel: compute_sobel(&px, w as u32, h as u32),
        width: w as u32,
        height: h as u32,
        layer_idx: 0,
        layer_revision: 0,
        sample_merged: true,
    };
    println!(
        "edge cache (first Radius use): {:.0} ms",
        t.elapsed().as_secs_f32() * 1000.0
    );
    let mask = disc(w, h, 2000.0, 1500.0, 1000.0);
    let cases = [
        (
            "Smooth 100",
            RefineParams {
                smooth: 100.0,
                ..Default::default()
            },
        ),
        (
            "Feather 150",
            RefineParams {
                feather: 150.0,
                ..Default::default()
            },
        ),
        (
            "Contrast+Shift",
            RefineParams {
                contrast: 40.0,
                shift_edge: -20.0,
                ..Default::default()
            },
        ),
        (
            "Radius 20",
            RefineParams {
                radius: 20.0,
                ..Default::default()
            },
        ),
        (
            "Radius 20 smart",
            RefineParams {
                radius: 20.0,
                smart_radius: true,
                ..Default::default()
            },
        ),
        (
            "Radius 64",
            RefineParams {
                radius: 64.0,
                ..Default::default()
            },
        ),
        (
            "Radius 250",
            RefineParams {
                radius: 250.0,
                ..Default::default()
            },
        ),
        (
            "All (R64 S20 F10 C20 Sh-10)",
            RefineParams {
                radius: 64.0,
                smart_radius: true,
                smooth: 20.0,
                feather: 10.0,
                contrast: 20.0,
                shift_edge: -10.0,
            },
        ),
    ];
    let mut out = vec![0u8; w * h];
    for (name, p) in cases {
        let mut s = session(w, h, mask.clone());
        s.set_params(p);
        let t = Instant::now();
        s.render(Some(&cache), &mut out, None);
        println!("{name}: {:.0} ms", t.elapsed().as_secs_f32() * 1000.0);
    }
    // A brush frame: a few dabs, partial re-render.
    let mut s = session(w, h, mask);
    s.set_params(RefineParams {
        radius: 20.0,
        smooth: 10.0,
        feather: 2.0,
        ..Default::default()
    });
    s.render(Some(&cache), &mut out, None);
    s.begin_stroke();
    let t = Instant::now();
    let dabs: Vec<(f32, f32)> = (0..6).map(|i| (3000.0 + i as f32 * 14.0, 1500.0)).collect();
    if let Some(r) = s.paint(
        Some(&cache),
        iai::core::refine::StampOp::Smart,
        &dabs,
        40.0,
        0.5,
    ) {
        s.render(Some(&cache), &mut out, Some(r));
    }
    println!(
        "brush frame (6 Smart dabs r40 + re-render): {:.1} ms",
        t.elapsed().as_secs_f32() * 1000.0
    );
}

#[test]
#[ignore]
fn refine_photo_png() {
    let Ok(dir) = std::env::var("REFINE_PROBE_OUT") else {
        println!("REFINE_PROBE_OUT not set — skipped");
        return;
    };
    let path = "C:/Windows/Web/Wallpaper/Theme1/img1.jpg";
    let Ok(img) = image::open(path) else {
        println!("no sample photo at {path}");
        return;
    };
    let img = img.to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let px = img.as_raw();
    let cache = EdgeCache {
        lab: pixels_to_lab(px, w as u32, h as u32),
        sobel: compute_sobel(px, w as u32, h as u32),
        width: w as u32,
        height: h as u32,
        layer_idx: 0,
        layer_revision: 0,
        sample_merged: true,
    };
    // Rough ellipse in the middle of the frame as the "selection".
    let mask: Vec<u8> = (0..w * h)
        .map(|i| {
            let (x, y) = (
                (i % w) as f32 / w as f32 - 0.5,
                (i / w) as f32 / h as f32 - 0.5,
            );
            if (x / 0.3).powi(2) + (y / 0.35).powi(2) <= 1.0 {
                255
            } else {
                0
            }
        })
        .collect();
    for (name, p) in [
        ("r0", RefineParams::default()),
        (
            "r24",
            RefineParams {
                radius: 24.0,
                ..Default::default()
            },
        ),
        (
            "r24smart",
            RefineParams {
                radius: 24.0,
                smart_radius: true,
                ..Default::default()
            },
        ),
    ] {
        let mut s = session(w, h, mask.clone());
        s.set_params(p);
        let mut out = vec![0u8; w * h];
        s.render(Some(&cache), &mut out, None);
        let mut on_black = img.clone();
        for (i, p) in on_black.pixels_mut().enumerate() {
            let a = out[i] as u32;
            for c in 0..3 {
                p[c] = (p[c] as u32 * a / 255) as u8;
            }
        }
        let file = format!("{dir}/refine_{name}.png");
        on_black.save(&file).unwrap();
        println!("wrote {file}");
    }
}

/// Objective check on a real photo: the rock against the sky in the Windows
/// wallpaper. Truth = "not sky" by colour; the rough selection is that truth
/// made blocky and spilled 4 px into the sky (a quick lasso). Radius should
/// bring the mean error down.
#[test]
#[ignore]
fn refine_radius_reduces_edge_error_on_a_photo() {
    let path = "C:/Windows/Web/Wallpaper/Theme1/img1.jpg";
    let Ok(img) = image::open(path) else {
        println!("no sample photo at {path}");
        return;
    };
    let img = img.to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let px = img.as_raw();
    let cache = EdgeCache {
        lab: pixels_to_lab(px, w as u32, h as u32),
        sobel: compute_sobel(px, w as u32, h as u32),
        width: w as u32,
        height: h as u32,
        layer_idx: 0,
        layer_revision: 0,
        sample_merged: true,
    };
    let (x0, y0, x1, y1) = (1190usize, 390usize, 1530usize, 700usize);
    let sky = |x: usize, y: usize| {
        let i = (y * w + x) * 4;
        let (r, b) = (px[i] as i32, px[i + 2] as i32);
        b > r + 20 && b > 120
    };
    let mut truth = vec![0u8; w * h];
    for y in y0..y1 {
        for x in x0..x1 {
            truth[y * w + x] = if sky(x, y) { 0 } else { 255 };
        }
    }
    // Blocky 8 px version, spilled 4 px outward.
    let mut rough = vec![0u8; w * h];
    for y in y0..y1 {
        for x in x0..x1 {
            let (bx, by) = (x / 8 * 8, y / 8 * 8);
            let mut n = 0;
            for yy in by..(by + 8).min(y1) {
                for xx in bx..(bx + 8).min(x1) {
                    n += (truth[yy * w + xx] > 0) as u32;
                }
            }
            if n >= 16 {
                for yy in y.saturating_sub(4)..(y + 5).min(y1) {
                    for xx in x.saturating_sub(4)..(x + 5).min(x1) {
                        rough[yy * w + xx] = 255;
                    }
                }
            }
        }
    }
    let inner = |m: &[u8]| -> f64 {
        let mut e = 0f64;
        let mut n = 0f64;
        for y in y0 + 20..y1 - 20 {
            for x in x0 + 20..x1 - 20 {
                e += (m[y * w + x] as f64 - truth[y * w + x] as f64).abs() / 255.0;
                n += 1.0;
            }
        }
        e / n * 100.0
    };
    println!("rough selection: {:.2}% mean error", inner(&rough));
    for (name, p) in [
        (
            "Radius 6",
            RefineParams {
                radius: 6.0,
                ..Default::default()
            },
        ),
        (
            "Radius 12",
            RefineParams {
                radius: 12.0,
                ..Default::default()
            },
        ),
        (
            "Radius 12 smart",
            RefineParams {
                radius: 12.0,
                smart_radius: true,
                ..Default::default()
            },
        ),
        (
            "Radius 24",
            RefineParams {
                radius: 24.0,
                ..Default::default()
            },
        ),
    ] {
        let mut s = session(w, h, rough.clone());
        s.set_params(p);
        let mut out = vec![0u8; w * h];
        s.render(Some(&cache), &mut out, None);
        println!("{name}: {:.2}% mean error", inner(&out));
        if let Ok(dir) = std::env::var("REFINE_PROBE_OUT") {
            let mut crop = image::RgbaImage::new((x1 - x0) as u32, (y1 - y0) as u32);
            for y in y0..y1 {
                for x in x0..x1 {
                    let a = out[y * w + x] as u32;
                    let i = (y * w + x) * 4;
                    let c = |k: usize| (px[i + k] as u32 * a / 255) as u8;
                    crop.put_pixel(
                        (x - x0) as u32,
                        (y - y0) as u32,
                        image::Rgba([c(0), c(1), c(2), 255]),
                    );
                }
            }
            crop.save(format!("{dir}/rock_{}.png", name.replace(' ', "_")))
                .unwrap();
        }
    }
}

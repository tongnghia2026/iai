//! Headless default-look renderer for evaluating RAW color without the GUI.
//!
//! For one RAW file, render the neutral default look under each embedded-JPEG
//! matching mode (full / gain-only / none) and write a downscaled PNG per mode,
//! so the default-look color can be compared side by side.
//!
//! ```text
//! IAI_RAW_LOOK_FILE="C:/path/to/file.arw" IAI_RAW_LOOK_OUT="C:/out" \
//!   cargo test --release --test raw_look_probe -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use iai::core::canvas::Canvas;
use iai::core::color_reference::summarize_encoded_rgba16;
use iai::core::develop_scene::render_default_look;
use iai::formats::png::PngExporter;
use iai::formats::raw::RawImporter;
use iai::formats::{ExportOptions, Exporter, Importer};

fn out_dir() -> PathBuf {
    std::env::var("IAI_RAW_LOOK_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
}

fn look_file() -> Option<PathBuf> {
    if let Ok(f) = std::env::var("IAI_RAW_LOOK_FILE") {
        let p = PathBuf::from(f);
        return p.is_file().then_some(p);
    }
    // Fall back to the first Sony .arw in the corpus dir.
    let dir = PathBuf::from(std::env::var("IAI_RAW_CORPUS").ok()?);
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("arw"))
        .collect();
    files.sort();
    files.into_iter().next()
}

/// Face-crop centre as (x, y) fractions of the image, overridable via
/// `IAI_RAW_LOOK_CROP="cx,cy"` to target any edge for pixel-level inspection.
fn crop_fracs() -> (f32, f32) {
    std::env::var("IAI_RAW_LOOK_CROP")
        .ok()
        .and_then(|s| {
            let p: Vec<f32> = s.split(',').filter_map(|v| v.trim().parse().ok()).collect();
            if p.len() == 2 {
                Some((p[0], p[1]))
            } else {
                None
            }
        })
        .unwrap_or((0.46, 0.40))
}

fn constrained_grid(w: u32, h: u32, target_w: u32) -> (u32, u32) {
    let sw = w.min(target_w);
    let sh = ((h as f64) * sw as f64 / w as f64).round().max(1.0) as u32;
    (sw, sh)
}

/// Center-crop to the target aspect, then box-resample to one exact metric
/// grid. Acutance is resolution-dependent, so every observation in a run must
/// use the same output dimensions even when the embedded preview is smaller.
fn resample_to_grid(px16: &[u16], w: u32, h: u32, target_w: u32, target_h: u32) -> Vec<u16> {
    assert_eq!(px16.len(), w as usize * h as usize * 4);
    let (crop_x, crop_y, crop_w, crop_h) =
        if u64::from(w) * u64::from(target_h) > u64::from(h) * u64::from(target_w) {
            let crop_w = ((u64::from(h) * u64::from(target_w)) / u64::from(target_h)).max(1) as u32;
            ((w - crop_w) / 2, 0, crop_w, h)
        } else {
            let crop_h = ((u64::from(w) * u64::from(target_h)) / u64::from(target_w)).max(1) as u32;
            (0, (h - crop_h) / 2, w, crop_h)
        };
    let sw = target_w;
    let sh = target_h;
    let mut out = vec![0u16; sw as usize * sh as usize * 4];
    for oy in 0..sh {
        let y0 = crop_y + (u64::from(oy) * u64::from(crop_h) / u64::from(sh)) as u32;
        let y1 = crop_y
            + ((u64::from(oy + 1) * u64::from(crop_h) + u64::from(sh) - 1) / u64::from(sh)) as u32;
        for ox in 0..sw {
            let x0 = crop_x + (u64::from(ox) * u64::from(crop_w) / u64::from(sw)) as u32;
            let x1 = crop_x
                + ((u64::from(ox + 1) * u64::from(crop_w) + u64::from(sw) - 1) / u64::from(sw))
                    as u32;
            let mut acc = [0u64; 4];
            let mut n = 0u64;
            for y in y0..y1.max(y0 + 1).min(crop_y + crop_h) {
                for x in x0..x1.max(x0 + 1).min(crop_x + crop_w) {
                    let i = (y as usize * w as usize + x as usize) * 4;
                    for c in 0..4 {
                        acc[c] += px16[i + c] as u64;
                    }
                    n += 1;
                }
            }
            let o = (oy as usize * sw as usize + ox as usize) * 4;
            for c in 0..4 {
                out[o + c] = (acc[c] / n.max(1)) as u16;
            }
        }
    }
    out
}

/// Mean channel value (0..255) over an interleaved RGBA16 buffer.
fn mean_rgb8(px16: &[u16]) -> [f32; 3] {
    let mut acc = [0f64; 3];
    let n = (px16.len() / 4).max(1) as f64;
    for p in px16.chunks_exact(4) {
        for c in 0..3 {
            acc[c] += p[c] as f64;
        }
    }
    [
        (acc[0] / n / 257.0) as f32,
        (acc[1] / n / 257.0) as f32,
        (acc[2] / n / 257.0) as f32,
    ]
}

/// Mean of the brightest ~8% of pixels (by luma) — a proxy for the white point,
/// so a green/warm cast on the near-white dress is visible as g/r, g/b ratios.
fn bright_mean_rgb8(px16: &[u16]) -> [f32; 3] {
    let mut lumas: Vec<u32> = px16
        .chunks_exact(4)
        .map(|p| p[0] as u32 * 2 + p[1] as u32 * 5 + p[2] as u32)
        .collect();
    lumas.sort_unstable();
    let thresh = lumas[(lumas.len() as f64 * 0.92) as usize];
    let mut acc = [0f64; 3];
    let mut n = 0f64;
    for p in px16.chunks_exact(4) {
        let l = p[0] as u32 * 2 + p[1] as u32 * 5 + p[2] as u32;
        if l >= thresh {
            for c in 0..3 {
                acc[c] += p[c] as f64;
            }
            n += 1.0;
        }
    }
    let n = n.max(1.0);
    [
        (acc[0] / n / 257.0) as f32,
        (acc[1] / n / 257.0) as f32,
        (acc[2] / n / 257.0) as f32,
    ]
}

fn print_quality(label: &str, px16: &[u16], width: u32, height: u32) {
    let Some(summary) = summarize_encoded_rgba16(px16, width as usize, height as usize) else {
        return;
    };
    println!(
        "{label:<16} Oklab L={:.4} C={:.4} Cshadow={:.4} Cmid={:.4} Chigh={:.4} clip={:.3}% acutance={:.7}",
        summary.mean_oklab_lightness,
        summary.mean_oklab_chroma,
        summary.shadow_chroma,
        summary.midtone_chroma,
        summary.highlight_chroma,
        summary.clipped_pixel_fraction * 100.0,
        summary.laplacian_acutance,
    );
}

#[test]
#[ignore = "requires a RAW file via IAI_RAW_LOOK_FILE/IAI_RAW_CORPUS; renders PNGs"]
fn raw_default_look_variants() {
    let Some(file) = look_file() else {
        eprintln!("no RAW file (set IAI_RAW_LOOK_FILE or IAI_RAW_CORPUS); skipping");
        return;
    };
    let out = out_dir();
    std::fs::create_dir_all(&out).expect("create RAW look output directory");
    let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("look");
    println!("\ndefault-look variants for {}\n", file.display());

    let opts = ExportOptions {
        flatten: true,
        embed_metadata: false,
        embed_icc: false,
        ..ExportOptions::default()
    };

    // The embedded camera JPEG is a picture-style observation, never an
    // accuracy ground truth. Metrics below are no-reference summaries; images
    // are exported for a separately registered pairwise comparison.
    let mut metric_grid = None;
    if let Some(prev) = iai::formats::raw_preview::extract(&file) {
        let px16: Vec<u16> = prev.rgba.iter().map(|&b| b as u16 * 257).collect();
        let (sw, sh) = constrained_grid(prev.width, prev.height, 1400);
        let small = resample_to_grid(&px16, prev.width, prev.height, sw, sh);
        metric_grid = Some((sw, sh));
        print_quality("CAMERA_JPEG", &small, sw, sh);
        let path = out.join(format!("{stem}__CAMERA_JPEG.png"));
        PngExporter
            .export(&Canvas::from_rgba16(small, sw, sh), &path, &opts)
            .expect("write camera jpeg");
        println!(
            "wrote {} ({}x{} source)",
            path.display(),
            prev.width,
            prev.height
        );
        // Native-resolution face crop of the embedded JPEG, same proportional
        // geometry as the render crops below, so hi-frequency colour noise, skin
        // hue and acutance can be measured against the target at true detail.
        let (w, h) = (prev.width, prev.height);
        let (cw, ch) = (1100u32.min(w), 1300u32.min(h));
        let (fx, fy) = crop_fracs();
        let cx = ((w as f32 * fx) as u32).min(w - cw / 2).max(cw / 2) - cw / 2;
        let cy = ((h as f32 * fy) as u32).min(h - ch / 2).max(ch / 2) - ch / 2;
        let mut crop = vec![0u16; cw as usize * ch as usize * 4];
        for row in 0..ch {
            let src = ((cy + row) as usize * w as usize + cx as usize) * 4;
            let dst = row as usize * cw as usize * 4;
            crop[dst..dst + cw as usize * 4].copy_from_slice(&px16[src..src + cw as usize * 4]);
        }
        let crop_path = out.join(format!("{stem}__CAMERA_JPEG__facecrop.png"));
        PngExporter
            .export(&Canvas::from_rgba16(crop, cw, ch), &crop_path, &opts)
            .expect("write camera jpeg crop");
        println!("wrote {}", crop_path.display());
        let m = mean_rgb8(&px16);
        let wp = bright_mean_rgb8(&px16);
        println!("CAMERA_JPEG  mean=({:.0},{:.0},{:.0})  whites=({:.0},{:.0},{:.0})  wb g/r={:.3} g/b={:.3}", m[0], m[1], m[2], wp[0], wp[1], wp[2], wp[1]/wp[0], wp[1]/wp[2]);
    } else {
        println!("no embedded preview found");
    }

    let requested_modes = std::env::var("IAI_RAW_LOOK_MODES").ok().map(|value| {
        value
            .split(',')
            .map(|mode| mode.trim().to_ascii_lowercase())
            .collect::<Vec<_>>()
    });
    for (mode_env, label) in [
        ("full", "full-jpeg-match"),
        ("off", "gain-only"),
        ("none", "no-match"),
    ]
    .into_iter()
    .filter(|(mode, _)| {
        requested_modes
            .as_ref()
            .is_none_or(|requested| requested.iter().any(|value| value == mode))
    }) {
        std::env::set_var("IAI_RAW_JPEG_MATCH", mode_env);
        let canvas = RawImporter.import(&file).expect("decode RAW");
        let scene = canvas
            .develop_source
            .as_ref()
            .expect("RAW attaches a scene");
        let px16 = render_default_look(scene);
        let (w, h) = (scene.width, scene.height);

        let (sw, sh) = metric_grid.unwrap_or_else(|| constrained_grid(w, h, 1400));
        let small = resample_to_grid(&px16, w, h, sw, sh);
        let m = mean_rgb8(&small);
        let wp = bright_mean_rgb8(&small);
        println!(
            "{label:<16} mean=({:.0},{:.0},{:.0})  whites=({:.0},{:.0},{:.0})  wb g/r={:.3} g/b={:.3}",
            m[0], m[1], m[2], wp[0], wp[1], wp[2], wp[1] / wp[0], wp[1] / wp[2]
        );
        print_quality(label, &small, sw, sh);
        let full_path = out.join(format!("{stem}__{label}.png"));
        PngExporter
            .export(&Canvas::from_rgba16(small, sw, sh), &full_path, &opts)
            .expect("write full PNG");
        println!("wrote {}", full_path.display());

        // Native-resolution face crop (proportional center) to judge skin tone
        // at true detail rather than through the downscale average.
        let (cw, ch) = (1100u32.min(w), 1300u32.min(h));
        let (fx, fy) = crop_fracs();
        let cx = ((w as f32 * fx) as u32).min(w - cw / 2).max(cw / 2) - cw / 2;
        let cy = ((h as f32 * fy) as u32).min(h - ch / 2).max(ch / 2) - ch / 2;
        let mut crop = vec![0u16; cw as usize * ch as usize * 4];
        for row in 0..ch {
            let src = ((cy + row) as usize * w as usize + cx as usize) * 4;
            let dst = row as usize * cw as usize * 4;
            crop[dst..dst + cw as usize * 4].copy_from_slice(&px16[src..src + cw as usize * 4]);
        }
        let crop_path = out.join(format!("{stem}__{label}__facecrop.png"));
        PngExporter
            .export(&Canvas::from_rgba16(crop, cw, ch), &crop_path, &opts)
            .expect("write crop PNG");
        println!("wrote {}", crop_path.display());
    }
    std::env::remove_var("IAI_RAW_JPEG_MATCH");
}

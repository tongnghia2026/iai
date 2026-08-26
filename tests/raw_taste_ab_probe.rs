//! Quality Milestone Q1 (item #7) — creative-taste vs technical-neutral A/B.
//!
//! The plan wants the technical RAW master free of un-versioned creative taste.
//! On the corpus (all embedded-JPEG matched, no camera profile) the one clearly
//! separable constants are now collected under a versioned `raw_render_recipe`.
//! This probe renders every corpus file BOTH ways — current `legacy-baked-v1`
//! and opt-in `technical-neutral-v2` (`IAI_RAW_RENDER_RECIPE=technical`) — writes
//! a side-by-side montage (left = current, right = technical), and measures the
//! lightness, chroma, and acutance the baked recipe actually adds before any
//! default is changed.
//!
//! It changes no default: the technical recipe is selected only for the right-
//! hand render via the env override. `#[ignore]`d and gated on
//! `IAI_RAW_CORPUS`. Set `IAI_Q0_OUT` for the montages + CSV.
//!
//! ```text
//! $env:IAI_RAW_CORPUS='C:\Users\Admin\Pictures\anh-raw'
//! $env:IAI_Q0_OUT='C:\Users\Admin\Documents\IAI\target\q0'
//! cargo test --release --test raw_taste_ab_probe -- --ignored --nocapture
//! ```

use std::fmt::Write as _;
use std::path::PathBuf;

use iai::core::canvas::Canvas;
use iai::core::color_reference::summarize_encoded_rgba16;
use iai::core::develop_scene::render_default_look;
use iai::formats::png::PngExporter;
use iai::formats::raw::RawImporter;
use iai::formats::{ExportOptions, Exporter, Importer};

const HALF_WIDTH: u32 = 760;

fn corpus_dir() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var("IAI_RAW_CORPUS").ok()?);
    path.is_dir().then_some(path)
}

/// Box-resample RGBA16 to `target_w` wide, aspect preserved.
fn resample(px16: &[u16], w: u32, h: u32, target_w: u32) -> (Vec<u16>, u32, u32) {
    let sw = w.min(target_w).max(1);
    let sh = (((h as u64) * (sw as u64) + (w as u64) / 2) / (w as u64)).max(1) as u32;
    let mut out = vec![0u16; sw as usize * sh as usize * 4];
    for oy in 0..sh {
        let y0 = (u64::from(oy) * u64::from(h) / u64::from(sh)) as u32;
        let y1 = ((u64::from(oy + 1) * u64::from(h)) / u64::from(sh)).max(u64::from(y0) + 1) as u32;
        for ox in 0..sw {
            let x0 = (u64::from(ox) * u64::from(w) / u64::from(sw)) as u32;
            let x1 =
                ((u64::from(ox + 1) * u64::from(w)) / u64::from(sw)).max(u64::from(x0) + 1) as u32;
            let mut acc = [0u64; 4];
            let mut n = 0u64;
            for y in y0..y1.min(h) {
                for x in x0..x1.min(w) {
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
    (out, sw, sh)
}

/// Render one corpus file's neutral default look at the selected RAW recipe.
fn render_resampled(path: &std::path::Path) -> Option<(Vec<u16>, u32, u32)> {
    let canvas = RawImporter.import(path).ok()?;
    let scene = canvas.develop_source.as_ref()?;
    let px16 = render_default_look(scene);
    Some(resample(&px16, scene.width, scene.height, HALF_WIDTH))
}

/// Place two equal-size renders left|right into one RGBA16 buffer.
fn montage(left: &[u16], right: &[u16], w: u32, h: u32) -> Vec<u16> {
    let full_w = w as usize * 2;
    let mut out = vec![0u16; full_w * h as usize * 4];
    for y in 0..h as usize {
        let src = y * w as usize * 4;
        let row = w as usize * 4;
        let dst = y * full_w * 4;
        out[dst..dst + row].copy_from_slice(&left[src..src + row]);
        out[dst + row..dst + 2 * row].copy_from_slice(&right[src..src + row]);
    }
    out
}

#[test]
#[ignore = "requires a local RAW corpus via IAI_RAW_CORPUS; decodes each file twice"]
fn raw_taste_ab_montage() {
    let Some(dir) = corpus_dir() else {
        eprintln!("IAI_RAW_CORPUS not set or not a directory; skipping Q1 taste A/B.");
        return;
    };
    let out = std::env::var_os("IAI_Q0_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("iai-q0"));
    let montage_dir = out.join("taste_ab");
    std::fs::create_dir_all(&montage_dir).expect("create taste A/B directory");

    let importer = RawImporter;
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("corpus directory is readable")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    entries.sort();

    let export = ExportOptions {
        flatten: true,
        embed_metadata: false,
        embed_icc: false,
        ..ExportOptions::default()
    };
    println!(
        "\nQ1 taste A/B — legacy-baked-v1 vs technical-neutral-v2\n  {} → montages left=legacy right=technical\n",
        montage_dir.display()
    );

    let mut csv = String::from(
        "file,lightness_legacy,lightness_technical,chroma_legacy,chroma_technical,chroma_delta,acutance_legacy,acutance_technical,shadow_legacy,shadow_technical,mid_legacy,mid_technical,high_legacy,high_technical\n",
    );
    let mut delta_sum = 0.0f64;
    let mut count = 0usize;

    for path in &entries {
        if !importer.can_import(path) {
            continue;
        }
        let name: String = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("<?>")
            .chars()
            .take(46)
            .collect();

        // Current default: make sure no recipe override lingers.
        std::env::remove_var("IAI_RAW_RENDER_RECIPE");
        let Some((on, w, h)) = render_resampled(path) else {
            println!("  DECODE-ERR {name}");
            continue;
        };
        // Technical-neutral v2 disables all decode-time taste as one recipe.
        std::env::set_var("IAI_RAW_RENDER_RECIPE", "technical");
        let off = render_resampled(path);
        std::env::remove_var("IAI_RAW_RENDER_RECIPE");
        let Some((off, ow, oh)) = off else {
            println!("  DECODE-ERR(off) {name}");
            continue;
        };
        if (ow, oh) != (w, h) {
            println!("  SIZE-MISMATCH {name}");
            continue;
        }

        let sum_on = summarize_encoded_rgba16(&on, w as usize, h as usize);
        let sum_off = summarize_encoded_rgba16(&off, w as usize, h as usize);
        let (Some(sum_on), Some(sum_off)) = (sum_on, sum_off) else {
            println!("  SUMMARY-ERR {name}");
            continue;
        };
        let delta = sum_on.mean_oklab_chroma - sum_off.mean_oklab_chroma;
        delta_sum += delta;
        count += 1;

        let stem: String = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("file")
            .chars()
            .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
            .take(44)
            .collect();
        let combined = montage(&on, &off, w, h);
        let png = montage_dir.join(format!("{stem}__on_vs_off.png"));
        PngExporter
            .export(&Canvas::from_rgba16(combined, w * 2, h), &png, &export)
            .expect("write taste A/B montage");

        println!(
            "  {:<40} C legacy={:.4} technical={:.4} Δ={:+.4} ({:+.1}%)  acutance {:.5}→{:.5}",
            name.chars().take(40).collect::<String>(),
            sum_on.mean_oklab_chroma,
            sum_off.mean_oklab_chroma,
            delta,
            if sum_off.mean_oklab_chroma > 1e-6 {
                delta / sum_off.mean_oklab_chroma * 100.0
            } else {
                0.0
            },
            sum_on.laplacian_acutance,
            sum_off.laplacian_acutance,
        );
        writeln!(
            csv,
            "{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.6},{:.6},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}",
            name.replace(',', ";"),
            sum_on.mean_oklab_lightness,
            sum_off.mean_oklab_lightness,
            sum_on.mean_oklab_chroma,
            sum_off.mean_oklab_chroma,
            delta,
            sum_on.laplacian_acutance,
            sum_off.laplacian_acutance,
            sum_on.shadow_chroma,
            sum_off.shadow_chroma,
            sum_on.midtone_chroma,
            sum_off.midtone_chroma,
            sum_on.highlight_chroma,
            sum_off.highlight_chroma,
        )
        .unwrap();
    }

    assert!(count > 0, "no corpus file rendered for the taste A/B");
    println!(
        "\naggregate: mean chroma added by legacy-baked-v1 = {:+.4} OKLab over {count} files\n  montages: {}",
        delta_sum / count as f64,
        montage_dir.display()
    );
    std::fs::write(out.join("q1_taste_ab.csv"), &csv).expect("write taste A/B CSV");
}

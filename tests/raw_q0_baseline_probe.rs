//! Quality Milestone Q0 — corpus-wide RAW baseline and profile-provenance audit.
//!
//! The RAM plan (`docs/planning/KE_HOACH_GIAM_RAM_MO_NHIEU_RAW_2026-08-25.md`,
//! Quality Q0) requires a *visible baseline* before any look is tuned: every
//! "washed out / wrong colour / soft / harsh" remark must map to a concrete
//! file, crop, metric, and the suspect pipeline stage — and each camera in the
//! corpus must have its profile resolution audited (which characterization was
//! selected, or why it fell back to the decoder matrix).
//!
//! This probe reads the private corpus from `IAI_RAW_CORPUS`, and for every
//! importable RAW it:
//!   1. records how the resolver characterized the camera (DCP / scene ICC /
//!      decoder-matrix fallback, with the fallback reason) and the embedded-JPEG
//!      match mode the decode chose — the Q0 profile-provenance audit;
//!   2. renders the neutral default look headless and measures no-reference
//!      quality (OKLab lightness/chroma by tonal band, clip fraction, acutance)
//!      plus a white-point cast proxy (bright-mean g/r, g/b);
//!   3. writes a width-normalized PNG per file so the renders form a contact
//!      sheet for on-screen A/B evaluation;
//!   4. optionally pairs each render against an ART reference render when one is
//!      supplied — ART stays a black-box oracle; nothing is copied from it.
//!
//! It is `#[ignore]`d so `cargo test` stays hermetic. The corpus files are local
//! private inputs only: never written back, never committed.
//!
//! ```text
//! $env:IAI_RAW_CORPUS='C:\Users\Admin\Pictures\anh-raw'
//! $env:IAI_Q0_OUT='C:\Users\Admin\Documents\IAI\target\q0'
//! # optional, once ART is built locally: a dir of <stem>.tif|tiff|png renders
//! # $env:IAI_ART_REFERENCE_DIR='C:\Users\Admin\Documents\IAI\target\q0\art'
//! cargo test --release --test raw_q0_baseline_probe -- --ignored --nocapture
//! ```

use std::fmt::Write as _;
use std::path::PathBuf;

use iai::core::camera_profile::resolver::SelectedProfileProvenance;
use iai::core::canvas::Canvas;
use iai::core::color_reference::{summarize_encoded_rgba16, ImageQualitySummary};
use iai::core::develop::DevelopSettings;
use iai::core::develop_scene::render_default_look;
use iai::formats::png::PngExporter;
use iai::formats::raw::RawImporter;
use iai::formats::{ExportOptions, Exporter, Importer};

/// Width every render is box-resampled to before metrics/PNG. Acutance is
/// resolution-dependent, so a fixed output width keeps the number comparable
/// across the corpus even though heights vary with each sensor's aspect ratio.
const METRIC_WIDTH: u32 = 1400;

fn corpus_dir() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var("IAI_RAW_CORPUS").ok()?);
    path.is_dir().then_some(path)
}

fn out_dir() -> PathBuf {
    std::env::var("IAI_Q0_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("iai-q0"))
}

/// Box-resample an interleaved RGBA16 image to `target_w` wide, aspect
/// preserved. Alpha is averaged with the rest; RAW renders are fully opaque, so
/// the resampled alpha stays `u16::MAX` and the summary accepts it.
fn box_resample_width(px16: &[u16], w: u32, h: u32, target_w: u32) -> (Vec<u16>, u32, u32) {
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

/// Mean of the brightest ~8% of pixels (by luma), 0..255 per channel. The ratios
/// g/r and g/b of this near-white proxy expose a systematic warm/green cast.
fn bright_mean_rgb8(px16: &[u16]) -> [f32; 3] {
    let mut lumas: Vec<u32> = px16
        .chunks_exact(4)
        .map(|p| p[0] as u32 * 2 + p[1] as u32 * 5 + p[2] as u32)
        .collect();
    if lumas.is_empty() {
        return [0.0; 3];
    }
    lumas.sort_unstable();
    let thresh = lumas[((lumas.len() as f64 * 0.92) as usize).min(lumas.len() - 1)];
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

/// Stable short discriminator for the CSV/JSON scoreboard column.
fn provenance_kind(selected: &SelectedProfileProvenance) -> &'static str {
    match selected {
        SelectedProfileProvenance::Dcp { .. } => "dcp",
        SelectedProfileProvenance::SceneIcc { .. } => "scene_icc",
        SelectedProfileProvenance::DecoderMatrix { .. } => "decoder_matrix",
    }
}

/// Human-readable one-line provenance detail (profile name / locator / fallback
/// reason) for the printed table and the JSON audit record.
fn provenance_detail(selected: &SelectedProfileProvenance) -> String {
    match selected {
        SelectedProfileProvenance::Dcp {
            tier,
            profile_name,
            unique_camera_model,
            illuminants,
            selected_cct_kelvin,
            ..
        } => format!(
            "tier={tier:?} name={:?} model={:?} illum={illuminants:?} cct={selected_cct_kelvin:.0}K",
            profile_name.as_deref().unwrap_or("-"),
            unique_camera_model.as_deref().unwrap_or("-"),
        ),
        SelectedProfileProvenance::SceneIcc { tier, locator, .. } => {
            format!("tier={tier:?} locator={locator}")
        }
        SelectedProfileProvenance::DecoderMatrix { backend, reason } => {
            format!("backend={backend:?} reason={reason:?}")
        }
    }
}

struct FileRecord {
    name: String,
    ext: String,
    camera: String,
    megapixels: f64,
    provenance_kind: &'static str,
    provenance_detail: String,
    jpeg_match: String,
    raw_recipe: &'static str,
    develop_engine: &'static str,
    working_space: String,
    output_space: String,
    pipeline_version: u16,
    summary: ImageQualitySummary,
    wb_g_over_r: f32,
    wb_g_over_b: f32,
}

#[test]
#[ignore = "requires a local RAW corpus via IAI_RAW_CORPUS; renders PNGs"]
fn raw_q0_corpus_baseline() {
    let Some(dir) = corpus_dir() else {
        eprintln!("IAI_RAW_CORPUS not set or not a directory; skipping Q0 corpus baseline.");
        return;
    };
    let out = out_dir();
    let contact = out.join("contact");
    std::fs::create_dir_all(&contact).expect("create Q0 output directory");

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
        "\nQuality Q0 corpus baseline — {} entries in {}\n  output: {}\n",
        entries.len(),
        dir.display(),
        out.display(),
    );

    let mut records: Vec<FileRecord> = Vec::new();
    let mut unsupported = 0usize;
    let mut failed = 0usize;

    for path in &entries {
        let name: String = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("<?>")
            .chars()
            .take(56)
            .collect();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        if !importer.can_import(path) {
            unsupported += 1;
            continue;
        }

        let canvas = match importer.import(path) {
            Ok(canvas) => canvas,
            Err(error) => {
                failed += 1;
                println!(
                    "  DECODE-ERR {name}: {}",
                    error.chars().take(60).collect::<String>()
                );
                continue;
            }
        };
        let scene = canvas
            .develop_source
            .as_ref()
            .expect("a RAW import must attach a Develop scene source");
        let characterization = scene
            .camera_profile
            .as_ref()
            .expect("a RAW scene must carry resolver provenance");
        let (kind, detail) = (
            provenance_kind(&characterization.resolution.selected),
            provenance_detail(&characterization.resolution.selected),
        );
        let jpeg_match = format!("{:?}", characterization.jpeg_match);
        let raw_recipe = characterization.raw_render_recipe.name();
        let develop_engine = DevelopSettings::default().develop_engine_version.label();
        let working_space = scene.color_pipeline.working.name().to_string();
        let output_space = format!("{:?}", scene.color_pipeline.output);
        let pipeline_version = scene.color_pipeline.algorithm_version;

        let px16 = render_default_look(scene);
        let (w, h) = (scene.width, scene.height);
        let (small, sw, sh) = box_resample_width(&px16, w, h, METRIC_WIDTH);
        let summary = summarize_encoded_rgba16(&small, sw as usize, sh as usize)
            .expect("neutral render must summarize");
        let wp = bright_mean_rgb8(&small);

        let stem: String = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("file")
            .chars()
            .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
            .take(48)
            .collect();
        let png = contact.join(format!("{stem}__neutral.png"));
        PngExporter
            .export(&Canvas::from_rgba16(small, sw, sh), &png, &export)
            .expect("write neutral contact PNG");

        let record = FileRecord {
            name: name.clone(),
            ext,
            camera: canvas.metadata.source_profile.chars().take(28).collect(),
            megapixels: (w as f64 * h as f64) / 1.0e6,
            provenance_kind: kind,
            provenance_detail: detail,
            jpeg_match,
            raw_recipe,
            develop_engine,
            working_space,
            output_space,
            pipeline_version,
            summary,
            wb_g_over_r: if wp[0] > 0.0 { wp[1] / wp[0] } else { 0.0 },
            wb_g_over_b: if wp[2] > 0.0 { wp[1] / wp[2] } else { 0.0 },
        };
        println!(
            "  {:<40} {:>5} {:>6.1}MP  {:<14} L={:.3} C={:.3} Csh={:.3} Chi={:.3} clip={:.2}% acut={:.5} wb g/r={:.3} g/b={:.3}",
            record.name.chars().take(40).collect::<String>(),
            record.ext,
            record.megapixels,
            record.provenance_kind,
            record.summary.mean_oklab_lightness,
            record.summary.mean_oklab_chroma,
            record.summary.shadow_chroma,
            record.summary.highlight_chroma,
            record.summary.clipped_pixel_fraction * 100.0,
            record.summary.laplacian_acutance,
            record.wb_g_over_r,
            record.wb_g_over_b,
        );
        records.push(record);
    }

    assert!(
        !records.is_empty(),
        "no RAW file in {} decoded through the Q0 baseline",
        dir.display()
    );

    // ── Provenance audit rollup ───────────────────────────────────────────────
    let dcp = records
        .iter()
        .filter(|r| r.provenance_kind == "dcp")
        .count();
    let scene_icc = records
        .iter()
        .filter(|r| r.provenance_kind == "scene_icc")
        .count();
    let fallback = records
        .iter()
        .filter(|r| r.provenance_kind == "decoder_matrix")
        .count();
    println!(
        "\nprofile provenance: {dcp} dcp, {scene_icc} scene_icc, {fallback} decoder_matrix (of {} decoded)",
        records.len()
    );
    for record in records.iter().filter(|r| r.provenance_kind != "dcp") {
        println!(
            "  no-DCP  {:<40}  {}",
            record.name.chars().take(40).collect::<String>(),
            record.provenance_detail,
        );
    }

    let mean = |select: fn(&FileRecord) -> f64| {
        records.iter().map(select).sum::<f64>() / records.len() as f64
    };
    println!(
        "\naggregate: mean L={:.3} C={:.3} Cshadow={:.3} Chigh={:.3} clip={:.2}% acutance={:.5} wb g/r={:.3}",
        mean(|r| r.summary.mean_oklab_lightness),
        mean(|r| r.summary.mean_oklab_chroma),
        mean(|r| r.summary.shadow_chroma),
        mean(|r| r.summary.highlight_chroma),
        mean(|r| r.summary.clipped_pixel_fraction) * 100.0,
        mean(|r| r.summary.laplacian_acutance),
        mean(|r| r.wb_g_over_r as f64),
    );
    println!(
        "  {} unsupported extension, {} decode errors, {} contact PNGs in {}",
        unsupported,
        failed,
        records.len(),
        contact.display()
    );

    // ── Machine-readable artifacts ────────────────────────────────────────────
    let mut csv = String::from(
        "file,ext,camera,megapixels,provenance,jpeg_match,raw_recipe,develop_engine,working_space,output_space,pipeline_version,mean_oklab_L,mean_oklab_C,shadow_chroma,midtone_chroma,highlight_chroma,clip_fraction,acutance,wb_g_over_r,wb_g_over_b\n",
    );
    for r in &records {
        writeln!(
            csv,
            "{},{},{},{:.2},{},{},{},{},{},{},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.6},{:.6},{:.4},{:.4}",
            r.name.replace(',', ";"),
            r.ext,
            r.camera.replace(',', ";"),
            r.megapixels,
            r.provenance_kind,
            r.jpeg_match,
            r.raw_recipe,
            r.develop_engine,
            r.working_space,
            r.output_space,
            r.pipeline_version,
            r.summary.mean_oklab_lightness,
            r.summary.mean_oklab_chroma,
            r.summary.shadow_chroma,
            r.summary.midtone_chroma,
            r.summary.highlight_chroma,
            r.summary.clipped_pixel_fraction,
            r.summary.laplacian_acutance,
            r.wb_g_over_r,
            r.wb_g_over_b,
        )
        .unwrap();
    }
    std::fs::write(out.join("q0_corpus_summary.csv"), &csv).expect("write Q0 summary CSV");

    let provenance_json = serde_json::json!({
        "corpus": dir.display().to_string(),
        "decoded": records.len(),
        "unsupported_ext": unsupported,
        "decode_errors": failed,
        "provenance_counts": {
            "dcp": dcp,
            "scene_icc": scene_icc,
            "decoder_matrix": fallback,
        },
        "files": records.iter().map(|r| serde_json::json!({
            "file": r.name,
            "ext": r.ext,
            "camera": r.camera,
            "megapixels": r.megapixels,
            "provenance": r.provenance_kind,
            "provenance_detail": r.provenance_detail,
            "jpeg_match": r.jpeg_match,
            "raw_recipe": r.raw_recipe,
            "develop_engine": r.develop_engine,
            "working_space": r.working_space,
            "output_space": r.output_space,
            "pipeline_version": r.pipeline_version,
            "mean_oklab_lightness": r.summary.mean_oklab_lightness,
            "mean_oklab_chroma": r.summary.mean_oklab_chroma,
            "shadow_chroma": r.summary.shadow_chroma,
            "midtone_chroma": r.summary.midtone_chroma,
            "highlight_chroma": r.summary.highlight_chroma,
            "clip_fraction": r.summary.clipped_pixel_fraction,
            "acutance": r.summary.laplacian_acutance,
            "wb_g_over_r": r.wb_g_over_r,
            "wb_g_over_b": r.wb_g_over_b,
        })).collect::<Vec<_>>(),
    });
    std::fs::write(
        out.join("q0_corpus_provenance.json"),
        serde_json::to_vec_pretty(&provenance_json).unwrap(),
    )
    .expect("write Q0 provenance JSON");

    // ── Optional ART pairing (black-box oracle; nothing copied from ART) ───────
    if let Some(art_dir) = std::env::var_os("IAI_ART_REFERENCE_DIR").map(PathBuf::from) {
        pair_against_art(&art_dir, &entries, &out);
    } else {
        println!(
            "\nART pairing skipped: set IAI_ART_REFERENCE_DIR to a folder of <stem>.tif|tiff|png ART renders to enable it."
        );
    }
}

/// When ART reference renders are available locally, report a paired per-file
/// delta (mean absolute encoded-channel difference at the metric grid) so the
/// iAi baseline can be read next to the ART oracle. ART is never a byte-exact
/// target — this is a difference magnitude for triage, not a pass/fail gate.
fn pair_against_art(art_dir: &std::path::Path, entries: &[PathBuf], out: &std::path::Path) {
    if !art_dir.is_dir() {
        println!(
            "\nART pairing skipped: {} is not a directory",
            art_dir.display()
        );
        return;
    }
    let mut rows = String::from("file,art_file,mean_abs_channel_diff_0_255\n");
    let mut paired = 0usize;
    for path in entries {
        if !RawImporter.can_import(path) {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };
        let art_path = ["tif", "tiff", "png"]
            .iter()
            .map(|ext| art_dir.join(format!("{stem}.{ext}")))
            .find(|candidate| candidate.is_file());
        let Some(art_path) = art_path else { continue };

        let canvas = match RawImporter.import(path) {
            Ok(canvas) => canvas,
            Err(_) => continue,
        };
        let scene = match canvas.develop_source.as_ref() {
            Some(scene) => scene,
            None => continue,
        };
        let px16 = render_default_look(scene);
        let (iai_small, sw, sh) =
            box_resample_width(&px16, scene.width, scene.height, METRIC_WIDTH);

        let decoded = image::ImageReader::open(&art_path)
            .ok()
            .and_then(|reader| reader.decode().ok());
        let art = match decoded {
            Some(image) => image.to_rgb8(),
            None => {
                println!("  ART decode failed for {}", art_path.display());
                continue;
            }
        };
        // Resample the ART render onto the same metric grid via nearest sampling
        // so the two share dimensions before the per-channel difference.
        let (aw, ah) = art.dimensions();
        let mut diff = 0f64;
        let mut n = 0f64;
        for y in 0..sh {
            let ay = (u64::from(y) * u64::from(ah) / u64::from(sh)).min(u64::from(ah) - 1) as u32;
            for x in 0..sw {
                let ax =
                    (u64::from(x) * u64::from(aw) / u64::from(sw)).min(u64::from(aw) - 1) as u32;
                let a = art.get_pixel(ax, ay).0;
                let i = (y as usize * sw as usize + x as usize) * 4;
                for c in 0..3 {
                    let iai = (iai_small[i + c] / 257) as f64;
                    diff += (iai - a[c] as f64).abs();
                    n += 1.0;
                }
            }
        }
        let mean_abs = diff / n.max(1.0);
        writeln!(
            rows,
            "{},{},{:.3}",
            stem.replace(',', ";"),
            art_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .replace(',', ";"),
            mean_abs,
        )
        .unwrap();
        println!("  ART pair {stem}: mean |Δ| = {mean_abs:.2}/255");
        paired += 1;
    }
    if paired > 0 {
        std::fs::write(out.join("q0_art_pairing.csv"), &rows).expect("write ART pairing CSV");
        println!(
            "ART pairing: {paired} files → {}",
            out.join("q0_art_pairing.csv").display()
        );
    } else {
        println!(
            "ART pairing: no <stem>.tif|tiff|png matches found in {}",
            art_dir.display()
        );
    }
}

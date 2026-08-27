//! Quality Milestone Q1 — reproducible ART ↔ iAi A/B evidence harness.
//!
//! The Develop completion plan
//! (`docs/planning/KE_HOACH_HOAN_THIEN_DEVELOP_IAI_2026-08-27.md`, Phase 1)
//! requires *reproducible visual evidence* before any Light/Mixer/Detail tuning:
//! render a locked set of recipes from both iAi (Develop3 candidate) and ART
//! (a black-box oracle — never copied), on the same RAW corpus, at the same size
//! and white balance, then measure the difference and hand a blind contact sheet
//! to the owner for an eyeball verdict.
//!
//! ART stays a pure black-box oracle. This harness authors ART `.arp` recipes
//! and drives `ART-cli.exe`; it copies no ART code, constant, LUT, profile or
//! asset into iAi. The ART renders and the iAi renders are compared only as
//! finished images.
//!
//! It is `#[ignore]`d so `cargo test` stays hermetic. Enable it with:
//!
//! ```text
//! $env:IAI_RAW_CORPUS='C:\Users\Admin\Pictures\anh-raw'
//! $env:IAI_ART_CLI='C:\Users\Admin\Pictures\1111\ART_1.26.7_Win64_portable'
//! $env:IAI_Q1_OUT='C:\Users\Admin\Documents\IAI\target\q1_ab'
//! # optional: limit scope for a first, fast review
//! $env:IAI_Q1_FILES='_DLL6009,KKK5695,DSC02534'   # name substrings
//! $env:IAI_Q1_MAX_FILES='6'
//! $env:IAI_Q1_RECIPES='neutral,exp_p1,light_shadows_up,color_sat_up,detail_sharpen'
//! cargo test --release --test raw_q1_art_ab -- --ignored --nocapture
//! ```
//!
//! Outputs (under `IAI_Q1_OUT`): labeled and blind side-by-side PNG pairs,
//! `index.html` / `blind.html`, a full `q1_ab_manifest.json`, per-pair metrics
//! (`q1_ab_metrics.csv/.json`), a blind answer key and a missing/extra report.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use iai::core::canvas::Canvas;
use iai::core::color_reference::{summarize_encoded_rgba16, ImageQualitySummary};
use iai::core::develop::{DevelopEngineVersion, DevelopSettings};
use iai::core::develop_scene::apply_scene_to_tilemap;
use iai::core::perceptual_color::linear_srgb_to_oklab;
use iai::formats::png::PngExporter;
use iai::formats::raw::RawImporter;
use iai::formats::{ExportOptions, Exporter, Importer};

/// Long-edge width both renders are box-resampled to before metrics/compositing.
/// Fixed so acutance and the contact sheets stay comparable across the corpus.
const DEFAULT_WIDTH: u32 = 1400;

// ── Recipe table ────────────────────────────────────────────────────────────

/// One A/B recipe. `apply_iai` mutates a Develop3 baseline; `art_fragment` is an
/// ART `.arp` fragment layered on top of the shared base profile. Both the iAi
/// slider values and the ART fragment are recorded verbatim in the manifest so
/// the pairing is reproducible and honest about what each engine was told to do.
struct Recipe {
    id: &'static str,
    family: &'static str,
    /// Human summary of the iAi side (recorded in the manifest).
    iai_desc: &'static str,
    /// Human summary of the ART side (recorded in the manifest).
    art_desc: &'static str,
    apply_iai: fn(&mut DevelopSettings),
    /// Extra `.arp` sections appended after the shared base (per-key override).
    art_fragment: &'static str,
}

/// The locked recipe set. Exposure and (approximately) the Light zones map
/// cleanly across engines; the colour/detail axes are representative pushes on
/// each engine's own control — never a claim of parameter parity.
fn recipes() -> Vec<Recipe> {
    vec![
        Recipe {
            id: "neutral",
            family: "baseline",
            iai_desc: "Develop3 default look, camera WB",
            art_desc: "ART default (auto-matched tone), camera WB",
            apply_iai: |_s| {},
            art_fragment: "",
        },
        Recipe {
            id: "exp_m2",
            family: "exposure",
            iai_desc: "exposure -20 (=-2 EV)",
            art_desc: "Exposure Compensation -2 EV",
            apply_iai: |s| s.exposure = -20.0,
            art_fragment: "[Exposure]\nEnabled=true\nCompensation=-2\n",
        },
        Recipe {
            id: "exp_m1",
            family: "exposure",
            iai_desc: "exposure -10 (=-1 EV)",
            art_desc: "Exposure Compensation -1 EV",
            apply_iai: |s| s.exposure = -10.0,
            art_fragment: "[Exposure]\nEnabled=true\nCompensation=-1\n",
        },
        Recipe {
            id: "exp_p1",
            family: "exposure",
            iai_desc: "exposure +10 (=+1 EV)",
            art_desc: "Exposure Compensation +1 EV",
            apply_iai: |s| s.exposure = 10.0,
            art_fragment: "[Exposure]\nEnabled=true\nCompensation=1\n",
        },
        Recipe {
            id: "exp_p2",
            family: "exposure",
            iai_desc: "exposure +20 (=+2 EV)",
            art_desc: "Exposure Compensation +2 EV",
            apply_iai: |s| s.exposure = 20.0,
            art_fragment: "[Exposure]\nEnabled=true\nCompensation=2\n",
        },
        Recipe {
            id: "light_shadows_up",
            family: "light",
            iai_desc: "shadows +120 (of ±200)",
            art_desc: "ToneEqualizer shadows band +60 (of ±100)",
            apply_iai: |s| s.shadows = 120.0,
            art_fragment: "[ToneEqualizer]\nEnabled=true\nBand1=60\nRegularization=4\n",
        },
        Recipe {
            id: "light_highlights_down",
            family: "light",
            iai_desc: "highlights -120 (of ±200)",
            art_desc: "ToneEqualizer highlights band -60 (of ±100)",
            apply_iai: |s| s.highlights = -120.0,
            art_fragment: "[ToneEqualizer]\nEnabled=true\nBand3=-60\nRegularization=4\n",
        },
        Recipe {
            // Global saturation stands in for the colour/Mixer family this round.
            // Magnitudes are a MILD push and are not per-unit equal across engines
            // (iAi's saturation is stronger per unit), so this is a "does skin/colour
            // stay natural under a gentle lift" test, judged by eye — not a parity
            // claim. Band-targeted Mixer A/B is a documented follow-up.
            id: "color_sat_up",
            family: "color",
            iai_desc: "saturation +40 (of ±200)",
            art_desc: "Saturation +25 (of ±100)",
            apply_iai: |s| s.saturation = 40.0,
            art_fragment: "[Saturation]\nEnabled=true\nSaturation=25\nVibrance=0\n",
        },
        Recipe {
            id: "detail_sharpen",
            family: "detail",
            iai_desc: "sharpening 75 (of 100)",
            art_desc: "Sharpening USM Amount 400, Radius 0.7",
            apply_iai: |s| s.sharpening = 75.0,
            art_fragment: concat!(
                "[Sharpening]\nEnabled=true\nContrast=0\nMethod=usm\nRadius=0.7\n",
                "Amount=400\nThreshold=20;80;2000;1200;\nOnlyEdges=false\n"
            ),
        },
    ]
}

/// Shared ART base profile: camera WB, camera DCP input, Rec2020 working,
/// sRGB output, ART's own auto-matched tone curve, and a long-edge resize so the
/// oracle renders straight onto a compact contact-sheet grid.
fn art_base_profile(long_edge: u32) -> String {
    format!(
        "[Version]\nAppVersion=1.26.7\nVersion=1045\n\
         [White Balance]\nEnabled=true\nSetting=Camera\n\
         [Color Management]\nInputProfile=(cameraICC)\nApplyHueSatMap=true\n\
         WorkingProfile=Rec2020\nOutputProfile=RTv2_sRGB\nOutputProfileIntent=Relative\nOutputBPC=true\n\
         [Exposure]\nEnabled=true\nCompensation=0\n\
         [ToneCurve]\nEnabled=true\nHistogramMatching=true\nCurveFromHistogramMatching=true\n\
         [Resize]\nEnabled=true\nAppliesTo=Cropped area\nMethod=Lanczos\n\
         DataSpecified=3\nWidth={long_edge}\nHeight={long_edge}\nAllowUpscaling=false\n"
    )
}

// ── Environment ─────────────────────────────────────────────────────────────

fn corpus_dir() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var("IAI_RAW_CORPUS").ok()?);
    path.is_dir().then_some(path)
}

fn out_dir() -> PathBuf {
    std::env::var("IAI_Q1_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("target/q1_ab"))
}

/// Resolve `IAI_ART_CLI` to `(exe_path, working_dir)`. Accepts either the exe
/// path or the portable folder (in which case `ART-cli.exe` is appended). The
/// working dir is the exe's folder so its bundled DLLs and profiles resolve.
fn art_cli() -> Option<(PathBuf, PathBuf)> {
    let raw = PathBuf::from(std::env::var("IAI_ART_CLI").ok()?);
    let exe = if raw.is_dir() {
        raw.join("ART-cli.exe")
    } else {
        raw
    };
    let dir = exe.parent()?.to_path_buf();
    exe.is_file().then_some((exe, dir))
}

fn env_width() -> u32 {
    std::env::var("IAI_Q1_WIDTH")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_WIDTH)
}

// ── Resampling / colour helpers ─────────────────────────────────────────────

/// Box-resample interleaved RGBA16 to `tw` wide, aspect preserved.
fn box_resample_width(px16: &[u16], w: u32, h: u32, tw: u32) -> (Vec<u16>, u32, u32) {
    let sw = w.min(tw).max(1);
    let sh = (((h as u64) * (sw as u64) + (w as u64) / 2) / (w as u64)).max(1) as u32;
    (box_resample_to(px16, w, h, sw, sh), sw, sh)
}

/// Box-resample interleaved RGBA16 to an exact `(dw, dh)` grid.
fn box_resample_to(px16: &[u16], w: u32, h: u32, dw: u32, dh: u32) -> Vec<u16> {
    let mut out = vec![0u16; dw as usize * dh as usize * 4];
    for oy in 0..dh {
        let y0 = (u64::from(oy) * u64::from(h) / u64::from(dh)) as u32;
        let y1 = ((u64::from(oy + 1) * u64::from(h)) / u64::from(dh)).max(u64::from(y0) + 1) as u32;
        for ox in 0..dw {
            let x0 = (u64::from(ox) * u64::from(w) / u64::from(dw)) as u32;
            let x1 =
                ((u64::from(ox + 1) * u64::from(w)) / u64::from(dw)).max(u64::from(x0) + 1) as u32;
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
            let o = (oy as usize * dw as usize + ox as usize) * 4;
            for c in 0..4 {
                out[o + c] = (acc[c] / n.max(1)) as u16;
            }
        }
    }
    out
}

fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// OKLab (L, a, b) of an encoded-sRGB 16-bit pixel.
fn oklab_of(px: &[u16]) -> (f32, f32, f32) {
    let lin = [
        srgb_to_linear(px[0] as f32 / 65535.0),
        srgb_to_linear(px[1] as f32 / 65535.0),
        srgb_to_linear(px[2] as f32 / 65535.0),
    ];
    let lab = linear_srgb_to_oklab(lin);
    (lab.l, lab.a, lab.b)
}

/// Rec.601 luma of an encoded-sRGB 16-bit pixel, 0..1.
fn luma01(px: &[u16]) -> f32 {
    (px[0] as f32 * 0.299 + px[1] as f32 * 0.587 + px[2] as f32 * 0.114) / 65535.0
}

fn hue_gap_deg(a: f32, b: f32) -> f32 {
    let d = (a - b).abs() % 360.0;
    d.min(360.0 - d)
}

// ── Cross metrics (never a single mean-delta number) ─────────────────────────

#[derive(Default)]
struct CrossMetrics {
    /// Mean |Δ| per channel, 0..255, over the whole frame.
    mad_overall: f64,
    /// Same, split by iAi luma tertile.
    mad_shadow: f64,
    mad_midtone: f64,
    mad_highlight: f64,
    /// Mean OKLab hue difference (deg) over pixels chromatic in both renders.
    hue_drift_deg: f64,
    /// Mean chroma difference over near-neutral pixels (neutral integrity).
    neutral_chroma_delta: f64,
}

fn cross_metrics(iai: &[u16], art: &[u16], w: u32, h: u32) -> CrossMetrics {
    let mut acc = [0f64; 4]; // overall, shadow, mid, high
    let mut cnt = [0f64; 4];
    let mut hue_sum = 0f64;
    let mut hue_n = 0f64;
    let mut neu_sum = 0f64;
    let mut neu_n = 0f64;
    for i in 0..(w as usize * h as usize) {
        let p = i * 4;
        let a = &iai[p..p + 4];
        let b = &art[p..p + 4];
        let mut d = 0f64;
        for c in 0..3 {
            d += ((a[c] as f64 - b[c] as f64) / 257.0).abs();
        }
        d /= 3.0;
        acc[0] += d;
        cnt[0] += 1.0;
        let band = {
            let l = luma01(a);
            if l < 1.0 / 3.0 {
                1
            } else if l < 2.0 / 3.0 {
                2
            } else {
                3
            }
        };
        acc[band] += d;
        cnt[band] += 1.0;

        let (_, aa, ab) = oklab_of(a);
        let (_, ba, bb) = oklab_of(b);
        let ca = aa.hypot(ab);
        let cb = ba.hypot(bb);
        if ca > 0.02 && cb > 0.02 {
            let ha = ab.atan2(aa).to_degrees();
            let hb = bb.atan2(ba).to_degrees();
            hue_sum += hue_gap_deg(ha, hb) as f64;
            hue_n += 1.0;
        }
        if ca < 0.02 && cb < 0.02 {
            neu_sum += (ca - cb).abs() as f64;
            neu_n += 1.0;
        }
    }
    CrossMetrics {
        mad_overall: acc[0] / cnt[0].max(1.0),
        mad_shadow: acc[1] / cnt[1].max(1.0),
        mad_midtone: acc[2] / cnt[2].max(1.0),
        mad_highlight: acc[3] / cnt[3].max(1.0),
        hue_drift_deg: hue_sum / hue_n.max(1.0),
        neutral_chroma_delta: neu_sum / neu_n.max(1.0),
    }
}

// ── Small utilities ─────────────────────────────────────────────────────────

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(s, "{b:02x}").unwrap();
    }
    s
}

fn sha256_file(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => hex(&hmac_sha256::Hash::hash(&bytes)),
        Err(_) => String::from("<unreadable>"),
    }
}

/// Deterministic per-pair coin flip for blind ordering (reproducible, no RNG).
fn blind_swap(key: &str) -> bool {
    hmac_sha256::Hash::hash(key.as_bytes())[0] & 1 == 1
}

fn sanitize_stem(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
        .take(40)
        .collect()
}

fn export_rgba16(px: Vec<u16>, w: u32, h: u32, path: &Path) {
    let opts = ExportOptions {
        flatten: true,
        embed_metadata: false,
        embed_icc: false,
        ..ExportOptions::default()
    };
    PngExporter
        .export(&Canvas::from_rgba16(px, w, h), path, &opts)
        .expect("write PNG");
}

/// Compose two same-size RGBA16 frames left|right with a neutral-grey gutter.
fn compose_h(left: &[u16], right: &[u16], w: u32, h: u32, gap: u32) -> (Vec<u16>, u32) {
    let ow = w * 2 + gap;
    let mut out = vec![0u16; ow as usize * h as usize * 4];
    for y in 0..h as usize {
        let row = y * ow as usize * 4;
        // Left.
        for x in 0..w as usize {
            let s = (y * w as usize + x) * 4;
            let d = row + x * 4;
            out[d..d + 4].copy_from_slice(&left[s..s + 4]);
        }
        // Gutter.
        for x in 0..gap as usize {
            let d = row + (w as usize + x) * 4;
            out[d..d + 4].copy_from_slice(&[24000, 24000, 24000, 65535]);
        }
        // Right.
        for x in 0..w as usize {
            let s = (y * w as usize + x) * 4;
            let d = row + (w as usize + gap as usize + x) * 4;
            out[d..d + 4].copy_from_slice(&right[s..s + 4]);
        }
    }
    (out, ow)
}

// ── ART driver ──────────────────────────────────────────────────────────────

/// Render one RAW through ART-cli with `base + fragment`, into a 16-bit PNG.
/// Returns the output path on success. Pure black-box: ART is only invoked.
fn art_render(
    art: &(PathBuf, PathBuf),
    raw: &Path,
    base_arp: &Path,
    fragment_arp: Option<&Path>,
    out_png: &Path,
) -> Result<(), String> {
    let (exe, dir) = art;
    let mut cmd = Command::new(exe);
    cmd.current_dir(dir)
        .arg("-Y")
        .arg("-o")
        .arg(out_png)
        .arg("-n")
        .arg("-b16")
        .arg("-q")
        .arg("-p")
        .arg(base_arp);
    if let Some(frag) = fragment_arp {
        cmd.arg("-p").arg(frag);
    }
    cmd.arg("-c").arg(raw);
    let output = cmd
        .output()
        .map_err(|e| format!("spawn ART-cli failed: {e}"))?;
    if !out_png.is_file() {
        let tail: String = String::from_utf8_lossy(&output.stdout)
            .lines()
            .chain(String::from_utf8_lossy(&output.stderr).lines())
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .join(" | ");
        return Err(format!(
            "no output (status {:?}): {tail}",
            output.status.code()
        ));
    }
    Ok(())
}

/// Decode an ART PNG/TIFF render to interleaved RGBA16 with dimensions.
fn decode_rgba16(path: &Path) -> Option<(Vec<u16>, u32, u32)> {
    let img = image::ImageReader::open(path).ok()?.decode().ok()?;
    let rgba = img.to_rgba16();
    let (w, h) = rgba.dimensions();
    Some((rgba.into_raw(), w, h))
}

// ── Records ─────────────────────────────────────────────────────────────────

struct PairRecord {
    file: String,
    stem: String,
    camera: String,
    recipe: String,
    family: String,
    out_w: u32,
    out_h: u32,
    iai_desc: String,
    art_desc: String,
    provenance_kind: String,
    raw_recipe: String,
    working_space: String,
    output_space: String,
    raw_sha256: String,
    iai_sha256: String,
    art_sha256: String,
    iai_summary: ImageQualitySummary,
    art_summary: ImageQualitySummary,
    cross: CrossMetrics,
    pair_png: String,
}

#[test]
#[ignore = "requires IAI_RAW_CORPUS and IAI_ART_CLI; drives ART-cli and renders PNGs"]
fn raw_q1_art_ab() {
    let Some(dir) = corpus_dir() else {
        eprintln!("IAI_RAW_CORPUS not set or not a directory; skipping Q1 ART A/B.");
        return;
    };
    let art = art_cli();
    if art.is_none() {
        eprintln!(
            "IAI_ART_CLI not set to ART-cli.exe or the ART portable folder; skipping Q1 ART A/B."
        );
        return;
    }
    let art = art.unwrap();
    let out = out_dir();
    let width = env_width();

    let pairs_dir = out.join("pairs");
    let blind_dir = out.join("blind");
    let iai_dir = out.join("iai_png");
    let art_out_dir = out.join("art_png");
    let arp_dir = out.join("art_arp");
    for d in [&pairs_dir, &blind_dir, &iai_dir, &art_out_dir, &arp_dir] {
        std::fs::create_dir_all(d).expect("create Q1 output dir");
    }

    // File selection: optional name-substring filter + optional cap.
    let file_filter: Vec<String> = std::env::var("IAI_Q1_FILES")
        .ok()
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let max_files: usize = std::env::var("IAI_Q1_MAX_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(usize::MAX);

    // Recipe selection.
    let recipe_filter: Vec<String> = std::env::var("IAI_Q1_RECIPES")
        .ok()
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let all_recipes = recipes();
    let selected_recipes: Vec<&Recipe> = all_recipes
        .iter()
        .filter(|r| recipe_filter.is_empty() || recipe_filter.iter().any(|f| f == r.id))
        .collect();

    // Base ART profile, written once.
    let base_arp = arp_dir.join("_base.arp");
    std::fs::write(&base_arp, art_base_profile(width)).expect("write base .arp");

    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("corpus directory is readable")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && RawImporter.can_import(p))
        .collect();
    entries.sort();
    let selected_files: Vec<PathBuf> = entries
        .into_iter()
        .filter(|p| {
            if file_filter.is_empty() {
                return true;
            }
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            file_filter.iter().any(|f| name.contains(f.as_str()))
        })
        .take(max_files)
        .collect();

    println!(
        "\nQuality Q1 ART A/B — {} files × {} recipes, width {}\n  corpus: {}\n  ART:    {}\n  out:    {}\n",
        selected_files.len(),
        selected_recipes.len(),
        width,
        dir.display(),
        art.0.display(),
        out.display(),
    );

    let mut records: Vec<PairRecord> = Vec::new();
    let mut missing: Vec<String> = Vec::new();

    for path in &selected_files {
        let file = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("<?>")
            .to_string();
        let stem = sanitize_stem(path.file_stem().and_then(|s| s.to_str()).unwrap_or("file"));

        let canvas = match RawImporter.import(path) {
            Ok(c) => c,
            Err(e) => {
                missing.push(format!(
                    "DECODE-ERR {file}: {}",
                    e.chars().take(80).collect::<String>()
                ));
                continue;
            }
        };
        let scene = match canvas.develop_source.as_ref() {
            Some(s) => s,
            None => {
                missing.push(format!("NO-SCENE {file}"));
                continue;
            }
        };
        let characterization = scene.camera_profile.as_ref();
        let provenance_kind = characterization
            .map(|c| match &c.resolution.selected {
                iai::core::camera_profile::resolver::SelectedProfileProvenance::Dcp { .. } => "dcp",
                iai::core::camera_profile::resolver::SelectedProfileProvenance::SceneIcc {
                    ..
                } => "scene_icc",
                iai::core::camera_profile::resolver::SelectedProfileProvenance::DecoderMatrix {
                    ..
                } => "decoder_matrix",
            })
            .unwrap_or("none")
            .to_string();
        let raw_recipe = characterization
            .map(|c| c.raw_render_recipe.name())
            .unwrap_or("-")
            .to_string();
        let working_space = scene.color_pipeline.working.name().to_string();
        let output_space = format!("{:?}", scene.color_pipeline.output);
        let camera: String = canvas.metadata.source_profile.chars().take(40).collect();
        let raw_sha256 = sha256_file(path);

        println!("• {file}  [{provenance_kind}, {raw_recipe}]");

        for recipe in &selected_recipes {
            // ── iAi render (Develop3 candidate) ──
            let mut settings = DevelopSettings {
                develop_engine_version: DevelopEngineVersion::Develop3,
                ..Default::default()
            };
            (recipe.apply_iai)(&mut settings);
            let iai_full = apply_scene_to_tilemap(scene, &settings, None).flatten16();
            let (iai16, ow, oh) = box_resample_width(&iai_full, scene.width, scene.height, width);
            drop(iai_full);
            let iai_png = iai_dir.join(format!("{stem}__{}.png", recipe.id));
            export_rgba16(iai16.clone(), ow, oh, &iai_png);

            // ── ART render (black-box oracle) ──
            let fragment_arp = if recipe.art_fragment.is_empty() {
                None
            } else {
                let p = arp_dir.join(format!("{stem}__{}.arp", recipe.id));
                std::fs::write(&p, recipe.art_fragment).expect("write recipe .arp");
                Some(p)
            };
            let art_png = art_out_dir.join(format!("{stem}__{}.png", recipe.id));
            if let Err(e) = art_render(&art, path, &base_arp, fragment_arp.as_deref(), &art_png) {
                missing.push(format!("ART-FAIL {file} / {}: {e}", recipe.id));
                println!("    {:<22} ART render failed: {e}", recipe.id);
                continue;
            }
            let Some((art_raw, aw, ah)) = decode_rgba16(&art_png) else {
                missing.push(format!("ART-DECODE {file} / {}", recipe.id));
                continue;
            };
            let art16 = box_resample_to(&art_raw, aw, ah, ow, oh);
            drop(art_raw);

            // ── Metrics ──
            let iai_summary = summarize_encoded_rgba16(&iai16, ow as usize, oh as usize)
                .expect("iAi render summarizes");
            let art_summary = summarize_encoded_rgba16(&art16, ow as usize, oh as usize)
                .expect("ART render summarizes");
            let cross = cross_metrics(&iai16, &art16, ow, oh);

            // ── Contact pairs: labeled (iAi | ART) + blind (deterministic swap) ──
            let (labeled, lw) = compose_h(&iai16, &art16, ow, oh, 12);
            let pair_name = format!("{stem}__{}.png", recipe.id);
            export_rgba16(labeled, lw, oh, &pairs_dir.join(&pair_name));

            let swap = blind_swap(&format!("{stem}:{}", recipe.id));
            let (blind, bw) = if swap {
                compose_h(&art16, &iai16, ow, oh, 12)
            } else {
                compose_h(&iai16, &art16, ow, oh, 12)
            };
            let recipe_blind_dir = blind_dir.join(recipe.id);
            std::fs::create_dir_all(&recipe_blind_dir).ok();
            export_rgba16(blind, bw, oh, &recipe_blind_dir.join(format!("{stem}.png")));

            println!(
                "    {:<22} MAD all={:.2} sh={:.2} mid={:.2} hi={:.2} | hueΔ={:.2}° | iAi C={:.3} ART C={:.3} | iAi clip={:.2}% ART clip={:.2}%",
                recipe.id,
                cross.mad_overall,
                cross.mad_shadow,
                cross.mad_midtone,
                cross.mad_highlight,
                cross.hue_drift_deg,
                iai_summary.mean_oklab_chroma,
                art_summary.mean_oklab_chroma,
                iai_summary.clipped_pixel_fraction * 100.0,
                art_summary.clipped_pixel_fraction * 100.0,
            );

            records.push(PairRecord {
                file: file.clone(),
                stem: stem.clone(),
                camera: camera.clone(),
                recipe: recipe.id.to_string(),
                family: recipe.family.to_string(),
                out_w: ow,
                out_h: oh,
                iai_desc: recipe.iai_desc.to_string(),
                art_desc: recipe.art_desc.to_string(),
                provenance_kind: provenance_kind.clone(),
                raw_recipe: raw_recipe.clone(),
                working_space: working_space.clone(),
                output_space: output_space.clone(),
                raw_sha256: raw_sha256.clone(),
                iai_sha256: sha256_file(&iai_png),
                art_sha256: sha256_file(&art_png),
                iai_summary,
                art_summary,
                cross,
                pair_png: format!("pairs/{pair_name}"),
            });
        }
    }

    assert!(
        !records.is_empty(),
        "no A/B pair rendered; check IAI_RAW_CORPUS / IAI_ART_CLI / filters"
    );

    write_metrics(&out, &records);
    write_manifest(&out, &art, width, &records, &selected_files);
    write_missing(&out, &missing, &records);
    write_html(&out, &records, &selected_recipes);
    write_blind_key(&out, &records);

    println!(
        "\nQ1 A/B done: {} pairs, {} issues.\n  labeled: {}\n  blind:   {}\n  open:    {}",
        records.len(),
        missing.len(),
        pairs_dir.display(),
        blind_dir.display(),
        out.join("index.html").display(),
    );
}

// ── Artifact writers ────────────────────────────────────────────────────────

fn write_metrics(out: &Path, records: &[PairRecord]) {
    let mut csv = String::from(
        "file,camera,recipe,family,out_w,out_h,provenance,raw_recipe,working,output,\
         mad_overall,mad_shadow,mad_midtone,mad_highlight,hue_drift_deg,neutral_chroma_delta,\
         iai_L,iai_C,iai_Csh,iai_Cmid,iai_Chi,iai_clip,iai_acut,\
         art_L,art_C,art_Csh,art_Cmid,art_Chi,art_clip,art_acut\n",
    );
    for r in records {
        writeln!(
            csv,
            "{},{},{},{},{},{},{},{},{},{},{:.3},{:.3},{:.3},{:.3},{:.3},{:.5},{:.4},{:.4},{:.4},{:.4},{:.4},{:.5},{:.6},{:.4},{:.4},{:.4},{:.4},{:.4},{:.5},{:.6}",
            r.file.replace(',', ";"),
            r.camera.replace(',', ";"),
            r.recipe,
            r.family,
            r.out_w,
            r.out_h,
            r.provenance_kind,
            r.raw_recipe,
            r.working_space,
            r.output_space,
            r.cross.mad_overall,
            r.cross.mad_shadow,
            r.cross.mad_midtone,
            r.cross.mad_highlight,
            r.cross.hue_drift_deg,
            r.cross.neutral_chroma_delta,
            r.iai_summary.mean_oklab_lightness,
            r.iai_summary.mean_oklab_chroma,
            r.iai_summary.shadow_chroma,
            r.iai_summary.midtone_chroma,
            r.iai_summary.highlight_chroma,
            r.iai_summary.clipped_pixel_fraction,
            r.iai_summary.laplacian_acutance,
            r.art_summary.mean_oklab_lightness,
            r.art_summary.mean_oklab_chroma,
            r.art_summary.shadow_chroma,
            r.art_summary.midtone_chroma,
            r.art_summary.highlight_chroma,
            r.art_summary.clipped_pixel_fraction,
            r.art_summary.laplacian_acutance,
        )
        .unwrap();
    }
    std::fs::write(out.join("q1_ab_metrics.csv"), &csv).expect("write metrics CSV");

    let json = serde_json::json!({
        "pairs": records.iter().map(|r| serde_json::json!({
            "file": r.file,
            "recipe": r.recipe,
            "family": r.family,
            "cross": {
                "mad_overall": r.cross.mad_overall,
                "mad_shadow": r.cross.mad_shadow,
                "mad_midtone": r.cross.mad_midtone,
                "mad_highlight": r.cross.mad_highlight,
                "hue_drift_deg": r.cross.hue_drift_deg,
                "neutral_chroma_delta": r.cross.neutral_chroma_delta,
            },
            "iai": summary_json(&r.iai_summary),
            "art": summary_json(&r.art_summary),
        })).collect::<Vec<_>>(),
    });
    std::fs::write(
        out.join("q1_ab_metrics.json"),
        serde_json::to_vec_pretty(&json).unwrap(),
    )
    .expect("write metrics JSON");
}

fn summary_json(s: &ImageQualitySummary) -> serde_json::Value {
    serde_json::json!({
        "mean_oklab_L": s.mean_oklab_lightness,
        "mean_oklab_C": s.mean_oklab_chroma,
        "shadow_chroma": s.shadow_chroma,
        "midtone_chroma": s.midtone_chroma,
        "highlight_chroma": s.highlight_chroma,
        "clip_fraction": s.clipped_pixel_fraction,
        "acutance": s.laplacian_acutance,
    })
}

fn write_manifest(
    out: &Path,
    art: &(PathBuf, PathBuf),
    width: u32,
    records: &[PairRecord],
    files: &[PathBuf],
) {
    let manifest = serde_json::json!({
        "schema": "iai.q1_ab.v1",
        "generated_from_baseline": "Develop3 (candidate) vs ART 1.26.7 (black-box oracle)",
        "art_cli": art.0.display().to_string(),
        "contact_width_px": width,
        "wb": "camera as-shot (both engines)",
        "crop": "none",
        "iai_engine": "Develop3",
        "art_output_profile": "RTv2_sRGB",
        "art_working_profile": "Rec2020",
        "art_input_profile": "(cameraICC)",
        "corpus_files": files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "pairs": records.iter().map(|r| serde_json::json!({
            "file": r.file,
            "camera": r.camera,
            "recipe": r.recipe,
            "family": r.family,
            "output_size": [r.out_w, r.out_h],
            "iai": {
                "engine": "Develop3",
                "recipe": r.iai_desc,
                "raw_render_recipe": r.raw_recipe,
                "input_profile_provenance": r.provenance_kind,
                "working_space": r.working_space,
                "output_space": r.output_space,
                "render_sha256": r.iai_sha256,
            },
            "art": {
                "app": "ART 1.26.7",
                "recipe": r.art_desc,
                "input_profile": "(cameraICC)",
                "working_space": "Rec2020",
                "output_space": "RTv2_sRGB",
                "render_sha256": r.art_sha256,
            },
            "raw_sha256": r.raw_sha256,
            "pair_png": r.pair_png,
        })).collect::<Vec<_>>(),
    });
    std::fs::write(
        out.join("q1_ab_manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .expect("write manifest");
}

fn write_missing(out: &Path, missing: &[String], records: &[PairRecord]) {
    let mut txt = String::new();
    writeln!(txt, "Q1 ART A/B — reference completeness report").unwrap();
    writeln!(txt, "pairs rendered: {}", records.len()).unwrap();
    writeln!(txt, "issues: {}", missing.len()).unwrap();
    writeln!(txt).unwrap();
    if missing.is_empty() {
        writeln!(txt, "No missing/failed references.").unwrap();
    } else {
        for m in missing {
            writeln!(txt, "- {m}").unwrap();
        }
    }
    std::fs::write(out.join("q1_ab_missing.txt"), &txt).expect("write missing report");
}

fn write_blind_key(out: &Path, records: &[PairRecord]) {
    // The key maps each blind pair to which side is iAi. Kept OUT of blind.html
    // so the review stays blind until the owner opens the key deliberately.
    let mut csv = String::from("recipe,file,left_engine,right_engine\n");
    for r in records {
        let swap = blind_swap(&format!("{}:{}", r.stem, r.recipe));
        let (l, rr) = if swap { ("ART", "iAi") } else { ("iAi", "ART") };
        writeln!(
            csv,
            "{},{},{},{}",
            r.recipe,
            r.file.replace(',', ";"),
            l,
            rr
        )
        .unwrap();
    }
    std::fs::write(out.join("blind_key.csv"), &csv).expect("write blind key");
}

fn write_html(out: &Path, records: &[PairRecord], recipes: &[&Recipe]) {
    // Labeled index: grouped by recipe, iAi always left / ART always right.
    let mut html = String::from(
        "<!doctype html><meta charset=utf-8><title>Q1 ART A/B (labeled)</title>\
         <style>body{background:#111;color:#ddd;font:14px system-ui;margin:24px}\
         h2{margin-top:32px;border-bottom:1px solid #333;padding-bottom:4px}\
         .pair{margin:14px 0}img{max-width:100%;image-rendering:auto;border:1px solid #222}\
         .cap{color:#9c9;margin:4px 0}.k{color:#888}.hdr{position:sticky;top:0;background:#111;padding:8px 0}\
         </style><div class=hdr><b>Left = iAi (Develop3) &nbsp;·&nbsp; Right = ART 1.26.7</b> \
         — black-box oracle; nothing copied from ART.</div>",
    );
    for recipe in recipes {
        let group: Vec<&PairRecord> = records.iter().filter(|r| r.recipe == recipe.id).collect();
        if group.is_empty() {
            continue;
        }
        writeln!(
            html,
            "<h2>{} <span class=k>[{}] &nbsp; iAi: {} &nbsp;|&nbsp; ART: {}</span></h2>",
            recipe.id, recipe.family, recipe.iai_desc, recipe.art_desc
        )
        .unwrap();
        for r in group {
            writeln!(
                html,
                "<div class=pair><div class=cap>{} <span class=k>· MAD sh/mid/hi = {:.1}/{:.1}/{:.1} · hueΔ {:.1}° · clip iAi {:.1}% ART {:.1}%</span></div>\
                 <img loading=lazy src=\"{}\"></div>",
                r.file,
                r.cross.mad_shadow,
                r.cross.mad_midtone,
                r.cross.mad_highlight,
                r.cross.hue_drift_deg,
                r.iai_summary.clipped_pixel_fraction * 100.0,
                r.art_summary.clipped_pixel_fraction * 100.0,
                r.pair_png,
            )
            .unwrap();
        }
    }
    std::fs::write(out.join("index.html"), &html).expect("write index.html");

    // Blind index: engines hidden, side order randomized; no key linked.
    let mut blind = String::from(
        "<!doctype html><meta charset=utf-8><title>Q1 ART A/B (blind)</title>\
         <style>body{background:#111;color:#ddd;font:14px system-ui;margin:24px}\
         h2{margin-top:32px;border-bottom:1px solid #333;padding-bottom:4px}\
         .pair{margin:14px 0}img{max-width:100%;border:1px solid #222}.cap{color:#9c9}\
         </style><div><b>Blind A/B</b> — which side looks better? Left = <b>A</b>, Right = <b>B</b>. \
         Engines hidden. Record A/B per image, then check blind_key.csv.</div>",
    );
    for recipe in recipes {
        let group: Vec<&PairRecord> = records.iter().filter(|r| r.recipe == recipe.id).collect();
        if group.is_empty() {
            continue;
        }
        writeln!(blind, "<h2>{}</h2>", recipe.id).unwrap();
        for r in group {
            writeln!(
                blind,
                "<div class=pair><div class=cap>{}</div><img loading=lazy src=\"blind/{}/{}.png\"></div>",
                r.file, r.recipe, r.stem
            )
            .unwrap();
        }
    }
    std::fs::write(out.join("blind.html"), &blind).expect("write blind.html");
}

//! Phase 3 (Light) tuning aid — measure the tonal *reach* of each Light zone in
//! ART vs iAi, so iAi's tone-equalizer zone widths can be matched to ART's
//! behaviour (owner request: "see how ART does it and learn from it").
//!
//! ART is a black-box oracle: this only observes its input→output mapping and
//! copies no code, constant, LUT or profile. For one RAW it renders a neutral
//! baseline and each single Light zone pushed, then bins the |luma change| by the
//! neutral pixel's luma. The bin where the change peaks is the zone centre; the
//! luma span where it stays above a fraction of the peak is the zone's reach.
//! Both engines are measured the same way so the reaches are directly comparable.
//!
//! ```text
//! $env:IAI_RAW_CORPUS='C:\Users\Admin\Pictures\anh-raw'
//! $env:IAI_ART_CLI='C:\Users\Admin\Pictures\1111\ART_1.26.7_Win64_portable'
//! $env:IAI_TONEEQ_FILE='_DLL6009'   # optional name substring; else first RAW
//! cargo test --release --test art_tone_eq_probe -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use iai::core::develop::{DevelopEngineVersion, DevelopSettings};
use iai::core::develop_scene::apply_scene_to_tilemap;
use iai::formats::raw::RawImporter;
use iai::formats::Importer;

const WIDTH: u32 = 900;
const BINS: usize = 24;

fn luma01(px: &[u16]) -> f32 {
    (px[0] as f32 * 0.299 + px[1] as f32 * 0.587 + px[2] as f32 * 0.114) / 65535.0
}

fn box_resample_width(px: &[u16], w: u32, h: u32, tw: u32) -> (Vec<u16>, u32, u32) {
    let sw = w.min(tw).max(1);
    let sh = (((h as u64) * (sw as u64) + (w as u64) / 2) / (w as u64)).max(1) as u32;
    let mut out = vec![0u16; sw as usize * sh as usize * 4];
    for oy in 0..sh {
        let y0 = (u64::from(oy) * u64::from(h) / u64::from(sh)) as u32;
        let y1 = ((u64::from(oy + 1) * u64::from(h)) / u64::from(sh)).max(u64::from(y0) + 1) as u32;
        for ox in 0..sw {
            let x0 = (u64::from(ox) * u64::from(w) / u64::from(sw)) as u32;
            let x1 =
                ((u64::from(ox + 1) * u64::from(w)) / u64::from(sw)).max(u64::from(x0) + 1) as u32;
            let (mut acc, mut n) = ([0u64; 4], 0u64);
            for y in y0..y1.min(h) {
                for x in x0..x1.min(w) {
                    let i = (y as usize * w as usize + x as usize) * 4;
                    (0..4).for_each(|c| acc[c] += px[i + c] as u64);
                    n += 1;
                }
            }
            let o = (oy as usize * sw as usize + ox as usize) * 4;
            (0..4).for_each(|c| out[o + c] = (acc[c] / n.max(1)) as u16);
        }
    }
    (out, sw, sh)
}

/// Bin mean |Δluma| by the neutral pixel's luma; return (per-bin influence,
/// peak-bin centre luma, low/high luma where influence ≥ 0.35·peak).
fn influence_curve(neutral: &[u16], pushed: &[u16]) -> (Vec<f32>, f32, f32, f32) {
    let mut acc = vec![0f64; BINS];
    let mut cnt = vec![0f64; BINS];
    for (n, p) in neutral.chunks_exact(4).zip(pushed.chunks_exact(4)) {
        let ln = luma01(n);
        let b = ((ln * BINS as f32) as usize).min(BINS - 1);
        acc[b] += (luma01(p) - ln).abs() as f64;
        cnt[b] += 1.0;
    }
    let curve: Vec<f32> = acc
        .iter()
        .zip(&cnt)
        .map(|(a, c)| (a / c.max(1.0)) as f32)
        .collect();
    let peak = curve.iter().cloned().fold(0.0f32, f32::max).max(1e-9);
    let peak_bin = curve.iter().position(|&v| v == peak).unwrap_or(0);
    let thr = peak * 0.35;
    let low = curve.iter().position(|&v| v >= thr).unwrap_or(0);
    let high = curve.iter().rposition(|&v| v >= thr).unwrap_or(BINS - 1);
    let to_luma = |b: usize| (b as f32 + 0.5) / BINS as f32;
    (curve, to_luma(peak_bin), to_luma(low), to_luma(high))
}

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

fn art_render(art: &(PathBuf, PathBuf), raw: &Path, arps: &[&Path], out_png: &Path) -> bool {
    let (exe, dir) = art;
    let mut cmd = Command::new(exe);
    cmd.current_dir(dir)
        .arg("-Y")
        .arg("-o")
        .arg(out_png)
        .arg("-n")
        .arg("-b16")
        .arg("-q");
    for a in arps {
        cmd.arg("-p").arg(a);
    }
    cmd.arg("-c").arg(raw);
    let _ = cmd.output();
    out_png.is_file()
}

fn decode(path: &Path) -> Option<(Vec<u16>, u32, u32)> {
    let img = image::ImageReader::open(path).ok()?.decode().ok()?;
    let rgba = img.to_rgba16();
    let (w, h) = rgba.dimensions();
    Some((rgba.into_raw(), w, h))
}

#[test]
#[ignore = "requires IAI_RAW_CORPUS + IAI_ART_CLI; drives ART-cli"]
fn art_tone_eq_reach() {
    let Some(corpus) = std::env::var("IAI_RAW_CORPUS").ok().map(PathBuf::from) else {
        eprintln!("IAI_RAW_CORPUS unset; skipping.");
        return;
    };
    let Some(art) = art_cli() else {
        eprintln!("IAI_ART_CLI unset; skipping.");
        return;
    };
    let want = std::env::var("IAI_TONEEQ_FILE").unwrap_or_default();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&corpus)
        .expect("corpus readable")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && RawImporter.can_import(p))
        .collect();
    files.sort();
    let raw = files
        .into_iter()
        .find(|p| {
            want.is_empty()
                || p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.contains(&want))
        })
        .expect("a RAW to probe");
    println!("\nTone-eq reach probe on {}\n", raw.display());

    let tmp = std::env::temp_dir().join("iai-toneeq");
    std::fs::create_dir_all(&tmp).unwrap();

    // Fast iterate: skip the (slow) ART renders and print the reaches measured
    // earlier on _DLL6009 as the tuning target, so iAi zones can be dialled in.
    if std::env::var("IAI_TONEEQ_SKIP_ART").is_ok() {
        println!("ART reference reach (measured on _DLL6009, band +100):");
        println!("  Blacks      reach luma 0.06..0.15  peak@0.06");
        println!("  Shadows     reach luma 0.02..0.44  peak@0.19");
        println!("  Midtones    reach luma 0.15..0.73  peak@0.40");
        println!("  Highlights  reach luma 0.35..0.85  peak@0.60");
        println!("  Whites      reach luma 0.52..0.90  peak@0.69");
    } else {
        run_art(&art, &raw, &tmp);
    }

    measure_iai(&raw);
    println!("\nMatch iAi zone WIDTHS/centres so its reach tracks ART's, per owner feedback.");
}

fn run_art(art: &(PathBuf, PathBuf), raw: &Path, tmp: &Path) {
    let base = tmp.join("base.arp");
    std::fs::write(
        &base,
        "[Version]\nAppVersion=1.26.7\nVersion=1045\n\
         [White Balance]\nEnabled=true\nSetting=Camera\n\
         [Color Management]\nInputProfile=(cameraICC)\nWorkingProfile=Rec2020\n\
         OutputProfile=RTv2_sRGB\nOutputProfileIntent=Relative\nOutputBPC=true\n\
         [ToneCurve]\nEnabled=true\nHistogramMatching=true\nCurveFromHistogramMatching=true\n\
         [Resize]\nEnabled=true\nAppliesTo=Cropped area\nDataSpecified=3\nWidth=900\nHeight=900\n",
    )
    .unwrap();
    let art_neutral_png = tmp.join("art_neutral.png");
    assert!(
        art_render(art, &raw, &[&base], &art_neutral_png),
        "ART neutral render failed"
    );
    let (art_neutral_raw, aw, ah) = decode(&art_neutral_png).expect("decode ART neutral");
    let (art_neutral, sw, sh) = box_resample_width(&art_neutral_raw, aw, ah, WIDTH);

    let zones = ["Blacks", "Shadows", "Midtones", "Highlights", "Whites"];
    println!("ART (band +100):");
    for (band, name) in zones.iter().enumerate() {
        let arp = tmp.join(format!("art_band{band}.arp"));
        std::fs::write(
            &arp,
            format!("[ToneEqualizer]\nEnabled=true\nBand{band}=100\nRegularization=4\n"),
        )
        .unwrap();
        let png = tmp.join(format!("art_band{band}.png"));
        if !art_render(art, &raw, &[&base, &arp], &png) {
            println!("  {name:<11} ART render failed");
            continue;
        }
        let (praw, pw, ph) = decode(&png).expect("decode ART band");
        let pushed = box_resample_width(&praw, pw, ph, WIDTH).0;
        let (curve, peak, lo, hi) = influence_curve(&art_neutral, &pushed);
        println!(
            "  {name:<11} reach luma {lo:.2}..{hi:.2}  peak@{peak:.2}  {}",
            dark_profile(&curve)
        );
    }
    let _ = (sw, sh);
}

/// Normalized influence in the darkest bins (display luma ~0.02/0.06/0.10/0.15),
/// so the true-black behaviour is visible: a fill light that protects the black
/// point reads low→high; one that washes it reads high→low.
fn dark_profile(curve: &[f32]) -> String {
    let max = curve.iter().cloned().fold(0.0f32, f32::max).max(1e-9);
    format!(
        "dark[.02/.06/.10/.15]={:.2}/{:.2}/{:.2}/{:.2}",
        curve[0] / max,
        curve[1] / max,
        curve[2] / max,
        curve[3] / max,
    )
}

/// Measure iAi Develop3's per-slider reach the same way. Reads `IAI_LIGHT_SMOOTH`
/// via the render path, so setting it before this test exercises the smooth zones.
fn measure_iai(raw: &Path) {
    let canvas = RawImporter.import(raw).expect("import RAW");
    let scene = canvas.develop_source.as_ref().expect("scene");
    let base_iai = DevelopSettings {
        develop_engine_version: DevelopEngineVersion::Develop3,
        ..Default::default()
    };
    let iai_neutral_full = apply_scene_to_tilemap(scene, &base_iai, None).flatten16();
    let iai_neutral = box_resample_width(&iai_neutral_full, scene.width, scene.height, WIDTH).0;
    let setters: [(&str, fn(&mut DevelopSettings)); 5] = [
        ("Blacks", |s| s.blacks = 100.0),
        ("Shadows", |s| s.shadows = 100.0),
        ("Midtones", |s| s.midtones = 100.0),
        ("Highlights", |s| s.highlights = 100.0),
        ("Whites", |s| s.whites = 100.0),
    ];
    let smooth = std::env::var("IAI_LIGHT_SMOOTH").is_ok();
    println!("\niAi Develop3 (slider +100, smooth={smooth}):");
    for (name, set) in setters {
        let mut s = base_iai.clone();
        set(&mut s);
        let pushed_full = apply_scene_to_tilemap(scene, &s, None).flatten16();
        let pushed = box_resample_width(&pushed_full, scene.width, scene.height, WIDTH).0;
        let (curve, peak, lo, hi) = influence_curve(&iai_neutral, &pushed);
        println!(
            "  {name:<11} reach luma {lo:.2}..{hi:.2}  peak@{peak:.2}  {}",
            dark_profile(&curve)
        );
    }
}

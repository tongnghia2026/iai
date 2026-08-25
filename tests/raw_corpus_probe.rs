//! Real-RAW corpus probe for Develop Engine 2.
//!
//! The master plan (Appendix D, "Known gaps") notes that real-RAW performance
//! and camera-coverage tests were never run because no local RAW corpus was
//! available. This harness closes that gap without embedding anyone's RAW files
//! in the repository: it reads a directory from the `IAI_RAW_CORPUS` environment
//! variable, decodes each supported file through iAi's real importer, runs a
//! neutral Develop2 render, and reports coverage + timing.
//!
//! It is `#[ignore]`d so `cargo test` stays hermetic; run it explicitly with the
//! corpus path set:
//!
//! ```text
//! IAI_RAW_CORPUS="C:\\path\\to\\raws" \
//!   cargo test --release --test raw_corpus_probe -- --ignored --nocapture
//! ```
//!
//! The corpus files are treated as local, private test inputs only: they are
//! never written back, never committed, and no render is exported.

use std::path::PathBuf;
use std::time::Instant;

use iai::core::develop::DevelopSettings;
use iai::core::develop2::{self, ColorModel, InputProvenance};
use iai::core::mem_report::{mib, process_working_set, MemClass, MemReport};
use iai::formats::raw::RawImporter;
use iai::formats::Importer;

fn corpus_dir() -> Option<PathBuf> {
    let raw = std::env::var("IAI_RAW_CORPUS").ok()?;
    let path = PathBuf::from(raw);
    path.is_dir().then_some(path)
}

#[test]
#[ignore = "requires a local RAW corpus via IAI_RAW_CORPUS; slow"]
fn raw_corpus_decodes_and_renders_through_develop2() {
    let Some(dir) = corpus_dir() else {
        eprintln!(
            "IAI_RAW_CORPUS not set or not a directory; skipping real-RAW probe. \
             Set it to a folder of RAW files to run this."
        );
        return;
    };

    let importer = RawImporter;
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("corpus directory is readable")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    entries.sort();

    println!(
        "\nRAW corpus probe — {} entries in {}",
        entries.len(),
        dir.display()
    );
    println!(
        "{:<52} {:>5}  {:<9} {:>11}  {:<22} {:>10} {:>10}",
        "file", "ext", "result", "MP", "camera", "decode ms", "dev ms"
    );

    let mut decoded = 0usize;
    let mut unsupported = 0usize;
    let mut failed = 0usize;

    for path in &entries {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("<?>")
            .chars()
            .take(50)
            .collect::<String>();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        if !importer.can_import(path) {
            unsupported += 1;
            println!(
                "{name:<52} {ext:>5}  {:<9} {:>11}  {:<22} {:>10} {:>10}",
                "unsupp.", "-", "-", "-", "-"
            );
            continue;
        }

        let t0 = Instant::now();
        let canvas = match importer.import(path) {
            Ok(canvas) => canvas,
            Err(e) => {
                failed += 1;
                let msg = e.chars().take(40).collect::<String>();
                println!(
                    "{name:<52} {ext:>5}  {:<9} {:>11}  {msg:<22} {:>10} {:>10}",
                    "DECODE-ERR",
                    "-",
                    t0.elapsed().as_millis(),
                    "-"
                );
                continue;
            }
        };
        let decode_ms = t0.elapsed().as_millis();

        let scene = canvas
            .develop_source
            .as_ref()
            .expect("a RAW import must attach a Develop scene source");
        let camera: String = canvas.metadata.source_profile.chars().take(20).collect();
        let mp = (scene.width as f64 * scene.height as f64) / 1.0e6;

        // Profile-aware input boundary must be valid and describe a RAW camera
        // scene master in linear ProPhoto.
        let boundary = develop2::describe_input_boundary(scene);
        assert_eq!(
            boundary.provenance,
            InputProvenance::RawCameraMatrix,
            "{name}: RAW scene master must report camera-matrix provenance"
        );
        assert_eq!(
            boundary.source,
            ColorModel::LinearProPhoto,
            "{name}: RAW scene master must be linear ProPhoto"
        );
        assert_eq!(
            boundary.validate(),
            Ok(()),
            "{name}: input boundary must validate"
        );

        // Neutral Develop2 render must produce a full-size result.
        let t1 = Instant::now();
        let tile = develop2::execute_scene(scene, &DevelopSettings::default(), None)
            .expect("neutral Develop2 must render a RAW scene");
        let dev_ms = t1.elapsed().as_millis();
        assert_eq!(
            (tile.width, tile.height),
            (scene.width, scene.height),
            "{name}: rendered tilemap must match scene dimensions"
        );

        decoded += 1;
        println!(
            "{name:<52} {ext:>5}  {:<9} {mp:>11.1}  {camera:<22} {decode_ms:>10} {dev_ms:>10}",
            "OK"
        );
    }

    println!(
        "\nsummary: {decoded} decoded+rendered, {unsupported} unsupported ext, {failed} decode errors, {} total\n",
        entries.len()
    );

    // Harness sanity: the corpus must contain at least one file iAi can develop.
    assert!(
        decoded > 0,
        "no RAW file in {} decoded through Develop2",
        dir.display()
    );
}

/// Memory Milestone M0 baseline: open the *whole* corpus at once — exactly the
/// user action that pushes iAi to ~12 GB — while measuring both the logical
/// per-class footprint and the OS process working set at each stage
/// (decode → all-resident peak → drop). The logical figure must explain most of
/// the working set; if it does, the plan's conclusion (§3.3: the RAM is
/// by-design retention, not a leak) is demonstrated with numbers.
///
/// It holds every decoded `Canvas` resident on purpose, so it really does grow
/// to the multi-GB footprint under investigation. `#[ignore]`d and gated on the
/// corpus env var like the probe above:
///
/// ```text
/// $env:IAI_RAW_CORPUS='C:\Users\Admin\Pictures\anh-raw'
/// cargo test --release --test raw_corpus_probe raw_corpus_memory_baseline -- --ignored --nocapture
/// ```
#[test]
#[ignore = "requires a local RAW corpus via IAI_RAW_CORPUS; allocates many GB"]
fn raw_corpus_memory_baseline() {
    let Some(dir) = corpus_dir() else {
        eprintln!(
            "IAI_RAW_CORPUS not set or not a directory; skipping M0 memory baseline. \
             Set it to a folder of RAW files to run this."
        );
        return;
    };

    let importer = RawImporter;
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("corpus directory is readable")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    entries.sort();

    let baseline_ws = process_working_set();
    if let Some(m) = baseline_ws {
        println!(
            "\nM0 memory baseline — {} entries in {}\n  process working set at start: {:.1} MiB (peak {:.1} MiB)",
            entries.len(),
            dir.display(),
            mib(m.working_set),
            mib(m.peak_working_set),
        );
    } else {
        println!(
            "\nM0 memory baseline — {} entries in {}\n  (process working-set query unavailable on this platform)",
            entries.len(),
            dir.display()
        );
    }
    println!(
        "\n{:<48} {:>9} {:>12} {:>12} {:>12}",
        "file (opened, held resident)", "MP", "logical MiB", "cumL MiB", "wset MiB"
    );

    // Hold every decoded canvas resident: this is the state the plan measures.
    let mut held: Vec<iai::core::canvas::Canvas> = Vec::new();
    let mut report = MemReport::new();
    let mut total_mp = 0.0f64;
    let mut decoded = 0usize;

    for path in &entries {
        if !importer.can_import(path) {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("<?>")
            .chars()
            .take(46)
            .collect::<String>();
        let canvas = match importer.import(path) {
            Ok(c) => c,
            Err(e) => {
                println!(
                    "{name:<48}  DECODE-ERR: {}",
                    e.chars().take(40).collect::<String>()
                );
                continue;
            }
        };
        let mp = (canvas.width as f64 * canvas.height as f64) / 1.0e6;
        total_mp += mp;
        decoded += 1;

        let mut this = MemReport::new();
        canvas.account_memory(&mut this, &name);
        report.merge(&this);
        held.push(canvas);

        let wset = process_working_set()
            .map(|m| mib(m.working_set))
            .unwrap_or(0.0);
        println!(
            "{name:<48} {mp:>9.1} {:>12.1} {:>12.1} {:>12.1}",
            mib(this.total()),
            mib(report.total()),
            wset,
        );
    }

    // ── All resident: the peak the plan cares about ───────────────────────────
    let peak_ws = process_working_set();
    println!("\nAll {decoded} RAW held resident ({total_mp:.1} MP total)\n");
    print!("{}", report.format_class_table());

    println!("\nheaviest owners (document → MiB):");
    for (owner, bytes) in report.owners_by_bytes().into_iter().take(8) {
        println!(
            "  {:<46} {:>10.2}",
            owner.chars().take(46).collect::<String>(),
            mib(bytes)
        );
    }

    let logical = report.total();
    if let (Some(base), Some(peak)) = (baseline_ws, peak_ws) {
        let ws_growth = peak.working_set.saturating_sub(base.working_set);
        println!(
            "\nprocess working set now: {:.1} MiB (peak {:.1} MiB)\n  growth since start:   {:.1} MiB\n  logical accounted:    {:.1} MiB\n  logical / ws-growth:  {:.1}%",
            mib(peak.working_set),
            mib(peak.peak_working_set),
            mib(ws_growth),
            mib(logical),
            if ws_growth > 0 { logical as f64 / ws_growth as f64 * 100.0 } else { 0.0 },
        );
    }

    // Machine-readable one-liner for scripted baseline diffing.
    let extra_ws = peak_ws.map(|m| m.working_set).unwrap_or(0);
    let extra_peak = peak_ws.map(|m| m.peak_working_set).unwrap_or(0);
    println!(
        "\nMEMBASELINE_JSON: {}",
        report.to_json(&[
            ("working_set", extra_ws),
            ("peak_working_set", extra_peak),
            ("decoded", decoded as u64),
        ])
    );

    // ── Drop everything (close all tabs) ──────────────────────────────────────
    let held_count = held.len();
    drop(held);
    if let Some(after) = process_working_set() {
        println!(
            "\nafter dropping all {held_count} canvases: working set {:.1} MiB\n  (the OS may return pages lazily; a large logical drop with a slow ws drop is expected)",
            mib(after.working_set)
        );
    }

    assert!(decoded > 0, "no RAW decoded from {}", dir.display());
    // The dominant classes must be exactly the full-resolution masters the plan
    // identifies; if any is zero the accounting missed a retained buffer.
    assert!(
        report.class_bytes(MemClass::SceneHalf) > 0,
        "no scene f16 accounted"
    );
    assert!(
        report.class_bytes(MemClass::TileRgba16) > 0,
        "no tile RGBA16 accounted"
    );
    assert!(
        report.class_bytes(MemClass::TileRgba8) > 0,
        "no tile RGBA8 accounted"
    );
}

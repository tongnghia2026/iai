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

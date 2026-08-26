//! Quality Milestone Q1 — sensor-metadata audit (item #1: audit before change).
//!
//! Before building any sensor-preprocessing stage, the plan requires auditing
//! what metadata the decoders actually return for each corpus file, and *not*
//! assuming fields that are not there. This probe decodes every corpus RAW just
//! far enough to read [`iai::formats::raw::RawSensorMetadata`] — CFA/active area,
//! black and white levels with white-level provenance, WB multipliers, and how
//! many masked optical-black regions the decoder exposed — without demosaicing.
//!
//! It answers plan item #4 directly: which files' reported white level is
//! trusted vs. replaced by the observed sensor maximum (and why), so a later
//! normalization never silently brightens an underexposed frame.
//!
//! `#[ignore]`d and gated on `IAI_RAW_CORPUS`, like the sibling probes. Set
//! `IAI_Q0_OUT` to also write `q1_sensor_metadata.csv` + `.json`.
//!
//! ```text
//! $env:IAI_RAW_CORPUS='C:\Users\Admin\Pictures\anh-raw'
//! $env:IAI_Q0_OUT='C:\Users\Admin\Documents\IAI\target\q0'
//! cargo test --release --test raw_sensor_metadata_probe -- --ignored --nocapture
//! ```

use std::fmt::Write as _;
use std::path::PathBuf;

use iai::formats::raw::RawImporter;
use iai::formats::raw::{probe_sensor_metadata, RawSensorMetadata, WhiteLevelSource};
use iai::formats::Importer;

fn corpus_dir() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var("IAI_RAW_CORPUS").ok()?);
    path.is_dir().then_some(path)
}

/// Short tag for a channel's white-level provenance.
fn source_tag(source: WhiteLevelSource) -> &'static str {
    match source {
        WhiteLevelSource::Reported => "ok",
        WhiteLevelSource::MissingReplacedByObserved => "missing",
        WhiteLevelSource::ContainerMaxReplacedByObserved => "container",
    }
}

/// Whether any of the R/G/B channels did not trust the reported white level.
fn has_white_fallback(meta: &RawSensorMetadata) -> bool {
    meta.white_level_source[..3]
        .iter()
        .any(|&s| s != WhiteLevelSource::Reported)
}

#[test]
#[ignore = "requires a local RAW corpus via IAI_RAW_CORPUS"]
fn raw_sensor_metadata_audit() {
    let Some(dir) = corpus_dir() else {
        eprintln!("IAI_RAW_CORPUS not set or not a directory; skipping Q1 sensor audit.");
        return;
    };

    let importer = RawImporter;
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("corpus directory is readable")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    entries.sort();

    println!("\nQuality Q1 sensor-metadata audit — {}\n", dir.display());

    let mut metas: Vec<(String, RawSensorMetadata)> = Vec::new();
    let mut failed = 0usize;
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
        match probe_sensor_metadata(path) {
            Ok(meta) => {
                println!(
                    "  {:<40} {:<9} {:<20} {}x{} cpp{} cfa={}({}) active={:?} black={:?} white_eff={:?} src=[{},{},{},{}] blackareas={}",
                    name.chars().take(40).collect::<String>(),
                    format!("{:?}", meta.backend),
                    format!("{} {}", meta.make, meta.model).chars().take(20).collect::<String>(),
                    meta.width,
                    meta.height,
                    meta.cpp,
                    meta.cfa_name,
                    if meta.is_mono { "mono" } else if meta.cfa_valid { "bayer" } else { "?" },
                    meta.active_area,
                    meta.black_levels.map(|v| v as i32),
                    meta.effective_white_levels.map(|v| v as i32),
                    source_tag(meta.white_level_source[0]),
                    source_tag(meta.white_level_source[1]),
                    source_tag(meta.white_level_source[2]),
                    source_tag(meta.white_level_source[3]),
                    meta.black_area_count,
                );
                metas.push((name, meta));
            }
            Err(error) => {
                failed += 1;
                println!(
                    "  PROBE-ERR {name}: {}",
                    error.chars().take(60).collect::<String>()
                );
            }
        }
    }

    assert!(
        !metas.is_empty(),
        "no RAW metadata read from {}",
        dir.display()
    );

    // ── Rollup: the facts later Q1 stages depend on ───────────────────────────
    let white_fallback = metas.iter().filter(|(_, m)| has_white_fallback(m)).count();
    let with_black_areas = metas.iter().filter(|(_, m)| m.black_area_count > 0).count();
    let mono = metas.iter().filter(|(_, m)| m.is_mono).count();
    let bayer = metas
        .iter()
        .filter(|(_, m)| m.cfa_valid && !m.is_mono)
        .count();
    let rawler = metas
        .iter()
        .filter(|(_, m)| format!("{:?}", m.backend) == "Rawler")
        .count();
    println!(
        "\nsummary: {} files, {rawler} via rawler fallback, {bayer} bayer, {mono} mono, \
         {white_fallback} with reported-white-level fallback, {with_black_areas} expose masked black areas, {failed} probe errors",
        metas.len(),
    );
    // Distinct CFA patterns present — X-Trans vs Bayer coverage for Q2.
    let mut patterns: Vec<String> = metas.iter().map(|(_, m)| m.cfa_name.clone()).collect();
    patterns.sort();
    patterns.dedup();
    println!("CFA patterns: {patterns:?}");
    if white_fallback > 0 {
        println!("reported-white-level NOT trusted (auto-normalized to observed maximum):");
        for (name, meta) in metas.iter().filter(|(_, m)| has_white_fallback(m)) {
            println!(
                "  {:<40} reported={:?} observed={:?} src=[{},{},{}]",
                name.chars().take(40).collect::<String>(),
                meta.reported_white_levels.map(|v| v as i32),
                meta.observed_white_maxima.map(|v| v as i32),
                source_tag(meta.white_level_source[0]),
                source_tag(meta.white_level_source[1]),
                source_tag(meta.white_level_source[2]),
            );
        }
    }

    // ── Machine-readable artifacts ────────────────────────────────────────────
    if let Some(out) = std::env::var_os("IAI_Q0_OUT").map(PathBuf::from) {
        std::fs::create_dir_all(&out).expect("create Q0 output directory");
        let mut csv = String::from(
            "file,backend,make,model,width,height,cpp,cfa,mono,active_top,active_left,active_w,active_h,black_r,black_g,black_b,white_eff_r,white_eff_g,white_eff_b,src_r,src_g,src_b,wb_r,wb_g,wb_b,black_areas\n",
        );
        for (name, m) in &metas {
            writeln!(
                csv,
                "{},{:?},{},{},{},{},{},{},{},{},{},{},{},{:.0},{:.0},{:.0},{:.0},{:.0},{:.0},{},{},{},{:.4},{:.4},{:.4},{}",
                name.replace(',', ";"),
                m.backend,
                m.make.replace(',', ";"),
                m.model.replace(',', ";"),
                m.width, m.height, m.cpp, m.cfa_name, m.is_mono,
                m.active_area[0], m.active_area[1], m.active_area[2], m.active_area[3],
                m.black_levels[0], m.black_levels[1], m.black_levels[2],
                m.effective_white_levels[0], m.effective_white_levels[1], m.effective_white_levels[2],
                source_tag(m.white_level_source[0]), source_tag(m.white_level_source[1]), source_tag(m.white_level_source[2]),
                m.wb_coeffs[0], m.wb_coeffs[1], m.wb_coeffs[2],
                m.black_area_count,
            )
            .unwrap();
        }
        std::fs::write(out.join("q1_sensor_metadata.csv"), &csv).expect("write Q1 sensor CSV");

        let json = serde_json::json!({
            "corpus": dir.display().to_string(),
            "files": metas.len(),
            "rawler_fallback": rawler,
            "bayer": bayer,
            "mono": mono,
            "white_level_fallback": white_fallback,
            "with_masked_black_areas": with_black_areas,
            "cfa_patterns": patterns,
            "records": metas.iter().map(|(name, m)| serde_json::json!({
                "file": name,
                "backend": format!("{:?}", m.backend),
                "make": m.make,
                "model": m.model,
                "width": m.width,
                "height": m.height,
                "cpp": m.cpp,
                "cfa": m.cfa_name,
                "cfa_valid": m.cfa_valid,
                "is_mono": m.is_mono,
                "active_area": m.active_area,
                "crop_margins": m.crop_margins,
                "black_levels": m.black_levels,
                "reported_white_levels": m.reported_white_levels,
                "observed_white_maxima": m.observed_white_maxima,
                "effective_white_levels": m.effective_white_levels,
                "white_level_source": m.white_level_source.iter().map(|&s| source_tag(s)).collect::<Vec<_>>(),
                "wb_coeffs": m.wb_coeffs,
                "black_area_count": m.black_area_count,
                "orientation": m.orientation,
            })).collect::<Vec<_>>(),
        });
        std::fs::write(
            out.join("q1_sensor_metadata.json"),
            serde_json::to_vec_pretty(&json).unwrap(),
        )
        .expect("write Q1 sensor JSON");
        println!("\nwrote {}", out.join("q1_sensor_metadata.csv").display());
    }
}

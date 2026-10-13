//! Detail A/B probe against Camera Raw: renders a 16-bit RGBA chart through the
//! production Develop commit path for every job in a Camera Raw job list and
//! writes the results for external comparison.
//!
//! IAI_DETAIL_PROBE_DIR=<dir> IAI_DETAIL_PROBE_CHART=chart2 \
//! IAI_DETAIL_PROBE_JOBS=<jobs.json> IAI_DETAIL_PROBE_OUT=iai2 \
//!   cargo test --release --test detail_pts_probe -- --ignored --nocapture

use iai::core::develop::DevelopSettings;
use iai::core::develop_scene::{apply_scene_to_tilemap, SceneSource};
use iai::core::tile::TileMap;

#[test]
#[ignore]
fn detail_pts_probe() {
    let Ok(dir) = std::env::var("IAI_DETAIL_PROBE_DIR") else {
        eprintln!("IAI_DETAIL_PROBE_DIR not set; skipping");
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let chart = std::env::var("IAI_DETAIL_PROBE_CHART").unwrap_or_else(|_| "chart".into());
    let (w, h) = if chart == "chart2" {
        (1536, 896)
    } else {
        (1536, 1024)
    };
    let bytes = std::fs::read(dir.join(format!("{chart}_rgba16.bin"))).expect("chart bin");
    let px16: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    assert_eq!(px16.len(), (w * h * 4) as usize);
    let tiles = TileMap::from_rgba16(&px16, w, h);
    let scene = SceneSource::from_display_tiles(&tiles);

    let jobs_path = std::env::var("IAI_DETAIL_PROBE_JOBS").expect("IAI_DETAIL_PROBE_JOBS");
    let jobs: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(jobs_path).unwrap()).unwrap();
    let out_dir = dir.join(std::env::var("IAI_DETAIL_PROBE_OUT").unwrap_or_else(|_| "iai".into()));
    std::fs::create_dir_all(&out_dir).unwrap();
    for job in jobs {
        let f = |k: &str| job[k].as_f64().unwrap() as f32;
        let settings = DevelopSettings {
            sharpening: f("shrp"),
            sharpen_radius: f("shpr"),
            sharpen_detail: f("shpd"),
            sharpen_masking: f("shpm"),
            noise_reduction: f("lnr"),
            noise_reduction_detail: f("lnrd"),
            noise_reduction_contrast: f("lnrc"),
            color_noise_reduction: f("cnr"),
            color_noise_detail: f("cnrd"),
            color_noise_smoothness: f("cnrs"),
            ..Default::default()
        };
        let name = job["name"].as_str().unwrap();
        let t0 = std::time::Instant::now();
        let out = apply_scene_to_tilemap(&scene, &settings, None).flatten16();
        let bytes: Vec<u8> = out.iter().flat_map(|v| v.to_le_bytes()).collect();
        std::fs::write(out_dir.join(format!("{name}.bin")), bytes).unwrap();
        println!("{name}: {:.0} ms", t0.elapsed().as_secs_f64() * 1000.0);
    }
}

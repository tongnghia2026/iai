//! Manual perf probe for the Develop live-preview hot path (scene sessions).
//!
//! Times each CPU stage `build_develop_gpu_preview` can run in a frame, on a
//! real RAW file, at two viewport scenarios (fit view / 100% zoom crop) — so
//! perf work targets the stage that actually dominates instead of a guess.
//!
//! Not part of CI (`#[ignore]`, needs a local RAW + release codegen). Run:
//!
//! ```text
//! cargo test --release perf_develop -- --ignored --nocapture
//! set IAI_PERF_RAW=D:\path\to\file.cr2   (optional override)
//! ```

use iai::core::develop::{self, fast_preview_downsample, DevelopSettings, TONE_DOWNSAMPLE};
use iai::core::develop_scene::{self, SceneSource};
use iai::formats::{raw::RawImporter, Importer};
use std::time::Instant;

/// Median-ish timing: 1 warmup + `iters` runs, report best and mean.
fn time<T>(label: &str, iters: usize, mut f: impl FnMut() -> T) -> T {
    let mut out = f(); // warmup (page-in, rayon pool spin-up)
    let mut best = f64::INFINITY;
    let mut total = 0.0;
    for _ in 0..iters {
        let t = Instant::now();
        out = f();
        let ms = t.elapsed().as_secs_f64() * 1e3;
        best = best.min(ms);
        total += ms;
    }
    eprintln!(
        "{label:<44} best {best:8.2} ms   mean {:8.2} ms",
        total / iters as f64
    );
    out
}

fn perf_raw_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("IAI_PERF_RAW") {
        return Some(p.into());
    }
    let default = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("anh-mau/IMG_5344.CR2");
    default.exists().then_some(default)
}

/// Settings shaped like the user's complaint: several groups engaged at once
/// (WB + Exposure + tone-eq zones + colour mixer), so every per-frame stage
/// of the preview is live.
fn multi_group_settings() -> DevelopSettings {
    let mut s = DevelopSettings {
        temperature: 12.0,
        exposure: -20.0,
        contrast: 25.0,
        highlights: -40.0,
        shadows: 55.0,
        ..DevelopSettings::default()
    };
    s.mixer_saturation[1] = 30.0;
    s.mixer_hue[4] = -20.0;
    s
}

fn probe_viewport(
    scene: &SceneSource,
    settings: &DevelopSettings,
    label: &str,
    ox: u32,
    oy: u32,
    rw: u32,
    rh: u32,
) {
    let s = fast_preview_downsample(rw, rh);
    eprintln!("\n-- {label}: region {rw}x{rh} @({ox},{oy}), downsample {s} --");
    let tone = develop_scene::build_scene_tone(settings);
    let (base, pw, ph) = time("  build_scene_color_base_box  (viewport chg)", 3, || {
        develop_scene::build_scene_color_base_box(scene, ox, oy, rw, rh, s)
    });
    eprintln!("     proxy {pw}x{ph} = {} texels", pw * ph);
    let region = time("  tone_lowpass_scene_region   (per frame)", 5, || {
        develop_scene::tone_lowpass_scene_region(&base, pw, ph, &tone, s)
    });
    time("  apply_color_to_region       (per frame)", 5, || {
        develop::apply_color_to_region(&region, settings, pw, ph)
    });
}

/// Times every stage `begin_develop_preview` runs on the MAIN thread when a
/// Develop session opens (D0/A3): tiles clone, scene linearization (identity
/// sessions), histogram proxy build, first histogram re-bin.
#[test]
#[ignore = "manual perf probe; needs a local RAW and --release"]
fn perf_begin_develop() {
    let Some(path) = perf_raw_path() else {
        eprintln!("perf_begin_develop: no RAW found (set IAI_PERF_RAW) - skipping");
        return;
    };
    eprintln!("RAW: {}", path.display());

    let t = Instant::now();
    let canvas = RawImporter.import(&path).expect("RAW decode failed");
    eprintln!(
        "decode+default-look {:>7.0} ms   ({}x{})",
        t.elapsed().as_secs_f64() * 1e3,
        canvas.width,
        canvas.height
    );
    let scene = canvas.develop_source.clone().expect("no scene master");
    let tiles = &canvas.layer_stack.layers[0].tiles;
    eprintln!("tiles: {} entries", tiles.tiles.len());

    eprintln!("\n-- RAW session (scene = Arc clone) --");
    time("  TileMap.clone (shallow Arc)", 5, || tiles.clone());
    let hist_proxy = time("  build_scene_histogram_proxy", 3, || {
        develop_scene::build_scene_histogram_proxy(&scene)
    });
    time("  histogram_rgbl_scene (first re-bin)", 5, || {
        develop_scene::histogram_rgbl_scene(
            &hist_proxy,
            &DevelopSettings::default(),
            develop_scene::BaseLook::Raw,
        )
    });

    eprintln!("\n-- Identity session (JPEG/PNG Develop on same-size tiles) --");
    let id_scene = time("  SceneSource::from_display_tiles", 3, || {
        SceneSource::from_display_tiles(tiles)
    });
    time("  build_scene_histogram_proxy (identity)", 3, || {
        develop_scene::build_scene_histogram_proxy(&id_scene)
    });

    eprintln!("\n-- Legacy session (display-domain fallback) --");
    time("  build_histogram_proxy (legacy)", 3, || {
        develop::build_histogram_proxy(tiles)
    });
}

#[test]
#[ignore = "manual perf probe; needs a local RAW and --release"]
fn perf_develop_stages() {
    let Some(path) = perf_raw_path() else {
        eprintln!("perf_develop_stages: no RAW found (set IAI_PERF_RAW) - skipping");
        return;
    };
    eprintln!("RAW: {}", path.display());

    let t = Instant::now();
    let canvas = RawImporter.import(&path).expect("RAW decode failed");
    eprintln!(
        "decode+default-look {:>7.0} ms   ({}x{})",
        t.elapsed().as_secs_f64() * 1e3,
        canvas.width,
        canvas.height
    );
    let scene = canvas.develop_source.clone().expect("no scene master");
    let settings = multi_group_settings();

    eprintln!("\n-- per-session (once per Develop open) --");
    let hist_proxy = time("  build_scene_histogram_proxy", 3, || {
        develop_scene::build_scene_histogram_proxy(&scene)
    });
    let (rbase, rpw, rph) = time("  build_scene_region_base", 3, || {
        develop_scene::build_scene_region_base(&scene, TONE_DOWNSAMPLE)
    });
    eprintln!("     E-plane {rpw}x{rph} = {} texels", rpw * rph);

    eprintln!("\n-- per-frame (every slider tick) --");
    let tone = time("  build_scene_tone            (x2: hist+prev)", 20, || {
        develop_scene::build_scene_tone(&settings)
    });
    time("  histogram_rgbl_scene        (per tick)", 5, || {
        develop_scene::histogram_rgbl_scene(&hist_proxy, &settings, develop_scene::BaseLook::Raw)
    });
    time("  finish_region_e             (EV/WB drag)", 5, || {
        develop_scene::finish_region_e(&rbase, rpw, rph, &tone, TONE_DOWNSAMPLE)
    });

    // Fit view: the viewport shows the whole image -> the colour proxy box IS
    // the full source. 100% zoom: a screen-sized crop in the middle.
    probe_viewport(
        &scene,
        &settings,
        "fit view",
        0,
        0,
        canvas.width,
        canvas.height,
    );
    let (vw, vh) = (2400.min(canvas.width), 1300.min(canvas.height));
    probe_viewport(
        &scene,
        &settings,
        "100% zoom",
        (canvas.width - vw) / 2,
        (canvas.height - vh) / 2,
        vw,
        vh,
    );
}

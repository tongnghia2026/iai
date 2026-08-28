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
use iai::core::layer::{Layer, LayerStack};
use iai::formats::{raw::RawImporter, Importer};
use iai::gpu::compositor::{CompositorState, DevelopGpuPreview};
use std::sync::Arc;
use std::time::Instant;

const EXPENSIVE_SAMPLES: usize = 15;
const FRAME_SAMPLES: usize = 30;

/// One warmup plus `iters` measured runs; report best, mean, and p95.
fn time<T>(label: &str, iters: usize, mut f: impl FnMut() -> T) -> T {
    let mut out = f(); // warmup (page-in, rayon pool spin-up)
    let mut best = f64::INFINITY;
    let mut total = 0.0;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        out = f();
        let ms = t.elapsed().as_secs_f64() * 1e3;
        best = best.min(ms);
        total += ms;
        samples.push(ms);
    }
    samples.sort_by(f64::total_cmp);
    let p95_index = ((samples.len() as f64 * 0.95).ceil() as usize)
        .saturating_sub(1)
        .min(samples.len() - 1);
    eprintln!(
        "{label:<44} best {best:8.2} ms   mean {:8.2} ms   p95 {:8.2} ms",
        total / iters as f64,
        samples[p95_index],
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

/// Hardware-local Phase-6 frame probe. A 640×360 scene is magnified 3× into a
/// 1920×1080 target, so every measured frame executes the full Develop scene
/// shader over 2.07 M fragments without requiring a large RAW fixture merely
/// to generate the same fit-view fragment load.
#[test]
#[ignore = "manual GPU p95 probe; hardware dependent"]
fn perf_headless_develop_slider_frames() {
    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("perf_headless_develop_slider_frames: no GPU adapter - skipping");
        return;
    };
    let (source_w, source_h) = (640, 360);
    let (viewport_w, viewport_h) = (1920, 1080);
    let mut scene = SceneSource::new(source_w, source_h);
    for y in 0..source_h {
        for x in 0..source_w {
            let fx = x as f32 / (source_w - 1) as f32;
            let fy = y as f32 / (source_h - 1) as f32;
            let input = [fx * 1.35, fy * 0.95, (1.0 - fx) * (0.45 + fy)];
            scene.set_rgb(x, y, scene.color_pipeline.working.from_linear_srgb(input));
        }
    }
    let neutral = develop_scene::render_default_look(&scene);
    let neutral8 = neutral.iter().map(|v| (v >> 8) as u8).collect();
    let mut stack = LayerStack::new(source_w, source_h);
    stack.layers[0] = Layer::from_rgba(0, "Background", neutral8, source_w, source_h);
    let scene = Arc::new(scene);
    let max_texture = device.limits().max_texture_dimension_2d;
    let mut compositor = CompositorState::new(&device, viewport_w, viewport_h, max_texture);

    let mut samples = Vec::with_capacity(FRAME_SAMPLES);
    for frame in 0..=FRAME_SAMPLES {
        let mut settings = multi_group_settings();
        settings.saturation = 18.0 + (frame % 11) as f32;
        settings.mixer_saturation[0] = 12.0 + (frame % 7) as f32;
        compositor.develop_preview = Some(DevelopGpuPreview {
            layer_id: 0,
            settings,
            region_luma: None,
            color: None,
            scene: Some(scene.clone()),
        });
        let started = Instant::now();
        compositor.composite_layers(&device, &queue, &stack, 0.0, 0.0, 3.0, None, false, false);
        device.poll(wgpu::PollType::wait_indefinitely()).ok();
        if frame > 0 {
            samples.push(started.elapsed().as_secs_f64() * 1e3);
        }
    }
    samples.sort_by(f64::total_cmp);
    let p95 = samples[((samples.len() as f64 * 0.95).ceil() as usize)
        .saturating_sub(1)
        .min(samples.len() - 1)];
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    eprintln!(
        "headless Develop 1920x1080: best {:.2} ms mean {mean:.2} ms p95 {p95:.2} ms",
        samples[0]
    );
    assert!(p95 < 50.0, "Develop slider-to-frame p95 {p95:.2} ms");
}

/// End-to-end cost of the native-resolution Detail path added for zoomed-in
/// WYSIWYG preview. Includes host upload, tiled compute and readback because all
/// three currently sit on the interactive frame boundary.
#[test]
#[ignore = "manual native GPU Detail p95 probe; hardware dependent"]
fn perf_headless_gpu_detail_native_frames() {
    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("perf_headless_gpu_detail_native_frames: no GPU adapter - skipping");
        return;
    };
    let (w, h) = (1920u32, 1080u32);
    let mut rgb = vec![0.0f32; (3 * w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            let fx = x as f32 / (w - 1) as f32;
            let fy = y as f32 / (h - 1) as f32;
            let texture = 0.018 * (x as f32 * 0.31).sin() + 0.012 * (y as f32 * 0.27).cos();
            rgb[3 * i] = (0.12 + 0.72 * fx + texture).max(0.0);
            rgb[3 * i + 1] = (0.10 + 0.61 * fy - texture * 0.4).max(0.0);
            rgb[3 * i + 2] = (0.08 + 0.48 * (1.0 - fx) + texture * 0.7).max(0.0);
        }
    }
    let params =
        iai::gpu::detail_gpu::DetailWorkingParams::from_sliders(60.0, 1.0, 25.0, 0.0, 25.0, 35.0);
    let runtime = iai::gpu::detail_gpu::DetailGpuRuntime::new(&device);
    let run = || {
        iai::gpu::detail_gpu::run_detail_tiled_with_runtime(
            &runtime,
            &device,
            &queue,
            &rgb,
            w,
            h,
            params,
            true,
            [0.272_229, 0.674_082, 0.053_689],
        )
    };
    std::hint::black_box(run());
    let mut samples = Vec::with_capacity(10);
    for _ in 0..10 {
        let started = Instant::now();
        std::hint::black_box(run());
        samples.push(started.elapsed().as_secs_f64() * 1e3);
    }
    samples.sort_by(f64::total_cmp);
    let p95 = samples[((samples.len() as f64 * 0.95).ceil() as usize)
        .saturating_sub(1)
        .min(samples.len() - 1)];
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    eprintln!(
        "native GPU Detail 1920x1080: best {:.2} ms mean {mean:.2} ms p95 {p95:.2} ms",
        samples[0]
    );
    assert!(p95 < 100.0, "native GPU Detail p95 {p95:.2} ms");
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
    let (base, pw, ph) = time(
        "  build_scene_color_base_box  (viewport chg)",
        EXPENSIVE_SAMPLES,
        || develop_scene::build_scene_color_base_box(scene, ox, oy, rw, rh, s),
    );
    eprintln!("     proxy {pw}x{ph} = {} texels", pw * ph);
    let region = time(
        "  tone_lowpass_scene_region   (per frame)",
        FRAME_SAMPLES,
        || develop_scene::tone_lowpass_scene_region(&base, pw, ph, &tone, s),
    );
    time(
        "  apply_color_to_region       (per frame)",
        FRAME_SAMPLES,
        || develop::apply_color_to_region(&region, settings, pw, ph),
    );
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
    let hist_proxy = time("  build_scene_histogram_proxy", EXPENSIVE_SAMPLES, || {
        develop_scene::build_scene_histogram_proxy(&scene)
    });
    let (rbase, rpw, rph) = time("  build_scene_region_base", EXPENSIVE_SAMPLES, || {
        develop_scene::build_scene_region_base(&scene, TONE_DOWNSAMPLE)
    });
    eprintln!("     E-plane {rpw}x{rph} = {} texels", rpw * rph);

    eprintln!("\n-- per-frame (every slider tick) --");
    let tone = time("  build_scene_tone            (x2: hist+prev)", 100, || {
        develop_scene::build_scene_tone(&settings)
    });
    time(
        "  histogram_rgbl_scene        (per tick)",
        FRAME_SAMPLES,
        || {
            develop_scene::histogram_rgbl_scene(
                &hist_proxy,
                &settings,
                develop_scene::BaseLook::Raw,
            )
        },
    );
    time(
        "  finish_region_e             (EV/WB drag)",
        FRAME_SAMPLES,
        || develop_scene::finish_region_e(&rbase, rpw, rph, &tone, TONE_DOWNSAMPLE),
    );

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

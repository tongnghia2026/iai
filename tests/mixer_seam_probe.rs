//! Colour preview/commit probe: runs a display-referred photo through the
//! production Develop commit and the headless GPU preview (proxies built like
//! `develop_preview.rs`) and reports discontinuities of the edit along the
//! 256 px tile grid plus the preview/commit difference.
//!
//! IAI_MIXER_PROBE_IMAGE=<photo> IAI_MIXER_PROBE_OUT=<dir> \
//!   cargo test --release --test mixer_seam_probe -- --ignored --nocapture
//!
//! Optional: IAI_MIXER_PROBE_{HUE,SAT,LUM}=<8 comma values>,
//! IAI_MIXER_PROBE_SATURATION / _VIBRANCE / _CLARITY / _SHARPEN=<value>,
//! IAI_MIXER_PROBE_LOOK=raw, IAI_MIXER_PROBE_ENGINE=scene1|legacy1,
//! IAI_MIXER_PROBE_MODEL=old (pre-fix preview proxy routing).

use iai::core::develop::{DevelopEngineVersion, DevelopSettings, MIXER_BANDS};
use iai::core::develop_scene::{
    apply_scene_to_tilemap, build_scene_color_base_box, build_scene_fast_base,
    build_scene_tone_for_scene, render_default_look, scene_fast_region_develop,
    tone_lowpass_scene_region, tone_scene_color_samples, BaseLook, SceneSource,
};
use iai::core::layer::{Layer, LayerStack};
use iai::core::tile::TileMap;
use iai::gpu::compositor::{ColorProxies, CompositorState, DevelopGpuPreview};
use std::sync::Arc;

fn env_bands(key: &str) -> [f32; MIXER_BANDS] {
    let mut out = [0.0; MIXER_BANDS];
    if let Ok(v) = std::env::var(key) {
        for (slot, part) in out.iter_mut().zip(v.split(',')) {
            *slot = part.trim().parse().unwrap_or(0.0);
        }
    }
    out
}

fn env_f32(key: &str) -> f32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0)
}

fn luma(p: &[u8]) -> f32 {
    0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
}

fn chroma(p: &[u8]) -> f32 {
    let mx = p[0].max(p[1]).max(p[2]) as f32;
    let mn = p[0].min(p[1]).min(p[2]) as f32;
    mx - mn
}

/// Mean |jump| of the edit (out − base luma) across each tile boundary versus
/// the mean jump across the neighbouring non-boundary columns/rows.
fn seam_report(label: &str, base: &[u8], out: &[u8], w: usize, h: usize) -> f32 {
    let delta: Vec<f32> = base
        .chunks_exact(4)
        .zip(out.chunks_exact(4))
        .map(|(s, o)| luma(o) - luma(s))
        .collect();
    let col_jump = |x: usize| -> f32 {
        (0..h)
            .map(|y| (delta[y * w + x] - delta[y * w + x - 1]).abs())
            .sum::<f32>()
            / h as f32
    };
    let row_jump = |y: usize| -> f32 {
        (0..w)
            .map(|x| (delta[y * w + x] - delta[(y - 1) * w + x]).abs())
            .sum::<f32>()
            / w as f32
    };
    let mut worst_ratio = 0.0f32;
    for x in (256..w.saturating_sub(2)).step_by(256) {
        let seam = col_jump(x);
        let near = (col_jump(x - 2) + col_jump(x - 1) + col_jump(x + 1) + col_jump(x + 2)) / 4.0;
        worst_ratio = worst_ratio.max(seam / near.max(0.05));
        eprintln!("[{label}] x={x}: mean jump {seam:.3} vs neighbours {near:.3}");
    }
    for y in (256..h.saturating_sub(2)).step_by(256) {
        let seam = row_jump(y);
        let near = (row_jump(y - 2) + row_jump(y - 1) + row_jump(y + 1) + row_jump(y + 2)) / 4.0;
        worst_ratio = worst_ratio.max(seam / near.max(0.05));
        eprintln!("[{label}] y={y}: mean jump {seam:.3} vs neighbours {near:.3}");
    }
    eprintln!("[{label}] worst seam/neighbour ratio {worst_ratio:.2}");
    worst_ratio
}

fn save(path: &std::path::Path, rgba: &[u8], w: u32, h: u32) {
    image::save_buffer(path, rgba, w, h, image::ExtendedColorType::Rgba8).unwrap();
}

#[test]
#[ignore]
fn mixer_seam_probe() {
    let Ok(path) = std::env::var("IAI_MIXER_PROBE_IMAGE") else {
        eprintln!("IAI_MIXER_PROBE_IMAGE not set; skipping");
        return;
    };
    let out_dir = std::path::PathBuf::from(
        std::env::var("IAI_MIXER_PROBE_OUT").unwrap_or_else(|_| ".".into()),
    );
    std::fs::create_dir_all(&out_dir).unwrap();
    let img = image::open(&path).expect("open probe image").to_rgba8();
    let (w, h) = img.dimensions();
    let src = img.into_raw();
    let tiles = TileMap::from_rgba(&src, w, h);
    let mut scene = SceneSource::from_display_tiles(&tiles);
    let raw = std::env::var("IAI_MIXER_PROBE_LOOK").is_ok_and(|v| v == "raw");
    if raw {
        scene.look = BaseLook::Raw;
    }
    let scene = Arc::new(scene);

    let mut settings = DevelopSettings {
        mixer_hue: env_bands("IAI_MIXER_PROBE_HUE"),
        mixer_saturation: env_bands("IAI_MIXER_PROBE_SAT"),
        mixer_luminance: env_bands("IAI_MIXER_PROBE_LUM"),
        saturation: env_f32("IAI_MIXER_PROBE_SATURATION"),
        vibrance: env_f32("IAI_MIXER_PROBE_VIBRANCE"),
        clarity: env_f32("IAI_MIXER_PROBE_CLARITY"),
        sharpening: env_f32("IAI_MIXER_PROBE_SHARPEN"),
        ..Default::default()
    };
    if std::env::var("IAI_MIXER_PROBE_DEFAULT_MIXER").is_ok_and(|v| v == "1") {
        settings.mixer_hue[1] = -40.0;
        settings.mixer_luminance[1] = 60.0;
    }
    if let Ok(engine) = std::env::var("IAI_MIXER_PROBE_ENGINE") {
        settings.develop_engine_version = match engine.as_str() {
            "legacy1" => DevelopEngineVersion::Legacy1,
            "scene1" => DevelopEngineVersion::Scene1,
            _ => DevelopEngineVersion::Develop3,
        };
    }
    let old_model = std::env::var("IAI_MIXER_PROBE_MODEL").is_ok_and(|v| v == "old");
    eprintln!(
        "image {w}x{h} look {} model {}; hue {:?} lum {:?} sat {} vib {} clarity {} sharpen {}",
        if raw { "raw" } else { "identity" },
        if old_model { "old" } else { "new" },
        settings.mixer_hue,
        settings.mixer_luminance,
        settings.saturation,
        settings.vibrance,
        settings.clarity,
        settings.sharpening,
    );

    // The neutral render is the baseline the edit is measured against.
    let neutral16 = render_default_look(&scene);
    let neutral: Vec<u8> = neutral16.iter().map(|v| (v >> 8) as u8).collect();
    let committed = apply_scene_to_tilemap(&scene, &settings, None).flatten();
    save(&out_dir.join("base.png"), &neutral, w, h);
    save(&out_dir.join("commit.png"), &committed, w, h);
    let commit_ratio = seam_report("commit", &neutral, &committed, w as usize, h as usize);

    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no headless GPU adapter; preview skipped");
        return;
    };
    let tone = build_scene_tone_for_scene(&settings, &scene);
    let need_fast = settings.texture.abs() > 0.001
        || settings.clarity.abs() > 0.001
        || settings.dehaze.abs() > 0.001
        || settings.vignette.abs() > 0.001
        || settings.has_detail()
        || settings.has_locals();
    let guided_capable = settings.develop_engine_version == DevelopEngineVersion::Develop3
        && settings.mixer_algorithm == iai::core::develop::ColorMixerAlgorithm::V2
        && settings
            .mixer_hue
            .iter()
            .chain(&settings.mixer_saturation)
            .chain(&settings.mixer_luminance)
            .any(|v| v.abs() > 0.001);
    let linear_scene_color = raw
        && settings.has_color()
        && if old_model {
            settings.develop_engine_version != DevelopEngineVersion::Develop3
        } else {
            !guided_capable
        };
    let need_color = settings.has_color() && !linear_scene_color && !need_fast;
    let color = if need_fast {
        let s = std::env::var("IAI_MIXER_PROBE_DOWNSAMPLE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1usize);
        let (base, pw, ph) = build_scene_fast_base(&scene, 0, 0, w, h, s, settings.has_detail());
        let (region, adjusted) =
            scene_fast_region_develop(&base, &tone, &settings, None, pw, ph, 0, 0, w, h, s as u32);
        eprintln!("preview: fast proxy s={s}");
        Some(ColorProxies {
            region: Arc::new(region),
            adjusted: Arc::new(adjusted),
            w: pw,
            h: ph,
            origin_x: 0,
            origin_y: 0,
            downsample: s as u32,
            fast_preview: true,
            guided_controls: false,
            exact_detail: settings.has_detail() && s == 1,
        })
    } else if need_color {
        let s = std::env::var("IAI_MIXER_PROBE_DOWNSAMPLE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| iai::core::develop::fast_preview_downsample(w, h));
        let (base, pw, ph) = build_scene_color_base_box(&scene, 0, 0, w, h, s);
        let region = tone_lowpass_scene_region(&base, pw, ph, &tone, s);
        let controls = if old_model || raw {
            let samples = tone_scene_color_samples(&base, &tone);
            iai::core::develop::guided_mixer_controls(&samples, &settings, pw, ph)
        } else {
            None
        };
        let guided = controls.is_some();
        let adjusted = controls.unwrap_or_else(|| {
            iai::core::develop::apply_color_to_region(&region, &settings, pw, ph)
        });
        eprintln!("preview: colour proxy s={s}, guided {guided}");
        Some(ColorProxies {
            region: Arc::new(region),
            adjusted: Arc::new(adjusted),
            w: pw,
            h: ph,
            origin_x: 0,
            origin_y: 0,
            downsample: s as u32,
            fast_preview: false,
            guided_controls: guided,
            exact_detail: false,
        })
    } else {
        eprintln!("preview: per-pixel scene shader, no proxies");
        None
    };
    let mut stack = LayerStack::new(w, h);
    stack.layers[0] = Layer::from_rgba(0, "Background", neutral.clone(), w, h);
    let max_texture = device.limits().max_texture_dimension_2d;
    let mut compositor = CompositorState::new(&device, w, h, max_texture);
    compositor.develop_preview = Some(DevelopGpuPreview {
        layer_id: 0,
        settings: settings.clone(),
        region_luma: None,
        color,
        scene: Some(scene.clone()),
    });
    let is_ping =
        compositor.composite_layers(&device, &queue, &stack, 0.0, 0.0, 1.0, None, false, false);
    let preview = compositor.readback_rgba8(&device, &queue, is_ping);
    save(&out_dir.join("preview.png"), &preview, w, h);
    seam_report("preview", &neutral, &preview, w as usize, h as usize);

    let n = (w * h) as f32;
    let mean_edit = |out: &[u8], f: fn(&[u8]) -> f32| {
        out.chunks_exact(4)
            .zip(neutral.chunks_exact(4))
            .map(|(o, b)| f(o) - f(b))
            .sum::<f32>()
            / n
    };
    eprintln!(
        "mean Δluma commit {:.2} preview {:.2}; mean Δchroma commit {:.2} preview {:.2}",
        mean_edit(&committed, luma),
        mean_edit(&preview, luma),
        mean_edit(&committed, chroma),
        mean_edit(&preview, chroma),
    );
    let mut diffs: Vec<f32> = preview
        .chunks_exact(4)
        .zip(committed.chunks_exact(4))
        .map(|(a, b)| {
            (0..3)
                .map(|c| a[c].abs_diff(b[c]) as f32)
                .fold(0.0, f32::max)
        })
        .collect();
    diffs.sort_by(|a, b| a.total_cmp(b));
    let pct = |p: f32| diffs[((diffs.len() as f32 * p) as usize).min(diffs.len() - 1)];
    eprintln!(
        "preview vs commit |Δrgb| p50 {:.1} p95 {:.1} p99 {:.1} max {:.1}",
        pct(0.50),
        pct(0.95),
        pct(0.99),
        pct(1.0)
    );
    if let Ok(limit) = std::env::var("IAI_MIXER_PROBE_MAX_SEAM") {
        let limit: f32 = limit.parse().unwrap();
        assert!(
            commit_ratio <= limit,
            "commit seam ratio {commit_ratio} > {limit}"
        );
    }
}

//! CPU/commit and headless GPU/commit parity gates. The GPU gate skips cleanly
//! when the test host exposes no compatible adapter.

use iai::core::develop::{DevelopEngineVersion, DevelopSettings};
use iai::core::develop_scene::{
    apply_scene_to_tilemap, eval_scene_pixel_for_scene, render_default_look, SceneSource,
};
use iai::core::layer::{Layer, LayerStack};
use iai::gpu::compositor::{ColorProxies, CompositorState, DevelopGpuPreview, RegionLumaProxy};
use std::sync::Arc;

#[test]
fn settled_pixel_evaluator_matches_committed_scene_for_non_spatial_edits() {
    let inputs = [
        [0.01, 0.02, 0.03],
        [0.18, 0.18, 0.18],
        [0.64, 0.06, 0.03],
        [0.05, 0.55, 0.08],
        [0.03, 0.08, 0.8],
        [-0.03, 0.2, 0.4],
        [0.5, 1.4, 2.5],
    ];
    let settings_cases = [
        DevelopSettings::default(),
        DevelopSettings {
            exposure: 42.0,
            contrast: -31.0,
            ..Default::default()
        },
        DevelopSettings {
            saturation: 55.0,
            vibrance: 27.0,
            ..Default::default()
        },
    ];

    for settings in settings_cases {
        let mut scene = SceneSource::new(inputs.len() as u32, 1);
        for (x, input) in inputs.iter().copied().enumerate() {
            scene.set_rgb(
                x as u32,
                0,
                scene.color_pipeline.working.from_linear_srgb(input),
            );
        }
        let committed = apply_scene_to_tilemap(&scene, &settings, None).flatten16();
        for (x, _input) in inputs.iter().copied().enumerate() {
            // Commit reads the f16 scene master, so parity must evaluate the
            // same representable input rather than the original f32 literal.
            let stored = scene.get_rgb(x as u32, 0);
            let settled = eval_scene_pixel_for_scene(&scene, stored, &settings);
            for channel in 0..3 {
                let expected = (settled[channel].clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
                let actual = committed[x * 4 + channel];
                assert!(
                    actual.abs_diff(expected) <= 1,
                    "settled/commit mismatch at x={x} channel={channel}: {actual} vs {expected}"
                );
            }
        }
    }
}

#[test]
fn compositor_shader_remains_valid_wgsl() {
    let source = include_str!("../src/gpu/compositor.wgsl");
    naga::front::wgsl::parse_str(source).expect("compositor.wgsl must parse");
}

#[test]
fn headless_gpu_preview_matches_committed_scene() {
    // Shared CI runners expose inconsistent software adapters/compiler stacks
    // (notably Windows FXC, which cannot compile the full compositor). Keep
    // pixel parity as a local real-GPU gate; WGSL syntax is still gated above.
    if std::env::var_os("CI").is_some() {
        eprintln!("headless GPU pixel parity is a local real-GPU test; skipped on CI");
        return;
    }
    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no headless GPU adapter; skipped");
        return;
    };
    let (width, height) = (16, 8);
    let mut scene = SceneSource::new(width, height);
    scene.as_shot_white_balance = Some(iai::core::cat16::WhiteBalance {
        cct_kelvin: 2856.0,
        duv: 0.006,
    });
    for y in 0..height {
        for x in 0..width {
            let fx = x as f32 / (width - 1) as f32;
            let fy = y as f32 / (height - 1) as f32;
            let input = [fx * 1.4 - 0.04, fy * 0.9, (1.0 - fx) * 1.8];
            scene.set_rgb(x, y, scene.color_pipeline.working.from_linear_srgb(input));
        }
    }
    // A deliberately visible camera-picture-style fit. This is part of every
    // real RAW default look and must remain active when the GPU preview takes
    // over from the neutral baked tiles.
    let mut camera_curve = Box::new([[0.0f32; 256]; 3]);
    for channel in 0..3 {
        for i in 0..256 {
            let x = i as f32 / 255.0;
            camera_curve[channel][i] = x.powf([0.92, 1.04, 1.10][channel]);
        }
    }
    scene.camera_rgb_curve = Some(camera_curve);
    let scene = Arc::new(scene);
    let settings = DevelopSettings {
        temperature: -120.0,
        tint: 37.0,
        exposure: 25.0,
        contrast: -15.0,
        saturation: 31.0,
        vibrance: 18.0,
        curve_points: vec![[0.0, 0.0], [0.25, 0.20], [0.70, 0.78], [1.0, 1.0]],
        ..Default::default()
    };
    let committed = apply_scene_to_tilemap(&scene, &settings, None).flatten();
    let neutral = render_default_look(&scene);
    let neutral8: Vec<u8> = neutral.iter().map(|v| (v >> 8) as u8).collect();
    let mut stack = LayerStack::new(width, height);
    stack.layers[0] = Layer::from_rgba(0, "Background", neutral8, width, height);

    let max_texture = device.limits().max_texture_dimension_2d;
    let mut compositor = CompositorState::new(&device, width, height, max_texture);
    compositor.develop_preview = Some(DevelopGpuPreview {
        layer_id: 0,
        settings: settings.clone(),
        region_luma: None,
        color: None,
        scene: Some(scene.clone()),
    });
    let result_is_ping =
        compositor.composite_layers(&device, &queue, &stack, 0.0, 0.0, 1.0, None, false, false);
    let gpu = compositor.readback_rgba8(&device, &queue, result_is_ping);
    assert_eq!(gpu.len(), committed.len());
    let mut max_error = 0u8;
    let mut errors = Vec::with_capacity(width as usize * height as usize * 3);
    let mut worst = Vec::with_capacity(width as usize * height as usize);
    for (pixel_index, (a, b)) in gpu
        .chunks_exact(4)
        .zip(committed.chunks_exact(4))
        .enumerate()
    {
        let mut pixel_error = 0u8;
        for channel in 0..3 {
            let error = a[channel].abs_diff(b[channel]);
            max_error = max_error.max(error);
            pixel_error = pixel_error.max(error);
            errors.push(error);
        }
        worst.push((
            pixel_error,
            pixel_index % width as usize,
            pixel_index / width as usize,
            [a[0], a[1], a[2]],
            [b[0], b[1], b[2]],
        ));
    }
    errors.sort_unstable();
    worst.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    let p99 = errors[(errors.len() * 99 / 100).min(errors.len() - 1)];
    eprintln!("GPU/commit max={max_error}/255 p99={p99}/255");
    for sample in worst.iter().take(8) {
        eprintln!("GPU/commit worst={sample:?}");
    }
    assert!(max_error <= 2, "GPU/commit max error {max_error}/255");
    assert!(p99 <= 1, "GPU/commit p99 error {p99}/255");

    // Part 1 above exercises the default engine (now Develop3) on the NON-spatial
    // stages, where the GPU preview needs no proxies. The Colour Mixer is spatial
    // in Develop3 (CPU-built, luma-guided control planes), so its GPU parity is
    // tested here WITH those proxies fed — the shader must consume the same
    // Hue/Saturation/Luminance controls instead of reclassifying each pixel.
    let mut v3 = settings;
    v3.develop_engine_version = DevelopEngineVersion::Develop3;
    v3.midtones = 35.0;
    v3.mixer_hue = [24.0, -13.0, 0.0, 0.0, 0.0, 0.0, 0.0, 9.0];
    v3.mixer_saturation = [18.0, -11.0, 0.0, 0.0, 0.0, 0.0, 0.0, 7.0];
    let committed_v3 = apply_scene_to_tilemap(&scene, &v3, None).flatten();
    let tone = iai::core::develop_scene::build_scene_tone_for_scene(&v3, &scene);
    let (base, pw, ph) =
        iai::core::develop_scene::build_scene_color_base_box(&scene, 0, 0, width, height, 1);
    let toned_samples = iai::core::develop_scene::tone_scene_color_samples(&base, &tone);
    let region = iai::core::develop_scene::tone_lowpass_scene_region(&base, pw, ph, &tone, 1);
    let controls = iai::core::develop::guided_mixer_controls(&toned_samples, &v3, pw, ph)
        .expect("Develop3 V2 mixer must build guided controls");
    let (tone_base, tone_w, tone_h) = iai::core::develop_scene::build_scene_region_base(
        &scene,
        iai::core::develop::TONE_DOWNSAMPLE,
    );
    let regional_e = iai::core::develop_scene::finish_region_e(
        &tone_base,
        tone_w,
        tone_h,
        &tone,
        iai::core::develop::TONE_DOWNSAMPLE,
    );
    compositor.develop_preview = Some(DevelopGpuPreview {
        layer_id: 0,
        settings: v3.clone(),
        region_luma: Some(RegionLumaProxy {
            data: Arc::new(regional_e),
            w: tone_w,
            h: tone_h,
            downsample: iai::core::develop::TONE_DOWNSAMPLE as u32,
        }),
        color: Some(ColorProxies {
            region: Arc::new(region),
            adjusted: Arc::new(controls),
            w: pw,
            h: ph,
            origin_x: 0,
            origin_y: 0,
            downsample: 1,
            fast_preview: false,
            guided_controls: true,
            exact_detail: false,
        }),
        scene: Some(scene.clone()),
    });
    let result_is_ping =
        compositor.composite_layers(&device, &queue, &stack, 0.0, 0.0, 1.0, None, false, false);
    let gpu_v3 = compositor.readback_rgba8(&device, &queue, result_is_ping);
    let max_v3 = gpu_v3
        .chunks_exact(4)
        .zip(committed_v3.chunks_exact(4))
        .flat_map(|(gpu, cpu)| (0..3).map(move |channel| gpu[channel].abs_diff(cpu[channel])))
        .max()
        .unwrap_or(0);
    eprintln!("Develop3 guided GPU/commit max={max_v3}/255");
    assert!(max_v3 <= 2, "Develop3 GPU/commit max error {max_v3}/255");

    // Native Detail preview supplies the already output-transformed, full-density
    // viewport plane. Mode 4 must select it directly (no proxy delta re-combine
    // or second effects pass), while keeping the compositor's final quantisation.
    let detail_settings = DevelopSettings {
        sharpening: 68.0,
        noise_reduction: 34.0,
        color_noise_reduction: 51.0,
        ..v3
    };
    let committed_detail16 = apply_scene_to_tilemap(&scene, &detail_settings, None).flatten16();
    let committed_detail = apply_scene_to_tilemap(&scene, &detail_settings, None).flatten();
    let exact: Vec<[f32; 3]> = committed_detail16
        .chunks_exact(4)
        .map(|p| {
            [
                p[0] as f32 / 65535.0,
                p[1] as f32 / 65535.0,
                p[2] as f32 / 65535.0,
            ]
        })
        .collect();
    compositor.develop_preview = Some(DevelopGpuPreview {
        layer_id: 0,
        settings: detail_settings,
        region_luma: None,
        color: Some(ColorProxies {
            region: Arc::new(exact.clone()),
            adjusted: Arc::new(exact),
            w: width as usize,
            h: height as usize,
            origin_x: 0,
            origin_y: 0,
            downsample: 1,
            fast_preview: true,
            guided_controls: false,
            exact_detail: true,
        }),
        scene: Some(scene),
    });
    let result_is_ping =
        compositor.composite_layers(&device, &queue, &stack, 0.0, 0.0, 1.0, None, false, false);
    let gpu_detail = compositor.readback_rgba8(&device, &queue, result_is_ping);
    let max_detail = gpu_detail
        .chunks_exact(4)
        .zip(committed_detail.chunks_exact(4))
        .flat_map(|(gpu, cpu)| (0..3).map(move |channel| gpu[channel].abs_diff(cpu[channel])))
        .max()
        .unwrap_or(0);
    eprintln!("native Detail proxy/commit max={max_detail}/255");
    assert!(
        max_detail <= 1,
        "native Detail proxy/commit max error {max_detail}/255"
    );
}

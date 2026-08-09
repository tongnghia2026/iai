//! Parity gates that can run without a display adapter. The CPU settled/commit
//! gate is active in CI; the remaining GPU readback gap is documented in the
//! Phase 0 baseline until the compositor exposes a headless Develop entry point.

use iai::core::develop::DevelopSettings;
use iai::core::develop_scene::{
    apply_scene_to_tilemap, eval_scene_pixel, render_default_look, BaseLook, SceneSource,
};
use iai::core::layer::{Layer, LayerStack};
use iai::gpu::compositor::{CompositorState, DevelopGpuPreview};
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
            let compatibility_input = scene.color_pipeline.working.to_linear_srgb(stored);
            let settled = eval_scene_pixel(compatibility_input, &settings, BaseLook::Raw);
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
#[ignore = "requires a local headless GPU adapter"]
fn headless_gpu_preview_matches_committed_scene() {
    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no headless GPU adapter; skipped");
        return;
    };
    let (width, height) = (16, 8);
    let mut scene = SceneSource::new(width, height);
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
    let settings = DevelopSettings {
        exposure: 25.0,
        contrast: -15.0,
        saturation: 31.0,
        vibrance: 18.0,
        curve_points: vec![[0.0, 0.0], [0.25, 0.20], [0.70, 0.78], [1.0, 1.0]],
        mixer_hue: [24.0, -13.0, 0.0, 0.0, 0.0, 0.0, 0.0, 9.0],
        mixer_saturation: [18.0, -11.0, 0.0, 0.0, 0.0, 0.0, 0.0, 7.0],
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
        settings,
        region_luma: None,
        color: None,
        scene: Some(Arc::new(scene)),
    });
    let result_is_ping =
        compositor.composite_layers(&device, &queue, &stack, 0.0, 0.0, 1.0, None, false, false);
    let gpu = compositor.readback_rgba8(&device, &queue, result_is_ping);
    assert_eq!(gpu.len(), committed.len());
    let mut max_error = 0u8;
    let mut errors = Vec::with_capacity(width as usize * height as usize * 3);
    for (a, b) in gpu.chunks_exact(4).zip(committed.chunks_exact(4)) {
        for channel in 0..3 {
            let error = a[channel].abs_diff(b[channel]);
            max_error = max_error.max(error);
            errors.push(error);
        }
    }
    errors.sort_unstable();
    let p99 = errors[(errors.len() * 99 / 100).min(errors.len() - 1)];
    eprintln!("GPU/commit max={max_error}/255 p99={p99}/255");
    assert!(max_error <= 2, "GPU/commit max error {max_error}/255");
    assert!(p99 <= 1, "GPU/commit p99 error {p99}/255");
}

//! CPU/commit and headless GPU/commit parity gates. The GPU gate skips cleanly
//! when the test host exposes no compatible adapter.

use iai::core::develop::{DevelopEngineVersion, DevelopSettings};
use iai::core::develop_scene::{
    apply_scene_to_tilemap, eval_scene_pixel_for_scene, render_default_look, SceneSource,
};
use iai::core::layer::{Layer, LayerStack};
use iai::gpu::compositor::{
    ColorProxies, CompositorState, DevelopDetailGpu, DevelopGpuPreview, RegionLumaProxy,
};
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

/// Identity (JPEG/PNG) scenes commit colour through the display-domain proxy
/// bake, so their live colour preview feeds the same region/adjusted RGB
/// proxies (mode 1) and the scene shader must not colour on top. Before this
/// gate the Develop3 preview applied RAW's per-pixel guided planes instead
/// (lift bled onto the background) and older engines applied the mixer twice.
#[test]
fn headless_identity_colour_preview_matches_commit() {
    if std::env::var_os("CI").is_some() {
        eprintln!("headless GPU pixel parity is a local real-GPU test; skipped on CI");
        return;
    }
    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no headless GPU adapter; skipped");
        return;
    };
    let (width, height) = (96u32, 48u32);
    let mut px = Vec::with_capacity((width * height * 4) as usize);
    for _y in 0..height {
        for x in 0..width {
            let rgb = if x < width / 2 {
                [220u8, 160, 120]
            } else {
                [30, 110, 210]
            };
            px.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    let tiles = iai::core::tile::TileMap::from_rgba(&px, width, height);
    let scene = Arc::new(SceneSource::from_display_tiles(&tiles));
    let mut stack = LayerStack::new(width, height);
    stack.layers[0] = Layer::from_rgba(0, "Background", px.clone(), width, height);
    let max_texture = device.limits().max_texture_dimension_2d;
    let mut compositor = CompositorState::new(&device, width, height, max_texture);

    for engine in [DevelopEngineVersion::Develop3, DevelopEngineVersion::Scene1] {
        let mut settings = DevelopSettings {
            develop_engine_version: engine,
            ..Default::default()
        };
        settings.mixer_hue[1] = -40.0;
        settings.mixer_luminance[1] = 60.0;
        let committed = apply_scene_to_tilemap(&scene, &settings, None).flatten();

        // The live colour path for an Identity scene (develop_preview.rs).
        let s = 6; // the commit's COLOR_DOWNSAMPLE grid
        let tone = iai::core::develop_scene::build_scene_tone_for_scene(&settings, &scene);
        let (base, pw, ph) =
            iai::core::develop_scene::build_scene_color_base_box(&scene, 0, 0, width, height, s);
        let region = iai::core::develop_scene::tone_lowpass_scene_region(&base, pw, ph, &tone, s);
        let adjusted = iai::core::develop::apply_color_to_region(&region, &settings, pw, ph);
        compositor.develop_preview = Some(DevelopGpuPreview {
            layer_id: 0,
            settings: settings.clone(),
            region_luma: None,
            color: Some(ColorProxies {
                region: Arc::new(region),
                adjusted: Arc::new(adjusted),
                w: pw,
                h: ph,
                origin_x: 0,
                origin_y: 0,
                downsample: s as u32,
                fast_preview: false,
                guided_controls: false,
                exact_detail: false,
            }),
            scene: Some(scene.clone()),
            detail: None,
        });
        let is_ping =
            compositor.composite_layers(&device, &queue, &stack, 0.0, 0.0, 1.0, None, false, false);
        let gpu = compositor.readback_rgba8(&device, &queue, is_ping);

        // Interior of each patch: away from the edge where linear (preview)
        // and display (commit) box averages legitimately differ. Scene1
        // commits colour per pixel (no smoothing) while its preview tapers
        // the edit toward a colour edge, so it is checked deeper inside —
        // where a doubled mixer would still show in full.
        let margin = if engine == DevelopEngineVersion::Scene1 {
            24
        } else {
            8
        };
        let half = width as usize / 2;
        let mut max_error = 0u8;
        for y in 8..height as usize - 8 {
            for x in (8..half - margin).chain(half + margin..width as usize - 8) {
                let k = (y * width as usize + x) * 4;
                for c in 0..3 {
                    max_error = max_error.max(gpu[k + c].abs_diff(committed[k + c]));
                }
            }
        }
        let skin = (24 * width as usize + 20) * 4;
        eprintln!(
            "{engine:?} identity colour preview/commit max={max_error}/255 skin gpu={:?} cpu={:?} src={:?}",
            &gpu[skin..skin + 3],
            &committed[skin..skin + 3],
            &px[skin..skin + 3]
        );
        assert!(
            committed[skin + 1] > px[skin + 1],
            "test setup: Orange Luminance must lift the skin"
        );
        assert!(
            max_error <= 2,
            "{engine:?} identity colour preview/commit max error {max_error}/255"
        );
    }
}

/// Identity fast-proxy modes (Effects/Detail engaged): the proxies carry the
/// display-domain Colour/Effects/Detail tail of the commit and the scene
/// shader only tones. The preview used to recolour per pixel on top, which
/// cancelled Saturation whenever Clarity or Detail was also on.
#[test]
fn headless_identity_fast_preview_matches_commit() {
    if std::env::var_os("CI").is_some() {
        eprintln!("headless GPU pixel parity is a local real-GPU test; skipped on CI");
        return;
    }
    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no headless GPU adapter; skipped");
        return;
    };
    let (width, height) = (96u32, 48u32);
    let mut px = Vec::with_capacity((width * height * 4) as usize);
    for _y in 0..height {
        for x in 0..width {
            let rgb = if x < width / 2 {
                [210u8, 150, 115]
            } else {
                [60, 120, 180]
            };
            px.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    let tiles = iai::core::tile::TileMap::from_rgba(&px, width, height);
    let scene = Arc::new(SceneSource::from_display_tiles(&tiles));
    let mut stack = LayerStack::new(width, height);
    stack.layers[0] = Layer::from_rgba(0, "Background", px.clone(), width, height);
    let max_texture = device.limits().max_texture_dimension_2d;
    let mut compositor = CompositorState::new(&device, width, height, max_texture);

    let sat_clarity = DevelopSettings {
        saturation: 45.0,
        clarity: 30.0,
        ..Default::default()
    };
    let mut sat_mixer_sharp = DevelopSettings {
        saturation: 30.0,
        sharpening: 40.0,
        ..Default::default()
    };
    sat_mixer_sharp.mixer_luminance[1] = 50.0;
    for (label, settings) in [
        ("sat+clarity", sat_clarity),
        ("sat+mixer+sharp", sat_mixer_sharp),
    ] {
        let committed = apply_scene_to_tilemap(&scene, &settings, None).flatten();
        let tone = iai::core::develop_scene::build_scene_tone_for_scene(&settings, &scene);
        let (base, pw, ph) = iai::core::develop_scene::build_scene_fast_base(
            &scene,
            0,
            0,
            width,
            height,
            1,
            settings.has_detail(),
        );
        let (region, adjusted) = iai::core::develop_scene::identity_fast_region_develop(
            &base, &tone, &settings, None, pw, ph, 0, 0, width, height, 1,
        );
        compositor.develop_preview = Some(DevelopGpuPreview {
            layer_id: 0,
            settings: settings.clone(),
            region_luma: None,
            color: Some(ColorProxies {
                region: Arc::new(region),
                adjusted: Arc::new(adjusted),
                w: pw,
                h: ph,
                origin_x: 0,
                origin_y: 0,
                downsample: 1,
                fast_preview: true,
                guided_controls: false,
                exact_detail: settings.has_detail(),
            }),
            scene: Some(scene.clone()),
            detail: None,
        });
        let is_ping =
            compositor.composite_layers(&device, &queue, &stack, 0.0, 0.0, 1.0, None, false, false);
        let gpu = compositor.readback_rgba8(&device, &queue, is_ping);
        let mut max_error = 0u8;
        for y in 8..height as usize - 8 {
            for x in (8..40).chain(56..88) {
                let k = (y * width as usize + x) * 4;
                for c in 0..3 {
                    max_error = max_error.max(gpu[k + c].abs_diff(committed[k + c]));
                }
            }
        }
        let skin = (24 * width as usize + 20) * 4;
        eprintln!(
            "identity fast {label}: preview/commit max={max_error}/255 skin gpu={:?} cpu={:?} src={:?}",
            &gpu[skin..skin + 3],
            &committed[skin..skin + 3],
            &px[skin..skin + 3]
        );
        let spread = |p: &[u8]| p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2]);
        assert!(
            spread(&committed[skin..skin + 3]) > spread(&px[skin..skin + 3]),
            "test setup: Saturation must widen the skin's channel spread"
        );
        assert!(
            max_error <= 2,
            "identity fast {label} preview/commit max error {max_error}/255"
        );
    }
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
        detail: None,
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
        detail: None,
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
        detail: None,
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

fn hash01(x: u32, y: u32) -> f32 {
    let mut v = x
        .wrapping_mul(2_654_435_761)
        .wrapping_add(y.wrapping_mul(2_246_822_519))
        .wrapping_add(2_463_534_242);
    v ^= v >> 15;
    v = v.wrapping_mul(2_246_822_519);
    v ^= v >> 13;
    v as f32 / u32::MAX as f32
}

/// Linear-sRGB test pattern with a hard edge, fine texture and noise, so
/// Sharpening, Luminance NR and Colour NR all engage.
fn textured_rgb(x: u32, y: u32, w: u32) -> [f32; 3] {
    let edge = if x > w / 2 { 0.34 } else { 0.07 };
    let tex = 0.025 * ((x as f32) * 1.7).sin() * ((y as f32) * 0.9).cos();
    let n = 0.03 * (hash01(x, y) - 0.5);
    let c = 0.02 * (hash01(x + 7, y + 3) - 0.5);
    [
        (edge * 1.2 + tex + n + c).max(0.0),
        (edge + tex + n).max(0.0),
        (edge * 0.7 + tex + n - c).max(0.0),
    ]
}

fn srgb8(linear: f32) -> u8 {
    let v = linear.clamp(0.0, 1.0);
    let e = if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    (e * 255.0 + 0.5) as u8
}

fn max_abs_rgb(a: &[u8], b: &[u8], keep: impl Fn(usize, usize) -> bool, width: usize) -> u8 {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .enumerate()
        .filter(|(i, _)| keep(i % width, i / width))
        .flat_map(|(_, (p, q))| (0..3).map(move |c| p[c].abs_diff(q[c])))
        .max()
        .unwrap_or(0)
}

/// GPU-resident Detail (mode 5) on a RAW scene: the compositor evaluates the
/// shader chain up to colour into a plane, runs Detail on the working-space
/// values and applies the output transform afterwards — the commit's order.
#[test]
fn headless_gpu_detail_raw_matches_commit() {
    if std::env::var_os("CI").is_some() {
        eprintln!("headless GPU pixel parity is a local real-GPU test; skipped on CI");
        return;
    }
    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no headless GPU adapter; skipped");
        return;
    };
    let (width, height) = (128u32, 96u32);
    let mut scene = SceneSource::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let rgb = textured_rgb(x, y, width);
            scene.set_rgb(x, y, scene.color_pipeline.working.from_linear_srgb(rgb));
        }
    }
    let scene = Arc::new(scene);
    let mut settings = DevelopSettings {
        develop_engine_version: DevelopEngineVersion::Develop3,
        exposure: 18.0,
        contrast: 12.0,
        shadows: 25.0,
        saturation: 15.0,
        sharpening: 70.0,
        noise_reduction: 30.0,
        color_noise_reduction: 45.0,
        ..Default::default()
    };
    settings.mixer_hue[1] = -20.0;
    settings.mixer_saturation[4] = 25.0;
    let committed = apply_scene_to_tilemap(&scene, &settings, None).flatten();
    let no_detail = DevelopSettings {
        sharpening: 0.0,
        noise_reduction: 0.0,
        color_noise_reduction: 0.0,
        ..settings.clone()
    };
    let committed_plain = apply_scene_to_tilemap(&scene, &no_detail, None).flatten();
    let detail_effect = max_abs_rgb(&committed, &committed_plain, |_, _| true, width as usize);
    assert!(
        detail_effect > 8,
        "test setup: Detail must visibly change the commit"
    );

    let tone = iai::core::develop_scene::build_scene_tone_for_scene(&settings, &scene);
    let (base, pw, ph) =
        iai::core::develop_scene::build_scene_color_base_box(&scene, 0, 0, width, height, 1);
    let toned_samples = iai::core::develop_scene::tone_scene_color_samples(&base, &tone);
    let region = iai::core::develop_scene::tone_lowpass_scene_region(&base, pw, ph, &tone, 1);
    let controls = iai::core::develop::guided_mixer_controls(&toned_samples, &settings, pw, ph)
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
    let neutral = render_default_look(&scene);
    let neutral8: Vec<u8> = neutral.iter().map(|v| (v >> 8) as u8).collect();
    let mut stack = LayerStack::new(width, height);
    stack.layers[0] = Layer::from_rgba(0, "Background", neutral8, width, height);
    let max_texture = device.limits().max_texture_dimension_2d;
    let mut compositor = CompositorState::new(&device, width, height, max_texture);
    compositor.develop_preview = Some(DevelopGpuPreview {
        layer_id: 0,
        settings: settings.clone(),
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
        detail: Some(DevelopDetailGpu {
            origin_x: 0,
            origin_y: 0,
            end_x: width,
            end_y: height,
            downsample: 1,
            linear: true,
            luma_coeff: tone.working_space.render_luminance_coefficients(),
            run_detail: true,
        }),
    });
    let is_ping =
        compositor.composite_layers(&device, &queue, &stack, 0.0, 0.0, 1.0, None, false, false);
    let gpu = compositor.readback_rgba8(&device, &queue, is_ping);
    let max_error = max_abs_rgb(&gpu, &committed, |_, _| true, width as usize);
    eprintln!(
        "RAW GPU Detail/commit max={max_error}/255 (Detail moves the commit by {detail_effect})"
    );
    assert!(
        max_error <= 2,
        "RAW GPU Detail/commit max error {max_error}/255"
    );

    // A recomposite with identical inputs reuses the plane (no GPU work) and
    // must show the same pixels.
    let is_ping =
        compositor.composite_layers(&device, &queue, &stack, 0.0, 0.0, 1.0, None, false, false);
    let again = compositor.readback_rgba8(&device, &queue, is_ping);
    assert_eq!(gpu, again, "a cached Detail plane must render identically");
}

/// GPU-resident Detail on an Identity (JPEG/PNG) scene: the display-domain
/// chain (tone, colour proxies) feeds display-domain Detail, like the commit.
#[test]
fn headless_gpu_detail_identity_matches_commit() {
    if std::env::var_os("CI").is_some() {
        eprintln!("headless GPU pixel parity is a local real-GPU test; skipped on CI");
        return;
    }
    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no headless GPU adapter; skipped");
        return;
    };
    let (width, height) = (128u32, 96u32);
    let mut px = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            for c in textured_rgb(x, y, width) {
                px.push(srgb8(c));
            }
            px.push(255);
        }
    }
    let tiles = iai::core::tile::TileMap::from_rgba(&px, width, height);
    let scene = Arc::new(SceneSource::from_display_tiles(&tiles));
    let mut stack = LayerStack::new(width, height);
    stack.layers[0] = Layer::from_rgba(0, "Background", px.clone(), width, height);
    let max_texture = device.limits().max_texture_dimension_2d;
    let mut compositor = CompositorState::new(&device, width, height, max_texture);

    let mut with_colour = DevelopSettings {
        exposure: 10.0,
        contrast: 20.0,
        sharpening: 80.0,
        noise_reduction: 25.0,
        color_noise_reduction: 40.0,
        ..Default::default()
    };
    with_colour.mixer_luminance[1] = 40.0;
    let light_only = DevelopSettings {
        exposure: -8.0,
        highlights: -30.0,
        sharpening: 60.0,
        sharpen_detail: 60.0,
        ..Default::default()
    };
    for (label, settings) in [("light+colour", with_colour), ("light", light_only)] {
        let committed = apply_scene_to_tilemap(&scene, &settings, None).flatten();
        let tone = iai::core::develop_scene::build_scene_tone_for_scene(&settings, &scene);
        let region_luma = settings.has_local_tone().then(|| {
            let (b, w, h) = iai::core::develop_scene::build_scene_region_base(
                &scene,
                iai::core::develop::TONE_DOWNSAMPLE,
            );
            RegionLumaProxy {
                data: Arc::new(iai::core::develop_scene::finish_region_e(
                    &b,
                    w,
                    h,
                    &tone,
                    iai::core::develop::TONE_DOWNSAMPLE,
                )),
                w,
                h,
                downsample: iai::core::develop::TONE_DOWNSAMPLE as u32,
            }
        });
        // The live preview passes no Identity colour proxies with a Detail
        // plane: the compositor builds the commit's colour field from it.
        compositor.develop_preview = Some(DevelopGpuPreview {
            layer_id: 0,
            settings: settings.clone(),
            region_luma,
            color: None,
            scene: Some(scene.clone()),
            detail: Some(DevelopDetailGpu {
                origin_x: 0,
                origin_y: 0,
                end_x: width,
                end_y: height,
                downsample: 1,
                linear: false,
                luma_coeff: [0.2126, 0.7152, 0.0722],
                run_detail: true,
            }),
        });
        let is_ping =
            compositor.composite_layers(&device, &queue, &stack, 0.0, 0.0, 1.0, None, false, false);
        let gpu = compositor.readback_rgba8(&device, &queue, is_ping);
        // Away from the colour edge, where the preview's colour low-pass grid
        // legitimately differs from the commit's (see the colour gate above).
        let half = width as usize / 2;
        let max_error = max_abs_rgb(
            &gpu,
            &committed,
            |x, _| x < half - 10 || x > half + 10,
            width as usize,
        );
        eprintln!("identity GPU Detail {label}: preview/commit max={max_error}/255");
        assert!(
            max_error <= 2,
            "identity GPU Detail {label} preview/commit max error {max_error}/255"
        );
    }
}

/// Mode-5 plane placement: a plane that starts inside the layer (a panned
/// view) must land on the right pixels, and a downsampled plane (a zoomed-out
/// view) must equal the CPU fast chain at the same preview scale.
#[test]
fn headless_gpu_detail_plane_offset_and_downsample() {
    if std::env::var_os("CI").is_some() {
        eprintln!("headless GPU pixel parity is a local real-GPU test; skipped on CI");
        return;
    }
    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no headless GPU adapter; skipped");
        return;
    };
    let (width, height) = (400u32, 120u32);
    let mut px = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            // Texture at 2×2-block granularity, so the zoomed-out plane's grid
            // average below equals one pixel of its block.
            for c in textured_rgb((x & !1) % 128, y & !1, 128) {
                px.push(srgb8(c));
            }
            px.push(255);
        }
    }
    let tiles = iai::core::tile::TileMap::from_rgba(&px, width, height);
    let scene = Arc::new(SceneSource::from_display_tiles(&tiles));
    let mut stack = LayerStack::new(width, height);
    stack.layers[0] = Layer::from_rgba(0, "Background", px.clone(), width, height);
    let max_texture = device.limits().max_texture_dimension_2d;
    let mut compositor = CompositorState::new(&device, width, height, max_texture);
    let settings = DevelopSettings {
        exposure: 12.0,
        sharpening: 90.0,
        noise_reduction: 20.0,
        ..Default::default()
    };
    let committed = apply_scene_to_tilemap(&scene, &settings, None).flatten();
    let preview = |detail: DevelopDetailGpu| DevelopGpuPreview {
        layer_id: 0,
        settings: settings.clone(),
        region_luma: None,
        color: None,
        scene: Some(scene.clone()),
        detail: Some(detail),
    };

    // Panned: plane over x ∈ [150, 400). Pixels a full apron inside its left
    // edge see exactly the neighbourhood the whole-image commit sees.
    compositor.develop_preview = Some(preview(DevelopDetailGpu {
        origin_x: 150,
        origin_y: 0,
        end_x: width,
        end_y: height,
        downsample: 1,
        linear: false,
        luma_coeff: [0.2126, 0.7152, 0.0722],
        run_detail: true,
    }));
    let is_ping =
        compositor.composite_layers(&device, &queue, &stack, 0.0, 0.0, 1.0, None, false, false);
    let gpu = compositor.readback_rgba8(&device, &queue, is_ping);
    // Detail's dependency radius (DETAIL_HALO = 72) plus a margin.
    let apron = 74;
    let max_error = max_abs_rgb(&gpu, &committed, |x, _| x >= 150 + apron, width as usize);
    eprintln!("offset GPU Detail plane/commit max={max_error}/255");
    assert!(max_error <= 2, "offset plane max error {max_error}/255");

    // Zoomed out: one texel per 2×2 block — the average of its (uniform)
    // pixels — and Detail at preview scale 2: the CPU fast chain on one pixel
    // per block, upsampled like the shader samples it.
    let ds = 2u32;
    compositor.develop_preview = Some(preview(DevelopDetailGpu {
        origin_x: 0,
        origin_y: 0,
        end_x: width,
        end_y: height,
        downsample: ds,
        linear: false,
        luma_coeff: [0.2126, 0.7152, 0.0722],
        run_detail: true,
    }));
    let is_ping =
        compositor.composite_layers(&device, &queue, &stack, 0.0, 0.0, 1.0, None, false, false);
    let gpu = compositor.readback_rgba8(&device, &queue, is_ping);
    let tone = iai::core::develop_scene::build_scene_tone_for_scene(&settings, &scene);
    let (base, pw, ph) = iai::core::develop_scene::build_scene_fast_base(
        &scene,
        0,
        0,
        width,
        height,
        ds as usize,
        false,
    );
    let (_, plane) = iai::core::develop_scene::identity_fast_region_develop(
        &base, &tone, &settings, None, pw, ph, 0, 0, width, height, ds,
    );
    let mut max_error = 0u8;
    for y in 0..height as usize {
        for x in 0..width as usize {
            let fx = ((x as f32 + 0.5) / ds as f32 - 0.5).clamp(0.0, (pw - 1) as f32);
            let fy = ((y as f32 + 0.5) / ds as f32 - 0.5).clamp(0.0, (ph - 1) as f32);
            let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
            let (x1, y1) = ((x0 + 1).min(pw - 1), (y0 + 1).min(ph - 1));
            let (wx, wy) = (fx - x0 as f32, fy - y0 as f32);
            for c in 0..3 {
                let top = plane[y0 * pw + x0][c] * (1.0 - wx) + plane[y0 * pw + x1][c] * wx;
                let bot = plane[y1 * pw + x0][c] * (1.0 - wx) + plane[y1 * pw + x1][c] * wx;
                let v = (top * (1.0 - wy) + bot * wy).clamp(0.0, 1.0);
                let expected = (v * 255.0 + 0.5) as u8;
                let got = gpu[(y * width as usize + x) * 4 + c];
                max_error = max_error.max(got.abs_diff(expected));
            }
        }
    }
    eprintln!("downsampled GPU Detail plane/CPU fast chain max={max_error}/255");
    assert!(
        max_error <= 2,
        "downsampled plane max error {max_error}/255"
    );
}

/// Mode-5 preview of `settings` over the whole image (full-resolution
/// plane), with the proxies the app sends for it: RAW guided mixer planes,
/// the regional tone plane, no Identity colour proxies.
fn plane_preview(
    scene: &Arc<SceneSource>,
    settings: &DevelopSettings,
    raw: bool,
) -> DevelopGpuPreview {
    let (width, height) = (scene.width, scene.height);
    let tone = iai::core::develop_scene::build_scene_tone_for_scene(settings, scene);
    let color = (raw && iai::core::develop::guided_mixer_active(settings)).then(|| {
        let (base, pw, ph) =
            iai::core::develop_scene::build_scene_color_base_box(scene, 0, 0, width, height, 1);
        let samples = iai::core::develop_scene::tone_scene_color_samples(&base, &tone);
        let region = iai::core::develop_scene::tone_lowpass_scene_region(&base, pw, ph, &tone, 1);
        let controls = iai::core::develop::guided_mixer_controls(&samples, settings, pw, ph)
            .expect("guided controls");
        ColorProxies {
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
        }
    });
    let region_luma = settings.has_local_tone().then(|| {
        let (b, w, h) = iai::core::develop_scene::build_scene_region_base(
            scene,
            iai::core::develop::TONE_DOWNSAMPLE,
        );
        RegionLumaProxy {
            data: Arc::new(iai::core::develop_scene::finish_region_e(
                &b,
                w,
                h,
                &tone,
                iai::core::develop::TONE_DOWNSAMPLE,
            )),
            w,
            h,
            downsample: iai::core::develop::TONE_DOWNSAMPLE as u32,
        }
    });
    DevelopGpuPreview {
        layer_id: 0,
        settings: settings.clone(),
        region_luma,
        color,
        scene: Some(scene.clone()),
        detail: Some(DevelopDetailGpu {
            origin_x: 0,
            origin_y: 0,
            end_x: width,
            end_y: height,
            downsample: 1,
            linear: raw,
            luma_coeff: if raw {
                tone.working_space.render_luminance_coefficients()
            } else {
                [0.2126, 0.7152, 0.0722]
            },
            run_detail: settings.has_detail(),
        }),
    }
}

/// Spatial Effects on the GPU plane (with and without Detail) against the
/// commit, for Identity (colour field / per-pixel colour / no colour) and
/// RAW. The image spans several 256 px tiles so the Identity base is
/// checked across tile seams too.
#[test]
fn headless_gpu_effects_match_commit() {
    if std::env::var_os("CI").is_some() {
        eprintln!("headless GPU pixel parity is a local real-GPU test; skipped on CI");
        return;
    }
    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no headless GPU adapter; skipped");
        return;
    };
    let (width, height) = (400u32, 300u32);
    let mut px = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let mut rgb = textured_rgb(x % 160, y, 160);
            // A broad brightness ramp so the regional base varies.
            let ramp = 0.6 + 0.8 * (x as f32 / width as f32);
            for c in &mut rgb {
                *c *= ramp;
            }
            for c in rgb {
                px.push(srgb8(c));
            }
            px.push(255);
        }
    }
    let tiles = iai::core::tile::TileMap::from_rgba(&px, width, height);
    let max_texture = device.limits().max_texture_dimension_2d;

    let effects = DevelopSettings {
        exposure: 8.0,
        clarity: 45.0,
        texture: 30.0,
        dehaze: 25.0,
        vignette: -30.0,
        ..Default::default()
    };
    let effects_detail = DevelopSettings {
        sharpening: 50.0,
        noise_reduction: 20.0,
        ..effects.clone()
    };
    let effects_sat = DevelopSettings {
        saturation: 20.0,
        ..effects_detail.clone()
    };
    let mut effects_mixer = effects_detail.clone();
    effects_mixer.mixer_luminance[1] = 40.0;
    effects_mixer.mixer_hue[4] = -20.0;

    for raw in [false, true] {
        let mut scene = SceneSource::from_display_tiles(&tiles);
        if raw {
            scene.look = iai::core::develop_scene::BaseLook::Raw;
        }
        let scene = Arc::new(scene);
        let source = if raw {
            iai::core::develop_scene::render_default_look(&scene)
                .iter()
                .map(|v| (v >> 8) as u8)
                .collect()
        } else {
            px.clone()
        };
        let mut stack = LayerStack::new(width, height);
        stack.layers[0] = Layer::from_rgba(0, "Background", source, width, height);
        let mut compositor = CompositorState::new(&device, width, height, max_texture);
        let mut effects_mixer_only = effects.clone();
        effects_mixer_only.mixer_luminance[1] = 40.0;
        effects_mixer_only.mixer_hue[4] = -20.0;
        let mut mixer_detail_only = DevelopSettings {
            sharpening: 50.0,
            noise_reduction: 20.0,
            exposure: 8.0,
            ..Default::default()
        };
        mixer_detail_only.mixer_luminance[1] = 40.0;
        mixer_detail_only.mixer_hue[4] = -20.0;
        let mut mixer_only = DevelopSettings {
            exposure: 8.0,
            ..Default::default()
        };
        mixer_only.mixer_luminance[1] = 40.0;
        mixer_only.mixer_hue[4] = -20.0;
        for (label, settings) in [
            ("mixer", &mixer_only),
            ("effects+mixer", &effects_mixer_only),
            ("mixer+Detail", &mixer_detail_only),
            ("effects", &effects),
            ("effects+Detail", &effects_detail),
            ("effects+sat+Detail", &effects_sat),
            ("effects+mixer+Detail", &effects_mixer),
        ] {
            let committed = apply_scene_to_tilemap(&scene, settings, None).flatten();
            compositor.develop_preview = Some(plane_preview(&scene, settings, raw));
            let is_ping = compositor
                .composite_layers(&device, &queue, &stack, 0.0, 0.0, 1.0, None, false, false);
            let gpu = compositor.readback_rgba8(&device, &queue, is_ping);
            let mut worst: Vec<(u8, usize, usize)> = gpu
                .chunks_exact(4)
                .zip(committed.chunks_exact(4))
                .enumerate()
                .map(|(i, (a, b))| {
                    let e = (0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap();
                    (e, i % width as usize, i / width as usize)
                })
                .collect();
            worst.sort_unstable_by(|a, b| b.0.cmp(&a.0));
            // A near-black pixel can land on the Effects' zero-luma branch on
            // one side only (f32 rounding at the 1e-6 threshold) and turn
            // neutral there: allow a handful of such isolated outliers.
            let outliers = worst.iter().filter(|w| w.0 > 3).count();

            let mut errors: Vec<u8> = gpu
                .chunks_exact(4)
                .zip(committed.chunks_exact(4))
                .flat_map(|(a, b)| (0..3).map(move |c| a[c].abs_diff(b[c])))
                .collect();
            errors.sort_unstable();
            let max = *errors.last().unwrap();
            let p99 = errors[errors.len() * 99 / 100];
            eprintln!(
                "{} GPU {label}: preview/commit max={max}/255 p99={p99}/255 outliers={outliers}",
                if raw { "RAW " } else { "JPEG" }
            );
            assert!(
                outliers * 10_000 <= worst.len() && p99 <= 1,
                "{label} ({}) preview/commit max {max}/255 p99 {p99}/255, {outliers} outliers",
                if raw { "RAW" } else { "JPEG" }
            );
        }
    }
}

/// Local masks run in the shader: Identity masks after Effects (before
/// Detail), RAW masks on the working pixel after Detail — both with and
/// without a GPU plane — against the commit.
#[test]
fn headless_gpu_local_masks_match_commit() {
    if std::env::var_os("CI").is_some() {
        eprintln!("headless GPU pixel parity is a local real-GPU test; skipped on CI");
        return;
    }
    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no headless GPU adapter; skipped");
        return;
    };
    use iai::core::develop::{LocalAdjustment, LocalMaskShape, LocalSettings};
    let (width, height) = (320u32, 240u32);
    let mut px = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let mut rgb = textured_rgb(x % 160, y, 160);
            let ramp = 0.5 + 0.9 * (y as f32 / height as f32);
            for c in &mut rgb {
                *c *= ramp;
            }
            for c in rgb {
                px.push(srgb8(c));
            }
            px.push(255);
        }
    }
    let tiles = iai::core::tile::TileMap::from_rgba(&px, width, height);
    let max_texture = device.limits().max_texture_dimension_2d;
    let masks = vec![
        LocalAdjustment {
            shape: LocalMaskShape::Linear {
                x0: 0.1,
                y0: 0.0,
                x1: 0.6,
                y1: 0.8,
            },
            settings: LocalSettings {
                exposure: 25.0,
                contrast: 30.0,
                saturation: 35.0,
                temperature: 20.0,
                ..Default::default()
            },
        },
        LocalAdjustment {
            shape: LocalMaskShape::Radial {
                cx: 0.7,
                cy: 0.5,
                rx: 0.25,
                ry: 0.35,
                feather: 0.6,
                invert: false,
            },
            settings: LocalSettings {
                exposure: -20.0,
                shadows: 40.0,
                highlights: -30.0,
                saturation: -40.0,
                tint: -15.0,
                ..Default::default()
            },
        },
    ];
    let only = DevelopSettings {
        exposure: 5.0,
        locals: masks.clone(),
        ..Default::default()
    };
    let with_colour = DevelopSettings {
        saturation: 15.0,
        ..only.clone()
    };
    let with_detail = DevelopSettings {
        sharpening: 50.0,
        noise_reduction: 20.0,
        clarity: 30.0,
        ..with_colour.clone()
    };
    for raw in [false, true] {
        let mut scene = SceneSource::from_display_tiles(&tiles);
        if raw {
            scene.look = iai::core::develop_scene::BaseLook::Raw;
        }
        let scene = Arc::new(scene);
        let source = if raw {
            iai::core::develop_scene::render_default_look(&scene)
                .iter()
                .map(|v| (v >> 8) as u8)
                .collect()
        } else {
            px.clone()
        };
        let mut stack = LayerStack::new(width, height);
        stack.layers[0] = Layer::from_rgba(0, "Background", source, width, height);
        let mut compositor = CompositorState::new(&device, width, height, max_texture);
        for (label, settings, plane) in [
            ("masks", &only, false),
            ("masks+sat", &with_colour, false),
            ("masks+sat+Clarity+Detail", &with_detail, true),
        ] {
            let committed = apply_scene_to_tilemap(&scene, settings, None).flatten();
            let mut preview = plane_preview(&scene, settings, raw);
            if !plane {
                // The ordinary shader path: Identity colour through the app's
                // proxies on the commit grid, no plane.
                preview.detail = None;
                if !raw && settings.has_color() {
                    let tone =
                        iai::core::develop_scene::build_scene_tone_for_scene(settings, &scene);
                    let s = 6;
                    let (base, pw, ph) = iai::core::develop_scene::build_scene_color_base_box(
                        &scene, 0, 0, width, height, s,
                    );
                    let region = iai::core::develop_scene::tone_lowpass_scene_region(
                        &base, pw, ph, &tone, s,
                    );
                    let adjusted =
                        iai::core::develop::apply_color_to_region(&region, settings, pw, ph);
                    preview.color = Some(ColorProxies {
                        region: Arc::new(region),
                        adjusted: Arc::new(adjusted),
                        w: pw,
                        h: ph,
                        origin_x: 0,
                        origin_y: 0,
                        downsample: s as u32,
                        fast_preview: false,
                        guided_controls: false,
                        exact_detail: false,
                    });
                }
            }
            compositor.develop_preview = Some(preview);
            let is_ping = compositor
                .composite_layers(&device, &queue, &stack, 0.0, 0.0, 1.0, None, false, false);
            let gpu = compositor.readback_rgba8(&device, &queue, is_ping);
            let mut errors: Vec<u8> = gpu
                .chunks_exact(4)
                .zip(committed.chunks_exact(4))
                .flat_map(|(a, b)| (0..3).map(move |c| a[c].abs_diff(b[c])))
                .collect();
            errors.sort_unstable();
            let max = *errors.last().unwrap();
            let p99 = errors[errors.len() * 99 / 100];
            let outliers = errors.iter().filter(|&&e| e > 3).count();
            eprintln!(
                "{} GPU {label}: preview/commit max={max}/255 p99={p99}/255 outliers={outliers}",
                if raw { "RAW " } else { "JPEG" }
            );
            assert!(
                outliers * 10_000 <= errors.len() && p99 <= 1,
                "{label} ({}) preview/commit max {max}/255 p99 {p99}/255, {outliers} outliers",
                if raw { "RAW" } else { "JPEG" }
            );
        }
    }
}

/// Linear-light test value with pixel-scale contrast: a noisy mid-grey with
/// dark and bright specks, the texture a zoomed-out view must not alias.
fn speckled_rgb(x: u32, y: u32) -> [f32; 3] {
    let h = hash01(x, y);
    let base = if h < 0.3 {
        0.012
    } else if h > 0.8 {
        0.62
    } else {
        0.09 + 0.1 * h
    };
    [
        base * (0.9 + 0.2 * hash01(x + 11, y)),
        base,
        base * (0.85 + 0.3 * hash01(x, y + 7)),
    ]
}

/// Mean of channel `c` over the grid pixels `xs × ys` of an RGBA8 image.
fn grid_mean(img: &[u8], width: u32, xs: &[u32], ys: &[u32], c: usize) -> f32 {
    let mut sum = 0.0;
    for &y in ys {
        for &x in xs {
            sum += img[((y * width + x) * 4) as usize + c] as f32;
        }
    }
    sum / (xs.len() * ys.len()) as f32
}

/// Zoomed out, a screen pixel averages its display grid of real pixels (the
/// whole 2×2 block at 50 %) instead of point-sampling one — for a plain layer, and for the
/// Develop preview, which must then equal the same grid of its commit.
#[test]
fn headless_zoomed_out_display_averages_the_pixel_grid() {
    if std::env::var_os("CI").is_some() {
        eprintln!("headless GPU pixel parity is a local real-GPU test; skipped on CI");
        return;
    }
    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no headless GPU adapter; skipped");
        return;
    };
    let (width, height) = (256u32, 128u32);
    let mut scene = SceneSource::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let rgb = speckled_rgb(x, y);
            scene.set_rgb(x, y, scene.color_pipeline.working.from_linear_srgb(rgb));
        }
    }
    let scene = Arc::new(scene);
    let neutral8: Vec<u8> = render_default_look(&scene)
        .iter()
        .map(|v| (v >> 8) as u8)
        .collect();
    let mut stack = LayerStack::new(width, height);
    stack.layers[0] = Layer::from_rgba(0, "Background", neutral8, width, height);
    let layer_px = stack.layers[0].tiles.flatten();
    let max_texture = device.limits().max_texture_dimension_2d;
    let (vw, vh) = (width / 2, height / 2);
    let mut compositor = CompositorState::new(&device, vw, vh, max_texture);
    // At 50 % screen pixel (sx, sy) covers layer px [2sx, 2sx + 2): its grid
    // is the whole 2×2 block.
    let check = |gpu: &[u8], reference: &[u8], label: &str| {
        let mut max_error = 0.0f32;
        for sy in 0..vh {
            for sx in 0..vw {
                let xs = [2 * sx, 2 * sx + 1];
                let ys = [2 * sy, 2 * sy + 1];
                for c in 0..3 {
                    let expected = grid_mean(reference, width, &xs, &ys, c);
                    let got = gpu[((sy * vw + sx) * 4) as usize + c] as f32;
                    max_error = max_error.max((got - expected).abs());
                }
            }
        }
        eprintln!("zoomed-out {label}/grid mean max={max_error:.2}/255");
        assert!(
            max_error <= 2.0,
            "zoomed-out {label} max error {max_error}/255"
        );
    };

    let is_ping =
        compositor.composite_layers(&device, &queue, &stack, 0.0, 0.0, 0.5, None, false, false);
    check(
        &compositor.readback_rgba8(&device, &queue, is_ping),
        &layer_px,
        "layer",
    );

    let settings = DevelopSettings {
        exposure: 15.0,
        contrast: 20.0,
        saturation: 25.0,
        ..Default::default()
    };
    let committed = apply_scene_to_tilemap(&scene, &settings, None).flatten();
    compositor.develop_preview = Some(DevelopGpuPreview {
        layer_id: 0,
        settings,
        region_luma: None,
        color: None,
        scene: Some(scene),
        detail: None,
    });
    let is_ping =
        compositor.composite_layers(&device, &queue, &stack, 0.0, 0.0, 0.5, None, false, false);
    check(
        &compositor.readback_rgba8(&device, &queue, is_ping),
        &committed,
        "Develop preview",
    );
}

/// A zoomed-out Detail plane texel stands for its block the way the display
/// shows the committed block: the chain runs on each grid pixel and the
/// results are averaged — never the other way round, which brightened fine
/// light/dark texture (hair, feathers) far past the commit.
#[test]
fn headless_zoomed_out_plane_averages_committed_pixels() {
    if std::env::var_os("CI").is_some() {
        eprintln!("headless GPU pixel parity is a local real-GPU test; skipped on CI");
        return;
    }
    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no headless GPU adapter; skipped");
        return;
    };
    // Texture periodic with the 6 px plane block, so every texel is equal and
    // the upsampled plane reads that one value everywhere.
    let (width, height, ds) = (96u32, 48u32, 6u32);
    let block = |x: u32, y: u32| speckled_rgb(x % ds, y % ds);
    let max_texture = device.limits().max_texture_dimension_2d;
    for raw in [false, true] {
        let scene = if raw {
            let mut scene = SceneSource::new(width, height);
            for y in 0..height {
                for x in 0..width {
                    let rgb = block(x, y);
                    scene.set_rgb(x, y, scene.color_pipeline.working.from_linear_srgb(rgb));
                }
            }
            scene
        } else {
            let mut px = Vec::with_capacity((width * height * 4) as usize);
            for y in 0..height {
                for x in 0..width {
                    for c in block(x, y) {
                        px.push(srgb8(c));
                    }
                    px.push(255);
                }
            }
            SceneSource::from_display_tiles(&iai::core::tile::TileMap::from_rgba(
                &px, width, height,
            ))
        };
        let scene = Arc::new(scene);
        let neutral8: Vec<u8> = render_default_look(&scene)
            .iter()
            .map(|v| (v >> 8) as u8)
            .collect();
        let mut stack = LayerStack::new(width, height);
        stack.layers[0] = Layer::from_rgba(0, "Background", neutral8, width, height);
        let settings = DevelopSettings {
            exposure: 10.0,
            ..Default::default()
        };
        let committed = apply_scene_to_tilemap(&scene, &settings, None).flatten();
        let tone = iai::core::develop_scene::build_scene_tone_for_scene(&settings, &scene);
        let mut compositor = CompositorState::new(&device, width, height, max_texture);
        compositor.develop_preview = Some(DevelopGpuPreview {
            layer_id: 0,
            settings,
            region_luma: None,
            color: None,
            scene: Some(scene),
            detail: Some(DevelopDetailGpu {
                origin_x: 0,
                origin_y: 0,
                end_x: width,
                end_y: height,
                downsample: ds,
                linear: raw,
                luma_coeff: if raw {
                    tone.working_space.render_luminance_coefficients()
                } else {
                    [0.2126, 0.7152, 0.0722]
                },
                run_detail: false,
            }),
        });
        let is_ping =
            compositor.composite_layers(&device, &queue, &stack, 0.0, 0.0, 1.0, None, false, false);
        let gpu = compositor.readback_rgba8(&device, &queue, is_ping);
        // A 6 px block's grid: 3 taps per axis at offsets 1, 3, 5.
        let taps = [1, 3, 5];
        let mut max_error = 0.0f32;
        for c in 0..3 {
            let expected = grid_mean(&committed, width, &taps, &taps, c);
            for p in gpu.chunks_exact(4) {
                max_error = max_error.max((p[c] as f32 - expected).abs());
            }
        }
        eprintln!(
            "{} zoomed-out plane/commit grid max={max_error:.2}/255",
            if raw { "RAW " } else { "JPEG" }
        );
        assert!(
            max_error <= 2.0,
            "zoomed-out plane max error {max_error}/255"
        );
    }
}

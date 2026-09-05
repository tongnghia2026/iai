use iai::core::develop::{DevelopEngineVersion, DevelopSettings};
use iai::core::develop_scene::{apply_scene_to_tilemap, eval_scene_pixel, BaseLook, SceneSource};

#[test]
fn scene_v1_neutral_reference_values_are_stable() {
    let mut settings = DevelopSettings::default();
    settings.develop_engine_version = DevelopEngineVersion::Scene1;
    let cases = [
        ([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]),
        ([0.18, 0.18, 0.18], [0.459_298_55; 3]),
        ([0.5, 0.5, 0.5], [0.768_629_43; 3]),
        ([1.0, 1.0, 1.0], [0.906_403_36; 3]),
    ];
    for (input, expected) in cases {
        let actual = eval_scene_pixel(input, &settings, BaseLook::Raw);
        for channel in 0..3 {
            assert!(
                (actual[channel] - expected[channel]).abs() <= 2.0e-6,
                "iai-scene-v1 golden changed for {input:?}: {actual:?} != {expected:?}"
            );
        }
    }
}

#[test]
fn serialized_develop2_snapshot_migrates_to_develop3_without_a_stale_engine() {
    let mut value = serde_json::to_value(DevelopSettings::default()).unwrap();
    value["develop_engine_version"] = serde_json::json!("Develop2");
    let migrated: DevelopSettings = serde_json::from_value(value).unwrap();
    assert_eq!(
        migrated.develop_engine_version,
        DevelopEngineVersion::Develop3
    );
    assert_eq!(
        serde_json::to_value(&migrated).unwrap()["develop_engine_version"],
        "Develop3"
    );
}

#[test]
fn every_engine_version_roundtrips_without_renderer_drift() {
    let mut scene = SceneSource::new(12, 8);
    for y in 0..8 {
        for x in 0..12 {
            let xf = x as f32 / 11.0;
            let yf = y as f32 / 7.0;
            scene.set_rgb(
                x,
                y,
                [0.03 + 0.52 * xf, 0.02 + 0.31 * yf, 0.04 + 0.18 * xf * yf],
            );
        }
    }

    for engine in [
        DevelopEngineVersion::Legacy1,
        DevelopEngineVersion::Scene1,
        DevelopEngineVersion::Develop3,
    ] {
        let settings = DevelopSettings {
            develop_engine_version: engine,
            exposure: 8.0,
            contrast: 17.0,
            shadows: 13.0,
            temperature: 9.0,
            tint: -6.0,
            saturation: 12.0,
            curve_lights: 7.0,
            ..Default::default()
        };
        let before = apply_scene_to_tilemap(&scene, &settings, None).flatten16();
        let json = serde_json::to_vec(&settings).expect("serialize Develop settings");
        let reopened: DevelopSettings =
            serde_json::from_slice(&json).expect("reopen Develop settings");
        assert_eq!(reopened.develop_engine_version, engine);
        let after = apply_scene_to_tilemap(&scene, &reopened, None).flatten16();
        assert_eq!(after, before, "renderer drift after {engine:?} reopen");
    }
}

#[test]
fn develop3_locked_recipe_bitmap_golden_is_stable() {
    let (w, h) = (24u32, 16u32);
    let mut scene = SceneSource::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let xf = x as f32 / (w - 1) as f32;
            let yf = y as f32 / (h - 1) as f32;
            let texture = 0.012 * (x as f32 * 0.73).sin() + 0.009 * (y as f32 * 0.51).cos();
            scene.set_rgb(
                x,
                y,
                [
                    (0.018 + 0.76 * xf + texture).max(0.0),
                    (0.012 + 0.48 * yf - texture * 0.35).max(0.0),
                    (0.025 + 0.31 * (1.0 - xf) + 0.16 * xf * yf + texture * 0.6).max(0.0),
                ],
            );
        }
    }

    // Moderate values span Develop3's Light, perceptual colour, guided Mixer,
    // Curve and Detail stages. This is an internal look-freeze guard: any
    // intentional tuning must update the hash with an accompanying visual note.
    let settings = DevelopSettings {
        develop_engine_version: DevelopEngineVersion::Develop3,
        exposure: 7.0,
        contrast: 14.0,
        highlights: -19.0,
        shadows: 16.0,
        midtones: 9.0,
        whites: 6.0,
        blacks: -8.0,
        temperature: 5.0,
        tint: -3.0,
        vibrance: 17.0,
        saturation: 8.0,
        texture: 11.0,
        clarity: 7.0,
        sharpening: 24.0,
        sharpen_detail: 31.0,
        sharpen_masking: 18.0,
        noise_reduction: 8.0,
        color_noise_reduction: 10.0,
        curve_highlights: -6.0,
        curve_lights: 8.0,
        curve_darks: -5.0,
        curve_shadows: 4.0,
        mixer_hue: [9.0, -5.0, 4.0, 0.0, -3.0, 6.0, -4.0, 7.0],
        mixer_saturation: [13.0, -8.0, 6.0, 3.0, -5.0, 7.0, -4.0, 9.0],
        mixer_luminance: [4.0, -3.0, 5.0, 0.0, -2.0, 3.0, -4.0, 2.0],
        ..Default::default()
    };
    let pixels = apply_scene_to_tilemap(&scene, &settings, None).flatten16();
    let mut channel_sums = [0u64; 3];
    let mut channel_square_sums = [0u128; 3];
    let mut channel_edge_energy = [0u64; 3];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let pixel = (y * w as usize + x) * 4;
            for channel in 0..3 {
                let value = pixels[pixel + channel];
                channel_sums[channel] += value as u64;
                channel_square_sums[channel] += value as u128 * value as u128;
                if x > 0 {
                    channel_edge_energy[channel] +=
                        value.abs_diff(pixels[pixel + channel - 4]) as u64;
                }
                if y > 0 {
                    channel_edge_energy[channel] +=
                        value.abs_diff(pixels[pixel + channel - w as usize * 4]) as u64;
                }
            }
        }
    }
    println!(
        "Develop3 golden metrics: sums={channel_sums:?}, squares={channel_square_sums:?}, edges={channel_edge_energy:?}"
    );

    // Byte hashes are unsuitable here: platform libm/SIMD changes low 16-bit
    // codes without a visible image change. These aggregates still freeze the
    // overall tone, colour and spatial detail, with sub-visual tolerances.
    const EXPECTED_SUMS: [u64; 3] = [17_816_550, 13_412_161, 14_110_038];
    const EXPECTED_SQUARE_SUMS: [u128; 3] = [1_049_288_629_928, 611_156_722_617, 560_138_900_596];
    const EXPECTED_EDGE_ENERGY: [u64; 3] = [1_590_202, 1_652_983, 1_072_626];
    for channel in 0..3 {
        assert!(
            channel_sums[channel].abs_diff(EXPECTED_SUMS[channel]) <= 8_192,
            "Develop3 channel {channel} tone/colour sum changed: {}",
            channel_sums[channel]
        );
        assert!(
            channel_square_sums[channel].abs_diff(EXPECTED_SQUARE_SUMS[channel]) <= 1_000_000_000,
            "Develop3 channel {channel} tone distribution changed: {}",
            channel_square_sums[channel]
        );
        assert!(
            channel_edge_energy[channel].abs_diff(EXPECTED_EDGE_ENERGY[channel]) <= 8_192,
            "Develop3 channel {channel} detail energy changed: {}",
            channel_edge_energy[channel]
        );
    }
}

#[test]
fn pre_versioned_snapshot_reopens_as_scene1_bit_exact() {
    let mut explicit = DevelopSettings {
        develop_engine_version: DevelopEngineVersion::Scene1,
        exposure: 11.0,
        contrast: 9.0,
        saturation: 15.0,
        ..Default::default()
    };
    // Fields introduced with Develop2 were absent from old project/preset JSON.
    let mut value = serde_json::to_value(&explicit).unwrap();
    let object = value.as_object_mut().unwrap();
    object.remove("develop_engine_version");
    object.remove("tone_map_mode");
    object.remove("point_curve_mode");
    object.remove("mixer_algorithm");
    let reopened: DevelopSettings = serde_json::from_value(value).unwrap();
    assert_eq!(
        reopened.develop_engine_version,
        DevelopEngineVersion::Scene1
    );

    explicit.tone_map_mode = reopened.tone_map_mode;
    explicit.point_curve_mode = reopened.point_curve_mode;
    explicit.mixer_algorithm = reopened.mixer_algorithm;
    let mut scene = SceneSource::new(4, 2);
    for y in 0..2 {
        for x in 0..4 {
            let v = 0.04 + (x + y * 4) as f32 * 0.08;
            scene.set_rgb(x, y, [v, v * 0.72, v * 0.48]);
        }
    }
    assert_eq!(
        apply_scene_to_tilemap(&scene, &reopened, None).flatten16(),
        apply_scene_to_tilemap(&scene, &explicit, None).flatten16(),
        "pre-versioned settings must retain the Scene1 renderer"
    );
}

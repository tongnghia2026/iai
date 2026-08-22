use iai::core::develop::{DevelopEngineVersion, DevelopSettings};
use iai::core::develop_scene::{
    apply_scene_to_tilemap, eval_scene_pixel, eval_scene_pixel_for_scene, BaseLook, SceneSource,
};

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
fn develop2_wide_working_reference_values_are_stable() {
    let inputs = [
        [0.18, 0.18, 0.18],
        [0.70, 0.02, 0.01],
        [0.01, 0.55, 0.04],
        [-0.04, 0.12, 1.35],
    ];
    let mut scene = SceneSource::new(inputs.len() as u32, 1);
    for (x, input) in inputs.into_iter().enumerate() {
        scene.set_rgb(
            x as u32,
            0,
            scene.color_pipeline.working.from_linear_srgb(input),
        );
    }
    let settings = DevelopSettings {
        saturation: 65.0,
        vibrance: 30.0,
        mixer_hue: [18.0, -7.0, 0.0, 0.0, 0.0, 0.0, 0.0, 9.0],
        mixer_saturation: [22.0, -12.0, 0.0, 0.0, 0.0, 0.0, 0.0, 8.0],
        ..Default::default()
    };
    let actual: Vec<[f32; 3]> = (0..inputs.len())
        .map(|x| eval_scene_pixel_for_scene(&scene, scene.get_rgb(x as u32, 0), &settings))
        .collect();
    let expected = [
        [0.458_745_3, 0.458_744_6, 0.458_744_88],
        [0.584_970_36, 0.430_418_37, 0.001_601_021_3],
        [0.144_731_07, 0.810_542_3, 0.003_895_895_6],
        [0.131_268_14, 0.271_369_58, 0.999_999_94],
    ];
    for (case, (actual, expected)) in actual.into_iter().zip(expected).enumerate() {
        for channel in 0..3 {
            assert!(
                (actual[channel] - expected[channel]).abs() <= 2.0e-6,
                "Develop2 wide-working golden changed for case {case}: {actual:?} != {expected:?}"
            );
        }
    }
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
        DevelopEngineVersion::Develop2,
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

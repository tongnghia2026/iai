use iai::core::develop::DevelopSettings;
use iai::core::develop_scene::{eval_scene_pixel, BaseLook};

const PROBES: [[f32; 3]; 9] = [
    [0.0, 0.0, 0.0],
    [0.18, 0.18, 0.18],
    [1.0, 1.0, 1.0],
    [0.64, 0.06, 0.03],
    [0.75, 0.25, 0.03],
    [0.05, 0.55, 0.08],
    [0.03, 0.08, 0.8],
    [-0.05, 0.2, 0.3],
    [0.4, 1.2, 3.0],
];

#[test]
fn scene_pixel_probe_is_finite_and_deterministic() {
    let settings = DevelopSettings::default();
    for input in PROBES {
        let a = eval_scene_pixel(input, &settings, BaseLook::Raw);
        let b = eval_scene_pixel(input, &settings, BaseLook::Raw);
        assert_eq!(a, b, "non-deterministic output for {input:?}");
        assert!(
            a.iter().all(|v| v.is_finite()),
            "non-finite output for {input:?}: {a:?}"
        );
    }
}

#[test]
fn neutral_greys_remain_neutral() {
    for value in [0.0, 0.001, 0.18, 0.5, 1.0, 4.0] {
        let out = eval_scene_pixel([value; 3], &DevelopSettings::default(), BaseLook::Raw);
        let spread = out.iter().copied().fold(f32::NEG_INFINITY, f32::max)
            - out.iter().copied().fold(f32::INFINITY, f32::min);
        assert!(spread <= 1.0e-5, "neutral axis drift at {value}: {out:?}");
    }
}

#[test]
fn exposure_extremes_are_monotone_on_neutral_ramp() {
    for input in [[0.02; 3], [0.18; 3], [0.7; 3]] {
        let mut previous = f32::NEG_INFINITY;
        for exposure in [-100.0, -50.0, 0.0, 50.0, 100.0] {
            let settings = DevelopSettings {
                exposure,
                ..Default::default()
            };
            let out = eval_scene_pixel(input, &settings, BaseLook::Raw);
            assert!(
                out[1] + 1.0e-6 >= previous,
                "exposure inversion for {input:?} at {exposure}: {out:?}"
            );
            previous = out[1];
        }
    }
}

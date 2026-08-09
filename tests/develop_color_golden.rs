use iai::core::develop::DevelopSettings;
use iai::core::develop_scene::{eval_scene_pixel, BaseLook};

#[test]
fn scene_v1_neutral_reference_values_are_stable() {
    let cases = [
        ([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]),
        ([0.18, 0.18, 0.18], [0.459_298_55; 3]),
        ([0.5, 0.5, 0.5], [0.768_629_43; 3]),
        ([1.0, 1.0, 1.0], [0.906_403_36; 3]),
    ];
    for (input, expected) in cases {
        let actual = eval_scene_pixel(input, &DevelopSettings::default(), BaseLook::Raw);
        for channel in 0..3 {
            assert!(
                (actual[channel] - expected[channel]).abs() <= 2.0e-6,
                "iai-scene-v1 golden changed for {input:?}: {actual:?} != {expected:?}"
            );
        }
    }
}

//! Phase-0 renderer banding baseline on a deterministic 16-bit neutral ramp.

use iai::core::color_reference::analyze_ramp;
use iai::core::develop_scene::{render_default_look, SceneSource};
use iai::core::working_color::WorkingColorSpace;

#[test]
fn default_renderer_neutral_gradient_banding_baseline() {
    const SAMPLE_COUNT: u32 = 4096;
    let mut scene = SceneSource::new(SAMPLE_COUNT, 1);
    for x in 0..SAMPLE_COUNT {
        let value = x as f32 / (SAMPLE_COUNT - 1) as f32;
        scene.set_rgb(
            x,
            0,
            WorkingColorSpace::LinearProPhoto.from_linear_srgb([value; 3]),
        );
    }

    let rendered = render_default_look(&scene);
    let encoded_luma = rendered
        .chunks_exact(4)
        .map(|pixel| {
            (0.212_672_9 * pixel[0] as f32
                + 0.715_152_2 * pixel[1] as f32
                + 0.072_175 * pixel[2] as f32)
                / 65_535.0
        })
        .collect::<Vec<_>>();
    let metrics = analyze_ramp(&encoded_luma, 65_535);
    let codes = encoded_luma
        .iter()
        .map(|value| (value * 65_535.0).round() as u16)
        .collect::<Vec<_>>();
    let longest_plateau = codes
        .chunk_by(|first, second| first == second)
        .map(<[u16]>::len)
        .max()
        .unwrap_or_default();
    let trailing_plateau = codes
        .iter()
        .rev()
        .take_while(|&&code| code == codes[codes.len() - 1])
        .count();
    println!(
        "neutral-render-ramp,samples={SAMPLE_COUNT},levels={},reversals={},longest_plateau={},trailing_plateau={},first_code={},last_code={},mean_step={:.9},step_variance={:.12},max_abs_step={:.9}",
        metrics.distinct_quantized_levels,
        metrics.reversals,
        longest_plateau,
        trailing_plateau,
        codes[0],
        codes[codes.len() - 1],
        metrics.mean_step,
        metrics.step_variance,
        metrics.max_abs_step,
    );

    assert_eq!(
        metrics.reversals, 0,
        "neutral render ramp must stay monotone"
    );
    assert!(
        (metrics.distinct_quantized_levels as isize - 3073).abs() <= 1,
        "neutral render ramp baseline drifted to {} levels",
        metrics.distinct_quantized_levels,
    );
    assert!(
        metrics.max_abs_step <= 0.0012,
        "neutral render ramp contains an excessive code jump: {}",
        metrics.max_abs_step
    );
    assert_eq!(longest_plateau, 2, "neutral ramp plateau baseline drifted");
    assert_eq!(trailing_plateau, 1, "neutral ramp tail became flat");
    assert_eq!(codes[0], 0, "neutral ramp black endpoint drifted");
    assert!(
        codes[codes.len() - 1].abs_diff(60_612) <= 1,
        "neutral ramp white endpoint drifted to {}",
        codes[codes.len() - 1]
    );
}

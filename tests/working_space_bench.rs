use iai::core::working_color::WorkingColorSpace;
use std::time::Instant;

#[test]
#[ignore = "manual release benchmark for the Phase 1 ADR"]
fn benchmark_acescg_vs_linear_prophoto() {
    let vectors: Vec<[f32; 3]> = (0..500_000)
        .map(|i| {
            let t = i as f32 / 499_999.0;
            [
                t * 4.0 - 0.15,
                (t * 31.0).sin() * 0.6 + 0.3,
                (1.0 - t) * 3.0,
            ]
        })
        .collect();
    for space in [WorkingColorSpace::AcesCg, WorkingColorSpace::LinearProPhoto] {
        let started = Instant::now();
        let mut max_error = 0.0f32;
        let mut negative_channels = 0usize;
        let mut checksum = 0.0f64;
        for input in &vectors {
            let working = space.from_linear_srgb(*input);
            negative_channels += working.iter().filter(|&&value| value < 0.0).count();
            let output = space.to_linear_srgb(working);
            for channel in 0..3 {
                max_error = max_error.max((output[channel] - input[channel]).abs());
                checksum += output[channel] as f64;
            }
        }
        eprintln!(
            "space={},vectors={},elapsed_ms={:.3},max_roundtrip_error={:.8},negative_channels={},checksum={:.3}",
            space.name(),
            vectors.len(),
            started.elapsed().as_secs_f64() * 1000.0,
            max_error,
            negative_channels,
            checksum
        );
        assert!(max_error < 0.001);
    }
}

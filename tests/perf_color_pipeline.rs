use iai::core::develop::DevelopSettings;
use iai::core::develop_scene::{apply_scene_to_tilemap, SceneSource};
use std::time::Instant;

#[test]
#[ignore = "manual release benchmark; allocates up to a 45 MP scene"]
fn benchmark_synthetic_12_24_45_mp_commit() {
    eprintln!(
        "os={} arch={}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let settings = DevelopSettings {
        exposure: 20.0,
        highlights: -35.0,
        shadows: 30.0,
        saturation: 25.0,
        mixer_saturation: [20.0, -10.0, 15.0, 0.0, 10.0, 25.0, -5.0, 10.0],
        ..Default::default()
    };
    for (label, width, height) in [
        ("12MP", 4000, 3000),
        ("24MP", 6000, 4000),
        ("45MP", 8256, 5504),
    ] {
        let mut scene = SceneSource::new(width, height);
        for y in 0..height {
            for x in 0..width {
                let fx = x as f32 / width as f32;
                let fy = y as f32 / height as f32;
                scene.set_rgb(x, y, [fx * 1.5 - 0.05, fy, (1.0 - fx) * 2.0]);
            }
        }
        let started = Instant::now();
        let output = apply_scene_to_tilemap(&scene, &settings, None);
        eprintln!(
            "{label},{width}x{height},commit_ms={:.3},output_tiles={}",
            started.elapsed().as_secs_f64() * 1000.0,
            output.tiles.len()
        );
    }
}

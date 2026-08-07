use std::time::{Duration, Instant};

use iai::core::geometry::Point;
use iai::core::vector::object::VectorObjectData;
use iai::core::vector::path::{Contour, FillRule, Node, PathData};
use iai::gpu::vector::mesh::tessellate;

fn flower(seed: usize, rule: FillRule) -> VectorObjectData {
    let cx = (seed % 25) as f32 * 48.0 + 24.0;
    let cy = (seed / 25) as f32 * 48.0 + 24.0;
    let mut nodes = Vec::with_capacity(20);
    for i in 0..20 {
        let a = i as f32 * std::f32::consts::TAU / 20.0;
        let r = if i % 2 == 0 { 20.0 } else { 8.0 };
        nodes.push(Node::sharp(Point::new(cx + r * a.cos(), cy + r * a.sin())));
    }
    let mut contours = vec![Contour::new(nodes, true)];
    // A deterministic counter-wound inner contour exercises holes and mirrors a
    // Text→Curves glyph counter.
    let mut counter = vec![
        Node::sharp(Point::new(cx - 3.0, cy - 3.0)),
        Node::sharp(Point::new(cx - 3.0, cy + 3.0)),
        Node::sharp(Point::new(cx + 3.0, cy + 3.0)),
        Node::sharp(Point::new(cx + 3.0, cy - 3.0)),
    ];
    if rule == FillRule::EvenOdd {
        counter.reverse();
    }
    contours.push(Contour::new(counter, true));
    VectorObjectData::from_path(PathData::new(contours, rule))
}

fn timed(mut f: impl FnMut(), repeats: u32) -> Duration {
    let start = Instant::now();
    for _ in 0..repeats {
        f();
    }
    start.elapsed() / repeats
}

#[test]
#[ignore = "manual release benchmark; run with --ignored --nocapture"]
fn phase0_and_mesh_cache_fixture() {
    for count in [10usize, 100, 300, 500] {
        let objects: Vec<_> = (0..count)
            .map(|i| {
                flower(
                    i,
                    if i % 2 == 0 {
                        FillRule::NonZero
                    } else {
                        FillRule::EvenOdd
                    },
                )
            })
            .collect();
        let all = timed(
            || {
                for object in &objects {
                    std::hint::black_box(tessellate(object, 0.1).unwrap());
                }
            },
            5,
        );
        let one = timed(
            || {
                std::hint::black_box(tessellate(&objects[count / 2], 0.1).unwrap());
            },
            50,
        );
        let benefit = 100.0 * (1.0 - one.as_secs_f64() / all.as_secs_f64());
        println!(
            "layers={count}, whole_run_us={}, one_object_us={}, avoided_work_percent={benefit:.2}",
            all.as_micros(),
            one.as_micros()
        );
    }
}

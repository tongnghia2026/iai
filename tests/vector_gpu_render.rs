//! Phase 1 GPU vector proof-of-concept snapshot tests.
//!
//! These render tessellated fills on a real GPU and compare against the CPU
//! rasteriser (`core::vector::raster`), which is the reference per the Phase 0
//! metric. They are **local/manual** — `#[ignore]` keeps them off headless CI
//! (which usually has no adapter and burns Actions minutes). Run with:
//!
//! ```text
//! cargo test --test vector_gpu_render -- --ignored --nocapture
//! ```
//!
//! Comparison follows the fixed Phase 0 thresholds: interior pixels (fully
//! covered, ≥1 px from an edge) must match the reference colour within a small
//! linear tolerance; clearly-exterior pixels (a hole counts as exterior) must be
//! transparent. The ±1 px anti-aliasing band is intentionally excluded — GPU MSAA
//! never matches the CPU coverage ramp there, and the plan does not require it to.

use iai::core::geometry::Point;
use iai::core::vector::affine::AffineTransform;
use iai::core::vector::color::ColorValue;
use iai::core::vector::object::VectorObjectData;
use iai::core::vector::path::{Contour, FillRule, Node, NodeKind, PathData};
use iai::core::vector::raster::{rasterize, PathRaster};
use iai::core::vector::style::{Gradient, GradientKind, Paint, VectorStyle};
use iai::gpu::vector::mesh::tessellate;
use iai::gpu::vector::renderer::{
    srgb_to_linear, CanvasView, GpuMesh, GpuPaint, VectorDraw, VectorRenderer,
};

/// sRGB fill applied to every fixture; a mid-tone exercises the gamma round-trip
/// harder than pure black/white.
fn fill_color() -> ColorValue {
    ColorValue::rgb(0.10, 0.55, 0.85)
}

fn styled(path: PathData, transform: AffineTransform) -> VectorObjectData {
    VectorObjectData::new(path, VectorStyle::filled(fill_color()), transform)
}

fn square(side: f32) -> PathData {
    PathData::new(
        vec![Contour::new(
            vec![
                Node::sharp(Point::new(0.0, 0.0)),
                Node::sharp(Point::new(side, 0.0)),
                Node::sharp(Point::new(side, side)),
                Node::sharp(Point::new(0.0, side)),
            ],
            true,
        )],
        FillRule::NonZero,
    )
}

/// A concave arrow (a chevron notch cut into a rectangle) — its interior is not
/// convex, so a wrong triangulation would fill the notch.
fn concave() -> PathData {
    let pts = [
        (0.0, 0.0),
        (60.0, 0.0),
        (60.0, 60.0),
        (30.0, 30.0),
        (0.0, 60.0),
    ];
    PathData::new(
        vec![Contour::new(
            pts.iter()
                .map(|&(x, y)| Node::sharp(Point::new(x, y)))
                .collect(),
            true,
        )],
        FillRule::NonZero,
    )
}

fn ring(cx: f32, cy: f32, r: f32, ccw: bool) -> Vec<Node> {
    let mut pts = [
        (cx - r, cy - r),
        (cx + r, cy - r),
        (cx + r, cy + r),
        (cx - r, cy + r),
    ];
    if ccw {
        pts.reverse();
    }
    pts.iter()
        .map(|&(x, y)| Node::sharp(Point::new(x, y)))
        .collect()
}

/// Outer box + counter-wound inner box: a NonZero hole (winding cancels).
fn compound_nonzero() -> PathData {
    PathData::new(
        vec![
            Contour::new(ring(30.0, 30.0, 28.0, false), true),
            Contour::new(ring(30.0, 30.0, 12.0, true), true),
        ],
        FillRule::NonZero,
    )
}

/// Outer box + same-wound inner box: an EvenOdd hole (parity).
fn compound_evenodd() -> PathData {
    PathData::new(
        vec![
            Contour::new(ring(30.0, 30.0, 28.0, false), true),
            Contour::new(ring(30.0, 30.0, 12.0, false), true),
        ],
        FillRule::EvenOdd,
    )
}

/// A 4-cubic ellipse, exercising the `cubic_bezier_to` tessellation path.
fn ellipse(rx: f32, ry: f32) -> PathData {
    const K: f32 = 0.552_284_75; // 4/3 * (sqrt(2)-1)
    let (cx, cy) = (rx, ry);
    let node = |ax: f32, ay: f32, ix: f32, iy: f32, ox: f32, oy: f32| {
        Node::with_handles(
            Point::new(ax, ay),
            Point::new(ix, iy),
            Point::new(ox, oy),
            NodeKind::Smooth,
        )
    };
    // East, South, West, North anchors with tangent handles.
    let nodes = vec![
        node(cx + rx, cy, cx + rx, cy - ry * K, cx + rx, cy + ry * K),
        node(cx, cy + ry, cx + rx * K, cy + ry, cx - rx * K, cy + ry),
        node(cx - rx, cy, cx - rx, cy + ry * K, cx - rx, cy - ry * K),
        node(cx, cy - ry, cx - rx * K, cy - ry, cx + rx * K, cy - ry),
    ];
    PathData::new(vec![Contour::new(nodes, true)], FillRule::NonZero)
}

/// Approximate uniform scale of a transform, so the object-space flatten
/// tolerance produces the same smoothness the CPU gets in layer space.
fn transform_scale(t: &AffineTransform) -> f32 {
    (t.a * t.d - t.b * t.c).abs().sqrt().max(1e-3)
}

struct Compare {
    interior: usize,
    interior_bad: usize,
    exterior_bad: usize,
}

fn alpha(rgba: &[u8], w: u32, x: i32, y: i32, h: u32) -> Option<u8> {
    if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
        return None;
    }
    Some(rgba[((y as u32 * w + x as u32) * 4 + 3) as usize])
}

/// Classify every pixel by the CPU reference and check the GPU output there.
fn compare(cpu: &PathRaster, gpu: &[u8]) -> Compare {
    let (w, h) = (cpu.width, cpu.height);
    let a = |rgba: &[u8], x: i32, y: i32| alpha(rgba, w, x, y, h);
    let mut out = Compare {
        interior: 0,
        interior_bad: 0,
        exterior_bad: 0,
    };
    let neighbors = [(-1, 0), (1, 0), (0, -1), (0, 1)];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let ca = a(&cpu.rgba, x, y).unwrap();
            let idx = ((y as u32 * w + x as u32) * 4) as usize;
            if ca == 255
                && neighbors
                    .iter()
                    .all(|&(dx, dy)| a(&cpu.rgba, x + dx, y + dy) == Some(255))
            {
                out.interior += 1;
                let ga = gpu[idx + 3];
                let mut colour_ok = ga >= 250;
                for c in 0..3 {
                    let dl = (srgb_to_linear(gpu[idx + c] as f32 / 255.0)
                        - srgb_to_linear(cpu.rgba[idx + c] as f32 / 255.0))
                    .abs();
                    if dl > 2.0 / 255.0 {
                        colour_ok = false;
                    }
                }
                if !colour_ok {
                    out.interior_bad += 1;
                }
            } else if ca == 0
                && neighbors
                    .iter()
                    .all(|&(dx, dy)| a(&cpu.rgba, x + dx, y + dy) == Some(0))
            {
                // A hole is exterior too: the GPU must not fill it.
                if gpu[idx + 3] > 6 {
                    out.exterior_bad += 1;
                }
            }
        }
    }
    out
}

/// Render `object` on the GPU into the exact frame the CPU reference produced and
/// assert interior colour + hole/exterior coverage agree.
fn assert_matches_reference(
    renderer: &VectorRenderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    object: &VectorObjectData,
) {
    let cpu = rasterize(object).expect("reference raster");
    let tol = 0.1 / transform_scale(&object.transform);
    let mesh = GpuMesh::upload(device, &tessellate(object, tol).expect("tessellate"));
    let view = CanvasView::tight(
        cpu.width,
        cpu.height,
        cpu.offset.0 as f32,
        cpu.offset.1 as f32,
    );
    let draws = [VectorDraw {
        mesh: &mesh,
        object_to_canvas: object.transform,
        fill: GpuPaint::from_model(object.style.fill),
        stroke: None,
        opacity: object.style.opacity,
    }];
    let gpu = renderer.render_offscreen(device, queue, view, &draws);
    let c = compare(&cpu, &gpu);
    assert!(
        c.interior > 40,
        "{label}: too few interior pixels ({}) — shape did not render",
        c.interior
    );
    assert_eq!(
        c.interior_bad, 0,
        "{label}: {} interior pixels differ from the reference colour",
        c.interior_bad
    );
    assert_eq!(
        c.exterior_bad, 0,
        "{label}: {} exterior/hole pixels are wrongly filled (fill-rule error)",
        c.exterior_bad
    );
    eprintln!(
        "{label}: {}x{} interior={} ok",
        cpu.width, cpu.height, c.interior
    );
}

#[test]
#[ignore = "local GPU snapshot; run with --ignored --nocapture"]
fn gpu_fill_matches_cpu_reference() {
    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no GPU adapter available; skipping GPU render snapshot");
        return;
    };
    let renderer = VectorRenderer::new(
        &device,
        iai::gpu::vector::renderer::VECTOR_TARGET_FORMAT,
        iai::gpu::vector::renderer::VECTOR_SAMPLE_COUNT,
    );

    let base = AffineTransform::translate(20.0, 20.0);
    let rotated = AffineTransform::translate(40.0, 40.0)
        .then(&AffineTransform::rotate(0.5))
        .then(&AffineTransform::scale(1.5, 0.8));
    let flip = AffineTransform::translate(70.0, 20.0).then(&AffineTransform::scale(-1.0, 1.0));

    let cases: Vec<(&str, VectorObjectData)> = vec![
        ("square", styled(square(40.0), base)),
        ("concave", styled(concave(), base)),
        ("hole_nonzero", styled(compound_nonzero(), base)),
        ("hole_evenodd", styled(compound_evenodd(), base)),
        ("ellipse", styled(ellipse(30.0, 18.0), base)),
        ("rotated_scaled_square", styled(square(30.0), rotated)),
        ("flipped_concave", styled(concave(), flip)),
    ];

    for (label, object) in &cases {
        // Zoom invariance: the same object at 1× and 8× effective scale both match
        // the CPU at that resolution (no re-tessellation policy is Phase 3; here we
        // only assert the render stays correct as the target grows).
        for zoom in [1.0f32, 8.0] {
            let mut zoomed = object.clone();
            zoomed.transform = AffineTransform::scale(zoom, zoom).then(&object.transform);
            assert_matches_reference(
                &renderer,
                &device,
                &queue,
                &format!("{label}@{zoom}x"),
                &zoomed,
            );
        }
    }
}

#[test]
#[ignore = "local GPU snapshot; run with --ignored --nocapture"]
fn gpu_rgb_gradients_match_cpu_reference() {
    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no GPU adapter; skipping gradient snapshots");
        return;
    };
    let renderer = VectorRenderer::new(
        &device,
        iai::gpu::vector::renderer::VECTOR_TARGET_FORMAT,
        iai::gpu::vector::renderer::VECTOR_SAMPLE_COUNT,
    );
    for kind in [GradientKind::Linear, GradientKind::Radial] {
        let mut object = styled(square(48.0), AffineTransform::translate(4.0, 4.0));
        object.style.fill = Paint::Gradient(Gradient::two_color(
            kind,
            ColorValue::rgb(0.1, 0.3, 0.9),
            ColorValue::rgb(0.9, 0.7, 0.1),
            AffineTransform::scale(48.0, 48.0),
        ));
        assert_matches_reference(
            &renderer,
            &device,
            &queue,
            match kind {
                GradientKind::Linear => "linear_gradient",
                GradientKind::Radial => "radial_gradient",
            },
            &object,
        );
    }
}

#[test]
#[ignore = "local GPU snapshot; run with --ignored --nocapture"]
fn gpu_gradient_alpha_stops_match_cpu_alpha() {
    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no GPU adapter; skipping gradient alpha snapshot");
        return;
    };
    let renderer = VectorRenderer::new(
        &device,
        iai::gpu::vector::renderer::VECTOR_TARGET_FORMAT,
        iai::gpu::vector::renderer::VECTOR_SAMPLE_COUNT,
    );
    let mut object = styled(square(48.0), AffineTransform::translate(4.0, 4.0));
    object.style.fill = Paint::Gradient(Gradient::two_color(
        GradientKind::Linear,
        ColorValue::rgba(0.2, 0.6, 0.9, 0.1),
        ColorValue::rgba(0.9, 0.2, 0.3, 0.9),
        AffineTransform::scale(48.0, 48.0),
    ));
    let cpu = rasterize(&object).unwrap();
    let mesh = GpuMesh::upload(&device, &tessellate(&object, 0.1).unwrap());
    let draws = [VectorDraw {
        mesh: &mesh,
        object_to_canvas: object.transform,
        fill: GpuPaint::from_model(object.style.fill),
        stroke: None,
        opacity: 1.0,
    }];
    let gpu = renderer.render_offscreen(
        &device,
        &queue,
        CanvasView::tight(
            cpu.width,
            cpu.height,
            cpu.offset.0 as f32,
            cpu.offset.1 as f32,
        ),
        &draws,
    );
    for x in [12u32, 28, 44] {
        let y = 28u32;
        let i = ((y * cpu.width + x) * 4 + 3) as usize;
        assert!(
            (gpu[i] as i32 - cpu.rgba[i] as i32).abs() <= 3,
            "alpha stop mismatch at x={x}: gpu={} cpu={}",
            gpu[i],
            cpu.rgba[i]
        );
    }
}

/// Phase 6: a parametric primitive (`VectorGeometry::Primitive`) is drawn by
/// converting it to the same `PathData` its raster twin uses. Each shape kind must
/// therefore match the CPU reference exactly, just like a hand-authored Path.
#[test]
#[ignore = "local GPU snapshot; run with --ignored --nocapture"]
fn gpu_primitive_matches_cpu_reference() {
    use iai::core::shape::{ShapeData, ShapeKind};
    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no GPU adapter available; skipping primitive snapshot");
        return;
    };
    let renderer = VectorRenderer::new(
        &device,
        iai::gpu::vector::renderer::VECTOR_TARGET_FORMAT,
        iai::gpu::vector::renderer::VECTOR_SAMPLE_COUNT,
    );
    // Solid fill on each primitive kind. `assert_matches_reference` draws the GPU
    // fill as `fill_color()` (= sRGB bytes [26,140,217]); match that so the CPU
    // reference colour agrees. from_canvas_span returns the shape (layer-local
    // geometry) + the layer offset that places it in canvas space.
    let make = |kind, radius| {
        let (mut shape, off) = ShapeData::from_canvas_span(
            kind,
            15.0,
            15.0,
            65.0,
            55.0,
            radius,
            true,
            [26, 140, 217, 255],
            0.0,
            [0, 0, 0, 0],
        );
        shape.sides = 5;
        shape.star_inner = 0.5;
        shape.to_vector_object(off)
    };
    let cases: Vec<(&str, VectorObjectData)> = vec![
        ("prim_rect", make(ShapeKind::Rectangle, 0.0)),
        ("prim_rounded_rect", make(ShapeKind::Rectangle, 10.0)),
        ("prim_ellipse", make(ShapeKind::Ellipse, 0.0)),
        ("prim_polygon", make(ShapeKind::Polygon, 0.0)),
        ("prim_star", make(ShapeKind::Star, 0.0)),
    ];
    for (label, object) in &cases {
        assert_matches_reference(&renderer, &device, &queue, label, object);
    }
}

/// A thick open line with round caps. The CPU rasteriser and the GPU tessellator
/// now both fill the same `stroke_outline_contours` reference, so the round cap
/// outline agrees on either path.
fn stroked_line() -> VectorObjectData {
    use iai::core::vector::style::{LineCap, LineJoin};
    let path = PathData::new(
        vec![Contour::new(
            vec![
                Node::sharp(Point::new(0.0, 0.0)),
                Node::sharp(Point::new(80.0, 0.0)),
            ],
            false,
        )],
        FillRule::NonZero,
    );
    let mut style = VectorStyle::stroked(ColorValue::rgb(0.90, 0.20, 0.20), 12.0);
    style.stroke_style.cap = LineCap::Round;
    style.stroke_style.join = LineJoin::Round;
    style.fill = iai::core::vector::style::Paint::None;
    VectorObjectData::new(path, style, AffineTransform::translate(20.0, 30.0))
}

/// A thick open polyline with a right-angle corner drawn with the DEFAULT stroke
/// style — butt caps and a miter join. This exercises the sharp cap/join outline
/// (not a round capsule), so the flat ends and mitred corner must match on both
/// the CPU rasteriser and the GPU twin.
fn stroked_corner() -> VectorObjectData {
    use iai::core::vector::style::{LineCap, LineJoin};
    let path = PathData::new(
        vec![Contour::new(
            vec![
                Node::sharp(Point::new(0.0, 0.0)),
                Node::sharp(Point::new(80.0, 0.0)),
                Node::sharp(Point::new(80.0, 80.0)),
            ],
            false,
        )],
        FillRule::NonZero,
    );
    let mut style = VectorStyle::stroked(ColorValue::rgb(0.20, 0.40, 0.90), 14.0);
    style.stroke_style.cap = LineCap::Butt;
    style.stroke_style.join = LineJoin::Miter;
    style.stroke_style.miter_limit = 4.0;
    style.fill = iai::core::vector::style::Paint::None;
    VectorObjectData::new(path, style, AffineTransform::translate(30.0, 30.0))
}

#[test]
#[ignore = "local GPU snapshot; run with --ignored --nocapture"]
fn gpu_stroke_matches_cpu_reference() {
    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no GPU adapter available; skipping GPU stroke snapshot");
        return;
    };
    let renderer = VectorRenderer::new(
        &device,
        iai::gpu::vector::renderer::VECTOR_TARGET_FORMAT,
        iai::gpu::vector::renderer::VECTOR_SAMPLE_COUNT,
    );
    let object = stroked_line();
    let cpu = rasterize(&object).expect("reference stroke raster");
    let cpu_mesh = tessellate(&object, 0.1).expect("tessellate stroke");
    assert!(
        !cpu_mesh.stroke_range.is_empty() && cpu_mesh.fill_range.is_empty(),
        "stroke-only object must produce stroke geometry and no fill"
    );
    let mesh = GpuMesh::upload(&device, &cpu_mesh);
    let view = CanvasView::tight(
        cpu.width,
        cpu.height,
        cpu.offset.0 as f32,
        cpu.offset.1 as f32,
    );
    let draws = [VectorDraw {
        mesh: &mesh,
        object_to_canvas: object.transform,
        fill: None,
        stroke: Some(GpuPaint::Solid([0.90, 0.20, 0.20, 1.0])),
        opacity: object.style.opacity,
    }];
    let gpu = renderer.render_offscreen(&device, &queue, view, &draws);
    let c = compare(&cpu, &gpu);
    assert!(
        c.interior > 40,
        "stroke: too few interior pixels ({})",
        c.interior
    );
    // A few pixels at the round-cap boundary can differ between the CPU capsule and
    // the Lyon arc tessellation; the fully-covered core must match.
    let bad_frac = c.interior_bad as f32 / c.interior as f32;
    assert!(
        bad_frac < 0.01,
        "stroke: {} / {} interior pixels differ ({:.2}%)",
        c.interior_bad,
        c.interior,
        bad_frac * 100.0
    );
    assert_eq!(
        c.exterior_bad, 0,
        "stroke: {} exterior pixels wrongly painted",
        c.exterior_bad
    );
    eprintln!(
        "stroke: {}x{} interior={} ok",
        cpu.width, cpu.height, c.interior
    );
}

#[test]
#[ignore = "local GPU snapshot; run with --ignored --nocapture"]
fn gpu_stroke_cap_join_matches_cpu_reference() {
    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no GPU adapter available; skipping GPU cap/join snapshot");
        return;
    };
    let renderer = VectorRenderer::new(
        &device,
        iai::gpu::vector::renderer::VECTOR_TARGET_FORMAT,
        iai::gpu::vector::renderer::VECTOR_SAMPLE_COUNT,
    );
    let object = stroked_corner();
    let cpu = rasterize(&object).expect("reference stroke raster");
    let cpu_mesh = tessellate(&object, 0.1).expect("tessellate stroke");
    let mesh = GpuMesh::upload(&device, &cpu_mesh);
    let view = CanvasView::tight(
        cpu.width,
        cpu.height,
        cpu.offset.0 as f32,
        cpu.offset.1 as f32,
    );
    let draws = [VectorDraw {
        mesh: &mesh,
        object_to_canvas: object.transform,
        fill: None,
        stroke: Some(GpuPaint::Solid([0.20, 0.40, 0.90, 1.0])),
        opacity: object.style.opacity,
    }];
    let gpu = renderer.render_offscreen(&device, &queue, view, &draws);
    let c = compare(&cpu, &gpu);
    assert!(
        c.interior > 40,
        "cap/join stroke: too few interior pixels ({})",
        c.interior
    );
    let bad_frac = c.interior_bad as f32 / c.interior as f32;
    assert!(
        bad_frac < 0.01,
        "cap/join stroke: {} / {} interior pixels differ ({:.2}%)",
        c.interior_bad,
        c.interior,
        bad_frac * 100.0
    );
    assert_eq!(
        c.exterior_bad, 0,
        "cap/join stroke: {} exterior pixels wrongly painted (butt cap / miter corner leaked)",
        c.exterior_bad
    );
    eprintln!(
        "cap/join stroke: {}x{} interior={} ok",
        cpu.width, cpu.height, c.interior
    );
}

#[test]
#[ignore = "local GPU snapshot; run with --ignored --nocapture"]
fn gpu_normal_opacity_multiplies_object_and_layer_alpha() {
    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no GPU adapter; skipping opacity snapshot");
        return;
    };
    let renderer = VectorRenderer::new(
        &device,
        iai::gpu::vector::renderer::VECTOR_TARGET_FORMAT,
        iai::gpu::vector::renderer::VECTOR_SAMPLE_COUNT,
    );
    let mut object = styled(square(40.0), AffineTransform::translate(2.0, 2.0));
    object.style.opacity = 0.5;
    let mesh = GpuMesh::upload(&device, &tessellate(&object, 0.1).unwrap());
    let draws = [VectorDraw {
        mesh: &mesh,
        object_to_canvas: object.transform,
        fill: Some(GpuPaint::Solid([0.10, 0.55, 0.85, 1.0])),
        stroke: None,
        opacity: object.style.opacity * 0.5,
    }];
    let out =
        renderer.render_offscreen(&device, &queue, CanvasView::tight(48, 48, 0.0, 0.0), &draws);
    let center = ((20 * 48 + 20) * 4) as usize;
    assert!(
        (out[center + 3] as i32 - 64).abs() <= 2,
        "object 0.5 × layer 0.5 must produce alpha 0.25, got {}",
        out[center + 3]
    );
}

#[test]
#[ignore = "local GPU snapshot; run with --ignored --nocapture"]
fn gpu_run_preserves_intra_run_z_order() {
    // Two overlapping opaque fills drawn as one run (multiple draws, one pass):
    // the later draw must occlude the earlier one in the overlap. This is the
    // Phase 2 intra-run compositing claim on the real renderer path.
    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no GPU adapter available; skipping GPU z-order snapshot");
        return;
    };
    let renderer = VectorRenderer::new(
        &device,
        iai::gpu::vector::renderer::VECTOR_TARGET_FORMAT,
        iai::gpu::vector::renderer::VECTOR_SAMPLE_COUNT,
    );
    let red = [0.85, 0.15, 0.15, 1.0];
    let green = [0.15, 0.75, 0.20, 1.0];
    let a = styled(square(40.0), AffineTransform::translate(10.0, 10.0)); // [10,50]
    let b = styled(square(40.0), AffineTransform::translate(30.0, 30.0)); // [30,70]
    let mesh_a = GpuMesh::upload(&device, &tessellate(&a, 0.1).unwrap());
    let mesh_b = GpuMesh::upload(&device, &tessellate(&b, 0.1).unwrap());
    let draws = [
        VectorDraw {
            mesh: &mesh_a,
            object_to_canvas: a.transform,
            fill: Some(GpuPaint::Solid(red)),
            stroke: None,
            opacity: 1.0,
        },
        VectorDraw {
            mesh: &mesh_b,
            object_to_canvas: b.transform,
            fill: Some(GpuPaint::Solid(green)),
            stroke: None,
            opacity: 1.0,
        },
    ];
    let (w, h) = (80u32, 80u32);
    let view = CanvasView::tight(w, h, 0.0, 0.0);
    let out = renderer.render_offscreen(&device, &queue, view, &draws);
    let byte = |x: u32, y: u32, c: usize| out[((y * w + x) * 4) as usize + c];
    let expect = |x: u32, y: u32, want: [u8; 3], label: &str| {
        for (c, &wc) in want.iter().enumerate() {
            let got = byte(x, y, c as usize);
            assert!(
                (got as i32 - wc as i32).abs() <= 2,
                "{label} at ({x},{y}) ch{c}: got {got}, want {wc}"
            );
        }
        assert!(byte(x, y, 3) >= 250, "{label} at ({x},{y}) not opaque");
    };
    // ColorValue::rgb stores sRGB bytes = round(v*255).
    let red_b: [u8; 3] = [217, 38, 38];
    let green_b: [u8; 3] = [38, 191, 51];
    expect(40, 40, green_b, "overlap → top(green)");
    expect(15, 15, red_b, "A only → red");
    expect(65, 65, green_b, "B only → green");
    assert_eq!(byte(3, 3, 3), 0, "outside both → transparent");
    assert_eq!(byte(72, 12, 3), 0, "outside both (corner) → transparent");
    eprintln!("intra-run z-order ok");
}

/// Exercise the live-compositor stage (`VectorCompositeStage::composite_run`): a
/// vector run composited over a known opaque background must place the shape
/// correctly and blend straight-alpha over the background (the Phase 2 path).
#[test]
#[ignore = "local GPU snapshot; run with --ignored --nocapture"]
fn gpu_composite_run_over_background() {
    use iai::core::layer::LayerMask;
    use iai::gpu::vector::composite::{VectorCompositeStage, VectorMask};

    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no GPU adapter; skipping composite-run test");
        return;
    };
    let fmt = iai::gpu::vector::renderer::VECTOR_TARGET_FORMAT;
    let mut stage = VectorCompositeStage::new(&device, fmt);
    let (w, h) = (80u32, 80u32);
    let extent = wgpu::Extent3d {
        width: w,
        height: h,
        depth_or_array_layers: 1,
    };
    let mk = |usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: fmt,
            usage,
            view_formats: &[],
        })
    };
    let dst_read =
        mk(wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING);
    let dst_write = mk(wgpu::TextureUsages::RENDER_ATTACHMENT
        | wgpu::TextureUsages::TEXTURE_BINDING
        | wgpu::TextureUsages::COPY_SRC);
    let read_view = dst_read.create_view(&Default::default());
    let write_view = dst_write.create_view(&Default::default());

    // Opaque red background (primaries are gamma-clean → exact bytes).
    let mut enc = device.create_command_encoder(&Default::default());
    {
        enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("bg_clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &read_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 1.0,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            ..Default::default()
        });
    }

    // A pure-green square over [10,50] in canvas space; zoom 1, no view offset.
    let sq = VectorObjectData::new(
        square(40.0),
        VectorStyle::filled(ColorValue::rgb(0.0, 1.0, 0.0)),
        AffineTransform::translate(10.0, 10.0),
    );
    let blue = VectorObjectData::new(
        square(40.0),
        VectorStyle::filled(ColorValue::rgb(0.0, 0.0, 1.0)),
        AffineTransform::translate(20.0, 10.0),
    );
    // The settled invariant: layer.offset == the model raster origin (drift 0).
    let origin = iai::core::vector::raster::raster_geometry(&sq)
        .map(|(o, _, _)| o)
        .unwrap_or((0, 0));
    let blue_origin = iai::core::vector::raster::raster_geometry(&blue)
        .map(|(o, _, _)| o)
        .unwrap_or((0, 0));
    let mut mask = LayerMask::new_black(42, 42);
    for y in 0..42 {
        for x in 0..35 {
            let value = if x < 28 { 255 } else { 128 };
            mask.tiles.set_pixel(x, y, value, value, value, 255);
        }
    }
    stage.begin_frame();
    stage.composite_run(
        &device,
        &queue,
        &mut enc,
        &read_view,
        &write_view,
        w,
        h,
        0.0,
        0.0,
        1.0,
        &[(&sq, origin, 1.0), (&blue, blue_origin, 1.0)],
        Some(VectorMask {
            layer_id: 77,
            layer_offset: origin,
            sample_shift: (5, 0),
            mask: &mask,
        }),
        0.5,
        iai::core::blend::BlendMode::Normal,
    );

    // Read back dst_write.
    let unpadded = w * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (padded * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &dst_write,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(h),
            },
        },
        extent,
    );
    queue.submit([enc.finish()]);
    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).ok();
    let data = slice.get_mapped_range();
    let at = |x: u32, y: u32, c: usize| data[(y * padded + x * 4) as usize + c];

    // Inside the square → green over red = green (opaque). Outside → red background.
    assert!(
        (120..=135).contains(&at(15, 30, 0))
            && (120..=135).contains(&at(15, 30, 1))
            && at(15, 30, 2) <= 5,
        "isolated run opacity must blend the green-only region once"
    );
    assert!(
        (120..=135).contains(&at(25, 30, 0))
            && at(25, 30, 1) <= 5
            && (120..=135).contains(&at(25, 30, 2)),
        "overlapping children must resolve to blue before group opacity"
    );
    assert!(
        (185..=200).contains(&at(35, 30, 0))
            && at(35, 30, 1) <= 5
            && (55..=70).contains(&at(35, 30, 2)),
        "soft shifted mask and group opacity must multiply coverage"
    );
    assert!(
        at(45, 30, 0) >= 250 && at(45, 30, 1) <= 5 && at(45, 30, 2) <= 5,
        "shifted black mask region must preserve red background"
    );
    assert!(
        at(70, 70, 0) >= 250 && at(70, 70, 1) <= 5,
        "outside run must stay red background"
    );
    assert_eq!(at(15, 30, 3), 255, "revealed area stays opaque");
    eprintln!("isolated opacity + PowerClip-shift composite-run ok");
}

/// Phase 3 acceptance: the GPU mesh cache must re-tessellate + re-upload **zero**
/// meshes on pan, zoom (within a bucket), move, rotate and non-uniform scale — the
/// transform is uniform-only — and rebuild exactly **one** mesh when a node moves.
#[test]
#[ignore = "local GPU snapshot; run with --ignored --nocapture"]
fn gpu_mesh_cache_is_transform_invariant() {
    use iai::core::vector::raster::raster_geometry;
    use iai::gpu::vector::composite::VectorCompositeStage;

    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no GPU adapter; skipping mesh-cache test");
        return;
    };
    let fmt = iai::gpu::vector::renderer::VECTOR_TARGET_FORMAT;
    let mut stage = VectorCompositeStage::new(&device, fmt);
    let (w, h) = (96u32, 96u32);
    let extent = wgpu::Extent3d {
        width: w,
        height: h,
        depth_or_array_layers: 1,
    };
    let mk = || {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: fmt,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default())
    };
    let read_view = mk();
    let write_view = mk();

    // One eligible solid-fill square. `offset` is the model raster origin so the
    // drag-drift correction is a no-op (it never affects tessellation anyway).
    let mut obj = styled(square(20.0), AffineTransform::translate(20.0, 20.0));
    let origin = |o: &VectorObjectData| raster_geometry(o).map(|(p, _, _)| p).unwrap_or((0, 0));

    // A single frame at (offset, zoom) with the current object; returns
    // (tessellations, uploads) charged this frame.
    let run = |stage: &mut VectorCompositeStage, obj: &VectorObjectData, ox, oy, zoom| {
        let mut enc = device.create_command_encoder(&Default::default());
        stage.begin_frame();
        stage.composite_run(
            &device,
            &queue,
            &mut enc,
            &read_view,
            &write_view,
            w,
            h,
            ox,
            oy,
            zoom,
            &[(obj, origin(obj), 1.0)],
            None,
            1.0,
            iai::core::blend::BlendMode::Normal,
        );
        (stage.last_frame_tessellations(), stage.last_frame_uploads())
    };

    // Frame 1: cold cache → exactly one tessellation + upload. Zoom 2.5 → bucket 4.
    assert_eq!(run(&mut stage, &obj, 0.0, 0.0, 2.5), (1, 1), "cold build");
    assert_eq!(stage.cache_len(), 1);

    // Pan (view offset changes only) → uniform-only, no rebuild.
    assert_eq!(run(&mut stage, &obj, 12.0, -8.0, 2.5), (0, 0), "pan");

    // Zoom within the same bucket (2.5 → 3.5 both bucket 4) → no rebuild.
    assert_eq!(
        run(&mut stage, &obj, 12.0, -8.0, 3.5),
        (0, 0),
        "zoom in-bucket"
    );

    // Move (object translate) → fingerprint ignores the transform → no rebuild.
    obj.transform = AffineTransform::translate(35.0, 30.0);
    assert_eq!(run(&mut stage, &obj, 0.0, 0.0, 2.5), (0, 0), "move");

    // Rotate around the origin → transform-only → no rebuild.
    obj.transform = AffineTransform::translate(40.0, 40.0).then(&AffineTransform::rotate(0.6));
    assert_eq!(run(&mut stage, &obj, 0.0, 0.0, 2.5), (0, 0), "rotate");

    // Non-uniform scale → transform-only (tolerance is keyed only by bucket) → no
    // rebuild.
    obj.transform = AffineTransform::scale(1.7, 0.8).then(&AffineTransform::translate(20.0, 20.0));
    assert_eq!(run(&mut stage, &obj, 0.0, 0.0, 2.5), (0, 0), "scale");

    assert_eq!(stage.cache_len(), 1, "no transform re-tessellated");

    // Node edit → the geometry fingerprint changes → exactly one mesh rebuilt, and
    // a second cache entry now exists (same bucket, new fingerprint).
    obj.path.contours[0].nodes[1].anchor = Point::new(28.0, -4.0);
    assert_eq!(run(&mut stage, &obj, 0.0, 0.0, 2.5), (1, 1), "node edit");
    assert_eq!(stage.cache_len(), 2, "edited path added one entry");

    eprintln!(
        "mesh-cache transform-invariance ok: {} entries, {} bytes, {} evictions",
        stage.cache_len(),
        stage.cache_bytes(),
        stage.cache_evictions()
    );
}

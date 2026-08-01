use bytemuck::{Pod, Zeroable};
use lyon_path::math::point;
use lyon_tessellation::geometry_builder::{BuffersBuilder, VertexBuffers};
use lyon_tessellation::{FillOptions, FillRule as LyonFillRule, FillTessellator, FillVertex};

use crate::core::vector::object::VectorObjectData;
use crate::core::vector::path::{FillRule, PathData};
use crate::core::vector::stroke::stroke_outline_contours;
use crate::core::vector::{flatten::flatten_path, raster::dashed_polylines};

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct VectorVertex {
    pub position: [f32; 2],
}

#[derive(Debug, Clone, Default)]
pub struct VectorMesh {
    pub vertices: Vec<VectorVertex>,
    pub indices: Vec<u32>,
    pub fill_range: std::ops::Range<u32>,
    pub stroke_range: std::ops::Range<u32>,
}

impl VectorMesh {
    pub fn byte_len(&self) -> usize {
        self.vertices.len() * std::mem::size_of::<VectorVertex>()
            + self.indices.len() * std::mem::size_of::<u32>()
    }
}

pub fn to_lyon_path(path: &PathData) -> lyon_path::Path {
    let mut builder = lyon_path::Path::builder();
    for contour in &path.contours {
        let Some(first) = contour.nodes.first() else {
            continue;
        };
        builder.begin(point(first.anchor.x, first.anchor.y));
        for i in 0..contour.segment_count() {
            let Some((p0, p1, p2, p3)) = contour.segment(i) else {
                continue;
            };
            let straight = p0 == p1 && p2 == p3;
            if straight {
                builder.line_to(point(p3.x, p3.y));
            } else {
                builder.cubic_bezier_to(point(p1.x, p1.y), point(p2.x, p2.y), point(p3.x, p3.y));
            }
        }
        builder.end(contour.closed);
    }
    builder.build()
}

/// The stroke's filled outline as a Lyon path, built from the shared CPU/GPU
/// [`stroke_outline_contours`] reference so the GPU fills the exact same shape the
/// rasteriser does. Every contour is one closed subpath; overlaps union under a
/// NonZero fill.
fn stroke_outline_lyon_path(object: &VectorObjectData, tolerance: f32) -> lyon_path::Path {
    let tol = tolerance.max(0.001);
    let flat = flatten_path(&object.path, tol);
    let closed: Vec<bool> = object.path.contours.iter().map(|c| c.closed).collect();
    let ss = object.style.stroke_style;
    let (lines, line_closed) = if ss.dash.is_solid() {
        (flat, closed)
    } else {
        dashed_polylines(&flat, &closed, ss.dash.as_slice(), ss.dash.offset)
    };
    let contours = stroke_outline_contours(
        &lines,
        &line_closed,
        ss.width * 0.5,
        ss.cap,
        ss.join,
        ss.miter_limit,
        tol,
    );
    let mut builder = lyon_path::Path::builder();
    for ring in &contours {
        let Some(first) = ring.first() else {
            continue;
        };
        builder.begin(point(first.x, first.y));
        for p in &ring[1..] {
            builder.line_to(point(p.x, p.y));
        }
        builder.end(true);
    }
    builder.build()
}

pub fn tessellate(object: &VectorObjectData, tolerance: f32) -> Result<VectorMesh, String> {
    object.validate()?;
    let path = to_lyon_path(&object.path);
    let mut buffers: VertexBuffers<VectorVertex, u32> = VertexBuffers::new();
    let mut fill_end = 0;

    if object.style.fill.is_visible() {
        let fill_rule = match object.path.fill_rule {
            FillRule::NonZero => LyonFillRule::NonZero,
            FillRule::EvenOdd => LyonFillRule::EvenOdd,
        };
        FillTessellator::new()
            .tessellate_path(
                &path,
                &FillOptions::default()
                    .with_fill_rule(fill_rule)
                    .with_tolerance(tolerance.max(0.001)),
                &mut BuffersBuilder::new(&mut buffers, |v: FillVertex| VectorVertex {
                    position: [v.position().x, v.position().y],
                }),
            )
            .map_err(|e| format!("fill tessellation failed: {e:?}"))?;
        fill_end = buffers.indices.len() as u32;
    }

    if object.style.stroke.is_visible() && object.style.stroke_style.width > 0.0 {
        // The stroke is drawn by FILLING its true outline (honouring cap/join)
        // with a NonZero fill — the exact same outline the CPU rasteriser fills
        // (`core::vector::stroke::stroke_outline_contours`), so the GPU twin
        // matches the reference and the flag toggles cleanly.
        let outline = stroke_outline_lyon_path(object, tolerance);
        FillTessellator::new()
            .tessellate_path(
                &outline,
                &FillOptions::default()
                    .with_fill_rule(LyonFillRule::NonZero)
                    .with_tolerance(tolerance.max(0.001)),
                &mut BuffersBuilder::new(&mut buffers, |v: FillVertex| VectorVertex {
                    position: [v.position().x, v.position().y],
                }),
            )
            .map_err(|e| format!("stroke outline tessellation failed: {e:?}"))?;
    }
    let index_end = buffers.indices.len() as u32;
    Ok(VectorMesh {
        vertices: buffers.vertices,
        indices: buffers.indices,
        fill_range: 0..fill_end,
        stroke_range: fill_end..index_end,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::geometry::Point;
    use crate::core::vector::color::ColorValue;
    use crate::core::vector::path::{Contour, Node};
    use crate::core::vector::style::VectorStyle;

    fn compound(rule: FillRule) -> VectorObjectData {
        let contour = |x: f32, s: f32| {
            Contour::new(
                vec![
                    Node::sharp(Point::new(x, x)),
                    Node::sharp(Point::new(x + s, x)),
                    Node::sharp(Point::new(x + s, x + s)),
                    Node::sharp(Point::new(x, x + s)),
                ],
                true,
            )
        };
        VectorObjectData::from_path(PathData::new(
            vec![contour(0.0, 20.0), contour(5.0, 5.0)],
            rule,
        ))
    }

    #[test]
    fn maps_both_fill_rules_and_curves() {
        for rule in [FillRule::NonZero, FillRule::EvenOdd] {
            let mesh = tessellate(&compound(rule), 0.1).unwrap();
            assert!(!mesh.vertices.is_empty());
            assert!(!mesh.fill_range.is_empty());
        }
    }

    #[test]
    fn stroke_generates_separate_index_range() {
        let mut object = compound(FillRule::NonZero);
        object.style = VectorStyle::stroked(ColorValue::BLACK, 3.0);
        let mesh = tessellate(&object, 0.1).unwrap();
        assert!(mesh.fill_range.is_empty());
        assert!(!mesh.stroke_range.is_empty());
    }

    #[test]
    fn dashed_stroke_tessellates_bounded_nonempty_geometry() {
        use crate::core::vector::style::DashPattern;
        let path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(100.0, 0.0)),
                ],
                false,
            )],
            FillRule::NonZero,
        );
        let mut dashed = VectorObjectData::from_path(path);
        dashed.style = VectorStyle::stroked(ColorValue::BLACK, 4.0);
        dashed.style.stroke_style.dash = DashPattern::from_slice(&[10.0, 10.0], 0.0);
        let mesh = tessellate(&dashed, 0.1).unwrap();
        // ~5 visible dashes, each a small butt-capped outline quad: the stroke is
        // non-empty but the dash split keeps the geometry bounded.
        assert!(!mesh.stroke_range.is_empty());
        assert!(
            mesh.vertices.len() < 200,
            "dash tessellation must stay bounded, got {}",
            mesh.vertices.len()
        );
    }

    #[test]
    fn cap_style_changes_gpu_stroke_geometry() {
        use crate::core::vector::style::LineCap;
        // A single open segment: round caps add arc geometry that a butt cap does
        // not, proving the GPU now honours the stored cap (no longer forced round).
        let make = |cap: LineCap| {
            let path = PathData::new(
                vec![Contour::new(
                    vec![
                        Node::sharp(Point::new(0.0, 0.0)),
                        Node::sharp(Point::new(50.0, 0.0)),
                    ],
                    false,
                )],
                FillRule::NonZero,
            );
            let mut o = VectorObjectData::from_path(path);
            o.style = VectorStyle::stroked(ColorValue::BLACK, 10.0);
            o.style.stroke_style.cap = cap;
            tessellate(&o, 0.1).unwrap()
        };
        let butt = make(LineCap::Butt);
        let round = make(LineCap::Round);
        assert!(
            round.vertices.len() > butt.vertices.len(),
            "round caps add geometry vs butt ({} vs {})",
            round.vertices.len(),
            butt.vertices.len()
        );
    }
}

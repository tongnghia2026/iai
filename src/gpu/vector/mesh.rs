use bytemuck::{Pod, Zeroable};
use lyon_path::math::point;
use lyon_tessellation::geometry_builder::{BuffersBuilder, VertexBuffers};
use lyon_tessellation::{
    FillOptions, FillRule as LyonFillRule, FillTessellator, FillVertex, LineCap as LyonLineCap,
    LineJoin as LyonLineJoin, StrokeOptions, StrokeTessellator, StrokeVertex,
};

use crate::core::vector::object::VectorObjectData;
use crate::core::vector::path::{FillRule, PathData};

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
        // The CPU reference (`core::vector::raster::stroke_coverage`) unions
        // per-segment round capsules, i.e. it draws every stroke with round caps
        // and round joins regardless of the style's cap/join. To match it exactly
        // (so the GPU output equals the raster twin and the flag toggles cleanly),
        // the GPU stroke is always tessellated round too — the cap/join style is
        // intentionally ignored here, mirroring the rasteriser.
        let options = StrokeOptions::default()
            .with_line_width(object.style.stroke_style.width)
            .with_start_cap(LyonLineCap::Round)
            .with_end_cap(LyonLineCap::Round)
            .with_line_join(LyonLineJoin::Round)
            .with_tolerance(tolerance.max(0.001));
        StrokeTessellator::new()
            .tessellate_path(
                &path,
                &options,
                &mut BuffersBuilder::new(&mut buffers, |v: StrokeVertex| VectorVertex {
                    position: [v.position().x, v.position().y],
                }),
            )
            .map_err(|e| format!("stroke tessellation failed: {e:?}"))?;
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
}

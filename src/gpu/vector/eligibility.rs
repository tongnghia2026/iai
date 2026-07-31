use crate::core::blend::BlendMode;
use crate::core::layer::{Layer, LayerType};
use crate::core::vector::color::ColorValue;
use crate::core::vector::object::{VectorGeometry, VectorObjectData};
use crate::core::vector::style::{LineCap, LineJoin, Paint};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackReason {
    Disabled,
    NotVector,
    Hidden,
    Mask,
    PowerClip,
    Group,
    BlendMode,
    LayerOpacity,
    ObjectOpacity,
    Cmyk,
    Gradient,
    Dash,
    /// The CPU rasteriser draws every stroke as round capsules (it ignores the
    /// cap/join style). A butt/square cap or miter/bevel join would render
    /// differently on the GPU, so those fall back until the CPU reference honours
    /// them too. See `core::vector::raster::stroke_coverage`.
    StrokeStyle,
    VectorBrush,
    InvalidGeometry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eligibility {
    GpuVector,
    RasterFallback(FallbackReason),
}

fn paint_supported(paint: Paint) -> Result<(), FallbackReason> {
    match paint {
        Paint::None => Ok(()),
        Paint::Solid(ColorValue::Rgb { .. }) => Ok(()),
        Paint::Solid(ColorValue::Cmyk { .. }) => Err(FallbackReason::Cmyk),
        Paint::Gradient(_) => Err(FallbackReason::Gradient),
    }
}

pub fn object_eligibility(object: &VectorObjectData) -> Eligibility {
    if object.validate().is_err() {
        return Eligibility::RasterFallback(FallbackReason::InvalidGeometry);
    }
    if object.brush.is_some() {
        return Eligibility::RasterFallback(FallbackReason::VectorBrush);
    }
    if object.style.opacity != 1.0 {
        return Eligibility::RasterFallback(FallbackReason::ObjectOpacity);
    }
    if !object.style.stroke_style.dash.is_solid() {
        return Eligibility::RasterFallback(FallbackReason::Dash);
    }
    // A visible stroke must be round to match the CPU capsule rasteriser; other
    // caps/joins fall back the whole layer (the fill would otherwise be shown with
    // a differently-shaped stroke).
    if object.style.stroke.is_visible()
        && object.style.effective_stroke_width() > 0.0
        && (object.style.stroke_style.cap != LineCap::Round
            || object.style.stroke_style.join != LineJoin::Round)
    {
        return Eligibility::RasterFallback(FallbackReason::StrokeStyle);
    }
    for paint in [object.style.fill, object.style.stroke] {
        if let Err(reason) = paint_supported(paint) {
            return Eligibility::RasterFallback(reason);
        }
    }
    Eligibility::GpuVector
}

pub fn layer_eligibility(layer: &Layer, enabled: bool) -> Eligibility {
    if !enabled {
        return Eligibility::RasterFallback(FallbackReason::Disabled);
    }
    if !layer.visible {
        return Eligibility::RasterFallback(FallbackReason::Hidden);
    }
    if layer.mask.as_ref().is_some_and(|m| m.enabled) {
        return Eligibility::RasterFallback(FallbackReason::Mask);
    }
    if layer.clip_parent_id.is_some() {
        return Eligibility::RasterFallback(FallbackReason::PowerClip);
    }
    if layer.parent_id.is_some() {
        return Eligibility::RasterFallback(FallbackReason::Group);
    }
    if layer.blend_mode != BlendMode::Normal {
        return Eligibility::RasterFallback(FallbackReason::BlendMode);
    }
    if layer.opacity != 1.0 {
        return Eligibility::RasterFallback(FallbackReason::LayerOpacity);
    }
    match &layer.layer_type {
        LayerType::Vector(VectorGeometry::Path(object)) => object_eligibility(object),
        // Primitive conversion is deterministic, but stays fallback until its
        // production draw path has snapshot coverage.
        LayerType::Vector(VectorGeometry::Primitive(_)) => {
            Eligibility::RasterFallback(FallbackReason::NotVector)
        }
        _ => Eligibility::RasterFallback(FallbackReason::NotVector),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::geometry::Point;
    use crate::core::vector::affine::AffineTransform;
    use crate::core::vector::object::VectorObjectData;
    use crate::core::vector::path::{Contour, FillRule, Node, PathData};
    use crate::core::vector::style::{Gradient, GradientKind};

    fn layer_with(object: VectorObjectData) -> Layer {
        let mut layer = Layer::new(7, "vector", 64, 64);
        layer.layer_type = LayerType::Vector(VectorGeometry::Path(object));
        layer
    }

    fn square() -> VectorObjectData {
        VectorObjectData::from_path(PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(10.0, 0.0)),
                    Node::sharp(Point::new(10.0, 10.0)),
                ],
                true,
            )],
            FillRule::NonZero,
        ))
    }

    #[test]
    fn solid_rgb_path_is_eligible_only_when_enabled() {
        let layer = layer_with(square());
        assert_eq!(layer_eligibility(&layer, true), Eligibility::GpuVector);
        assert_eq!(
            layer_eligibility(&layer, false),
            Eligibility::RasterFallback(FallbackReason::Disabled)
        );
    }

    #[test]
    fn non_round_stroke_falls_back_but_round_stroke_is_eligible() {
        use crate::core::vector::color::ColorValue;
        use crate::core::vector::style::{LineCap, LineJoin, VectorStyle};
        let mut object = square();
        // Default stroked style is butt cap / miter join — the CPU draws round.
        object.style = VectorStyle::stroked(ColorValue::BLACK, 3.0);
        assert_eq!(
            object_eligibility(&object),
            Eligibility::RasterFallback(FallbackReason::StrokeStyle)
        );
        object.style.stroke_style.cap = LineCap::Round;
        object.style.stroke_style.join = LineJoin::Round;
        assert_eq!(object_eligibility(&object), Eligibility::GpuVector);
    }

    #[test]
    fn unsupported_features_fallback_whole_layer() {
        let mut layer = layer_with(square());
        layer.mask = Some(crate::core::layer::LayerMask::new_white(64, 64));
        assert_eq!(
            layer_eligibility(&layer, true),
            Eligibility::RasterFallback(FallbackReason::Mask)
        );
        layer.mask = None;
        if let LayerType::Vector(VectorGeometry::Path(object)) = &mut layer.layer_type {
            object.style.fill = Paint::Gradient(Gradient::two_color(
                GradientKind::Linear,
                ColorValue::BLACK,
                ColorValue::WHITE,
                AffineTransform::IDENTITY,
            ));
        }
        assert_eq!(
            layer_eligibility(&layer, true),
            Eligibility::RasterFallback(FallbackReason::Gradient)
        );
    }
}

use crate::core::blend::BlendMode;
use crate::core::layer::{Layer, LayerType};
use crate::core::vector::color::ColorValue;
use crate::core::vector::object::{VectorGeometry, VectorObjectData};
use crate::core::vector::style::Paint;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackReason {
    Disabled,
    NotVector,
    Hidden,
    Mask,
    PowerClip,
    Group,
    BlendMode,
    Cmyk,
    Dash,
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
        Paint::Gradient(gradient) => {
            if gradient
                .active_stops()
                .iter()
                .all(|stop| matches!(stop.color, ColorValue::Rgb { .. }))
            {
                Ok(())
            } else {
                Err(FallbackReason::Cmyk)
            }
        }
    }
}

/// Style-only eligibility, shared by [`object_eligibility`] and the primitive
/// path (a primitive's geometry is always valid and never carries a brush, so its
/// eligibility is decided entirely by its style — no path allocation needed).
pub fn style_eligibility(style: &crate::core::vector::style::VectorStyle) -> Eligibility {
    if !style.stroke_style.dash.is_solid() {
        return Eligibility::RasterFallback(FallbackReason::Dash);
    }
    // Cap/join style is NOT gated: the GPU tessellates every stroke round (see
    // `mesh::tessellate`) to match the CPU capsule rasteriser, which also ignores
    // cap/join. So a butt/miter outline renders identically to the raster twin and
    // is eligible; only dash still falls back (the GPU does not dash yet).
    for paint in [style.fill, style.stroke] {
        if let Err(reason) = paint_supported(paint) {
            return Eligibility::RasterFallback(reason);
        }
    }
    Eligibility::GpuVector
}

pub fn object_eligibility(object: &VectorObjectData) -> Eligibility {
    if object.validate().is_err() {
        return Eligibility::RasterFallback(FallbackReason::InvalidGeometry);
    }
    if object.brush.is_some() {
        return Eligibility::RasterFallback(FallbackReason::VectorBrush);
    }
    style_eligibility(&object.style)
}

fn layer_eligibility_impl(
    layer: &Layer,
    enabled: bool,
    allow_group_child: bool,
    allow_powerclip: bool,
) -> Eligibility {
    if !enabled {
        return Eligibility::RasterFallback(FallbackReason::Disabled);
    }
    if !layer.visible {
        return Eligibility::RasterFallback(FallbackReason::Hidden);
    }
    if layer.clip_parent_id.is_some() && !allow_powerclip {
        return Eligibility::RasterFallback(FallbackReason::PowerClip);
    }
    if layer.parent_id.is_some() && !allow_group_child {
        return Eligibility::RasterFallback(FallbackReason::Group);
    }
    if layer.blend_mode != BlendMode::Normal {
        return Eligibility::RasterFallback(FallbackReason::BlendMode);
    }
    match &layer.layer_type {
        LayerType::Vector(VectorGeometry::Path(object)) => object_eligibility(object),
        // Phase 6: a primitive is drawn by converting it to the exact `PathData`
        // the raster reference uses (`ShapeData::to_vector_object`), so it is
        // GPU-native under the same style rules as a Path. Its geometry is always
        // valid, so only the style gates it.
        LayerType::Vector(VectorGeometry::Primitive(shape)) => style_eligibility(&shape.style),
        _ => Eligibility::RasterFallback(FallbackReason::NotVector),
    }
}

pub fn layer_eligibility(layer: &Layer, enabled: bool) -> Eligibility {
    layer_eligibility_impl(layer, enabled, false, false)
}

/// Stack-aware eligibility for group children. A plain Normal/100%/unmasked
/// group is pass-through in the CPU compositor, so its children may be rendered
/// inline without changing z-order. Any missing, hidden, or effected ancestor
/// keeps the conservative whole-layer raster fallback.
pub fn layer_eligibility_in_stack(
    layer: &Layer,
    stack: &crate::core::layer::LayerStack,
    enabled: bool,
) -> Eligibility {
    let powerclip_ok = match layer.clip_parent_id {
        None => false,
        Some(frame_id) => {
            layer.mask.as_ref().is_some_and(|mask| mask.enabled)
                && stack.layers.iter().any(|frame| {
                    frame.id == frame_id && frame.visible && frame.has_renderable_content()
                })
        }
    };
    if layer.clip_parent_id.is_some() && !powerclip_ok {
        return Eligibility::RasterFallback(FallbackReason::PowerClip);
    }
    let mut parent_id = layer.parent_id;
    while let Some(id) = parent_id {
        let Some(parent) = stack.layers.iter().find(|candidate| candidate.id == id) else {
            return Eligibility::RasterFallback(FallbackReason::Group);
        };
        if !parent.is_group()
            || !parent.visible
            || parent.opacity < 0.999
            || parent.blend_mode != BlendMode::Normal
            || parent.mask.as_ref().is_some_and(|mask| mask.enabled)
        {
            return Eligibility::RasterFallback(FallbackReason::Group);
        }
        parent_id = parent.parent_id;
    }
    layer_eligibility_impl(layer, enabled, true, powerclip_ok)
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
    fn any_solid_stroke_cap_join_is_eligible() {
        use crate::core::vector::color::ColorValue;
        use crate::core::vector::style::{LineCap, LineJoin, VectorStyle};
        let mut object = square();
        // Default stroked style is butt cap / miter join. The GPU draws it round
        // (matching the CPU capsule rasteriser), so it is eligible either way.
        object.style = VectorStyle::stroked(ColorValue::BLACK, 3.0);
        assert_eq!(object_eligibility(&object), Eligibility::GpuVector);
        object.style.stroke_style.cap = LineCap::Round;
        object.style.stroke_style.join = LineJoin::Round;
        assert_eq!(object_eligibility(&object), Eligibility::GpuVector);
        // Dash still falls back (the GPU does not dash yet).
        object.style.stroke_style.dash =
            crate::core::vector::style::DashPattern::from_slice(&[4.0, 4.0], 0.0);
        assert_eq!(
            object_eligibility(&object),
            Eligibility::RasterFallback(FallbackReason::Dash)
        );
    }

    #[test]
    fn normal_object_and_layer_opacity_are_gpu_eligible() {
        let mut layer = layer_with(square());
        layer.opacity = 0.4;
        if let LayerType::Vector(VectorGeometry::Path(object)) = &mut layer.layer_type {
            object.style.opacity = 0.5;
        }
        assert_eq!(layer_eligibility(&layer, true), Eligibility::GpuVector);
        layer.blend_mode = BlendMode::Multiply;
        assert_eq!(
            layer_eligibility(&layer, true),
            Eligibility::RasterFallback(FallbackReason::BlendMode)
        );
    }

    #[test]
    fn rgb_gradient_primitive_is_eligible() {
        use crate::core::shape::{ShapeData, ShapeKind};
        let (shape, _off) = ShapeData::from_canvas_span(
            ShapeKind::Rectangle,
            10.0,
            10.0,
            50.0,
            40.0,
            0.0,
            true,
            [200, 40, 40, 255],
            0.0,
            [0, 0, 0, 0],
        );
        let mut layer = Layer::new(9, "rect", 64, 64);
        layer.layer_type = LayerType::Vector(VectorGeometry::Primitive(shape.clone()));
        assert_eq!(layer_eligibility(&layer, true), Eligibility::GpuVector);

        let mut grad = shape;
        grad.style.fill = Paint::Gradient(Gradient::two_color(
            GradientKind::Linear,
            ColorValue::BLACK,
            ColorValue::WHITE,
            AffineTransform::IDENTITY,
        ));
        layer.layer_type = LayerType::Vector(VectorGeometry::Primitive(grad));
        assert_eq!(layer_eligibility(&layer, true), Eligibility::GpuVector);
    }

    #[test]
    fn unsupported_features_fallback_whole_layer() {
        let mut layer = layer_with(square());
        layer.mask = Some(crate::core::layer::LayerMask::new_white(64, 64));
        assert_eq!(layer_eligibility(&layer, true), Eligibility::GpuVector);
        layer.clip_parent_id = Some(99);
        assert_eq!(
            layer_eligibility(&layer, true),
            Eligibility::RasterFallback(FallbackReason::PowerClip)
        );
        layer.clip_parent_id = None;
        layer.mask = None;
        if let LayerType::Vector(VectorGeometry::Path(object)) = &mut layer.layer_type {
            object.style.fill = Paint::Gradient(Gradient::two_color(
                GradientKind::Linear,
                ColorValue::cmyk(0.0, 0.0, 0.0, 1.0),
                ColorValue::WHITE,
                AffineTransform::IDENTITY,
            ));
        }
        assert_eq!(
            layer_eligibility(&layer, true),
            Eligibility::RasterFallback(FallbackReason::Cmyk)
        );
    }

    #[test]
    fn only_plain_group_ancestors_allow_gpu_children() {
        let mut child = layer_with(square());
        child.parent_id = Some(8);
        let mut group = Layer::new_group(8, "plain", 64, 64);
        let mut stack = crate::core::layer::LayerStack::new(64, 64);
        stack.layers = vec![child.clone(), group.clone()];
        assert_eq!(
            layer_eligibility_in_stack(&stack.layers[0], &stack, true),
            Eligibility::GpuVector
        );

        group.opacity = 0.5;
        stack.layers[1] = group;
        assert_eq!(
            layer_eligibility_in_stack(&stack.layers[0], &stack, true),
            Eligibility::RasterFallback(FallbackReason::Group)
        );
        assert_eq!(
            layer_eligibility(&child, true),
            Eligibility::RasterFallback(FallbackReason::Group),
            "context-free callers remain conservative"
        );
    }

    #[test]
    fn powerclip_needs_a_live_frame_and_derived_mask() {
        let mut child = layer_with(square());
        child.clip_parent_id = Some(8);
        child.mask = Some(crate::core::layer::LayerMask::new_white(64, 64));
        let mut frame = Layer::new(8, "frame", 64, 64);
        frame.tiles.set_pixel(0, 0, 255, 255, 255, 255);
        let mut stack = crate::core::layer::LayerStack::new(64, 64);
        stack.layers = vec![frame, child];
        assert_eq!(
            layer_eligibility_in_stack(&stack.layers[1], &stack, true),
            Eligibility::GpuVector
        );

        stack.layers[1].mask = None;
        assert_eq!(
            layer_eligibility_in_stack(&stack.layers[1], &stack, true),
            Eligibility::RasterFallback(FallbackReason::PowerClip)
        );
    }
}

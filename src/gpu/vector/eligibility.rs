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

pub fn blend_mode_supported(mode: BlendMode) -> bool {
    matches!(
        mode,
        BlendMode::Normal
            | BlendMode::Multiply
            | BlendMode::ColorBurn
            | BlendMode::Darken
            | BlendMode::Screen
            | BlendMode::ColorDodge
            | BlendMode::Lighten
            | BlendMode::Overlay
            | BlendMode::SoftLight
            | BlendMode::HardLight
            | BlendMode::LinearLight
            | BlendMode::Difference
            | BlendMode::Exclusion
            | BlendMode::Hue
            | BlendMode::Saturation
            | BlendMode::Color
            | BlendMode::Luminosity
    )
    // Dissolve is intentionally excluded: it is stochastic per pixel and cannot
    // match the CPU raster reference across zoom/transform.
}

/// Style-only eligibility, shared by [`object_eligibility`] and the primitive
/// path (a primitive's geometry is always valid and never carries a brush, so its
/// eligibility is decided entirely by its style — no path allocation needed).
pub fn style_eligibility(style: &crate::core::vector::style::VectorStyle) -> Eligibility {
    // Cap/join style is NOT gated: the GPU fills the stroke's true outline from the
    // shared `core::vector::stroke::stroke_outline_contours` reference (see
    // `mesh::tessellate`), the same outline the CPU rasteriser fills, so any
    // butt/round/square cap and miter/round/bevel join renders identically to the
    // raster twin. Dashed strokes are split with the raster reference's exact dash
    // walker before the outline is built.
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
    if !blend_mode_supported(layer.blend_mode) {
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

fn isolated_group_supported(stack: &crate::core::layer::LayerStack, group_id: u32) -> bool {
    let Some(group) = stack
        .layers
        .iter()
        .find(|candidate| candidate.id == group_id)
    else {
        return false;
    };
    if !group.is_group()
        || !group.visible
        || group.parent_id.is_some()
        || (group.opacity >= 0.999 && group.blend_mode == BlendMode::Normal)
        || !blend_mode_supported(group.blend_mode)
        || group.mask.as_ref().is_some_and(|mask| mask.enabled)
    {
        return false;
    }
    let children: Vec<&Layer> = stack
        .layers
        .iter()
        .enumerate()
        .filter(|(index, child)| {
            child.parent_id == Some(group_id) && stack.is_effectively_visible(*index)
        })
        .map(|(_, child)| child)
        .collect();
    !children.is_empty()
        && children.iter().all(|child| {
            !child.is_group()
                && child.blend_mode == BlendMode::Normal
                && child.mask.as_ref().is_none_or(|mask| !mask.enabled)
                && child.clip_parent_id.is_none()
                && matches!(
                    layer_eligibility_impl(child, true, true, false),
                    Eligibility::GpuVector
                )
        })
}

/// True only for the deliberately narrow isolation slice: every visible
/// effected group is a top-level group whose supported opacity/blend can be
/// applied once after its visible direct children form one unmasked/unclipped
/// GPU-vector run.
pub fn stack_supports_gpu_opacity_groups(stack: &crate::core::layer::LayerStack) -> bool {
    stack.layers.iter().enumerate().all(|(index, group)| {
        let effected = group.is_group()
            && stack.is_effectively_visible(index)
            && (group.opacity < 0.999
                || group.blend_mode != BlendMode::Normal
                || group.mask.as_ref().is_some_and(|mask| mask.enabled));
        !effected || isolated_group_supported(stack, group.id)
    })
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
        let pass_through = parent.is_group()
            && parent.visible
            && parent.opacity >= 0.999
            && parent.blend_mode == BlendMode::Normal
            && parent.mask.as_ref().is_none_or(|mask| !mask.enabled);
        if !pass_through && !isolated_group_supported(stack, parent.id) {
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
        // Dash uses the shared CPU/GPU dash splitter and remains GPU eligible.
        object.style.stroke_style.dash =
            crate::core::vector::style::DashPattern::from_slice(&[4.0, 4.0], 0.0);
        assert_eq!(object_eligibility(&object), Eligibility::GpuVector);
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
        assert_eq!(layer_eligibility(&layer, true), Eligibility::GpuVector);
        // The non-separable modes are GPU-native too (mirroring the CPU reference).
        layer.blend_mode = BlendMode::Luminosity;
        assert_eq!(layer_eligibility(&layer, true), Eligibility::GpuVector);
        // Dissolve remains the one unsupported mode → whole-layer raster fallback.
        layer.blend_mode = BlendMode::Dissolve;
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
    fn plain_and_vector_only_effect_groups_allow_gpu_children() {
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
            Eligibility::GpuVector
        );
        assert!(stack_supports_gpu_opacity_groups(&stack));

        stack.layers[1].opacity = 1.0;
        stack.layers[1].blend_mode = BlendMode::Multiply;
        assert_eq!(
            layer_eligibility_in_stack(&stack.layers[0], &stack, true),
            Eligibility::GpuVector
        );
        assert!(stack_supports_gpu_opacity_groups(&stack));

        stack.layers[1].blend_mode = BlendMode::Dissolve;
        assert!(!stack_supports_gpu_opacity_groups(&stack));
        stack.layers[1].blend_mode = BlendMode::Multiply;

        let mut raster = Layer::new(9, "mixed", 64, 64);
        raster.parent_id = Some(8);
        raster.tiles.set_pixel(0, 0, 255, 255, 255, 255);
        stack.layers.insert(1, raster);
        assert!(!stack_supports_gpu_opacity_groups(&stack));
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

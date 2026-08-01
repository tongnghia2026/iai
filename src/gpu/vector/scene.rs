use crate::core::layer::{LayerStack, LayerType};
use crate::core::vector::object::VectorGeometry;

use super::eligibility::{layer_eligibility_in_stack, Eligibility};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneRun {
    Raster(Vec<u32>),
    GpuVector(Vec<u32>),
}

impl SceneRun {
    fn same_kind(&self, gpu: bool) -> bool {
        matches!(
            (self, gpu),
            (Self::GpuVector(_), true) | (Self::Raster(_), false)
        )
    }

    fn push(&mut self, id: u32) {
        match self {
            Self::Raster(ids) | Self::GpuVector(ids) => ids.push(id),
        }
    }
}

/// Pure planning step used by both modes. No raster twin is suppressed here;
/// suppression is only legal in the draw transaction after vector preparation
/// succeeds for the complete run.
pub fn plan_runs(stack: &LayerStack, enabled: bool) -> Vec<SceneRun> {
    let mut runs: Vec<SceneRun> = Vec::new();
    let mut previous_gpu_masked = false;
    let mut previous_gpu_blended = false;
    let mut previous_parent_id = None;
    for (index, layer) in stack.layers.iter().enumerate() {
        if layer.is_group()
            || !stack.is_effectively_visible(index)
            || layer.opacity <= 0.001
            || !layer.has_renderable_content()
        {
            continue;
        }
        let gpu = matches!(
            layer_eligibility_in_stack(layer, stack, enabled),
            Eligibility::GpuVector
        );
        let gpu_masked = gpu && layer.mask.as_ref().is_some_and(|mask| mask.enabled);
        let gpu_blended = gpu && layer.blend_mode != crate::core::blend::BlendMode::Normal;
        if runs.last().is_some_and(|run| run.same_kind(gpu))
            && layer.parent_id == previous_parent_id
            && !gpu_masked
            && !previous_gpu_masked
            && !gpu_blended
            && !previous_gpu_blended
        {
            runs.last_mut().unwrap().push(layer.id);
        } else if gpu {
            runs.push(SceneRun::GpuVector(vec![layer.id]));
        } else {
            runs.push(SceneRun::Raster(vec![layer.id]));
        }
        previous_gpu_masked = gpu_masked;
        previous_gpu_blended = gpu_blended;
        previous_parent_id = layer.parent_id;
    }
    runs
}

pub fn gpu_layer_object(
    stack: &LayerStack,
    id: u32,
) -> Option<&crate::core::vector::object::VectorObjectData> {
    stack.layers.iter().find_map(|layer| {
        if layer.id != id {
            return None;
        }
        match &layer.layer_type {
            LayerType::Vector(VectorGeometry::Path(object)) => Some(object),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::layer::{Layer, LayerType};
    use crate::core::vector::object::{VectorGeometry, VectorObjectData};

    #[test]
    fn preserves_interleaved_z_order() {
        let mut stack = LayerStack::new(1, 1);
        let mut raster_a = Layer::new(1, "r1", 1, 1);
        raster_a.tiles.set_pixel(0, 0, 0, 0, 0, 255);
        let mut vector = Layer::new(2, "v", 1, 1);
        vector.layer_type = LayerType::Vector(VectorGeometry::Path(VectorObjectData::default()));
        vector.tiles.set_pixel(0, 0, 0, 0, 0, 255);
        let mut raster_b = Layer::new(3, "r2", 1, 1);
        raster_b.tiles.set_pixel(0, 0, 0, 0, 0, 255);
        stack.layers = vec![raster_a, vector, raster_b];
        assert_eq!(
            plan_runs(&stack, true),
            vec![
                SceneRun::Raster(vec![1]),
                SceneRun::GpuVector(vec![2]),
                SceneRun::Raster(vec![3])
            ]
        );
    }

    #[test]
    fn pass_through_group_child_preserves_inline_z_order() {
        let mut stack = LayerStack::new(1, 1);
        let mut raster = Layer::new(1, "below", 1, 1);
        raster.tiles.set_pixel(0, 0, 0, 0, 0, 255);
        let mut vector = Layer::new(2, "child", 1, 1);
        vector.layer_type = LayerType::Vector(VectorGeometry::Path(VectorObjectData::default()));
        vector.parent_id = Some(3);
        vector.tiles.set_pixel(0, 0, 0, 0, 0, 255);
        let group = Layer::new_group(3, "plain", 1, 1);
        stack.layers = vec![raster, vector, group];

        assert_eq!(
            plan_runs(&stack, true),
            vec![SceneRun::Raster(vec![1]), SceneRun::GpuVector(vec![2])]
        );

        // Reference image: a plain group is defined as byte-identical to its
        // children composited inline. The raster twins provide the CPU oracle;
        // the GPU planner above must preserve that same ordering.
        let grouped_reference = stack.flatten(1, 1);
        let mut inline = stack.clone();
        inline.layers.retain(|layer| !layer.is_group());
        inline.layers[1].parent_id = None;
        assert_eq!(grouped_reference, inline.flatten(1, 1));
    }

    #[test]
    fn masked_vectors_are_isolated_runs() {
        let mut stack = LayerStack::new(1, 1);
        let make_vector = |id| {
            let mut layer = Layer::new(id, "v", 1, 1);
            layer.layer_type = LayerType::Vector(VectorGeometry::Path(VectorObjectData::default()));
            layer.tiles.set_pixel(0, 0, 0, 0, 0, 255);
            layer
        };
        let below = make_vector(1);
        let mut masked = make_vector(2);
        masked.mask = Some(crate::core::layer::LayerMask::new_white(1, 1));
        let above = make_vector(3);
        stack.layers = vec![below, masked, above];
        assert_eq!(
            plan_runs(&stack, true),
            vec![
                SceneRun::GpuVector(vec![1]),
                SceneRun::GpuVector(vec![2]),
                SceneRun::GpuVector(vec![3]),
            ]
        );
    }

    #[test]
    fn blended_vector_is_isolated_from_normal_neighbours() {
        let mut stack = LayerStack::new(1, 1);
        let make_vector = |id| {
            let mut layer = Layer::new(id, "v", 1, 1);
            layer.layer_type = LayerType::Vector(VectorGeometry::Path(VectorObjectData::default()));
            layer.tiles.set_pixel(0, 0, 0, 0, 0, 255);
            layer
        };
        let below = make_vector(1);
        let mut multiply = make_vector(2);
        multiply.blend_mode = crate::core::blend::BlendMode::Multiply;
        let above = make_vector(3);
        stack.layers = vec![below, multiply, above];
        assert_eq!(
            plan_runs(&stack, true),
            vec![
                SceneRun::GpuVector(vec![1]),
                SceneRun::GpuVector(vec![2]),
                SceneRun::GpuVector(vec![3]),
            ]
        );
    }

    #[test]
    fn powerclip_child_stays_between_frame_and_upper_raster() {
        let mut stack = LayerStack::new(1, 1);
        let mut frame = Layer::new(1, "frame", 1, 1);
        frame.tiles.set_pixel(0, 0, 255, 255, 255, 255);
        let mut child = Layer::new(2, "clip content", 1, 1);
        child.layer_type = LayerType::Vector(VectorGeometry::Path(VectorObjectData::default()));
        child.tiles.set_pixel(0, 0, 0, 0, 0, 255);
        child.clip_parent_id = Some(frame.id);
        child.mask = Some(crate::core::layer::LayerMask::new_white(1, 1));
        let mut upper = Layer::new(3, "upper", 1, 1);
        upper.tiles.set_pixel(0, 0, 0, 0, 0, 255);
        stack.layers = vec![frame, child, upper];
        assert_eq!(
            plan_runs(&stack, true),
            vec![
                SceneRun::Raster(vec![1]),
                SceneRun::GpuVector(vec![2]),
                SceneRun::Raster(vec![3]),
            ]
        );
    }

    #[test]
    fn opacity_group_children_form_one_isolated_run_in_z_order() {
        let mut stack = LayerStack::new(1, 1);
        let mut below = Layer::new(1, "below", 1, 1);
        below.tiles.set_pixel(0, 0, 0, 0, 0, 255);
        let make_child = |id| {
            let mut child = Layer::new(id, "child", 1, 1);
            child.layer_type = LayerType::Vector(VectorGeometry::Path(VectorObjectData::default()));
            child.parent_id = Some(4);
            child.tiles.set_pixel(0, 0, 0, 0, 0, 255);
            child
        };
        let mut group = Layer::new_group(4, "opacity", 1, 1);
        group.opacity = 0.5;
        let mut above = Layer::new(5, "above", 1, 1);
        above.tiles.set_pixel(0, 0, 0, 0, 0, 255);
        stack.layers = vec![below, make_child(2), make_child(3), group, above];
        assert_eq!(
            plan_runs(&stack, true),
            vec![
                SceneRun::Raster(vec![1]),
                SceneRun::GpuVector(vec![2, 3]),
                SceneRun::Raster(vec![5]),
            ]
        );
    }

    #[test]
    fn blended_group_children_form_one_isolated_run_in_z_order() {
        let mut stack = LayerStack::new(1, 1);
        let mut below = Layer::new(1, "below", 1, 1);
        below.tiles.set_pixel(0, 0, 255, 255, 255, 255);
        let make_child = |id| {
            let mut child = Layer::new(id, "child", 1, 1);
            child.layer_type = LayerType::Vector(VectorGeometry::Path(VectorObjectData::default()));
            child.parent_id = Some(4);
            child.tiles.set_pixel(0, 0, 0, 0, 0, 255);
            child
        };
        let mut group = Layer::new_group(4, "multiply", 1, 1);
        group.blend_mode = crate::core::blend::BlendMode::Multiply;
        let mut above = Layer::new(5, "above", 1, 1);
        above.tiles.set_pixel(0, 0, 0, 0, 0, 255);
        stack.layers = vec![below, make_child(2), make_child(3), group, above];

        assert_eq!(
            plan_runs(&stack, true),
            vec![
                SceneRun::Raster(vec![1]),
                SceneRun::GpuVector(vec![2, 3]),
                SceneRun::Raster(vec![5]),
            ]
        );
    }

    #[test]
    fn masked_group_children_form_one_isolated_run_and_match_cpu_mask_semantics() {
        let mut stack = LayerStack::new(1, 1);
        let mut below = Layer::new(1, "below", 1, 1);
        below.tiles.set_pixel(0, 0, 255, 255, 255, 255);
        let mut child = Layer::new(2, "child", 1, 1);
        child.layer_type = LayerType::Vector(VectorGeometry::Path(VectorObjectData::default()));
        child.parent_id = Some(3);
        child.tiles.set_pixel(0, 0, 255, 0, 0, 255);
        let mut group = Layer::new_group(3, "masked", 1, 1);
        group.mask = Some(crate::core::layer::LayerMask::new_black(1, 1));
        stack.layers = vec![below, child, group];

        assert_eq!(
            plan_runs(&stack, true),
            vec![SceneRun::Raster(vec![1]), SceneRun::GpuVector(vec![2]),]
        );
        assert_eq!(
            stack.flatten(1, 1),
            vec![255, 255, 255, 255],
            "the CPU oracle applies the black group mask after resolving children"
        );
    }
}

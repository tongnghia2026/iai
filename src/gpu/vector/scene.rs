use crate::core::layer::{LayerStack, LayerType};
use crate::core::vector::object::VectorGeometry;

use super::eligibility::{layer_eligibility, Eligibility};

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
    for (index, layer) in stack.layers.iter().enumerate() {
        if layer.is_group()
            || !stack.is_effectively_visible(index)
            || layer.opacity <= 0.001
            || !layer.has_renderable_content()
        {
            continue;
        }
        let gpu = matches!(layer_eligibility(layer, enabled), Eligibility::GpuVector);
        if runs.last().is_some_and(|run| run.same_kind(gpu)) {
            runs.last_mut().unwrap().push(layer.id);
        } else if gpu {
            runs.push(SceneRun::GpuVector(vec![layer.id]));
        } else {
            runs.push(SceneRun::Raster(vec![layer.id]));
        }
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
}

use super::render::CanvasEvent;
use super::state::App;
use crate::core::command_vector::{CreatePathLayer, ReplacePathGeometry};
use crate::core::gateway::ChangeKind;
use crate::core::layer::LayerType;
use crate::core::vector::from_shape::{elbow_connector_path, ConnectorRoute};
use crate::core::vector::object::VectorGeometry;
use crate::core::vector::style::ArrowHead;

impl App {
    /// Settings represented by the selected Arrow layer, if the active layer is
    /// one of the open, arrow-headed Paths created by this tool.
    pub fn active_arrow_settings(&self) -> Option<(f32, u8, u8)> {
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let layer = canvas
            .layer_stack
            .layers
            .get(canvas.layer_stack.active_idx)?;
        if !layer.selected || !layer.visible || layer.locked || layer.is_background {
            return None;
        }
        let LayerType::Vector(VectorGeometry::Path(object)) = &layer.layer_type else {
            return None;
        };
        if object.style.stroke_style.end_arrow.kind == ArrowHead::None {
            return None;
        }
        let contour = object.path.contours.as_slice();
        let [contour] = contour else { return None };
        if contour.closed || !(2..=4).contains(&contour.nodes.len()) {
            return None;
        }
        let route = match contour.nodes.len() {
            2 => 0,
            3 => {
                let start = contour.nodes[0].anchor;
                let bend = contour.nodes[1].anchor;
                if (bend.y - start.y).abs() <= 0.001 {
                    1
                } else {
                    2
                }
            }
            4 => 3,
            _ => unreachable!(),
        };
        Some((
            object.style.stroke_style.width,
            object.style.stroke_style.end_arrow.kind.to_u8(),
            route,
        ))
    }

    /// Re-route the selected Arrow while preserving its endpoints, style and
    /// object transform. The gateway records the geometry replacement for undo.
    pub fn set_active_arrow_route(&mut self, route: u8) {
        if self.active_arrow_settings().is_none() {
            return;
        }
        self.path_style_commit();
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let idx = canvas.layer_stack.active_idx;
        let layer = &mut canvas.layer_stack.layers[idx];
        crate::core::command_vector::fold_offset_into_model(layer);
        let LayerType::Vector(VectorGeometry::Path(object)) = &layer.layer_type else {
            return;
        };
        let Some(contour) = object.path.contours.first() else {
            return;
        };
        let (Some(start), Some(end)) = (contour.nodes.first(), contour.nodes.last()) else {
            return;
        };
        let path = elbow_connector_path(
            start.anchor.x,
            start.anchor.y,
            end.anchor.x,
            end.anchor.y,
            ConnectorRoute::from_u8(route),
        );
        let layer_id = layer.id;
        if canvas
            .execute(
                Box::new(ReplacePathGeometry::new(layer_id, path)),
                ChangeKind::LayerStructure,
            )
            .is_err()
        {
            return;
        }
        canvas.reconcile_path_ink();
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }

    pub fn commit_arrow(&mut self) {
        let fg = self.edit.fg_color;
        let Some(object) = self.edit.tools.arrow_mut().take_arrow_object(fg) else {
            return;
        };
        let doc_idx = self.docs.active_doc_idx;
        let canvas = &mut self.docs.documents[doc_idx].canvas;
        if canvas
            .execute(
                Box::new(CreatePathLayer::new(object, "Arrow")),
                ChangeKind::LayerStructure,
            )
            .is_err()
        {
            return;
        }
        let new_idx = canvas.layer_stack.active_idx;
        for layer in &mut canvas.layer_stack.layers {
            layer.selected = false;
        }
        if let Some(layer) = canvas.layer_stack.layers.get_mut(new_idx) {
            layer.selected = true;
        }
        canvas.reconcile_path_ink();
        canvas.layer_revision += 1;
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.shell.status_msg = "Arrow / connector created".to_string();
    }
}

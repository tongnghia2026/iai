// Vector Brush → editable Path layer (Phase 6B).
//
// The Vector Brush tool captures a pressure-aware centerline during a drag; on
// release the App turns it into a new Path layer carrying a `BrushStroke`
// appearance. Like the Pen's "commit as Path" (see `pen_ops`), adding a layer
// must run through the gateway (undoable) and then refresh the compositor — work
// that lives at the App level, not in the tool. Expand Stroke bakes the ribbon
// into a closed outline, also as one undo step.

use super::render::CanvasEvent;
use super::state::App;
use crate::core::command_vector::{CreatePathLayer, ExpandVectorStroke};
use crate::core::gateway::ChangeKind;
use crate::core::layer::LayerType;
use crate::core::vector::object::VectorGeometry;

impl App {
    /// Commit the Vector Brush tool's just-drawn stroke as a new editable Path
    /// layer. Does nothing when the stroke is too short. Undoable; the new layer
    /// becomes the sole selection so the Move/Node transform box shows at once.
    pub fn commit_vector_brush_stroke(&mut self) {
        let fg = self.edit.fg_color;
        let Some(object) = self.edit.tools.vector_brush_mut().take_stroke_object(fg) else {
            return;
        };
        let doc_idx = self.docs.active_doc_idx;
        let canvas = &mut self.docs.documents[doc_idx].canvas;
        let created = canvas
            .execute(
                Box::new(CreatePathLayer::new(object, "Brush")),
                ChangeKind::LayerStructure,
            )
            .is_ok();
        if !created {
            return;
        }
        // Select only the freshly created stroke (mirrors `commit_pen_as_path`).
        let canvas = &mut self.docs.documents[doc_idx].canvas;
        let new_idx = canvas.layer_stack.active_idx;
        for l in &mut canvas.layer_stack.layers {
            l.selected = false;
        }
        if let Some(l) = canvas.layer_stack.layers.get_mut(new_idx) {
            l.selected = true;
        }
        // CMYK: re-derive ink planes for the new Path raster from the RGB mirror.
        canvas.reconcile_path_ink();
        canvas.layer_revision += 1;
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.shell.status_msg = "Vector brush stroke".to_string();
    }

    /// The active layer's id if it is a Path holding a Vector Brush stroke, else
    /// `None`. Used to enable/route the Expand Stroke command.
    pub fn active_brush_layer_id(&self) -> Option<u32> {
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let layer = canvas
            .layer_stack
            .layers
            .get(canvas.layer_stack.active_idx)?;
        match &layer.layer_type {
            LayerType::Vector(VectorGeometry::Path(obj)) if obj.is_brush() => Some(layer.id),
            _ => None,
        }
    }

    /// Bake the active Vector Brush stroke into a closed-outline Path (Expand
    /// Stroke), so it can be filled/booleaned like any object. One undo step.
    pub fn expand_active_brush_stroke(&mut self) {
        let Some(id) = self.active_brush_layer_id() else {
            self.shell.status_msg =
                "Select a vector brush stroke to convert into an outline".to_string();
            return;
        };
        let doc_idx = self.docs.active_doc_idx;
        let canvas = &mut self.docs.documents[doc_idx].canvas;
        match canvas.execute(
            Box::new(ExpandVectorStroke::new(id)),
            ChangeKind::LayerStructure,
        ) {
            Ok(_) => {
                canvas.reconcile_path_ink();
                canvas.layer_revision += 1;
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
                self.shell.status_msg = "Converted the brush stroke into an outline".to_string();
            }
            Err(e) => {
                self.shell.status_msg = e.message;
            }
        }
    }
}

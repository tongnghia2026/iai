// Pen tool → editable Path layer commit (Bước 4 / T4.4).
//
// The Pen tool's Selection/Fill/Stroke commits mutate the active canvas directly
// (tools/pen.rs). Committing as a vector Path layer instead ADDS a layer, which
// must run through the gateway (undoable) and then refresh the compositor like
// any layer-structure change — work that lives at the App level, not inside the
// tool. Mirrors `shape_ops::begin_new_shape`.

use super::render::CanvasEvent;
use super::state::App;
use crate::core::command_vector::CreatePathLayer;
use crate::core::gateway::ChangeKind;

impl App {
    /// Commit the Pen tool's in-progress path as a new editable Path layer, the
    /// vector alternative to its raster Selection/Fill/Stroke commits. Does
    /// nothing when the path has too few anchors. Undoable (CreatePathLayer runs
    /// through `Canvas::execute`); the new layer becomes active.
    pub fn commit_pen_as_path(&mut self) {
        let Some(object) = self.edit.tools.pen_mut().take_path_object() else {
            return;
        };
        let doc_idx = self.docs.active_doc_idx;
        let canvas = &mut self.docs.documents[doc_idx].canvas;
        let created = canvas
            .execute(
                Box::new(CreatePathLayer::new(object, "Path")),
                ChangeKind::LayerStructure,
            )
            .is_ok();
        if created {
            // Make the freshly created Path the sole selection so the Move tool's
            // on-canvas transform box (which only shows for a SELECTED active Path,
            // see `active_path_transform_box`) appears immediately without an extra
            // click. `add_layer` already pointed `active_idx` at it.
            let canvas = &mut self.docs.documents[doc_idx].canvas;
            let new_idx = canvas.layer_stack.active_idx;
            for l in &mut canvas.layer_stack.layers {
                l.selected = false;
            }
            if let Some(l) = canvas.layer_stack.layers.get_mut(new_idx) {
                l.selected = true;
            }
            canvas.layer_revision += 1;
            // Rebuilds the GPU atlas / flattens for the added layer and schedules
            // a redraw (LayerStructureChanged owns that bookkeeping).
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        }
    }
}

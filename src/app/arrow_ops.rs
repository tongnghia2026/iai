use super::render::CanvasEvent;
use super::state::App;
use crate::core::command_vector::CreatePathLayer;
use crate::core::gateway::ChangeKind;

impl App {
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

//! Trial of MediaPipe face landmarks (`core::ai::face_mesh`) from the AI panel.

use crate::app::state::App;

impl App {
    /// Trial: fit MediaPipe face landmarks to every face in the flattened image
    /// and add them as an undoable overlay layer.
    pub fn do_face_mesh_trial(&mut self) {
        if self.has_only_welcome_placeholder() {
            self.shell.ui.ai_status = "Hãy mở một ảnh trước".to_string();
            return;
        }
        let doc_idx = self.docs.active_doc_idx;
        let doc_id = self.docs.documents[doc_idx].id.0;
        let canvas = &self.docs.documents[doc_idx].canvas;
        let (width, height) = (canvas.width, canvas.height);
        let rgba = canvas.flatten_for_export();
        let started = std::time::Instant::now();
        let status = match crate::core::ai::face_mesh::detect(&rgba, width, height) {
            Ok(meshes) if meshes.is_empty() => "Mốc mặt: không tìm thấy khuôn mặt nào".to_string(),
            Ok(meshes) => {
                let elapsed = started.elapsed().as_millis();
                let overlay = crate::core::ai::face_mesh::render_overlay(width, height, &meshes);
                let placed = self.place_ai_result_named(
                    Some(doc_id),
                    overlay,
                    width,
                    height,
                    false,
                    "Mốc mặt MediaPipe (thử)",
                );
                format!(
                    "Mốc mặt: {} khuôn mặt, {} điểm mỗi mặt, {elapsed} ms — {placed}",
                    meshes.len(),
                    crate::core::ai::face_mesh::LANDMARK_COUNT
                )
            }
            Err(error) => format!("Mốc mặt lỗi: {error}"),
        };
        self.shell.status_msg = status.clone();
        self.shell.ui.ai_status = status;
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }
}

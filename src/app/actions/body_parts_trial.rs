//! Trial of Sapiens2 body-part segmentation (`core::ai::body_parts`) cross-checked
//! against the face mesh, from the AI panel. Runs on a worker thread.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Mutex;
use std::time::Instant;

use crate::app::state::App;
use crate::core::ai::{body_parts, face_mesh};

/// Share of mesh landmarks that must land on face-like classes to trust a face.
const TRUSTED_AGREEMENT: f32 = 0.8;

struct TrialOutput {
    parts: Vec<u8>,
    mesh: Vec<u8>,
    width: u32,
    height: u32,
    status: String,
}

type TrialResult = Result<TrialOutput, String>;

static PENDING: Mutex<Option<(u32, Receiver<TrialResult>)>> = Mutex::new(None);

fn run(rgba: Vec<u8>, width: u32, height: u32) -> TrialResult {
    let started = Instant::now();
    let meshes = face_mesh::detect(&rgba, width, height)?;
    if meshes.is_empty() {
        return Err("không tìm thấy khuôn mặt nào".to_string());
    }
    let mesh_ms = started.elapsed().as_millis();
    let started = Instant::now();
    let mut segmenter = body_parts::Segmenter::load(crate::core::ai::ort_ep::prefer_gpu())?;
    let load_ms = started.elapsed().as_millis();
    let started = Instant::now();
    let mut parts = Vec::with_capacity(meshes.len());
    for mesh in &meshes {
        parts.push(segmenter.segment_face(&rgba, width, height, mesh)?);
    }
    let segment_ms = started.elapsed().as_millis();
    let agreements: Vec<String> = parts
        .iter()
        .map(|part| format!("{:.0}%", part.agreement * 100.0))
        .collect();
    let doubtful = parts
        .iter()
        .filter(|part| part.agreement < TRUSTED_AGREEMENT)
        .count();
    let mut status = format!(
        "Tách vùng: {} mặt — mốc mặt {mesh_ms} ms, nạp model {load_ms} ms, tách vùng {segment_ms} ms ({}); khớp với MediaPipe: {}",
        meshes.len(),
        if segmenter.on_gpu { "GPU" } else { "CPU" },
        agreements.join(", ")
    );
    if doubtful > 0 {
        status.push_str(&format!(
            " — {doubtful} mặt lệch (gạch đỏ): phần này sẽ dùng mốc MediaPipe"
        ));
    }
    Ok(TrialOutput {
        parts: body_parts::render_overlay(width, height, &parts, TRUSTED_AGREEMENT),
        mesh: face_mesh::render_overlay(width, height, &meshes),
        width,
        height,
        status,
    })
}

impl App {
    pub fn do_body_parts_trial(&mut self) {
        if self.has_only_welcome_placeholder() {
            self.shell.ui.ai_status = "Hãy mở một ảnh trước".to_string();
            return;
        }
        let mut pending = PENDING.lock().unwrap();
        if pending.is_some() {
            self.shell.ui.ai_status = "Tách vùng đang chạy — đợi xong đã".to_string();
            return;
        }
        let doc = &self.docs.documents[self.docs.active_doc_idx];
        let doc_id = doc.id.0;
        let (width, height) = (doc.canvas.width, doc.canvas.height);
        let rgba = doc.canvas.flatten_for_export();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(run(rgba, width, height));
        });
        *pending = Some((doc_id, receiver));
        self.shell.ui.ai_status =
            "Đang tách vùng (Sapiens2 + MediaPipe)… mỗi mặt vài giây".to_string();
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }

    /// Place a finished trial as two undoable layers; keep repainting while it runs.
    pub fn poll_body_parts_trial(&mut self) {
        let finished = {
            let mut pending = PENDING.lock().unwrap();
            let Some((doc_id, receiver)) = pending.take() else {
                return;
            };
            match receiver.try_recv() {
                Ok(result) => Some((doc_id, result)),
                Err(TryRecvError::Empty) => {
                    *pending = Some((doc_id, receiver));
                    None
                }
                Err(TryRecvError::Disconnected) => {
                    Some((doc_id, Err("luồng tách vùng dừng bất thường".to_string())))
                }
            }
        };
        let Some((doc_id, result)) = finished else {
            if let Some(window) = &self.win.window {
                window.request_redraw();
            }
            return;
        };
        let status = match result {
            Ok(output) => {
                let placed = self.place_ai_result_named(
                    Some(doc_id),
                    output.parts,
                    output.width,
                    output.height,
                    false,
                    "Vùng Sapiens2 (thử)",
                );
                self.place_ai_result_named(
                    Some(doc_id),
                    output.mesh,
                    output.width,
                    output.height,
                    false,
                    "Mốc mặt MediaPipe (thử)",
                );
                format!("{} — {placed}", output.status)
            }
            Err(error) => format!("Tách vùng lỗi: {error}"),
        };
        self.shell.status_msg = status.clone();
        self.shell.ui.ai_status = status;
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }
}

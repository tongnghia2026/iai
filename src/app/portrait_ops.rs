//! "Chỉnh chân dung" (Image ▸ Chỉnh chân dung…): skin, blemish, under-eye, eye
//! and teeth retouching with a live canvas preview. The photo is analysed once
//! on a worker thread (`core::portrait::analyze`); each slider change then only
//! recombines the cached layers of the analysis. OK adds the result as a new
//! layer above the source, holding just the retouched pixels.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};

use super::render::CanvasEvent;
use super::state::App;
use crate::core::portrait::{self, PortraitModel, PortraitSettings};
use crate::core::tile::TileMap;

const RESULT_LAYER: &str = "Chân dung";

pub struct PortraitSession {
    pub doc_id: crate::core::document::DocumentId,
    pub layer_id: u32,
    pub w: u32,
    pub h: u32,
    pub original_tiles: TileMap,
    pub src: Arc<Vec<u8>>,
    /// Latest progress line from the analysis worker.
    pub progress: Arc<Mutex<String>>,
    pub rx: Option<Receiver<Result<PortraitModel, String>>>,
    pub model: Option<Arc<PortraitModel>>,
    pub error: Option<String>,
    /// What the dialog last asked to see, and what the canvas shows now.
    pub wanted: Option<(PortraitSettings, Vec<bool>, bool, bool)>,
    pub shown: Option<(PortraitSettings, Vec<bool>, bool, bool)>,
}

impl App {
    /// Open the dialog's session: snapshot the active raster layer and start
    /// the analysis in the background. Returns a message when the layer cannot
    /// be retouched.
    pub(crate) fn begin_portrait(&mut self) -> Result<(), String> {
        self.cancel_portrait();
        let idx = self.docs.active_doc_idx;
        let canvas = &self.docs.documents[idx].canvas;
        if canvas.is_cmyk() {
            return Err("Chỉnh chân dung chưa hỗ trợ chế độ CMYK".to_string());
        }
        let Some(layer) = canvas.layer_stack.layers.get(canvas.layer_stack.active_idx) else {
            return Err("Không có layer để chỉnh".to_string());
        };
        if (!layer.is_background && layer.locked) || !layer.is_raster() {
            return Err("Hãy chọn layer ảnh (không khóa) để chỉnh chân dung".to_string());
        }
        let (w, h) = (layer.width, layer.height);
        let src = Arc::new(layer.flatten_tiles());
        if w == 0 || h == 0 || src.len() != w as usize * h as usize * 4 {
            return Err("Layer ảnh không hợp lệ".to_string());
        }
        let progress = Arc::new(Mutex::new("Đang chuẩn bị…".to_string()));
        let (tx, rx) = mpsc::channel();
        {
            let src = Arc::clone(&src);
            let progress = Arc::clone(&progress);
            let prefer_gpu = crate::core::ai::ort_ep::prefer_gpu();
            std::thread::spawn(move || {
                let report = |line: String| {
                    if let Ok(mut slot) = progress.lock() {
                        *slot = line;
                    }
                };
                let _ = tx.send(portrait::analyze(&src, w, h, prefer_gpu, &report));
            });
        }
        self.shell.portrait = Some(PortraitSession {
            doc_id: self.docs.documents[idx].id,
            layer_id: layer.id,
            w,
            h,
            original_tiles: layer.tiles.clone(),
            src,
            progress,
            rx: Some(rx),
            model: None,
            error: None,
            wanted: None,
            shown: None,
        });
        Ok(())
    }

    /// Collect a finished analysis; keep repainting while it runs so the
    /// progress line stays live.
    pub(crate) fn poll_portrait(&mut self) {
        let finished = {
            let Some(session) = self.shell.portrait.as_mut() else {
                return;
            };
            let Some(rx) = session.rx.take() else {
                return;
            };
            match rx.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => {
                    session.rx = Some(rx);
                    None
                }
                Err(TryRecvError::Disconnected) => {
                    Some(Err("phân tích dừng bất thường".to_string()))
                }
            }
        };
        match finished {
            Some(Ok(model)) => {
                if let Some(session) = self.shell.portrait.as_mut() {
                    session.model = Some(Arc::new(model));
                }
                self.refresh_portrait_preview();
            }
            Some(Err(error)) => {
                if let Some(session) = self.shell.portrait.as_mut() {
                    session.error = Some(error);
                }
            }
            None => {}
        }
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }

    /// The dialog streams its sliders every frame; re-render only on change.
    /// `preview` off shows the photo; `masks` tints the detected areas instead.
    pub(crate) fn set_portrait_preview(
        &mut self,
        settings: PortraitSettings,
        enabled: Vec<bool>,
        preview: bool,
        masks: bool,
    ) {
        if let Some(session) = self.shell.portrait.as_mut() {
            session.wanted = Some((settings, enabled, preview, masks));
        }
        self.refresh_portrait_preview();
    }

    fn refresh_portrait_preview(&mut self) {
        let idx = self.docs.active_doc_idx;
        let Some(session) = self.shell.portrait.as_ref() else {
            return;
        };
        if session.doc_id != self.docs.documents[idx].id || session.wanted == session.shown {
            return;
        }
        let Some((settings, enabled, preview, masks)) = session.wanted.clone() else {
            return;
        };
        let Some(model) = session.model.clone() else {
            return;
        };
        let mut tiles = session.original_tiles.clone();
        let shown = if masks {
            portrait::render_masks(&session.src, &model, &enabled)
        } else if preview {
            portrait::render(&session.src, &model, &settings, &enabled)
        } else {
            None
        };
        if let Some((region, pixels)) = shown {
            tiles.write_region(region.x, region.y, region.w, region.h, &pixels);
        }
        let layer_id = session.layer_id;
        self.docs.documents[idx]
            .canvas
            .preview_layer_tiles(layer_id, tiles);
        self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
        if let Some(session) = self.shell.portrait.as_mut() {
            session.shown = Some((settings, enabled, preview, masks));
        }
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }

    /// Drop the session and put the layer back as it was.
    pub(crate) fn cancel_portrait(&mut self) {
        let Some(session) = self.shell.portrait.take() else {
            return;
        };
        if let Some(doc) = self
            .docs
            .documents
            .iter_mut()
            .find(|d| d.id == session.doc_id)
        {
            doc.canvas
                .restore_layer_tiles(session.layer_id, session.original_tiles);
        }
        self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }

    /// Add the retouch as a new layer above the source (only the changed
    /// pixels are opaque, so layer opacity scales the whole retouch).
    pub(crate) fn apply_portrait(
        &mut self,
        settings: PortraitSettings,
        enabled: Vec<bool>,
    ) -> Result<(), String> {
        let (doc_id, layer_id, w, h, src, model) = {
            let Some(session) = self.shell.portrait.as_ref() else {
                return Err("Chưa mở chỉnh chân dung".to_string());
            };
            let Some(model) = session.model.clone() else {
                return Err("Đang phân tích ảnh — đợi xong rồi bấm Áp dụng".to_string());
            };
            (
                session.doc_id,
                session.layer_id,
                session.w,
                session.h,
                Arc::clone(&session.src),
                model,
            )
        };
        self.cancel_portrait();
        let Some((region, pixels)) = portrait::render(&src, &model, &settings, &enabled) else {
            return Err("Không có khuôn mặt nào được chọn".to_string());
        };
        let mut patch = vec![0u8; pixels.len()];
        let mut changed = false;
        for row in 0..region.h as usize {
            for col in 0..region.w as usize {
                let o = (row * region.w as usize + col) * 4;
                let s = ((region.y as usize + row) * w as usize + region.x as usize + col) * 4;
                if pixels[o..o + 3] != src[s..s + 3] {
                    patch[o..o + 3].copy_from_slice(&pixels[o..o + 3]);
                    patch[o + 3] = src[s + 3];
                    changed = true;
                }
            }
        }
        if !changed {
            return Err("Các thanh trượt đang ở 0 — ảnh không đổi".to_string());
        }
        let Some(idx) = self.docs.documents.iter().position(|d| d.id == doc_id) else {
            return Err("Tài liệu đã đóng".to_string());
        };
        let canvas = &mut self.docs.documents[idx].canvas;
        let Some(source_idx) = canvas
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == layer_id)
        else {
            return Err("Layer gốc không còn".to_string());
        };
        let offset = canvas.layer_stack.layers[source_idx].offset;
        let (cw, ch) = (canvas.width, canvas.height);
        let mut tiles = TileMap::new(w, h);
        tiles.write_region(region.x, region.y, region.w, region.h, &patch);
        let mut cmd = crate::core::command::LayerStructureCommand::capture_before(
            RESULT_LAYER,
            &canvas.layer_stack,
            cw,
            ch,
        );
        for layer in &mut canvas.layer_stack.layers {
            layer.selected = false;
        }
        canvas.layer_stack.active_idx = source_idx;
        let new_idx = canvas.layer_stack.add_layer(w, h);
        {
            let layer = &mut canvas.layer_stack.layers[new_idx];
            layer.name = RESULT_LAYER.to_string();
            layer.tiles = tiles;
            layer.offset = offset;
            layer.selected = true;
        }
        canvas.layer_stack.active_idx = new_idx;
        cmd.capture_after(&canvas.layer_stack, cw, ch);
        canvas.record(Box::new(cmd));
        canvas.layer_revision += 1;
        if idx == self.docs.active_doc_idx {
            self.upload_full();
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        }
        Ok(())
    }

    /// Dialog view of the session: progress/status line, whether sliders can
    /// preview yet, and per face whether its part masks are trusted.
    pub(crate) fn portrait_dialog_state(&self) -> (String, bool, Vec<bool>) {
        let Some(session) = self.shell.portrait.as_ref() else {
            return ("Không chỉnh được layer này".to_string(), false, Vec::new());
        };
        if let Some(error) = &session.error {
            return (format!("Không chỉnh được: {error}"), false, Vec::new());
        }
        let Some(model) = &session.model else {
            let line = session
                .progress
                .lock()
                .map(|p| p.clone())
                .unwrap_or_default();
            return (line, false, Vec::new());
        };
        let faces: Vec<bool> = model
            .faces
            .iter()
            .map(|face| !model.parts_used || face.trusted())
            .collect();
        let seconds = model.timings.iter().sum::<u128>() as f32 / 1000.0;
        let mut line = format!(
            "Tìm thấy {} khuôn mặt · phân tích {seconds:.1} s",
            faces.len()
        );
        if !model.parts_used {
            line.push_str(" · chỉ dùng mốc mặt");
        }
        (line, true, faces)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::Canvas;
    use std::time::{Duration, Instant};

    fn app_with_photo() -> Option<App> {
        let path =
            std::path::Path::new("tmp/model-sources/gfpgan/inputs/whole_imgs/Blake_Lively.jpg");
        if !path.is_file() || crate::core::ai::face_mesh::model_path().is_none() {
            return None;
        }
        let image = image::open(path).ok()?.to_rgba8();
        let (w, h) = image.dimensions();
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        app.docs.documents[0].canvas = Canvas::from_rgba(image.into_raw(), w, h);
        Some(app)
    }

    fn photo_pixels(app: &App) -> Vec<u8> {
        app.docs.documents[0].canvas.layer_stack.layers[0]
            .tiles
            .flatten()
    }

    #[test]
    fn preview_toggles_and_apply_adds_one_undoable_layer() {
        let Some(mut app) = app_with_photo() else {
            return;
        };
        let original = photo_pixels(&app);
        let undo_before = app.docs.documents[0].canvas.undo_count();
        app.begin_portrait().unwrap();
        let started = Instant::now();
        while app
            .shell
            .portrait
            .as_ref()
            .is_some_and(|s| s.model.is_none() && s.error.is_none())
        {
            assert!(
                started.elapsed() < Duration::from_secs(180),
                "analysis hung"
            );
            std::thread::sleep(Duration::from_millis(50));
            app.poll_portrait();
        }
        let (status, ready, faces) = app.portrait_dialog_state();
        assert!(ready, "{status}");
        assert!(!faces.is_empty());
        let on = vec![true; faces.len()];

        app.set_portrait_preview(PortraitSettings::default(), on.clone(), true, false);
        assert_ne!(photo_pixels(&app), original, "preview shows the retouch");
        app.set_portrait_preview(PortraitSettings::default(), on.clone(), false, false);
        assert_eq!(photo_pixels(&app), original, "preview off shows the photo");
        app.set_portrait_preview(PortraitSettings::default(), on.clone(), true, false);

        app.apply_portrait(PortraitSettings::default(), on).unwrap();
        let canvas = &app.docs.documents[0].canvas;
        assert_eq!(canvas.layer_stack.layers.len(), 2);
        assert_eq!(canvas.layer_stack.layers[1].name, RESULT_LAYER);
        assert_eq!(photo_pixels(&app), original, "the photo layer is untouched");
        assert_eq!(canvas.undo_count(), undo_before + 1);
        assert!(app.shell.portrait.is_none());
    }

    #[test]
    fn cancel_while_analysing_restores_the_photo() {
        let Some(mut app) = app_with_photo() else {
            return;
        };
        let original = photo_pixels(&app);
        let undo_before = app.docs.documents[0].canvas.undo_count();
        app.begin_portrait().unwrap();
        app.cancel_portrait();
        assert!(app.shell.portrait.is_none());
        assert_eq!(photo_pixels(&app), original);
        assert_eq!(app.docs.documents[0].canvas.undo_count(), undo_before);
    }
}

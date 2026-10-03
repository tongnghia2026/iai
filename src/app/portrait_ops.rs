//! "Chỉnh chân dung" (Image ▸ Chỉnh chân dung…): skin, blemish, under-eye, eye
//! and teeth retouching with a live canvas preview. The photo is analysed once
//! on a worker thread (`core::portrait::analyze`); each slider change then only
//! recombines the cached layers of the analysis, also on a worker so dragging
//! never stalls the window (a drag skips to the latest values). The "Tô vùng"
//! brush (`portrait_brush`) edits the skin and hair masks in between. OK adds
//! the result as a new layer above the source, holding just the retouched
//! pixels and the recipe (sliders, faces, painted masks, selection) so the
//! dialog can reopen that layer and update it in place.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};

use super::portrait_brush::PortraitBrush;
use super::render::CanvasEvent;
use super::state::App;
use crate::core::layer::Layer;
use crate::core::portrait::looks::{self, LookLut};
use crate::core::portrait::{
    self, FaceEdits, PortraitModel, PortraitRecipe, PortraitSettings, Region,
};
use crate::core::tile::TileMap;

const RESULT_LAYER: &str = "Chân dung";

/// What the preview shows: settings, faces on, retouch on, areas tinted, and
/// a revision of what they act on: the brush edits, how many faces have
/// their AI detail and whether bodies are analysed.
type PreviewKey = (PortraitSettings, Vec<bool>, bool, bool, u64);
type Rendered = Option<(Region, Vec<u8>)>;

pub struct PortraitSession {
    pub doc_id: crate::core::document::DocumentId,
    pub layer_id: u32,
    pub w: u32,
    pub h: u32,
    /// Where the layer sits on the canvas.
    pub offset: (i32, i32),
    pub original_tiles: TileMap,
    pub src: Arc<Vec<u8>>,
    /// Latest progress line from the analysis worker.
    pub progress: Arc<Mutex<String>>,
    pub rx: Option<Receiver<Result<PortraitModel, String>>>,
    pub model: Option<Arc<PortraitModel>>,
    pub error: Option<String>,
    /// What the dialog last asked to see, and what the canvas shows now.
    pub wanted: Option<PreviewKey>,
    pub shown: Option<PreviewKey>,
    /// The preview render running on a worker, and what it will show.
    rendering: Option<(PreviewKey, Receiver<Rendered>)>,
    /// The body analysis running on a worker (started by the first body
    /// slider moved); it fills the model's bodies, then signals.
    body_rx: Option<Receiver<()>>,
    /// The AI face detail being made on a worker (started by "Chi tiết mặt
    /// (AI)" leaving 0); it fills the faces' detail, then signals.
    detail_rx: Option<Receiver<()>>,
    /// Masks painted with the brush, per face, and their revision.
    pub edits: Vec<FaceEdits>,
    pub edit_rev: u64,
    pub brush: PortraitBrush,
    /// The "Chân dung" layer being reopened, if any.
    pub reopened: Option<Reopened>,
    /// Its saved sliders and which faces were on, for the dialog to take
    /// once (the faces once the analysis has found them again).
    pub restore_settings: Option<PortraitSettings>,
    pub restore_faces: Option<Vec<bool>>,
}

/// A "Chân dung" layer reopened for more edits: hidden while the dialog
/// previews on the photo beneath it, shown again when the session ends.
pub struct Reopened {
    pub layer_id: u32,
    visible: bool,
    recipe: Arc<PortraitRecipe>,
}

impl App {
    /// Open the dialog's session: snapshot the active raster layer and start
    /// the analysis in the background. Returns a message when the layer cannot
    /// be retouched.
    pub(crate) fn begin_portrait(&mut self) -> Result<(), String> {
        self.cancel_portrait();
        let idx = self.docs.active_doc_idx;
        self.docs.documents[idx].canvas.selection.refresh_bbox();
        let canvas = &self.docs.documents[idx].canvas;
        if canvas.is_cmyk() {
            return Err("Chỉnh chân dung chưa hỗ trợ chế độ CMYK".to_string());
        }
        let (source_idx, reopen) =
            reopen_target(&canvas.layer_stack.layers, canvas.layer_stack.active_idx)?;
        let Some(layer) = canvas.layer_stack.layers.get(source_idx) else {
            return Err("Không có layer để chỉnh".to_string());
        };
        if (!layer.is_background && layer.locked) || !layer.is_raster() {
            return Err("Hãy chọn layer ảnh (không khóa) để chỉnh chân dung".to_string());
        }
        let (w, h) = (layer.width, layer.height);
        let offset = layer.offset;
        let clip = if canvas.selection.active {
            Some(
                selection_clip(&canvas.selection, offset, w, h)
                    .ok_or_else(|| "Vùng chọn nằm ngoài layer ảnh".to_string())?,
            )
        } else {
            // Reopening keeps to the selection it was made with.
            reopen
                .as_ref()
                .and_then(|(_, recipe)| recipe.clip.clone())
                .filter(|c| {
                    let r = c.region;
                    r.x + r.w <= w && r.y + r.h <= h && c.mask.len() == r.len()
                })
        };
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
                let _ = tx.send(portrait::analyze(&src, w, h, prefer_gpu, clip, &report));
            });
        }
        let layer_id = layer.id;
        let original_tiles = layer.tiles.clone();
        // The reopened layer would cover the preview drawn on the photo.
        let reopened = reopen.map(|(result_idx, recipe)| {
            let canvas = &mut self.docs.documents[idx].canvas;
            let layer = &mut canvas.layer_stack.layers[result_idx];
            let reopened = Reopened {
                layer_id: layer.id,
                visible: layer.visible,
                recipe,
            };
            layer.visible = false;
            canvas.layer_revision += 1;
            reopened
        });
        // A reopened layer brings back its own sliders; a new photo starts
        // from the defaults, never from the last photo's (not everyone wants
        // a slimmer face or lipstick).
        let restore_settings = Some(
            reopened
                .as_ref()
                .map_or_else(PortraitSettings::default, |r| r.recipe.settings),
        );
        let restore_faces = reopened.is_none().then(Vec::new);
        if reopened.is_some() {
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        }
        self.shell.portrait = Some(PortraitSession {
            doc_id: self.docs.documents[idx].id,
            layer_id,
            w,
            h,
            offset,
            original_tiles,
            src,
            progress,
            rx: Some(rx),
            model: None,
            error: None,
            wanted: None,
            shown: None,
            rendering: None,
            body_rx: None,
            detail_rx: None,
            edits: Vec::new(),
            edit_rev: 0,
            brush: PortraitBrush::default(),
            reopened,
            restore_settings,
            restore_faces,
        });
        Ok(())
    }

    /// Make the "Chân dung" layer at `idx` the active one and reopen it in
    /// the dialog (a double-click on it in the Layers panel).
    pub(crate) fn reopen_portrait_layer(&mut self, idx: usize) -> Result<(), String> {
        let stack = &mut self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack;
        if stack.layers.get(idx).is_none_or(|l| l.portrait.is_none()) {
            return Err("Layer này không phải layer \"Chân dung\"".to_string());
        }
        for (i, layer) in stack.layers.iter_mut().enumerate() {
            layer.selected = i == idx;
        }
        stack.active_idx = idx;
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.begin_portrait()
    }

    /// Collect a finished analysis or preview render; keep repainting while
    /// either runs so the progress line and the preview stay live.
    pub(crate) fn poll_portrait(&mut self) {
        let Some(session) = self.shell.portrait.as_mut() else {
            return;
        };
        let busy = session.rx.is_some()
            || session.rendering.is_some()
            || session.body_rx.is_some()
            || session.detail_rx.is_some();
        // Bodies analysed, or AI detail made: the preview redraws with them.
        let done = |rx: &Option<Receiver<()>>| {
            rx.as_ref()
                .is_some_and(|rx| !matches!(rx.try_recv(), Err(TryRecvError::Empty)))
        };
        let bodies_done = done(&session.body_rx);
        if bodies_done {
            session.body_rx = None;
        }
        let details_done = done(&session.detail_rx);
        if details_done {
            session.detail_rx = None;
        }
        let finished = session.rx.take().and_then(|rx| match rx.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => {
                session.rx = Some(rx);
                None
            }
            Err(TryRecvError::Disconnected) => Some(Err("phân tích dừng bất thường".to_string())),
        });
        match finished {
            Some(Ok(model)) => {
                let model = Arc::new(model);
                session.model = Some(Arc::clone(&model));
                let restored = session.reopened.as_ref().map(|r| r.recipe.restore(&model));
                if let Some(restored) = restored {
                    session.restore_faces = Some(restored.iter().map(|f| f.enabled).collect());
                    self.restore_portrait_masks(restored);
                }
                self.refresh_portrait_preview();
            }
            Some(Err(error)) => session.error = Some(error),
            None => {}
        }
        if bodies_done || details_done {
            self.refresh_portrait_preview();
        }
        self.poll_portrait_brush();
        self.collect_portrait_render();
        if busy || self.shell.portrait.as_ref().is_some_and(|s| s.brush.busy()) {
            if let Some(window) = &self.win.window {
                window.request_redraw();
            }
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
            session.wanted = Some((settings, enabled, preview, masks, 0));
        }
        self.refresh_portrait_preview();
    }

    /// Start rendering what the dialog wants unless it is on screen or a
    /// render is already running (that one's completion starts the next).
    /// While the brush paints, its overlay shows the mask instead of the
    /// tinted areas.
    pub(super) fn refresh_portrait_preview(&mut self) {
        let idx = self.docs.active_doc_idx;
        let Some(session) = self.shell.portrait.as_mut() else {
            return;
        };
        let painting = session.brush.target.is_some();
        let bodies = session
            .model
            .as_ref()
            .is_some_and(|m| m.bodies.get().is_some());
        let details = session.model.as_ref().map_or(0, |m| {
            m.faces
                .iter()
                .filter(|f| f.ai_detail.get().is_some())
                .count()
        });
        if let Some(key) = session.wanted.as_mut() {
            key.3 &= !painting;
            // Analysed bodies and AI detail change what the same sliders show.
            key.4 = (session.edit_rev << 32) | ((details as u64) << 1) | bodies as u64;
            // The brush paints the face as shot: show it unreshaped meanwhile.
            if painting {
                key.0 = key.0.without_shape();
            }
        }
        if session.doc_id != self.docs.documents[idx].id
            || session.wanted == session.shown
            || session.rendering.is_some()
        {
            return;
        }
        let Some(key) = session.wanted.clone() else {
            return;
        };
        let Some(model) = session.model.clone() else {
            return;
        };
        let (settings, enabled, preview, masks, _) = key.clone();
        if !settings.body_shape().is_neutral()
            && model.bodies.get().is_none()
            && session.body_rx.is_none()
        {
            session.body_rx = Some(start_body_analysis(&session.src, &model));
        }
        if settings.ai_detail > 0.0 && session.detail_rx.is_none() && lacks_detail(&model, &enabled)
        {
            session.detail_rx = Some(start_detail_analysis(&session.src, &model, &enabled));
        }
        if !preview && !masks {
            self.show_portrait_preview(key, None);
            return;
        }
        let src = Arc::clone(&session.src);
        let edits = session.edits.clone();
        let (w, h) = (session.w, session.h);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let rendered = if masks {
                portrait::render_masks(&src, &model, &settings, &enabled, &edits)
            } else {
                let retouched = portrait::render(&src, &model, &settings, &enabled, &edits);
                match settings.studio_look() {
                    Some((look, strength)) => {
                        looks::preview_with_look(&src, w, h, retouched, look, strength)
                    }
                    None => retouched,
                }
            };
            let _ = tx.send(rendered);
        });
        session.rendering = Some((key, rx));
    }

    /// Put a finished preview render on the canvas, then start the next one
    /// if the sliders moved meanwhile.
    fn collect_portrait_render(&mut self) {
        let Some(session) = self.shell.portrait.as_mut() else {
            return;
        };
        let Some((key, rx)) = session.rendering.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(rendered) => {
                self.show_portrait_preview(key, rendered);
                self.refresh_portrait_preview();
            }
            Err(TryRecvError::Empty) => session.rendering = Some((key, rx)),
            Err(TryRecvError::Disconnected) => {}
        }
    }

    fn show_portrait_preview(&mut self, key: PreviewKey, rendered: Rendered) {
        let Some(session) = self.shell.portrait.as_mut() else {
            return;
        };
        let Some(idx) = self
            .docs
            .documents
            .iter()
            .position(|d| d.id == session.doc_id)
        else {
            return;
        };
        let mut tiles = session.original_tiles.clone();
        if let Some((region, pixels)) = rendered {
            tiles.write_region(region.x, region.y, region.w, region.h, &pixels);
        }
        let layer_id = session.layer_id;
        session.shown = Some(key);
        self.docs.documents[idx]
            .canvas
            .preview_layer_tiles(layer_id, tiles);
        if idx == self.docs.active_doc_idx {
            self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
        }
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }

    /// Drop the session and put the layer back as it was.
    pub(crate) fn cancel_portrait(&mut self) {
        self.end_portrait_brush();
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
            if let Some(reopened) = &session.reopened {
                if let Some(layer) = doc
                    .canvas
                    .layer_stack
                    .layers
                    .iter_mut()
                    .find(|l| l.id == reopened.layer_id)
                {
                    layer.visible = reopened.visible;
                }
                doc.canvas.layer_revision += 1;
            }
        }
        self.apply_canvas_event(if session.reopened.is_some() {
            CanvasEvent::LayerStructureChanged
        } else {
            CanvasEvent::LayerPixelsChanged
        });
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }

    /// Add the retouch as a new layer above the source (only the changed
    /// pixels are opaque, so layer opacity scales the whole retouch) with its
    /// recipe, or update the reopened "Chân dung" layer in place. Returns
    /// whether a layer was updated rather than added.
    pub(crate) fn apply_portrait(
        &mut self,
        settings: PortraitSettings,
        enabled: Vec<bool>,
    ) -> Result<bool, String> {
        let (doc_id, layer_id, w, h, src, model, reopened) = {
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
                session.reopened.as_ref().map(|r| r.layer_id),
            )
        };
        let edits = self.finished_portrait_edits();
        // The body sliders need the bodies: wait for their analysis.
        if !settings.body_shape().is_neutral() && model.bodies.get().is_none() {
            let running = self.shell.portrait.as_mut().and_then(|s| s.body_rx.take());
            let rx = running.unwrap_or_else(|| start_body_analysis(&src, &model));
            let _ = rx.recv();
        }
        // "Chi tiết mặt (AI)" needs the model's detail for every face on: wait
        // for a run under way (it may be for other faces), then make the rest.
        if settings.ai_detail > 0.0 {
            if let Some(rx) = self
                .shell
                .portrait
                .as_mut()
                .and_then(|s| s.detail_rx.take())
            {
                let _ = rx.recv();
            }
            if lacks_detail(&model, &enabled) {
                let _ = start_detail_analysis(&src, &model, &enabled).recv();
            }
        }
        self.cancel_portrait();
        let Some((region, pixels)) = portrait::render(&src, &model, &settings, &enabled, &edits)
        else {
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
        // A studio look recolours the whole photo, so the layer then holds
        // all of it: retouch and look at its strength, in one layer.
        let graded = settings.studio_look().and_then(|(look, strength)| {
            let lut = LookLut::new(look)?;
            let mut full = looks::with_retouch(&src, w, Some((region, pixels.clone())));
            lut.apply(&mut full, strength);
            Some(full)
        });
        if !changed && reopened.is_none() && graded.is_none() {
            return Err("Các thanh trượt đang ở 0 — ảnh không đổi".to_string());
        }
        let recipe = Arc::new(PortraitRecipe::new(
            layer_id,
            (w, h),
            settings,
            &model,
            &enabled,
            &edits,
        ));
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
        // The retouch shows only where its photo does (a cut-out keeps its
        // old background hidden).
        let source_mask = canvas.layer_stack.layers[source_idx].mask.clone();
        let (cw, ch) = (canvas.width, canvas.height);
        let tiles = match graded {
            Some(full) => TileMap::from_rgba(&full, w, h),
            None => {
                let mut tiles = TileMap::new(w, h);
                tiles.write_region(region.x, region.y, region.w, region.h, &patch);
                tiles
            }
        };
        let mut cmd = crate::core::command::LayerStructureCommand::capture_before(
            RESULT_LAYER,
            &canvas.layer_stack,
            cw,
            ch,
        );
        let result_idx =
            reopened.and_then(|id| canvas.layer_stack.layers.iter().position(|l| l.id == id));
        if let Some(result_idx) = result_idx {
            let layer = &mut canvas.layer_stack.layers[result_idx];
            layer.tiles = tiles;
            (layer.width, layer.height, layer.offset) = (w, h, offset);
            layer.visible = true;
            layer.portrait = Some(recipe);
            set_result_mask(layer, source_mask);
        } else {
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
                layer.portrait = Some(recipe);
                set_result_mask(layer, source_mask);
            }
            canvas.layer_stack.active_idx = new_idx;
        }
        cmd.capture_after(&canvas.layer_stack, cw, ch);
        canvas.record(Box::new(cmd));
        canvas.layer_revision += 1;
        if idx == self.docs.active_doc_idx {
            self.upload_full();
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        }
        Ok(result_idx.is_some())
    }

    /// The brush edits with every stroke's skin rebuilt (waits for workers
    /// still running).
    fn finished_portrait_edits(&mut self) -> Vec<FaceEdits> {
        self.poll_portrait_brush();
        self.end_portrait_stroke();
        while self.shell.portrait.as_ref().is_some_and(|s| s.brush.busy()) {
            std::thread::sleep(std::time::Duration::from_millis(5));
            self.poll_portrait_brush();
        }
        self.shell
            .portrait
            .as_ref()
            .map(|s| s.edits.clone())
            .unwrap_or_default()
    }

    /// Dialog view of the session: progress/status line, whether sliders can
    /// preview yet, per face whether its part masks are trusted, and whether
    /// any hair was found.
    pub(crate) fn portrait_dialog_state(&self) -> (String, bool, Vec<bool>, bool) {
        let Some(session) = self.shell.portrait.as_ref() else {
            return (
                "Không chỉnh được layer này".to_string(),
                false,
                Vec::new(),
                false,
            );
        };
        if let Some(error) = &session.error {
            return (
                format!("Không chỉnh được: {error}"),
                false,
                Vec::new(),
                false,
            );
        }
        let Some(model) = &session.model else {
            let line = session
                .progress
                .lock()
                .map(|p| p.clone())
                .unwrap_or_default();
            return (line, false, Vec::new(), false);
        };
        let faces: Vec<bool> = model
            .faces
            .iter()
            .map(|face| !model.parts_used || face.trusted())
            .collect();
        let seconds = model.timings.iter().sum::<u128>() as f32 / 1000.0;
        let mut line = format!(
            "{}{}Tìm thấy {} khuôn mặt · phân tích {seconds:.1} s",
            if session.reopened.is_some() {
                "Chỉnh tiếp layer \"Chân dung\" · "
            } else {
                ""
            },
            if model.clip.is_some() {
                "Trong vùng chọn · "
            } else {
                ""
            },
            faces.len()
        );
        if !model.parts_used {
            line.push_str(" · chỉ dùng mốc mặt");
        }
        let hair = model.faces.iter().any(|face| !face.hair_region.is_empty());
        (line, true, faces, hair)
    }

    /// A note for the dialog's body group, and whether it is a warning: the
    /// body analysis still to come or running, or failed or finding no one.
    pub(crate) fn portrait_body_note(&self) -> Option<(String, bool)> {
        let session = self.shell.portrait.as_ref()?;
        let model = session.model.as_ref()?;
        if session.body_rx.is_some() {
            return Some(("Đang phân tích dáng người…".to_string(), false));
        }
        match model.bodies.get() {
            None if crate::core::ai::pose::model_path().is_none() => Some((
                "Cần model khung xương (models\\pose) — chưa cài".to_string(),
                true,
            )),
            None => Some((
                "Lần đầu kéo thanh, app phân tích dáng người vài giây.".to_string(),
                false,
            )),
            Some(Err(error)) => Some((format!("Không phân tích được dáng người: {error}"), true)),
            Some(Ok(bodies)) if bodies.iter().all(Option::is_none) => Some((
                "Không nhận ra dáng người (cần thấy rõ hai vai)".to_string(),
                true,
            )),
            Some(Ok(_)) => None,
        }
    }

    /// A note for the dialog's skin group, and whether it is a warning: the
    /// AI detail being made, or why it could not be.
    pub(crate) fn portrait_detail_note(&self) -> Option<(String, bool)> {
        let session = self.shell.portrait.as_ref()?;
        let model = session.model.as_ref()?;
        if session.detail_rx.is_some() {
            return Some(("Đang tạo chi tiết bằng AI…".to_string(), false));
        }
        if !crate::core::ai::retouch::FaceRestorer::installed() {
            return Some((
                "Chi tiết mặt (AI) cần model GFPGAN (models\\gfpgan) — chưa cài".to_string(),
                true,
            ));
        }
        model
            .faces
            .iter()
            .find_map(|face| face.ai_detail.get()?.as_ref().err())
            .map(|error| (format!("Không tạo được chi tiết (AI): {error}"), true))
    }

    /// Dialog view of a reopened layer: whether one is reopened, and the
    /// saved sliders and faces the dialog has not taken yet.
    pub(crate) fn portrait_restore(&self) -> (bool, Option<PortraitSettings>, Option<Vec<bool>>) {
        self.shell
            .portrait
            .as_ref()
            .map_or((false, None, None), |s| {
                (
                    s.reopened.is_some(),
                    s.restore_settings,
                    s.restore_faces.clone(),
                )
            })
    }

    /// The dialog took the saved sliders and/or faces.
    pub(crate) fn portrait_restored(&mut self, settings: bool, faces: bool) {
        if let Some(session) = self.shell.portrait.as_mut() {
            if settings {
                session.restore_settings = None;
            }
            if faces {
                session.restore_faces = None;
            }
        }
    }
}

/// Analyse the bodies below `model`'s faces on a worker; the receiver hears
/// once the model holds them.
fn start_body_analysis(src: &Arc<Vec<u8>>, model: &Arc<PortraitModel>) -> Receiver<()> {
    let (tx, rx) = mpsc::channel();
    let (src, model) = (Arc::clone(src), Arc::clone(model));
    let prefer_gpu = crate::core::ai::ort_ep::prefer_gpu();
    std::thread::spawn(move || {
        let bodies = portrait::body::analyze_bodies(&src, &model, prefer_gpu);
        let _ = model.bodies.set(bodies);
        let _ = tx.send(());
    });
    rx
}

/// Whether a face that is on has no AI detail yet.
fn lacks_detail(model: &PortraitModel, enabled: &[bool]) -> bool {
    model
        .faces
        .iter()
        .zip(enabled.iter().chain(std::iter::repeat(&true)))
        .any(|(face, &on)| on && face.ai_detail.get().is_none())
}

/// Make the AI detail of `model`'s faces that are on, on a worker; the
/// receiver hears once the faces hold it.
fn start_detail_analysis(
    src: &Arc<Vec<u8>>,
    model: &Arc<PortraitModel>,
    enabled: &[bool],
) -> Receiver<()> {
    let (tx, rx) = mpsc::channel();
    let (src, model, enabled) = (Arc::clone(src), Arc::clone(model), enabled.to_vec());
    std::thread::spawn(move || {
        portrait::ai_detail::analyze_details(&src, &model, &enabled);
        let _ = tx.send(());
    });
    rx
}

/// The photo layer to analyse and, when reopening, the "Chân dung" layer
/// made from it with its recipe: that layer is active, or sits right above
/// the active photo.
#[allow(clippy::type_complexity)]
fn reopen_target(
    layers: &[Layer],
    active: usize,
) -> Result<(usize, Option<(usize, Arc<PortraitRecipe>)>), String> {
    let made_from = |result: usize, recipe: &PortraitRecipe| {
        layers
            .iter()
            .position(|l| l.id == recipe.source)
            .filter(|&s| s != result && (layers[s].width, layers[s].height) == recipe.source_size)
    };
    let unlocked = |result: usize| {
        if layers[result].locked {
            Err("Layer \"Chân dung\" đang khóa — mở khóa để chỉnh tiếp".to_string())
        } else {
            Ok(())
        }
    };
    if let Some(recipe) = layers.get(active).and_then(|l| l.portrait.clone()) {
        unlocked(active)?;
        let source = made_from(active, &recipe)
            .ok_or_else(|| "Không còn layer ảnh gốc của layer \"Chân dung\" này".to_string())?;
        return Ok((source, Some((active, recipe))));
    }
    if let Some(recipe) = layers.get(active + 1).and_then(|l| l.portrait.clone()) {
        if made_from(active + 1, &recipe) == Some(active) {
            unlocked(active + 1)?;
            return Ok((active, Some((active + 1, recipe))));
        }
    }
    Ok((active, None))
}

fn set_result_mask(layer: &mut Layer, mask: Option<crate::core::layer::LayerMask>) {
    layer.mask = mask;
    layer.mask_active = false;
    layer.paint_target = crate::core::layer::PaintTarget::Pixels;
}

/// The canvas selection over a layer at `offset` of `w` x `h` pixels, in the
/// layer's own pixels; `None` when it misses the layer.
pub(crate) fn selection_clip(
    selection: &crate::core::selection::Selection,
    offset: (i32, i32),
    w: u32,
    h: u32,
) -> Option<portrait::Clip> {
    let (x0, y0, x1, y1) = selection.bounding_box_cached();
    let to_layer = |v: f32, o: i32, size: u32| (v as i64 - o as i64).clamp(0, size as i64) as u32;
    let (lx0, ly0) = (
        to_layer(x0.floor(), offset.0, w),
        to_layer(y0.floor(), offset.1, h),
    );
    let (lx1, ly1) = (
        to_layer(x1.ceil(), offset.0, w),
        to_layer(y1.ceil(), offset.1, h),
    );
    if lx1 <= lx0 || ly1 <= ly0 {
        return None;
    }
    let region = Region {
        x: lx0,
        y: ly0,
        w: lx1 - lx0,
        h: ly1 - ly0,
    };
    let mask: Vec<u8> = (0..region.len())
        .map(|i| {
            let (x, y) = (
                (lx0 + i as u32 % region.w) as i64 + offset.0 as i64,
                (ly0 + i as u32 / region.w) as i64 + offset.1 as i64,
            );
            if x < 0 || y < 0 {
                return 0;
            }
            (selection.sample(x as u32, y as u32) * 255.0).round() as u8
        })
        .collect();
    mask.iter()
        .any(|&m| m > 0)
        .then_some(portrait::Clip { region, mask })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::Canvas;
    use crate::core::portrait::looks::StudioLook;
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

    /// Let the preview worker catch up with the last slider values.
    fn wait_for_preview(app: &mut App) {
        let started = Instant::now();
        while app
            .shell
            .portrait
            .as_ref()
            .is_some_and(|s| s.wanted != s.shown)
        {
            assert!(started.elapsed() < Duration::from_secs(60), "preview hung");
            std::thread::sleep(Duration::from_millis(10));
            app.poll_portrait();
        }
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
        // A new photo starts from the defaults with every face on.
        assert_eq!(
            app.portrait_restore(),
            (false, Some(PortraitSettings::default()), Some(Vec::new()))
        );
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
        let (status, ready, faces, _) = app.portrait_dialog_state();
        assert!(ready, "{status}");
        assert!(!faces.is_empty());
        let on = vec![true; faces.len()];

        app.set_portrait_preview(PortraitSettings::default(), on.clone(), true, false);
        wait_for_preview(&mut app);
        assert_ne!(photo_pixels(&app), original, "preview shows the retouch");
        app.set_portrait_preview(PortraitSettings::default(), on.clone(), false, false);
        wait_for_preview(&mut app);
        assert_eq!(photo_pixels(&app), original, "preview off shows the photo");
        // A drag: only the last values need to end up on screen.
        let mut strong = PortraitSettings::default();
        for step in 0..5 {
            strong.brighten = step as f32 * 20.0;
            app.set_portrait_preview(strong, on.clone(), true, false);
        }
        wait_for_preview(&mut app);
        let shown = app.shell.portrait.as_ref().and_then(|s| s.shown.clone());
        assert_eq!(shown.map(|k| k.0), Some(strong));
        app.set_portrait_preview(PortraitSettings::default(), on.clone(), true, false);
        wait_for_preview(&mut app);

        app.apply_portrait(PortraitSettings::default(), on).unwrap();
        let canvas = &app.docs.documents[0].canvas;
        assert_eq!(canvas.layer_stack.layers.len(), 2);
        assert_eq!(canvas.layer_stack.layers[1].name, RESULT_LAYER);
        assert_eq!(photo_pixels(&app), original, "the photo layer is untouched");
        assert_eq!(canvas.undo_count(), undo_before + 1);
        assert!(app.shell.portrait.is_none());
    }

    #[test]
    fn brush_paints_hair_and_skin_with_undo_and_apply() {
        use crate::core::portrait::brush::MaskTarget;
        use crate::core::refine::{MaskBrushEvent, StampOp};
        use crate::tools::ToolId;
        let Some(mut app) = app_with_photo() else {
            return;
        };
        let tool_before = app.edit.tools.active_id();
        app.shell.ui.show_portrait_dialog = true;
        app.begin_portrait().unwrap();
        let started = Instant::now();
        while app
            .shell
            .portrait
            .as_ref()
            .is_some_and(|s| s.model.is_none())
        {
            assert!(
                started.elapsed() < Duration::from_secs(180),
                "analysis hung"
            );
            std::thread::sleep(Duration::from_millis(50));
            app.poll_portrait();
        }
        let model = app.shell.portrait.as_ref().unwrap().model.clone().unwrap();
        let face = &model.faces[0];
        if face.hair_region.is_empty() {
            return;
        }
        let (centre, extent, _) = face.mesh.frame();
        let at = (centre[0], centre[1]);
        let stroke = |app: &mut App, op: StampOp| {
            let queue = app.docs.documents[0].canvas.mask_brush.as_mut().unwrap();
            queue.push(MaskBrushEvent::Begin(op));
            queue.push(MaskBrushEvent::Dabs {
                points: vec![at],
                radius: extent * 0.05,
                hardness: 1.0,
            });
            queue.push(MaskBrushEvent::End);
            app.poll_portrait_brush();
        };
        let hair_here = |app: &App| {
            let s = app.shell.portrait.as_ref().unwrap();
            let r = s.model.as_ref().unwrap().faces[0].hair_region;
            let k = (at.1 as u32 - r.y) * r.w + at.0 as u32 - r.x;
            let mask = s.edits.first().and_then(|e| e.hair.clone());
            mask.map_or(
                s.model.as_ref().unwrap().faces[0].hair_mask()[k as usize],
                |m| m[k as usize],
            )
        };
        let skin_here = |app: &App| {
            let s = app.shell.portrait.as_ref().unwrap();
            let r = s.model.as_ref().unwrap().faces[0].skin.region();
            let k = ((at.1 as u32 - r.y) * r.w + at.0 as u32 - r.x) as usize;
            s.edits
                .first()
                .and_then(|e| e.skin.clone())
                .map(|l| l.mask()[k])
        };
        let analysed = hair_here(&app);

        app.set_portrait_brush_target(Some(MaskTarget::Hair));
        assert!(app.portrait_painting());
        assert_eq!(app.edit.tools.active_id(), ToolId::RefineBrush);
        assert!(app.portrait_brush_view().3.is_some(), "overlay shown");
        stroke(&mut app, StampOp::Add);
        assert_eq!(hair_here(&app), 255);
        app.portrait_brush_step(false);
        assert_eq!(hair_here(&app), analysed, "undo");
        app.portrait_brush_step(true);
        assert_eq!(hair_here(&app), 255, "redo");

        app.set_portrait_brush_target(Some(MaskTarget::Skin));
        stroke(&mut app, StampOp::Subtract);
        let started = Instant::now();
        while app.shell.portrait.as_ref().is_some_and(|s| s.brush.busy()) {
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "skin rebuild hung"
            );
            std::thread::sleep(Duration::from_millis(10));
            app.poll_portrait_brush();
        }
        assert_eq!(skin_here(&app), Some(0));

        app.set_portrait_brush_target(None);
        assert_eq!(app.edit.tools.active_id(), tool_before, "tool given back");
        assert!(app.docs.documents[0].canvas.mask_brush.is_none());
        let strong = PortraitSettings {
            hair_hue: 200.0,
            hair_tint: 100.0,
            ..PortraitSettings::default()
        };
        app.apply_portrait(strong, vec![true; model.faces.len()])
            .unwrap();
        let layer = &app.docs.documents[0].canvas.layer_stack.layers[1];
        let (x, y) = (at.0 as u32, at.1 as u32);
        assert!(
            layer.tiles.get_pixel(x, y).3 > 0,
            "the painted hair at the face centre was retouched"
        );
    }

    fn analysed(app: &mut App) -> Result<Arc<PortraitModel>, String> {
        app.begin_portrait()?;
        let started = Instant::now();
        loop {
            let session = app.shell.portrait.as_ref().unwrap();
            if let Some(model) = &session.model {
                return Ok(Arc::clone(model));
            }
            if let Some(error) = &session.error {
                return Err(error.clone());
            }
            assert!(
                started.elapsed() < Duration::from_secs(180),
                "analysis hung"
            );
            std::thread::sleep(Duration::from_millis(50));
            app.poll_portrait();
        }
    }

    #[test]
    fn a_studio_look_lands_in_the_one_portrait_layer() {
        let Some(mut app) = app_with_photo() else {
            return;
        };
        let original = photo_pixels(&app);
        let layers_before = app.docs.documents[0].canvas.layer_stack.layers.len();
        analysed(&mut app).unwrap();
        let settings = PortraitSettings {
            look: StudioLook::Warm.index(),
            look_strength: 50.0,
            ..PortraitSettings::NEUTRAL
        };
        let faces = vec![
            true;
            app.shell
                .portrait
                .as_ref()
                .unwrap()
                .model
                .as_ref()
                .unwrap()
                .faces
                .len()
        ];
        app.apply_portrait(settings, faces).unwrap();
        let layers = &app.docs.documents[0].canvas.layer_stack.layers;
        assert_eq!(
            layers.len(),
            layers_before + 1,
            "retouch and look in one layer"
        );
        let result = layers.last().unwrap();
        assert_eq!(result.name, RESULT_LAYER);
        assert_eq!(result.opacity, 1.0);
        // Far from the face, the layer holds the photo with half the look.
        let mut expected = original[..4].to_vec();
        LookLut::new(StudioLook::Warm)
            .unwrap()
            .apply(&mut expected, 0.5);
        assert_eq!(&result.tiles.flatten()[..4], &expected[..]);
        assert_ne!(&expected[..3], &original[..3]);
    }

    #[test]
    fn a_selection_picks_the_faces_and_bounds_the_retouch() {
        let Some(mut app) = app_with_photo() else {
            return;
        };
        let original = photo_pixels(&app);
        let (w, h) = {
            let c = &app.docs.documents[0].canvas;
            (c.width, c.height)
        };
        let whole = analysed(&mut app).unwrap();
        let (centre, extent, _) = whole.faces[0].mesh.frame();
        app.cancel_portrait();

        // Nothing but a corner far from the face: no face to retouch.
        let corner = (extent * 0.3) as u32;
        let far_x = if centre[0] > w as f32 / 2.0 {
            0
        } else {
            w - corner
        };
        let far_y = if centre[1] > h as f32 / 2.0 {
            0
        } else {
            h - corner
        };
        app.docs.documents[0].canvas.selection.select_rect(
            far_x,
            far_y,
            far_x + corner,
            far_y + corner,
        );
        let error = analysed(&mut app).err().expect("no face in the corner");
        assert!(error.contains("vùng chọn"), "{error}");
        app.cancel_portrait();

        // A box around the head: the face is found, the retouch stays inside.
        let half = (extent * 0.8) as u32;
        let (cx, cy) = (centre[0] as u32, centre[1] as u32);
        let (x0, y0) = (cx.saturating_sub(half), cy.saturating_sub(half));
        let (x1, y1) = ((cx + half).min(w), (cy + half).min(h));
        app.docs.documents[0]
            .canvas
            .selection
            .select_rect(x0, y0, x1, y1);
        let model = analysed(&mut app).unwrap();
        assert_eq!(model.faces.len(), 1);
        assert!(model.clip.is_some());
        let (status, ..) = app.portrait_dialog_state();
        assert!(status.starts_with("Trong vùng chọn"), "{status}");
        let strong = PortraitSettings {
            brighten: 100.0,
            ..PortraitSettings::default()
        };
        app.apply_portrait(strong, vec![true]).unwrap();
        let layer = &app.docs.documents[0].canvas.layer_stack.layers[1];
        let mut inside = 0;
        for y in (0..h).step_by(4) {
            for x in (0..w).step_by(4) {
                let alpha = layer.tiles.get_pixel(x, y).3;
                if x >= x0 && x < x1 && y >= y0 && y < y1 {
                    inside += (alpha > 0) as u32;
                } else {
                    assert_eq!(alpha, 0, "changed outside the selection at {x},{y}");
                }
            }
        }
        assert!(inside > 0, "the face was retouched");
        assert_eq!(photo_pixels(&app), original);
    }

    #[test]
    fn reopening_the_portrait_layer_restores_and_updates_it() {
        use crate::core::portrait::brush::MaskTarget;
        use crate::core::refine::{MaskBrushEvent, StampOp};
        let Some(mut app) = app_with_photo() else {
            return;
        };
        app.shell.ui.show_portrait_dialog = true;
        let model = analysed(&mut app).unwrap();
        let face = &model.faces[0];
        if face.hair_region.is_empty() {
            return;
        }
        let (centre, extent, _) = face.mesh.frame();
        let at = (centre[0], centre[1]);
        let r = face.hair_region;
        let k = ((at.1 as u32 - r.y) * r.w + at.0 as u32 - r.x) as usize;
        app.set_portrait_brush_target(Some(MaskTarget::Hair));
        let queue = app.docs.documents[0].canvas.mask_brush.as_mut().unwrap();
        queue.push(MaskBrushEvent::Begin(StampOp::Add));
        queue.push(MaskBrushEvent::Dabs {
            points: vec![at],
            radius: extent * 0.05,
            hardness: 1.0,
        });
        queue.push(MaskBrushEvent::End);
        app.poll_portrait_brush();
        app.set_portrait_brush_target(None);
        let first = PortraitSettings {
            hair_hue: 200.0,
            hair_tint: 100.0,
            ..PortraitSettings::NEUTRAL
        };
        let faces = vec![true; model.faces.len()];
        assert!(!app.apply_portrait(first, faces.clone()).unwrap(), "added");
        let layers = |app: &App| app.docs.documents[0].canvas.layer_stack.layers.clone();
        let undo_after_first = app.docs.documents[0].canvas.undo_count();
        let recipe = layers(&app)[1].portrait.clone().expect("recipe kept");
        assert_eq!(recipe.settings, first);
        assert_eq!(recipe.faces[0].hair.as_ref().unwrap().mask[k], 255);
        assert!(recipe.faces[0].skin.is_none());
        let first_pixels = layers(&app)[1].tiles.flatten();

        // Reopened from the "Chân dung" layer (active after OK): sliders,
        // faces and the painted hair come back; the layer hides meanwhile.
        analysed(&mut app).unwrap();
        assert!(!layers(&app)[1].visible, "hidden while previewing");
        let (reopened, settings, restored) = app.portrait_restore();
        assert!(reopened);
        assert_eq!(settings, Some(first));
        assert_eq!(restored, Some(faces.clone()));
        let session = app.shell.portrait.as_ref().unwrap();
        assert_eq!(session.edits[0].hair.as_ref().unwrap()[k], 255);
        app.portrait_restored(true, true);
        assert_eq!(app.portrait_restore(), (true, None, None));
        let (status, ..) = app.portrait_dialog_state();
        assert!(status.starts_with("Chỉnh tiếp"), "{status}");
        app.cancel_portrait();
        assert!(layers(&app)[1].visible, "shown again on cancel");
        assert_eq!(layers(&app)[1].tiles.flatten(), first_pixels);
        assert_eq!(app.docs.documents[0].canvas.undo_count(), undo_after_first);

        // A double-click on its row reopens it whichever layer is active; any
        // other layer is refused.
        app.docs.documents[0].canvas.layer_stack.active_idx = 0;
        app.reopen_portrait_layer(1).unwrap();
        assert_eq!(app.docs.documents[0].canvas.layer_stack.active_idx, 1);
        assert!(app.shell.portrait.as_ref().unwrap().reopened.is_some());
        app.cancel_portrait();
        assert!(app.reopen_portrait_layer(0).is_err());

        // Reopened from the photo beneath: OK updates the layer in place.
        app.docs.documents[0].canvas.layer_stack.active_idx = 0;
        analysed(&mut app).unwrap();
        let second = PortraitSettings {
            hair_tint: 40.0,
            ..first
        };
        assert!(app.apply_portrait(second, faces).unwrap(), "updated");
        let after = layers(&app);
        assert_eq!(after.len(), 2);
        assert!(after[1].visible);
        let recipe = after[1].portrait.clone().unwrap();
        assert_eq!(recipe.settings, second);
        assert_eq!(recipe.faces[0].hair.as_ref().unwrap().mask[k], 255);
        assert_ne!(after[1].tiles.flatten(), first_pixels);
        assert_eq!(
            app.docs.documents[0].canvas.undo_count(),
            undo_after_first + 1
        );
        app.docs.documents[0].canvas.undo();
        let undone = layers(&app);
        assert_eq!(undone[1].tiles.flatten(), first_pixels);
        assert_eq!(undone[1].portrait.as_ref().unwrap().settings, first);
    }

    #[test]
    fn brush_paints_brows_moves_the_skin_and_reopens() {
        use crate::core::portrait::brush::MaskTarget;
        use crate::core::refine::{MaskBrushEvent, StampOp};
        let Some(mut app) = app_with_photo() else {
            return;
        };
        app.shell.ui.show_portrait_dialog = true;
        let model = analysed(&mut app).unwrap();
        let face = &model.faces[0];
        let (brows, skin) = (face.brow_layers(), &face.skin);
        let (br, sr) = (brows.region(), skin.region());
        // Plain forehead skin beside a brow, well inside the brow region.
        let spot = (0..br.len())
            .filter(|&k| {
                let (x, y) = (k as u32 % br.w, k as u32 / br.w);
                x >= 8 && y >= 8 && x + 8 < br.w && y + 8 < br.h
            })
            .find(|&k| brows.area()[k] == 0 && skin.mask()[sr.index_of(br, k)] > 230)
            .expect("skin beside the brows");
        let at = (
            (br.x + spot as u32 % br.w) as f32 + 0.5,
            (br.y + spot as u32 / br.w) as f32 + 0.5,
        );
        let s = sr.index_of(br, spot);
        let analysed_skin = skin.mask()[s];
        let stroke = |app: &mut App, op: StampOp| {
            let queue = app.docs.documents[0].canvas.mask_brush.as_mut().unwrap();
            queue.push(MaskBrushEvent::Begin(op));
            queue.push(MaskBrushEvent::Dabs {
                points: vec![at],
                radius: 3.0,
                hardness: 1.0,
            });
            queue.push(MaskBrushEvent::End);
            app.poll_portrait_brush();
        };
        let settle = |app: &mut App| {
            let started = Instant::now();
            while app.shell.portrait.as_ref().is_some_and(|s| s.brush.busy()) {
                assert!(
                    started.elapsed() < Duration::from_secs(60),
                    "skin rebuild hung"
                );
                std::thread::sleep(Duration::from_millis(10));
                app.poll_portrait_brush();
            }
        };
        let now = |app: &App| {
            let edit = &app.shell.portrait.as_ref().unwrap().edits[0];
            (
                edit.brows.as_ref().map(|b| b.area()[spot]),
                edit.skin.as_ref().map(|l| l.mask()[s]),
            )
        };

        app.set_portrait_brush_target(Some(MaskTarget::Brows));
        stroke(&mut app, StampOp::Smart);
        settle(&mut app);
        assert_eq!(now(&app), (Some(255), Some(0)), "brow in, skin out");
        app.portrait_brush_step(false);
        settle(&mut app);
        assert_eq!(now(&app), (Some(0), Some(analysed_skin)), "undo");
        app.portrait_brush_step(true);
        settle(&mut app);
        assert_eq!(now(&app), (Some(255), Some(0)), "redo");

        app.set_portrait_brush_target(None);
        let faces = vec![true; model.faces.len()];
        let darker = PortraitSettings {
            brows: 100.0,
            ..PortraitSettings::NEUTRAL
        };
        assert!(!app.apply_portrait(darker, faces).unwrap(), "added");
        let layers = &app.docs.documents[0].canvas.layer_stack.layers;
        let recipe = layers[1].portrait.clone().expect("recipe kept");
        assert_eq!(recipe.faces[0].brows.as_ref().unwrap().mask[spot], 255);
        assert!(recipe.faces[0].skin.is_none(), "no skin was painted");
        assert!(
            layers[1].tiles.get_pixel(at.0 as u32, at.1 as u32).3 > 0,
            "the painted brow was darkened"
        );

        // Reopened: the painted brow comes back and takes the skin again.
        analysed(&mut app).unwrap();
        settle(&mut app);
        assert_eq!(now(&app), (Some(255), Some(0)), "reopened");
        app.cancel_portrait();
    }

    #[test]
    fn body_shape_analyses_bodies_on_first_use_and_narrows_the_waist() {
        let path = std::path::Path::new("tmp/anh-thu-dang/doorway_man.jpg");
        if !path.is_file()
            || crate::core::ai::face_mesh::model_path().is_none()
            || crate::core::ai::pose::model_path().is_none()
            || crate::core::ai::body_parts::model_path().is_none()
        {
            return;
        }
        let image =
            image::open(path)
                .unwrap()
                .resize(900, 1350, image::imageops::FilterType::Triangle);
        let image = image.to_rgba8();
        let (w, h) = image.dimensions();
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        app.docs.documents[0].canvas = Canvas::from_rgba(image.into_raw(), w, h);
        app.shell.ui.show_portrait_dialog = true;
        let model = analysed(&mut app).unwrap();
        let faces = vec![true; model.faces.len()];
        assert!(model.bodies.get().is_none(), "not before a body slider");

        // The first body slider starts the analysis; the preview follows.
        let waist = PortraitSettings {
            body_waist: 100.0,
            ..PortraitSettings::NEUTRAL
        };
        app.set_portrait_preview(waist, faces.clone(), true, false);
        assert_eq!(
            app.portrait_body_note(),
            Some(("Đang phân tích dáng người…".to_string(), false))
        );
        let started = Instant::now();
        while model.bodies.get().is_none() || app.portrait_body_note().is_some() {
            assert!(
                started.elapsed() < Duration::from_secs(180),
                "body analysis hung"
            );
            std::thread::sleep(Duration::from_millis(50));
            app.poll_portrait();
        }
        wait_for_preview(&mut app);
        let bodies = model.bodies.get().unwrap().as_ref().unwrap();
        let body = bodies[0].as_ref().expect("the man's body");
        let edge = body.shape.waist.expect("his waist").start;

        assert!(!app.apply_portrait(waist, faces).unwrap(), "added");
        let layer = &app.docs.documents[0].canvas.layer_stack.layers[1];
        assert!(
            layer.tiles.get_pixel(edge[0] as u32, edge[1] as u32).3 > 0,
            "the waist's edge moved"
        );
        assert_eq!(layer.tiles.get_pixel(w / 2, 2).3, 0, "above the head stays");
        let recipe = layer.portrait.clone().expect("recipe kept");
        assert_eq!(recipe.settings.body_shape(), waist.body_shape());
    }

    #[test]
    fn ai_detail_is_made_on_first_use_and_lands_in_the_layer() {
        let Some(mut app) = app_with_photo() else {
            return;
        };
        if !crate::core::ai::retouch::FaceRestorer::installed() {
            return;
        }
        app.shell.ui.show_portrait_dialog = true;
        let model = analysed(&mut app).unwrap();
        let faces = vec![true; model.faces.len()];
        let made = |model: &PortraitModel| model.faces.iter().all(|f| f.ai_detail.get().is_some());
        assert!(!made(&model), "not before the slider moves");
        assert_eq!(app.portrait_detail_note(), None);

        // Without the detail the slider shows nothing yet.
        app.set_portrait_preview(PortraitSettings::NEUTRAL, faces.clone(), true, false);
        wait_for_preview(&mut app);
        let plain = photo_pixels(&app);

        // The slider leaving 0 starts the model; the preview follows.
        let detail = PortraitSettings {
            ai_detail: 100.0,
            ..PortraitSettings::NEUTRAL
        };
        app.set_portrait_preview(detail, faces.clone(), true, false);
        assert_eq!(
            app.portrait_detail_note(),
            Some(("Đang tạo chi tiết bằng AI…".to_string(), false))
        );
        let started = Instant::now();
        while !made(&model) || app.portrait_detail_note().is_some() {
            assert!(
                started.elapsed() < Duration::from_secs(240),
                "AI detail hung"
            );
            std::thread::sleep(Duration::from_millis(50));
            app.poll_portrait();
        }
        wait_for_preview(&mut app);
        assert!(model.faces[0].ai_detail.get().unwrap().is_ok());
        assert_ne!(photo_pixels(&app), plain, "the preview gained the detail");

        assert!(!app.apply_portrait(detail, faces).unwrap(), "added");
        let layer = &app.docs.documents[0].canvas.layer_stack.layers[1];
        let nose = model.faces[0].mesh.points[4];
        let near = (-6i32..=6).any(|d| {
            layer
                .tiles
                .get_pixel((nose[0] as i32 + d) as u32, nose[1] as u32)
                .3
                > 0
        });
        assert!(near, "the skin by the nose took the detail");
        let recipe = layer.portrait.clone().expect("recipe kept");
        assert_eq!(recipe.settings.ai_detail, 100.0);
    }

    #[test]
    fn face_shape_warps_the_face_only_and_is_kept_in_the_recipe() {
        let Some(mut app) = app_with_photo() else {
            return;
        };
        app.shell.ui.show_portrait_dialog = true;
        let model = analysed(&mut app).unwrap();
        let points = &model.faces[0].mesh.points;
        let (jaw, mouth) = (points[172], points[61]);
        let shape = PortraitSettings {
            face_slim: 100.0,
            smile: 60.0,
            lip_fullness: -40.0,
            eye_tilt: 50.0,
            face_squeeze: 30.0,
            ..PortraitSettings::NEUTRAL
        };
        let faces = vec![true; model.faces.len()];
        assert!(!app.apply_portrait(shape, faces).unwrap(), "added");
        let layer = &app.docs.documents[0].canvas.layer_stack.layers[1];
        for (p, what) in [(jaw, "the jaw"), (mouth, "the mouth corner")] {
            assert!(
                layer.tiles.get_pixel(p[0] as u32, p[1] as u32).3 > 0,
                "{what} moved"
            );
        }
        assert_eq!(layer.tiles.get_pixel(0, 0).3, 0, "the corner stays");
        let recipe = layer.portrait.clone().expect("recipe kept");
        assert_eq!(recipe.settings.face_shape(), shape.face_shape());
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

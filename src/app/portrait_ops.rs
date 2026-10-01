//! "Chỉnh chân dung" (Image ▸ Chỉnh chân dung…): skin, blemish, under-eye, eye
//! and teeth retouching with a live canvas preview. The photo is analysed once
//! on a worker thread (`core::portrait::analyze`); each slider change then only
//! recombines the cached layers of the analysis, also on a worker so dragging
//! never stalls the window (a drag skips to the latest values). The "Tô vùng"
//! brush (`portrait_brush`) edits the skin and hair masks in between. OK adds
//! the result as a new layer above the source, holding just the retouched
//! pixels.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};

use super::portrait_brush::PortraitBrush;
use super::render::CanvasEvent;
use super::state::App;
use crate::core::portrait::{self, FaceEdits, PortraitModel, PortraitSettings, Region};
use crate::core::tile::TileMap;

const RESULT_LAYER: &str = "Chân dung";

/// What the preview shows: settings, faces on, retouch on, areas tinted, and
/// the brush edits' revision.
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
    /// Masks painted with the brush, per face, and their revision.
    pub edits: Vec<FaceEdits>,
    pub edit_rev: u64,
    pub brush: PortraitBrush,
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
        let Some(layer) = canvas.layer_stack.layers.get(canvas.layer_stack.active_idx) else {
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
            None
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
        self.shell.portrait = Some(PortraitSession {
            doc_id: self.docs.documents[idx].id,
            layer_id: layer.id,
            w,
            h,
            offset,
            original_tiles: layer.tiles.clone(),
            src,
            progress,
            rx: Some(rx),
            model: None,
            error: None,
            wanted: None,
            shown: None,
            rendering: None,
            edits: Vec::new(),
            edit_rev: 0,
            brush: PortraitBrush::default(),
        });
        Ok(())
    }

    /// Collect a finished analysis or preview render; keep repainting while
    /// either runs so the progress line and the preview stay live.
    pub(crate) fn poll_portrait(&mut self) {
        let Some(session) = self.shell.portrait.as_mut() else {
            return;
        };
        let busy = session.rx.is_some() || session.rendering.is_some();
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
                session.model = Some(Arc::new(model));
                self.refresh_portrait_preview();
            }
            Some(Err(error)) => session.error = Some(error),
            None => {}
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
        if let Some(key) = session.wanted.as_mut() {
            key.3 &= !painting;
            key.4 = session.edit_rev;
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
        if !preview && !masks {
            self.show_portrait_preview(key, None);
            return;
        }
        let src = Arc::clone(&session.src);
        let edits = session.edits.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let rendered = if masks {
                portrait::render_masks(&src, &model, &enabled, &edits)
            } else {
                portrait::render(&src, &model, &settings, &enabled, &edits)
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
        let edits = self.finished_portrait_edits();
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
            "{}Tìm thấy {} khuôn mặt · phân tích {seconds:.1} s",
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
}

/// The canvas selection over a layer at `offset` of `w` x `h` pixels, in the
/// layer's own pixels; `None` when it misses the layer.
fn selection_clip(
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
            let r = s.model.as_ref().unwrap().faces[0].region;
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

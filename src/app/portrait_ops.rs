//! "Auto retouch" (Image ▸ Auto retouch…), its Chân dung side: skin, blemish, under-eye, eye
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
use crate::core::imposition::{Sheet, SheetOptions};
use crate::core::layer::Layer;
use crate::core::portrait::correct;
use crate::core::portrait::looks::{self, LookLut};
use crate::core::portrait::{
    self, FaceEdits, PortraitModel, PortraitRecipe, PortraitSettings, Region,
};
use crate::core::tile::TileMap;
use crate::tools::ToolId;
use crate::ui::UiActions;

const RESULT_LAYER: &str = "Chân dung";

/// What the preview shows: settings, faces on, retouch on, areas tinted, and
/// a revision of what they act on: the brush edits, how many faces have
/// their clothes found, their clothes drawn, their neck's detail and their AI
/// detail, and whether bodies are analysed.
type PreviewKey = (PortraitSettings, Vec<bool>, bool, bool, u64);
type Rendered = Option<(Region, Vec<u8>)>;
/// A garment's pixels as relit for the preview, with the sliders ("Sáng áo",
/// "Đều sáng áo") they were made at.
type RelitGarment = Option<((f32, f32), Vec<u8>)>;

const GARMENT_LIGHT_STEP: &str = "Sáng áo";

/// What "Áp dụng" did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Applied {
    /// The retouch went into a new "Chân dung" layer.
    Added,
    /// It went into the "Chân dung" layer that was reopened.
    Updated,
    /// Only the garment the photo wears changed: its layer took it.
    Garment,
}

/// The garment a dressed photo wears, a layer of its own, while "Sáng áo"
/// and "Đều sáng áo" relight it.
struct WornGarment {
    layer_id: u32,
    /// The layer's tiles when the session took it: put back when the
    /// session is given up.
    original: TileMap,
    /// The garment before any relight, which the sliders work from, and the
    /// sliders that made `original` of it.
    base_tiles: TileMap,
    base: Arc<Vec<u8>>,
    size: (u32, u32),
    start: (f32, f32),
    laid: Arc<portrait::LaidGarment>,
    /// The sliders the canvas shows the garment at; `None` for `original`.
    shown: Option<(f32, f32)>,
}

impl WornGarment {
    /// The layer's tiles at `look`; `None` when that is what it holds.
    fn relit(&self, look: (f32, f32)) -> Option<TileMap> {
        if look == self.start {
            return None;
        }
        if look == (0.0, 0.0) {
            return Some(self.base_tiles.clone());
        }
        let (w, h) = self.size;
        let pixels = self.laid.relit(&self.base, w, look.0, look.1);
        Some(TileMap::from_rgba(&pixels, w, h))
    }

    /// `settings` as a session on this garment starts with them: the two
    /// sliders of its light where the garment stands, "Nét áo" at rest.
    fn starting(&self, settings: PortraitSettings) -> PortraitSettings {
        PortraitSettings {
            clothes_sharpen: 0.0,
            clothes_brightness: self.start.0,
            clothes_even: self.start.1,
            ..settings
        }
    }
}

pub struct PortraitSession {
    pub doc_id: crate::core::document::DocumentId,
    pub layer_id: u32,
    pub w: u32,
    pub h: u32,
    /// Where the layer sits on the canvas.
    pub offset: (i32, i32),
    pub original_tiles: TileMap,
    /// Whether the layer was shown before the dialog opened: a hidden one
    /// (the photo under an applied "Chân dung" layer) is shown meanwhile.
    source_visible: bool,
    pub src: Arc<Vec<u8>>,
    /// Latest progress line from the analysis worker.
    pub progress: Arc<Mutex<String>>,
    pub rx: Option<Receiver<Result<Arc<PortraitModel>, String>>>,
    /// The analysis came from the last session on this photo.
    reused: bool,
    pub model: Option<Arc<PortraitModel>>,
    pub error: Option<String>,
    /// The sliders and faces the dialog last sent, as sent.
    asked: Option<(PortraitSettings, Vec<bool>)>,
    /// What the dialog last asked to see, and what the canvas shows now.
    pub wanted: Option<PreviewKey>,
    pub shown: Option<PreviewKey>,
    /// The preview render running on a worker, and what it will show.
    rendering: Option<(PreviewKey, Receiver<(Rendered, RelitGarment)>)>,
    /// Whether the photo wears a garment laid on from a sheet: the person's
    /// layer then takes none of the clothes sliders.
    dressed: bool,
    /// That garment, whose light the clothes' two light sliders are, while
    /// the session holds it (not while it is being transformed).
    garment: Option<WornGarment>,
    /// The body analysis running on a worker (started by the first body
    /// slider moved); it fills the model's bodies, then signals.
    body_rx: Option<Receiver<()>>,
    /// The AI face detail being made on a worker (started by "Chi tiết mặt
    /// (AI)" leaving 0); it fills the faces' detail, then signals.
    detail_rx: Option<Receiver<()>>,
    /// The neck's AI detail being made on a worker (started by "Da cổ"
    /// leaving 0 or the brush picking the neck); it fills the faces' necks,
    /// then signals.
    neck_rx: Option<Receiver<()>>,
    /// The clothes being found on a worker (started by a clothes slider
    /// leaving rest, the tinted areas or the brush asking for them); it
    /// fills the faces' clothes areas, then signals.
    area_rx: Option<Receiver<()>>,
    /// The clothes being found and drawn sharp on a worker (started by "Nét
    /// áo" leaving 0); it fills the faces' clothes, then signals.
    clothes_rx: Option<Receiver<()>>,
    /// How many times the photo was enlarged from what was shot (an ID photo
    /// cropped from a small one); 1 when it was not, or it is not known.
    enlarged: f32,
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

/// The analysis of the last session, with what it was made from: the same
/// layer pixels and selection give the same analysis, so a reopened layer
/// starts at once, with its AI detail and bodies too.
pub struct PortraitCache {
    doc_id: crate::core::document::DocumentId,
    layer_id: u32,
    size: (u32, u32),
    src: Arc<Vec<u8>>,
    model: Arc<PortraitModel>,
}

impl PortraitCache {
    /// The kept analysis, if it was made from this very layer content and
    /// selection.
    fn matching(
        &self,
        doc_id: crate::core::document::DocumentId,
        layer_id: u32,
        size: (u32, u32),
        src: &[u8],
        clip: Option<&portrait::Clip>,
    ) -> Option<Arc<PortraitModel>> {
        let same_clip = match (self.model.clip.as_ref(), clip) {
            (None, None) => true,
            (Some(a), Some(b)) => a.region == b.region && a.mask == b.mask,
            _ => false,
        };
        (self.doc_id == doc_id
            && self.layer_id == layer_id
            && self.size == size
            && same_clip
            && self.src[..] == *src)
            .then(|| Arc::clone(&self.model))
    }
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
            return Err("Auto retouch chưa hỗ trợ chế độ CMYK".to_string());
        }
        let (source_idx, reopen) =
            reopen_target(&canvas.layer_stack.layers, canvas.layer_stack.active_idx)?;
        let Some(layer) = canvas.layer_stack.layers.get(source_idx) else {
            return Err("Không có layer để chỉnh".to_string());
        };
        if (!layer.is_background && layer.locked) || !layer.is_raster() {
            return Err("Hãy chọn layer ảnh (không khóa) để chỉnh chân dung".to_string());
        }
        // A "Chân dung" layer that is no longer what its recipe was made on
        // (cropped since) is retouched as the photo it now is. It holds a
        // retouch already: more is added from nothing.
        let retouched = reopen.is_none() && layer.portrait.is_some();
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
        let doc_id = self.docs.documents[idx].id;
        let kept = self
            .shell
            .portrait_cache
            .as_ref()
            .and_then(|c| c.matching(doc_id, layer.id, (w, h), &src, clip.as_ref()));
        let reused = kept.is_some();
        if let Some(model) = kept {
            let _ = tx.send(Ok(model));
        } else {
            let src = Arc::clone(&src);
            let progress = Arc::clone(&progress);
            let prefer_gpu = crate::core::ai::ort_ep::prefer_gpu();
            std::thread::spawn(move || {
                let report = |line: String| {
                    if let Ok(mut slot) = progress.lock() {
                        *slot = line;
                    }
                };
                let analysed = portrait::analyze(&src, w, h, prefer_gpu, clip, &report);
                let _ = tx.send(analysed.map(Arc::new));
            });
        }
        let layer_id = layer.id;
        let original_tiles = layer.tiles.clone();
        let source_visible = layer.visible;
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
        let dressed = self.garment_layer(doc_id).is_some();
        let garment = self.worn_garment(doc_id);
        let restore_settings = match &reopened {
            Some(reopened) => reopened.recipe.settings,
            None if retouched => PortraitSettings::NEUTRAL,
            None => PortraitSettings::default(),
        };
        let restore_settings = Some(match &garment {
            Some(garment) => garment.starting(restore_settings),
            None if dressed => restore_settings.without_clothes(),
            None => restore_settings,
        });
        let restore_faces = reopened.is_none().then(Vec::new);
        // The preview is drawn on the photo layer: it has to show.
        if !source_visible {
            let canvas = &mut self.docs.documents[idx].canvas;
            canvas.layer_stack.layers[source_idx].visible = true;
            canvas.layer_revision += 1;
        }
        if reopened.is_some() || !source_visible {
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        }
        let enlarged = self.id_photo_enlarged(doc_id);
        self.shell.portrait = Some(PortraitSession {
            doc_id,
            layer_id,
            w,
            h,
            offset,
            original_tiles,
            source_visible,
            src,
            progress,
            rx: Some(rx),
            reused,
            model: None,
            error: None,
            asked: None,
            wanted: None,
            shown: None,
            rendering: None,
            dressed,
            garment,
            body_rx: None,
            detail_rx: None,
            neck_rx: None,
            area_rx: None,
            clothes_rx: None,
            enlarged,
            edits: Vec::new(),
            edit_rev: 0,
            brush: PortraitBrush::default(),
            reopened,
            restore_settings,
            restore_faces,
        });
        Ok(())
    }

    /// `begin_portrait` on a new photo, its sliders starting from `settings`
    /// (a preset) rather than the defaults. A reopened layer keeps its own.
    pub(crate) fn begin_portrait_from(
        &mut self,
        settings: Option<PortraitSettings>,
    ) -> Result<(), String> {
        self.begin_portrait()?;
        if let (Some(settings), Some(session)) = (settings, self.shell.portrait.as_mut()) {
            if session.reopened.is_none() {
                session.restore_settings = Some(match &session.garment {
                    Some(garment) => garment.starting(settings),
                    None if session.dressed => settings.without_clothes(),
                    None => settings,
                });
            }
        }
        Ok(())
    }

    /// The garment document `doc_id` wears, as a session relights it: from
    /// what it was before its last relight while its layer still holds that
    /// one, else from what the layer holds now.
    fn worn_garment(&self, doc_id: crate::core::document::DocumentId) -> Option<WornGarment> {
        let layer_id = self.garment_layer(doc_id)?;
        let doc = self.docs.documents.iter().find(|d| d.id == doc_id)?;
        let layer = doc
            .canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == layer_id && l.is_raster())?;
        let size = (layer.width, layer.height);
        let original = layer.tiles.clone();
        let (base_tiles, start) = match self.garment_relit(doc_id, layer_id) {
            Some(relit) => (relit.base.clone(), relit.look),
            None => (original.clone(), (0.0, 0.0)),
        };
        let base = base_tiles.flatten();
        if size.0 == 0 || base.len() != size.0 as usize * size.1 as usize * 4 {
            return None;
        }
        let laid = portrait::LaidGarment::read(&base, size.0, size.1);
        Some(WornGarment {
            layer_id,
            original,
            base_tiles,
            base: Arc::new(base),
            size,
            start,
            laid: Arc::new(laid),
            shown: None,
        })
    }

    /// The garment is about to be worked on outside the retouch (moved,
    /// scaled, turned): what the retouch shows of its light lands in its
    /// layer first, a step of its own, and the session lets the garment go
    /// until it is taken again (`take_garment_again`).
    pub(in crate::app) fn settle_garment_light(&mut self) {
        let Some(session) = self.shell.portrait.as_mut() else {
            return;
        };
        let Some(garment) = session.garment.take() else {
            return;
        };
        let doc_id = session.doc_id;
        let Some(look) = garment.shown else {
            return;
        };
        let Some(idx) = self.docs.documents.iter().position(|d| d.id == doc_id) else {
            return;
        };
        let canvas = &mut self.docs.documents[idx].canvas;
        let Some(shown) = canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == garment.layer_id)
            .map(|l| l.tiles.clone())
        else {
            return;
        };
        if canvas.commit_layer_tiles_change(
            garment.layer_id,
            garment.original,
            shown,
            GARMENT_LIGHT_STEP,
        ) {
            self.keep_garment_light(doc_id, garment.layer_id, garment.base_tiles, look);
            if idx == self.docs.active_doc_idx {
                self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
            }
        }
    }

    /// Note that the garment in `layer` now holds `base` relit at `look`.
    fn keep_garment_light(
        &mut self,
        doc_id: crate::core::document::DocumentId,
        layer: u32,
        base: TileMap,
        look: (f32, f32),
    ) {
        let made = self
            .docs
            .documents
            .iter()
            .find(|d| d.id == doc_id)
            .and_then(|d| d.canvas.layer_stack.layers.iter().find(|l| l.id == layer))
            .map(|l| l.tiles.content_hash());
        let relit = made
            .filter(|_| look != (0.0, 0.0))
            .map(|made| super::garment_ops::Relit { base, look, made });
        self.set_garment_relit(doc_id, layer, relit);
    }

    /// A session that let its garment go takes it again once nothing else
    /// works on it, as the garment now is; the two sliders of its light go
    /// where it stands.
    fn take_garment_again(&mut self) {
        let Some(session) = self.shell.portrait.as_ref() else {
            return;
        };
        if session.garment.is_some() || self.edit.transform_state.is_some() {
            return;
        }
        let Some(garment) = self.worn_garment(session.doc_id) else {
            return;
        };
        let Some(session) = self.shell.portrait.as_mut() else {
            return;
        };
        if let Some((settings, _)) = session.asked {
            let look = (settings.clothes_brightness, settings.clothes_even);
            if look != garment.start || settings.clothes_sharpen != 0.0 {
                session.restore_settings = Some(garment.starting(settings));
            }
        }
        session.garment = Some(garment);
    }

    /// Open the dialog. Nothing is analysed until the owner asks ("Tự động
    /// làm đẹp", or "Làm ảnh thẻ"): opening only shows it. A "Chân dung"
    /// layer picked to be edited further is the exception, reopened at once.
    pub(crate) fn open_portrait_dialog(&mut self) {
        self.cancel_portrait();
        self.close_id_photo();
        self.shell.portrait_error = None;
        self.shell.ui.show_portrait_dialog = true;
        let stack = &self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack;
        let reopening = matches!(
            reopen_target(&stack.layers, stack.active_idx),
            Ok((_, Some(_)))
        );
        if reopening {
            self.shell.ui.portrait_id_side = false;
            self.start_portrait_retouch();
        }
    }

    /// "Tự động làm đẹp": analyse the photo and retouch it from the usual
    /// sliders. Why it could not start is kept for the dialog.
    pub(crate) fn start_portrait_retouch(&mut self) {
        if self.shell.portrait.is_some() || self.id_photo_state().busy {
            return;
        }
        self.aim_at_the_dressed_person();
        self.shell.portrait_error = self.begin_portrait().err();
        if let Some(message) = &self.shell.portrait_error {
            self.shell.status_msg = message.clone();
        }
    }

    /// A dressed photo's retouch is the person's, whatever layer was worked
    /// on last (the garment after "Chỉnh áo", the hair over it): the layer
    /// that shows the person becomes the active one.
    fn aim_at_the_dressed_person(&mut self) {
        if self.edit.transform_state.is_some() {
            return;
        }
        let idx = self.docs.active_doc_idx;
        let doc_id = self.docs.documents[idx].id;
        let Some(shown) = self.dressed_person_shown(doc_id) else {
            return;
        };
        let canvas = &mut self.docs.documents[idx].canvas;
        if canvas.layer_stack.active_idx == shown {
            return;
        }
        for layer in &mut canvas.layer_stack.layers {
            layer.selected = false;
        }
        canvas.layer_stack.active_idx = shown;
        canvas.layer_stack.layers[shown].selected = true;
        canvas.layer_revision += 1;
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
    }

    /// Whether the dialog holds the canvas: a retouch is previewed on it, or
    /// an ID photo is being made. Open and idle it holds nothing, and every
    /// tool and command works as with any other panel.
    pub(crate) fn portrait_under_way(&self) -> bool {
        self.shell.ui.show_portrait_dialog
            && (self.shell.portrait.is_some() || self.id_photo_busy())
    }

    /// Whether a retouch under way is all that holds the canvas. It gives
    /// way to any command from outside the dialog (`yield_portrait`), so
    /// nothing outside is locked meanwhile.
    pub(crate) fn portrait_yields(&self) -> bool {
        self.portrait_under_way() && !self.is_blocking_modal() && !self.locked_beside_retouch()
    }

    /// A command from outside the dialog, with only its retouch holding the
    /// canvas: the retouch previewed is applied as it stands, so the command
    /// works on what shows, and work still running (the face being found,
    /// an ID photo being made) is given up. The dialog stays open, holding
    /// the canvas no more. Returns the places in the layer stack of the
    /// photo and of the retouch applied.
    pub(crate) fn yield_portrait(&mut self) -> Option<(usize, usize)> {
        if !self.portrait_yields() {
            return None;
        }
        let making = self.id_photo_busy();
        let (layers, finding, asked) = match self.shell.portrait.as_ref() {
            Some(session) => (
                Some((
                    session.doc_id,
                    session.layer_id,
                    session.reopened.as_ref().map(|r| r.layer_id),
                )),
                session.model.is_none() && session.error.is_none(),
                session.asked.clone(),
            ),
            None => (None, false, None),
        };
        let applied =
            asked.and_then(|(settings, enabled)| self.apply_portrait(settings, enabled).ok());
        // The ID photo's framing can no longer be done again either, the
        // document moves on.
        self.cancel_portrait();
        self.close_id_photo();
        self.shell.portrait_error = None;
        self.shell.status_msg = match applied {
            Some(Applied::Garment) => "Auto retouch: đã chỉnh sáng áo ghép",
            Some(_) => "Auto retouch: đã áp dụng vào layer \"Chân dung\"",
            None if finding => "Auto retouch: đã dừng nhận diện khuôn mặt",
            None if making => "Auto retouch: đã dừng làm ảnh thẻ",
            None => "Auto retouch: không có gì để áp dụng",
        }
        .to_string();
        // With only the garment changed there is no retouched layer.
        applied.filter(|applied| *applied != Applied::Garment)?;
        let (doc_id, photo, reopened) = layers?;
        let doc = &self.docs.documents[self.docs.active_doc_idx];
        let stack = &doc.canvas.layer_stack;
        let place = |id: u32| stack.layers.iter().position(|l| l.id == id);
        // A new layer is the active one.
        let retouched = reopened.map_or(Some(stack.active_idx), place)?;
        (doc.id == doc_id).then_some((place(photo)?, retouched))
    }

    /// What the frame asks from outside the dialog while only its retouch
    /// holds the canvas. Undo gives the preview up (a "Tô vùng" stroke
    /// first); any other command has the retouch applied, then runs on it
    /// (`yield_portrait`).
    pub(crate) fn portrait_gives_way(&mut self, actions: &mut UiActions) {
        if !self.portrait_yields() {
            return;
        }
        // Asked for again from a menu, the dialog is already here.
        if actions.dialogs.show_portrait_dialog == Some(true) {
            actions.dialogs.show_portrait_dialog = None;
        }
        let doc = &mut actions.doc;
        if doc.undo || doc.redo {
            let forward = !doc.undo;
            (doc.undo, doc.redo) = (false, false);
            if self.portrait_painting() {
                self.portrait_brush_step(forward);
            } else if !forward {
                self.drop_portrait_preview();
            }
        } else if doc.jump_history.is_some() {
            self.drop_portrait_preview();
        } else if actions.reaches_past_retouch() {
            let count = |app: &App| {
                let doc = &app.docs.documents[app.docs.active_doc_idx];
                doc.canvas.layer_stack.layers.len()
            };
            let before = count(self);
            if let Some((photo, retouched)) = self.yield_portrait() {
                actions.retarget_layers(photo, retouched, count(self) > before);
            }
        }
    }

    /// Ctrl+Z from outside the dialog: the preview is given up rather than
    /// applied and then undone, and work still running is stopped. The
    /// dialog stays open; an ID photo already made stays too, one more
    /// Ctrl+Z away.
    pub(crate) fn drop_portrait_preview(&mut self) {
        self.cancel_portrait();
        self.stop_id_photo();
        self.shell.portrait_error = None;
        self.shell.status_msg = "Auto retouch: đã bỏ phần xem trước".to_string();
    }

    /// A tool other than the view ones was picked with the dialog open: a
    /// command from outside like any other. The dialog stays open.
    pub(crate) fn portrait_tool_picked(&mut self, crop: bool) {
        self.yield_portrait();
        if crop && !self.portrait_under_way() {
            self.lock_crop_to_id_photo();
        }
    }

    /// Whether the retouch that gives way is on show: the face found (or
    /// not), no worker of the analysis or of an ID photo still running.
    pub(crate) fn portrait_on_show(&self) -> bool {
        let found = |s: &PortraitSession| s.model.is_some() || s.error.is_some();
        self.portrait_yields()
            && !self.id_photo_busy()
            && self.shell.portrait.as_ref().is_some_and(found)
    }

    /// Whether the tool in hand gets the canvas while the dialog holds it:
    /// the view tools always, any other once the retouch is on show (its
    /// press then applies it, see `portrait_pressed`). A stray press is not
    /// to stop the face being found or an ID photo being made.
    pub(crate) fn portrait_frees_canvas(&self) -> bool {
        matches!(self.edit.tools.active_id(), ToolId::Hand | ToolId::Zoom)
            || self.portrait_on_show()
    }

    /// The tool in hand was pressed on the canvas: any but a view tool and
    /// the dialog's own brush has the retouch applied first.
    pub(crate) fn portrait_pressed(&mut self) {
        let view = matches!(self.edit.tools.active_id(), ToolId::Hand | ToolId::Zoom);
        if !view && !self.portrait_painting() {
            self.yield_portrait();
        }
    }

    /// An ID photo cropped again stays the print it is: the Crop tool is set
    /// to the document's own pixel size and resolution.
    fn lock_crop_to_id_photo(&mut self) {
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let (w, h, dpi) = (canvas.width, canvas.height, canvas.metadata.resolution_ppi);
        if crate::core::imposition::PhotoKind::detect(w, h, dpi).is_none() {
            return;
        }
        self.edit
            .tools
            .crop_mut()
            .apply_preset(&crate::tools::crop::CropPreset {
                name: String::new(),
                width: w as f32,
                height: h as f32,
                unit: crate::core::units::Unit::Pixels,
                dpi,
            });
    }

    /// Close the dialog: the retouch under way is given up, an ID photo
    /// made stays (Ctrl+Z undoes it).
    pub(crate) fn close_portrait_dialog(&mut self) {
        self.shell.ui.show_portrait_dialog = false;
        self.shell.portrait_error = None;
        self.cancel_portrait();
        self.close_id_photo();
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
            || session.detail_rx.is_some()
            || session.neck_rx.is_some()
            || session.area_rx.is_some()
            || session.clothes_rx.is_some();
        // Bodies analysed, AI detail made, necks or clothes found or drawn:
        // the preview redraws with them.
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
        let necks_done = done(&session.neck_rx);
        if necks_done {
            session.neck_rx = None;
        }
        let clothes_done = done(&session.clothes_rx);
        if clothes_done {
            session.clothes_rx = None;
        }
        let areas_done = done(&session.area_rx);
        if areas_done {
            session.area_rx = None;
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
        if areas_done || clothes_done {
            self.clothes_found();
        }
        if necks_done {
            self.neck_found();
        }
        if bodies_done || details_done || necks_done || clothes_done || areas_done {
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
            session.asked = Some((settings, enabled.clone()));
            session.wanted = Some((settings, enabled, preview, masks, 0));
        }
        self.refresh_portrait_preview();
    }

    /// Start rendering what the dialog wants unless it is on screen or a
    /// render is already running (that one's completion starts the next).
    /// While the brush paints, its overlay shows the mask instead of the
    /// tinted areas.
    pub(super) fn refresh_portrait_preview(&mut self) {
        self.take_garment_again();
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
        let necks = session.model.as_ref().map_or(0, |m| {
            m.faces.iter().filter(|f| f.neck.get().is_some()).count()
        });
        let clothes = session.model.as_ref().map_or(0, |m| {
            m.faces.iter().filter(|f| f.clothes.get().is_some()).count()
        });
        let areas = session.model.as_ref().map_or(0, |m| {
            m.faces
                .iter()
                .filter(|f| f.clothes_area.get().is_some())
                .count()
        });
        // The clothes are found for the sliders, the tinted areas and the
        // brush alike, whatever is on screen; "Nét áo" has them drawn too.
        let asked = session
            .wanted
            .as_ref()
            .map(|key| (key.0, key.1.clone(), key.3));
        if let (Some(model), Some((settings, enabled, masks))) = (session.model.clone(), asked) {
            // A garment worn is a layer of its own: the person's layer has
            // no clothes to find.
            let dressed = session.dressed;
            let wanted =
                !dressed && (settings.clothes_active() || masks || session.brush.wants_clothes());
            if settings.clothes_sharpen > 0.0 && !dressed {
                if session.clothes_rx.is_none() && lacks_clothes(&model, &enabled) {
                    session.clothes_rx = Some(start_clothes_analysis(
                        &session.src,
                        &model,
                        &enabled,
                        session.enlarged,
                    ));
                }
            } else if wanted
                && session.area_rx.is_none()
                && session.clothes_rx.is_none()
                && lacks_clothes_area(&model, &enabled)
            {
                session.area_rx = Some(start_clothes_search(&session.src, &model, &enabled));
            }
            // The neck is found, and its detail made, for its slider and for
            // the brush; one run of the face model at a time.
            if (settings.neck > 0.0 || session.brush.wants_neck())
                && session.neck_rx.is_none()
                && session.detail_rx.is_none()
                && lacks_neck(&model, &enabled)
            {
                session.neck_rx = Some(start_neck_analysis(&session.src, &model, &enabled));
            }
        }
        if let Some(key) = session.wanted.as_mut() {
            key.3 &= !painting;
            // Analysed bodies, AI detail and clothes found or drawn change
            // what the same sliders show.
            key.4 = (session.edit_rev << 32)
                | ((areas as u64) << 24)
                | ((clothes as u64) << 16)
                | ((necks as u64) << 8)
                | ((details as u64) << 1)
                | bodies as u64;
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
            self.show_portrait_preview(key, None, None);
            return;
        }
        // The garment worn takes the clothes' light, when that is not what
        // the canvas shows of it already; the person's layer none of it.
        let look = (settings.clothes_brightness, settings.clothes_even);
        let relight = session
            .garment
            .as_ref()
            .filter(|g| !masks && look != g.start && g.shown != Some(look))
            .map(|g| (Arc::clone(&g.base), Arc::clone(&g.laid), g.size.0));
        let settings = if session.dressed {
            settings.without_clothes()
        } else {
            settings
        };
        let src = Arc::clone(&session.src);
        let edits = session.edits.clone();
        let (w, h) = (session.w, session.h);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let garment =
                relight.map(|(base, laid, width)| (look, laid.relit(&base, width, look.0, look.1)));
            let rendered = if masks {
                portrait::render_masks(&src, &model, &settings, &enabled, &edits)
            } else {
                let retouched = portrait::render(&src, &model, &settings, &enabled, &edits);
                let fix = settings
                    .fixes()
                    .and_then(|fixes| correct::fix_lut(&model.light, &fixes));
                let look = settings.studio_look();
                let clip = model.clip.as_ref();
                looks::preview_graded(&src, w, h, retouched, fix.as_ref(), look, clip)
            };
            let _ = tx.send((rendered, garment));
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
            Ok((rendered, garment)) => {
                self.show_portrait_preview(key, rendered, garment);
                self.refresh_portrait_preview();
            }
            Err(TryRecvError::Empty) => session.rendering = Some((key, rx)),
            Err(TryRecvError::Disconnected) => {}
        }
    }

    fn show_portrait_preview(&mut self, key: PreviewKey, rendered: Rendered, relit: RelitGarment) {
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
        // The garment as the sliders shown have it: relit, or as the session
        // found it when the preview is off, the areas are tinted or the
        // sliders are back where it stands.
        let (settings, preview, masks) = (key.0, key.2, key.3);
        let look = (settings.clothes_brightness, settings.clothes_even);
        let garment = session.garment.as_mut().and_then(|g| {
            let target = (preview && !masks && look != g.start).then_some(look);
            if target == g.shown {
                return None;
            }
            let tiles = match (target, relit) {
                (None, _) => g.original.clone(),
                (Some(target), Some((made, pixels))) if made == target => {
                    TileMap::from_rgba(&pixels, g.size.0, g.size.1)
                }
                // Not made for these sliders: the next render brings it.
                (Some(_), _) => return None,
            };
            g.shown = target;
            Some((g.layer_id, tiles))
        });
        session.shown = Some(key);
        let canvas = &mut self.docs.documents[idx].canvas;
        if let Some((garment, tiles)) = garment {
            canvas.preview_layer_tiles(garment, tiles);
        }
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
            if let Some(garment) = session.garment.filter(|g| g.shown.is_some()) {
                doc.canvas
                    .restore_layer_tiles(garment.layer_id, garment.original);
            }
            doc.canvas
                .restore_layer_tiles(session.layer_id, session.original_tiles);
            if !session.source_visible {
                if let Some(layer) = doc
                    .canvas
                    .layer_stack
                    .layers
                    .iter_mut()
                    .find(|l| l.id == session.layer_id)
                {
                    layer.visible = false;
                }
                doc.canvas.layer_revision += 1;
            }
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
        self.apply_canvas_event(if session.reopened.is_some() || !session.source_visible {
            CanvasEvent::LayerStructureChanged
        } else {
            CanvasEvent::LayerPixelsChanged
        });
        if let Some(model) = session.model {
            self.shell.portrait_cache = Some(PortraitCache {
                doc_id: session.doc_id,
                layer_id: session.layer_id,
                size: (session.w, session.h),
                src: session.src,
                model,
            });
        }
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }

    /// Drop the kept analysis of a document that is closing.
    pub(crate) fn forget_portrait_analysis(&mut self, doc_id: crate::core::document::DocumentId) {
        if self
            .shell
            .portrait_cache
            .as_ref()
            .is_some_and(|c| c.doc_id == doc_id)
        {
            self.shell.portrait_cache = None;
        }
    }

    /// Add the retouched photo as a new layer above the source, with its
    /// recipe, and hide the source: the new layer holds the whole photo, so
    /// nothing of the unretouched one shows (or prints) around a reshaped
    /// face. A reopened "Chân dung" layer is updated in place. Returns
    /// whether a layer was updated rather than added.
    /// "Áp dụng" of the dialog: the retouch lands in its layer and the
    /// dialog closes; then, when "Xếp ảnh in" asked for one, the result is
    /// laid out on that print sheet.
    pub(crate) fn finish_portrait_dialog(
        &mut self,
        settings: PortraitSettings,
        enabled: Vec<bool>,
        sheet: Option<(Sheet, SheetOptions)>,
    ) {
        let applied = self.apply_portrait(settings, enabled);
        // A sheet asked for with no retouch to apply (none under way, or
        // every slider at 0) lays out the photo as it is.
        let as_it_is = sheet.is_some() && self.shell.portrait.is_none();
        match applied {
            Ok(applied) => {
                self.close_portrait_dialog();
                self.shell.status_msg = match applied {
                    Applied::Updated => "Auto retouch: đã cập nhật layer \"Chân dung\"",
                    Applied::Added => "Auto retouch: đã thêm layer \"Chân dung\"",
                    Applied::Garment => "Auto retouch: đã chỉnh sáng áo ghép",
                }
                .to_string();
            }
            Err(_) if as_it_is && !self.id_photo_state().busy => self.close_portrait_dialog(),
            Err(message) => {
                self.shell.status_msg = message;
                return;
            }
        }
        if let Some((sheet, options)) = sheet {
            self.do_impose_sheet(sheet, options);
        }
    }

    pub(crate) fn apply_portrait(
        &mut self,
        settings: PortraitSettings,
        enabled: Vec<bool>,
    ) -> Result<Applied, String> {
        let (doc_id, layer_id, w, h, src, model, reopened, enlarged) = {
            let Some(session) = self.shell.portrait.as_ref() else {
                return Err("Chưa bấm Tự động làm đẹp".to_string());
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
                session.enlarged,
            )
        };
        // A garment worn takes the clothes' light, in its own layer; the
        // person's layer, whose own clothes are gone, none of the clothes'.
        let garment = self
            .shell
            .portrait
            .as_ref()
            .and_then(|s| s.garment.as_ref())
            .map(|g| {
                let look = (settings.clothes_brightness, settings.clothes_even);
                let relit = g.relit(look);
                (
                    g.layer_id,
                    g.original.clone(),
                    g.base_tiles.clone(),
                    look,
                    relit,
                )
            });
        let dressed = self.shell.portrait.as_ref().is_some_and(|s| s.dressed);
        let recipe_settings = settings;
        let settings = if dressed {
            settings.without_clothes()
        } else {
            settings
        };
        // The clothes sliders, and a clothes mask kept by a reopened layer,
        // need the clothes found, and "Nét áo" needs them drawn: wait for a
        // run under way (it may be for other faces), then make the rest.
        let kept = !dressed
            && self
                .shell
                .portrait
                .as_ref()
                .is_some_and(|s| s.brush.wants_clothes());
        if settings.clothes_active() || kept {
            let running = self
                .shell
                .portrait
                .as_mut()
                .map(|s| (s.area_rx.take(), s.clothes_rx.take()));
            if let Some((areas, drawn)) = running {
                for rx in [areas, drawn].into_iter().flatten() {
                    let _ = rx.recv();
                }
            }
            if settings.clothes_sharpen > 0.0 {
                if lacks_clothes(&model, &enabled) {
                    let _ = start_clothes_analysis(&src, &model, &enabled, enlarged).recv();
                }
            } else if lacks_clothes_area(&model, &enabled) {
                let _ = start_clothes_search(&src, &model, &enabled).recv();
            }
            self.clothes_found();
        }
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
        // "Da cổ" needs the neck of every face on, the same way.
        if settings.neck > 0.0 {
            if let Some(rx) = self.shell.portrait.as_mut().and_then(|s| s.neck_rx.take()) {
                let _ = rx.recv();
            }
            if lacks_neck(&model, &enabled) {
                let _ = start_neck_analysis(&src, &model, &enabled).recv();
            }
        }
        self.cancel_portrait();
        let Some((region, pixels)) = portrait::render(&src, &model, &settings, &enabled, &edits)
        else {
            return Err("Không có khuôn mặt nào được chọn".to_string());
        };
        let row = region.w as usize * 4;
        let changed = (0..region.h as usize).any(|y| {
            let s = ((region.y as usize + y) * w as usize + region.x as usize) * 4;
            pixels[y * row..(y + 1) * row] != src[s..s + row]
        });
        // The layer holds the whole photo: the retouch, then the corrections
        // and the studio look at its strength.
        let fix = settings
            .fixes()
            .and_then(|fixes| correct::fix_lut(&model.light, &fixes));
        let look = settings
            .studio_look()
            .and_then(|(look, strength)| Some((LookLut::new(look)?, strength)));
        let relit = garment
            .as_ref()
            .and_then(|(layer, before, base, look, relit)| {
                Some((*layer, before.clone(), base.clone(), *look, relit.clone()?))
            });
        if !changed && reopened.is_none() && fix.is_none() && look.is_none() {
            // The garment alone changed: its layer takes it, and no layer is
            // added for a person who is as before.
            let Some((layer, before, base, look, after)) = relit else {
                return Err("Các thanh trượt đang ở 0 — ảnh không đổi".to_string());
            };
            let Some(idx) = self.docs.documents.iter().position(|d| d.id == doc_id) else {
                return Err("Tài liệu đã đóng".to_string());
            };
            let canvas = &mut self.docs.documents[idx].canvas;
            if !canvas.commit_layer_tiles_change(layer, before, after, GARMENT_LIGHT_STEP) {
                return Err("Layer áo không còn".to_string());
            }
            self.keep_garment_light(doc_id, layer, base, look);
            if idx == self.docs.active_doc_idx {
                self.upload_full();
                self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
            }
            return Ok(Applied::Garment);
        }
        let mut full = looks::with_retouch(&src, w, Some((region, pixels)));
        let look = look.as_ref().map(|(lut, strength)| (lut, *strength));
        looks::grade(&mut full, w, fix.as_ref(), look, model.clip.as_ref());
        let tiles = TileMap::from_rgba(&full, w, h);
        let mut recipe =
            PortraitRecipe::new(layer_id, (w, h), recipe_settings, &model, &enabled, &edits);
        recipe.made = Some(tiles.content_hash());
        let recipe = Arc::new(recipe);
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
        // The photo under it would show around a reshaped face, and print.
        if let Some(source) = canvas
            .layer_stack
            .layers
            .iter_mut()
            .find(|l| l.id == layer_id)
        {
            source.visible = false;
        }
        // The garment relit, in the same step.
        let mut relit_kept = None;
        if let Some((layer, _, base, look, after)) = relit {
            if let Some(garment) = canvas.layer_stack.layers.iter_mut().find(|l| l.id == layer) {
                garment.tiles = after;
                relit_kept = Some((layer, base, look));
            }
        }
        cmd.capture_after(&canvas.layer_stack, cw, ch);
        canvas.record(Box::new(cmd));
        canvas.layer_revision += 1;
        if let Some((layer, base, look)) = relit_kept {
            self.keep_garment_light(doc_id, layer, base, look);
        }
        if idx == self.docs.active_doc_idx {
            self.upload_full();
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        }
        Ok(if result_idx.is_some() {
            Applied::Updated
        } else {
            Applied::Added
        })
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
        // No retouch yet: none was asked for, or it could not start.
        let Some(session) = self.shell.portrait.as_ref() else {
            let line = self.shell.portrait_error.clone().unwrap_or_default();
            return (line, false, Vec::new(), false);
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
        let analysed = if session.reused {
            "dùng lại phân tích lần trước".to_string()
        } else {
            format!("phân tích {seconds:.1} s")
        };
        let mut line = format!(
            "{}{}Tìm thấy {} khuôn mặt · {analysed}",
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
        // The AI detail is on for a new photo: say why the preview is about
        // to sharpen.
        if session.detail_rx.is_some() {
            line.push_str(" · đang tạo chi tiết AI…");
        }
        if session.neck_rx.is_some() {
            line.push_str(" · đang làm da cổ…");
        }
        if session.clothes_rx.is_some() {
            line.push_str(" · đang làm nét áo…");
        } else if session.area_rx.is_some() {
            line.push_str(" · đang tìm áo…");
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
        let failed = model
            .faces
            .iter()
            .find_map(|face| face.ai_detail.get()?.as_ref().err())
            .map(|error| (format!("Không tạo được chi tiết (AI): {error}"), true));
        if failed.is_some() {
            return failed;
        }
        // Hair is found by the part model alone: without it the AI detail
        // stops at the face, which is worth saying once the slider is on.
        let on = session
            .wanted
            .as_ref()
            .is_some_and(|key| key.0.ai_detail > 0.0);
        let no_hair = model.faces.iter().all(|f| f.hair_region.is_empty());
        (on && no_hair).then(|| {
            (
                "AI chỉ làm nét khuôn mặt: chưa nhận ra tóc (cần model tách vùng models\\sapiens2-seg)"
                    .to_string(),
                false,
            )
        })
    }

    /// A note under "Da cổ", and whether it is a warning: the neck's detail
    /// being made, or why it could not be.
    pub(crate) fn portrait_neck_note(&self) -> Option<(String, bool)> {
        let session = self.shell.portrait.as_ref()?;
        let model = session.model.as_ref()?;
        if session.neck_rx.is_some() {
            return Some(("Đang tìm da cổ và tạo chi tiết bằng AI…".to_string(), false));
        }
        let asked = session.brush.wants_neck()
            || session.wanted.as_ref().is_some_and(|key| key.0.neck > 0.0);
        if !asked {
            return None;
        }
        if !crate::core::ai::retouch::FaceRestorer::installed() {
            return Some((
                "Da cổ cần model GFPGAN (models\\gfpgan) — chưa cài".to_string(),
                true,
            ));
        }
        model
            .faces
            .iter()
            .find_map(|face| face.neck.get()?.as_ref().err())
            .map(|error| (format!("Không làm được da cổ: {error}"), true))
    }

    /// A note for the dialog's clothes group, and whether it is a warning:
    /// the clothes being found or drawn, or why they could not be.
    pub(crate) fn portrait_clothes_note(&self) -> Option<(String, bool)> {
        let session = self.shell.portrait.as_ref()?;
        let model = session.model.as_ref()?;
        // A garment worn is a layer of its own: nothing is looked for.
        if session.dressed {
            return None;
        }
        if session.clothes_rx.is_some() {
            return Some(("Đang tìm áo và làm nét bằng AI…".to_string(), false));
        }
        if session.area_rx.is_some() {
            return Some(("Đang tìm áo…".to_string(), false));
        }
        let lost = model
            .faces
            .iter()
            .find_map(|face| face.clothes_area.get()?.as_ref().err())
            .map(|error| (format!("Không tìm được áo: {error}"), true));
        if lost.is_some() {
            return lost;
        }
        if !crate::core::ai::retouch::Upscaler::installed() {
            return Some((
                "Nét áo cần model Real-ESRGAN (models\\realesrgan) — chưa cài".to_string(),
                true,
            ));
        }
        let failed = model
            .faces
            .iter()
            .find_map(|face| face.clothes.get()?.as_ref().err())
            .map(|error| (format!("Không làm nét được áo: {error}"), true));
        if failed.is_some() {
            return failed;
        }
        model
            .faces
            .iter()
            .all(|face| face.clothes_area.get().is_none())
            .then(|| {
                (
                    "Lần đầu kéo thanh, app tìm áo rồi làm nét vài giây.".to_string(),
                    false,
                )
            })
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

/// Whether a face that is on has no neck found yet.
fn lacks_neck(model: &PortraitModel, enabled: &[bool]) -> bool {
    model
        .faces
        .iter()
        .zip(enabled.iter().chain(std::iter::repeat(&true)))
        .any(|(face, &on)| on && face.neck.get().is_none())
}

/// Find the neck of `model`'s faces that are on and make its AI detail, on a
/// worker; the receiver hears once the faces hold it.
fn start_neck_analysis(
    src: &Arc<Vec<u8>>,
    model: &Arc<PortraitModel>,
    enabled: &[bool],
) -> Receiver<()> {
    let (tx, rx) = mpsc::channel();
    let (src, model, enabled) = (Arc::clone(src), Arc::clone(model), enabled.to_vec());
    std::thread::spawn(move || {
        let began = std::time::Instant::now();
        portrait::neck::analyze_necks(&src, &model, &enabled);
        crate::diag::note(
            "perf",
            &format!(
                "portrait neck detail made in {} ms ({} x {})",
                began.elapsed().as_millis(),
                model.width,
                model.height
            ),
        );
        let _ = tx.send(());
    });
    rx
}

/// Whether a face that is on has not had its clothes looked for yet.
fn lacks_clothes_area(model: &PortraitModel, enabled: &[bool]) -> bool {
    model
        .faces
        .iter()
        .zip(enabled.iter().chain(std::iter::repeat(&true)))
        .any(|(face, &on)| on && face.clothes_area.get().is_none())
}

/// Find the clothes of `model`'s faces that are on, on a worker; the
/// receiver hears once the faces hold where they lie.
fn start_clothes_search(
    src: &Arc<Vec<u8>>,
    model: &Arc<PortraitModel>,
    enabled: &[bool],
) -> Receiver<()> {
    let (tx, rx) = mpsc::channel();
    let (src, model, enabled) = (Arc::clone(src), Arc::clone(model), enabled.to_vec());
    let prefer_gpu = crate::core::ai::ort_ep::prefer_gpu();
    std::thread::spawn(move || {
        let began = std::time::Instant::now();
        portrait::clothes::analyze_clothes_areas(&src, &model, &enabled, prefer_gpu);
        crate::diag::note(
            "perf",
            &format!(
                "portrait clothes found in {} ms ({} x {})",
                began.elapsed().as_millis(),
                model.width,
                model.height
            ),
        );
        let _ = tx.send(());
    });
    rx
}

/// Whether a face that is on has no clothes drawn yet.
fn lacks_clothes(model: &PortraitModel, enabled: &[bool]) -> bool {
    model
        .faces
        .iter()
        .zip(enabled.iter().chain(std::iter::repeat(&true)))
        .any(|(face, &on)| on && face.clothes.get().is_none())
}

/// Find and draw the clothes of `model`'s faces that are on, on a worker;
/// the receiver hears once the faces hold them.
fn start_clothes_analysis(
    src: &Arc<Vec<u8>>,
    model: &Arc<PortraitModel>,
    enabled: &[bool],
    enlarged: f32,
) -> Receiver<()> {
    let (tx, rx) = mpsc::channel();
    let (src, model, enabled) = (Arc::clone(src), Arc::clone(model), enabled.to_vec());
    let prefer_gpu = crate::core::ai::ort_ep::prefer_gpu();
    std::thread::spawn(move || {
        let began = std::time::Instant::now();
        portrait::clothes::analyze_clothes(&src, &model, &enabled, prefer_gpu, enlarged);
        crate::diag::note(
            "perf",
            &format!(
                "portrait clothes drawn in {} ms ({} x {}, enlarged {enlarged:.2})",
                began.elapsed().as_millis(),
                model.width,
                model.height
            ),
        );
        let _ = tx.send(());
    });
    rx
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
    // Reopened, a layer is made again from the photo: one worked on since
    // its retouch (painted, adjusted) would lose that work.
    let as_made = |result: usize, recipe: &PortraitRecipe| {
        recipe
            .made
            .is_none_or(|made| layers[result].tiles.content_hash() == made)
    };
    // A "Chân dung" layer whose photo is gone or no longer its size (cropped
    // since), or that was worked on since, cannot be reopened: it is
    // retouched as a photo of its own.
    if let Some(recipe) = layers.get(active).and_then(|l| l.portrait.clone()) {
        if let Some(source) = made_from(active, &recipe).filter(|_| as_made(active, &recipe)) {
            unlocked(active)?;
            return Ok((source, Some((active, recipe))));
        }
    }
    if let Some(recipe) = layers.get(active + 1).and_then(|l| l.portrait.clone()) {
        if made_from(active + 1, &recipe) == Some(active) {
            if !as_made(active + 1, &recipe) {
                return Err(
                    "Layer \"Chân dung\" phía trên đã được sửa thêm — chọn layer đó để chỉnh tiếp"
                        .to_string(),
                );
            }
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

    /// Whether the "Chân dung" layer differs from the photo under it at
    /// (x, y): it holds the whole photo, retouched.
    fn changed_at(app: &App, x: u32, y: u32) -> bool {
        let layers = &app.docs.documents[0].canvas.layer_stack.layers;
        layers[1].tiles.get_pixel(x, y) != layers[0].tiles.get_pixel(x, y)
    }

    fn photo_pixels(app: &App) -> Vec<u8> {
        app.docs.documents[0].canvas.layer_stack.layers[0]
            .tiles
            .flatten()
    }

    #[test]
    fn a_sheet_asked_in_the_dialog_applies_the_retouch_then_lays_it_out() {
        let Some(mut app) = app_with_photo() else {
            return;
        };
        app.shell.ui.show_portrait_dialog = true;
        app.begin_portrait().unwrap();
        let started = Instant::now();
        let faces = loop {
            app.poll_portrait();
            let (status, ready, faces, _) = app.portrait_dialog_state();
            if ready {
                break faces.len();
            }
            assert!(
                started.elapsed() < Duration::from_secs(180),
                "analysis hung: {status}"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        let settings = PortraitSettings {
            smooth: 50.0,
            ..PortraitSettings::NEUTRAL
        };
        app.finish_portrait_dialog(
            settings,
            vec![true; faces],
            Some((Sheet::Mixed, SheetOptions::default())),
        );
        // The dialog closed over its "Chân dung" layer, and the sheet is a
        // new document in front.
        assert!(!app.shell.ui.show_portrait_dialog && app.shell.portrait.is_none());
        assert_eq!((app.docs.documents.len(), app.docs.active_doc_idx), (2, 1));
        let photo = &app.docs.documents[0].canvas.layer_stack.layers;
        assert!(photo.iter().any(|l| l.name == "Chân dung"));
        let sheet = &app.docs.documents[1];
        assert_eq!(sheet.title, "Trang 13×18 — 6 tấm 3×4 + 2 tấm 4×6");
        assert_eq!(sheet.canvas.layer_stack.layers.len(), 11);
        assert!(
            app.shell.status_msg.starts_with("Đã xếp trang"),
            "{}",
            app.shell.status_msg
        );
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

        // Skin sliders alone leave the photo's corner as it is.
        let skin_only = PortraitSettings {
            smooth: 40.0,
            blemish: 60.0,
            ..PortraitSettings::NEUTRAL
        };
        app.apply_portrait(skin_only, on).unwrap();
        let canvas = &app.docs.documents[0].canvas;
        assert_eq!(canvas.layer_stack.layers.len(), 2);
        assert_eq!(canvas.layer_stack.layers[1].name, RESULT_LAYER);
        assert_eq!(photo_pixels(&app), original, "the photo layer is untouched");
        assert_eq!(canvas.undo_count(), undo_before + 1);
        assert!(app.shell.portrait.is_none());
        // The new layer holds the whole photo and the photo under it is
        // hidden, so none of it shows or prints; undo brings it back.
        let layers = &canvas.layer_stack.layers;
        assert!(!layers[0].visible && layers[1].visible);
        assert_eq!(
            layers[1].tiles.get_pixel(0, 0),
            layers[0].tiles.get_pixel(0, 0)
        );
        app.docs.documents[0].canvas.undo();
        let layers = &app.docs.documents[0].canvas.layer_stack.layers;
        assert_eq!(layers.len(), 1);
        assert!(layers[0].visible, "shown again by undo");
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

    #[test]
    fn a_preset_given_at_the_start_is_what_the_dialog_opens_with() {
        let Some(mut app) = app_with_photo() else {
            return;
        };
        let preset = PortraitSettings {
            smooth: 55.0,
            ..PortraitSettings::default()
        };
        app.begin_portrait_from(Some(preset)).unwrap();
        assert_eq!(
            app.portrait_restore(),
            (false, Some(preset), Some(Vec::new()))
        );
        app.cancel_portrait();
        // Without one, the defaults.
        app.begin_portrait_from(None).unwrap();
        assert_eq!(app.portrait_restore().1, Some(PortraitSettings::default()));
        app.cancel_portrait();
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
    fn asking_for_the_crop_tool_applies_the_retouch_and_the_cropped_photo_is_retouched_anew() {
        let Some(mut app) = app_with_photo() else {
            return;
        };
        // Open and idle, the dialog holds nothing.
        app.shell.ui.show_portrait_dialog = true;
        assert!(!app.portrait_under_way() && !app.modal_lock_active());
        app.begin_portrait().unwrap();
        assert!(app.portrait_under_way() && app.modal_lock_active());
        // A tool picked while the face is being found stops the finding.
        app.portrait_tool_picked(true);
        assert!(app.shell.ui.show_portrait_dialog && app.shell.portrait.is_none());
        let model = analysed(&mut app).unwrap();
        app.set_portrait_preview(
            PortraitSettings::default(),
            vec![true; model.faces.len()],
            true,
            false,
        );
        wait_for_preview(&mut app);

        // The retouch as it stands lands in its layer; the dialog stays
        // open and holds the canvas no more.
        app.portrait_tool_picked(true);
        assert!(app.shell.ui.show_portrait_dialog && app.shell.portrait.is_none());
        assert!(!app.portrait_under_way() && !app.modal_lock_active());
        let canvas = &mut app.docs.documents[0].canvas;
        let result = canvas.layer_stack.layers[1].id;
        assert_eq!(canvas.layer_stack.layers.len(), 2);
        assert_eq!(canvas.layer_stack.layers[1].name, RESULT_LAYER);
        assert!(canvas.layer_stack.layers[1].portrait.is_some());
        // The photo is no ID print: the Crop tool is left as it was.
        assert!(app.edit.tools.crop().mode != crate::tools::crop::CropMode::FixedSize);

        // "Tự động làm đẹp" again takes that layer up where it was left.
        app.start_portrait_retouch();
        let session = app.shell.portrait.as_ref().unwrap();
        assert_eq!(session.reopened.as_ref().map(|r| r.layer_id), Some(result));
        assert_eq!(session.restore_settings, Some(PortraitSettings::default()));
        app.cancel_portrait();

        // Cropped, the layer is no longer what its recipe was made on: it is
        // retouched as the photo it now is, from nothing.
        let canvas = &mut app.docs.documents[0].canvas;
        let (w, h) = (canvas.width, canvas.height);
        assert!(canvas.crop(20, 20, w - 40, h - 40, true));
        app.begin_portrait().unwrap();
        let session = app.shell.portrait.as_ref().unwrap();
        assert!(session.reopened.is_none());
        assert_eq!(session.layer_id, result);
        assert_eq!((session.w, session.h), (w - 40, h - 40));
        assert_eq!(session.restore_settings, Some(PortraitSettings::NEUTRAL));
        app.cancel_portrait();
    }

    #[test]
    fn a_command_from_outside_applies_the_previewed_retouch_and_runs_on_it() {
        use winit::keyboard::{KeyCode, PhysicalKey};
        let Some(mut app) = app_with_photo() else {
            return;
        };
        let original = photo_pixels(&app);
        app.shell.ui.show_portrait_dialog = true;
        let previewed = |app: &mut App, settings: PortraitSettings| {
            let model = analysed(app).unwrap();
            let on = vec![true; model.faces.len()];
            app.set_portrait_preview(settings, on, true, false);
            wait_for_preview(app);
        };
        previewed(&mut app, PortraitSettings::default());
        assert_ne!(photo_pixels(&app), original, "preview shows the retouch");
        // On show, the tool in hand gets the canvas.
        app.edit.tools.select(ToolId::Brush);
        app.win.cursor_ownership.pointer_inside = true;
        let view = &mut app.edit.view;
        (view.zoom, view.offset_x, view.offset_y) = (1.0, 300.0, 200.0);
        app.refresh_pointer_ui_state(600.0, 400.0);
        assert!(!app.edit.input.was_over_ui);

        // Ctrl+Z gives the preview up: nothing is added, nothing undone.
        let mut actions = UiActions::default();
        actions.doc.undo = true;
        app.portrait_gives_way(&mut actions);
        assert!(!actions.doc.undo && app.shell.portrait.is_none());
        assert!(app.shell.ui.show_portrait_dialog);
        assert_eq!((layer_count(&app), undo_count(&app)), (1, 0));
        assert_eq!(photo_pixels(&app), original);

        // The Layers panel asks for the photo at half opacity: the retouch
        // lands in its layer, the dialog stays open, and the ask is of that
        // layer.
        previewed(&mut app, PortraitSettings::default());
        let mut actions = UiActions::default();
        actions.layers.set_opacity = Some((0, 0.5));
        app.portrait_gives_way(&mut actions);
        assert!(app.shell.portrait.is_none() && app.shell.ui.show_portrait_dialog);
        assert!(!app.modal_lock_active());
        assert_eq!((layer_count(&app), undo_count(&app)), (2, 1));
        assert_eq!(actions.layers.set_opacity, Some((1, 0.5)));
        let layers = &app.docs.documents[0].canvas.layer_stack.layers;
        assert_eq!(layers[1].name, RESULT_LAYER);
        assert!(!layers[0].visible && layers[1].visible);
        assert_eq!(photo_pixels(&app), original, "the photo layer is untouched");
        let applied = layers[1].tiles.flatten();
        assert_ne!(applied, original);

        // Taken up again, that layer is updated in place by a press of the
        // brush in hand...
        let stronger = PortraitSettings {
            smooth: 90.0,
            ..PortraitSettings::default()
        };
        previewed(&mut app, stronger);
        assert!(app.shell.portrait.as_ref().unwrap().reopened.is_some());
        app.portrait_pressed();
        assert!(app.shell.portrait.is_none() && app.shell.ui.show_portrait_dialog);
        assert_eq!((layer_count(&app), undo_count(&app)), (2, 2));
        let layers = &app.docs.documents[0].canvas.layer_stack.layers;
        assert_ne!(layers[1].tiles.flatten(), applied);

        // ...by Ctrl+L, which goes on to Levels...
        previewed(&mut app, PortraitSettings::default());
        app.edit.input.ctrl_held = true;
        assert!(app.portrait_key_passes(PhysicalKey::Code(KeyCode::KeyL), false));
        app.edit.input.ctrl_held = false;
        assert!(app.shell.portrait.is_none());
        assert_eq!((layer_count(&app), undo_count(&app)), (2, 3));

        // ...and by its own eye in the Layers panel, hidden while the
        // preview shows on the photo: applying shows it, the click is spent.
        previewed(&mut app, stronger);
        let mut actions = UiActions::default();
        actions.layers.toggle_visible = Some(1);
        app.portrait_gives_way(&mut actions);
        assert_eq!(actions.layers.toggle_visible, None);
        let layers = &app.docs.documents[0].canvas.layer_stack.layers;
        assert!(!layers[0].visible && layers[1].visible);
        assert_eq!(layer_count(&app), 2);

        // The view tools look without applying.
        previewed(&mut app, PortraitSettings::default());
        app.edit.tools.select(ToolId::Hand);
        app.portrait_pressed();
        assert!(app.shell.portrait.is_some());
        app.cancel_portrait();

        // Worked on since (a stroke, an adjustment), the layer is not made
        // again from the photo over that work: it is retouched as a photo
        // of its own, from nothing. Undone, it reopens as before.
        let canvas = &mut app.docs.documents[0].canvas;
        let result = canvas.layer_stack.layers[1].id;
        let was = canvas.layer_stack.layers[1].tiles.clone();
        canvas.layer_stack.layers[1]
            .tiles
            .set_pixel(5, 5, 1, 2, 3, 255);
        analysed(&mut app).unwrap();
        let session = app.shell.portrait.as_ref().unwrap();
        assert!(session.reopened.is_none());
        assert_eq!(session.layer_id, result);
        assert_eq!(session.restore_settings, Some(PortraitSettings::NEUTRAL));
        app.cancel_portrait();
        // The photo under it does not reopen it either, and says why.
        let stack = &mut app.docs.documents[0].canvas.layer_stack;
        stack.active_idx = 0;
        let refused = app.begin_portrait().unwrap_err();
        assert!(refused.contains("đã được sửa thêm"), "{refused}");
        let stack = &mut app.docs.documents[0].canvas.layer_stack;
        stack.layers[1].tiles = was;
        stack.active_idx = 1;
        previewed(&mut app, PortraitSettings::default());
        let session = app.shell.portrait.as_ref().unwrap();
        assert_eq!(session.reopened.as_ref().map(|r| r.layer_id), Some(result));

        // Leaving the app applies the retouch on show too, to be saved
        // with the rest.
        let undone = undo_count(&app);
        assert!(app.portrait_on_show());
        app.request_app_exit();
        assert!(app.shell.portrait.is_none());
        assert_eq!((layer_count(&app), undo_count(&app)), (2, undone + 1));
    }

    /// An app whose one document is a blank `w × h` photo at `ppi`.
    fn app_with_blank(w: u32, h: u32, ppi: f32) -> App {
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        let grey = [128u8, 128, 128, 255].repeat((w * h) as usize);
        let mut canvas = Canvas::from_rgba(grey, w, h);
        canvas.metadata.resolution_ppi = ppi;
        app.docs.documents[0].canvas = canvas;
        app
    }

    #[test]
    fn the_crop_tool_keeps_an_id_photo_the_print_it_is() {
        use crate::core::id_photo::{PRINT_PPI, PRINT_PX};
        use crate::tools::crop::CropMode;
        // The dialog open and idle on an ID photo: it stays, and the Crop
        // tool is set to the photo's own pixels and resolution.
        let mut app = app_with_blank(PRINT_PX.0, PRINT_PX.1, PRINT_PPI);
        app.shell.ui.show_portrait_dialog = true;
        app.portrait_tool_picked(true);
        assert!(app.shell.ui.show_portrait_dialog);
        let crop = app.edit.tools.crop();
        assert!(crop.mode == CropMode::FixedSize);
        assert_eq!(
            crop.fixed_size_pixels(PRINT_PX.0, PRINT_PX.1),
            (PRINT_PX.0 as f32, PRINT_PX.1 as f32)
        );
        assert_eq!(crop.dpi, PRINT_PPI);

        // Any other photo leaves the tool as the owner set it, and so does
        // any other tool picked on an ID photo.
        let mut app = app_with_blank(800, 600, 72.0);
        app.shell.ui.show_portrait_dialog = true;
        app.portrait_tool_picked(true);
        assert!(app.edit.tools.crop().mode != CropMode::FixedSize);
        let mut app = app_with_blank(PRINT_PX.0, PRINT_PX.1, PRINT_PPI);
        app.shell.ui.show_portrait_dialog = true;
        app.portrait_tool_picked(false);
        assert!(app.edit.tools.crop().mode != CropMode::FixedSize);
    }

    /// Start a retouch whose face is still being found, with no worker
    /// behind it: a test that ends while one loads its models can hang the
    /// process on its way out.
    fn being_found(app: &mut App) {
        let doc = &app.docs.documents[0];
        let stack = &doc.canvas.layer_stack;
        let layer = &stack.layers[stack.active_idx];
        let (_tx, rx) = mpsc::channel();
        app.shell.portrait = Some(PortraitSession {
            doc_id: doc.id,
            layer_id: layer.id,
            w: layer.width,
            h: layer.height,
            offset: layer.offset,
            original_tiles: layer.tiles.clone(),
            source_visible: layer.visible,
            src: Arc::new(layer.flatten_tiles()),
            progress: Arc::new(Mutex::new(String::new())),
            rx: Some(rx),
            reused: false,
            model: None,
            error: None,
            asked: None,
            wanted: None,
            shown: None,
            rendering: None,
            dressed: false,
            garment: None,
            body_rx: None,
            detail_rx: None,
            neck_rx: None,
            area_rx: None,
            clothes_rx: None,
            enlarged: 1.0,
            edits: Vec::new(),
            edit_rev: 0,
            brush: PortraitBrush::default(),
            reopened: None,
            restore_settings: None,
            restore_faces: None,
        });
    }

    fn undo_count(app: &App) -> usize {
        app.docs.documents[0].canvas.undo_count()
    }

    fn layer_count(app: &App) -> usize {
        app.docs.documents[0].canvas.layer_stack.layers.len()
    }

    #[test]
    fn work_still_running_gives_way_to_a_command_and_ctrl_z_without_a_trace() {
        let mut app = app_with_blank(64, 64, 72.0);
        app.shell.ui.show_portrait_dialog = true;
        being_found(&mut app);
        // Only the retouch holds the canvas: nothing outside is locked.
        assert!(app.modal_lock_active() && !app.locked_beside_retouch());
        assert!(app.portrait_yields());
        // The app is not left while a worker loads its models.
        assert!(!app.portrait_on_show());
        assert_eq!(app.exit_blocking_operation(), Some("live preview"));
        assert!(!app.collect_ui_data().chrome.is_tool_modal);
        // A stray press is not to stop the face being found: the canvas is
        // left to the view tools meanwhile.
        let canvas_is_the_tools = |app: &mut App, tool: ToolId| {
            app.edit.tools.select(tool);
            app.win.cursor_ownership.pointer_inside = true;
            let view = &mut app.edit.view;
            (view.zoom, view.offset_x, view.offset_y) = (10.0, 300.0, 200.0);
            app.refresh_pointer_ui_state(600.0, 400.0);
            !app.edit.input.was_over_ui
        };
        assert!(!canvas_is_the_tools(&mut app, ToolId::Brush));
        assert!(canvas_is_the_tools(&mut app, ToolId::Hand));

        // What the dialog asks itself, and looking at the photo, leave it be.
        let mut actions = UiActions::default();
        actions.doc.zoom_in = true;
        actions.dialogs.set_portrait_preview =
            Some((PortraitSettings::default(), Vec::new(), true, false));
        actions.dialogs.show_portrait_dialog = Some(true);
        app.portrait_gives_way(&mut actions);
        assert!(app.shell.portrait.is_some());
        assert_eq!(actions.dialogs.show_portrait_dialog, None, "already open");

        // A command stops the finding and runs; the dialog stays.
        let mut actions = UiActions::default();
        actions.layers.add_layer = true;
        app.portrait_gives_way(&mut actions);
        assert!(app.shell.portrait.is_none() && app.shell.ui.show_portrait_dialog);
        assert!(actions.layers.add_layer);
        assert!(!app.modal_lock_active());
        assert_eq!((layer_count(&app), undo_count(&app)), (1, 0));
        assert!(
            app.shell.status_msg.contains("dừng nhận diện"),
            "{}",
            app.shell.status_msg
        );

        // Ctrl+Z gives the work up and undoes nothing; redo is not for it.
        for (undo, kept) in [(false, true), (true, false)] {
            being_found(&mut app);
            let mut actions = UiActions::default();
            (actions.doc.undo, actions.doc.redo) = (undo, !undo);
            app.portrait_gives_way(&mut actions);
            assert!(!actions.doc.undo && !actions.doc.redo);
            assert_eq!(app.shell.portrait.is_some(), kept);
            assert!(app.shell.ui.show_portrait_dialog);
            app.cancel_portrait();
        }

        // Under another operation's lock the retouch gives way to nothing.
        being_found(&mut app);
        app.shell.ui.show_adjustment_dialog = true;
        assert!(!app.portrait_yields());
        assert!(app.collect_ui_data().chrome.is_tool_modal);
        let mut actions = UiActions::default();
        actions.layers.add_layer = true;
        app.portrait_gives_way(&mut actions);
        assert_eq!(app.yield_portrait(), None);
        assert!(app.shell.portrait.is_some());
        app.shell.ui.show_adjustment_dialog = false;
        app.cancel_portrait();
    }

    #[test]
    fn a_key_that_edits_goes_on_once_the_retouch_gave_way_and_the_rest_leave_it() {
        use winit::keyboard::{KeyCode, PhysicalKey};
        let mut app = app_with_blank(64, 64, 72.0);
        app.shell.ui.show_portrait_dialog = true;
        let key = |app: &mut App, ctrl: bool, code: KeyCode| {
            if app.shell.portrait.is_none() {
                being_found(app);
            }
            app.edit.input.ctrl_held = ctrl;
            let passes = app.portrait_key_passes(PhysicalKey::Code(code), false);
            (passes, app.shell.portrait.is_some())
        };
        // Enter and Esc are the dialog's, an unbound key is nobody's.
        for code in [KeyCode::Enter, KeyCode::Escape, KeyCode::F9, KeyCode::Tab] {
            assert_eq!(key(&mut app, false, code), (false, true), "{code:?}");
        }
        // Copy, the tip size and the paint colours change no pixel.
        assert_eq!(key(&mut app, true, KeyCode::KeyC), (true, true));
        for code in [KeyCode::BracketLeft, KeyCode::KeyX, KeyCode::KeyD] {
            assert_eq!(key(&mut app, false, code), (true, true), "{code:?}");
        }
        // Arrows move nothing with this tool and no selection.
        app.edit.tools.select(ToolId::Brush);
        assert_eq!(key(&mut app, false, KeyCode::ArrowLeft), (false, true));
        // Typed into a field, Ctrl+A and Ctrl+V are the field's.
        let field = egui::Id::new("a value being typed");
        app.win.egui_ctx.memory_mut(|m| m.request_focus(field));
        assert!(app.win.egui_ctx.egui_wants_keyboard_input());
        for code in [KeyCode::KeyA, KeyCode::KeyV] {
            assert_eq!(key(&mut app, true, code), (false, true), "{code:?}");
        }
        app.win.egui_ctx.memory_mut(|m| m.surrender_focus(field));
        // Ctrl+L, Ctrl+M, Ctrl+X, Ctrl+V, Delete, Ctrl+E: the work under
        // way gives way and the key goes on to its command.
        for (ctrl, code) in [
            (true, KeyCode::KeyL),
            (true, KeyCode::KeyM),
            (true, KeyCode::KeyX),
            (true, KeyCode::KeyV),
            (false, KeyCode::Delete),
            (true, KeyCode::KeyE),
        ] {
            assert_eq!(key(&mut app, ctrl, code), (true, false), "{code:?}");
            assert!(app.shell.ui.show_portrait_dialog);
        }
        app.edit.tools.select(ToolId::Move);
        assert_eq!(key(&mut app, false, KeyCode::ArrowLeft), (true, false));
        // Ctrl+Z gives the work up and is spent on that; redo does nothing.
        assert_eq!(key(&mut app, true, KeyCode::KeyZ), (false, false));
        app.edit.input.shift_held = true;
        assert_eq!(key(&mut app, true, KeyCode::KeyZ), (false, true));
        app.edit.input.shift_held = false;
        assert_eq!(undo_count(&app), 0);
        app.cancel_portrait();
    }

    #[test]
    fn a_frame_nobody_touches_asks_nothing_from_outside_the_dialog() {
        let mut app = app_with_blank(800, 600, 72.0);
        app.shell.ui.show_portrait_dialog = true;
        being_found(&mut app);
        let ctx = app.win.egui_ctx.clone();
        ctx.set_fonts(crate::ui::snapshot::fonts());
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 900.0));
        let at_rest = |app: &mut App, rest: egui::Pos2, what: &str| {
            for _ in 0..3 {
                let data = app.collect_ui_data();
                let mut actions = UiActions::default();
                let input = egui::RawInput {
                    screen_rect: Some(screen),
                    events: vec![egui::Event::PointerMoved(rest)],
                    ..Default::default()
                };
                let _ = ctx.run_ui(input, |ui| crate::ui::frame(ui, &data, &mut actions));
                assert!(actions.dialogs.set_portrait_preview.is_some(), "drawn");
                assert!(
                    !actions.reaches_past_retouch() && actions.tool.select_tool.is_none(),
                    "a frame at rest on {rest:?} ({what}) reads as a command"
                );
                app.portrait_gives_way(&mut actions);
                assert!(app.shell.portrait.is_some());
            }
        };
        // The pointer resting on the menus, the toolbar, the canvas, the
        // panels on the right and the status bar; then with the AI panel and
        // every docked panel open.
        let rests = [
            egui::pos2(120.0, 12.0),
            egui::pos2(18.0, 300.0),
            egui::pos2(600.0, 450.0),
            egui::pos2(1300.0, 300.0),
            egui::pos2(1300.0, 700.0),
            egui::pos2(700.0, 890.0),
        ];
        for panels in [false, true] {
            if panels {
                let ui = &mut app.shell.ui;
                ui.show_ai_panel = true;
                ui.show_history_panel = true;
                ui.show_info_panel = true;
                ui.show_channels_panel = true;
                ui.show_color_panel = true;
                ui.show_rulers = true;
            }
            for rest in rests {
                at_rest(&mut app, rest, if panels { "panels" } else { "plain" });
            }
        }
        // Each tool in hand, the pointer on its options bar and on the
        // canvas: none is given up for another, none sets an option.
        for tool in [
            ToolId::Brush,
            ToolId::Eraser,
            ToolId::Pencil,
            ToolId::Move,
            ToolId::Crop,
            ToolId::Zoom,
            ToolId::Hand,
            ToolId::Fill,
            ToolId::Gradient,
            ToolId::Eyedropper,
            ToolId::SelectionRect,
            ToolId::SelectionEllipse,
            ToolId::Lasso,
            ToolId::PolygonLasso,
            ToolId::SmartSelect,
            ToolId::Clone,
            ToolId::Text,
            ToolId::Shape,
            ToolId::Repair,
            ToolId::PerspectiveCrop,
            ToolId::Pen,
            ToolId::Smudge,
            ToolId::Dodge,
            ToolId::Burn,
            ToolId::Patch,
            ToolId::Node,
            ToolId::VectorBrush,
            ToolId::Arrow,
        ] {
            app.edit.tools.select(tool);
            for rest in [egui::pos2(300.0, 42.0), egui::pos2(600.0, 450.0)] {
                at_rest(&mut app, rest, tool.name());
            }
        }
        app.cancel_portrait();
    }

    #[test]
    fn a_sheet_asked_with_no_retouch_under_way_lays_out_the_photo_as_it_is() {
        use crate::core::imposition::{Paper, PhotoKind};
        let (w, h) = PhotoKind::Id3x4.cell_px();
        let mut app = app_with_blank(w, h, 600.0);
        app.shell.ui.show_portrait_dialog = true;
        let sheet = Sheet::Grid(Paper::P10x15, PhotoKind::Id3x4);
        app.finish_portrait_dialog(
            PortraitSettings::default(),
            Vec::new(),
            Some((sheet, SheetOptions::default())),
        );
        assert!(!app.shell.ui.show_portrait_dialog);
        assert_eq!(app.docs.documents.len(), 2);
        assert_eq!(
            app.docs.documents[app.docs.active_doc_idx].title,
            "Trang 10×15 — 10 tấm 3×4"
        );
        // Without a sheet there is nothing to apply and the dialog stays.
        let mut app = app_with_blank(w, h, 600.0);
        app.shell.ui.show_portrait_dialog = true;
        app.finish_portrait_dialog(PortraitSettings::default(), Vec::new(), None);
        assert!(app.shell.ui.show_portrait_dialog);
        assert_eq!(app.docs.documents.len(), 1);
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
    fn corrections_recolour_the_whole_layer_before_the_look() {
        let Some(mut app) = app_with_photo() else {
            return;
        };
        let original = photo_pixels(&app);
        let model = analysed(&mut app).unwrap();
        let faces = vec![true; model.faces.len()];
        assert!(model.light.skin.is_some(), "skin was measured");
        let settings = PortraitSettings {
            fix_cast: 100.0,
            fix_warmth: 40.0,
            fix_exposure: 100.0,
            fix_haze: 60.0,
            look: StudioLook::Warm.index(),
            look_strength: 50.0,
            ..PortraitSettings::NEUTRAL
        };

        // The preview shows them over the whole photo.
        app.set_portrait_preview(settings, faces.clone(), true, false);
        wait_for_preview(&mut app);
        let shown = photo_pixels(&app);
        assert_ne!(&shown[..3], &original[..3], "far from the face too");

        // Applied: one layer, the corrections in full under half the look.
        app.apply_portrait(settings, faces).unwrap();
        let layers = &app.docs.documents[0].canvas.layer_stack.layers;
        let result = layers.last().unwrap();
        assert_eq!(result.name, RESULT_LAYER);
        let fix = correct::fix_lut(&model.light, &settings.fixes().unwrap()).unwrap();
        let mut expected = original[..4].to_vec();
        fix.apply(&mut expected, 1.0);
        LookLut::new(StudioLook::Warm)
            .unwrap()
            .apply(&mut expected, 0.5);
        assert_eq!(&result.tiles.flatten()[..4], &expected[..]);
        assert_eq!(&shown[..4], &expected[..], "as the preview showed");
        let recipe = result.portrait.clone().expect("recipe kept");
        assert_eq!(recipe.settings.fixes(), settings.fixes());
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
        let mut inside = 0;
        for y in (0..h).step_by(4) {
            for x in (0..w).step_by(4) {
                let changed = changed_at(&app, x, y);
                if x >= x0 && x < x1 && y >= y0 && y < y1 {
                    inside += changed as u32;
                } else {
                    assert!(!changed, "changed outside the selection at {x},{y}");
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
        assert_eq!(
            app.apply_portrait(first, faces.clone()).unwrap(),
            Applied::Added
        );
        let layers = |app: &App| app.docs.documents[0].canvas.layer_stack.layers.clone();
        let undo_after_first = app.docs.documents[0].canvas.undo_count();
        let recipe = layers(&app)[1].portrait.clone().expect("recipe kept");
        assert_eq!(recipe.settings, first);
        assert_eq!(recipe.faces[0].hair.as_ref().unwrap().mask[k], 255);
        assert!(recipe.faces[0].skin.is_none());
        let first_pixels = layers(&app)[1].tiles.flatten();

        assert!(!layers(&app)[0].visible, "the photo is hidden once applied");

        // Reopened from the "Chân dung" layer (active after OK): sliders,
        // faces and the painted hair come back; the layer hides meanwhile
        // and the photo shows, to carry the preview. The analysis is the
        // one already made: no model runs again.
        let reanalysed = analysed(&mut app).unwrap();
        assert!(Arc::ptr_eq(&reanalysed, &model), "the analysis is reused");
        let (status, ..) = app.portrait_dialog_state();
        assert!(status.contains("dùng lại"), "{status}");
        assert!(!layers(&app)[1].visible, "hidden while previewing");
        assert!(layers(&app)[0].visible, "the photo shows meanwhile");
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
        assert!(!layers(&app)[0].visible, "and the photo hidden again");
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
        assert_eq!(app.apply_portrait(second, faces).unwrap(), Applied::Updated);
        let after = layers(&app);
        assert_eq!(after.len(), 2);
        assert!(after[1].visible && !after[0].visible);
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
        assert_eq!(app.apply_portrait(darker, faces).unwrap(), Applied::Added);
        let layers = &app.docs.documents[0].canvas.layer_stack.layers;
        let recipe = layers[1].portrait.clone().expect("recipe kept");
        assert_eq!(recipe.faces[0].brows.as_ref().unwrap().mask[spot], 255);
        assert!(recipe.faces[0].skin.is_none(), "no skin was painted");
        assert!(
            changed_at(&app, at.0 as u32, at.1 as u32),
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

        assert_eq!(app.apply_portrait(waist, faces).unwrap(), Applied::Added);
        assert!(
            changed_at(&app, edge[0] as u32, edge[1] as u32),
            "the waist's edge moved"
        );
        assert!(!changed_at(&app, w / 2, 2), "above the head stays");
        let layer = &app.docs.documents[0].canvas.layer_stack.layers[1];
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
        let busy = |app: &App| {
            app.portrait_detail_note()
                .is_some_and(|(note, _)| note.starts_with("Đang"))
        };
        while !made(&model) || busy(&app) {
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

        assert_eq!(app.apply_portrait(detail, faces).unwrap(), Applied::Added);
        let nose = model.faces[0].mesh.points[4];
        let near =
            (-6i32..=6).any(|d| changed_at(&app, (nose[0] as i32 + d) as u32, nose[1] as u32));
        assert!(near, "the skin by the nose took the detail");
        let layer = &app.docs.documents[0].canvas.layer_stack.layers[1];
        let recipe = layer.portrait.clone().expect("recipe kept");
        assert_eq!(recipe.settings.ai_detail, 100.0);
    }

    /// A customer's phone photo, in a polo shirt, with the dialog open;
    /// `None` without the photo or the models that find a face and clothes.
    fn app_with_customer() -> Option<App> {
        let path = std::path::Path::new("tmp/anh-the/am-mau/khach_1.jpg");
        if !path.is_file()
            || crate::core::ai::face_mesh::model_path().is_none()
            || crate::core::ai::body_parts::model_path().is_none()
        {
            return None;
        }
        let image = image::open(path).ok()?.to_rgba8();
        let (w, h) = image.dimensions();
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        app.docs.documents[0].canvas = Canvas::from_rgba(image.into_raw(), w, h);
        app.shell.ui.show_portrait_dialog = true;
        Some(app)
    }

    #[test]
    fn the_brush_mends_the_clothes_and_their_light_moves_only_what_is_left() {
        use crate::core::portrait::brush::MaskTarget;
        use crate::core::refine::{MaskBrushEvent, StampOp};
        let Some(mut app) = app_with_customer() else {
            return;
        };
        let model = analysed(&mut app).unwrap();
        let faces = vec![true; model.faces.len()];
        let found = |model: &PortraitModel| model.faces[0].clothes_area.get().is_some();
        app.set_portrait_preview(PortraitSettings::NEUTRAL, faces.clone(), true, false);
        assert!(!found(&model), "not before they are asked for");

        // Picking the clothes for the brush has them found.
        app.set_portrait_brush_target(Some(MaskTarget::Clothes));
        assert_eq!(
            app.portrait_clothes_note(),
            Some(("Đang tìm áo…".to_string(), false))
        );
        let started = Instant::now();
        while !found(&model)
            || app
                .portrait_clothes_note()
                .is_some_and(|n| n.0.starts_with("Đang"))
        {
            assert!(
                started.elapsed() < Duration::from_secs(240),
                "the clothes hung"
            );
            std::thread::sleep(Duration::from_millis(50));
            app.poll_portrait();
        }
        let clothes = model.faces[0].clothes_area.get().unwrap().as_ref().unwrap();
        assert!(model.faces[0].clothes.get().is_none(), "no model drew them");
        let (r, b) = (clothes.region, clothes.bounds());
        let solid = |x: u32, y: u32| clothes.mask()[r.index_at(x, y).unwrap()] == 255;
        // A point well inside the shirt to rub out, another to keep.
        let rubbed = (b.x + b.w / 2, b.y + b.h / 2);
        let kept = (b.x + b.w / 2, b.y + b.h * 4 / 5);
        assert!(solid(rubbed.0, rubbed.1) && solid(kept.0, kept.1));
        let k = r.index_at(rubbed.0, rubbed.1).unwrap();
        let radius = model.faces[0].extent * 0.2;
        assert!((kept.1 - rubbed.1) as f32 > 2.0 * radius);

        let queue = app.docs.documents[0].canvas.mask_brush.as_mut().unwrap();
        queue.push(MaskBrushEvent::Begin(StampOp::Subtract));
        queue.push(MaskBrushEvent::Dabs {
            points: vec![(rubbed.0 as f32, rubbed.1 as f32)],
            radius,
            hardness: 1.0,
        });
        queue.push(MaskBrushEvent::End);
        app.poll_portrait_brush();
        app.set_portrait_brush_target(None);
        let session = app.shell.portrait.as_ref().unwrap();
        assert_eq!(session.edits[0].clothes.as_ref().unwrap().mask()[k], 0);

        // Darker clothes: the shirt left in the mask, not the patch rubbed
        // out of it, and not the face.
        let darker = PortraitSettings {
            clothes_brightness: -100.0,
            ..PortraitSettings::NEUTRAL
        };
        assert_eq!(
            app.apply_portrait(darker, faces.clone()).unwrap(),
            Applied::Added
        );
        assert!(changed_at(&app, kept.0, kept.1), "the shirt went darker");
        assert!(
            !changed_at(&app, rubbed.0, rubbed.1),
            "the patch is as shot"
        );
        let nose = model.faces[0].mesh.points[4];
        assert!(!changed_at(&app, nose[0] as u32, nose[1] as u32));
        let layer = &app.docs.documents[0].canvas.layer_stack.layers[1];
        let recipe = layer.portrait.clone().expect("recipe kept");
        assert_eq!(recipe.settings, darker);
        assert_eq!(recipe.faces[0].clothes.as_ref().unwrap().mask[k], 0);

        // Reopened, the painted clothes come back with the sliders.
        let again = analysed(&mut app).unwrap();
        assert!(Arc::ptr_eq(&again, &model), "the analysis is reused");
        let session = app.shell.portrait.as_ref().unwrap();
        assert_eq!(session.edits[0].clothes.as_ref().unwrap().mask()[k], 0);
        assert_eq!(app.portrait_restore().1, Some(darker));
    }

    #[test]
    fn the_neck_takes_its_detail_on_first_use_where_the_brush_leaves_it() {
        use crate::core::portrait::brush::MaskTarget;
        use crate::core::refine::{MaskBrushEvent, StampOp};
        let Some(mut app) = app_with_customer() else {
            return;
        };
        if !crate::core::ai::retouch::FaceRestorer::installed() {
            return;
        }
        let model = analysed(&mut app).unwrap();
        let faces = vec![true; model.faces.len()];
        let found = |model: &PortraitModel| model.faces[0].neck.get().is_some();
        app.set_portrait_preview(PortraitSettings::NEUTRAL, faces.clone(), true, false);
        wait_for_preview(&mut app);
        assert!(!found(&model), "not before the slider moves");
        assert_eq!(app.portrait_neck_note(), None);
        let plain = photo_pixels(&app);

        // The slider leaving 0 starts the model; the preview follows.
        let neck = PortraitSettings {
            neck: 100.0,
            ..PortraitSettings::NEUTRAL
        };
        app.set_portrait_preview(neck, faces.clone(), true, false);
        assert_eq!(
            app.portrait_neck_note(),
            Some(("Đang tìm da cổ và tạo chi tiết bằng AI…".to_string(), false))
        );
        let started = Instant::now();
        while !found(&model) || app.portrait_neck_note().is_some() {
            assert!(
                started.elapsed() < Duration::from_secs(240),
                "the neck hung"
            );
            std::thread::sleep(Duration::from_millis(50));
            app.poll_portrait();
        }
        wait_for_preview(&mut app);
        assert_ne!(photo_pixels(&app), plain, "the preview shows the neck");
        let face = &model.faces[0];
        assert!(
            face.ai_detail.get().is_none(),
            "the face's detail is not made"
        );
        let skin_region = face.skin.region();
        let mask = face.neck.get().unwrap().as_ref().unwrap().mask(&face.skin);
        // Two points well inside the neck: one to rub out, one to keep.
        let solid: Vec<(u32, u32)> = (0..mask.len())
            .filter(|&i| mask[i] == 255)
            .map(|i| {
                (
                    skin_region.x + (i % skin_region.w as usize) as u32,
                    skin_region.y + (i / skin_region.w as usize) as u32,
                )
            })
            .collect();
        assert!(solid.len() > 2000, "a neck was found: {}", solid.len());
        let (top, bottom) = (solid[0].1, solid[solid.len() - 1].1);
        let middle_of = |row: u32| {
            let columns: Vec<u32> = solid.iter().filter(|p| p.1 == row).map(|p| p.0).collect();
            (columns[columns.len() / 2], row)
        };
        let rubbed = middle_of(top + (bottom - top) / 3);
        let kept = middle_of(top + (bottom - top) * 3 / 4);
        let radius = face.extent * 0.05;
        assert!((kept.1 - rubbed.1) as f32 > 3.0 * radius);
        let k = skin_region.index_at(rubbed.0, rubbed.1).unwrap();
        let j = skin_region.index_at(kept.0, kept.1).unwrap();
        // And one below it that the skin mask does not hold: the shirt.
        let column = middle_of(bottom).0;
        let skin_at = |y: u32| skin_region.index_at(column, y).map(|i| face.skin.mask()[i]);
        let below = (bottom..skin_region.y + skin_region.h)
            .find(|&y| skin_at(y) == Some(0))
            .expect("the skin ends above its region's end");
        let added = (column, below + (2.0 * radius) as u32);
        let a = skin_region
            .index_at(added.0, added.1)
            .expect("inside the skin's region");
        assert_eq!((face.skin.mask()[a], mask[a]), (0, 0));

        // The brush shows the neck as found, takes a patch out of it and
        // adds one the skin mask never held.
        app.set_portrait_brush_target(Some(MaskTarget::Neck));
        let queue = app.docs.documents[0].canvas.mask_brush.as_mut().unwrap();
        for (op, at) in [(StampOp::Subtract, rubbed), (StampOp::Add, added)] {
            queue.push(MaskBrushEvent::Begin(op));
            queue.push(MaskBrushEvent::Dabs {
                points: vec![(at.0 as f32, at.1 as f32)],
                radius,
                hardness: 1.0,
            });
            queue.push(MaskBrushEvent::End);
        }
        app.poll_portrait_brush();
        app.set_portrait_brush_target(None);
        let session = app.shell.portrait.as_ref().unwrap();
        let painted = session.edits[0].neck.as_ref().expect("the neck as painted");
        assert_eq!((painted[k], painted[j], painted[a]), (0, 255, 255));

        assert_eq!(
            app.apply_portrait(neck, faces.clone()).unwrap(),
            Applied::Added
        );
        let around =
            |at: (u32, u32)| (-4i32..=4).any(|d| changed_at(&app, (at.0 as i32 + d) as u32, at.1));
        assert!(around(kept), "the neck took the detail");
        assert!(
            !changed_at(&app, rubbed.0, rubbed.1),
            "the patch is as shot"
        );
        assert!(around(added), "what the brush added is neck too");
        // Not a pixel of the face moved.
        let outline: Vec<[f32; 2]> = crate::core::portrait::geometry::FACE_OVAL
            .iter()
            .map(|&p| {
                let p = face.mesh.points[p as usize];
                [p[0], p[1]]
            })
            .collect();
        let inside = |x: u32, y: u32| {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let mut hit = false;
            let mut last = outline[outline.len() - 1];
            for &point in &outline {
                if (point[1] > py) != (last[1] > py)
                    && px < (last[0] - point[0]) * (py - point[1]) / (last[1] - point[1]) + point[0]
                {
                    hit = !hit;
                }
                last = point;
            }
            hit
        };
        let r = face.region;
        let moved = (r.y..r.y + r.h)
            .flat_map(|y| (r.x..r.x + r.w).map(move |x| (x, y)))
            .filter(|&(x, y)| inside(x, y) && changed_at(&app, x, y))
            .count();
        assert_eq!(moved, 0, "the face is as shot");
        let layer = &app.docs.documents[0].canvas.layer_stack.layers[1];
        let recipe = layer.portrait.clone().expect("recipe kept");
        assert_eq!(recipe.settings, neck);
        assert_eq!(recipe.faces[0].neck.as_ref().unwrap().mask[k], 0);

        // Reopened, the painted neck comes back with the slider.
        let again = analysed(&mut app).unwrap();
        assert!(Arc::ptr_eq(&again, &model), "the analysis is reused");
        let session = app.shell.portrait.as_ref().unwrap();
        let painted = session.edits[0].neck.as_ref().expect("the neck kept");
        assert_eq!((painted[k], painted[j], painted[a]), (0, 255, 255));
        assert_eq!(app.portrait_restore().1, Some(neck));
    }

    /// The customer's photo wearing a garment from a sheet: the photo is the
    /// layer "Người", and a plain garment, dimmer toward the right, the
    /// layer "Áo" over its lower part. Returns the garment's layer and its
    /// pixels as laid, with their width.
    fn dressed_customer() -> Option<(App, u32, Vec<u8>, u32)> {
        use super::super::garment_ops::{GARMENT_LAYER, PERSON_LAYER};
        let mut app = app_with_customer()?;
        let canvas = &mut app.docs.documents[0].canvas;
        let (w, h) = (canvas.width, canvas.height);
        canvas.layer_stack.layers[0].name = PERSON_LAYER.to_string();
        let (gw, gh) = (w / 2, h / 4);
        let pixels: Vec<u8> = (0..gw * gh)
            .flat_map(|i| {
                let x = i % gw;
                let shows = x >= 10 && x < gw - 10;
                let v = 190 - (x * 60 / gw) as u8;
                [v, v, v + 8, if shows { 255 } else { 0 }]
            })
            .collect();
        let at = canvas.layer_stack.add_layer(gw, gh);
        let layer = &mut canvas.layer_stack.layers[at];
        layer.name = GARMENT_LAYER.to_string();
        layer.tiles = TileMap::from_rgba(&pixels, gw, gh);
        layer.offset = ((w / 4) as i32, (h * 3 / 4) as i32);
        layer.selected = false;
        let garment = layer.id;
        // The person is the layer worked on.
        canvas.layer_stack.active_idx = 0;
        canvas.layer_stack.layers[0].selected = true;
        Some((app, garment, pixels, gw))
    }

    fn garment_px(app: &App, garment: u32, x: u32, y: u32) -> [u8; 4] {
        let layers = &app.docs.documents[0].canvas.layer_stack.layers;
        let layer = layers.iter().find(|l| l.id == garment).unwrap();
        layer.tiles.get_pixel(x, y).into()
    }

    #[test]
    fn a_garment_worn_takes_the_clothes_light_and_can_be_relit_again() {
        let Some((mut app, garment, laid, gw)) = dressed_customer() else {
            return;
        };
        let doc_id = app.docs.documents[0].id;
        let model = analysed(&mut app).unwrap();
        let faces = vec![true; model.faces.len()];
        let at = |app: &App| garment_px(app, garment, 60, 40);
        let as_laid = at(&app);
        let light = |s: PortraitSettings| (s.clothes_brightness, s.clothes_even, s.clothes_sharpen);
        // A photo taken up for the first time would start from the usual
        // sliders: the garment's light stands at rest.
        assert_eq!(app.portrait_restore().1.map(light), Some((0.0, 0.0, 0.0)));
        app.set_portrait_preview(PortraitSettings::NEUTRAL, faces.clone(), true, false);
        wait_for_preview(&mut app);
        let photo = photo_pixels(&app);

        // "Sáng áo" darkens the garment in the preview, and nothing of the
        // person's layer, whose own clothes are not even looked for.
        let darker = PortraitSettings {
            clothes_brightness: -100.0,
            ..PortraitSettings::NEUTRAL
        };
        app.set_portrait_preview(darker, faces.clone(), true, false);
        wait_for_preview(&mut app);
        assert!(at(&app)[0] < as_laid[0] - 40, "{:?}", at(&app));
        assert_eq!(photo_pixels(&app), photo, "the person's layer stays");
        assert!(model.faces[0].clothes_area.get().is_none());
        assert_eq!(app.portrait_clothes_note(), None);
        // With the preview off the garment shows as laid.
        app.set_portrait_preview(darker, faces.clone(), false, false);
        wait_for_preview(&mut app);
        assert_eq!(at(&app), as_laid);
        app.set_portrait_preview(darker, faces.clone(), true, false);
        wait_for_preview(&mut app);

        // Applied, only the garment's layer changes, in one step.
        let canvas = &app.docs.documents[0].canvas;
        let (layers, steps) = (canvas.layer_stack.layers.len(), canvas.undo_count());
        assert_eq!(
            app.apply_portrait(darker, faces.clone()).unwrap(),
            Applied::Garment
        );
        let canvas = &app.docs.documents[0].canvas;
        assert_eq!(canvas.layer_stack.layers.len(), layers, "no layer is added");
        assert_eq!(canvas.undo_count(), steps + 1);
        assert!(at(&app)[0] < as_laid[0] - 40);
        assert_eq!(garment_px(&app, garment, 2, 40)[3], 0, "what hides stays");

        // Taken up again the slider stands where it was left, and half of
        // it is made of the garment as laid, not of the darker one.
        analysed(&mut app).unwrap();
        assert_eq!(
            app.portrait_restore().1.map(light),
            Some((-100.0, 0.0, 0.0))
        );
        let half = PortraitSettings {
            clothes_brightness: -50.0,
            ..PortraitSettings::NEUTRAL
        };
        app.set_portrait_preview(half, faces.clone(), true, false);
        wait_for_preview(&mut app);
        let gh = laid.len() as u32 / 4 / gw;
        let expected = portrait::LaidGarment::read(&laid, gw, gh).relit(&laid, gw, -50.0, 0.0);
        let o = ((40 * gw + 60) * 4) as usize;
        assert_eq!(at(&app)[..], expected[o..o + 4]);
        assert_eq!(
            app.apply_portrait(half, faces.clone()).unwrap(),
            Applied::Garment
        );
        assert_eq!(at(&app)[..], expected[o..o + 4]);

        // Back at rest the garment is as laid, and nothing is kept of it.
        analysed(&mut app).unwrap();
        assert_eq!(
            app.apply_portrait(PortraitSettings::NEUTRAL, faces.clone())
                .unwrap(),
            Applied::Garment
        );
        assert_eq!(at(&app), as_laid);
        assert!(app.garment_relit(doc_id, garment).is_none());

        // With a retouch of the person too, both land in one step.
        analysed(&mut app).unwrap();
        let both = PortraitSettings {
            smooth: 80.0,
            clothes_brightness: 60.0,
            ..PortraitSettings::NEUTRAL
        };
        let steps = app.docs.documents[0].canvas.undo_count();
        assert_eq!(app.apply_portrait(both, faces).unwrap(), Applied::Added);
        assert_eq!(app.docs.documents[0].canvas.undo_count(), steps + 1);
        assert!(at(&app)[0] > as_laid[0], "the garment is lighter");
        app.docs.documents[0].canvas.undo();
        assert_eq!(at(&app), as_laid, "undone with the retouch");
    }

    #[test]
    fn a_dressed_photo_mended_by_hand_is_retouched_on_from_what_shows_of_the_person() {
        let Some((mut app, garment, _, _)) = dressed_customer() else {
            return;
        };
        let model = analysed(&mut app).unwrap();
        let faces = vec![true; model.faces.len()];
        let smooth = PortraitSettings {
            smooth: 80.0,
            ..PortraitSettings::NEUTRAL
        };
        assert_eq!(app.apply_portrait(smooth, faces).unwrap(), Applied::Added);
        // The retouch is mended by hand, then the garment is worked on: its
        // layer is the active one.
        let stack = &mut app.docs.documents[0].canvas.layer_stack;
        let retouch = stack
            .layers
            .iter()
            .position(|l| l.portrait.is_some())
            .expect("the retouch's layer");
        let retouch_id = stack.layers[retouch].id;
        stack.layers[retouch].tiles.set_pixel(5, 5, 1, 2, 3, 255);
        stack.active_idx = stack.layers.iter().position(|l| l.id == garment).unwrap();

        // Asked to go on, the retouch is of what shows of the person, from
        // sliders at rest, and the garment is the session's again.
        app.shell.ui.show_portrait_dialog = true;
        app.start_portrait_retouch();
        assert_eq!(app.shell.portrait_error, None);
        let session = app.shell.portrait.as_ref().expect("a retouch under way");
        assert_eq!(session.layer_id, retouch_id);
        assert!(session.dressed && session.garment.is_some());
        assert!(session.reopened.is_none(), "it is no longer what was made");
        assert_eq!(app.portrait_restore().1, Some(PortraitSettings::NEUTRAL));
        let stack = &app.docs.documents[0].canvas.layer_stack;
        assert_eq!(stack.layers[stack.active_idx].id, retouch_id);
    }

    #[test]
    fn a_garment_about_to_be_transformed_keeps_the_light_it_shows() {
        let Some((mut app, garment, _, _)) = dressed_customer() else {
            return;
        };
        let model = analysed(&mut app).unwrap();
        let faces = vec![true; model.faces.len()];
        let at = |app: &App| garment_px(app, garment, 60, 40);
        let as_laid = at(&app);
        let darker = PortraitSettings {
            clothes_brightness: -100.0,
            ..PortraitSettings::NEUTRAL
        };
        app.set_portrait_preview(darker, faces.clone(), true, false);
        wait_for_preview(&mut app);
        let shown = at(&app);
        assert!(shown[0] < as_laid[0] - 40);

        // What shows becomes the garment's own, a step of its own, and the
        // session lets the garment go.
        let steps = app.docs.documents[0].canvas.undo_count();
        app.settle_garment_light();
        assert_eq!(app.docs.documents[0].canvas.undo_count(), steps + 1);
        assert_eq!(at(&app), shown);
        let session = app.shell.portrait.as_ref().unwrap();
        assert!(session.garment.is_none() && session.dressed);

        // Nothing was done to it after all: the session takes it again
        // where it stands, and giving the session up leaves it so.
        app.set_portrait_preview(darker, faces.clone(), true, false);
        let session = app.shell.portrait.as_ref().unwrap();
        assert_eq!(
            session.garment.as_ref().map(|g| g.start),
            Some((-100.0, 0.0))
        );
        app.cancel_portrait();
        assert_eq!(at(&app), shown);

        // Worked on since (here, painted), it is relit from what it now is:
        // the sliders start at rest.
        let layers = &mut app.docs.documents[0].canvas.layer_stack.layers;
        let layer = layers.iter_mut().find(|l| l.id == garment).unwrap();
        layer.tiles.set_pixel(5, 5, 1, 2, 3, 255);
        analysed(&mut app).unwrap();
        let start = app.portrait_restore().1.unwrap();
        assert_eq!((start.clothes_brightness, start.clothes_even), (0.0, 0.0));
    }

    #[test]
    fn clothes_are_drawn_on_first_use_and_the_faces_stay_as_shot() {
        let Some(mut app) = app_with_customer() else {
            return;
        };
        if !crate::core::ai::retouch::Upscaler::installed() {
            return;
        }
        let model = analysed(&mut app).unwrap();
        let faces = vec![true; model.faces.len()];
        let made = |model: &PortraitModel| model.faces.iter().all(|f| f.clothes.get().is_some());
        assert!(!made(&model), "not before the slider moves");
        assert_eq!(
            app.portrait_clothes_note(),
            Some((
                "Lần đầu kéo thanh, app tìm áo rồi làm nét vài giây.".to_string(),
                false
            ))
        );

        // The slider leaving 0 starts the search and the model.
        let sharp = PortraitSettings {
            clothes_sharpen: 100.0,
            ..PortraitSettings::NEUTRAL
        };
        app.set_portrait_preview(sharp, faces.clone(), true, false);
        assert_eq!(
            app.portrait_clothes_note(),
            Some(("Đang tìm áo và làm nét bằng AI…".to_string(), false))
        );
        let started = Instant::now();
        let busy = |app: &App| {
            app.portrait_clothes_note()
                .is_some_and(|(note, _)| note.starts_with("Đang"))
        };
        while !made(&model) || busy(&app) {
            assert!(
                started.elapsed() < Duration::from_secs(240),
                "the clothes hung"
            );
            std::thread::sleep(Duration::from_millis(50));
            app.poll_portrait();
        }
        wait_for_preview(&mut app);
        let worn: Vec<_> = model
            .faces
            .iter()
            .filter(|face| face.clothes.get().is_some_and(|drawn| drawn.is_ok()))
            .filter_map(|face| face.clothes_area.get()?.as_ref().ok())
            .collect();
        assert!(!worn.is_empty(), "no clothes found");

        assert_eq!(app.apply_portrait(sharp, faces).unwrap(), Applied::Added);
        let redrawn = worn.iter().any(|clothes| {
            let r = clothes.region;
            (0..r.len()).step_by(7).any(|i| {
                clothes.mask()[i] == 255
                    && changed_at(&app, r.x + i as u32 % r.w, r.y + i as u32 / r.w)
            })
        });
        assert!(redrawn, "the clothes took the model's picture");
        for face in &model.faces {
            let nose = face.mesh.points[4];
            assert!(
                !changed_at(&app, nose[0] as u32, nose[1] as u32),
                "a face is as shot"
            );
        }
        let layer = &app.docs.documents[0].canvas.layer_stack.layers[1];
        let recipe = layer.portrait.clone().expect("recipe kept");
        assert_eq!(recipe.settings.clothes_sharpen, 100.0);
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
        assert_eq!(app.apply_portrait(shape, faces).unwrap(), Applied::Added);
        for (p, what) in [(jaw, "the jaw"), (mouth, "the mouth corner")] {
            assert!(changed_at(&app, p[0] as u32, p[1] as u32), "{what} moved");
        }
        assert!(!changed_at(&app, 0, 0), "the corner stays");
        let layer = &app.docs.documents[0].canvas.layer_stack.layers[1];
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

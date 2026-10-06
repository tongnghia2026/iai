//! Changing clothes on an ID photo: the "Áo" box of Auto retouch's Ảnh thẻ
//! side. A garment is a layer of one of the shop's own sheets, taken as it
//! is; `core::garment` lays it on the person.
//!
//! The garment is dragged from its sheet onto the box (or taken with the
//! box's button) and kept there for the photos that follow. The photo it is
//! put on gets its layers over those it had, which are hidden: "Người"
//! (the person, old clothes gone), "Áo" (the garment, whole, to move or
//! transform by hand), "Viền áo" (the shade the two cast on each other,
//! multiplied in) and "Tóc trên áo" when hair falls over it. What the
//! models read of the person is kept, so another garment is laid at once.
//!
//! "Viền áo" is made from the outlines of the layers around it
//! (`core::seam`), so it is made again when they change: by itself once the
//! garment has been moved or transformed, as part of that step, and when
//! asked ("Khớp viền áo") after the person was worked on by hand.
//!
//! "Sáng áo" and "Đều sáng áo" of Chỉnh chân dung relight the garment's
//! layer. What it was before they did is kept here ([`Relit`]), so the
//! sliders can be moved again without the garment wearing out.
//!
//! The box also opens the shop's sheets ("Mở file áo") and goes back to the
//! one a garment came from ("Đổi áo khác"). A garment taken there goes on
//! the photo looked at last; the sheets' files are remembered in prefs.json
//! (key `garment_sheets`), so they are never mistaken for that photo.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};

use super::file_ops::normalized_path_key;
use super::render::CanvasEvent;
use super::state::App;
use crate::core::blend::BlendMode;
use crate::core::canvas::Canvas;
use crate::core::command::LayerStructureCommand;
use crate::core::document::DocumentId;
use crate::core::garment::{Collar, Dressed, Fitting, Garment};
use crate::core::portrait::PortraitSettings;
use crate::core::tile::TileMap;
use crate::tools::ToolId;

pub const PERSON_LAYER: &str = "Người";
pub const GARMENT_LAYER: &str = "Áo";
pub const HAIR_LAYER: &str = "Tóc trên áo";
pub const SEAM_LAYER: &str = "Viền áo";
const DRESS_STEP: &str = "Mặc áo";
const SEAM_STEP: &str = "Khớp viền áo";
const UNDRESS_STEP: &str = "Bỏ áo";
/// The longer side of the garment's picture in the box.
const THUMB: u32 = 128;
/// A photo worked on has this many layers at most; a sheet of garments the
/// app has yet to see one taken from has more.
const MOST_PHOTO_LAYERS: usize = 16;
const SHEETS_KEY: &str = "garment_sheets";
/// How many of the shop's sheets are remembered.
const MOST_SHEETS: usize = 12;
const PICK_HINT: &str =
    "Bấm vào cái áo khách chọn rồi bấm Lấy áo đang chọn, hoặc kéo áo thả vào ô này";

/// The files garments came from, the last one first, each with the key an
/// open document of it is known by.
#[derive(Default)]
struct SheetFiles(Vec<(String, PathBuf)>);

impl SheetFiles {
    fn load() -> Self {
        let files: Vec<PathBuf> = crate::ui::dialogs::load_pref(SHEETS_KEY).unwrap_or_default();
        Self(
            files
                .into_iter()
                .map(|file| (normalized_path_key(&file), file))
                .collect(),
        )
    }

    fn files(&self) -> impl Iterator<Item = &PathBuf> {
        self.0.iter().map(|(_, file)| file)
    }

    fn holds(&self, path: &Path) -> bool {
        let key = normalized_path_key(path);
        self.0.iter().any(|(known, _)| *known == key)
    }

    /// Put `paths` first, the first of them foremost.
    fn remember(&mut self, paths: &[PathBuf]) {
        let before: Vec<PathBuf> = self.files().cloned().collect();
        for path in paths.iter().rev() {
            let key = normalized_path_key(path);
            self.0.retain(|(known, _)| *known != key);
            self.0.insert(0, (key, path.clone()));
        }
        self.0.truncate(MOST_SHEETS);
        let files: Vec<&PathBuf> = self.files().collect();
        if !files.iter().copied().eq(before.iter()) {
            crate::ui::dialogs::save_pref(SHEETS_KEY, &files);
        }
    }
}

/// The garment in the box.
struct Picked {
    garment: Arc<Garment>,
    collar: Collar,
    name: String,
    thumb: egui::TextureHandle,
}

/// What dressing put in a document: its layers, and the layers they stand
/// for, hidden.
#[derive(Clone)]
struct Worn {
    doc_id: DocumentId,
    person: u32,
    garment: u32,
    seam: Option<u32>,
    hair: Option<u32>,
    hidden: Vec<u32>,
}

impl Worn {
    fn layers(&self) -> Vec<u32> {
        [Some(self.person), Some(self.garment), self.seam, self.hair]
            .into_iter()
            .flatten()
            .collect()
    }
}

/// A garment's layer as its shade was made for it: where it lies and what
/// it holds.
#[derive(Clone, Copy, PartialEq)]
struct Lying {
    layer: u32,
    offset: (i32, i32),
    tiles: u64,
}

/// A garment's layer before "Sáng áo" and "Đều sáng áo" changed it, with the
/// sliders that made what it holds now (`made`, its tiles' content hash). A
/// layer worked on since (moved, scaled, painted) is no longer that: it is
/// relit from what it has become.
pub(in crate::app) struct Relit {
    pub base: TileMap,
    pub look: (f32, f32),
    pub made: u64,
}

struct Job {
    doc_id: DocumentId,
    /// The history's revision when the person was read.
    revision: u64,
    retouch: Option<Option<PortraitSettings>>,
    progress: Arc<Mutex<String>>,
    rx: Receiver<Result<(Arc<Fitting>, Dressed), String>>,
}

/// A layer of a sheet let go over the box, to be taken on the next frame:
/// by then the Move tool has put its move in the sheet's history.
struct Dropped {
    doc_id: DocumentId,
    undo_count: usize,
    clean: bool,
}

#[derive(Default)]
pub struct GarmentSession {
    picked: Option<Picked>,
    /// The person as the models read them, of this document at this size.
    fitting: Option<(DocumentId, (u32, u32), Arc<Fitting>)>,
    worn: Option<Worn>,
    job: Option<Job>,
    dropped: Option<Dropped>,
    /// Where the panel shows the box (window points) and the sliders the
    /// retouch starts over from, while it is on show.
    drop_box: Option<([f32; 4], PortraitSettings)>,
    /// Garments relit, by document and layer.
    relit: HashMap<(DocumentId, u32), Relit>,
    /// The garment each document's shade was last made for (or first seen
    /// with).
    shaded: HashMap<DocumentId, Lying>,
    /// The shade out of sight while its garment is transformed by hand: its
    /// document and layer.
    seam_held: Option<(DocumentId, u32)>,
    /// The documents garments were taken from.
    sheets: Vec<DocumentId>,
    /// The shop's sheets by their files, read from prefs.json when first
    /// asked for.
    sheet_files: std::cell::OnceCell<SheetFiles>,
    status: String,
    error: bool,
}

impl GarmentSession {
    fn set(&mut self, status: impl Into<String>, error: bool) {
        self.status = status.into();
        self.error = error;
    }

    fn sheet_files(&self) -> &SheetFiles {
        self.sheet_files.get_or_init(SheetFiles::load)
    }

    fn remember_sheets(&mut self, paths: &[PathBuf]) {
        self.sheet_files();
        if let Some(files) = self.sheet_files.get_mut() {
            files.remember(paths);
        }
    }
}

/// The panel's view of the box.
#[derive(Clone, Debug, Default)]
pub struct GarmentState {
    pub thumb: Option<egui::TextureId>,
    pub name: String,
    /// The active document wears the garment.
    pub worn: bool,
    pub busy: bool,
    pub status: String,
    pub error: bool,
}

/// The plain colour a photo's person stands on: its visible Background layer
/// when that is one flat opaque colour.
fn backdrop_of(canvas: &Canvas) -> Option<[u8; 3]> {
    let background = canvas
        .layer_stack
        .layers
        .first()
        .filter(|l| l.is_background && l.visible)?;
    let fill = background.flatten_tiles();
    let first = fill.get(0..4).filter(|px| px[3] == 255)?;
    fill.chunks_exact(4)
        .all(|px| px == first)
        .then(|| [first[0], first[1], first[2]])
}

impl App {
    pub(crate) fn garment_state(&self) -> GarmentState {
        let session = &self.shell.garment;
        let active = self.docs.documents.get(self.docs.active_doc_idx);
        let mut state = GarmentState {
            thumb: session.picked.as_ref().map(|p| p.thumb.id()),
            name: session
                .picked
                .as_ref()
                .map(|p| p.name.clone())
                .unwrap_or_default(),
            worn: active.is_some_and(|doc| self.worn_in(doc.id).is_some()),
            busy: session.job.is_some(),
            status: session.status.clone(),
            error: session.error,
        };
        if let Some(job) = &session.job {
            if let Ok(line) = job.progress.lock() {
                state.status = line.clone();
            }
        }
        state
    }

    pub(crate) fn garment_busy(&self) -> bool {
        self.shell.garment.job.is_some()
    }

    /// Where the panel shows the box this frame, if it does.
    pub(crate) fn set_garment_box(&mut self, shown: Option<([f32; 4], PortraitSettings)>) {
        self.shell.garment.drop_box = shown;
    }

    /// The layer of the garment document `doc_id` wears, if it wears one.
    pub(in crate::app) fn garment_layer(&self, doc_id: DocumentId) -> Option<u32> {
        self.worn_in(doc_id).map(|worn| worn.garment)
    }

    /// In a photo that wears a garment, the place in the layer stack of
    /// what shows of the person: the topmost visible layer from theirs up
    /// to the garment's (their own layer, or the last retouch of it).
    pub(in crate::app) fn dressed_person_shown(&self, doc_id: DocumentId) -> Option<usize> {
        let worn = self.worn_in(doc_id)?;
        let doc = self.docs.documents.iter().find(|d| d.id == doc_id)?;
        let layers = &doc.canvas.layer_stack.layers;
        let person = layers.iter().position(|l| l.id == worn.person)?;
        let garment = layers.iter().position(|l| l.id == worn.garment)?;
        (person..garment)
            .rev()
            .find(|&at| layers[at].visible && layers[at].is_raster())
    }

    /// What the garment in `layer` of `doc_id` was before it was relit, if
    /// its layer still holds what the relight made.
    pub(in crate::app) fn garment_relit(&self, doc_id: DocumentId, layer: u32) -> Option<&Relit> {
        let made = self
            .docs
            .documents
            .iter()
            .find(|d| d.id == doc_id)?
            .canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == layer)?
            .tiles
            .content_hash();
        self.shell
            .garment
            .relit
            .get(&(doc_id, layer))
            .filter(|relit| relit.made == made)
    }

    /// Keep what a garment was before its relight, or forget it (`None`:
    /// the layer is as laid again).
    pub(in crate::app) fn set_garment_relit(
        &mut self,
        doc_id: DocumentId,
        layer: u32,
        relit: Option<Relit>,
    ) {
        match relit {
            Some(relit) => self.shell.garment.relit.insert((doc_id, layer), relit),
            None => self.shell.garment.relit.remove(&(doc_id, layer)),
        };
    }

    /// Document `doc_id` is closing: its garments are no longer kept, and it
    /// is no sheet to go back to.
    pub(crate) fn forget_garment_light(&mut self, doc_id: DocumentId) {
        let session = &mut self.shell.garment;
        session.relit.retain(|(doc, _), _| *doc != doc_id);
        session.shaded.remove(&doc_id);
        session.sheets.retain(|sheet| *sheet != doc_id);
    }

    /// What dressing left in document `doc_id`, while its layers are there.
    /// A photo dressed before this session (saved, opened again) is known
    /// by its layers' names.
    fn worn_in(&self, doc_id: DocumentId) -> Option<Worn> {
        let doc = self.docs.documents.iter().find(|d| d.id == doc_id)?;
        let layers = &doc.canvas.layer_stack.layers;
        let kept = self.shell.garment.worn.as_ref().filter(|worn| {
            worn.doc_id == doc_id
                && worn
                    .layers()
                    .iter()
                    .all(|id| layers.iter().any(|l| l.id == *id))
        });
        if let Some(worn) = kept {
            return Some(worn.clone());
        }
        let garment = layers
            .iter()
            .rposition(|l| l.visible && l.name == GARMENT_LAYER)?;
        // The person's layer is hidden once a retouch of it lies over it:
        // something of the person must show under the garment.
        let person = layers[..garment]
            .iter()
            .rposition(|l| l.name == PERSON_LAYER)?;
        if !layers[person..garment].iter().any(|l| l.visible) {
            return None;
        }
        let over = |name: &str| {
            layers[garment + 1..]
                .iter()
                .find(|l| l.name == name)
                .map(|l| l.id)
        };
        Some(Worn {
            doc_id,
            person: layers[person].id,
            garment: layers[garment].id,
            seam: over(SEAM_LAYER),
            hair: over(HAIR_LAYER),
            hidden: layers[..person]
                .iter()
                .filter(|l| !l.is_background && !l.visible)
                .map(|l| l.id)
                .collect(),
        })
    }

    /// The left button let go: a layer dragged with Move from a sheet and
    /// released over the box is the garment asked for.
    pub(in crate::app) fn note_garment_drop(&mut self) {
        let Some((rect, _)) = self.shell.garment.drop_box else {
            return;
        };
        if !self.edit.input.painting || self.edit.tools.active_id() != ToolId::Move {
            return;
        }
        let scale = self.win.egui_ctx.pixels_per_point().max(0.1);
        let (x, y) = (
            self.edit.input.mouse_x / scale,
            self.edit.input.mouse_y / scale,
        );
        if x < rect[0] || y < rect[1] || x > rect[2] || y > rect[3] {
            return;
        }
        let doc = &self.docs.documents[self.docs.active_doc_idx];
        self.shell.garment.dropped = Some(Dropped {
            doc_id: doc.id,
            undo_count: doc.canvas.undo_count(),
            clean: !doc.canvas.is_dirty(),
        });
    }

    /// Take the active layer of the active document as the garment, then
    /// show the photo it is for (`photo_to_dress`) and put it on. A photo
    /// with no plain backdrop yet is shown and waits for its ID photo.
    pub(crate) fn take_garment(&mut self, settings: Option<PortraitSettings>) {
        let source = self.docs.active_doc_idx;
        let picked = {
            let doc = &self.docs.documents[source];
            let stack = &doc.canvas.layer_stack;
            let Some(layer) = stack.layers.get(stack.active_idx) else {
                return;
            };
            if !layer.is_raster() || layer.is_background {
                self.shell
                    .garment
                    .set("Bấm vào một cái áo trong file áo rồi kéo vào ô này", true);
                return;
            }
            let garment = Garment::trimmed(&layer.flatten_tiles(), layer.width, layer.height);
            let Some((garment, collar)) = garment.and_then(|g| g.collar().map(|c| (g, c))) else {
                self.shell.garment.set(
                    format!(
                        "Lớp \"{}\" không có khoảng hở cổ — không dùng làm áo được",
                        layer.name
                    ),
                    true,
                );
                return;
            };
            let Some(full) =
                image::RgbaImage::from_raw(garment.width, garment.height, garment.rgba.clone())
            else {
                return;
            };
            // A square picture for the box, the garment in the middle of it.
            let longer = garment.width.max(garment.height).max(1);
            let side = |of: u32| (of * THUMB / longer).max(1);
            let small =
                image::imageops::thumbnail(&full, side(garment.width), side(garment.height));
            let mut thumb = image::RgbaImage::new(THUMB, THUMB);
            image::imageops::replace(
                &mut thumb,
                &small,
                ((THUMB - small.width()) / 2) as i64,
                ((THUMB - small.height()) / 2) as i64,
            );
            let picture = egui::ColorImage::from_rgba_unmultiplied(
                [THUMB as usize, THUMB as usize],
                thumb.as_raw(),
            );
            Picked {
                garment: Arc::new(garment),
                collar,
                name: layer.name.clone(),
                thumb: self.win.egui_ctx.load_texture(
                    "garment_in_the_box",
                    picture,
                    egui::TextureOptions::LINEAR,
                ),
            }
        };
        let source_id = self.docs.documents[source].id;
        self.shell.garment.picked = Some(picked);
        self.note_garment_sheet(source);
        let Some(target) = self.photo_to_dress(source_id) else {
            self.shell.garment.set(
                "Đã lấy áo. Mở ảnh rồi bấm Làm ảnh thẻ tự động — áo sẽ được mặc luôn",
                false,
            );
            return;
        };
        self.switch_to_doc(target);
        if self.docs.active_doc_idx != target {
            return;
        }
        if backdrop_of(&self.docs.documents[target].canvas).is_some() {
            self.dress_photo(settings);
        } else {
            self.shell.garment.set(
                "Đã lấy áo. Bấm Làm ảnh thẻ tự động (nền Trắng hoặc Xanh) — áo sẽ được mặc luôn",
                false,
            );
        }
    }

    /// A garment came from the document at `idx`: it is a sheet of garments,
    /// the one to go back to for another. A dressed photo whose own garment
    /// was taken is not.
    fn note_garment_sheet(&mut self, idx: usize) {
        let doc = &self.docs.documents[idx];
        let (id, path) = (doc.id, doc.path.clone());
        if self.worn_in(id).is_some() {
            return;
        }
        let session = &mut self.shell.garment;
        if !session.sheets.contains(&id) {
            session.sheets.push(id);
        }
        if let Some(path) = path {
            session.remember_sheets(&[path]);
        }
    }

    /// Whether the document at `idx` is known as a sheet of garments: one
    /// was taken from it, or its file was opened as one, now or before.
    fn is_garment_sheet(&self, idx: usize) -> bool {
        let doc = &self.docs.documents[idx];
        let session = &self.shell.garment;
        session.sheets.contains(&doc.id)
            || doc
                .path
                .as_deref()
                .is_some_and(|path| session.sheet_files().holds(path))
    }

    /// Whether the document at `idx` may be a photo to put a garment on: it
    /// wears one, or it is one picture in a few layers, none of them a group
    /// (a sheet of prints keeps its prints in groups), and no known sheet.
    fn may_be_dressed(&self, idx: usize) -> bool {
        let doc = &self.docs.documents[idx];
        if self.worn_in(doc.id).is_some() {
            return true;
        }
        let layers = &doc.canvas.layer_stack.layers;
        !self.is_garment_sheet(idx)
            && !doc.is_flow_text()
            && doc.pdf_page.is_none()
            && doc.pages.is_empty()
            && layers.len() <= MOST_PHOTO_LAYERS
            && !layers.iter().any(|l| l.is_group())
    }

    /// The document a garment taken from the sheet `source` is for: the one
    /// looked at last that may be a photo. With several photos open that is
    /// the one opened or shown just before the sheet, not one dressed before.
    fn photo_to_dress(&self, source: DocumentId) -> Option<usize> {
        self.docs
            .doc_mru
            .iter()
            .filter(|id| **id != source)
            .filter_map(|id| self.docs.documents.iter().position(|d| d.id == *id))
            .find(|idx| self.may_be_dressed(*idx))
    }

    /// A sheet is about to show: the Move tool picks a garment there by a
    /// click, and the box says what to do with it.
    fn ready_to_pick_a_garment(&mut self) {
        if !self.is_tool_modal_active() {
            self.edit.tools.select(ToolId::Move);
            self.edit.arrow_multi_layer = None;
        }
        self.shell.garment.set(PICK_HINT, false);
    }

    /// The sheet to go back to for another garment, if one is open: the one
    /// looked at last (a sheet opened with others may never have been).
    fn open_garment_sheet(&self) -> Option<usize> {
        self.docs
            .doc_mru
            .iter()
            .filter_map(|id| self.docs.documents.iter().position(|d| d.id == *id))
            .chain(0..self.docs.documents.len())
            .find(|idx| self.is_garment_sheet(*idx))
    }

    /// "Đổi áo khác": show the sheet the garment came from, to pick another
    /// there. A sheet closed since is opened again; with none remembered the
    /// owner is asked for the file.
    pub(crate) fn change_garment(&mut self) {
        if self.modal_lock_active() {
            self.deny_modal_action();
            return;
        }
        if let Some(sheet) = self.open_garment_sheet() {
            self.ready_to_pick_a_garment();
            self.switch_to_doc(sheet);
            return;
        }
        let session = &self.shell.garment;
        let file = session.sheet_files().files().find(|file| file.is_file());
        match file.cloned() {
            Some(file) => self.open_garment_sheets(vec![file]),
            None => self.pick_garment_sheets(),
        }
    }

    /// "Mở file áo": ask for the shop's sheets, in the folder of the one
    /// used last. The answer comes back through `poll_file_dialog`, to
    /// [`Self::open_garment_sheets`]; until then nothing changes.
    pub(crate) fn pick_garment_sheets(&mut self) {
        if self.locked_beside_retouch() {
            self.deny_modal_action();
            return;
        }
        if self.jobs.pending_file_dialog.is_some() {
            return;
        }
        let Some(window) = self.win.window.as_ref() else {
            return;
        };
        let parent = crate::file_io::dialog_parent(window);
        let session = &self.shell.garment;
        let folder = session
            .sheet_files()
            .files()
            .next()
            .and_then(|file| file.parent())
            .map(Path::to_path_buf);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let picked = crate::file_io::dialog_open_garment_sheets(parent, folder);
            if let Some(paths) = picked.filter(|paths| !paths.is_empty()) {
                let _ = tx.send(crate::file_io::FileDialogResult::OpenedGarmentSheets(paths));
            }
        });
        self.jobs.pending_file_dialog = Some(rx);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Open the shop's sheets `paths`, each in a tab of its own (one open
    /// already is shown), known as sheets of garments from here on. The
    /// photo is left: a retouch previewed on it is applied as it stands, as
    /// when another tab is shown.
    pub(crate) fn open_garment_sheets(&mut self, paths: Vec<PathBuf>) {
        self.yield_portrait();
        self.shell.garment.remember_sheets(&paths);
        self.ready_to_pick_a_garment();
        self.start_load_paths(paths);
    }

    /// The person of the active document as it shows, without its backdrop
    /// and without the layers a last dressing put over it.
    fn person_to_dress(&self) -> Result<(Vec<u8>, [u8; 3]), String> {
        let doc = &self.docs.documents[self.docs.active_doc_idx];
        let canvas = &doc.canvas;
        if canvas.is_cmyk() {
            return Err("chưa hỗ trợ ảnh CMYK".to_string());
        }
        let backdrop = backdrop_of(canvas).ok_or_else(|| {
            "ảnh chưa tách nền — bấm Làm ảnh thẻ tự động với nền Trắng hoặc Xanh trước".to_string()
        })?;
        let over: Vec<u32> = self
            .worn_in(doc.id)
            .map(|worn| {
                [Some(worn.garment), worn.seam, worn.hair]
                    .into_iter()
                    .flatten()
                    .collect()
            })
            .unwrap_or_default();
        let mut above = canvas.layer_stack.clone();
        above.layers[0].visible = false;
        for layer in &mut above.layers {
            if over.contains(&layer.id) {
                layer.visible = false;
            }
        }
        let person = above.flatten(canvas.width, canvas.height);
        if !person.chunks_exact(4).any(|px| px[3] >= 128) {
            return Err("ảnh không có người nào trên nền".to_string());
        }
        Ok((person, backdrop))
    }

    /// Put the garment in the box on the active document. A retouch
    /// previewed there is of the person as they were: it starts over on the
    /// person dressed, from `settings`.
    pub(crate) fn dress_photo(&mut self, settings: Option<PortraitSettings>) {
        if self.shell.garment.job.is_some() {
            return;
        }
        let Some((garment, collar)) = self
            .shell
            .garment
            .picked
            .as_ref()
            .map(|p| (Arc::clone(&p.garment), p.collar))
        else {
            return;
        };
        let idx = self.docs.active_doc_idx;
        let doc_id = self.docs.documents[idx].id;
        let previewed = self
            .shell
            .portrait
            .as_ref()
            .is_some_and(|s| s.doc_id == doc_id);
        if previewed {
            self.cancel_portrait();
        }
        let retouch = (previewed || self.retouch_is_due()).then_some(settings);
        let (person, backdrop) = match self.person_to_dress() {
            Ok(read) => read,
            Err(e) => {
                self.shell
                    .garment
                    .set(format!("Chưa mặc áo được: {e}"), true);
                if let Some(settings) = retouch {
                    self.retouch_after(settings);
                }
                return;
            }
        };
        let canvas = &self.docs.documents[idx].canvas;
        let size = (canvas.width, canvas.height);
        let kept = self
            .shell
            .garment
            .fitting
            .as_ref()
            .filter(|(doc, of, _)| *doc == doc_id && *of == size)
            .map(|(_, _, fitting)| Arc::clone(fitting));
        if let Some(fitting) = kept {
            let dressed = fitting.dress(&person, &garment, &collar);
            self.apply_dressed(doc_id, dressed, retouch);
            return;
        }
        let progress = Arc::new(Mutex::new("Đang chuẩn bị mặc áo…".to_string()));
        let (tx, rx) = mpsc::channel();
        {
            let progress = Arc::clone(&progress);
            let prefer_gpu = crate::core::ai::ort_ep::prefer_gpu();
            std::thread::spawn(move || {
                let report = |line: String| {
                    if let Ok(mut slot) = progress.lock() {
                        *slot = line;
                    }
                };
                let read = Fitting::read(&person, size.0, size.1, backdrop, prefer_gpu, &report);
                let done = read.map(|fitting| {
                    report("Đang mặc áo…".to_string());
                    let dressed = fitting.dress(&person, &garment, &collar);
                    (Arc::new(fitting), dressed)
                });
                let _ = tx.send(done);
            });
        }
        self.shell.garment.job = Some(Job {
            doc_id,
            revision: canvas.history_revision(),
            retouch,
            progress,
            rx,
        });
        self.shell.garment.set("Đang chuẩn bị mặc áo…", false);
    }

    /// Every frame: take a garment dropped on the box, put on the garment a
    /// worker has laid, and keep the shade with a garment that moved.
    pub(crate) fn poll_garment(&mut self) {
        self.follow_garment();
        if let Some(dropped) = self.shell.garment.dropped.take() {
            let idx = self.docs.active_doc_idx;
            if self.docs.documents[idx].id == dropped.doc_id {
                // The drag moved the garment on its sheet: it goes back.
                if self.docs.documents[idx].canvas.undo_count() > dropped.undo_count {
                    self.sync_brush_gpu_to_cpu();
                    let canvas = &mut self.docs.documents[idx].canvas;
                    canvas.undo();
                    if dropped.clean {
                        canvas.mark_saved();
                    }
                    self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
                    self.apply_canvas_event(CanvasEvent::SelectionChanged);
                }
                let settings = self.shell.garment.drop_box.map(|(_, settings)| settings);
                self.take_garment(settings);
            }
        }
        let Some(job) = self.shell.garment.job.as_ref() else {
            return;
        };
        let result = match job.rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => {
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
                return;
            }
            Err(TryRecvError::Disconnected) => Err("xử lý dừng bất thường".to_string()),
        };
        let job = self.shell.garment.job.take().expect("job checked above");
        let unchanged = self
            .docs
            .documents
            .iter()
            .find(|d| d.id == job.doc_id)
            .is_some_and(|d| d.canvas.history_revision() == job.revision);
        match result {
            Ok(_) if !unchanged => {
                self.shell
                    .garment
                    .set("Ảnh đã đổi trong lúc mặc áo — kéo áo vào lại", true);
            }
            Ok((fitting, dressed)) => {
                let size = (fitting.figure.width, fitting.figure.height);
                self.shell.garment.fitting = Some((job.doc_id, size, fitting));
                self.apply_dressed(job.doc_id, dressed, job.retouch);
            }
            Err(e) => {
                self.shell
                    .garment
                    .set(format!("Chưa mặc áo được: {e}"), true);
                if let Some(settings) = job.retouch {
                    self.retouch_after(settings);
                }
            }
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Lay the dressed photo's layers over those it had, as one undo step:
    /// the layers of a last dressing go, what shows of the person is hidden
    /// (its pixels are in "Người" now).
    fn apply_dressed(
        &mut self,
        doc_id: DocumentId,
        dressed: Dressed,
        retouch: Option<Option<PortraitSettings>>,
    ) {
        let Some(idx) = self.docs.documents.iter().position(|d| d.id == doc_id) else {
            return;
        };
        let (old, mut hidden) = match self.worn_in(doc_id) {
            Some(worn) => (worn.layers(), worn.hidden.clone()),
            None => (Vec::new(), Vec::new()),
        };
        let canvas = &mut self.docs.documents[idx].canvas;
        let (before, steps) = (canvas.history_revision(), canvas.undo_count());
        let (width, height) = (canvas.width, canvas.height);
        let mut cmd =
            LayerStructureCommand::capture_before(DRESS_STEP, &canvas.layer_stack, width, height);
        let stack = &mut canvas.layer_stack;
        stack.layers.retain(|l| !old.contains(&l.id));
        for layer in &mut stack.layers {
            layer.selected = false;
            if layer.visible && !layer.is_background {
                layer.visible = false;
                hidden.push(layer.id);
            }
        }
        stack.active_idx = stack.layers.len().saturating_sub(1);
        let mut add = |name: &str, rgba: &[u8], w: u32, h: u32, offset: (i32, i32)| {
            // add_layer inserts above the active layer and makes it active.
            let at = stack.add_layer(w, h);
            let layer = &mut stack.layers[at];
            layer.name = name.to_string();
            layer.parent_id = None;
            layer.tiles = TileMap::from_rgba(rgba, w, h);
            layer.offset = offset;
            layer.id
        };
        let person = add(PERSON_LAYER, &dressed.person, width, height, (0, 0));
        let piece = &dressed.garment;
        let garment = add(
            GARMENT_LAYER,
            &piece.rgba,
            piece.width,
            piece.height,
            piece.offset,
        );
        let seam = dressed
            .shade
            .as_deref()
            .map(|rgba| add(SEAM_LAYER, rgba, width, height, (0, 0)));
        let hair = dressed
            .hair
            .as_deref()
            .map(|rgba| add(HAIR_LAYER, rgba, width, height, (0, 0)));
        if let Some(layer) = stack.layers.iter_mut().find(|l| Some(l.id) == seam) {
            layer.blend_mode = BlendMode::Multiply;
        }
        // The person stays the layer worked on: the retouch is theirs.
        if let Some(at) = stack.layers.iter().position(|l| l.id == person) {
            stack.active_idx = at;
            stack.layers[at].selected = true;
        }
        cmd.capture_after(&canvas.layer_stack, width, height);
        canvas.record(Box::new(cmd));
        canvas.layer_revision += 1;
        let (after, taken) = (
            canvas.history_revision(),
            canvas.undo_count().saturating_sub(steps),
        );
        self.shell.garment.worn = Some(Worn {
            doc_id,
            person,
            garment,
            seam,
            hair,
            hidden,
        });
        // The shade is this garment's as it lies.
        self.shell.garment.shaded.remove(&doc_id);
        self.id_photo_took(doc_id, before, taken, after);
        if idx == self.docs.active_doc_idx {
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        }
        let name = self
            .shell
            .garment
            .picked
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_default();
        self.shell.garment.set(
            format!("Đã mặc áo \"{name}\". Áo là layer riêng: bấm Chỉnh áo để dời, phóng, xoay"),
            false,
        );
        self.shell.status_msg = "Đã mặc áo (Ctrl+Z để hoàn tác)".to_string();
        if let Some(settings) = retouch {
            self.retouch_after(settings);
        }
    }

    /// Empty the box, and take the garment off the active document: its
    /// layers go and the layers they stood for show again.
    pub(crate) fn remove_garment(&mut self, settings: Option<PortraitSettings>) {
        self.shell.garment.picked = None;
        self.shell.garment.set("", false);
        let idx = self.docs.active_doc_idx;
        let doc_id = self.docs.documents[idx].id;
        let Some((ours, hidden)) = self
            .worn_in(doc_id)
            .map(|worn| (worn.layers(), worn.hidden.clone()))
        else {
            return;
        };
        let previewed = self
            .shell
            .portrait
            .as_ref()
            .is_some_and(|s| s.doc_id == doc_id);
        if previewed {
            self.cancel_portrait();
        }
        let canvas = &mut self.docs.documents[idx].canvas;
        let (before, steps) = (canvas.history_revision(), canvas.undo_count());
        let (width, height) = (canvas.width, canvas.height);
        let mut cmd =
            LayerStructureCommand::capture_before(UNDRESS_STEP, &canvas.layer_stack, width, height);
        let stack = &mut canvas.layer_stack;
        // What was made of the dressed person since (a retouch applied) is
        // of a person that is no longer there: it is hidden.
        let person = stack
            .layers
            .iter()
            .position(|l| ours.first() == Some(&l.id));
        for layer in stack
            .layers
            .iter_mut()
            .skip(person.map_or(usize::MAX, |at| at + 1))
        {
            if !ours.contains(&layer.id) {
                layer.visible = false;
            }
        }
        stack.layers.retain(|l| !ours.contains(&l.id));
        let mut top = None;
        for (at, layer) in stack.layers.iter_mut().enumerate() {
            layer.selected = false;
            if hidden.contains(&layer.id) {
                layer.visible = true;
                top = Some(at);
            }
        }
        stack.active_idx = top.unwrap_or(stack.layers.len().saturating_sub(1));
        if let Some(layer) = stack.layers.get_mut(stack.active_idx) {
            layer.selected = true;
        }
        cmd.capture_after(&canvas.layer_stack, width, height);
        canvas.record(Box::new(cmd));
        canvas.layer_revision += 1;
        let (after, taken) = (
            canvas.history_revision(),
            canvas.undo_count().saturating_sub(steps),
        );
        self.shell.garment.worn = None;
        self.id_photo_took(doc_id, before, taken, after);
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.shell.status_msg = "Đã bỏ áo (Ctrl+Z để hoàn tác)".to_string();
        if previewed {
            self.retouch_after(settings);
        }
    }

    /// How the garment of document `idx` lies now, if it wears one.
    fn garment_lying(&self, idx: usize) -> Option<(Worn, Lying)> {
        let doc = self.docs.documents.get(idx)?;
        let worn = self.worn_in(doc.id)?;
        let layer = doc
            .canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == worn.garment)?;
        let lying = Lying {
            layer: layer.id,
            offset: layer.offset,
            tiles: layer.tiles.revision_fingerprint(),
        };
        Some((worn, lying))
    }

    /// Make the shade between the garment and the person of document `idx`
    /// again, from its layers as they are now: "Viền áo", over the garment.
    /// `follows` makes it part of the step recorded last (the garment's move
    /// it answers); otherwise it is a step of its own. Whether the photo
    /// changed.
    fn fit_seam(&mut self, idx: usize, follows: bool) -> bool {
        let Some((worn, lying)) = self.garment_lying(idx) else {
            return false;
        };
        let doc_id = worn.doc_id;
        self.shell.garment.shaded.insert(doc_id, lying);
        let canvas = &self.docs.documents[idx].canvas;
        let (width, height) = (canvas.width, canvas.height);
        let stack = &canvas.layer_stack;
        let Some(under) = stack.layers.iter().position(|l| l.id == worn.garment) else {
            return false;
        };
        // What shows of the person under the garment, the garment, and the
        // hair over it, each alone.
        let alone = |keep: &dyn Fn(usize, u32) -> bool| {
            let mut part = stack.clone();
            for (at, layer) in part.layers.iter_mut().enumerate() {
                layer.visible &= !layer.is_background && keep(at, layer.id);
            }
            part.flatten(width, height)
        };
        let person = alone(&|at, _| at < under);
        let garment = alone(&|_, id| id == worn.garment);
        let hair = worn.hair.map(|hair| alone(&|_, id| id == hair));
        let shade = crate::core::seam::shade(&person, &garment, hair.as_deref(), width, height)
            .map(|rgba| TileMap::from_rgba(&rgba, width, height));
        let had = worn
            .seam
            .and_then(|id| stack.layers.iter().find(|l| l.id == id));
        let same = match (&shade, had) {
            (None, None) => true,
            (Some(made), Some(had)) => {
                had.offset == (0, 0) && had.tiles.content_hash() == made.content_hash()
            }
            _ => false,
        };
        if same {
            return false;
        }

        let canvas = &mut self.docs.documents[idx].canvas;
        let mut cmd =
            LayerStructureCommand::capture_before(SEAM_STEP, &canvas.layer_stack, width, height);
        let stack = &mut canvas.layer_stack;
        let worked_on = stack.layers.get(stack.active_idx).map(|l| l.id);
        let chosen: Vec<u32> = stack
            .layers
            .iter()
            .filter(|l| l.selected)
            .map(|l| l.id)
            .collect();
        // A layer the owner dimmed or hid stays so.
        let old = worn
            .seam
            .and_then(|id| stack.layers.iter().position(|l| l.id == id))
            .map(|at| stack.layers.remove(at));
        let mut seam = None;
        if let (Some(tiles), Some(under)) = (
            shade,
            stack.layers.iter().position(|l| l.id == worn.garment),
        ) {
            stack.active_idx = under;
            let at = stack.add_layer(width, height);
            let layer = &mut stack.layers[at];
            layer.name = SEAM_LAYER.to_string();
            layer.tiles = tiles;
            layer.blend_mode = BlendMode::Multiply;
            if let Some(old) = &old {
                (layer.opacity, layer.visible) = (old.opacity, old.visible);
            }
            seam = Some(layer.id);
        }
        // The layer worked on stays the one worked on.
        let stays = |id: u32| {
            if Some(id) == worn.seam {
                seam
            } else {
                Some(id)
            }
        };
        let worked_on = worked_on.and_then(stays);
        let chosen: Vec<u32> = chosen.into_iter().filter_map(stays).collect();
        for layer in &mut stack.layers {
            layer.selected = chosen.contains(&layer.id);
        }
        stack.active_idx = stack
            .layers
            .iter()
            .position(|l| Some(l.id) == worked_on)
            .unwrap_or(stack.layers.len().saturating_sub(1));
        cmd.capture_after(&canvas.layer_stack, width, height);
        if follows {
            canvas.record_onto_last(Box::new(cmd));
        } else {
            canvas.record(Box::new(cmd));
        }
        canvas.layer_revision += 1;
        if let Some(kept) = self
            .shell
            .garment
            .worn
            .as_mut()
            .filter(|kept| kept.doc_id == doc_id)
        {
            kept.seam = seam;
        }
        if idx == self.docs.active_doc_idx {
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        }
        true
    }

    /// "Khớp viền áo": the person or the garment of the active document was
    /// worked on by hand, and the shade between them is made again.
    pub(crate) fn fit_garment_seam(&mut self) {
        let idx = self.docs.active_doc_idx;
        let doc_id = self.docs.documents[idx].id;
        if self.shell.garment.job.is_some() || self.worn_in(doc_id).is_none() {
            return;
        }
        self.sync_brush_gpu_to_cpu();
        let (line, status) = if self.fit_seam(idx, false) {
            (
                "Đã khớp lại viền áo với da và tóc như ảnh đang có",
                "Đã khớp viền áo (Ctrl+Z để hoàn tác)",
            )
        } else {
            ("Viền áo đã khớp với ảnh đang có", "Viền áo đã khớp")
        };
        self.shell.garment.set(line, false);
        self.shell.status_msg = status.to_string();
    }

    /// Every frame: a garment moved, scaled, turned or relit since its shade
    /// was made takes the shade along, as part of the step that did it. A
    /// photo with no "Viền áo" layer has none to take along.
    fn follow_garment(&mut self) {
        self.show_seam_again();
        if self.shell.garment.job.is_some() || self.work_under_way() {
            return;
        }
        let idx = self.docs.active_doc_idx;
        let Some((worn, lying)) = self.garment_lying(idx) else {
            return;
        };
        let shaded = &mut self.shell.garment.shaded;
        let Some(last) = shaded.get(&worn.doc_id).copied() else {
            // First seen: its shade is taken as made for it.
            shaded.insert(worn.doc_id, lying);
            return;
        };
        // A step undone is put back whole by redoing it: nothing is laid over
        // what could still be redone.
        let canvas = &self.docs.documents[idx].canvas;
        if last == lying || worn.seam.is_none() || canvas.can_redo() {
            return;
        }
        self.fit_seam(idx, canvas.can_undo());
    }

    /// The shade put out of sight for a transform of its garment shows
    /// again once the transform is over.
    fn show_seam_again(&mut self) {
        let Some((doc_id, layer)) = self.shell.garment.seam_held else {
            return;
        };
        if self.edit.transform_state.is_some() || self.edit.pending_transform_commit.is_some() {
            return;
        }
        self.shell.garment.seam_held = None;
        let Some(idx) = self.docs.documents.iter().position(|d| d.id == doc_id) else {
            return;
        };
        let canvas = &mut self.docs.documents[idx].canvas;
        let Some(seam) = canvas.layer_stack.layers.iter_mut().find(|l| l.id == layer) else {
            return;
        };
        seam.visible = true;
        canvas.layer_revision += 1;
        if idx == self.docs.active_doc_idx {
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        }
    }

    /// Hand the garment over to Free Transform: move, scale and turn it.
    pub(crate) fn adjust_garment(&mut self) {
        let idx = self.docs.active_doc_idx;
        let doc_id = self.docs.documents[idx].id;
        let Some(worn) = self.worn_in(doc_id) else {
            return;
        };
        let garment = worn.garment;
        // What a retouch under way shows of the garment's light is the
        // garment's from here on: it is that garment which is transformed.
        self.settle_garment_light();
        let stack = &mut self.docs.documents[idx].canvas.layer_stack;
        let Some(at) = stack.layers.iter().position(|l| l.id == garment) else {
            return;
        };
        for layer in &mut stack.layers {
            layer.selected = false;
            // The shade is of the garment as it lay: it would stay behind
            // while the garment moves, so it is out of sight until then.
            if Some(layer.id) == worn.seam && layer.visible {
                layer.visible = false;
                self.shell.garment.seam_held = Some((doc_id, layer.id));
            }
        }
        stack.active_idx = at;
        stack.layers[at].selected = true;
        self.docs.documents[idx].canvas.layer_revision += 1;
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.begin_transform();
    }

    /// An ID photo was made (or made again) in `doc_id`: what was read of
    /// the person there and what they wore are of the photo before. The
    /// garment in the box goes on the new one.
    pub(crate) fn garment_after_id_photo(
        &mut self,
        doc_id: DocumentId,
        settings: Option<PortraitSettings>,
    ) {
        let session = &mut self.shell.garment;
        if session
            .fitting
            .as_ref()
            .is_some_and(|(doc, ..)| *doc == doc_id)
        {
            session.fitting = None;
        }
        if session
            .worn
            .as_ref()
            .is_some_and(|worn| worn.doc_id == doc_id)
        {
            session.worn = None;
        }
        let active = self.docs.documents[self.docs.active_doc_idx].id == doc_id;
        if active && self.shell.garment.picked.is_some() {
            self.dress_photo(settings);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::garment::{neck_of, Figure};
    use crate::core::id_photo::FaceMarks;

    const BACKDROP: [u8; 3] = [0, 144, 255];
    const SHIRT: [u8; 4] = [240, 240, 250, 255];

    /// A photo 400x600 on a blue backdrop: a head, a neck 80 wide down to
    /// row 360 and shoulders under it in a red top, on "Layer 1"; with it,
    /// what the models would read of that person.
    fn photo() -> (Canvas, Fitting) {
        let (w, h) = (400usize, 600usize);
        let mut matte = vec![0u8; w * h];
        let mut skin = vec![0u8; w * h];
        let mut other = vec![0u8; w * h];
        let mut person = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let from_axis = (x as i32 - 200).abs();
                let i = y * w + x;
                let bare = ((100..300).contains(&y) && from_axis < 70)
                    || ((300..360).contains(&y) && from_axis < 40);
                let dressed = y >= 360 && from_axis < 40 + (y as i32 - 360) * 3;
                if bare {
                    (matte[i], skin[i]) = (255, 255);
                    person[i * 4..i * 4 + 4].copy_from_slice(&[220, 170, 140, 255]);
                } else if dressed {
                    (matte[i], other[i]) = (255, 255);
                    person[i * 4..i * 4 + 4].copy_from_slice(&[200, 0, 0, 255]);
                }
            }
        }
        let mut canvas = Canvas::new(w as u32, h as u32);
        let background = &mut canvas.layer_stack.layers[0];
        background.is_background = true;
        background.tiles = TileMap::new_solid(
            w as u32,
            h as u32,
            BACKDROP[0],
            BACKDROP[1],
            BACKDROP[2],
            255,
        );
        let at = canvas.layer_stack.add_layer(w as u32, h as u32);
        let layer = &mut canvas.layer_stack.layers[at];
        layer.name = "Layer 1".to_string();
        layer.tiles = TileMap::from_rgba(&person, w as u32, h as u32);
        let figure = Figure {
            width: w as u32,
            height: h as u32,
            matte,
            hair: vec![0u8; w * h],
            skin,
            other,
        };
        let face = FaceMarks {
            eyes: [200.0, 190.0],
            chin: [200.0, 300.0],
            cheeks: [200.0, 220.0],
            nose: [200.0, 230.0],
            tilt: 0.0,
            width: 140.0,
        };
        let neck = neck_of(&figure, &face);
        (canvas, Fitting { figure, face, neck })
    }

    /// A sheet 600x400 on green with one shirt on it ("Layer 23", the active
    /// layer): shoulders from its row 60, a collar rising to row 10 either
    /// side of an opening that dips to row 50.
    fn sheet() -> Canvas {
        let (w, h) = (200usize, 120usize);
        let mut shirt = vec![0u8; w * h * 4];
        for x in 0..w {
            let from_middle = (x as i32 - 100).unsigned_abs() as usize;
            let top = if from_middle <= 20 {
                50 - from_middle * 2
            } else {
                (10 + (from_middle - 20)).min(60)
            };
            for y in top..h {
                shirt[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&SHIRT);
            }
        }
        let mut canvas = Canvas::new(600, 400);
        let background = &mut canvas.layer_stack.layers[0];
        background.is_background = true;
        background.tiles = TileMap::new_solid(600, 400, 170, 210, 140, 255);
        let at = canvas.layer_stack.add_layer(w as u32, h as u32);
        let layer = &mut canvas.layer_stack.layers[at];
        layer.name = "Layer 23".to_string();
        layer.tiles = TileMap::from_rgba(&shirt, w as u32, h as u32);
        layer.offset = (50, 60);
        canvas
    }

    /// The photo in the first tab, read already; the sheet in the second,
    /// which is the one looked at.
    fn shop() -> App {
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        let (canvas, fitting) = photo();
        app.docs.documents[0].canvas = canvas;
        let photo_id = app.docs.documents[0].id;
        app.open_new_doc_tab();
        app.docs.documents[1].canvas = sheet();
        app.shell.garment.fitting = Some((photo_id, (400, 600), Arc::new(fitting)));
        app
    }

    fn names(app: &App, doc: usize) -> Vec<(String, bool)> {
        app.docs.documents[doc]
            .canvas
            .layer_stack
            .layers
            .iter()
            .map(|l| (l.name.clone(), l.visible))
            .collect()
    }

    fn shown(app: &App, doc: usize, x: usize, y: usize) -> [u8; 3] {
        let canvas = &app.docs.documents[doc].canvas;
        let flat = canvas.flatten_for_export();
        let i = (y * canvas.width as usize + x) * 4;
        [flat[i], flat[i + 1], flat[i + 2]]
    }

    fn on(name: &str) -> (String, bool) {
        (name.to_string(), true)
    }

    fn off(name: &str) -> (String, bool) {
        (name.to_string(), false)
    }

    #[test]
    fn a_garment_taken_from_the_sheet_goes_on_the_photo_as_layers_of_its_own() {
        let mut app = shop();
        let undo_before = app.docs.documents[0].canvas.undo_count();
        app.take_garment(None);

        // The photo is looked at again, dressed.
        assert_eq!(app.docs.active_doc_idx, 0);
        let state = app.garment_state();
        assert!(!state.error, "{}", state.status);
        assert!(state.thumb.is_some() && state.worn);
        assert_eq!(
            names(&app, 0),
            vec![
                on("Background"),
                off("Layer 1"),
                on(PERSON_LAYER),
                on(GARMENT_LAYER),
                on(SEAM_LAYER)
            ]
        );
        let stack = &app.docs.documents[0].canvas.layer_stack;
        assert_eq!(stack.layers[stack.active_idx].name, PERSON_LAYER);
        // The shirt covers the shoulder; beside it the old top is gone and
        // the backdrop shows; the face is as it was.
        assert_eq!(shown(&app, 0, 280, 520), [SHIRT[0], SHIRT[1], SHIRT[2]]);
        assert_eq!(shown(&app, 0, 399, 590), BACKDROP);
        assert_eq!(shown(&app, 0, 200, 200), [220, 170, 140]);
        // The garment layer is the whole garment, far larger than what the
        // collar's opening shows of the person.
        let garment = &app.docs.documents[0].canvas.layer_stack.layers[3];
        assert!(
            garment.width > 380 && garment.height > 230,
            "{}x{}",
            garment.width,
            garment.height
        );

        // One step, undone as one.
        assert_eq!(app.docs.documents[0].canvas.undo_count(), undo_before + 1);
        app.docs.documents[0].canvas.undo();
        assert_eq!(names(&app, 0), vec![on("Background"), on("Layer 1")]);
        // The sheet is as it was.
        assert_eq!(app.docs.documents[1].canvas.layer_stack.layers.len(), 2);
    }

    #[test]
    fn another_garment_takes_the_place_of_the_one_worn_and_none_gives_the_photo_back() {
        let mut app = shop();
        app.take_garment(None);
        // Back on the sheet, the shirt is taken again.
        app.switch_to_doc(1);
        app.take_garment(None);
        assert_eq!(
            names(&app, 0),
            vec![
                on("Background"),
                off("Layer 1"),
                on(PERSON_LAYER),
                on(GARMENT_LAYER),
                on(SEAM_LAYER)
            ]
        );
        assert_eq!(shown(&app, 0, 280, 520), [SHIRT[0], SHIRT[1], SHIRT[2]]);

        app.remove_garment(None);
        assert_eq!(names(&app, 0), vec![on("Background"), on("Layer 1")]);
        assert_eq!(shown(&app, 0, 280, 520), [200, 0, 0]);
        let state = app.garment_state();
        assert!(state.thumb.is_none() && !state.worn);
    }

    #[test]
    fn a_photo_dressed_in_another_session_is_known_by_its_layers() {
        let mut app = shop();
        app.take_garment(None);
        // As after saving the photo and opening it again: nothing is
        // remembered but what the document holds.
        app.shell.garment = GarmentSession::default();
        assert!(app.garment_state().worn);
        app.adjust_garment();
        let stack = &app.docs.documents[0].canvas.layer_stack;
        assert_eq!(stack.layers[stack.active_idx].name, GARMENT_LAYER);
        app.cancel_transform();
        app.remove_garment(None);
        assert_eq!(names(&app, 0), vec![on("Background"), on("Layer 1")]);
    }

    #[test]
    fn a_dressed_photo_is_known_with_its_person_under_a_retouch() {
        let mut app = shop();
        app.take_garment(None);
        app.shell.garment = GarmentSession::default();
        // A retouch applied: its layer shows over the person's, now hidden.
        let canvas = &mut app.docs.documents[0].canvas;
        let person = canvas
            .layer_stack
            .layers
            .iter()
            .position(|l| l.name == PERSON_LAYER)
            .unwrap();
        canvas.layer_stack.active_idx = person;
        let retouch = canvas.layer_stack.add_layer(400, 600);
        canvas.layer_stack.layers[retouch].name = "Chân dung".to_string();
        canvas.layer_stack.layers[person].visible = false;
        let doc_id = app.docs.documents[0].id;
        assert!(app.garment_state().worn);
        // What shows of the person is that retouch, whatever is active.
        assert_eq!(app.dressed_person_shown(doc_id), Some(retouch));
        // With nothing of the person showing, the photo is not dressed.
        app.docs.documents[0].canvas.layer_stack.layers[retouch].visible = false;
        assert!(!app.garment_state().worn);
        assert_eq!(app.dressed_person_shown(doc_id), None);
    }

    #[test]
    fn a_layer_with_no_collar_is_no_garment() {
        let mut app = shop();
        // The sheet's backdrop, then a plain block.
        app.docs.documents[1].canvas.layer_stack.active_idx = 0;
        app.take_garment(None);
        assert!(app.garment_state().error);
        assert!(app.garment_state().thumb.is_none());
        let canvas = &mut app.docs.documents[1].canvas;
        let at = canvas.layer_stack.add_layer(60, 40);
        canvas.layer_stack.layers[at].tiles = TileMap::new_solid(60, 40, 9, 9, 9, 255);
        app.take_garment(None);
        assert!(app.garment_state().error);
        assert_eq!(app.docs.active_doc_idx, 1, "nothing was put on the photo");
        assert_eq!(app.docs.documents[0].canvas.layer_stack.layers.len(), 2);
    }

    #[test]
    fn a_garment_dragged_onto_the_box_is_taken_and_the_sheet_put_back() {
        let mut app = shop();
        app.set_garment_box(Some(([20.0, 30.0, 300.0, 90.0], PortraitSettings::NEUTRAL)));
        app.edit.tools.select(ToolId::Move);
        let scale = app.win.egui_ctx.pixels_per_point();
        // Let go beside the box: nothing is asked.
        app.edit.input.painting = true;
        (app.edit.input.mouse_x, app.edit.input.mouse_y) = (400.0 * scale, 60.0 * scale);
        app.note_garment_drop();
        app.poll_garment();
        assert!(app.garment_state().thumb.is_none());

        // Let go over it: the Move tool then records where the drag left
        // the shirt on its sheet.
        (app.edit.input.mouse_x, app.edit.input.mouse_y) = (100.0 * scale, 60.0 * scale);
        app.note_garment_drop();
        let canvas = &mut app.docs.documents[1].canvas;
        let undo_before = canvas.undo_count();
        let mut moved =
            LayerStructureCommand::capture_before("Move", &canvas.layer_stack, 600, 400);
        canvas.layer_stack.layers[1].offset = (300, 200);
        moved.capture_after(&canvas.layer_stack, 600, 400);
        canvas.record(Box::new(moved));
        app.edit.input.painting = false;

        app.poll_garment();
        let sheet = &app.docs.documents[1].canvas;
        assert_eq!(sheet.layer_stack.layers[1].offset, (50, 60));
        assert_eq!(sheet.undo_count(), undo_before);
        assert!(!sheet.is_dirty());
        assert_eq!(app.docs.active_doc_idx, 0);
        assert!(app.garment_state().worn);
        assert_eq!(shown(&app, 0, 280, 520), [SHIRT[0], SHIRT[1], SHIRT[2]]);
    }

    /// With a garment in the box, the photo made is dressed before the
    /// retouch starts on it, and dressed again when it is framed again.
    /// Runs the models, when they and the retouch tests' portrait are here.
    #[test]
    fn a_photo_made_with_a_garment_in_the_box_wears_it() {
        use crate::core::id_photo::{IdPhotoOptions, IdPhotoRequest, Nudge};
        use crate::core::select_subject::{SelectSubjectEngine, SelectSubjectModel};

        let path =
            std::path::Path::new("tmp/model-sources/gfpgan/inputs/whole_imgs/Blake_Lively.jpg");
        if !path.is_file()
            || crate::core::ai::face_mesh::model_path().is_none()
            || crate::core::ai::body_parts::model_path().is_none()
            || !SelectSubjectEngine::model_path_for(SelectSubjectModel::BiRefNetTiny).is_file()
        {
            return;
        }
        let image = image::open(path).unwrap().to_rgba8();
        let (w, h) = image.dimensions();
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        app.docs.documents[0].canvas = Canvas::from_rgba(image.into_raw(), w, h);
        app.shell.ui.show_portrait_dialog = true;
        app.open_new_doc_tab();
        app.docs.documents[1].canvas = sheet();
        // The photo is not cut out yet: it is shown, and the garment waits
        // in the box for its ID photo.
        app.take_garment(None);
        assert_eq!(app.docs.active_doc_idx, 0);
        let state = app.garment_state();
        assert!(state.thumb.is_some() && !state.error, "{}", state.status);
        assert!(!state.worn && !state.busy);

        let mut asked = IdPhotoRequest {
            options: IdPhotoOptions::default(),
            nudge: Nudge::default(),
            settings: Some(PortraitSettings::NEUTRAL),
        };
        let settled = |app: &mut App, what: &str| {
            let started = std::time::Instant::now();
            while app.id_photo_state().busy || app.garment_busy() || app.shell.portrait.is_none() {
                assert!(started.elapsed().as_secs() < 300, "timed out: {what}");
                std::thread::sleep(std::time::Duration::from_millis(20));
                app.poll_id_photo();
                app.poll_garment();
                app.poll_portrait();
            }
        };
        app.run_id_photo(asked);
        settled(&mut app, "the photo made, dressed and its retouch begun");
        let state = app.garment_state();
        assert!(state.worn && !state.error, "{}", state.status);
        let layers = names(&app, 0);
        assert!(
            layers.contains(&on(PERSON_LAYER)) && layers.contains(&on(GARMENT_LAYER)),
            "{layers:?}"
        );
        // The retouch is of the person dressed, and the photo can still be
        // framed again.
        let stack = &app.docs.documents[0].canvas.layer_stack;
        let retouched = app.shell.portrait.as_ref().unwrap().layer_id;
        assert_eq!(
            stack
                .layers
                .iter()
                .find(|l| l.id == retouched)
                .unwrap()
                .name,
            PERSON_LAYER
        );
        assert!(app.id_photo_state().made);

        asked.nudge.down = 0.02;
        app.run_id_photo(asked);
        settled(&mut app, "the photo framed again and dressed again");
        let layers = names(&app, 0);
        assert_eq!(
            layers
                .iter()
                .filter(|(name, _)| name == GARMENT_LAYER)
                .count(),
            1,
            "{layers:?}"
        );
        assert!(app.garment_state().worn);
        assert!(app.id_photo_state().made);
    }

    /// The "Viền áo" layer of the photo: its place, and what it holds.
    fn seam(app: &App) -> Option<(usize, u64)> {
        let layers = &app.docs.documents[0].canvas.layer_stack.layers;
        let at = layers.iter().position(|l| l.name == SEAM_LAYER)?;
        Some((at, layers[at].tiles.content_hash()))
    }

    /// How much of the photo's pixel the "Viền áo" layer shades, 0..255.
    fn shade_at(app: &App, x: u32, y: u32) -> u8 {
        let layers = &app.docs.documents[0].canvas.layer_stack.layers;
        let layer = layers.iter().find(|l| l.name == SEAM_LAYER).unwrap();
        layer.flatten_tiles()[((y * 400 + x) * 4 + 3) as usize]
    }

    /// The garment's layer moved as the Move tool leaves it: a step.
    fn move_garment(app: &mut App, by: (i32, i32)) {
        let canvas = &mut app.docs.documents[0].canvas;
        let mut moved =
            LayerStructureCommand::capture_before("Move", &canvas.layer_stack, 400, 600);
        let layer = canvas
            .layer_stack
            .layers
            .iter_mut()
            .find(|l| l.name == GARMENT_LAYER)
            .unwrap();
        layer.offset = (layer.offset.0 + by.0, layer.offset.1 + by.1);
        moved.capture_after(&canvas.layer_stack, 400, 600);
        canvas.record(Box::new(moved));
    }

    #[test]
    fn the_garment_and_the_person_shade_each_other_in_a_layer_over_the_garment() {
        let mut app = shop();
        app.take_garment(None);
        let (at, _) = seam(&app).expect("a shade layer");
        let layers = &app.docs.documents[0].canvas.layer_stack.layers;
        assert_eq!(layers[at - 1].name, GARMENT_LAYER);
        assert_eq!(layers[at].blend_mode, BlendMode::Multiply);
        // The collar's opening holds skin from row 360 down, 20 either side
        // of the middle at row 400: beside the garment's edge it is shaded,
        // and the garment beside the skin too; the face and the shoulder far
        // from the neck are not.
        assert!(
            shade_at(&app, 181, 400) > 90,
            "{}",
            shade_at(&app, 181, 400)
        );
        assert!(
            shade_at(&app, 176, 400) > 20,
            "{}",
            shade_at(&app, 176, 400)
        );
        assert_eq!(shade_at(&app, 200, 200), 0);
        assert_eq!(shade_at(&app, 280, 520), 0);
        // It darkens what is under it, and nothing where it holds nothing.
        let with = shown(&app, 0, 181, 400);
        app.docs.documents[0].canvas.layer_stack.layers[at].visible = false;
        let without = shown(&app, 0, 181, 400);
        assert!(with[0] + 10 < without[0], "{with:?} {without:?}");
        assert_eq!(shown(&app, 0, 200, 200), [220, 170, 140]);
    }

    #[test]
    fn a_garment_moved_takes_its_shade_along_in_the_same_step() {
        let mut app = shop();
        app.take_garment(None);
        app.poll_garment();
        let (_, laid) = seam(&app).unwrap();
        let beside = shade_at(&app, 181, 400);
        let steps = app.docs.documents[0].canvas.undo_count();

        move_garment(&mut app, (0, 30));
        // Not while the move is still under way.
        app.edit.input.painting = true;
        app.poll_garment();
        assert_eq!(seam(&app).unwrap().1, laid);
        app.edit.input.painting = false;
        app.poll_garment();
        let (at, moved) = seam(&app).unwrap();
        assert_ne!(moved, laid);
        // The garment's edge is 30 lower now, and the shade with it.
        assert!(
            shade_at(&app, 181, 400) < 30,
            "{}",
            shade_at(&app, 181, 400)
        );
        assert!(shade_at(&app, 181, 430).abs_diff(beside) < 10);
        let canvas = &app.docs.documents[0].canvas;
        assert_eq!(canvas.layer_stack.layers[at - 1].name, GARMENT_LAYER);
        assert_eq!(canvas.undo_count(), steps + 1);
        assert_eq!(canvas.history_entries().last().unwrap().label, "Move");

        // One undo takes the move and its shade back; nothing is laid over
        // the step while it can be redone, and redoing it gives both again.
        app.docs.documents[0].canvas.undo();
        assert_eq!(seam(&app).unwrap().1, laid);
        app.poll_garment();
        let canvas = &mut app.docs.documents[0].canvas;
        assert_eq!((canvas.undo_count(), canvas.redo_count()), (steps, 1));
        canvas.redo();
        assert_eq!(seam(&app).unwrap().1, moved);
        app.poll_garment();
        assert_eq!(app.docs.documents[0].canvas.undo_count(), steps + 1);
    }

    #[test]
    fn the_shade_is_made_again_when_asked_after_the_person_was_worked_on() {
        let mut app = shop();
        app.take_garment(None);
        assert!(shade_at(&app, 184, 400) > 20);
        // The skin beside the garment's edge is rubbed out by hand.
        let canvas = &mut app.docs.documents[0].canvas;
        let person = canvas
            .layer_stack
            .layers
            .iter_mut()
            .find(|l| l.name == PERSON_LAYER)
            .unwrap();
        let mut rgba = person.flatten_tiles();
        for y in 390..410 {
            for x in 180..190 {
                rgba[(y * 400 + x) * 4 + 3] = 0;
            }
        }
        person.tiles = TileMap::from_rgba(&rgba, 400, 600);
        let steps = canvas.undo_count();
        // Nothing follows that by itself: the shade is asked for.
        app.poll_garment();
        assert!(shade_at(&app, 184, 400) > 20);

        app.fit_garment_seam();
        assert_eq!(shade_at(&app, 184, 400), 0);
        let canvas = &app.docs.documents[0].canvas;
        assert_eq!(canvas.undo_count(), steps + 1);
        assert_eq!(canvas.history_entries().last().unwrap().label, SEAM_STEP);
        let stack = &canvas.layer_stack;
        assert_eq!(stack.layers[stack.active_idx].name, PERSON_LAYER);
        // Asked again with nothing changed, it is no step.
        app.fit_garment_seam();
        assert_eq!(app.docs.documents[0].canvas.undo_count(), steps + 1);
        assert!(app.garment_state().status.contains("đã khớp"));
    }

    #[test]
    fn a_photo_whose_shade_was_thrown_away_gets_one_only_when_asked() {
        let mut app = shop();
        app.take_garment(None);
        app.poll_garment();
        let (at, _) = seam(&app).unwrap();
        app.docs.documents[0].canvas.layer_stack.layers.remove(at);
        move_garment(&mut app, (0, 30));
        app.poll_garment();
        assert!(seam(&app).is_none());
        assert!(app.garment_state().worn);

        app.fit_garment_seam();
        let (at, _) = seam(&app).unwrap();
        let layers = &app.docs.documents[0].canvas.layer_stack.layers;
        assert_eq!(layers[at - 1].name, GARMENT_LAYER);
        assert!(shade_at(&app, 181, 430) > 90);
        // A layer dimmed by hand stays dimmed when it is made again.
        app.docs.documents[0].canvas.layer_stack.layers[at].opacity = 0.5;
        move_garment(&mut app, (0, -30));
        app.poll_garment();
        let (at, _) = seam(&app).unwrap();
        let layer = &app.docs.documents[0].canvas.layer_stack.layers[at];
        assert_eq!(layer.opacity, 0.5);
        assert!(shade_at(&app, 181, 400) > 90);
        // Taking the garment off takes the shade with it.
        app.remove_garment(None);
        assert_eq!(names(&app, 0), vec![on("Background"), on("Layer 1")]);
    }

    #[test]
    fn the_garment_worn_is_handed_to_free_transform() {
        let mut app = shop();
        app.take_garment(None);
        app.adjust_garment();
        let stack = &app.docs.documents[0].canvas.layer_stack;
        assert_eq!(stack.layers[stack.active_idx].name, GARMENT_LAYER);
        assert!(app.edit.transform_state.is_some());
        // Its shade is out of sight while it is moved, and back after.
        let (at, laid) = seam(&app).unwrap();
        assert!(!stack.layers[at].visible);
        app.poll_garment();
        assert!(!app.docs.documents[0].canvas.layer_stack.layers[at].visible);
        app.cancel_transform();
        app.poll_garment();
        assert!(app.docs.documents[0].canvas.layer_stack.layers[at].visible);
        assert_eq!(seam(&app), Some((at, laid)));
    }

    /// One more tab, looked at, holding `canvas`.
    fn open_tab(app: &mut App, canvas: Canvas) -> usize {
        app.open_new_doc_tab();
        let idx = app.docs.active_doc_idx;
        app.docs.documents[idx].canvas = canvas;
        idx
    }

    #[test]
    fn a_garment_goes_on_the_photo_looked_at_last_not_on_one_dressed_before() {
        let mut app = shop();
        app.take_garment(None);
        let first = names(&app, 0);
        // A second customer's photo is opened, then the sheet shown again.
        let (canvas, fitting) = photo();
        let second = open_tab(&mut app, canvas);
        let id = app.docs.documents[second].id;
        app.shell.garment.fitting = Some((id, (400, 600), Arc::new(fitting)));
        app.switch_to_doc(1);
        app.take_garment(None);

        assert_eq!(app.docs.active_doc_idx, second);
        let state = app.garment_state();
        assert!(state.worn && !state.error, "{}", state.status);
        assert_eq!(
            names(&app, second),
            vec![
                on("Background"),
                off("Layer 1"),
                on(PERSON_LAYER),
                on(GARMENT_LAYER),
                on(SEAM_LAYER)
            ]
        );
        assert_eq!(names(&app, 0), first, "the photo done before is left alone");

        // Shown again before the sheet, the first photo is the one dressed.
        let first_id = app.docs.documents[0].id;
        app.shell.garment.fitting = Some((first_id, (400, 600), Arc::new(photo().1)));
        app.switch_to_doc(0);
        app.switch_to_doc(1);
        app.take_garment(None);
        assert_eq!(app.docs.active_doc_idx, 0);
        assert!(!app.garment_busy());
    }

    #[test]
    fn a_photo_with_no_plain_backdrop_yet_is_shown_and_the_garment_waits_for_it() {
        let mut app = shop();
        // As shot: one layer, no two rows of it alike.
        let shot: Vec<u8> = (0..400 * 600)
            .flat_map(|i| [(i / 400 % 256) as u8, 120, 90, 255])
            .collect();
        let raw = open_tab(&mut app, Canvas::from_rgba(shot, 400, 600));
        let layers = names(&app, raw);
        app.switch_to_doc(1);
        app.take_garment(None);

        assert_eq!(app.docs.active_doc_idx, raw);
        let state = app.garment_state();
        assert!(state.thumb.is_some() && !state.worn, "{}", state.status);
        assert!(!state.busy && !state.error, "{}", state.status);
        assert!(state.status.contains("Làm ảnh thẻ tự động"));
        assert_eq!(names(&app, raw), layers);
        assert_eq!(names(&app, 0), vec![on("Background"), on("Layer 1")]);
    }

    #[test]
    fn sheets_of_garments_and_of_prints_are_passed_over_for_the_photo() {
        let mut app = shop();
        // A sheet kept as a file garments came from before.
        let known = open_tab(&mut app, sheet());
        let file = PathBuf::from("kho-ao/vest-nu.psd");
        app.docs.documents[known].path = Some(file.clone());
        app.shell.garment.remember_sheets(&[file]);
        // One never used, which only its many layers tell from a photo.
        let mut many = sheet();
        for _ in 0..MOST_PHOTO_LAYERS {
            many.layer_stack.add_layer(20, 20);
        }
        open_tab(&mut app, many);
        // A sheet of prints: its prints are in a group.
        let mut prints = sheet();
        prints
            .layer_stack
            .layers
            .push(crate::core::layer::Layer::new_group(90, "3×4", 600, 400));
        open_tab(&mut app, prints);

        app.switch_to_doc(1);
        app.take_garment(None);
        assert_eq!(app.docs.active_doc_idx, 0);
        let state = app.garment_state();
        assert!(state.worn && !state.error, "{}", state.status);
    }

    #[test]
    fn another_garment_is_picked_on_the_sheet_the_last_one_came_from() {
        let mut app = shop();
        app.take_garment(None);
        app.edit.tools.select(ToolId::Brush);
        app.change_garment();
        // The sheet shows, with the tool that picks a garment by a click.
        assert_eq!(app.docs.active_doc_idx, 1);
        assert_eq!(app.edit.tools.active_id(), ToolId::Move);
        let state = app.garment_state();
        assert!(
            !state.error && state.status == PICK_HINT,
            "{}",
            state.status
        );
        // Taken there, the garment goes on the photo it was asked from.
        app.take_garment(None);
        assert_eq!(app.docs.active_doc_idx, 0);
        assert!(app.garment_state().worn);

        // The sheet closed and no file of it kept: its file is asked for
        // (there is no window here to ask in), the photo stays.
        app.close_doc_confirmed(1);
        app.change_garment();
        assert_eq!(app.docs.documents.len(), 1);
        assert!(app.jobs.pending_file_dialog.is_none());
        assert!(app.garment_state().worn);
    }

    #[test]
    fn a_dressed_photo_whose_own_garment_is_taken_is_no_sheet() {
        let mut app = shop();
        app.take_garment(None);
        // "Lấy áo đang chọn" pressed on the photo, its garment layer picked.
        let stack = &mut app.docs.documents[0].canvas.layer_stack;
        stack.active_idx = stack
            .layers
            .iter()
            .position(|l| l.name == GARMENT_LAYER)
            .unwrap();
        app.take_garment(None);
        assert!(!app.is_garment_sheet(0) && app.is_garment_sheet(1));
        assert_eq!(app.docs.active_doc_idx, 0);
        assert!(app.may_be_dressed(0) && !app.may_be_dressed(1));
    }

    #[test]
    fn the_files_of_sheets_are_kept_latest_first() {
        let file = |n: usize| PathBuf::from(format!("kho-ao/mau-{n}.psd"));
        let mut files = SheetFiles::default();
        files.remember(&[file(1), file(2)]);
        files.remember(&[file(3)]);
        files.remember(&[file(2)]);
        let kept: Vec<PathBuf> = files.files().cloned().collect();
        assert_eq!(kept, vec![file(2), file(3), file(1)]);
        assert!(files.holds(&file(3)) && !files.holds(&file(9)));
        for n in 10..40 {
            files.remember(&[file(n)]);
        }
        assert_eq!(files.files().count(), MOST_SHEETS);
        assert_eq!(files.files().next(), Some(&file(39)));
    }

    /// A sheet's file chosen from the box is shown (here it is open
    /// already), is known as a sheet from then on, and is the one "Đổi áo
    /// khác" goes back to: its tab, or its file once that tab is closed.
    #[test]
    fn a_sheet_asked_for_from_the_box_is_shown_and_known_as_one() {
        let dir = std::env::temp_dir().join(format!("iai-garment-sheet-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("ao.png");
        image::RgbaImage::from_pixel(64, 48, image::Rgba([240, 240, 250, 255]))
            .save(&file)
            .unwrap();
        let mut app = shop();
        let opened = open_tab(&mut app, sheet());
        app.docs.documents[opened].path = Some(file.clone());
        app.docs.documents[opened].file_modified_at =
            crate::core::document::file_modified_at(&file);
        app.switch_to_doc(0);
        assert!(!app.is_garment_sheet(opened));

        app.edit.tools.select(ToolId::Brush);
        app.open_garment_sheets(vec![file.clone()]);
        assert_eq!(app.docs.active_doc_idx, opened);
        assert!(app.is_garment_sheet(opened) && !app.may_be_dressed(opened));
        assert_eq!(app.edit.tools.active_id(), ToolId::Move);
        assert_eq!(app.garment_state().status, PICK_HINT);

        // From the photo, the sheet looked at last is the one gone back to.
        app.switch_to_doc(0);
        app.change_garment();
        assert_eq!(app.docs.active_doc_idx, opened);

        // Its tab closed, its file is opened again.
        app.close_doc_confirmed(opened);
        app.switch_to_doc(0);
        assert!(app.jobs.pending_loads.is_empty());
        app.change_garment();
        assert_eq!(app.jobs.pending_loads.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

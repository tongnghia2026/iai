//! "Tô vùng" in Chỉnh chân dung: the Refine Brush paints one face's skin,
//! hair, brow, clothes or neck mask (`core::portrait::brush`), with undo inside
//! the dialog and a tinted overlay of the mask being painted. After each
//! stroke the preview re-renders from the edited masks: hair at once, brows
//! once their layers are rebuilt (quick, a small region), skin once its
//! dependent layers are rebuilt on a worker, clothes once the light across
//! them is read again. Brow takes precedence over skin, so a brow stroke
//! rebuilds the skin too. The clothes and the neck are found only when asked
//! for: until they are, the brush has nothing of theirs to paint.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;

use super::state::App;
use crate::core::portrait::brush::{MaskPaint, MaskTarget};
use crate::core::portrait::recipe::{RestoredFace, SavedMask};
use crate::core::portrait::{ClothesArea, FaceEdits, PortraitModel, Region, SkinLayers};
use crate::core::refine::{MaskBrushEvent, Rect, StampOp};
use crate::tools::ToolId;

/// Largest side of the overlay texture.
const MAX_OVERLAY_TEX: usize = 4096;
const OVERLAY_ALPHA: f32 = 0.55;
/// In-dialog undo keeps at most this many bytes of mask patches.
const UNDO_BUDGET: usize = 512 << 20;

struct Patch {
    face: usize,
    target: MaskTarget,
    rect: Rect,
    before: Vec<u8>,
    after: Vec<u8>,
}

struct Stroke {
    op: StampOp,
    /// The face the stroke paints, from its first dab; `None` until then or
    /// when it started outside every face.
    face: Option<usize>,
    started: bool,
    before: Vec<u8>,
    rect: Option<Rect>,
}

/// The tinted mask over the canvas while painting.
pub struct PortraitOverlay {
    pub target: MaskTarget,
    /// Area covered, in image (layer) pixels.
    pub rect: Region,
    size: [usize; 2],
    pub tex: egui::TextureHandle,
}

#[derive(Default)]
pub struct PortraitBrush {
    pub target: Option<MaskTarget>,
    prev_tool: Option<ToolId>,
    paints: HashMap<(usize, MaskTarget), MaskPaint>,
    stroke: Option<Stroke>,
    undo: Vec<Patch>,
    redo: Vec<Patch>,
    /// Skin layers being rebuilt: face, generation, result.
    skin_jobs: Vec<(usize, u64, Receiver<SkinLayers>)>,
    skin_gen: HashMap<usize, u64>,
    /// Clothes masks a reopened layer kept, per face, to lay over the
    /// clothes once those are found.
    kept_clothes: HashMap<usize, SavedMask>,
    /// Neck masks a reopened layer kept, per face: the preview uses them at
    /// once, the brush paints on from them once the neck is found.
    kept_neck: HashMap<usize, Vec<u8>>,
    pub overlay: Option<PortraitOverlay>,
}

impl PortraitBrush {
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn busy(&self) -> bool {
        !self.skin_jobs.is_empty()
    }

    /// Whether clothes are wanted before the sliders ask: to paint them, or
    /// to lay a kept mask over them.
    pub fn wants_clothes(&self) -> bool {
        self.target == Some(MaskTarget::Clothes) || !self.kept_clothes.is_empty()
    }

    /// Whether the neck is wanted before its slider asks: to paint it.
    pub fn wants_neck(&self) -> bool {
        self.target == Some(MaskTarget::Neck)
    }
}

/// The clothes of face `face` as found, once they are.
fn found_clothes(model: &PortraitModel, face: usize) -> Option<&ClothesArea> {
    model.faces[face]
        .clothes_area
        .get()
        .and_then(|found| found.as_ref().ok())
}

/// Whether the neck of face `face` has been found.
fn neck_found(model: &PortraitModel, face: usize) -> bool {
    matches!(model.faces[face].neck.get(), Some(Ok(_)))
}

const NOWHERE: Region = Region {
    x: 0,
    y: 0,
    w: 0,
    h: 0,
};

fn region_of(model: &PortraitModel, face: usize, target: MaskTarget) -> Region {
    let f = &model.faces[face];
    match target {
        MaskTarget::Skin => f.skin.region(),
        // The neck is painted over the skin's region, once it is found.
        MaskTarget::Neck if neck_found(model, face) => f.skin.region(),
        MaskTarget::Neck => NOWHERE,
        MaskTarget::Hair => f.hair_region,
        MaskTarget::Brows => f.brow_layers().region(),
        MaskTarget::Clothes => found_clothes(model, face).map_or(NOWHERE, |clothes| clothes.region),
    }
}

/// A face's mask as painted, else as analysed. (Every edit comes from a
/// paint; skin is the mask before an edited brow takes its share. The neck
/// is always a paint: `neck_paint` makes one of what was found.)
fn painted_mask<'a>(
    model: &'a PortraitModel,
    paints: &'a HashMap<(usize, MaskTarget), MaskPaint>,
    face: usize,
    target: MaskTarget,
) -> &'a [u8] {
    if let Some(p) = paints.get(&(face, target)) {
        return &p.mask;
    }
    let f = &model.faces[face];
    match target {
        MaskTarget::Skin => f.skin.mask(),
        MaskTarget::Hair => f.hair_mask(),
        MaskTarget::Brows => f.brow_layers().area(),
        MaskTarget::Clothes => found_clothes(model, face).map_or(&[], |clothes| clothes.mask()),
        MaskTarget::Neck => &[],
    }
}

/// The face whose `target` region holds image point (x, y), nearest first in
/// face sizes.
fn face_at(model: &PortraitModel, target: MaskTarget, x: f32, y: f32) -> Option<usize> {
    model
        .faces
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            let r = region_of(model, *i, target);
            !r.is_empty()
                && x >= r.x as f32
                && y >= r.y as f32
                && x < (r.x + r.w) as f32
                && y < (r.y + r.h) as f32
        })
        .map(|(i, f)| {
            let (c, extent, _) = f.mesh.frame();
            (i, (x - c[0]).hypot(y - c[1]) / extent.max(1.0))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

impl App {
    /// The dialog's brush is on: canvas clicks paint the chosen mask.
    pub(crate) fn portrait_painting(&self) -> bool {
        self.shell.ui.show_portrait_dialog
            && self
                .shell
                .portrait
                .as_ref()
                .is_some_and(|s| s.brush.target.is_some())
    }

    /// Dialog view of the brush: target, can undo, can redo, and the overlay
    /// (texture, canvas-pixel rect) when its document is the active one.
    #[allow(clippy::type_complexity)]
    pub(crate) fn portrait_brush_view(
        &self,
    ) -> (
        Option<MaskTarget>,
        bool,
        bool,
        Option<(egui::TextureId, egui::Rect)>,
    ) {
        let Some(session) = self
            .shell
            .portrait
            .as_ref()
            .filter(|_| self.shell.ui.show_portrait_dialog)
        else {
            return (None, false, false, None);
        };
        let brush = &session.brush;
        let active = self
            .docs
            .documents
            .get(self.docs.active_doc_idx)
            .is_some_and(|d| d.id == session.doc_id);
        let overlay = brush.overlay.as_ref().filter(|_| active).map(|o| {
            let (ox, oy) = (session.offset.0 as f32, session.offset.1 as f32);
            (
                o.tex.id(),
                egui::Rect::from_min_size(
                    egui::pos2(o.rect.x as f32 + ox, o.rect.y as f32 + oy),
                    egui::vec2(o.rect.w as f32, o.rect.h as f32),
                ),
            )
        });
        (brush.target, brush.can_undo(), brush.can_redo(), overlay)
    }

    /// Pick which mask the brush paints (`None` puts the brush away and
    /// gives back the tool that was active).
    pub(crate) fn set_portrait_brush_target(&mut self, target: Option<MaskTarget>) {
        let Some(session) = self.shell.portrait.as_mut() else {
            return;
        };
        if session.brush.target == target || (target.is_some() && session.model.is_none()) {
            return;
        }
        self.end_portrait_stroke();
        let Some(session) = self.shell.portrait.as_mut() else {
            return;
        };
        let doc_id = session.doc_id;
        session.brush.target = target;
        let queue = target.map(|_| Vec::new());
        let prev = match target {
            Some(_) if session.brush.prev_tool.is_none() => {
                session.brush.prev_tool = Some(self.edit.tools.active_id());
                self.edit.tools.select(ToolId::RefineBrush);
                None
            }
            None => session.brush.prev_tool.take(),
            _ => None,
        };
        if let Some(tool) = prev {
            self.edit.tools.active_on_cancel();
            self.edit.tools.select(tool);
        }
        if let Some(doc) = self.docs.documents.iter_mut().find(|d| d.id == doc_id) {
            doc.canvas.mask_brush = queue;
        }
        self.rebuild_portrait_overlay();
        self.refresh_portrait_preview();
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }

    /// Put the brush away when the dialog closes.
    pub(super) fn end_portrait_brush(&mut self) {
        self.set_portrait_brush_target(None);
    }

    /// Apply the strokes the Refine Brush queued and collect rebuilt skin.
    pub(super) fn poll_portrait_brush(&mut self) {
        let Some(session) = self.shell.portrait.as_mut() else {
            return;
        };
        let doc_id = session.doc_id;
        let events = self
            .docs
            .documents
            .iter_mut()
            .find(|d| d.id == doc_id)
            .and_then(|d| d.canvas.mask_brush.as_mut())
            .map(std::mem::take)
            .unwrap_or_default();
        for event in events {
            match event {
                MaskBrushEvent::Begin(op) => {
                    self.end_portrait_stroke();
                    if let Some(session) = self.shell.portrait.as_mut() {
                        session.brush.stroke = Some(Stroke {
                            op,
                            face: None,
                            started: false,
                            before: Vec::new(),
                            rect: None,
                        });
                    }
                }
                MaskBrushEvent::Dabs {
                    points,
                    radius,
                    hardness,
                } => self.portrait_dabs(&points, radius, hardness),
                MaskBrushEvent::End => self.end_portrait_stroke(),
            }
        }
        self.collect_portrait_skin();
    }

    fn portrait_dabs(&mut self, points: &[(f32, f32)], radius: f32, hardness: f32) {
        let Some(session) = self.shell.portrait.as_mut() else {
            return;
        };
        let (Some(target), Some(model)) = (session.brush.target, session.model.clone()) else {
            return;
        };
        let (ox, oy) = session.offset;
        let brush = &mut session.brush;
        let Some(stroke) = brush.stroke.as_mut() else {
            return;
        };
        let points: Vec<(f32, f32)> = points
            .iter()
            .map(|&(x, y)| (x - ox as f32, y - oy as f32))
            .collect();
        if !stroke.started {
            stroke.started = true;
            stroke.face = points
                .first()
                .and_then(|&(x, y)| face_at(&model, target, x, y));
        }
        let Some(face) = stroke.face else {
            return;
        };
        if target == MaskTarget::Neck && !brush.paints.contains_key(&(face, target)) {
            // Paints are made when the neck is found: none means none found.
            return;
        }
        let paint = brush.paints.entry((face, target)).or_insert_with(|| {
            let now = painted_mask(&model, &HashMap::new(), face, target).to_vec();
            MaskPaint::new(region_of(&model, face, target), now)
        });
        // Brows are a soft shape around sparse hairs, not a colour area:
        // Smart paints them plainly.
        let op = match (target, stroke.op) {
            (MaskTarget::Brows, StampOp::Smart) => StampOp::Add,
            (MaskTarget::Brows, StampOp::SmartOut) => StampOp::Subtract,
            (_, op) => op,
        };
        if stroke.before.is_empty() {
            stroke.before = paint.mask.clone();
            paint.begin_stroke();
        }
        let mut touched: Option<Rect> = None;
        for &(x, y) in &points {
            if let Some(r) = paint.stamp(&session.src, session.w, op, x, y, radius, hardness) {
                touched = Some(touched.map_or(r, |t| t.union(r)));
            }
        }
        let Some(touched) = touched else {
            return;
        };
        stroke.rect = Some(stroke.rect.map_or(touched, |r| r.union(touched)));
        let r = paint.region;
        self.patch_portrait_overlay(Rect {
            x0: r.x as usize + touched.x0,
            y0: r.y as usize + touched.y0,
            x1: r.x as usize + touched.x1,
            y1: r.y as usize + touched.y1,
        });
    }

    /// Close the open stroke as one undo step and use its mask.
    pub(super) fn end_portrait_stroke(&mut self) {
        let Some(session) = self.shell.portrait.as_mut() else {
            return;
        };
        let Some(stroke) = session.brush.stroke.take() else {
            return;
        };
        let (Some(face), Some(target)) = (stroke.face, session.brush.target) else {
            return;
        };
        let Some(paint) = session.brush.paints.get_mut(&(face, target)) else {
            return;
        };
        paint.end_stroke();
        let Some(rect) = stroke.rect else {
            return;
        };
        let w = paint.region.w as usize;
        let before: Vec<u8> = (rect.y0..rect.y1)
            .flat_map(|y| {
                stroke.before[y * w + rect.x0..y * w + rect.x1]
                    .iter()
                    .copied()
            })
            .collect();
        let after = paint.read(rect);
        if before == after {
            return;
        }
        let brush = &mut session.brush;
        brush.redo.clear();
        brush.undo.push(Patch {
            face,
            target,
            rect,
            before,
            after,
        });
        let bytes = |p: &Patch| p.before.len() + p.after.len();
        let mut total: usize = brush.undo.iter().map(bytes).sum();
        while total > UNDO_BUDGET && brush.undo.len() > 1 {
            total -= bytes(&brush.undo.remove(0));
        }
        self.use_portrait_mask(face, target);
    }

    /// Ctrl+Z / Ctrl+Shift+Z in the dialog: step the brush strokes.
    pub(crate) fn portrait_brush_step(&mut self, redo: bool) {
        self.end_portrait_stroke();
        let Some(session) = self.shell.portrait.as_mut() else {
            return;
        };
        let brush = &mut session.brush;
        let Some(patch) = (if redo {
            brush.redo.pop()
        } else {
            brush.undo.pop()
        }) else {
            self.shell.status_msg = if redo {
                "Không còn nét tô để làm lại".to_string()
            } else {
                "Không còn nét tô để hoàn tác".to_string()
            };
            return;
        };
        let (face, target, rect) = (patch.face, patch.target, patch.rect);
        let region = brush.paints.get_mut(&(face, target)).map(|paint| {
            paint.write(rect, if redo { &patch.after } else { &patch.before });
            paint.region
        });
        if redo {
            brush.undo.push(patch);
        } else {
            brush.redo.push(patch);
        }
        let Some(r) = region else {
            return;
        };
        self.use_portrait_mask(face, target);
        if self
            .shell
            .portrait
            .as_ref()
            .is_some_and(|s| s.brush.target == Some(target))
        {
            self.patch_portrait_overlay(Rect {
                x0: r.x as usize + rect.x0,
                y0: r.y as usize + rect.y0,
                x1: r.x as usize + rect.x1,
                y1: r.y as usize + rect.y1,
            });
        }
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }

    /// Take the painted masks a reopened "Chân dung" layer kept: the brush
    /// paints on from them and the preview uses them (they are not undo
    /// steps of this session).
    pub(super) fn restore_portrait_masks(&mut self, restored: Vec<RestoredFace>) {
        let Some(session) = self.shell.portrait.as_mut() else {
            return;
        };
        let Some(model) = session.model.clone() else {
            return;
        };
        let mut used = Vec::new();
        for (face, saved) in restored.into_iter().enumerate() {
            if let Some(clothes) = saved.clothes {
                session.brush.kept_clothes.insert(face, clothes);
            }
            if let Some(neck) = saved.neck {
                let region = region_of(&model, face, MaskTarget::Skin);
                if neck.len() == region.len() {
                    if session.edits.len() < model.faces.len() {
                        session
                            .edits
                            .resize(model.faces.len(), FaceEdits::default());
                    }
                    session.edits[face].neck = Some(Arc::new(neck.clone()));
                    session.edit_rev += 1;
                    session.brush.kept_neck.insert(face, neck);
                }
            }
            for (target, mask) in [
                (MaskTarget::Skin, saved.skin),
                (MaskTarget::Hair, saved.hair),
                (MaskTarget::Brows, saved.brows),
            ] {
                let Some(mask) = mask else {
                    continue;
                };
                let region = region_of(&model, face, target);
                if mask.len() == region.len() {
                    session
                        .brush
                        .paints
                        .insert((face, target), MaskPaint::new(region, mask));
                    used.push((face, target));
                }
            }
        }
        // A brow rebuilds the skin as well.
        let brows: Vec<usize> = used
            .iter()
            .filter(|u| u.1 == MaskTarget::Brows)
            .map(|u| u.0)
            .collect();
        for (face, target) in used {
            if target != MaskTarget::Skin || !brows.contains(&face) {
                self.use_portrait_mask(face, target);
            }
        }
        self.clothes_found();
        self.neck_found();
    }

    /// The necks were found (or may have been): give the brush each one to
    /// paint on from, as a reopened layer kept it or else as found.
    pub(super) fn neck_found(&mut self) {
        let Some(session) = self.shell.portrait.as_mut() else {
            return;
        };
        let Some(model) = session.model.clone() else {
            return;
        };
        let mut made = false;
        for face in 0..model.faces.len() {
            let key = (face, MaskTarget::Neck);
            if session.brush.paints.contains_key(&key) {
                continue;
            }
            let Some(Ok(neck)) = model.faces[face].neck.get() else {
                continue;
            };
            let skin = &model.faces[face].skin;
            let region = skin.region();
            let mask = match session.brush.kept_neck.remove(&face) {
                Some(kept) if kept.len() == region.len() => kept,
                // As the skin stands now, painted or not.
                _ => {
                    let skin = session
                        .edits
                        .get(face)
                        .and_then(|e| e.skin.as_deref())
                        .unwrap_or(skin);
                    neck.mask(skin)
                }
            };
            session
                .brush
                .paints
                .insert(key, MaskPaint::new(region, mask));
            made = true;
        }
        if made && session.brush.target == Some(MaskTarget::Neck) {
            self.rebuild_portrait_overlay();
        }
    }

    /// The clothes were found (or may have been): lay the masks a reopened
    /// layer kept over them, and show them to the brush that waits to paint
    /// them.
    pub(super) fn clothes_found(&mut self) {
        let Some(session) = self.shell.portrait.as_mut() else {
            return;
        };
        let Some(model) = session.model.clone() else {
            return;
        };
        let kept: Vec<usize> = session.brush.kept_clothes.keys().copied().collect();
        let mut used = Vec::new();
        for face in kept {
            if face >= model.faces.len() || model.faces[face].clothes_area.get().is_none() {
                continue;
            }
            let Some(saved) = session.brush.kept_clothes.remove(&face) else {
                continue;
            };
            if let Some(found) = found_clothes(&model, face) {
                let mask = saved.onto(found.region, found.mask());
                session.brush.paints.insert(
                    (face, MaskTarget::Clothes),
                    MaskPaint::new(found.region, mask),
                );
                used.push(face);
            }
        }
        let painting = session.brush.target == Some(MaskTarget::Clothes);
        for face in used {
            self.use_portrait_mask(face, MaskTarget::Clothes);
        }
        if painting {
            self.rebuild_portrait_overlay();
        }
    }

    /// Hand a painted mask to the preview: hair at once, brows once their
    /// layers are rebuilt, skin once its dependent layers are rebuilt on a
    /// worker (after a brow stroke as well).
    fn use_portrait_mask(&mut self, face: usize, target: MaskTarget) {
        let Some(session) = self.shell.portrait.as_mut() else {
            return;
        };
        let (Some(model), Some(paint)) = (
            session.model.clone(),
            session.brush.paints.get(&(face, target)),
        ) else {
            return;
        };
        if session.edits.len() < model.faces.len() {
            session
                .edits
                .resize(model.faces.len(), FaceEdits::default());
        }
        match target {
            MaskTarget::Hair => {
                session.edits[face].hair = Some(Arc::new(paint.mask.clone()));
                session.edit_rev += 1;
                self.refresh_portrait_preview();
            }
            MaskTarget::Brows => {
                let brows = model.brow_layers_from(&session.src, face, &paint.mask);
                session.edits[face].brows = Some(Arc::new(brows));
                session.edit_rev += 1;
                self.refresh_portrait_preview();
                self.rebuild_portrait_skin(face);
            }
            MaskTarget::Skin => self.rebuild_portrait_skin(face),
            MaskTarget::Clothes => {
                let Some(found) = found_clothes(&model, face) else {
                    return;
                };
                let painted = found.with_mask(&session.src, session.w, paint.mask.clone());
                session.edits[face].clothes = Some(Arc::new(painted));
                session.edit_rev += 1;
                self.refresh_portrait_preview();
            }
            MaskTarget::Neck => {
                session.edits[face].neck = Some(Arc::new(paint.mask.clone()));
                session.edit_rev += 1;
                self.refresh_portrait_preview();
            }
        }
    }

    /// Rebuild a face's skin layers on a worker from its skin mask as
    /// painted, with an edited brow taking its share.
    fn rebuild_portrait_skin(&mut self, face: usize) {
        let Some(session) = self.shell.portrait.as_mut() else {
            return;
        };
        let Some(model) = session.model.clone() else {
            return;
        };
        if session.edits.len() < model.faces.len() {
            session
                .edits
                .resize(model.faces.len(), FaceEdits::default());
        }
        let paints = &session.brush.paints;
        let painted = paints.get(&(face, MaskTarget::Skin)).map(|p| &p.mask);
        let skin = painted_mask(&model, paints, face, MaskTarget::Skin);
        let mask = match paints.get(&(face, MaskTarget::Brows)) {
            Some(brows) => model.faces[face].skin_with_brows(skin, &brows.mask),
            None => skin.to_vec(),
        };
        let edit = &mut session.edits[face];
        edit.skin_paint = painted.map(|m| Arc::new(m.clone()));
        let generation = session.brush.skin_gen.entry(face).or_insert(0);
        *generation += 1;
        let generation = *generation;
        // Unchanged (a brow stroke away from the skin): only drop a pending
        // rebuild, which the generation just did.
        let shown = edit
            .skin
            .as_deref()
            .map_or(model.faces[face].skin.mask(), |s| s.mask());
        if shown == &mask[..] {
            return;
        }
        let src = Arc::clone(&session.src);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(model.skin_layers_from(&src, face, &mask));
        });
        session.brush.skin_jobs.push((face, generation, rx));
    }

    fn collect_portrait_skin(&mut self) {
        let Some(session) = self.shell.portrait.as_mut() else {
            return;
        };
        let mut done = false;
        let jobs = std::mem::take(&mut session.brush.skin_jobs);
        for (face, generation, rx) in jobs {
            match rx.try_recv() {
                Ok(layers) => {
                    if session.brush.skin_gen.get(&face) == Some(&generation) {
                        session.edits[face].skin = Some(Arc::new(layers));
                        session.edit_rev += 1;
                        done = true;
                        // A neck not painted yet follows the skin: the
                        // brush is given it anew.
                        if session.edits[face].neck.is_none() {
                            session.brush.paints.remove(&(face, MaskTarget::Neck));
                        }
                    }
                }
                Err(TryRecvError::Empty) => session.brush.skin_jobs.push((face, generation, rx)),
                Err(TryRecvError::Disconnected) => {}
            }
        }
        if done {
            self.neck_found();
            self.refresh_portrait_preview();
        }
    }

    /// Build the overlay of the brush's mask over every face, or drop it.
    fn rebuild_portrait_overlay(&mut self) {
        let Some(session) = self.shell.portrait.as_mut() else {
            return;
        };
        session.brush.overlay = None;
        let (Some(target), Some(model)) = (session.brush.target, session.model.as_ref()) else {
            return;
        };
        let mut bounds: Option<(u32, u32, u32, u32)> = None;
        for i in 0..model.faces.len() {
            let r = region_of(model, i, target);
            if r.is_empty() {
                continue;
            }
            let (x1, y1) = (r.x + r.w, r.y + r.h);
            bounds = Some(match bounds {
                None => (r.x, r.y, x1, y1),
                Some((a, b, c, d)) => (a.min(r.x), b.min(r.y), c.max(x1), d.max(y1)),
            });
        }
        let Some((x0, y0, x1, y1)) = bounds else {
            return;
        };
        let rect = Region {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        };
        let scale = (MAX_OVERLAY_TEX as f32 / rect.w.max(rect.h) as f32).min(1.0);
        let size = [
            ((rect.w as f32 * scale).round() as usize).max(1),
            ((rect.h as f32 * scale).round() as usize).max(1),
        ];
        let image = overlay_pixels(
            model,
            &session.brush.paints,
            target,
            rect,
            size,
            Rect::full(size[0], size[1]),
        );
        let tex =
            self.win
                .egui_ctx
                .load_texture("portrait_overlay", image, egui::TextureOptions::LINEAR);
        session.brush.overlay = Some(PortraitOverlay {
            target,
            rect,
            size,
            tex,
        });
    }

    /// Redraw the overlay over `area` (image pixels).
    fn patch_portrait_overlay(&mut self, area: Rect) {
        let Some(session) = self.shell.portrait.as_mut() else {
            return;
        };
        let Some(model) = session.model.as_ref() else {
            return;
        };
        let Some(overlay) = session.brush.overlay.as_mut() else {
            return;
        };
        let (rect, size) = (overlay.rect, overlay.size);
        let to_tex = |v: usize, origin: u32, extent: u32, tex: usize| {
            ((v.saturating_sub(origin as usize)) * tex) as f32 / extent as f32
        };
        let tex_rect = Rect {
            x0: to_tex(area.x0, rect.x, rect.w, size[0]).floor() as usize,
            y0: to_tex(area.y0, rect.y, rect.h, size[1]).floor() as usize,
            x1: (to_tex(area.x1, rect.x, rect.w, size[0]).ceil() as usize + 1).min(size[0]),
            y1: (to_tex(area.y1, rect.y, rect.h, size[1]).ceil() as usize + 1).min(size[1]),
        };
        if tex_rect.x1 <= tex_rect.x0 || tex_rect.y1 <= tex_rect.y0 {
            return;
        }
        let image = overlay_pixels(
            model,
            &session.brush.paints,
            overlay.target,
            rect,
            size,
            tex_rect,
        );
        overlay.tex.set_partial(
            [tex_rect.x0, tex_rect.y0],
            image,
            egui::TextureOptions::LINEAR,
        );
    }
}

/// A face in the overlay: index, region, mask and, for skin, an edited brow
/// area.
type OverlayFace<'a> = (usize, Region, &'a [u8], Option<&'a [u8]>);

/// Overlay texels in `area` of a `size` texture over image `rect`: the mask
/// of the nearest face's `target` area, tinted like "Hiện vùng nhận diện".
fn overlay_pixels(
    model: &PortraitModel,
    paints: &HashMap<(usize, MaskTarget), MaskPaint>,
    target: MaskTarget,
    rect: Region,
    size: [usize; 2],
    area: Rect,
) -> egui::ColorImage {
    use rayon::prelude::*;
    let colour = match target {
        MaskTarget::Skin => [255, 40, 40],
        MaskTarget::Hair => [150, 60, 255],
        MaskTarget::Brows => [255, 230, 0],
        MaskTarget::Clothes => [0, 150, 130],
        MaskTarget::Neck => [40, 130, 255],
    };
    let lut: Vec<egui::Color32> = (0..256)
        .map(|m| {
            let [r, g, b, a] = super::actions::premultiply_for_linear_target(
                colour,
                m as f32 / 255.0 * OVERLAY_ALPHA,
            );
            egui::Color32::from_rgba_premultiplied(r, g, b, a)
        })
        .collect();
    // Skin as retouched: an edited brow takes its share.
    let faces: Vec<OverlayFace> = (0..model.faces.len())
        .map(|i| {
            let brows = paints
                .get(&(i, MaskTarget::Brows))
                .filter(|_| target == MaskTarget::Skin)
                .map(|p| &p.mask[..]);
            (
                i,
                region_of(model, i, target),
                painted_mask(model, paints, i, target),
                brows,
            )
        })
        .filter(|(_, r, m, _)| !r.is_empty() && m.len() == r.len())
        .collect();
    let (aw, ah) = (area.width(), area.height());
    let mut pixels = vec![egui::Color32::TRANSPARENT; aw * ah];
    pixels
        .par_chunks_mut(aw)
        .enumerate()
        .for_each(|(row, line)| {
            let ty = area.y0 + row;
            let y = rect.y + ((ty as f32 + 0.5) * rect.h as f32 / size[1] as f32) as u32;
            for (col, px) in line.iter_mut().enumerate() {
                let tx = area.x0 + col;
                let x = rect.x + ((tx as f32 + 0.5) * rect.w as f32 / size[0] as f32) as u32;
                let mut m = 0u8;
                for &(i, r, mask, brows) in &faces {
                    if x >= r.x && y >= r.y && x < r.x + r.w && y < r.y + r.h {
                        let mut v = mask[((y - r.y) * r.w + x - r.x) as usize];
                        if let Some(area) = brows {
                            v = model.faces[i].skin_with_brow_at(area, x, y, v);
                        }
                        m = m.max(v);
                    }
                }
                // Only what the retouch reaches: inside the selection, if any.
                if let Some(clip) = &model.clip {
                    m = (m as f32 * clip.at(x, y)).round() as u8;
                }
                *px = lut[m as usize];
            }
        });
    egui::ColorImage {
        size: [aw, ah],
        pixels,
        source_size: egui::Vec2::new(aw as f32, ah as f32),
    }
}

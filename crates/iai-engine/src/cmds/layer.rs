//! Layers of the shown page: add, delete, reorder, change, place a picture.

use iai_canvas::canvas::Canvas;
use iai_history::command::LayerStructureCommand;
use iai_history::gateway::ChangeKind;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{Blend, RectPx, not_enabled, parse_color};
use super::doc::{LayerInfo, layer_infos};
use iai_canvas::canvas::MAX_DIMENSION;

use crate::{
    Apply, CmdError, Command, CommandMeta, Effect, EngineEvent, ErrorCode, Job, Plan, Session,
    Validate, layer_index,
};

/// Run `edit` on `canvas`'s layer stack as one undo step named `label`.
pub(super) fn structural<T>(
    canvas: &mut Canvas,
    label: &str,
    edit: impl FnOnce(&mut Canvas) -> Result<T, CmdError>,
) -> Result<T, CmdError> {
    let (w, h) = (canvas.width, canvas.height);
    let mut cmd = LayerStructureCommand::capture_before(label, &canvas.layer_stack, w, h);
    let out = edit(canvas)?;
    cmd.capture_after(&canvas.layer_stack, canvas.width, canvas.height);
    canvas.record_as(Box::new(cmd), ChangeKind::LayerStructure);
    canvas.layer_revision = canvas.layer_revision.saturating_add(1);
    canvas.flatten_full();
    Ok(out)
}

/// What a layer command answers: the layer, and the stack after it.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LayerChanged {
    pub doc: u32,
    /// The layer made or changed (for `layer.delete`, the one now active).
    pub layer: Option<u32>,
    /// The stack after the change, top first.
    pub layers: Vec<LayerInfo>,
}

pub(super) fn answer(
    s: &mut Session<'_>,
    doc: u32,
    layer: Option<u32>,
) -> Result<LayerChanged, CmdError> {
    let layers = layer_infos(s.canvas(Some(doc))?);
    s.emit(EngineEvent::Structure { doc });
    Ok(LayerChanged { doc, layer, layers })
}

/// The kind of layer `layer.new` adds.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NewKind {
    /// Empty and clear (default).
    #[default]
    Raster,
    /// Filled with `fill`.
    Fill,
    /// A folder; the layers moved into it go with it.
    Group,
    /// An adjustment layer: `adjustment` applied to what is below it, the
    /// pixels left as they are.
    Adjustment,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NewParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    #[schemars(length(min = 1, max = 200))]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: NewKind,
    /// The colour of a `fill` layer, "#RRGGBB" or "#RRGGBBAA".
    pub fill: Option<String>,
    /// Put it right above this layer; above the active layer when omitted.
    pub above: Option<u32>,
    /// What an `adjustment` layer does (as `image.adjust` takes it).
    pub adjustment: Option<super::adjust::Adjustment>,
}

impl Validate for NewParams {
    fn validate(&self) -> Result<(), CmdError> {
        match (&self.kind, &self.adjustment) {
            (NewKind::Adjustment, None) => {
                return Err(CmdError::bad_param(
                    "adjustment",
                    "what the adjustment layer does, for kind = adjustment",
                ));
            }
            (NewKind::Adjustment, Some(_)) | (_, None) => {}
            (_, Some(_)) => {
                return Err(CmdError::bad_param(
                    "adjustment",
                    "only with kind = adjustment",
                ));
            }
        }
        match (&self.kind, &self.fill) {
            (NewKind::Fill, None) => Err(CmdError::bad_param("fill", "a colour for kind = fill")),
            (NewKind::Fill, Some(c)) => parse_color("fill", c).map(|_| ()),
            (_, Some(_)) => Err(CmdError::bad_param("fill", "only with kind = fill")),
            _ => Ok(()),
        }
    }
}

static NEW: CommandMeta = CommandMeta {
    id: "layer.new",
    label_vi: "Layer mới",
    label_en: "New Layer",
    doc: "Add a layer (empty, filled with a colour, a group folder, or an adjustment layer) \
          above the active layer or above `above`. It becomes the active layer, the only one \
          picked.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct NewLayer;

impl Command for NewLayer {
    type Params = NewParams;
    type Output = LayerChanged;
    fn meta(&self) -> &'static CommandMeta {
        &NEW
    }
    fn plan(&self, s: &mut Session<'_>, p: NewParams) -> Result<Plan<LayerChanged>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        if let Some(above) = p.above {
            canvas.layer_stack.active_idx = layer_index(canvas, Some(above))?;
        }
        let fill = p
            .fill
            .as_deref()
            .map(|c| parse_color("fill", c))
            .transpose()?;
        if let (NewKind::Adjustment, Some(adjustment)) = (p.kind, &p.adjustment) {
            // Adjustment layers composite in RGB on the mirrors: in CMYK the
            // inks would no longer be what prints.
            if canvas.is_cmyk() {
                return Err(not_enabled(
                    "no adjustment layer in a CMYK document",
                    "Adjustment layer chưa hỗ trợ ở chế độ CMYK",
                ));
            }
            let adjustment = adjustment.core()?;
            let id = structural(canvas, "Add Adjustment Layer", |canvas| {
                let (w, h) = (canvas.width, canvas.height);
                let stack = &mut canvas.layer_stack;
                let idx = stack.add_adjustment_layer(adjustment, w, h);
                for (i, l) in stack.layers.iter_mut().enumerate() {
                    l.selected = i == idx;
                }
                let layer = stack
                    .layers
                    .get_mut(idx)
                    .ok_or_else(|| CmdError::internal("new layer"))?;
                if let Some(name) = &p.name {
                    layer.name = name.clone();
                }
                Ok(layer.id)
            })?;
            return answer(s, doc, Some(id)).map(Plan::Done);
        }
        let id = structural(canvas, "New Layer", |canvas| {
            let (w, h) = (canvas.width, canvas.height);
            let stack = &mut canvas.layer_stack;
            let idx = stack.add_layer(w, h);
            let layer = stack
                .layers
                .get_mut(idx)
                .ok_or_else(|| CmdError::internal("new layer"))?;
            match p.kind {
                NewKind::Raster => {}
                NewKind::Fill => {
                    let [r, g, b, a] = fill.unwrap_or([255, 255, 255, 255]);
                    layer.tiles = iai_raster::tile::TileMap::new_solid(w, h, r, g, b, a);
                }
                // Made above.
                NewKind::Adjustment => {}
                NewKind::Group => {
                    let parent = layer.parent_id;
                    let mut group = iai_doc::layer::Layer::new_group(layer.id, &layer.name, w, h);
                    group.parent_id = parent;
                    *layer = group;
                }
            }
            if let Some(name) = &p.name {
                layer.name = name.clone();
            }
            let id = layer.id;
            // Only the new layer is picked, so what is done next acts on it.
            for (i, l) in stack.layers.iter_mut().enumerate() {
                l.selected = i == idx;
            }
            Ok(id)
        })?;
        answer(s, doc, Some(id)).map(Plan::Done)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DeleteParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// Layer id (see `doc.inspect`).
    pub layer: u32,
    /// When the layer is one of those picked in the Layers panel, delete
    /// every picked one, as the panel's Delete does.
    #[serde(default)]
    pub picked: bool,
}
impl Validate for DeleteParams {}

static DELETE: CommandMeta = CommandMeta {
    id: "layer.delete",
    label_vi: "Xóa layer",
    label_en: "Delete Layer",
    doc: "Delete a layer; a group goes with the layers in it. The last layer cannot be deleted. \
          With `picked`, the other layers picked in the Layers panel go too.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct DeleteLayer;

impl Command for DeleteLayer {
    type Params = DeleteParams;
    type Output = LayerChanged;
    fn meta(&self) -> &'static CommandMeta {
        &DELETE
    }
    fn plan(&self, s: &mut Session<'_>, p: DeleteParams) -> Result<Plan<LayerChanged>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let idx = layer_index(canvas, Some(p.layer))?;
        if p.picked {
            // The panel's own way: the picked layers with it, when it is one.
            if !canvas.delete_layer_at(idx) {
                return Err(not_enabled(
                    "a page keeps at least one layer",
                    "Trang phải còn ít nhất một layer",
                ));
            }
        } else {
            let mut doomed = canvas.layer_stack.with_group_contents(&[idx]);
            if doomed.len() >= canvas.layer_stack.layers.len() {
                return Err(not_enabled(
                    "a page keeps at least one layer",
                    "Trang phải còn ít nhất một layer",
                ));
            }
            doomed.sort_unstable_by(|a, b| b.cmp(a));
            structural(canvas, "Delete Layer", |canvas| {
                for i in doomed {
                    canvas.layer_stack.remove_layer(i);
                }
                canvas.layer_stack.normalize_active_idx();
                Ok(())
            })?;
        }
        let active = {
            let c = s.canvas(Some(doc))?;
            c.layer_stack
                .layers
                .get(c.layer_stack.active_idx)
                .map(|l| l.id)
        };
        answer(s, doc, active).map(Plan::Done)
    }
}

/// Where `layer.move` puts the layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MoveTo {
    /// One step up the stack (in front).
    Up,
    /// One step down (behind).
    Down,
    /// To the top of its folder or of the page.
    Top,
    /// To the bottom, just above the Background.
    Bottom,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MoveParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// Layer id (see `doc.inspect`).
    pub layer: u32,
    pub to: MoveTo,
}
impl Validate for MoveParams {}

static MOVE: CommandMeta = CommandMeta {
    id: "layer.move",
    label_vi: "Đổi thứ tự layer",
    label_en: "Move Layer",
    doc: "Move a layer (a group with its layers) up or down the stack, or to the top or bottom. \
          Nothing goes under the Background.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct MoveLayer;

impl Command for MoveLayer {
    type Params = MoveParams;
    type Output = LayerChanged;
    fn meta(&self) -> &'static CommandMeta {
        &MOVE
    }
    fn plan(&self, s: &mut Session<'_>, p: MoveParams) -> Result<Plan<LayerChanged>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        layer_index(canvas, Some(p.layer))?;
        let id = p.layer;
        let moved = structural(canvas, "Move Layer", |canvas| {
            let stack = &mut canvas.layer_stack;
            let step = |stack: &mut iai_doc::layer::LayerStack, up: bool| {
                let Some(i) = stack.layers.iter().position(|l| l.id == id) else {
                    return false;
                };
                if up {
                    stack.move_layer_up(i)
                } else {
                    stack.move_layer_down(i)
                }
            };
            let moved = match p.to {
                MoveTo::Up => step(stack, true),
                MoveTo::Down => step(stack, false),
                MoveTo::Top | MoveTo::Bottom => {
                    let up = p.to == MoveTo::Top;
                    let mut any = false;
                    let limit = stack.layers.len();
                    for _ in 0..limit {
                        if !step(stack, up) {
                            break;
                        }
                        any = true;
                    }
                    any
                }
            };
            if let Some(i) = stack.layers.iter().position(|l| l.id == id) {
                stack.active_idx = i;
            }
            Ok(moved)
        })?;
        if !moved {
            return Err(not_enabled(
                "the layer cannot move that way",
                "Không chuyển layer theo hướng đó được",
            ));
        }
        answer(s, doc, Some(id)).map(Plan::Done)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SetParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// Layer id (see `doc.inspect`); the active layer when omitted.
    pub layer: Option<u32>,
    #[schemars(length(min = 1, max = 200))]
    pub name: Option<String>,
    pub visible: Option<bool>,
    pub locked: Option<bool>,
    /// Lock the transparent pixels: painting keeps to what is there.
    pub lock_alpha: Option<bool>,
    /// 0 (clear) to 1 (opaque).
    #[schemars(range(min = 0.0, max = 1.0))]
    pub opacity: Option<f32>,
    pub blend: Option<Blend>,
    /// Move it so its top-left corner is here, in document pixels.
    #[schemars(range(min = -100000, max = 100000))]
    pub x: Option<i32>,
    #[schemars(range(min = -100000, max = 100000))]
    pub y: Option<i32>,
    /// Make it the active layer.
    #[serde(default)]
    pub activate: bool,
}

impl SetParams {
    /// The undo step's name: the panel's for one change, else one for all.
    fn step(&self) -> &'static str {
        let changes = [
            (self.name.is_some(), "Rename Layer"),
            (self.visible.is_some(), "Toggle Visibility"),
            (self.locked.is_some(), "Toggle Lock"),
            (self.lock_alpha.is_some(), "Toggle Lock Alpha"),
            (self.opacity.is_some(), "Set Opacity"),
            (self.blend.is_some(), "Set Blend Mode"),
            (self.x.is_some() || self.y.is_some(), "Move Layer"),
        ];
        let mut given = changes.iter().filter(|(on, _)| *on);
        match (given.next(), given.next()) {
            (Some((_, one)), None) => one,
            _ => "Layer Properties",
        }
    }
}

impl Validate for SetParams {
    fn validate(&self) -> Result<(), CmdError> {
        let nothing = self.name.is_none()
            && self.visible.is_none()
            && self.locked.is_none()
            && self.lock_alpha.is_none()
            && self.opacity.is_none()
            && self.blend.is_none()
            && self.x.is_none()
            && self.y.is_none()
            && !self.activate;
        if nothing {
            return Err(CmdError::bad_param(
                "(params)",
                "at least one of name, visible, locked, lockAlpha, opacity, blend, x, y, activate",
            ));
        }
        Ok(())
    }
}

static SET: CommandMeta = CommandMeta {
    id: "layer.set",
    label_vi: "Đổi thuộc tính layer",
    label_en: "Set Layer Properties",
    doc: "Change a layer's name, visibility, lock (all, or of its transparent pixels), opacity, \
          blend mode or position, or make it the active layer. Only the fields given change; one \
          undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct SetLayer;

impl Command for SetLayer {
    type Params = SetParams;
    type Output = LayerChanged;
    fn meta(&self) -> &'static CommandMeta {
        &SET
    }
    fn plan(&self, s: &mut Session<'_>, p: SetParams) -> Result<Plan<LayerChanged>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let idx = layer_index(canvas, p.layer)?;
        let moves = p.x.is_some() || p.y.is_some();
        if moves {
            let is_background = canvas
                .layer_stack
                .layers
                .get(idx)
                .is_some_and(|l| l.is_background);
            if is_background {
                return Err(CmdError::new(
                    ErrorCode::NotEnabled,
                    "the Background layer does not move",
                )
                .vi("Layer nền không di chuyển được"));
            }
        }
        let id = structural(canvas, p.step(), |canvas| {
            let stack = &mut canvas.layer_stack;
            if p.activate {
                stack.active_idx = idx;
                for (i, l) in stack.layers.iter_mut().enumerate() {
                    l.selected = i == idx;
                }
            }
            let layer = stack
                .layers
                .get_mut(idx)
                .ok_or_else(|| CmdError::internal("layer index"))?;
            if let Some(name) = &p.name {
                layer.name = name.clone();
            }
            if let Some(v) = p.visible {
                layer.visible = v;
            }
            if let Some(v) = p.locked {
                layer.locked = v;
            }
            if let Some(v) = p.lock_alpha {
                layer.lock_alpha = v;
            }
            if let Some(v) = p.opacity {
                layer.opacity = v.clamp(0.0, 1.0);
            }
            if let Some(b) = p.blend {
                layer.blend_mode = b.to_core();
            }
            if moves {
                // As the Move tool moves a layer: by its offset.
                layer.offset = (p.x.unwrap_or(layer.offset.0), p.y.unwrap_or(layer.offset.1));
            }
            Ok(layer.id)
        })?;
        answer(s, doc, Some(id)).map(Plan::Done)
    }
}

/// Where `layer.place` takes its picture from.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaceSource {
    /// An image file.
    #[schemars(length(min = 1, max = 4096))]
    pub path: Option<String>,
    /// Or an open document, as its shown page looks.
    pub doc: Option<u32>,
}

/// How the picture is sized into its box.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlaceFit {
    /// As large as fits whole, centred (the default).
    #[default]
    Contain,
    /// Filling the box, centred, the overflow cut off.
    Cover,
    /// At its own size, centred.
    None,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PlaceParams {
    /// Document id (see `doc.list`) to place into; the document shown when omitted.
    pub doc: Option<u32>,
    pub from: PlaceSource,
    /// The box on the page to place it in, in pixels; the whole page when omitted.
    #[serde(rename = "box")]
    pub area: Option<RectPx>,
    #[serde(default)]
    pub fit: PlaceFit,
    /// Kept clear inside the box, in pixels.
    #[serde(default)]
    #[schemars(range(min = 0, max = 10000))]
    pub margin_px: u32,
    /// Turn the picture clockwise first: 0, 90, 180 or 270.
    #[serde(default)]
    pub turn: u32,
    #[schemars(length(min = 1, max = 200))]
    pub name: Option<String>,
}

impl Validate for PlaceParams {
    fn validate(&self) -> Result<(), CmdError> {
        if self.from.path.is_some() == self.from.doc.is_some() {
            return Err(CmdError::bad_param("from", "a `path` or a `doc`"));
        }
        if !matches!(self.turn, 0 | 90 | 180 | 270) {
            return Err(CmdError::bad_param("turn", "0, 90, 180 or 270"));
        }
        Ok(())
    }
}

static PLACE: CommandMeta = CommandMeta {
    id: "layer.place",
    label_vi: "Đặt ảnh vào trang",
    label_en: "Place Image",
    doc: "Put a picture (an image file, or another open document) on the page as a new layer, \
          sized into a box (the whole page by default) and centred in it: `fit` contain \
          (whole, as large as fits), cover (filling the box) or none (own size). To centre a \
          photo on an A4 page, or lay several out on one page, one box each. One undo step.",
    effects: &[Effect::Edit, Effect::ReadFiles],
    featured: true,
};

pub struct PlaceImage;

/// The picture `from` names, as RGBA, and its name.
fn place_source(
    s: &mut Session<'_>,
    from: &PlaceSource,
) -> Result<(image::RgbaImage, String), CmdError> {
    match (&from.path, from.doc) {
        (Some(path), _) => {
            let path = std::path::Path::new(path);
            let canvas = iai_formats::FormatRegistry::new()
                .import(path)
                .map_err(|e| {
                    CmdError::new(ErrorCode::NotFound, format!("{}: {e}", path.display()))
                        .vi(format!("Không mở được ảnh {}", path.display()))
                })?;
            let image = image::RgbaImage::from_raw(
                canvas.width,
                canvas.height,
                canvas.flatten_for_export(),
            )
            .ok_or_else(|| CmdError::internal("empty picture"))?;
            let name = path
                .file_stem()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            Ok((image, name))
        }
        (None, Some(doc)) => {
            let title = s.document(Some(doc))?.title.clone();
            let canvas = s.canvas(Some(doc))?;
            let image = image::RgbaImage::from_raw(
                canvas.width,
                canvas.height,
                canvas.flatten_for_export(),
            )
            .ok_or_else(|| CmdError::internal("empty picture"))?;
            Ok((image, title))
        }
        (None, None) => Err(CmdError::bad_param("from", "a `path` or a `doc`")),
    }
}

impl Command for PlaceImage {
    type Params = PlaceParams;
    type Output = LayerChanged;

    fn meta(&self) -> &'static CommandMeta {
        &PLACE
    }

    fn plan(&self, s: &mut Session<'_>, p: PlaceParams) -> Result<Plan<LayerChanged>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let (picture, source_name) = place_source(s, &p.from)?;
        let picture = match p.turn {
            90 => image::imageops::rotate90(&picture),
            180 => image::imageops::rotate180(&picture),
            270 => image::imageops::rotate270(&picture),
            _ => picture,
        };
        let canvas = s.canvas_mut(Some(doc))?;
        let (page_w, page_h) = (canvas.width, canvas.height);
        let area = p.area.unwrap_or(RectPx {
            x: 0,
            y: 0,
            w: page_w,
            h: page_h,
        });
        let margin = p.margin_px.saturating_mul(2);
        let (room_w, room_h) = (
            area.w.saturating_sub(margin).max(1),
            area.h.saturating_sub(margin).max(1),
        );
        let (pw, ph) = (picture.width().max(1), picture.height().max(1));
        let scale = match p.fit {
            PlaceFit::Contain => (room_w as f32 / pw as f32).min(room_h as f32 / ph as f32),
            PlaceFit::Cover => (room_w as f32 / pw as f32).max(room_h as f32 / ph as f32),
            PlaceFit::None => 1.0,
        };
        let (sw, sh) = (
            ((pw as f32 * scale).round() as u32).max(1),
            ((ph as f32 * scale).round() as u32).max(1),
        );
        let largest = iai_canvas::canvas::MAX_DIMENSION;
        if sw > largest || sh > largest {
            return Err(CmdError::new(
                ErrorCode::TooLarge,
                "the placed picture is too large",
            ));
        }
        let scaled = if (sw, sh) == (pw, ph) {
            picture
        } else {
            image::imageops::resize(&picture, sw, sh, image::imageops::FilterType::Lanczos3)
        };
        // Cover: only what lies inside the box is kept.
        let (kept, kw, kh) = if p.fit == PlaceFit::Cover && (sw > room_w || sh > room_h) {
            let (kw, kh) = (sw.min(room_w), sh.min(room_h));
            let cut = image::imageops::crop_imm(
                &scaled,
                sw.saturating_sub(kw) / 2,
                sh.saturating_sub(kh) / 2,
                kw,
                kh,
            )
            .to_image();
            (cut, kw, kh)
        } else {
            (scaled, sw, sh)
        };
        let half = |room: u32, size: u32| i32::try_from(room.saturating_sub(size) / 2).unwrap_or(0);
        let margin_i = i32::try_from(p.margin_px).unwrap_or(0);
        let x = area
            .x
            .saturating_add(margin_i)
            .saturating_add(half(room_w, kw));
        let y = area
            .y
            .saturating_add(margin_i)
            .saturating_add(half(room_h, kh));
        let name = p.name.clone().unwrap_or(source_name);
        let id = structural(canvas, "Place Image", |canvas| {
            let (w, h) = (canvas.width, canvas.height);
            let stack = &mut canvas.layer_stack;
            let idx = stack.add_layer(w, h);
            let layer = stack
                .layers
                .get_mut(idx)
                .ok_or_else(|| CmdError::internal("new layer"))?;
            let id = layer.id;
            let parent = layer.parent_id;
            let mut placed = iai_doc::layer::Layer::from_rgba(id, &name, kept.into_raw(), kw, kh);
            placed.offset = (x, y);
            placed.parent_id = parent;
            *layer = placed;
            Ok(id)
        })?;
        answer(s, doc, Some(id)).map(Plan::Done)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ViaCopyParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// Layer id (see `doc.inspect`) to copy from; the active layer when omitted.
    pub layer: Option<u32>,
}
impl Validate for ViaCopyParams {}

static VIA_COPY: CommandMeta = CommandMeta {
    id: "layer.via_copy",
    label_vi: "Layer từ vùng chọn",
    label_en: "Layer via Copy",
    doc: "Copy what is selected on a layer to a new layer right above it, as Ctrl+J does: the \
          whole layer when nothing is selected, a folder with what is in it, and the \
          selection is dropped after. Without `layer`, the layers picked in the Layers panel \
          are copied. The copy becomes the active layer. One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct LayerViaCopy;

impl Command for LayerViaCopy {
    type Params = ViaCopyParams;
    type Output = LayerChanged;
    fn meta(&self) -> &'static CommandMeta {
        &VIA_COPY
    }
    fn plan(&self, s: &mut Session<'_>, p: ViaCopyParams) -> Result<Plan<LayerChanged>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        if p.layer.is_some() {
            let idx = layer_index(canvas, p.layer)?;
            canvas.layer_stack.select_only(idx);
        } else {
            layer_index(canvas, None)?;
        }
        let had_selection = canvas.selection.active;
        // A folder picked alone is copied with what is in it.
        if !canvas.duplicate_active_group() {
            canvas.begin_undo_group("Layer via Copy");
            canvas.layer_via_copy();
            if had_selection {
                canvas.deselect();
            }
            canvas.end_undo_group();
        }
        let stack = &canvas.layer_stack;
        let id = stack.layers.get(stack.active_idx).map(|l| l.id);
        if had_selection {
            s.emit(EngineEvent::Selection { doc });
        }
        answer(s, doc, id).map(Plan::Done)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TransformParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// Layer id (see `doc.inspect`); the active layer when omitted.
    pub layer: Option<u32>,
    /// Its size both ways, 1 being as it is (0.5 half, 2 twice).
    #[schemars(range(min = 0.01, max = 50.0))]
    pub scale: Option<f32>,
    /// Or its width and its height on their own.
    #[schemars(range(min = 0.01, max = 50.0))]
    pub scale_x: Option<f32>,
    #[schemars(range(min = 0.01, max = 50.0))]
    pub scale_y: Option<f32>,
    /// Turn it clockwise by this many degrees, round its middle.
    #[serde(default)]
    #[schemars(range(min = -360.0, max = 360.0))]
    pub angle: f32,
    /// Then move it right (and down) by this many pixels.
    #[serde(default)]
    #[schemars(range(min = -100000, max = 100000))]
    pub dx: i32,
    #[serde(default)]
    #[schemars(range(min = -100000, max = 100000))]
    pub dy: i32,
}

impl TransformParams {
    fn scales(&self) -> (f32, f32) {
        match self.scale {
            Some(s) => (s, s),
            None => (self.scale_x.unwrap_or(1.0), self.scale_y.unwrap_or(1.0)),
        }
    }
}

impl Validate for TransformParams {
    fn validate(&self) -> Result<(), CmdError> {
        if self.scale.is_some() && (self.scale_x.is_some() || self.scale_y.is_some()) {
            return Err(CmdError::bad_param(
                "scale",
                "either `scale`, or `scaleX` / `scaleY`",
            ));
        }
        let (sx, sy) = self.scales();
        if sx == 1.0 && sy == 1.0 && self.angle % 360.0 == 0.0 && self.dx == 0 && self.dy == 0 {
            return Err(CmdError::bad_param(
                "(params)",
                "a scale, an angle or a move that changes something",
            ));
        }
        Ok(())
    }
}

static TRANSFORM: CommandMeta = CommandMeta {
    id: "layer.transform",
    label_vi: "Biến đổi layer",
    label_en: "Transform Layer",
    doc: "Scale a picture layer, turn it by any angle round its middle and move it, as Free \
          Transform does (the pixels are resampled, bilinear). The layer grows to hold what it \
          becomes. Not for the Background, a locked layer, type, shapes or a layer with a mask. \
          One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct TransformLayer;

/// Where a point of the layer goes: scaled and turned round `centre`, then
/// moved; and back.
struct Affine {
    centre: (f32, f32),
    scale: (f32, f32),
    sin: f32,
    cos: f32,
    shift: (f32, f32),
}

impl Affine {
    fn forward(&self, x: f32, y: f32) -> (f32, f32) {
        let (u, v) = (
            (x - self.centre.0) * self.scale.0,
            (y - self.centre.1) * self.scale.1,
        );
        (
            self.centre.0 + self.shift.0 + u * self.cos - v * self.sin,
            self.centre.1 + self.shift.1 + u * self.sin + v * self.cos,
        )
    }

    fn back(&self, x: f32, y: f32) -> (f32, f32) {
        let (u, v) = (
            x - self.centre.0 - self.shift.0,
            y - self.centre.1 - self.shift.1,
        );
        let (u, v) = (u * self.cos + v * self.sin, -u * self.sin + v * self.cos);
        (
            self.centre.0 + u / self.scale.0,
            self.centre.1 + v / self.scale.1,
        )
    }
}

impl Command for TransformLayer {
    type Params = TransformParams;
    type Output = LayerChanged;
    fn meta(&self) -> &'static CommandMeta {
        &TRANSFORM
    }
    fn plan(
        &self,
        s: &mut Session<'_>,
        p: TransformParams,
    ) -> Result<Plan<LayerChanged>, CmdError> {
        use iai_raster::tile::TileMap;
        use rayon::prelude::*;
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let idx = layer_index(canvas, p.layer)?;
        let layer = canvas
            .layer_stack
            .layers
            .get(idx)
            .ok_or_else(|| CmdError::internal("layer index"))?;
        if layer.is_background {
            return Err(not_enabled(
                "the Background does not transform: duplicate it (layer.duplicate) first",
                "Layer nền không biến đổi được: nhân đôi layer trước",
            ));
        }
        if layer.locked || !layer.is_raster() || layer.mask.is_some() {
            return Err(not_enabled(
                "only an unlocked picture layer without a mask transforms",
                "Chỉ biến đổi được layer ảnh không khóa, không có mặt nạ",
            ));
        }
        let id = layer.id;
        let (sx, sy) = p.scales();
        if sx == 1.0 && sy == 1.0 && p.angle % 360.0 == 0.0 {
            // A move alone: by its offset, nothing resampled.
            structural(canvas, "Move Layer", |canvas| {
                let l = canvas
                    .layer_stack
                    .layers
                    .get_mut(idx)
                    .ok_or_else(|| CmdError::internal("layer index"))?;
                l.offset = (
                    l.offset.0.saturating_add(p.dx),
                    l.offset.1.saturating_add(p.dy),
                );
                Ok(())
            })?;
            s.emit(EngineEvent::Pixels { doc });
            return answer(s, doc, Some(id)).map(Plan::Done);
        }
        let Some((bx0, by0, bx1, by1)) = layer.tiles.content_bounds() else {
            return Err(not_enabled(
                "the layer is empty",
                "Layer trống, không có gì để biến đổi",
            ));
        };
        let (ox, oy) = (layer.offset.0 as f32, layer.offset.1 as f32);
        let (x0, y0, x1, y1) = (
            ox + bx0 as f32,
            oy + by0 as f32,
            ox + bx1 as f32,
            oy + by1 as f32,
        );
        let radians = p.angle.to_radians();
        let affine = Affine {
            centre: ((x0 + x1) / 2.0, (y0 + y1) / 2.0),
            scale: (sx, sy),
            sin: radians.sin(),
            cos: radians.cos(),
            shift: (p.dx as f32, p.dy as f32),
        };
        let corners = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)].map(|(x, y)| affine.forward(x, y));
        let lo_x = corners.iter().map(|c| c.0).fold(f32::MAX, f32::min).floor();
        let lo_y = corners.iter().map(|c| c.1).fold(f32::MAX, f32::min).floor();
        let hi_x = corners.iter().map(|c| c.0).fold(f32::MIN, f32::max).ceil();
        let hi_y = corners.iter().map(|c| c.1).fold(f32::MIN, f32::max).ceil();
        let (new_w, new_h) = (hi_x - lo_x, hi_y - lo_y);
        let fits = |v: f32| v >= 1.0 && v <= MAX_DIMENSION as f32;
        if !fits(new_w) || !fits(new_h) || !lo_x.is_finite() || !lo_y.is_finite() {
            return Err(CmdError::new(
                ErrorCode::TooLarge,
                format!(
                    "the layer would be {new_w}×{new_h} pixels: at most {MAX_DIMENSION} a side"
                ),
            )
            .vi("Layer sau biến đổi quá lớn"));
        }
        let (new_w, new_h) = (new_w as u32, new_h as u32);
        let Some(len) = Canvas::checked_rgba_len(new_w, new_h) else {
            return Err(
                CmdError::new(ErrorCode::TooLarge, "the layer would be too large")
                    .vi("Layer sau biến đổi quá lớn"),
            );
        };
        let origin = (lo_x as i32, lo_y as i32);
        let before = layer.tiles.clone();
        const STEP: &str = "Free Transform";
        Ok(Plan::Job(Job {
            doc: Some(doc),
            label_vi: "Biến đổi layer".into(),
            work: Box::new(move |cx| {
                cx.progress(0.1, "Đang biến đổi layer…");
                let row = (new_w as usize).saturating_mul(4);
                let mut out = vec![0u8; len];
                out.par_chunks_mut(row).enumerate().for_each(|(y, line)| {
                    let at_y = origin.1 as f32 + y as f32 + 0.5;
                    for (x, px) in line.chunks_exact_mut(4).enumerate() {
                        let at_x = origin.0 as f32 + x as f32 + 0.5;
                        let (qx, qy) = affine.back(at_x, at_y);
                        let (r, g, b, a) = before.sample_bilinear(qx - ox - 0.5, qy - oy - 0.5);
                        px.copy_from_slice(&[r, g, b, a]);
                    }
                });
                cx.check_cancelled()?;
                let after = TileMap::from_rgba(&out, new_w, new_h);
                Ok(Box::new(move |s: &mut Session<'_>| {
                    let canvas = s.canvas_mut(Some(doc))?;
                    let idx = layer_index(canvas, Some(id))?;
                    let same = canvas
                        .layer_stack
                        .layers
                        .get(idx)
                        .is_some_and(|l| l.tiles.same_tiles(&before));
                    if !same {
                        return Err(CmdError::new(
                            ErrorCode::StaleRevision,
                            "the layer changed while it was being worked on; run it again",
                        )
                        .vi("Layer đã đổi trong lúc xử lý — hãy chạy lại"));
                    }
                    structural(canvas, STEP, |canvas| {
                        let l = canvas
                            .layer_stack
                            .layers
                            .get_mut(idx)
                            .ok_or_else(|| CmdError::internal("layer index"))?;
                        l.tiles = after;
                        l.width = new_w;
                        l.height = new_h;
                        l.offset = origin;
                        Ok(())
                    })?;
                    s.emit(EngineEvent::Pixels { doc });
                    answer(s, doc, Some(id))
                }) as Apply<LayerChanged>)
            }),
        }))
    }
}

/// What to do with a layer's mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MaskOp {
    /// A new mask showing all of the layer.
    RevealAll,
    /// A new mask hiding all of it (paint it white where it should show).
    HideAll,
    /// Bake the mask into the layer's pixels and drop it.
    Apply,
    /// Drop the mask; the layer shows whole again.
    Delete,
    /// Turn the mask on, or off (it is kept, the layer shows whole).
    Enable,
    Disable,
    /// Link the mask to the layer's pixels (they move together), or not.
    Link,
    Unlink,
    /// Swap what the mask shows and hides.
    Invert,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MaskParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// Layer id (see `doc.inspect`); the active layer when omitted.
    pub layer: Option<u32>,
    pub op: MaskOp,
}
impl Validate for MaskParams {}

static MASK: CommandMeta = CommandMeta {
    id: "layer.mask",
    label_vi: "Mặt nạ layer",
    label_en: "Layer Mask",
    doc: "Add a layer mask that reveals or hides all of the layer, apply the mask to its \
          pixels, delete it, turn it on or off, link or unlink it, or invert it (Layer ▸ Layer \
          Mask, the Layers panel). One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct LayerMask;

impl Command for LayerMask {
    type Params = MaskParams;
    type Output = LayerChanged;
    fn meta(&self) -> &'static CommandMeta {
        &MASK
    }
    fn plan(&self, s: &mut Session<'_>, p: MaskParams) -> Result<Plan<LayerChanged>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let idx = layer_index(canvas, p.layer)?;
        let layer = canvas
            .layer_stack
            .layers
            .get(idx)
            .ok_or_else(|| CmdError::internal("layer index"))?;
        let has = layer.mask.is_some();
        match p.op {
            MaskOp::RevealAll | MaskOp::HideAll if layer.is_background => {
                return Err(not_enabled(
                    "the Background takes no mask",
                    "Layer nền không có mặt nạ",
                ));
            }
            MaskOp::RevealAll | MaskOp::HideAll if has => {
                return Err(not_enabled(
                    "the layer has a mask already",
                    "Layer đã có mặt nạ",
                ));
            }
            MaskOp::RevealAll | MaskOp::HideAll => {}
            _ if !has => {
                return Err(not_enabled("the layer has no mask", "Layer chưa có mặt nạ"));
            }
            _ => {}
        }
        let label = match p.op {
            MaskOp::RevealAll => "Add Layer Mask",
            MaskOp::HideAll => "Add Hide-All Layer Mask",
            MaskOp::Apply => "Apply Layer Mask",
            MaskOp::Delete => "Delete Layer Mask",
            MaskOp::Enable | MaskOp::Disable => "Toggle Layer Mask",
            MaskOp::Link | MaskOp::Unlink => "Toggle Mask Link",
            MaskOp::Invert => "Invert Mask",
        };
        let id = structural(canvas, label, |canvas| {
            if p.op == MaskOp::Apply {
                // The panel's way: a folder's mask bakes into what it holds.
                if !canvas.layer_stack.apply_layer_mask(idx) {
                    return Err(not_enabled(
                        "the mask cannot be applied: the group holds type, vectors or an \
                         adjustment",
                        "Không áp dụng được mask: nhóm có chữ, vector hoặc adjustment",
                    ));
                }
            }
            let layer = canvas
                .layer_stack
                .layers
                .get_mut(idx)
                .ok_or_else(|| CmdError::internal("layer index"))?;
            match p.op {
                MaskOp::RevealAll => layer.add_mask(true),
                MaskOp::HideAll => layer.add_mask(false),
                MaskOp::Apply => {}
                MaskOp::Delete => layer.delete_mask(),
                MaskOp::Enable | MaskOp::Disable => {
                    if let Some(mask) = layer.mask.as_mut() {
                        mask.enabled = p.op == MaskOp::Enable;
                    }
                }
                MaskOp::Link | MaskOp::Unlink => layer.mask_linked = p.op == MaskOp::Link,
                MaskOp::Invert => {
                    if let Some(mask) = layer.mask.as_mut() {
                        mask.invert_tiles();
                    }
                }
            }
            Ok(layer.id)
        })?;
        answer(s, doc, Some(id)).map(Plan::Done)
    }
}

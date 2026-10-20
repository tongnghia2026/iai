//! Layers of the shown page: add, delete, reorder, change, place a picture.

use iai_canvas::canvas::Canvas;
use iai_history::command::LayerStructureCommand;
use iai_history::gateway::ChangeKind;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{not_enabled, parse_color, Blend, RectPx};
use super::doc::{layer_infos, LayerInfo};
use crate::{
    layer_index, CmdError, Command, CommandMeta, Effect, EngineEvent, ErrorCode, Plan, Session,
    Validate,
};

/// Run `edit` on `canvas`'s layer stack as one undo step named `label`.
fn structural<T>(
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

fn answer(s: &mut Session<'_>, doc: u32, layer: Option<u32>) -> Result<LayerChanged, CmdError> {
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
}

impl Validate for NewParams {
    fn validate(&self) -> Result<(), CmdError> {
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
    doc: "Add a layer (empty, filled with a colour, or a group folder) above the active layer \
          or above `above`. It becomes the active layer.",
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
            Ok(layer.id)
        })?;
        answer(s, doc, Some(id)).map(Plan::Done)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LayerParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// Layer id (see `doc.inspect`).
    pub layer: u32,
}
impl Validate for LayerParams {}

static DELETE: CommandMeta = CommandMeta {
    id: "layer.delete",
    label_vi: "Xóa layer",
    label_en: "Delete Layer",
    doc: "Delete a layer; a group goes with the layers in it. The last layer cannot be deleted.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct DeleteLayer;

impl Command for DeleteLayer {
    type Params = LayerParams;
    type Output = LayerChanged;
    fn meta(&self) -> &'static CommandMeta {
        &DELETE
    }
    fn plan(&self, s: &mut Session<'_>, p: LayerParams) -> Result<Plan<LayerChanged>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let idx = layer_index(canvas, Some(p.layer))?;
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

impl Validate for SetParams {
    fn validate(&self) -> Result<(), CmdError> {
        let nothing = self.name.is_none()
            && self.visible.is_none()
            && self.locked.is_none()
            && self.opacity.is_none()
            && self.blend.is_none()
            && self.x.is_none()
            && self.y.is_none()
            && !self.activate;
        if nothing {
            return Err(CmdError::bad_param(
                "(params)",
                "at least one of name, visible, locked, opacity, blend, x, y, activate",
            ));
        }
        Ok(())
    }
}

static SET: CommandMeta = CommandMeta {
    id: "layer.set",
    label_vi: "Đổi thuộc tính layer",
    label_en: "Set Layer Properties",
    doc: "Change a layer's name, visibility, lock, opacity, blend mode or position, or make it \
          the active layer. Only the fields given change; one undo step.",
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
        let id = structural(canvas, "Layer Properties", |canvas| {
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
    doc: "Copy what is selected on a layer to a new layer right above it (Ctrl+J); the whole \
          layer when nothing is selected. The new layer becomes the active one. One undo step.",
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
        canvas.layer_stack.active_idx = layer_index(canvas, p.layer)?;
        let idx = if canvas.selection.active {
            canvas.layer_via_copy()
        } else {
            canvas.duplicate_active_layer()
        };
        canvas.layer_stack.active_idx = idx;
        let id = canvas.layer_stack.layers.get(idx).map(|l| l.id);
        answer(s, doc, id).map(Plan::Done)
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
          pixels, or delete it (Layer ▸ Layer Mask). One undo step.",
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
            MaskOp::Apply | MaskOp::Delete if !has => {
                return Err(not_enabled("the layer has no mask", "Layer chưa có mặt nạ"));
            }
            _ => {}
        }
        let label = match p.op {
            MaskOp::RevealAll | MaskOp::HideAll => "Add Layer Mask",
            MaskOp::Apply => "Apply Layer Mask",
            MaskOp::Delete => "Delete Layer Mask",
        };
        let id = structural(canvas, label, |canvas| {
            let layer = canvas
                .layer_stack
                .layers
                .get_mut(idx)
                .ok_or_else(|| CmdError::internal("layer index"))?;
            match p.op {
                MaskOp::RevealAll => layer.add_mask(true),
                MaskOp::HideAll => layer.add_mask(false),
                MaskOp::Apply => layer.apply_mask(),
                MaskOp::Delete => layer.delete_mask(),
            }
            Ok(layer.id)
        })?;
        answer(s, doc, Some(id)).map(Plan::Done)
    }
}

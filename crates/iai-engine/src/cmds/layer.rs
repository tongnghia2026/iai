//! Layers of the shown page: add, delete, reorder, change.

use iai_canvas::canvas::Canvas;
use iai_history::command::LayerStructureCommand;
use iai_history::gateway::ChangeKind;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{not_enabled, parse_color, Blend};
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

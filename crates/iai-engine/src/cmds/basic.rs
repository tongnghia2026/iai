//! Everyday edits the menus have: fill and clear, the selection's edge,
//! duplicate / merge / flatten layers, close a document, select by colour or
//! with the wand, turn the page grey.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{not_enabled, parse_color, DocParams, SelectMode};
use super::doc::{layer_infos, LayerInfo};
use super::select::{selected, Selected};
use crate::{
    layer_index, CmdError, Command, CommandMeta, Effect, EngineEvent, ErrorCode, Plan, Session,
    Validate,
};

/// The layers after a change.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Layers {
    pub doc: u32,
    /// The layer made or changed, active now.
    pub layer: Option<u32>,
    /// The stack after the change, top first.
    pub layers: Vec<LayerInfo>,
}

fn layers(s: &mut Session<'_>, doc: u32) -> Result<Layers, CmdError> {
    let canvas = s.canvas(Some(doc))?;
    let layer = canvas
        .layer_stack
        .layers
        .get(canvas.layer_stack.active_idx)
        .map(|l| l.id);
    let layers = layer_infos(canvas);
    s.emit(EngineEvent::Structure { doc });
    Ok(Layers { doc, layer, layers })
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FillParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// Layer id (see `doc.inspect`); the active layer when omitted. It
    /// becomes the active layer.
    pub layer: Option<u32>,
    /// "#RRGGBB" or "#RRGGBBAA".
    pub color: String,
}

impl Validate for FillParams {
    fn validate(&self) -> Result<(), CmdError> {
        parse_color("color", &self.color).map(|_| ())
    }
}

static FILL: CommandMeta = CommandMeta {
    id: "edit.fill",
    label_vi: "Tô màu",
    label_en: "Fill",
    doc: "Fill the selection (the whole layer when nothing is selected) with a colour, as \
          Edit ▸ Fill does. One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct Fill;

impl Command for Fill {
    type Params = FillParams;
    type Output = Layers;

    fn meta(&self) -> &'static CommandMeta {
        &FILL
    }

    fn plan(&self, s: &mut Session<'_>, p: FillParams) -> Result<Plan<Layers>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let color = parse_color("color", &p.color)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let idx = layer_index(canvas, p.layer)?;
        let layer = canvas
            .layer_stack
            .layers
            .get(idx)
            .ok_or_else(|| CmdError::internal("layer index"))?;
        if !layer.is_raster() || (layer.locked && !layer.is_background) {
            return Err(not_enabled(
                "the layer is not a raster layer, or it is locked",
                "Layer này không phải layer ảnh, hoặc đang khóa",
            ));
        }
        canvas.layer_stack.active_idx = idx;
        canvas.fill_solid_color(color);
        s.emit(EngineEvent::Pixels { doc });
        layers(s, doc).map(Plan::Done)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ClearParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// Layer id (see `doc.inspect`); the active layer when omitted.
    pub layer: Option<u32>,
}

impl Validate for ClearParams {}

static CLEAR: CommandMeta = CommandMeta {
    id: "edit.clear",
    label_vi: "Xóa vùng chọn",
    label_en: "Clear",
    doc: "Erase what is selected on a layer to clear, as the Delete key does (on the \
          Background the selection is filled with white instead). Needs a selection. One undo \
          step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct Clear;

impl Command for Clear {
    type Params = ClearParams;
    type Output = Layers;

    fn meta(&self) -> &'static CommandMeta {
        &CLEAR
    }

    fn plan(&self, s: &mut Session<'_>, p: ClearParams) -> Result<Plan<Layers>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        if !canvas.selection.active {
            return Err(not_enabled("nothing is selected", "Chưa chọn vùng nào"));
        }
        let idx = layer_index(canvas, p.layer)?;
        canvas.layer_stack.active_idx = idx;
        let background = canvas
            .layer_stack
            .layers
            .get(idx)
            .is_some_and(|l| l.is_background);
        let done = if background {
            canvas.fill_solid_color([255, 255, 255, 255]);
            true
        } else {
            canvas.clear_selection()
        };
        if !done {
            return Err(not_enabled(
                "the layer is locked, or the selection misses it",
                "Layer đang khóa, hoặc vùng chọn không nằm trên layer",
            ));
        }
        s.emit(EngineEvent::Pixels { doc });
        layers(s, doc).map(Plan::Done)
    }
}

/// A change of the selection's edge.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EdgeChange {
    /// Soften the edge over `amount` pixels.
    Feather,
    /// Move the edge out by `amount` pixels.
    Grow,
    /// Move the edge in by `amount` pixels.
    Shrink,
    /// Round off corners and specks over `amount` pixels.
    Smooth,
    /// Only a band `amount` pixels wide along the edge.
    Border,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModifyParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    pub change: EdgeChange,
    /// In pixels, 0.1 to 500.
    #[schemars(range(min = 0.1, max = 500.0))]
    pub amount: f32,
}

impl Validate for ModifyParams {
    fn validate(&self) -> Result<(), CmdError> {
        if !(0.1..=500.0).contains(&self.amount) {
            return Err(CmdError::bad_param("amount", "0.1 to 500 pixels"));
        }
        Ok(())
    }
}

static MODIFY: CommandMeta = CommandMeta {
    id: "select.modify",
    label_vi: "Sửa viền vùng chọn",
    label_en: "Modify Selection",
    doc: "Change the selection's edge as Select ▸ Modify does: feather (soften), grow, \
          shrink, smooth or border, by `amount` pixels. One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct ModifySelection;

impl Command for ModifySelection {
    type Params = ModifyParams;
    type Output = Selected;

    fn meta(&self) -> &'static CommandMeta {
        &MODIFY
    }

    fn plan(&self, s: &mut Session<'_>, p: ModifyParams) -> Result<Plan<Selected>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        if !canvas.selection.active {
            return Err(not_enabled("nothing is selected", "Chưa chọn vùng nào"));
        }
        let amount = p.amount.clamp(0.1, 500.0);
        let pixels = (amount.round() as u32).max(1);
        match p.change {
            EdgeChange::Feather => canvas.feather_selection(amount),
            EdgeChange::Grow => canvas.grow_selection(pixels),
            EdgeChange::Shrink => canvas.shrink_selection(pixels),
            EdgeChange::Smooth => canvas.smooth_selection(amount),
            EdgeChange::Border => canvas.border_selection(pixels),
        }
        selected(s, doc).map(Plan::Done)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OneLayerParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// Layer id (see `doc.inspect`); the active layer when omitted.
    pub layer: Option<u32>,
}

impl Validate for OneLayerParams {}

static DUPLICATE: CommandMeta = CommandMeta {
    id: "layer.duplicate",
    label_vi: "Nhân đôi layer",
    label_en: "Duplicate Layer",
    doc: "Copy a layer (Ctrl+J without a selection): the copy goes right above it and becomes \
          the active layer. One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct DuplicateLayer;

impl Command for DuplicateLayer {
    type Params = OneLayerParams;
    type Output = Layers;

    fn meta(&self) -> &'static CommandMeta {
        &DUPLICATE
    }

    fn plan(&self, s: &mut Session<'_>, p: OneLayerParams) -> Result<Plan<Layers>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        canvas.layer_stack.active_idx = layer_index(canvas, p.layer)?;
        let idx = canvas.duplicate_active_layer();
        canvas.layer_stack.active_idx = idx;
        layers(s, doc).map(Plan::Done)
    }
}

static MERGE_DOWN: CommandMeta = CommandMeta {
    id: "layer.merge_down",
    label_vi: "Gộp xuống",
    label_en: "Merge Down",
    doc: "Merge a layer into the one below it (Ctrl+E). One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct MergeDown;

impl Command for MergeDown {
    type Params = OneLayerParams;
    type Output = Layers;

    fn meta(&self) -> &'static CommandMeta {
        &MERGE_DOWN
    }

    fn plan(&self, s: &mut Session<'_>, p: OneLayerParams) -> Result<Plan<Layers>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let idx = layer_index(canvas, p.layer)?;
        if !canvas.merge_down(idx) {
            return Err(not_enabled(
                "it cannot be merged down: the bottom layer, a folder's edge, or a locked or \
                 adjustment layer below",
                "Không gộp xuống được layer này",
            ));
        }
        layers(s, doc).map(Plan::Done)
    }
}

static FLATTEN: CommandMeta = CommandMeta {
    id: "layer.flatten",
    label_vi: "Gộp thành một lớp",
    label_en: "Flatten Image",
    doc: "Merge every visible layer into one Background over white, as Layer ▸ Flatten Image \
          does (hidden layers are dropped). One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct Flatten;

impl Command for Flatten {
    type Params = DocParams;
    type Output = Layers;

    fn meta(&self) -> &'static CommandMeta {
        &FLATTEN
    }

    fn plan(&self, s: &mut Session<'_>, p: DocParams) -> Result<Plan<Layers>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        s.canvas_mut(Some(doc))?.flatten_all();
        s.emit(EngineEvent::Pixels { doc });
        s.notice("Đã gộp thành một lớp", "Image flattened");
        layers(s, doc).map(Plan::Done)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CloseParams {
    /// Document id (see `doc.list`).
    pub doc: u32,
    /// Close it although it has changes not saved (they are lost).
    #[serde(default)]
    pub discard: bool,
}

impl Validate for CloseParams {}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Closed {
    pub doc: u32,
    /// The document shown now.
    pub shown: Option<u32>,
}

static CLOSE: CommandMeta = CommandMeta {
    id: "doc.close",
    label_vi: "Đóng tài liệu",
    label_en: "Close Document",
    doc: "Close a document (its tab), e.g. a photo done with in a long batch, to free memory. A \
          document with changes not saved is refused unless `discard` is true. Not taken back \
          by an atomic batch, and no dry run.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct CloseDocument;

impl Command for CloseDocument {
    type Params = CloseParams;
    type Output = Closed;

    fn meta(&self) -> &'static CommandMeta {
        &CLOSE
    }

    fn plan(&self, s: &mut Session<'_>, p: CloseParams) -> Result<Plan<Closed>, CmdError> {
        let modified = s.document(Some(p.doc))?.is_modified();
        if modified && !p.discard {
            return Err(CmdError::new(
                ErrorCode::NotEnabled,
                "the document has changes not saved: save it first, or close with discard",
            )
            .vi("Tài liệu còn thay đổi chưa lưu"));
        }
        let shown = s.close_document(p.doc)?;
        Ok(Plan::Done(Closed { doc: p.doc, shown }))
    }
}

/// A point on the page, in pixels.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PointPx {
    #[schemars(range(min = 0, max = 100000))]
    pub x: u32,
    #[schemars(range(min = 0, max = 100000))]
    pub y: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ColorRangeParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The colour to select, "#RRGGBB".
    pub color: Option<String>,
    /// Or the colour the page has at this point.
    pub at: Option<PointPx>,
    /// How far from the colour still counts (0 to 200): small picks only
    /// that colour, large a whole family with a soft edge.
    #[serde(default = "forty")]
    #[schemars(range(min = 0, max = 200))]
    pub fuzziness: u8,
    #[serde(default)]
    pub mode: SelectMode,
}

fn forty() -> u8 {
    40
}

impl Validate for ColorRangeParams {
    fn validate(&self) -> Result<(), CmdError> {
        if self.color.is_some() == self.at.is_some() {
            return Err(CmdError::bad_param("color", "a `color` or a point `at`"));
        }
        if let Some(c) = &self.color {
            parse_color("color", c)?;
        }
        Ok(())
    }
}

static COLOR_RANGE: CommandMeta = CommandMeta {
    id: "select.color_range",
    label_vi: "Chọn theo màu",
    label_en: "Color Range",
    doc: "Select every part of the page of a colour (Select ▸ Color Range), e.g. a plain \
          background: give the colour, or a point `at` to take it from. One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct ColorRange;

impl Command for ColorRange {
    type Params = ColorRangeParams;
    type Output = Selected;

    fn meta(&self) -> &'static CommandMeta {
        &COLOR_RANGE
    }

    fn plan(&self, s: &mut Session<'_>, p: ColorRangeParams) -> Result<Plan<Selected>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let (w, h) = (canvas.width, canvas.height);
        canvas.ensure_pixels();
        let target = match (&p.color, p.at) {
            (Some(text), _) => {
                let [r, g, b, _] = parse_color("color", text)?;
                [r, g, b]
            }
            (None, Some(at)) => {
                if at.x >= w || at.y >= h {
                    return Err(CmdError::bad_param("at", "a point on the page"));
                }
                let i = (at.y as usize)
                    .saturating_mul(w as usize)
                    .saturating_add(at.x as usize)
                    .saturating_mul(4);
                let px = canvas
                    .pixels
                    .get(i..i.saturating_add(3))
                    .ok_or_else(|| CmdError::internal("page pixels"))?;
                [
                    px.first().copied().unwrap_or(0),
                    px.get(1).copied().unwrap_or(0),
                    px.get(2).copied().unwrap_or(0),
                ]
            }
            (None, None) => return Err(CmdError::bad_param("color", "a `color` or `at`")),
        };
        let mask = iai_doc::selection::color_range_mask(&canvas.pixels, w, h, target, p.fuzziness);
        if mask.len() != canvas.selection.mask.len() {
            return Err(CmdError::internal("the mask is not the page's size"));
        }
        let mut cmd = iai_history::command::SelectionCommand::capture_before(
            "Color Range",
            &canvas.selection,
        );
        canvas.selection.apply_with_mode(mask, p.mode.to_core());
        cmd.capture_after(&canvas.selection);
        canvas.record_as(Box::new(cmd), iai_history::gateway::ChangeKind::Selection);
        selected(s, doc).map(Plan::Done)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WandParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// Where to click.
    pub at: PointPx,
    /// How different a colour may be and still be taken (0 to 255).
    #[serde(default = "thirty_two")]
    pub tolerance: u8,
    /// Only the area joined to the point (default), or that colour everywhere.
    #[serde(default = "yes")]
    pub contiguous: bool,
    /// From the page as it shows (default), or the active layer alone.
    #[serde(default = "yes")]
    pub sample_merged: bool,
    #[serde(default)]
    pub mode: SelectMode,
}

fn thirty_two() -> u8 {
    32
}

fn yes() -> bool {
    true
}

impl Validate for WandParams {}

static WAND: CommandMeta = CommandMeta {
    id: "select.wand",
    label_vi: "Cây đũa thần",
    label_en: "Magic Wand",
    doc: "Select the area of like colour around a point, as the Magic Wand does: e.g. click \
          the background to select it. One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct Wand;

impl Command for Wand {
    type Params = WandParams;
    type Output = Selected;

    fn meta(&self) -> &'static CommandMeta {
        &WAND
    }

    fn plan(&self, s: &mut Session<'_>, p: WandParams) -> Result<Plan<Selected>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        if p.at.x >= canvas.width || p.at.y >= canvas.height {
            return Err(CmdError::bad_param("at", "a point on the page"));
        }
        canvas.select_wand(
            p.at.x,
            p.at.y,
            p.tolerance,
            0,
            p.contiguous,
            true,
            0.0,
            p.sample_merged,
            p.mode.to_core(),
        );
        selected(s, doc).map(Plan::Done)
    }
}

static GRAYSCALE: CommandMeta = CommandMeta {
    id: "image.grayscale",
    label_vi: "Chuyển ảnh xám",
    label_en: "Grayscale",
    doc: "Turn every layer of the page grey (Image ▸ Mode ▸ Grayscale): each pixel its \
          brightness. One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct Grayscale;

impl Command for Grayscale {
    type Params = DocParams;
    type Output = Layers;

    fn meta(&self) -> &'static CommandMeta {
        &GRAYSCALE
    }

    fn plan(&self, s: &mut Session<'_>, p: DocParams) -> Result<Plan<Layers>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        s.canvas_mut(Some(doc))?.convert_to_grayscale();
        s.emit(EngineEvent::Pixels { doc });
        s.notice("Đã chuyển ảnh xám", "Converted to Grayscale");
        layers(s, doc).map(Plan::Done)
    }
}

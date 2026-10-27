//! The Layers panel's own work on the layer stack: groups, merging, the
//! Background, clipping, inverting, rasterizing, and a selection from a
//! layer's pixels — each done as the panel does it, one undo step named as
//! the panel names it.

use iai_canvas::canvas::Canvas;
use iai_history::command::LayerStructureCommand;
use iai_vector::arrange::{self, Align, Distribute};
use schemars::JsonSchema;
use serde::Deserialize;

use super::common::not_enabled;
use super::layer::{LayerChanged, answer, structural};
use super::select::{Selected, selected};
use crate::{
    CmdError, Command, CommandMeta, Effect, EngineEvent, Plan, Session, Validate, layer_index,
};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OnLayer {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// Layer id (see `doc.inspect`); the active layer when omitted.
    pub layer: Option<u32>,
}
impl Validate for OnLayer {}

/// The active layer's id now.
fn active_id(canvas: &Canvas) -> Option<u32> {
    let stack = &canvas.layer_stack;
    stack.layers.get(stack.active_idx).map(|l| l.id)
}

/// Pick `ids` in the Layers panel, the first of them active.
fn pick(canvas: &mut Canvas, ids: &[u32]) -> Result<(), CmdError> {
    let rows: Vec<usize> = ids
        .iter()
        .map(|id| layer_index(canvas, Some(*id)))
        .collect::<Result<_, _>>()?;
    let stack = &mut canvas.layer_stack;
    for (i, l) in stack.layers.iter_mut().enumerate() {
        l.selected = rows.contains(&i);
    }
    if let Some(&first) = rows.first() {
        stack.active_idx = first;
    }
    Ok(())
}

macro_rules! meta {
    ($name:ident, $id:literal, $vi:literal, $en:literal, $doc:literal) => {
        static $name: CommandMeta = CommandMeta {
            id: $id,
            label_vi: $vi,
            label_en: $en,
            doc: $doc,
            effects: &[Effect::Edit],
            featured: false,
        };
    };
}

// ---- groups ---------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GroupParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The layers to put in the folder; the ones picked in the Layers panel
    /// (else the active one) when omitted.
    #[schemars(length(min = 1, max = 1000))]
    pub layers: Option<Vec<u32>>,
}
impl Validate for GroupParams {}

meta!(
    GROUP,
    "layer.group",
    "Nhóm layer",
    "Group Layers",
    "Put layers in a new group folder (Ctrl+G): the given ones, or those picked in the Layers \
     panel. One undo step."
);

pub struct GroupLayers;

impl Command for GroupLayers {
    type Params = GroupParams;
    type Output = LayerChanged;
    fn meta(&self) -> &'static CommandMeta {
        &GROUP
    }
    fn plan(&self, s: &mut Session<'_>, p: GroupParams) -> Result<Plan<LayerChanged>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        if let Some(ids) = &p.layers {
            pick(canvas, ids)?;
        }
        let stack = &canvas.layer_stack;
        let can_group = stack.layers.iter().any(|l| l.selected && !l.is_background)
            || stack
                .layers
                .get(stack.active_idx)
                .is_some_and(|l| !l.is_background);
        if !can_group {
            return Err(not_enabled(
                "no layer to put in a group",
                "Không có layer nào để nhóm",
            ));
        }
        structural(canvas, "Group Layers", |canvas| {
            let (w, h) = (canvas.width, canvas.height);
            canvas.layer_stack.create_group_from_selected(w, h);
            Ok(())
        })?;
        s.notice("Grouped layers", "Grouped layers");
        let id = active_id(s.canvas(Some(doc))?);
        answer(s, doc, id).map(Plan::Done)
    }
}

meta!(
    UNGROUP,
    "layer.ungroup",
    "Bỏ nhóm",
    "Ungroup Layers",
    "Dissolve a group folder (Ctrl+Shift+G): its layers rise to where it was. One undo step."
);

pub struct Ungroup;

impl Command for Ungroup {
    type Params = OnLayer;
    type Output = LayerChanged;
    fn meta(&self) -> &'static CommandMeta {
        &UNGROUP
    }
    fn plan(&self, s: &mut Session<'_>, p: OnLayer) -> Result<Plan<LayerChanged>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let idx = layer_index(canvas, p.layer)?;
        if !canvas
            .layer_stack
            .layers
            .get(idx)
            .is_some_and(|l| l.is_group())
        {
            return Err(not_enabled(
                "the layer is not a group",
                "Layer hiện tại không phải nhóm",
            ));
        }
        structural(canvas, "Ungroup", |canvas| {
            canvas.layer_stack.ungroup(idx);
            Ok(())
        })?;
        s.notice("Ungrouped", "Ungrouped");
        let id = active_id(s.canvas(Some(doc))?);
        answer(s, doc, id).map(Plan::Done)
    }
}

// ---- merging ----------------------------------------------------------------

/// What `layer.merge` merges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MergeWhat {
    /// The layers picked in the Layers panel, into one (Ctrl+E).
    Picked,
    /// A group folder (`layer`), into one layer.
    Group,
    /// Every visible layer, stamped into a new one on top; they stay
    /// (Ctrl+Shift+E).
    Visible,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MergeParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    pub what: MergeWhat,
    /// The group, for `group`; the active layer when omitted.
    pub layer: Option<u32>,
}
impl Validate for MergeParams {}

meta!(
    MERGE,
    "layer.merge",
    "Gộp layer",
    "Merge Layers",
    "Merge the layers picked in the Layers panel into one (`picked`, Ctrl+E), a group folder \
     into one layer (`group`), or stamp every visible layer into a new one on top (`visible`, \
     Ctrl+Shift+E). One undo step."
);

/// What the panel says when there is nothing it can merge.
pub const MERGE_REFUSED_VI: &str =
    "Không gộp được: không có layer phù hợp bên dưới (nhóm, adjustment hoặc đang khóa)";

pub struct Merge;

impl Command for Merge {
    type Params = MergeParams;
    type Output = LayerChanged;
    fn meta(&self) -> &'static CommandMeta {
        &MERGE
    }
    fn plan(&self, s: &mut Session<'_>, p: MergeParams) -> Result<Plan<LayerChanged>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        match p.what {
            MergeWhat::Picked => {
                if !canvas.merge_selected() {
                    return Err(not_enabled("nothing to merge", MERGE_REFUSED_VI));
                }
            }
            MergeWhat::Group => {
                let idx = layer_index(canvas, p.layer)?;
                let group = canvas
                    .layer_stack
                    .layers
                    .get(idx)
                    .is_some_and(|l| l.is_group());
                if !group || !canvas.merge_group(idx) {
                    return Err(not_enabled(
                        "the layer is not a group that merges",
                        "Không gộp được nhóm này",
                    ));
                }
                s.notice("Merged group", "Merged group");
            }
            MergeWhat::Visible => {
                if !canvas.merge_visible() {
                    return Err(not_enabled(
                        "no visible layer to stamp",
                        "Không có layer nào đang hiện để gộp",
                    ));
                }
                s.notice("Stamped visible layers", "Stamped visible layers");
            }
        }
        let id = active_id(s.canvas(Some(doc))?);
        answer(s, doc, id).map(Plan::Done)
    }
}

// ---- the Background, inverting, rasterizing, clipping -------------------------

meta!(
    FROM_BACKGROUND,
    "layer.from_background",
    "Biến layer nền thành layer thường",
    "Layer From Background",
    "Make the Background an ordinary layer (it can then move, be cleared to transparent, take \
     a mask). One undo step."
);

pub struct FromBackground;

impl Command for FromBackground {
    type Params = OnLayer;
    type Output = LayerChanged;
    fn meta(&self) -> &'static CommandMeta {
        &FROM_BACKGROUND
    }
    fn plan(&self, s: &mut Session<'_>, p: OnLayer) -> Result<Plan<LayerChanged>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let idx = layer_index(canvas, p.layer)?;
        if !canvas
            .layer_stack
            .layers
            .get(idx)
            .is_some_and(|l| l.is_background)
        {
            return Err(not_enabled(
                "the layer is not the Background",
                "Layer này không phải layer nền",
            ));
        }
        let id = structural(canvas, "Layer From Background", |canvas| {
            canvas.layer_stack.unlock_background_layer(idx);
            Ok(canvas.layer_stack.layers.get(idx).map(|l| l.id))
        })?;
        s.notice(
            "Background converted to Layer 0",
            "Background converted to Layer 0",
        );
        answer(s, doc, id).map(Plan::Done)
    }
}

meta!(
    INVERT,
    "layer.invert",
    "Đảo màu",
    "Invert",
    "Invert a layer as Ctrl+I does: its mask when the mask is what is painted on (the Layers \
     panel's mask thumbnail picked), else its pixels (a raster layer, not locked, not in \
     CMYK). One undo step."
);

pub struct Invert;

impl Command for Invert {
    type Params = OnLayer;
    type Output = LayerChanged;
    fn meta(&self) -> &'static CommandMeta {
        &INVERT
    }
    fn plan(&self, s: &mut Session<'_>, p: OnLayer) -> Result<Plan<LayerChanged>, CmdError> {
        use iai_doc::layer::PaintTarget;
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let idx = layer_index(canvas, p.layer)?;
        let layer = canvas
            .layer_stack
            .layers
            .get(idx)
            .ok_or_else(|| CmdError::internal("layer index"))?;
        let mask = layer.paint_target == PaintTarget::Mask && layer.mask.is_some();
        if !mask && (!layer.is_raster() || layer.locked) {
            return Err(not_enabled(
                "Invert takes a raster layer, not locked, or a mask",
                "Invert cần layer raster (không khoá) hoặc một mask",
            ));
        }
        // Pixel invert edits the RGB mirror only and would orphan the inks.
        if !mask && canvas.is_cmyk() {
            return Err(not_enabled(
                "Invert does not work in CMYK (invert each ink with Curves)",
                "Invert chưa hỗ trợ ở chế độ CMYK (dùng Curves đảo từng kênh mực)",
            ));
        }
        let label = if mask { "Invert Mask" } else { "Invert" };
        let id = structural(canvas, label, |canvas| {
            let layer = canvas
                .layer_stack
                .layers
                .get_mut(idx)
                .ok_or_else(|| CmdError::internal("layer index"))?;
            if mask {
                if let Some(m) = layer.mask.as_mut() {
                    m.invert_tiles();
                }
            } else {
                layer.invert_pixels();
            }
            Ok(layer.id)
        })?;
        s.notice(label, label);
        answer(s, doc, Some(id)).map(Plan::Done)
    }
}

meta!(
    RASTERIZE,
    "layer.rasterize",
    "Raster hóa layer",
    "Rasterize Layer",
    "Turn a vector path layer into a plain picture layer that brushes paint on; it looks the \
     same. One undo step."
);

pub struct Rasterize;

impl Command for Rasterize {
    type Params = OnLayer;
    type Output = LayerChanged;
    fn meta(&self) -> &'static CommandMeta {
        &RASTERIZE
    }
    fn plan(&self, s: &mut Session<'_>, p: OnLayer) -> Result<Plan<LayerChanged>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let idx = layer_index(canvas, p.layer)?;
        let id = canvas
            .layer_stack
            .layers
            .get(idx)
            .map(|l| l.id)
            .ok_or_else(|| CmdError::internal("layer index"))?;
        canvas
            .execute(
                Box::new(iai_vector::command_vector::RasterizeVectorLayer::new(id)),
                iai_history::gateway::ChangeKind::LayerStructure,
            )
            .map_err(|e| not_enabled(e.message.clone(), e.message))?;
        canvas.layer_revision = canvas.layer_revision.saturating_add(1);
        answer(s, doc, Some(id)).map(Plan::Done)
    }
}

meta!(
    CLIP,
    "layer.clip",
    "Clipping mask",
    "Create / Release Clipping Mask",
    "Clip a layer to the one below it, so it shows only where that one has pixels (Ctrl+Alt+G); \
     on a layer already clipped, release it. One undo step."
);

pub struct Clip;

impl Command for Clip {
    type Params = OnLayer;
    type Output = LayerChanged;
    fn meta(&self) -> &'static CommandMeta {
        &CLIP
    }
    fn plan(&self, s: &mut Session<'_>, p: OnLayer) -> Result<Plan<LayerChanged>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let idx = layer_index(canvas, p.layer)?;
        canvas.layer_stack.active_idx = idx;
        let stack = &canvas.layer_stack;
        let layer = stack
            .layers
            .get(idx)
            .ok_or_else(|| CmdError::internal("layer index"))?;
        let release = layer.clip_parent_id.is_some();
        let base = if release {
            None
        } else {
            let Some(below) = idx.checked_sub(1).and_then(|b| stack.layers.get(b)) else {
                return Err(not_enabled(
                    "no layer below to clip into",
                    "No layer below to clip into",
                ));
            };
            // Clipped to a clipped layer: to the same base, one clipping group.
            let base = below.clip_parent_id.unwrap_or(below.id);
            if !stack.can_attach_clipped_child(layer.id, base) {
                return Err(not_enabled(
                    "the layer below takes no clipping (a group, or invalid)",
                    "Can't clip into the layer below (group/invalid)",
                ));
            }
            Some(base)
        };
        let (w, h) = (canvas.width, canvas.height);
        let label = if release {
            "Release Clipping Mask"
        } else {
            "Clipping Mask"
        };
        let mut cmd = LayerStructureCommand::capture_before(label, &canvas.layer_stack, w, h);
        if let Some(l) = canvas.layer_stack.layers.get_mut(idx) {
            if release {
                l.clip_parent_id = None;
                // The mask was the managed clip.
                l.mask = None;
                l.mask_active = false;
            } else {
                l.clip_parent_id = base;
            }
        }
        if release {
            canvas.clip_fp = 0;
        }
        canvas.refresh_clip_masks();
        cmd.capture_after(&canvas.layer_stack, w, h);
        canvas.record_as(
            Box::new(cmd),
            iai_history::gateway::ChangeKind::LayerStructure,
        );
        canvas.layer_revision = canvas.layer_revision.saturating_add(1);
        canvas.flatten_full();
        let said = if release {
            "Clipping mask removed"
        } else {
            "Clipping mask created (Ctrl+Alt+G)"
        };
        s.notice(said, said);
        let id = active_id(s.canvas(Some(doc))?);
        answer(s, doc, id).map(Plan::Done)
    }
}

// ---- a selection from a layer -------------------------------------------------

meta!(
    FROM_LAYER,
    "select.from_layer",
    "Chọn theo layer",
    "Load Layer Transparency",
    "Select the pixels a layer has, by their opacity (Ctrl+click its thumbnail), replacing the \
     selection."
);

pub struct SelectFromLayer;

impl Command for SelectFromLayer {
    type Params = OnLayer;
    type Output = Selected;
    fn meta(&self) -> &'static CommandMeta {
        &FROM_LAYER
    }
    fn plan(&self, s: &mut Session<'_>, p: OnLayer) -> Result<Plan<Selected>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let idx = layer_index(canvas, p.layer)?;
        if !canvas.load_layer_alpha_selection(idx) {
            return Err(not_enabled(
                "the layer has no pixels to select",
                "Layer has no pixels to select",
            ));
        }
        s.notice(
            "Loaded layer transparency as selection",
            "Loaded layer transparency as selection",
        );
        s.emit(EngineEvent::Selection { doc });
        selected(s, doc).map(Plan::Done)
    }
}

// ---- align / distribute ---------------------------------------------------

/// Where `layer.align` lines the layers up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AlignTo {
    Left,
    HorizontalCenter,
    Right,
    Top,
    VerticalCenter,
    Bottom,
}

impl AlignTo {
    fn core(self) -> Align {
        match self {
            AlignTo::Left => Align::Left,
            AlignTo::HorizontalCenter => Align::HorizontalCenter,
            AlignTo::Right => Align::Right,
            AlignTo::Top => Align::Top,
            AlignTo::VerticalCenter => Align::VerticalCenter,
            AlignTo::Bottom => Align::Bottom,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AlignParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The layers; the ones picked in the Layers panel (else the active one)
    /// when omitted.
    #[schemars(length(min = 1, max = 1000))]
    pub layers: Option<Vec<u32>>,
    pub align: AlignTo,
}
impl Validate for AlignParams {}

meta!(
    ALIGN,
    "layer.align",
    "Căn layer",
    "Align Layers",
    "Line layers up (the Move tool's Align buttons): one layer to the canvas, several to the \
     box round them all; a picked group moves as one. Locked layers and the Background stay. \
     One undo step."
);

pub struct AlignLayers;

impl Command for AlignLayers {
    type Params = AlignParams;
    type Output = LayerChanged;
    fn meta(&self) -> &'static CommandMeta {
        &ALIGN
    }
    fn plan(&self, s: &mut Session<'_>, p: AlignParams) -> Result<Plan<LayerChanged>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        if let Some(ids) = &p.layers {
            pick(canvas, ids)?;
        }
        let (w, h) = (canvas.width, canvas.height);
        let shifts = arrange::align(&canvas.layer_stack, w, h, p.align.core());
        if shifts.is_empty() {
            return Err(not_enabled(
                "no movable layer content to align (or it is aligned already)",
                "No movable layer content to align",
            ));
        }
        structural(canvas, "Align Layers", |canvas| {
            arrange::apply(&mut canvas.layer_stack, &shifts);
            Ok(())
        })?;
        let line = match shifts.len() {
            1 => "Aligned 1 layer".to_string(),
            n => format!("Aligned {n} layers"),
        };
        s.notice(line.clone(), line);
        let id = active_id(s.canvas(Some(doc))?);
        answer(s, doc, id).map(Plan::Done)
    }
}

/// Which centres `layer.distribute` spaces out evenly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DistributeParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The layers (three or more); the ones picked in the Layers panel when
    /// omitted.
    #[schemars(length(min = 3, max = 1000))]
    pub layers: Option<Vec<u32>>,
    pub axis: Axis,
}
impl Validate for DistributeParams {}

meta!(
    DISTRIBUTE,
    "layer.distribute",
    "Dàn đều layer",
    "Distribute Layers",
    "Space three or more layers' centres evenly across or down (the Move tool's Distribute \
     buttons); the outermost two stay where they are. One undo step."
);

pub struct DistributeLayers;

impl Command for DistributeLayers {
    type Params = DistributeParams;
    type Output = LayerChanged;
    fn meta(&self) -> &'static CommandMeta {
        &DISTRIBUTE
    }
    fn plan(
        &self,
        s: &mut Session<'_>,
        p: DistributeParams,
    ) -> Result<Plan<LayerChanged>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        if let Some(ids) = &p.layers {
            pick(canvas, ids)?;
        }
        let mode = match p.axis {
            Axis::Horizontal => Distribute::HorizontalCenters,
            Axis::Vertical => Distribute::VerticalCenters,
        };
        let shifts = arrange::distribute(&canvas.layer_stack, mode);
        if shifts.is_empty() {
            return Err(not_enabled(
                "pick at least 3 movable layers, not evenly spaced already",
                "Select at least 3 movable objects to distribute",
            ));
        }
        structural(canvas, "Distribute Layers", |canvas| {
            arrange::apply(&mut canvas.layer_stack, &shifts);
            Ok(())
        })?;
        let line = format!("Distributed {} objects", shifts.len());
        s.notice(line.clone(), line);
        let id = active_id(s.canvas(Some(doc))?);
        answer(s, doc, id).map(Plan::Done)
    }
}

// ---- step and repeat ------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StepCopyParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The layers to copy; the ones picked in the Layers panel when omitted.
    #[schemars(length(min = 1, max = 1000))]
    pub layers: Option<Vec<u32>>,
    /// How far right each copy lands from its original, in pixels.
    #[serde(default)]
    pub dx: f32,
    /// How far down, in pixels.
    #[serde(default)]
    pub dy: f32,
    /// Instead of `dx` / `dy`, the whole step as a page affine
    /// `[a, b, c, d, e, f]` (x' = a·x + c·y + e, y' = b·x + d·y + f): a turn
    /// or a scale with the move. Only path layers turn and scale; the others
    /// go where it takes their centre.
    pub matrix: Option<[f32; 6]>,
}

impl Validate for StepCopyParams {
    fn validate(&self) -> Result<(), CmdError> {
        if let Some(m) = &self.matrix {
            if self.dx != 0.0 || self.dy != 0.0 {
                return Err(CmdError::bad_param(
                    "matrix",
                    "either `matrix` or `dx` / `dy`",
                ));
            }
            if m.iter().any(|v| !v.is_finite()) {
                return Err(CmdError::bad_param("matrix", "six finite numbers"));
            }
        } else if !self.dx.is_finite() || !self.dy.is_finite() {
            return Err(CmdError::bad_param("dx", "finite numbers of pixels"));
        }
        Ok(())
    }
}

meta!(
    STEP_COPY,
    "layer.step_copy",
    "Nhân bản theo bước",
    "Step and Repeat",
    "Copy layers and move each copy by a step (the Move tool's Duplicate / Repeat): the copies \
     become the picked layers, so doing it again goes on from them — a row, a ring, a spiral. \
     Locked layers, groups and the Background are left out. One undo step."
);

pub struct StepCopy;

impl Command for StepCopy {
    type Params = StepCopyParams;
    type Output = LayerChanged;
    fn meta(&self) -> &'static CommandMeta {
        &STEP_COPY
    }
    fn plan(&self, s: &mut Session<'_>, p: StepCopyParams) -> Result<Plan<LayerChanged>, CmdError> {
        use iai_vector::vector::affine::AffineTransform;
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        if let Some(ids) = &p.layers {
            pick(canvas, ids)?;
        }
        let m = match p.matrix {
            Some([a, b, c, d, e, f]) => AffineTransform { a, b, c, d, e, f },
            None => AffineTransform::translate(p.dx, p.dy),
        };
        let copied = structural(canvas, "Repeat", |canvas| {
            match arrange::step_copy(&mut canvas.layer_stack, m) {
                0 => Err(not_enabled(
                    "no movable layer picked to copy",
                    "No movable objects selected",
                )),
                n => Ok(n),
            }
        })?;
        let line = format!(
            "Duplicated {copied} object{}",
            if copied == 1 { "" } else { "s" }
        );
        s.notice(line.clone(), line);
        let id = active_id(s.canvas(Some(doc))?);
        answer(s, doc, id).map(Plan::Done)
    }
}

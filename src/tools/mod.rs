#![allow(dead_code)]

pub mod brush;
pub mod clone_tool;
pub mod crop;
pub mod dodge_burn;
pub mod eraser;
pub mod eyedropper;
pub mod fill;
pub mod gradient;
pub mod hand_tool;
pub mod lasso;
pub mod move_tool;
pub mod patch;
pub mod pen;
pub mod pencil;
pub mod perspective_crop;
pub mod polygon_lasso;
pub mod refine_brush;
pub mod repair_brush;
pub mod scrub;
pub mod selection_ellipse;
pub mod selection_rect;
pub mod shape;
pub mod smart_select;
pub mod smudge;
pub mod text_tool;
pub mod transform_tool;
pub mod zoom_tool;

use crate::core::selection::SelectionMode;
pub use crate::extension::tool::{PointerEvent, Tool, ToolCtx, ToolId, ToolResponse};

pub const SEL_GROUP: &[ToolId] = &[ToolId::SelectionRect, ToolId::SelectionEllipse];
pub const LASSO_GROUP: &[ToolId] = &[ToolId::Lasso, ToolId::PolygonLasso];
pub const BRUSH_GROUP: &[ToolId] = &[ToolId::Brush, ToolId::Pencil];
pub const FILL_GROUP: &[ToolId] = &[ToolId::Fill, ToolId::Gradient];
pub const CROP_GROUP: &[ToolId] = &[ToolId::Crop, ToolId::PerspectiveCrop];
pub const DODGE_GROUP: &[ToolId] = &[ToolId::Dodge, ToolId::Burn];

const TOOL_GROUPS: &[&[ToolId]] = &[
    SEL_GROUP,
    LASSO_GROUP,
    BRUSH_GROUP,
    FILL_GROUP,
    CROP_GROUP,
    DODGE_GROUP,
];

fn group_for(id: ToolId) -> Option<&'static [ToolId]> {
    TOOL_GROUPS
        .iter()
        .copied()
        .find(|group| group.contains(&id))
}

fn default_group_tool(group: &[ToolId]) -> ToolId {
    if group.contains(&ToolId::Gradient) {
        ToolId::Gradient
    } else {
        group[0]
    }
}

impl ToolId {
    pub fn name(&self) -> &str {
        match self {
            ToolId::Brush => "Brush",
            ToolId::Eraser => "Eraser",
            ToolId::Pencil => "Pencil",
            ToolId::Move => "Move",
            ToolId::Crop => "Crop",
            ToolId::Zoom => "Zoom",
            ToolId::Hand => "Hand",
            ToolId::Fill => "Fill",
            ToolId::Gradient => "Gradient",
            ToolId::Eyedropper => "Eyedropper",
            ToolId::SelectionRect => "Rectangular Selection",
            ToolId::SelectionEllipse => "Ellipse Selection",
            ToolId::Lasso => "Lasso",
            ToolId::PolygonLasso => "Polygon Lasso",
            ToolId::SmartSelect => "Smart Select",
            ToolId::Clone => "Clone",
            ToolId::Transform => "Free Transform",
            ToolId::RefineBrush => "Refine Brush",
            ToolId::Text => "Type",
            ToolId::Shape => "Shape",
            ToolId::Repair => "Repair Brush",
            ToolId::PerspectiveCrop => "Perspective Crop",
            ToolId::Pen => "Pen",
            ToolId::Smudge => "Smudge",
            ToolId::Dodge => "Dodge",
            ToolId::Burn => "Burn",
            ToolId::Patch => "Patch",
        }
    }

    /// Whether this tool may act on a CMYK document. Selection/view/move/crop
    /// tools never write ink and are always fine. Pixel-writing tools are listed
    /// here only once they have an ink-native path; the rest would paint RGB
    /// through `get_tile_mut`, dropping the ink plane and desyncing the doc, so
    /// they stay blocked until their ink variant lands. Still blocked in v1:
    /// Smudge/Dodge/Burn, Text/Repair/Patch/Pen, Transform/PerspectiveCrop.
    pub fn allowed_in_cmyk(self) -> bool {
        matches!(
            self,
            ToolId::Brush
                | ToolId::Pencil
                | ToolId::Eraser
                | ToolId::Fill
                | ToolId::Gradient
                | ToolId::Clone
                | ToolId::Shape
                | ToolId::Move
                | ToolId::Crop
                | ToolId::Zoom
                | ToolId::Hand
                | ToolId::Eyedropper
                | ToolId::SelectionRect
                | ToolId::SelectionEllipse
                | ToolId::Lasso
                | ToolId::PolygonLasso
                | ToolId::SmartSelect
                | ToolId::RefineBrush
        )
    }
}

#[derive(Debug, Clone)]
pub struct ToolInput {
    pub screen_x: f32,
    pub screen_y: f32,
    pub pressure: f32,
    pub offset_x: f32,
    pub offset_y: f32,
    pub zoom: f32,
    pub selection_mode: SelectionMode,
    pub alt_held: bool,
    pub shift_held: bool,
    pub ctrl_held: bool,
}

impl ToolInput {
    pub fn to_pointer_event(&self) -> PointerEvent {
        let cx = (self.screen_x - self.offset_x) / self.zoom;
        let cy = (self.screen_y - self.offset_y) / self.zoom;
        PointerEvent {
            canvas_x: cx,
            canvas_y: cy,
            screen_x: self.screen_x,
            screen_y: self.screen_y,
            pressure: self.pressure,
            shift: self.shift_held,
            ctrl: self.ctrl_held,
            alt: self.alt_held,
            space: false,
            selection_mode: self.selection_mode,
        }
    }

    pub fn canvas_pos(&self) -> (f32, f32) {
        (
            (self.screen_x - self.offset_x) / self.zoom,
            (self.screen_y - self.offset_y) / self.zoom,
        )
    }
}

/// Every tool IAI ships, held as a concrete field.
///
/// Registration IS the struct: adding a tool means adding a field, one arm in
/// `tool_dyn`/`tool_dyn_mut`, and its accessor. That replaces the old design —
/// a `Vec<Box<dyn Tool>>` searched by id and then downcast with a panicking
/// `Any` cast — with compile-time wiring: a missing or mistyped tool is a type
/// error, not a runtime panic.
pub struct ToolManager {
    active: ToolId,
    last_event: Option<PointerEvent>,
    group_preferences: Vec<(ToolId, ToolId)>,

    brush: brush::BrushTool,
    eraser: eraser::EraserTool,
    move_tool: move_tool::MoveTool,
    eyedropper: eyedropper::EyedropperTool,
    fill: fill::FillTool,
    crop: crop::CropTool,
    gradient: gradient::GradientTool,
    pencil: pencil::PencilTool,
    zoom: zoom_tool::ZoomTool,
    hand: hand_tool::HandTool,
    selection_rect: selection_rect::SelectionRectTool,
    selection_ellipse: selection_ellipse::SelectionEllipseTool,
    lasso: lasso::LassoTool,
    polygon_lasso: polygon_lasso::PolygonLassoTool,
    clone: clone_tool::CloneTool,
    transform: transform_tool::TransformTool,
    smart_select: smart_select::SmartSelectTool,
    refine_brush: refine_brush::RefineBrushTool,
    text: text_tool::TextTool,
    shape: shape::ShapeTool,
    repair: repair_brush::RepairBrushTool,
    perspective_crop: perspective_crop::PerspectiveCropTool,
    pen: pen::PenTool,
    smudge: smudge::SmudgeTool,
    /// Dodge and Burn share one tool type but are two independent instances
    /// (separate size/exposure settings), exactly as before.
    dodge: dodge_burn::DodgeBurnTool,
    burn: dodge_burn::DodgeBurnTool,
    patch: patch::PatchTool,
}

impl ToolManager {
    pub fn new() -> Self {
        Self {
            active: ToolId::Move,
            last_event: None,
            group_preferences: TOOL_GROUPS
                .iter()
                .map(|group| (group[0], default_group_tool(group)))
                .collect(),
            brush: brush::BrushTool::new(),
            eraser: eraser::EraserTool::new(),
            move_tool: move_tool::MoveTool::new(),
            eyedropper: eyedropper::EyedropperTool::new(),
            fill: fill::FillTool::new(),
            crop: crop::CropTool::new(),
            gradient: gradient::GradientTool::new(),
            pencil: pencil::PencilTool::new(),
            zoom: zoom_tool::ZoomTool::new(),
            hand: hand_tool::HandTool::new(),
            selection_rect: selection_rect::SelectionRectTool::new(),
            selection_ellipse: selection_ellipse::SelectionEllipseTool::new(),
            lasso: lasso::LassoTool::new(),
            polygon_lasso: polygon_lasso::PolygonLassoTool::new(),
            clone: clone_tool::CloneTool::new(),
            transform: transform_tool::TransformTool::new(),
            smart_select: smart_select::SmartSelectTool::new(),
            refine_brush: refine_brush::RefineBrushTool::new(),
            text: text_tool::TextTool::new(),
            shape: shape::ShapeTool::new(),
            repair: repair_brush::RepairBrushTool::new(),
            perspective_crop: perspective_crop::PerspectiveCropTool::new(),
            pen: pen::PenTool::new(),
            smudge: smudge::SmudgeTool::new(),
            dodge: dodge_burn::DodgeBurnTool::dodge(),
            burn: dodge_burn::DodgeBurnTool::burn(),
            patch: patch::PatchTool::new(),
        }
    }

    /// Dyn view of one tool, for id-driven dispatch. Every `ToolId` has a
    /// field, so this is total — no lookup, no failure path.
    fn tool_dyn(&self, id: ToolId) -> &dyn Tool {
        match id {
            ToolId::Brush => &self.brush,
            ToolId::Eraser => &self.eraser,
            ToolId::Pencil => &self.pencil,
            ToolId::Move => &self.move_tool,
            ToolId::Crop => &self.crop,
            ToolId::Zoom => &self.zoom,
            ToolId::Hand => &self.hand,
            ToolId::Fill => &self.fill,
            ToolId::Gradient => &self.gradient,
            ToolId::Eyedropper => &self.eyedropper,
            ToolId::SelectionRect => &self.selection_rect,
            ToolId::SelectionEllipse => &self.selection_ellipse,
            ToolId::Lasso => &self.lasso,
            ToolId::PolygonLasso => &self.polygon_lasso,
            ToolId::SmartSelect => &self.smart_select,
            ToolId::Clone => &self.clone,
            ToolId::Transform => &self.transform,
            ToolId::RefineBrush => &self.refine_brush,
            ToolId::Text => &self.text,
            ToolId::Shape => &self.shape,
            ToolId::Repair => &self.repair,
            ToolId::PerspectiveCrop => &self.perspective_crop,
            ToolId::Pen => &self.pen,
            ToolId::Smudge => &self.smudge,
            ToolId::Dodge => &self.dodge,
            ToolId::Burn => &self.burn,
            ToolId::Patch => &self.patch,
        }
    }

    fn tool_dyn_mut(&mut self, id: ToolId) -> &mut dyn Tool {
        match id {
            ToolId::Brush => &mut self.brush,
            ToolId::Eraser => &mut self.eraser,
            ToolId::Pencil => &mut self.pencil,
            ToolId::Move => &mut self.move_tool,
            ToolId::Crop => &mut self.crop,
            ToolId::Zoom => &mut self.zoom,
            ToolId::Hand => &mut self.hand,
            ToolId::Fill => &mut self.fill,
            ToolId::Gradient => &mut self.gradient,
            ToolId::Eyedropper => &mut self.eyedropper,
            ToolId::SelectionRect => &mut self.selection_rect,
            ToolId::SelectionEllipse => &mut self.selection_ellipse,
            ToolId::Lasso => &mut self.lasso,
            ToolId::PolygonLasso => &mut self.polygon_lasso,
            ToolId::SmartSelect => &mut self.smart_select,
            ToolId::Clone => &mut self.clone,
            ToolId::Transform => &mut self.transform,
            ToolId::RefineBrush => &mut self.refine_brush,
            ToolId::Text => &mut self.text,
            ToolId::Shape => &mut self.shape,
            ToolId::Repair => &mut self.repair,
            ToolId::PerspectiveCrop => &mut self.perspective_crop,
            ToolId::Pen => &mut self.pen,
            ToolId::Smudge => &mut self.smudge,
            ToolId::Dodge => &mut self.dodge,
            ToolId::Burn => &mut self.burn,
            ToolId::Patch => &mut self.patch,
        }
    }

    pub fn select(&mut self, id: ToolId) {
        self.active = id;
        self.last_event = None;
        self.remember_group_tool(id);
    }

    pub fn select_group(&mut self, group: &[ToolId]) {
        let id = self.preferred_in_group(group);
        self.select(id);
    }

    pub fn preferred_in_group(&self, group: &[ToolId]) -> ToolId {
        if group.is_empty() {
            return self.active_id();
        }
        let stored = self
            .group_preferences
            .iter()
            .find_map(|(key, id)| (*key == group[0]).then_some(*id))
            .unwrap_or_else(|| default_group_tool(group));
        if group.contains(&stored) {
            stored
        } else {
            default_group_tool(group)
        }
    }

    pub fn group_preferences(&self) -> Vec<ToolId> {
        self.group_preferences.iter().map(|(_, id)| *id).collect()
    }

    fn remember_group_tool(&mut self, id: ToolId) {
        if let Some(group) = group_for(id) {
            if let Some((_, stored)) = self
                .group_preferences
                .iter_mut()
                .find(|(key, _)| *key == group[0])
            {
                *stored = id;
            } else {
                self.group_preferences.push((group[0], id));
            }
        }
    }

    /// Cycle through a group of tools (by convention — pressing the same shortcut rotates).
    /// If current tool is in the group, advance to next; otherwise select the first in group.
    pub fn cycle_group(&mut self, group: &[ToolId]) {
        if group.is_empty() {
            return;
        }
        let current = self.active_id();
        let next = if let Some(pos) = group.iter().position(|&id| id == current) {
            group[(pos + 1) % group.len()]
        } else {
            group[0]
        };
        self.select(next);
    }

    pub fn active_id(&self) -> ToolId {
        self.active
    }

    pub fn cursor_size(&self) -> f32 {
        self.tool_dyn(self.active).cursor_size()
    }

    pub fn on_press(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        let id = self.active;
        let resp = self.tool_dyn_mut(id).on_press(event, ctx);
        self.last_event = Some(event);
        resp
    }

    pub fn on_drag(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        let id = self.active;
        let prev = self.last_event.unwrap_or(event);
        let resp = self.tool_dyn_mut(id).on_drag(event, &prev, ctx);
        self.last_event = Some(event);
        resp
    }

    pub fn on_release(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        let id = self.active;
        let resp = self.tool_dyn_mut(id).on_release(event, ctx);
        self.last_event = None;
        resp
    }

    pub fn active_on_cancel(&mut self) {
        let id = self.active;
        self.tool_dyn_mut(id).on_cancel();
    }

    pub fn active_on_confirm(&mut self, ctx: &mut ToolCtx) {
        let id = self.active;
        self.tool_dyn_mut(id).on_confirm(ctx);
    }

    pub fn brush(&self) -> &brush::BrushTool {
        &self.brush
    }
    pub fn brush_mut(&mut self) -> &mut brush::BrushTool {
        &mut self.brush
    }
    pub fn eraser(&self) -> &eraser::EraserTool {
        &self.eraser
    }
    pub fn eraser_mut(&mut self) -> &mut eraser::EraserTool {
        &mut self.eraser
    }
    pub fn crop(&self) -> &crop::CropTool {
        &self.crop
    }
    pub fn crop_mut(&mut self) -> &mut crop::CropTool {
        &mut self.crop
    }
    pub fn fill(&self) -> &fill::FillTool {
        &self.fill
    }
    pub fn fill_mut(&mut self) -> &mut fill::FillTool {
        &mut self.fill
    }
    pub fn gradient(&self) -> &gradient::GradientTool {
        &self.gradient
    }
    pub fn gradient_mut(&mut self) -> &mut gradient::GradientTool {
        &mut self.gradient
    }
    pub fn eyedropper(&self) -> &eyedropper::EyedropperTool {
        &self.eyedropper
    }
    pub fn eyedropper_mut(&mut self) -> &mut eyedropper::EyedropperTool {
        &mut self.eyedropper
    }
    pub fn move_tool(&self) -> &move_tool::MoveTool {
        &self.move_tool
    }
    pub fn move_tool_mut(&mut self) -> &mut move_tool::MoveTool {
        &mut self.move_tool
    }
    pub fn pencil(&self) -> &pencil::PencilTool {
        &self.pencil
    }
    pub fn pencil_mut(&mut self) -> &mut pencil::PencilTool {
        &mut self.pencil
    }
    pub fn clone_tool(&self) -> &clone_tool::CloneTool {
        &self.clone
    }
    pub fn clone_tool_mut(&mut self) -> &mut clone_tool::CloneTool {
        &mut self.clone
    }
    pub fn selection_rect(&self) -> &selection_rect::SelectionRectTool {
        &self.selection_rect
    }
    pub fn selection_rect_mut(&mut self) -> &mut selection_rect::SelectionRectTool {
        &mut self.selection_rect
    }
    pub fn selection_ellipse(&self) -> &selection_ellipse::SelectionEllipseTool {
        &self.selection_ellipse
    }
    pub fn lasso(&self) -> &lasso::LassoTool {
        &self.lasso
    }
    pub fn lasso_mut(&mut self) -> &mut lasso::LassoTool {
        &mut self.lasso
    }
    pub fn polygon_lasso(&self) -> &polygon_lasso::PolygonLassoTool {
        &self.polygon_lasso
    }
    pub fn polygon_lasso_mut(&mut self) -> &mut polygon_lasso::PolygonLassoTool {
        &mut self.polygon_lasso
    }
    pub fn smart_select(&self) -> &smart_select::SmartSelectTool {
        &self.smart_select
    }
    pub fn smart_select_mut(&mut self) -> &mut smart_select::SmartSelectTool {
        &mut self.smart_select
    }
    pub fn wand(&self) -> &smart_select::SmartSelectTool {
        &self.smart_select
    }
    pub fn wand_mut(&mut self) -> &mut smart_select::SmartSelectTool {
        &mut self.smart_select
    }
    pub fn refine_brush(&self) -> &refine_brush::RefineBrushTool {
        &self.refine_brush
    }
    pub fn refine_brush_mut(&mut self) -> &mut refine_brush::RefineBrushTool {
        &mut self.refine_brush
    }
    pub fn shape(&self) -> &shape::ShapeTool {
        &self.shape
    }
    pub fn shape_mut(&mut self) -> &mut shape::ShapeTool {
        &mut self.shape
    }
    pub fn healing(&self) -> &repair_brush::RepairBrushTool {
        &self.repair
    }
    pub fn healing_mut(&mut self) -> &mut repair_brush::RepairBrushTool {
        &mut self.repair
    }
    pub fn perspective_crop(&self) -> &perspective_crop::PerspectiveCropTool {
        &self.perspective_crop
    }
    pub fn perspective_crop_mut(&mut self) -> &mut perspective_crop::PerspectiveCropTool {
        &mut self.perspective_crop
    }
    pub fn pen(&self) -> &pen::PenTool {
        &self.pen
    }
    pub fn pen_mut(&mut self) -> &mut pen::PenTool {
        &mut self.pen
    }
    pub fn smudge(&self) -> &smudge::SmudgeTool {
        &self.smudge
    }
    pub fn smudge_mut(&mut self) -> &mut smudge::SmudgeTool {
        &mut self.smudge
    }

    /// The active Dodge/Burn tool (two independent instances of one type).
    /// Falls back to Dodge when neither is active (values just aren't shown).
    pub fn dodge_burn(&self) -> &dodge_burn::DodgeBurnTool {
        if self.active_id() == ToolId::Burn {
            &self.burn
        } else {
            &self.dodge
        }
    }
    pub fn dodge_burn_mut(&mut self) -> &mut dodge_burn::DodgeBurnTool {
        if self.active_id() == ToolId::Burn {
            &mut self.burn
        } else {
            &mut self.dodge
        }
    }

    pub fn patch(&self) -> &patch::PatchTool {
        &self.patch
    }
    pub fn patch_mut(&mut self) -> &mut patch::PatchTool {
        &mut self.patch
    }

    /// The Clone-Stamp-engine tool that is currently active (Clone or
    /// Repair Brush share the same `CloneTool` core). Used by the UI to read
    /// and write the active tool's brush settings. Falls back to Clone when
    /// neither is active (values just aren't shown then).
    pub fn clone_like(&self) -> &clone_tool::CloneTool {
        if self.active_id() == ToolId::Repair {
            &self.repair.0
        } else {
            &self.clone
        }
    }
    pub fn clone_like_mut(&mut self) -> &mut clone_tool::CloneTool {
        if self.active_id() == ToolId::Repair {
            &mut self.repair.0
        } else {
            &mut self.clone
        }
    }
}

impl Default for ToolManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::ToolId;

    #[test]
    fn cmyk_allows_non_pixel_tools_and_blocks_writers() {
        // Selection / view / move / crop + the ink-native brushes are allowed.
        for t in [
            ToolId::Brush,
            ToolId::Pencil,
            ToolId::Eraser,
            ToolId::Shape,
            ToolId::Move,
            ToolId::Crop,
            ToolId::Zoom,
            ToolId::Hand,
            ToolId::Eyedropper,
            ToolId::SelectionRect,
            ToolId::SelectionEllipse,
            ToolId::Lasso,
            ToolId::PolygonLasso,
            ToolId::SmartSelect,
            ToolId::RefineBrush,
        ] {
            assert!(
                t.allowed_in_cmyk(),
                "{} should be allowed in CMYK",
                t.name()
            );
        }
        // Tools with no ink path in v1 stay blocked so they can't drop the ink
        // plane through get_tile_mut and desync the document.
        for t in [
            ToolId::Smudge,
            ToolId::Dodge,
            ToolId::Burn,
            ToolId::Text,
            ToolId::Repair,
            ToolId::Patch,
            ToolId::Transform,
            ToolId::PerspectiveCrop,
            ToolId::Pen,
        ] {
            assert!(
                !t.allowed_in_cmyk(),
                "{} should be blocked in CMYK",
                t.name()
            );
        }
    }
}

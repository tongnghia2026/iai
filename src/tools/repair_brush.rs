// Repair Brush — a separate toolbar tool that reuses the Clone engine with
// healing always on. It samples like the Clone (Alt+click for a source) but
// colour-matches the cloned texture to the destination so blemishes melt away.
// A "Spot Heal" option turns on 1-click auto-sourcing (no Alt+click needed).
//
// Implemented as a thin newtype over `CloneTool` so all the dab/stroke/heal
// logic lives in one place; this wrapper only changes the tool identity.

use super::clone_tool::CloneTool;
use super::{PointerEvent, Tool, ToolCtx, ToolResponse};

pub struct RepairBrushTool(pub CloneTool);

impl RepairBrushTool {
    pub fn new() -> Self {
        let mut core = CloneTool::new();
        core.heal_mode = true;
        core.spot_mode = true;
        Self(core)
    }
}

impl Default for RepairBrushTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for RepairBrushTool {
    fn id(&self) -> &'static str {
        "healing"
    }
    fn name(&self) -> &str {
        "Repair Brush"
    }
    fn shortcut(&self) -> Option<char> {
        Some('j')
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::Repair
    }
    fn paints(&self) -> bool {
        true
    }
    fn cursor_size(&self) -> f32 {
        self.0.size
    }

    fn on_press(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        self.0.on_press(event, ctx)
    }
    fn on_drag(
        &mut self,
        event: PointerEvent,
        prev: &PointerEvent,
        ctx: &mut ToolCtx,
    ) -> ToolResponse {
        self.0.on_drag(event, prev, ctx)
    }
    fn on_release(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        self.0.on_release(event, ctx)
    }
}

use super::{PointerEvent, Tool, ToolCtx, ToolResponse};

pub struct HandTool {
    pub is_panning: bool,
}

impl HandTool {
    pub fn new() -> Self {
        Self { is_panning: false }
    }
}

impl Tool for HandTool {
    fn id(&self) -> &'static str {
        "hand"
    }
    fn name(&self) -> &str {
        "Hand"
    }
    fn shortcut(&self) -> Option<char> {
        Some('H')
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::Hand
    }

    fn on_press(&mut self, _event: PointerEvent, _ctx: &mut ToolCtx) -> ToolResponse {
        self.is_panning = true;
        ToolResponse::none()
    }

    fn on_drag(
        &mut self,
        _event: PointerEvent,
        _prev: &PointerEvent,
        _ctx: &mut ToolCtx,
    ) -> ToolResponse {
        ToolResponse::none()
    }

    fn on_release(&mut self, _event: PointerEvent, _ctx: &mut ToolCtx) -> ToolResponse {
        self.is_panning = false;
        ToolResponse::none()
    }
}

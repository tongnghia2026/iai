#![allow(dead_code)]
use super::{PointerEvent, Tool, ToolCtx, ToolResponse};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ZoomDirection {
    In,
    Out,
}

pub struct ZoomTool {
    pub direction: ZoomDirection,
    pub step: f32,
}

impl ZoomTool {
    pub fn new() -> Self {
        Self {
            direction: ZoomDirection::In,
            step: 1.5,
        }
    }

    pub fn zoom_in(&self) -> f32 {
        self.step
    }
    pub fn zoom_out(&self) -> f32 {
        1.0 / self.step
    }
}

impl Tool for ZoomTool {
    fn id(&self) -> &'static str {
        "zoom"
    }
    fn name(&self) -> &str {
        "Zoom"
    }
    fn shortcut(&self) -> Option<char> {
        Some('Z')
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::Zoom
    }

    fn on_press(&mut self, _event: PointerEvent, _ctx: &mut ToolCtx) -> ToolResponse {
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
        ToolResponse::none()
    }
}

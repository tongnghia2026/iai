//! The Node tool (direct-selection) — edits the anchor points of a Path layer.
//!
//! Like the Shape tool, this type is a thin identity marker: the actual editing
//! (hit-testing nodes/segments, moving/inserting/deleting anchors, the on-canvas
//! overlay, live re-raster and the `ReplacePathGeometry` undo entry) lives at the
//! App level in `src/app/node_ops.rs`, because it needs the gateway, the overlay
//! view-model and screen/zoom context. Pointer events for this tool are routed
//! there directly (see `input/pointer.rs`), so the trait callbacks stay no-ops.

use super::{PointerEvent, Tool, ToolCtx, ToolResponse};

pub struct NodeTool;

impl NodeTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for NodeTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for NodeTool {
    fn id(&self) -> &'static str {
        "node"
    }
    fn name(&self) -> &str {
        "Node"
    }
    fn shortcut(&self) -> Option<char> {
        Some('A')
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::Node
    }

    // The App drives node editing directly; these never fire for the Node tool.
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

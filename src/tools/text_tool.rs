// Type tool. The actual editing flow (create/edit a text layer, the egui editing
// overlay, rasterization on commit) is driven at the App level in
// `src/app/text_ops.rs`, so this tool itself is a thin identity stub — it only
// needs to exist so the toolbar, shortcut and cursor switching work.

use super::{Tool, ToolId};

pub struct TextTool;

impl TextTool {
    pub fn new() -> Self {
        TextTool
    }
}

impl Tool for TextTool {
    fn id(&self) -> &'static str {
        "text"
    }
    fn name(&self) -> &str {
        "Type"
    }
    fn shortcut(&self) -> Option<char> {
        Some('T')
    }
    fn tool_id(&self) -> ToolId {
        ToolId::Text
    }
}

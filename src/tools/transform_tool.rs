// TransformTool — placeholder while Ctrl+T free-transform is active.
//
// Pointer events for this tool are intercepted BEFORE dispatch in input.rs:
// App::transform_on_press/drag/release are called directly there.
// This struct only exists so ToolManager recognises ToolId::Transform.

use super::Tool;

pub struct TransformTool;

impl TransformTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for TransformTool {
    fn id(&self) -> &'static str {
        "transform"
    }
    fn name(&self) -> &str {
        "Free Transform"
    }
    fn shortcut(&self) -> Option<char> {
        None
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::Transform
    }
}

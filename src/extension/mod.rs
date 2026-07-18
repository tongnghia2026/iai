//! Internal extension interfaces — the seams IAI's own features plug into.
//!
//! # Scope: compile-time and internal only
//!
//! "Extension" here means an in-tree feature implementing an in-tree trait,
//! wired up at compile time. This is deliberately NOT a plugin SDK:
//!
//! - No dynamic loading, no `dlopen`/DLL discovery.
//! - No stable ABI, and no compatibility promise to third parties.
//! - Every trait here may change shape in any commit; the only implementors
//!   are inside this repository and get updated in the same change.
//!
//! It exists to keep feature code out of `core`, not to expose an API.
//!
//! # Adding a tool
//!
//! Create `src/tools/<name>.rs`, implement [`tool::Tool`], and register it in
//! `tools::ToolManager::new`. Nothing in `core` needs to change.
//!
//! # Dependency direction
//!
//! ```text
//! core  <-  extension  <-  tools
//! ```
//!
//! `extension` may depend on `core` (documents, canvas, selection) but never on
//! the concrete feature modules that implement its traits — hence [`tool::ToolId`]
//! lives here alongside the trait that returns it, rather than in `tools`.

pub mod tool;

pub use tool::{KeyEvent, PointerEvent, Tool, ToolCtx, ToolCursor, ToolId, ToolResponse};

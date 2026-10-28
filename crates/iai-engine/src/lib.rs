//! The iAi engine: the command registry, background jobs and the one call
//! the GUI, scripts and AI agents all go through, `Session::execute`.
//! Plan: docs/planning/KIEN_TRUC_AGENT_NATIVE_2026-10-14.md.
#![forbid(unsafe_code)]
// Commands read untrusted params: indices and arithmetic are checked.
#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

mod access;
pub mod cmds;
pub mod command;
pub mod error;
pub mod events;
pub mod hunt;
pub mod jobs;
pub mod policy;
pub mod schema_check;
pub mod session;
pub mod workspace;

pub use access::layer_index;
pub use command::{
    Apply, Command, CommandInfo, CommandMeta, DynCommand, Effect, Job, Plan, Registry, Validate,
    Work,
};
pub use error::{CmdError, ErrorCode};
pub use events::EngineEvent;
pub use jobs::{JobCtx, JobId, JobState, catching_panics};
pub use policy::{Policy, Source};
pub use session::{BatchFailure, BatchReply, CommandCall, Engine, MAX_BATCH, Reply, Session};
pub use workspace::{HeadlessWorkspace, Tabs, Workspace};

#[cfg(test)]
mod tests;

//! How AI agents and scripts reach iAi.
//!
//! - [`mcp`]: a Model Context Protocol server over stdio (JSON-RPC 2.0, one
//!   message a line): every command of the engine as a tool, its params'
//!   JSON Schema as the tool's input schema.
//! - [`host`]: where the commands run — an engine of its own with no window
//!   ([`host::LocalHost`]), or the app the person has open
//!   ([`bridge::BridgeHost`]).
//! - [`gate`]: what an agent was granted — effects, the folders it may read
//!   and write, sizes — checked before a call reaches the host.
//! - [`bridge`]: the app's side of the link to a running window (loopback
//!   TCP, a 256-bit token) and the client that uses it.
//! - [`script`]: a script of calls (`iai.exe run`), longer than a batch when
//!   it is not all or nothing.
//!
//! Plan: docs/planning/KIEN_TRUC_AGENT_NATIVE_2026-10-14.md (Phase 4).
#![forbid(unsafe_code)]
// Everything here reads what another program sent.
#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

pub mod bridge;
pub mod gate;
pub mod host;
pub mod jsonrpc;
pub mod mcp;
pub mod schema;
pub mod script;

pub use gate::{Gate, Roots};
pub use host::{Host, LocalHost};

/// Limits on what an agent may send or be sent.
pub mod limits {
    /// The longest message line read, in bytes.
    pub const MAX_LINE: usize = 1 << 20;
    /// The largest answer sent, in bytes.
    pub const MAX_RESULT: usize = 8 << 20;
    /// The most calls in one batch.
    pub const MAX_BATCH: usize = iai_engine::MAX_BATCH;
    /// The most calls in one script that is not atomic (run a batch at a time).
    pub const MAX_SCRIPT: usize = 100_000;
    /// The most links to a running app at once.
    pub const MAX_LINKS: usize = 16;
}

#[cfg(test)]
mod tests;

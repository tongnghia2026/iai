//! IAI — a GPU-accelerated image editor.
//!
//! Everything real lives in this library; `src/main.rs` is a thin process entry
//! point that installs the panic handler and calls [`bootstrap::run`]. Tests
//! compile against this library, so they never depend on the binary's module
//! tree.
//!
//! # Dependency direction
//!
//! ```text
//! main (bin)
//!   └─> app  ──> ui, tools, formats, gpu, file_io
//!         └─> core  (document/raster core — depends on none of the above)
//! ```
//!
//! `core` is the bottom layer: document, canvas, layer, tile, selection,
//! command/history and raster math. It must not reach up into `app`, `ui`,
//! winit, wgpu or the format registry.

// The AI gateway (`iai.exe mcp|run`, the Antigravity relay) is the Pro
// pack's; the free build only says so (`no_gateway`).
#[cfg(feature = "pro")]
#[rustfmt::skip]
pub mod agent;
#[cfg(feature = "pro")]
#[rustfmt::skip]
pub mod antigravity;
pub mod app;
pub mod author_mail;
pub mod author_news;
pub mod bootstrap;
pub mod core;
pub mod crash;
pub use iai_diag as diag;
pub mod diag_ui;
pub mod extension;
pub mod file_io;
pub use iai_formats as formats;
pub mod gpu;
#[cfg(not(feature = "pro"))]
pub mod no_gateway;
#[cfg(feature = "pro")]
pub use iai_pro::pro;
pub mod tools;
pub mod ui;
pub mod update;
#[cfg(windows)]
pub use iai_platform::windows_integration;

// `crate::log_crash` is called from deep subsystems (GPU device-lost recovery);
// keep the short root path working now that the implementation lives in `crash`.
pub use crash::log_crash;

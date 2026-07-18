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

pub mod app;
pub mod bootstrap;
pub mod core;
pub mod crash;
pub mod event_bus;
pub mod extension;
pub mod file_io;
pub mod formats;
pub mod gpu;
pub mod tools;
pub mod ui;

// `crate::log_crash` is called from deep subsystems (GPU device-lost recovery);
// keep the short root path working now that the implementation lives in `crash`.
pub use crash::log_crash;

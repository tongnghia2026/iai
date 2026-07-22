// Core module — declares all sub-modules.
//
// Re-exports here keep only what's actually imported via the short path
// (crate::core::X instead of crate::core::sub_module::X).
// Everything else is imported directly from its sub-module to avoid unused-import warnings.

pub mod ai;

pub mod blend;
pub mod canvas;
pub mod cat16;
pub mod channels;
pub mod cms;
pub mod color;
pub mod command;
pub mod command_vector;
pub mod develop;
pub mod develop_scene;
pub mod gateway;
pub mod geometry;
pub mod hw;
pub mod layer;
pub mod selection;
pub mod shape;
pub mod smart_fill;
pub mod ucs;
pub mod units;
pub mod vector;
pub mod warp;

pub mod document;
pub mod filters;
pub mod imposition;
pub mod lama;
pub mod presets;
pub mod print;
pub mod print_gdi;
pub mod select_subject;
pub mod snapping;
pub mod text;
pub mod tile;

pub use canvas::{Canvas, DEFAULT_H, DEFAULT_W};

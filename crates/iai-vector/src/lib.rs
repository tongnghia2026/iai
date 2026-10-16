//! Vector layers for iAi: boolean operations, stroking, curve fitting,
//! hit-testing, snapping, mask tracing, text to paths, the display list and
//! the CPU rasteriser, and the undo steps that edit vector layers.

#![forbid(unsafe_code)]

pub mod command_vector;
pub mod vector;

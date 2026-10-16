//! Raster storage for iAi: 256-pixel tiles shared copy-on-write, blend
//! modes, the 8/16-bit dithering, fast maths and the mask box filter.

#![forbid(unsafe_code)]

pub mod blend;
pub mod box_filter;
pub mod fast_math;
pub mod tile;

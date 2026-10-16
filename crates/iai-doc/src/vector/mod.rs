//! The vector layers as data: paths, styles, colours and objects, and the
//! geometry that belongs to them (paths from shapes, flattening, brush
//! strokes). Boolean operations, stroking and rasterising are `iai-vector`.

pub mod brush;
pub mod color;
pub mod flatten;
pub mod from_shape;
pub mod object;
pub mod path;
pub mod style;

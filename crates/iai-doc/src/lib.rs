//! The document's data for iAi: the layer stack and its layers, the
//! selection, channels, pages, the document palette, text and shape data,
//! the portrait recipe, and the vector layers' paths, styles and colours with
//! the geometry they need (building paths from shapes, flattening, brush
//! strokes). Plain data: no history, no rendering beyond a shape's own pixels.

#![forbid(unsafe_code)]

pub mod channels;
pub mod connector;
pub mod layer;
pub mod layer_clip;
pub mod model;
pub mod page;
pub mod palette;
pub mod portrait_recipe;
pub mod selection;
pub mod shape;
pub mod text_document;
pub mod vector;

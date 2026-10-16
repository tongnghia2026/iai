//! Plane geometry for iAi: points, rectangles, affine transforms, physical
//! units (px / mm / cm / pt at a resolution) and 1-D snapping. No document
//! types; everything above builds on these.
#![forbid(unsafe_code)]

pub mod geometry;
pub mod snapping;
pub mod units;
pub mod vector;

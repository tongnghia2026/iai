//! Image operations for iAi: filters, Refine Selection, Smart Select,
//! warps, scan clean-up, output sharpening, the Smart brush and finding a
//! card in a photo. No machine learning (that is `iai-ai`).

#![forbid(unsafe_code)]

pub mod card_find;
pub mod filters;
pub mod output_sharpen;
pub mod quick_select;
pub mod refine;
pub mod scan_cleanup;
pub mod smart_brush;
pub mod warp;

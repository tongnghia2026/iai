//! Image operations for iAi: filters, Refine Selection, Smart Select,
//! warps, scan clean-up, output sharpening and the Smart brush. No machine
//! learning (that is `iai-ai`).

#![forbid(unsafe_code)]

pub mod filters;
pub mod output_sharpen;
pub mod quick_select;
pub mod refine;
pub mod scan_cleanup;
pub mod smart_brush;
pub mod warp;

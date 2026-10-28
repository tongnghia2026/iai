//! The canvas iAi edits: a page's layers, selection and history with its
//! composited pixels and the operations on it (raster, selection, layer,
//! geometry, colour mode), the open document of one or more pages, the disk
//! spill of RAW masters, and building PDFs (print and export).

#![forbid(unsafe_code)]

pub mod canvas;
pub mod document;
pub mod free_transform;
pub mod powerclip;
pub mod print;
pub mod raw_spill;
pub mod shaping;

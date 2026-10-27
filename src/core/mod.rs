// Core module — declares all sub-modules.
//
// Re-exports here keep only what's actually imported via the short path
// (crate::core::X instead of crate::core::sub_module::X).
// Everything else is imported directly from its sub-module to avoid unused-import warnings.

pub use iai_ai::ai;

pub use iai_canvas::canvas;
pub use iai_canvas::raw_spill;
pub use iai_color::cms;
pub use iai_color::color;
pub use iai_color::perceptual_color;
pub use iai_develop::camera_profile;
pub use iai_develop::cat16;
pub use iai_develop::color_reference;
pub use iai_develop::develop;
pub use iai_develop::develop_scene;
pub use iai_develop::develop2;
pub use iai_doc::channels;
pub use iai_doc::connector;
pub use iai_doc::layer;
pub use iai_doc::layer_clip;
pub use iai_doc::model;
pub use iai_doc::page;
pub use iai_doc::palette;
pub use iai_doc::portrait_recipe;
pub use iai_doc::selection;
pub use iai_geom::geometry;
pub use iai_history::command;
pub use iai_history::command_clip;
pub use iai_history::command_palette;
pub use iai_history::gateway;
pub use iai_imaging::output_sharpen;
pub use iai_imaging::quick_select;
pub use iai_imaging::refine;
pub use iai_imaging::scan_cleanup;
pub use iai_platform::atomic_file;
pub use iai_platform::hw;
pub use iai_platform::mem_report;
#[cfg(feature = "pro")]
pub use iai_pro::core::id_photo;
#[cfg(feature = "pro")]
pub use iai_pro::core::portrait;
pub use iai_raster::blend;
pub use iai_raster::box_filter;
pub use iai_vector::command_vector;
pub mod settings;
pub use iai_ai::smart_fill;
pub use iai_color::ucs;
pub use iai_doc::shape;
pub use iai_geom::units;
pub use iai_imaging::smart_brush;
pub use iai_vector::vector;
pub mod vietqr;
pub use iai_color::working_color;
pub use iai_imaging::warp;

pub mod canvas_editor_conversion;
pub use iai_ai::lama;
pub use iai_canvas::document;
pub use iai_color::gamut_map;
pub use iai_imaging::filters;
pub use iai_layout::mail_merge;
#[cfg(feature = "pro")]
pub use iai_pro::core::imposition;
pub use iai_raster::fast_math;
pub mod prefs_store;
pub mod presets;
pub use crate::formats::svg;
pub use iai_ai::select_subject;
pub use iai_canvas::print;
pub use iai_doc::text_document;
pub use iai_geom::snapping;
pub use iai_print::print_gdi;
pub use iai_print::printers;
pub use iai_raster::tile;
pub use iai_text::text;
pub use iai_text::text_layout;

pub use canvas::{Canvas, DEFAULT_H, DEFAULT_W};

//! Undo for iAi: the `Command` an edit is recorded as, the history that
//! keeps them within a memory budget, the tile deltas of a stroke, the
//! gateway's change report, and the undo steps of clipping and the palette.

#![forbid(unsafe_code)]

pub mod command;
pub mod command_clip;
pub mod command_palette;
pub mod gateway;

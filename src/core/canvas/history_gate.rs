//! The history vault — the one object allowed to add to the undo stack.
//!
//! `CommandHistory` lives *inside* this type behind a private field, so no
//! sibling module of `canvas` (layer_ops, selection_ops, …) can reach `.push`.
//! The single writing door is [`record`](HistoryGate::record); every recorded
//! mutation therefore passes through
//! [`Canvas::record`](super::Canvas::record) /
//! [`Canvas::record_as`](super::Canvas::record_as) /
//! [`Canvas::execute`](super::Canvas::execute). That "one door" is now a
//! compiler guarantee, not a convention: the field is unreachable from outside,
//! and the only mutator exposed here appends to history.
//!
//! Everything else forwarded below is a read or a history-control operation
//! (undo/redo, grouping, save checkpoint, clear) — none of them can smuggle a
//! command onto the stack outside of `record`.

use crate::core::command::{Command, CommandHistory, EditContext, HistoryEntry};

#[derive(Default)]
pub struct HistoryGate {
    history: CommandHistory,
}

impl HistoryGate {
    pub fn new() -> Self {
        Self {
            history: CommandHistory::new(),
        }
    }

    /// The ONE way to add a command to history. Returns the post-change
    /// `(revision, is_dirty)` so the gateway can build a `ChangeOutcome` without
    /// re-reading state.
    pub fn record(&mut self, cmd: Box<dyn Command>) -> (u64, bool) {
        self.history.push(cmd);
        (self.history.revision(), self.history.is_dirty())
    }

    pub fn undo(&mut self, ctx: &mut EditContext) -> bool {
        self.history.undo(ctx)
    }

    pub fn redo(&mut self, ctx: &mut EditContext) -> bool {
        self.history.redo(ctx)
    }

    pub fn revision(&self) -> u64 {
        self.history.revision()
    }

    pub fn is_dirty(&self) -> bool {
        self.history.is_dirty()
    }

    pub fn mark_saved(&mut self) {
        self.history.mark_saved();
    }

    pub fn mark_saved_state_unreachable(&mut self) {
        self.history.mark_saved_state_unreachable();
    }

    pub fn undo_count(&self) -> usize {
        self.history.undo_count()
    }

    /// Exact billed history bytes — Memory Milestone M0 accounting.
    pub fn total_memory_bytes(&self) -> usize {
        self.history.total_memory_bytes()
    }

    pub fn redo_count(&self) -> usize {
        self.history.redo_count()
    }

    pub fn history_entries(&self) -> Vec<HistoryEntry> {
        self.history.history_entries()
    }

    pub fn begin_group(&mut self, label: &str) {
        self.history.begin_group(label);
    }

    pub fn end_group(&mut self) {
        self.history.end_group();
    }

    pub fn clear(&mut self) {
        self.history.clear();
    }
}

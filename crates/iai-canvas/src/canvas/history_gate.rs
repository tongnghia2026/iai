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

use iai_history::command::{Command, CommandHistory, EditContext, HistoryEntry};

#[derive(Default)]
pub struct HistoryGate {
    history: CommandHistory,
    /// The names of the steps recorded since they were last taken, for the
    /// engine to hear of every change, whoever made it.
    recorded: Vec<String>,
}

/// Steps kept for the taker at most; older ones are dropped.
const RECORDED_KEPT: usize = 64;

impl HistoryGate {
    pub fn new() -> Self {
        Self {
            history: CommandHistory::new(),
            recorded: Vec::new(),
        }
    }

    /// The ONE way to add a command to history. Returns the post-change
    /// `(revision, is_dirty)` so the gateway can build a `ChangeOutcome` without
    /// re-reading state.
    pub fn record(&mut self, cmd: Box<dyn Command>) -> (u64, bool) {
        if self.recorded.len() >= RECORDED_KEPT {
            self.recorded.remove(0);
        }
        self.recorded.push(cmd.label().to_string());
        self.history.push(cmd);
        (self.history.revision(), self.history.is_dirty())
    }

    /// Every step of this history, oldest first, taken out of it: a page-wide
    /// change made on a copy of a page (see `Canvas::page_work_copy`) goes back
    /// with the steps it recorded there. They are recorded again through
    /// [`record`](Self::record) on the page, so the one door still holds.
    pub fn take_steps(&mut self) -> Vec<Box<dyn Command>> {
        self.recorded.clear();
        self.history.take_undo_steps()
    }

    /// The names of the steps recorded since the last call, oldest first.
    pub fn take_recorded(&mut self) -> Vec<String> {
        std::mem::take(&mut self.recorded)
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

    pub fn set_max_entries(&mut self, steps: usize) {
        self.history.set_max_entries(steps);
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

    pub fn checkpoint(&self) -> iai_history::command::Checkpoint {
        self.history.checkpoint()
    }

    pub fn rollback_to(
        &mut self,
        mark: iai_history::command::Checkpoint,
        ctx: &mut EditContext,
    ) -> bool {
        self.history.rollback_to(mark, ctx)
    }
}

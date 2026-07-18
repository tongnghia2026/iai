//! The mutation gateway: the single door every persistent document change goes
//! through.
//!
//! Before this, ~35 call sites reached into `canvas.cmd_history` and pushed, then
//! each separately remembered to bump revisions, set a dirty bool and ask for the
//! right GPU invalidation. The ones that forgot produced the bugs you would
//! predict: a frozen History panel, a document that stayed "unsaved" after undo,
//! a crop whose undo skipped the GPU resize.
//!
//! `cmd_history` is now private to [`Canvas`](super::canvas::Canvas). The only
//! ways to record a change are [`Canvas::record`](super::canvas::Canvas::record)
//! and [`Canvas::execute`](super::canvas::Canvas::execute), so the compiler —
//! not a convention — enforces that history, dirty tracking and invalidation
//! stay in step.
//!
//! Both return a [`ChangeOutcome`] describing what changed. The renderer maps
//! that to its invalidation; `core` stays free of any GPU dependency.

/// What a change means for persistence.
///
/// Categories are deliberately explicit: "does this dirty the project?" was
/// previously answered ad hoc at each call site, which is how transient preview
/// state ended up marking documents unsaved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceImpact {
    /// Real document content. Dirties the document, needs autosave, must
    /// serialize.
    PersistentDocument,
    /// View, transient selection or guides — state the format does not save.
    /// Does not dirty the project on its own.
    SessionState,
    /// Live preview (drag, scrub, transform in flight). Creates no history and
    /// no dirt until it is committed.
    PreviewTransient,
}

/// What the compositor has to redo after a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositeInvalidation {
    /// Nothing to recomposite.
    None,
    /// Only the reported dirty region changed.
    Region,
    /// The whole canvas must be recomposited (layer structure or size changed).
    Full,
}

/// A rectangle of canvas that changed, in canvas pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirtyRegion {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Typed description of what a change did — the gateway's return value.
///
/// Callers read this instead of inferring consequences from the call site.
#[derive(Debug, Clone, PartialEq)]
pub struct ChangeOutcome {
    /// Layer pixels changed.
    pub content_changed: bool,
    /// Layers were added/removed/reordered, or the canvas was resized.
    pub layer_structure_changed: bool,
    /// The selection mask or offset changed.
    pub selection_changed: bool,
    /// Layer ids the change touched. Empty means "not tracked / all".
    pub affected_layers: Vec<u32>,
    pub dirty_region: Option<DirtyRegion>,
    pub composite: CompositeInvalidation,
    pub persistence: PersistenceImpact,
    /// History stamp AFTER the change. The History panel keys its cache off
    /// this, so it can never go stale.
    pub history_revision: u64,
    /// The document holds unsaved changes, derived from the saved checkpoint.
    pub is_dirty: bool,
    /// User-facing note (e.g. an operation refused for being too large).
    pub status: Option<String>,
}

impl ChangeOutcome {
    /// A change that recorded nothing — a refused or empty operation.
    pub fn nothing(history_revision: u64, is_dirty: bool) -> Self {
        Self {
            content_changed: false,
            layer_structure_changed: false,
            selection_changed: false,
            affected_layers: Vec::new(),
            dirty_region: None,
            composite: CompositeInvalidation::None,
            persistence: PersistenceImpact::SessionState,
            history_revision,
            is_dirty,
            status: None,
        }
    }

    pub fn with_status(mut self, status: impl Into<String>) -> Self {
        self.status = Some(status.into());
        self
    }
}

/// What kind of change is being recorded. Determines the invalidation the
/// renderer performs and whether the change dirties the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// Brush/fill/filter — pixels inside existing layers.
    LayerPixels,
    /// Add/remove/reorder/merge/opacity/blend/visibility, crop/resize.
    LayerStructure,
    /// Selection mask or offset.
    Selection,
}

impl ChangeKind {
    pub(crate) fn outcome(self, history_revision: u64, is_dirty: bool) -> ChangeOutcome {
        let mut out = ChangeOutcome::nothing(history_revision, is_dirty);
        out.persistence = PersistenceImpact::PersistentDocument;
        match self {
            ChangeKind::LayerPixels => {
                out.content_changed = true;
                out.composite = CompositeInvalidation::Region;
            }
            ChangeKind::LayerStructure => {
                out.layer_structure_changed = true;
                out.composite = CompositeInvalidation::Full;
            }
            ChangeKind::Selection => {
                out.selection_changed = true;
                // The selection is not serialized by `.iai`, but selection ops
                // do enter history (so undo reaches them) and therefore move the
                // saved checkpoint. Kept as PersistentDocument to preserve the
                // behaviour the app has always had.
                out.composite = CompositeInvalidation::None;
            }
        }
        out
    }
}

/// A command failed. The document is unchanged and nothing entered history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeError {
    pub message: String,
}

impl std::fmt::Display for ChangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ChangeError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::Canvas;
    use crate::core::command::{Command, EditContext};

    struct FailingCommand;
    impl Command for FailingCommand {
        fn execute(&mut self, _ctx: &mut EditContext) -> Result<(), String> {
            Err("refused".to_string())
        }
        fn undo(&mut self, _ctx: &mut EditContext) -> Result<(), String> {
            Ok(())
        }
        fn label(&self) -> &str {
            "Failing"
        }
        fn memory_bytes(&self) -> usize {
            1
        }
    }

    fn canvas() -> Canvas {
        Canvas::from_rgba(vec![255; 4 * 4 * 4], 4, 4)
    }

    #[test]
    fn a_failed_command_neither_enters_history_nor_dirties() {
        let mut c = canvas();
        c.mark_saved();

        let err = c
            .execute(Box::new(FailingCommand), ChangeKind::LayerPixels)
            .unwrap_err();

        assert_eq!(err.message, "refused");
        assert_eq!(c.undo_count(), 0, "a failed command must not enter history");
        assert!(
            !c.is_dirty(),
            "a failed command must not dirty the document"
        );
    }

    #[test]
    fn undo_and_redo_report_the_same_invalidation_as_the_edit() {
        let mut c = canvas();
        // A real structural edit through the gateway.
        c.deselect();
        let edit_revision = c.history_revision();

        let undone = c.undo().expect("something to undo");
        assert_eq!(
            undone.composite,
            CompositeInvalidation::Full,
            "undo must invalidate at least as widely as the edit it reverses"
        );
        assert!(undone.layer_structure_changed && undone.selection_changed);
        assert_ne!(
            undone.history_revision, edit_revision,
            "undo must move the history revision so the panel cache refreshes"
        );

        let redone = c.redo().expect("something to redo");
        assert_eq!(redone.composite, undone.composite);
        assert_eq!(
            redone.layer_structure_changed,
            undone.layer_structure_changed
        );
        assert_eq!(redone.selection_changed, undone.selection_changed);
    }

    #[test]
    fn undo_with_empty_history_reports_nothing_to_do() {
        let mut c = canvas();
        assert!(c.undo().is_none());
        assert!(c.redo().is_none());
    }

    #[test]
    fn a_recorded_change_reports_dirty_and_a_fresh_revision() {
        let mut c = canvas();
        c.mark_saved();
        let before = c.history_revision();

        c.deselect();

        assert!(c.is_dirty());
        assert_ne!(c.history_revision(), before);
    }

    #[test]
    fn change_kinds_map_to_their_invalidation_and_all_persist() {
        for (kind, composite) in [
            (ChangeKind::LayerPixels, CompositeInvalidation::Region),
            (ChangeKind::LayerStructure, CompositeInvalidation::Full),
            (ChangeKind::Selection, CompositeInvalidation::None),
        ] {
            let out = kind.outcome(7, true);
            assert_eq!(out.composite, composite);
            assert_eq!(out.persistence, PersistenceImpact::PersistentDocument);
            assert_eq!(out.history_revision, 7);
            assert!(out.is_dirty);
        }
    }
}

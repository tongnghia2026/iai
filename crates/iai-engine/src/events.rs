//! What happened, for whoever shows it: the GUI re-uploads what changed to
//! the GPU and writes the status line; an agent's link turns them into
//! notifications. Commands emit them through the session; the caller takes
//! them after each call with `Engine::take_events`.

use schemars::JsonSchema;
use serde::Serialize;

use crate::jobs::JobId;
use crate::CmdError;

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EngineEvent {
    /// Pixels of the shown page changed; the page's dirty region says where.
    Pixels { doc: u32 },
    /// The selection of the shown page changed.
    Selection { doc: u32 },
    /// Layers were added, removed, reordered or restyled, or the page was
    /// resized: everything is drawn again.
    Structure { doc: u32 },
    /// A document was opened as a new tab.
    Opened { doc: u32 },
    /// Another document is shown.
    Shown { doc: u32 },
    /// Another page of the document is shown (or one was added).
    PageShown { doc: u32 },
    /// The document was written to `path`.
    Saved { doc: u32, path: String },
    /// A line for the status bar.
    Notice { vi: String, en: String },
    JobStarted {
        job: JobId,
        command: String,
        label_vi: String,
    },
    JobFinished {
        job: JobId,
        command: String,
        /// Why it failed; `None` when it was done or cancelled.
        error: Option<CmdError>,
        cancelled: bool,
    },
}

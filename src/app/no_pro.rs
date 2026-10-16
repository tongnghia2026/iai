//! The build without the Pro part (`--no-default-features`): what the rest
//! of the app asks of that part, answered by "there is none". Each of these
//! is a method the Pro part has under the same name; with it, this file is
//! not compiled.

use std::path::PathBuf;
use std::sync::Arc;

use super::state::App;
use crate::core::document::DocumentId;
use crate::ui::{DialogViewModel, DocAiBusy, SupportDialog, UiActions};

impl App {
    // A retouch previewed on the canvas: there is never one.

    pub(crate) fn portrait_under_way(&self) -> bool {
        false
    }

    pub(crate) fn portrait_yields(&self) -> bool {
        false
    }

    pub(crate) fn portrait_on_show(&self) -> bool {
        false
    }

    pub(crate) fn portrait_frees_canvas(&self) -> bool {
        false
    }

    pub(crate) fn portrait_painting(&self) -> bool {
        false
    }

    pub(crate) fn portrait_in_session(&self) -> bool {
        false
    }

    pub(crate) fn yield_portrait(&mut self) -> Option<(usize, usize)> {
        None
    }

    pub(crate) fn portrait_pressed(&mut self) {}

    pub(crate) fn portrait_tool_picked(&mut self, _crop: bool) {}

    pub(crate) fn portrait_gives_way(&mut self, _actions: &mut UiActions) {}

    pub(crate) fn portrait_brush_step(&mut self, _redo: bool) {}

    pub(crate) fn drop_portrait_preview(&mut self) {}

    pub(crate) fn forget_portrait_analysis(&mut self, _doc_id: DocumentId) {}

    // The print sheets.

    pub(crate) fn set_print_folder(&mut self, _dir: PathBuf) {}

    // The "AI" window and the Pro key: nothing asks, nothing is shown.

    pub(crate) fn handle_ai_actions(&mut self, _actions: &mut UiActions) {}

    pub(crate) fn handle_retouch_actions(&mut self, _actions: &mut UiActions) {}

    pub(crate) fn handle_pro_actions(&mut self, _actions: &mut UiActions) {}

    pub(crate) fn forget_pro_outcome(&mut self, _kind: SupportDialog) {}

    pub(crate) fn fill_retouch_view(&self, _dialogs: &mut DialogViewModel) {}

    // The work sent to Gemini / ChatGPT: no tab ever waits on any.

    pub(crate) fn doc_ai_busy(&self) -> Arc<Vec<Option<DocAiBusy>>> {
        Arc::new(self.docs.documents.iter().map(|_| None).collect())
    }

    pub(crate) fn drop_doc_ai_jobs(&mut self, _id: DocumentId) {}

    pub(crate) fn poll_pro_jobs(&mut self) {}

    pub(crate) fn pro_jobs_running(&self) -> bool {
        false
    }

    pub(crate) fn ai_api_running(&self) -> bool {
        false
    }

    pub(crate) fn ai_web_running(&self) -> bool {
        false
    }

    pub(crate) fn ext_workers_busy(&self) -> bool {
        false
    }

    pub(crate) fn ai_web_shown(&self) -> bool {
        false
    }
}

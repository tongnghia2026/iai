//! Undo and redo on the shown page of a document.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::not_enabled;
use crate::{CmdError, Command, CommandMeta, Effect, EngineEvent, Plan, Session, Validate};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StepParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// How many steps.
    #[serde(default = "one")]
    #[schemars(range(min = 1, max = 100))]
    pub steps: u32,
}

fn one() -> u32 {
    1
}

impl Validate for StepParams {}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Stepped {
    pub doc: u32,
    /// Steps actually taken (fewer when history ran out).
    pub steps: u32,
    pub can_undo: bool,
    pub can_redo: bool,
}

static UNDO: CommandMeta = CommandMeta {
    id: "edit.undo",
    label_vi: "Hoàn tác",
    label_en: "Undo",
    doc: "Undo the last change(s) on the shown page of a document.",
    effects: &[Effect::Edit],
    featured: false,
};

static REDO: CommandMeta = CommandMeta {
    id: "edit.redo",
    label_vi: "Làm lại",
    label_en: "Redo",
    doc: "Redo change(s) undone on the shown page of a document.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct Undo;
pub struct Redo;

fn step(s: &mut Session<'_>, p: StepParams, redo: bool) -> Result<Plan<Stepped>, CmdError> {
    let doc = s.doc_id(p.doc)?;
    let canvas = s.canvas_mut(Some(doc))?;
    let can = if redo {
        canvas.can_redo()
    } else {
        canvas.can_undo()
    };
    if !can {
        return Err(if redo {
            not_enabled("nothing to redo", "Không có gì để làm lại")
        } else {
            not_enabled("nothing to undo", "Không có gì để hoàn tác")
        });
    }
    let mut steps = 0u32;
    while steps < p.steps {
        let done = if redo { canvas.redo() } else { canvas.undo() };
        if done.is_none() {
            break;
        }
        steps = steps.saturating_add(1);
    }
    let (can_undo, can_redo) = (canvas.can_undo(), canvas.can_redo());
    s.emit(EngineEvent::Structure { doc });
    s.emit(EngineEvent::Selection { doc });
    Ok(Plan::Done(Stepped {
        doc,
        steps,
        can_undo,
        can_redo,
    }))
}

impl Command for Undo {
    type Params = StepParams;
    type Output = Stepped;
    fn meta(&self) -> &'static CommandMeta {
        &UNDO
    }
    fn plan(&self, s: &mut Session<'_>, p: StepParams) -> Result<Plan<Stepped>, CmdError> {
        step(s, p, false)
    }
}

impl Command for Redo {
    type Params = StepParams;
    type Output = Stepped;
    fn meta(&self) -> &'static CommandMeta {
        &REDO
    }
    fn plan(&self, s: &mut Session<'_>, p: StepParams) -> Result<Plan<Stepped>, CmdError> {
        step(s, p, true)
    }
}

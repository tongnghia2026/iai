//! What a command is — its id, labels, the effects it may have, its typed
//! params and output — and the registry that keeps commands by id, with the
//! JSON Schema of their params and output made once, when registered.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::jobs::JobCtx;
use crate::session::Session;
use crate::{CmdError, schema_check};

/// What a command may touch. A session is granted a set of them (an agent's
/// leaves out printing and the network); a command needing one it was not
/// granted is refused with `permission_denied`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// Reads the open documents.
    Query,
    /// Changes an open document, or opens a new one (one undo step).
    Edit,
    /// Reads files from disk.
    ReadFiles,
    /// Writes files to disk.
    WriteFiles,
    /// Runs a local AI model.
    LocalAi,
    /// Sends a page to a printer: paper and ink.
    Print,
    /// Talks to a server.
    Network,
}

impl Effect {
    pub const ALL: [Effect; 7] = [
        Effect::Query,
        Effect::Edit,
        Effect::ReadFiles,
        Effect::WriteFiles,
        Effect::LocalAi,
        Effect::Print,
        Effect::Network,
    ];
}

/// The fixed facts of a command.
#[derive(Debug)]
pub struct CommandMeta {
    /// Stable id, `area.verb_object`; never changes once shipped.
    pub id: &'static str,
    pub label_vi: &'static str,
    pub label_en: &'static str,
    /// What an agent reads to decide when to use it (English).
    pub doc: &'static str,
    pub effects: &'static [Effect],
    /// Offered to agents as an MCP tool of its own; the others are run
    /// through the generic `command_run` tool.
    pub featured: bool,
}

impl CommandMeta {
    /// Whether it may run on a snapshot and be taken back (`dryRun`): only
    /// commands whose every effect stays inside the session.
    pub fn can_dry_run(&self) -> bool {
        // Closing a tab is no step of any page's history: nothing takes it back.
        self.id != "doc.close"
            && self
                .effects
                .iter()
                .all(|e| matches!(e, Effect::Query | Effect::Edit | Effect::LocalAi))
    }
}

/// Rules between fields that a schema cannot say. Ranges and lengths are
/// already checked from the schema itself, and serde refuses unknown fields
/// and wrong types.
pub trait Validate {
    fn validate(&self) -> Result<(), CmdError> {
        Ok(())
    }
}

/// The commit half of a job: runs on the session's thread and makes the
/// change, as one undo step.
pub type Apply<O> = Box<dyn FnOnce(&mut Session<'_>) -> Result<O, CmdError> + Send>;

/// The long half of a job: runs on a worker thread against what the plan
/// copied out of the documents, never the session itself.
pub type Work<O> = Box<dyn FnOnce(&JobCtx) -> Result<Apply<O>, CmdError> + Send>;

/// A command that takes long (an AI model, many files): `work` runs on a
/// worker, then its `Apply` on the session's thread.
pub struct Job<O> {
    /// The document it reads and will change; one job per document at a time.
    pub doc: Option<u32>,
    /// Shown while it runs.
    pub label_vi: String,
    pub work: Work<O>,
}

/// What a command's plan gives: the result at once, or a job to run.
pub enum Plan<O> {
    Done(O),
    Job(Job<O>),
}

/// One command, typed. It never panics: every failure is a `CmdError`, and
/// a panic that slips through is caught and reported as `internal`, with the
/// document put back as it was.
pub trait Command: Send + Sync + 'static {
    type Params: DeserializeOwned + JsonSchema + Validate;
    type Output: Serialize + JsonSchema;

    fn meta(&self) -> &'static CommandMeta;

    /// Whether it can run now; explains a refusal (greys a menu item).
    fn enabled(&self, _s: &Session<'_>, _p: &Self::Params) -> Result<(), CmdError> {
        Ok(())
    }

    /// Check the documents and either do the change or return the job that
    /// will. Nothing may be changed before the last check has passed.
    fn plan(&self, s: &mut Session<'_>, p: Self::Params) -> Result<Plan<Self::Output>, CmdError>;
}

/// A command with its types erased, as the registry keeps it.
pub trait DynCommand: Send + Sync {
    fn meta(&self) -> &'static CommandMeta;
    fn params_schema(&self) -> &Value;
    fn output_schema(&self) -> &Value;
    /// Read `args` against the params' schema and type, then plan.
    fn plan_json(&self, s: &mut Session<'_>, args: Value) -> Result<Plan<Value>, CmdError>;
}

struct Typed<C> {
    cmd: C,
    params_schema: Value,
    output_schema: Value,
}

impl<C: Command> Typed<C> {
    fn read(&self, args: Value) -> Result<C::Params, CmdError> {
        let args = if args.is_null() {
            Value::Object(Default::default())
        } else {
            args
        };
        schema_check::check(&self.params_schema, &args)?;
        let params: C::Params =
            serde_json::from_value(args).map_err(|e| CmdError::bad_params(&e))?;
        params.validate()?;
        Ok(params)
    }
}

impl<C: Command> DynCommand for Typed<C> {
    fn meta(&self) -> &'static CommandMeta {
        self.cmd.meta()
    }

    fn params_schema(&self) -> &Value {
        &self.params_schema
    }

    fn output_schema(&self) -> &Value {
        &self.output_schema
    }

    fn plan_json(&self, s: &mut Session<'_>, args: Value) -> Result<Plan<Value>, CmdError> {
        let params = self.read(args)?;
        self.cmd.enabled(s, &params)?;
        Ok(match self.cmd.plan(s, params)? {
            Plan::Done(out) => Plan::Done(to_json(out)?),
            Plan::Job(job) => Plan::Job(Job {
                doc: job.doc,
                label_vi: job.label_vi,
                work: Box::new(move |cx| {
                    let apply = (job.work)(cx)?;
                    Ok(
                        Box::new(move |s: &mut Session<'_>| apply(s).and_then(to_json))
                            as Apply<Value>,
                    )
                }),
            }),
        })
    }
}

pub(crate) fn to_json<O: Serialize>(out: O) -> Result<Value, CmdError> {
    serde_json::to_value(out).map_err(|e| CmdError::internal(format!("output: {e}")))
}

fn schema_of<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).unwrap_or(Value::Null)
}

/// One command as `command_list` describes it to an agent.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandInfo {
    pub id: String,
    pub label_vi: String,
    pub label_en: String,
    pub doc: String,
    pub effects: Vec<Effect>,
    pub featured: bool,
    pub can_dry_run: bool,
    pub params_schema: Value,
    pub output_schema: Value,
}

/// Every command, by id.
#[derive(Default)]
pub struct Registry {
    by_id: BTreeMap<&'static str, Box<dyn DynCommand>>,
    /// Ids registered twice (the second is dropped); a test keeps it empty.
    duplicates: Vec<&'static str>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<C: Command>(&mut self, cmd: C) {
        let id = cmd.meta().id;
        if self.by_id.contains_key(id) {
            self.duplicates.push(id);
            return;
        }
        let typed = Typed {
            params_schema: schema_of::<C::Params>(),
            output_schema: schema_of::<C::Output>(),
            cmd,
        };
        self.by_id.insert(id, Box::new(typed));
    }

    pub fn get(&self, id: &str) -> Option<&dyn DynCommand> {
        self.by_id.get(id).map(|c| c.as_ref())
    }

    pub fn iter(&self) -> impl Iterator<Item = &dyn DynCommand> {
        self.by_id.values().map(|c| c.as_ref())
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    pub fn duplicates(&self) -> &[&'static str] {
        &self.duplicates
    }

    /// Every command with its schemas, for `command_list`.
    pub fn list(&self) -> Vec<CommandInfo> {
        self.iter()
            .map(|c| {
                let m = c.meta();
                CommandInfo {
                    id: m.id.to_string(),
                    label_vi: m.label_vi.to_string(),
                    label_en: m.label_en.to_string(),
                    doc: m.doc.to_string(),
                    effects: m.effects.to_vec(),
                    featured: m.featured,
                    can_dry_run: m.can_dry_run(),
                    params_schema: c.params_schema().clone(),
                    output_schema: c.output_schema().clone(),
                }
            })
            .collect()
    }
}

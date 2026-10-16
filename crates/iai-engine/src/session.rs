//! The one door: every caller — a menu, a key, a tool's commit, a script, an
//! agent — sends a [`CommandCall`] to [`Session::execute`].
//!
//! [`Engine`] is what lives as long as the program: the registry, the jobs,
//! the events not yet taken, the replies kept by request id, the revisions,
//! the policy. A [`Session`] is the engine with the open documents lent to
//! it for one call (see `workspace`).

use std::any::{Any, TypeId};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iai_canvas::document::Document;
use iai_history::command::Checkpoint;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::command::{DynCommand, Effect, Plan, Registry};
use crate::events::EngineEvent;
use crate::jobs::{self, JobId, JobState, Jobs};
use crate::policy::{Policy, Source};
use crate::workspace::Workspace;
use crate::{CmdError, ErrorCode};

/// What every caller sends.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CommandCall {
    /// Command id, e.g. `ai.remove_background` (see `command_list`).
    pub command: String,
    /// The command's params; their JSON Schema comes with `command_list`.
    #[serde(default)]
    pub args: Value,
    /// Refuse with `stale_revision` unless the target document (`args.doc`,
    /// or the one shown) is still at this revision.
    #[serde(default)]
    pub expect_revision: Option<u64>,
    /// A long command answers `{"status": "started", "job": id}` at once;
    /// `job_status` tells when it is done.
    #[serde(default)]
    pub background: bool,
    /// A call retried with the same id gets the first reply: nothing runs
    /// twice.
    #[serde(default)]
    pub request_id: Option<String>,
    /// Run, report the result, and take the change back.
    #[serde(default)]
    pub dry_run: bool,
}

impl CommandCall {
    pub fn new(command: impl Into<String>, args: Value) -> Self {
        Self {
            command: command.into(),
            args,
            ..Self::default()
        }
    }

    pub fn in_background(mut self) -> Self {
        self.background = true;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Reply {
    /// Done; `revision` is the target document's after the change.
    Done { result: Value, revision: u64 },
    /// Running in the background.
    Started { job: JobId },
}

impl Reply {
    pub fn result(&self) -> Option<&Value> {
        match self {
            Reply::Done { result, .. } => Some(result),
            Reply::Started { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BatchFailure {
    pub index: usize,
    pub error: CmdError,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BatchReply {
    /// Replies of the calls that ran, in order (none when rolled back).
    pub completed: Vec<Reply>,
    pub failed: Option<BatchFailure>,
    /// The batch was atomic and failed: everything it did was taken back.
    pub rolled_back: bool,
}

/// Most calls one batch may hold.
pub const MAX_BATCH: usize = 256;
/// Replies kept for retried request ids.
const KEEP_REPLIES: usize = 256;

/// What lives as long as the program.
pub struct Engine {
    registry: Arc<Registry>,
    jobs: Jobs,
    events: Vec<EngineEvent>,
    replies: VecDeque<(String, Reply)>,
    /// Per document: the last stamp seen and the revision it was given.
    revisions: HashMap<u32, (u64, u64)>,
    policy: Policy,
    source: Source,
    /// Caches commands keep between calls, by type.
    stash: HashMap<TypeId, Box<dyn Any + Send>>,
    /// Jobs whose work has ended, waiting for `Session::poll` to apply them.
    ready: Vec<jobs::Ended>,
}

impl Engine {
    pub fn new(registry: Registry, source: Source, policy: Policy) -> Self {
        Self {
            registry: Arc::new(registry),
            jobs: Jobs::default(),
            events: Vec::new(),
            replies: VecDeque::new(),
            revisions: HashMap::new(),
            policy,
            source,
            stash: HashMap::new(),
            ready: Vec::new(),
        }
    }

    /// Lend it the open documents for a call.
    pub fn session<'a>(&'a mut self, ws: &'a mut dyn Workspace) -> Session<'a> {
        Session { engine: self, ws }
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// The registry, shared: to look a command up while a session holds the
    /// engine.
    pub fn registry_arc(&self) -> Arc<Registry> {
        Arc::clone(&self.registry)
    }

    pub fn source(&self) -> Source {
        self.source
    }

    /// Whose calls come next (the window answering an agent's, then the
    /// person's again).
    pub fn set_source(&mut self, source: Source) {
        self.source = source;
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    pub fn set_policy(&mut self, policy: Policy) {
        self.policy = policy;
    }

    /// The events since they were last taken.
    pub fn take_events(&mut self) -> Vec<EngineEvent> {
        std::mem::take(&mut self.events)
    }

    /// A job is running or waits to be applied: the GUI keeps polling.
    pub fn busy(&self) -> bool {
        self.jobs.busy() || !self.ready.is_empty()
    }

    /// The jobs whose work has ended since the last poll, by id and command:
    /// `Session::poll` will apply them. The GUI looks before it polls, to put
    /// away what a result is about to replace.
    pub fn ended_jobs(&mut self) -> Vec<(JobId, &'static str)> {
        let ended = self.jobs.take_ended();
        self.ready.extend(ended);
        self.ready.iter().map(|e| (e.id, e.command)).collect()
    }

    /// What a command kept between calls, to read.
    pub fn stash<T: Send + 'static>(&self) -> Option<&T> {
        self.stash
            .get(&TypeId::of::<T>())
            .and_then(|b| b.downcast_ref::<T>())
    }

    /// Forget what a command kept between calls.
    pub fn stash_clear<T: Send + 'static>(&mut self) {
        self.stash.remove(&TypeId::of::<T>());
    }

    /// The command a job running on `doc` is, if one is.
    pub fn working_on(&self, doc: u32) -> Option<&'static str> {
        self.jobs.working_on(doc)
    }

    pub fn running_jobs(&self) -> Vec<JobId> {
        self.jobs.running_ids()
    }

    pub fn job_state(&self, job: JobId) -> Option<JobState> {
        self.jobs.state(job)
    }

    /// Progress of a running job: 0 to 1, and its Vietnamese line.
    pub fn job_progress(&self, job: JobId) -> Option<(f32, String)> {
        self.jobs.progress_of(job)
    }

    /// Ask a running job to stop; it ends at its next check and changes
    /// nothing. False when no such job runs.
    pub fn cancel(&mut self, job: JobId) -> bool {
        self.jobs.cancel(job)
    }

    fn remember_reply(&mut self, id: String, reply: Reply) {
        if self.replies.len() >= KEEP_REPLIES {
            self.replies.pop_front();
        }
        self.replies.push_back((id, reply));
    }

    fn earlier_reply(&self, id: &str) -> Option<Reply> {
        self.replies
            .iter()
            .rev()
            .find(|(i, _)| i == id)
            .map(|(_, r)| r.clone())
    }

    fn forget_reply(&mut self, id: &str) {
        self.replies.retain(|(i, _)| i != id);
    }
}

/// The engine with the documents, for one call.
pub struct Session<'a> {
    pub(crate) engine: &'a mut Engine,
    pub(crate) ws: &'a mut dyn Workspace,
}

/// Which page of a document is the one being edited: a history checkpoint
/// only means something on the page it was taken on.
#[derive(Clone, Copy, PartialEq, Eq)]
struct PageKey {
    page: usize,
    master: bool,
    pages: usize,
}

impl PageKey {
    fn of(doc: &Document) -> Self {
        Self {
            page: doc.active_artboard,
            master: doc.editing_master,
            pages: doc.pages.len(),
        }
    }
}

/// How a call ended, when it did not fail.
enum Outcome {
    Done(Value),
    Started(JobId),
}

/// What a call may have to take back.
struct Txn {
    marks: Vec<(u32, PageKey, Checkpoint)>,
    docs: Vec<u32>,
    events: usize,
    /// Each document's revision before the call: a call taken back leaves it.
    revisions: Vec<(u32, u64)>,
}

impl Session<'_> {
    /// Run one call. A failure, a panic or a dry run leaves the documents as
    /// they were.
    pub fn execute(&mut self, call: CommandCall) -> Result<Reply, CmdError> {
        if let Some(earlier) = call
            .request_id
            .as_deref()
            .and_then(|id| self.engine.earlier_reply(id))
        {
            return Ok(earlier);
        }
        let registry = Arc::clone(&self.engine.registry);
        let cmd = registry.get(&call.command).ok_or_else(|| {
            CmdError::new(
                ErrorCode::UnknownCommand,
                format!("no command `{}`", call.command),
            )
            .vi(format!("Không có lệnh `{}`", call.command))
            .with_hint(serde_json::json!({"tool": "command_list"}))
        })?;
        let meta = cmd.meta();
        self.engine.policy.check(meta)?;
        self.check_not_working(cmd, &call.args)?;
        if call.dry_run && !meta.can_dry_run() {
            return Err(CmdError::bad_param(
                "dryRun",
                "false: this command writes files, prints or uses the network",
            ));
        }
        if let Some(expected) = call.expect_revision {
            self.check_revision(&call.args, expected)?;
        }
        let source = self.engine.source.as_str();
        iai_diag::note(
            "cmd",
            &format!("{source} {} {}", meta.id, short(&call.args)),
        );

        let txn = self.begin();
        let planned = match jobs::guarded(|| cmd.plan_json(self, call.args.clone())) {
            Ok(planned) => planned,
            Err(payload) => Err(jobs::panicked(meta.id, &jobs::panic_text(payload.as_ref()))),
        };
        let outcome = match planned {
            Ok(Plan::Done(result)) => Ok(Outcome::Done(result)),
            Ok(Plan::Job(job)) => {
                if let Some(other) = job.doc.and_then(|d| self.engine.jobs.working_on(d)) {
                    Err(CmdError::new(
                        ErrorCode::Busy,
                        format!("`{other}` is still working on this document"),
                    )
                    .vi("Ảnh này đang được xử lý, đợi xong rồi làm tiếp"))
                } else if call.background && !call.dry_run {
                    let label_vi = job.label_vi.clone();
                    self.engine.jobs.spawn(meta.id, job).map(|id| {
                        self.engine.events.push(EngineEvent::JobStarted {
                            job: id,
                            command: meta.id.to_string(),
                            label_vi,
                        });
                        Outcome::Started(id)
                    })
                } else {
                    jobs::Jobs::run_here(meta.id, job).and_then(|apply| {
                        match jobs::guarded(|| apply(self)) {
                            Ok(out) => out.map(Outcome::Done),
                            Err(payload) => {
                                Err(jobs::panicked(meta.id, &jobs::panic_text(payload.as_ref())))
                            }
                        }
                    })
                }
            }
            Err(e) => Err(e),
        };

        match outcome {
            Ok(Outcome::Started(job)) => {
                let reply = Reply::Started { job };
                if let Some(rid) = call.request_id {
                    self.engine.remember_reply(rid, reply.clone());
                }
                Ok(reply)
            }
            Ok(Outcome::Done(result)) => {
                if call.dry_run {
                    self.rollback(txn);
                }
                let revision = self.target_revision(&call.args);
                let reply = Reply::Done { result, revision };
                if !call.dry_run {
                    if let Some(rid) = call.request_id {
                        self.engine.remember_reply(rid, reply.clone());
                    }
                }
                Ok(reply)
            }
            Err(e) => {
                self.rollback(txn);
                iai_diag::note("cmd", &format!("{source} {} failed: {e}", meta.id));
                Err(e)
            }
        }
    }

    /// Run calls in order. `atomic`: the first failure takes back the calls
    /// before it as well. Long commands run to the end, one after another.
    pub fn execute_batch(&mut self, calls: Vec<CommandCall>, atomic: bool) -> BatchReply {
        if calls.len() > MAX_BATCH {
            return BatchReply {
                completed: Vec::new(),
                failed: Some(BatchFailure {
                    index: 0,
                    error: CmdError::new(
                        ErrorCode::TooLarge,
                        format!("a batch holds at most {MAX_BATCH} calls"),
                    ),
                }),
                rolled_back: false,
            };
        }
        let txn = self.begin();
        let mut completed = Vec::new();
        let mut request_ids = Vec::new();
        for (index, mut call) in calls.into_iter().enumerate() {
            call.background = false;
            if let Some(id) = &call.request_id {
                request_ids.push(id.clone());
            }
            match self.execute(call) {
                Ok(reply) => completed.push(reply),
                Err(error) => {
                    if atomic {
                        self.rollback(txn);
                        for id in &request_ids {
                            self.engine.forget_reply(id);
                        }
                        completed.clear();
                    }
                    return BatchReply {
                        completed,
                        failed: Some(BatchFailure { index, error }),
                        rolled_back: atomic,
                    };
                }
            }
        }
        BatchReply {
            completed,
            failed: None,
            rolled_back: false,
        }
    }

    /// Apply the jobs whose work has ended. The GUI calls it every frame;
    /// returns how many ended.
    pub fn poll(&mut self) -> usize {
        let mut ended = std::mem::take(&mut self.engine.ready);
        ended.extend(self.engine.jobs.take_ended());
        let count = ended.len();
        for job in ended {
            let state = if job.cancelled {
                JobState::Cancelled
            } else {
                match job.outcome {
                    Ok(apply) => {
                        let txn = self.begin();
                        let applied = match jobs::guarded(|| apply(self)) {
                            Ok(out) => out,
                            Err(payload) => Err(jobs::panicked(
                                job.command,
                                &jobs::panic_text(payload.as_ref()),
                            )),
                        };
                        match applied {
                            Ok(result) => {
                                let revision = self.active_revision();
                                JobState::Done { result, revision }
                            }
                            Err(error) => {
                                self.rollback(txn);
                                JobState::Failed { error }
                            }
                        }
                    }
                    Err(error) if error.code == ErrorCode::Cancelled => JobState::Cancelled,
                    Err(error) => JobState::Failed { error },
                }
            };
            let (error, cancelled) = match &state {
                JobState::Failed { error } => (Some(error.clone()), false),
                JobState::Cancelled => (None, true),
                _ => (None, false),
            };
            let source = self.engine.source.as_str();
            match &error {
                Some(e) => {
                    iai_diag::note("cmd", &format!("{source} {} job failed: {e}", job.command))
                }
                None => iai_diag::note("cmd", &format!("{source} {} job ended", job.command)),
            }
            self.engine.events.push(EngineEvent::JobFinished {
                job: job.id,
                command: job.command.to_string(),
                error,
                cancelled,
            });
            self.engine.jobs.remember(job.id, state);
        }
        count
    }

    /// Poll until `job` has ended or `timeout` has passed; its state then.
    pub fn wait(&mut self, job: JobId, timeout: Duration) -> Option<JobState> {
        let start = Instant::now();
        loop {
            self.poll();
            let state = self.engine.jobs.state(job)?;
            if !matches!(state, JobState::Running { .. }) || start.elapsed() >= timeout {
                return Some(state);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    // ---- revisions -------------------------------------------------------

    /// The revision of document `doc`: it goes up whenever the document's
    /// content, selection or pages change, by whoever.
    pub fn revision(&mut self, doc: u32) -> u64 {
        let Some(stamp) = self.find(doc).map(stamp) else {
            return 0;
        };
        let entry = self.engine.revisions.entry(doc).or_insert((stamp, 0));
        if entry.0 != stamp {
            *entry = (stamp, entry.1.saturating_add(1));
        }
        entry.1
    }

    fn active_revision(&mut self) -> u64 {
        match self.active_id() {
            Some(doc) => self.revision(doc),
            None => 0,
        }
    }

    fn target_revision(&mut self, args: &Value) -> u64 {
        match arg_doc(args).or_else(|| self.active_id()) {
            Some(doc) => self.revision(doc),
            None => 0,
        }
    }

    /// An agent's or a script's change to a document a job is still working
    /// on waits for the job: it would land under the job's result, or write
    /// the document as it was before (an export of a photo not yet made).
    /// The person at the window sees the job and is left to judge.
    fn check_not_working(&mut self, cmd: &dyn DynCommand, args: &Value) -> Result<(), CmdError> {
        let reads_only = cmd.meta().effects.iter().all(|e| *e == Effect::Query);
        if reads_only || self.engine.source == Source::Gui {
            return Ok(());
        }
        let targets_a_doc = cmd
            .params_schema()
            .get("properties")
            .is_some_and(|p| p.get("doc").is_some());
        let Some(doc) = targets_a_doc
            .then(|| arg_doc(args).or_else(|| self.active_id()))
            .flatten()
        else {
            return Ok(());
        };
        match self.engine.jobs.working_on(doc) {
            None => Ok(()),
            Some(other) => Err(CmdError::new(
                ErrorCode::Busy,
                format!("`{other}` is still working on document {doc}: wait for its job to end"),
            )
            .vi("Ảnh này đang được xử lý, đợi xong rồi làm tiếp")
            .with_hint(serde_json::json!({"tool": "job_status"}))),
        }
    }

    fn check_revision(&mut self, args: &Value, expected: u64) -> Result<(), CmdError> {
        let doc = arg_doc(args)
            .or_else(|| self.active_id())
            .ok_or_else(no_document)?;
        if self.find(doc).is_none() {
            return Err(no_such_document(doc));
        }
        let now = self.revision(doc);
        if now == expected {
            return Ok(());
        }
        Err(CmdError::new(
            ErrorCode::StaleRevision,
            format!("document {doc} is at revision {now}, not {expected}: read it again"),
        )
        .vi("Ảnh vừa được sửa ở chỗ khác; đọc lại rồi làm tiếp")
        .with_hint(serde_json::json!({"command": "doc.inspect", "args": {"doc": doc}})))
    }

    // ---- transactions ----------------------------------------------------

    fn begin(&mut self) -> Txn {
        let ids: Vec<u32> = self.ws.documents().iter().map(|d| d.id.0).collect();
        let revisions = ids.iter().map(|&d| (d, self.revision(d))).collect();
        let docs = self.ws.documents();
        Txn {
            marks: docs
                .iter()
                .map(|d| (d.id.0, PageKey::of(d), d.canvas.history_checkpoint()))
                .collect(),
            docs: ids,
            events: self.engine.events.len(),
            revisions,
        }
    }

    /// Take back what was done since `txn`: undo each page to its mark, close
    /// documents opened since. Events emitted since are dropped; what the
    /// screen must redraw is emitted again.
    fn rollback(&mut self, txn: Txn) {
        self.engine.events.truncate(txn.events);
        let mut redraw = Vec::new();
        for (doc, key, mark) in &txn.marks {
            let Some(d) = self.ws.documents_mut().iter_mut().find(|d| d.id.0 == *doc) else {
                continue;
            };
            if PageKey::of(d) != *key || d.canvas.history_checkpoint() == *mark {
                continue;
            }
            d.canvas.rollback_to(*mark);
            redraw.push(*doc);
        }
        let opened: Vec<u32> = self
            .ws
            .documents()
            .iter()
            .map(|d| d.id.0)
            .filter(|id| !txn.docs.contains(id))
            .collect();
        for doc in &opened {
            if let Some(i) = self.index_of(*doc) {
                self.ws.remove(i);
            }
        }
        for doc in redraw {
            // Back where it was, so at the revision it had.
            if let (Some(stamp), Some((_, rev))) = (
                self.find(doc).map(stamp),
                txn.revisions.iter().find(|(d, _)| *d == doc),
            ) {
                self.engine.revisions.insert(doc, (stamp, *rev));
            }
            self.engine.events.push(EngineEvent::Structure { doc });
            self.engine.events.push(EngineEvent::Selection { doc });
        }
        if !opened.is_empty() {
            if let Some(doc) = self.active_id() {
                self.engine.events.push(EngineEvent::Shown { doc });
            }
        }
    }

    // ---- for commands -----------------------------------------------------

    pub fn source(&self) -> Source {
        self.engine.source
    }

    pub fn emit(&mut self, event: EngineEvent) {
        self.engine.events.push(event);
    }

    /// A line for the status bar.
    pub fn notice(&mut self, vi: impl Into<String>, en: impl Into<String>) {
        self.engine.events.push(EngineEvent::Notice {
            vi: vi.into(),
            en: en.into(),
        });
    }

    /// Take what a command kept between calls (e.g. what a model found).
    pub fn stash_take<T: Send + 'static>(&mut self) -> Option<T> {
        self.engine
            .stash
            .remove(&TypeId::of::<T>())
            .and_then(|b| b.downcast::<T>().ok())
            .map(|b| *b)
    }

    /// Keep `value` for a later call; replaces what was kept of its type.
    pub fn stash_put<T: Send + 'static>(&mut self, value: T) {
        self.engine.stash.insert(TypeId::of::<T>(), Box::new(value));
    }

    pub fn workspace(&self) -> &dyn Workspace {
        &*self.ws
    }

    pub fn workspace_mut(&mut self) -> &mut dyn Workspace {
        &mut *self.ws
    }

    pub(crate) fn find(&self, doc: u32) -> Option<&Document> {
        self.ws.documents().iter().find(|d| d.id.0 == doc)
    }

    pub(crate) fn index_of(&self, doc: u32) -> Option<usize> {
        self.ws.documents().iter().position(|d| d.id.0 == doc)
    }

    /// The id of the document shown.
    pub fn active_id(&self) -> Option<u32> {
        let i = self.ws.active()?;
        self.ws.documents().get(i).map(|d| d.id.0)
    }

    /// The registry, to list commands.
    pub fn registry(&self) -> &Registry {
        &self.engine.registry
    }
}

/// What tells two states of a document apart for its revision: its history,
/// selection, size and pages. Not `layer_revision`: the GUI bumps it to
/// redraw, which is no change an agent should be told about.
fn stamp(doc: &Document) -> u64 {
    let mut h = DefaultHasher::new();
    let c = &doc.canvas;
    c.history_revision().hash(&mut h);
    c.selection.mask_revision.hash(&mut h);
    c.selection.active.hash(&mut h);
    (c.width, c.height).hash(&mut h);
    doc.active_artboard.hash(&mut h);
    doc.editing_master.hash(&mut h);
    doc.pages.len().hash(&mut h);
    for page in doc.pages.iter().flatten() {
        page.history_revision().hash(&mut h);
    }
    if let Some(master) = &doc.master {
        master.history_revision().hash(&mut h);
    }
    if let Some(pdf) = &doc.pdf_document {
        (pdf.active_page, pdf.selected_pages.len()).hash(&mut h);
    }
    h.finish()
}

/// `args.doc`, when the call names one.
fn arg_doc(args: &Value) -> Option<u32> {
    args.get("doc")
        .and_then(Value::as_u64)
        .and_then(|d| u32::try_from(d).ok())
}

/// Args for the flight recorder: short, one line.
fn short(args: &Value) -> String {
    let text = args.to_string();
    if text.chars().count() <= 300 {
        return text;
    }
    let cut: String = text.chars().take(300).collect();
    format!("{cut}…")
}

pub(crate) fn no_document() -> CmdError {
    CmdError::new(ErrorCode::NoDocument, "no document is open").vi("Chưa mở ảnh nào")
}

pub(crate) fn no_such_document(doc: u32) -> CmdError {
    CmdError::new(ErrorCode::NoDocument, format!("document {doc} is not open"))
        .vi(format!("Tài liệu {doc} không còn mở"))
}

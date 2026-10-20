//! A Model Context Protocol server: JSON-RPC 2.0 over a pair of streams
//! (stdio), one message a line. Tools:
//!
//! - one per command marked `featured` (e.g. `iai_ai_remove_background`),
//!   its input schema the command's own params schema;
//! - a few that reach every command: `iai_command_list` (with schemas),
//!   `iai_command_run`, `iai_command_batch`, `iai_job_status`,
//!   `iai_job_cancel`, `iai_render_preview` (an image to look at).
//!
//! Resources: `iai://session` (the open documents), `iai://commands`, and
//! `iai://doc/{id}` (one document as `doc.inspect` describes it).
//!
//! With a skill library ([`Server::with_skills`]): `iai_task_begin` /
//! `iai_task_end` around a task (what the agent does in between is recorded
//! and kept as a skill when it went well), `iai_skill_search`,
//! `iai_skill_get`, `iai_skill_run`.

use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::time::{Duration, Instant};

use iai_engine::{CmdError, CommandCall, CommandInfo, Effect, ErrorCode, JobId, JobState, Reply};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::gate::Gate;
use crate::host::Host;
use crate::jsonrpc::{self, Line, Request, RpcError};
use crate::skills::{self, Recorded, Run, Store};

/// Protocol versions this server speaks, newest first.
const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// What the agent is told when it connects.
pub const INSTRUCTIONS: &str = "iAi is a photo editor (layers, selections, local AI models for \
cutting out a subject and filling, ID photos and print sheets, PDF). Every action is a command \
with typed params; the featured ones are tools of their own, and `iai_command_list` lists all \
with their JSON Schema, `iai_command_run` runs any. Documents, pages and layers are named by the \
ids `doc.list` / `doc.inspect` give; leave `doc` out for the document shown. Files: paths are \
read relative to the folders this session was granted (absolute paths must lie inside them); \
`iai_file_list` shows what a folder holds, `iai_file_contact_sheet` its photos numbered in one \
picture (to sort a customer's photos at a glance). Cards shot at a slant: \
`image.perspective_crop` lays one flat, `print.card_sheet` (Pro) lays ID cards out on A4. \
Tone and colour: `image.auto_levels`, `image.adjust`, `image.filter`, `image.scan_cleanup`; \
pictures onto a page: `layer.place`. \
Long commands (AI models, files) answer when done, or with a job id to poll with \
`iai_job_status`. Every change is one undo step (`edit.undo`). To check work, measure it: \
`analyze.region` gives the average colour, spread and coverage of a rectangle; \
`iai_render_preview` shows a picture. Errors carry a `code` to branch on and often a `hint` \
with the call that would fix them.";

/// Added to [`INSTRUCTIONS`] when the skill library is on.
pub const SKILL_INSTRUCTIONS: &str = " Saved skills: begin every task the person gives with \
`iai_task_begin {request}` in their words; it answers with saved skills that already do such a \
job — run one with `iai_skill_run` (fill its params) instead of writing the steps again. \
Otherwise work step by step, check the result, then end with `iai_task_end {status: \"done\", \
params: {…}}`, naming the values from the request a like request would change (size, colour, \
copies): iAi keeps what you did as a skill by itself, for the next agent.";

fn skill_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "iai_task_begin".into(),
            title: "Begin a Task".into(),
            description: "Start a task the person asked for, in their own words. Answers with the saved skills that fit it best: run one with iai_skill_run instead of doing the steps again. What you do until iai_task_end is recorded, so iAi can keep it as a skill.".into(),
            input_schema: object(json!({
                "request": {"type": "string", "description": "What the person asked for, as they said it."}
            }), &["request"]),
            read_only: true,
            kind: Kind::TaskBegin,
        },
        Tool {
            name: "iai_task_end".into(),
            title: "End the Task".into(),
            description: "End the task. With status \"done\" (you checked the work and it is right) iAi keeps what was done as a skill — or, when a saved skill already does that job, counts one more use. In `params` name the values from the request a similar request would change, e.g. {\"size\": \"3x4\", \"copies\": 2}. Give `title` and `description` in Vietnamese when you can. `helpers`: programs you wrote and ran beside iAi (files inside the granted folders), with `howToRun`.".into(),
            input_schema: object(json!({
                "status": {"type": "string", "enum": ["done", "failed", "cancelled"], "default": "done"},
                "name": {"type": "string", "description": "A short name for the skill (a-z, 0-9, -)."},
                "title": {"type": "string", "description": "A few words, Vietnamese."},
                "description": {"type": "string", "description": "One sentence, Vietnamese: what the skill does."},
                "descriptionEn": {"type": "string"},
                "params": {"type": "object", "description": "Values from the request that a like request would change: {name: value as used}."},
                "checks": {"type": "array", "items": {"type": "string"}, "description": "How you checked the work was right."},
                "helpers": {"type": "array", "items": {"type": "string"}, "description": "Programs you ran beside iAi, to keep with the skill."},
                "howToRun": {"type": "string", "description": "For helpers: the lines that run them; ${param} is filled in."}
            }), &[]),
            read_only: false,
            kind: Kind::TaskEnd,
        },
        Tool {
            name: "iai_skill_search".into(),
            title: "Find Skills".into(),
            description: "Find saved skills by a request or a few words (Vietnamese marks optional). An empty query lists every skill.".into(),
            input_schema: object(json!({
                "query": {"type": "string"},
                "max": {"type": "integer", "minimum": 1, "maximum": 50, "default": 5}
            }), &[]),
            read_only: true,
            kind: Kind::SkillSearch,
        },
        Tool {
            name: "iai_skill_get".into(),
            title: "Read a Skill".into(),
            description: "A saved skill: what it does, its params, how it is run, its SKILL.md. `recipe: true` adds its steps.".into(),
            input_schema: object(json!({
                "name": {"type": "string"},
                "recipe": {"type": "boolean", "default": false}
            }), &["name"]),
            read_only: true,
            kind: Kind::SkillGet,
        },
        Tool {
            name: "iai_skill_run".into(),
            title: "Run a Skill".into(),
            description: "Run a saved skill with its params (those left out take their defaults; folders must be granted). `dryRun` checks the params and commands and says what the loops will take, running nothing. A long run answers {status: \"running\", run: id}: call again with {run: id} to go on.".into(),
            input_schema: object(json!({
                "name": {"type": "string"},
                "params": {"type": "object"},
                "dryRun": {"type": "boolean", "default": false},
                "run": {"type": "integer", "minimum": 1, "description": "Go on with a run that answered \"running\"."}
            }), &[]),
            read_only: false,
            kind: Kind::SkillRun,
        },
    ]
}

#[derive(Debug, Clone)]
enum Kind {
    Command(String),
    List,
    Run,
    Batch,
    JobStatus,
    JobCancel,
    Preview,
    TaskBegin,
    TaskEnd,
    SkillSearch,
    SkillGet,
    SkillRun,
}

#[derive(Debug, Clone)]
struct Tool {
    name: String,
    title: String,
    description: String,
    input_schema: Value,
    read_only: bool,
    kind: Kind,
}

/// `ai.remove_background` → `iai_ai_remove_background`.
pub fn tool_name(id: &str) -> String {
    format!("iai_{}", id.replace('.', "_"))
}

fn object(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}

fn generic_tools() -> Vec<Tool> {
    let call_props = json!({
        "command": {"type": "string", "description": "Command id, e.g. \"image.crop\" (see iai_command_list)."},
        "args": {"type": "object", "description": "The command's params, as its schema says."},
        "expectRevision": {"type": "integer", "minimum": 0, "description": "Refuse with stale_revision unless the target document is still at this revision."},
        "requestId": {"type": "string", "description": "A retried call with the same id gets the first answer; nothing runs twice."},
        "dryRun": {"type": "boolean", "description": "Run, report, and take the change back (commands that write no file)."},
        "background": {"type": "boolean", "description": "Answer at once with a job id for a long command, instead of waiting."}
    });
    vec![
        Tool {
            name: "iai_command_list".into(),
            title: "List Commands".into(),
            description: "Every command iAi has: id, Vietnamese and English labels, what it does, the effects it may have, and the JSON Schema of its params. Filter by an id prefix (e.g. \"layer.\").".into(),
            input_schema: object(json!({
                "prefix": {"type": "string", "description": "Only ids starting with this."},
                "schemas": {"type": "boolean", "default": true, "description": "Include the params schemas."}
            }), &[]),
            read_only: true,
            kind: Kind::List,
        },
        Tool {
            name: "iai_command_run".into(),
            title: "Run Command".into(),
            description: "Run any command by id with its params (see iai_command_list). Answers with the command's result and the document's revision.".into(),
            input_schema: object(call_props.clone(), &["command"]),
            read_only: false,
            kind: Kind::Run,
        },
        Tool {
            name: "iai_command_batch".into(),
            title: "Run Commands in Order".into(),
            description: "Run several commands in order. With atomic (the default) the first failure takes back every change the batch made. Long commands run to their end.".into(),
            input_schema: object(json!({
                "calls": {"type": "array", "minItems": 1, "maxItems": crate::limits::MAX_BATCH,
                          "items": object(call_props, &["command"])},
                "atomic": {"type": "boolean", "default": true}
            }), &["calls"]),
            read_only: false,
            kind: Kind::Batch,
        },
        Tool {
            name: "iai_job_status".into(),
            title: "Job Status".into(),
            description: "Where a long command's job is (running with progress, done with its result, failed, cancelled), after waiting up to waitSeconds for it to end.".into(),
            input_schema: object(json!({
                "job": {"type": "integer", "minimum": 1},
                "waitSeconds": {"type": "number", "minimum": 0, "maximum": 120, "default": 0}
            }), &["job"]),
            read_only: true,
            kind: Kind::JobStatus,
        },
        Tool {
            name: "iai_job_cancel".into(),
            title: "Cancel Job".into(),
            description: "Ask a running job to stop; it changes nothing.".into(),
            input_schema: object(json!({"job": {"type": "integer", "minimum": 1}}), &["job"]),
            read_only: false,
            kind: Kind::JobCancel,
        },
        Tool {
            name: "iai_render_preview".into(),
            title: "Preview".into(),
            description: "A PNG picture of the shown page of a document, to look at. For checks by numbers use iai_analyze_region.".into(),
            input_schema: object(json!({
                "doc": {"type": "integer", "minimum": 0, "description": "Document id; the document shown when omitted."},
                "maxEdge": {"type": "integer", "minimum": 16, "maximum": 2048, "default": 1024}
            }), &[]),
            read_only: true,
            kind: Kind::Preview,
        },
    ]
}

/// The server, on a host, behind a gate.
pub struct Server<'a> {
    host: &'a mut dyn Host,
    gate: Gate,
    tools: Vec<Tool>,
    commands: Vec<CommandInfo>,
    /// How long a tool call waits for a long command before answering with
    /// its job id.
    pub wait: Duration,
    name: String,
    version: String,
    /// The skill library, when on.
    skills: Option<Store>,
    /// The task under way: what the agent has done in it.
    task: Option<Task>,
    /// Skill runs that answered "running", by id.
    runs: BTreeMap<u64, Run>,
    next_run: u64,
    /// The agent's app, as it introduced itself.
    client: String,
}

/// What an agent did in a task, for the library.
#[derive(Default)]
struct Task {
    request: String,
    calls: Vec<Recorded>,
    /// Calls that changed something and failed.
    failed: usize,
}

/// A tool's answer, before it becomes MCP content.
enum Answer {
    Value(Value),
    Image { png_base64: String, about: Value },
}

/// A command's answer, shown as a picture when its result carries one
/// (`pngBase64`, as `file.contact_sheet` gives).
fn pictured(mut answer: Value) -> Answer {
    let png = answer
        .get_mut("result")
        .and_then(Value::as_object_mut)
        .and_then(|result| result.remove("pngBase64"));
    match png {
        Some(Value::String(png_base64)) => Answer::Image {
            png_base64,
            about: answer,
        },
        Some(other) => {
            if let Some(result) = answer.get_mut("result").and_then(Value::as_object_mut) {
                result.insert("pngBase64".into(), other);
            }
            Answer::Value(answer)
        }
        None => Answer::Value(answer),
    }
}

impl<'a> Server<'a> {
    pub fn new(host: &'a mut dyn Host, gate: Gate, version: &str) -> Result<Self, CmdError> {
        let mut server = Self {
            host,
            gate,
            tools: Vec::new(),
            commands: Vec::new(),
            wait: Duration::from_secs(110),
            name: "iai".to_string(),
            version: version.to_string(),
            skills: None,
            task: None,
            runs: BTreeMap::new(),
            next_run: 1,
            client: String::new(),
        };
        server.refresh()?;
        Ok(server)
    }

    /// Turn the skill library in `store` on.
    pub fn with_skills(mut self, store: Store) -> Self {
        self.skills = Some(store);
        let _ = self.refresh();
        self
    }

    fn instructions(&self) -> String {
        if self.skills.is_some() {
            format!("{INSTRUCTIONS}{SKILL_INSTRUCTIONS}")
        } else {
            INSTRUCTIONS.to_string()
        }
    }

    /// Learn the host's commands again and build the tool list.
    fn refresh(&mut self) -> Result<(), CmdError> {
        self.commands = self.host.commands()?;
        self.gate.learn(&self.commands);
        let mut tools = Vec::new();
        for c in &self.commands {
            if !c.featured || !self.gate.allows(c) {
                continue;
            }
            tools.push(Tool {
                name: tool_name(&c.id),
                title: c.label_en.clone(),
                description: format!("{} (command `{}`; {})", c.doc, c.id, c.label_vi),
                input_schema: crate::schema::for_tool(&c.params_schema),
                read_only: c.effects.iter().all(|e| *e == iai_engine::Effect::Query),
                kind: Kind::Command(c.id.clone()),
            });
        }
        let extra = if self.skills.is_some() {
            skill_tools()
        } else {
            Vec::new()
        };
        for t in generic_tools().into_iter().chain(extra) {
            if !tools.iter().any(|x| x.name == t.name) {
                tools.push(t);
            }
        }
        self.tools = tools;
        Ok(())
    }

    /// Serve until `input` ends.
    pub fn serve<R: BufRead, W: Write>(&mut self, mut input: R, mut output: W) -> io::Result<()> {
        loop {
            let line = match jsonrpc::read_line(&mut input)? {
                Line::End => {
                    self.end_session();
                    return Ok(());
                }
                Line::TooLong => {
                    let e = RpcError::new(
                        jsonrpc::INVALID_REQUEST,
                        format!("a message is at most {} bytes", crate::limits::MAX_LINE),
                    );
                    jsonrpc::write_message(&mut output, &jsonrpc::response(Value::Null, Err(e)))?;
                    continue;
                }
                Line::Text(t) => t,
            };
            if line.trim().is_empty() {
                continue;
            }
            if let Some(answer) = self.handle_text(&line) {
                jsonrpc::write_message(&mut output, &answer)?;
            }
        }
    }

    /// The agent has gone: a task it never ended is kept as an unconfirmed
    /// skill when everything in it worked. [`Server::serve`] ends with this
    /// at the end of its input.
    pub fn end_session(&mut self) {
        if let Some(task) = self.task.take() {
            let _ = self.keep_task(task, &Value::Null, true);
        }
    }

    /// One message in, its answer out (none for a notification).
    pub fn handle_text(&mut self, text: &str) -> Option<Value> {
        let value: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => {
                let err = RpcError::new(jsonrpc::PARSE_ERROR, e.to_string());
                return Some(jsonrpc::response(Value::Null, Err(err)));
            }
        };
        let request: Request = match serde_json::from_value(value) {
            Ok(r) => r,
            Err(e) => {
                let err = RpcError::new(jsonrpc::INVALID_REQUEST, e.to_string());
                return Some(jsonrpc::response(Value::Null, Err(err)));
            }
        };
        let id = request.id.clone()?;
        let outcome = self.handle(&request.method, request.params);
        Some(jsonrpc::response(id, outcome))
    }

    fn handle(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
        match method {
            "initialize" => {
                let asked = params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let version = VERSIONS
                    .iter()
                    .find(|v| **v == asked)
                    .or(VERSIONS.first())
                    .copied()
                    .unwrap_or("2025-06-18");
                if let Some(name) = params.pointer("/clientInfo/name").and_then(Value::as_str) {
                    let version = params
                        .pointer("/clientInfo/version")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    self.client = format!("{name} {version}").trim().to_string();
                }
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {
                        "tools": {"listChanged": false},
                        "resources": {"subscribe": false, "listChanged": false}
                    },
                    "serverInfo": {"name": self.name, "title": "iAi", "version": self.version},
                    "instructions": self.instructions(),
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => {
                self.refresh()
                    .map_err(|e| RpcError::new(jsonrpc::INTERNAL_ERROR, e.message))?;
                let tools: Vec<Value> = self
                    .tools
                    .iter()
                    .map(|t| {
                        json!({
                            "name": t.name,
                            "title": t.title,
                            "description": t.description,
                            "inputSchema": t.input_schema,
                            "annotations": {
                                "title": t.title,
                                "readOnlyHint": t.read_only,
                                // Every change is one undo step.
                                "destructiveHint": false,
                                "openWorldHint": false,
                            }
                        })
                    })
                    .collect();
                Ok(json!({"tools": tools}))
            }
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| RpcError::new(jsonrpc::INVALID_PARAMS, "`name` is missing"))?
                    .to_string();
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                let tool = self
                    .tools
                    .iter()
                    .find(|t| t.name == name)
                    .cloned()
                    .ok_or_else(|| {
                        RpcError::new(jsonrpc::INVALID_PARAMS, format!("no tool `{name}`"))
                    })?;
                Ok(self.call_tool(&tool, args))
            }
            "resources/list" => Ok(json!({"resources": [
                {"uri": "iai://session", "name": "session", "title": "Open documents",
                 "mimeType": "application/json",
                 "description": "The open documents: ids, titles, sizes, pages, which is shown."},
                {"uri": "iai://commands", "name": "commands", "title": "Commands",
                 "mimeType": "application/json",
                 "description": "Every command with its params' JSON Schema."}
            ]})),
            "resources/templates/list" => Ok(json!({"resourceTemplates": [
                {"uriTemplate": "iai://doc/{id}", "name": "document", "title": "A document",
                 "mimeType": "application/json",
                 "description": "One document: size, dpi, pages, layers, selection, revision."}
            ]})),
            "resources/read" => {
                let uri = params
                    .get("uri")
                    .and_then(Value::as_str)
                    .ok_or_else(|| RpcError::new(jsonrpc::INVALID_PARAMS, "`uri` is missing"))?
                    .to_string();
                let value = self.read_resource(&uri)?;
                Ok(json!({"contents": [{
                    "uri": uri, "mimeType": "application/json",
                    "text": serde_json::to_string_pretty(&value).unwrap_or_default()
                }]}))
            }
            "prompts/list" => Ok(json!({"prompts": []})),
            _ => Err(RpcError::new(
                jsonrpc::METHOD_NOT_FOUND,
                format!("no method `{method}`"),
            )),
        }
    }

    fn read_resource(&mut self, uri: &str) -> Result<Value, RpcError> {
        let fail = |e: CmdError| RpcError {
            code: jsonrpc::INVALID_PARAMS,
            message: e.message.clone(),
            data: serde_json::to_value(&e).ok(),
        };
        if uri == "iai://session" {
            return self
                .run(CommandCall::new("doc.list", json!({})), true)
                .map_err(fail);
        }
        if uri == "iai://commands" {
            return Ok(self.list(None, true));
        }
        if let Some(id) = uri
            .strip_prefix("iai://doc/")
            .and_then(|d| d.parse::<u32>().ok())
        {
            return self
                .run(CommandCall::new("doc.inspect", json!({"doc": id})), true)
                .map_err(fail);
        }
        Err(RpcError::new(
            jsonrpc::INVALID_PARAMS,
            format!("no resource `{uri}`"),
        ))
    }

    fn list(&self, prefix: Option<&str>, schemas: bool) -> Value {
        let items: Vec<Value> = self
            .commands
            .iter()
            .filter(|c| prefix.is_none_or(|p| c.id.starts_with(p)))
            .map(|c| {
                let mut v = json!({
                    "id": c.id, "labelVi": c.label_vi, "labelEn": c.label_en, "doc": c.doc,
                    "effects": c.effects, "featured": c.featured, "canDryRun": c.can_dry_run,
                    "granted": self.gate.allows(c),
                });
                if schemas {
                    if let Some(o) = v.as_object_mut() {
                        o.insert(
                            "paramsSchema".to_string(),
                            crate::schema::for_tool(&c.params_schema),
                        );
                    }
                }
                v
            })
            .collect();
        json!({"commands": items})
    }

    /// Run `call` through the gate; wait for a long one when `wait`. A task
    /// under way records it.
    fn run(&mut self, mut call: CommandCall, wait: bool) -> Result<Value, CmdError> {
        let checked = self.gate.check(&mut call);
        let (command, args, dry_run) = (call.command.clone(), call.args.clone(), call.dry_run);
        let outcome = checked.and_then(|()| self.run_checked(call, wait));
        if !dry_run {
            self.note(&command, &args, &outcome);
        }
        outcome
    }

    /// Keep a call in the task under way; a first change begins one.
    fn note(&mut self, command: &str, args: &Value, outcome: &Result<Value, CmdError>) {
        if self.skills.is_none() {
            return;
        }
        let info = self.commands.iter().find(|c| c.id == command);
        // Reading documents or files changes nothing a skill would do again.
        let query = info.is_some_and(|i| {
            i.effects
                .iter()
                .all(|e| matches!(e, Effect::Query | Effect::ReadFiles))
        });
        let writes = info.is_some_and(|i| i.effects.contains(&Effect::WriteFiles));
        if self.task.is_none() {
            if query {
                return;
            }
            self.task = Some(Task::default());
        }
        let Some(task) = self.task.as_mut() else {
            return;
        };
        if task.calls.len() >= crate::limits::MAX_SCRIPT {
            return;
        }
        match outcome {
            Ok(v) => {
                let result = v.get("result").cloned();
                let job = if result.is_none() {
                    v.get("job").and_then(Value::as_u64)
                } else {
                    None
                };
                task.calls.push(Recorded {
                    command: command.to_string(),
                    args: args.clone(),
                    result,
                    job,
                    query,
                    writes,
                    skill: None,
                });
            }
            Err(_) if !query => task.failed = task.failed.saturating_add(1),
            Err(_) => {}
        }
    }

    fn run_checked(&mut self, mut call: CommandCall, wait: bool) -> Result<Value, CmdError> {
        let asked_background = call.background;
        call.background = true;
        let reply = self.host.execute(call)?;
        match reply {
            Reply::Done { result, revision } => Ok(json!({"result": result, "revision": revision})),
            Reply::Started { job } if !wait || asked_background => Ok(json!({
                "status": "started", "job": job,
                "hint": "poll with iai_job_status"
            })),
            Reply::Started { job } => self.wait_for(job, self.wait),
        }
    }

    fn wait_for(&mut self, job: JobId, wait: Duration) -> Result<Value, CmdError> {
        match self.host.job(job, wait)? {
            Some(JobState::Done { result, revision }) => {
                Ok(json!({"result": result, "revision": revision}))
            }
            Some(JobState::Failed { error }) => Err(error),
            Some(JobState::Cancelled) => {
                Err(CmdError::new(ErrorCode::Cancelled, "the job was cancelled"))
            }
            Some(JobState::Running {
                command,
                fraction,
                vi,
                seconds,
            }) => Ok(json!({
                "status": "running", "job": job, "command": command,
                "fraction": fraction, "progressVi": vi, "seconds": seconds,
                "hint": "still running: poll with iai_job_status"
            })),
            None => Err(CmdError::new(
                ErrorCode::NotFound,
                format!("no job {}", job.0),
            )),
        }
    }

    fn call_tool(&mut self, tool: &Tool, args: Value) -> Value {
        let answer = self.answer(tool, args);
        let answer = answer.and_then(|a| {
            let size_of = match &a {
                Answer::Value(v) => v.clone(),
                Answer::Image { about, .. } => about.clone(),
            };
            self.gate.check_size(&size_of).map(|()| a)
        });
        match answer {
            Ok(Answer::Value(v)) => {
                let text = serde_json::to_string(&v).unwrap_or_default();
                let mut out =
                    json!({"content": [{"type": "text", "text": text}], "isError": false});
                if v.is_object() {
                    if let Some(o) = out.as_object_mut() {
                        o.insert("structuredContent".to_string(), v);
                    }
                }
                out
            }
            Ok(Answer::Image { png_base64, about }) => json!({
                "content": [
                    {"type": "image", "data": png_base64, "mimeType": "image/png"},
                    {"type": "text", "text": about.to_string()}
                ],
                "isError": false
            }),
            Err(e) => {
                let v = serde_json::to_value(&e).unwrap_or(Value::Null);
                json!({
                    "content": [{"type": "text", "text": v.to_string()}],
                    "structuredContent": {"error": v},
                    "isError": true
                })
            }
        }
    }

    fn answer(&mut self, tool: &Tool, args: Value) -> Result<Answer, CmdError> {
        let bad = |e: serde_json::Error| CmdError::bad_params(&e);
        match &tool.kind {
            Kind::Command(id) => self
                .run(CommandCall::new(id.clone(), args), true)
                .map(pictured),
            Kind::Run => {
                let call: CommandCall = serde_json::from_value(args).map_err(bad)?;
                self.run(call, true).map(pictured)
            }
            Kind::Batch => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Batch {
                    calls: Vec<CommandCall>,
                    #[serde(default = "yes")]
                    atomic: bool,
                }
                fn yes() -> bool {
                    true
                }
                let mut b: Batch = serde_json::from_value(args).map_err(bad)?;
                self.gate.check_batch(&mut b.calls)?;
                let asked: Vec<(String, Value, bool)> = b
                    .calls
                    .iter()
                    .map(|c| (c.command.clone(), c.args.clone(), c.dry_run))
                    .collect();
                let reply = self.host.batch(b.calls, b.atomic)?;
                for (i, (command, args, dry_run)) in asked.iter().enumerate() {
                    let outcome = match (reply.completed.get(i), &reply.failed) {
                        (Some(r), _) => Ok(json!({"result": r.result()})),
                        (None, Some(f)) if f.index == i => Err(f.error.clone()),
                        _ => continue,
                    };
                    if !dry_run {
                        self.note(command, args, &outcome);
                    }
                }
                serde_json::to_value(reply)
                    .map(Answer::Value)
                    .map_err(|e| CmdError::internal(e.to_string()))
            }
            Kind::List => {
                let prefix = args
                    .get("prefix")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let schemas = args.get("schemas").and_then(Value::as_bool).unwrap_or(true);
                Ok(Answer::Value(self.list(prefix.as_deref(), schemas)))
            }
            Kind::JobStatus => {
                let job = args
                    .get("job")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| CmdError::bad_param("job", "a job id"))?;
                let secs = args
                    .get("waitSeconds")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0)
                    .clamp(0.0, 120.0);
                let state = self.host.job(JobId(job), Duration::from_secs_f64(secs))?;
                if let (Some(JobState::Done { result, .. }), Some(task)) =
                    (&state, self.task.as_mut())
                {
                    if let Some(c) = task
                        .calls
                        .iter_mut()
                        .find(|c| c.job == Some(job) && c.result.is_none())
                    {
                        c.result = Some(result.clone());
                    }
                }
                match state {
                    Some(s) => serde_json::to_value(s)
                        .map(Answer::Value)
                        .map_err(|e| CmdError::internal(e.to_string())),
                    None => Err(CmdError::new(ErrorCode::NotFound, format!("no job {job}"))),
                }
            }
            Kind::JobCancel => {
                let job = args
                    .get("job")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| CmdError::bad_param("job", "a job id"))?;
                let cancelled = self.host.cancel(JobId(job))?;
                Ok(Answer::Value(json!({"job": job, "cancelled": cancelled})))
            }
            Kind::Preview => {
                let v = self.run(CommandCall::new("doc.preview", args), true)?;
                let mut result = v.get("result").cloned().unwrap_or(Value::Null);
                let png = result
                    .get("pngBase64")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if let Some(o) = result.as_object_mut() {
                    o.remove("pngBase64");
                }
                Ok(Answer::Image {
                    png_base64: png,
                    about: result,
                })
            }
            Kind::TaskBegin
            | Kind::TaskEnd
            | Kind::SkillSearch
            | Kind::SkillGet
            | Kind::SkillRun => self.skill_answer(&tool.kind, args).map(Answer::Value),
        }
    }

    fn store(&self) -> Result<Store, CmdError> {
        self.skills
            .clone()
            .ok_or_else(|| CmdError::new(ErrorCode::NotEnabled, "the skill library is off"))
    }

    fn skill_answer(&mut self, kind: &Kind, args: Value) -> Result<Value, CmdError> {
        let store = self.store()?;
        let text = |k: &str| {
            args.get(k)
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string()
        };
        match kind {
            Kind::TaskBegin => {
                let request = text("request");
                // A task under way that was never ended is kept as it stands.
                if let Some(earlier) = self.task.take() {
                    let _ = self.keep_task(earlier, &Value::Null, true);
                }
                self.task = Some(Task {
                    request: request.clone(),
                    ..Task::default()
                });
                let found: Vec<Value> = store
                    .search(&request, 3)
                    .iter()
                    .map(skills::Found::brief)
                    .collect();
                let hint = if found.is_empty() {
                    "No saved skill does this yet: do it step by step, check it, then call \
                     iai_task_end with status \"done\" and iAi keeps it as a skill."
                } else {
                    "A saved skill may already do this: run it with iai_skill_run {name, params} \
                     (iai_skill_get tells more). If none fits, do it step by step and end with \
                     iai_task_end."
                };
                Ok(json!({"skills": found, "library": store.root(), "hint": hint}))
            }
            Kind::TaskEnd => {
                let status = args.get("status").and_then(Value::as_str).unwrap_or("done");
                let Some(task) = self.task.take() else {
                    return Ok(json!({"saved": null, "reason": "nothing was done in this task"}));
                };
                if status != "done" {
                    return Ok(
                        json!({"saved": null, "reason": format!("the task ended {status}: nothing kept")}),
                    );
                }
                self.keep_task(task, &args, false)
            }
            Kind::SkillSearch => {
                let max = args
                    .get("max")
                    .and_then(Value::as_u64)
                    .unwrap_or(5)
                    .clamp(1, 50);
                let found: Vec<Value> = store
                    .search(&text("query"), usize::try_from(max).unwrap_or(5))
                    .iter()
                    .map(skills::Found::brief)
                    .collect();
                Ok(json!({"skills": found, "library": store.root()}))
            }
            Kind::SkillGet => {
                let name = text("name");
                let recipe = store.load(&name)?;
                let folder = store.root().join(&name);
                let mut out = json!({
                    "name": name,
                    "folder": folder,
                    "title": recipe.meta.title,
                    "description": recipe.meta.description,
                    "params": recipe.params_brief(),
                    "tier": recipe.meta.tier.max(1),
                    "status": recipe.meta.status.vi(recipe.meta.uses),
                    "helpers": recipe.meta.helpers.iter().map(|h| folder.join(h)).collect::<Vec<_>>(),
                    "howToRun": recipe.meta.how_to_run,
                    "skillMd": store.skill_md(&name)?,
                });
                if args.get("recipe").and_then(Value::as_bool) == Some(true) {
                    if let Some(o) = out.as_object_mut() {
                        o.insert(
                            "recipe".to_string(),
                            serde_json::to_value(&recipe).unwrap_or(Value::Null),
                        );
                    }
                }
                Ok(out)
            }
            Kind::SkillRun => self.skill_run(&store, &args),
            _ => Err(CmdError::internal("not a skill tool")),
        }
    }

    /// Run a skill, or go on with one that answered "running".
    fn skill_run(&mut self, store: &Store, args: &Value) -> Result<Value, CmdError> {
        let budget = self
            .wait
            .saturating_sub(Duration::from_secs(10))
            .max(Duration::from_secs(5));
        let until = Instant::now().checked_add(budget);
        let mut run = if let Some(id) = args.get("run").and_then(Value::as_u64) {
            self.runs.remove(&id).ok_or_else(|| {
                CmdError::new(ErrorCode::NotFound, format!("no skill run {id} under way"))
            })?
        } else {
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            let given = args.get("params").cloned().unwrap_or(Value::Null);
            let (run, plan) = Run::start(store, &self.gate, name, &given)?;
            if args.get("dryRun").and_then(Value::as_bool) == Some(true) {
                return Ok(json!({"status": "checked", "plan": plan}));
            }
            run
        };
        let how = format!(
            "{} · {}",
            run.skill,
            if self.client.is_empty() {
                "AI"
            } else {
                &self.client
            }
        );
        match run.advance(&mut *self.host, &self.gate, store, until) {
            Ok(skills::Progress::Done) => {
                store.note(
                    &run.skill,
                    &format!("chạy · xong · {} lệnh · {}", run.calls, self.client),
                );
                let next = self.how_to_run(store, &run);
                let out = json!({
                    "status": "done", "skill": run.skill, "calls": run.calls,
                    "written": run.written, "next": next,
                    "hint": if next.is_empty() { "done: check the result, then iai_task_end" }
                            else { "this skill's programs run outside iAi: run `next` with your shell, then iai_task_end" },
                });
                let task = self.task.get_or_insert_with(Task::default);
                task.calls.push(Recorded {
                    skill: Some((run.skill.clone(), run.params.clone())),
                    result: Some(json!({"calls": run.calls, "written": run.written})),
                    // What it wrote is the task's result: files opened after
                    // it to check them are not part of the job.
                    writes: !run.written.is_empty(),
                    ..Recorded::default()
                });
                Ok(out)
            }
            Ok(skills::Progress::Paused) => {
                let id = self.next_run;
                self.next_run = self.next_run.saturating_add(1);
                let calls = run.calls;
                self.runs.insert(id, run);
                Ok(json!({
                    "status": "running", "run": id, "calls": calls,
                    "hint": format!("still running: call iai_skill_run with {{\"run\": {id}}} to go on")
                }))
            }
            Err(f) => {
                let broken = matches!(f.error.code, ErrorCode::BadParams | ErrorCode::NotFound)
                    && !f.step.is_empty();
                let _ = store.record_use(&run.skill, false, broken, "", &how);
                if let Some(task) = self.task.as_mut() {
                    task.failed = task.failed.saturating_add(1);
                }
                let round = f.round.map(|r| format!(", round {r}")).unwrap_or_default();
                let mut e = f.error.clone();
                e.message = format!(
                    "skill `{}` stopped at step `{}`{round}: {}",
                    run.skill, f.step, e.message
                );
                Err(e)
            }
        }
    }

    /// A tier-2 skill's lines to run, its params filled in.
    fn how_to_run(&self, store: &Store, run: &Run) -> String {
        let Ok(recipe) = store.load(&run.skill) else {
            return String::new();
        };
        if recipe.meta.how_to_run.trim().is_empty() {
            return String::new();
        }
        let mut scope = skills::template::Scope::default();
        if let Value::Object(o) = &run.params {
            for (k, v) in o {
                scope.push(k, v.clone());
            }
        }
        let exe = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        scope.push("iai", json!(exe));
        scope.push(
            "thu_muc_ky_nang",
            json!(store.root().join(&run.skill).display().to_string()),
        );
        skills::template::fill(
            &json!(recipe.meta.how_to_run),
            &scope,
            &skills::template::Results::new(),
            &run.skill,
        )
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| recipe.meta.how_to_run.clone())
    }

    /// Keep `task` in the library (see [`Store::learn`]); `end` is what
    /// `iai_task_end` was given. A task kept unasked (`unconfirmed`) is kept
    /// only when nothing in it failed and it changed something twice or more.
    fn keep_task(&mut self, task: Task, end: &Value, unconfirmed: bool) -> Result<Value, CmdError> {
        let Some(store) = self.skills.clone() else {
            return Ok(json!({"saved": null}));
        };
        let changes = task.calls.iter().filter(|c| !c.query).count();
        let ran_skill = task.calls.iter().any(|c| c.skill.is_some());
        if unconfirmed && (task.failed > 0 || (changes < 2 && !ran_skill)) {
            return Ok(
                json!({"saved": null, "reason": "an unended task is kept only when all of it worked"}),
            );
        }
        let named: BTreeMap<String, Value> = end
            .get("params")
            .and_then(Value::as_object)
            .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let Some(draft) = skills::learn::generalize(&task.calls, &named) else {
            return Ok(json!({"saved": null, "reason": "the task changed nothing"}));
        };
        let text = |k: &str| {
            end.get(k)
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string()
        };
        let mut helpers = Vec::new();
        for h in end
            .get("helpers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(p) = h.as_str() {
                helpers.push(self.gate.resolve(p, false)?);
            }
        }
        let checks: Vec<String> = end
            .get("checks")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let mut title = text("title");
        if title.is_empty() && task.request.trim().is_empty() {
            // Nothing said what it was: what it did names it.
            let mut labels: Vec<String> = Vec::new();
            for c in task.calls.iter().filter(|c| !c.query) {
                let label = self
                    .commands
                    .iter()
                    .find(|i| i.id == c.command)
                    .map(|i| i.label_vi.clone())
                    .unwrap_or_else(|| c.command.clone());
                if !labels.contains(&label) {
                    labels.push(label);
                }
            }
            labels.truncate(4);
            title = labels.join(" → ");
        }
        let lesson = skills::Lesson {
            request: task.request.clone(),
            name: text("name"),
            title,
            description: text("description"),
            description_en: text("descriptionEn"),
            checks,
            helpers,
            how_to_run: text("howToRun"),
            app: self.version.clone(),
            client: self.client.clone(),
            unconfirmed,
        };
        let saved = store.learn(draft, &lesson)?;
        Ok(json!({"saved": saved.to_json(&store)}))
    }
}

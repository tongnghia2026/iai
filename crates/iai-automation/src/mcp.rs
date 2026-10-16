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

use std::io::{self, BufRead, Write};
use std::time::Duration;

use iai_engine::{CmdError, CommandCall, CommandInfo, ErrorCode, JobId, JobState, Reply};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::gate::Gate;
use crate::host::Host;
use crate::jsonrpc::{self, Line, Request, RpcError};

/// Protocol versions this server speaks, newest first.
const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// What the agent is told when it connects.
pub const INSTRUCTIONS: &str = "iAi is a photo editor (layers, selections, local AI models for \
cutting out a subject and filling, ID photos and print sheets, PDF). Every action is a command \
with typed params; the featured ones are tools of their own, and `iai_command_list` lists all \
with their JSON Schema, `iai_command_run` runs any. Documents, pages and layers are named by the \
ids `doc.list` / `doc.inspect` give; leave `doc` out for the document shown. Files: paths are \
read relative to the folders this session was granted (absolute paths must lie inside them). \
Long commands (AI models, files) answer when done, or with a job id to poll with \
`iai_job_status`. Every change is one undo step (`edit.undo`). To check work, measure it: \
`analyze.region` gives the average colour, spread and coverage of a rectangle; \
`iai_render_preview` shows a picture. Errors carry a `code` to branch on and often a `hint` \
with the call that would fix them.";

#[derive(Debug, Clone)]
enum Kind {
    Command(String),
    List,
    Run,
    Batch,
    JobStatus,
    JobCancel,
    Preview,
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
}

/// A tool's answer, before it becomes MCP content.
enum Answer {
    Value(Value),
    Image { png_base64: String, about: Value },
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
        };
        server.refresh()?;
        Ok(server)
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
        for t in generic_tools() {
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
                Line::End => return Ok(()),
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
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {
                        "tools": {"listChanged": false},
                        "resources": {"subscribe": false, "listChanged": false}
                    },
                    "serverInfo": {"name": self.name, "title": "iAi", "version": self.version},
                    "instructions": INSTRUCTIONS,
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

    /// Run `call` through the gate; wait for a long one when `wait`.
    fn run(&mut self, mut call: CommandCall, wait: bool) -> Result<Value, CmdError> {
        self.gate.check(&mut call)?;
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
                .map(Answer::Value),
            Kind::Run => {
                let call: CommandCall = serde_json::from_value(args).map_err(bad)?;
                self.run(call, true).map(Answer::Value)
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
                let reply = self.host.batch(b.calls, b.atomic)?;
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
        }
    }
}

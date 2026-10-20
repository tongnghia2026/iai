//! Running a skill: its params checked and filled, then its steps one at a
//! time, so a run can stop when its time is up and go on at the next call
//! (an agent's tool call cannot wait for ever) with what every step answered.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use iai_engine::{CmdError, CommandCall, ErrorCode, JobId, JobState, Reply};
use serde_json::{json, Value};

use super::recipe::{Over, ParamKind, Recipe, Step};
use super::template::{fill, result_key, Results, Scope};
use super::Store;
use crate::{Gate, Host};

/// How deep skills may call skills.
const MAX_DEPTH: usize = 4;
/// The longest a single wait for a job lasts before the run looks at its clock.
const JOB_WAIT: Duration = Duration::from_secs(110);

/// A run under way.
pub struct Run {
    pub skill: String,
    /// The params as given and filled in, for the history.
    pub params: Value,
    frames: Vec<Frame>,
    results: Results,
    waiting: Option<Waiting>,
    /// Calls that ran.
    pub calls: usize,
    /// Files the run wrote, as the commands answered them.
    pub written: Vec<String>,
}

struct Waiting {
    job: JobId,
    key: String,
    command: String,
}

struct Frame {
    /// Whose step ids these are.
    ns: String,
    steps: Vec<Step>,
    next: usize,
    /// A skill's params (on its first frame).
    names: Vec<(String, Value)>,
    round: Option<Round>,
    /// Starts a skill (counts toward the depth).
    skill: bool,
}

struct Round {
    var: String,
    items: Vec<Value>,
    index: usize,
}

pub enum Progress {
    Done,
    /// Time is up; call `advance` again to go on.
    Paused,
}

/// Where and why a run stopped short.
#[derive(Debug)]
pub struct Failure {
    pub step: String,
    /// 1-based round of the innermost loop, when in one.
    pub round: Option<usize>,
    pub error: CmdError,
}

impl Failure {
    pub fn to_json(&self) -> Value {
        json!({"step": self.step, "round": self.round, "error": self.error})
    }
}

fn bad(text: impl Into<String>) -> CmdError {
    CmdError::new(ErrorCode::BadParams, text)
}

/// The params of `recipe` from `given` (an object, or null for none): the
/// defaults filled in, types checked, folders and files made absolute inside
/// the folders granted.
pub fn resolve_params(
    recipe: &Recipe,
    given: &Value,
    gate: &Gate,
) -> Result<BTreeMap<String, Value>, CmdError> {
    let given = match given {
        Value::Null => serde_json::Map::new(),
        Value::Object(o) => o.clone(),
        _ => return Err(bad("`params` is an object: {\"name\": value}")),
    };
    if let Some(unknown) = given.keys().find(|k| !recipe.params.contains_key(*k)) {
        let known: Vec<&String> = recipe.params.keys().collect();
        return Err(bad(format!(
            "this skill has no param `{unknown}`; it has {known:?}"
        )));
    }
    let mut out = BTreeMap::new();
    for (name, p) in &recipe.params {
        let value = given
            .get(name)
            .cloned()
            .unwrap_or_else(|| p.default.clone());
        if value.is_null() {
            return Err(CmdError::bad_param(name, &format!("needed: {}", p.label)));
        }
        let fits = match p.kind {
            ParamKind::Folder | ParamKind::File | ParamKind::Text => value.is_string(),
            ParamKind::Number => value.is_number(),
            ParamKind::Bool => value.is_boolean(),
            ParamKind::List => value.is_array(),
        };
        if !fits {
            return Err(CmdError::bad_param(
                name,
                &format!("a {:?}, not {value}", p.kind).to_lowercase(),
            ));
        }
        let value = match (p.kind, value.as_str()) {
            (ParamKind::Folder | ParamKind::File, Some(text)) => {
                let path = gate.resolve(text, p.write)?;
                if p.kind == ParamKind::Folder && !p.write && !path.is_dir() {
                    return Err(CmdError::new(
                        ErrorCode::NotFound,
                        format!("`{name}`: no folder {}", path.display()),
                    ));
                }
                Value::String(path.display().to_string())
            }
            _ => value,
        };
        out.insert(name.clone(), value);
    }
    Ok(out)
}

/// Whether every command `recipe` (and the skills it calls) runs exists here
/// and was granted.
fn check_commands(
    store: &Store,
    gate: &Gate,
    recipe: &Recipe,
    depth: usize,
) -> Result<(), CmdError> {
    if depth > MAX_DEPTH {
        return Err(bad(format!(
            "skills call skills more than {MAX_DEPTH} deep"
        )));
    }
    for step in recipe.all_steps() {
        if let Some(id) = &step.command {
            let info = gate.info(id).ok_or_else(|| {
                CmdError::new(
                    ErrorCode::NotFound,
                    format!("step `{}` runs `{id}`, which this iAi does not have: the skill needs fixing", step.id),
                )
                .vi(format!("Kỹ năng cần sửa: lệnh `{id}` không còn trong bản iAi này"))
            })?;
            if !gate.allows(info) {
                let mut call = CommandCall::new(id.clone(), json!({}));
                gate.check(&mut call)?;
            }
        }
        if let Some(name) = &step.skill {
            let inner = store.load(name)?;
            check_commands(store, gate, &inner, depth.saturating_add(1))?;
        }
    }
    Ok(())
}

/// The files a `files` loop goes over: name order, the extensions asked for
/// (any case), or exactly `names` when given.
fn folder_items(dir: &Path, over: &Over) -> Result<Vec<Value>, CmdError> {
    let read = std::fs::read_dir(dir)
        .map_err(|e| CmdError::new(ErrorCode::NotFound, format!("{}: {e}", dir.display())))?;
    let mut files: Vec<(String, std::path::PathBuf)> = read
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| (e.file_name().to_string_lossy().to_string(), e.path()))
        .filter(|(name, _)| !name.starts_with('.'))
        .collect();
    files.sort_by_key(|(name, _)| name.to_lowercase());
    let ext_of = |name: &str| {
        Path::new(name)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    let chosen: Vec<(String, std::path::PathBuf)> = if over.names.is_empty() {
        files
            .into_iter()
            .filter(|(name, _)| over.ext.is_empty() || over.ext.contains(&ext_of(name)))
            .collect()
    } else {
        over.names
            .iter()
            .map(|want| {
                files
                    .iter()
                    .find(|(name, _)| name == want)
                    .cloned()
                    .ok_or_else(|| {
                        CmdError::new(
                            ErrorCode::NotFound,
                            format!("no file {want} in {}", dir.display()),
                        )
                    })
            })
            .collect::<Result<_, _>>()?
    };
    Ok(chosen
        .into_iter()
        .map(|(name, path)| {
            let stem = Path::new(&name)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            json!({"path": path.display().to_string(), "name": name, "stem": stem, "ext": ext_of(&name)})
        })
        .collect())
}

impl Run {
    /// Check `name`'s params and commands; the run, and what it will do.
    pub fn start(
        store: &Store,
        gate: &Gate,
        name: &str,
        given: &Value,
    ) -> Result<(Run, Value), CmdError> {
        let recipe = store.load(name)?;
        let params = resolve_params(&recipe, given, gate)?;
        check_commands(store, gate, &recipe, 0)?;
        // What the loops over folders will take, known before anything runs.
        let mut scope = Scope::default();
        for (k, v) in &params {
            scope.push(k, v.clone());
        }
        let mut loops = Vec::new();
        for step in &recipe.steps {
            if let Some(over) = &step.foreach {
                if let Some(t) = &over.files {
                    if let Ok(Value::String(dir)) =
                        fill(&Value::String(t.clone()), &scope, &Results::new(), name)
                    {
                        let n = folder_items(Path::new(&dir), over).map(|v| v.len()).ok();
                        loops.push(json!({"step": step.id, "folder": dir, "files": n}));
                    }
                }
            }
        }
        let plan = json!({
            "skill": name,
            "params": params,
            "steps": recipe.all_steps().len(),
            "loops": loops,
            "tier": recipe.meta.tier.max(1),
            "howToRun": recipe.meta.how_to_run,
        });
        let run = Run {
            skill: name.to_string(),
            params: serde_json::to_value(&params).unwrap_or(Value::Null),
            frames: vec![Frame {
                ns: name.to_string(),
                steps: recipe.steps,
                next: 0,
                names: params.into_iter().collect(),
                round: None,
                skill: true,
            }],
            results: Results::new(),
            waiting: None,
            calls: 0,
            written: Vec::new(),
        };
        Ok((run, plan))
    }

    fn scope(&self) -> Scope {
        let mut scope = Scope::default();
        for f in &self.frames {
            for (k, v) in &f.names {
                scope.push(k, v.clone());
            }
            if let Some(r) = &f.round {
                if let Some(item) = r.items.get(r.index) {
                    scope.push(&r.var, item.clone());
                    scope.push("i", json!(r.index));
                    scope.push("n", json!(r.index.saturating_add(1)));
                }
            }
        }
        scope
    }

    fn round(&self) -> Option<usize> {
        self.frames
            .iter()
            .rev()
            .find_map(|f| f.round.as_ref().map(|r| r.index.saturating_add(1)))
    }

    fn keep(&mut self, key: String, command: &str, result: Value, gate: &Gate) {
        let writes = gate
            .info(command)
            .is_some_and(|i| i.effects.contains(&iai_engine::Effect::WriteFiles));
        if writes {
            if let Some(p) = result.get("path").and_then(Value::as_str) {
                self.written.push(p.to_string());
            }
        }
        self.results.insert(key, result);
        self.calls = self.calls.saturating_add(1);
    }

    /// Run steps until the skill is done, it fails, or `until` passes.
    pub fn advance(
        &mut self,
        host: &mut dyn Host,
        gate: &Gate,
        store: &Store,
        until: Option<Instant>,
    ) -> Result<Progress, Box<Failure>> {
        loop {
            let late = until.is_some_and(|u| Instant::now() >= u);
            if let Some(w) = self.waiting.take() {
                let wait = until
                    .map(|u| u.saturating_duration_since(Instant::now()).min(JOB_WAIT))
                    .unwrap_or(JOB_WAIT);
                let fail = |error: CmdError, round| {
                    Box::new(Failure {
                        step: w.key.clone(),
                        round,
                        error,
                    })
                };
                match host.job(w.job, wait).map_err(|e| fail(e, self.round()))? {
                    Some(JobState::Done { result, .. }) => {
                        let (key, command) = (w.key.clone(), w.command.clone());
                        self.keep(key, &command, result, gate);
                    }
                    Some(JobState::Running { .. }) => {
                        self.waiting = Some(w);
                        if until.is_some_and(|u| Instant::now() >= u) {
                            return Ok(Progress::Paused);
                        }
                    }
                    Some(JobState::Failed { error }) => return Err(fail(error, self.round())),
                    Some(JobState::Cancelled) => {
                        return Err(fail(
                            CmdError::new(ErrorCode::Cancelled, "the job was cancelled"),
                            self.round(),
                        ))
                    }
                    None => {
                        return Err(fail(
                            CmdError::new(ErrorCode::NotFound, format!("no job {}", w.job.0)),
                            self.round(),
                        ))
                    }
                }
                continue;
            }
            if late {
                return Ok(Progress::Paused);
            }
            if self.calls >= crate::limits::MAX_SCRIPT {
                return Err(Box::new(Failure {
                    step: String::new(),
                    round: self.round(),
                    error: CmdError::new(
                        ErrorCode::TooLarge,
                        format!("a skill runs at most {} calls", crate::limits::MAX_SCRIPT),
                    ),
                }));
            }
            let Some(frame) = self.frames.last_mut() else {
                return Ok(Progress::Done);
            };
            let Some(step) = frame.steps.get(frame.next).cloned() else {
                if let Some(r) = frame.round.as_mut() {
                    r.index = r.index.saturating_add(1);
                    if r.index < r.items.len() {
                        frame.next = 0;
                        continue;
                    }
                }
                self.frames.pop();
                continue;
            };
            frame.next = frame.next.saturating_add(1);
            let ns = frame.ns.clone();
            let key = result_key(&ns, &step.id);
            let round = self.round();
            let fail = |error: CmdError| {
                Box::new(Failure {
                    step: step.id.clone(),
                    round,
                    error,
                })
            };
            let scope = self.scope();
            if let Some(command) = &step.command {
                let args =
                    fill(&step.args, &scope, &self.results, &ns).map_err(|e| fail(bad(e)))?;
                let mut call = CommandCall::new(command.clone(), args);
                gate.check(&mut call).map_err(fail)?;
                call.background = true;
                match host.execute(call).map_err(fail)? {
                    Reply::Done { result, .. } => self.keep(key, command, result, gate),
                    Reply::Started { job } => {
                        self.waiting = Some(Waiting {
                            job,
                            key,
                            command: command.clone(),
                        });
                    }
                }
            } else if let Some(over) = &step.foreach {
                let items = if let Some(t) = &over.files {
                    let dir = fill(&Value::String(t.clone()), &scope, &self.results, &ns)
                        .map_err(|e| fail(bad(e)))?;
                    let dir = dir.as_str().unwrap_or_default().to_string();
                    let dir = gate.resolve(&dir, false).map_err(fail)?;
                    folder_items(&dir, over).map_err(fail)?
                } else if !over.items.is_null() {
                    match fill(&over.items, &scope, &self.results, &ns).map_err(|e| fail(bad(e)))? {
                        Value::Array(items) => items,
                        other => {
                            return Err(fail(bad(format!("a loop goes over a list, not {other}"))))
                        }
                    }
                } else {
                    let count = fill(&over.count, &scope, &self.results, &ns)
                        .map_err(|e| fail(bad(e)))?
                        .as_u64()
                        .ok_or_else(|| fail(bad("a loop's count is a whole number")))?;
                    let count = usize::try_from(count)
                        .unwrap_or(usize::MAX)
                        .min(crate::limits::MAX_SCRIPT);
                    (0..count).map(|i| json!(i)).collect()
                };
                if !items.is_empty() {
                    self.frames.push(Frame {
                        ns,
                        steps: step.steps.clone(),
                        next: 0,
                        names: Vec::new(),
                        round: Some(Round {
                            var: if step.var.is_empty() {
                                "it".to_string()
                            } else {
                                step.var.clone()
                            },
                            items,
                            index: 0,
                        }),
                        skill: false,
                    });
                }
            } else if let Some(name) = &step.skill {
                let depth = self.frames.iter().filter(|f| f.skill).count();
                if depth >= MAX_DEPTH {
                    return Err(fail(bad(format!(
                        "skills call skills more than {MAX_DEPTH} deep"
                    ))));
                }
                let inner = store.load(name).map_err(fail)?;
                let given =
                    fill(&step.params, &scope, &self.results, &ns).map_err(|e| fail(bad(e)))?;
                let params = resolve_params(&inner, &given, gate).map_err(fail)?;
                self.frames.push(Frame {
                    ns: name.clone(),
                    steps: inner.steps,
                    next: 0,
                    names: params.into_iter().collect(),
                    round: None,
                    skill: true,
                });
            }
        }
    }
}

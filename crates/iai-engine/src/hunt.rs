//! Panic hunt: every command is called with params made from its own JSON
//! Schema to probe its edges — each value at its bounds and past them, of
//! the wrong type, missing, null, unknown fields — on a fresh set of
//! documents each time. A command that panics (the engine catches it and
//! answers `internal … panicked`) or does not answer in time is a finding.
//! Tests of this crate and of the app run it; `cargo xtask panic-hunt` runs
//! those tests.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use iai_canvas::document::Document;
use serde_json::{json, Map, Value};

use crate::command::{Effect, Registry};
use crate::session::{CommandCall, Engine};
use crate::workspace::Workspace;
use crate::ErrorCode;

/// A call that panicked or hung.
#[derive(Debug, Clone)]
pub struct Finding {
    pub command: String,
    pub args: Value,
    pub what: String,
}

/// One set of params to try, and whether it is meant to be accepted.
#[derive(Debug, Clone)]
pub struct Probe {
    pub args: Value,
    /// Every value is within the schema: the call may run for real.
    pub valid: bool,
}

/// What a probe runs against: a fresh engine and documents each time.
pub type Fixture = dyn Fn() -> (Engine, Box<dyn Workspace>) + Send + Sync;

/// Run every command named in `commands` (all of the registry's when empty)
/// with every probe of its schema. `scratch` is a folder the file commands
/// may write into. Commands running a local AI model only get their valid
/// probes once or twice: past the params, they run the model.
pub fn hunt(
    registry: &Registry,
    commands: &[&str],
    fixture: Arc<Fixture>,
    scratch: &Path,
    timeout: Duration,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    for cmd in registry.iter() {
        let meta = cmd.meta();
        if !commands.is_empty() && !commands.contains(&meta.id) {
            continue;
        }
        let heavy = meta.effects.contains(&Effect::LocalAi);
        let mut full_runs = 0usize;
        for probe in probes(cmd.params_schema(), scratch) {
            // A model run takes seconds: past one, a command running one is
            // only planned — params read, checks made, the job made but not
            // run — which is all the params reach.
            let plan_only = heavy && (!probe.valid || full_runs >= 1);
            if heavy && !plan_only {
                full_runs = full_runs.saturating_add(1);
            }
            let found = run_one(&fixture, meta.id, probe.args.clone(), timeout, plan_only);
            if let Some(what) = found {
                findings.push(Finding {
                    command: meta.id.to_string(),
                    args: probe.args,
                    what,
                });
            }
        }
    }
    findings
}

/// Run one call on a thread of its own; why it is a finding, if it is.
/// `plan_only`: the command is planned and its job dropped unrun.
fn run_one(
    fixture: &Arc<Fixture>,
    command: &str,
    args: Value,
    timeout: Duration,
    plan_only: bool,
) -> Option<String> {
    let (tx, rx) = mpsc::channel();
    let fixture = Arc::clone(fixture);
    let id = command.to_string();
    let call = CommandCall::new(command, args);
    let spawned = std::thread::Builder::new()
        .name(format!("hunt {command}"))
        .spawn(move || {
            let (mut engine, mut ws) = fixture();
            let reply = if plan_only {
                let registry = engine.registry_arc();
                let mut s = engine.session(ws.as_mut());
                match registry.get(&id) {
                    Some(cmd) => cmd
                        .plan_json(&mut s, call.args)
                        .map(|_| crate::Reply::Done {
                            result: Value::Null,
                            revision: 0,
                        }),
                    None => Ok(crate::Reply::Done {
                        result: Value::Null,
                        revision: 0,
                    }),
                }
            } else {
                // The call's own jobs (background was not asked) run to the end.
                engine.session(ws.as_mut()).execute(call)
            };
            let _ = tx.send(reply);
        });
    if let Err(e) = spawned {
        return Some(format!("could not start a thread: {e}"));
    }
    match rx.recv_timeout(timeout) {
        Ok(Ok(_)) => None,
        Ok(Err(e)) if e.code == ErrorCode::Internal && e.message.contains("panicked") => {
            Some(e.message)
        }
        Ok(Err(_)) => None,
        Err(mpsc::RecvTimeoutError::Timeout) => Some(format!("no answer in {timeout:?}")),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Some("the call's thread died: a panic escaped the engine".to_string())
        }
    }
}

/// Documents worth hunting on, the first shown: a photo with two layers and
/// a selection; a document of three pages; a text document.
pub fn hunting_ground(ws: &mut dyn Workspace) {
    let id = ws.new_document_id();
    let mut doc = Document::new(id, 64, 48);
    doc.canvas.add_layer();
    doc.canvas.select_rect(8, 8, 40, 30);
    ws.add(doc);
    let id = ws.new_document_id();
    let mut pages = Document::new(id, 40, 30);
    pages.add_blank_page();
    pages.add_blank_page();
    ws.add(pages);
    let id = ws.new_document_id();
    ws.add(Document::new_flow_text(id));
    ws.set_active(0);
}

/// The probes of a params schema.
pub fn probes(schema: &Value, scratch: &Path) -> Vec<Probe> {
    let g = Gen {
        defs: schema
            .get("$defs")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
        scratch: scratch.to_path_buf(),
    };
    let schema = g.resolve(schema);
    let mut out = vec![
        Probe {
            args: Value::Null,
            valid: false,
        },
        Probe {
            args: json!([]),
            valid: false,
        },
        Probe {
            args: json!("x"),
            valid: false,
        },
    ];
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        out.push(Probe {
            args: json!({}),
            valid: true,
        });
        return out;
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let base = g.object_sample(props, &required, false);
    let full = g.object_sample(props, &required, true);
    out.push(Probe {
        args: base.clone(),
        valid: true,
    });
    out.push(Probe {
        args: full.clone(),
        valid: true,
    });
    let mut unknown = full.clone();
    if let Some(o) = unknown.as_object_mut() {
        o.insert("bogusField".to_string(), json!(1));
    }
    out.push(Probe {
        args: unknown,
        valid: false,
    });
    for name in &required {
        let mut missing = full.clone();
        if let Some(o) = missing.as_object_mut() {
            o.remove(*name);
        }
        out.push(Probe {
            args: missing,
            valid: false,
        });
    }
    for (name, prop) in props {
        for (value, valid) in g.edges(name, prop, 0) {
            let mut args = full.clone();
            if let Some(o) = args.as_object_mut() {
                o.insert(name.clone(), value);
            }
            out.push(Probe { args, valid });
        }
    }
    out
}

struct Gen {
    defs: Map<String, Value>,
    scratch: PathBuf,
}

impl Gen {
    fn resolve<'a>(&'a self, schema: &'a Value) -> &'a Value {
        let mut s = schema;
        for _ in 0..8 {
            let Some(target) = s
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|r| r.strip_prefix("#/$defs/"))
                .and_then(|n| self.defs.get(n))
            else {
                break;
            };
            s = target;
        }
        s
    }

    /// The schema's branches: `anyOf` / `oneOf`, or itself.
    fn branches<'a>(&'a self, schema: &'a Value) -> Vec<&'a Value> {
        let schema = self.resolve(schema);
        for key in ["oneOf", "anyOf"] {
            if let Some(list) = schema.get(key).and_then(Value::as_array) {
                return list.iter().map(|b| self.resolve(b)).collect();
            }
        }
        vec![schema]
    }

    fn types(schema: &Value) -> Vec<String> {
        match schema.get("type") {
            Some(Value::String(t)) => vec![t.clone()],
            Some(Value::Array(ts)) => ts
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        }
    }

    fn object_sample(&self, props: &Map<String, Value>, required: &[&str], all: bool) -> Value {
        let mut o = Map::new();
        for (name, prop) in props {
            if all || required.contains(&name.as_str()) {
                o.insert(name.clone(), self.sample(name, prop, 0));
            }
        }
        Value::Object(o)
    }

    /// A value the schema accepts.
    fn sample(&self, name: &str, schema: &Value, depth: u32) -> Value {
        if depth > 6 {
            return Value::Null;
        }
        let schema = self.resolve(schema);
        if let Some(c) = schema.get("const") {
            return c.clone();
        }
        if let Some(first) = schema
            .get("enum")
            .and_then(Value::as_array)
            .and_then(|e| e.first())
        {
            return first.clone();
        }
        if schema.get("oneOf").is_some() || schema.get("anyOf").is_some() {
            let branches = self.branches(schema);
            let pick = branches
                .iter()
                .find(|b| Self::types(b) != ["null"])
                .or(branches.first());
            return pick.map_or(Value::Null, |b| {
                self.sample(name, b, depth.saturating_add(1))
            });
        }
        let types = Self::types(schema);
        let t = types
            .iter()
            .find(|t| *t != "null")
            .map(String::as_str)
            .unwrap_or("null");
        match t {
            "object" => {
                let props = schema
                    .get("properties")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                let required: Vec<String> = schema
                    .get("required")
                    .and_then(Value::as_array)
                    .map(|r| {
                        r.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let mut o = Map::new();
                for (n, p) in &props {
                    if required.contains(n) {
                        o.insert(n.clone(), self.sample(n, p, depth.saturating_add(1)));
                    }
                }
                Value::Object(o)
            }
            "array" => {
                let n = schema
                    .get("minItems")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .max(1)
                    .min(3);
                let item = schema.get("items").map_or(Value::Null, |i| {
                    self.sample(name, i, depth.saturating_add(1))
                });
                if let Some(prefix) = schema.get("prefixItems").and_then(Value::as_array) {
                    return Value::Array(
                        prefix
                            .iter()
                            .map(|p| self.sample(name, p, depth.saturating_add(1)))
                            .collect(),
                    );
                }
                Value::Array((0..n).map(|_| item.clone()).collect())
            }
            "string" => {
                if name.to_ascii_lowercase().contains("path") {
                    return json!(self.scratch.join("hunt.png").display().to_string());
                }
                if ["fill", "stroke", "color", "background"].contains(&name) {
                    return json!("#3366CC");
                }
                let n = schema
                    .get("minLength")
                    .and_then(Value::as_u64)
                    .unwrap_or(1)
                    .max(1);
                json!("x".repeat(n as usize))
            }
            "integer" => {
                let min = schema.get("minimum").and_then(Value::as_i64).unwrap_or(0);
                let max = schema
                    .get("maximum")
                    .and_then(Value::as_i64)
                    .unwrap_or(i64::MAX);
                json!(min
                    .max(0)
                    .min(max)
                    .max(min)
                    .max(if name == "doc" { 1 } else { min }))
            }
            "number" => {
                let min = schema.get("minimum").and_then(Value::as_f64).unwrap_or(0.0);
                let max = schema
                    .get("maximum")
                    .and_then(Value::as_f64)
                    .unwrap_or(f64::MAX);
                json!(min.max(0.0).min(max).max(min))
            }
            "boolean" => json!(false),
            _ => Value::Null,
        }
    }

    /// Values for property `name`: at and past its edges, and of the wrong
    /// kind, each with whether the schema accepts it.
    fn edges(&self, name: &str, schema: &Value, depth: u32) -> Vec<(Value, bool)> {
        let mut out = vec![(Value::Null, false), (json!({"bogus": 1}), false)];
        if depth > 2 {
            return out;
        }
        for branch in self.branches(schema) {
            if let Some(options) = branch.get("enum").and_then(Value::as_array) {
                out.extend(options.iter().map(|v| (v.clone(), true)));
                out.push((json!("bogus"), false));
                continue;
            }
            if let Some(c) = branch.get("const") {
                out.push((c.clone(), true));
                out.push((json!("bogus"), false));
                continue;
            }
            let types = Self::types(branch);
            for t in types.iter().map(String::as_str) {
                match t {
                    "integer" | "number" => {
                        let min = branch.get("minimum").and_then(Value::as_f64);
                        let max = branch.get("maximum").and_then(Value::as_f64);
                        let int = t == "integer";
                        let num = |x: f64| if int { json!(x as i64) } else { json!(x) };
                        if let Some(m) = min {
                            out.push((num(m), true));
                            out.push((num(m - 1.0), false));
                        }
                        if let Some(m) = max {
                            out.push((num(m), true));
                            out.push((num(m + 1.0), false));
                        }
                        out.push((json!(0), min.is_none_or(|m| m <= 0.0)));
                        out.push((
                            json!(1),
                            min.is_none_or(|m| m <= 1.0) && max.is_none_or(|m| m >= 1.0),
                        ));
                        out.push((json!(-1), false));
                        out.push((json!(4_294_967_296u64), false));
                        out.push((json!(1e300), false));
                        out.push((json!(-1e300), false));
                        if int {
                            out.push((json!(1.5), false));
                        }
                        out.push((json!("7"), false));
                    }
                    "string" => {
                        let lower = name.to_ascii_lowercase();
                        if lower.contains("path") {
                            let s = &self.scratch;
                            for p in [
                                s.join("hunt.png"),
                                s.join("hunt.iai"),
                                s.join("hunt.pdf"),
                                s.join("hunt.jpg"),
                                s.join("no such folder").join("x.png"),
                                s.join("hunt"),
                                s.to_path_buf(),
                            ] {
                                out.push((json!(p.display().to_string()), true));
                            }
                            out.push((json!(""), false));
                            out.push((json!("\\\\?\\bad\u{0}path.png"), false));
                            out.push((json!("..\\..\\..\\x.png"), false));
                        } else {
                            out.push((json!(""), false));
                            out.push((json!("#00000000"), false));
                            out.push((json!("#GGGGGG"), false));
                            out.push((json!("Ảnh thẻ \u{0} 🎞"), false));
                        }
                        out.push((json!("x".repeat(10_000)), false));
                        out.push((json!(12), false));
                    }
                    "boolean" => {
                        out.push((json!(true), true));
                        out.push((json!(false), true));
                        out.push((json!("true"), false));
                    }
                    "array" => {
                        let item = branch.get("items").map_or(Value::Null, |i| {
                            self.sample(name, i, depth.saturating_add(1))
                        });
                        out.push((json!([]), false));
                        out.push((json!([item.clone()]), false));
                        out.push((Value::Array(vec![item; 200]), false));
                        out.push((json!([null]), false));
                        out.push((json!("[]"), false));
                    }
                    "object" => {
                        out.push((json!({}), false));
                        let sample = self.sample(name, branch, depth.saturating_add(1));
                        out.push((sample.clone(), true));
                        if let (Some(props), Some(base)) = (
                            branch.get("properties").and_then(Value::as_object),
                            sample.as_object(),
                        ) {
                            for (sub, prop) in props {
                                for (v, valid) in self.edges(sub, prop, depth.saturating_add(1)) {
                                    let mut o = base.clone();
                                    o.insert(sub.clone(), v);
                                    out.push((Value::Object(o), valid));
                                }
                            }
                        }
                        out.push((json!([]), false));
                    }
                    _ => {}
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmds::core_registry;
    use crate::{HeadlessWorkspace, Policy, Source};

    #[test]
    fn the_probes_of_a_schema_reach_past_every_bound() {
        let r = core_registry();
        let scratch = std::env::temp_dir();
        let cmd = r.get("select.rect").unwrap();
        let probes = probes(cmd.params_schema(), &scratch);
        assert!(probes.len() > 40, "{}", probes.len());
        let has = |f: &dyn Fn(&Value) -> bool| probes.iter().any(|p| f(&p.args));
        assert!(has(&|a| a["rect"]["w"] == json!(0)));
        assert!(has(&|a| a["rect"]["w"] == json!(100001)));
        assert!(has(&|a| a["featherPx"] == json!(500.0)));
        assert!(has(&|a| a["mode"] == json!("bogus")));
        assert!(has(&|a| a.get("bogusField").is_some()));
        assert!(has(&|a| a.get("rect").is_none() && a.is_object()));
    }

    #[test]
    fn panic_hunt_finds_nothing_in_the_core_commands() {
        let scratch = std::env::temp_dir().join(format!("iai-hunt-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).unwrap();
        let fixture: Arc<Fixture> = Arc::new(|| {
            let mut ws = HeadlessWorkspace::new();
            hunting_ground(&mut ws);
            let policy = Policy::all().revoke(Effect::Network).revoke(Effect::Print);
            (
                Engine::new(core_registry(), Source::Test, policy),
                Box::new(ws) as Box<dyn Workspace>,
            )
        });
        let findings = hunt(
            &core_registry(),
            &[],
            fixture,
            &scratch,
            Duration::from_secs(300),
        );
        let _ = std::fs::remove_dir_all(&scratch);
        assert!(findings.is_empty(), "{findings:#?}");
    }
}

#[cfg(test)]
mod timing {
    use super::*;
    use crate::cmds::core_registry;
    use crate::{HeadlessWorkspace, Policy, Source};

    #[test]
    #[ignore]
    fn probe_hunt_time_per_command() {
        let scratch = std::env::temp_dir().join(format!("iai-hunt-t-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).unwrap();
        let fixture: Arc<Fixture> = Arc::new(|| {
            let mut ws = HeadlessWorkspace::new();
            hunting_ground(&mut ws);
            (
                Engine::new(
                    core_registry(),
                    Source::Test,
                    Policy::all().revoke(Effect::Network),
                ),
                Box::new(ws) as Box<dyn Workspace>,
            )
        });
        let r = core_registry();
        for c in r.iter() {
            let id = c.meta().id;
            let t = std::time::Instant::now();
            let n = probes(c.params_schema(), &scratch).len();
            hunt(
                &r,
                &[id],
                Arc::clone(&fixture),
                &scratch,
                Duration::from_secs(300),
            );
            println!("{id:24} {n:4} probes {:8.2?}", t.elapsed());
        }
        let _ = std::fs::remove_dir_all(&scratch);
    }
}

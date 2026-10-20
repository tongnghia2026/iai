//! Turning what an agent did into a recipe that does it again:
//!
//! 1. an id a call used (`doc`, `layer`, `page`…) that an earlier call
//!    answered becomes a `$ref` to that call's answer;
//! 2. the folders read from and written to become params (`vao`, `ra`), and
//!    so do the values the agent named;
//! 3. calls repeated for each file, each item of an answer, or a number of
//!    rounds become one loop, when every value that changes between rounds
//!    is explained by the round (`${f.stem}`, `${it.doc}`, `${n}`);
//! 4. a single file read or written outside a loop becomes a param of its own
//!    (`tep_vao`, `tep_ra`); params nothing uses are dropped.
//!
//! What cannot be explained stays as it was: the recipe is then less general
//! but still does what was done.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{json, Value};

use super::recipe::{signature, Over, Param, ParamKind, Step};

/// One call as the agent made it (after the gate made its paths absolute),
/// and what it answered.
#[derive(Debug, Clone, Default)]
pub struct Recorded {
    pub command: String,
    pub args: Value,
    /// The command's `result` (unknown while its job runs).
    pub result: Option<Value>,
    pub job: Option<u64>,
    /// It only reads the documents.
    pub query: bool,
    /// It writes files.
    pub writes: bool,
    /// A skill run instead of a command: its name and params.
    pub skill: Option<(String, Value)>,
}

/// A recipe's params and steps, before the library names and files it.
#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    pub params: BTreeMap<String, Param>,
    pub steps: Vec<Step>,
    pub signature: String,
}

/// Ids that name what a document holds.
const ID_KEYS: [&str; 4] = ["doc", "layer", "page", "group"];
/// The longest loop body looked for.
const MAX_BODY: usize = 128;

#[derive(Debug, Clone)]
struct G {
    id: String,
    command: Option<String>,
    args: Value,
    result: Option<Value>,
    query: bool,
    writes: bool,
    skill: Option<String>,
}

/// The pointer to a field `key` holding `v` inside `value`, depth first.
fn find_pointer(value: &Value, key: &str, v: i64, at: &str) -> Option<String> {
    match value {
        Value::Object(o) => {
            if o.get(key).and_then(Value::as_i64) == Some(v) {
                return Some(format!("{at}/{key}"));
            }
            o.iter()
                .find_map(|(k, x)| find_pointer(x, key, v, &format!("{at}/{k}")))
        }
        Value::Array(a) => a
            .iter()
            .enumerate()
            .find_map(|(i, x)| find_pointer(x, key, v, &format!("{at}/{i}"))),
        _ => None,
    }
}

/// The earliest call before this one that answered `key` = `v`: the one
/// that made it. Calls that change things come before queries.
fn producer(before: &[G], key: &str, v: i64) -> Option<(String, String)> {
    for queries in [false, true] {
        for g in before.iter().filter(|g| g.query == queries) {
            if let Some(p) = g.result.as_ref().and_then(|r| find_pointer(r, key, v, "")) {
                return Some((g.id.clone(), p));
            }
        }
    }
    None
}

fn link_ids(value: &mut Value, before: &[G]) {
    match value {
        Value::Object(o) => {
            for (k, x) in o.iter_mut() {
                let id = x.as_i64();
                match id {
                    Some(v) if ID_KEYS.contains(&k.as_str()) => {
                        if let Some((step, path)) = producer(before, k, v) {
                            *x = json!({"$ref": step, "path": path});
                        }
                    }
                    _ => link_ids(x, before),
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| link_ids(x, before)),
        _ => {}
    }
}

fn refs_in(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(o) => {
            if let Some(r) = o.get("$ref").and_then(Value::as_str) {
                out.insert(r.to_string());
            }
            o.values().for_each(|x| refs_in(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| refs_in(x, out)),
        _ => {}
    }
}

fn rename_refs(value: &mut Value, map: &BTreeMap<String, String>) {
    match value {
        Value::Object(o) => {
            if let Some(Value::String(r)) = o.get_mut("$ref") {
                if let Some(to) = map.get(r.as_str()) {
                    *r = to.clone();
                }
            }
            o.values_mut().for_each(|x| rename_refs(x, map));
        }
        Value::Array(a) => a.iter_mut().for_each(|x| rename_refs(x, map)),
        _ => {}
    }
}

/// Every `path` field's text, with whether its call writes.
fn path_leaves(value: &Value, out: &mut Vec<String>) {
    match value {
        // A `$ref`'s `path` points into an answer, not at a file.
        Value::Object(o) if o.contains_key("$ref") => {}
        Value::Object(o) => {
            for (k, x) in o {
                match x {
                    Value::String(s) if k == "path" => out.push(s.clone()),
                    _ => path_leaves(x, out),
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| path_leaves(x, out)),
        _ => {}
    }
}

fn slashes(s: &str) -> String {
    s.replace('\\', "/")
}

/// `path` with `dir` as `${name}`, when it lies in it.
fn under(path: &str, dir: &str, name: &str) -> Option<String> {
    let (p, d) = (slashes(path), slashes(dir));
    if p.eq_ignore_ascii_case(&d) {
        return Some(format!("${{{name}}}"));
    }
    let rest = p
        .get(..d.len())
        .filter(|head| head.eq_ignore_ascii_case(&d))
        .and_then(|_| p.get(d.len()..))?;
    let rest = rest.strip_prefix('/')?;
    Some(format!("${{{name}}}/{rest}"))
}

fn rewrite_paths(value: &mut Value, dirs: &[(String, String)]) {
    match value {
        Value::Object(o) if o.contains_key("$ref") => {}
        Value::Object(o) => {
            for (k, x) in o.iter_mut() {
                match x {
                    Value::String(s) if k == "path" => {
                        // The deepest folder that holds it names it.
                        let best = dirs
                            .iter()
                            .filter_map(|(dir, name)| under(s, dir, name).map(|t| (dir.len(), t)))
                            .max_by_key(|(len, _)| *len);
                        if let Some((_, t)) = best {
                            *s = t;
                        }
                    }
                    _ => rewrite_paths(x, dirs),
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| rewrite_paths(x, dirs)),
        _ => {}
    }
}

/// Literal leaves equal to `want` as `${name}` (not inside `$ref`s, not
/// strings already templated, not paths).
fn name_values(value: &mut Value, want: &Value, name: &str, key: Option<&str>) -> bool {
    match value {
        Value::Object(o) if o.contains_key("$ref") => false,
        Value::Object(o) => {
            let mut any = false;
            for (k, x) in o.iter_mut() {
                any |= name_values(x, want, name, Some(k));
            }
            any
        }
        // Inside a list of objects; a list of numbers (pages, points) is
        // data, not a choice.
        Value::Array(a) if !want.is_array() => {
            let mut any = false;
            for x in a.iter_mut().filter(|x| x.is_object()) {
                any |= name_values(x, want, name, key);
            }
            any
        }
        v if key != Some("path") && *v == *want && !super::template::is_template(v) => {
            *v = Value::String(format!("${{{name}}}"));
            true
        }
        _ => false,
    }
}

fn kind_of(v: &Value) -> ParamKind {
    match v {
        Value::Number(_) => ParamKind::Number,
        Value::Bool(_) => ParamKind::Bool,
        Value::Array(_) => ParamKind::List,
        _ => ParamKind::Text,
    }
}

// ---- loops ---------------------------------------------------------------

/// What the rounds of a loop being looked for go over.
#[derive(Default)]
struct Facts {
    /// A folder template and the file read in each round.
    files: Option<(String, Vec<String>)>,
    /// A step and the pointer to the array whose items the rounds take.
    items: Option<(String, String)>,
}

struct Blocks<'a> {
    blocks: Vec<&'a [G]>,
}

impl Blocks<'_> {
    fn rounds(&self) -> usize {
        self.blocks.len()
    }

    /// Where step `id` sits inside round `b`, if it is one of its steps.
    fn offset(&self, b: usize, id: &str) -> Option<usize> {
        self.blocks.get(b)?.iter().position(|g| g.id == id)
    }

    fn first_id(&self, offset: usize) -> Option<String> {
        self.blocks.first()?.get(offset).map(|g| g.id.clone())
    }

    fn inside(&self, id: &str) -> bool {
        (0..self.rounds()).any(|b| self.offset(b, id).is_some())
    }
}

fn stem(name: &str) -> String {
    Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// `texts` (one a round) as one template: each holds the round's value of
/// one of `names` at the same place, with the same text around it.
fn explain(texts: &[&str], names: &[(String, Vec<String>)]) -> Option<String> {
    for (template, values) in names {
        if values.len() != texts.len() {
            continue;
        }
        // The value's first place, or its last ("p" in "p.png" is both).
        for last in [false, true] {
            let mut around: Option<(String, String)> = None;
            let mut fits = true;
            for (t, v) in texts.iter().zip(values) {
                let at = if v.is_empty() {
                    None
                } else if last {
                    t.rfind(v.as_str())
                } else {
                    t.find(v.as_str())
                };
                let Some(at) = at else {
                    fits = false;
                    break;
                };
                let pre = t.get(..at).unwrap_or("").to_string();
                let suf = t
                    .get(at.saturating_add(v.len())..)
                    .unwrap_or("")
                    .to_string();
                match &around {
                    None => around = Some((pre, suf)),
                    Some((p, s)) if *p == pre && *s == suf => {}
                    Some(_) => {
                        fits = false;
                        break;
                    }
                }
            }
            if fits {
                if let Some((pre, suf)) = around {
                    return Some(format!("{pre}${{{template}}}{suf}"));
                }
            }
        }
    }
    None
}

fn counters(rounds: usize) -> Vec<(String, Vec<String>)> {
    vec![
        (
            "n".to_string(),
            (1..=rounds).map(|n| n.to_string()).collect(),
        ),
        (
            "i".to_string(),
            (0..rounds).map(|i| i.to_string()).collect(),
        ),
    ]
}

/// One value of each round as one template, or `None` when a change between
/// rounds cannot be explained.
fn unify(
    vals: &[&Value],
    key: Option<&str>,
    reads: bool,
    bl: &Blocks,
    facts: &mut Facts,
) -> Option<Value> {
    let first = *vals.first()?;
    let rounds = vals.len();
    // A `$ref`: to a step of the same round, or to one before the loop.
    if vals.iter().all(|v| v.get("$ref").is_some()) {
        let targets: Vec<&str> = vals
            .iter()
            .filter_map(|v| v.get("$ref")?.as_str())
            .collect();
        let paths: Vec<&str> = vals
            .iter()
            .map(|v| v.get("path").and_then(Value::as_str).unwrap_or(""))
            .collect();
        let same_path = paths.windows(2).all(|w| w.first() == w.get(1));
        let offsets: Vec<Option<usize>> = targets
            .iter()
            .enumerate()
            .map(|(b, t)| bl.offset(b, t))
            .collect();
        if offsets.iter().all(Option::is_some) {
            let at = offsets.first().copied().flatten()?;
            if !offsets.iter().all(|o| *o == Some(at)) || !same_path {
                return None;
            }
            return Some(json!({"$ref": bl.first_id(at)?, "path": paths.first()?}));
        }
        let target = *targets.first()?;
        if !targets.iter().all(|t| *t == target) || bl.inside(target) {
            return None;
        }
        if same_path {
            return Some(first.clone());
        }
        // The rounds take the items of an array one by one.
        let split: Vec<Vec<&str>> = paths.iter().map(|p| p.split('/').collect()).collect();
        let len = split.first()?.len();
        if !split.iter().all(|s| s.len() == len) {
            return None;
        }
        let head_parts = split.first()?;
        let varying: Vec<usize> = (0..len)
            .filter(|&j| split.iter().any(|s| s.get(j) != head_parts.get(j)))
            .collect();
        let [j] = varying.as_slice() else { return None };
        let rounds_match = split
            .iter()
            .enumerate()
            .all(|(b, s)| s.get(*j).copied() == Some(b.to_string().as_str()));
        if !rounds_match {
            return None;
        }
        let head = split.first()?;
        let array = head.get(..*j)?.join("/");
        let rest: Vec<&str> = head.get(j.saturating_add(1)..)?.to_vec();
        match &facts.items {
            None => facts.items = Some((target.to_string(), array.clone())),
            Some((t, a)) if t == target && *a == array => {}
            Some(_) => return None,
        }
        return Some(if rest.is_empty() {
            json!("${it}")
        } else {
            json!(format!("${{it.{}}}", rest.join(".")))
        });
    }
    if vals.iter().all(|v| *v == first) {
        return Some(first.clone());
    }
    match first {
        Value::Object(o) => {
            let objects: Vec<&serde_json::Map<String, Value>> =
                vals.iter().filter_map(|v| v.as_object()).collect();
            if objects.len() != rounds || !objects.iter().all(|x| x.len() == o.len()) {
                return None;
            }
            let mut out = serde_json::Map::new();
            for k in o.keys() {
                let column: Vec<&Value> = objects.iter().filter_map(|x| x.get(k)).collect();
                if column.len() != rounds {
                    return None;
                }
                out.insert(k.clone(), unify(&column, Some(k), reads, bl, facts)?);
            }
            Some(Value::Object(out))
        }
        Value::Array(a) => {
            let arrays: Vec<&Vec<Value>> = vals.iter().filter_map(|v| v.as_array()).collect();
            if arrays.len() != rounds || !arrays.iter().all(|x| x.len() == a.len()) {
                return None;
            }
            let mut out = Vec::new();
            for i in 0..a.len() {
                let column: Vec<&Value> = arrays.iter().filter_map(|x| x.get(i)).collect();
                out.push(unify(&column, key, reads, bl, facts)?);
            }
            Some(Value::Array(out))
        }
        Value::String(_) => {
            let texts: Vec<&str> = vals.iter().filter_map(|v| v.as_str()).collect();
            if texts.len() != rounds {
                return None;
            }
            // A path: the same folder, another file each round.
            if key == Some("path") {
                let parts: Vec<(&str, &str)> = texts
                    .iter()
                    .map(|t| t.rsplit_once('/').unwrap_or(("", t)))
                    .collect();
                let dir = parts.first()?.0;
                if !dir.is_empty() && parts.iter().all(|(d, _)| *d == dir) {
                    let names: Vec<&str> = parts.iter().map(|(_, n)| *n).collect();
                    if facts.files.is_none() && reads && facts.items.is_none() {
                        facts.files = Some((
                            dir.to_string(),
                            names.iter().map(|n| n.to_string()).collect(),
                        ));
                        return Some(json!(format!("{dir}/${{f.name}}")));
                    }
                    let mut known = Vec::new();
                    if let Some((_, files)) = &facts.files {
                        known.push(("f.name".to_string(), files.clone()));
                        known.push((
                            "f.stem".to_string(),
                            files.iter().map(|f| stem(f)).collect(),
                        ));
                    }
                    known.extend(counters(rounds));
                    let name = explain(&names, &known)?;
                    return Some(json!(format!("{dir}/{name}")));
                }
            }
            let mut known = Vec::new();
            if let Some((_, files)) = &facts.files {
                known.push(("f.name".to_string(), files.clone()));
                known.push((
                    "f.stem".to_string(),
                    files.iter().map(|f| stem(f)).collect(),
                ));
            }
            known.extend(counters(rounds));
            explain(&texts, &known).map(Value::String)
        }
        Value::Number(_) => {
            let ns: Vec<Option<i64>> = vals.iter().map(|v| v.as_i64()).collect();
            let is = |off: i64| {
                ns.iter()
                    .enumerate()
                    .all(|(b, n)| i64::try_from(b).ok().and_then(|b| b.checked_add(off)) == *n)
            };
            if is(0) {
                Some(json!("${i}"))
            } else if is(1) {
                Some(json!("${n}"))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn same_commands(a: &[G], b: &[G]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x.command == y.command && x.skill == y.skill)
}

/// The value of a param that `template` (`${vao}` or `${vao}/sub`) names.
fn folder_of(template: &str, params: &BTreeMap<String, Param>) -> Option<String> {
    let inner = template.strip_prefix("${")?;
    let (name, rest) = inner.split_once('}')?;
    let base = params.get(name)?.default.as_str()?.to_string();
    Some(format!("{base}{rest}"))
}

/// An earlier step's answer holding the whole number `count`.
fn count_source(before: &[G], count: usize) -> Option<(String, String)> {
    let want = i64::try_from(count).ok()?;
    for key in ["pages", "count", "total", "sheets"] {
        for g in before.iter().rev() {
            if let Some(p) = g
                .result
                .as_ref()
                .and_then(|r| find_pointer(r, key, want, ""))
            {
                return Some((g.id.clone(), p));
            }
        }
    }
    None
}

/// `rounds` blocks of `len` steps from `g` as one loop: its body (block 0's
/// ids) and what it goes over.
fn try_loop(
    g: &[G],
    len: usize,
    rounds: usize,
    before: &[G],
    params: &BTreeMap<String, Param>,
) -> Option<(Vec<Step>, Over, String)> {
    let blocks: Vec<&[G]> = (0..rounds)
        .map(|b| g.get(b.checked_mul(len)?..b.checked_add(1)?.checked_mul(len)?))
        .collect::<Option<_>>()?;
    let bl = Blocks { blocks };
    let mut facts = Facts::default();
    let mut body = Vec::new();
    for q in 0..len {
        let column: Vec<&G> = bl.blocks.iter().filter_map(|b| b.get(q)).collect();
        let head = *column.first()?;
        let vals: Vec<&Value> = column.iter().map(|c| &c.args).collect();
        let args = unify(&vals, None, !head.writes, &bl, &mut facts)?;
        let mut step = Step {
            id: head.id.clone(),
            command: head.command.clone(),
            skill: head.skill.clone(),
            ..Step::default()
        };
        if step.skill.is_some() {
            step.params = args;
        } else {
            step.args = args;
        }
        body.push(step);
    }
    let (over, var) = match (&facts.files, &facts.items) {
        (Some(_), Some(_)) => return None,
        (Some((dir, names)), None) => {
            let mut ext: Vec<String> = names
                .iter()
                .filter_map(|n| {
                    Path::new(n)
                        .extension()
                        .map(|e| e.to_string_lossy().to_lowercase())
                })
                .collect();
            ext.sort();
            ext.dedup();
            // Every such file of the folder, in name order: the whole folder.
            let whole = folder_of(dir, params).and_then(|real| {
                let mut found: Vec<String> = std::fs::read_dir(real)
                    .ok()?
                    .filter_map(Result::ok)
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .filter(|n| !n.starts_with('.'))
                    .filter(|n| {
                        Path::new(n)
                            .extension()
                            .is_some_and(|e| ext.contains(&e.to_string_lossy().to_lowercase()))
                    })
                    .collect();
                found.sort_by_key(|n| n.to_lowercase());
                Some(found == *names)
            });
            let over = Over {
                files: Some(dir.clone()),
                ext: ext.clone(),
                names: if whole == Some(true) {
                    Vec::new()
                } else {
                    names.clone()
                },
                ..Over::default()
            };
            (over, "f")
        }
        (None, Some((step, array))) => (
            Over {
                items: json!({"$ref": step, "path": array}),
                ..Over::default()
            },
            "it",
        ),
        (None, None) => {
            let count = match count_source(before, rounds) {
                Some((step, path)) => json!({"$ref": step, "path": path}),
                None => json!(rounds),
            };
            (
                Over {
                    count,
                    ..Over::default()
                },
                "lan",
            )
        }
    };
    Some((body, over, var.to_string()))
}

// ---- the whole --------------------------------------------------------------

fn to_step(g: &G) -> Step {
    let mut s = Step {
        id: g.id.clone(),
        command: g.command.clone(),
        skill: g.skill.clone(),
        ..Step::default()
    };
    if s.skill.is_some() {
        s.params = g.args.clone();
    } else {
        s.args = g.args.clone();
    }
    s
}

/// `f` on every template a step holds (args, params, what a loop goes over).
pub(crate) fn walk_values(steps: &mut [Step], f: &mut dyn FnMut(&mut Value)) {
    for s in steps.iter_mut() {
        f(&mut s.args);
        f(&mut s.params);
        if let Some(over) = s.foreach.as_mut() {
            f(&mut over.items);
            f(&mut over.count);
            if let Some(files) = over.files.as_mut() {
                let mut v = Value::String(files.clone());
                f(&mut v);
                if let Value::String(t) = v {
                    *files = t;
                }
            }
        }
        walk_values(&mut s.steps, f);
    }
}

/// The one `$ref` in `v` when it reads item 0 of an array: (step, array
/// pointer, the field after the index joined by dots).
fn lone_first_item(v: &Value) -> Option<(String, String, String)> {
    fn each(v: &Value, out: &mut Vec<(String, String)>) {
        match v {
            Value::Object(o) => {
                if let (Some(r), Some(p)) = (
                    o.get("$ref").and_then(Value::as_str),
                    o.get("path").and_then(Value::as_str),
                ) {
                    out.push((r.to_string(), p.to_string()));
                }
                o.values().for_each(|x| each(x, out));
            }
            Value::Array(a) => a.iter().for_each(|x| each(x, out)),
            _ => {}
        }
    }
    let mut refs = Vec::new();
    each(v, &mut refs);
    let [(step, path)] = refs.as_slice() else {
        return None;
    };
    let (array, rest) = path.split_once("/0")?;
    if !(rest.is_empty() || rest.starts_with('/')) {
        return None;
    }
    Some((
        step.clone(),
        array.to_string(),
        rest.trim_start_matches('/').replace('/', "."),
    ))
}

/// Number steps `s1…` and loops `l1…` in order, `$ref`s following.
fn renumber(steps: &mut [Step]) {
    fn assign(
        steps: &mut [Step],
        map: &mut BTreeMap<String, String>,
        s: &mut usize,
        l: &mut usize,
    ) {
        for step in steps.iter_mut() {
            let id = if step.foreach.is_some() {
                *l = l.saturating_add(1);
                format!("l{l}")
            } else {
                *s = s.saturating_add(1);
                format!("s{s}")
            };
            map.insert(step.id.clone(), id.clone());
            step.id = id;
            assign(&mut step.steps, map, s, l);
        }
    }
    let mut map = BTreeMap::new();
    let (mut s, mut l) = (0usize, 0usize);
    assign(steps, &mut map, &mut s, &mut l);
    walk_values(steps, &mut |v| rename_refs(v, &map));
}

fn refs_of(v: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    refs_in(v, &mut out);
    out
}

/// Doc ids in `v` (`doc` fields, at any depth).
fn docs_in(v: &Value, out: &mut BTreeSet<i64>) {
    match v {
        Value::Object(o) => {
            if let Some(d) = o.get("doc").and_then(Value::as_i64) {
                out.insert(d);
            }
            o.values().for_each(|x| docs_in(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| docs_in(x, out)),
        _ => {}
    }
}

/// Whether folding `rounds` blocks of `len` from `g[i..]` into one loop keeps
/// its meaning: after the loop only the last round's answers are kept, so no
/// later step may read an earlier round's.
fn safe_to_fold(g: &[G], i: usize, len: usize, rounds: usize) -> bool {
    let start = i;
    let last = i.saturating_add(rounds.saturating_sub(1).saturating_mul(len));
    let end = i.saturating_add(rounds.saturating_mul(len));
    let early: BTreeSet<&str> = g
        .get(start..last)
        .unwrap_or(&[])
        .iter()
        .map(|x| x.id.as_str())
        .collect();
    g.iter()
        .skip(end)
        .all(|x| refs_of(&x.args).iter().all(|r| !early.contains(r.as_str())))
}

/// Steps that do not lead to what the task wrote: the checks after its last
/// file was written (opening the PDF again to look at it), and documents
/// made to look at (a sheet laid out only to see a folder) that no later
/// step uses before the next document is made.
fn drop_dead_ends(g: &mut Vec<G>) {
    // Files opened again after the last one was written: to look at them.
    // (Other changes after it stay: on the open window they are the result.)
    if let Some(last_write) = g.iter().rposition(|x| x.writes) {
        let mut at = 0usize;
        g.retain(|x| {
            let after = at > last_write;
            at = at.saturating_add(1);
            !(after && x.command.as_deref() == Some("file.open"))
        });
    }
    // Which docs each step brought in (ids not seen before it).
    let mut seen: BTreeSet<i64> = BTreeSet::new();
    let mut made: Vec<BTreeSet<i64>> = Vec::new();
    for x in g.iter() {
        docs_in(&x.args, &mut seen);
        let mut out = BTreeSet::new();
        if let Some(r) = &x.result {
            docs_in(r, &mut out);
        }
        let new: BTreeSet<i64> = out.difference(&seen).copied().collect();
        seen.extend(out);
        made.push(new);
    }
    let all_refs: BTreeSet<String> = g.iter().flat_map(|x| refs_of(&x.args)).collect();
    let dead: BTreeSet<String> = g
        .iter()
        .enumerate()
        .filter(|(k, x)| {
            let brought = made.get(*k).is_some_and(|m| !m.is_empty());
            // The next step that is not a query brings in documents of its
            // own, or names the document it works on: nothing can have
            // worked on these without naming them.
            let next_makes = g
                .iter()
                .enumerate()
                .skip(k.saturating_add(1))
                .find(|(_, y)| !y.query)
                .is_some_and(|(j, y)| {
                    made.get(j).is_some_and(|m| !m.is_empty())
                        || y.args.get("doc").is_some_and(|d| !d.is_null())
                });
            brought && !x.writes && next_makes && !all_refs.contains(&x.id)
        })
        .map(|(_, x)| x.id.clone())
        .collect();
    g.retain(|x| !dead.contains(&x.id));
}

/// An agent that worked in phases — every file opened, then every one made
/// over, then every one written — did per file what one loop does: when the
/// j-th step of each later phase reads only the j-th step of the phases
/// before it, put them back in rounds (open, make, write; open, make, write…).
fn fuse_phases(g: &mut Vec<G>) {
    let mut i = 0usize;
    while i < g.len() {
        let Some(head) = g.get(i) else { break };
        let cmd = head.command.clone();
        let run = g
            .iter()
            .skip(i)
            .take_while(|x| x.command == cmd && x.skill.is_none())
            .count();
        if run < 2 || cmd.is_none() {
            i = i.saturating_add(run.max(1));
            continue;
        }
        let k = run;
        let mut phases: Vec<usize> = vec![i];
        let mut j = i.saturating_add(k);
        while let Some(next) = g.get(j..j.saturating_add(k)) {
            let same = next.len() == k
                && next.iter().all(|x| x.skill.is_none())
                && next
                    .windows(2)
                    .all(|w| w.first().map(|a| &a.command) == w.get(1).map(|b| &b.command));
            if !same {
                break;
            }
            // Round r of this phase reads round r of an earlier phase, and no
            // other round of them.
            let aligned = (0..k).all(|r| {
                let refs = g
                    .get(j.saturating_add(r))
                    .map(|x| refs_of(&x.args))
                    .unwrap_or_default();
                let mut own = false;
                for &p in &phases {
                    for q in 0..k {
                        let Some(id) = g.get(p.saturating_add(q)).map(|x| x.id.clone()) else {
                            continue;
                        };
                        if refs.contains(&id) {
                            if q == r {
                                own = true;
                            } else {
                                return false;
                            }
                        }
                    }
                }
                own
            });
            if !aligned {
                break;
            }
            phases.push(j);
            j = j.saturating_add(k);
        }
        if phases.len() < 2 {
            i = i.saturating_add(k);
            continue;
        }
        let block: Vec<G> = (0..k)
            .flat_map(|r| phases.iter().filter_map(move |&p| p.checked_add(r)))
            .filter_map(|at| g.get(at).cloned())
            .collect();
        let span = phases.len().saturating_mul(k);
        if block.len() == span {
            g.splice(i..i.saturating_add(span), block);
        }
        i = i.saturating_add(span);
    }
}

/// A recipe's draft from `calls`, with `named` (values the agent said a like
/// request would change) as params; `None` when nothing was changed.
pub fn generalize(calls: &[Recorded], named: &BTreeMap<String, Value>) -> Option<Draft> {
    let mut g: Vec<G> = calls
        .iter()
        .enumerate()
        .map(|(i, c)| G {
            id: format!("r{i}"),
            command: if c.skill.is_some() {
                None
            } else {
                Some(c.command.clone())
            },
            args: match &c.skill {
                Some((_, params)) => params.clone(),
                None => c.args.clone(),
            },
            result: c.result.clone(),
            query: c.query && c.skill.is_none(),
            writes: c.writes,
            skill: c.skill.as_ref().map(|(n, _)| n.clone()),
        })
        .collect();
    if !g.iter().any(|x| !x.query) {
        return None;
    }
    // 1. Ids an earlier call answered.
    for i in 0..g.len() {
        let (before, rest) = g.split_at_mut(i);
        if let Some(cur) = rest.first_mut() {
            if cur.skill.is_none() {
                link_ids(&mut cur.args, before);
            }
        }
    }
    let mut used = BTreeSet::new();
    for x in &g {
        refs_in(&x.args, &mut used);
    }
    g.retain(|x| !x.query || used.contains(&x.id));
    drop_dead_ends(&mut g);
    fuse_phases(&mut g);

    // 2. Folders as params.
    let mut params: BTreeMap<String, Param> = BTreeMap::new();
    let mut dirs: Vec<(String, String)> = Vec::new();
    let (mut reads, mut writes) = (0usize, 0usize);
    for x in &g {
        let mut leaves = Vec::new();
        path_leaves(&x.args, &mut leaves);
        if x.skill.is_some() {
            // A skill's params: absolute paths among them.
            leaves.clear();
            if let Value::Object(o) = &x.args {
                leaves.extend(
                    o.values()
                        .filter_map(Value::as_str)
                        .filter(|s| Path::new(s).is_absolute())
                        .map(str::to_string),
                );
            }
        }
        for p in leaves {
            let path = Path::new(&p);
            let dir = if path.is_dir() {
                path.to_path_buf()
            } else {
                match path.parent() {
                    Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
                    _ => continue,
                }
            };
            let dir = dir.display().to_string();
            if let Some((_, name)) = dirs
                .iter()
                .find(|(d, _)| slashes(d).eq_ignore_ascii_case(&slashes(&dir)))
            {
                if x.writes {
                    if let Some(param) = params.get_mut(name) {
                        param.write = true;
                    }
                }
                continue;
            }
            let (base, count) = if x.writes {
                writes = writes.saturating_add(1);
                ("ra", writes)
            } else {
                reads = reads.saturating_add(1);
                ("vao", reads)
            };
            let name = if count == 1 {
                base.to_string()
            } else {
                format!("{base}_{count}")
            };
            params.insert(
                name.clone(),
                Param {
                    kind: ParamKind::Folder,
                    default: json!(dir),
                    label: if x.writes {
                        "Thư mục ghi kết quả".to_string()
                    } else {
                        "Thư mục lấy ảnh / file vào".to_string()
                    },
                    write: x.writes,
                },
            );
            dirs.push((dir, name));
        }
    }
    for x in g.iter_mut() {
        if x.skill.is_some() {
            if let Value::Object(o) = &mut x.args {
                for v in o.values_mut() {
                    if let Some(s) = v.as_str() {
                        let best = dirs
                            .iter()
                            .filter_map(|(dir, name)| under(s, dir, name).map(|t| (dir.len(), t)))
                            .max_by_key(|(len, _)| *len);
                        if let Some((_, t)) = best {
                            *v = json!(t);
                        }
                    }
                }
            }
        } else {
            rewrite_paths(&mut x.args, &dirs);
        }
    }

    // The values the agent named.
    for (name, want) in named {
        let name = super::text::slug(name).replace('-', "_");
        if let Some(text) = want.as_str() {
            let renamed = dirs
                .iter()
                .find(|(d, _)| slashes(d).eq_ignore_ascii_case(&slashes(text)))
                .map(|(_, n)| n.clone());
            if let Some(old) = renamed {
                if old != name && !params.contains_key(&name) {
                    if let Some(p) = params.remove(&old) {
                        params.insert(name.clone(), p);
                    }
                    let (from, to) = (format!("${{{old}}}"), format!("${{{name}}}"));
                    for x in g.iter_mut() {
                        replace_text(&mut x.args, &from, &to);
                    }
                    for d in dirs.iter_mut().filter(|(_, n)| *n == old) {
                        d.1 = name.clone();
                    }
                }
                continue;
            }
        }
        if params.contains_key(&name) {
            continue;
        }
        let mut any = false;
        for x in g.iter_mut() {
            any |= name_values(&mut x.args, want, &name, None);
        }
        if any {
            params.insert(
                name.clone(),
                Param {
                    kind: kind_of(want),
                    default: want.clone(),
                    label: name.replace('_', " "),
                    write: false,
                },
            );
        }
    }

    // 3. Loops.
    let mut steps: Vec<Step> = Vec::new();
    let mut i = 0usize;
    while i < g.len() {
        let rest = g.get(i..).unwrap_or(&[]);
        let before = g.get(..i).unwrap_or(&[]);
        let mut best: Option<(usize, usize, Vec<Step>, Over, String)> = None;
        let max_len = (rest.len() / 2).min(MAX_BODY);
        for len in 1..=max_len {
            let Some(first) = rest.get(..len) else { break };
            let mut rounds = 1usize;
            while let Some(next) =
                rest.get(rounds.saturating_mul(len)..rounds.saturating_add(1).saturating_mul(len))
            {
                if !same_commands(first, next) {
                    break;
                }
                rounds = rounds.saturating_add(1);
            }
            if rounds < 2 {
                continue;
            }
            let covers = len.saturating_mul(rounds);
            if best
                .as_ref()
                .is_some_and(|(l, r, ..)| l.saturating_mul(*r) >= covers)
            {
                continue;
            }
            for k in (2..=rounds).rev() {
                let span = len.saturating_mul(k);
                if best
                    .as_ref()
                    .is_some_and(|(l, r, ..)| l.saturating_mul(*r) >= span)
                {
                    break;
                }
                if !safe_to_fold(&g, i, len, k) {
                    continue;
                }
                if let Some((body, over, var)) = try_loop(rest, len, k, before, &params) {
                    best = Some((len, k, body, over, var));
                    break;
                }
            }
        }
        match best {
            Some((len, rounds, body, over, var)) => {
                // Later steps that read a round's step read the last round's:
                // the loop keeps the latest answer under the body's ids.
                let mut map = BTreeMap::new();
                for b in 1..rounds {
                    for q in 0..len {
                        let at = i.saturating_add(b.saturating_mul(len)).saturating_add(q);
                        let to = body.get(q).map(|s| s.id.clone());
                        if let (Some(from), Some(to)) = (g.get(at).map(|x| x.id.clone()), to) {
                            map.insert(from, to);
                        }
                    }
                }
                let end = i.saturating_add(len.saturating_mul(rounds));
                for later in g.iter_mut().skip(end) {
                    rename_refs(&mut later.args, &map);
                }
                steps.push(Step {
                    id: format!("loop{i}"),
                    foreach: Some(over),
                    var,
                    steps: body,
                    ..Step::default()
                });
                i = end;
            }
            None => {
                if let Some(x) = g.get(i) {
                    steps.push(to_step(x));
                }
                i = i.saturating_add(1);
            }
        }
    }

    // A step that took the first item of a one-item answer goes over all.
    let results: BTreeMap<String, Value> = g
        .iter()
        .filter_map(|x| Some((x.id.clone(), x.result.clone()?)))
        .collect();
    let mut wrapped = Vec::new();
    for s in steps {
        let lone = s.command.as_ref().and(lone_first_item(&s.args));
        let fits = lone.as_ref().is_some_and(|(r, array, _)| {
            results
                .get(r)
                .and_then(|v| v.pointer(array))
                .and_then(Value::as_array)
                .is_some_and(|a| a.len() == 1)
        });
        match (lone, fits) {
            (Some((r, array, rest)), true) => {
                let mut inner = s.clone();
                let field = if rest.is_empty() {
                    "${it}".to_string()
                } else {
                    format!("${{it.{rest}}}")
                };
                replace_ref(&mut inner.args, &r, &format!("{array}/0"), &field);
                wrapped.push(Step {
                    id: format!("loop_{}", s.id),
                    foreach: Some(Over {
                        items: json!({"$ref": r, "path": array}),
                        ..Over::default()
                    }),
                    var: "it".to_string(),
                    steps: vec![inner],
                    ..Step::default()
                });
            }
            _ => wrapped.push(s),
        }
    }
    let mut steps = wrapped;

    // 4. A single file outside a loop is a param of its own.
    let mut files: Vec<(String, String)> = Vec::new();
    let (mut file_reads, mut file_writes) = (0usize, 0usize);
    let writes_of: BTreeMap<String, bool> = g.iter().map(|x| (x.id.clone(), x.writes)).collect();
    for s in steps.iter_mut().filter(|s| s.command.is_some()) {
        let write = writes_of.get(&s.id).copied().unwrap_or(false);
        let mut leaves = Vec::new();
        path_leaves(&s.args, &mut leaves);
        for leaf in leaves {
            let Some(inner) = leaf.strip_prefix("${") else {
                continue;
            };
            let Some((dir_param, rest)) = inner.split_once("}/") else {
                continue;
            };
            if rest.contains('/') || rest.contains("${") || rest.is_empty() {
                continue;
            }
            let Some(base) = params
                .get(dir_param)
                .and_then(|p| p.default.as_str())
                .map(str::to_string)
            else {
                continue;
            };
            let name = match files.iter().find(|(l, _)| *l == leaf) {
                Some((_, n)) => n.clone(),
                None => {
                    let (b, c) = if write {
                        file_writes = file_writes.saturating_add(1);
                        ("tep_ra", file_writes)
                    } else {
                        file_reads = file_reads.saturating_add(1);
                        ("tep_vao", file_reads)
                    };
                    let n = if c == 1 {
                        b.to_string()
                    } else {
                        format!("{b}_{c}")
                    };
                    params.insert(
                        n.clone(),
                        Param {
                            kind: ParamKind::File,
                            default: json!(format!("{}/{rest}", slashes(&base))),
                            label: if write {
                                "File ghi ra".to_string()
                            } else {
                                "File lấy vào".to_string()
                            },
                            write,
                        },
                    );
                    files.push((leaf.clone(), n.clone()));
                    n
                }
            };
            replace_text(&mut s.args, &leaf, &format!("${{{name}}}"));
        }
    }

    renumber(&mut steps);
    let mut names = Vec::new();
    walk_values(&mut steps, &mut |v| {
        super::template::names_used(v, &mut names)
    });
    params.retain(|k, _| names.contains(k));
    let signature = signature(&steps);
    Some(Draft {
        params,
        steps,
        signature,
    })
}

fn replace_text(value: &mut Value, from: &str, to: &str) {
    match value {
        Value::String(s) if s.contains(from) => *s = s.replace(from, to),
        Value::Array(a) => a.iter_mut().for_each(|x| replace_text(x, from, to)),
        Value::Object(o) => o.values_mut().for_each(|x| replace_text(x, from, to)),
        _ => {}
    }
}

fn replace_ref(value: &mut Value, step: &str, path_head: &str, with: &str) {
    match value {
        Value::Object(o) => {
            let hit = o.get("$ref").and_then(Value::as_str) == Some(step)
                && o.get("path")
                    .and_then(Value::as_str)
                    .is_some_and(|p| p.starts_with(path_head));
            if hit {
                *value = json!(with);
                return;
            }
            o.values_mut()
                .for_each(|x| replace_ref(x, step, path_head, with));
        }
        Value::Array(a) => a
            .iter_mut()
            .for_each(|x| replace_ref(x, step, path_head, with)),
        _ => {}
    }
}

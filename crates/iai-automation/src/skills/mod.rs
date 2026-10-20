//! The skill library: what agents did well, kept as recipes iAi can run
//! again without an agent writing the steps anew.
//!
//! A skill is a folder in the library (`Documents\iAi_Ky_nang` unless told
//! otherwise): `kich_ban.json` (the [`recipe::Recipe`]), `SKILL.md` written
//! from it (the Agent Skills layout, for agents and people), `lich_su.md`
//! (each use) and, for a tier-2 skill, the programs it runs beside iAi.
//!
//! A task an agent finishes well is saved by [`Store::learn`]: the same job
//! again only counts a use, the same job with other values gains params, a
//! job close to one already kept becomes a version of it, anything else a
//! new skill. Plan: docs/planning/KE_HOACH_4F_VA_KHO_KY_NANG_2026-10-20.md.

pub mod learn;
pub mod recipe;
pub mod run;
pub mod template;
pub mod text;

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use iai_engine::{CmdError, ErrorCode};
use serde_json::{json, Value};

pub use learn::{Draft, Recorded};
pub use recipe::{Meta, Param, Recipe, Status, Step};
pub use run::{Failure, Progress, Run};

const RECIPE: &str = "kich_ban.json";
const SKILL_MD: &str = "SKILL.md";
const HISTORY: &str = "lich_su.md";
/// Where removed skills go (a folder the library skips), not the bin.
pub const REMOVED: &str = ".da_xoa";
/// What a recipe file may weigh.
const MAX_RECIPE: u64 = 8 << 20;
/// A helper copied into a skill: at most this many, this large.
const MAX_HELPERS: usize = 40;
const MAX_HELPER: u64 = 4 << 20;
/// Examples a skill keeps.
const MAX_EXAMPLES: usize = 8;

/// `Documents\iAi_Ky_nang` in the user's folder. Not `Documents\iAi\…`: names
/// do not tell case apart on Windows, and `Documents\IAI` may be iAi's own code.
pub fn default_root() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    home.join("Documents").join("iAi_Ky_nang")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn when(secs: u64) -> String {
    iai_diag::local_minute(
        SystemTime::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(secs))
            .unwrap_or(SystemTime::UNIX_EPOCH),
    )
}

fn io(path: &Path, e: impl std::fmt::Display) -> CmdError {
    CmdError::new(ErrorCode::Io, format!("{}: {e}", path.display()))
}

/// A skill found by a search.
#[derive(Debug, Clone)]
pub struct Found {
    pub name: String,
    pub score: f32,
    pub recipe: Recipe,
}

impl Found {
    /// What an agent sees of it in a list: a few lines.
    pub fn brief(&self) -> Value {
        let m = &self.recipe.meta;
        json!({
            "name": self.name,
            "title": m.title,
            "description": m.description,
            "descriptionEn": m.description_en,
            "params": self.recipe.params_brief(),
            "tier": m.tier.max(1),
            "status": m.status.vi(m.uses),
            "uses": m.uses,
            "match": (self.score * 100.0).round() / 100.0,
        })
    }
}

/// What [`Store::learn`] did with a task.
#[derive(Debug, Clone, PartialEq)]
pub enum Saved {
    /// A new skill.
    Created(String),
    /// A new version of the second skill.
    Version(String, String),
    /// The same job as this skill, with values that are now its params.
    Updated(String, Vec<String>),
    /// The same job as this skill: one more use.
    Reused(String),
}

impl Saved {
    pub fn name(&self) -> &str {
        match self {
            Saved::Created(n) | Saved::Version(n, _) | Saved::Updated(n, _) | Saved::Reused(n) => n,
        }
    }

    pub fn to_json(&self, store: &Store) -> Value {
        let (action, vi) = match self {
            Saved::Created(_) => ("created", "Đã lưu kỹ năng mới".to_string()),
            Saved::Version(_, base) => (
                "version",
                format!("Đã lưu phiên bản mới của kỹ năng «{base}»"),
            ),
            Saved::Updated(_, params) => (
                "updated",
                format!("Kỹ năng đã có; thêm tham số {}", params.join(", ")),
            ),
            Saved::Reused(_) => ("reused", "Kỹ năng đã có; ghi thêm một lần dùng".to_string()),
        };
        json!({
            "action": action,
            "name": self.name(),
            "folder": store.root().join(self.name()).display().to_string(),
            "vi": vi,
        })
    }
}

/// What a finished task says about itself, for [`Store::learn`].
#[derive(Debug, Clone, Default)]
pub struct Lesson {
    pub request: String,
    pub name: String,
    pub title: String,
    pub description: String,
    pub description_en: String,
    pub checks: Vec<String>,
    /// Programs the agent wrote and ran beside iAi (absolute, already
    /// checked to lie in a granted folder).
    pub helpers: Vec<PathBuf>,
    pub how_to_run: String,
    pub app: String,
    pub client: String,
    /// Saved at the end of a session, not by the agent ending its task.
    pub unconfirmed: bool,
}

/// The library, in a folder.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn dir(&self, name: &str) -> Result<PathBuf, CmdError> {
        if name.is_empty() || text::slug(name) != name {
            return Err(CmdError::new(
                ErrorCode::NotFound,
                format!("no skill `{name}` (a skill's name is its folder: a-z, 0-9, -)"),
            ));
        }
        Ok(self.root.join(name))
    }

    /// Every skill, by name. A folder whose recipe does not read is left out.
    pub fn list(&self) -> Vec<(String, Recipe)> {
        let Ok(read) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut out: Vec<(String, Recipe)> = read
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    return None;
                }
                let recipe = self.load(&name).ok()?;
                Some((name, recipe))
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    pub fn load(&self, name: &str) -> Result<Recipe, CmdError> {
        let file = self.dir(name)?.join(RECIPE);
        let size = std::fs::metadata(&file)
            .map_err(|_| CmdError::new(ErrorCode::NotFound, format!("no skill `{name}`")))?
            .len();
        if size > MAX_RECIPE {
            return Err(CmdError::new(
                ErrorCode::TooLarge,
                format!("{}: too large", file.display()),
            ));
        }
        let text = std::fs::read_to_string(&file).map_err(|e| io(&file, e))?;
        let recipe: Recipe = serde_json::from_str(&text)
            .map_err(|e| CmdError::new(ErrorCode::BadParams, format!("{}: {e}", file.display())))?;
        if recipe.version > recipe::VERSION {
            return Err(CmdError::new(
                ErrorCode::NotEnabled,
                format!(
                    "skill `{name}` needs a newer iAi (recipe version {})",
                    recipe.version
                ),
            ));
        }
        Ok(recipe)
    }

    /// Write `recipe` and its `SKILL.md`.
    pub fn write(&self, name: &str, recipe: &Recipe) -> Result<(), CmdError> {
        let dir = self.dir(name)?;
        std::fs::create_dir_all(&dir).map_err(|e| io(&dir, e))?;
        let json =
            serde_json::to_string_pretty(recipe).map_err(|e| CmdError::internal(e.to_string()))?;
        let file = dir.join(RECIPE);
        iai_platform::atomic_file::write_bytes_atomic(&file, json.as_bytes())
            .map_err(|e| io(&file, e))?;
        let md = recipe::skill_md(name, recipe, &when);
        let file = dir.join(SKILL_MD);
        iai_platform::atomic_file::write_bytes_atomic(&file, md.as_bytes())
            .map_err(|e| io(&file, e))?;
        Ok(())
    }

    /// A line in the skill's history.
    pub fn note(&self, name: &str, line: &str) {
        let Ok(dir) = self.dir(name) else { return };
        let file = dir.join(HISTORY);
        let head = if file.exists() {
            ""
        } else {
            "# Lịch sử dùng\n\n"
        };
        let text = format!("{head}- {} · {}\n", when(now()), line.trim());
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file)
        {
            let _ = f.write_all(text.as_bytes());
        }
    }

    /// The `SKILL.md` text of a skill, written again if it is missing.
    pub fn skill_md(&self, name: &str) -> Result<String, CmdError> {
        let file = self.dir(name)?.join(SKILL_MD);
        match std::fs::read_to_string(&file) {
            Ok(t) => Ok(t),
            Err(_) => {
                let recipe = self.load(name)?;
                self.write(name, &recipe)?;
                Ok(recipe::skill_md(name, &recipe, &when))
            }
        }
    }

    /// The skills that best fit `request`, best first; every skill when it
    /// says nothing.
    pub fn search(&self, request: &str, max: usize) -> Vec<Found> {
        let asked = text::words(request);
        let mut found: Vec<Found> = self
            .list()
            .into_iter()
            .map(|(name, recipe)| {
                let score = if asked.is_empty() {
                    0.0
                } else {
                    score(&asked, &name, &recipe)
                };
                Found {
                    name,
                    score,
                    recipe,
                }
            })
            .filter(|f| asked.is_empty() || f.score >= 0.25)
            .collect();
        found.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(b.recipe.meta.uses.cmp(&a.recipe.meta.uses))
                .then(b.recipe.meta.last_used_at.cmp(&a.recipe.meta.last_used_at))
        });
        found.truncate(max);
        found
    }

    /// Note a use of `name`: it went well (`ok`), or failed in a way that
    /// says the recipe is wrong (`broken`); `request`, when given, becomes
    /// one of its examples.
    pub fn record_use(
        &self,
        name: &str,
        ok: bool,
        broken: bool,
        request: &str,
        how: &str,
    ) -> Result<(), CmdError> {
        let mut r = self.load(name)?;
        let m = &mut r.meta;
        m.uses = m.uses.saturating_add(1);
        m.last_used_at = now();
        if ok {
            if matches!(
                m.status,
                Status::AutoSaved | Status::Unconfirmed | Status::NeedsFix
            ) {
                m.status = Status::InUse;
            }
        } else {
            m.failures = m.failures.saturating_add(1);
            if broken {
                m.status = Status::NeedsFix;
            }
        }
        add_example(&mut m.examples, request);
        self.write(name, &r)?;
        let verdict = if ok {
            "đạt"
        } else if broken {
            "hỏng — cần sửa"
        } else {
            "không xong"
        };
        self.note(name, &format!("dùng lại · {verdict} · {how}"));
        Ok(())
    }

    /// Move a skill out of the library (into `.da_xoa`, where it can be
    /// taken back from).
    pub fn remove(&self, name: &str) -> Result<PathBuf, CmdError> {
        let dir = self.dir(name)?;
        if !dir.join(RECIPE).is_file() {
            return Err(CmdError::new(
                ErrorCode::NotFound,
                format!("no skill `{name}`"),
            ));
        }
        let bin = self.root.join(REMOVED);
        std::fs::create_dir_all(&bin).map_err(|e| io(&bin, e))?;
        let mut to = bin.join(name);
        let mut k = 2u32;
        while to.exists() {
            to = bin.join(format!("{name}-{k}"));
            k = k.saturating_add(1);
        }
        std::fs::rename(&dir, &to).map_err(|e| io(&dir, e))?;
        Ok(to)
    }

    fn free_name(&self, wanted: &str) -> String {
        let base = text::slug(wanted);
        let mut name = base.clone();
        let mut k = 2u32;
        while self.root.join(&name).exists() {
            name = format!("{base}-{k}");
            k = k.saturating_add(1);
        }
        name
    }

    /// Keep what a finished task did (see the module's notes).
    pub fn learn(&self, draft: Draft, lesson: &Lesson) -> Result<Saved, CmdError> {
        let how = format!(
            "{} lệnh · {}",
            draft.steps.len(),
            if lesson.client.is_empty() {
                "AI"
            } else {
                &lesson.client
            }
        );
        // The whole task was saved skills, one after another: one more use of
        // each, and nothing new (they are each kept already).
        let ran: Vec<&String> = draft
            .steps
            .iter()
            .filter_map(|s| s.skill.as_ref().filter(|_| s.command.is_none()))
            .collect();
        if let Some(first) = ran.first().filter(|_| ran.len() == draft.steps.len()) {
            let mut counted: Vec<&String> = Vec::new();
            for name in &ran {
                if !counted.contains(name) {
                    self.record_use(name, true, false, &lesson.request, &how)?;
                    counted.push(name);
                }
            }
            return Ok(Saved::Reused((*first).clone()));
        }
        let skills = self.list();
        for (name, existing) in &skills {
            if existing.meta.signature != draft.signature {
                continue;
            }
            match compare(&existing.steps, &draft.steps) {
                Some(diffs) if diffs.is_empty() => {
                    let mut r = existing.clone();
                    // The folders last used are the ones offered next.
                    for (k, p) in &draft.params {
                        if let Some(old) = r.params.get_mut(k) {
                            old.default = p.default.clone();
                        }
                    }
                    self.write(name, &r)?;
                    self.record_use(name, true, false, &lesson.request, &how)?;
                    return Ok(Saved::Reused(name.clone()));
                }
                Some(diffs) if diffs.len() <= 6 => {
                    let mut r = existing.clone();
                    let added = make_params(&mut r, &diffs);
                    for (k, p) in &draft.params {
                        if let Some(old) = r.params.get_mut(k) {
                            old.default = p.default.clone();
                        }
                    }
                    self.write(name, &r)?;
                    self.record_use(name, true, false, &lesson.request, &how)?;
                    self.note(name, &format!("thêm tham số {}", added.join(", ")));
                    return Ok(Saved::Updated(name.clone(), added));
                }
                _ => {}
            }
        }
        // Close to one already kept: a version of it.
        let mine = commands_of(&draft.steps);
        let asked = text::words(&lesson.request);
        let base = skills
            .iter()
            .map(|(name, r)| {
                let theirs = commands_of(&r.steps);
                let shared = mine.intersection(&theirs).count();
                let all = mine.union(&theirs).count().max(1);
                let overlap = shared as f32 / all as f32;
                let fits = if asked.is_empty() {
                    0.0
                } else {
                    score(&asked, name, r)
                };
                (name.clone(), overlap, fits)
            })
            .filter(|(_, overlap, fits)| *overlap >= 0.6 && *fits >= 0.35)
            .max_by(|a, b| (a.1 + a.2).total_cmp(&(b.1 + b.2)))
            .map(|(name, ..)| name)
            // A task that ran a kept skill and did more is a version of it.
            .or_else(|| draft.steps.iter().find_map(|s| s.skill.clone()));

        let wanted = [&lesson.name, &lesson.title, &lesson.request]
            .into_iter()
            .find(|s| !s.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| "viec-tu-luu".to_string());
        let name = match &base {
            Some(b) if lesson.name.trim().is_empty() => self.free_name(b),
            _ => self.free_name(&wanted),
        };
        let title = if lesson.title.trim().is_empty() {
            short_title(&lesson.request)
        } else {
            lesson.title.trim().to_string()
        };
        let mut helpers = Vec::new();
        let dir = self.dir(&name)?;
        if lesson.helpers.len() > MAX_HELPERS {
            return Err(CmdError::new(
                ErrorCode::TooLarge,
                format!("a skill keeps at most {MAX_HELPERS} files beside it"),
            ));
        }
        for h in &lesson.helpers {
            let size = std::fs::metadata(h).map_err(|e| io(h, e))?.len();
            if !h.is_file() || size > MAX_HELPER {
                return Err(CmdError::new(
                    ErrorCode::BadParams,
                    format!(
                        "{}: a helper is a file of at most {MAX_HELPER} bytes",
                        h.display()
                    ),
                ));
            }
            let Some(file_name) = h.file_name() else {
                continue;
            };
            let to = dir.join("helpers").join(file_name);
            std::fs::create_dir_all(dir.join("helpers")).map_err(|e| io(&dir, e))?;
            std::fs::copy(h, &to).map_err(|e| io(h, e))?;
            helpers.push(format!("helpers/{}", file_name.to_string_lossy()));
        }
        let mut examples = Vec::new();
        add_example(&mut examples, &lesson.request);
        let recipe = Recipe {
            version: recipe::VERSION,
            meta: Meta {
                title,
                description: if lesson.description.trim().is_empty() {
                    lesson.request.trim().to_string()
                } else {
                    lesson.description.trim().to_string()
                },
                description_en: lesson.description_en.trim().to_string(),
                examples,
                tier: if helpers.is_empty() { 1 } else { 2 },
                status: if lesson.unconfirmed {
                    Status::Unconfirmed
                } else {
                    Status::AutoSaved
                },
                uses: 0,
                failures: 0,
                created_at: now(),
                last_used_at: 0,
                app: lesson.app.clone(),
                based_on: base.clone(),
                helpers,
                how_to_run: lesson.how_to_run.trim().to_string(),
                checks: lesson.checks.clone(),
                signature: draft.signature.clone(),
            },
            params: draft.params,
            steps: draft.steps,
        };
        self.write(&name, &recipe)?;
        let from = if lesson.request.trim().is_empty() {
            String::new()
        } else {
            format!(" · từ lời: \"{}\"", lesson.request.trim())
        };
        self.note(&name, &format!("tạo · {how}{from}"));
        Ok(match base {
            Some(b) => Saved::Version(name, b),
            None => Saved::Created(name),
        })
    }
}

fn add_example(examples: &mut Vec<String>, request: &str) {
    let request = request.trim();
    if request.is_empty() {
        return;
    }
    let folded = text::fold(request);
    examples.retain(|e| text::fold(e) != folded);
    examples.push(request.to_string());
    while examples.len() > MAX_EXAMPLES {
        examples.remove(0);
    }
}

/// A title from a request: its first words.
fn short_title(request: &str) -> String {
    let words: Vec<&str> = request.split_whitespace().take(10).collect();
    if words.is_empty() {
        "Việc lưu tự động".to_string()
    } else {
        words.join(" ")
    }
}

fn commands_of(steps: &[Step]) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for s in steps {
        let mut all = Vec::new();
        s.walk(&mut all);
        for x in all {
            if let Some(c) = &x.command {
                out.insert(c.clone());
            }
            if let Some(k) = &x.skill {
                out.insert(format!("skill:{k}"));
            }
        }
    }
    out
}

/// How well a skill fits the words of a request, 0 to 1.
fn score(asked: &[String], name: &str, r: &Recipe) -> f32 {
    let m = &r.meta;
    let fields: [(f32, Vec<String>); 5] = [
        (
            3.0,
            text::words(&format!("{} {}", m.title, m.examples.join(" "))),
        ),
        (
            2.0,
            text::words(&format!("{} {}", m.description, m.description_en)),
        ),
        (2.0, text::words(&name.replace('-', " "))),
        (
            1.0,
            text::words(&m.signature.replace(['.', '_', ',', '(', ')'], " ")),
        ),
        (
            1.0,
            text::words(
                &r.params
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .replace('_', " "),
            ),
        ),
    ];
    let hit = |a: &str, b: &str| {
        a == b || (a.len() >= 4 && b.len() >= 4 && (a.starts_with(b) || b.starts_with(a)))
    };
    let total: f32 = asked
        .iter()
        .map(|w| {
            fields
                .iter()
                .filter(|(_, ws)| ws.iter().any(|x| hit(w, x)))
                .map(|(weight, _)| *weight)
                .fold(0.0, f32::max)
        })
        .sum();
    total / (3.0 * asked.len().max(1) as f32)
}

/// Where two recipes of one shape differ: the literal values a param could
/// take (pointer into the steps as JSON, old value), or `None` when they
/// differ in more than values.
fn compare(old: &[Step], new: &[Step]) -> Option<Vec<(String, Value)>> {
    let a = serde_json::to_value(old).ok()?;
    let b = serde_json::to_value(new).ok()?;
    let mut diffs = Vec::new();
    diff(&a, &b, "", false, &mut diffs).then_some(diffs)
}

fn diff(a: &Value, b: &Value, at: &str, in_args: bool, out: &mut Vec<(String, Value)>) -> bool {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            if x.contains_key("$ref") || y.contains_key("$ref") {
                return a == b;
            }
            let keys: std::collections::BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            keys.into_iter().all(|k| {
                if k == "id" {
                    return true;
                }
                // A loop over named files is the same loop over other files.
                if k == "names" {
                    return true;
                }
                let inside = in_args || k == "args" || k == "params";
                match (x.get(k), y.get(k)) {
                    (Some(p), Some(q)) => diff(p, q, &format!("{at}/{k}"), inside, out),
                    _ => false,
                }
            })
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y)
                    .enumerate()
                    .all(|(i, (p, q))| diff(p, q, &format!("{at}/{i}"), in_args, out))
        }
        _ if a == b => true,
        // A value the kept skill already takes as a param.
        (Value::String(s), lit) if in_args && is_param(s) && is_literal(lit) => true,
        // A value the new task named as a param, a literal in the kept skill.
        (lit, Value::String(s)) if in_args && is_param(s) && is_literal(lit) => {
            out.push((at.to_string(), a.clone()));
            true
        }
        (Value::String(s), _) | (_, Value::String(s)) if s.contains("${") => false,
        (
            Value::String(_) | Value::Number(_) | Value::Bool(_),
            Value::String(_) | Value::Number(_) | Value::Bool(_),
        ) if in_args => {
            out.push((at.to_string(), a.clone()));
            true
        }
        _ => false,
    }
}

/// `${name}` and nothing else.
fn is_param(s: &str) -> bool {
    s.strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
        .is_some_and(|name| !name.is_empty() && !name.contains(['$', '{', '}']))
}

/// A plain value: text without `${`, a number, true or false.
fn is_literal(v: &Value) -> bool {
    match v {
        Value::String(s) => !s.contains("${"),
        Value::Number(_) | Value::Bool(_) => true,
        _ => false,
    }
}

/// Turn the values at `diffs` into params of `r`, named after their field.
fn make_params(r: &mut Recipe, diffs: &[(String, Value)]) -> Vec<String> {
    let Ok(mut steps) = serde_json::to_value(&r.steps) else {
        return Vec::new();
    };
    let mut added = Vec::new();
    for (pointer, old) in diffs {
        let field = pointer
            .rsplit('/')
            .find(|seg| seg.parse::<usize>().is_err())
            .unwrap_or("gia_tri");
        let base = text::slug(field).replace('-', "_");
        let mut name = base.clone();
        let mut k = 2u32;
        while r.params.contains_key(&name) && r.params.get(&name).map(|p| &p.default) != Some(old) {
            name = format!("{base}_{k}");
            k = k.saturating_add(1);
        }
        if let Some(slot) = steps.pointer_mut(pointer) {
            *slot = json!(format!("${{{name}}}"));
        }
        if !r.params.contains_key(&name) {
            r.params.insert(
                name.clone(),
                Param {
                    kind: match old {
                        Value::Number(_) => recipe::ParamKind::Number,
                        Value::Bool(_) => recipe::ParamKind::Bool,
                        _ => recipe::ParamKind::Text,
                    },
                    default: old.clone(),
                    label: field.to_string(),
                    write: false,
                },
            );
            added.push(name);
        }
    }
    if let Ok(s) = serde_json::from_value(steps) {
        r.steps = s;
    }
    added
}

#[cfg(test)]
mod tests;

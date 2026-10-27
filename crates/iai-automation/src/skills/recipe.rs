//! A skill as its folder keeps it: `kich_ban.json` (what this module reads
//! and writes) and the `SKILL.md` written from it for agents and people.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The recipe format this build writes.
pub const VERSION: u32 = 1;

/// Everything about one skill.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recipe {
    pub version: u32,
    pub meta: Meta,
    #[serde(default)]
    pub params: BTreeMap<String, Param>,
    #[serde(default)]
    pub steps: Vec<Step>,
}

/// How far a skill can be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Saved by iAi when a task went well; not used again yet.
    #[default]
    AutoSaved,
    /// Saved at the end of a session whose task was never closed.
    Unconfirmed,
    /// Used again, and it went well.
    InUse,
    /// Its last use failed in a way that says the recipe is wrong.
    NeedsFix,
    /// The person said it is good.
    Approved,
}

impl Status {
    pub fn vi(self, uses: u32) -> String {
        match self {
            Status::AutoSaved => "tự lưu".to_string(),
            Status::Unconfirmed => "tự lưu, chưa xác nhận".to_string(),
            Status::InUse => format!("đã dùng {uses} lần"),
            Status::NeedsFix => "cần sửa".to_string(),
            Status::Approved => format!("đã duyệt, đã dùng {uses} lần"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Meta {
    /// Vietnamese, a few words.
    pub title: String,
    /// One sentence, Vietnamese.
    pub description: String,
    pub description_en: String,
    /// What people asked for when this skill did the job.
    pub examples: Vec<String>,
    /// 1: only iAi commands. 2: programs beside it (`helpers`) run outside iAi.
    pub tier: u8,
    pub status: Status,
    /// Times it was used after it was saved, and how many of those failed.
    pub uses: u32,
    pub failures: u32,
    /// Seconds since 1970.
    pub created_at: u64,
    pub last_used_at: u64,
    /// The iAi build it was saved or last ran well on.
    pub app: String,
    /// The skill this one was made from.
    pub based_on: Option<String>,
    /// Files beside the recipe, relative to the skill's folder.
    pub helpers: Vec<String>,
    /// For a tier-2 skill: how to run its programs (`${…}` filled in).
    pub how_to_run: String,
    /// How the task was checked to be right.
    pub checks: Vec<String>,
    /// The commands it runs, loops shown: tells a skill's shape at a glance.
    pub signature: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamKind {
    Folder,
    File,
    #[default]
    Text,
    Number,
    Bool,
    List,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Param {
    #[serde(rename = "type")]
    pub kind: ParamKind,
    /// What is used when the call leaves it out; `null` makes it required.
    pub default: Value,
    /// Vietnamese.
    pub label: String,
    /// A folder or file results are written to (checked against the folders
    /// granted for writing).
    pub write: bool,
}

/// One step: a command, a loop over steps, or another skill.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Step {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub args: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreach: Option<Over>,
    /// The loop's name for the item it is on (`${f.stem}`, `${it.doc}`).
    #[serde(rename = "as", skip_serializing_if = "String::is_empty")]
    pub var: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<Step>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub params: Value,
}

/// What a loop goes over (one of these).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Over {
    /// The files of a folder (a template), sorted by name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<String>,
    /// Their extensions, lower case, no dot; empty takes every file.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ext: Vec<String>,
    /// Only these names, in this order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
    /// The items of an array (a template, usually a step's result).
    #[serde(skip_serializing_if = "Value::is_null")]
    pub items: Value,
    /// A number of rounds (a template).
    #[serde(skip_serializing_if = "Value::is_null")]
    pub count: Value,
}

impl Step {
    pub fn command(id: &str, command: &str, args: Value) -> Self {
        Self {
            id: id.to_string(),
            command: Some(command.to_string()),
            args,
            ..Self::default()
        }
    }

    /// Every step inside this one, this one first.
    pub fn walk<'a>(&'a self, out: &mut Vec<&'a Step>) {
        out.push(self);
        for s in &self.steps {
            s.walk(out);
        }
    }
}

impl Recipe {
    pub fn all_steps(&self) -> Vec<&Step> {
        let mut out = Vec::new();
        for s in &self.steps {
            s.walk(&mut out);
        }
        out
    }

    /// It runs on a folder the person picks, with no agent: iAi's commands
    /// only, not marked broken, a folder of photos to read, and every other
    /// param a folder for the results or a value a form takes (no single
    /// file, no list: those are an agent's to fill).
    pub fn runs_on_a_folder(&self) -> bool {
        let fillable = self.params.values().all(|p| {
            matches!(
                p.kind,
                ParamKind::Folder | ParamKind::Text | ParamKind::Number | ParamKind::Bool
            )
        });
        let reads_a_folder = self
            .params
            .values()
            .any(|p| p.kind == ParamKind::Folder && !p.write);
        self.meta.tier <= 1
            && self.meta.status != Status::NeedsFix
            && !self.steps.is_empty()
            && fillable
            && reads_a_folder
    }

    /// `name: kind = default` for each param, for a short listing.
    pub fn params_brief(&self) -> Value {
        let mut out = serde_json::Map::new();
        for (name, p) in &self.params {
            out.insert(
                name.clone(),
                serde_json::json!({"type": p.kind, "default": p.default, "label": p.label}),
            );
        }
        Value::Object(out)
    }
}

/// The signature of `steps`: command ids in order, loops as `each(…)`.
pub fn signature(steps: &[Step]) -> String {
    let parts: Vec<String> = steps
        .iter()
        .map(|s| {
            if let Some(c) = &s.command {
                c.clone()
            } else if let Some(k) = &s.skill {
                format!("skill:{k}")
            } else {
                format!("each({})", signature(&s.steps))
            }
        })
        .collect();
    parts.join(",")
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `SKILL.md` for the skill in folder `name`: the Agent Skills layout (a
/// `name` / `description` header), then what an agent or a person needs.
pub fn skill_md(name: &str, r: &Recipe, when: &dyn Fn(u64) -> String) -> String {
    let m = &r.meta;
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("name: {name}\n"));
    let mut description = one_line(&m.description);
    if !m.description_en.is_empty() {
        description.push_str(" / ");
        description.push_str(&one_line(&m.description_en));
    }
    out.push_str(&format!(
        "description: {}\n",
        description.replace(':', " -")
    ));
    out.push_str("---\n\n");
    out.push_str(&format!("# {}\n\n", one_line(&m.title)));
    out.push_str(&format!("{}\n\n", m.description.trim()));
    if !m.description_en.is_empty() {
        out.push_str(&format!("{}\n\n", m.description_en.trim()));
    }
    if !m.examples.is_empty() {
        out.push_str("## Người dùng từng nói\n\n");
        for e in &m.examples {
            out.push_str(&format!("- \"{}\"\n", one_line(e)));
        }
        out.push('\n');
    }
    if !r.params.is_empty() {
        out.push_str("## Tham số\n\n| Tên | Loại | Mặc định | Nghĩa |\n|---|---|---|---|\n");
        for (pname, p) in &r.params {
            let kind = serde_json::to_value(p.kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            let default = match &p.default {
                Value::Null => "(phải đưa)".to_string(),
                Value::String(s) => s.clone(),
                v => v.to_string(),
            };
            out.push_str(&format!(
                "| `{pname}` | {kind} | `{}` | {} |\n",
                default.replace('|', "/"),
                one_line(&p.label)
            ));
        }
        out.push('\n');
    }
    out.push_str("## Cách chạy\n\n");
    if !r.steps.is_empty() {
        out.push_str(&format!(
            "Qua cổng MCP của iAi: `iai_skill_run {{\"name\": \"{name}\", \"params\": {{…}}}}`. \
             Không có AI: `iai.exe run --skill {name} --param ten=gia_tri …`.\n\n"
        ));
    }
    if !m.how_to_run.trim().is_empty() {
        out.push_str(&format!("{}\n\n", m.how_to_run.trim()));
    }
    if !m.helpers.is_empty() {
        out.push_str("File đi kèm (trong thư mục này): ");
        out.push_str(
            &m.helpers
                .iter()
                .map(|h| format!("`{h}`"))
                .collect::<Vec<_>>()
                .join(", "),
        );
        out.push_str("\n\n");
    }
    if !m.checks.is_empty() {
        out.push_str("## Kiểm \"đạt\"\n\n");
        for c in &m.checks {
            out.push_str(&format!("- {}\n", one_line(c)));
        }
        out.push('\n');
    }
    out.push_str("## Trạng thái\n\n");
    out.push_str(&format!("- {}", m.status.vi(m.uses)));
    if m.created_at > 0 {
        out.push_str(&format!(", lưu {}", when(m.created_at)));
    }
    if m.last_used_at > 0 {
        out.push_str(&format!(", dùng gần nhất {}", when(m.last_used_at)));
    }
    if !m.app.is_empty() {
        out.push_str(&format!(", bản iAi {}", m.app));
    }
    out.push('\n');
    if let Some(base) = &m.based_on {
        out.push_str(&format!("- Làm từ kỹ năng `{base}`.\n"));
    }
    if !m.signature.is_empty() {
        out.push_str(&format!("- Các lệnh: `{}`\n", m.signature));
    }
    out.push_str("\n_iAi viết lại file này từ `kich_ban.json` mỗi lần kỹ năng thay đổi._\n");
    out
}

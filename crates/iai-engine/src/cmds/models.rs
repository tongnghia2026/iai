//! The AI models of this machine as commands (the "Model AI" window, A3–A5
//! of the plan of 30/10): one of the author's list fetched, or a file the
//! person has (`.onnx`, or a `.zip` of models) installed, each checked and
//! put in the main model folder (`iai_ai::model_store`).

use std::sync::atomic::{AtomicBool, Ordering};

use iai_ai::model_store;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    Apply, CmdError, Command, CommandMeta, Effect, ErrorCode, Job, Plan, Session, Validate,
};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FetchParams {
    /// The model's id in the author's list (see `models.list`).
    #[schemars(length(min = 1, max = 200))]
    pub id: String,
}
impl Validate for FetchParams {}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Fetched {
    pub id: String,
    /// Where the file is now.
    pub path: String,
}

static FETCH: CommandMeta = CommandMeta {
    id: "models.fetch",
    label_vi: "Tải model AI",
    label_en: "Fetch an AI Model",
    doc: "Fetch one model of the author's list into the main model folder, trying its sources \
          in turn (the model's author, the public repository's releases, the author's Drive), \
          every file checked by its SHA-256; a fetch broken off goes on from where it stopped. \
          Needs the network.",
    effects: &[Effect::Network, Effect::WriteFiles],
    featured: false,
};

pub struct FetchModel;

impl Command for FetchModel {
    type Params = FetchParams;
    type Output = Fetched;

    fn meta(&self) -> &'static CommandMeta {
        &FETCH
    }

    fn plan(&self, _: &mut Session<'_>, p: FetchParams) -> Result<Plan<Fetched>, CmdError> {
        Ok(Plan::Job(Job {
            doc: None,
            label_vi: "Đang tải model AI".into(),
            work: Box::new(move |cx| {
                let stop = AtomicBool::new(false);
                let progress = |have: u64, total: u64| {
                    if cx.cancelled() {
                        stop.store(true, Ordering::Relaxed);
                    }
                    let fraction = if total > 0 {
                        have as f32 / total as f32
                    } else {
                        0.0
                    };
                    cx.progress(
                        fraction,
                        format!(
                            "Đang tải model {}: {} / {} MB",
                            p.id,
                            have / 1_000_000,
                            total / 1_000_000
                        ),
                    );
                };
                let path = model_store::status::download_id(&p.id, &progress, &stop)
                    .map_err(|e| CmdError::new(ErrorCode::Io, e.text()).vi(e.text()))?;
                model_store::rescan();
                let (id, path) = (p.id, path.display().to_string());
                Ok(Box::new(move |_: &mut Session<'_>| Ok(Fetched { id, path })) as Apply<Fetched>)
            }),
        }))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct InstallParams {
    /// A model file (`.onnx`) or a `.zip` of them, on this machine.
    #[schemars(length(min = 1, max = 4096))]
    pub path: String,
}
impl Validate for InstallParams {}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Installed {
    /// One line for people: "Đã cài 3 model, bỏ qua 1 file".
    pub summary: String,
    /// What went in, and where.
    pub installed: Vec<String>,
    /// Files left out, each with why.
    pub skipped: Vec<String>,
}

static INSTALL: CommandMeta = CommandMeta {
    id: "models.install",
    label_vi: "Cài model AI từ file",
    label_en: "Install AI Models From A File",
    doc: "Install a model file (`.onnx`, with its weights file beside it) or a `.zip` of models: \
          each recognised by its SHA-256 or, a model of a known family, by the shape of its \
          inputs and outputs, and copied into the right folder of the main model folder. Files \
          not usable are named with why (a `.pth` not yet converted to ONNX…).",
    effects: &[Effect::ReadFiles, Effect::WriteFiles],
    featured: false,
};

pub struct InstallModels;

impl Command for InstallModels {
    type Params = InstallParams;
    type Output = Installed;

    fn meta(&self) -> &'static CommandMeta {
        &INSTALL
    }

    fn plan(&self, _: &mut Session<'_>, p: InstallParams) -> Result<Plan<Installed>, CmdError> {
        let path = std::path::PathBuf::from(&p.path);
        if !path.is_file() {
            return Err(
                CmdError::new(ErrorCode::NotFound, format!("{}: no such file", p.path))
                    .vi(format!("Không thấy file {}", p.path)),
            );
        }
        Ok(Plan::Job(Job {
            doc: None,
            label_vi: "Đang cài model AI".into(),
            work: Box::new(move |cx| {
                cx.progress(0.1, "Đang kiểm file model…");
                let report = model_store::install::install(&path)
                    .map_err(|e| CmdError::new(ErrorCode::Io, e.clone()).vi(e))?;
                model_store::rescan();
                let made = Installed {
                    summary: report.summary(),
                    installed: report
                        .installed
                        .iter()
                        .map(|i| format!("{} → {}", i.what, i.to.display()))
                        .collect(),
                    skipped: report
                        .skipped
                        .iter()
                        .map(|s| format!("{}: {}", s.name, s.why))
                        .collect(),
                };
                Ok(Box::new(move |_: &mut Session<'_>| Ok(made)) as Apply<Installed>)
            }),
        }))
    }
}

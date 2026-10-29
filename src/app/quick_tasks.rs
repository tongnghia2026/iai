//! "Việc nhanh": jobs iAi does by itself on a folder the person picks, with
//! no AI agent and no network: photos of papers made clean, an ID photo of
//! everyone in it, and the saved skills that run on a folder. A job runs on
//! an engine of its own on another thread, so the window stays free; what
//! it made that is not a file opened from the folder (a sheet of ID photos)
//! comes back as tabs, and its files go to the folder's `iAi_ket_qua`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use iai_automation::skills::recipe::ParamKind;
use iai_automation::skills::{Progress, Recipe, Run, Store};
use iai_automation::{Gate, Host, LocalHost, Roots};
use iai_engine::{Engine, ErrorCode, Policy, Source, Workspace};
use serde_json::{Value, json};

use super::errands::RESULTS;
use super::state::App;
use crate::core::document::Document;
use crate::ui::{
    QuickField, QuickForm, QuickIntent, QuickRunView, QuickSkill, QuickTask, QuickValue, QuickView,
};

/// How long a job runs between looks at "Dừng".
const SLICE: Duration = Duration::from_millis(250);
/// How often the library is read again while the window is open.
const SKILLS_FRESH: Duration = Duration::from_secs(5);
/// The most tabs a job's results open.
const MAX_TABS: usize = 12;
/// The photos a folder's job takes.
pub(crate) const PHOTOS: [&str; 7] = ["jpg", "jpeg", "png", "webp", "bmp", "tif", "tiff"];
/// The most portraits one ID-photo job takes (as `id_photo.batch`).
const MAX_PORTRAITS: usize = 40;
/// The name a job is noted under in a skill's history.
const HOW: &str = "Việc nhanh";

enum Stage {
    Working(String),
    Done(String),
    Failed(String),
    Stopped,
}

/// The job under way, or the last one.
struct Under {
    title: String,
    results: PathBuf,
    stage: Stage,
    stop: Arc<AtomicBool>,
    rx: Option<Receiver<Msg>>,
}

/// "Việc nhanh"'s state in the app.
#[derive(Default)]
pub(crate) struct QuickTasks {
    pub(crate) open: bool,
    preset: Option<(u64, QuickTask)>,
    under: Option<Under>,
    /// The saved skills offered, and when the library was read.
    skills: Option<(Instant, Vec<QuickSkill>)>,
    /// What a finished job made, held while the person is in the middle of
    /// an edit (a new tab would take the window from under them).
    made: Vec<Document>,
}

impl QuickTasks {
    fn running(&self) -> bool {
        self.under
            .as_ref()
            .is_some_and(|u| matches!(u.stage, Stage::Working(_)))
    }
}

/// What the thread is to run.
struct Job {
    task: QuickTask,
    /// The skill's name (the library's, or the recipe's below).
    name: String,
    /// A recipe iAi carries; `None` runs the library's skill `name`.
    recipe: Option<Recipe>,
    params: Value,
    read: PathBuf,
    write: PathBuf,
}

enum Msg {
    Stage(String),
    Ended(Ended),
}

struct Ended {
    stage: Stage,
    /// Documents the job made, for tabs of the window.
    docs: Vec<Document>,
}

/// The photos in `dir`, by name.
fn photos_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = read
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().to_string());
            let hidden = name.is_some_and(|n| n.starts_with('.'));
            let ext = p
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            !hidden && PHOTOS.contains(&ext.as_str())
        })
        .collect();
    out.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    out
}

/// Every photo of a paper in `vao` made clean, a JPEG of the same name in
/// `ra`.
fn scan_recipe() -> Recipe {
    let open = json!({"$ref": "open", "path": "/doc"});
    // A recipe written here, checked by the test below: it always reads.
    serde_json::from_value(json!({
        "version": 1,
        "meta": {
            "title": "Làm sạch ảnh chụp giấy tờ",
            "description": "Làm sạch mọi ảnh chụp giấy tờ trong thư mục: nền trắng, chữ rõ.",
            "tier": 1
        },
        "params": {
            "vao": {"type": "folder", "default": null, "label": "Thư mục ảnh"},
            "ra": {"type": "folder", "default": null, "label": "Thư mục kết quả", "write": true},
            "look": {"type": "text", "default": "grayscale", "label": "Kiểu"}
        },
        "steps": [
            {"id": "each", "foreach": {"files": "${vao}", "ext": PHOTOS}, "as": "f", "steps": [
                {"id": "open", "command": "file.open", "args": {"path": "${f.path}"}},
                {"id": "clean", "command": "image.scan_cleanup", "args": {"doc": open, "look": "${look}"}},
                {"id": "write", "command": "file.export", "args": {"doc": open, "path": "${ra}/${f.stem}.jpg"}},
                {"id": "close", "command": "doc.close", "args": {"doc": open, "discard": true}}
            ]}
        ]
    }))
    .unwrap_or_else(|_| Recipe {
        version: 1,
        meta: Default::default(),
        params: Default::default(),
        steps: Vec::new(),
    })
}

/// The ID cards of the photos in `vao` laid out on A4, each front beside
/// its back, in one document shown as a tab: no PDF (the person looks it
/// over and prints it).
fn id_card_recipe() -> Recipe {
    serde_json::from_value(json!({
        "version": 1,
        "meta": {
            "title": "Xếp căn cước lên A4",
            "description": "Mọi ảnh căn cước trong thư mục, mặt trước cạnh mặt sau, một tab.",
            "tier": 1
        },
        "params": {
            "vao": {"type": "folder", "default": null, "label": "Thư mục ảnh"},
            "ban": {"type": "number", "default": 1, "label": "Số bản mỗi người"}
        },
        "steps": [
            {"id": "make", "command": "print.card_folder", "args": {
                "path": "${vao}", "copies": "${ban}"
            }}
        ]
    }))
    .unwrap_or_else(|_| Recipe {
        version: 1,
        meta: Default::default(),
        params: Default::default(),
        steps: Vec::new(),
    })
}

/// An ID photo of each portrait in `anh`, laid out on photo paper, the
/// sheets a tab: no PDF (the person looks them over, fixes a print from its
/// right-click menu, and prints them).
fn id_photo_recipe() -> Recipe {
    serde_json::from_value(json!({
        "version": 1,
        "meta": {
            "title": "Ảnh thẻ cho nhiều người",
            "description": "Ảnh thẻ cho mọi ảnh chân dung trong thư mục, xếp lên giấy ảnh, một tab.",
            "tier": 1
        },
        "params": {
            "anh": {"type": "list", "default": null, "label": "Ảnh chân dung"},
            "size": {"type": "text", "default": "3x4", "label": "Cỡ"},
            "backdrop": {"type": "text", "default": "white", "label": "Nền"},
            "paper": {"type": "text", "default": "10x15", "label": "Giấy"}
        },
        "steps": [
            {"id": "make", "command": "id_photo.batch", "args": {
                "items": "${anh}", "size": "${size}", "backdrop": "${backdrop}", "paper": "${paper}"
            }}
        ]
    }))
    .unwrap_or_else(|_| Recipe {
        version: 1,
        meta: Default::default(),
        params: Default::default(),
        steps: Vec::new(),
    })
}

/// The skills of the library that run on a folder, used most first, and
/// their forms.
fn read_skills(store: &Store) -> Vec<QuickSkill> {
    let mut skills: Vec<(String, Recipe)> = store
        .list()
        .into_iter()
        .filter(|(_, r)| r.runs_on_a_folder())
        .collect();
    skills.sort_by_key(|(_, r)| {
        (
            std::cmp::Reverse(r.meta.uses),
            std::cmp::Reverse(r.meta.last_used_at.max(r.meta.created_at)),
        )
    });
    skills
        .into_iter()
        .map(|(name, r)| QuickSkill {
            title: if r.meta.title.trim().is_empty() {
                name.clone()
            } else {
                r.meta.title.clone()
            },
            description: r.meta.description.clone(),
            status: r.meta.status.vi(r.meta.uses),
            fields: r
                .params
                .iter()
                .filter_map(|(pname, p)| {
                    let value = match p.kind {
                        ParamKind::Text => {
                            QuickValue::Text(p.default.as_str().unwrap_or_default().to_string())
                        }
                        ParamKind::Number => QuickValue::Number(p.default.as_f64().unwrap_or(0.0)),
                        ParamKind::Bool => QuickValue::Bool(p.default.as_bool().unwrap_or(false)),
                        _ => return None,
                    };
                    Some(QuickField {
                        name: pname.clone(),
                        label: if p.label.trim().is_empty() {
                            pname.clone()
                        } else {
                            p.label.clone()
                        },
                        value,
                    })
                })
                .collect(),
            name,
        })
        .collect()
}

/// A saved skill's params: its folders the one chosen (to read) and its
/// results' (to write), the rest as filled in.
fn skill_params(recipe: &Recipe, fields: &[QuickField], folder: &Path, results: &Path) -> Value {
    let mut out = serde_json::Map::new();
    for (name, p) in &recipe.params {
        let value = match p.kind {
            ParamKind::Folder if p.write => json!(results.display().to_string()),
            ParamKind::Folder => json!(folder.display().to_string()),
            _ => match fields.iter().find(|f| &f.name == name).map(|f| &f.value) {
                Some(QuickValue::Text(t)) => json!(t),
                Some(QuickValue::Number(n)) if n.fract() == 0.0 => json!(*n as i64),
                Some(QuickValue::Number(n)) => json!(n),
                Some(QuickValue::Bool(b)) => json!(b),
                None => continue,
            },
        };
        out.insert(name.clone(), value);
    }
    Value::Object(out)
}

fn error_line(e: &iai_engine::CmdError) -> String {
    e.message_vi.clone().unwrap_or_else(|| e.message.clone())
}

/// Where a running job stands, in the person's words.
fn stage_line(run: &Run) -> String {
    match run.rounds() {
        Some((i, n)) => format!("Đang làm ảnh {i} / {n}…"),
        None => run.doing.clone().unwrap_or_else(|| "Đang làm…".to_string()),
    }
}

/// What a finished job made.
fn done_line(task: &QuickTask, run: &Run) -> String {
    match task {
        QuickTask::ScanCleanup => {
            let n = run.written.len();
            format!("Xong: {n} ảnh đã làm sạch, lưu trong thư mục {RESULTS}.")
        }
        QuickTask::IdPhotos => {
            let made = run.result("make");
            let count = |key: &str| made.and_then(|m| m.get(key)).and_then(Value::as_u64);
            let people = made
                .and_then(|m| m.get("people"))
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            let mut line = format!(
                "Xong: {} tấm của {people} người trên {} tờ (tab mới). Tấm nào chưa ưng: \
                 chuột phải lên tấm đó ▸ «Sửa ảnh thẻ này…».",
                count("prints").unwrap_or(0),
                count("sheets").unwrap_or(0),
            );
            let failed: Vec<String> = made
                .and_then(|m| m.get("failed"))
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|f| {
                            let path = f.get("path").and_then(Value::as_str)?;
                            let name = Path::new(path).file_name()?.to_string_lossy().to_string();
                            let why = f.get("why").and_then(Value::as_str).unwrap_or("");
                            Some(format!("{name} ({why})"))
                        })
                        .collect()
                })
                .unwrap_or_default();
            if !failed.is_empty() {
                line.push_str(&format!(" Không làm được: {}.", failed.join(", ")));
            }
            line
        }
        QuickTask::IdCards => {
            let made = run.result("make");
            let count = |key: &str| made.and_then(|m| m.get(key)).and_then(Value::as_u64);
            let names = |key: &str| -> Vec<String> {
                made.and_then(|m| m.get(key))
                    .and_then(Value::as_array)
                    .map(|v| {
                        v.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let mut line = format!(
                "Xong: {} người trên {} trang A4 (tab mới); {} người ghép đủ hai mặt theo số \
                 căn cước.",
                count("people").unwrap_or(0),
                count("pages").unwrap_or(0),
                count("byNumber").unwrap_or(0),
            );
            let twice = names("twice").len();
            if twice > 0 {
                line.push_str(&format!(
                    " {twice} ảnh chụp trùng thẻ đã có (gửi hai lần) — không xếp lại."
                ));
            }
            let look = names("unsure").len();
            if look > 0 {
                line.push_str(&format!(
                    " {look} thẻ cần xem (viền đỏ): bấm chuột phải lên thẻ để sửa."
                ));
            }
            let skipped = names("skipped");
            if !skipped.is_empty() {
                line.push_str(&format!(
                    " Không phải căn cước / không tìm thấy thẻ: {}.",
                    skipped.join(", ")
                ));
            }
            line
        }
        QuickTask::Skill(_) => format!(
            "Xong: {} lệnh, {} file ghi vào thư mục {RESULTS}.",
            run.calls,
            run.written.len()
        ),
    }
}

/// The documents `ws` holds that the job made, not opened from a file.
fn made_documents(ws: &mut dyn Workspace) -> Vec<Document> {
    let mut docs = Vec::new();
    let mut index = ws.documents().len();
    while index > 0 {
        index -= 1;
        if ws.documents()[index].path.is_none()
            && let Some(doc) = ws.remove(index)
        {
            docs.push(doc);
        }
    }
    docs.reverse();
    docs.truncate(MAX_TABS);
    docs
}

/// Run `job` to its end (or "Dừng"), saying where it stands.
fn work(job: Job, stop: &AtomicBool, tell: &dyn Fn(String)) -> Ended {
    let failed = |why: String| Ended {
        stage: Stage::Failed(why),
        docs: Vec::new(),
    };
    let engine = Engine::new(
        crate::agent::registry_for_agents(),
        Source::Cli,
        Policy::agent(),
    );
    let mut host = LocalHost::new(engine);
    let roots = match Roots::new(
        std::slice::from_ref(&job.read),
        std::slice::from_ref(&job.write),
    ) {
        Ok(r) => r,
        Err(why) => return failed(why),
    };
    let mut gate = Gate::new(Policy::agent(), roots);
    match host.commands() {
        Ok(list) => gate.learn(&list),
        Err(e) => return failed(error_line(&e)),
    }
    // The OCR model card_folder reads numbers with, fetched once: the person
    // asked for this job at the window, as from the File menu.
    #[cfg(feature = "pro")]
    if job.task == crate::ui::QuickTask::IdCards {
        tell("Đang chuẩn bị model đọc chữ…".to_string());
        let fetched = iai_ai::ai::ocr::ensure_model(&|f| {
            tell(format!("Đang tải model đọc chữ… {:.0}%", f * 100.0));
        });
        if let Err(why) = fetched {
            return failed(format!("Không tải được model đọc chữ: {why}"));
        }
    }
    let store = Store::new(iai_automation::skills::default_root());
    let saved = job.recipe.is_none();
    let started = match job.recipe {
        Some(recipe) => Run::start_recipe(&store, &gate, &job.name, recipe, &job.params),
        None => Run::start(&store, &gate, &job.name, &job.params),
    };
    let mut run = match started {
        Ok((run, _)) => run,
        Err(e) => return failed(error_line(&e)),
    };
    let stage = loop {
        if stop.load(Ordering::Relaxed) {
            if let Some(j) = run.job() {
                let _ = host.cancel(j);
            }
            break Stage::Stopped;
        }
        match run.advance(&mut host, &gate, &store, Instant::now().checked_add(SLICE)) {
            Ok(Progress::Done) => {
                if saved {
                    let _ = store.record_use(&job.name, true, false, "", HOW);
                }
                break Stage::Done(done_line(&job.task, &run));
            }
            Ok(Progress::Paused) => tell(stage_line(&run)),
            Err(f) => {
                if saved {
                    let broken = matches!(f.error.code, ErrorCode::BadParams | ErrorCode::NotFound)
                        && !f.step.is_empty();
                    let _ = store.record_use(&job.name, false, broken, "", HOW);
                }
                let at = f
                    .round
                    .map(|r| format!("Dừng ở ảnh thứ {r}: "))
                    .unwrap_or_default();
                break Stage::Failed(format!("{at}{}", error_line(&f.error)));
            }
        }
    };
    Ended {
        docs: made_documents(&mut host.ws),
        stage,
    }
}

impl App {
    /// "Việc nhanh" as its window shows it.
    pub(crate) fn quick_view(&self) -> QuickView {
        let q = &self.shell.quick;
        QuickView {
            open: q.open,
            folder: self
                .shell
                .errands
                .folder
                .as_ref()
                .map(|f| f.display().to_string())
                .unwrap_or_default(),
            pro: self.pro_open(),
            skills: q
                .skills
                .as_ref()
                .map(|(_, s)| s.clone())
                .unwrap_or_default(),
            run: q.under.as_ref().map(|u| {
                let (stage, running, failed) = match &u.stage {
                    Stage::Working(s) => (s.clone(), true, false),
                    Stage::Done(s) => (s.clone(), false, false),
                    Stage::Failed(s) => (s.clone(), false, true),
                    Stage::Stopped => ("Đã dừng.".to_string(), false, false),
                };
                QuickRunView {
                    title: u.title.clone(),
                    stage,
                    running,
                    failed,
                    results: u.results.is_dir().then(|| u.results.display().to_string()),
                }
            }),
            preset: q.preset.clone(),
        }
    }

    pub(crate) fn quick_intent(&mut self, intent: QuickIntent) {
        match intent {
            QuickIntent::Open => {
                self.shell.quick.open = true;
                self.shell.quick.skills = None;
            }
            QuickIntent::OpenFor(task) => {
                let q = &mut self.shell.quick;
                q.open = true;
                q.skills = None;
                let n = q.preset.as_ref().map_or(1, |(n, _)| n.wrapping_add(1));
                q.preset = Some((n, task));
            }
            QuickIntent::Close => self.shell.quick.open = false,
            QuickIntent::PickFolder => self.errand_intent(crate::ui::ErrandIntent::PickFolder),
            QuickIntent::Start(form) => self.start_quick_task(&form),
            QuickIntent::Stop => {
                if let Some(u) = &self.shell.quick.under {
                    u.stop.store(true, Ordering::Relaxed);
                }
            }
            QuickIntent::OpenResults => {
                if let Some(u) = &self.shell.quick.under {
                    #[cfg(windows)]
                    let _ = std::process::Command::new("explorer.exe")
                        .arg(&u.results)
                        .spawn();
                    let _ = &u.results;
                }
            }
        }
    }

    /// The job `form` asks for, on the folder chosen.
    fn quick_job(&self, form: &QuickForm, folder: &Path, results: &Path) -> Result<Job, String> {
        let photos = photos_in(folder);
        let read = folder.to_path_buf();
        let write = results.to_path_buf();
        match &form.task {
            QuickTask::ScanCleanup => {
                if photos.is_empty() {
                    return Err("Thư mục không có ảnh (JPG, PNG, TIFF…).".to_string());
                }
                let look = if form.look == "bilevel" {
                    "bilevel"
                } else {
                    "grayscale"
                };
                Ok(Job {
                    task: form.task.clone(),
                    name: "lam-sach-giay-to".to_string(),
                    recipe: Some(scan_recipe()),
                    params: json!({
                        "vao": folder.display().to_string(),
                        "ra": results.display().to_string(),
                        "look": look,
                    }),
                    read,
                    write,
                })
            }
            QuickTask::IdPhotos => {
                if !self.pro_open() {
                    return Err("Ảnh thẻ cần Gói Pro (Help ▸ Gói Pro).".to_string());
                }
                if photos.is_empty() {
                    return Err("Thư mục không có ảnh chân dung (JPG, PNG…).".to_string());
                }
                if photos.len() > MAX_PORTRAITS {
                    return Err(format!(
                        "Thư mục có {} ảnh; mỗi lần làm tối đa {MAX_PORTRAITS} người. Chia ra \
                         vài thư mục rồi làm từng phần.",
                        photos.len()
                    ));
                }
                let items: Vec<Value> = photos
                    .iter()
                    .map(|p| json!({"path": p.display().to_string(), "copies": form.copies.clamp(1, 20)}))
                    .collect();
                Ok(Job {
                    task: form.task.clone(),
                    name: "anh-the-ca-thu-muc".to_string(),
                    recipe: Some(id_photo_recipe()),
                    params: json!({
                        "anh": items,
                        "size": form.size,
                        "backdrop": form.backdrop,
                        "paper": form.paper,
                    }),
                    // Nothing is written: the sheets are a tab.
                    write: read.clone(),
                    read,
                })
            }
            QuickTask::IdCards => {
                if !self.pro_open() {
                    return Err("Xếp căn cước cần Gói Pro (Help ▸ Gói Pro).".to_string());
                }
                if photos.is_empty() {
                    return Err("Thư mục không có ảnh (JPG, PNG…).".to_string());
                }
                Ok(Job {
                    task: form.task.clone(),
                    name: "xep-can-cuoc-thu-muc".to_string(),
                    recipe: Some(id_card_recipe()),
                    params: json!({"vao": folder.display().to_string(), "ban": 1}),
                    // Nothing is written: the pages are a tab.
                    write: read.clone(),
                    read,
                })
            }
            QuickTask::Skill(name) => {
                let store = Store::new(iai_automation::skills::default_root());
                let recipe = store.load(name).map_err(|e| error_line(&e))?;
                Ok(Job {
                    task: form.task.clone(),
                    name: name.clone(),
                    recipe: None,
                    params: skill_params(&recipe, &form.fields, folder, results),
                    read,
                    write,
                })
            }
        }
    }

    fn start_quick_task(&mut self, form: &QuickForm) {
        if self.shell.quick.running() {
            return;
        }
        let Some(folder) = self.shell.errands.folder.clone() else {
            return;
        };
        let results = folder.join(RESULTS);
        let title = {
            let what = match &form.task {
                QuickTask::ScanCleanup => "Làm sạch ảnh chụp giấy tờ".to_string(),
                QuickTask::IdPhotos => "Ảnh thẻ cho nhiều người".to_string(),
                QuickTask::IdCards => "Xếp căn cước lên A4".to_string(),
                QuickTask::Skill(name) => self
                    .shell
                    .quick
                    .skills
                    .as_ref()
                    .and_then(|(_, s)| s.iter().find(|k| &k.name == name))
                    .map_or_else(|| name.clone(), |k| k.title.clone()),
            };
            let place = folder
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            format!("{what} · {place}")
        };
        let stop = Arc::new(AtomicBool::new(false));
        // Only the jobs that write files have a results' folder.
        let writes = matches!(form.task, QuickTask::ScanCleanup | QuickTask::Skill(_));
        let results = if writes { results } else { PathBuf::new() };
        let job = (if writes {
            std::fs::create_dir_all(&results)
        } else {
            Ok(())
        })
        .map_err(|e| format!("Không tạo được thư mục {}: {e}", results.display()))
        .and_then(|()| self.quick_job(form, &folder, &results));
        let (stage, rx) = match job {
            Err(why) => (Stage::Failed(why), None),
            Ok(job) => {
                crate::diag::note("quick", &format!("{} on {}", job.name, folder.display()));
                let (tx, rx) = std::sync::mpsc::channel();
                let flag = Arc::clone(&stop);
                let spawned = std::thread::Builder::new()
                    .name("viec-nhanh".to_string())
                    .spawn(move || {
                        let progress = tx.clone();
                        let tell = move |line: String| {
                            let _ = progress.send(Msg::Stage(line));
                        };
                        let ended = work(job, &flag, &tell);
                        let _ = tx.send(Msg::Ended(ended));
                    });
                match spawned {
                    Ok(_) => (Stage::Working("Đang bắt đầu…".to_string()), Some(rx)),
                    Err(e) => (Stage::Failed(format!("Không chạy được: {e}")), None),
                }
            }
        };
        self.shell.quick.under = Some(Under {
            title,
            results,
            stage,
            stop,
            rx,
        });
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Every frame: the library read again while the window is open, and
    /// what the job under way says.
    pub(crate) fn poll_quick_tasks(&mut self) {
        let q = &mut self.shell.quick;
        if q.open
            && q.skills
                .as_ref()
                .is_none_or(|(at, _)| at.elapsed() > SKILLS_FRESH)
        {
            let store = Store::new(iai_automation::skills::default_root());
            q.skills = Some((Instant::now(), read_skills(&store)));
        }
        if !q.made.is_empty() && !self.person_mid_edit() {
            let made = std::mem::take(&mut self.shell.quick.made);
            self.adopt_documents(made);
        }
        let q = &mut self.shell.quick;
        let Some(under) = q.under.as_mut() else {
            return;
        };
        let Some(rx) = under.rx.as_ref() else {
            return;
        };
        let mut ended = None;
        loop {
            match rx.try_recv() {
                Ok(Msg::Stage(line)) => under.stage = Stage::Working(line),
                Ok(Msg::Ended(e)) => {
                    ended = Some(e);
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    ended = Some(Ended {
                        stage: Stage::Failed("Việc dừng giữa chừng.".to_string()),
                        docs: Vec::new(),
                    });
                    break;
                }
            }
        }
        let Some(ended) = ended else {
            return;
        };
        under.rx = None;
        under.stage = ended.stage;
        let line = match &under.stage {
            Stage::Done(s) | Stage::Failed(s) => s.clone(),
            Stage::Stopped => "Việc nhanh: đã dừng".to_string(),
            Stage::Working(_) => String::new(),
        };
        crate::diag::note("quick", &line);
        q.made.extend(ended.docs);
        if !self.person_mid_edit() {
            let made = std::mem::take(&mut self.shell.quick.made);
            self.adopt_documents(made);
        }
        self.shell.status_msg = line;
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_recipes_iai_carries_read_and_run_without_an_agent() {
        for recipe in [scan_recipe(), id_photo_recipe(), id_card_recipe()] {
            assert!(!recipe.steps.is_empty(), "{}", recipe.meta.title);
        }
        assert!(scan_recipe().runs_on_a_folder());
        assert!(id_card_recipe().runs_on_a_folder());
    }

    #[test]
    fn a_saved_skill_takes_the_folder_chosen_and_the_values_filled_in() {
        let recipe: Recipe = serde_json::from_value(json!({
            "version": 1,
            "meta": {"title": "Ảnh thẻ hàng loạt", "tier": 1},
            "params": {
                "vao": {"type": "folder", "default": r"C:\khach_cu", "label": "Thư mục ảnh"},
                "ra": {"type": "folder", "default": r"C:\khach_cu\iAi_ket_qua", "write": true},
                "copies": {"type": "number", "default": 2, "label": "Số tấm"},
                "backdrop": {"type": "text", "default": "blue"}
            },
            "steps": [{"id": "s", "command": "doc.list"}]
        }))
        .unwrap();
        let dir = std::env::temp_dir().join(format!("iai-quick-skills-{}", std::process::id()));
        let store = Store::new(&dir);
        store.write("anh-the-hang-loat", &recipe).unwrap();
        let skills = read_skills(&store);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(skills.len(), 1);
        let fields = &skills[0].fields;
        assert_eq!(fields.len(), 2);
        assert!(
            fields
                .iter()
                .any(|f| f.label == "Số tấm" && f.value == QuickValue::Number(2.0))
        );
        let filled: Vec<QuickField> = fields
            .iter()
            .map(|f| match f.name.as_str() {
                "copies" => QuickField {
                    value: QuickValue::Number(5.0),
                    ..f.clone()
                },
                _ => f.clone(),
            })
            .collect();
        let params = skill_params(
            &recipe,
            &filled,
            Path::new(r"D:\khach"),
            Path::new(r"D:\khach\iAi_ket_qua"),
        );
        assert_eq!(
            params,
            json!({
                "vao": r"D:\khach", "ra": r"D:\khach\iAi_ket_qua",
                "copies": 5, "backdrop": "blue"
            })
        );
    }

    /// Two photos of papers on a folder: both made clean in the results'
    /// folder, every tab closed, nothing for the window.
    #[test]
    fn papers_of_a_folder_are_made_clean_on_a_thread_of_their_own() {
        let dir = std::env::temp_dir().join(format!("iai-quick-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let results = dir.join(RESULTS);
        std::fs::create_dir_all(&results).unwrap();
        for (name, shade) in [("trang1.png", 200u8), ("trang2.jpg", 180)] {
            let mut page = image::RgbImage::from_pixel(120, 160, image::Rgb([shade, shade, shade]));
            for x in 20..100 {
                page.put_pixel(x, 40, image::Rgb([20, 20, 20]));
            }
            page.save(dir.join(name)).unwrap();
        }
        std::fs::write(dir.join("ghi_chu.txt"), b"x").unwrap();
        let job = Job {
            task: QuickTask::ScanCleanup,
            name: "lam-sach-giay-to".to_string(),
            recipe: Some(scan_recipe()),
            params: json!({
                "vao": dir.display().to_string(),
                "ra": results.display().to_string(),
                "look": "grayscale",
            }),
            read: dir.clone(),
            write: results.clone(),
        };
        let said = std::sync::Mutex::new(Vec::new());
        let ended = work(job, &AtomicBool::new(false), &|line| {
            said.lock().unwrap().push(line)
        });
        let written = (
            results.join("trang1.jpg").is_file(),
            results.join("trang2.jpg").is_file(),
        );
        let paper = image::open(results.join("trang1.jpg")).map(|i| i.to_luma8());
        let _ = std::fs::remove_dir_all(&dir);
        match &ended.stage {
            Stage::Done(line) => assert!(line.contains("2 ảnh"), "{line}"),
            Stage::Failed(why) => panic!("{why}"),
            _ => panic!("not done"),
        }
        assert_eq!(written, (true, true));
        // The grey paper came out white.
        assert!(paper.unwrap().get_pixel(5, 5)[0] > 240);
        assert!(ended.docs.is_empty());
    }

    /// Opt-in, with the AI models: IAI_QUICK_PROBE is a folder of portraits;
    /// they become ID photos on a sheet, as "Việc nhanh" makes them.
    #[cfg(feature = "pro")]
    #[test]
    #[ignore]
    fn probe_id_photos_of_a_folder_with_the_real_models() {
        let Ok(dir) = std::env::var("IAI_QUICK_PROBE") else {
            return;
        };
        let dir = PathBuf::from(dir);
        let results = dir.join(RESULTS);
        std::fs::create_dir_all(&results).unwrap();
        let items: Vec<Value> = photos_in(&dir)
            .iter()
            .map(|p| json!({"path": p.display().to_string(), "copies": 2}))
            .collect();
        let job = Job {
            task: QuickTask::IdPhotos,
            name: "anh-the-ca-thu-muc".to_string(),
            recipe: Some(id_photo_recipe()),
            params: json!({
                "anh": items, "size": "3x4", "backdrop": "blue", "paper": "10x15",
            }),
            read: dir.clone(),
            write: dir.clone(),
        };
        let started = Instant::now();
        let ended = work(job, &AtomicBool::new(false), &|line| eprintln!("{line}"));
        match &ended.stage {
            Stage::Done(line) => eprintln!("{line} ({:.1} s)", started.elapsed().as_secs_f32()),
            Stage::Failed(why) => panic!("{why}"),
            _ => panic!("not done"),
        }
        assert!(!results.join("anh_the.pdf").exists(), "no PDF any more");
        assert_eq!(ended.docs.len(), 1);
        assert!(ended.docs[0].path.is_none());
    }

    /// What a job made comes to the window as a tab of its own, shown, with
    /// an id of the window's; the end is said on the status line.
    #[test]
    fn a_sheet_a_job_made_comes_back_as_a_tab_shown() {
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        let before = app.docs.documents.len();
        let (tx, rx) = std::sync::mpsc::channel();
        app.shell.quick.under = Some(Under {
            title: "Ảnh thẻ cho nhiều người · khach".to_string(),
            results: std::env::temp_dir(),
            stage: Stage::Working("Đang làm…".to_string()),
            stop: Arc::new(AtomicBool::new(false)),
            rx: Some(rx),
        });
        assert!(app.shell.quick.running());
        let clash = app.docs.active().id;
        let sheet = Document::from_canvas(clash, crate::core::canvas::Canvas::new(30, 20), None);
        tx.send(Msg::Stage("Đang làm ảnh thẻ 2/3…".to_string()))
            .unwrap();
        app.poll_quick_tasks();
        assert_eq!(app.quick_view().run.unwrap().stage, "Đang làm ảnh thẻ 2/3…");
        tx.send(Msg::Ended(Ended {
            stage: Stage::Done("Xong: 6 tấm".to_string()),
            docs: vec![sheet],
        }))
        .unwrap();
        app.poll_quick_tasks();
        assert!(!app.shell.quick.running());
        assert_eq!(app.docs.documents.len(), before + 1);
        let shown = app.docs.active();
        assert_eq!((shown.canvas.width, shown.canvas.height), (30, 20));
        assert_ne!(shown.id, clash);
        assert_eq!(app.shell.status_msg, "Xong: 6 tấm");
    }

    #[test]
    fn a_folder_without_photos_is_refused_before_it_runs() {
        let dir = std::env::temp_dir().join(format!("iai-quick-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut app = App::new();
        app.shell.errands.folder = Some(dir.clone());
        app.quick_intent(QuickIntent::Start(QuickForm::default()));
        let view = app.quick_view();
        let run = view.run.unwrap();
        assert!(
            run.failed && run.stage.contains("không có ảnh"),
            "{}",
            run.stage
        );
        assert!(!app.shell.quick.running());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// "Xếp căn cước lên A4" run as the window runs it, on the photos of
    /// `IAI_CARD_FOLDER` (needs the OCR model, or the network to fetch it).
    #[cfg(feature = "pro")]
    #[test]
    #[ignore = "probe: needs IAI_CARD_FOLDER"]
    fn probe_quick_id_cards() {
        let Some(dir) = std::env::var_os("IAI_CARD_FOLDER") else {
            return;
        };
        let folder = PathBuf::from(dir);
        let _ = std::fs::create_dir_all(folder.join(RESULTS));
        let job = Job {
            task: QuickTask::IdCards,
            name: "xep-can-cuoc-thu-muc".to_string(),
            recipe: Some(id_card_recipe()),
            params: json!({"vao": folder.display().to_string(), "ban": 1}),
            read: folder.clone(),
            write: folder.join(RESULTS),
        };
        let ended = work(job, &AtomicBool::new(false), &|line| println!("{line}"));
        match ended.stage {
            Stage::Done(line) | Stage::Failed(line) => println!("{line}"),
            _ => println!("stopped"),
        }
        println!("{} tab(s)", ended.docs.len());
    }
}

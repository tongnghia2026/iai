//! The author's list of models, `docs/author/models.json`: for each model,
//! the sources it is fetched from in order (its own author's, then the
//! author of iAi's GitHub Releases, then his Google Drive), where to read of
//! it, the video that shows how to get it. It also lets the author add a
//! file the app should know by its hash (a file a video points to, a newer
//! export) without a new build.
//!
//! The app reads it from the public repository, as `author_news.rs` reads
//! `news.json`, keeps the last copy for a start without a network, and has
//! the copy of this repository built in for when it has neither. What it
//! reads only adds to the built-in table: it cannot change a known model's
//! hash, file or licence.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::catalog::{self, Slot};
use super::recognize::{Accepted, Known};

/// The list as this repository holds it, built into the app.
pub const BUILT_IN: &str = include_str!("../../../../docs/author/models.json");
/// The schema this build reads; a list of a later one is not taken.
pub const SCHEMA: u32 = 1;
/// Where the list is read, the first that answers: the branch the app is
/// developed on, then the repository's own. `IAI_MODELS_FEED` names another
/// place, an address or a file on this machine, to rehearse.
const FEEDS: [&str; 2] = [
    "https://raw.githubusercontent.com/tongnghia2026/iai/feat/vector-core-foundation/docs/author/models.json",
    "https://raw.githubusercontent.com/tongnghia2026/iai/main/docs/author/models.json",
];
/// No more than this is taken of the file, and of its list.
pub const MAX_LIST_BYTES: u64 = 512 * 1024;
const MAX_MODELS: usize = 500;
/// The list is read again at most this often.
const REFRESH_EVERY: Duration = Duration::from_secs(10 * 60);
/// What a place not yet known is marked with in the file.
pub const TODO: &str = "TODO chủ điền";

/// The file, as the author writes it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelList {
    pub schema: u32,
    /// A note for whoever edits the file; the app does not read it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub about: String,
    pub models: Vec<Entry>,
    pub packs: Vec<PackEntry>,
}

/// One model of the list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    /// The table's name for it, or a new one for a file the list adds.
    pub id: String,
    /// For a file the list adds: the id of the table's model whose place in
    /// the code it takes.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub slot: String,
    pub name: String,
    pub feature: String,
    pub dir: String,
    pub file: String,
    pub size: u64,
    pub approx_mb: u32,
    pub sha256: String,
    pub license: String,
    pub redistributable: bool,
    pub pack: String,
    pub sources: Vec<Source>,
    /// A page to open by hand (for a model the app does not fetch).
    pub page: String,
    pub video: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub companions: Vec<CompanionEntry>,
}

/// A place a file is fetched from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Source {
    /// `author`, `github` or `drive`: for people, and the report of a failure.
    pub from: String,
    pub url: String,
}

impl Source {
    /// A place the app can fetch from: an `https://` address, not a
    /// placeholder.
    pub fn usable(&self) -> bool {
        is_https(&self.url)
    }
}

/// A file a model of the list cannot run without (its weights beside it).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompanionEntry {
    pub file: String,
    pub size: u64,
    pub approx_mb: u32,
    pub sha256: String,
    pub sources: Vec<Source>,
}

/// One of the two model packs of link 2.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PackEntry {
    pub id: String,
    pub name: String,
    pub file: String,
    pub size: u64,
    pub sha256: String,
    pub sources: Vec<Source>,
}

/// An address the app will open or fetch from.
pub fn is_https(url: &str) -> bool {
    let url = url.trim();
    url.len() > "https://".len()
        && url.starts_with("https://")
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

impl Entry {
    /// The slot it fills: its own id's, or the one it names.
    pub fn slot(&self) -> Option<Slot> {
        Slot::from_id(&self.id).or_else(|| Slot::from_id(&self.slot))
    }

    /// Whether it is one of the table's models.
    pub fn built_in(&self) -> bool {
        Slot::from_id(&self.id).is_some()
    }

    /// The sources this build fetches from. The author's own Drive is for
    /// the Pro pack's customers only (03/11): a free build fetches from the
    /// model's makers, never from it.
    pub fn usable_sources(&self) -> impl Iterator<Item = &Source> {
        self.sources
            .iter()
            .filter(|s| s.usable() && (cfg!(feature = "pro") || s.from != "drive"))
    }

    /// Whether the app fetches it, and if not, why not (for people).
    pub fn downloadable(&self) -> Result<(), String> {
        if !self.redistributable {
            return Err(
                "Giấy phép của model này chưa cho phát lại: app không tự tải, chỉ chỉ chỗ tải"
                    .to_string(),
            );
        }
        if !is_sha256(&self.sha256) {
            return Err("Chưa có mã kiểm tra của file: app chưa tải được".to_string());
        }
        if self.usable_sources().next().is_none() {
            return Err("Chưa có link tải — xem hướng dẫn hoặc liên hệ tác giả".to_string());
        }
        for c in &self.companions {
            if !is_sha256(&c.sha256) || !c.sources.iter().any(Source::usable) {
                return Err(format!("Chưa có link tải hoặc mã kiểm tra của {}", c.file));
            }
        }
        Ok(())
    }

    /// Bytes of the model and what comes with it, measured or approximate.
    pub fn total_bytes(&self) -> u64 {
        let one = |size: u64, approx: u32| {
            if size > 0 {
                size
            } else {
                u64::from(approx) * catalog::MB
            }
        };
        self.companions
            .iter()
            .fold(one(self.size, self.approx_mb), |sum, c| {
                sum + one(c.size, c.approx_mb)
            })
    }

    fn checked(mut self) -> Self {
        let keep_https = |text: &mut String| {
            if !is_https(text) {
                text.clear();
            }
        };
        keep_https(&mut self.page);
        keep_https(&mut self.video);
        self.sha256 = self.sha256.trim().to_ascii_lowercase();
        for c in &mut self.companions {
            c.sha256 = c.sha256.trim().to_ascii_lowercase();
        }
        self
    }
}

impl ModelList {
    /// Read a list; `None` when it is not one this build takes.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() as u64 > MAX_LIST_BYTES {
            return None;
        }
        let mut list: ModelList = serde_json::from_slice(bytes).ok()?;
        if list.schema != SCHEMA {
            return None;
        }
        list.models.truncate(MAX_MODELS);
        list.models = list.models.into_iter().map(Entry::checked).collect();
        Some(list)
    }

    /// The list built into the app.
    pub fn built_in() -> Self {
        Self::parse(BUILT_IN.as_bytes()).unwrap_or_default()
    }

    /// This list with what `newer` (the author's, read from the network)
    /// says: where the table's models are fetched from now, their pages and
    /// videos, a size not yet measured; and the files it adds for a slot,
    /// each with its hash. Nothing else of a known model changes.
    pub fn with(&self, newer: &ModelList) -> Self {
        let mut merged = self.clone();
        for entry in &newer.models {
            if let Some(mine) = merged.models.iter_mut().find(|m| m.id == entry.id) {
                if !entry.sources.is_empty() {
                    mine.sources = entry.sources.clone();
                }
                if !entry.page.is_empty() {
                    mine.page = entry.page.clone();
                }
                if !entry.video.is_empty() {
                    mine.video = entry.video.clone();
                }
                if mine.size == 0 && entry.sha256 == mine.sha256 {
                    mine.size = entry.size;
                }
                if entry.approx_mb > 0 {
                    mine.approx_mb = entry.approx_mb;
                }
                for c in &mut mine.companions {
                    if let Some(theirs) = entry.companions.iter().find(|t| t.file == c.file)
                        && !theirs.sources.is_empty()
                    {
                        c.sources = theirs.sources.clone();
                    }
                }
            } else if !entry.built_in() && entry.slot().is_some() && is_sha256(&entry.sha256) {
                merged.models.push(entry.clone());
            }
        }
        for pack in &newer.packs {
            match merged.packs.iter_mut().find(|p| p.id == pack.id) {
                Some(mine) => *mine = pack.clone(),
                None => merged.packs.push(pack.clone()),
            }
        }
        merged
    }

    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.models.iter().find(|m| m.id == id)
    }

    /// The entry of `slot`'s model in the table.
    pub fn entry_of(&self, slot: Slot) -> Option<&Entry> {
        self.entry(slot.model().id)
    }

    /// The files known by hash: the table's, then those the list adds.
    pub fn known(&self) -> Known {
        let mut known = Known::table();
        for entry in self.models.iter().filter(|e| !e.built_in()) {
            let Some(slot) = entry.slot() else {
                continue;
            };
            known.accepted.push(Accepted {
                id: entry.id.clone(),
                name: entry.name.clone(),
                slot,
                file: entry.file.clone(),
                size: entry.size,
                sha256: entry.sha256.clone(),
                companions: entry
                    .companions
                    .iter()
                    .map(|c| (c.file.clone(), c.size, c.sha256.clone()))
                    .collect(),
            });
        }
        known
    }
}

struct Current {
    list: Arc<ModelList>,
    refreshed_at: Option<Instant>,
}

fn current_cell() -> &'static Mutex<Current> {
    static CURRENT: OnceLock<Mutex<Current>> = OnceLock::new();
    CURRENT.get_or_init(|| {
        let built_in = ModelList::built_in();
        let list = match kept_file().and_then(|f| std::fs::read(f).ok()) {
            Some(bytes) => match ModelList::parse(&bytes) {
                Some(kept) => built_in.with(&kept),
                None => built_in,
            },
            None => built_in,
        };
        Mutex::new(Current {
            list: Arc::new(list),
            refreshed_at: None,
        })
    })
}

/// The list as the app knows it now: built in, with the author's last copy.
pub fn current() -> Arc<ModelList> {
    Arc::clone(
        &current_cell()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .list,
    )
}

/// Where the last copy read is kept: `%APPDATA%\IAI\author\models.json`,
/// beside the news. A rehearsal keeps none.
fn kept_file() -> Option<PathBuf> {
    if rehearsed().is_some() || cfg!(test) {
        return None;
    }
    Some(
        iai_platform::app_data::dir()
            .join("author")
            .join("models.json"),
    )
}

fn rehearsed() -> Option<String> {
    std::env::var("IAI_MODELS_FEED")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Read the author's list again, unless it was read in the last minutes:
/// from the network through `transport`, the first feed that answers with a
/// list this build takes. On success it is kept for a start without a
/// network. A failure leaves the list as it was.
pub fn refresh(transport: &dyn super::fetch::Transport) -> Result<(), String> {
    {
        let current = current_cell().lock().unwrap_or_else(|e| e.into_inner());
        if current
            .refreshed_at
            .is_some_and(|at| at.elapsed() < REFRESH_EVERY)
        {
            return Ok(());
        }
    }
    let feeds: Vec<String> = match rehearsed() {
        Some(place) => vec![place],
        None => FEEDS.iter().map(|f| f.to_string()).collect(),
    };
    let mut last_error = String::from("no feed");
    for feed in feeds {
        let bytes = if is_https(&feed) {
            super::fetch::get_small(transport, &feed, MAX_LIST_BYTES)
        } else {
            std::fs::read(&feed).map_err(|e| format!("{feed}: {e}"))
        };
        match bytes {
            Ok(bytes) => match ModelList::parse(&bytes) {
                Some(newer) => {
                    if let Some(file) = kept_file() {
                        if let Some(dir) = file.parent() {
                            let _ = std::fs::create_dir_all(dir);
                        }
                        let _ = iai_platform::atomic_file::write_bytes_atomic(&file, &bytes);
                    }
                    let mut current = current_cell().lock().unwrap_or_else(|e| e.into_inner());
                    current.list = Arc::new(ModelList::built_in().with(&newer));
                    current.refreshed_at = Some(Instant::now());
                    return Ok(());
                }
                None => last_error = format!("{feed}: not a list this build takes"),
            },
            Err(e) => last_error = e,
        }
    }
    Err(last_error)
}

/// [`refresh`] on a thread of its own, over the network.
pub fn refresh_in_background() {
    if cfg!(test) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("model-list".into())
        .spawn(|| {
            if let Ok(http) = super::fetch::Http::quick() {
                let _ = refresh(&http);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_of_this_repository_is_built_in_and_matches_the_table() {
        let list = ModelList::parse(BUILT_IN.as_bytes()).expect("docs/author/models.json");
        for model in catalog::MODELS {
            let entry = list
                .entry(model.id)
                .unwrap_or_else(|| panic!("{} not listed", model.id));
            assert_eq!(entry.file, model.file, "{}", model.id);
            assert_eq!(entry.dir, model.dir, "{}", model.id);
            assert_eq!(entry.sha256, model.sha256, "{}", model.id);
            assert_eq!(entry.size, model.size, "{}", model.id);
            assert_eq!(entry.feature, model.feature.id(), "{}", model.id);
            assert_eq!(entry.license, model.license, "{}", model.id);
            assert_eq!(entry.redistributable, model.redistributable, "{}", model.id);
            assert_eq!(entry.pack, model.pack.id(), "{}", model.id);
            let companions: Vec<(&str, &str)> = entry
                .companions
                .iter()
                .map(|c| (c.file.as_str(), c.sha256.as_str()))
                .collect();
            let table: Vec<(&str, &str)> = model
                .companions
                .iter()
                .map(|c| (c.file, c.sha256))
                .collect();
            assert_eq!(companions, table, "{}", model.id);
            // Sources in order: the model's author, then the Drive of the
            // author of iAi (03/11: the one place he hands models out); none
            // for a model that may not be handed out.
            let from: Vec<&str> = entry.sources.iter().map(|s| s.from.as_str()).collect();
            if model.redistributable {
                assert!(from.ends_with(&["drive"]), "{}: {from:?}", model.id);
            } else {
                assert!(from.is_empty(), "{}: {from:?}", model.id);
            }
            for source in &entry.sources {
                assert!(
                    source.usable() || source.url == TODO,
                    "{}: {}",
                    model.id,
                    source.url
                );
            }
        }
        assert!(list.models.iter().all(|e| e.slot().is_some()));
        // GFPGAN is pointed to, never fetched.
        let gfpgan = list.entry_of(Slot::Gfpgan).unwrap();
        assert!(gfpgan.downloadable().is_err());
        // What the app fetched before has its author's source still.
        for slot in [Slot::Lama, Slot::BiRefNetTiny, Slot::Ocr] {
            let entry = list.entry_of(slot).unwrap();
            assert!(entry.downloadable().is_ok(), "{slot:?}");
        }
        assert_eq!(
            list.entry_of(Slot::Lama).unwrap().sources[0].url,
            "https://huggingface.co/Carve/LaMa-ONNX/resolve/main/lama_fp32.onnx"
        );
    }

    #[test]
    fn what_the_author_reads_out_adds_sources_and_files_but_changes_no_known_hash() {
        let built_in = ModelList::built_in();
        let newer = ModelList::parse(
            br#"{
              "schema": 1,
              "models": [
                {"id": "lama", "sha256": "0000000000000000000000000000000000000000000000000000000000000000",
                 "file": "evil.onnx", "redistributable": false,
                 "sources": [{"from": "github", "url": "https://github.com/tongnghia2026/iai/releases/download/models-v1/lama_fp32.onnx"}],
                 "video": "https://youtu.be/abc"},
                {"id": "restoreformer-pp", "slot": "gfpgan-1.4", "name": "RestoreFormer++",
                 "file": "RestoreFormer_PP.onnx", "dir": "gfpgan",
                 "sha256": "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"},
                {"id": "no-slot", "sha256": "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"},
                {"id": "no-hash", "slot": "gfpgan-1.4", "sha256": "TODO"}
              ]
            }"#,
        )
        .unwrap();
        let merged = built_in.with(&newer);
        let lama = merged.entry_of(Slot::Lama).unwrap();
        assert_eq!(lama.sha256, Slot::Lama.model().sha256);
        assert_eq!(lama.file, "lama_fp32.onnx");
        assert!(lama.redistributable);
        assert_eq!(lama.sources.len(), 1);
        assert_eq!(lama.video, "https://youtu.be/abc");
        assert!(merged.entry("restoreformer-pp").is_some());
        assert!(merged.entry("no-slot").is_none());
        assert!(merged.entry("no-hash").is_none());
        let known = merged.known();
        let added = known
            .accepted
            .iter()
            .find(|a| a.id == "restoreformer-pp")
            .unwrap();
        assert_eq!(added.slot, Slot::Gfpgan);
    }

    #[test]
    fn a_list_of_another_schema_or_too_large_is_not_taken() {
        assert!(ModelList::parse(br#"{"schema": 2, "models": []}"#).is_none());
        assert!(ModelList::parse(b"not json").is_none());
        let large = vec![b' '; MAX_LIST_BYTES as usize + 1];
        assert!(ModelList::parse(&large).is_none());
        // A page that is not https is dropped.
        let list = ModelList::parse(
            br#"{"schema": 1, "models": [{"id": "x", "page": "http://plain", "video": "javascript:alert(1)"}]}"#,
        )
        .unwrap();
        assert_eq!(list.models[0].page, "");
        assert_eq!(list.models[0].video, "");
    }
}

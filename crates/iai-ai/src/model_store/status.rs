//! What the "Model AI" window shows (Help menu and the AI window; the app
//! draws it): one row a feature, the models it can run under it, each with
//! its size and state ("✓ đã kiểm" / "✓ tự đặt" / "✗ thiếu"), whether the app
//! fetches it or only shows where to get it, and the files in the model
//! folders that cannot be used, with why.

use std::path::{Path, PathBuf};

use super::catalog::{Family, Feature, Slot};
use super::fetch;
use super::list::{Entry, ModelList};
use super::recognize::Recognition;
use super::scan::Inventory;

/// A model's state on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Checked by its hash: "✓ đã kiểm".
    Verified,
    /// The user's own model of the family: "✓ tự đặt".
    Custom,
    /// Not on this machine: "✗ thiếu".
    Missing,
    /// There, but not usable, and why: "✗ …".
    Unusable(String),
}

impl State {
    pub fn usable(&self) -> bool {
        matches!(self, State::Verified | State::Custom)
    }

    /// For people.
    pub fn text(&self) -> String {
        match self {
            State::Verified => "✓ đã kiểm".to_string(),
            State::Custom => "✓ tự đặt".to_string(),
            State::Missing => "✗ thiếu".to_string(),
            State::Unusable(why) => format!("✗ {why}"),
        }
    }
}

/// One model under a feature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelRow {
    /// Its name in the author's list, what "Tải" is asked with
    /// ([`super::fetch::download`] takes the slot, [`download_id`] the id).
    pub id: String,
    pub slot: Slot,
    pub name: String,
    /// "~200 MB", the model and what comes with it.
    pub size_text: String,
    pub bytes: u64,
    pub state: State,
    /// The file used, when there is one.
    pub path: Option<PathBuf>,
    /// The app fetches it ("Tải"); when not, why ("Mở link tải" instead).
    pub download: Result<(), String>,
    /// Where to read of it or get it by hand ("Mở link tải").
    pub page: Option<String>,
    /// The author's video of how to get it ("Xem video").
    pub video: Option<String>,
    pub license: String,
}

/// A file the user put there that no row is about: their own model of a
/// family ("tự đặt"), shown under its feature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomRow {
    pub family: Family,
    pub path: PathBuf,
    pub size_text: String,
}

/// One feature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeatureRow {
    pub feature: Feature,
    pub label: &'static str,
    /// One of its models can run.
    pub ready: bool,
    pub models: Vec<ModelRow>,
    pub custom: Vec<CustomRow>,
}

/// The whole window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overview {
    pub features: Vec<FeatureRow>,
    /// Files that look like models but cannot be used, and why.
    pub problems: Vec<(PathBuf, String)>,
    /// Where "Mở thư mục model" opens, and models fetched or installed go.
    pub main_folder: PathBuf,
    /// Every folder looked in.
    pub roots: Vec<PathBuf>,
    /// The folders are being looked through: the rows may still change.
    pub scanning: bool,
}

/// What a free build says in place of "Tải".
pub const FREE_FETCH: &str = "Bản miễn phí: tự tìm file model trên mạng, tải về rồi chép vào thư mục model (hoặc «Cài model từ file…»). Gói Pro có nút «Tải»";

/// The window's rows from a scan and a list. `pro`: the build has the Pro
/// part (without it, its features are not shown, and nothing is fetched
/// from the window).
pub fn features(inventory: &Inventory, list: &ModelList, pro: bool) -> Vec<FeatureRow> {
    Feature::ALL
        .into_iter()
        .filter(|f| pro || !f.pro())
        .map(|feature| {
            let mut models: Vec<ModelRow> = list
                .models
                .iter()
                .filter(|e| e.slot().is_some_and(|s| s.model().feature == feature))
                .filter_map(|e| row(e, inventory))
                .collect();
            // "Tải" is the Pro pack's (03/11): in a free build the person
            // finds the file, and copies it in or installs it from the file.
            if !pro {
                for m in &mut models {
                    if m.download.is_ok() {
                        m.download = Err(FREE_FETCH.to_string());
                    }
                }
            }
            // Known models first, in the table's order; the list's additions
            // after.
            models.sort_by_key(|m| (!Slot::ALL.iter().any(|s| s.model().id == m.id), m.slot));
            let families: Vec<Family> = {
                let mut f: Vec<Family> = Slot::ALL
                    .iter()
                    .filter(|s| s.model().feature == feature)
                    .map(|s| s.model().family)
                    .collect();
                f.dedup();
                f
            };
            let custom: Vec<CustomRow> = families
                .iter()
                .flat_map(|&family| {
                    inventory.custom_of(family).map(move |found| CustomRow {
                        family,
                        path: found.path.clone(),
                        size_text: fetch::size_text(found.size),
                    })
                })
                .collect();
            let ready = models.iter().any(|m| m.state.usable()) || !custom.is_empty();
            FeatureRow {
                feature,
                label: feature.label(),
                ready,
                models,
                custom,
            }
        })
        .collect()
}

fn row(entry: &Entry, inventory: &Inventory) -> Option<ModelRow> {
    let slot = entry.slot()?;
    let verified = inventory.verified_id(&entry.id).next();
    // A file where the slot keeps it under its name, that is not it.
    let in_place = || {
        inventory.found.iter().find(|f| {
            f.path
                .file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case(entry.file.as_str()))
                && !matches!(f.recognition, Recognition::Verified(_))
        })
    };
    let (state, path) = match verified {
        Some(found) => (State::Verified, Some(found.path.clone())),
        None => match in_place() {
            Some(found) => match &found.recognition {
                Recognition::Custom(family) if super::recognize::fits(*family, slot) => {
                    (State::Custom, Some(found.path.clone()))
                }
                Recognition::Unusable(reason) => {
                    (State::Unusable(reason.text()), Some(found.path.clone()))
                }
                _ => (State::Missing, None),
            },
            None => (State::Missing, None),
        },
    };
    let link = |text: &str| super::list::is_https(text).then(|| text.trim().to_string());
    let bytes = entry.total_bytes();
    Some(ModelRow {
        id: entry.id.clone(),
        slot,
        name: entry.name.clone(),
        size_text: if bytes > 0 {
            format!("~{}", fetch::size_text(bytes))
        } else {
            "chưa rõ cỡ".to_string()
        },
        bytes,
        state,
        path,
        download: entry.downloadable(),
        page: link(&entry.page),
        video: link(&entry.video),
        license: entry.license.clone(),
    })
}

/// The files of a scan that cannot be used, and why.
pub fn problems(inventory: &Inventory) -> Vec<(PathBuf, String)> {
    inventory
        .problems()
        .map(|(path, reason)| (path.to_path_buf(), reason.text()))
        .collect()
}

/// The window as it stands on this machine, from the last scan (an empty one
/// while the first runs: [`Overview::scanning`] says so). Call
/// [`super::rescan`] when the window opens and when the app comes back to the
/// front, and read this again when [`super::generation`] moves.
pub fn overview() -> Overview {
    let inventory = super::inventory();
    if inventory.is_none() {
        super::rescan();
    }
    let inventory = inventory.unwrap_or_default();
    let list = super::list::current();
    Overview {
        features: features(&inventory, &list, cfg!(feature = "pro")),
        problems: problems(&inventory),
        main_folder: super::main_folder(),
        roots: super::roots(),
        scanning: super::is_scanning(),
    }
}

/// Fetch the model of the author's list named `id` on this machine (a row's
/// "Tải"), on the calling thread.
pub fn download_id(
    id: &str,
    progress: &dyn Fn(u64, u64),
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<PathBuf, fetch::FetchError> {
    let http = fetch::Http::new().map_err(fetch::FetchError::Disk)?;
    if let Ok(quick) = fetch::Http::quick() {
        let _ = super::list::refresh(&quick);
    }
    fetch::download_in(
        &http,
        &super::list::current(),
        id,
        &super::main_folder(),
        progress,
        cancel,
    )
}

/// A message for the author when every source failed (A6): the model, its
/// size and hash, for the user to copy and send.
pub fn message_for_author(row: &ModelRow, list: &ModelList) -> String {
    let sha = list.entry(&row.id).map_or("", |e| e.sha256.as_str());
    format!(
        "Chào tác giả iAi, máy em không tải được model «{}» ({}, mã {}). Nhờ anh gửi giúp file. Cảm ơn anh!",
        row.name,
        row.size_text,
        if sha.is_empty() { "chưa có" } else { sha }
    )
}

/// The first model folder holding `path`, for showing where a file is.
pub fn root_of<'a>(path: &Path, roots: &'a [PathBuf]) -> Option<&'a PathBuf> {
    roots.iter().find(|root| path.starts_with(root))
}

#[cfg(test)]
mod tests {
    use super::super::recognize::{Accepted, Reason, tests::temp};
    use super::super::scan::Found;
    use super::*;

    fn found(path: &str, recognition: Recognition) -> Found {
        Found {
            path: PathBuf::from(path),
            size: 1000,
            recognition,
        }
    }

    #[test]
    fn each_feature_lists_its_models_and_their_state() {
        let list = ModelList::built_in();
        let inventory = Inventory {
            roots: vec![PathBuf::from("/m")],
            found: vec![
                found(
                    "/m/lama_fp32.onnx",
                    Recognition::Verified(Accepted::of(Slot::Lama.model())),
                ),
                found(
                    "/m/gfpgan/RestoreFormer.onnx",
                    Recognition::Custom(Family::FaceRestore),
                ),
                found(
                    "/m/birefnet-general-tiny-epoch_232.onnx",
                    Recognition::Unusable(Reason::Damaged("hết file".into())),
                ),
                found(
                    "/m/x.pth",
                    Recognition::Unusable(Reason::NotConverted("pth".into())),
                ),
            ],
            cut_short: false,
        };
        let rows = features(&inventory, &list, true);
        assert_eq!(rows.len(), Feature::ALL.len());
        let by = |f: Feature| rows.iter().find(|r| r.feature == f).unwrap();

        let remove = by(Feature::RemoveObject);
        assert!(remove.ready);
        assert_eq!(remove.models[0].state, State::Verified);
        assert_eq!(remove.models[0].state.text(), "✓ đã kiểm");
        // LaMa as measured on the author's file (208 044 816 bytes).
        assert_eq!(remove.models[0].size_text, "~208 MB");
        assert!(remove.models[0].download.is_ok());

        let subject = by(Feature::SelectSubject);
        assert!(!subject.ready);
        assert!(matches!(&subject.models[0].state, State::Unusable(why) if why.contains("hỏng")));

        let face = by(Feature::FaceRestore);
        assert!(face.ready, "the user's own restorer runs");
        assert_eq!(face.models[0].state, State::Missing);
        assert!(
            face.models[0].download.is_err(),
            "GFPGAN is pointed to, not fetched"
        );
        assert_eq!(face.custom.len(), 1);

        let upscale = by(Feature::Upscale);
        assert_eq!(upscale.models.len(), 3);
        assert!(!upscale.ready);
        assert_eq!(upscale.models[0].state.text(), "✗ thiếu");

        assert_eq!(problems(&inventory).len(), 2);
        // Without the Pro part, only its two free features.
        let free = features(&inventory, &list, false);
        let shown: Vec<Feature> = free.iter().map(|r| r.feature).collect();
        assert_eq!(shown, [Feature::RemoveObject, Feature::SelectSubject]);
    }

    #[test]
    fn a_file_the_authors_list_adds_shows_under_its_feature() {
        let list = ModelList::built_in().with(
            &ModelList::parse(
                br#"{"schema": 1, "models": [{"id": "restoreformer-pp", "slot": "gfpgan-1.4",
                    "name": "RestoreFormer++ (video)", "file": "RestoreFormer_PP.onnx", "dir": "gfpgan",
                    "sha256": "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
                    "page": "https://example.org/rf", "approx_mb": 300}]}"#,
            )
            .unwrap(),
        );
        let added = Accepted {
            id: "restoreformer-pp".into(),
            ..Accepted::of(Slot::Gfpgan.model())
        };
        let inventory = Inventory {
            found: vec![found(
                "/m/gfpgan/RestoreFormer_PP.onnx",
                Recognition::Verified(added),
            )],
            ..Inventory::default()
        };
        let rows = features(&inventory, &list, true);
        let face = rows
            .iter()
            .find(|r| r.feature == Feature::FaceRestore)
            .unwrap();
        assert_eq!(face.models.len(), 2);
        assert_eq!(face.models[0].id, "gfpgan-1.4");
        assert_eq!(face.models[1].name, "RestoreFormer++ (video)");
        assert_eq!(face.models[1].state, State::Verified);
        assert_eq!(
            face.models[1].page.as_deref(),
            Some("https://example.org/rf")
        );
        assert!(face.ready);
        let message = message_for_author(&face.models[0], &list);
        assert!(
            message.contains("GFPGAN") && message.contains("0dac008d"),
            "{message}"
        );
    }

    #[test]
    fn a_scan_of_real_folders_feeds_the_rows() {
        let base = temp("status");
        let root = base.join("models");
        std::fs::create_dir_all(root.join("ocr")).unwrap();
        std::fs::write(
            root.join("ocr").join("ch_PP-OCRv4_rec_infer.onnx"),
            b"not it",
        )
        .unwrap();
        let inventory = super::super::scan::scan(
            std::slice::from_ref(&root),
            &ModelList::built_in().known(),
            &mut super::super::recognize::HashCache::in_memory(),
        );
        let rows = features(&inventory, &ModelList::built_in(), true);
        let text = rows
            .iter()
            .find(|r| r.feature == Feature::ReadText)
            .unwrap();
        assert!(
            matches!(&text.models[0].state, State::Unusable(_)),
            "{:?}",
            text.models[0]
        );
        assert_eq!(
            root_of(&root.join("ocr").join("x"), std::slice::from_ref(&root)),
            Some(&root)
        );
        let _ = std::fs::remove_dir_all(base);
    }
}

//! What a file in a model folder is, in three steps:
//! 1. its SHA-256 is that of a model iAi knows: "đã kiểm";
//! 2. else its inputs and outputs have the shape of a family the code runs:
//!    the user's own model ("tự đặt"), run in that family's slots;
//! 3. else why it cannot be used, said plainly.
//!
//! Hashing reads the whole file (1.6 GB for Sapiens2's weights), so it is
//! done only for a file whose size is that of a known model (or of one whose
//! size is not measured yet and whose name or shape it has), and remembered
//! by path, size and modification time in `%APPDATA%\IAI\model-checks.json`:
//! a file is hashed once, not each time the app opens.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::catalog::{self, Family, KnownModel, NOT_CONVERTED, Slot};
use super::onnx::{self, FLOAT, Header};

/// A file iAi knows by its hash: a model of the table, or one the author's
/// list adds (a file a video points to, a newer export of a model).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Accepted {
    /// Its name in `models.json`.
    pub id: String,
    pub name: String,
    pub slot: Slot,
    pub file: String,
    pub size: u64,
    pub sha256: String,
    /// Files it cannot run without, kept beside it: (name, size, SHA-256),
    /// a size of 0 and an empty hash when not measured.
    pub companions: Vec<(String, u64, String)>,
}

impl Accepted {
    pub fn of(model: &KnownModel) -> Self {
        Self {
            id: model.id.to_string(),
            name: model.name.to_string(),
            slot: model.slot,
            file: model.file.to_string(),
            size: model.size,
            sha256: model.sha256.to_string(),
            companions: model
                .companions
                .iter()
                .map(|c| (c.file.to_string(), c.size, c.sha256.to_string()))
                .collect(),
        }
    }
}

/// The files known by hash: the table's, then the list's.
#[derive(Clone, Debug, Default)]
pub struct Known {
    pub accepted: Vec<Accepted>,
}

impl Known {
    /// The table alone.
    pub fn table() -> Self {
        Self {
            accepted: catalog::MODELS.iter().map(Accepted::of).collect(),
        }
    }

    fn by_hash(&self, sha256: &str) -> Option<&Accepted> {
        self.accepted
            .iter()
            .find(|a| !a.sha256.is_empty() && a.sha256 == sha256)
    }

    /// Whether a file of this size and name, of this family if its shape is
    /// known, could be one of the known files: only then is it hashed.
    fn worth_hashing(&self, size: u64, name: &str, family: Option<Family>) -> bool {
        self.accepted.iter().any(|a| {
            !a.sha256.is_empty()
                && if a.size > 0 {
                    a.size == size
                } else {
                    a.file.eq_ignore_ascii_case(name)
                        || family.is_some_and(|f| f == a.slot.model().family)
                }
        })
    }

    /// Whether a weights file of this size could be a known one's.
    fn companion_worth_hashing(&self, size: u64, name: &str) -> bool {
        self.accepted.iter().any(|a| {
            a.companions.iter().any(|(file, csize, sha)| {
                !sha.is_empty()
                    && if *csize > 0 {
                        *csize == size
                    } else {
                        file.eq_ignore_ascii_case(name)
                    }
            })
        })
    }
}

/// Why a file cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    /// Weights not yet converted to ONNX (`.pth`, `.safetensors`…).
    NotConverted(String),
    /// A packed file the user has to unpack (or install from the window).
    Packed(String),
    /// A download that did not finish.
    Partial,
    /// Not a readable ONNX file.
    Damaged(String),
    /// An ONNX file of no family the app runs.
    UnknownShape(String),
    /// Its family's code needs inputs it does not take in its own way.
    WrongType(String),
    /// A file its weights are in is not beside it.
    MissingCompanion(String),
    /// The file its weights are in is not the one it was made with.
    WrongCompanion(String),
}

impl Reason {
    /// For people, in the "Model AI" window.
    pub fn text(&self) -> String {
        match self {
            Reason::NotConverted(ext) => format!(
                "Bản gốc .{ext} chưa chuyển sang ONNX, app không dùng được — tải đúng file .onnx trong link hướng dẫn"
            ),
            Reason::Packed(ext) => format!(
                "File nén .{ext}: giải nén vào thư mục model, hoặc bấm «Cài model từ file…» (app mở được .zip)"
            ),
            Reason::Partial => "File tải dở — bấm «Tải» để tải tiếp".to_string(),
            Reason::Damaged(why) => format!("File hỏng hoặc chưa tải xong: {why}"),
            Reason::UnknownShape(shape) => format!(
                "Không phải loại model nào app dùng được ({shape}) — model khác họ cần bản app mới"
            ),
            Reason::WrongType(why) => format!("Đúng họ model nhưng {why}"),
            Reason::MissingCompanion(file) => {
                format!("Thiếu file đi kèm {file} — để nó cạnh file .onnx")
            }
            Reason::WrongCompanion(file) => {
                format!("File đi kèm {file} không đúng bản — tải lại cả hai file")
            }
        }
    }
}

/// What a file is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recognition {
    /// A known file, checked by its hash ("✓ đã kiểm").
    Verified(Accepted),
    /// The user's own model of a family the code runs ("✓ tự đặt").
    Custom(Family),
    /// The weights of a model beside it; it goes with that model.
    Companion,
    Unusable(Reason),
    /// Not a model, and not to be mistaken for one (a README, a licence).
    NotAModel,
}

/// The SHA-256 of files, remembered by path, size and modification time.
#[derive(Clone, Debug, Default)]
pub struct HashCache {
    file: Option<PathBuf>,
    entries: HashMap<PathBuf, (u64, u128, String)>,
    changed: bool,
}

impl HashCache {
    /// The cache kept in `file`, empty if there is none or it is unreadable.
    pub fn load(file: PathBuf) -> Self {
        let entries = std::fs::read_to_string(&file)
            .ok()
            .and_then(|text| serde_json::from_str::<Vec<CacheLine>>(&text).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|l| (PathBuf::from(l.path), (l.size, l.modified, l.sha256)))
            .collect();
        Self {
            file: Some(file),
            entries,
            changed: false,
        }
    }

    /// One kept only in memory, for tests.
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// The hash already known for the file as it is now, without reading it.
    pub fn known(&self, path: &Path) -> Option<String> {
        let (size, modified) = stamp(path)?;
        let (s, m, sha) = self.entries.get(path)?;
        ((*s, *m) == (size, modified)).then(|| sha.clone())
    }

    /// The file's hash: the one remembered, or read now and remembered.
    pub fn sha256(&mut self, path: &Path) -> std::io::Result<String> {
        if let Some(sha) = self.known(path) {
            return Ok(sha);
        }
        let (size, modified) = stamp(path).ok_or_else(|| std::io::Error::other("not a file"))?;
        let sha = crate::ai::download::file_sha256_hex(path)?;
        self.remember(path, size, modified, &sha);
        Ok(sha)
    }

    /// Remember `sha` for the file as it is now (one just written whose hash
    /// is known).
    pub fn remember_now(&mut self, path: &Path, sha: &str) {
        if let Some((size, modified)) = stamp(path) {
            self.remember(path, size, modified, sha);
        }
    }

    /// Take in what a copy of this cache learnt.
    pub fn merge(&mut self, other: HashCache) {
        if other.changed {
            self.entries.extend(other.entries);
            self.changed = true;
        }
    }

    fn remember(&mut self, path: &Path, size: u64, modified: u128, sha: &str) {
        self.entries
            .insert(path.to_path_buf(), (size, modified, sha.to_string()));
        self.changed = true;
    }

    /// Keep the cache on disk if anything was added; files no longer there
    /// are dropped from it.
    pub fn save(&mut self) {
        let Some(file) = self.file.clone() else {
            return;
        };
        if !self.changed {
            return;
        }
        self.entries.retain(|path, _| path.is_file());
        let lines: Vec<CacheLine> = self
            .entries
            .iter()
            .map(|(path, (size, modified, sha))| CacheLine {
                path: path.to_string_lossy().into_owned(),
                size: *size,
                modified: *modified,
                sha256: sha.clone(),
            })
            .collect();
        if let Ok(json) = serde_json::to_string(&lines) {
            if let Some(dir) = file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if iai_platform::atomic_file::write_bytes_atomic(&file, json.as_bytes()).is_ok() {
                self.changed = false;
            }
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CacheLine {
    path: String,
    size: u64,
    modified: u128,
    sha256: String,
}

/// A file's size and modification time, `None` when it is not a file.
pub fn stamp(path: &Path) -> Option<(u64, u128)> {
    let metadata = path.metadata().ok().filter(|m| m.is_file())?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    Some((metadata.len(), modified))
}

/// The ending of a file's name, lowercase.
fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// What the file at `path` is. Reads its header and, when it could be a
/// known model, its whole content (once: see [`HashCache`]).
pub fn recognize(path: &Path, known: &Known, cache: &mut HashCache) -> Recognition {
    let ext = extension(path);
    let name = file_name(path);
    match ext.as_str() {
        "onnx" => {}
        "data" | "weights" | "pb" if name.to_ascii_lowercase().contains(".onnx") => {
            return Recognition::Companion;
        }
        "part" | "crdownload" | "partial" | "tmp" => {
            return Recognition::Unusable(Reason::Partial);
        }
        "zip" | "7z" | "rar" | "tar" | "gz" => {
            return Recognition::Unusable(Reason::Packed(ext));
        }
        _ if NOT_CONVERTED.contains(&ext.as_str()) => {
            return Recognition::Unusable(Reason::NotConverted(ext));
        }
        _ => return Recognition::NotAModel,
    }
    let Some((size, _)) = stamp(path) else {
        return Recognition::Unusable(Reason::Damaged("không đọc được".to_string()));
    };
    let header = onnx::read_header(path);
    let family = header.as_ref().ok().and_then(family_of);

    // 1. A known file?
    if let Some(sha) = cache.known(path).or_else(|| {
        known
            .worth_hashing(size, &name, family)
            .then(|| cache.sha256(path).ok())
            .flatten()
    }) && let Some(accepted) = known.by_hash(&sha)
    {
        let dir = path.parent().unwrap_or(Path::new("."));
        for (file, csize, csha) in &accepted.companions {
            match companion_check(&dir.join(file), *csize, csha, cache) {
                CompanionCheck::Fine => {}
                CompanionCheck::Missing => {
                    return Recognition::Unusable(Reason::MissingCompanion(file.clone()));
                }
                CompanionCheck::Wrong => {
                    return Recognition::Unusable(Reason::WrongCompanion(file.clone()));
                }
            }
        }
        return Recognition::Verified(accepted.clone());
    }

    // 2. A file of a family the code runs?
    let header = match header {
        Ok(header) => header,
        Err(why) => return Recognition::Unusable(Reason::Damaged(why)),
    };
    let dir = path.parent().unwrap_or(Path::new("."));
    if let Some(missing) = header
        .external_files
        .iter()
        .find(|file| !dir.join(file).is_file())
    {
        return Recognition::Unusable(Reason::MissingCompanion(missing.clone()));
    }
    match family {
        Some(family) => match type_problem(&header) {
            Some(why) => Recognition::Unusable(Reason::WrongType(why)),
            None => Recognition::Custom(family),
        },
        // 3. Neither.
        None => Recognition::Unusable(Reason::UnknownShape(header.describe())),
    }
}

enum CompanionCheck {
    Fine,
    Missing,
    Wrong,
}

fn companion_check(path: &Path, size: u64, sha: &str, cache: &mut HashCache) -> CompanionCheck {
    let Some((actual, _)) = stamp(path) else {
        return CompanionCheck::Missing;
    };
    if size > 0 && actual != size {
        return CompanionCheck::Wrong;
    }
    if sha.is_empty() {
        return CompanionCheck::Fine;
    }
    match cache.sha256(path) {
        Ok(actual) if actual == sha => CompanionCheck::Fine,
        _ => CompanionCheck::Wrong,
    }
}

/// Whether a weights file beside a model could be a known one's, so worth
/// hashing when the scan meets it.
pub fn companion_could_be_known(path: &Path, known: &Known) -> bool {
    stamp(path).is_some_and(|(size, _)| known.companion_worth_hashing(size, &file_name(path)))
}

/// Every adapter of the crate feeds float tensors: a model that wants
/// another type (a half-precision export) fails when it is run.
fn type_problem(header: &Header) -> Option<String> {
    header
        .inputs
        .iter()
        .find(|t| t.elem_type != FLOAT)
        .map(|t| format!("đầu vào {} không phải số thực 32 bit (bản fp16?)", t.name))
}

/// The family whose code can run a model of this header: the shapes and
/// names the adapters of the crate feed and read (`lama.rs`,
/// `select_subject.rs`, `ai/ocr.rs`, `ai/retouch.rs`, `ai/face_mesh.rs`,
/// `ai/pose.rs`, `ai/body_parts.rs`).
pub fn family_of(header: &Header) -> Option<Family> {
    let single = header.inputs.len() == 1;
    let first_in = header.inputs.first()?;
    let first_out = header.outputs.first()?;
    let has = |name: &str| header.output(name).is_some();

    // YuNet: read by the names of its twelve heads.
    let yunet_heads = ["cls", "obj", "bbox", "kps"]
        .iter()
        .all(|head| [8, 16, 32].iter().all(|s| has(&format!("{head}_{s}"))));
    if yunet_heads && single && shaped(first_in, [ANY, Some(3), ANY, ANY], 1) {
        return Some(Family::YuNet);
    }
    // LaMa: fed "image" and "mask" by name, 512 square.
    if let (Some(img), Some(mask)) = (header.input("image"), header.input("mask"))
        && header.inputs.len() == 2
        && shaped(img, [ANY, Some(3), Some(512), Some(512)], 0)
        && shaped(mask, [ANY, Some(1), Some(512), Some(512)], 0)
        && shaped(first_out, [ANY, Some(3), Some(512), Some(512)], 0)
    {
        return Some(Family::Lama);
    }
    // Sapiens2: fed "pixel_values" at 512×384, read from "logits".
    if let (Some(px), Some(logits)) = (header.input("pixel_values"), header.output("logits"))
        && single
        && shaped(px, [ANY, Some(3), Some(512), Some(384)], 0)
        && shaped(logits, [ANY, Some(29), ANY, ANY], 0)
    {
        return Some(Family::Sapiens2Seg);
    }
    // PP-OCR line recogniser: 48 high, its characters in its metadata.
    if single
        && header.has_metadata("character")
        && shaped(first_in, [ANY, Some(3), Some(48), ANY], 2)
        && first_out.rank() == Some(3)
    {
        return Some(Family::PpOcrRec);
    }
    // MediaPipe pose and face mesh: 256 square NHWC, read by output names.
    if single && shaped(first_in, [ANY, Some(256), Some(256), Some(3)], 3) {
        let landmarks = header.output("Identity");
        if first_in.name == "input_1"
            && has("Identity_1")
            && has("Identity_3")
            && landmarks.is_some_and(|t| t.elements().is_none_or(|n| n == 39 * 5))
        {
            return Some(Family::BlazePose);
        }
        if has("Identity_1") && landmarks.is_some_and(|t| t.elements().is_none_or(|n| n == 478 * 3))
        {
            return Some(Family::FaceMesh);
        }
    }
    if !single {
        return None;
    }
    // BiRefNet and its kind: 1024 square in, one mask out.
    if shaped(first_in, [ANY, Some(3), Some(1024), Some(1024)], 1)
        && shaped(first_out, [ANY, Some(1), ANY, ANY], 1)
    {
        return Some(Family::BiRefNet);
    }
    let fixed_sides = |t: &onnx::Tensor| (1..4).all(|i| t.dim(i).is_fixed());
    // Face restorers: an aligned face, 512 square, in and out.
    let face = [ANY, Some(3), Some(512), Some(512)];
    if shaped(first_in, face, 0)
        && fixed_sides(first_in)
        && shaped(first_out, face, 0)
        && fixed_sides(first_out)
    {
        return Some(Family::FaceRestore);
    }
    // Real-ESRGAN and its kind: a picture of any size in, a larger one out.
    // One of a fixed size cannot take the tiles the upscaler cuts.
    if header.outputs.len() == 1
        && shaped(first_in, [ANY, Some(3), ANY, ANY], 1)
        && !first_in.dim(2).is_fixed()
        && !first_in.dim(3).is_fixed()
        && shaped(first_out, [ANY, Some(3), ANY, ANY], 0)
    {
        return Some(Family::RealEsrgan);
    }
    None
}

/// Any size.
const ANY: Option<i64> = None;

/// `t` is four-sided and each side fits what is wanted of it (a side the
/// model chooses when run fits any size); side `fixed`, besides, is fixed in
/// the file (0 for none: the batch side is never asked to be).
fn shaped(t: &onnx::Tensor, want: [Option<i64>; 4], fixed: usize) -> bool {
    t.rank() == Some(4)
        && want
            .iter()
            .enumerate()
            .all(|(i, w)| w.is_none_or(|size| t.dim(i).fits(size)))
        && (fixed == 0 || t.dim(fixed).is_fixed())
}

/// Whether `family` can fill `slot`.
pub fn fits(family: Family, slot: Slot) -> bool {
    slot.model().family == family
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::onnx::write::Model;
    use super::*;

    pub fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "iai_model_recognize_{name}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A fake model with the inputs and outputs of each family.
    pub fn fake(family: Family) -> Vec<u8> {
        let io =
            |inputs: Vec<(&'static str, Vec<i64>)>, outputs: Vec<(&'static str, Vec<i64>)>| Model {
                inputs,
                outputs,
                weights: vec![("w", vec![0u8; 16], None)],
                ..Model::default()
            };
        let model = match family {
            Family::Lama => io(
                vec![
                    ("image", vec![1, 3, 512, 512]),
                    ("mask", vec![1, 1, 512, 512]),
                ],
                vec![("output", vec![1, 3, 512, 512])],
            ),
            Family::BiRefNet => io(
                vec![("input_image", vec![1, 3, 1024, 1024])],
                vec![("output_image", vec![1, 1, 1024, 1024])],
            ),
            Family::PpOcrRec => Model {
                metadata: vec![("character", "0\n1\n2")],
                ..io(
                    vec![("x", vec![-1, 3, 48, -2])],
                    vec![("softmax", vec![-1, -2, 6625])],
                )
            },
            Family::YuNet => io(
                vec![("input", vec![1, 3, -1, -2])],
                ["cls", "obj", "bbox", "kps"]
                    .iter()
                    .flat_map(|h| {
                        [8, 16, 32].map(|s| {
                            let name: &'static str = Box::leak(format!("{h}_{s}").into_boxed_str());
                            (name, vec![1, -3, 1])
                        })
                    })
                    .collect(),
            ),
            Family::FaceMesh => io(
                vec![("input_12", vec![1, 256, 256, 3])],
                vec![
                    ("Identity", vec![1, 1, 1, 1434]),
                    ("Identity_1", vec![1, 1, 1, 1]),
                    ("Identity_2", vec![1, 1]),
                ],
            ),
            Family::BlazePose => io(
                vec![("input_1", vec![1, 256, 256, 3])],
                vec![
                    ("Identity", vec![1, 195]),
                    ("Identity_1", vec![1, 1]),
                    ("Identity_2", vec![1, 256, 256, 1]),
                    ("Identity_3", vec![1, 64, 64, 39]),
                    ("Identity_4", vec![1, 117]),
                ],
            ),
            Family::Sapiens2Seg => io(
                vec![("pixel_values", vec![1, 3, 512, 384])],
                vec![("logits", vec![1, 29, 512, 384])],
            ),
            Family::FaceRestore => io(
                vec![("input", vec![1, 3, 512, 512])],
                vec![("output", vec![1, 3, 512, 512])],
            ),
            Family::RealEsrgan => io(
                vec![("input", vec![-1, 3, -2, -3])],
                vec![("output", vec![-1, 3, -4, -5])],
            ),
        };
        model.bytes()
    }

    #[test]
    fn each_family_is_told_by_its_shape_and_by_nothing_else() {
        for family in Family::ALL {
            let header = onnx::parse_header(&fake(family)).unwrap();
            assert_eq!(family_of(&header), Some(family), "{family:?}");
        }
        // A classifier: a shape no slot runs.
        let other = Model {
            inputs: vec![("data", vec![1, 3, 224, 224])],
            outputs: vec![("prob", vec![1, 1000])],
            ..Model::default()
        };
        let header = onnx::parse_header(&other.bytes()).unwrap();
        assert_eq!(family_of(&header), None);
        // A face restorer that takes a second input (CodeFormer's fidelity)
        // is not fed it by the code.
        let two = Model {
            inputs: vec![("x", vec![1, 3, 512, 512]), ("w", vec![1])],
            outputs: vec![("y", vec![1, 3, 512, 512])],
            ..Model::default()
        };
        assert_eq!(family_of(&onnx::parse_header(&two.bytes()).unwrap()), None);
        // An upscaler of one fixed size cannot take the tiles it is fed.
        let fixed = Model {
            inputs: vec![("x", vec![1, 3, 64, 64])],
            outputs: vec![("y", vec![1, 3, 256, 256])],
            ..Model::default()
        };
        assert_eq!(
            family_of(&onnx::parse_header(&fixed.bytes()).unwrap()),
            None
        );
    }

    #[test]
    fn a_known_file_is_checked_by_its_hash_and_hashed_once() {
        let dir = temp("verified");
        let bytes = fake(Family::FaceRestore);
        let path = dir.join("renamed anything.onnx");
        std::fs::write(&path, &bytes).unwrap();
        let sha = crate::ai::download::file_sha256_hex(&path).unwrap();
        let known = Known {
            accepted: vec![Accepted {
                size: bytes.len() as u64,
                sha256: sha.clone(),
                ..Accepted::of(Slot::Gfpgan.model())
            }],
        };
        let cache_file = dir.join("checks.json");
        let mut cache = HashCache::load(cache_file.clone());
        match recognize(&path, &known, &mut cache) {
            Recognition::Verified(a) => assert_eq!(a.slot, Slot::Gfpgan),
            other => panic!("{other:?}"),
        }
        cache.save();
        // The next start knows the hash without reading the file.
        let again = HashCache::load(cache_file);
        assert_eq!(again.known(&path), Some(sha));
        // A file changed since is not the one that was hashed.
        std::fs::write(&path, b"changed").unwrap();
        assert_eq!(again.known(&path), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_file_of_a_family_that_is_not_known_is_the_users_own() {
        let dir = temp("custom");
        let path = dir.join("RestoreFormer++.onnx");
        std::fs::write(&path, fake(Family::FaceRestore)).unwrap();
        let mut cache = HashCache::in_memory();
        assert_eq!(
            recognize(&path, &Known::table(), &mut cache),
            Recognition::Custom(Family::FaceRestore)
        );
        assert!(fits(Family::FaceRestore, Slot::Gfpgan));
        assert!(fits(Family::RealEsrgan, Slot::RealesrganRrdbX2));
        assert!(!fits(Family::RealEsrgan, Slot::Gfpgan));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn what_cannot_be_used_says_why() {
        let dir = temp("unusable");
        let mut cache = HashCache::in_memory();
        let known = Known::table();
        let mut check = |name: &str, bytes: &[u8]| {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            recognize(&path, &known, &mut cache)
        };
        for name in [
            "GFPGANv1.4.pth",
            "model.safetensors",
            "sd.ckpt",
            "pytorch_model.bin",
        ] {
            match check(name, b"weights") {
                Recognition::Unusable(reason @ Reason::NotConverted(_)) => {
                    assert!(
                        reason.text().contains("chưa chuyển sang ONNX"),
                        "{}",
                        reason.text()
                    );
                }
                other => panic!("{name}: {other:?}"),
            }
        }
        assert_eq!(
            check("pack.zip", b"PK"),
            Recognition::Unusable(Reason::Packed("zip".into()))
        );
        assert_eq!(
            check("lama_fp32.onnx.part", b"half"),
            Recognition::Unusable(Reason::Partial)
        );
        assert_eq!(check("README.txt", b"read me"), Recognition::NotAModel);
        assert_eq!(check("x.onnx.data", b"weights"), Recognition::Companion);
        assert!(matches!(
            check("broken.onnx", b"<html>"),
            Recognition::Unusable(Reason::Damaged(_))
        ));
        let classifier = Model {
            inputs: vec![("data", vec![1, 3, 224, 224])],
            outputs: vec![("prob", vec![1, 1000])],
            ..Model::default()
        };
        match check("resnet.onnx", &classifier.bytes()) {
            Recognition::Unusable(reason @ Reason::UnknownShape(_)) => {
                assert!(
                    reason.text().contains("data [1, 3, 224, 224]"),
                    "{}",
                    reason.text()
                );
            }
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_model_whose_weights_are_beside_it_needs_them_there() {
        let dir = temp("companion");
        let model = Model {
            inputs: vec![("pixel_values", vec![1, 3, 512, 384])],
            outputs: vec![("logits", vec![1, 29, 512, 384])],
            weights: vec![("w", vec![], Some("mine.onnx.data"))],
            ..Model::default()
        };
        let path = dir.join("mine.onnx");
        std::fs::write(&path, model.bytes()).unwrap();
        let mut cache = HashCache::in_memory();
        assert_eq!(
            recognize(&path, &Known::table(), &mut cache),
            Recognition::Unusable(Reason::MissingCompanion("mine.onnx.data".into()))
        );
        std::fs::write(dir.join("mine.onnx.data"), b"weights").unwrap();
        assert_eq!(
            recognize(&path, &Known::table(), &mut cache),
            Recognition::Custom(Family::Sapiens2Seg)
        );

        // A known one needs the very weights it was made with.
        let bytes = model.bytes();
        let sha = crate::ai::download::file_sha256_hex(&path).unwrap();
        let known = Known {
            accepted: vec![Accepted {
                size: bytes.len() as u64,
                sha256: sha,
                companions: vec![(
                    "mine.onnx.data".into(),
                    0,
                    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into(),
                )],
                ..Accepted::of(Slot::BodyParts.model())
            }],
        };
        assert_eq!(
            recognize(&path, &known, &mut cache),
            Recognition::Unusable(Reason::WrongCompanion("mine.onnx.data".into()))
        );
        std::fs::write(dir.join("mine.onnx.data"), b"abc").unwrap();
        assert!(matches!(
            recognize(&path, &known, &mut cache),
            Recognition::Verified(_)
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_half_precision_export_of_a_family_says_so() {
        let dir = temp("fp16");
        let mut bytes = Model {
            inputs: vec![("input", vec![1, 3, 512, 512])],
            outputs: vec![("output", vec![1, 3, 512, 512])],
            ..Model::default()
        }
        .bytes();
        // The first elem_type FLOAT (1), the input's, becomes FLOAT16 (10).
        let at = bytes.windows(2).position(|w| w == [0x08, 0x01]).unwrap();
        bytes[at + 1] = 10;
        let path = dir.join("gfpgan_fp16.onnx");
        std::fs::write(&path, &bytes).unwrap();
        let header = onnx::read_header(&path).unwrap();
        assert_eq!(header.inputs[0].elem_type, 10);
        match recognize(&path, &Known::table(), &mut HashCache::in_memory()) {
            Recognition::Unusable(reason @ Reason::WrongType(_)) => {
                assert!(reason.text().contains("fp16"), "{}", reason.text());
            }
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}

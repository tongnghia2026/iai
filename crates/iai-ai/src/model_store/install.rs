//! Installing a model from a file the user picked or dropped on the window:
//! an `.onnx` (with its weights file beside it, when it has one) or a `.zip`
//! of models, such as the model packs. Each model in it is recognised (by
//! hash, else by shape) and copied where its slot keeps it in the main model
//! folder; the report says what was taken and what was not, and why.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use super::catalog::Family;
use super::recognize::{self, HashCache, Known, Reason, Recognition};

/// No file in a zip larger than this is taken.
const MAX_ENTRY_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// A model installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    /// The file's name as it came.
    pub name: String,
    /// Where it is now.
    pub to: PathBuf,
    /// What it is, for people: the known model's name, or its family's.
    pub what: String,
    /// Checked by its hash (else the user's own model of a family).
    pub verified: bool,
    /// The very file was there already: nothing was copied.
    pub already: bool,
}

/// A file not installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    pub name: String,
    pub why: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub installed: Vec<Installed>,
    pub skipped: Vec<Skipped>,
}

impl Report {
    /// One line for people: "Đã cài 3 model, bỏ qua 1 file".
    pub fn summary(&self) -> String {
        match (self.installed.len(), self.skipped.len()) {
            (0, 0) => "Không có model nào trong file".to_string(),
            (n, 0) => format!("Đã cài {n} model"),
            (0, s) => format!("Không cài được model nào (bỏ qua {s} file)"),
            (n, s) => format!("Đã cài {n} model, bỏ qua {s} file"),
        }
    }
}

/// Install from `path` into the main model folder `into`.
pub fn install_from(
    path: &Path,
    into: &Path,
    known: &Known,
    cache: &mut HashCache,
) -> Result<Report, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    std::fs::create_dir_all(into).map_err(|e| format!("Không tạo được thư mục model: {e}"))?;
    let mut report = Report::default();
    match ext.as_str() {
        "zip" => install_zip(path, into, known, cache, &mut report)?,
        "onnx" => {
            let staged = Staged {
                path: path.to_path_buf(),
                name: name_of(path),
                movable: false,
            };
            place(&staged, into, known, cache, &mut report);
        }
        _ => {
            let why = match recognize::recognize(path, known, cache) {
                Recognition::Unusable(reason) => reason.text(),
                _ => "Không phải file model (.onnx) hay gói model (.zip)".to_string(),
            };
            report.skipped.push(Skipped {
                name: name_of(path),
                why,
            });
        }
    }
    Ok(report)
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A model file to place: one the user picked (copied), or one unpacked
/// into the staging folder (moved).
struct Staged {
    path: PathBuf,
    name: String,
    movable: bool,
}

fn install_zip(
    path: &Path,
    into: &Path,
    known: &Known,
    cache: &mut HashCache,
    report: &mut Report,
) -> Result<(), String> {
    let file = std::fs::File::open(path).map_err(|e| format!("Không mở được file: {e}"))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("File .zip hỏng hoặc không đọc được: {e}"))?;
    // Unpacked beside its final place, so moving it there is a rename; a
    // hidden folder, which no scan looks in.
    let staging = into.join(format!(".iai-install-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("Không tạo được thư mục tạm: {e}"))?;
    let result = (|| {
        let mut models = Vec::new();
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| format!("File .zip hỏng: {e}"))?;
            if entry.is_dir() {
                continue;
            }
            let shown = entry.name().to_string();
            let Some(relative) = entry.enclosed_name().filter(|p| safe(p)) else {
                report.skipped.push(Skipped {
                    name: shown,
                    why: "Đường dẫn lạ trong file .zip".to_string(),
                });
                continue;
            };
            let name = name_of(&relative);
            let lower = name.to_ascii_lowercase();
            let in_licenses = relative
                .components()
                .any(|c| c.as_os_str().eq_ignore_ascii_case("licenses"));
            let kind = if in_licenses {
                Kind::License
            } else if lower.ends_with(".onnx") {
                Kind::Model
            } else if lower.contains(".onnx.") || entry.size() > 1024 * 1024 {
                // Weights beside a model (any large file may be one).
                Kind::Weights
            } else {
                Kind::Other
            };
            if kind == Kind::Other {
                // The pack's README and checksums: nothing to install.
                if !(lower.starts_with("readme") || lower.starts_with("sha256")) {
                    let why = match recognize::recognize(Path::new(&name), known, cache) {
                        Recognition::Unusable(reason @ Reason::NotConverted(_)) => reason.text(),
                        _ => "Không phải file model".to_string(),
                    };
                    report.skipped.push(Skipped { name, why });
                }
                continue;
            }
            if entry.size() > MAX_ENTRY_BYTES {
                report.skipped.push(Skipped {
                    name,
                    why: "File quá lớn".to_string(),
                });
                continue;
            }
            let out = staging.join(&relative);
            if let Some(dir) = out.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("Không ghi được: {e}"))?;
            }
            let mut writer =
                std::fs::File::create(&out).map_err(|e| format!("Không ghi được: {e}"))?;
            copy_limited(&mut entry, &mut writer, MAX_ENTRY_BYTES)
                .map_err(|e| format!("Không giải nén được {name}: {e}"))?;
            drop(writer);
            match kind {
                Kind::Model => models.push(Staged {
                    path: out,
                    name,
                    movable: true,
                }),
                Kind::License => {
                    let licenses = into.join("licenses");
                    let _ = std::fs::create_dir_all(&licenses);
                    let _ = std::fs::rename(&out, licenses.join(&name));
                }
                Kind::Weights | Kind::Other => {}
            }
        }
        for staged in &models {
            place(staged, into, known, cache, report);
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Model,
    Weights,
    License,
    Other,
}

/// A path inside the zip that stays inside the folder it is unpacked in.
fn safe(path: &Path) -> bool {
    path.components().all(|c| matches!(c, Component::Normal(_)))
}

fn copy_limited(from: &mut dyn Read, to: &mut dyn Write, max: u64) -> std::io::Result<u64> {
    let copied = std::io::copy(&mut from.take(max + 1), to)?;
    if copied > max {
        return Err(std::io::Error::other("larger than it said"));
    }
    to.flush()?;
    Ok(copied)
}

/// Put one model where its slot keeps it, with the files it needs.
fn place(staged: &Staged, into: &Path, known: &Known, cache: &mut HashCache, report: &mut Report) {
    let skip = |report: &mut Report, why: String| {
        report.skipped.push(Skipped {
            name: staged.name.clone(),
            why,
        });
    };
    let source_dir = staged.path.parent().unwrap_or(Path::new("."));
    let (dir, file, what, verified, companions): (String, String, String, bool, Vec<String>) =
        match recognize::recognize(&staged.path, known, cache) {
            Recognition::Verified(accepted) => {
                let model = accepted.slot.model();
                // A file the author's list adds goes in its slot's folder.
                let file = if accepted.file.is_empty() {
                    staged.name.clone()
                } else {
                    accepted.file.clone()
                };
                let companions = accepted.companions.iter().map(|c| c.0.clone()).collect();
                (model.dir.to_string(), file, accepted.name, true, companions)
            }
            Recognition::Custom(family) => {
                let companions = super::onnx::read_header(&staged.path)
                    .map(|h| h.external_files)
                    .unwrap_or_default();
                (
                    family.dir().to_string(),
                    staged.name.clone(),
                    custom_name(family),
                    false,
                    companions,
                )
            }
            Recognition::Unusable(reason) => return skip(report, reason.text()),
            Recognition::Companion | Recognition::NotAModel => {
                return skip(report, "Không phải file model".to_string());
            }
        };
    let mut target_dir = into.join(&dir);
    let mut target = target_dir.join(&file);
    // The very file in place already: nothing to copy.
    if same_file(&staged.path, &target, cache) {
        report.installed.push(Installed {
            name: staged.name.clone(),
            to: target,
            what,
            verified,
            already: true,
        });
        return;
    }
    // Another model of the user's under the same name: this one goes in a
    // folder of its own beside it, with its weights. A known model takes its
    // place over whatever is there.
    if !verified && target.exists() {
        let stem = Path::new(&file)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut n = 2;
        loop {
            target_dir = into.join(&dir).join(format!("{stem}-{n}"));
            if !target_dir.exists() {
                break;
            }
            n += 1;
        }
        target = target_dir.join(&file);
    }
    if let Err(e) = std::fs::create_dir_all(&target_dir) {
        return skip(report, format!("Không tạo được thư mục: {e}"));
    }
    for companion in &companions {
        let from = source_dir.join(companion);
        let to = target_dir.join(companion);
        if !same_file(&from, &to, cache)
            && let Err(e) = put(&from, &to, staged.movable)
        {
            return skip(report, format!("Không chép được {companion}: {e}"));
        }
    }
    match put(&staged.path, &target, staged.movable) {
        Ok(()) => {
            if let Ok(sha) = cache.sha256(&target) {
                cache.remember_now(&target, &sha);
            }
            report.installed.push(Installed {
                name: staged.name.clone(),
                to: target,
                what,
                verified,
                already: false,
            });
        }
        Err(e) => skip(report, format!("Không chép được: {e}")),
    }
}

fn custom_name(family: Family) -> String {
    format!("model tự đặt họ {}", family.label())
}

/// `a` and `b` are the same content (and `b` exists).
fn same_file(a: &Path, b: &Path, cache: &mut HashCache) -> bool {
    match (recognize::stamp(a), recognize::stamp(b)) {
        (Some((sa, _)), Some((sb, _))) if sa == sb => {
            matches!((cache.sha256(a), cache.sha256(b)), (Ok(x), Ok(y)) if x == y)
        }
        _ => false,
    }
}

/// Move (from the staging folder) or copy (from where the user keeps it)
/// `from` to `to`, through a `.part` so a copy cut short is never taken for
/// the model.
fn put(from: &Path, to: &Path, movable: bool) -> std::io::Result<()> {
    if movable && std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    let part = super::fetch::part_path(to);
    std::fs::copy(from, &part)?;
    std::fs::rename(&part, to)
}

/// Install from `path` on this machine: into the main model folder, the
/// models known by the table and the author's list. The folders are looked
/// through again afterwards.
pub fn install(path: &Path) -> Result<Report, String> {
    let known = super::known();
    let into = super::main_folder();
    let report = super::with_cache(|cache| install_from(path, &into, &known, cache))?;
    super::forget_found();
    super::rescan();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::super::catalog::Slot;
    use super::super::onnx::write::Model;
    use super::super::recognize::Accepted;
    use super::super::recognize::tests::{fake, temp};
    use super::*;

    fn known_face_restorer(bytes: &[u8], dir: &Path) -> Known {
        let probe = dir.join("probe.onnx");
        std::fs::write(&probe, bytes).unwrap();
        let sha = crate::ai::download::file_sha256_hex(&probe).unwrap();
        std::fs::remove_file(&probe).unwrap();
        Known {
            accepted: vec![Accepted {
                size: bytes.len() as u64,
                sha256: sha,
                ..Accepted::of(Slot::Gfpgan.model())
            }],
        }
    }

    #[test]
    fn a_known_onnx_goes_where_its_slot_keeps_it_under_its_own_name() {
        let base = temp("install_known");
        let bytes = fake(Family::FaceRestore);
        let known = known_face_restorer(&bytes, &base);
        let picked = base.join("Downloads").join("gfpgan (1).onnx");
        std::fs::create_dir_all(picked.parent().unwrap()).unwrap();
        std::fs::write(&picked, &bytes).unwrap();
        let into = base.join("models");
        let mut cache = HashCache::in_memory();

        let report = install_from(&picked, &into, &known, &mut cache).unwrap();
        let to = into.join("gfpgan").join("GFPGANv1.4.onnx");
        assert_eq!(report.installed.len(), 1, "{report:?}");
        assert_eq!(report.installed[0].to, to);
        assert!(report.installed[0].verified);
        assert_eq!(std::fs::read(&to).unwrap(), bytes);
        assert!(picked.is_file(), "the user's file stays where it was");
        // Again: it is there already.
        let again = install_from(&picked, &into, &known, &mut cache).unwrap();
        assert!(again.installed[0].already);
        assert_eq!(report.summary(), "Đã cài 1 model");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_users_own_model_goes_in_its_familys_folder_and_never_over_another() {
        let base = temp("install_custom");
        let into = base.join("models");
        let mut cache = HashCache::in_memory();
        let first = base.join("a").join("4x-Mine.onnx");
        std::fs::create_dir_all(first.parent().unwrap()).unwrap();
        std::fs::write(&first, fake(Family::RealEsrgan)).unwrap();
        let report = install_from(&first, &into, &Known::table(), &mut cache).unwrap();
        assert_eq!(
            report.installed[0].to,
            into.join("realesrgan").join("4x-Mine.onnx")
        );
        assert!(!report.installed[0].verified);
        assert!(report.installed[0].what.contains("Real-ESRGAN"));
        // Another model under the same name.
        let mut other = fake(Family::RealEsrgan);
        other.extend_from_slice(b"\x7a\x00");
        let second = base.join("b").join("4x-Mine.onnx");
        std::fs::create_dir_all(second.parent().unwrap()).unwrap();
        std::fs::write(&second, &other).unwrap();
        let report = install_from(&second, &into, &Known::table(), &mut cache).unwrap();
        assert_eq!(
            report.installed[0].to,
            into.join("realesrgan")
                .join("4x-Mine-2")
                .join("4x-Mine.onnx")
        );
        assert_eq!(
            std::fs::read(into.join("realesrgan").join("4x-Mine.onnx")).unwrap(),
            fake(Family::RealEsrgan)
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_zip_installs_each_model_with_its_weights_and_says_what_it_left() {
        let base = temp("install_zip");
        let into = base.join("models");
        let zip_path = base.join("pack.zip");
        let segmenter = Model {
            inputs: vec![("pixel_values", vec![1, 3, 512, 384])],
            outputs: vec![("logits", vec![1, 29, 512, 384])],
            weights: vec![("w", vec![], Some("seg.onnx.data"))],
            ..Model::default()
        }
        .bytes();
        {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
            let options = zip::write::SimpleFileOptions::default();
            let mut add = |name: &str, bytes: &[u8]| {
                zip.start_file(name, options).unwrap();
                zip.write_all(bytes).unwrap();
            };
            add("iAi-models/README.txt", b"giai nen vao models");
            add("iAi-models/licenses/Sapiens2-LICENSE.md", b"licence");
            add("iAi-models/sapiens2-seg/seg.onnx", &segmenter);
            add("iAi-models/sapiens2-seg/seg.onnx.data", b"weights");
            add("iAi-models/x/lama.onnx", &fake(Family::Lama));
            add("iAi-models/gfpgan/GFPGANv1.4.pth", b"torch");
            add("../escape.onnx", &fake(Family::Lama));
            zip.finish().unwrap();
        }
        let mut cache = HashCache::in_memory();
        let report = install_from(&zip_path, &into, &Known::table(), &mut cache).unwrap();

        let mut to: Vec<PathBuf> = report.installed.iter().map(|i| i.to.clone()).collect();
        to.sort();
        assert_eq!(
            to,
            [
                into.join("lama.onnx"),
                into.join("sapiens2-seg").join("seg.onnx")
            ],
            "{report:?}"
        );
        assert_eq!(
            std::fs::read(into.join("sapiens2-seg").join("seg.onnx.data")).unwrap(),
            b"weights"
        );
        assert!(into.join("licenses").join("Sapiens2-LICENSE.md").is_file());
        let skipped: Vec<(&str, &str)> = report
            .skipped
            .iter()
            .map(|s| (s.name.as_str(), s.why.as_str()))
            .collect();
        assert_eq!(skipped.len(), 2, "{skipped:?}");
        assert!(
            skipped
                .iter()
                .any(|(n, w)| *n == "GFPGANv1.4.pth" && w.contains("chưa chuyển sang ONNX"))
        );
        assert!(skipped.iter().any(|(n, _)| n.contains("escape")));
        assert!(!base.join("escape.onnx").exists());
        // Nothing of the unpacking is left behind.
        let hidden: Vec<_> = std::fs::read_dir(&into)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
            .collect();
        assert!(hidden.is_empty());
        assert_eq!(report.summary(), "Đã cài 2 model, bỏ qua 2 file");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn what_is_not_a_model_is_left_with_the_reason() {
        let base = temp("install_other");
        let into = base.join("models");
        let mut cache = HashCache::in_memory();
        for (name, bytes, why) in [
            ("model.safetensors", &b"w"[..], "chưa chuyển sang ONNX"),
            ("photo.jpg", b"jpeg", "Không phải file model"),
            ("broken.onnx", b"<html>", "hỏng"),
        ] {
            let path = base.join(name);
            std::fs::write(&path, bytes).unwrap();
            let report = install_from(&path, &into, &Known::table(), &mut cache).unwrap();
            assert!(report.installed.is_empty());
            assert!(
                report.skipped[0].why.contains(why),
                "{name}: {}",
                report.skipped[0].why
            );
        }
        let _ = std::fs::remove_dir_all(base);
    }
}

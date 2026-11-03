//! `bundle-models`: the model packs of link 2, from a folder of models.
//!
//! ```text
//! cargo xtask bundle-models --from <folder of models> --out <folder>
//! ```
//!
//! What goes in is decided by the model table of `iai-ai`
//! (`crates/iai-ai/src/model_store/catalog.rs`, included here as it is): a
//! file is taken only when its SHA-256 is that of a model whose licence lets
//! it be handed out, so nothing else in the folder (a model of the user's
//! own, a stray copy) can end up in a pack. One pack, «Gói model iAi», of
//! every such model (03/11: one folder, on the author's Drive, for the Pro
//! pack's customers): one zip, cut into several (each whole on its own)
//! only past `ZIP_LIMIT`, holding the
//! models under their subfolders, `licenses/` with the licence of each, a
//! `README.txt` for the user and `SHA256SUMS.txt`. Files are stored, not
//! compressed: model weights do not compress.
//!
//! Models of the table with no hash or size yet are reported with the hash
//! and size of the file found under their name, to be written into the
//! table and `docs/author/models.json`.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[allow(dead_code)]
#[path = "../../crates/iai-ai/src/model_store/catalog.rs"]
mod catalog;

/// Drive takes files far larger than the pack: it is cut only past this.
pub const ZIP_LIMIT: u64 = 20_000_000_000;
/// What a stored zip adds for each file besides its bytes and name twice:
/// the local header (30), the central directory entry (46), and room.
const PER_FILE: u64 = 30 + 46 + 64;
/// The end of the central directory, and room.
const PER_ZIP: u64 = 22 + 1024;
/// What each zip keeps for itself: its end, `README.txt` and
/// `SHA256SUMS.txt` (a few hundred bytes a model).
const RESERVED: u64 = PER_ZIP + 8 * 1024;
/// How deep the source folder is looked through.
const MAX_DEPTH: usize = 10;

/// A file a model is made of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub file: String,
    pub size: u64,
    pub sha256: String,
}

/// A model of the table, as the packs need it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub dir: String,
    /// The model, then the files it cannot run without.
    pub parts: Vec<Part>,
    pub license: String,
    pub license_file: String,
    /// `basic` or `none`.
    pub pack: String,
}

impl Item {
    fn path_in_zip(&self, file: &str) -> String {
        if self.dir.is_empty() {
            file.to_string()
        } else {
            format!("{}/{file}", self.dir)
        }
    }
}

/// The table's models.
pub fn table() -> Vec<Item> {
    catalog::MODELS
        .iter()
        .map(|m| Item {
            id: m.id.to_string(),
            name: m.name.to_string(),
            dir: m.dir.to_string(),
            parts: std::iter::once(Part {
                file: m.file.to_string(),
                size: m.size,
                sha256: m.sha256.to_string(),
            })
            .chain(m.companions.iter().map(|c| Part {
                file: c.file.to_string(),
                size: c.size,
                sha256: c.sha256.to_string(),
            }))
            .collect(),
            license: m.license.to_string(),
            license_file: m.license_file.to_string(),
            pack: if m.redistributable {
                m.pack.id().to_string()
            } else {
                catalog::Pack::None.id().to_string()
            },
        })
        .collect()
}

/// The packs, in order: (id, name for people, zip name without `.zip`).
const PACKS: [(&str, &str, &str); 1] = [("basic", "Gói model iAi", "iAi-models")];

pub fn run(args: Vec<String>) -> Result<(), String> {
    const USAGE: &str = "usage: cargo xtask bundle-models --from <folder of models> --out <folder>";
    let (mut from, mut out) = (None, None);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--from" => from = args.next(),
            "--out" => out = args.next(),
            _ => return Err(USAGE.to_owned()),
        }
    }
    let (Some(from), Some(out)) = (from, out) else {
        return Err(USAGE.to_owned());
    };
    let licenses = super::workspace_root()?.join("licenses");
    let report = bundle(
        Path::new(&from),
        Path::new(&out),
        &table(),
        &licenses,
        ZIP_LIMIT,
    )?;
    println!("{}", report.text());
    Ok(())
}

/// What a bundle made, and what it left out.
#[derive(Debug, Default)]
pub struct Report {
    /// (zip, its size, its SHA-256, the ids in it).
    pub zips: Vec<(PathBuf, u64, String, Vec<String>)>,
    /// Models that may be handed out but were not found, with why.
    pub missing: Vec<String>,
    /// Lines for the table: a model found under its name with no hash or
    /// size in the table yet.
    pub to_measure: Vec<String>,
}

impl Report {
    pub fn text(&self) -> String {
        let mut text = String::new();
        for (zip, size, sha, ids) in &self.zips {
            text += &format!(
                "{}  {size} bytes  sha256 {sha}\n    {}\n",
                zip.display(),
                ids.join(", ")
            );
        }
        if !self.missing.is_empty() {
            text += "\nNot in any pack:\n";
            for line in &self.missing {
                text += &format!("    {line}\n");
            }
        }
        if !self.to_measure.is_empty() {
            text += "\nTo write into crates/iai-ai/src/model_store/catalog.rs and docs/author/models.json:\n";
            for line in &self.to_measure {
                text += &format!("    {line}\n");
            }
        }
        text
    }
}

/// Every file under `dir`, with its size.
fn files_under(dir: &Path, depth: usize, out: &mut Vec<(PathBuf, u64)>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            files_under(&entry.path(), depth + 1, out);
        } else if kind.is_file() {
            let size = entry.metadata().map_or(0, |m| m.len());
            out.push((entry.path(), size));
        }
    }
}

fn sha256_of(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = hmac_sha256::Hash::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(buf.get(..n).unwrap_or_default());
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Build the packs of `items` from the files under `from` into `out`, each
/// zip no larger than `limit`, the licences taken from `licenses`.
pub fn bundle(
    from: &Path,
    out: &Path,
    items: &[Item],
    licenses: &Path,
    limit: u64,
) -> Result<Report, String> {
    if !from.is_dir() {
        return Err(format!("not a folder: {}", from.display()));
    }
    let mut files = Vec::new();
    files_under(from, 0, &mut files);
    files.sort();

    // Hash only what could be a part: its size, or (size not measured) its
    // name. Each file once.
    let mut hashes: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut hash = |path: &Path| -> Result<String, String> {
        if let Some(sha) = hashes.get(path) {
            return Ok(sha.clone());
        }
        let sha = sha256_of(path)?;
        hashes.insert(path.to_path_buf(), sha.clone());
        Ok(sha)
    };

    let mut report = Report::default();
    // (item, the source of each of its parts)
    let mut found: Vec<(&Item, Vec<PathBuf>)> = Vec::new();
    for item in items {
        let mut sources = Vec::new();
        for part in &item.parts {
            let mut source = None;
            for (path, size) in &files {
                let named = file_name(path).eq_ignore_ascii_case(&part.file);
                let candidate = if part.size > 0 {
                    *size == part.size
                } else {
                    named
                };
                if !candidate {
                    continue;
                }
                let sha = hash(path)?;
                if !part.sha256.is_empty() && sha == part.sha256 {
                    if part.size == 0 {
                        report.to_measure.push(format!(
                            "{} {}: size {size} (sha256 {sha})",
                            item.id, part.file
                        ));
                    }
                    source = Some(path.clone());
                    break;
                }
                if part.sha256.is_empty() && named {
                    report.to_measure.push(format!(
                        "{} {}: sha256 {sha}, size {size} ({})",
                        item.id,
                        part.file,
                        path.display()
                    ));
                }
            }
            sources.push(source);
        }
        if item.pack == catalog::Pack::None.id() {
            continue;
        }
        let missing: Vec<&str> = item
            .parts
            .iter()
            .zip(&sources)
            .filter(|(_, s)| s.is_none())
            .map(|(p, _)| p.file.as_str())
            .collect();
        if !missing.is_empty() {
            let why = if item.parts.iter().any(|p| p.sha256.is_empty()) {
                "no SHA-256 in the table yet"
            } else {
                "no file with its SHA-256"
            };
            report
                .missing
                .push(format!("{} ({}): {why}", item.id, missing.join(", ")));
            continue;
        }
        if item.license_file.is_empty() || !licenses.join(&item.license_file).is_file() {
            report.missing.push(format!(
                "{}: its licence file licenses/{} is not there",
                item.id, item.license_file
            ));
            continue;
        }
        found.push((item, sources.into_iter().flatten().collect()));
    }

    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    for (pack, pack_name, zip_name) in PACKS {
        let members: Vec<&(&Item, Vec<PathBuf>)> =
            found.iter().filter(|(item, _)| item.pack == pack).collect();
        if members.is_empty() {
            continue;
        }
        // Cut into zips under the limit, each model whole in one.
        let mut groups: Vec<Vec<&(&Item, Vec<PathBuf>)>> = vec![Vec::new()];
        let mut used = RESERVED;
        for member in members {
            let (item, _) = member;
            let size: u64 = item
                .parts
                .iter()
                .map(|p| {
                    let name = item.path_in_zip(&p.file).len() as u64;
                    p.size.max(1) + PER_FILE + 2 * name
                })
                .sum::<u64>()
                + licence_size(licenses, item);
            let size = size.max(actual_size(member));
            if RESERVED + size > limit {
                return Err(format!(
                    "{} is {size} bytes: larger than one zip may be ({limit})",
                    item.id
                ));
            }
            if used + size > limit && groups.last().is_some_and(|g| !g.is_empty()) {
                groups.push(Vec::new());
                used = RESERVED;
            }
            used += size;
            if let Some(group) = groups.last_mut() {
                group.push(member);
            }
        }
        let several = groups.len() > 1;
        for (n, group) in groups.iter().enumerate() {
            let name = if several {
                format!("{zip_name}-{}.zip", n + 1)
            } else {
                format!("{zip_name}.zip")
            };
            let path = out.join(&name);
            let part = if several {
                format!("{pack_name}, phần {} / {}", n + 1, groups.len())
            } else {
                pack_name.to_string()
            };
            write_zip(&path, &part, group, licenses)?;
            let size = std::fs::metadata(&path)
                .map_err(|e| format!("{}: {e}", path.display()))?
                .len();
            if size > limit {
                return Err(format!(
                    "{} came out at {size} bytes, past {limit}",
                    path.display()
                ));
            }
            let sha = sha256_of(&path)?;
            let ids = group.iter().map(|(item, _)| item.id.clone()).collect();
            report.zips.push((path, size, sha, ids));
        }
    }
    Ok(report)
}

fn licence_size(licenses: &Path, item: &Item) -> u64 {
    std::fs::metadata(licenses.join(&item.license_file)).map_or(0, |m| m.len())
        + PER_FILE
        + 2 * (item.license_file.len() as u64 + 9)
}

/// The bytes of a model's files as they are on disk (the table may not
/// have measured them).
fn actual_size((item, sources): &(&Item, Vec<PathBuf>)) -> u64 {
    sources
        .iter()
        .zip(&item.parts)
        .map(|(s, p)| {
            std::fs::metadata(s).map_or(0, |m| m.len())
                + PER_FILE
                + 2 * item.path_in_zip(&p.file).len() as u64
        })
        .sum()
}

/// The one folder everything of a pack sits in: unzipped beside iai.exe it
/// is the model folder (03/11: "one folder").
const TOP: &str = "models";

fn write_zip(
    path: &Path,
    pack_name: &str,
    group: &[&(&Item, Vec<PathBuf>)],
    licenses: &Path,
) -> Result<(), String> {
    let at = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let part = path.with_extension("zip.part");
    let file = std::fs::File::create(&part).map_err(|e| at(&e))?;
    let mut zip = zip::ZipWriter::new(file);
    let stored = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .large_file(false);

    let mut readme = readme(pack_name, group);
    readme = readme.replace('\n', "\r\n");
    zip.start_file(format!("{TOP}/README.txt"), stored)
        .map_err(|e| at(&e))?;
    zip.write_all(readme.as_bytes()).map_err(|e| at(&e))?;

    let mut sums = String::new();
    let mut licences_done = std::collections::BTreeSet::new();
    for (item, sources) in group {
        if licences_done.insert(item.license_file.clone()) {
            let text = std::fs::read(licenses.join(&item.license_file)).map_err(|e| at(&e))?;
            zip.start_file(format!("{TOP}/licenses/{}", item.license_file), stored)
                .map_err(|e| at(&e))?;
            zip.write_all(&text).map_err(|e| at(&e))?;
        }
        for (source, part) in sources.iter().zip(&item.parts) {
            let inside = item.path_in_zip(&part.file);
            zip.start_file(format!("{TOP}/{inside}"), stored)
                .map_err(|e| at(&e))?;
            let mut reader = std::fs::File::open(source).map_err(|e| at(&e))?;
            std::io::copy(&mut reader, &mut zip).map_err(|e| at(&e))?;
            sums += &format!("{}  {inside}\r\n", part.sha256);
        }
    }
    zip.start_file(format!("{TOP}/SHA256SUMS.txt"), stored)
        .map_err(|e| at(&e))?;
    zip.write_all(sums.as_bytes()).map_err(|e| at(&e))?;
    let mut file = zip.finish().map_err(|e| at(&e))?;
    file.flush().map_err(|e| at(&e))?;
    drop(file);
    std::fs::rename(&part, path).map_err(|e| at(&e))
}

/// The user's guide inside a pack, in Vietnamese.
fn readme(pack_name: &str, group: &[&(&Item, Vec<PathBuf>)]) -> String {
    let mut text = format!(
        "iAi — {pack_name}\n\
         ==============================\n\n\
         Cách dùng: giải nén file này vào thư mục chứa iai.exe (ví dụ D:\\iAi):\n\
         ra đúng một thư mục models nằm cạnh iai.exe (đã có thì cứ chép đè).\n\
         Giải nén kiểu nào cũng được, kể cả có thêm một thư mục con: app tự\n\
         tìm model trong mọi thư mục con và tự kiểm từng file. Mở iAi, vào\n\
         Help > Model AI để xem model nào đã sẵn sàng.\n\n\
         Không cần đổi tên file. Không xóa thư mục licenses: đó là giấy phép\n\
         của các model, phải đi kèm khi chép cho người khác.\n\n\
         Trong gói này:\n"
    );
    for (item, _) in group {
        text += &format!("- {} — giấy phép {}\n", item.name, item.license);
    }
    text += "\nMã kiểm tra SHA-256 của từng file: SHA256SUMS.txt.\n";
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "iai_bundle_models_{name}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sha(bytes: &[u8]) -> String {
        let mut h = hmac_sha256::Hash::new();
        h.update(bytes);
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    fn item(id: &str, dir: &str, parts: &[(&str, &[u8])], pack: &str, licence: &str) -> Item {
        Item {
            id: id.into(),
            name: format!("Model {id}"),
            dir: dir.into(),
            parts: parts
                .iter()
                .map(|(file, bytes)| Part {
                    file: (*file).into(),
                    size: bytes.len() as u64,
                    sha256: sha(bytes),
                })
                .collect(),
            license: "MIT".into(),
            license_file: licence.into(),
            pack: pack.into(),
        }
    }

    fn entries(zip: &Path) -> BTreeMap<String, Vec<u8>> {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(zip).unwrap()).unwrap();
        (0..archive.len())
            .map(|i| {
                let mut entry = archive.by_index(i).unwrap();
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).unwrap();
                // Everything sits in the one folder; named here inside it.
                let name = entry.name().to_string();
                let inside = name.strip_prefix("models/").map(str::to_string);
                (
                    inside.unwrap_or_else(|| panic!("{name} is outside models/")),
                    bytes,
                )
            })
            .collect()
    }

    #[test]
    fn the_packs_take_only_the_right_files_that_may_be_handed_out() {
        let base = temp("packs");
        let (from, out, licenses) = (base.join("from"), base.join("out"), base.join("licenses"));
        std::fs::create_dir_all(from.join("deep").join("ocr")).unwrap();
        std::fs::create_dir_all(&licenses).unwrap();
        std::fs::write(licenses.join("MIT.txt"), b"MIT licence").unwrap();
        std::fs::write(licenses.join("Sapiens2-LICENSE.md"), b"Sapiens2 licence").unwrap();
        let (lama, ocr, seg, data, gfpgan) = (
            vec![1u8; 300],
            vec![2u8; 200],
            vec![3u8; 50],
            vec![4u8; 900],
            vec![5u8; 100],
        );
        // Renamed, deep: found by hash.
        std::fs::write(from.join("lama-renamed.onnx"), &lama).unwrap();
        std::fs::write(from.join("deep").join("ocr").join("rec.onnx"), &ocr).unwrap();
        std::fs::write(from.join("seg.onnx"), &seg).unwrap();
        std::fs::write(from.join("seg.onnx.data"), &data).unwrap();
        std::fs::write(from.join("GFPGANv1.4.onnx"), &gfpgan).unwrap();
        // The user's own upscaler under a known name: wrong hash, left out.
        let mut wrong = vec![6u8; 120];
        std::fs::write(from.join("RealESRGAN_x2plus.onnx"), &wrong).unwrap();
        wrong[0] = 7;
        let items = vec![
            item("lama", "", &[("lama_fp32.onnx", &lama)], "basic", "MIT.txt"),
            item("ocr", "ocr", &[("rec.onnx", &ocr)], "basic", "MIT.txt"),
            item(
                "seg",
                "sapiens2-seg",
                &[("seg.onnx", &seg), ("seg.onnx.data", &data)],
                "basic",
                "Sapiens2-LICENSE.md",
            ),
            item(
                "gfpgan",
                "gfpgan",
                &[("GFPGANv1.4.onnx", &gfpgan)],
                "none",
                "",
            ),
            item(
                "x2",
                "realesrgan",
                &[("RealESRGAN_x2plus.onnx", &wrong)],
                "basic",
                "MIT.txt",
            ),
        ];

        let report = bundle(&from, &out, &items, &licenses, ZIP_LIMIT).unwrap();

        assert_eq!(report.zips.len(), 1, "{}", report.text());
        let basic = entries(&out.join("iAi-models.zip"));
        let names: Vec<&str> = basic.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            [
                "README.txt",
                "SHA256SUMS.txt",
                "lama_fp32.onnx",
                "licenses/MIT.txt",
                "licenses/Sapiens2-LICENSE.md",
                "ocr/rec.onnx",
                "sapiens2-seg/seg.onnx",
                "sapiens2-seg/seg.onnx.data"
            ]
        );
        assert_eq!(basic["lama_fp32.onnx"], lama);
        let readme = String::from_utf8(basic["README.txt"].clone()).unwrap();
        assert!(
            readme.contains("giải nén file này vào thư mục chứa iai.exe"),
            "{readme}"
        );
        let sums = String::from_utf8(basic["SHA256SUMS.txt"].clone()).unwrap();
        assert!(
            sums.contains(&format!("{}  ocr/rec.onnx", sha(&ocr))),
            "{sums}"
        );

        // Sapiens2 and its weights in the same one zip (03/11).
        assert_eq!(basic["sapiens2-seg/seg.onnx.data"], data);
        assert!(basic.contains_key("licenses/Sapiens2-LICENSE.md"));
        // A model in no pack, and a file that is not the model, stay out.
        assert!(
            report
                .zips
                .iter()
                .all(|(_, _, _, ids)| !ids.contains(&"gfpgan".to_string()))
        );
        assert_eq!(report.missing.len(), 1, "{:?}", report.missing);
        assert!(report.missing[0].starts_with("x2"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_pack_past_the_limit_is_cut_into_zips_each_whole() {
        let base = temp("split");
        let (from, out, licenses) = (base.join("from"), base.join("out"), base.join("licenses"));
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&licenses).unwrap();
        std::fs::write(licenses.join("MIT.txt"), b"MIT").unwrap();
        let models: Vec<Vec<u8>> = (0..3u8).map(|i| vec![i; 40_000]).collect();
        let mut items = Vec::new();
        for (i, bytes) in models.iter().enumerate() {
            let file = format!("m{i}.onnx");
            std::fs::write(from.join(&file), bytes).unwrap();
            items.push(item(
                &format!("m{i}"),
                "",
                &[(&file, bytes)],
                "basic",
                "MIT.txt",
            ));
        }
        let limit = 100_000;
        let report = bundle(&from, &out, &items, &licenses, limit).unwrap();
        assert_eq!(report.zips.len(), 2, "{}", report.text());
        for (zip, size, _, _) in &report.zips {
            assert!(*size <= limit);
            let inside = entries(zip);
            assert!(inside.contains_key("README.txt") && inside.contains_key("licenses/MIT.txt"));
        }
        assert!(out.join("iAi-models-1.zip").is_file());
        assert!(out.join("iAi-models-2.zip").is_file());
        // One model larger than a zip may be is refused, not cut.
        let big = vec![9u8; 120_000];
        std::fs::write(from.join("big.onnx"), &big).unwrap();
        let too_big = vec![item("big", "", &[("big.onnx", &big)], "basic", "MIT.txt")];
        let error = bundle(&from, &base.join("out2"), &too_big, &licenses, limit).unwrap_err();
        assert!(error.contains("larger than one zip"), "{error}");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_model_the_table_has_not_measured_is_reported_with_its_hash() {
        let base = temp("measure");
        let (from, out, licenses) = (base.join("from"), base.join("out"), base.join("licenses"));
        std::fs::create_dir_all(from.join("pose")).unwrap();
        std::fs::create_dir_all(&licenses).unwrap();
        std::fs::write(licenses.join("MIT.txt"), b"MIT").unwrap();
        let pose = b"the pose model".to_vec();
        std::fs::write(
            from.join("pose").join("pose_landmarks_detector.onnx"),
            &pose,
        )
        .unwrap();
        let unmeasured = Item {
            parts: vec![Part {
                file: "pose_landmarks_detector.onnx".into(),
                size: 0,
                sha256: String::new(),
            }],
            ..item("pose", "pose", &[], "basic", "MIT.txt")
        };
        let report = bundle(&from, &out, &[unmeasured], &licenses, ZIP_LIMIT).unwrap();
        assert!(report.zips.is_empty());
        assert!(
            report.missing[0].contains("no SHA-256"),
            "{:?}",
            report.missing
        );
        assert!(
            report.to_measure[0].contains(&sha(&pose)) && report.to_measure[0].contains("size 14"),
            "{:?}",
            report.to_measure
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn the_table_of_iai_ai_makes_the_one_pack() {
        let items = table();
        let pack = |id: &str| items.iter().find(|i| i.id == id).map(|i| i.pack.clone());
        assert_eq!(pack("sapiens2-seg").as_deref(), Some("basic"));
        assert_eq!(pack("gfpgan-1.4").as_deref(), Some("basic"));
        assert_eq!(pack("lama").as_deref(), Some("basic"));
        let root = super::super::workspace_root().unwrap();
        for item in items.iter().filter(|i| i.pack != "none") {
            assert!(
                root.join("licenses").join(&item.license_file).is_file(),
                "{}: licenses/{}",
                item.id,
                item.license_file
            );
        }
        let sapiens = items.iter().find(|i| i.id == "sapiens2-seg").unwrap();
        assert_eq!(sapiens.parts.len(), 2);
    }
}

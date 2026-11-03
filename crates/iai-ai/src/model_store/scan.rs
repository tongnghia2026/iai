//! Every model folder looked through, all the way down: a pack unpacked in
//! whatever way, a model in no subfolder or under another name, is found.

use std::path::{Path, PathBuf};

use super::catalog::{Family, Slot};
use super::recognize::{self, HashCache, Known, Reason, Recognition};

/// How deep under a model folder files are looked for.
const MAX_DEPTH: usize = 8;
/// No more entries than this are looked at in one scan: a model folder
/// pointed at a whole drive is not walked to its end.
const MAX_ENTRIES: usize = 50_000;

/// A file found in a model folder, and what it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    pub size: u64,
    pub recognition: Recognition,
}

/// What the model folders hold.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inventory {
    /// The folders looked through, most authoritative first.
    pub roots: Vec<PathBuf>,
    /// The files that are models, or look like ones, in the order of the
    /// roots and then of their paths.
    pub found: Vec<Found>,
    /// The scan stopped at [`MAX_ENTRIES`].
    pub cut_short: bool,
}

impl Inventory {
    /// The file `slot` runs: the first one checked as one of its models,
    /// else the first of its family the user put there.
    pub fn best_for(&self, slot: Slot) -> Option<&Found> {
        self.verified_for(slot).next().or_else(|| {
            self.found.iter().find(|f| {
                matches!(f.recognition, Recognition::Custom(family) if recognize::fits(family, slot))
            })
        })
    }

    /// The files checked as models of `slot`.
    pub fn verified_for(&self, slot: Slot) -> impl Iterator<Item = &Found> {
        self.found
            .iter()
            .filter(move |f| matches!(&f.recognition, Recognition::Verified(a) if a.slot == slot))
    }

    /// The files checked as the model of the author's list named `id`.
    pub fn verified_id<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a Found> {
        self.found
            .iter()
            .filter(move |f| matches!(&f.recognition, Recognition::Verified(a) if a.id == id))
    }

    /// The user's own files of `family`.
    pub fn custom_of(&self, family: Family) -> impl Iterator<Item = &Found> {
        self.found
            .iter()
            .filter(move |f| f.recognition == Recognition::Custom(family))
    }

    /// The files that look like models but cannot be used, and why.
    pub fn problems(&self) -> impl Iterator<Item = (&Path, &Reason)> {
        self.found.iter().filter_map(|f| match &f.recognition {
            Recognition::Unusable(reason) => Some((f.path.as_path(), reason)),
            _ => None,
        })
    }
}

/// Look through `roots` and say what each model file is.
pub fn scan(roots: &[PathBuf], known: &Known, cache: &mut HashCache) -> Inventory {
    let mut inventory = Inventory {
        roots: roots.to_vec(),
        ..Inventory::default()
    };
    let mut seen = std::collections::HashSet::new();
    let mut entries = 0usize;
    for root in roots {
        let mut files = Vec::new();
        walk(root, 0, &mut files, &mut entries, &mut inventory.cut_short);
        files.sort();
        for path in files {
            // A folder inside another root (a build's `models` above it) is
            // looked through once.
            let key = path.canonicalize().unwrap_or_else(|_| path.clone());
            if !seen.insert(key) {
                continue;
            }
            let recognition = recognize::recognize(&path, known, cache);
            if matches!(recognition, Recognition::NotAModel) {
                continue;
            }
            if recognition == Recognition::Companion {
                // Hashed now, while the scan is in the background, when it
                // could be a known model's: not later, when it is run.
                if recognize::companion_could_be_known(&path, known) {
                    let _ = cache.sha256(&path);
                }
                continue;
            }
            let size = recognize::stamp(&path).map_or(0, |(size, _)| size);
            inventory.found.push(Found {
                path,
                size,
                recognition,
            });
        }
    }
    inventory
}

fn walk(dir: &Path, depth: usize, files: &mut Vec<PathBuf>, entries: &mut usize, cut: &mut bool) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        *entries += 1;
        if *entries > MAX_ENTRIES {
            *cut = true;
            return;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Hidden folders (an install's staging, `.git`) and what is not
        // followed: links, which could loop.
        if name.starts_with('.') {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            walk(&entry.path(), depth + 1, files, entries, cut);
        } else if kind.is_file() {
            files.push(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::recognize::Accepted;
    use super::super::recognize::tests::{fake, temp};
    use super::*;

    #[test]
    fn models_are_found_however_deep_and_however_named() {
        let base = temp("scan");
        let (a, b) = (base.join("exe-models"), base.join("appdata-models"));
        // A pack unpacked one folder too deep, a model renamed, a .pth beside.
        let deep = a
            .join("iAi-models-co-ban")
            .join("models")
            .join("realesrgan");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("4x-UltraSharp.onnx"), fake(Family::RealEsrgan)).unwrap();
        std::fs::create_dir_all(a.join("gfpgan")).unwrap();
        std::fs::write(a.join("gfpgan").join("GFPGANv1.4.pth"), b"torch").unwrap();
        std::fs::write(a.join("README.txt"), b"giai nen vao day").unwrap();
        // A known model, by hash, in the old folder, in no subfolder.
        std::fs::create_dir_all(&b).unwrap();
        let face = fake(Family::FaceMesh);
        std::fs::write(b.join("mesh.onnx"), &face).unwrap();
        let sha = crate::ai::download::file_sha256_hex(&b.join("mesh.onnx")).unwrap();
        let known = Known {
            accepted: vec![Accepted {
                size: face.len() as u64,
                sha256: sha,
                ..Accepted::of(Slot::FaceMesh.model())
            }],
        };
        // A hidden staging folder is not looked in.
        std::fs::create_dir_all(b.join(".iai-install-1")).unwrap();
        std::fs::write(b.join(".iai-install-1").join("x.onnx"), fake(Family::Lama)).unwrap();

        let inventory = scan(&[a.clone(), b.clone()], &known, &mut HashCache::in_memory());

        assert_eq!(inventory.found.len(), 3, "{:#?}", inventory.found);
        let upscaler = inventory.best_for(Slot::RealesrganRrdbX2).unwrap();
        assert_eq!(upscaler.path, deep.join("4x-UltraSharp.onnx"));
        assert_eq!(
            upscaler.recognition,
            Recognition::Custom(Family::RealEsrgan)
        );
        let mesh = inventory.best_for(Slot::FaceMesh).unwrap();
        assert!(matches!(mesh.recognition, Recognition::Verified(_)));
        assert_eq!(inventory.verified_id("face-mesh").count(), 1);
        assert!(inventory.best_for(Slot::Lama).is_none());
        let problems: Vec<_> = inventory.problems().collect();
        assert_eq!(problems.len(), 1);
        assert!(problems[0].1.text().contains("chưa chuyển sang ONNX"));
        let _ = std::fs::remove_dir_all(base);
    }

    /// Opt-in: IAI_MODEL_SCAN_PROBE is a model folder of real models; prints
    /// what the scan makes of each file, and how long it took.
    #[test]
    #[ignore]
    fn probe_a_real_folder() {
        let Ok(dir) = std::env::var("IAI_MODEL_SCAN_PROBE") else {
            return;
        };
        let started = std::time::Instant::now();
        let list = super::super::list::ModelList::built_in();
        let inventory = scan(
            &[PathBuf::from(dir)],
            &list.known(),
            &mut HashCache::in_memory(),
        );
        for found in &inventory.found {
            println!(
                "{} ({} bytes): {:?}",
                found.path.display(),
                found.size,
                found.recognition
            );
        }
        println!("{} ms", started.elapsed().as_millis());
    }

    #[test]
    fn a_checked_model_comes_before_the_users_own_and_a_folder_is_read_once() {
        let base = temp("order");
        let root = base.join("models");
        std::fs::create_dir_all(root.join("gfpgan")).unwrap();
        std::fs::write(
            root.join("gfpgan").join("a-mine.onnx"),
            fake(Family::FaceRestore),
        )
        .unwrap();
        let mut verified = fake(Family::FaceRestore);
        verified.extend_from_slice(b"\x7a\x00"); // field 15, a varint: still a model
        std::fs::write(root.join("gfpgan").join("z-theirs.onnx"), &verified).unwrap();
        let sha = crate::ai::download::file_sha256_hex(&root.join("gfpgan").join("z-theirs.onnx"))
            .unwrap();
        let known = Known {
            accepted: vec![Accepted {
                size: verified.len() as u64,
                sha256: sha,
                ..Accepted::of(Slot::Gfpgan.model())
            }],
        };
        // The same folder twice, as a build's `models` above it can be.
        let inventory = scan(
            &[root.clone(), root.clone()],
            &known,
            &mut HashCache::in_memory(),
        );
        assert_eq!(inventory.found.len(), 2);
        assert_eq!(
            inventory.best_for(Slot::Gfpgan).map(|f| f.path.clone()),
            Some(root.join("gfpgan").join("z-theirs.onnx"))
        );
        let _ = std::fs::remove_dir_all(base);
    }
}

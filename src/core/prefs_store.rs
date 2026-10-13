//! The one door to `prefs.json`, the file every subsystem keeps its remembered
//! choices in (Preferences, theme, dialog options, presets).
//!
//! Each writer used to read the file, change its own keys and write it back by
//! itself, straight onto the file and ignoring errors; a file cut short then
//! reset every preference without a word. Here a change re-reads the file under
//! a lock, is written beside it and renamed into place, and the file as it was
//! before the change is kept as `prefs.json.bak` to fall back on.
//!
//! Tests neither read nor write the owner's file: [`read`] is empty and
//! [`update`] does nothing there.

use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub type Prefs = Map<String, Value>;

/// What a damaged file was replaced with, for the app to tell the user once.
static NOTICE: Mutex<Option<&'static str>> = Mutex::new(None);

/// One change at a time: a second writer re-reads what the first one wrote.
static WRITER: Mutex<()> = Mutex::new(());

pub fn prefs_path() -> PathBuf {
    let base = if let Ok(appdata) = std::env::var("APPDATA") {
        PathBuf::from(appdata).join("IAI")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local").join("share").join("iai")
    } else {
        PathBuf::from(".")
    };
    base.join("prefs.json")
}

fn backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
}

fn parse(bytes: &[u8]) -> Option<Prefs> {
    match serde_json::from_slice(bytes).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

/// What [`read_at`] found.
struct Loaded {
    prefs: Prefs,
    /// The file's bytes when it was sound: what a change backs up first.
    sound: Option<Vec<u8>>,
    /// Set when the file was damaged: what to tell the user.
    notice: Option<&'static str>,
    /// The file is there but could not be opened (another program has it).
    /// Its contents are unknown, so it must not be written over.
    unreadable: bool,
}

/// The file's bytes, `None` when there is no file. A file another program is
/// reading at this instant (a virus scanner, right after a write) is given a
/// moment before it counts as unreadable.
fn read_bytes(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    let mut tries = 0;
    loop {
        match std::fs::read(path) {
            Ok(bytes) => return Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) if tries == 5 => return Err(e),
            Err(_) => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }
}

/// The preferences in `path`. A file that is there but cannot be read as
/// preferences is set aside as `prefs.corrupt-<time>.json` and the backup takes
/// its place.
fn read_at(path: &Path) -> Loaded {
    let bytes = match read_bytes(path) {
        Ok(Some(bytes)) => bytes,
        missing_or_unreadable => {
            return Loaded {
                prefs: Prefs::new(),
                sound: None,
                notice: None,
                unreadable: missing_or_unreadable.is_err(),
            }
        }
    };
    if let Some(prefs) = parse(&bytes) {
        return Loaded {
            prefs,
            sound: Some(bytes),
            notice: None,
            unreadable: false,
        };
    }

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let set_aside = std::fs::rename(
        path,
        path.with_file_name(format!("prefs.corrupt-{stamp}.json")),
    )
    .is_ok();
    let backup = std::fs::read(backup_path(path))
        .ok()
        .filter(|bytes| parse(bytes).is_some());
    if let (true, Some(bytes)) = (set_aside, &backup) {
        let _ = crate::core::atomic_file::write_bytes_atomic(path, bytes);
    }
    Loaded {
        prefs: backup.as_deref().and_then(parse).unwrap_or_default(),
        notice: Some(if backup.is_some() {
            "File cài đặt bị hỏng, đã khôi phục bản gần nhất"
        } else {
            "File cài đặt bị hỏng và không có bản dự phòng: các cài đặt trở về mặc định"
        }),
        sound: backup.filter(|_| set_aside),
        unreadable: false,
    }
}

/// Apply `change` to the preferences in `path`. Also returns what [`read_at`]
/// had to say about the file it found.
fn update_at(
    path: &Path,
    change: impl FnOnce(&mut Prefs),
) -> (Option<&'static str>, Result<(), String>) {
    let _writer = WRITER.lock().unwrap_or_else(|e| e.into_inner());
    let Loaded {
        mut prefs,
        sound,
        notice,
        unreadable,
    } = read_at(path);
    if unreadable {
        // Writing now would replace every preference with this one change.
        return (
            notice,
            Err("prefs.json could not be read; left as it is".to_string()),
        );
    }
    let before = prefs.clone();
    change(&mut prefs);
    if prefs == before {
        return (notice, Ok(()));
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Not flushed: the file it copies stays whole until the rename below.
    if let Some(bytes) = sound {
        let _ = std::fs::write(backup_path(path), bytes);
    }
    let written = serde_json::to_string_pretty(&Value::Object(prefs))
        .map_err(|e| e.to_string())
        .and_then(|json| crate::core::atomic_file::write_bytes_atomic(path, json.as_bytes()));
    (notice, written)
}

fn keep_notice(notice: Option<&'static str>) {
    if notice.is_some() {
        *NOTICE.lock().unwrap_or_else(|e| e.into_inner()) = notice;
    }
}

/// Every preference, as the JSON object on disk. Empty when there is no file.
pub fn read() -> Prefs {
    if cfg!(test) {
        return Prefs::new();
    }
    let loaded = read_at(&prefs_path());
    keep_notice(loaded.notice);
    loaded.prefs
}

/// One preference, or `None` when it is absent or of another shape.
pub fn get<T: serde::de::DeserializeOwned>(key: &str) -> Option<T> {
    serde_json::from_value(read().remove(key)?).ok()
}

/// Change preferences, keeping every key `change` does not touch. Best effort:
/// a preference that cannot be written must not stop the work in hand.
pub fn update(change: impl FnOnce(&mut Prefs)) {
    if cfg!(test) {
        return;
    }
    keep_notice(update_at(&prefs_path(), change).0);
}

/// Remember one value.
pub fn set<T: serde::Serialize>(key: &str, value: &T) {
    if let Ok(value) = serde_json::to_value(value) {
        update(|prefs| {
            prefs.insert(key.to_string(), value);
        });
    }
}

/// What to tell the user if the file was found damaged, once.
pub fn take_notice() -> Option<&'static str> {
    NOTICE.lock().unwrap_or_else(|e| e.into_inner()).take()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder per test, removed when the test ends.
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "iai_prefs_{name}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn set_at(path: &Path, key: &str, value: Value) {
        update_at(path, |prefs| {
            prefs.insert(key.to_string(), value);
        })
        .1
        .unwrap();
    }

    #[test]
    fn two_writers_keep_each_others_keys() {
        let folder = Folder::new("merge");
        let path = folder.0.join("prefs.json");
        set_at(&path, "theme_mode", "Light".into());
        set_at(&path, "autosave_interval_secs", 120.into());

        let prefs = read_at(&path).prefs;
        assert_eq!(prefs["theme_mode"], "Light");
        assert_eq!(prefs["autosave_interval_secs"], 120);
    }

    #[test]
    fn no_file_reads_as_empty_and_leaves_nothing_behind() {
        let folder = Folder::new("none");
        let path = folder.0.join("prefs.json");
        assert!(read_at(&path).prefs.is_empty());
        update_at(&path, |_| {}).1.unwrap();
        assert!(
            folder.names().is_empty(),
            "nothing changed, nothing written"
        );
    }

    #[test]
    fn an_unchanged_value_is_not_written_again() {
        let folder = Folder::new("same");
        let path = folder.0.join("prefs.json");
        set_at(&path, "snap_default", true.into());
        let written = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        set_at(&path, "snap_default", true.into());
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            written
        );
    }

    #[test]
    fn a_damaged_file_is_set_aside_and_the_backup_takes_its_place() {
        let folder = Folder::new("damaged");
        let path = folder.0.join("prefs.json");
        set_at(&path, "theme_mode", "Light".into());
        // The second change makes the first one the backup.
        set_at(&path, "snap_default", true.into());
        // Cut short, as a power loss leaves a file written in place.
        std::fs::write(&path, b"{\"theme_mode\": \"Li").unwrap();

        let Loaded { prefs, notice, .. } = read_at(&path);

        assert_eq!(prefs["theme_mode"], "Light");
        assert!(
            !prefs.contains_key("snap_default"),
            "the change before last"
        );
        assert_eq!(
            notice,
            Some("File cài đặt bị hỏng, đã khôi phục bản gần nhất")
        );
        let names = folder.names();
        assert_eq!(names.len(), 3, "{names:?}");
        assert!(names[0].starts_with("prefs.corrupt-"), "{names:?}");
        assert_eq!(&names[1..], ["prefs.json", "prefs.json.bak"]);
        // Restored on disk too, so the next read finds a sound file.
        let again = read_at(&path);
        assert_eq!(again.prefs, prefs);
        assert_eq!(again.notice, None);
    }

    #[test]
    fn a_damaged_file_with_no_backup_falls_back_to_defaults_and_can_be_written() {
        let folder = Folder::new("nobackup");
        let path = folder.0.join("prefs.json");
        std::fs::write(&path, b"").unwrap();

        let (notice, written) = update_at(&path, |prefs| {
            prefs.insert("theme_mode".to_string(), "Dark".into());
        });
        written.unwrap();

        assert!(notice.is_some_and(|notice| notice.contains("mặc định")));
        let prefs = read_at(&path).prefs;
        assert_eq!(prefs.len(), 1);
        assert_eq!(prefs["theme_mode"], "Dark");
    }

    #[cfg(windows)]
    #[test]
    fn a_file_that_cannot_be_opened_is_neither_judged_nor_written_over() {
        use std::os::windows::fs::OpenOptionsExt;

        let folder = Folder::new("held");
        let path = folder.0.join("prefs.json");
        set_at(&path, "theme_mode", "Light".into());
        let before = std::fs::read(&path).unwrap();
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();

        let (notice, written) = update_at(&path, |prefs| {
            prefs.insert("snap_default".to_string(), true.into());
        });
        drop(held);

        assert!(written.is_err(), "the change is given up, not the file");
        assert_eq!(notice, None, "unreadable is not damaged");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(folder.names(), ["prefs.json"]);
    }

    #[test]
    fn a_file_that_is_not_an_object_counts_as_damaged() {
        let folder = Folder::new("array");
        let path = folder.0.join("prefs.json");
        std::fs::write(&path, b"[1, 2, 3]").unwrap();
        let loaded = read_at(&path);
        assert!(loaded.prefs.is_empty());
        assert!(loaded.notice.is_some());
        assert!(!path.exists());
    }
}

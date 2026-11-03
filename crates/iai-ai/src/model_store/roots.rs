//! Where models are looked for, and where a model fetched or installed is
//! put. Every model of the crate goes through here.
//!
//! Looked for, in this order:
//! 1. `<folder of iai.exe>\models`: the portable build's own (link 2 is
//!    unpacked there);
//! 2. a build being tried out from `target\`: `models` in the folders above
//!    it, so it finds the author's models in the workspace;
//! 3. the folder the user chose (a small drive C keeps them on D), saved as
//!    `model_folder` in `ai.json`;
//! 4. `%APPDATA%\IAI\models` (`~/.local/share/iai/models` elsewhere), where
//!    every model went before, so a machine in use has nothing to redo;
//! 5. `models` in the working folder.
//!
//! A model fetched or installed goes to the user's folder when there is one,
//! else beside the exe when that is a portable build and can be written,
//! else to `%APPDATA%\IAI\models`.

use std::path::{Path, PathBuf};

/// How many folders above a build in `target\` are looked in for `models`.
const BUILD_ANCESTORS: usize = 4;
/// The key of `ai.json` that holds the folder the user chose.
const FOLDER_KEY: &str = "model_folder";

/// What the model folders are worked out from: this machine's, or a test's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Places {
    /// The folder `iai.exe` is in.
    pub exe_dir: Option<PathBuf>,
    /// The folder the user chose for models.
    pub user_folder: Option<PathBuf>,
    /// iAi's own folder: `%APPDATA%\IAI`.
    pub app_data: PathBuf,
    /// The working folder.
    pub cwd: Option<PathBuf>,
}

impl Places {
    /// This machine's, as the app runs now.
    pub fn of_this_machine() -> Self {
        let app_data = iai_platform::app_data::dir();
        Self {
            exe_dir: std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(Path::to_path_buf)),
            user_folder: user_folder_in(&app_data),
            app_data,
            cwd: std::env::current_dir().ok(),
        }
    }

    /// The model folders, most authoritative first, each once.
    pub fn roots(&self) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        if let Some(exe_dir) = &self.exe_dir {
            roots.push(exe_dir.join("models"));
            if is_build_dir(exe_dir) {
                roots.extend(
                    exe_dir
                        .ancestors()
                        .skip(1)
                        .take(BUILD_ANCESTORS)
                        .map(|a| a.join("models")),
                );
            }
        }
        roots.extend(self.user_folder.clone());
        roots.push(self.app_data_models());
        if let Some(cwd) = &self.cwd {
            roots.push(cwd.join("models"));
        }
        let mut seen = std::collections::HashSet::new();
        roots.retain(|root| seen.insert(root.clone()));
        roots
    }

    /// `%APPDATA%\IAI\models`.
    pub fn app_data_models(&self) -> PathBuf {
        self.app_data.join("models")
    }

    /// Where a model fetched or installed is put: the user's folder, else
    /// beside a portable exe, else `%APPDATA%\IAI\models`; the first of these
    /// that can be written. The folder is made if it is not there.
    pub fn main_folder(&self) -> PathBuf {
        let portable = self
            .exe_dir
            .as_ref()
            .filter(|dir| !is_build_dir(dir))
            .map(|dir| dir.join("models"));
        self.user_folder
            .iter()
            .cloned()
            .chain(portable)
            .find(|dir| writable(dir))
            .unwrap_or_else(|| self.app_data_models())
    }
}

/// A build being tried out: the exe is under a folder named `target`.
pub fn is_build_dir(dir: &Path) -> bool {
    dir.components()
        .any(|c| c.as_os_str().eq_ignore_ascii_case("target"))
}

/// Whether a file can be made in `dir` (made first if it is not there).
pub fn writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(".iai-write-test-{}", std::process::id()));
    let made = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .is_ok();
    if made {
        let _ = std::fs::remove_file(&probe);
    }
    made
}

/// `ai.json` in iAi's folder: the AI settings, where the model folder the
/// user chose is kept beside the rest (`ai/settings.rs` keeps what it does
/// not know of when it saves).
fn settings_file(app_data: &Path) -> PathBuf {
    app_data.join("ai.json")
}

fn user_folder_in(app_data: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(settings_file(app_data)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let folder = value.get(FOLDER_KEY)?.as_str()?.trim();
    (!folder.is_empty()).then(|| PathBuf::from(folder))
}

/// The folder the user chose for models, if any.
pub fn user_folder() -> Option<PathBuf> {
    user_folder_in(&iai_platform::app_data::dir())
}

/// Keep `folder` as the user's model folder (`None`: none any more). Every
/// other setting of `ai.json` is kept as it is.
pub fn set_user_folder(folder: Option<&Path>) -> Result<(), String> {
    set_user_folder_in(&iai_platform::app_data::dir(), folder)?;
    super::forget_found();
    Ok(())
}

fn set_user_folder_in(app_data: &Path, folder: Option<&Path>) -> Result<(), String> {
    let path = settings_file(app_data);
    let mut value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    if let Some(map) = value.as_object_mut() {
        match folder {
            Some(folder) => {
                if !writable(folder) {
                    return Err(format!(
                        "Không ghi được vào thư mục {} — chọn thư mục khác",
                        folder.display()
                    ));
                }
                map.insert(
                    FOLDER_KEY.to_string(),
                    serde_json::Value::String(folder.to_string_lossy().into_owned()),
                );
            }
            None => {
                map.remove(FOLDER_KEY);
            }
        }
    }
    std::fs::create_dir_all(app_data).map_err(|e| format!("create dir: {e}"))?;
    let json = serde_json::to_string_pretty(&value).map_err(|e| format!("serialize: {e}"))?;
    iai_platform::atomic_file::write_bytes_atomic(&path, json.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "iai_model_roots_{name}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_portable_build_looks_beside_itself_first_and_puts_models_there() {
        let base = temp("portable");
        let places = Places {
            exe_dir: Some(base.join("iAi")),
            user_folder: None,
            app_data: base.join("AppData").join("IAI"),
            cwd: Some(base.join("work")),
        };
        assert_eq!(
            places.roots(),
            [
                base.join("iAi").join("models"),
                base.join("AppData").join("IAI").join("models"),
                base.join("work").join("models"),
            ]
        );
        // Not under `target`: the folders above it are not looked in.
        assert_eq!(places.main_folder(), base.join("iAi").join("models"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_build_in_target_also_looks_above_it_and_puts_models_in_app_data() {
        let base = temp("build");
        let exe_dir = base.join("iai-pro").join("target").join("release");
        let places = Places {
            exe_dir: Some(exe_dir.clone()),
            user_folder: None,
            app_data: base.join("AppData").join("IAI"),
            cwd: None,
        };
        let roots = places.roots();
        assert_eq!(roots[0], exe_dir.join("models"));
        assert_eq!(roots[1], base.join("iai-pro").join("target").join("models"));
        assert_eq!(roots[2], base.join("iai-pro").join("models"));
        assert_eq!(roots.len(), 1 + BUILD_ANCESTORS + 1);
        assert_eq!(
            roots.last(),
            Some(&base.join("AppData").join("IAI").join("models"))
        );
        // `cargo clean` would take what is fetched with it.
        assert_eq!(places.main_folder(), places.app_data_models());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn the_users_folder_comes_after_the_exe_and_takes_what_is_fetched() {
        let base = temp("user");
        let places = Places {
            exe_dir: Some(base.join("iAi")),
            user_folder: Some(base.join("D").join("models")),
            app_data: base.join("AppData").join("IAI"),
            cwd: None,
        };
        let roots = places.roots();
        assert_eq!(roots[0], base.join("iAi").join("models"));
        assert_eq!(roots[1], base.join("D").join("models"));
        assert_eq!(places.main_folder(), base.join("D").join("models"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_folder_that_cannot_be_written_is_passed_over() {
        let base = temp("readonly");
        // A file where the folder should be: nothing can be made under it.
        std::fs::write(base.join("iAi"), b"a file, not a folder").unwrap();
        let places = Places {
            exe_dir: Some(base.join("iAi")),
            user_folder: Some(base.join("iAi").join("mine")),
            app_data: base.join("AppData").join("IAI"),
            cwd: None,
        };
        assert_eq!(places.main_folder(), places.app_data_models());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn the_chosen_folder_is_kept_in_ai_json_beside_the_other_settings() {
        let base = temp("settings");
        let app_data = base.join("IAI");
        std::fs::create_dir_all(&app_data).unwrap();
        std::fs::write(
            app_data.join("ai.json"),
            r#"{"api_key":"k","history":["a"]}"#,
        )
        .unwrap();
        let chosen = base.join("D-models");
        set_user_folder_in(&app_data, Some(&chosen)).unwrap();
        assert_eq!(user_folder_in(&app_data), Some(chosen));
        let kept: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(app_data.join("ai.json")).unwrap())
                .unwrap();
        assert_eq!(kept["api_key"], "k");
        assert_eq!(kept["history"][0], "a");
        set_user_folder_in(&app_data, None).unwrap();
        assert_eq!(user_folder_in(&app_data), None);
        let _ = std::fs::remove_dir_all(base);
    }
}

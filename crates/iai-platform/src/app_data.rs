//! The folder iAi keeps its files in for this user.

use std::path::PathBuf;

/// `%APPDATA%\IAI` on Windows; `~/.local/share/iai` elsewhere; the current
/// folder when neither is known. `prefs.json`, `pro.json` and the rest live
/// here.
pub fn dir() -> PathBuf {
    if let Ok(appdata) = std::env::var("APPDATA") {
        PathBuf::from(appdata).join("IAI")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local").join("share").join("iai")
    } else {
        PathBuf::from(".")
    }
}

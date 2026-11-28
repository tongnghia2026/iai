//! Showing a folder, or a file in its folder, in the system's file manager:
//! Explorer on Windows, Finder on the Mac, the desktop's own elsewhere.
//! Nothing is waited for and a failure is not reported: there is nothing more
//! the app could do about it.

use std::path::Path;
use std::process::Command;

/// Open `dir` in the file manager.
pub fn open_folder(dir: &Path) {
    let program = if cfg!(windows) {
        "explorer.exe"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = Command::new(program).arg(dir).spawn();
}

/// Show `path` selected in its folder; where the file manager cannot be
/// asked to select (Linux), open the folder it is in.
pub fn reveal(path: &Path) {
    if cfg!(windows) {
        let _ = Command::new("explorer.exe")
            .arg("/select,")
            .arg(path)
            .spawn();
    } else if cfg!(target_os = "macos") {
        let _ = Command::new("open").arg("-R").arg(path).spawn();
    } else if let Some(dir) = path.parent() {
        open_folder(dir);
    }
}

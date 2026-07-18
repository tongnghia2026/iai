// Crash reporting — usable before the GPU/window exist and after they are gone.
//
// Lives at the library root (not in `app`) because both the thin binary entry
// point and deep subsystems (GPU device-lost recovery) report through it, and
// it must not depend on winit/egui/wgpu being initialised.

use std::path::PathBuf;

/// Log path: `%APPDATA%\IAI\iai_crash.log`, falling back to the temp directory.
pub fn crash_log_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("IAI").join("iai_crash.log")
}

/// Append a timestamped line to the crash log. Never panics — best effort only.
pub fn log_crash(text: &str) {
    let path = crash_log_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{ts}] {text}");
    }
}

/// Native error dialog (no winit/egui dependency — works even before the GPU/window is up).
///
/// Run MessageDialog on a WORKER THREAD, then join: if a panic happens INSIDE a winit handler
/// (main thread in the event loop), opening a modal dialog on the main thread freezes
/// (winit buffers/re-posts WM_PAINT while the runner is suspended) → the app hangs instead of showing it.
/// A worker thread avoids that; join so the dialog finishes before continuing (then abort).
pub fn report_fatal(text: &str) {
    let log = crash_log_path();
    let description = format!(
        "{text}\n\nLog saved to:\n{}\n\nThis is usually caused by an outdated graphics \
         driver — please update your GPU driver and reopen iAi.",
        log.display()
    );
    let _ = std::thread::spawn(move || {
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("iAi — Crash")
            .set_description(description)
            .show();
    })
    .join();
}

/// Last resort: write `iai_crash.log` + show a native dialog instead of aborting silently.
pub fn install_panic_handler() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "unknown".to_string());
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string());
        let full = format!("iAi panicked at {location}\n{msg}");
        log_crash(&full);
        report_fatal(&full);
        default_hook(info);
    }));
}

//! Build script.
//!
//! On Windows, embed the application icon into the compiled `iai.exe` as a
//! resource. Windows uses that embedded icon as the program's identity: it is
//! what Explorer draws for the file and what the taskbar shows when the app is
//! launched. Without it the OS falls back to the generic executable glyph.
//!
//! The runtime window icon (set via winit's `with_window_icon`) covers the
//! title bar and Alt-Tab, but the taskbar/launch identity comes from this
//! embedded resource — so both are needed for the logo to appear everywhere.
//!
//! No-op on non-Windows hosts (the CI Linux/macOS builds), where the resource
//! compiler and `winresource` crate are absent.
//!
//! On every host, tell the program which build it is (`IAI_BUILD_COMMIT`,
//! `IAI_BUILD_TIME`): the app's self-update compares that with the build
//! published last (see `src/update.rs`).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    stamp_build();
    #[cfg(windows)]
    embed_windows_icon();
}

/// The last commit that touched the code, and when it was made (seconds since
/// 1970). A commit of documents or of CI workflows alone leaves the program
/// what it was, so it is no newer build (`release.yml` skips the same paths).
/// "unknown" and 0 where there is no git to ask.
fn stamp_build() {
    let git = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git").args(args).output().ok()?;
        let text = String::from_utf8(out.stdout).ok()?;
        (out.status.success() && !text.trim().is_empty()).then(|| text.trim().to_owned())
    };
    let stamp = git(&[
        "log",
        "-1",
        "--format=%H %ct",
        "--",
        ".",
        ":(exclude)docs",
        ":(exclude)*.md",
        ":(exclude).github",
    ]);
    let (commit, time) = stamp
        .as_deref()
        .and_then(|s| s.split_once(' '))
        .filter(|(_, time)| time.parse::<u64>().is_ok())
        .unwrap_or(("unknown", "0"));
    println!("cargo:rustc-env=IAI_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=IAI_BUILD_TIME={time}");
    // Every commit, checkout and reset writes to HEAD's log.
    for path in ["HEAD", "logs/HEAD"] {
        if let Some(path) = git(&["rev-parse", "--git-path", path]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}

#[cfg(windows)]
fn embed_windows_icon() {
    println!("cargo:rerun-if-changed=assets/iai.ico");

    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/iai.ico");
    if let Err(e) = res.compile() {
        // Keep the build alive if the resource compiler is unavailable: the app
        // still runs, just without the embedded exe icon. Surface it as a
        // warning so a missing icon is diagnosable rather than silent.
        println!("cargo:warning=không nhúng được icon Windows vào iai.exe: {e}");
    }
}

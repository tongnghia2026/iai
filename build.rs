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

fn main() {
    #[cfg(windows)]
    embed_windows_icon();
}

#[cfg(windows)]
fn embed_windows_icon() {
    // Only re-run when the icon itself changes, not on every source edit.
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

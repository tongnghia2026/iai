//! Checks of the iAi workspace, run as `cargo xtask <command>`.
//!
//! - `layers`: every crate depends only on crates of its own layer or below,
//!   and nothing under the UI layer pulls in a window or GUI library.
//! - `lints [--update] [cargo args…]`: clippy over the workspace, failing on
//!   any warning, except the crash lints each crate is allowed a number of in
//!   `xtask/lint-budget.txt`; those may go down, never up.
//! - `panic-hunt`: every command called with params made from its JSON Schema
//!   to probe its edges (`iai_engine::hunt`); fails on a panic or a hang.
#![forbid(unsafe_code)]

mod layers;
mod lints;

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: cargo xtask layers
       cargo xtask lints [--update] [arguments for cargo clippy, e.g. --features iai/canvas-editor-webview]
       cargo xtask panic-hunt";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let result = match args.next().as_deref() {
        Some("layers") => layers::run(),
        Some("lints") => lints::run(args.collect()),
        Some("panic-hunt") => panic_hunt(),
        _ => Err(USAGE.to_owned()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// The panic hunt is a test of iai-engine (the free core's commands) and of
/// the app (the Pro pack's): run both.
fn panic_hunt() -> Result<(), String> {
    for args in [
        &["test", "-p", "iai-engine", "--lib", "panic_hunt"][..],
        &[
            "test",
            "-p",
            "iai",
            "--lib",
            "--features",
            "canvas-editor-webview",
            "panic_hunt",
        ][..],
    ] {
        let status = cargo()
            .args(args)
            .status()
            .map_err(|e| format!("cargo: {e}"))?;
        if !status.success() {
            return Err(format!("panic-hunt: `cargo {}` failed", args.join(" ")));
        }
    }
    Ok(())
}

/// The workspace root: the folder above this crate's own.
fn workspace_root() -> Result<PathBuf, String> {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(PathBuf::from)
        .ok_or_else(|| "xtask is not inside a workspace".to_owned())
}

/// `cargo` as cargo itself was started, so the toolchain is the same.
fn cargo() -> std::process::Command {
    let exe = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = std::process::Command::new(exe);
    if let Ok(root) = workspace_root() {
        cmd.current_dir(root);
    }
    cmd
}

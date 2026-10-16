//! The layer order of the workspace's crates.
//!
//! A crate may depend on crates of its own layer or a lower one, never a
//! higher one, and only the UI layer and the apps may pull in a window or GUI
//! library. The table holds the crates planned in
//! docs/planning/KIEN_TRUC_AGENT_NATIVE_2026-10-14.md (section 3.2) as well
//! as the ones already here; a new crate goes into it before it builds.

use serde_json::Value;

/// L0 (plain data and maths) up to the apps. `iai-canvas` (the canvas, its
/// operations and the open document) sits above the AI, below the file
/// formats and the GPU, which read and draw it (plan section 9, Phase 1).
const LAYERS: &[(&str, u8)] = &[
    ("iai-geom", 0),
    ("iai-color", 0),
    ("iai-raster", 0),
    ("iai-platform", 0),
    ("iai-raw-api", 0),
    ("iai-raw", 0),
    ("iai-doc", 1),
    ("iai-history", 2),
    ("iai-vector", 3),
    ("iai-text", 3),
    ("iai-develop", 3),
    ("iai-imaging", 3),
    ("iai-layout", 3),
    ("iai-ai", 4),
    ("iai-canvas", 5),
    ("iai-render-cpu", 6),
    ("iai-gpu", 6),
    ("iai-formats", 6),
    ("iai-diag", 6),
    ("iai-engine", 7),
    ("iai-tools", 7),
    ("iai-automation", 8),
    ("iai-ui", 8),
    ("iai-pro", 8),
    ("iai", 9),
    ("iai-cli", 9),
];

/// From this layer up a crate may draw windows and widgets.
const UI_LAYER: u8 = 8;

/// Libraries that open windows or draw widgets.
const GUI_LIBRARIES: &[&str] = &[
    "egui",
    "egui-wgpu",
    "egui-winit",
    "egui-phosphor",
    "winit",
    "rfd",
    "wry",
];

/// Tools that build nothing that ships.
const OUTSIDE: &[&str] = &["xtask"];

fn layer_of(name: &str) -> Option<u8> {
    LAYERS.iter().find(|(n, _)| *n == name).map(|&(_, l)| l)
}

pub fn run() -> Result<(), String> {
    let out = crate::cargo()
        .args(["metadata", "--format-version", "1", "--no-deps", "--locked"])
        .output()
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo metadata failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let meta: Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata: {e}"))?;
    let packages = meta["packages"]
        .as_array()
        .ok_or("cargo metadata: no package list")?;

    let (checked, problems) = check(packages);
    if problems.is_empty() {
        println!("layers: {checked} crates, every dependency points down");
        Ok(())
    } else {
        Err(format!("layers:\n  {}", problems.join("\n  ")))
    }
}

/// The crates checked, and what is wrong with them.
fn check(packages: &[Value]) -> (usize, Vec<String>) {
    let mut problems = Vec::new();
    let mut checked = 0;
    for pkg in packages {
        let name = pkg["name"].as_str().unwrap_or_default();
        if OUTSIDE.contains(&name) {
            continue;
        }
        let Some(layer) = layer_of(name) else {
            problems.push(format!(
                "{name}: not in the layer table (xtask/src/layers.rs)"
            ));
            continue;
        };
        checked += 1;
        for dep in pkg["dependencies"].as_array().into_iter().flatten() {
            // Tests may reach anywhere; what ships may not.
            if dep["kind"].as_str() == Some("dev") {
                continue;
            }
            let dep_name = dep["name"].as_str().unwrap_or_default();
            if dep["path"].is_string() {
                match layer_of(dep_name) {
                    Some(dep_layer) if dep_layer > layer => problems.push(format!(
                        "{name} (L{layer}) depends on {dep_name} (L{dep_layer}), a higher layer"
                    )),
                    Some(_) => {}
                    None => problems.push(format!(
                        "{name} depends on {dep_name}, which is not in the layer table"
                    )),
                }
            } else if layer < UI_LAYER && GUI_LIBRARIES.contains(&dep_name) {
                problems.push(format!(
                    "{name} (L{layer}) depends on {dep_name}: only L{UI_LAYER} and up may draw"
                ));
            }
        }
    }
    (checked, problems)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pkg(name: &str, deps: Value) -> Value {
        json!({"name": name, "dependencies": deps})
    }

    #[test]
    fn dependencies_that_point_down_or_across_pass() {
        let packages = [
            pkg("iai-raw-api", json!([])),
            pkg(
                "iai-raw",
                json!([{"name": "iai-raw-api", "kind": null, "path": "C:/x/crates/iai-raw-api"}]),
            ),
            pkg(
                "iai",
                json!([
                    {"name": "iai-raw", "kind": null, "path": "C:/x/crates/iai-raw"},
                    {"name": "egui", "kind": null},
                ]),
            ),
            pkg(
                "xtask",
                json!([{"name": "iai", "kind": null, "path": "C:/x"}]),
            ),
        ];
        assert_eq!(check(&packages), (3, vec![]));
    }

    #[test]
    fn an_upward_dependency_a_gui_library_low_down_and_an_unknown_crate_fail() {
        let packages = [
            pkg(
                "iai-doc",
                json!([
                    {"name": "iai-engine", "kind": null, "path": "C:/x/crates/iai-engine"},
                    {"name": "winit", "kind": null},
                    // A test may use anything.
                    {"name": "iai-ui", "kind": "dev", "path": "C:/x/crates/iai-ui"},
                ]),
            ),
            pkg("iai-mystery", json!([])),
        ];
        let (checked, problems) = check(&packages);
        assert_eq!(checked, 1);
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems[0].contains("iai-engine (L7), a higher layer"));
        assert!(problems[1].contains("winit"));
        assert!(problems[2].starts_with("iai-mystery: not in the layer table"));
    }
}

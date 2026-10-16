//! The core commands, driven as an agent drives them: JSON in, JSON out,
//! no window.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};

use crate::*;

struct Setup {
    engine: Engine,
    ws: HeadlessWorkspace,
}

impl Setup {
    fn new() -> Self {
        Self {
            engine: Engine::new(super::core_registry(), Source::Test, Policy::all()),
            ws: HeadlessWorkspace::new(),
        }
    }

    /// Run a command to the end; its result.
    fn run(&mut self, command: &str, args: Value) -> Value {
        match self.try_run(command, args) {
            Ok(v) => v,
            Err(e) => panic!("{command} failed: {e:?}"),
        }
    }

    fn try_run(&mut self, command: &str, args: Value) -> Result<Value, CmdError> {
        let mut s = self.engine.session(&mut self.ws);
        match s.execute(CommandCall::new(command, args))? {
            Reply::Done { result, .. } => Ok(result),
            Reply::Started { job } => match s.wait(job, Duration::from_secs(60)) {
                Some(JobState::Done { result, .. }) => Ok(result),
                Some(JobState::Failed { error }) => Err(error),
                other => panic!("{command}: {other:?}"),
            },
        }
    }

    fn inspect(&mut self) -> Value {
        self.run("doc.inspect", json!({}))
    }

    fn mean(&mut self, rect: Value) -> String {
        self.run("analyze.region", json!({"rect": rect}))["mean"]
            .as_str()
            .unwrap()
            .to_string()
    }
}

/// A folder of its own under the system temp folder, removed after.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("iai-engine-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self, file: &str) -> String {
        self.0.join(file).display().to_string()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_core_registry_holds_every_command_once_with_a_schema() {
    let r = super::core_registry();
    assert!(r.duplicates().is_empty(), "{:?}", r.duplicates());
    assert!(r.len() >= 30, "{}", r.len());
    for c in r.list() {
        assert!(c.id.contains('.'), "{}", c.id);
        assert_eq!(c.params_schema["type"], json!("object"), "{}", c.id);
        assert!(!c.doc.is_empty() && !c.label_vi.is_empty(), "{}", c.id);
    }
}

#[test]
fn a_new_document_is_opened_shown_and_described() {
    let mut t = Setup::new();
    let made = t.run(
        "file.new",
        json!({"widthPx": 600, "heightPx": 300, "dpi": 300, "background": "#0090FF", "title": "Thẻ"}),
    );
    assert_eq!(made["widthPx"], json!(600));
    let doc = made["doc"].as_u64().unwrap();
    let list = t.run("doc.list", json!({}));
    assert_eq!(list["shown"], json!(doc));
    assert_eq!(list["docs"][0]["title"], json!("Thẻ"));
    let info = t.inspect();
    assert_eq!(info["dpi"], json!(300.0));
    // 600 px at 300 dpi is 2 inches.
    assert!((info["widthMm"].as_f64().unwrap() - 50.8).abs() < 0.01);
    assert_eq!(info["layers"].as_array().unwrap().len(), 1);
    assert_eq!(info["modified"], json!(false));
    assert_eq!(
        t.mean(json!({"x": 0, "y": 0, "w": 600, "h": 300})),
        "#0090FF"
    );
}

#[test]
fn layers_are_added_changed_moved_and_deleted_by_id() {
    let mut t = Setup::new();
    t.run("file.new", json!({"widthPx": 64, "heightPx": 64}));
    let red = t.run(
        "layer.new",
        json!({"kind": "fill", "fill": "#FF0000", "name": "Đỏ"}),
    );
    let red_id = red["layer"].as_u64().unwrap();
    let group = t.run("layer.new", json!({"kind": "group", "name": "Nhóm"}));
    let group_id = group["layer"].as_u64().unwrap();
    assert_eq!(t.mean(json!({"x": 0, "y": 0, "w": 8, "h": 8})), "#FF0000");

    t.run(
        "layer.set",
        json!({"layer": red_id, "opacity": 0.5, "blend": "multiply", "name": "Đỏ mờ"}),
    );
    let info = t.inspect();
    let red_info = info["layers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["id"] == json!(red_id))
        .unwrap()
        .clone();
    assert_eq!(red_info["opacity"], json!(0.5));
    assert_eq!(red_info["blend"], json!("multiply"));
    assert_eq!(red_info["name"], json!("Đỏ mờ"));

    // Top first: the group is above the red layer; move red to the top.
    assert_eq!(info["layers"][0]["id"], json!(group_id));
    t.run("layer.move", json!({"layer": red_id, "to": "top"}));
    assert_eq!(t.inspect()["layers"][0]["id"], json!(red_id));
    let e = t
        .try_run("layer.move", json!({"layer": red_id, "to": "up"}))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotEnabled);

    t.run("layer.delete", json!({"layer": red_id}));
    assert_eq!(t.inspect()["layers"].as_array().unwrap().len(), 2);
    assert_eq!(t.mean(json!({"x": 0, "y": 0, "w": 8, "h": 8})), "#FFFFFF");
    // Each step is one undo step.
    t.run("edit.undo", json!({}));
    assert_eq!(t.inspect()["layers"].as_array().unwrap().len(), 3);
    let e = t
        .try_run("layer.delete", json!({"layer": 999}))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
}

#[test]
fn the_last_layer_is_kept() {
    let mut t = Setup::new();
    t.run("file.new", json!({"widthPx": 16, "heightPx": 16}));
    let id = t.inspect()["layers"][0]["id"].as_u64().unwrap();
    let e = t.try_run("layer.delete", json!({"layer": id})).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotEnabled);
}

#[test]
fn a_selection_is_made_combined_inverted_and_cleared() {
    let mut t = Setup::new();
    t.run("file.new", json!({"widthPx": 100, "heightPx": 80}));
    let sel = t.run(
        "select.rect",
        json!({"rect": {"x": 10, "y": 20, "w": 30, "h": 40}}),
    );
    assert_eq!(sel["bounds"], json!({"x": 10, "y": 20, "w": 30, "h": 40}));
    let sel = t.run(
        "select.rect",
        json!({"rect": {"x": 50, "y": 20, "w": 10, "h": 10}, "mode": "add"}),
    );
    assert_eq!(sel["bounds"], json!({"x": 10, "y": 20, "w": 50, "h": 40}));
    let sel = t.run("select.all", json!({}));
    assert_eq!(sel["bounds"], json!({"x": 0, "y": 0, "w": 100, "h": 80}));
    let sel = t.run("select.invert", json!({}));
    assert_eq!(sel["active"], json!(false));
    t.run(
        "select.rect",
        json!({"rect": {"x": -50, "y": -50, "w": 60, "h": 60}}),
    );
    assert_eq!(
        t.inspect()["selection"]["bounds"],
        json!({"x": 0, "y": 0, "w": 10, "h": 10})
    );
    let sel = t.run("select.none", json!({}));
    assert_eq!(sel["active"], json!(false));
    let e = t
        .try_run(
            "select.rect",
            json!({"rect": {"x": 500, "y": 0, "w": 5, "h": 5}}),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
}

#[test]
fn the_page_is_cropped_resized_rotated_and_flipped() {
    let mut t = Setup::new();
    t.run(
        "file.new",
        json!({"widthPx": 200, "heightPx": 100, "dpi": 100}),
    );
    // A red square at the left; flipping puts it on the right.
    t.run(
        "vector.add_shape",
        json!({"shape": "rectangle", "rect": {"x": 0, "y": 0, "w": 50, "h": 100}, "fill": "#FF0000"}),
    );
    assert_eq!(
        t.mean(json!({"x": 10, "y": 10, "w": 20, "h": 20})),
        "#FF0000"
    );
    t.run("image.flip", json!({"axis": "horizontal"}));
    assert_eq!(
        t.mean(json!({"x": 10, "y": 10, "w": 20, "h": 20})),
        "#FFFFFF"
    );
    assert_eq!(
        t.mean(json!({"x": 170, "y": 10, "w": 20, "h": 20})),
        "#FF0000"
    );

    let r = t.run("image.rotate", json!({"angle": "90"}));
    assert_eq!(
        (r["widthPx"].clone(), r["heightPx"].clone()),
        (json!(100), json!(200))
    );
    let r = t.run("image.rotate", json!({"angle": "180"}));
    assert_eq!(r["widthPx"], json!(100));
    let r = t.run("image.rotate", json!({"angle": "-90"}));
    assert_eq!(r["widthPx"], json!(200));
    // Back where it was, after 90 + 180 + -90 = a half turn: the square is left.
    assert_eq!(
        t.mean(json!({"x": 10, "y": 10, "w": 20, "h": 20})),
        "#FF0000"
    );

    let r = t.run("image.resize", json!({"widthPx": 100}));
    assert_eq!(
        (r["widthPx"].clone(), r["heightPx"].clone()),
        (json!(100), json!(50))
    );
    let r = t.run("image.resize", json!({"dpi": 300}));
    assert_eq!(
        (r["widthPx"].clone(), r["dpi"].clone()),
        (json!(100), json!(300.0))
    );
    t.run("edit.undo", json!({}));
    assert_eq!(t.inspect()["dpi"], json!(100.0));

    // Inside the red block, clear of the edge the resample softened.
    let r = t.run(
        "image.crop",
        json!({"rect": {"x": 0, "y": 0, "w": 20, "h": 50}}),
    );
    assert_eq!(
        (r["widthPx"].clone(), r["heightPx"].clone()),
        (json!(20), json!(50))
    );
    assert_eq!(t.mean(json!({"x": 2, "y": 5, "w": 16, "h": 40})), "#FF0000");
    t.run(
        "select.rect",
        json!({"rect": {"x": 5, "y": 5, "w": 10, "h": 20}}),
    );
    let r = t.run("image.crop", json!({}));
    assert_eq!(
        (r["widthPx"].clone(), r["heightPx"].clone()),
        (json!(10), json!(20))
    );
    assert_eq!(t.inspect()["selection"]["active"], json!(false));
    let e = t.try_run("image.crop", json!({})).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotEnabled);
}

#[test]
fn a_dry_run_crop_reports_the_size_and_changes_nothing() {
    let mut t = Setup::new();
    t.run("file.new", json!({"widthPx": 90, "heightPx": 60}));
    let mut call = CommandCall::new(
        "image.crop",
        json!({"rect": {"x": 0, "y": 0, "w": 30, "h": 30}}),
    );
    call.dry_run = true;
    let reply = t.engine.session(&mut t.ws).execute(call).unwrap();
    assert_eq!(reply.result().unwrap()["widthPx"], json!(30));
    let info = t.inspect();
    assert_eq!(
        (info["widthPx"].clone(), info["canUndo"].clone()),
        (json!(90), json!(false))
    );
}

#[test]
fn pages_are_added_and_shown() {
    let mut t = Setup::new();
    t.run("file.new", json!({"widthPx": 40, "heightPx": 30}));
    let p = t.run("page.add", json!({}));
    assert_eq!(
        (p["page"].clone(), p["pages"].clone()),
        (json!(1), json!(2))
    );
    t.run("layer.new", json!({"kind": "fill", "fill": "#00FF00"}));
    assert_eq!(t.mean(json!({"x": 0, "y": 0, "w": 40, "h": 30})), "#00FF00");
    t.run("page.select", json!({"page": 0}));
    assert_eq!(t.mean(json!({"x": 0, "y": 0, "w": 40, "h": 30})), "#FFFFFF");
    let info = t.inspect();
    assert_eq!(info["page"], json!(0));
    assert_eq!(info["pageList"].as_array().unwrap().len(), 2);
    let e = t.try_run("page.select", json!({"page": 2})).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
}

#[test]
fn shapes_paths_and_text_become_editable_layers() {
    let mut t = Setup::new();
    t.run("file.new", json!({"widthPx": 300, "heightPx": 200}));
    let shape = t.run(
        "vector.add_shape",
        json!({"shape": "ellipse", "rect": {"x": 100, "y": 50, "w": 100, "h": 100},
               "fill": "#00FF00", "stroke": "#000000", "strokeWidth": 4}),
    );
    assert_eq!(
        t.mean(json!({"x": 140, "y": 90, "w": 20, "h": 20})),
        "#00FF00"
    );
    let path = t.run(
        "vector.add_path",
        json!({"runs": [{"points": [[0, 0], [60, 0], [0, 60]], "closed": true}], "fill": "#0000FF"}),
    );
    assert_eq!(t.mean(json!({"x": 2, "y": 2, "w": 10, "h": 10})), "#0000FF");
    let text = t.run(
        "text.add",
        json!({"text": "Xin chào", "x": 20, "y": 120, "sizePx": 40, "color": "#FF0000"}),
    );
    let layers = t.inspect()["layers"].clone();
    let kind = |id: &Value| {
        layers
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["id"] == *id)
            .unwrap()["kind"]
            .clone()
    };
    assert_eq!(kind(&shape["layer"]), json!("vector"));
    assert_eq!(kind(&path["layer"]), json!("vector"));
    assert_eq!(kind(&text["layer"]), json!("text"));
    assert!(text["bounds"]["w"].as_u64().unwrap() > 40);
    let e = t
        .try_run(
            "vector.add_shape",
            json!({"shape": "star", "rect": {"x": 0, "y": 0, "w": 5, "h": 5}}),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
}

#[test]
fn a_document_is_saved_reopened_and_exported() {
    let dir = TempDir::new("files");
    let mut t = Setup::new();
    t.run(
        "file.new",
        json!({"widthPx": 50, "heightPx": 40, "dpi": 150}),
    );
    t.run("layer.new", json!({"kind": "fill", "fill": "#123456"}));
    t.run("page.add", json!({}));
    let saved = t.run("file.save", json!({"path": dir.path("two pages.iai")}));
    assert!(saved["bytes"].as_u64().unwrap() > 0);
    assert_eq!(t.inspect()["modified"], json!(false));

    let opened = t.run("file.open", json!({"path": dir.path("two pages.iai")}));
    assert_eq!(
        (opened["pages"].clone(), opened["dpi"].clone()),
        (json!(2), json!(150.0))
    );
    assert_eq!(
        t.run("doc.list", json!({}))["docs"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    t.run("page.select", json!({"page": 0}));
    assert_eq!(t.mean(json!({"x": 0, "y": 0, "w": 50, "h": 40})), "#123456");

    t.run("file.export", json!({"path": dir.path("page.png")}));
    let png = image::open(dir.path("page.png")).unwrap().to_rgba8();
    assert_eq!(png.dimensions(), (50, 40));
    assert_eq!(png.get_pixel(25, 20).0, [0x12, 0x34, 0x56, 255]);

    let pdf = t.run("export.pdf", json!({"path": dir.path("all.pdf")}));
    assert_eq!(pdf["pages"], json!(2));
    let bytes = std::fs::read(dir.path("all.pdf")).unwrap();
    assert!(bytes.starts_with(b"%PDF"));
    let pdf = t.run(
        "export.pdf",
        json!({"path": dir.path("tabs.pdf"), "allOpenDocuments": true}),
    );
    assert_eq!(pdf["pages"], json!(4));

    let e = t
        .try_run("file.open", json!({"path": dir.path("none.png")}))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    let e = t
        .try_run("file.save", json!({"path": dir.path("x.png")}))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
    let e = t
        .try_run("export.pdf", json!({"path": dir.path("x.png")}))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
}

#[test]
fn an_atomic_batch_closes_the_document_it_opened() {
    let mut t = Setup::new();
    t.run("file.new", json!({"widthPx": 10, "heightPx": 10}));
    let reply = t.engine.session(&mut t.ws).execute_batch(
        vec![
            CommandCall::new("file.new", json!({"widthPx": 20, "heightPx": 20})),
            CommandCall::new("layer.new", json!({})),
            CommandCall::new("layer.delete", json!({"layer": 4242})),
        ],
        true,
    );
    assert!(reply.rolled_back);
    assert_eq!(
        t.run("doc.list", json!({}))["docs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(t.inspect()["widthPx"], json!(10));
}

#[test]
fn an_agent_may_not_download_without_the_network() {
    let mut t = Setup::new();
    t.engine.set_policy(Policy::agent());
    let e = t
        .try_run("ai.model_download", json!({"model": "birefnet"}))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied);
}

#[test]
fn ai_commands_say_what_they_need() {
    let mut t = Setup::new();
    t.run("file.new", json!({"widthPx": 30, "heightPx": 30}));
    let e = t.try_run("ai.inpaint", json!({})).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotEnabled);
    assert_eq!(e.hint.unwrap()["command"], json!("select.rect"));
    let e = t
        .try_run(
            "ai.remove_background",
            json!({"output": "selection", "background": "#0090FF"}),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
    let text = t.run("text.add", json!({"text": "A", "x": 0, "y": 0}));
    let e = t
        .try_run("ai.remove_background", json!({"layer": text["layer"]}))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotEnabled);
}

#[test]
fn a_patch_fill_closes_a_hole_in_a_flat_colour() {
    let mut t = Setup::new();
    t.run(
        "file.new",
        json!({"widthPx": 64, "heightPx": 64, "background": "#336699"}),
    );
    let dot = t.run(
        "vector.add_shape",
        json!({"shape": "rectangle", "rect": {"x": 28, "y": 28, "w": 8, "h": 8}, "fill": "#FF0000"}),
    );
    // Flatten the dot into the background, as a photo would have it.
    t.run("layer.delete", json!({"layer": dot["layer"]}));
    let bg = t.inspect()["layers"][0]["id"].clone();
    t.run(
        "select.rect",
        json!({"rect": {"x": 26, "y": 26, "w": 12, "h": 12}}),
    );
    let filled = t.run("ai.inpaint", json!({"layer": bg}));
    assert!(filled["pixelsFilled"].as_u64().unwrap() >= 144);
    assert_eq!(
        t.mean(json!({"x": 26, "y": 26, "w": 12, "h": 12})),
        "#336699"
    );
}

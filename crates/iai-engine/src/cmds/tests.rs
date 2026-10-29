//! The core commands, driven as an agent drives them: JSON in, JSON out,
//! no window.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

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
fn an_ellipse_and_a_polygon_are_selected_and_the_page_cuts_them() {
    let mut t = Setup::new();
    t.run(
        "file.new",
        json!({"widthPx": 100, "heightPx": 80, "background": "#FFFFFF"}),
    );
    let sel = t.run(
        "select.ellipse",
        json!({"rect": {"x": 10, "y": 10, "w": 40, "h": 20}}),
    );
    assert_eq!(sel["bounds"], json!({"x": 10, "y": 10, "w": 40, "h": 20}));
    // Filled: its middle is red, the box's corner is not.
    t.run("edit.fill", json!({"color": "#FF0000"}));
    assert_eq!(t.mean(json!({"x": 28, "y": 18, "w": 4, "h": 4})), "#FF0000");
    assert_eq!(t.mean(json!({"x": 10, "y": 10, "w": 3, "h": 3})), "#FFFFFF");
    // A circle the page's corner cuts: a quarter of it.
    let sel = t.run(
        "select.ellipse",
        json!({"rect": {"x": -30, "y": -30, "w": 60, "h": 60}}),
    );
    let b = &sel["bounds"];
    assert_eq!((b["x"].as_i64(), b["y"].as_i64()), (Some(0), Some(0)));
    assert!((29..=31).contains(&b["w"].as_i64().unwrap()), "{b}");

    // A triangle added to it, then filled blue.
    t.run("select.none", json!({}));
    let sel = t.run(
        "select.polygon",
        json!({"points": [[50, 40], [90, 40], [50, 78]], "mode": "add"}),
    );
    let b = &sel["bounds"];
    assert_eq!((b["x"].as_i64(), b["y"].as_i64()), (Some(50), Some(40)));
    t.run("edit.fill", json!({"color": "#0000FF"}));
    assert_eq!(t.mean(json!({"x": 52, "y": 42, "w": 4, "h": 4})), "#0000FF");
    assert_eq!(t.mean(json!({"x": 84, "y": 72, "w": 4, "h": 4})), "#FFFFFF");
    // One undo step each.
    t.run("edit.undo", json!({}));
    assert_eq!(t.mean(json!({"x": 52, "y": 42, "w": 4, "h": 4})), "#FFFFFF");

    let e = t
        .try_run(
            "select.polygon",
            json!({"points": [[200, 0], [300, 0], [250, 50]]}),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
    let e = t
        .try_run("select.polygon", json!({"points": [[0, 0], [10, 0]]}))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
}

#[test]
fn a_layer_is_scaled_turned_and_moved_as_free_transform_does() {
    let mut t = Setup::new();
    t.run(
        "file.new",
        json!({"widthPx": 120, "heightPx": 100, "background": "#FFFFFF"}),
    );
    // A red bar 20 × 10 at (10, 20), on a layer of its own.
    let layer = t.run("layer.new", json!({}))["layer"].as_u64().unwrap();
    t.run(
        "select.rect",
        json!({"rect": {"x": 10, "y": 20, "w": 20, "h": 10}}),
    );
    t.run("edit.fill", json!({"color": "#FF0000"}));
    t.run("select.none", json!({}));

    // Twice as large round its middle (20, 25): 40 × 20 from (0, 15).
    t.run("layer.transform", json!({"layer": layer, "scale": 2}));
    assert_eq!(
        t.mean(json!({"x": 2, "y": 17, "w": 36, "h": 16})),
        "#FF0000"
    );
    assert_eq!(t.mean(json!({"x": 2, "y": 37, "w": 36, "h": 4})), "#FFFFFF");
    // A quarter turn: 20 wide, 40 high, round the same middle.
    t.run("layer.transform", json!({"layer": layer, "angle": 90}));
    assert_eq!(
        t.mean(json!({"x": 12, "y": 7, "w": 16, "h": 36})),
        "#FF0000"
    );
    assert_eq!(
        t.mean(json!({"x": 32, "y": 20, "w": 6, "h": 10})),
        "#FFFFFF"
    );
    // A move alone shifts it, nothing resampled.
    t.run(
        "layer.transform",
        json!({"layer": layer, "dx": 50, "dy": 10}),
    );
    assert_eq!(
        t.mean(json!({"x": 62, "y": 17, "w": 16, "h": 36})),
        "#FF0000"
    );
    // Each was one undo step.
    t.run("edit.undo", json!({}));
    t.run("edit.undo", json!({}));
    assert_eq!(
        t.mean(json!({"x": 2, "y": 17, "w": 36, "h": 16})),
        "#FF0000"
    );

    // Nothing to do, both kinds of scale, the Background: refused.
    let e = t
        .try_run("layer.transform", json!({"layer": layer, "scale": 1}))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
    let e = t
        .try_run(
            "layer.transform",
            json!({"layer": layer, "scale": 2, "scaleX": 3}),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
    let background = t.inspect()["layers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["id"].as_u64() != Some(layer))
        .and_then(|l| l["id"].as_u64())
        .unwrap();
    let e = t
        .try_run("layer.transform", json!({"layer": background, "angle": 3}))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotEnabled);
}

/// The layer ids of the shown page, bottom first.
fn layer_ids(t: &mut Setup) -> Vec<u64> {
    let mut ids: Vec<u64> = t.inspect()["layers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["id"].as_u64().unwrap())
        .collect();
    // doc.inspect lists the top first.
    ids.reverse();
    ids
}

#[test]
fn layers_are_grouped_merged_masked_inverted_and_clipped_as_the_panel_does() {
    let mut t = Setup::new();
    t.run(
        "file.new",
        json!({"widthPx": 40, "heightPx": 30, "background": "#FFFFFF"}),
    );
    let a = t.run("layer.new", json!({}))["layer"].as_u64().unwrap();
    let b = t.run("layer.new", json!({}))["layer"].as_u64().unwrap();
    // Group the two, then dissolve the folder.
    let grouped = t.run("layer.group", json!({"layers": [a, b]}));
    let kinds: Vec<String> = grouped["layers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["kind"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(kinds.iter().any(|k| k == "group"), "{kinds:?}");
    let group = grouped["layers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == json!("group"))
        .and_then(|l| l["id"].as_u64())
        .unwrap();
    t.run("layer.ungroup", json!({"layer": group}));
    assert_eq!(layer_ids(&mut t).len(), 3);
    let e = t.try_run("layer.ungroup", json!({"layer": a})).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotEnabled);

    // A mask: off and on, linked, inverted, applied.
    t.run(
        "select.rect",
        json!({"rect": {"x": 0, "y": 0, "w": 10, "h": 10}}),
    );
    t.run("edit.fill", json!({"layer": b, "color": "#FF0000"}));
    t.run("select.none", json!({}));
    t.run("layer.mask", json!({"layer": b, "op": "hide_all"}));
    assert_eq!(t.mean(json!({"x": 2, "y": 2, "w": 6, "h": 6})), "#FFFFFF");
    t.run("layer.mask", json!({"layer": b, "op": "disable"}));
    assert_eq!(t.mean(json!({"x": 2, "y": 2, "w": 6, "h": 6})), "#FF0000");
    t.run("layer.mask", json!({"layer": b, "op": "enable"}));
    t.run("layer.mask", json!({"layer": b, "op": "invert"}));
    assert_eq!(t.mean(json!({"x": 2, "y": 2, "w": 6, "h": 6})), "#FF0000");
    t.run("layer.mask", json!({"layer": b, "op": "unlink"}));
    t.run("layer.mask", json!({"layer": b, "op": "apply"}));
    assert_eq!(t.mean(json!({"x": 2, "y": 2, "w": 6, "h": 6})), "#FF0000");

    // Invert the red into cyan; one step back.
    t.run("layer.invert", json!({"layer": b}));
    assert_eq!(t.mean(json!({"x": 2, "y": 2, "w": 6, "h": 6})), "#00FFFF");
    t.run("edit.undo", json!({}));
    assert_eq!(t.mean(json!({"x": 2, "y": 2, "w": 6, "h": 6})), "#FF0000");

    // The red's pixels as a selection.
    let sel = t.run("select.from_layer", json!({"layer": b}));
    assert_eq!(sel["bounds"], json!({"x": 0, "y": 0, "w": 10, "h": 10}));
    t.run("select.none", json!({}));

    // Clip `b` to `a` below it (empty): nothing of it shows; released, it does.
    t.run("layer.clip", json!({"layer": b}));
    assert_eq!(t.mean(json!({"x": 2, "y": 2, "w": 6, "h": 6})), "#FFFFFF");
    t.run("layer.clip", json!({"layer": b}));
    assert_eq!(t.mean(json!({"x": 2, "y": 2, "w": 6, "h": 6})), "#FF0000");

    // Stamp the visible ones into a new layer, merge the picked, and the
    // Background becomes a plain layer.
    let before = layer_ids(&mut t).len();
    t.run("layer.merge", json!({"what": "visible"}));
    assert_eq!(layer_ids(&mut t).len(), before + 1);
    let ids = layer_ids(&mut t);
    t.run("layer.group", json!({"layers": [ids[1], ids[2]]}));
    let folder = layer_ids(&mut t)
        .into_iter()
        .find(|id| {
            t.inspect()["layers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|l| l["id"].as_u64() == Some(*id) && l["kind"] == json!("group"))
        })
        .unwrap();
    t.run("layer.merge", json!({"what": "group", "layer": folder}));
    assert!(
        !t.inspect()["layers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["kind"] == json!("group"))
    );
    let background = layer_ids(&mut t)[0];
    t.run("layer.from_background", json!({"layer": background}));
    let info = t.inspect();
    assert!(
        info["layers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|l| l["isBackground"] == json!(false))
    );

    // A copy, and an adjustment layer.
    // With one layer picked, one copy; with two, two.
    let n = layer_ids(&mut t).len();
    t.run("layer.set", json!({"layer": background, "activate": true}));
    t.run("layer.duplicate", json!({"picked": true}));
    assert_eq!(layer_ids(&mut t).len(), n + 1);
    let n = layer_ids(&mut t).len();
    let adj = t.run(
        "layer.new",
        json!({"kind": "adjustment", "adjustment": {"kind": "invert"}}),
    );
    assert_eq!(layer_ids(&mut t).len(), n + 1);
    assert!(adj["layer"].is_u64());
    let e = t
        .try_run("layer.new", json!({"kind": "adjustment"}))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
}

#[test]
fn the_mode_and_the_profile_change_and_come_back_with_undo() {
    let mut t = Setup::new();
    t.run(
        "file.new",
        json!({"widthPx": 20, "heightPx": 10, "background": "#3366CC"}),
    );
    let m = t.run("image.mode", json!({"bits": 16}));
    assert_eq!(
        (m["bits"].clone(), m["color"].clone()),
        (json!(16), json!("rgb"))
    );
    let m = t.run("image.mode", json!({"color": "cmyk"}));
    assert_eq!(m["color"], json!("cmyk"));
    let m = t.run("image.mode", json!({"color": "rgb"}));
    assert_eq!(m["color"], json!("rgb"));
    t.run(
        "image.profile",
        json!({"action": "convert", "profile": "adobe_rgb"}),
    );
    t.run(
        "image.profile",
        json!({"action": "assign", "profile": "srgb"}),
    );
    let e = t.try_run("image.mode", json!({"bits": 12})).unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
    let e = t
        .try_run(
            "image.mode",
            json!({"color": "rgb", "icc": {"path": "x.icc"}}),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
}

#[test]
fn a_selection_is_kept_as_a_channel_and_loaded_back() {
    let mut t = Setup::new();
    t.run("file.new", json!({"widthPx": 40, "heightPx": 30}));
    let e = t
        .try_run("channel.edit", json!({"op": "save_selection"}))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotEnabled);
    t.run(
        "select.rect",
        json!({"rect": {"x": 5, "y": 5, "w": 10, "h": 8}}),
    );
    let kept = t.run("channel.edit", json!({"op": "save_selection"}));
    assert_eq!(kept["channels"].as_array().unwrap().len(), 1);
    t.run("channel.edit", json!({"op": "duplicate", "channel": 0}));
    let named = t.run(
        "channel.edit",
        json!({"op": "rename", "channel": 1, "name": "Áo"}),
    );
    assert_eq!(named["channels"][1], json!("Áo"));
    t.run("channel.edit", json!({"op": "new"}));
    t.run("channel.edit", json!({"op": "delete", "channel": 2}));
    // Loaded back: the same box; added to another, the two.
    t.run("select.none", json!({}));
    let sel = t.run("select.from_channel", json!({"channel": 0}));
    assert_eq!(sel["bounds"], json!({"x": 5, "y": 5, "w": 10, "h": 8}));
    t.run(
        "select.rect",
        json!({"rect": {"x": 30, "y": 20, "w": 5, "h": 5}}),
    );
    let sel = t.run("select.from_channel", json!({"channel": 1, "mode": "add"}));
    assert_eq!(sel["bounds"], json!({"x": 5, "y": 5, "w": 30, "h": 20}));
    let e = t
        .try_run("select.from_channel", json!({"channel": 7}))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
}

#[test]
fn a_turned_frame_is_cropped_to_a_print_size_and_the_edge_filled() {
    let mut t = Setup::new();
    t.run(
        "file.new",
        json!({"widthPx": 200, "heightPx": 100, "background": "#3366CC"}),
    );
    // Turned a little and scaled to 2×3 cm at 300 dpi: 236×354 px.
    let out = t.run(
        "image.crop",
        json!({"frame": {"x": 100, "y": 50, "w": 60, "h": 90, "angle": 5,
                         "outW": 236, "outH": 354}, "dpi": 300}),
    );
    assert_eq!(
        (
            out["widthPx"].clone(),
            out["heightPx"].clone(),
            out["dpi"].clone()
        ),
        (json!(236), json!(354), json!(300.0))
    );
    // Past the edge: what the crop adds takes the fill.
    t.run("edit.undo", json!({}));
    let out = t.run(
        "image.crop",
        json!({"rect": {"x": -20, "y": 0, "w": 60, "h": 20}, "fill": "#FFFFFF"}),
    );
    assert_eq!(out["widthPx"], json!(60));
    assert_eq!(t.mean(json!({"x": 0, "y": 0, "w": 15, "h": 20})), "#FFFFFF");
    assert_eq!(
        t.mean(json!({"x": 25, "y": 0, "w": 30, "h": 20})),
        "#3366CC"
    );
    let e = t
        .try_run(
            "image.crop",
            json!({"rect": {"x": 0, "y": 0, "w": 5, "h": 5},
                   "frame": {"x": 5, "y": 5, "w": 5, "h": 5}}),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
    let e = t
        .try_run(
            "image.crop",
            json!({"frame": {"x": 5, "y": 5, "w": 0.5, "h": 5}}),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
}

/// A frame crop is a job made on a copy of the page. Should the page change
/// while it runs, the crop is made again on the page as it is then; either
/// way it is one undo step.
#[test]
fn a_crop_in_the_background_lands_on_the_page_as_it_is() {
    let frame = json!({"frame": {"x": 50, "y": 50, "w": 100, "h": 100, "angle": 0.5,
                                 "outW": 100, "outH": 100}});
    for changed_meanwhile in [false, true] {
        let mut t = Setup::new();
        t.run(
            "file.new",
            json!({"widthPx": 200, "heightPx": 100, "background": "#0000FF"}),
        );
        t.run(
            "select.rect",
            json!({"rect": {"x": 0, "y": 0, "w": 100, "h": 100}}),
        );
        t.run("edit.fill", json!({"color": "#FF0000"}));
        t.run("select.none", json!({}));
        let job = {
            let mut s = t.engine.session(&mut t.ws);
            match s.execute(CommandCall::new("image.crop", frame.clone()).in_background()) {
                Ok(Reply::Started { job }) => job,
                other => panic!("{other:?}"),
            }
        };
        if changed_meanwhile {
            // The person turns the page around under the job: red goes right.
            t.ws.documents[0].canvas.flip_horizontal();
        }
        {
            let mut s = t.engine.session(&mut t.ws);
            let state = s.wait(job, Duration::from_secs(60));
            assert!(matches!(state, Some(JobState::Done { .. })), "{state:?}");
        }
        let kept = if changed_meanwhile {
            "#0000FF"
        } else {
            "#FF0000"
        };
        assert_eq!(
            t.mean(json!({"x": 40, "y": 40, "w": 20, "h": 20})),
            kept,
            "changed meanwhile: {changed_meanwhile}"
        );
        t.run("edit.undo", json!({}));
        assert_eq!(t.inspect()["widthPx"], json!(200), "one step undoes it");
    }
}

/// Canvas Size keeps the picture where the anchor says, and fills what the
/// Background gains when given a colour.
#[test]
fn canvas_size_grows_round_the_anchor() {
    let mut t = Setup::new();
    t.run(
        "file.new",
        json!({"widthPx": 100, "heightPx": 50, "background": "#FF0000"}),
    );
    let out = t.run(
        "image.canvas_size",
        json!({"widthPx": 200, "heightPx": 100, "anchor": "center", "fill": "#FFFFFF"}),
    );
    assert_eq!(
        (out["widthPx"].clone(), out["heightPx"].clone()),
        (json!(200), json!(100))
    );
    assert_eq!(
        t.mean(json!({"x": 60, "y": 35, "w": 80, "h": 30})),
        "#FF0000"
    );
    assert_eq!(t.mean(json!({"x": 0, "y": 0, "w": 40, "h": 20})), "#FFFFFF");
    assert_eq!(
        t.mean(json!({"x": 160, "y": 80, "w": 40, "h": 20})),
        "#FFFFFF"
    );
    t.run("edit.undo", json!({}));
    let out = t.run(
        "image.canvas_size",
        json!({"widthPx": 60, "heightPx": 50, "anchor": "right"}),
    );
    assert_eq!(out["widthPx"], json!(60));
    assert_eq!(t.mean(json!({"x": 0, "y": 0, "w": 60, "h": 50})), "#FF0000");
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
fn pages_are_renamed_moved_deleted_and_share_a_master() {
    let mut t = Setup::new();
    t.run("file.new", json!({"widthPx": 40, "heightPx": 30}));
    t.run("page.add", json!({}));
    t.run("layer.new", json!({"kind": "fill", "fill": "#00FF00"}));
    t.run("page.add", json!({}));
    let names = |t: &mut Setup| -> Vec<String> {
        t.inspect()["pageList"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap().to_string())
            .collect()
    };
    t.run("page.rename", json!({"page": 1, "name": "Bìa"}));
    assert_eq!(names(&mut t), ["Trang 1", "Bìa", "Trang 3"]);
    // The page keeps what it holds when it moves.
    t.run("page.move", json!({"from": 1, "to": 0}));
    assert_eq!(names(&mut t)[0], "Bìa");
    t.run("page.select", json!({"page": 0}));
    assert_eq!(t.mean(json!({"x": 0, "y": 0, "w": 40, "h": 30})), "#00FF00");
    t.run("page.rename", json!({"page": 0, "name": ""}));
    assert_eq!(names(&mut t)[0], "Trang 1");

    let p = t.run("page.delete", json!({"page": 2}));
    assert_eq!(p["pages"], json!(2));
    let e = t.try_run("page.delete", json!({"page": 5})).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    t.run("page.delete", json!({"page": 1}));
    let e = t.try_run("page.delete", json!({"page": 0})).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotEnabled);

    // The master drawn beneath a page that shows it.
    t.run("page.add", json!({}));
    t.run("page.master", json!({"op": "edit"}));
    t.run("layer.new", json!({"kind": "fill", "fill": "#FF0000"}));
    t.run("page.master", json!({"op": "done"}));
    t.run("page.select", json!({"page": 1}));
    let shows_master = |t: &Setup| {
        use crate::Workspace;
        t.ws.documents()[0].active_master_backdrop().is_some()
    };
    t.run("page.master", json!({"op": "show", "page": 1}));
    assert!(shows_master(&t));
    t.run("page.master", json!({"op": "hide", "page": 1}));
    assert!(!shows_master(&t));
    t.run("page.master", json!({"op": "delete"}));
    let e = t
        .try_run("page.master", json!({"op": "delete"}))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotEnabled);
    let e = t.try_run("page.master", json!({"op": "show"})).unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);

    t.engine.take_events();
    t.run("page.setup", json!({"bleedMm": 3, "marginMm": 5}));
    let said = t.engine.take_events().into_iter().find_map(|e| match e {
        crate::EngineEvent::Notice { vi, .. } => Some(vi),
        _ => None,
    });
    assert_eq!(said.as_deref(), Some("Trang: bleed 3mm · lề an toàn 5mm"));
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

/// Where a layer's pixels lie: `x, y, w, h`.
fn bounds_of(t: &mut Setup, id: &Value) -> [i64; 4] {
    let layers = t.inspect()["layers"].clone();
    let b = layers
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["id"] == *id)
        .unwrap()["bounds"]
        .clone();
    ["x", "y", "w", "h"].map(|k| b[k].as_i64().unwrap())
}

#[test]
fn shapes_are_aligned_spread_copied_by_steps_and_welded() {
    let mut t = Setup::new();
    t.run("file.new", json!({"widthPx": 300, "heightPx": 120}));
    let rect = |t: &mut Setup, x: i64, y: i64, fill: &str| {
        t.run(
            "vector.add_shape",
            json!({"shape": "rectangle", "rect": {"x": x, "y": y, "w": 20, "h": 20}, "fill": fill}),
        )["layer"]
            .clone()
    };
    let a = rect(&mut t, 10, 10, "#FF0000");
    let b = rect(&mut t, 50, 60, "#00FF00");
    let c = rect(&mut t, 200, 30, "#0000FF");

    // Several line up with the box round them all: the tops with the top-most.
    t.run("layer.align", json!({"layers": [a, b, c], "align": "top"}));
    let tops: Vec<i64> = [&a, &b, &c].map(|id| bounds_of(&mut t, id)[1]).to_vec();
    assert!(tops.iter().all(|&y| y == tops[0]), "{tops:?}");
    // The middle one halfway between the outer two.
    t.run(
        "layer.distribute",
        json!({"layers": [a, b, c], "axis": "horizontal"}),
    );
    let mid = |b: [i64; 4]| b[0] * 2 + b[2];
    let (ba, bb, bc) = (
        bounds_of(&mut t, &a),
        bounds_of(&mut t, &b),
        bounds_of(&mut t, &c),
    );
    assert!(
        (mid(ba) + mid(bc) - 2 * mid(bb)).abs() <= 2,
        "{ba:?} {bb:?} {bc:?}"
    );
    let e = t
        .try_run(
            "layer.distribute",
            json!({"layers": [a, b], "axis": "vertical"}),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);

    // A copy a step to the right; again, from the copy.
    let before = layer_ids(&mut t).len();
    let one = t.run("layer.step_copy", json!({"layers": [a], "dx": 30}))["layer"].clone();
    assert_eq!(bounds_of(&mut t, &one)[0], ba[0] + 30);
    let two = t.run("layer.step_copy", json!({"dx": 30}))["layer"].clone();
    assert_eq!(bounds_of(&mut t, &two)[0], ba[0] + 60);
    assert_eq!(layer_ids(&mut t).len(), before + 2);
    t.run("edit.undo", json!({}));
    assert_eq!(layer_ids(&mut t).len(), before + 1);
    let e = t
        .try_run(
            "layer.step_copy",
            json!({"dx": 5, "matrix": [1, 0, 0, 1, 5, 0]}),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
    let bg = layer_ids(&mut t)[0];
    let e = t
        .try_run("layer.step_copy", json!({"layers": [bg], "dx": 5}))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotEnabled);

    // A shape becomes curves once; welded with another, one is left.
    t.run("vector.to_curves", json!({"layer": b}));
    let e = t
        .try_run("vector.to_curves", json!({"layer": b}))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotEnabled);
    let before = layer_ids(&mut t).len();
    t.run("vector.shaping", json!({"op": "weld", "layers": [a, c]}));
    assert_eq!(layer_ids(&mut t).len(), before - 1);
}

#[test]
fn a_picture_is_put_in_a_frame_arranged_and_taken_out() {
    let mut t = Setup::new();
    t.run("file.new", json!({"widthPx": 100, "heightPx": 50}));
    let frame = t.run(
        "vector.add_shape",
        json!({"shape": "rectangle", "rect": {"x": 0, "y": 0, "w": 50, "h": 50}, "fill": "#0000FF"}),
    )["layer"]
        .clone();
    let photo = t.run("layer.new", json!({"kind": "fill", "fill": "#FF0000"}))["layer"].clone();
    let left = json!({"x": 10, "y": 20, "w": 30, "h": 10});
    let right = json!({"x": 60, "y": 20, "w": 30, "h": 10});

    let e = t
        .try_run("vector.powerclip", json!({"op": "fit", "layers": [photo]}))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotEnabled);
    // In the frame: the photo shows only inside it.
    t.run(
        "vector.powerclip",
        json!({"op": "place", "layers": [photo, frame]}),
    );
    assert_eq!(t.mean(left.clone()), "#FF0000");
    assert_eq!(t.mean(right.clone()), "#FFFFFF");
    // Fit: 100×50 into 50×50 is 50×25, centred; the frame shows above it.
    t.run("vector.powerclip", json!({"op": "fit", "layers": [photo]}));
    assert_eq!(t.mean(json!({"x": 10, "y": 2, "w": 30, "h": 6})), "#0000FF");
    assert_eq!(
        t.mean(json!({"x": 10, "y": 20, "w": 30, "h": 8})),
        "#FF0000"
    );
    t.run("edit.undo", json!({}));
    assert_eq!(t.mean(json!({"x": 10, "y": 2, "w": 30, "h": 6})), "#FF0000");
    // Out again: all of it shows.
    t.run(
        "vector.powerclip",
        json!({"op": "extract", "layers": [photo]}),
    );
    assert_eq!(t.mean(right), "#FF0000");
    let e = t
        .try_run(
            "vector.powerclip",
            json!({"op": "place", "layers": [photo]}),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotEnabled);
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

    // A .iai copy of a document of pages keeps them all.
    t.run("file.export", json!({"path": dir.path("copy.iai")}));
    let copy = t.run("file.open", json!({"path": dir.path("copy.iai")}));
    assert_eq!(copy["pages"], json!(2));
    t.run("doc.close", json!({"doc": copy["doc"]}));

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

/// A PDF of `greys.len()` pages, each 1 × 0.5 inch and of one grey.
fn grey_pages_pdf(path: &str, greys: &[u8]) {
    let pages: Vec<_> = greys
        .iter()
        .map(|&g| iai_formats::pdf::ImagePdfPage {
            rgba: [g, g, g, 255].repeat(8),
            width: 4,
            height: 2,
            page_points: (72.0, 36.0),
        })
        .collect();
    iai_formats::pdf::write_image_pdf(std::path::Path::new(path), &pages).unwrap();
}

fn grey(hex: &str) -> u8 {
    u8::from_str_radix(&hex[1..3], 16).unwrap()
}

#[test]
fn a_pdf_opens_the_pages_asked_at_the_dpi_asked_and_is_written_back() {
    let dir = TempDir::new("pdf-pages");
    let source = dir.path("five.pdf");
    grey_pages_pdf(&source, &[0x10, 0x30, 0x50, 0x70, 0x90]);
    let mut t = Setup::new();

    let all = t.run("file.open", json!({"path": source}));
    assert_eq!(
        (
            all["pages"].clone(),
            all["widthPx"].clone(),
            all["dpi"].clone()
        ),
        (json!(5), json!(300), json!(300.0))
    );

    // In the PDF's order, each once.
    let some = t.run(
        "file.open",
        json!({"path": source, "pages": [3, 1, 3], "dpi": 100}),
    );
    assert_eq!(
        (
            some["pages"].clone(),
            some["widthPx"].clone(),
            some["heightPx"].clone()
        ),
        (json!(2), json!(100), json!(50))
    );
    assert_eq!(some["title"], json!("five"));
    let whole = json!({"x": 0, "y": 0, "w": 100, "h": 50});
    assert_eq!(t.mean(whole.clone()), "#303030");
    t.run("page.select", json!({"page": 1}));
    assert_eq!(t.mean(whole.clone()), "#707070");
    t.run(
        "vector.add_path",
        json!({"runs": [{"points": [[10, 40], [50, 10], [90, 40]], "smooth": true}],
               "stroke": "#FF0000", "strokeWidth": 4, "join": "round", "cap": "round"}),
    );
    let pdf = t.run("export.pdf", json!({"path": dir.path("out.pdf")}));
    assert_eq!(pdf["pages"], json!(2));
    let reread =
        iai_formats::pdf::PdfImporter::probe(std::path::Path::new(&dir.path("out.pdf"))).unwrap();
    assert_eq!(reread.page_count, 2);

    // Not its file: saving in place would replace the five pages.
    let e = t.try_run("file.save", json!({})).unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
    t.run("file.save", json!({"path": dir.path("two.iai")}));

    let e = t
        .try_run("file.open", json!({"path": source, "pages": [5]}))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    let e = t
        .try_run("file.open", json!({"path": source, "dpi": 1200}))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
    let e = t
        .try_run(
            "file.open",
            json!({"path": dir.path("two.iai"), "pages": [0]}),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
}

#[test]
fn pdf_pages_too_large_together_are_refused_with_the_call_that_fits() {
    let dir = TempDir::new("pdf-budget");
    let source = dir.path("five.pdf");
    grey_pages_pdf(&source, &[0x20; 5]);
    let choice = super::file::PdfChoice {
        pages: None,
        dpi: Some(100),
    };
    // Each page is 100 × 50 px at 100 dpi: 25 000 px in all.
    let fits = super::file::decode_pdf(
        std::path::Path::new(&source),
        &choice,
        25_000,
        &JobCtx::new(),
    );
    assert!(fits.is_ok());
    let e = match super::file::decode_pdf(
        std::path::Path::new(&source),
        &choice,
        12_000,
        &JobCtx::new(),
    ) {
        Ok(_) => panic!("five pages over the limit were opened"),
        Err(e) => e,
    };
    assert_eq!(e.code, ErrorCode::TooLarge);
    assert!(e.message.contains("the first 2 fit"), "{}", e.message);
    // 68 dpi: 68 × 34 px a page, 11 560 in all.
    assert!(e.message.contains("all fit at 68"), "{}", e.message);
    assert_eq!(
        e.hint.unwrap(),
        json!({"command": "file.open", "args": {"path": source, "pages": [0, 1], "dpi": 100}})
    );
}

#[test]
fn a_path_can_curve_through_its_points_and_round_its_corners() {
    let mut t = Setup::new();
    t.run("file.new", json!({"widthPx": 200, "heightPx": 200}));
    let arch = json!([[20, 100], [100, 20], [180, 100]]);
    // Straight, the run passes 7 px below (55, 55); smooth, through it.
    let straight = t.run(
        "vector.add_path",
        json!({"runs": [{"points": arch}], "stroke": "#000000", "strokeWidth": 6}),
    );
    let on_curve = json!({"x": 54, "y": 54, "w": 2, "h": 2});
    assert_eq!(grey(&t.mean(on_curve.clone())), 0xFF);
    t.run("layer.delete", json!({"layer": straight["layer"]}));
    let smooth = t.run(
        "vector.add_path",
        json!({"runs": [{"points": arch, "smooth": true}], "stroke": "#000000", "strokeWidth": 6}),
    );
    assert!(grey(&t.mean(on_curve)) < 0x40);
    t.run("layer.delete", json!({"layer": smooth["layer"]}));

    // A pointed V: a mitred corner reaches 22 px past its tip, a round one 10.
    let v = json!([{"points": [[20, 180], [100, 20], [180, 180]]}]);
    let past_tip = json!({"x": 99, "y": 4, "w": 2, "h": 2});
    let mitred = t.run(
        "vector.add_path",
        json!({"runs": v, "stroke": "#000000", "strokeWidth": 20}),
    );
    assert!(grey(&t.mean(past_tip.clone())) < 0x40);
    t.run("layer.delete", json!({"layer": mitred["layer"]}));
    t.run(
        "vector.add_path",
        json!({"runs": v, "stroke": "#000000", "strokeWidth": 20, "join": "round"}),
    );
    assert_eq!(grey(&t.mean(past_tip)), 0xFF);
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

#[test]
fn a_folder_is_listed_by_name_with_image_sizes_and_endings_filtered() {
    let dir = std::env::temp_dir().join(format!("iai-file-list-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("lop_a")).unwrap();
    std::fs::create_dir_all(dir.join(".an")).unwrap();
    let mut s = Setup::new();
    s.run("file.new", json!({"widthPx": 30, "heightPx": 20}));
    for name in ["b.png", "A.png"] {
        s.run(
            "file.export",
            json!({"path": dir.join(name).display().to_string()}),
        );
    }
    std::fs::write(dir.join("ghi_chu.txt"), b"x").unwrap();
    std::fs::write(dir.join(".an.png"), b"x").unwrap();
    let all = s.run("file.list", json!({"path": dir.display().to_string()}));
    let names: Vec<&str> = all["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["A.png", "b.png", "ghi_chu.txt"]);
    assert_eq!(all["folders"], json!(["lop_a"]));
    assert_eq!(
        (
            all["files"][0]["widthPx"].clone(),
            all["files"][0]["heightPx"].clone()
        ),
        (json!(30), json!(20))
    );
    assert!(all["files"][2].get("widthPx").is_none());
    let png = s.run(
        "file.list",
        json!({"path": dir.display().to_string(), "ext": [".PNG"]}),
    );
    assert_eq!(png["files"].as_array().unwrap().len(), 2);
    let e = s
        .try_run(
            "file.list",
            json!({"path": dir.join("khong").display().to_string()}),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A phone photo of an ID card lying on a red stool: the card's corners are
/// (163, 440) to (797, 840), with lines of print on it.
fn card_photo(path: &str) {
    let img = image::RgbaImage::from_fn(960, 1280, |x, y| {
        let inside = (163..797).contains(&x) && (440..840).contains(&y);
        if !inside {
            let n = ((x * 7 + y * 13) % 17) as u8;
            return image::Rgba([185 + n, 40 + n / 2, 45, 255]);
        }
        let print = (360..760).contains(&x) && (y / 24) % 2 == 1 && (520..800).contains(&y);
        let photo = (190..330).contains(&x) && (560..760).contains(&y);
        if print {
            image::Rgba([60, 70, 80, 255])
        } else if photo {
            image::Rgba([120, 100, 90, 255])
        } else {
            image::Rgba([210, 230, 220, 255])
        }
    });
    img.save(path).unwrap();
}

#[test]
fn an_id_card_shot_at_a_slant_is_found_and_laid_flat() {
    let dir = TempDir::new("perspective");
    let photo = dir.path("the.png");
    card_photo(&photo);
    let mut s = Setup::new();
    s.run("file.open", json!({"path": photo}));
    let flat = s.run("image.perspective_crop", json!({}));
    assert_eq!(
        (
            flat["widthPx"].clone(),
            flat["heightPx"].clone(),
            flat["dpi"].clone()
        ),
        (json!(1011), json!(638), json!(300.0))
    );
    let want = [
        [163.0, 440.0],
        [797.0, 440.0],
        [797.0, 840.0],
        [163.0, 840.0],
    ];
    for (got, want) in flat["corners"].as_array().unwrap().iter().zip(want) {
        let (x, y) = (got[0].as_f64().unwrap(), got[1].as_f64().unwrap());
        assert!(
            (x - want[0]).abs() <= 2.0 && (y - want[1]).abs() <= 2.0,
            "{flat}"
        );
    }
    assert!(flat["found"].as_f64().unwrap() > 0.3, "{flat}");
    // Nothing of the stool is left, but for the blur of the edge itself.
    assert_eq!(
        s.mean(json!({"x": 3, "y": 300, "w": 6, "h": 30})),
        "#D2E6DC"
    );

    // Corners read roughly off a preview are fitted to the edges.
    s.run("edit.undo", json!({}));
    assert_eq!(s.inspect()["widthPx"], json!(960));
    let given = s.run(
        "image.perspective_crop",
        json!({"corners": [[805, 832], [170, 452], [155, 830], [790, 447]], "widthMm": 60, "heightMm": 40, "dpi": 254}),
    );
    assert_eq!(
        (given["widthPx"].clone(), given["heightPx"].clone()),
        (json!(600), json!(400))
    );
    assert!(given["found"].is_null());
    let first = &given["corners"][0];
    assert!(
        (first[0].as_f64().unwrap() - 163.0).abs() <= 2.0
            && (first[1].as_f64().unwrap() - 440.0).abs() <= 2.0,
        "{given}"
    );

    // A photo with no card in it says so.
    s.run("file.new", json!({"widthPx": 400, "heightPx": 300}));
    let e = s.try_run("image.perspective_crop", json!({})).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
}

#[test]
fn a_contact_sheet_numbers_a_folders_photos_in_one_picture() {
    let dir = TempDir::new("contact");
    for (name, w, h) in [
        ("b.png", 300u32, 200u32),
        ("a.png", 120, 240),
        ("c.jpg", 50, 50),
    ] {
        image::RgbImage::from_pixel(w, h, image::Rgb([90, 120, 150]))
            .save(dir.path(name))
            .unwrap();
    }
    std::fs::write(dir.path("ghi_chu.txt"), b"x").unwrap();
    std::fs::write(dir.path("hong.png"), b"not a png").unwrap();
    let mut s = Setup::new();
    let sheet = s.run(
        "file.contact_sheet",
        json!({"path": dir.0.display().to_string(), "cellPx": 128}),
    );
    let names: Vec<(u64, &str)> = sheet["photos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["n"].as_u64().unwrap(), p["name"].as_str().unwrap()))
        .collect();
    assert_eq!(names, [(1, "a.png"), (2, "b.png"), (3, "c.jpg")]);
    assert_eq!(sheet["photos"][1]["widthPx"], json!(300));
    assert_eq!(sheet["unread"], json!(["hong.png"]));
    use base64::Engine as _;
    let png = base64::engine::general_purpose::STANDARD
        .decode(sheet["pngBase64"].as_str().unwrap())
        .unwrap();
    let picture = image::load_from_memory(&png).unwrap();
    // Three copies of 128 in a row, 8 apart.
    assert_eq!((picture.width(), picture.height()), (3 * 136 + 8, 136 + 8));

    // Files given are shown in their order.
    let given = s.run(
        "file.contact_sheet",
        json!({"files": [{"path": dir.path("c.jpg")}, {"path": dir.path("a.png")}]}),
    );
    assert_eq!(given["photos"][0]["name"], json!("c.jpg"));
    assert_eq!(given["photos"][1]["n"], json!(2));
}

/// The grey level of a `#RRGGBB` mean (its red).
fn level(hex: &str) -> u8 {
    u8::from_str_radix(&hex[1..3], 16).unwrap()
}

/// A page, half dark grey and half light grey, on its one Background layer.
fn two_greys(t: &mut Setup) -> Value {
    let made = t.run(
        "file.new",
        json!({"widthPx": 80, "heightPx": 40, "background": "#A0A0A0"}),
    );
    t.run(
        "select.rect",
        json!({"rect": {"x": 0, "y": 0, "w": 40, "h": 40}}),
    );
    t.run("edit.fill", json!({"color": "#404040"}));
    t.run("select.none", json!({}));
    made
}

#[test]
fn tone_and_colour_are_adjusted_as_the_adjustments_menu_does() {
    let mut t = Setup::new();
    two_greys(&mut t);
    assert_eq!(t.mean(json!({"x": 5, "y": 5, "w": 10, "h": 10})), "#404040");
    t.run("image.adjust", json!({"adjustment": {"kind": "invert"}}));
    assert_eq!(t.mean(json!({"x": 5, "y": 5, "w": 10, "h": 10})), "#BFBFBF");
    t.run("edit.undo", json!({}));
    // Levels: 64 becomes black, 160 white.
    t.run(
        "image.adjust",
        json!({"adjustment": {"kind": "levels", "inBlack": 64, "inWhite": 160}}),
    );
    assert_eq!(t.mean(json!({"x": 5, "y": 5, "w": 10, "h": 10})), "#000000");
    assert_eq!(
        t.mean(json!({"x": 60, "y": 5, "w": 10, "h": 10})),
        "#FFFFFF"
    );
    t.run("edit.undo", json!({}));
    // A curve lifting the mids lightens the dark half.
    t.run(
        "image.adjust",
        json!({"adjustment": {"kind": "curves", "points": [[0, 0], [64, 128], [255, 255]]}}),
    );
    assert!(level(&t.mean(json!({"x": 5, "y": 5, "w": 10, "h": 10}))) > 0x70);
    for adjustment in [
        json!({"kind": "brightness_contrast", "brightness": 30}),
        json!({"kind": "hue_saturation", "saturation": -50}),
        json!({"kind": "vibrance", "vibrance": 20}),
        json!({"kind": "exposure", "exposure": 0.5}),
        json!({"kind": "color_balance", "midtones": [10, 0, -10]}),
        json!({"kind": "desaturate"}),
        json!({"kind": "threshold"}),
        json!({"kind": "posterize", "levels": 3}),
        json!({"kind": "black_and_white"}),
        json!({"kind": "photo_filter", "color": "#3366FF"}),
    ] {
        let done = t.run("image.adjust", json!({"adjustment": adjustment}));
        assert!(!done["step"].as_str().unwrap().is_empty(), "{adjustment}");
    }
    let e = t
        .try_run(
            "image.adjust",
            json!({"adjustment": {"kind": "levels", "inBlack": 200, "inWhite": 100}}),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
}

#[test]
fn auto_levels_stretches_a_dull_photo_end_to_end() {
    let mut t = Setup::new();
    two_greys(&mut t);
    t.run("image.auto_levels", json!({"clipPercent": 0}));
    assert_eq!(t.mean(json!({"x": 5, "y": 5, "w": 10, "h": 10})), "#000000");
    assert_eq!(
        t.mean(json!({"x": 60, "y": 5, "w": 10, "h": 10})),
        "#FFFFFF"
    );
    // One flat grey has nothing to stretch.
    t.run(
        "file.new",
        json!({"widthPx": 20, "heightPx": 20, "background": "#808080"}),
    );
    let e = t.try_run("image.auto_levels", json!({})).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotEnabled);
}

#[test]
fn a_filter_blurs_the_edge_and_only_inside_the_selection() {
    let mut t = Setup::new();
    two_greys(&mut t);
    t.run(
        "image.filter",
        json!({"filter": {"kind": "gaussian_blur", "radius": 4}}),
    );
    let edge = level(&t.mean(json!({"x": 38, "y": 10, "w": 4, "h": 10})));
    assert!((0x50..0x90).contains(&edge), "{edge:#x}");
    t.run("edit.undo", json!({}));
    // Selected: the left edge of the selection is the only edge that moves.
    t.run(
        "select.rect",
        json!({"rect": {"x": 0, "y": 0, "w": 20, "h": 40}}),
    );
    t.run(
        "image.filter",
        json!({"filter": {"kind": "gaussian_blur", "radius": 4}}),
    );
    assert_eq!(
        t.mean(json!({"x": 38, "y": 10, "w": 4, "h": 10})),
        "#707070"
    );
    for filter in [
        json!({"kind": "sharpen"}),
        json!({"kind": "high_pass", "radius": 3}),
        json!({"kind": "add_noise", "amount": 10}),
        json!({"kind": "pixelate", "cell": 4}),
        json!({"kind": "reduce_noise"}),
    ] {
        t.run("image.filter", json!({"filter": filter}));
    }
}

#[test]
fn a_photographed_page_comes_out_white_with_black_writing() {
    let mut t = Setup::new();
    // Greyish paper, darker at the right (uneven light), and a line of writing.
    t.run(
        "file.new",
        json!({"widthPx": 200, "heightPx": 120, "background": "#B4B4B4"}),
    );
    t.run(
        "select.rect",
        json!({"rect": {"x": 120, "y": 0, "w": 80, "h": 120}}),
    );
    t.run("edit.fill", json!({"color": "#9A9A9A"}));
    t.run(
        "select.rect",
        json!({"rect": {"x": 20, "y": 55, "w": 160, "h": 6}}),
    );
    t.run("edit.fill", json!({"color": "#202020"}));
    t.run("select.none", json!({}));
    t.run("image.scan_cleanup", json!({}));
    assert!(level(&t.mean(json!({"x": 10, "y": 10, "w": 30, "h": 30}))) >= 0xF0);
    assert!(level(&t.mean(json!({"x": 150, "y": 10, "w": 30, "h": 30}))) >= 0xF0);
    assert!(level(&t.mean(json!({"x": 40, "y": 56, "w": 100, "h": 4}))) <= 0x40);
}

#[test]
fn a_picture_is_placed_centred_in_its_box() {
    let dir = TempDir::new("place");
    let photo = dir.path("do.png");
    image::RgbImage::from_pixel(300, 100, image::Rgb([200, 30, 30]))
        .save(&photo)
        .unwrap();
    let mut t = Setup::new();
    t.run("file.new", json!({"widthPx": 1000, "heightPx": 1000}));
    // Whole page: as wide as the page, centred down it.
    let placed = t.run("layer.place", json!({"from": {"path": photo}}));
    let id = placed["layer"].clone();
    let bounds = |t: &mut Setup, id: &Value| {
        let info = t.inspect();
        let layer = info["layers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["id"] == *id)
            .unwrap()
            .clone();
        let b = &layer["bounds"];
        (
            b["x"].as_i64().unwrap(),
            b["y"].as_i64().unwrap(),
            b["w"].as_i64().unwrap(),
            b["h"].as_i64().unwrap(),
        )
    };
    assert_eq!(bounds(&mut t, &id), (0, 333, 1000, 333));
    assert_eq!(
        t.mean(json!({"x": 400, "y": 450, "w": 200, "h": 100})),
        "#C81E1E"
    );
    // A box with a margin, turned upright.
    let placed = t.run(
        "layer.place",
        json!({"from": {"path": photo}, "box": {"x": 100, "y": 100, "w": 300, "h": 500}, "marginPx": 50, "turn": 90}),
    );
    assert_eq!(bounds(&mut t, &placed["layer"]), (183, 150, 133, 400));
    // Cover fills the box; what overflows is cut.
    let placed = t.run(
        "layer.place",
        json!({"from": {"path": photo}, "box": {"x": 600, "y": 600, "w": 200, "h": 200}, "fit": "cover", "name": "Ô"}),
    );
    assert_eq!(bounds(&mut t, &placed["layer"]), (600, 600, 200, 200));
    // Another document's page.
    let other = t.run(
        "file.new",
        json!({"widthPx": 50, "heightPx": 50, "background": "#00FF00"}),
    )["doc"]
        .clone();
    let first = t.run("doc.list", json!({}))["docs"][0]["doc"].clone();
    let placed = t.run(
        "layer.place",
        json!({"doc": first, "from": {"doc": other}, "fit": "none"}),
    );
    assert_eq!(placed["doc"], first);
    let e = t
        .try_run(
            "layer.place",
            json!({"from": {"path": photo, "doc": other}}),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
}

#[test]
fn everyday_edits_fill_clear_edge_duplicate_merge_flatten_and_close() {
    let mut t = Setup::new();
    let made = t.run("file.new", json!({"widthPx": 60, "heightPx": 40}));
    let doc = made["doc"].clone();
    let count = |t: &mut Setup| t.inspect()["layers"].as_array().unwrap().len();
    t.run("layer.new", json!({"kind": "fill", "fill": "#3366CC"}));
    // Clear a corner of the blue layer: the white page shows through.
    t.run(
        "select.rect",
        json!({"rect": {"x": 0, "y": 0, "w": 20, "h": 20}}),
    );
    t.run("edit.clear", json!({}));
    assert_eq!(t.mean(json!({"x": 2, "y": 2, "w": 10, "h": 10})), "#FFFFFF");
    // Grow the selection, then fill it red.
    let grown = t.run("select.modify", json!({"change": "grow", "amount": 5}));
    assert_eq!(grown["bounds"]["w"], json!(25));
    t.run("edit.fill", json!({"color": "#FF0000"}));
    assert_eq!(t.mean(json!({"x": 2, "y": 2, "w": 10, "h": 10})), "#FF0000");
    t.run("select.modify", json!({"change": "feather", "amount": 3}));
    t.run("select.none", json!({}));
    let e = t
        .try_run("select.modify", json!({"change": "shrink", "amount": 2}))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotEnabled);
    // Duplicate, merge down, flatten.
    assert_eq!(count(&mut t), 2);
    t.run("layer.duplicate", json!({}));
    assert_eq!(count(&mut t), 3);
    t.run("layer.merge_down", json!({}));
    assert_eq!(count(&mut t), 2);
    t.run("layer.flatten", json!({}));
    assert_eq!(count(&mut t), 1);
    assert_eq!(
        t.mean(json!({"x": 40, "y": 30, "w": 10, "h": 5})),
        "#3366CC"
    );
    // Close: refused with changes, unless discarded; never as a dry run.
    let e = t.try_run("doc.close", json!({"doc": doc})).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotEnabled);
    let mut s = t.engine.session(&mut t.ws);
    let mut call = CommandCall::new("doc.close", json!({"doc": doc, "discard": true}));
    call.dry_run = true;
    assert_eq!(s.execute(call).unwrap_err().code, ErrorCode::BadParams);
    drop(s);
    let other = t.run("file.new", json!({"widthPx": 10, "heightPx": 10}))["doc"].clone();
    let closed = t.run("doc.close", json!({"doc": doc, "discard": true}));
    assert_eq!(closed["shown"], other);
    assert_eq!(
        t.run("doc.list", json!({}))["docs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn a_colour_is_selected_by_its_value_by_a_point_or_by_the_wand() {
    let mut t = Setup::new();
    two_greys(&mut t);
    let dark = t.run(
        "select.color_range",
        json!({"color": "#404040", "fuzziness": 10}),
    );
    assert_eq!(dark["bounds"], json!({"x": 0, "y": 0, "w": 40, "h": 40}));
    let light = t.run(
        "select.color_range",
        json!({"at": {"x": 70, "y": 5}, "fuzziness": 10}),
    );
    assert_eq!(light["bounds"]["x"], json!(40));
    t.run("select.none", json!({}));
    let wand = t.run("select.wand", json!({"at": {"x": 10, "y": 30}}));
    assert_eq!(wand["bounds"], json!({"x": 0, "y": 0, "w": 40, "h": 40}));
    let both = t.run(
        "select.wand",
        json!({"at": {"x": 70, "y": 30}, "mode": "add"}),
    );
    assert_eq!(both["bounds"]["w"], json!(80));
    let e = t
        .try_run("select.wand", json!({"at": {"x": 500, "y": 5}}))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
    let e = t
        .try_run(
            "select.color_range",
            json!({"color": "#FFFFFF", "at": {"x": 1, "y": 1}}),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
}

#[test]
fn a_colour_page_turns_grey() {
    let mut t = Setup::new();
    t.run(
        "file.new",
        json!({"widthPx": 20, "heightPx": 20, "background": "#C83214"}),
    );
    t.run("image.grayscale", json!({}));
    let mean = t.mean(json!({"x": 0, "y": 0, "w": 20, "h": 20}));
    assert_eq!(&mean[1..3], &mean[3..5]);
    assert_eq!(&mean[3..5], &mean[5..7]);
}

#[test]
fn a_selection_becomes_a_layer_and_a_mask_hides_and_is_applied() {
    let mut t = Setup::new();
    two_greys(&mut t);
    t.run(
        "select.rect",
        json!({"rect": {"x": 0, "y": 0, "w": 10, "h": 10}}),
    );
    let copied = t.run("layer.via_copy", json!({}));
    let layers = copied["layers"].as_array().unwrap();
    assert_eq!(layers.len(), 2);
    let id = layers[0]["id"].clone();
    assert_eq!(copied["layer"], id);
    // As Ctrl+J: the selection is dropped once copied.
    assert_eq!(t.inspect()["selection"]["active"], json!(false));
    // One undo step takes the copy and the drop back together.
    t.run("edit.undo", json!({}));
    let info = t.inspect();
    assert_eq!(info["layers"].as_array().unwrap().len(), 1);
    assert_eq!(info["selection"]["active"], json!(true));
    t.run("layer.via_copy", json!({}));
    // Hide all of it: the grey below shows; apply: its pixels are gone.
    t.run("edit.fill", json!({"layer": id, "color": "#FF0000"}));
    assert_eq!(t.mean(json!({"x": 2, "y": 2, "w": 6, "h": 6})), "#FF0000");
    t.run("layer.mask", json!({"layer": id, "op": "hide_all"}));
    assert_eq!(t.mean(json!({"x": 2, "y": 2, "w": 6, "h": 6})), "#404040");
    let e = t
        .try_run("layer.mask", json!({"layer": id, "op": "reveal_all"}))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotEnabled);
    t.run("layer.mask", json!({"layer": id, "op": "apply"}));
    assert_eq!(t.mean(json!({"x": 2, "y": 2, "w": 6, "h": 6})), "#404040");
    let e = t
        .try_run("layer.mask", json!({"layer": id, "op": "delete"}))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotEnabled);
    // Nothing selected: the whole layer is copied.
    let whole = t.run("layer.via_copy", json!({}));
    assert_eq!(whole["layers"].as_array().unwrap().len(), 3);
}

#[test]
fn free_transform_scales_distorts_and_goes_back_to_the_first_pixels() {
    let mut t = Setup::new();
    t.run(
        "file.new",
        json!({"widthPx": 120, "heightPx": 100, "background": "#FFFFFF"}),
    );
    // A red bar 20 × 10 at (10, 20), on a layer of its own.
    let layer = t.run("layer.new", json!({}))["layer"].as_u64().unwrap();
    t.run(
        "select.rect",
        json!({"rect": {"x": 10, "y": 20, "w": 20, "h": 10}}),
    );
    t.run("edit.fill", json!({"color": "#FF0000"}));
    t.run("select.none", json!({}));

    // Twice as large round its middle (20, 25): 40 × 20 from (0, 15).
    let out = t.run(
        "layer.free_transform",
        json!({"layers": [layer], "scaleX": 2, "scaleY": 2}),
    );
    assert_eq!(out["changed"], json!(true));
    assert_eq!(out["layers"], json!([layer]));
    assert_eq!(
        t.mean(json!({"x": 2, "y": 17, "w": 36, "h": 16})),
        "#FF0000"
    );
    // Back to half: resampled from the first pixels, the bar is as sharp as
    // it was, edge to edge.
    t.run(
        "layer.free_transform",
        json!({"layers": [layer], "scaleX": 0.5, "scaleY": 0.5}),
    );
    assert_eq!(
        t.mean(json!({"x": 10, "y": 20, "w": 20, "h": 10})),
        "#FF0000"
    );
    assert_eq!(
        t.mean(json!({"x": 31, "y": 20, "w": 8, "h": 10})),
        "#FFFFFF"
    );
    // Undo gives back the bar as the first transform left it, not the first
    // pixels the second one was drawn from.
    t.run("edit.undo", json!({}));
    assert_eq!(
        t.mean(json!({"x": 2, "y": 17, "w": 36, "h": 16})),
        "#FF0000"
    );
    t.run("edit.redo", json!({}));
    assert_eq!(
        t.mean(json!({"x": 31, "y": 20, "w": 8, "h": 10})),
        "#FFFFFF"
    );

    // A move alone is done at once; nothing at all changes nothing.
    let moved = t.run("layer.free_transform", json!({"layers": [layer], "dx": 50}));
    assert_eq!(moved["changed"], json!(true));
    assert_eq!(
        t.mean(json!({"x": 60, "y": 20, "w": 20, "h": 10})),
        "#FF0000"
    );
    let none = t.run("layer.free_transform", json!({"layers": [layer]}));
    assert_eq!(none["changed"], json!(false));

    // Its corners sent out to a wider box: a distort.
    t.run(
        "layer.free_transform",
        json!({"layers": [layer], "corners": [[50, 10], [100, 10], [100, 40], [50, 40]]}),
    );
    assert_eq!(
        t.mean(json!({"x": 52, "y": 12, "w": 46, "h": 26})),
        "#FF0000"
    );
    // One undo step each.
    t.run("edit.undo", json!({}));
    assert_eq!(
        t.mean(json!({"x": 60, "y": 20, "w": 20, "h": 10})),
        "#FF0000"
    );

    // The Background and nonsense are refused.
    let e = t
        .try_run("layer.free_transform", json!({"layers": [0], "scaleX": 2}))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotEnabled);
    let e = t
        .try_run(
            "layer.free_transform",
            json!({"layers": [layer], "scaleX": 0}),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
}

#[test]
fn a_shape_drawn_between_pixels_keeps_its_span_and_its_corners() {
    let mut t = Setup::new();
    t.run(
        "file.new",
        json!({"widthPx": 120, "heightPx": 100, "background": "#FFFFFF"}),
    );
    let drawn = t.run(
        "vector.add_shape",
        json!({
            "shape": "rectangle",
            "span": [10.5, 20.25, 60.5, 70.25],
            "fill": "#0000FFFF",
            "cornerRadius": 8,
            "corners": "chamfer",
        }),
    );
    let layer = drawn["layer"].as_u64().unwrap();
    assert_eq!(
        t.mean(json!({"x": 20, "y": 30, "w": 30, "h": 30})),
        "#0000FF"
    );
    // A chamfer cuts the corner straight: its very corner is white.
    assert_eq!(t.mean(json!({"x": 11, "y": 21, "w": 1, "h": 1})), "#FFFFFF");
    let info = t.inspect();
    let shape = info["layers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["id"].as_u64() == Some(layer))
        .unwrap();
    assert_eq!(shape["kind"], json!("vector"), "{shape}");

    let e = t
        .try_run(
            "vector.add_shape",
            json!({"shape": "ellipse", "fill": "#000000"}),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
    let e = t
        .try_run(
            "vector.add_shape",
            json!({
                "shape": "ellipse",
                "fill": "#000000",
                "rect": {"x": 0, "y": 0, "w": 5, "h": 5},
                "span": [0, 0, 5, 5],
            }),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
}

#[test]
fn a_type_spec_carries_every_style_of_a_type_layer() {
    use iai_doc::model::text::{GlyphStyle, TextAlign, TextData, TextFontFamily};
    let data = TextData {
        content: "Xin chào\nbạn".to_string(),
        font_px: 37.5,
        color: [10, 20, 30, 200],
        font_family: TextFontFamily::System("Arial".to_string()),
        align: TextAlign::Center,
        line_height: 1.35,
        bold: true,
        italic: false,
        underline: true,
        tracking_px: 1.25,
        opacity: 0.8,
        stretch_x: 1.1,
        rotation_deg: -12.5,
        flip_x: true,
        flip_y: false,
        glyph_styles: (0..12)
            .map(|i| GlyphStyle {
                color: [i, 0, 0, 255],
                font_px: 30.0 + f32::from(i),
                font_family: if i % 2 == 0 {
                    TextFontFamily::Tahoma
                } else {
                    TextFontFamily::SystemFace {
                        family: "Times New Roman".to_string(),
                        style: "Bold".to_string(),
                    }
                },
                bold: i % 3 == 0,
                italic: true,
                underline: false,
            })
            .collect(),
    };
    let spec = super::text::TypeSpec::of(&data);
    let json = serde_json::to_value(&spec).unwrap();
    let back: super::text::TypeSpec = serde_json::from_value(json).unwrap();
    assert_eq!(back.data(), data);
}

#[test]
fn text_set_writes_a_new_type_layer_then_changes_it_one_step_each() {
    let mut t = Setup::new();
    t.run(
        "file.new",
        json!({"widthPx": 200, "heightPx": 100, "background": "#FFFFFF"}),
    );
    let spec = |text: &str, color: [u8; 4]| {
        json!({
            "text": text, "sizePx": 40, "color": color,
            "font": {"kind": "builtin", "name": "Arial"}, "align": "left",
            "lineHeight": 1.2, "bold": false, "italic": false, "underline": false,
            "trackingPx": 0, "opacity": 1, "stretchX": 1, "rotation": 0,
            "flipX": false, "flipY": false,
        })
    };
    let made = t.run(
        "text.set",
        json!({"type": spec("AB", [0, 0, 0, 255]), "origin": [10, 10]}),
    );
    let layer = made["layer"].as_u64().unwrap();
    let dark = t.mean(json!({"x": 10, "y": 10, "w": 60, "h": 40}));
    assert_ne!(dark, "#FFFFFF");
    t.run(
        "text.set",
        json!({"layer": layer, "type": spec("AB", [255, 0, 0, 255]), "origin": [10, 10]}),
    );
    let info = t.inspect();
    let names: Vec<&str> = info["layers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["name"].as_str().unwrap())
        .collect();
    // Changed in place: still one type layer, named by its text.
    assert_eq!(names, ["AB", "Background"]);
    // Undo gives back the black type, then no type at all.
    t.run("edit.undo", json!({}));
    assert_eq!(t.mean(json!({"x": 10, "y": 10, "w": 60, "h": 40})), dark);
    t.run("edit.undo", json!({}));
    assert_eq!(
        t.mean(json!({"x": 10, "y": 10, "w": 60, "h": 40})),
        "#FFFFFF"
    );
    // Not on a picture layer.
    let e = t
        .try_run(
            "text.set",
            json!({"layer": 0, "type": spec("x", [0, 0, 0, 255]), "origin": [0, 0]}),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotEnabled);
}

#[test]
fn a_step_a_tool_made_reaches_the_engine_and_a_commands_own_does_not_twice() {
    let mut t = Setup::new();
    t.run("file.new", json!({"widthPx": 40, "heightPx": 40}));
    t.engine.take_events();
    // As a brush stroke is recorded: straight into the page's history.
    {
        let canvas = &mut t.ws.documents_mut()[0].canvas;
        let (w, h) = (canvas.width, canvas.height);
        let mut step = iai_history::command::LayerStructureCommand::capture_before(
            "Brush",
            &canvas.layer_stack,
            w,
            h,
        );
        step.capture_after(&canvas.layer_stack, w, h);
        canvas.record(Box::new(step));
    }
    // The next call hears of it first; its own step is not told again.
    t.run("layer.new", json!({}));
    let steps: Vec<String> = t
        .engine
        .take_events()
        .into_iter()
        .filter_map(|e| match e {
            EngineEvent::Stepped { label, .. } => Some(label),
            _ => None,
        })
        .collect();
    assert_eq!(steps, ["Brush"]);
    t.engine.session(&mut t.ws).take_window_steps();
    assert!(
        !t.engine
            .take_events()
            .iter()
            .any(|e| matches!(e, EngineEvent::Stepped { .. }))
    );
}

#[test]
fn free_transform_keeps_a_path_a_path_and_refuses_to_distort_it() {
    let mut t = Setup::new();
    t.run(
        "file.new",
        json!({"widthPx": 200, "heightPx": 200, "background": "#FFFFFF"}),
    );
    let path = t.run(
        "vector.add_path",
        json!({"runs": [{"points": [[10, 10], [50, 10], [50, 50], [10, 50]], "closed": true}], "fill": "#0000FF"}),
    )["layer"]
        .as_u64()
        .unwrap();
    t.run(
        "layer.free_transform",
        json!({"layers": [path], "scaleX": 2, "scaleY": 2, "pivot": [10, 10]}),
    );
    // Twice as large from its top-left corner: blue to (90, 90), still a path.
    assert_eq!(
        t.mean(json!({"x": 12, "y": 12, "w": 75, "h": 75})),
        "#0000FF"
    );
    assert_eq!(
        t.mean(json!({"x": 95, "y": 95, "w": 20, "h": 20})),
        "#FFFFFF"
    );
    let info = t.inspect();
    let kind = info["layers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["id"].as_u64() == Some(path))
        .map(|l| l["kind"].clone())
        .unwrap();
    assert_eq!(kind, json!("vector"));
    let e = t
        .try_run(
            "layer.free_transform",
            json!({"layers": [path], "corners": [[0, 0], [100, 10], [100, 100], [0, 90]]}),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotEnabled);
}

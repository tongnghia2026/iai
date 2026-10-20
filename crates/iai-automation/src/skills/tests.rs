//! The skill library: what an agent did, generalized; the library's rules
//! for keeping it; and recipes run on an engine with no window.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use iai_engine::{CommandCall, Engine, ErrorCode, Policy, Source};
use serde_json::{json, Value};

use super::learn::{generalize, Recorded};
use super::*;
use crate::gate::{Gate, Roots};
use crate::host::{Host, LocalHost};

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let d = std::env::temp_dir().join(format!("iai-skills-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Self(std::fs::canonicalize(&d).map(|p| plain(&p)).unwrap())
    }
    fn path(&self, rest: &str) -> String {
        self.0.join(rest).display().to_string()
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn plain(p: &Path) -> PathBuf {
    let t = p.display().to_string();
    PathBuf::from(t.strip_prefix(r"\\?\").unwrap_or(&t))
}

fn call(command: &str, args: Value, result: Value) -> Recorded {
    Recorded {
        command: command.to_string(),
        args,
        result: Some(result),
        writes: matches!(command, "file.export" | "export.pdf" | "file.save"),
        query: matches!(command, "doc.list" | "doc.inspect" | "analyze.region"),
        ..Recorded::default()
    }
}

/// An agent's ID-photo errand: each photo made and exported, then the
/// sheets laid out and each written as a PDF.
fn id_photo_errand(input: &Dir, output: &Dir, photos: &[&str], sheets: usize) -> Vec<Recorded> {
    let mut calls = Vec::new();
    for (k, photo) in photos.iter().enumerate() {
        let doc = k + 1;
        calls.push(call(
            "file.open",
            json!({"path": input.path(photo)}),
            json!({"doc": doc, "pages": 1}),
        ));
        calls.push(call(
            "id_photo.make",
            json!({"doc": doc, "size": "3x4", "backdrop": "blue"}),
            json!({"doc": doc, "widthPx": 354}),
        ));
        calls.push(call(
            "doc.list",
            json!({}),
            json!({"documents": [{"doc": doc}]}),
        ));
        let stem = Path::new(photo)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .to_string();
        calls.push(call(
            "file.export",
            json!({"doc": doc, "path": output.path(&format!("{stem}.png"))}),
            json!({"doc": doc, "path": output.path(&format!("{stem}.png"))}),
        ));
    }
    let sheet_docs: Vec<Value> = (0..sheets)
        .map(|s| json!({"doc": 100 + s, "copies": 4}))
        .collect();
    calls.push(call(
        "print.impose_sheet",
        json!({"source": {"kind": "folder", "path": output.0.display().to_string()},
               "sheet": {"kind": "grid", "paper": "10x15", "photo": "3x4"}, "copies": 2}),
        json!({"sheets": sheet_docs}),
    ));
    for s in 0..sheets {
        let path = output.path(&format!("to_in_{}.pdf", s + 1));
        calls.push(call(
            "export.pdf",
            json!({"doc": 100 + s, "path": path}),
            json!({"path": path, "pages": 1}),
        ));
    }
    calls
}

fn touch(dir: &Dir, names: &[&str]) {
    for n in names {
        std::fs::write(dir.0.join(n), b"x").unwrap();
    }
}

fn named(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

#[test]
fn an_id_photo_errand_becomes_loops_over_the_folder_and_the_sheets() {
    let (input, output) = (Dir::new("in"), Dir::new("out"));
    touch(&input, &["an.jpg", "binh.jpg", "chi.jpg"]);
    let calls = id_photo_errand(&input, &output, &["an.jpg", "binh.jpg", "chi.jpg"], 2);
    let d = generalize(
        &calls,
        &named(&[("size", json!("3x4")), ("copies", json!(2))]),
    )
    .unwrap();
    assert_eq!(
        d.signature,
        "each(file.open,id_photo.make,file.export),print.impose_sheet,each(export.pdf)"
    );
    let steps = serde_json::to_value(&d.steps).unwrap();
    assert_eq!(
        steps,
        json!([
            {"id": "l1", "foreach": {"files": "${vao}", "ext": ["jpg"]}, "as": "f", "steps": [
                {"id": "s1", "command": "file.open", "args": {"path": "${vao}/${f.name}"}},
                {"id": "s2", "command": "id_photo.make",
                 "args": {"doc": {"$ref": "s1", "path": "/doc"}, "size": "${size}", "backdrop": "blue"}},
                {"id": "s3", "command": "file.export",
                 "args": {"doc": {"$ref": "s1", "path": "/doc"}, "path": "${ra}/${f.stem}.png"}}
            ]},
            {"id": "s4", "command": "print.impose_sheet",
             "args": {"source": {"kind": "folder", "path": "${ra}"},
                      "sheet": {"kind": "grid", "paper": "10x15", "photo": "${size}"}, "copies": "${copies}"}},
            {"id": "l2", "foreach": {"items": {"$ref": "s4", "path": "/sheets"}}, "as": "it", "steps": [
                {"id": "s5", "command": "export.pdf",
                 "args": {"doc": "${it.doc}", "path": "${ra}/to_in_${n}.pdf"}}
            ]}
        ]),
        "{}",
        serde_json::to_string_pretty(&steps).unwrap()
    );
    let names: Vec<&String> = d.params.keys().collect();
    assert_eq!(names, ["copies", "ra", "size", "vao"]);
    assert!(d.params["ra"].write && !d.params["vao"].write);
    assert_eq!(
        d.params["vao"].default,
        json!(input.0.display().to_string())
    );
    assert_eq!(d.params["copies"].default, json!(2));
}

#[test]
fn a_folder_with_other_files_loops_over_the_named_ones_and_one_sheet_still_loops() {
    let (input, output) = (Dir::new("in2"), Dir::new("out2"));
    touch(&input, &["an.jpg", "binh.jpg", "extra.jpg"]);
    let calls = id_photo_errand(&input, &output, &["an.jpg", "binh.jpg"], 1);
    let d = generalize(&calls, &BTreeMap::new()).unwrap();
    let first = d.steps.first().unwrap().foreach.clone().unwrap();
    assert_eq!(first.names, ["an.jpg", "binh.jpg"]);
    // The one sheet's export is a loop over however many sheets come out.
    let last = d.steps.last().unwrap();
    assert_eq!(
        last.foreach.as_ref().unwrap().items,
        json!({"$ref": "s4", "path": "/sheets"})
    );
    assert_eq!(last.steps[0].args["doc"], json!("${it.doc}"));
    // Not named, the size stays as it was.
    assert_eq!(d.steps[0].steps[1].args["size"], json!("3x4"));
}

#[test]
fn pages_drawn_alike_become_a_loop_as_long_as_the_pdf() {
    let (input, output) = (Dir::new("in3"), Dir::new("out3"));
    touch(&input, &["thiep.pdf"]);
    let mut calls = vec![call(
        "file.open",
        json!({"path": input.path("thiep.pdf"), "dpi": 300}),
        json!({"doc": 1, "pages": 3}),
    )];
    let flower =
        json!({"points": [[1.0, 2.0], [3.0, 4.0], [5.0, 1.0]], "fill": "#33aa33", "smooth": true});
    for page in 0..3 {
        calls.push(call(
            "page.select",
            json!({"page": page}),
            json!({"doc": 1, "page": page, "pages": 3}),
        ));
        calls.push(call(
            "vector.add_path",
            flower.clone(),
            json!({"doc": 1, "layer": 10 + 2 * page}),
        ));
        calls.push(call(
            "vector.add_path",
            flower.clone(),
            json!({"doc": 1, "layer": 11 + 2 * page}),
        ));
    }
    let out = output.path("thiep_hoa.pdf");
    calls.push(call(
        "export.pdf",
        json!({"path": out}),
        json!({"path": out, "pages": 3}),
    ));
    let d = generalize(&calls, &BTreeMap::new()).unwrap();
    assert_eq!(
        d.signature,
        "file.open,each(page.select,vector.add_path,vector.add_path),export.pdf"
    );
    let steps = serde_json::to_value(&d.steps).unwrap();
    assert_eq!(steps[0]["args"], json!({"path": "${tep_vao}", "dpi": 300}));
    assert_eq!(
        steps[1]["foreach"],
        json!({"count": {"$ref": "s1", "path": "/pages"}})
    );
    assert_eq!(steps[1]["steps"][0]["args"], json!({"page": "${i}"}));
    assert_eq!(steps[2]["args"], json!({"path": "${tep_ra}"}));
    let names: Vec<&String> = d.params.keys().collect();
    assert_eq!(names, ["tep_ra", "tep_vao"]);
    assert_eq!(
        d.params["tep_vao"].default,
        json!(format!(
            "{}/thiep.pdf",
            input.0.display().to_string().replace('\\', "/")
        ))
    );
}

#[test]
fn what_changes_without_a_reason_stays_as_it_was_done() {
    let calls = vec![
        call(
            "file.new",
            json!({"widthPx": 100, "heightPx": 100}),
            json!({"doc": 1}),
        ),
        call(
            "vector.add_shape",
            json!({"shape": "ellipse", "rect": {"x": 3, "y": 9, "w": 10, "h": 10}}),
            json!({"doc": 1}),
        ),
        call(
            "vector.add_shape",
            json!({"shape": "ellipse", "rect": {"x": 41, "y": 2, "w": 10, "h": 10}}),
            json!({"doc": 1}),
        ),
        call("doc.inspect", json!({}), json!({"doc": 1})),
    ];
    let d = generalize(&calls, &BTreeMap::new()).unwrap();
    assert_eq!(d.signature, "file.new,vector.add_shape,vector.add_shape");
    assert!(d.params.is_empty());
    // Only queries: nothing to keep.
    assert!(generalize(&calls[3..], &BTreeMap::new()).is_none());
}

fn lesson(request: &str) -> Lesson {
    Lesson {
        request: request.to_string(),
        app: "test".to_string(),
        client: "tester 1".to_string(),
        ..Lesson::default()
    }
}

#[test]
fn the_library_counts_a_job_again_learns_new_values_and_keeps_new_jobs_apart() {
    let lib = Dir::new("lib");
    let store = Store::new(&lib.0);
    let (input, output) = (Dir::new("in4"), Dir::new("out4"));
    touch(&input, &["an.jpg", "binh.jpg"]);
    let first = generalize(
        &id_photo_errand(&input, &output, &["an.jpg", "binh.jpg"], 1),
        &named(&[("size", json!("3x4"))]),
    )
    .unwrap();
    let saved = store
        .learn(
            first,
            &lesson("Làm ảnh thẻ 3x4 nền xanh cho cả thư mục, xếp tờ 10x15"),
        )
        .unwrap();
    let Saved::Created(name) = saved.clone() else {
        panic!("{saved:?}")
    };
    assert_eq!(name, "lam-anh-the-3x4-nen-xanh-cho-ca-thu-muc-xep-to");
    let dir = lib.0.join(&name);
    let md = std::fs::read_to_string(dir.join("SKILL.md")).unwrap();
    assert!(
        md.starts_with(&format!("---\nname: {name}\ndescription: ")),
        "{md}"
    );
    assert!(md.contains("| `size` | text | `3x4` |"), "{md}");
    assert!(std::fs::read_to_string(dir.join("lich_su.md"))
        .unwrap()
        .contains("tạo"));
    assert_eq!(store.load(&name).unwrap().meta.status, Status::AutoSaved);

    // The same job on other folders: one more use, and these folders next time.
    let (in2, out2) = (Dir::new("in5"), Dir::new("out5"));
    touch(&in2, &["x.jpg", "y.jpg"]);
    let again = generalize(
        &id_photo_errand(&in2, &out2, &["x.jpg", "y.jpg"], 1),
        &named(&[("size", json!("3x4"))]),
    )
    .unwrap();
    assert_eq!(
        store
            .learn(again, &lesson("ảnh thẻ cho thư mục khác"))
            .unwrap(),
        Saved::Reused(name.clone())
    );
    let r = store.load(&name).unwrap();
    assert_eq!((r.meta.uses, r.meta.status), (1, Status::InUse));
    assert_eq!(r.params["vao"].default, json!(in2.0.display().to_string()));
    assert_eq!(r.meta.examples.len(), 2);

    // Same job, white backdrop: the backdrop becomes a param, no new skill.
    let mut white = id_photo_errand(&in2, &out2, &["x.jpg", "y.jpg"], 1);
    for c in white.iter_mut().filter(|c| c.command == "id_photo.make") {
        c.args["backdrop"] = json!("white");
    }
    let white = generalize(&white, &named(&[("size", json!("3x4"))])).unwrap();
    assert_eq!(
        store.learn(white, &lesson("ảnh thẻ nền trắng")).unwrap(),
        Saved::Updated(name.clone(), vec!["backdrop".to_string()])
    );
    let r = store.load(&name).unwrap();
    assert_eq!(r.params["backdrop"].default, json!("blue"));
    assert_eq!(r.steps[0].steps[1].args["backdrop"], json!("${backdrop}"));
    assert_eq!(store.list().len(), 1);
    // Done again with a literal where the skill now has its param: the same job.
    let mut yellow = id_photo_errand(&in2, &out2, &["x.jpg", "y.jpg"], 1);
    for c in yellow.iter_mut().filter(|c| c.command == "id_photo.make") {
        c.args["backdrop"] = json!("yellow");
    }
    let yellow = generalize(&yellow, &named(&[("size", json!("3x4"))])).unwrap();
    assert_eq!(
        store.learn(yellow, &lesson("ảnh thẻ nền vàng")).unwrap(),
        Saved::Reused(name.clone())
    );
    assert_eq!(store.list().len(), 1);

    // Another job altogether: a skill of its own.
    let cutout = vec![
        call(
            "file.open",
            json!({"path": in2.path("x.jpg")}),
            json!({"doc": 7}),
        ),
        call("ai.remove_background", json!({"doc": 7}), json!({"doc": 7})),
        call(
            "file.export",
            json!({"doc": 7, "path": out2.path("x.png")}),
            json!({"path": out2.path("x.png")}),
        ),
    ];
    let saved = store
        .learn(
            generalize(&cutout, &BTreeMap::new()).unwrap(),
            &lesson("Tách nền ảnh sản phẩm, xuất PNG nền trong"),
        )
        .unwrap();
    assert!(matches!(saved, Saved::Created(_)), "{saved:?}");
    assert_eq!(store.list().len(), 2);

    // Found by what people say, marks or not.
    let found = store.search("lam anh the nen xanh", 3);
    assert_eq!(found.first().map(|f| f.name.as_str()), Some(name.as_str()));
    let found = store.search("tách nền", 3);
    assert_eq!(
        found.first().map(|f| f.name.clone()),
        Some(saved.name().to_string())
    );
    assert!(store.search("in hóa đơn bán hàng", 3).is_empty());
    assert_eq!(store.search("", 10).len(), 2);

    // Removed: out of the library, kept aside.
    let aside = store.remove(saved.name()).unwrap();
    assert!(aside.starts_with(lib.0.join(REMOVED)) && aside.join("kich_ban.json").is_file());
    assert_eq!(store.list().len(), 1);
    assert_eq!(
        store.remove("khong-co").unwrap_err().code,
        ErrorCode::NotFound
    );
    assert_eq!(store.load("../x").unwrap_err().code, ErrorCode::NotFound);
}

#[test]
fn a_job_close_to_a_kept_one_is_saved_as_its_version() {
    let lib = Dir::new("lib2");
    let store = Store::new(&lib.0);
    let (input, output) = (Dir::new("in6"), Dir::new("out6"));
    touch(&input, &["an.jpg", "binh.jpg"]);
    let base = generalize(
        &id_photo_errand(&input, &output, &["an.jpg", "binh.jpg"], 1),
        &BTreeMap::new(),
    )
    .unwrap();
    let base = store.learn(base, &lesson("làm ảnh thẻ xếp tờ in")).unwrap();
    // The same, with each photo made smaller before it is exported.
    let mut calls = id_photo_errand(&input, &output, &["an.jpg", "binh.jpg"], 1);
    let at: Vec<usize> = calls
        .iter()
        .enumerate()
        .filter(|(_, c)| c.command == "file.export")
        .map(|(i, _)| i)
        .collect();
    for i in at.into_iter().rev() {
        let doc = calls[i].args["doc"].clone();
        calls.insert(
            i,
            call(
                "image.resize",
                json!({"doc": doc, "widthPx": 300}),
                json!({"doc": doc}),
            ),
        );
    }
    let draft = generalize(&calls, &BTreeMap::new()).unwrap();
    let saved = store
        .learn(draft, &lesson("làm ảnh thẻ nhỏ hơn rồi xếp tờ in"))
        .unwrap();
    assert_eq!(
        saved,
        Saved::Version(format!("{}-2", base.name()), base.name().to_string())
    );
    assert_eq!(
        store.load(saved.name()).unwrap().meta.based_on.as_deref(),
        Some(base.name())
    );
}

// ---- running ----------------------------------------------------------------

fn host() -> LocalHost {
    LocalHost::new(Engine::new(
        iai_engine::cmds::core_registry(),
        Source::Agent,
        Policy::agent(),
    ))
}

fn gate(read: &Dir, write: &Dir) -> Gate {
    let mut g = Gate::new(
        Policy::agent(),
        Roots::new(&[read.0.clone()], &[write.0.clone()]).unwrap(),
    );
    g.learn(&iai_engine::cmds::core_registry().list());
    g
}

/// Two small PNGs in `dir`, made by the engine.
fn make_pngs(host: &mut LocalHost, dir: &Dir) {
    for (name, w) in [("a.png", 40), ("b.png", 60)] {
        host.execute(CommandCall::new(
            "file.new",
            json!({"widthPx": w, "heightPx": 30}),
        ))
        .unwrap();
        host.execute(CommandCall::new(
            "file.export",
            json!({"path": dir.path(name)}),
        ))
        .unwrap();
    }
}

fn flip_recipe() -> Recipe {
    serde_json::from_value(json!({
        "version": 1,
        "meta": {"title": "Lật ảnh", "description": "Lật ngang mọi ảnh PNG trong thư mục", "tier": 1},
        "params": {
            "vao": {"type": "folder", "default": null, "label": "Thư mục ảnh"},
            "ra": {"type": "folder", "default": null, "label": "Thư mục ghi", "write": true},
            "duoi": {"type": "text", "default": "_lat"}
        },
        "steps": [
            {"id": "l1", "foreach": {"files": "${vao}", "ext": ["png"]}, "as": "f", "steps": [
                {"id": "s1", "command": "file.open", "args": {"path": "${f.path}"}},
                {"id": "s2", "command": "image.flip", "args": {"doc": {"$ref": "s1", "path": "/doc"}, "axis": "horizontal"}},
                {"id": "s3", "command": "file.export", "args": {"doc": {"$ref": "s1", "path": "/doc"}, "path": "${ra}/${f.stem}${duoi}.png"}}
            ]}
        ]
    }))
    .unwrap()
}

#[test]
fn a_skill_runs_its_loop_over_the_folder_and_stops_and_goes_on_when_time_is_up() {
    let (input, output, lib) = (Dir::new("in7"), Dir::new("out7"), Dir::new("lib3"));
    let mut host = host();
    make_pngs(&mut host, &input);
    let store = Store::new(&lib.0);
    store.write("lat-anh", &flip_recipe()).unwrap();
    let g = gate(&input, &output);
    let params =
        json!({"vao": input.0.display().to_string(), "ra": output.0.display().to_string()});
    let (mut run, plan) = Run::start(&store, &g, "lat-anh", &params).unwrap();
    assert_eq!(plan["loops"][0]["files"], json!(2));
    // No time at all: it stops before doing anything, then goes on.
    let past = std::time::Instant::now();
    assert!(matches!(
        run.advance(&mut host, &g, &store, Some(past)),
        Ok(Progress::Paused)
    ));
    assert_eq!(run.calls, 0);
    assert!(matches!(
        run.advance(&mut host, &g, &store, None),
        Ok(Progress::Done)
    ));
    assert_eq!(run.calls, 6);
    assert_eq!(run.written.len(), 2);
    assert!(output.0.join("a_lat.png").is_file() && output.0.join("b_lat.png").is_file());

    // Params: an unknown one, a missing one, a folder outside the grant.
    let e = Run::start(
        &store,
        &g,
        "lat-anh",
        &json!({"vao": "x", "ra": "y", "bogus": 1}),
    )
    .err()
    .unwrap();
    assert_eq!(e.code, ErrorCode::BadParams);
    let e = Run::start(
        &store,
        &g,
        "lat-anh",
        &json!({"ra": output.0.display().to_string()}),
    )
    .err()
    .unwrap();
    assert!(e.message.contains("vao"), "{}", e.message);
    let elsewhere = std::env::temp_dir().display().to_string();
    let e = Run::start(
        &store,
        &g,
        "lat-anh",
        &json!({"vao": elsewhere, "ra": output.0.display().to_string()}),
    )
    .err()
    .unwrap();
    assert_eq!(e.code, ErrorCode::PermissionDenied);

    // A command this iAi does not have: the skill says it needs fixing.
    let mut broken = flip_recipe();
    broken.steps[0].steps[1].command = Some("image.vanish".to_string());
    store.write("hong", &broken).unwrap();
    let e = Run::start(&store, &g, "hong", &params).err().unwrap();
    assert_eq!(e.code, ErrorCode::NotFound);
    assert!(e.message.contains("needs fixing"), "{}", e.message);
}

#[test]
fn a_skill_runs_another_and_its_step_ids_are_its_own() {
    let (input, output, lib) = (Dir::new("in8"), Dir::new("out8"), Dir::new("lib4"));
    let mut host = host();
    make_pngs(&mut host, &input);
    let store = Store::new(&lib.0);
    store.write("lat-anh", &flip_recipe()).unwrap();
    let outer: Recipe = serde_json::from_value(json!({
        "version": 1,
        "meta": {"title": "Lật rồi làm trang", "description": "x"},
        "params": {"vao": {"type": "folder", "default": null}, "ra": {"type": "folder", "default": null, "write": true}},
        "steps": [
            {"id": "s1", "command": "file.new", "args": {"widthPx": 20, "heightPx": 20}},
            {"id": "s2", "skill": "lat-anh", "params": {"vao": "${vao}", "ra": "${ra}", "duoi": "_2"}},
            {"id": "s3", "command": "file.export", "args": {"doc": {"$ref": "s1", "path": "/doc"}, "path": "${ra}/trang.png"}}
        ]
    }))
    .unwrap();
    store.write("lat-roi-trang", &outer).unwrap();
    let g = gate(&input, &output);
    let params =
        json!({"vao": input.0.display().to_string(), "ra": output.0.display().to_string()});
    let (mut run, _) = Run::start(&store, &g, "lat-roi-trang", &params).unwrap();
    assert!(matches!(
        run.advance(&mut host, &g, &store, None),
        Ok(Progress::Done)
    ));
    // s1 of the inner skill did not stand in for the outer one's s1.
    assert_eq!(run.calls, 2 + 6);
    assert!(output.0.join("a_2.png").is_file() && output.0.join("trang.png").is_file());
}

/// How an agent really did it on 20/10 (4f-b, B1): it laid out the folder
/// on a sheet only to look at it, opened every portrait, then made every ID
/// photo, then exported every one, then laid out and wrote the sheets, then
/// opened the PDFs again to check them.
#[test]
fn an_agent_working_in_phases_is_kept_as_one_loop_per_file() {
    let (input, output) = (Dir::new("in9"), Dir::new("out9"));
    let photos = ["an.jpg", "binh.jpg", "chi.jpg"];
    touch(&input, &photos);
    let mut calls = vec![call(
        "print.impose_sheet",
        json!({"source": {"kind": "folder", "path": input.0.display().to_string()},
               "sheet": {"kind": "grid", "paper": "10x15", "photo": "3x4"}, "copies": 1}),
        json!({"sheets": [{"doc": 1}, {"doc": 2}]}),
    )];
    calls.push(call("doc.inspect", json!({"doc": 1}), json!({"doc": 1})));
    for (k, photo) in photos.iter().enumerate() {
        calls.push(call(
            "file.open",
            json!({"path": input.path(photo)}),
            json!({"doc": k + 3}),
        ));
    }
    for k in 0..photos.len() {
        calls.push(call(
            "id_photo.make",
            json!({"doc": k + 3, "size": "3x4", "backdrop": "blue"}),
            json!({"doc": k + 3}),
        ));
    }
    for (k, photo) in photos.iter().enumerate() {
        let stem = photo.trim_end_matches(".jpg");
        let path = output.path(&format!("anh_the_{stem}.jpg"));
        calls.push(call(
            "file.export",
            json!({"doc": k + 3, "path": path}),
            json!({"doc": k + 3, "path": path}),
        ));
    }
    calls.push(call(
        "print.impose_sheet",
        json!({"source": {"kind": "folder", "path": output.0.display().to_string()},
               "sheet": {"kind": "grid", "paper": "10x15", "photo": "3x4"}, "copies": 2}),
        json!({"sheets": [{"doc": 20}]}),
    ));
    let pdf = output.path("to_in_1.pdf");
    calls.push(call(
        "export.pdf",
        json!({"doc": 20, "path": pdf}),
        json!({"path": pdf}),
    ));
    calls.push(call("file.open", json!({"path": pdf}), json!({"doc": 21})));
    let d = generalize(&calls, &BTreeMap::new()).unwrap();
    assert_eq!(
        d.signature,
        "each(file.open,id_photo.make,file.export),print.impose_sheet,each(export.pdf)"
    );
    let body = &d.steps[0].steps;
    assert_eq!(body[1].args["doc"], json!({"$ref": "s1", "path": "/doc"}));
    assert_eq!(body[2].args["path"], json!("${ra}/anh_the_${f.name}"));
}

#[test]
fn a_loop_whose_early_rounds_are_read_later_is_left_as_it_was() {
    // Two pages made, then the first one written: folding the two "file.new"
    // into a loop would keep only the second page's answer.
    let out = Dir::new("out10");
    let calls = vec![
        call(
            "file.new",
            json!({"widthPx": 10, "heightPx": 10}),
            json!({"doc": 1}),
        ),
        call(
            "file.new",
            json!({"widthPx": 10, "heightPx": 10}),
            json!({"doc": 2}),
        ),
        call(
            "file.export",
            json!({"doc": 1, "path": out.path("a.png")}),
            json!({"path": out.path("a.png")}),
        ),
        call(
            "layer.new",
            json!({"doc": 2}),
            json!({"doc": 2, "layer": 7}),
        ),
    ];
    let d = generalize(&calls, &BTreeMap::new()).unwrap();
    assert_eq!(d.signature, "file.new,file.new,file.export,layer.new");
    assert_eq!(
        d.steps[2].args["doc"],
        json!({"$ref": "s1", "path": "/doc"})
    );
}

/// Opt-in: learn a skill again from a real agent's transcript (Claude
/// Code's `--output-format stream-json`), as `iai_task_end` would have:
/// `IAI_RELEARN_JSONL=<file> IAI_RELEARN_LIB=<library>`. The skill the agent
/// named, when there, is set aside first.
#[test]
#[ignore]
fn probe_relearn_from_a_transcript() {
    let (Ok(log), Ok(lib)) = (
        std::env::var("IAI_RELEARN_JSONL"),
        std::env::var("IAI_RELEARN_LIB"),
    ) else {
        return;
    };
    let registry = iai_engine::cmds::core_registry().list();
    let mut ids: Vec<String> = registry.iter().map(|c| c.id.clone()).collect();
    ids.extend(["id_photo.make", "print.impose_sheet"].map(str::to_string));
    let info = |id: &str| registry.iter().find(|c| c.id == id);
    let mut uses: BTreeMap<String, (String, Value)> = BTreeMap::new();
    let mut answers: BTreeMap<String, (Value, bool)> = BTreeMap::new();
    let mut order = Vec::new();
    for line in std::fs::read_to_string(&log).unwrap().lines() {
        let Ok(ev) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let content = ev
            .pointer("/message/content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for c in content {
            match c["type"].as_str() {
                Some("tool_use") => {
                    let id = c["id"].as_str().unwrap_or("").to_string();
                    let name = c["name"]
                        .as_str()
                        .unwrap_or("")
                        .trim_start_matches("mcp__iai__")
                        .to_string();
                    uses.insert(id.clone(), (name, c["input"].clone()));
                    order.push(id);
                }
                Some("tool_result") => {
                    let text = match &c["content"] {
                        Value::String(s) => s.clone(),
                        Value::Array(a) => a
                            .iter()
                            .filter_map(|x| x["text"].as_str())
                            .collect::<Vec<_>>()
                            .join(""),
                        _ => String::new(),
                    };
                    let v = serde_json::from_str(&text).unwrap_or(Value::Null);
                    let id = c["tool_use_id"].as_str().unwrap_or("").to_string();
                    answers.insert(id, (v, c["is_error"].as_bool().unwrap_or(false)));
                }
                _ => {}
            }
        }
    }
    let record = |command: &str, args: &Value, result: Option<Value>| {
        let i = info(command);
        Recorded {
            command: command.to_string(),
            args: args.clone(),
            result,
            query: i.is_some_and(|i| {
                i.effects
                    .iter()
                    .all(|e| matches!(e, iai_engine::Effect::Query | iai_engine::Effect::ReadFiles))
            }),
            writes: i.is_some_and(|i| i.effects.contains(&iai_engine::Effect::WriteFiles)),
            ..Recorded::default()
        }
    };
    let (mut calls, mut request, mut end) = (Vec::new(), String::new(), Value::Null);
    for id in order {
        let (name, input) = uses[&id].clone();
        let (answer, failed) = answers.get(&id).cloned().unwrap_or((Value::Null, true));
        match name.as_str() {
            "iai_task_begin" => request = input["request"].as_str().unwrap_or("").to_string(),
            "iai_task_end" => end = input.clone(),
            _ if failed => {}
            "iai_command_run" => {
                let c = input["command"].as_str().unwrap_or("");
                calls.push(record(c, &input["args"], answer.get("result").cloned()));
            }
            "iai_command_batch" => {
                for (k, c) in input["calls"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .enumerate()
                {
                    if let Some(done) = answer["completed"].get(k) {
                        calls.push(record(
                            c["command"].as_str().unwrap_or(""),
                            &c["args"],
                            done.get("result").cloned(),
                        ));
                    }
                }
            }
            other => {
                if let Some(cmd) = ids.iter().find(|i| crate::mcp::tool_name(i) == other) {
                    calls.push(record(cmd, &input, answer.get("result").cloned()));
                }
            }
        }
    }
    let named: BTreeMap<String, Value> = end["params"]
        .as_object()
        .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    let draft = generalize(&calls, &named).unwrap();
    println!("calls {} -> signature {}", calls.len(), draft.signature);
    let store = Store::new(&lib);
    let text = |k: &str| end[k].as_str().unwrap_or("").to_string();
    if let Ok(aside) = store.remove(&text::slug(&text("name"))) {
        println!("set aside the old one: {}", aside.display());
    }
    let lesson = Lesson {
        request,
        name: text("name"),
        title: text("title"),
        description: text("description"),
        description_en: text("descriptionEn"),
        checks: end["checks"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        app: "học lại từ bản ghi".to_string(),
        client: "relearn".to_string(),
        ..Lesson::default()
    };
    println!("{:?}", store.learn(draft, &lesson).unwrap());
}

/// B2b1 (20/10): the agent opened an ID photo only to look at it, then wrote
/// the sheet it had made before; and the task ran a kept skill and did more.
#[test]
fn a_photo_opened_to_look_at_is_not_kept_and_a_skill_with_more_is_its_version() {
    let (lib, out) = (Dir::new("lib5"), Dir::new("out11"));
    let store = Store::new(&lib.0);
    store.write("lat-anh", &flip_recipe()).unwrap();
    let pdf = out.path("to.pdf");
    let mut calls = vec![Recorded {
        skill: Some((
            "lat-anh".to_string(),
            json!({"vao": out.0.display().to_string()}),
        )),
        result: Some(json!({"calls": 6})),
        writes: true,
        ..Recorded::default()
    }];
    calls.push(call(
        "file.new",
        json!({"widthPx": 10, "heightPx": 10}),
        json!({"doc": 1}),
    ));
    calls.push(call(
        "file.open",
        json!({"path": out.path("a_lat.png")}),
        json!({"doc": 2}),
    ));
    calls.push(call(
        "analyze.region",
        json!({"doc": 2}),
        json!({"mean": "#FFFFFF"}),
    ));
    calls.push(call(
        "export.pdf",
        json!({"doc": 1, "path": pdf}),
        json!({"path": pdf}),
    ));
    let d = generalize(&calls, &BTreeMap::new()).unwrap();
    assert_eq!(d.signature, "skill:lat-anh,file.new,export.pdf");
    let saved = store
        .learn(d, &lesson("lật ảnh rồi làm một trang PDF"))
        .unwrap();
    assert_eq!(
        saved,
        Saved::Version("lat-anh-2".to_string(), "lat-anh".to_string())
    );
}

/// C1 (20/10): ID cards with one skill, ID photos with another, in one task:
/// a use of each, nothing new kept.
#[test]
fn a_task_of_saved_skills_one_after_another_is_a_use_of_each() {
    let (lib, out) = (Dir::new("lib6"), Dir::new("out12"));
    let store = Store::new(&lib.0);
    store.write("lat-anh", &flip_recipe()).unwrap();
    store.write("lat-anh-nua", &flip_recipe()).unwrap();
    let ran = |name: &str| Recorded {
        skill: Some((
            name.to_string(),
            json!({"vao": out.0.display().to_string()}),
        )),
        result: Some(json!({"calls": 2})),
        writes: true,
        ..Recorded::default()
    };
    let mut calls = vec![ran("lat-anh"), ran("lat-anh-nua")];
    // Looking at the result is no step of the job.
    calls.push(Recorded {
        query: true,
        ..call("doc.preview", json!({"doc": 1}), json!({"doc": 1}))
    });
    let d = generalize(&calls, &BTreeMap::new()).unwrap();
    assert_eq!(
        store.learn(d, &lesson("lật ảnh rồi lật nữa")).unwrap(),
        Saved::Reused("lat-anh".to_string())
    );
    assert_eq!(store.list().len(), 2);
    for name in ["lat-anh", "lat-anh-nua"] {
        assert_eq!(store.load(name).unwrap().meta.uses, 1, "{name}");
    }
}

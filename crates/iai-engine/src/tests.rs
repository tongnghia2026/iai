//! The dispatcher, driven by small commands made for these tests.

use std::time::Duration;

use iai_canvas::document::Document;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::*;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct AddParams {
    doc: Option<u32>,
    #[serde(default)]
    fail_after: bool,
    #[serde(default)]
    panic_after: bool,
    /// Layers to add.
    #[serde(default = "one")]
    #[schemars(range(min = 1, max = 3))]
    count: u32,
}

fn one() -> u32 {
    1
}

impl Validate for AddParams {}

#[derive(Serialize, JsonSchema)]
struct Layers {
    layers: usize,
}

static ADD: CommandMeta = CommandMeta {
    id: "t.add_layer",
    label_vi: "Thêm layer",
    label_en: "Add layer",
    doc: "test",
    effects: &[Effect::Edit],
    featured: false,
};

struct AddLayer;

impl Command for AddLayer {
    type Params = AddParams;
    type Output = Layers;
    fn meta(&self) -> &'static CommandMeta {
        &ADD
    }
    fn plan(&self, s: &mut Session<'_>, p: AddParams) -> Result<Plan<Layers>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        for _ in 0..p.count {
            canvas.add_layer();
        }
        let layers = canvas.layer_stack.layers.len();
        if p.fail_after {
            return Err(CmdError::new(ErrorCode::NotEnabled, "asked to fail"));
        }
        if p.panic_after {
            panic!("asked to panic");
        }
        s.emit(EngineEvent::Structure { doc });
        Ok(Plan::Done(Layers { layers }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoParams {}
impl Validate for NoParams {}

static COUNT: CommandMeta = CommandMeta {
    id: "t.count",
    label_vi: "Đếm",
    label_en: "Count",
    doc: "test",
    effects: &[Effect::Query],
    featured: false,
};

struct Count;

impl Command for Count {
    type Params = NoParams;
    type Output = Layers;
    fn meta(&self) -> &'static CommandMeta {
        &COUNT
    }
    fn plan(&self, s: &mut Session<'_>, _: NoParams) -> Result<Plan<Layers>, CmdError> {
        let layers = s.canvas(None)?.layer_stack.layers.len();
        Ok(Plan::Done(Layers { layers }))
    }
}

static PRINT: CommandMeta = CommandMeta {
    id: "t.print",
    label_vi: "In",
    label_en: "Print",
    doc: "test",
    effects: &[Effect::Query, Effect::Print],
    featured: false,
};

struct Print;

impl Command for Print {
    type Params = NoParams;
    type Output = Value;
    fn meta(&self) -> &'static CommandMeta {
        &PRINT
    }
    fn plan(&self, _: &mut Session<'_>, _: NoParams) -> Result<Plan<Value>, CmdError> {
        Ok(Plan::Done(Value::Null))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SlowParams {
    #[serde(default)]
    wait_for_cancel: bool,
    #[serde(default)]
    fail: bool,
}
impl Validate for SlowParams {}

static SLOW: CommandMeta = CommandMeta {
    id: "t.slow",
    label_vi: "Chậm",
    label_en: "Slow",
    doc: "test",
    effects: &[Effect::Edit, Effect::LocalAi],
    featured: false,
};

struct Slow;

impl Command for Slow {
    type Params = SlowParams;
    type Output = Layers;
    fn meta(&self) -> &'static CommandMeta {
        &SLOW
    }
    fn plan(&self, s: &mut Session<'_>, p: SlowParams) -> Result<Plan<Layers>, CmdError> {
        let doc = s.doc_id(None)?;
        Ok(Plan::Job(Job {
            doc: Some(doc),
            label_vi: "chậm".into(),
            work: Box::new(move |cx| {
                cx.progress(0.5, "đang làm");
                if p.wait_for_cancel {
                    while !cx.cancelled() {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    cx.check_cancelled()?;
                }
                if p.fail {
                    return Err(CmdError::new(ErrorCode::ModelMissing, "no model"));
                }
                Ok(Box::new(move |s: &mut Session<'_>| {
                    let canvas = s.canvas_mut(Some(doc))?;
                    canvas.add_layer();
                    Ok(Layers {
                        layers: canvas.layer_stack.layers.len(),
                    })
                }) as Apply<Layers>)
            }),
        }))
    }
}

static OPEN: CommandMeta = CommandMeta {
    id: "t.open_then_fail",
    label_vi: "Mở",
    label_en: "Open",
    doc: "test",
    effects: &[Effect::Edit],
    featured: false,
};

struct OpenThenFail;

impl Command for OpenThenFail {
    type Params = NoParams;
    type Output = Value;
    fn meta(&self) -> &'static CommandMeta {
        &OPEN
    }
    fn plan(&self, s: &mut Session<'_>, _: NoParams) -> Result<Plan<Value>, CmdError> {
        let id = s.new_document_id();
        s.add_document(Document::new(id, 8, 8));
        Err(CmdError::new(ErrorCode::Io, "disk full"))
    }
}

fn registry() -> Registry {
    let mut r = Registry::new();
    r.register(AddLayer);
    r.register(Count);
    r.register(Print);
    r.register(Slow);
    r.register(OpenThenFail);
    r
}

fn setup() -> (Engine, HeadlessWorkspace) {
    let engine = Engine::new(registry(), Source::Test, Policy::all());
    let mut ws = HeadlessWorkspace::new();
    let id = ws.new_document_id();
    ws.add(Document::new(id, 16, 16));
    (engine, ws)
}

fn layers(engine: &mut Engine, ws: &mut HeadlessWorkspace) -> u64 {
    let reply = engine
        .session(ws)
        .execute(CommandCall::new("t.count", json!({})))
        .unwrap();
    reply.result().unwrap()["layers"].as_u64().unwrap()
}

#[test]
fn every_test_command_is_registered_once() {
    let r = registry();
    assert!(r.duplicates().is_empty());
    let mut again = registry();
    again.register(Count);
    assert_eq!(again.duplicates(), &["t.count"]);
    assert_eq!(r.len(), 5);
    let listed = r.list();
    let add = listed.iter().find(|c| c.id == "t.add_layer").unwrap();
    assert_eq!(add.params_schema["additionalProperties"], json!(false));
    assert_eq!(
        add.params_schema["properties"]["count"]["maximum"],
        json!(3)
    );
    assert!(add.can_dry_run);
}

#[test]
fn an_unknown_command_or_field_or_value_is_refused() {
    let (mut engine, mut ws) = setup();
    let mut s = engine.session(&mut ws);
    let e = s
        .execute(CommandCall::new("t.nope", json!({})))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::UnknownCommand);
    let e = s
        .execute(CommandCall::new("t.add_layer", json!({"cuont": 2})))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
    assert!(e.message.contains("cuont"), "{}", e.message);
    let e = s
        .execute(CommandCall::new("t.add_layer", json!({"count": 4})))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
    assert!(e.message.contains("1 to 3"), "{}", e.message);
    let e = s
        .execute(CommandCall::new("t.add_layer", json!({"doc": 99})))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NoDocument);
}

#[test]
fn a_change_raises_the_revision_and_says_what_to_redraw() {
    let (mut engine, mut ws) = setup();
    let before = engine.session(&mut ws).revision(1);
    let reply = engine
        .session(&mut ws)
        .execute(CommandCall::new("t.add_layer", json!({"count": 2})))
        .unwrap();
    let Reply::Done { result, revision } = reply else {
        panic!("not done")
    };
    assert_eq!(result["layers"], json!(3));
    assert!(revision > before);
    assert_eq!(
        engine.take_events(),
        vec![EngineEvent::Structure { doc: 1 }]
    );
    // Reading it changes nothing.
    assert_eq!(layers(&mut engine, &mut ws), 3);
    assert_eq!(engine.session(&mut ws).revision(1), revision);
}

#[test]
fn a_command_that_fails_or_panics_half_way_leaves_the_document_as_it_was() {
    let (mut engine, mut ws) = setup();
    let e = engine
        .session(&mut ws)
        .execute(CommandCall::new(
            "t.add_layer",
            json!({"count": 2, "failAfter": true}),
        ))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotEnabled);
    assert_eq!(layers(&mut engine, &mut ws), 1);
    assert_eq!(engine.session(&mut ws).revision(1), 0);
    let canvas = &ws.documents()[0].canvas;
    assert!(!canvas.can_undo() && !canvas.can_redo());

    let e = engine
        .session(&mut ws)
        .execute(CommandCall::new("t.add_layer", json!({"panicAfter": true})))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Internal);
    assert!(e.message.contains("panicked"), "{}", e.message);
    assert!(!catching_panics());
    assert_eq!(layers(&mut engine, &mut ws), 1);
    assert!(!ws.documents()[0].canvas.can_redo());
}

#[test]
fn a_retried_request_runs_once() {
    let (mut engine, mut ws) = setup();
    let mut call = CommandCall::new("t.add_layer", json!({}));
    call.request_id = Some("abc".into());
    let first = engine.session(&mut ws).execute(call.clone()).unwrap();
    let second = engine.session(&mut ws).execute(call).unwrap();
    assert_eq!(first, second);
    assert_eq!(layers(&mut engine, &mut ws), 2);
}

#[test]
fn a_stale_revision_is_refused() {
    let (mut engine, mut ws) = setup();
    let rev = engine.session(&mut ws).revision(1);
    engine
        .session(&mut ws)
        .execute(CommandCall::new("t.add_layer", json!({})))
        .unwrap();
    let mut call = CommandCall::new("t.add_layer", json!({"doc": 1}));
    call.expect_revision = Some(rev);
    let e = engine.session(&mut ws).execute(call.clone()).unwrap_err();
    assert_eq!(e.code, ErrorCode::StaleRevision);
    assert_eq!(e.hint.unwrap()["command"], json!("doc.inspect"));
    call.expect_revision = Some(engine.session(&mut ws).revision(1));
    engine.session(&mut ws).execute(call).unwrap();
    assert_eq!(layers(&mut engine, &mut ws), 3);
}

#[test]
fn a_dry_run_reports_and_takes_the_change_back() {
    let (mut engine, mut ws) = setup();
    let rev = engine.session(&mut ws).revision(1);
    let mut call = CommandCall::new("t.add_layer", json!({"count": 3}));
    call.dry_run = true;
    let reply = engine.session(&mut ws).execute(call).unwrap();
    assert_eq!(reply.result().unwrap()["layers"], json!(4));
    assert_eq!(layers(&mut engine, &mut ws), 1);
    assert!(!ws.documents()[0].canvas.can_redo());
    // The page is drawn again; nothing else is announced.
    let events = engine.take_events();
    assert!(events.contains(&EngineEvent::Structure { doc: 1 }));
    assert!(!events
        .iter()
        .any(|e| matches!(e, EngineEvent::Notice { .. })));
    // Back as it was, at the revision it was.
    assert_eq!(engine.session(&mut ws).revision(1), rev);
}

#[test]
fn printing_needs_the_effect_granted() {
    let (mut engine, mut ws) = setup();
    engine.set_policy(Policy::agent());
    let e = engine
        .session(&mut ws)
        .execute(CommandCall::new("t.print", json!({})))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied);
    assert_eq!(e.hint.unwrap()["relaunchWith"], json!("--allow-print"));
    engine.set_policy(Policy::agent().grant(Effect::Print));
    engine
        .session(&mut ws)
        .execute(CommandCall::new("t.print", json!({})))
        .unwrap();
    let mut call = CommandCall::new("t.print", json!({}));
    call.dry_run = true;
    let e = engine.session(&mut ws).execute(call).unwrap_err();
    assert_eq!(e.code, ErrorCode::BadParams);
}

#[test]
fn an_atomic_batch_takes_everything_back_when_a_step_fails() {
    let (mut engine, mut ws) = setup();
    let calls = vec![
        CommandCall::new("t.add_layer", json!({})),
        CommandCall::new("t.add_layer", json!({"count": 2})),
        CommandCall::new("t.add_layer", json!({"count": 9})),
    ];
    let reply = engine.session(&mut ws).execute_batch(calls.clone(), true);
    assert!(reply.rolled_back && reply.completed.is_empty());
    assert_eq!(reply.failed.as_ref().unwrap().index, 2);
    assert_eq!(layers(&mut engine, &mut ws), 1);

    let reply = engine.session(&mut ws).execute_batch(calls, false);
    assert!(!reply.rolled_back);
    assert_eq!(reply.completed.len(), 2);
    assert_eq!(layers(&mut engine, &mut ws), 4);
}

#[test]
fn a_document_opened_by_a_failing_command_is_closed_again() {
    let (mut engine, mut ws) = setup();
    let e = engine
        .session(&mut ws)
        .execute(CommandCall::new("t.open_then_fail", json!({})))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Io);
    assert_eq!(ws.documents().len(), 1);
    assert_eq!(ws.active(), Some(0));
}

#[test]
fn a_job_runs_in_the_background_and_lands_on_poll() {
    let (mut engine, mut ws) = setup();
    let reply = engine
        .session(&mut ws)
        .execute(CommandCall::new("t.slow", json!({})).in_background())
        .unwrap();
    let Reply::Started { job } = reply else {
        panic!("not started")
    };
    let state = engine
        .session(&mut ws)
        .wait(job, Duration::from_secs(10))
        .unwrap();
    let JobState::Done { result, .. } = state else {
        panic!("{state:?}")
    };
    assert_eq!(result["layers"], json!(2));
    assert!(!engine.busy());
    let events = engine.take_events();
    assert!(matches!(
        events.first(),
        Some(EngineEvent::JobStarted { .. })
    ));
    assert!(matches!(
        events.last(),
        Some(EngineEvent::JobFinished {
            error: None,
            cancelled: false,
            ..
        })
    ));
    // Without `background`, the same command runs to the end at once.
    let reply = engine
        .session(&mut ws)
        .execute(CommandCall::new("t.slow", json!({})))
        .unwrap();
    assert_eq!(reply.result().unwrap()["layers"], json!(3));
}

#[test]
fn a_cancelled_job_changes_nothing_and_one_job_works_on_a_document() {
    let (mut engine, mut ws) = setup();
    let Reply::Started { job } = engine
        .session(&mut ws)
        .execute(CommandCall::new("t.slow", json!({"waitForCancel": true})).in_background())
        .unwrap()
    else {
        panic!("not started")
    };
    let e = engine
        .session(&mut ws)
        .execute(CommandCall::new("t.slow", json!({})).in_background())
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Busy);
    assert!(engine.cancel(job));
    let state = engine
        .session(&mut ws)
        .wait(job, Duration::from_secs(10))
        .unwrap();
    assert_eq!(state, JobState::Cancelled);
    assert_eq!(layers(&mut engine, &mut ws), 1);
}

#[test]
fn an_agent_waits_for_a_job_before_changing_its_document() {
    let (mut engine, mut ws) = setup();
    engine.set_source(Source::Agent);
    let Reply::Started { job } = engine
        .session(&mut ws)
        .execute(CommandCall::new("t.slow", json!({"waitForCancel": true})).in_background())
        .unwrap()
    else {
        panic!("not started")
    };
    let e = engine
        .session(&mut ws)
        .execute(CommandCall::new("t.add_layer", json!({})))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Busy);
    assert_eq!(e.hint, Some(json!({"tool": "job_status"})));
    // Reading is fine; the person at the window is left to judge.
    assert_eq!(layers(&mut engine, &mut ws), 1);
    engine.set_source(Source::Gui);
    assert!(engine
        .session(&mut ws)
        .execute(CommandCall::new("t.add_layer", json!({})))
        .is_ok());
    assert!(engine.cancel(job));
    engine.session(&mut ws).wait(job, Duration::from_secs(10));
    engine.set_source(Source::Agent);
    assert!(engine
        .session(&mut ws)
        .execute(CommandCall::new("t.add_layer", json!({})))
        .is_ok());
}

#[test]
fn a_failing_job_reports_why() {
    let (mut engine, mut ws) = setup();
    let Reply::Started { job } = engine
        .session(&mut ws)
        .execute(CommandCall::new("t.slow", json!({"fail": true})).in_background())
        .unwrap()
    else {
        panic!("not started")
    };
    let state = engine
        .session(&mut ws)
        .wait(job, Duration::from_secs(10))
        .unwrap();
    let JobState::Failed { error } = state else {
        panic!("{state:?}")
    };
    assert_eq!(error.code, ErrorCode::ModelMissing);
}

//! The window's side of the command engine (`iai-engine`): the app lends its
//! tabs to the engine for each call, runs commands for the person at the
//! window, polls the background jobs every frame, and turns what the engine
//! reports into GPU uploads and the status line.

use iai_engine::{
    BatchReply, CmdError, CommandCall, EngineEvent, JobId, Policy, Registry, Reply, Source,
    Workspace,
};
use serde_json::Value;

use super::document_session::DocumentSession;
use super::render::CanvasEvent;
use super::state::App;
use crate::core::document::{Document, DocumentId};

/// The app's tabs, lent to the engine.
pub(in crate::app) struct GuiWorkspace<'a> {
    docs: &'a mut DocumentSession,
    /// The only tab is the welcome page: the first document opened takes its
    /// place instead of a tab of its own.
    over_welcome: bool,
}

impl Workspace for GuiWorkspace<'_> {
    fn documents(&self) -> &[Document] {
        self.docs.tabs.documents()
    }

    fn documents_mut(&mut self) -> &mut [Document] {
        self.docs.tabs.documents_mut()
    }

    fn active(&self) -> Option<usize> {
        self.docs.tabs.active()
    }

    fn set_active(&mut self, index: usize) {
        self.docs.tabs.set_active(index);
    }

    fn new_document_id(&mut self) -> DocumentId {
        self.docs.tabs.new_document_id()
    }

    fn add(&mut self, doc: Document) -> usize {
        let tabs = &mut self.docs.tabs;
        if std::mem::take(&mut self.over_welcome) && tabs.documents.len() == 1 {
            tabs.documents[0] = doc;
            tabs.active_doc_idx = 0;
            0
        } else {
            tabs.add(doc)
        }
    }

    fn remove(&mut self, index: usize) -> Option<Document> {
        self.docs.tabs.remove(index)
    }
}

/// Every command this build has: the free core's, and the Pro pack's.
pub(crate) fn command_registry() -> Registry {
    #[cfg_attr(not(feature = "pro"), allow(unused_mut))]
    let mut registry = iai_engine::cmds::core_registry();
    #[cfg(feature = "pro")]
    crate::pro::cmds::register(&mut registry);
    registry
}

/// The engine of the window: the person may do everything.
pub(crate) fn new_engine() -> iai_engine::Engine {
    iai_engine::Engine::new(command_registry(), Source::Gui, Policy::all())
}

/// The commands that make their change of the whole page in a job: the
/// window runs them in the background and holds the tab meanwhile.
pub(crate) const PAGE_JOBS: &[&str] = &["image.crop", "image.resize", "image.perspective_crop"];

/// What a failed command says on the status line.
pub(crate) fn error_line(e: &CmdError) -> String {
    e.message_vi.clone().unwrap_or_else(|| e.message.clone())
}

impl App {
    /// Run `call` for the person at the window, on the document shown unless
    /// it names another. What it changed is redrawn and what it says goes to
    /// the status line, a failure included.
    pub(crate) fn execute_command(&mut self, call: CommandCall) -> Result<Reply, CmdError> {
        let command = call.command.clone();
        self.prepare_for_command();
        let over_welcome = self.has_only_welcome_placeholder();
        let reply = {
            let mut ws = GuiWorkspace {
                docs: &mut self.docs,
                over_welcome,
            };
            self.engine.session(&mut ws).execute(call)
        };
        self.apply_engine_events();
        if let Err(e) = &reply {
            self.shell.status_msg = error_line(e);
        }
        // The Channels panel shows the channel picked; it changed with them.
        if command.starts_with("channel.") || command == "select.from_channel" {
            self.refresh_channel_view_display();
        }
        if command == "page.delete" || command == "page.move" {
            self.shell.ui.page_rename_target = None;
        }
        reply
    }

    /// Several calls in order, on the tabs (`atomic`: all or none), as the
    /// engine runs a batch; what they changed is redrawn.
    pub(crate) fn execute_batch(&mut self, calls: Vec<CommandCall>, atomic: bool) -> BatchReply {
        self.prepare_for_command();
        let over_welcome = self.has_only_welcome_placeholder();
        let reply = {
            let mut ws = GuiWorkspace {
                docs: &mut self.docs,
                over_welcome,
            };
            self.engine.session(&mut ws).execute_batch(calls, atomic)
        };
        self.apply_engine_events();
        if let Some(f) = &reply.failed {
            self.shell.status_msg = error_line(&f.error);
        }
        reply
    }

    /// Run command `id` on the document shown, with no params, to the end.
    pub(crate) fn run_now(&mut self, id: &str) -> Result<Reply, CmdError> {
        self.execute_command(CommandCall::new(id, serde_json::json!({})))
    }

    /// Run command `id` with `args` to the end.
    pub(crate) fn run_with(&mut self, id: &str, args: Value) -> Result<Reply, CmdError> {
        self.execute_command(CommandCall::new(id, args))
    }

    /// Run command `id` with `args`; a long one in the background. `None`
    /// when it failed (the status line says why).
    pub(crate) fn start_command(&mut self, id: &str, args: Value) -> Option<Reply> {
        self.execute_command(CommandCall::new(id, args).in_background())
            .ok()
    }

    /// Before a command: what the window holds that the documents do not yet
    /// (the view of the tab shown, a brush stroke on the GPU, a vector style
    /// being previewed) goes into them, as a tab switch does.
    fn prepare_for_command(&mut self) {
        self.sync_brush_gpu_to_cpu();
        self.path_style_commit();
        let (zoom, x, y) = (
            self.edit.view.zoom,
            self.edit.view.offset_x,
            self.edit.view.offset_y,
        );
        let doc = self.docs.active_mut();
        doc.saved_zoom = zoom;
        doc.saved_offset_x = x;
        doc.saved_offset_y = y;
    }

    /// Every frame: the steps the window's tools made since the last frame
    /// go to the engine (the journal, and its events).
    pub(crate) fn hand_steps_to_engine(&mut self) {
        let over_welcome = self.has_only_welcome_placeholder();
        let mut ws = GuiWorkspace {
            docs: &mut self.docs,
            over_welcome,
        };
        self.engine.session(&mut ws).take_window_steps();
        self.apply_engine_events();
    }

    /// Every frame: apply the jobs whose work has ended.
    pub(crate) fn poll_engine(&mut self) {
        if !self.engine.busy() {
            return;
        }
        let ended = self.engine.ended_jobs();
        if ended.is_empty() {
            return;
        }
        self.before_transform_lands(&ended);
        #[cfg(feature = "pro")]
        self.before_pro_jobs_land(&ended);
        let over_welcome = self.has_only_welcome_placeholder();
        {
            let mut ws = GuiWorkspace {
                docs: &mut self.docs,
                over_welcome,
            };
            self.engine.session(&mut ws).poll();
        }
        self.apply_engine_events();
    }

    /// A command job is running.
    pub(crate) fn engine_busy(&self) -> bool {
        self.engine.busy()
    }

    /// A page-wide job (see [`PAGE_JOBS`]) works on the tab shown: what is
    /// painted there meanwhile would be lost under its result.
    pub(crate) fn page_job_holds_shown_tab(&self) -> bool {
        let shown = self.docs.active().id.0;
        self.engine
            .working_on(shown)
            .is_some_and(|command| PAGE_JOBS.contains(&command))
    }

    /// Redraw what the engine changed, and say what it said.
    fn apply_engine_events(&mut self) {
        let events = self.engine.take_events();
        if events.is_empty() {
            return;
        }
        let active = self.docs.active().id.0;
        let (mut refresh, mut fit) = (false, false);
        let (mut structure, mut pixels, mut selection) = (false, false, false);
        let mut finished = Vec::new();
        for event in events {
            match event {
                EngineEvent::Pixels { doc } => pixels |= doc == active,
                EngineEvent::Selection { doc } if doc == active => selection = true,
                EngineEvent::Selection { doc } => {
                    if let Some(d) = self.docs.by_id_mut(DocumentId(doc)) {
                        d.canvas.selection.refresh_bbox();
                    }
                }
                EngineEvent::Structure { doc } => structure |= doc == active,
                EngineEvent::Opened { doc } => {
                    self.shell.ui.show_welcome = false;
                    // A tab opened behind the one shown waits to be looked at.
                    if doc == active {
                        self.edit.input.painting = false;
                        self.edit.transform_state = None;
                        self.edit.pending_stroke_inputs.clear();
                        refresh = true;
                    }
                }
                EngineEvent::Shown { .. } => refresh = true,
                EngineEvent::PageShown { doc } => {
                    refresh |= doc == active;
                    fit |= doc == active;
                }
                EngineEvent::Saved {
                    doc,
                    path,
                    complete,
                } => {
                    if let Some(idx) = self.docs.index_of(DocumentId(doc)) {
                        if idx == self.docs.active_doc_idx {
                            self.docs.current_file = Some(path.into());
                        }
                        // Work done while it was written is not in the file:
                        // its recovery copy stays.
                        if complete {
                            self.clear_autosave(idx);
                        }
                    }
                }
                EngineEvent::Notice { vi, .. } => self.shell.status_msg = vi,
                EngineEvent::Stepped { .. } => {}
                EngineEvent::JobStarted { label_vi, .. } => {
                    self.shell.status_msg = format!("{label_vi}…");
                }
                EngineEvent::JobFinished {
                    job,
                    command,
                    error,
                    cancelled,
                } => {
                    if let Some(e) = &error {
                        self.shell.status_msg = error_line(e);
                    } else if cancelled {
                        self.shell.status_msg = "Đã hủy".to_string();
                    }
                    finished.push((job, command, error));
                }
            }
        }
        if refresh {
            self.show_new_tab_or_page(fit);
        } else {
            if structure {
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
                self.apply_proof_settings();
            }
            if pixels && !structure {
                self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
            }
            if selection || structure {
                self.apply_canvas_event(CanvasEvent::SelectionChanged);
            }
        }
        for (job, command, error) in finished {
            self.on_job_finished(job, &command, error.as_ref());
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Another tab (or page) is shown: draw it.
    fn show_new_tab_or_page(&mut self, fit: bool) {
        // A text being typed belongs to the tab it was started in.
        let shown = self.docs.active().id;
        if self
            .edit
            .text_edit
            .as_ref()
            .is_some_and(|t| t.doc_id != shown)
        {
            self.commit_text_edit();
        }
        self.shell.ui.show_welcome = false;
        self.refresh_active_document();
        if fit {
            self.fit_canvas_to_screen();
        }
        let idx = self.docs.active_doc_idx;
        self.ensure_raw_resident(idx);
    }

    /// Documents made elsewhere (by a job on an engine of its own) become
    /// tabs, the last one shown.
    pub(in crate::app) fn adopt_documents(&mut self, docs: Vec<Document>) {
        if docs.is_empty() {
            return;
        }
        self.prepare_for_command();
        let over_welcome = self.has_only_welcome_placeholder();
        {
            let mut ws = GuiWorkspace {
                docs: &mut self.docs,
                over_welcome,
            };
            for mut doc in docs {
                doc.id = ws.new_document_id();
                ws.add(doc);
            }
        }
        self.show_new_tab_or_page(false);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// What a finished job means to the panels that started it.
    fn on_job_finished(&mut self, job: JobId, command: &str, error: Option<&CmdError>) {
        if command == "export.pdf" {
            self.on_pdf_export_finished(job, error);
        }
        if command == "file.save" || command == "file.export" {
            self.on_write_finished(job, error);
        }
        if command == "layer.free_transform" {
            self.on_free_transform_finished(job, error);
        }
        if command == "image.crop" {
            self.on_crop_finished(job, error);
        }
        if command == "file.open" {
            self.on_open_finished(job, error);
        }
        #[cfg(feature = "pro")]
        self.on_pro_job_finished(job, command, error);
    }

    /// Wait for every command job to end and apply it (tests and the exit
    /// path, never a frame).
    #[allow(dead_code)]
    pub(crate) fn finish_commands(&mut self, timeout: std::time::Duration) {
        let start = std::time::Instant::now();
        while self.engine.busy() && start.elapsed() < timeout {
            self.poll_engine();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::Canvas;
    use iai_engine::HeadlessWorkspace;
    use serde_json::json;
    use std::time::Duration;

    /// An app showing a `w × h` photo of a blue square on white, no welcome
    /// page.
    fn app_with_photo(w: u32, h: u32) -> App {
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        let mut rgba = vec![255u8; (w * h * 4) as usize];
        for y in h / 4..h / 2 {
            for x in w / 4..w / 2 {
                let o = ((y * w + x) * 4) as usize;
                rgba[o..o + 4].copy_from_slice(&[0, 0, 255, 255]);
            }
        }
        app.docs.documents[0].canvas = Canvas::from_rgba(rgba, w, h);
        app
    }

    /// The same command on a copy of the document, without a window.
    fn headless(app: &App, call: CommandCall) -> (Canvas, Reply) {
        let source = &app.docs.active().canvas;
        let mut ws = HeadlessWorkspace::new();
        let id = ws.new_document_id();
        let canvas = Canvas::from_rgba(source.flatten_for_export(), source.width, source.height);
        ws.add(Document::from_canvas(id, canvas, None));
        let mut engine = iai_engine::Engine::new(command_registry(), Source::Test, Policy::all());
        let mut s = engine.session(&mut ws);
        let reply = match s.execute(call).unwrap() {
            Reply::Started { job } => match s.wait(job, Duration::from_secs(600)) {
                Some(iai_engine::JobState::Done { result, revision }) => {
                    Reply::Done { result, revision }
                }
                other => panic!("{other:?}"),
            },
            done => done,
        };
        let canvas = std::mem::replace(&mut ws.documents_mut()[0].canvas, Canvas::new(1, 1));
        (canvas, reply)
    }

    #[test]
    fn flipping_and_turning_from_the_menu_is_the_command() {
        let mut app = app_with_photo(64, 40);
        for (id, args) in [
            ("image.flip", json!({"axis": "horizontal"})),
            ("image.rotate", json!({"angle": "90"})),
        ] {
            let (expected, _) = headless(&app, CommandCall::new(id, args.clone()));
            app.run_with(id, args).unwrap();
            let canvas = &app.docs.active().canvas;
            assert_eq!(
                (canvas.width, canvas.height),
                (expected.width, expected.height)
            );
            assert_eq!(
                canvas.flatten_for_export(),
                expected.flatten_for_export(),
                "{id}"
            );
        }
        // One undo step each.
        assert_eq!(app.docs.active().canvas.undo_count(), 2);
    }

    #[test]
    fn select_all_from_the_keyboard_is_one_undo_step_and_says_so() {
        let mut app = app_with_photo(30, 20);
        app.run_now("select.all").unwrap();
        let canvas = &app.docs.active().canvas;
        assert!(canvas.selection.active);
        assert_eq!(canvas.undo_count(), 1);
        assert_eq!(app.shell.status_msg, "Đã chọn tất cả (30×20)");
        app.run_now("select.invert").unwrap();
        assert!(!app.docs.active().canvas.selection.active);
        app.run_now("select.all").unwrap();
        app.run_now("select.none").unwrap();
        assert!(!app.docs.active().canvas.selection.active);
    }

    #[test]
    fn pages_are_added_and_switched_from_the_page_bar() {
        let mut app = app_with_photo(30, 20);
        app.add_artboard();
        let doc = app.docs.active();
        assert_eq!((doc.page_count(), doc.active_artboard), (2, 1));
        assert_eq!(app.shell.status_msg, "Đã thêm trang 2 / 2");
        app.set_active_artboard(0);
        assert_eq!(app.docs.active().active_artboard, 0);
        assert_eq!(app.shell.status_msg, "Trang 1");
        // The photo is back on the page shown.
        let canvas = &app.docs.active().canvas;
        let px = canvas.flatten_for_export();
        let o = ((7 * 30 + 10) * 4) as usize;
        assert_eq!(&px[o..o + 3], &[0, 0, 255]);
    }

    #[test]
    fn a_failing_command_says_why_on_the_status_line() {
        let mut app = app_with_photo(10, 10);
        assert!(app.run_with("page.select", json!({"page": 5})).is_err());
        assert_eq!(app.shell.status_msg, "Không có trang 6");
    }

    /// The panic hunt of iai-engine, over the commands of the Pro pack.
    #[cfg(feature = "pro")]
    #[test]
    fn panic_hunt_finds_nothing_in_the_pro_commands() {
        use iai_engine::hunt::{Fixture, hunt, hunting_ground};
        use std::sync::Arc;
        crate::core::ai::take_model_test_turn();
        let scratch = std::env::temp_dir().join(format!("iai-hunt-pro-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).unwrap();
        let fixture: Arc<Fixture> = Arc::new(|| {
            let mut ws = HeadlessWorkspace::new();
            hunting_ground(&mut ws);
            let policy = Policy::all()
                .revoke(iai_engine::Effect::Network)
                .revoke(iai_engine::Effect::Print);
            (
                iai_engine::Engine::new(command_registry(), Source::Test, policy),
                Box::new(ws) as Box<dyn Workspace>,
            )
        });
        let findings = hunt(
            &command_registry(),
            &[
                "print.impose_sheet",
                "id_photo.make",
                "print.card_sheet",
                "id_photo.batch",
            ],
            fixture,
            &scratch,
            crate::core::ai::MODEL_TEST_PATIENCE,
        );
        let _ = std::fs::remove_dir_all(&scratch);
        assert!(findings.is_empty(), "{findings:#?}");
    }

    // Model tests take turns through a helper of the Pro build.
    #[cfg(feature = "pro")]
    #[test]
    fn select_subject_from_the_window_is_the_command() {
        use crate::core::select_subject::SelectSubjectModel;
        let path = std::path::Path::new("tmp/model-sources/gfpgan/inputs/whole_imgs/00.jpg");
        if !path.is_file() || !SelectSubjectModel::BiRefNetTiny.is_installed() {
            return;
        }
        crate::core::ai::take_model_test_turn();
        let photo = image::open(path).unwrap().to_rgba8();
        let (w, h) = photo.dimensions();
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        app.docs.documents[0].canvas = Canvas::from_rgba(photo.into_raw(), w, h);
        app.docs.documents[0].canvas.ensure_pixels();

        let (expected, reply) = headless(&app, CommandCall::new("select.subject", json!({})));
        app.do_select_subject();
        assert!(app.engine_busy(), "{}", app.shell.status_msg);
        app.finish_commands(crate::core::ai::MODEL_TEST_PATIENCE);
        let canvas = &app.docs.active().canvas;
        assert!(canvas.selection.active);
        assert!(canvas.selection.mask == expected.selection.mask);
        assert_eq!(canvas.undo_count(), 1);
        assert!(
            app.shell.status_msg.starts_with("Select Subject xong"),
            "{}",
            app.shell.status_msg
        );
        let result = reply.result().unwrap();
        assert!(result["bounds"]["w"].as_u64().unwrap() > 0);
    }
}

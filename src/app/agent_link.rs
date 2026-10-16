//! "Cho AI điều khiển iAi": the window lets an AI agent on this machine run
//! commands on the documents it shows. When the person turns it on, the
//! app listens on 127.0.0.1 with a fresh 256-bit token and writes where it
//! can be reached to `%APPDATA%\IAI\control.json`; `iai.exe mcp --bridge`
//! (what the agent starts) reads it and links up. Each request is answered
//! here, a frame at a time, as an agent's call: printing and the network
//! stay off, and it goes to the flight recorder as `agent`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use iai_automation::bridge::{self, BridgeServer, Control};
use iai_engine::{
    BatchReply, CmdError, CommandCall, CommandInfo, Effect, ErrorCode, JobId, JobState, Policy,
    Reply, Source,
};

use super::state::App;

/// The link while it is on.
pub struct AgentLink {
    server: BridgeServer,
    file: PathBuf,
}

impl Drop for AgentLink {
    fn drop(&mut self) {
        // Stale, it would send agents to a port nobody listens on.
        let _ = std::fs::remove_file(&self.file);
    }
}

/// The agent's view of the window: commands run on the documents shown,
/// as an agent's.
struct WindowHost<'a> {
    app: &'a mut App,
}

impl WindowHost<'_> {
    /// Run `f` with the engine set for an agent, then back for the person.
    fn as_agent<T>(&mut self, f: impl FnOnce(&mut App) -> T) -> T {
        let engine = &mut self.app.engine;
        let (source, policy) = (engine.source(), engine.policy().clone());
        engine.set_source(Source::Agent);
        engine.set_policy(Policy::agent());
        let out = f(self.app);
        self.app.engine.set_source(source);
        self.app.engine.set_policy(policy);
        out
    }
}

/// The window has every command, the Pro pack's too; an agent gets those
/// while this machine's Pro is open (unlocked or on trial).
fn pro_refused(app: &App, id: &str) -> Option<CmdError> {
    #[cfg(feature = "pro")]
    {
        let pro = ["id_photo.make", "print.impose_sheet"];
        if pro.contains(&id) && !app.shell.pro.status().open() {
            return Some(
                CmdError::new(
                    ErrorCode::PermissionDenied,
                    format!("`{id}` is a Pro command, and Pro is not open on this machine"),
                )
                .vi("Lệnh này thuộc gói Pro, máy này chưa mở khóa Pro"),
            );
        }
    }
    let _ = (app, id);
    None
}

/// While the person is in the middle of an edit, an agent may look but not
/// change anything.
fn wait_for_person(app: &App, id: &str) -> Option<CmdError> {
    let reads_only = app
        .engine
        .registry()
        .get(id)
        .is_some_and(|c| c.meta().effects.iter().all(|e| *e == Effect::Query));
    (!reads_only && app.person_mid_edit()).then(|| {
        CmdError::new(
            ErrorCode::Busy,
            "the person at the window is in the middle of an edit (a transform, a text, \
             a preview); ask again in a while",
        )
        .vi("Bạn đang làm dở một thao tác trong iAi; AI chờ bạn làm xong")
    })
}

impl iai_automation::Host for WindowHost<'_> {
    fn commands(&mut self) -> Result<Vec<CommandInfo>, CmdError> {
        let app = &*self.app;
        Ok(app
            .engine
            .registry()
            .list()
            .into_iter()
            .filter(|c| pro_refused(app, &c.id).is_none())
            .collect())
    }

    fn execute(&mut self, call: CommandCall) -> Result<Reply, CmdError> {
        let app = &*self.app;
        if let Some(e) =
            pro_refused(app, &call.command).or_else(|| wait_for_person(app, &call.command))
        {
            return Err(e);
        }
        self.as_agent(|app| app.execute_command(call))
    }

    fn batch(&mut self, calls: Vec<CommandCall>, atomic: bool) -> Result<BatchReply, CmdError> {
        let app = &*self.app;
        if let Some(e) = calls
            .iter()
            .find_map(|c| pro_refused(app, &c.command).or_else(|| wait_for_person(app, &c.command)))
        {
            return Err(e);
        }
        Ok(self.as_agent(|app| app.execute_batch(calls, atomic)))
    }

    fn job(&mut self, job: JobId, _wait: Duration) -> Result<Option<JobState>, CmdError> {
        // Answered at once: the client asks again while it waits.
        self.app.poll_engine();
        Ok(self.app.engine.job_state(job))
    }

    fn cancel(&mut self, job: JobId) -> Result<bool, CmdError> {
        Ok(self.app.engine.cancel(job))
    }
}

impl App {
    /// Turn the link for AI agents on or off.
    pub(crate) fn set_agent_control(&mut self, on: bool) {
        if !on {
            if self.shell.agent_link.take().is_some() {
                crate::diag::note("agent", "control off");
                self.shell.status_msg = "Đã tắt: AI không còn điều khiển iAi".to_string();
            }
            return;
        }
        if self.shell.agent_link.is_some() {
            return;
        }
        match self.start_agent_link(crate::agent::control_file()) {
            Ok(link) => {
                crate::diag::note("agent", &format!("control on, port {}", link.server.port()));
                self.shell.agent_link = Some(link);
                self.shell.agent_link_error = None;
                self.shell.status_msg = "Đã bật: AI trên máy này điều khiển được iAi".to_string();
            }
            Err(e) => {
                let line = format!("Không bật được: {e}");
                self.shell.status_msg = line.clone();
                self.shell.agent_link_error = Some(line);
            }
        }
    }

    /// Listen, and say where in `file`.
    fn start_agent_link(&self, file: PathBuf) -> std::io::Result<AgentLink> {
        let token = bridge::new_token()?;
        let window = self.win.window.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(w) = &window {
                w.request_redraw();
            }
        });
        let server = BridgeServer::start(token.clone(), wake)?;
        Control {
            port: server.port(),
            token,
            pid: std::process::id(),
        }
        .write(&file)?;
        Ok(AgentLink { server, file })
    }

    pub(crate) fn agent_control_on(&self) -> bool {
        self.shell.agent_link.is_some()
    }

    /// The dialog's view; the program's path only while it is open.
    pub(crate) fn agent_view(&self) -> crate::ui::AgentView {
        let open = self.shell.ui.support_dialog == Some(crate::ui::SupportDialog::Agent);
        crate::ui::AgentView {
            on: self.agent_control_on(),
            error: self.shell.agent_link_error.clone(),
            exe: if open {
                std::env::current_exe()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "iai.exe".to_string())
            } else {
                String::new()
            },
        }
    }

    /// Every frame: answer what agents asked.
    pub(crate) fn poll_agent_link(&mut self) {
        // A few at most a frame: the window keeps drawing.
        for _ in 0..8 {
            let Some(request) = self.shell.agent_link.as_ref().and_then(|l| l.server.next()) else {
                return;
            };
            let mut host = WindowHost { app: self };
            bridge::answer_with(&mut host, request);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iai_automation::bridge::BridgeHost;
    use iai_automation::Host;
    use iai_engine::ErrorCode;
    use serde_json::json;

    /// An agent's process on one side, the window's frames on the other.
    #[test]
    fn an_agent_works_in_the_window_and_the_person_comes_first() {
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        let file = std::env::temp_dir().join(format!("iai-agent-link-{}.json", std::process::id()));
        app.shell.agent_link = Some(app.start_agent_link(file.clone()).unwrap());
        let control = Control::read(&file).unwrap();

        let agent = std::thread::spawn(move || {
            let mut host = BridgeHost::connect(&control).unwrap();
            let ids: Vec<String> = host.commands().unwrap().into_iter().map(|c| c.id).collect();
            let flip = host.execute(CommandCall::new(
                "image.flip",
                json!({"axis": "horizontal"}),
            ));
            // Paper and the network stay the person's.
            let download = host.execute(CommandCall::new("ai.model_download", json!({})));
            let batch = host
                .batch(
                    vec![
                        CommandCall::new("layer.new", json!({})),
                        CommandCall::new("layer.delete", json!({"layer": 999})),
                    ],
                    true,
                )
                .unwrap();
            (ids, flip, download, batch)
        });
        let start = std::time::Instant::now();
        while !agent.is_finished() {
            assert!(start.elapsed() < Duration::from_secs(60));
            app.poll_agent_link();
            std::thread::sleep(Duration::from_millis(2));
        }
        let (ids, flip, download, batch) = agent.join().unwrap();
        assert!(ids.iter().any(|id| id == "image.flip"));
        assert!(flip.is_ok(), "{flip:?}");
        assert_eq!(download.unwrap_err().code, ErrorCode::PermissionDenied);
        assert!(batch.rolled_back && batch.failed.is_some());
        // One undo step, the flip; the batch was taken back.
        let canvas = &app.docs.active().canvas;
        assert_eq!(canvas.undo_count(), 1);
        assert_eq!(canvas.layer_count(), 1);
        // The window is the person's again.
        assert_eq!(app.engine.source(), Source::Gui);
        assert!(app
            .engine
            .policy()
            .grants(&[Effect::Print, Effect::Network]));

        // In the middle of an edit: the agent may look, not change.
        app.shell.ui.show_adjustment_dialog = true;
        assert!(app.person_mid_edit());
        let mut host = WindowHost { app: &mut app };
        let e = host
            .execute(CommandCall::new("image.flip", json!({"axis": "vertical"})))
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Busy);
        assert!(host
            .execute(CommandCall::new("doc.inspect", json!({})))
            .is_ok());
        app.shell.ui.show_adjustment_dialog = false;

        // Off: the file that pointed agents here is gone.
        app.set_agent_control(false);
        assert!(!file.exists());
        assert!(!app.agent_control_on());
    }
}

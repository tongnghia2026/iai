//! "Cho AI điều khiển iAi": the window lets an AI agent on this machine run
//! commands on the documents it shows. When the person turns it on, the
//! app listens on 127.0.0.1 with a fresh 256-bit token and writes where it
//! can be reached to `%APPDATA%\IAI\control.json`; `iai.exe mcp --bridge`
//! (what the agent starts) reads it and links up. Each request is answered
//! here, a frame at a time, as an agent's call: printing and the network
//! stay off, and it goes to the flight recorder as `agent`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    /// What this window wrote to the file.
    control: Control,
    /// When the file was last looked at.
    checked: Instant,
}

impl AgentLink {
    /// The file says where this window is: written again when it is gone
    /// (another window that went away, or another program, took it out). A
    /// file another window wrote since is left to it.
    fn keep_file(&mut self) {
        if self.checked.elapsed() < Duration::from_secs(3) {
            return;
        }
        self.checked = Instant::now();
        if !self.file.exists() {
            let _ = self.control.write(&self.file);
        }
    }
}

impl Drop for AgentLink {
    fn drop(&mut self) {
        // Stale, it would send agents to a port nobody listens on; another
        // window's file is not this one's to take out.
        if Control::read(&self.file).is_ok_and(|c| c == self.control) {
            let _ = std::fs::remove_file(&self.file);
        }
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
        let pro = [
            "id_photo.make",
            "id_photo.batch",
            "id_photo.dress",
            "id_photo.dress_input",
            "print.impose_sheet",
        ];
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
    pub(super) fn start_agent_link(&self, file: PathBuf) -> std::io::Result<AgentLink> {
        let token = bridge::new_token()?;
        let window = self.win.window.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(w) = &window {
                w.request_redraw();
            }
        });
        let server = BridgeServer::start(token.clone(), wake)?;
        let control = Control {
            port: server.port(),
            token,
            pid: std::process::id(),
        };
        control.write(&file)?;
        Ok(AgentLink {
            server,
            file,
            control,
            checked: Instant::now(),
        })
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
            skills_dir: if open {
                iai_automation::skills::default_root().display().to_string()
            } else {
                String::new()
            },
            skills: if open {
                self.shell
                    .agent_skills
                    .as_ref()
                    .map(|(_, rows)| rows.clone())
                    .unwrap_or_default()
            } else {
                Vec::new()
            },
        }
    }

    /// The library as the dialog lists it: last used first.
    fn read_skills() -> Vec<crate::ui::SkillRow> {
        let store = iai_automation::skills::Store::new(iai_automation::skills::default_root());
        let mut skills = store.list();
        skills.sort_by_key(|(_, r)| std::cmp::Reverse(r.meta.last_used_at.max(r.meta.created_at)));
        skills
            .into_iter()
            .map(|(name, r)| {
                let at = r.meta.last_used_at.max(r.meta.created_at);
                crate::ui::SkillRow {
                    title: if r.meta.title.trim().is_empty() {
                        name.clone()
                    } else {
                        r.meta.title.clone()
                    },
                    name,
                    status: r.meta.status.vi(r.meta.uses),
                    when: if at == 0 {
                        String::new()
                    } else {
                        crate::diag::local_minute(
                            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(at),
                        )
                    },
                }
            })
            .collect()
    }

    /// Show the library's folder (made if it is not there yet).
    pub(crate) fn open_skills_folder(&mut self) {
        let dir = iai_automation::skills::default_root();
        let _ = std::fs::create_dir_all(&dir);
        #[cfg(windows)]
        let _ = std::process::Command::new("explorer.exe").arg(&dir).spawn();
    }

    /// Take a skill out of the library, into its `.da_xoa` folder.
    pub(crate) fn remove_skill(&mut self, name: &str) {
        let store = iai_automation::skills::Store::new(iai_automation::skills::default_root());
        let title = store
            .load(name)
            .map(|r| r.meta.title)
            .unwrap_or_else(|_| name.to_string());
        self.shell.status_msg = match store.remove(name) {
            Ok(_) => format!(
                "Đã xóa kỹ năng «{title}» (cất trong thư mục {} của kho kỹ năng)",
                iai_automation::skills::REMOVED
            ),
            Err(e) => format!("Không xóa được kỹ năng «{title}»: {}", e.message),
        };
        self.shell.agent_skills = None;
    }

    /// Every frame: answer what agents asked; while the dialog is open, keep
    /// its list of skills fresh (an agent may be saving one meanwhile).
    pub(crate) fn poll_agent_link(&mut self) {
        let open = self.shell.ui.support_dialog == Some(crate::ui::SupportDialog::Agent);
        if !open {
            self.shell.agent_skills = None;
        } else if self
            .shell
            .agent_skills
            .as_ref()
            .is_none_or(|(at, _)| at.elapsed() >= std::time::Duration::from_secs(3))
        {
            self.shell.agent_skills = Some((std::time::Instant::now(), Self::read_skills()));
        }
        if let Some(link) = self.shell.agent_link.as_mut() {
            link.keep_file();
        }
        // A few at most a frame: the window keeps drawing.
        for _ in 0..8 {
            let Some(request) = self.shell.agent_link.as_ref().and_then(|l| l.server.next()) else {
                return;
            };
            if !request.op.is_command() {
                self.answer_errand_word(request);
                continue;
            }
            if let Some(refused) = self.errand_step(&request.op) {
                request.answer(Err(refused));
                continue;
            }
            let mut host = WindowHost { app: self };
            bridge::answer_with(&mut host, request);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iai_automation::Host;
    use iai_automation::bridge::BridgeHost;
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
        assert!(
            app.engine
                .policy()
                .grants(&[Effect::Print, Effect::Network])
        );

        // In the middle of an edit: the agent may look, not change.
        app.shell.ui.show_adjustment_dialog = true;
        assert!(app.person_mid_edit());
        let mut host = WindowHost { app: &mut app };
        let e = host
            .execute(CommandCall::new("image.flip", json!({"axis": "vertical"})))
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Busy);
        assert!(
            host.execute(CommandCall::new("doc.inspect", json!({})))
                .is_ok()
        );
        app.shell.ui.show_adjustment_dialog = false;

        // The file taken out from under it: written again, as it was.
        let ours = Control::read(&file).unwrap();
        std::fs::remove_file(&file).unwrap();
        let past = |app: &mut App| {
            if let Some(link) = app.shell.agent_link.as_mut() {
                link.checked = Instant::now().checked_sub(Duration::from_secs(5)).unwrap();
            }
        };
        past(&mut app);
        app.poll_agent_link();
        assert_eq!(Control::read(&file).unwrap(), ours);

        // Off: the file that pointed agents here is gone.
        app.set_agent_control(false);
        assert!(!file.exists());
        assert!(!app.agent_control_on());

        // Another window's file is not this one's to take out.
        app.shell.agent_link = Some(app.start_agent_link(file.clone()).unwrap());
        let other = Control { port: 1, ..ours };
        other.write(&file).unwrap();
        past(&mut app);
        app.poll_agent_link();
        app.set_agent_control(false);
        assert_eq!(Control::read(&file).unwrap(), other);
        let _ = std::fs::remove_file(&file);
    }
}

//! Where commands run for an agent.

use std::path::PathBuf;
use std::time::Duration;

use iai_engine::{
    BatchReply, CmdError, CommandCall, CommandInfo, Engine, HeadlessWorkspace, JobId, JobState,
    Reply, Workspace,
};
use serde::{Deserialize, Serialize};

/// Folders the person granted where the commands run, beyond those the
/// session started with: the window grants the folder of an errand the
/// person sent from it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grants {
    #[serde(default)]
    pub read: Vec<PathBuf>,
    #[serde(default)]
    pub write: Vec<PathBuf>,
}

/// Where commands run: an engine with no window, or the app the person has
/// open (through the bridge).
pub trait Host {
    /// Every command, with its schemas.
    fn commands(&mut self) -> Result<Vec<CommandInfo>, CmdError>;
    fn execute(&mut self, call: CommandCall) -> Result<Reply, CmdError>;
    fn batch(&mut self, calls: Vec<CommandCall>, atomic: bool) -> Result<BatchReply, CmdError>;
    /// The job's state, after waiting up to `wait` for it to end; `None`
    /// when no such job is known.
    fn job(&mut self, job: JobId, wait: Duration) -> Result<Option<JobState>, CmdError>;
    /// Ask a running job to stop; false when none runs by that id.
    fn cancel(&mut self, job: JobId) -> Result<bool, CmdError>;
    /// The agent began a task the person asked for, in their words.
    fn task_begin(&mut self, _request: &str) {}
    /// The agent ended its task: "done", "failed" or "cancelled".
    fn task_end(&mut self, _status: &str, _title: &str) {}
    /// Folders the person granted there, beyond the session's own.
    fn grants(&mut self) -> Grants {
        Grants::default()
    }
    /// Nothing is open on it: moving to another host leaves no work behind.
    fn idle(&self) -> bool {
        false
    }
}

/// An engine of its own, with no window: what `iai.exe mcp` runs on.
pub struct LocalHost {
    pub engine: Engine,
    pub ws: HeadlessWorkspace,
}

impl LocalHost {
    pub fn new(engine: Engine) -> Self {
        Self {
            engine,
            ws: HeadlessWorkspace::new(),
        }
    }
}

impl Host for LocalHost {
    fn commands(&mut self) -> Result<Vec<CommandInfo>, CmdError> {
        Ok(self.engine.registry().list())
    }

    fn execute(&mut self, call: CommandCall) -> Result<Reply, CmdError> {
        self.engine.session(&mut self.ws).execute(call)
    }

    fn batch(&mut self, calls: Vec<CommandCall>, atomic: bool) -> Result<BatchReply, CmdError> {
        Ok(self
            .engine
            .session(&mut self.ws)
            .execute_batch(calls, atomic))
    }

    fn job(&mut self, job: JobId, wait: Duration) -> Result<Option<JobState>, CmdError> {
        let mut s = self.engine.session(&mut self.ws);
        s.poll();
        Ok(s.wait(job, wait))
    }

    fn cancel(&mut self, job: JobId) -> Result<bool, CmdError> {
        Ok(self.engine.cancel(job))
    }

    fn idle(&self) -> bool {
        self.ws.documents().is_empty()
    }
}

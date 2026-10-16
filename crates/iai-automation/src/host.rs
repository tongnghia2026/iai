//! Where commands run for an agent.

use std::time::Duration;

use iai_engine::{
    BatchReply, CmdError, CommandCall, CommandInfo, Engine, HeadlessWorkspace, JobId, JobState,
    Reply,
};

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
}

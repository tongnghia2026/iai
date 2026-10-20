//! A script of calls, as `iai.exe run` takes it. One that is all or nothing
//! (`atomic`) is one batch; one that is not may be longer, and runs batch
//! after batch on the same documents.

use iai_engine::{BatchFailure, BatchReply, CmdError, CommandCall, ErrorCode};

use crate::limits::{MAX_BATCH, MAX_SCRIPT};
use crate::{Gate, Host};

/// Run `calls` on `host`, every one checked by `gate` (which has learned the
/// host's commands) before any runs. A script that is not atomic stops at its
/// first failure, whose index counts from the script's first call.
pub fn run(
    host: &mut dyn Host,
    gate: &Gate,
    mut calls: Vec<CommandCall>,
    atomic: bool,
) -> Result<BatchReply, CmdError> {
    if atomic && calls.len() > MAX_BATCH {
        return Err(CmdError::new(
            ErrorCode::TooLarge,
            format!(
                "an atomic script holds at most {MAX_BATCH} calls; leave `atomic` out to run \
                 a longer one, {MAX_BATCH} calls at a time"
            ),
        ));
    }
    if calls.len() > MAX_SCRIPT {
        return Err(CmdError::new(
            ErrorCode::TooLarge,
            format!("a script holds at most {MAX_SCRIPT} calls"),
        ));
    }
    for chunk in calls.chunks_mut(MAX_BATCH) {
        gate.check_batch(chunk)?;
    }
    if atomic {
        return host.batch(calls, true);
    }
    let mut completed = Vec::with_capacity(calls.len());
    let mut rest = calls.into_iter().peekable();
    while rest.peek().is_some() {
        let chunk: Vec<CommandCall> = rest.by_ref().take(MAX_BATCH).collect();
        let before = completed.len();
        let reply = host.batch(chunk, false)?;
        completed.extend(reply.completed);
        if let Some(failure) = reply.failed {
            return Ok(BatchReply {
                completed,
                failed: Some(BatchFailure {
                    index: before.saturating_add(failure.index),
                    error: failure.error,
                }),
                rolled_back: false,
            });
        }
    }
    Ok(BatchReply {
        completed,
        failed: None,
        rolled_back: false,
    })
}

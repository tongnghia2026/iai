//! Who is calling, and what they were granted.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::Serialize;

use crate::command::{CommandMeta, Effect};
use crate::{CmdError, ErrorCode};

/// Where a call comes from; written to the flight recorder with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The person, through the window.
    Gui,
    /// A script run from the command line.
    Cli,
    /// An AI agent (MCP), headless or through the window's bridge.
    Agent,
    Test,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Gui => "gui",
            Source::Cli => "cli",
            Source::Agent => "agent",
            Source::Test => "test",
        }
    }
}

/// The effects a session may cause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    granted: BTreeSet<Effect>,
}

impl Policy {
    /// Everything: the person at the window.
    pub fn all() -> Self {
        Self {
            granted: Effect::ALL.into_iter().collect(),
        }
    }

    /// An agent's: no printing, no network, unless granted at launch.
    pub fn agent() -> Self {
        Self {
            granted: [
                Effect::Query,
                Effect::Edit,
                Effect::ReadFiles,
                Effect::WriteFiles,
                Effect::LocalAi,
            ]
            .into_iter()
            .collect(),
        }
    }

    pub fn grant(mut self, effect: Effect) -> Self {
        self.granted.insert(effect);
        self
    }

    pub fn revoke(mut self, effect: Effect) -> Self {
        self.granted.remove(&effect);
        self
    }

    pub fn grants(&self, effects: &[Effect]) -> bool {
        effects.iter().all(|e| self.granted.contains(e))
    }

    /// Refuse a command with an effect this session was not granted.
    pub fn check(&self, meta: &CommandMeta) -> Result<(), CmdError> {
        let Some(missing) = meta.effects.iter().find(|e| !self.granted.contains(e)) else {
            return Ok(());
        };
        let name = serde_json::to_value(missing)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let mut e = CmdError::new(
            ErrorCode::PermissionDenied,
            format!("effect `{name}` was not granted to this session"),
        )
        .vi(format!("Phiên này không được phép `{name}`"));
        let flag = match missing {
            Effect::Print => Some("--allow-print"),
            Effect::Network => Some("--allow-network"),
            _ => None,
        };
        if let Some(flag) = flag {
            e = e.with_hint(serde_json::json!({ "relaunchWith": flag }));
        }
        Err(e)
    }
}

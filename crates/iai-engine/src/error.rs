//! What a command returns when it fails: a code an agent can branch on, an
//! English message for agents and logs, a Vietnamese one for the status bar,
//! and the call that would fix it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What went wrong. Serialized in snake_case (`model_missing`); a code never
/// changes meaning once shipped, new ones are added at the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// No command has that id.
    UnknownCommand,
    /// The params did not parse, or a value is out of range.
    BadParams,
    /// The command exists but cannot run now (e.g. nothing selected).
    NotEnabled,
    /// No document is open, or the one named is not.
    NoDocument,
    /// A page, layer or file named in the params does not exist.
    NotFound,
    /// The document changed since the revision the caller expected.
    StaleRevision,
    /// The image or the request is larger than the engine accepts.
    TooLarge,
    /// The AI model the command needs is not installed.
    ModelMissing,
    /// A job is already working on the document.
    Busy,
    /// The caller cancelled the job.
    Cancelled,
    /// The session was not granted what the command needs (printing,
    /// network, a folder).
    PermissionDenied,
    /// Reading or writing a file failed.
    Io,
    /// The GPU failed or is not there.
    Gpu,
    /// A fault of the engine itself.
    Internal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, thiserror::Error)]
#[error("{code:?}: {message}")]
#[serde(rename_all = "camelCase")]
pub struct CmdError {
    pub code: ErrorCode,
    /// English, for agents and logs.
    pub message: String,
    /// Vietnamese, for the status bar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_vi: Option<String>,
    /// The call that would fix it, e.g.
    /// `{"command": "ai.model_download", "args": {"model": "birefnet"}}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<Value>,
}

impl CmdError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            message_vi: None,
            hint: None,
        }
    }

    /// The params did not parse (unknown field, wrong type, missing field).
    pub fn bad_params(e: &serde_json::Error) -> Self {
        Self::new(ErrorCode::BadParams, e.to_string())
    }

    /// One param is out of range or contradicts another.
    pub fn bad_param(field: &str, expected: &str) -> Self {
        Self::new(
            ErrorCode::BadParams,
            format!("`{field}`: expected {expected}"),
        )
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    /// Adds the Vietnamese text the status bar shows.
    pub fn vi(mut self, text: impl Into<String>) -> Self {
        self.message_vi = Some(text.into());
        self
    }

    /// Adds the call that would fix it.
    pub fn with_hint(mut self, hint: Value) -> Self {
        self.hint = Some(hint);
        self
    }
}

impl From<std::io::Error> for CmdError {
    fn from(e: std::io::Error) -> Self {
        let code = match e.kind() {
            std::io::ErrorKind::NotFound => ErrorCode::NotFound,
            std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
            _ => ErrorCode::Io,
        };
        Self::new(code, e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_error_reads_as_an_agent_expects_it() {
        let e = CmdError::new(
            ErrorCode::ModelMissing,
            "the subject model is not installed",
        )
        .vi("Chưa có model tách nền")
        .with_hint(json!({"command": "ai.model_download", "args": {"model": "birefnet"}}));
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(
            v,
            json!({
                "code": "model_missing",
                "message": "the subject model is not installed",
                "messageVi": "Chưa có model tách nền",
                "hint": {"command": "ai.model_download", "args": {"model": "birefnet"}},
            })
        );
        let back: CmdError = serde_json::from_value(v).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn what_is_not_set_is_left_out() {
        let v =
            serde_json::to_value(CmdError::bad_param("dpi", "a number from 1 to 2400")).unwrap();
        assert_eq!(
            v,
            json!({"code": "bad_params", "message": "`dpi`: expected a number from 1 to 2400"})
        );
        let back: CmdError = serde_json::from_value(v).unwrap();
        assert_eq!(back.message_vi, None);
        assert_eq!(back.hint, None);
    }

    #[test]
    fn a_parse_failure_names_the_field() {
        #[derive(Debug, Deserialize)]
        #[serde(deny_unknown_fields)]
        #[allow(dead_code)]
        struct Params {
            dpi: u32,
        }
        let parse = serde_json::from_value::<Params>(json!({"dpi": 300, "dpii": 2}));
        let e = CmdError::bad_params(&parse.unwrap_err());
        assert_eq!(e.code, ErrorCode::BadParams);
        assert!(e.message.contains("dpii"), "{}", e.message);
    }

    #[test]
    fn a_missing_file_is_not_found_and_shows_its_code() {
        let e = CmdError::from(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert_eq!(e.code, ErrorCode::NotFound);
        assert!(e.to_string().starts_with("NotFound: "), "{e}");
    }
}

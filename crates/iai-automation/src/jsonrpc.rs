//! JSON-RPC 2.0, one message a line, as MCP's stdio transport and the
//! bridge use it.

use std::io::{self, BufRead, Write};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::limits::MAX_LINE;

/// Standard error codes.
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;

/// A request or a notification (no `id`).
#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

/// The answer to a request: its `result` or its `error`.
pub fn response(id: Value, outcome: Result<Value, RpcError>) -> Value {
    match outcome {
        Ok(result) => serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => serde_json::json!({"jsonrpc": "2.0", "id": id, "error": error}),
    }
}

/// What reading a line gave.
pub enum Line {
    Text(String),
    /// The line was longer than [`MAX_LINE`]; it was read through and dropped.
    TooLong,
    End,
}

/// Read one line of at most [`MAX_LINE`] bytes; a longer one is skipped to
/// its end, never held whole in memory.
pub fn read_line<R: BufRead>(input: &mut R) -> io::Result<Line> {
    let mut buf = Vec::new();
    let mut too_long = false;
    loop {
        let (done, used) = {
            let available = input.fill_buf()?;
            if available.is_empty() {
                if buf.is_empty() && !too_long {
                    return Ok(Line::End);
                }
                (true, 0)
            } else {
                match available.iter().position(|&b| b == b'\n') {
                    Some(at) => {
                        if !too_long {
                            buf.extend_from_slice(available.get(..at).unwrap_or_default());
                        }
                        (true, at.saturating_add(1))
                    }
                    None => {
                        if !too_long {
                            buf.extend_from_slice(available);
                        }
                        (false, available.len())
                    }
                }
            }
        };
        input.consume(used);
        if buf.len() > MAX_LINE {
            too_long = true;
            buf = Vec::new();
        }
        if done {
            break;
        }
    }
    if too_long {
        return Ok(Line::TooLong);
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    Ok(Line::Text(String::from_utf8_lossy(&buf).into_owned()))
}

/// Write `message` as one line and flush it.
pub fn write_message<W: Write>(output: &mut W, message: &Value) -> io::Result<()> {
    let mut text = serde_json::to_string(message).map_err(io::Error::other)?;
    text.push('\n');
    output.write_all(text.as_bytes())?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(input: &[u8]) -> Vec<String> {
        let mut reader = io::BufReader::with_capacity(16, input);
        let mut out = Vec::new();
        loop {
            match read_line(&mut reader).unwrap() {
                Line::Text(t) => out.push(t),
                Line::TooLong => out.push("<too long>".to_string()),
                Line::End => return out,
            }
        }
    }

    #[test]
    fn lines_are_read_one_by_one_with_or_without_a_carriage_return() {
        assert_eq!(lines(b"a\r\nbb\nccc"), ["a", "bb", "ccc"]);
        assert!(lines(b"").is_empty());
    }

    #[test]
    fn a_line_past_the_limit_is_dropped_and_the_next_one_read() {
        let mut input = vec![b'x'; MAX_LINE + 10];
        input.extend_from_slice(b"\nok\n");
        assert_eq!(lines(&input), ["<too long>", "ok"]);
    }
}

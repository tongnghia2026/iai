//! The link between an agent's process and the app the person has open.
//!
//! The app listens on 127.0.0.1 (a port the system picks) and writes the
//! port and a fresh 256-bit token to a file only the user's own programs
//! read; a client must send the token first, or is cut off. After that,
//! one JSON message a line each way: `{"id", "op", …}` in,
//! `{"id", "ok": …}` or `{"id", "err": CmdError}` out. The app answers each
//! request from its own thread, a frame at a time: commands run there, on
//! the documents the person sees.

use std::io::{self, BufReader};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iai_engine::{
    BatchReply, CmdError, CommandCall, CommandInfo, ErrorCode, JobId, JobState, Reply,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::host::Host;
use crate::jsonrpc::{self, Line};
use crate::limits::MAX_LINKS;

/// What a client asks the app.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Commands,
    Execute {
        call: CommandCall,
    },
    Batch {
        calls: Vec<CommandCall>,
        atomic: bool,
    },
    /// The job's state now (the client waits by asking again).
    Job {
        job: JobId,
    },
    Cancel {
        job: JobId,
    },
}

/// A request waiting for the app's answer.
pub struct Request {
    pub op: Op,
    reply: Sender<Result<Value, CmdError>>,
}

impl Request {
    pub fn answer(self, outcome: Result<Value, CmdError>) {
        let _ = self.reply.send(outcome);
    }
}

/// Where a running app can be reached: what the control file holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Control {
    pub port: u16,
    pub token: String,
    pub pid: u32,
}

impl Control {
    pub fn read(path: &Path) -> io::Result<Self> {
        let bytes = std::fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }

    pub fn write(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec_pretty(self).map_err(io::Error::other)?,
        )?;
        std::fs::rename(&tmp, path)
    }
}

/// 256 random bits, as hex.
pub fn new_token() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Equal, taking as long whatever the first difference.
fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The app's end: a listener, and the requests that came in.
pub struct BridgeServer {
    port: u16,
    requests: Receiver<Request>,
    stop: Arc<AtomicBool>,
}

impl BridgeServer {
    /// Listen on 127.0.0.1. `wake` is called when a request arrives, so an
    /// idle window draws a frame to answer it.
    pub fn start(token: String, wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
        let port = listener.local_addr()?.port();
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let links = Arc::new(AtomicUsize::new(0));
        let stopping = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("control listener".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if stopping.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(stream) = stream else {
                        continue;
                    };
                    if links.load(Ordering::Relaxed) >= MAX_LINKS {
                        continue;
                    }
                    links.fetch_add(1, Ordering::Relaxed);
                    let (tx, token, wake, links, stopping) = (
                        tx.clone(),
                        token.clone(),
                        Arc::clone(&wake),
                        Arc::clone(&links),
                        Arc::clone(&stopping),
                    );
                    let _ = std::thread::Builder::new()
                        .name("control link".into())
                        .spawn(move || {
                            let _ = serve_link(stream, &token, &tx, &*wake, &stopping);
                            links.fetch_sub(1, Ordering::Relaxed);
                        });
                }
            })?;
        Ok(Self {
            port,
            requests: rx,
            stop,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The next request waiting, if any.
    pub fn next(&self) -> Option<Request> {
        self.requests.try_recv().ok()
    }
}

impl Drop for BridgeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wake the listener so it sees the flag.
        let _ = TcpStream::connect_timeout(
            &SocketAddr::from((Ipv4Addr::LOCALHOST, self.port)),
            Duration::from_millis(200),
        );
    }
}

fn serve_link(
    stream: TcpStream,
    token: &str,
    requests: &Sender<Request>,
    wake: &(dyn Fn() + Send + Sync),
    stop: &AtomicBool,
) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(600)))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    // The token first, within a few seconds.
    reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(5)))?;
    let Line::Text(first) = jsonrpc::read_line(&mut reader)? else {
        return Ok(());
    };
    let given = serde_json::from_str::<Value>(&first)
        .ok()
        .and_then(|v| v.get("token").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default();
    if !same(&given, token) {
        jsonrpc::write_message(&mut writer, &json!({"err": "bad token"}))?;
        return Ok(());
    }
    jsonrpc::write_message(&mut writer, &json!({"ok": "iai"}))?;
    reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(3600)))?;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let text = match jsonrpc::read_line(&mut reader)? {
            Line::End => return Ok(()),
            Line::TooLong => {
                jsonrpc::write_message(
                    &mut writer,
                    &json!({"id": null, "err": CmdError::new(ErrorCode::TooLarge, "message too long")}),
                )?;
                continue;
            }
            Line::Text(t) => t,
        };
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let id = value.get("id").cloned().unwrap_or(Value::Null);
        let op: Result<Op, CmdError> =
            serde_json::from_value(value).map_err(|e| CmdError::bad_params(&e));
        let outcome = match op {
            Err(e) => Err(e),
            Ok(op) => {
                let (tx, rx) = mpsc::channel();
                if requests.send(Request { op, reply: tx }).is_err() {
                    return Ok(());
                }
                wake();
                rx.recv_timeout(Duration::from_secs(600))
                    .unwrap_or_else(|_| {
                        Err(CmdError::new(ErrorCode::Busy, "the app did not answer"))
                    })
            }
        };
        let message = match outcome {
            Ok(v) => json!({"id": id, "ok": v}),
            Err(e) => json!({"id": id, "err": e}),
        };
        jsonrpc::write_message(&mut writer, &message)?;
    }
}

/// The agent's end of the link: a host that is the running app.
pub struct BridgeHost {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    next: u64,
}

fn link_lost(e: io::Error) -> CmdError {
    CmdError::new(ErrorCode::Io, format!("the link to iAi broke: {e}"))
        .vi("Mất kết nối với iAi đang mở")
}

impl BridgeHost {
    pub fn connect(control: &Control) -> io::Result<Self> {
        let stream = TcpStream::connect_timeout(
            &SocketAddr::from((Ipv4Addr::LOCALHOST, control.port)),
            Duration::from_secs(3),
        )?;
        stream.set_nodelay(true)?;
        let mut writer = stream.try_clone()?;
        let mut reader = BufReader::new(stream);
        jsonrpc::write_message(&mut writer, &json!({"token": control.token}))?;
        match jsonrpc::read_line(&mut reader)? {
            Line::Text(t) if t.contains("\"ok\"") => Ok(Self {
                reader,
                writer,
                next: 0,
            }),
            _ => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "iAi refused the link (an old control file?)",
            )),
        }
    }

    fn ask(&mut self, op: Op) -> Result<Value, CmdError> {
        self.next = self.next.saturating_add(1);
        let mut message =
            serde_json::to_value(&op).map_err(|e| CmdError::internal(e.to_string()))?;
        if let Some(o) = message.as_object_mut() {
            o.insert("id".to_string(), json!(self.next));
        }
        jsonrpc::write_message(&mut self.writer, &message).map_err(link_lost)?;
        let line = match jsonrpc::read_line(&mut self.reader).map_err(link_lost)? {
            Line::Text(t) => t,
            Line::End => return Err(link_lost(io::Error::from(io::ErrorKind::UnexpectedEof))),
            Line::TooLong => {
                return Err(CmdError::new(ErrorCode::TooLarge, "the answer is too long"))
            }
        };
        let mut v: Value =
            serde_json::from_str(&line).map_err(|e| CmdError::internal(e.to_string()))?;
        if let Some(err) = v.get_mut("err").map(Value::take) {
            return Err(serde_json::from_value(err).unwrap_or_else(|_| {
                CmdError::internal("the app answered with an unreadable error")
            }));
        }
        Ok(v.get_mut("ok").map(Value::take).unwrap_or(Value::Null))
    }

    fn typed<T: serde::de::DeserializeOwned>(&mut self, op: Op) -> Result<T, CmdError> {
        let v = self.ask(op)?;
        serde_json::from_value(v).map_err(|e| CmdError::internal(format!("unreadable answer: {e}")))
    }
}

impl Host for BridgeHost {
    fn commands(&mut self) -> Result<Vec<CommandInfo>, CmdError> {
        self.typed(Op::Commands)
    }

    fn execute(&mut self, call: CommandCall) -> Result<Reply, CmdError> {
        self.typed(Op::Execute { call })
    }

    fn batch(&mut self, calls: Vec<CommandCall>, atomic: bool) -> Result<BatchReply, CmdError> {
        self.typed(Op::Batch { calls, atomic })
    }

    fn job(&mut self, job: JobId, wait: Duration) -> Result<Option<JobState>, CmdError> {
        let start = Instant::now();
        loop {
            let state: Option<JobState> = self.typed(Op::Job { job })?;
            let running = matches!(state, Some(JobState::Running { .. }));
            if !running || start.elapsed() >= wait {
                return Ok(state);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn cancel(&mut self, job: JobId) -> Result<bool, CmdError> {
        self.typed(Op::Cancel { job })
    }
}

/// Answer one request on the app's thread with `host`: what the app's frame
/// loop calls for each request that came in. `Job` is answered at once, the
/// client waits by asking again.
pub fn answer_with(host: &mut dyn Host, request: Request) {
    let outcome = match &request.op {
        Op::Commands => host.commands().and_then(to_value),
        Op::Execute { call } => host.execute(call.clone()).and_then(to_value),
        Op::Batch { calls, atomic } => host.batch(calls.clone(), *atomic).and_then(to_value),
        Op::Job { job } => host.job(*job, Duration::ZERO).and_then(to_value),
        Op::Cancel { job } => host.cancel(*job).and_then(to_value),
    };
    request.answer(outcome);
}

fn to_value<T: Serialize>(v: T) -> Result<Value, CmdError> {
    serde_json::to_value(v).map_err(|e| CmdError::internal(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::LocalHost;
    use iai_engine::{Engine, Policy, Source};

    #[test]
    fn tokens_are_new_each_time_and_compared_whole() {
        let (a, b) = (new_token().unwrap(), new_token().unwrap());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert!(same(&a, &a.clone()));
        assert!(!same(&a, &b) && !same(&a, &a[..63]));
    }

    /// A stand-in for the app: a thread that answers requests from its own
    /// engine, as the app's frames do.
    fn app() -> (Control, Arc<AtomicBool>) {
        let token = new_token().unwrap();
        let server = BridgeServer::start(token.clone(), Arc::new(|| {})).unwrap();
        let control = Control {
            port: server.port(),
            token,
            pid: std::process::id(),
        };
        let done = Arc::new(AtomicBool::new(false));
        let quit = Arc::clone(&done);
        std::thread::spawn(move || {
            let engine = Engine::new(
                iai_engine::cmds::core_registry(),
                Source::Agent,
                Policy::agent(),
            );
            let mut host = LocalHost::new(engine);
            while !quit.load(Ordering::Relaxed) {
                match server.next() {
                    Some(request) => answer_with(&mut host, request),
                    None => std::thread::sleep(Duration::from_millis(2)),
                }
            }
        });
        (control, done)
    }

    #[test]
    fn a_client_with_the_token_runs_commands_in_the_app() {
        let (control, done) = app();
        let mut host = BridgeHost::connect(&control).unwrap();
        assert!(host.commands().unwrap().iter().any(|c| c.id == "file.new"));
        let reply = host
            .execute(CommandCall::new(
                "file.new",
                json!({"widthPx": 20, "heightPx": 10}),
            ))
            .unwrap();
        assert_eq!(reply.result().unwrap()["widthPx"], json!(20));
        let e = host
            .execute(CommandCall::new("nope", json!({})))
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::UnknownCommand);
        done.store(true, Ordering::Relaxed);
    }

    #[test]
    fn a_client_without_it_is_cut_off() {
        let (control, done) = app();
        let wrong = Control {
            token: "0".repeat(64),
            ..control
        };
        assert!(BridgeHost::connect(&wrong).is_err());
        done.store(true, Ordering::Relaxed);
    }

    #[test]
    fn the_control_file_round_trips() {
        let path = std::env::temp_dir().join(format!("iai-control-{}.json", std::process::id()));
        let c = Control {
            port: 4242,
            token: new_token().unwrap(),
            pid: 7,
        };
        c.write(&path).unwrap();
        assert_eq!(Control::read(&path).unwrap(), c);
        let _ = std::fs::remove_file(path);
    }
}

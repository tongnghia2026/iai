//! The link between an agent's process and the app the person has open.
//!
//! The app listens on 127.0.0.1 (a port the system picks) and writes the
//! port and a fresh 256-bit token to a file only the user's own programs
//! read; a client must send the token first, or is cut off. After that,
//! one JSON message a line each way: `{"id", "op", …}` in,
//! `{"id", "ok": …}` or `{"id", "err": CmdError}` out. The app answers each
//! request from its own thread, a frame at a time: commands run there, on
//! the documents the person sees.
//!
//! Besides commands, the link carries what the window and an agent elsewhere
//! tell each other: a task's beginning and end, the folders the person
//! granted, and errands — work the person sent from the window, which a
//! relay (`iai.exe antigravity-relay`) takes and hands to an agent app.

use std::io::{self, BufReader};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime};

use iai_engine::{
    BatchReply, CmdError, CommandCall, CommandInfo, ErrorCode, JobId, JobState, Reply,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::host::{Grants, Host};
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
    /// The agent began a task the person asked for (`iai_task_begin`).
    TaskBegin {
        request: String,
    },
    /// The agent ended it (`iai_task_end`): "done", "failed", "cancelled".
    TaskEnd {
        status: String,
        #[serde(default)]
        title: String,
    },
    /// The folders the person granted in the window (an errand's).
    Grants,
    /// A relay asks for the next errand the person sent; `relay` names it.
    NextErrand {
        #[serde(default)]
        relay: String,
    },
    /// The relay handed the errand on: the conversation it began there.
    ErrandSent {
        errand: u64,
        conversation: String,
    },
    /// The relay could not hand it on.
    ErrandFailed {
        errand: u64,
        message: String,
    },
}

impl Op {
    /// A command for the engine (answered by [`answer_with`]), not word
    /// between the window and an agent.
    pub fn is_command(&self) -> bool {
        matches!(
            self,
            Op::Commands
                | Op::Execute { .. }
                | Op::Batch { .. }
                | Op::Job { .. }
                | Op::Cancel { .. }
        )
    }
}

/// Work the person sent from the window for an agent elsewhere (Gemini in
/// Antigravity): what a relay takes and hands on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Errand {
    pub id: u64,
    /// A few words, for the conversation's title.
    pub title: String,
    /// What the agent is asked, in full.
    pub prompt: String,
    /// The model asked for, when the person chose one.
    #[serde(default)]
    pub model: Option<String>,
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
    /// The link broke: nothing more goes through it.
    broken: bool,
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
                broken: false,
            }),
            _ => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "iAi refused the link (an old control file?)",
            )),
        }
    }

    /// The link broke (the window closed, or let AI out).
    pub fn broken(&self) -> bool {
        self.broken
    }

    fn lost(&mut self, e: io::Error) -> CmdError {
        self.broken = true;
        link_lost(e)
    }

    fn ask(&mut self, op: Op) -> Result<Value, CmdError> {
        if self.broken {
            return Err(link_lost(io::Error::from(io::ErrorKind::NotConnected)));
        }
        self.next = self.next.saturating_add(1);
        let mut message =
            serde_json::to_value(&op).map_err(|e| CmdError::internal(e.to_string()))?;
        if let Some(o) = message.as_object_mut() {
            o.insert("id".to_string(), json!(self.next));
        }
        if let Err(e) = jsonrpc::write_message(&mut self.writer, &message) {
            return Err(self.lost(e));
        }
        let line = match jsonrpc::read_line(&mut self.reader) {
            Ok(Line::Text(t)) => t,
            Ok(Line::End) => return Err(self.lost(io::Error::from(io::ErrorKind::UnexpectedEof))),
            Ok(Line::TooLong) => {
                return Err(CmdError::new(ErrorCode::TooLarge, "the answer is too long"));
            }
            Err(e) => return Err(self.lost(e)),
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

    /// The next errand the person sent from the window, if any; `relay`
    /// names who asks.
    pub fn next_errand(&mut self, relay: &str) -> Result<Option<Errand>, CmdError> {
        self.typed(Op::NextErrand {
            relay: relay.to_string(),
        })
    }

    /// Tell the window the errand was handed on, to `conversation`.
    pub fn errand_sent(&mut self, errand: u64, conversation: &str) -> Result<(), CmdError> {
        self.ask(Op::ErrandSent {
            errand,
            conversation: conversation.to_string(),
        })
        .map(|_| ())
    }

    /// Tell the window the errand could not be handed on, and why.
    pub fn errand_failed(&mut self, errand: u64, message: &str) -> Result<(), CmdError> {
        self.ask(Op::ErrandFailed {
            errand,
            message: message.to_string(),
        })
        .map(|_| ())
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

    // A window from before these were known answers them with an error:
    // what it is told is a courtesy, not a need.
    fn task_begin(&mut self, request: &str) {
        let _ = self.ask(Op::TaskBegin {
            request: request.to_string(),
        });
    }

    fn task_end(&mut self, status: &str, title: &str) {
        let _ = self.ask(Op::TaskEnd {
            status: status.to_string(),
            title: title.to_string(),
        });
    }

    fn grants(&mut self) -> Grants {
        self.typed(Op::Grants).unwrap_or_default()
    }
}

/// The window the person has open when it lets AI in, else a host of its
/// own: what `iai.exe mcp --prefer-window` runs on. The window is looked for
/// when a task begins, and before a call while nothing is open on the host
/// of its own (so no work is left behind there); a control file that led
/// nowhere is not tried again until it changes.
pub struct PreferWindow<H: Host> {
    control: PathBuf,
    window: Option<BridgeHost>,
    alone: H,
    /// The control file as it was when it last led nowhere.
    tried: Option<(Option<SystemTime>, u64)>,
}

impl<H: Host> PreferWindow<H> {
    pub fn new(control: PathBuf, alone: H) -> Self {
        let mut host = Self {
            control,
            window: None,
            alone,
            tried: None,
        };
        host.look();
        host
    }

    /// The commands go to the open window now.
    pub fn on_window(&self) -> bool {
        self.window.is_some()
    }

    /// Link up with the window when it lets AI in.
    fn look(&mut self) {
        if self.window.as_ref().is_some_and(BridgeHost::broken) {
            self.window = None;
        }
        if self.window.is_some() {
            return;
        }
        let Ok(meta) = std::fs::metadata(&self.control) else {
            self.tried = None;
            return;
        };
        let stamp = (meta.modified().ok(), meta.len());
        if self.tried == Some(stamp) {
            return;
        }
        match Control::read(&self.control).and_then(|c| BridgeHost::connect(&c)) {
            Ok(link) => {
                self.window = Some(link);
                self.tried = None;
            }
            Err(_) => self.tried = Some(stamp),
        }
    }

    /// Where calls go now, looking for the window first when that loses
    /// nothing.
    fn host(&mut self) -> &mut dyn Host {
        if self.window.as_ref().is_some_and(BridgeHost::broken) {
            self.window = None;
        }
        if self.window.is_none() && self.alone.idle() {
            self.look();
        }
        self.current()
    }

    /// Where calls go now, as it stands (a job is asked after where it runs).
    fn current(&mut self) -> &mut dyn Host {
        match self.window.as_mut() {
            Some(window) => window,
            None => &mut self.alone,
        }
    }
}

impl<H: Host> Host for PreferWindow<H> {
    fn commands(&mut self) -> Result<Vec<CommandInfo>, CmdError> {
        self.host().commands()
    }

    fn execute(&mut self, call: CommandCall) -> Result<Reply, CmdError> {
        self.host().execute(call)
    }

    fn batch(&mut self, calls: Vec<CommandCall>, atomic: bool) -> Result<BatchReply, CmdError> {
        self.host().batch(calls, atomic)
    }

    fn job(&mut self, job: JobId, wait: Duration) -> Result<Option<JobState>, CmdError> {
        self.current().job(job, wait)
    }

    fn cancel(&mut self, job: JobId) -> Result<bool, CmdError> {
        self.current().cancel(job)
    }

    fn task_begin(&mut self, request: &str) {
        self.look();
        self.current().task_begin(request);
    }

    fn task_end(&mut self, status: &str, title: &str) {
        self.current().task_end(status, title);
    }

    fn grants(&mut self) -> Grants {
        self.current().grants()
    }

    fn idle(&self) -> bool {
        self.window.is_none() && self.alone.idle()
    }
}

/// Answer one request on the app's thread with `host`: what the app's frame
/// loop calls for each command that came in (see [`Op::is_command`]). `Job`
/// is answered at once, the client waits by asking again.
pub fn answer_with(host: &mut dyn Host, request: Request) {
    let outcome = match &request.op {
        Op::Commands => host.commands().and_then(to_value),
        Op::Execute { call } => host.execute(call.clone()).and_then(to_value),
        Op::Batch { calls, atomic } => host.batch(calls.clone(), *atomic).and_then(to_value),
        Op::Job { job } => host.job(*job, Duration::ZERO).and_then(to_value),
        Op::Cancel { job } => host.cancel(*job).and_then(to_value),
        _ => Err(CmdError::new(
            ErrorCode::UnknownCommand,
            "this end of the link runs commands only",
        )),
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

    type Told = Arc<std::sync::Mutex<Vec<String>>>;

    /// A stand-in for the app: a thread that answers requests from its own
    /// engine, as the app's frames do; it keeps what it is told, holds one
    /// errand and grants the temp folder.
    fn app() -> (Control, Arc<AtomicBool>, Told) {
        let token = new_token().unwrap();
        let server = BridgeServer::start(token.clone(), Arc::new(|| {})).unwrap();
        let control = Control {
            port: server.port(),
            token,
            pid: std::process::id(),
        };
        let done = Arc::new(AtomicBool::new(false));
        let told: Told = Arc::default();
        let (quit, log) = (Arc::clone(&done), Arc::clone(&told));
        std::thread::spawn(move || {
            let engine = Engine::new(
                iai_engine::cmds::core_registry(),
                Source::Agent,
                Policy::agent(),
            );
            let mut host = LocalHost::new(engine);
            let mut errand = Some(Errand {
                id: 7,
                title: "In căn cước".to_string(),
                prompt: "Thư mục D:\\khach".to_string(),
                model: None,
            });
            while !quit.load(Ordering::Relaxed) {
                let Some(request) = server.next() else {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                };
                if request.op.is_command() {
                    answer_with(&mut host, request);
                    continue;
                }
                let line = match &request.op {
                    Op::TaskBegin { request } => format!("begin {request}"),
                    Op::TaskEnd { status, title } => format!("end {status} {title}"),
                    Op::ErrandSent {
                        errand,
                        conversation,
                    } => format!("sent {errand} {conversation}"),
                    Op::ErrandFailed { errand, message } => format!("failed {errand} {message}"),
                    other => format!("{other:?}"),
                };
                log.lock().unwrap().push(line);
                let answer = match &request.op {
                    Op::NextErrand { .. } => to_value(errand.take()),
                    Op::Grants => to_value(Grants {
                        read: Vec::new(),
                        write: vec![std::env::temp_dir()],
                    }),
                    _ => Ok(Value::Null),
                };
                request.answer(answer);
            }
        });
        (control, done, told)
    }

    #[test]
    fn a_client_with_the_token_runs_commands_in_the_app() {
        let (control, done, _) = app();
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
        let (control, done, _) = app();
        let wrong = Control {
            token: "0".repeat(64),
            ..control
        };
        assert!(BridgeHost::connect(&wrong).is_err());
        done.store(true, Ordering::Relaxed);
    }

    #[test]
    fn an_errand_goes_to_a_relay_once_and_word_comes_back() {
        let (control, done, told) = app();
        let mut relay = BridgeHost::connect(&control).unwrap();
        let errand = relay.next_errand("antigravity").unwrap().unwrap();
        assert_eq!((errand.id, errand.title.as_str()), (7, "In căn cước"));
        assert_eq!(relay.next_errand("antigravity").unwrap(), None);
        relay.errand_sent(7, "c0ffee").unwrap();
        relay.errand_failed(8, "Antigravity chưa mở").unwrap();
        // The agent's side: its task, and the folder the person granted.
        let mut agent = BridgeHost::connect(&control).unwrap();
        agent.task_begin("In căn cước: D:\\khach");
        assert_eq!(agent.grants().write, [std::env::temp_dir()]);
        agent.task_end("done", "Căn cước lên A4");
        let told = told.lock().unwrap().clone();
        assert_eq!(
            told,
            [
                "NextErrand { relay: \"antigravity\" }",
                "NextErrand { relay: \"antigravity\" }",
                "sent 7 c0ffee",
                "failed 8 Antigravity chưa mở",
                "begin In căn cước: D:\\khach",
                "Grants",
                "end done Căn cước lên A4",
            ]
        );
        done.store(true, Ordering::Relaxed);
    }

    fn file_new(host: &mut dyn Host) {
        host.execute(CommandCall::new(
            "file.new",
            json!({"widthPx": 8, "heightPx": 8}),
        ))
        .unwrap();
    }

    fn docs(host: &mut dyn Host) -> usize {
        let reply = host
            .execute(CommandCall::new("doc.list", json!({})))
            .unwrap();
        reply.result().unwrap()["docs"]
            .as_array()
            .map_or(0, Vec::len)
    }

    #[test]
    fn the_window_is_preferred_when_it_lets_ai_in_and_no_work_is_left_behind() {
        let file = std::env::temp_dir().join(format!("iai-prefer-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&file);
        let alone = LocalHost::new(Engine::new(
            iai_engine::cmds::core_registry(),
            Source::Agent,
            Policy::agent(),
        ));
        // No window: the host of its own.
        let mut host = PreferWindow::new(file.clone(), alone);
        assert!(!host.on_window() && host.idle());
        file_new(&mut host);
        file_new(&mut host);
        assert_eq!(docs(&mut host), 2);
        // The window lets AI in while documents are open here: they stay in use.
        let (control, done, told) = app();
        control.write(&file).unwrap();
        file_new(&mut host);
        assert!(!host.on_window());
        assert_eq!(docs(&mut host), 3);
        // A new task goes to the window, which has none of those.
        host.task_begin("Làm ảnh thẻ");
        assert!(host.on_window() && !host.idle());
        assert_eq!(docs(&mut host), 0);
        assert_eq!(host.grants().write, [std::env::temp_dir()]);
        assert_eq!(told.lock().unwrap()[0], "begin Làm ảnh thẻ");
        done.store(true, Ordering::Relaxed);

        // A file that leads nowhere is tried once, then left until it changes.
        let stale = Control { port: 1, ..control };
        stale.write(&file).unwrap();
        let alone = LocalHost::new(Engine::new(
            iai_engine::cmds::core_registry(),
            Source::Agent,
            Policy::agent(),
        ));
        let mut host = PreferWindow::new(file.clone(), alone);
        assert!(!host.on_window() && host.tried.is_some());
        file_new(&mut host);
        assert_eq!(docs(&mut host), 1);
        let _ = std::fs::remove_file(&file);
        host.task_begin("x");
        assert!(host.tried.is_none() && !host.on_window());
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

//! Flight recorder: what the user did, and where the app was when it stalled.
//!
//! Two halves. Inside the app, a journal: one line per user action, slow
//! frame, status message and panic, appended to `journal.log` as it happens.
//! Beside the app, a watcher: this same exe started with [`WATCH_FLAG`], which
//! reads [`Shared`] out of this process and samples its stacks whenever the UI
//! thread overruns a handler (see `watch.rs`). The watcher is a separate
//! process so that a hung or dead app can still be described.
//!
//! Both write to `%APPDATA%\IAI\diagnostics\<session>\`. A crate of its
//! own, low enough that the file formats and the GPU report through it as
//! the entry point, the event loop and the UI do (the app reaches it as
//! `crate::diag`; window and widget events are read in `crate::diag_ui`).
// The flight recorder reads this process and the one it watches through Win32.
#![allow(unsafe_code)]

use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime};

mod selftest;
#[cfg(all(windows, target_arch = "x86_64"))]
mod watch;

pub use selftest::pending as selftest_pending;

/// First argument of the exe when it is to run as the watcher.
pub const WATCH_FLAG: &str = "--diag-watch";

/// A frame or handler at least this long is written down as slow.
const SLOW_MS: u128 = 50;
/// Lines of one slow kind closer together than this are counted, not written.
const THROTTLE_GAP: Duration = Duration::from_millis(250);
/// Changes to one control closer together than this are one run.
const RUN_GAP: Duration = Duration::from_secs(1);
const JOURNAL_CAP_BYTES: u64 = 16 * 1024 * 1024;
/// The user's sessions kept, newest first.
const KEEP_SESSIONS: usize = 20;
/// Runs with no window (`iai run`, `iai mcp`) kept, apart from the user's: a
/// script that runs the exe over and over must not push the user's out.
const KEEP_AGENT_SESSIONS: usize = 5;
/// How long a session past the newest is still kept when the user marked a
/// problem in it, or the app froze, hung or died in it.
const KEEP_FLAGGED_FOR: Duration = Duration::from_secs(30 * 24 * 3600);
/// A session whose journal was written to this recently may belong to a
/// running instance, and is not removed.
const MAYBE_RUNNING: Duration = Duration::from_secs(12 * 3600);
/// An empty session folder this young may be one being opened right now.
const OPENING: Duration = Duration::from_secs(10 * 60);
/// What a windowless run's journal says it was asked to do begins with this;
/// the pruning tells those sessions from the user's by it.
const AGENT_TAG: &str = "agent:";
/// Incident kinds, as `INDEX.log` names them, that keep their session for
/// [`KEEP_FLAGGED_FOR`].
const SEVERE_INCIDENTS: [&str; 4] = ["HANG", "FREEZE", "EXIT HANG", "ABNORMAL EXIT"];
const MARK_TEXT: &str = "======== the user marked a problem here ========";
/// The journal's last line when the app left on its own terms. The watcher
/// takes a session that ends without it for a crash.
const CLEAN_END: &str = "session closed normally";

const MAGIC: u64 = 0x6941_695f_6469_6167;

/// What the UI thread is in the middle of, as the watcher reads it.
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Idle = 0,
    Redraw = 1,
    Key = 2,
    Mouse = 3,
    Move = 4,
    Wheel = 5,
    Window = 6,
    Wait = 7,
    DevelopRedraw = 8,
    DevelopInput = 9,
}

impl Phase {
    #[cfg_attr(not(all(windows, target_arch = "x86_64")), allow(dead_code))]
    fn from_code(code: u32) -> Phase {
        match code {
            1 => Phase::Redraw,
            2 => Phase::Key,
            3 => Phase::Mouse,
            4 => Phase::Move,
            5 => Phase::Wheel,
            6 => Phase::Window,
            7 => Phase::Wait,
            8 => Phase::DevelopRedraw,
            9 => Phase::DevelopInput,
            _ => Phase::Idle,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Redraw => "main window frame",
            Phase::Key => "key",
            Phase::Mouse => "mouse button",
            Phase::Move => "mouse move",
            Phase::Wheel => "mouse wheel",
            Phase::Window => "window event",
            Phase::Wait => "event-loop tail",
            Phase::DevelopRedraw => "Develop window frame",
            Phase::DevelopInput => "Develop window input",
        }
    }
}

/// The few words the watcher reads out of this process, every few
/// milliseconds, through `ReadProcessMemory`. Plain atomics, no pointers.
#[repr(C)]
struct Shared {
    magic: AtomicU64,
    /// Bumped each time the UI thread enters a handler.
    busy_seq: AtomicU64,
    frames: AtomicU64,
    ui_thread: AtomicU32,
    /// A [`Phase`]; zero between handlers.
    phase: AtomicU32,
    /// 1 once the event loop has returned, 2 once `main` is about to.
    exiting: AtomicU32,
    _pad: AtomicU32,
}

static SHARED: Shared = Shared {
    magic: AtomicU64::new(MAGIC),
    busy_seq: AtomicU64::new(0),
    frames: AtomicU64::new(0),
    ui_thread: AtomicU32::new(0),
    phase: AtomicU32::new(0),
    exiting: AtomicU32::new(0),
    _pad: AtomicU32::new(0),
};

thread_local! {
    static WORK_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Marks the UI thread as inside a handler for as long as it lives.
pub struct UiWork {
    phase: Phase,
    since: Instant,
}

/// Enter a UI-thread handler. Hold the result to the end of it.
pub fn ui_work(phase: Phase) -> UiWork {
    let depth = WORK_DEPTH.get();
    WORK_DEPTH.set(depth + 1);
    if depth == 0 {
        SHARED.busy_seq.fetch_add(1, Ordering::Relaxed);
        SHARED.phase.store(phase as u32, Ordering::Release);
    }
    UiWork {
        phase,
        since: Instant::now(),
    }
}

impl Drop for UiWork {
    fn drop(&mut self) {
        let depth = WORK_DEPTH.get().saturating_sub(1);
        WORK_DEPTH.set(depth);
        if depth == 0 {
            SHARED.phase.store(0, Ordering::Release);
        }
        // The main window's frame accounts for itself, in more detail (`frame`).
        let ms = self.since.elapsed().as_millis();
        if ms >= SLOW_MS && self.phase != Phase::Redraw {
            if let Some(mut journal) = journal() {
                if let Some(skipped) = journal.slow_events.pass(ms) {
                    let text = format!("{} handler ran {ms} ms{skipped}", self.phase.label());
                    journal.line("slow", &text);
                }
            }
        }
    }
}

/// Counts lines that come too fast instead of writing each.
#[derive(Default)]
struct Throttle {
    last: Option<Instant>,
    skipped: u32,
    worst: u128,
}

impl Throttle {
    /// `None` while lines of this kind are coming too fast; otherwise what to
    /// append about the ones that were skipped.
    fn pass(&mut self, ms: u128) -> Option<String> {
        let now = Instant::now();
        if self
            .last
            .is_some_and(|last| now.duration_since(last) < THROTTLE_GAP)
        {
            self.skipped += 1;
            self.worst = self.worst.max(ms);
            return None;
        }
        self.last = Some(now);
        let tail = if self.skipped > 0 {
            format!(
                " [+{} more since the last line, worst {} ms]",
                self.skipped, self.worst
            )
        } else {
            String::new()
        };
        self.skipped = 0;
        self.worst = 0;
        Some(tail)
    }
}

/// Changes to one control in quick succession, held back so that a dragged
/// slider is two lines — where it started and where it settled — not hundreds.
struct Run {
    kind: &'static str,
    key: String,
    last: String,
    at: Instant,
    at_text: String,
    count: u32,
}

struct Journal {
    out: Box<dyn Write + Send>,
    bytes: u64,
    run: Option<Run>,
    status: String,
    context: u64,
    slow_frames: Throttle,
    slow_events: Throttle,
}

impl Journal {
    fn new(out: Box<dyn Write + Send>) -> Journal {
        Journal {
            out,
            bytes: 0,
            run: None,
            status: String::new(),
            context: 0,
            slow_frames: Throttle::default(),
            slow_events: Throttle::default(),
        }
    }

    fn line(&mut self, kind: &str, text: &str) {
        self.end_run();
        self.write(&time_of(SystemTime::now()), kind, text);
    }

    /// One change to a control: written at once when it starts a run, folded
    /// into the run otherwise.
    fn change(&mut self, kind: &'static str, key: &str, value: &str) {
        let now = Instant::now();
        if let Some(run) = &mut self.run {
            if run.kind == kind && run.key == key && now.duration_since(run.at) < RUN_GAP {
                run.last.clear();
                run.last.push_str(value);
                run.at = now;
                run.at_text = time_of(SystemTime::now());
                run.count += 1;
                return;
            }
        }
        self.end_run();
        let at_text = time_of(SystemTime::now());
        self.write(&at_text, kind, join(key, value).as_str());
        self.run = Some(Run {
            kind,
            key: key.to_owned(),
            last: value.to_owned(),
            at: now,
            at_text,
            count: 1,
        });
    }

    fn end_run(&mut self) {
        if let Some(run) = self.run.take() {
            if run.count > 1 {
                let text = format!(
                    "{} ({} changes in a row)",
                    join(&run.key, &format!("\u{2026} {}", run.last)),
                    run.count
                );
                self.write(&run.at_text, run.kind, &text);
            }
        }
    }

    fn write(&mut self, at: &str, kind: &str, text: &str) {
        if self.bytes >= JOURNAL_CAP_BYTES {
            return;
        }
        let mut block = String::with_capacity(text.len() + 32);
        let mut lines = text.lines();
        let _ = writeln!(block, "{at} {kind:<6} {}", lines.next().unwrap_or(""));
        for line in lines {
            let _ = writeln!(block, "{:20}{line}", "");
        }
        self.bytes += block.len() as u64;
        if self.bytes >= JOURNAL_CAP_BYTES {
            block.push_str("-- journal full: nothing more is recorded this session --\n");
        }
        // Unbuffered on purpose: the line has to be on disk if the app dies next.
        let _ = self.out.write_all(block.as_bytes());
    }
}

fn join(key: &str, value: &str) -> String {
    match (key.is_empty(), value.is_empty()) {
        (true, _) => value.to_owned(),
        (_, true) => key.to_owned(),
        _ => format!("{key} {value}"),
    }
}

static JOURNAL: OnceLock<Mutex<Journal>> = OnceLock::new();
static SESSION_DIR: OnceLock<PathBuf> = OnceLock::new();

fn journal() -> Option<MutexGuard<'static, Journal>> {
    JOURNAL
        .get()
        .map(|journal| journal.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Seconds to add to UTC for this machine's wall clock, read once.
fn utc_offset_secs() -> i64 {
    static OFFSET: OnceLock<i64> = OnceLock::new();
    *OFFSET.get_or_init(|| {
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::SYSTEMTIME;
            use windows_sys::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};
            let mut local: SYSTEMTIME = unsafe { std::mem::zeroed() };
            let mut utc: SYSTEMTIME = unsafe { std::mem::zeroed() };
            unsafe {
                GetLocalTime(&mut local);
                GetSystemTime(&mut utc);
            }
            let secs = |t: &SYSTEMTIME| {
                days_from_civil(t.wYear as i64, t.wMonth as i64, t.wDay as i64) * 86_400
                    + t.wHour as i64 * 3600
                    + t.wMinute as i64 * 60
                    + t.wSecond as i64
            };
            // The two reads can straddle a second; zones sit on quarter hours.
            let diff = secs(&local) - secs(&utc);
            (diff as f64 / 900.0).round() as i64 * 900
        }
        #[cfg(not(windows))]
        {
            0
        }
    })
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `t` on this machine's wall clock, as (days since 1970, millis into the day).
fn local_parts(t: SystemTime) -> (i64, i64) {
    let since = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let ms = since.as_millis() as i64 + utc_offset_secs() * 1000;
    (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000))
}

/// `t` on this machine's wall clock, as seconds since 1970.
fn local_secs(t: SystemTime) -> i64 {
    let (days, ms) = local_parts(t);
    days * 86_400 + ms / 1000
}

/// `HH:MM:SS.mmm`, local.
fn time_of(t: SystemTime) -> String {
    let (_, ms) = local_parts(t);
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// `YYYY-MM-DD`, local.
fn date_of(t: SystemTime) -> String {
    let (y, m, d) = civil_from_days(local_parts(t).0);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `DD/MM/YYYY HH:MM`, local: a moment as the user is told it.
pub fn local_minute(t: SystemTime) -> String {
    let (days, ms) = local_parts(t);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{d:02}/{m:02}/{y:04} {:02}:{:02}",
        ms / 3_600_000,
        ms / 60_000 % 60
    )
}

/// The app's own folder, `%APPDATA%\IAI`, falling back to the temp directory.
pub fn data_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("IAI")
}

/// Where every session's folder lives: `%APPDATA%\IAI\diagnostics`.
pub fn root_dir() -> PathBuf {
    data_dir().join("diagnostics")
}

fn is_session_dir_name(name: &str) -> bool {
    let b = name.as_bytes();
    // On bytes: a stray folder with a non-ASCII name must not be sliced mid-character.
    b.len() > 23 && b[4] == b'-' && b[7] == b'-' && b[10] == b'_' && &b[19..23] == b"_pid"
}

/// When a session began, in [`local_secs`], read from its folder's name.
fn session_began(name: &str) -> Option<i64> {
    let num = |range: std::ops::Range<usize>| name.get(range)?.parse::<i64>().ok();
    let days = days_from_civil(num(0..4)?, num(5..7)?, num(8..10)?);
    Some(days * 86_400 + num(11..13)? * 3600 + num(14..16)? * 60 + num(17..19)?)
}

/// One line of `INDEX.log`, where every session's incidents are listed together.
#[cfg_attr(not(all(windows, target_arch = "x86_64")), allow(dead_code))]
fn index_line(at: SystemTime, kind: &str, span: &str, about: &str, session: &str) -> String {
    format!(
        "{} {}  {kind:<13} {span:<8} {about}  ->  {session}\\incidents.log\n",
        date_of(at),
        &time_of(at)[..8],
    )
}

/// The session an `INDEX.log` line is about, when its incident is one of
/// [`SEVERE_INCIDENTS`].
fn severe_incident_session(line: &str) -> Option<&str> {
    let (head, target) = line.rsplit_once("  ->  ")?;
    // The kind follows `YYYY-MM-DD HH:MM:SS  `; one inside a session is numbered.
    let kind = head.get(21..)?;
    let kind = match kind.strip_prefix('#') {
        Some(numbered) => numbered.split_once(' ')?.1,
        None => kind,
    };
    let severe = SEVERE_INCIDENTS.iter().any(|name| {
        kind.strip_prefix(name)
            .is_some_and(|rest| rest.starts_with(' '))
    });
    if !severe {
        return None;
    }
    target.split('\\').next()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SessionKind {
    User,
    /// A run with no window, by the line [`start_without_window`] opens its journal with.
    Agent,
    /// Nothing inside: what a removal that could not finish leaves behind.
    Empty,
}

fn session_kind(dir: &Path) -> SessionKind {
    use std::io::Read;
    if std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_none()) {
        return SessionKind::Empty;
    }
    let mut head = Vec::new();
    if let Ok(file) = std::fs::File::open(dir.join("journal.log")) {
        let _ = file.take(4096).read_to_end(&mut head);
    }
    let agent = String::from_utf8_lossy(&head).lines().take(8).any(|line| {
        let mut words = line.split_whitespace();
        words.nth(1) == Some("start") && words.next() == Some(AGENT_TAG)
    });
    if agent {
        SessionKind::Agent
    } else {
        SessionKind::User
    }
}

fn has_mark(dir: &Path) -> bool {
    std::fs::read(dir.join("journal.log"))
        .is_ok_and(|bytes| String::from_utf8_lossy(&bytes).contains(MARK_TEXT))
}

/// Whether the session's journal was written to less than `within` before `now`.
fn written_within(dir: &Path, now: SystemTime, within: Duration) -> bool {
    std::fs::metadata(dir.join("journal.log"))
        .and_then(|m| m.modified())
        // One written after `now` counts as recent: the clock has gone back.
        .is_ok_and(|at| now.duration_since(at).map_or(true, |age| age < within))
}

/// Run from inside the Claude app (a packaged app), `remove_dir_all` deletes
/// a session's files and then fails on its folder with a sharing violation;
/// a plain `remove_dir` still takes the folder.
fn remove_session_dir(dir: &Path) {
    if std::fs::remove_dir_all(dir).is_err() {
        let _ = std::fs::remove_dir(dir);
    }
}

/// Keep the user's newest sessions and, apart from them, a few of the runs
/// with no window; drop the rest, and empty folders. A session past those is
/// still kept while its journal was written to in the last half day (it may
/// belong to a running instance), and for a month when the user marked a
/// problem in it or the app froze, hung or died in it.
fn prune_old_sessions(root: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| is_session_dir_name(name))
        .collect();
    names.sort_unstable_by(|a, b| b.cmp(a));
    let severe: std::collections::HashSet<String> = std::fs::read(root.join("INDEX.log"))
        .map(|bytes| {
            String::from_utf8_lossy(&bytes)
                .lines()
                .filter_map(severe_incident_session)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let now_local = local_secs(now);
    let age_secs = |name: &str| session_began(name).map(|began| now_local - began);
    let (mut users, mut agents) = (0, 0);
    for name in names {
        let dir = root.join(&name);
        let newest = match session_kind(&dir) {
            SessionKind::Empty => {
                if age_secs(&name).is_none_or(|age| age >= OPENING.as_secs() as i64) {
                    let _ = std::fs::remove_dir(&dir);
                }
                continue;
            }
            SessionKind::User => {
                users += 1;
                users <= KEEP_SESSIONS
            }
            SessionKind::Agent => {
                agents += 1;
                agents <= KEEP_AGENT_SESSIONS
            }
        };
        if newest || written_within(&dir, now, MAYBE_RUNNING) {
            continue;
        }
        let young = age_secs(&name).is_some_and(|age| age < KEEP_FLAGGED_FOR.as_secs() as i64);
        if young && (severe.contains(&name) || has_mark(&dir)) {
            continue;
        }
        remove_session_dir(&dir);
    }
}

#[cfg(windows)]
fn current_thread_id() -> u32 {
    unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() }
}

#[cfg(not(windows))]
fn current_thread_id() -> u32 {
    0
}

#[cfg(all(windows, target_arch = "x86_64"))]
fn spawn_watcher(dir: &Path) -> Result<u32, String> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    if std::env::var_os("IAI_DIAG_WATCH").is_some_and(|v| v == "0") {
        return Err("switched off by IAI_DIAG_WATCH=0".to_owned());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let child = Command::new(exe)
        .arg(WATCH_FLAG)
        .arg(std::process::id().to_string())
        .arg(format!("{:x}", &SHARED as *const Shared as usize))
        .arg(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // No console of its own, and none shared with the app's.
        .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(child.id())
}

#[cfg(not(all(windows, target_arch = "x86_64")))]
fn spawn_watcher(_dir: &Path) -> Result<u32, String> {
    Err("stack sampling is only built for 64-bit Windows".to_owned())
}

/// Open this session's journal and start the watcher. Call once, from `main`,
/// on the thread that will run the event loop, with the build the code is of
/// (`"<commit> (<day>)"`). `IAI_DIAG=0` switches it off.
pub fn start(build: &str) {
    if open_journal(build, "") {
        match spawn_watcher_for_session() {
            Ok(pid) => note("start", &format!("watcher running, pid {pid}")),
            Err(why) => note("start", &format!("no watcher: {why}")),
        }
        selftest::arm();
    }
}

/// Open a journal for a process with no window (`iai.exe mcp`, `iai.exe
/// run`): what it ran is written down, and nothing watches a UI thread.
pub fn start_without_window(build: &str, what: &str) {
    open_journal(build, &agent_what(what));
}

/// `what` as a windowless run's journal opens with it: after [`AGENT_TAG`].
fn agent_what(what: &str) -> String {
    if what.starts_with(AGENT_TAG) {
        what.to_owned()
    } else {
        format!("{AGENT_TAG} {what}")
    }
}

fn spawn_watcher_for_session() -> Result<u32, String> {
    match SESSION_DIR.get() {
        Some(dir) => spawn_watcher(dir),
        None => Err("no session folder".to_owned()),
    }
}

/// Open this session's journal; whether it is open.
fn open_journal(build: &str, what: &str) -> bool {
    if std::env::var_os("IAI_DIAG").is_some_and(|v| v == "0") {
        return false;
    }
    SHARED
        .ui_thread
        .store(current_thread_id(), Ordering::Relaxed);
    let now = SystemTime::now();
    let root = root_dir();
    let dir = root.join(format!(
        "{}_{}_pid{}",
        date_of(now),
        time_of(now)[..8].replace(':', "-"),
        std::process::id()
    ));
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("journal.log"))
    else {
        return false;
    };
    if JOURNAL
        .set(Mutex::new(Journal::new(Box::new(file))))
        .is_err()
    {
        return false;
    }
    let _ = SESSION_DIR.set(dir.clone());
    prune_old_sessions(&root, now);

    let exe = std::env::current_exe().ok();
    let built = exe
        .as_ref()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .map(|at| format!("{} {}", date_of(at), &time_of(at)[..8]))
        .unwrap_or_else(|| "?".to_owned());
    note(
        "start",
        &format!(
            "iAi {} \u{2014} {} pid {}\nexe {} (built {built})",
            env!("CARGO_PKG_VERSION"),
            date_of(now),
            std::process::id(),
            exe.as_deref().unwrap_or(Path::new("?")).display(),
        ),
    );
    note("start", &format!("code {build}"));
    if !what.is_empty() {
        note("start", what);
    }
    true
}

/// This session's folder, once [`start`] has run.
pub fn session_dir() -> Option<&'static Path> {
    SESSION_DIR.get().map(PathBuf::as_path)
}

/// Show the diagnostics folder in the file manager.
pub fn open_folder() {
    let dir = root_dir();
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer.exe").arg(&dir).spawn();
    #[cfg(not(windows))]
    let _ = dir;
}

/// When the exe was started as the watcher: do that job and report `true`, so
/// `main` returns without ever becoming the app.
pub fn run_watcher_if_requested() -> bool {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new(WATCH_FLAG)) {
        return false;
    }
    #[cfg(all(windows, target_arch = "x86_64"))]
    watch::run(args.collect());
    true
}

/// One journal line. `kind` is a short tag (`key`, `ui`, `perf`…).
pub fn note(kind: &str, text: &str) {
    if let Some(mut journal) = journal() {
        journal.line(kind, text);
    }
}

/// A change that tends to come in bursts (a window being resized, a value
/// being dragged): the first is written, the rest of the run summed up.
pub fn note_change(kind: &'static str, key: &str, value: &str) {
    if let Some(mut journal) = journal() {
        journal.change(kind, key, value);
    }
}

/// The main window finished a frame. `describe` is only run for a slow one.
pub fn frame(took: Duration, describe: impl FnOnce() -> String) {
    SHARED.frames.fetch_add(1, Ordering::Relaxed);
    let ms = took.as_millis();
    if ms >= SLOW_MS {
        if let Some(mut journal) = journal() {
            if let Some(skipped) = journal.slow_frames.pass(ms) {
                let text = format!("frame {ms} ms ({}){skipped}", describe());
                journal.line("slow", &text);
            }
        }
    }
    selftest::step();
}

/// The status-bar text, written whenever it changes: it is where most
/// operations say how they ended.
pub fn status(text: &str) {
    let Some(mut journal) = journal() else {
        return;
    };
    if text.is_empty() || journal.status == text {
        return;
    }
    journal.status.clear();
    journal.status.push_str(text);
    journal.change("status", "", text);
}

/// What the user has in front of them (tool, document, open dialog), written
/// whenever `key` — a hash of it — changes. `describe` only runs then.
pub fn context(key: u64, describe: impl FnOnce() -> String) {
    let Some(mut journal) = journal() else {
        return;
    };
    if journal.context == key {
        return;
    }
    journal.context = key;
    let text = describe();
    journal.line("now", &text);
}

/// Icon-font glyphs and line breaks would make a label unreadable in a log.
pub fn readable(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    for c in label.chars().take(80) {
        match c {
            '\u{e000}'..='\u{f8ff}' => {
                let _ = write!(out, "[icon {:04X}]", c as u32);
            }
            '\n' | '\r' | '\t' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Whether this session keeps a journal (`start` ran, `IAI_DIAG` is not 0).
pub fn recording() -> bool {
    JOURNAL.get().is_some()
}

/// A widget the user touched: `changing` when it is one of a run of value
/// changes (written once the run ends), else a line of its own.
pub fn widget(changing: bool, what: &str, value: &str) {
    if changing {
        note_change("ui", what, value);
    } else {
        note("ui", &join(what, value));
    }
}

/// The user's "it just went wrong here" mark. Returns the time written.
pub fn mark() -> Option<String> {
    let mut journal = journal()?;
    let at = time_of(SystemTime::now());
    journal.line("MARK", MARK_TEXT);
    Some(at[..8].to_owned())
}

/// From the panic hook: the message and where the panicking thread stood.
pub fn panic(text: &str) {
    let Some(journal) = JOURNAL.get() else {
        return;
    };
    let trace = std::backtrace::Backtrace::force_capture().to_string();
    // `try_lock`: the panic may have come from under the journal's own lock.
    let mut journal = match journal.try_lock() {
        Ok(journal) => journal,
        Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return,
    };
    journal.line("PANIC", &format!("{text}\n{trace}"));
}

/// The event loop has returned; what follows is teardown.
pub fn exiting() {
    SHARED.exiting.store(1, Ordering::Release);
    note("end", "event loop returned");
    selftest::linger_at_exit();
}

/// The session ended on its own terms. Call last, from `main`.
pub fn finish() {
    SHARED.exiting.store(2, Ordering::Release);
    note("end", CLEAN_END);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Sink {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    #[test]
    fn calendar_round_trips() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        for (y, m, d) in [(2026, 10, 5), (2024, 2, 29), (2000, 12, 31), (1999, 3, 1)] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
        assert_eq!(days_from_civil(2026, 10, 5), 20_731);
    }

    #[test]
    fn session_folder_names_are_told_from_other_folders() {
        assert!(is_session_dir_name("2026-10-05_15-42-10_pid1234"));
        assert!(!is_session_dir_name("autosave"));
        assert!(!is_session_dir_name("2026-10-05_15-42-10"));
        assert_eq!(
            session_began("2026-10-05_15-42-10_pid1234"),
            Some(days_from_civil(2026, 10, 5) * 86_400 + 15 * 3600 + 42 * 60 + 10)
        );
    }

    /// A folder of the test's own under the temp directory, empty.
    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iai-diag-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// When the session in the folder `name` began.
    fn began(name: &str) -> SystemTime {
        let utc = session_began(name).unwrap() - utc_offset_secs();
        SystemTime::UNIX_EPOCH + Duration::from_secs(utc as u64)
    }

    /// A session folder whose journal holds `lines`, last written at `written`.
    fn session_written(root: &Path, name: &str, lines: &[(&str, &str)], written: SystemTime) {
        let sink = Sink::default();
        let mut journal = Journal::new(Box::new(sink.clone()));
        for (kind, text) in lines {
            journal.line(kind, text);
        }
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("journal.log");
        std::fs::write(&path, sink.text()).unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(written).unwrap();
    }

    /// A session folder whose journal holds `lines`, last written as it began.
    fn session(root: &Path, name: &str, lines: &[(&str, &str)]) {
        session_written(root, name, lines, began(name));
    }

    const USER_START: [(&str, &str); 2] = [
        (
            "start",
            "iAi 0.1.0 \u{2014} 2026-10-01 pid 1\nexe C:\\iAi\\iai.exe",
        ),
        ("start", "watcher running, pid 2"),
    ];

    /// The user's twenty newest sessions: 1 to 20 October, at eight.
    fn twenty_user_sessions(root: &Path) -> Vec<String> {
        (1..=20)
            .map(|day| {
                let name = format!("2026-10-{day:02}_08-00-00_pid{day}");
                session(root, &name, &USER_START);
                name
            })
            .collect()
    }

    fn folders(root: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(root)
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    fn noon_20_october() -> SystemTime {
        began("2026-10-20_12-00-00_pid1")
    }

    #[test]
    fn windowless_runs_keep_a_few_of_their_own_and_leave_the_users_sessions_alone() {
        let root = scratch("prune-agent");
        let mut expected = twenty_user_sessions(&root);
        session(&root, "2026-09-30_08-00-00_pid99", &USER_START);
        // A script ran the exe 21 times in a row, the night before.
        let runs: Vec<String> = (0..21)
            .map(|i| {
                let name = format!("2026-10-19_23-{:02}-00_pid{}", 13 + i, 100 + i);
                let what = if i % 2 == 0 {
                    agent_what("agent: iai run job.json")
                } else {
                    agent_what("iai mcp")
                };
                let lines = [
                    (
                        "start",
                        "iAi 0.1.0 \u{2014} 2026-10-19 pid 100\nexe C:\\iAi\\iai.exe",
                    ),
                    ("start", "code abc"),
                    ("start", what.as_str()),
                    ("cmd", "agent file.open {}"),
                ];
                session(&root, &name, &lines);
                name
            })
            .collect();
        prune_old_sessions(&root, noon_20_october());
        expected.extend_from_slice(&runs[21 - KEEP_AGENT_SESSIONS..]);
        expected.sort();
        assert_eq!(folders(&root), expected);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_session_marked_or_frozen_in_is_kept_for_a_month_past_the_newest() {
        let root = scratch("prune-flagged");
        let mut expected = twenty_user_sessions(&root);
        let mark = [USER_START[0], ("MARK", MARK_TEXT), ("key", "Ctrl+Z")];
        let incidents: [(&str, &str); 7] = [
            ("2026-09-24_09-00-00_pid202", "#3 HANG"),
            ("2026-09-23_09-00-00_pid203", "#12 FREEZE"),
            ("2026-09-22_09-00-00_pid204", "ABNORMAL EXIT"),
            ("2026-09-22_10-00-00_pid205", "EXIT HANG"),
            ("2026-09-21_09-00-00_pid206", "#1 STALL"),
            ("2026-09-21_09-00-00_pid206", "#2 DIALOG WAIT"),
            ("2026-09-11_09-00-00_pid207", "#1 HANG"),
        ];
        let mut index = String::new();
        for (name, kind) in incidents {
            session(&root, name, &USER_START);
            index.push_str(&index_line(
                began(name),
                kind,
                "6 s",
                "iai::app::App::frame",
                name,
            ));
        }
        std::fs::write(root.join("INDEX.log"), index).unwrap();
        session(&root, "2026-09-25_09-00-00_pid201", &mark);
        session(&root, "2026-09-10_09-00-00_pid208", &mark);
        // Still open: its journal was written an hour ago.
        let now = noon_20_october();
        let open = now - Duration::from_secs(3600);
        session_written(&root, "2026-09-26_09-00-00_pid209", &USER_START, open);

        prune_old_sessions(&root, now);
        expected.extend(
            [
                "2026-09-22_09-00-00_pid204",
                "2026-09-22_10-00-00_pid205",
                "2026-09-23_09-00-00_pid203",
                "2026-09-24_09-00-00_pid202",
                "2026-09-25_09-00-00_pid201",
                "2026-09-26_09-00-00_pid209",
            ]
            .map(str::to_owned),
        );
        expected.sort();
        // Gone: the one with only short stalls, and those older than a month.
        assert_eq!(folders(&root), expected);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn empty_session_folders_are_cleared_away_and_not_counted() {
        let root = scratch("prune-empty");
        let mut expected = twenty_user_sessions(&root);
        for name in [
            "2026-10-15_06-39-04_pid22580",
            "2026-10-20_09-00-00_pid301",
            "2026-10-20_09-30-00_pid302",
            "2026-10-20_10-00-00_pid303",
            // Being opened by an instance starting right now.
            "2026-10-20_11-58-00_pid304",
            "autosave",
        ] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        prune_old_sessions(&root, noon_20_october());
        expected.extend(["2026-10-20_11-58-00_pid304", "autosave"].map(str::to_owned));
        expected.sort();
        assert_eq!(folders(&root), expected);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn index_lines_name_the_session_of_a_freeze_hang_or_crash() {
        let session = "2026-10-19_09-23-51_pid10396";
        let at = began(session);
        for (kind, severe) in [
            ("#28 STALL", false),
            ("#1 LAG BURST", false),
            ("#2 DIALOG WAIT", false),
            ("#3 HANG", true),
            ("#112 FREEZE", true),
            ("EXIT HANG", true),
            ("ABNORMAL EXIT", true),
        ] {
            let line = index_line(
                at,
                kind,
                "1.2 s",
                "iai::tools::crop::CropTool::commit",
                session,
            );
            assert_eq!(
                severe_incident_session(line.trim_end()),
                severe.then_some(session),
                "{line}"
            );
        }
    }

    #[test]
    fn a_dragged_value_is_its_first_and_last_line() {
        let sink = Sink::default();
        let mut journal = Journal::new(Box::new(sink.clone()));
        for v in 1..=40 {
            journal.change("ui", "Slider \"Smooth\"", &format!("= {v}"));
        }
        journal.line("key", "Enter");
        let text = sink.text();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert!(lines[0].ends_with("Slider \"Smooth\" = 1"));
        assert!(lines[1].ends_with("Slider \"Smooth\" \u{2026} = 40 (40 changes in a row)"));
        assert!(lines[2].ends_with("Enter"));
    }

    #[test]
    fn a_single_change_is_a_single_line() {
        let sink = Sink::default();
        let mut journal = Journal::new(Box::new(sink.clone()));
        journal.change("ui", "Checkbox \"Snap\"", "[on]");
        journal.change("ui", "Checkbox \"Grid\"", "[on]");
        journal.end_run();
        assert_eq!(sink.text().lines().count(), 2);
    }

    #[test]
    fn continuation_lines_are_indented_under_the_text() {
        let sink = Sink::default();
        let mut journal = Journal::new(Box::new(sink.clone()));
        journal.line("PANIC", "boom\nframe one");
        let text = sink.text();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0].find("boom"), lines[1].find("frame one"));
    }

    #[test]
    fn slow_lines_closer_than_the_gap_are_counted_not_written() {
        let mut throttle = Throttle::default();
        assert_eq!(throttle.pass(60).as_deref(), Some(""));
        assert_eq!(throttle.pass(90), None);
        assert_eq!(throttle.pass(70), None);
        std::thread::sleep(THROTTLE_GAP + Duration::from_millis(20));
        assert_eq!(
            throttle.pass(55).as_deref(),
            Some(" [+2 more since the last line, worst 90 ms]")
        );
    }

    #[test]
    fn icon_glyphs_are_named_not_printed() {
        assert_eq!(readable("\u{e4f6} Crop\nnow"), "[icon E4F6] Crop now");
    }

    #[test]
    fn nested_handlers_keep_the_outer_phase() {
        let outer = ui_work(Phase::Key);
        let seq = SHARED.busy_seq.load(Ordering::Relaxed);
        {
            let _inner = ui_work(Phase::Redraw);
            assert_eq!(SHARED.busy_seq.load(Ordering::Relaxed), seq);
        }
        assert_eq!(SHARED.phase.load(Ordering::Relaxed), Phase::Key as u32);
        drop(outer);
        assert_eq!(SHARED.phase.load(Ordering::Relaxed), 0);
    }
}

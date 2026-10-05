//! Flight recorder: what the user did, and where the app was when it stalled.
//!
//! Two halves. Inside the app, a journal: one line per user action, slow
//! frame, status message and panic, appended to `journal.log` as it happens.
//! Beside the app, a watcher: this same exe started with [`WATCH_FLAG`], which
//! reads [`Shared`] out of this process and samples its stacks whenever the UI
//! thread overruns a handler (see `watch.rs`). The watcher is a separate
//! process so that a hung or dead app can still be described.
//!
//! Both write to `%APPDATA%\IAI\diagnostics\<session>\`. Lives at the library
//! root, like `crash`, because the entry point, the event loop and the UI all
//! report through it.

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
const KEEP_SESSIONS: usize = 20;
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
    /// The phase a window event is handled under.
    pub fn of(event: &winit::event::WindowEvent, develop_window: bool) -> Phase {
        use winit::event::WindowEvent as E;
        match (event, develop_window) {
            (E::RedrawRequested, false) => Phase::Redraw,
            (E::RedrawRequested, true) => Phase::DevelopRedraw,
            (_, true) => Phase::DevelopInput,
            (E::KeyboardInput { .. }, _) => Phase::Key,
            (E::MouseInput { .. }, _) => Phase::Mouse,
            (E::CursorMoved { .. }, _) => Phase::Move,
            (E::MouseWheel { .. }, _) => Phase::Wheel,
            _ => Phase::Window,
        }
    }

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

/// Where every session's folder lives: `%APPDATA%\IAI\diagnostics`.
pub fn root_dir() -> PathBuf {
    crate::crash::crash_log_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir)
        .join("diagnostics")
}

fn is_session_dir_name(name: &str) -> bool {
    let b = name.as_bytes();
    // On bytes: a stray folder with a non-ASCII name must not be sliced mid-character.
    b.len() > 23 && b[4] == b'-' && b[7] == b'-' && b[10] == b'_' && &b[19..23] == b"_pid"
}

/// Keep the newest sessions, drop the rest. A folder whose journal was written
/// to in the last half day is left alone: it may belong to a running instance.
fn prune_old_sessions(root: &Path) {
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
    for name in names.into_iter().skip(KEEP_SESSIONS) {
        let dir = root.join(name);
        let recent = std::fs::metadata(dir.join("journal.log"))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|age| age < Duration::from_secs(12 * 3600));
        if !recent {
            let _ = std::fs::remove_dir_all(dir);
        }
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
/// on the thread that will run the event loop. `IAI_DIAG=0` switches it off.
pub fn start() {
    if std::env::var_os("IAI_DIAG").is_some_and(|v| v == "0") {
        return;
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
        return;
    }
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("journal.log"))
    else {
        return;
    };
    if JOURNAL
        .set(Mutex::new(Journal::new(Box::new(file))))
        .is_err()
    {
        return;
    }
    let _ = SESSION_DIR.set(dir.clone());
    prune_old_sessions(&root);

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
    match spawn_watcher(&dir) {
        Ok(pid) => note("start", &format!("watcher running, pid {pid}")),
        Err(why) => note("start", &format!("no watcher: {why}")),
    }
    selftest::arm();
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
fn readable(label: &str) -> String {
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

/// What egui says the user just did to a widget: clicks, toggles, value edits.
pub fn widget_event(event: &egui::output::OutputEvent) {
    if JOURNAL.get().is_none() {
        return;
    }
    match widget_line(event) {
        Some((true, what, value)) => note_change("ui", &what, &value),
        Some((false, what, value)) => note("ui", &join(&what, &value)),
        None => {}
    }
}

/// A widget event as journal text: whether it is one of a run of value
/// changes, what was touched, and the value it now has.
fn widget_line(event: &egui::output::OutputEvent) -> Option<(bool, String, String)> {
    use egui::output::OutputEvent as E;
    let (verb, info) = match event {
        E::Clicked(info) => ("click ", info),
        E::DoubleClicked(info) => ("double-click ", info),
        E::TripleClicked(info) => ("triple-click ", info),
        E::FocusGained(info) => ("focus ", info),
        E::ValueChanged(info) => ("", info),
        E::TextSelectionChanged(_) => return None,
    };
    let mut what = format!("{verb}{:?}", info.typ);
    if let Some(label) = info.label.as_deref().filter(|l| !l.is_empty()) {
        let _ = write!(what, " \"{}\"", readable(label));
    }
    let mut value = String::new();
    if let Some(v) = info.value {
        let _ = write!(value, "= {}", (v * 1000.0).round() / 1000.0);
    }
    if let Some(on) = info.selected {
        value.push_str(if on { "[on]" } else { "[off]" });
    }
    // The text itself stays out of the log; its length says enough.
    if let Some(text) = &info.current_text_value {
        let _ = write!(value, "({} chars)", text.chars().count());
    }
    Some((matches!(event, E::ValueChanged(_)), what, value))
}

/// A press this far (in points) from a piece of text is still told with it.
const NEAR_TEXT: f32 = 60.0;

/// The pointer went down over the UI at `at`: name the spot by the text drawn
/// there. It is the only name a control painted by hand has — egui reports
/// nothing for those (see [`widget_event`]).
pub fn pressed_text(shapes: &[egui::epaint::ClippedShape], at: egui::Pos2) {
    if JOURNAL.get().is_none() {
        return;
    }
    if let Some(line) = text_under(shapes, at) {
        note("ui", &line);
    }
}

/// The text at `at`, or failing that the nearest within [`NEAR_TEXT`]. Shapes
/// come in paint order, so of several at the same distance the last — the one
/// drawn on top — is the one the user saw.
fn text_under(shapes: &[egui::epaint::ClippedShape], at: egui::Pos2) -> Option<String> {
    fn visit<'a>(
        shape: &'a egui::Shape,
        clip: egui::Rect,
        at: egui::Pos2,
        best: &mut Option<(f32, &'a str)>,
    ) {
        match shape {
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    visit(shape, clip, at, best);
                }
            }
            egui::Shape::Text(text) => {
                let rect = text.visual_bounding_rect().intersect(clip);
                let words = text.galley.text().trim();
                if !rect.is_positive() || words.is_empty() {
                    return;
                }
                let distance = rect.distance_to_pos(at);
                if distance <= NEAR_TEXT && best.is_none_or(|(known, _)| distance <= known) {
                    *best = Some((distance, words));
                }
            }
            _ => {}
        }
    }
    let mut best = None;
    for clipped in shapes {
        visit(&clipped.shape, clipped.clip_rect, at, &mut best);
    }
    let (distance, words) = best?;
    let mut words = readable(words);
    if let Some((cut, _)) = words.char_indices().nth(48) {
        words.truncate(cut);
        words.push('\u{2026}');
    }
    Some(format!(
        "press {} \"{words}\"",
        if distance == 0.0 { "on" } else { "near" }
    ))
}

/// The user's "it just went wrong here" mark. Returns the time written.
pub fn mark() -> Option<String> {
    let mut journal = journal()?;
    let at = time_of(SystemTime::now());
    journal.line("MARK", "======== the user marked a problem here ========");
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

    /// Click at `at` in a small UI and return what the journal would be told.
    fn lines_for_a_click(
        at: egui::Pos2,
        mut ui: impl FnMut(&mut egui::Ui),
    ) -> Vec<(bool, String, String)> {
        let ctx = egui::Context::default();
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let mut lines = Vec::new();
        for events in [
            vec![],
            vec![egui::Event::PointerMoved(at)],
            vec![button(true)],
            vec![button(false)],
            vec![],
        ] {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 300.0),
                )),
                events,
                ..Default::default()
            };
            #[allow(deprecated)]
            let output = ctx.run_ui(input, |root| ui(root));
            lines.extend(output.platform_output.events.iter().filter_map(widget_line));
        }
        lines
    }

    #[test]
    fn a_clicked_button_is_journalled_by_its_label() {
        let mut rect = egui::Rect::NOTHING;
        // A first pass only to learn where the button lands.
        lines_for_a_click(egui::pos2(-10.0, -10.0), |ui| {
            rect = ui.button("Làm ảnh thẻ tự động").rect;
        });
        let lines = lines_for_a_click(rect.center(), |ui| {
            let _ = ui.button("Làm ảnh thẻ tự động");
        });
        assert_eq!(
            lines,
            vec![(
                false,
                "click Button \"Làm ảnh thẻ tự động\"".to_owned(),
                String::new()
            )]
        );
    }

    #[test]
    fn a_ticked_checkbox_is_journalled_with_its_new_state() {
        let mut rect = egui::Rect::NOTHING;
        let mut snap = false;
        lines_for_a_click(egui::pos2(-10.0, -10.0), |ui| {
            rect = ui.checkbox(&mut snap, "Snap").rect;
        });
        let lines = lines_for_a_click(rect.center(), |ui| {
            let _ = ui.checkbox(&mut snap, "Snap");
        });
        assert!(snap);
        assert_eq!(
            lines,
            vec![(
                false,
                "click Checkbox \"Snap\"".to_owned(),
                "[on]".to_owned()
            )]
        );
    }

    #[test]
    fn a_press_is_named_by_the_text_drawn_under_it() {
        let ctx = egui::Context::default();
        let mut blue = egui::Rect::NOTHING;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 300.0),
            )),
            ..Default::default()
        };
        #[allow(deprecated)]
        let output = ctx.run_ui(input, |ui| {
            ui.label("Nền trắng");
            ui.add_space(120.0);
            blue = ui.label("Nền xanh").rect;
        });
        assert_eq!(
            text_under(&output.shapes, blue.center()).as_deref(),
            Some("press on \"Nền xanh\"")
        );
        // Just off the text is still that text's control; far from any, nothing.
        assert_eq!(
            text_under(&output.shapes, blue.right_center() + egui::vec2(20.0, 0.0)).as_deref(),
            Some("press near \"Nền xanh\"")
        );
        assert_eq!(text_under(&output.shapes, egui::pos2(390.0, 290.0)), None);
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

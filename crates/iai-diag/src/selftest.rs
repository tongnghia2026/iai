//! Scripted trouble, to prove the recorder end to end without a user at the
//! mouse. `IAI_DIAG_SELFTEST=stall,par,lag,quit` runs those steps a few
//! seconds apart once the first frame is up (steps: `stall`, `par`, `lag`,
//! `pump`, `linger`, `panic`, `abort`, `quit`). Without the variable — always,
//! outside such a run — every entry point here returns at its first line.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const FIRST_STEP_AFTER: Duration = Duration::from_secs(3);
const BETWEEN_STEPS: Duration = Duration::from_secs(3);
const SPIN: Duration = Duration::from_millis(1200);
const LAG_FRAMES: u32 = 30;
const LAG_FRAME: Duration = Duration::from_millis(70);

struct Script {
    /// Steps still to run, last first.
    steps: Vec<String>,
    next_at: Option<Instant>,
    lag_frames: u32,
}

static ARMED: AtomicBool = AtomicBool::new(false);
static QUIT: AtomicBool = AtomicBool::new(false);
static LINGER: AtomicBool = AtomicBool::new(false);
static SCRIPT: Mutex<Option<Script>> = Mutex::new(None);

pub(super) fn arm() {
    let Ok(spec) = std::env::var("IAI_DIAG_SELFTEST") else {
        return;
    };
    let steps: Vec<String> = spec
        .split(',')
        .map(|step| step.trim().to_owned())
        .filter(|step| !step.is_empty())
        .rev()
        .collect();
    if steps.is_empty() {
        return;
    }
    super::note("test", &format!("self-test armed: {spec}"));
    *SCRIPT.lock().unwrap_or_else(|e| e.into_inner()) = Some(Script {
        steps,
        next_at: None,
        lag_frames: 0,
    });
    ARMED.store(true, Ordering::Release);
}

/// `None` unless a self-test is running; then whether it has asked the event
/// loop to end. While it is `Some`, the event loop keeps frames coming.
pub fn pending() -> Option<bool> {
    if !ARMED.load(Ordering::Acquire) {
        return None;
    }
    Some(QUIT.load(Ordering::Acquire))
}

/// Burn the UI thread for `d`, under a name the watcher's report can show.
#[inline(never)]
fn spin(d: Duration) {
    let since = Instant::now();
    let mut x = 1u64;
    while since.elapsed() < d {
        for _ in 0..4096 {
            x = std::hint::black_box(x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1));
        }
    }
}

/// Keep the UI thread inside its handler for `d` while still answering window
/// messages, the way a native dialog's own loop does.
#[cfg(windows)]
fn pump(d: Duration) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };
    let since = Instant::now();
    while since.elapsed() < d {
        unsafe {
            let mut message: MSG = std::mem::zeroed();
            while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(not(windows))]
fn pump(_: Duration) {}

/// With the `linger` step run: stay around after the event loop has returned,
/// as a teardown that hangs would.
pub(super) fn linger_at_exit() {
    if LINGER.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_secs(8));
    }
}

/// Called at the end of every main-window frame.
pub(super) fn step() {
    if !ARMED.load(Ordering::Acquire) {
        return;
    }
    let step = {
        let mut guard = SCRIPT.lock().unwrap_or_else(|e| e.into_inner());
        let Some(script) = guard.as_mut() else {
            return;
        };
        if script.lag_frames > 0 {
            script.lag_frames -= 1;
            drop(guard);
            spin(LAG_FRAME);
            return;
        }
        let now = Instant::now();
        if now < *script.next_at.get_or_insert(now + FIRST_STEP_AFTER) {
            return;
        }
        let Some(step) = script.steps.pop() else {
            ARMED.store(false, Ordering::Release);
            return;
        };
        if step == "lag" {
            script.lag_frames = LAG_FRAMES;
        }
        script.next_at = Some(now + SPIN + BETWEEN_STEPS);
        step
    };
    super::note("test", &format!("self-test step: {step}"));
    match step.as_str() {
        "stall" => spin(SPIN),
        "par" => {
            use rayon::prelude::*;
            (0..rayon::current_num_threads())
                .into_par_iter()
                .for_each(|_| spin(SPIN));
        }
        "pump" => pump(SPIN * 2),
        "linger" => LINGER.store(true, Ordering::Release),
        "panic" => panic!("flight recorder self-test"),
        "abort" => std::process::abort(),
        "quit" => QUIT.store(true, Ordering::Release),
        _ => {}
    }
}

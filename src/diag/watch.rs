//! The watcher half of the flight recorder: a second process that looks into
//! the app while its UI thread is stuck.
//!
//! Every tick it reads `Shared` out of the app. A handler still running after
//! [`STALL`] is a stall: the UI thread's stack is sampled until the handler
//! returns, the other threads' now and then, and what was seen is written to
//! `incidents.log` as call trees. A process that ends without the journal's
//! closing line is written up as a crash.
//!
//! A thread is held still only for as long as it takes to copy its registers
//! and stack; the walk runs afterwards, on the copy. dbghelp never runs while
//! a thread of the app is suspended, so nothing it does can leave one that way.

use super::{date_of, time_of, Phase, CLEAN_END, MAGIC};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_void, OsString};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{
    CloseHandle, LocalFree, FILETIME, HANDLE, HWND, INVALID_HANDLE_VALUE, LPARAM, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Diagnostics::Debug::{
    AddrModeFlat, GetThreadContext, ReadProcessMemory, StackWalk64, SymCleanup, SymFromAddr,
    SymFunctionTableAccess64, SymGetLineFromAddr64, SymGetModuleBase64, SymGetModuleInfoW64,
    SymInitializeW, SymRefreshModuleList, SymSetOptions, ADDRESS64, CONTEXT, CONTEXT_CONTROL_AMD64,
    CONTEXT_INTEGER_AMD64, IMAGEHLP_LINE64, IMAGEHLP_MODULEW64, STACKFRAME64, SYMBOL_INFO,
    SYMOPT_DEFERRED_LOADS, SYMOPT_FAIL_CRITICAL_ERRORS, SYMOPT_LOAD_LINES, SYMOPT_NO_PROMPTS,
    SYMOPT_UNDNAME,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows_sys::Win32::System::Memory::{VirtualQueryEx, MEMORY_BASIC_INFORMATION};
use windows_sys::Win32::System::ProcessStatus::{
    K32GetModuleBaseNameW, K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    PROCESS_MEMORY_COUNTERS_EX,
};
use windows_sys::Win32::System::SystemInformation::{
    GlobalMemoryStatusEx, IMAGE_FILE_MACHINE_AMD64, MEMORYSTATUSEX,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentThread, GetExitCodeProcess, GetProcessTimes, GetSystemTimes, GetThreadDescription,
    OpenProcess, OpenThread, ResumeThread, SetThreadPriority, SuspendThread, WaitForSingleObject,
    PROCESS_QUERY_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_VM_READ, THREAD_GET_CONTEXT,
    THREAD_PRIORITY_ABOVE_NORMAL, THREAD_QUERY_INFORMATION, THREAD_SUSPEND_RESUME,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumThreadWindows, SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_NULL,
};

/// A handler running this long is a stall worth sampling.
const STALL: Duration = Duration::from_millis(45);
const TICK_MS: u32 = 10;
/// A second without a stall closes the incident the stalls before it made.
const QUIET: Duration = Duration::from_secs(1);
/// A run of short stalls is cut into incidents no longer than this.
const INCIDENT_MAX: Duration = Duration::from_secs(20);
/// An incident is written up when its longest stall or their sum reaches these.
const WORTH_LONGEST_MS: u128 = 150;
const WORTH_TOTAL_MS: u128 = 400;
const PING_AFTER: Duration = Duration::from_millis(400);
const OTHERS_AFTER: Duration = Duration::from_millis(120);
/// The longest one pass over the other threads may keep the UI thread unsampled.
const OTHERS_BUDGET: Duration = Duration::from_millis(30);
const REPORT_CAP_BYTES: u64 = 32 * 1024 * 1024;
/// A stall this long gets a report while it is still going, in case it never ends.
const INTERIM_AFTER: Duration = Duration::from_secs(10);
/// The process is given this long to go once its event loop has returned.
const EXIT_GRACE: Duration = Duration::from_secs(5);
const MAX_FRAMES: usize = 128;
const UI_STACK_BYTES: usize = 512 * 1024;
const OTHER_STACK_BYTES: usize = 128 * 1024;
const MAX_UI_SAMPLES: usize = 3000;
const MAX_OTHER_SAMPLES: usize = 6000;
/// Call-tree branches under this share of the samples are left out.
const TREE_MIN_PERCENT: u32 = 3;
/// A run of pass-through library frames this long is one line of the tree.
const PLUMBING_RUN: usize = 3;
/// Names in the report are cut to this many characters.
const CLIP_CHARS: usize = 140;
const NAME_MAX: usize = 4096;
const THREAD_ACCESS: u32 = THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_QUERY_INFORMATION;

/// `super::Shared` as plain words: an atomic has the layout of its integer.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SharedCopy {
    magic: u64,
    busy_seq: u64,
    frames: u64,
    ui_thread: u32,
    phase: u32,
    exiting: u32,
    _pad: u32,
}

const _: () = assert!(std::mem::size_of::<SharedCopy>() == std::mem::size_of::<super::Shared>());

/// `GetThreadContext` refuses a CONTEXT that is not 16-byte aligned, and the
/// binding's own type does not ask for it.
#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct AlignedContext(CONTEXT);

/// The stack bytes the walk in progress reads instead of the live thread's.
struct StackCopy {
    base: u64,
    bytes: Vec<u8>,
}

thread_local! {
    static STACK: RefCell<StackCopy> = const {
        RefCell::new(StackCopy { base: 0, bytes: Vec::new() })
    };
}

/// dbghelp's window onto the app's memory during a walk: the copied stack
/// where it covers the read, the live process (code, unwind tables) otherwise.
unsafe extern "system" fn read_memory(
    process: HANDLE,
    address: u64,
    buffer: *mut c_void,
    size: u32,
    read: *mut u32,
) -> BOOL {
    let served = STACK.with(|stack| {
        let stack = stack.borrow();
        let Some(offset) = address.checked_sub(stack.base) else {
            return false;
        };
        let (offset, len) = (offset as usize, size as usize);
        if offset
            .checked_add(len)
            .is_none_or(|end| end > stack.bytes.len())
        {
            return false;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(stack.bytes.as_ptr().add(offset), buffer as *mut u8, len);
        }
        true
    });
    if served {
        if !read.is_null() {
            unsafe { *read = size };
        }
        return 1;
    }
    let mut got = 0usize;
    let ok = unsafe {
        ReadProcessMemory(
            process,
            address as *const c_void,
            buffer,
            size as usize,
            &mut got,
        )
    };
    if !read.is_null() {
        unsafe { *read = got as u32 };
    }
    ok
}

/// Hold `thread` still just long enough to copy its registers and the live
/// part of its stack (at most `max` bytes, into `stack`).
fn capture(
    process: HANDLE,
    thread: HANDLE,
    max: usize,
    stack: &mut Vec<u8>,
) -> Option<AlignedContext> {
    stack.clear();
    stack.reserve(max);
    unsafe {
        if SuspendThread(thread) == u32::MAX {
            return None;
        }
        let mut context: AlignedContext = std::mem::zeroed();
        context.0.ContextFlags = CONTEXT_CONTROL_AMD64 | CONTEXT_INTEGER_AMD64;
        // Also what makes sure the thread has really stopped: SuspendThread
        // only asks, GetThreadContext waits for it.
        let ok = GetThreadContext(thread, &mut context.0) != 0;
        if ok {
            let rsp = context.0.Rsp;
            let mut region: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
            let asked = VirtualQueryEx(
                process,
                rsp as *const c_void,
                &mut region,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            );
            if asked != 0 {
                let top = region.BaseAddress as u64 + region.RegionSize as u64;
                let len = (top.saturating_sub(rsp) as usize).min(max);
                stack.resize(len, 0);
                let mut got = 0usize;
                ReadProcessMemory(
                    process,
                    rsp as *const c_void,
                    stack.as_mut_ptr() as *mut c_void,
                    len,
                    &mut got,
                );
                stack.truncate(got);
            }
        }
        ResumeThread(thread);
        ok.then_some(context)
    }
}

/// Unwind a captured thread: program counters, innermost first.
fn walk(
    process: HANDLE,
    thread: HANDLE,
    mut context: AlignedContext,
    stack: &mut Vec<u8>,
) -> Vec<u64> {
    let flat = |offset: u64| ADDRESS64 {
        Offset: offset,
        Segment: 0,
        Mode: AddrModeFlat,
    };
    STACK.with(|copy| {
        let mut copy = copy.borrow_mut();
        copy.base = context.0.Rsp;
        std::mem::swap(&mut copy.bytes, stack);
    });
    let mut frame: STACKFRAME64 = unsafe { std::mem::zeroed() };
    frame.AddrPC = flat(context.0.Rip);
    frame.AddrFrame = flat(context.0.Rbp);
    frame.AddrStack = flat(context.0.Rsp);
    let mut pcs = Vec::with_capacity(48);
    while pcs.len() < MAX_FRAMES {
        let ok = unsafe {
            StackWalk64(
                IMAGE_FILE_MACHINE_AMD64 as u32,
                process,
                thread,
                &mut frame,
                &mut context.0 as *mut CONTEXT as *mut c_void,
                Some(read_memory),
                Some(SymFunctionTableAccess64),
                Some(SymGetModuleBase64),
                None,
            )
        };
        if ok == 0 || frame.AddrPC.Offset == 0 {
            break;
        }
        pcs.push(frame.AddrPC.Offset);
    }
    // Hand the buffer back so the next capture reuses its allocation.
    STACK.with(|copy| std::mem::swap(&mut copy.borrow_mut().bytes, stack));
    pcs
}

/// One resolved stack frame.
struct Frame {
    /// The function, or `module!?` where no symbol covers the address.
    func: Rc<str>,
    /// `file:line` where the PDB has line tables, `module+0xRVA` otherwise.
    place: Rc<str>,
    /// Whether it is this app's own code.
    app: bool,
}

/// The last three components: enough to tell `src\app\transform.rs` from a
/// dependency's file of the same name.
fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.split(['\\', '/']).filter(|p| !p.is_empty()).collect();
    parts[parts.len().saturating_sub(3)..].join("\\")
}

fn wide_text(buffer: &[u16]) -> String {
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

struct Symbols {
    process: HANDLE,
    exe: String,
    frames: HashMap<(u64, bool), Rc<Frame>>,
    modules: HashMap<u64, Rc<str>>,
}

impl Symbols {
    fn open(process: HANDLE) -> Result<Symbols, String> {
        unsafe {
            SymSetOptions(
                SYMOPT_DEFERRED_LOADS
                    | SYMOPT_LOAD_LINES
                    | SYMOPT_UNDNAME
                    | SYMOPT_FAIL_CRITICAL_ERRORS
                    | SYMOPT_NO_PROMPTS,
            );
        }
        // The exe names its PDB by file name alone, and dbghelp only looks in
        // the working directory unless told: point it at the exe's folder,
        // where cargo puts `iai.pdb` (this process is that same exe).
        let beside_exe: Vec<u16> = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.as_os_str().to_owned()))
            .map(|dir| {
                use std::os::windows::ffi::OsStrExt;
                dir.encode_wide().chain(std::iter::once(0)).collect()
            })
            .unwrap_or_else(|| vec![0]);
        // The app starts this process first thing; give its loader a moment.
        let mut ready = false;
        for _ in 0..30 {
            if unsafe { SymInitializeW(process, beside_exe.as_ptr(), 1) } != 0 {
                ready = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if !ready {
            return Err("dbghelp could not open the process".to_owned());
        }
        let mut name = [0u16; 260];
        unsafe { K32GetModuleBaseNameW(process, std::ptr::null_mut(), name.as_mut_ptr(), 260) };
        Ok(Symbols {
            process,
            exe: wide_text(&name),
            frames: HashMap::new(),
            modules: HashMap::new(),
        })
    }

    fn module_of(&mut self, address: u64) -> (Rc<str>, u64) {
        let base = unsafe { SymGetModuleBase64(self.process, address) };
        if base == 0 {
            return ("?".into(), 0);
        }
        let process = self.process;
        let name = self.modules.entry(base).or_insert_with(|| {
            let mut name = [0u16; 260];
            unsafe { K32GetModuleBaseNameW(process, base as *mut c_void, name.as_mut_ptr(), 260) };
            let name = wide_text(&name);
            if name.is_empty() {
                "?".into()
            } else {
                name.into()
            }
        });
        (name.clone(), base)
    }

    fn symbol_at(&self, address: u64) -> Option<String> {
        // SYMBOL_INFO ends in a one-byte name the caller is meant to extend.
        #[repr(C)]
        struct Buffer {
            info: SYMBOL_INFO,
            name: [u8; NAME_MAX],
        }
        let mut buffer: Buffer = unsafe { std::mem::zeroed() };
        buffer.info.SizeOfStruct = std::mem::size_of::<SYMBOL_INFO>() as u32;
        buffer.info.MaxNameLen = NAME_MAX as u32;
        let mut displacement = 0u64;
        let whole = &mut buffer as *mut Buffer;
        if unsafe {
            SymFromAddr(
                self.process,
                address,
                &mut displacement,
                whole as *mut SYMBOL_INFO,
            )
        } == 0
        {
            return None;
        }
        let len = (buffer.info.NameLen as usize).min(NAME_MAX);
        let raw = unsafe {
            let start = (whole as *const u8).add(std::mem::offset_of!(SYMBOL_INFO, Name));
            std::slice::from_raw_parts(start, len)
        };
        let name = String::from_utf8_lossy(raw);
        let mut name = match rustc_demangle::try_demangle(&name) {
            Ok(demangled) => format!("{demangled:#}"),
            Err(_) => name.into_owned(),
        };
        if name.len() > 200 {
            let cut = (0..=200)
                .rev()
                .find(|&i| name.is_char_boundary(i))
                .unwrap_or(0);
            name.truncate(cut);
            name.push('\u{2026}');
        }
        Some(name)
    }

    fn line_at(&self, address: u64) -> Option<String> {
        let mut line: IMAGEHLP_LINE64 = unsafe { std::mem::zeroed() };
        line.SizeOfStruct = std::mem::size_of::<IMAGEHLP_LINE64>() as u32;
        let mut displacement = 0u32;
        let found =
            unsafe { SymGetLineFromAddr64(self.process, address, &mut displacement, &mut line) };
        if found == 0 || line.FileName.is_null() {
            return None;
        }
        let file = unsafe { std::ffi::CStr::from_ptr(line.FileName as *const std::ffi::c_char) };
        Some(format!(
            "{}:{}",
            short_path(&file.to_string_lossy()),
            line.LineNumber
        ))
    }

    fn frame(&mut self, pc: u64, innermost: bool) -> Rc<Frame> {
        if let Some(known) = self.frames.get(&(pc, innermost)) {
            return known.clone();
        }
        // Above the innermost frame a pc is a return address: the instruction
        // after the call. Step back into the call itself.
        let address = if innermost { pc } else { pc.saturating_sub(1) };
        let (module, base) = self.module_of(address);
        let in_exe = module.eq_ignore_ascii_case(&self.exe);
        let func: Rc<str> = match self.symbol_at(address) {
            // Outside every module: generated code, or a walk gone astray.
            _ if base == 0 => "(not in any module)".into(),
            Some(name) if in_exe => name.into(),
            Some(name) => format!("{module}!{name}").into(),
            None => format!("{module}!?").into(),
        };
        let place: Rc<str> = match self.line_at(address) {
            Some(line) => line,
            None if base == 0 => format!("0x{address:X}"),
            None => format!("{module}+0x{:X}", address.wrapping_sub(base)),
        }
        .into();
        let frame = Rc::new(Frame {
            app: in_exe && is_app_function(&func),
            func,
            place,
        });
        self.frames.insert((pc, innermost), frame.clone());
        frame
    }

    fn resolve(&mut self, pcs: &[u64]) -> Vec<Rc<Frame>> {
        pcs.iter()
            .enumerate()
            .map(|(i, &pc)| self.frame(pc, i == 0))
            .collect()
    }

    /// How well the exe's own code can be named, for `watcher.log`.
    fn exe_symbols(&self, inside_exe: u64) -> String {
        let mut info: IMAGEHLP_MODULEW64 = unsafe { std::mem::zeroed() };
        info.SizeOfStruct = std::mem::size_of::<IMAGEHLP_MODULEW64>() as u32;
        if unsafe { SymGetModuleInfoW64(self.process, inside_exe, &mut info) } == 0 {
            return "unknown".to_owned();
        }
        let kind = match info.SymType {
            3 | 7 => "pdb",
            4 => "exports only (no pdb beside the exe: app frames will be bare offsets)",
            5 => "not loaded yet",
            0 => "none",
            _ => "other",
        };
        format!(
            "{kind}, line numbers {}, {}",
            if info.LineNumbers != 0 { "yes" } else { "no" },
            wide_text(&info.LoadedPdbName)
        )
    }
}

fn is_app_function(func: &str) -> bool {
    func.starts_with("iai::") || func.starts_with("<iai::")
}

/// A thread parked in the kernel waiting for something, going by its
/// innermost frame.
fn is_waiting(stack: &[Rc<Frame>]) -> bool {
    const MARKS: [&str; 8] = [
        "WaitFor",
        "DelayExecution",
        "RemoveIoCompletion",
        "MsgWait",
        "GetMessage",
        "WaitMessage",
        "SignalAndWait",
        "NtYieldExecution",
    ];
    stack
        .first()
        .is_none_or(|inner| MARKS.iter().any(|mark| inner.func.contains(mark)))
}

/// Samples merged into a call tree, outermost frame at the root.
#[derive(Default)]
struct Node {
    count: u32,
    places: HashMap<Rc<str>, u32>,
    kids: HashMap<Rc<str>, Node>,
}

impl Node {
    /// `stack` is innermost first, as walked.
    fn add(&mut self, stack: &[Rc<Frame>]) {
        self.count += 1;
        let mut node = self;
        for frame in stack.iter().rev() {
            node = node.kids.entry(frame.func.clone()).or_default();
            node.count += 1;
            *node.places.entry(frame.place.clone()).or_default() += 1;
        }
    }

    /// The children worth a line, heaviest first.
    fn heavy(&self, total: u32) -> Vec<(&Rc<str>, &Node)> {
        let mut kids: Vec<(&Rc<str>, &Node)> = self
            .kids
            .iter()
            .filter(|(_, kid)| kid.count * 100 >= total * TREE_MIN_PERCENT)
            .collect();
        kids.sort_by(|a, b| b.1.count.cmp(&a.1.count).then_with(|| a.0.cmp(b.0)));
        kids
    }

    fn print(&self, total: u32, depth: usize, out: &mut String) {
        for (mut func, mut kid) in self.heavy(total) {
            let mut depth = depth;
            // Library frames that neither branch nor hold samples of their own
            // (the event loop's plumbing, rayon's) say nothing one by one: a
            // run of them becomes a single line.
            let mut run: Vec<&str> = Vec::new();
            let (mut end_func, mut end) = (func, kid);
            while !is_app_function(end_func) {
                match end.heavy(total).as_slice() {
                    [(next_func, next)] if next.count == end.count => {
                        run.push(end_func);
                        end_func = next_func;
                        end = next;
                    }
                    _ => break,
                }
            }
            if run.len() >= PLUMBING_RUN {
                let _ = writeln!(
                    out,
                    "      {:indent$}\u{2026} {} library frames: {} \u{2026} {}",
                    "",
                    run.len(),
                    clip(run[0]),
                    clip(run[run.len() - 1]),
                    indent = depth.min(60),
                );
                depth += 1;
                func = end_func;
                kid = end;
            }
            let _ = writeln!(
                out,
                "{:>4}% {:indent$}{}  [{}]",
                kid.count * 100 / total,
                "",
                clip(func),
                top_place(&kid.places),
                indent = depth.min(60),
            );
            kid.print(total, depth + 1, out);
        }
    }
}

/// A name cut to what fits a line of the report.
fn clip(name: &str) -> &str {
    match name.char_indices().nth(CLIP_CHARS) {
        Some((at, _)) => &name[..at],
        None => name,
    }
}

/// The place a function was most often sampled at, and how many others.
fn top_place(places: &HashMap<Rc<str>, u32>) -> String {
    let top = places
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)));
    match (top, places.len()) {
        (None, _) => String::new(),
        (Some((place, _)), 1) => place.to_string(),
        (Some((place, _)), n) => format!("{place} +{} more", n - 1),
    }
}

fn ranked(counts: HashMap<String, u32>, keep: usize) -> Vec<(String, u32)> {
    let mut list: Vec<(String, u32)> = counts.into_iter().collect();
    list.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    list.truncate(keep);
    list
}

/// The three views of a set of samples: which app function was on the stack,
/// where the instruction pointer was, and the call tree joining the two.
fn describe(title: &str, stacks: &[Vec<Rc<Frame>>], out: &mut String) {
    let total = stacks.len() as u32;
    let _ = writeln!(out, "\n--- {title}: {total} samples ---");
    if total == 0 {
        return;
    }
    let mut app: HashMap<Rc<str>, (u32, HashMap<Rc<str>, u32>)> = HashMap::new();
    let mut spots = HashMap::new();
    let mut tree = Node::default();
    for stack in stacks {
        if let Some(frame) = stack.iter().find(|frame| frame.app) {
            let seen = app.entry(frame.func.clone()).or_default();
            seen.0 += 1;
            *seen.1.entry(frame.place.clone()).or_default() += 1;
        }
        if let Some(inner) = stack.first() {
            *spots
                .entry(format!("{}  [{}]", clip(&inner.func), inner.place))
                .or_insert(0) += 1;
        }
        tree.add(stack);
    }
    let mut app: Vec<(Rc<str>, (u32, HashMap<Rc<str>, u32>))> = app.into_iter().collect();
    app.sort_by(|a, b| (b.1).0.cmp(&(a.1).0).then_with(|| a.0.cmp(&b.0)));
    out.push_str("Innermost iAi function on the stack:\n");
    for (func, (count, places)) in app.iter().take(8) {
        let _ = writeln!(
            out,
            "{:>4}% {}  [{}]",
            count * 100 / total,
            clip(func),
            top_place(places)
        );
    }
    out.push_str("Where the thread actually was:\n");
    for (what, count) in ranked(spots, 10) {
        let _ = writeln!(out, "{:>4}% {what}", count * 100 / total);
    }
    let _ = writeln!(
        out,
        "Call tree (outermost first, branches under {TREE_MIN_PERCENT}% left out):"
    );
    tree.print(total, 0, out);
}

/// The latest stack of each thread, in the order the threads were first seen.
fn last_seen(samples: &[(u32, Vec<Rc<Frame>>)]) -> Vec<(u32, Vec<Rc<Frame>>)> {
    let mut latest: Vec<(u32, Vec<Rc<Frame>>)> = Vec::new();
    for (tid, stack) in samples {
        match latest.iter_mut().find(|(known, _)| known == tid) {
            Some(slot) => slot.1 = stack.clone(),
            None => latest.push((*tid, stack.clone())),
        }
    }
    latest
}

fn span_text(ms: u128) -> String {
    if ms >= 10_000 {
        format!("{} s", ms / 1000)
    } else if ms >= 1000 {
        format!("{:.1} s", ms as f64 / 1000.0)
    } else {
        format!("{ms} ms")
    }
}

fn incident_kind(longest_ms: u128) -> &'static str {
    match longest_ms {
        5000.. => "HANG",
        1000.. => "FREEZE",
        WORTH_LONGEST_MS.. => "STALL",
        _ => "LAG BURST",
    }
}

fn exit_meaning(code: u32) -> &'static str {
    match code {
        0 => "exit code 0, but the session was never closed",
        1 => "ended from outside (Task Manager, end task) or a fatal start-up error",
        101 => "Rust panic on the main thread",
        0xC000_0005 => "access violation",
        0xC000_00FD => "stack overflow",
        0xC000_0409 => "abort / fail-fast",
        0xC000_001D => "illegal instruction",
        0xC000_0374 => "heap corruption",
        0xC000_013A => "console window closed or Ctrl+C",
        0xC000_0142 => "a DLL failed to initialise",
        0x4001_0004 => "ended by a debugger",
        _ => "unrecognised exit code",
    }
}

struct StallRecord {
    at_ms: u128,
    phase: Phase,
    ms: u128,
    pumping: bool,
}

/// The stall in progress.
struct Stall {
    phase: Phase,
    /// `busy_seq` of the handler that is overrunning.
    seq: u64,
    started: Instant,
    /// The UI thread answers window messages: it sits in a native dialog's own
    /// loop, not in a computation.
    pumping: bool,
    last_ui: Option<Instant>,
    last_others: Option<Instant>,
    last_ping: Option<Instant>,
    interim_done: bool,
}

struct OtherThread {
    tid: u32,
    handle: HANDLE,
    name: String,
}

/// Stalls close enough together to be one event for the user.
struct Incident {
    no: u32,
    began: Instant,
    began_wall: SystemTime,
    cpu_at_start: Duration,
    /// The whole machine's (idle, total) CPU time when the incident began.
    system_at_start: (Duration, Duration),
    stalls: Vec<StallRecord>,
    ui: Vec<Vec<u64>>,
    /// The other threads' stacks, in the order taken.
    others: Vec<(u32, Vec<u64>)>,
    /// How many times the other threads were gone round, and where in
    /// `threads` the next round starts.
    rounds: u32,
    next_thread: usize,
    threads: Vec<OtherThread>,
    threads_listed: Option<Instant>,
}

struct Watcher {
    pid: u32,
    process: HANDLE,
    shared_at: usize,
    dir: PathBuf,
    ui_tid: u32,
    ui_thread: HANDLE,
    symbols: Symbols,
    stack: Vec<u8>,
    incidents: u32,
}

fn filetime(t: FILETIME) -> Duration {
    Duration::from_nanos((((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64) * 100)
}

fn thread_name(thread: HANDLE) -> String {
    let mut text: *mut u16 = std::ptr::null_mut();
    if unsafe { GetThreadDescription(thread, &mut text) } < 0 || text.is_null() {
        return String::new();
    }
    let mut len = 0;
    while unsafe { *text.add(len) } != 0 {
        len += 1;
    }
    let name = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
    unsafe { LocalFree(text as *mut c_void) };
    name
}

impl Watcher {
    fn log(&self, text: &str) {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("watcher.log"))
        {
            let _ = writeln!(file, "{} {text}", time_of(SystemTime::now()));
        }
    }

    fn read_shared(&self) -> Option<SharedCopy> {
        let mut copy = SharedCopy::default();
        let size = std::mem::size_of::<SharedCopy>();
        let mut got = 0usize;
        let ok = unsafe {
            ReadProcessMemory(
                self.process,
                self.shared_at as *const c_void,
                &mut copy as *mut SharedCopy as *mut c_void,
                size,
                &mut got,
            )
        };
        (ok != 0 && got == size && copy.magic == MAGIC).then_some(copy)
    }

    fn stack_of(&mut self, thread: HANDLE, max: usize) -> Option<Vec<u64>> {
        let context = capture(self.process, thread, max, &mut self.stack)?;
        Some(walk(self.process, thread, context, &mut self.stack))
    }

    fn cpu_time(&self) -> Duration {
        let mut times = [FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        }; 4];
        let [created, exited, kernel, user] = &mut times;
        if unsafe { GetProcessTimes(self.process, created, exited, kernel, user) } == 0 {
            return Duration::ZERO;
        }
        filetime(times[2]) + filetime(times[3])
    }

    /// The whole machine's CPU time so far, over all cores: (idle, total).
    fn system_times() -> (Duration, Duration) {
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut idle, mut kernel, mut user) = (zero, zero, zero);
        if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
            return (Duration::ZERO, Duration::ZERO);
        }
        // Kernel time counts the idle thread's as well.
        (filetime(idle), filetime(kernel) + filetime(user))
    }

    /// How busy the app and the machine were since `incident` began. A lag
    /// while something else (a build, a scan) holds the cores is not the
    /// app's doing, and the report has to let that show.
    fn cpu_text(&self, incident: &Incident) -> String {
        let wall = incident.began.elapsed().as_secs_f64().max(0.001);
        let app = self
            .cpu_time()
            .saturating_sub(incident.cpu_at_start)
            .as_secs_f64()
            / wall;
        let (idle, total) = Self::system_times();
        let idle = idle
            .saturating_sub(incident.system_at_start.0)
            .as_secs_f64();
        let total = total
            .saturating_sub(incident.system_at_start.1)
            .as_secs_f64();
        let cores = std::thread::available_parallelism().map_or(1.0, |n| n.get() as f64);
        let busy = ((total - idle).max(0.0) / wall).min(cores);
        let others = busy - app;
        let mut text =
            format!("app {app:.1} cores; whole machine {busy:.1} of {cores:.0} cores busy");
        // A couple of cores of background noise is any desktop; a quarter of
        // the machine held by something else is contention worth a flag.
        if others >= cores / 4.0 {
            let _ = write!(
                text,
                " \u{2014} OTHER PROGRAMS were using about {others:.0} cores (a build? a scan?)"
            );
        }
        text
    }

    fn memory_text(&self) -> String {
        const GB: f64 = 1024.0 * 1024.0 * 1024.0;
        let mut mine: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        let mut system: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        system.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        unsafe {
            K32GetProcessMemoryInfo(
                self.process,
                &mut mine as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
                size,
            );
            GlobalMemoryStatusEx(&mut system);
        }
        format!(
            "app {:.2} GB private ({:.2} GB in RAM, peak {:.2} GB); system {:.1} of {:.1} GB free",
            mine.PrivateUsage as f64 / GB,
            mine.WorkingSetSize as f64 / GB,
            mine.PeakPagefileUsage as f64 / GB,
            system.ullAvailPhys as f64 / GB,
            system.ullTotalPhys as f64 / GB,
        )
    }

    /// Whether the UI thread is answering window messages right now.
    fn pumping(&self) -> bool {
        unsafe extern "system" fn first(window: HWND, out: LPARAM) -> BOOL {
            unsafe { *(out as *mut HWND) = window };
            0
        }
        let mut window: HWND = std::ptr::null_mut();
        let mut answer = 0usize;
        unsafe {
            EnumThreadWindows(self.ui_tid, Some(first), &mut window as *mut HWND as LPARAM);
            !window.is_null()
                && SendMessageTimeoutW(window, WM_NULL, 0, 0, SMTO_ABORTIFHUNG, 50, &mut answer)
                    != 0
        }
    }

    fn list_threads(&self, known: &mut Vec<OtherThread>) {
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return;
            }
            let mut entry: THREADENTRY32 = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
            let mut more = Thread32First(snapshot, &mut entry);
            while more != 0 {
                let tid = entry.th32ThreadID;
                if entry.th32OwnerProcessID == self.pid
                    && tid != self.ui_tid
                    && !known.iter().any(|thread| thread.tid == tid)
                {
                    let handle = OpenThread(THREAD_ACCESS, 0, tid);
                    if !handle.is_null() {
                        known.push(OtherThread {
                            tid,
                            handle,
                            name: thread_name(handle),
                        });
                    }
                }
                more = Thread32Next(snapshot, &mut entry);
            }
            CloseHandle(snapshot);
        }
    }

    /// A stack from each of `threads`, starting at `*next` and going round,
    /// for as long as `budget` lasts. With every core busy a thread can take
    /// tens of milliseconds to stop; the budget keeps a round from starving
    /// the UI thread's own sampling, and `next` lets the following round take
    /// up where this one left off.
    fn sample_others(
        &mut self,
        threads: &[OtherThread],
        next: &mut usize,
        budget: Duration,
    ) -> Vec<(u32, Vec<u64>)> {
        let since = Instant::now();
        let mut round = Vec::with_capacity(threads.len());
        for _ in 0..threads.len() {
            let thread = &threads[*next % threads.len()];
            *next = (*next + 1) % threads.len();
            if let Some(pcs) = self.stack_of(thread.handle, OTHER_STACK_BYTES) {
                if !pcs.is_empty() {
                    round.push((thread.tid, pcs));
                }
            }
            if since.elapsed() >= budget {
                break;
            }
        }
        round
    }

    fn open_incident(&mut self, now: Instant) -> Incident {
        // A stall may be over in a fifth of a second: the UI thread's stack is
        // taken before anything that costs time, or the report has no sample.
        let first = self.stack_of(self.ui_thread, UI_STACK_BYTES);
        // DLLs loaded since the last look (AI runtimes come in late).
        unsafe { SymRefreshModuleList(self.process) };
        self.incidents += 1;
        Incident {
            no: self.incidents,
            began: now,
            began_wall: SystemTime::now(),
            cpu_at_start: self.cpu_time(),
            system_at_start: Self::system_times(),
            stalls: Vec::new(),
            ui: first.into_iter().collect(),
            others: Vec::new(),
            rounds: 0,
            next_thread: 0,
            // Listed when the other threads are first gone round.
            threads: Vec::new(),
            threads_listed: None,
        }
    }

    fn sample(&mut self, stall: &mut Stall, incident: &mut Incident, now: Instant) {
        let age = now.duration_since(stall.started);
        let ping_due = match stall.last_ping {
            None => age >= PING_AFTER,
            Some(at) => {
                let gap = if stall.pumping { 500 } else { 5000 };
                now.duration_since(at) >= Duration::from_millis(gap)
            }
        };
        if ping_due {
            let answered = self.pumping();
            // A handler that returned while the ping was out answers it too:
            // an answer only counts while that same handler is still running.
            let same_handler = self
                .read_shared()
                .is_some_and(|shared| shared.phase != 0 && shared.busy_seq == stall.seq);
            if !answered || same_handler {
                stall.pumping = answered;
            }
            stall.last_ping = Some(Instant::now());
        }

        // Every tick while the stall is young, thinning out as it ages.
        let ui_gap = if stall.pumping {
            Duration::from_secs(2)
        } else if age < Duration::from_secs(2) {
            Duration::ZERO
        } else if age < Duration::from_secs(20) {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(1)
        };
        if incident.ui.len() < MAX_UI_SAMPLES
            && stall
                .last_ui
                .is_none_or(|at| now.duration_since(at) >= ui_gap)
        {
            if let Some(pcs) = self.stack_of(self.ui_thread, UI_STACK_BYTES) {
                incident.ui.push(pcs);
            }
            stall.last_ui = Some(now);
        }

        let others_gap = if age < Duration::from_secs(3) {
            Duration::from_millis(150)
        } else if age < Duration::from_secs(20) {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(5)
        };
        if !stall.pumping
            && age >= OTHERS_AFTER
            && incident.others.len() < MAX_OTHER_SAMPLES
            && stall
                .last_others
                .is_none_or(|at| now.duration_since(at) >= others_gap)
        {
            if incident
                .threads_listed
                .is_none_or(|at| now.duration_since(at) >= Duration::from_secs(2))
            {
                self.list_threads(&mut incident.threads);
                incident.threads_listed = Some(now);
            }
            let round =
                self.sample_others(&incident.threads, &mut incident.next_thread, OTHERS_BUDGET);
            incident.others.extend(round);
            incident.rounds += 1;
            stall.last_others = Some(now);
        }

        if age >= INTERIM_AFTER && !stall.interim_done {
            stall.interim_done = true;
            self.write_incident(incident, Some((stall.phase, age.as_millis())), None);
        }
    }

    fn journal_tail(&self, lines: usize) -> String {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut file) = std::fs::File::open(self.dir.join("journal.log")) else {
            return String::new();
        };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        let _ = file.seek(SeekFrom::Start(len.saturating_sub(24 * 1024)));
        let mut bytes = Vec::new();
        let _ = file.read_to_end(&mut bytes);
        let text = String::from_utf8_lossy(&bytes);
        let all: Vec<&str> = text.lines().collect();
        all[all.len().saturating_sub(lines)..].join("\n")
    }

    fn append(&self, file: &str, text: &str) {
        let path = self.dir.join(file);
        // A session that lags for hours must not fill the disk; INDEX.log
        // still gets its line for each incident left out here.
        if std::fs::metadata(&path).is_ok_and(|meta| meta.len() >= REPORT_CAP_BYTES) {
            return;
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = file.write_all(text.as_bytes());
        }
    }

    /// One line per incident, all sessions together, beside the session folders.
    fn index(&self, at: SystemTime, kind: &str, span: &str, about: &str) {
        let session = self
            .dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let line = format!(
            "{} {}  {kind:<13} {span:<8} {about}  ->  {session}\\incidents.log\n",
            date_of(at),
            &time_of(at)[..8],
        );
        if let Some(root) = self.dir.parent() {
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(root.join("INDEX.log"))
            {
                let _ = file.write_all(line.as_bytes());
            }
        }
    }

    /// Write an incident up. `ongoing` is the stall still in progress (an
    /// interim report); `died` the exit code when the process ended inside it.
    fn write_incident(
        &mut self,
        incident: &Incident,
        ongoing: Option<(Phase, u128)>,
        died: Option<u32>,
    ) {
        let mut stalls: Vec<(u128, Phase, u128, bool)> = incident
            .stalls
            .iter()
            .map(|s| (s.at_ms, s.phase, s.ms, s.pumping))
            .collect();
        if let Some((phase, ms)) = ongoing {
            let at = incident.began.elapsed().as_millis().saturating_sub(ms);
            stalls.push((at, phase, ms, false));
        }
        let longest = stalls.iter().map(|s| s.2).max().unwrap_or(0);
        let total: u128 = stalls.iter().map(|s| s.2).sum();
        let all_pumping = !stalls.is_empty() && stalls.iter().all(|s| s.3);
        let kind = if all_pumping {
            "DIALOG WAIT"
        } else {
            incident_kind(longest)
        };
        let worst_phase = stalls
            .iter()
            .max_by_key(|s| s.2)
            .map(|s| s.1)
            .unwrap_or(Phase::Idle);

        let ui: Vec<Vec<Rc<Frame>>> = incident
            .ui
            .iter()
            .map(|pcs| self.symbols.resolve(pcs))
            .collect();
        let others: Vec<(u32, Vec<Rc<Frame>>)> = incident
            .others
            .iter()
            .map(|(tid, pcs)| (*tid, self.symbols.resolve(pcs)))
            .collect();

        let wall = incident.began.elapsed();
        let mut out = String::new();
        let _ = writeln!(out, "\n{}", "=".repeat(78));
        let _ = writeln!(
            out,
            "INCIDENT #{}  {kind}  {} {}{}",
            incident.no,
            date_of(incident.began_wall),
            time_of(incident.began_wall),
            match (ongoing, died) {
                (_, Some(_)) => "  (the process ended during it)",
                (Some(_), None) => "  (STILL FROZEN \u{2014} interim report)",
                _ => "",
            }
        );
        let _ = writeln!(
            out,
            "UI thread blocked {} time(s): longest {}, {} in all, within {}. Worst in: {}.",
            stalls.len(),
            span_text(longest),
            span_text(total),
            span_text(wall.as_millis()),
            worst_phase.label(),
        );
        if all_pumping {
            out.push_str(
                "The UI thread kept answering window messages: a native dialog was open. \
                 Not a freeze.\n",
            );
        }
        let _ = writeln!(
            out,
            "CPU: {}. Memory: {}.",
            self.cpu_text(incident),
            self.memory_text()
        );
        if stalls.len() > 1 {
            out.push_str("Stalls (ms into the incident: length, handler):\n");
            for (at, phase, ms, _) in stalls.iter().take(16) {
                let _ = writeln!(out, "  +{at}: {} ({})", span_text(*ms), phase.label());
            }
            if stalls.len() > 16 {
                let _ = writeln!(out, "  \u{2026} and {} more", stalls.len() - 16);
            }
        }
        out.push_str("\n--- What the user was doing (end of journal.log) ---\n");
        out.push_str(&self.journal_tail(40));
        out.push('\n');

        describe("UI thread", &ui, &mut out);

        let working: Vec<Vec<Rc<Frame>>> = others
            .iter()
            .filter(|(_, stack)| !is_waiting(stack))
            .map(|(_, stack)| stack.clone())
            .collect();
        if !others.is_empty() {
            let _ = writeln!(
                out,
                "\nOther threads: {} seen, gone round {} time(s); {} of {} samples were not waiting.",
                incident.threads.len(),
                incident.rounds,
                working.len(),
                others.len(),
            );
            if !working.is_empty() {
                describe("Other threads at work", &working, &mut out);
            }
        }
        // A long freeze may be a deadlock: show everyone, waiting or not.
        if (longest >= 2000 || died.is_some()) && !others.is_empty() {
            out.push_str("\n--- Every other thread, as last seen ---\n");
            self.print_distinct(&last_seen(&others), &incident.threads, &mut out);
        }
        self.append("incidents.log", &out);

        if ongoing.is_none() || died.is_some() {
            let about = ui
                .iter()
                .filter_map(|stack| stack.iter().find(|frame| frame.app))
                .fold(HashMap::<&str, u32>::new(), |mut counts, frame| {
                    *counts.entry(&*frame.func).or_insert(0) += 1;
                    counts
                })
                .into_iter()
                .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))
                .map(|(func, _)| func.to_owned())
                .unwrap_or_else(|| "(no iAi frame sampled)".to_owned());
            self.index(
                incident.began_wall,
                &format!("#{} {kind}", incident.no),
                &span_text(longest),
                &about,
            );
        }
    }

    /// Stacks grouped where several threads share one, innermost frame first.
    fn print_distinct(
        &self,
        round: &[(u32, Vec<Rc<Frame>>)],
        threads: &[OtherThread],
        out: &mut String,
    ) {
        let mut groups: Vec<(Vec<u32>, &Vec<Rc<Frame>>)> = Vec::new();
        for (tid, stack) in round {
            let same = groups.iter_mut().find(|(_, known)| {
                known.len() == stack.len() && known.iter().zip(stack).all(|(a, b)| Rc::ptr_eq(a, b))
            });
            match same {
                Some((tids, _)) => tids.push(*tid),
                None => groups.push((vec![*tid], stack)),
            }
        }
        groups.sort_by_key(|(tids, _)| std::cmp::Reverse(tids.len()));
        for (tids, stack) in groups {
            let names: Vec<String> = tids
                .iter()
                .map(
                    |tid| match threads.iter().find(|t| t.tid == *tid && !t.name.is_empty()) {
                        Some(thread) => format!("{tid} \"{}\"", thread.name),
                        None => tid.to_string(),
                    },
                )
                .collect();
            let _ = writeln!(out, "{} thread(s): {}", tids.len(), names.join(", "));
            for frame in stack.iter().take(48) {
                let _ = writeln!(out, "    {}  [{}]", frame.func, frame.place);
            }
        }
    }

    fn close_incident(&mut self, incident: Incident, died: Option<u32>) {
        let longest = incident.stalls.iter().map(|s| s.ms).max().unwrap_or(0);
        let total: u128 = incident.stalls.iter().map(|s| s.ms).sum();
        if longest >= WORTH_LONGEST_MS || total >= WORTH_TOTAL_MS || died.is_some() {
            self.write_incident(&incident, None, died);
        } else {
            self.incidents -= 1;
        }
        for thread in &incident.threads {
            unsafe { CloseHandle(thread.handle) };
        }
    }

    /// The event loop returned long ago and the process is still here.
    fn report_exit_hang(&mut self) {
        unsafe { SymRefreshModuleList(self.process) };
        let mut threads = vec![OtherThread {
            tid: self.ui_tid,
            handle: self.ui_thread,
            name: "main".to_owned(),
        }];
        self.list_threads(&mut threads);
        let round: Vec<(u32, Vec<Rc<Frame>>)> = self
            .sample_others(&threads, &mut 0, Duration::from_secs(5))
            .into_iter()
            .map(|(tid, pcs)| (tid, self.symbols.resolve(&pcs)))
            .collect();
        let now = SystemTime::now();
        let mut out = String::new();
        let _ = writeln!(out, "\n{}", "=".repeat(78));
        let _ = writeln!(
            out,
            "EXIT HANG  {} {}\nThe event loop returned {} s ago and the process has not ended.",
            date_of(now),
            time_of(now),
            EXIT_GRACE.as_secs()
        );
        out.push_str("\n--- Every thread ---\n");
        self.print_distinct(&round, &threads, &mut out);
        self.append("incidents.log", &out);
        self.index(now, "EXIT HANG", "", "process outlived its event loop");
        // The first entry borrows the UI thread's handle; the rest are ours.
        for thread in &threads[1..] {
            unsafe { CloseHandle(thread.handle) };
        }
    }

    fn process_ended(
        &mut self,
        stall: Option<Stall>,
        incident: Option<Incident>,
        last: SharedCopy,
    ) {
        let mut code = 0u32;
        unsafe { GetExitCodeProcess(self.process, &mut code) };
        let tail = self.journal_tail(60);
        let clean = code == 0 && tail.lines().rev().take(3).any(|l| l.contains(CLEAN_END));
        let during = stall.as_ref().map(|s| (s.phase, s.started.elapsed()));
        if let Some(mut incident) = incident {
            if let Some((phase, age)) = during {
                incident.stalls.push(StallRecord {
                    at_ms: incident.began.elapsed().saturating_sub(age).as_millis(),
                    phase,
                    ms: age.as_millis(),
                    pumping: false,
                });
            }
            self.close_incident(incident, (!clean).then_some(code));
        }
        if clean {
            self.log("the app closed normally");
            return;
        }
        let now = SystemTime::now();
        let mut out = String::new();
        let _ = writeln!(out, "\n{}", "=".repeat(78));
        let _ = writeln!(
            out,
            "ABNORMAL EXIT  {} {}\nExit code 0x{code:08X} ({code}): {}.",
            date_of(now),
            time_of(now),
            exit_meaning(code)
        );
        let _ = writeln!(
            out,
            "At the last look the UI thread was {}; {} frames drawn this session{}.",
            match during {
                Some((phase, age)) => format!(
                    "{} into a {} handler",
                    span_text(age.as_millis()),
                    phase.label()
                ),
                None => match Phase::from_code(last.phase) {
                    Phase::Idle => "between handlers".to_owned(),
                    phase => format!("in a {} handler", phase.label()),
                },
            },
            last.frames,
            if last.exiting != 0 {
                "; the event loop had already returned (it died during shutdown)"
            } else {
                ""
            },
        );
        out.push_str("\n--- The last things in journal.log ---\n");
        out.push_str(&tail);
        out.push('\n');
        self.append("incidents.log", &out);
        self.index(
            now,
            "ABNORMAL EXIT",
            "",
            &format!("0x{code:08X} {}", exit_meaning(code)),
        );
        self.log(&format!("the app ended abnormally, exit code 0x{code:08X}"));
    }

    /// Have dbghelp load the exe's symbols now, while the app is still
    /// starting, and not in the middle of the first stall; then walk the UI
    /// thread once to say in `watcher.log` how well stacks will read.
    fn warm_up(&mut self) {
        // Any address inside the exe makes dbghelp open its PDB.
        let _ = self.symbols.symbol_at(self.shared_at as u64);
        let frames = self
            .stack_of(self.ui_thread, UI_STACK_BYTES)
            .map(|pcs| self.symbols.resolve(&pcs))
            .unwrap_or_default();
        let named = frames.iter().filter(|f| !f.func.ends_with("!?")).count();
        self.log(&format!(
            "ready. Symbols for {}: {}. Trial walk of the UI thread: {} frames, {named} named, {} of them iAi's.",
            self.symbols.exe,
            self.symbols.exe_symbols(self.shared_at as u64),
            frames.len(),
            frames.iter().filter(|f| f.app).count(),
        ));
    }

    fn watch(&mut self) {
        self.warm_up();
        let started = Instant::now();
        // The handler being timed: its sequence number and when it was first seen.
        let mut seq = u64::MAX;
        let mut seq_seen = started;
        let mut stall: Option<Stall> = None;
        let mut incident: Option<Incident> = None;
        let mut last_stall_end = started;
        let mut exiting_since: Option<Instant> = None;
        let mut exit_hang_reported = false;
        let mut last = SharedCopy::default();

        while unsafe { WaitForSingleObject(self.process, TICK_MS) } != WAIT_OBJECT_0 {
            let Some(shared) = self.read_shared() else {
                continue;
            };
            last = shared;
            let now = Instant::now();

            if shared.phase != 0 && shared.busy_seq == seq {
                if now.duration_since(seq_seen) >= STALL {
                    let stall = stall.get_or_insert_with(|| Stall {
                        phase: Phase::from_code(shared.phase),
                        seq,
                        started: seq_seen,
                        pumping: false,
                        last_ui: None,
                        last_others: None,
                        last_ping: None,
                        interim_done: false,
                    });
                    if incident.is_none() {
                        incident = Some(self.open_incident(seq_seen));
                    }
                    if let Some(incident) = incident.as_mut() {
                        self.sample(stall, incident, now);
                    }
                }
            } else {
                if let (Some(ended), Some(incident)) = (stall.take(), incident.as_mut()) {
                    incident.stalls.push(StallRecord {
                        at_ms: ended.started.duration_since(incident.began).as_millis(),
                        phase: ended.phase,
                        ms: now.duration_since(ended.started).as_millis(),
                        pumping: ended.pumping,
                    });
                    last_stall_end = now;
                }
                seq = if shared.phase != 0 {
                    shared.busy_seq
                } else {
                    u64::MAX
                };
                seq_seen = now;
            }

            if stall.is_none()
                && incident.as_ref().is_some_and(|open| {
                    now.duration_since(last_stall_end) >= QUIET
                        || now.duration_since(open.began) >= INCIDENT_MAX
                })
            {
                if let Some(done) = incident.take() {
                    self.close_incident(done, None);
                }
            }

            if shared.exiting != 0 && !exit_hang_reported {
                let since = *exiting_since.get_or_insert(now);
                if now.duration_since(since) >= EXIT_GRACE {
                    exit_hang_reported = true;
                    self.report_exit_hang();
                }
            }
        }
        self.process_ended(stall, incident, last);
    }
}

/// Entry point of the watcher process. `args`: the app's pid, the address of
/// its `Shared` (hex), and the session folder.
pub(super) fn run(args: Vec<OsString>) {
    let [pid, shared_at, dir] = args.as_slice() else {
        return;
    };
    let Some(pid) = pid.to_str().and_then(|s| s.parse::<u32>().ok()) else {
        return;
    };
    let Some(shared_at) = shared_at
        .to_str()
        .and_then(|s| usize::from_str_radix(s, 16).ok())
    else {
        return;
    };
    let dir = PathBuf::from(dir);
    let log = dir.join("watcher.log");
    let fail = move |why: &str| {
        let _ = std::fs::write(&log, format!("watcher gave up: {why}\n"));
    };

    let process = unsafe {
        OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_VM_READ | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if process.is_null() {
        return fail("could not open the app's process");
    }
    // Sampling must not wait behind the very threads it is there to describe.
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL) };
    let symbols = match Symbols::open(process) {
        Ok(symbols) => symbols,
        Err(why) => return fail(&why),
    };
    let mut watcher = Watcher {
        pid,
        process,
        shared_at,
        dir,
        ui_tid: 0,
        ui_thread: std::ptr::null_mut(),
        symbols,
        stack: Vec::new(),
        incidents: 0,
    };
    let Some(shared) = watcher.read_shared() else {
        return fail("could not read the app's shared state");
    };
    watcher.ui_tid = shared.ui_thread;
    watcher.ui_thread = unsafe { OpenThread(THREAD_ACCESS, 0, shared.ui_thread) };
    if watcher.ui_thread.is_null() {
        return fail("could not open the app's UI thread");
    }
    watcher.watch();
    unsafe {
        SymCleanup(process);
        CloseHandle(watcher.ui_thread);
        CloseHandle(process);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(func: &str, place: &str) -> Rc<Frame> {
        Rc::new(Frame {
            func: func.into(),
            place: place.into(),
            app: is_app_function(func),
        })
    }

    #[test]
    fn the_call_tree_puts_the_heaviest_branch_first_and_drops_the_thin_ones() {
        let main = frame("main", "main.rs:3");
        let crop = frame("iai::app::crop", "src\\app\\crop.rs:10");
        let resample = frame("iai::core::resample", "src\\core\\resample.rs:412");
        let rare = frame("iai::core::rare", "src\\core\\rare.rs:1");
        let mut tree = Node::default();
        for _ in 0..98 {
            tree.add(&[resample.clone(), crop.clone(), main.clone()]);
        }
        tree.add(&[rare.clone(), crop.clone(), main.clone()]);
        tree.add(&[crop.clone(), main.clone()]);
        let mut out = String::new();
        tree.print(tree.count, 0, &mut out);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "{out}");
        assert!(lines[0].starts_with(" 100% main"));
        assert!(lines[1].starts_with(" 100%  iai::app::crop"));
        assert!(lines[2].starts_with("  98%   iai::core::resample  [src\\core\\resample.rs:412]"));
    }

    #[test]
    fn a_run_of_pass_through_library_frames_is_one_line_of_the_tree() {
        let stack = [
            frame("iai::core::resample::row", "src\\core\\resample.rs:9"),
            frame("iai::app::input::window_event", "src\\app\\input\\mod.rs:1"),
            frame("winit::c", "c.rs:3"),
            frame("winit::b", "b.rs:2"),
            frame("winit::a", "a.rs:1"),
            frame("iai::main", "src\\main.rs:11"),
        ];
        let mut tree = Node::default();
        for _ in 0..10 {
            tree.add(&stack);
        }
        let mut out = String::new();
        tree.print(tree.count, 0, &mut out);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 4, "{out}");
        assert!(lines[0].starts_with(" 100% iai::main"));
        assert!(lines[1].contains("\u{2026} 3 library frames: winit::a \u{2026} winit::c"));
        assert!(lines[2].contains("iai::app::input::window_event"));
        assert!(lines[3].contains("iai::core::resample::row"));
    }

    #[test]
    fn the_summary_names_the_innermost_app_function_not_the_library_under_it() {
        let stacks = vec![vec![
            frame("ntdll.dll!NtWaitForAlertByThreadId", "ntdll.dll+0x1"),
            frame("rayon_core::latch::LockLatch::wait", "iai.exe+0x2"),
            frame("iai::core::resample::resize", "src\\core\\resample.rs:40"),
            frame("iai::app::crop::commit", "src\\app\\crop.rs:12"),
        ]];
        let mut out = String::new();
        describe("UI thread", &stacks, &mut out);
        let app_line = out
            .lines()
            .skip_while(|l| !l.starts_with("Innermost iAi function"))
            .nth(1)
            .unwrap();
        assert!(app_line.contains("iai::core::resample::resize"), "{out}");
        assert!(!is_app_function("rayon_core::latch::LockLatch::wait"));
        assert!(is_app_function("<iai::app::state::App>::redraw"));
    }

    #[test]
    fn a_thread_parked_in_the_kernel_is_not_counted_as_working() {
        assert!(is_waiting(&[frame("ntdll.dll!ZwWaitForSingleObject", "")]));
        assert!(is_waiting(&[frame("ntdll.dll!NtDelayExecution", "")]));
        assert!(is_waiting(&[]));
        assert!(!is_waiting(&[frame("iai::core::resample::row", "")]));
    }

    #[test]
    fn each_thread_is_shown_as_it_was_last_seen() {
        let early = vec![frame("early", "")];
        let late = vec![frame("late", "")];
        let seen = last_seen(&[(1, early.clone()), (2, early), (1, late)]);
        assert_eq!(seen.len(), 2);
        assert_eq!((seen[0].0, &*seen[0].1[0].func), (1, "late"));
        assert_eq!((seen[1].0, &*seen[1].1[0].func), (2, "early"));
    }

    #[test]
    fn source_paths_keep_their_last_three_components() {
        assert_eq!(
            short_path("C:\\Users\\Admin\\Documents\\IAI\\src\\app\\transform.rs"),
            "src\\app\\transform.rs"
        );
        assert_eq!(
            short_path("/rustc/abc/library/core/src/ops/function.rs"),
            "src\\ops\\function.rs"
        );
        assert_eq!(short_path("lone.rs"), "lone.rs");
    }

    #[test]
    fn incidents_are_named_by_their_longest_stall() {
        assert_eq!(incident_kind(80), "LAG BURST");
        assert_eq!(incident_kind(600), "STALL");
        assert_eq!(incident_kind(1500), "FREEZE");
        assert_eq!(incident_kind(9000), "HANG");
        assert_eq!(span_text(640), "640 ms");
        assert_eq!(span_text(3200), "3.2 s");
        assert_eq!(exit_meaning(0xC000_0005), "access violation");
    }
}

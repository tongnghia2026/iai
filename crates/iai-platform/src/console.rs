//! iai.exe is a window program: started from the taskbar it brings no black
//! console along (closing that console killed the app unasked, H8). Run as
//! a command (`mcp`, `run`, `--build-info`) from a terminal, it borrows the
//! terminal's console so what it prints shows there.
// Win32 console calls.
#![allow(unsafe_code)]

use windows_sys::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE,
};

/// Print into the console of whoever started this process, unless output
/// already goes somewhere (a pipe or a file, as an agent's is).
pub fn attach_parent_console() {
    // SAFETY: plain queries and an attach with no pointers handed over.
    unsafe {
        let out = GetStdHandle(STD_OUTPUT_HANDLE);
        if out.is_null() || out == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

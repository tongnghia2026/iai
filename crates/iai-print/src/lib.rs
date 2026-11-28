//! Printing straight to a Windows printer through GDI (`print_gdi`), on the
//! Mac through CUPS (`cups`), and the printers this machine has, their paper
//! and settings (`printers`). Win32 calls, so `unsafe` where they are made;
//! elsewhere they say no printer.

#[cfg(target_os = "macos")]
pub mod cups;
pub mod print_gdi;
pub mod printers;

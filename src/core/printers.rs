//! The machine's printers: the list, the default one, the driver's own
//! settings dialog, and handing a finished PDF to a printer. The pages
//! themselves are laid out and encoded by `core::print`.

use std::path::Path;

use crate::core::print::PrinterInfo;

#[cfg(target_os = "windows")]
mod winspool {
    #[repr(C)]
    pub struct PrinterInfo4W {
        pub printer_name: *mut u16,
        pub server_name: *mut u16,
        pub attributes: u32,
    }

    #[link(name = "winspool")]
    extern "system" {
        pub fn EnumPrintersW(
            flags: u32,
            name: *const u16,
            level: u32,
            buf: *mut u8,
            cb: u32,
            needed: *mut u32,
            returned: *mut u32,
        ) -> i32;
        pub fn GetDefaultPrinterW(buffer: *mut u16, size: *mut u32) -> i32;
    }

    pub const PRINTER_ENUM_LOCAL: u32 = 2;
    pub const PRINTER_ENUM_CONNECTIONS: u32 = 4;
}

/// Installed printer names straight from the spooler (level 4 is a cheap
/// registry read — no driver round trip).
#[cfg(target_os = "windows")]
#[allow(unsafe_code)] // winspool: the printer list
fn enum_printer_names() -> Result<Vec<String>, String> {
    let flags = winspool::PRINTER_ENUM_LOCAL | winspool::PRINTER_ENUM_CONNECTIONS;
    let (mut needed, mut returned) = (0u32, 0u32);
    unsafe {
        winspool::EnumPrintersW(
            flags,
            std::ptr::null(),
            4,
            std::ptr::null_mut(),
            0,
            &mut needed,
            &mut returned,
        )
    };
    if needed == 0 {
        return Ok(Vec::new());
    }
    let mut buf = vec![0u8; needed as usize];
    let ok = unsafe {
        winspool::EnumPrintersW(
            flags,
            std::ptr::null(),
            4,
            buf.as_mut_ptr(),
            needed,
            &mut needed,
            &mut returned,
        )
    };
    if ok == 0 {
        return Err("Cannot list printers from Windows spooler".to_string());
    }
    let infos = buf.as_ptr() as *const winspool::PrinterInfo4W;
    let mut names = Vec::with_capacity(returned as usize);
    for i in 0..returned as usize {
        let p = unsafe { (*infos.add(i)).printer_name };
        if p.is_null() {
            continue;
        }
        let mut len = 0usize;
        while unsafe { *p.add(len) } != 0 {
            len += 1;
        }
        let name = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) });
        if !name.trim().is_empty() {
            names.push(name);
        }
    }
    Ok(names)
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)] // winspool: the default printer
fn default_printer_name() -> Option<String> {
    let mut size = 0u32;
    unsafe { winspool::GetDefaultPrinterW(std::ptr::null_mut(), &mut size) };
    if size == 0 {
        return None;
    }
    let mut buf = vec![0u16; size as usize];
    if unsafe { winspool::GetDefaultPrinterW(buf.as_mut_ptr(), &mut size) } == 0 {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// One printer's current paper/printable geometry, read from its driver DC —
/// the same numbers the GDI print path uses, so preview and print agree.
/// `is_default` is left `false`; callers that patch an existing list keep the
/// flag they already have.
#[cfg(target_os = "windows")]
pub fn query_printer(name: &str) -> Result<PrinterInfo, String> {
    query_printer_with_settings(name, None)
}

#[cfg(target_os = "windows")]
pub fn query_printer_with_settings(
    name: &str,
    settings: Option<&crate::core::print_gdi::PrinterSettings>,
) -> Result<PrinterInfo, String> {
    let metrics = crate::core::print_gdi::printer_page_metrics_with_settings(name, settings)?;
    Ok(PrinterInfo {
        name: name.to_string(),
        is_default: false,
        paper_points: Some(metrics.paper_points()),
        printable_rect_points: Some(metrics.printable_rect_points()),
    })
}

#[cfg(target_os = "windows")]
pub fn available_printers() -> Result<Vec<PrinterInfo>, String> {
    let names = enum_printer_names()?;
    let default = default_printer_name();
    let mut printers: Vec<PrinterInfo> = names
        .into_iter()
        .map(|name| {
            let is_default = default.as_deref() == Some(name.as_str());
            match query_printer(&name) {
                Ok(mut info) => {
                    info.is_default = is_default;
                    info
                }
                // Offline/broken driver: keep the printer selectable, fall
                // back to A4 for layout like before.
                Err(_) => PrinterInfo {
                    name,
                    is_default,
                    paper_points: None,
                    printable_rect_points: None,
                },
            }
        })
        .collect();
    printers.sort_by(|a, b| b.is_default.cmp(&a.is_default).then(a.name.cmp(&b.name)));
    Ok(printers)
}

#[cfg(not(target_os = "windows"))]
pub fn available_printers() -> Result<Vec<PrinterInfo>, String> {
    let output = std::process::Command::new("lpstat")
        .arg("-e")
        .output()
        .map_err(|e| format!("Cannot run lpstat: {e}"))?;
    if !output.status.success() {
        return Err("Cannot list CUPS printers".to_string());
    }

    let default = std::process::Command::new("lpstat")
        .arg("-d")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| text.split(':').nth(1).map(|name| name.trim().to_string()));

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| PrinterInfo {
            name: name.to_string(),
            is_default: default.as_deref() == Some(name),
            paper_points: None,
            printable_rect_points: None,
        })
        .collect())
}

#[cfg(target_os = "windows")]
pub fn open_printer_settings(
    printer: &str,
    current: Option<&crate::core::print_gdi::PrinterSettings>,
    owner_hwnd: isize,
) -> Result<Option<crate::core::print_gdi::PrinterSettings>, String> {
    crate::core::print_gdi::prompt_printer_settings(printer, current, owner_hwnd)
}

#[cfg(not(target_os = "windows"))]
pub fn open_printer_settings(
    _printer: &str,
    _current: Option<&crate::core::print_gdi::PrinterSettings>,
    _owner_hwnd: isize,
) -> Result<Option<crate::core::print_gdi::PrinterSettings>, String> {
    Err("Printer driver settings are only wired on Windows right now".to_string())
}

/// Send a (PDF) file to the system's default printer. Cross-platform.
#[cfg(target_os = "windows")]
#[allow(dead_code)]
pub fn send_to_printer(path: &Path) -> Result<(), String> {
    send_to_printer_with_options(path, None, 1)
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)] // ShellExecuteW "print"
pub fn send_to_printer_with_options(
    path: &Path,
    printer: Option<&str>,
    copies: u32,
) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            hwnd: *mut core::ffi::c_void,
            op: *const u16,
            file: *const u16,
            params: *const u16,
            dir: *const u16,
            show: i32,
        ) -> isize;
    }

    let file: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let selected_printer = printer.filter(|p| !p.trim().is_empty());
    let verb_text = if selected_printer.is_some() {
        "printto"
    } else {
        "print"
    };
    let verb: Vec<u16> = verb_text.encode_utf16().chain(std::iter::once(0)).collect();
    let params_buf = selected_printer.map(|printer| {
        format!("\"{printer}\"")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    });
    let params = params_buf
        .as_ref()
        .map_or(std::ptr::null(), |buf| buf.as_ptr());
    // SW_HIDE = 0. ShellExecute returns >32 on success.
    for _ in 0..copies.max(1) {
        let r = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                params,
                std::ptr::null(),
                0,
            )
        };
        if r <= 32 {
            return Err(format!("Print failed with code {r}"));
        }
    }
    Ok(())
}

/// Send a (PDF) file to the default printer via CUPS `lp` (Linux & macOS).
#[cfg(not(target_os = "windows"))]
pub fn send_to_printer(path: &Path) -> Result<(), String> {
    let status = std::process::Command::new("lp")
        .arg(path)
        .status()
        .map_err(|e| format!("Cannot run 'lp' (CUPS): {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("The 'lp' print command returned an error".into())
    }
}

#[cfg(not(target_os = "windows"))]
pub fn send_to_printer_with_options(
    path: &Path,
    printer: Option<&str>,
    copies: u32,
) -> Result<(), String> {
    let mut cmd = std::process::Command::new("lp");
    if let Some(printer) = printer.filter(|p| !p.trim().is_empty()) {
        cmd.args(["-d", printer]);
    }
    let copies = copies.max(1).to_string();
    let status = cmd
        .args(["-n", &copies])
        .arg(path)
        .status()
        .map_err(|e| format!("Cannot run lp (CUPS): {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("lp returned an error".into())
    }
}

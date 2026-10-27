//! Per-user Windows integration. No administrator rights are required.
// Explorer integration: Win32 registry and shell calls.
#![allow(unsafe_code)]

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegSetValueExW,
};
use windows_sys::Win32::UI::Shell::{
    CSIDL_DESKTOPDIRECTORY, SHCNE_ASSOCCHANGED, SHCNF_FLUSH, SHCNF_IDLIST, SHChangeNotify,
    SHGetFolderPathW,
};

const FILE_ICON: &[u8] = include_bytes!("../../../assets/iai.ico");

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

/// The user's Desktop folder, wherever Windows keeps it (OneDrive may have
/// taken it over).
pub fn desktop_dir() -> Option<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    // MAX_PATH characters, as the call requires.
    let mut path = [0u16; 260];
    // SAFETY: `path` holds MAX_PATH writable UTF-16 units, as the call needs.
    let found = unsafe {
        SHGetFolderPathW(
            std::ptr::null_mut(),
            CSIDL_DESKTOPDIRECTORY as i32,
            std::ptr::null_mut(),
            0,
            path.as_mut_ptr(),
        )
    } == 0;
    let len = path.iter().position(|&c| c == 0)?;
    (found && len > 0).then(|| std::ffi::OsString::from_wide(&path[..len]).into())
}

fn set_default_value(subkey: &str, value: &OsStr) -> bool {
    let subkey = wide(OsStr::new(subkey));
    let value = wide(value);
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: `subkey` is NUL-terminated UTF-16 that outlives the call; `key`
    // is a live HKEY slot the call writes the opened key into.
    let created = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        )
    } == 0;
    if !created {
        return false;
    }
    // SAFETY: `key` was opened just above; the data pointer and its length
    // in bytes describe `value`, which outlives the call.
    let written = unsafe {
        RegSetValueExW(
            key,
            std::ptr::null(),
            0,
            REG_SZ,
            value.as_ptr().cast(),
            (value.len() * std::mem::size_of::<u16>()) as u32,
        )
    } == 0;
    // SAFETY: `key` was opened above and is closed once, here.
    unsafe {
        RegCloseKey(key);
    }
    written
}

/// Register `.iai` and its icon under HKCU, then refresh Explorer's icon cache.
/// Failures are non-fatal: editing must still start on locked-down PCs.
pub fn register_iai_file_type() {
    let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") else {
        return;
    };
    let icon_dir = Path::new(&local_app_data).join("IAI");
    if std::fs::create_dir_all(&icon_dir).is_err() {
        return;
    }
    let icon_path = icon_dir.join("iai.ico");
    if std::fs::read(&icon_path).ok().as_deref() != Some(FILE_ICON)
        && std::fs::write(&icon_path, FILE_ICON).is_err()
    {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let open_command = format!("\"{}\" \"%1\"", exe.display());

    let changed = set_default_value("Software\\Classes\\.iai", OsStr::new("IAI.Project"))
        & set_default_value("Software\\Classes\\IAI.Project", OsStr::new("iAi Project"))
        & set_default_value(
            "Software\\Classes\\IAI.Project\\DefaultIcon",
            icon_path.as_os_str(),
        )
        & set_default_value(
            "Software\\Classes\\IAI.Project\\shell\\open\\command",
            OsStr::new(&open_command),
        );

    if changed {
        // SAFETY: SHCNE_ASSOCCHANGED takes no items: both pointers are null.
        unsafe {
            SHChangeNotify(
                SHCNE_ASSOCCHANGED as i32,
                SHCNF_IDLIST | SHCNF_FLUSH,
                std::ptr::null(),
                std::ptr::null(),
            );
        }
    }
}

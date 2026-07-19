//! Per-user Windows integration. No administrator rights are required.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_WRITE,
    REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows_sys::Win32::UI::Shell::{
    SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_FLUSH, SHCNF_IDLIST,
};

const FILE_ICON: &[u8] = include_bytes!("../assets/iai.ico");

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn set_default_value(subkey: &str, value: &OsStr) -> bool {
    let subkey = wide(OsStr::new(subkey));
    let value = wide(value);
    let mut key: HKEY = std::ptr::null_mut();
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

//! Write a file so that a failure part-way leaves what was there untouched.
//!
//! The new contents go to a temporary file beside the destination, are flushed
//! to disk, and only then renamed over it. A power cut, a full disk or an
//! encoder error therefore costs the new file, never the old one.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static NEXT_TEMP: AtomicU32 = AtomicU32::new(0);

/// A sibling of `dest` to write into first. It keeps `dest`'s extension because
/// some encoders pick the format from it.
fn temp_sibling(dest: &Path) -> PathBuf {
    let n = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let mut name = OsString::from(dest.file_stem().unwrap_or_default());
    name.push(format!(".~iai{:x}-{n:x}", std::process::id()));
    if let Some(ext) = dest.extension() {
        name.push(".");
        name.push(ext);
    }
    dest.with_file_name(name)
}

/// Windows refuses to open or rename a file that a virus scanner or the search
/// indexer is reading at that instant, which they do to every file just written.
/// That clears within milliseconds, so try again for a third of a second before
/// reporting it.
fn retry_while_busy<T>(mut op: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut wait_ms = 10;
    loop {
        match op() {
            Err(e) if cfg!(windows) && wait_ms <= 160 && held_by_another_program(&e) => {
                std::thread::sleep(std::time::Duration::from_millis(wait_ms));
                wait_ms *= 2;
            }
            other => return other,
        }
    }
}

/// ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION.
fn held_by_another_program(error: &std::io::Error) -> bool {
    cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32 | 33))
}

/// Rename `source` over `destination` in one step, replacing it if it exists.
#[cfg(target_os = "windows")]
fn move_over(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source_w: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination_w: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let ok = unsafe {
        MoveFileExW(
            source_w.as_ptr(),
            destination_w.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok != 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    // ERROR_PATH_NOT_FOUND / ERROR_FILENAME_EXCED_RANGE: the call above stops at
    // 260 characters, which the temporary name can cross; std's rename does not.
    if matches!(error.raw_os_error(), Some(3 | 206)) {
        return std::fs::rename(source, destination);
    }
    Err(error)
}

#[cfg(not(target_os = "windows"))]
fn move_over(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

/// Push `path`'s contents out of the OS cache, so the rename that follows cannot
/// leave a finished name over unfinished data after a power cut.
fn flush_to_disk(path: &Path) -> std::io::Result<()> {
    // The data is complete whether or not the file can be reopened, so only a
    // flush that was attempted and failed is an error.
    let Ok(file) = retry_while_busy(|| std::fs::OpenOptions::new().write(true).open(path)) else {
        return Ok(());
    };
    match file.sync_all() {
        // Some network and virtual drives have no flush at all.
        Err(e)
            if !matches!(
                e.kind(),
                std::io::ErrorKind::Unsupported | std::io::ErrorKind::InvalidInput
            ) && !(cfg!(windows) && e.raw_os_error() == Some(1)) =>
        {
            Err(e)
        }
        _ => Ok(()),
    }
}

fn replace_error(dest: &Path, error: &std::io::Error) -> String {
    let name = dest.file_name().unwrap_or_default().to_string_lossy();
    if held_by_another_program(error) {
        format!(
            "Không ghi đè được \"{name}\": file đang được chương trình khác mở, hoặc là file \
             chỉ đọc. File cũ còn nguyên. ({error})"
        )
    } else {
        format!("Không ghi được \"{name}\": {error}")
    }
}

/// Let `body` write the new contents to `temp`, then put `temp` in `dest`'s
/// place. If `body` fails, or the file cannot be put in place, `temp` is removed
/// and `dest` is exactly as it was. `temp` must be on the same volume as `dest`.
pub fn write_atomic_through(
    dest: &Path,
    temp: &Path,
    body: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let result = body(temp).and_then(|()| {
        flush_to_disk(temp).map_err(|e| replace_error(dest, &e))?;
        retry_while_busy(|| move_over(temp, dest)).map_err(|e| replace_error(dest, &e))
    });
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

/// [`write_atomic_through`] with a temporary file beside `dest` that keeps its
/// extension (`photo.jpg` is written as `photo.~iai1a2b-0.jpg`).
pub fn write_atomic(
    dest: &Path,
    body: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    write_atomic_through(dest, &temp_sibling(dest), body)
}

/// Replace `dest` with `bytes`, or leave it untouched.
pub fn write_bytes_atomic(dest: &Path, bytes: &[u8]) -> Result<(), String> {
    write_atomic(dest, |temp| {
        std::fs::write(temp, bytes).map_err(|e| replace_error(dest, &e))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder per test, removed when the test ends.
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "iai_atomic_{name}_{}_{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn success_replaces_the_file_and_leaves_no_temporary() {
        let folder = Folder::new("ok");
        let dest = folder.0.join("anh.jpg");
        std::fs::write(&dest, b"old").unwrap();

        write_bytes_atomic(&dest, b"new contents").unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), b"new contents");
        assert_eq!(folder.names(), ["anh.jpg"]);
    }

    #[test]
    fn a_new_file_is_created_when_none_was_there() {
        let folder = Folder::new("fresh");
        let dest = folder.0.join("moi.pdf");

        write_bytes_atomic(&dest, b"%PDF").unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), b"%PDF");
        assert_eq!(folder.names(), ["moi.pdf"]);
    }

    #[test]
    fn a_failing_body_leaves_the_old_file_byte_for_byte() {
        let folder = Folder::new("fail");
        let dest = folder.0.join("anh.png");
        std::fs::write(&dest, b"the customer's original").unwrap();

        let result = write_atomic(&dest, |temp| {
            // Half a file is on disk when the encoder gives up.
            std::fs::write(temp, b"half").unwrap();
            Err("encoder failed".to_string())
        });

        assert_eq!(result.unwrap_err(), "encoder failed");
        assert_eq!(std::fs::read(&dest).unwrap(), b"the customer's original");
        assert_eq!(folder.names(), ["anh.png"]);
    }

    #[test]
    fn a_body_that_writes_nothing_is_an_error_not_an_empty_file() {
        let folder = Folder::new("nothing");
        let dest = folder.0.join("anh.png");
        std::fs::write(&dest, b"original").unwrap();

        let result = write_atomic(&dest, |_| Ok(()));

        assert!(result.is_err());
        assert_eq!(std::fs::read(&dest).unwrap(), b"original");
        assert_eq!(folder.names(), ["anh.png"]);
    }

    #[test]
    fn the_temporary_keeps_the_extension_and_the_folder() {
        let dest = Path::new("thu muc").join("ảnh thẻ.JPG");
        let temp = temp_sibling(&dest);
        assert_eq!(temp.parent(), dest.parent());
        assert_eq!(temp.extension(), dest.extension());
        assert_ne!(temp, dest);
        assert!(temp
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("ảnh thẻ.~iai"));

        let bare = temp_sibling(Path::new("khong_duoi"));
        assert!(bare.to_string_lossy().starts_with("khong_duoi.~iai"));

        assert_ne!(temp_sibling(&dest), temp_sibling(&dest));
    }

    #[test]
    fn a_path_longer_than_260_characters_still_saves() {
        let folder = Folder::new("long");
        let mut dir = folder.0.clone();
        while dir.as_os_str().len() < 250 {
            dir.push("thu_muc_co_ten_rat_dai_de_vuot_gioi_han");
        }
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("anh.jpg");
        std::fs::write(&dest, b"old").unwrap();

        write_bytes_atomic(&dest, b"new").unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
    }

    #[cfg(windows)]
    #[test]
    fn a_file_another_program_holds_open_is_reported_and_kept() {
        use std::os::windows::fs::OpenOptionsExt;

        let folder = Folder::new("held");
        let dest = folder.0.join("anh.jpg");
        std::fs::write(&dest, b"original").unwrap();
        // No sharing at all, as a program that locks the file it has open.
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&dest)
            .unwrap();

        let error = write_bytes_atomic(&dest, b"new").unwrap_err();
        drop(held);

        assert!(error.contains("chương trình khác"), "{error}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"original");
        assert_eq!(folder.names(), ["anh.jpg"]);
    }
}

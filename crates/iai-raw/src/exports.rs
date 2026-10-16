//! The four C functions the app calls when this library is a file of its own
//! (`iai_raw.dll`). `iai_raw_api::ffi` describes them, and what a frame is.
// Functions exported to C, reading what C hands them.
#![allow(unsafe_code)]

use std::path::Path;

use iai_raw_api::ffi::{self, Frame};

/// # Safety
/// `len` readable bytes lie at `ptr`, or `len` is 0.
unsafe fn bytes<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        &[]
    } else {
        // SAFETY: the caller vouches for `len` readable bytes at `ptr`.
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}

#[no_mangle]
pub extern "C" fn iai_raw_abi() -> u32 {
    ffi::ABI
}

/// # Safety
/// `path_len` readable bytes lie at `path`.
#[no_mangle]
pub unsafe extern "C" fn iai_raw_decode_file(path: *const u8, path_len: usize) -> *const Frame {
    // SAFETY: the caller vouches for `path_len` bytes at `path`.
    let decoded = match std::str::from_utf8(unsafe { bytes(path, path_len) }) {
        Ok(path) => crate::decode_file(Path::new(path)),
        Err(_) => Err("the path of the file is not UTF-8".to_owned()),
    };
    ffi::lend(decoded)
}

/// # Safety
/// `frame` is a pointer [`iai_raw_decode_file`] returned, given back once.
#[no_mangle]
pub unsafe extern "C" fn iai_raw_free(frame: *const Frame) {
    // SAFETY: the caller hands back, once, a frame this library returned.
    unsafe { ffi::take_back(frame) }
}

/// # Safety
/// `file_len` readable bytes lie at `file`.
#[no_mangle]
pub unsafe extern "C" fn iai_raw_orientation(file: *const u8, file_len: usize) -> u32 {
    // SAFETY: the caller vouches for `file_len` bytes at `file`.
    u32::from(crate::orientation(unsafe { bytes(file, file_len) }).exif())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exported_functions_answer_as_the_interface_says() {
        assert_eq!(iai_raw_abi(), ffi::ABI);

        let path = "no such folder/không có.cr2";
        let decoded =
            unsafe { ffi::receive(iai_raw_decode_file(path.as_ptr(), path.len()), iai_raw_free) };
        assert_eq!(decoded.err(), crate::decode_file(Path::new(path)).err());

        let not_utf8 = [0xffu8, 0xfe];
        let decoded = unsafe {
            ffi::receive(
                iai_raw_decode_file(not_utf8.as_ptr(), not_utf8.len()),
                iai_raw_free,
            )
        };
        assert!(decoded.is_err_and(|why| why.contains("UTF-8")));

        let file = b"not a camera file";
        assert_eq!(unsafe { iai_raw_orientation(file.as_ptr(), file.len()) }, 0);
        assert_eq!(unsafe { iai_raw_orientation(std::ptr::null(), 0) }, 0);
    }
}

//! The library that decodes camera RAW files (`crates/iai-raw`: rawloader and
//! rawler, both LGPL-2.1), which the app has one of two ways. A build with
//! the `raw-dll` feature loads it at run time from a file beside the exe
//! (`iai_raw.dll`), which whoever has the app may replace with a build of
//! their own; a build without the feature links it in. The pictures are the
//! same either way.
// Loads iai_raw.dll and calls its C functions.
#![allow(unsafe_code)]

use std::path::Path;

pub use iai_raw_api::{Cfa, Decoder, Orientation, RawImage, RawImageData};

/// Decode the RAW file at `path`.
pub fn decode_file(path: &Path) -> Result<RawImage, String> {
    #[cfg(feature = "raw-dll")]
    return loaded::decode_file(path);
    #[cfg(not(feature = "raw-dll"))]
    iai_raw::decode_file(path)
}

/// The EXIF orientation of the RAW file whose bytes `file` is, read without
/// decoding the picture. Any failure is [`Orientation::Unknown`].
pub fn orientation(file: &[u8]) -> Orientation {
    #[cfg(feature = "raw-dll")]
    return loaded::orientation(file);
    #[cfg(not(feature = "raw-dll"))]
    iai_raw::orientation(file)
}

/// Where the library's file belongs when this build wants one and has none:
/// the update fetches it there (`crate::update`). `None` when the file is
/// there, or the library is linked in.
pub fn missing_file() -> Option<std::path::PathBuf> {
    #[cfg(feature = "raw-dll")]
    return loaded::find().err();
    #[cfg(not(feature = "raw-dll"))]
    None
}

#[cfg(feature = "raw-dll")]
mod loaded {
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;

    use iai_raw_api::ffi;
    use iai_raw_api::{Orientation, RawImage};

    struct Library {
        decode_file: ffi::DecodeFileFn,
        free: ffi::FreeFn,
        orientation: ffi::OrientationFn,
        /// Never unloaded: pictures hold samples the library lent them.
        _file: libloading::Library,
    }

    /// Where the library is looked for, in order: beside the exe, where an
    /// installed app has it and where a build of the user's own goes; then
    /// in `deps` beside the exe, where cargo leaves it for an app run from
    /// its build folder.
    fn places(exe: &Path) -> [PathBuf; 2] {
        let name = libloading::library_filename(ffi::LIBRARY);
        let dir = exe.parent().unwrap_or(Path::new("."));
        [dir.join(&name), dir.join("deps").join(&name)]
    }

    /// The library's file, or else where it belongs.
    pub fn find() -> Result<PathBuf, PathBuf> {
        // Never from the folder the app was started in: only from its own.
        let Ok(exe) = std::env::current_exe() else {
            return Err(libloading::library_filename(ffi::LIBRARY).into());
        };
        let [beside, built] = places(&exe);
        if beside.is_file() {
            Ok(beside)
        } else if built.is_file() {
            Ok(built)
        } else {
            Err(beside)
        }
    }

    fn name_of(path: &Path) -> String {
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }

    fn open(path: &Path) -> Result<Library, String> {
        let name = name_of(path);
        let unusable = |why: &dyn std::fmt::Display| {
            iai_diag::note("raw", &format!("{}: {why}", path.display()));
            format!("Không dùng được file {name} (phần đọc ảnh RAW) cạnh app: {why}")
        };
        // The file is one this app was published with, or one the user put
        // there in its place: loading it runs it, as starting the app does.
        unsafe {
            let file = libloading::Library::new(path).map_err(|e| unusable(&e))?;
            let abi = *file
                .get::<ffi::AbiFn>(ffi::ABI_SYMBOL)
                .map_err(|e| unusable(&e))?;
            let spoken = abi();
            if spoken != ffi::ABI {
                return Err(unusable(&format!(
                    "file thuộc bản khác của app (giao diện số {spoken}, app này cần số {})",
                    ffi::ABI
                )));
            }
            let decode_file = *file
                .get::<ffi::DecodeFileFn>(ffi::DECODE_FILE_SYMBOL)
                .map_err(|e| unusable(&e))?;
            let free = *file
                .get::<ffi::FreeFn>(ffi::FREE_SYMBOL)
                .map_err(|e| unusable(&e))?;
            let orientation = *file
                .get::<ffi::OrientationFn>(ffi::ORIENTATION_SYMBOL)
                .map_err(|e| unusable(&e))?;
            Ok(Library {
                decode_file,
                free,
                orientation,
                _file: file,
            })
        }
    }

    /// The library, loaded on first use. A failure is not remembered: the
    /// file may come later (the update fetches a missing one).
    fn library() -> Result<&'static Library, String> {
        static LIBRARY: OnceLock<Library> = OnceLock::new();
        if let Some(library) = LIBRARY.get() {
            return Ok(library);
        }
        let path = find().map_err(|wanted| {
            format!(
                "Thiếu file {} (phần đọc ảnh RAW) cạnh app. App tự tải lại file này khi có mạng; \
                 hoặc vào Help ▸ Kiểm tra cập nhật.",
                name_of(&wanted)
            )
        })?;
        let library = open(&path)?;
        iai_diag::note("raw", &format!("decoders loaded from {}", path.display()));
        Ok(LIBRARY.get_or_init(|| library))
    }

    pub fn decode_file(path: &Path) -> Result<RawImage, String> {
        let library = library()?;
        let path = path
            .to_str()
            .ok_or("Tên file có ký tự không đọc được: hãy đổi tên file rồi mở lại.")?;
        unsafe {
            ffi::receive(
                (library.decode_file)(path.as_ptr(), path.len()),
                library.free,
            )
        }
    }

    pub fn orientation(file: &[u8]) -> Orientation {
        let Ok(library) = library() else {
            return Orientation::Unknown;
        };
        let number = unsafe { (library.orientation)(file.as_ptr(), file.len()) };
        Orientation::from_exif(u16::try_from(number).unwrap_or(0))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use iai_raw_api::RawImageData;

        #[test]
        fn the_library_is_looked_for_beside_the_exe_then_in_cargos_folder() {
            let name = libloading::library_filename(ffi::LIBRARY);
            let exe = Path::new("somewhere").join("iai.exe");
            assert_eq!(
                places(&exe),
                [
                    Path::new("somewhere").join(&name),
                    Path::new("somewhere").join("deps").join(&name)
                ]
            );
        }

        /// The linked-in decoder is in this test binary only, to compare with.
        #[test]
        fn the_library_file_answers_as_the_linked_decoder_does() {
            let found = find().expect("cargo builds the library beside the tests");
            assert!(found.is_file());

            let path =
                std::env::temp_dir().join(format!("iai_không_phải_raw_{}.cr2", std::process::id()));
            std::fs::write(&path, b"not a camera file").unwrap();
            let (loaded, linked) = (decode_file(&path), iai_raw::decode_file(&path));
            let _ = std::fs::remove_file(&path);
            let (loaded, linked) = (loaded.err(), linked.err());
            assert!(loaded
                .as_deref()
                .is_some_and(|why| why.starts_with("rawloader: ")));
            assert_eq!(loaded, linked);

            assert_eq!(orientation(b"not a camera file"), Orientation::Unknown);
        }

        fn bits<const N: usize>(values: [f32; N]) -> [u32; N] {
            values.map(f32::to_bits)
        }

        /// Every fact and every sample, bit for bit.
        fn assert_same(loaded: &RawImage, linked: &RawImage, file: &str) {
            assert_eq!(loaded.decoder, linked.decoder, "{file}");
            assert_eq!(loaded.make, linked.make, "{file}");
            assert_eq!(loaded.model, linked.model, "{file}");
            assert_eq!(loaded.clean_make, linked.clean_make, "{file}");
            assert_eq!(loaded.clean_model, linked.clean_model, "{file}");
            assert_eq!(
                (loaded.width, loaded.height, loaded.cpp),
                (linked.width, linked.height, linked.cpp),
                "{file}"
            );
            assert_eq!(bits(loaded.wb_coeffs), bits(linked.wb_coeffs), "{file}");
            assert_eq!(bits(loaded.neutral_wb), bits(linked.neutral_wb), "{file}");
            assert_eq!(loaded.whitelevels, linked.whitelevels, "{file}");
            assert_eq!(loaded.blacklevels, linked.blacklevels, "{file}");
            assert_eq!(
                loaded.xyz_to_cam.map(bits),
                linked.xyz_to_cam.map(bits),
                "{file}"
            );
            assert_eq!(
                loaded.cam_to_xyz_normalized.map(bits),
                linked.cam_to_xyz_normalized.map(bits),
                "{file}"
            );
            assert_eq!(loaded.cfa.name, linked.cfa.name, "{file}");
            assert_eq!(loaded.cfa.colors(), linked.cfa.colors(), "{file}");
            assert_eq!(loaded.crops, linked.crops, "{file}");
            assert_eq!(loaded.blackareas, linked.blackareas, "{file}");
            assert_eq!(loaded.orientation, linked.orientation, "{file}");
            match (&loaded.data, &linked.data) {
                (RawImageData::Integer(a), RawImageData::Integer(b)) => {
                    assert!(a[..] == b[..], "{file}: samples differ")
                }
                (RawImageData::Float(a), RawImageData::Float(b)) => assert!(
                    a.len() == b.len()
                        && a.iter()
                            .zip(b.iter())
                            .all(|(a, b)| a.to_bits() == b.to_bits()),
                    "{file}: samples differ"
                ),
                _ => panic!("{file}: one decode is integer, the other float"),
            }
        }

        /// Hand-run over a folder of RAW files:
        /// `$env:IAI_RAW_CORPUS='C:\path\to\raws'`, then
        /// `cargo test --release --lib --features canvas-editor-webview probe_library_file -- --ignored --nocapture`
        #[test]
        #[ignore]
        fn probe_library_file_and_linked_decoder_agree_on_the_corpus() {
            let Some(dir) = std::env::var_os("IAI_RAW_CORPUS") else {
                return;
            };
            let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
                .expect("IAI_RAW_CORPUS is a folder")
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| crate::raw::is_raw_path(path))
                .collect();
            files.sort();
            assert!(!files.is_empty(), "no RAW file in IAI_RAW_CORPUS");
            let mut decoded = 0;
            for path in &files {
                let file = name_of(path);
                let bytes = std::fs::read(path).unwrap();
                assert_eq!(orientation(&bytes), iai_raw::orientation(&bytes), "{file}");
                match (decode_file(path), iai_raw::decode_file(path)) {
                    (Ok(loaded), Ok(linked)) => {
                        assert_same(&loaded, &linked, &file);
                        decoded += 1;
                        eprintln!(
                            "same  {:?} {}x{}  {file}",
                            loaded.decoder, loaded.width, loaded.height
                        );
                    }
                    (Err(loaded), Err(linked)) => {
                        assert_eq!(loaded, linked, "{file}");
                        eprintln!("same  (neither decodes it)  {file}");
                    }
                    _ => panic!("{file}: one decodes it, the other does not"),
                }
            }
            eprintln!(
                "{decoded} of {} files decoded, the same both ways",
                files.len()
            );
        }
    }
}

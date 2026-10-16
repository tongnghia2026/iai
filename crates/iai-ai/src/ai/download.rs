//! The last step of every model download: the file is checked against the
//! SHA-256 its source publishes before it is given the model's name, so a
//! download damaged on the way is refused instead of being run.

use std::io::Read;
use std::path::Path;

/// SHA-256 of the file at `path`, in lowercase hex. Reads the whole file —
/// hundreds of megabytes for the larger models — so never on the UI thread.
pub fn file_sha256_hex(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = hmac_sha256::Hash::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Move the finished download `part` to `dest` if it is the file `sha256`
/// names. Any other file is deleted and reported; `dest` is not touched.
pub fn install_verified(part: &Path, dest: &Path, sha256: &str) -> Result<(), String> {
    let actual = match file_sha256_hex(part) {
        Ok(actual) => actual,
        Err(e) => {
            let _ = std::fs::remove_file(part);
            return Err(format!("read back the download: {e}"));
        }
    };
    if actual != sha256 {
        let _ = std::fs::remove_file(part);
        return Err("File tải về không đúng — thử lại".to_string());
    }
    std::fs::rename(part, dest).map_err(|e| format!("rename: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SHA-256 of the three bytes `abc`, from FIPS 180-2.
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn folder(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "iai_model_download_{name}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_published_file_is_put_in_place() {
        let dir = folder("ok");
        let (part, dest) = (dir.join("model.onnx.part"), dir.join("model.onnx"));
        std::fs::write(&part, b"abc").unwrap();

        install_verified(&part, &dest, ABC).unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), b"abc");
        assert!(!part.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn any_other_file_is_deleted_and_the_model_in_place_is_kept() {
        let dir = folder("wrong");
        let (part, dest) = (dir.join("model.onnx.part"), dir.join("model.onnx"));
        std::fs::write(&dest, b"the model that works").unwrap();
        // Right length, one byte off: the old length check let this through.
        std::fs::write(&part, b"abd").unwrap();

        let error = install_verified(&part, &dest, ABC).unwrap_err();

        assert!(error.contains("không đúng"), "{error}");
        assert!(
            !part.exists(),
            "the bad download is not left to be retried over"
        );
        assert_eq!(std::fs::read(&dest).unwrap(), b"the model that works");
        let _ = std::fs::remove_dir_all(dir);
    }
}

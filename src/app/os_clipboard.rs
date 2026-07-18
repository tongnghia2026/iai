//! OS-clipboard image bridge (arboard). Keeps iai's internal layer clipboard and
//! the system clipboard in sync: an in-app copy also lands on the OS clipboard
//! (paste into other apps), and paste prefers whichever side is newer — "newer"
//! detected by comparing the OS image's content hash against the hash of what
//! iai itself last wrote.

use std::hash::{Hash, Hasher};

pub struct OsClipboardImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Content hash used to tell "the OS clipboard still holds what iai wrote" apart
/// from "another app replaced it". Sampled (head + tail + strided bytes) so a
/// 50MB image hashes in well under a millisecond — this is a freshness check,
/// not a security boundary.
pub fn image_hash(width: u32, height: u32, rgba: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (width, height, rgba.len()).hash(&mut h);
    let head = rgba.len().min(4096);
    rgba[..head].hash(&mut h);
    if rgba.len() > 4096 {
        rgba[rgba.len() - 4096..].hash(&mut h);
        let mut i = 4096;
        while i < rgba.len() {
            rgba[i].hash(&mut h);
            i += 4093; // prime stride so tile-aligned patterns can't dodge samples
        }
    }
    h.finish()
}

/// Read an image from the OS clipboard (None when it holds no image).
pub fn read_image() -> Result<Option<OsClipboardImage>, String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| format!("Clipboard unavailable: {e}"))?;
    let image = match clipboard.get_image() {
        Ok(image) => image,
        Err(arboard::Error::ContentNotAvailable) => return Ok(None),
        Err(e) => return Err(format!("Clipboard image unavailable: {e}")),
    };

    let width =
        u32::try_from(image.width).map_err(|_| "Clipboard image width is too large".to_string())?;
    let height = u32::try_from(image.height)
        .map_err(|_| "Clipboard image height is too large".to_string())?;
    if width == 0 || height == 0 {
        return Err("Clipboard image is empty".to_string());
    }
    if width > crate::core::canvas::MAX_DIMENSION || height > crate::core::canvas::MAX_DIMENSION {
        return Err(format!(
            "Clipboard image is too large: {}x{} px",
            width, height
        ));
    }

    let expected_len = crate::core::canvas::Canvas::checked_rgba_len(width, height)
        .ok_or_else(|| "Clipboard image is too large".to_string())?;
    let pixels = image.bytes.into_owned();
    if pixels.len() != expected_len {
        return Err("Clipboard image has an unexpected pixel buffer size".to_string());
    }

    Ok(Some(OsClipboardImage {
        width,
        height,
        pixels,
    }))
}

/// Write RGBA pixels to the OS clipboard. Returns the content hash of the
/// clipboard as READ BACK — Windows round-trips RGBA through DIB, which need not
/// be byte-identical to what was written, and paste compares against a hash of
/// clipboard reads, so the recorded hash must use the same representation.
pub fn write_image(width: u32, height: u32, rgba: &[u8]) -> Result<u64, String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| format!("Clipboard unavailable: {e}"))?;
    clipboard
        .set_image(arboard::ImageData {
            width: width as usize,
            height: height as usize,
            bytes: std::borrow::Cow::Borrowed(rgba),
        })
        .map_err(|e| format!("Clipboard write failed: {e}"))?;
    drop(clipboard);
    match read_image()? {
        Some(img) => Ok(image_hash(img.width, img.height, &img.pixels)),
        None => Ok(image_hash(width, height, rgba)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_changes_with_content_and_dims() {
        let a = vec![10u8; 64 * 64 * 4];
        let mut b = a.clone();
        b[3000] ^= 0xFF; // inside the head window
        assert_ne!(image_hash(64, 64, &a), image_hash(64, 64, &b));
        assert_ne!(image_hash(64, 64, &a), image_hash(32, 128, &a));
        assert_eq!(image_hash(64, 64, &a), image_hash(64, 64, &a.clone()));
    }

    #[test]
    fn hash_samples_interior_of_large_buffers() {
        // A change on a sampled stride index must flip the hash even deep inside
        // a large buffer.
        let a = vec![0u8; 1024 * 1024];
        let mut b = a.clone();
        b[4096 + 4093 * 100] = 1;
        assert_ne!(image_hash(512, 512, &a), image_hash(512, 512, &b));
    }
}

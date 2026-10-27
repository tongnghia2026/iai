//! OS-clipboard image bridge (arboard). Keeps iai's internal layer clipboard and
//! the system clipboard in sync: an in-app copy also lands on the OS clipboard
//! (paste into other apps), and paste prefers whichever side is newer — "newer"
//! detected by comparing the OS image's content hash against the hash of what
//! iai itself last wrote.
// The resolution of a clipboard image is read through Win32.
#![allow(unsafe_code)]

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

/// Read an image from the OS clipboard (None when it holds no image), no
/// side longer than `max_dimension` pixels.
pub fn read_image(max_dimension: u32) -> Result<Option<OsClipboardImage>, String> {
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
    if width > max_dimension || height > max_dimension {
        return Err(format!(
            "Clipboard image is too large: {}x{} px",
            width, height
        ));
    }

    let expected_len = (width as u64)
        .checked_mul(height as u64)
        .and_then(|n| n.checked_mul(4))
        .and_then(|n| usize::try_from(n).ok())
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

/// The resolution the OS clipboard's image records, in pixels per inch: the
/// pHYs chunk of its PNG, else the header of its DIB. `None` when it records
/// none, as an image copied in a browser or in Zalo mostly does.
pub fn read_image_ppi() -> Option<f32> {
    #[cfg(windows)]
    {
        win::image_ppi()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Pixels per inch from metres, rounded: 11811 px/m is 300 ppi.
fn per_metre(ppm: u32) -> Option<f32> {
    (ppm > 0).then(|| (ppm as f32 * 0.0254).round().max(1.0))
}

/// Pixels per inch in a PNG's pHYs chunk, when it is given in metres.
fn png_ppi(bytes: &[u8]) -> Option<f32> {
    let mut rest = bytes.strip_prefix(b"\x89PNG\r\n\x1a\n")?;
    while rest.len() >= 12 {
        let len = u32::from_be_bytes(rest[..4].try_into().ok()?) as usize;
        let body = rest.get(8..8 + len)?;
        match &rest[4..8] {
            b"pHYs" if len == 9 && body[8] == 1 => {
                return per_metre(u32::from_be_bytes(body[..4].try_into().ok()?));
            }
            // pHYs comes before the image data or not at all.
            b"IDAT" => return None,
            _ => {}
        }
        rest = rest.get(12 + len..)?;
    }
    None
}

/// Pixels per inch in a DIB's header (`biXPelsPerMeter`), when it is set.
fn dib_ppi(bytes: &[u8]) -> Option<f32> {
    let ppm = i32::from_le_bytes(bytes.get(24..28)?.try_into().ok()?);
    per_metre(u32::try_from(ppm).ok()?)
}

#[cfg(windows)]
mod win {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
        RegisterClipboardFormatW,
    };
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

    const CF_DIB: u32 = 8;
    const CF_DIBV5: u32 = 17;
    /// Past this a PNG's pHYs is not looked for.
    const PNG_HEAD: usize = 1 << 20;

    pub(super) fn image_ppi() -> Option<f32> {
        let _open = Open::new()?;
        let name: Vec<u16> = "PNG\0".encode_utf16().collect();
        // SAFETY: `name` is NUL-terminated UTF-16 that outlives the call.
        let png = unsafe { RegisterClipboardFormatW(name.as_ptr()) };
        let from_png = (png != 0)
            .then(|| head(png, PNG_HEAD))
            .flatten()
            .and_then(|bytes| super::png_ppi(&bytes));
        from_png.or_else(|| {
            [CF_DIBV5, CF_DIB]
                .into_iter()
                .find_map(|format| head(format, 40).and_then(|bytes| super::dib_ppi(&bytes)))
        })
    }

    /// The clipboard, open while this lives. Another program may hold it a
    /// moment.
    struct Open;

    impl Open {
        fn new() -> Option<Self> {
            for _ in 0..10 {
                // SAFETY: a null owner opens the clipboard for this task.
                if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                    return Some(Open);
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            None
        }
    }

    impl Drop for Open {
        fn drop(&mut self) {
            // SAFETY: this value exists only while the clipboard is open.
            unsafe { CloseClipboard() };
        }
    }

    /// The first `most` bytes of `format` on the open clipboard.
    fn head(format: u32, most: usize) -> Option<Vec<u8>> {
        // SAFETY: the clipboard is open (the caller holds an `Open`); the
        // handle is locked before it is read, no further than its own size,
        // and copied out before it is unlocked.
        unsafe {
            if IsClipboardFormatAvailable(format) == 0 {
                return None;
            }
            let handle = GetClipboardData(format);
            if handle.is_null() {
                return None;
            }
            let at = GlobalLock(handle) as *const u8;
            if at.is_null() {
                return None;
            }
            let bytes = std::slice::from_raw_parts(at, GlobalSize(handle).min(most)).to_vec();
            GlobalUnlock(handle);
            Some(bytes)
        }
    }
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
    // Read back no larger than what was written.
    match read_image(width.max(height))? {
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
    fn the_resolution_an_image_records_is_read_from_its_png_or_its_dib() {
        let chunk = |kind: &[u8], body: &[u8]| {
            let mut c = (body.len() as u32).to_be_bytes().to_vec();
            c.extend_from_slice(kind);
            c.extend_from_slice(body);
            c.extend_from_slice(&[0; 4]);
            c
        };
        let png = |chunks: &[Vec<u8>]| {
            let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
            bytes.extend(chunks.concat());
            bytes
        };
        let phys = |ppm: u32, unit: u8| {
            let mut body = ppm.to_be_bytes().to_vec();
            body.extend_from_slice(&ppm.to_be_bytes());
            body.push(unit);
            chunk(b"pHYs", &body)
        };
        let ihdr = chunk(b"IHDR", &[0; 13]);
        let idat = chunk(b"IDAT", &[0; 5]);
        assert_eq!(
            png_ppi(&png(&[ihdr.clone(), phys(11811, 1), idat.clone()])),
            Some(300.0)
        );
        assert_eq!(png_ppi(&png(&[ihdr.clone(), phys(23622, 1)])), Some(600.0));
        // No pHYs, one in no unit, or one after the image data: none.
        assert_eq!(png_ppi(&png(&[ihdr.clone(), idat.clone()])), None);
        assert_eq!(png_ppi(&png(&[ihdr.clone(), phys(11811, 0)])), None);
        assert_eq!(png_ppi(&png(&[ihdr, idat, phys(11811, 1)])), None);
        assert_eq!(png_ppi(b"not a png"), None);

        let dib = |ppm: i32| {
            let mut header = vec![0u8; 40];
            header[24..28].copy_from_slice(&ppm.to_le_bytes());
            header
        };
        assert_eq!(dib_ppi(&dib(3780)), Some(96.0));
        assert_eq!(dib_ppi(&dib(0)), None);
        assert_eq!(dib_ppi(&dib(-5)), None);
        assert_eq!(dib_ppi(&[0; 20]), None);
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

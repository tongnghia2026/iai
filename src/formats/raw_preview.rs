// Embedded-JPEG preview extraction from camera RAW files.
//
// rawloader decodes only the sensor mosaic and exposes no preview, yet every RAW
// container embeds one or more JPEG previews (CR2/NEF/ARW/DNG are TIFF-based;
// CR3/RAF/RW2 are not — a byte scan for JPEG streams covers them all). We pick
// the largest baseline/progressive JPEG and decode it to an upright RGBA8 image,
// far cheaper than a full demosaic. Used for the instant on-open preview (shown
// while the full decode runs) and, later, library thumbnails.
//
// Robustness notes:
//   • CR2 stores its raw sensor data as *lossless* JPEG (SOF3). Those streams get
//     scanned too, so we accept only baseline (SOF0) and progressive (SOF2) —
//     the marker kinds real previews use and `image` can decode.
//   • The EXIF orientation is read via rawloader's metadata-only `decode_dummy`
//     (no demosaic — a few tens of ms even for an 85 MB file) and applied so
//     portraits aren't sideways, matching raw.rs's full-decode orientation.

use std::path::Path;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, Debug)]
pub struct RawPreviewStats {
    pub mean_rgb: [f32; 3],
    pub mean_luma: f32,
}

fn preview_luma_cache(
) -> &'static Mutex<std::collections::HashMap<std::path::PathBuf, RawPreviewStats>> {
    static CACHE: OnceLock<Mutex<std::collections::HashMap<std::path::PathBuf, RawPreviewStats>>> =
        OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// A decoded, upright RGBA8 preview from a RAW's embedded JPEG.
pub struct RawPreview {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Formatted camera, lens and exposure metadata parsed from the RAW's TIFF
    /// tags for the Develop window; `None` for non-TIFF containers. Filled by
    /// `extract` (which holds the file bytes anyway), not `extract_from_bytes`.
    pub exif: Option<String>,
}

/// Largest sane preview dimension we trust from a scanned SOF header. Guards
/// against a mis-aligned byte sequence parsing as an absurd size and triggering a
/// huge decode allocation.
const MAX_PREVIEW_DIM: u32 = 20_000;

/// Read `path` and extract its embedded preview. Returns None when the file can't
/// be read or carries no usable preview.
pub fn extract(path: &Path) -> Option<RawPreview> {
    let data = std::fs::read(path).ok()?;
    let mut preview = extract_from_bytes(&data)?;
    preview.exif = crate::formats::raw_exif::exif_summary(&data);
    if let Some(stats) = preview_stats(&preview) {
        if let Ok(mut cache) = preview_luma_cache().lock() {
            cache.insert(path.to_path_buf(), stats);
        }
    }
    Some(preview)
}

/// Consume brightness already measured by the fast-preview worker. In the
/// normal open path this saves another full file read plus another JPEG decode
/// at the end of the expensive RAW demosaic.
pub fn take_cached_mean_luma(path: &Path) -> Option<f32> {
    preview_luma_cache()
        .lock()
        .ok()?
        .remove(path)
        .map(|s| s.mean_luma)
}

pub fn take_cached_stats(path: &Path) -> Option<RawPreviewStats> {
    preview_luma_cache().lock().ok()?.remove(path)
}

pub fn forget_cached_mean_luma(path: &Path) {
    if let Ok(mut cache) = preview_luma_cache().lock() {
        cache.remove(path);
    }
}

/// Extract the largest embedded baseline/progressive JPEG from RAW `data` and
/// decode it to upright RGBA8. Returns None if no candidate decodes.
pub fn extract_from_bytes(data: &[u8]) -> Option<RawPreview> {
    let mut cands = scan_jpeg_candidates(data);
    if cands.is_empty() {
        return None;
    }
    // Largest first; return the first that actually decodes (a mis-scan that
    // parsed a bogus SOF simply fails to decode and we fall through).
    cands.sort_unstable_by_key(|c| std::cmp::Reverse((c.w as u64) * (c.h as u64)));
    let orient = dummy_orientation(data);
    for c in cands {
        let Some((rgba, w, h)) = decode_jpeg_rgba8(&data[c.offset..]) else {
            continue;
        };
        let (rgba, w, h) = orient_rgba8(&rgba, w, h, orient);
        return Some(RawPreview {
            width: w as u32,
            height: h as u32,
            rgba,
            exif: None,
        });
    }
    None
}

/// Mean display luma (0..1) of the embedded preview — the camera JPEG's overall
/// brightness. Used to lift the RAW render's baseline exposure so opening a RAW
/// matches the preview instead of looking darker. None when no preview decodes.
pub fn preview_mean_luma_from_bytes(data: &[u8]) -> Option<f32> {
    let p = extract_from_bytes(data)?;
    preview_mean_luma(&p)
}

pub fn preview_stats_from_bytes(data: &[u8]) -> Option<RawPreviewStats> {
    preview_stats(&extract_from_bytes(data)?)
}

fn preview_mean_luma(p: &RawPreview) -> Option<f32> {
    preview_stats(p).map(|s| s.mean_luma)
}

fn preview_stats(p: &RawPreview) -> Option<RawPreviewStats> {
    if p.rgba.is_empty() {
        return None;
    }
    let n = (p.rgba.len() / 4) as f64;
    let sum = p.rgba.chunks_exact(4).fold([0.0f64; 3], |mut s, px| {
        for c in 0..3 {
            s[c] += px[c] as f64;
        }
        s
    });
    let mean_rgb = [
        (sum[0] / n / 255.0) as f32,
        (sum[1] / n / 255.0) as f32,
        (sum[2] / n / 255.0) as f32,
    ];
    Some(RawPreviewStats {
        mean_rgb,
        mean_luma: 0.2126 * mean_rgb[0] + 0.7152 * mean_rgb[1] + 0.0722 * mean_rgb[2],
    })
}

/// One scanned embedded-JPEG candidate: byte offset of its SOI and the frame
/// dimensions parsed from the SOF header.
struct Candidate {
    offset: usize,
    w: u32,
    h: u32,
}

/// Scan `data` for every baseline/progressive JPEG (SOI + SOF0/SOF2 with sane
/// dimensions). Lossless (SOF3) raw-sensor streams and out-of-range sizes are
/// skipped.
fn scan_jpeg_candidates(data: &[u8]) -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 3 < data.len() {
        if data[i] == 0xFF && data[i + 1] == 0xD8 && data[i + 2] == 0xFF {
            if let Some((w, h, marker)) = jpeg_frame(&data[i..]) {
                let baseline_or_progressive = marker == 0xC0 || marker == 0xC2;
                let sane = (1..=MAX_PREVIEW_DIM).contains(&w) && (1..=MAX_PREVIEW_DIM).contains(&h);
                if baseline_or_progressive && sane {
                    out.push(Candidate { offset: i, w, h });
                }
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    out
}

/// Parse (width, height, sof_marker) from the SOF header of the JPEG starting at
/// `buf[0]`, without decoding pixels. Returns None if no SOF precedes the scan.
fn jpeg_frame(buf: &[u8]) -> Option<(u32, u32, u8)> {
    if buf.len() < 4 || buf[0] != 0xFF || buf[1] != 0xD8 {
        return None;
    }
    let mut i = 2usize;
    while i + 4 <= buf.len() {
        if buf[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = buf[i + 1];
        // Standalone markers carry no length segment.
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        let len = ((buf[i + 2] as usize) << 8) | buf[i + 3] as usize;
        // SOFn frame headers (0xC0..=0xCF) hold dimensions, except DHT (C4),
        // JPG (C8) and DAC (CC) which are not frame headers.
        let is_sof =
            (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC;
        if is_sof {
            if i + 9 > buf.len() {
                return None;
            }
            let h = ((buf[i + 5] as u32) << 8) | buf[i + 6] as u32;
            let w = ((buf[i + 7] as u32) << 8) | buf[i + 8] as u32;
            return Some((w, h, marker));
        }
        if marker == 0xDA {
            return None; // reached the scan with no SOF
        }
        i += 2 + len;
    }
    None
}

/// Decode a JPEG stream to RGBA8, returning (pixels, width, height). The slice may
/// extend past the stream's EOI — the decoder stops at it. Guarded against decoder
/// panics so a malformed preview can't take down the worker thread.
fn decode_jpeg_rgba8(bytes: &[u8]) -> Option<(Vec<u8>, usize, usize)> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let img = image::load_from_memory(bytes).ok()?.into_rgba8();
        let (w, h) = (img.width() as usize, img.height() as usize);
        Some((img.into_raw(), w, h))
    }))
    .ok()
    .flatten()
}

/// EXIF orientation via rawloader's metadata-only pass (skips demosaic). Any
/// failure or panic yields `Unknown` (no rotation).
fn dummy_orientation(data: &[u8]) -> rawloader::Orientation {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rawloader::decode_dummy(&mut std::io::Cursor::new(data))
            .map(|r| r.orientation)
            .unwrap_or(rawloader::Orientation::Unknown)
    }))
    .unwrap_or(rawloader::Orientation::Unknown)
}

/// Remap an RGBA8 buffer to upright orientation per the EXIF tag: an optional
/// transpose followed by horizontal/vertical flips. Mirrors raw.rs::apply_orientation
/// (the RGBA16 full-decode version) so preview and commit agree.
fn orient_rgba8(
    src: &[u8],
    w: usize,
    h: usize,
    o: rawloader::Orientation,
) -> (Vec<u8>, usize, usize) {
    use rawloader::Orientation::*;
    let (swap, fx, fy) = match o {
        Normal | Unknown => (false, false, false),
        HorizontalFlip => (false, true, false),
        Rotate180 => (false, true, true),
        VerticalFlip => (false, false, true),
        Transpose => (true, false, false),
        Rotate90 => (true, true, false),
        Transverse => (true, true, true),
        Rotate270 => (true, false, true),
    };
    if !swap && !fx && !fy {
        return (src.to_vec(), w, h);
    }
    let (dw, dh) = if swap { (h, w) } else { (w, h) };
    let mut dst = vec![0u8; dw * dh * 4];
    for sy in 0..h {
        for sx in 0..w {
            let (mut dx, mut dy) = if swap { (sy, sx) } else { (sx, sy) };
            if fx {
                dx = dw - 1 - dx;
            }
            if fy {
                dy = dh - 1 - dy;
            }
            let s = (sy * w + sx) * 4;
            let d = (dy * dw + dx) * 4;
            dst[d..d + 4].copy_from_slice(&src[s..s + 4]);
        }
    }
    (dst, dw, dh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Encode a solid-colour RGBA image to a baseline JPEG byte stream.
    fn jpeg_bytes(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb(rgb));
        let mut buf = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Jpeg)
            .unwrap();
        buf
    }

    #[test]
    fn scans_and_picks_largest_baseline_jpeg() {
        // Fake container: junk, a small JPEG, more junk, a big JPEG, trailing junk.
        let small = jpeg_bytes(32, 24, [10, 20, 30]);
        let big = jpeg_bytes(128, 96, [200, 100, 50]);
        let mut data = vec![0u8; 64];
        data.extend_from_slice(&small);
        data.extend_from_slice(&[0xABu8; 128]);
        data.extend_from_slice(&big);
        data.extend_from_slice(&[0u8; 32]);

        let p = extract_from_bytes(&data).expect("should find a preview");
        // Largest wins; orientation Unknown (no RAW metadata) so dims are as-is.
        assert_eq!((p.width, p.height), (128, 96));
        assert_eq!(p.rgba.len(), 128 * 96 * 4);
    }

    #[test]
    fn skips_lossless_sof3_streams() {
        // A synthetic SOF3 (lossless) frame with a huge size must be ignored even
        // though it "looks" larger than a real baseline preview.
        let real = jpeg_bytes(80, 60, [0, 128, 255]);
        // SOI + APP0-ish filler + SOF3 header claiming 9000x9000, no valid scan.
        let mut fake_lossless = vec![0xFF, 0xD8, 0xFF];
        fake_lossless.extend_from_slice(&[0xC3, 0x00, 0x0B, 0x08]); // SOF3, len 11, precision 8
        fake_lossless.extend_from_slice(&9000u16.to_be_bytes()); // height
        fake_lossless.extend_from_slice(&9000u16.to_be_bytes()); // width
        fake_lossless.extend_from_slice(&[0x01, 0x00, 0x11, 0x00]); // 1 component

        let mut data = Vec::new();
        data.extend_from_slice(&fake_lossless);
        data.extend_from_slice(&[0u8; 16]);
        data.extend_from_slice(&real);

        let p = extract_from_bytes(&data).expect("should fall back to the baseline JPEG");
        assert_eq!((p.width, p.height), (80, 60));
    }

    #[test]
    fn none_when_no_jpeg() {
        assert!(extract_from_bytes(&[0u8; 512]).is_none());
    }

    #[test]
    fn orientation_transpose_swaps_dims() {
        // 2x1 red/blue row, Rotate90 -> transpose+flipx yields a 1x2 column.
        let src = vec![
            255, 0, 0, 255, // (0,0) red
            0, 0, 255, 255, // (1,0) blue
        ];
        let (dst, w, h) = orient_rgba8(&src, 2, 1, rawloader::Orientation::Rotate90);
        assert_eq!((w, h), (1, 2));
        assert_eq!(dst.len(), 2 * 4);
    }
}

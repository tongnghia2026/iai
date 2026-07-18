use super::{ExportOptions, Exporter, Importer};
use crate::core::canvas::Canvas;
use std::path::Path;

pub struct JpegImporter;
pub struct JpegExporter;

impl Importer for JpegImporter {
    fn extensions(&self) -> &[&str] {
        &["jpg", "jpeg", "jfif", "jpe"]
    }

    fn import(&self, path: &Path) -> Result<Canvas, String> {
        let (raw, w, h, profile, source) = super::import_color_managed(path)?;
        let dpi = read_jpeg_dpi(path).unwrap_or(72.0);
        let mut canvas = Canvas::from_rgba(raw, w, h);
        canvas.metadata.resolution_ppi = dpi;
        canvas.icc_profile = profile;
        canvas.metadata.source_profile = source;
        Ok(canvas)
    }
}

impl Exporter for JpegExporter {
    fn extensions(&self) -> &[&str] {
        &["jpg", "jpeg"]
    }

    fn export(&self, canvas: &Canvas, path: &Path, opts: &ExportOptions) -> Result<(), String> {
        let pixels = if opts.flatten {
            canvas.export_flat()
        } else {
            canvas.pixels.clone()
        };

        let w = canvas.width as usize;
        let h = canvas.height as usize;
        let mut rgb = vec![0u8; w * h * 3];
        for i in 0..w * h {
            let a = pixels[i * 4 + 3] as u16;
            let inv_a = 255 - a;
            rgb[i * 3] = ((pixels[i * 4] as u16 * a + 255 * inv_a) / 255) as u8;
            rgb[i * 3 + 1] = ((pixels[i * 4 + 1] as u16 * a + 255 * inv_a) / 255) as u8;
            rgb[i * 3 + 2] = ((pixels[i * 4 + 2] as u16 * a + 255 * inv_a) / 255) as u8;
        }

        let mut buf: Vec<u8> = Vec::new();
        {
            use image::codecs::jpeg::JpegEncoder;
            let mut enc = JpegEncoder::new_with_quality(&mut buf, 90);
            enc.encode(
                &rgb,
                canvas.width,
                canvas.height,
                image::ExtendedColorType::Rgb8,
            )
            .map_err(|e| e.to_string())?;
        }

        if opts.embed_metadata {
            let dpi = canvas.metadata.resolution_ppi;
            if dpi > 0.0 {
                patch_jfif_dpi(&mut buf, dpi as u16);
            }
        }

        if opts.embed_icc {
            let icc = super::export_icc_bytes(canvas);
            if !icc.is_empty() {
                inject_icc_app2(&mut buf, &icc);
            }
        }

        std::fs::write(path, &buf).map_err(|e| e.to_string())
    }
}

/// Embed an ICC profile as one or more `APP2` "ICC_PROFILE" segments, inserted
/// after the JFIF `APP0` header (or right after SOI if absent). Profiles larger
/// than one segment are split into the standard numbered chunks.
fn inject_icc_app2(buf: &mut Vec<u8>, icc: &[u8]) {
    if buf.len() < 2 || buf[0] != 0xFF || buf[1] != 0xD8 {
        return;
    }
    // Insertion point: just past the APP0 (JFIF) segment when present.
    let mut pos = 2usize;
    if buf.len() >= 6 && buf[2] == 0xFF && buf[3] == 0xE0 {
        let app0_len = ((buf[4] as usize) << 8) | (buf[5] as usize);
        pos = 4 + app0_len;
    }
    if pos > buf.len() {
        return;
    }

    const ID: &[u8] = b"ICC_PROFILE\0"; // 12 bytes
    const MAX_DATA: usize = 65535 - 2 - 12 - 2; // length(2)+id(12)+seq/total(2)
    let total_chunks = icc.len().div_ceil(MAX_DATA).max(1);
    if total_chunks > 255 {
        return; // ICC too large for the 1-byte chunk counter (won't happen in practice)
    }

    let mut segments: Vec<u8> = Vec::new();
    for (i, chunk) in icc.chunks(MAX_DATA).enumerate() {
        let payload_len = 12 + 2 + chunk.len(); // id + seq/total + data
        let seg_len = payload_len + 2; // + the 2 length bytes
        segments.push(0xFF);
        segments.push(0xE2);
        segments.push((seg_len >> 8) as u8);
        segments.push((seg_len & 0xFF) as u8);
        segments.extend_from_slice(ID);
        segments.push((i + 1) as u8);
        segments.push(total_chunks as u8);
        segments.extend_from_slice(chunk);
    }

    buf.splice(pos..pos, segments);
}

/// Patch JFIF APP0 header bytes to embed DPI.
/// JFIF structure: SOI(2) + APP0_marker(2) + len(2) + "JFIF\0"(5) + version(2) +
///   density_unit(1) + Xdensity(2) + Ydensity(2) + ...
/// Byte offsets: marker=2, len=4, identifier=6, version=11, unit=13, Xden=14, Yden=16
fn patch_jfif_dpi(buf: &mut Vec<u8>, dpi: u16) {
    if buf.len() < 18 {
        return;
    }
    if buf[0] != 0xFF || buf[1] != 0xD8 {
        return;
    }
    if buf[2] != 0xFF || buf[3] != 0xE0 {
        return;
    }
    if &buf[6..11] != b"JFIF\0" {
        return;
    }
    buf[13] = 1; // density unit: 1 = DPI
    buf[14] = (dpi >> 8) as u8;
    buf[15] = (dpi & 0xFF) as u8;
    buf[16] = (dpi >> 8) as u8;
    buf[17] = (dpi & 0xFF) as u8;
}

/// Read DPI from JFIF APP0 header.
#[allow(dead_code)]
fn read_jpeg_dpi_legacy(path: &Path) -> Option<f32> {
    let data = std::fs::read(path).ok()?;
    if data.len() < 18 {
        return None;
    }
    if data[0] != 0xFF || data[1] != 0xD8 {
        return None;
    }
    if data[2] != 0xFF || data[3] != 0xE0 {
        return None;
    }
    if &data[6..11] != b"JFIF\0" {
        return None;
    }
    let unit = data[13];
    let xden = ((data[14] as u16) << 8) | (data[15] as u16);
    if unit == 1 && xden > 0 {
        Some(xden as f32)
    } else if unit == 2 && xden > 0 {
        // dots per cm → DPI
        Some(xden as f32 * 2.54)
    } else {
        None
    }
}

fn read_jpeg_dpi(path: &Path) -> Option<f32> {
    let data = std::fs::read(path).ok()?;
    if data.len() < 18 || data[0] != 0xFF || data[1] != 0xD8 {
        return None;
    }

    let mut exif_dpi = None;
    let mut pos = 2usize;
    while pos + 4 <= data.len() {
        if data[pos] != 0xFF {
            pos += 1;
            continue;
        }
        while pos < data.len() && data[pos] == 0xFF {
            pos += 1;
        }
        if pos >= data.len() {
            break;
        }
        let marker = data[pos];
        pos += 1;
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            continue;
        }
        if pos + 2 > data.len() {
            break;
        }
        let seg_len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
        if seg_len < 2 || pos + seg_len > data.len() {
            break;
        }
        let seg = &data[pos + 2..pos + seg_len];
        match marker {
            0xE0 => {
                if let Some(dpi) = read_jfif_dpi(seg) {
                    return Some(dpi);
                }
            }
            0xE1 => {
                exif_dpi = exif_dpi.or_else(|| read_exif_dpi(seg));
            }
            _ => {}
        }
        pos += seg_len;
    }

    exif_dpi
}

fn read_jfif_dpi(seg: &[u8]) -> Option<f32> {
    if seg.len() < 12 || &seg[0..5] != b"JFIF\0" {
        return None;
    }
    let unit = seg[7];
    let xden = u16::from_be_bytes([seg[8], seg[9]]);
    if unit == 1 && xden > 0 {
        Some(xden as f32)
    } else if unit == 2 && xden > 0 {
        Some(xden as f32 * 2.54)
    } else {
        None
    }
}

fn read_exif_dpi(seg: &[u8]) -> Option<f32> {
    if seg.len() < 14 || &seg[0..6] != b"Exif\0\0" {
        return None;
    }
    let tiff = &seg[6..];
    let le = match tiff.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    if read_u16(tiff, 2, le)? != 42 {
        return None;
    }
    let ifd0 = read_u32(tiff, 4, le)? as usize;
    read_tiff_ifd_dpi(tiff, ifd0, le)
}

fn read_tiff_ifd_dpi(tiff: &[u8], offset: usize, le: bool) -> Option<f32> {
    let count = read_u16(tiff, offset, le)? as usize;
    let mut xres = None;
    let mut unit = 2u16;

    for i in 0..count {
        let entry = offset + 2 + i * 12;
        if entry + 12 > tiff.len() {
            return None;
        }
        let tag = read_u16(tiff, entry, le)?;
        let kind = read_u16(tiff, entry + 2, le)?;
        let n = read_u32(tiff, entry + 4, le)?;
        match tag {
            0x011A if kind == 5 && n >= 1 => {
                let ptr = read_u32(tiff, entry + 8, le)? as usize;
                let num = read_u32(tiff, ptr, le)? as f32;
                let den = read_u32(tiff, ptr + 4, le)? as f32;
                if den > 0.0 {
                    xres = Some(num / den);
                }
            }
            0x0128 if kind == 3 && n >= 1 => {
                unit = read_u16(tiff, entry + 8, le)?;
            }
            _ => {}
        }
    }

    let dpi = xres?;
    match unit {
        2 => Some(dpi),
        3 => Some(dpi * 2.54),
        _ => Some(dpi),
    }
}

fn read_u16(data: &[u8], offset: usize, le: bool) -> Option<u16> {
    let bytes: [u8; 2] = data.get(offset..offset + 2)?.try_into().ok()?;
    Some(if le {
        u16::from_le_bytes(bytes)
    } else {
        u16::from_be_bytes(bytes)
    })
}

fn read_u32(data: &[u8], offset: usize, le: bool) -> Option<u32> {
    let bytes: [u8; 4] = data.get(offset..offset + 4)?.try_into().ok()?;
    Some(if le {
        u32::from_le_bytes(bytes)
    } else {
        u32::from_be_bytes(bytes)
    })
}

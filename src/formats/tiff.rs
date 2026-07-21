use super::{ExportOptions, Exporter, Importer};
use crate::core::canvas::Canvas;
use std::path::Path;

pub struct TiffImporter;
pub struct TiffExporter;

/// Photoshop stores a layered TIFF's editable stack in this private tag
/// (ImageSourceData) as an `8BIM`/`Layr` block — the same layer data a PSD holds.
const TAG_IMAGE_SOURCE_DATA: u16 = 37724;

impl Importer for TiffImporter {
    fn extensions(&self) -> &[&str] {
        &["tiff", "tif"]
    }

    fn import(&self, path: &Path) -> Result<Canvas, String> {
        // The `image` crate's TIFF decoder drops the ICC tag under size limits, so
        // read it directly via the `tiff` crate and pass it as the override.
        // import_canvas_managed preserves 16-bit precision for 16-bit TIFFs.
        let icc = read_tiff_icc(path);

        // A TIFF saved from Photoshop with "Layers" carries the full editable
        // stack in the ImageSourceData tag. Rebuild it (reusing the PSD parser)
        // so the file no longer opens flattened; any trouble falls through to the
        // flat composite below, so nothing regresses.
        if let Some(mut canvas) = try_import_layered_tiff(path, icc.as_deref()) {
            canvas.metadata.resolution_ppi = read_tiff_dpi(path).unwrap_or(72.0);
            return Ok(canvas);
        }

        let (mut canvas, _source) = super::import_canvas_managed(path, icc)?;
        canvas.metadata.resolution_ppi = read_tiff_dpi(path).unwrap_or(72.0);
        Ok(canvas)
    }
}

/// Read the embedded ICC profile (tag 34675) from a TIFF, if present.
fn read_tiff_icc(path: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut dec = tiff::decoder::Decoder::new(std::io::BufReader::new(file)).ok()?;
    dec.get_tag_u8_vec(tiff::tags::Tag::IccProfile).ok()
}

/// Rebuild the editable layer stack from a Photoshop-layered TIFF. `None` when
/// the file has no ImageSourceData / no usable layer block (→ flat composite).
fn try_import_layered_tiff(path: &Path, icc: Option<&[u8]>) -> Option<Canvas> {
    let data = std::fs::read(path).ok()?;
    let isd = tiff_tag_bytes(&data, TAG_IMAGE_SOURCE_DATA)?;
    let (block, depth) = find_photoshop_layer_block(isd)?;

    let (w, h) = tiff_dimensions(path)?;
    let max = crate::core::canvas::MAX_DIMENSION;
    if w == 0 || h == 0 || w > max || h > max {
        return None;
    }
    // Same rule as the composite import path: keep 16-bit precision only for an
    // untagged/sRGB source; a tagged non-sRGB 16-bit file takes the flat path.
    let icc_is_srgb = match icc {
        None => true,
        Some(bytes) => crate::core::cms::profile_from_bytes(bytes)
            .map(|p| crate::core::cms::name_is_srgb(&crate::core::cms::profile_name(&p)))
            .unwrap_or(false),
    };
    super::psd::import_tiff_photoshop_layers(block, depth, w, h, icc, icc_is_srgb)
}

fn tiff_dimensions(path: &Path) -> Option<(u32, u32)> {
    let file = std::fs::File::open(path).ok()?;
    let mut dec = tiff::decoder::Decoder::new(std::io::BufReader::new(file)).ok()?;
    dec.dimensions().ok()
}

/// Locate the `Layr`/`Lr16`/`Lr32` layer block inside an ImageSourceData blob and
/// return its payload plus the depth it implies (8/16/32). The blob opens with a
/// signature string then `8BIM` blocks; we scan for the block signature directly,
/// which is robust to the exact signature/padding. A false positive in pixel data
/// is rejected later by the per-record `8BIM` check in the PSD parser.
fn find_photoshop_layer_block(isd: &[u8]) -> Option<(&[u8], u16)> {
    let mut i = 0usize;
    while i + 12 <= isd.len() {
        if &isd[i..i + 4] == b"8BIM" {
            let depth = match &isd[i + 4..i + 8] {
                b"Layr" => Some(8u16),
                b"Lr16" => Some(16u16),
                b"Lr32" => Some(32u16),
                _ => None,
            };
            if let Some(depth) = depth {
                let len =
                    u32::from_be_bytes([isd[i + 8], isd[i + 9], isd[i + 10], isd[i + 11]]) as usize;
                let start = i + 12;
                let end = start.checked_add(len)?;
                if end <= isd.len() {
                    return Some((&isd[start..end], depth));
                }
            }
        }
        i += 1;
    }
    None
}

/// Return the raw bytes of a classic-TIFF tag in IFD0 (following the value offset
/// for out-of-line data). Bounds-checked; `None` on any short/odd input.
fn tiff_tag_bytes(data: &[u8], want: u16) -> Option<&[u8]> {
    let le = match data.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    if read_u16(data, 2, le)? != 42 {
        return None; // classic TIFF only (BigTIFF layer data is not written by PS)
    }
    let ifd0 = read_u32(data, 4, le)? as usize;
    let count = (read_u16(data, ifd0, le)? as usize).min(4096);
    for idx in 0..count {
        let e = ifd0.checked_add(2 + idx * 12)?;
        if read_u16(data, e, le)? != want {
            continue;
        }
        let ty = read_u16(data, e + 2, le)?;
        let cnt = read_u32(data, e + 4, le)? as usize;
        let elem = match ty {
            1 | 2 | 6 | 7 => 1, // BYTE / ASCII / SBYTE / UNDEFINED
            3 | 8 => 2,         // SHORT / SSHORT
            4 | 9 | 11 => 4,    // LONG / SLONG / FLOAT
            5 | 10 | 12 => 8,   // RATIONAL / SRATIONAL / DOUBLE
            _ => 1,
        };
        let byte_len = cnt.checked_mul(elem)?;
        if byte_len <= 4 {
            return data.get(e + 8..e + 8 + byte_len);
        }
        let off = read_u32(data, e + 8, le)? as usize;
        return data.get(off..off.checked_add(byte_len)?);
    }
    None
}

fn read_u16(data: &[u8], at: usize, le: bool) -> Option<u16> {
    let b = data.get(at..at + 2)?;
    Some(if le {
        u16::from_le_bytes([b[0], b[1]])
    } else {
        u16::from_be_bytes([b[0], b[1]])
    })
}

fn read_u32(data: &[u8], at: usize, le: bool) -> Option<u32> {
    let b = data.get(at..at + 4)?;
    Some(if le {
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    } else {
        u32::from_be_bytes([b[0], b[1], b[2], b[3]])
    })
}

impl Exporter for TiffExporter {
    fn extensions(&self) -> &[&str] {
        &["tiff", "tif"]
    }

    fn export(&self, canvas: &Canvas, path: &Path, opts: &ExportOptions) -> Result<(), String> {
        let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
        let mut enc = tiff::encoder::TiffEncoder::new(file).map_err(|e| e.to_string())?;

        let dpi = if opts.embed_metadata {
            canvas.metadata.resolution_ppi
        } else {
            0.0
        };
        let icc = if opts.embed_icc {
            super::export_icc_bytes(canvas)
        } else {
            Vec::new()
        };
        let res = |unit, n| (unit, tiff::encoder::Rational { n, d: 1 });

        // 16-bit path for single-layer 16-bit documents.
        let hdr = if opts.flatten {
            canvas.export_flat16_samples()
        } else {
            None
        };
        if let Some(px16) = hdr {
            let mut img = enc
                .new_image::<tiff::encoder::colortype::RGBA16>(canvas.width, canvas.height)
                .map_err(|e| e.to_string())?;
            if dpi > 0.0 {
                let (u, r) = res(tiff::tags::ResolutionUnit::Inch, dpi as u32);
                img.resolution(u, r);
            }
            if !icc.is_empty() {
                img.encoder()
                    .write_tag(tiff::tags::Tag::IccProfile, &icc[..])
                    .map_err(|e| e.to_string())?;
            }
            return img.write_data(&px16[..]).map_err(|e| e.to_string());
        }

        let pixels = if opts.flatten {
            canvas.export_flat()
        } else {
            canvas.pixels.clone()
        };
        let mut img = enc
            .new_image::<tiff::encoder::colortype::RGBA8>(canvas.width, canvas.height)
            .map_err(|e| e.to_string())?;
        if dpi > 0.0 {
            let (u, r) = res(tiff::tags::ResolutionUnit::Inch, dpi as u32);
            img.resolution(u, r);
        }
        if !icc.is_empty() {
            // ICCProfile tag (34675). Must be written before the image data.
            img.encoder()
                .write_tag(tiff::tags::Tag::IccProfile, &icc[..])
                .map_err(|e| e.to_string())?;
        }
        img.write_data(&pixels[..]).map_err(|e| e.to_string())
    }
}

/// Read DPI from TIFF XResolution + ResolutionUnit tags.
fn read_tiff_dpi(path: &Path) -> Option<f32> {
    let file = std::fs::File::open(path).ok()?;
    let mut dec = tiff::decoder::Decoder::new(std::io::BufReader::new(file)).ok()?;
    let xres = dec.find_tag(tiff::tags::Tag::XResolution).ok()??;
    let unit = dec
        .find_tag(tiff::tags::Tag::ResolutionUnit)
        .ok()
        .flatten()
        .and_then(|v| v.into_u16().ok())
        .unwrap_or(2);
    let dpi = match xres {
        tiff::decoder::ifd::Value::Rational(n, d) if d > 0 => n as f32 / d as f32,
        _ => return None,
    };
    match unit {
        2 => Some(dpi),
        3 => Some(dpi * 2.54),
        _ => Some(dpi),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_layer_block_after_signature() {
        let mut isd = Vec::new();
        isd.extend_from_slice(b"Adobe Photoshop Document Data Block\0");
        isd.extend_from_slice(b"8BIM");
        isd.extend_from_slice(b"Layr");
        isd.extend_from_slice(&4u32.to_be_bytes());
        isd.extend_from_slice(&[1, 2, 3, 4]);
        let (block, depth) = find_photoshop_layer_block(&isd).expect("layer block");
        assert_eq!(block, &[1, 2, 3, 4]);
        assert_eq!(depth, 8);
    }

    #[test]
    fn finds_16bit_block_and_ignores_other_keys() {
        let mut isd = Vec::new();
        // An unrelated block first, then the Lr16 layer block.
        isd.extend_from_slice(b"8BIM");
        isd.extend_from_slice(b"Patt");
        isd.extend_from_slice(&2u32.to_be_bytes());
        isd.extend_from_slice(&[9, 9]);
        isd.extend_from_slice(b"8BIM");
        isd.extend_from_slice(b"Lr16");
        isd.extend_from_slice(&3u32.to_be_bytes());
        isd.extend_from_slice(&[7, 7, 7]);
        let (block, depth) = find_photoshop_layer_block(&isd).expect("layer block");
        assert_eq!(block, &[7, 7, 7]);
        assert_eq!(depth, 16);
    }

    #[test]
    fn no_layer_block_returns_none() {
        assert!(find_photoshop_layer_block(b"no photoshop blocks here").is_none());
        // A truncated block (declared length past the end) must not panic.
        let mut isd = Vec::new();
        isd.extend_from_slice(b"8BIM");
        isd.extend_from_slice(b"Layr");
        isd.extend_from_slice(&999u32.to_be_bytes());
        assert!(find_photoshop_layer_block(&isd).is_none());
    }

    #[test]
    fn reads_out_of_line_tag_bytes_little_endian() {
        // IFD0 at 8, one entry for tag 37724 (UNDEFINED, 5 bytes) whose data
        // lives out-of-line at offset 30.
        let mut d = vec![0u8; 64];
        d[0..2].copy_from_slice(b"II");
        d[2..4].copy_from_slice(&42u16.to_le_bytes());
        d[4..8].copy_from_slice(&8u32.to_le_bytes());
        d[8..10].copy_from_slice(&1u16.to_le_bytes()); // 1 entry
        d[10..12].copy_from_slice(&TAG_IMAGE_SOURCE_DATA.to_le_bytes());
        d[12..14].copy_from_slice(&7u16.to_le_bytes()); // UNDEFINED
        d[14..18].copy_from_slice(&5u32.to_le_bytes()); // count 5
        d[18..22].copy_from_slice(&30u32.to_le_bytes()); // offset
        d[30..35].copy_from_slice(&[10, 20, 30, 40, 50]);
        assert_eq!(
            tiff_tag_bytes(&d, TAG_IMAGE_SOURCE_DATA),
            Some(&[10u8, 20, 30, 40, 50][..])
        );
        assert_eq!(tiff_tag_bytes(&d, 999), None);
    }

    #[test]
    fn reads_inline_tag_bytes_big_endian() {
        // A short (<=4 byte) value is stored inline in the entry's value field.
        let mut d = vec![0u8; 32];
        d[0..2].copy_from_slice(b"MM");
        d[2..4].copy_from_slice(&42u16.to_be_bytes());
        d[4..8].copy_from_slice(&8u32.to_be_bytes());
        d[8..10].copy_from_slice(&1u16.to_be_bytes());
        d[10..12].copy_from_slice(&TAG_IMAGE_SOURCE_DATA.to_be_bytes());
        d[12..14].copy_from_slice(&1u16.to_be_bytes()); // BYTE
        d[14..18].copy_from_slice(&3u32.to_be_bytes()); // count 3
        d[18..21].copy_from_slice(&[1, 2, 3]); // inline value
        assert_eq!(
            tiff_tag_bytes(&d, TAG_IMAGE_SOURCE_DATA),
            Some(&[1u8, 2, 3][..])
        );
    }
}

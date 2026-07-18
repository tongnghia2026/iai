use super::{ExportOptions, Exporter, Importer};
use crate::core::canvas::Canvas;
use std::path::Path;

pub struct TiffImporter;
pub struct TiffExporter;

impl Importer for TiffImporter {
    fn extensions(&self) -> &[&str] {
        &["tiff", "tif"]
    }

    fn import(&self, path: &Path) -> Result<Canvas, String> {
        // The `image` crate's TIFF decoder drops the ICC tag under size limits, so
        // read it directly via the `tiff` crate and pass it as the override.
        // import_canvas_managed preserves 16-bit precision for 16-bit TIFFs.
        let icc = read_tiff_icc(path);
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

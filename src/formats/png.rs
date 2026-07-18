use super::{ExportOptions, Exporter, Importer};
use crate::core::canvas::Canvas;
use std::path::Path;

pub struct PngImporter;
pub struct PngExporter;

impl Importer for PngImporter {
    fn extensions(&self) -> &[&str] {
        &["png"]
    }

    fn import(&self, path: &Path) -> Result<Canvas, String> {
        // import_canvas_managed preserves 16-bit precision for 16-bit PNGs.
        let (mut canvas, _source) = super::import_canvas_managed(path, None)?;
        canvas.metadata.resolution_ppi = read_png_dpi(path).unwrap_or(72.0);
        Ok(canvas)
    }
}

impl Exporter for PngExporter {
    fn extensions(&self) -> &[&str] {
        &["png"]
    }

    fn export(&self, canvas: &Canvas, path: &Path, opts: &ExportOptions) -> Result<(), String> {
        // 16-bit document → write 16-bit RGBA (big-endian) when precision survives;
        // otherwise the usual 8-bit path.
        let hdr = if opts.flatten {
            canvas.export_flat16()
        } else {
            None
        };
        let (pixels, depth) = match hdr {
            Some(bytes) => (bytes, png::BitDepth::Sixteen),
            None => {
                let p = if opts.flatten {
                    canvas.export_flat()
                } else {
                    canvas.pixels.clone()
                };
                (p, png::BitDepth::Eight)
            }
        };

        let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
        let buf = std::io::BufWriter::new(file);

        // Build Info directly so the ICC profile (iCCP chunk) can be embedded —
        // the png Encoder has no `set_icc_profile` setter.
        let mut info = png::Info::default();
        info.width = canvas.width;
        info.height = canvas.height;
        info.color_type = png::ColorType::Rgba;
        info.bit_depth = depth;

        if opts.embed_metadata {
            let dpi = canvas.metadata.resolution_ppi;
            if dpi > 0.0 {
                let ppm = (dpi / 0.0254) as u32;
                info.pixel_dims = Some(png::PixelDimensions {
                    xppu: ppm,
                    yppu: ppm,
                    unit: png::Unit::Meter,
                });
            }
        }

        if opts.embed_icc {
            let icc = super::export_icc_bytes(canvas);
            if !icc.is_empty() {
                info.icc_profile = Some(std::borrow::Cow::Owned(icc));
            }
        }

        let encoder = png::Encoder::with_info(buf, info).map_err(|e| e.to_string())?;
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&pixels).map_err(|e| e.to_string())
    }
}

/// Read DPI from PNG pHYs chunk. Returns None if chunk absent or unit not meter.
fn read_png_dpi(path: &Path) -> Option<f32> {
    let file = std::fs::File::open(path).ok()?;
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let reader = decoder.read_info().ok()?;
    let phys = reader.info().pixel_dims?;
    if phys.unit == png::Unit::Meter && phys.xppu > 0 {
        Some(phys.xppu as f32 * 0.0254)
    } else {
        None
    }
}

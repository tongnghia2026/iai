use super::{ExportOptions, Exporter};
use crate::core::canvas::Canvas;
use std::path::Path;

pub struct BmpExporter;

impl Exporter for BmpExporter {
    fn extensions(&self) -> &[&str] {
        &["bmp"]
    }

    fn export(&self, canvas: &Canvas, path: &Path, opts: &ExportOptions) -> Result<(), String> {
        let pixels = if opts.flatten {
            canvas
                .materialize_flat_for_export()
                .ok_or_else(|| "Could not materialize the canvas for BMP export".to_string())?
        } else {
            canvas.pixels.clone()
        };
        let expected = Canvas::checked_rgba_len(canvas.width, canvas.height)
            .ok_or_else(|| "BMP dimensions are too large".to_string())?;
        if pixels.len() != expected {
            return Err("BMP export has no complete RGBA frame".to_string());
        }

        // Few programs read the alpha of a 32-bit BMP the same way, so the file
        // is plain 24-bit with transparency laid over white, as JPEG does.
        let mut rgb = Vec::with_capacity(expected / 4 * 3);
        for px in pixels.chunks_exact(4) {
            let a = px[3] as u16;
            let inv_a = 255 - a;
            rgb.extend(
                px[..3]
                    .iter()
                    .map(|&c| ((c as u16 * a + 255 * inv_a) / 255) as u8),
            );
        }
        image::save_buffer_with_format(
            path,
            &rgb,
            canvas.width,
            canvas.height,
            image::ColorType::Rgb8,
            image::ImageFormat::Bmp,
        )
        .map_err(|e| e.to_string())
    }
}

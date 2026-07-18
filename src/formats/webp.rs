use super::{ExportOptions, Exporter, Importer};
use crate::core::canvas::Canvas;
use std::path::Path;

pub struct WebpImporter;
pub struct WebpExporter;

impl Importer for WebpImporter {
    fn extensions(&self) -> &[&str] {
        &["webp"]
    }

    fn import(&self, path: &Path) -> Result<Canvas, String> {
        let (raw, w, h, profile, source) = super::import_color_managed(path)?;
        let mut canvas = Canvas::from_rgba(raw, w, h);
        canvas.icc_profile = profile;
        canvas.metadata.source_profile = source;
        Ok(canvas)
    }
}

impl Exporter for WebpExporter {
    fn extensions(&self) -> &[&str] {
        &["webp"]
    }

    fn export(&self, canvas: &Canvas, path: &Path, opts: &ExportOptions) -> Result<(), String> {
        let pixels = if opts.flatten {
            canvas.export_flat()
        } else {
            canvas.pixels.clone()
        };
        image::save_buffer(
            path,
            &pixels,
            canvas.width,
            canvas.height,
            image::ColorType::Rgba8,
        )
        .map_err(|e| e.to_string())
    }
}

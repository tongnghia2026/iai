#![allow(dead_code)]

pub mod iai;
pub mod iai_palette;
pub mod iai_vector;
pub mod jpeg;
pub mod pdf;
pub mod png;
pub mod psd;
pub mod psd_adjust;
pub mod psd_descriptor;
pub mod psd_text;
pub mod raw;
pub mod raw_exif;
pub mod raw_preview;
pub mod tiff;
pub mod webp;

use crate::core::canvas::Canvas;
use std::path::Path;

/// Decode an image file to RGBA8 *and* return any embedded ICC profile, with the
/// decoder's allocation capped to [`crate::core::canvas::MAX_DIMENSION`]. Use this
/// instead of `image::open` for any importer handed an untrusted file, so a
/// crafted header cannot OOM the process.
pub fn decode_rgba_limited_with_icc(
    path: &Path,
) -> Result<(image::RgbaImage, Option<Vec<u8>>), String> {
    let (img, icc) = decode_dynamic_with_icc(path)?;
    Ok((img.to_rgba8(), icc))
}

/// Decode to a `DynamicImage` (preserving the source bit depth) plus any embedded
/// ICC profile, with the dimension cap applied. Shared by the 8-bit and 16-bit
/// import paths.
pub fn decode_dynamic_with_icc(
    path: &Path,
) -> Result<(image::DynamicImage, Option<Vec<u8>>), String> {
    use image::ImageDecoder;

    let reader = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;

    // Build the decoder WITHOUT limits first and read the ICC profile: image's
    // TIFF decoder drops the ICC tag once `set_limits` is applied, so the profile
    // must be read on the pristine decoder. Reading a tag/header allocates
    // nothing dangerous; the dimension cap is applied below for the actual decode.
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let icc = decoder.icc_profile().ok().flatten();

    let max = crate::core::canvas::MAX_DIMENSION;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(max);
    limits.max_image_height = Some(max);
    decoder
        .set_limits(limits)
        .map_err(|e| format!("{e} (max supported size is {max}x{max})"))?;

    let img = image::DynamicImage::from_decoder(decoder)
        .map_err(|e| format!("{e} (max supported size is {max}x{max})"))?;
    Ok((img, icc))
}

/// Decode + colour-manage into a ready [`Canvas`], preserving 16-bit precision
/// when the source is 16-bit AND untagged or sRGB (colour correctness wins over
/// precision for tagged non-sRGB sources, which take the 8-bit conversion path).
/// Returns `(canvas, source_profile_name)`. Used by the PNG/TIFF importers.
/// `icc_override` lets a caller supply the ICC itself (TIFF, where the `image`
/// crate drops the tag under size limits — see `read_tiff_icc`); when `None` the
/// profile read by the `image` decoder is used.
pub fn import_canvas_managed(
    path: &Path,
    icc_override: Option<Vec<u8>>,
) -> Result<(Canvas, String), String> {
    use image::ColorType;

    let (img, img_icc) = decode_dynamic_with_icc(path)?;
    let icc = icc_override.or(img_icc);
    let (w, h) = {
        use image::GenericImageView;
        img.dimensions()
    };
    if w == 0 || h == 0 {
        return Err("Ảnh có kích thước bằng 0".into());
    }

    let is_16bit = matches!(
        img.color(),
        ColorType::Rgb16 | ColorType::Rgba16 | ColorType::L16 | ColorType::La16
    );
    // 16-bit is preserved only when no colour conversion is needed.
    let icc_is_srgb = match icc.as_deref() {
        None => true,
        Some(bytes) => crate::core::cms::profile_from_bytes(bytes)
            .map(|p| crate::core::cms::name_is_srgb(&crate::core::cms::profile_name(&p)))
            .unwrap_or(false),
    };

    if is_16bit && icc_is_srgb {
        let px16 = img.to_rgba16().into_raw();
        let mut canvas = Canvas::from_rgba16(px16, w, h);
        canvas.icc_profile = crate::core::canvas::IccProfile {
            name: crate::core::cms::WorkingProfile::Srgb.name().to_string(),
            data: crate::core::cms::srgb_icc_bytes(),
        };
        let source = icc
            .as_deref()
            .and_then(crate::core::cms::profile_from_bytes)
            .map(|p| crate::core::cms::profile_name(&p))
            .unwrap_or_default();
        canvas.metadata.source_profile = source.clone();
        return Ok((canvas, source));
    }

    // 8-bit path (also handles tagged non-sRGB 16-bit, converted to sRGB).
    let mut raw = img.to_rgba8().into_raw();
    let (tag, source) = apply_input_profile(&mut raw, icc.as_deref());
    let mut canvas = Canvas::from_rgba(raw, w, h);
    canvas.icc_profile = tag;
    canvas.metadata.source_profile = source.clone();
    Ok((canvas, source))
}

/// Decode to RGBA8 (discarding any embedded ICC profile). Prefer
/// [`import_color_managed`] for importers so colour management is applied.
pub fn decode_rgba_limited(path: &Path) -> Result<image::RgbaImage, String> {
    Ok(decode_rgba_limited_with_icc(path)?.0)
}

/// Decode `path` with input colour management: any embedded ICC profile that is
/// not already sRGB is converted into the sRGB working space (alpha preserved),
/// and the document is tagged sRGB. Returns the canvas-ready RGBA buffer, its
/// dimensions, the document profile tag, and the name of the source profile it
/// was converted from (empty when untagged or already sRGB).
pub fn import_color_managed(
    path: &Path,
) -> Result<(Vec<u8>, u32, u32, crate::core::canvas::IccProfile, String), String> {
    let (rgba, icc) = decode_rgba_limited_with_icc(path)?;
    let (w, h) = rgba.dimensions();
    let mut raw = rgba.into_raw();
    let (tag, source) = apply_input_profile(&mut raw, icc.as_deref());
    Ok((raw, w, h, tag, source))
}

/// Convert `buf` (RGBA8) from its embedded profile into sRGB in place and return
/// the document tag + source-profile name. Untagged or unreadable → left as-is,
/// treated as sRGB (empty tag). Importers that read the ICC profile themselves
/// (e.g. TIFF, where the `image` crate drops it) call this directly.
pub(crate) fn apply_input_profile(
    buf: &mut [u8],
    icc: Option<&[u8]>,
) -> (crate::core::canvas::IccProfile, String) {
    use crate::core::cms;
    let srgb_tag = || crate::core::canvas::IccProfile {
        name: cms::WorkingProfile::Srgb.name().to_string(),
        data: cms::srgb_icc_bytes(),
    };

    let Some(bytes) = icc else {
        return (crate::core::canvas::IccProfile::default(), String::new());
    };
    let Some(src) = cms::profile_from_bytes(bytes) else {
        return (crate::core::canvas::IccProfile::default(), String::new());
    };
    let name = cms::profile_name(&src);
    if cms::name_is_srgb(&name) {
        // Already sRGB: tag it, no pixel change.
        return (srgb_tag(), name);
    }
    let dst = cms::srgb_profile();
    cms::convert_rgba8(buf, &src, &dst, cms::DEFAULT_INTENT);
    (srgb_tag(), name)
}

/// ICC bytes to embed for `canvas` when "Embed Color Profile" is enabled: the
/// document's assigned profile, or sRGB when untagged.
pub fn export_icc_bytes(canvas: &Canvas) -> Vec<u8> {
    if canvas.icc_profile.data.is_empty() {
        crate::core::cms::srgb_icc_bytes()
    } else {
        canvas.icc_profile.data.clone()
    }
}

#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub flatten: bool,
    pub embed_metadata: bool,
    pub embed_icc: bool,
    /// Fit the longest side to this many pixels on export (downscale only);
    /// `None` keeps the native size. Raster image formats only. Because output
    /// sizing/sharpening is a display-referred final step, a reprocessed export
    /// is written as 8-bit.
    pub resize_long_edge: Option<u32>,
    /// Output sharpening strength (0..=100, 0 = off), applied AFTER the resize so
    /// it re-crisps detail at the final delivery size.
    pub output_sharpen: u8,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            flatten: true,
            embed_metadata: true,
            embed_icc: true,
            resize_long_edge: None,
            output_sharpen: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExportFormat {
    Png { compression: u8 },
    Jpeg { quality: u8 },
    Webp { quality: u8, lossless: bool },
    Tiff,
    Bmp,
    Iai,
    Pdf { dpi: u32 },
}

impl ExportFormat {
    pub fn extension(&self) -> &str {
        match self {
            ExportFormat::Png { .. } => "png",
            ExportFormat::Jpeg { .. } => "jpg",
            ExportFormat::Webp { .. } => "webp",
            ExportFormat::Tiff => "tiff",
            ExportFormat::Bmp => "bmp",
            ExportFormat::Iai => "iai",
            ExportFormat::Pdf { .. } => "pdf",
        }
    }

    pub fn name(&self) -> &str {
        match self {
            ExportFormat::Png { .. } => "PNG",
            ExportFormat::Jpeg { .. } => "JPEG",
            ExportFormat::Webp { .. } => "WebP",
            ExportFormat::Tiff => "TIFF",
            ExportFormat::Bmp => "BMP",
            ExportFormat::Iai => "iAi Project",
            ExportFormat::Pdf { .. } => "PDF",
        }
    }

    pub fn all_export() -> Vec<ExportFormat> {
        vec![
            ExportFormat::Png { compression: 6 },
            ExportFormat::Jpeg { quality: 90 },
            ExportFormat::Webp {
                quality: 90,
                lossless: false,
            },
            ExportFormat::Tiff,
            ExportFormat::Bmp,
            ExportFormat::Pdf { dpi: 300 },
            ExportFormat::Iai,
        ]
    }

    pub fn from_extension(ext: &str) -> Option<ExportFormat> {
        match ext.to_lowercase().as_str() {
            "png" => Some(ExportFormat::Png { compression: 6 }),
            "jpg" | "jpeg" => Some(ExportFormat::Jpeg { quality: 90 }),
            "webp" => Some(ExportFormat::Webp {
                quality: 90,
                lossless: false,
            }),
            "tiff" | "tif" => Some(ExportFormat::Tiff),
            "bmp" => Some(ExportFormat::Bmp),
            "pdf" => Some(ExportFormat::Pdf { dpi: 300 }),
            "iai" => Some(ExportFormat::Iai),
            _ => None,
        }
    }
}

pub trait Importer: Send + Sync {
    fn extensions(&self) -> &[&str];
    fn import(&self, path: &Path) -> Result<Canvas, String>;
    fn import_many(&self, path: &Path) -> Result<Vec<Canvas>, String> {
        self.import(path).map(|canvas| vec![canvas])
    }
    fn can_import(&self, path: &Path) -> bool {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();
        self.extensions().iter().any(|e| *e == ext.as_str())
    }
}

pub trait Exporter: Send + Sync {
    fn extensions(&self) -> &[&str];
    fn export(&self, canvas: &Canvas, path: &Path, opts: &ExportOptions) -> Result<(), String>;
    fn can_export(&self, path: &Path) -> bool {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();
        self.extensions().iter().any(|e| *e == ext.as_str())
    }
}

pub struct FormatRegistry {
    importers: Vec<Box<dyn Importer>>,
    exporters: Vec<Box<dyn Exporter>>,
}

impl FormatRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            importers: Vec::new(),
            exporters: Vec::new(),
        };
        registry.register_defaults();
        registry
    }

    fn register_defaults(&mut self) {
        self.importers.push(Box::new(png::PngImporter));
        self.importers.push(Box::new(jpeg::JpegImporter));
        self.importers.push(Box::new(webp::WebpImporter));
        self.importers.push(Box::new(tiff::TiffImporter));
        self.importers.push(Box::new(iai::IaiImporter));
        self.importers.push(Box::new(psd::PsdImporter));
        self.importers.push(Box::new(raw::RawImporter));
        self.importers.push(Box::new(pdf::PdfImporter));
        self.exporters.push(Box::new(png::PngExporter));
        self.exporters.push(Box::new(jpeg::JpegExporter));
        self.exporters.push(Box::new(webp::WebpExporter));
        self.exporters.push(Box::new(tiff::TiffExporter));
        self.exporters.push(Box::new(pdf::PdfExporter));
        self.exporters.push(Box::new(iai::IaiExporter));
    }

    pub fn import(&self, path: &Path) -> Result<Canvas, String> {
        for importer in &self.importers {
            if importer.can_import(path) {
                return importer.import(path);
            }
        }
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("fdf"))
        {
            return Err(
                "FDF contains PDF form data but no renderable pages; open the corresponding PDF"
                    .to_string(),
            );
        }
        let (raw, w, h, profile, source) = import_color_managed(path).map_err(|e| {
            format!(
                "Không mở được {} (định dạng không hỗ trợ): {e}",
                path.display()
            )
        })?;
        if w == 0 || h == 0 {
            return Err("Ảnh có kích thước bằng 0".into());
        }
        let mut canvas = Canvas::from_rgba(raw, w, h);
        canvas.icc_profile = profile;
        canvas.metadata.source_profile = source;
        Ok(canvas)
    }

    pub fn import_many(&self, path: &Path) -> Result<Vec<Canvas>, String> {
        for importer in &self.importers {
            if importer.can_import(path) {
                return importer.import_many(path);
            }
        }
        self.import(path).map(|canvas| vec![canvas])
    }

    pub fn export(&self, canvas: &Canvas, path: &Path, opts: &ExportOptions) -> Result<(), String> {
        // Optional output sizing/sharpening produces a derived 8-bit canvas; when
        // it doesn't apply, `prepared` is None and the export is byte-identical to
        // before this feature existed.
        let prepared = prepare_output_canvas(canvas, path, opts);
        let target = prepared.as_ref().unwrap_or(canvas);
        for exporter in &self.exporters {
            if exporter.can_export(path) {
                return exporter.export(target, path, opts);
            }
        }
        Err(format!("No exporter found for: {}", path.display()))
    }

    pub fn supported_import_extensions(&self) -> Vec<&str> {
        let mut exts: Vec<&str> = self
            .importers
            .iter()
            .flat_map(|i| i.extensions().iter().copied())
            .collect();
        exts.dedup();
        exts
    }

    pub fn supported_export_extensions(&self) -> Vec<&str> {
        let mut exts: Vec<&str> = self
            .exporters
            .iter()
            .flat_map(|e| e.extensions().iter().copied())
            .collect();
        exts.dedup();
        exts
    }
}

/// Raster image extensions that support output sizing/sharpening on export.
fn is_raster_image_ext(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "tiff" | "tif" | "bmp"
    )
}

/// Build a derived, single-layer 8-bit canvas for output sizing/sharpening:
/// flatten → high-quality Lanczos downscale (longest-side fit, downscale only) →
/// output sharpen at the final grid. Returns `None` when the request does not
/// apply — no resize and no sharpen, a non-raster target, an empty or oversized
/// canvas, or a resize that would upscale — so those exports stay byte-identical.
fn prepare_output_canvas(canvas: &Canvas, path: &Path, opts: &ExportOptions) -> Option<Canvas> {
    if !opts.flatten || !is_raster_image_ext(path) {
        return None;
    }
    if opts.resize_long_edge.is_none() && opts.output_sharpen == 0 {
        return None;
    }
    let (w, h) = (canvas.width, canvas.height);
    if w == 0 || h == 0 || !Canvas::fits_flat_buffer(w, h) {
        return None;
    }
    let mut rgba = canvas.export_flat();
    if rgba.len() != (w as usize) * (h as usize) * 4 {
        return None;
    }
    let (mut cw, mut ch) = (w, h);

    if let Some(le) = opts.resize_long_edge {
        let longest = w.max(h);
        if le > 0 && (le as u64) < longest as u64 {
            let scale = le as f64 / longest as f64;
            let nw = ((w as f64 * scale).round() as u32).max(1);
            let nh = ((h as f64 * scale).round() as u32).max(1);
            let src = image::RgbaImage::from_raw(w, h, rgba)?;
            let dst = image::imageops::resize(&src, nw, nh, image::imageops::FilterType::Lanczos3);
            cw = nw;
            ch = nh;
            rgba = dst.into_raw();
        }
    }

    // Nothing to do (the resize was an up-scale we declined and no sharpening) —
    // keep the original export path.
    if cw == w && ch == h && opts.output_sharpen == 0 {
        return None;
    }

    if opts.output_sharpen > 0 {
        crate::core::output_sharpen::apply_output_sharpen(
            &mut rgba,
            cw as usize,
            ch as usize,
            opts.output_sharpen,
        );
    }

    let mut derived = Canvas::from_rgba(rgba, cw, ch);
    derived.icc_profile = canvas.icc_profile.clone();
    derived.metadata = canvas.metadata.clone();
    Some(derived)
}

#[cfg(test)]
mod icc_roundtrip_tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("iai_icc_{}_{}", std::process::id(), name));
        p
    }

    fn small_canvas() -> Canvas {
        Canvas::from_rgba(vec![128u8; 4 * 4 * 4], 4, 4)
    }

    #[test]
    fn fdf_reports_that_it_is_not_a_renderable_pdf() {
        let error = match FormatRegistry::new().import(Path::new("form-data.fdf")) {
            Ok(_) => panic!("FDF must not be treated as a renderable PDF"),
            Err(error) => error,
        };
        assert!(error.contains("form data"));
        assert!(error.contains("PDF"));
    }

    // Each format must embed an ICC profile that a standard decoder can read back
    // — exercises the PNG iCCP chunk, the JPEG APP2 splice and the TIFF tag 34675.
    fn assert_embeds(exporter: &dyn Exporter, ext: &str) {
        let canvas = small_canvas();
        let path = tmp(&format!("rt.{ext}"));
        let opts = ExportOptions {
            embed_icc: true,
            ..Default::default()
        };
        exporter.export(&canvas, &path, &opts).unwrap();
        // Round-trip through the importer (the app's real read path), which for
        // TIFF reads the ICC tag via the `tiff` crate.
        let loaded = FormatRegistry::new().import(&path).unwrap();
        assert!(
            !loaded.icc_profile.data.is_empty(),
            "{ext}: embedded ICC should round-trip on import"
        );
        std::fs::remove_file(&path).ok();
    }

    fn assert_no_embed(exporter: &dyn Exporter, ext: &str) {
        let canvas = small_canvas();
        let path = tmp(&format!("noicc.{ext}"));
        let opts = ExportOptions {
            embed_icc: false,
            ..Default::default()
        };
        exporter.export(&canvas, &path, &opts).unwrap();
        let loaded = FormatRegistry::new().import(&path).unwrap();
        assert!(
            loaded.icc_profile.data.is_empty(),
            "{ext}: no ICC expected when embedding is disabled"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn png_icc_roundtrip() {
        assert_embeds(&png::PngExporter, "png");
        assert_no_embed(&png::PngExporter, "png");
    }

    #[test]
    fn jpeg_icc_roundtrip() {
        assert_embeds(&jpeg::JpegExporter, "jpg");
        assert_no_embed(&jpeg::JpegExporter, "jpg");
    }

    #[test]
    fn tiff_icc_roundtrip() {
        assert_embeds(&tiff::TiffExporter, "tiff");
        assert_no_embed(&tiff::TiffExporter, "tiff");
    }

    // A 16-bit image must survive export→import bit-exact (precision preserved,
    // reimported as 16-bit). Exercises Canvas::from_rgba16 + 16-bit PNG/TIFF I/O.
    fn assert_16bit_roundtrip(ext: &str) {
        let reg = FormatRegistry::new();
        let (w, h) = (3u32, 2u32);
        // Non-zero low bytes → destroyed by any 8-bit truncation.
        let mut px16 = vec![0u16; (w * h * 4) as usize];
        for (i, v) in px16.iter_mut().enumerate() {
            *v = if i % 4 == 3 {
                0xFFFF // opaque alpha
            } else {
                ((i as u32 * 4097 + 123) & 0xFFFF) as u16
            };
        }
        let canvas = Canvas::from_rgba16(px16.clone(), w, h);
        assert_eq!(canvas.bit_depth, crate::core::canvas::BitDepth::Sixteen);

        let path = tmp(&format!("rt16.{ext}"));
        reg.export(&canvas, &path, &ExportOptions::default())
            .unwrap();
        let loaded = reg.import(&path).unwrap();
        assert_eq!(
            loaded.bit_depth,
            crate::core::canvas::BitDepth::Sixteen,
            "{ext}: must reimport as 16-bit"
        );
        let got = loaded.export_flat16_samples().expect("16-bit samples");
        assert_eq!(got, px16, "{ext}: 16-bit round-trip must be lossless");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn iai_mask_enabled_flag_roundtrips() {
        let reg = FormatRegistry::new();
        let mut canvas = small_canvas();
        let idx = canvas.layer_stack.active_idx;
        let mut mask = crate::core::layer::LayerMask::new_white(4, 4);
        mask.enabled = false;
        canvas.layer_stack.layers[idx].mask = Some(mask);

        let path = tmp("mask.iai");
        reg.export(&canvas, &path, &ExportOptions::default())
            .unwrap();
        let loaded = reg.import(&path).unwrap();
        let lmask = loaded.layer_stack.layers[idx]
            .mask
            .as_ref()
            .expect("mask survives the round-trip");
        assert!(!lmask.enabled, "disabled flag must persist in the manifest");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn png_16bit_roundtrip_is_lossless() {
        assert_16bit_roundtrip("png");
    }

    #[test]
    fn tiff_16bit_roundtrip_is_lossless() {
        assert_16bit_roundtrip("tiff");
    }
}

#[cfg(test)]
mod output_prep_tests {
    use super::*;
    use crate::core::canvas::Canvas;
    use std::path::Path;

    fn solid_canvas(w: u32, h: u32) -> Canvas {
        Canvas::from_rgba(vec![128u8; (w as usize) * (h as usize) * 4], w, h)
    }

    #[test]
    fn default_export_is_left_untouched() {
        let c = solid_canvas(64, 40);
        assert!(
            prepare_output_canvas(&c, Path::new("out.png"), &ExportOptions::default()).is_none()
        );
    }

    #[test]
    fn resize_fits_longest_side_and_downscales_only() {
        let c = solid_canvas(400, 200);
        let opts = ExportOptions {
            resize_long_edge: Some(100),
            ..ExportOptions::default()
        };
        let out =
            prepare_output_canvas(&c, Path::new("out.jpg"), &opts).expect("derived export canvas");
        assert_eq!((out.width, out.height), (100, 50));

        // An up-scale request is declined (output sizing only downscales).
        let up = ExportOptions {
            resize_long_edge: Some(800),
            ..ExportOptions::default()
        };
        assert!(prepare_output_canvas(&c, Path::new("out.jpg"), &up).is_none());
    }

    #[test]
    fn non_raster_targets_are_skipped() {
        let c = solid_canvas(400, 200);
        let opts = ExportOptions {
            resize_long_edge: Some(100),
            output_sharpen: 60,
            ..ExportOptions::default()
        };
        assert!(prepare_output_canvas(&c, Path::new("out.pdf"), &opts).is_none());
        assert!(prepare_output_canvas(&c, Path::new("out.iai"), &opts).is_none());
    }
}

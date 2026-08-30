use super::{ExportOptions, Exporter, Importer};
use crate::core::canvas::Canvas;
use lopdf::dictionary;
use std::io::Write;
use std::path::Path;

/// Default cap for the "Auto" resolution in the page dialog.
const PDF_AUTO_MAX_DPI: f32 = 300.0;
/// Absolute ceiling — even an explicit 600 DPI choice never exceeds this.
const PDF_HARD_MAX_DPI: f32 = 600.0;
const PDF_MIN_IMPORT_DPI: f32 = 18.0;
const MAX_PDF_PAGES: usize = 10_000;
const MAX_PDF_BYTES: u64 = 512 * 1024 * 1024;
/// Maximum raster size of one page. Multi-page imports are loaded lazily, so
/// this protects the active page without making its DPI depend on page count.
const MAX_PDF_PAGE_PIXELS: u64 = 50_000_000;
/// Maximum pixels returned by one `render_selected` call. Callers handling large
/// documents should render one page at a time instead of silently lowering DPI.
const MAX_PDF_RENDER_BATCH_PIXELS: u64 = 50_000_000;

pub struct PdfImporter;

/// Lightweight description of a PDF: how many pages and each page's size in
/// points. Produced by [`PdfImporter::probe`] without rasterizing anything, so
/// the page-selection dialog can be shown before the (expensive) render step.
/// Plain data — `Send` across the load worker thread boundary.
pub struct PdfProbe {
    pub page_count: usize,
    pub page_dims: Vec<(f32, f32)>,
}

pub struct PdfRenderResult {
    pub document_id: crate::core::document::DocumentId,
    pub page_index: usize,
    pub canvas: Result<Canvas, String>,
}

struct PdfRenderRequest {
    document_id: crate::core::document::DocumentId,
    page_index: usize,
    dpi: f32,
}

pub struct PdfRenderService {
    request_tx: std::sync::mpsc::Sender<PdfRenderRequest>,
    result_rx: std::sync::mpsc::Receiver<PdfRenderResult>,
}

impl PdfRenderService {
    pub fn start(path: std::path::PathBuf) -> Self {
        let (request_tx, request_rx) = std::sync::mpsc::channel::<PdfRenderRequest>();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let opened = PdfImporter::open_pdf(&path).and_then(|pdf| {
                let dimensions = PdfImporter::collect_dims(&pdf)?;
                Ok((pdf, dimensions))
            });
            let cache = hayro::RenderCache::new();
            let interpreter_settings = hayro::hayro_interpret::InterpreterSettings::default();
            while let Ok(request) = request_rx.recv() {
                let canvas = match &opened {
                    Ok((pdf, dimensions)) => PdfImporter::render_open_page(
                        pdf,
                        dimensions,
                        request.page_index,
                        request.dpi,
                        &cache,
                        &interpreter_settings,
                    ),
                    Err(error) => Err(error.clone()),
                };
                let _ = result_tx.send(PdfRenderResult {
                    document_id: request.document_id,
                    page_index: request.page_index,
                    canvas,
                });
            }
        });
        Self {
            request_tx,
            result_rx,
        }
    }

    pub fn request(
        &self,
        document_id: crate::core::document::DocumentId,
        page_index: usize,
        dpi: f32,
    ) -> Result<(), String> {
        self.request_tx
            .send(PdfRenderRequest {
                document_id,
                page_index,
                dpi,
            })
            .map_err(|_| "PDF render service stopped".to_string())
    }

    pub fn try_recv(&self) -> Result<PdfRenderResult, std::sync::mpsc::TryRecvError> {
        self.result_rx.try_recv()
    }

    #[cfg(test)]
    fn recv_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> Result<PdfRenderResult, std::sync::mpsc::RecvTimeoutError> {
        self.result_rx.recv_timeout(timeout)
    }
}

/// Returns true when `path` looks like a PDF (used to route file opens through
/// the page-selection dialog instead of the generic loader).
pub fn is_pdf_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

impl PdfImporter {
    /// Read + parse the PDF, enforcing the file-size limit. hayro takes ownership
    /// of the bytes, so this returns the parsed document.
    fn open_pdf(path: &Path) -> Result<hayro::hayro_syntax::Pdf, String> {
        let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
        if metadata.len() > MAX_PDF_BYTES {
            return Err(format!(
                "PDF is too large ({} MB; limit is {} MB)",
                metadata.len() / (1024 * 1024),
                MAX_PDF_BYTES / (1024 * 1024)
            ));
        }
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        hayro::hayro_syntax::Pdf::new(bytes).map_err(|e| format!("Could not parse PDF: {e:?}"))
    }

    /// Collect per-page dimensions (points), enforcing the page-count limit and
    /// rejecting pages with non-finite / sub-pixel sizes.
    fn collect_dims(pdf: &hayro::hayro_syntax::Pdf) -> Result<Vec<(f32, f32)>, String> {
        let page_count = pdf.pages().iter().count();
        if page_count == 0 {
            return Err("PDF has no pages".to_string());
        }
        if page_count > MAX_PDF_PAGES {
            return Err(format!(
                "PDF has {page_count} pages; the current import limit is {MAX_PDF_PAGES}"
            ));
        }
        let mut dims = Vec::with_capacity(page_count);
        for (page_index, page) in pdf.pages().iter().enumerate() {
            let (page_width, page_height) = page.render_dimensions();
            if !page_width.is_finite()
                || !page_height.is_finite()
                || page_width < 1.0
                || page_height < 1.0
            {
                return Err(format!("PDF page {} has an invalid size", page_index + 1));
            }
            dims.push((page_width, page_height));
        }
        Ok(dims)
    }

    /// Parse the PDF and report its page count + sizes without rasterizing.
    pub fn probe(path: &Path) -> Result<PdfProbe, String> {
        let pdf = Self::open_pdf(path)?;
        let page_dims = Self::collect_dims(&pdf)?;
        Ok(PdfProbe {
            page_count: page_dims.len(),
            page_dims,
        })
    }

    fn render_open_page<'a>(
        pdf: &'a hayro::hayro_syntax::Pdf,
        dimensions: &[(f32, f32)],
        page_index: usize,
        target_dpi: f32,
        cache: &hayro::RenderCache<'a>,
        interpreter_settings: &hayro::hayro_interpret::InterpreterSettings,
    ) -> Result<Canvas, String> {
        let (page_width, page_height) = dimensions
            .get(page_index)
            .copied()
            .ok_or_else(|| format!("PDF page {} is out of range", page_index + 1))?;
        let target = target_dpi.clamp(PDF_MIN_IMPORT_DPI, PDF_HARD_MAX_DPI);
        let page_points = (page_width as f64 * page_height as f64).max(1.0);
        let pixel_scale = ((MAX_PDF_PAGE_PIXELS as f64 * 0.98) / page_points).sqrt() as f32;
        let dimension_scale = (crate::core::canvas::MAX_DIMENSION as f32 / page_width)
            .min(crate::core::canvas::MAX_DIMENSION as f32 / page_height);
        let render_scale = (target / 72.0).min(pixel_scale).min(dimension_scale);
        if !render_scale.is_finite() || render_scale < PDF_MIN_IMPORT_DPI / 72.0 {
            return Err(format!(
                "PDF page is too large to rasterize safely, even at {} DPI",
                PDF_MIN_IMPORT_DPI as u32
            ));
        }
        let page = pdf
            .pages()
            .iter()
            .nth(page_index)
            .ok_or_else(|| format!("PDF page {} is missing", page_index + 1))?;
        let width = (page_width * render_scale).ceil() as u32;
        let height = (page_height * render_scale).ceil() as u32;
        let settings = hayro::RenderSettings {
            x_scale: render_scale,
            y_scale: render_scale,
            width: Some(width as u16),
            height: Some(height as u16),
            bg_color: hayro::vello_cpu::color::palette::css::WHITE,
        };
        let pixmap = hayro::render(page, cache, interpreter_settings, &settings);
        let width = u32::from(pixmap.width());
        let height = u32::from(pixmap.height());
        if width == 0 || height == 0 {
            return Err(format!("PDF page {} has zero size", page_index + 1));
        }
        let mut canvas = Canvas::from_rgba(pixmap.data_as_u8_slice().to_vec(), width, height);
        canvas.metadata.resolution_ppi = render_scale * 72.0;
        Ok(canvas)
    }

    /// Rasterize just the requested page indices (0-based). `target_dpi` is an
    /// upper bound (the dialog's resolution choice); `None` uses the Auto cap.
    /// Resolution is limited per page, never divided by the number of pages.
    /// A request whose aggregate output is too large is rejected so callers can
    /// render lazily instead of receiving silently downscaled pages.
    /// Duplicate/unsorted input is normalized; output is in ascending page order.
    pub fn render_selected(
        path: &Path,
        pages: &[usize],
        target_dpi: Option<f32>,
    ) -> Result<Vec<Canvas>, String> {
        if pages.is_empty() {
            return Err("No PDF pages selected".to_string());
        }
        let pdf = Self::open_pdf(path)?;
        let all_dims = Self::collect_dims(&pdf)?;
        let page_count = all_dims.len();

        // Sorted, unique set of the pages to render.
        let wanted: std::collections::BTreeSet<usize> = pages.iter().copied().collect();
        if let Some(&max_idx) = wanted.iter().next_back() {
            if max_idx >= page_count {
                return Err(format!(
                    "PDF page {} is out of range (document has {page_count})",
                    max_idx + 1
                ));
            }
        }

        // Per-page limits only: page count must never change a page's DPI.
        let mut dimension_scale = f32::INFINITY;
        let mut pixel_scale = f32::INFINITY;
        for &idx in &wanted {
            let (page_width, page_height) = all_dims[idx];
            dimension_scale = dimension_scale
                .min(crate::core::canvas::MAX_DIMENSION as f32 / page_width)
                .min(crate::core::canvas::MAX_DIMENSION as f32 / page_height);
            let page_points = (page_width as f64 * page_height as f64).max(1.0);
            let page_budget = MAX_PDF_PAGE_PIXELS as f64 * 0.98;
            pixel_scale = pixel_scale.min((page_budget / page_points).sqrt() as f32);
        }

        let target = target_dpi
            .unwrap_or(PDF_AUTO_MAX_DPI)
            .clamp(PDF_MIN_IMPORT_DPI, PDF_HARD_MAX_DPI);
        let max_scale = target / 72.0;
        let min_scale = PDF_MIN_IMPORT_DPI / 72.0;
        let render_scale = max_scale.min(pixel_scale).min(dimension_scale);
        if !render_scale.is_finite() || render_scale < min_scale {
            return Err(format!(
                "PDF is too large to rasterize safely, even at {} DPI",
                PDF_MIN_IMPORT_DPI as u32
            ));
        }
        let import_dpi = render_scale * 72.0;

        let batch_pixels = wanted.iter().try_fold(0u64, |total, &idx| {
            let (page_width, page_height) = all_dims[idx];
            let width = (page_width * render_scale).ceil() as u64;
            let height = (page_height * render_scale).ceil() as u64;
            total.checked_add(width.checked_mul(height)?)
        });
        if batch_pixels.is_none_or(|pixels| pixels > MAX_PDF_RENDER_BATCH_PIXELS) {
            return Err(format!(
                "PDF render batch exceeds {} megapixels; render fewer pages at a time",
                MAX_PDF_RENDER_BATCH_PIXELS / 1_000_000
            ));
        }

        let cache = hayro::RenderCache::new();
        let interpreter_settings = hayro::hayro_interpret::InterpreterSettings::default();

        let mut canvases = Vec::with_capacity(wanted.len());
        for (page_index, page) in pdf.pages().iter().enumerate() {
            if !wanted.contains(&page_index) {
                continue;
            }
            let (page_width, page_height) = all_dims[page_index];
            let width = (page_width * render_scale).ceil() as u32;
            let height = (page_height * render_scale).ceil() as u32;
            let render_settings = hayro::RenderSettings {
                x_scale: render_scale,
                y_scale: render_scale,
                width: Some(width as u16),
                height: Some(height as u16),
                bg_color: hayro::vello_cpu::color::palette::css::WHITE,
            };
            let pixmap = hayro::render(page, &cache, &interpreter_settings, &render_settings);
            let width = u32::from(pixmap.width());
            let height = u32::from(pixmap.height());
            if width == 0 || height == 0 {
                return Err(format!("PDF page {} has zero size", page_index + 1));
            }
            let rgba = pixmap.data_as_u8_slice().to_vec();
            let mut canvas = Canvas::from_rgba(rgba, width, height);
            canvas.metadata.resolution_ppi = import_dpi;
            canvases.push(canvas);
        }

        Ok(canvases)
    }
}

impl Importer for PdfImporter {
    fn extensions(&self) -> &[&str] {
        &["pdf"]
    }

    fn import(&self, path: &Path) -> Result<Canvas, String> {
        Self::render_selected(path, &[0], None)?
            .into_iter()
            .next()
            .ok_or_else(|| "PDF has no pages".to_string())
    }

    fn import_many(&self, path: &Path) -> Result<Vec<Canvas>, String> {
        let probe = Self::probe(path)?;
        let all: Vec<usize> = (0..probe.page_count).collect();
        Self::render_selected(path, &all, None)
    }
}

pub enum HybridPageContent {
    Original,
    Overlay {
        rgba: Vec<u8>,
        vectors: Vec<crate::core::print::PdfVectorObject>,
        width: u32,
        height: u32,
        dpi: f32,
    },
    Raster {
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        dpi: f32,
    },
}

pub struct HybridPage {
    pub source_index: usize,
    pub content: HybridPageContent,
}

pub struct HybridPdf {
    pub bytes: Vec<u8>,
    pub vector_pages: usize,
    pub overlay_pages: usize,
    pub raster_pages: usize,
}

#[derive(Clone, Copy)]
struct PdfRect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

fn object_number(object: &lopdf::Object) -> Option<f32> {
    match object {
        lopdf::Object::Integer(value) => Some(*value as f32),
        lopdf::Object::Real(value) => Some(*value),
        _ => None,
    }
}

fn inherited_page_object(
    document: &lopdf::Document,
    mut object_id: lopdf::ObjectId,
    key: &[u8],
) -> Option<lopdf::Object> {
    let mut seen = std::collections::HashSet::new();
    while seen.insert(object_id) {
        let dictionary = document.get_dictionary(object_id).ok()?;
        if let Ok(object) = dictionary.get(key) {
            return Some(object.clone());
        }
        object_id = dictionary.get(b"Parent").ok()?.as_reference().ok()?;
    }
    None
}

fn dereferenced_object<'a>(
    document: &'a lopdf::Document,
    object: &'a lopdf::Object,
) -> Option<&'a lopdf::Object> {
    document.dereference(object).ok().map(|(_, object)| object)
}

fn page_rect(document: &lopdf::Document, page_id: lopdf::ObjectId) -> Option<PdfRect> {
    let object = inherited_page_object(document, page_id, b"CropBox")
        .or_else(|| inherited_page_object(document, page_id, b"MediaBox"))?;
    let array = dereferenced_object(document, &object)?.as_array().ok()?;
    if array.len() < 4 {
        return None;
    }
    let x0 = object_number(dereferenced_object(document, &array[0])?)?;
    let y0 = object_number(dereferenced_object(document, &array[1])?)?;
    let x1 = object_number(dereferenced_object(document, &array[2])?)?;
    let y1 = object_number(dereferenced_object(document, &array[3])?)?;
    let width = (x1 - x0).abs();
    let height = (y1 - y0).abs();
    (width > 0.0 && height > 0.0).then_some(PdfRect {
        x: x0.min(x1),
        y: y0.min(y1),
        width,
        height,
    })
}

fn page_rotation(document: &lopdf::Document, page_id: lopdf::ObjectId) -> i64 {
    inherited_page_object(document, page_id, b"Rotate")
        .as_ref()
        .and_then(|object| dereferenced_object(document, object))
        .and_then(|object| object.as_i64().ok())
        .unwrap_or(0)
        .rem_euclid(360)
}

fn page_user_unit(document: &lopdf::Document, page_id: lopdf::ObjectId) -> f32 {
    inherited_page_object(document, page_id, b"UserUnit")
        .as_ref()
        .and_then(|object| dereferenced_object(document, object))
        .and_then(object_number)
        .unwrap_or(1.0)
}

fn page_accepts_overlay(
    document: &lopdf::Document,
    page_id: lopdf::ObjectId,
    width: u32,
    height: u32,
    dpi: f32,
) -> bool {
    let Some(rect) = page_rect(document, page_id) else {
        return false;
    };
    if page_rotation(document, page_id) != 0
        || (page_user_unit(document, page_id) - 1.0).abs() > 0.001
    {
        return false;
    }
    let dpi = dpi.max(1.0);
    let raster_width = width as f32 / dpi * 72.0;
    let raster_height = height as f32 / dpi * 72.0;
    (rect.width - raster_width).abs() <= 1.0 && (rect.height - raster_height).abs() <= 1.0
}

pub fn hybrid_overlay_compatibility(path: &Path) -> Result<Vec<bool>, String> {
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    let document = lopdf::Document::load_mem(&bytes)
        .map_err(|error| format!("Could not parse PDF structure: {error}"))?;
    Ok(document
        .get_pages()
        .into_values()
        .map(|page_id| {
            page_rotation(&document, page_id) == 0
                && (page_user_unit(&document, page_id) - 1.0).abs() <= 0.001
                && page_rect(&document, page_id).is_some()
        })
        .collect())
}

fn zlib_compress(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(bytes)
        .map_err(|error| format!("PDF image compression error: {error}"))?;
    encoder
        .finish()
        .map_err(|error| format!("PDF image compression error: {error}"))
}

fn flatten_white(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let pixel_count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| "PDF page dimensions overflow".to_string())?;
    if rgba.len() < pixel_count.saturating_mul(4) {
        return Err("PDF page buffer is too small".to_string());
    }
    let mut rgb = vec![0u8; pixel_count * 3];
    for (source, destination) in rgba
        .chunks_exact(4)
        .take(pixel_count)
        .zip(rgb.chunks_exact_mut(3))
    {
        let alpha = source[3] as u16;
        let inverse = 255 - alpha;
        for channel in 0..3 {
            destination[channel] = ((source[channel] as u16 * alpha + 255 * inverse) / 255) as u8;
        }
    }
    Ok(rgb)
}

fn alpha_bounds(rgba: &[u8], width: u32, height: u32) -> Option<(u32, u32, u32, u32)> {
    let mut x0 = width;
    let mut y0 = height;
    let mut x1 = 0;
    let mut y1 = 0;
    let mut found = false;
    for y in 0..height {
        for x in 0..width {
            let alpha = rgba.get(((y * width + x) * 4 + 3) as usize).copied()?;
            if alpha != 0 {
                found = true;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    found.then_some((x0, y0, x1, y1))
}

fn inherit_resources_onto_page(
    document: &mut lopdf::Document,
    page_id: lopdf::ObjectId,
) -> Result<(), String> {
    // Always detach the effective resources onto this page. Otherwise adding an
    // XObject through a shared/inherited dictionary would also mutate clean pages.
    let mut resources = inherited_page_object(document, page_id, b"Resources")
        .and_then(|object| {
            dereferenced_object(document, &object)
                .and_then(|object| object.as_dict().ok())
                .cloned()
        })
        .unwrap_or_default();
    for key in [b"XObject".as_slice(), b"Shading".as_slice()] {
        let detached = resources.get(key).ok().and_then(|value| {
            dereferenced_object(document, value)
                .and_then(|object| object.as_dict().ok())
                .cloned()
        });
        if let Some(dictionary) = detached {
            resources.set(key, dictionary);
        }
    }
    document
        .get_dictionary_mut(page_id)
        .map_err(|error| error.to_string())?
        .set("Resources", resources);
    Ok(())
}

fn add_overlay(
    document: &mut lopdf::Document,
    page_id: lopdf::ObjectId,
    name: &[u8],
    rgba: &[u8],
    width: u32,
    height: u32,
    dpi: f32,
) -> Result<bool, String> {
    if !page_accepts_overlay(document, page_id, width, height, dpi) {
        return Err("PDF page geometry is not safe for a vector overlay".to_string());
    }
    let Some((x0, y0, x1, y1)) = alpha_bounds(rgba, width, height) else {
        return Ok(false);
    };
    let crop_width = x1 - x0;
    let crop_height = y1 - y0;
    let pixels = (crop_width as usize) * (crop_height as usize);
    let mut rgb = Vec::with_capacity(pixels * 3);
    let mut alpha = Vec::with_capacity(pixels);
    for y in y0..y1 {
        for x in x0..x1 {
            let index = ((y * width + x) * 4) as usize;
            rgb.extend_from_slice(&rgba[index..index + 3]);
            alpha.push(rgba[index + 3]);
        }
    }

    let alpha_stream = lopdf::Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => crop_width,
            "Height" => crop_height,
            "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 8,
            "Filter" => "FlateDecode",
        },
        zlib_compress(&alpha)?,
    )
    .with_compression(false);
    let alpha_id = document.add_object(alpha_stream);
    let image_stream = lopdf::Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => crop_width,
            "Height" => crop_height,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "Filter" => "FlateDecode",
            "SMask" => alpha_id,
        },
        zlib_compress(&rgb)?,
    )
    .with_compression(false);
    let image_id = document.add_object(image_stream);

    inherit_resources_onto_page(document, page_id)?;
    document
        .add_xobject(page_id, name.to_vec(), image_id)
        .map_err(|error| error.to_string())?;

    let rect =
        page_rect(document, page_id).ok_or_else(|| "PDF page has no page box".to_string())?;
    let draw_width = crop_width as f32 / width as f32 * rect.width;
    let draw_height = crop_height as f32 / height as f32 * rect.height;
    let draw_x = rect.x + x0 as f32 / width as f32 * rect.width;
    let draw_y = rect.y + (height - y1) as f32 / height as f32 * rect.height;
    let content = format!(
        "q\n{draw_width:.4} 0 0 {draw_height:.4} {draw_x:.4} {draw_y:.4} cm\n/{} Do\nQ\n",
        String::from_utf8_lossy(name)
    );
    document
        .add_page_contents(page_id, content.into_bytes())
        .map_err(|error| error.to_string())?;
    Ok(true)
}

fn pdf_real(value: f32) -> lopdf::Object {
    lopdf::Object::Real(value)
}

fn pdf_real_array(values: impl IntoIterator<Item = f32>) -> lopdf::Object {
    lopdf::Object::Array(values.into_iter().map(pdf_real).collect())
}

fn gradient_function_object(gradient: &crate::core::vector::style::Gradient) -> lopdf::Object {
    let color = |value: crate::core::vector::color::ColorValue| {
        let [r, g, b, _] = value.to_rgba8();
        [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0]
    };
    let mut stops: Vec<(f32, [f32; 3])> = Vec::new();
    for stop in gradient.active_stops() {
        let offset = stop.offset.clamp(0.0, 1.0);
        if stops
            .last()
            .is_some_and(|(previous, _)| (offset - *previous).abs() < 1e-6)
        {
            if let Some(last) = stops.last_mut() {
                last.1 = color(stop.color);
            }
        } else {
            stops.push((offset, color(stop.color)));
        }
    }
    if stops.is_empty() {
        stops.push((0.0, [0.0; 3]));
    }
    if stops[0].0 > 0.0 {
        stops.insert(0, (0.0, stops[0].1));
    }
    if stops.last().is_some_and(|(offset, _)| *offset < 1.0) {
        let last = stops.last().expect("checked").1;
        stops.push((1.0, last));
    }
    if stops.len() == 1 {
        stops.push((1.0, stops[0].1));
    }
    let type2 = |a: [f32; 3], b: [f32; 3]| {
        lopdf::Object::Dictionary(dictionary! {
            "FunctionType" => 2,
            "Domain" => pdf_real_array([0.0, 1.0]),
            "C0" => pdf_real_array(a),
            "C1" => pdf_real_array(b),
            "N" => pdf_real(1.0),
        })
    };
    if stops.len() == 2 {
        return type2(stops[0].1, stops[1].1);
    }
    let functions = lopdf::Object::Array(
        stops
            .windows(2)
            .map(|pair| type2(pair[0].1, pair[1].1))
            .collect(),
    );
    let bounds = pdf_real_array(stops[1..stops.len() - 1].iter().map(|(offset, _)| *offset));
    let encode = pdf_real_array((0..stops.len() - 1).flat_map(|_| [0.0, 1.0]));
    lopdf::Object::Dictionary(dictionary! {
        "FunctionType" => 3,
        "Domain" => pdf_real_array([0.0, 1.0]),
        "Functions" => functions,
        "Bounds" => bounds,
        "Encode" => encode,
    })
}

fn add_vector_shading_resources(
    document: &mut lopdf::Document,
    page_id: lopdf::ObjectId,
    objects: &[crate::core::print::PdfVectorObject],
) -> Result<(), String> {
    use crate::core::vector::style::GradientKind;
    let mut additions = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        let Some(gradient) = object.fill_gradient else {
            continue;
        };
        let (kind, coords) = match gradient.kind {
            GradientKind::Linear => (2, pdf_real_array([0.0, 0.0, 1.0, 0.0])),
            GradientKind::Radial => (3, pdf_real_array([0.0, 0.0, 0.0, 0.0, 0.0, 1.0])),
        };
        additions.push((
            format!("IaiSh{index}"),
            lopdf::Object::Dictionary(dictionary! {
                "ShadingType" => kind,
                "ColorSpace" => "DeviceRGB",
                "Coords" => coords,
                "Function" => gradient_function_object(&gradient),
                "Extend" => lopdf::Object::Array(vec![
                    lopdf::Object::Boolean(true),
                    lopdf::Object::Boolean(true),
                ]),
            }),
        ));
    }
    if additions.is_empty() {
        return Ok(());
    }
    let page = document
        .get_dictionary_mut(page_id)
        .map_err(|error| error.to_string())?;
    let resources = page
        .get_mut(b"Resources")
        .map_err(|error| error.to_string())?
        .as_dict_mut()
        .map_err(|error| error.to_string())?;
    let mut shadings = resources
        .get(b"Shading")
        .ok()
        .and_then(|object| object.as_dict().ok())
        .cloned()
        .unwrap_or_default();
    for (name, shading) in additions {
        shadings.set(name, shading);
    }
    resources.set("Shading", shadings);
    Ok(())
}

fn add_vector_overlay(
    document: &mut lopdf::Document,
    page_id: lopdf::ObjectId,
    objects: &[crate::core::print::PdfVectorObject],
    width: u32,
    height: u32,
    dpi: f32,
) -> Result<bool, String> {
    if objects.is_empty() {
        return Ok(false);
    }
    if !page_accepts_overlay(document, page_id, width, height, dpi) {
        return Err("PDF page geometry is not safe for a vector overlay".to_string());
    }
    let rect =
        page_rect(document, page_id).ok_or_else(|| "PDF page has no page box".to_string())?;
    inherit_resources_onto_page(document, page_id)?;
    add_vector_shading_resources(document, page_id, objects)?;
    let mut content = String::new();
    crate::core::print::append_vector_content(
        &mut content,
        objects,
        width,
        height,
        rect.width,
        rect.height,
        rect.x,
        rect.y,
        None,
    );
    if content.is_empty() {
        return Ok(false);
    }
    document
        .add_page_contents(page_id, content.into_bytes())
        .map_err(|error| error.to_string())?;
    Ok(true)
}

/// Paint one normalized top-left rectangle over a PDF page. This is vector PDF
/// content (a few bytes per page), so a 1,000-page scan stays compact and no
/// source page needs to be rasterized. `/Rotate` is inverted so "top-left" in
/// the editor remains top-left on rotated source pages.
fn add_global_clear_rect(
    document: &mut lopdf::Document,
    page_id: lopdf::ObjectId,
    clear: &crate::core::document::PdfGlobalClear,
) -> Result<bool, String> {
    let rect =
        page_rect(document, page_id).ok_or_else(|| "PDF page has no page box".to_string())?;
    let (sx0, sy0, sx1, sy1) = (
        clear.x0.clamp(0.0, 1.0),
        clear.y0.clamp(0.0, 1.0),
        clear.x1.clamp(0.0, 1.0),
        clear.y1.clamp(0.0, 1.0),
    );
    if sx1 <= sx0 || sy1 <= sy0 {
        return Ok(false);
    }
    let (px0, py0, px1, py1) = match page_rotation(document, page_id) {
        90 => (sy0, sx0, sy1, sx1),
        180 => (1.0 - sx1, sy0, 1.0 - sx0, sy1),
        270 => (1.0 - sy1, 1.0 - sx1, 1.0 - sy0, 1.0 - sx0),
        _ => (sx0, 1.0 - sy1, sx1, 1.0 - sy0),
    };
    let x = rect.x + px0 * rect.width;
    let y = rect.y + py0 * rect.height;
    let width = (px1 - px0) * rect.width;
    let height = (py1 - py0) * rect.height;
    if width <= 0.0 || height <= 0.0 {
        return Ok(false);
    }
    let [r, g, b, _] = clear.color;
    let content = format!(
        "q\n{:.6} {:.6} {:.6} rg\n{x:.4} {y:.4} {width:.4} {height:.4} re f\nQ\n",
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
    );
    document
        .add_page_contents(page_id, content.into_bytes())
        .map_err(|error| error.to_string())?;
    Ok(true)
}

fn replace_page_with_raster(
    document: &mut lopdf::Document,
    page_id: lopdf::ObjectId,
    name: &[u8],
    rgba: &[u8],
    width: u32,
    height: u32,
    dpi: f32,
) -> Result<(), String> {
    let dpi = dpi.max(1.0);
    let page_width = width as f32 / dpi * 72.0;
    let page_height = height as f32 / dpi * 72.0;
    let rgb = flatten_white(rgba, width, height)?;
    let image_stream = lopdf::Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => width,
            "Height" => height,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "Filter" => "FlateDecode",
        },
        zlib_compress(&rgb)?,
    )
    .with_compression(false);
    let image_id = document.add_object(image_stream);
    let content = format!(
        "q\n{page_width:.4} 0 0 {page_height:.4} 0 0 cm\n/{} Do\nQ\n",
        String::from_utf8_lossy(name)
    );
    let content_id = document.add_object(lopdf::Stream::new(
        lopdf::Dictionary::new(),
        content.into_bytes(),
    ));

    let old_rect = page_rect(document, page_id);
    let old_rotation = page_rotation(document, page_id);
    let page = document
        .get_dictionary_mut(page_id)
        .map_err(|error| error.to_string())?;
    page.set("Contents", content_id);
    page.set(
        "Resources",
        dictionary! {
            "XObject" => dictionary! {
                name.to_vec() => image_id,
            },
        },
    );
    let page_box = vec![0.into(), 0.into(), page_width.into(), page_height.into()];
    page.set("MediaBox", page_box.clone());
    page.set("CropBox", page_box);
    page.set("Rotate", 0);
    page.set("UserUnit", 1);
    for key in [b"BleedBox".as_slice(), b"TrimBox", b"ArtBox"] {
        page.remove(key);
    }
    let geometry_changed = old_rotation != 0
        || old_rect.is_none_or(|rect| {
            (rect.width - page_width).abs() > 1.0 || (rect.height - page_height).abs() > 1.0
        });
    if geometry_changed {
        page.remove(b"Annots");
    }
    Ok(())
}

fn root_pages_id(document: &lopdf::Document) -> Result<lopdf::ObjectId, String> {
    let catalog_id = document
        .trailer
        .get(b"Root")
        .and_then(lopdf::Object::as_reference)
        .map_err(|error| error.to_string())?;
    document
        .get_dictionary(catalog_id)
        .and_then(|catalog| catalog.get(b"Pages"))
        .and_then(lopdf::Object::as_reference)
        .map_err(|error| error.to_string())
}

fn flatten_page_tree(
    document: &mut lopdf::Document,
    pages_root: lopdf::ObjectId,
    page_ids: &[lopdf::ObjectId],
) -> Result<(), String> {
    // A page may inherit these values from an intermediate /Pages node. Copy
    // them onto the page before reparenting every selected page directly under
    // the root, otherwise a valid source PDF can lose its box or resources.
    for &page_id in page_ids {
        let inherited: Vec<(&[u8], lopdf::Object)> = [
            b"Resources".as_slice(),
            b"MediaBox",
            b"CropBox",
            b"Rotate",
            b"UserUnit",
            b"BleedBox",
            b"TrimBox",
            b"ArtBox",
        ]
        .into_iter()
        .filter_map(|key| inherited_page_object(document, page_id, key).map(|value| (key, value)))
        .collect();
        let page = document
            .get_dictionary_mut(page_id)
            .map_err(|error| error.to_string())?;
        for (key, value) in inherited {
            if !page.has(key) {
                page.set(key, value);
            }
        }
        page.set("Parent", pages_root);
    }

    let root = document
        .get_dictionary_mut(pages_root)
        .map_err(|error| error.to_string())?;
    root.set(
        "Kids",
        page_ids
            .iter()
            .copied()
            .map(lopdf::Object::Reference)
            .collect::<Vec<_>>(),
    );
    root.set("Count", page_ids.len() as i64);
    Ok(())
}

pub fn build_hybrid_pdf(path: &Path, pages: &[HybridPage]) -> Result<HybridPdf, String> {
    build_hybrid_pdf_with_global_clears(path, pages, &[])
}

pub fn build_hybrid_pdf_with_global_clears(
    path: &Path,
    pages: &[HybridPage],
    global_clears: &[crate::core::document::PdfGlobalClear],
) -> Result<HybridPdf, String> {
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    let mut document = lopdf::Document::load_mem(&bytes)
        .map_err(|error| format!("Could not parse PDF structure: {error}"))?;
    if document.is_encrypted() {
        return Err("Hybrid export does not support encrypted PDFs".to_string());
    }
    let source_pages = document.get_pages();
    if pages.is_empty() {
        return Err("Hybrid export needs at least one page".to_string());
    }
    let mut unique_pages = std::collections::HashSet::new();
    if pages
        .iter()
        .any(|page| !unique_pages.insert(page.source_index))
    {
        return Err("Hybrid export pages must be unique".to_string());
    }
    let pages_root = root_pages_id(&document)?;

    let mut vector_pages = 0;
    let mut overlay_pages = 0;
    let mut raster_pages = 0;
    let mut ordered_page_ids = Vec::with_capacity(pages.len());
    for (position, page) in pages.iter().enumerate() {
        let source_page_id = source_pages.get(&(page.source_index as u32 + 1)).copied();
        let page_id = if let Some(page_id) = source_page_id {
            page_id
        } else if matches!(page.content, HybridPageContent::Raster { .. }) {
            let content_id =
                document.add_object(lopdf::Stream::new(lopdf::Dictionary::new(), Vec::new()));
            document.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_root,
                "MediaBox" => vec![0.into(), 0.into(), 1.into(), 1.into()],
                "Resources" => lopdf::Dictionary::new(),
                "Contents" => content_id,
            })
        } else {
            return Err(format!(
                "Inserted page {} must carry raster content",
                position + 1
            ));
        };
        ordered_page_ids.push(page_id);
        let name = format!("IAIPage{}", position + 1).into_bytes();
        let mut page_has_overlay = false;
        let page_is_raster = matches!(page.content, HybridPageContent::Raster { .. });
        match &page.content {
            HybridPageContent::Original => {}
            HybridPageContent::Overlay {
                rgba,
                vectors,
                width,
                height,
                dpi,
            } => {
                let raster_added =
                    add_overlay(&mut document, page_id, &name, rgba, *width, *height, *dpi)?;
                let vector_added =
                    add_vector_overlay(&mut document, page_id, vectors, *width, *height, *dpi)?;
                page_has_overlay |= raster_added || vector_added;
            }
            HybridPageContent::Raster {
                rgba,
                width,
                height,
                dpi,
            } => {
                replace_page_with_raster(
                    &mut document,
                    page_id,
                    &name,
                    rgba,
                    *width,
                    *height,
                    *dpi,
                )?;
            }
        }
        for clear in global_clears {
            page_has_overlay |= add_global_clear_rect(&mut document, page_id, clear)?;
        }
        if page_is_raster {
            raster_pages += 1;
        } else if page_has_overlay {
            overlay_pages += 1;
        } else {
            vector_pages += 1;
        }
    }

    // Flattening lets the visible document order differ from the physical
    // source order and also lets newly created raster pages sit anywhere.
    flatten_page_tree(&mut document, pages_root, &ordered_page_ids)?;
    document.prune_objects();

    document.change_producer("iAi Hybrid PDF");
    document.compress();
    let mut output = Vec::new();
    document
        .save_to(&mut output)
        .map_err(|error| format!("Could not write hybrid PDF: {error}"))?;
    Ok(HybridPdf {
        bytes: output,
        vector_pages,
        overlay_pages,
        raster_pages,
    })
}

/// PDF export. Single page, sized from the canvas pixel dimensions and document
/// DPI. Cross-platform; the PDF is built in pure Rust.
pub struct PdfExporter;

impl Exporter for PdfExporter {
    fn extensions(&self) -> &[&str] {
        &["pdf"]
    }

    fn export(&self, canvas: &Canvas, path: &Path, opts: &ExportOptions) -> Result<(), String> {
        if Canvas::pixel_count(canvas.width, canvas.height).is_none_or(|p| p > MAX_PDF_PAGE_PIXELS)
        {
            return Err("PDF canvas is too large to export safely".to_string());
        }
        // Press-ready marks (bleed + crop/registration) when requested in the
        // Export dialog; a plain export keeps `PrintLayout::default()`.
        let layout = crate::core::print::export_pdf_layout(
            opts.pdf_marks,
            canvas.width,
            canvas.height,
            canvas.metadata.resolution_ppi,
        );
        if canvas.is_cmyk() {
            // Promote qualifying Path/Shape/Text layers to native DeviceCMYK vector
            // paths over an ink raster base — crisp at any size (no "răng cưa"),
            // colour-managed via the embedded CMYK profile so the PDF matches the
            // app's preview. Mirrors the RGB path below.
            let selection = crate::core::print::collect_pdf_vectors(canvas);
            if let Some(ink) = crate::core::print::pdf_ink_base(canvas, &selection) {
                let page = crate::core::print::encode_pdf_page_cmyk(
                    &ink,
                    canvas.width,
                    canvas.height,
                    canvas.metadata.resolution_ppi,
                )?;
                let profile = canvas.cmyk_pdf_profile();
                let pdf = crate::core::print::build_pdf_encoded_with_vectors(
                    &page,
                    &selection.objects,
                    &layout,
                    profile.as_deref(),
                )?;
                return std::fs::write(path, &pdf).map_err(|e| e.to_string());
            }
            // Not ink-exact (group / adjustment / mask / ink-less pixel): fall back
            // to a colour-managed RGB raster of the on-screen mirror (no vectors, so
            // no CMYK operators leak onto an RGB page). Still matches the preview.
            let rgba = canvas.export_flat();
            let icc = super::export_icc_bytes(canvas);
            let pdf = crate::core::print::build_pdf_with_vectors(
                &rgba,
                canvas.width,
                canvas.height,
                canvas.metadata.resolution_ppi,
                &[],
                &layout,
                Some(&icc),
            )?;
            return std::fs::write(path, &pdf).map_err(|e| e.to_string());
        }
        // `_layered` keeps a Path/Text run crisp even when opaque image layers
        // sit above it: those come back in `above_layer_ids` and are re-drawn as
        // a transparent overlay ON TOP of the native vectors (correct z-order),
        // instead of flattening the vectors into the raster (which went jagged).
        let selection = crate::core::print::collect_pdf_vectors_layered(canvas);
        let rgba = crate::core::print::pdf_raster_base(canvas, &selection);
        let overlay = crate::core::print::pdf_overlay_raster(canvas, &selection);
        let icc = super::export_icc_bytes(canvas);
        let pdf = crate::core::print::build_pdf_with_vectors_overlay(
            &rgba,
            canvas.width,
            canvas.height,
            canvas.metadata.resolution_ppi,
            &selection.objects,
            overlay,
            &layout,
            Some(&icc),
        )?;
        std::fs::write(path, &pdf).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_pdf_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "iai-{name}-{}-{}.pdf",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn generic_pdf_exporter_writes_native_path_content() {
        use crate::core::command_vector::apply_object_to_layer;
        use crate::core::geometry::Point;
        use crate::core::vector::affine::AffineTransform;
        use crate::core::vector::color::ColorValue;
        use crate::core::vector::object::VectorObjectData;
        use crate::core::vector::path::{Contour, FillRule, Node, PathData};
        use crate::core::vector::style::VectorStyle;

        let mut canvas = Canvas::from_rgba(vec![255; 8 * 8 * 4], 8, 8);
        let index = canvas.add_layer();
        apply_object_to_layer(
            &mut canvas.layer_stack.layers[index],
            VectorObjectData::new(
                PathData::new(
                    vec![Contour::new(
                        vec![
                            Node::sharp(Point::new(1.0, 1.0)),
                            Node::sharp(Point::new(7.0, 1.0)),
                            Node::sharp(Point::new(4.0, 7.0)),
                        ],
                        true,
                    )],
                    FillRule::NonZero,
                ),
                VectorStyle::filled(ColorValue::rgb(0.0, 0.0, 1.0)),
                AffineTransform::IDENTITY,
            ),
        );
        let output = temp_pdf_path("generic-native-vector");
        PdfExporter
            .export(&canvas, &output, &ExportOptions::default())
            .expect("export");
        let bytes = std::fs::read(&output).expect("read pdf");
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains(" m\n") && text.contains("f\nQ"));
        let _ = std::fs::remove_file(output);
    }

    #[test]
    fn export_keeps_vectors_crisp_under_pixel_layers() {
        // A vector layer with opaque pixel (image) layers stacked ABOVE it must
        // still export as crisp native paths, with the images redrawn as a
        // transparent overlay ON TOP — not flattened into the raster (which is
        // what made the vectors go jagged when a couple of image layers sat over
        // them). File ▸ Export ▸ PDF path.
        use crate::core::command_vector::apply_object_to_layer;
        use crate::core::geometry::Point;
        use crate::core::print::{
            collect_pdf_vectors, collect_pdf_vectors_layered, pdf_overlay_raster,
        };
        use crate::core::vector::affine::AffineTransform;
        use crate::core::vector::color::ColorValue;
        use crate::core::vector::object::VectorObjectData;
        use crate::core::vector::path::{Contour, FillRule, Node, PathData};
        use crate::core::vector::style::VectorStyle;

        let mut canvas = Canvas::from_rgba(vec![255; 16 * 16 * 4], 16, 16); // raster "photo"
        let vec_idx = canvas.add_layer();
        apply_object_to_layer(
            &mut canvas.layer_stack.layers[vec_idx],
            VectorObjectData::new(
                PathData::new(
                    vec![Contour::new(
                        vec![
                            Node::sharp(Point::new(2.0, 2.0)),
                            Node::sharp(Point::new(14.0, 2.0)),
                            Node::sharp(Point::new(8.0, 14.0)),
                        ],
                        true,
                    )],
                    FillRule::NonZero,
                ),
                VectorStyle::filled(ColorValue::rgb(0.0, 0.0, 1.0)),
                AffineTransform::IDENTITY,
            ),
        );
        let img_idx = canvas.add_layer(); // an opaque pixel layer ON TOP of the vector
        let above_id = canvas.layer_stack.layers[img_idx].id;

        // The conservative collector stops at the image on top and promotes
        // nothing; the layered one promotes the vector and holds the image aside.
        assert!(
            collect_pdf_vectors(&canvas).objects.is_empty(),
            "plain collector should not promote a vector buried under a pixel layer"
        );
        let layered = collect_pdf_vectors_layered(&canvas);
        assert!(
            !layered.objects.is_empty(),
            "layered collector must keep the vector native despite the image above it"
        );
        assert_eq!(layered.above_layer_ids, vec![above_id]);
        assert!(pdf_overlay_raster(&canvas, &layered).is_some());

        let output = temp_pdf_path("crisp-under-pixels");
        PdfExporter
            .export(&canvas, &output, &ExportOptions::default())
            .expect("export");
        let bytes = std::fs::read(&output).expect("read pdf");
        let text = String::from_utf8_lossy(&bytes);
        // Native vector path ops AND the transparent overlay image both present.
        assert!(
            text.contains(" m\n") && text.contains("f\nQ"),
            "no native vector path"
        );
        assert!(text.contains("/Im1"), "overlay image XObject not emitted");
        assert!(text.contains("/SMask"), "overlay alpha (SMask) not emitted");
        let _ = std::fs::remove_file(output);
    }

    #[test]
    fn cmyk_pdf_exporter_writes_native_vectors_not_flat_raster() {
        // File ▸ Export ▸ PDF on a CMYK document must keep vectors crisp (native
        // DeviceCMYK paths over an ink base), colour-managed via the embedded
        // profile — not flatten the whole page to a raster (which looked jagged).
        use crate::core::canvas::CmykProfile;
        use crate::core::command_vector::apply_object_to_layer;
        use crate::core::geometry::Point;
        use crate::core::vector::affine::AffineTransform;
        use crate::core::vector::color::ColorValue;
        use crate::core::vector::object::VectorObjectData;
        use crate::core::vector::path::{Contour, FillRule, Node, PathData};
        use crate::core::vector::style::VectorStyle;

        // Convert to CMYK first, then add the vector (conversion merges prior
        // layers), matching how a CMYK design is authored.
        let mut canvas = Canvas::from_rgba(vec![255; 16 * 16 * 4], 16, 16);
        canvas
            .convert_to_cmyk(CmykProfile::Naive)
            .expect("convert to CMYK");
        let index = canvas.add_layer();
        apply_object_to_layer(
            &mut canvas.layer_stack.layers[index],
            VectorObjectData::new(
                PathData::new(
                    vec![Contour::new(
                        vec![
                            Node::sharp(Point::new(2.0, 2.0)),
                            Node::sharp(Point::new(14.0, 2.0)),
                            Node::sharp(Point::new(8.0, 14.0)),
                        ],
                        true,
                    )],
                    FillRule::NonZero,
                ),
                VectorStyle::filled(ColorValue::cmyk(0.0, 1.0, 1.0, 0.0)),
                AffineTransform::IDENTITY,
            ),
        );

        let output = temp_pdf_path("cmyk-native-vector");
        PdfExporter
            .export(&canvas, &output, &ExportOptions::default())
            .expect("export");
        let bytes = std::fs::read(&output).expect("read pdf");
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains(" m\n"),
            "crisp native vector path present (not a flattened raster)"
        );
        assert!(
            text.contains("[/ICCBased"),
            "ink base is tagged with the embedded CMYK profile"
        );
        assert!(
            text.contains("/OutputIntents"),
            "page declares the CMYK output intent"
        );
        // The vector paints through the embedded CMYK profile (/IaiCmyk cs … scn),
        // not raw `k`, so it matches the ICC-tagged raster and the app's preview.
        assert!(
            text.contains("/IaiCmyk") && text.contains(" scn\n"),
            "vector fill is colour-managed through the embedded CMYK space"
        );
        assert!(
            !text.contains(" k\n"),
            "no raw DeviceCMYK fill operator (would be the viewer's default colour)"
        );
        let _ = std::fs::remove_file(output);
    }

    #[test]
    fn hybrid_pdf_keeps_new_path_edit_as_native_vector() {
        use crate::core::geometry::Point;
        use crate::core::print::PdfVectorObject;
        use crate::core::vector::affine::AffineTransform;
        use crate::core::vector::color::ColorValue;
        use crate::core::vector::path::{Contour, FillRule, Node, PathData};
        use crate::core::vector::style::{Gradient, GradientKind};

        let source = temp_pdf_path("hybrid-vector-source");
        std::fs::write(&source, minimal_pdf(&[(40, 40)])).expect("write source");
        let vector = PdfVectorObject {
            path: PathData::new(
                vec![Contour::new(
                    vec![
                        Node::sharp(Point::new(4.0, 4.0)),
                        Node::sharp(Point::new(36.0, 4.0)),
                        Node::sharp(Point::new(20.0, 36.0)),
                    ],
                    true,
                )],
                FillRule::NonZero,
            ),
            fill: Some(crate::core::print::PdfPaintColor::Rgb([1.0, 0.0, 0.0])),
            fill_gradient: None,
            stroke: None,
            stroke_width_px: 0.0,
            stroke_cap: crate::core::vector::style::LineCap::Butt,
            stroke_join: crate::core::vector::style::LineJoin::Miter,
            stroke_miter_limit: 4.0,
            stroke_dash: Vec::new(),
            stroke_dash_offset: 0.0,
            even_odd: false,
            fill_overprint: false,
            stroke_overprint: false,
            fill_alpha: 1.0,
            stroke_alpha: 1.0,
        };
        let mut gradient_vector = vector.clone();
        gradient_vector.fill = None;
        gradient_vector.fill_gradient = Some(Gradient::two_color(
            GradientKind::Linear,
            ColorValue::BLACK,
            ColorValue::WHITE,
            AffineTransform::translate(4.0, 4.0).then(&AffineTransform::scale(32.0, 32.0)),
        ));
        let hybrid = build_hybrid_pdf(
            &source,
            &[HybridPage {
                source_index: 0,
                content: HybridPageContent::Overlay {
                    rgba: vec![0; 40 * 40 * 4],
                    vectors: vec![vector, gradient_vector],
                    width: 40,
                    height: 40,
                    dpi: 72.0,
                },
            }],
        )
        .expect("hybrid export");
        let document = lopdf::Document::load_mem(&hybrid.bytes).expect("parse hybrid");
        let page_id = *document.get_pages().values().next().expect("page");
        let content = document.get_page_content(page_id).expect("page content");
        let text = String::from_utf8_lossy(&content);
        assert!(text.contains("1.0000 0.0000 0.0000 rg"));
        assert!(text.contains(" m\n") && text.contains("f\nQ"));
        assert!(text.contains("/IaiSh1 sh"));
        let page = document.get_dictionary(page_id).expect("page dictionary");
        let resources = page
            .get(b"Resources")
            .expect("resources")
            .as_dict()
            .expect("resource dictionary");
        let shadings = resources
            .get(b"Shading")
            .expect("shading resources")
            .as_dict()
            .expect("shading dictionary");
        assert!(shadings.has(b"IaiSh1"));
        let _ = std::fs::remove_file(source);
    }

    fn minimal_pdf(page_sizes: &[(u32, u32)]) -> Vec<u8> {
        let page_count = page_sizes.len();
        let first_content_id = 3 + page_count;
        let kids = (0..page_count)
            .map(|index| format!("{} 0 R", 3 + index))
            .collect::<Vec<_>>()
            .join(" ");
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            format!("<< /Type /Pages /Count {page_count} /Kids [{kids}] >>"),
        ];
        for (index, &(width, height)) in page_sizes.iter().enumerate() {
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] \
                 /Resources << >> /Contents {} 0 R >>",
                first_content_id + index
            ));
        }
        for _ in page_sizes {
            objects.push("<< /Length 0 >>\nstream\n\nendstream".to_string());
        }

        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for (index, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    #[test]
    fn imports_a_generated_pdf_page_at_auto_dpi() {
        let path = temp_pdf_path("pdf-import");
        let rgba = vec![255u8; 8 * 6 * 4];
        let pdf = crate::core::print::build_pdf(
            &rgba,
            8,
            6,
            72.0,
            &crate::core::print::PrintLayout::default(),
            None,
        )
        .unwrap();
        std::fs::write(&path, pdf).unwrap();

        let imported = PdfImporter.import_many(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(imported.len(), 1);
        // Auto caps at 300 DPI (scale 300/72); the tiny page is nowhere near the
        // pixel budget, so it renders at the full cap: ceil(8*300/72)=34, 6*300/72=25.
        assert_eq!(imported[0].metadata.resolution_ppi, PDF_AUTO_MAX_DPI);
        assert_eq!((imported[0].width, imported[0].height), (34, 25));
        assert_eq!(imported[0].layer_stack.layers.len(), 1);
        assert!(imported[0].layer_stack.layers[0].is_background);
    }

    #[test]
    fn imports_every_pdf_page() {
        let path = temp_pdf_path("pdf-pages");
        std::fs::write(&path, minimal_pdf(&[(20, 30), (40, 10)])).unwrap();

        let imported = PdfImporter.import_many(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        // import_many uses Auto (300 DPI, scale 300/72) for these tiny pages.
        assert_eq!(imported.len(), 2);
        assert_eq!((imported[0].width, imported[0].height), (84, 125));
        assert_eq!((imported[1].width, imported[1].height), (167, 42));
    }

    #[test]
    fn probe_reports_pages_and_dims() {
        let path = temp_pdf_path("pdf-probe");
        std::fs::write(&path, minimal_pdf(&[(20, 30), (40, 10), (15, 15)])).unwrap();

        let probe = PdfImporter::probe(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(probe.page_count, 3);
        assert_eq!(probe.page_dims.len(), 3);
        assert_eq!(probe.page_dims[0], (20.0, 30.0));
        assert_eq!(probe.page_dims[1], (40.0, 10.0));
        assert_eq!(probe.page_dims[2], (15.0, 15.0));
    }

    #[test]
    fn renders_only_selected_pages() {
        let path = temp_pdf_path("pdf-selected");
        std::fs::write(&path, minimal_pdf(&[(20, 30), (40, 10), (15, 15)])).unwrap();

        // Pick pages 1 and 3 (0-based 0 and 2); out-of-order + duplicate input is
        // normalized to ascending, unique order. target_dpi=72 => scale 1.0, so
        // pixel dims equal the point dims (clean, cap-independent assert).
        let rendered = PdfImporter::render_selected(&path, &[2, 0, 0], Some(72.0)).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(rendered.len(), 2);
        assert_eq!((rendered[0].width, rendered[0].height), (20, 30));
        assert_eq!((rendered[1].width, rendered[1].height), (15, 15));
    }

    #[test]
    fn render_selected_honors_target_dpi() {
        let path = temp_pdf_path("pdf-dpi");
        std::fs::write(&path, minimal_pdf(&[(20, 30)])).unwrap();

        let at150 = PdfImporter::render_selected(&path, &[0], Some(150.0)).unwrap();
        let at600 = PdfImporter::render_selected(&path, &[0], Some(600.0)).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(at150[0].metadata.resolution_ppi, 150.0);
        assert_eq!(at600[0].metadata.resolution_ppi, 600.0);
        // Higher DPI => more pixels: 20*600/72=166.67->167 vs 20*150/72=41.67->42.
        assert_eq!(at150[0].width, 42);
        assert_eq!(at600[0].width, 167);
    }

    #[test]
    fn render_selected_rejects_empty_and_out_of_range() {
        let path = temp_pdf_path("pdf-bad-select");
        std::fs::write(&path, minimal_pdf(&[(20, 30), (40, 10)])).unwrap();

        assert!(PdfImporter::render_selected(&path, &[], None).is_err());
        assert!(PdfImporter::render_selected(&path, &[5], None).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn imports_more_than_sixty_four_small_pages() {
        let path = temp_pdf_path("pdf-many-pages");
        let page_sizes = vec![(10, 10); 65];
        std::fs::write(&path, minimal_pdf(&page_sizes)).unwrap();

        let imported = PdfImporter.import_many(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        // Auto 300 DPI, well within budget: ceil(10*300/72) = 42.
        assert_eq!(imported.len(), 65);
        assert!(imported
            .iter()
            .all(|canvas| (canvas.width, canvas.height) == (42, 42)));
    }

    #[test]
    fn probes_and_lazily_renders_a_thousand_page_document() {
        let path = temp_pdf_path("pdf-thousand-pages");
        std::fs::write(&path, minimal_pdf(&vec![(10, 10); 1_000])).unwrap();

        let probe = PdfImporter::probe(&path).unwrap();
        assert_eq!(probe.page_count, 1_000);

        let service = PdfRenderService::start(path.clone());
        let document_id = crate::core::document::DocumentId(77);
        service.request(document_id, 999, 72.0).unwrap();
        let last = service
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        assert_eq!(last.page_index, 999);
        assert_eq!(
            (
                last.canvas.as_ref().unwrap().width,
                last.canvas.as_ref().unwrap().height
            ),
            (10, 10)
        );

        service.request(document_id, 0, 72.0).unwrap();
        let first = service
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        assert_eq!(first.page_index, 0);
        assert_eq!(
            (
                first.canvas.as_ref().unwrap().width,
                first.canvas.as_ref().unwrap().height
            ),
            (10, 10)
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn selected_page_dpi_does_not_depend_on_document_page_count() {
        let path = temp_pdf_path("pdf-many-pages-one-selected");
        std::fs::write(&path, minimal_pdf(&vec![(400, 400); 63])).unwrap();

        let rendered = PdfImporter::render_selected(&path, &[62], Some(300.0)).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(rendered.len(), 1);
        assert_eq!(rendered[0].metadata.resolution_ppi, 300.0);
        assert_eq!((rendered[0].width, rendered[0].height), (1667, 1667));
    }

    #[test]
    fn oversized_batch_is_rejected_instead_of_silently_lowering_dpi() {
        let path = temp_pdf_path("pdf-large-batch");
        std::fs::write(&path, minimal_pdf(&[(596, 843), (596, 843)])).unwrap();

        let error = match PdfImporter::render_selected(&path, &[0, 1], Some(600.0)) {
            Ok(_) => panic!("oversized batch should be rejected"),
            Err(error) => error,
        };
        let _ = std::fs::remove_file(&path);

        assert!(error.contains("render fewer pages at a time"));
    }

    #[test]
    fn hybrid_export_keeps_clean_page_and_appends_transparent_overlay() {
        let source_path = temp_pdf_path("pdf-hybrid-source");
        let output_path = temp_pdf_path("pdf-hybrid-output");
        std::fs::write(&source_path, minimal_pdf(&[(20, 30), (40, 10)])).unwrap();

        // Exercise inherited resources: appending an XObject must not shadow
        // fonts/resources inherited from the Pages node.
        let mut source = lopdf::Document::load(&source_path).unwrap();
        let second_page_id = source.get_pages()[&2];
        let parent_id = source
            .get_dictionary(second_page_id)
            .unwrap()
            .get(b"Parent")
            .unwrap()
            .as_reference()
            .unwrap();
        source
            .get_dictionary_mut(second_page_id)
            .unwrap()
            .remove(b"Resources");
        source
            .get_dictionary_mut(parent_id)
            .unwrap()
            .set("Resources", dictionary! { "KeepMe" => 42 });
        source.save(&source_path).unwrap();

        let source = lopdf::Document::load(&source_path).unwrap();
        let source_page_id = source.get_pages()[&1];
        let original_contents = source
            .get_dictionary(source_page_id)
            .unwrap()
            .get(b"Contents")
            .unwrap()
            .clone();

        let mut overlay = vec![0u8; 40 * 10 * 4];
        let pixel = (3 * 40 + 4) * 4;
        overlay[pixel..pixel + 4].copy_from_slice(&[255, 0, 0, 255]);
        let hybrid = build_hybrid_pdf(
            &source_path,
            &[
                HybridPage {
                    source_index: 0,
                    content: HybridPageContent::Original,
                },
                HybridPage {
                    source_index: 1,
                    content: HybridPageContent::Overlay {
                        rgba: overlay,
                        vectors: Vec::new(),
                        width: 40,
                        height: 10,
                        dpi: 72.0,
                    },
                },
            ],
        )
        .unwrap();
        assert_eq!(
            (
                hybrid.vector_pages,
                hybrid.overlay_pages,
                hybrid.raster_pages
            ),
            (1, 1, 0)
        );
        std::fs::write(&output_path, &hybrid.bytes).unwrap();

        let output = lopdf::Document::load(&output_path).unwrap();
        let output_pages = output.get_pages();
        assert_eq!(output_pages.len(), 2);
        let clean_contents = output
            .get_dictionary(output_pages[&1])
            .unwrap()
            .get(b"Contents")
            .unwrap();
        assert!(clean_contents == &original_contents);
        assert_eq!(output.get_page_contents(output_pages[&2]).len(), 2);
        let overlay_resources = output
            .get_dictionary(output_pages[&2])
            .unwrap()
            .get(b"Resources")
            .unwrap()
            .as_dict()
            .unwrap();
        assert!(overlay_resources.has(b"KeepMe"));
        assert!(overlay_resources.has(b"XObject"));

        let rendered = PdfImporter::render_selected(&output_path, &[1], Some(72.0)).unwrap();
        let pixels = rendered[0].export_flat();
        let rendered_pixel = &pixels[pixel..pixel + 4];
        assert!(
            rendered_pixel[0] > 200 && rendered_pixel[1] < 80 && rendered_pixel[2] < 80,
            "overlay pixel moved or flipped: {rendered_pixel:?}"
        );

        let _ = std::fs::remove_file(source_path);
        let _ = std::fs::remove_file(output_path);
    }

    #[test]
    fn global_clear_rect_covers_same_normalized_corner_on_every_page() {
        let source_path = temp_pdf_path("pdf-global-clear-source");
        let output_path = temp_pdf_path("pdf-global-clear-output");
        std::fs::write(&source_path, minimal_pdf(&[(20, 20), (40, 20)])).unwrap();
        let pages = vec![
            HybridPage {
                source_index: 0,
                content: HybridPageContent::Original,
            },
            HybridPage {
                source_index: 1,
                content: HybridPageContent::Original,
            },
        ];
        let clear = crate::core::document::PdfGlobalClear {
            x0: 0.0,
            y0: 0.0,
            x1: 0.25,
            y1: 0.25,
            color: [230, 20, 30, 255],
        };
        let hybrid = build_hybrid_pdf_with_global_clears(&source_path, &pages, &[clear]).unwrap();
        assert_eq!(
            (
                hybrid.vector_pages,
                hybrid.overlay_pages,
                hybrid.raster_pages
            ),
            (0, 2, 0)
        );
        std::fs::write(&output_path, hybrid.bytes).unwrap();

        let rendered = PdfImporter::render_selected(&output_path, &[0, 1], Some(72.0)).unwrap();
        for canvas in &rendered {
            let pixels = canvas.export_flat();
            let top_left = &pixels[((canvas.width + 1) * 4) as usize..];
            assert!(
                top_left[0] > 180 && top_left[1] < 80 && top_left[2] < 80,
                "global cover missed top-left on {}x{} page: {:?}",
                canvas.width,
                canvas.height,
                &top_left[..4]
            );
            let center = ((canvas.height / 2 * canvas.width + canvas.width / 2) * 4) as usize;
            assert!(pixels[center..center + 3]
                .iter()
                .all(|&channel| channel > 220));
        }

        let _ = std::fs::remove_file(source_path);
        let _ = std::fs::remove_file(output_path);
    }

    #[test]
    fn hybrid_export_reorders_source_pages_and_inserts_a_raster_page() {
        let source_path = temp_pdf_path("pdf-insert-source");
        let output_path = temp_pdf_path("pdf-insert-output");
        std::fs::write(&source_path, minimal_pdf(&[(20, 30), (40, 50)])).unwrap();

        let inserted_rgba = [25, 180, 60, 255]
            .into_iter()
            .cycle()
            .take(10 * 12 * 4)
            .collect();
        let hybrid = build_hybrid_pdf(
            &source_path,
            &[
                HybridPage {
                    source_index: 1,
                    content: HybridPageContent::Original,
                },
                HybridPage {
                    source_index: 2,
                    content: HybridPageContent::Raster {
                        rgba: inserted_rgba,
                        width: 10,
                        height: 12,
                        dpi: 72.0,
                    },
                },
                HybridPage {
                    source_index: 0,
                    content: HybridPageContent::Original,
                },
            ],
        )
        .unwrap();
        assert_eq!(
            (
                hybrid.vector_pages,
                hybrid.overlay_pages,
                hybrid.raster_pages
            ),
            (2, 0, 1)
        );
        std::fs::write(&output_path, hybrid.bytes).unwrap();

        let probe = PdfImporter::probe(&output_path).unwrap();
        assert_eq!(probe.page_count, 3);
        assert_eq!(
            probe.page_dims,
            vec![(40.0, 50.0), (10.0, 12.0), (20.0, 30.0)]
        );
        let rendered = PdfImporter::render_selected(&output_path, &[1], Some(72.0)).unwrap();
        assert!(rendered[0].export_flat().chunks_exact(4).all(|pixel| {
            pixel[0] == 25 && pixel[1] == 180 && pixel[2] == 60 && pixel[3] == 255
        }));

        let _ = std::fs::remove_file(source_path);
        let _ = std::fs::remove_file(output_path);
    }

    #[test]
    fn hybrid_export_can_keep_only_selected_source_pages() {
        let source_path = temp_pdf_path("pdf-hybrid-subset-source");
        let output_path = temp_pdf_path("pdf-hybrid-subset-output");
        std::fs::write(&source_path, minimal_pdf(&[(20, 30), (40, 10), (50, 60)])).unwrap();

        let hybrid = build_hybrid_pdf(
            &source_path,
            &[
                HybridPage {
                    source_index: 0,
                    content: HybridPageContent::Original,
                },
                HybridPage {
                    source_index: 2,
                    content: HybridPageContent::Original,
                },
            ],
        )
        .unwrap();
        assert_eq!(hybrid.vector_pages, 2);
        std::fs::write(&output_path, hybrid.bytes).unwrap();

        let probe = PdfImporter::probe(&output_path).unwrap();
        assert_eq!(probe.page_count, 2);
        assert_eq!(probe.page_dims, vec![(20.0, 30.0), (50.0, 60.0)]);

        let _ = std::fs::remove_file(source_path);
        let _ = std::fs::remove_file(output_path);
    }

    #[test]
    fn hybrid_export_rasterizes_only_the_modified_page() {
        let source_path = temp_pdf_path("pdf-hybrid-raster-source");
        let output_path = temp_pdf_path("pdf-hybrid-raster-output");
        std::fs::write(&source_path, minimal_pdf(&[(20, 30), (40, 10)])).unwrap();
        let blue = [20, 40, 220, 255]
            .into_iter()
            .cycle()
            .take(40 * 10 * 4)
            .collect();

        let hybrid = build_hybrid_pdf(
            &source_path,
            &[
                HybridPage {
                    source_index: 0,
                    content: HybridPageContent::Original,
                },
                HybridPage {
                    source_index: 1,
                    content: HybridPageContent::Raster {
                        rgba: blue,
                        width: 40,
                        height: 10,
                        dpi: 72.0,
                    },
                },
            ],
        )
        .unwrap();
        assert_eq!(
            (
                hybrid.vector_pages,
                hybrid.overlay_pages,
                hybrid.raster_pages
            ),
            (1, 0, 1)
        );
        std::fs::write(&output_path, &hybrid.bytes).unwrap();

        let rendered = PdfImporter::render_selected(&output_path, &[1], Some(72.0)).unwrap();
        let pixels = rendered[0].export_flat();
        assert!(pixels
            .chunks_exact(4)
            .all(|pixel| pixel[2] > 180 && pixel[0] < 60 && pixel[1] < 80));

        let _ = std::fs::remove_file(source_path);
        let _ = std::fs::remove_file(output_path);
    }
}

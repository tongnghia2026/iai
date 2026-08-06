//! Printing (Phase C) — cross-platform.
//!
//! The page is laid out into a **print-ready PDF** (hand-written PDF 1.4, no
//! extra crates): the flattened canvas is composited onto white, losslessly
//! compressed and embedded as a `FlateDecode` image XObject.
//! That PDF is the universal artifact for File ▸ Export ▸ PDF. Reaching a
//! printer is OS-specific:
//!   * Windows  → direct GDI (`super::print_gdi`), which keeps the image at its
//!     exact physical size; the shell "print" verb (default PDF handler) stays
//!     as the fallback. Printers and their paper geometry come straight from
//!     winspool + driver DCs (same ground truth the GDI path prints with).
//!   * Linux/macOS → CUPS `lp` with the PDF.

use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrinterInfo {
    pub name: String,
    pub is_default: bool,
    pub paper_points: Option<(u32, u32)>,
    /// Printable area in PDF points `(x, y, w, h)`, with `(0, 0)` at the paper's
    /// bottom-left. Borderless driver presets should report an area close to the
    /// full paper size; normal presets report hardware margins.
    pub printable_rect_points: Option<(u32, u32, u32, u32)>,
}

/// ICC rendering intent for the colour-managed print path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderIntent {
    Perceptual,
    RelativeColorimetric,
    Saturation,
    AbsoluteColorimetric,
}

impl RenderIntent {
    pub fn all() -> &'static [RenderIntent] {
        &[
            RenderIntent::Perceptual,
            RenderIntent::RelativeColorimetric,
            RenderIntent::Saturation,
            RenderIntent::AbsoluteColorimetric,
        ]
    }
    pub fn label(self) -> &'static str {
        match self {
            RenderIntent::Perceptual => "Perceptual",
            RenderIntent::RelativeColorimetric => "Relative Colorimetric",
            RenderIntent::Saturation => "Saturation",
            RenderIntent::AbsoluteColorimetric => "Absolute Colorimetric",
        }
    }
    /// PDF `RenderingIntent` name (PDF spec spelling).
    pub fn pdf_name(self) -> &'static str {
        match self {
            RenderIntent::Perceptual => "Perceptual",
            RenderIntent::RelativeColorimetric => "RelativeColorimetric",
            RenderIntent::Saturation => "Saturation",
            RenderIntent::AbsoluteColorimetric => "AbsoluteColorimetric",
        }
    }
    pub fn to_lcms(self) -> lcms2::Intent {
        match self {
            RenderIntent::Perceptual => lcms2::Intent::Perceptual,
            RenderIntent::RelativeColorimetric => lcms2::Intent::RelativeColorimetric,
            RenderIntent::Saturation => lcms2::Intent::Saturation,
            RenderIntent::AbsoluteColorimetric => lcms2::Intent::AbsoluteColorimetric,
        }
    }
}

/// Press marks written around the artwork on a PDF page (File ▸ Export / Save
/// as PDF). The document canvas is the printed area (the **bleed box**); the
/// **trim box** is that rectangle inset by `bleed_mm` on every side — so a design
/// that already carries bleed is marked correctly without fabricating pixels.
/// Crop marks show the cut (trim), registration targets align the plates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PrintMarks {
    /// Distance the trim line sits inside the artwork edge, in millimetres.
    pub bleed_mm: f32,
    pub crop_marks: bool,
    pub registration_marks: bool,
}

impl Default for PrintMarks {
    fn default() -> Self {
        Self::none()
    }
}

impl PrintMarks {
    pub const fn none() -> Self {
        Self {
            bleed_mm: 0.0,
            crop_marks: false,
            registration_marks: false,
        }
    }

    /// Whether anything is drawn / any box overrides the plain page.
    pub fn is_active(&self) -> bool {
        self.crop_marks || self.registration_marks || self.bleed_mm > 0.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PrintLayout {
    pub page_points: Option<(f32, f32)>,
    pub printable_rect_points: Option<(f32, f32, f32, f32)>,
    pub margin_mm: f32,
    pub center: bool,
    /// ICC rendering intent for colour-managed printing (copies & printer device
    /// are tracked separately on the app — see `print_copies`/`print_selected_printer`).
    pub intent: RenderIntent,
    /// Crop / registration marks and bleed for a press-ready PDF.
    pub marks: PrintMarks,
}

impl Default for PrintLayout {
    fn default() -> Self {
        Self {
            page_points: None,
            printable_rect_points: None,
            margin_mm: 0.0,
            center: true,
            intent: RenderIntent::Perceptual,
            marks: PrintMarks::none(),
        }
    }
}

pub const A4_POINTS: (f32, f32) = (595.28, 841.89);

pub fn selected_printer_page_points(printers: &[PrinterInfo], selected_name: &str) -> (f32, f32) {
    printers
        .iter()
        .find(|p| p.name == selected_name)
        .or_else(|| printers.iter().find(|p| p.is_default))
        .or_else(|| printers.first())
        .and_then(|p| p.paper_points)
        .map(|(w, h)| (w as f32, h as f32))
        .unwrap_or(A4_POINTS)
}

pub fn selected_printer_printable_rect(
    printers: &[PrinterInfo],
    selected_name: &str,
) -> Option<(f32, f32, f32, f32)> {
    printers
        .iter()
        .find(|p| p.name == selected_name)
        .or_else(|| printers.iter().find(|p| p.is_default))
        .or_else(|| printers.first())
        .and_then(|p| p.printable_rect_points)
        .map(|(x, y, w, h)| (x as f32, y as f32, w as f32, h as f32))
}

pub fn layout_for_printer(
    mut layout: PrintLayout,
    printers: &[PrinterInfo],
    selected_name: &str,
) -> PrintLayout {
    layout.page_points = Some(selected_printer_page_points(printers, selected_name));
    layout.printable_rect_points = selected_printer_printable_rect(printers, selected_name);
    layout
}

/// Page size in PDF points, derived from the document pixel dimensions and DPI.
pub fn document_page_points(img_w: u32, img_h: u32, dpi: f32) -> (f32, f32) {
    let dpi = if dpi > 1.0 { dpi } else { 72.0 };
    let pw = img_w.max(1) as f32 / dpi * 72.0;
    let ph = img_h.max(1) as f32 / dpi * 72.0;
    (pw.max(1.0), ph.max(1.0))
}

/// Effective PDF page size. Export leaves `layout.page_points` empty so the page
/// matches the document; Print fills it from the selected printer paper.
pub fn page_points(layout: &PrintLayout, img_w: u32, img_h: u32, dpi: f32) -> (f32, f32) {
    layout
        .page_points
        .unwrap_or_else(|| document_page_points(img_w, img_h, dpi))
}

pub fn printable_area_points(
    layout: &PrintLayout,
    img_w: u32,
    img_h: u32,
    dpi: f32,
) -> (f32, f32, f32, f32) {
    let (pw, ph) = page_points(layout, img_w, img_h, dpi);
    let (x, y, w, h) = layout.printable_rect_points.unwrap_or((0.0, 0.0, pw, ph));
    let margin = (layout.margin_mm * 72.0 / 25.4).max(0.0);
    (
        x + margin,
        y + margin,
        (w - 2.0 * margin).max(1.0),
        (h - 2.0 * margin).max(1.0),
    )
}

/// Placement of the image on the page, in points: `(draw_w, draw_h, x, y)` with
/// `(x, y)` the bottom-left corner (PDF origin is bottom-left). Shared by the PDF
/// writer and the dialog preview so what you see matches what prints.
pub fn placement(layout: &PrintLayout, img_w: u32, img_h: u32, dpi: f32) -> (f32, f32, f32, f32) {
    let (_pw, _ph) = page_points(layout, img_w, img_h, dpi);
    let (area_x, area_y, avail_w, avail_h) = printable_area_points(layout, img_w, img_h, dpi);

    let iw = img_w.max(1) as f32;
    let ih = img_h.max(1) as f32;
    let dpi = if dpi > 1.0 { dpi } else { 72.0 };
    let actual_w = iw / dpi * 72.0;
    let actual_h = ih / dpi * 72.0;

    let (dw, dh) = (actual_w, actual_h);

    let (x, y) = if layout.center {
        (area_x + (avail_w - dw) * 0.5, area_y + (avail_h - dh) * 0.5)
    } else {
        // Top-left of the printable area.
        (area_x, area_y + avail_h - dh)
    };
    (dw, dh, x, y)
}

/// Composite straight-alpha RGBA8 onto white and return packed RGB8 (print has no
/// transparency — paper is white).
fn flatten_onto_white(rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    let n = (w as usize) * (h as usize);
    let mut rgb = vec![0u8; n * 3];
    for i in 0..n {
        let a = rgba[i * 4 + 3] as u16;
        let inv = 255 - a;
        rgb[i * 3] = ((rgba[i * 4] as u16 * a + 255 * inv) / 255) as u8;
        rgb[i * 3 + 1] = ((rgba[i * 4 + 1] as u16 * a + 255 * inv) / 255) as u8;
        rgb[i * 3 + 2] = ((rgba[i * 4 + 2] as u16 * a + 255 * inv) / 255) as u8;
    }
    rgb
}

fn compress_rgb_lossless(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    let rgb = flatten_onto_white(rgba, w, h);
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(&rgb)
        .map_err(|e| format!("PDF image compression error: {e}"))?;
    encoder
        .finish()
        .map_err(|e| format!("PDF image compression error: {e}"))
}

/// Build a single-page print-ready PDF from a flattened RGBA8 canvas. The image is
/// embedded with lossless zlib compression (`FlateDecode`). When `icc` is given, the image
/// is tagged with that ICC profile (ICCBased colour space, 3-component) and the
/// page carries the layout's rendering intent — so a colour-managed print pipeline
/// reproduces the document accurately. Returns the PDF bytes.
pub fn build_pdf(
    rgba: &[u8],
    w: u32,
    h: u32,
    dpi: f32,
    layout: &PrintLayout,
    icc: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    build_pdf_with_vectors(rgba, w, h, dpi, &[], layout, icc)
}

/// Like [`build_pdf`], but overlays crisp vector paths (see [`PdfVectorObject`])
/// over the flattened RGB image, so a Path layer prints resolution-independent
/// through the print/Save-as-PDF path just like the File ▸ Export path.
pub fn build_pdf_with_vectors(
    rgba: &[u8],
    w: u32,
    h: u32,
    dpi: f32,
    vectors: &[PdfVectorObject],
    layout: &PrintLayout,
    icc: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    if w == 0 || h == 0 {
        return Err("Empty image, cannot print".into());
    }
    let expected = (w as usize) * (h as usize) * 4;
    if rgba.len() < expected {
        return Err("Image buffer too small to print".into());
    }
    let page = EncodedPdfPage {
        compressed_rgb: compress_rgb_lossless(rgba, w, h)?,
        w,
        h,
        dpi,
        components: 3,
    };
    build_pdf_encoded_with_vectors(&page, vectors, layout, icc)
}

/// Row-band height pulled per callback by [`encode_pdf_page_streamed`].
pub const PDF_STREAM_BAND_ROWS: u32 = 256;

/// Encode one page by pulling RGBA row bands top-to-bottom from `next_band`
/// (called with `(y0, rows)`, must return a `width × rows` RGBA buffer) and
/// feeding them straight into the zlib stream — no canvas-sized buffer is ever
/// built, so canvases beyond the flat-buffer cap can print (Viewport
/// Streaming). Byte-identical to [`encode_pdf_page`] on the same pixels.
pub fn encode_pdf_page_streamed(
    w: u32,
    h: u32,
    dpi: f32,
    mut next_band: impl FnMut(u32, u32) -> Result<Vec<u8>, String>,
) -> Result<EncodedPdfPage, String> {
    if w == 0 || h == 0 {
        return Err("Empty page, cannot export PDF".into());
    }
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    let mut y = 0u32;
    while y < h {
        let rows = PDF_STREAM_BAND_ROWS.min(h - y);
        let band = next_band(y, rows)?;
        let expected = (w as usize) * (rows as usize) * 4;
        if band.len() < expected {
            return Err("Print band buffer too small".into());
        }
        let rgb = flatten_onto_white(&band, w, rows);
        encoder
            .write_all(&rgb)
            .map_err(|e| format!("PDF image compression error: {e}"))?;
        y += rows;
    }
    Ok(EncodedPdfPage {
        compressed_rgb: encoder
            .finish()
            .map_err(|e| format!("PDF image compression error: {e}"))?,
        w,
        h,
        dpi,
        components: 3,
    })
}

/// Assemble the single-page print PDF around an already-encoded image (see
/// [`build_pdf`] for the layout/ICC semantics).
pub fn build_pdf_encoded(
    page: &EncodedPdfPage,
    layout: &PrintLayout,
    icc: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    build_pdf_encoded_with_vectors(page, &[], layout, icc)
}

/// Like [`build_pdf_encoded`], but overlays crisp vector paths (see
/// [`PdfVectorObject`]) over the raster image. On an RGB page the paths carry
/// sRGB colour; on a DeviceCMYK ink page they carry process ink (`PdfPaintColor`
/// resolves each colour to the page's space), so a CMYK design keeps its vector
/// artwork instead of flattening to one image. Gradient shadings are RGB-only.
pub fn build_pdf_encoded_with_vectors(
    page: &EncodedPdfPage,
    vectors: &[PdfVectorObject],
    layout: &PrintLayout,
    icc: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    let (w, h, dpi) = (page.w, page.h, page.dpi);
    let compressed = &page.compressed_rgb;

    let (pw, ph) = page_points(layout, w, h, dpi);
    let (dw, dh, tx, ty) = placement(layout, w, h, dpi);
    let (clip_x, clip_y, clip_w, clip_h) = printable_area_points(layout, w, h, dpi);

    let mut pdf: Vec<u8> = Vec::new();
    let mut offsets: Vec<usize> = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");

    offsets.push(pdf.len());
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    offsets.push(pdf.len());
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    offsets.push(pdf.len());
    let shading_resources = if page.components == 3 {
        pdf_shading_resources(vectors)
    } else {
        String::new()
    };
    let extgstate_resources = pdf_extgstate_resources(vectors);
    let colorspace_resources = pdf_colorspace_resources(vectors);
    // TrimBox / BleedBox tag the cut size for a press when marks are on: the
    // artwork is the bleed box, the trim box is that inset by the bleed.
    let box_entries = if layout.marks.is_active() {
        let (trx0, try0, trx1, try1) = marks_trim_box(tx, ty, dw, dh, layout.marks.bleed_mm);
        format!(
            " /BleedBox [{tx:.2} {ty:.2} {:.2} {:.2}] /TrimBox [{trx0:.2} {try0:.2} {trx1:.2} {try1:.2}]",
            tx + dw,
            ty + dh
        )
    } else {
        String::new()
    };
    pdf.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw:.2} {ph:.2}]{box_entries} \
             /Resources << /XObject << /Im0 5 0 R >> {shading_resources} {extgstate_resources} {colorspace_resources} >> /Contents 4 0 R >>\nendobj\n"
        )
        .as_bytes(),
    );

    let mut content = format!(
        "q\n{clip_x:.2} {clip_y:.2} {clip_w:.2} {clip_h:.2} re W n\n{dw:.2} 0 0 {dh:.2} {tx:.2} {ty:.2} cm\n/Im0 Do\nQ\n"
    );
    // Overlay crisp vector paths. RGB pages emit rg/RG; DeviceCMYK ink pages emit
    // k/K (each object's PdfPaintColor already matches the page's colour space).
    append_vector_content(&mut content, vectors, w, h, dw, dh, tx, ty);
    // Crop / registration marks sit in the page margin around the artwork.
    append_print_marks(
        &mut content,
        tx,
        ty,
        dw,
        dh,
        &layout.marks,
        page.components == 4,
    );
    offsets.push(pdf.len());
    pdf.extend_from_slice(format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes());
    pdf.extend_from_slice(content.as_bytes());
    pdf.extend_from_slice(b"\nendstream\nendobj\n");

    // DeviceCMYK ink pages ignore the (RGB) printer profile; RGB pages get an
    // ICCBased colour space (object 6) when a profile is supplied.
    let icc = if page.components == 4 { None } else { icc };
    let colorspace = if page.components == 4 {
        "/DeviceCMYK".to_string()
    } else if icc.is_some() {
        "[/ICCBased 6 0 R]".to_string()
    } else {
        "/DeviceRGB".to_string()
    };
    offsets.push(pdf.len());
    pdf.extend_from_slice(
        format!(
            "5 0 obj\n<< /Type /XObject /Subtype /Image /Width {w} /Height {h} \
             /ColorSpace {colorspace} /Intent /{} /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>\nstream\n",
            layout.intent.pdf_name(),
            compressed.len()
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(&compressed);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");

    if let Some(icc) = icc {
        offsets.push(pdf.len());
        pdf.extend_from_slice(
            format!(
                "6 0 obj\n<< /N 3 /Alternate /DeviceRGB /Length {} >>\nstream\n",
                icc.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(icc);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
    }

    // Cross-reference table + trailer.
    let xref_off = pdf.len();
    let count = offsets.len() + 1;
    pdf.extend_from_slice(format!("xref\n0 {count}\n").as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size {count} /Root 1 0 R >>\nstartxref\n{xref_off}\n%%EOF\n")
            .as_bytes(),
    );

    Ok(pdf)
}

/// One page's pixels + geometry for [`build_pdf_multipage`].
#[allow(dead_code)]
pub struct MultiPageInput<'a> {
    pub rgba: &'a [u8],
    pub w: u32,
    pub h: u32,
    pub dpi: f32,
}

/// Losslessly compressed raster page ready for the PDF writer.
pub struct EncodedPdfPage {
    compressed_rgb: Vec<u8>,
    w: u32,
    h: u32,
    dpi: f32,
    /// Colour components per pixel in the compressed stream: 3 = RGB,
    /// 4 = DeviceCMYK (ink planes, 255 = full ink).
    components: u8,
}

/// Encode one page independently so a caller can render a large source PDF
/// sequentially and release each full RGBA buffer immediately.
pub fn encode_pdf_page(rgba: &[u8], w: u32, h: u32, dpi: f32) -> Result<EncodedPdfPage, String> {
    if w == 0 || h == 0 {
        return Err("Empty page, cannot export PDF".into());
    }
    let expected = (w as usize)
        .checked_mul(h as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "PDF page dimensions overflow".to_string())?;
    if rgba.len() < expected {
        return Err("Image buffer too small to export PDF".into());
    }
    Ok(EncodedPdfPage {
        compressed_rgb: compress_rgb_lossless(rgba, w, h)?,
        w,
        h,
        dpi,
        components: 3,
    })
}

/// Encode one page of raw CMYK8 ink (4 bytes/pixel, 255 = full ink — exactly
/// PDF's `/DeviceCMYK` convention) for a press-ready PDF. The buffer comes
/// from [`crate::core::canvas::Canvas::flatten_ink`], already composited on
/// white paper, so no alpha handling is needed.
pub fn encode_pdf_page_cmyk(
    cmyk: &[u8],
    w: u32,
    h: u32,
    dpi: f32,
) -> Result<EncodedPdfPage, String> {
    if w == 0 || h == 0 {
        return Err("Empty page, cannot export PDF".into());
    }
    let expected = (w as usize)
        .checked_mul(h as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "PDF page dimensions overflow".to_string())?;
    if cmyk.len() < expected {
        return Err("Ink buffer too small to export PDF".into());
    }
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(&cmyk[..expected])
        .map_err(|e| format!("PDF image compression error: {e}"))?;
    Ok(EncodedPdfPage {
        compressed_rgb: encoder
            .finish()
            .map_err(|e| format!("PDF image compression error: {e}"))?,
        w,
        h,
        dpi,
        components: 4,
    })
}

/// A resolved paint colour for a PDF vector object, already in the page's output
/// colour space: sRGB for an RGB page, process ink for a CMYK page. CMYK is kept
/// verbatim (never round-tripped through RGB) so pure/rich-black intent reaches
/// the separations, matching the DeviceCMYK raster base drawn on the same page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PdfPaintColor {
    /// sRGB channels in `0..1` → PDF `rg` (fill) / `RG` (stroke).
    Rgb([f32; 3]),
    /// Process CMYK ink fractions in `0..1` → PDF `k` (fill) / `K` (stroke).
    Cmyk([f32; 4]),
    /// A named spot ink at a tint, painted through a `Separation` colour space so
    /// it lands on its own plate. `alt` is the process-CMYK the ink approximates
    /// at full strength (the separation's tint-transform target).
    Spot {
        name: crate::core::vector::color::SpotName,
        tint: f32,
        alt: [f32; 4],
    },
}

impl PdfPaintColor {
    /// The PDF fill-colour operator line for a device colour (`… rg` / `… k`).
    /// Spot inks are painted via a colour-space resource, not here (see
    /// [`append_vector_content`]); this falls back to the process alternate.
    fn fill_op(&self) -> String {
        match self {
            PdfPaintColor::Rgb([r, g, b]) => format!("{r:.4} {g:.4} {b:.4} rg\n"),
            PdfPaintColor::Cmyk([c, m, y, k]) => format!("{c:.4} {m:.4} {y:.4} {k:.4} k\n"),
            PdfPaintColor::Spot { alt, tint, .. } => {
                let [c, m, y, k] = scale_alt(*alt, *tint);
                format!("{c:.4} {m:.4} {y:.4} {k:.4} k\n")
            }
        }
    }
    /// The PDF stroke-colour operator line for a device colour (`… RG` / `… K`).
    fn stroke_op(&self) -> String {
        match self {
            PdfPaintColor::Rgb([r, g, b]) => format!("{r:.4} {g:.4} {b:.4} RG\n"),
            PdfPaintColor::Cmyk([c, m, y, k]) => format!("{c:.4} {m:.4} {y:.4} {k:.4} K\n"),
            PdfPaintColor::Spot { alt, tint, .. } => {
                let [c, m, y, k] = scale_alt(*alt, *tint);
                format!("{c:.4} {m:.4} {y:.4} {k:.4} K\n")
            }
        }
    }
}

/// Scale a process-CMYK alternate (floats in `0..1`) by a spot tint.
fn scale_alt(alt: [f32; 4], tint: f32) -> [f32; 4] {
    let t = tint.clamp(0.0, 1.0);
    [alt[0] * t, alt[1] * t, alt[2] * t, alt[3] * t]
}

/// One vector object drawn crisply on a PDF page, ON TOP of the raster image.
/// Geometry is in CANVAS PIXEL coordinates (the object transform already
/// applied); the page builder maps it to points with the SAME placement as the
/// image, so each vector lands exactly over its rasterised twin and sharpens the
/// edges at any zoom / print size (true resolution independence). Colours are
/// resolved to the page's colour space ([`PdfPaintColor`]). Only opaque, unmasked,
/// Normal-blend Path layers that sit above all raster content become vectors;
/// everything else stays in the image, so the output is always visually correct.
#[derive(Clone)]
pub struct PdfVectorObject {
    pub path: crate::core::vector::path::PathData,
    pub fill: Option<PdfPaintColor>,
    /// Gradient transform maps unit gradient space directly into canvas pixels.
    /// Keeping it here lets the PDF writer paint axial/radial shadings through
    /// the same affine transform as the editable object, including ellipses.
    /// RGB pages only — CMYK gradient shadings aren't emitted yet.
    pub fill_gradient: Option<crate::core::vector::style::Gradient>,
    pub stroke: Option<PdfPaintColor>,
    pub stroke_width_px: f32,
    pub stroke_cap: crate::core::vector::style::LineCap,
    pub stroke_join: crate::core::vector::style::LineJoin,
    pub stroke_miter_limit: f32,
    pub stroke_dash: Vec<f32>,
    pub stroke_dash_offset: f32,
    pub even_odd: bool,
    /// Overprint the fill on the separations (leave the underlying inks, don't
    /// knock them out). Only meaningful on a DeviceCMYK page; the writer emits an
    /// overprint `ExtGState` (`/op true /OPM 1`) when set.
    pub fill_overprint: bool,
    /// Overprint the outline, independent of the fill (`/OP true /OPM 1`).
    pub stroke_overprint: bool,
}

/// A PDF-ready split of a canvas: opaque Path layers that PDF can represent
/// natively, plus the layer ids that must be omitted from the raster base. The
/// eligibility rule is deliberately conservative because the current writer
/// overlays native paths after one raster image; only the top uninterrupted Path
/// run can be promoted without changing z-order.
pub struct PdfVectorSelection {
    pub objects: Vec<PdfVectorObject>,
    pub promoted_layer_ids: Vec<u32>,
}

fn shape_as_vector(
    shape: &crate::core::shape::ShapeData,
    layer_offset: (i32, i32),
) -> crate::core::vector::object::VectorObjectData {
    shape.to_vector_object(layer_offset)
}

fn arrowheads_as_pdf_path(
    path: &crate::core::vector::path::PathData,
    half: f32,
    start: crate::core::vector::style::ArrowStyle,
    end: crate::core::vector::style::ArrowStyle,
) -> Option<crate::core::vector::path::PathData> {
    use crate::core::vector::path::{Contour, FillRule, Node, PathData};
    let lines = crate::core::vector::flatten::flatten_path(path, 0.25);
    let closed: Vec<bool> = path.contours.iter().map(|contour| contour.closed).collect();
    let rings =
        crate::core::vector::stroke::arrowhead_contours(&lines, &closed, half, start, end, 0.25);
    if rings.is_empty() {
        return None;
    }
    Some(PathData {
        contours: rings
            .into_iter()
            .map(|ring| Contour {
                nodes: ring.into_iter().map(Node::sharp).collect(),
                closed: true,
            })
            .collect(),
        fill_rule: FillRule::NonZero,
    })
}

pub fn collect_pdf_vectors(canvas: &crate::core::canvas::Canvas) -> PdfVectorSelection {
    use crate::core::layer::{BlendMode, LayerType};
    use crate::core::vector::path::FillRule;
    use crate::core::vector::style::Paint;

    // A CMYK document resolves every paint to process ink under its OWN profile,
    // so the native vectors separate identically to the DeviceCMYK raster base on
    // the page (K stays K). If the profile is unusable we cannot separate safely,
    // so keep the whole page raster (empty selection) rather than guess.
    let cmyk_conv = if canvas.is_cmyk() {
        match canvas.cmyk_converter() {
            Some(conv) => Some(conv),
            None => {
                return PdfVectorSelection {
                    objects: Vec::new(),
                    promoted_layer_ids: Vec::new(),
                };
            }
        }
    } else {
        None
    };
    let layers = &canvas.layer_stack.layers;
    // Resolve a solid paint into the page's output colour space: verbatim/
    // separated ink on a CMYK doc, sRGB otherwise. Non-solid paints have no flat
    // colour here (gradients are handled separately).
    let paint_color = |p: Paint| -> Option<PdfPaintColor> {
        use crate::core::vector::color::ColorValue;
        match p {
            // A spot ink keeps its plate identity on ANY page (it becomes a PDF
            // Separation colour space); it is never folded into process here.
            Paint::Solid(ColorValue::Spot {
                name, tint, alt, ..
            }) => Some(PdfPaintColor::Spot {
                name,
                tint,
                alt: [
                    alt[0] as f32 / 255.0,
                    alt[1] as f32 / 255.0,
                    alt[2] as f32 / 255.0,
                    alt[3] as f32 / 255.0,
                ],
            }),
            Paint::Solid(c) => Some(match &cmyk_conv {
                Some(conv) => {
                    let [ci, m, y, k] = c.to_cmyk8(conv);
                    PdfPaintColor::Cmyk([
                        ci as f32 / 255.0,
                        m as f32 / 255.0,
                        y as f32 / 255.0,
                        k as f32 / 255.0,
                    ])
                }
                None => {
                    let [r, g, b, _] = c.to_rgba8();
                    PdfPaintColor::Rgb([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0])
                }
            }),
            Paint::Gradient(_) => None,
            Paint::None => None,
        }
    };
    let ancestors_are_plain = |index: usize| {
        let mut parent_id = layers[index].parent_id;
        while let Some(id) = parent_id {
            let Some(parent) = layers.iter().find(|layer| layer.id == id) else {
                return false;
            };
            if !parent.visible
                || parent.mask.is_some()
                || (parent.opacity - 1.0).abs() > 1e-3
                || parent.blend_mode != BlendMode::Normal
            {
                return false;
            }
            parent_id = parent.parent_id;
        }
        true
    };

    let mut objects = Vec::new();
    let mut promoted_layer_ids = Vec::new();
    // Walk from the top down and stop at the first painted layer that cannot be
    // represented natively. This guarantees the later vector overlay never
    // jumps above unsupported content that originally covered it.
    for (i, layer) in layers.iter().enumerate().rev() {
        if !canvas.layer_stack.is_effectively_visible(i) || layer.is_group() {
            continue;
        }
        if layer.mask.is_some()
            || (layer.opacity - 1.0).abs() > 1e-3
            || layer.blend_mode != BlendMode::Normal
            || !ancestors_are_plain(i)
        {
            break;
        }
        // Editable text is promoted through transient glyph outlines. The layer
        // remains Text in the document; only the PDF representation is curves.
        if let LayerType::Text(text) = &layer.layer_type {
            let mut text_objects = crate::core::text::text_vector_objects(text, layer.offset);
            // The tiles are the authoritative appearance of an editable Text
            // layer.  After move/transform/edit round-trips, their ink bounds
            // can differ from the origin reconstructed from TextData (notably
            // for older .iai files).  Screen compositing uses those tiles, so
            // register the transient PDF outlines to the same ink top-left.
            // This prevents exported text drifting right/down or being clipped
            // even though it still looks correctly positioned in the editor.
            if let (Some((tile_x, tile_y, _, _)), Some(vector_bounds)) = (
                layer.tiles.content_bounds(),
                text_objects
                    .iter()
                    .filter_map(|object| object.path_in_layer_space().control_bounds())
                    .reduce(|a, b| a.union(b)),
            ) {
                let dx = layer.offset.0 as f32 + tile_x as f32 - vector_bounds.x;
                let dy = layer.offset.1 as f32 + tile_y as f32 - vector_bounds.y;
                if dx.abs() > 0.01 || dy.abs() > 0.01 {
                    let correction =
                        crate::core::vector::affine::AffineTransform::translate(dx, dy);
                    for object in &mut text_objects {
                        object.transform = correction.then(&object.transform);
                    }
                }
            }
            if text_objects.is_empty()
                || text_objects.iter().any(|object| {
                    (object.style.opacity - 1.0).abs() > 1e-3
                        || !matches!(object.style.fill, Paint::Solid(color) if color.alpha() >= 1.0 - 1e-3)
                })
            {
                break;
            }
            for object in text_objects {
                let fill = paint_color(object.style.fill);
                if fill.is_none() {
                    break;
                }
                objects.push(PdfVectorObject {
                    path: object.path_in_layer_space(),
                    fill,
                    fill_gradient: None,
                    stroke: None,
                    stroke_width_px: 0.0,
                    stroke_cap: object.style.stroke_style.cap,
                    stroke_join: object.style.stroke_style.join,
                    stroke_miter_limit: object.style.stroke_style.miter_limit,
                    stroke_dash: Vec::new(),
                    stroke_dash_offset: 0.0,
                    even_odd: false,
                    fill_overprint: false,
                    stroke_overprint: false,
                });
            }
            promoted_layer_ids.push(layer.id);
            continue;
        }
        let shape_object;
        let obj = match &layer.layer_type {
            LayerType::Vector(crate::core::vector::object::VectorGeometry::Path(object)) => object,
            LayerType::Vector(crate::core::vector::object::VectorGeometry::Primitive(shape)) => {
                shape_object = shape_as_vector(shape, layer.offset);
                &shape_object
            }
            _ => break,
        };
        if (obj.style.opacity - 1.0).abs() > 1e-3 {
            break;
        }
        // Overprint only has separation meaning on a DeviceCMYK page: the writer
        // emits an overprint ExtGState so the ink leaves the planes underneath
        // (e.g. black text over colour doesn't knock a hole in it). On an RGB
        // page there are no separations to overprint, so keep such an object in
        // the raster base rather than emit a path with knockout semantics.
        if cmyk_conv.is_none() && (obj.style.fill_overprint || obj.style.stroke_overprint) {
            break;
        }
        // Gradient outlines still need a PDF pattern-stroke implementation.
        // Never promote only the supported half of an object because that would
        // drop artwork. Fill gradients and dash arrays are emitted natively.
        if matches!(obj.style.stroke, Paint::Gradient(_)) {
            break;
        }
        // CMYK gradient shadings aren't emitted yet (the shading resource is
        // DeviceRGB). Keep gradient-FILLED objects in the ink raster base on a
        // CMYK page rather than dropping the gradient or mixing colour spaces.
        if cmyk_conv.is_some() && matches!(obj.style.fill, Paint::Gradient(_)) {
            break;
        }
        // The native writer does not yet emit transparency graphics states.
        // Keep any translucent paint in the raster base rather than changing
        // its appearance.
        let fill_is_opaque = match obj.style.fill {
            Paint::None => true,
            Paint::Solid(c) => c.alpha() >= 1.0 - 1e-3,
            Paint::Gradient(g) => g
                .active_stops()
                .iter()
                .all(|stop| stop.color.alpha() >= 1.0 - 1e-3),
        };
        let stroke_is_opaque = match obj.style.stroke {
            Paint::None => true,
            Paint::Solid(c) => c.alpha() >= 1.0 - 1e-3,
            Paint::Gradient(_) => false,
        };
        if !fill_is_opaque || !stroke_is_opaque {
            break;
        }
        let fill = match obj.style.fill {
            Paint::Solid(_) => paint_color(obj.style.fill),
            Paint::None | Paint::Gradient(_) => None,
        };
        let fill_gradient = match obj.style.fill {
            Paint::Gradient(mut gradient) => {
                gradient.transform = obj.transform.then(&gradient.transform);
                Some(gradient)
            }
            Paint::None | Paint::Solid(_) => None,
        };
        let half = obj.style.effective_stroke_width() * 0.5;
        let stroke = (obj.style.stroke.is_visible() && half > 0.0)
            .then(|| paint_color(obj.style.stroke))
            .flatten();
        if fill.is_none() && fill_gradient.is_none() && stroke.is_none() {
            continue;
        }
        // A Vector Brush stroke is a variable-width ribbon over an OPEN centerline
        // — filling that centerline as a polygon would be wrong. Emit its expanded
        // closed outline instead (the same geometry Expand Stroke bakes), so the
        // PDF ribbon matches the on-screen raster. If it can't be expanded, fall
        // back to the raster base like any other unsupported object.
        let emitted_path = match &obj.brush {
            Some(brush) => match crate::core::vector::brush::expand_stroke(&obj.path, brush) {
                Some(mut outline) => {
                    outline.transform(&obj.transform);
                    outline
                }
                None => break,
            },
            None => obj.path_in_layer_space(),
        };
        let arrow_path = if obj.brush.is_none() && stroke.is_some() {
            arrowheads_as_pdf_path(
                &emitted_path,
                obj.style.effective_stroke_width() * 0.5,
                obj.style.stroke_style.start_arrow,
                obj.style.stroke_style.end_arrow,
            )
        } else {
            None
        };
        objects.push(PdfVectorObject {
            path: emitted_path,
            fill,
            fill_gradient,
            stroke,
            stroke_width_px: obj.style.effective_stroke_width(),
            stroke_cap: obj.style.stroke_style.cap,
            stroke_join: obj.style.stroke_style.join,
            stroke_miter_limit: obj.style.stroke_style.miter_limit,
            stroke_dash: obj.style.stroke_style.dash.as_slice().to_vec(),
            stroke_dash_offset: obj.style.stroke_style.dash.offset,
            even_odd: obj.path.fill_rule == FillRule::EvenOdd,
            fill_overprint: obj.style.fill_overprint,
            stroke_overprint: obj.style.stroke_overprint,
        });
        if let (Some(path), Some(fill)) = (arrow_path, stroke) {
            objects.push(PdfVectorObject {
                path,
                fill: Some(fill),
                fill_gradient: None,
                stroke: None,
                stroke_width_px: 0.0,
                stroke_cap: obj.style.stroke_style.cap,
                stroke_join: obj.style.stroke_style.join,
                stroke_miter_limit: obj.style.stroke_style.miter_limit,
                stroke_dash: Vec::new(),
                stroke_dash_offset: 0.0,
                even_odd: false,
                // The arrowhead is drawn with the outline's ink, so it follows
                // the outline's overprint setting.
                fill_overprint: obj.style.stroke_overprint,
                stroke_overprint: false,
            });
        }
        promoted_layer_ids.push(layer.id);
    }
    objects.reverse();
    promoted_layer_ids.reverse();
    PdfVectorSelection {
        objects,
        promoted_layer_ids,
    }
}

/// Flatten the PDF raster base without native vector paths. Operates on a clone
/// so export cannot perturb visibility, selection, undo, or document dirtiness.
pub fn pdf_raster_base(
    canvas: &crate::core::canvas::Canvas,
    selection: &PdfVectorSelection,
) -> Vec<u8> {
    if selection.promoted_layer_ids.is_empty() {
        return canvas.export_flat();
    }
    let promoted: std::collections::HashSet<u32> =
        selection.promoted_layer_ids.iter().copied().collect();
    let mut stack = canvas.layer_stack.clone();
    for layer in &mut stack.layers {
        if promoted.contains(&layer.id) {
            layer.visible = false;
        }
    }
    stack.flatten(canvas.width, canvas.height)
}

/// The DeviceCMYK ink raster base for a CMYK page: the flattened ink planes with
/// the promoted vector layers hidden (they are redrawn as native CMYK paths on
/// top), mirroring [`pdf_raster_base`] for RGB. Returns `None` when the stack is
/// not ink-exact (group / blend / mask / adjustment / ink-less pixel) — the
/// caller then rasterises the whole page without native vectors.
pub fn pdf_ink_base(
    canvas: &crate::core::canvas::Canvas,
    selection: &PdfVectorSelection,
) -> Option<Vec<u8>> {
    let hidden: std::collections::HashSet<u32> =
        selection.promoted_layer_ids.iter().copied().collect();
    canvas.flatten_ink_excluding(&hidden)
}

/// Append the PDF content-stream operators that draw `objects` (in canvas pixel
/// space) over a page whose image occupies `dw×dh` points at `(tx,ty)` for a
/// `w×h` pixel source. All inline (colours as `rg`/`RG`), so no extra PDF
/// resources or object-number changes are needed.
fn append_pdf_path(
    out: &mut String,
    path: &crate::core::vector::path::PathData,
    mx: impl Fn(f32) -> f32,
    my: impl Fn(f32) -> f32,
) {
    for contour in &path.contours {
        if contour.nodes.is_empty() {
            continue;
        }
        let node_count = contour.nodes.len();
        let first = contour.nodes[0].anchor;
        out.push_str(&format!("{:.3} {:.3} m\n", mx(first.x), my(first.y)));
        for segment in 0..contour.segment_count() {
            let Some((_p0, p1, p2, p3)) = contour.segment(segment) else {
                continue;
            };
            let straight = contour.nodes[segment].out_handle.is_none()
                && contour.nodes[(segment + 1) % node_count]
                    .in_handle
                    .is_none();
            if straight {
                out.push_str(&format!("{:.3} {:.3} l\n", mx(p3.x), my(p3.y)));
            } else {
                out.push_str(&format!(
                    "{:.3} {:.3} {:.3} {:.3} {:.3} {:.3} c\n",
                    mx(p1.x),
                    my(p1.y),
                    mx(p2.x),
                    my(p2.y),
                    mx(p3.x),
                    my(p3.y)
                ));
            }
        }
        if contour.closed {
            out.push_str("h\n");
        }
    }
}

fn gradient_function_pdf(gradient: &crate::core::vector::style::Gradient) -> String {
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
        format!(
            "<< /FunctionType 2 /Domain [0 1] /C0 [{:.5} {:.5} {:.5}] /C1 [{:.5} {:.5} {:.5}] /N 1 >>",
            a[0], a[1], a[2], b[0], b[1], b[2]
        )
    };
    if stops.len() == 2 {
        return type2(stops[0].1, stops[1].1);
    }
    let functions = stops
        .windows(2)
        .map(|pair| type2(pair[0].1, pair[1].1))
        .collect::<Vec<_>>()
        .join(" ");
    let bounds = stops[1..stops.len() - 1]
        .iter()
        .map(|(offset, _)| format!("{offset:.6}"))
        .collect::<Vec<_>>()
        .join(" ");
    let encode = (0..stops.len() - 1)
        .map(|_| "0 1")
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "<< /FunctionType 3 /Domain [0 1] /Functions [{functions}] /Bounds [{bounds}] /Encode [{encode}] >>"
    )
}

pub(crate) fn pdf_shading_resources(objects: &[PdfVectorObject]) -> String {
    use crate::core::vector::style::GradientKind;
    let mut entries = String::new();
    for (index, object) in objects.iter().enumerate() {
        let Some(gradient) = object.fill_gradient else {
            continue;
        };
        let (kind, coords) = match gradient.kind {
            GradientKind::Linear => (2, "0 0 1 0"),
            GradientKind::Radial => (3, "0 0 0 0 0 1"),
        };
        entries.push_str(&format!(
            "/IaiSh{index} << /ShadingType {kind} /ColorSpace /DeviceRGB /Coords [{coords}] /Function {} /Extend [true true] >> ",
            gradient_function_pdf(&gradient)
        ));
    }
    if entries.is_empty() {
        String::new()
    } else {
        format!("/Shading << {entries}>>")
    }
}

/// The effective overprint of an object: fill overprint counts only when the
/// object actually paints a fill, stroke overprint only when it strokes.
fn object_overprint(o: &PdfVectorObject) -> (bool, bool) {
    let has_fill = o.fill.is_some() || o.fill_gradient.is_some();
    let has_stroke = o.stroke.is_some() && o.stroke_width_px > 0.0;
    (
        o.fill_overprint && has_fill,
        o.stroke_overprint && has_stroke,
    )
}

/// The named `ExtGState` for an object's fill/stroke overprint combination, or
/// `None` when it overprints nothing. `/op` governs non-stroking (fill) and
/// `/OP` stroking; `/OPM 1` is the standard mode where a zero colorant leaves
/// that separation untouched.
fn overprint_state_name(fill_op: bool, stroke_op: bool) -> Option<&'static str> {
    match (fill_op, stroke_op) {
        (true, true) => Some("IaiOPa"),
        (true, false) => Some("IaiOPf"),
        (false, true) => Some("IaiOPs"),
        (false, false) => None,
    }
}

/// The `/ExtGState` resource entry for the page, holding only the overprint
/// graphics states the `objects` actually reference. Empty when none overprint.
pub(crate) fn pdf_extgstate_resources(objects: &[PdfVectorObject]) -> String {
    let (mut both, mut fill_only, mut stroke_only) = (false, false, false);
    for o in objects {
        match object_overprint(o) {
            (true, true) => both = true,
            (true, false) => fill_only = true,
            (false, true) => stroke_only = true,
            (false, false) => {}
        }
    }
    if !(both || fill_only || stroke_only) {
        return String::new();
    }
    let mut dict = String::from("/ExtGState << ");
    if both {
        dict.push_str("/IaiOPa << /Type /ExtGState /OP true /op true /OPM 1 >> ");
    }
    if fill_only {
        dict.push_str("/IaiOPf << /Type /ExtGState /OP false /op true /OPM 1 >> ");
    }
    if stroke_only {
        dict.push_str("/IaiOPs << /Type /ExtGState /OP true /op false /OPM 1 >> ");
    }
    dict.push_str(">>");
    dict
}

/// A unique spot plate referenced on a page: the colorant name and the process
/// CMYK it approximates at full strength (the `Separation` tint-transform target).
struct SpotPlate {
    name: crate::core::vector::color::SpotName,
    alt: [f32; 4],
}

/// The spot plates referenced by `objects`, in first-use order (deduped by name).
fn spot_plates(objects: &[PdfVectorObject]) -> Vec<SpotPlate> {
    let mut plates: Vec<SpotPlate> = Vec::new();
    for o in objects {
        for paint in [o.fill, o.stroke].into_iter().flatten() {
            if let PdfPaintColor::Spot { name, alt, .. } = paint {
                if !plates.iter().any(|p| p.name == name) {
                    plates.push(SpotPlate { name, alt });
                }
            }
        }
    }
    plates
}

/// Escape a colorant name into a PDF name token (`/Name`), `#`-encoding anything
/// that isn't a plain printable-ASCII non-delimiter (so spaces etc. are legal).
fn pdf_name_escape(name: &str) -> String {
    let mut out = String::from("/");
    for &b in name.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+') {
            out.push(b as char);
        } else {
            out.push_str(&format!("#{b:02X}"));
        }
    }
    if out.len() == 1 {
        out.push_str("Spot");
    }
    out
}

/// The `/ColorSpace` resource declaring one `Separation` colour space per spot
/// plate (`/IaiSp{i}`), each with a Type-2 tint transform to its CMYK alternate
/// so the ink previews correctly and knocks out onto its own plate.
pub(crate) fn pdf_colorspace_resources(objects: &[PdfVectorObject]) -> String {
    let plates = spot_plates(objects);
    if plates.is_empty() {
        return String::new();
    }
    let mut entries = String::new();
    for (i, plate) in plates.iter().enumerate() {
        let [c, m, y, k] = plate.alt;
        entries.push_str(&format!(
            "/IaiSp{i} [/Separation {} /DeviceCMYK << /FunctionType 2 /Domain [0 1] \
             /C0 [0 0 0 0] /C1 [{c:.5} {m:.5} {y:.5} {k:.5}] /N 1 >>] ",
            pdf_name_escape(plate.name.as_str())
        ));
    }
    format!("/ColorSpace << {entries}>>")
}

/// Millimetres → PDF points.
const MM_TO_PT: f32 = 72.0 / 25.4;

/// The trim box `(x0, y0, x1, y1)` in points for an artwork placed at `(tx, ty)`
/// with size `dw × dh` (the bleed box) and the given bleed in millimetres. The
/// trim is the bleed box inset by the bleed, clamped so it never crosses centre.
pub(crate) fn marks_trim_box(
    tx: f32,
    ty: f32,
    dw: f32,
    dh: f32,
    bleed_mm: f32,
) -> (f32, f32, f32, f32) {
    let bleed = (bleed_mm * MM_TO_PT)
        .max(0.0)
        .min(dw * 0.5 - 0.5)
        .min(dh * 0.5 - 0.5)
        .max(0.0);
    (tx + bleed, ty + bleed, tx + dw - bleed, ty + dh - bleed)
}

/// Append a full circle centred at `(cx, cy)` with radius `r` as four cubic
/// Bézier arcs (path operators only — the caller strokes or fills it).
fn append_pdf_circle(out: &mut String, cx: f32, cy: f32, r: f32) {
    let k = 0.552_284_75 * r;
    out.push_str(&format!("{:.3} {:.3} m\n", cx + r, cy));
    out.push_str(&format!(
        "{:.3} {:.3} {:.3} {:.3} {:.3} {:.3} c\n",
        cx + r,
        cy + k,
        cx + k,
        cy + r,
        cx,
        cy + r
    ));
    out.push_str(&format!(
        "{:.3} {:.3} {:.3} {:.3} {:.3} {:.3} c\n",
        cx - k,
        cy + r,
        cx - r,
        cy + k,
        cx - r,
        cy
    ));
    out.push_str(&format!(
        "{:.3} {:.3} {:.3} {:.3} {:.3} {:.3} c\n",
        cx - r,
        cy - k,
        cx - k,
        cy - r,
        cx,
        cy - r
    ));
    out.push_str(&format!(
        "{:.3} {:.3} {:.3} {:.3} {:.3} {:.3} c\n",
        cx + k,
        cy - r,
        cx + r,
        cy - k,
        cx + r,
        cy
    ));
    out.push_str("h\n");
}

/// Append the crop and registration marks for an artwork rectangle placed at
/// `(tx, ty)` (bottom-left, PDF points) with size `dw × dh`. That rectangle is the
/// bleed box; the trim box is inset by the bleed. Marks paint in registration
/// colour so they land on every separation — all-ink on a DeviceCMYK page, black
/// on RGB — and sit OUTSIDE the artwork so they never touch the design.
pub(crate) fn append_print_marks(
    out: &mut String,
    tx: f32,
    ty: f32,
    dw: f32,
    dh: f32,
    marks: &PrintMarks,
    cmyk: bool,
) {
    if !marks.is_active() || dw <= 0.0 || dh <= 0.0 {
        return;
    }
    const GAP: f32 = 3.0;
    const CROP_LEN: f32 = 12.0;
    const REG_RADIUS: f32 = 4.0;
    let (l, r, b, t) = (tx, tx + dw, ty, ty + dh); // bleed-box edges
    let (tl, tr, tb, tt) = marks_trim_box(tx, ty, dw, dh, marks.bleed_mm); // trim edges

    let stroke = if cmyk { "1 1 1 1 K" } else { "0 0 0 RG" };
    out.push_str("q\n");
    out.push_str(&format!("{stroke}\n0.5 w\n0 J\n"));

    if marks.crop_marks {
        let mut line = |x1: f32, y1: f32, x2: f32, y2: f32| {
            out.push_str(&format!("{x1:.3} {y1:.3} m\n{x2:.3} {y2:.3} l\nS\n"));
        };
        // Horizontal marks continue the bottom/top trim lines past the L/R edges.
        line(l - GAP - CROP_LEN, tb, l - GAP, tb);
        line(l - GAP - CROP_LEN, tt, l - GAP, tt);
        line(r + GAP, tb, r + GAP + CROP_LEN, tb);
        line(r + GAP, tt, r + GAP + CROP_LEN, tt);
        // Vertical marks continue the left/right trim lines past the B/T edges.
        line(tl, b - GAP - CROP_LEN, tl, b - GAP);
        line(tr, b - GAP - CROP_LEN, tr, b - GAP);
        line(tl, t + GAP, tl, t + GAP + CROP_LEN);
        line(tr, t + GAP, tr, t + GAP + CROP_LEN);
    }

    if marks.registration_marks {
        let reg_offset = GAP + REG_RADIUS + 3.0;
        let cross = REG_RADIUS + 3.0;
        let centers = [
            (tx + dw * 0.5, b - reg_offset), // bottom edge
            (tx + dw * 0.5, t + reg_offset), // top edge
            (l - reg_offset, ty + dh * 0.5), // left edge
            (r + reg_offset, ty + dh * 0.5), // right edge
        ];
        for (cx, cy) in centers {
            append_pdf_circle(out, cx, cy, REG_RADIUS);
            out.push_str("S\n");
            out.push_str(&format!(
                "{:.3} {:.3} m\n{:.3} {:.3} l\nS\n",
                cx - cross,
                cy,
                cx + cross,
                cy
            ));
            out.push_str(&format!(
                "{:.3} {:.3} m\n{:.3} {:.3} l\nS\n",
                cx,
                cy - cross,
                cx,
                cy + cross
            ));
        }
    }
    out.push_str("Q\n");
}

pub(crate) fn append_vector_content(
    out: &mut String,
    objects: &[PdfVectorObject],
    w: u32,
    h: u32,
    dw: f32,
    dh: f32,
    tx: f32,
    ty: f32,
) {
    if objects.is_empty() || w == 0 || h == 0 {
        return;
    }
    let sx = dw / w as f32;
    let sy = dh / h as f32;
    // Canvas pixel (top-left origin, y-down) → PDF point (bottom-left, y-up).
    let mx = |x: f32| tx + x * sx;
    let my = |y: f32| ty + dh - y * sy;

    // Spot inks paint through page-level Separation colour spaces (/IaiSp{i}).
    let plates = spot_plates(objects);
    let spot_index = |name| plates.iter().position(|p| p.name == name).unwrap_or(0);

    out.push_str("q\n");
    for (index, o) in objects.iter().enumerate() {
        let has_solid_fill = o.fill.is_some();
        let has_gradient_fill = o.fill_gradient.is_some();
        let has_fill = has_solid_fill || has_gradient_fill;
        let has_stroke = o.stroke.is_some() && o.stroke_width_px > 0.0;
        if !has_fill && !has_stroke {
            continue;
        }
        if let Some(gradient) = o.fill_gradient {
            out.push_str("q\n");
            append_pdf_path(out, &o.path, mx, my);
            out.push_str(if o.even_odd { "W* n\n" } else { "W n\n" });
            let page = crate::core::vector::affine::AffineTransform {
                a: sx,
                b: 0.0,
                c: 0.0,
                d: -sy,
                e: tx,
                f: ty + dh,
            };
            let matrix = page.then(&gradient.transform);
            out.push_str(&format!(
                "{:.6} {:.6} {:.6} {:.6} {:.6} {:.6} cm\n/IaiSh{index} sh\nQ\n",
                matrix.a, matrix.b, matrix.c, matrix.d, matrix.e, matrix.f
            ));
        }

        if has_solid_fill || has_stroke {
            out.push_str("q\n");
            // Set the overprint graphics state for this object (scoped by q/Q),
            // so its ink leaves the separations underneath instead of knocking
            // them out. Only DeviceCMYK objects carry these flags.
            let (fill_op, stroke_op) = object_overprint(o);
            if let Some(name) = overprint_state_name(fill_op, stroke_op) {
                out.push_str(&format!("/{name} gs\n"));
            }
            if let Some(color) = o.fill {
                match color {
                    PdfPaintColor::Spot { name, tint, .. } => {
                        out.push_str(&format!("/IaiSp{} cs\n{tint:.4} scn\n", spot_index(name)))
                    }
                    _ => out.push_str(&color.fill_op()),
                }
            }
            if has_stroke {
                if let Some(color) = o.stroke {
                    use crate::core::vector::style::{LineCap, LineJoin};
                    let cap = match o.stroke_cap {
                        LineCap::Butt => 0,
                        LineCap::Round => 1,
                        LineCap::Square => 2,
                    };
                    let join = match o.stroke_join {
                        LineJoin::Miter => 0,
                        LineJoin::Round => 1,
                        LineJoin::Bevel => 2,
                    };
                    let dash = o
                        .stroke_dash
                        .iter()
                        .map(|value| format!("{:.3}", value * sx))
                        .collect::<Vec<_>>()
                        .join(" ");
                    match color {
                        PdfPaintColor::Spot { name, tint, .. } => {
                            out.push_str(&format!("/IaiSp{} CS\n{tint:.4} SCN\n", spot_index(name)))
                        }
                        _ => out.push_str(&color.stroke_op()),
                    }
                    out.push_str(&format!(
                        "{:.3} w\n{cap} J\n{join} j\n{:.3} M\n[{dash}] {:.3} d\n",
                        o.stroke_width_px * sx,
                        o.stroke_miter_limit.max(1.0),
                        o.stroke_dash_offset * sx,
                    ));
                }
            }
            append_pdf_path(out, &o.path, mx, my);
            let op = match (has_solid_fill, has_stroke) {
                (true, true) => {
                    if o.even_odd {
                        "B*"
                    } else {
                        "B"
                    }
                }
                (true, false) => {
                    if o.even_odd {
                        "f*"
                    } else {
                        "f"
                    }
                }
                (false, true) => "S",
                (false, false) => "n",
            };
            out.push_str(op);
            out.push_str("\nQ\n");
        }
    }
    out.push_str("Q\n");
}

/// Build a multi-page print-ready PDF — one image page per input, each sized to
/// its document's physical dimensions (page = document, image fills the page).
/// Every page is losslessly embedded (`FlateDecode`); a single ICC object is shared
/// across pages when `icc` is given. Object numbers are assigned dynamically:
/// 1=Catalog, 2=Pages, then Page/Contents/Image per page, ICC (if any) last.
#[allow(dead_code)]
pub fn build_pdf_multipage(
    pages: &[MultiPageInput],
    icc: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    if pages.is_empty() {
        return Err("No pages to export as PDF".into());
    }
    let encoded = pages
        .iter()
        .map(|page| encode_pdf_page(page.rgba, page.w, page.h, page.dpi))
        .collect::<Result<Vec<_>, _>>()?;
    build_pdf_multipage_encoded(&encoded, &[], icc)
}

/// `vectors[i]` (when present) are drawn as crisp vector paths over page `i`'s
/// image — see [`PdfVectorObject`]. Pass `&[]` for an all-raster PDF.
pub fn build_pdf_multipage_encoded(
    pages: &[EncodedPdfPage],
    vectors: &[Vec<PdfVectorObject>],
    icc: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    if pages.is_empty() {
        return Err("No pages to export as PDF".into());
    }
    let layout = PrintLayout::default();
    let base = 3usize; // objects 1=Catalog, 2=Pages; per-page objects start at 3
    let n = pages.len();
    // The shared (RGB) ICC object only exists when an RGB page would reference
    // it; DeviceCMYK ink pages always carry their own colour space.
    let icc = if pages.iter().any(|p| p.components == 3) {
        icc
    } else {
        None
    };
    let icc_obj = icc.map(|_| base + n * 3);
    let rgb_colorspace = match icc_obj {
        Some(obj) => format!("[/ICCBased {obj} 0 R]"),
        None => "/DeviceRGB".to_string(),
    };

    // Resolve page geometry; pixel data is already compressed independently.
    struct Encoded<'a> {
        compressed_rgb: &'a [u8],
        w: u32,
        h: u32,
        pw: f32,
        ph: f32,
        dw: f32,
        dh: f32,
        tx: f32,
        ty: f32,
        components: u8,
    }
    let mut encoded: Vec<Encoded<'_>> = Vec::with_capacity(n);
    for page in pages {
        let (pw, ph) = page_points(&layout, page.w, page.h, page.dpi);
        let (dw, dh, tx, ty) = placement(&layout, page.w, page.h, page.dpi);
        encoded.push(Encoded {
            compressed_rgb: &page.compressed_rgb,
            w: page.w,
            h: page.h,
            pw,
            ph,
            dw,
            dh,
            tx,
            ty,
            components: page.components,
        });
    }

    let mut pdf: Vec<u8> = Vec::new();
    let mut offsets: Vec<usize> = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");

    offsets.push(pdf.len());
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let kids = (0..n)
        .map(|i| format!("{} 0 R", base + i * 3))
        .collect::<Vec<_>>()
        .join(" ");
    offsets.push(pdf.len());
    pdf.extend_from_slice(
        format!("2 0 obj\n<< /Type /Pages /Kids [{kids}] /Count {n} >>\nendobj\n").as_bytes(),
    );

    for (i, e) in encoded.iter().enumerate() {
        let page_obj = base + i * 3;
        let content_obj = page_obj + 1;
        let image_obj = page_obj + 2;
        let shading_resources = if e.components == 3 {
            vectors
                .get(i)
                .map(|objects| pdf_shading_resources(objects))
                .unwrap_or_default()
        } else {
            String::new()
        };
        let extgstate_resources = vectors
            .get(i)
            .map(|objects| pdf_extgstate_resources(objects))
            .unwrap_or_default();
        let colorspace_resources = vectors
            .get(i)
            .map(|objects| pdf_colorspace_resources(objects))
            .unwrap_or_default();

        offsets.push(pdf.len());
        pdf.extend_from_slice(
            format!(
                "{page_obj} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.2} {:.2}] \
                 /Resources << /XObject << /Im0 {image_obj} 0 R >> {shading_resources} {extgstate_resources} {colorspace_resources} >> /Contents {content_obj} 0 R >>\nendobj\n",
                e.pw, e.ph
            )
            .as_bytes(),
        );

        let mut content = format!(
            "q\n0 0 {:.2} {:.2} re W n\n{:.2} 0 0 {:.2} {:.2} {:.2} cm\n/Im0 Do\nQ\n",
            e.pw, e.ph, e.dw, e.dh, e.tx, e.ty
        );
        // Overlay crisp vector paths. RGB pages emit rg/RG; DeviceCMYK ink pages
        // emit k/K (each object's PdfPaintColor matches the page's colour space).
        if let Some(objs) = vectors.get(i) {
            append_vector_content(&mut content, objs, e.w, e.h, e.dw, e.dh, e.tx, e.ty);
        }
        offsets.push(pdf.len());
        pdf.extend_from_slice(
            format!(
                "{content_obj} 0 obj\n<< /Length {} >>\nstream\n",
                content.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(content.as_bytes());
        pdf.extend_from_slice(b"\nendstream\nendobj\n");

        let colorspace = if e.components == 4 {
            "/DeviceCMYK"
        } else {
            rgb_colorspace.as_str()
        };
        offsets.push(pdf.len());
        pdf.extend_from_slice(
            format!(
                "{image_obj} 0 obj\n<< /Type /XObject /Subtype /Image /Width {} /Height {} \
                 /ColorSpace {colorspace} /Intent /{} /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>\nstream\n",
                e.w,
                e.h,
                layout.intent.pdf_name(),
                e.compressed_rgb.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(&e.compressed_rgb);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
    }

    if let (Some(obj), Some(icc)) = (icc_obj, icc) {
        offsets.push(pdf.len());
        pdf.extend_from_slice(
            format!(
                "{obj} 0 obj\n<< /N 3 /Alternate /DeviceRGB /Length {} >>\nstream\n",
                icc.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(icc);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
    }

    let xref_off = pdf.len();
    let count = offsets.len() + 1;
    pdf.extend_from_slice(format!("xref\n0 {count}\n").as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size {count} /Root 1 0 R >>\nstartxref\n{xref_off}\n%%EOF\n")
            .as_bytes(),
    );

    Ok(pdf)
}

#[cfg(target_os = "windows")]
mod winspool {
    #[repr(C)]
    pub struct PrinterInfo4W {
        pub printer_name: *mut u16,
        pub server_name: *mut u16,
        pub attributes: u32,
    }

    #[link(name = "winspool")]
    extern "system" {
        pub fn EnumPrintersW(
            flags: u32,
            name: *const u16,
            level: u32,
            buf: *mut u8,
            cb: u32,
            needed: *mut u32,
            returned: *mut u32,
        ) -> i32;
        pub fn GetDefaultPrinterW(buffer: *mut u16, size: *mut u32) -> i32;
    }

    pub const PRINTER_ENUM_LOCAL: u32 = 2;
    pub const PRINTER_ENUM_CONNECTIONS: u32 = 4;
}

/// Installed printer names straight from the spooler (level 4 is a cheap
/// registry read — no driver round trip).
#[cfg(target_os = "windows")]
fn enum_printer_names() -> Result<Vec<String>, String> {
    let flags = winspool::PRINTER_ENUM_LOCAL | winspool::PRINTER_ENUM_CONNECTIONS;
    let (mut needed, mut returned) = (0u32, 0u32);
    unsafe {
        winspool::EnumPrintersW(
            flags,
            std::ptr::null(),
            4,
            std::ptr::null_mut(),
            0,
            &mut needed,
            &mut returned,
        )
    };
    if needed == 0 {
        return Ok(Vec::new());
    }
    let mut buf = vec![0u8; needed as usize];
    let ok = unsafe {
        winspool::EnumPrintersW(
            flags,
            std::ptr::null(),
            4,
            buf.as_mut_ptr(),
            needed,
            &mut needed,
            &mut returned,
        )
    };
    if ok == 0 {
        return Err("Cannot list printers from Windows spooler".to_string());
    }
    let infos = buf.as_ptr() as *const winspool::PrinterInfo4W;
    let mut names = Vec::with_capacity(returned as usize);
    for i in 0..returned as usize {
        let p = unsafe { (*infos.add(i)).printer_name };
        if p.is_null() {
            continue;
        }
        let mut len = 0usize;
        while unsafe { *p.add(len) } != 0 {
            len += 1;
        }
        let name = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) });
        if !name.trim().is_empty() {
            names.push(name);
        }
    }
    Ok(names)
}

#[cfg(target_os = "windows")]
fn default_printer_name() -> Option<String> {
    let mut size = 0u32;
    unsafe { winspool::GetDefaultPrinterW(std::ptr::null_mut(), &mut size) };
    if size == 0 {
        return None;
    }
    let mut buf = vec![0u16; size as usize];
    if unsafe { winspool::GetDefaultPrinterW(buf.as_mut_ptr(), &mut size) } == 0 {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// One printer's current paper/printable geometry, read from its driver DC —
/// the same numbers the GDI print path uses, so preview and print agree.
/// `is_default` is left `false`; callers that patch an existing list keep the
/// flag they already have.
#[cfg(target_os = "windows")]
pub fn query_printer(name: &str) -> Result<PrinterInfo, String> {
    let metrics = crate::core::print_gdi::printer_page_metrics(name)?;
    Ok(PrinterInfo {
        name: name.to_string(),
        is_default: false,
        paper_points: Some(metrics.paper_points()),
        printable_rect_points: Some(metrics.printable_rect_points()),
    })
}

#[cfg(target_os = "windows")]
pub fn available_printers() -> Result<Vec<PrinterInfo>, String> {
    let names = enum_printer_names()?;
    let default = default_printer_name();
    let mut printers: Vec<PrinterInfo> = names
        .into_iter()
        .map(|name| {
            let is_default = default.as_deref() == Some(name.as_str());
            match query_printer(&name) {
                Ok(mut info) => {
                    info.is_default = is_default;
                    info
                }
                // Offline/broken driver: keep the printer selectable, fall
                // back to A4 for layout like before.
                Err(_) => PrinterInfo {
                    name,
                    is_default,
                    paper_points: None,
                    printable_rect_points: None,
                },
            }
        })
        .collect();
    printers.sort_by(|a, b| b.is_default.cmp(&a.is_default).then(a.name.cmp(&b.name)));
    Ok(printers)
}

#[cfg(not(target_os = "windows"))]
pub fn available_printers() -> Result<Vec<PrinterInfo>, String> {
    let output = std::process::Command::new("lpstat")
        .arg("-e")
        .output()
        .map_err(|e| format!("Cannot run lpstat: {e}"))?;
    if !output.status.success() {
        return Err("Cannot list CUPS printers".to_string());
    }

    let default = std::process::Command::new("lpstat")
        .arg("-d")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| text.split(':').nth(1).map(|name| name.trim().to_string()));

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| PrinterInfo {
            name: name.to_string(),
            is_default: default.as_deref() == Some(name),
            paper_points: None,
            printable_rect_points: None,
        })
        .collect())
}

#[cfg(target_os = "windows")]
pub fn open_printer_settings(printer: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;

    if printer.trim().is_empty() {
        return Err("No printer selected".to_string());
    }

    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let status = std::process::Command::new("rundll32.exe")
        .creation_flags(CREATE_NO_WINDOW)
        .args(["printui.dll,PrintUIEntry", "/e", "/n", printer])
        .status()
        .map_err(|e| format!("Cannot open printer driver settings: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Printer driver settings exited with {status}"))
    }
}

#[cfg(not(target_os = "windows"))]
pub fn open_printer_settings(_printer: &str) -> Result<(), String> {
    Err("Printer driver settings are only wired on Windows right now".to_string())
}

/// Send a (PDF) file to the system's default printer. Cross-platform.
#[cfg(target_os = "windows")]
#[allow(dead_code)]
pub fn send_to_printer(path: &Path) -> Result<(), String> {
    send_to_printer_with_options(path, None, 1)
}

#[cfg(target_os = "windows")]
pub fn send_to_printer_with_options(
    path: &Path,
    printer: Option<&str>,
    copies: u32,
) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            hwnd: *mut core::ffi::c_void,
            op: *const u16,
            file: *const u16,
            params: *const u16,
            dir: *const u16,
            show: i32,
        ) -> isize;
    }

    let file: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let selected_printer = printer.filter(|p| !p.trim().is_empty());
    let verb_text = if selected_printer.is_some() {
        "printto"
    } else {
        "print"
    };
    let verb: Vec<u16> = verb_text.encode_utf16().chain(std::iter::once(0)).collect();
    let params_buf = selected_printer.map(|printer| {
        format!("\"{printer}\"")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    });
    let params = params_buf
        .as_ref()
        .map_or(std::ptr::null(), |buf| buf.as_ptr());
    // SW_HIDE = 0. ShellExecute returns >32 on success.
    for _ in 0..copies.max(1) {
        let r = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                params,
                std::ptr::null(),
                0,
            )
        };
        if r <= 32 {
            return Err(format!("Print failed with code {r}"));
        }
    }
    Ok(())
}

/// Send a (PDF) file to the default printer via CUPS `lp` (Linux & macOS).
#[cfg(not(target_os = "windows"))]
pub fn send_to_printer(path: &Path) -> Result<(), String> {
    let status = std::process::Command::new("lp")
        .arg(path)
        .status()
        .map_err(|e| format!("Cannot run 'lp' (CUPS): {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("The 'lp' print command returned an error".into())
    }
}

#[cfg(not(target_os = "windows"))]
pub fn send_to_printer_with_options(
    path: &Path,
    printer: Option<&str>,
    copies: u32,
) -> Result<(), String> {
    let mut cmd = std::process::Command::new("lp");
    if let Some(printer) = printer.filter(|p| !p.trim().is_empty()) {
        cmd.args(["-d", printer]);
    }
    let copies = copies.max(1).to_string();
    let status = cmd
        .args(["-n", &copies])
        .arg(path)
        .status()
        .map_err(|e| format!("Cannot run lp (CUPS): {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("lp returned an error".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_keeps_document_physical_size_even_when_larger_than_paper() {
        let layout = PrintLayout {
            page_points: Some((360.0, 504.0)),
            center: true,
            margin_mm: 10.0,
            ..Default::default()
        };
        let (dw, dh, x, y) = placement(&layout, 2480, 3508, 300.0);

        assert!(
            (dw - 595.2).abs() < 0.5,
            "A4 width at 300 dpi should stay physical"
        );
        assert!(
            (dh - 841.92).abs() < 0.5,
            "A4 height at 300 dpi should stay physical"
        );
        assert!(
            x < 0.0,
            "oversized centered image should extend beyond paper horizontally"
        );
        assert!(
            y < 0.0,
            "oversized centered image should extend beyond paper vertically"
        );
    }

    #[test]
    fn build_pdf_produces_valid_header_and_eof() {
        let rgba = vec![128u8; 8 * 8 * 4];
        let pdf = build_pdf(&rgba, 8, 8, 72.0, &PrintLayout::default(), None).expect("pdf");
        assert!(pdf.starts_with(b"%PDF-1.4"), "missing PDF header");
        assert!(pdf.ends_with(b"%%EOF\n"), "missing EOF");
        assert!(
            pdf.windows(11).any(|w| w == b"FlateDecode"),
            "image not embedded"
        );
        assert!(
            !pdf.windows(9).any(|w| w == b"DCTDecode"),
            "lossy JPEG compression must not be used"
        );
        assert!(pdf.windows(9).any(|w| w == b"startxref"), "missing xref");
    }

    #[test]
    fn build_pdf_without_icc_uses_device_rgb() {
        let rgba = vec![128u8; 8 * 8 * 4];
        let pdf = build_pdf(&rgba, 8, 8, 72.0, &PrintLayout::default(), None).expect("pdf");
        let text = String::from_utf8_lossy(&pdf);
        assert!(
            text.contains("/ColorSpace /DeviceRGB"),
            "printer-managed PDFs should stay plain RGB"
        );
        assert!(
            !pdf.windows(8).any(|w| w == b"ICCBased"),
            "ICC object should only be emitted for app-managed print colour"
        );
    }

    #[test]
    fn pdf_image_compression_roundtrips_losslessly() {
        use std::io::Read;

        let rgba = [
            10, 20, 30, 255, // opaque
            200, 100, 50, 128, // alpha-composited onto white
        ];
        let compressed = compress_rgb_lossless(&rgba, 2, 1).expect("compress");
        let mut decoded = Vec::new();
        flate2::read::ZlibDecoder::new(compressed.as_slice())
            .read_to_end(&mut decoded)
            .expect("decompress");

        assert_eq!(decoded, flatten_onto_white(&rgba, 2, 1));
    }

    #[test]
    fn build_pdf_media_box_matches_document_size() {
        let rgba = vec![128u8; 600 * 300 * 4];
        let pdf = build_pdf(&rgba, 600, 300, 300.0, &PrintLayout::default(), None).expect("pdf");
        let text = String::from_utf8_lossy(&pdf);
        assert!(
            text.contains("/MediaBox [0 0 144.00 72.00]"),
            "PDF page size should match canvas physical size"
        );
    }

    #[test]
    fn build_pdf_can_preview_printer_paper_size() {
        let rgba = vec![128u8; 1181 * 1772 * 4];
        let layout = PrintLayout {
            page_points: Some(A4_POINTS),
            center: true,
            ..Default::default()
        };
        let pdf = build_pdf(&rgba, 1181, 1772, 300.0, &layout, None).expect("pdf");
        let text = String::from_utf8_lossy(&pdf);
        assert!(
            text.contains("/MediaBox [0 0 595.28 841.89]"),
            "Print PDF page should match selected printer paper"
        );
        assert!(
            text.contains("283.44 0 0 425.28"),
            "10x15cm image should stay physically small on A4 paper"
        );
    }

    #[test]
    fn streamed_pdf_matches_flat_pdf_bytes() {
        // The same pixels through the flat path and the row-band streamed path
        // must produce identical PDF bytes, pinning the Viewport-Streaming
        // print output to the classic one.
        let (w, h) = (60u32, 700u32); // > one 256-row band, uneven remainder
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(&[
                    (x * 4) as u8,
                    (y % 251) as u8,
                    ((x + y) % 253) as u8,
                    if x % 7 == 0 { 200 } else { 255 },
                ]);
            }
        }
        let layout = PrintLayout::default();
        let flat = build_pdf(&rgba, w, h, 300.0, &layout, None).unwrap();
        let page = encode_pdf_page_streamed(w, h, 300.0, |y, rows| {
            let start = ((y * w) * 4) as usize;
            let len = ((rows * w) * 4) as usize;
            Ok(rgba[start..start + len].to_vec())
        })
        .unwrap();
        let streamed = build_pdf_encoded(&page, &layout, None).unwrap();
        assert_eq!(flat, streamed);
    }

    #[test]
    fn build_pdf_rejects_empty() {
        assert!(build_pdf(&[], 0, 0, 72.0, &PrintLayout::default(), None).is_err());
    }

    #[test]
    fn build_pdf_multipage_has_one_page_per_input() {
        let a = vec![10u8; 8 * 8 * 4];
        let b = vec![20u8; 16 * 8 * 4];
        let c = vec![30u8; 4 * 12 * 4];
        let pages = [
            MultiPageInput {
                rgba: &a,
                w: 8,
                h: 8,
                dpi: 72.0,
            },
            MultiPageInput {
                rgba: &b,
                w: 16,
                h: 8,
                dpi: 72.0,
            },
            MultiPageInput {
                rgba: &c,
                w: 4,
                h: 12,
                dpi: 72.0,
            },
        ];
        let pdf = build_pdf_multipage(&pages, None).expect("pdf");
        assert!(pdf.starts_with(b"%PDF-1.4"), "missing header");
        assert!(pdf.ends_with(b"%%EOF\n"), "missing EOF");
        assert!(
            pdf.windows(9).any(|w| w == b"startxref"),
            "missing startxref"
        );
        let count = pdf.windows(11).filter(|w| *w == b"FlateDecode").count();
        assert_eq!(count, 3, "expected one image per page");
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/Count 3"), "page count should be 3");
        // First page is 8x8 at 72 dpi => 8pt square MediaBox.
        assert!(
            text.contains("/MediaBox [0 0 8.00 8.00]"),
            "first page size wrong"
        );
    }

    #[test]
    fn build_pdf_multipage_rejects_empty() {
        assert!(build_pdf_multipage(&[], None).is_err());
    }

    #[test]
    fn single_page_print_pdf_embeds_vector_path_operators() {
        use crate::core::geometry::Point;
        use crate::core::vector::path::{Contour, FillRule, Node, PathData};

        // The Ctrl+P / print Save-as-PDF path (build_pdf_with_vectors) must draw
        // qualifying Path layers as crisp vectors, just like File ▸ Export does.
        let rgba = vec![255u8; 4 * 4 * 4];
        let path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(4.0, 0.0)),
                    Node::sharp(Point::new(2.0, 4.0)),
                ],
                true,
            )],
            FillRule::NonZero,
        );
        let obj = PdfVectorObject {
            path,
            fill: Some(PdfPaintColor::Rgb([0.0, 0.0, 1.0])),
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
        };
        let pdf = build_pdf_with_vectors(
            &rgba,
            4,
            4,
            72.0,
            std::slice::from_ref(&obj),
            &PrintLayout::default(),
            None,
        )
        .expect("pdf");
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains(" m\n"), "vector moveto present");
        assert!(text.contains(" l\n"), "vector lineto present");
        assert!(
            text.contains("0.0000 0.0000 1.0000 rg"),
            "blue fill colour set"
        );
        assert!(text.contains("f\nQ"), "nonzero fill operator present");
        // Plain build_pdf (no vectors) stays pure raster — no fill operator.
        let plain = build_pdf(&rgba, 4, 4, 72.0, &PrintLayout::default(), None).expect("pdf");
        assert!(
            !String::from_utf8_lossy(&plain).contains(" rg\n"),
            "raster-only PDF has no vector overlay"
        );
    }

    #[test]
    fn editable_text_is_promoted_to_crisp_pdf_curves() {
        use crate::core::layer::LayerType;
        use crate::core::text::TextData;

        let mut canvas = crate::core::canvas::Canvas::new_blank(320, 120);
        let index = canvas.layer_stack.active_idx;
        let text = TextData {
            content: "Sharp PDF text".to_string(),
            font_px: 42.0,
            color: [12, 34, 56, 255],
            ..TextData::default()
        };
        let layer_id = {
            let layer = &mut canvas.layer_stack.layers[index];
            layer.layer_type = LayerType::Text(text);
            layer.offset = (18, 22);
            layer.id
        };

        let selection = collect_pdf_vectors(&canvas);
        assert_eq!(selection.promoted_layer_ids, vec![layer_id]);
        assert!(!selection.objects.is_empty());
        assert!(selection.objects.iter().all(|object| object.fill.is_some()));

        let base = pdf_raster_base(&canvas, &selection);
        let pdf = build_pdf_with_vectors(
            &base,
            canvas.width,
            canvas.height,
            72.0,
            &selection.objects,
            &PrintLayout::default(),
            None,
        )
        .expect("text PDF");
        let source = String::from_utf8_lossy(&pdf);
        assert!(source.contains(" m\n") && source.contains("f\nQ"));
    }

    #[test]
    fn pdf_text_curves_follow_the_visible_tile_ink_position() {
        use crate::core::layer::LayerType;
        use crate::core::text::{rasterize, TextData};
        use crate::core::tile::TileMap;

        let text = TextData {
            content: "Visible text".to_string(),
            font_px: 36.0,
            ..TextData::default()
        };
        let Some(raster) = rasterize(&text) else {
            return;
        };
        let inset = (37u32, 19u32);
        let (w, h) = (raster.width + inset.0 + 4, raster.height + inset.1 + 4);
        let mut rgba = vec![0u8; w as usize * h as usize * 4];
        for y in 0..raster.height {
            let src = y as usize * raster.width as usize * 4;
            let dst = ((y + inset.1) as usize * w as usize + inset.0 as usize) * 4;
            rgba[dst..dst + raster.width as usize * 4]
                .copy_from_slice(&raster.rgba[src..src + raster.width as usize * 4]);
        }

        let mut canvas = crate::core::canvas::Canvas::new_blank(500, 200);
        let layer = &mut canvas.layer_stack.layers[canvas.layer_stack.active_idx];
        layer.layer_type = LayerType::Text(text);
        layer.offset = (11, 23);
        layer.width = w;
        layer.height = h;
        layer.tiles = TileMap::from_rgba(&rgba, w, h);
        let (ink_x, ink_y, _, _) = layer.tiles.content_bounds().expect("tile ink bounds");
        let expected_x = (layer.offset.0 + ink_x) as f32;
        let expected_y = (layer.offset.1 + ink_y) as f32;

        let selection = collect_pdf_vectors(&canvas);
        let bounds = selection
            .objects
            .iter()
            .filter_map(|object| object.path.control_bounds())
            .reduce(|a, b| a.union(b))
            .expect("text vector bounds");
        assert!((bounds.x - expected_x).abs() < 0.01);
        assert!((bounds.y - expected_y).abs() < 0.01);
    }

    #[test]
    fn promoted_path_is_removed_from_pdf_raster_base() {
        use crate::core::command_vector::apply_object_to_layer;
        use crate::core::geometry::Point;
        use crate::core::vector::affine::AffineTransform;
        use crate::core::vector::color::ColorValue;
        use crate::core::vector::object::VectorObjectData;
        use crate::core::vector::path::{Contour, FillRule, Node, PathData};
        use crate::core::vector::style::VectorStyle;

        let mut canvas = crate::core::canvas::Canvas::from_rgba(vec![255; 16 * 16 * 4], 16, 16);
        let index = canvas.add_layer();
        let path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(2.0, 2.0)),
                    Node::sharp(Point::new(14.0, 2.0)),
                    Node::sharp(Point::new(14.0, 14.0)),
                    Node::sharp(Point::new(2.0, 14.0)),
                ],
                true,
            )],
            FillRule::NonZero,
        );
        apply_object_to_layer(
            &mut canvas.layer_stack.layers[index],
            VectorObjectData::new(
                path,
                VectorStyle::filled(ColorValue::rgb(1.0, 0.0, 0.0)),
                AffineTransform::IDENTITY,
            ),
        );

        let full = canvas.export_flat();
        let selection = collect_pdf_vectors(&canvas);
        assert_eq!(selection.objects.len(), 1);
        let base = pdf_raster_base(&canvas, &selection);
        let center = ((8 * 16 + 8) * 4) as usize;
        assert!(
            full[center] > 200 && full[center + 1] < 30,
            "full canvas contains red Path"
        );
        assert_eq!(&base[center..center + 4], &[255, 255, 255, 255]);
    }

    #[test]
    fn gradient_fill_and_dashed_outline_are_native_pdf_vectors() {
        use crate::core::command_vector::apply_object_to_layer;
        use crate::core::geometry::Point;
        use crate::core::vector::affine::AffineTransform;
        use crate::core::vector::color::ColorValue;
        use crate::core::vector::object::VectorObjectData;
        use crate::core::vector::path::{Contour, FillRule, Node, PathData};
        use crate::core::vector::style::{
            DashPattern, Gradient, GradientKind, GradientStop, Paint, StrokeStyle, VectorStyle,
        };

        let mut gradient = Gradient::two_color(
            GradientKind::Radial,
            ColorValue::rgb(1.0, 0.0, 0.0),
            ColorValue::rgb(0.0, 0.0, 1.0),
            AffineTransform::translate(8.0, 8.0).then(&AffineTransform::scale(6.0, 3.0)),
        );
        gradient.stop_count = 3;
        gradient.stops[1] = GradientStop {
            offset: 0.4,
            color: ColorValue::rgb(0.0, 1.0, 0.0),
        };
        gradient.stops[2] = GradientStop {
            offset: 1.0,
            color: ColorValue::rgb(0.0, 0.0, 1.0),
        };
        let style = VectorStyle {
            fill: Paint::Gradient(gradient),
            stroke: Paint::Solid(ColorValue::BLACK),
            stroke_style: StrokeStyle {
                width: 2.0,
                dash: DashPattern::from_slice(&[3.0, 2.0], 1.0),
                ..StrokeStyle::default()
            },
            ..VectorStyle::default()
        };
        let path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(2.0, 2.0)),
                    Node::sharp(Point::new(14.0, 2.0)),
                    Node::sharp(Point::new(14.0, 14.0)),
                    Node::sharp(Point::new(2.0, 14.0)),
                ],
                true,
            )],
            FillRule::NonZero,
        );
        let mut canvas = crate::core::canvas::Canvas::from_rgba(vec![255; 16 * 16 * 4], 16, 16);
        let index = canvas.add_layer();
        apply_object_to_layer(
            &mut canvas.layer_stack.layers[index],
            VectorObjectData::new(path, style, AffineTransform::IDENTITY),
        );

        let selection = collect_pdf_vectors(&canvas);
        assert_eq!(selection.objects.len(), 1);
        assert!(selection.objects[0].fill_gradient.is_some());
        assert_eq!(selection.objects[0].stroke_dash, vec![3.0, 2.0]);
        let base = pdf_raster_base(&canvas, &selection);
        let pdf = build_pdf_with_vectors(
            &base,
            16,
            16,
            72.0,
            &selection.objects,
            &PrintLayout::default(),
            None,
        )
        .expect("gradient pdf");
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/ShadingType 3"));
        assert!(text.contains("/FunctionType 3"));
        assert!(text.contains("/IaiSh0 sh"));
        assert!(text.contains("[3.000 2.000] 1.000 d"));
        lopdf::Document::load_mem(&pdf).expect("valid PDF structure");
    }

    #[test]
    fn top_shape_does_not_force_path_below_it_into_pdf_raster() {
        use crate::core::command_vector::apply_object_to_layer;
        use crate::core::geometry::Point;
        use crate::core::layer::LayerType;
        use crate::core::shape::{ShapeData, ShapeKind};
        use crate::core::vector::affine::AffineTransform;
        use crate::core::vector::color::ColorValue;
        use crate::core::vector::object::VectorObjectData;
        use crate::core::vector::path::{Contour, FillRule, Node, PathData};
        use crate::core::vector::style::{Gradient, GradientKind, Paint, VectorStyle};

        let mut canvas = crate::core::canvas::Canvas::from_rgba(vec![255; 100 * 100 * 4], 100, 100);
        let path_index = canvas.add_layer();
        apply_object_to_layer(
            &mut canvas.layer_stack.layers[path_index],
            VectorObjectData::new(
                PathData::new(
                    vec![Contour::new(
                        vec![
                            Node::sharp(Point::new(10.0, 10.0)),
                            Node::sharp(Point::new(90.0, 10.0)),
                            Node::sharp(Point::new(50.0, 90.0)),
                        ],
                        true,
                    )],
                    FillRule::NonZero,
                ),
                VectorStyle::filled(ColorValue::rgb(1.0, 0.0, 0.0)),
                AffineTransform::IDENTITY,
            ),
        );
        let shape_index = canvas.add_layer();
        let (mut shape, offset) = ShapeData::from_canvas_span(
            ShapeKind::Rectangle,
            15.0,
            15.0,
            70.0,
            70.0,
            12.0,
            true,
            [0, 0, 0, 255],
            0.0,
            [0, 0, 0, 0],
        );
        shape.style.fill = Paint::Gradient(Gradient::two_color(
            GradientKind::Linear,
            ColorValue::rgb(0.0, 0.0, 0.0),
            ColorValue::rgb(0.0, 0.0, 1.0),
            AffineTransform::translate(shape.x0, shape.y0)
                .then(&AffineTransform::scale((shape.x1 - shape.x0).abs(), 1.0)),
        ));
        canvas.layer_stack.layers[shape_index].layer_type = LayerType::Vector(
            crate::core::vector::object::VectorGeometry::Primitive(shape),
        );
        canvas.layer_stack.layers[shape_index].offset = offset;

        let selection = collect_pdf_vectors(&canvas);
        assert_eq!(selection.objects.len(), 2);
        assert_eq!(selection.promoted_layer_ids.len(), 2);
        assert!(
            selection.objects[1].fill_gradient.is_some(),
            "primitive Shape gradient must stay native"
        );
        let base = pdf_raster_base(&canvas, &selection);
        let pdf = build_pdf_with_vectors(
            &base,
            100,
            100,
            72.0,
            &selection.objects,
            &PrintLayout::default(),
            None,
        )
        .expect("shape and path PDF");
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains(" rg\n"), "solid Path must stay native");
        assert!(
            text.contains("/ShadingType 2") && text.contains("/IaiSh1 sh"),
            "primitive Shape gradient must be emitted as native PDF shading"
        );
    }

    #[test]
    fn nonopaque_path_stays_in_pdf_raster_base() {
        use crate::core::layer::LayerType;
        let mut canvas = crate::core::canvas::Canvas::new(4, 4);
        let index = canvas.add_layer();
        canvas.layer_stack.layers[index].opacity = 0.5;
        // A non-Path layer is enough to pin the conservative eligibility rule;
        // importantly, no layer id may be hidden from the raster base.
        assert!(matches!(
            canvas.layer_stack.layers[index].layer_type,
            LayerType::Raster
        ));
        let selection = collect_pdf_vectors(&canvas);
        assert!(selection.promoted_layer_ids.is_empty());
    }

    #[test]
    fn brush_stroke_promotes_as_closed_native_outline() {
        use crate::core::command_vector::apply_object_to_layer;
        use crate::core::geometry::Point;
        use crate::core::vector::affine::AffineTransform;
        use crate::core::vector::brush::BrushStroke;
        use crate::core::vector::color::ColorValue;
        use crate::core::vector::object::VectorObjectData;
        use crate::core::vector::path::{Contour, FillRule, Node, PathData};
        use crate::core::vector::style::{LineCap, VectorStyle};

        let mut canvas = crate::core::canvas::Canvas::from_rgba(vec![255; 60 * 60 * 4], 60, 60);
        let index = canvas.add_layer();
        // Open centerline + uniform brush, painted by a solid fill.
        let centerline = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(10.0, 30.0)),
                    Node::sharp(Point::new(50.0, 30.0)),
                ],
                false,
            )],
            FillRule::NonZero,
        );
        apply_object_to_layer(
            &mut canvas.layer_stack.layers[index],
            VectorObjectData::new_brush(
                centerline,
                VectorStyle::filled(ColorValue::rgb(0.0, 0.0, 0.0)),
                AffineTransform::IDENTITY,
                BrushStroke::uniform(8.0, LineCap::Round),
            ),
        );
        let selection = collect_pdf_vectors(&canvas);
        assert_eq!(selection.objects.len(), 1, "brush ribbon promoted natively");
        // The emitted path is the CLOSED expanded outline, not the open centerline.
        assert!(
            selection.objects[0].path.contours.iter().all(|c| c.closed),
            "emitted brush outline must be closed"
        );
        assert!(
            selection.objects[0].fill.is_some(),
            "ribbon filled natively"
        );
    }

    #[test]
    fn rgb_overprint_path_stays_in_raster_base() {
        use crate::core::command_vector::apply_object_to_layer;
        use crate::core::geometry::Point;
        use crate::core::vector::affine::AffineTransform;
        use crate::core::vector::color::ColorValue;
        use crate::core::vector::object::VectorObjectData;
        use crate::core::vector::path::{Contour, FillRule, Node, PathData};
        use crate::core::vector::style::VectorStyle;

        // Overprint is meaningful on separations, not on an additive RGB page, so
        // an RGB overprint object is left in the raster base (CMYK is promoted —
        // see cmyk_overprint_object_is_promoted_and_emits_extgstate).
        let mut style = VectorStyle::filled(ColorValue::rgb(1.0, 0.0, 0.0));
        style.fill_overprint = true;
        let path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(1.0, 1.0)),
                    Node::sharp(Point::new(7.0, 1.0)),
                    Node::sharp(Point::new(4.0, 7.0)),
                ],
                true,
            )],
            FillRule::NonZero,
        );
        let mut canvas = crate::core::canvas::Canvas::new(8, 8);
        let index = canvas.add_layer();
        apply_object_to_layer(
            &mut canvas.layer_stack.layers[index],
            VectorObjectData::new(path, style, AffineTransform::IDENTITY),
        );
        let selection = collect_pdf_vectors(&canvas);
        assert!(selection.objects.is_empty());
        assert!(selection.promoted_layer_ids.is_empty());
    }

    #[test]
    fn multipage_pdf_embeds_vector_path_operators() {
        use crate::core::geometry::Point;
        use crate::core::vector::path::{Contour, FillRule, Node, PathData};

        // A 4×4 white RGB page with a filled red triangle drawn on top as vector.
        let rgba = vec![255u8; 4 * 4 * 4];
        let page = encode_pdf_page(&rgba, 4, 4, 72.0).expect("encode");
        let path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(4.0, 0.0)),
                    Node::sharp(Point::new(2.0, 4.0)),
                ],
                true,
            )],
            FillRule::NonZero,
        );
        let obj = PdfVectorObject {
            path,
            fill: Some(PdfPaintColor::Rgb([1.0, 0.0, 0.0])),
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
        };
        let pdf = build_pdf_multipage_encoded(&[page], &[vec![obj]], None).expect("pdf");
        let text = String::from_utf8_lossy(&pdf);
        // The content stream is uncompressed, so the path operators are visible.
        assert!(text.contains(" m\n"), "vector moveto present");
        assert!(text.contains(" l\n"), "vector lineto present");
        assert!(
            text.contains("1.0000 0.0000 0.0000 rg"),
            "red fill colour set"
        );
        assert!(text.contains("f\nQ"), "nonzero fill operator present");
        // Still exactly one embedded image (the raster page) — no extra XObjects.
        let images = pdf.windows(11).filter(|w| *w == b"FlateDecode").count();
        assert_eq!(images, 1, "vector overlay adds no image XObject");
    }

    #[test]
    fn cmyk_page_gets_native_devicecmyk_vector_overlay() {
        use crate::core::geometry::Point;
        use crate::core::vector::path::{Contour, FillRule, Node, PathData};

        // A DeviceCMYK ink page carries its vectors as native DeviceCMYK paths
        // (k operator), keeping the artwork crisp with no sRGB leak.
        let ink = vec![0u8; 4 * 4 * 4];
        let page = encode_pdf_page_cmyk(&ink, 4, 4, 72.0).expect("encode cmyk");
        let path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(4.0, 0.0)),
                    Node::sharp(Point::new(2.0, 4.0)),
                ],
                true,
            )],
            FillRule::NonZero,
        );
        let obj = PdfVectorObject {
            path,
            fill: Some(PdfPaintColor::Cmyk([0.0, 1.0, 1.0, 0.0])),
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
        };
        let pdf = build_pdf_multipage_encoded(&[page], &[vec![obj]], None).expect("pdf");
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/DeviceCMYK"), "ink page is DeviceCMYK");
        assert!(
            text.contains(" k\n"),
            "vector fill uses the DeviceCMYK k operator"
        );
        assert!(
            !text.contains(" rg\n"),
            "no sRGB fill leaks onto a CMYK page"
        );
    }

    #[test]
    fn build_pdf_multipage_can_embed_icc() {
        let a = vec![10u8; 8 * 8 * 4];
        let icc = crate::core::cms::srgb_icc_bytes();
        let pages = [MultiPageInput {
            rgba: &a,
            w: 8,
            h: 8,
            dpi: 72.0,
        }];
        let pdf = build_pdf_multipage(&pages, Some(&icc)).expect("pdf");
        assert!(pdf.windows(8).any(|w| w == b"ICCBased"), "missing ICCBased");
    }

    #[test]
    fn build_pdf_with_icc_is_color_managed() {
        let rgba = vec![128u8; 8 * 8 * 4];
        let icc = crate::core::cms::srgb_icc_bytes();
        let layout = PrintLayout {
            intent: RenderIntent::RelativeColorimetric,
            ..Default::default()
        };
        let pdf = build_pdf(&rgba, 8, 8, 72.0, &layout, Some(&icc)).expect("pdf");
        assert!(pdf.windows(8).any(|w| w == b"ICCBased"), "missing ICCBased");
        let intent = b"RelativeColorimetric";
        assert!(
            pdf.windows(intent.len()).any(|w| w == intent),
            "missing rendering intent"
        );
    }

    #[test]
    fn cmyk_pdf_page_uses_devicecmyk_and_drops_printer_icc() {
        use std::io::Read;

        let cmyk: Vec<u8> = (0..2 * 2 * 4).map(|i| (i * 13) as u8).collect();
        let page = encode_pdf_page_cmyk(&cmyk, 2, 2, 72.0).expect("encode");
        let icc = crate::core::cms::srgb_icc_bytes();
        let pdf = build_pdf_encoded(&page, &PrintLayout::default(), Some(&icc)).expect("pdf");
        let text = String::from_utf8_lossy(&pdf);
        assert!(
            text.contains("/ColorSpace /DeviceCMYK"),
            "ink page must be DeviceCMYK"
        );
        assert!(
            !pdf.windows(8).any(|w| w == b"ICCBased"),
            "an RGB printer profile must not tag an ink page"
        );
        // The embedded stream is the raw ink bytes, losslessly.
        let mut decoded = Vec::new();
        flate2::read::ZlibDecoder::new(page.compressed_rgb.as_slice())
            .read_to_end(&mut decoded)
            .expect("decompress");
        assert_eq!(decoded, cmyk);
    }

    #[test]
    fn cmyk_pdf_page_rejects_short_buffer() {
        assert!(encode_pdf_page_cmyk(&[0u8; 15], 2, 2, 72.0).is_err());
        assert!(encode_pdf_page_cmyk(&[], 0, 2, 72.0).is_err());
    }

    #[test]
    fn multipage_mixed_pages_keep_per_page_colorspace() {
        let rgb_page = encode_pdf_page(&[10u8; 8 * 8 * 4], 8, 8, 72.0).expect("rgb");
        let ink_page = encode_pdf_page_cmyk(&[20u8; 8 * 8 * 4], 8, 8, 72.0).expect("ink");
        let icc = crate::core::cms::srgb_icc_bytes();
        let pdf = build_pdf_multipage_encoded(&[rgb_page, ink_page], &[], Some(&icc)).expect("pdf");
        let text = String::from_utf8_lossy(&pdf);
        assert!(
            text.contains("/ColorSpace [/ICCBased"),
            "RGB page keeps the shared ICC colour space"
        );
        assert!(
            text.contains("/ColorSpace /DeviceCMYK"),
            "ink page is DeviceCMYK"
        );
        assert!(pdf.ends_with(b"%%EOF\n"), "missing EOF");
    }

    #[test]
    fn multipage_all_ink_pages_skip_unused_icc_object() {
        let ink_page = encode_pdf_page_cmyk(&[20u8; 8 * 8 * 4], 8, 8, 72.0).expect("ink");
        let icc = crate::core::cms::srgb_icc_bytes();
        let pdf = build_pdf_multipage_encoded(&[ink_page], &[], Some(&icc)).expect("pdf");
        assert!(
            !pdf.windows(8).any(|w| w == b"ICCBased"),
            "no RGB page references the ICC object, so it must not be embedded"
        );
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/ColorSpace /DeviceCMYK"));
        assert!(pdf.ends_with(b"%%EOF\n"), "missing EOF");
    }

    #[test]
    fn pdf_paint_color_emits_devicecmyk_operators() {
        assert_eq!(
            PdfPaintColor::Rgb([1.0, 0.0, 0.0]).fill_op(),
            "1.0000 0.0000 0.0000 rg\n"
        );
        assert_eq!(
            PdfPaintColor::Cmyk([0.0, 0.0, 0.0, 1.0]).fill_op(),
            "0.0000 0.0000 0.0000 1.0000 k\n"
        );
        assert_eq!(
            PdfPaintColor::Cmyk([0.1, 0.2, 0.3, 0.4]).stroke_op(),
            "0.1000 0.2000 0.3000 0.4000 K\n"
        );
    }

    /// A CMYK document keeps its vector artwork as native DeviceCMYK paths, and a
    /// CMYK-authored pure black stays pure K (verbatim, never round-tripped
    /// through RGB) all the way to the promoted PDF colour.
    #[test]
    fn cmyk_document_promotes_vectors_with_verbatim_process_ink() {
        use crate::core::canvas::CmykProfile;
        use crate::core::command_vector::apply_object_to_layer;
        use crate::core::geometry::Point;
        use crate::core::vector::affine::AffineTransform;
        use crate::core::vector::color::ColorValue;
        use crate::core::vector::object::VectorObjectData;
        use crate::core::vector::path::{Contour, FillRule, Node, PathData};
        use crate::core::vector::style::VectorStyle;

        let mut canvas = crate::core::canvas::Canvas::from_rgba(vec![255; 32 * 32 * 4], 32, 32);
        canvas
            .convert_to_cmyk(CmykProfile::Naive)
            .expect("convert to CMYK");
        let index = canvas.add_layer();
        let path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(4.0, 4.0)),
                    Node::sharp(Point::new(28.0, 4.0)),
                    Node::sharp(Point::new(28.0, 28.0)),
                    Node::sharp(Point::new(4.0, 28.0)),
                ],
                true,
            )],
            FillRule::NonZero,
        );
        apply_object_to_layer(
            &mut canvas.layer_stack.layers[index],
            VectorObjectData::new(
                path,
                VectorStyle::filled(ColorValue::cmyk(0.0, 0.0, 0.0, 1.0)),
                AffineTransform::IDENTITY,
            ),
        );

        let selection = collect_pdf_vectors(&canvas);
        assert_eq!(
            selection.objects.len(),
            1,
            "the top CMYK vector is promoted"
        );
        match selection.objects[0].fill {
            Some(PdfPaintColor::Cmyk([c, m, y, k])) => {
                assert!(
                    c == 0.0 && m == 0.0 && y == 0.0 && k >= 0.999,
                    "pure K must stay pure K, got {c},{m},{y},{k}"
                );
            }
            other => panic!("expected a DeviceCMYK fill, got {other:?}"),
        }
    }

    /// The DeviceCMYK ink base drops the promoted vector layers so they aren't
    /// painted twice (once baked, once as the native path on top).
    #[test]
    fn pdf_ink_base_excludes_promoted_layers() {
        use crate::core::canvas::CmykProfile;

        // Red → M+Y ink; a single ink-exact raster layer.
        let mut canvas = crate::core::canvas::Canvas::from_rgba(vec![255, 0, 0, 255], 1, 1);
        canvas
            .convert_to_cmyk(CmykProfile::Naive)
            .expect("convert to CMYK");
        let layer_id = canvas.layer_stack.layers[0].id;

        let none = PdfVectorSelection {
            objects: Vec::new(),
            promoted_layer_ids: Vec::new(),
        };
        assert_eq!(
            pdf_ink_base(&canvas, &none).expect("ink base")[..4],
            [0, 255, 255, 0],
            "with nothing promoted the base carries the red ink"
        );

        let promoted = PdfVectorSelection {
            objects: Vec::new(),
            promoted_layer_ids: vec![layer_id],
        };
        assert_eq!(
            pdf_ink_base(&canvas, &promoted).expect("ink base")[..4],
            [0, 0, 0, 0],
            "the only layer is promoted, so the base is bare paper"
        );
    }

    /// End to end: a CMYK page carries both the DeviceCMYK raster base and native
    /// CMYK vector paths (k/K operators), never sRGB (rg/RG).
    #[test]
    fn cmyk_vector_pdf_uses_devicecmyk_operators() {
        use crate::core::canvas::CmykProfile;
        use crate::core::command_vector::apply_object_to_layer;
        use crate::core::geometry::Point;
        use crate::core::vector::affine::AffineTransform;
        use crate::core::vector::color::ColorValue;
        use crate::core::vector::object::VectorObjectData;
        use crate::core::vector::path::{Contour, FillRule, Node, PathData};
        use crate::core::vector::style::VectorStyle;

        let mut canvas = crate::core::canvas::Canvas::from_rgba(vec![255; 24 * 24 * 4], 24, 24);
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
                            Node::sharp(Point::new(3.0, 3.0)),
                            Node::sharp(Point::new(21.0, 3.0)),
                            Node::sharp(Point::new(21.0, 21.0)),
                            Node::sharp(Point::new(3.0, 21.0)),
                        ],
                        true,
                    )],
                    FillRule::NonZero,
                ),
                VectorStyle::filled(ColorValue::cmyk(0.0, 1.0, 1.0, 0.0)),
                AffineTransform::IDENTITY,
            ),
        );

        let selection = collect_pdf_vectors(&canvas);
        let ink = pdf_ink_base(&canvas, &selection).expect("ink base");
        let page = encode_pdf_page_cmyk(&ink, 24, 24, 72.0).expect("cmyk page");
        let pdf = build_pdf_encoded_with_vectors(
            &page,
            &selection.objects,
            &PrintLayout::default(),
            None,
        )
        .expect("cmyk vector pdf");
        let text = String::from_utf8_lossy(&pdf);
        assert!(
            text.contains("/ColorSpace /DeviceCMYK"),
            "raster base is DeviceCMYK"
        );
        assert!(
            text.contains(" k\n"),
            "vector fill uses the DeviceCMYK k operator"
        );
        assert!(
            !text.contains(" rg\n"),
            "no sRGB fill leaks onto a CMYK page"
        );
    }

    #[test]
    fn pdf_extgstate_resources_lists_only_used_overprint_states() {
        use crate::core::geometry::Point;
        use crate::core::vector::path::{Contour, FillRule, Node, PathData};

        let tri = || {
            PathData::new(
                vec![Contour::new(
                    vec![
                        Node::sharp(Point::new(0.0, 0.0)),
                        Node::sharp(Point::new(4.0, 0.0)),
                        Node::sharp(Point::new(2.0, 4.0)),
                    ],
                    true,
                )],
                FillRule::NonZero,
            )
        };
        let base = PdfVectorObject {
            path: tri(),
            fill: Some(PdfPaintColor::Cmyk([0.0, 0.0, 0.0, 1.0])),
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
        };
        // Nothing overprints → no ExtGState resource.
        assert!(pdf_extgstate_resources(&[base.clone()]).is_empty());

        // A fill-overprint object references only the fill-overprint state.
        let mut fill_op = base.clone();
        fill_op.fill_overprint = true;
        let res = pdf_extgstate_resources(&[fill_op]);
        assert!(
            res.contains("/IaiOPf"),
            "fill-overprint state present: {res}"
        );
        assert!(res.contains("/op true"));
        assert!(res.contains("/OPM 1"));
        assert!(!res.contains("/IaiOPa"), "unused states are omitted: {res}");

        // A fill overprint flag with no visible fill draws nothing to overprint.
        let mut ghost = base;
        ghost.fill = None;
        ghost.fill_overprint = true;
        assert!(pdf_extgstate_resources(&[ghost]).is_empty());
    }

    /// A CMYK document with an overprint fill promotes the object AND carries the
    /// overprint flag through, so the writer can emit the ExtGState.
    #[test]
    fn cmyk_overprint_object_is_promoted_and_emits_extgstate() {
        use crate::core::canvas::CmykProfile;
        use crate::core::command_vector::apply_object_to_layer;
        use crate::core::geometry::Point;
        use crate::core::vector::affine::AffineTransform;
        use crate::core::vector::color::ColorValue;
        use crate::core::vector::object::VectorObjectData;
        use crate::core::vector::path::{Contour, FillRule, Node, PathData};
        use crate::core::vector::style::VectorStyle;

        let mut canvas = crate::core::canvas::Canvas::from_rgba(vec![255; 24 * 24 * 4], 24, 24);
        canvas
            .convert_to_cmyk(CmykProfile::Naive)
            .expect("convert to CMYK");
        let index = canvas.add_layer();
        let mut style = VectorStyle::filled(ColorValue::cmyk(0.0, 0.0, 0.0, 1.0));
        style.fill_overprint = true;
        apply_object_to_layer(
            &mut canvas.layer_stack.layers[index],
            VectorObjectData::new(
                PathData::new(
                    vec![Contour::new(
                        vec![
                            Node::sharp(Point::new(3.0, 3.0)),
                            Node::sharp(Point::new(21.0, 3.0)),
                            Node::sharp(Point::new(21.0, 21.0)),
                            Node::sharp(Point::new(3.0, 21.0)),
                        ],
                        true,
                    )],
                    FillRule::NonZero,
                ),
                style,
                AffineTransform::IDENTITY,
            ),
        );

        let selection = collect_pdf_vectors(&canvas);
        assert_eq!(
            selection.objects.len(),
            1,
            "overprint object is still promoted"
        );
        assert!(
            selection.objects[0].fill_overprint,
            "the fill overprint flag reaches the PDF object"
        );

        let ink = pdf_ink_base(&canvas, &selection).expect("ink base");
        let page = encode_pdf_page_cmyk(&ink, 24, 24, 72.0).expect("cmyk page");
        let pdf = build_pdf_encoded_with_vectors(
            &page,
            &selection.objects,
            &PrintLayout::default(),
            None,
        )
        .expect("cmyk overprint pdf");
        let text = String::from_utf8_lossy(&pdf);
        assert!(
            text.contains("/ExtGState"),
            "page declares an ExtGState resource"
        );
        assert!(text.contains("/OPM 1"), "standard overprint mode set");
        assert!(
            text.contains("/IaiOPf gs\n"),
            "overprint state applied to the object"
        );
    }

    #[test]
    fn marks_trim_box_insets_by_bleed() {
        // No bleed → the trim box equals the bleed box (the artwork).
        assert_eq!(
            marks_trim_box(10.0, 20.0, 100.0, 80.0, 0.0),
            (10.0, 20.0, 110.0, 100.0)
        );
        // 25.4 mm = 72 pt inset on every side.
        let (x0, y0, x1, y1) = marks_trim_box(0.0, 0.0, 300.0, 300.0, 25.4);
        assert!((x0 - 72.0).abs() < 0.01 && (y0 - 72.0).abs() < 0.01);
        assert!((x1 - 228.0).abs() < 0.01 && (y1 - 228.0).abs() < 0.01);
        // An absurd bleed clamps so the trim never crosses the centre.
        let (x0, _, x1, _) = marks_trim_box(0.0, 0.0, 100.0, 100.0, 1000.0);
        assert!((0.0..=1.0).contains(&(x1 - x0)));
    }

    #[test]
    fn append_print_marks_draws_crop_and_registration_in_registration_colour() {
        // CMYK page: registration colour is all-ink so marks hit every plate.
        let mut cmyk = String::new();
        append_print_marks(
            &mut cmyk,
            50.0,
            50.0,
            120.0,
            90.0,
            &PrintMarks {
                bleed_mm: 3.0,
                crop_marks: true,
                registration_marks: true,
            },
            true,
        );
        assert!(
            cmyk.contains("1 1 1 1 K"),
            "registration ink on CMYK: {cmyk}"
        );
        assert!(cmyk.contains(" l\nS\n"), "crop mark line stroked");
        assert!(cmyk.contains(" c\n"), "registration circle arcs present");

        // RGB page: registration marks fall back to black.
        let mut rgb = String::new();
        append_print_marks(
            &mut rgb,
            0.0,
            0.0,
            100.0,
            100.0,
            &PrintMarks {
                bleed_mm: 0.0,
                crop_marks: true,
                registration_marks: false,
            },
            false,
        );
        assert!(rgb.contains("0 0 0 RG"), "black marks on RGB");
        assert!(
            !rgb.contains(" c\n"),
            "no registration circle when disabled"
        );

        // Nothing to draw when marks are off.
        let mut off = String::new();
        append_print_marks(&mut off, 0.0, 0.0, 100.0, 100.0, &PrintMarks::none(), true);
        assert!(off.is_empty());
    }

    #[test]
    fn print_marks_write_trim_and_bleed_boxes() {
        let rgba = vec![255u8; 4 * 4 * 4];
        let layout = PrintLayout {
            page_points: Some((300.0, 300.0)),
            marks: PrintMarks {
                bleed_mm: 2.0,
                crop_marks: true,
                registration_marks: true,
            },
            ..Default::default()
        };
        let pdf = build_pdf(&rgba, 4, 4, 72.0, &layout, None).expect("pdf");
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/TrimBox ["), "trim box tagged for the press");
        assert!(
            text.contains("/BleedBox ["),
            "bleed box tagged for the press"
        );

        // Default layout (marks off) leaves the page exactly as before.
        let plain = build_pdf(&rgba, 4, 4, 72.0, &PrintLayout::default(), None).expect("pdf");
        let plain = String::from_utf8_lossy(&plain);
        assert!(!plain.contains("/TrimBox"), "no press boxes without marks");
    }

    #[test]
    fn pdf_name_escape_encodes_spaces_and_specials() {
        assert_eq!(pdf_name_escape("PANTONE 185 C"), "/PANTONE#20185#20C");
        assert_eq!(pdf_name_escape("Silver"), "/Silver");
        assert_eq!(pdf_name_escape(""), "/Spot");
    }

    /// A spot-filled object exports as a PDF `Separation` colour space (its own
    /// plate) rather than being folded into process, on an RGB or CMYK page.
    #[test]
    fn spot_object_exports_as_a_separation_colour_space() {
        use crate::core::command_vector::apply_object_to_layer;
        use crate::core::geometry::Point;
        use crate::core::vector::affine::AffineTransform;
        use crate::core::vector::color::ColorValue;
        use crate::core::vector::object::VectorObjectData;
        use crate::core::vector::path::{Contour, FillRule, Node, PathData};
        use crate::core::vector::style::VectorStyle;

        let mut canvas = crate::core::canvas::Canvas::from_rgba(vec![255; 24 * 24 * 4], 24, 24);
        let index = canvas.add_layer();
        apply_object_to_layer(
            &mut canvas.layer_stack.layers[index],
            VectorObjectData::new(
                PathData::new(
                    vec![Contour::new(
                        vec![
                            Node::sharp(Point::new(3.0, 3.0)),
                            Node::sharp(Point::new(21.0, 3.0)),
                            Node::sharp(Point::new(21.0, 21.0)),
                            Node::sharp(Point::new(3.0, 21.0)),
                        ],
                        true,
                    )],
                    FillRule::NonZero,
                ),
                // Full-tint warm-red spot ink (alt = M+Y process).
                VectorStyle::filled(ColorValue::spot(
                    "PANTONE Warm Red C",
                    [0, 255, 255, 0],
                    1.0,
                )),
                AffineTransform::IDENTITY,
            ),
        );

        let selection = collect_pdf_vectors(&canvas);
        assert_eq!(selection.objects.len(), 1, "the spot object is promoted");
        assert!(
            matches!(selection.objects[0].fill, Some(PdfPaintColor::Spot { .. })),
            "fill kept as a spot ink, not converted to process"
        );

        let base = pdf_raster_base(&canvas, &selection);
        let pdf = build_pdf_with_vectors(
            &base,
            24,
            24,
            72.0,
            &selection.objects,
            &PrintLayout::default(),
            None,
        )
        .expect("spot pdf");
        let text = String::from_utf8_lossy(&pdf);
        assert!(
            text.contains("/Separation /PANTONE#20Warm#20Red#20C /DeviceCMYK"),
            "declares the named Separation plate: {text}"
        );
        assert!(
            text.contains("/IaiSp0 cs\n"),
            "paints through the spot colour space"
        );
        assert!(text.contains("1.0000 scn\n"), "full-tint fill");
        // The tint transform maps full tint to the process alternate (M+Y).
        assert!(text.contains("/C1 [0.00000 1.00000 1.00000 0.00000]"));
    }

    #[test]
    fn half_tint_spot_paints_at_reduced_coverage() {
        let obj = PdfVectorObject {
            path: crate::core::vector::path::PathData::new(
                vec![crate::core::vector::path::Contour::new(
                    vec![
                        crate::core::vector::path::Node::sharp(crate::core::geometry::Point::new(
                            0.0, 0.0,
                        )),
                        crate::core::vector::path::Node::sharp(crate::core::geometry::Point::new(
                            4.0, 0.0,
                        )),
                        crate::core::vector::path::Node::sharp(crate::core::geometry::Point::new(
                            2.0, 4.0,
                        )),
                    ],
                    true,
                )],
                crate::core::vector::path::FillRule::NonZero,
            ),
            fill: Some(PdfPaintColor::Spot {
                name: crate::core::vector::color::SpotName::new("Spot Green"),
                tint: 0.5,
                alt: [1.0, 0.0, 1.0, 0.0],
            }),
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
        };
        let mut content = String::new();
        append_vector_content(
            &mut content,
            std::slice::from_ref(&obj),
            4,
            4,
            4.0,
            4.0,
            0.0,
            0.0,
        );
        assert!(
            content.contains("/IaiSp0 cs\n0.5000 scn\n"),
            "half tint: {content}"
        );
        assert!(pdf_colorspace_resources(std::slice::from_ref(&obj)).contains("/Separation"));
    }
}

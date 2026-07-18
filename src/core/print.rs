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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PrintLayout {
    pub page_points: Option<(f32, f32)>,
    pub printable_rect_points: Option<(f32, f32, f32, f32)>,
    pub margin_mm: f32,
    pub center: bool,
    /// ICC rendering intent for colour-managed printing (copies & printer device
    /// are tracked separately on the app — see `print_copies`/`print_selected_printer`).
    pub intent: RenderIntent,
}

impl Default for PrintLayout {
    fn default() -> Self {
        Self {
            page_points: None,
            printable_rect_points: None,
            margin_mm: 0.0,
            center: true,
            intent: RenderIntent::Perceptual,
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
    build_pdf_encoded(&page, layout, icc)
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
    pdf.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw:.2} {ph:.2}] \
             /Resources << /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >>\nendobj\n"
        )
        .as_bytes(),
    );

    let content = format!(
        "q\n{clip_x:.2} {clip_y:.2} {clip_w:.2} {clip_h:.2} re W n\n{dw:.2} 0 0 {dh:.2} {tx:.2} {ty:.2} cm\n/Im0 Do\nQ\n"
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
    build_pdf_multipage_encoded(&encoded, icc)
}

pub fn build_pdf_multipage_encoded(
    pages: &[EncodedPdfPage],
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

        offsets.push(pdf.len());
        pdf.extend_from_slice(
            format!(
                "{page_obj} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.2} {:.2}] \
                 /Resources << /XObject << /Im0 {image_obj} 0 R >> >> /Contents {content_obj} 0 R >>\nendobj\n",
                e.pw, e.ph
            )
            .as_bytes(),
        );

        let content = format!(
            "q\n0 0 {:.2} {:.2} re W n\n{:.2} 0 0 {:.2} {:.2} {:.2} cm\n/Im0 Do\nQ\n",
            e.pw, e.ph, e.dw, e.dh, e.tx, e.ty
        );
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
        let pdf = build_pdf_multipage_encoded(&[rgb_page, ink_page], Some(&icc)).expect("pdf");
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
        let pdf = build_pdf_multipage_encoded(&[ink_page], Some(&icc)).expect("pdf");
        assert!(
            !pdf.windows(8).any(|w| w == b"ICCBased"),
            "no RGB page references the ICC object, so it must not be embedded"
        );
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/ColorSpace /DeviceCMYK"));
        assert!(pdf.ends_with(b"%%EOF\n"), "missing EOF");
    }
}

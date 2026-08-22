//! Direct GDI printing (Windows).
//!
//! The PDF print path hands the job to the OS default PDF handler
//! (Foxit/Acrobat/...), which applies its own page scaling — usually
//! "fit to printer margins" — so prints come out smaller than their true
//! physical size. Printing straight through a printer DC removes that
//! middleman: the DC reports the real paper/printable metrics, the shared
//! [`placement`] math positions the image exactly as the Print dialog
//! previews it, and row bands are blitted with `StretchDIBits` at a fixed
//! pixel→point mapping. File ▸ Export and the fallback path keep the PDF.

use super::print::{placement, printable_area_points, PrintLayout};

/// Driver settings selected for one IAI print session.
///
/// Windows printer drivers expose a variable-sized `DEVMODE` block.  Keeping
/// it here (instead of asking PrintUI to edit "Printing Preferences") lets us
/// pass the choices to this app's printer DC without changing the printer's
/// defaults for every other application.
#[derive(Debug, Clone)]
pub struct PrinterSettings {
    printer: String,
    // `DocumentPropertiesW` requires DEVMODE-aligned storage. A byte Vec is
    // only aligned to 1 by contract, whereas usize storage is sufficiently
    // aligned for the structure and its driver-private tail.
    storage: Vec<usize>,
    byte_len: usize,
}

impl PrinterSettings {
    fn new(printer: &str, byte_len: usize) -> Self {
        let words = byte_len.div_ceil(std::mem::size_of::<usize>());
        Self {
            printer: printer.to_string(),
            storage: vec![0; words],
            byte_len,
        }
    }

    pub fn matches_printer(&self, printer: &str) -> bool {
        self.printer == printer
    }

    #[cfg(target_os = "windows")]
    fn as_ptr(&self) -> *const core::ffi::c_void {
        self.storage.as_ptr().cast()
    }

    #[cfg(target_os = "windows")]
    fn as_mut_ptr(&mut self) -> *mut core::ffi::c_void {
        self.storage.as_mut_ptr().cast()
    }
}

/// Physical metrics of one printer device context. Lengths are device pixels;
/// `(printable_x, printable_y)` is the printable area's top-left offset from
/// the paper corner (GDI `PHYSICALOFFSETX/Y`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeviceMetrics {
    pub dpi_x: f32,
    pub dpi_y: f32,
    pub paper_w: i32,
    pub paper_h: i32,
    pub printable_x: i32,
    pub printable_y: i32,
    pub printable_w: i32,
    pub printable_h: i32,
}

impl DeviceMetrics {
    /// Paper size in whole PDF points ([`super::print::PrinterInfo`] convention).
    pub fn paper_points(&self) -> (u32, u32) {
        let w = (self.paper_w as f32 / self.dpi_x * 72.0).round().max(1.0) as u32;
        let h = (self.paper_h as f32 / self.dpi_y * 72.0).round().max(1.0) as u32;
        (w, h)
    }

    /// Printable rect in whole points with a bottom-left origin
    /// ([`super::print::PrinterInfo`] convention).
    pub fn printable_rect_points(&self) -> (u32, u32, u32, u32) {
        let to_x = |v: i32| (v as f32 / self.dpi_x * 72.0).round().max(0.0) as u32;
        let to_y = |v: i32| (v as f32 / self.dpi_y * 72.0).round().max(0.0) as u32;
        let y_bottom = (self.paper_h - self.printable_y - self.printable_h).max(0);
        (
            to_x(self.printable_x),
            to_y(y_bottom),
            to_x(self.printable_w).max(1),
            to_y(self.printable_h).max(1),
        )
    }

    /// A [`PrintLayout`] whose page/printable rects mirror this DC exactly
    /// (PDF points, bottom-left origin), keeping the user's margin/centering/
    /// intent. Using DC ground truth instead of the enumerated printer list
    /// guarantees the blit lands where the driver actually prints.
    pub fn to_layout(&self, user: &PrintLayout) -> PrintLayout {
        let ptx = 72.0 / self.dpi_x;
        let pty = 72.0 / self.dpi_y;
        let page_w = self.paper_w as f32 * ptx;
        let page_h = self.paper_h as f32 * pty;
        let w = self.printable_w as f32 * ptx;
        let h = self.printable_h as f32 * pty;
        let x = self.printable_x as f32 * ptx;
        let y = page_h - (self.printable_y as f32 * pty + h);
        PrintLayout {
            page_points: Some((page_w, page_h)),
            printable_rect_points: Some((x, y, w, h)),
            ..*user
        }
    }
}

/// Whole-image destination rectangle `(x, y, w, h)` in device pixels, origin
/// at the printable area's top-left (the printer DC origin).
pub fn image_dest_rect(
    m: &DeviceMetrics,
    user: &PrintLayout,
    img_w: u32,
    img_h: u32,
    dpi: f32,
) -> (i32, i32, i32, i32) {
    let layout = m.to_layout(user);
    let (dw, dh, tx, ty) = placement(&layout, img_w, img_h, dpi);
    let (_, page_h) = layout.page_points.expect("to_layout always sets page");
    let x = (tx / 72.0 * m.dpi_x).round() as i32 - m.printable_x;
    let y = ((page_h - ty - dh) / 72.0 * m.dpi_y).round() as i32 - m.printable_y;
    let w = ((dw / 72.0 * m.dpi_x).round() as i32).max(1);
    let h = ((dh / 72.0 * m.dpi_y).round() as i32).max(1);
    (x, y, w, h)
}

/// Clip rectangle `(left, top, right, bottom)` in device pixels (printable-
/// origin, right/bottom exclusive) matching the PDF path's content clip:
/// printable area shrunk by the user margin.
pub fn clip_dest_rect(
    m: &DeviceMetrics,
    user: &PrintLayout,
    img_w: u32,
    img_h: u32,
    dpi: f32,
) -> (i32, i32, i32, i32) {
    let layout = m.to_layout(user);
    let (cx, cy, cw, ch) = printable_area_points(&layout, img_w, img_h, dpi);
    let (_, page_h) = layout.page_points.expect("to_layout always sets page");
    let left = (cx / 72.0 * m.dpi_x).round() as i32 - m.printable_x;
    let top = ((page_h - cy - ch) / 72.0 * m.dpi_y).round() as i32 - m.printable_y;
    let right = left + ((cw / 72.0 * m.dpi_x).round() as i32).max(1);
    let bottom = top + ((ch / 72.0 * m.dpi_y).round() as i32).max(1);
    (left, top, right, bottom)
}

/// Vertical destination span `(top, height)` of the source rows `[y0, y0+rows)`
/// inside an image blitted to `dest_y..dest_y+dest_h`. Cumulative rounding, so
/// consecutive bands tile the destination exactly (no gaps, no overlaps).
pub fn band_dest_span(dest_y: i32, dest_h: i32, img_h: u32, y0: u32, rows: u32) -> (i32, i32) {
    let map = |row: u32| ((row as f64) * (dest_h as f64) / (img_h as f64)).round() as i32;
    let top = dest_y + map(y0);
    (top, map(y0 + rows) - map(y0))
}

/// DWORD-aligned row stride of a 24-bit DIB.
pub fn dib_stride(w: u32) -> usize {
    ((w as usize) * 3 + 3) & !3
}

/// Convert a straight-alpha RGBA band into a bottom-up 24-bit BGR DIB:
/// composited onto white (print has no transparency) and DWORD-padded.
pub fn rgba_band_to_dib_bgr(band: &[u8], w: u32, rows: u32) -> Vec<u8> {
    let stride = dib_stride(w);
    let mut dib = vec![0u8; stride * rows as usize];
    for row in 0..rows as usize {
        // Bottom-up DIB: last image row first in memory.
        let dst_row = rows as usize - 1 - row;
        let src = &band[row * (w as usize) * 4..];
        let dst = &mut dib[dst_row * stride..];
        for x in 0..w as usize {
            let a = src[x * 4 + 3] as u16;
            let inv = 255 - a;
            dst[x * 3] = ((src[x * 4 + 2] as u16 * a + 255 * inv) / 255) as u8;
            dst[x * 3 + 1] = ((src[x * 4 + 1] as u16 * a + 255 * inv) / 255) as u8;
            dst[x * 3 + 2] = ((src[x * 4] as u16 * a + 255 * inv) / 255) as u8;
        }
    }
    dib
}

#[cfg(target_os = "windows")]
mod ffi {
    pub type Hdc = *mut core::ffi::c_void;
    pub type Handle = *mut core::ffi::c_void;
    pub type Hwnd = *mut core::ffi::c_void;

    #[repr(C)]
    pub struct DocInfoW {
        pub cb_size: i32,
        pub doc_name: *const u16,
        pub output: *const u16,
        pub datatype: *const u16,
        pub fw_type: u32,
    }

    #[repr(C)]
    pub struct BitmapInfoHeader {
        pub bi_size: u32,
        pub bi_width: i32,
        pub bi_height: i32,
        pub bi_planes: u16,
        pub bi_bit_count: u16,
        pub bi_compression: u32,
        pub bi_size_image: u32,
        pub bi_x_pels_per_meter: i32,
        pub bi_y_pels_per_meter: i32,
        pub bi_clr_used: u32,
        pub bi_clr_important: u32,
    }

    #[link(name = "gdi32")]
    extern "system" {
        pub fn CreateDCW(
            driver: *const u16,
            device: *const u16,
            output: *const u16,
            init_data: *const core::ffi::c_void,
        ) -> Hdc;
        pub fn DeleteDC(hdc: Hdc) -> i32;
        pub fn StartDocW(hdc: Hdc, di: *const DocInfoW) -> i32;
        pub fn EndDoc(hdc: Hdc) -> i32;
        pub fn AbortDoc(hdc: Hdc) -> i32;
        pub fn StartPage(hdc: Hdc) -> i32;
        pub fn EndPage(hdc: Hdc) -> i32;
        pub fn GetDeviceCaps(hdc: Hdc, index: i32) -> i32;
        pub fn SetStretchBltMode(hdc: Hdc, mode: i32) -> i32;
        pub fn IntersectClipRect(hdc: Hdc, left: i32, top: i32, right: i32, bottom: i32) -> i32;
        #[allow(clippy::too_many_arguments)]
        pub fn StretchDIBits(
            hdc: Hdc,
            x_dest: i32,
            y_dest: i32,
            dest_w: i32,
            dest_h: i32,
            x_src: i32,
            y_src: i32,
            src_w: i32,
            src_h: i32,
            bits: *const core::ffi::c_void,
            bmi: *const BitmapInfoHeader,
            usage: u32,
            rop: u32,
        ) -> i32;
    }

    #[link(name = "winspool")]
    extern "system" {
        pub fn OpenPrinterW(
            printer_name: *mut u16,
            printer: *mut Handle,
            defaults: *const core::ffi::c_void,
        ) -> i32;
        pub fn ClosePrinter(printer: Handle) -> i32;
        pub fn DocumentPropertiesW(
            hwnd: Hwnd,
            printer: Handle,
            device_name: *mut u16,
            output: *mut core::ffi::c_void,
            input: *const core::ffi::c_void,
            mode: u32,
        ) -> i32;
    }

    pub const HORZRES: i32 = 8;
    pub const VERTRES: i32 = 10;
    pub const LOGPIXELSX: i32 = 88;
    pub const LOGPIXELSY: i32 = 90;
    pub const PHYSICALWIDTH: i32 = 110;
    pub const PHYSICALHEIGHT: i32 = 111;
    pub const PHYSICALOFFSETX: i32 = 112;
    pub const PHYSICALOFFSETY: i32 = 113;
    pub const BI_RGB: u32 = 0;
    pub const DIB_RGB_COLORS: u32 = 0;
    pub const SRCCOPY: u32 = 0x00CC_0020;
    pub const HALFTONE: i32 = 4;
    pub const GDI_ERROR: i32 = -1;
    pub const DM_OUT_BUFFER: u32 = 0x0000_0002;
    pub const DM_IN_PROMPT: u32 = 0x0000_0004;
    pub const DM_IN_BUFFER: u32 = 0x0000_0008;
    pub const IDOK: i32 = 1;
    pub const IDCANCEL: i32 = 2;
}

#[cfg(target_os = "windows")]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(target_os = "windows")]
struct Dc(ffi::Hdc);
#[cfg(target_os = "windows")]
impl Drop for Dc {
    fn drop(&mut self) {
        unsafe { ffi::DeleteDC(self.0) };
    }
}

#[cfg(target_os = "windows")]
struct PrinterHandle(ffi::Handle);
#[cfg(target_os = "windows")]
impl Drop for PrinterHandle {
    fn drop(&mut self) {
        unsafe { ffi::ClosePrinter(self.0) };
    }
}

/// Show the selected driver's property sheet and return an app-local DEVMODE.
/// No `DM_UPDATE` flag is ever supplied, so the driver's system/user defaults
/// are not written. `Ok(None)` means the user cancelled the dialog.
#[cfg(target_os = "windows")]
pub fn prompt_printer_settings(
    printer: &str,
    current: Option<&PrinterSettings>,
    owner_hwnd: isize,
) -> Result<Option<PrinterSettings>, String> {
    if printer.trim().is_empty() {
        return Err("No printer selected".to_string());
    }

    let mut device = wide(printer);
    let mut raw_handle = std::ptr::null_mut();
    if unsafe { ffi::OpenPrinterW(device.as_mut_ptr(), &mut raw_handle, std::ptr::null()) } == 0 {
        return Err(format!(
            "Cannot open printer '{printer}': {}",
            std::io::Error::last_os_error()
        ));
    }
    let handle = PrinterHandle(raw_handle);
    let hwnd = owner_hwnd as ffi::Hwnd;

    let required = unsafe {
        ffi::DocumentPropertiesW(
            hwnd,
            handle.0,
            device.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null(),
            0,
        )
    };
    if required <= 0 {
        return Err("Printer driver did not provide a DEVMODE".to_string());
    }
    let required = required as usize;

    let input = match current
        .filter(|s| s.matches_printer(printer) && s.byte_len == required && !s.storage.is_empty())
    {
        Some(settings) => settings.clone(),
        None => {
            let mut defaults = PrinterSettings::new(printer, required);
            let result = unsafe {
                ffi::DocumentPropertiesW(
                    hwnd,
                    handle.0,
                    device.as_mut_ptr(),
                    defaults.as_mut_ptr(),
                    std::ptr::null(),
                    ffi::DM_OUT_BUFFER,
                )
            };
            if result != ffi::IDOK {
                return Err("Cannot read the printer's current settings".to_string());
            }
            defaults
        }
    };

    // Keep input and output separate: a few older vendor drivers do not
    // tolerate an in-place DEVMODE during their property sheet callback.
    let mut output = PrinterSettings::new(printer, required);
    let result = unsafe {
        ffi::DocumentPropertiesW(
            hwnd,
            handle.0,
            device.as_mut_ptr(),
            output.as_mut_ptr(),
            input.as_ptr(),
            ffi::DM_IN_BUFFER | ffi::DM_OUT_BUFFER | ffi::DM_IN_PROMPT,
        )
    };
    match result {
        ffi::IDOK => Ok(Some(output)),
        ffi::IDCANCEL => Ok(None),
        code => Err(format!("Printer settings failed with code {code}")),
    }
}

#[cfg(target_os = "windows")]
fn open_printer_dc(printer: &str, settings: Option<&PrinterSettings>) -> Result<Dc, String> {
    if printer.trim().is_empty() {
        return Err("No printer selected".to_string());
    }
    let driver = wide("WINSPOOL");
    let device = wide(printer);
    let init_data = settings
        .filter(|s| s.matches_printer(printer))
        .map_or(std::ptr::null(), PrinterSettings::as_ptr);
    let dc = Dc(unsafe {
        ffi::CreateDCW(
            driver.as_ptr(),
            device.as_ptr(),
            std::ptr::null(),
            init_data,
        )
    });
    if dc.0.is_null() {
        return Err(format!("Cannot open printer '{printer}'"));
    }
    Ok(dc)
}

/// Query one printer's paper/printable metrics straight from its driver —
/// the same ground truth the GDI print path uses. Milliseconds per printer,
/// unlike the seconds-long PowerShell enumeration this replaced.
#[cfg(target_os = "windows")]
pub fn printer_page_metrics(printer: &str) -> Result<DeviceMetrics, String> {
    printer_page_metrics_with_settings(printer, None)
}

/// Query metrics using the app-local DEVMODE returned by
/// [`prompt_printer_settings`], keeping preview and the eventual print DC in
/// agreement without touching the driver's saved defaults.
#[cfg(target_os = "windows")]
pub fn printer_page_metrics_with_settings(
    printer: &str,
    settings: Option<&PrinterSettings>,
) -> Result<DeviceMetrics, String> {
    let dc = open_printer_dc(printer, settings)?;
    metrics_from_dc(dc.0)
}

#[cfg(target_os = "windows")]
fn metrics_from_dc(hdc: ffi::Hdc) -> Result<DeviceMetrics, String> {
    let caps = |index: i32| unsafe { ffi::GetDeviceCaps(hdc, index) };
    let dpi_x = caps(ffi::LOGPIXELSX);
    let dpi_y = caps(ffi::LOGPIXELSY);
    if dpi_x <= 0 || dpi_y <= 0 {
        return Err("Printer reports no resolution".to_string());
    }
    let printable_w = caps(ffi::HORZRES);
    let printable_h = caps(ffi::VERTRES);
    if printable_w <= 0 || printable_h <= 0 {
        return Err("Printer reports an empty printable area".to_string());
    }
    let printable_x = caps(ffi::PHYSICALOFFSETX).max(0);
    let printable_y = caps(ffi::PHYSICALOFFSETY).max(0);
    // Some virtual drivers report no physical paper; fall back to the
    // printable area plus symmetric offsets so centering still works.
    let mut paper_w = caps(ffi::PHYSICALWIDTH);
    let mut paper_h = caps(ffi::PHYSICALHEIGHT);
    if paper_w <= 0 {
        paper_w = printable_w + 2 * printable_x;
    }
    if paper_h <= 0 {
        paper_h = printable_h + 2 * printable_y;
    }
    Ok(DeviceMetrics {
        dpi_x: dpi_x as f32,
        dpi_y: dpi_y as f32,
        paper_w,
        paper_h,
        printable_x,
        printable_y,
        printable_w,
        printable_h,
    })
}

/// Print `copies` sheets of one image straight to `printer` through GDI.
/// `next_band(y0, rows)` must return a `img_w × rows` straight-alpha RGBA
/// buffer (same contract as the streamed PDF path), so arbitrarily large
/// canvases print without a flat canvas-sized buffer.
#[cfg(target_os = "windows")]
pub fn print_bands(
    printer: &str,
    settings: Option<&PrinterSettings>,
    doc_name: &str,
    img_w: u32,
    img_h: u32,
    dpi: f32,
    layout: &PrintLayout,
    copies: u32,
    mut next_band: impl FnMut(u32, u32) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    if img_w == 0 || img_h == 0 {
        return Err("Empty image, cannot print".to_string());
    }

    let dc = open_printer_dc(printer, settings)?;
    let metrics = metrics_from_dc(dc.0)?;
    let (dest_x, dest_y, dest_w, dest_h) = image_dest_rect(&metrics, layout, img_w, img_h, dpi);
    let (clip_l, clip_t, clip_r, clip_b) = clip_dest_rect(&metrics, layout, img_w, img_h, dpi);

    // Abort the spooler job unless every page ends cleanly.
    struct Job {
        hdc: ffi::Hdc,
        done: bool,
    }
    impl Drop for Job {
        fn drop(&mut self) {
            if !self.done {
                unsafe { ffi::AbortDoc(self.hdc) };
            }
        }
    }
    let name = wide(doc_name);
    let di = ffi::DocInfoW {
        cb_size: std::mem::size_of::<ffi::DocInfoW>() as i32,
        doc_name: name.as_ptr(),
        output: std::ptr::null(),
        datatype: std::ptr::null(),
        fw_type: 0,
    };
    if unsafe { ffi::StartDocW(dc.0, &di) } <= 0 {
        return Err(format!(
            "Printer refused the job: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut job = Job {
        hdc: dc.0,
        done: false,
    };

    for _ in 0..copies.max(1) {
        if unsafe { ffi::StartPage(job.hdc) } <= 0 {
            return Err(format!(
                "StartPage failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        // StartPage resets DC state, so clip/stretch mode go per page.
        unsafe {
            ffi::SetStretchBltMode(job.hdc, ffi::HALFTONE);
            ffi::IntersectClipRect(job.hdc, clip_l, clip_t, clip_r, clip_b);
        }

        let mut y = 0u32;
        while y < img_h {
            let rows = super::print::PDF_STREAM_BAND_ROWS.min(img_h - y);
            let band = next_band(y, rows)?;
            let expected = (img_w as usize) * (rows as usize) * 4;
            if band.len() < expected {
                return Err("Print band buffer too small".to_string());
            }
            let (band_top, band_h) = band_dest_span(dest_y, dest_h, img_h, y, rows);
            if band_h > 0 {
                let dib = rgba_band_to_dib_bgr(&band, img_w, rows);
                let bmi = ffi::BitmapInfoHeader {
                    bi_size: std::mem::size_of::<ffi::BitmapInfoHeader>() as u32,
                    bi_width: img_w as i32,
                    bi_height: rows as i32, // bottom-up
                    bi_planes: 1,
                    bi_bit_count: 24,
                    bi_compression: ffi::BI_RGB,
                    bi_size_image: 0,
                    bi_x_pels_per_meter: 0,
                    bi_y_pels_per_meter: 0,
                    bi_clr_used: 0,
                    bi_clr_important: 0,
                };
                // Returns scan lines copied; 0 is legal when the band is fully
                // clipped (image bleeding past the printable area), so only
                // GDI_ERROR is fatal.
                let r = unsafe {
                    ffi::StretchDIBits(
                        job.hdc,
                        dest_x,
                        band_top,
                        dest_w,
                        band_h,
                        0,
                        0,
                        img_w as i32,
                        rows as i32,
                        dib.as_ptr().cast(),
                        &bmi,
                        ffi::DIB_RGB_COLORS,
                        ffi::SRCCOPY,
                    )
                };
                if r == ffi::GDI_ERROR {
                    return Err("StretchDIBits failed".to_string());
                }
            }
            y += rows;
        }

        if unsafe { ffi::EndPage(job.hdc) } <= 0 {
            return Err(format!(
                "EndPage failed: {}",
                std::io::Error::last_os_error()
            ));
        }
    }

    job.done = true;
    if unsafe { ffi::EndDoc(job.hdc) } <= 0 {
        return Err(format!(
            "EndDoc failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// EPSON photo printer, 10×15 cm paper at 720 dpi with ~3 mm margins —
    /// the exact setup that exposed the Foxit fit-to-margins shrink.
    fn epson_10x15() -> DeviceMetrics {
        DeviceMetrics {
            dpi_x: 720.0,
            dpi_y: 720.0,
            paper_w: 2880,   // 4.0 in
            paper_h: 4320,   // 6.0 in
            printable_x: 86, // 0.12 in
            printable_y: 86,
            printable_w: 2708, // 3.77 in
            printable_h: 4148, // 5.77 in
        }
    }

    #[test]
    fn metrics_convert_to_printer_info_points() {
        let m = epson_10x15();
        assert_eq!(m.paper_points(), (288, 432));
        // 86 px @720dpi = 8.6 pt → rounds to 9; bottom offset mirrors the top.
        assert_eq!(m.printable_rect_points(), (9, 9, 271, 415));
    }

    #[test]
    fn to_layout_mirrors_device_caps() {
        let m = epson_10x15();
        let layout = m.to_layout(&PrintLayout::default());
        let (pw, ph) = layout.page_points.unwrap();
        assert!((pw - 288.0).abs() < 0.01, "paper width should be 4 in");
        assert!((ph - 432.0).abs() < 0.01, "paper height should be 6 in");
        let (x, y, w, h) = layout.printable_rect_points.unwrap();
        assert!((x - 8.6).abs() < 0.01);
        assert!((w - 270.8).abs() < 0.01);
        assert!((h - 414.8).abs() < 0.01);
        // Bottom-left origin: y = page_h - top_offset - printable_h.
        assert!((y - (432.0 - 8.6 - 414.8)).abs() < 0.01);
    }

    #[test]
    fn image_prints_at_exact_physical_size() {
        // 10×15 cm imposition sheet: 2362×3543 px at 600 dpi = 3.937×5.905 in.
        let m = epson_10x15();
        let (x, y, w, h) = image_dest_rect(&m, &PrintLayout::default(), 2362, 3543, 600.0);
        // Physical size preserved: 3.937 in × 720 dpi ≈ 2834 device px.
        assert_eq!(w, 2834);
        assert_eq!(h, 4252);
        // Centered on the *paper*: (2880-2834)/2 = 23 px from the paper edge,
        // minus the 86 px printable offset → negative DC coordinate.
        assert_eq!(x, 23 - 86);
        assert_eq!(y, 34 - 86);
    }

    #[test]
    fn clip_matches_printable_area_when_no_margin() {
        let m = epson_10x15();
        let (l, t, r, b) = clip_dest_rect(&m, &PrintLayout::default(), 2362, 3543, 600.0);
        assert_eq!((l, t), (0, 0));
        assert_eq!((r, b), (2708, 4148));
    }

    #[test]
    fn band_spans_tile_destination_exactly() {
        let (dest_y, dest_h, img_h) = (-63, 4252, 3543u32);
        let mut expected_top = band_dest_span(dest_y, dest_h, img_h, 0, 1).0;
        let mut total = 0;
        let mut y = 0u32;
        while y < img_h {
            let rows = 256.min(img_h - y);
            let (top, h) = band_dest_span(dest_y, dest_h, img_h, y, rows);
            assert_eq!(top, expected_top, "bands must tile without gaps");
            assert!(h >= 0);
            expected_top = top + h;
            total += h;
            y += rows;
        }
        assert_eq!(total, dest_h, "bands must cover the whole destination");
        assert_eq!(expected_top, dest_y + dest_h);
    }

    #[test]
    fn dib_rows_are_dword_aligned() {
        assert_eq!(dib_stride(1), 4);
        assert_eq!(dib_stride(2), 8);
        assert_eq!(dib_stride(4), 12);
        assert_eq!(dib_stride(640), 1920);
    }

    #[test]
    fn dib_conversion_is_bottom_up_bgr_on_white() {
        // 1×2 band: opaque red on top, half-transparent green below.
        let band = [255, 0, 0, 255, 0, 200, 0, 128];
        let dib = rgba_band_to_dib_bgr(&band, 1, 2);
        assert_eq!(dib.len(), 8, "two DWORD-aligned rows");
        // Bottom-up: first stored row = bottom pixel (green over white).
        let g = ((200u16 * 128 + 255 * 127) / 255) as u8;
        let w = ((0u16 * 128 + 255 * 127) / 255) as u8;
        assert_eq!(&dib[0..3], &[w, g, w], "BGR of green composited on white");
        // Second stored row = top pixel, opaque red → BGR (0,0,255).
        assert_eq!(&dib[4..7], &[0, 0, 255]);
    }
}

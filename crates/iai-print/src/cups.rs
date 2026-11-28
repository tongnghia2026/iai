//! Printing on the Mac through CUPS, the printing system macOS ships with:
//! `lpstat` lists the printers and says how they are, `lpoptions` gives a
//! printer's paper, and `lp` takes the job. A page goes out as a one-page PDF
//! the size of the paper with the picture at its true size on it, and CUPS is
//! told not to scale it, so a 3×4 cm photo prints 3×4 cm.
//!
//! The commands run in the C locale: their wording is parsed, and on a Mac
//! set to Vietnamese they would answer in Vietnamese.

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use iai_canvas::print::{PrintLayout, PrinterInfo, build_pdf_encoded, encode_pdf_page_streamed};

use crate::print_gdi::{PrintProgress, PrintStage, PrinterState};

/// What is said when the Mac has no printer at all.
pub const NO_PRINTER: &str = "Máy Mac này chưa có máy in nào. Mở Cài đặt hệ thống > Máy in & \
Máy quét để thêm máy in, rồi bấm Refresh trong hộp In.";

/// How long `lp` may take to hand a job to CUPS before the print is given up.
const LP_TIMEOUT: Duration = Duration::from_secs(60);

/// A CUPS command in the C locale.
fn cups(program: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.env("LC_ALL", "C").env("LANG", "C").stdin(Stdio::null());
    cmd
}

fn run(cmd: &mut Command, what: &str) -> Result<Output, String> {
    cmd.output()
        .map_err(|e| format!("Không chạy được {what} của hệ thống in: {e}"))
}

/// The printers `lpstat -p -d` names, and the default one.
pub fn parse_lpstat(text: &str) -> (Vec<String>, Option<String>) {
    let mut names = Vec::new();
    let mut default = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("printer ") {
            if let Some(name) = rest.split_whitespace().next() {
                names.push(name.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("system default destination:") {
            let name = rest.trim();
            if !name.is_empty() {
                default = Some(name.to_string());
            }
        }
    }
    (names, default)
}

/// The paper a printer prints on unless told otherwise: the starred choice
/// of `PageSize` in `lpoptions -p <printer> -l`.
pub fn parse_default_page_size(text: &str) -> Option<String> {
    let line = text
        .lines()
        .find(|l| l.starts_with("PageSize/") || l.starts_with("media/"))?;
    let (_, choices) = line.split_once(':')?;
    choices
        .split_whitespace()
        .find_map(|c| c.strip_prefix('*'))
        .map(str::to_string)
}

/// The size of a paper named the PPD way, in points: from the printer's PPD
/// when it has the name (`*PaperDimension`), else from the usual names.
pub fn paper_points(size: &str, ppd: Option<&str>) -> Option<(f32, f32)> {
    if let Some(ppd) = ppd
        && let Some(value) = ppd_value(ppd, "*PaperDimension", size)
    {
        let mut nums = value
            .split_whitespace()
            .filter_map(|v| v.parse::<f32>().ok());
        if let (Some(w), Some(h)) = (nums.next(), nums.next()) {
            return Some((w, h));
        }
    }
    known_paper(size)
}

/// The printable area of a paper, `(x, y, w, h)` in points from its
/// bottom-left corner, from the PPD's `*ImageableArea`.
pub fn printable_points(size: &str, ppd: &str) -> Option<(f32, f32, f32, f32)> {
    let value = ppd_value(ppd, "*ImageableArea", size)?;
    let nums: Vec<f32> = value
        .split_whitespace()
        .filter_map(|v| v.parse::<f32>().ok())
        .collect();
    match nums[..] {
        [llx, lly, urx, ury] if urx > llx && ury > lly => Some((llx, lly, urx - llx, ury - lly)),
        _ => None,
    }
}

/// The quoted value of `*Keyword Option/Label: "value"` in a PPD.
fn ppd_value<'a>(ppd: &'a str, keyword: &str, option: &str) -> Option<&'a str> {
    ppd.lines().find_map(|line| {
        let rest = line.strip_prefix(keyword)?.strip_prefix(' ')?;
        let (key, value) = rest.split_once(':')?;
        let name = key.split('/').next()?.trim();
        (name == option).then(|| value.trim().trim_matches('"'))
    })
}

/// Common paper names: ISO, US and photo sizes, a borderless or other
/// variant (`A4.Borderless`) as its base size, and CUPS's own `w288h432`.
fn known_paper(size: &str) -> Option<(f32, f32)> {
    let base = size.split('.').next().unwrap_or(size);
    let mm = |w: f32, h: f32| Some((w * 72.0 / 25.4, h * 72.0 / 25.4));
    let inch = |w: f32, h: f32| Some((w * 72.0, h * 72.0));
    match base.to_ascii_lowercase().as_str() {
        "a3" => mm(297.0, 420.0),
        "a4" | "iso_a4_210x297mm" => mm(210.0, 297.0),
        "a5" | "iso_a5_148x210mm" => mm(148.0, 210.0),
        "a6" | "iso_a6_105x148mm" => mm(105.0, 148.0),
        "b5" | "iso_b5_176x250mm" => mm(176.0, 250.0),
        "letter" | "na_letter_8.5x11in" => inch(8.5, 11.0),
        "legal" | "na_legal_8.5x14in" => inch(8.5, 14.0),
        "4x6" | "na_index-4x6_4x6in" => inch(4.0, 6.0),
        "5x7" | "na_5x7_5x7in" => inch(5.0, 7.0),
        "3.5x5" | "l" => inch(3.5, 5.0),
        other => {
            let rest = other.strip_prefix('w')?;
            let (w, h) = rest.split_once('h')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        }
    }
}

/// The printer's PPD, when this user may read it.
fn read_ppd(printer: &str) -> Option<String> {
    if printer.contains('/') {
        return None;
    }
    std::fs::read_to_string(Path::new("/etc/cups/ppd").join(format!("{printer}.ppd"))).ok()
}

/// A printer with its current paper, as the Print dialog lays pages on it.
pub fn printer_info(name: &str, is_default: bool) -> PrinterInfo {
    let size = default_page_size(name);
    let ppd = read_ppd(name);
    let paper = size
        .as_deref()
        .and_then(|s| paper_points(s, ppd.as_deref()));
    let printable = size
        .as_deref()
        .zip(ppd.as_deref())
        .and_then(|(s, ppd)| printable_points(s, ppd));
    let whole = |v: f32| v.round().max(0.0) as u32;
    PrinterInfo {
        name: name.to_string(),
        is_default,
        paper_points: paper.map(|(w, h)| (whole(w).max(1), whole(h).max(1))),
        printable_rect_points: printable
            .map(|(x, y, w, h)| (whole(x), whole(y), whole(w).max(1), whole(h).max(1))),
    }
}

fn default_page_size(printer: &str) -> Option<String> {
    let out = cups("lpoptions")
        .args(["-p", printer, "-l"])
        .output()
        .ok()?;
    parse_default_page_size(&String::from_utf8_lossy(&out.stdout))
}

/// The Mac's printers, the default one first. None at all is no error: the
/// list is empty and the dialog says so.
pub fn available_printers() -> Result<Vec<PrinterInfo>, String> {
    let out = run(cups("lpstat").args(["-p", "-d"]), "lpstat")?;
    let (names, default) = parse_lpstat(&String::from_utf8_lossy(&out.stdout));
    let mut printers: Vec<PrinterInfo> = names
        .iter()
        .map(|name| printer_info(name, default.as_deref() == Some(name.as_str())))
        .collect();
    printers.sort_by(|a, b| b.is_default.cmp(&a.is_default).then(a.name.cmp(&b.name)));
    Ok(printers)
}

/// What the printer's alerts and state words mean, in the user's words.
pub fn troubles(lpstat_long: &str) -> Vec<&'static str> {
    const WORDS: [(&str, &str); 10] = [
        ("offline", "mất kết nối (offline)"),
        ("connecting-to-device", "chưa kết nối được tới máy in"),
        ("paused", "đang tạm dừng"),
        ("media-empty", "hết giấy"),
        ("media-needed", "hết giấy"),
        ("media-jam", "kẹt giấy"),
        ("door-open", "nắp máy đang mở"),
        ("cover-open", "nắp máy đang mở"),
        ("toner-empty", "hết mực"),
        ("marker-supply-empty", "hết mực"),
    ];
    let text = lpstat_long.to_ascii_lowercase();
    let mut found: Vec<&'static str> = Vec::new();
    let first = text.lines().next().unwrap_or("");
    if first.contains(" disabled") {
        found.push("đang tạm dừng");
    }
    for line in text.lines() {
        let line = line.trim();
        let Some(alerts) = line.strip_prefix("alerts:") else {
            continue;
        };
        for (word, said) in WORDS {
            if alerts.contains(word) && !found.contains(&said) {
                found.push(said);
            }
        }
    }
    found
}

/// How CUPS says the printer is: its troubles, the jobs waiting, its address.
pub fn printer_state(printer: &str) -> Option<PrinterState> {
    let out = cups("lpstat").args(["-l", "-p", printer]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let queued_jobs = cups("lpstat")
        .args(["-o", printer])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().count() as u32)
        .unwrap_or(0);
    let port = cups("lpstat")
        .args(["-v", printer])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .and_then(|l| l.split_once(": ").map(|(_, uri)| uri.trim().to_string()))
        })
        .unwrap_or_default();
    Some(PrinterState {
        troubles: troubles(&text),
        queued_jobs,
        port,
    })
}

/// What `lp` said, in the user's words.
pub fn lp_error(printer: &str, stderr: &str) -> String {
    let said = stderr.to_ascii_lowercase();
    if said.contains("no default destination") {
        NO_PRINTER.to_string()
    } else if said.contains("does not exist") || said.contains("no such") {
        format!(
            "Không tìm thấy máy in «{printer}» (máy in đã bị gỡ khỏi máy?). Bấm Refresh trong hộp In rồi chọn lại."
        )
    } else if said.contains("not accepting") {
        format!(
            "Máy in «{printer}» đang không nhận lệnh in. Mở Cài đặt hệ thống > Máy in & Máy quét để bật lại."
        )
    } else {
        let detail = stderr.trim();
        if detail.is_empty() {
            format!("Máy in «{printer}» không nhận lệnh in.")
        } else {
            format!("Máy in «{printer}» không nhận lệnh in: {detail}")
        }
    }
}

/// The `lp` arguments for one job: the printer, copies, title, paper, and
/// no scaling, then the file.
pub fn lp_args(
    printer: &str,
    copies: u32,
    title: &str,
    media: Option<&str>,
    file: &Path,
) -> Vec<String> {
    let mut args = Vec::new();
    if !printer.trim().is_empty() {
        args.extend(["-d".to_string(), printer.to_string()]);
    }
    args.extend(["-n".to_string(), copies.max(1).to_string()]);
    if !title.trim().is_empty() {
        args.extend(["-t".to_string(), title.to_string()]);
    }
    if let Some(media) = media {
        args.extend(["-o".to_string(), format!("media={media}")]);
    }
    // The page is the paper with the picture at its size on it: print it as
    // it is (`print-scaling` for IPP printers, `nofit-to-page` for the rest).
    args.extend([
        "-o".to_string(),
        "print-scaling=none".to_string(),
        "-o".to_string(),
        "nofit-to-page".to_string(),
    ]);
    args.push(file.to_string_lossy().into_owned());
    args
}

/// The page laid on `info`'s paper: its size and printable area replace
/// what the layout was given; margin, centring and intent are the user's.
pub fn layout_on(info: &PrinterInfo, user: &PrintLayout) -> PrintLayout {
    PrintLayout {
        page_points: info
            .paper_points
            .map(|(w, h)| (w as f32, h as f32))
            .or(user.page_points),
        printable_rect_points: info
            .printable_rect_points
            .map(|(x, y, w, h)| (x as f32, y as f32, w as f32, h as f32))
            .or(user.printable_rect_points),
        ..*user
    }
}

/// A file for one job's PDF, apart from every other job's.
fn job_file() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("iai_print_{}_{n}.pdf", std::process::id()))
}

/// Removes the job's file however the job ends.
struct TempFile(std::path::PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Print one page: the picture (`img_w`×`img_h` at `dpi`, pulled in bands
/// from `next_band`) on `printer`'s paper as `layout` places it, `copies`
/// times. The printer is asked first: one out of paper, jammed, paused or
/// unplugged gets no job and the user is told why.
#[allow(clippy::too_many_arguments)]
pub fn print_bands(
    printer: &str,
    doc_name: &str,
    img_w: u32,
    img_h: u32,
    dpi: f32,
    layout: &PrintLayout,
    copies: u32,
    progress: &PrintProgress,
    mut next_band: impl FnMut(u32, u32) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    if img_w == 0 || img_h == 0 {
        return Err("Trang trống, không có gì để in".to_string());
    }
    if printer.trim().is_empty() {
        return Err(NO_PRINTER.to_string());
    }
    progress.enter(PrintStage::OpeningDevice)?;
    if let Some(state) = printer_state(printer)
        && !state.troubles.is_empty()
    {
        return Err(format!(
            "Máy in «{printer}» {}: chưa gửi lệnh in. Kiểm tra máy in rồi in lại.",
            state.troubles.join(", ")
        ));
    }
    let info = printer_info(printer, false);
    let layout = layout_on(&info, layout);

    progress.enter(PrintStage::SendingPage)?;
    let page = encode_pdf_page_streamed(img_w, img_h, dpi, |y, rows| {
        progress.step()?;
        next_band(y, rows)
    })?;
    let pdf = build_pdf_encoded(&page, &layout, None)?;
    let file = TempFile(job_file());
    std::fs::write(&file.0, &pdf).map_err(|e| format!("Không tạo được file tạm để in: {e}"))?;

    progress.enter(PrintStage::StartingJob)?;
    let media = default_page_size(printer);
    let args = lp_args(printer, copies, doc_name, media.as_deref(), &file.0);
    let mut child = cups("lp")
        .args(&args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Không chạy được lệnh in (lp) của hệ thống: {e}"))?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| format!("Lệnh in (lp) dừng giữa chừng: {e}"))?
        {
            break status;
        }
        if let Err(stopped) = progress.step() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(stopped);
        }
        if started.elapsed() > LP_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "Hệ thống in không nhận lệnh in tới «{printer}» sau {} giây. Kiểm tra máy in rồi in lại.",
                LP_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    progress.enter(PrintStage::FinishingJob)?;
    if status.success() {
        return Ok(());
    }
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        use std::io::Read;
        let _ = pipe.read_to_string(&mut stderr);
    }
    Err(lp_error(printer, &stderr))
}

/// The Mac keeps printer settings in System Settings > Printers & Scanners.
pub fn open_printer_settings() -> Result<(), String> {
    Command::new("open")
        .arg("x-apple.systempreferences:com.apple.Print-Scanners-Settings.extension")
        .status()
        .map_err(|e| format!("Không mở được Cài đặt máy in: {e}"))
        .and_then(|s| {
            if s.success() {
                Ok(())
            } else {
                Err("Không mở được Cài đặt máy in".to_string())
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_printers_and_the_default_are_read_from_lpstat() {
        let text = "printer HP_LaserJet_M15w is idle.  enabled since Mon Nov 24 09:00:00 2026\n\
                    printer Canon_G3010 disabled since Mon Nov 24 09:00:00 2026 -\n\
                    \treason unknown\n\
                    system default destination: Canon_G3010\n";
        let (names, default) = parse_lpstat(text);
        assert_eq!(names, ["HP_LaserJet_M15w", "Canon_G3010"]);
        assert_eq!(default.as_deref(), Some("Canon_G3010"));
        let (names, default) = parse_lpstat("no system default destination\n");
        assert!(names.is_empty() && default.is_none());
    }

    #[test]
    fn the_default_paper_is_the_starred_page_size() {
        let text = "ColorModel/Color Mode: *RGB Gray\n\
                    PageSize/Media Size: Letter Legal *A4 A5 4x6.Borderless\n\
                    Duplex/2-Sided: *None\n";
        assert_eq!(parse_default_page_size(text).as_deref(), Some("A4"));
        assert_eq!(parse_default_page_size("Duplex/2-Sided: *None"), None);
    }

    #[test]
    fn a_paper_is_measured_from_the_ppd_or_its_name() {
        let ppd = "*PaperDimension A4/A4: \"595.28 841.89\"\n\
                   *ImageableArea A4/A4: \"12 12 583.28 829.89\"\n\
                   *PaperDimension 4x6.Borderless/4x6 Borderless: \"288 432\"\n";
        assert_eq!(paper_points("A4", Some(ppd)), Some((595.28, 841.89)));
        assert_eq!(
            printable_points("A4", ppd),
            Some((12.0, 12.0, 571.28, 817.89))
        );
        assert_eq!(
            paper_points("4x6.Borderless", Some(ppd)),
            Some((288.0, 432.0))
        );
        // No PPD: the usual names, CUPS's own `wNNNhNNN`.
        let (w, h) = paper_points("A4", None).unwrap_or_default();
        assert!((w - 595.28).abs() < 0.01 && (h - 841.89).abs() < 0.01);
        assert_eq!(paper_points("Letter", None), Some((612.0, 792.0)));
        assert_eq!(paper_points("w288h432", None), Some((288.0, 432.0)));
        assert_eq!(paper_points("Mystery", None), None);
    }

    #[test]
    fn the_page_goes_out_unscaled_on_the_chosen_printer() {
        let args = lp_args(
            "Canon_G3010",
            3,
            "anh the",
            Some("A4"),
            Path::new("/tmp/x.pdf"),
        );
        assert_eq!(
            args,
            [
                "-d",
                "Canon_G3010",
                "-n",
                "3",
                "-t",
                "anh the",
                "-o",
                "media=A4",
                "-o",
                "print-scaling=none",
                "-o",
                "nofit-to-page",
                "/tmp/x.pdf"
            ]
        );
        // No copies is one copy; no printer is the default one.
        let args = lp_args("", 0, "", None, Path::new("/tmp/x.pdf"));
        assert_eq!(args[..2], ["-n", "1"]);
    }

    #[test]
    fn what_the_printer_says_is_put_in_the_users_words() {
        let paused = "printer Canon disabled since Mon Nov 24 2026 -\n\tPaused\n\tAlerts: media-empty-error media-jam-warning\n";
        assert_eq!(troubles(paused), ["đang tạm dừng", "hết giấy", "kẹt giấy"]);
        let fine = "printer Canon is idle.  enabled since Mon Nov 24 2026\n\tAlerts: none\n";
        assert!(troubles(fine).is_empty());
        assert!(lp_error("X", "lp: Error - No default destination.").contains("chưa có máy in"));
        assert!(
            lp_error("X", "lp: The printer or class does not exist.")
                .contains("Không tìm thấy máy in «X»")
        );
    }

    #[test]
    fn a_photo_lands_at_its_true_size_on_the_printers_paper() {
        // A 3×4 cm ID photo at 300 dpi, on A4 with a 4.2 mm printable margin.
        let (w, h, dpi) = (354u32, 472u32, 300.0f32);
        let info = PrinterInfo {
            name: "P".into(),
            is_default: true,
            paper_points: Some((595, 842)),
            printable_rect_points: Some((12, 12, 571, 818)),
        };
        let layout = layout_on(&info, &PrintLayout::default());
        let (dw, dh, x, y) = iai_canvas::print::placement(&layout, w, h, dpi);
        let to_mm = |pt: f32| pt * 25.4 / 72.0;
        assert!((to_mm(dw) - 29.97).abs() < 0.05, "width {} mm", to_mm(dw));
        assert!((to_mm(dh) - 39.96).abs() < 0.05, "height {} mm", to_mm(dh));
        // Centred on the printable area.
        assert!((x + dw / 2.0 - (12.0 + 571.0 / 2.0)).abs() < 0.01);
        assert!((y + dh / 2.0 - (12.0 + 818.0 / 2.0)).abs() < 0.01);
        // And the PDF's page is the paper.
        let page =
            encode_pdf_page_streamed(w, h, dpi, |_, rows| Ok(vec![200; (w * rows * 4) as usize]));
        let pdf = page.and_then(|p| build_pdf_encoded(&p, &layout, None));
        let text = String::from_utf8_lossy(pdf.as_deref().unwrap_or_default()).into_owned();
        assert!(
            text.contains("/MediaBox [0 0 595.00 842.00]"),
            "the paper's page"
        );
        // The picture drawn 84.96 × 113.28 pt: 29.97 × 39.96 mm.
        assert!(
            text.contains("84.96 0 0 113.28"),
            "the picture's size on it"
        );
    }
}

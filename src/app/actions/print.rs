//! Printer-list plumbing for the Print dialog. Split out of app/actions.rs.

use crate::app::state::App;

impl App {
    /// Apply the result of the native printer property sheet without ever
    /// blocking/re-entering winit's UI thread.
    pub(super) fn poll_printer_settings(&mut self) {
        use crate::app::jobs::Polled;
        let Some(job) = self.jobs.pending_printer_settings.take() else {
            return;
        };

        match job.poll() {
            Polled::Done((printer, Ok(Some(settings)))) => {
                // The native sheet is modal, but keep this guard so a stale
                // worker result can never be applied to a newly selected device.
                if printer == self.shell.print_selected_printer {
                    self.shell.print_driver_settings = Some(settings);
                    self.shell.status_msg =
                        format!("Printer settings applied to this IAI print only: {printer}");
                    self.refresh_selected_printer();
                }
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            Polled::Done((_printer, Ok(None))) => {
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            Polled::Done((_printer, Err(e))) => {
                self.shell.status_msg = e;
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            Polled::Pending => {
                self.jobs.pending_printer_settings = Some(job);
                return;
            }
            Polled::Failed(why) => {
                self.shell.status_msg =
                    format!("Printer settings dialog stopped unexpectedly: {why}");
            }
        }
        // The sheet is gone. Run on its own thread, it may have closed
        // without handing the keyboard back (the TOSHIBA driver's does): no
        // Focused(true) then comes, and the canvas stays deaf to the pointer.
        if let Some(held) = self
            .win
            .window
            .as_ref()
            .and_then(|w| crate::app::cursor::take_keyboard_back(w))
        {
            crate::diag::note("window", &format!("printer settings closed: {held}"));
        }
    }

    /// Hear from the print worker. A sheet that went out closes the dialog;
    /// one the device refused goes round again, next frame, through the PDF
    /// handler (`handle_color_print_actions` takes `print_gdi_failed`).
    ///
    /// A worker that stands still is inside a driver call the printer does not
    /// answer (switched off, out of paper, a message of the driver's nobody
    /// sees). The user is told once; from then on the job holds up neither the
    /// app's exit nor, once stopped, another print.
    pub(in crate::app) fn poll_print_job(&mut self) {
        let Some(mut job) = self.jobs.pending_print.take() else {
            // The next sheet in turn, unless a refused one is on its way
            // round through the PDF route first.
            if self.jobs.print_gdi_failed.is_none()
                && let Some(task) = self.jobs.print_queue.pop_front()
            {
                self.start_print_task(task);
            }
            return;
        };
        match job.rx.try_recv() {
            Ok(Ok(())) => {
                let waiting = self.jobs.print_queue.len();
                self.shell.status_msg = if waiting == 0 {
                    self.print_done_note(None)
                } else {
                    format!("Đã gửi lệnh in — còn {waiting} lệnh đang chờ gửi")
                };
                if let Some(task) = self.jobs.print_queue.pop_front() {
                    self.start_print_task(task);
                }
            }
            // A PDF page that could not be rendered (its file moved, say):
            // that page is left out, the rest of the print goes on, and what
            // was left out is said once the last sheet has gone.
            Ok(Err(e))
                if job.progress.stage() == crate::core::print_gdi::PrintStage::RenderingPage =>
            {
                crate::diag::note("print", &format!("PDF page not rendered: {e}"));
                self.jobs
                    .print_skipped
                    .push(if job.page_of.label.is_empty() {
                        "trang PDF".to_string()
                    } else {
                        job.page_of.label.clone()
                    });
                match self.jobs.print_queue.pop_front() {
                    Some(task) => self.start_print_task(task),
                    None => self.shell.status_msg = self.print_done_note(Some(&e)),
                }
            }
            // The Mac has no other way to the printer than CUPS: what it
            // said is told, and the sheets behind it are not sent to the
            // same printer.
            #[cfg(target_os = "macos")]
            Ok(Err(e)) => {
                crate::diag::note("print", &format!("{}: {e}", job.printer));
                let dropped = self.jobs.print_queue.len();
                self.jobs.print_queue.clear();
                self.jobs.print_skipped.clear();
                self.shell.status_msg = if dropped == 0 {
                    e
                } else {
                    format!("{e} ({dropped} tờ còn lại chưa in)")
                };
            }
            #[cfg(not(target_os = "macos"))]
            Ok(Err(e)) => {
                self.jobs.print_gdi_failed = Some(e);
                self.jobs.print_failed_doc = Some(job.page_of);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                job.watch();
                let tell = job.is_stuck() && !job.told_stuck;
                if tell {
                    job.told_stuck = true;
                    crate::diag::note(
                        "print",
                        &format!(
                            "{}: no step for {} s while {}",
                            job.printer,
                            job.still_for().as_secs(),
                            job.progress.stage().label()
                        ),
                    );
                    self.shell.status_msg = format!(
                        "Máy in «{}» chưa nhận lệnh in{} — kiểm tra máy in, hoặc mở Print (Ctrl+P) bấm «Dừng gửi» để in bằng máy khác",
                        job.printer,
                        job.printer_says()
                    );
                }
                self.jobs.pending_print = Some(job);
                if tell && let Some(w) = &self.win.window {
                    w.request_redraw();
                }
                return;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.jobs.print_gdi_failed = Some("the print worker stopped".to_string());
            }
        }
        // The frame that shows this must be a full one, not a reuse of the
        // last UI under the marching ants.
        self.win.ants_redraw_pending = false;
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Stop waiting for the print on its way. The worker is told to abort at
    /// its next step; one that is inside the driver cannot be reached, and is
    /// left there for the printer to release.
    pub(in crate::app) fn stop_print_job(&mut self) {
        let Some(job) = self.jobs.pending_print.take() else {
            return;
        };
        job.progress.cancel();
        crate::diag::note(
            "print",
            &format!(
                "{}: stopped by the user after {} s, while {}",
                job.printer,
                job.started.elapsed().as_secs(),
                job.progress.stage().label()
            ),
        );
        // What waited behind it would wait on the same printer.
        let dropped = self.jobs.print_queue.len();
        self.jobs.print_queue.clear();
        self.jobs.print_skipped.clear();
        self.shell.status_msg = if dropped == 0 {
            format!("Đã dừng gửi lệnh in tới «{}»", job.printer)
        } else {
            format!(
                "Đã dừng gửi lệnh in tới «{}» và bỏ {dropped} lệnh đang chờ",
                job.printer
            )
        };
        self.win.ants_redraw_pending = false;
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// What the status line says once the last sheet of a print has gone:
    /// the pages left out (could not be rendered) too, with why when known.
    fn print_done_note(&mut self, why: Option<&str>) -> String {
        let skipped = std::mem::take(&mut self.jobs.print_skipped);
        if skipped.is_empty() {
            return "Đã gửi lệnh in".to_string();
        }
        const SHOWN: usize = 5;
        let mut pages = skipped
            .iter()
            .take(SHOWN)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if skipped.len() > SHOWN {
            pages.push_str(&format!("… ({} trang)", skipped.len()));
        }
        let why = why.unwrap_or("không dựng được trang để in");
        format!("Không in được {pages}: {why}")
    }

    /// The sheets Print sends for the tab in front, in order. A page is
    /// printed as export writes it (over its master page, when it shows one);
    /// in an opened PDF, as the tab shows it, and a page never shown is
    /// rendered by the worker that prints it. Several pages with several
    /// copies go out a whole set at a time (1, 2, 3, 1, 2, 3), as Acrobat
    /// collates them.
    #[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
    pub(in crate::app) fn print_tasks(
        &self,
        pages: &crate::ui::PrintPages,
        layout: crate::core::print::PrintLayout,
        profile: Option<Vec<u8>>,
    ) -> Vec<crate::app::background_jobs::PrintTask> {
        use crate::app::background_jobs::{PrintTask, PrintedPage};
        let doc = self.docs.active();
        let (name, sheets) = match doc.pdf_document.as_ref() {
            Some(pdf) => (
                pdf.source
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| doc.title.clone()),
                pdf_print_sheets(doc, pdf, pages),
            ),
            None => (doc.title.clone(), artboard_print_sheets(doc, pages)),
        };
        let several = sheets.len() > 1;
        let copies = self.shell.print_copies.max(1);
        let (sets, copies_each) = if several { (copies, 1) } else { (1, copies) };
        let mut tasks = Vec::with_capacity(sheets.len().saturating_mul(sets as usize));
        for set in 1..=sets {
            for page in &sheets {
                let label = match (several, sets > 1) {
                    (false, _) => String::new(),
                    (true, false) => format!("trang {}", page.number),
                    (true, true) => format!("trang {} (bộ {set}/{sets})", page.number),
                };
                tasks.push(PrintTask {
                    of: PrintedPage {
                        doc: doc.id,
                        page: page.artboard,
                        pdf_page: page.pdf_page,
                        label,
                    },
                    doc_name: if several || page.pdf_page.is_some() {
                        format!("{name} — trang {}", page.number)
                    } else {
                        name.clone()
                    },
                    sheet: page.sheet.clone(),
                    layout,
                    profile: profile.clone(),
                    printer: self.shell.print_selected_printer.clone(),
                    settings: self.shell.print_driver_settings.clone(),
                    copies: copies_each,
                });
            }
        }
        tasks
    }

    /// Send `task` now, or after the sheet already on its way.
    #[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
    pub(in crate::app) fn queue_print_task(
        &mut self,
        task: crate::app::background_jobs::PrintTask,
    ) {
        if self.jobs.pending_print.is_some() || self.jobs.print_gdi_failed.is_some() {
            self.jobs.print_queue.push_back(task);
            self.shell.status_msg = format!(
                "Đã xếp lệnh in — {} lệnh đang chờ gửi",
                self.jobs.print_queue.len()
            );
        } else {
            self.start_print_task(task);
        }
    }

    /// Hand `task` to a print worker: it composites the page, converts it to
    /// the printer's profile when there is one, and sends it through GDI.
    /// `poll_print_job` hears how it went.
    #[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
    pub(in crate::app) fn start_print_task(
        &mut self,
        task: crate::app::background_jobs::PrintTask,
    ) {
        let crate::app::background_jobs::PrintTask {
            of,
            doc_name,
            sheet,
            layout,
            profile,
            printer,
            settings,
            copies,
        } = task;
        let label = of.label.clone();
        let intent = layout.intent.to_lcms();
        let (tx, rx) = std::sync::mpsc::channel();
        // Each stage goes to the journal as the worker enters it: the last one
        // written is where a printer that never answered left it.
        let journal_printer = printer.clone();
        let progress = std::sync::Arc::new(crate::core::print_gdi::PrintProgress::reporting_to(
            move |stage| {
                crate::diag::note("print", &format!("{journal_printer}: {}", stage.label()));
            },
        ));
        let printer_state = std::sync::Arc::new(std::sync::OnceLock::new());
        self.jobs.pending_print = Some(crate::app::background_jobs::PrintJob::new(
            printer.clone(),
            of,
            rx,
            std::sync::Arc::clone(&progress),
            std::sync::Arc::clone(&printer_state),
        ));
        let waiting = self.jobs.print_queue.len();
        self.shell.status_msg = match (label.is_empty(), waiting) {
            (true, 0) => "Đang gửi lệnh in…".to_string(),
            (true, _) => format!("Đang gửi lệnh in… (còn {waiting} lệnh chờ)"),
            (false, 0) => format!("Đang in {label}…"),
            (false, _) => format!("Đang in {label}… (còn {waiting} tờ chờ)"),
        };
        std::thread::spawn(move || {
            // A PDF page never shown is rendered here first, off the window's
            // thread; it is in memory only while it prints.
            let (mut stack, cw, ch, dpi) = match sheet {
                crate::app::background_jobs::PrintSheet::Layers {
                    stack,
                    width,
                    height,
                    dpi,
                } => (stack, width, height, dpi),
                crate::app::background_jobs::PrintSheet::PdfPage { source, index } => {
                    let rendered = progress
                        .enter(crate::core::print_gdi::PrintStage::RenderingPage)
                        .and_then(|()| {
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                source.render(index)
                            }))
                            .unwrap_or_else(|_| Err("the PDF renderer stopped".to_string()))
                        });
                    match rendered {
                        Ok(page) => page,
                        Err(e) => {
                            let _ = tx.send(Err(e));
                            return;
                        }
                    }
                }
            };
            let _ = progress.enter(crate::core::print_gdi::PrintStage::ReadingState);
            if let Some(state) = crate::core::print_gdi::printer_state(&printer) {
                crate::diag::note(
                    "print",
                    &format!(
                        "{printer} on {}: {} job(s) queued, troubles {:?}",
                        state.port, state.queued_jobs, state.troubles
                    ),
                );
                let _ = printer_state.set(state);
            }
            // The page composited here, off the window's thread: small pages
            // whole (as the window's own flat buffer is made), large ones a
            // band at a time.
            let mut flat = crate::core::canvas::Canvas::fits_flat_buffer(cw, ch)
                .then(|| stack.flatten(cw, ch));
            if let (Some(rgba), Some(pp)) = (flat.as_mut(), &profile) {
                crate::core::cms::convert_srgb_to_rgb_profile(rgba, pp, intent);
            }
            let result = crate::core::print_gdi::print_bands(
                &printer,
                settings.as_ref(),
                &doc_name,
                cw,
                ch,
                dpi,
                &layout,
                copies,
                &progress,
                move |y, rows| match &flat {
                    Some(flat) => {
                        let start = (y as usize) * (cw as usize) * 4;
                        let len = (rows as usize) * (cw as usize) * 4;
                        flat.get(start..start + len)
                            .map(<[u8]>::to_vec)
                            .ok_or_else(|| "the page is smaller than its bands".to_string())
                    }
                    None => {
                        let mut band = stack.flatten_band(cw, ch, y, rows);
                        if let Some(pp) = &profile {
                            crate::core::cms::convert_srgb_to_rgb_profile(&mut band, pp, intent);
                        }
                        Ok(band)
                    }
                },
            );
            crate::diag::note(
                "print",
                &match &result {
                    Ok(()) => format!("{printer}: the job is with the spooler"),
                    Err(e) => format!("{printer}: {e}"),
                },
            );
            let _ = tx.send(result);
        });
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Start the selected driver's property sheet away from the winit thread.
    /// The HWND remains the native owner, so Windows keeps the sheet in front
    /// and restores focus correctly when it closes.
    pub(crate) fn open_printer_settings_async(&mut self, owner_hwnd: isize) {
        if self.jobs.pending_printer_settings.is_some() {
            return;
        }
        let printer = self.shell.print_selected_printer.clone();
        let settings = self.shell.print_driver_settings.clone();
        let worker_printer = printer.clone();
        self.shell.status_msg = format!("Opening printer settings: {printer}");

        // Reset before the worker creates the native property sheet.  Waiting
        // for a later redraw leaves a small race where Windows can inherit the
        // active brush/pen/tool cursor (or the hidden-cursor state).
        if let Some(w) = &self.win.window {
            w.set_cursor_visible(true);
            w.set_cursor(winit::window::CursorIcon::Default);
        }

        let job = crate::app::jobs::Job::spawn("printer-settings", move || {
            let result = crate::core::printers::open_printer_settings(
                &worker_printer,
                settings.as_ref(),
                owner_hwnd,
            );
            (worker_printer, result)
        });
        self.jobs.pending_printer_settings = Some(job);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    #[cfg_attr(target_os = "macos", allow(unused_mut))]
    pub(super) fn apply_printer_list(
        &mut self,
        mut printers: Vec<crate::core::print::PrinterInfo>,
    ) {
        if self.shell.print_selected_printer.is_empty()
            || !printers
                .iter()
                .any(|p| p.name == self.shell.print_selected_printer)
        {
            self.shell.print_selected_printer = printers
                .iter()
                .find(|p| p.is_default)
                .or_else(|| printers.first())
                .map(|p| p.name.clone())
                .unwrap_or_default();
        }
        #[cfg(target_os = "windows")]
        if let Some(settings) = self.shell.print_driver_settings.as_ref() {
            let name = &self.shell.print_selected_printer;
            if settings.matches_printer(name)
                && let Ok(updated) =
                    crate::core::printers::query_printer_with_settings(name, Some(settings))
                && let Some(slot) = printers.iter_mut().find(|p| p.name == *name)
            {
                slot.paper_points = updated.paper_points;
                slot.printable_rect_points = updated.printable_rect_points;
            }
        }
        self.shell.print_printers = printers;
        if self.shell.print_printers.is_empty() {
            #[cfg(target_os = "macos")]
            {
                self.shell.status_msg = iai_print::cups::NO_PRINTER.to_string();
            }
            #[cfg(not(target_os = "macos"))]
            {
                self.shell.status_msg = "No printers found".to_string();
            }
        }
    }

    pub(super) fn poll_printer_refresh(&mut self) {
        use crate::app::jobs::Polled;
        let Some(job) = self.jobs.pending_printer_refresh.take() else {
            return;
        };

        match job.poll() {
            Polled::Done(Ok(printers)) => {
                self.apply_printer_list(printers);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            Polled::Done(Err(e)) => {
                self.shell.print_printers.clear();
                self.shell.status_msg = e;
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            Polled::Pending => {
                self.jobs.pending_printer_refresh = Some(job);
                if self.shell.ui.show_print_dialog
                    && let Some(w) = &self.win.window
                {
                    w.request_redraw();
                }
                return;
            }
            Polled::Failed(why) => {
                self.shell.status_msg = format!("Printer refresh stopped: {why}");
            }
        }

        // A selection or settings change that arrived mid-query ran against the
        // old printer; re-read the printer that is selected now.
        if std::mem::take(&mut self.jobs.printer_refresh_queued) {
            self.refresh_selected_printer();
        }
    }

    pub(crate) fn refresh_printer_list(&mut self) {
        if self.jobs.pending_printer_refresh.is_some() {
            return;
        }
        self.jobs.pending_printer_refresh = Some(crate::app::jobs::Job::spawn(
            "printers",
            crate::core::printers::available_printers,
        ));
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Re-read just the selected printer's paper geometry (its driver defaults
    /// may have changed in Print Settings). One driver DC query instead of a
    /// full enumeration, so the preview updates in milliseconds; on failure the
    /// list simply stays as it was.
    pub(crate) fn refresh_selected_printer(&mut self) {
        #[cfg(not(target_os = "windows"))]
        self.refresh_printer_list();
        #[cfg(target_os = "windows")]
        {
            if self.shell.print_selected_printer.is_empty() || self.shell.print_printers.is_empty()
            {
                self.refresh_printer_list();
                return;
            }
            if self.jobs.pending_printer_refresh.is_some() {
                self.jobs.printer_refresh_queued = true;
                return;
            }
            let name = self.shell.print_selected_printer.clone();
            let settings = self.shell.print_driver_settings.clone();
            let mut printers = self.shell.print_printers.clone();
            self.jobs.pending_printer_refresh =
                Some(crate::app::jobs::Job::spawn("printer", move || {
                    if let Ok(updated) =
                        crate::core::printers::query_printer_with_settings(&name, settings.as_ref())
                    {
                        match printers.iter_mut().find(|p| p.name == name) {
                            Some(slot) => {
                                slot.paper_points = updated.paper_points;
                                slot.printable_rect_points = updated.printable_rect_points;
                            }
                            None => printers.push(updated),
                        }
                    }
                    Ok(printers)
                }));
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
    }

    pub(crate) fn open_print_dialog(&mut self) {
        // Where printing is not built (it goes through Windows), say how to
        // print instead of opening a dialog that cannot.
        if !crate::ui::dialogs::CAN_PRINT {
            self.shell.status_msg = crate::ui::dialogs::NO_PRINTING_HERE.to_string();
            self.redraw_main();
            return;
        }
        self.shell.ui.show_print_dialog = true;
        // The printer list is cached for the session (Refresh re-enumerates, e.g.
        // after plugging in a printer), but the selected printer's paper is re-read
        // on every open: its driver defaults may have changed outside iAi.
        if self.shell.print_printers.is_empty() {
            self.refresh_printer_list();
        } else {
            self.refresh_selected_printer();
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
}

/// One page Print sends: which page it is as the user counts them (1 being
/// the first), its artboard, its PDF page in an opened PDF, and its sheet.
#[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
struct PageToPrint {
    number: usize,
    artboard: usize,
    pdf_page: Option<usize>,
    sheet: crate::app::background_jobs::PrintSheet,
}

/// The artboards of `doc` asked for, in order, each over its master page
/// when it shows one. Pages it does not have are left out.
#[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
fn artboard_print_sheets(
    doc: &crate::core::document::Document,
    pages: &crate::ui::PrintPages,
) -> Vec<PageToPrint> {
    let indices = match pages {
        crate::ui::PrintPages::Current => vec![doc.active_artboard],
        crate::ui::PrintPages::Pages(list) => list.clone(),
    };
    indices
        .into_iter()
        .filter_map(|page| {
            let canvas = doc.page_render_canvas(page).or_else(|| {
                doc.all_page_canvases()
                    .get(page)
                    .map(|c| c.export_snapshot())
            })?;
            Some(PageToPrint {
                number: page + 1,
                artboard: page,
                pdf_page: None,
                sheet: crate::app::background_jobs::PrintSheet::Layers {
                    width: canvas.width,
                    height: canvas.height,
                    dpi: canvas.metadata.resolution_ppi,
                    stack: canvas.layer_stack,
                },
            })
        })
        .collect()
}

/// The pages of an opened PDF asked for, as its page bar counts them (0
/// being the first), in order, each as the tab shows it: the page in front
/// and the pages edited from their layers, with what was done to every page
/// laid over them; a page never shown is left to the print worker to render
/// (see [`PdfPrintSource`]). Pages it does not have are left out.
///
/// [`PdfPrintSource`]: crate::app::background_jobs::PdfPrintSource
#[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
fn pdf_print_sheets(
    doc: &crate::core::document::Document,
    pdf: &crate::core::document::PdfDocumentState,
    pages: &crate::ui::PrintPages,
) -> Vec<PageToPrint> {
    use crate::app::background_jobs::{PdfPrintSource, PrintSheet};
    let positions = match pages {
        crate::ui::PrintPages::Current => vec![
            pdf.selected_pages
                .iter()
                .position(|&page| page == pdf.active_page)
                .unwrap_or(0),
        ],
        crate::ui::PrintPages::Pages(list) => list.clone(),
    };
    let source = std::sync::Arc::new(PdfPrintSource::new(
        pdf.effective_source().to_path_buf(),
        pdf.requested_dpi,
        pdf.global_edits.clone(),
    ));
    positions
        .into_iter()
        .filter_map(|position| {
            let page = *pdf.selected_pages.get(position)?;
            let shown = if page == pdf.active_page {
                Some(&doc.canvas)
            } else {
                pdf.edited_pages.get(&page).map(|cached| &cached.canvas)
            };
            let sheet = match shown {
                Some(canvas) => {
                    let mut stack = canvas.export_snapshot().layer_stack;
                    if let Some(overlay) = crate::core::document::build_pdf_global_overlay_stack(
                        &pdf.global_edits,
                        page,
                        canvas.width,
                        canvas.height,
                    ) {
                        stack = stack.with_overlay(&overlay);
                    }
                    PrintSheet::Layers {
                        stack,
                        width: canvas.width,
                        height: canvas.height,
                        dpi: canvas.metadata.resolution_ppi,
                    }
                }
                None if page < pdf.page_count => PrintSheet::PdfPage {
                    source: std::sync::Arc::clone(&source),
                    index: page,
                },
                None => return None,
            };
            Some(PageToPrint {
                number: position + 1,
                artboard: doc.active_artboard,
                pdf_page: Some(page),
                sheet,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::app::state::App;

    /// A print handed to a worker that is driven by hand through `tx`.
    fn job_for(
        printer: &str,
    ) -> (
        std::sync::mpsc::Sender<Result<(), String>>,
        crate::app::background_jobs::PrintJob,
    ) {
        job_of(
            printer,
            crate::app::background_jobs::PrintedPage::alone(
                crate::core::document::DocumentId(1),
                0,
            ),
        )
    }

    /// The same, for the page `of`.
    fn job_of(
        printer: &str,
        of: crate::app::background_jobs::PrintedPage,
    ) -> (
        std::sync::mpsc::Sender<Result<(), String>>,
        crate::app::background_jobs::PrintJob,
    ) {
        let (tx, rx) = std::sync::mpsc::channel();
        let job = crate::app::background_jobs::PrintJob::new(
            printer.to_string(),
            of,
            rx,
            Default::default(),
            Default::default(),
        );
        (tx, job)
    }

    // The Mac has no PDF route behind CUPS: see the test after this one.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_print_that_went_out_says_so_and_one_refused_is_kept_for_the_pdf_route_of_its_tab() {
        let mut app = App::new();
        app.shell.ui.show_print_dialog = true;
        let (tx, job) = job_for("Printer");
        app.jobs.pending_print = Some(job);

        // Still on its way: nothing changes, and the app will not exit under it.
        app.poll_print_job();
        assert!(app.jobs.pending_print.is_some());
        assert!(app.shell.ui.show_print_dialog);
        assert_eq!(app.exit_blocking_operation(), Some("the print being sent"));

        // Gone out: it says so, and leaves a dialog opened since alone.
        tx.send(Ok(())).unwrap();
        app.poll_print_job();
        assert!(app.jobs.pending_print.is_none());
        assert!(app.shell.ui.show_print_dialog);
        assert_eq!(app.shell.status_msg, "Đã gửi lệnh in");

        let (tx, job) = job_for("Printer");
        app.jobs.pending_print = Some(job);
        tx.send(Err("out of paper".to_string())).unwrap();
        app.poll_print_job();
        assert!(app.jobs.pending_print.is_none());
        assert_eq!(app.jobs.print_gdi_failed.as_deref(), Some("out of paper"));
        // The PDF route is for that sheet's tab.
        assert_eq!(
            app.jobs.print_failed_doc,
            Some(crate::app::background_jobs::PrintedPage::alone(
                crate::core::document::DocumentId(1),
                0
            ))
        );
    }

    /// On the Mac a sheet CUPS refuses is told as CUPS's reason, and the
    /// sheets behind it are not sent to the same printer.
    #[cfg(target_os = "macos")]
    #[test]
    fn on_the_mac_a_refused_sheet_says_why_and_stops_the_rest() {
        use crate::ui::PrintPages;
        let folder = Folder::new("mac_refused");
        let mut app = app_with_an_opened_pdf(&folder);
        let (tx, job) = job_for("Printer");
        app.jobs.pending_print = Some(job);
        tx.send(Ok(())).unwrap();
        app.poll_print_job();
        assert_eq!(app.shell.status_msg, "Đã gửi lệnh in");

        let layout = app.shell.print_layout;
        let behind = app.print_tasks(&PrintPages::Pages(vec![2, 3]), layout, None);
        app.jobs.print_queue.extend(behind);
        let (tx, job) = job_for("Printer");
        app.jobs.pending_print = Some(job);
        let why = "Máy in «Printer» hết giấy: chưa gửi lệnh in.";
        tx.send(Err(why.to_string())).unwrap();
        app.poll_print_job();
        app.handle_color_print_actions(&mut crate::ui::UiActions::default());
        assert!(app.jobs.pending_print.is_none());
        assert!(app.jobs.print_queue.is_empty());
        assert!(
            app.jobs.print_gdi_failed.is_none(),
            "no second way to print"
        );
        assert_eq!(
            app.shell.status_msg,
            format!("{why} (2 tờ còn lại chưa in)")
        );
    }

    /// 11/10/2026: a print sent to a printer in trouble never came back. Print
    /// stayed off for every printer and the app would not close, so it had to
    /// be killed. A worker that stops moving is now told of once, no longer
    /// holds the exit, and can be left for another print.
    #[test]
    fn a_printer_that_never_answers_holds_up_neither_the_exit_nor_the_next_print() {
        use crate::app::background_jobs::PrintJob;
        use crate::core::print_gdi::{PrintStage, PrinterState};

        let mut app = App::new();
        app.shell.ui.show_print_dialog = true;
        let (tx, job) = job_for("Canon LBP2900");
        let progress = std::sync::Arc::clone(&job.progress);
        job.printer_state
            .set(PrinterState {
                troubles: vec!["mất kết nối (offline)"],
                queued_jobs: 1,
                port: "USB001".to_string(),
            })
            .unwrap();
        app.jobs.pending_print = Some(job);

        // An ordinary job: the app waits for it, and offers nothing to stop.
        progress.enter(PrintStage::StartingJob).unwrap();
        app.poll_print_job();
        assert_eq!(app.exit_blocking_operation(), Some("the print being sent"));
        assert_eq!(app.collect_ui_data().print.print_waiting_note, "");

        // It outlives an ordinary one while still moving: stopping is offered.
        let job = app.jobs.pending_print.as_mut().unwrap();
        job.stand_still_for(PrintJob::STOP_OFFERED_AFTER);
        progress.enter(PrintStage::SendingPage).unwrap();
        app.poll_print_job();
        assert!(
            app.collect_ui_data()
                .print
                .print_waiting_note
                .starts_with("Đang gửi tới «Canon LBP2900»")
        );
        assert_eq!(app.exit_blocking_operation(), Some("the print being sent"));

        // The worker stands still inside the driver: the user is told, once.
        let job = app.jobs.pending_print.as_mut().unwrap();
        job.stand_still_for(PrintJob::STUCK_AFTER);
        app.poll_print_job();
        assert!(
            app.shell
                .status_msg
                .contains("«Canon LBP2900» chưa nhận lệnh in")
                && app.shell.status_msg.contains("mất kết nối (offline)")
                && app
                    .shell
                    .status_msg
                    .contains("1 lệnh đang nằm trong hàng chờ in"),
            "{}",
            app.shell.status_msg
        );
        assert!(
            app.collect_ui_data()
                .print
                .print_waiting_note
                .starts_with("«Canon LBP2900» chưa trả lời")
        );
        app.shell.status_msg.clear();
        app.poll_print_job();
        assert_eq!(app.shell.status_msg, "", "said once, not every frame");
        // The app can be closed, though the job is still listed.
        assert!(app.jobs.pending_print.is_some());
        assert_eq!(app.exit_blocking_operation(), None);

        // Stopped: the worker hears it at its next step, and Print is free
        // again for another printer.
        let mut actions = crate::ui::UiActions::default();
        actions.print.print_stop = true;
        app.handle_color_print_actions(&mut actions);
        assert!(app.jobs.pending_print.is_none());
        assert!(progress.is_cancelled());
        assert!(
            app.shell.ui.show_print_dialog,
            "the dialog stays for the next print"
        );
        assert!(!app.collect_ui_data().print.print_sending);
        assert_eq!(
            app.shell.status_msg,
            "Đã dừng gửi lệnh in tới «Canon LBP2900»"
        );
        // What the left worker answers later reaches nobody.
        assert!(tx.send(Ok(())).is_err());
        assert!(app.jobs.print_gdi_failed.is_none());
    }

    #[test]
    fn a_worker_that_moves_again_is_no_longer_stuck() {
        use crate::app::background_jobs::PrintJob;
        use crate::core::print_gdi::PrintStage;

        let (_tx, mut job) = job_for("Printer");
        job.stand_still_for(PrintJob::STUCK_AFTER);
        assert!(job.is_stuck());
        // Seen at once, before the next `watch`.
        job.progress.enter(PrintStage::ClosingPage).unwrap();
        assert!(!job.is_stuck());
        job.watch();
        assert!(!job.is_stuck());
    }

    /// The whole hand-off with a real GDI call in the worker: a device that
    /// does not exist is refused there, off this thread, and the refusal is
    /// what comes back. (The PDF route that follows would hand a file to the
    /// system's PDF program, so the test stops short of it.)
    #[cfg(target_os = "windows")]
    #[test]
    fn printing_runs_on_a_worker_and_reports_a_device_that_refuses() {
        let mut app = App::new();
        app.shell.print_selected_printer = "iAi test: no such printer".to_string();
        app.shell.ui.show_print_dialog = true;
        let mut actions = crate::ui::UiActions::default();
        actions.print.print_send = Some(crate::ui::PrintPages::Current);
        app.handle_color_print_actions(&mut actions);
        assert!(app.jobs.pending_print.is_some(), "the job went to a worker");
        assert_eq!(app.shell.status_msg, "Đang gửi lệnh in…");
        assert!(
            !app.shell.ui.show_print_dialog,
            "the dialog closes at Print, the work goes on"
        );

        // A second Print while it is on its way waits its turn.
        app.handle_color_print_actions(&mut actions);
        assert_eq!(app.jobs.print_queue.len(), 1);
        assert_eq!(app.shell.status_msg, "Đã xếp lệnh in — 1 lệnh đang chờ gửi");

        let began = std::time::Instant::now();
        while app.jobs.pending_print.is_some() {
            assert!(began.elapsed().as_secs() < 30, "the worker never answered");
            std::thread::sleep(std::time::Duration::from_millis(10));
            app.poll_print_job();
        }
        assert!(app.jobs.print_gdi_failed.is_some());
        // The sheet behind waits for the refused one's PDF route.
        app.poll_print_job();
        assert_eq!(app.jobs.print_queue.len(), 1);
        assert!(app.jobs.pending_print.is_none());
    }

    /// Each page asked for is a print of its own, in order, made of that
    /// page; the page in front when the range is the current page.
    #[test]
    fn each_page_asked_for_is_a_print_of_its_own() {
        use crate::ui::PrintPages;
        use serde_json::json;
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        app.run_with("file.new", json!({"widthPx": 40, "heightPx": 30}))
            .unwrap();
        app.run_with("page.add", json!({})).unwrap();
        app.run_with("layer.new", json!({"kind": "fill", "fill": "#00FF00"}))
            .unwrap();
        app.run_with("page.add", json!({})).unwrap();
        app.run_with("page.select", json!({"page": 0})).unwrap();
        let layout = app.shell.print_layout;

        let tasks = app.print_tasks(&PrintPages::Pages(vec![0, 1, 2]), layout, None);
        assert_eq!(
            tasks.iter().map(|t| t.of.page).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        let doc = app.docs.active().id;
        assert!(
            tasks
                .iter()
                .all(|t| t.of.doc == doc && t.of.pdf_page.is_none())
        );
        assert!(tasks[1].doc_name.ends_with("trang 2"));
        assert_eq!(tasks[1].of.label, "trang 2");
        assert_eq!(&first_pixel(&tasks[1].sheet)[0..3], &[0, 255, 0]);
        assert_ne!(&first_pixel(&tasks[0].sheet)[0..3], &[0, 255, 0]);

        let tasks = app.print_tasks(&PrintPages::Current, layout, None);
        assert_eq!(tasks.iter().map(|t| t.of.page).collect::<Vec<_>>(), vec![0]);
        assert!(!tasks[0].doc_name.contains("trang"));
        assert_eq!(tasks[0].of.label, "", "a sheet alone");
    }

    /// The pixel at `(fx, fy)` (fractions of the page) of what a print worker
    /// would composite for `sheet`, rendering a PDF page as it would.
    fn pixel_at(sheet: &crate::app::background_jobs::PrintSheet, fx: f32, fy: f32) -> [u8; 4] {
        use crate::app::background_jobs::PrintSheet;
        let (flat, width, height) = match sheet {
            PrintSheet::Layers {
                stack,
                width,
                height,
                ..
            } => (stack.flatten(*width, *height), *width, *height),
            PrintSheet::PdfPage { source, index } => {
                let (stack, width, height, _) = source.render(*index).unwrap();
                (stack.flatten(width, height), width, height)
            }
        };
        let x = ((width as f32 * fx) as u32).min(width - 1);
        let y = ((height as f32 * fy) as u32).min(height - 1);
        let at = ((y * width + x) * 4) as usize;
        [flat[at], flat[at + 1], flat[at + 2], flat[at + 3]]
    }

    /// The colour in the left half of a sheet, where nothing is laid over.
    fn first_pixel(sheet: &crate::app::background_jobs::PrintSheet) -> [u8; 4] {
        snap(pixel_at(sheet, 0.25, 0.5))
    }

    /// The colour at the right edge of a sheet.
    fn right_pixel(sheet: &crate::app::background_jobs::PrintSheet) -> [u8; 4] {
        snap(pixel_at(sheet, 0.95, 0.5))
    }

    /// A rendered page's colour, give or take its renderer's rounding.
    fn snap(rgba: [u8; 4]) -> [u8; 4] {
        rgba.map(|c| match c {
            0..=3 => 0,
            125..=131 => 128,
            252..=255 => 255,
            c => c,
        })
    }

    /// Several pages with several copies go out a set at a time, every sheet
    /// once: 1, 2, 3, 1, 2, 3.
    #[test]
    fn several_pages_with_several_copies_go_out_a_set_at_a_time() {
        use crate::ui::PrintPages;
        use serde_json::json;
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        app.run_with("file.new", json!({"widthPx": 20, "heightPx": 20}))
            .unwrap();
        app.run_with("page.add", json!({})).unwrap();
        app.shell.print_copies = 3;
        let layout = app.shell.print_layout;

        let tasks = app.print_tasks(&PrintPages::Pages(vec![0, 1]), layout, None);
        assert_eq!(
            tasks.iter().map(|t| t.of.page).collect::<Vec<_>>(),
            vec![0, 1, 0, 1, 0, 1]
        );
        assert!(tasks.iter().all(|t| t.copies == 1));
        assert_eq!(tasks[0].of.label, "trang 1 (bộ 1/3)");
        assert_eq!(tasks[5].of.label, "trang 2 (bộ 3/3)");

        // One page: the copies are the driver's, one sheet asked for.
        let tasks = app.print_tasks(&PrintPages::Current, layout, None);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].copies, 3);
    }

    /// A folder of the test's own, gone when it ends.
    struct Folder(std::path::PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "iai_print_{name}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn solid_canvas(rgba: [u8; 4], side: u32) -> crate::core::canvas::Canvas {
        let mut canvas = crate::core::canvas::Canvas::from_rgba(
            rgba.into_iter()
                .cycle()
                .take((side * side * 4) as usize)
                .collect(),
            side,
            side,
        );
        canvas.metadata.resolution_ppi = 72.0;
        canvas
    }

    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const YELLOW: [u8; 4] = [255, 255, 0, 255];
    const PURPLE: [u8; 4] = [128, 0, 128, 255];

    /// An app whose tab is an opened PDF of four pages, red, green, blue and
    /// yellow, written to `folder`, shown at 72 DPI: page 2 in front, page 4
    /// edited to purple and cached, pages 1 and 3 never shown.
    fn app_with_an_opened_pdf(folder: &Folder) -> App {
        const SIDE: u32 = 16;
        let path = folder.0.join("hop-dong.pdf");
        let pages: Vec<_> = [RED, GREEN, BLUE, YELLOW]
            .into_iter()
            .map(|rgba| crate::formats::pdf::ImagePdfPage {
                rgba: rgba
                    .into_iter()
                    .cycle()
                    .take((SIDE * SIDE * 4) as usize)
                    .collect(),
                width: SIDE,
                height: SIDE,
                page_points: (SIDE as f32, SIDE as f32),
            })
            .collect();
        crate::formats::pdf::write_image_pdf(&path, &pages).unwrap();

        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        let reference = |index| {
            let mut reference = crate::core::document::PdfPageRef {
                group_id: 1,
                source: path.clone(),
                index,
                count: 4,
                requested_dpi: 72.0,
                loaded: true,
                original_width: 0,
                original_height: 0,
                original_dpi: 72.0,
                original_layer_id: 0,
                original_tiles_fingerprint: 0,
            };
            reference.record_canvas_baseline(&solid_canvas(GREEN, SIDE));
            reference
        };
        let doc = &mut app.docs.documents[0];
        doc.canvas = solid_canvas(GREEN, SIDE);
        doc.pdf_page = Some(reference(1));
        let mut edited = std::collections::HashMap::new();
        edited.insert(
            3,
            crate::core::document::PdfCachedPage {
                canvas: solid_canvas(PURPLE, SIDE),
                reference: reference(3),
                saved_zoom: 0.0,
                saved_offset_x: 0.0,
                saved_offset_y: 0.0,
            },
        );
        doc.pdf_document = Some(crate::core::document::PdfDocumentState {
            source: path,
            embedded_source: None,
            page_count: 4,
            selected_pages: vec![0, 1, 2, 3],
            selected_pages_saved: vec![0, 1, 2, 3],
            page_names: std::collections::BTreeMap::new(),
            page_names_saved: std::collections::BTreeMap::new(),
            requested_dpi: 72.0,
            active_page: 1,
            active_page_modified: false,
            edited_pages: edited,
            global_edits: Vec::new(),
            global_edits_saved: Vec::new(),
            global_edits_redo: Vec::new(),
            global_overlay_cache: None,
        });
        app
    }

    /// 17/11/2026: printing an opened PDF sent only the page in front. Every
    /// page asked for now goes, in order, each as the tab shows it: the page
    /// in front and an edited page from their layers, a page never shown
    /// rendered from the file by the print worker (the window renders none).
    #[test]
    fn an_opened_pdf_prints_every_page_asked_for_as_the_tab_shows_it() {
        use crate::app::background_jobs::PrintSheet;
        use crate::ui::PrintPages;
        let folder = Folder::new("pdf_pages");
        let app = app_with_an_opened_pdf(&folder);
        let layout = app.shell.print_layout;

        let tasks = app.print_tasks(&PrintPages::Pages(vec![0, 1, 2, 3]), layout, None);
        assert_eq!(
            tasks.iter().map(|t| t.of.pdf_page).collect::<Vec<_>>(),
            vec![Some(0), Some(1), Some(2), Some(3)]
        );
        assert_eq!(
            tasks
                .iter()
                .map(|t| t.of.label.as_str())
                .collect::<Vec<_>>(),
            vec!["trang 1", "trang 2", "trang 3", "trang 4"]
        );
        assert_eq!(tasks[2].doc_name, "hop-dong.pdf — trang 3");
        let kinds: Vec<bool> = tasks
            .iter()
            .map(|t| matches!(t.sheet, PrintSheet::PdfPage { .. }))
            .collect();
        assert_eq!(
            kinds,
            vec![true, false, true, false],
            "only the pages never shown are left to render"
        );
        // The pages left to render share one opened PDF.
        let (PrintSheet::PdfPage { source: a, .. }, PrintSheet::PdfPage { source: b, .. }) =
            (&tasks[0].sheet, &tasks[2].sheet)
        else {
            unreachable!()
        };
        assert!(std::sync::Arc::ptr_eq(a, b));
        let colours: Vec<[u8; 4]> = tasks.iter().map(|t| first_pixel(&t.sheet)).collect();
        assert_eq!(colours, vec![RED, GREEN, BLUE, PURPLE]);

        // Typed pages, in the page bar's order; the page in front alone.
        let tasks = app.print_tasks(&PrintPages::Pages(vec![2, 3]), layout, None);
        assert_eq!(
            tasks
                .iter()
                .map(|t| first_pixel(&t.sheet))
                .collect::<Vec<_>>(),
            vec![BLUE, PURPLE]
        );
        let tasks = app.print_tasks(&PrintPages::Current, layout, None);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].of.pdf_page, Some(1));
        assert_eq!(tasks[0].of.label, "");
        assert_eq!(first_pixel(&tasks[0].sheet), GREEN);
    }

    /// A page moved in the page bar prints where it now stands, and what was
    /// done to every page (a strip cleared on the right) is on the sheets too.
    #[test]
    fn an_opened_pdf_prints_in_its_page_order_with_what_was_done_to_every_page() {
        use crate::ui::PrintPages;
        let folder = Folder::new("pdf_order");
        let mut app = app_with_an_opened_pdf(&folder);
        let pdf = app.docs.documents[0].pdf_document.as_mut().unwrap();
        pdf.selected_pages = vec![2, 0, 1, 3];
        pdf.global_edits
            .push(crate::core::document::PdfGlobalEdit::Clear(
                crate::core::document::PdfGlobalClear {
                    x0: 0.5,
                    y0: 0.0,
                    x1: 1.0,
                    y1: 1.0,
                    color: [255, 255, 255, 255],
                    target_pages: Vec::new(),
                },
            ));
        let layout = app.shell.print_layout;

        let tasks = app.print_tasks(&PrintPages::Pages(vec![0, 1, 2, 3]), layout, None);
        assert_eq!(
            tasks.iter().map(|t| t.of.pdf_page).collect::<Vec<_>>(),
            vec![Some(2), Some(0), Some(1), Some(3)]
        );
        assert_eq!(
            tasks
                .iter()
                .map(|t| first_pixel(&t.sheet))
                .collect::<Vec<_>>(),
            vec![BLUE, RED, GREEN, PURPLE]
        );
        for task in &tasks {
            assert_eq!(right_pixel(&task.sheet), [255, 255, 255, 255]);
        }
    }

    /// A page that cannot be rendered (its PDF moved away) is left out, the
    /// rest go on, and the end of the print says which pages were left out.
    #[test]
    fn a_pdf_page_that_cannot_be_rendered_is_left_out_and_said() {
        use crate::ui::PrintPages;
        let folder = Folder::new("pdf_moved");
        let mut app = app_with_an_opened_pdf(&folder);
        let pdf = app.docs.documents[0].pdf_document.as_mut().unwrap();
        pdf.source = folder.0.join("da-chuyen-di.pdf");
        let layout = app.shell.print_layout;
        let tasks = app.print_tasks(&PrintPages::Pages(vec![0, 2]), layout, None);
        for task in tasks {
            app.queue_print_task(task);
        }
        assert_eq!(app.shell.status_msg, "Đã xếp lệnh in — 1 lệnh đang chờ gửi");
        let began = std::time::Instant::now();
        while app.jobs.pending_print.is_some() || !app.jobs.print_queue.is_empty() {
            assert!(began.elapsed().as_secs() < 30, "the worker never answered");
            std::thread::sleep(std::time::Duration::from_millis(5));
            app.poll_print_job();
        }
        assert!(app.jobs.print_gdi_failed.is_none(), "no PDF route for it");
        assert_eq!(
            app.shell.status_msg,
            "Không in được trang 1, trang 3: không thấy file PDF «da-chuyen-di.pdf» (đã đổi tên hay chuyển đi?)"
        );
        assert!(app.jobs.print_skipped.is_empty());
    }

    /// A page of an opened PDF the printer refuses stops the print: the
    /// sheets behind it would go to the same printer.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_pdf_page_the_printer_refuses_stops_the_rest() {
        use crate::app::background_jobs::PrintedPage;
        use crate::ui::PrintPages;
        let folder = Folder::new("pdf_refused");
        let mut app = app_with_an_opened_pdf(&folder);
        let layout = app.shell.print_layout;
        let doc = app.docs.active().id;
        let behind = app.print_tasks(&PrintPages::Pages(vec![2, 3]), layout, None);
        app.jobs.print_queue.extend(behind);
        let (tx, job) = job_of(
            "Printer",
            PrintedPage {
                doc,
                page: 0,
                pdf_page: Some(1),
                label: "trang 2".to_string(),
            },
        );
        app.jobs.pending_print = Some(job);
        tx.send(Err("out of paper".to_string())).unwrap();
        app.poll_print_job();
        app.handle_color_print_actions(&mut crate::ui::UiActions::default());
        assert!(app.jobs.print_queue.is_empty());
        assert!(app.jobs.print_gdi_failed.is_none());
        assert_eq!(
            app.shell.status_msg,
            "Máy in không nhận trang 2 (out of paper) — đã dừng, 2 tờ còn lại chưa in; kiểm tra máy in rồi in lại"
        );
    }

    /// A worker rendering a PDF page is at its own work: never "a printer
    /// that does not answer", however long the page takes.
    #[test]
    fn a_worker_rendering_a_pdf_page_is_not_a_printer_that_does_not_answer() {
        use crate::app::background_jobs::PrintJob;
        use crate::core::print_gdi::PrintStage;
        let (_tx, mut job) = job_for("Printer");
        job.progress.enter(PrintStage::RenderingPage).unwrap();
        job.watch();
        job.stand_still_for(PrintJob::STUCK_AFTER);
        assert!(!job.is_stuck());
        assert!(
            job.waiting_note()
                .is_some_and(|note| note.starts_with("Đang dựng trang PDF để in…"))
        );
        job.progress.enter(PrintStage::OpeningDevice).unwrap();
        job.watch();
        job.stand_still_for(PrintJob::STUCK_AFTER);
        assert!(job.is_stuck());
    }

    #[test]
    fn a_print_worker_that_died_is_treated_as_a_refusal() {
        let mut app = App::new();
        let (tx, job) = job_for("Printer");
        app.jobs.pending_print = Some(job);
        drop(tx);
        app.poll_print_job();
        assert!(app.jobs.pending_print.is_none());
        assert!(app.jobs.print_gdi_failed.is_some());
    }
}

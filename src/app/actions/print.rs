//! Printer-list plumbing for the Print dialog. Split out of app/actions.rs.

use crate::app::state::App;

impl App {
    /// Apply the result of the native printer property sheet without ever
    /// blocking/re-entering winit's UI thread.
    pub(super) fn poll_printer_settings(&mut self) {
        let Some(rx) = self.jobs.pending_printer_settings.take() else {
            return;
        };

        match rx.try_recv() {
            Ok((printer, Ok(Some(settings)))) => {
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
            Ok((_printer, Ok(None))) => {
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            Ok((_printer, Err(e))) => {
                self.shell.status_msg = e;
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.jobs.pending_printer_settings = Some(rx);
                return;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.shell.status_msg = "Printer settings dialog stopped unexpectedly".to_string();
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
                    "Đã gửi lệnh in".to_string()
                } else {
                    format!("Đã gửi lệnh in — còn {waiting} lệnh đang chờ gửi")
                };
                if let Some(task) = self.jobs.print_queue.pop_front() {
                    self.start_print_task(task);
                }
            }
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

    /// One print task a page of the tab in front, in order: the page as
    /// export writes it (over its master page, when it shows one).
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub(in crate::app) fn print_tasks(
        &self,
        pages: &crate::ui::PrintPages,
        layout: crate::core::print::PrintLayout,
        profile: Option<Vec<u8>>,
    ) -> Vec<crate::app::background_jobs::PrintTask> {
        let doc = self.docs.active();
        let indices = match pages {
            crate::ui::PrintPages::Current => vec![doc.active_artboard],
            crate::ui::PrintPages::Pages(list) => list.clone(),
        };
        let several = indices.len() > 1;
        indices
            .into_iter()
            .filter_map(|page| {
                let canvas = doc.page_render_canvas(page).or_else(|| {
                    doc.all_page_canvases()
                        .get(page)
                        .map(|c| c.export_snapshot())
                })?;
                Some(crate::app::background_jobs::PrintTask {
                    doc_id: doc.id,
                    page,
                    doc_name: if several {
                        format!("{} — trang {}", doc.title, page + 1)
                    } else {
                        doc.title.clone()
                    },
                    width: canvas.width,
                    height: canvas.height,
                    dpi: canvas.metadata.resolution_ppi,
                    stack: canvas.layer_stack,
                    layout,
                    profile: profile.clone(),
                    printer: self.shell.print_selected_printer.clone(),
                    settings: self.shell.print_driver_settings.clone(),
                    copies: self.shell.print_copies,
                })
            })
            .collect()
    }

    /// Send `task` now, or after the sheet already on its way.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
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
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub(in crate::app) fn start_print_task(
        &mut self,
        task: crate::app::background_jobs::PrintTask,
    ) {
        let crate::app::background_jobs::PrintTask {
            doc_id,
            page,
            doc_name,
            mut stack,
            width: cw,
            height: ch,
            dpi,
            layout,
            profile,
            printer,
            settings,
            copies,
        } = task;
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
            (doc_id, page),
            rx,
            std::sync::Arc::clone(&progress),
            std::sync::Arc::clone(&printer_state),
        ));
        let waiting = self.jobs.print_queue.len();
        self.shell.status_msg = if waiting == 0 {
            "Đang gửi lệnh in…".to_string()
        } else {
            format!("Đang gửi lệnh in… (còn {waiting} lệnh chờ)")
        };
        std::thread::spawn(move || {
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
        let (tx, rx) = std::sync::mpsc::channel();
        self.jobs.pending_printer_settings = Some(rx);
        self.shell.status_msg = format!("Opening printer settings: {printer}");

        // Reset before the worker creates the native property sheet.  Waiting
        // for a later redraw leaves a small race where Windows can inherit the
        // active brush/pen/tool cursor (or the hidden-cursor state).
        if let Some(w) = &self.win.window {
            w.set_cursor_visible(true);
            w.set_cursor(winit::window::CursorIcon::Default);
        }

        std::thread::spawn(move || {
            let result = crate::core::printers::open_printer_settings(
                &worker_printer,
                settings.as_ref(),
                owner_hwnd,
            );
            let _ = tx.send((worker_printer, result));
        });
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

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
            self.shell.status_msg = "No printers found".to_string();
        }
    }

    pub(super) fn poll_printer_refresh(&mut self) {
        let Some(rx) = self.jobs.pending_printer_refresh.take() else {
            return;
        };

        match rx.try_recv() {
            Ok(Ok(printers)) => {
                self.apply_printer_list(printers);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            Ok(Err(e)) => {
                self.shell.print_printers.clear();
                self.shell.status_msg = e;
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.jobs.pending_printer_refresh = Some(rx);
                if self.shell.ui.show_print_dialog
                    && let Some(w) = &self.win.window
                {
                    w.request_redraw();
                }
                return;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.shell.status_msg = "Printer refresh stopped".to_string();
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
        let (tx, rx) = std::sync::mpsc::channel();
        self.jobs.pending_printer_refresh = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(crate::core::printers::available_printers());
        });
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
            let (tx, rx) = std::sync::mpsc::channel();
            self.jobs.pending_printer_refresh = Some(rx);
            std::thread::spawn(move || {
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
                let _ = tx.send(Ok(printers));
            });
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
    }

    pub(crate) fn open_print_dialog(&mut self) {
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
        let (tx, rx) = std::sync::mpsc::channel();
        let job = crate::app::background_jobs::PrintJob::new(
            printer.to_string(),
            (crate::core::document::DocumentId(1), 0),
            rx,
            Default::default(),
            Default::default(),
        );
        (tx, job)
    }

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
            Some((crate::core::document::DocumentId(1), 0))
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
            tasks.iter().map(|t| t.page).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        let doc = app.docs.active().id;
        assert!(tasks.iter().all(|t| t.doc_id == doc));
        assert!(tasks[1].doc_name.ends_with("trang 2"));
        let green = &tasks[1];
        let flat = green.stack.flatten(green.width, green.height);
        assert_eq!(&flat[0..3], &[0, 255, 0]);
        let first = &tasks[0];
        let flat = first.stack.flatten(first.width, first.height);
        assert_ne!(&flat[0..3], &[0, 255, 0]);

        let tasks = app.print_tasks(&PrintPages::Current, layout, None);
        assert_eq!(tasks.iter().map(|t| t.page).collect::<Vec<_>>(), vec![0]);
        assert!(!tasks[0].doc_name.contains("trang"));
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

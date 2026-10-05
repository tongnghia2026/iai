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
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.shell.status_msg = "Printer settings dialog stopped unexpectedly".to_string();
            }
        }
    }

    /// Hear from the print worker. A sheet that went out closes the dialog;
    /// one the device refused goes round again, next frame, through the PDF
    /// handler (`handle_color_print_actions` takes `print_gdi_failed`).
    pub(in crate::app) fn poll_print_job(&mut self) {
        let Some(rx) = self.jobs.pending_print.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(())) => {
                self.shell.status_msg = "Sent to printer".to_string();
                self.shell.ui.show_print_dialog = false;
            }
            Ok(Err(e)) => self.jobs.print_gdi_failed = Some(e),
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.jobs.pending_print = Some(rx);
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
            let result = crate::core::print::open_printer_settings(
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
            if settings.matches_printer(name) {
                if let Ok(updated) =
                    crate::core::print::query_printer_with_settings(name, Some(settings))
                {
                    if let Some(slot) = printers.iter_mut().find(|p| p.name == *name) {
                        slot.paper_points = updated.paper_points;
                        slot.printable_rect_points = updated.printable_rect_points;
                    }
                }
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
                if self.shell.ui.show_print_dialog {
                    if let Some(w) = &self.win.window {
                        w.request_redraw();
                    }
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
            let _ = tx.send(crate::core::print::available_printers());
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
                    crate::core::print::query_printer_with_settings(&name, settings.as_ref())
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

    #[test]
    fn a_print_that_went_out_closes_the_dialog_and_one_refused_is_kept_for_the_pdf_route() {
        let mut app = App::new();
        app.shell.ui.show_print_dialog = true;
        let (tx, rx) = std::sync::mpsc::channel();
        app.jobs.pending_print = Some(rx);

        // Still on its way: nothing changes, and the app will not exit under it.
        app.poll_print_job();
        assert!(app.jobs.pending_print.is_some());
        assert!(app.shell.ui.show_print_dialog);
        assert_eq!(app.exit_blocking_operation(), Some("the print being sent"));

        tx.send(Ok(())).unwrap();
        app.poll_print_job();
        assert!(app.jobs.pending_print.is_none());
        assert!(!app.shell.ui.show_print_dialog);
        assert_eq!(app.shell.status_msg, "Sent to printer");

        let (tx, rx) = std::sync::mpsc::channel();
        app.jobs.pending_print = Some(rx);
        tx.send(Err("out of paper".to_string())).unwrap();
        app.poll_print_job();
        assert!(app.jobs.pending_print.is_none());
        assert_eq!(app.jobs.print_gdi_failed.as_deref(), Some("out of paper"));
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
        actions.print.print_send = true;
        app.handle_color_print_actions(&mut actions);
        assert!(app.jobs.pending_print.is_some(), "the job went to a worker");
        assert_eq!(app.shell.status_msg, "Sending to printer…");

        // A second click while it is on its way starts nothing new.
        app.handle_color_print_actions(&mut actions);
        assert_eq!(app.shell.status_msg, "Still sending the previous print…");

        let began = std::time::Instant::now();
        while app.jobs.pending_print.is_some() {
            assert!(began.elapsed().as_secs() < 30, "the worker never answered");
            std::thread::sleep(std::time::Duration::from_millis(10));
            app.poll_print_job();
        }
        assert!(app.jobs.print_gdi_failed.is_some());
        assert!(
            app.shell.ui.show_print_dialog,
            "a refused print keeps the dialog"
        );
    }

    #[test]
    fn a_print_worker_that_died_is_treated_as_a_refusal() {
        let mut app = App::new();
        let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
        app.jobs.pending_print = Some(rx);
        drop(tx);
        app.poll_print_job();
        assert!(app.jobs.pending_print.is_none());
        assert!(app.jobs.print_gdi_failed.is_some());
    }
}

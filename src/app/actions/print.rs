//! Printer-list plumbing for the Print dialog. Split out of app/actions.rs.

use crate::app::state::App;

impl App {
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
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.shell.status_msg = "Printer refresh stopped".to_string();
            }
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
        // Enumerating printers (+ their paper sizes) via PowerShell takes ~2-3s, so
        // cache the list for the session: only auto-query when we have none yet. The
        // dialog's Refresh button re-queries on demand (e.g. after plugging in a printer).
        if self.shell.print_printers.is_empty() {
            self.refresh_printer_list();
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
}

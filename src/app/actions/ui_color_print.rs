//! apply_ui_actions handlers: colour management (proof/CMS/profiles) and
//! the print dialog. Split out of actions.rs (phase 2).

use crate::app::state::App;
use crate::ui::UiActions;

impl App {
    pub(crate) fn handle_color_print_actions(&mut self, actions: &mut UiActions) {
        if let Some(target) = actions.print.set_proof_target.take() {
            self.shell.proof_target = target;
            self.shell.proof_enabled = true;
            self.apply_proof_settings();
            self.shell.status_msg = format!("Proof: {}", self.shell.proof_target.label());
        }
        if actions.print.toggle_proof_colors {
            self.shell.proof_enabled = !self.shell.proof_enabled;
            self.apply_proof_settings();
            self.shell.status_msg = if self.shell.proof_enabled {
                format!("Proof Colors on - {}", self.shell.proof_target.label())
            } else {
                "Proof Colors off".to_string()
            };
        }
        if actions.print.toggle_gamut_warning {
            self.shell.proof_gamut_warn = !self.shell.proof_gamut_warn;
            // Gamut warning only means anything while proofing.
            if self.shell.proof_gamut_warn {
                self.shell.proof_enabled = true;
            }
            self.apply_proof_settings();
            self.shell.status_msg = if self.shell.proof_gamut_warn {
                "Gamut Warning on".to_string()
            } else {
                "Gamut Warning off".to_string()
            };
        }
        if actions.print.load_proof_profile {
            if let Some(window) = self.win.window.as_ref() {
                let parent = crate::file_io::dialog_parent(window);
                let mut dialog = rfd::FileDialog::new().add_filter("ICC Profile", &["icc", "icm"]);
                if let Some(p) = parent {
                    dialog = dialog.set_parent(&p);
                }
                if let Some(path) = dialog.pick_file() {
                    match std::fs::read(&path) {
                        Ok(bytes) => {
                            let name = path
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .unwrap_or("Custom")
                                .to_string();
                            self.shell.proof_target =
                                crate::core::cms::ProofTarget::Custom { name, icc: bytes };
                            self.shell.proof_enabled = true;
                            self.apply_proof_settings();
                            self.shell.status_msg =
                                format!("Proof: {}", self.shell.proof_target.label());
                        }
                        Err(e) => {
                            self.shell.status_msg = format!("Couldn't read ICC profile: {e}");
                        }
                    }
                }
            }
        }

        // -- Display colour management (View > Display Profile) --
        if actions.print.display_cms_off {
            self.shell.display_cms_enabled = false;
            self.shell.display_profile_from_system = false;
            self.apply_proof_settings();
            self.shell.status_msg = "Display color management off".to_string();
        }
        if actions.print.display_cms_from_system {
            match self.enable_system_display_profiles() {
                Some(name) => {
                    self.shell.status_msg = format!("Display profile: {name}");
                }
                None => {
                    self.shell.status_msg =
                        "Couldn't get a display profile from the OS - use Load Profile..."
                            .to_string();
                }
            }
        }
        if actions.print.display_cms_load {
            if let Some(window) = self.win.window.as_ref() {
                let parent = crate::file_io::dialog_parent(window);
                let mut dialog = rfd::FileDialog::new().add_filter("ICC Profile", &["icc", "icm"]);
                if let Some(p) = parent {
                    dialog = dialog.set_parent(&p);
                }
                if let Some(path) = dialog.pick_file() {
                    match std::fs::read(&path) {
                        Ok(bytes) => {
                            let name = path
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .unwrap_or("Display")
                                .to_string();
                            self.shell.display_profile = Some(bytes);
                            self.shell.display_profile_name = name.clone();
                            self.shell.develop_display_profile = self.shell.display_profile.clone();
                            self.shell.develop_display_profile_name = name.clone();
                            self.shell.display_profile_from_system = false;
                            self.shell.display_cms_enabled = true;
                            self.apply_proof_settings();
                            self.shell.status_msg = format!("Display profile: {name}");
                        }
                        Err(e) => {
                            self.shell.status_msg = format!("Couldn't read ICC profile: {e}");
                        }
                    }
                }
            }
        }

        // -- CMYK separations export (Phase D slice) --
        if actions.print.export_cmyk_separations {
            self.export_cmyk_separations();
        }

        if actions.print.export_svg {
            self.export_svg();
        }

        // -- Printing (File > Print) --
        if let Some(v) = actions.print.show_print_dialog.take() {
            if v {
                self.open_print_dialog();
            } else {
                self.shell.ui.show_print_dialog = false;
            }
        }
        if let Some(layout) = actions.print.set_print_layout.take() {
            self.shell.print_layout = layout;
        }
        if actions.print.refresh_printers {
            self.refresh_printer_list();
        }
        if let Some(printer) = actions.print.set_print_printer.take() {
            if printer != self.shell.print_selected_printer {
                self.shell.print_driver_settings = None;
                self.shell.print_selected_printer = printer;
                // The cached geometry may be stale (driver defaults changed, or a
                // slot still carries an earlier app-local paper), so re-read it.
                self.refresh_selected_printer();
            }
        }
        if let Some(copies) = actions.print.set_print_copies.take() {
            self.shell.print_copies = copies.clamp(1, 999);
        }
        if actions.print.open_printer_settings {
            let owner_hwnd = self
                .win
                .window
                .as_ref()
                .and_then(|window| crate::file_io::dialog_parent(window.as_ref()))
                .map(crate::file_io::DialogParent::hwnd)
                .unwrap_or(0);
            self.open_printer_settings_async(owner_hwnd);
        }
        if actions.print.clear_print_printer_profile {
            self.shell.print_printer_profile = None;
            self.shell.print_printer_profile_name.clear();
            self.shell.status_msg = "Print color: Printer Manages Colors".to_string();
        }
        if actions.print.load_print_printer_profile {
            if let Some(window) = self.win.window.as_ref() {
                let parent = crate::file_io::dialog_parent(window);
                let mut dialog = rfd::FileDialog::new().add_filter("ICC Profile", &["icc", "icm"]);
                if let Some(p) = parent {
                    dialog = dialog.set_parent(&p);
                }
                if let Some(path) = dialog.pick_file() {
                    match std::fs::read(&path) {
                        Ok(bytes) => {
                            if crate::core::cms::profile_is_rgb(&bytes) {
                                let name = path
                                    .file_stem()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("Printer")
                                    .to_string();
                                self.shell.print_printer_profile = Some(bytes);
                                self.shell.print_printer_profile_name = name.clone();
                                self.shell.status_msg = format!("Print color: convert to {name}");
                            } else {
                                self.shell.status_msg =
                                    "CMYK profiles can't print directly yet (needs Phase D) - use Printer Manages Colors".to_string();
                            }
                        }
                        Err(e) => self.shell.status_msg = format!("Couldn't read ICC profile: {e}"),
                    }
                }
            }
        }
        // A direct print its worker could not make comes back here, a frame
        // later, for the PDF route (see `poll_print_job`).
        let gdi_failed = self.jobs.print_gdi_failed.take();
        let mut print_send = actions.print.print_send;
        if print_send && self.jobs.pending_print.is_some() {
            // One sheet at a time; the dialog's Print button is off meanwhile.
            self.shell.status_msg = "Still sending the previous print…".to_string();
            print_send = false;
        }
        if actions.print.print_save_pdf || print_send || gdi_failed.is_some() {
            self.sync_brush_gpu_to_cpu();
            let (cw, ch) = {
                let c = &self.docs.documents[self.docs.active_doc_idx].canvas;
                (c.width, c.height)
            };
            {
                let dpi = self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .metadata
                    .resolution_ppi;
                let layout = crate::core::print::layout_for_printer(
                    self.shell.print_layout,
                    &self.shell.print_printers,
                    &self.shell.print_selected_printer,
                );
                // Colour handling: with a loaded RGB printer profile, convert the
                // page to that device (app-managed) and tag the PDF with it. In
                // printer-managed mode, leave the image as plain DeviceRGB; some
                // Windows PDF print handlers route ICCBased image XObjects through
                // monochrome driver paths.
                let printer_profile = self
                    .shell
                    .print_printer_profile
                    .clone()
                    .filter(|pp| crate::core::cms::profile_is_rgb(pp));
                let pdf_icc = printer_profile.as_deref();

                // To-printer jobs go straight through GDI on Windows: external
                // PDF handlers (Foxit/Acrobat) rescale pages to the printer
                // margins, so prints came out smaller than their physical size.
                // The PDF handler route stays as a fallback (and off-Windows).
                //
                // The job itself runs on a worker: opening the device, the
                // spooler's StartDoc and the bands took 0.15-0.45 s a sheet on
                // this thread, and more with every copy. What the worker needs
                // of the document is taken here; `poll_print_job` hears how it
                // went.
                #[cfg(target_os = "windows")]
                if print_send && gdi_failed.is_none() {
                    let doc_name = self.docs.documents[self.docs.active_doc_idx].title.clone();
                    let flat = crate::core::canvas::Canvas::fits_flat_buffer(cw, ch).then(|| {
                        self.docs.documents[self.docs.active_doc_idx]
                            .canvas
                            .export_flat()
                    });
                    let mut stack = if flat.is_none() {
                        Some(
                            self.docs.documents[self.docs.active_doc_idx]
                                .canvas
                                .layer_stack
                                .clone(),
                        )
                    } else {
                        None
                    };
                    let profile = printer_profile.clone();
                    let intent = layout.intent.to_lcms();
                    let printer = self.shell.print_selected_printer.clone();
                    let settings = self.shell.print_driver_settings.clone();
                    let copies = self.shell.print_copies;
                    let (tx, rx) = std::sync::mpsc::channel();
                    self.jobs.pending_print = Some(rx);
                    self.shell.status_msg = "Sending to printer…".to_string();
                    std::thread::spawn(move || {
                        let mut flat = flat;
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
                            move |y, rows| {
                                if let Some(flat) = &flat {
                                    let start = (y as usize) * (cw as usize) * 4;
                                    let len = (rows as usize) * (cw as usize) * 4;
                                    Ok(flat[start..start + len].to_vec())
                                } else {
                                    let stack = stack.as_mut().expect("streamed path has a stack");
                                    let mut band = stack.flatten_band(cw, ch, y, rows);
                                    if let Some(pp) = &profile {
                                        crate::core::cms::convert_srgb_to_rgb_profile(
                                            &mut band, pp, intent,
                                        );
                                    }
                                    Ok(band)
                                }
                            },
                        );
                        let _ = tx.send(result);
                    });
                    if let Some(w) = &self.win.window {
                        w.request_redraw();
                    }
                    if !actions.print.print_save_pdf {
                        return;
                    }
                }
                // Why the direct route failed, when this frame is its fallback.
                let gdi_result: Option<Result<(), String>> = gdi_failed.map(Err);
                let send_via_pdf =
                    gdi_result.is_some() || (cfg!(not(target_os = "windows")) && print_send);
                if !actions.print.print_save_pdf && !send_via_pdf {
                    return;
                }
                // Save-as-PDF on a CMYK document: promote qualifying vectors to
                // native DeviceCMYK paths over an ink raster base — a press-ready,
                // resolution-independent hand-off with no RGB round trip. The
                // to-printer path stays RGB: consumer drivers mishandle CMYK PDFs
                // the same way they mishandled ICC-tagged ones (the printer-managed
                // B&W bug). Ink needs a full flat buffer, so past the flat-buffer
                // cap this falls back to the streamed RGB path.
                let cmyk_pdf = actions.print.print_save_pdf
                    && !actions.print.print_send
                    && self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .is_cmyk()
                    && crate::core::canvas::Canvas::fits_flat_buffer(cw, ch);
                let cmyk_selection = if cmyk_pdf {
                    crate::core::print::collect_pdf_vectors(
                        &self.docs.documents[self.docs.active_doc_idx].canvas,
                    )
                } else {
                    crate::core::print::PdfVectorSelection {
                        objects: Vec::new(),
                        promoted_layer_ids: Vec::new(),
                        above_layer_ids: Vec::new(),
                    }
                };
                let ink_page = if cmyk_pdf {
                    crate::core::print::pdf_ink_base(
                        &self.docs.documents[self.docs.active_doc_idx].canvas,
                        &cmyk_selection,
                    )
                } else {
                    None
                };
                let ink_native = ink_page.is_some();
                // Crisp vector overlay: an RGB page draws the qualifying Path /
                // Shape / Text layers as true PDF vectors (Ctrl+P Save-as-PDF /
                // send-to-printer is resolution-independent too — no "PDF răng
                // cưa"); a CMYK ink page draws them as native DeviceCMYK vectors
                // over the ink base. CMYK pages that aren't ink-exact, and the
                // send-to-printer RGB path, stay pure raster (empty overlay).
                let vector_selection = if ink_native {
                    cmyk_selection
                } else if self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .is_cmyk()
                {
                    crate::core::print::PdfVectorSelection {
                        objects: Vec::new(),
                        promoted_layer_ids: Vec::new(),
                        above_layer_ids: Vec::new(),
                    }
                } else {
                    crate::core::print::collect_pdf_vectors(
                        &self.docs.documents[self.docs.active_doc_idx].canvas,
                    )
                };
                // Small canvases keep the exact flat path; past the flat-buffer
                // cap the page is composited and zlib-encoded in row bands
                // (Viewport Streaming) - same bytes, no canvas-sized buffer.
                let pdf_result = if let Some(ink) = ink_page {
                    // Embed the document's CMYK profile so the DeviceCMYK ink (and
                    // the native CMYK vector paths over it) render the colours the
                    // app previews, not the viewer's default CMYK interpretation.
                    let cmyk_profile = self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .cmyk_pdf_profile();
                    crate::core::print::encode_pdf_page_cmyk(&ink, cw, ch, dpi).and_then(|page| {
                        crate::core::print::build_pdf_encoded_with_vectors(
                            &page,
                            &vector_selection.objects,
                            &layout,
                            cmyk_profile.as_deref(),
                        )
                    })
                } else if crate::core::canvas::Canvas::fits_flat_buffer(cw, ch) {
                    let mut rgba = crate::core::print::pdf_raster_base(
                        &self.docs.documents[self.docs.active_doc_idx].canvas,
                        &vector_selection,
                    );
                    if let Some(pp) = &printer_profile {
                        crate::core::cms::convert_srgb_to_rgb_profile(
                            &mut rgba,
                            pp,
                            layout.intent.to_lcms(),
                        );
                    }
                    crate::core::print::build_pdf_with_vectors(
                        &rgba,
                        cw,
                        ch,
                        dpi,
                        &vector_selection.objects,
                        &layout,
                        pdf_icc,
                    )
                } else {
                    let mut stack = self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .clone();
                    let promoted: std::collections::HashSet<u32> = vector_selection
                        .promoted_layer_ids
                        .iter()
                        .copied()
                        .collect();
                    for layer in &mut stack.layers {
                        if promoted.contains(&layer.id) {
                            layer.visible = false;
                        }
                    }
                    crate::core::print::encode_pdf_page_streamed(cw, ch, dpi, |y, rows| {
                        let mut band = stack.flatten_band(cw, ch, y, rows);
                        if let Some(pp) = &printer_profile {
                            crate::core::cms::convert_srgb_to_rgb_profile(
                                &mut band,
                                pp,
                                layout.intent.to_lcms(),
                            );
                        }
                        Ok(band)
                    })
                    .and_then(|page| {
                        crate::core::print::build_pdf_encoded_with_vectors(
                            &page,
                            &vector_selection.objects,
                            &layout,
                            pdf_icc,
                        )
                    })
                };
                match pdf_result {
                    Ok(pdf) => {
                        if actions.print.print_save_pdf {
                            if let Some(window) = self.win.window.as_ref() {
                                let parent = crate::file_io::dialog_parent(window);
                                let mut dialog = rfd::FileDialog::new()
                                    .add_filter("PDF", &["pdf"])
                                    .set_file_name("print.pdf");
                                if let Some(p) = parent {
                                    dialog = dialog.set_parent(&p);
                                }
                                if let Some(mut path) = dialog.save_file() {
                                    path.set_extension("pdf");
                                    match std::fs::write(&path, &pdf) {
                                        Ok(_) => {
                                            let name = path
                                                .file_name()
                                                .and_then(|n| n.to_str())
                                                .unwrap_or("print.pdf");
                                            self.shell.status_msg = if ink_native {
                                                format!("Saved PDF (DeviceCMYK ink): {name}")
                                            } else {
                                                format!("Saved PDF: {name}")
                                            };
                                        }
                                        Err(e) => {
                                            self.shell.status_msg = format!("Error saving PDF: {e}")
                                        }
                                    }
                                }
                            }
                        }
                        if send_via_pdf {
                            let mut tmp = std::env::temp_dir();
                            tmp.push(format!("iai_print_{}.pdf", std::process::id()));
                            let printer = if self.shell.print_selected_printer.is_empty() {
                                None
                            } else {
                                Some(self.shell.print_selected_printer.as_str())
                            };
                            match std::fs::write(&tmp, &pdf) {
                                Ok(_) => match crate::core::print::send_to_printer_with_options(
                                    &tmp,
                                    printer,
                                    self.shell.print_copies,
                                ) {
                                    Ok(_) => {
                                        // The external PDF handler may rescale; say
                                        // why the exact-size GDI path was skipped.
                                        self.shell.status_msg = match &gdi_result {
                                            Some(Err(e)) => format!(
                                                "Direct print failed ({e}) - sent via PDF handler (size may be scaled)"
                                            ),
                                            _ => "Sent to printer".to_string(),
                                        };
                                    }
                                    Err(e) => self.shell.status_msg = e,
                                },
                                Err(e) => {
                                    self.shell.status_msg = format!("Error creating temp PDF: {e}")
                                }
                            }
                        }
                        self.shell.ui.show_print_dialog = false;
                    }
                    Err(e) => self.shell.status_msg = e,
                }
            }
        }
    }
}

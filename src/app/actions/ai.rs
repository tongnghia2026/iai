//! AI-backed actions: Select Subject, Gemini edits, the external bridge and
//! Smart Fill. Split out of app/actions.rs.

use crate::app::render::CanvasEvent;
use crate::app::state::App;

fn api_provider_label(provider: crate::core::ai::settings::AiProvider) -> &'static str {
    match provider {
        crate::core::ai::settings::AiProvider::Gemini => "Gemini API",
        crate::core::ai::settings::AiProvider::OpenAi => "OpenAI API",
    }
}

/// Worker side of the "Copy image" wait: poll the OS clipboard until it holds
/// an image other than what iai wrote (`written`), or give up after 6 s. The
/// source-hash compare keeps a not-yet-updated clipboard from being taken as
/// the result; reading natively avoids the browser's CORS/canvas limits.
fn wait_for_clipboard_result(
    written: Option<u64>,
) -> Result<crate::app::os_clipboard::OsClipboardImage, String> {
    let started = std::time::Instant::now();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(300));
        if let Ok(Some(img)) = crate::app::os_clipboard::read_image() {
            let hash = crate::app::os_clipboard::image_hash(img.width, img.height, &img.pixels);
            if written != Some(hash) {
                return Ok(img);
            }
        }
        if started.elapsed() >= std::time::Duration::from_secs(6) {
            return Err(
                "Trang không chép được ảnh vào clipboard sau khi bấm Copy — thử lại".to_string(),
            );
        }
    }
}

fn ai_placement_succeeded(status: &str) -> bool {
    status.starts_with("Xong") || status.starts_with("Ảnh đã vào")
}

impl App {
    /// Called when user clicks "Select Subject".
    /// Triggers model download (if needed) then starts async inference.
    pub fn do_select_subject(&mut self) {
        use crate::core::select_subject::SubjectStatus;

        {
            let status = self.jobs.select_subject.status.lock().unwrap().clone();
            if matches!(status, SubjectStatus::NoModel | SubjectStatus::Error(_)) {
                drop(status);
                self.jobs.select_subject.download_model_async();
                self.shell.status_msg = self.jobs.select_subject.status_text();
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
                return;
            }
            if matches!(status, SubjectStatus::Downloading { .. }) {
                self.shell.status_msg = "Model is still downloading…".to_string();
                return;
            }
            if matches!(status, SubjectStatus::LoadingModel | SubjectStatus::Running) {
                self.shell.status_msg = self.jobs.select_subject.status_text();
                return;
            }
        }

        let w = self.docs.documents[self.docs.active_doc_idx].canvas.width;
        let h = self.docs.documents[self.docs.active_doc_idx].canvas.height;
        let pixels = {
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .ensure_pixels();
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .pixels
                .clone()
        };

        if self.jobs.select_subject.run_async(pixels, w, h) {
            self.shell.status_msg = self.jobs.select_subject.status_text();
        }
        if let Some(win) = &self.win.window {
            win.request_redraw();
        }
    }

    /// Poll for a completed Select Subject result and apply it to the selection.
    /// Called every frame from the render loop.
    pub fn poll_select_subject(&mut self) {
        if let Some(result) = self.jobs.select_subject.poll_result() {
            match result {
                Ok(mask) => {
                    let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
                    let n = (canvas.width * canvas.height) as usize;
                    if mask.len() == n {
                        let mut cmd = crate::core::command::SelectionCommand::capture_before(
                            "Select Subject",
                            &canvas.selection,
                        );
                        canvas.selection.mask.copy_from_slice(&mask);
                        canvas.selection.active = mask.iter().any(|&v| v > 0);
                        canvas.selection.mask_revision += 1;
                        canvas.selection.mark_bbox_dirty();
                        cmd.capture_after(&canvas.selection);
                        canvas.record(Box::new(cmd));

                        self.apply_canvas_event(CanvasEvent::SelectionChanged);
                        self.shell.status_msg = "Select Subject done".to_string();
                    }
                }
                Err(e) => {
                    self.shell.status_msg = format!("Select Subject error: {}", e);
                }
            }
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
    }

    /// Start an API edit for the active document. Other documents may run in
    /// parallel, up to the engine's global safety cap.
    pub fn do_ai_edit(&mut self, prompt: String) {
        // Sync the panel's current provider + keys into the engine so a just-picked
        // provider works even before the user hits "Lưu".
        self.jobs.ai_engine.settings.provider = self.shell.ui.ai.provider;
        self.jobs.ai_engine.settings.api_key = self.shell.ui.ai.api_key.trim().to_string();
        self.jobs.ai_engine.settings.openai_api_key =
            self.shell.ui.ai.openai_api_key.trim().to_string();

        if self.jobs.ai_engine.settings.active_key().trim().is_empty() {
            self.shell.ui.ai_status = "Chưa có API key — nhập rồi bấm Lưu".to_string();
            return;
        }
        if self.has_only_welcome_placeholder() {
            self.shell.ui.ai_status = "Hãy mở một ảnh trước".to_string();
            return;
        }

        let doc_id = self.docs.documents[self.docs.active_doc_idx].id.0;
        if self.jobs.ai_engine.doc_running(doc_id) || self.jobs.ext.doc_busy(doc_id) {
            self.shell.ui.ai_status =
                "Tài liệu này đang có lệnh AI — đợi xong hoặc bấm Hủy".to_string();
            return;
        }
        if self.jobs.ai_engine.job_count() >= crate::core::ai::edit::MAX_API_JOBS {
            self.shell.ui.ai_status = format!(
                "Đang chạy tối đa {} lệnh API — vui lòng đợi một lệnh hoàn tất",
                crate::core::ai::edit::MAX_API_JOBS
            );
            return;
        }

        let w = self.docs.documents[self.docs.active_doc_idx].canvas.width;
        let h = self.docs.documents[self.docs.active_doc_idx].canvas.height;
        let rgba = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .flatten_for_export();
        self.jobs.ai_engine.settings.add_history(&prompt);
        let _ = self.jobs.ai_engine.settings.save();

        let provider = self.jobs.ai_engine.settings.provider;
        let output_new_file = self.shell.ui.ai.output_new_file;

        self.shell.ui.ai_status =
            if self
                .jobs
                .ai_engine
                .run_async(doc_id, rgba, w, h, prompt, output_new_file)
            {
                format!("Đang gửi sang {}…", api_provider_label(provider))
            } else {
                "Không thể bắt đầu yêu cầu".to_string()
            };
        if let Some(win) = &self.win.window {
            win.request_redraw();
        }
    }

    /// Drain all finished API results and apply them (called every frame from the
    /// redraw loop, next to `poll_select_subject`).
    pub fn poll_ai_edits(&mut self) {
        let finished = self.jobs.ai_engine.poll_finished();
        for job in finished {
            if job.abandoned {
                continue;
            }
            match job.result {
                Ok(r) => {
                    let s = self.place_gemini_result(
                        Some(job.doc_id),
                        r.rgba,
                        r.width,
                        r.height,
                        job.output_new_file,
                    );
                    let success = ai_placement_succeeded(&s);
                    self.shell.ui.ai_status = s;
                    self.notify_done(success);
                }
                Err(e) => {
                    self.shell.ui.ai_status =
                        format!("Lỗi {}: {e}", api_provider_label(job.provider));
                    self.notify_done(false);
                }
            }
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if self.jobs.ai_engine.has_jobs() {
            // Keep repainting so the spinner animates and polling continues.
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
    }

    /// Start the fully local Auto Retouch pipeline.  The worker owns all image
    /// buffers, so the UI remains responsive while each model/fallback stage is
    /// loaded and released sequentially.
    pub fn do_offline_retouch(&mut self) {
        if self.has_only_welcome_placeholder() {
            self.shell.ui.ai_status = "Hãy mở một ảnh trước".to_string();
            return;
        }
        let doc_id = self.docs.documents[self.docs.active_doc_idx].id.0;
        if self.jobs.retouch_engine.is_busy(doc_id)
            || self.jobs.ai_engine.doc_running(doc_id)
            || self.jobs.ext.doc_busy(doc_id)
        {
            self.shell.ui.ai_status =
                "Tài liệu này đang có lệnh AI — đợi xong hoặc bấm Hủy".to_string();
            return;
        }
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let width = canvas.width;
        let height = canvas.height;
        let rgba = canvas.flatten_for_export();
        let config = self.shell.ui.ai.retouch.clone();
        if self
            .jobs
            .retouch_engine
            .run_async(doc_id, rgba, width, height, config)
        {
            self.shell.ui.ai_status = "Đang chạy AI Auto Retouch offline…".to_string();
        } else {
            self.shell.ui.ai_status = "Không thể bắt đầu Auto Retouch".to_string();
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Drain offline retouch workers and place the result as an undoable layer.
    pub fn poll_offline_retouch(&mut self) {
        let finished = self.jobs.retouch_engine.poll_finished();
        for job in finished {
            if job.abandoned {
                continue;
            }
            match job.result {
                Ok(result) => {
                    let timing = crate::core::ai::retouch::benchmark_line(&result);
                    let warnings = result.warnings.clone();
                    let mask_preview = result.mask_preview_rgba;
                    let result_width = result.width;
                    let result_height = result.height;
                    let mut status = self.place_ai_result_named(
                        Some(job.doc_id),
                        result.rgba,
                        result_width,
                        result_height,
                        false,
                        "AI Auto Retouch",
                    );
                    if let Some(mask_rgba) = mask_preview {
                        let mask_status = self.place_ai_result_named(
                            Some(job.doc_id),
                            mask_rgba,
                            result_width,
                            result_height,
                            false,
                            "AI Retouch Mask Preview",
                        );
                        if ai_placement_succeeded(&mask_status) {
                            status.push_str(" — đã thêm layer Preview Masks");
                        }
                    }
                    if !warnings.is_empty() {
                        status.push_str(" — ");
                        status.push_str(&warnings.join("; "));
                    }
                    self.shell.ui.ai_status = format!("{status}. {timing}");
                    self.shell.status_msg = timing;
                    self.notify_done(true);
                }
                Err(e) => {
                    self.shell.ui.ai_status = format!("Lỗi Auto Retouch: {e}");
                    self.notify_done(false);
                }
            }
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if self.jobs.retouch_engine.has_jobs() {
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
    }

    /// Run an edit through the browser extension: send the flattened canvas + prompt
    /// to the user's logged-in Gemini/ChatGPT tab. The result returns via
    /// `poll_ext_bridge`. Site is taken from the currently-selected web source.
    pub fn do_ext_edit(&mut self, prompt: String) {
        if self.has_only_welcome_placeholder() {
            self.jobs.ext.status = "Hãy mở một ảnh trước".to_string();
            return;
        }
        if !self.jobs.ext.connected {
            self.jobs.ext.status =
                "Extension chưa kết nối — cài extension và dán token (xem hướng dẫn)".to_string();
            return;
        }

        let doc_id = self.docs.documents[self.docs.active_doc_idx].id.0;
        if self.jobs.ai_engine.doc_running(doc_id)
            || self.jobs.ext.doc_busy(doc_id)
            || self.jobs.ext_uploads.iter().any(|u| u.doc_id == doc_id)
        {
            self.jobs.ext.status =
                "Tài liệu này đang có lệnh AI — đợi xong hoặc bấm Hủy".to_string();
            return;
        }
        let site = self
            .shell
            .ui
            .ai
            .source
            .web_site()
            .unwrap_or("gemini")
            .to_string();
        let w = self.docs.documents[self.docs.active_doc_idx].canvas.width;
        let h = self.docs.documents[self.docs.active_doc_idx].canvas.height;
        // Flatten + downscale + PNG-encode on a worker; on a large canvas this
        // was a visible pause on the UI thread.
        let stack = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rgba = stack.flatten(w, h);
            let _ = tx.send(crate::app::ext_bridge::PreparedUpload::from_rgba(
                rgba, w, h,
            ));
        });
        self.jobs
            .ext_uploads
            .push(crate::app::ext_bridge::PendingUpload {
                doc_id,
                width: w,
                height: h,
                site: site.clone(),
                prompt: crate::core::ai::guarded_edit_prompt(&prompt),
                output_new_file: self.shell.ui.ai.output_new_file,
                rx,
            });
        let line = format!("Đang chuẩn bị ảnh gửi sang {site}…");
        self.jobs.ext.status = line.clone();
        self.jobs.ext.push_log(&line);
        if let Some(win) = &self.win.window {
            win.request_redraw();
        }
    }

    /// Hand finished upload preparations to the bridge (send now or queue).
    fn poll_ext_uploads(&mut self) {
        use crate::app::ext_bridge::EnqueueOutcome;
        let mut i = 0;
        while i < self.jobs.ext_uploads.len() {
            let result = match self.jobs.ext_uploads[i].rx.try_recv() {
                Ok(result) => result,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    i += 1;
                    continue;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Err("worker dừng bất thường".to_string())
                }
            };
            let job = self.jobs.ext_uploads.remove(i);
            let site = job.site.clone();
            let outcome = match result {
                Ok(upload) => self.jobs.ext.enqueue_prepared(
                    upload,
                    job.width,
                    job.height,
                    &job.site,
                    job.prompt,
                    job.doc_id,
                    job.output_new_file,
                ),
                Err(e) => Err(e),
            };
            let line = match outcome {
                Ok(EnqueueOutcome::Sent) => format!("Đã gửi sang {site} (extension)…"),
                Ok(EnqueueOutcome::Queued(pos)) => {
                    format!("Đã thêm vào hàng chờ (vị trí {pos})…")
                }
                Err(e) => format!("Không gửi được: {e}"),
            };
            self.jobs.ext.status = line.clone();
            self.jobs.ext.push_log(&line);
            if let Some(win) = &self.win.window {
                win.request_redraw();
            }
        }
    }

    pub fn cancel_active_ai(&mut self) {
        if self.has_only_welcome_placeholder() {
            return;
        }
        let doc_id = self.docs.documents[self.docs.active_doc_idx].id.0;
        let api = self.jobs.ai_engine.abandon_doc_job(doc_id);
        let retouch = self.jobs.retouch_engine.cancel_doc(doc_id);
        let preparing = self.jobs.ext_uploads.len();
        self.jobs.ext_uploads.retain(|job| job.doc_id != doc_id);
        let bridge =
            self.jobs.ext.cancel_for_doc(doc_id) || self.jobs.ext_uploads.len() != preparing;
        if api {
            self.shell.ui.ai_status = "Đã hủy lệnh API của ảnh này".to_string();
        }
        if retouch {
            self.shell.ui.ai_status = "Đã hủy Auto Retouch của ảnh này".to_string();
        }
        if bridge {
            self.jobs.ext.status = "Đã hủy lệnh của ảnh này".to_string();
        }
        if let Some(win) = &self.win.window {
            win.request_redraw();
        }
    }

    /// Drain extension-bridge events each frame: status lines land in `ext.status`
    /// (via `drain`), and a finished result is decoded + placed like a Gemini edit.
    /// Background extension work (upload prep, result decode, clipboard wait)
    /// that must keep the frame loop ticking until it lands.
    pub(crate) fn ext_workers_busy(&self) -> bool {
        !self.jobs.ext_uploads.is_empty()
            || !self.jobs.ext_decodes.is_empty()
            || self.edit.pending_ext_clipboard.is_some()
    }

    pub fn poll_ext_bridge(&mut self) {
        self.poll_ext_uploads();
        self.poll_ext_decodes();
        for ev in self.jobs.ext.drain() {
            match ev {
                crate::app::ext_bridge::ExtInbound::Failed { origin, .. } => {
                    // Unsolicited id=0 errors have no origin and should not ring.
                    if origin.is_some() {
                        self.notify_done(false);
                    }
                    if let Some(win) = &self.win.window {
                        win.request_redraw();
                    }
                }
                crate::app::ext_bridge::ExtInbound::Result {
                    image_b64,
                    origin: Some(origin),
                    ..
                } => {
                    // Base64 + PNG decode on a worker; placed by poll_ext_decodes.
                    let (tx, rx) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let _ = tx.send(crate::app::ext_bridge::decode_result(&image_b64));
                    });
                    self.jobs.ext_decodes.push((origin, rx));
                    if let Some(win) = &self.win.window {
                        win.request_redraw();
                    }
                }
                crate::app::ext_bridge::ExtInbound::ResultClipboard {
                    origin: Some(origin),
                    ..
                } => {
                    // The site writes the clipboard asynchronously after the Copy
                    // click. A worker waits for it: a clipboard read can block
                    // while the browser renders the image.
                    let written = self
                        .jobs
                        .ext
                        .last_clipboard_write
                        .or(self.edit.os_clipboard_written);
                    let (tx, rx) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let _ = tx.send(wait_for_clipboard_result(written));
                    });
                    self.edit.pending_ext_clipboard = Some((origin, rx));
                    let s = "Da bam Copy — dang cho trang chep anh vao clipboard...".to_string();
                    self.jobs.ext.push_log(&s);
                    self.jobs.ext.status = s;
                    if let Some(win) = &self.win.window {
                        win.request_redraw();
                    }
                }
                _ => {}
            }
        }

        // Queue advancement happens inside drain, so clipboard ownership must be
        // harvested every frame rather than only from the original button click.
        if let Some(hash) = self.jobs.ext.last_clipboard_write.take() {
            self.edit.os_clipboard_written = Some(hash);
        }

        // Drive the async "Copy image" clipboard wait, if one is in progress.
        self.poll_pending_ext_clipboard();
    }

    fn notify_done(&self, success: bool) {
        if success {
            crate::app::state::notify_beep();
        } else {
            crate::app::state::alert_beep();
        }
        if !self.win.window_focused {
            if let Some(window) = &self.win.window {
                window
                    .request_user_attention(Some(winit::window::UserAttentionType::Informational));
            }
        }
    }

    /// Place a Gemini bridge result. In layer mode it targets the ORIGIN document
    /// (by stable id) so a doc switch mid-request can't misplace it; if that doc is
    /// not the active one, the layer is added to its data and shows on switch.
    pub(crate) fn place_gemini_result(
        &mut self,
        origin_id: Option<u32>,
        rgba: Vec<u8>,
        w: u32,
        h: u32,
        output_new_file: bool,
    ) -> String {
        self.place_ai_result_named(origin_id, rgba, w, h, output_new_file, "Gemini")
    }

    fn place_ai_result_named(
        &mut self,
        origin_id: Option<u32>,
        rgba: Vec<u8>,
        w: u32,
        h: u32,
        output_new_file: bool,
        layer_name: &str,
    ) -> String {
        // New-file mode: the result becomes its own document — origin irrelevant.
        if output_new_file {
            let id = crate::core::document::DocumentId(self.docs.next_doc_id);
            self.docs.next_doc_id += 1;
            let canvas = crate::core::canvas::Canvas::from_rgba(rgba, w, h);
            let doc = crate::core::document::Document::from_canvas(id, canvas, None);
            if self.has_only_welcome_placeholder() {
                self.docs.documents[0] = doc;
                self.docs.active_doc_idx = 0;
            } else {
                self.docs.documents.push(doc);
                self.docs.active_doc_idx = self.docs.documents.len() - 1;
            }
            self.touch_doc_mru();
            self.docs.current_file = None;
            self.shell.ui.show_welcome = false;
            if let Some(gpu) = &mut self.win.gpu {
                gpu.resize_canvas_texture(w, h);
            }
            self.fit_canvas_to_screen();
            self.push_canvas_uniforms();
            self.upload_full();
            self.upload_selection_mask();
            return "Xong — kết quả mở thành file mới".to_string();
        }

        // Layer mode: find the origin doc by id; fall back to the active doc.
        let idx = match origin_id {
            Some(id) => match self.docs.documents.iter().position(|d| d.id.0 == id) {
                Some(i) => i,
                None => return "Document gốc đã đóng — bỏ ảnh".to_string(),
            },
            None => self.docs.active_doc_idx,
        };

        // The result keeps the model's native resolution, which may be smaller
        // than the canvas (the source is downscaled for upload and models cap
        // their output). Place it as a layer CENTRED on the canvas rather than
        // upscaling it to fill — that upscale is what made web results blurrier
        // than the browser. The undo snapshot still records the CANVAS size.
        let cw = self.docs.documents[idx].canvas.width;
        let ch = self.docs.documents[idx].canvas.height;

        let tiles = crate::core::tile::TileMap::from_rgba(&rgba, w, h);
        let mut cmd = crate::core::command::LayerStructureCommand::capture_before(
            layer_name,
            &self.docs.documents[idx].canvas.layer_stack,
            cw,
            ch,
        );
        for l in &mut self.docs.documents[idx].canvas.layer_stack.layers {
            l.selected = false;
        }
        let new_idx = self.docs.documents[idx].canvas.layer_stack.add_layer(w, h);
        {
            let layer = &mut self.docs.documents[idx].canvas.layer_stack.layers[new_idx];
            layer.name = layer_name.to_string();
            layer.tiles = tiles;
            layer.width = w;
            layer.height = h;
            layer.offset = (
                ((cw as i64 - w as i64) / 2) as i32,
                ((ch as i64 - h as i64) / 2) as i32,
            );
            layer.selected = true;
        }
        self.docs.documents[idx].canvas.layer_stack.active_idx = new_idx;
        cmd.capture_after(&self.docs.documents[idx].canvas.layer_stack, cw, ch);
        self.docs.documents[idx].canvas.record(Box::new(cmd));
        self.docs.documents[idx].canvas.layer_revision += 1;

        if idx == self.docs.active_doc_idx {
            self.upload_full();
            self.upload_selection_mask();
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            format!("Xong — layer {layer_name} trên Background")
        } else {
            // Result went to a non-active doc; it shows on switch (switch_to_doc
            // re-uploads). Tell the user where it landed.
            let title = self.docs.documents[idx].title.clone();
            format!("Ảnh đã vào document \"{title}\" — chuyển qua tab đó để xem")
        }
    }

    /// Collect a browser "Copy image" result from the clipboard worker.
    pub(crate) fn poll_pending_ext_clipboard(&mut self) {
        let Some((origin, rx)) = self.edit.pending_ext_clipboard.take() else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.edit.pending_ext_clipboard = Some((origin, rx));
                return;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err("Không đọc được clipboard (worker dừng bất thường)".to_string())
            }
        };
        self.finish_ext_result(
            origin,
            result.map(|img| (img.pixels, img.width, img.height)),
        );
    }

    /// Place extension results decoded by workers.
    fn poll_ext_decodes(&mut self) {
        let mut i = 0;
        while i < self.jobs.ext_decodes.len() {
            let result = match self.jobs.ext_decodes[i].1.try_recv() {
                Ok(result) => result,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    i += 1;
                    continue;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Err("worker dừng bất thường".to_string())
                }
            };
            let (origin, _) = self.jobs.ext_decodes.remove(i);
            self.finish_ext_result(
                origin,
                result.map_err(|e| format!("Lỗi ảnh extension: {e}")),
            );
        }
    }

    /// Place a finished extension image at the model's native resolution
    /// (upscaling to the source canvas made web results blurrier than the
    /// browser showed) and report it.
    fn finish_ext_result(
        &mut self,
        origin: crate::app::ext_bridge::EditOrigin,
        result: Result<(Vec<u8>, u32, u32), String>,
    ) {
        let (line, success) = match result {
            Ok((rgba, w, h)) => {
                let s = self.place_gemini_result(
                    Some(origin.doc_id),
                    rgba,
                    w,
                    h,
                    origin.output_new_file,
                );
                let ok = ai_placement_succeeded(&s);
                (s, ok)
            }
            Err(e) => (e, false),
        };
        self.jobs.ext.push_log(&line);
        self.jobs.ext.status = line;
        self.notify_done(success);
        if let Some(win) = &self.win.window {
            win.request_redraw();
        }
    }

    /// Open the Smart Fill dialog (method picker). Requires a selection.
    pub(crate) fn request_smart_fill_fill(&mut self) {
        if !self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .selection
            .active
        {
            self.shell.status_msg = "Smart Fill cần một vùng chọn".to_string();
            return;
        }
        self.shell.ui.show_smart_fill_dialog = true;
    }

    /// Edit → Smart Fill. One-shot (no live preview): synthesise pixels
    /// inside the active selection from the surrounding texture and commit an
    /// undoable tile change. Closes any open preview dialog first so they can't
    /// fight over the layer's tiles.
    /// Run the fill. `use_ai` = the method chosen in the dialog (true → LaMa when
    /// its model is downloaded, else classic + a background download is started).
    pub(crate) fn do_smart_fill_fill(&mut self, use_ai: bool) {
        self.shell.ui.show_smart_fill_dialog = false;
        self.shell.ui.show_filter_dialog = false;
        self.cancel_filter_preview();
        self.shell.ui.show_adjustment_dialog = false;
        self.cancel_adjustment_preview();
        self.abandon_develop_session();

        if !self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .selection
            .active
        {
            self.shell.status_msg = "Smart Fill cần một vùng chọn".to_string();
            return;
        }

        let lama_ready = use_ai && crate::core::lama::is_available();
        if use_ai && !lama_ready {
            crate::core::lama::ensure_downloading();
        }

        let ok = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .smart_fill_fill(lama_ready);
        if ok {
            self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
            self.shell.status_msg = if lama_ready {
                "Applied Smart Fill (AI)".to_string()
            } else if use_ai {
                match crate::core::lama::status_text() {
                    Some(s) => format!("Applied Smart Fill (classic) — {s}"),
                    None => "Applied Smart Fill (classic)".to_string(),
                }
            } else {
                "Applied Smart Fill (classic)".to_string()
            };
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        } else {
            self.shell.status_msg =
                "Smart Fill cần một layer raster mở khoá và một vùng chọn".to_string();
        }
    }
}

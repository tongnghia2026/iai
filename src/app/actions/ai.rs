//! The AI-backed actions of the core app: Select Subject, Smart Heal and
//! Smart Fill. Ai Image Studio's are in `ai_studio` (the Pro part).

use crate::app::render::CanvasEvent;
use crate::app::state::App;

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

        let doc_id = self.docs.active().id.0;
        let w = self.docs.active().canvas.width;
        let h = self.docs.active().canvas.height;
        let pixels = {
            self.docs.active_mut().canvas.ensure_pixels();
            self.docs.active().canvas.pixels.clone()
        };

        if self.jobs.select_subject.run_async(doc_id, pixels, w, h) {
            self.shell.status_msg = self.jobs.select_subject.status_text();
        }
        if let Some(win) = &self.win.window {
            win.request_redraw();
        }
    }

    /// Poll for a completed Select Subject result and apply it to the selection.
    /// Called every frame from the render loop.
    pub fn poll_select_subject(&mut self) {
        if let Some((doc_id, result)) = self.jobs.select_subject.poll_result() {
            match result {
                Ok(mask) => {
                    // Apply to the document the job was started on, not whatever
                    // tab is active now — the user may have switched tabs while
                    // inference ran. Falling back to the active doc for a legacy
                    // job with no id keeps old behaviour.
                    let idx = match doc_id {
                        Some(id) => self.docs.documents.iter().position(|d| d.id.0 == id),
                        None => Some(self.docs.active_doc_idx),
                    };
                    let Some(idx) = idx else {
                        self.shell.status_msg =
                            "Select Subject: tab gốc đã đóng — bỏ kết quả".to_string();
                        if let Some(w) = &self.win.window {
                            w.request_redraw();
                        }
                        return;
                    };
                    let active = self.docs.active_doc_idx;
                    let engine = if self.jobs.select_subject.used_gpu() {
                        "GPU"
                    } else {
                        "CPU"
                    };
                    let n = {
                        let c = &self.docs.documents[idx].canvas;
                        (c.width * c.height) as usize
                    };
                    if mask.len() != n {
                        // Canvas was resized (e.g. cropped) while inference ran.
                        self.shell.status_msg =
                            "Select Subject: kích thước ảnh đã đổi — thử lại".to_string();
                    } else {
                        {
                            let canvas = &mut self.docs.documents[idx].canvas;
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
                        }
                        if idx == active {
                            self.apply_canvas_event(CanvasEvent::SelectionChanged);
                            self.shell.status_msg = format!("Select Subject xong ({engine})");
                        } else {
                            // Non-active tab: refresh its bbox now so the mask is
                            // correct when the user switches to it (switch_to_doc
                            // re-uploads the selection on activation).
                            self.docs.documents[idx].canvas.selection.refresh_bbox();
                            let title = self.docs.documents[idx].title.clone();
                            self.shell.status_msg = format!(
                                "Select Subject xong ({engine}) ở tab \"{title}\" — chuyển qua tab đó để xem"
                            );
                        }
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

    /// Hand a large Smart Repair stroke to the AI inpainter on a worker
    /// thread (seconds on a CPU); the painted wash stays until
    /// [`Self::poll_repair_ai`] commits it. Returns the stroke back when it
    /// should be healed right away instead (small, no model, or no thread).
    pub(in crate::app) fn start_repair_ai(
        &mut self,
        stroke: crate::tools::clone_tool::SmartHealStroke,
        wash: Option<std::sync::Arc<crate::ui::CloneSourcePreview>>,
    ) -> Option<crate::tools::clone_tool::SmartHealStroke> {
        use crate::core::smart_fill::{SPOT_HEAL_AI_MIN_AREA, SPOT_HEAL_COVER};
        let area = stroke
            .cover
            .iter()
            .filter(|&&v| v >= SPOT_HEAL_COVER)
            .count();
        if area < SPOT_HEAL_AI_MIN_AREA
            || self.jobs.repair_ai.is_some()
            || !crate::core::lama::is_available()
        {
            return Some(stroke);
        }
        let doc = self.docs.active_mut();
        let doc_id = doc.id.0;
        let Some(mut work) = doc.canvas.spot_heal_begin(
            stroke.x0,
            stroke.y0,
            stroke.w,
            stroke.h,
            &stroke.cover,
            stroke.opacity,
        ) else {
            return Some(stroke);
        };
        let (mut pixels, cover, w, h, opacity) = work.take_job();
        let spawned = std::thread::Builder::new()
            .name("repair-ai".into())
            .spawn(move || {
                crate::core::smart_fill::fill_spot_with(&mut pixels, w, h, &cover, opacity, true)
                    .then_some(pixels)
            });
        match spawned {
            Ok(handle) => {
                self.jobs.repair_ai = Some(crate::app::background_jobs::RepairAiJob {
                    doc_id,
                    work,
                    overlay: wash,
                    handle,
                });
                self.shell.status_msg = "AI đang xoá vùng vừa tô… (vài giây)".to_string();
                None
            }
            Err(_) => Some(stroke),
        }
    }

    /// Commit a finished AI Repair heal into the document it was painted on.
    pub(crate) fn poll_repair_ai(&mut self) {
        if !self
            .jobs
            .repair_ai
            .as_ref()
            .is_some_and(|job| job.handle.is_finished())
        {
            return;
        }
        let Some(job) = self.jobs.repair_ai.take() else {
            return;
        };
        let pixels = job.handle.join().ok().flatten();
        let idx = self
            .docs
            .documents
            .iter()
            .position(|d| d.id.0 == job.doc_id);
        self.shell.status_msg = match (pixels, idx) {
            (None, _) => "Smart Heal (AI): không có gì thay đổi".to_string(),
            (_, None) => "Smart Heal (AI): tab gốc đã đóng — bỏ kết quả".to_string(),
            (Some(pixels), Some(idx)) => {
                let mut work = job.work;
                work.set_pixels(pixels);
                match self.docs.documents[idx]
                    .canvas
                    .spot_heal_finish(work, "Smart Heal (AI)")
                {
                    Ok(true) => {
                        if idx == self.docs.active_doc_idx {
                            self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
                        }
                        "Smart Heal (AI)".to_string()
                    }
                    Ok(false) => "Smart Heal (AI): layer đã bị xoá — bỏ kết quả".to_string(),
                    Err(why) => format!("Smart Heal (AI): {why} — bỏ kết quả"),
                }
            }
        };
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Open the Smart Fill dialog (method picker). Requires a selection.
    pub(crate) fn request_smart_fill_fill(&mut self) {
        if !self.docs.active().canvas.selection.active {
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

        if !self.docs.active().canvas.selection.active {
            self.shell.status_msg = "Smart Fill cần một vùng chọn".to_string();
            return;
        }

        let lama_ready = use_ai && crate::core::lama::is_available();
        if use_ai && !lama_ready {
            crate::core::lama::ensure_downloading();
        }

        let ok = self.docs.active_mut().canvas.smart_fill_fill(lama_ready);
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

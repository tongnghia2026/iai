//! The RedrawRequested path for the main window, extracted verbatim from
//! the window_event match arm.

use crate::app::state::App;
use crate::extension::tool::ToolCtx;
use winit::event_loop::ActiveEventLoop;

impl App {
    /// The main window's RedrawRequested arm, verbatim: GPU recovery, frame
    /// build, ants cadence and the needs-another-frame poll.
    pub(in crate::app) fn on_main_redraw(&mut self, event_loop: &ActiveEventLoop) {
        if self.win.rendering {
            return;
        }
        self.win.rendering = true;

        // A lost GPU device (driver reset / TDR — typically after a
        // heavy AI inference or an idle power cycle) invalidates every
        // texture and pipeline; without this rebuild the app freezes on
        // an endless stream of validation errors and eventually dies.
        if self
            .win
            .gpu
            .as_ref()
            .is_some_and(|g| g.device_lost.load(std::sync::atomic::Ordering::SeqCst))
        {
            self.recover_gpu_device();
        }

        if matches!(
            self.win.startup_phase,
            crate::app::state::StartupPhase::Loading(_)
        ) {
            if !self.finish_startup_if_ready(event_loop) {
                self.win.rendering = false;
                return;
            }
        }

        self.poll_file_dialog(event_loop);
        self.poll_pdf_export();
        self.poll_raw_previews(event_loop);
        self.poll_loads();
        self.poll_iai_projects();
        self.poll_pdf_probe();
        self.poll_pdf_render();
        self.poll_pdf_page_render();
        self.flush_dropped_pdf_page_files();
        self.poll_pdf_page_insert();
        self.enter_pending_develop(event_loop);
        self.poll_develop_bake_all();
        self.poll_reload_open_file();
        self.poll_transform_commit();
        if !self.docs.crash_recovery_checked {
            self.docs.crash_recovery_checked = true;
            self.check_crash_recovery();
        }
        self.maybe_autosave();

        if self.win.pending_view_change {
            let flushed_view = self.flush_pending_view_change();
            if flushed_view {
                self.win.last_cursor_radius = 0;
                self.sync_cursor(event_loop);
            }
        }

        if !self.edit.pending_stroke_inputs.is_empty() {
            let events: Vec<_> = self.edit.pending_stroke_inputs.drain(..).collect();
            let mut did_paint = false;
            for event in events {
                let mut ctx = ToolCtx::new(
                    &mut self.docs.documents[self.docs.active_doc_idx],
                    self.edit.fg_color,
                    self.edit.bg_color,
                    self.edit.view.zoom,
                    self.edit.view.offset_x,
                    self.edit.view.offset_y,
                );
                let _ = self.edit.tools.on_drag(event, &mut ctx);
                did_paint = true;
            }
            if did_paint {
                self.flush_canvas();
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .selection
                    .refresh_bbox();
                if self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .selection
                    .active
                {
                    self.upload_selection_mask();
                }
            }
        }

        if self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .selection
            .active
        {
            let now = std::time::Instant::now();
            let delta = now.duration_since(self.win.last_selection_push);
            if delta.as_millis() >= 16 {
                self.push_selection_uniforms();
                self.win.last_selection_push = now;
            }
        }

        self.poll_select_subject();
        self.poll_ai_edits();
        self.poll_ext_bridge();

        self.update_refine_overlay_tex();

        // Background jobs above must still make progress while minimized, but
        // never build egui or acquire/present a hidden surface. A degenerate egui
        // frame persists collapsed dock dimensions AND clamps floating panels to a
        // tiny screen rect (they reappear stuck at the left edge on restore) — so
        // skip the frame whenever the window is minimized or near-zero-sized. The
        // real min inner size is 640x400, so a <200px dimension only ever occurs
        // in the transient minimize/restore state, never for a real window.
        let degenerate_size = self
            .win
            .window
            .as_ref()
            .map(|window| window.inner_size())
            .is_some_and(|size| size.width < 200 || size.height < 200);
        let minimized = self
            .win
            .window
            .as_ref()
            .is_some_and(|window| window.is_minimized() == Some(true));
        if self.win.window_occluded || degenerate_size || minimized {
            self.win.rendering = false;
            return;
        }

        if let Some(window) = self.win.window.as_ref().cloned() {
            let mut main_frame_presented = self.win.gpu.is_none();
            // "Ants-only" frame: this redraw was scheduled purely by the
            // marching-ants timer (ants_redraw_pending — any other window
            // event clears it) AND nothing carries new UI state: egui wants
            // no repaint, no input/async/preview/transform is in flight.
            // Reuse the last tessellated UI instead of rebuilding the whole
            // immediate-mode tree ~15×/sec; only the selection `time` uniform
            // (pushed above) advances. Every async poll still ran above, so
            // a completing job can't be stranded by this skip.
            let ants_only = self.win.ants_redraw_pending
                && !self.win.cached_egui_primitives.is_empty()
                && self.win.egui_repaint_deadline.is_none()
                && self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .selection
                    .active
                && !self.edit.input.painting
                && self.edit.pending_stroke_inputs.is_empty()
                && !self.win.pending_view_change
                && !self.win.interactive_recompose_pending
                && self.edit.transform_state.is_none()
                && self.edit.pending_fill.is_none()
                && self.edit.pending_stroke.is_none()
                && !self.dev.develop_gpu_preview_dirty
                && self.shell.adjustment_preview_pending.is_none()
                && !self.jobs.select_subject.is_busy()
                && !self.jobs.ai_engine.has_jobs()
                && self.jobs.pending_file_dialog.is_none()
                && self.jobs.pending_pdf_export.is_none()
                && self.jobs.pending_loads.is_empty()
                && !self.jobs.ext.busy()
                && !crate::core::lama::is_downloading();
            self.win.ants_redraw_pending = false;

            let pixels_per_point = window.scale_factor() as f32;
            let backdrop = self.shell.ui.theme_mode.palette().canvas_backdrop();

            if ants_only {
                let hide_selection_outline = self.edit.show_refine_panel
                    && self.edit.tools.active_id() == crate::tools::ToolId::RefineBrush;
                let draw_selection = self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .selection
                    .active
                    && !hide_selection_outline;
                let canvas_clip = self.canvas_screen_clip();
                let draw_canvas = self.main_window_draws_canvas();
                let empty_delta = egui::TexturesDelta::default();
                if let Some(gpu) = &mut self.win.gpu {
                    let presented = gpu.render(
                        &self.win.egui_ctx,
                        &self.win.cached_egui_primitives,
                        &empty_delta,
                        pixels_per_point,
                        draw_selection,
                        None,
                        backdrop,
                        canvas_clip,
                        draw_canvas,
                    );
                    main_frame_presented = presented;
                    // A dropped frame (surface Outdated/Lost, now
                    // reconfigured) must retry, or the window shows its
                    // stale last frame until an external repaint.
                    if !presented {
                        window.request_redraw();
                    }
                }
                if !self.win.window_visible {
                    window.set_visible(true);
                    self.win.window_visible = true;
                    self.win.startup_focus_until =
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(1500));
                    // Activate on first reveal so Windows gives the hidden-created
                    // borderless window a real WM_SETFOCUS (IME association),
                    // otherwise it dings on every keystroke until minimise+restore.
                    window.focus_window();
                }
            } else {
                // Push the theme into egui's global Visuals only when it
                // actually changed (set_global_style takes effect for this
                // frame's run_ui below).
                if self.win.theme_applied != Some(self.shell.ui.theme_mode) {
                    crate::ui::theme::apply_theme(&self.win.egui_ctx, self.shell.ui.theme_mode);
                    self.win.theme_applied = Some(self.shell.ui.theme_mode);
                }
                let ui_data = self.collect_ui_data();
                let (primitives, textures_delta, actions, repaint_delay) = crate::ui::build(
                    &self.win.egui_ctx,
                    &mut self.win.egui_state,
                    &*window,
                    &ui_data,
                );
                self.win.egui_repaint_deadline = if repaint_delay == std::time::Duration::MAX {
                    None
                } else {
                    Some(std::time::Instant::now() + repaint_delay)
                };
                self.apply_ui_actions(actions, event_loop);
                self.flush_pending_adjustment_preview();

                if self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .selection
                    .active
                {
                    self.push_selection_uniforms();
                }

                if let Some(fill_color) = self.edit.pending_fill.take() {
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .fill_solid_color(fill_color);
                }

                if let Some(stroke) = self.edit.pending_stroke.take() {
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .stroke_selection(stroke);
                    self.apply_canvas_event(crate::app::render::CanvasEvent::LayerPixelsChanged);
                }

                self.flush_canvas();
                self.flush_pending_interactive_recompose();

                self.flush_develop_gpu_preview();

                // Recompute after apply_ui_actions, which may have toggled
                // the selection / refine panel or changed the view.
                let hide_selection_outline = self.edit.show_refine_panel
                    && self.edit.tools.active_id() == crate::tools::ToolId::RefineBrush;
                let draw_selection = self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .selection
                    .active
                    && !hide_selection_outline;
                let canvas_clip = self.canvas_screen_clip();
                let draw_canvas = self.main_window_draws_canvas();

                self.win.cached_egui_primitives = primitives;
                if let Some(gpu) = &mut self.win.gpu {
                    let presented = gpu.render(
                        &self.win.egui_ctx,
                        &self.win.cached_egui_primitives,
                        &textures_delta,
                        pixels_per_point,
                        draw_selection,
                        None,
                        backdrop,
                        canvas_clip,
                        draw_canvas,
                    );
                    main_frame_presented = presented;
                    // A dropped frame (surface Outdated/Lost, now
                    // reconfigured) must retry, or the window shows its
                    // stale last frame until an external repaint.
                    if !presented {
                        window.request_redraw();
                    }
                }
                if !self.win.window_visible {
                    window.set_visible(true);
                    self.win.window_visible = true;
                    self.win.startup_focus_until =
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(1500));
                    // Activate on first reveal so Windows gives the hidden-created
                    // borderless window a real WM_SETFOCUS (IME association),
                    // otherwise it dings on every keystroke until minimise+restore.
                    window.focus_window();
                }
            }
            if main_frame_presented {
                self.retire_develop_window_after_main_present();
            }
        }
        // The Develop window samples the shared compositor, so it must
        // resample after the main window recomposites (e.g. a Develop
        // slider changed the live preview).
        if let Some(dev) = &self.win.develop_window {
            dev.request_redraw();
        }
        self.win.rendering = false;
    }
}

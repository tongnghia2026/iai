//! The Develop stage's second OS window (Track D / D1 spike).
//!
//! This is the infrastructure spike for a Camera-Raw-style Develop window that
//! lives in its own top-level OS window instead of an in-canvas `egui::Window`.
//! It proves the risky parts before the real UI moves in (D2): creating a second
//! winit window at runtime, giving it its own egui context (with the Vietnamese
//! font + theme so it does not render tofu), routing its events by `WindowId`,
//! sharing the one wgpu device, and tearing it all down cleanly.
//!
//! The window hosts the real Develop controls (D2.3) and shows the canvas with
//! its own zoom/pan in both compositor modes: Mode A re-samples the shared
//! canvas-sized texture, Mode B (large canvases — every RAW in practice) has
//! the compositor bake this window's view while it is open (D2.4).

use crate::app::develop_shell::DevelopTool;
use crate::app::state::App;
use egui_phosphor::regular as ph;
use std::sync::Arc;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::Window;

fn develop_commit_needs_settled_frame(
    gpu_preview_active: bool,
    processing: bool,
    receiver_active: bool,
) -> bool {
    gpu_preview_active || processing || receiver_active
}

fn develop_session_entry_blocks_commit(decode_in_flight: bool, decode_failed: bool) -> bool {
    decode_in_flight || decode_failed
}

fn develop_commit_targets_entry(
    entry: crate::core::document::DocumentId,
    active: crate::core::document::DocumentId,
) -> bool {
    entry == active
}

fn develop_display_lut_active(
    proof_enabled: bool,
    display_cms_enabled: bool,
    document_is_wide_gamut: bool,
) -> f32 {
    if proof_enabled || display_cms_enabled || document_is_wide_gamut {
        1.0
    } else {
        0.0
    }
}

#[derive(Clone, Copy)]
struct DevelopWorkArea {
    origin: winit::dpi::PhysicalPosition<i32>,
    size: winit::dpi::PhysicalSize<u32>,
    scale_factor: f64,
}

#[cfg(windows)]
fn native_work_area(
    window: &Window,
) -> Option<(
    winit::dpi::PhysicalPosition<i32>,
    winit::dpi::PhysicalSize<u32>,
)> {
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let handle = window.window_handle().ok()?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return None;
    };
    let hwnd = handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_null() {
        return None;
    }
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
        return None;
    }
    let width = info.rcWork.right.saturating_sub(info.rcWork.left) as u32;
    let height = info.rcWork.bottom.saturating_sub(info.rcWork.top) as u32;
    (width > 0 && height > 0).then_some((
        winit::dpi::PhysicalPosition::new(info.rcWork.left, info.rcWork.top),
        winit::dpi::PhysicalSize::new(width, height),
    ))
}

fn develop_work_area(
    main: Option<&Window>,
    monitor: &winit::monitor::MonitorHandle,
) -> DevelopWorkArea {
    let mut area = DevelopWorkArea {
        origin: monitor.position(),
        size: monitor.size(),
        scale_factor: monitor.scale_factor(),
    };
    #[cfg(windows)]
    if let Some((origin, size)) = main.and_then(native_work_area) {
        area.origin = origin;
        area.size = size;
    }
    area
}

fn centre_window_in_work_area(window: &Window, area: DevelopWorkArea) {
    let mut outer = window.outer_size();
    if outer.width > area.size.width || outer.height > area.size.height {
        let inner = window.inner_size();
        let excess_w = outer.width.saturating_sub(area.size.width);
        let excess_h = outer.height.saturating_sub(area.size.height);
        let requested = winit::dpi::PhysicalSize::new(
            inner.width.saturating_sub(excess_w).max(320),
            inner.height.saturating_sub(excess_h).max(240),
        );
        let _ = window.request_inner_size(requested);
        outer = window.outer_size();
    }
    let x = area.origin.x + (area.size.width.saturating_sub(outer.width) / 2) as i32;
    let y = area.origin.y + (area.size.height.saturating_sub(outer.height) / 2) as i32;
    window.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
}

/// Width (logical points) of the Develop window's right-hand controls panel.
/// The canvas viewport is everything between it and the left tool rail.
const DEVELOP_PANEL_W: f32 = 360.0;

/// Width (logical points) of the Develop window's left tool rail (Hand / Zoom /
/// White-balance eyedropper — D5). The canvas viewport starts to its right.
const DEVELOP_RAIL_W: f32 = 46.0;

/// Height (logical points) of the bottom filmstrip panel, shown for a
/// multi-image session. The canvas viewport sits above it.
const FILMSTRIP_H: f32 = 92.0;

/// Filmstrip thumbnail height (logical points); width follows the aspect.
const FILMSTRIP_THUMB_H: f32 = 64.0;

/// Do not replace the final edited preview with a progress/original-image frame
/// for a commit that finishes almost immediately. Slow commits still get the
/// normal progress UI after this grace period.
const COMMIT_PROGRESS_DELAY: std::time::Duration = std::time::Duration::from_millis(180);

impl App {
    /// Enter the Develop stage: start a Develop session on the active document
    /// and open its OS window (own egui context/state — fonts + theme applied
    /// so text is not tofu — and a GPU surface on the shared device). Every
    /// Develop entry (menu, Ctrl+Shift+A, a freshly-loaded RAW) lands here. If
    /// the OS window cannot be created the session stays alive and the
    /// in-canvas dialog shows instead (it is only gated off while
    /// `develop_window` is open), so Develop still works.
    pub fn open_develop_window(&mut self, event_loop: &ActiveEventLoop) {
        if self.win.develop_window.is_some() || self.win.retiring_develop_window.is_some() {
            return;
        }
        // The session first: no point in a window without one. Starts the live
        // preview and sets ui.show_develop_dialog (which the in-canvas dialog
        // uses as its fallback gate).
        if !self.begin_develop_preview(crate::core::develop::DevelopSettings::default()) {
            self.shell.status_msg = "Develop requires an unlocked raster layer".to_string();
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return;
        }
        let active_id = self.docs.documents[self.docs.active_doc_idx].id;
        self.develop_session_push(active_id, false);
        if self.win.gpu.is_none() {
            return; // Session open; the in-canvas dialog takes over.
        }

        // Open large and centred in the usable work area. On Windows this is the
        // monitor rcWork (taskbar excluded), not the full monitor rectangle.
        // Leave room for native decorations; the outer rect is clamped again
        // after creation when its exact title-bar/border size is known.
        let monitor = self
            .win
            .window
            .as_ref()
            .and_then(|w| w.current_monitor())
            .or_else(|| event_loop.primary_monitor());
        let (size, position, work_area) = match &monitor {
            Some(mon) => {
                let area = develop_work_area(self.win.window.as_deref(), mon);
                let sf = area.scale_factor;
                let work_w = area.size.width as f64 / sf;
                let work_h = area.size.height as f64 / sf;
                let max_inner_w = (work_w - 32.0 / sf).max(640.0);
                let max_inner_h = (work_h - 72.0 / sf).max(400.0);
                let win_w = (work_w * 0.82).max(1000.0).min(2400.0).min(max_inner_w);
                let win_h = (work_h * 0.86).max(680.0).min(1500.0).min(max_inner_h);
                let x = area.origin.x as f64 + (area.size.width as f64 - win_w * sf) / 2.0;
                let y = area.origin.y as f64 + (area.size.height as f64 - win_h * sf) / 2.0;
                (
                    winit::dpi::LogicalSize::new(win_w, win_h),
                    Some(winit::dpi::PhysicalPosition::new(x, y)),
                    Some(area),
                )
            }
            None => (winit::dpi::LogicalSize::new(1500.0, 950.0), None, None),
        };

        #[allow(unused_mut)]
        let mut attrs = Window::default_attributes()
            .with_title(self.develop_window_title())
            .with_inner_size(size)
            .with_min_inner_size(winit::dpi::LogicalSize::new(640u32, 400u32));
        if let Some(pos) = position {
            attrs = attrs.with_position(pos);
        }

        // Windows: own the Develop window to the main window so it floats above
        // it and minimises with it — the ACR-dialog behaviour. The HWND comes
        // from raw-window-handle 0.6 (winit 0.30.13 has no `WindowExtWindows::hwnd`).
        #[cfg(windows)]
        {
            use winit::platform::windows::WindowAttributesExtWindows;
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Some(main) = &self.win.window {
                if let Ok(handle) = main.window_handle() {
                    if let RawWindowHandle::Win32(h) = handle.as_raw() {
                        attrs = attrs.with_owner_window(h.hwnd.get());
                    }
                }
            }
        }

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                self.shell.status_msg = format!("Could not open the Develop window: {e}");
                return;
            }
        };
        if let Some(area) = work_area {
            centre_window_in_work_area(&window, area);
        }

        // A dedicated egui context for this window. It must carry the same fonts
        // (Vietnamese glyphs + phosphor icons) and theme as the main context, or
        // it renders tofu / the wrong colours.
        let ctx = egui::Context::default();
        if let Some(fonts) = &self.win.ui_fonts {
            ctx.set_fonts(fonts.clone());
        }
        crate::ui::theme::apply_theme(&ctx, self.shell.ui.theme_mode);

        let egui_state = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            None,
            None,
            // Real adapter texture ceiling (egui's default is a 2048px guess).
            self.win
                .gpu
                .as_ref()
                .map(|g| g.max_texture_dimension as usize),
        );

        if let Some(gpu) = &mut self.win.gpu {
            if let Err(e) = gpu.open_develop_surface(Arc::clone(&window)) {
                self.shell.status_msg = e;
                return;
            }
        }

        window.request_redraw();
        self.win.develop_egui_ctx = Some(ctx);
        self.win.develop_egui_state = Some(egui_state);
        self.win.develop_window = Some(window);
        self.refresh_develop_system_display_profile();
        // Start from a fresh fit view each time the window opens.
        self.dev.develop_view_fit = true;
        self.dev.develop_pan_drag = None;
        self.dev.develop_composited_view = None;
        self.shell.status_msg = "Opened the Develop window".to_string();
        // Repaint the main window so its soft-modal (locked) state shows.
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Cancel path (X / Escape): discard the Develop session — including the
    /// transient RAW document, if this session is a RAW Develop stage — and
    /// tear the window down.
    pub fn close_develop_window(&mut self) {
        if self.win.develop_window.is_none() {
            return;
        }
        // Mid-"Open Image": the sequential bake must run to completion (its
        // results are already landing as undoable commits).
        if self.dev.develop_bake_all.is_some() {
            self.shell.status_msg = "Developing images — please wait".to_string();
            return;
        }
        self.cancel_develop_preview();
        self.shell.ui.show_develop_dialog = false;
        // Stop a multi-RAW preview extractor from appending more filmstrip
        // placeholders after this session has been cancelled.
        self.jobs.pending_raw_previews.clear();
        // Cancelling the Develop stage discards the whole session's transient
        // RAW documents (pre-editor flow: Open Image is what creates them).
        self.discard_develop_session_docs();
        self.teardown_develop_window();
        self.shell.status_msg = "Closed the Develop window".to_string();
    }

    /// Commit path ("Open Image"): bake only the active filmstrip image, matching
    /// Camera Raw's single-selection workflow. Other transient RAW placeholders
    /// are discarded instead of being full-decoded and materialized as raster
    /// tabs; users can select a different filmstrip image before pressing Open.
    fn commit_develop_window(&mut self) {
        if self.win.develop_window.is_none() || self.dev.develop_bake_all.is_some() {
            return;
        }
        let active_id = self.docs.documents[self.docs.active_doc_idx].id;
        let active_waiting = self
            .dev
            .develop_session
            .iter()
            .filter(|entry| develop_commit_targets_entry(entry.doc, active_id))
            .any(|entry| {
                let decode_in_flight = self.raw_decode_in_flight_for_doc(entry.doc);
                develop_session_entry_blocks_commit(
                    decode_in_flight,
                    self.jobs.raw_preview_failures.contains_key(&entry.doc),
                )
            });
        if active_waiting || self.docs.documents[self.docs.active_doc_idx].deferred_raw {
            self.ensure_raw_resident(self.docs.active_doc_idx);
            self.shell.status_msg =
                "Wait for the selected RAW to finish decoding, or select another image".to_string();
            if let Some(w) = &self.win.develop_window {
                w.request_redraw();
            }
            return;
        }
        // Never tear down a live GPU approximation directly into CPU-committed
        // tiles. Arm (or wait for) the exact settled bake first; the render loop
        // resumes this commit automatically as soon as those tiles land.
        if let Some(preview) = &mut self.dev.develop_preview {
            if develop_commit_needs_settled_frame(
                preview.gpu_preview_active,
                preview.processing,
                preview.rx.is_some(),
            ) {
                self.dev.develop_commit_after_refine = true;
                if preview.gpu_preview_active && !preview.processing {
                    preview.detail_refine_settings = Some(self.shell.ui.develop_settings.clone());
                    preview.detail_refine_at = Some(std::time::Instant::now());
                }
                self.shell.status_msg = "Finalizing full-quality RAW preview…".to_string();
                if let Some(w) = &self.win.develop_window {
                    w.request_redraw();
                }
                return;
            }
        }
        self.dev.develop_commit_after_refine = false;
        self.develop_session_save_active_settings();
        // Open commits the current selection only. Drop the preview receiver so
        // late thumbnails from the original file-dialog batch cannot create a
        // fresh Develop session after this window tears down.
        self.jobs.pending_raw_previews.clear();
        // The live preview stays UP through the whole bake: cancelling it here
        // restored the pristine tiles, so the window (and the main canvas
        // behind it) flashed back to the ORIGINAL colours for the entire
        // progress screen. Instead the bake sources the pristine tiles from
        // the preview state itself (develop_bake_all_start_next) and the
        // preview is dropped — without restoring — the moment its document's
        // baked tiles land (poll_develop_bake_all), so the edited look stays
        // on screen from the click to the final canvas.
        let entries = std::mem::take(&mut self.dev.develop_session);
        let active_entry = entries
            .iter()
            .find(|entry| develop_commit_targets_entry(entry.doc, active_id));
        let single_raw = active_entry.is_some_and(|entry| entry.transient);
        let discarded: Vec<_> = entries
            .iter()
            .filter(|entry| !develop_commit_targets_entry(entry.doc, active_id) && entry.transient)
            .map(|entry| entry.doc)
            .collect();
        self.discard_transient_develop_docs(&discarded);
        let pending: std::collections::VecDeque<_> = active_entry
            .map(|entry| (entry.doc, entry.settings.clone()))
            .into_iter()
            .collect();
        let total_images = usize::from(!pending.is_empty());
        if pending.is_empty() {
            self.finish_develop_bake_all(total_images, 0, single_raw);
            return;
        }
        let bake_total = pending.len();
        self.dev.develop_bake_all = Some(crate::app::state::DevelopBakeAll {
            started_at: std::time::Instant::now(),
            pending,
            bake_total,
            total_images,
            done: 0,
            developed: 0,
            single_raw,
            rx: None,
        });
        self.develop_bake_all_start_next();
    }

    /// End of the "Open Image" commit: report what happened and tear the
    /// window down. Also the no-bake path (an all-neutral session).
    pub(crate) fn finish_develop_bake_all(
        &mut self,
        total: usize,
        developed: usize,
        single_raw: bool,
    ) {
        self.shell.ui.show_develop_dialog = false;
        // Normally the preview was already dropped when its bake landed; it is
        // still alive here only for a neutral (never-baked) active image, where
        // the restore below is a pixel no-op.
        self.cancel_develop_preview();
        self.shell.status_msg = match (total, developed) {
            (1, 0) if single_raw => "Opened RAW".to_string(),
            (1, 0) => "No Develop changes applied".to_string(),
            (1, _) if single_raw => "Developed RAW".to_string(),
            (1, _) => "Applied Develop".to_string(),
            (n, d) => format!("Opened {n} images ({d} developed)"),
        };
        self.teardown_develop_window();
    }

    /// Drop the Develop window's OS window, egui context and GPU surface. The
    /// session (commit/cancel) is handled by the caller.
    fn teardown_develop_window(&mut self) {
        self.dev.develop_commit_after_refine = false;
        self.win.develop_egui_state = None;
        self.win.develop_egui_ctx = None;
        // Stop treating it as the active Develop host immediately, but keep the
        // last Arc alive while the main canvas recomposites/presents behind it.
        // Dropping the OS window here exposes the main surface's previous buffer
        // for one DWM frame, which is the visible flash on commit.
        self.win.retiring_develop_window = self.win.develop_window.take().map(|window| (window, 0));
        self.dev.develop_pan_drag = None;
        self.dev.develop_composited_view = None;
        // Filmstrip thumbnails are textures on the dropped egui context.
        self.dev.develop_thumbs.clear();
        // Mode B: the compositor was serving the Develop window's viewport and
        // view — hand it back to the main window (resize + rebake its view).
        if self
            .win
            .gpu
            .as_ref()
            .map_or(false, |g| !g.compositor.canvas_space)
        {
            self.recomposite();
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        // Belt-and-braces: that request can race the owned window's OS-side
        // destruction; a short repaint deadline guarantees the main loop wakes
        // and repaints even if it was swallowed (about_to_wait honours it).
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(50);
        self.win.egui_repaint_deadline = Some(
            self.win
                .egui_repaint_deadline
                .map_or(deadline, |d| d.min(deadline)),
        );
    }

    /// Advance the two-phase Develop-window teardown after a successful main
    /// surface presentation. Keep the old window for one additional presented
    /// frame so DWM has consumed the fresh main buffer before the cover window
    /// disappears.
    pub(crate) fn retire_develop_window_after_main_present(&mut self) {
        let Some((_, presented_frames)) = &mut self.win.retiring_develop_window else {
            return;
        };
        if *presented_frames == 0 {
            *presented_frames = 1;
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return;
        }

        // Keep the swapchain alive with the cover window, then release both only
        // after the final canvas has already been submitted twice to main.
        if let Some(gpu) = &mut self.win.gpu {
            gpu.close_develop_surface();
        }
        self.win.retiring_develop_window = None;
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Handle a `WindowEvent` addressed to the Develop window.
    pub fn develop_window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        // Feed the event to this window's egui context first; repaint when egui
        // asks (hover highlights, focus, animations).
        if let (Some(state), Some(window)) =
            (&mut self.win.develop_egui_state, &self.win.develop_window)
        {
            if state.on_window_event(window, &event).repaint {
                window.request_redraw();
            }
        }

        match event {
            WindowEvent::CloseRequested => {
                // The X button is the Develop "Cancel" path (discard). D2 adds a
                // confirm; for the spike it just closes.
                self.close_develop_window();
            }
            WindowEvent::Resized(sz) => {
                if sz.width > 0 && sz.height > 0 {
                    if let Some(gpu) = &mut self.win.gpu {
                        if let Some(ws) = &mut gpu.develop {
                            let device = &gpu.device;
                            ws.resize(device, sz.width, sz.height);
                        }
                    }
                    // Re-fit the canvas to the new viewport size.
                    self.dev.develop_view_fit = true;
                    if let Some(w) = &self.win.develop_window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::Moved(_) => {
                self.refresh_develop_system_display_profile();
            }
            WindowEvent::CursorMoved { position, .. } => {
                let now = (position.x as f32, position.y as f32);
                self.dev.develop_cursor = now;
                if let Some(last) = self.dev.develop_pan_drag {
                    self.dev.develop_view_off.0 += now.0 - last.0;
                    self.dev.develop_view_off.1 += now.1 - last.1;
                    self.dev.develop_pan_drag = Some(now);
                    self.dev.develop_view_fit = false;
                    if let Some(w) = &self.win.develop_window {
                        w.request_redraw();
                    }
                } else if self.dev.develop_local_drag.is_some() {
                    // An in-progress local-mask placement follows the pointer.
                    let (cx, cy) = self.develop_canvas_coords();
                    self.develop_local_pointer_drag_at(cx, cy);
                    if let Some(w) = &self.win.develop_window {
                        w.request_redraw();
                    }
                } else if self.develop_cursor_in_viewport() || self.dev.develop_readout.is_some() {
                    // Hovering the image refreshes the histogram R/G/B readout
                    // (and leaving the viewport clears it).
                    if let Some(w) = &self.win.develop_window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Middle,
                ..
            } => {
                // Middle-drag pans the viewport (matches the main window).
                self.dev.develop_pan_drag = match state {
                    ElementState::Pressed => Some(self.dev.develop_cursor),
                    ElementState::Released => None,
                };
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => {
                    if self.shell.ui.develop_local_arm.is_some()
                        && self.develop_cursor_in_viewport()
                    {
                        // An armed local mask captures the left drag over the
                        // viewport (over the panel the click belongs to egui);
                        // it takes precedence over the tool rail.
                        let (cx, cy) = self.develop_canvas_coords();
                        self.develop_local_pointer_down_at(cx, cy);
                        if let Some(w) = &self.win.develop_window {
                            w.request_redraw();
                        }
                    } else if self.develop_cursor_in_viewport() {
                        // Otherwise the active rail tool handles the click.
                        match self.dev.develop_tool {
                            DevelopTool::Hand => {
                                // Left-drag pans, like the middle button; the
                                // CursorMoved handler follows `develop_pan_drag`.
                                self.dev.develop_pan_drag = Some(self.dev.develop_cursor);
                            }
                            DevelopTool::Zoom => {
                                let alt = self
                                    .win
                                    .develop_egui_ctx
                                    .as_ref()
                                    .is_some_and(|c| c.input(|i| i.modifiers.alt));
                                let factor = if alt { 1.0 / 1.5 } else { 1.5 };
                                self.develop_zoom_at_cursor(factor);
                            }
                            DevelopTool::WhiteBalance => {
                                self.develop_wb_sample_at_cursor();
                            }
                        }
                        if let Some(w) = &self.win.develop_window {
                            w.request_redraw();
                        }
                    }
                }
                ElementState::Released => {
                    if self.dev.develop_local_drag.is_some() {
                        self.develop_local_pointer_up();
                    }
                    // End a Hand-tool left-drag pan (a no-op if none is active).
                    self.dev.develop_pan_drag = None;
                    if let Some(w) = &self.win.develop_window {
                        w.request_redraw();
                    }
                }
            },
            WindowEvent::MouseWheel { delta, .. } => {
                // Scroll zooms about the cursor, but only over the canvas
                // viewport — over the panel the scroll belongs to egui.
                if self.develop_cursor_in_viewport() {
                    let scroll = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y,
                        MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40.0,
                    };
                    if scroll != 0.0 {
                        let factor = if scroll > 0.0 { 1.15 } else { 1.0 / 1.15 };
                        self.develop_zoom_at_cursor(factor);
                        if let Some(w) = &self.win.develop_window {
                            w.request_redraw();
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                self.redraw_develop_window(event_loop);
            }
            // Tool-rail shortcuts (ACR-style): H Hand, Z Zoom, I white-balance
            // eyedropper. Ignored while a text field (preset name, numeric slider
            // entry) holds keyboard focus.
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key:
                            PhysicalKey::Code(code @ (KeyCode::KeyH | KeyCode::KeyZ | KeyCode::KeyI)),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                let typing = self
                    .win
                    .develop_egui_ctx
                    .as_ref()
                    .is_some_and(|ctx| ctx.egui_wants_keyboard_input());
                if !typing {
                    self.dev.develop_tool = match code {
                        KeyCode::KeyH => DevelopTool::Hand,
                        KeyCode::KeyZ => DevelopTool::Zoom,
                        _ => DevelopTool::WhiteBalance,
                    };
                    self.shell.ui.develop_local_arm = None;
                    if let Some(w) = &self.win.develop_window {
                        w.request_redraw();
                    }
                }
            }
            // Escape from the Develop window is the Cancel path (like the X
            // button) — so it can be closed while it holds keyboard focus, where
            // the main window's Ctrl+Alt+D toggle never arrives. A focused text
            // field (preset name, numeric slider entry) consumes Escape itself.
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(KeyCode::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                let typing = self
                    .win
                    .develop_egui_ctx
                    .as_ref()
                    .is_some_and(|ctx| ctx.egui_wants_keyboard_input());
                if !typing {
                    self.close_develop_window();
                }
            }
            _ => {
                // Any other event may change UI state; schedule a repaint.
                if let Some(w) = &self.win.develop_window {
                    w.request_redraw();
                }
            }
        }
    }

    /// Build this window's egui UI and render one frame to its surface.
    #[allow(deprecated)] // CentralPanel::show — same pattern as ui/welcome.rs
    fn redraw_develop_window(&mut self, event_loop: &ActiveEventLoop) {
        // Once this owned window is foreground, Windows may coalesce paints for
        // the covered main window. Pump file workers here as well so a finished
        // RAW can always replace its embedded preview and unlock the controls.
        self.poll_raw_previews(event_loop);
        self.poll_loads();
        self.enter_pending_develop(event_loop);
        // Pump the "Open Image" bake queue; this may finish the commit and
        // tear the window down (the extraction below then bails out).
        self.poll_develop_bake_all();
        if let Some(started_at) = self.dev.develop_bake_all.as_ref().map(|b| b.started_at) {
            let now = std::time::Instant::now();
            if now.duration_since(started_at) < COMMIT_PROGRESS_DELAY {
                // Keep the last fully-rendered Develop preview on screen while
                // polling a fast commit; no intermediate frame is presented
                // here. Self-request the next tick — the covered main window's
                // paints can be coalesced, so its repaint deadline alone must
                // not be the only thing keeping this pump alive.
                let deadline = (now + std::time::Duration::from_millis(16))
                    .min(started_at + COMMIT_PROGRESS_DELAY);
                self.win.egui_repaint_deadline = Some(
                    self.win
                        .egui_repaint_deadline
                        .map_or(deadline, |d| d.min(deadline)),
                );
                if let Some(w) = &self.win.develop_window {
                    w.request_redraw();
                }
                return;
            }
        }
        // The Develop window owns the controls that start/update this preview,
        // so it must also pump preview workers itself. Relying only on the main
        // (owned/covered) window's redraw loop can strand a completed preview
        // until Windows generates another paint event (for example after a
        // minimize/restore cycle).
        self.poll_develop_preview();
        if self.dev.develop_commit_after_refine
            && self.dev.develop_preview.as_ref().is_some_and(|p| {
                !p.gpu_preview_active
                    && !p.processing
                    && p.rx.is_none()
                    && p.detail_refine_at.is_none()
            })
        {
            self.commit_develop_window();
        }
        let (Some(ctx), Some(window)) = (
            self.win.develop_egui_ctx.clone(),
            self.win.develop_window.clone(),
        ) else {
            return;
        };

        let raw_input = match &mut self.win.develop_egui_state {
            Some(state) => state.take_egui_input(&window),
            None => return,
        };

        // Right-hand panel hosting the real Develop controls; the central area is
        // left unpainted so the canvas shows through (Camera-Raw layout).
        // Resolve a pending fit up front so the zoom label and the local-mask
        // overlay see this frame's view.
        self.develop_resolve_fit();
        self.develop_ensure_thumbs(&ctx);
        let zoom_pct = self.dev.develop_view_zoom * 100.0;
        let win_logical_h = window.inner_size().height as f32 / window.scale_factor() as f32;
        let max_scroll_h = (win_logical_h - 150.0).max(240.0);
        let local_overlay = self.develop_local_overlay_for_view(
            self.dev.develop_view_off.0,
            self.dev.develop_view_off.1,
            self.dev.develop_view_zoom,
            window.scale_factor() as f32,
        );
        // Filmstrip rows (doc, tab title, thumbnail, is-active) for a
        // multi-image session — prepared here so the UI closure borrows no App
        // state.
        let active_id = self.docs.documents[self.docs.active_doc_idx].id;
        let active_raw_loading = self.raw_decode_in_flight_for_doc(active_id);
        let active_raw_deferred = self.docs.documents[self.docs.active_doc_idx].deferred_raw;
        let active_raw_failure = self.jobs.raw_preview_failures.get(&active_id).cloned();
        // Histogram R/G/B readout for the pixel under the cursor (D4).
        self.dev.develop_readout = self.compute_develop_readout();
        let filmstrip: Vec<(
            crate::core::document::DocumentId,
            String,
            Option<(egui::TextureId, egui::Vec2)>,
            bool,
            Option<String>,
        )> = if self.dev.develop_session.len() > 1 {
            self.dev
                .develop_session
                .iter()
                .map(|e| {
                    let title = self
                        .docs
                        .documents
                        .iter()
                        .find(|d| d.id == e.doc)
                        .map(|d| d.title.clone())
                        .unwrap_or_default();
                    let tex = self.dev.develop_thumbs.get(&e.doc).map(|t| {
                        let s = t.size();
                        let scale = FILMSTRIP_THUMB_H / (s[1] as f32).max(1.0);
                        (t.id(), egui::vec2(s[0] as f32 * scale, FILMSTRIP_THUMB_H))
                    });
                    let state = if let Some(error) = self.jobs.raw_preview_failures.get(&e.doc) {
                        Some(format!("RAW decode failed: {error}"))
                    } else if self.raw_decode_in_flight_for_doc(e.doc) {
                        Some("Decoding RAW...".to_string())
                    } else if self
                        .docs
                        .documents
                        .iter()
                        .find(|doc| doc.id == e.doc)
                        .is_some_and(|doc| doc.deferred_raw)
                    {
                        Some("Preview — click to load".to_string())
                    } else {
                        None
                    };
                    (e.doc, title, tex, e.doc == active_id, state)
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut filmstrip_clicked: Option<crate::core::document::DocumentId> = None;
        // Mid-"Open Image": the panel is replaced by a progress readout.
        let bake_progress = self
            .dev
            .develop_bake_all
            .as_ref()
            .map(|b| (b.done, b.bake_total));
        let ui_data = self.collect_ui_data();
        let mut actions = crate::ui::UiActions::default();
        let mut fit_clicked = false;
        let mut reset_100 = false;
        let mut apply_dev = false;
        let mut cancel_dev = false;
        let active_tool = self.dev.develop_tool;
        let panning = self.dev.develop_pan_drag.is_some();
        let mut tool_clicked: Option<DevelopTool> = None;
        let full_output = ctx.run_ui(raw_input, |ctx| {
            // Left tool rail (D5): Hand / Zoom / White-balance eyedropper. Hidden
            // while the "Open Image" bake owns the window (modal progress).
            if bake_progress.is_none() {
                egui::SidePanel::left("develop_tool_rail")
                    .exact_width(DEVELOP_RAIL_W)
                    .resizable(false)
                    .show(ctx, |ui| {
                        ui.add_space(8.0);
                        ui.vertical_centered(|ui| {
                            for (tool, icon, tip) in [
                                (DevelopTool::Hand, ph::HAND, "Hand — drag to pan (H)"),
                                (
                                    DevelopTool::Zoom,
                                    ph::MAGNIFYING_GLASS,
                                    "Zoom — click to zoom in, Alt-click to zoom out (Z)",
                                ),
                                (
                                    DevelopTool::WhiteBalance,
                                    ph::EYEDROPPER,
                                    "White Balance — click a neutral grey to set Temp/Tint (I)",
                                ),
                            ] {
                                let resp = ui
                                    .add_sized(
                                        [34.0, 30.0],
                                        egui::SelectableLabel::new(
                                            active_tool == tool,
                                            egui::RichText::new(icon).size(18.0),
                                        ),
                                    )
                                    .on_hover_text(tip);
                                if resp.clicked() {
                                    tool_clicked = Some(tool);
                                }
                                ui.add_space(2.0);
                            }
                        });
                    });
            }
            egui::SidePanel::right("develop_panel")
                .exact_width(DEVELOP_PANEL_W)
                .resizable(false)
                .show(ctx, |ui| {
                    if let Some((done, total)) = bake_progress {
                        ui.add_space(24.0);
                        ui.vertical_centered(|ui| {
                            ui.add(egui::Spinner::new().size(28.0));
                            ui.add_space(8.0);
                            ui.label(format!(
                                "Developing image {} of {}…",
                                (done + 1).min(total),
                                total
                            ));
                        });
                        return;
                    }
                    if let Some(error) = &active_raw_failure {
                        ui.add_space(32.0);
                        ui.vertical_centered(|ui| {
                            ui.heading("RAW decode failed");
                            ui.add_space(8.0);
                            ui.label(error);
                            ui.add_space(18.0);
                            cancel_dev = ui.button("Cancel").clicked();
                        });
                        return;
                    }
                    if active_raw_deferred {
                        ui.add_space(32.0);
                        ui.vertical_centered(|ui| {
                            ui.add(egui::Spinner::new().size(30.0));
                            ui.add_space(10.0);
                            ui.heading(if active_raw_loading {
                                "Decoding RAW..."
                            } else {
                                "RAW queued..."
                            });
                            ui.add_space(6.0);
                            ui.label("Showing the camera's embedded preview.");
                            ui.label("Develop controls will unlock when the full scene is ready.");
                            ui.add_space(18.0);
                            cancel_dev = ui.button("Cancel").clicked();
                        });
                        return;
                    }
                    ui.add_space(6.0);
                    // Profile picker (stub): one built-in look for now — the
                    // disabled dropdown marks where camera/creative profiles land.
                    ui.add_enabled_ui(false, |ui| {
                        egui::ComboBox::from_label("Profile")
                            .selected_text("iAi Standard")
                            .show_ui(ui, |_ui| {});
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        fit_clicked = ui.button("Fit").clicked();
                        reset_100 = ui.button("100%").clicked();
                        ui.label(format!("{zoom_pct:.0}%"));
                        ui.label("· scroll=zoom, mid-drag=pan");
                    });
                    ui.separator();
                    let (a, c) = crate::ui::develop::develop_panel_contents(
                        ui,
                        &ui_data,
                        &mut actions,
                        max_scroll_h,
                    );
                    apply_dev = a;
                    cancel_dev = c;
                });
            // Bottom filmstrip for a multi-image session: click a thumbnail to
            // make it the active image (settings are saved per image).
            if !filmstrip.is_empty() {
                egui::TopBottomPanel::bottom("develop_filmstrip")
                    .exact_height(FILMSTRIP_H)
                    .show(ctx, |ui| {
                        egui::ScrollArea::horizontal().show(ui, |ui| {
                            ui.horizontal_centered(|ui| {
                                for (doc, title, tex, active, state) in &filmstrip {
                                    let resp = match tex {
                                        Some((id, size)) => ui.add(
                                            egui::ImageButton::new(egui::load::SizedTexture::new(
                                                *id, *size,
                                            ))
                                            .selected(*active),
                                        ),
                                        None => ui.add(
                                            egui::Button::new(title.as_str()).selected(*active),
                                        ),
                                    }
                                    .on_hover_text(match state {
                                        Some(state) => format!("{title}\n{state}"),
                                        None => title.clone(),
                                    });
                                    if resp.clicked() && !*active {
                                        filmstrip_clicked = Some(*doc);
                                    }
                                }
                            });
                        });
                    });
            }
            // Selected local-mask outline, clipped to the canvas viewport (the
            // rail lines are viewport-crossing and must not run over the panel
            // or the filmstrip).
            if let Some(ov) = &local_overlay {
                let strip_h = if filmstrip.is_empty() {
                    0.0
                } else {
                    FILMSTRIP_H
                };
                let vp = egui::Rect::from_min_max(
                    egui::pos2(DEVELOP_RAIL_W, 0.0),
                    egui::pos2(
                        (ctx.screen_rect().width() - DEVELOP_PANEL_W).max(DEVELOP_RAIL_W),
                        (ctx.screen_rect().height() - strip_h).max(0.0),
                    ),
                );
                let painter = ctx
                    .layer_painter(egui::LayerId::new(
                        egui::Order::Foreground,
                        egui::Id::new("develop_local_overlay"),
                    ))
                    .with_clip_rect(vp);
                crate::ui::draw_develop_local_overlay(&painter, ov);
            }
            // Reflect the active tool in the cursor while the pointer is over the
            // canvas viewport (between the rail and panel, above the filmstrip).
            // Setting it through egui — not winit directly — keeps egui's own
            // cursor tracking correct as the pointer crosses back onto the chrome.
            if bake_progress.is_none() {
                let strip_h = if filmstrip.is_empty() {
                    0.0
                } else {
                    FILMSTRIP_H
                };
                let vp = egui::Rect::from_min_max(
                    egui::pos2(DEVELOP_RAIL_W, 0.0),
                    egui::pos2(
                        (ctx.screen_rect().width() - DEVELOP_PANEL_W).max(DEVELOP_RAIL_W),
                        (ctx.screen_rect().height() - strip_h).max(0.0),
                    ),
                );
                if ctx.pointer_latest_pos().is_some_and(|p| vp.contains(p)) {
                    let icon = match tool_clicked.unwrap_or(active_tool) {
                        DevelopTool::Hand if panning => egui::CursorIcon::Grabbing,
                        DevelopTool::Hand => egui::CursorIcon::Grab,
                        DevelopTool::Zoom => egui::CursorIcon::ZoomIn,
                        DevelopTool::WhiteBalance => egui::CursorIcon::Crosshair,
                    };
                    ctx.set_cursor_icon(icon);
                }
            }
        });

        if let Some(state) = &mut self.win.develop_egui_state {
            state.handle_platform_output(&window, full_output.platform_output);
        }

        // Apply a tool-rail selection (the cursor for it was already emitted
        // through egui inside the frame above).
        if let Some(tool) = tool_clicked {
            self.dev.develop_tool = tool;
            // Choosing a rail tool cancels a pending local-mask placement.
            self.shell.ui.develop_local_arm = None;
        }

        let pixels_per_point = window.scale_factor() as f32;

        // Apply the panel's outputs to the shared Develop session.
        if fit_clicked {
            self.dev.develop_view_fit = true;
        } else if reset_100 {
            self.develop_set_zoom_100(&window, pixels_per_point);
        }
        self.set_develop_controls_pointer_down(actions.develop.develop_controls_pointer_down);
        if let Some(s) = actions.develop.set_develop_settings.take() {
            self.shell.ui.develop_settings = s.clone();
            self.update_develop_preview(s);
            // Keep the covered main window's cached UI in sync as well. The
            // actual preview recomposite is flushed by this window below.
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            window.request_redraw();
        }
        // Presets and local-mask arm/select — shared with the fallback dialog.
        self.apply_develop_panel_actions(&mut actions);
        // The Develop window owns its own UiActions frame, so proof controls
        // must be consumed here instead of waiting for the covered main UI.
        self.handle_color_print_actions(&mut actions);
        // Filmstrip switch (after the settings above — they belong to the image
        // the panel was showing this frame).
        if let Some(doc) = filmstrip_clicked {
            self.develop_session_activate(doc);
        }
        if apply_dev {
            self.commit_develop_window();
            return;
        }
        if cancel_dev {
            self.close_develop_window();
            return;
        }

        // Slider changes above mark the shared compositor preview dirty. Flush
        // it here, in the same OS-window frame that will present it, instead of
        // waiting for a RedrawRequested on the covered main window. This makes
        // Color/WB/Tone drags update immediately and removes the old
        // "minimize then restore to refresh" failure mode.
        self.flush_develop_gpu_preview();

        // Fit the canvas into the viewport (window minus the panel) at this
        // window's own view. Mode A re-samples the canvas-sized texture at any
        // view; Mode B bakes the view into the composite, so a zoom/pan/resize
        // here must recomposite before the blit.
        let mode_b = self
            .win
            .gpu
            .as_ref()
            .map_or(false, |g| !g.compositor.canvas_space);
        if mode_b {
            self.develop_resolve_fit();
            if self.develop_view_key() != self.dev.develop_composited_view {
                // Throttle to the measured composite cost: pan/zoom events can
                // arrive faster than a 45MP viewport composite of the Develop
                // chain. A skipped frame keeps blitting the last baked view and
                // retries next redraw, so the final view always lands.
                let now = std::time::Instant::now();
                let ready = self.dev.develop_gpu_recompose_last.map_or(true, |last| {
                    let interval = self.dev.develop_gpu_recompose_cost.mul_f32(0.75).clamp(
                        std::time::Duration::from_millis(8),
                        std::time::Duration::from_millis(48),
                    );
                    now.duration_since(last) >= interval
                });
                if ready {
                    let start = std::time::Instant::now();
                    self.recomposite();
                    self.dev.develop_gpu_recompose_cost = start.elapsed();
                    self.dev.develop_gpu_recompose_last = Some(std::time::Instant::now());
                } else {
                    window.request_redraw();
                }
            }
        }
        let primitives = ctx.tessellate(full_output.shapes, full_output.pixels_per_point);
        let backdrop = self.shell.ui.theme_mode.palette().canvas_backdrop();
        let canvas_uniforms = self.develop_canvas_uniforms(&window);

        let mut presented = true;
        if let Some(gpu) = &mut self.win.gpu {
            presented = gpu.render_develop(
                &primitives,
                &full_output.textures_delta,
                pixels_per_point,
                backdrop,
                canvas_uniforms,
            );
        }

        // Keep retrying after a dropped frame (surface Outdated/Lost, already
        // reconfigured), and while dirty/throttled GPU previews, CPU workers,
        // the live histogram, or the "Open Image" queue still need pumping.
        // None of those raises a Develop-window event when its result is ready.
        let preview_in_flight = self
            .dev
            .develop_preview
            .as_ref()
            .is_some_and(|p| p.processing || p.detail_refine_at.is_some());
        if !presented
            || self.dev.develop_bake_all.is_some()
            || !self.jobs.pending_loads.is_empty()
            || !self.jobs.pending_raw_previews.is_empty()
            || self.dev.develop_gpu_preview_dirty
            || self.dev.develop_histogram_stale
            || preview_in_flight
        {
            window.request_redraw();
        }
    }

    /// Resolve a pending fit view: while `develop_view_fit` is set, re-fit the
    /// canvas into the window's viewport area (left of the panel) and store the
    /// resulting zoom/offset, so the first user zoom/pan continues from there.
    /// Called before both the blit (Mode A) and the composite bake (Mode B) so
    /// the two always agree on the view.
    pub(crate) fn develop_resolve_fit(&mut self) {
        if !self.dev.develop_view_fit {
            return;
        }
        let Some(window) = self.win.develop_window.clone() else {
            return;
        };
        let pixels_per_point = window.scale_factor() as f32;
        let sz = window.inner_size();
        let panel_px = DEVELOP_PANEL_W * pixels_per_point;
        let rail_px = DEVELOP_RAIL_W * pixels_per_point;
        let vw = (sz.width as f32 - panel_px - rail_px).max(1.0);
        let vh = (sz.height as f32 - self.develop_filmstrip_px(pixels_per_point)).max(1.0);
        let cw = self.docs.documents[self.docs.active_doc_idx].canvas.width as f32;
        let ch = self.docs.documents[self.docs.active_doc_idx].canvas.height as f32;
        if cw < 1.0 || ch < 1.0 {
            return;
        }
        let pad = 24.0 * pixels_per_point;
        let zoom = ((vw - pad * 2.0) / cw)
            .min((vh - pad * 2.0) / ch)
            .clamp(0.01, 64.0);
        // Centre within the viewport area (between the rail and the panel), in
        // physical px — the offset is measured from the window origin, so the
        // rail's width shifts the canvas right.
        self.dev.develop_view_zoom = zoom;
        self.dev.develop_view_off = (rail_px + (vw - cw * zoom) / 2.0, (vh - ch * zoom) / 2.0);
    }

    /// The current Develop view as a compare-key against
    /// `develop_composited_view` (offset/zoom bit patterns + window size — a
    /// resize must recomposite even if the view happens to be unchanged, since
    /// Mode B's texture is viewport-sized).
    fn develop_view_key(&self) -> Option<[u32; 5]> {
        let window = self.win.develop_window.as_ref()?;
        let sz = window.inner_size();
        Some([
            self.dev.develop_view_off.0.to_bits(),
            self.dev.develop_view_off.1.to_bits(),
            self.dev.develop_view_zoom.to_bits(),
            sz.width,
            sz.height,
        ])
    }

    /// Compute the Develop window's canvas-blit uniforms from its own view.
    /// Mode A samples the canvas-sized compositor texture positioned by this
    /// window's offset/zoom; Mode B fullscreen-blits the viewport-sized texture
    /// the compositor baked for this window (see `recomposite_with_dirty`).
    /// Returns `None` without a GPU or with a degenerate canvas.
    fn develop_canvas_uniforms(&mut self, window: &Window) -> Option<crate::gpu::CanvasUniforms> {
        let canvas_space = self.win.gpu.as_ref()?.compositor.canvas_space;
        let sz = window.inner_size();
        let (dw, dh) = (sz.width as f32, sz.height as f32);
        let cw = self.docs.documents[self.docs.active_doc_idx].canvas.width as f32;
        let ch = self.docs.documents[self.docs.active_doc_idx].canvas.height as f32;
        if cw < 1.0 || ch < 1.0 {
            return None;
        }
        self.develop_resolve_fit();

        Some(crate::gpu::CanvasUniforms {
            offset: [self.dev.develop_view_off.0, self.dev.develop_view_off.1],
            zoom: self.dev.develop_view_zoom,
            vp_mode: if canvas_space { 0.0 } else { 1.0 },
            screen_size: [dw, dh],
            proof_enabled: develop_display_lut_active(
                self.shell.proof_enabled,
                self.shell.display_cms_enabled,
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .color_space
                    != crate::core::canvas::ColorSpace::SRGB,
            ),
            channel_view: 0.0,
        })
    }

    /// False while the Develop window owns the Mode B compositor texture — it is
    /// baked for THAT window's view, so the main window must not blit it (Mode A
    /// stays view-independent and keeps showing under the soft-modal dim).
    pub(crate) fn main_window_draws_canvas(&self) -> bool {
        self.win.develop_window.is_none()
            || self
                .win
                .gpu
                .as_ref()
                .map_or(true, |g| g.compositor.canvas_space)
    }

    /// Title for the Develop window: the document name, plus the camera
    /// make/model for a RAW session (the importer stores it in
    /// `metadata.source_profile`; for other formats that field holds an ICC
    /// profile name, so it is only shown when a scene master exists).
    pub(crate) fn develop_window_title(&self) -> String {
        let doc = &self.docs.documents[self.docs.active_doc_idx];
        let mut title = format!("Develop — {}", doc.title);
        // Filmstrip position "(i of N)" for a multi-image session.
        let n = self.dev.develop_session.len();
        if n > 1 {
            if let Some(i) = self
                .dev
                .develop_session
                .iter()
                .position(|e| e.doc == doc.id)
            {
                title.push_str(&format!(" ({} of {})", i + 1, n));
            }
        }
        if doc.canvas.develop_source.is_some() {
            let cam = doc.canvas.metadata.source_profile.trim();
            if !cam.is_empty() {
                title.push_str(&format!(" — {cam}"));
            }
        }
        if self.jobs.raw_preview_failures.contains_key(&doc.id) {
            title.push_str(" - RAW decode failed");
        } else if self.raw_decode_in_flight_for_doc(doc.id) {
            title.push_str(" - Decoding RAW...");
        } else if doc.deferred_raw {
            title.push_str(" - Preview");
        }
        title
    }

    /// Physical height claimed by the filmstrip (0 when it is hidden — single
    /// image sessions).
    fn develop_filmstrip_px(&self, pixels_per_point: f32) -> f32 {
        if self.dev.develop_session.len() > 1 {
            FILMSTRIP_H * pixels_per_point
        } else {
            0.0
        }
    }

    /// Build any missing filmstrip thumbnails (nearest-neighbour sample of the
    /// develop layer's tiles — O(thumb pixels), independent of image size).
    /// Textures live on the Develop window's egui context; cleared on teardown.
    fn develop_ensure_thumbs(&mut self, ctx: &egui::Context) {
        if self.dev.develop_session.len() < 2 {
            return;
        }
        let ids: Vec<crate::core::document::DocumentId> =
            self.dev.develop_session.iter().map(|e| e.doc).collect();
        for id in ids {
            if self.dev.develop_thumbs.contains_key(&id) {
                continue;
            }
            let Some(doc) = self.docs.documents.iter().find(|d| d.id == id) else {
                continue;
            };
            let Some(layer) = doc.canvas.layer_stack.layers.iter().find(|l| l.is_raster()) else {
                continue;
            };
            let (w, h) = (layer.width, layer.height);
            if w == 0 || h == 0 {
                continue;
            }
            let th = FILMSTRIP_THUMB_H.round() as u32 * 2; // 2× for crisp hidpi
            let tw = ((w as f32 / h as f32) * th as f32)
                .round()
                .clamp(16.0, th as f32 * 4.0) as u32;
            let mut px = Vec::with_capacity((tw * th * 4) as usize);
            for ty in 0..th {
                let sy = ((ty as f32 + 0.5) / th as f32 * h as f32) as u32;
                for tx in 0..tw {
                    let sx = ((tx as f32 + 0.5) / tw as f32 * w as f32) as u32;
                    let (r, g, b, a) = layer.tiles.get_pixel(sx, sy);
                    px.extend_from_slice(&[r, g, b, a]);
                }
            }
            let img = egui::ColorImage::from_rgba_unmultiplied([tw as usize, th as usize], &px);
            let tex = ctx.load_texture(
                format!("develop_thumb_{}", id.0),
                img,
                egui::TextureOptions::LINEAR,
            );
            self.dev.develop_thumbs.insert(id, tex);
        }
    }

    /// The last cursor position mapped into canvas pixels through this window's
    /// view (inverse of `canvas px × zoom + offset`, all in physical px).
    fn develop_canvas_coords(&self) -> (f32, f32) {
        let (cx, cy) = self.dev.develop_cursor;
        let z = self.dev.develop_view_zoom.max(1e-6);
        (
            (cx - self.dev.develop_view_off.0) / z,
            (cy - self.dev.develop_view_off.1) / z,
        )
    }

    /// Display R/G/B (0–255) of the scene pixel under the Develop-window
    /// cursor, through the current settings — the histogram readout (D4).
    /// Runs the same per-sample chain the histogram bins, so the two agree;
    /// spatial stages (Detail/Effects/locals) are not evaluated. `None`
    /// off-viewport, without a scene session, or out of the image.
    fn compute_develop_readout(&self) -> Option<[u8; 3]> {
        if !self.develop_cursor_in_viewport() {
            return None;
        }
        let preview = self.dev.develop_preview.as_ref()?;
        let scene = preview.scene.as_ref()?;
        let doc = &self.docs.documents[self.docs.active_doc_idx];
        if preview.doc_id != doc.id {
            return None;
        }
        // Canvas → layer-local coordinates (the scene master covers the
        // session layer's pixels, which may sit offset in the canvas).
        let layer = doc
            .canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == preview.layer_id)?;
        let (cx, cy) = self.develop_canvas_coords();
        let lx = (cx - layer.offset.0 as f32).floor();
        let ly = (cy - layer.offset.1 as f32).floor();
        if lx < 0.0 || ly < 0.0 || lx >= scene.width as f32 || ly >= scene.height as f32 {
            return None;
        }
        let rgb = scene.get_rgb(lx as u32, ly as u32);
        let d = crate::core::develop_scene::eval_scene_pixel(
            rgb,
            &self.shell.ui.develop_settings,
            scene.look,
        );
        Some([
            (d[0] * 255.0 + 0.5) as u8,
            (d[1] * 255.0 + 0.5) as u8,
            (d[2] * 255.0 + 0.5) as u8,
        ])
    }

    /// White-balance eyedropper (D5): average a small patch of the scene master
    /// around the cursor, solve the Temperature/Tint pair that neutralises it
    /// (inverse CAT16), and apply it to the live Develop settings. The sample is
    /// the pre-white-balance scene value, so the solved sliders are absolute.
    /// Needs a scene session (RAW, or a linearised JPEG/PNG Develop).
    fn develop_wb_sample_at_cursor(&mut self) {
        if !self.develop_cursor_in_viewport() {
            return;
        }
        let (cx, cy) = self.develop_canvas_coords();
        let active_id = self.docs.documents[self.docs.active_doc_idx].id;
        // Average a small box of scene pixels to beat sensor noise; the borrow of
        // the preview/scene ends with this block so the settings can be applied.
        let avg = {
            let Some(preview) = self.dev.develop_preview.as_ref() else {
                return;
            };
            if preview.doc_id != active_id {
                return;
            }
            let Some(scene) = preview.scene.as_ref() else {
                self.shell.status_msg =
                    "White balance needs a RAW or Develop scene image".to_string();
                return;
            };
            let doc = &self.docs.documents[self.docs.active_doc_idx];
            let Some(layer) = doc
                .canvas
                .layer_stack
                .layers
                .iter()
                .find(|l| l.id == preview.layer_id)
            else {
                return;
            };
            let lx = (cx - layer.offset.0 as f32).round() as i32;
            let ly = (cy - layer.offset.1 as f32).round() as i32;
            const R: i32 = 2; // 5×5 patch
            let mut sum = [0.0f64; 3];
            let mut n = 0u32;
            for oy in -R..=R {
                for ox in -R..=R {
                    let x = lx + ox;
                    let y = ly + oy;
                    if x < 0 || y < 0 || x >= scene.width as i32 || y >= scene.height as i32 {
                        continue;
                    }
                    let rgb = scene.get_rgb(x as u32, y as u32);
                    sum[0] += rgb[0] as f64;
                    sum[1] += rgb[1] as f64;
                    sum[2] += rgb[2] as f64;
                    n += 1;
                }
            }
            if n == 0 {
                return; // Clicked outside the image.
            }
            [
                (sum[0] / n as f64) as f32,
                (sum[1] / n as f64) as f32,
                (sum[2] / n as f64) as f32,
            ]
        };

        let (temp, tint) = crate::core::cat16::neutralize(avg);
        let mut settings = self.shell.ui.develop_settings.clone();
        settings.temperature = temp;
        settings.tint = tint;
        self.shell.ui.develop_settings = settings.clone();
        self.update_develop_preview(settings);
        self.develop_session_save_active_settings();
        self.shell.status_msg = format!("White balance set (Temp {temp:.0}, Tint {tint:.0})");
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        if let Some(w) = &self.win.develop_window {
            w.request_redraw();
        }
    }

    /// True when the last cursor position sits over the canvas viewport (left
    /// of the controls panel, above the filmstrip) rather than the chrome.
    fn develop_cursor_in_viewport(&self) -> bool {
        let Some(w) = &self.win.develop_window else {
            return false;
        };
        let ppp = w.scale_factor() as f32;
        let panel_px = DEVELOP_PANEL_W * ppp;
        let rail_px = DEVELOP_RAIL_W * ppp;
        self.dev.develop_cursor.0 > rail_px
            && self.dev.develop_cursor.0 < (w.inner_size().width as f32 - panel_px)
            && self.dev.develop_cursor.1
                < (w.inner_size().height as f32 - self.develop_filmstrip_px(ppp))
    }

    /// Zoom the Develop viewport about the cursor by `factor`, keeping the canvas
    /// point under the pointer fixed (mirrors the main window's zoom math).
    fn develop_zoom_at_cursor(&mut self, factor: f32) {
        let (cx, cy) = self.dev.develop_cursor;
        let old = self.dev.develop_view_zoom;
        let new = (old * factor).clamp(0.02, 64.0);
        if (new - old).abs() < f32::EPSILON {
            return;
        }
        let k = new / old;
        self.dev.develop_view_off.0 = cx - (cx - self.dev.develop_view_off.0) * k;
        self.dev.develop_view_off.1 = cy - (cy - self.dev.develop_view_off.1) * k;
        self.dev.develop_view_zoom = new;
        self.dev.develop_view_fit = false;
    }

    /// Reset the Develop view to 100% (1 canvas pixel = 1 device pixel), centred
    /// in the viewport area left of the panel.
    fn develop_set_zoom_100(&mut self, window: &Window, pixels_per_point: f32) {
        let sz = window.inner_size();
        let rail_px = DEVELOP_RAIL_W * pixels_per_point;
        let vw = (sz.width as f32 - DEVELOP_PANEL_W * pixels_per_point - rail_px).max(1.0);
        let vh = sz.height as f32 - self.develop_filmstrip_px(pixels_per_point);
        let cw = self.docs.documents[self.docs.active_doc_idx].canvas.width as f32;
        let ch = self.docs.documents[self.docs.active_doc_idx].canvas.height as f32;
        self.dev.develop_view_zoom = 1.0;
        self.dev.develop_view_off = (rail_px + (vw - cw) / 2.0, (vh - ch) / 2.0);
        self.dev.develop_view_fit = false;
    }
}

#[cfg(test)]
mod phase6_transition_tests {
    use super::{
        develop_commit_needs_settled_frame, develop_commit_targets_entry,
        develop_display_lut_active, develop_session_entry_blocks_commit,
    };
    use crate::core::document::DocumentId;

    #[test]
    fn open_image_waits_for_every_unsettled_preview_state() {
        assert!(develop_commit_needs_settled_frame(true, false, false));
        assert!(develop_commit_needs_settled_frame(false, true, false));
        assert!(develop_commit_needs_settled_frame(false, false, true));
        assert!(!develop_commit_needs_settled_frame(false, false, false));
    }

    #[test]
    fn evicted_raw_mapping_does_not_block_open_image_without_a_decode() {
        assert!(!develop_session_entry_blocks_commit(false, false));
        assert!(develop_session_entry_blocks_commit(true, false));
        assert!(develop_session_entry_blocks_commit(false, true));
    }

    #[test]
    fn open_image_targets_only_the_active_filmstrip_entry() {
        let active = DocumentId(2);
        assert!(!develop_commit_targets_entry(DocumentId(1), active));
        assert!(develop_commit_targets_entry(DocumentId(2), active));
        assert!(!develop_commit_targets_entry(DocumentId(3), active));
    }

    #[test]
    fn develop_window_enables_the_shared_display_lut_for_every_view_transform() {
        assert_eq!(develop_display_lut_active(true, false, false), 1.0);
        assert_eq!(develop_display_lut_active(false, true, false), 1.0);
        assert_eq!(develop_display_lut_active(false, false, true), 1.0);
        assert_eq!(develop_display_lut_active(false, false, false), 0.0);
    }
}

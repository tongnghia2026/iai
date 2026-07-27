mod keyboard;
mod pointer;
mod redraw;

use super::state::App;
use crate::gpu::GpuState;
use crate::tools::ToolId;
use std::sync::Arc;
use winit::{
    application::ApplicationHandler,
    event::{ElementState, KeyEvent, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow},
    keyboard::{KeyCode, PhysicalKey},
    window::{CursorIcon, Window, WindowId},
};

/// Logo shown by the OS (title bar / taskbar / Alt-Tab).
fn load_window_icon() -> Option<winit::window::Icon> {
    let bytes = include_bytes!("../../../logo_iAi.png");
    // OS icons are small; downscale so we never hand the platform a 3000² image.
    let src = image::load_from_memory(bytes).ok()?;
    let src = if src.width().max(src.height()) > 256 {
        src.resize(256, 256, image::imageops::FilterType::Lanczos3)
    } else {
        src
    };
    let img = src.into_rgba8();
    let (w, h) = img.dimensions();
    winit::window::Icon::from_rgba(img.into_raw(), w, h).ok()
}

impl App {
    fn alpha_view_allows_tool(&self, tool: ToolId) -> bool {
        !matches!(
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .channels
                .view,
            crate::core::channels::ChannelView::Alpha(_)
        ) || matches!(
            tool,
            ToolId::Brush | ToolId::Eraser | ToolId::Pencil | ToolId::Hand | ToolId::Zoom
        )
    }

    fn deny_alpha_view_tool(&mut self, tool: ToolId) {
        self.shell.status_msg = format!("{} cannot edit an alpha channel", tool.name());
        self.edit.input.painting = false;
        self.edit.pending_stroke_inputs.clear();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Whether `tool` may act on the active document while it is in CMYK mode.
    /// Selection/view/move/crop tools are always fine; pixel-writing tools are
    /// allowed only once they have an ink-native path. Tools not on the allow
    /// list would paint RGB through `get_tile_mut`, which drops the ink plane
    /// (fail-loud) and desyncs the document — so they are blocked with a message
    /// until their ink variant lands. RGB documents allow everything.
    fn cmyk_allows_tool(&self, tool: ToolId) -> bool {
        !self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .is_cmyk()
            || tool.allowed_in_cmyk()
    }

    fn deny_cmyk_tool(&mut self, tool: ToolId) {
        self.shell.status_msg = format!("{} chưa dùng được ở chế độ CMYK", tool.name());
        self.edit.input.painting = false;
        self.edit.pending_stroke_inputs.clear();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Show a native error dialog when the window / GPU can't initialize, so the
    /// release build doesn't "vanish silently" on a machine missing drivers. This is
    /// fatal → after reporting, the caller calls `event_loop.exit()` to quit cleanly.
    fn report_fatal_init(detail: &str) {
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("iAi — Không khởi động được")
            .set_description(format!(
                "{detail}\n\nThường do driver card màn hình quá cũ hoặc thiếu. \
                 Hãy cập nhật driver GPU rồi mở lại IAI."
            ))
            .show();
    }

    /// Chrome-bounds test at a physical-pixel window position: returns
    /// (over fixed UI chrome, outside the canvas rect). Mirrors the layout of
    /// the egui chrome (top bars, panels, status bar, rulers).
    pub(crate) fn ui_chrome_hit(&self, mx: f32, my: f32) -> (bool, bool) {
        let scale_factor = if let Some(w) = &self.win.window {
            w.scale_factor() as f32
        } else {
            1.0
        };
        let win_size = if let Some(w) = &self.win.window {
            w.inner_size()
        } else {
            winit::dpi::PhysicalSize::new(1280, 720)
        };

        let lx = mx / scale_factor;
        let ly = my / scale_factor;
        let lw = win_size.width as f32 / scale_factor;
        let lh = win_size.height as f32 / scale_factor;

        let ruler_size = if self.shell.ui.show_rulers { 20.0 } else { 0.0 };
        let top_ui = 28.0 + 26.0 + 32.0 + ruler_size;
        let bottom_ui = 22.0;
        let left_ui = self.shell.toolbar_w + ruler_size;
        let right_ui = self.shell.panel_r_w;

        let in_ui = ly < top_ui || ly > lh - bottom_ui || lx < left_ui || lx > lw - right_ui;
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let canvas_right = self.edit.view.offset_x + canvas.width as f32 * self.edit.view.zoom;
        let canvas_bottom = self.edit.view.offset_y + canvas.height as f32 * self.edit.view.zoom;
        let outside_canvas = mx < self.edit.view.offset_x
            || my < self.edit.view.offset_y
            || mx >= canvas_right
            || my >= canvas_bottom;
        (in_ui, outside_canvas)
    }

    /// True when the active tool belongs to the selection family (marquee / lasso
    /// / quick-select), so a right-click should open the selection context menu.
    /// Polygon lasso is excluded while a path is being placed — right-click there
    /// must not interrupt point placement.
    fn is_selection_tool_active(&self) -> bool {
        use crate::tools::ToolId::*;
        match self.edit.tools.active_id() {
            SelectionRect | SelectionEllipse | Lasso | SmartSelect => true,
            PolygonLasso => self.edit.tools.polygon_lasso().preview_points().is_empty(),
            _ => false,
        }
    }

    /// Index of the guide under the current cursor (within a few screen px), or
    /// None. Returns None when guides are hidden or locked.
    pub(crate) fn guide_at_screen(&self) -> Option<usize> {
        use crate::core::document::GuideOrientation;
        if !self.shell.ui.show_guides || self.shell.ui.lock_guides {
            return None;
        }
        let mx = self.edit.input.mouse_x;
        let my = self.edit.input.mouse_y;
        let zoom = self.edit.view.zoom;
        const HIT_PX: f32 = 5.0;
        self.docs.documents[self.docs.active_doc_idx]
            .guides
            .iter()
            .position(|g| match g.orientation {
                GuideOrientation::Vertical => {
                    (g.pos * zoom + self.edit.view.offset_x - mx).abs() <= HIT_PX
                }
                GuideOrientation::Horizontal => {
                    (g.pos * zoom + self.edit.view.offset_y - my).abs() <= HIT_PX
                }
            })
    }

    /// Poll startup font loading without presenting an intermediate splash. The
    /// window stays hidden until the first normal Welcome frame has been rendered.
    fn finish_startup_if_ready(&mut self, event_loop: &ActiveEventLoop) -> bool {
        if !matches!(
            self.win.startup_phase,
            crate::app::state::StartupPhase::Loading(_)
        ) {
            return true;
        }

        let mut new_logs: Vec<String> = Vec::new();
        let mut final_fonts = None;
        if let Some(rx) = &self.win.startup_rx {
            while let Ok(progress) = rx.try_recv() {
                match progress {
                    crate::app::state::StartupProgress::Log(s) => new_logs.push(s),
                    crate::app::state::StartupProgress::FontsReady(fonts) => {
                        final_fonts = Some(fonts);
                    }
                }
            }
        }
        self.win.startup_log.extend(new_logs);

        let Some(fonts) = final_fonts else {
            return false;
        };

        // Keep the definitions: GPU-device recovery rebuilds the egui context
        // and must re-install the same fonts (see `recover_gpu_device`).
        self.edit.text_fonts_registered = fonts
            .families
            .keys()
            .filter_map(|f| match f {
                egui::FontFamily::Name(n) if n.starts_with("iai_text_") => Some(n.to_string()),
                _ => None,
            })
            .collect();
        self.win.ui_fonts = Some(fonts.clone());
        self.win.egui_ctx.set_fonts(fonts);
        self.win.startup_phase = crate::app::state::StartupPhase::Done;
        self.win.startup_rx = None;
        self.fit_canvas_to_screen();
        self.push_cursor_uniforms();
        self.sync_cursor(event_loop);
        self.upload_full();
        true
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window = match event_loop.create_window(
            Window::default_attributes()
                .with_title("iAi")
                .with_window_icon(load_window_icon())
                // Borderless (custom titlebar) but open maximised so it fills the
                // screen; 1280×720 is only the restored (un-maximised) size.
                .with_inner_size(winit::dpi::LogicalSize::new(1280u32, 720u32))
                .with_min_inner_size(winit::dpi::LogicalSize::new(640u32, 400u32))
                .with_maximized(true)
                .with_decorations(false)
                .with_visible(false),
        ) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                Self::report_fatal_init(&format!("Không tạo được cửa sổ ứng dụng: {e}"));
                event_loop.exit();
                return;
            }
        };

        let egui_state = egui_winit::State::new(
            self.win.egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            None,
            None,
            None,
        );
        self.win.egui_state = Some(egui_state);

        let (tx, rx) = std::sync::mpsc::channel();
        self.win.startup_rx = Some(rx);
        self.win.startup_phase =
            crate::app::state::StartupPhase::Loading("Đang khởi tạo nền tảng...".to_string());

        std::thread::spawn(move || {
            // Small pauses keep startup work staged without blocking the UI thread.
            let beat = || std::thread::sleep(std::time::Duration::from_millis(130));
            beat();
            let _ = tx.send(crate::app::state::StartupProgress::Log(
                "Đang tải font giao diện...".to_string(),
            ));
            let mut fonts = egui::FontDefinitions::default();
            // Only the builtin families are registered up front; other system
            // fonts are registered on demand when the user picks them (see
            // `ensure_text_font_registered`) — loading every installed font
            // into egui would cost hundreds of MB and a slow startup.
            for family in &crate::core::text::builtin_families() {
                if let Some(data) = crate::core::text::font_bytes_for(family) {
                    let id = family.egui_family_name();
                    fonts.font_data.insert(
                        id.clone(),
                        std::sync::Arc::new(egui::FontData::from_owned(data)),
                    );
                    fonts
                        .families
                        .insert(egui::FontFamily::Name(id.clone().into()), vec![id]);
                }
            }
            beat();
            let _ = tx.send(crate::app::state::StartupProgress::Log(
                "Đang tải font tiếng Việt...".to_string(),
            ));
            for path in [
                "C:/Windows/Fonts/segoeui.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
                "/System/Library/Fonts/Supplemental/Arial.ttf",
            ] {
                if let Ok(data) = std::fs::read(path) {
                    fonts.font_data.insert(
                        "ui_vietnamese".to_owned(),
                        std::sync::Arc::new(egui::FontData::from_owned(data)),
                    );
                    if let Some(v) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
                        v.insert(0, "ui_vietnamese".to_owned());
                    }
                    if let Some(v) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
                        v.insert(0, "ui_vietnamese".to_owned());
                    }
                    break;
                }
            }
            beat();
            let _ = tx.send(crate::app::state::StartupProgress::Log(
                "Đang tải font biểu tượng...".to_string(),
            ));
            for path in [
                "C:/Windows/Fonts/seguisym.ttf",
                "C:/Windows/Fonts/seguiemj.ttf",
            ] {
                if let Ok(data) = std::fs::read(path) {
                    fonts.font_data.insert(
                        "segoe_symbol".to_owned(),
                        std::sync::Arc::new(egui::FontData::from_owned(data)),
                    );
                    if let Some(v) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
                        v.push("segoe_symbol".to_owned());
                    }
                    if let Some(v) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
                        v.push("segoe_symbol".to_owned());
                    }
                    break;
                }
            }
            beat();
            let _ = tx.send(crate::app::state::StartupProgress::Log(
                "Đang khởi tạo icon...".to_string(),
            ));
            egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
            beat();
            let _ = tx.send(crate::app::state::StartupProgress::Log(
                "Hoàn tất!".to_string(),
            ));
            beat();
            let _ = tx.send(crate::app::state::StartupProgress::FontsReady(fonts));
            // Warm the system font index in the background so opening the
            // font dropdown later doesn't block the UI thread on a full scan.
            crate::core::text::warm_system_font_index();
        });

        // We defer upload_full to RedrawRequested. GpuState is still created now
        // while the window is hidden so the first visible frame can be Welcome.
        self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .ensure_pixels();
        let gpu = match GpuState::new(
            Arc::clone(&window),
            &self.docs.documents[self.docs.active_doc_idx].canvas.pixels,
            self.docs.documents[self.docs.active_doc_idx].canvas.width,
            self.docs.documents[self.docs.active_doc_idx].canvas.height,
        ) {
            Ok(g) => g,
            Err(e) => {
                Self::report_fatal_init(&e);
                event_loop.exit();
                return;
            }
        };
        // Tell egui the adapter's REAL texture limit. Without this egui assumes
        // a 2048px ceiling: big textures panic and — worse — the font atlas
        // caps at 2048² and hits its "almost full → recreate" cycle far more
        // often, each recreate being a full-image delta that must not be lost.
        if let Some(state) = &mut self.win.egui_state {
            state.set_max_texture_side(gpu.max_texture_dimension as usize);
        }
        self.win.gpu = Some(gpu);
        self.win.window = Some(window);

        // We will do fit_canvas_to_screen, upload_full, render Welcome, then show the window.
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        // Route events for the Develop second OS window (D1) to its own handler;
        // everything else is the main window (there are at most two windows).
        if self
            .win
            .develop_window
            .as_ref()
            .is_some_and(|w| w.id() == id)
        {
            self.develop_window_event(event_loop, event);
            return;
        }
        // During the two-phase Develop teardown the old OS window remains as a
        // visual cover for one main presentation, but it is no longer an input
        // host. Do not misroute its final paint/focus events to the main window.
        if self
            .win
            .retiring_develop_window
            .as_ref()
            .is_some_and(|(w, _)| w.id() == id)
        {
            return;
        }

        // Numpad Enter commits the text overlay. egui-winit maps both the main
        // and numpad Enter to the same `Key::Enter`, so it must be caught here
        // (before egui sees it) to keep the main Enter as a newline.
        if self.edit.text_edit.is_some() {
            if let WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(KeyCode::NumpadEnter),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } = &event
            {
                if !(self.shell.ui.show_paint_color_dialog
                    && self.shell.ui.paint_color_dialog_target == 2)
                {
                    self.commit_text_edit();
                    if let Some(w) = &self.win.window {
                        w.request_redraw();
                    }
                    return;
                }
            }
        }

        // Ctrl+N is handled by the app shortcut layer below and also advertised
        // as an egui menu shortcut. If the key is first queued into egui and then
        // opens the modal here, the same raw input is replayed during the next UI
        // pass and gets denied by the modal lock, producing the OS error bell.
        // Consume it before egui sees it so New Canvas opens once and silently.
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    physical_key: PhysicalKey::Code(KeyCode::KeyN),
                    state,
                    repeat,
                    ..
                },
            ..
        } = &event
        {
            if self.edit.input.ctrl_held {
                if *state == ElementState::Pressed && !repeat && !self.shell.ui.show_new_dialog {
                    if self.modal_lock_active() {
                        self.deny_modal_action();
                    } else {
                        self.open_new_canvas_dialog_with_clipboard_hint();
                        if let Some(w) = &self.win.window {
                            w.request_redraw();
                        }
                    }
                }
                return;
            }
        }

        if let Some(state) = &mut self.win.egui_state {
            if let Some(window) = &self.win.window {
                let resp = state.on_window_event(window, &event);
                if resp.repaint {
                    window.request_redraw();
                }
                self.edit.input.is_over_ui = self.win.egui_ctx.egui_wants_pointer_input()
                    || self.win.egui_ctx.is_pointer_over_egui()
                    || self.win.egui_ctx.egui_wants_keyboard_input();

                if self.edit.text_edit.is_some()
                    && matches!(
                        event,
                        WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
                    )
                {
                    let mut view_key = false;
                    if let WindowEvent::KeyboardInput {
                        event:
                            KeyEvent {
                                physical_key,
                                state,
                                ..
                            },
                        ..
                    } = &event
                    {
                        let pressed = *state == ElementState::Pressed;
                        match physical_key {
                            PhysicalKey::Code(KeyCode::AltLeft)
                            | PhysicalKey::Code(KeyCode::AltRight) => {
                                self.edit.input.alt_held = pressed;
                                if !pressed {
                                    self.edit.input.alt_right_dragging = false;
                                }
                            }
                            PhysicalKey::Code(KeyCode::ControlLeft)
                            | PhysicalKey::Code(KeyCode::ControlRight) => {
                                self.edit.input.ctrl_held = pressed;
                            }
                            PhysicalKey::Code(KeyCode::ShiftLeft)
                            | PhysicalKey::Code(KeyCode::ShiftRight) => {
                                self.edit.input.shift_held = pressed;
                            }
                            _ => {}
                        }
                        // View shortcuts (zoom/fit) stay live while editing;
                        // everything else belongs to the TextEdit.
                        view_key = pressed
                            && self.edit.input.ctrl_held
                            && matches!(
                                physical_key,
                                PhysicalKey::Code(KeyCode::Digit0)
                                    | PhysicalKey::Code(KeyCode::Numpad0)
                                    | PhysicalKey::Code(KeyCode::Digit1)
                                    | PhysicalKey::Code(KeyCode::Numpad1)
                                    | PhysicalKey::Code(KeyCode::Equal)
                                    | PhysicalKey::Code(KeyCode::Minus)
                                    | PhysicalKey::Code(KeyCode::NumpadAdd)
                                    | PhysicalKey::Code(KeyCode::NumpadSubtract)
                            );
                    }
                    if !view_key {
                        return;
                    }
                }

                if resp.consumed {
                    match &event {
                        WindowEvent::ModifiersChanged(_) => {}
                        WindowEvent::CursorMoved { .. } => {}
                        WindowEvent::MouseInput {
                            state: ElementState::Released,
                            ..
                        } => {}
                        WindowEvent::KeyboardInput {
                            event:
                                KeyEvent {
                                    physical_key: PhysicalKey::Code(KeyCode::KeyY),
                                    state: ElementState::Pressed,
                                    ..
                                },
                            ..
                        } if self.edit.input.ctrl_held => {}
                        WindowEvent::KeyboardInput {
                            event:
                                KeyEvent {
                                    physical_key: PhysicalKey::Code(KeyCode::KeyC),
                                    state: ElementState::Pressed,
                                    ..
                                },
                            ..
                        } if self.edit.input.ctrl_held => {}
                        WindowEvent::KeyboardInput {
                            event:
                                KeyEvent {
                                    physical_key: PhysicalKey::Code(KeyCode::KeyA),
                                    state: ElementState::Pressed,
                                    ..
                                },
                            ..
                        } if self.edit.input.ctrl_held => {}
                        WindowEvent::KeyboardInput {
                            event:
                                KeyEvent {
                                    physical_key: PhysicalKey::Code(KeyCode::KeyV),
                                    state: ElementState::Pressed,
                                    ..
                                },
                            ..
                        } if self.edit.input.ctrl_held => {}
                        WindowEvent::KeyboardInput {
                            event:
                                KeyEvent {
                                    physical_key: PhysicalKey::Code(KeyCode::KeyO),
                                    state: ElementState::Pressed,
                                    ..
                                },
                            ..
                        } if self.edit.input.ctrl_held => {}
                        WindowEvent::KeyboardInput {
                            event:
                                KeyEvent {
                                    physical_key:
                                        PhysicalKey::Code(KeyCode::KeyB)
                                        | PhysicalKey::Code(KeyCode::KeyL)
                                        | PhysicalKey::Code(KeyCode::KeyU)
                                        | PhysicalKey::Code(KeyCode::KeyM)
                                        | PhysicalKey::Code(KeyCode::KeyG)
                                        | PhysicalKey::Code(KeyCode::KeyI),
                                    state: ElementState::Pressed,
                                    ..
                                },
                            ..
                        } if self.edit.input.ctrl_held => {}
                        // View shortcuts forwarded past the text-editing gate
                        // above (egui's TextEdit has focus but ignores them).
                        WindowEvent::KeyboardInput {
                            event:
                                KeyEvent {
                                    physical_key:
                                        PhysicalKey::Code(KeyCode::Digit0)
                                        | PhysicalKey::Code(KeyCode::Numpad0)
                                        | PhysicalKey::Code(KeyCode::Digit1)
                                        | PhysicalKey::Code(KeyCode::Numpad1)
                                        | PhysicalKey::Code(KeyCode::Equal)
                                        | PhysicalKey::Code(KeyCode::Minus)
                                        | PhysicalKey::Code(KeyCode::NumpadAdd)
                                        | PhysicalKey::Code(KeyCode::NumpadSubtract),
                                    state: ElementState::Pressed,
                                    ..
                                },
                            ..
                        } if self.edit.input.ctrl_held && self.edit.text_edit.is_some() => {}
                        // While editing text the wheel still zooms/pans the
                        // canvas — egui consumes it only because the pointer
                        // sits on the invisible overlay TextEdit.
                        WindowEvent::MouseWheel { .. }
                            if self.edit.text_edit.is_some()
                                && !self
                                    .ui_chrome_hit(self.edit.input.mouse_x, self.edit.input.mouse_y)
                                    .0 => {}
                        _ => return,
                    }
                }
            }
        }

        let (is_in_ui_bounds, is_outside_canvas) = {
            let (mx, my) = match &event {
                WindowEvent::CursorMoved { position, .. } => (position.x as f32, position.y as f32),
                _ => (self.edit.input.mouse_x, self.edit.input.mouse_y),
            };
            self.ui_chrome_hit(mx, my)
        };

        let modal_ui = self.is_modal_open() && !self.shell.ui.show_paint_color_dialog;
        // Tools/states that legitimately act on the gray pasteboard outside the
        // page. Brush-like tools need their center to cross the page edge so they
        // can paint cleanly up to it; the actual pixel writes remain canvas-clipped.
        // The vector tools (Pen/Shape/Node/Gradient/Text) work in canvas space —
        // their anchors, geometry, handles and layers may legitimately sit off the
        // page — so they must keep receiving pointer events there too.
        // For these tools, only the surrounding chrome counts as UI.
        let pasteboard_ok = self.edit.transform_state.is_some()
            || matches!(
                self.edit.tools.active_id(),
                ToolId::Brush
                    | ToolId::Eraser
                    | ToolId::Pencil
                    | ToolId::Clone
                    | ToolId::Repair
                    | ToolId::RefineBrush
                    | ToolId::Smudge
                    | ToolId::Dodge
                    | ToolId::Burn
                    | ToolId::Crop
                    | ToolId::PerspectiveCrop
                    | ToolId::Move
                    | ToolId::Pen
                    | ToolId::Shape
                    | ToolId::Node
                    | ToolId::Gradient
                    | ToolId::Text
                    | ToolId::SelectionRect
                    | ToolId::SelectionEllipse
                    | ToolId::Lasso
                    | ToolId::PolygonLasso
                    | ToolId::SmartSelect
            );
        self.edit.input.in_ui_chrome = is_in_ui_bounds;
        let current_ui_state = self.edit.input.is_over_ui
            || modal_ui
            || is_in_ui_bounds
            || (is_outside_canvas && !pasteboard_ok);

        if self.edit.input.was_over_ui != current_ui_state {
            self.edit.input.was_over_ui = current_ui_state;
            self.sync_cursor(event_loop);
        }

        // Any real event (input, resize, async-driven) means the next frame may
        // carry new UI state, so the cached-UI ants fast path must not be taken.
        if !matches!(event, WindowEvent::RedrawRequested) {
            self.win.ants_redraw_pending = false;
        }

        match event {
            WindowEvent::CloseRequested => {
                if self.request_app_exit() {
                    self.clear_all_autosave();
                    event_loop.exit();
                }
            }

            WindowEvent::CursorLeft { .. } => {
                if let Some(w) = &self.win.window {
                    w.set_cursor_visible(true);
                    w.set_cursor(CursorIcon::Default);
                }
            }

            WindowEvent::CursorEntered { .. } => {
                self.sync_cursor(event_loop);
            }

            // Losing focus (Alt-Tab, etc.) means the matching mouse-release may
            // never arrive. Finalize any in-progress stroke into history and clear
            // the drag flags so we neither lose the edit nor get stuck "painting".
            WindowEvent::Focused(focused) => {
                self.win.window_focused = focused;
                if !focused {
                    if self.edit.input.painting {
                        self.docs.documents[self.docs.active_doc_idx]
                            .canvas
                            .end_stroke();
                        self.apply_canvas_event(
                            crate::app::render::CanvasEvent::LayerPixelsChanged,
                        );
                    }
                    self.edit.input.painting = false;
                    self.edit.input.mid_dragging = false;
                    self.edit.input.space_dragging = false;
                    self.edit.input.alt_right_dragging = false;
                    self.edit.input.zoom_dragging = false;
                    self.edit.input.zoom_drag_moved = false;
                }
            }

            WindowEvent::Resized(sz) => {
                if sz.width > 0 && sz.height > 0 {
                    if let Some(gpu) = &mut self.win.gpu {
                        let device = &gpu.device;
                        gpu.main.resize(device, sz.width, sz.height);
                    }
                    // Re-centre / re-clamp the canvas for the new viewport size,
                    // otherwise after a maximise the canvas keeps its old offset and
                    // only snaps to centre on the next scroll/pan (which calls this).
                    self.constrain_pan();
                    self.push_canvas_uniforms();
                    self.on_view_changed();
                    if let Some(w) = &self.win.window {
                        w.request_redraw();
                    }
                }
            }

            WindowEvent::DroppedFile(path) => {
                // Strict modal lock: opening documents is refused while a
                // modal operation is in progress.
                if self.modal_lock_active() {
                    self.deny_modal_action();
                    return;
                }
                // Decode off the UI thread (a large dropped image used to freeze the
                // window) and open it as a new tab via the shared async loader.
                self.start_load_paths(vec![path]);
            }

            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key,
                        state,
                        repeat,
                        ..
                    },
                ..
            } => self.on_main_keyboard_input(event_loop, physical_key, state, repeat),

            WindowEvent::MouseWheel { delta, .. } => {
                if self.is_blocking_modal() {
                    return;
                }
                if self.edit.input.alt_right_dragging {
                    return;
                }
                let (sx, sy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x as f64, y as f64),
                    MouseScrollDelta::PixelDelta(p) => (p.x * 0.1, p.y * 0.1),
                };
                if self.edit.input.alt_held {
                    let f = if sy > 0.0 { 1.15f32 } else { 1.0 / 1.15 };
                    let old_zoom = self.edit.view.zoom;
                    let new_zoom = (old_zoom * f).clamp(0.02, 64.0);
                    let actual_f = new_zoom / old_zoom;
                    self.edit.view.offset_x = self.edit.input.mouse_x
                        - (self.edit.input.mouse_x - self.edit.view.offset_x) * actual_f;
                    self.edit.view.offset_y = self.edit.input.mouse_y
                        - (self.edit.input.mouse_y - self.edit.view.offset_y) * actual_f;
                    self.edit.view.zoom = new_zoom;
                } else if self.edit.input.ctrl_held {
                    self.edit.view.offset_x += sy as f32 * 40.0;
                } else {
                    self.edit.view.offset_y += sy as f32 * 40.0;
                    self.edit.view.offset_x += sx as f32 * 40.0;
                }
                self.constrain_pan();
                self.push_canvas_uniforms();
                self.win.pending_view_change = true;
                self.win.last_cursor_radius = 0;
                self.sync_cursor(event_loop);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }

            WindowEvent::ModifiersChanged(modifiers) => {
                let state = modifiers.state();
                self.edit.input.alt_held = state.alt_key();
                self.edit.input.ctrl_held = state.control_key();
                self.edit.input.shift_held = state.shift_key();
                if !self.edit.input.alt_held {
                    self.edit.input.alt_right_dragging = false;
                }
                self.sync_cursor(event_loop);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }

            WindowEvent::MouseInput { state, button, .. } => {
                self.on_main_mouse_input(event_loop, state, button)
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.on_main_cursor_moved(event_loop, position)
            }

            WindowEvent::RedrawRequested => self.on_main_redraw(event_loop),

            _ => {}
        }

        let lama_downloading = crate::core::lama::is_downloading();
        if lama_downloading {
            if let Some(s) = crate::core::lama::status_text() {
                self.shell.status_msg = s;
            }
        }

        let view_change_ready = self.win.pending_view_change
            && self
                .win
                .view_recompose_deadline
                .map_or(true, |deadline| std::time::Instant::now() >= deadline);
        let needs_redraw = self.edit.input.painting
            || !self.edit.pending_stroke_inputs.is_empty()
            || view_change_ready
            || self.win.interactive_recompose_pending
            || self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .selection
                .active
            || self.edit.input.mid_dragging
            || self.edit.input.space_dragging
            || self.edit.input.alt_right_dragging
            || self.edit.transform_state.is_some()
            || self.edit.pending_transform_commit.is_some()
            || self.jobs.select_subject.is_busy()
            || self.jobs.ai_engine.has_jobs()
            || lama_downloading
            || self.jobs.pending_file_dialog.is_some()
            || !self.jobs.pending_loads.is_empty()
            || self
                .shell
                .filter_preview
                .as_ref()
                .is_some_and(|p| p.processing)
            || self
                .dev
                .develop_preview
                .as_ref()
                .is_some_and(|p| p.processing || p.detail_refine_at.is_some());

        if needs_redraw {
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
    }

    /// Called when the event queue is empty — the right place to set ControlFlow.
    /// Without this, winit defaults to Poll → spin loop → 12-14% idle CPU.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if matches!(
            self.win.startup_phase,
            crate::app::state::StartupPhase::Loading(_)
        ) {
            if self.finish_startup_if_ready(event_loop) {
                if let Some(w) = &self.win.window {
                    if !self.win.window_visible {
                        w.set_visible(true);
                        self.win.window_visible = true;
                    }
                    w.request_redraw();
                }
                event_loop.set_control_flow(ControlFlow::Poll);
            } else {
                event_loop.set_control_flow(ControlFlow::WaitUntil(
                    std::time::Instant::now() + std::time::Duration::from_millis(16),
                ));
            }
            return;
        }

        if self.jobs.pending_file_dialog.is_some()
            || !self.jobs.pending_loads.is_empty()
            || self.edit.pending_transform_commit.is_some()
            || self
                .shell
                .filter_preview
                .as_ref()
                .is_some_and(|p| p.processing)
            || self
                .dev
                .develop_preview
                .as_ref()
                .is_some_and(|p| p.processing || p.detail_refine_at.is_some())
            || self.jobs.ext.busy()
        {
            // Both windows: the main window's RedrawRequested is what pumps the
            // extension bridge / PDF probes, but its paints can be coalesced
            // while the Develop window covers it — so the Develop window (which
            // pumps the file/RAW workers itself) is woken as well.
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            if let Some(w) = &self.win.develop_window {
                w.request_redraw();
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                std::time::Instant::now() + std::time::Duration::from_millis(33),
            ));
            return;
        }

        let interacting = self.edit.input.painting
            || !self.edit.pending_stroke_inputs.is_empty()
            || self.edit.transform_state.is_some()
            || self.win.interactive_recompose_pending;

        if interacting {
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            event_loop.set_control_flow(ControlFlow::Poll);
            return;
        }

        if self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .selection
            .active
        {
            let now = std::time::Instant::now();
            let frame = std::time::Duration::from_millis(66);
            if now.duration_since(self.win.last_ants_frame) >= frame {
                self.win.last_ants_frame = now;
                // Mark this redraw as ants-driven so RedrawRequested can reuse the
                // cached UI (see the `ants_only` fast path). Cleared by any other
                // window event before the redraw lands.
                self.win.ants_redraw_pending = true;
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
                event_loop.set_control_flow(ControlFlow::Poll);
            } else {
                event_loop
                    .set_control_flow(ControlFlow::WaitUntil(self.win.last_ants_frame + frame));
            }
            return;
        }

        // While the AI panel is open on a web (extension) source, tick a few times a
        // second so the connection status / progress from the extension shows up
        // promptly even when the UI is otherwise idle. (Cheap; no EventLoopProxy.)
        if self.shell.ui.show_ai_panel && self.shell.ui.ai.source.is_web() {
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                std::time::Instant::now() + std::time::Duration::from_millis(250),
            ));
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return;
        }

        match self.win.egui_repaint_deadline {
            None => {
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            Some(deadline) => {
                let now = std::time::Instant::now();
                if now >= deadline {
                    if let Some(w) = &self.win.window {
                        w.request_redraw();
                    }
                    event_loop.set_control_flow(ControlFlow::Poll);
                } else {
                    event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
                }
            }
        }
    }
}

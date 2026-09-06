//! Cursor ownership shared by native tool cursors and the GPU brush ring.

#[derive(Default)]
pub(super) struct CursorOwnership {
    pub pointer_inside: bool,
    applied: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum CursorUpdate {
    Apply,
    Release,
    Ignore,
}

impl CursorOwnership {
    pub fn can_control(&self, focused: bool, native_dialog: bool) -> bool {
        self.pointer_inside && focused && !native_dialog
    }

    pub fn update(&mut self, focused: bool, native_dialog: bool) -> CursorUpdate {
        if self.can_control(focused, native_dialog) {
            self.applied = true;
            CursorUpdate::Apply
        } else if std::mem::take(&mut self.applied) {
            self.pointer_inside = false;
            // Release once. Repeated SetCursor(Default) calls from background
            // redraws would also overwrite the next window's cursor.
            CursorUpdate::Release
        } else {
            CursorUpdate::Ignore
        }
    }
}

pub(super) fn apply_ui_cursor(window: &winit::window::Window, icon: egui::CursorIcon) {
    let native = native_ui_cursor(icon);
    window.set_cursor_visible(native.is_some());
    if let Some(native) = native {
        window.set_cursor(native);
    }
}

fn native_ui_cursor(icon: egui::CursorIcon) -> Option<winit::window::CursorIcon> {
    use egui::CursorIcon as E;
    use winit::window::CursorIcon as W;
    Some(match icon {
        E::None => return None,
        E::Default => W::Default,
        E::ContextMenu => W::ContextMenu,
        E::Help => W::Help,
        E::PointingHand => W::Pointer,
        E::Progress => W::Progress,
        E::Wait => W::Wait,
        E::Cell => W::Cell,
        E::Crosshair => W::Crosshair,
        E::Text => W::Text,
        E::VerticalText => W::VerticalText,
        E::Alias => W::Alias,
        E::Copy => W::Copy,
        E::Move => W::Move,
        E::NoDrop => W::NoDrop,
        E::NotAllowed => W::NotAllowed,
        E::Grab => W::Grab,
        E::Grabbing => W::Grabbing,
        E::AllScroll => W::AllScroll,
        E::ResizeHorizontal => W::EwResize,
        E::ResizeNeSw => W::NeswResize,
        E::ResizeNwSe => W::NwseResize,
        E::ResizeVertical => W::NsResize,
        E::ResizeEast => W::EResize,
        E::ResizeSouthEast => W::SeResize,
        E::ResizeSouth => W::SResize,
        E::ResizeSouthWest => W::SwResize,
        E::ResizeWest => W::WResize,
        E::ResizeNorthWest => W::NwResize,
        E::ResizeNorth => W::NResize,
        E::ResizeNorthEast => W::NeResize,
        E::ResizeColumn => W::ColResize,
        E::ResizeRow => W::RowResize,
        E::ZoomIn => W::ZoomIn,
        E::ZoomOut => W::ZoomOut,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaving_canvas_releases_once_and_background_redraws_stay_quiet() {
        let mut cursor = CursorOwnership::default();
        cursor.pointer_inside = true;
        assert_eq!(cursor.update(true, false), CursorUpdate::Apply);
        cursor.pointer_inside = false;
        assert_eq!(cursor.update(true, false), CursorUpdate::Release);
        for _ in 0..10 {
            assert_eq!(cursor.update(true, false), CursorUpdate::Ignore);
            assert!(!cursor.can_control(true, false)); // GPU ring stays off too.
        }
        cursor.pointer_inside = true;
        assert_eq!(cursor.update(true, false), CursorUpdate::Apply);
    }

    #[test]
    fn alt_tab_or_native_dialog_blocks_even_with_a_stale_canvas_position() {
        for (focused, native_dialog) in [(false, false), (true, true)] {
            let mut cursor = CursorOwnership::default();
            cursor.pointer_inside = true;
            assert_eq!(cursor.update(true, false), CursorUpdate::Apply);
            assert_eq!(cursor.update(focused, native_dialog), CursorUpdate::Release);
            assert!(!cursor.can_control(focused, native_dialog));
            assert_eq!(cursor.update(focused, native_dialog), CursorUpdate::Ignore);
            assert_eq!(cursor.update(true, false), CursorUpdate::Ignore);
            cursor.pointer_inside = true; // A fresh position permits re-entry.
            assert_eq!(cursor.update(true, false), CursorUpdate::Apply);
        }
    }

    #[test]
    fn ui_handoff_preserves_text_resize_and_visibility() {
        use winit::window::CursorIcon as W;
        assert_eq!(
            native_ui_cursor(egui::CursorIcon::Default),
            Some(W::Default)
        );
        assert_eq!(native_ui_cursor(egui::CursorIcon::Text), Some(W::Text));
        assert_eq!(
            native_ui_cursor(egui::CursorIcon::ResizeHorizontal),
            Some(W::EwResize)
        );
        assert_eq!(native_ui_cursor(egui::CursorIcon::None), None);
    }

    #[test]
    fn floating_window_takes_cursor_without_another_mouse_move() {
        let mut app = crate::app::App::new();
        app.shell.ui.show_welcome = false;
        app.edit.tools.select(crate::tools::ToolId::Brush);
        app.win.cursor_ownership.pointer_inside = true;
        let pos = egui::pos2(600.0, 300.0);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 720.0),
            )),
            events: vec![egui::Event::PointerMoved(pos)],
            ..Default::default()
        };
        let _ = app.win.egui_ctx.run_ui(input, |_| {});
        app.refresh_pointer_ui_state(pos.x, pos.y);
        assert!(!app.edit.input.was_over_ui);

        // The pointer is stationary. A floating window appears over the canvas.
        for _ in 0..3 {
            let _ = app.win.egui_ctx.run_ui(egui::RawInput::default(), |ctx| {
                egui::Window::new("Cursor regression")
                    .fixed_pos(egui::pos2(500.0, 200.0))
                    .fixed_size(egui::vec2(250.0, 200.0))
                    .show(ctx, |ui| {
                        ui.set_min_size(egui::vec2(250.0, 200.0));
                        ui.label("Dialog content");
                    });
            });
        }
        app.refresh_pointer_ui_state(pos.x, pos.y);
        assert!(app.edit.input.was_over_ui);

        // Closing it must return cursor ownership without another pointer event.
        for _ in 0..3 {
            let _ = app.win.egui_ctx.run_ui(egui::RawInput::default(), |_| {});
        }
        app.refresh_pointer_ui_state(pos.x, pos.y);
        assert!(!app.edit.input.was_over_ui);
    }
}

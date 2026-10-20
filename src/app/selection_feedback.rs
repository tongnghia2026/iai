//! What the marquee and lasso tools tell the user: a cursor that shows what
//! the next press will do (draw in New / Add / Subtract / Intersect, or drag
//! the selection), and, once a gesture ends, a journal line plus a status-bar
//! note when the drag came to nothing.

use crate::app::state::App;
use crate::core::selection::{Selection, SelectionMode};
use crate::tools::selection_common::{self, NOTHING_HAPPENED_NOTES};
use crate::tools::{PointerEvent, ToolId};
use winit::event_loop::ActiveEventLoop;
use winit::window::CustomCursor;

/// A cursor bitmap: RGBA pixels, width, height, hotspot x, hotspot y.
pub(in crate::app) type CursorRgba = (Vec<u8>, u16, u16, u16, u16);

/// What the next press of a marquee / lasso does, as its cursor shows it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::app) enum SelectionCursor {
    /// Draw a shape combined in this mode (badged unless New).
    Draw(SelectionMode),
    /// Drag the selection: a New-mode press inside it.
    Move,
}

/// The cursor for a press that would come with `event`; `placing_polygon`
/// while a polygon lasso has corners down (its presses add corners).
pub(in crate::app) fn selection_cursor_for(
    event: &PointerEvent,
    selection: &Selection,
    placing_polygon: bool,
) -> SelectionCursor {
    if !placing_polygon && selection_common::press_moves_selection(event, selection) {
        SelectionCursor::Move
    } else {
        SelectionCursor::Draw(event.selection_mode)
    }
}

fn mode_index(mode: SelectionMode) -> usize {
    match mode {
        SelectionMode::New => 0,
        SelectionMode::Add => 1,
        SelectionMode::Subtract => 2,
        SelectionMode::Intersect => 3,
    }
}

/// The marquee / lasso cursors, built on first use.
#[derive(Default)]
pub(in crate::app) struct SelectionCursors {
    marquee: [Option<CustomCursor>; 4],
    lasso: [Option<CustomCursor>; 4],
    move_selection: Option<CustomCursor>,
    /// The kind on screen. Kept through a drag, so the cursor does not flip
    /// as the pointer crosses the selection's edge.
    shown: Option<SelectionCursor>,
}

/// Photoshop's mode mark beside a selection cursor: `+` Add, `−` Subtract,
/// `×` Intersect (New has none). A 7×7 white mark on a dark halo, its top-left
/// at (`x0`, `y0`).
pub(in crate::app) fn paint_mode_badge(
    rgba: &mut [u8],
    w: usize,
    h: usize,
    x0: usize,
    y0: usize,
    mode: SelectionMode,
) {
    let core: Vec<(usize, usize)> = match mode {
        SelectionMode::New => return,
        SelectionMode::Add => (0..7).flat_map(|i| [(i, 3), (3, i)]).collect(),
        SelectionMode::Subtract => (0..7).map(|i| (i, 3)).collect(),
        SelectionMode::Intersect => (0..7).flat_map(|i| [(i, i), (6 - i, i)]).collect(),
    };
    let mut put = |x: usize, y: usize, px: [u8; 4]| {
        if x < w && y < h {
            let i = (y * w + x) * 4;
            if let Some(dst) = rgba.get_mut(i..i + 4) {
                dst.copy_from_slice(&px);
            }
        }
    };
    for &(cx, cy) in &core {
        for dy in 0..3 {
            for dx in 0..3 {
                if (x0 + cx + dx) >= 1 && (y0 + cy + dy) >= 1 {
                    put(x0 + cx + dx - 1, y0 + cy + dy - 1, [0, 0, 0, 210]);
                }
            }
        }
    }
    for &(cx, cy) in &core {
        put(x0 + cx, y0 + cy, [255, 255, 255, 255]);
    }
}

/// The marquee crosshair (hotspot at its centre), with the mode's badge to
/// its lower right.
pub(in crate::app) fn marquee_cursor_rgba(mode: SelectionMode) -> CursorRgba {
    const SIZE: usize = 21;
    const CENTER: usize = 9;
    const ARM: usize = 19;
    const GAP: usize = 2;
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    let mut put = |x: usize, y: usize, col: u8, alpha: u8| {
        let i = (y * SIZE + x) * 4;
        rgba[i..i + 4].copy_from_slice(&[col, col, col, alpha]);
    };
    for i in 0..ARM {
        if i + GAP >= CENTER && i <= CENTER + GAP {
            continue;
        }
        for (off, col, alpha) in [(-1i32, 0u8, 210u8), (0, 255, 245), (1, 0, 210)] {
            let line = (CENTER as i32 + off) as usize;
            put(i, line, col, alpha);
            put(line, i, col, alpha);
        }
    }
    paint_mode_badge(&mut rgba, SIZE, SIZE, 13, 13, mode);
    (rgba, SIZE as u16, SIZE as u16, CENTER as u16, CENTER as u16)
}

impl App {
    /// The cursor of the active marquee / lasso; `None` for any other tool.
    pub(in crate::app) fn selection_tool_cursor(
        &mut self,
        event_loop: &ActiveEventLoop,
    ) -> Option<CustomCursor> {
        let tool = self.edit.tools.active_id();
        let lasso = match tool {
            ToolId::SelectionRect | ToolId::SelectionEllipse => false,
            ToolId::Lasso | ToolId::PolygonLasso => true,
            _ => return None,
        };
        let kind = match self.win.selection_cursors.shown {
            Some(kind) if self.edit.input.painting => kind,
            _ => {
                let placing = tool == ToolId::PolygonLasso
                    && !self.edit.tools.polygon_lasso().preview_points().is_empty();
                let event = self.tool_event();
                selection_cursor_for(&event, &self.docs.active().canvas.selection, placing)
            }
        };
        let cursors = &mut self.win.selection_cursors;
        cursors.shown = Some(kind);
        let slot = match kind {
            SelectionCursor::Move => &mut cursors.move_selection,
            SelectionCursor::Draw(mode) if lasso => &mut cursors.lasso[mode_index(mode)],
            SelectionCursor::Draw(mode) => &mut cursors.marquee[mode_index(mode)],
        };
        if slot.is_none() {
            let rgba = match kind {
                SelectionCursor::Move => Self::move_selection_cursor_rgba(),
                SelectionCursor::Draw(mode) if lasso => Self::lasso_cursor_rgba(mode),
                SelectionCursor::Draw(mode) => Some(marquee_cursor_rgba(mode)),
            };
            *slot = rgba
                .and_then(|(px, w, h, hx, hy)| CustomCursor::from_rgba(px, w, h, hx, hy).ok())
                .map(|source| event_loop.create_custom_cursor(source));
        }
        slot.clone()
    }

    /// After a marquee / lasso gesture: a journal line for what it did, and a
    /// status-bar note when the drag came to nothing — taken down again by
    /// the next one that works.
    pub(in crate::app) fn report_selection_outcome(&mut self) {
        let Some(outcome) = self.edit.tools.take_selection_outcome() else {
            return;
        };
        crate::diag::note("select", &outcome.journal_text());
        match outcome.nothing_happened_note() {
            Some(note) => self.shell.status_msg = note.to_string(),
            None if NOTHING_HAPPENED_NOTES.contains(&self.shell.status_msg.as_str()) => {
                self.shell.status_msg.clear();
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::Canvas;

    fn event(x: f32, y: f32, mode: SelectionMode) -> PointerEvent {
        PointerEvent {
            selection_mode: mode,
            ..PointerEvent::new(x, y)
        }
    }

    #[test]
    fn the_cursor_shows_a_drag_only_where_a_press_would_drag() {
        let mut sel = Selection::new(60, 60);
        sel.select_rect(10, 10, 40, 40);
        let inside = |mode| selection_cursor_for(&event(20.0, 20.0, mode), &sel, false);
        assert_eq!(inside(SelectionMode::New), SelectionCursor::Move);
        for mode in [
            SelectionMode::Add,
            SelectionMode::Subtract,
            SelectionMode::Intersect,
        ] {
            assert_eq!(inside(mode), SelectionCursor::Draw(mode));
        }
        assert_eq!(
            selection_cursor_for(&event(50.0, 50.0, SelectionMode::New), &sel, false),
            SelectionCursor::Draw(SelectionMode::New)
        );
        // A polygon lasso with corners down adds a corner wherever it is pressed.
        assert_eq!(
            selection_cursor_for(&event(20.0, 20.0, SelectionMode::New), &sel, true),
            SelectionCursor::Draw(SelectionMode::New)
        );
    }

    #[test]
    fn each_mode_badges_the_marquee_differently_and_new_not_at_all() {
        let bitmaps: Vec<Vec<u8>> = [
            SelectionMode::New,
            SelectionMode::Add,
            SelectionMode::Subtract,
            SelectionMode::Intersect,
        ]
        .into_iter()
        .map(|mode| {
            let (rgba, w, h, hx, hy) = marquee_cursor_rgba(mode);
            assert_eq!((w, h, hx, hy), (21, 21, 9, 9));
            rgba
        })
        .collect();
        // The crosshair's centre stays clear; New has nothing in the badge corner.
        let alpha = |rgba: &[u8], x: usize, y: usize| rgba[(y * 21 + x) * 4 + 3];
        assert_eq!(alpha(&bitmaps[0], 9, 9), 0);
        assert!((13..20).all(|y| (13..20).all(|x| alpha(&bitmaps[0], x, y) == 0)));
        assert_eq!(alpha(&bitmaps[1], 16, 13), 255, "+ has its upright");
        assert_eq!(alpha(&bitmaps[2], 16, 13), 0, "− has none");
        assert_eq!(alpha(&bitmaps[3], 13, 13), 255, "× has its corners");
        for i in 0..4 {
            for j in i + 1..4 {
                assert_ne!(bitmaps[i], bitmaps[j]);
            }
        }
    }

    #[test]
    fn a_working_drag_takes_the_last_nothing_happened_note_down() {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(100, 100);
        app.edit.tools.select(ToolId::SelectionRect);
        let fg = app.edit.fg_color;
        let bg = app.edit.bg_color;
        let gesture = |app: &mut App, mode| {
            let mut ctx =
                crate::tools::ToolCtx::new(&mut app.docs.documents[0], fg, bg, 1.0, 0.0, 0.0);
            app.edit.tools.on_press(event(10.0, 10.0, mode), &mut ctx);
            app.edit.tools.on_drag(event(40.0, 40.0, mode), &mut ctx);
            app.edit.tools.on_release(event(40.0, 40.0, mode), &mut ctx);
        };
        gesture(&mut app, SelectionMode::Subtract);
        app.report_selection_outcome();
        assert!(
            app.shell.status_msg.contains("Trừ"),
            "{}",
            app.shell.status_msg
        );
        gesture(&mut app, SelectionMode::New);
        app.report_selection_outcome();
        assert_eq!(app.shell.status_msg, "");

        // Someone else's message stays.
        app.shell.status_msg = "Đã lưu".to_string();
        gesture(&mut app, SelectionMode::New);
        app.report_selection_outcome();
        assert_eq!(app.shell.status_msg, "Đã lưu");
    }
}

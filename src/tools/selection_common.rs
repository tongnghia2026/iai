// What the marquee and lasso tools share: when a press drags the selection
// instead of drawing, and what a finished gesture did (status bar + journal).

use super::PointerEvent;
use crate::core::canvas::Canvas;
use crate::core::selection::{Selection, SelectionMode, SelectionSnapshot};

/// Photoshop's rule for every marquee and lasso: only a press in New mode
/// (no Shift / Alt) inside the selection drags it. Add / Subtract /
/// Intersect, from the top bar or from Shift / Alt, draw a shape there.
pub fn press_moves_selection(event: &PointerEvent, selection: &Selection) -> bool {
    event.selection_mode == SelectionMode::New
        && selection.active
        && event.canvas_x >= 0.0
        && event.canvas_y >= 0.0
        && selection.is_selected(event.canvas_x as u32, event.canvas_y as u32)
}

/// How one press-drag-release of a selection tool ended.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SelectionOutcome {
    /// The press landed inside the selection and dragged it.
    Moved { dx: i32, dy: i32 },
    /// A press without a shape: New mode drops the selection, the rest keep it.
    Click { mode: SelectionMode },
    Drawn {
        mode: SelectionMode,
        /// The mode came from Shift / Alt held down, not from the top bar.
        by_keys: bool,
        changed: bool,
        /// The selection afterwards: x0, y0, x1, y1 (`None` when empty).
        result: Option<[i32; 4]>,
    },
}

/// Selection bounds after a gesture, `None` when nothing is selected.
fn result_bounds(canvas: &mut Canvas) -> Option<[i32; 4]> {
    if !canvas.selection.active {
        return None;
    }
    let (x0, y0, x1, y1) = canvas.selection.bounding_box();
    Some([x0 as i32, y0 as i32, x1 as i32, y1 as i32])
}

/// What a shape just combined into `canvas.selection` did, against the
/// selection the gesture started from.
pub fn drawn_outcome(
    canvas: &mut Canvas,
    before: Option<&SelectionSnapshot>,
    mode: SelectionMode,
    by_keys: bool,
) -> SelectionOutcome {
    let changed = before.is_none_or(|snap| !snap.selects_same_as(&canvas.selection));
    SelectionOutcome::Drawn {
        mode,
        by_keys,
        changed,
        result: result_bounds(canvas),
    }
}

/// Whether Shift / Alt picked `mode` rather than the top bar.
pub fn mode_from_keys(event: &PointerEvent) -> bool {
    event.shift || event.alt
}

fn mode_name(mode: SelectionMode) -> &'static str {
    match mode {
        SelectionMode::New => "New",
        SelectionMode::Add => "Add",
        SelectionMode::Subtract => "Subtract",
        SelectionMode::Intersect => "Intersect",
    }
}

impl SelectionOutcome {
    /// One journal line: what the gesture did to the selection.
    pub fn journal_text(&self) -> String {
        match *self {
            Self::Moved { dx, dy } => format!("moved the selection by ({dx}, {dy})"),
            Self::Click { mode } => format!("click without a shape ({})", mode_name(mode)),
            Self::Drawn {
                mode,
                by_keys,
                changed,
                result,
            } => {
                let keys = if by_keys { " by Shift/Alt" } else { "" };
                let what = if changed { "changed" } else { "UNCHANGED" };
                let now = match result {
                    Some([x0, y0, x1, y1]) => {
                        format!("{}x{} at ({x0}, {y0})", x1 - x0, y1 - y0)
                    }
                    None => "nothing selected".to_string(),
                };
                format!("drew a shape ({}{keys}): {what}, {now}", mode_name(mode))
            }
        }
    }

    /// The status-bar note for a drag that came to nothing, so a marquee
    /// that "doesn't work" says why. `None` when the drag did what it looks
    /// like it did.
    pub fn nothing_happened_note(&self) -> Option<&'static str> {
        let Self::Drawn {
            mode,
            by_keys,
            changed,
            result,
        } = *self
        else {
            return None;
        };
        let [add_keys, add_bar, sub_keys, sub_bar, int_keys, int_bar, int_covers] =
            NOTHING_HAPPENED_NOTES;
        Some(match mode {
            SelectionMode::Add if !changed => {
                if by_keys {
                    add_keys
                } else {
                    add_bar
                }
            }
            SelectionMode::Subtract if !changed => {
                if by_keys {
                    sub_keys
                } else {
                    sub_bar
                }
            }
            SelectionMode::Intersect if result.is_none() => {
                if by_keys {
                    int_keys
                } else {
                    int_bar
                }
            }
            SelectionMode::Intersect if !changed => int_covers,
            _ => return None,
        })
    }
}

/// Every note [`SelectionOutcome::nothing_happened_note`] can show, so the
/// next drag that works can take its note off the status bar again.
pub const NOTHING_HAPPENED_NOTES: [&str; 7] = [
    "Đang giữ Shift (cộng thêm) mà chỗ vừa kéo đã nằm trong vùng chọn nên không đổi gì. Bấm Ctrl+D để bỏ chọn rồi kéo lại.",
    "Chế độ Cộng (nút + trên thanh trên): chỗ vừa kéo đã nằm trong vùng chọn nên không đổi gì. Bấm Ctrl+D để bỏ chọn, hoặc bấm nút New để vẽ vùng chọn mới.",
    "Đang giữ Alt nên vùng kéo bị TRỪ khỏi vùng chọn, mà chỗ đó không có gì để trừ. Nhả Alt rồi kéo lại.",
    "Đang ở chế độ Trừ (nút − trên thanh trên): vùng vừa kéo không trừ được gì. Bấm nút New để vẽ vùng chọn mới.",
    "Đang giữ Shift+Alt (chế độ Giao): vùng vừa kéo không chạm vùng chọn cũ nên không còn gì được chọn. Nhả phím rồi kéo lại.",
    "Đang ở chế độ Giao (nút ∩ trên thanh trên): vùng vừa kéo không chạm vùng chọn cũ nên không còn gì được chọn. Bấm nút New để vẽ vùng chọn mới.",
    "Chế độ Giao: vùng vừa kéo bao trọn vùng chọn cũ nên không đổi gì. Bấm nút New để vẽ vùng chọn mới.",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn event(x: f32, y: f32, mode: SelectionMode) -> PointerEvent {
        PointerEvent {
            selection_mode: mode,
            ..PointerEvent::new(x, y)
        }
    }

    #[test]
    fn only_a_new_mode_press_inside_the_selection_moves_it() {
        let mut sel = Selection::new(50, 50);
        sel.select_rect(10, 10, 40, 40);
        assert!(press_moves_selection(
            &event(20.0, 20.0, SelectionMode::New),
            &sel
        ));
        for mode in [
            SelectionMode::Add,
            SelectionMode::Subtract,
            SelectionMode::Intersect,
        ] {
            assert!(
                !press_moves_selection(&event(20.0, 20.0, mode), &sel),
                "{mode:?} draws inside the selection"
            );
        }
        assert!(!press_moves_selection(
            &event(5.0, 5.0, SelectionMode::New),
            &sel
        ));
        // Off the canvas to the left / top is never "inside" pixel 0.
        sel.select_all();
        assert!(!press_moves_selection(
            &event(-3.0, 4.0, SelectionMode::New),
            &sel
        ));
    }

    #[test]
    fn a_drag_that_came_to_nothing_says_why_in_the_status_bar() {
        let drawn = |mode, by_keys, changed, result| SelectionOutcome::Drawn {
            mode,
            by_keys,
            changed,
            result,
        };
        let some = Some([0, 0, 10, 10]);
        // Subtract with nothing to subtract (Alt held, or the − button).
        let note = drawn(SelectionMode::Subtract, true, false, None)
            .nothing_happened_note()
            .expect("note");
        assert!(note.contains("Alt"), "{note}");
        let note = drawn(SelectionMode::Subtract, false, false, some)
            .nothing_happened_note()
            .expect("note");
        assert!(note.contains("Trừ") && note.contains("New"), "{note}");
        // Intersect that left nothing.
        let note = drawn(SelectionMode::Intersect, false, true, None)
            .nothing_happened_note()
            .expect("note");
        assert!(note.contains("Giao"), "{note}");
        // Add inside a selection that already covers the shape (after Ctrl+A).
        let note = drawn(SelectionMode::Add, false, false, some)
            .nothing_happened_note()
            .expect("note");
        assert!(note.contains("Ctrl+D"), "{note}");
        // Drags that did something stay quiet.
        assert_eq!(
            drawn(SelectionMode::Add, false, true, some).nothing_happened_note(),
            None
        );
        assert_eq!(
            drawn(SelectionMode::Subtract, false, true, some).nothing_happened_note(),
            None
        );
        assert_eq!(
            drawn(SelectionMode::New, false, false, some).nothing_happened_note(),
            None
        );
        assert_eq!(
            SelectionOutcome::Moved { dx: 3, dy: 0 }.nothing_happened_note(),
            None
        );
    }

    #[test]
    fn the_journal_line_says_what_the_selection_became() {
        let text = SelectionOutcome::Drawn {
            mode: SelectionMode::Add,
            by_keys: false,
            changed: false,
            result: Some([0, 0, 2480, 3508]),
        }
        .journal_text();
        assert_eq!(text, "drew a shape (Add): UNCHANGED, 2480x3508 at (0, 0)");
        assert_eq!(
            SelectionOutcome::Moved { dx: -4, dy: 7 }.journal_text(),
            "moved the selection by (-4, 7)"
        );
    }
}

//! What the flight recorder (`diag`) is told about the window and its
//! widgets. The winit and egui types are read here, so `diag` needs neither.

use std::fmt::Write as _;

use crate::diag::{self, Phase};

/// The phase a window event is handled under.
pub fn phase_of(event: &winit::event::WindowEvent, develop_window: bool) -> Phase {
    use winit::event::WindowEvent as E;
    match (event, develop_window) {
        (E::RedrawRequested, false) => Phase::Redraw,
        (E::RedrawRequested, true) => Phase::DevelopRedraw,
        (_, true) => Phase::DevelopInput,
        (E::KeyboardInput { .. }, _) => Phase::Key,
        (E::MouseInput { .. }, _) => Phase::Mouse,
        (E::CursorMoved { .. }, _) => Phase::Move,
        (E::MouseWheel { .. }, _) => Phase::Wheel,
        _ => Phase::Window,
    }
}

/// What egui says the user just did to a widget: clicks, toggles, value edits.
pub fn widget_event(event: &egui::output::OutputEvent) {
    if !diag::recording() {
        return;
    }
    if let Some((changing, what, value)) = widget_line(event) {
        diag::widget(changing, &what, &value);
    }
}

/// A widget event as journal text: whether it is one of a run of value
/// changes, what was touched, and the value it now has.
fn widget_line(event: &egui::output::OutputEvent) -> Option<(bool, String, String)> {
    use egui::output::OutputEvent as E;
    let (verb, info) = match event {
        E::Clicked(info) => ("click ", info),
        E::DoubleClicked(info) => ("double-click ", info),
        E::TripleClicked(info) => ("triple-click ", info),
        E::FocusGained(info) => ("focus ", info),
        E::ValueChanged(info) => ("", info),
        E::TextSelectionChanged(_) => return None,
    };
    let mut what = format!("{verb}{:?}", info.typ);
    if let Some(label) = info.label.as_deref().filter(|l| !l.is_empty()) {
        let _ = write!(what, " \"{}\"", diag::readable(label));
    }
    let mut value = String::new();
    if let Some(v) = info.value {
        let _ = write!(value, "= {}", (v * 1000.0).round() / 1000.0);
    }
    if let Some(on) = info.selected {
        value.push_str(if on { "[on]" } else { "[off]" });
    }
    // The text itself stays out of the log; its length says enough.
    if let Some(text) = &info.current_text_value {
        let _ = write!(value, "({} chars)", text.chars().count());
    }
    Some((matches!(event, E::ValueChanged(_)), what, value))
}

/// A press this far (in points) from a piece of text is still told with it.
const NEAR_TEXT: f32 = 60.0;

/// The pointer went down over the UI at `at`: name the spot by the text drawn
/// there. It is the only name a control painted by hand has — egui reports
/// nothing for those (see [`widget_event`]).
pub fn pressed_text(shapes: &[egui::epaint::ClippedShape], at: egui::Pos2) {
    if !diag::recording() {
        return;
    }
    if let Some(line) = text_under(shapes, at) {
        diag::note("ui", &line);
    }
}

/// The text at `at`, or failing that the nearest within [`NEAR_TEXT`]. Shapes
/// come in paint order, so of several at the same distance the last — the one
/// drawn on top — is the one the user saw.
fn text_under(shapes: &[egui::epaint::ClippedShape], at: egui::Pos2) -> Option<String> {
    fn visit<'a>(
        shape: &'a egui::Shape,
        clip: egui::Rect,
        at: egui::Pos2,
        best: &mut Option<(f32, &'a str)>,
    ) {
        match shape {
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    visit(shape, clip, at, best);
                }
            }
            egui::Shape::Text(text) => {
                let rect = text.visual_bounding_rect().intersect(clip);
                let words = text.galley.text().trim();
                if !rect.is_positive() || words.is_empty() {
                    return;
                }
                let distance = rect.distance_to_pos(at);
                if distance <= NEAR_TEXT && best.is_none_or(|(known, _)| distance <= known) {
                    *best = Some((distance, words));
                }
            }
            _ => {}
        }
    }
    let mut best = None;
    for clipped in shapes {
        visit(&clipped.shape, clipped.clip_rect, at, &mut best);
    }
    let (distance, words) = best?;
    let mut words = diag::readable(words);
    if let Some((cut, _)) = words.char_indices().nth(48) {
        words.truncate(cut);
        words.push('\u{2026}');
    }
    Some(format!(
        "press {} \"{words}\"",
        if distance == 0.0 { "on" } else { "near" }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Click at `at` in a small UI and return what the journal would be told.
    fn lines_for_a_click(
        at: egui::Pos2,
        mut ui: impl FnMut(&mut egui::Ui),
    ) -> Vec<(bool, String, String)> {
        let ctx = egui::Context::default();
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let mut lines = Vec::new();
        for events in [
            vec![],
            vec![egui::Event::PointerMoved(at)],
            vec![button(true)],
            vec![button(false)],
            vec![],
        ] {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 300.0),
                )),
                events,
                ..Default::default()
            };
            #[allow(deprecated)]
            let output = ctx.run_ui(input, |root| ui(root));
            lines.extend(output.platform_output.events.iter().filter_map(widget_line));
        }
        lines
    }

    #[test]
    fn a_clicked_button_is_journalled_by_its_label() {
        let mut rect = egui::Rect::NOTHING;
        // A first pass only to learn where the button lands.
        lines_for_a_click(egui::pos2(-10.0, -10.0), |ui| {
            rect = ui.button("Làm ảnh thẻ tự động").rect;
        });
        let lines = lines_for_a_click(rect.center(), |ui| {
            let _ = ui.button("Làm ảnh thẻ tự động");
        });
        assert_eq!(
            lines,
            vec![(
                false,
                "click Button \"Làm ảnh thẻ tự động\"".to_owned(),
                String::new()
            )]
        );
    }

    #[test]
    fn a_ticked_checkbox_is_journalled_with_its_new_state() {
        let mut rect = egui::Rect::NOTHING;
        let mut snap = false;
        lines_for_a_click(egui::pos2(-10.0, -10.0), |ui| {
            rect = ui.checkbox(&mut snap, "Snap").rect;
        });
        let lines = lines_for_a_click(rect.center(), |ui| {
            let _ = ui.checkbox(&mut snap, "Snap");
        });
        assert!(snap);
        assert_eq!(
            lines,
            vec![(
                false,
                "click Checkbox \"Snap\"".to_owned(),
                "[on]".to_owned()
            )]
        );
    }

    #[test]
    fn a_press_is_named_by_the_text_drawn_under_it() {
        let ctx = egui::Context::default();
        let mut blue = egui::Rect::NOTHING;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 300.0),
            )),
            ..Default::default()
        };
        #[allow(deprecated)]
        let output = ctx.run_ui(input, |ui| {
            ui.label("Nền trắng");
            ui.add_space(120.0);
            blue = ui.label("Nền xanh").rect;
        });
        assert_eq!(
            text_under(&output.shapes, blue.center()).as_deref(),
            Some("press on \"Nền xanh\"")
        );
        // Just off the text is still that text's control; far from any, nothing.
        assert_eq!(
            text_under(&output.shapes, blue.right_center() + egui::vec2(20.0, 0.0)).as_deref(),
            Some("press near \"Nền xanh\"")
        );
        assert_eq!(text_under(&output.shapes, egui::pos2(390.0, 290.0)), None);
    }
}

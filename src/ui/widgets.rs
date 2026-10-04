//! Shared custom widgets. The Camera-Raw slider (a gradient track + a triangular
//! thumb + a numeric readout) lives here so the adjustment dialogs can reuse the
//! exact same look instead of egui's default slider.

use egui::Color32;

/// Move keyboard focus to a `DragValue` field AND select all of its text, so the
/// next keystroke replaces the current number instead of appending to it. Used by
/// custom Tab handlers (Crop options, New Canvas) that jump between numeric fields.
///
/// egui only auto-selects a `DragValue` on `gained_focus`, which does NOT fire
/// when focus is moved mid-frame with `request_focus()` (by then the widget was
/// already "focused" earlier this pass). So we replicate egui's own click-path
/// behaviour: request focus and stamp a select-all cursor range into the field's
/// `TextEdit` state. The end index is deliberately large; egui clamps the cursor
/// to the text, so it reliably selects the whole field whatever its length.
pub(crate) fn focus_field_select_all(ui: &egui::Ui, response: &egui::Response) {
    response.request_focus();
    let id = response.id;
    let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(0),
            egui::text::CCursor::new(64),
        )));
    state.store(ui.ctx(), id);
}

/// Whether a text field had the keyboard as this frame or the last began:
/// then Esc and Enter are that field's, not the dialog's around it. egui
/// drops the focus on Esc before the frame's UI runs, so the frame of the
/// Esc itself no longer shows it; the last frame's answer is kept under `id`.
pub(crate) fn typing_in_a_field(ctx: &egui::Context, id: egui::Id) -> bool {
    let now = ctx.egui_wants_keyboard_input();
    let before = ctx.data_mut(|d| {
        let before = d.get_temp::<bool>(id).unwrap_or(false);
        d.insert_temp(id, now);
        before
    });
    now || before
}

const PLAIN_PRESS: &str = "plain_press_pass";

/// Note that the press held this frame is on a control through which no
/// drag changes a value: a slider's value box, a group's header.
fn note_plain_press(ctx: &egui::Context) {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(PLAIN_PRESS), pass));
}

/// Whether the press held this frame is on such a control (call after the
/// controls are drawn). Develop leaves its preview alone for one: switching
/// to the drag preview and back, with nothing dragged, only blinks the image.
pub(crate) fn plain_press(ctx: &egui::Context) -> bool {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.get_temp::<u64>(egui::Id::new(PLAIN_PRESS))) == Some(pass)
}

/// Linear interpolate two colours (premultiplied-agnostic), `t` in 0..1.
pub(crate) fn mix_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    Color32::from_rgba_unmultiplied(
        (a.r() as f32 + (b.r() as f32 - a.r() as f32) * t).round() as u8,
        (a.g() as f32 + (b.g() as f32 - a.g() as f32) * t).round() as u8,
        (a.b() as f32 + (b.b() as f32 - a.b() as f32) * t).round() as u8,
        (a.a() as f32 + (b.a() as f32 - a.a() as f32) * t).round() as u8,
    )
}

/// Sample a multi-stop gradient at `t` in 0..1.
pub(crate) fn sample_gradient(colors: &[Color32], t: f32) -> Color32 {
    if colors.is_empty() {
        return Color32::from_gray(128);
    }
    if colors.len() == 1 {
        return colors[0];
    }
    let scaled = t.clamp(0.0, 1.0) * (colors.len() - 1) as f32;
    let i = scaled.floor() as usize;
    let f = scaled - i as f32;
    let a = colors[i.min(colors.len() - 1)];
    let b = colors[(i + 1).min(colors.len() - 1)];
    mix_color(a, b, f)
}

const LABEL_W: f32 = 84.0;
const VALUE_W: f32 = 42.0;
/// Height of a stacked slider row (label over the track, value box right).
pub const STACKED_ROW_H: f32 = 33.0;

/// What a row or a header gains under the pointer.
fn row_glow() -> Color32 {
    Color32::from_white_alpha(22)
}

/// A header's icon: a Phosphor glyph, or one drawn here in the same line
/// style where Phosphor has none.
#[derive(Clone, Copy)]
pub enum HeaderIcon<'a> {
    Glyph(&'a str),
    Lips,
}

impl<'a> From<&'a str> for HeaderIcon<'a> {
    fn from(glyph: &'a str) -> Self {
        HeaderIcon::Glyph(glyph)
    }
}

/// A closed mouth, 16 px wide around `centre`: the upper lip's bow, the lower
/// lip, and the line between them.
fn paint_lips(painter: &egui::Painter, centre: egui::Pos2, colour: Color32) {
    let stroke = egui::Stroke::new(1.3_f32, colour);
    let at = |x: f32, y: f32| centre + egui::vec2(x, y);
    let curve = |from: egui::Pos2, pull: egui::Pos2, to: egui::Pos2| {
        painter.add(egui::epaint::QuadraticBezierShape::from_points_stroke(
            [from, pull, to],
            false,
            Color32::TRANSPARENT,
            stroke,
        ));
    };
    let (left, right) = (at(-7.5, 0.0), at(7.5, 0.0));
    let dip = at(0.0, -2.6);
    curve(left, at(-3.6, -6.4), dip);
    curve(dip, at(3.6, -6.4), right);
    curve(left, at(0.0, 10.4), right);
    curve(left, at(0.0, 2.2), right);
}

/// The header bar of a collapsible group, shared by Chỉnh chân dung and
/// Develop: icon, bold title, a dot while something in the group is at work,
/// and the caret. It lights up under the pointer; a click toggles the group.
pub fn section_header<'a>(
    ui: &mut egui::Ui,
    icon: impl Into<HeaderIcon<'a>>,
    title: &str,
    open: bool,
    active: bool,
) -> egui::Response {
    use egui_phosphor::regular as ph;
    // See `stacked_slider` for why the visible width bounds the row.
    let visible_w = (ui.clip_rect().right() - ui.cursor().left()).max(1.0);
    let width = ui.available_width().min(visible_w).max(1.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 30.0), egui::Sense::click());
    if response.is_pointer_button_down_on() {
        note_plain_press(ui.ctx());
    }
    let visuals = ui.visuals();
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, visuals.faint_bg_color);
    if response.hovered() {
        painter.rect_filled(rect, 4.0, row_glow());
    }
    let bright = visuals.strong_text_color();
    match icon.into() {
        HeaderIcon::Glyph(glyph) => {
            painter.text(
                rect.left_center() + egui::vec2(10.0, 0.0),
                egui::Align2::LEFT_CENTER,
                glyph,
                egui::FontId::proportional(16.0),
                bright,
            );
        }
        HeaderIcon::Lips => paint_lips(painter, rect.left_center() + egui::vec2(18.0, 0.0), bright),
    }
    painter.text(
        rect.left_center() + egui::vec2(34.0, 0.0),
        egui::Align2::LEFT_CENTER,
        title,
        super::theme::bold_font(ui.ctx(), 14.0),
        bright,
    );
    let caret = if open {
        ph::CARET_DOWN
    } else {
        ph::CARET_RIGHT
    };
    painter.text(
        rect.right_center() - egui::vec2(10.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        caret,
        egui::FontId::proportional(12.0),
        visuals.weak_text_color(),
    );
    if active {
        painter.circle_filled(
            rect.right_center() - egui::vec2(34.0, 0.0),
            3.5,
            Color32::from_rgb(90, 170, 255),
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Track-position easing exponent for the Exposure slider. `> 1` expands the
/// near-neutral part of the track so small drags there change exposure gently
/// (an ART/PTS-like light touch), while the track ends still reach the full
/// range. Interaction is unchanged (thumb follows the cursor); only the
/// position↔value arithmetic is eased. `1.0` everywhere else (linear).
pub(crate) const EXPOSURE_POS_POWER: f32 = 2.0;

/// Map a normalized track position `t` (0..1) to a slider value, optionally with
/// a symmetric centre-fine ease (`power > 1`). Exact linear map when `power == 1`.
fn pos_to_value(t: f32, min: f32, max: f32, power: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if (power - 1.0).abs() < 1e-6 {
        return min + (max - min) * t;
    }
    let mid = (min + max) * 0.5;
    let half = (max - min) * 0.5;
    let c = 2.0 * t - 1.0;
    mid + c.signum() * c.abs().powf(power) * half
}

/// Inverse of [`pos_to_value`]: slider value → normalized track position, so the
/// thumb sits under the cursor and typed values land at the right spot.
fn value_to_pos(value: f32, min: f32, max: f32, power: f32) -> f32 {
    if (power - 1.0).abs() < 1e-6 {
        return ((value - min) / (max - min)).clamp(0.0, 1.0);
    }
    let mid = (min + max) * 0.5;
    let half = (max - min) * 0.5;
    let e = ((value - mid) / half).clamp(-1.0, 1.0);
    let c = e.signum() * e.abs().powf(1.0 / power);
    ((c + 1.0) * 0.5).clamp(0.0, 1.0)
}

#[cfg(test)]
mod slider_ease_tests {
    use super::{pos_to_value, value_to_pos, EXPOSURE_POS_POWER};

    #[test]
    fn pos_value_roundtrip_and_anchors() {
        let (min, max, p) = (-50.0f32, 50.0f32, EXPOSURE_POS_POWER);
        // Anchors: both ends and the centre map exactly, either power.
        for power in [1.0, p] {
            assert!((pos_to_value(0.0, min, max, power) - min).abs() < 1e-4);
            assert!((pos_to_value(1.0, min, max, power) - max).abs() < 1e-4);
            assert!((pos_to_value(0.5, min, max, power) - 0.0).abs() < 1e-4);
        }
        // Round-trip position → value → position is stable.
        for i in 0..=20 {
            let t = i as f32 / 20.0;
            let v = pos_to_value(t, min, max, p);
            let back = value_to_pos(v, min, max, p);
            assert!((back - t).abs() < 1e-3, "roundtrip {t} -> {v} -> {back}");
        }
    }

    #[test]
    fn centre_fine_ease_is_gentler_near_neutral() {
        let (min, max, p) = (-50.0f32, 50.0f32, EXPOSURE_POS_POWER);
        // Just off centre, the eased value is smaller in magnitude than linear:
        // more track is devoted to small exposures → finer control there.
        let t = 0.62;
        assert!(pos_to_value(t, min, max, p).abs() < pos_to_value(t, min, max, 1.0).abs());
        // Monotonic across the track.
        let mut prev = f32::NEG_INFINITY;
        for i in 0..=40 {
            let v = pos_to_value(i as f32 / 40.0, min, max, p);
            assert!(v >= prev - 1e-4, "must stay monotonic");
            prev = v;
        }
    }
}

/// Camera-Raw-style slider returning the track `Response` (so callers can debounce
/// on `drag_stopped()` like the filter dialogs). `label` (fixed-width) · gradient
/// track that FILLS the remaining row width · triangle thumb · numeric value. The
/// adaptive track width lets the same widget fit narrow popups and wide panels.
pub fn dev_slider_resp(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    colors: &[Color32],
) -> egui::Response {
    let min = *range.start();
    let max = *range.end();
    let mut changed = false;
    let mut response = ui
        .horizontal(|ui| {
            ui.add_sized([LABEL_W, 18.0], egui::Label::new(label));
            let track_w = (ui.available_width() - VALUE_W - ui.spacing().item_spacing.x)
                .clamp(44.0, ui.spacing().slider_width);
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(track_w, 18.0), egui::Sense::click_and_drag());
            if (response.dragged() || response.clicked())
                && response.interact_pointer_pos().is_some()
            {
                let pos = response.interact_pointer_pos().unwrap();
                let t = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                let new_value = min + (max - min) * t;
                if (*value - new_value).abs() > f32::EPSILON {
                    *value = new_value;
                    changed = true;
                }
            }
            paint_gradient_slider(
                ui,
                rect,
                value_to_pos(*value, min, max, 1.0),
                min,
                max,
                colors,
            );
            // Small ranges (e.g. a 0..5 filter amount) need a decimal; wide ranges
            // (−100..100, 0..255) read better as integers.
            let text = if (max - min) <= 12.0 {
                format!("{:.1}", *value)
            } else {
                format!("{:.0}", *value)
            };
            ui.add_sized([VALUE_W, 18.0], egui::Label::new(text).truncate());
            response
        })
        .inner;
    if changed {
        response.mark_changed();
    }
    response
}

/// As [`dev_slider_resp`] but returns whether the value changed.
#[allow(dead_code)]
pub fn dev_slider_colored(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    colors: &[Color32],
) -> bool {
    dev_slider_resp(ui, label, value, range, colors).changed()
}

/// Neutral (dark → mid → light) track for generic adjustments without a meaningful
/// colour gradient.
/// Compact Develop row: the label is painted above the track area, so long
/// labels do not steal horizontal space from the slider.
pub fn dev_slider_colored_stacked(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    colors: &[Color32],
    pos_power: f32,
) -> bool {
    dev_slider_stacked_resp(ui, label, value, range, colors, pos_power).changed()
}

pub fn dev_slider_stacked_resp(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    colors: &[Color32],
    // Track-position ease exponent (1.0 = linear; > 1 = centre-fine).
    pos_power: f32,
) -> egui::Response {
    let (min, max) = (*range.start(), *range.end());
    stacked_slider(
        ui,
        label,
        value,
        range,
        colors,
        |t| pos_to_value(t, min, max, pos_power),
        |v| value_to_pos(v, min, max, pos_power),
    )
}

/// [`dev_slider_stacked_resp`] on a logarithmic track (`min` must be > 0), for
/// sizes spanning several decades such as a brush diameter.
pub fn dev_slider_stacked_log_resp(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    colors: &[Color32],
) -> egui::Response {
    let min = range.start().max(1e-3);
    let span = (range.end().max(min * 1.001) / min).ln();
    stacked_slider(
        ui,
        label,
        value,
        range,
        colors,
        |t| min * (span * t.clamp(0.0, 1.0)).exp(),
        |v| ((v.max(min) / min).ln() / span).clamp(0.0, 1.0),
    )
}

fn stacked_slider(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    colors: &[Color32],
    to_value: impl Fn(f32) -> f32,
    to_pos: impl Fn(f32) -> f32,
) -> egui::Response {
    let min = *range.start();
    let max = *range.end();
    // A vertical ScrollArea may retain a wider virtual content width after a
    // child (curve editor, combo, etc.) requested it. Limit rows to the part
    // that is actually visible, otherwise the numeric box and the right end
    // of the track are laid out beyond the Develop side panel and clipped.
    let visible_w = (ui.clip_rect().right() - ui.cursor().left()).max(1.0);
    let row_w = ui.available_width().min(visible_w).max(1.0);
    let (rect, mut response) = ui.allocate_exact_size(
        egui::vec2(row_w, STACKED_ROW_H),
        egui::Sense::click_and_drag(),
    );
    // Each slider is a row of its own: a hairline under it, and it lights up
    // under the pointer.
    let lit = ui.is_enabled() && (response.dragged() || ui.rect_contains_pointer(rect));
    if lit {
        ui.painter().rect_filled(rect, 3.0, row_glow());
    }
    let gap = ui.spacing().item_spacing.y;
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() + gap * 0.5,
        egui::Stroke::new(1.0_f32, Color32::from_white_alpha(10)),
    );

    let precise = (max - min) <= 12.0;
    let value_text = if precise {
        format!("{:.1}", *value)
    } else {
        format!("{:.0}", *value)
    };
    // Clear of the scroll bar that floats over the right edge.
    let value_rect = egui::Rect::from_min_size(
        egui::pos2(rect.right() - VALUE_W - 8.0, rect.top() + 13.0),
        egui::vec2(VALUE_W, 18.0),
    );
    let track_left = (rect.left() + 92.0).min(value_rect.left() - 64.0);
    let track_rect = egui::Rect::from_min_max(
        egui::pos2(track_left, rect.top() + 15.0),
        egui::pos2(value_rect.left() - 8.0, rect.top() + 33.0),
    );

    // Direct numeric entry: clicking the value box swaps it for a text field.
    // Enter or clicking away commits (clamped to the range), Esc discards.
    let edit_id = response.id.with("value_edit");
    let te_id = edit_id.with("te");
    let focus_id = edit_id.with("focus");
    let editing = ui
        .ctx()
        .data_mut(|d| d.get_temp::<String>(edit_id))
        .is_some();
    // Keyed off the press ORIGIN so a drag that started on the track keeps
    // driving the slider past the box, and one that started in the box never
    // yanks the slider to its maximum. egui forgets the origin as the button
    // comes up, which is the frame of the click: there the click's own
    // position stands in.
    let pressed_in_value_box = ui
        .input(|i| i.pointer.press_origin())
        .or_else(|| response.interact_pointer_pos())
        .is_some_and(|p| value_rect.contains(p));
    if pressed_in_value_box {
        note_plain_press(ui.ctx());
    }

    if !editing && (response.dragged() || response.clicked()) {
        if response.clicked() && pressed_in_value_box {
            ui.ctx().data_mut(|d| {
                d.insert_temp(edit_id, value_text.clone());
                d.insert_temp(focus_id, true);
            });
        } else if !pressed_in_value_box {
            if let Some(pos) = response.interact_pointer_pos() {
                let t = ((pos.x - track_rect.left()) / track_rect.width()).clamp(0.0, 1.0);
                let new_value = to_value(t);
                if (*value - new_value).abs() > f32::EPSILON {
                    *value = new_value;
                    response.mark_changed();
                }
            }
        }
    }

    paint_gradient_slider(ui, track_rect, to_pos(*value), min, max, colors);
    let font = egui::FontId::proportional(11.5);
    let color = ui.visuals().text_color();
    ui.painter().text(
        egui::pos2(rect.left() + 8.0, rect.top() + 2.0),
        egui::Align2::LEFT_TOP,
        label,
        font.clone(),
        color,
    );
    // The value sits in a box: it is a field, a click types into it.
    let over_value = lit && ui.rect_contains_pointer(value_rect);
    if over_value {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
    }
    let outline = if over_value {
        ui.visuals().selection.stroke.color
    } else {
        Color32::from_white_alpha(26)
    };
    ui.painter().rect(
        value_rect,
        3.0,
        ui.visuals().extreme_bg_color,
        egui::Stroke::new(1.0_f32, outline),
        egui::StrokeKind::Inside,
    );
    if let Some(mut text) = ui.ctx().data_mut(|d| d.get_temp::<String>(edit_id)) {
        // In a child of its own: `ui.put` would move this row's cursor up to
        // the field's bottom edge, and every row below would jump with it.
        let mut field = ui.new_child(egui::UiBuilder::new().max_rect(value_rect).layout(
            egui::Layout::centered_and_justified(egui::Direction::TopDown),
        ));
        let out = field.add(
            egui::TextEdit::singleline(&mut text)
                .id(te_id)
                .font(font)
                .margin(egui::vec2(2.0, 1.0)),
        );
        // The field takes the focus once it exists, the number selected so
        // typing replaces it. Asked for before the field is made, on the
        // frame of the click, the focus is given up at once: egui has a
        // focused widget the click did not land on let go, and this field
        // was not there yet when the click was aimed.
        let opening = ui
            .ctx()
            .data_mut(|d| d.remove_temp::<bool>(focus_id))
            .unwrap_or(false);
        if opening {
            focus_field_select_all(ui, &out);
        }
        if out.changed() {
            ui.ctx().data_mut(|d| d.insert_temp(edit_id, text.clone()));
        }
        if out.lost_focus() || !(opening || out.has_focus()) {
            ui.ctx().data_mut(|d| d.remove::<String>(edit_id));
            // Esc = discard; any other way out (Enter, click away) commits.
            if !ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                if let Ok(v) = text.trim().replace(',', ".").parse::<f32>() {
                    let v = if v.is_finite() {
                        v.clamp(min, max)
                    } else {
                        *value
                    };
                    if (*value - v).abs() > f32::EPSILON {
                        *value = v;
                        response.mark_changed();
                    }
                }
            }
        }
    } else {
        ui.painter().text(
            value_rect.center(),
            egui::Align2::CENTER_CENTER,
            value_text,
            font,
            color,
        );
    }

    response
}

pub fn dev_slider(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
) -> bool {
    dev_slider_resp(ui, label, value, range, &neutral_track(ui)).changed()
}

/// Neutral-track [`dev_slider_resp`] (returns the `Response` for debouncing).
pub fn dev_slider_neutral_resp(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
) -> egui::Response {
    dev_slider_resp(ui, label, value, range, &neutral_track(ui))
}

fn neutral_track(ui: &egui::Ui) -> [Color32; 3] {
    let visuals = ui.visuals();
    let neutral = mix_color(visuals.weak_text_color(), visuals.text_color(), 0.45);
    [neutral, neutral, neutral]
}

fn paint_gradient_slider(
    ui: &egui::Ui,
    rect: egui::Rect,
    thumb_t: f32,
    min: f32,
    max: f32,
    colors: &[Color32],
) {
    let painter = ui.painter();
    let track = egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width(), 5.0));
    let steps = 72;
    for i in 0..steps {
        let t0 = i as f32 / steps as f32;
        let t1 = (i + 1) as f32 / steps as f32;
        let x0 = egui::lerp(track.left()..=track.right(), t0) - 0.5;
        let x1 = egui::lerp(track.left()..=track.right(), t1) + 0.5;
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(x0, track.top()), egui::pos2(x1, track.bottom())),
            0.0,
            sample_gradient(colors, (t0 + t1) * 0.5),
        );
    }
    if min < 0.0 && max > 0.0 {
        let zero_t = ((0.0 - min) / (max - min)).clamp(0.0, 1.0);
        let x = egui::lerp(track.left()..=track.right(), zero_t);
        painter.line_segment(
            [
                egui::pos2(x, rect.top() + 3.0),
                egui::pos2(x, rect.bottom() - 3.0),
            ],
            egui::Stroke::new(1.0_f32, ui.visuals().weak_text_color()),
        );
    }
    let x = egui::lerp(track.left()..=track.right(), thumb_t.clamp(0.0, 1.0));
    let thumb = [
        egui::pos2(x, rect.top() + 1.0),
        egui::pos2(x - 7.0, rect.top() + 12.0),
        egui::pos2(x + 7.0, rect.top() + 12.0),
    ];
    painter.add(egui::Shape::convex_polygon(
        thumb.to_vec(),
        ui.visuals().text_color(),
        egui::Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color),
    ));
}

#[cfg(test)]
mod value_box_tests {
    use super::*;

    /// One frame of a lone stacked slider on a 400 × 200 screen; returns the
    /// row's rectangle.
    fn frame(ctx: &egui::Context, value: &mut f32, events: Vec<egui::Event>) -> egui::Rect {
        let mut row = egui::Rect::NOTHING;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 200.0),
            )),
            events,
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| {
            let track = [Color32::DARK_GRAY, Color32::LIGHT_GRAY];
            row = dev_slider_stacked_resp(ui, "Thử", value, 0.0..=100.0, &track, 1.0).rect;
        });
        row
    }

    fn click(at: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn a_click_in_the_value_box_types_a_value_and_never_moves_the_slider() {
        let ctx = egui::Context::default();
        let mut value = 40.0f32;
        let row = frame(&ctx, &mut value, vec![]);
        let in_box = egui::pos2(row.right() - 8.0 - VALUE_W * 0.5, row.top() + 22.0);
        frame(&ctx, &mut value, vec![egui::Event::PointerMoved(in_box)]);
        frame(&ctx, &mut value, vec![click(in_box, true)]);
        frame(&ctx, &mut value, vec![click(in_box, false)]);
        assert_eq!(value, 40.0, "the click dragged the slider");
        // The box is a text field with its number selected: typing replaces
        // it, Enter takes the value.
        frame(&ctx, &mut value, vec![egui::Event::Text("75".to_string())]);
        assert_eq!(value, 40.0, "nothing is taken before Enter");
        frame(&ctx, &mut value, vec![key(egui::Key::Enter)]);
        assert_eq!(value, 75.0);

        // Again, out of range and given up with Esc: nothing changes.
        frame(&ctx, &mut value, vec![click(in_box, true)]);
        frame(&ctx, &mut value, vec![click(in_box, false)]);
        frame(&ctx, &mut value, vec![egui::Event::Text("5".to_string())]);
        frame(&ctx, &mut value, vec![key(egui::Key::Escape)]);
        assert_eq!(value, 75.0);
        // A typed value past the range stops at its end.
        frame(&ctx, &mut value, vec![click(in_box, true)]);
        frame(&ctx, &mut value, vec![click(in_box, false)]);
        frame(&ctx, &mut value, vec![egui::Event::Text("250".to_string())]);
        frame(&ctx, &mut value, vec![key(egui::Key::Enter)]);
        assert_eq!(value, 100.0);
    }

    #[test]
    fn a_click_on_the_track_still_moves_the_slider() {
        let ctx = egui::Context::default();
        let mut value = 40.0f32;
        let row = frame(&ctx, &mut value, vec![]);
        // The track runs from 92 px in to 8 px short of the value box.
        let (left, right) = (row.left() + 92.0, row.right() - 8.0 - VALUE_W - 8.0);
        let quarter = egui::pos2(left + (right - left) * 0.25, row.top() + 24.0);
        frame(&ctx, &mut value, vec![egui::Event::PointerMoved(quarter)]);
        frame(&ctx, &mut value, vec![click(quarter, true)]);
        frame(&ctx, &mut value, vec![click(quarter, false)]);
        assert!((value - 25.0).abs() < 0.5, "{value}");
    }
}

//! Shared custom widgets. The Camera-Raw slider (a gradient track + a triangular
//! thumb + a numeric readout) lives here so the adjustment dialogs can reuse the
//! exact same look instead of egui's default slider.

use egui::Color32;

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
            paint_gradient_slider(ui, rect, *value, min, max, colors);
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
) -> bool {
    dev_slider_stacked_resp(ui, label, value, range, colors).changed()
}

pub fn dev_slider_stacked_resp(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    colors: &[Color32],
) -> egui::Response {
    let min = *range.start();
    let max = *range.end();
    let row_w = ui.available_width().max(190.0);
    let (rect, mut response) =
        ui.allocate_exact_size(egui::vec2(row_w, 33.0), egui::Sense::click_and_drag());

    let precise = (max - min) <= 12.0;
    let value_text = if precise {
        format!("{:.1}", *value)
    } else {
        format!("{:.0}", *value)
    };
    let value_rect = egui::Rect::from_min_size(
        egui::pos2(rect.right() - VALUE_W, rect.top() + 14.0),
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
    let editing = ui
        .ctx()
        .data_mut(|d| d.get_temp::<String>(edit_id))
        .is_some();
    // Keyed off the press ORIGIN so a drag that started on the track keeps
    // driving the slider past the box, and one that started in the box never
    // yanks the slider to its maximum.
    let pressed_in_value_box = ui
        .input(|i| i.pointer.press_origin())
        .is_some_and(|p| value_rect.contains(p));

    if !editing && (response.dragged() || response.clicked()) {
        if response.clicked() && pressed_in_value_box {
            ui.ctx()
                .data_mut(|d| d.insert_temp(edit_id, value_text.clone()));
            ui.ctx().memory_mut(|m| m.request_focus(te_id));
        } else if !pressed_in_value_box {
            if let Some(pos) = response.interact_pointer_pos() {
                let t = ((pos.x - track_rect.left()) / track_rect.width()).clamp(0.0, 1.0);
                let new_value = min + (max - min) * t;
                if (*value - new_value).abs() > f32::EPSILON {
                    *value = new_value;
                    response.mark_changed();
                }
            }
        }
    }

    paint_gradient_slider(ui, track_rect, *value, min, max, colors);
    let font = egui::FontId::proportional(12.5);
    let color = ui.visuals().text_color();
    ui.painter().text(
        egui::pos2(rect.left() + 8.0, rect.top() + 1.0),
        egui::Align2::LEFT_TOP,
        label,
        font.clone(),
        color,
    );
    if let Some(mut text) = ui.ctx().data_mut(|d| d.get_temp::<String>(edit_id)) {
        let out = ui.put(
            value_rect,
            egui::TextEdit::singleline(&mut text)
                .id(te_id)
                .font(font)
                .margin(egui::vec2(2.0, 1.0)),
        );
        if out.changed() {
            ui.ctx().data_mut(|d| d.insert_temp(edit_id, text.clone()));
        }
        if out.lost_focus() {
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
    value: f32,
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
    let t = ((value - min) / (max - min)).clamp(0.0, 1.0);
    let x = egui::lerp(track.left()..=track.right(), t);
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

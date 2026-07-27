//! Compact colour picker — a trimmed port of egui's `color_picker_color32`
//! (egui-0.34 `widgets/color_picker.rs`). The ONLY behavioural change is a much
//! smaller 2D selection-indicator circle: egui hardcodes it to `rect.width()/12`
//! (≈22 px for our 270 px square — see color_picker.rs:227), which looked
//! oversized in the colour dialog. Opaque colours only (the paint dialog never
//! edits alpha). Keeps egui's hue/saturation cache so the hue doesn't jump when
//! saturation/value hit an extreme (e.g. pure black or white).

use egui::ecolor::{Hsva, HsvaGamma};
use egui::{
    epaint, lerp, pos2, remap_clamp, vec2, Color32, DragValue, Mesh, Response, Rgba, Sense, Shape,
    Stroke, StrokeKind, Ui, Vec2,
};

/// Vertices per dimension in the colour meshes (multiple of 6 to hit pure hues).
const N: u32 = 6 * 6;
/// Selection-indicator radius in the 2D square (egui uses `rect.width()/12`).
const INDICATOR_RADIUS: f32 = 7.0;

fn contrast_color(color: Color32) -> Color32 {
    if Rgba::from(color).intensity() < 0.5 {
        Color32::WHITE
    } else {
        Color32::BLACK
    }
}

fn color_slider_1d(ui: &mut Ui, value: &mut f32, color_at: impl Fn(f32) -> Color32) -> Response {
    let desired = vec2(ui.spacing().slider_width, ui.spacing().interact_size.y);
    let (rect, response) = ui.allocate_at_least(desired, Sense::click_and_drag());

    if let Some(mpos) = response.interact_pointer_pos() {
        *value = remap_clamp(mpos.x, rect.left()..=rect.right(), 0.0..=1.0);
    }

    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);

        let mut mesh = Mesh::default();
        for i in 0..=N {
            let t = i as f32 / N as f32;
            let color = color_at(t);
            let x = lerp(rect.left()..=rect.right(), t);
            mesh.colored_vertex(pos2(x, rect.top()), color);
            mesh.colored_vertex(pos2(x, rect.bottom()), color);
            if i < N {
                mesh.add_triangle(2 * i, 2 * i + 1, 2 * i + 2);
                mesh.add_triangle(2 * i + 1, 2 * i + 2, 2 * i + 3);
            }
        }
        ui.painter().add(Shape::mesh(mesh));
        ui.painter()
            .rect_stroke(rect, 0.0, visuals.bg_stroke, StrokeKind::Inside);

        let x = lerp(rect.left()..=rect.right(), *value);
        let r = rect.height() / 4.0;
        let picked = color_at(*value);
        ui.painter().add(Shape::convex_polygon(
            vec![
                pos2(x, rect.center().y),
                pos2(x + r, rect.bottom()),
                pos2(x - r, rect.bottom()),
            ],
            picked,
            Stroke::new(visuals.fg_stroke.width, contrast_color(picked)),
        ));
    }

    response
}

fn color_slider_2d(
    ui: &mut Ui,
    x_value: &mut f32,
    y_value: &mut f32,
    color_at: impl Fn(f32, f32) -> Color32,
) -> Response {
    let desired = Vec2::splat(ui.spacing().slider_width);
    let (rect, response) = ui.allocate_at_least(desired, Sense::click_and_drag());

    if let Some(mpos) = response.interact_pointer_pos() {
        *x_value = remap_clamp(mpos.x, rect.left()..=rect.right(), 0.0..=1.0);
        *y_value = remap_clamp(mpos.y, rect.bottom()..=rect.top(), 0.0..=1.0);
    }

    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);

        let mut mesh = Mesh::default();
        for xi in 0..=N {
            for yi in 0..=N {
                let xt = xi as f32 / N as f32;
                let yt = yi as f32 / N as f32;
                let color = color_at(xt, yt);
                let x = lerp(rect.left()..=rect.right(), xt);
                let y = lerp(rect.bottom()..=rect.top(), yt);
                mesh.colored_vertex(pos2(x, y), color);
                if xi < N && yi < N {
                    let x_offset = 1;
                    let y_offset = N + 1;
                    let tl = yi * y_offset + xi;
                    mesh.add_triangle(tl, tl + x_offset, tl + y_offset);
                    mesh.add_triangle(tl + x_offset, tl + y_offset, tl + y_offset + x_offset);
                }
            }
        }
        ui.painter().add(Shape::mesh(mesh));
        ui.painter()
            .rect_stroke(rect, 0.0, visuals.bg_stroke, StrokeKind::Inside);

        let x = lerp(rect.left()..=rect.right(), *x_value);
        let y = lerp(rect.bottom()..=rect.top(), *y_value);
        let picked = color_at(*x_value, *y_value);
        ui.painter().add(epaint::CircleShape {
            center: pos2(x, y),
            radius: INDICATOR_RADIUS,
            fill: picked,
            stroke: Stroke::new(visuals.fg_stroke.width.max(1.5), contrast_color(picked)),
        });
    }

    response
}

/// Editable `#RRGGBB` field for copy/paste. The in-progress text is kept in egui
/// memory (alongside the colour it maps to) so a partial edit survives the
/// per-frame model rebuild. Crucially, if the colour is changed from *elsewhere*
/// in the picker (dragging the 2-D square or hue) while this field is focused,
/// the buffer is dropped and focus surrendered so the field follows the picked
/// colour instead of holding a stale value. Returns `true` when a complete,
/// valid hex updated `rgb`.
fn hex_field(ui: &mut Ui, rgb: &mut [u8; 3]) -> bool {
    let id = ui.id().with("iai_hex_field");
    let live_rgb = *rgb;
    let live = format!("{:02X}{:02X}{:02X}", live_rgb[0], live_rgb[1], live_rgb[2]);

    // Keep the in-progress buffer only while it still maps to the live colour
    // (small tolerance absorbs HSV round-trip noise); otherwise resync to live.
    let close = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b.iter()).all(|(x, y)| x.abs_diff(*y) <= 2);
    let prior: Option<(String, [u8; 3])> = ui.data(|d| d.get_temp(id));
    let (mut text, resynced) = match prior {
        Some((buf, buf_rgb)) if close(buf_rgb, live_rgb) => (buf, false),
        Some(_) => (live.clone(), true),
        None => (live.clone(), false),
    };

    let resp = ui.add(
        egui::TextEdit::singleline(&mut text)
            .desired_width(62.0)
            .font(egui::TextStyle::Monospace)
            .char_limit(7)
            .hint_text("RRGGBB"),
    );

    // The colour moved under a focused field (user clicked into the square):
    // release the selection so it visibly deselects and tracks the new colour.
    if resynced && resp.has_focus() {
        resp.surrender_focus();
    }

    let mut changed = false;
    let mut mapped = live_rgb;
    if resp.has_focus() {
        // Editing: accept `#RRGGBB` or `RRGGBB`, keep the buffer verbatim.
        let s = text.trim().trim_start_matches('#');
        if s.len() == 6 {
            if let Ok(v) = u32::from_str_radix(s, 16) {
                let parsed = [
                    ((v >> 16) & 0xFF) as u8,
                    ((v >> 8) & 0xFF) as u8,
                    (v & 0xFF) as u8,
                ];
                *rgb = parsed;
                mapped = parsed;
                changed = true;
            }
        }
        ui.data_mut(|d| d.insert_temp(id, (text, mapped)));
    } else {
        // Idle: mirror the live colour (canonical upper-case form).
        ui.data_mut(|d| d.insert_temp(id, (live, live_rgb)));
    }
    changed
}

/// Compact opaque colour picker (RGB + #hex row + 2D saturation/value square +
/// hue slider) with a small selection indicator. Returns `true` if `srgba`
/// changed.
pub fn color_picker_compact(ui: &mut Ui, srgba: &mut Color32) -> bool {
    let before = *srgba;

    let id = ui.id().with("iai_compact_color_picker");
    let mut hsva: HsvaGamma = ui
        .data(|d| d.get_temp::<(Color32, HsvaGamma)>(id))
        .filter(|(cached, _)| *cached == *srgba)
        .map(|(_, hsva)| hsva)
        .unwrap_or_else(|| HsvaGamma::from(Hsva::from(*srgba)));

    ui.vertical(|ui| {
        let mut rgba = Hsva::from(hsva).to_srgba_unmultiplied();
        let mut rgb_edited = false;
        ui.horizontal(|ui| {
            rgb_edited |= ui
                .add(DragValue::new(&mut rgba[0]).speed(0.5).prefix("R "))
                .changed();
            rgb_edited |= ui
                .add(DragValue::new(&mut rgba[1]).speed(0.5).prefix("G "))
                .changed();
            rgb_edited |= ui
                .add(DragValue::new(&mut rgba[2]).speed(0.5).prefix("B "))
                .changed();
            ui.add_space(4.0);
            ui.label("#");
            let mut rgb3 = [rgba[0], rgba[1], rgba[2]];
            if hex_field(ui, &mut rgb3) {
                rgba = [rgb3[0], rgb3[1], rgb3[2], 255];
                rgb_edited = true;
            }
        });
        if rgb_edited {
            rgba[3] = 255;
            hsva = HsvaGamma::from(Hsva::from_srgba_unmultiplied(rgba));
        }

        let HsvaGamma { h, s, v, a: _ } = &mut hsva;
        let h_now = *h;
        color_slider_2d(ui, s, v, |s, v| {
            HsvaGamma {
                h: h_now,
                s,
                v,
                a: 1.0,
            }
            .into()
        })
        .on_hover_text("Saturation / Value");
        color_slider_1d(ui, h, |h| {
            HsvaGamma {
                h,
                s: 1.0,
                v: 1.0,
                a: 1.0,
            }
            .into()
        })
        .on_hover_text("Hue");
    });

    hsva.a = 1.0;
    *srgba = Color32::from(hsva);
    ui.data_mut(|d| d.insert_temp(id, (*srgba, hsva)));

    *srgba != before
}

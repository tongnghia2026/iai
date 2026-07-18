//! Adjustment dialogs: Levels/Curves/Color Balance/Gradient Map editors,
//! their presets, histograms and auto algorithms.

use super::*;

/// Draws a full-screen semi-transparent overlay that blocks clicks behind modal dialogs.
///
/// Dialog windows use `DIALOG_ORDER` (`Foreground`) instead of `Tooltip` because
/// egui combo boxes and menus open on `Foreground`. Putting a dialog on `Tooltip`
/// makes its own drop-downs appear underneath the dialog and miss input.
pub(crate) fn adjustment_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let esc_pressed = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    let enter_pressed = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
    if esc_pressed {
        actions.dialogs.cancel_adjustment_dialog = true;
    }

    let mut adj = data.dialogs.adjustment_dialog.clone();
    let title = adj.name().to_string();
    let is_levels = matches!(adj, AdjustmentType::Levels { .. });
    let is_curves = matches!(adj, AdjustmentType::Curves { .. });
    let is_color_balance = matches!(adj, AdjustmentType::ColorBalance { .. });
    let mut changed = false;
    let mut apply = enter_pressed;
    let mut cancel = false;
    let mut custom_footer = false;

    let dialog_width = if is_levels {
        484.0
    } else if is_color_balance {
        456.0
    } else if is_curves {
        484.0
    } else {
        360.0
    };
    let default_pos = document_side_dialog_pos(ctx, data, dialog_width, 96.0);
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .movable(true)
        .default_pos(default_pos)
        .default_width(dialog_width)
        .min_width(dialog_width)
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.spacing_mut().slider_width = 220.0;
            ui.add_space(6.0);

            match &mut adj {
                AdjustmentType::Levels { channels } => {
                    custom_footer = true;
                    changed |= levels_editor_ui(
                        ui,
                        data,
                        data.dialogs.levels_histogram.as_ref(),
                        channels,
                        data.dialogs.adjustment_options,
                        data.dialogs.adjustment_preview_enabled,
                        data.dialogs.adj_eyedropper,
                        &mut apply,
                        &mut cancel,
                        actions,
                    );
                    changed |= normalize_levels_channels(channels);
                }
                AdjustmentType::HueSaturation {
                    hue,
                    saturation,
                    lightness,
                } => {
                    let hue_colors: Vec<egui::Color32> = (0..=12)
                        .map(|i| hsv_to_color32(i as f32 / 12.0, 1.0, 1.0))
                        .collect();
                    changed |=
                        color_slider_f32(ui, hue, -180.0..=180.0, "Hue", &hue_colors).changed();
                    changed |= color_slider_f32(
                        ui,
                        saturation,
                        -100.0..=100.0,
                        "Saturation",
                        &[
                            egui::Color32::from_gray(130),
                            hsv_to_color32(0.0, 0.55, 0.85),
                            hsv_to_color32(0.0, 1.0, 1.0),
                        ],
                    )
                    .changed();
                    changed |= color_slider_f32(
                        ui,
                        lightness,
                        -100.0..=100.0,
                        "Lightness",
                        &[
                            egui::Color32::BLACK,
                            egui::Color32::GRAY,
                            egui::Color32::WHITE,
                        ],
                    )
                    .changed();
                }
                AdjustmentType::ColorBalance {
                    shadows,
                    midtones,
                    highlights,
                    preserve_luminosity,
                } => {
                    custom_footer = true;
                    changed |= color_balance_editor_ui(
                        ui,
                        shadows,
                        midtones,
                        highlights,
                        preserve_luminosity,
                        &mut apply,
                        &mut cancel,
                    );
                }
                AdjustmentType::Curves { channels } => {
                    custom_footer = true;
                    changed |= curves_editor_ui(
                        ui,
                        data,
                        data.dialogs.levels_histogram.as_ref(),
                        channels,
                        data.dialogs.adjustment_options,
                        data.dialogs.adjustment_preview_enabled,
                        data.dialogs.adj_eyedropper,
                        &mut apply,
                        &mut cancel,
                        actions,
                    );
                }
                AdjustmentType::BrightnessContrast {
                    brightness,
                    contrast,
                } => {
                    changed |= dev_slider(ui, "Brightness", brightness, -100.0..=100.0);
                    ui.add_space(2.0);
                    changed |= dev_slider(ui, "Contrast", contrast, -100.0..=100.0);
                }
                AdjustmentType::Vibrance {
                    vibrance,
                    saturation,
                } => {
                    changed |= dev_slider(ui, "Vibrance", vibrance, -100.0..=100.0);
                    ui.add_space(2.0);
                    changed |= dev_slider(ui, "Saturation", saturation, -100.0..=100.0);
                }
                AdjustmentType::Exposure {
                    exposure,
                    offset,
                    gamma,
                } => {
                    changed |= dev_slider(ui, "Exposure", exposure, -20.0..=20.0);
                    ui.add_space(2.0);
                    changed |= dev_slider(ui, "Offset", offset, -0.5..=0.5);
                    ui.add_space(2.0);
                    changed |= dev_slider(ui, "Gamma", gamma, 0.01..=9.99);
                }
                AdjustmentType::Threshold { value } => {
                    changed |= ui
                        .add(egui::Slider::new(value, 1..=255).text("Threshold Level"))
                        .changed();
                }
                AdjustmentType::Posterize { levels } => {
                    changed |= ui
                        .add(egui::Slider::new(levels, 2..=255).text("Levels"))
                        .changed();
                }
                AdjustmentType::BlackAndWhite { r, y, g, c, b, m } => {
                    for (val, label) in [
                        (r, "Reds"),
                        (y, "Yellows"),
                        (g, "Greens"),
                        (c, "Cyans"),
                        (b, "Blues"),
                        (m, "Magentas"),
                    ] {
                        changed |= dev_slider(ui, label, val, -200.0..=300.0);
                        ui.add_space(2.0);
                    }
                }
                AdjustmentType::PhotoFilter {
                    color,
                    density,
                    luminosity,
                } => {
                    ui.horizontal(|ui| {
                        ui.label("Filter Color");
                        changed |= ui.color_edit_button_srgb(color).changed();
                    });
                    ui.add_space(4.0);
                    changed |= dev_slider(ui, "Density", density, 0.0..=1.0);
                    ui.add_space(4.0);
                    changed |= ui.checkbox(luminosity, "Preserve Luminosity").changed();
                }
                AdjustmentType::GradientMap {
                    stops,
                    reverse,
                    dither,
                } => {
                    changed |= gradient_map_ui(ui, stops, reverse, dither);
                }
                AdjustmentType::ChannelMixer {
                    red,
                    green,
                    blue,
                    monochrome,
                } => {
                    changed |= ui.checkbox(monochrome, "Monochrome").changed();
                    ui.add_space(6.0);
                    if *monochrome {
                        // One "Gray" output: keep all three output rows identical so
                        // luminance(mix) equals the weighted gray the sliders describe.
                        let mut w = *red;
                        let mut row_changed = false;
                        ui.label(egui::RichText::new("Gray Output").strong());
                        for (i, label) in ["Red", "Green", "Blue"].iter().enumerate() {
                            row_changed |= dev_slider(ui, label, &mut w[i], -2.0..=2.0);
                        }
                        if row_changed {
                            *red = w;
                            *green = w;
                            *blue = w;
                            changed = true;
                        }
                    } else {
                        for (row, name) in [
                            (&mut *red, "Output Red"),
                            (&mut *green, "Output Green"),
                            (&mut *blue, "Output Blue"),
                        ] {
                            ui.label(egui::RichText::new(name).strong());
                            for (i, label) in ["Red", "Green", "Blue"].iter().enumerate() {
                                changed |= dev_slider(ui, label, &mut row[i], -2.0..=2.0);
                            }
                            ui.add_space(4.0);
                        }
                    }
                }
                _ => {
                    ui.label("This adjustment does not have a dialog yet.");
                }
            }

            if changed {
                actions.dialogs.set_adjustment_dialog = Some(adj.clone());
            }

            if !custom_footer {
                ui.add_space(12.0);
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Reset").clicked() {
                        actions.dialogs.set_adjustment_dialog = Some(default_adjustment_like(&adj));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                        if ui.button("OK").clicked() {
                            apply = true;
                        }
                    });
                });
                ui.add_space(4.0);
            }
        });

    if apply {
        if changed {
            actions.dialogs.set_adjustment_dialog = Some(adj);
        }
        actions.dialogs.apply_adjustment_dialog = true;
    }
    if cancel {
        actions.dialogs.cancel_adjustment_dialog = true;
    }
}

/// standard Gradient Map editor: a live gradient bar with draggable color
/// stops (click an empty part of the strip to add one, drag to move, right-click or
/// press Delete to remove the selected stop), a color picker + precise location for
/// the selected stop, Reverse / Dither toggles, and a few preset gradients.
/// Returns true when anything changed this frame.
pub(crate) fn gradient_map_ui(
    ui: &mut egui::Ui,
    stops: &mut Vec<(f32, [u8; 3])>,
    reverse: &mut bool,
    dither: &mut bool,
) -> bool {
    use egui::{Color32, Mesh, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Vec2};
    let mut changed = false;

    // Invariants: at least two stops, kept sorted ascending by position.
    if stops.len() < 2 {
        *stops = vec![(0.0, [0, 0, 0]), (1.0, [255, 255, 255])];
        changed = true;
    }
    stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let id_base = ui.id().with("gmap");
    let sel_id = id_base.with("sel");
    let mut sel: usize = ui.data(|d| d.get_temp(sel_id)).unwrap_or(0);
    sel = sel.min(stops.len() - 1);

    // ---- Preset gradients ------------------------------------------------
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Preset")
                .size(11.0)
                .color(Color32::from_gray(150)),
        );
        let presets: [(&str, &[(f32, [u8; 3])]); 4] = [
            ("B→W", &[(0.0, [0, 0, 0]), (1.0, [255, 255, 255])]),
            (
                "Sepia",
                &[
                    (0.0, [30, 16, 8]),
                    (0.5, [146, 96, 50]),
                    (1.0, [255, 244, 222]),
                ],
            ),
            (
                "Cool→Warm",
                &[
                    (0.0, [20, 30, 80]),
                    (0.5, [180, 120, 90]),
                    (1.0, [255, 235, 170]),
                ],
            ),
            (
                "Spectrum",
                &[
                    (0.0, [0, 0, 0]),
                    (0.25, [120, 0, 160]),
                    (0.5, [220, 40, 40]),
                    (0.75, [240, 200, 40]),
                    (1.0, [255, 255, 255]),
                ],
            ),
        ];
        for (name, preset) in presets {
            if ui.button(name).clicked() {
                *stops = preset.to_vec();
                sel = 0;
                changed = true;
            }
        }
    });
    ui.add_space(8.0);

    // ---- Geometry: gradient bar + a draggable markers row beneath it -----
    let bar_w = 256.0;
    let bar_h = 24.0;
    let marker_h = 14.0;
    let (area, _) =
        ui.allocate_exact_size(Vec2::new(bar_w, bar_h + 4.0 + marker_h), Sense::hover());
    let bar_rect = Rect::from_min_size(area.min, Vec2::new(bar_w, bar_h));
    let marker_top = bar_rect.bottom() + 4.0;
    let marker_rect = Rect::from_min_size(
        Pos2::new(area.left(), marker_top),
        Vec2::new(bar_w, marker_h),
    );
    let resp = ui.interact(
        marker_rect,
        id_base.with("markers"),
        Sense::click_and_drag(),
    );

    let to_x = |t: f32| bar_rect.left() + t.clamp(0.0, 1.0) * bar_w;
    let to_t = |x: f32| ((x - bar_rect.left()) / bar_w).clamp(0.0, 1.0);
    let hit_px = 8.0;
    let nearest = |x: f32, stops: &[(f32, [u8; 3])]| -> Option<usize> {
        let mut best = None;
        let mut best_d = hit_px;
        for (i, (pos, _)) in stops.iter().enumerate() {
            let d = (to_x(*pos) - x).abs();
            if d <= best_d {
                best_d = d;
                best = Some(i);
            }
        }
        best
    };

    // ---- Interaction (before drawing so the markers reflect new state) ---
    if resp.drag_started() || resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            if let Some(i) = nearest(p.x, stops) {
                sel = i;
            } else {
                let t = to_t(p.x);
                let c = crate::core::color::sample_gradient_stops(stops, t);
                let idx = stops.partition_point(|(pp, _)| *pp < t);
                stops.insert(idx, (t, c));
                sel = idx;
                changed = true;
            }
        }
    }
    if resp.dragged() {
        if let Some(p) = resp.interact_pointer_pos() {
            let lo = if sel == 0 {
                0.0
            } else {
                stops[sel - 1].0 + 0.001
            };
            let hi = if sel + 1 >= stops.len() {
                1.0
            } else {
                stops[sel + 1].0 - 0.001
            };
            let new_t = to_t(p.x).clamp(lo.min(hi), hi.max(lo));
            if (new_t - stops[sel].0).abs() > f32::EPSILON {
                stops[sel].0 = new_t;
                changed = true;
            }
        }
    }
    if resp.secondary_clicked() && stops.len() > 2 {
        if let Some(p) = resp.interact_pointer_pos() {
            if let Some(i) = nearest(p.x, stops) {
                stops.remove(i);
                sel = sel.min(stops.len() - 1);
                changed = true;
            }
        }
    }

    // ---- Gradient bar as one GPU-interpolated mesh (fast + smooth) -------
    {
        let painter = ui.painter_at(bar_rect);
        // Edge points so the bar still fills if end stops aren't at 0 / 1.
        let mut pts: Vec<(f32, Color32)> = Vec::with_capacity(stops.len() + 2);
        if stops[0].0 > 0.0 {
            let c = stops[0].1;
            pts.push((0.0, Color32::from_rgb(c[0], c[1], c[2])));
        }
        for (pos, col) in stops.iter() {
            pts.push((
                pos.clamp(0.0, 1.0),
                Color32::from_rgb(col[0], col[1], col[2]),
            ));
        }
        let lastc = stops[stops.len() - 1].1;
        if stops[stops.len() - 1].0 < 1.0 {
            pts.push((1.0, Color32::from_rgb(lastc[0], lastc[1], lastc[2])));
        }
        let mut mesh = Mesh::default();
        for (k, (t, c)) in pts.iter().enumerate() {
            let x = bar_rect.left() + t * bar_w;
            mesh.colored_vertex(Pos2::new(x, bar_rect.top()), *c);
            mesh.colored_vertex(Pos2::new(x, bar_rect.bottom()), *c);
            if k > 0 {
                let b = ((k - 1) * 2) as u32;
                mesh.add_triangle(b, b + 1, b + 2);
                mesh.add_triangle(b + 1, b + 2, b + 3);
            }
        }
        painter.add(Shape::mesh(mesh));
        painter.rect_stroke(
            bar_rect,
            0.0,
            Stroke::new(1.0_f32, Color32::from_gray(80)),
            StrokeKind::Inside,
        );
    }

    // ---- Color-stop markers (house shape pointing up at the bar) ---------
    {
        let painter = ui.painter_at(marker_rect);
        for (i, (pos, col)) in stops.iter().enumerate() {
            let x = to_x(*pos);
            let fill = Color32::from_rgb(col[0], col[1], col[2]);
            let selected = i == sel;
            let outline = if selected {
                Color32::WHITE
            } else {
                Color32::from_gray(120)
            };
            let w = 5.0;
            painter.add(Shape::convex_polygon(
                vec![
                    Pos2::new(x, marker_top),                // tip (touches the bar)
                    Pos2::new(x - w, marker_top + 5.0),      // left shoulder
                    Pos2::new(x - w, marker_top + marker_h), // bottom-left
                    Pos2::new(x + w, marker_top + marker_h), // bottom-right
                    Pos2::new(x + w, marker_top + 5.0),      // right shoulder
                ],
                fill,
                Stroke::new(if selected { 2.0_f32 } else { 1.0_f32 }, outline),
            ));
        }
    }

    ui.add_space(10.0);

    // ---- "Stops" controls for the selected stop --------------------------
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label("Color");
            let mut col = stops[sel].1;
            if ui.color_edit_button_srgb(&mut col).changed() {
                stops[sel].1 = col;
                changed = true;
            }
            ui.add_space(12.0);
            ui.label("Location");
            let lo = if sel == 0 {
                0.0
            } else {
                stops[sel - 1].0 + 0.001
            };
            let hi = if sel + 1 >= stops.len() {
                1.0
            } else {
                stops[sel + 1].0 - 0.001
            };
            let mut pct = stops[sel].0 * 100.0;
            if ui
                .add(
                    egui::DragValue::new(&mut pct)
                        .range((lo * 100.0)..=(hi * 100.0))
                        .suffix("%")
                        .speed(0.5),
                )
                .changed()
            {
                stops[sel].0 = (pct / 100.0).clamp(lo, hi);
                changed = true;
            }
            ui.add_space(12.0);
            if ui
                .add_enabled(stops.len() > 2, egui::Button::new("Delete"))
                .clicked()
            {
                stops.remove(sel);
                sel = sel.min(stops.len() - 1);
                changed = true;
            }
        });
    });

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        changed |= ui.checkbox(reverse, "Reverse").changed();
        ui.add_space(16.0);
        changed |= ui.checkbox(dither, "Dither").changed();
    });
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new(
            "Click below the bar to add a stop · drag to move · right-click to remove",
        )
        .size(10.0)
        .color(Color32::from_gray(140)),
    );

    ui.data_mut(|d| d.insert_temp(sel_id, sel));
    changed
}

/// Sample an RGBA color-stop gradient at `t` (0..1). `stops` sorted ascending.
pub(crate) fn sample_rgba(stops: &[(f32, [u8; 4])], t: f32) -> [u8; 4] {
    if stops.is_empty() {
        return [0, 0, 0, 255];
    }
    let t = t.clamp(0.0, 1.0);
    let last = stops.len() - 1;
    if t <= stops[0].0 {
        return stops[0].1;
    }
    if t >= stops[last].0 {
        return stops[last].1;
    }
    for i in 0..last {
        let (p0, c0) = stops[i];
        let (p1, c1) = stops[i + 1];
        if t >= p0 && t <= p1 {
            let range = p1 - p0;
            if range < 1e-6 {
                return c1;
            }
            let lt = (t - p0) / range;
            let inv = 1.0 - lt;
            return [
                (c0[0] as f32 * inv + c1[0] as f32 * lt).round() as u8,
                (c0[1] as f32 * inv + c1[1] as f32 * lt).round() as u8,
                (c0[2] as f32 * inv + c1[2] as f32 * lt).round() as u8,
                (c0[3] as f32 * inv + c1[3] as f32 * lt).round() as u8,
            ];
        }
    }
    stops[last].1
}

/// Paint a gradient ramp into `rect`: an optional checkerboard (for transparency),
/// the gradient as one interpolated mesh, then a thin border. Shared by the
/// Gradient tool's editor and its options-bar swatch.
pub(crate) fn paint_gradient_bar(
    painter: &egui::Painter,
    rect: egui::Rect,
    stops: &[(f32, [u8; 4])],
    checker: bool,
) {
    use egui::{Color32, Mesh, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2};
    if stops.is_empty() {
        return;
    }
    if checker {
        let cs = 6.0;
        let cols = (rect.width() / cs).ceil() as i32;
        let rows = (rect.height() / cs).ceil() as i32;
        for ry in 0..rows {
            for rx in 0..cols {
                let on = (rx + ry) % 2 == 0;
                let col = if on {
                    Color32::from_gray(160)
                } else {
                    Color32::from_gray(96)
                };
                let x0 = rect.left() + rx as f32 * cs;
                let y0 = rect.top() + ry as f32 * cs;
                let cell = Rect::from_min_size(Pos2::new(x0, y0), Vec2::splat(cs)).intersect(rect);
                painter.rect_filled(cell, 0.0, col);
            }
        }
    }
    let to_col = |c: [u8; 4]| Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
    let mut pts: Vec<(f32, Color32)> = Vec::with_capacity(stops.len() + 2);
    if stops[0].0 > 0.0 {
        pts.push((0.0, to_col(stops[0].1)));
    }
    for (p, c) in stops.iter() {
        pts.push((p.clamp(0.0, 1.0), to_col(*c)));
    }
    if stops[stops.len() - 1].0 < 1.0 {
        pts.push((1.0, to_col(stops[stops.len() - 1].1)));
    }
    let mut mesh = Mesh::default();
    for (k, (t, c)) in pts.iter().enumerate() {
        let x = rect.left() + t * rect.width();
        mesh.colored_vertex(Pos2::new(x, rect.top()), *c);
        mesh.colored_vertex(Pos2::new(x, rect.bottom()), *c);
        if k > 0 {
            let b = ((k - 1) * 2) as u32;
            mesh.add_triangle(b, b + 1, b + 2);
            mesh.add_triangle(b + 1, b + 2, b + 3);
        }
    }
    painter.add(Shape::mesh(mesh));
    painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0_f32, Color32::from_gray(80)),
        StrokeKind::Inside,
    );
}

/// standard gradient editor on RGBA stops (used by the Gradient tool).
/// Draggable color stops below the bar (click to add, drag to move, right-click to
/// remove), per-stop Color / Opacity / Location, presets. Returns true on change.
pub(crate) fn gradient_editor(
    ui: &mut egui::Ui,
    stops: &mut Vec<(f32, [u8; 4])>,
    show_opacity: bool,
) -> bool {
    use egui::{Color32, Pos2, Rect, Sense, Shape, Stroke, Vec2};
    let mut changed = false;

    if stops.len() < 2 {
        *stops = vec![(0.0, [0, 0, 0, 255]), (1.0, [255, 255, 255, 255])];
        changed = true;
    }
    stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let id_base = ui.id().with("grad_editor");
    let sel_id = id_base.with("sel");
    let mut sel: usize = ui.data(|d| d.get_temp(sel_id)).unwrap_or(0);
    sel = sel.min(stops.len() - 1);

    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Preset")
                .size(11.0)
                .color(Color32::from_gray(150)),
        );
        let presets: [(&str, &[(f32, [u8; 4])]); 4] = [
            ("B→W", &[(0.0, [0, 0, 0, 255]), (1.0, [255, 255, 255, 255])]),
            (
                "Sepia",
                &[
                    (0.0, [30, 16, 8, 255]),
                    (0.5, [146, 96, 50, 255]),
                    (1.0, [255, 244, 222, 255]),
                ],
            ),
            (
                "Cool→Warm",
                &[
                    (0.0, [20, 30, 80, 255]),
                    (0.5, [180, 120, 90, 255]),
                    (1.0, [255, 235, 170, 255]),
                ],
            ),
            (
                "Spectrum",
                &[
                    (0.0, [0, 0, 0, 255]),
                    (0.25, [120, 0, 160, 255]),
                    (0.5, [220, 40, 40, 255]),
                    (0.75, [240, 200, 40, 255]),
                    (1.0, [255, 255, 255, 255]),
                ],
            ),
        ];
        for (name, preset) in presets {
            if ui.button(name).clicked() {
                *stops = preset.to_vec();
                sel = 0;
                changed = true;
            }
        }
        if show_opacity && ui.button("FG→Transp").clicked() {
            let c = stops[sel].1;
            *stops = vec![(0.0, [c[0], c[1], c[2], 255]), (1.0, [c[0], c[1], c[2], 0])];
            sel = 0;
            changed = true;
        }
    });
    ui.add_space(8.0);

    let bar_w = 256.0;
    let bar_h = 24.0;
    let marker_h = 14.0;
    let (area, _) =
        ui.allocate_exact_size(Vec2::new(bar_w, bar_h + 4.0 + marker_h), Sense::hover());
    let bar_rect = Rect::from_min_size(area.min, Vec2::new(bar_w, bar_h));
    let marker_top = bar_rect.bottom() + 4.0;
    let marker_rect = Rect::from_min_size(
        Pos2::new(area.left(), marker_top),
        Vec2::new(bar_w, marker_h),
    );
    let resp = ui.interact(
        marker_rect,
        id_base.with("markers"),
        Sense::click_and_drag(),
    );

    let to_x = |t: f32| bar_rect.left() + t.clamp(0.0, 1.0) * bar_w;
    let to_t = |x: f32| ((x - bar_rect.left()) / bar_w).clamp(0.0, 1.0);
    let hit_px = 8.0;
    let nearest = |x: f32, stops: &[(f32, [u8; 4])]| -> Option<usize> {
        let mut best = None;
        let mut best_d = hit_px;
        for (i, (pos, _)) in stops.iter().enumerate() {
            let d = (to_x(*pos) - x).abs();
            if d <= best_d {
                best_d = d;
                best = Some(i);
            }
        }
        best
    };

    if resp.drag_started() || resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            if let Some(i) = nearest(p.x, stops) {
                sel = i;
            } else {
                let t = to_t(p.x);
                let c = sample_rgba(stops, t);
                let idx = stops.partition_point(|(pp, _)| *pp < t);
                stops.insert(idx, (t, c));
                sel = idx;
                changed = true;
            }
        }
    }
    if resp.dragged() {
        if let Some(p) = resp.interact_pointer_pos() {
            let lo = if sel == 0 {
                0.0
            } else {
                stops[sel - 1].0 + 0.001
            };
            let hi = if sel + 1 >= stops.len() {
                1.0
            } else {
                stops[sel + 1].0 - 0.001
            };
            let new_t = to_t(p.x).clamp(lo.min(hi), hi.max(lo));
            if (new_t - stops[sel].0).abs() > f32::EPSILON {
                stops[sel].0 = new_t;
                changed = true;
            }
        }
    }
    if resp.secondary_clicked() && stops.len() > 2 {
        if let Some(p) = resp.interact_pointer_pos() {
            if let Some(i) = nearest(p.x, stops) {
                stops.remove(i);
                sel = sel.min(stops.len() - 1);
                changed = true;
            }
        }
    }

    paint_gradient_bar(&ui.painter_at(bar_rect), bar_rect, stops, show_opacity);

    {
        let painter = ui.painter_at(marker_rect);
        for (i, (pos, col)) in stops.iter().enumerate() {
            let x = to_x(*pos);
            let fill = Color32::from_rgb(col[0], col[1], col[2]);
            let selected = i == sel;
            let outline = if selected {
                Color32::WHITE
            } else {
                Color32::from_gray(120)
            };
            let w = 5.0;
            painter.add(Shape::convex_polygon(
                vec![
                    Pos2::new(x, marker_top),
                    Pos2::new(x - w, marker_top + 5.0),
                    Pos2::new(x - w, marker_top + marker_h),
                    Pos2::new(x + w, marker_top + marker_h),
                    Pos2::new(x + w, marker_top + 5.0),
                ],
                fill,
                Stroke::new(if selected { 2.0_f32 } else { 1.0_f32 }, outline),
            ));
        }
    }

    ui.add_space(10.0);

    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label("Color");
            let mut rgb = [stops[sel].1[0], stops[sel].1[1], stops[sel].1[2]];
            if ui.color_edit_button_srgb(&mut rgb).changed() {
                stops[sel].1[0] = rgb[0];
                stops[sel].1[1] = rgb[1];
                stops[sel].1[2] = rgb[2];
                changed = true;
            }
            if show_opacity {
                ui.add_space(10.0);
                ui.label("Opacity");
                let mut op = stops[sel].1[3] as f32 / 255.0 * 100.0;
                if ui
                    .add(
                        egui::DragValue::new(&mut op)
                            .range(0.0..=100.0)
                            .suffix("%")
                            .speed(0.5),
                    )
                    .changed()
                {
                    stops[sel].1[3] = (op / 100.0 * 255.0).round().clamp(0.0, 255.0) as u8;
                    changed = true;
                }
            }
            ui.add_space(10.0);
            ui.label("Location");
            let lo = if sel == 0 {
                0.0
            } else {
                stops[sel - 1].0 + 0.001
            };
            let hi = if sel + 1 >= stops.len() {
                1.0
            } else {
                stops[sel + 1].0 - 0.001
            };
            let mut pct = stops[sel].0 * 100.0;
            if ui
                .add(
                    egui::DragValue::new(&mut pct)
                        .range((lo * 100.0)..=(hi * 100.0))
                        .suffix("%")
                        .speed(0.5),
                )
                .changed()
            {
                stops[sel].0 = (pct / 100.0).clamp(lo, hi);
                changed = true;
            }
            ui.add_space(10.0);
            if ui
                .add_enabled(stops.len() > 2, egui::Button::new("Delete"))
                .clicked()
            {
                stops.remove(sel);
                sel = sel.min(stops.len() - 1);
                changed = true;
            }
        });
    });
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(
            "Click below the bar to add a stop · drag to move · right-click to remove",
        )
        .size(10.0)
        .color(Color32::from_gray(140)),
    );

    ui.data_mut(|d| d.insert_temp(sel_id, sel));
    changed
}

/// The Gradient tool's pop-up Gradient Editor window (opened from the options bar).
pub(crate) fn gradient_editor_window(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let mut stops = data.tool.gradient_stops.clone();
    let mut changed = false;
    let mut open = true;
    egui::Window::new("Gradient Editor")
        .collapsible(false)
        .resizable(false)
        .default_pos(ctx.content_rect().center() - egui::vec2(160.0, 140.0))
        .min_width(320.0)
        .order(DIALOG_ORDER)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.add_space(4.0);
            changed = gradient_editor(ui, &mut stops, true);
            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Reset").clicked() {
                    stops = vec![(0.0, [0, 0, 0, 255]), (1.0, [255, 255, 255, 255])];
                    changed = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Done").clicked() {
                        actions.tool.toggle_gradient_editor = Some(false);
                    }
                });
            });
            ui.add_space(2.0);
        });
    if changed {
        actions.tool.set_gradient_stops = Some(stops);
    }
    if !open {
        actions.tool.toggle_gradient_editor = Some(false);
    }
}

/// Interactive tone-curve editor (standard raster editors "Curves").
///
/// Points are stored normalised in [0,1]×[0,1] (input → output), sorted
/// ascending by x — the same representation `AdjustmentType::Curves` feeds to the
/// CPU `apply_pixel` (monotone cubic Hermite via `curves_eval`). The first/last points
/// are pinned to x=0 / x=1 (only their output moves); interior points slide
/// freely between their neighbours. Click empty space to add a point, drag a
/// point to move it, right-click an interior point to delete it. Returns true
/// when the curve changed this frame.
pub(crate) fn curves_editor_ui(
    ui: &mut egui::Ui,
    data: &UiData,
    histograms: &[[u32; 256]; 4],
    channels: &mut [Vec<(f32, f32)>; 4],
    options: AdjustmentOptions,
    preview_enabled: bool,
    armed_eyedropper: Option<AdjEyedropperKind>,
    apply: &mut bool,
    cancel: &mut bool,
    actions: &mut UiActions,
) -> bool {
    let mut changed = false;
    // Same CMYK policy as the Levels editor: slots read `[C, M, Y, K]`,
    // RGB-semantic presets/Auto/eyedroppers are hidden.
    let cmyk = data.doc.is_cmyk;
    let selected_id = ui.make_persistent_id("curves_channel_selected");
    let mut selected = ui
        .ctx()
        .data(|data| data.get_temp::<usize>(selected_id))
        .unwrap_or(0)
        .min(3);

    ui.horizontal_top(|ui| {
        ui.add_space(14.0);
        ui.vertical(|ui| {
            ui.set_width(318.0);
            egui::Grid::new("curves_header_grid")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    if !cmyk {
                        ui.label("Preset:");
                        egui::ComboBox::from_id_salt("curves_preset")
                            .selected_text("Default")
                            .width(206.0)
                            .show_ui(ui, |ui| {
                                for (name, preset) in builtin_curves_presets() {
                                    if ui.selectable_label(false, name).clicked() {
                                        *channels = preset;
                                        changed = true;
                                    }
                                }
                                let user = &data.dialogs.adjustment_presets.curves;
                                if !user.is_empty() {
                                    ui.separator();
                                    let mut del_idx: Option<usize> = None;
                                    for (i, preset) in user.iter().enumerate() {
                                        ui.horizontal(|ui| {
                                            if ui.selectable_label(false, &preset.name).clicked() {
                                                *channels = preset.channels.clone();
                                                changed = true;
                                            }
                                            if ui
                                                .small_button("×")
                                                .on_hover_text("Delete preset")
                                                .clicked()
                                            {
                                                del_idx = Some(i);
                                            }
                                        });
                                    }
                                    if let Some(i) = del_idx {
                                        actions.dialogs.delete_curves_preset = Some(i);
                                    }
                                }
                            });
                        ui.end_row();

                        ui.label("Save as:");
                        if let Some(name) = preset_save_row(ui, "curves_preset_name") {
                            actions.dialogs.save_curves_preset = Some((name, channels.clone()));
                        }
                        ui.end_row();
                    }

                    ui.label("Channel:");
                    ui.horizontal(|ui| {
                        let labels = if cmyk {
                            ["C", "M", "Y", "K"]
                        } else {
                            ["RGB", "R", "G", "B"]
                        };
                        for (idx, label) in labels.iter().enumerate() {
                            if ui.selectable_label(selected == idx, *label).clicked() {
                                selected = idx;
                            }
                        }
                    });
                    ui.end_row();
                });

            ui.ctx()
                .data_mut(|data| data.insert_temp(selected_id, selected));
            let active_id = ui.make_persistent_id(("curves_active_point", selected));
            let histogram = levels_histogram_for_channel(histograms, selected, cmyk);
            changed |= curve_graph_ui(
                ui,
                histogram,
                &mut channels[selected],
                curve_histogram_color(selected, cmyk),
                levels_channel_color(selected, cmyk),
                active_id,
            );
            changed |= curves_point_values_ui(ui, &mut channels[selected], active_id);

            ui.add_space(6.0);
            if ui.button("Reset Channel").clicked() {
                channels[selected] = crate::core::layer::identity_curve();
                changed = true;
            }
        });

        ui.add_space(18.0);
        ui.vertical(|ui| {
            ui.set_width(90.0);
            if levels_side_button(ui, "OK").clicked() {
                *apply = true;
            }
            if levels_side_button(ui, "Cancel").clicked() {
                *cancel = true;
            }
            if !cmyk {
                if levels_side_button(ui, "Auto").clicked() {
                    changed |= apply_auto_curves_to_channels(channels, histograms, options);
                }
                levels_options_menu(ui, options, actions);

                ui.add_space(22.0);
                adjustment_eyedropper_buttons(ui, armed_eyedropper, actions);
            }
            ui.add_space(8.0);
            let mut preview = preview_enabled;
            if ui.checkbox(&mut preview, "Preview").changed() {
                actions.dialogs.set_adjustment_preview_enabled = Some(preview);
            }
        });
    });

    changed
}

pub(crate) fn curve_histogram_color(idx: usize, cmyk: bool) -> egui::Color32 {
    if cmyk {
        let c = levels_channel_color(idx, true);
        return egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 90);
    }
    match idx {
        1 => egui::Color32::from_rgba_unmultiplied(235, 90, 86, 90),
        2 => egui::Color32::from_rgba_unmultiplied(78, 205, 110, 90),
        3 => egui::Color32::from_rgba_unmultiplied(104, 150, 255, 90),
        _ => egui::Color32::from_rgba_unmultiplied(190, 190, 190, 78),
    }
}

pub(crate) fn builtin_curves_presets() -> Vec<(&'static str, [Vec<(f32, f32)>; 4])> {
    vec![
        (
            "Default",
            std::array::from_fn(|_| crate::core::layer::identity_curve()),
        ),
        (
            "Linear Contrast",
            curves_preset_from_master(vec![(0.0, 0.0), (0.25, 0.18), (0.75, 0.82), (1.0, 1.0)]),
        ),
        (
            "Strong Contrast",
            curves_preset_from_master(vec![(0.0, 0.0), (0.25, 0.10), (0.75, 0.90), (1.0, 1.0)]),
        ),
        (
            "Negative",
            curves_preset_from_master(vec![(0.0, 1.0), (1.0, 0.0)]),
        ),
        (
            "Cross Process",
            [
                crate::core::layer::identity_curve(),
                vec![(0.0, 0.03), (0.35, 0.30), (1.0, 0.96)],
                vec![(0.0, 0.0), (0.55, 0.62), (1.0, 1.0)],
                vec![(0.0, 0.08), (0.55, 0.48), (1.0, 0.92)],
            ],
        ),
    ]
}

pub(crate) fn curves_preset_from_master(master: Vec<(f32, f32)>) -> [Vec<(f32, f32)>; 4] {
    let mut channels: [Vec<(f32, f32)>; 4] =
        std::array::from_fn(|_| crate::core::layer::identity_curve());
    channels[0] = master;
    channels
}

pub(crate) fn apply_auto_curves_to_channels(
    channels: &mut [Vec<(f32, f32)>; 4],
    histograms: &[[u32; 256]; 4],
    options: AdjustmentOptions,
) -> bool {
    let Some(levels) = auto_levels_channels_from_histogram(histograms, options) else {
        return false;
    };
    let next: [Vec<(f32, f32)>; 4] = std::array::from_fn(|i| curve_from_levels_params(&levels[i]));
    if *channels == next {
        return false;
    }
    *channels = next;
    true
}

pub(crate) fn curve_from_levels_params(params: &LevelsParams) -> Vec<(f32, f32)> {
    if params.is_identity() {
        return crate::core::layer::identity_curve();
    }
    let ib = params.in_black as f32 / 255.0;
    let iw = params.in_white as f32 / 255.0;
    let ob = params.out_black as f32 / 255.0;
    let ow = params.out_white as f32 / 255.0;
    let mut points = Vec::with_capacity(4);
    push_curve_point(&mut points, 0.0, ob);
    push_curve_point(&mut points, ib, ob);
    push_curve_point(&mut points, iw, ow);
    push_curve_point(&mut points, 1.0, ow);
    points
}

pub(crate) fn push_curve_point(points: &mut Vec<(f32, f32)>, x: f32, y: f32) {
    let x = x.clamp(0.0, 1.0);
    let y = y.clamp(0.0, 1.0);
    if let Some(last) = points.last_mut() {
        if (last.0 - x).abs() < 1e-6 {
            last.1 = y;
            return;
        }
    }
    points.push((x, y));
}

pub(crate) fn curves_point_values_ui(
    ui: &mut egui::Ui,
    points: &mut Vec<(f32, f32)>,
    active_id: egui::Id,
) -> bool {
    let active = ui
        .memory(|m| m.data.get_temp::<Option<usize>>(active_id))
        .flatten()
        .filter(|idx| *idx < points.len());
    let mut changed = false;

    ui.horizontal(|ui| {
        ui.label("Input");
        if let Some(idx) = active {
            let last = points.len().saturating_sub(1);
            let mut input = points[idx].0 * 255.0;
            let input_enabled = idx != 0 && idx != last;
            if ui
                .add_enabled(
                    input_enabled,
                    egui::DragValue::new(&mut input)
                        .range(0.0..=255.0)
                        .speed(1.0),
                )
                .changed()
            {
                let lo = points[idx - 1].0 + 0.001;
                let hi = points[idx + 1].0 - 0.001;
                points[idx].0 = (input / 255.0).clamp(lo, hi);
                changed = true;
            }
        } else {
            let mut input = 0.0;
            ui.add_enabled(false, egui::DragValue::new(&mut input));
        }

        ui.label("Output");
        if let Some(idx) = active {
            let mut output = points[idx].1 * 255.0;
            if ui
                .add(
                    egui::DragValue::new(&mut output)
                        .range(0.0..=255.0)
                        .speed(1.0),
                )
                .changed()
            {
                points[idx].1 = (output / 255.0).clamp(0.0, 1.0);
                changed = true;
            }
        } else {
            let mut output = 0.0;
            ui.add_enabled(false, egui::DragValue::new(&mut output));
        }
    });

    changed
}

pub(crate) fn curve_graph_ui(
    ui: &mut egui::Ui,
    histogram: &[u32],
    points: &mut Vec<(f32, f32)>,
    histogram_color: egui::Color32,
    curve_color: egui::Color32,
    active_id: egui::Id,
) -> bool {
    use egui::{Color32, Pos2, Sense, Stroke, Vec2};
    let mut changed = false;

    if points.len() < 2 {
        *points = vec![(0.0, 0.0), (1.0, 1.0)];
        changed = true;
    }
    points.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    if let Some(first) = points.first_mut() {
        if first.0 != 0.0 {
            first.0 = 0.0;
            changed = true;
        }
    }
    if let Some(last) = points.last_mut() {
        if last.0 != 1.0 {
            last.0 = 1.0;
            changed = true;
        }
    }

    let size = 256.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click_and_drag());
    let painter = ui.painter_at(rect);

    painter.rect_filled(rect, 0.0, Color32::from_gray(28));

    if histogram.len() >= 2 {
        let max = histogram.iter().copied().max().unwrap_or(1).max(1) as f32;
        let n = histogram.len() as f32;
        for (i, &h) in histogram.iter().enumerate() {
            if h == 0 {
                continue;
            }
            let x = rect.left() + (i as f32 / (n - 1.0)) * size;
            let bh = (h as f32 / max).sqrt() * size;
            painter.line_segment(
                [
                    Pos2::new(x, rect.bottom()),
                    Pos2::new(x, rect.bottom() - bh),
                ],
                Stroke::new(1.0_f32, histogram_color),
            );
        }
    }

    let grid = Stroke::new(1.0_f32, Color32::from_gray(48));
    for k in 1..4 {
        let t = k as f32 / 4.0;
        let gx = rect.left() + t * size;
        let gy = rect.top() + t * size;
        painter.line_segment(
            [Pos2::new(gx, rect.top()), Pos2::new(gx, rect.bottom())],
            grid,
        );
        painter.line_segment(
            [Pos2::new(rect.left(), gy), Pos2::new(rect.right(), gy)],
            grid,
        );
    }
    painter.line_segment(
        [
            Pos2::new(rect.left(), rect.bottom()),
            Pos2::new(rect.right(), rect.top()),
        ],
        Stroke::new(1.0_f32, Color32::from_gray(58)),
    );
    let border = Stroke::new(1.0_f32, Color32::from_gray(90));
    painter.line_segment([rect.left_top(), rect.right_top()], border);
    painter.line_segment([rect.right_top(), rect.right_bottom()], border);
    painter.line_segment([rect.right_bottom(), rect.left_bottom()], border);
    painter.line_segment([rect.left_bottom(), rect.left_top()], border);

    let to_screen = |p: (f32, f32)| Pos2::new(rect.left() + p.0 * size, rect.bottom() - p.1 * size);
    let to_norm = |pos: Pos2| {
        (
            ((pos.x - rect.left()) / size).clamp(0.0, 1.0),
            ((rect.bottom() - pos.y) / size).clamp(0.0, 1.0),
        )
    };
    let nearest = |pos: Pos2, points: &[(f32, f32)]| -> Option<usize> {
        let mut best: Option<usize> = None;
        let mut best_d = 11.0;
        for (i, p) in points.iter().enumerate() {
            let d = to_screen(*p).distance(pos);
            if d < best_d {
                best_d = d;
                best = Some(i);
            }
        }
        best
    };

    let mut active: Option<usize> = ui
        .memory(|m| m.data.get_temp::<Option<usize>>(active_id))
        .flatten();

    if response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            active = nearest(pos, points);
            ui.memory_mut(|m| m.data.insert_temp(active_id, active));
        }
    }

    if response.drag_started() {
        if let Some(pos) = response.interact_pointer_pos() {
            active = nearest(pos, points);
            if active.is_none() {
                let (nx, ny) = to_norm(pos);
                let nx = nx.clamp(0.001, 0.999);
                let ins = points.iter().position(|p| p.0 > nx).unwrap_or(points.len());
                points.insert(ins, (nx, ny));
                active = Some(ins);
                changed = true;
            }
            ui.memory_mut(|m| m.data.insert_temp(active_id, active));
        }
    }

    if response.dragged() {
        if let (Some(i), Some(pos)) = (active, response.interact_pointer_pos()) {
            let (mut nx, ny) = to_norm(pos);
            let last = points.len() - 1;
            if i == 0 {
                nx = 0.0;
            } else if i == last {
                nx = 1.0;
            } else {
                let lo = points[i - 1].0 + 0.001;
                let hi = points[i + 1].0 - 0.001;
                nx = nx.clamp(lo, hi);
            }
            points[i] = (nx, ny);
            changed = true;
        }
    }

    if response.drag_stopped() {
        ui.memory_mut(|m| m.data.insert_temp(active_id, active));
    }

    if response.secondary_clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            if let Some(i) = nearest(pos, points) {
                if i != 0 && i != points.len() - 1 {
                    points.remove(i);
                    changed = true;
                }
            }
        }
    }

    let mut poly = Vec::with_capacity(129);
    for k in 0..=128 {
        let x = k as f32 / 128.0;
        let y = crate::core::layer::curves_eval(points, x);
        poly.push(to_screen((x, y)));
    }
    painter.add(egui::Shape::line(poly, Stroke::new(1.8_f32, curve_color)));

    for (i, p) in points.iter().enumerate() {
        let sp = to_screen(*p);
        let r = if active == Some(i) { 5.0 } else { 4.0 };
        painter.circle_filled(sp, r, Color32::WHITE);
        painter.circle_stroke(sp, r, Stroke::new(1.0_f32, Color32::from_gray(40)));
    }

    changed
}

pub(crate) fn levels_editor_ui(
    ui: &mut egui::Ui,
    data: &UiData,
    histograms: &[[u32; 256]; 4],
    channels: &mut [LevelsParams; 4],
    options: AdjustmentOptions,
    preview_enabled: bool,
    armed_eyedropper: Option<AdjEyedropperKind>,
    apply: &mut bool,
    cancel: &mut bool,
    actions: &mut UiActions,
) -> bool {
    let mut changed = false;
    // Presets/Auto/eyedroppers carry RGB semantics ([master,r,g,b] slots,
    // luma-derived stretch, RGB point picks) — hidden on a CMYK document,
    // where the slots read `[C, M, Y, K]` ink coverage.
    let cmyk = data.doc.is_cmyk;
    let selected_id = ui.make_persistent_id("levels_channel_selected");
    let mut selected = ui
        .ctx()
        .data(|data| data.get_temp::<usize>(selected_id))
        .unwrap_or(0)
        .min(3);

    ui.horizontal_top(|ui| {
        ui.add_space(14.0);
        ui.vertical(|ui| {
            ui.set_width(318.0);
            egui::Grid::new("levels_header_grid")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    if !cmyk {
                        ui.label("Preset:");
                        egui::ComboBox::from_id_salt("levels_preset")
                            .selected_text("Default")
                            .width(206.0)
                            .show_ui(ui, |ui| {
                                for (name, preset) in builtin_levels_presets() {
                                    if ui.selectable_label(false, name).clicked() {
                                        *channels = preset;
                                        changed = true;
                                    }
                                }
                                let user = &data.dialogs.adjustment_presets.levels;
                                if !user.is_empty() {
                                    ui.separator();
                                    let mut del_idx: Option<usize> = None;
                                    for (i, preset) in user.iter().enumerate() {
                                        ui.horizontal(|ui| {
                                            if ui.selectable_label(false, &preset.name).clicked() {
                                                *channels = preset.channels;
                                                changed = true;
                                            }
                                            if ui
                                                .small_button("×")
                                                .on_hover_text("Delete preset")
                                                .clicked()
                                            {
                                                del_idx = Some(i);
                                            }
                                        });
                                    }
                                    if let Some(i) = del_idx {
                                        actions.dialogs.delete_levels_preset = Some(i);
                                    }
                                }
                            });
                        ui.end_row();

                        ui.label("Save as:");
                        if let Some(name) = preset_save_row(ui, "levels_preset_name") {
                            actions.dialogs.save_levels_preset = Some((name, *channels));
                        }
                        ui.end_row();
                    }

                    ui.label("Channel:");
                    egui::ComboBox::from_id_salt("levels_channel")
                        .selected_text(levels_channel_label(selected, cmyk))
                        .width(132.0)
                        .show_ui(ui, |ui| {
                            for idx in 0..4 {
                                ui.selectable_value(
                                    &mut selected,
                                    idx,
                                    levels_channel_label(idx, cmyk),
                                );
                            }
                        });
                    ui.end_row();
                });

            ui.ctx()
                .data_mut(|data| data.insert_temp(selected_id, selected));
            let histogram = levels_histogram_for_channel(histograms, selected, cmyk);
            let params = &mut channels[selected];

            ui.add_space(4.0);
            ui.label("Input Levels:");
            changed |= levels_input_graph(
                ui,
                histogram,
                levels_channel_color(selected, cmyk),
                &mut params.in_black,
                &mut params.in_white,
                &mut params.gamma,
            );
            changed |= levels_input_values(
                ui,
                &mut params.in_black,
                &mut params.gamma,
                &mut params.in_white,
            );

            ui.add_space(10.0);
            ui.label("Output Levels:");
            changed |= levels_output_bar(ui, &mut params.out_black, &mut params.out_white);
            changed |= levels_output_values(ui, &mut params.out_black, &mut params.out_white);
        });

        ui.add_space(18.0);
        ui.vertical(|ui| {
            ui.set_width(90.0);
            if levels_side_button(ui, "OK").clicked() {
                *apply = true;
            }
            if levels_side_button(ui, "Cancel").clicked() {
                *cancel = true;
            }
            if !cmyk {
                if levels_side_button(ui, "Auto").clicked() {
                    changed |= apply_auto_levels_to_channels(channels, histograms, options);
                }
                levels_options_menu(ui, options, actions);

                ui.add_space(22.0);
                adjustment_eyedropper_buttons(ui, armed_eyedropper, actions);
            }
            ui.add_space(8.0);
            let mut preview = preview_enabled;
            if ui.checkbox(&mut preview, "Preview").changed() {
                actions.dialogs.set_adjustment_preview_enabled = Some(preview);
            }
        });
    });

    changed
}

pub(crate) fn builtin_levels_presets() -> Vec<(&'static str, [LevelsParams; 4])> {
    let mut darker = [LevelsParams::default(); 4];
    darker[0].gamma = 0.80;
    let mut lighter = [LevelsParams::default(); 4];
    lighter[0].gamma = 1.25;
    let mut contrast = [LevelsParams::default(); 4];
    contrast[0].in_black = 16;
    contrast[0].in_white = 239;
    vec![
        ("Default", [LevelsParams::default(); 4]),
        ("Darker", darker),
        ("Lighter", lighter),
        ("Increase Contrast", contrast),
    ]
}

pub(crate) fn levels_channel_label(idx: usize, cmyk: bool) -> &'static str {
    if cmyk {
        return match idx {
            1 => "Magenta",
            2 => "Yellow",
            3 => "Black",
            _ => "Cyan",
        };
    }
    match idx {
        1 => "Red",
        2 => "Green",
        3 => "Blue",
        _ => "RGB",
    }
}

pub(crate) fn levels_channel_color(idx: usize, cmyk: bool) -> egui::Color32 {
    if cmyk {
        return match idx {
            1 => egui::Color32::from_rgb(236, 60, 140),
            2 => egui::Color32::from_rgb(230, 200, 40),
            3 => egui::Color32::from_rgb(190, 190, 190),
            _ => egui::Color32::from_rgb(60, 174, 239),
        };
    }
    match idx {
        1 => egui::Color32::from_rgb(235, 90, 86),
        2 => egui::Color32::from_rgb(78, 205, 110),
        3 => egui::Color32::from_rgb(104, 150, 255),
        _ => egui::Color32::from_rgb(214, 214, 214),
    }
}

pub(crate) fn levels_histogram_for_channel(
    histograms: &[[u32; 256]; 4],
    idx: usize,
    cmyk: bool,
) -> &[u32; 256] {
    // CMYK histograms are stored `[C, M, Y, K]`, matching the channel slots.
    if cmyk {
        return &histograms[idx.min(3)];
    }
    match idx {
        1 => &histograms[0],
        2 => &histograms[1],
        3 => &histograms[2],
        _ => &histograms[3],
    }
}

pub(crate) fn normalize_levels_channels(channels: &mut [LevelsParams; 4]) -> bool {
    let mut changed = false;
    for params in channels {
        if params.in_black >= params.in_white {
            let (black, white) = if params.in_black == 255 {
                (254, 255)
            } else {
                (params.in_black, params.in_black + 1)
            };
            changed |= params.in_black != black || params.in_white != white;
            params.in_black = black;
            params.in_white = white;
        }
        if params.out_black >= params.out_white {
            let (black, white) = if params.out_black == 255 {
                (254, 255)
            } else {
                (params.out_black, params.out_black + 1)
            };
            changed |= params.out_black != black || params.out_white != white;
            params.out_black = black;
            params.out_white = white;
        }
    }
    changed
}

pub(crate) fn apply_auto_levels_to_channels(
    channels: &mut [LevelsParams; 4],
    histograms: &[[u32; 256]; 4],
    options: AdjustmentOptions,
) -> bool {
    let Some(next) = auto_levels_channels_from_histogram(histograms, options) else {
        return false;
    };
    if *channels == next {
        return false;
    }
    *channels = next;
    true
}

/// Preset-name TextEdit + Save button (one grid cell). Returns the trimmed
/// name when Save is clicked with a non-empty name; the field is then cleared.
/// The draft name lives in egui temp data under `id` so it survives reruns.
pub(crate) fn preset_save_row(ui: &mut egui::Ui, id: &str) -> Option<String> {
    let store_id = egui::Id::new(id);
    let mut name = ui
        .ctx()
        .data_mut(|d| d.get_temp::<String>(store_id).unwrap_or_default());
    let mut saved = None;
    ui.horizontal(|ui| {
        let resp = ui.add(
            egui::TextEdit::singleline(&mut name)
                .desired_width(150.0)
                .hint_text("Preset name…"),
        );
        if resp.changed() {
            ui.ctx().data_mut(|d| d.insert_temp(store_id, name.clone()));
        }
        if ui.button("Save").clicked() && !name.trim().is_empty() {
            saved = Some(name.trim().to_string());
            ui.ctx()
                .data_mut(|d| d.insert_temp(store_id, String::new()));
        }
    });
    saved
}

pub(crate) fn levels_options_menu(
    ui: &mut egui::Ui,
    options: AdjustmentOptions,
    actions: &mut UiActions,
) {
    let mut next = options;
    ui.menu_button("Options...", |ui| {
        ui.set_min_width(210.0);
        ui.label(egui::RichText::new("Auto Correction").strong());
        let mut changed = false;
        changed |= ui
            .selectable_value(
                &mut next.auto_levels_algorithm,
                AutoLevelsAlgorithm::PerChannelContrast,
                "Per-Channel Contrast",
            )
            .changed();
        changed |= ui
            .selectable_value(
                &mut next.auto_levels_algorithm,
                AutoLevelsAlgorithm::Monochromatic,
                "Monochromatic",
            )
            .changed();
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Clip");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut next.auto_clip_percent)
                        .range(0.0..=10.0)
                        .speed(0.01)
                        .suffix("%"),
                )
                .changed();
        });
        if changed {
            actions.dialogs.set_adjustment_options = Some(sanitize_adjustment_options(next));
        }
    });
}

pub(crate) fn adjustment_eyedropper_buttons(
    ui: &mut egui::Ui,
    armed: Option<AdjEyedropperKind>,
    actions: &mut UiActions,
) {
    ui.horizontal(|ui| {
        for (kind, label, tip) in [
            (AdjEyedropperKind::Black, "B", "Set black point"),
            (AdjEyedropperKind::Gray, "G", "Set gray point"),
            (AdjEyedropperKind::White, "W", "Set white point"),
        ] {
            let selected = armed == Some(kind);
            if ui
                .selectable_label(selected, label)
                .on_hover_text(tip)
                .clicked()
            {
                actions.dialogs.set_adj_eyedropper = Some(if selected { None } else { Some(kind) });
            }
        }
    });
}

pub(crate) fn levels_side_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add_sized([90.0, 24.0], egui::Button::new(text))
}

pub(crate) fn levels_input_graph(
    ui: &mut egui::Ui,
    histogram: &[u32],
    histogram_color: egui::Color32,
    in_black: &mut u8,
    in_white: &mut u8,
    gamma: &mut f32,
) -> bool {
    let size = egui::vec2(260.0, 100.0);
    let (graph_rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let handle_y = graph_rect.bottom() + 7.0;
    let handle_rect = egui::Rect::from_min_max(
        egui::pos2(graph_rect.left() - 8.0, graph_rect.bottom()),
        egui::pos2(graph_rect.right() + 8.0, graph_rect.bottom() + 16.0),
    );
    ui.allocate_rect(handle_rect, egui::Sense::hover());

    let ib_x = level_to_x(graph_rect, *in_black);
    let iw_x = level_to_x(graph_rect, *in_white);
    let gamma_x = gamma_to_x(graph_rect, *in_black, *in_white, *gamma);

    let mut changed = false;
    let black_resp = ui.interact(
        egui::Rect::from_center_size(egui::pos2(ib_x, handle_y), egui::vec2(18.0, 16.0)),
        ui.id().with("levels_in_black_handle"),
        egui::Sense::click_and_drag(),
    );
    let gamma_resp = ui.interact(
        egui::Rect::from_center_size(egui::pos2(gamma_x, handle_y), egui::vec2(18.0, 16.0)),
        ui.id().with("levels_gamma_handle"),
        egui::Sense::click_and_drag(),
    );
    let white_resp = ui.interact(
        egui::Rect::from_center_size(egui::pos2(iw_x, handle_y), egui::vec2(18.0, 16.0)),
        ui.id().with("levels_in_white_handle"),
        egui::Sense::click_and_drag(),
    );

    if black_resp.dragged() || black_resp.clicked() {
        if let Some(pos) = black_resp.interact_pointer_pos() {
            let v = x_to_level(graph_rect, pos.x).min(in_white.saturating_sub(1));
            if *in_black != v {
                *in_black = v;
                changed = true;
            }
        }
    }
    if white_resp.dragged() || white_resp.clicked() {
        if let Some(pos) = white_resp.interact_pointer_pos() {
            let v = x_to_level(graph_rect, pos.x).max(in_black.saturating_add(1));
            if *in_white != v {
                *in_white = v;
                changed = true;
            }
        }
    }
    if gamma_resp.dragged() || gamma_resp.clicked() {
        if let Some(pos) = gamma_resp.interact_pointer_pos() {
            let ib = *in_black as f32 / 255.0;
            let iw = *in_white as f32 / 255.0;
            let level = x_to_level(graph_rect, pos.x) as f32 / 255.0;
            let mid = ((level - ib) / (iw - ib).max(0.001)).clamp(0.02, 0.98);
            let next_gamma = (mid.ln() / 0.5_f32.ln()).clamp(0.10, 9.99);
            if (*gamma - next_gamma).abs() > 0.001 {
                *gamma = next_gamma;
                changed = true;
            }
        }
    }

    draw_histogram(ui, graph_rect, histogram, histogram_color);
    draw_level_triangle(
        ui,
        egui::pos2(level_to_x(graph_rect, *in_black), handle_y),
        true,
    );
    draw_level_triangle(
        ui,
        egui::pos2(
            gamma_to_x(graph_rect, *in_black, *in_white, *gamma),
            handle_y,
        ),
        false,
    );
    draw_level_triangle(
        ui,
        egui::pos2(level_to_x(graph_rect, *in_white), handle_y),
        false,
    );

    changed
}

pub(crate) fn levels_input_values(
    ui: &mut egui::Ui,
    in_black: &mut u8,
    gamma: &mut f32,
    in_white: &mut u8,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        changed |= ui
            .add_sized([44.0, 20.0], egui::DragValue::new(in_black).range(0..=254))
            .changed();
        ui.add_space(44.0);
        changed |= ui
            .add_sized(
                [52.0, 20.0],
                egui::DragValue::new(gamma).range(0.10..=9.99).speed(0.01),
            )
            .changed();
        ui.add_space(36.0);
        changed |= ui
            .add_sized([44.0, 20.0], egui::DragValue::new(in_white).range(1..=255))
            .changed();
    });
    changed
}

pub(crate) fn levels_output_bar(ui: &mut egui::Ui, out_black: &mut u8, out_white: &mut u8) -> bool {
    let bar_size = egui::vec2(260.0, 18.0);
    let handle_pad = 8.0;
    let total_size = egui::vec2(bar_size.x + handle_pad * 2.0, 40.0);
    let (full_rect, _) = ui.allocate_exact_size(total_size, egui::Sense::hover());
    let bar_rect = egui::Rect::from_min_size(full_rect.min + egui::vec2(handle_pad, 0.0), bar_size);
    draw_levels_gradient(ui, bar_rect);

    let handle_y = bar_rect.bottom() + 6.0;

    let black_x = level_to_x(bar_rect, *out_black);
    let white_x = level_to_x(bar_rect, *out_white);
    let black_resp = ui.interact(
        egui::Rect::from_center_size(egui::pos2(black_x, handle_y), egui::vec2(18.0, 16.0)),
        ui.id().with("levels_out_black_handle"),
        egui::Sense::click_and_drag(),
    );
    let white_resp = ui.interact(
        egui::Rect::from_center_size(egui::pos2(white_x, handle_y), egui::vec2(18.0, 16.0)),
        ui.id().with("levels_out_white_handle"),
        egui::Sense::click_and_drag(),
    );

    let mut changed = false;
    if black_resp.dragged() || black_resp.clicked() {
        if let Some(pos) = black_resp.interact_pointer_pos() {
            let v = x_to_level(bar_rect, pos.x).min(out_white.saturating_sub(1));
            if *out_black != v {
                *out_black = v;
                changed = true;
            }
        }
    }
    if white_resp.dragged() || white_resp.clicked() {
        if let Some(pos) = white_resp.interact_pointer_pos() {
            let v = x_to_level(bar_rect, pos.x).max(out_black.saturating_add(1));
            if *out_white != v {
                *out_white = v;
                changed = true;
            }
        }
    }

    draw_level_triangle(
        ui,
        egui::pos2(level_to_x(bar_rect, *out_black), handle_y),
        true,
    );
    draw_level_triangle(
        ui,
        egui::pos2(level_to_x(bar_rect, *out_white), handle_y),
        false,
    );
    changed
}

pub(crate) fn levels_output_values(
    ui: &mut egui::Ui,
    out_black: &mut u8,
    out_white: &mut u8,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        changed |= ui
            .add_sized([44.0, 20.0], egui::DragValue::new(out_black).range(0..=254))
            .changed();
        ui.add_space(136.0);
        changed |= ui
            .add_sized([44.0, 20.0], egui::DragValue::new(out_white).range(1..=255))
            .changed();
    });
    changed
}

pub(crate) fn draw_histogram(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    histogram: &[u32],
    bar_color: egui::Color32,
) {
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(61, 61, 61));
    painter.rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(46, 46, 46)),
        egui::StrokeKind::Inside,
    );

    let max = histogram.iter().copied().max().unwrap_or(0);
    if max == 0 {
        return;
    }

    for i in 0..256 {
        let count = histogram.get(i).copied().unwrap_or(0);
        if count == 0 {
            continue;
        }
        let t0 = i as f32 / 256.0;
        let t1 = (i + 1) as f32 / 256.0;
        let x0 = egui::lerp(rect.left()..=rect.right(), t0);
        let x1 = egui::lerp(rect.left()..=rect.right(), t1).max(x0 + 1.0);
        let h = rect.height() * (count as f32 / max as f32).sqrt();
        let r = egui::Rect::from_min_max(
            egui::pos2(x0, rect.bottom() - h),
            egui::pos2(x1, rect.bottom()),
        );
        painter.rect_filled(r, 0.0, bar_color);
    }
}

pub(crate) fn draw_levels_gradient(ui: &mut egui::Ui, rect: egui::Rect) {
    let steps = 128;
    for i in 0..steps {
        let t0 = i as f32 / steps as f32;
        let t1 = (i + 1) as f32 / steps as f32;
        let x0 = egui::lerp(rect.left()..=rect.right(), t0);
        let x1 = egui::lerp(rect.left()..=rect.right(), t1);
        let gray = (t0 * 255.0).round() as u8;
        let r = egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom()));
        ui.painter()
            .rect_filled(r, 0.0, egui::Color32::from_gray(gray));
    }
    ui.painter().rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(42, 42, 42)),
        egui::StrokeKind::Inside,
    );
}

pub(crate) fn draw_level_triangle(ui: &mut egui::Ui, center: egui::Pos2, dark: bool) {
    let fill = if dark {
        egui::Color32::from_rgb(32, 32, 32)
    } else {
        egui::Color32::from_rgb(202, 202, 202)
    };
    let stroke = egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(18, 18, 18));
    let points = vec![
        egui::pos2(center.x, center.y - 6.0),
        egui::pos2(center.x - 6.0, center.y + 5.0),
        egui::pos2(center.x + 6.0, center.y + 5.0),
    ];
    ui.painter()
        .add(egui::Shape::convex_polygon(points, fill, stroke));
}

pub(crate) fn level_to_x(rect: egui::Rect, value: u8) -> f32 {
    egui::lerp(rect.left()..=rect.right(), value as f32 / 255.0)
}

pub(crate) fn x_to_level(rect: egui::Rect, x: f32) -> u8 {
    let t = ((x - rect.left()) / rect.width()).clamp(0.0, 1.0);
    (t * 255.0).round() as u8
}

pub(crate) fn gamma_to_x(rect: egui::Rect, in_black: u8, in_white: u8, gamma: f32) -> f32 {
    let ib = in_black as f32 / 255.0;
    let iw = in_white as f32 / 255.0;
    let mid = 0.5_f32.powf(gamma.clamp(0.10, 9.99));
    egui::lerp(rect.left()..=rect.right(), ib + (iw - ib).max(0.001) * mid)
}

pub(crate) fn auto_levels_from_histogram_with_clip(
    histogram: &[u32],
    clip_percent: f32,
) -> Option<(u8, u8)> {
    let total: u32 = histogram.iter().sum();
    if total == 0 {
        return None;
    }
    let clip = (total as f32 * (clip_percent.clamp(0.0, 10.0) / 100.0)).round() as u32;
    let mut acc = 0_u32;
    let mut black = 0_u8;
    for (i, count) in histogram.iter().copied().enumerate() {
        acc = acc.saturating_add(count);
        if acc > clip {
            black = i as u8;
            break;
        }
    }

    acc = 0;
    let mut white = 255_u8;
    for (i, count) in histogram.iter().copied().enumerate().rev() {
        acc = acc.saturating_add(count);
        if acc > clip {
            white = i as u8;
            break;
        }
    }

    (black < white).then_some((black, white))
}

pub(crate) fn auto_levels_channels_from_histogram(
    histograms: &[[u32; 256]; 4],
    options: AdjustmentOptions,
) -> Option<[LevelsParams; 4]> {
    let options = sanitize_adjustment_options(options);
    let mut channels = [LevelsParams::default(); 4];
    match options.auto_levels_algorithm {
        AutoLevelsAlgorithm::Monochromatic => {
            let (black, white) =
                auto_levels_from_histogram_with_clip(&histograms[3], options.auto_clip_percent)?;
            channels[0].in_black = black;
            channels[0].in_white = white;
        }
        AutoLevelsAlgorithm::PerChannelContrast => {
            let mut any = false;
            for (plane, channel) in [(0, 1), (1, 2), (2, 3)] {
                if let Some((black, white)) = auto_levels_from_histogram_with_clip(
                    &histograms[plane],
                    options.auto_clip_percent,
                ) {
                    channels[channel].in_black = black;
                    channels[channel].in_white = white;
                    any = true;
                }
            }
            if !any {
                return None;
            }
        }
    }
    Some(channels)
}

pub(crate) fn color_balance_editor_ui(
    ui: &mut egui::Ui,
    shadows: &mut [f32; 3],
    midtones: &mut [f32; 3],
    highlights: &mut [f32; 3],
    preserve_luminosity: &mut bool,
    apply: &mut bool,
    cancel: &mut bool,
) -> bool {
    let tone_id = ui.make_persistent_id("color_balance_tone");
    let mut tone = ui
        .data(|data| data.get_temp::<usize>(tone_id))
        .unwrap_or(1)
        .min(2);
    let mut changed = false;

    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(334.0);
            ui.group(|ui| {
                ui.set_width(318.0);
                ui.label("Color Balance");
                ui.add_space(4.0);
                {
                    let values = match tone {
                        0 => shadows,
                        2 => highlights,
                        _ => midtones,
                    };
                    changed |= color_balance_values_row(ui, values);
                    ui.add_space(5.0);
                    changed |= color_balance_slider_row(
                        ui,
                        "Cyan",
                        "Red",
                        &mut values[0],
                        &[
                            egui::Color32::from_rgb(0, 210, 220),
                            egui::Color32::from_gray(86),
                            egui::Color32::from_rgb(235, 55, 45),
                        ],
                    );
                    changed |= color_balance_slider_row(
                        ui,
                        "Magenta",
                        "Green",
                        &mut values[1],
                        &[
                            egui::Color32::from_rgb(220, 45, 210),
                            egui::Color32::from_gray(86),
                            egui::Color32::from_rgb(45, 210, 65),
                        ],
                    );
                    changed |= color_balance_slider_row(
                        ui,
                        "Yellow",
                        "Blue",
                        &mut values[2],
                        &[
                            egui::Color32::from_rgb(230, 215, 30),
                            egui::Color32::from_gray(86),
                            egui::Color32::from_rgb(45, 95, 235),
                        ],
                    );
                }
            });

            ui.add_space(8.0);
            ui.group(|ui| {
                ui.set_width(318.0);
                ui.label("Tone Balance");
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.radio_value(&mut tone, 0, "Shadows");
                    ui.add_space(18.0);
                    ui.radio_value(&mut tone, 1, "Midtones");
                    ui.add_space(18.0);
                    ui.radio_value(&mut tone, 2, "Highlights");
                });
                ui.add_space(5.0);
                changed |= ui
                    .checkbox(preserve_luminosity, "Preserve Luminosity")
                    .changed();
            });
        });

        ui.add_space(10.0);
        ui.vertical(|ui| {
            ui.set_width(82.0);
            if levels_side_button(ui, "OK").clicked() {
                *apply = true;
            }
            if levels_side_button(ui, "Cancel").clicked() {
                *cancel = true;
            }
            ui.add_space(12.0);
            let mut preview = true;
            let _ = ui.checkbox(&mut preview, "Preview");
        });
    });

    ui.data_mut(|data| data.insert_temp(tone_id, tone));
    changed
}

pub(crate) fn color_balance_values_row(ui: &mut egui::Ui, values: &mut [f32; 3]) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.add_sized([110.0, 20.0], egui::Label::new("Color Levels:"));
        for value in values.iter_mut() {
            let mut v = value.round() as i32;
            let response = ui.add_sized(
                [42.0, 20.0],
                egui::DragValue::new(&mut v).range(-100..=100).speed(1.0),
            );
            if response.changed() {
                *value = v.clamp(-100, 100) as f32;
                changed = true;
            }
            ui.add_space(6.0);
        }
    });
    changed
}

pub(crate) fn color_balance_slider_row(
    ui: &mut egui::Ui,
    left_label: &str,
    right_label: &str,
    value: &mut f32,
    colors: &[egui::Color32],
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.add_sized(
            [68.0, 18.0],
            egui::Label::new(left_label).halign(egui::Align::RIGHT),
        );
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(196.0, 18.0), egui::Sense::click_and_drag());
        if (response.dragged() || response.clicked()) && response.interact_pointer_pos().is_some() {
            let pos = response.interact_pointer_pos().unwrap();
            let t = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            let new_value = -100.0 + 200.0 * t;
            if (*value - new_value).abs() > f32::EPSILON {
                *value = new_value;
                changed = true;
            }
        }
        paint_gradient_slider(ui, rect, *value, -100.0, 100.0, colors);
        ui.add_sized([52.0, 18.0], egui::Label::new(right_label));
    });
    changed
}

pub(crate) fn color_slider_f32(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    label: &str,
    colors: &[egui::Color32],
) -> egui::Response {
    let min = *range.start();
    let max = *range.end();
    let mut changed = false;
    let mut response = ui
        .horizontal(|ui| {
            ui.add_sized([112.0, 18.0], egui::Label::new(label));
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(160.0, 18.0), egui::Sense::click_and_drag());
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
            ui.add_sized(
                [42.0, 18.0],
                egui::Label::new(format!("{:.0}", *value)).truncate(),
            );
            response
        })
        .inner;
    if changed {
        response.mark_changed();
    }
    response
}

pub(crate) fn paint_gradient_slider(
    ui: &egui::Ui,
    rect: egui::Rect,
    value: f32,
    min: f32,
    max: f32,
    colors: &[egui::Color32],
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
            egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgba_unmultiplied(240, 240, 240, 120),
            ),
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
        egui::Color32::from_gray(230),
        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(70)),
    ));
}

pub(crate) fn sample_gradient(colors: &[egui::Color32], t: f32) -> egui::Color32 {
    if colors.is_empty() {
        return egui::Color32::from_gray(128);
    }
    if colors.len() == 1 {
        return colors[0];
    }
    let scaled = t.clamp(0.0, 1.0) * (colors.len() - 1) as f32;
    let idx = scaled.floor() as usize;
    let next = (idx + 1).min(colors.len() - 1);
    let local = scaled - idx as f32;
    lerp_color(colors[idx], colors[next], local)
}

pub(crate) fn lerp_color(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let lerp = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    egui::Color32::from_rgba_premultiplied(
        lerp(a.r(), b.r()),
        lerp(a.g(), b.g()),
        lerp(a.b(), b.b()),
        lerp(a.a(), b.a()),
    )
}

pub(crate) fn hsv_to_color32(h: f32, s: f32, v: f32) -> egui::Color32 {
    let h = h.rem_euclid(1.0) * 6.0;
    let i = h.floor();
    let f = h - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    let (r, g, b) = match i as i32 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    egui::Color32::from_rgb(
        (r.clamp(0.0, 1.0) * 255.0).round() as u8,
        (g.clamp(0.0, 1.0) * 255.0).round() as u8,
        (b.clamp(0.0, 1.0) * 255.0).round() as u8,
    )
}

pub(crate) fn default_adjustment_like(adj: &AdjustmentType) -> AdjustmentType {
    match adj {
        AdjustmentType::Levels { .. } => AdjustmentType::default_levels(),
        AdjustmentType::HueSaturation { .. } => AdjustmentType::HueSaturation {
            hue: 0.0,
            saturation: 0.0,
            lightness: 0.0,
        },
        AdjustmentType::ColorBalance { .. } => AdjustmentType::ColorBalance {
            shadows: [0.0; 3],
            midtones: [0.0; 3],
            highlights: [0.0; 3],
            preserve_luminosity: true,
        },
        AdjustmentType::Curves { .. } => AdjustmentType::default_curves(),
        AdjustmentType::BrightnessContrast { .. } => AdjustmentType::BrightnessContrast {
            brightness: 0.0,
            contrast: 0.0,
        },
        AdjustmentType::Vibrance { .. } => AdjustmentType::Vibrance {
            vibrance: 0.0,
            saturation: 0.0,
        },
        AdjustmentType::Exposure { .. } => AdjustmentType::Exposure {
            exposure: 0.0,
            offset: 0.0,
            gamma: 1.0,
        },
        AdjustmentType::Threshold { .. } => AdjustmentType::Threshold { value: 128 },
        AdjustmentType::Posterize { .. } => AdjustmentType::Posterize { levels: 4 },
        AdjustmentType::BlackAndWhite { .. } => AdjustmentType::BlackAndWhite {
            r: 40.0,
            y: 60.0,
            g: 40.0,
            c: 60.0,
            b: 20.0,
            m: 80.0,
        },
        AdjustmentType::PhotoFilter { .. } => AdjustmentType::PhotoFilter {
            color: [236, 138, 0],
            density: 0.25,
            luminosity: true,
        },
        AdjustmentType::GradientMap { .. } => AdjustmentType::default_gradient_map(),
        AdjustmentType::ChannelMixer { .. } => AdjustmentType::ChannelMixer {
            red: [1.0, 0.0, 0.0],
            green: [0.0, 1.0, 0.0],
            blue: [0.0, 0.0, 1.0],
            monochrome: false,
        },
        other => other.clone(),
    }
}

//! Filter dialogs (standard + PS-style proxy preview) and Smart Fill.

use super::*;

pub(crate) fn default_filter_like(f: &FilterType) -> FilterType {
    match f {
        FilterType::GaussianBlur { .. } => FilterType::GaussianBlur { radius: 2.0 },
        FilterType::Sharpen { .. } => FilterType::Sharpen {
            amount: 1.0,
            radius: 2.0,
        },
        FilterType::HighPass { .. } => FilterType::HighPass { radius: 3.0 },
        FilterType::AddNoise { .. } => FilterType::AddNoise {
            amount: 25.0,
            monochromatic: false,
        },
        FilterType::Pixelate { .. } => FilterType::Pixelate { cell: 8.0 },
        FilterType::ReduceNoise { .. } => FilterType::ReduceNoise { strength: 50.0 },
    }
}

pub(crate) fn filter_dialog_std(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let esc_pressed = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    let enter_pressed = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
    if esc_pressed {
        actions.dialogs.cancel_filter_dialog = true;
    }

    let mut filter = data.dialogs.filter_dialog;
    let mut preview_enabled = data.dialogs.filter_preview_enabled;
    let mut value_changed = false;
    let mut commit_preview = false;
    let mut apply = enter_pressed;
    let mut cancel = false;
    let mut open = true;

    let default_pos = document_side_dialog_pos(ctx, data, 420.0, 96.0);
    egui::Window::new(filter.name())
        .collapsible(false)
        .resizable(false)
        .movable(true)
        .open(&mut open)
        .default_pos(default_pos)
        .min_width(420.0)
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    filter_proxy_preview_ps_ui(ui, data);
                    ui.add_space(6.0);
                    filter_controls_ps_ui(ui, &mut filter, &mut value_changed, &mut commit_preview);
                    if data.dialogs.filter_preview_processing {
                        ui.add_space(2.0);
                        ui.label(
                            egui::RichText::new("Rendering preview...")
                                .size(10.0)
                                .color(egui::Color32::from_gray(150)),
                        );
                    }
                });

                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.set_width(78.0);
                    if ui
                        .add_sized([76.0, 24.0], egui::Button::new("OK"))
                        .clicked()
                    {
                        apply = true;
                    }
                    if ui
                        .add_sized([76.0, 24.0], egui::Button::new("Cancel"))
                        .clicked()
                    {
                        cancel = true;
                    }
                    if ui.checkbox(&mut preview_enabled, "Preview").changed() {
                        actions.dialogs.set_filter_preview_enabled = Some(preview_enabled);
                        if preview_enabled {
                            actions.dialogs.apply_filter_preview = true;
                        }
                    }
                    ui.add_space(4.0);
                    if ui
                        .add_sized([76.0, 22.0], egui::Button::new("Reset"))
                        .clicked()
                    {
                        filter = default_filter_like(&filter);
                        value_changed = true;
                        commit_preview = preview_enabled;
                    }
                });
            });
        });

    if value_changed {
        actions.dialogs.set_filter_dialog = Some(filter);
    }
    if commit_preview {
        actions.dialogs.apply_filter_preview = true;
    }
    if apply {
        if value_changed {
            actions.dialogs.set_filter_dialog = Some(filter);
        }
        actions.dialogs.apply_filter_dialog = true;
    }
    if cancel || !open {
        actions.dialogs.cancel_filter_dialog = true;
    }
}

/// Filter dialog (Gaussian Blur / Sharpen) with a live preview on the active
/// layer. Mirrors `adjustment_dialog`: editing a slider sets `set_filter_dialog`
/// (re-applies the preview); OK commits undoably, Cancel/Esc restores.
pub(crate) fn filter_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    if data.dialogs.show_filter_dialog {
        filter_dialog_std(ctx, data, actions);
        return;
    }

    let esc_pressed = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    let enter_pressed = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
    if esc_pressed {
        actions.dialogs.cancel_filter_dialog = true;
    }

    let mut filter = data.dialogs.filter_dialog;
    let title = filter.name().to_string();
    let mut value_changed = false;
    let mut commit_preview = false;
    let mut apply = enter_pressed;
    let mut cancel = false;

    let debounced = |resp: &egui::Response, vc: &mut bool, cp: &mut bool| {
        if resp.changed() {
            *vc = true;
        }
        if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
            *cp = true;
        }
    };

    let default_pos = document_side_dialog_pos(ctx, data, 340.0, 96.0);
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .movable(true)
        .default_pos(default_pos)
        .min_width(340.0)
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.spacing_mut().slider_width = 200.0;
            ui.add_space(6.0);
            filter_proxy_preview_ui(ui, data);
            ui.add_space(10.0);

            match &mut filter {
                FilterType::GaussianBlur { radius } => {
                    let r = crate::ui::widgets::dev_slider_neutral_resp(
                        ui,
                        "Radius",
                        radius,
                        0.1..=100.0,
                    );
                    debounced(&r, &mut value_changed, &mut commit_preview);
                }
                FilterType::Sharpen { amount, radius } => {
                    let r1 = crate::ui::widgets::dev_slider_neutral_resp(
                        ui,
                        "Amount",
                        amount,
                        0.0..=5.0,
                    );
                    debounced(&r1, &mut value_changed, &mut commit_preview);
                    ui.add_space(2.0);
                    let r2 = crate::ui::widgets::dev_slider_neutral_resp(
                        ui,
                        "Radius",
                        radius,
                        0.1..=50.0,
                    );
                    debounced(&r2, &mut value_changed, &mut commit_preview);
                }
                FilterType::HighPass { radius } => {
                    let r = crate::ui::widgets::dev_slider_neutral_resp(
                        ui,
                        "Radius",
                        radius,
                        0.1..=250.0,
                    );
                    debounced(&r, &mut value_changed, &mut commit_preview);
                }
                FilterType::AddNoise {
                    amount,
                    monochromatic,
                } => {
                    let r = crate::ui::widgets::dev_slider_neutral_resp(
                        ui,
                        "Amount",
                        amount,
                        0.0..=100.0,
                    );
                    debounced(&r, &mut value_changed, &mut commit_preview);
                    ui.add_space(4.0);
                    // A checkbox has no drag phase — apply the preview on every toggle.
                    if ui.checkbox(monochromatic, "Monochromatic").changed() {
                        value_changed = true;
                        commit_preview = true;
                    }
                }
                FilterType::Pixelate { cell } => {
                    let r = crate::ui::widgets::dev_slider_neutral_resp(
                        ui,
                        "Cell Size",
                        cell,
                        2.0..=200.0,
                    );
                    debounced(&r, &mut value_changed, &mut commit_preview);
                }
                FilterType::ReduceNoise { strength } => {
                    let r = crate::ui::widgets::dev_slider_neutral_resp(
                        ui,
                        "Strength",
                        strength,
                        0.0..=100.0,
                    );
                    debounced(&r, &mut value_changed, &mut commit_preview);
                }
            }

            if value_changed {
                actions.dialogs.set_filter_dialog = Some(filter);
            }
            if commit_preview {
                actions.dialogs.apply_filter_preview = true;
            }

            ui.add_space(12.0);
            let hint = if data.dialogs.filter_preview_processing {
                "Rendering full canvas preview..."
            } else {
                "Dialog and canvas previews update live; OK applies full resolution."
            };
            ui.label(
                egui::RichText::new(hint)
                    .size(10.0)
                    .color(egui::Color32::from_gray(140)),
            );
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Reset").clicked() {
                    actions.dialogs.set_filter_dialog = Some(default_filter_like(&filter));
                    actions.dialogs.apply_filter_preview = true;
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
        });

    if apply {
        if value_changed {
            actions.dialogs.set_filter_dialog = Some(filter);
        }
        actions.dialogs.apply_filter_dialog = true;
    }
    if cancel {
        actions.dialogs.cancel_filter_dialog = true;
    }
}

pub(crate) fn filter_proxy_preview_ui(ui: &mut egui::Ui, data: &UiData) {
    let slot_size = egui::vec2(236.0, 172.0);
    let (slot, _) = ui.allocate_exact_size(slot_size, egui::Sense::hover());
    let painter = ui.painter_at(slot);
    painter.rect_filled(slot, 0.0, egui::Color32::from_rgb(48, 48, 48));
    draw_checkerboard_in_rect(ui, slot.shrink(1.0), 8.0);

    if let Some(image) = &data.dialogs.filter_proxy_preview {
        let cache_id = egui::Id::new("filter_proxy_preview_texture");
        let image_key = std::sync::Arc::as_ptr(image) as usize;
        let texture = ui
            .ctx()
            .data(|data| data.get_temp::<(usize, egui::TextureHandle)>(cache_id))
            .map(|(cached_key, mut texture)| {
                if cached_key != image_key {
                    texture.set((**image).clone(), egui::TextureOptions::LINEAR);
                    ui.ctx().data_mut(|store| {
                        store.insert_temp(cache_id, (image_key, texture.clone()))
                    });
                }
                texture
            })
            .unwrap_or_else(|| {
                let texture = ui.ctx().load_texture(
                    "filter_proxy_preview",
                    (**image).clone(),
                    egui::TextureOptions::LINEAR,
                );
                ui.ctx()
                    .data_mut(|store| store.insert_temp(cache_id, (image_key, texture.clone())));
                texture
            });

        let iw = image.size[0].max(1) as f32;
        let ih = image.size[1].max(1) as f32;
        let scale = (slot.width() / iw).min(slot.height() / ih).min(1.0);
        let draw_size = egui::vec2(iw * scale, ih * scale);
        let rect = egui::Rect::from_center_size(slot.center(), draw_size);
        painter.image(
            texture.id(),
            rect,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    } else {
        painter.text(
            slot.center(),
            egui::Align2::CENTER_CENTER,
            "Preview",
            egui::FontId::proportional(12.0),
            egui::Color32::from_gray(150),
        );
    }

    painter.rect_stroke(
        slot,
        0.0,
        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(92)),
        egui::StrokeKind::Inside,
    );
}

pub(crate) fn filter_proxy_preview_ps_ui(ui: &mut egui::Ui, data: &UiData) {
    let slot_size = egui::vec2(300.0, 300.0);
    let (slot, response) = ui.allocate_exact_size(slot_size, egui::Sense::click_and_drag());
    let painter = ui.painter_at(slot);
    painter.rect_filled(slot, 0.0, egui::Color32::from_rgb(48, 48, 48));
    draw_checkerboard_in_rect(ui, slot.shrink(1.0), 8.0);

    let pan_active_id = egui::Id::new("filter_proxy_pan_active");
    let pointer_down = ui.input(|i| i.pointer.primary_down());
    let mut pan_active = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(pan_active_id))
        .unwrap_or(false);
    if response.drag_started() {
        pan_active = true;
    }
    if !pointer_down {
        pan_active = false;
    }
    ui.ctx()
        .data_mut(|d| d.insert_temp(pan_active_id, pan_active));

    if response.hovered() || pan_active {
        ui.output_mut(|o| {
            o.cursor_icon = if pan_active && pointer_down {
                egui::CursorIcon::Grabbing
            } else {
                egui::CursorIcon::Grab
            }
        });
    }

    // Hold the primary button anywhere on the thumbnail to inspect the ORIGINAL
    // (before) image — including while dragging to pan a zoomed-in view. Release
    // to snap back to the filtered (after) result. (Previously a drag flipped it
    // back to the filtered image mid-pan, so you couldn't pan the original.)
    let shown_image = if response.is_pointer_button_down_on() {
        data.dialogs
            .filter_proxy_original
            .as_ref()
            .or(data.dialogs.filter_proxy_preview.as_ref())
    } else {
        data.dialogs
            .filter_proxy_preview
            .as_ref()
            .or(data.dialogs.filter_proxy_original.as_ref())
    };

    if let Some(image) = shown_image {
        let cache_id = egui::Id::new("filter_proxy_preview_texture");
        let image_key = std::sync::Arc::as_ptr(image) as usize;
        let texture = ui
            .ctx()
            .data(|data| data.get_temp::<(usize, egui::TextureHandle)>(cache_id))
            .map(|(cached_key, mut texture)| {
                if cached_key != image_key {
                    texture.set((**image).clone(), egui::TextureOptions::LINEAR);
                    ui.ctx().data_mut(|store| {
                        store.insert_temp(cache_id, (image_key, texture.clone()))
                    });
                }
                texture
            })
            .unwrap_or_else(|| {
                let texture = ui.ctx().load_texture(
                    "filter_proxy_preview",
                    (**image).clone(),
                    egui::TextureOptions::LINEAR,
                );
                ui.ctx()
                    .data_mut(|store| store.insert_temp(cache_id, (image_key, texture.clone())));
                texture
            });

        let zoom_id = egui::Id::new("filter_proxy_zoom");
        let zoom = ui
            .ctx()
            .data(|d| d.get_temp::<f32>(zoom_id))
            .unwrap_or(1.0)
            .clamp(0.25, 4.0);
        let iw = image.size[0].max(1) as f32;
        let ih = image.size[1].max(1) as f32;
        let fit = (slot.width() / iw).min(slot.height() / ih).min(1.0);
        let draw_size = egui::vec2(iw * fit * zoom, ih * fit * zoom);
        let mut rect = egui::Rect::from_center_size(slot.center(), draw_size);
        let mut uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
        if draw_size.x > slot.width() || draw_size.y > slot.height() {
            let vis_x = (slot.width() / draw_size.x).min(1.0);
            let vis_y = (slot.height() / draw_size.y).min(1.0);
            let pan_id = egui::Id::new("filter_proxy_pan");
            let mut pan = ui
                .ctx()
                .data(|d| d.get_temp::<egui::Vec2>(pan_id))
                .unwrap_or(egui::Vec2::ZERO);
            if pan_active && pointer_down {
                let delta = ui.input(|i| i.pointer.delta());
                pan -= egui::vec2(
                    delta.x / draw_size.x.max(1.0),
                    delta.y / draw_size.y.max(1.0),
                );
            }
            pan.x = pan.x.clamp((vis_x * 0.5) - 0.5, 0.5 - (vis_x * 0.5));
            pan.y = pan.y.clamp((vis_y * 0.5) - 0.5, 0.5 - (vis_y * 0.5));
            ui.ctx().data_mut(|d| d.insert_temp(pan_id, pan));
            let center = egui::pos2(0.5 + pan.x, 0.5 + pan.y);
            uv = egui::Rect::from_min_max(
                egui::pos2(center.x - vis_x * 0.5, center.y - vis_y * 0.5),
                egui::pos2(center.x + vis_x * 0.5, center.y + vis_y * 0.5),
            );
            rect = slot;
        } else {
            ui.ctx()
                .data_mut(|d| d.insert_temp(egui::Id::new("filter_proxy_pan"), egui::Vec2::ZERO));
        }
        painter.image(texture.id(), rect, uv, egui::Color32::WHITE);
    } else {
        painter.text(
            slot.center(),
            egui::Align2::CENTER_CENTER,
            "Preview",
            egui::FontId::proportional(12.0),
            egui::Color32::from_gray(150),
        );
    }

    painter.rect_stroke(
        slot,
        0.0,
        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(92)),
        egui::StrokeKind::Inside,
    );

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(87.0);
        let zoom_id = egui::Id::new("filter_proxy_zoom");
        let mut zoom = ui
            .ctx()
            .data(|d| d.get_temp::<f32>(zoom_id))
            .unwrap_or(1.0)
            .clamp(0.25, 4.0);
        if ui.small_button("-").clicked() {
            zoom = next_filter_zoom(zoom, -1);
        }
        ui.add_sized(
            [74.0, 18.0],
            egui::Label::new(format!("{:.0}%", zoom * 100.0)),
        );
        if ui.small_button(ph::PLUS).clicked() {
            zoom = next_filter_zoom(zoom, 1);
        }
        ui.ctx().data_mut(|d| d.insert_temp(zoom_id, zoom));
    });
}

pub(crate) fn next_filter_zoom(current: f32, dir: i32) -> f32 {
    let stops = [0.25, 0.5, 1.0, 2.0, 4.0];
    let mut idx = stops
        .iter()
        .position(|z| (*z - current).abs() < 0.01)
        .unwrap_or_else(|| stops.partition_point(|z| *z < current).min(stops.len() - 1));
    if dir < 0 {
        idx = idx.saturating_sub(1);
    } else {
        idx = (idx + 1).min(stops.len() - 1);
    }
    stops[idx]
}

pub(crate) fn filter_controls_ps_ui(
    ui: &mut egui::Ui,
    filter: &mut FilterType,
    value_changed: &mut bool,
    commit_preview: &mut bool,
) {
    ui.set_width(300.0);
    match filter {
        FilterType::GaussianBlur { radius } => filter_value_ps_control(
            ui,
            "Radius:",
            radius,
            0.1..=100.0,
            0.1,
            "Pixels",
            1,
            value_changed,
            commit_preview,
        ),
        FilterType::Sharpen { amount, radius } => {
            filter_value_ps_control(
                ui,
                "Amount:",
                amount,
                0.0..=5.0,
                0.05,
                "",
                2,
                value_changed,
                commit_preview,
            );
            filter_value_ps_control(
                ui,
                "Radius:",
                radius,
                0.1..=50.0,
                0.1,
                "Pixels",
                1,
                value_changed,
                commit_preview,
            );
        }
        FilterType::HighPass { radius } => filter_value_ps_control(
            ui,
            "Radius:",
            radius,
            0.1..=250.0,
            0.1,
            "Pixels",
            1,
            value_changed,
            commit_preview,
        ),
        FilterType::AddNoise {
            amount,
            monochromatic,
        } => {
            filter_value_ps_control(
                ui,
                "Amount:",
                amount,
                0.0..=100.0,
                0.5,
                "%",
                1,
                value_changed,
                commit_preview,
            );
            if ui.checkbox(monochromatic, "Monochromatic").changed() {
                *value_changed = true;
                *commit_preview = true;
            }
        }
        FilterType::Pixelate { cell } => filter_value_ps_control(
            ui,
            "Cell Size:",
            cell,
            2.0..=200.0,
            1.0,
            "Pixels",
            0,
            value_changed,
            commit_preview,
        ),
        FilterType::ReduceNoise { strength } => filter_value_ps_control(
            ui,
            "Strength:",
            strength,
            0.0..=100.0,
            0.5,
            "%",
            1,
            value_changed,
            commit_preview,
        ),
    }
}

pub(crate) fn filter_value_ps_control(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    speed: f32,
    unit: &str,
    decimals: usize,
    value_changed: &mut bool,
    commit_preview: &mut bool,
) {
    let min = *range.start();
    let max = *range.end();
    ui.horizontal(|ui| {
        ui.add_sized([48.0, 20.0], egui::Label::new(label));
        let drag = ui.add_sized(
            [58.0, 20.0],
            egui::DragValue::new(value)
                .range(min..=max)
                .speed(speed)
                .max_decimals(decimals),
        );
        if drag.changed() {
            *value = value.clamp(min, max);
            *value_changed = true;
            *commit_preview = true;
        }
        if !unit.is_empty() {
            ui.label(unit);
        }
    });
    let slider = filter_line_slider(ui, value, min, max);
    if slider.changed() {
        *value_changed = true;
    }
    if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
        *commit_preview = true;
    }
    ui.add_space(2.0);
}

pub(crate) fn filter_line_slider(
    ui: &mut egui::Ui,
    value: &mut f32,
    min: f32,
    max: f32,
) -> egui::Response {
    let (rect, mut response) =
        ui.allocate_exact_size(egui::vec2(196.0, 18.0), egui::Sense::click_and_drag());
    if (response.dragged() || response.clicked()) && response.interact_pointer_pos().is_some() {
        let pos = response.interact_pointer_pos().unwrap();
        let t = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        let next = min + (max - min) * t;
        if (*value - next).abs() > f32::EPSILON {
            *value = next;
            response.mark_changed();
        }
    }

    let painter = ui.painter_at(rect);
    let y = rect.center().y;
    painter.line_segment(
        [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
        egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
    );
    let t = ((*value - min) / (max - min)).clamp(0.0, 1.0);
    let x = egui::lerp(rect.left()..=rect.right(), t);
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(x, y - 6.0),
            egui::pos2(x - 5.0, y + 3.0),
            egui::pos2(x + 5.0, y + 3.0),
        ],
        egui::Color32::WHITE,
        egui::Stroke::NONE,
    ));
    response
}

pub(crate) fn smart_fill_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let mut open = true;
    // Shift+F5 opens this modal; Enter confirms it just like clicking Apply.
    // Consume the key here so it cannot leak through to the canvas/tool session.
    let mut do_apply = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));

    let id = egui::Id::new("smart_fill_use_ai");
    // Default to Classic (PatchMatch) — instant, no model download.
    let mut use_ai = ctx.data_mut(|d| d.get_temp::<bool>(id)).unwrap_or(false);

    modal_overlay(ctx, "smart_fill_overlay");

    egui::Window::new("Smart Fill")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.label("Fill method:");
            egui::ComboBox::from_id_salt("ca_method")
                .selected_text(if use_ai {
                    "AI (LaMa) — best quality"
                } else {
                    "Classic (PatchMatch)"
                })
                .width(220.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut use_ai, true, "AI (LaMa) — best quality");
                    ui.selectable_value(&mut use_ai, false, "Classic (PatchMatch)");
                });

            if use_ai {
                ui.add_space(6.0);
                if data.dialogs.lama_available {
                    ui.colored_label(
                        egui::Color32::from_rgb(60, 170, 90),
                        format!("{} AI model ready", ph::CHECK),
                    );
                } else if !data.dialogs.lama_status_msg.is_empty() {
                    ui.label(&data.dialogs.lama_status_msg);
                } else {
                    ui.label("AI model (~200 MB) not downloaded.");
                    ui.label("Apply now uses Classic and downloads the model in the background,");
                    ui.label("or download it first:");
                    if ui.button("Download AI model").clicked() {
                        actions.dialogs.download_lama_model = true;
                    }
                }
            }

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Apply").clicked() {
                    do_apply = true;
                }
                if ui.button("Cancel").clicked() {
                    actions.dialogs.cancel_smart_fill_dialog = true;
                }
            });
        });

    ctx.data_mut(|d| d.insert_temp(id, use_ai));

    if do_apply {
        actions.dialogs.apply_smart_fill_fill = Some(use_ai);
    }
    if !open {
        actions.dialogs.cancel_smart_fill_dialog = true;
    }
}

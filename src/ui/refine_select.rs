// "Refine Selection" workspace — right side panel (replaces normal panels while open).
//
// Layout (top-to-bottom):
//   ── Header: title + close (×)
//   ── Overlay Tint: color and opacity
//   ── Refine Brush: Mode buttons, Size, Hardness
//   ── Global Refinements: Feather, Smooth, Contrast, Shift Edge
//   ── Output Settings: Output To dropdown
//   ── Footer: OK (commit) / Cancel (revert)

use super::{UiActions, UiData};
use crate::core::selection::RefineBrushMode;
use egui;
use egui_phosphor::regular as ph;

#[allow(deprecated)]
pub fn build(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    if !data.sel.show_refine_panel {
        return;
    }

    egui::SidePanel::right("sam_panel")
        .exact_size(data.chrome.panel_r_w)
        .resizable(false)
        .frame(
            egui::Frame::new()
                .fill(egui::Color32::from_rgb(38, 38, 38))
                .inner_margin(egui::Margin::same(0)),
        )
        .show(ctx, |ui| {
            ui.set_min_width(data.chrome.panel_r_w);

            let header_fill = egui::Color32::from_rgb(28, 28, 28);
            egui::Frame::new()
                .fill(header_fill)
                .inner_margin(egui::Margin {
                    left: 10,
                    right: 6,
                    top: 6,
                    bottom: 6,
                })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Refine Selection")
                                .strong()
                                .size(13.0)
                                .color(egui::Color32::from_rgb(210, 210, 210)),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let close_btn = egui::Button::new(
                                egui::RichText::new(ph::X)
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(160, 160, 160)),
                            )
                            .frame(false);
                            if ui.add(close_btn).on_hover_text("Cancel (Esc)").clicked() {
                                actions.sel.refine_cancel = true;
                            }
                        });
                    });
                });

            ui.add_space(2.0);

            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    ui.add_space(4.0);

                    let pad = egui::Margin {
                        left: 10,
                        right: 10,
                        top: 0,
                        bottom: 0,
                    };

                    section_header(ui, "Overlay Tint");

                    egui::Frame::new().inner_margin(pad).show(ui, |ui| {
                        let color = egui::Color32::from_rgba_unmultiplied(
                            data.sel.refine_overlay_color[0],
                            data.sel.refine_overlay_color[1],
                            data.sel.refine_overlay_color[2],
                            data.sel.refine_overlay_color[3],
                        );
                        ui.horizontal(|ui| {
                            ui.label("Color:");
                            let swatch = egui::Button::new("")
                                .fill(color)
                                .min_size(egui::vec2(34.0, 20.0));
                            if ui.add(swatch).on_hover_text("Edit overlay tint").clicked() {
                                actions.sel.open_refine_color_dialog = true;
                            }
                            ui.label(
                                egui::RichText::new("Overlay")
                                    .small()
                                    .color(egui::Color32::from_rgb(150, 180, 230)),
                            );
                        });
                        ui.add_space(2.0);
                        let mut opacity = data.sel.refine_overlay_color[3] as f32 / 255.0;
                        if ui
                            .add(
                                egui::Slider::new(&mut opacity, 0.05..=0.95)
                                    .text("Opacity")
                                    .clamping(egui::SliderClamping::Always),
                            )
                            .changed()
                        {
                            let alpha = (opacity * 255.0).round().clamp(0.0, 255.0) as u8;
                            actions.sel.set_refine_overlay_color = Some([
                                data.sel.refine_overlay_color[0],
                                data.sel.refine_overlay_color[1],
                                data.sel.refine_overlay_color[2],
                                alpha,
                            ]);
                        }
                    });

                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(4.0);

                    section_header(ui, "Refine Brush");

                    egui::Frame::new().inner_margin(pad).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let modes = [
                                (
                                    RefineBrushMode::Smart,
                                    "Smart",
                                    "Auto edge matting (best for hair/fur)",
                                ),
                                (RefineBrushMode::Add, "+ Add", "Force-include pixels"),
                                (RefineBrushMode::Subtract, "- Sub", "Force-exclude pixels"),
                            ];
                            for (mode, label, tip) in &modes {
                                let selected = data.sel.refine_brush_mode == *mode;
                                let btn = egui::Button::new(*label)
                                    .fill(if selected {
                                        egui::Color32::from_rgb(30, 90, 160)
                                    } else {
                                        egui::Color32::from_rgb(55, 55, 55)
                                    })
                                    .min_size(egui::vec2(60.0, 22.0));
                                if ui.add(btn).on_hover_text(*tip).clicked() {
                                    actions.sel.set_refine_brush_mode = Some(*mode);
                                }
                            }
                        });
                        ui.add_space(4.0);

                        ui.horizontal(|ui| {
                            ui.label("Size:");
                            ui.add_space(8.0);
                            let mut v = data.sel.refine_brush_size;
                            if ui
                                .add(
                                    egui::Slider::new(&mut v, 1.0f32..=500.0)
                                        .suffix(" px")
                                        .clamping(egui::SliderClamping::Always),
                                )
                                .changed()
                            {
                                actions.sel.set_refine_brush_size = Some(v);
                            }
                        });
                        ui.add_space(2.0);

                        ui.horizontal(|ui| {
                            ui.label("Hardness:");
                            let mut v = data.sel.refine_brush_hardness;
                            if ui
                                .add(
                                    egui::Slider::new(&mut v, 0.0f32..=1.0)
                                        .clamping(egui::SliderClamping::Always),
                                )
                                .changed()
                            {
                                actions.sel.set_refine_brush_hardness = Some(v);
                            }
                        });

                        if data.sel.refine_brush_mode == RefineBrushMode::Smart {
                            ui.add_space(3.0);
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} Paint over hair/fur edges",
                                    ph::SPARKLE
                                ))
                                .small()
                                .color(egui::Color32::from_rgb(140, 200, 140)),
                            );
                        }
                        ui.label(
                            egui::RichText::new("[ / ] resize  ·  Alt+drag resize")
                                .small()
                                .color(egui::Color32::from_rgb(110, 110, 110)),
                        );
                    });

                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(4.0);

                    section_header(ui, "Global Refinements");

                    egui::Frame::new().inner_margin(pad).show(ui, |ui| {
                        let fire =
                            |r: egui::Response| r.drag_stopped() || (r.changed() && !r.dragged());
                        let r = slider_row(
                            ui,
                            "Smooth:",
                            data.sel.refine_smooth as f32,
                            0.0,
                            100.0,
                            "",
                            |v| actions.sel.set_refine_smooth = Some(v as u32),
                        );
                        if fire(r) {
                            actions.sel.trigger_refine_apply = true;
                        }

                        let r = slider_row(
                            ui,
                            "Smart Radius:",
                            data.sel.refine_smart_radius,
                            0.0,
                            64.0,
                            " px",
                            |v| actions.sel.set_refine_smart_radius = Some(v),
                        );
                        if fire(r) {
                            actions.sel.trigger_refine_apply = true;
                        }

                        let r = slider_row(
                            ui,
                            "Feather:",
                            data.sel.refine_feather,
                            0.0,
                            150.0,
                            " px",
                            |v| actions.sel.set_refine_feather = Some(v),
                        );
                        if fire(r) {
                            actions.sel.trigger_refine_apply = true;
                        }

                        let r = slider_row(
                            ui,
                            "Contrast:",
                            data.sel.refine_contrast,
                            0.0,
                            100.0,
                            "%",
                            |v| actions.sel.set_refine_contrast = Some(v),
                        );
                        if fire(r) {
                            actions.sel.trigger_refine_apply = true;
                        }

                        let r = slider_row(
                            ui,
                            "Shift Edge:",
                            data.sel.refine_shift_edge,
                            -100.0,
                            100.0,
                            "%",
                            |v| actions.sel.set_refine_shift_edge = Some(v),
                        );
                        if fire(r) {
                            actions.sel.trigger_refine_apply = true;
                        }
                    });

                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(4.0);

                    section_header(ui, "Output Settings");

                    egui::Frame::new().inner_margin(pad).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Output To:");
                            ui.add_space(4.0);
                            egui::ComboBox::from_id_salt("sam_output")
                                .selected_text(output_label(data.sel.refine_output_mode))
                                .width(160.0)
                                .show_ui(ui, |ui| {
                                    let modes = [
                                        (RefineOutputMode::Selection, "Selection"),
                                        (RefineOutputMode::LayerMask, "Layer Mask"),
                                        (RefineOutputMode::NewLayer, "New Layer"),
                                        (RefineOutputMode::NewLayerWithMask, "New Layer with Mask"),
                                    ];
                                    for (mode, label) in &modes {
                                        let selected = data.sel.refine_output_mode == *mode;
                                        if ui.selectable_label(selected, *label).clicked() {
                                            actions.sel.set_refine_output_mode = Some(*mode);
                                        }
                                    }
                                });
                        });
                        ui.add_space(2.0);
                        let desc = match data.sel.refine_output_mode {
                            RefineOutputMode::Selection => "Keep as active selection",
                            RefineOutputMode::LayerMask => "Add mask to active layer",
                            RefineOutputMode::NewLayer => "Copy selection to new layer",
                            RefineOutputMode::NewLayerWithMask => "New layer + apply as mask",
                        };
                        ui.label(
                            egui::RichText::new(desc)
                                .small()
                                .color(egui::Color32::from_rgb(130, 130, 130)),
                        );

                        ui.add_space(6.0);
                        let mut decontaminate = data.sel.refine_decontaminate;
                        if ui
                            .checkbox(&mut decontaminate, "Decontaminate Colors")
                            .on_hover_text("Reduce background color spill on new-layer outputs")
                            .changed()
                        {
                            actions.sel.set_refine_decontaminate = Some(decontaminate);
                        }
                        let mut amount = data.sel.refine_decontaminate_amount;
                        let resp = ui.add_enabled(
                            data.sel.refine_decontaminate,
                            egui::Slider::new(&mut amount, 0.0f32..=1.0)
                                .text("Amount")
                                .clamping(egui::SliderClamping::Always),
                        );
                        if resp.changed() {
                            actions.sel.set_refine_decontaminate_amount = Some(amount);
                        }
                    });

                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(6.0);

                    egui::Frame::new().inner_margin(pad).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let ok = egui::Button::new(
                                egui::RichText::new("  OK  ").color(egui::Color32::WHITE),
                            )
                            .fill(egui::Color32::from_rgb(30, 90, 160))
                            .min_size(egui::vec2(80.0, 28.0));
                            if ui.add(ok).on_hover_text("Apply refinements").clicked() {
                                actions.sel.refine_apply = true;
                            }

                            ui.add_space(8.0);

                            let cancel = egui::Button::new("Cancel")
                                .fill(egui::Color32::from_rgb(60, 60, 60))
                                .min_size(egui::vec2(80.0, 28.0));
                            if ui
                                .add(cancel)
                                .on_hover_text("Revert to original selection")
                                .clicked()
                            {
                                actions.sel.refine_cancel = true;
                            }
                        });
                    });

                    ui.add_space(8.0);
                });
        });

    draw_refine_color_dialog(ctx, data, actions);
}

fn draw_refine_color_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    if !data.sel.show_refine_color_dialog {
        return;
    }

    let esc_pressed = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    let enter_pressed = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
    if esc_pressed {
        actions.sel.refine_color_dialog_cancel = true;
    }
    if enter_pressed {
        actions.sel.refine_color_dialog_ok = true;
    }

    let side_pos = crate::ui::document_side_dialog_pos(ctx, data, 340.0, 96.0);
    let mut open = data.sel.show_refine_color_dialog;
    let mut window = egui::Window::new("Overlay Tint")
        .id(egui::Id::new("sam_overlay_tint_dialog"))
        .collapsible(false)
        .resizable(false)
        .movable(true)
        .default_pos(side_pos)
        .default_width(340.0)
        .open(&mut open);

    if data.sel.refine_color_dialog_center_next {
        window = window.current_pos(side_pos);
        actions.sel.refine_color_dialog_centered = true;
    }

    window.show(ctx, |ui| {
        ui.spacing_mut().slider_width = 270.0;

        let mut color = egui::Color32::from_rgba_unmultiplied(
            data.sel.refine_color_dialog_color[0],
            data.sel.refine_color_dialog_color[1],
            data.sel.refine_color_dialog_color[2],
            data.sel.refine_color_dialog_color[3],
        );
        if egui::color_picker::color_picker_color32(
            ui,
            &mut color,
            egui::color_picker::Alpha::OnlyBlend,
        ) {
            actions.sel.set_refine_color_dialog_color =
                Some([color.r(), color.g(), color.b(), color.a()]);
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            color_preview(ui, "Current", data.sel.refine_color_dialog_original);
            ui.add_space(8.0);
            color_preview(ui, "New", data.sel.refine_color_dialog_color);
        });

        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("Swatches")
                .small()
                .color(egui::Color32::GRAY),
        );
        ui.horizontal_wrapped(|ui| {
            let alpha = data.sel.refine_color_dialog_color[3];
            let swatches = [
                [210, 30, 30, alpha],
                [255, 92, 0, alpha],
                [255, 214, 0, alpha],
                [70, 180, 90, alpha],
                [0, 150, 220, alpha],
                [120, 90, 240, alpha],
                [255, 80, 170, alpha],
                [245, 245, 245, alpha],
                [35, 35, 35, alpha],
            ];
            for swatch in swatches {
                if color_swatch(ui, swatch).clicked() {
                    actions.sel.set_refine_color_dialog_color = Some(swatch);
                }
            }
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let mut live_preview = data.sel.refine_color_dialog_live_preview;
            if ui.checkbox(&mut live_preview, "Preview").changed() {
                actions.sel.set_refine_color_dialog_live_preview = Some(live_preview);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let [r, g, b, a] = data.sel.refine_color_dialog_color;
                ui.label(
                    egui::RichText::new(format!("#{r:02X}{g:02X}{b:02X}  {a}"))
                        .monospace()
                        .small()
                        .color(egui::Color32::GRAY),
                );
            });
        });

        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Default").clicked() {
                actions.sel.refine_color_dialog_default = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(egui::Button::new("Cancel").min_size(egui::vec2(72.0, 24.0)))
                    .clicked()
                {
                    actions.sel.refine_color_dialog_cancel = true;
                }
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new("OK").color(egui::Color32::WHITE))
                            .fill(egui::Color32::from_rgb(30, 90, 160))
                            .min_size(egui::vec2(72.0, 24.0)),
                    )
                    .clicked()
                {
                    actions.sel.refine_color_dialog_ok = true;
                }
            });
        });
    });

    if !open {
        actions.sel.refine_color_dialog_cancel = true;
    }
}

fn color_preview(ui: &mut egui::Ui, label: &str, color: [u8; 4]) {
    ui.vertical(|ui| {
        ui.label(
            egui::RichText::new(label)
                .small()
                .color(egui::Color32::GRAY),
        );
        let (rect, _) = ui.allocate_exact_size(egui::vec2(86.0, 30.0), egui::Sense::hover());
        ui.painter().rect_filled(
            rect,
            2.0,
            egui::Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]),
        );
        ui.painter().rect_stroke(
            rect,
            2.0,
            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(90)),
            egui::StrokeKind::Outside,
        );
    });
}

fn color_swatch(ui: &mut egui::Ui, color: [u8; 4]) -> egui::Response {
    ui.add(
        egui::Button::new("")
            .fill(egui::Color32::from_rgba_unmultiplied(
                color[0], color[1], color[2], color[3],
            ))
            .min_size(egui::vec2(22.0, 22.0)),
    )
}

fn section_header(ui: &mut egui::Ui, title: &str) {
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        ui.label(
            egui::RichText::new(title)
                .strong()
                .size(11.5)
                .color(egui::Color32::from_rgb(170, 195, 255)),
        );
    });
    ui.add_space(3.0);
}

/// Returns the slider's egui Response so callers can check drag_released().
fn slider_row(
    ui: &mut egui::Ui,
    label: &str,
    mut value: f32,
    min: f32,
    max: f32,
    suffix: &str,
    mut on_change: impl FnMut(f32),
) -> egui::Response {
    let inner = ui.horizontal(|ui| {
        ui.set_min_width(ui.available_width());
        ui.allocate_ui_with_layout(
            egui::vec2(80.0, 18.0),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                ui.label(label);
            },
        );
        let slider = egui::Slider::new(&mut value, min..=max)
            .suffix(suffix)
            .clamping(egui::SliderClamping::Always);
        let r = ui.add(slider);
        if r.changed() {
            on_change(value);
        }
        r
    });
    ui.add_space(2.0);
    inner.inner
}

fn output_label(mode: RefineOutputMode) -> &'static str {
    match mode {
        RefineOutputMode::Selection => "Selection",
        RefineOutputMode::LayerMask => "Layer Mask",
        RefineOutputMode::NewLayer => "New Layer",
        RefineOutputMode::NewLayerWithMask => "New Layer with Mask",
    }
}

/// How the refine overlay is drawn while the panel is open.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum RefineViewMode {
    #[default]
    Overlay,
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum RefineOutputMode {
    #[default]
    Selection,
    LayerMask,
    NewLayer,
    NewLayerWithMask,
}

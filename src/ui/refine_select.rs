// "Refine Selection" workspace — right side panel (replaces normal panels while
// open), laid out like Photoshop's Select and Mask:
//   View · Refine Brush · Edge Detection · Global Refinements · Output · OK/Cancel

use super::{UiActions, UiData};
use crate::core::refine::RefineParams;
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

                    section_header(ui, "View");
                    egui::Frame::new().inner_margin(pad).show(ui, |ui| {
                        view_section(ui, data, actions);
                    });
                    section_gap(ui);

                    section_header(ui, "Refine Brush");
                    egui::Frame::new().inner_margin(pad).show(ui, |ui| {
                        brush_section(ui, data, actions);
                    });
                    section_gap(ui);

                    let mut p = data.sel.refine_params;
                    let mut changed = false;
                    let mut release = false;
                    let fire =
                        |r: &egui::Response| r.drag_stopped() || (r.changed() && !r.dragged());

                    section_header(ui, "Edge Detection");
                    egui::Frame::new().inner_margin(pad).show(ui, |ui| {
                        let r = slider(ui, "Radius", &mut p.radius, 0.0..=RefineParams::MAX_RADIUS)
                            .on_hover_text(
                                "Width (px) of the edge band re-read from the photo colours (hair, fur)",
                            );
                        changed |= r.changed();
                        release |= fire(&r);
                        let r = ui
                            .checkbox(&mut p.smart_radius, "Smart Radius")
                            .on_hover_text("Keep the band tight on crisp edges, wide on soft ones");
                        changed |= r.changed();
                        release |= r.changed();
                    });
                    section_gap(ui);

                    section_header(ui, "Global Refinements");
                    egui::Frame::new().inner_margin(pad).show(ui, |ui| {
                        let rows: [(&str, &mut f32, std::ops::RangeInclusive<f32>, &str); 4] = [
                            ("Smooth", &mut p.smooth, 0.0..=100.0, "Round off jagged outlines"),
                            (
                                "Feather",
                                &mut p.feather,
                                0.0..=RefineParams::MAX_FEATHER,
                                "Blur the edge (px)",
                            ),
                            ("Contrast", &mut p.contrast, 0.0..=100.0, "Sharpen soft edges (%)"),
                            (
                                "Shift Edge",
                                &mut p.shift_edge,
                                -100.0..=100.0,
                                "Move soft edges in (-) or out (+), in %",
                            ),
                        ];
                        for (label, value, range, tip) in rows {
                            let r = slider(ui, label, value, range).on_hover_text(tip);
                            changed |= r.changed();
                            release |= fire(&r);
                        }
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            if ui
                                .button(format!("{} Clear", ph::SELECTION_SLASH))
                                .on_hover_text("Clear Selection: start the mask over from nothing")
                                .clicked()
                            {
                                actions.sel.refine_clear = true;
                            }
                            if ui
                                .button(format!("{} Invert", ph::SELECTION_INVERSE))
                                .on_hover_text("Invert the mask")
                                .clicked()
                            {
                                actions.sel.refine_invert = true;
                            }
                        });
                    });
                    if changed {
                        actions.sel.set_refine_params = Some(p);
                    }
                    if release {
                        actions.sel.trigger_refine_apply = true;
                    }
                    section_gap(ui);

                    section_header(ui, "Output Settings");
                    egui::Frame::new().inner_margin(pad).show(ui, |ui| {
                        output_section(ui, data, actions);
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
                            if ui.add(ok).on_hover_text("Apply (Enter)").clicked() {
                                actions.sel.refine_apply = true;
                            }
                            ui.add_space(8.0);
                            let cancel = egui::Button::new("Cancel")
                                .fill(egui::Color32::from_rgb(60, 60, 60))
                                .min_size(egui::vec2(80.0, 28.0));
                            if ui
                                .add(cancel)
                                .on_hover_text("Back to the selection you started with (Esc)")
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

fn section_gap(ui: &mut egui::Ui) {
    ui.add_space(6.0);
    ui.separator();
    ui.add_space(4.0);
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .small()
            .color(egui::Color32::from_rgb(120, 120, 120)),
    );
}

/// The Develop panel's slider: label above a gradient track, the value on the
/// right (click it to type one).
fn slider(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
) -> egui::Response {
    let colors = super::develop::tone_gradient(label);
    crate::ui::widgets::dev_slider_stacked_resp(ui, label, value, range, &colors, 1.0)
}

/// A 0..1 setting shown as a percentage; returns the new 0..1 value.
fn percent_slider(ui: &mut egui::Ui, label: &str, value: f32, min_pct: f32) -> Option<f32> {
    let mut pct = value * 100.0;
    slider(ui, label, &mut pct, min_pct..=100.0)
        .changed()
        .then_some(pct / 100.0)
}

fn view_section(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    let mode = data.sel.refine_view_mode;
    ui.horizontal(|ui| {
        ui.label("View:");
        egui::ComboBox::from_id_salt("sam_view")
            .selected_text(mode.label())
            .width(150.0)
            .show_ui(ui, |ui| {
                for m in RefineViewMode::ALL {
                    if ui.selectable_label(m == mode, m.label()).clicked() {
                        actions.sel.set_refine_view_mode = Some(m);
                    }
                }
            });
    });
    let mut original = data.sel.refine_show_original;
    if ui
        .checkbox(&mut original, "Show Original (X)")
        .on_hover_text("Hide the preview to compare with the photo")
        .changed()
    {
        actions.sel.set_refine_show_original = Some(original);
    }
    match mode {
        RefineViewMode::Overlay => {
            let [r, g, b, a] = data.sel.refine_overlay_color;
            ui.horizontal(|ui| {
                ui.label("Color:");
                let swatch = egui::Button::new("")
                    .fill(egui::Color32::from_rgb(r, g, b))
                    .min_size(egui::vec2(34.0, 20.0));
                if ui
                    .add(swatch)
                    .on_hover_text("Edit overlay colour")
                    .clicked()
                {
                    actions.sel.open_refine_color_dialog = true;
                }
            });
            if let Some(v) = percent_slider(ui, "Opacity", a as f32 / 255.0, 5.0) {
                let alpha = (v * 255.0).round().clamp(0.0, 255.0) as u8;
                actions.sel.set_refine_overlay_color = Some([r, g, b, alpha]);
            }
        }
        RefineViewMode::OnBlack | RefineViewMode::OnWhite => {
            if let Some(v) = percent_slider(ui, "Opacity", data.sel.refine_view_opacity, 5.0) {
                actions.sel.set_refine_view_opacity = Some(v);
            }
        }
        RefineViewMode::BlackWhite | RefineViewMode::MarchingAnts => {}
    }
    hint(ui, "F: next view  ·  X: show original");
}

fn brush_section(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    ui.horizontal(|ui| {
        let modes = [
            (
                RefineBrushMode::Smart,
                format!("{} Smart", ph::SPARKLE),
                "Read the edge from the photo colours under the brush (hair, fur)",
            ),
            (
                RefineBrushMode::Add,
                format!("{} Add", ph::PLUS),
                "Paint into the selection",
            ),
            (
                RefineBrushMode::Subtract,
                format!("{} Subtract", ph::MINUS),
                "Paint out of the selection",
            ),
        ];
        for (mode, label, tip) in modes {
            let selected = data.sel.refine_brush_mode == mode;
            let btn = egui::Button::new(label)
                .fill(if selected {
                    egui::Color32::from_rgb(30, 90, 160)
                } else {
                    egui::Color32::from_rgb(55, 55, 55)
                })
                .min_size(egui::vec2(72.0, 22.0));
            if ui.add(btn).on_hover_text(tip).clicked() {
                actions.sel.set_refine_brush_mode = Some(mode);
            }
        }
    });
    ui.add_space(4.0);
    let mut size = data.sel.refine_brush_size;
    let colors = super::develop::tone_gradient("Size");
    if crate::ui::widgets::dev_slider_stacked_log_resp(ui, "Size", &mut size, 1.0..=1000.0, &colors)
        .on_hover_text("Brush diameter (px)")
        .changed()
    {
        actions.sel.set_refine_brush_size = Some(size);
    }
    if let Some(v) = percent_slider(ui, "Hardness", data.sel.refine_brush_hardness, 0.0) {
        actions.sel.set_refine_brush_hardness = Some(v);
    }
    ui.horizontal(|ui| {
        let undo = ui
            .add_enabled(
                data.sel.refine_can_undo,
                egui::Button::new(ph::ARROW_U_UP_LEFT),
            )
            .on_hover_text("Undo stroke (Ctrl+Z)");
        if undo.clicked() {
            actions.sel.refine_undo = true;
        }
        let redo = ui
            .add_enabled(
                data.sel.refine_can_redo,
                egui::Button::new(ph::ARROW_U_UP_RIGHT),
            )
            .on_hover_text("Redo stroke (Ctrl+Shift+Z)");
        if redo.clicked() {
            actions.sel.refine_redo = true;
        }
    });
    hint(ui, "Alt: reverse the brush (Smart puts the start back)");
    hint(ui, "[ / ] size  ·  Alt+right-drag size");
}

fn output_section(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    let mut decontaminate = data.sel.refine_decontaminate;
    if ui
        .checkbox(&mut decontaminate, "Decontaminate Colors")
        .on_hover_text(
            "Replace background colour fringes with nearby subject colour (new layer outputs)",
        )
        .changed()
    {
        actions.sel.set_refine_decontaminate = Some(decontaminate);
    }
    let amount = data.sel.refine_decontaminate_amount;
    ui.add_enabled_ui(data.sel.refine_decontaminate, |ui| {
        if let Some(v) = percent_slider(ui, "Amount", amount, 0.0) {
            actions.sel.set_refine_decontaminate_amount = Some(v);
        }
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Output To:");
        ui.add_space(4.0);
        egui::ComboBox::from_id_salt("sam_output")
            .selected_text(output_label(data.sel.refine_output_mode))
            .width(160.0)
            .show_ui(ui, |ui| {
                let modes = [
                    RefineOutputMode::Selection,
                    RefineOutputMode::LayerMask,
                    RefineOutputMode::NewLayer,
                    RefineOutputMode::NewLayerWithMask,
                ];
                for mode in modes {
                    let allowed = !data.sel.refine_decontaminate
                        || matches!(
                            mode,
                            RefineOutputMode::NewLayer | RefineOutputMode::NewLayerWithMask
                        );
                    let selected = data.sel.refine_output_mode == mode;
                    if ui
                        .add_enabled(
                            allowed,
                            egui::Button::selectable(selected, output_label(mode)),
                        )
                        .clicked()
                    {
                        actions.sel.set_refine_output_mode = Some(mode);
                    }
                }
            });
    });
    let desc = match data.sel.refine_output_mode {
        RefineOutputMode::Selection => "Keep as the active selection",
        RefineOutputMode::LayerMask => "Mask the active layer",
        RefineOutputMode::NewLayer => "Cut the subject to a new layer",
        RefineOutputMode::NewLayerWithMask => "Copy the layer with this mask",
    };
    hint(ui, desc);
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

fn output_label(mode: RefineOutputMode) -> &'static str {
    match mode {
        RefineOutputMode::Selection => "Selection",
        RefineOutputMode::LayerMask => "Layer Mask",
        RefineOutputMode::NewLayer => "New Layer",
        RefineOutputMode::NewLayerWithMask => "New Layer with Mask",
    }
}

/// How the Refine Selection panel previews the selection on the canvas.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum RefineViewMode {
    #[default]
    Overlay,
    OnBlack,
    OnWhite,
    BlackWhite,
    MarchingAnts,
}

impl RefineViewMode {
    pub const ALL: [RefineViewMode; 5] = [
        RefineViewMode::Overlay,
        RefineViewMode::OnBlack,
        RefineViewMode::OnWhite,
        RefineViewMode::BlackWhite,
        RefineViewMode::MarchingAnts,
    ];

    pub fn label(self) -> &'static str {
        match self {
            RefineViewMode::Overlay => "Overlay",
            RefineViewMode::OnBlack => "On Black",
            RefineViewMode::OnWhite => "On White",
            RefineViewMode::BlackWhite => "Black & White",
            RefineViewMode::MarchingAnts => "Marching Ants",
        }
    }

    /// The next view (F cycles, as in Photoshop).
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|&m| m == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum RefineOutputMode {
    #[default]
    Selection,
    LayerMask,
    NewLayer,
    NewLayerWithMask,
}

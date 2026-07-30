use super::{UiActions, UiData};
use egui;
use egui_phosphor::regular as ph;

pub(super) const HEIGHT: f32 = 22.0;

#[allow(deprecated)]
pub fn build(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let pal = data.chrome.theme_mode.palette();
    #[allow(deprecated)]
    egui::TopBottomPanel::bottom("statusbar")
        .exact_size(HEIGHT)
        .frame(
            egui::Frame::new()
                .fill(pal.top_bar_bg)
                .inner_margin(egui::Margin::symmetric(4, 1)),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(4.0);

                if ui
                    .small_button(ph::MINUS)
                    .on_hover_text("Zoom Out")
                    .clicked()
                {
                    actions.doc.zoom_out = true;
                }

                let zoom_text = format!("{:.0}%", data.doc.zoom * 100.0);
                if ui
                    .small_button(&zoom_text)
                    .on_hover_text("Click to set zoom (Ctrl+0 = Fit)")
                    .clicked()
                {
                    actions.doc.zoom_100 = true;
                }

                if ui.small_button(ph::PLUS).on_hover_text("Zoom In").clicked() {
                    actions.doc.zoom_in = true;
                }

                ui.separator();

                use crate::core::units::{format_value, from_pixels, Unit};
                let size_text = if data.doc.canvas_unit == Unit::Pixels {
                    format!("{}x{} px", data.doc.canvas_w, data.doc.canvas_h)
                } else {
                    let u = data.doc.canvas_unit;
                    let dpi = data.doc.canvas_dpi;
                    let w = from_pixels(data.doc.canvas_w as f32, u, dpi, data.doc.canvas_w as f32);
                    let h = from_pixels(data.doc.canvas_h as f32, u, dpi, data.doc.canvas_h as f32);
                    format!(
                        "{} × {} {}",
                        format_value(w, u),
                        format_value(h, u),
                        u.name()
                    )
                };
                ui.label(
                    egui::RichText::new(size_text)
                        .color(pal.text_secondary)
                        .size(11.0),
                );

                ui.separator();

                ui.label(
                    egui::RichText::new(format!("{:.0} DPI", data.doc.canvas_dpi))
                        .color(pal.text_secondary)
                        .size(11.0),
                );

                ui.separator();

                let layer_name = data
                    .layers
                    .layer_names
                    .get(data.layers.active_layer_idx)
                    .map(|s| s.as_str())
                    .unwrap_or("No Layer");
                ui.label(
                    egui::RichText::new(format!("Layer: {}", layer_name))
                        .color(pal.text_secondary)
                        .size(11.0),
                );

                ui.separator();

                ui.label(
                    egui::RichText::new(format!(
                        "Undo: {}  Redo: {}",
                        data.doc.undo_count, data.doc.redo_count
                    ))
                    .color(pal.text_secondary)
                    .size(11.0),
                );

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(4.0);

                    let hint = match data.tool.active_tool {
                        crate::tools::ToolId::Brush => {
                            "B=Brush | [ ]=Size | Shift+[ ]=Hard | X=Swap"
                        }
                        crate::tools::ToolId::Eraser => "E=Eraser | [ ]=Size",
                        crate::tools::ToolId::Move => "V=Move | Click+Drag to pan",
                        crate::tools::ToolId::Crop if data.tool.crop_rect.is_none() => {
                            "C=Crop | Drag=Select area | Click image=Start"
                        }
                        crate::tools::ToolId::Crop => "C=Crop | Enter=Apply | Esc=Cancel",
                        crate::tools::ToolId::Eyedropper => "I=Eyedropper | Click to pick color",
                        crate::tools::ToolId::Fill => "G=Fill | Click to flood fill",
                        crate::tools::ToolId::Zoom => "Z=Zoom | Drag right/down=In | left/up=Out",
                        crate::tools::ToolId::Hand => "H=Hand | Click+Drag to pan",
                        _ => "Space+Drag=Pan | Alt+Scroll=Zoom",
                    };

                    if !data.chrome.status_msg.is_empty() {
                        ui.label(
                            egui::RichText::new(&data.chrome.status_msg)
                                .color(pal.accent_hover)
                                .size(11.0),
                        );
                        ui.separator();
                    }

                    ui.label(
                        egui::RichText::new(hint)
                            .color(pal.text_disabled)
                            .size(10.0),
                    );
                });
            });
        });
}

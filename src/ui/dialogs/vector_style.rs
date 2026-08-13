//! Batch vector styling dialog (Object ▸ Format All Vectors…). Vector layers
//! are split into arrows, parametric shapes and non-arrow paths; each style row
//! is opt-in, and the whole application is one undo step.

use super::*;
use crate::ui::intent::VectorBatchStyle;

pub(crate) fn vector_style_dialog(ctx: &egui::Context, _data: &UiData, actions: &mut UiActions) {
    let arrows_id = egui::Id::new("vecfmt_arrows");
    let shapes_id = egui::Id::new("vecfmt_shapes");
    let curves_id = egui::Id::new("vecfmt_curves");
    let fill_on_id = egui::Id::new("vecfmt_fill_on");
    let fill_id = egui::Id::new("vecfmt_fill");
    let stroke_on_id = egui::Id::new("vecfmt_stroke_on");
    let stroke_id = egui::Id::new("vecfmt_stroke");
    let width_on_id = egui::Id::new("vecfmt_width_on");
    let width_id = egui::Id::new("vecfmt_width");

    let mut arrows = ctx.data_mut(|d| d.get_temp::<bool>(arrows_id).unwrap_or(true));
    let mut shapes = ctx.data_mut(|d| d.get_temp::<bool>(shapes_id).unwrap_or(true));
    let mut curves = ctx.data_mut(|d| d.get_temp::<bool>(curves_id).unwrap_or(true));
    let mut fill_on = ctx.data_mut(|d| d.get_temp::<bool>(fill_on_id).unwrap_or(false));
    let mut fill = ctx.data_mut(|d| {
        d.get_temp::<egui::Color32>(fill_id)
            .unwrap_or(egui::Color32::WHITE)
    });
    let mut stroke_on = ctx.data_mut(|d| d.get_temp::<bool>(stroke_on_id).unwrap_or(false));
    let mut stroke = ctx.data_mut(|d| {
        d.get_temp::<egui::Color32>(stroke_id)
            .unwrap_or(egui::Color32::BLACK)
    });
    let mut width_on = ctx.data_mut(|d| d.get_temp::<bool>(width_on_id).unwrap_or(false));
    let mut width = ctx.data_mut(|d| d.get_temp::<f32>(width_id).unwrap_or(2.0));

    let (enter_pressed, esc_pressed) = consume_dialog_enter_escape(ctx);
    let mut do_apply = enter_pressed;
    let mut do_cancel = esc_pressed;

    modal_overlay(ctx, "vector_style_overlay");
    egui::Window::new("Đổi màu và độ dày viền hàng loạt")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(DIALOG_ORDER)
        .min_width(390.0)
        .show(ctx, |ui| {
            ui.add_space(6.0);
            ui.label("Chọn loại đối tượng và chỉ bật những thuộc tính cần thay đổi.");
            ui.add_space(10.0);

            ui.strong("Phạm vi");
            ui.horizontal(|ui| {
                ui.checkbox(&mut arrows, "Mũi tên");
                ui.checkbox(&mut shapes, "Shape");
                ui.checkbox(&mut curves, "Đường/Curve");
            });

            ui.add_space(10.0);
            ui.strong("Thay đổi");
            egui::Grid::new("vecfmt_grid")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.checkbox(&mut fill_on, "Màu tô (fill)");
                    ui.add_enabled_ui(fill_on, |ui| {
                        ui.color_edit_button_srgba(&mut fill);
                    });
                    ui.end_row();

                    ui.checkbox(&mut stroke_on, "Màu viền (stroke)");
                    ui.add_enabled_ui(stroke_on, |ui| {
                        ui.color_edit_button_srgba(&mut stroke);
                    });
                    ui.end_row();

                    ui.checkbox(&mut width_on, "Độ dày viền (px)");
                    ui.add_enabled_ui(width_on, |ui| {
                        ui.add(
                            egui::DragValue::new(&mut width)
                                .range(0.0..=500.0)
                                .suffix(" px")
                                .speed(0.2),
                        );
                    });
                    ui.end_row();
                });

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("  Áp dụng  ").clicked() {
                    do_apply = true;
                }
                if ui.button("Hủy").clicked() {
                    do_cancel = true;
                }
            });
            ui.add_space(4.0);
        });

    ctx.data_mut(|d| {
        d.insert_temp(arrows_id, arrows);
        d.insert_temp(shapes_id, shapes);
        d.insert_temp(curves_id, curves);
        d.insert_temp(fill_on_id, fill_on);
        d.insert_temp(fill_id, fill);
        d.insert_temp(stroke_on_id, stroke_on);
        d.insert_temp(stroke_id, stroke);
        d.insert_temp(width_on_id, width_on);
        d.insert_temp(width_id, width);
    });

    if do_apply {
        let spec = VectorBatchStyle {
            include_arrows: arrows,
            include_shapes: shapes,
            include_curves: curves,
            set_fill: fill_on.then(|| fill.to_array()),
            set_stroke: stroke_on.then(|| stroke.to_array()),
            set_stroke_width: width_on.then_some(width.clamp(0.0, 500.0)),
        };
        if spec.is_noop() {
            actions.dialogs.show_vector_style_dialog = Some(false);
        } else {
            actions.dialogs.apply_vector_style = Some(spec);
        }
    } else if do_cancel {
        actions.dialogs.show_vector_style_dialog = Some(false);
    }
}

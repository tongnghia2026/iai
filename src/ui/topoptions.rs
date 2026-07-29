use super::{modal_flash_btn, LayerAlign, LayerDistribute, MoveTransformAction, UiActions, UiData};
use crate::core::text::TextAlign;
use crate::tools::ToolId;
use egui;
use egui_phosphor::regular as ph;

use crate::ui::widgets::focus_field_select_all;

fn dimension_drag<'a>(
    value: &'a mut f32,
    parsed_unit: &'a std::cell::Cell<Option<crate::core::units::Unit>>,
    speed: f64,
    decimals: usize,
) -> egui::DragValue<'a> {
    egui::DragValue::new(value)
        .min_decimals(decimals)
        .max_decimals(decimals)
        .range(0.0..=99999.0)
        .speed(speed)
        .custom_parser(move |text| {
            crate::core::units::parse_dimension(text).map(|(value, unit)| {
                if unit.is_some() {
                    parsed_unit.set(unit);
                }
                value as f64
            })
        })
}

pub fn build(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let pal = data.chrome.theme_mode.palette();
    #[allow(deprecated)]
    egui::TopBottomPanel::top("topoptions")
        .exact_size(32.0)
        .frame(
            egui::Frame::new()
                .fill(pal.top_bar_bg)
                .inner_margin(egui::Margin::symmetric(4, 3)),
        )
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.spacing_mut().button_padding = egui::vec2(7.0, 3.0);
            // Keep every tool's options reachable: when the row is wider than the
            // window (e.g. Move with a vector object selected) it scrolls instead
            // of pushing controls off the right edge.
            egui::ScrollArea::horizontal()
                .auto_shrink([false, false])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        ui.add_space(4.0);

                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new(ph::HOUSE).size(15.0).color(pal.icon),
                                )
                                .min_size(egui::vec2(26.0, 24.0)),
                            )
                            .on_hover_text("Home — Welcome Screen")
                            .clicked()
                        {
                            actions.chrome.show_welcome = Some(true);
                        }

                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new(ph::GRID_NINE)
                                        .size(15.0)
                                        .color(pal.icon),
                                )
                                .min_size(egui::vec2(26.0, 24.0)),
                            )
                            .on_hover_text("Library — browse a folder of photos")
                            .clicked()
                        {
                            actions.chrome.show_library = Some(true);
                        }
                        ui.separator();

                        match data.tool.active_tool {
                            ToolId::Brush | ToolId::Pencil => brush_options(ui, data, actions),
                            ToolId::Eraser => eraser_options(ui, data, actions),
                            ToolId::Crop => crop_options(ui, data, actions),
                            ToolId::Fill => fill_options(ui, data, actions),
                            ToolId::Gradient => gradient_options(ui, data, actions),
                            ToolId::Eyedropper => eyedropper_options(ui, data, actions),
                            ToolId::Clone => clone_tool_options(ui, data, actions, false),
                            ToolId::Repair => clone_tool_options(ui, data, actions, true),
                            ToolId::Smudge => smudge_options(ui, data, actions),
                            ToolId::Dodge | ToolId::Burn => dodge_burn_options(ui, data, actions),
                            ToolId::Patch => patch_options(ui, data, actions),
                            ToolId::Move => move_options(ui, data, actions),
                            ToolId::Zoom => zoom_options(ui, data, actions),
                            ToolId::SmartSelect => smart_select_options(ui, data, actions),
                            ToolId::SelectionRect
                            | ToolId::SelectionEllipse
                            | ToolId::Lasso
                            | ToolId::PolygonLasso => selection_options(ui, data, actions),
                            ToolId::Transform => transform_options(ui, data, actions),
                            ToolId::Text => text_options(ui, data, actions),
                            ToolId::Shape => shape_options(ui, data, actions),
                            ToolId::Node => node_options(ui, data, actions),
                            ToolId::PerspectiveCrop => perspective_crop_options(ui, data, actions),
                            ToolId::Pen => pen_options(ui, data, actions),
                            ToolId::VectorBrush => vector_brush_options(ui, data, actions),
                            _ => default_options(ui, data),
                        }
                    });
                });
        });
}

fn perspective_crop_options(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    use crate::core::units::Unit;
    let pal = data.chrome.theme_mode.palette();

    ui.label(egui::RichText::new("Perspective Crop").strong());
    ui.separator();

    let mut unit = match data.tool.persp_unit {
        1 => Unit::Centimeters,
        2 => Unit::Millimeters,
        3 => Unit::Inches,
        4 => Unit::Points,
        5 => Unit::Picas,
        6 => Unit::Percent,
        _ => Unit::Pixels,
    };
    let (speed, decimals) = if data.tool.persp_unit == 0 {
        (1.0_f64, 0_usize)
    } else {
        (0.01, 2)
    };

    // Output size — typeable before OR after the corners are picked.
    ui.label("W:");
    let mut w_disp = data.tool.persp_w_display;
    let typed_unit = std::cell::Cell::new(None);
    let response = ui
        .add(dimension_drag(&mut w_disp, &typed_unit, speed, decimals))
        .on_hover_text("Output width; type a unit such as 10 cm, 15mm, or 8 in (0 = auto)");
    if let Some(parsed) = typed_unit.get() {
        unit = parsed;
        actions.tool.set_persp_unit = Some(parsed as u8);
    }
    if response.changed() || typed_unit.get().is_some() {
        if w_disp <= 0.0 {
            actions.tool.clear_persp_size = true;
        } else {
            actions.tool.set_persp_out_w = Some(w_disp);
        }
    }
    ui.label("H:");
    let mut h_disp = data.tool.persp_h_display;
    let typed_unit = std::cell::Cell::new(None);
    let response = ui
        .add(dimension_drag(&mut h_disp, &typed_unit, speed, decimals))
        .on_hover_text("Output height; type a unit such as 10 cm, 15mm, or 8 in (0 = auto)");
    if let Some(parsed) = typed_unit.get() {
        unit = parsed;
        actions.tool.set_persp_unit = Some(parsed as u8);
    }
    if response.changed() || typed_unit.get().is_some() {
        if h_disp <= 0.0 {
            actions.tool.clear_persp_size = true;
        } else {
            actions.tool.set_persp_out_h = Some(h_disp);
        }
    }

    ui.separator();
    let unit_names = ["px", "cm", "mm", "in", "pt", "pc", "%"];
    let mut unit_idx = unit as usize;
    egui::ComboBox::from_id_salt("persp_unit")
        .selected_text(unit_names.get(unit_idx).copied().unwrap_or("px"))
        .width(44.0)
        .show_ui(ui, |ui| {
            for (i, name) in unit_names.iter().enumerate() {
                if ui.selectable_value(&mut unit_idx, i, *name).changed() {
                    actions.tool.set_persp_unit = Some(unit_idx as u8);
                }
            }
        });

    ui.label("DPI:");
    let mut dpi_val = data.tool.persp_dpi;
    if ui
        .add(
            egui::DragValue::new(&mut dpi_val)
                .range(1.0..=10000.0)
                .speed(1.0),
        )
        .on_hover_text("Output resolution (px/inch)")
        .changed()
    {
        actions.tool.set_persp_dpi = Some(dpi_val);
    }

    if data.tool.persp_size_manual
        && ui
            .button("Auto")
            .on_hover_text("Derive size from the quad")
            .clicked()
    {
        actions.tool.clear_persp_size = true;
    }

    ui.separator();

    if data.tool.persp_has_quad {
        let cancel_btn = egui::Button::new(egui::RichText::new(ph::X).size(13.0).color(pal.danger))
            .min_size(egui::vec2(26.0, 22.0));
        let cancel_btn = modal_flash_btn(cancel_btn, ui, data);
        if ui.add(cancel_btn).on_hover_text("Cancel (Esc)").clicked() {
            actions.tool.crop_cancel = true;
        }
        let confirm_btn =
            egui::Button::new(egui::RichText::new(ph::CHECK).size(13.0).color(pal.success))
                .min_size(egui::vec2(26.0, 22.0));
        let confirm_btn = modal_flash_btn(confirm_btn, ui, data);
        if ui.add(confirm_btn).on_hover_text("Apply (Enter)").clicked() {
            actions.tool.crop_confirm = true;
        }
        ui.separator();
        ui.label(
            egui::RichText::new(
                "Drag the 4 corners to adjust · Enter/✓ to apply · Esc/✗ to cancel",
            )
            .size(11.0)
            .color(pal.text_secondary),
        );
    } else {
        ui.label(
            egui::RichText::new(
                "Click the 4 corners of the area to rectify, OR drag to draw a frame · W/H can be entered first",
            )
            .size(11.0)
            .color(pal.text_secondary),
        );
    }
}

fn pen_options(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    ui.label(egui::RichText::new("Pen").strong());
    ui.separator();

    ui.label("On Enter:");
    let mut mode = data.tool.pen_mode;
    for (v, name) in [
        (0u8, "Selection"),
        (1, "Fill"),
        (2, "Stroke"),
        (3, "Path (Vector)"),
    ] {
        if ui.selectable_value(&mut mode, v, name).changed() {
            actions.tool.set_pen_mode = Some(mode);
        }
    }

    if mode == 2 {
        ui.separator();
        ui.label("Width:");
        let mut sw = data.tool.pen_stroke_width;
        if ui
            .add(
                egui::DragValue::new(&mut sw)
                    .range(1.0..=500.0)
                    .suffix(" px")
                    .speed(0.2),
            )
            .changed()
        {
            actions.tool.set_pen_stroke_width = Some(sw);
        }
    }
}

fn vector_brush_options(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    ui.label(egui::RichText::new("Vector Brush").strong());
    ui.separator();

    ui.label("Width:");
    let mut w = data.tool.vector_brush_width;
    if ui
        .add(
            egui::DragValue::new(&mut w)
                .range(0.5..=500.0)
                .suffix(" px")
                .speed(0.2),
        )
        .changed()
    {
        actions.tool.set_vector_brush_width = Some(w);
    }

    ui.separator();
    ui.label("Smoothing:");
    let mut s = data.tool.vector_brush_smoothing;
    if ui
        .add(egui::Slider::new(&mut s, 0.0..=0.95).fixed_decimals(2))
        .changed()
    {
        actions.tool.set_vector_brush_smoothing = Some(s);
    }

    ui.separator();
    let mut pressure = data.tool.vector_brush_pressure;
    if ui.checkbox(&mut pressure, "Pressure").changed() {
        actions.tool.set_vector_brush_pressure = Some(pressure);
    }
    let mut velocity = data.tool.vector_brush_velocity;
    if ui.checkbox(&mut velocity, "Speed taper").changed() {
        actions.tool.set_vector_brush_velocity = Some(velocity);
    }

    ui.separator();
    ui.add_enabled_ui(data.tool.vector_brush_can_expand, |ui| {
        if ui
            .button("Expand Stroke")
            .on_hover_text("Chuyển nét vẽ thành đường viền kín (để tô/kết hợp hình)")
            .clicked()
        {
            actions.tool.expand_vector_brush = true;
        }
    });
}

fn brush_options(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    let presets = crate::tools::brush::BRUSH_PRESETS;
    let sel_text = presets
        .get(data.tool.brush_preset_idx)
        .map(|p| p.name)
        .unwrap_or("Custom");
    egui::ComboBox::from_id_salt("brush_preset")
        .selected_text(sel_text)
        .width(130.0)
        .show_ui(ui, |ui| {
            for (i, p) in presets.iter().enumerate() {
                if ui
                    .selectable_label(data.tool.brush_preset_idx == i, p.name)
                    .clicked()
                {
                    actions.tool.select_brush_preset = Some(i);
                }
            }
        });

    ui.separator();
    ui.label("Size:");
    let mut size = data.tool.brush_size;
    if ui
        .add(
            egui::DragValue::new(&mut size)
                .range(1.0..=5000.0)
                .suffix("px")
                .speed(0.5),
        )
        .changed()
    {
        actions.tool.brush_size = Some(size);
    }

    ui.separator();
    ui.label("Hardness:");
    let mut hard = data.tool.brush_hardness * 100.0;
    if ui
        .add(
            egui::DragValue::new(&mut hard)
                .range(0.0..=100.0)
                .suffix("%")
                .speed(0.5),
        )
        .changed()
    {
        actions.tool.brush_hardness = Some(hard / 100.0);
    }

    ui.separator();
    ui.label("Opacity:");
    let mut opacity = data.tool.brush_opacity * 100.0;
    if ui
        .add(
            egui::DragValue::new(&mut opacity)
                .range(1.0..=100.0)
                .suffix("%")
                .speed(0.5),
        )
        .changed()
    {
        actions.tool.brush_opacity = Some(opacity / 100.0);
    }

    ui.separator();
    ui.label("Flow:");
    let mut flow = data.tool.brush_flow * 100.0;
    if ui
        .add(
            egui::DragValue::new(&mut flow)
                .range(1.0..=100.0)
                .suffix("%")
                .speed(0.5),
        )
        .changed()
    {
        actions.tool.brush_flow = Some(flow / 100.0);
    }

    ui.separator();
    ui.label("Spacing:");
    let mut spacing = data.tool.brush_spacing * 100.0;
    if ui
        .add(
            egui::DragValue::new(&mut spacing)
                .range(1.0..=200.0)
                .suffix("%")
                .speed(0.5),
        )
        .changed()
    {
        actions.tool.brush_spacing = Some(spacing / 100.0);
    }

    ui.separator();
    ui.label("Smooth:");
    let mut smoothing = data.tool.brush_smoothing * 100.0;
    if ui
        .add(
            egui::DragValue::new(&mut smoothing)
                .range(0.0..=100.0)
                .suffix("%")
                .speed(0.5),
        )
        .changed()
    {
        actions.tool.brush_smoothing = Some(smoothing / 100.0);
    }
}

fn eraser_options(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    ui.label("Size:");
    let mut size = data.tool.brush_size;
    if ui
        .add(
            egui::DragValue::new(&mut size)
                .range(1.0..=5000.0)
                .suffix("px")
                .speed(0.5),
        )
        .changed()
    {
        actions.tool.brush_size = Some(size);
    }

    ui.separator();
    ui.label("Hardness:");
    let mut hard = data.tool.brush_hardness * 100.0;
    if ui
        .add(
            egui::DragValue::new(&mut hard)
                .range(0.0..=100.0)
                .suffix("%")
                .speed(0.5),
        )
        .changed()
    {
        actions.tool.brush_hardness = Some(hard / 100.0);
    }

    ui.separator();
    ui.label("Opacity:");
    let mut opacity = data.tool.brush_opacity * 100.0;
    if ui
        .add(
            egui::DragValue::new(&mut opacity)
                .range(1.0..=100.0)
                .suffix("%")
                .speed(0.5),
        )
        .changed()
    {
        actions.tool.brush_opacity = Some(opacity / 100.0);
    }
}

fn clone_tool_options(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions, is_healing: bool) {
    let pal = data.chrome.theme_mode.palette();
    if let Some(i) = scrub_preset_combo(ui, "clone_preset", data.tool.clone_hardness) {
        actions.tool.set_clone_preset = Some(i);
    }

    ui.separator();
    ui.label("Size:");
    let mut size = data.tool.clone_size;
    if ui
        .add(
            egui::DragValue::new(&mut size)
                .range(1.0..=5000.0)
                .suffix("px")
                .speed(0.5),
        )
        .changed()
    {
        actions.tool.set_clone_size = Some(size);
    }

    ui.separator();
    ui.label("Hardness:");
    let mut hard = data.tool.clone_hardness * 100.0;
    if ui
        .add(
            egui::DragValue::new(&mut hard)
                .range(0.0..=100.0)
                .suffix("%")
                .speed(0.5),
        )
        .changed()
    {
        actions.tool.set_clone_hardness = Some(hard / 100.0);
    }

    ui.separator();
    ui.label("Opacity:");
    let mut opacity = data.tool.clone_opacity * 100.0;
    if ui
        .add(
            egui::DragValue::new(&mut opacity)
                .range(1.0..=100.0)
                .suffix("%")
                .speed(0.5),
        )
        .changed()
    {
        actions.tool.set_clone_opacity = Some(opacity / 100.0);
    }

    ui.separator();
    ui.label("Spacing:");
    let mut spacing = data.tool.clone_spacing * 100.0;
    if ui
        .add(
            egui::DragValue::new(&mut spacing)
                .range(1.0..=200.0)
                .suffix("%")
                .speed(0.5),
        )
        .changed()
    {
        actions.tool.set_clone_spacing = Some(spacing / 100.0);
    }

    ui.separator();
    let mut aligned = data.tool.clone_aligned;
    if ui.checkbox(&mut aligned, "Aligned").changed() {
        actions.tool.set_clone_aligned = Some(aligned);
    }

    ui.separator();
    let mut sample_merged = data.tool.clone_sample_merged;
    if ui.checkbox(&mut sample_merged, "Sample Merged").changed() {
        actions.tool.set_clone_sample_merged = Some(sample_merged);
    }

    if is_healing {
        ui.separator();
        ui.label("Type:");
        let ca = data.tool.clone_smart_fill;
        if ui
            .selectable_label(ca, "Smart")
            .on_hover_text("Synthesise the fill from surrounding texture (PatchMatch)")
            .clicked()
            && !ca
        {
            actions.tool.set_clone_smart_fill = Some(true);
        }
        if ui
            .selectable_label(!ca, "Proximity")
            .on_hover_text("Heal by cloning a clean nearby patch (default)")
            .clicked()
            && ca
        {
            actions.tool.set_clone_smart_fill = Some(false);
        }

        ui.separator();
        let hint = if ca {
            "Paint over the area · release to fill (Smart)"
        } else {
            "Click to heal (auto source) · Alt+click to pick a source"
        };
        ui.label(
            egui::RichText::new(hint)
                .size(11.0)
                .color(pal.text_secondary),
        );
    }
}

/// Soft/Hard tip picker shared by the scrub tools. Reuses the Brush preset list
/// (only `hardness` + `spacing` are applied). Returns the chosen preset index.
fn scrub_preset_combo(ui: &mut egui::Ui, id_salt: &str, hardness: f32) -> Option<usize> {
    let presets = crate::tools::brush::BRUSH_PRESETS;
    let sel = presets
        .iter()
        .position(|p| (p.hardness - hardness).abs() < 0.001);
    let label = sel.map(|i| presets[i].name).unwrap_or("Custom");
    let mut chosen = None;
    egui::ComboBox::from_id_salt(id_salt)
        .selected_text(label)
        .width(130.0)
        .show_ui(ui, |ui| {
            for (i, p) in presets.iter().enumerate() {
                if ui.selectable_label(sel == Some(i), p.name).clicked() {
                    chosen = Some(i);
                }
            }
        });
    chosen
}

fn smudge_options(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    if let Some(i) = scrub_preset_combo(ui, "smudge_preset", data.tool.smudge_hardness) {
        actions.tool.set_smudge_preset = Some(i);
    }
    ui.separator();
    ui.label("Size:");
    let mut size = data.tool.smudge_size;
    if ui
        .add(
            egui::DragValue::new(&mut size)
                .range(1.0..=5000.0)
                .suffix("px")
                .speed(0.5),
        )
        .changed()
    {
        actions.tool.set_smudge_size = Some(size);
    }

    ui.separator();
    ui.label("Hardness:");
    let mut hard = data.tool.smudge_hardness * 100.0;
    if ui
        .add(
            egui::DragValue::new(&mut hard)
                .range(0.0..=100.0)
                .suffix("%")
                .speed(0.5),
        )
        .changed()
    {
        actions.tool.set_smudge_hardness = Some(hard / 100.0);
    }

    ui.separator();
    ui.label("Strength:");
    let mut strength = data.tool.smudge_strength * 100.0;
    if ui
        .add(
            egui::DragValue::new(&mut strength)
                .range(0.0..=100.0)
                .suffix("%")
                .speed(0.5),
        )
        .changed()
    {
        actions.tool.set_smudge_strength = Some(strength / 100.0);
    }

    ui.separator();
    let mut finger = data.tool.smudge_finger_painting;
    if ui
        .checkbox(&mut finger, "Finger Painting")
        .on_hover_text("Start the smear with the foreground colour")
        .changed()
    {
        actions.tool.set_smudge_finger_painting = Some(finger);
    }
}

fn dodge_burn_options(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    if let Some(i) = scrub_preset_combo(ui, "dodge_preset", data.tool.dodge_hardness) {
        actions.tool.set_dodge_preset = Some(i);
    }
    ui.separator();
    ui.label("Size:");
    let mut size = data.tool.dodge_size;
    if ui
        .add(
            egui::DragValue::new(&mut size)
                .range(1.0..=5000.0)
                .suffix("px")
                .speed(0.5),
        )
        .changed()
    {
        actions.tool.set_dodge_size = Some(size);
    }

    ui.separator();
    ui.label("Hardness:");
    let mut hard = data.tool.dodge_hardness * 100.0;
    if ui
        .add(
            egui::DragValue::new(&mut hard)
                .range(0.0..=100.0)
                .suffix("%")
                .speed(0.5),
        )
        .changed()
    {
        actions.tool.set_dodge_hardness = Some(hard / 100.0);
    }

    ui.separator();
    ui.label("Range:");
    let mut range = data.tool.dodge_range;
    for (v, name) in [(0u8, "Shadows"), (1, "Midtones"), (2, "Highlights")] {
        if ui.selectable_value(&mut range, v, name).changed() {
            actions.tool.set_dodge_range = Some(range);
        }
    }

    ui.separator();
    ui.label("Exposure:");
    let mut exp = data.tool.dodge_exposure * 100.0;
    if ui
        .add(
            egui::DragValue::new(&mut exp)
                .range(0.0..=100.0)
                .suffix("%")
                .speed(0.25),
        )
        .changed()
    {
        actions.tool.set_dodge_exposure = Some(exp / 100.0);
    }

    ui.separator();
    let mut protect = data.tool.dodge_protect_tones;
    if ui
        .checkbox(&mut protect, "Protect Tones")
        .on_hover_text("Keep hue stable and resist clipping")
        .changed()
    {
        actions.tool.set_dodge_protect_tones = Some(protect);
    }
}

fn patch_options(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    ui.label("Patch:");
    let mut mode = data.tool.patch_mode;
    if ui
        .selectable_value(&mut mode, 0, "Source")
        .on_hover_text("Drag the region onto a clean area to clone it in")
        .changed()
    {
        actions.tool.set_patch_mode = Some(mode);
    }
    if ui
        .selectable_value(&mut mode, 1, "Smart")
        .on_hover_text("Synthesise the region from its surroundings (PatchMatch)")
        .changed()
    {
        actions.tool.set_patch_mode = Some(mode);
    }

    ui.separator();
    let hint = if data.tool.patch_mode == 1 {
        "Draw a region around the subject · release to fill"
    } else {
        "Draw a region · then drag it over a clean source area"
    };
    ui.label(
        egui::RichText::new(hint)
            .size(11.0)
            .color(egui::Color32::from_gray(150)),
    );
}

fn crop_options(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    let pal = data.chrome.theme_mode.palette();
    use crate::core::units::Unit;

    let preset_label = crop_preset_label(data);
    let mut apply_preset: Option<PresetAction> = None;
    let mut open_new_preset = false;
    let mut open_delete_dialog = false;
    let preset_popup_height = crop_preset_popup_height(data.dialogs.user_presets.len());

    egui::ComboBox::from_id_salt("crop_preset_combined")
        .selected_text(preset_label)
        // Long physical-size and user preset names should remain readable.
        .width(270.0)
        // Egui's default combo height is intentionally short and adds a scroll
        // bar. Size this menu from its actual row count so the full preset list
        // opens at once (the popup system still constrains it to the monitor).
        .height(preset_popup_height)
        .show_ui(ui, |ui| {
            ui.label(
                egui::RichText::new("Ratio")
                    .color(egui::Color32::GRAY)
                    .size(10.5),
            );
            let items: &[(&str, PresetAction)] = &[
                ("W × H × Resolution", PresetAction::Free),
                ("Original Ratio", PresetAction::OriginalRatio),
                ("1 : 1  (Square)", PresetAction::Ratio(1.0, 1.0)),
                ("4 : 5  (8 : 10)", PresetAction::Ratio(4.0, 5.0)),
                ("5 : 7", PresetAction::Ratio(5.0, 7.0)),
                ("2 : 3  (4 : 6)", PresetAction::Ratio(2.0, 3.0)),
                ("16 : 9", PresetAction::Ratio(16.0, 9.0)),
            ];
            for (label, action) in items {
                let sel = preset_action_matches(data, action);
                if ui.selectable_label(sel, *label).clicked() {
                    apply_preset = Some(action.clone());
                    ui.close();
                }
            }

            ui.separator();

            if ui.selectable_label(false, "Front Image").clicked() {
                apply_preset = Some(PresetAction::FrontImage);
                ui.close();
            }

            ui.separator();

            ui.label(
                egui::RichText::new("Size")
                    .color(egui::Color32::GRAY)
                    .size(10.5),
            );
            let builtin: &[(&str, f32, f32, f32)] = &[
                ("4 × 5 in  300 ppi", 1200.0, 1500.0, 300.0),
                ("8.5 × 11 in  300 ppi", 2550.0, 3300.0, 300.0),
                ("1024 × 768 px  92 ppi", 1024.0, 768.0, 92.0),
                ("1280 × 800 px  113 ppi", 1280.0, 800.0, 113.0),
                ("1366 × 768 px  135 ppi", 1366.0, 768.0, 135.0),
                ("1920 × 1080 px", 1920.0, 1080.0, 72.0),
                ("2400 × 1600 px", 2400.0, 1600.0, 72.0),
            ];
            for (label, w, h, dpi) in builtin {
                if ui.selectable_label(false, *label).clicked() {
                    apply_preset = Some(PresetAction::Size(*w, *h, *dpi));
                    ui.close();
                }
            }

            if !data.dialogs.user_presets.is_empty() {
                ui.separator();
                ui.label(
                    egui::RichText::new("Saved Presets")
                        .color(egui::Color32::GRAY)
                        .size(10.5),
                );
                for (i, preset) in data.dialogs.user_presets.iter().enumerate() {
                    if ui.selectable_label(false, &preset.name).clicked() {
                        apply_preset = Some(PresetAction::UserPreset(i));
                        ui.close();
                    }
                }
            }

            ui.separator();
            if ui.button("New Crop Preset\u{2026}").clicked() {
                open_new_preset = true;
                ui.close();
            }
            if !data.dialogs.user_presets.is_empty()
                && ui.button("Delete Crop Preset\u{2026}").clicked()
            {
                open_delete_dialog = true;
                ui.close();
            }
        });

    if let Some(action) = apply_preset {
        match action {
            PresetAction::Free => {
                actions.tool.set_crop_mode = Some(0);
            }
            PresetAction::OriginalRatio => {
                actions.tool.set_crop_mode = Some(1);
                actions.tool.set_crop_ratio_w = Some(data.doc.canvas_w as f32);
                actions.tool.set_crop_ratio_h = Some(data.doc.canvas_h as f32);
            }
            PresetAction::Ratio(w, h) => {
                actions.tool.set_crop_mode = Some(1);
                actions.tool.set_crop_ratio_w = Some(w);
                actions.tool.set_crop_ratio_h = Some(h);
            }
            PresetAction::FrontImage => {
                actions.tool.set_crop_mode = Some(2);
                actions.tool.set_crop_unit = Some(0);
                actions.tool.set_crop_dpi = Some(data.doc.canvas_dpi);
                actions.tool.set_crop_w_px = Some(data.doc.canvas_w as f32);
                actions.tool.set_crop_h_px = Some(data.doc.canvas_h as f32);
            }
            PresetAction::Size(w, h, dpi) => {
                actions.tool.set_crop_mode = Some(2);
                actions.tool.set_crop_unit = Some(0);
                actions.tool.set_crop_dpi = Some(dpi);
                actions.tool.set_crop_w_px = Some(w);
                actions.tool.set_crop_h_px = Some(h);
            }
            PresetAction::UserPreset(i) => {
                let p = &data.dialogs.user_presets[i];
                actions.tool.set_crop_mode = Some(2);
                actions.tool.set_crop_unit = Some(p.unit_idx());
                actions.tool.set_crop_dpi = Some(p.dpi);
                actions.tool.set_crop_w_px = Some(p.pixel_width_for(data.doc.canvas_w as f32));
                actions.tool.set_crop_h_px = Some(p.pixel_height_for(data.doc.canvas_h as f32));
            }
        }
    }
    if open_delete_dialog {
        actions.dialogs.open_delete_preset_dialog = true;
    }

    ui.separator();

    let mut unit = match data.tool.crop_unit {
        0 => Unit::Pixels,
        1 => Unit::Centimeters,
        2 => Unit::Millimeters,
        3 => Unit::Inches,
        4 => Unit::Points,
        5 => Unit::Picas,
        _ => Unit::Percent,
    };
    let (speed, decimals) = if data.tool.crop_unit == 0 {
        (1.0_f64, 0_usize)
    } else {
        (0.01, 2)
    };

    ui.label("W:");
    let mut w_disp = data.tool.crop_w_display;
    let typed_unit = std::cell::Cell::new(None);
    let w_response = ui
        .add(dimension_drag(&mut w_disp, &typed_unit, speed, decimals))
        .on_hover_text("Type a unit such as 10 cm, 15mm, or 8 in");
    if let Some(parsed) = typed_unit.get() {
        unit = parsed;
        actions.tool.set_crop_unit = Some(parsed as u8);
    }
    if w_response.changed() || typed_unit.get().is_some() {
        actions.tool.set_crop_w_value = Some(w_disp);
    }

    let swap_btn = egui::Button::new(egui::RichText::new(ph::SWAP).size(12.0).color(pal.icon))
        .min_size(egui::vec2(22.0, 22.0));
    let swap_response = ui.add(swap_btn).on_hover_text("Swap Width and Height");
    if swap_response.clicked() {
        actions.tool.swap_crop_wh = true;
    }

    ui.label("H:");
    let mut h_disp = data.tool.crop_h_display;
    let typed_unit = std::cell::Cell::new(None);
    let h_response = ui
        .add(dimension_drag(&mut h_disp, &typed_unit, speed, decimals))
        .on_hover_text("Type a unit such as 10 cm, 15mm, or 8 in");
    if let Some(parsed) = typed_unit.get() {
        unit = parsed;
        actions.tool.set_crop_unit = Some(parsed as u8);
    }
    if h_response.changed() || typed_unit.get().is_some() {
        actions.tool.set_crop_h_value = Some(h_disp);
    }

    ui.separator();

    let unit_names = ["px", "cm", "mm", "in", "pt", "pc", "%"];
    let mut unit_idx = unit as usize;
    let unit_response = egui::ComboBox::from_id_salt("crop_unit")
        .selected_text(unit_names.get(unit_idx).copied().unwrap_or("px"))
        .width(44.0)
        .show_ui(ui, |ui| {
            for (i, name) in unit_names.iter().enumerate() {
                if ui.selectable_value(&mut unit_idx, i, *name).changed() {
                    actions.tool.set_crop_unit = Some(unit_idx as u8);
                    unit = Unit::all()[unit_idx];
                }
            }
        })
        .response;

    let mut dpi_val = data.tool.crop_dpi;
    let dpi_response = ui.add(
        egui::DragValue::new(&mut dpi_val)
            .range(1.0..=2400.0)
            .speed(1.0)
            .suffix(" ppi"),
    );
    if dpi_response.changed() {
        actions.tool.set_crop_dpi = Some(dpi_val);
    }

    // Crop's keyboard workflow is data-entry oriented. Keep Tab moving through
    // W → H → DPI (and Shift+Tab in reverse), skipping the mouse-oriented swap
    // button and unit picker. Clicking the unit picker still works normally.
    let (tab, shift) = ui
        .ctx()
        .input(|input| (input.key_pressed(egui::Key::Tab), input.modifiers.shift));
    if tab {
        if !shift && (w_response.has_focus() || swap_response.has_focus()) {
            focus_field_select_all(ui, &h_response);
        } else if !shift && h_response.has_focus() {
            focus_field_select_all(ui, &dpi_response);
        } else if !shift && unit_response.has_focus() {
            focus_field_select_all(ui, &dpi_response);
        } else if shift && dpi_response.has_focus() {
            focus_field_select_all(ui, &h_response);
        } else if shift && h_response.has_focus() {
            focus_field_select_all(ui, &w_response);
        } else if shift && swap_response.has_focus() {
            focus_field_select_all(ui, &w_response);
        } else if shift && unit_response.has_focus() {
            focus_field_select_all(ui, &h_response);
        }
    }

    if ui
        .small_button("\u{2B}")
        .on_hover_text("Save current W/H/DPI as new preset")
        .clicked()
        && w_disp > 0.0
        && h_disp > 0.0
    {
        let unit_str = unit_names.get(unit as usize).copied().unwrap_or("px");
        actions.dialogs.open_preset_dialog = Some((w_disp, h_disp, unit_str.to_string(), dpi_val));
    }
    if open_new_preset && w_disp > 0.0 && h_disp > 0.0 {
        let unit_str = unit_names.get(unit as usize).copied().unwrap_or("px");
        actions.dialogs.open_preset_dialog = Some((w_disp, h_disp, unit_str.to_string(), dpi_val));
    }

    ui.separator();

    let overlay_names = ["None", "Thirds", "Grid", "Golden"];
    let mut overlay = data.tool.crop_overlay;
    egui::ComboBox::from_id_salt("crop_overlay")
        .selected_text(
            overlay_names
                .get(overlay as usize)
                .copied()
                .unwrap_or("None"),
        )
        .width(66.0)
        .show_ui(ui, |ui| {
            if ui.selectable_value(&mut overlay, 0, "None").changed() {
                actions.tool.set_crop_overlay = Some(overlay);
            }
            if ui
                .selectable_value(&mut overlay, 1, "Rule of Thirds")
                .changed()
            {
                actions.tool.set_crop_overlay = Some(overlay);
            }
            if ui.selectable_value(&mut overlay, 2, "Grid").changed() {
                actions.tool.set_crop_overlay = Some(overlay);
            }
            if ui
                .selectable_value(&mut overlay, 3, "Golden Ratio")
                .changed()
            {
                actions.tool.set_crop_overlay = Some(overlay);
            }
        });

    if data.tool.crop_rect.is_some() {
        ui.separator();
        let cancel_btn = egui::Button::new(egui::RichText::new(ph::X).size(13.0).color(pal.danger))
            .min_size(egui::vec2(26.0, 22.0));
        let cancel_btn = modal_flash_btn(cancel_btn, ui, data);
        if ui
            .add(cancel_btn)
            .on_hover_text("Cancel crop (Esc)")
            .clicked()
        {
            actions.tool.crop_cancel = true;
        }
        let confirm_btn =
            egui::Button::new(egui::RichText::new(ph::CHECK).size(13.0).color(pal.success))
                .min_size(egui::vec2(26.0, 22.0));
        let confirm_btn = modal_flash_btn(confirm_btn, ui, data);
        if ui
            .add(confirm_btn)
            .on_hover_text("Apply crop (Enter)")
            .clicked()
        {
            actions.tool.crop_confirm = true;
        }
    }
}

/// Enough vertical room for every fixed Crop row plus all saved presets.
/// Separators/headings are included in the base; each saved preset contributes
/// one normal menu row, and a non-empty saved section also shows Delete Preset.
fn crop_preset_popup_height(saved_preset_count: usize) -> f32 {
    const FIXED_CONTENT_HEIGHT: f32 = 560.0;
    const MENU_ROW_HEIGHT: f32 = 26.0;
    FIXED_CONTENT_HEIGHT
        + saved_preset_count as f32 * MENU_ROW_HEIGHT
        + if saved_preset_count > 0 {
            MENU_ROW_HEIGHT
        } else {
            0.0
        }
}

#[derive(Clone)]
enum PresetAction {
    Free,
    OriginalRatio,
    Ratio(f32, f32),
    FrontImage,
    Size(f32, f32, f32),
    UserPreset(usize),
}

fn crop_preset_label(data: &UiData) -> String {
    match data.tool.crop_mode {
        0 => "W \u{00D7} H \u{00D7} Resolution".to_string(),
        1 => {
            let w = data.tool.crop_ratio_w;
            let h = data.tool.crop_ratio_h;
            if (w - 1.0).abs() < 0.01 && (h - 1.0).abs() < 0.01 {
                "1 : 1  (Square)".to_string()
            } else if w > 0.0 && h > 0.0 {
                let ratio = w / h;
                if (ratio - 4.0 / 5.0).abs() < 0.01 {
                    return "4 : 5  (8 : 10)".to_string();
                }
                if (ratio - 5.0 / 7.0).abs() < 0.01 {
                    return "5 : 7".to_string();
                }
                if (ratio - 2.0 / 3.0).abs() < 0.01 {
                    return "2 : 3  (4 : 6)".to_string();
                }
                if (ratio - 16.0 / 9.0).abs() < 0.01 {
                    return "16 : 9".to_string();
                }
                format!("{:.0} : {:.0}", w, h)
            } else {
                "Ratio".to_string()
            }
        }
        _ => "W \u{00D7} H \u{00D7} Resolution".to_string(),
    }
}

fn preset_action_matches(data: &UiData, action: &PresetAction) -> bool {
    match action {
        PresetAction::Free => data.tool.crop_mode == 0,
        PresetAction::OriginalRatio => false,
        PresetAction::Ratio(w, h) => {
            data.tool.crop_mode == 1
                && (data.tool.crop_ratio_w - w).abs() < 0.01
                && (data.tool.crop_ratio_h - h).abs() < 0.01
        }
        _ => false,
    }
}

fn fill_options(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    ui.label("Tolerance:");
    let mut tol = data.tool.fill_tolerance;
    if ui
        .add(egui::DragValue::new(&mut tol).range(0..=255).speed(1.0))
        .changed()
    {
        actions.tool.set_fill_tolerance = Some(tol);
    }

    ui.separator();
    let mut contiguous = data.tool.fill_contiguous;
    if ui.checkbox(&mut contiguous, "Contiguous").changed() {
        actions.tool.set_fill_contiguous = Some(contiguous);
    }

    ui.separator();
    let mut anti_alias = data.tool.fill_anti_alias;
    if ui.checkbox(&mut anti_alias, "Anti-alias").changed() {
        actions.tool.set_fill_anti_alias = Some(anti_alias);
    }

    ui.separator();
    let mut all_layers = data.tool.fill_all_layers;
    if ui.checkbox(&mut all_layers, "All Layers").changed() {
        actions.tool.set_fill_all_layers = Some(all_layers);
    }
}

fn gradient_options(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    let vector_mode = data.tool.gradient_mode == 2;
    let mode_label = match data.tool.gradient_mode {
        1 => "Pixel",
        2 => "Vector",
        3 => "Mask",
        _ => "Unavailable",
    };
    ui.label(
        egui::RichText::new(mode_label)
            .strong()
            .color(if vector_mode {
                egui::Color32::from_rgb(90, 190, 255)
            } else {
                egui::Color32::from_gray(190)
            }),
    );
    ui.separator();
    if data.tool.gradient_mode == 0 {
        ui.label("Select an editable Raster, Vector, or Mask target");
        return;
    }
    // Clickable gradient swatch → opens the Gradient Editor (pick the ramp colors).
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(88.0, 20.0), egui::Sense::click());
    crate::ui::dialogs::paint_gradient_bar(
        &ui.painter_at(rect),
        rect,
        &data.tool.gradient_stops,
        true,
    );
    let resp = resp.on_hover_text("Click to edit the gradient");
    if resp.clicked() {
        actions.tool.toggle_gradient_editor = Some(!data.tool.show_gradient_editor);
    }
    ui.separator();

    ui.label("Type:");
    let mut g_type = data.tool.gradient_type;
    egui::ComboBox::from_id_salt("gradient_type")
        .selected_text(match g_type {
            0 => "Linear",
            1 => "Radial",
            2 => "Angle",
            3 => "Reflected",
            _ => "Diamond",
        })
        .width(90.0)
        .show_ui(ui, |ui| {
            if ui.selectable_value(&mut g_type, 0, "Linear").changed() {
                actions.tool.set_gradient_type = Some(g_type);
            }
            if ui.selectable_value(&mut g_type, 1, "Radial").changed() {
                actions.tool.set_gradient_type = Some(g_type);
            }
            if !vector_mode {
                if ui.selectable_value(&mut g_type, 2, "Angle").changed() {
                    actions.tool.set_gradient_type = Some(g_type);
                }
                if ui.selectable_value(&mut g_type, 3, "Reflected").changed() {
                    actions.tool.set_gradient_type = Some(g_type);
                }
                if ui.selectable_value(&mut g_type, 4, "Diamond").changed() {
                    actions.tool.set_gradient_type = Some(g_type);
                }
            }
        });

    ui.separator();
    if vector_mode {
        if ui.button("Reverse").clicked() {
            actions.tool.set_gradient_reverse = Some(true);
        }
    } else {
        ui.label("Opacity:");
        let mut op = data.tool.gradient_opacity * 100.0;
        if ui
            .add(
                egui::DragValue::new(&mut op)
                    .range(0.0..=100.0)
                    .suffix("%")
                    .speed(0.5),
            )
            .changed()
        {
            actions.tool.set_gradient_opacity = Some(op / 100.0);
        }

        ui.separator();
        let mut reverse = data.tool.gradient_reverse;
        if ui.checkbox(&mut reverse, "Reverse").changed() {
            actions.tool.set_gradient_reverse = Some(reverse);
        }
        let mut dither = data.tool.gradient_dither;
        if ui.checkbox(&mut dither, "Dither").changed() {
            actions.tool.set_gradient_dither = Some(dither);
        }
    }
}

/// A small clickable colour chip; returns true when clicked.
fn shape_color_chip(ui: &mut egui::Ui, color: [u8; 4], tip: &str) -> bool {
    let c = egui::Color32::from_rgb(color[0], color[1], color[2]);
    ui.add_sized([22.0, 18.0], egui::Button::new("").fill(c))
        .on_hover_text(tip)
        .clicked()
}

fn shape_options(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    ui.label("Shape:");
    let mut kind = data.tool.shape_kind;
    egui::ComboBox::from_id_salt("shape_kind")
        .selected_text(match kind {
            1 => "Ellipse",
            2 => "Line",
            3 => "Polygon",
            4 => "Star",
            _ => "Rectangle",
        })
        .width(100.0)
        .show_ui(ui, |ui| {
            for (v, name) in [
                (0u8, "Rectangle"),
                (1, "Ellipse"),
                (2, "Line"),
                (3, "Polygon"),
                (4, "Star"),
            ] {
                if ui.selectable_value(&mut kind, v, name).changed() {
                    actions.tool.set_shape_kind = Some(kind);
                }
            }
        });

    ui.separator();

    // Fill: enable toggle + colour chip (not for lines, which are stroke-only).
    if kind != 2 {
        let mut fill = data.tool.shape_fill;
        if ui.checkbox(&mut fill, "Fill").changed() {
            actions.tool.set_shape_fill = Some(fill);
        }
        if shape_color_chip(ui, data.tool.shape_fill_color, "Fill color") {
            actions.dialogs.open_paint_color_dialog = Some(3);
        }
        ui.separator();
    }

    // Stroke: width + colour chip.
    ui.label(if kind == 2 { "Width:" } else { "Stroke:" });
    let mut sw = data.tool.shape_stroke_width;
    if ui
        .add(
            egui::DragValue::new(&mut sw)
                .range(0.0..=500.0)
                .suffix(" px")
                .speed(0.2),
        )
        .changed()
    {
        actions.tool.set_shape_stroke_width = Some(sw);
    }
    if shape_color_chip(ui, data.tool.shape_stroke_color, "Stroke color") {
        actions.dialogs.open_paint_color_dialog = Some(4);
    }

    // Rounded-rectangle corner radius.
    if kind == 0 {
        ui.separator();
        ui.label("Radius:");
        let mut r = data.tool.shape_corner_radius;
        if ui
            .add(
                egui::DragValue::new(&mut r)
                    .range(0.0..=2000.0)
                    .suffix(" px")
                    .speed(0.3),
            )
            .changed()
        {
            actions.tool.set_shape_corner_radius = Some(r);
        }
    }

    // Polygon edge count / Star point count, and the star's inner radius.
    if kind == 3 || kind == 4 {
        ui.separator();
        ui.label(if kind == 4 { "Points:" } else { "Sides:" });
        let mut n = data.tool.shape_sides as i32;
        if ui
            .add(egui::DragValue::new(&mut n).range(3..=100).speed(0.15))
            .changed()
        {
            actions.tool.set_shape_sides = Some(n.clamp(3, 100) as u32);
        }
    }
    if kind == 4 {
        ui.label("Inner:");
        let mut f = data.tool.shape_star_inner;
        if ui
            .add(egui::DragValue::new(&mut f).range(0.05..=0.95).speed(0.005))
            .changed()
        {
            actions.tool.set_shape_star_inner = Some(f);
        }
    }

    ui.separator();
    ui.label(
        egui::RichText::new("Drag · Shift = constrain · Alt = from center")
            .size(11.0)
            .color(egui::Color32::from_gray(150)),
    );
}

fn eyedropper_options(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    ui.label("Sample:");
    let mut sample = data.tool.eyedropper_sample;
    egui::ComboBox::from_id_salt("eyedropper_sample")
        .selected_text(match sample {
            0 => "Point Sample",
            1 => "3x3 Average",
            2 => "5x5 Average",
            _ => "11x11 Average",
        })
        .width(120.0)
        .show_ui(ui, |ui| {
            if ui
                .selectable_value(&mut sample, 0, "Point Sample")
                .changed()
            {
                actions.tool.set_eyedropper_sample = Some(sample);
            }
            if ui.selectable_value(&mut sample, 1, "3x3 Average").changed() {
                actions.tool.set_eyedropper_sample = Some(sample);
            }
            if ui.selectable_value(&mut sample, 2, "5x5 Average").changed() {
                actions.tool.set_eyedropper_sample = Some(sample);
            }
            if ui
                .selectable_value(&mut sample, 3, "11x11 Average")
                .changed()
            {
                actions.tool.set_eyedropper_sample = Some(sample);
            }
        });

    ui.separator();
    let mut all_layers = data.tool.eyedropper_sample_merged;
    if ui.checkbox(&mut all_layers, "Sample Merged").changed() {
        actions.tool.set_eyedropper_sample_merged = Some(all_layers);
    }
}

/// How many currently-selected layers are boolean-eligible vector objects
/// (Shape or Path, unlocked, non-background). The Shaping icons show once this
/// reaches two.
fn selected_vector_count(data: &UiData) -> usize {
    (0..data.layers.layer_count)
        .filter(|&i| {
            data.layers.layer_selected.get(i).copied().unwrap_or(false)
                && !data.layers.layer_locked.get(i).copied().unwrap_or(false)
                && !data
                    .layers
                    .layer_is_background
                    .get(i)
                    .copied()
                    .unwrap_or(false)
                && data
                    .layers
                    .layer_types
                    .get(i)
                    .is_some_and(|t| t == "Shape" || t == "Path")
        })
        .count()
}

/// Quick Weld/Trim/Intersect/Exclude icons for the options bar — the same
/// commands as Layer ▸ Shaping, one click away. Only rendered when at least two
/// vector objects are selected (the caller checks [`selected_vector_count`]).
fn shaping_buttons(ui: &mut egui::Ui, actions: &mut UiActions) {
    use crate::core::vector::boolean::BooleanOp;
    ui.label("Kết hợp:");
    for (icon, op, tip) in [
        (
            ph::UNITE,
            BooleanOp::Union,
            "Hàn liền (Weld) — gộp các vật thể thành một hình",
        ),
        (
            ph::SUBTRACT,
            BooleanOp::Difference,
            "Cắt bớt (Trim) — hình dưới bị các hình trên cắt, gộp thành một",
        ),
        (
            ph::INTERSECT,
            BooleanOp::Intersect,
            "Giao nhau (Intersect) — tạo hình phần chung, giữ nguyên hình gốc",
        ),
        (
            ph::EXCLUDE,
            BooleanOp::Exclude,
            "Loại trừ (Exclude) — bỏ phần chồng, giữ mỗi hình riêng",
        ),
    ] {
        if ui.button(icon).on_hover_text(tip).clicked() {
            actions.layers.boolean_op = Some(op);
        }
    }
}

fn move_options(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    let ctrl_held = ui.input(|i| i.modifiers.ctrl);
    let mut auto_select = data.tool.move_auto_select ^ ctrl_held;
    if ui.checkbox(&mut auto_select, "Auto-Select Layer").changed() {
        actions.tool.set_move_auto_select = Some(auto_select ^ ctrl_held);
    }
    ui.separator();
    let mut show_transform = data.tool.move_show_transform;
    if ui
        .checkbox(&mut show_transform, "Show Transform Controls")
        .changed()
    {
        actions.tool.set_move_show_transform = Some(show_transform);
    }
    ui.separator();
    align_button(
        ui,
        actions,
        LayerAlign::Left,
        "Align left edges to selection bounds; single layer uses canvas",
    );
    align_button(
        ui,
        actions,
        LayerAlign::HorizontalCenter,
        "Align horizontal centers to selection bounds; single layer uses canvas",
    );
    align_button(
        ui,
        actions,
        LayerAlign::Right,
        "Align right edges to selection bounds; single layer uses canvas",
    );
    ui.separator();
    align_button(
        ui,
        actions,
        LayerAlign::Top,
        "Align top edges to selection bounds; single layer uses canvas",
    );
    align_button(
        ui,
        actions,
        LayerAlign::VerticalCenter,
        "Align vertical centers to selection bounds; single layer uses canvas",
    );
    align_button(
        ui,
        actions,
        LayerAlign::Bottom,
        "Align bottom edges to selection bounds; single layer uses canvas",
    );
    ui.separator();
    if ui
        .button(ph::ARROWS_LEFT_RIGHT)
        .on_hover_text("Distribute horizontal centres of 3 or more selected objects")
        .clicked()
    {
        actions.layers.distribute_layers = Some(LayerDistribute::HorizontalCenters);
    }
    if ui
        .button(ph::ARROWS_DOWN_UP)
        .on_hover_text("Distribute vertical centres of 3 or more selected objects")
        .clicked()
    {
        actions.layers.distribute_layers = Some(LayerDistribute::VerticalCenters);
    }
    ui.separator();
    if ui
        .button("Dup")
        .on_hover_text("Duplicate selected objects with a 10 × 10 px step")
        .clicked()
    {
        actions.layers.duplicate_selected_step = true;
    }
    if ui
        .button("Repeat")
        .on_hover_text("Repeat the last duplicate translation (Ctrl+Shift+D)")
        .clicked()
    {
        actions.layers.repeat_duplicate_step = true;
    }
    ui.separator();
    move_transform_button(
        ui,
        actions,
        MoveTransformAction::Rotate90Ccw,
        ph::ARROW_COUNTER_CLOCKWISE,
        "Rotate selected layers 90 deg counter-clockwise",
    );
    move_transform_button(
        ui,
        actions,
        MoveTransformAction::Rotate90Cw,
        ph::ARROW_CLOCKWISE,
        "Rotate selected layers 90 deg clockwise",
    );
    move_transform_button(
        ui,
        actions,
        MoveTransformAction::Rotate180,
        ph::ARROWS_CLOCKWISE,
        "Rotate selected layers 180 deg",
    );
    ui.separator();
    move_transform_button(
        ui,
        actions,
        MoveTransformAction::FlipHorizontal,
        ph::FLIP_HORIZONTAL,
        "Flip selected layers horizontally",
    );
    move_transform_button(
        ui,
        actions,
        MoveTransformAction::FlipVertical,
        ph::FLIP_VERTICAL,
        "Flip selected layers vertically",
    );

    // When a vector object is active, expose a compact Fill/Outline/Width here;
    // the full editor (gradient stops, dash, overprint) lives in the Color panel
    // so this already-crowded bar stays reachable.
    if data.tool.path_style.is_some() {
        ui.separator();
        path_style_quick(ui, data, actions);
    }

    // Shaping (Weld/Trim/Intersect/Exclude) once ≥2 vector objects are selected.
    if selected_vector_count(data) >= 2 {
        ui.separator();
        shaping_buttons(ui, actions);
    }
}

/// Compact Fill/Outline/Width quick-access for the crowded Move options bar.
/// Full styling (gradient ramp, dash, overprint, fill/dash kinds) lives in the
/// Color panel's Object section, so this row stays short. Same `path_*` actions
/// as the panel, so the two surfaces stay in sync.
fn path_style_quick(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    let Some(style) = data.tool.path_style else {
        return;
    };
    let mut fill = style.fill_enabled;
    if ui.checkbox(&mut fill, "Fill").changed() {
        actions.tool.set_path_fill_enabled = Some(fill);
    }
    if shape_color_chip(ui, style.fill_color, "Fill colour") {
        actions.dialogs.open_paint_color_dialog = Some(5);
    }
    ui.separator();
    let mut stroke = style.stroke_enabled;
    if ui.checkbox(&mut stroke, "Outline").changed() {
        actions.tool.set_path_stroke_enabled = Some(stroke);
    }
    if shape_color_chip(ui, style.stroke_color, "Outline colour") {
        actions.dialogs.open_paint_color_dialog = Some(6);
    }
    ui.label("Width:");
    let mut w = style.stroke_width;
    let resp = ui.add(
        egui::DragValue::new(&mut w)
            .range(0.0..=500.0)
            .suffix(" px")
            .speed(0.2),
    );
    if resp.changed() {
        actions.tool.set_path_stroke_width = Some(w);
    }
    if resp.drag_stopped() || resp.lost_focus() {
        actions.tool.commit_path_style = true;
    }
    ui.label(
        egui::RichText::new("More in Color panel")
            .size(10.0)
            .color(egui::Color32::from_gray(140)),
    );
}

fn node_options(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    use crate::core::vector::ops::{AlignRef, Axis};
    ui.label(egui::RichText::new("Node").strong());
    ui.separator();
    if data.tool.path_style.is_some() {
        path_style_options(ui, data, actions);
        ui.separator();
    }

    // Align the multi-selected nodes (Shift+click to pick several). Snaps X for
    // left/centre/right, Y for top/middle/bottom; a no-op below 2 selected nodes.
    ui.label("Align:");
    if ui
        .button(ph::ALIGN_LEFT)
        .on_hover_text("Align left (selected points, ≥2)")
        .clicked()
    {
        actions.tool.node_align = Some((Axis::Vertical, AlignRef::Min));
    }
    if ui
        .button(ph::ALIGN_CENTER_VERTICAL)
        .on_hover_text("Align horizontal centres")
        .clicked()
    {
        actions.tool.node_align = Some((Axis::Vertical, AlignRef::Average));
    }
    if ui
        .button(ph::ALIGN_RIGHT)
        .on_hover_text("Align right")
        .clicked()
    {
        actions.tool.node_align = Some((Axis::Vertical, AlignRef::Max));
    }
    ui.separator();
    if ui
        .button(ph::ALIGN_TOP)
        .on_hover_text("Align top")
        .clicked()
    {
        actions.tool.node_align = Some((Axis::Horizontal, AlignRef::Min));
    }
    if ui
        .button(ph::ALIGN_CENTER_HORIZONTAL)
        .on_hover_text("Align vertical centres")
        .clicked()
    {
        actions.tool.node_align = Some((Axis::Horizontal, AlignRef::Average));
    }
    if ui
        .button(ph::ALIGN_BOTTOM)
        .on_hover_text("Align bottom")
        .clicked()
    {
        actions.tool.node_align = Some((Axis::Horizontal, AlignRef::Max));
    }
    ui.separator();

    // Break the path at the selected node / join two selected endpoints.
    if ui
        .button("Break")
        .on_hover_text("Break the path at the selected point")
        .clicked()
    {
        actions.tool.node_break = true;
    }
    if ui
        .button("Join")
        .on_hover_text("Join the two selected endpoints (close or weld the path)")
        .clicked()
    {
        actions.tool.node_join = true;
    }
    ui.separator();

    ui.label(
        egui::RichText::new(
            "Drag point/handle · click edge = insert · Alt+edge = line/curve · \
             double-click point = corner/smooth · Shift+click or box = multi-select · \
             click another path = select · Delete = remove",
        )
        .size(11.0)
        .color(egui::Color32::from_gray(150)),
    );
}

/// Fill/Outline colour + outline width for the active Path layer. Shared by the
/// Move and Node options bars. Colours open the paint dialog (targets 5/6);
/// the width scrub previews live and commits one undo on release.
fn path_style_options(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    let Some(style) = data.tool.path_style else {
        return;
    };
    // Fill.
    let mut fill = style.fill_enabled;
    if ui.checkbox(&mut fill, "Fill").changed() {
        actions.tool.set_path_fill_enabled = Some(fill);
    }
    if shape_color_chip(ui, style.fill_color, "Fill colour") {
        actions.dialogs.open_paint_color_dialog = Some(5);
    }
    let mut fill_overprint = style.fill_overprint;
    if ui
        .add_enabled(
            style.fill_enabled,
            egui::Checkbox::new(&mut fill_overprint, "OP Fill"),
        )
        .on_hover_text("Overprint Fill — preserve underlying inks on output")
        .changed()
    {
        actions.tool.set_path_fill_overprint = Some(fill_overprint);
    }
    let mut fill_kind = style.fill_kind;
    egui::ComboBox::from_id_salt("path_fill_kind")
        .selected_text(match fill_kind {
            1 => "Linear",
            2 => "Radial",
            _ => "Solid",
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut fill_kind, 0, "Solid");
            ui.selectable_value(&mut fill_kind, 1, "Linear");
            ui.selectable_value(&mut fill_kind, 2, "Radial");
        });
    if fill_kind != style.fill_kind {
        actions.tool.set_path_fill_kind = Some(fill_kind);
    }
    if style.fill_kind != 0 && shape_color_chip(ui, style.fill_end_color, "Gradient end colour") {
        actions.dialogs.open_paint_color_dialog = Some(7);
    }
    if style.fill_kind != 0 {
        ui.menu_button(format!("Stops: {}", style.gradient_stop_count), |ui| {
            ui.set_min_width(285.0);
            ui.label("Gradient stops");
            ui.separator();
            let count = style.gradient_stop_count as usize;
            for index in 0..count {
                ui.horizontal(|ui| {
                    ui.label(format!("{}.", index + 1));
                    if shape_color_chip(ui, style.gradient_stop_colors[index], "Edit stop colour") {
                        actions.dialogs.open_paint_color_dialog = Some(8 + index as u8);
                    }
                    let lo = if index == 0 {
                        0.0
                    } else {
                        style.gradient_stop_offsets[index - 1]
                    };
                    let hi = if index + 1 == count {
                        1.0
                    } else {
                        style.gradient_stop_offsets[index + 1]
                    };
                    let mut percent = style.gradient_stop_offsets[index] * 100.0;
                    let response = ui.add(
                        egui::DragValue::new(&mut percent)
                            .range((lo * 100.0)..=(hi * 100.0))
                            .suffix("%")
                            .speed(0.25)
                            .max_decimals(1),
                    );
                    if response.changed() {
                        actions.tool.set_path_gradient_stop_offset =
                            Some((index as u8, percent / 100.0));
                    }
                    if response.drag_stopped() || response.lost_focus() {
                        actions.tool.commit_path_style = true;
                    }
                    if ui
                        .add_enabled(count > 2, egui::Button::new("Delete"))
                        .clicked()
                    {
                        actions.tool.remove_path_gradient_stop = Some(index as u8);
                    }
                });
            }
            ui.separator();
            if ui
                .add_enabled(
                    count < crate::core::vector::style::MAX_GRADIENT_STOPS,
                    egui::Button::new("Add stop"),
                )
                .clicked()
            {
                actions.tool.add_path_gradient_stop = true;
            }
            ui.label(
                egui::RichText::new("Stops stay sorted; minimum 2, maximum 8")
                    .size(10.0)
                    .color(egui::Color32::from_gray(140)),
            );
        });
    }
    ui.separator();
    // Outline.
    let mut stroke = style.stroke_enabled;
    if ui.checkbox(&mut stroke, "Outline").changed() {
        actions.tool.set_path_stroke_enabled = Some(stroke);
    }
    if shape_color_chip(ui, style.stroke_color, "Outline colour") {
        actions.dialogs.open_paint_color_dialog = Some(6);
    }
    let mut stroke_overprint = style.stroke_overprint;
    if ui
        .add_enabled(
            style.stroke_enabled,
            egui::Checkbox::new(&mut stroke_overprint, "OP Line"),
        )
        .on_hover_text("Overprint Outline — preserve underlying inks on output")
        .changed()
    {
        actions.tool.set_path_stroke_overprint = Some(stroke_overprint);
    }
    ui.label("Width:");
    let mut w = style.stroke_width;
    let resp = ui.add(
        egui::DragValue::new(&mut w)
            .range(0.0..=500.0)
            .suffix(" px")
            .speed(0.2),
    );
    if resp.changed() {
        actions.tool.set_path_stroke_width = Some(w);
    }
    // One undo step per scrub: commit when the drag stops or the field loses focus.
    if resp.drag_stopped() || resp.lost_focus() {
        actions.tool.commit_path_style = true;
    }
    let mut dash_kind = style.dash_kind;
    egui::ComboBox::from_id_salt("path_dash_kind")
        .selected_text(match dash_kind {
            1 => "Dashed",
            2 => "Dotted",
            _ => "Solid",
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut dash_kind, 0, "Solid");
            ui.selectable_value(&mut dash_kind, 1, "Dashed");
            ui.selectable_value(&mut dash_kind, 2, "Dotted");
        });
    if dash_kind != style.dash_kind {
        actions.tool.set_path_dash_kind = Some(dash_kind);
    }
    ui.add_enabled_ui(style.dash_len > 0, |ui| {
        ui.menu_button("Dash\u{2026}", |ui| {
            ui.set_min_width(330.0);

            let mut values = style.dash_values;
            let mut len = style.dash_len.clamp(1, 8);
            ui.horizontal(|ui| {
                ui.label("Segments:");
                let response = ui.add(egui::DragValue::new(&mut len).range(1..=8));
                if response.changed() {
                    let old_len = style.dash_len.max(1) as usize;
                    for index in old_len..len as usize {
                        values[index] = values[index % old_len].max(0.01);
                    }
                    actions.tool.set_path_dash_values = Some((values, len));
                }
                if response.drag_stopped() || response.lost_focus() {
                    actions.tool.commit_path_style = true;
                }
            });

            ui.label("Dash / gap lengths:");
            let mut values_changed = false;
            let mut values_finished = false;
            ui.horizontal_wrapped(|ui| {
                for (index, value) in values[..len as usize].iter_mut().enumerate() {
                    ui.label(format!("{}:", index + 1));
                    let response = ui.add(
                        egui::DragValue::new(value)
                            .range(0.01..=10_000.0)
                            .speed(0.1)
                            .max_decimals(2),
                    );
                    values_changed |= response.changed();
                    values_finished |= response.drag_stopped() || response.lost_focus();
                }
            });
            if values_changed {
                actions.tool.set_path_dash_values = Some((values, len));
            }
            if values_finished {
                actions.tool.commit_path_style = true;
            }

            ui.horizontal(|ui| {
                ui.label("Offset:");
                let mut offset = style.dash_offset;
                let response = ui.add(
                    egui::DragValue::new(&mut offset)
                        .range(-10_000.0..=10_000.0)
                        .speed(0.1)
                        .max_decimals(2),
                );
                if response.changed() {
                    actions.tool.set_path_dash_offset = Some(offset);
                }
                if response.drag_stopped() || response.lost_focus() {
                    actions.tool.commit_path_style = true;
                }
            });
        });
    });
}

fn align_button(ui: &mut egui::Ui, actions: &mut UiActions, align: LayerAlign, tooltip: &str) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(24.0, 22.0), egui::Sense::click());
    let resp = resp.on_hover_text(tooltip);
    let fill = if resp.hovered() {
        ui.visuals().widgets.hovered.bg_fill
    } else {
        ui.visuals().widgets.inactive.bg_fill
    };
    let icon_col = ui.visuals().text_color();
    ui.painter().rect_filled(rect, 2.0, fill);
    paint_align_icon(ui.painter(), rect, align, icon_col);
    if resp.clicked() {
        actions.layers.align_layers = Some(align);
    }
}

fn move_transform_button(
    ui: &mut egui::Ui,
    actions: &mut UiActions,
    action: MoveTransformAction,
    icon: &str,
    tooltip: &str,
) {
    if ui
        .add(
            egui::Button::new(
                egui::RichText::new(icon)
                    .size(14.0)
                    .color(ui.visuals().text_color()),
            )
            .min_size(egui::vec2(24.0, 22.0)),
        )
        .on_hover_text(tooltip)
        .clicked()
    {
        actions.tool.move_transform = Some(action);
    }
}

fn paint_align_icon(
    painter: &egui::Painter,
    rect: egui::Rect,
    align: LayerAlign,
    color: egui::Color32,
) {
    let stroke = egui::Stroke::new(1.4_f32, color);
    let pad = 5.0;
    let r = rect.shrink(4.0);

    match align {
        LayerAlign::Left | LayerAlign::HorizontalCenter | LayerAlign::Right => {
            let guide_x = match align {
                LayerAlign::Left => r.left(),
                LayerAlign::HorizontalCenter => r.center().x,
                LayerAlign::Right => r.right(),
                _ => r.left(),
            };
            painter.line_segment(
                [
                    egui::pos2(guide_x, r.top()),
                    egui::pos2(guide_x, r.bottom()),
                ],
                stroke,
            );
            let bar_h = 4.0;
            for (y, w) in [(r.top() + 2.0, 10.0), (r.bottom() - 6.0, 14.0)] {
                let x = match align {
                    LayerAlign::Left => guide_x + pad,
                    LayerAlign::HorizontalCenter => guide_x - w * 0.5,
                    LayerAlign::Right => guide_x - pad - w,
                    _ => guide_x,
                };
                painter.rect_filled(
                    egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, bar_h)),
                    0.8,
                    color,
                );
            }
        }
        LayerAlign::Top | LayerAlign::VerticalCenter | LayerAlign::Bottom => {
            let guide_y = match align {
                LayerAlign::Top => r.top(),
                LayerAlign::VerticalCenter => r.center().y,
                LayerAlign::Bottom => r.bottom(),
                _ => r.top(),
            };
            painter.line_segment(
                [
                    egui::pos2(r.left(), guide_y),
                    egui::pos2(r.right(), guide_y),
                ],
                stroke,
            );
            let bar_w = 4.0;
            for (x, h) in [(r.left() + 3.0, 10.0), (r.right() - 7.0, 14.0)] {
                let y = match align {
                    LayerAlign::Top => guide_y + pad,
                    LayerAlign::VerticalCenter => guide_y - h * 0.5,
                    LayerAlign::Bottom => guide_y - pad - h,
                    _ => guide_y,
                };
                painter.rect_filled(
                    egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(bar_w, h)),
                    0.8,
                    color,
                );
            }
        }
    }
}

fn zoom_options(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    if ui.small_button("Zoom In").clicked() {
        actions.doc.zoom_in = true;
    }
    if ui.small_button("Zoom Out").clicked() {
        actions.doc.zoom_out = true;
    }
    ui.separator();
    if ui.small_button("Fit Screen").clicked() {
        actions.doc.fit_to_screen = true;
    }
    if ui.small_button("100%").clicked() {
        actions.doc.zoom_100 = true;
    }
    ui.separator();
    ui.label(format!("{:.0}%", data.doc.zoom * 100.0));
}

fn default_options(ui: &mut egui::Ui, data: &UiData) {
    let pal = data.chrome.theme_mode.palette();
    ui.label(egui::RichText::new(data.tool.active_tool.name()).color(pal.text_secondary));
}

fn smart_select_options(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    use crate::core::selection::SelectionMode;
    let pal = data.chrome.theme_mode.palette();

    ui.label("Mode:");
    let modes = [
        (SelectionMode::New, "New", "Replace"),
        (SelectionMode::Add, ph::PLUS, "Add to selection (Shift)"),
        (
            SelectionMode::Subtract,
            ph::MINUS,
            "Subtract from selection (Alt)",
        ),
        (
            SelectionMode::Intersect,
            ph::INTERSECT,
            "Intersect with selection (Shift+Alt)",
        ),
    ];
    for (mode, label, tooltip) in &modes {
        let selected = data.sel.selection_mode == *mode;
        let btn = egui::Button::new(*label)
            .fill(if selected {
                ui.visuals().selection.bg_fill
            } else {
                ui.visuals().widgets.inactive.bg_fill
            })
            .min_size(egui::vec2(24.0, 22.0));
        if ui.add(btn).on_hover_text(*tooltip).clicked() {
            actions.sel.set_selection_mode = Some(*mode);
        }
    }

    ui.separator();

    ui.label("Size:")
        .on_hover_text("Brush radius (px). Alt+Right-drag to resize.");
    let mut size = data.tool.wand_brush_size;
    if ui
        .add(
            egui::DragValue::new(&mut size)
                .range(1.0..=2000.0)
                .suffix("px")
                .speed(0.5),
        )
        .changed()
    {
        actions.tool.set_wand_brush_size = Some(size);
    }

    ui.separator();

    ui.label("Tolerance:").on_hover_text("Colour distance allowed when expanding. 0 = colour-blind (pure brush), 255 = flood entire region.");
    let mut tol = data.tool.wand_tolerance;
    if ui
        .add(egui::DragValue::new(&mut tol).range(0..=255).speed(1.0))
        .changed()
    {
        actions.tool.set_wand_tolerance = Some(tol);
    }

    ui.separator();

    ui.label("Edge:").on_hover_text("Edge sensitivity: stop expanding at luminance edges.\n0 = off (pure brush)  |  50 = portrait/person  |  100 = hard edges only");
    let mut edge = data.tool.wand_edge_sensitivity;
    if ui
        .add(egui::Slider::new(&mut edge, 0u8..=100u8).show_value(true))
        .changed()
    {
        actions.tool.set_wand_edge_sensitivity = Some(edge);
    }

    ui.separator();
    let mut merged = data.tool.wand_sample_merged;
    if ui.checkbox(&mut merged, "Sample All Layers").changed() {
        actions.tool.set_wand_sample_merged = Some(merged);
    }

    ui.separator();

    let busy = data.sel.select_subject_busy;
    let mut selected_model = data.sel.select_subject_model;
    egui::ComboBox::from_id_salt("select_subject_model")
        .selected_text(selected_model.short_label())
        .width(105.0)
        .show_ui(ui, |ui| {
            for model in crate::core::select_subject::SelectSubjectModel::ALL {
                if ui
                    .selectable_value(&mut selected_model, model, model.label())
                    .clicked()
                {
                    actions.sel.set_select_subject_model = Some(model);
                }
            }
        });
    let btn_label = if busy {
        format!("{} Running…", ph::HOURGLASS)
    } else {
        format!("{} Select Subject", ph::SPARKLE)
    };
    let btn_text = egui::RichText::new(btn_label).color(pal.text_primary);
    let btn = egui::Button::new(btn_text)
        .fill(if busy { pal.pressed_bg } else { pal.button_bg })
        .stroke(egui::Stroke::new(1.0_f32, pal.border_subtle))
        .min_size(egui::vec2(120.0, 22.0));
    let resp = ui.add_enabled(!busy, btn).on_hover_text(
        if !data.sel.select_subject_status_msg.is_empty() {
            data.sel.select_subject_status_msg.as_str()
        } else {
            "Auto-select subject / person using the selected local AI model"
        },
    );
    if resp.clicked() {
        actions.sel.trigger_select_subject = true;
    }

    if data.sel.has_selection {
        ui.separator();
        let sm_btn = egui::Button::new("Refine Selection\u{2026}")
            .fill(ui.visuals().widgets.inactive.bg_fill)
            .min_size(egui::vec2(130.0, 22.0));
        if ui
            .add(sm_btn)
            .on_hover_text("Refine selection edges: feather, smooth, shift, contrast")
            .clicked()
        {
            actions.sel.open_refine_panel = true;
        }
    }
}

fn selection_options(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    use crate::core::selection::SelectionMode;

    ui.label("Mode:");

    let modes = [
        (SelectionMode::New, "New", "Replace"),
        (SelectionMode::Add, ph::PLUS, "Add to selection"),
        (
            SelectionMode::Subtract,
            ph::MINUS,
            "Subtract from selection",
        ),
        (
            SelectionMode::Intersect,
            ph::INTERSECT,
            "Intersect with selection",
        ),
    ];

    for (mode, label, tooltip) in &modes {
        let selected = data.sel.selection_mode == *mode;
        let btn = egui::Button::new(*label)
            .fill(if selected {
                ui.visuals().selection.bg_fill
            } else {
                ui.visuals().widgets.inactive.bg_fill
            })
            .min_size(egui::vec2(24.0, 22.0));
        if ui.add(btn).on_hover_text(*tooltip).clicked() {
            actions.sel.set_selection_mode = Some(*mode);
        }
    }

    ui.separator();

    if ui.button("Feather...").clicked() {
        actions.sel.show_feather_dialog = Some(true);
    }

    ui.separator();

    if ui
        .small_button("Grow")
        .on_hover_text("Grow selection by 1px")
        .clicked()
    {
        actions.sel.selection_grow = Some(1);
    }
    if ui
        .small_button("Shrink")
        .on_hover_text("Shrink selection by 1px")
        .clicked()
    {
        actions.sel.selection_shrink = Some(1);
    }
}

fn text_options(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    let pal = data.chrome.theme_mode.palette();
    if !data.tool.text_font_available {
        ui.colored_label(
            pal.warning,
            format!(
                "{} No system font found — text cannot be rendered",
                ph::WARNING
            ),
        );
        return;
    }

    let panel_btn = egui::Button::new(egui::RichText::new(ph::TEXT_T).size(14.0).color(pal.icon))
        .min_size(egui::vec2(26.0, 22.0));
    if ui.add(panel_btn).on_hover_text("Show Text panel").clicked() {
        actions.chrome.show_text_panel = Some(true);
    }

    ui.separator();
    ui.label("Size:");
    let mut px = data.tool.text_font_px;
    if ui
        .add(
            egui::DragValue::new(&mut px)
                .range(4.0..=1600.0)
                .suffix(" px")
                .speed(0.5),
        )
        .changed()
    {
        actions.tool.set_text_font_px = Some(px);
    }

    ui.separator();
    for (selected, icon, tip, target) in [
        (data.tool.text_bold, ph::TEXT_B, "Bold", 0u8),
        (data.tool.text_italic, ph::TEXT_ITALIC, "Italic", 1),
        (data.tool.text_underline, ph::TEXT_UNDERLINE, "Underline", 2),
    ] {
        let btn = egui::Button::new(egui::RichText::new(icon).size(14.0))
            .fill(if selected {
                pal.accent_selected_bg
            } else {
                pal.button_bg
            })
            .min_size(egui::vec2(26.0, 22.0));
        if ui.add(btn).on_hover_text(tip).clicked() {
            match target {
                0 => actions.tool.set_text_bold = Some(!data.tool.text_bold),
                1 => actions.tool.set_text_italic = Some(!data.tool.text_italic),
                _ => actions.tool.set_text_underline = Some(!data.tool.text_underline),
            }
        }
    }

    ui.separator();
    for (align, icon, tip) in [
        (TextAlign::Left, ph::TEXT_ALIGN_LEFT, "Left align"),
        (TextAlign::Center, ph::TEXT_ALIGN_CENTER, "Center align"),
        (TextAlign::Right, ph::TEXT_ALIGN_RIGHT, "Right align"),
    ] {
        let selected = data.tool.text_align == align;
        let btn = egui::Button::new(egui::RichText::new(icon).size(14.0))
            .fill(if selected {
                pal.accent_selected_bg
            } else {
                pal.button_bg
            })
            .min_size(egui::vec2(26.0, 22.0));
        if ui.add(btn).on_hover_text(tip).clicked() {
            actions.tool.set_text_align = Some(align);
        }
    }

    if data.tool.text_editing {
        ui.separator();
        let text_color_dialog_open =
            data.dialogs.show_paint_color_dialog && data.dialogs.paint_color_dialog_target == 2;
        let commit_btn = modal_flash_btn(
            egui::Button::new(
                egui::RichText::new(format!("{} Commit", ph::CHECK)).color(pal.success),
            ),
            ui,
            data,
        );
        let commit_tip = if text_color_dialog_open {
            "Finish the text color picker first"
        } else {
            "Finalize text (Esc)"
        };
        if ui
            .add_enabled(!text_color_dialog_open, commit_btn)
            .on_hover_text(commit_tip)
            .clicked()
        {
            actions.tool.text_commit = true;
        }
        let cancel_btn = modal_flash_btn(
            egui::Button::new(egui::RichText::new(format!("{} Cancel", ph::X)).color(pal.danger)),
            ui,
            data,
        );
        if ui.add(cancel_btn).clicked() {
            actions.tool.text_cancel = true;
        }
    } else {
        ui.label(
            egui::RichText::new("Click the canvas to add text")
                .size(10.5)
                .color(pal.text_disabled),
        );
    }
}

fn transform_options(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    let pal = data.chrome.theme_mode.palette();
    ui.label("W:");
    let mut sx = data.tool.transform_scale_x * 100.0;
    if ui
        .add(
            egui::DragValue::new(&mut sx)
                .range(-1000.0..=1000.0)
                .suffix("%")
                .speed(0.5)
                .min_decimals(1)
                .max_decimals(2),
        )
        .changed()
    {
        actions.tool.set_transform_scale_x = Some(sx / 100.0);
    }

    ui.label("H:");
    let mut sy = data.tool.transform_scale_y * 100.0;
    if ui
        .add(
            egui::DragValue::new(&mut sy)
                .range(-1000.0..=1000.0)
                .suffix("%")
                .speed(0.5)
                .min_decimals(1)
                .max_decimals(2),
        )
        .changed()
    {
        actions.tool.set_transform_scale_y = Some(sy / 100.0);
    }

    ui.separator();

    ui.label(format!("{}:", ph::ANGLE));
    let mut angle = data.tool.transform_angle;
    if ui
        .add(
            egui::DragValue::new(&mut angle)
                .range(-360.0..=360.0)
                .suffix("°")
                .speed(0.5)
                .min_decimals(1)
                .max_decimals(2),
        )
        .changed()
    {
        actions.tool.set_transform_angle = Some(angle);
    }

    ui.separator();

    ui.label("X:");
    let mut tx = data.tool.transform_tx;
    if ui
        .add(egui::DragValue::new(&mut tx).suffix("px").speed(1.0))
        .changed()
    {
        actions.tool.set_transform_tx = Some(tx);
    }
    ui.label("Y:");
    let mut ty = data.tool.transform_ty;
    if ui
        .add(egui::DragValue::new(&mut ty).suffix("px").speed(1.0))
        .changed()
    {
        actions.tool.set_transform_ty = Some(ty);
    }

    ui.separator();

    ui.label("Interpolation:");
    let mut current = data.tool.transform_interpolation;
    egui::ComboBox::from_id_salt("transform_interpolation_combo")
        .selected_text(match current {
            crate::core::geometry::InterpolationMode::Bilinear => "Bilinear",
            crate::core::geometry::InterpolationMode::NearestNeighbor => "Nearest Neighbor",
        })
        .width(130.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(
                &mut current,
                crate::core::geometry::InterpolationMode::Bilinear,
                "Bilinear",
            );
            ui.selectable_value(
                &mut current,
                crate::core::geometry::InterpolationMode::NearestNeighbor,
                "Nearest Neighbor",
            );
        });
    if current != data.tool.transform_interpolation {
        actions.tool.set_transform_interpolation = Some(current);
    }

    ui.separator();
    let cancel_btn = egui::Button::new(egui::RichText::new(ph::X).size(13.0).color(pal.danger))
        .min_size(egui::vec2(26.0, 22.0));
    let cancel_btn = modal_flash_btn(cancel_btn, ui, data);
    if ui
        .add(cancel_btn)
        .on_hover_text("Hủy transform (Esc)")
        .clicked()
    {
        actions.tool.transform_cancel = true;
    }
    let commit_btn =
        egui::Button::new(egui::RichText::new(ph::CHECK).size(13.0).color(pal.success))
            .min_size(egui::vec2(26.0, 22.0));
    let commit_btn = modal_flash_btn(commit_btn, ui, data);
    if ui
        .add(commit_btn)
        .on_hover_text("Xác nhận transform (Enter)")
        .clicked()
    {
        actions.tool.transform_commit = true;
    }
}

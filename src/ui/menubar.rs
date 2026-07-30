use super::{UiActions, UiData};
use crate::formats::ExportFormat;
use egui;
use egui_phosphor::regular as ph;

pub const TITLEBAR_H: f32 = 28.0;

fn load_logo(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let cache_id = egui::Id::new("iai_logo_tex");
    let cached = ctx.data(|d| d.get_temp::<egui::TextureHandle>(cache_id));
    if let Some(tex) = cached {
        return Some(tex);
    }
    let bytes = include_bytes!("../../logo_iAi.png");
    // The logo may ship far larger than it renders; clamp its side to a size every
    // GPU accepts (older cards cap texture sides at 2048) before uploading it.
    let src = image::load_from_memory(bytes).ok()?;
    let src = if src.width().max(src.height()) > 512 {
        src.resize(512, 512, image::imageops::FilterType::Lanczos3)
    } else {
        src
    };
    let img = src.into_rgba8();
    let (w, h) = img.dimensions();
    let color_image =
        egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw());
    let tex = ctx.load_texture("iai_logo", color_image, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(cache_id, tex.clone()));
    Some(tex)
}

#[allow(deprecated)]
pub fn build(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let logo_tex = load_logo(ctx);
    let pal = data.chrome.theme_mode.palette();

    egui::TopBottomPanel::top("titlebar")
        .exact_size(TITLEBAR_H)
        .frame(
            egui::Frame::new()
                .fill(pal.top_bar_bg)
                .inner_margin(egui::Margin::ZERO),
        )
        .show(ctx, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            {
                let visuals = ui.visuals_mut();
                visuals.widgets.inactive.bg_stroke = egui::Stroke::NONE;
                visuals.widgets.hovered.bg_stroke = egui::Stroke::NONE;
                visuals.widgets.active.bg_stroke = egui::Stroke::NONE;
                visuals.widgets.open.bg_stroke = egui::Stroke::NONE;
            }

            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;

                ui.add_space(6.0);
                if let Some(ref tex) = logo_tex {
                    let tex_size = tex.size_vec2();
                    let icon_h = 20.0_f32;
                    let icon_w = icon_h * tex_size.x / tex_size.y;
                    let (icon_rect, _) =
                        ui.allocate_exact_size(egui::vec2(icon_w, icon_h), egui::Sense::hover());
                    let logo_color = pal.text_primary;
                    ui.painter().image(
                        tex.id(),
                        icon_rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        logo_color,
                    );
                } else {
                    ui.label(
                        egui::RichText::new("P")
                            .size(16.0)
                            .strong()
                            .color(pal.accent_primary),
                    );
                }
                ui.add_space(6.0);

                ui.add(egui::Separator::default().vertical());
                ui.add_space(2.0);

                ui.spacing_mut().item_spacing.x = 2.0;

                // Strict modal lock: the menus are disabled while a modal
                // operation is open; clicking them rings the bell. The window
                // buttons further right stay live.
                let menus = ui.add_enabled_ui(!data.chrome.is_tool_modal, |ui| {
                    ui.scope(|ui| {
                        let visuals = ui.visuals_mut();
                        visuals.window_fill = pal.panel_bg;
                        visuals.window_stroke = egui::Stroke::new(1.0_f32, pal.border_subtle);
                        visuals.widgets.hovered.bg_fill = pal.hover_bg;
                        visuals.widgets.hovered.bg_stroke = egui::Stroke::NONE;
                        visuals.widgets.hovered.fg_stroke =
                            egui::Stroke::new(1.0_f32, pal.text_primary);

                        ui.menu_button("File", |ui| {
                            if ui.add(menu_item("New", "Ctrl+N")).clicked() {
                                actions.dialogs.show_new_dialog = Some(true);
                                ui.close();
                            }
                            if ui.add(menu_item("Open...", "Ctrl+O")).clicked() {
                                actions.doc.open_file = true;
                                ui.close();
                            }
                            ui.menu_button("Open Recent", |ui| {
                                if ui.button("Clear Recent").clicked() {
                                    ui.close();
                                }
                                ui.separator();
                                ui.label(
                                    egui::RichText::new("(No recent files)")
                                        .color(pal.text_secondary)
                                        .size(11.0),
                                );
                            });
                            if ui
                                .add(menu_item_enabled("Close", "Ctrl+W", data.doc.has_doc))
                                .clicked()
                            {
                                actions.doc.close_doc_tab = Some(data.doc.active_doc_idx);
                                ui.close();
                            }
                            ui.separator();
                            if ui.add(menu_item("Save", "Ctrl+S")).clicked() {
                                actions.doc.save = true;
                                ui.close();
                            }
                            if ui.add(menu_item("Save As...", "Ctrl+Shift+S")).clicked() {
                                actions.doc.save_as = true;
                                ui.close();
                            }
                            ui.separator();
                            ui.menu_button("Export As", |ui| {
                                for fmt in ExportFormat::all_export() {
                                    if ui.button(format!("{}...", fmt.name())).clicked() {
                                        actions.doc.export = Some(fmt);
                                        actions.dialogs.show_export_dialog = Some(true);
                                        ui.close();
                                    }
                                }
                            });
                            ui.separator();
                            if ui
                                .add(menu_item_enabled("Print...", "Ctrl+P", data.doc.has_doc))
                                .clicked()
                            {
                                actions.print.show_print_dialog = Some(true);
                                ui.close();
                            }
                            if ui
                                .add(menu_item_enabled(
                                    "PDF (multi-page)...",
                                    "",
                                    data.doc.doc_count > 0,
                                ))
                                .clicked()
                            {
                                actions.print.export_multipage_pdf = true;
                                ui.close();
                            }
                            if ui
                                .add(menu_item_enabled(
                                    "CMYK Separations...",
                                    "",
                                    data.doc.has_doc,
                                ))
                                .clicked()
                            {
                                actions.print.export_cmyk_separations = true;
                                ui.close();
                            }
                            ui.separator();
                            if ui.add(menu_item("Preferences", "Ctrl+,")).clicked() {
                                actions.dialogs.show_preferences = Some(true);
                                ui.close();
                            }
                            ui.separator();
                            if ui.button("Exit").clicked() {
                                if data.doc.is_modified {
                                    actions.dialogs.show_exit_dialog = Some(true);
                                } else {
                                    actions.doc.exit = true;
                                }
                                ui.close();
                            }
                        });
                    });

                    ui.menu_button("Edit", |ui| {
                        if ui
                            .add(menu_item_enabled("Undo", "Ctrl+Z", data.doc.undo_count > 0))
                            .clicked()
                        {
                            actions.doc.undo = true;
                            ui.close();
                        }
                        if ui
                            .add(menu_item_enabled(
                                "Redo",
                                "Ctrl+Shift+Z",
                                data.doc.redo_count > 0,
                            ))
                            .clicked()
                        {
                            actions.doc.redo = true;
                            ui.close();
                        }
                        ui.separator();
                        if ui
                            .add(menu_item_enabled("Cut", "Ctrl+X", data.sel.has_selection))
                            .clicked()
                        {
                            actions.doc.cut = true;
                            ui.close();
                        }
                        if ui.add(menu_item("Copy", "Ctrl+C")).clicked() {
                            actions.doc.copy = true;
                            ui.close();
                        }
                        if ui.add(menu_item("Paste", "Ctrl+V")).clicked() {
                            actions.doc.paste = true;
                            ui.close();
                        }
                        if ui
                            .add(menu_item_enabled(
                                "Free Transform",
                                "Ctrl+T",
                                data.doc.has_doc && !data.doc.is_cmyk,
                            ))
                            .clicked()
                        {
                            actions.tool.start_transform = true;
                            ui.close();
                        }
                        ui.separator();
                        if ui
                            .add(menu_item_enabled(
                                "Fill Foreground",
                                "Alt+Delete",
                                data.doc.has_doc,
                            ))
                            .clicked()
                        {
                            actions.doc.fill_foreground = true;
                            ui.close();
                        }
                        if ui
                            .add(menu_item_enabled(
                                "Fill Background",
                                "Ctrl+Delete",
                                data.doc.has_doc,
                            ))
                            .clicked()
                        {
                            actions.doc.fill_background = true;
                            ui.close();
                        }
                        if ui
                            .add(menu_item_enabled("Clear", "Delete", data.sel.has_selection))
                            .clicked()
                        {
                            actions.sel.clear_selection_pixels = true;
                            ui.close();
                        }
                        if ui
                            .add(menu_item_enabled(
                                "Smart Fill",
                                "Shift+F5",
                                data.sel.has_selection,
                            ))
                            .clicked()
                        {
                            actions.dialogs.smart_fill_fill = true;
                            ui.close();
                        }
                        ui.separator();
                        if ui.add(menu_item("Preferences", "Ctrl+,")).clicked() {
                            actions.dialogs.show_preferences = Some(true);
                            ui.close();
                        }
                    });

                    ui.menu_button("Image", |ui| {
                        if ui.add(menu_item("Canvas Size...", "")).clicked() {
                            actions.dialogs.show_resize_dialog = Some(true);
                            ui.close();
                        }
                        if ui.add(menu_item("Image Size...", "Ctrl+Alt+I")).clicked() {
                            actions.dialogs.show_image_size_dialog = Some(true);
                            ui.close();
                        }
                        ui.separator();
                        ui.menu_button("Mode", |ui| {
                            if ui
                                .selectable_label(!data.doc.is_cmyk, "RGB Color")
                                .clicked()
                            {
                                if data.doc.is_cmyk {
                                    actions.doc.convert_rgb_mode = true;
                                }
                                ui.close();
                            }
                            let cmyk_label = if data.doc.is_cmyk {
                                format!("CMYK Color — {}", data.doc.cmyk_profile_name)
                            } else {
                                "CMYK Color…".to_string()
                            };
                            if ui
                                .add_enabled(
                                    data.doc.has_doc,
                                    egui::SelectableLabel::new(data.doc.is_cmyk, cmyk_label),
                                )
                                .clicked()
                            {
                                if !data.doc.is_cmyk {
                                    actions.doc.show_cmyk_convert_dialog = Some(true);
                                }
                                ui.close();
                            }
                            ui.separator();
                            // Bit depth applies to RGB documents (CMYK ink is 8-bit).
                            ui.add_enabled_ui(!data.doc.is_cmyk, |ui| {
                                if ui
                                    .selectable_label(
                                        data.doc.canvas_bit_depth == 8,
                                        "8 Bits/Channel",
                                    )
                                    .clicked()
                                {
                                    actions.doc.set_bit_depth = Some(false);
                                    ui.close();
                                }
                                if ui
                                    .selectable_label(
                                        data.doc.canvas_bit_depth == 16,
                                        "16 Bits/Channel",
                                    )
                                    .clicked()
                                {
                                    actions.doc.set_bit_depth = Some(true);
                                    ui.close();
                                }
                            });
                            ui.separator();
                            if ui
                                .add_enabled(
                                    data.doc.has_doc && !data.doc.is_cmyk,
                                    egui::Button::new("Grayscale"),
                                )
                                .clicked()
                            {
                                actions.doc.convert_grayscale = true;
                                ui.close();
                            }
                        });
                        ui.menu_button("Color Profile", |ui| {
                            ui.label(
                                egui::RichText::new("Assign Profile")
                                    .color(pal.text_secondary)
                                    .size(11.0),
                            );
                            for wp in crate::core::cms::WorkingProfile::all() {
                                if ui
                                    .add_enabled(data.doc.has_doc, egui::Button::new(wp.name()))
                                    .clicked()
                                {
                                    actions.doc.assign_profile = Some(*wp);
                                    ui.close();
                                }
                            }
                            ui.separator();
                            ui.label(
                                egui::RichText::new("Convert to Profile")
                                    .color(pal.text_secondary)
                                    .size(11.0),
                            );
                            for wp in crate::core::cms::WorkingProfile::all() {
                                if ui
                                    .add_enabled(data.doc.has_doc, egui::Button::new(wp.name()))
                                    .clicked()
                                {
                                    actions.doc.convert_profile = Some(*wp);
                                    ui.close();
                                }
                            }
                        });
                        ui.separator();
                        ui.menu_button("Rotate Canvas", |ui| {
                            if ui.button("90° Clockwise").clicked() {
                                actions.doc.rotate_cw = true;
                                ui.close();
                            }
                            if ui.button("90° Counter-Clockwise").clicked() {
                                actions.doc.rotate_ccw = true;
                                ui.close();
                            }
                        });
                        ui.menu_button("Flip Canvas", |ui| {
                            if ui.button("Flip Horizontal").clicked() {
                                actions.doc.flip_horizontal = true;
                                ui.close();
                            }
                            if ui.button("Flip Vertical").clicked() {
                                actions.doc.flip_vertical = true;
                                ui.close();
                            }
                        });
                        ui.separator();
                        if ui.add(menu_item("Develop...", "Ctrl+Shift+A")).clicked() {
                            actions.develop.open_develop_dialog = true;
                            ui.close();
                        }
                        ui.separator();
                        ui.menu_button("Adjustments", |ui| {
                            use crate::core::layer::AdjustmentType;
                            if ui.add(menu_item("Levels...", "Ctrl+L")).clicked() {
                                actions.dialogs.open_adjustment_dialog =
                                    Some(AdjustmentType::default_levels());
                                ui.close();
                            }
                            if ui
                                .add(menu_item_enabled(
                                    "Auto Levels",
                                    "Ctrl+Shift+L",
                                    data.doc.has_doc,
                                ))
                                .clicked()
                            {
                                actions.dialogs.auto_levels = true;
                                ui.close();
                            }
                            if ui.add(menu_item("Curves...", "Ctrl+M")).clicked() {
                                actions.dialogs.open_adjustment_dialog =
                                    Some(AdjustmentType::default_curves());
                                ui.close();
                            }
                            if ui.add(menu_item("Color Balance...", "Ctrl+B")).clicked() {
                                actions.dialogs.open_adjustment_dialog =
                                    Some(AdjustmentType::ColorBalance {
                                        shadows: [0.0; 3],
                                        midtones: [0.0; 3],
                                        highlights: [0.0; 3],
                                        preserve_luminosity: true,
                                    });
                                ui.close();
                            }
                            if ui.add(menu_item("Hue/Saturation...", "Ctrl+U")).clicked() {
                                actions.dialogs.open_adjustment_dialog =
                                    Some(AdjustmentType::HueSaturation {
                                        hue: 0.0,
                                        saturation: 0.0,
                                        lightness: 0.0,
                                    });
                                ui.close();
                            }
                            if ui
                                .add(menu_item_enabled(
                                    "Desaturate",
                                    "Ctrl+Shift+U",
                                    data.doc.has_doc,
                                ))
                                .clicked()
                            {
                                actions.dialogs.apply_direct_adjustment =
                                    Some(AdjustmentType::Desaturate);
                                ui.close();
                            }
                            if ui
                                .add(menu_item_enabled("Invert", "Ctrl+I", data.doc.has_doc))
                                .clicked()
                            {
                                actions.layers.invert_active = Some(data.layers.active_layer_idx);
                                ui.close();
                            }
                            ui.separator();
                            if ui.add(menu_item("Brightness/Contrast...", "")).clicked() {
                                actions.dialogs.open_adjustment_dialog =
                                    Some(AdjustmentType::BrightnessContrast {
                                        brightness: 0.0,
                                        contrast: 0.0,
                                    });
                                ui.close();
                            }
                            if ui.add(menu_item("Exposure...", "")).clicked() {
                                actions.dialogs.open_adjustment_dialog =
                                    Some(AdjustmentType::Exposure {
                                        exposure: 0.0,
                                        offset: 0.0,
                                        gamma: 1.0,
                                    });
                                ui.close();
                            }
                            if ui.add(menu_item("Vibrance...", "")).clicked() {
                                actions.dialogs.open_adjustment_dialog =
                                    Some(AdjustmentType::Vibrance {
                                        vibrance: 0.0,
                                        saturation: 0.0,
                                    });
                                ui.close();
                            }
                            ui.separator();
                            if ui.add(menu_item("Threshold...", "")).clicked() {
                                actions.dialogs.open_adjustment_dialog =
                                    Some(AdjustmentType::Threshold { value: 128 });
                                ui.close();
                            }
                            if ui.add(menu_item("Posterize...", "")).clicked() {
                                actions.dialogs.open_adjustment_dialog =
                                    Some(AdjustmentType::Posterize { levels: 4 });
                                ui.close();
                            }
                            ui.separator();
                            if ui.add(menu_item("Black & White...", "")).clicked() {
                                actions.dialogs.open_adjustment_dialog =
                                    Some(AdjustmentType::BlackAndWhite {
                                        r: 40.0,
                                        y: 60.0,
                                        g: 40.0,
                                        c: 60.0,
                                        b: 20.0,
                                        m: 80.0,
                                    });
                                ui.close();
                            }
                            if ui.add(menu_item("Channel Mixer...", "")).clicked() {
                                actions.dialogs.open_adjustment_dialog =
                                    Some(AdjustmentType::ChannelMixer {
                                        red: [1.0, 0.0, 0.0],
                                        green: [0.0, 1.0, 0.0],
                                        blue: [0.0, 0.0, 1.0],
                                        monochrome: false,
                                    });
                                ui.close();
                            }
                            if ui.add(menu_item("Photo Filter...", "")).clicked() {
                                actions.dialogs.open_adjustment_dialog =
                                    Some(AdjustmentType::PhotoFilter {
                                        color: [236, 138, 0],
                                        density: 0.25,
                                        luminosity: true,
                                    });
                                ui.close();
                            }
                            if ui.add(menu_item("Gradient Map...", "")).clicked() {
                                actions.dialogs.open_adjustment_dialog =
                                    Some(AdjustmentType::default_gradient_map());
                                ui.close();
                            }
                        });
                    });

                    ui.menu_button("Layer", |ui| {
                        if ui.button("New Layer").clicked() {
                            actions.layers.add_layer = true;
                            ui.close();
                        }
                        if ui
                            .add(menu_item_enabled(
                                "Duplicate Layer",
                                "",
                                data.layers.layer_count > 0,
                            ))
                            .clicked()
                        {
                            actions.layers.duplicate_layer = Some(data.layers.active_layer_idx);
                            ui.close();
                        }
                        if ui
                            .add(menu_item_enabled(
                                "Layer via Copy",
                                "Ctrl+J",
                                data.layers.layer_count > 0,
                            ))
                            .clicked()
                        {
                            actions.layers.layer_via_copy = true;
                            ui.close();
                        }
                        if ui
                            .add(menu_item_enabled(
                                "Delete Layer",
                                "Delete",
                                data.layers.layer_count > 1,
                            ))
                            .clicked()
                        {
                            actions.layers.remove_layer = Some(data.layers.active_layer_idx);
                            ui.close();
                        }
                        if ui
                            .add(menu_item_enabled(
                                "Rasterize Layer",
                                "",
                                data.layers
                                    .layer_types
                                    .get(data.layers.active_layer_idx)
                                    .is_some_and(|t| t == "Path"),
                            ))
                            .clicked()
                        {
                            actions.layers.rasterize_layer = Some(data.layers.active_layer_idx);
                            ui.close();
                        }
                        if ui
                            .add(menu_item_enabled(
                                "Convert to Curves",
                                "",
                                data.layers
                                    .layer_types
                                    .get(data.layers.active_layer_idx)
                                    .is_some_and(|t| t == "Shape"),
                            ))
                            .clicked()
                        {
                            actions.layers.convert_to_curves = Some(data.layers.active_layer_idx);
                            ui.close();
                        }
                        if ui
                            .add(menu_item_enabled(
                                "Convert Text to Curves",
                                "",
                                data.layers
                                    .layer_types
                                    .get(data.layers.active_layer_idx)
                                    .is_some_and(|t| t == "Text"),
                            ))
                            .clicked()
                        {
                            actions.layers.text_to_curves = Some(data.layers.active_layer_idx);
                            ui.close();
                        }
                        {
                            use crate::core::vector::boolean::BooleanOp;
                            // Enabled once at least two selected vector objects
                            // (Shape or Path, unlocked, non-background) exist.
                            let shaping_enabled = (0..data.layers.layer_count)
                                .filter(|&i| {
                                    data.layers.layer_selected.get(i).copied().unwrap_or(false)
                                        && !data
                                            .layers
                                            .layer_locked
                                            .get(i)
                                            .copied()
                                            .unwrap_or(false)
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
                                >= 2;
                            ui.add_enabled_ui(shaping_enabled, |ui| {
                                ui.menu_button("Shaping", |ui| {
                                    for (label, op) in [
                                        ("Weld", BooleanOp::Union),
                                        ("Trim", BooleanOp::Difference),
                                        ("Intersect", BooleanOp::Intersect),
                                        ("Simplify", BooleanOp::Exclude),
                                    ] {
                                        if ui.button(label).clicked() {
                                            actions.layers.boolean_op = Some(op);
                                            ui.close();
                                        }
                                    }
                                });
                            });
                        }
                        {
                            // PowerClip: place selected content inside a selected
                            // vector frame. Enabled when ≥2 layers are selected and
                            // at least one of them is a vector (the frame); the
                            // command re-validates the exact roles.
                            let sel = |i: usize| {
                                data.layers.layer_selected.get(i).copied().unwrap_or(false)
                                    && !data.layers.layer_locked.get(i).copied().unwrap_or(false)
                                    && !data
                                        .layers
                                        .layer_is_background
                                        .get(i)
                                        .copied()
                                        .unwrap_or(false)
                            };
                            let sel_total =
                                (0..data.layers.layer_count).filter(|&i| sel(i)).count();
                            let sel_vector = (0..data.layers.layer_count)
                                .filter(|&i| {
                                    sel(i)
                                        && data
                                            .layers
                                            .layer_types
                                            .get(i)
                                            .is_some_and(|t| t == "Shape" || t == "Path")
                                })
                                .count();
                            let place_enabled = sel_total >= 2 && sel_vector >= 1;
                            ui.menu_button("PowerClip", |ui| {
                                if ui
                                    .add(menu_item_enabled("Place Inside Frame", "", place_enabled))
                                    .on_hover_text(
                                        "Place the selected content inside the topmost vector \
                                         shape (content is clipped to the frame)",
                                    )
                                    .clicked()
                                {
                                    actions.layers.powerclip_place = true;
                                    ui.close();
                                }
                                if ui
                                    .button("Extract From Frame")
                                    .on_hover_text("Remove the clip: release the selected content")
                                    .clicked()
                                {
                                    actions.layers.powerclip_release = true;
                                    ui.close();
                                }
                            });
                        }
                        if ui
                            .add(menu_item_enabled(
                                "Clipping Mask",
                                "Ctrl+Alt+G",
                                data.layers.layer_count > 1,
                            ))
                            .on_hover_text(
                                "Clip the selected layer to the shape of the layer directly \
                                 below (click again to release). Works for both vector and pixel.",
                            )
                            .clicked()
                        {
                            actions.layers.toggle_clipping_mask = true;
                            ui.close();
                        }
                        ui.separator();
                        ui.menu_button("New Adjustment Layer", |ui| {
                            use crate::core::layer::AdjustmentType;
                            let adjs = [
                                (
                                    "Brightness/Contrast",
                                    AdjustmentType::BrightnessContrast {
                                        brightness: 0.0,
                                        contrast: 0.0,
                                    },
                                ),
                                (
                                    "Hue/Saturation",
                                    AdjustmentType::HueSaturation {
                                        hue: 0.0,
                                        saturation: 0.0,
                                        lightness: 0.0,
                                    },
                                ),
                                ("Levels", AdjustmentType::default_levels()),
                                ("Curves", AdjustmentType::default_curves()),
                                (
                                    "Vibrance",
                                    AdjustmentType::Vibrance {
                                        vibrance: 0.0,
                                        saturation: 0.0,
                                    },
                                ),
                                (
                                    "Exposure",
                                    AdjustmentType::Exposure {
                                        exposure: 0.0,
                                        offset: 0.0,
                                        gamma: 1.0,
                                    },
                                ),
                                ("Invert", AdjustmentType::Invert),
                                ("Threshold", AdjustmentType::Threshold { value: 128 }),
                                ("Posterize", AdjustmentType::Posterize { levels: 4 }),
                                ("Desaturate", AdjustmentType::Desaturate),
                                (
                                    "Black & White",
                                    AdjustmentType::BlackAndWhite {
                                        r: 40.0,
                                        y: 60.0,
                                        g: 40.0,
                                        c: 60.0,
                                        b: 20.0,
                                        m: 80.0,
                                    },
                                ),
                                (
                                    "Channel Mixer",
                                    AdjustmentType::ChannelMixer {
                                        red: [1.0, 0.0, 0.0],
                                        green: [0.0, 1.0, 0.0],
                                        blue: [0.0, 0.0, 1.0],
                                        monochrome: false,
                                    },
                                ),
                                (
                                    "Photo Filter",
                                    AdjustmentType::PhotoFilter {
                                        color: [236, 138, 0],
                                        density: 0.25,
                                        luminosity: true,
                                    },
                                ),
                                ("Gradient Map", AdjustmentType::default_gradient_map()),
                            ];
                            for (name, adj) in adjs {
                                if ui.button(name).clicked() {
                                    actions.layers.add_adjustment_layer = Some(adj);
                                    ui.close();
                                }
                            }
                        });
                        ui.separator();
                        if ui.add(menu_item("Group Layers", "Ctrl+G")).clicked() {
                            actions.layers.group_selected = true;
                            ui.close();
                        }
                        if ui
                            .add(menu_item("Ungroup Layers", "Ctrl+Shift+G"))
                            .clicked()
                        {
                            actions.layers.ungroup = Some(data.layers.active_layer_idx);
                            ui.close();
                        }
                        ui.separator();
                        if ui
                            .add(menu_item_enabled(
                                "Merge Selected",
                                "Ctrl+E",
                                data.layers.layer_count > 1,
                            ))
                            .clicked()
                        {
                            actions.layers.merge_selected = true;
                            ui.close();
                        }
                        if ui
                            .add(menu_item_enabled(
                                "Merge Down",
                                "",
                                data.layers.active_layer_idx > 0,
                            ))
                            .clicked()
                        {
                            actions.layers.merge_down = Some(data.layers.active_layer_idx);
                            ui.close();
                        }
                        if ui.add(menu_item("Stamp Visible", "Ctrl+Shift+E")).clicked() {
                            actions.layers.merge_visible = true;
                            ui.close();
                        }
                        if ui.button("Merge All").clicked() {
                            actions.layers.merge_all = true;
                            ui.close();
                        }
                        ui.separator();
                        ui.menu_button("Layer Mask", |ui| {
                            if ui.button("Add Mask (Reveal All)").clicked() {
                                actions.layers.add_mask_white = Some(data.layers.active_layer_idx);
                                ui.close();
                            }
                            if ui.button("Add Mask (Hide All)").clicked() {
                                actions.layers.add_mask_black = Some(data.layers.active_layer_idx);
                                ui.close();
                            }
                            if ui
                                .add(menu_item_enabled(
                                    "Apply Mask",
                                    "",
                                    data.layers
                                        .layer_has_mask
                                        .get(data.layers.active_layer_idx)
                                        .copied()
                                        .unwrap_or(false),
                                ))
                                .clicked()
                            {
                                actions.layers.apply_mask = Some(data.layers.active_layer_idx);
                                ui.close();
                            }
                            if ui
                                .add(menu_item_enabled(
                                    "Delete Mask",
                                    "",
                                    data.layers
                                        .layer_has_mask
                                        .get(data.layers.active_layer_idx)
                                        .copied()
                                        .unwrap_or(false),
                                ))
                                .clicked()
                            {
                                actions.layers.delete_mask = Some(data.layers.active_layer_idx);
                                ui.close();
                            }
                        });
                    });

                    ui.menu_button("Select", |ui| {
                        if ui
                            .add(menu_item_enabled("Select All", "Ctrl+A", data.doc.has_doc))
                            .clicked()
                        {
                            actions.sel.select_all = true;
                            ui.close();
                        }
                        if ui
                            .add(menu_item_enabled(
                                "Deselect",
                                "Ctrl+D",
                                data.sel.has_selection,
                            ))
                            .clicked()
                        {
                            actions.sel.deselect = true;
                            ui.close();
                        }
                        if ui
                            .add(menu_item_enabled(
                                "Invert Selection",
                                "Shift+F7",
                                data.sel.has_selection,
                            ))
                            .clicked()
                        {
                            actions.sel.invert_selection = true;
                            ui.close();
                        }
                        ui.separator();
                        if ui
                            .add(menu_item_enabled(
                                "Feather...",
                                "Shift+F6",
                                data.sel.has_selection,
                            ))
                            .clicked()
                        {
                            actions.sel.show_feather_dialog = Some(true);
                            ui.close();
                        }
                        if ui
                            .add(menu_item_enabled("Stroke...", "", data.sel.has_selection))
                            .clicked()
                        {
                            actions.sel.show_stroke_dialog = Some(true);
                            ui.close();
                        }
                        ui.menu_button("Modify", |ui| {
                            use crate::ui::SelectionModifyKind;
                            let has = data.sel.has_selection;
                            if ui
                                .add_enabled(has, egui::Button::new("Expand..."))
                                .clicked()
                            {
                                actions.sel.open_modify_dialog = Some(SelectionModifyKind::Expand);
                                ui.close();
                            }
                            if ui
                                .add_enabled(has, egui::Button::new("Contract..."))
                                .clicked()
                            {
                                actions.sel.open_modify_dialog =
                                    Some(SelectionModifyKind::Contract);
                                ui.close();
                            }
                            if ui
                                .add_enabled(has, egui::Button::new("Feather..."))
                                .clicked()
                            {
                                actions.sel.show_feather_dialog = Some(true);
                                ui.close();
                            }
                            if ui
                                .add_enabled(has, egui::Button::new("Smooth..."))
                                .clicked()
                            {
                                actions.sel.open_modify_dialog = Some(SelectionModifyKind::Smooth);
                                ui.close();
                            }
                            if ui
                                .add_enabled(has, egui::Button::new("Border..."))
                                .clicked()
                            {
                                actions.sel.open_modify_dialog = Some(SelectionModifyKind::Border);
                                ui.close();
                            }
                        });
                    });

                    ui.menu_button("Filter", |ui| {
                        use crate::core::filters::FilterType;
                        if ui.add(menu_item("Warp...", "Ctrl+Shift+X")).clicked() {
                            actions.dialogs.open_warp_dialog = true;
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Gaussian Blur...").clicked() {
                            actions.dialogs.open_filter_dialog =
                                Some(FilterType::GaussianBlur { radius: 2.0 });
                            ui.close();
                        }
                        if ui.button("Sharpen...").clicked() {
                            actions.dialogs.open_filter_dialog = Some(FilterType::Sharpen {
                                amount: 1.0,
                                radius: 2.0,
                            });
                            ui.close();
                        }
                        if ui.button("High Pass...").clicked() {
                            actions.dialogs.open_filter_dialog =
                                Some(FilterType::HighPass { radius: 3.0 });
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Add Noise...").clicked() {
                            actions.dialogs.open_filter_dialog = Some(FilterType::AddNoise {
                                amount: 25.0,
                                monochromatic: false,
                            });
                            ui.close();
                        }
                        if ui.button("Reduce Noise...").clicked() {
                            actions.dialogs.open_filter_dialog =
                                Some(FilterType::ReduceNoise { strength: 50.0 });
                            ui.close();
                        }
                        if ui.button("Pixelate...").clicked() {
                            actions.dialogs.open_filter_dialog =
                                Some(FilterType::Pixelate { cell: 8.0 });
                            ui.close();
                        }
                    });

                    ui.menu_button("View", |ui| {
                        if ui.add(menu_item("Zoom In", "Ctrl++")).clicked() {
                            actions.doc.zoom_in = true;
                            ui.close();
                        }
                        if ui.add(menu_item("Zoom Out", "Ctrl+-")).clicked() {
                            actions.doc.zoom_out = true;
                            ui.close();
                        }
                        if ui.add(menu_item("Fit to Screen", "Ctrl+0")).clicked() {
                            actions.doc.fit_to_screen = true;
                            ui.close();
                        }
                        if ui.add(menu_item("100%", "Ctrl+1")).clicked() {
                            actions.doc.zoom_100 = true;
                            ui.close();
                        }
                        ui.separator();
                        ui.menu_button("Proof Setup", |ui| {
                            use crate::core::cms::ProofTarget;
                            if ui
                                .selectable_label(data.print.proof_target_label == "sRGB", "sRGB")
                                .clicked()
                            {
                                actions.print.set_proof_target = Some(ProofTarget::Srgb);
                                ui.close();
                            }
                            if ui
                                .selectable_label(
                                    data.print.proof_target_label == "Adobe RGB (1998)",
                                    "Adobe RGB (1998)",
                                )
                                .clicked()
                            {
                                actions.print.set_proof_target = Some(ProofTarget::AdobeRgb);
                                ui.close();
                            }
                            ui.separator();
                            if ui.button("Load Profile…").clicked() {
                                actions.print.load_proof_profile = true;
                                ui.close();
                            }
                            ui.label(
                                egui::RichText::new(format!(
                                    "Current: {}",
                                    data.print.proof_target_label
                                ))
                                .color(pal.text_secondary)
                                .size(10.0),
                            );
                        });
                        if ui
                            .checkbox(
                                &mut data.print.proof_enabled.clone(),
                                "Proof Colors  (Ctrl+Y)",
                            )
                            .clicked()
                        {
                            actions.print.toggle_proof_colors = true;
                        }
                        if ui
                            .checkbox(
                                &mut data.print.proof_gamut_warn.clone(),
                                "Gamut Warning  (Ctrl+Shift+Y)",
                            )
                            .clicked()
                        {
                            actions.print.toggle_gamut_warning = true;
                        }
                        ui.menu_button("Display Profile", |ui| {
                            if ui
                                .selectable_label(
                                    !data.print.display_cms_enabled,
                                    "Off (assume sRGB)",
                                )
                                .clicked()
                            {
                                actions.print.display_cms_off = true;
                                ui.close();
                            }
                            if ui.button("From System").clicked() {
                                actions.print.display_cms_from_system = true;
                                ui.close();
                            }
                            if ui.button("Load Profile…").clicked() {
                                actions.print.display_cms_load = true;
                                ui.close();
                            }
                            if data.print.display_cms_enabled {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "On: {}",
                                        data.print.display_profile_name
                                    ))
                                    .color(pal.text_secondary)
                                    .size(10.0),
                                );
                            }
                        });
                        ui.separator();
                        if ui
                            .checkbox(&mut data.chrome.show_color_panel.clone(), "Color Panel")
                            .clicked()
                        {
                            actions.chrome.toggle_color_panel = true;
                        }
                        if ui
                            .checkbox(&mut data.chrome.show_text_panel.clone(), "Text Panel")
                            .clicked()
                        {
                            actions.chrome.toggle_text_panel = true;
                        }
                        if ui
                            .checkbox(&mut data.chrome.show_layer_panel.clone(), "Layer Panel")
                            .clicked()
                        {
                            actions.chrome.toggle_layer_panel = true;
                        }
                        if ui
                            .checkbox(&mut data.chrome.show_history_panel.clone(), "History Panel")
                            .clicked()
                        {
                            actions.chrome.toggle_history_panel = true;
                        }
                        if ui
                            .checkbox(&mut data.chrome.show_info_panel.clone(), "Info Panel")
                            .clicked()
                        {
                            actions.chrome.toggle_info_panel = true;
                        }
                        if ui
                            .checkbox(
                                &mut data.chrome.show_channels_panel.clone(),
                                "Channels Panel",
                            )
                            .clicked()
                        {
                            actions.chrome.toggle_channels_panel = true;
                        }
                        ui.separator();
                        if ui
                            .checkbox(&mut data.chrome.show_rulers.clone(), "Rulers  (Ctrl+R)")
                            .clicked()
                        {
                            actions.chrome.toggle_rulers = true;
                        }
                        if ui
                            .checkbox(&mut data.chrome.show_guides.clone(), "Show Guides")
                            .clicked()
                        {
                            actions.chrome.toggle_show_guides = true;
                        }
                        if ui
                            .checkbox(&mut data.chrome.lock_guides.clone(), "Lock Guides")
                            .clicked()
                        {
                            actions.chrome.toggle_lock_guides = true;
                        }
                        if ui
                            .checkbox(&mut data.chrome.snap_enabled.clone(), "Snap")
                            .clicked()
                        {
                            actions.chrome.toggle_snap = true;
                        }
                        if ui
                            .add_enabled(
                                !data.chrome.guides.is_empty(),
                                egui::Button::new("Clear Guides"),
                            )
                            .clicked()
                        {
                            actions.chrome.clear_guides = true;
                            ui.close();
                        }
                    });

                    ui.menu_button("Window", |ui| {
                        ui.label(
                            egui::RichText::new("Workspace layouts coming soon")
                                .color(pal.text_secondary)
                                .size(11.0),
                        );
                    });

                    ui.menu_button("Help", |ui| {
                        if ui.button("About iAi").clicked() {
                            ui.close();
                        }
                        if ui.button("Keyboard Shortcuts").clicked() {
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("GitHub").clicked() {
                            ui.close();
                        }
                    });

                    // AI Studio is a direct action, not a drop-down menu. Keep it
                    // slightly separated at the outer end of the menu row so one
                    // click opens the panel immediately.
                    ui.add_space(10.0);
                    if ui
                        .add_enabled(
                            data.doc.has_doc,
                            egui::Button::new("AI").selected(data.ai.show_ai_panel),
                        )
                        .on_hover_text("Open Ai Image Studio")
                        .clicked()
                    {
                        actions.ai.toggle_ai_panel = Some(true);
                    }
                });
                if data.chrome.is_tool_modal
                    && ui.input(|i| i.pointer.primary_clicked())
                    && ui
                        .input(|i| i.pointer.interact_pos())
                        .is_some_and(|pos| menus.response.rect.contains(pos))
                {
                    actions.chrome.modal_denied = true;
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;

                    let close_hovered_id = egui::Id::new("tb_close_hovered");
                    let close_hovered = ctx
                        .data(|d| d.get_temp::<bool>(close_hovered_id))
                        .unwrap_or(false);
                    let close_fill = if close_hovered {
                        pal.danger
                    } else {
                        egui::Color32::TRANSPARENT
                    };
                    let close_resp = ui.add(
                        egui::Button::new(egui::RichText::new(ph::X).size(10.0).color(pal.icon))
                            .frame(false)
                            .fill(close_fill)
                            .min_size(egui::vec2(44.0, TITLEBAR_H)),
                    );
                    ctx.data_mut(|d| d.insert_temp(close_hovered_id, close_resp.hovered()));
                    if close_resp.clicked() {
                        if data.doc.is_modified {
                            actions.dialogs.show_exit_dialog = Some(true);
                        } else {
                            actions.doc.exit = true;
                        }
                    }

                    let max_hovered_id = egui::Id::new("tb_max_hovered");
                    let max_hovered = ctx
                        .data(|d| d.get_temp::<bool>(max_hovered_id))
                        .unwrap_or(false);
                    let max_fill = if max_hovered {
                        pal.hover_bg
                    } else {
                        egui::Color32::TRANSPARENT
                    };
                    let max_resp = ui.add(
                        egui::Button::new(
                            egui::RichText::new(ph::SQUARE).size(10.0).color(pal.icon),
                        )
                        .frame(false)
                        .fill(max_fill)
                        .min_size(egui::vec2(44.0, TITLEBAR_H)),
                    );
                    ctx.data_mut(|d| d.insert_temp(max_hovered_id, max_resp.hovered()));
                    if max_resp.clicked() {
                        actions.chrome.window_maximize_toggle = true;
                    }

                    let min_hovered_id = egui::Id::new("tb_min_hovered");
                    let min_hovered = ctx
                        .data(|d| d.get_temp::<bool>(min_hovered_id))
                        .unwrap_or(false);
                    let min_fill = if min_hovered {
                        pal.hover_bg
                    } else {
                        egui::Color32::TRANSPARENT
                    };
                    let min_resp = ui.add(
                        egui::Button::new(
                            egui::RichText::new(ph::MINUS).size(10.0).color(pal.icon),
                        )
                        .frame(false)
                        .fill(min_fill)
                        .min_size(egui::vec2(44.0, TITLEBAR_H)),
                    );
                    ctx.data_mut(|d| d.insert_temp(min_hovered_id, min_resp.hovered()));
                    if min_resp.clicked() {
                        actions.chrome.window_minimize = true;
                    }

                    ui.add_space(4.0);
                    ui.add(egui::Separator::default().vertical().shrink(6.0));
                    ui.add_space(8.0);

                    let drag_w = ui.available_width();
                    if drag_w > 2.0 {
                        let (_, drag_resp) = ui.allocate_exact_size(
                            egui::vec2(drag_w, TITLEBAR_H),
                            egui::Sense::click_and_drag(),
                        );
                        if drag_resp.double_clicked() {
                            actions.chrome.window_maximize_toggle = true;
                        }
                        if drag_resp.drag_started() {
                            actions.chrome.window_drag = true;
                        }
                    }
                });
            });
        });
}

fn menu_item<'a>(label: &'a str, shortcut: &'a str) -> egui::Button<'a> {
    let mut btn = egui::Button::new(label).wrap_mode(egui::TextWrapMode::Extend);
    if !shortcut.is_empty() {
        btn = btn.shortcut_text(shortcut);
    }
    btn
}

fn menu_item_enabled<'a>(
    label: &'a str,
    shortcut: &'a str,
    enabled: bool,
) -> impl egui::Widget + 'a {
    move |ui: &mut egui::Ui| -> egui::Response {
        ui.add_enabled_ui(enabled, |ui| ui.add(menu_item(label, shortcut)))
            .inner
    }
}

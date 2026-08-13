use super::{modal_flash_btn, UiActions, UiData};
use crate::core::layer::{BlendMode, PaintTarget};
use crate::core::text::{TextAlign, TextFontFamily};
use egui;
use egui_phosphor::regular as ph;

/// Width of the Corel-style vertical colour strip pinned to the right edge of
/// the docked panel band. Included in `panel_r_w`, so the canvas layout already
/// accounts for it.
const VECTOR_PALETTE_STRIP_W: f32 = 40.0;
const LAYER_PANEL_THUMB_SIZE: f32 = 38.0;
const CHANNEL_PANEL_THUMB_SIZE: f32 = 38.0;
const PANEL_ICON_BUTTON_SIZE: f32 = 22.0;
const LAYER_MENU_POPUP_WIDTH: f32 = 136.0;
const ADJUSTMENT_POPUP_WIDTH: f32 = 150.0;
const LAYER_DRAG_THUMB_SIZE: f32 = 48.0;
const LAYER_DRAG_THUMB_ALPHA: u8 = 128;

#[allow(deprecated)]
pub fn build(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let pal = data.chrome.theme_mode.palette();
    // The docked right band now holds only Layers/Channels plus the Corel-style
    // vertical colour strip pinned to the window edge. "Color & Brush" lives in
    // a separate floating panel (added below with the other floating panels), so
    // there is no resizable divider left that could slide the layer list off the
    // bottom of the screen.
    let show_right_band = data.chrome.show_layer_panel || data.chrome.show_channels_panel;
    if show_right_band {
        #[allow(deprecated)]
        egui::SidePanel::right("right_panels")
            .exact_size(data.chrome.panel_r_w)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(pal.panel_bg)
                    .inner_margin(egui::Margin::symmetric(6, 6)),
            )
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 5.0);
                // The colour strip is pinned to the far right edge; the layer
                // list fills the rest. Both are fixed — nothing here resizes.
                egui::SidePanel::right("vector_palette_strip")
                    .exact_size(VECTOR_PALETTE_STRIP_W)
                    .resizable(false)
                    .frame(egui::Frame::new())
                    .show_inside(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("vector_palette_strip_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                vector_palette_strip(ui, data, actions);
                            });
                    });
                egui::CentralPanel::default()
                    .frame(egui::Frame::new())
                    .show_inside(ui, |ui| {
                        // No outer scroll wrapper: the layer list scrolls
                        // internally and grows to the panel height, so the
                        // panel's real height is what `available_height` reports.
                        layers_channels_panel(ui, data, actions);
                    });
            });
    }

    let screen = ctx.content_rect();
    let dock_width = if show_right_band {
        data.chrome.panel_r_w
    } else {
        0.0
    };
    let default_x = |width: f32| {
        (screen.max.x - dock_width - width - 12.0).max(screen.min.x + data.chrome.toolbar_w + 12.0)
    };

    // Floating "Color & Brush": precise colour picker, the vector Object
    // appearance editor, and swatch management. Opened on demand from
    // Window ▸ Color Panel (like the Levels dialog). The toolbar's
    // foreground/background chips still open a colour dialog when it is closed,
    // and the right-edge strip covers quick swatch application.
    if data.chrome.show_color_panel {
        let mut open = true;
        floating_panel(
            ctx,
            "Color & Brush",
            egui::pos2(default_x(300.0), 96.0),
            300.0,
            &mut open,
            |ui| color_brush_panel(ui, data, actions),
        );
        if !open {
            actions.chrome.toggle_color_panel = true;
        }
    }

    if data.chrome.show_text_panel {
        let mut open = true;
        let response = floating_panel(
            ctx,
            "Text",
            egui::pos2(default_x(420.0), 128.0),
            420.0,
            &mut open,
            |ui| text_panel(ui, data, actions),
        );
        actions.tool.text_panel_hovered = response.is_some_and(|r| r.contains_pointer());
        if !open {
            actions.chrome.show_text_panel = Some(false);
        }
    }

    if data.chrome.show_history_panel {
        let mut open = true;
        floating_panel(
            ctx,
            "History",
            egui::pos2(default_x(260.0), 160.0),
            260.0,
            &mut open,
            |ui| history_panel(ui, data, actions),
        );
        if !open {
            actions.chrome.toggle_history_panel = true;
        }
    }

    if data.chrome.show_info_panel {
        let mut open = true;
        floating_panel(
            ctx,
            "Info",
            egui::pos2(default_x(260.0), 192.0),
            260.0,
            &mut open,
            |ui| info_panel(ui, data),
        );
        if !open {
            actions.chrome.toggle_info_panel = true;
        }
    }
}

fn floating_panel(
    ctx: &egui::Context,
    title: &'static str,
    default_pos: egui::Pos2,
    default_width: f32,
    open: &mut bool,
    add_contents: impl FnOnce(&mut egui::Ui),
) -> Option<egui::Response> {
    egui::Window::new(title)
        .open(open)
        .collapsible(false)
        .movable(true)
        .resizable(true)
        .order(egui::Order::Foreground)
        .default_pos(default_pos)
        .default_width(default_width)
        .min_width(220.0)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 5.0);
            add_contents(ui);
        })
        .map(|inner| inner.response)
}

fn text_panel(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    let pal = data.chrome.theme_mode.palette();
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new("Type Tool")
            .strong()
            .color(pal.text_primary),
    );

    if !data.tool.text_font_available {
        ui.colored_label(pal.warning, "No system font found");
        return;
    }

    ui.add_space(6.0);
    ui.label(
        egui::RichText::new("Font")
            .size(10.5)
            .color(pal.text_secondary),
    );
    // Hovering a row live-previews the font; the pre-open family is
    // stashed in temp memory and restored if the popup closes without
    // a click, so browsing can't silently change the font.
    let font_combo_open_id = egui::Id::new("text_panel_font_family_preview_open");
    let font_search_id = egui::Id::new("text_panel_font_family_search");
    let font_combo = egui::ComboBox::from_id_salt("text_panel_font_family")
        .selected_text(data.tool.text_font_family.name())
        .width(ui.available_width().min(250.0))
        .height(420.0)
        .show_ui(ui, |ui| {
            ui.set_min_width(320.0);

            let had_search_state = ui.data(|d| d.get_temp::<String>(font_search_id).is_some());
            let mut search = ui
                .data(|d| d.get_temp::<String>(font_search_id))
                .unwrap_or_default();
            let search_response = ui.add(
                egui::TextEdit::singleline(&mut search)
                    .hint_text("Search fonts…")
                    .desired_width(f32::INFINITY),
            );
            if !had_search_state {
                search_response.request_focus();
            }
            ui.data_mut(|d| d.insert_temp(font_search_id, search.clone()));
            ui.separator();

            let search = search.trim().to_lowercase();
            let mut committed = false;
            let mut match_count = 0;
            egui::ScrollArea::vertical()
                .id_salt("text_panel_font_family_list")
                .max_height(370.0)
                .show(ui, |ui| {
                    for family in TextFontFamily::all() {
                        if !search.is_empty() && !family.name().to_lowercase().contains(&search) {
                            continue;
                        }
                        match_count += 1;
                        let row = ui
                            .selectable_label(&data.tool.text_font_family == family, family.name());
                        if row.clicked() {
                            committed = true;
                            actions.tool.set_text_font_family = Some(family.clone());
                        } else if row.hovered() && &data.tool.text_font_family != family {
                            actions.tool.preview_text_font_family = Some(family.clone());
                        }
                    }
                    if match_count == 0 {
                        ui.weak("No matching fonts");
                    }
                });
            committed
        });
    let family_rect = font_combo.response.rect;
    let style_left = family_rect.right() + 6.0;
    let style_width = (ui.max_rect().right() - style_left).max(90.0);
    let style_rect = egui::Rect::from_min_size(
        egui::pos2(style_left, family_rect.top()),
        egui::vec2(style_width, family_rect.height()),
    );
    #[allow(deprecated)]
    ui.allocate_ui_at_rect(style_rect, |ui| {
        egui::ComboBox::from_id_salt("text_panel_font_face")
            .selected_text(data.tool.text_font_family.style_name())
            .width(style_width)
            .show_ui(ui, |ui| {
                for face in data.tool.text_font_family.faces() {
                    let selected = face.style_name() == data.tool.text_font_family.style_name();
                    if ui.selectable_label(selected, face.style_name()).clicked() {
                        actions.tool.set_text_font_family = Some(face);
                        // The chosen face already carries its real weight/slant;
                        // avoid stacking the legacy faux Bold/Italic on top.
                        actions.tool.set_text_bold = Some(false);
                        actions.tool.set_text_italic = Some(false);
                    }
                }
            });
    });
    match font_combo.inner {
        Some(committed) => {
            if committed {
                ui.data_mut(|d| d.remove::<bool>(font_combo_open_id));
                ui.data_mut(|d| d.remove::<String>(font_search_id));
            } else {
                ui.data_mut(|d| d.insert_temp(font_combo_open_id, true));
            }
        }
        None => {
            if ui
                .data(|d| d.get_temp::<bool>(font_combo_open_id))
                .unwrap_or(false)
            {
                ui.data_mut(|d| d.remove::<bool>(font_combo_open_id));
                ui.data_mut(|d| d.remove::<String>(font_search_id));
                actions.tool.cancel_text_font_family_preview = true;
            }
        }
    }

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Size")
                .size(10.5)
                .color(pal.text_secondary),
        );
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
    });

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Style")
                .size(10.5)
                .color(pal.text_secondary),
        );
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
                .min_size(egui::vec2(28.0, 22.0));
            if ui.add(btn).on_hover_text(tip).clicked() {
                match target {
                    0 => actions.tool.set_text_bold = Some(!data.tool.text_bold),
                    1 => actions.tool.set_text_italic = Some(!data.tool.text_italic),
                    _ => actions.tool.set_text_underline = Some(!data.tool.text_underline),
                }
            }
        }
    });

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Align")
                .size(10.5)
                .color(pal.text_secondary),
        );
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
                .min_size(egui::vec2(28.0, 22.0));
            if ui.add(btn).on_hover_text(tip).clicked() {
                actions.tool.set_text_align = Some(align);
            }
        }
    });

    // Change-case buttons — the discoverable form of the Shift+F3 shortcut.
    // Only meaningful while a text session is open (recases the selection, or
    // the whole text when nothing is selected), so they're disabled otherwise.
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Case")
                .size(10.5)
                .color(pal.text_secondary),
        );
        for (case, label, tip) in [
            (crate::core::text::TextCase::Lower, "aa", "lowercase"),
            (crate::core::text::TextCase::Upper, "AA", "UPPERCASE"),
            (
                crate::core::text::TextCase::Title,
                "Aa",
                "Capitalize Each Word",
            ),
        ] {
            let btn = egui::Button::new(egui::RichText::new(label).size(12.5))
                .fill(pal.button_bg)
                .min_size(egui::vec2(30.0, 22.0));
            if ui
                .add_enabled(data.tool.text_editing, btn)
                .on_hover_text(format!("{tip} (Shift+F3 cycles)"))
                .clicked()
            {
                actions.tool.set_text_case = Some(case);
            }
        }
    });

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Type")
                .size(10.5)
                .color(pal.text_secondary),
        );
        ui.label(
            egui::RichText::new(ph::ARROWS_OUT_LINE_VERTICAL)
                .size(13.0)
                .color(pal.text_secondary),
        )
        .on_hover_text("Line height");
        let mut line_height = data.tool.text_line_height;
        if ui
            .add(
                egui::DragValue::new(&mut line_height)
                    .range(0.5..=4.0)
                    .speed(0.02),
            )
            .on_hover_text("Line height")
            .changed()
        {
            actions.tool.set_text_line_height = Some(line_height);
        }
        if let Some(v) = text_preset_menu(
            ui,
            "text_line_height_presets",
            data.tool.text_line_height,
            &[3.0, 2.5, 2.0, 1.5, 1.2, 1.0, 0.8, 0.6, 0.5],
            |v| format!("{v:.2}"),
        ) {
            actions.tool.set_text_line_height = Some(v);
        }

        ui.label(
            egui::RichText::new(ph::TEXT_INDENT)
                .size(13.0)
                .color(pal.text_secondary),
        )
        .on_hover_text("Tracking");
        let mut tracking = data.tool.text_tracking_px;
        if ui
            .add(
                egui::DragValue::new(&mut tracking)
                    .range(-200.0..=500.0)
                    .suffix(" px")
                    .speed(0.2),
            )
            .on_hover_text("Tracking")
            .changed()
        {
            actions.tool.set_text_tracking_px = Some(tracking);
        }
        // Positive values on top, 0 in the middle, negatives below.
        if let Some(v) = text_preset_menu(
            ui,
            "text_tracking_presets",
            data.tool.text_tracking_px,
            &[
                200.0, 100.0, 75.0, 50.0, 25.0, 10.0, 5.0, 0.0, -5.0, -10.0, -25.0, -50.0, -75.0,
                -100.0,
            ],
            |v| {
                if v == 0.0 {
                    "0 px".to_string()
                } else {
                    format!("{v:+.0} px")
                }
            },
        ) {
            actions.tool.set_text_tracking_px = Some(v);
        }
    });

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Opacity")
                .size(10.5)
                .color(pal.text_secondary),
        );
        let mut opacity = data.tool.text_opacity * 100.0;
        if ui
            .add(
                egui::DragValue::new(&mut opacity)
                    .range(0.0..=100.0)
                    .suffix("%")
                    .speed(0.5),
            )
            .changed()
        {
            actions.tool.set_text_opacity = Some(opacity / 100.0);
        }
    });

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Color")
                .size(10.5)
                .color(pal.text_secondary),
        );
        let fill = egui::Color32::from_rgb(
            data.tool.text_color[0],
            data.tool.text_color[1],
            data.tool.text_color[2],
        );
        if ui
            .add(
                egui::Button::new("")
                    .fill(fill)
                    .stroke(egui::Stroke::new(1.0_f32, pal.border_subtle))
                    .min_size(egui::vec2(42.0, 22.0))
                    .corner_radius(2.0),
            )
            .on_hover_text("Text color")
            .clicked()
        {
            actions.dialogs.open_paint_color_dialog = Some(2);
        }
        let [r, g, b, _] = data.tool.text_color;
        ui.label(
            egui::RichText::new(format!("#{r:02X}{g:02X}{b:02X}"))
                .monospace()
                .small()
                .color(pal.text_secondary),
        );
    });

    if data.tool.text_editing {
        ui.add_space(10.0);
        ui.separator();
        ui.horizontal(|ui| {
            let text_color_dialog_open =
                data.dialogs.show_paint_color_dialog && data.dialogs.paint_color_dialog_target == 2;
            let commit_btn = modal_flash_btn(
                egui::Button::new(
                    egui::RichText::new(format!("{} Commit", ph::CHECK)).color(pal.success),
                )
                .min_size(egui::vec2(92.0, 24.0)),
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
                egui::Button::new(
                    egui::RichText::new(format!("{} Cancel", ph::X)).color(pal.danger),
                )
                .min_size(egui::vec2(86.0, 24.0)),
                ui,
                data,
            );
            if ui.add(cancel_btn).clicked() {
                actions.tool.text_cancel = true;
            }
        });
    }
}

/// Small ▾ button next to a value field; opens a preset list (callers order it
/// positives-first / 0 / negatives). Returns the picked value.
fn text_preset_menu(
    ui: &mut egui::Ui,
    id: &str,
    current: f32,
    presets: &[f32],
    label: impl Fn(f32) -> String,
) -> Option<f32> {
    let mut picked = None;
    ui.push_id(id, |ui| {
        ui.menu_button(egui::RichText::new(ph::CARET_DOWN).size(10.0), |ui| {
            ui.set_min_width(72.0);
            for &v in presets {
                let selected = (current - v).abs() < 0.005;
                if ui.selectable_label(selected, label(v)).clicked() {
                    picked = Some(v);
                    ui.close();
                }
            }
        });
    });
    picked
}

/// Contents of the floating "Color & Brush" panel: the precise foreground
/// colour picker, the vector Object appearance editor (when a Path is
/// selected), and document-swatch management. The quick colour chips live in
/// the right-edge strip ([`vector_palette_strip`]) instead.
fn color_brush_panel(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    {
        {
            ui.add_space(4.0);

            // The compact picker now carries its own R/G/B + editable #hex row,
            // so no separate hex field is needed here.
            ui.scope(|ui| {
                ui.spacing_mut().slider_width = ui.available_width();
                let mut c = egui::Color32::from_rgb(
                    data.tool.brush_color[0],
                    data.tool.brush_color[1],
                    data.tool.brush_color[2],
                );
                if crate::ui::color_picker::color_picker_compact(ui, &mut c) {
                    actions.tool.brush_color = Some([c.r(), c.g(), c.b(), 255]);
                }
            });

            if data.tool.path_style.is_some() {
                ui.add_space(8.0);
                ui.separator();
                ui.label(egui::RichText::new("Object").strong().size(11.0));
                ui.add_space(2.0);
                vector_appearance(ui, data, actions);
            }

            ui.add_space(8.0);
            ui.separator();
            vector_palette_manage(ui, data, actions);
        }
    }
}

/// Full Fill / Outline / Gradient / Dash editor for the active vector object.
/// It lives in the docked Color panel so the controls are laid out vertically
/// with room to breathe, unlike the crowded single-row top options bar where
/// they used to overflow off screen. Every edit goes through the same `path_*`
/// actions as the options bar, so both surfaces stay perfectly in sync.
fn vector_appearance(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    let Some(style) = data.tool.path_style else {
        return;
    };

    // ---- Fill ----
    ui.horizontal(|ui| {
        let mut fill = style.fill_enabled;
        if ui.checkbox(&mut fill, "Fill").changed() {
            actions.tool.set_path_fill_enabled = Some(fill);
        }
        if appearance_color_chip(ui, style.fill_color, "Fill colour") {
            actions.dialogs.open_paint_color_dialog = Some(5);
        }
        let mut fill_kind = style.fill_kind;
        egui::ComboBox::from_id_salt("appearance_fill_kind")
            .selected_text(match fill_kind {
                1 => "Linear",
                2 => "Radial",
                _ => "Solid",
            })
            .width(84.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut fill_kind, 0, "Solid");
                ui.selectable_value(&mut fill_kind, 1, "Linear");
                ui.selectable_value(&mut fill_kind, 2, "Radial");
            });
        if fill_kind != style.fill_kind {
            actions.tool.set_path_fill_kind = Some(fill_kind);
        }
    });

    // Interactive gradient ramp — only shown for gradient fills.
    if style.fill_kind != 0 && style.gradient_stop_count >= 2 {
        gradient_ramp(ui, &style, actions);
    }

    ui.add_space(6.0);

    // ---- Outline ----
    ui.horizontal(|ui| {
        let mut stroke = style.stroke_enabled;
        if ui.checkbox(&mut stroke, "Outline").changed() {
            actions.tool.set_path_stroke_enabled = Some(stroke);
        }
        if appearance_color_chip(ui, style.stroke_color, "Outline colour") {
            actions.dialogs.open_paint_color_dialog = Some(6);
        }
        ui.label("Width");
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
        // One undo step per scrub: commit when the drag stops or focus leaves.
        if resp.drag_stopped() || resp.lost_focus() {
            actions.tool.commit_path_style = true;
        }
    });

    ui.horizontal(|ui| {
        ui.label("Dash");
        let mut dash_kind = style.dash_kind;
        egui::ComboBox::from_id_salt("appearance_dash_kind")
            .selected_text(match dash_kind {
                1 => "Dashed",
                2 => "Dotted",
                _ => "Solid",
            })
            .width(84.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut dash_kind, 0, "Solid");
                ui.selectable_value(&mut dash_kind, 1, "Dashed");
                ui.selectable_value(&mut dash_kind, 2, "Dotted");
            });
        if dash_kind != style.dash_kind {
            actions.tool.set_path_dash_kind = Some(dash_kind);
        }
    });

    ui.horizontal(|ui| {
        ui.label("Cap");
        let mut cap = style.cap;
        egui::ComboBox::from_id_salt("appearance_line_cap")
            .selected_text(match cap {
                1 => "Round",
                2 => "Square",
                _ => "Butt",
            })
            .width(84.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut cap, 0, "Butt");
                ui.selectable_value(&mut cap, 1, "Round");
                ui.selectable_value(&mut cap, 2, "Square");
            });
        if cap != style.cap {
            actions.tool.set_path_cap = Some(cap);
        }

        ui.label("Join");
        let mut join = style.join;
        egui::ComboBox::from_id_salt("appearance_line_join")
            .selected_text(match join {
                1 => "Round",
                2 => "Bevel",
                _ => "Miter",
            })
            .width(84.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut join, 0, "Miter");
                ui.selectable_value(&mut join, 1, "Round");
                ui.selectable_value(&mut join, 2, "Bevel");
            });
        if join != style.join {
            actions.tool.set_path_join = Some(join);
        }
    });

    if style.dash_len > 0 {
        dash_editor(ui, &style, actions);
    }
}

/// A draggable gradient ramp: a live preview bar with one handle per stop.
/// Drag a handle to move its offset (clamped between its neighbours); click to
/// select it; the row beneath edits the selected stop's colour and position and
/// adds/removes stops. Replaces the buried "Stops…" nested menu.
fn gradient_ramp(ui: &mut egui::Ui, style: &crate::ui::PathStyleData, actions: &mut UiActions) {
    use crate::core::vector::style::MAX_GRADIENT_STOPS;
    let count = (style.gradient_stop_count as usize).clamp(2, MAX_GRADIENT_STOPS);

    // The selected stop persists across frames in egui temp memory.
    let sel_id = egui::Id::new("vector_appearance_stop_sel");
    let handle_base = egui::Id::new("vector_appearance_stop_handle");
    let mut selected = ui.data(|d| d.get_temp::<usize>(sel_id)).unwrap_or(0);
    if selected >= count {
        selected = count - 1;
    }

    // Gradient preview bar plus a strip below it for the handles.
    let bar_h = 18.0;
    let handle_h = 9.0;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), bar_h + handle_h + 4.0),
        egui::Sense::hover(),
    );
    let bar_rect = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), bar_h));

    let stops: Vec<(f32, [u8; 4])> = (0..count)
        .map(|i| {
            (
                style.gradient_stop_offsets[i],
                style.gradient_stop_colors[i],
            )
        })
        .collect();
    crate::ui::dialogs::paint_gradient_bar(&ui.painter_at(bar_rect), bar_rect, &stops, true);

    let mut new_selected = selected;
    for i in 0..count {
        let off = style.gradient_stop_offsets[i].clamp(0.0, 1.0);
        let cx = bar_rect.left() + off * bar_rect.width();
        let handle_rect = egui::Rect::from_center_size(
            egui::pos2(cx, bar_rect.bottom() + handle_h * 0.5 + 1.0),
            egui::vec2(12.0, handle_h + 2.0),
        );
        let resp = ui.interact(
            handle_rect,
            handle_base.with(i),
            egui::Sense::click_and_drag(),
        );

        let [r, g, b, _] = style.gradient_stop_colors[i];
        let fill = egui::Color32::from_rgb(r, g, b);
        let top = egui::pos2(cx, bar_rect.bottom() + 1.0);
        let bl = egui::pos2(cx - 5.0, bar_rect.bottom() + handle_h + 1.0);
        let br = egui::pos2(cx + 5.0, bar_rect.bottom() + handle_h + 1.0);
        let outline = if i == selected {
            ui.visuals().selection.stroke.color
        } else {
            ui.visuals().widgets.inactive.fg_stroke.color
        };
        ui.painter().add(egui::Shape::convex_polygon(
            vec![top, bl, br],
            fill,
            egui::Stroke::new(1.5_f32, outline),
        ));

        if resp.clicked() {
            new_selected = i;
        }
        if resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                let raw = ((p.x - bar_rect.left()) / bar_rect.width().max(1.0)).clamp(0.0, 1.0);
                let lo = if i == 0 {
                    0.0
                } else {
                    style.gradient_stop_offsets[i - 1]
                };
                let hi = if i + 1 == count {
                    1.0
                } else {
                    style.gradient_stop_offsets[i + 1]
                };
                actions.tool.set_path_gradient_stop_offset = Some((i as u8, raw.clamp(lo, hi)));
                new_selected = i;
            }
        }
        if resp.drag_stopped() {
            actions.tool.commit_path_style = true;
        }
    }
    if new_selected != selected {
        selected = new_selected;
        ui.data_mut(|d| d.insert_temp(sel_id, selected));
    }

    // Selected-stop editor.
    ui.horizontal_wrapped(|ui| {
        ui.label(format!("Stop {}", selected + 1));
        if appearance_color_chip(ui, style.gradient_stop_colors[selected], "Stop colour") {
            actions.dialogs.open_paint_color_dialog = Some(8 + selected as u8);
        }
        let lo = if selected == 0 {
            0.0
        } else {
            style.gradient_stop_offsets[selected - 1]
        };
        let hi = if selected + 1 == count {
            1.0
        } else {
            style.gradient_stop_offsets[selected + 1]
        };
        let mut percent = style.gradient_stop_offsets[selected] * 100.0;
        let resp = ui.add(
            egui::DragValue::new(&mut percent)
                .range((lo * 100.0)..=(hi * 100.0))
                .suffix("%")
                .speed(0.25)
                .max_decimals(1),
        );
        if resp.changed() {
            actions.tool.set_path_gradient_stop_offset = Some((selected as u8, percent / 100.0));
        }
        if resp.drag_stopped() || resp.lost_focus() {
            actions.tool.commit_path_style = true;
        }
        if ui
            .add_enabled(count > 2, egui::Button::new("Delete"))
            .on_hover_text("Remove this stop")
            .clicked()
        {
            actions.tool.remove_path_gradient_stop = Some(selected as u8);
        }
        if ui
            .add_enabled(count < MAX_GRADIENT_STOPS, egui::Button::new("Add"))
            .on_hover_text("Add a stop")
            .clicked()
        {
            actions.tool.add_path_gradient_stop = true;
        }
    });
}

/// Free dash/gap length editor for the active outline. Mirrors the options-bar
/// "Dash…" menu but stays open in the panel instead of a transient popup.
fn dash_editor(ui: &mut egui::Ui, style: &crate::ui::PathStyleData, actions: &mut UiActions) {
    let mut values = style.dash_values;
    let mut len = style.dash_len.clamp(1, 8);
    ui.horizontal(|ui| {
        ui.label("Segments");
        let resp = ui.add(egui::DragValue::new(&mut len).range(1..=8));
        if resp.changed() {
            let old_len = style.dash_len.max(1) as usize;
            for index in old_len..len as usize {
                values[index] = values[index % old_len].max(0.01);
            }
            actions.tool.set_path_dash_values = Some((values, len));
        }
        if resp.drag_stopped() || resp.lost_focus() {
            actions.tool.commit_path_style = true;
        }
    });

    let mut values_changed = false;
    let mut values_finished = false;
    ui.horizontal_wrapped(|ui| {
        for (index, value) in values[..len as usize].iter_mut().enumerate() {
            ui.label(format!("{}:", index + 1));
            let resp = ui.add(
                egui::DragValue::new(value)
                    .range(0.01..=10_000.0)
                    .speed(0.1)
                    .max_decimals(2),
            );
            values_changed |= resp.changed();
            values_finished |= resp.drag_stopped() || resp.lost_focus();
        }
    });
    if values_changed {
        actions.tool.set_path_dash_values = Some((values, len));
    }
    if values_finished {
        actions.tool.commit_path_style = true;
    }

    ui.horizontal(|ui| {
        ui.label("Offset");
        let mut offset = style.dash_offset;
        let resp = ui.add(
            egui::DragValue::new(&mut offset)
                .range(-10_000.0..=10_000.0)
                .speed(0.1)
                .max_decimals(2),
        );
        if resp.changed() {
            actions.tool.set_path_dash_offset = Some(offset);
        }
        if resp.drag_stopped() || resp.lost_focus() {
            actions.tool.commit_path_style = true;
        }
    });
}

/// A small clickable colour chip for the Appearance editor; returns true on click.
fn appearance_color_chip(ui: &mut egui::Ui, color: [u8; 4], tip: &str) -> bool {
    let c = egui::Color32::from_rgb(color[0], color[1], color[2]);
    ui.add_sized([26.0, 20.0], egui::Button::new("").fill(c))
        .on_hover_text(tip)
        .clicked()
}

/// Quick colours as a set of named swatches, shared by the strip and elsewhere.
/// A broad, production-friendly colour rail ordered from neutrals through the
/// colour wheel and into earth tones. Keeping the tones adjacent makes the
/// long strip quick to scan while giving it enough entries to comfortably fill
/// the space beside Layers on a normal-height window.
const QUICK_PALETTE: [(&str, [u8; 4]); 32] = [
    ("Black", [0, 0, 0, 255]),
    ("Charcoal", [51, 51, 51, 255]),
    ("Gray", [112, 112, 112, 255]),
    ("Silver", [192, 192, 192, 255]),
    ("White", [255, 255, 255, 255]),
    ("Crimson", [160, 20, 48, 255]),
    ("Red", [237, 28, 36, 255]),
    ("Coral", [255, 102, 94, 255]),
    ("Vermilion", [242, 82, 32, 255]),
    ("Orange", [247, 148, 30, 255]),
    ("Amber", [255, 193, 7, 255]),
    ("Yellow", [255, 235, 59, 255]),
    ("Lemon", [255, 250, 120, 255]),
    ("Lime", [139, 195, 74, 255]),
    ("Green", [0, 166, 81, 255]),
    ("Emerald", [0, 121, 83, 255]),
    ("Mint", [102, 215, 170, 255]),
    ("Teal", [0, 137, 123, 255]),
    ("Cyan", [0, 188, 212, 255]),
    ("Sky Blue", [41, 182, 246, 255]),
    ("Blue", [30, 112, 210, 255]),
    ("Royal Blue", [45, 75, 190, 255]),
    ("Navy", [25, 45, 105, 255]),
    ("Indigo", [63, 81, 181, 255]),
    ("Violet", [103, 58, 183, 255]),
    ("Purple", [142, 36, 170, 255]),
    ("Magenta", [236, 0, 140, 255]),
    ("Pink", [244, 81, 130, 255]),
    ("Rose", [194, 24, 91, 255]),
    ("Ochre", [184, 134, 11, 255]),
    ("Brown", [117, 76, 36, 255]),
    ("Dark Brown", [78, 52, 35, 255]),
];

/// The Corel-style vertical colour strip pinned to the right edge: a "no fill"
/// chip, the quick colours, then the document swatches — each full-width and
/// stacked top to bottom. Left click applies Fill (or Foreground when no Path
/// is selected); right click applies Outline (or Background).
fn vector_palette_strip(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    use crate::core::vector::color::ColorValue;

    let has_path = data.tool.path_style.is_some();
    let gesture = if has_path {
        "Left: Fill · Hold: adjust colour · Right: Outline"
    } else {
        "Left: Foreground · Hold: adjust colour · Right: Background"
    };

    // Small squares centred in the strip, like CorelDRAW's palette column.
    ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);

        let none = palette_none_button(ui).on_hover_text(if has_path {
            "No colour\nLeft: remove Fill · Right: remove Outline"
        } else {
            "Select an editable Path to remove Fill/Outline"
        });
        if none.clicked_by(egui::PointerButton::Primary) {
            actions.tool.clear_palette_fill = true;
        }
        if none.clicked_by(egui::PointerButton::Secondary) {
            actions.tool.clear_palette_outline = true;
        }

        for (name, rgba) in QUICK_PALETTE {
            let color = ColorValue::from_rgba8(rgba);
            let response =
                palette_strip_button(ui, color).on_hover_text(format!("{name}\n{gesture}"));
            palette_gesture(ui, response, color, has_path, actions);
        }

        if !data.doc.swatches.is_empty() {
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(4.0);
            for swatch in data.doc.swatches.iter() {
                let response = palette_strip_button(ui, swatch.color)
                    .on_hover_text(format!("{}\n{gesture}", swatch_tooltip(swatch)));
                palette_gesture(ui, response, swatch.color, has_path, actions);
            }
        }
    });
}

/// Document-swatch management + Overprint controls for the floating panel. The
/// quick/document colour chips themselves live in [`vector_palette_strip`].
fn vector_palette_manage(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    use crate::core::vector::color::ColorValue;

    let has_path = data.tool.path_style.is_some();
    ui.label(
        egui::RichText::new(if has_path {
            "Palette (right strip) — Left: Fill · Right: Outline"
        } else {
            "Palette (right strip) — Left: Foreground · Right: Background"
        })
        .small()
        .weak(),
    );

    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(format!("Document Swatches ({})", data.doc.swatches.len()))
            .strong()
            .size(11.0),
    );
    let add_color = data
        .tool
        .path_style
        .and_then(|style| style.fill_value.or(style.stroke_value))
        .unwrap_or_else(|| ColorValue::from_rgba8(data.tool.brush_color));
    ui.horizontal_wrapped(|ui| {
        if ui
            .small_button(format!("{} Add current", ph::PLUS))
            .on_hover_text("Add the selected Path colour (or Foreground) to this document")
            .clicked()
        {
            actions.tool.add_document_swatch = Some(add_color);
        }
        ui.menu_button("Manage\u{2026}", |ui| {
            ui.set_min_width(300.0);
            if data.doc.swatches.is_empty() {
                ui.weak("No document swatches yet");
            }
            for (index, swatch) in data.doc.swatches.iter().enumerate() {
                ui.push_id(("manage_document_swatch", index), |ui| {
                    ui.horizontal(|ui| {
                        let _ = palette_color_button(ui, swatch.color);
                        let edit_id = ui.id().with("name");
                        let mut name = ui
                            .data(|memory| memory.get_temp::<String>(edit_id))
                            .unwrap_or_else(|| swatch.name.clone());
                        let changed = ui
                            .add(
                                egui::TextEdit::singleline(&mut name)
                                    .desired_width(170.0)
                                    .char_limit(crate::core::palette::MAX_SWATCH_NAME_BYTES),
                            )
                            .changed();
                        if changed {
                            ui.data_mut(|memory| memory.insert_temp(edit_id, name.clone()));
                        }
                        if ui
                            .small_button(ph::CHECK)
                            .on_hover_text("Rename swatch")
                            .clicked()
                        {
                            actions.tool.rename_document_swatch = Some((index, name));
                        }
                        if ui
                            .small_button(ph::TRASH)
                            .on_hover_text("Remove swatch")
                            .clicked()
                        {
                            actions.tool.remove_document_swatch = Some(index);
                        }
                    });
                });
            }
        });
    });

    // Spot ink: a named separation plate. The current Fill/Foreground colour
    // becomes the ink's process alternate; applying the swatch (left-click) sets
    // a Path's Fill to this spot ink, which exports as a PDF Separation plate.
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let edit_id = ui.id().with("iai_spot_name");
        let mut spot_name = ui
            .data(|memory| memory.get_temp::<String>(edit_id))
            .unwrap_or_default();
        ui.label("Spot ink:");
        let response = ui.add(
            egui::TextEdit::singleline(&mut spot_name)
                .desired_width(150.0)
                .hint_text("Plate name")
                .char_limit(crate::core::vector::color::SPOT_NAME_CAP),
        );
        if response.changed() {
            ui.data_mut(|memory| memory.insert_temp(edit_id, spot_name.clone()));
        }
        let can_add = !spot_name.trim().is_empty();
        if ui
            .add_enabled(can_add, egui::Button::new(format!("{} Add Spot", ph::DROP)))
            .on_hover_text("Make a spot ink from the current colour — its own separation plate")
            .clicked()
        {
            actions.tool.add_spot_swatch = Some((spot_name.trim().to_string(), add_color));
            ui.data_mut(|memory| memory.insert_temp(edit_id, String::new()));
        }
    });

    if data.doc.swatches.is_empty() {
        ui.label(
            egui::RichText::new("Add named RGB/CMYK/spot colours used by this project.")
                .small()
                .weak(),
        );
    }

    if let Some(style) = data.tool.path_style {
        ui.add_space(5.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Overprint").strong().size(11.0));
            let mut fill = style.fill_overprint;
            if ui
                .add_enabled(style.fill_enabled, egui::Checkbox::new(&mut fill, "Fill"))
                .on_hover_text("Overprint Fill — preserve underlying inks on output")
                .changed()
            {
                actions.tool.set_path_fill_overprint = Some(fill);
            }
            let mut outline = style.stroke_overprint;
            if ui
                .add_enabled(
                    style.stroke_enabled,
                    egui::Checkbox::new(&mut outline, "Outline"),
                )
                .on_hover_text("Overprint Outline — preserve underlying inks on output")
                .changed()
            {
                actions.tool.set_path_stroke_overprint = Some(outline);
            }
        });
    }
}

/// Hold duration (seconds) before a left press turns into "open the colour
/// editor" instead of a plain Fill click — CorelDRAW's click-and-hold gesture.
const PALETTE_HOLD_SECS: f64 = 0.4;

fn palette_gesture(
    ui: &egui::Ui,
    response: egui::Response,
    color: crate::core::vector::color::ColorValue,
    has_path: bool,
    actions: &mut UiActions,
) {
    // Corel-style gestures:
    //   • left click            → apply Fill (or Foreground) instantly
    //   • left click and HOLD   → open the colour editor seeded with this swatch
    //   • right click           → apply Outline (or Background) instantly
    let id_start = response.id.with("iai_palette_hold_start");
    let id_fired = response.id.with("iai_palette_hold_fired");
    let now = ui.input(|i| i.time);
    // Only the *primary* button arms the hold — a held right-click still means
    // "apply Outline", not "open the editor".
    let down = response.is_pointer_button_down_on() && ui.input(|i| i.pointer.primary_down());
    let fired = ui.data(|d| d.get_temp::<bool>(id_fired)).unwrap_or(false);

    if down {
        let start = match ui.data(|d| d.get_temp::<f64>(id_start)) {
            Some(t) => t,
            None => {
                ui.data_mut(|d| d.insert_temp(id_start, now));
                now
            }
        };
        if !fired && now - start >= PALETTE_HOLD_SECS {
            // Path selected → tune its Fill (target 5); otherwise Foreground (0).
            let target = if has_path { 5 } else { 0 };
            actions.dialogs.open_paint_color_dialog_with = Some((target, color.to_rgba8()));
            ui.data_mut(|d| d.insert_temp(id_fired, true));
        }
        // Keep the clock advancing while the button is held without moving.
        ui.ctx().request_repaint();
    }

    // A quick left click (released before the hold fired) applies Fill.
    if response.clicked_by(egui::PointerButton::Primary) && !fired {
        actions.tool.apply_palette_fill = Some(color);
    }
    if response.clicked_by(egui::PointerButton::Secondary) {
        actions.tool.apply_palette_outline = Some(color);
    }

    // Reset the hold timers once the button is no longer held on this chip.
    if !down {
        ui.data_mut(|d| {
            d.remove::<f64>(id_start);
            d.remove::<bool>(id_fired);
        });
    }
}

fn palette_color_button(
    ui: &mut egui::Ui,
    color: crate::core::vector::color::ColorValue,
) -> egui::Response {
    let [r, g, b, _] = color.to_rgba8();
    ui.add(
        egui::Button::new("")
            .fill(egui::Color32::from_rgb(r, g, b))
            .stroke(egui::Stroke::new(
                1.0_f32,
                ui.visuals().widgets.noninteractive.bg_stroke.color,
            ))
            .min_size(egui::vec2(22.0, 22.0))
            .sense(egui::Sense::click()),
    )
}

/// Edge length of one colour square in the vertical strip.
const PALETTE_STRIP_CHIP: f32 = 22.0;

/// A small square colour chip for the vertical strip (Corel-style palette).
fn palette_strip_button(
    ui: &mut egui::Ui,
    color: crate::core::vector::color::ColorValue,
) -> egui::Response {
    let [r, g, b, _] = color.to_rgba8();
    ui.add(
        egui::Button::new("")
            .fill(egui::Color32::from_rgb(r, g, b))
            .stroke(egui::Stroke::new(
                1.0_f32,
                ui.visuals().widgets.noninteractive.bg_stroke.color,
            ))
            .min_size(egui::vec2(PALETTE_STRIP_CHIP, PALETTE_STRIP_CHIP))
            .corner_radius(0.0)
            .sense(egui::Sense::click()),
    )
}

fn palette_none_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(PALETTE_STRIP_CHIP, PALETTE_STRIP_CHIP),
        egui::Sense::click(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 1.0, egui::Color32::WHITE);
    painter.rect_stroke(
        rect,
        1.0,
        egui::Stroke::new(1.0_f32, egui::Color32::GRAY),
        egui::StrokeKind::Inside,
    );
    painter.line_segment(
        [rect.left_bottom(), rect.right_top()],
        egui::Stroke::new(2.0_f32, egui::Color32::RED),
    );
    response
}

fn swatch_tooltip(swatch: &crate::core::palette::DocumentSwatch) -> String {
    use crate::core::vector::color::ColorValue;
    match swatch.color {
        ColorValue::Rgb { .. } => {
            let [r, g, b, _] = swatch.color.to_rgba8();
            format!("{}\nRGB #{r:02X}{g:02X}{b:02X}", swatch.name)
        }
        ColorValue::Cmyk { c, m, y, k, .. } => {
            let p = |v: f32| (v.clamp(0.0, 1.0) * 100.0).round() as u8;
            format!(
                "{}\nCMYK C{} M{} Y{} K{}",
                swatch.name,
                p(c),
                p(m),
                p(y),
                p(k)
            )
        }
        ColorValue::Spot { name, tint, .. } => {
            let p = |v: f32| (v.clamp(0.0, 1.0) * 100.0).round() as u8;
            format!("{}\nSpot ink “{}” {}%", swatch.name, name.as_str(), p(tint))
        }
    }
}

fn ps_layer_thumbnail(
    ui: &mut egui::Ui,
    layer_type: &str,
    is_background: bool,
    size: egui::Vec2,
    thumbnail: Option<&[u8]>,
    texture_cache_id: egui::Id,
    active: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, ui.visuals().faint_bg_color);
    let mut target_frame_rect = rect;

    match layer_type {
        "Adjustment" => {
            painter.rect_filled(rect.shrink(1.0), 0.0, egui::Color32::WHITE);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Adj",
                egui::FontId::proportional(8.0),
                egui::Color32::from_gray(35),
            );
        }
        "Text" => {
            painter.rect_filled(rect.shrink(1.0), 0.0, egui::Color32::WHITE);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "T",
                egui::FontId::proportional(13.0),
                egui::Color32::BLACK,
            );
        }
        "Group" => {
            painter.rect_filled(rect.shrink(1.0), 0.0, egui::Color32::from_rgb(70, 58, 32));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Dir",
                egui::FontId::proportional(8.0),
                egui::Color32::from_rgb(230, 190, 95),
            );
        }
        _ => {
            if let Some(thumbnail) = thumbnail.filter(|t| ps_thumbnail_side(t).is_some()) {
                draw_ps_checkerboard(ui, rect.shrink(1.0), 4.0);
                target_frame_rect =
                    draw_ps_thumbnail_texture(ui, rect.shrink(1.0), thumbnail, texture_cache_id);
            } else if is_background {
                painter.rect_filled(rect.shrink(1.0), 0.0, egui::Color32::WHITE);
            } else {
                draw_ps_checkerboard(ui, rect.shrink(1.0), 4.0);
            }
        }
    }
    if matches!(layer_type, "Shape" | "Path") {
        // Vector layers render a plain raster thumbnail like any other layer; a small
        // curve badge in the corner tells them apart at a glance.
        let badge = egui::Rect::from_min_size(
            egui::pos2(
                target_frame_rect.right() - 12.0,
                target_frame_rect.bottom() - 12.0,
            ),
            egui::vec2(11.0, 11.0),
        );
        painter.rect_filled(
            badge,
            2.0,
            egui::Color32::from_rgba_unmultiplied(28, 28, 30, 200),
        );
        painter.text(
            badge.center(),
            egui::Align2::CENTER_CENTER,
            ph::BEZIER_CURVE,
            egui::FontId::proportional(9.0),
            egui::Color32::from_rgb(122, 200, 255),
        );
    }
    let anim = ui
        .ctx()
        .animate_bool_with_time(texture_cache_id.with("active_anim"), active, 0.22);
    paint_target_frame(ui, target_frame_rect, active, anim);
    resp
}

fn ps_mask_thumbnail(
    ui: &mut egui::Ui,
    size: egui::Vec2,
    thumbnail: Option<&[u8]>,
    texture_cache_id: egui::Id,
    active: bool,
    enabled: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, ui.visuals().faint_bg_color);
    let mut target_frame_rect = rect;
    if let Some(thumbnail) = thumbnail.filter(|t| ps_thumbnail_side(t).is_some()) {
        draw_ps_checkerboard(ui, rect.shrink(1.0), 4.0);
        target_frame_rect =
            draw_ps_thumbnail_texture(ui, rect.shrink(1.0), thumbnail, texture_cache_id);
    } else {
        painter.rect_filled(rect.shrink(1.0), 0.0, egui::Color32::WHITE);
    }
    if !enabled {
        painter.rect_filled(
            target_frame_rect,
            0.0,
            egui::Color32::from_rgba_unmultiplied(20, 20, 20, 115),
        );
    }
    let anim = ui
        .ctx()
        .animate_bool_with_time(texture_cache_id.with("active_anim"), active, 0.22);
    paint_target_frame(ui, target_frame_rect, active, anim);
    resp
}

/// "Chain" button linking pixel↔mask (standard raster editors). Draws the chain-link icon.
/// The layer row hit-test toggles the `mask_linked` state.
fn ps_link_button(ui: &mut egui::Ui, linked: bool) -> egui::Response {
    let color = if linked {
        ui.visuals().text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    let icon = if linked {
        ph::LINK_SIMPLE_HORIZONTAL
    } else {
        ph::LINK_SIMPLE_HORIZONTAL_BREAK
    };
    ps_panel_icon_label(
        ui,
        icon,
        12.5,
        egui::vec2(14.0, LAYER_PANEL_THUMB_SIZE),
        color,
        egui::Sense::hover(),
    )
}

fn ps_panel_icon_label(
    ui: &mut egui::Ui,
    icon: &str,
    icon_size: f32,
    widget_size: egui::Vec2,
    color: egui::Color32,
    sense: egui::Sense,
) -> egui::Response {
    ui.add_sized(
        [widget_size.x, widget_size.y],
        egui::Label::new(
            egui::RichText::new(icon.to_owned())
                .size(icon_size)
                .color(color),
        )
        .sense(sense),
    )
}

fn ps_panel_icon_button(icon: &str, size: f32) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(icon.to_owned()).size(size))
        .min_size(egui::vec2(PANEL_ICON_BUTTON_SIZE, PANEL_ICON_BUTTON_SIZE))
        .frame(false)
        .corner_radius(2.0)
}

/// Mask button for the bottom toolbar, using the same Phosphor icon set as the app toolbar.
fn ps_mask_toolbar_button(ui: &mut egui::Ui) -> egui::Response {
    ui.add(ps_panel_icon_button(ph::CIRCLE_DASHED, 14.0))
}

/// New Adjustment Layer button for the bottom toolbar: a half-black half-white
/// circle (◑) — exactly the standard "Adjustment Layer" icon.
fn ps_adjustment_toolbar_button(ui: &mut egui::Ui) -> egui::Response {
    ui.add(ps_panel_icon_button(ph::CIRCLE_HALF_TILT, 14.0))
}

/// New Layer button for the bottom bar.
fn ps_new_layer_toolbar_button(ui: &mut egui::Ui) -> egui::Response {
    ui.add(ps_panel_icon_button(ph::STACK_PLUS, 14.0))
}

/// Collapse/expand triangle for a group row: ▾ open, ▸ collapsed. As tall as the row thumbnail.
fn ps_disclosure_triangle(ui: &mut egui::Ui, expanded: bool) -> egui::Response {
    let icon = if expanded {
        ph::CARET_DOWN
    } else {
        ph::CARET_RIGHT
    };
    ps_panel_icon_label(
        ui,
        icon,
        12.0,
        egui::vec2(12.0, LAYER_PANEL_THUMB_SIZE),
        ui.visuals().text_color(),
        egui::Sense::hover(),
    )
}

/// Group button for the bottom bar: a folder icon — groups the selected layers.
fn ps_group_toolbar_button(ui: &mut egui::Ui) -> egui::Response {
    ui.add(ps_panel_icon_button(ph::FOLDER_OPEN, 14.0))
}

/// Delete button for the bottom bar.
fn ps_trash_toolbar_button(ui: &mut egui::Ui) -> egui::Response {
    ui.add(ps_panel_icon_button(ph::TRASH, 14.0))
}

const LAYER_LOCK_BUTTON_SIZE: egui::Vec2 = egui::vec2(28.0, 22.0);

/// Lock Transparent Pixels button.
fn ps_lock_alpha_icon_button(
    ui: &mut egui::Ui,
    active: bool,
    active_color: egui::Color32,
) -> egui::Response {
    ps_layer_lock_button(ui, ph::CHECKERBOARD, active, active_color)
}

fn ps_lock_all_button(
    ui: &mut egui::Ui,
    locked: bool,
    active_color: egui::Color32,
) -> egui::Response {
    let icon = if locked {
        ph::LOCK_SIMPLE
    } else {
        ph::LOCK_SIMPLE_OPEN
    };
    ps_layer_lock_button(ui, icon, locked, active_color)
}

fn ps_layer_lock_button(
    ui: &mut egui::Ui,
    icon: &str,
    active: bool,
    active_color: egui::Color32,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(LAYER_LOCK_BUTTON_SIZE, egui::Sense::click());
    let pal = ui.visuals();
    let fill = if active {
        egui::Color32::from_rgba_unmultiplied(
            active_color.r(),
            active_color.g(),
            active_color.b(),
            42,
        )
    } else if resp.hovered() {
        pal.widgets.hovered.bg_fill
    } else {
        egui::Color32::TRANSPARENT
    };
    let stroke = if active {
        egui::Stroke::new(1.0_f32, active_color)
    } else {
        egui::Stroke::new(1.0_f32, pal.widgets.noninteractive.bg_stroke.color)
    };
    ui.painter().rect_filled(rect, 2.0, fill);
    ui.painter()
        .rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
    let color = if active {
        active_color
    } else {
        pal.text_color()
    };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(15.0),
        color,
    );
    resp
}

fn paint_target_frame(ui: &mut egui::Ui, rect: egui::Rect, active: bool, anim: f32) {
    let painter = ui.painter();
    let visuals = ui.visuals();
    if active {
        let accent = visuals.selection.stroke.color;

        if anim < 0.999 {
            let grow = 1.0 + (1.0 - anim) * 3.0;
            let glow_alpha = ((1.0 - anim) * 200.0) as u8;
            let glow = egui::Color32::from_rgba_unmultiplied(
                accent.r(),
                accent.g(),
                accent.b(),
                glow_alpha,
            );
            painter.rect_stroke(
                rect.expand(grow),
                2.0,
                egui::Stroke::new(2.0_f32, glow),
                egui::StrokeKind::Outside,
            );
        }

        painter.rect_stroke(
            rect.expand(1.0),
            0.0,
            egui::Stroke::new(1.0_f32, visuals.text_color()),
            egui::StrokeKind::Outside,
        );
        painter.rect_stroke(
            rect,
            0.0,
            egui::Stroke::new(2.5_f32, accent),
            egui::StrokeKind::Inside,
        );
        painter.rect_stroke(
            rect.shrink(2.5),
            0.0,
            egui::Stroke::new(1.0_f32, visuals.text_color()),
            egui::StrokeKind::Inside,
        );
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.left() + 4.0, rect.bottom() - 4.0),
                egui::pos2(rect.right() - 4.0, rect.bottom() - 1.0),
            ),
            0.0,
            accent,
        );
    } else {
        painter.rect_stroke(
            rect,
            0.0,
            egui::Stroke::new(1.0_f32, visuals.widgets.noninteractive.bg_stroke.color),
            egui::StrokeKind::Inside,
        );
    }
}

fn ps_thumbnail_texture(
    ctx: &egui::Context,
    rgba: &[u8],
    cache_id: egui::Id,
) -> Option<egui::TextureHandle> {
    use std::hash::{Hash, Hasher};

    let Some(thumb) = ps_thumbnail_side(rgba) else {
        return None;
    };
    let rgba = &rgba[..thumb * thumb * 4];

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    thumb.hash(&mut hasher);
    rgba.hash(&mut hasher);
    let hash = hasher.finish();

    let texture = ctx
        .data(|data| data.get_temp::<(u64, egui::TextureHandle)>(cache_id))
        .map(|(cached_hash, mut texture)| {
            if cached_hash != hash {
                let image = egui::ColorImage::from_rgba_unmultiplied([thumb, thumb], rgba);
                texture.set(image, egui::TextureOptions::LINEAR);
                ctx.data_mut(|data| data.insert_temp(cache_id, (hash, texture.clone())));
            }
            texture
        })
        .unwrap_or_else(|| {
            let image = egui::ColorImage::from_rgba_unmultiplied([thumb, thumb], rgba);
            let texture = ctx.load_texture(
                format!("ps_thumb_{hash:016x}"),
                image,
                egui::TextureOptions::LINEAR,
            );
            ctx.data_mut(|data| data.insert_temp(cache_id, (hash, texture.clone())));
            texture
        });

    Some(texture)
}

fn draw_ps_thumbnail_texture(
    ui: &mut egui::Ui,
    slot_rect: egui::Rect,
    rgba: &[u8],
    cache_id: egui::Id,
) -> egui::Rect {
    let Some(texture) = ps_thumbnail_texture(ui.ctx(), rgba, cache_id) else {
        return slot_rect;
    };
    let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
    ui.painter()
        .image(texture.id(), slot_rect, uv, egui::Color32::WHITE);
    slot_rect
}

fn ps_thumbnail_side(rgba: &[u8]) -> Option<usize> {
    let pixels = rgba.len() / 4;
    let side = (pixels as f32).sqrt() as usize;
    (side > 0 && side * side * 4 == rgba.len()).then_some(side)
}

fn draw_ps_checkerboard(ui: &mut egui::Ui, rect: egui::Rect, cell: f32) {
    paint_ps_checkerboard(ui.painter(), ui.visuals(), rect, cell, 255);
}

fn paint_ps_checkerboard(
    painter: &egui::Painter,
    visuals: &egui::Visuals,
    rect: egui::Rect,
    cell: f32,
    alpha: u8,
) {
    let cols = ((rect.width() / cell).ceil() as i32).max(1);
    let rows = ((rect.height() / cell).ceil() as i32).max(1);
    for row in 0..rows {
        for col in 0..cols {
            let x = rect.min.x + col as f32 * cell;
            let y = rect.min.y + row as f32 * cell;
            let w = cell.min(rect.max.x - x);
            let h = cell.min(rect.max.y - y);
            if w > 0.0 && h > 0.0 {
                let base = if (row + col) % 2 == 0 {
                    visuals.faint_bg_color
                } else {
                    visuals.widgets.inactive.bg_fill
                };
                let color = color_with_alpha(base, alpha);
                painter.rect_filled(
                    egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h)),
                    0.0,
                    color,
                );
            }
        }
    }
}

fn color_with_alpha(color: egui::Color32, alpha: u8) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

fn ps_eye_icon(ui: &mut egui::Ui, visible: bool, sense: egui::Sense) -> egui::Response {
    let icon = if visible { ph::EYE } else { ph::EYE_SLASH };
    let color = if visible {
        ui.visuals().text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    ps_panel_icon_label(
        ui,
        icon,
        15.0,
        egui::vec2(22.0, LAYER_PANEL_THUMB_SIZE),
        color,
        sense,
    )
}

fn ps_eye_button(ui: &mut egui::Ui, visible: bool) -> egui::Response {
    ps_eye_icon(ui, visible, egui::Sense::hover())
}

fn ps_clickable_eye_button(ui: &mut egui::Ui, visible: bool) -> egui::Response {
    ps_eye_icon(ui, visible, egui::Sense::click())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LayerFilterKind {
    Any,
    Raster,
    Adjustment,
    Text,
    Group,
}

impl LayerFilterKind {
    fn label(self) -> &'static str {
        match self {
            Self::Any => "Kind",
            Self::Raster => "Pixel",
            Self::Adjustment => "Adjustment",
            Self::Text => "Type",
            Self::Group => "Group",
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Raster,
            2 => Self::Adjustment,
            3 => Self::Text,
            4 => Self::Group,
            _ => Self::Any,
        }
    }

    fn as_u8(self) -> u8 {
        match self {
            Self::Any => 0,
            Self::Raster => 1,
            Self::Adjustment => 2,
            Self::Text => 3,
            Self::Group => 4,
        }
    }

    fn matches(self, layer_type: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Raster => layer_type == "Raster",
            Self::Adjustment => layer_type == "Adjustment",
            Self::Text => layer_type == "Text",
            Self::Group => layer_type == "Group",
        }
    }
}

fn ps_layer_menu_button(ui: &mut egui::Ui) -> egui::Response {
    ui.add_sized(
        [20.0, 20.0],
        egui::Button::new(
            egui::RichText::new(ph::DOTS_THREE_VERTICAL)
                .size(15.0)
                .color(ui.visuals().text_color()),
        )
        .frame(false)
        .corner_radius(0.0),
    )
}

fn ps_flat_popup_button(ui: &mut egui::Ui, width: f32, icon: &str, label: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 20.0), egui::Sense::click());
    let visuals = ui.visuals();
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, 0.0, visuals.widgets.hovered.bg_fill);
    }
    let color = if resp.hovered() {
        visuals.text_color()
    } else {
        visuals.widgets.inactive.fg_stroke.color
    };
    let center_y = rect.center().y;
    ui.painter().text(
        egui::pos2(rect.left() + 6.0, center_y),
        egui::Align2::LEFT_CENTER,
        icon,
        egui::FontId::proportional(12.5),
        color,
    );
    ui.painter().text(
        egui::pos2(rect.left() + 25.0, center_y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(11.5),
        color,
    );
    resp
}

fn layers_channels_panel(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    let pal = data.chrome.theme_mode.palette();
    let channels_active = data.chrome.show_channels_panel;
    egui::Frame::new()
        .fill(pal.panel_bg)
        .stroke(egui::Stroke::new(1.0_f32, pal.border_subtle))
        .inner_margin(egui::Margin::same(0))
        .show(ui, |ui| {
            ui.set_clip_rect(ui.max_rect());
            ui.set_min_height(300.0);

            egui::Frame::new()
                .fill(pal.panel_header_bg)
                .inner_margin(egui::Margin::symmetric(6, 4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui
                            .add_sized(
                                [70.0, 22.0],
                                egui::Button::selectable(
                                    !channels_active,
                                    egui::RichText::new("Layers").strong().size(11.0),
                                ),
                            )
                            .clicked()
                        {
                            actions.chrome.show_layer_panel = Some(true);
                            actions.chrome.show_channels_panel = Some(false);
                        }
                        if ui
                            .add_sized(
                                [82.0, 22.0],
                                egui::Button::selectable(
                                    channels_active,
                                    egui::RichText::new("Channels").strong().size(11.0),
                                ),
                            )
                            .clicked()
                        {
                            actions.chrome.show_layer_panel = Some(false);
                            actions.chrome.show_channels_panel = Some(true);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if !channels_active {
                                let menu_resp =
                                    ps_layer_menu_button(ui).on_hover_text("Layer panel menu");
                                let menu_popup_id = egui::Popup::default_response_id(&menu_resp);
                                egui::Popup::from_response(&menu_resp)
                                    .open_memory(
                                        menu_resp.clicked().then_some(egui::SetOpenCommand::Toggle),
                                    )
                                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                                    .show(|ui| {
                                        ui.set_width(LAYER_MENU_POPUP_WIDTH);
                                        ui.spacing_mut().item_spacing.y = 0.0;
                                        ui.spacing_mut().button_padding = egui::vec2(6.0, 1.0);
                                        if ps_flat_popup_button(
                                            ui,
                                            LAYER_MENU_POPUP_WIDTH,
                                            ph::STACK_PLUS,
                                            "New Layer",
                                        )
                                        .clicked()
                                        {
                                            actions.layers.add_layer = true;
                                            egui::Popup::close_id(ui.ctx(), menu_popup_id);
                                        }
                                        if ps_flat_popup_button(
                                            ui,
                                            LAYER_MENU_POPUP_WIDTH,
                                            ph::COPY_SIMPLE,
                                            "Duplicate Layer",
                                        )
                                        .clicked()
                                        {
                                            actions.layers.duplicate_layer =
                                                Some(data.layers.active_layer_idx);
                                            egui::Popup::close_id(ui.ctx(), menu_popup_id);
                                        }
                                        if ps_flat_popup_button(
                                            ui,
                                            LAYER_MENU_POPUP_WIDTH,
                                            ph::TEXT_AA,
                                            "Rename Layer...",
                                        )
                                        .clicked()
                                        {
                                            actions.dialogs.show_rename_dialog =
                                                Some((true, data.layers.active_layer_idx));
                                            egui::Popup::close_id(ui.ctx(), menu_popup_id);
                                        }
                                        ui.add_space(2.0);
                                        ui.separator();
                                        ui.add_space(2.0);
                                        if ps_flat_popup_button(
                                            ui,
                                            LAYER_MENU_POPUP_WIDTH,
                                            ph::ARROWS_IN_LINE_VERTICAL,
                                            "Merge Down",
                                        )
                                        .clicked()
                                        {
                                            actions.layers.merge_down =
                                                Some(data.layers.active_layer_idx);
                                            egui::Popup::close_id(ui.ctx(), menu_popup_id);
                                        }
                                        if ps_flat_popup_button(
                                            ui,
                                            LAYER_MENU_POPUP_WIDTH,
                                            ph::STACK,
                                            "Flatten Image",
                                        )
                                        .clicked()
                                        {
                                            actions.layers.merge_all = true;
                                            egui::Popup::close_id(ui.ctx(), menu_popup_id);
                                        }
                                    });
                            }
                        });
                    });
                });

            ui.separator();
            if channels_active {
                channels_panel_contents(ui, data, actions);
                return;
            }

            ui.add_enabled_ui(!data.chrome.is_tool_modal, |ui| {
                let filter_id = egui::Id::new("layer_filter_kind");
                let mut filter = LayerFilterKind::from_u8(
                    ui.ctx()
                        .data_mut(|d| d.get_temp::<u8>(filter_id).unwrap_or(0)),
                );

                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    egui::ComboBox::from_id_salt("layer_filter_kind_combo")
                        .selected_text(filter.label())
                        .width(78.0)
                        .show_ui(ui, |ui| {
                            for kind in [
                                LayerFilterKind::Any,
                                LayerFilterKind::Raster,
                                LayerFilterKind::Adjustment,
                                LayerFilterKind::Text,
                                LayerFilterKind::Group,
                            ] {
                                if ui.selectable_label(filter == kind, kind.label()).clicked() {
                                    filter = kind;
                                    ui.ctx()
                                        .data_mut(|d| d.insert_temp(filter_id, filter.as_u8()));
                                }
                            }
                        });
                });

                if data.layers.layer_count > 0 {
                    let idx = data.layers.active_layer_idx;
                    let current_mode = data
                        .layers
                        .layer_blend_modes
                        .get(idx)
                        .copied()
                        .unwrap_or(BlendMode::Normal);
                    let current_opacity =
                        data.layers.layer_opacities.get(idx).copied().unwrap_or(1.0);
                    let lock_alpha = data
                        .layers
                        .layer_lock_alpha
                        .get(idx)
                        .copied()
                        .unwrap_or(false);
                    let locked = data.layers.layer_locked.get(idx).copied().unwrap_or(false);

                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.add_space(6.0);
                        egui::ComboBox::from_id_salt("layer_blend_mode")
                            .selected_text(current_mode.name())
                            .width(118.0)
                            .height(420.0)
                            .show_ui(ui, |ui| {
                                for mode in BlendMode::all() {
                                    if ui
                                        .selectable_label(current_mode == *mode, mode.name())
                                        .clicked()
                                    {
                                        actions.layers.set_blend_mode = Some((idx, *mode));
                                    }
                                }
                            });
                        ui.label(egui::RichText::new("Opacity:").size(10.5));
                        let mut op = current_opacity * 100.0;
                        let resp = ui
                            .add_sized(
                                [56.0, 20.0],
                                egui::DragValue::new(&mut op)
                                    .range(0.0..=100.0)
                                    .suffix("%")
                                    .speed(1.0)
                                    .max_decimals(0),
                            )
                            .on_hover_text("Layer opacity");
                        if resp.changed() {
                            actions.layers.set_opacity = Some((idx, op / 100.0));
                        }
                        if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                            actions.layers.set_opacity_commit = true;
                        }
                    });

                    ui.horizontal(|ui| {
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new("Lock:").size(10.5));
                        if ps_lock_alpha_icon_button(ui, lock_alpha, pal.warning)
                            .on_hover_text("Lock transparent pixels")
                            .clicked()
                        {
                            actions.layers.toggle_lock_alpha = Some(idx);
                        }
                        if ps_lock_all_button(ui, locked, pal.warning)
                            .on_hover_text(if locked { "Unlock all" } else { "Lock all" })
                            .clicked()
                        {
                            actions.layers.toggle_lock = Some(idx);
                        }
                    });
                }

                ui.separator();
                let visible_indices: Vec<usize> = (0..data.layers.layer_count)
                    .rev()
                    .filter(|idx| {
                        if data
                            .layers
                            .layer_collapsed_hidden
                            .get(*idx)
                            .copied()
                            .unwrap_or(false)
                        {
                            return false;
                        }
                        let layer_type = data
                            .layers
                            .layer_types
                            .get(*idx)
                            .map(|s| s.as_str())
                            .unwrap_or("Raster");
                        filter.matches(layer_type)
                    })
                    .collect();
                let row_h = 50.0;
                // Grow the list to fill the panel instead of capping at 8 rows,
                // so the empty area below the layers is used. Reserve room for
                // the separator + toolbar row that follow; auto_shrink keeps the
                // list from stretching past the actual layers when there are few.
                let list_reserve = 52.0;
                let list_h = (ui.available_height() - list_reserve).max(row_h * 2.0);
                egui::ScrollArea::vertical()
                    .id_salt("layer_list_scroll")
                    .max_height(list_h)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for idx in visible_indices {
                            layer_item(ui, data, actions, idx);
                        }
                        if ui.input(|i| i.pointer.any_released()) {
                            ui.memory_mut(|mem| {
                                mem.data.remove::<usize>(egui::Id::new("dragging_layer"))
                            });
                        } else {
                            paint_layer_drag_thumbnail(ui, data);
                        }
                    });

                ui.separator();
                ui.horizontal(|ui| {
                    ui.add_space(4.0);

                    let mask_resp = ps_mask_toolbar_button(ui).on_hover_text("Add Layer Mask");
                    if mask_resp.clicked() {
                        let alt = ui.input(|i| i.modifiers.alt);
                        if alt {
                            actions.layers.add_mask_black = Some(data.layers.active_layer_idx);
                        } else {
                            actions.layers.add_mask_white = Some(data.layers.active_layer_idx);
                        }
                    }

                    let adj_resp =
                        ps_adjustment_toolbar_button(ui).on_hover_text("New adjustment layer");
                    let adj_popup_id = egui::Popup::default_response_id(&adj_resp);
                    egui::Popup::from_response(&adj_resp)
                        .open_memory(adj_resp.clicked().then_some(egui::SetOpenCommand::Toggle))
                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                        .show(|ui| {
                            ui.set_width(ADJUSTMENT_POPUP_WIDTH);
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.spacing_mut().button_padding = egui::vec2(6.0, 1.0);
                            use crate::core::layer::AdjustmentType;
                            for (name, adj) in [
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
                            ] {
                                if ps_flat_popup_button(
                                    ui,
                                    ADJUSTMENT_POPUP_WIDTH,
                                    ph::SLIDERS_HORIZONTAL,
                                    name,
                                )
                                .clicked()
                                {
                                    actions.layers.add_adjustment_layer = Some(adj);
                                    egui::Popup::close_id(ui.ctx(), adj_popup_id);
                                }
                            }
                        });

                    let group_resp = ps_group_toolbar_button(ui)
                        .on_hover_text("Group selected (Ctrl+G) · Alt-click: Ungroup");
                    if group_resp.clicked() {
                        if ui.input(|i| i.modifiers.alt) {
                            actions.layers.ungroup = Some(data.layers.active_layer_idx);
                        } else {
                            actions.layers.group_selected = true;
                        }
                    }

                    if ps_new_layer_toolbar_button(ui)
                        .on_hover_text("New layer")
                        .clicked()
                    {
                        actions.layers.add_layer = true;
                    }

                    let active = data.layers.active_layer_idx;
                    let active_has_mask = data
                        .layers
                        .layer_has_mask
                        .get(active)
                        .copied()
                        .unwrap_or(false);
                    let active_on_mask = data.layers.layer_paint_targets.get(active).copied()
                        == Some(PaintTarget::Mask);
                    let delete_mask_mode = active_has_mask && active_on_mask;
                    if ps_trash_toolbar_button(ui)
                        .on_hover_text(if delete_mask_mode {
                            "Delete layer mask"
                        } else {
                            "Delete layer"
                        })
                        .clicked()
                    {
                        if delete_mask_mode {
                            actions.layers.delete_mask = Some(active);
                        } else {
                            actions.layers.remove_layer = Some(active);
                        }
                    }
                });
            });
        });
}

fn paint_layer_drag_thumbnail(ui: &egui::Ui, data: &UiData) {
    let Some(idx) = ui.memory(|mem| mem.data.get_temp::<usize>(egui::Id::new("dragging_layer")))
    else {
        return;
    };
    if idx >= data.layers.layer_count {
        return;
    }

    let Some(pointer) = ui.input(|i| i.pointer.hover_pos().or_else(|| i.pointer.interact_pos()))
    else {
        return;
    };

    let pal = data.chrome.theme_mode.palette();
    let layer_type = data
        .layers
        .layer_types
        .get(idx)
        .map(|s| s.as_str())
        .unwrap_or("Raster");
    let is_background = data
        .layers
        .layer_is_background
        .get(idx)
        .copied()
        .unwrap_or(false);
    let rect = egui::Rect::from_min_size(
        pointer + egui::vec2(14.0, 14.0),
        egui::vec2(LAYER_DRAG_THUMB_SIZE, LAYER_DRAG_THUMB_SIZE),
    );
    let inner = rect.shrink(4.0);
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("layer_drag_thumbnail"),
    ));

    painter.rect_filled(rect, 4.0, color_with_alpha(pal.panel_bg, 100));
    match layer_type {
        "Adjustment" => {
            painter.rect_filled(
                inner,
                2.0,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, LAYER_DRAG_THUMB_ALPHA),
            );
            painter.text(
                inner.center(),
                egui::Align2::CENTER_CENTER,
                "Adj",
                egui::FontId::proportional(10.0),
                color_with_alpha(egui::Color32::from_gray(35), LAYER_DRAG_THUMB_ALPHA),
            );
        }
        "Text" => {
            painter.rect_filled(
                inner,
                2.0,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, LAYER_DRAG_THUMB_ALPHA),
            );
            painter.text(
                inner.center(),
                egui::Align2::CENTER_CENTER,
                "T",
                egui::FontId::proportional(18.0),
                color_with_alpha(egui::Color32::BLACK, LAYER_DRAG_THUMB_ALPHA),
            );
        }
        "Group" => {
            painter.rect_filled(
                inner,
                2.0,
                egui::Color32::from_rgba_unmultiplied(70, 58, 32, LAYER_DRAG_THUMB_ALPHA),
            );
            painter.text(
                inner.center(),
                egui::Align2::CENTER_CENTER,
                "Dir",
                egui::FontId::proportional(10.0),
                egui::Color32::from_rgba_unmultiplied(230, 190, 95, LAYER_DRAG_THUMB_ALPHA),
            );
        }
        _ => {
            if let Some(thumbnail) = data
                .layers
                .layer_thumbnails
                .get(idx)
                .map(|v| v.as_slice())
                .filter(|t| ps_thumbnail_side(t).is_some())
            {
                paint_ps_checkerboard(&painter, ui.visuals(), inner, 4.0, 95);
                if let Some(texture) = ps_thumbnail_texture(
                    ui.ctx(),
                    thumbnail,
                    egui::Id::new("layer_drag_thumbnail_texture").with(idx),
                ) {
                    let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
                    painter.image(
                        texture.id(),
                        inner,
                        uv,
                        egui::Color32::from_rgba_unmultiplied(
                            255,
                            255,
                            255,
                            LAYER_DRAG_THUMB_ALPHA,
                        ),
                    );
                }
            } else if is_background {
                painter.rect_filled(
                    inner,
                    2.0,
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, LAYER_DRAG_THUMB_ALPHA),
                );
            } else {
                paint_ps_checkerboard(&painter, ui.visuals(), inner, 4.0, 95);
            }
        }
    }

    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0_f32, color_with_alpha(pal.accent_guide, 150)),
        egui::StrokeKind::Inside,
    );
    ui.ctx().request_repaint();
}

fn layer_drop_target(
    ui: &egui::Ui,
    row_rect: egui::Rect,
    idx: usize,
) -> Option<(usize, usize, bool)> {
    let src = ui.memory(|mem| mem.data.get_temp::<usize>(egui::Id::new("dragging_layer")))?;
    if src == idx {
        return None;
    }

    let pos = ui.input(|i| i.pointer.hover_pos())?;
    if !row_rect.expand(2.0).contains(pos) {
        return None;
    }

    let insert_above = pos.y < row_rect.center().y;
    let dst = if insert_above { idx + 1 } else { idx };
    if dst == src || dst == src + 1 {
        return None;
    }

    Some((src, dst, insert_above))
}

fn paint_layer_drop_indicator(
    ui: &egui::Ui,
    row_rect: egui::Rect,
    insert_above: bool,
    color: egui::Color32,
) {
    let painter = ui.painter();
    let y = if insert_above {
        row_rect.top() + 0.5
    } else {
        row_rect.bottom() - 0.5
    };
    let left = row_rect.left() + 4.0;
    let right = row_rect.right() - 4.0;
    let glow = egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 90);
    let soft = egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 34);
    let gap_rect = egui::Rect::from_min_max(egui::pos2(left, y - 2.0), egui::pos2(right, y + 2.0));

    painter.rect_filled(gap_rect, 0.0, soft);
    painter.line_segment(
        [egui::pos2(left, y - 1.5), egui::pos2(right, y - 1.5)],
        egui::Stroke::new(1.0_f32, glow),
    );
    painter.line_segment(
        [egui::pos2(left, y + 1.5), egui::pos2(right, y + 1.5)],
        egui::Stroke::new(1.0_f32, glow),
    );
    painter.line_segment(
        [egui::pos2(left, y), egui::pos2(right, y)],
        egui::Stroke::new(2.0_f32, color),
    );
}

fn layer_item(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions, idx: usize) {
    let pal = data.chrome.theme_mode.palette();
    let is_active = idx == data.layers.active_layer_idx;
    let is_selected = data
        .layers
        .layer_selected
        .get(idx)
        .copied()
        .unwrap_or(false);
    let visible = data.layers.layer_visibles.get(idx).copied().unwrap_or(true);
    let locked = data.layers.layer_locked.get(idx).copied().unwrap_or(false);
    let lock_alpha = data
        .layers
        .layer_lock_alpha
        .get(idx)
        .copied()
        .unwrap_or(false);
    let is_background = data
        .layers
        .layer_is_background
        .get(idx)
        .copied()
        .unwrap_or(false);
    let has_mask = data
        .layers
        .layer_has_mask
        .get(idx)
        .copied()
        .unwrap_or(false);
    let mask_enabled = data
        .layers
        .layer_mask_enabled
        .get(idx)
        .copied()
        .unwrap_or(true);
    let paint_target = data
        .layers
        .layer_paint_targets
        .get(idx)
        .copied()
        .unwrap_or(PaintTarget::Pixels);
    let layer_type = data
        .layers
        .layer_types
        .get(idx)
        .map(|s| s.as_str())
        .unwrap_or("Raster");
    let depth = data.layers.layer_depths.get(idx).copied().unwrap_or(0);
    let is_group = layer_type == "Group";
    let expanded = data.layers.layer_expanded.get(idx).copied().unwrap_or(true);
    let is_clipped = data
        .layers
        .layer_is_clipped
        .get(idx)
        .copied()
        .unwrap_or(false);
    let is_clip_base = data
        .layers
        .layer_is_clip_base
        .get(idx)
        .copied()
        .unwrap_or(false);

    let item_id = egui::Id::new("layer_item").with(idx);
    let dragging_layer =
        ui.memory(|mem| mem.data.get_temp::<usize>(egui::Id::new("dragging_layer")));
    let is_being_dragged = dragging_layer == Some(idx);

    let bg_color = if is_active || is_selected {
        pal.accent_selected_bg
    } else if is_being_dragged {
        pal.button_bg
    } else {
        pal.panel_bg
    };

    let stroke = if is_active || is_selected {
        egui::Stroke::new(1.0_f32, pal.border_subtle)
    } else {
        egui::Stroke::NONE
    };

    let mut eye_rect: Option<egui::Rect> = None;
    let mut pixel_thumb_rect: Option<egui::Rect> = None;
    let mut mask_thumb_rect: Option<egui::Rect> = None;
    let mut link_rect: Option<egui::Rect> = None;
    let mut tri_rect: Option<egui::Rect> = None;

    let frame = egui::Frame::new()
        .fill(bg_color)
        .stroke(stroke)
        .inner_margin(egui::Margin::symmetric(6, 5))
        .corner_radius(0.0);

    let frame_resp = frame
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if depth > 0 {
                    ui.add_space(depth as f32 * 14.0);
                }
                if is_group {
                    tri_rect = Some(ps_disclosure_triangle(ui, expanded).rect);
                } else {
                    ui.add_space(12.0);
                }

                let eye_btn = ps_eye_button(ui, visible);
                eye_rect = Some(eye_btn.rect);
                eye_btn.on_hover_text(if visible { "Hide Layer" } else { "Show Layer" });

                // Clipping-mask indicator: a small right-angle arrow pointing DOWN
                // into the layer below marks a layer clipped to it (Photoshop-style
                // — an elbow that points at the base, not a curve toward the eye).
                if is_clipped {
                    ui.add_space(1.0);
                    ui.label(
                        egui::RichText::new(ph::ARROW_ELBOW_LEFT_DOWN)
                            .size(12.0)
                            .color(pal.text_dim),
                    )
                    .on_hover_text("Clip to the layer below (clipping mask · Ctrl+Alt+G)");
                    ui.add_space(1.0);
                }

                let thumb_resp = ps_layer_thumbnail(
                    ui,
                    layer_type,
                    is_background,
                    egui::vec2(LAYER_PANEL_THUMB_SIZE, LAYER_PANEL_THUMB_SIZE),
                    data.layers.layer_thumbnails.get(idx).map(|v| v.as_slice()),
                    egui::Id::new("layer_pixel_thumbnail_texture").with(idx),
                    is_active && paint_target == PaintTarget::Pixels,
                )
                .on_hover_text("Layer pixels");
                pixel_thumb_rect = Some(thumb_resp.rect);

                if has_mask {
                    ui.add_space(1.0);
                    let mask_linked = data
                        .layers
                        .layer_mask_linked
                        .get(idx)
                        .copied()
                        .unwrap_or(true);
                    let link_resp = ps_link_button(ui, mask_linked).on_hover_text(if mask_linked {
                        "Mask linked"
                    } else {
                        "Mask unlinked"
                    });
                    link_rect = Some(link_resp.rect);
                    ui.add_space(1.0);

                    let mask_resp = ps_mask_thumbnail(
                        ui,
                        egui::vec2(LAYER_PANEL_THUMB_SIZE, LAYER_PANEL_THUMB_SIZE),
                        data.layers
                            .layer_mask_thumbnails
                            .get(idx)
                            .map(|v| v.as_slice()),
                        egui::Id::new("layer_mask_thumbnail_texture").with(idx),
                        is_active && paint_target == PaintTarget::Mask,
                        mask_enabled,
                    )
                    .on_hover_text("Layer mask  (Ctrl+I to invert)");
                    mask_thumb_rect = Some(mask_resp.rect);
                }

                let name_color = if is_active || visible {
                    pal.text
                } else {
                    pal.text_dim
                };
                let name = data
                    .layers
                    .layer_names
                    .get(idx)
                    .map(|s| s.as_str())
                    .unwrap_or("Layer");
                let mut name_text = if is_background {
                    egui::RichText::new(name)
                        .size(12.0)
                        .color(name_color)
                        .italics()
                } else {
                    egui::RichText::new(name).size(12.0).color(name_color)
                };
                // The base of a clipping mask gets an underlined name (Photoshop).
                if is_clip_base {
                    name_text = name_text.underline();
                }
                let right_reserve = if locked || lock_alpha || is_background {
                    14.0
                } else {
                    0.0
                };
                let available = (ui.available_width() - right_reserve).max(30.0);
                ui.allocate_ui(egui::vec2(available, 20.0), |ui| {
                    ui.add(egui::Label::new(name_text).selectable(false).truncate());
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if locked || is_background {
                        ui.label(
                            egui::RichText::new(ph::LOCK_SIMPLE)
                                .size(10.0)
                                .color(pal.warning),
                        );
                    } else if lock_alpha {
                        ui.label(
                            egui::RichText::new(ph::CHECKERBOARD)
                                .size(10.0)
                                .color(pal.accent_hover),
                        );
                    }
                });
            });
        })
        .response;

    let row_resp = ui.interact(frame_resp.rect, item_id, egui::Sense::click_and_drag());
    // Selection changed this frame — bring the active row into view (its collapsed
    // ancestors were already expanded app-side). `None` scrolls only as far as
    // needed, so an already-visible row isn't yanked to the centre.
    if is_active && data.layers.scroll_layers_to_active {
        row_resp.scroll_to_me(None);
    }
    let drop_target = layer_drop_target(ui, frame_resp.rect, idx);
    let is_drop_target = drop_target.is_some();

    if row_resp.hovered() && !is_active && !is_being_dragged && dragging_layer.is_none() {
        ui.painter().rect_stroke(
            frame_resp.rect.shrink(0.5),
            3.0,
            egui::Stroke::new(1.0_f32, pal.hover_bg),
            egui::StrokeKind::Inside,
        );
    }
    if (is_active || is_selected) && !is_drop_target {
        let indicator = egui::Rect::from_min_max(
            frame_resp.rect.left_top(),
            egui::pos2(frame_resp.rect.left() + 3.0, frame_resp.rect.bottom()),
        );
        ui.painter().rect_filled(indicator, 1.0, pal.accent_primary);
        ui.painter().rect_stroke(
            frame_resp.rect.shrink(0.5),
            3.0,
            egui::Stroke::new(1.0_f32, pal.border_subtle),
            egui::StrokeKind::Inside,
        );
    }
    if let Some((_src, _dst, insert_above)) = drop_target {
        paint_layer_drop_indicator(ui, frame_resp.rect, insert_above, pal.accent_guide);
        ui.ctx().request_repaint();
    }

    if row_resp.clicked() {
        let pos = ui.input(|i| i.pointer.interact_pos()).unwrap_or_default();
        let on_triangle = tri_rect.is_some_and(|r| r.contains(pos));
        let on_eye = eye_rect.is_some_and(|r| r.contains(pos));
        let on_pixel = pixel_thumb_rect.is_some_and(|r| r.contains(pos));
        let on_mask = mask_thumb_rect.is_some_and(|r| r.contains(pos));
        let on_link = link_rect.is_some_and(|r| r.contains(pos));
        if on_triangle {
            actions.layers.toggle_group_expanded = Some(idx);
        } else if on_eye {
            actions.layers.toggle_visible = Some(idx);
        } else if on_link {
            actions.layers.toggle_mask_link = Some(idx);
        } else if on_pixel {
            let ctrl = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
            if ctrl {
                actions.layers.load_layer_selection = Some(idx);
            } else {
                actions.layers.select_layer = Some((idx, false, false));
                actions.layers.set_paint_target = Some((idx, PaintTarget::Pixels));
            }
        } else if on_mask {
            actions.layers.select_layer = Some((idx, false, false));
            actions.layers.set_paint_target = Some((idx, PaintTarget::Mask));
        } else {
            let ctrl = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
            let shift = ui.input(|i| i.modifiers.shift);
            actions.layers.select_layer = Some((idx, ctrl, shift));
        }
    }

    if row_resp.double_clicked() {
        let pos = ui.input(|i| i.pointer.interact_pos()).unwrap_or_default();
        let on_eye = eye_rect.is_some_and(|r| r.contains(pos));
        if is_background {
            let on_icon = on_eye
                || mask_thumb_rect.is_some_and(|r| r.contains(pos))
                || link_rect.is_some_and(|r| r.contains(pos));
            if !on_icon {
                actions.layers.unlock_background_layer = Some(idx);
            }
        } else if layer_type == "Adjustment" {
            if !on_eye {
                actions.dialogs.edit_adjustment_layer = Some(idx);
            }
        } else if layer_type == "Text" {
            if !on_eye {
                actions.tool.edit_text_layer = Some(idx);
            }
        } else {
            let on_icon = on_eye
                || pixel_thumb_rect.is_some_and(|r| r.contains(pos))
                || mask_thumb_rect.is_some_and(|r| r.contains(pos))
                || link_rect.is_some_and(|r| r.contains(pos));
            if !on_icon {
                actions.dialogs.show_rename_dialog = Some((true, idx));
            }
        }
    }

    if row_resp.drag_started() {
        ui.memory_mut(|mem| mem.data.insert_temp(egui::Id::new("dragging_layer"), idx));
    }
    if row_resp.hovered() && ui.input(|i| i.pointer.any_released()) {
        if let Some((src, dst, _insert_above)) = layer_drop_target(ui, frame_resp.rect, idx) {
            if src != dst && src != idx {
                actions.layers.move_layer_to = Some((src, dst));
            }
        }
        ui.memory_mut(|mem| mem.data.remove::<usize>(egui::Id::new("dragging_layer")));
    }

    if row_resp.secondary_clicked() {
        // Preserve an existing multi-selection when right-clicking one of its
        // rows. Right-clicking an unselected row makes that row the sole target,
        // matching standard Layers-panel context-menu behaviour.
        if !is_selected {
            actions.layers.select_layer = Some((idx, false, false));
        }
        ui.ctx().request_repaint();
    }
    row_resp.context_menu(|ui| {
        if ui.button("Duplicate").clicked() {
            actions.layers.duplicate_layer = Some(idx);
            ui.close();
        }
        if ui.button("Layer via Copy (Ctrl+J)").clicked() {
            actions.layers.layer_via_copy = true;
            ui.close();
        }
        if ui.button("Rename...").clicked() {
            actions.dialogs.show_rename_dialog = Some((true, idx));
            ui.close();
        }
        let has_selected_vector = data
            .layers
            .layer_selected
            .iter()
            .zip(data.layers.layer_types.iter())
            .any(|(selected, kind)| *selected && (kind == "Shape" || kind == "Path"));
        let clicked_vector = layer_type == "Shape" || layer_type == "Path";
        if ui
            .add_enabled(
                has_selected_vector || clicked_vector,
                egui::Button::new("Format Selected Vectors…"),
            )
            .clicked()
        {
            actions.dialogs.open_vector_style_dialog =
                Some(crate::ui::intent::VectorStyleTarget::Selected);
            ui.close();
        }
        ui.separator();
        if ui.button("Move Up").clicked() {
            actions.layers.move_layer_up = Some(idx);
            ui.close();
        }
        if ui.button("Move Down").clicked() {
            actions.layers.move_layer_down = Some(idx);
            ui.close();
        }
        ui.separator();
        let la_label = if lock_alpha {
            "Unlock Transparent Pixels"
        } else {
            "Lock Transparent Pixels"
        };
        if ui.button(la_label).clicked() {
            actions.layers.toggle_lock_alpha = Some(idx);
            ui.close();
        }
        ui.separator();
        if !has_mask {
            if ui.button("Add Mask (Reveal All)").clicked() {
                actions.layers.add_mask_white = Some(idx);
                ui.close();
            }
            if ui.button("Add Mask (Hide All)").clicked() {
                actions.layers.add_mask_black = Some(idx);
                ui.close();
            }
        }
        if has_mask {
            let link_label = if data
                .layers
                .layer_mask_linked
                .get(idx)
                .copied()
                .unwrap_or(true)
            {
                "Unlink Mask"
            } else {
                "Link Mask"
            };
            if ui.button(link_label).clicked() {
                actions.layers.toggle_mask_link = Some(idx);
                ui.close();
            }
            if ui.button("Invert  (Ctrl+I)").clicked() {
                actions.layers.invert_active = Some(idx);
                ui.close();
            }
            if ui.button("Apply Mask").clicked() {
                actions.layers.apply_mask = Some(idx);
                ui.close();
            }
            if ui.button("Delete Mask").clicked() {
                actions.layers.delete_mask = Some(idx);
                ui.close();
            }
        }
        ui.separator();
        if ui.button("Group Layers  (Ctrl+G)").clicked() {
            actions.layers.group_selected = true;
            ui.close();
        }
        if is_group {
            if ui.button("Ungroup  (Ctrl+Shift+G)").clicked() {
                actions.layers.ungroup = Some(idx);
                ui.close();
            }
            if ui.button("Merge Group").clicked() {
                actions.layers.merge_group = Some(idx);
                ui.close();
            }
        }
        ui.separator();
        if ui.button("Merge Down").clicked() {
            actions.layers.merge_down = Some(idx);
            ui.close();
        }
        if ui.button("Stamp Visible  (Ctrl+Shift+E)").clicked() {
            actions.layers.merge_visible = true;
            ui.close();
        }
        ui.separator();
        if ui
            .add(egui::Button::new(
                egui::RichText::new("Delete Layer").color(pal.danger),
            ))
            .clicked()
        {
            actions.layers.remove_layer = Some(idx);
            ui.close();
        }
    });
}

/// Channels panel: composite + R/G/B rows and saved alpha channels. Clicking
/// a row selects what tools write to (and what the canvas displays); Ctrl or
/// Shift-click adds/removes a colour channel from the write set, like PTS.
fn channels_panel_contents(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    use crate::core::channels::ChannelView;

    ui.add_space(4.0);
    let additive = ui.input(|i| i.modifiers.ctrl || i.modifiers.shift);
    let viewing_alpha = matches!(data.channels.channel_view, ChannelView::Alpha(_));

    let row = |ui: &mut egui::Ui,
               selected: bool,
               eye_on: bool,
               name: &str,
               thumb: Option<&[u8]>,
               thumb_id: egui::Id|
     -> bool {
        let mut clicked = false;
        ui.horizontal(|ui| {
            let eye_resp = ps_clickable_eye_button(ui, eye_on).on_hover_text(if eye_on {
                "Channel visible in current view"
            } else {
                "View channel"
            });
            if eye_resp.clicked() {
                clicked = true;
            }
            ps_layer_thumbnail(
                ui,
                "Raster",
                false,
                egui::vec2(CHANNEL_PANEL_THUMB_SIZE, CHANNEL_PANEL_THUMB_SIZE),
                thumb,
                thumb_id,
                selected,
            );
            ui.add_space(2.0);
            if ui
                .add_sized(
                    [ui.available_width().max(40.0), CHANNEL_PANEL_THUMB_SIZE],
                    egui::Button::selectable(selected, name),
                )
                .clicked()
            {
                clicked = true;
            }
        });
        clicked
    };

    let color_thumb = |idx: usize| {
        data.channels
            .channel_thumbnails
            .get(idx)
            .map(|v| v.as_slice())
    };

    // Composite + per-channel rows follow the document's colour mode:
    // RGB → RGB/Red/Green/Blue, CMYK → CMYK/Cyan/Magenta/Yellow/Black.
    let (composite_name, channel_names): (&str, &[&str]) = if data.doc.is_cmyk {
        ("CMYK", &["Cyan", "Magenta", "Yellow", "Black"])
    } else {
        ("RGB", &["Red", "Green", "Blue"])
    };

    if row(
        ui,
        data.channels.channel_view == ChannelView::Composite && !viewing_alpha,
        data.channels.channel_view == ChannelView::Composite && !viewing_alpha,
        composite_name,
        color_thumb(0),
        egui::Id::new("channel_thumb_composite"),
    ) {
        actions.channels.select_channel_row = Some((ChannelView::Composite, false));
    }
    for (c, name) in channel_names.iter().enumerate() {
        let selected = !viewing_alpha
            && data.channels.channels_write_mask[c]
            && !(data.channels.channel_view == ChannelView::Composite
                && data.channels.channels_write_mask == [true; 4]);
        if row(
            ui,
            selected,
            !viewing_alpha && data.channels.channels_write_mask[c],
            name,
            color_thumb(c + 1),
            egui::Id::new("channel_thumb_color").with(c),
        ) {
            actions.channels.select_channel_row = Some((ChannelView::Single(c as u8), additive));
        }
    }

    if !data.channels.alpha_channels.is_empty() {
        ui.add_space(2.0);
        ui.separator();
        let rename_id = egui::Id::new("channels_alpha_rename");
        let rename_text_id = egui::Id::new("channels_alpha_rename_text");
        let renaming: Option<u32> = ui.ctx().data(|d| d.get_temp(rename_id));
        for (i, (id, name)) in data.channels.alpha_channels.iter().enumerate() {
            if renaming == Some(*id) {
                // Inline rename: Enter/click-away commits, Esc cancels.
                let mut text: String = ui
                    .ctx()
                    .data(|d| d.get_temp(rename_text_id))
                    .unwrap_or_else(|| name.clone());
                let resp =
                    ui.add(egui::TextEdit::singleline(&mut text).desired_width(f32::INFINITY));
                resp.request_focus();
                if resp.changed() {
                    ui.ctx()
                        .data_mut(|d| d.insert_temp(rename_text_id, text.clone()));
                }
                let cancel = ui.input(|inp| inp.key_pressed(egui::Key::Escape));
                let commit = !cancel
                    && (ui.input(|inp| inp.key_pressed(egui::Key::Enter)) || resp.lost_focus());
                if commit || cancel {
                    if commit && !text.trim().is_empty() && text.trim() != name {
                        actions.channels.rename_alpha_channel = Some((i, text.trim().to_string()));
                    }
                    ui.ctx().data_mut(|d| {
                        d.remove::<u32>(rename_id);
                        d.remove::<String>(rename_text_id);
                    });
                }
                continue;
            }

            let selected = data.channels.channel_view == ChannelView::Alpha(*id);
            let mut clicked = false;
            let label_resp = ui
                .horizontal(|ui| {
                    let eye_resp =
                        ps_clickable_eye_button(ui, selected).on_hover_text(if selected {
                            "Channel visible in current view"
                        } else {
                            "View channel"
                        });
                    if eye_resp.clicked() {
                        clicked = true;
                    }
                    ps_layer_thumbnail(
                        ui,
                        "Raster",
                        false,
                        egui::vec2(CHANNEL_PANEL_THUMB_SIZE, CHANNEL_PANEL_THUMB_SIZE),
                        data.channels.alpha_thumbnails.get(i).map(|v| v.as_slice()),
                        egui::Id::new("channel_thumb_alpha").with(*id),
                        selected,
                    );
                    ui.add_space(2.0);
                    let resp = ui.add_sized(
                        [ui.available_width().max(40.0), CHANNEL_PANEL_THUMB_SIZE],
                        egui::Button::selectable(selected, name),
                    );
                    if resp.clicked() {
                        clicked = true;
                    }
                    resp
                })
                .inner;
            if clicked {
                actions.channels.select_channel_row = Some((ChannelView::Alpha(*id), false));
            }
            if label_resp.double_clicked() {
                ui.ctx().data_mut(|d| {
                    d.insert_temp(rename_id, *id);
                    d.insert_temp(rename_text_id, name.clone());
                });
            }
            label_resp.context_menu(|ui| {
                if ui.button("Rename…").clicked() {
                    ui.ctx().data_mut(|d| {
                        d.insert_temp(rename_id, *id);
                        d.insert_temp(rename_text_id, name.clone());
                    });
                    ui.close();
                }
                if ui.button("Duplicate").clicked() {
                    actions.channels.duplicate_alpha_channel = Some(i);
                    ui.close();
                }
                if ui.button("Delete").clicked() {
                    actions.channels.delete_alpha_channel = Some(i);
                    ui.close();
                }
            });
        }
    }

    // Bottom strip: load-as-selection / save-selection / new / delete.
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let alpha_target = data.channels.active_alpha_channel;
        if ui
            .add_enabled(
                alpha_target.is_some(),
                egui::Button::new(egui::RichText::new(ph::CIRCLE_DASHED).size(13.0)),
            )
            .on_hover_text(
                "Load channel as selection\nCtrl = Add · Alt = Subtract · Ctrl+Alt = Intersect",
            )
            .clicked()
        {
            if let Some(idx) = alpha_target {
                use crate::core::selection::MaskCombine;
                let (ctrl, alt) = ui.input(|inp| (inp.modifiers.ctrl, inp.modifiers.alt));
                let mode = match (ctrl, alt) {
                    (true, true) => MaskCombine::Intersect,
                    (true, false) => MaskCombine::Add,
                    (false, true) => MaskCombine::Subtract,
                    (false, false) => MaskCombine::Replace,
                };
                actions.channels.load_channel_as_selection = Some((idx, mode));
            }
        }
        if ui
            .button(egui::RichText::new(ph::SELECTION_PLUS).size(13.0))
            .on_hover_text("Save selection as channel")
            .clicked()
        {
            actions.channels.save_selection_as_channel = true;
        }
        if ui
            .button(egui::RichText::new(ph::PLUS).size(13.0))
            .on_hover_text("New channel")
            .clicked()
        {
            actions.channels.new_alpha_channel = true;
        }
        if ui
            .add_enabled(
                alpha_target.is_some(),
                egui::Button::new(egui::RichText::new(ph::TRASH).size(13.0)),
            )
            .on_hover_text("Delete channel")
            .clicked()
        {
            if let Some(idx) = alpha_target {
                actions.channels.delete_alpha_channel = Some(idx);
            }
        }
    });
}

fn history_panel(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    ui.add_space(4.0);

    if data.doc.history_entries.is_empty() {
        ui.label(
            egui::RichText::new("No history")
                .color(egui::Color32::GRAY)
                .size(11.0),
        );
        return;
    }

    egui::ScrollArea::vertical()
        .max_height(150.0)
        .show(ui, |ui| {
            for (i, entry) in data.doc.history_entries.iter().enumerate() {
                let is_current = entry.is_current;
                let text_color = if is_current {
                    egui::Color32::WHITE
                } else if entry.is_undone {
                    egui::Color32::from_gray(100)
                } else {
                    egui::Color32::LIGHT_GRAY
                };

                let resp = ui.selectable_label(
                    is_current,
                    egui::RichText::new(&entry.label)
                        .size(11.0)
                        .color(text_color),
                );
                if resp.clicked() {
                    actions.doc.jump_history = Some(i);
                }
            }
        });

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui.add(egui::Button::new("Undo").small()).clicked() {
            actions.doc.undo = true;
        }
        if ui.add(egui::Button::new("Redo").small()).clicked() {
            actions.doc.redo = true;
        }
    });
}

fn info_panel(ui: &mut egui::Ui, data: &UiData) {
    ui.add_space(4.0);

    let row = |ui: &mut egui::Ui, label: &str, value: &str| {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(label)
                    .color(egui::Color32::GRAY)
                    .size(11.0),
            );
            ui.label(egui::RichText::new(value).size(11.0));
        });
    };

    row(
        ui,
        "Canvas:",
        &format!("{}×{} px", data.doc.canvas_w, data.doc.canvas_h),
    );
    let dpi = data.doc.canvas_dpi;
    let unit = data.doc.canvas_unit;
    if unit != crate::core::units::Unit::Pixels {
        let pw = crate::core::units::from_pixels(data.doc.canvas_w as f32, unit, dpi, 0.0);
        let ph = crate::core::units::from_pixels(data.doc.canvas_h as f32, unit, dpi, 0.0);
        row(
            ui,
            "Print:",
            &format!(
                "{} × {} {}",
                crate::core::units::format_value(pw, unit),
                crate::core::units::format_value(ph, unit),
                unit.name()
            ),
        );
    }
    row(ui, "DPI:", &format!("{:.0}", data.doc.canvas_dpi));
    row(ui, "Depth:", &format!("{}-bit", data.doc.canvas_bit_depth));
    row(ui, "Profile:", &data.doc.doc_profile_name);
    row(ui, "Zoom:", &format!("{:.0}%", data.doc.zoom * 100.0));
    row(ui, "Layers:", &format!("{}", data.layers.layer_count));
    row(ui, "Undo:", &format!("{} steps", data.doc.undo_count));

    let color = data.tool.brush_color;
    row(
        ui,
        "Color:",
        &format!("#{:02X}{:02X}{:02X}", color[0], color[1], color[2]),
    );
    if let Some(ink) = data.tool.brush_ink {
        let pct = |v: u8| (v as f32 / 255.0 * 100.0).round() as u32;
        row(
            ui,
            "Ink:",
            &format!(
                "C{} M{} Y{} K{} %",
                pct(ink[0]),
                pct(ink[1]),
                pct(ink[2]),
                pct(ink[3])
            ),
        );
    }

    if !data.chrome.status_msg.is_empty() {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(&data.chrome.status_msg)
                .color(egui::Color32::from_rgb(180, 220, 180))
                .size(11.0),
        );
    }
}

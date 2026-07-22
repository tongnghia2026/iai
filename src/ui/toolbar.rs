use super::{UiActions, UiData};
use crate::tools::ToolId;
use egui;
use egui_phosphor::regular as ph;

const SEL_GROUP: &[(ToolId, &str, &str)] = &[
    (ToolId::SelectionRect, ph::SELECTION, "Rect Selection (M)"),
    (
        ToolId::SelectionEllipse,
        ph::CIRCLE_DASHED,
        "Ellipse Selection (M)",
    ),
];

const WAND_GROUP: &[(ToolId, &str, &str)] =
    &[(ToolId::SmartSelect, ph::SPARKLE, "Smart Select (W)")];

const LASSO_GROUP: &[(ToolId, &str, &str)] = &[
    (ToolId::Lasso, ph::LASSO, "Lasso (L)"),
    (ToolId::PolygonLasso, ph::POLYGON, "Polygon Lasso (L)"),
];

const MOVE_GROUP: &[(ToolId, &str, &str)] = &[(ToolId::Move, ph::ARROWS_OUT_CARDINAL, "Move (V)")];

const CROP_GROUP: &[(ToolId, &str, &str)] = &[
    (ToolId::Crop, ph::CROP, "Crop (C)"),
    (
        ToolId::PerspectiveCrop,
        ph::PERSPECTIVE,
        "Perspective Crop (C)",
    ),
];

const TEXT_GROUP: &[(ToolId, &str, &str)] = &[(ToolId::Text, ph::TEXT_T, "Type (T)")];

const SHAPE_GROUP: &[(ToolId, &str, &str)] = &[(ToolId::Shape, ph::SHAPES, "Shape (U)")];

const PEN_GROUP: &[(ToolId, &str, &str)] = &[(ToolId::Pen, ph::PEN_NIB, "Pen (P)")];

const NODE_GROUP: &[(ToolId, &str, &str)] =
    &[(ToolId::Node, ph::BEZIER_CURVE, "Node — edit points (A)")];

const BRUSH_GROUP: &[(ToolId, &str, &str)] = &[
    (ToolId::Brush, ph::PAINT_BRUSH, "Brush (B)"),
    (ToolId::Pencil, ph::PENCIL, "Pencil (B)"),
];

const ERASER_GROUP: &[(ToolId, &str, &str)] = &[(ToolId::Eraser, ph::ERASER, "Eraser (E)")];

const FILL_GROUP: &[(ToolId, &str, &str)] = &[
    (ToolId::Fill, ph::PAINT_BUCKET, "Fill (G)"),
    (ToolId::Gradient, ph::GRADIENT, "Gradient (G)"),
];

const EYEDROPPER_GROUP: &[(ToolId, &str, &str)] =
    &[(ToolId::Eyedropper, ph::EYEDROPPER, "Eyedropper (I)")];

const CLONE_GROUP: &[(ToolId, &str, &str)] = &[(ToolId::Clone, ph::STAMP, "Clone (S)")];

const HEAL_GROUP: &[(ToolId, &str, &str)] = &[(ToolId::Repair, ph::BANDAIDS, "Repair Brush (J)")];

const PATCH_GROUP: &[(ToolId, &str, &str)] = &[(ToolId::Patch, ph::SELECTION_PLUS, "Patch (J)")];

const SMUDGE_GROUP: &[(ToolId, &str, &str)] = &[(ToolId::Smudge, ph::FINGERPRINT, "Smudge")];

const DODGE_GROUP: &[(ToolId, &str, &str)] = &[
    (ToolId::Dodge, ph::SUN, "Dodge (O)"),
    (ToolId::Burn, ph::MOON, "Burn (O)"),
];

const ZOOM_GROUP: &[(ToolId, &str, &str)] = &[(ToolId::Zoom, ph::MAGNIFYING_GLASS, "Zoom (Z)")];

const HAND_GROUP: &[(ToolId, &str, &str)] = &[(ToolId::Hand, ph::HAND, "Hand (H)")];

const TOOL_GROUPS: &[&[(ToolId, &str, &str)]] = &[
    MOVE_GROUP,
    SEL_GROUP,
    LASSO_GROUP,
    WAND_GROUP,
    CROP_GROUP,
    EYEDROPPER_GROUP,
    HEAL_GROUP,
    PATCH_GROUP,
    BRUSH_GROUP,
    CLONE_GROUP,
    ERASER_GROUP,
    FILL_GROUP,
    SMUDGE_GROUP,
    DODGE_GROUP,
    PEN_GROUP,
    NODE_GROUP,
    TEXT_GROUP,
    SHAPE_GROUP,
    HAND_GROUP,
    ZOOM_GROUP,
];

const TOOLBOX_TOOL_W: f32 = 36.0;
const TOOLBOX_TOOL_H: f32 = 30.0;
const TOOLBOX_GRID_COLS: usize = 3;
const TOOLBOX_ROW_SPACING_Y: f32 = 3.0;
const TOOLBOX_HEADER_BTN_H: f32 = 28.0;
const TOOLBOX_HEADER_SPACING_Y: f32 = 4.0;
const TOOLBOX_SECTION_H: f32 = 20.0;
const TOOLBOX_SWATCH_H: f32 = 38.0;
const TOOLBOX_FRAME_EXTRA_H: f32 = 6.0;
const TOOLBOX_THREE_COLUMN_EXTRA_H: f32 = 38.0;
const TOOLBAR_TOP_H: f32 = 86.0;
const RULER_SIZE: f32 = 20.0;
const STATUSBAR_H: f32 = 22.0;

pub fn build(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let pal = data.chrome.theme_mode.palette();

    #[allow(deprecated)]
    egui::SidePanel::left("toolbar")
        .exact_size(data.chrome.toolbar_w)
        .resizable(false)
        .frame(egui::Frame::new().fill(pal.toolbar_bg))
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(6.0);

                toolbox_toggle_btn(ui, data, actions);
                ui.add_space(8.0);
                active_tool_btn(ui, data, actions);

                ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                    ui.add_space(8.0);
                    color_swatches(ui, data, actions);
                });
            });
        });

    if data.chrome.toolbox_open {
        toolbox_popup(ctx, data, actions);
    }
}

fn toolbox_toggle_btn(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    let pal = data.chrome.theme_mode.palette();
    let mut btn = egui::Button::new(egui::RichText::new(ph::TOOLBOX).size(18.0).color(pal.icon))
        .min_size(egui::vec2(36.0, 34.0));
    if data.chrome.toolbox_open {
        btn = btn.fill(pal.accent_selected_bg);
    }

    let resp = ui.add(btn).on_hover_text(if data.chrome.toolbox_open {
        "Close Toolbox"
    } else {
        "Open Toolbox"
    });
    if resp.clicked() {
        actions.chrome.set_toolbox_open = Some(!data.chrome.toolbox_open);
    }
}

fn active_tool_btn(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    if let Some(group) = TOOL_GROUPS
        .iter()
        .copied()
        .find(|group| group.iter().any(|&(id, _, _)| id == data.tool.active_tool))
    {
        group_btn(ui, data, actions, group);
    }
}

fn toolbox_popup(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let pal = data.chrome.theme_mode.palette();
    let screen = ctx.content_rect();
    let default_pos = egui::pos2(data.chrome.toolbar_w + 8.0, 92.0);
    let rest_pos = data
        .chrome
        .toolbox_pos
        .map(|(x, y)| egui::pos2(x, y))
        .unwrap_or(default_pos);
    let pos = clamp_toolbox_pos(rest_pos, data, screen);
    let docked = toolbox_is_docked(data, pos);
    let margin = toolbox_margin(docked);
    let fixed_h = toolbox_fixed_h(data, docked, margin);
    let max_h = (screen.max.y - STATUSBAR_H - pos.y - fixed_h).max(120.0);
    let content_w = toolbox_content_w(data, docked);
    let mut scroll_area = egui::ScrollArea::vertical().max_height(max_h);
    if !docked && !data.chrome.toolbox_single_column {
        scroll_area = scroll_area
            .min_scrolled_height(toolbox_scroll_content_h(data))
            .auto_shrink([false, false]);
    } else {
        scroll_area = scroll_area.auto_shrink([false, true]);
    }

    egui::Area::new(egui::Id::new("toolbox_popup"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .fill(pal.window_bg)
                .stroke(egui::Stroke::new(1.0_f32, pal.separator))
                .inner_margin(egui::Margin::same(margin))
                .show(ui, |ui| {
                    ui.set_min_width(content_w);
                    ui.set_max_width(content_w);
                    if docked {
                        let docked_h = toolbox_docked_content_h(screen, pos, margin);
                        ui.set_min_size(egui::vec2(content_w, docked_h));
                        ui.set_max_height(docked_h);
                    }

                    toolbox_header(ui, data, actions, pos, screen, docked);

                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(6.0);

                    // Strict modal lock: tool switching is disabled while a
                    // modal operation is open (the header stays live so the
                    // toolbox can still be moved or closed).
                    let tools_ui = ui.add_enabled_ui(!data.chrome.is_tool_modal, |ui| {
                        scroll_area.show(ui, |ui| {
                            let columns = toolbox_columns(data);
                            egui::Grid::new("toolbox_grid")
                                .num_columns(columns)
                                .spacing(egui::vec2(4.0, TOOLBOX_ROW_SPACING_Y))
                                .show(ui, |ui| {
                                    for (i, group) in TOOL_GROUPS.iter().copied().enumerate() {
                                        group_btn(ui, data, actions, group);
                                        if (i + 1) % columns == 0 {
                                            ui.end_row();
                                        }
                                    }
                                    if !TOOL_GROUPS.len().is_multiple_of(columns) {
                                        ui.end_row();
                                    }
                                });

                            ui.add_space(6.0);
                            ui.separator();
                            ui.add_space(6.0);
                            ui.vertical_centered(|ui| color_swatches(ui, data, actions));
                            if !data.chrome.toolbox_single_column {
                                ui.add_space(TOOLBOX_THREE_COLUMN_EXTRA_H);
                            }
                        });
                    });
                    if data.chrome.is_tool_modal
                        && ui.input(|i| i.pointer.primary_clicked())
                        && ui
                            .input(|i| i.pointer.interact_pos())
                            .is_some_and(|pos| tools_ui.response.rect.contains(pos))
                    {
                        actions.chrome.modal_denied = true;
                    }
                });
        });
}

fn toolbox_header(
    ui: &mut egui::Ui,
    data: &UiData,
    actions: &mut UiActions,
    pos: egui::Pos2,
    screen: egui::Rect,
    docked: bool,
) {
    if docked {
        ui.vertical_centered(|ui| {
            toolbox_drag_grip(ui, data, actions, pos, screen);
            toolbox_mode_button(ui, data, actions, screen);
        });
        return;
    }

    if data.chrome.toolbox_single_column {
        ui.vertical_centered(|ui| {
            toolbox_drag_grip(ui, data, actions, pos, screen);
            toolbox_drag_button(ui, data, actions, pos, screen);
            toolbox_mode_button(ui, data, actions, screen);
            toolbox_close_button(ui, actions);
        });
    } else {
        ui.horizontal(|ui| {
            toolbox_drag_button(ui, data, actions, pos, screen);
            toolbox_drag_grip(ui, data, actions, pos, screen);
            toolbox_mode_button(ui, data, actions, screen);
            toolbox_close_button(ui, actions);
        });
    }
}

fn toolbox_drag_button(
    ui: &mut egui::Ui,
    data: &UiData,
    actions: &mut UiActions,
    pos: egui::Pos2,
    screen: egui::Rect,
) {
    let resp = ui
        .add(
            egui::Button::new(
                egui::RichText::new(ph::TOOLBOX)
                    .size(16.0)
                    .color(data.chrome.theme_mode.palette().icon),
            )
            .min_size(egui::vec2(28.0, 28.0)),
        )
        .on_hover_text("Drag Toolbox");
    if resp.dragged() {
        move_toolbox(actions, data, screen, pos + resp.drag_delta(), false);
    }
    if resp.drag_stopped() {
        move_toolbox(actions, data, screen, pos + resp.drag_delta(), true);
    }
}

fn toolbox_drag_grip(
    ui: &mut egui::Ui,
    data: &UiData,
    actions: &mut UiActions,
    pos: egui::Pos2,
    screen: egui::Rect,
) {
    let pal = data.chrome.theme_mode.palette();
    let grip_w = if data.chrome.toolbox_single_column {
        ui.available_width().max(28.0)
    } else {
        24.0
    };
    let (drag_rect, drag_resp) =
        ui.allocate_exact_size(egui::vec2(grip_w, 28.0), egui::Sense::drag());
    ui.painter().text(
        drag_rect.center(),
        egui::Align2::CENTER_CENTER,
        ph::DOTS_SIX_VERTICAL,
        egui::FontId::proportional(15.0),
        pal.icon,
    );
    if drag_resp.dragged() {
        move_toolbox(actions, data, screen, pos + drag_resp.drag_delta(), false);
    }
    if drag_resp.drag_stopped() {
        move_toolbox(actions, data, screen, pos + drag_resp.drag_delta(), true);
    }
}

fn toolbox_mode_button(
    ui: &mut egui::Ui,
    data: &UiData,
    actions: &mut UiActions,
    screen: egui::Rect,
) {
    let icon = if data.chrome.toolbox_single_column {
        ph::GRID_NINE
    } else {
        ph::LIST
    };
    let tip = if data.chrome.toolbox_single_column {
        "Switch to 3 Columns"
    } else {
        "Switch to 1 Column"
    };
    let resp = ui
        .add(
            egui::Button::new(
                egui::RichText::new(icon)
                    .size(15.0)
                    .color(data.chrome.theme_mode.palette().icon),
            )
            .min_size(egui::vec2(28.0, 28.0)),
        )
        .on_hover_text(tip);
    if resp.clicked() {
        let next_single_column = !data.chrome.toolbox_single_column;
        actions.chrome.set_toolbox_single_column = Some(next_single_column);
        if next_single_column {
            actions.chrome.set_toolbox_pos = Some((0.0, toolbar_top_y(data, screen)));
        }
    }
}

fn toolbox_close_button(ui: &mut egui::Ui, actions: &mut UiActions) {
    let icon_color = ui.visuals().text_color();
    let close = egui::Button::new(egui::RichText::new(ph::X).size(14.0).color(icon_color))
        .min_size(egui::vec2(28.0, 28.0));
    if ui.add(close).on_hover_text("Close").clicked() {
        actions.chrome.set_toolbox_open = Some(false);
    }
}

fn move_toolbox(
    actions: &mut UiActions,
    data: &UiData,
    screen: egui::Rect,
    desired_pos: egui::Pos2,
    snap: bool,
) {
    let next = if snap {
        snap_toolbox_pos(desired_pos, data, screen)
    } else {
        clamp_toolbox_pos(desired_pos, data, screen)
    };
    actions.chrome.set_toolbox_pos = Some((next.x, next.y));
}

fn toolbox_columns(data: &UiData) -> usize {
    if data.chrome.toolbox_single_column {
        1
    } else {
        TOOLBOX_GRID_COLS
    }
}

fn toolbox_content_w(data: &UiData, docked: bool) -> f32 {
    if docked {
        return (data.chrome.toolbar_w - f32::from(toolbox_margin(true)) * 2.0).max(TOOLBOX_TOOL_W);
    }
    if data.chrome.toolbox_single_column {
        return 38.0;
    }
    let columns = toolbox_columns(data) as f32;
    columns * TOOLBOX_TOOL_W + (columns - 1.0).max(0.0) * 4.0
}

fn toolbox_est_w(data: &UiData) -> f32 {
    toolbox_content_w(data, false) + f32::from(toolbox_margin(false)) * 2.0
}

fn toolbox_est_h(data: &UiData) -> f32 {
    let margin = toolbox_margin(false);
    toolbox_fixed_h(data, false, margin) + toolbox_scroll_content_h(data)
}

fn toolbox_fixed_h(data: &UiData, docked: bool, margin: i8) -> f32 {
    f32::from(margin) * 2.0
        + toolbox_header_h(data, docked)
        + TOOLBOX_SECTION_H
        + TOOLBOX_FRAME_EXTRA_H
}

fn toolbox_scroll_content_h(data: &UiData) -> f32 {
    let rows = toolbox_row_count(data);
    let tool_h =
        rows as f32 * TOOLBOX_TOOL_H + rows.saturating_sub(1) as f32 * TOOLBOX_ROW_SPACING_Y;
    let extra_h = if data.chrome.toolbox_single_column {
        0.0
    } else {
        TOOLBOX_THREE_COLUMN_EXTRA_H
    };
    tool_h + TOOLBOX_SECTION_H + TOOLBOX_SWATCH_H + extra_h
}

fn toolbox_header_h(data: &UiData, docked: bool) -> f32 {
    let buttons: usize = if docked {
        2
    } else if data.chrome.toolbox_single_column {
        4
    } else {
        1
    };
    buttons as f32 * TOOLBOX_HEADER_BTN_H
        + buttons.saturating_sub(1) as f32 * TOOLBOX_HEADER_SPACING_Y
}

fn toolbox_row_count(data: &UiData) -> usize {
    let columns = toolbox_columns(data).max(1);
    TOOL_GROUPS.len().div_ceil(columns)
}

fn snap_toolbox_pos(pos: egui::Pos2, data: &UiData, screen: egui::Rect) -> egui::Pos2 {
    let mut pos = clamp_toolbox_pos(pos, data, screen);
    if data.chrome.toolbox_single_column && pos.x <= data.chrome.toolbar_w + 18.0 {
        pos.x = 0.0;
        pos.y = toolbar_top_y(data, screen);
    }
    pos
}

fn clamp_toolbox_pos(pos: egui::Pos2, data: &UiData, screen: egui::Rect) -> egui::Pos2 {
    if data.chrome.toolbox_single_column && pos.x <= 0.5 {
        return egui::pos2(0.0, toolbar_top_y(data, screen));
    }

    let min_x = if data.chrome.toolbox_single_column {
        0.0
    } else {
        data.chrome.toolbar_w + 4.0
    };
    let max_x = (screen.max.x - toolbox_est_w(data)).max(min_x);
    let max_y = (screen.max.y - STATUSBAR_H - toolbox_est_h(data)).max(34.0);
    egui::pos2(pos.x.clamp(min_x, max_x), pos.y.clamp(34.0, max_y))
}

fn toolbox_is_docked(data: &UiData, pos: egui::Pos2) -> bool {
    data.chrome.toolbox_single_column && pos.x <= 0.5
}

fn toolbar_top_y(data: &UiData, screen: egui::Rect) -> f32 {
    screen.min.y
        + TOOLBAR_TOP_H
        + if data.chrome.show_rulers {
            RULER_SIZE
        } else {
            0.0
        }
}

fn toolbox_docked_content_h(screen: egui::Rect, pos: egui::Pos2, margin: i8) -> f32 {
    (screen.max.y - STATUSBAR_H - pos.y - f32::from(margin) * 2.0).max(160.0)
}

fn toolbox_margin(docked: bool) -> i8 {
    if docked {
        5
    } else {
        8
    }
}

/// One toolbar slot for a group.
/// - Left-click  → activate the currently-shown tool.
/// - Right-click → context menu with all tools in group.
/// - Small ▶ triangle in corner marks groups with more than one tool.
fn group_btn(
    ui: &mut egui::Ui,
    data: &UiData,
    actions: &mut UiActions,
    entries: &[(ToolId, &str, &str)],
) {
    let pal = data.chrome.theme_mode.palette();
    let active_entry = entries
        .iter()
        .find(|&&(id, _, _)| id == data.tool.active_tool);
    let preferred_entry = data
        .tool
        .tool_group_preferences
        .iter()
        .find_map(|pref| entries.iter().find(|&&(id, _, _)| id == *pref));
    let (shown_id, shown_icon, shown_tip) = active_entry.or(preferred_entry).unwrap_or(&entries[0]);
    let is_active = active_entry.is_some();
    let has_multi = entries.len() > 1;

    // Flat design: only the active tool keeps a filled box (the accent); inactive
    // slots are transparent and reveal a hover fill via the egui widget visuals.
    let icon_color = pal.icon;
    let mut btn = egui::Button::new(
        egui::RichText::new(*shown_icon)
            .size(15.0)
            .color(icon_color),
    )
    .min_size(egui::vec2(TOOLBOX_TOOL_W, TOOLBOX_TOOL_H));
    if is_active {
        btn = btn.fill(pal.accent_selected_bg);
    }

    let resp = ui.add(btn).on_hover_text(*shown_tip);

    if resp.clicked() {
        actions.tool.select_tool = Some(*shown_id);
    }

    if has_multi {
        let r = resp.rect;
        let size = 5.0f32;
        let br = r.right_bottom();
        let p0 = egui::pos2(br.x - size - 1.0, br.y - 1.0);
        let p1 = egui::pos2(br.x - 1.0, br.y - size - 1.0);
        let p2 = egui::pos2(br.x - 1.0, br.y - 1.0);
        let col = if is_active {
            pal.accent_hover
        } else {
            pal.icon
        };
        ui.painter().add(egui::Shape::convex_polygon(
            vec![p0, p1, p2],
            col,
            egui::Stroke::NONE,
        ));

        resp.context_menu(|ui| {
            ui.set_min_width(160.0);
            for &(entry_id, entry_icon, entry_name) in entries {
                let checked = entry_id == data.tool.active_tool;
                let label = format!("{} {}", entry_icon, entry_name);
                if ui.add(egui::Button::selectable(checked, label)).clicked() {
                    actions.tool.select_tool = Some(entry_id);
                    ui.close();
                }
            }
        });
    }
}

fn color_swatches(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    let pal = data.chrome.theme_mode.palette();
    let area_size = egui::vec2(38.0, 38.0);
    let swatch_size = egui::vec2(20.0, 20.0);
    let action_size = egui::vec2(11.0, 11.0);

    let (area, _) = ui.allocate_exact_size(area_size, egui::Sense::hover());

    let fg_rect = egui::Rect::from_min_size(area.min + egui::vec2(4.0, 4.0), swatch_size);
    let bg_rect = egui::Rect::from_min_size(area.min + egui::vec2(14.0, 14.0), swatch_size);
    let swap_rect = egui::Rect::from_min_size(area.min + egui::vec2(27.0, 1.0), action_size);
    let reset_rect = egui::Rect::from_min_size(area.min + egui::vec2(1.0, 27.0), action_size);

    let bg_resp = ui
        .allocate_rect(bg_rect, egui::Sense::click())
        .on_hover_text("Background Color");
    let fg_resp = ui
        .allocate_rect(fg_rect, egui::Sense::click())
        .on_hover_text("Foreground Color");
    let swap_resp = ui
        .allocate_rect(swap_rect, egui::Sense::click())
        .on_hover_text("Swap FG/BG (X)");
    let reset_resp = ui
        .allocate_rect(reset_rect, egui::Sense::click())
        .on_hover_text("Reset to Black/White (D)");

    let p = ui.painter();

    let bg_c = egui::Color32::from_rgb(
        data.tool.bg_color[0],
        data.tool.bg_color[1],
        data.tool.bg_color[2],
    );
    paint_swatch(p, bg_rect, bg_c, bg_resp.hovered(), pal);

    let fg_c = egui::Color32::from_rgb(
        data.tool.brush_color[0],
        data.tool.brush_color[1],
        data.tool.brush_color[2],
    );
    paint_swatch(p, fg_rect, fg_c, fg_resp.hovered(), pal);

    paint_swap_button(p, swap_rect, swap_resp.hovered(), pal);
    paint_reset_button(p, reset_rect, reset_resp.hovered(), pal);

    if swap_resp.clicked() {
        actions.tool.swap_colors = true;
    }
    if reset_resp.clicked() {
        actions.tool.brush_color = Some([0, 0, 0, 255]);
        actions.tool.bg_color = Some([255, 255, 255, 255]);
    }
    if bg_resp.clicked() {
        actions.dialogs.open_paint_color_dialog = Some(1);
    }
    if fg_resp.clicked() {
        actions.dialogs.open_paint_color_dialog = Some(0);
    }
}

fn paint_swatch(
    p: &egui::Painter,
    rect: egui::Rect,
    color: egui::Color32,
    hovered: bool,
    pal: crate::ui::theme::Palette,
) {
    p.rect_filled(rect, 1.0, pal.separator);
    p.rect_filled(rect.shrink(2.0), 1.0, color);
    p.rect_stroke(
        rect,
        1.0,
        egui::Stroke::new(if hovered { 1.5_f32 } else { 1.0_f32 }, pal.icon),
        egui::StrokeKind::Inside,
    );
}

fn paint_swap_button(
    p: &egui::Painter,
    rect: egui::Rect,
    hovered: bool,
    pal: crate::ui::theme::Palette,
) {
    p.rect_filled(
        rect,
        2.0,
        if hovered {
            pal.button_hover
        } else {
            pal.button_bg
        },
    );
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        ph::SWAP,
        egui::FontId::proportional(9.0),
        pal.icon,
    );
}

fn paint_reset_button(
    p: &egui::Painter,
    rect: egui::Rect,
    hovered: bool,
    pal: crate::ui::theme::Palette,
) {
    p.rect_filled(
        rect,
        2.0,
        if hovered {
            pal.button_hover
        } else {
            pal.button_bg
        },
    );

    let mini = 6.0_f32;
    let black_sq =
        egui::Rect::from_min_size(rect.min + egui::vec2(1.5, 1.5), egui::vec2(mini, mini));
    let white_sq =
        egui::Rect::from_min_size(rect.min + egui::vec2(3.5, 3.5), egui::vec2(mini, mini));
    p.rect_filled(white_sq, 1.0, egui::Color32::WHITE);
    p.rect_stroke(
        white_sq,
        1.0,
        egui::Stroke::new(0.5_f32, pal.separator),
        egui::StrokeKind::Outside,
    );
    p.rect_filled(black_sq, 1.0, egui::Color32::BLACK);
    p.rect_stroke(
        black_sq,
        1.0,
        egui::Stroke::new(0.5_f32, pal.separator),
        egui::StrokeKind::Outside,
    );
}

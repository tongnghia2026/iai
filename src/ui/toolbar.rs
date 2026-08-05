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

const VECTOR_BRUSH_GROUP: &[(ToolId, &str, &str)] = &[(
    ToolId::VectorBrush,
    ph::SCRIBBLE_LOOP,
    "Vector Brush — freehand editable stroke",
)];

const ARROW_GROUP: &[(ToolId, &str, &str)] =
    &[((ToolId::Arrow, ph::ARROW_UP_RIGHT, "Arrow / Connector"))];

const BRUSH_GROUP: &[(ToolId, &str, &str)] = &[
    (ToolId::Brush, ph::PAINT_BRUSH, "Brush (B)"),
    (ToolId::Pencil, ph::PENCIL, "Pencil (B)"),
];

const ERASER_GROUP: &[(ToolId, &str, &str)] = &[(ToolId::Eraser, ph::ERASER, "Eraser (E)")];

const PIXEL_FILL_GROUP: &[(ToolId, &str, &str)] = &[(ToolId::Fill, ph::PAINT_BUCKET, "Fill (G)")];

const GRADIENT_GROUP: &[(ToolId, &str, &str)] = &[(ToolId::Gradient, ph::GRADIENT, "Gradient (G)")];

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

const COMMON_TOP_GROUPS: &[&[(ToolId, &str, &str)]] = &[
    MOVE_GROUP,
    SEL_GROUP,
    LASSO_GROUP,
    WAND_GROUP,
    CROP_GROUP,
    EYEDROPPER_GROUP,
];

const PIXEL_GROUPS: &[&[(ToolId, &str, &str)]] = &[
    HEAL_GROUP,
    PATCH_GROUP,
    BRUSH_GROUP,
    CLONE_GROUP,
    ERASER_GROUP,
    PIXEL_FILL_GROUP,
    SMUDGE_GROUP,
    DODGE_GROUP,
];

const VECTOR_GROUPS: &[&[(ToolId, &str, &str)]] = &[NODE_GROUP];
const NO_CONTEXT_GROUPS: &[&[(ToolId, &str, &str)]] = &[];

const COMMON_BOTTOM_GROUPS: &[&[(ToolId, &str, &str)]] = &[
    GRADIENT_GROUP,
    PEN_GROUP,
    VECTOR_BRUSH_GROUP,
    ARROW_GROUP,
    TEXT_GROUP,
    SHAPE_GROUP,
    HAND_GROUP,
    ZOOM_GROUP,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ToolbarContext {
    Pixel,
    Vector,
    Neutral,
}

const TOOLBOX_TOOL_W: f32 = 36.0;
const TOOLBOX_TOOL_H: f32 = 30.0;
const TOOLBAR_ICON_SIZE: f32 = 19.0;

pub fn build(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let pal = data.chrome.theme_mode.palette();
    let context = toolbar_context(data);
    let background_locked = active_background_locked(data);
    let active_visible = tool_visible_in_context(context, data.tool.active_tool);

    // A layer-context change must never leave a hidden destructive tool active.
    // Modal sessions own their tool until commit/cancel, so defer the fallback.
    let active_blocked_by_background =
        background_locked && tool_writes_layer(data.tool.active_tool);
    if (!active_visible || active_blocked_by_background) && !data.chrome.is_tool_modal {
        actions.tool.select_tool = Some(ToolId::Move);
    }

    #[allow(deprecated)]
    egui::SidePanel::left("toolbar")
        .exact_size(data.chrome.toolbar_w)
        .resizable(false)
        .frame(egui::Frame::new().fill(pal.toolbar_bg))
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(4.0);
                let swatch_h = 48.0;
                let tools_h = (ui.available_height() - swatch_h).max(80.0);
                egui::ScrollArea::vertical()
                    .id_salt("fixed_context_toolbar")
                    .max_height(tools_h)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            for group in visible_groups(context) {
                                let locked = background_locked && tool_writes_active_layer(group);
                                contextual_group_btn(ui, data, actions, group, locked);
                                ui.add_space(2.0);
                            }
                        });
                    });
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                    ui.add_space(4.0);
                    color_swatches(ui, data, actions);
                });
            });
        });
}

fn toolbar_context(data: &UiData) -> ToolbarContext {
    context_for_layer_type(
        data.layers
            .layer_types
            .get(data.layers.active_layer_idx)
            .map(String::as_str),
    )
}

fn context_for_layer_type(layer_type: Option<&str>) -> ToolbarContext {
    match layer_type {
        Some("Raster") => ToolbarContext::Pixel,
        Some("Shape" | "Path") => ToolbarContext::Vector,
        _ => ToolbarContext::Neutral,
    }
}

fn active_background_locked(data: &UiData) -> bool {
    let idx = data.layers.active_layer_idx;
    data.layers
        .layer_is_background
        .get(idx)
        .copied()
        .unwrap_or(false)
        && data.layers.layer_locked.get(idx).copied().unwrap_or(false)
}

fn visible_groups(
    context: ToolbarContext,
) -> impl Iterator<Item = &'static [(ToolId, &'static str, &'static str)]> {
    let contextual = match context {
        ToolbarContext::Pixel => PIXEL_GROUPS,
        ToolbarContext::Vector => VECTOR_GROUPS,
        ToolbarContext::Neutral => NO_CONTEXT_GROUPS,
    };
    COMMON_TOP_GROUPS
        .iter()
        .chain(contextual)
        .chain(COMMON_BOTTOM_GROUPS)
        .copied()
}

fn tool_writes_active_layer(entries: &[(ToolId, &str, &str)]) -> bool {
    entries.iter().any(|&(id, _, _)| tool_writes_layer(id))
}

fn tool_writes_layer(id: ToolId) -> bool {
    matches!(
        id,
        ToolId::Move
            | ToolId::Brush
            | ToolId::Pencil
            | ToolId::Eraser
            | ToolId::Fill
            | ToolId::Gradient
            | ToolId::Clone
            | ToolId::Repair
            | ToolId::Patch
            | ToolId::Smudge
            | ToolId::Dodge
            | ToolId::Burn
    )
}

fn tool_visible_in_context(context: ToolbarContext, tool: ToolId) -> bool {
    visible_groups(context).any(|group| group.iter().any(|&(id, _, _)| id == tool))
}

fn contextual_group_btn(
    ui: &mut egui::Ui,
    data: &UiData,
    actions: &mut UiActions,
    entries: &[(ToolId, &str, &str)],
    background_locked: bool,
) {
    if background_locked {
        ui.add_enabled_ui(false, |ui| group_btn(ui, data, actions, entries))
            .response
            .on_disabled_hover_text("Background đang khóa — mở khóa để chỉnh sửa");
    } else {
        group_btn(ui, data, actions, entries);
    }
}

#[cfg(any())]
mod legacy_toolbox {
    use super::*;

    fn toolbox_toggle_btn(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
        let pal = data.chrome.theme_mode.palette();
        let mut btn =
            egui::Button::new(egui::RichText::new(ph::TOOLBOX).size(21.0).color(pal.icon))
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
        if let Some(group) = ALL_TOOL_GROUPS
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
                                        for (i, group) in
                                            ALL_TOOL_GROUPS.iter().copied().enumerate()
                                        {
                                            group_btn(ui, data, actions, group);
                                            if (i + 1) % columns == 0 {
                                                ui.end_row();
                                            }
                                        }
                                        if !ALL_TOOL_GROUPS.len().is_multiple_of(columns) {
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
                        .size(TOOLBAR_HEADER_ICON_SIZE)
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
            egui::FontId::proportional(TOOLBAR_HEADER_ICON_SIZE),
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
                        .size(TOOLBAR_HEADER_ICON_SIZE)
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
        let close = egui::Button::new(
            egui::RichText::new(ph::X)
                .size(TOOLBAR_HEADER_ICON_SIZE)
                .color(icon_color),
        )
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
            return (data.chrome.toolbar_w - f32::from(toolbox_margin(true)) * 2.0)
                .max(TOOLBOX_TOOL_W);
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
        ALL_TOOL_GROUPS.len().div_ceil(columns)
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
            .size(TOOLBAR_ICON_SIZE)
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
                let mut label = egui::text::LayoutJob::default();
                label.append(
                    entry_icon,
                    0.0,
                    egui::TextFormat {
                        font_id: egui::FontId::proportional(TOOLBAR_ICON_SIZE),
                        color: ui.visuals().text_color(),
                        ..Default::default()
                    },
                );
                label.append(
                    &format!("  {entry_name}"),
                    0.0,
                    egui::TextFormat {
                        font_id: egui::TextStyle::Button.resolve(ui.style()),
                        color: ui.visuals().text_color(),
                        valign: egui::Align::Center,
                        ..Default::default()
                    },
                );
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
        egui::FontId::proportional(11.0),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_types_map_to_expected_toolbar_context() {
        assert_eq!(
            context_for_layer_type(Some("Raster")),
            ToolbarContext::Pixel
        );
        assert_eq!(
            context_for_layer_type(Some("Shape")),
            ToolbarContext::Vector
        );
        assert_eq!(context_for_layer_type(Some("Path")), ToolbarContext::Vector);
        assert_eq!(
            context_for_layer_type(Some("Text")),
            ToolbarContext::Neutral
        );
        assert_eq!(context_for_layer_type(None), ToolbarContext::Neutral);
    }

    #[test]
    fn shared_tools_stay_visible_across_contexts() {
        for context in [
            ToolbarContext::Pixel,
            ToolbarContext::Vector,
            ToolbarContext::Neutral,
        ] {
            assert!(tool_visible_in_context(context, ToolId::Move));
            assert!(tool_visible_in_context(context, ToolId::Eyedropper));
            assert!(tool_visible_in_context(context, ToolId::Hand));
            assert!(tool_visible_in_context(context, ToolId::Zoom));
        }
    }

    #[test]
    fn pixel_and_vector_specific_tools_do_not_leak() {
        assert!(tool_visible_in_context(
            ToolbarContext::Pixel,
            ToolId::Brush
        ));
        assert!(!tool_visible_in_context(
            ToolbarContext::Pixel,
            ToolId::Node
        ));
        assert!(tool_visible_in_context(
            ToolbarContext::Vector,
            ToolId::Node
        ));
        assert!(!tool_visible_in_context(
            ToolbarContext::Vector,
            ToolId::Brush
        ));
        assert!(!tool_visible_in_context(
            ToolbarContext::Neutral,
            ToolId::Node
        ));
    }

    #[test]
    fn creation_tools_are_available_on_a_new_raster_document() {
        for tool in [
            ToolId::Pen,
            ToolId::Shape,
            ToolId::VectorBrush,
            ToolId::Arrow,
            ToolId::Text,
            ToolId::Gradient,
        ] {
            assert!(tool_visible_in_context(ToolbarContext::Pixel, tool));
            assert!(tool_visible_in_context(ToolbarContext::Neutral, tool));
        }
    }
}

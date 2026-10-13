use super::{UiActions, UiData};
use crate::core::warp::WarpMode;
use crate::ui::widgets::dev_slider;

const PANEL_W: f32 = 248.0;

/// Floating Warp panel + on-canvas overlays (brush ring, freeze mask). Reads the
/// live `warp_params` snapshot and emits actions; the warp happens on the canvas
/// via pointer input handled in `app/input.rs`.
pub fn build(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    if !data.dialogs.show_warp_dialog {
        return;
    }

    let screen = ctx.content_rect();
    let pos_x =
        (screen.max.x - data.chrome.panel_r_w - PANEL_W - 12.0).max(data.chrome.toolbar_w + 36.0);
    let mut open = true;
    let mut params = data.dialogs.warp_params;
    let mut changed = false;
    let mut apply = false;
    let mut cancel = false;
    let mut restore = false;

    egui::Window::new("Warp")
        .id(egui::Id::new("warp_dialog"))
        .open(&mut open)
        .default_pos(egui::pos2(pos_x, 96.0))
        .default_width(PANEL_W)
        .resizable(false)
        .collapsible(false)
        .show(ctx, |ui| {
            ui.set_min_width(PANEL_W - 26.0);

            ui.label("Tool");
            egui::Grid::new("warp_tools")
                .num_columns(2)
                .spacing([6.0, 6.0])
                .show(ui, |ui| {
                    for (i, mode) in WarpMode::ALL.iter().enumerate() {
                        let selected = params.mode == *mode;
                        if ui
                            .add_sized(
                                [104.0, 22.0],
                                egui::Button::selectable(selected, mode.label()),
                            )
                            .clicked()
                            && !selected
                        {
                            params.mode = *mode;
                            changed = true;
                        }
                        if i % 2 == 1 {
                            ui.end_row();
                        }
                    }
                });

            ui.add_space(8.0);
            ui.label("Brush");
            changed |= dev_slider(ui, "Size", &mut params.size, 10.0..=1000.0);
            changed |= dev_slider(ui, "Pressure", &mut params.pressure, 0.05..=1.0);

            ui.add_space(6.0);
            let hint = if params.mode.is_mask() {
                "Paint to protect (Freeze) or release (Thaw) areas."
            } else {
                "Drag on the image to warp.  Enter = apply · Esc = cancel"
            };
            ui.label(egui::RichText::new(hint).weak().small());

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Restore All").clicked() {
                    restore = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("OK").clicked() {
                        apply = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        });

    draw_freeze_overlay(ctx, data);
    draw_brush_ring(ctx, data, params);

    if changed {
        actions.dialogs.set_warp_params = Some(params);
    }
    if restore {
        actions.dialogs.warp_restore_all = true;
    }
    if apply {
        actions.dialogs.apply_warp_dialog = true;
    } else if cancel || !open {
        actions.dialogs.cancel_warp_dialog = true;
    }
}

/// Translucent red mask over frozen areas (standard), scaled from the mesh
/// node grid onto the layer's canvas rect.
fn draw_freeze_overlay(ctx: &egui::Context, data: &UiData) {
    let Some(view) = data.dialogs.warp_freeze.as_ref() else {
        return;
    };
    if view.gw == 0 || view.gh == 0 {
        return;
    }
    // Semi-transparent warm red where frozen.
    let pixels: Vec<egui::Color32> = view
        .alpha
        .iter()
        .map(|&a| egui::Color32::from_rgba_unmultiplied(220, 40, 40, (a as u16 * 120 / 255) as u8))
        .collect();
    let img = egui::ColorImage::new([view.gw, view.gh], pixels);
    let tex = ctx.load_texture("warp_freeze", img, egui::TextureOptions::LINEAR);
    let min = egui::pos2(
        data.doc.offset_x + view.layer_x * data.doc.zoom,
        data.doc.offset_y + view.layer_y * data.doc.zoom,
    );
    let rect = egui::Rect::from_min_size(
        min,
        egui::vec2(view.layer_w * data.doc.zoom, view.layer_h * data.doc.zoom),
    );
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Middle,
        egui::Id::new("warp_freeze_overlay"),
    ));
    painter.image(
        tex.id(),
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
}

/// Brush-size ring around the cursor (red for the Freeze/Thaw mask brushes).
fn draw_brush_ring(ctx: &egui::Context, data: &UiData, params: crate::core::warp::WarpParams) {
    if !ctx.input(|i| i.focused) || ctx.pointer_hover_pos().is_none() || ctx.is_pointer_over_egui()
    {
        return;
    }
    // While resizing (Alt+right drag) the cursor is moving but the ring must stay
    // pinned at the press point — like the Brush/Eraser tools.
    let pos = if data.dialogs.warp_resizing {
        egui::pos2(
            data.dialogs.warp_resize_anchor.0,
            data.dialogs.warp_resize_anchor.1,
        )
    } else {
        let Some(p) = ctx.pointer_latest_pos() else {
            return;
        };
        p
    };
    let r = (params.size * 0.5 * data.doc.zoom).max(2.0);
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("warp_brush_ring"),
    ));
    let ring = if params.mode.is_mask() {
        egui::Color32::from_rgb(230, 70, 70)
    } else {
        egui::Color32::from_rgb(240, 240, 240)
    };
    // Black halo under a light ring keeps it visible on any background.
    painter.circle_stroke(
        pos,
        r + 1.0,
        egui::Stroke::new(2.0_f32, egui::Color32::from_black_alpha(140)),
    );
    painter.circle_stroke(pos, r, egui::Stroke::new(1.0_f32, ring));
    painter.circle_filled(pos, 1.5, ring);
}

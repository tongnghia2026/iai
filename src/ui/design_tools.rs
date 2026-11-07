//! Two small windows for the designer's tools that need numbers: Contour
//! (nới / thu viền) and text on a path (where the text stands, how high,
//! which side). Their state lives in egui's memory — they only send engine
//! commands, so they add no field to `UiActions`.

use super::intent::UiActions;
use super::viewmodel::UiData;

#[derive(Clone)]
struct ContourForm {
    open: bool,
    distance: f32,
    steps: u32,
    inward: bool,
    colors: [[u8; 3]; 2],
    two_colours: bool,
    round: bool,
}

impl Default for ContourForm {
    fn default() -> Self {
        Self {
            open: false,
            distance: 6.0,
            steps: 1,
            inward: false,
            colors: [[255, 255, 255], [0, 0, 0]],
            two_colours: false,
            round: true,
        }
    }
}

#[derive(Clone)]
struct PathForm {
    open: bool,
    start: f32,
    offset: f32,
    flip: bool,
}

impl Default for PathForm {
    fn default() -> Self {
        Self {
            open: false,
            start: 0.75,
            offset: 0.0,
            flip: false,
        }
    }
}

fn contour_id() -> egui::Id {
    egui::Id::new("design_tools_contour")
}

fn path_id() -> egui::Id {
    egui::Id::new("design_tools_text_path")
}

/// Open the Contour window (from the Object menu).
pub fn open_contour(ctx: &egui::Context) {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<ContourForm>(contour_id()).open = true);
}

/// Open the text-on-a-path window (from the Text menu).
pub fn open_text_path(ctx: &egui::Context) {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<PathForm>(path_id()).open = true);
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

/// Draw whichever of the windows is open.
pub fn show(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let mut form = ctx.data(|d| d.get_temp::<ContourForm>(contour_id()).unwrap_or_default());
    if form.open {
        let mut open = true;
        let mut apply = false;
        egui::Window::new("Nới / thu viền (Contour)")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.label("Chọn hình hoặc lớp chữ trước.");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut form.inward, false, "Ra ngoài");
                    ui.radio_value(&mut form.inward, true, "Vào trong");
                });
                ui.horizontal(|ui| {
                    ui.label("Khoảng cách (px)");
                    ui.add(
                        egui::DragValue::new(&mut form.distance)
                            .range(0.5..=500.0)
                            .speed(0.5),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Số lớp");
                    ui.add(egui::DragValue::new(&mut form.steps).range(1..=50));
                });
                ui.horizontal(|ui| {
                    ui.label("Màu");
                    ui.color_edit_button_srgb(&mut form.colors[0]);
                    ui.checkbox(&mut form.two_colours, "xen màu");
                    if form.two_colours {
                        ui.color_edit_button_srgb(&mut form.colors[1]);
                    }
                });
                ui.checkbox(&mut form.round, "Góc tròn");
                ui.add_space(4.0);
                apply = ui.button("Áp dụng").clicked();
            });
        if apply {
            let mut colors = vec![hex(form.colors[0])];
            if form.two_colours {
                colors.push(hex(form.colors[1]));
            }
            let d = if form.inward {
                -form.distance
            } else {
                form.distance
            };
            actions.run(
                "vector.contour",
                serde_json::json!({
                    "distance": d,
                    "steps": form.steps,
                    "colors": colors,
                    "corners": if form.round { "round" } else { "sharp" },
                }),
            );
        }
        form.open = open;
        ctx.data_mut(|d| d.insert_temp(contour_id(), form));
    }

    let mut form = ctx.data(|d| d.get_temp::<PathForm>(path_id()).unwrap_or_default());
    if form.open {
        let is_text = data
            .layers
            .layer_types
            .get(data.layers.active_layer_idx)
            .is_some_and(|t| t == "Text");
        let mut open = true;
        let mut apply = false;
        egui::Window::new("Chữ theo đường")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                if !is_text {
                    ui.label("Chọn lớp chữ đang nằm theo đường.");
                }
                ui.horizontal(|ui| {
                    ui.label("Vị trí trên đường");
                    apply |= ui
                        .add(egui::Slider::new(&mut form.start, 0.0..=1.0).show_value(false))
                        .drag_stopped();
                });
                ui.small("Vòng tròn: 0,25 đáy · 0,5 trái · 0,75 đỉnh");
                ui.horizontal(|ui| {
                    ui.label("Nâng chữ (px)");
                    apply |= ui
                        .add(
                            egui::DragValue::new(&mut form.offset)
                                .range(-500.0..=500.0)
                                .speed(0.5),
                        )
                        .drag_stopped();
                });
                apply |= ui.checkbox(&mut form.flip, "Lật sang phía kia").changed();
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    apply |= ui
                        .add_enabled(is_text, egui::Button::new("Áp dụng"))
                        .clicked();
                });
            });
        if apply && is_text {
            actions.run(
                "text.fit_to_path",
                serde_json::json!({
                    "start": form.start,
                    "offset": form.offset,
                    "flip": form.flip,
                }),
            );
        }
        form.open = open;
        ctx.data_mut(|d| d.insert_temp(path_id(), form));
    }
}

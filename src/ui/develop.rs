use super::{UiActions, UiData};
use crate::core::color::{hsl_to_rgb, rgb_to_hsl};
use crate::core::develop::{
    DevelopMixerMode, DevelopSettings, LocalMaskKind, LocalMaskShape, CONTROL_LIMIT,
    EXPOSURE_LIMIT, MIXER_COLORS, MIXER_LABELS,
};
use egui_phosphor::regular as ph;

const PANEL_W: f32 = 326.0;
const MIN_STABLE_VIEWPORT_H: f32 = 320.0;

// ── Panel sections (D4) ──────────────────────────────────────────────────────
// Collapse state is persisted by panel-order index.

const SEC_PRESETS: usize = 0;
const SEC_LIGHT: usize = 1;
const SEC_COLOR: usize = 2;
const SEC_DETAIL: usize = 3;
const SEC_EFFECTS: usize = 4;
const SEC_CURVE: usize = 5;
const SEC_MIXER: usize = 6;
const SEC_LOCALS: usize = 7;
const SEC_SCOPES: usize = 8;
pub const DEV_PANEL_SECTIONS: usize = 9;

/// The pre-D4 `default_open` flags, used until the user first toggles a header.
pub const DEFAULT_SECTIONS_OPEN: [bool; DEV_PANEL_SECTIONS] =
    [false, true, false, false, false, false, true, false, true];

/// Saved open/closed state of the panel sections (prefs.json
/// `develop_sections_open`); missing/short entries fall back to the defaults.
pub fn load_sections_open() -> [bool; DEV_PANEL_SECTIONS] {
    let mut open = DEFAULT_SECTIONS_OPEN;
    let saved = std::fs::read_to_string(super::theme::prefs_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| {
            v.get("develop_sections_open")
                .and_then(|a| serde_json::from_value::<Vec<bool>>(a.clone()).ok())
        });
    if let Some(saved) = saved {
        for (slot, v) in open.iter_mut().zip(saved) {
            *slot = v;
        }
    }
    open
}

/// Persist the section collapse state (merge-into-object, like the theme).
pub fn save_sections_open(open: &[bool; DEV_PANEL_SECTIONS]) {
    let path = super::theme::prefs_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
    if !value.is_object() {
        value = serde_json::Value::Object(Default::default());
    }
    if let Some(map) = value.as_object_mut() {
        if let Ok(v) = serde_json::to_value(open.to_vec()) {
            map.insert("develop_sections_open".to_string(), v);
        }
    }
    if let Ok(json) = serde_json::to_string_pretty(&value) {
        let _ = std::fs::write(&path, json);
    }
}

pub fn build(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    if !data.develop.show_develop_dialog {
        return;
    }
    let screen = ctx.content_rect();
    if !stable_viewport_for_panel(screen) {
        return;
    }
    let pos_x =
        (screen.max.x - data.chrome.panel_r_w - PANEL_W - 12.0).max(data.chrome.toolbar_w + 36.0);
    let mut open = true;
    let esc_pressed = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    let enter_pressed = if ctx.egui_wants_keyboard_input() {
        false
    } else {
        ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
    };
    let mut apply = enter_pressed;
    let mut cancel = esc_pressed;

    // One Lightroom-style Develop feature whether opened as the RAW pre-editor
    // (develop_mode) or as a raster-layer adjustment.
    egui::Window::new("Develop")
        .id(egui::Id::new("develop_dialog"))
        .open(&mut open)
        .default_pos(egui::pos2(pos_x, 96.0))
        .default_width(PANEL_W)
        .resizable(false)
        .collapsible(false)
        .show(ctx, |ui| {
            ui.set_min_width(PANEL_W - 26.0);
            let max_h = (screen.height() - 190.0).max(260.0);
            let (a, c) = develop_panel_contents(ui, data, actions, max_h);
            apply |= a;
            cancel |= c;
        });
    if apply {
        actions.develop.apply_develop_dialog = true;
    } else if cancel || !open {
        actions.develop.cancel_develop_dialog = true;
    }
}

/// Build the Develop control sections + action row into `ui`, cloning the
/// current settings and pushing any change through `actions.develop.set_develop_settings`.
/// Shared by the in-canvas dialog (`build`) and the Develop window (D2). Returns
/// `(commit_clicked, cancel_clicked)` from its Open Image / Cancel buttons.
pub(crate) fn develop_panel_contents(
    ui: &mut egui::Ui,
    data: &UiData,
    actions: &mut UiActions,
    max_scroll_h: f32,
) -> (bool, bool) {
    // True only while the primary button is actively driving an egui control
    // (e.g. a slider drag). `is_using_pointer()` is false for clicks on the
    // canvas image — it renders on its own GPU surface, not as an egui widget —
    // so a plain click there no longer counts. Previously this was the GLOBAL
    // `primary_down()`, so ANY mouse press anywhere flipped the RAW colour
    // preview between the fast chroma proxy (down) and the exact per-pixel
    // shader (up), flashing the image brighter on every click once the Colour
    // Mixer was engaged.
    actions.develop.develop_controls_pointer_down =
        ui.input(|input| input.pointer.primary_down()) && ui.ctx().egui_is_using_pointer();
    let mut settings = data.develop.develop_settings.clone();
    let mut changed = false;
    let mut apply = false;
    let mut cancel = false;
    ui.spacing_mut().slider_width = 142.0;

    ui.horizontal(|ui| {
        ui.label("Preview");
        let (text, color) = if data.develop.develop_preview_settled {
            ("Full quality", egui::Color32::from_rgb(74, 180, 110))
        } else if data.develop.develop_preview_refining {
            ("Refining…", egui::Color32::from_rgb(220, 165, 65))
        } else {
            ("Interactive", egui::Color32::from_rgb(95, 155, 220))
        };
        ui.colored_label(color, text);
    });
    let engine_color = if settings.develop_engine_version
        == crate::core::develop::DevelopEngineVersion::Develop3
    {
        egui::Color32::from_rgb(104, 190, 132)
    } else {
        egui::Color32::from_gray(150)
    };
    let source_kind = if data.develop.develop_mode {
        "RAW"
    } else {
        "Raster"
    };
    let engine_badge = ui.colored_label(
        engine_color,
        egui::RichText::new(format!(
            "Engine: {} · {source_kind}",
            settings.develop_engine_version.label()
        ))
        .monospace()
        .size(10.5),
    );
    let mut engine_tooltip = format!(
        "Renderer: {}\nSession: {source_kind}",
        settings.develop_engine_version.label()
    );
    if let Some(diagnostic) = data.develop.develop_pipeline_diagnostic.as_deref() {
        engine_tooltip.push('\n');
        engine_tooltip.push_str(diagnostic);
    }
    engine_badge.on_hover_text(engine_tooltip);

    // ── D4 header: RGB histogram + cursor readout + EXIF + Auto/B&W ─────────
    let header_top = ui.cursor().top();
    if let Some(hist) = data.develop.develop_histogram.as_deref() {
        histogram_overlay(ui, hist);
        let readout = match data.develop.develop_readout {
            Some([r, g, b]) => format!("R {r:>3}   G {g:>3}   B {b:>3}"),
            None => "R  ---   G  ---   B  ---".to_string(),
        };
        ui.label(egui::RichText::new(readout).monospace().size(11.0));
        if let Some(exif) = &data.develop.develop_exif {
            ui.label(
                egui::RichText::new(exif)
                    .size(11.0)
                    .color(egui::Color32::GRAY),
            );
        }
        ui.add_space(2.0);
    }
    // Auto fits Exposure to a fixed target brightness (scene sessions); B&W
    // toggles Saturation −100.
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                data.develop.develop_auto_available,
                egui::Button::new("Auto"),
            )
            .on_hover_text("Auto exposure")
            .clicked()
        {
            actions.develop.develop_auto = true;
        }
        let bw = settings.saturation <= -99.5;
        if ui
            .selectable_label(bw, "B&W")
            .on_hover_text("Black & white (Saturation −100)")
            .clicked()
        {
            settings.saturation = if bw { 0.0 } else { -100.0 };
            changed = true;
        }
    });
    ui.add_space(4.0);
    // The header eats into the fixed budget the caller sized for the sections.
    let max_scroll_h = (max_scroll_h - (ui.cursor().top() - header_top)).max(160.0);

    egui::ScrollArea::vertical()
        .max_height(max_scroll_h)
        .show(ui, |ui| {
            let out = section(ui, data, SEC_SCOPES, "Scopes", |ui| {
                scopes_ui(
                    ui,
                    data.develop.develop_scopes.as_ref(),
                    data.develop.develop_scopes_revision,
                    data.develop.develop_scope_visibility,
                    actions,
                );
                ui.separator();
                proof_controls_ui(ui, data, actions);
            });
            note_section(out, SEC_SCOPES, actions);

            let out = section(ui, data, SEC_PRESETS, "Presets", |ui| {
                ui.horizontal(|ui| {
                    let mut apply_idx: Option<usize> = None;
                    let mut del_idx: Option<usize> = None;
                    egui::ComboBox::from_id_salt("develop_preset_select")
                        .selected_text("Saved presets…")
                        .width(170.0)
                        .show_ui(ui, |ui| {
                            if data.develop.develop_presets.is_empty() {
                                ui.label("No presets yet");
                            }
                            for (i, preset) in data.develop.develop_presets.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    if ui.selectable_label(false, &preset.name).clicked() {
                                        apply_idx = Some(i);
                                    }
                                    if ui
                                        .small_button(ph::X)
                                        .on_hover_text("Delete preset")
                                        .clicked()
                                    {
                                        del_idx = Some(i);
                                    }
                                });
                            }
                        });
                    if let Some(i) = apply_idx {
                        // The mixer tab is view state, not part of the look.
                        let mixer_mode = settings.mixer_mode;
                        settings = data.develop.develop_presets[i].settings.clone();
                        settings.mixer_mode = mixer_mode;
                        changed = true;
                    }
                    if let Some(i) = del_idx {
                        actions.develop.delete_develop_preset = Some(i);
                    }
                });
                ui.horizontal(|ui| {
                    let name_id = egui::Id::new("develop_preset_name");
                    let mut preset_name = ui
                        .ctx()
                        .data_mut(|d| d.get_temp::<String>(name_id).unwrap_or_default());
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut preset_name)
                            .desired_width(170.0)
                            .hint_text("Preset name…"),
                    );
                    if resp.changed() {
                        ui.ctx()
                            .data_mut(|d| d.insert_temp(name_id, preset_name.clone()));
                    }
                    if ui.button("Save").clicked() && !preset_name.trim().is_empty() {
                        actions.develop.save_develop_preset = Some(preset_name.trim().to_string());
                        ui.ctx().data_mut(|d| d.insert_temp(name_id, String::new()));
                    }
                });
            });
            note_section(out, SEC_PRESETS, actions);

            let out = section(ui, data, SEC_LIGHT, "Light", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Tone Mapping");
                    egui::ComboBox::from_id_salt("develop_tone_map_mode")
                        .selected_text(match settings.tone_map_mode {
                            crate::core::develop::ToneMapMode::Perceptual => "Perceptual",
                            crate::core::develop::ToneMapMode::FilmLike => "Film-like",
                            crate::core::develop::ToneMapMode::Neutral => "Neutral",
                        })
                        .show_ui(ui, |ui| {
                            changed |= ui
                                .selectable_value(
                                    &mut settings.tone_map_mode,
                                    crate::core::develop::ToneMapMode::Perceptual,
                                    "Perceptual — balanced skin and colour",
                                )
                                .changed();
                            changed |= ui
                                .selectable_value(
                                    &mut settings.tone_map_mode,
                                    crate::core::develop::ToneMapMode::FilmLike,
                                    "Film-like — soft colour shoulder",
                                )
                                .changed();
                            changed |= ui
                                .selectable_value(
                                    &mut settings.tone_map_mode,
                                    crate::core::develop::ToneMapMode::Neutral,
                                    "Neutral — minimum hue/chroma drift",
                                )
                                .changed();
                        });
                });
                changed |= slider_row_fine(
                    ui,
                    "Exposure",
                    &mut settings.exposure,
                    -EXPOSURE_LIMIT..=EXPOSURE_LIMIT,
                    crate::ui::widgets::EXPOSURE_POS_POWER,
                );
                changed |= slider_row(
                    ui,
                    "Contrast",
                    &mut settings.contrast,
                    -CONTROL_LIMIT..=CONTROL_LIMIT,
                );
                changed |= slider_row(
                    ui,
                    "Highlights",
                    &mut settings.highlights,
                    -CONTROL_LIMIT..=CONTROL_LIMIT,
                );
                changed |= slider_row(
                    ui,
                    "Shadows",
                    &mut settings.shadows,
                    -CONTROL_LIMIT..=CONTROL_LIMIT,
                );
                if settings.develop_engine_version
                    == crate::core::develop::DevelopEngineVersion::Develop3
                {
                    changed |= slider_row(
                        ui,
                        "Midtones",
                        &mut settings.midtones,
                        -CONTROL_LIMIT..=CONTROL_LIMIT,
                    );
                }
                changed |= slider_row(
                    ui,
                    "Whites",
                    &mut settings.whites,
                    -CONTROL_LIMIT..=CONTROL_LIMIT,
                );
                changed |= slider_row(
                    ui,
                    "Blacks",
                    &mut settings.blacks,
                    -CONTROL_LIMIT..=CONTROL_LIMIT,
                );
            });
            note_section(out, SEC_LIGHT, actions);

            let out = section(ui, data, SEC_COLOR, "Color", |ui| {
                changed |= slider_row(
                    ui,
                    "Temperature",
                    &mut settings.temperature,
                    -CONTROL_LIMIT..=CONTROL_LIMIT,
                );
                changed |= slider_row(
                    ui,
                    "Tint",
                    &mut settings.tint,
                    -CONTROL_LIMIT..=CONTROL_LIMIT,
                );
                changed |= slider_row(
                    ui,
                    "Vividness",
                    &mut settings.vibrance,
                    -CONTROL_LIMIT..=CONTROL_LIMIT,
                );
                changed |= slider_row(
                    ui,
                    "Saturation",
                    &mut settings.saturation,
                    -CONTROL_LIMIT..=CONTROL_LIMIT,
                );
                changed |= slider_row(
                    ui,
                    "Color Smoothing",
                    &mut settings.color_smoothing,
                    0.0..=100.0,
                );
                ui.separator();
                changed |= grade_row(
                    ui,
                    "Shadow grade",
                    &mut settings.grade_shadow_hue,
                    &mut settings.grade_shadow_strength,
                );
                changed |= grade_row(
                    ui,
                    "Highlight grade",
                    &mut settings.grade_highlight_hue,
                    &mut settings.grade_highlight_strength,
                );
            });
            note_section(out, SEC_COLOR, actions);

            let out = section(ui, data, SEC_DETAIL, "Detail", |ui| {
                changed |= slider_row(ui, "Sharpening", &mut settings.sharpening, 0.0..=100.0);
                changed |= slider_row(
                    ui,
                    "Sharpen Radius",
                    &mut settings.sharpen_radius,
                    0.5..=3.0,
                );
                changed |= slider_row(
                    ui,
                    "Sharpen Detail",
                    &mut settings.sharpen_detail,
                    0.0..=100.0,
                );
                changed |= slider_row(
                    ui,
                    "Sharpen Masking",
                    &mut settings.sharpen_masking,
                    0.0..=100.0,
                );
                changed |= slider_row(
                    ui,
                    "Noise Reduction",
                    &mut settings.noise_reduction,
                    0.0..=100.0,
                );
                changed |= slider_row(
                    ui,
                    "Color Noise Reduction",
                    &mut settings.color_noise_reduction,
                    0.0..=100.0,
                );
                // Defringe slider hidden pending a native fringe sample to tune
                // against (GUI acceptance failed). The `defringe` field, its
                // apply_defringe pass and its regression test remain in place so
                // it can be re-exposed once validated on a real crop.
                changed |= slider_row(
                    ui,
                    "Texture",
                    &mut settings.texture,
                    -CONTROL_LIMIT..=CONTROL_LIMIT,
                );
                changed |= slider_row(
                    ui,
                    "Definition",
                    &mut settings.clarity,
                    -CONTROL_LIMIT..=CONTROL_LIMIT,
                );
            });
            note_section(out, SEC_DETAIL, actions);

            let out = section(ui, data, SEC_EFFECTS, "Effects", |ui| {
                changed |= slider_row(
                    ui,
                    "Defog",
                    &mut settings.dehaze,
                    -CONTROL_LIMIT..=CONTROL_LIMIT,
                );
                changed |= slider_row(
                    ui,
                    "Vignette",
                    &mut settings.vignette,
                    -CONTROL_LIMIT..=CONTROL_LIMIT,
                );
            });
            note_section(out, SEC_EFFECTS, actions);

            let out = section(ui, data, SEC_CURVE, "Curve", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Master Curve");
                    changed |= ui
                        .selectable_value(
                            &mut settings.point_curve_mode,
                            crate::core::develop::PointCurveMode::Perceptual,
                            "Perceptual",
                        )
                        .changed();
                    changed |= ui
                        .selectable_value(
                            &mut settings.point_curve_mode,
                            crate::core::develop::PointCurveMode::Luminance,
                            "Luminance",
                        )
                        .changed();
                    ui.label("RGB tabs remain per-channel");
                });
                changed |=
                    curve_editor_ui(ui, &mut settings, data.develop.develop_histogram.as_deref());
                changed |= slider_row(
                    ui,
                    "Highlights",
                    &mut settings.curve_highlights,
                    -CONTROL_LIMIT..=CONTROL_LIMIT,
                );
                changed |= slider_row(
                    ui,
                    "Lights",
                    &mut settings.curve_lights,
                    -CONTROL_LIMIT..=CONTROL_LIMIT,
                );
                changed |= slider_row(
                    ui,
                    "Darks",
                    &mut settings.curve_darks,
                    -CONTROL_LIMIT..=CONTROL_LIMIT,
                );
                changed |= slider_row(
                    ui,
                    "Shadows",
                    &mut settings.curve_shadows,
                    -CONTROL_LIMIT..=CONTROL_LIMIT,
                );
            });
            note_section(out, SEC_CURVE, actions);

            let out = section(ui, data, SEC_MIXER, "Color Mixer", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Adjust");
                    egui::ComboBox::from_id_salt("develop_mixer_mode")
                        .selected_text("HSL")
                        .width(98.0)
                        .show_ui(ui, |ui| {
                            ui.label("HSL");
                        });
                });
                ui.horizontal(|ui| {
                    changed |= mixer_mode_tab(ui, &mut settings, DevelopMixerMode::Hue, "Hue");
                    changed |= mixer_mode_tab(
                        ui,
                        &mut settings,
                        DevelopMixerMode::Saturation,
                        "Saturation",
                    );
                    changed |=
                        mixer_mode_tab(ui, &mut settings, DevelopMixerMode::Luminance, "Luminance");
                    changed |= mixer_mode_tab(ui, &mut settings, DevelopMixerMode::All, "All");
                });
                ui.add_space(4.0);
                match settings.mixer_mode {
                    DevelopMixerMode::Hue => {
                        for i in 0..MIXER_LABELS.len() {
                            changed |= mixer_slider_row(
                                ui,
                                MIXER_LABELS[i],
                                &mut settings.mixer_hue[i],
                                MIXER_COLORS[i],
                                MixerSliderKind::Hue,
                            );
                        }
                    }
                    DevelopMixerMode::Saturation => {
                        for i in 0..MIXER_LABELS.len() {
                            changed |= mixer_slider_row(
                                ui,
                                MIXER_LABELS[i],
                                &mut settings.mixer_saturation[i],
                                MIXER_COLORS[i],
                                MixerSliderKind::Saturation,
                            );
                        }
                    }
                    DevelopMixerMode::Luminance => {
                        for i in 0..MIXER_LABELS.len() {
                            changed |= mixer_slider_row(
                                ui,
                                MIXER_LABELS[i],
                                &mut settings.mixer_luminance[i],
                                MIXER_COLORS[i],
                                MixerSliderKind::Luminance,
                            );
                        }
                    }
                    DevelopMixerMode::All => {
                        for i in 0..MIXER_LABELS.len() {
                            ui.add_space(2.0);
                            ui.label(egui::RichText::new(MIXER_LABELS[i]).strong());
                            changed |= mixer_slider_row(
                                ui,
                                "Hue",
                                &mut settings.mixer_hue[i],
                                MIXER_COLORS[i],
                                MixerSliderKind::Hue,
                            );
                            changed |= mixer_slider_row(
                                ui,
                                "Sat",
                                &mut settings.mixer_saturation[i],
                                MIXER_COLORS[i],
                                MixerSliderKind::Saturation,
                            );
                            changed |= mixer_slider_row(
                                ui,
                                "Lum",
                                &mut settings.mixer_luminance[i],
                                MIXER_COLORS[i],
                                MixerSliderKind::Luminance,
                            );
                        }
                    }
                }
            });
            note_section(out, SEC_MIXER, actions);

            let out = section(ui, data, SEC_LOCALS, "Local Masks", |ui| {
                local_masks_ui(ui, data, actions, &mut settings, &mut changed);
            });
            note_section(out, SEC_LOCALS, actions);
        });

    ui.add_space(6.0);
    let footer_w = (ui.clip_rect().right() - ui.cursor().left()).max(1.0);
    ui.allocate_ui_with_layout(
        egui::vec2(footer_w, 24.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            if ui
                .add_sized([52.0, 22.0], egui::Button::new("Reset"))
                .clicked()
            {
                settings = DevelopSettings::default();
                changed = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_sized([58.0, 22.0], egui::Button::new("Cancel"))
                    .clicked()
                {
                    cancel = true;
                }
                let commit_label = if data.develop.develop_mode {
                    "Open Image"
                } else {
                    "OK"
                };
                if ui
                    .add_sized([82.0, 22.0], egui::Button::new(commit_label))
                    .clicked()
                {
                    apply = true;
                }
            });
        },
    );
    if changed {
        actions.develop.set_develop_settings = Some(settings);
    }
    (apply, cancel)
}

fn stable_viewport_for_panel(screen: egui::Rect) -> bool {
    screen.width() >= PANEL_W + 80.0 && screen.height() >= MIN_STABLE_VIEWPORT_H
}

/// Camera-Raw-style RGB histogram: the three channel curves drawn as
/// translucent filled areas over a dark plot, so overlaps read as mixes
/// (R+G = yellow-ish, all three = grey). `hist` is `develop_histogram`
/// (R/G/B/Luma, each peak-normalised); sqrt scaling matches the curve
/// editor's backdrop.
fn histogram_overlay(ui: &mut egui::Ui, hist: &[[f32; 256]; 4]) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 84.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, egui::Color32::from_gray(24));

    let bins = 128usize;
    let bin_w = rect.width() / bins as f32;
    let fills = [
        egui::Color32::from_rgba_unmultiplied(225, 80, 80, 95),
        egui::Color32::from_rgba_unmultiplied(95, 205, 95, 95),
        egui::Color32::from_rgba_unmultiplied(95, 135, 235, 95),
    ];
    for (chan, fill) in fills.iter().enumerate() {
        let plane = &hist[chan];
        // Per-bin translucent bars (a histogram silhouette is concave, so a
        // single filled polygon is out); overlapping channels blend additively
        // enough to read as mixes.
        for bin in 0..bins {
            let v = 0.5 * (plane[bin * 2] + plane[bin * 2 + 1]);
            if v <= 0.002 {
                continue;
            }
            let bar_h = v.sqrt() * (rect.height() - 4.0);
            let x0 = rect.left() + bin as f32 * bin_w;
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(x0, rect.bottom() - bar_h),
                    egui::pos2(x0 + bin_w, rect.bottom()),
                ),
                0.0,
                *fill,
            );
        }
    }
    painter.rect_stroke(
        rect,
        3.0,
        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(52)),
        egui::StrokeKind::Inside,
    );
}

const SCOPE_WAVEFORM: u8 = 1 << 0;
const SCOPE_PARADE: u8 = 1 << 1;
const SCOPE_VECTOR: u8 = 1 << 2;
pub const DEFAULT_SCOPE_VISIBILITY: u8 = SCOPE_WAVEFORM;

fn scopes_ui(
    ui: &mut egui::Ui,
    scopes: Option<&std::sync::Arc<crate::core::develop2::scopes::DevelopScopes>>,
    revision: u64,
    mut visible: u8,
    actions: &mut UiActions,
) {
    let before = visible;
    ui.horizontal_wrapped(|ui| {
        scope_toggle(ui, &mut visible, SCOPE_WAVEFORM, "Waveform");
        scope_toggle(ui, &mut visible, SCOPE_PARADE, "RGB Parade");
        scope_toggle(ui, &mut visible, SCOPE_VECTOR, "Vectorscope");
    });
    if visible != before {
        actions.develop.set_develop_scope_visibility = Some(visible);
    }
    ui.label(
        egui::RichText::new("Display encoded sRGB · pre-monitor")
            .size(10.0)
            .color(egui::Color32::GRAY),
    );

    let Some(scopes) = scopes else {
        ui.label("Scopes are waiting for a Develop preview.");
        return;
    };
    if visible == 0 {
        return;
    }
    let textures = scope_textures(ui, scopes, revision);
    if visible & SCOPE_WAVEFORM != 0 {
        if let Some(texture) = textures.first() {
            draw_scope_texture(ui, "Waveform (luma)", texture, 92.0);
        }
    }
    if visible & SCOPE_PARADE != 0 {
        if let Some(texture) = textures.get(1) {
            draw_scope_texture(ui, "RGB Parade", texture, 92.0);
        }
    }
    if visible & SCOPE_VECTOR != 0 {
        if let Some(texture) = textures.get(2) {
            draw_scope_texture(ui, "Vectorscope (Rec.709)", texture, 150.0);
        }
    }
    ui.label(
        egui::RichText::new(format!("{} sampled pixels", scopes.sample_count))
            .size(10.0)
            .color(egui::Color32::GRAY),
    );
}

fn scope_toggle(ui: &mut egui::Ui, visible: &mut u8, bit: u8, label: &str) {
    let mut enabled = *visible & bit != 0;
    if ui.toggle_value(&mut enabled, label).changed() {
        if enabled {
            *visible |= bit;
        } else {
            *visible &= !bit;
        }
    }
}

fn scope_textures(
    ui: &mut egui::Ui,
    scopes: &std::sync::Arc<crate::core::develop2::scopes::DevelopScopes>,
    revision: u64,
) -> Vec<egui::TextureHandle> {
    let cache_id = ui.make_persistent_id("develop_scope_textures");
    let images = || {
        vec![
            waveform_scope_image(scopes),
            parade_scope_image(scopes),
            vectorscope_image(scopes),
        ]
    };
    ui.ctx()
        .data(|data| data.get_temp::<(u64, Vec<egui::TextureHandle>)>(cache_id))
        .map(|(cached_key, mut textures)| {
            if cached_key != revision || textures.len() != 3 {
                let images = images();
                if textures.len() == images.len() {
                    for (texture, image) in textures.iter_mut().zip(images) {
                        texture.set(image, egui::TextureOptions::NEAREST);
                    }
                } else {
                    textures = load_scope_textures(ui.ctx(), images);
                }
                ui.ctx()
                    .data_mut(|data| data.insert_temp(cache_id, (revision, textures.clone())));
            }
            textures
        })
        .unwrap_or_else(|| {
            let textures = load_scope_textures(ui.ctx(), images());
            ui.ctx()
                .data_mut(|data| data.insert_temp(cache_id, (revision, textures.clone())));
            textures
        })
}

fn load_scope_textures(
    ctx: &egui::Context,
    images: Vec<egui::ColorImage>,
) -> Vec<egui::TextureHandle> {
    images
        .into_iter()
        .enumerate()
        .map(|(index, image)| {
            ctx.load_texture(
                format!("develop_scope_{index}"),
                image,
                egui::TextureOptions::NEAREST,
            )
        })
        .collect()
}

fn draw_scope_texture(ui: &mut egui::Ui, label: &str, texture: &egui::TextureHandle, height: f32) {
    ui.label(egui::RichText::new(label).size(10.0));
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, egui::Color32::from_gray(18));
    let texture_size = texture.size_vec2();
    let scale = (rect.width() / texture_size.x)
        .min(rect.height() / texture_size.y)
        .max(0.0);
    let image_rect = egui::Rect::from_center_size(rect.center(), texture_size * scale);
    painter.image(
        texture.id(),
        image_rect,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
    painter.rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(52)),
        egui::StrokeKind::Inside,
    );
}

fn waveform_scope_image(scopes: &crate::core::develop2::scopes::DevelopScopes) -> egui::ColorImage {
    let width = scopes.horizontal_bins.max(1);
    let height = scopes.value_bins.max(1);
    let mut pixels = vec![egui::Color32::from_gray(18); width * height];
    let peak = scopes.waveform.iter().copied().max().unwrap_or(0);
    for x in 0..width {
        for value in 0..height {
            let level = scope_density(scopes.waveform_at(x, value), peak);
            if level > 0 {
                let row = height - 1 - value;
                pixels[row * width + x] = egui::Color32::from_rgb(level, level, level);
            }
        }
    }
    egui::ColorImage::new([width, height], pixels)
}

fn parade_scope_image(scopes: &crate::core::develop2::scopes::DevelopScopes) -> egui::ColorImage {
    let plane_width = scopes.horizontal_bins.max(1);
    let height = scopes.value_bins.max(1);
    let width = plane_width * 3;
    let mut pixels = vec![egui::Color32::from_gray(18); width * height];
    for channel in 0..3 {
        let peak = scopes.parade[channel].iter().copied().max().unwrap_or(0);
        for x in 0..plane_width {
            for value in 0..height {
                let level = scope_density(scopes.parade_at(channel, x, value), peak);
                if level == 0 {
                    continue;
                }
                let row = height - 1 - value;
                let index = row * width + channel * plane_width + x;
                let mut rgb = [38, 38, 38];
                rgb[channel] = level;
                pixels[index] = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
            }
        }
    }
    egui::ColorImage::new([width, height], pixels)
}

fn vectorscope_image(scopes: &crate::core::develop2::scopes::DevelopScopes) -> egui::ColorImage {
    let size = scopes.vectorscope_bins.max(1);
    let mut pixels = vec![egui::Color32::from_gray(18); size * size];
    let centre = size / 2;
    for i in 0..size {
        pixels[centre * size + i] = egui::Color32::from_gray(38);
        pixels[i * size + centre] = egui::Color32::from_gray(38);
    }
    let peak = scopes.vectorscope.iter().copied().max().unwrap_or(0);
    for cr in 0..size {
        for cb in 0..size {
            let level = scope_density(scopes.vectorscope_at(cb, cr), peak);
            if level > 0 {
                let base = pixels[cr * size + cb];
                pixels[cr * size + cb] = egui::Color32::from_rgb(
                    base.r().max(level / 3),
                    base.g().max(level),
                    base.b().max(level / 2),
                );
            }
        }
    }
    egui::ColorImage::new([size, size], pixels)
}

fn scope_density(count: u32, peak: u32) -> u8 {
    if count == 0 || peak == 0 {
        return 0;
    }
    let normalized = (count as f32).ln_1p() / (peak as f32).ln_1p();
    (64.0 + 191.0 * normalized.sqrt()).round().clamp(0.0, 255.0) as u8
}

fn proof_controls_ui(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    ui.label(egui::RichText::new("Soft Proof").strong());
    ui.horizontal(|ui| {
        let mut enabled = data.print.proof_enabled;
        if ui.checkbox(&mut enabled, "Proof colors").changed() {
            actions.print.toggle_proof_colors = true;
        }
        let mut warning = data.print.proof_gamut_warn;
        if ui.checkbox(&mut warning, "Gamut warning").changed() {
            actions.print.toggle_gamut_warning = true;
        }
    });
    ui.horizontal(|ui| {
        ui.label("Target");
        egui::ComboBox::from_id_salt("develop_proof_target")
            .selected_text(&data.print.proof_target_label)
            .width(150.0)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(data.print.proof_target_label == "sRGB", "sRGB")
                    .clicked()
                {
                    actions.print.set_proof_target = Some(crate::core::cms::ProofTarget::Srgb);
                }
                if ui
                    .selectable_label(
                        data.print.proof_target_label == "Adobe RGB (1998)",
                        "Adobe RGB (1998)",
                    )
                    .clicked()
                {
                    actions.print.set_proof_target = Some(crate::core::cms::ProofTarget::AdobeRgb);
                }
            });
        if ui.small_button("Load ICC…").clicked() {
            actions.print.load_proof_profile = true;
        }
    });
    let monitor = if data.print.display_cms_enabled {
        data.print.display_profile_name.as_str()
    } else {
        "system display transform off"
    };
    ui.label(
        egui::RichText::new(format!("View only · monitor: {monitor}"))
            .size(10.0)
            .color(egui::Color32::GRAY),
    );
}

/// Route a section header's collapse-state change into `UiActions` (the
/// contents closure may itself borrow `actions`, so `section` can't).
fn note_section(open_changed: Option<bool>, idx: usize, actions: &mut UiActions) {
    if let Some(open) = open_changed {
        actions.develop.set_develop_section_open = Some((idx, open));
    }
}

/// One collapsible panel section with a persisted open state (seeded from
/// prefs via `data.develop.develop_sections_open`). Returns the new open state when
/// the user toggled the header this frame. The right side of the header is a
/// reserved slot for future per-section controls — anything placed there must
/// keep clear of the scroll bar that overlays the panel's right edge.
fn section(
    ui: &mut egui::Ui,
    data: &UiData,
    idx: usize,
    title: &str,
    add_contents: impl FnOnce(&mut egui::Ui),
) -> Option<bool> {
    let open_pref = data.develop.develop_sections_open[idx];
    let id = ui.make_persistent_id(("develop_section", idx));
    let state =
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, open_pref);
    state
        .show_header(ui, |ui| {
            ui.label(egui::RichText::new(title).strong());
        })
        .body(|ui| {
            ui.add_space(2.0);
            add_contents(ui);
            ui.add_space(4.0);
        });
    let open_now = egui::collapsing_header::CollapsingState::load(ui.ctx(), id)
        .map_or(open_pref, |s| s.is_open());
    (open_now != open_pref).then_some(open_now)
}

fn slider_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
) -> bool {
    gradient_slider_row(ui, label, value, range, &tone_gradient(label), 1.0)
}

/// Like [`slider_row`] but with a centre-fine track ease (`pos_power > 1`), so
/// small drags near neutral are gentle. Used by Exposure, whose wide ±5 EV range
/// otherwise makes the narrow panel track jump brightness too fast.
fn slider_row_fine(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    pos_power: f32,
) -> bool {
    gradient_slider_row(ui, label, value, range, &tone_gradient(label), pos_power)
}

fn grade_row(ui: &mut egui::Ui, label: &str, hue: &mut f32, strength: &mut f32) -> bool {
    let (r, g, b) = hsl_to_rgb(hue.rem_euclid(360.0) / 360.0, 0.78, 0.52);
    let mut rgb = [
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    ];
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        if ui.color_edit_button_srgb(&mut rgb).changed() {
            let (h, _, _) = rgb_to_hsl(
                rgb[0] as f32 / 255.0,
                rgb[1] as f32 / 255.0,
                rgb[2] as f32 / 255.0,
            );
            *hue = h * 360.0;
            changed = true;
        }
    });
    changed | slider_row(ui, "Strength", strength, 0.0..=CONTROL_LIMIT)
}

/// The Local Masks section: arm a linear/radial placement (the next canvas
/// drag places it), list the masks, and edit the selected mask's sliders.
fn local_masks_ui(
    ui: &mut egui::Ui,
    data: &UiData,
    actions: &mut UiActions,
    settings: &mut DevelopSettings,
    changed: &mut bool,
) {
    ui.horizontal(|ui| {
        let lin = data.develop.develop_local_arm == Some(LocalMaskKind::Linear);
        let rad = data.develop.develop_local_arm == Some(LocalMaskKind::Radial);
        if ui.selectable_label(lin, "+ Linear").clicked() {
            if lin {
                actions.develop.disarm_develop_local = true;
            } else {
                actions.develop.arm_develop_local = Some((LocalMaskKind::Linear, None));
            }
        }
        if ui.selectable_label(rad, "+ Radial").clicked() {
            if rad {
                actions.develop.disarm_develop_local = true;
            } else {
                actions.develop.arm_develop_local = Some((LocalMaskKind::Radial, None));
            }
        }
    });
    if data.develop.develop_local_arm.is_some() {
        ui.label(
            egui::RichText::new("Drag on the image to place the mask (Esc cancels)")
                .size(11.0)
                .color(egui::Color32::GRAY),
        );
    }

    let mut delete_idx: Option<usize> = None;
    for i in 0..settings.locals.len() {
        let selected = data.develop.develop_local_selected == Some(i);
        ui.horizontal(|ui| {
            let name = match settings.locals[i].shape.kind() {
                LocalMaskKind::Linear => format!("Linear {}", i + 1),
                LocalMaskKind::Radial => format!("Radial {}", i + 1),
            };
            if ui.selectable_label(selected, name).clicked() {
                actions.develop.select_develop_local = Some(if selected { None } else { Some(i) });
            }
            if ui
                .small_button(ph::X)
                .on_hover_text("Delete mask")
                .clicked()
            {
                delete_idx = Some(i);
            }
        });
        if selected {
            let local = &mut settings.locals[i];
            *changed |= slider_row(
                ui,
                "Exposure",
                &mut local.settings.exposure,
                -EXPOSURE_LIMIT..=EXPOSURE_LIMIT,
            );
            *changed |= slider_row(
                ui,
                "Contrast",
                &mut local.settings.contrast,
                -CONTROL_LIMIT..=CONTROL_LIMIT,
            );
            *changed |= slider_row(
                ui,
                "Highlights",
                &mut local.settings.highlights,
                -CONTROL_LIMIT..=CONTROL_LIMIT,
            );
            *changed |= slider_row(
                ui,
                "Shadows",
                &mut local.settings.shadows,
                -CONTROL_LIMIT..=CONTROL_LIMIT,
            );
            *changed |= slider_row(
                ui,
                "Temperature",
                &mut local.settings.temperature,
                -CONTROL_LIMIT..=CONTROL_LIMIT,
            );
            *changed |= slider_row(
                ui,
                "Tint",
                &mut local.settings.tint,
                -CONTROL_LIMIT..=CONTROL_LIMIT,
            );
            *changed |= slider_row(
                ui,
                "Saturation",
                &mut local.settings.saturation,
                -CONTROL_LIMIT..=CONTROL_LIMIT,
            );
            if let LocalMaskShape::Radial {
                feather, invert, ..
            } = &mut local.shape
            {
                let mut feather_pct = *feather * 100.0;
                if slider_row(ui, "Feather", &mut feather_pct, 0.0..=100.0) {
                    *feather = feather_pct / 100.0;
                    *changed = true;
                }
                if ui.checkbox(invert, "Invert").changed() {
                    *changed = true;
                }
            }
            let kind = local.shape.kind();
            if ui.button("Re-place mask").clicked() {
                actions.develop.arm_develop_local = Some((kind, Some(i)));
            }
            ui.add_space(4.0);
        }
    }
    if let Some(i) = delete_idx {
        settings.locals.remove(i);
        *changed = true;
        actions.develop.select_develop_local = Some(None);
        actions.develop.disarm_develop_local = true;
    }
}

#[derive(Clone, Copy)]
enum MixerSliderKind {
    Hue,
    Saturation,
    Luminance,
}

fn mixer_mode_tab(
    ui: &mut egui::Ui,
    settings: &mut DevelopSettings,
    mode: DevelopMixerMode,
    label: &str,
) -> bool {
    let selected = settings.mixer_mode == mode;
    if ui.selectable_label(selected, label).clicked() && !selected {
        settings.mixer_mode = mode;
        true
    } else {
        false
    }
}

fn mixer_slider_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    color: [u8; 3],
    kind: MixerSliderKind,
) -> bool {
    gradient_slider_row(
        ui,
        label,
        value,
        -CONTROL_LIMIT..=CONTROL_LIMIT,
        &mixer_gradient(color, kind),
        1.0,
    )
}

fn mixer_gradient(color: [u8; 3], kind: MixerSliderKind) -> Vec<egui::Color32> {
    let base = egui::Color32::from_rgb(color[0], color[1], color[2]);
    match kind {
        MixerSliderKind::Hue => vec![rotate_color(base, -0.12), base, rotate_color(base, 0.12)],
        MixerSliderKind::Saturation => {
            let dark = crate::ui::widgets::mix_color(egui::Color32::from_gray(38), base, 0.35);
            let muted = crate::ui::widgets::mix_color(egui::Color32::from_gray(128), base, 0.45);
            vec![dark, muted, base]
        }
        MixerSliderKind::Luminance => vec![
            crate::ui::widgets::mix_color(egui::Color32::BLACK, base, 0.35),
            base,
            crate::ui::widgets::mix_color(base, egui::Color32::WHITE, 0.65),
        ],
    }
}

fn gradient_slider_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    colors: &[egui::Color32],
    pos_power: f32,
) -> bool {
    crate::ui::widgets::dev_slider_colored_stacked(ui, label, value, range, colors, pos_power)
}

/// Interactive point-curve editor: Luma/R/G/B channel tabs over a plot with
/// the source histogram as backdrop (per selected channel).
/// Drag a control point to move it (x clamped between its neighbours), press
/// on empty curve to add a point there, double-click a point to remove it
/// (endpoints stay). Returns true when the settings changed.
fn curve_editor_ui(
    ui: &mut egui::Ui,
    settings: &mut DevelopSettings,
    histogram: Option<&[[f32; 256]; 4]>,
) -> bool {
    use crate::core::develop::{eval_point_curve, identity_curve};

    let mut changed = false;
    let chan_id = ui.id().with("dev_curve_channel");
    let mut channel: u8 = ui.ctx().data_mut(|d| *d.get_temp_mut_or(chan_id, 0u8));

    ui.horizontal(|ui| {
        for (i, label) in ["Luma", "R", "G", "B"].iter().enumerate() {
            if ui.selectable_label(channel == i as u8, *label).clicked() {
                channel = i as u8;
                ui.ctx().data_mut(|d| d.insert_temp(chan_id, channel));
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("Reset").clicked() {
                *curve_points_mut(settings, channel) = identity_curve();
                changed = true;
            }
        });
    });

    let width = ui.available_width().clamp(150.0, 260.0);
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, 140.0), egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, egui::Color32::from_gray(26));

    // Histogram backdrop for the selected channel (Luma tab → luma histogram).
    if let Some(hist) = histogram {
        let plane = &hist[match channel {
            1 => 0,
            2 => 1,
            3 => 2,
            _ => 3,
        }];
        let fill = match channel {
            1 => egui::Color32::from_rgba_unmultiplied(200, 90, 90, 70),
            2 => egui::Color32::from_rgba_unmultiplied(100, 190, 100, 70),
            3 => egui::Color32::from_rgba_unmultiplied(100, 135, 215, 70),
            _ => egui::Color32::from_rgba_unmultiplied(170, 170, 170, 64),
        };
        let bins = 128usize;
        let bin_w = rect.width() / bins as f32;
        for bin in 0..bins {
            let v = 0.5 * (plane[bin * 2] + plane[bin * 2 + 1]);
            if v <= 0.002 {
                continue;
            }
            let bar_h = v.sqrt() * (rect.height() - 4.0);
            let x0 = rect.left() + bin as f32 * bin_w;
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(x0, rect.bottom() - bar_h),
                    egui::pos2(x0 + bin_w, rect.bottom()),
                ),
                0.0,
                fill,
            );
        }
    }

    for f in [0.25f32, 0.5, 0.75] {
        let x = rect.left() + f * rect.width();
        let y = rect.top() + f * rect.height();
        let grid = egui::Stroke::new(1.0_f32, egui::Color32::from_gray(42));
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            grid,
        );
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            grid,
        );
    }
    painter.line_segment(
        [rect.left_bottom(), rect.right_top()],
        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(58)),
    );

    let to_screen = |p: [f32; 2]| {
        egui::pos2(
            rect.left() + p[0] * rect.width(),
            rect.bottom() - p[1] * rect.height(),
        )
    };
    let from_screen = |pos: egui::Pos2| {
        [
            ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0),
            ((rect.bottom() - pos.y) / rect.height()).clamp(0.0, 1.0),
        ]
    };
    let curve_color = match channel {
        1 => egui::Color32::from_rgb(226, 96, 96),
        2 => egui::Color32::from_rgb(110, 206, 110),
        3 => egui::Color32::from_rgb(110, 148, 232),
        _ => egui::Color32::from_gray(222),
    };

    let points = curve_points_mut(settings, channel);

    // Interaction first, so the paint below shows this frame's state.
    let drag_id = ui.id().with(("dev_curve_drag", channel));
    if response.drag_started() {
        if let Some(pos) = response.interact_pointer_pos() {
            let near = nearest_curve_point(points, pos, &to_screen);
            let idx = match near {
                Some(i) => i,
                None => {
                    let p = from_screen(pos);
                    let at = [p[0], eval_point_curve(points, p[0])];
                    let idx = points
                        .iter()
                        .position(|q| q[0] > at[0])
                        .unwrap_or(points.len());
                    points.insert(idx, at);
                    changed = true;
                    idx
                }
            };
            ui.ctx().data_mut(|d| d.insert_temp(drag_id, Some(idx)));
        }
    }
    if response.dragged() {
        let dragging: Option<usize> = ui.ctx().data_mut(|d| *d.get_temp_mut_or(drag_id, None));
        if let (Some(idx), Some(pos)) = (dragging, response.interact_pointer_pos()) {
            if idx < points.len() {
                let p = from_screen(pos);
                let lo = if idx == 0 {
                    0.0
                } else {
                    points[idx - 1][0] + 0.004
                };
                let hi = if idx + 1 == points.len() {
                    1.0
                } else {
                    points[idx + 1][0] - 0.004
                };
                points[idx] = [p[0].clamp(lo, hi.max(lo)), p[1]];
                changed = true;
            }
        }
    }
    if response.drag_stopped() {
        ui.ctx()
            .data_mut(|d| d.insert_temp(drag_id, Option::<usize>::None));
    }
    if response.double_clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            if let Some(idx) = nearest_curve_point(points, pos, &to_screen) {
                if points.len() > 2 && idx > 0 && idx + 1 < points.len() {
                    points.remove(idx);
                    changed = true;
                }
            }
        }
    }

    let n = 65;
    let line: Vec<egui::Pos2> = (0..n)
        .map(|i| {
            let x = i as f32 / (n - 1) as f32;
            to_screen([x, eval_point_curve(points, x)])
        })
        .collect();
    painter.add(egui::Shape::line(
        line,
        egui::Stroke::new(1.5_f32, curve_color),
    ));
    for p in points.iter() {
        painter.circle_filled(to_screen(*p), 3.5, curve_color);
        painter.circle_stroke(
            to_screen(*p),
            3.5,
            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(20)),
        );
    }

    changed
}

fn curve_points_mut(settings: &mut DevelopSettings, channel: u8) -> &mut Vec<[f32; 2]> {
    match channel {
        1 => &mut settings.curve_points_r,
        2 => &mut settings.curve_points_g,
        3 => &mut settings.curve_points_b,
        _ => &mut settings.curve_points,
    }
}

fn nearest_curve_point(
    points: &[[f32; 2]],
    pos: egui::Pos2,
    to_screen: &impl Fn([f32; 2]) -> egui::Pos2,
) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, p) in points.iter().enumerate() {
        let d = to_screen(*p).distance(pos);
        if d <= 10.0 && best.map_or(true, |(_, bd)| d < bd) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

fn tone_gradient(label: &str) -> Vec<egui::Color32> {
    let black = egui::Color32::from_rgb(24, 24, 24);
    let mid = egui::Color32::from_rgb(118, 118, 118);
    let light = egui::Color32::from_rgb(235, 235, 235);
    match label {
        "Exposure" | "Highlights" | "Whites" => vec![black, mid, light],
        "Shadows" | "Blacks" | "Darks" | "Midtones" => {
            vec![egui::Color32::BLACK, mid, light]
        }
        "Contrast" | "Definition" | "Defog" => vec![
            egui::Color32::from_rgb(82, 82, 82),
            egui::Color32::from_rgb(154, 154, 154),
            egui::Color32::from_rgb(245, 245, 245),
        ],
        "Sharpening" | "Sharpen Radius" | "Sharpen Detail" | "Sharpen Masking" => vec![
            egui::Color32::from_rgb(50, 50, 50),
            egui::Color32::from_rgb(138, 138, 138),
            egui::Color32::from_rgb(250, 250, 250),
        ],
        "Noise Reduction" | "Color Noise Reduction" => vec![
            egui::Color32::from_rgb(46, 46, 46),
            egui::Color32::from_rgb(96, 126, 144),
            egui::Color32::from_rgb(184, 210, 218),
        ],
        // The green↔magenta axis the cleanup neutralises.
        "Defringe" => vec![
            egui::Color32::from_rgb(150, 70, 168),
            egui::Color32::from_rgb(120, 120, 120),
            egui::Color32::from_rgb(78, 168, 96),
        ],
        "Temperature" => vec![
            egui::Color32::from_rgb(73, 124, 218),
            mid,
            egui::Color32::from_rgb(229, 145, 64),
        ],
        "Tint" => vec![
            egui::Color32::from_rgb(76, 170, 94),
            mid,
            egui::Color32::from_rgb(198, 78, 160),
        ],
        "Vividness" | "Saturation" => vec![
            egui::Color32::from_gray(110),
            egui::Color32::from_rgb(88, 150, 205),
            egui::Color32::from_rgb(214, 78, 112),
        ],
        "Vignette" => vec![egui::Color32::BLACK, mid, light],
        _ => vec![black, mid, light],
    }
}

fn rotate_color(color: egui::Color32, delta: f32) -> egui::Color32 {
    let (h, s, l) = rgb_to_hsl(
        color.r() as f32 / 255.0,
        color.g() as f32 / 255.0,
        color.b() as f32 / 255.0,
    );
    let (r, g, b) = hsl_to_rgb((h + delta).rem_euclid(1.0), s, l);
    egui::Color32::from_rgb(
        (r.clamp(0.0, 1.0) * 255.0).round() as u8,
        (g.clamp(0.0, 1.0) * 255.0).round() as u8,
        (b.clamp(0.0, 1.0) * 255.0).round() as u8,
    )
}

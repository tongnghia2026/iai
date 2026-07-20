// AI image editing panel.
//
// The backend is still split into:
// - Gemini API: paid/key based, stable for production work.
// - Gemini Web bridge: uses the user's signed-in Gemini web session.
//
// The UI is intentionally provider-oriented so OpenAI or another model can be
// added later without redesigning the panel again.

use crate::core::ai::AiPanelState;

use super::{UiActions, UiData};

const PANEL_W: f32 = 386.0;
const MIN_PANEL_W: f32 = 340.0;
const MIN_PANEL_H: f32 = 220.0;
const MIN_STABLE_VIEWPORT_H: f32 = 320.0;
const TOP_CHROME_H: f32 = 28.0 + 26.0 + 32.0;
const STATUS_BAR_H: f32 = 22.0;

// ID-photo outfit colours: (Vietnamese label, English colour term for the prompt).
const SHIRT_COLORS: [(&str, &str); 5] = [
    ("Trắng", "white"),
    ("Xanh nhạt", "light blue"),
    ("Hồng nhạt", "light pink"),
    ("Xám", "light gray"),
    ("Đen", "black"),
];
const AODAI_COLORS: [(&str, &str); 6] = [
    ("Đỏ", "red"),
    ("Trắng", "white"),
    ("Hồng", "pink"),
    ("Xanh", "blue"),
    ("Vàng", "yellow"),
    ("Tím", "violet"),
];
const VEST_COLORS: [(&str, &str); 3] = [("Đen", "black"), ("Navy", "navy"), ("Xám", "light gray")];
const TIE_COLORS: [(&str, &str); 5] = [
    ("Đỏ đô", "burgundy"),
    ("Navy", "navy"),
    ("Đen", "black"),
    ("Xám", "charcoal gray"),
    ("Xanh dương", "medium blue"),
];

const RETOUCH_CHOICES: [(&str, &str); 4] = [
    (
        "Rất nhẹ",
        "Very light skin retouch: remove only tiny temporary blemishes and reduce minor redness. Keep pores, skin texture, face shape, facial features, expression and identity completely natural.",
    ),
    (
        "Tự nhiên",
        "Natural professional skin retouch: reduce acne, blemishes, uneven tone and shine while preserving pores, real skin texture, face shape, facial features, expression and identity.",
    ),
    (
        "Đẹp ảnh thẻ",
        "Clean ID photo skin retouch: make skin tone even and tidy for a formal portrait. Preserve natural texture, facial features, face shape, expression, identity, crop and head size.",
    ),
    (
        "Studio",
        "Polished studio portrait retouch: clean visible blemishes and smooth uneven skin tone without plastic skin. Preserve the real face geometry, facial features, expression, age and identity.",
    ),
];

const COLOR_CHOICES: [(&str, &str); 5] = [
    (
        "Tự nhiên",
        "Professionally color grade this photo with natural clean tones: correct white balance, remove color casts, improve contrast and color harmony without changing the person, face, crop or composition.",
    ),
    (
        "Da đẹp",
        "Correct color for pleasing natural skin tone: reduce color cast, keep realistic skin color, improve tonal balance. Do not change facial features, identity, crop, size or composition.",
    ),
    (
        "Ấm áp",
        "Apply a gentle warm portrait color grade: improve white balance, skin tone and contrast while keeping the person, face, crop, canvas size and composition unchanged.",
    ),
    (
        "Mát trong",
        "Apply a clean slightly cool color grade: neutralize color casts, improve clarity and contrast while keeping the person, face, crop, canvas size and composition unchanged.",
    ),
    (
        "In ấn",
        "Prepare the photo for printing: neutral color, clean contrast, balanced saturation and natural skin tone. Preserve all shapes, face, crop, canvas size and aspect ratio.",
    ),
];

const LIGHT_CHOICES: [(&str, &str); 5] = [
    (
        "Cân bằng",
        "Improve lighting naturally: balance exposure, recover shadows and highlights, keep the original light direction, face, pose, crop, canvas size and composition unchanged.",
    ),
    (
        "Studio mềm",
        "Improve lighting with soft studio-like portrait light on the subject: soften harsh shadows and improve facial illumination while preserving facial features, pose, crop and background perspective.",
    ),
    (
        "Ảnh thẻ",
        "Improve lighting for a clean ID photo look: even frontal illumination, neutral exposure and natural skin tone. Keep face, head size, pose, crop, aspect ratio and identity unchanged.",
    ),
    (
        "Cứu ảnh tối",
        "Recover an underexposed portrait naturally: brighten the subject, lift shadows and protect highlights. Preserve the original face, crop, image size, pose and composition.",
    ),
    (
        "Giảm bóng",
        "Reduce harsh facial shadows and hot spots while keeping the original light direction believable. Preserve face shape, expression, identity, crop, canvas size and background.",
    ),
];

const SHARPEN_CHOICES: [(&str, &str); 3] = [
    (
        "Nhẹ",
        "Apply light natural sharpening and mild detail enhancement. Improve eye, hair and clothing clarity slightly without changing facial features, skin texture, face shape, crop or image size.",
    ),
    (
        "Vừa",
        "Apply moderate natural sharpening and clarity enhancement. Make the portrait cleaner and crisper without oversharpening, halos, artifacts, or changing facial features, face shape, skin texture, crop or image size.",
    ),
    (
        "Giảm mờ",
        "Reduce mild blur and improve detail in a realistic way. Avoid halos, artifacts, fake facial details or any change to the original facial features, expression, crop, image size and aspect ratio.",
    ),
];

const CLEANUP_CHOICES: [(&str, &str); 4] = [
    (
        "Giảm noise",
        "Reduce image noise and compression artifacts while preserving real skin texture, hair detail, facial features, crop, canvas size and aspect ratio.",
    ),
    (
        "Xóa vết nhỏ",
        "Remove small dust, scratches, lint or temporary marks from the photo. Do not alter facial features, expression, identity, pose, crop, canvas size or important details.",
    ),
    (
        "Giảm bóng kính",
        "Reduce glare on eyeglasses naturally while keeping the eyes, face, glasses shape, expression, crop, canvas size and identity unchanged.",
    ),
    (
        "Làm sạch nền",
        "Clean small stains or uneven patches in the background only. Keep the subject, face, hair, clothing, pose, crop, canvas size and lighting unchanged.",
    ),
];

const BG_CHOICES: [(&str, &str); 5] = [
    (
        "Trắng",
        "Replace only the background with a clean plain white ID photo background. Keep the subject, face, hair, clothing, pose, crop, head size, canvas size and lighting unchanged.",
    ),
    (
        "Xanh nhạt",
        "Replace only the background with a clean light blue ID photo background. Keep the subject, face, hair, clothing, pose, crop, head size, canvas size and lighting unchanged.",
    ),
    (
        "Xám sáng",
        "Replace only the background with a smooth light gray studio background. Keep the subject, face, hair, clothing, pose, crop, head size, canvas size and lighting unchanged.",
    ),
    (
        "Nâu sáng",
        "Replace only the background with a soft warm beige studio background. Keep the subject, face, hair, clothing, pose, crop, head size, canvas size and lighting unchanged.",
    ),
    (
        "Văn phòng",
        "Replace only the background with a subtle professional office blur. Keep the subject, face, hair, clothing, pose, crop, head size, canvas size and lighting unchanged.",
    ),
];

const CLOTHES_CHOICES: [(&str, &str); 10] = [
    (
        "Sơ mi trắng",
        "Replace only the clothing with a clean plain white button-up shirt suitable for an ID photo. Keep neck, face, hair, body proportions, pose, background, crop and lighting unchanged. Natural fabric folds.",
    ),
    (
        "Sơ mi xanh nhạt",
        "Replace only the clothing with a clean light blue button-up shirt suitable for an ID photo. Keep neck, face, hair, body proportions, pose, background, crop and lighting unchanged. Natural fabric folds.",
    ),
    (
        "Vest đen",
        "Replace only the clothing with a black suit jacket over a plain white shirt, suitable for a formal ID photo. Keep neck, face, hair, body proportions, pose, background, crop and lighting unchanged.",
    ),
    (
        "Vest navy",
        "Replace only the clothing with a navy suit jacket over a plain white shirt, suitable for a formal ID photo. Keep neck, face, hair, body proportions, pose, background, crop and lighting unchanged.",
    ),
    (
        "Blazer xám",
        "Replace only the clothing with a light gray blazer over a plain white shirt. Keep neck, face, hair, body proportions, pose, background, crop and lighting unchanged.",
    ),
    (
        "Áo polo trắng",
        "Replace only the clothing with a clean plain white polo shirt suitable for a simple ID photo. Keep neck, face, hair, body proportions, pose, background, crop and lighting unchanged.",
    ),
    (
        "Áo phông đen",
        "Replace only the clothing with a clean plain black crew neck shirt. Keep neck, face, hair, body proportions, pose, background, crop and lighting unchanged.",
    ),
    (
        "Áo dài trắng",
        "Replace only the clothing with a simple white ao dai suitable for a clean portrait or ID-style photo. Keep neck, face, hair, body proportions, pose, background, crop and lighting unchanged.",
    ),
    (
        "Đồng phục",
        "Replace only the clothing with a neat neutral office uniform shirt. Keep neck, face, hair, body proportions, pose, background, crop and lighting unchanged.",
    ),
    (
        "Đồ trẻ em",
        "Replace only the clothing with a neat simple child ID photo shirt. Keep face, age, hair, body proportions, pose, background, crop and lighting unchanged.",
    ),
];

const MAKEUP_CHOICES: [(&str, &str); 4] = [
    (
        "Rất nhẹ",
        "Apply very light natural makeup: subtle skin tone evening and minimal lip/eye definition. Preserve the original face shape, facial features, expression, age and identity.",
    ),
    (
        "Nhẹ",
        "Apply light natural makeup: soft base, subtle lip color and gentle eye definition. Preserve the original face shape, facial features, expression, age and identity.",
    ),
    (
        "Vừa",
        "Apply moderate natural portrait makeup: clean base, soft blush, defined but natural eyes and lips. Preserve the original face shape, facial features, expression, age and identity.",
    ),
    (
        "Mạnh",
        "Apply stronger polished makeup suitable for a professional portrait, with clear eyes and lips, but do not reshape or alter facial features, face shape, expression, age or identity. Keep it photorealistic.",
    ),
];

pub fn build(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    if !data.ai.show_ai_panel {
        return;
    }

    let mut st = data.ai.ai.clone();
    let mut changed = false;
    let mut open = true;

    let screen = ctx.content_rect();
    let panel_bounds = panel_bounds(screen, data);
    if !stable_viewport_for_panel(screen, panel_bounds) {
        return;
    }
    let pos_x = (panel_bounds.max.x - PANEL_W - 12.0).max(panel_bounds.min.x + 12.0);
    let default_h = panel_bounds.height();
    let panel_id = egui::Id::new("ai_gemini_panel");

    let mut collapsing = egui::collapsing_header::CollapsingState::load_with_default_open(
        ctx,
        panel_id.with("collapsing"),
        true,
    );
    if !collapsing.is_open() {
        collapsing.set_open(true);
        collapsing.store(ctx);
    }

    egui::Window::new("Ai Image Studio")
        .id(panel_id)
        .open(&mut open)
        .default_pos(egui::pos2(pos_x, panel_bounds.top()))
        .default_size(egui::vec2(PANEL_W, default_h))
        .min_size(egui::vec2(MIN_PANEL_W, MIN_PANEL_H))
        .max_size(panel_bounds.size())
        // Keep the floating panel inside the canvas workspace. In particular,
        // its title bar can no longer be dragged above the horizontal ruler.
        .constrain_to(panel_bounds)
        .resizable(true)
        .collapsible(false)
        .show(ctx, |ui| {
            // Fill the user-selected window size. The ScrollArea then becomes the
            // flexible content viewport instead of forcing the window back to its
            // old fixed dimensions after an edge/corner drag.
            ui.set_min_size(ui.available_size());
            ui.spacing_mut().item_spacing.y = 6.0;

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    section(
                        ui,
                        "Nguồn xử lý",
                        "Chọn cách gửi ảnh sang AI.",
                        true,
                        |ui| {
                            source_selector(ui, &mut st, &mut changed);
                        },
                    );

                    if st.use_bridge {
                        ext_connection_ui(ui, data);
                    } else {
                        api_connection_ui(ui, data, &mut st, &mut changed, actions);
                    }

                    ai_live_status(ui, data, actions);

                    let active_busy = data
                        .doc
                        .doc_ai_busy
                        .get(data.doc.active_doc_idx)
                        .is_some_and(Option::is_some);
                    let can_run = if st.use_bridge {
                        data.doc.has_doc && data.ai.ext_connected && !active_busy
                    } else {
                        data.doc.has_doc && !active_busy
                    };

                    section(
                        ui,
                        "Ảnh thẻ (1 chạm)",
                        "Thay nền + sửa sáng + màu + da tự nhiên — giữ nguyên nét mặt.",
                        true,
                        |ui| {
                            let use_bridge = st.use_bridge;
                            id_photo_section(
                                ui,
                                &mut st,
                                can_run,
                                use_bridge,
                                &mut changed,
                                actions,
                            );
                        },
                    );

                    section(
                        ui,
                        "Xếp ảnh in",
                        "Nhân bản ảnh đang mở thành trang in 600dpi — mỗi tấm một layer.",
                        true,
                        |ui| {
                            impose_section(ui, data, &mut st, &mut changed, actions);
                        },
                    );

                    section(
                        ui,
                        "Chỉnh nhanh (preset)",
                        "Chỉnh từng phần: chọn mức rồi bấm Chạy.",
                        false,
                        |ui| {
                            let use_bridge = st.use_bridge;
                            preset_grid2(ui, &mut st, can_run, use_bridge, &mut changed, actions);
                        },
                    );

                    section(
                        ui,
                        "Tự viết yêu cầu",
                        "Viết prompt tùy ý — có lịch sử các lệnh đã dùng.",
                        false,
                        |ui| {
                            manual_editor(ui, data, &mut st, can_run, &mut changed, actions);
                        },
                    );

                    section(
                        ui,
                        "Đầu ra",
                        "Kết quả nên dùng như layer để vẽ mask.",
                        false,
                        |ui| {
                            output_selector(ui, &mut st, &mut changed);
                            ui.label(
                                egui::RichText::new(
                            "Layer kết quả được resize khớp Background để vẽ mask lấy từng vùng.",
                        )
                                .small()
                                .weak(),
                            );
                        },
                    );
                }); // end ScrollArea
        });

    if changed {
        actions.ai.set_ai_panel = Some(st);
    }
    if !open {
        actions.ai.toggle_ai_panel = Some(false);
    }
}

fn panel_bounds(screen: egui::Rect, data: &UiData) -> egui::Rect {
    let ruler_off = if data.chrome.show_rulers {
        super::RULER_SIZE
    } else {
        0.0
    };
    egui::Rect::from_min_max(
        egui::pos2(data.chrome.toolbar_w + ruler_off, TOP_CHROME_H + ruler_off),
        egui::pos2(
            screen.max.x - data.chrome.panel_r_w,
            screen.max.y - STATUS_BAR_H,
        ),
    )
}

fn stable_viewport_for_panel(screen: egui::Rect, bounds: egui::Rect) -> bool {
    screen.height() >= MIN_STABLE_VIEWPORT_H
        && bounds.width() >= MIN_PANEL_W
        && bounds.height() >= MIN_PANEL_H
}

/// A collapsible group (arrow ▸/▾, like the Develop panel). `default_open`
/// controls only the first-time state; egui then persists the user's toggle.
fn section<R>(
    ui: &mut egui::Ui,
    title: &str,
    subtitle: &str,
    default_open: bool,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    egui::CollapsingHeader::new(egui::RichText::new(title).strong())
        .id_salt(title)
        .default_open(default_open)
        .show(ui, |ui| {
            if !subtitle.is_empty() {
                ui.label(egui::RichText::new(subtitle).small().weak());
                ui.add_space(4.0);
            }
            add_contents(ui)
        })
        .body_returned
}

fn source_selector(ui: &mut egui::Ui, st: &mut AiPanelState, changed: &mut bool) {
    use crate::core::ai::AiSource;
    // Selecting a source is the single source of truth; keep the legacy
    // `use_bridge`/`provider` derived from it so the rest of the panel is unchanged.
    let pick = |st: &mut AiPanelState, src: AiSource, changed: &mut bool| {
        st.source = src;
        st.use_bridge = src.is_web();
        if let Some(p) = src.api_provider() {
            st.provider = p;
        }
        *changed = true;
    };
    // Top row: the two paid APIs.
    ui.horizontal(|ui| {
        let gap = ui.spacing().item_spacing.x;
        let w = ((ui.available_width() - gap) / 2.0).floor();
        if source_button(
            ui,
            w,
            st.source == AiSource::GeminiApi,
            "Gemini API",
            "nano-banana",
        )
        .clicked()
        {
            pick(st, AiSource::GeminiApi, changed);
        }
        if source_button(
            ui,
            w,
            st.source == AiSource::OpenAiApi,
            "OpenAI API",
            "gpt-image-1",
        )
        .clicked()
        {
            pick(st, AiSource::OpenAiApi, changed);
        }
    });
    // Bottom row: free, via the browser extension.
    ui.horizontal(|ui| {
        let gap = ui.spacing().item_spacing.x;
        let w = ((ui.available_width() - gap) / 2.0).floor();
        if source_button(
            ui,
            w,
            st.source == AiSource::GeminiWeb,
            "Gemini Web",
            "Extension",
        )
        .clicked()
        {
            pick(st, AiSource::GeminiWeb, changed);
        }
        if source_button(
            ui,
            w,
            st.source == AiSource::ChatGptWeb,
            "ChatGPT Web",
            "Extension",
        )
        .clicked()
        {
            pick(st, AiSource::ChatGptWeb, changed);
        }
    });
}

fn source_button(
    ui: &mut egui::Ui,
    width: f32,
    selected: bool,
    title: &str,
    subtitle: &str,
) -> egui::Response {
    let fill = if selected {
        egui::Color32::from_rgb(42, 75, 118)
    } else {
        egui::Color32::from_rgb(38, 38, 42)
    };
    let stroke = if selected {
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(125, 170, 245))
    } else {
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(64, 64, 68))
    };
    ui.add_sized(
        [width, 48.0],
        egui::Button::new(format!("{title}\n{subtitle}"))
            .fill(fill)
            .stroke(stroke),
    )
}

fn api_connection_ui(
    ui: &mut egui::Ui,
    data: &UiData,
    st: &mut AiPanelState,
    changed: &mut bool,
    actions: &mut UiActions,
) {
    use crate::core::ai::AiProvider;
    let openai = st.provider == AiProvider::OpenAi;
    let (title, hint): (&str, &str) = if openai {
        ("OpenAI API", "sk-...")
    } else {
        ("Gemini API", "AIza...")
    };
    section(
        ui,
        title,
        "Dùng key để xử lý trực tiếp trong app.",
        true,
        |ui| {
            ui.horizontal(|ui| {
                ui.label("API key");
                let field = if openai {
                    &mut st.openai_api_key
                } else {
                    &mut st.api_key
                };
                let r = ui.add(
                    egui::TextEdit::singleline(field)
                        .password(true)
                        .hint_text(hint)
                        .desired_width(ui.available_width() - 54.0),
                );
                *changed |= r.changed();
                if ui.button("Lưu").clicked() {
                    actions.ai.set_ai_panel = Some(st.clone());
                    actions.ai.ai_save_key = true;
                }
            });
            if openai {
                ui.label(
                    egui::RichText::new("Cần verify organization trên OpenAI để dùng gpt-image-1.")
                        .small()
                        .weak(),
                );
            }
            if !data.ai.ai_status.is_empty() {
                ui.label(status_text(&data.ai.ai_status));
            }
        },
    );
}

fn ext_connection_ui(ui: &mut egui::Ui, data: &UiData) {
    section(
        ui,
        "Extension trình duyệt",
        "Dùng phiên Gemini/ChatGPT của bạn (chạy cả trên Linux).",
        true,
        |ui| {
            let (dot, color) = if data.ai.ext_connected {
                ("● Đã kết nối", egui::Color32::from_rgb(80, 200, 120))
            } else {
                ("○ Chưa kết nối", egui::Color32::from_gray(150))
            };
            ui.label(egui::RichText::new(dot).color(color).strong());

            ui.add_space(3.0);
            ui.label(
                egui::RichText::new("Token (dán vào popup extension):")
                    .small()
                    .weak(),
            );
            ui.horizontal(|ui| {
                let mut tok = data.ai.ext_token.clone();
                ui.add(
                    egui::TextEdit::singleline(&mut tok).desired_width(ui.available_width() - 54.0),
                );
                if ui.button("Copy").clicked() {
                    ui.ctx().copy_text(data.ai.ext_token.clone());
                }
            });

            if !data.ai.ext_connected {
                ui.label(
                    egui::RichText::new(
                        "Cài extension (thư mục extension/) → dán token → mở gemini.google.com.",
                    )
                    .small()
                    .weak(),
                );
            }
            if !data.ai.ext_status.is_empty() {
                ui.label(status_text(&data.ai.ext_status));
            }
            // Rolling log so a status line that flashed by can still be read.
            if !data.ai.ext_log.is_empty() {
                egui::CollapsingHeader::new(egui::RichText::new("Nhật ký").small())
                    .default_open(false)
                    .show(ui, |ui| {
                        for line in data.ai.ext_log.iter().rev() {
                            ui.label(egui::RichText::new(line).small().weak());
                        }
                    });
            }
        },
    );
}

fn ai_live_status(ui: &mut egui::Ui, data: &UiData, actions: &mut UiActions) {
    if data.ai.ext_queue_len > 0 {
        ui.label(
            egui::RichText::new(format!("Hàng chờ: {} lệnh", data.ai.ext_queue_len))
                .small()
                .weak(),
        );
    }

    let Some(busy) = data
        .doc
        .doc_ai_busy
        .get(data.doc.active_doc_idx)
        .and_then(Option::as_ref)
    else {
        return;
    };

    ui.horizontal_wrapped(|ui| {
        if let Some(pos) = busy.queue_pos {
            ui.label(
                egui::RichText::new(format!(
                    "{} Đang chờ lượt ({}, vị trí {pos})",
                    egui_phosphor::regular::CLOCK,
                    busy.label
                ))
                .small()
                .color(ui.visuals().weak_text_color()),
            );
        } else {
            ui.add(egui::Spinner::new().size(13.0));
            let elapsed = busy.elapsed_secs.unwrap_or(0);
            ui.label(status_text(&format!(
                "Đang xử lý ({})… {elapsed}s",
                busy.label
            )));
        }
        if ui.small_button("Hủy lệnh của ảnh này").clicked() {
            actions.ai.ai_cancel_active = true;
        }
    });
}

fn output_selector(ui: &mut egui::Ui, st: &mut AiPanelState, changed: &mut bool) {
    ui.horizontal_wrapped(|ui| {
        if ui
            .selectable_label(!st.output_new_file, "Layer trên Background")
            .clicked()
        {
            st.output_new_file = false;
            *changed = true;
        }
        if ui
            .selectable_label(st.output_new_file, "File mới")
            .clicked()
        {
            st.output_new_file = true;
            *changed = true;
        }
    });
}

fn manual_editor(
    ui: &mut egui::Ui,
    data: &UiData,
    st: &mut AiPanelState,
    can_run: bool,
    changed: &mut bool,
    actions: &mut UiActions,
) {
    let prompt_r = ui.add(
        egui::TextEdit::multiline(&mut st.prompt)
            .hint_text("Nhập yêu cầu chỉnh sửa…")
            .desired_rows(4)
            .desired_width(f32::INFINITY),
    );
    *changed |= prompt_r.changed();
    history_combo(ui, data, st, changed);

    let prompt = st.prompt.trim().to_string();
    let label = if st.use_bridge {
        "Gửi sang web (extension)"
    } else {
        "Chạy bằng API"
    };
    if ui
        .add_enabled(can_run && !prompt.is_empty(), egui::Button::new(label))
        .clicked()
    {
        dispatch_prompt(st, &prompt, st.use_bridge, actions);
    }
}

fn preset_grid2(
    ui: &mut egui::Ui,
    st: &mut AiPanelState,
    can_run: bool,
    use_bridge: bool,
    changed: &mut bool,
    actions: &mut UiActions,
) {
    ui.label(egui::RichText::new("Chân dung").small().strong());
    if let Some(prompt) = choice_preset_row(
        ui,
        "ai_retouch_preset",
        "Retouch da",
        &RETOUCH_CHOICES,
        &mut st.retouch_preset,
        can_run,
        changed,
    ) {
        dispatch_prompt(st, prompt, use_bridge, actions);
    }
    if let Some(prompt) = choice_preset_row(
        ui,
        "ai_makeup_preset",
        "Trang điểm",
        &MAKEUP_CHOICES,
        &mut st.makeup_preset,
        can_run,
        changed,
    ) {
        dispatch_prompt(st, prompt, use_bridge, actions);
    }
    if let Some(prompt) = choice_preset_row(
        ui,
        "ai_clothes_preset",
        "Thay áo",
        &CLOTHES_CHOICES,
        &mut st.clothes_preset,
        can_run,
        changed,
    ) {
        dispatch_prompt(st, prompt, use_bridge, actions);
    }

    ui.add_space(6.0);
    ui.label(egui::RichText::new("Ảnh thẻ / studio").small().strong());
    if let Some(prompt) = choice_preset_row(
        ui,
        "ai_bg_preset",
        "Nền ảnh",
        &BG_CHOICES,
        &mut st.bg_preset,
        can_run,
        changed,
    ) {
        dispatch_prompt(st, prompt, use_bridge, actions);
    }
    if let Some(prompt) = choice_preset_row(
        ui,
        "ai_light_preset",
        "Ánh sáng",
        &LIGHT_CHOICES,
        &mut st.light_preset,
        can_run,
        changed,
    ) {
        dispatch_prompt(st, prompt, use_bridge, actions);
    }
    if let Some(prompt) = choice_preset_row(
        ui,
        "ai_color_preset",
        "Màu sắc",
        &COLOR_CHOICES,
        &mut st.color_preset,
        can_run,
        changed,
    ) {
        dispatch_prompt(st, prompt, use_bridge, actions);
    }

    ui.add_space(6.0);
    ui.label(egui::RichText::new("Kỹ thuật").small().strong());
    if let Some(prompt) = choice_preset_row(
        ui,
        "ai_sharpen_preset",
        "Độ nét",
        &SHARPEN_CHOICES,
        &mut st.sharpen_preset,
        can_run,
        changed,
    ) {
        dispatch_prompt(st, prompt, use_bridge, actions);
    }
    if let Some(prompt) = choice_preset_row(
        ui,
        "ai_cleanup_preset",
        "Làm sạch",
        &CLEANUP_CHOICES,
        &mut st.cleanup_preset,
        can_run,
        changed,
    ) {
        dispatch_prompt(st, prompt, use_bridge, actions);
    }
}

fn choice_preset_row(
    ui: &mut egui::Ui,
    id: &'static str,
    label: &'static str,
    choices: &'static [(&'static str, &'static str)],
    selected: &mut usize,
    can_run: bool,
    changed: &mut bool,
) -> Option<&'static str> {
    if choices.is_empty() {
        return None;
    }
    *selected = (*selected).min(choices.len() - 1);
    let mut run_prompt = None;
    ui.horizontal(|ui| {
        ui.add_sized([92.0, 22.0], egui::Label::new(label));
        egui::ComboBox::from_id_salt(id)
            .selected_text(choices[*selected].0)
            .width(164.0)
            .show_ui(ui, |ui| {
                for (idx, (name, _)) in choices.iter().enumerate() {
                    if ui.selectable_value(selected, idx, *name).changed() {
                        *changed = true;
                    }
                }
            });
        if ui
            .add_enabled(can_run, egui::Button::new("Chạy"))
            .on_hover_text("Chạy preset này trên ảnh hiện tại")
            .clicked()
        {
            run_prompt = Some(choices[*selected].1);
        }
    });
    run_prompt
}

fn history_combo(ui: &mut egui::Ui, data: &UiData, st: &mut AiPanelState, changed: &mut bool) {
    if data.ai.ai_history.is_empty() {
        return;
    }
    egui::ComboBox::from_id_salt("ai_prompt_history")
        .selected_text("Lịch sử prompt")
        .show_ui(ui, |ui| {
            for h in data.ai.ai_history.iter() {
                if ui.selectable_label(false, h).clicked() {
                    st.prompt = h.clone();
                    *changed = true;
                }
            }
        });
}

/// Compose the one-click ID-photo edit instruction from the toggles. Returns the
/// CORE instruction; the run path wraps it with `guarded_edit_prompt`, whose
/// identity-preservation is exactly what an ID photo needs.
fn compose_id_photo_prompt(st: &AiPanelState) -> String {
    let bg = if st.id_bg == 1 {
        "a solid, perfectly even blue background of the EXACT color hex #009CFF (sRGB 0, 156, 255) — \
         fill the entire background with this precise uniform blue, no gradient and no other shade"
    } else {
        "a solid, evenly lit pure white background"
    };
    let mut p = String::new();
    p.push_str(
        "This is a professional, flattering ID/passport photo retouch of the same person. Replace \
         the background with ",
    );
    p.push_str(bg);
    p.push_str(
        ". Create a clean, polished ID-photo look: bright but not overexposed, soft frontal studio \
         light, smooth highlight transitions, neutral-to-subtly-warm white balance and natural \
         contrast. Keep the person's real skin tone with only a very slight warm golden undertone; \
         avoid an overly pale, white or pink complexion. Retouch skin beautifully but realistically: \
         smoother, clearer and even-toned across the face and neck; reduce shine, excess redness, \
         acne and dark patches without removing the skin's natural warm/yellow undertone or creating \
         plastic skin. Keep pores and natural texture \
         subtle. Add gentle clarity to the eyes, hair and clothing, with no halos or harsh \
         sharpening. ",
    );
    match st.id_gender {
        1 => p.push_str("The subject is male: no makeup. "),
        2 => p.push_str(
            "The subject is female: at most very light, natural makeup (clean, subtle), with the natural lip color made only slightly redder and healthier-looking, never vivid or heavily made up. ",
        ),
        _ => {}
    }
    if st.id_change_clothes {
        let outfit = match st.id_outfit {
            1 => {
                let color = AODAI_COLORS
                    .get(st.id_aodai_color as usize)
                    .map(|c| c.1)
                    .unwrap_or("white");
                format!(
                    "a traditional Vietnamese áo dài (long dress) in {color}, with a neat high collar"
                )
            }
            2 => {
                let color = VEST_COLORS
                    .get(st.id_vest_color as usize)
                    .map(|c| c.1)
                    .unwrap_or("black");
                let buttons = match st.id_vest_buttons {
                    0 => "one-button",
                    2 => "three-button",
                    _ => "two-button",
                };
                let front = if st.id_vest_front == 1 {
                    "double-breasted"
                } else {
                    "single-breasted"
                };
                let tie = if st.id_vest_tie {
                    let tie_color = TIE_COLORS
                        .get(st.id_tie_color as usize)
                        .map(|c| c.1)
                        .unwrap_or("burgundy");
                    let pattern = match st.id_tie_pattern {
                        1 => "with a subtle diagonal stripe pattern",
                        2 => "with a subtle small-dot pattern",
                        _ => "plain with no pattern",
                    };
                    format!(", plus a neat {tie_color} tie {pattern}")
                } else {
                    ", with no tie".to_string()
                };
                format!(
                    "a formal {color}, {front}, {buttons} suit jacket worn over a plain white collared dress shirt{tie}, neat and well-fitted"
                )
            }
            _ => {
                let color = SHIRT_COLORS
                    .get(st.id_shirt_color as usize)
                    .map(|c| c.1)
                    .unwrap_or("white");
                format!(
                    "a clean {color} collared dress shirt — the shirt ONLY, with NO suit jacket, NO vest and NO tie"
                )
            }
        };
        p.push_str("Replace ONLY the clothing with ");
        p.push_str(&outfit);
        p.push_str("; keep the same body pose and shoulders, change nothing else. ");
    }
    if st.id_tidy_hair {
        p.push_str(
            "Tidy the hair naturally: smooth only stray or flyaway hairs and make the outline neat, while preserving the original hairstyle, hairline, length, volume, color and identity. ",
        );
    }
    p.push_str(
        "Preserve the person's identity and natural likeness: keep the same face structure, facial \
         features, jawline, nose, eyes, eyebrows, mouth, expression, gaze, age, hairstyle, head pose, \
         head size and identifying marks. Flattering retouch is allowed, but do not change bone \
         structure, facial proportions, expression, or make the person look like someone else. Do not \
         zoom, rotate, re-crop, re-frame, or change canvas/aspect ratio. Only edit background, \
         lighting, color, skin, requested clothing and requested stray-hair cleanup. Photorealistic, soft, polished and natural ID \
         photo.",
    );
    p
}

/// One-click "Ảnh thẻ" composer: gender / background / change-clothes toggles that
/// dynamically build the prompt, plus a run button.
fn id_photo_section(
    ui: &mut egui::Ui,
    st: &mut AiPanelState,
    can_run: bool,
    use_bridge: bool,
    changed: &mut bool,
    actions: &mut UiActions,
) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Giới tính").small().weak());
        for (val, label) in [(0u8, "Tự động"), (1, "Nam"), (2, "Nữ")] {
            if ui.selectable_label(st.id_gender == val, label).clicked() {
                st.id_gender = val;
                *changed = true;
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Phông nền").small().weak());
        for (val, label) in [(0u8, "Trắng"), (1, "Xanh")] {
            if ui.selectable_label(st.id_bg == val, label).clicked() {
                st.id_bg = val;
                *changed = true;
            }
        }
    });
    if ui.checkbox(&mut st.id_change_clothes, "Thay áo").changed() {
        *changed = true;
    }
    if st.id_change_clothes {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Loại áo").small().weak());
            for (val, label) in [(0u8, "Sơ mi"), (1, "Áo dài"), (2, "Vest")] {
                if ui.selectable_label(st.id_outfit == val, label).clicked() {
                    st.id_outfit = val;
                    *changed = true;
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Màu áo").small().weak());
            match st.id_outfit {
                1 => {
                    for (i, (label, _)) in AODAI_COLORS.iter().enumerate() {
                        if ui
                            .selectable_label(st.id_aodai_color as usize == i, *label)
                            .clicked()
                        {
                            st.id_aodai_color = i as u8;
                            *changed = true;
                        }
                    }
                }
                2 => {
                    for (i, (label, _)) in VEST_COLORS.iter().enumerate() {
                        if ui
                            .selectable_label(st.id_vest_color as usize == i, *label)
                            .clicked()
                        {
                            st.id_vest_color = i as u8;
                            *changed = true;
                        }
                    }
                }
                _ => {
                    for (i, (label, _)) in SHIRT_COLORS.iter().enumerate() {
                        if ui
                            .selectable_label(st.id_shirt_color as usize == i, *label)
                            .clicked()
                        {
                            st.id_shirt_color = i as u8;
                            *changed = true;
                        }
                    }
                }
            }
        });
        if st.id_outfit == 2 {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("Số cúc").small().weak());
                for (val, label) in [(0u8, "1 cúc"), (1, "2 cúc"), (2, "3 cúc")] {
                    if ui
                        .selectable_label(st.id_vest_buttons == val, label)
                        .clicked()
                    {
                        st.id_vest_buttons = val;
                        *changed = true;
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("Kiểu vạt").small().weak());
                for (val, label) in [(0u8, "Vạt đơn"), (1, "Vạt đôi")] {
                    if ui
                        .selectable_label(st.id_vest_front == val, label)
                        .clicked()
                    {
                        st.id_vest_front = val;
                        *changed = true;
                    }
                }
            });
            if ui.checkbox(&mut st.id_vest_tie, "Thêm cà vạt").changed() {
                *changed = true;
            }
            if st.id_vest_tie {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("Màu cà vạt").small().weak());
                    for (i, (label, _)) in TIE_COLORS.iter().enumerate() {
                        if ui
                            .selectable_label(st.id_tie_color as usize == i, *label)
                            .clicked()
                        {
                            st.id_tie_color = i as u8;
                            *changed = true;
                        }
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("Hoa văn").small().weak());
                    for (val, label) in [(0u8, "Trơn"), (1, "Sọc chéo"), (2, "Chấm nhỏ")] {
                        if ui
                            .selectable_label(st.id_tie_pattern == val, label)
                            .clicked()
                        {
                            st.id_tie_pattern = val;
                            *changed = true;
                        }
                    }
                });
            }
        }
    }

    if ui.checkbox(&mut st.id_tidy_hair, "Tóc gọn gàng").changed() {
        *changed = true;
    }

    ui.add_space(6.0);
    let prompt = compose_id_photo_prompt(st);
    let btn = egui::Button::new(egui::RichText::new("Làm ảnh thẻ nhanh").strong().size(16.0))
        .min_size(egui::vec2(ui.available_width(), 40.0))
        .fill(egui::Color32::from_rgb(42, 95, 70));
    if ui.add_enabled(can_run, btn).clicked() {
        dispatch_prompt(st, &prompt, use_bridge, actions);
    }
    if !can_run {
        ui.label(
            egui::RichText::new(
                "Chưa chạy được — cần ảnh đang mở và nguồn AI đã kết nối (xem Nguồn xử lý).",
            )
            .small()
            .weak(),
        );
    }
}

/// "Xếp ảnh in": duplicate the open (pre-cropped) photo onto a 10×15 / 13×18
/// print sheet at 600dpi — one layer per copy, laid out by core/imposition.
fn impose_section(
    ui: &mut egui::Ui,
    data: &UiData,
    st: &mut AiPanelState,
    changed: &mut bool,
    actions: &mut UiActions,
) {
    use crate::core::imposition::{self, Paper, PhotoKind};

    let detected = if data.doc.has_doc {
        PhotoKind::detect(data.doc.canvas_w, data.doc.canvas_h, data.doc.canvas_dpi)
    } else {
        None
    };
    let info = match detected {
        Some(kind) => format!(
            "Ảnh hiện tại: {} ({}×{} px @{:.0}dpi)",
            kind.label(),
            data.doc.canvas_w,
            data.doc.canvas_h,
            data.doc.canvas_dpi
        ),
        None if data.doc.has_doc => {
            "Ảnh hiện tại không đúng cỡ 3×4 (2.8×3.8cm) / 4×6 — vẫn xếp được, ảnh sẽ được co về đúng ô."
                .to_string()
        }
        None => "Hãy mở ảnh đã crop trước.".to_string(),
    };
    ui.label(egui::RichText::new(info).small().weak());

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Khe cắt").small().weak());
        let mut gap = st.impose_gap_px as i64;
        let r = ui.add(egui::DragValue::new(&mut gap).range(0..=100).suffix(" px"));
        if r.changed() {
            st.impose_gap_px = gap.clamp(0, 100) as u32;
            *changed = true;
        }
    });

    let gap = st.impose_gap_px;
    let jobs: [(Paper, PhotoKind); 3] = [
        (Paper::P10x15, PhotoKind::Id3x4),
        (Paper::P13x18, PhotoKind::Id3x4),
        (Paper::P13x18, PhotoKind::Id4x6),
    ];
    for (paper, kind) in jobs {
        let count = imposition::layout(paper, kind, gap).placements.len();
        let label = format!("Xếp {} — {} tấm {}", paper.label(), count, kind.label());
        let btn = egui::Button::new(egui::RichText::new(label).strong())
            .min_size(egui::vec2(ui.available_width(), 26.0));
        if ui.add_enabled(data.doc.has_doc && count > 0, btn).clicked() {
            actions.ai.set_ai_panel = Some(st.clone());
            actions.doc.impose_sheet = Some((paper, kind, gap));
        }
    }
    ui.label(
        egui::RichText::new("Mỗi tấm là một layer riêng — dùng Move tool để tự sắp xếp lại.")
            .small()
            .weak(),
    );
}

fn dispatch_prompt(st: &mut AiPanelState, prompt: &str, use_bridge: bool, actions: &mut UiActions) {
    st.prompt = prompt.trim().to_string();
    actions.ai.set_ai_panel = Some(st.clone());
    if use_bridge {
        // Web source → drive the user's browser via the extension (do_ext_edit
        // reads the target site from the selected source).
        actions.ai.ext_run = Some(prompt.trim().to_string());
    } else {
        actions.ai.ai_run = Some(prompt.trim().to_string());
    }
}

fn status_text(s: &str) -> egui::RichText {
    egui::RichText::new(s)
        .small()
        .color(egui::Color32::from_rgb(125, 180, 250))
}

//! "Việc nhanh": pick a job and the customer's folder, and iAi does it by
//! itself, no AI and no network; where it stands, and "Dừng". The window
//! stays free while it runs.

use super::*;
use crate::ui::{QuickField, QuickForm, QuickIntent, QuickTask, QuickValue};

pub(crate) const QUICK_TITLE: &str = "Việc nhanh";
const QUICK_WIDTH: f32 = 420.0;
const OK_GREEN: egui::Color32 = egui::Color32::from_rgb(40, 160, 70);
const FAIL_RED: egui::Color32 = egui::Color32::from_rgb(200, 60, 60);
pub(crate) const QUICK_SCAN: &str = "Làm sạch ảnh chụp giấy tờ";
pub(crate) const QUICK_IDS: &str = "Ảnh thẻ cho nhiều người";

fn form_id() -> egui::Id {
    egui::Id::new("quick_form")
}

/// The last job asked for from elsewhere that the window took.
fn preset_id() -> egui::Id {
    egui::Id::new("quick_preset_taken")
}

fn combo(ui: &mut egui::Ui, id: &str, value: &mut String, choices: &[(&str, &str)], width: f32) {
    let shown = choices
        .iter()
        .find(|(v, _)| v == value)
        .map_or(value.as_str(), |(_, text)| text)
        .to_string();
    egui::ComboBox::from_id_salt(id)
        .selected_text(shown)
        .width(width)
        .show_ui(ui, |ui| {
            for (v, text) in choices {
                ui.selectable_value(value, (*v).to_string(), *text);
            }
        });
}

/// A saved skill's values, as the person fills them.
fn skill_fields(ui: &mut egui::Ui, fields: &mut [QuickField]) {
    for (k, field) in fields.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.label(&field.label);
            match &mut field.value {
                QuickValue::Text(text) => {
                    ui.add(egui::TextEdit::singleline(text).desired_width(140.0));
                }
                QuickValue::Number(n) => {
                    ui.add(egui::DragValue::new(n).speed(1.0).max_decimals(2));
                }
                QuickValue::Bool(b) => {
                    ui.push_id(k, |ui| ui.checkbox(b, ""));
                }
            }
        });
    }
}

pub(crate) fn quick_window(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let view = &data.chrome.quick;
    let mut form = ctx
        .data(|d| d.get_temp::<QuickForm>(form_id()))
        .unwrap_or_default();
    if let Some((n, task)) = &view.preset
        && ctx.data(|d| d.get_temp::<u64>(preset_id())) != Some(*n)
    {
        form.task = task.clone();
        let n = *n;
        ctx.data_mut(|d| d.insert_temp(preset_id(), n));
    }
    let offered = |task: &QuickTask| match task {
        QuickTask::ScanCleanup => true,
        QuickTask::IdPhotos => cfg!(feature = "pro"),
        QuickTask::Skill(name) => view.skills.iter().any(|s| &s.name == name),
    };
    if !offered(&form.task) {
        form.task = QuickTask::ScanCleanup;
    }
    // A skill chosen anew: its own values.
    if let QuickTask::Skill(name) = &form.task
        && let Some(skill) = view.skills.iter().find(|s| &s.name == name)
    {
        let same = skill.fields.len() == form.fields.len()
            && skill
                .fields
                .iter()
                .zip(&form.fields)
                .all(|(a, b)| a.name == b.name);
        if !same {
            form.fields = skill.fields.clone();
        }
    }
    let mut open = true;
    let corner = ctx.content_rect().right_top() + egui::vec2(-QUICK_WIDTH - 60.0, 76.0);
    egui::Window::new(QUICK_TITLE)
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .fixed_size([QUICK_WIDTH, 0.0])
        .default_pos(corner)
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.label(
                egui::RichText::new(
                    "iAi tự làm trên máy này: không cần AI, không cần mạng. Bạn vẫn làm việc \
                     khác được trong lúc chờ.",
                )
                .small()
                .weak(),
            );
            ui.add_space(4.0);
            let running = view.run.as_ref().is_some_and(|r| r.running);
            ui.add_enabled_ui(!running, |ui| {
                ui.label(egui::RichText::new("Thư mục ảnh của khách").strong());
                ui.horizontal(|ui| {
                    if ui.button("Chọn thư mục…").clicked() {
                        actions.chrome.quick = Some(QuickIntent::PickFolder);
                    }
                    let folder = if view.folder.is_empty() {
                        egui::RichText::new("Chưa chọn").weak()
                    } else {
                        egui::RichText::new(&view.folder).small()
                    };
                    ui.add(egui::Label::new(folder).truncate());
                });
                ui.add_space(6.0);
                ui.label(egui::RichText::new("Việc").strong());
                ui.radio_value(&mut form.task, QuickTask::ScanCleanup, QUICK_SCAN);
                if cfg!(feature = "pro") {
                    let text = if view.pro {
                        QUICK_IDS.to_string()
                    } else {
                        format!("{QUICK_IDS} (Gói Pro)")
                    };
                    ui.add_enabled_ui(view.pro, |ui| {
                        ui.radio_value(&mut form.task, QuickTask::IdPhotos, text)
                    });
                }
                if !view.skills.is_empty() {
                    ui.add_space(2.0);
                    ui.label(egui::RichText::new("Kỹ năng đã lưu").small().weak());
                    egui::ScrollArea::vertical()
                        .id_salt("quick_skills")
                        .max_height(120.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for s in &view.skills {
                                ui.horizontal(|ui| {
                                    ui.radio_value(
                                        &mut form.task,
                                        QuickTask::Skill(s.name.clone()),
                                        &s.title,
                                    );
                                    ui.label(egui::RichText::new(&s.status).small().weak());
                                });
                            }
                        });
                }
                ui.add_space(6.0);
                match &form.task {
                    QuickTask::ScanCleanup => {
                        ui.horizontal(|ui| {
                            ui.label("Kiểu");
                            combo(
                                ui,
                                "quick_look",
                                &mut form.look,
                                &[
                                    ("grayscale", "Xám, nền trắng"),
                                    ("bilevel", "Đen trắng (chữ rõ nhất)"),
                                ],
                                170.0,
                            );
                        });
                    }
                    QuickTask::IdPhotos => {
                        ui.label(
                            egui::RichText::new(
                                "Mỗi ảnh trong thư mục là một người. Ảnh không thấy rõ mặt sẽ được \
                                 báo lại.",
                            )
                            .small()
                            .weak(),
                        );
                        ui.horizontal(|ui| {
                            ui.label("Cỡ");
                            combo(
                                ui,
                                "quick_size",
                                &mut form.size,
                                super::ai_errand::ID_PHOTO_SIZES,
                                78.0,
                            );
                            ui.label("Nền");
                            combo(
                                ui,
                                "quick_backdrop",
                                &mut form.backdrop,
                                &[("white", "Trắng"), ("blue", "Xanh")],
                                78.0,
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("Giấy");
                            combo(
                                ui,
                                "quick_paper",
                                &mut form.paper,
                                &[("10x15", "10×15"), ("13x18", "13×18")],
                                78.0,
                            );
                            ui.label("Mỗi người");
                            ui.add(egui::DragValue::new(&mut form.copies).range(1..=20));
                            ui.label("tấm");
                        });
                    }
                    QuickTask::Skill(name) => {
                        if let Some(s) = view.skills.iter().find(|s| &s.name == name)
                            && !s.description.trim().is_empty()
                        {
                            ui.label(egui::RichText::new(&s.description).small().weak());
                        }
                        skill_fields(ui, &mut form.fields);
                    }
                }
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("Kết quả lưu vào thư mục iAi_ket_qua trong thư mục ảnh.")
                        .small()
                        .weak(),
                );
                ui.add_space(6.0);
                let ready = !view.folder.is_empty() && (!form.task.pro() || view.pro);
                let go = egui::Button::new(
                    egui::RichText::new(format!("{}  Làm", ph::LIGHTNING)).strong(),
                );
                if ui.add_enabled(ready, go).clicked() {
                    actions.chrome.quick = Some(QuickIntent::Start(form.clone()));
                }
            });
            if let Some(run) = &view.run {
                ui.add_space(6.0);
                ui.separator();
                ui.label(egui::RichText::new(&run.title).strong());
                let stage = egui::RichText::new(&run.stage);
                let stage = if run.failed {
                    stage.color(FAIL_RED)
                } else if !run.running {
                    stage.color(OK_GREEN)
                } else {
                    stage
                };
                ui.label(stage);
                ui.horizontal(|ui| {
                    if run.running && ui.button("Dừng").clicked() {
                        actions.chrome.quick = Some(QuickIntent::Stop);
                    }
                    if run.results.is_some() && ui.button("Mở thư mục kết quả").clicked() {
                        actions.chrome.quick = Some(QuickIntent::OpenResults);
                    }
                });
                if run.running {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(300));
                }
            }
        });
    ctx.data_mut(|d| d.insert_temp(form_id(), form));
    if !open {
        actions.chrome.quick = Some(QuickIntent::Close);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::dialogs::stage::Stage;
    use crate::ui::{QuickRunView, QuickSkill, QuickView};

    fn staged(view: QuickView) -> Stage {
        let mut data = UiData::default();
        data.chrome.quick = view;
        Stage::on(data, egui::vec2(1000.0, 800.0), quick_window)
    }

    fn skill() -> QuickSkill {
        QuickSkill {
            name: "anh-the-hang-loat".to_string(),
            title: "Ảnh thẻ hàng loạt → tờ in → PDF".to_string(),
            description: "Làm ảnh thẻ cho mọi ảnh chân dung trong một thư mục.".to_string(),
            status: "đã dùng 2 lần".to_string(),
            fields: vec![QuickField {
                name: "copies".to_string(),
                label: "Số tấm mỗi người".to_string(),
                value: QuickValue::Number(2.0),
            }],
        }
    }

    #[test]
    fn a_job_starts_on_the_folder_chosen_and_never_without_one() {
        let ready = |folder: &str| {
            staged(QuickView {
                open: true,
                folder: folder.to_string(),
                pro: true,
                skills: vec![skill()],
                ..QuickView::default()
            })
        };
        let empty = ready("");
        assert!(empty.says("Chưa chọn") && empty.says("không cần AI"));
        assert_eq!(empty.click("Làm").0.chrome.quick, None);
        assert_eq!(
            empty.click("Chọn thư mục…").0.chrome.quick,
            Some(QuickIntent::PickFolder)
        );
        let chosen = ready(r"C:\khach");
        match chosen.click("Làm").0.chrome.quick {
            Some(QuickIntent::Start(form)) => {
                assert_eq!(form.task, QuickTask::ScanCleanup);
                assert_eq!(form.look, "grayscale");
            }
            other => panic!("{other:?}"),
        }
        // A saved skill: its description and its values.
        chosen.click("Ảnh thẻ hàng loạt → tờ in → PDF");
        assert!(chosen.says("Số tấm mỗi người") && chosen.says("mọi ảnh chân dung"));
        match chosen.click("Làm").0.chrome.quick {
            Some(QuickIntent::Start(form)) => {
                assert_eq!(form.task, QuickTask::Skill("anh-the-hang-loat".to_string()));
                assert_eq!(form.fields, skill().fields);
            }
            other => panic!("{other:?}"),
        }
    }

    #[cfg(feature = "pro")]
    #[test]
    fn id_photos_come_with_pro_and_a_job_asked_for_is_shown_chosen() {
        let view = |pro, preset| QuickView {
            open: true,
            folder: r"C:\khach".to_string(),
            pro,
            preset,
            ..QuickView::default()
        };
        let asked = staged(view(true, Some((1, QuickTask::IdPhotos))));
        assert!(asked.says("Mỗi người") && asked.says("tấm"));
        match asked.click("Làm").0.chrome.quick {
            Some(QuickIntent::Start(form)) => {
                assert_eq!(form.task, QuickTask::IdPhotos);
                assert_eq!((form.size.as_str(), form.copies), ("3x4", 4));
            }
            other => panic!("{other:?}"),
        }
        let locked = staged(view(false, Some((1, QuickTask::IdPhotos))));
        assert!(locked.says("(Gói Pro)"));
        assert_eq!(locked.click("Làm").0.chrome.quick, None);
    }

    #[test]
    fn a_job_under_way_shows_where_it_stands_and_can_be_stopped() {
        let shown = staged(QuickView {
            open: true,
            folder: r"C:\khach".to_string(),
            run: Some(QuickRunView {
                title: "Làm sạch ảnh chụp giấy tờ · khach".to_string(),
                stage: "Đang làm ảnh 3 / 8…".to_string(),
                running: true,
                failed: false,
                results: Some(r"C:\khach\iAi_ket_qua".to_string()),
            }),
            ..QuickView::default()
        });
        assert!(shown.says("Đang làm ảnh 3 / 8…"));
        assert_eq!(shown.click("Dừng").0.chrome.quick, Some(QuickIntent::Stop));
        assert_eq!(
            shown.click("Mở thư mục kết quả").0.chrome.quick,
            Some(QuickIntent::OpenResults)
        );
        // Nothing new starts while it runs.
        assert_eq!(shown.click("Làm").0.chrome.quick, None);
    }

    /// Opt-in: IAI_UI_SNAPSHOT is a folder; the window is drawn there,
    /// `viec_nhanh_<state>.png`.
    #[test]
    #[ignore]
    fn probe_dialog_snapshot_of_the_quick_window() {
        let Ok(dir) = std::env::var("IAI_UI_SNAPSHOT") else {
            return;
        };
        let base = QuickView {
            open: true,
            folder: r"C:\Users\Admin\Downloads\New folder (11)".to_string(),
            pro: true,
            skills: vec![skill()],
            ..QuickView::default()
        };
        let running = QuickView {
            run: Some(QuickRunView {
                title: "Ảnh thẻ cho nhiều người · New folder (11)".to_string(),
                stage: "Đang làm ảnh thẻ 3 / 12…".to_string(),
                running: true,
                failed: false,
                results: None,
            }),
            ..base.clone()
        };
        let done = QuickView {
            run: Some(QuickRunView {
                title: "Làm sạch ảnh chụp giấy tờ · New folder (11)".to_string(),
                stage: "Xong: 8 ảnh đã làm sạch, lưu trong thư mục iAi_ket_qua.".to_string(),
                running: false,
                failed: false,
                results: Some(r"C:\x".to_string()),
            }),
            ..base.clone()
        };
        for (name, view, task) in [
            ("giay_to", base.clone(), QuickTask::ScanCleanup),
            ("anh_the", base.clone(), QuickTask::IdPhotos),
            (
                "ky_nang",
                base,
                QuickTask::Skill("anh-the-hang-loat".to_string()),
            ),
            ("dang_lam", running, QuickTask::IdPhotos),
            ("xong", done, QuickTask::ScanCleanup),
        ] {
            let mut data = UiData::default();
            data.chrome.quick = view;
            let image = crate::ui::snapshot::render(560.0, 640.0, 1.5, None, |ctx| {
                ctx.data_mut(|d| {
                    d.insert_temp(
                        form_id(),
                        QuickForm {
                            task: task.clone(),
                            ..QuickForm::default()
                        },
                    )
                });
                #[allow(deprecated)]
                egui::CentralPanel::default().show(ctx, |_| {});
                quick_window(ctx, &data, &mut UiActions::default());
            });
            image
                .save(std::path::Path::new(&dir).join(format!("viec_nhanh_{name}.png")))
                .unwrap();
        }
    }
}

//! "Nhờ AI làm": pick a job and the customer's folder, and Gemini (in
//! Antigravity) does it on this window; where it stands, and "Dừng". It
//! does not hold the window: the person watches the work on the canvas.

use super::*;
use crate::ui::{AntigravityState, ErrandForm, ErrandIntent, ErrandJob, QuickIntent, QuickTask};

pub(crate) const ERRAND_TITLE: &str = "Nhờ AI làm";
/// The switch for online AI's hard cases; off, iAi's own AI does all.
const ONLINE_SWITCH: &str = "Dùng AI online cho ca khó (thay áo, xóa chữ, nền quá rối)";
const ERRAND_WIDTH: f32 = 420.0;
const OK_GREEN: egui::Color32 = egui::Color32::from_rgb(40, 160, 70);
const FAIL_RED: egui::Color32 = egui::Color32::from_rgb(200, 60, 60);

fn form_id() -> egui::Id {
    egui::Id::new("errand_form")
}

/// The last job asked for from elsewhere that the window took.
fn preset_id() -> egui::Id {
    egui::Id::new("errand_preset_taken")
}

/// The clothes online AI is to put on (empty keeps them).
fn shirt_combo(ui: &mut egui::Ui, shirt: &mut String) {
    ui.horizontal(|ui| {
        ui.label("Áo");
        let shown = crate::ui::ERRAND_SHIRTS
            .iter()
            .find(|(v, _)| v == shirt)
            .map_or("Giữ nguyên áo", |(_, text)| text);
        egui::ComboBox::from_id_salt("errand_shirt")
            .selected_text(shown)
            .width(170.0)
            .show_ui(ui, |ui| {
                for (v, text) in crate::ui::ERRAND_SHIRTS {
                    ui.selectable_value(shirt, v.to_string(), text);
                }
            });
    });
}

/// A job iAi does by itself on a folder: the way to "Việc nhanh", which
/// needs no AI.
fn quick_offer(ui: &mut egui::Ui, actions: &mut UiActions, task: QuickTask) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Việc này iAi tự làm được, không cần AI:")
                .small()
                .weak(),
        );
        let go = egui::Button::new(
            egui::RichText::new(format!("{}  {}", ph::LIGHTNING, QUICK_TITLE)).small(),
        );
        if ui.add(go).clicked() {
            actions.chrome.quick = Some(QuickIntent::OpenFor(task));
        }
    });
}

/// The jobs this build offers (Pro's only where Pro is built in).
fn jobs() -> impl Iterator<Item = ErrandJob> {
    ErrandJob::ALL
        .into_iter()
        .filter(|j| !j.pro() || cfg!(feature = "pro"))
}

fn combo(ui: &mut egui::Ui, id: &str, value: &mut String, choices: &[(&str, &str)]) {
    let shown = choices
        .iter()
        .find(|(v, _)| v == value)
        .map_or(value.as_str(), |(_, text)| text)
        .to_string();
    egui::ComboBox::from_id_salt(id)
        .selected_text(shown)
        .width(78.0)
        .show_ui(ui, |ui| {
            for (v, text) in choices {
                ui.selectable_value(value, (*v).to_string(), *text);
            }
        });
}

pub(crate) fn errand_window(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let view = &data.chrome.errand;
    let mut form = ctx
        .data(|d| d.get_temp::<ErrandForm>(form_id()))
        .unwrap_or_default();
    if let Some((n, job)) = view.preset
        && ctx.data(|d| d.get_temp::<u64>(preset_id())) != Some(n)
    {
        form.job = job;
        ctx.data_mut(|d| d.insert_temp(preset_id(), n));
    }
    if !jobs().any(|j| j == form.job) {
        form.job = ErrandJob::ScanCleanup;
    }
    let mut open = true;
    let corner = ctx.content_rect().right_top() + egui::vec2(-ERRAND_WIDTH - 40.0, 56.0);
    egui::Window::new(ERRAND_TITLE)
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .fixed_size([ERRAND_WIDTH, 0.0])
        .default_pos(corner)
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            match view.antigravity {
                AntigravityState::Absent => {
                    ui.label(
                        "Tính năng này nhờ Gemini trong Antigravity (ứng dụng AI của Google) làm \
                         việc ngay trên iAi. Máy này chưa có Antigravity: cài và đăng nhập \
                         Antigravity trước, rồi mở lại hộp này.",
                    );
                    ui.add_space(4.0);
                    quick_offer(ui, actions, QuickTask::ScanCleanup);
                    return;
                }
                AntigravityState::NotConnected => {
                    ui.label(
                        "iAi chưa nối với Antigravity. Bấm nút dưới để iAi tự cài (làm một lần). \
                         Antigravity sẽ khởi động lại: việc đang chạy trong Antigravity sẽ dừng.",
                    );
                    ui.add_space(4.0);
                    if ui.button("Nối với Antigravity").clicked() {
                        actions.chrome.errand = Some(ErrandIntent::Connect);
                    }
                    if let Some(note) = &view.connect_note {
                        ui.label(egui::RichText::new(note).small().color(FAIL_RED));
                    }
                    return;
                }
                AntigravityState::Ready => {}
            }
            if let Some(note) = &view.connect_note {
                ui.label(egui::RichText::new(note).small().weak());
            }
            let running = view.run.as_ref().is_some_and(|r| r.running);
            ui.add_enabled_ui(!running, |ui| {
                ui.label(egui::RichText::new("Việc").strong());
                for job in jobs() {
                    let locked = job.pro() && !view.pro;
                    let text = if locked {
                        format!("{} (Gói Pro)", job.label())
                    } else {
                        job.label().to_string()
                    };
                    ui.add_enabled_ui(!locked, |ui| ui.radio_value(&mut form.job, job, text));
                }
                ui.add_space(6.0);
                if form.job.on_open_photo() {
                    let dress = form.job == ErrandJob::DressIdPhoto;
                    let heading = if dress {
                        "Ảnh thẻ"
                    } else {
                        "Ảnh cần sửa"
                    };
                    ui.label(egui::RichText::new(heading).strong());
                    let photo = match &view.photo {
                        Some(title) => egui::RichText::new(title.as_str()).small(),
                        None => egui::RichText::new("Chưa mở ảnh nào: mở ảnh cần sửa trước").weak(),
                    };
                    ui.add(egui::Label::new(photo).truncate());
                    let about = if dress {
                        "Gemini vẽ áo và phần vai bị thiếu; iAi đặt lại khớp đúng khuôn mặt: từ \
                         cằm trở lên vẫn là ảnh gốc. Kết quả là lớp mới có mặt nạ, một bản lưu \
                         trong thư mục iAi_ket_qua cạnh ảnh."
                    } else {
                        "Ảnh AI vẽ về thành lớp mới trên chính ảnh này (che mặt nạ để giữ \
                         lại phần nào của ảnh gốc), một bản lưu trong thư mục iAi_ket_qua cạnh \
                         ảnh. Xem kỹ khuôn mặt trước khi in."
                    };
                    ui.label(egui::RichText::new(about).small().weak());
                    if dress {
                        ui.add_space(4.0);
                        shirt_combo(ui, &mut form.shirt);
                        if form.shirt.is_empty() {
                            ui.label(
                                egui::RichText::new(
                                    "Giữ áo khách đang mặc: chỉ lấp phần vai, thân bị thiếu.",
                                )
                                .small()
                                .weak(),
                            );
                        }
                    }
                } else {
                    ui.label(egui::RichText::new("Thư mục ảnh của khách").strong());
                    ui.horizontal(|ui| {
                        if ui.button("Chọn thư mục…").clicked() {
                            actions.chrome.errand = Some(ErrandIntent::PickFolder);
                        }
                        let folder = if view.folder.is_empty() {
                            egui::RichText::new("Chưa chọn").weak()
                        } else {
                            egui::RichText::new(&view.folder).small()
                        };
                        ui.add(egui::Label::new(folder).truncate());
                    });
                }
                if form.job == ErrandJob::IdPhotos {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label("Cỡ");
                        combo(
                            ui,
                            "errand_size",
                            &mut form.size,
                            &[("3x4", "3×4"), ("2x3", "2×3"), ("4x6", "4×6")],
                        );
                        ui.label("Nền");
                        combo(
                            ui,
                            "errand_backdrop",
                            &mut form.backdrop,
                            &[("white", "Trắng"), ("blue", "Xanh")],
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Giấy");
                        combo(
                            ui,
                            "errand_paper",
                            &mut form.paper,
                            &[("10x15", "10×15"), ("13x18", "13×18")],
                        );
                        ui.label("Mỗi người");
                        ui.add(egui::DragValue::new(&mut form.copies).range(1..=40));
                        ui.label("tấm");
                    });
                }
                let by_itself = match form.job {
                    ErrandJob::ScanCleanup => Some(QuickTask::ScanCleanup),
                    ErrandJob::IdPhotos if !form.online => Some(QuickTask::IdPhotos),
                    _ => None,
                };
                if let Some(task) = by_itself {
                    ui.add_space(2.0);
                    quick_offer(ui, actions, task);
                }
                if form.job.online_ok() {
                    ui.add_space(4.0);
                    ui.checkbox(&mut form.online, ONLINE_SWITCH);
                    if form.online {
                        ui.label(
                            egui::RichText::new(
                                "Gemini vẽ lại những ảnh này: xem kỹ khuôn mặt trước khi in. \
                                 Ảnh khác vẫn làm bằng AI offline của iAi.",
                            )
                            .small()
                            .weak(),
                        );
                        if form.job == ErrandJob::IdPhotos {
                            shirt_combo(ui, &mut form.shirt);
                        }
                    }
                }
                ui.add_space(6.0);
                let hint = match form.job {
                    ErrandJob::Other => {
                        "Viết việc cần làm, ví dụ: ghép 4 ảnh thành một tờ A4 rồi xuất PDF"
                    }
                    ErrandJob::EditPhoto => {
                        "Sửa gì, ví dụ: thay áo sơ mi trắng, nền xanh trơn; xóa chữ trên áo"
                    }
                    ErrandJob::DressIdPhoto => "Dặn thêm (không bắt buộc), ví dụ: áo cổ tròn",
                    _ => "Dặn thêm (không bắt buộc), ví dụ: chị Lan 8 tấm",
                };
                ui.add(
                    egui::TextEdit::multiline(&mut form.note)
                        .hint_text(hint)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(6.0);
                let needs_words = matches!(form.job, ErrandJob::Other | ErrandJob::EditPhoto);
                let has_input = if form.job.on_open_photo() {
                    view.photo.is_some()
                } else {
                    !view.folder.is_empty()
                };
                let ready = has_input
                    && (!needs_words || !form.note.trim().is_empty())
                    && (!form.job.pro() || view.pro);
                let send = egui::Button::new(
                    egui::RichText::new(format!("{}  Gửi cho Gemini", ph::PAPER_PLANE_TILT))
                        .strong(),
                );
                if ui.add_enabled(ready, send).clicked() {
                    actions.chrome.errand = Some(ErrandIntent::Send(form.clone()));
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
                if run.steps > 0 {
                    ui.label(
                        egui::RichText::new(format!("{} bước · vừa làm: {}", run.steps, run.last))
                            .small()
                            .weak(),
                    );
                }
                ui.horizontal(|ui| {
                    if run.running && ui.button("Dừng").clicked() {
                        actions.chrome.errand = Some(ErrandIntent::Stop);
                    }
                    if run.results.is_some() && ui.button("Mở thư mục kết quả").clicked() {
                        actions.chrome.errand = Some(ErrandIntent::OpenResults);
                    }
                });
                if run.running {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(500));
                }
            }
        });
    ctx.data_mut(|d| d.insert_temp(form_id(), form));
    if !open {
        actions.chrome.errand = Some(ErrandIntent::Close);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::dialogs::stage::Stage;
    use crate::ui::{ErrandRunView, ErrandView};

    fn staged(view: ErrandView) -> Stage {
        let mut data = UiData::default();
        data.chrome.errand = view;
        Stage::on(data, egui::vec2(1000.0, 800.0), errand_window)
    }

    #[test]
    fn it_asks_to_connect_first_then_sends_the_job_on_the_folder() {
        let absent = staged(ErrandView {
            open: true,
            ..ErrandView::default()
        });
        assert!(absent.says("chưa có Antigravity"));

        let unconnected = staged(ErrandView {
            open: true,
            antigravity: AntigravityState::NotConnected,
            ..ErrandView::default()
        });
        assert_eq!(
            unconnected.click("Nối với Antigravity").0.chrome.errand,
            Some(ErrandIntent::Connect)
        );

        let ready = |folder: &str| {
            staged(ErrandView {
                open: true,
                antigravity: AntigravityState::Ready,
                folder: folder.to_string(),
                pro: true,
                ..ErrandView::default()
            })
        };
        // No folder yet: nothing is sent.
        let empty = ready("");
        assert!(empty.says("Chưa chọn"));
        assert_eq!(empty.click("Gửi cho Gemini").0.chrome.errand, None);
        assert_eq!(
            empty.click("Chọn thư mục…").0.chrome.errand,
            Some(ErrandIntent::PickFolder)
        );
        // A folder: the job picked goes.
        let chosen = ready(r"C:\khach");
        chosen.click(ErrandJob::ScanCleanup.label());
        let sent = chosen.click("Gửi cho Gemini").0.chrome.errand;
        match sent {
            Some(ErrandIntent::Send(form)) => assert_eq!(form.job, ErrandJob::ScanCleanup),
            other => panic!("{other:?}"),
        }
        // "Việc khác" needs words first.
        chosen.click(ErrandJob::Other.label());
        assert!(chosen.says("Viết việc cần làm"));
        assert_eq!(chosen.click("Gửi cho Gemini").0.chrome.errand, None);
    }

    #[test]
    fn online_ai_is_a_switch_off_by_default_and_never_for_id_cards() {
        let shown = staged(ErrandView {
            open: true,
            antigravity: AntigravityState::Ready,
            folder: r"C:\khach".to_string(),
            pro: true,
            ..ErrandView::default()
        });
        // ID cards (scans, in a build without Pro): no switch.
        assert!(!shown.says(ONLINE_SWITCH));
        // ID photos come with Pro.
        #[cfg(feature = "pro")]
        {
            shown.click(ErrandJob::IdPhotos.label());
            assert!(shown.says(ONLINE_SWITCH) && !shown.says("Giữ nguyên áo"));
            let off = shown.click("Gửi cho Gemini").0.chrome.errand;
            assert!(matches!(off, Some(ErrandIntent::Send(f)) if !f.online));
            shown.click(ONLINE_SWITCH);
            assert!(shown.says("Giữ nguyên áo") && shown.says("xem kỹ khuôn mặt"));
            let on = shown.click("Gửi cho Gemini").0.chrome.errand;
            assert!(matches!(on, Some(ErrandIntent::Send(f)) if f.online && f.shirt.is_empty()));
        }
        // "Việc khác", in every build: the switch, no clothes to pick.
        shown.click(ErrandJob::Other.label());
        assert!(shown.says(ONLINE_SWITCH) && !shown.says("Giữ nguyên áo"));
        shown.click(ErrandJob::ScanCleanup.label());
        assert!(!shown.says(ONLINE_SWITCH));
    }

    #[test]
    fn the_open_photo_is_edited_with_words_and_no_folder() {
        let staged_with = |photo: Option<&str>| {
            staged(ErrandView {
                open: true,
                antigravity: AntigravityState::Ready,
                photo: photo.map(str::to_string),
                ..ErrandView::default()
            })
        };
        // No photo open: nothing to send.
        let none = staged_with(None);
        none.click(ErrandJob::EditPhoto.label());
        assert!(none.says("Chưa mở ảnh nào") && !none.says("Chọn thư mục…"));
        assert_eq!(none.click("Gửi cho Gemini").0.chrome.errand, None);
        // A photo: the words first, then it goes; no folder is asked for.
        let open = staged_with(Some("chi_lan.jpg"));
        open.click(ErrandJob::EditPhoto.label());
        assert!(open.says("chi_lan.jpg") && open.says("lớp mới"));
        assert!(!open.says(ONLINE_SWITCH));
        assert_eq!(open.click("Gửi cho Gemini").0.chrome.errand, None);
        open.click("Sửa gì, ví dụ: thay áo sơ mi trắng, nền xanh trơn; xóa chữ trên áo");
        open.type_text("thay áo sơ mi trắng");
        match open.click("Gửi cho Gemini").0.chrome.errand {
            Some(ErrandIntent::Send(form)) => {
                assert_eq!(form.job, ErrandJob::EditPhoto);
                assert_eq!(form.note, "thay áo sơ mi trắng");
            }
            other => panic!("{other:?}"),
        }
    }

    #[cfg(feature = "pro")]
    #[test]
    fn an_id_photo_is_dressed_on_the_window_shown_chosen_when_asked_for() {
        let view = |preset| ErrandView {
            open: true,
            antigravity: AntigravityState::Ready,
            photo: Some("chi_lan.jpg".to_string()),
            pro: true,
            preset,
            ..ErrandView::default()
        };
        let asked = staged(view(Some((1, ErrandJob::DressIdPhoto))));
        assert!(asked.says("Ảnh thẻ") && asked.says("chi_lan.jpg"));
        assert!(asked.says("Giữ nguyên áo") && asked.says("chỉ lấp phần vai"));
        assert!(!asked.says(ONLINE_SWITCH) && !asked.says("Chọn thư mục…"));
        // No words needed: it goes as it is, keeping the clothes.
        match asked.click("Gửi cho Gemini").0.chrome.errand {
            Some(ErrandIntent::Send(form)) => {
                assert_eq!(form.job, ErrandJob::DressIdPhoto);
                assert!(form.shirt.is_empty());
            }
            other => panic!("{other:?}"),
        }
        // Once taken, the person's own pick stands.
        asked.click(ErrandJob::ScanCleanup.label());
        assert!(!asked.says("chỉ lấp phần vai"));
        // Without Pro it is not offered.
        let locked = staged(ErrandView {
            pro: false,
            ..view(Some((1, ErrandJob::DressIdPhoto)))
        });
        assert_eq!(locked.click("Gửi cho Gemini").0.chrome.errand, None);
    }

    #[test]
    fn an_errand_under_way_shows_where_it_stands_and_can_be_stopped() {
        let shown = staged(ErrandView {
            open: true,
            antigravity: AntigravityState::Ready,
            folder: r"C:\khach".to_string(),
            run: Some(ErrandRunView {
                title: "iAi · In căn cước lên A4 · khach".to_string(),
                stage: "Gemini đang làm trên iAi".to_string(),
                steps: 3,
                last: "Cắt phối cảnh".to_string(),
                running: true,
                failed: false,
                results: Some(r"C:\khach\iAi_ket_qua".to_string()),
            }),
            ..ErrandView::default()
        });
        assert!(
            shown.says("Gemini đang làm trên iAi") && shown.says("3 bước · vừa làm: Cắt phối cảnh")
        );
        assert_eq!(
            shown.click("Dừng").0.chrome.errand,
            Some(ErrandIntent::Stop)
        );
        assert_eq!(
            shown.click("Mở thư mục kết quả").0.chrome.errand,
            Some(ErrandIntent::OpenResults)
        );
        // Nothing new is sent while it runs.
        assert_eq!(shown.click("Gửi cho Gemini").0.chrome.errand, None);
    }

    /// Opt-in: IAI_UI_SNAPSHOT is a folder; the window is drawn there,
    /// `nho_ai_lam_<state>.png`.
    #[test]
    #[ignore]
    fn probe_dialog_snapshot_of_the_errand_window() {
        let Ok(dir) = std::env::var("IAI_UI_SNAPSHOT") else {
            return;
        };
        let base = ErrandView {
            open: true,
            antigravity: AntigravityState::Ready,
            folder: r"C:\Users\Admin\Downloads\New folder (11)".to_string(),
            pro: true,
            ..ErrandView::default()
        };
        let running = ErrandView {
            run: Some(ErrandRunView {
                title: "iAi · In căn cước lên A4 · New folder (11)".to_string(),
                stage: "Gemini đang làm trên iAi".to_string(),
                steps: 7,
                last: "Xếp căn cước lên A4".to_string(),
                running: true,
                failed: false,
                results: Some(r"C:\x".to_string()),
            }),
            ..base.clone()
        };
        let unconnected = ErrandView {
            antigravity: AntigravityState::NotConnected,
            ..base.clone()
        };
        for (name, view, job, online) in [
            ("san_sang", base.clone(), ErrandJob::IdCards, false),
            ("anh_the", base.clone(), ErrandJob::IdPhotos, false),
            ("anh_the_online", base.clone(), ErrandJob::IdPhotos, true),
            (
                "thay_ao",
                ErrandView {
                    photo: Some("chi_lan.jpg".to_string()),
                    ..base
                },
                ErrandJob::DressIdPhoto,
                false,
            ),
            ("dang_lam", running, ErrandJob::IdCards, false),
            ("chua_noi", unconnected, ErrandJob::IdCards, false),
        ] {
            let mut data = UiData::default();
            data.chrome.errand = view;
            let image = crate::ui::snapshot::render(560.0, 640.0, 1.5, None, |ctx| {
                ctx.data_mut(|d| {
                    d.insert_temp(
                        form_id(),
                        ErrandForm {
                            job,
                            online,
                            shirt: if online { "white_shirt" } else { "" }.to_string(),
                            ..ErrandForm::default()
                        },
                    )
                });
                #[allow(deprecated)]
                egui::CentralPanel::default().show(ctx, |_| {});
                errand_window(ctx, &data, &mut UiActions::default());
            });
            image
                .save(std::path::Path::new(&dir).join(format!("nho_ai_lam_{name}.png")))
                .unwrap();
        }
    }
}

//! "Cho AI điều khiển iAi": the switch that lets an AI agent on this
//! machine (Claude Desktop, Claude Code…) work in the window, and what to
//! paste into that agent's settings to reach iAi.

use super::*;

const AGENT_DIALOG_WIDTH: f32 = 540.0;
pub(crate) const AGENT_TITLE: &str = "Cho AI điều khiển iAi";

/// The settings an MCP client takes to start iAi for an agent: on the
/// window that is open (`bridge`), or on its own; `folder` is where it may
/// read and write files.
pub(crate) fn mcp_config(exe: &str, bridge: bool, folder: &str) -> String {
    let mut args = vec!["mcp".to_string()];
    if bridge {
        args.push("--bridge".to_string());
    }
    if !folder.trim().is_empty() {
        args.push("--write-root".to_string());
        args.push(folder.trim().to_string());
    }
    let config = serde_json::json!({
        "mcpServers": {"iai": {"command": exe, "args": args}}
    });
    serde_json::to_string_pretty(&config).unwrap_or_default()
}

/// The folder offered at first: the user's Pictures.
fn default_folder() -> String {
    std::env::var_os("USERPROFILE")
        .map(|home| PathBuf::from(home).join("Pictures").display().to_string())
        .unwrap_or_default()
}

pub(crate) fn agent_dialog(ctx: &egui::Context, data: &UiData, actions: &mut UiActions) {
    let typing = crate::ui::widgets::typing_in_a_field(ctx, egui::Id::new("agent_typing"));
    // The window that removes a skill, while open, takes Escape itself.
    let removing = ctx.data(|d| d.get_temp::<String>(removing_id()).is_some());
    let esc =
        !removing && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    let mut close = esc && !typing;
    let view = &data.chrome.agent;
    let folder_id = egui::Id::new("agent_dialog_folder");
    let mut folder = ctx
        .data(|d| d.get_temp::<String>(folder_id))
        .unwrap_or_else(default_folder);
    modal_overlay(ctx, "agent_dialog_overlay");
    egui::Window::new(AGENT_TITLE)
        .collapsible(false)
        .resizable(false)
        .fixed_size([AGENT_DIALOG_WIDTH, 0.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(DIALOG_ORDER)
        .show(ctx, |ui| {
            ui.label(
                "Khi bật, trợ lý AI trên máy này (Claude Desktop, Claude Code…) làm việc \
                 được trên đúng các ảnh đang mở trong iAi: mở ảnh, tách nền, làm ảnh thẻ, \
                 xếp ảnh in, xuất PDF… Bạn thấy từng bước trên màn hình và Ctrl+Z được. \
                 AI không được in và không được ra mạng.",
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let (text, color) = if view.on {
                    ("● Đang bật", egui::Color32::from_rgb(40, 160, 70))
                } else {
                    ("○ Đang tắt", ui.visuals().weak_text_color())
                };
                ui.label(egui::RichText::new(text).color(color).strong());
                ui.add_space(12.0);
                let button = if view.on { "Tắt" } else { "Bật" };
                if ui.button(button).clicked() {
                    actions.chrome.agent_control = Some(!view.on);
                }
            });
            if let Some(error) = &view.error {
                ui.colored_label(egui::Color32::from_rgb(200, 60, 60), error);
            }
            ui.add_space(8.0);
            ui.separator();
            ui.label(egui::RichText::new("Cài cho trợ lý AI (làm một lần)").strong());
            ui.label("Thư mục AI được đọc ảnh và ghi kết quả (ngoài thư mục này AI không đụng tới file nào):");
            ui.add(egui::TextEdit::singleline(&mut folder).desired_width(f32::INFINITY));
            ui.add_space(4.0);
            ui.label(
                "Dán đoạn dưới vào phần MCP của trợ lý AI (Claude Desktop: Settings > \
                 Developer > Edit Config), rồi mở lại trợ lý AI đó.",
            );
            let config = mcp_config(&view.exe, true, &folder);
            ui.add(
                egui::TextEdit::multiline(&mut config.as_str())
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY)
                    .desired_rows(9),
            );
            ui.horizontal(|ui| {
                if ui.button("Chép đoạn này").clicked() {
                    ui.ctx().copy_text(config.clone());
                }
                ui.label(
                    egui::RichText::new("Bỏ dòng \"--bridge\" thì AI làm việc riêng, không cần mở iAi.")
                        .small()
                        .weak(),
                );
            });
            ui.add_space(8.0);
            ui.separator();
            skills_section(ui, view, actions);
            ui.add_space(8.0);
            if ui.button("Đóng").clicked() {
                close = true;
            }
        });
    ctx.data_mut(|d| d.insert_temp(folder_id, folder));
    remove_skill_window(ctx, view, actions);
    if close {
        actions.chrome.close_support = true;
    }
}

const SKILLS_TITLE: &str = "Kỹ năng đã lưu";
pub(crate) const REMOVE_SKILL: &str = "Xóa kỹ năng\u{2026}";
const REMOVE_TITLE: &str = "Xóa kỹ năng";

fn removing_id() -> egui::Id {
    egui::Id::new("agent_remove_skill")
}

/// The skills agents have saved: newest use first, and the line that opens
/// the window to take one out (no delete button beside the names).
fn skills_section(ui: &mut egui::Ui, view: &crate::ui::AgentView, actions: &mut UiActions) {
    ui.label(egui::RichText::new(format!("{SKILLS_TITLE} ({})", view.skills.len())).strong());
    ui.label(
        "Mỗi việc AI làm xong và đạt, iAi tự lưu thành một kỹ năng. Lần sau gặp việc tương tự, \
         AI lấy kỹ năng đó ra dùng ngay, không phải làm lại từ đầu.",
    );
    ui.add_space(4.0);
    if view.skills.is_empty() {
        ui.label(egui::RichText::new("Chưa có kỹ năng nào.").weak());
    } else {
        egui::ScrollArea::vertical()
            .id_salt("agent_skills_list")
            .max_height(132.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for s in &view.skills {
                    ui.horizontal(|ui| {
                        ui.label(&s.title);
                        let detail = if s.when.is_empty() {
                            s.status.clone()
                        } else {
                            format!("{} · {}", s.status, s.when)
                        };
                        ui.label(egui::RichText::new(detail).small().weak());
                    });
                }
            });
        if ui.selectable_label(false, REMOVE_SKILL).clicked() {
            ui.ctx()
                .data_mut(|d| d.insert_temp(removing_id(), String::new()));
        }
    }
    ui.horizontal(|ui| {
        if ui.button("Mở thư mục kỹ năng").clicked() {
            actions.chrome.open_skills_folder = true;
        }
        if !view.skills_dir.is_empty() {
            ui.label(egui::RichText::new(&view.skills_dir).small().weak());
        }
    });
}

/// The window that takes one skill out: pick it, then Xóa.
fn remove_skill_window(ctx: &egui::Context, view: &crate::ui::AgentView, actions: &mut UiActions) {
    let Some(mut picked) = ctx.data(|d| d.get_temp::<String>(removing_id())) else {
        return;
    };
    let mut done = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    egui::Window::new(REMOVE_TITLE)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.label("Chọn kỹ năng cần xóa (iAi cất nó sang thư mục .da_xoa, lấy lại được):");
            egui::ScrollArea::vertical()
                .id_salt("agent_remove_list")
                .max_height(220.0)
                .show(ui, |ui| {
                    for s in &view.skills {
                        ui.radio_value(&mut picked, s.name.clone(), &s.title);
                    }
                });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let chosen = !picked.is_empty();
                if ui.add_enabled(chosen, egui::Button::new("Xóa")).clicked() {
                    actions.chrome.remove_skill = Some(picked.clone());
                    done = true;
                }
                if ui.button("Hủy").clicked() {
                    done = true;
                }
            });
        });
    ctx.data_mut(|d| {
        if done {
            d.remove::<String>(removing_id());
        } else {
            d.insert_temp(removing_id(), picked);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_config_names_the_exe_and_the_folder() {
        let text = mcp_config(r"C:\iAi\iai.exe", true, r" D:\in ");
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let server = &v["mcpServers"]["iai"];
        assert_eq!(server["command"], r"C:\iAi\iai.exe");
        assert_eq!(
            server["args"],
            serde_json::json!(["mcp", "--bridge", "--write-root", r"D:\in"])
        );
        let alone: serde_json::Value =
            serde_json::from_str(&mcp_config("iai.exe", false, "")).unwrap();
        assert_eq!(
            alone["mcpServers"]["iai"]["args"],
            serde_json::json!(["mcp"])
        );
    }

    #[test]
    fn the_switch_turns_it_on_and_off_and_escape_closes() {
        use crate::ui::dialogs::stage::Stage;
        let staged = |on: bool| {
            let mut data = UiData::default();
            data.chrome.agent.on = on;
            data.chrome.agent.exe = r"C:\iAi\iai.exe".to_string();
            Stage::on(data, egui::vec2(900.0, 700.0), agent_dialog)
        };
        let off = staged(false);
        assert!(off.says("Đang tắt") && off.says("--bridge"));
        assert_eq!(off.click("Bật").0.chrome.agent_control, Some(true));
        let on = staged(true);
        assert!(on.says("Đang bật"));
        assert_eq!(on.click("Tắt").0.chrome.agent_control, Some(false));
        assert!(on.click("Đóng").0.chrome.close_support);
        assert!(on.key(egui::Key::Escape).chrome.close_support);
    }

    fn rows() -> Vec<crate::ui::SkillRow> {
        vec![
            crate::ui::SkillRow {
                name: "anh-the-hang-loat".to_string(),
                title: "Ảnh thẻ hàng loạt → tờ in → PDF".to_string(),
                status: "đã dùng 3 lần".to_string(),
                when: "20/10/2026 10:31".to_string(),
            },
            crate::ui::SkillRow {
                name: "thiep-20-10".to_string(),
                title: "Thiệp 20/10 tô màu cho cả lớp".to_string(),
                status: "tự lưu".to_string(),
                when: "19/10/2026 23:28".to_string(),
            },
        ]
    }

    #[test]
    fn saved_skills_are_listed_and_one_is_taken_out_through_its_own_window() {
        use crate::ui::dialogs::stage::Stage;
        let staged = |skills: Vec<crate::ui::SkillRow>| {
            let mut data = UiData::default();
            data.chrome.agent.exe = r"C:\iAi\iai.exe".to_string();
            data.chrome.agent.skills = skills;
            Stage::on(data, egui::vec2(900.0, 800.0), agent_dialog)
        };
        let empty = staged(Vec::new());
        assert!(empty.says("Kỹ năng đã lưu (0)") && empty.says("Chưa có kỹ năng nào."));
        assert!(!empty.says(REMOVE_SKILL));
        assert!(
            empty
                .click("Mở thư mục kỹ năng")
                .0
                .chrome
                .open_skills_folder
        );

        let shown = staged(rows());
        assert!(shown.says("Kỹ năng đã lưu (2)"));
        assert!(shown.says("Thiệp 20/10 tô màu cho cả lớp") && shown.says("đã dùng 3 lần"));
        // The line under the list opens the window; nothing is removed yet.
        let opened = shown.click(REMOVE_SKILL).0;
        assert_eq!(opened.chrome.remove_skill, None);
        assert!(shown.says("Chọn kỹ năng cần xóa"));
        // "Xóa" does nothing until a skill is picked.
        assert_eq!(shown.click("Xóa").0.chrome.remove_skill, None);
        // The title as the window draws it (over the list, drawn last).
        let at = shown
            .texts()
            .iter()
            .rev()
            .find(|(text, _)| text.trim() == "Thiệp 20/10 tô màu cho cả lớp")
            .map(|(_, pos)| *pos + egui::vec2(6.0, 5.0))
            .unwrap();
        let press = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        shown.frame(vec![egui::Event::PointerMoved(at)]);
        shown.frame(vec![press(true)]);
        shown.frame(vec![press(false)]);
        assert_eq!(
            shown.click("Xóa").0.chrome.remove_skill.as_deref(),
            Some("thiep-20-10")
        );
        assert!(!shown.says("Chọn kỹ năng cần xóa"));
        // Escape in the window closes it, not the dialog.
        shown.click(REMOVE_SKILL);
        assert!(!shown.key(egui::Key::Escape).chrome.close_support);
        assert!(!shown.says("Chọn kỹ năng cần xóa"));
    }

    /// Opt-in: IAI_UI_SNAPSHOT is a folder; the dialog is drawn there off
    /// and on, `ai_dieu_khien_<state>.png`.
    #[test]
    #[ignore]
    fn probe_dialog_snapshot_of_the_agent_dialog() {
        let Ok(dir) = std::env::var("IAI_UI_SNAPSHOT") else {
            return;
        };
        for (name, on, error) in [
            ("tat", false, None),
            ("bat", true, None),
            (
                "loi",
                false,
                Some("Không bật được: cổng bị chặn".to_string()),
            ),
        ] {
            let mut data = UiData::default();
            data.chrome.support = Some(crate::ui::SupportDialog::Agent);
            data.chrome.agent = crate::ui::AgentView {
                on,
                error,
                exe: r"C:\Users\Admin\Documents\IAI\target\release\iai.exe".to_string(),
                skills_dir: r"C:\Users\Admin\Documents\iAi_Ky_nang".to_string(),
                skills: if on { rows() } else { Vec::new() },
            };
            let image = crate::ui::snapshot::render(760.0, 860.0, 1.5, None, |ctx| {
                #[allow(deprecated)]
                egui::CentralPanel::default().show(ctx, |_| {});
                agent_dialog(ctx, &data, &mut UiActions::default());
            });
            image
                .save(std::path::Path::new(&dir).join(format!("ai_dieu_khien_{name}.png")))
                .unwrap();
        }
    }
}

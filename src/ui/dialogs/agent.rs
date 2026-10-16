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
    let esc = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
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
            if ui.button("Đóng").clicked() {
                close = true;
            }
        });
    ctx.data_mut(|d| d.insert_temp(folder_id, folder));
    if close {
        actions.chrome.close_support = true;
    }
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
            };
            let image = crate::ui::snapshot::render(760.0, 680.0, 1.5, None, |ctx| {
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

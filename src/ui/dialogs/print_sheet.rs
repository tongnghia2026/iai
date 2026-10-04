//! "Xếp ảnh in": the section that lays the open photo out on a print sheet,
//! shown in the AI panel and in Chỉnh chân dung. Its options are remembered
//! in prefs.json (key `print_sheet`).

use super::*;
use crate::core::imposition::{Backdrop, Paper, PhotoKind, Sheet, SheetOptions};

const PREFS_KEY: &str = "print_sheet";

/// The section. Returns the sheet to make once one of its buttons is clicked.
pub(crate) fn print_sheet_section(
    ui: &mut egui::Ui,
    data: &UiData,
) -> Option<(Sheet, SheetOptions)> {
    let id = egui::Id::new("print_sheet_options");
    let saved: SheetOptions = ui
        .ctx()
        .data_mut(|d| d.get_temp(id))
        .unwrap_or_else(|| load_pref(PREFS_KEY).unwrap_or_default());
    let mut options = saved;
    let mut settled = false;

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
            "Ảnh hiện tại không đúng cỡ 2×3 / 3×4 (2,8×3,8 cm) / 4×6 — vẫn xếp được, ảnh sẽ được co về đúng ô."
                .to_string()
        }
        None => "Hãy mở ảnh đã crop trước.".to_string(),
    };
    ui.label(egui::RichText::new(info).small().weak());

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Khe cắt").small().weak());
        let mut gap = options.gap as i64;
        let r = ui.add(egui::DragValue::new(&mut gap).range(0..=100).suffix(" px"));
        options.gap = gap.clamp(0, 100) as u32;
        settled |= r.drag_stopped() || r.lost_focus();
    });

    // One button per size and paper: the row is the size, the column the
    // paper, the button says how many copies fit.
    let mut chosen = None;
    let label_w = 30.0;
    let spacing = ui.spacing().item_spacing.x;
    let button_w = ((ui.available_width() - label_w - 2.0 * spacing) / 2.0).max(40.0);
    ui.horizontal(|ui| {
        ui.allocate_space(egui::vec2(label_w, 1.0));
        for paper in Paper::ALL {
            ui.allocate_ui_with_layout(
                egui::vec2(button_w, 14.0),
                egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                |ui| {
                    ui.label(
                        egui::RichText::new(format!("Giấy {}", paper.label()))
                            .small()
                            .weak(),
                    );
                },
            );
        }
    });
    for kind in PhotoKind::ALL {
        ui.horizontal(|ui| {
            ui.add_sized([label_w, 24.0], egui::Label::new(kind.label()));
            for paper in Paper::ALL {
                let sheet = Sheet::Grid(paper, kind);
                let count = sheet.count(options.gap);
                let text = if count > 0 {
                    format!("{count} tấm")
                } else {
                    "—".to_string()
                };
                let button = egui::Button::new(egui::RichText::new(text).strong())
                    .min_size(egui::vec2(button_w, 24.0));
                if ui
                    .add_enabled(data.doc.has_doc && count > 0, button)
                    .clicked()
                {
                    chosen = Some(sheet);
                }
            }
        });
    }
    ui.label(
        egui::RichText::new("Trang một cỡ in đúng nền của ảnh.")
            .small()
            .weak(),
    );

    ui.add_space(4.0);
    let copies = Sheet::Mixed.copies(options.gap);
    let text = match &copies {
        Some(copies) => format!("Ghép {}: {copies}", Sheet::Mixed.paper().label()),
        None => "Không ghép được — khe cắt quá lớn".to_string(),
    };
    let button = egui::Button::new(egui::RichText::new(text).strong())
        .min_size(egui::vec2(ui.available_width(), 24.0));
    if ui
        .add_enabled(data.doc.has_doc && copies.is_some(), button)
        .clicked()
    {
        chosen = Some(Sheet::Mixed);
    }
    ui.horizontal_wrapped(|ui| {
        for (label, backdrop) in [
            ("Nền 3×4", &mut options.backdrop_3x4),
            ("Nền 4×6", &mut options.backdrop_4x6),
        ] {
            ui.label(egui::RichText::new(label).small().weak());
            for choice in [Backdrop::Blue, Backdrop::White] {
                if ui
                    .selectable_label(*backdrop == choice, choice.label())
                    .clicked()
                {
                    *backdrop = choice;
                    settled = true;
                }
            }
            ui.add_space(6.0);
        }
    })
    .response
    .on_hover_text(
        "Trang ghép đặt người lên nền riêng của từng cỡ khi người đã tách khỏi nền. Ảnh chưa tách nền thì giữ nguyên nền của ảnh",
    );
    ui.label(
        egui::RichText::new(
            "Ảnh nền trắng có viền đỏ để cắt. Mỗi tấm là một layer riêng — dùng Move tool để tự sắp xếp lại.",
        )
        .small()
        .weak(),
    );

    if options != saved {
        ui.ctx().data_mut(|d| d.insert_temp(id, options));
    }
    if settled || chosen.is_some() {
        save_pref(PREFS_KEY, &options);
    }
    chosen.map(|sheet| (sheet, options))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_section_draws_and_asks_for_nothing_until_a_sheet_is_clicked() {
        let mut data = UiData::default();
        data.doc.has_doc = true;
        (data.doc.canvas_w, data.doc.canvas_h, data.doc.canvas_dpi) = (661, 898, 600.0);
        let ctx = egui::Context::default();
        let mut asked = None;
        for _ in 0..2 {
            let _ = ctx.run_ui(Default::default(), |ui| {
                ui.set_max_width(300.0);
                asked = print_sheet_section(ui, &data);
            });
        }
        assert!(asked.is_none());
    }
}

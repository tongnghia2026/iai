//! Document mode (thử nghiệm) — a lightweight in-app word-processor surface.
//!
//! Self-contained in the UI layer: all state lives in a thread-local, because
//! the main-window egui frame always runs on the winit main thread. That keeps
//! the feature free of app-state / UiData / UiActions plumbing — the only hooks
//! are one call from `ui::build` and a menu toggle.
//!
//! You type into an egui multiline `TextEdit` (so IME / Vietnamese Telex comes
//! for free from egui-winit, same as any text field here) and see a live A4
//! page rendered by [`crate::core::text_layout`], with automatic pagination.
//! Rich inline editing directly on the page is a later iteration; v1 edits plain
//! text and formats the whole document (font size + alignment) uniformly.

use std::cell::RefCell;

use cosmic_text::{FontSystem, SwashCache};

use crate::core::text_document::{PaperSize, ParagraphAlign, TextDocument};
use crate::core::text_layout::DocumentLayout;

const DPI: f32 = 96.0;

struct DocRuntime {
    active: bool,
    text: String,
    align: ParagraphAlign,
    font_pt: f32,
    paper: PaperSize,
    page_index: usize,
    page_count: usize,
    /// Lazily created on first render (FontSystem scans the OS font dir ~60ms).
    engine: Option<(FontSystem, SwashCache)>,
    /// Cached flow keyed by the content hash (text + font + align + paper).
    layout: Option<(u64, DocumentLayout)>,
    /// Cached page texture keyed by `(content hash, page index)`.
    tex: Option<(u64, usize, egui::TextureHandle)>,
    preview_px: [f32; 2],
}

impl Default for DocRuntime {
    fn default() -> Self {
        Self {
            active: false,
            text: SAMPLE_TEXT.to_string(),
            align: ParagraphAlign::Left,
            font_pt: 13.0,
            paper: PaperSize::A4,
            page_index: 0,
            page_count: 1,
            engine: None,
            layout: None,
            tex: None,
            preview_px: [1.0, 1.0],
        }
    }
}

const SAMPLE_TEXT: &str = "CỘNG HÒA XÃ HỘI CHỦ NGHĨA VIỆT NAM\n\
Độc lập – Tự do – Hạnh phúc\n\
\n\
HỢP ĐỒNG THUÊ NHÀ\n\
\n\
Hôm nay, ngày … tháng … năm …, tại …, chúng tôi gồm:\n\
- Bên cho thuê (Bên A): …\n\
- Bên thuê (Bên B): …\n\
\n\
Hai bên cùng thỏa thuận ký kết hợp đồng thuê nhà với các điều khoản sau đây.";

thread_local! {
    static DOC: RefCell<DocRuntime> = RefCell::new(DocRuntime::default());
}

/// Whether document mode is currently open.
pub fn is_active() -> bool {
    DOC.with(|d| d.borrow().active)
}

/// Toggle document mode open/closed (called from the menu).
pub fn toggle_active() {
    DOC.with(|d| {
        let mut d = d.borrow_mut();
        d.active = !d.active;
    });
}

/// Draw the document-mode window if it is open. Call once per main-window frame.
pub fn build(ctx: &egui::Context) {
    DOC.with(|cell| {
        let mut d = cell.borrow_mut();
        if !d.active {
            return;
        }
        let mut open = true;
        egui::Window::new("Soạn thảo văn bản (thử nghiệm)")
            .open(&mut open)
            .default_size([1000.0, 700.0])
            .min_width(640.0)
            .collapsible(false)
            .show(ctx, |ui| window_ui(ctx, ui, &mut d));
        if !open {
            d.active = false;
        }
    });
}

fn window_ui(ctx: &egui::Context, ui: &mut egui::Ui, d: &mut DocRuntime) {
    // --- Toolbar: paper, alignment, font size ---
    ui.horizontal_wrapped(|ui| {
        egui::ComboBox::from_id_salt("doc_paper")
            .selected_text(paper_name(d.paper))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut d.paper, PaperSize::A4, "A4");
                ui.selectable_value(&mut d.paper, PaperSize::A5, "A5");
                ui.selectable_value(&mut d.paper, PaperSize::LETTER, "Letter");
            });
        ui.separator();
        for (label, a) in [
            ("Trái", ParagraphAlign::Left),
            ("Giữa", ParagraphAlign::Center),
            ("Phải", ParagraphAlign::Right),
            ("Đều", ParagraphAlign::Justify),
        ] {
            if ui.selectable_label(d.align == a, label).clicked() {
                d.align = a;
            }
        }
        ui.separator();
        ui.add(egui::Slider::new(&mut d.font_pt, 8.0..=48.0).text("Cỡ chữ"));
    });
    ui.separator();

    // Refresh the preview to reflect the toolbar (uses last frame's text; edits
    // below take effect on the next frame — imperceptible).
    render_preview(ctx, d);

    let page_count = d.page_count.max(1);
    let preview = d.tex.as_ref().map(|(_, _, t)| t.id());
    let preview_px = d.preview_px;

    ui.columns(2, |cols| {
        // Left: the editable text.
        cols[0].label("Nội dung");
        egui::ScrollArea::vertical()
            .id_salt("doc_editor")
            .auto_shrink([false, false])
            .show(&mut cols[0], |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut d.text)
                        .desired_width(f32::INFINITY)
                        .desired_rows(28)
                        .hint_text("Gõ nội dung tài liệu ở đây…"),
                );
            });

        // Right: the rendered A4 page + page navigation.
        cols[1].horizontal(|ui| {
            ui.label("Xem trước (A4)");
            if ui.small_button("‹").clicked() && d.page_index > 0 {
                d.page_index -= 1;
            }
            ui.label(format!("Trang {}/{}", d.page_index + 1, page_count));
            if ui.small_button("›").clicked() && d.page_index + 1 < page_count {
                d.page_index += 1;
            }
        });
        egui::ScrollArea::both()
            .id_salt("doc_preview")
            .auto_shrink([false, false])
            .show(&mut cols[1], |ui| {
                if let Some(id) = preview {
                    let avail_w = ui.available_width().max(64.0);
                    let scale = (avail_w / preview_px[0]).min(1.0);
                    let size = egui::vec2(preview_px[0] * scale, preview_px[1] * scale);
                    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                    ui.painter().image(id, rect, uv, egui::Color32::WHITE);
                }
            });
    });
}

/// Rebuild the flow + page texture only when the content or page changed.
fn render_preview(ctx: &egui::Context, d: &mut DocRuntime) {
    let hash = content_hash(d);

    // (Re)flow when the content changed.
    if d.layout.as_ref().map(|(h, _)| *h) != Some(hash) {
        if d.engine.is_none() {
            d.engine = Some((FontSystem::new(), SwashCache::new()));
        }
        let doc = build_doc(d);
        let layout = {
            let engine = d.engine.as_mut().expect("engine inited");
            DocumentLayout::build(&doc, DPI, &mut engine.0)
        };
        d.layout = Some((hash, layout));
    }

    let page_count = d.layout.as_ref().expect("layout built").1.page_count();
    d.page_count = page_count;
    if d.page_index >= page_count {
        d.page_index = page_count.saturating_sub(1);
    }

    // (Re)rasterise the visible page when content or page changed.
    let want = (hash, d.page_index);
    if d.tex.as_ref().map(|(h, p, _)| (*h, *p)) != Some(want) {
        let page = d.page_index;
        let (img, pw, ph) = {
            let engine = d.engine.as_mut().expect("engine inited");
            let (fs, sc) = (&mut engine.0, &mut engine.1);
            let layout = &d.layout.as_ref().expect("layout built").1;
            let (pw, ph) = layout.page_px();
            let rgba = layout.render_page(page, fs, sc);
            (
                egui::ColorImage::from_rgba_unmultiplied([pw, ph], &rgba),
                pw,
                ph,
            )
        };
        let tex = ctx.load_texture("iai_doc_preview", img, egui::TextureOptions::LINEAR);
        d.preview_px = [pw as f32, ph as f32];
        d.tex = Some((hash, page, tex));
    }
}

/// Turn the plain-text buffer into a document, formatting every paragraph with
/// the current uniform font size + alignment.
fn build_doc(d: &DocRuntime) -> TextDocument {
    let mut doc = TextDocument::from_plain_text(&d.text);
    doc.default_char.size_pt = d.font_pt;
    doc.page.paper = d.paper;
    for p in &mut doc.paragraphs {
        p.style.align = d.align;
        for r in &mut p.runs {
            r.style.size_pt = d.font_pt;
        }
    }
    doc
}

fn content_hash(d: &DocRuntime) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    d.text.hash(&mut h);
    d.font_pt.to_bits().hash(&mut h);
    (d.align as u8).hash(&mut h);
    d.paper.width_mm.to_bits().hash(&mut h);
    d.paper.height_mm.to_bits().hash(&mut h);
    h.finish()
}

fn paper_name(p: PaperSize) -> &'static str {
    if p == PaperSize::A4 {
        "A4"
    } else if p == PaperSize::A5 {
        "A5"
    } else if p == PaperSize::LETTER {
        "Letter"
    } else {
        "Tùy chỉnh"
    }
}

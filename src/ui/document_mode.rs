//! Document mode (thử nghiệm) — an in-app WYSIWYG word-processor surface.
//!
//! You type directly on an A4 page: a `cosmic-text` Editor owns the document,
//! handling the caret, selection, IME (Vietnamese Telex via egui) and editing,
//! while this module renders the current page (glyphs → texture) with the caret
//! and selection painted on top, and paginates the flowing buffer for display.
//!
//! Self-contained: all state lives in a UI-thread-local (the main-window egui
//! frame always runs on the winit main thread), so the only app hooks are one
//! call from `ui::build` and a menu toggle.
//!
//! v1 edits one uniform style. Per-selection bold / italic and paragraph
//! alignment are the next step; PDF export already writes selectable vector text.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use cosmic_text::{
    Action, Attrs, Buffer, Color as CtColor, Edit, Editor, Family, FontSystem, Metrics, Motion,
    Selection, Shaping, SwashCache,
};

use crate::core::text_document::{PageSetup, PaperSize, TextDocument};
use crate::core::text_layout::DocumentLayout;

const DPI: f32 = 96.0;
const LINE_SPACING: f32 = 1.3;

struct DocRuntime {
    active: bool,
    engine: Option<(FontSystem, SwashCache)>,
    editor: Option<Editor<'static>>,
    setup: PageSetup,
    font_pt: f32,
    page_index: usize,
    page_count: usize,
    /// Cached current-page texture, keyed by `(content revision, page)`.
    tex: Option<(u64, usize, egui::TextureHandle)>,
    /// Bumped whenever the text content changes, to invalidate `tex`.
    revision: u64,
    status: String,
    pending_pdf: Option<Receiver<Option<PathBuf>>>,
}

impl Default for DocRuntime {
    fn default() -> Self {
        Self {
            active: false,
            engine: None,
            editor: None,
            setup: PageSetup::default(),
            font_pt: 13.0,
            page_index: 0,
            page_count: 1,
            tex: None,
            revision: 0,
            status: String::new(),
            pending_pdf: None,
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

pub fn is_active() -> bool {
    DOC.with(|d| d.borrow().active)
}

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
            .default_size([760.0, 860.0])
            .min_width(520.0)
            .collapsible(false)
            .show(ctx, |ui| window_ui(ctx, ui, &mut d));
        if !open {
            d.active = false;
        }
    });
}

/// Line height in layout pixels for the current font size.
fn line_height(font_pt: f32) -> f32 {
    font_pt * DPI / 72.0 * LINE_SPACING
}

fn base_metrics(font_pt: f32) -> Metrics {
    let px = font_pt * DPI / 72.0;
    Metrics::new(px, px * LINE_SPACING)
}

/// Create the editor (and font engine) on first open.
fn ensure_editor(d: &mut DocRuntime) {
    if d.editor.is_some() {
        return;
    }
    if d.engine.is_none() {
        d.engine = Some((FontSystem::new(), SwashCache::new()));
    }
    let cw = d.setup.content_width_px(DPI);
    let fs = &mut d.engine.as_mut().expect("engine").0;
    let mut buffer = Buffer::new(fs, base_metrics(d.font_pt));
    buffer.set_size(Some(cw), None);
    let attrs = Attrs::new().family(Family::Name("Times New Roman"));
    buffer.set_text(SAMPLE_TEXT, &attrs, Shaping::Advanced, None);
    buffer.shape_until_scroll(fs, false);
    d.editor = Some(Editor::new(buffer));
}

fn window_ui(ctx: &egui::Context, ui: &mut egui::Ui, d: &mut DocRuntime) {
    poll_pdf_export(ctx, d);
    ensure_editor(d);

    // --- Toolbar ---
    let mut reshape = false;
    ui.horizontal_wrapped(|ui| {
        egui::ComboBox::from_id_salt("doc_paper")
            .selected_text(paper_name(d.setup.paper))
            .show_ui(ui, |ui| {
                for (p, name) in [
                    (PaperSize::A4, "A4"),
                    (PaperSize::A5, "A5"),
                    (PaperSize::LETTER, "Letter"),
                ] {
                    if ui.selectable_label(d.setup.paper == p, name).clicked() && d.setup.paper != p
                    {
                        d.setup.paper = p;
                        reshape = true;
                    }
                }
            });
        ui.separator();
        if ui
            .add(egui::Slider::new(&mut d.font_pt, 8.0..=48.0).text("Cỡ chữ"))
            .changed()
        {
            reshape = true;
        }
        ui.separator();
        let exporting = d.pending_pdf.is_some();
        if ui
            .add_enabled(!exporting, egui::Button::new("Xuất PDF"))
            .clicked()
        {
            d.pending_pdf = Some(spawn_save_dialog());
            d.status = "Đang chọn nơi lưu…".to_string();
        }
    });

    if reshape {
        apply_reshape(d);
    }

    // --- Page geometry (layout px at 96 dpi) ---
    let (cx, cy, _cw, ch) = d.setup.content_rect_px(DPI);
    let page_w = d.setup.paper.width_px(DPI);
    let page_h = d.setup.paper.height_px(DPI);

    // Reserve the page area; scale it to fit the available box (leave room for
    // the page-nav footer) so the whole sheet stays visible.
    let avail_w = ui.available_width().max(120.0);
    let avail_h = (ui.available_height() - 36.0).max(120.0);
    let scale = (avail_w / page_w).min(avail_h / page_h).min(1.4);
    let disp = egui::vec2(page_w * scale, page_h * scale);
    let (rect, response) = ui.allocate_exact_size(disp, egui::Sense::click_and_drag());

    if response.clicked() || response.drag_started() {
        response.request_focus();
    }
    let focused = response.has_focus();

    // Split disjoint borrows of the runtime.
    let page_index = d.page_index;
    let engine = d.engine.as_mut().expect("engine");
    let editor = d.editor.as_mut().expect("editor");
    let fs = &mut engine.0;
    let sc = &mut engine.1;

    let mut dirty = false;

    // Pointer → caret / selection (buffer coords).
    if focused && (response.clicked() || response.dragged()) {
        if let Some(pos) = response.interact_pointer_pos() {
            let bx = ((pos.x - rect.min.x) / scale - cx).max(0.0);
            let by = page_index as f32 * ch + (pos.y - rect.min.y) / scale - cy;
            let act = if response.dragged() && !response.drag_started() {
                Action::Drag {
                    x: bx as i32,
                    y: by as i32,
                }
            } else {
                Action::Click {
                    x: bx as i32,
                    y: by as i32,
                }
            };
            editor.action(fs, act);
        }
    }

    // Keyboard / text / IME.
    if focused {
        let events = ui.input(|i| i.events.clone());
        for ev in events {
            match ev {
                egui::Event::Text(t) if !t.is_empty() => {
                    editor.insert_string(&t, None);
                    dirty = true;
                }
                egui::Event::Paste(t) if !t.is_empty() => {
                    editor.insert_string(&t, None);
                    dirty = true;
                }
                egui::Event::Ime(egui::ImeEvent::Commit(t)) if !t.is_empty() => {
                    editor.insert_string(&t, None);
                    dirty = true;
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    let shift = modifiers.shift;
                    match key {
                        egui::Key::Backspace => {
                            editor.action(fs, Action::Backspace);
                            dirty = true;
                        }
                        egui::Key::Delete => {
                            editor.action(fs, Action::Delete);
                            dirty = true;
                        }
                        egui::Key::Enter => {
                            editor.action(fs, Action::Enter);
                            dirty = true;
                        }
                        egui::Key::ArrowLeft => motion(editor, fs, Motion::Left, shift),
                        egui::Key::ArrowRight => motion(editor, fs, Motion::Right, shift),
                        egui::Key::ArrowUp => motion(editor, fs, Motion::Up, shift),
                        egui::Key::ArrowDown => motion(editor, fs, Motion::Down, shift),
                        egui::Key::Home => motion(editor, fs, Motion::Home, shift),
                        egui::Key::End => motion(editor, fs, Motion::End, shift),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    editor.shape_as_needed(fs, false);
    if dirty {
        d.revision = d.revision.wrapping_add(1);
    }

    // Pagination from the flowed buffer height.
    let total_h = editor.with_buffer(|b| {
        b.layout_runs()
            .fold(0.0f32, |m, run| m.max(run.line_top + run.line_height))
    });
    let page_count = ((total_h / ch).ceil() as usize).max(1);
    let mut page_index = page_index.min(page_count - 1);

    // Rasterise the current page (cached by revision + page).
    let revision = d.revision;
    let need = d.tex.as_ref().map(|(r, p, _)| (*r, *p)) != Some((revision, page_index));
    let new_tex = if need {
        let px = render_page(editor, fs, sc, page_index, cx, cy, ch, page_w, page_h);
        Some(egui::ColorImage::from_rgba_unmultiplied(
            [page_w.ceil() as usize, page_h.ceil() as usize],
            &px,
        ))
    } else {
        None
    };

    // Collect selection rects and the caret (page-local px) for overlay drawing.
    let sel = editor.selection_bounds();
    let caret = editor.cursor_position();
    let mut sel_rects: Vec<egui::Rect> = Vec::new();
    let lh = line_height(d.font_pt);
    if let Some((start, end)) = sel {
        editor.with_buffer(|b| {
            for run in b.layout_runs() {
                let p = (run.line_top / ch).floor() as usize;
                if p != page_index {
                    continue;
                }
                let top = cy + run.line_top - page_index as f32 * ch;
                for (hx, hw) in run.highlight(start, end) {
                    let min = rect.min + egui::vec2((cx + hx) * scale, top * scale);
                    let max = min + egui::vec2(hw.max(2.0) * scale, lh * scale);
                    sel_rects.push(egui::Rect::from_min_max(min, max));
                }
            }
        });
    }

    // Done with the editor/engine borrows.
    d.page_index = page_index;
    d.page_count = page_count;
    if let Some(image) = new_tex {
        let tex = ctx.load_texture("iai_doc_page", image, egui::TextureOptions::LINEAR);
        d.tex = Some((revision, page_index, tex));
    }

    // --- Paint page, selection, caret ---
    let painter = ui.painter_at(rect);
    if let Some((_, _, tex)) = &d.tex {
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        painter.image(tex.id(), rect, uv, egui::Color32::WHITE);
    }
    let sel_color = egui::Color32::from_rgba_unmultiplied(60, 120, 240, 70);
    for r in sel_rects {
        painter.rect_filled(r, 0.0, sel_color);
    }
    if focused {
        if let Some((qx, qy)) = caret {
            let p = (qy as f32 / ch).floor() as usize;
            if p == page_index {
                let x = rect.min.x + (cx + qx as f32) * scale;
                let y0 = rect.min.y + (cy + qy as f32 - page_index as f32 * ch) * scale;
                let blink = ui.input(|i| (i.time * 1.5) as i64 % 2 == 0);
                if blink {
                    painter.line_segment(
                        [egui::pos2(x, y0), egui::pos2(x, y0 + lh * scale)],
                        egui::Stroke::new(1.5, egui::Color32::from_rgb(20, 20, 20)),
                    );
                }
                // Position the OS IME candidate window at the caret.
                let caret_rect = egui::Rect::from_min_max(
                    egui::pos2(x, y0),
                    egui::pos2(x + 1.0, y0 + lh * scale),
                );
                ctx.output_mut(|o| {
                    o.ime = Some(egui::output::IMEOutput {
                        rect: caret_rect,
                        cursor_rect: caret_rect,
                    });
                });
            }
            ctx.request_repaint_after(Duration::from_millis(400)); // caret blink
        }
    }

    // --- Footer: page nav + status ---
    ui.horizontal(|ui| {
        if ui.small_button("‹").clicked() && d.page_index > 0 {
            d.page_index -= 1;
        }
        ui.label(format!(
            "Trang {}/{}",
            d.page_index + 1,
            d.page_count.max(1)
        ));
        if ui.small_button("›").clicked() && d.page_index + 1 < d.page_count {
            d.page_index += 1;
        }
        if !focused {
            ui.separator();
            ui.weak("Bấm vào trang để bắt đầu gõ");
        }
    });
    if !d.status.is_empty() {
        ui.label(&d.status);
    }
}

/// Extend or collapse the selection, then move the cursor.
fn motion(editor: &mut Editor<'static>, fs: &mut FontSystem, m: Motion, extend: bool) {
    if extend {
        if matches!(editor.selection(), Selection::None) {
            editor.set_selection(Selection::Normal(editor.cursor()));
        }
    } else {
        editor.set_selection(Selection::None);
    }
    editor.action(fs, Action::Motion(m));
}

/// Re-apply page width / metrics to the editor buffer after a toolbar change.
fn apply_reshape(d: &mut DocRuntime) {
    let cw = d.setup.content_width_px(DPI);
    let metrics = base_metrics(d.font_pt);
    let engine = d.engine.as_mut().expect("engine");
    let editor = d.editor.as_mut().expect("editor");
    editor.with_buffer_mut(|b| {
        b.set_metrics(metrics);
        b.set_size(Some(cw), None);
    });
    editor.shape_as_needed(&mut engine.0, false);
    d.revision = d.revision.wrapping_add(1);
}

/// Rasterise one page of the editor buffer to opaque white RGBA.
#[allow(clippy::too_many_arguments)]
fn render_page(
    editor: &Editor<'static>,
    fs: &mut FontSystem,
    cache: &mut SwashCache,
    page: usize,
    cx: f32,
    cy: f32,
    ch: f32,
    page_w: f32,
    page_h: f32,
) -> Vec<u8> {
    let pw = page_w.ceil() as usize;
    let ph = page_h.ceil() as usize;
    let mut buf = vec![255u8; pw * ph * 4];
    let ink = CtColor::rgb(0x1a, 0x1a, 0x1a);
    editor.with_buffer(|b| {
        for run in b.layout_runs() {
            let p = (run.line_top / ch).floor() as usize;
            if p != page {
                continue;
            }
            let base_y = cy + run.line_y - page as f32 * ch;
            for glyph in run.glyphs {
                let color = glyph.color_opt.unwrap_or(ink);
                let phys = glyph.physical((cx, base_y), 1.0);
                cache.with_pixels(fs, phys.cache_key, color, |gx, gy, col| {
                    blend_px(&mut buf, pw, ph, phys.x + gx, phys.y + gy, col);
                });
            }
        }
    });
    buf
}

fn blend_px(buf: &mut [u8], w: usize, h: usize, x: i32, y: i32, color: CtColor) {
    if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
        return;
    }
    let a = color.a() as f32 / 255.0;
    if a <= 0.0 {
        return;
    }
    let idx = (y as usize * w + x as usize) * 4;
    let src = [color.r() as f32, color.g() as f32, color.b() as f32];
    for (c, s) in src.iter().enumerate() {
        let d = buf[idx + c] as f32;
        buf[idx + c] = (s * a + d * (1.0 - a)).round() as u8;
    }
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

// --- PDF export -----------------------------------------------------------

fn spawn_save_dialog() -> Receiver<Option<PathBuf>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let path = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name("tai-lieu.pdf")
            .save_file()
            .map(|mut p| {
                if p.extension().is_none() {
                    p.set_extension("pdf");
                }
                p
            });
        let _ = tx.send(path);
    });
    rx
}

fn poll_pdf_export(ctx: &egui::Context, d: &mut DocRuntime) {
    enum Poll {
        Idle,
        Waiting,
        Done(Option<PathBuf>),
    }
    let poll = match &d.pending_pdf {
        None => Poll::Idle,
        Some(rx) => match rx.try_recv() {
            Ok(path) => Poll::Done(path),
            Err(TryRecvError::Empty) => Poll::Waiting,
            Err(TryRecvError::Disconnected) => Poll::Done(None),
        },
    };
    match poll {
        Poll::Idle => {}
        Poll::Waiting => ctx.request_repaint_after(Duration::from_millis(100)),
        Poll::Done(Some(path)) => {
            d.status = match export_pdf(d, &path) {
                Ok(pages) => format!("Đã lưu PDF ({pages} trang): {}", path.display()),
                Err(e) => format!("Lỗi xuất PDF: {e}"),
            };
            d.pending_pdf = None;
        }
        Poll::Done(None) => {
            d.status = "Đã hủy xuất PDF.".to_string();
            d.pending_pdf = None;
        }
    }
}

/// Plain text of the document (one paragraph per hard line).
fn editor_text(editor: &Editor<'static>) -> String {
    editor.with_buffer(|b| {
        b.lines
            .iter()
            .map(|l| l.text())
            .collect::<Vec<_>>()
            .join("\n")
    })
}

/// Write the document as a selectable-text vector PDF. Returns the page count.
fn export_pdf(d: &mut DocRuntime, path: &std::path::Path) -> Result<usize, String> {
    let text = editor_text(d.editor.as_ref().expect("editor"));
    let mut tdoc = TextDocument::from_plain_text(&text);
    tdoc.default_char.size_pt = d.font_pt;
    tdoc.page = d.setup;
    for p in &mut tdoc.paragraphs {
        for r in &mut p.runs {
            r.style.size_pt = d.font_pt;
        }
    }
    let engine = d.engine.as_mut().expect("engine");
    let layout = DocumentLayout::build(&tdoc, DPI, &mut engine.0);
    let pages = layout.page_count();
    layout.write_text_pdf(&mut engine.0, path)?;
    Ok(pages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_page_rasterises_editor_text() {
        let mut fs = FontSystem::new();
        let mut sc = SwashCache::new();
        let setup = PageSetup::default();
        let (cx, cy, _, ch) = setup.content_rect_px(DPI);
        let page_w = setup.paper.width_px(DPI);
        let page_h = setup.paper.height_px(DPI);

        let mut buffer = Buffer::new(&mut fs, base_metrics(13.0));
        buffer.set_size(Some(setup.content_width_px(DPI)), None);
        buffer.set_text(
            "Cộng hòa xã hội chủ nghĩa Việt Nam",
            &Attrs::new().family(Family::Name("Times New Roman")),
            Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut fs, false);
        let editor = Editor::new(buffer);

        let px = render_page(&editor, &mut fs, &mut sc, 0, cx, cy, ch, page_w, page_h);
        assert_eq!(
            px.len(),
            page_w.ceil() as usize * page_h.ceil() as usize * 4
        );
        assert!(
            px.chunks_exact(4).any(|p| p[0] < 200),
            "expected rasterised ink on the page"
        );
    }
}

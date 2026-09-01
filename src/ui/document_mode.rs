//! Document mode (thử nghiệm) — an in-app WYSIWYG word-processor surface.
//!
//! You type directly on an A4 page: a `cosmic-text` Editor owns the document,
//! handling the caret, selection, IME (Vietnamese Telex via egui) and editing,
//! while this module renders the current page (glyphs → texture) with the caret
//! and selection painted on top, and paginates the flowing buffer for display.
//!
//! Canonical text lives on the active application `Document`; this module keeps
//! only the ephemeral cosmic-text editor and page texture projection.
//!
//! v1 edits one uniform style. Per-selection bold / italic and paragraph
//! alignment are the next step; PDF export already writes selectable vector text.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;

use cosmic_text::{
    Action, Align, Attrs, Buffer, Color as CtColor, Edit, Editor, Family, FontSystem, Metrics,
    Motion, Selection, Shaping, SwashCache,
};
use egui_phosphor::regular as ph;

use crate::core::document::DocumentId;
use crate::core::text_document::{PageSetup, PaperSize, ParagraphAlign, TextDocument};
use crate::ui::{FlowTextViewModel, UiActions, UiData};

const DPI: f32 = 96.0;
const LINE_SPACING: f32 = 1.3;

struct DocRuntime {
    bound_model_revision: u64,
    editor: Option<Editor<'static>>,
    setup: PageSetup,
    font_pt: f32,
    /// Display zoom multiplier on top of fit-to-window (1.0 = fit).
    zoom: f32,
    /// True while a click-drag selection is in progress (so the anchor Click is
    /// always sent before the first Drag — otherwise the anchor is stale).
    drag_active: bool,
    page_index: usize,
    page_count: usize,
    /// Cached current-page texture, keyed by `(content revision, page)`.
    tex: Option<(u64, usize, egui::TextureHandle)>,
    /// Bumped whenever the text content changes, to invalidate `tex`.
    revision: u64,
}

impl Default for DocRuntime {
    fn default() -> Self {
        Self {
            bound_model_revision: 0,
            editor: None,
            setup: PageSetup::default(),
            font_pt: 13.0,
            zoom: 1.0,
            drag_active: false,
            page_index: 0,
            page_count: 1,
            tex: None,
            revision: 0,
        }
    }
}

struct DocumentModeRuntime {
    font_system: FontSystem,
    swash_cache: SwashCache,
    documents: HashMap<DocumentId, DocRuntime>,
}

impl Default for DocumentModeRuntime {
    fn default() -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
            documents: HashMap::new(),
        }
    }
}

thread_local! {
    static DOCUMENT_MODE: RefCell<DocumentModeRuntime> =
        RefCell::new(DocumentModeRuntime::default());
}

/// Draw the active flowing-text document inside the shared central viewport.
pub fn build(ctx: &egui::Context, data: &UiData, actions: &mut UiActions, viewport: egui::Rect) {
    let Some(view) = data.doc.flow_text.as_ref() else {
        return;
    };
    DOCUMENT_MODE.with(|cell| {
        let mut runtime = cell.borrow_mut();
        runtime
            .documents
            .retain(|id, _| data.doc.doc_ids.iter().any(|open_id| open_id == id));
        let DocumentModeRuntime {
            font_system,
            swash_cache,
            documents,
        } = &mut *runtime;
        let d = documents.entry(data.doc.id).or_default();
        sync_runtime(d, font_system, view);
        d.zoom = data.doc.zoom;
        let revision_before = d.revision;

        egui::Area::new(egui::Id::new("flow_text_document_surface"))
            // Keep modal dialogs and floating panels above the editing surface.
            .order(egui::Order::Background)
            .fixed_pos(viewport.min)
            .show(ctx, |ui| {
                ui.set_min_size(viewport.size());
                ui.set_max_size(viewport.size());
                egui::Frame::new()
                    .fill(egui::Color32::from_gray(54))
                    .inner_margin(egui::Margin::ZERO)
                    .show(ui, |ui| {
                        window_ui(ctx, ui, d, font_system, swash_cache, actions, data)
                    });
            });

        if d.revision != revision_before {
            actions.doc.replace_flow_text_document = Some((data.doc.id, editor_document(&d)));
        }
        if d.page_count != view.page_count || d.page_index != view.active_page {
            actions.doc.set_flow_text_layout = Some((data.doc.id, d.page_count, d.page_index));
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

fn sync_runtime(d: &mut DocRuntime, fs: &mut FontSystem, view: &FlowTextViewModel) {
    if d.bound_model_revision == view.revision && d.editor.is_some() {
        d.page_index = view.active_page.min(view.page_count.saturating_sub(1));
        return;
    }

    // An update emitted by this editor comes back through UiData on the next
    // frame. If content is already identical, acknowledge the model revision
    // without rebuilding and losing caret/selection.
    if d.editor.is_some() && editor_document(d) == *view.document {
        d.bound_model_revision = view.revision;
        d.page_index = view.active_page.min(view.page_count.saturating_sub(1));
        return;
    }
    d.setup = view.document.page;
    d.font_pt = view.document.default_char.size_pt;
    let cw = d.setup.content_width_px(DPI);
    let mut buffer = Buffer::new(fs, base_metrics(d.font_pt));
    buffer.set_size(Some(cw), None);
    let attrs = Attrs::new().family(Family::Name(view.document.default_char.font.name()));
    buffer.set_text(&view.document.plain_text(), &attrs, Shaping::Advanced, None);
    for (line, paragraph) in buffer.lines.iter_mut().zip(&view.document.paragraphs) {
        line.set_align(Some(match paragraph.style.align {
            ParagraphAlign::Left => Align::Left,
            ParagraphAlign::Center => Align::Center,
            ParagraphAlign::Right => Align::Right,
            ParagraphAlign::Justify => Align::Justified,
        }));
    }
    buffer.shape_until_scroll(fs, false);
    d.editor = Some(Editor::new(buffer));
    d.bound_model_revision = view.revision;
    d.page_index = view.active_page.min(view.page_count.saturating_sub(1));
    d.page_count = view.page_count.max(1);
    d.tex = None;
    d.revision = d.revision.wrapping_add(1);
}

fn window_ui(
    ctx: &egui::Context,
    ui: &mut egui::Ui,
    d: &mut DocRuntime,
    fs: &mut FontSystem,
    swash_cache: &mut SwashCache,
    actions: &mut UiActions,
    data: &UiData,
) {
    // --- Toolbar (Phosphor icons) ---
    let mut reshape = false;
    let mut align_cmd: Option<Align> = None;
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

        // Font size.
        ui.label(ph::TEXT_AA);
        if ui
            .add(
                egui::DragValue::new(&mut d.font_pt)
                    .range(8.0..=48.0)
                    .speed(0.25)
                    .suffix(" pt"),
            )
            .changed()
        {
            reshape = true;
        }
        ui.separator();

        // Paragraph alignment.
        for (icon, a, tip) in [
            (ph::TEXT_ALIGN_LEFT, Align::Left, "Căn trái"),
            (ph::TEXT_ALIGN_CENTER, Align::Center, "Căn giữa"),
            (ph::TEXT_ALIGN_RIGHT, Align::Right, "Căn phải"),
            (ph::TEXT_ALIGN_JUSTIFY, Align::Justified, "Căn đều"),
        ] {
            if ui.button(icon).on_hover_text(tip).clicked() {
                align_cmd = Some(a);
            }
        }
        ui.separator();

        // Zoom.
        if ui
            .button(ph::MAGNIFYING_GLASS_MINUS)
            .on_hover_text("Thu nhỏ (Alt + lăn xuống)")
            .clicked()
        {
            actions.doc.zoom_out = true;
        }
        ui.label(format!("{}%", (d.zoom * 100.0).round() as i32));
        if ui
            .button(ph::MAGNIFYING_GLASS_PLUS)
            .on_hover_text("Phóng to (Alt + lăn lên)")
            .clicked()
        {
            actions.doc.zoom_in = true;
        }
        ui.label("Alt + lăn chuột");
    });

    if reshape {
        apply_reshape(d, fs);
    }
    if let Some(a) = align_cmd {
        apply_align(d, fs, a);
    }

    // --- Page geometry (layout px at 96 dpi) ---
    let (cx, cy, _cw, ch) = d.setup.content_rect_px(DPI);
    let page_w = d.setup.paper.width_px(DPI);
    let page_h = d.setup.paper.height_px(DPI);
    let pw = page_w.ceil() as usize;
    let ph_px = page_h.ceil() as usize;
    let lh = line_height(d.font_pt);

    // Reserve fixed top/left strips for the same ruler geometry used by canvas
    // mode. Only the paper workspace scrolls; the ruler never moves with it.
    let avail_w = ui.available_width().max(120.0);
    let avail_h = ui.available_height().max(120.0);
    let ruler_size = if data.chrome.show_rulers {
        super::RULER_SIZE
    } else {
        0.0
    };
    let surface_rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(avail_w, avail_h));
    ui.allocate_rect(surface_rect, egui::Sense::hover());
    let h_ruler_rect = egui::Rect::from_min_max(
        egui::pos2(surface_rect.left() + ruler_size, surface_rect.top()),
        egui::pos2(surface_rect.right(), surface_rect.top() + ruler_size),
    );
    let v_ruler_rect = egui::Rect::from_min_max(
        egui::pos2(surface_rect.left(), surface_rect.top() + ruler_size),
        egui::pos2(surface_rect.left() + ruler_size, surface_rect.bottom()),
    );
    let scroll_rect = egui::Rect::from_min_max(
        egui::pos2(
            surface_rect.left() + ruler_size,
            surface_rect.top() + ruler_size,
        ),
        surface_rect.right_bottom(),
    );
    let fit_w = scroll_rect.width().max(80.0);
    let fit_h = scroll_rect.height().max(80.0);
    let fit = (fit_w / page_w).min(fit_h / page_h);
    let scale = (fit * d.zoom).clamp(0.05, 6.0);
    let disp = egui::vec2(page_w * scale, page_h * scale);

    // The page lives in a scroll area so a zoomed-in sheet can be panned.
    let mut scroll_ui = ui.new_child(egui::UiBuilder::new().max_rect(scroll_rect));
    let scroll_output = egui::ScrollArea::both()
        .id_salt("doc_page_scroll")
        .auto_shrink([false, false])
        .max_height(scroll_rect.height())
        .show(&mut scroll_ui, |ui| {
            // Keep a fit/small page centred in the editing surface. At larger
            // zoom levels the workspace grows with the page and the scroll area
            // continues to provide normal two-axis panning.
            let workspace_size = egui::vec2(
                scroll_rect.width().max(disp.x + 12.0),
                scroll_rect.height().max(disp.y + 12.0),
            );
            let (workspace_rect, _) = ui.allocate_exact_size(workspace_size, egui::Sense::hover());
            let rect = egui::Rect::from_center_size(workspace_rect.center(), disp);
            let response = ui.interact(
                rect,
                ui.id().with("doc_page_interaction"),
                egui::Sense::click_and_drag(),
            );
            if response.clicked() || response.drag_started() {
                response.request_focus();
            }
            let focused = response.has_focus();

            let map = |pos: egui::Pos2, page: usize| {
                let bx = ((pos.x - rect.min.x) / scale - cx).max(0.0);
                let by = page as f32 * ch + (pos.y - rect.min.y) / scale - cy;
                (bx as i32, by as i32)
            };

            let (image, sel_rects, caret, page_count, page_index, revision) = {
                let editor = d.editor.as_mut().expect("editor");
                let page_index = d.page_index;
                let mut dirty = false;

                // Pointer → caret / selection. `drag_active` guarantees an anchor
                // Click precedes the first Drag, so a drag selects only its span.
                if focused {
                    if response.drag_started() {
                        if let Some(p) = response.interact_pointer_pos() {
                            let (x, y) = map(p, page_index);
                            editor.action(fs, Action::Click { x, y });
                        }
                        d.drag_active = true;
                    } else if response.dragged() {
                        if let Some(p) = response.interact_pointer_pos() {
                            let (x, y) = map(p, page_index);
                            if d.drag_active {
                                editor.action(fs, Action::Drag { x, y });
                            } else {
                                editor.action(fs, Action::Click { x, y });
                                d.drag_active = true;
                            }
                        }
                    } else if response.clicked() {
                        if let Some(p) = response.interact_pointer_pos() {
                            let (x, y) = map(p, page_index);
                            editor.action(fs, Action::Click { x, y });
                        }
                    }
                    if response.drag_stopped() {
                        d.drag_active = false;
                    }
                }

                // Keyboard / text / IME / clipboard.
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
                            egui::Event::Copy => {
                                if let Some(s) = editor.copy_selection() {
                                    ctx.copy_text(s);
                                }
                            }
                            egui::Event::Cut => {
                                if let Some(s) = editor.copy_selection() {
                                    ctx.copy_text(s);
                                }
                                if editor.delete_selection() {
                                    dirty = true;
                                }
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
                                    egui::Key::ArrowRight => {
                                        motion(editor, fs, Motion::Right, shift)
                                    }
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
                let revision = if dirty {
                    d.revision.wrapping_add(1)
                } else {
                    d.revision
                };

                let total_h = editor.with_buffer(|b| {
                    b.layout_runs()
                        .fold(0.0f32, |m, run| m.max(run.line_top + run.line_height))
                });
                let page_count = ((total_h / ch).ceil() as usize).max(1);
                let page_index = page_index.min(page_count - 1);

                let need = d.tex.as_ref().map(|(r, p, _)| (*r, *p)) != Some((revision, page_index));
                let image = if need {
                    let px = render_page(
                        editor,
                        fs,
                        swash_cache,
                        page_index,
                        cx,
                        cy,
                        ch,
                        page_w,
                        page_h,
                    );
                    Some(egui::ColorImage::from_rgba_unmultiplied([pw, ph_px], &px))
                } else {
                    None
                };

                let mut sel_rects: Vec<egui::Rect> = Vec::new();
                if let Some((start, end)) = editor.selection_bounds() {
                    editor.with_buffer(|b| {
                        for run in b.layout_runs() {
                            if (run.line_top / ch).floor() as usize != page_index {
                                continue;
                            }
                            // LayoutRun::highlight only clips character indices
                            // on the two boundary lines. Without this explicit
                            // line-range check it considers every unrelated line
                            // selected because both boundary line IDs differ.
                            if !selection_contains_line(start.line, end.line, run.line_i) {
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
                let caret = editor.cursor_position();
                (image, sel_rects, caret, page_count, page_index, revision)
            };

            d.page_index = page_index;
            d.page_count = page_count;
            d.revision = revision;
            if let Some(img) = image {
                let tex = ctx.load_texture("iai_doc_page", img, egui::TextureOptions::LINEAR);
                d.tex = Some((revision, page_index, tex));
            }

            // Paint the page, selection then caret.
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
                    if (qy as f32 / ch).floor() as usize == page_index {
                        let x = rect.min.x + (cx + qx as f32) * scale;
                        let y0 = rect.min.y + (cy + qy as f32 - page_index as f32 * ch) * scale;
                        let blink = ui.input(|i| (i.time * 1.5) as i64 % 2 == 0);
                        if blink {
                            painter.line_segment(
                                [egui::pos2(x, y0), egui::pos2(x, y0 + lh * scale)],
                                egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(20, 20, 20)),
                            );
                        }
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
            rect
        });

    if data.chrome.show_rulers {
        paint_document_rulers(
            ui.painter(),
            h_ruler_rect,
            v_ruler_rect,
            scroll_output.inner,
            scale,
            d.setup,
            data.chrome.theme_mode.palette(),
        );
    }
}

/// Paint fixed ruler strips around the flowing-text viewport. Their zero follows
/// the physical sheet while the bars themselves remain stationary, matching the
/// canvas editor; accent markers show the active text margins.
fn paint_document_rulers(
    painter: &egui::Painter,
    h: egui::Rect,
    v: egui::Rect,
    page: egui::Rect,
    scale: f32,
    setup: PageSetup,
    pal: crate::ui::theme::Palette,
) {
    let corner = egui::Rect::from_min_max(
        egui::pos2(v.left(), h.top()),
        egui::pos2(v.right(), h.bottom()),
    );
    for rect in [h, v, corner] {
        painter.rect_filled(rect, 1.0, pal.ruler_bg);
        painter.rect_stroke(
            rect,
            1.0,
            egui::Stroke::new(1.0_f32, pal.border_subtle),
            egui::StrokeKind::Inside,
        );
    }

    let px_per_mm = DPI / 25.4 * scale;
    let major_mm = [5.0_f32, 10.0, 20.0, 50.0, 100.0]
        .into_iter()
        .find(|step| step * px_per_mm >= 34.0)
        .unwrap_or(100.0);
    let font = egui::FontId::monospace(9.0);
    let h_painter = painter.with_clip_rect(h);
    let v_painter = painter.with_clip_rect(v);

    let mut mm = 0.0_f32;
    while mm <= setup.paper.width_mm + 0.01 {
        let x = page.left() + mm * px_per_mm;
        h_painter.line_segment(
            [egui::pos2(x, h.bottom() - 7.0), egui::pos2(x, h.bottom())],
            egui::Stroke::new(1.0_f32, pal.text_disabled),
        );
        h_painter.text(
            egui::pos2(x + 2.0, h.top() + 2.0),
            egui::Align2::LEFT_TOP,
            format!("{}", mm.round() as i32),
            font.clone(),
            pal.text_secondary,
        );
        let mid = x + major_mm * px_per_mm * 0.5;
        if mid < h.right() {
            h_painter.line_segment(
                [
                    egui::pos2(mid, h.bottom() - 3.0),
                    egui::pos2(mid, h.bottom()),
                ],
                egui::Stroke::new(1.0_f32, pal.text_disabled),
            );
        }
        mm += major_mm;
    }

    mm = 0.0;
    while mm <= setup.paper.height_mm + 0.01 {
        let y = page.top() + mm * px_per_mm;
        v_painter.line_segment(
            [egui::pos2(v.right() - 7.0, y), egui::pos2(v.right(), y)],
            egui::Stroke::new(1.0_f32, pal.text_disabled),
        );
        v_painter.text(
            egui::pos2(v.left() + 2.0, y + 2.0),
            egui::Align2::LEFT_TOP,
            format!("{}", mm.round() as i32),
            font.clone(),
            pal.text_secondary,
        );
        let mid = y + major_mm * px_per_mm * 0.5;
        if mid < v.bottom() {
            v_painter.line_segment(
                [egui::pos2(v.right() - 3.0, mid), egui::pos2(v.right(), mid)],
                egui::Stroke::new(1.0_f32, pal.text_disabled),
            );
        }
        mm += major_mm;
    }

    let margin_stroke = egui::Stroke::new(2.0_f32, pal.accent_guide);
    for margin_mm in [
        setup.margins.left_mm,
        setup.paper.width_mm - setup.margins.right_mm,
    ] {
        let x = page.left() + margin_mm * px_per_mm;
        h_painter.line_segment(
            [
                egui::pos2(x, h.top() + 1.0),
                egui::pos2(x, h.bottom() - 1.0),
            ],
            margin_stroke,
        );
    }
    for margin_mm in [
        setup.margins.top_mm,
        setup.paper.height_mm - setup.margins.bottom_mm,
    ] {
        let y = page.top() + margin_mm * px_per_mm;
        v_painter.line_segment(
            [
                egui::pos2(v.left() + 1.0, y),
                egui::pos2(v.right() - 1.0, y),
            ],
            margin_stroke,
        );
    }
    painter.text(
        corner.center(),
        egui::Align2::CENTER_CENTER,
        "mm",
        egui::FontId::monospace(8.0),
        pal.text_secondary,
    );
}

fn selection_contains_line(start_line: usize, end_line: usize, line: usize) -> bool {
    (start_line..=end_line).contains(&line)
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
fn apply_reshape(d: &mut DocRuntime, fs: &mut FontSystem) {
    let cw = d.setup.content_width_px(DPI);
    let metrics = base_metrics(d.font_pt);
    let editor = d.editor.as_mut().expect("editor");
    editor.with_buffer_mut(|b| {
        b.set_metrics(metrics);
        b.set_size(Some(cw), None);
    });
    editor.shape_as_needed(fs, false);
    d.revision = d.revision.wrapping_add(1);
}

/// Set the alignment of every paragraph touched by the selection (or the caret
/// line when there is no selection).
fn apply_align(d: &mut DocRuntime, fs: &mut FontSystem, align: Align) {
    let editor = d.editor.as_mut().expect("editor");
    let (l0, l1) = match editor.selection_bounds() {
        Some((s, e)) => (s.line, e.line),
        None => {
            let c = editor.cursor();
            (c.line, c.line)
        }
    };
    editor.with_buffer_mut(|b| {
        for i in l0..=l1 {
            if let Some(line) = b.lines.get_mut(i) {
                line.set_align(Some(align));
            }
        }
    });
    editor.shape_as_needed(fs, false);
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

/// Snapshot the interactive cosmic-text projection back into the canonical
/// engine-agnostic document model. The current surface supports one character
/// style plus per-paragraph alignment; richer run mapping is added behind this
/// same bridge rather than creating a second source of truth.
fn editor_document(d: &DocRuntime) -> TextDocument {
    let editor = d.editor.as_ref().expect("editor");
    let mut document = TextDocument::from_plain_text(&editor_text(editor));
    document.page = d.setup;
    document.default_char.size_pt = d.font_pt;
    document.default_para.line_spacing = LINE_SPACING;

    editor.with_buffer(|buffer| {
        for (paragraph, line) in document.paragraphs.iter_mut().zip(&buffer.lines) {
            paragraph.style.line_spacing = LINE_SPACING;
            paragraph.style.align = match line.align().unwrap_or(Align::Left) {
                Align::Center => ParagraphAlign::Center,
                Align::Right | Align::End => ParagraphAlign::Right,
                Align::Justified => ParagraphAlign::Justify,
                _ => ParagraphAlign::Left,
            };
            for run in &mut paragraph.runs {
                run.style.size_pt = d.font_pt;
            }
        }
    });
    document
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_highlight_is_limited_to_selected_lines() {
        assert!(!selection_contains_line(2, 4, 1));
        assert!(selection_contains_line(2, 4, 2));
        assert!(selection_contains_line(2, 4, 3));
        assert!(selection_contains_line(2, 4, 4));
        assert!(!selection_contains_line(2, 4, 5));
    }

    #[test]
    fn single_line_selection_does_not_highlight_other_lines() {
        assert!(selection_contains_line(7, 7, 7));
        assert!(!selection_contains_line(7, 7, 6));
        assert!(!selection_contains_line(7, 7, 8));
    }

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

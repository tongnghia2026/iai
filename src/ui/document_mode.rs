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
//! Formatting so far: per-selection bold / italic / underline (Ctrl+B / I / U)
//! and text colour, plus per-paragraph alignment; one shared body face and size.
//! Per-run font and size, lists and inline images are the next steps. PDF export
//! and `.iai` save already carry the styled runs.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;

use cosmic_text::{
    Action, Align, Attrs, AttrsList, Buffer, Color as CtColor, Cursor, Edit, Editor, Family,
    FontSystem, Metrics, Motion, Selection, Shaping, Style, SwashCache, UnderlineStyle, Weight,
};
use egui_phosphor::regular as ph;

use crate::core::color::Color;
use crate::core::document::DocumentId;
use crate::core::text::TextFontFamily;
use crate::core::text_document::{
    CharStyle, ImageBlock, ImageWrap, ListKind, PageSetup, PaperSize, Paragraph, ParagraphAlign,
    ParagraphStyle, Run, TextDocument,
};
use crate::ui::intent::FlowTextFocus;
use crate::ui::{FlowTextViewModel, UiActions, UiData};

/// Hanging indent for a list item, in mm (matches `text_layout::LIST_INDENT_MM`).
const LIST_INDENT_MM: f32 = 8.0;

fn list_indent_px() -> f32 {
    LIST_INDENT_MM / 25.4 * DPI
}

/// List kind stored in a `BufferLine`'s metadata (survives edits; only a full
/// line reset clears it). 0 = none, 1 = bullet, 2 = numbered.
fn list_kind_to_code(kind: ListKind) -> usize {
    match kind {
        ListKind::None => 0,
        ListKind::Bullet => 1,
        ListKind::Numbered => 2,
    }
}

fn list_kind_from_code(code: usize) -> ListKind {
    match code {
        1 => ListKind::Bullet,
        2 => ListKind::Numbered,
        _ => ListKind::None,
    }
}

// A buffer line's `AttrsList` defaults `metadata` packs two per-paragraph
// properties that must survive edits (cosmic preserves the defaults across
// `split_off`/`append`): the list kind in the low byte, and a per-line line
// spacing override (multiplier × 1000, 0 = inherit the document default) in the
// high bits. Image placeholder lines instead store the image id and never carry
// either property.
const LIST_CODE_MASK: usize = 0xFF;
const SPACING_SHIFT: usize = 8;

/// Encode a line-spacing multiplier for the metadata high bits.
fn spacing_to_code(mult: f32) -> usize {
    (mult * 1000.0).round().max(0.0) as usize
}

/// Decode a per-line spacing override; `None` when the line inherits the default.
fn code_to_spacing(code: usize) -> Option<f32> {
    (code != 0).then(|| code as f32 / 1000.0)
}

/// The per-line spacing override on `line`, if it sets one.
fn line_spacing_override(line: &cosmic_text::BufferLine) -> Option<f32> {
    if line.text() == IMAGE_PLACEHOLDER {
        return None;
    }
    code_to_spacing(line.attrs_list().defaults().metadata >> SPACING_SHIFT)
}

/// The Unicode object-replacement character marks a line that holds one block
/// image. Its glyph is drawn transparent; the picture is painted as an overlay.
const IMAGE_PLACEHOLDER: &str = "\u{FFFC}";
/// Default displayed width for a freshly inserted image (clamped to the column).
const DEFAULT_IMAGE_WIDTH_MM: f32 = 60.0;

/// A picture chosen by the user, waiting to be inserted at the caret. Filled by
/// the app's file-dialog worker via [`queue_image`], drained next frame.
struct PendingImage {
    data: Vec<u8>,
    natural_w: u32,
    natural_h: u32,
}

thread_local! {
    /// Images picked via the file dialog, pending insertion into the active doc.
    static IMAGE_INBOX: RefCell<Vec<PendingImage>> = const { RefCell::new(Vec::new()) };
    /// A pending "focus this object" request from the Layers panel.
    static FOCUS_INBOX: RefCell<Option<(DocumentId, FlowTextFocus)>> =
        const { RefCell::new(None) };
}

/// Ask the document editor to focus an object (the whole text, or an image by
/// ordinal) on the next frame. Called by the app when a Layers-panel row is
/// clicked; drained in [`build`].
pub fn request_focus(doc_id: DocumentId, focus: FlowTextFocus) {
    FOCUS_INBOX.with(|cell| *cell.borrow_mut() = Some((doc_id, focus)));
}

/// Hand a loaded picture to the document editor; it is inserted at the caret on
/// the next frame. Called by the app after its file-dialog worker decodes one.
pub fn queue_image(data: Vec<u8>, natural_w: u32, natural_h: u32) {
    IMAGE_INBOX.with(|inbox| {
        inbox.borrow_mut().push(PendingImage {
            data,
            natural_w,
            natural_h,
        });
    });
}

/// A character-style flag that can be toggled over the selection.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CharToggle {
    Bold,
    Italic,
    Underline,
}

/// The character formatting that varies per run in the editor: emphasis plus an
/// optional explicit colour (`None` = the document's default ink). Font and size
/// are uniform across the document, so they are not carried here.
#[derive(Clone, Copy, PartialEq)]
struct RunFmt {
    bold: bool,
    italic: bool,
    underline: bool,
    color: Option<Color>,
}

impl RunFmt {
    /// True when this run differs from the plain body style and therefore needs
    /// its own attribute span (plain runs rely on the line's default attrs).
    fn is_styled(&self) -> bool {
        self.bold || self.italic || self.underline || self.color.is_some()
    }
}

const DPI: f32 = 96.0;
/// Fallback line spacing when a document does not specify one.
const DEFAULT_LINE_SPACING: f32 = 1.3;
/// Line-spacing presets offered in the toolbar.
const LINE_SPACING_PRESETS: [(f32, &str); 5] = [
    (1.0, "1,0"),
    (1.15, "1,15"),
    (1.3, "1,3"),
    (1.5, "1,5"),
    (2.0, "2,0"),
];
/// Longest side (in device pixels) allowed for a rendered page texture; caps
/// the supersample so an extreme zoom cannot allocate a huge bitmap.
const MAX_PAGE_TEX_PX: f32 = 4096.0;

struct DocRuntime {
    bound_model_revision: u64,
    editor: Option<Editor<'static>>,
    setup: PageSetup,
    /// Body font family; the whole document shares one face (no per-run font
    /// picker yet). Stored so the buffer and the model round-trip faithfully.
    font: TextFontFamily,
    font_pt: f32,
    /// Line spacing as a multiple of single spacing, applied document-wide.
    line_spacing: f32,
    /// Display zoom multiplier on top of fit-to-window (1.0 = fit).
    zoom: f32,
    /// True while a click-drag selection is in progress (so the anchor Click is
    /// always sent before the first Drag — otherwise the anchor is stale).
    drag_active: bool,
    page_index: usize,
    page_count: usize,
    /// Cached current-page texture, keyed by `(content revision, page, render
    /// scale key)` so it re-rasterises when the content, page or zoom changes.
    tex: Option<(u64, usize, u32, egui::TextureHandle)>,
    /// Bumped whenever the text content changes, to invalidate `tex`.
    revision: u64,
    /// Image blocks referenced by placeholder lines, keyed by the id stored in
    /// the placeholder glyph's `Attrs` metadata.
    images: HashMap<usize, ImageBlock>,
    next_image_id: usize,
    /// Lazily-built egui textures for image blocks, keyed by image id. Floating
    /// images reuse this map under keys offset by `FLOATING_TEX_BASE`.
    image_tex: HashMap<usize, egui::TextureHandle>,
    /// The image id currently being dragged on the page (realign / reorder).
    dragging_image: Option<usize>,
    /// Floating images (Word-style wrapping), in document order. Not part of the
    /// text buffer; drawn as overlays and round-tripped via `floating_images`.
    floating: Vec<ImageBlock>,
    /// The selected floating image (index into `floating`), if any.
    selected_floating: Option<usize>,
    /// An in-progress floating-image transform (move or resize by a handle).
    float_drag: Option<FloatDrag>,
}

/// The kind of drag acting on the selected floating image.
#[derive(Clone, Copy)]
enum FloatDrag {
    /// Moving the whole image; the field is the grab offset (mm) from its corner.
    Move { dx_mm: f32, dy_mm: f32 },
    /// Resizing from a corner handle (0=TL, 1=TR, 2=BR, 3=BL).
    Resize { corner: u8 },
}

impl Default for DocRuntime {
    fn default() -> Self {
        Self {
            bound_model_revision: 0,
            editor: None,
            setup: PageSetup::default(),
            font: CharStyle::default().font,
            font_pt: 13.0,
            line_spacing: DEFAULT_LINE_SPACING,
            zoom: 1.0,
            drag_active: false,
            page_index: 0,
            page_count: 1,
            tex: None,
            revision: 0,
            images: HashMap::new(),
            next_image_id: 1,
            image_tex: HashMap::new(),
            dragging_image: None,
            floating: Vec::new(),
            selected_floating: None,
            float_drag: None,
        }
    }
}

/// Texture-key offset so floating images do not collide with inline image ids in
/// `image_tex`. Floating image at index `i` uses key `FLOATING_TEX_BASE + i`.
const FLOATING_TEX_BASE: usize = 1_000_000;

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
        // Insert any pictures the file dialog handed us (queued via `queue_image`).
        let pending: Vec<PendingImage> =
            IMAGE_INBOX.with(|inbox| inbox.borrow_mut().drain(..).collect());
        for image in pending {
            insert_image_at_caret(d, font_system, image);
        }
        // Focus an object requested from the Layers panel (text or an image).
        let focus = FOCUS_INBOX.with(|cell| cell.borrow_mut().take());
        if let Some((focus_id, target)) = focus {
            if focus_id == data.doc.id {
                apply_focus(d, font_system, target);
            }
        }

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
fn line_height(font_pt: f32, line_spacing: f32) -> f32 {
    font_pt * DPI / 72.0 * line_spacing
}

fn base_metrics(font_pt: f32, line_spacing: f32) -> Metrics {
    let px = font_pt * DPI / 72.0;
    Metrics::new(px, px * line_spacing.max(0.1))
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
    d.font = view.document.default_char.font.clone();
    d.font_pt = view.document.default_char.size_pt;
    d.line_spacing = if view.document.default_para.line_spacing > 0.0 {
        view.document.default_para.line_spacing
    } else {
        DEFAULT_LINE_SPACING
    };
    // Rebuild the image store with fresh ids for this buffer generation.
    d.images.clear();
    d.image_tex.clear();
    d.next_image_id = 1;
    // Floating images come straight from the model (no line references).
    d.floating = view.document.floating_images.clone();
    if d.selected_floating.is_some_and(|i| i >= d.floating.len()) {
        d.selected_floating = None;
    }
    let mut image_ids: Vec<Option<usize>> = Vec::with_capacity(view.document.paragraphs.len());
    let buffer_text = view
        .document
        .paragraphs
        .iter()
        .map(|p| {
            if let Some(block) = &p.image {
                let id = d.next_image_id;
                d.next_image_id += 1;
                d.images.insert(id, block.clone());
                image_ids.push(Some(id));
                IMAGE_PLACEHOLDER
            } else {
                image_ids.push(None);
                ""
            }
        })
        .collect::<Vec<_>>();
    // Text lines are the paragraph text; images stand in as a placeholder char.
    let joined = view
        .document
        .paragraphs
        .iter()
        .zip(&buffer_text)
        .map(|(p, ph)| {
            if ph.is_empty() {
                p.text()
            } else {
                ph.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    let cw = d.setup.content_width_px(DPI);
    let mut buffer = Buffer::new(fs, base_metrics(d.font_pt, d.line_spacing));
    buffer.set_size(Some(cw), None);
    let font_name = d.font.name().to_string();
    let attrs = Attrs::new().family(Family::Name(font_name.as_str()));
    buffer.set_text(&joined, &attrs, Shaping::Advanced, None);
    for (i, (line, paragraph)) in buffer
        .lines
        .iter_mut()
        .zip(&view.document.paragraphs)
        .enumerate()
    {
        line.set_align(Some(match paragraph.style.align {
            ParagraphAlign::Left => Align::Left,
            ParagraphAlign::Center => Align::Center,
            ParagraphAlign::Right => Align::Right,
            ParagraphAlign::Justify => Align::Justified,
        }));
        // Image line: attach the placeholder attrs (id + tall metrics).
        if let Some(Some(id)) = image_ids.get(i) {
            if let Some(block) = d.images.get(id) {
                let (_, h_px) = image_display_px(block, &d.setup);
                let ph_attrs = placeholder_attrs(font_name.as_str(), *id, h_px);
                line.set_attrs_list(AttrsList::new(&ph_attrs));
                line.set_align(Some(Align::Center));
            }
            continue;
        }
        // Reproduce per-run emphasis / colour as attribute spans. Size stays on
        // the buffer metrics (not the spans) so the global size control keeps
        // working; underline is drawn by the renderer from these spans.
        if paragraph.runs.iter().any(|r| fmt_of(&r.style).is_styled()) {
            let mut list = AttrsList::new(&attrs);
            let mut byte = 0usize;
            for run in &paragraph.runs {
                let len = run.text.len();
                let fmt = fmt_of(&run.style);
                if len > 0 && fmt.is_styled() {
                    list.add_span(byte..byte + len, &styled_attrs(font_name.as_str(), fmt));
                }
                byte += len;
            }
            line.set_attrs_list(list);
        }
        // Store the list kind in the line's attrs defaults (preserved across
        // edits, unlike BufferLine::set_metadata which cosmic clears on insert).
        let list_code = list_kind_to_code(paragraph.style.list);
        if list_code != 0 {
            set_line_list_code(line, list_code);
        }
        // A per-paragraph line spacing that differs from the document default is
        // stored as a per-line override (applied by `refresh_line_spacing`).
        if paragraph.style.line_spacing > 0.0
            && (paragraph.style.line_spacing - d.line_spacing).abs() > 1e-4
        {
            set_line_spacing_code(line, spacing_to_code(paragraph.style.line_spacing));
        }
    }
    buffer.shape_until_scroll(fs, false);
    d.editor = Some(Editor::new(buffer));
    // Narrow the list lines so their hanging indent shows from the first frame.
    relayout_list_lines(d.editor.as_mut().expect("editor"), fs, cw);
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
    // A popup (colour picker, a combo) open coming into this frame swallows the
    // click that dismisses it. Capture that here, before the toolbar renders and
    // closes it, so the dismiss click does not also land on the page and move
    // the caret — which would drop the selection the user is still formatting.
    let popup_open = ctx.any_popup_open();

    // --- Toolbar (Phosphor icons) ---
    let mut reshape = false;
    let mut align_cmd: Option<Align> = None;
    let mut char_toggle: Option<CharToggle> = None;
    let mut char_color: Option<Color> = None;
    let mut spacing_cmd: Option<f32> = None;
    let mut list_cmd: Option<ListKind> = None;
    // Image commands from the contextual image row (see below).
    let mut img_align_cmd: Option<ParagraphAlign> = None;
    let mut img_width_cmd: Option<f32> = None;
    let mut img_move_cmd: Option<i32> = None;
    let mut img_delete_cmd = false;
    // Image layout-mode conversions + floating-image controls.
    let mut img_to_floating: Option<ImageWrap> = None;
    let mut float_set_wrap: Option<ImageWrap> = None;
    let mut float_width_cmd: Option<f32> = None;
    let mut float_delete_cmd = false;
    let cur = current_fmt(d.editor.as_ref().expect("editor"));
    let cur_list = caret_list_kind(d.editor.as_ref().expect("editor"));
    let cur_spacing = caret_spacing(d.editor.as_ref().expect("editor"), d.line_spacing);
    let active_image = active_image_id(d.editor.as_ref().expect("editor"));
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

        // Character emphasis (applies to the selection).
        if ui
            .selectable_label(cur.bold, ph::TEXT_B)
            .on_hover_text("Đậm (Ctrl+B)")
            .clicked()
        {
            char_toggle = Some(CharToggle::Bold);
        }
        if ui
            .selectable_label(cur.italic, ph::TEXT_ITALIC)
            .on_hover_text("Nghiêng (Ctrl+I)")
            .clicked()
        {
            char_toggle = Some(CharToggle::Italic);
        }
        if ui
            .selectable_label(cur.underline, ph::TEXT_UNDERLINE)
            .on_hover_text("Gạch chân (Ctrl+U)")
            .clicked()
        {
            char_toggle = Some(CharToggle::Underline);
        }

        // Text colour (applies to the selection).
        let base = cur.color.unwrap_or(Color::BLACK);
        let mut rgba = egui::Color32::from_rgb(base.r, base.g, base.b);
        if ui
            .color_edit_button_srgba(&mut rgba)
            .on_hover_text("Màu chữ")
            .changed()
        {
            char_color = Some(Color::new(rgba.r(), rgba.g(), rgba.b(), 255));
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

        // Lists (apply to the selected paragraphs).
        if ui
            .selectable_label(cur_list == ListKind::Bullet, ph::LIST_BULLETS)
            .on_hover_text("Danh sách chấm")
            .clicked()
        {
            list_cmd = Some(ListKind::Bullet);
        }
        if ui
            .selectable_label(cur_list == ListKind::Numbered, ph::LIST_NUMBERS)
            .on_hover_text("Danh sách đánh số")
            .clicked()
        {
            list_cmd = Some(ListKind::Numbered);
        }
        ui.separator();

        // Line spacing (applies to the selected paragraphs).
        let spacing_label = LINE_SPACING_PRESETS
            .iter()
            .find(|(v, _)| (v - cur_spacing).abs() < 1e-3)
            .map(|(_, l)| *l)
            .unwrap_or("—");
        ui.label(ph::ARROWS_VERTICAL)
            .on_hover_text("Giãn dòng (đoạn đang chọn)");
        egui::ComboBox::from_id_salt("doc_line_spacing")
            .selected_text(spacing_label)
            .show_ui(ui, |ui| {
                for (v, label) in LINE_SPACING_PRESETS {
                    if ui
                        .selectable_label((v - cur_spacing).abs() < 1e-3, label)
                        .clicked()
                    {
                        spacing_cmd = Some(v);
                    }
                }
            });
        ui.separator();

        // Insert a picture at the caret (letterhead, signature, stamp).
        if ui.button(ph::IMAGE).on_hover_text("Chèn ảnh").clicked() {
            actions.doc.pick_flow_text_image = true;
        }
        ui.separator();

        // Mail merge: fill {{field}} placeholders from a CSV/Excel file and
        // export one PDF per row.
        if ui
            .button(ph::ENVELOPE_SIMPLE)
            .on_hover_text("Trộn thư — xuất hàng loạt từ Excel/CSV (chỗ giữ chỗ {{Tên cột}})")
            .clicked()
        {
            actions.doc.start_mail_merge = true;
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

    // Contextual image row: shown when the caret sits on an inline image block,
    // so the picture can be aligned, resized, moved, converted or removed. A
    // selected floating image takes priority (its own row below).
    if let Some(id) = active_image.filter(|_| d.selected_floating.is_none()) {
        let (cur_align, cur_width) = d
            .images
            .get(&id)
            .map(|b| (b.align, b.width_mm))
            .unwrap_or((ParagraphAlign::Center, DEFAULT_IMAGE_WIDTH_MM));
        ui.horizontal_wrapped(|ui| {
            ui.label(ph::IMAGE).on_hover_text("Ảnh đang chọn");
            // Word-style layout mode: in line with text vs floating on top.
            egui::ComboBox::from_id_salt("doc_img_wrap_inline")
                .selected_text("Cùng dòng chữ")
                .show_ui(ui, |ui| {
                    let _ = ui.selectable_label(true, "Cùng dòng chữ");
                    if ui.selectable_label(false, "Nổi trên chữ").clicked() {
                        img_to_floating = Some(ImageWrap::InFrontOfText);
                    }
                    if ui.selectable_label(false, "Nổi sau chữ").clicked() {
                        img_to_floating = Some(ImageWrap::BehindText);
                    }
                });
            ui.separator();
            for (icon, a, tip) in [
                (ph::TEXT_ALIGN_LEFT, ParagraphAlign::Left, "Ảnh sát trái"),
                (
                    ph::TEXT_ALIGN_CENTER,
                    ParagraphAlign::Center,
                    "Ảnh căn giữa",
                ),
                (ph::TEXT_ALIGN_RIGHT, ParagraphAlign::Right, "Ảnh sát phải"),
            ] {
                if ui
                    .selectable_label(cur_align == a, icon)
                    .on_hover_text(tip)
                    .clicked()
                {
                    img_align_cmd = Some(a);
                }
            }
            ui.separator();
            ui.label("Rộng").on_hover_text("Chiều rộng ảnh (mm)");
            let mut width_mm = cur_width;
            if ui
                .add(
                    egui::DragValue::new(&mut width_mm)
                        .range(10.0..=d.setup.content_width_mm().max(10.0))
                        .speed(0.5)
                        .suffix(" mm"),
                )
                .changed()
            {
                img_width_cmd = Some(width_mm);
            }
            ui.separator();
            if ui
                .button(ph::ARROW_UP)
                .on_hover_text("Đưa ảnh lên trên")
                .clicked()
            {
                img_move_cmd = Some(-1);
            }
            if ui
                .button(ph::ARROW_DOWN)
                .on_hover_text("Đưa ảnh xuống dưới")
                .clicked()
            {
                img_move_cmd = Some(1);
            }
            ui.separator();
            if ui.button(ph::TRASH).on_hover_text("Xoá ảnh").clicked() {
                img_delete_cmd = true;
            }
        });
    }

    // Contextual row for a selected floating image (Word "in front of text").
    if let Some(fi) = d.selected_floating {
        if let Some((cur_width, cur_wrap)) = d.floating.get(fi).map(|b| (b.width_mm, b.wrap)) {
            let wrap_label = if cur_wrap == ImageWrap::BehindText {
                "Nổi sau chữ"
            } else {
                "Nổi trên chữ"
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(ph::IMAGE).on_hover_text("Ảnh nổi đang chọn");
                egui::ComboBox::from_id_salt("doc_img_wrap_float")
                    .selected_text(wrap_label)
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(false, "Cùng dòng chữ").clicked() {
                            float_set_wrap = Some(ImageWrap::Inline);
                        }
                        if ui
                            .selectable_label(cur_wrap == ImageWrap::InFrontOfText, "Nổi trên chữ")
                            .clicked()
                        {
                            float_set_wrap = Some(ImageWrap::InFrontOfText);
                        }
                        if ui
                            .selectable_label(cur_wrap == ImageWrap::BehindText, "Nổi sau chữ")
                            .clicked()
                        {
                            float_set_wrap = Some(ImageWrap::BehindText);
                        }
                    });
                ui.separator();
                ui.label("Rộng").on_hover_text("Chiều rộng ảnh (mm)");
                let mut width_mm = cur_width;
                if ui
                    .add(
                        egui::DragValue::new(&mut width_mm)
                            .range(10.0..=400.0)
                            .speed(0.5)
                            .suffix(" mm"),
                    )
                    .changed()
                {
                    float_width_cmd = Some(width_mm);
                }
                ui.separator();
                ui.label("Kéo ảnh để di chuyển; kéo góc để đổi cỡ.");
                ui.separator();
                if ui.button(ph::TRASH).on_hover_text("Xoá ảnh").clicked() {
                    float_delete_cmd = true;
                }
            });
        }
    }

    if reshape {
        apply_reshape(d, fs);
    }
    if let Some(a) = align_cmd {
        apply_align(d, fs, a);
    }
    if let Some(toggle) = char_toggle {
        apply_char_style(d, fs, toggle);
    }
    if let Some(color) = char_color {
        apply_char_color(d, fs, color);
    }
    if let Some(spacing) = spacing_cmd {
        apply_line_spacing(d, fs, spacing);
    }
    if let Some(kind) = list_cmd {
        apply_list(d, fs, kind);
    }
    if let Some(id) = active_image {
        if let Some(a) = img_align_cmd {
            apply_image_align(d, fs, id, a);
        }
        if let Some(w) = img_width_cmd {
            apply_image_width(d, fs, id, w);
        }
        if let Some(dir) = img_move_cmd {
            apply_image_move(d, fs, id, dir);
        }
        if img_delete_cmd {
            apply_image_delete(d, fs, id);
        }
        if let Some(wrap) = img_to_floating {
            convert_inline_to_floating(d, fs, id, wrap);
        }
    }
    if let Some(fi) = d.selected_floating {
        if let Some(wrap) = float_set_wrap {
            if wrap == ImageWrap::Inline {
                convert_floating_to_inline(d, fs, fi);
            } else if let Some(block) = d.floating.get_mut(fi) {
                if block.wrap != wrap {
                    block.wrap = wrap;
                    d.revision = d.revision.wrapping_add(1);
                }
            }
        } else if let Some(w) = float_width_cmd {
            if let Some(block) = d.floating.get_mut(fi) {
                block.width_mm = w.clamp(10.0, 400.0);
                d.revision = d.revision.wrapping_add(1);
            }
        } else if float_delete_cmd {
            if fi < d.floating.len() {
                d.floating.remove(fi);
                d.selected_floating = None;
                clear_floating_textures(d);
                d.revision = d.revision.wrapping_add(1);
            }
        }
    }

    // --- Page geometry (layout px at 96 dpi) ---
    let (cx, cy, cw, ch) = d.setup.content_rect_px(DPI);
    let page_w = d.setup.paper.width_px(DPI);
    let page_h = d.setup.paper.height_px(DPI);
    let lh = line_height(d.font_pt, d.line_spacing);

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

    // Rasterise the page at the real device-pixel resolution so glyphs are crisp
    // (a fixed 96-dpi bitmap upsampled by the display scale looks blurry). One
    // texel maps to one device pixel: render_scale = on-screen scale × the
    // window's points-to-pixels factor, capped so a zoomed-in sheet can't blow up
    // the texture — beyond the cap egui falls back to a mild linear upscale.
    let max_render_scale = (MAX_PAGE_TEX_PX / page_w.max(page_h)).max(1.0);
    let render_scale = {
        let raw = (scale * ctx.pixels_per_point()).clamp(1.0, max_render_scale);
        (raw * 4.0).round() / 4.0 // quantise to 0.25 steps so resizing doesn't thrash the cache
    };

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

            let font_name = d.font.name().to_string();
            let font_pt = d.font_pt;
            let doc_line_spacing = d.line_spacing;

            // Floating images (Word-style, in front of text) take the pointer
            // first: select / move / resize free of the text flow.
            let float_consumed = if focused && !popup_open {
                handle_floating_pointer(d, &response, rect, scale, d.page_index)
            } else {
                false
            };

            // Image drag: grab a picture and drag it to realign (by horizontal
            // third) or reorder between paragraphs (vertical). Runs before text
            // hit-testing so dragging a picture never selects text.
            let mut image_drag = false;
            if focused && !popup_open && !float_consumed {
                let pointer = response.interact_pointer_pos().map(|p| {
                    let bx = ((p.x - rect.min.x) / scale - cx).max(0.0);
                    let by = d.page_index as f32 * ch + (p.y - rect.min.y) / scale - cy;
                    (bx, by)
                });
                if response.drag_started() {
                    if let Some((bx, by)) = pointer {
                        if let Some((id, line)) = image_at(d, cw, bx, by) {
                            d.dragging_image = Some(id);
                            if let Some(e) = d.editor.as_mut() {
                                e.set_selection(Selection::None);
                                e.set_cursor(Cursor::new(line, 0));
                            }
                            image_drag = true;
                        }
                    }
                } else if response.dragged() {
                    if let (Some(id), Some((bx, by))) = (d.dragging_image, pointer) {
                        image_drag = true;
                        let third = cw / 3.0;
                        let align = if bx < third {
                            ParagraphAlign::Left
                        } else if bx > 2.0 * third {
                            ParagraphAlign::Right
                        } else {
                            ParagraphAlign::Center
                        };
                        apply_image_align(d, fs, id, align);
                        let cur = find_image_line(d.editor.as_ref().expect("editor"), id);
                        let target = line_at_y(d.editor.as_ref().expect("editor"), by);
                        if let (Some(cur), Some(target)) = (cur, target) {
                            if target > cur {
                                apply_image_move(d, fs, id, 1);
                            } else if target < cur {
                                apply_image_move(d, fs, id, -1);
                            }
                        }
                    }
                }
                if response.drag_stopped() {
                    d.dragging_image = None;
                }
            }

            let (
                image,
                sel_rects,
                caret,
                page_count,
                page_index,
                revision,
                image_lines,
                caret_on_list,
            ) = {
                let editor = d.editor.as_mut().expect("editor");
                let page_index = d.page_index;
                let mut dirty = false;

                // Pointer → caret / selection. `drag_active` guarantees an anchor
                // Click precedes the first Drag, so a drag selects only its span.
                // Skip while a popup was dismissed this frame: that click is for
                // the popup, not a caret move (keeps the selection during colour).
                // Also skip while dragging an image (handled above).
                if focused && !popup_open && !image_drag && !float_consumed {
                    if response.drag_started() {
                        if let Some(p) = response.interact_pointer_pos() {
                            let (mut x, y) = map(p, page_index);
                            x -= list_indent_at_y(editor, y); // list lines are shifted right
                            editor.action(fs, Action::Click { x, y });
                        }
                        d.drag_active = true;
                    } else if response.dragged() {
                        if let Some(p) = response.interact_pointer_pos() {
                            let (mut x, y) = map(p, page_index);
                            x -= list_indent_at_y(editor, y); // list lines are shifted right
                            if d.drag_active {
                                editor.action(fs, Action::Drag { x, y });
                            } else {
                                editor.action(fs, Action::Click { x, y });
                                d.drag_active = true;
                            }
                        }
                    } else if response.clicked() {
                        if let Some(p) = response.interact_pointer_pos() {
                            let (mut x, y) = map(p, page_index);
                            x -= list_indent_at_y(editor, y); // list lines are shifted right
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
                        // An image line is an atomic block: typing is ignored,
                        // Backspace/Delete removes the picture.
                        let on_image = line_is_image(editor, editor.cursor().line);
                        match ev {
                            egui::Event::Text(t) if !t.is_empty() && !on_image => {
                                editor.insert_string(&t, None);
                                dirty = true;
                            }
                            egui::Event::Paste(t) if !t.is_empty() && !on_image => {
                                editor.insert_string(&t, None);
                                dirty = true;
                            }
                            egui::Event::Ime(egui::ImeEvent::Commit(t))
                                if !t.is_empty() && !on_image =>
                            {
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
                                    egui::Key::Backspace | egui::Key::Delete if on_image => {
                                        // Remove the picture: delete the placeholder char.
                                        let line = editor.cursor().line;
                                        editor.set_cursor(Cursor::new(line, 0));
                                        editor.action(fs, Action::Delete);
                                        dirty = true;
                                    }
                                    egui::Key::Enter if on_image => {} // atomic block: no split
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
                                    egui::Key::B if modifiers.command => {
                                        dirty |= toggle_selection_style(
                                            editor,
                                            fs,
                                            &font_name,
                                            CharToggle::Bold,
                                        );
                                    }
                                    egui::Key::I if modifiers.command => {
                                        dirty |= toggle_selection_style(
                                            editor,
                                            fs,
                                            &font_name,
                                            CharToggle::Italic,
                                        );
                                    }
                                    egui::Key::U if modifiers.command => {
                                        dirty |= toggle_selection_style(
                                            editor,
                                            fs,
                                            &font_name,
                                            CharToggle::Underline,
                                        );
                                    }
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                    }
                }

                editor.shape_as_needed(fs, false);
                // Give paragraphs with a custom line spacing their per-line line
                // height (via metrics_opt), recomputed against the current size.
                refresh_line_spacing(editor, fs, font_pt * DPI / 72.0);
                // Edits re-lay list lines at the global width; narrow them again
                // so the hanging indent and pagination stay correct.
                relayout_list_lines(editor, fs, cw);
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

                let rs_key = (render_scale * 256.0) as u32;
                let need = d.tex.as_ref().map(|(r, p, k, _)| (*r, *p, *k))
                    != Some((revision, page_index, rs_key));
                let image = if need {
                    let (px, tw, th) = render_page(
                        editor,
                        fs,
                        swash_cache,
                        page_index,
                        cx,
                        cy,
                        ch,
                        page_w,
                        page_h,
                        render_scale,
                        &font_name,
                        font_pt,
                        doc_line_spacing,
                    );
                    Some((
                        egui::ColorImage::from_rgba_unmultiplied([tw, th], &px),
                        rs_key,
                    ))
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
                            // List lines are shifted right; match the highlight.
                            let ox = cx
                                + if line_list_code(&b.lines[run.line_i]) != 0 {
                                    list_indent_px()
                                } else {
                                    0.0
                                };
                            for (hx, hw) in run.highlight(start, end) {
                                let min = rect.min + egui::vec2((ox + hx) * scale, top * scale);
                                let max = min + egui::vec2(hw.max(2.0) * scale, lh * scale);
                                sel_rects.push(egui::Rect::from_min_max(min, max));
                            }
                        }
                    });
                }
                // Image placeholder lines on this page: `(id, line_top, height)`
                // in content pixels. The picture itself is painted as an overlay;
                // list markers are drawn into the page texture by `render_page`.
                let mut image_lines: Vec<(usize, f32, f32)> = Vec::new();
                editor.with_buffer(|b| {
                    for run in b.layout_runs() {
                        if run.text == IMAGE_PLACEHOLDER
                            && (run.line_top / ch).floor() as usize == page_index
                        {
                            if let Some(g) = run.glyphs.first() {
                                image_lines.push((g.metadata, run.line_top, run.line_height));
                            }
                        }
                    }
                });
                let caret = editor.cursor_position();
                let caret_on_list = {
                    let cl = editor.cursor().line;
                    editor.with_buffer(|b| {
                        b.lines
                            .get(cl)
                            .map(|l| line_list_code(l) != 0)
                            .unwrap_or(false)
                    })
                };
                (
                    image,
                    sel_rects,
                    caret,
                    page_count,
                    page_index,
                    revision,
                    image_lines,
                    caret_on_list,
                )
            };

            d.page_index = page_index;
            d.page_count = page_count;
            d.revision = revision;
            if let Some((img, rs_key)) = image {
                let tex = ctx.load_texture("iai_doc_page", img, egui::TextureOptions::LINEAR);
                d.tex = Some((revision, page_index, rs_key, tex));
            }

            // Build floating textures before painting (paint reads them).
            ensure_floating_textures(ctx, d, page_index);

            // Paint order: white paper → behind-text images → the (transparent)
            // text texture → selection → inline images → in-front images → caret.
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 0.0, egui::Color32::WHITE);
            for i in 0..d.floating.len() {
                if d.floating[i].page == page_index && d.floating[i].wrap == ImageWrap::BehindText {
                    paint_floating_image(&painter, d, i, rect, scale);
                }
            }
            if let Some((_, _, _, tex)) = &d.tex {
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                painter.image(tex.id(), rect, uv, egui::Color32::WHITE);
            }
            let sel_color = egui::Color32::from_rgba_unmultiplied(60, 120, 240, 70);
            for r in sel_rects {
                painter.rect_filled(r, 0.0, sel_color);
            }
            // Paint image blocks over their (transparent) placeholder lines.
            for (id, line_top, _lh) in image_lines {
                let Some(block) = d.images.get(&id) else {
                    continue;
                };
                let (w_px, h_px) = image_display_px(block, &d.setup);
                let need_tex = !d.image_tex.contains_key(&id);
                let data = if need_tex {
                    Some(block.data.clone())
                } else {
                    None
                };
                if let Some(bytes) = data {
                    if let Some(ci) = decode_color_image(&bytes) {
                        let tex = ctx.load_texture(
                            format!("iai_doc_img_{id}"),
                            ci,
                            egui::TextureOptions::LINEAR,
                        );
                        d.image_tex.insert(id, tex);
                    }
                }
                if let Some(tex) = d.image_tex.get(&id) {
                    let top = cy + line_top - page_index as f32 * ch;
                    let x = rect.min.x + (cx + image_align_offset(block.align, cw, w_px)) * scale;
                    let y = rect.min.y + top * scale;
                    let img_rect = egui::Rect::from_min_size(
                        egui::pos2(x, y),
                        egui::vec2(w_px * scale, h_px * scale),
                    );
                    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                    painter.image(tex.id(), img_rect, uv, egui::Color32::WHITE);
                    // Outline the selected / dragged image so it reads as picked
                    // up and its contextual controls make sense.
                    if active_image == Some(id) || d.dragging_image == Some(id) {
                        painter.rect_stroke(
                            img_rect.expand(2.0),
                            2.0,
                            egui::Stroke::new(2.0, egui::Color32::from_rgb(60, 120, 240)),
                            egui::StrokeKind::Outside,
                        );
                    }
                }
            }

            // Floating images that sit in front of the text (everything except
            // BehindText, which was drawn under the text texture above).
            for i in 0..d.floating.len() {
                if d.floating[i].page == page_index && d.floating[i].wrap != ImageWrap::BehindText {
                    paint_floating_image(&painter, d, i, rect, scale);
                }
            }
            let caret_indent = if caret_on_list { list_indent_px() } else { 0.0 };
            if focused {
                if let Some((qx, qy)) = caret {
                    if (qy as f32 / ch).floor() as usize == page_index {
                        let x = rect.min.x + (cx + caret_indent + qx as f32) * scale;
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
    let metrics = base_metrics(d.font_pt, d.line_spacing);
    let editor = d.editor.as_mut().expect("editor");
    editor.with_buffer_mut(|b| {
        b.set_metrics(metrics);
        b.set_size(Some(cw), None);
    });
    editor.shape_as_needed(fs, false);
    d.revision = d.revision.wrapping_add(1);
}

/// Set the line spacing of the paragraphs the selection touches (or the caret
/// line). Spacing that equals the document default clears the override so the
/// line follows the buffer's global metrics; other values are stored per line
/// and applied via `refresh_line_spacing`.
fn apply_line_spacing(d: &mut DocRuntime, fs: &mut FontSystem, spacing: f32) {
    let spacing = spacing.max(0.1);
    let default = d.line_spacing;
    let base_px = d.font_pt * DPI / 72.0;
    let editor = d.editor.as_mut().expect("editor");
    let (l0, l1) = match editor.selection_bounds() {
        Some((s, e)) => (s.line, e.line),
        None => {
            let c = editor.cursor();
            (c.line, c.line)
        }
    };
    let code = if (spacing - default).abs() < 1e-4 {
        0
    } else {
        spacing_to_code(spacing)
    };
    editor.with_buffer_mut(|b| {
        for i in l0..=l1 {
            if let Some(line) = b.lines.get_mut(i) {
                if line.text() == IMAGE_PLACEHOLDER {
                    continue;
                }
                set_line_spacing_code(line, code);
            }
        }
    });
    editor.shape_as_needed(fs, false);
    refresh_line_spacing(editor, fs, base_px);
    d.revision = d.revision.wrapping_add(1);
}

/// The effective line spacing on the caret's line (its override or the default).
fn caret_spacing(editor: &Editor<'static>, default: f32) -> f32 {
    let line = editor.cursor().line;
    editor.with_buffer(|b| {
        b.lines
            .get(line)
            .and_then(line_spacing_override)
            .unwrap_or(default)
    })
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

/// Map a model [`CharStyle`] to the editor's per-run format. A pure-black colour
/// is represented as `None` so plain body text keeps the softened ink look and
/// does not need an explicit colour span.
fn fmt_of(style: &CharStyle) -> RunFmt {
    RunFmt {
        bold: style.bold,
        italic: style.italic,
        underline: style.underline,
        color: (style.color != Color::BLACK).then_some(style.color),
    }
}

/// Attrs for a run: body face plus emphasis, underline and colour. Size is
/// intentionally left on the buffer metrics so the global font-size control is
/// not pinned per span.
fn styled_attrs(font_name: &str, fmt: RunFmt) -> Attrs<'_> {
    let mut attrs = Attrs::new()
        .family(Family::Name(font_name))
        .weight(if fmt.bold {
            Weight::BOLD
        } else {
            Weight::NORMAL
        })
        .style(if fmt.italic {
            Style::Italic
        } else {
            Style::Normal
        });
    if fmt.underline {
        attrs = attrs.underline(UnderlineStyle::Single);
    }
    if let Some(c) = fmt.color {
        attrs = attrs.color(CtColor::rgba(c.r, c.g, c.b, c.a));
    }
    attrs
}

/// Read the formatting at a byte position within a line's attr list.
fn span_fmt(list: &AttrsList, index: usize) -> RunFmt {
    let a = list.get_span(index);
    RunFmt {
        bold: a.weight == Weight::BOLD,
        italic: a.style == Style::Italic,
        underline: a.text_decoration.underline != UnderlineStyle::None,
        color: a.color_opt.map(|c| Color::new(c.r(), c.g(), c.b(), c.a())),
    }
}

/// The selected byte range on `line_i`, clamped to `[0, line_len]`.
fn line_selection_range(
    line_i: usize,
    start: Cursor,
    end: Cursor,
    line_len: usize,
) -> std::ops::Range<usize> {
    let lo = if line_i == start.line { start.index } else { 0 };
    let hi = if line_i == end.line {
        end.index
    } else {
        line_len
    };
    lo.min(line_len)..hi.min(line_len)
}

/// Formatting to reflect in the toolbar. For a selection, a flag is "on" only
/// when every selected character has it, and the colour is reported only when
/// uniform; with no selection it is the style new typing would inherit (the
/// character before the caret).
fn current_fmt(editor: &Editor<'static>) -> RunFmt {
    editor.with_buffer(|b| {
        if let Some((start, end)) = editor.selection_bounds() {
            let mut any = false;
            let (mut bold, mut italic, mut underline) = (true, true, true);
            let mut color: Option<Option<Color>> = None; // None = not seen; Some(x) = uniform x
            for li in start.line..=end.line {
                let Some(line) = b.lines.get(li) else {
                    continue;
                };
                let text = line.text();
                let range = line_selection_range(li, start, end, text.len());
                let list = line.attrs_list();
                for (i, _) in text.char_indices() {
                    if i < range.start || i >= range.end {
                        continue;
                    }
                    let f = span_fmt(list, i);
                    bold &= f.bold;
                    italic &= f.italic;
                    underline &= f.underline;
                    color = match color {
                        None => Some(f.color),
                        Some(c) if c == f.color => Some(c),
                        Some(_) => Some(None), // mixed colours -> treat as default
                    };
                    any = true;
                }
            }
            if any {
                RunFmt {
                    bold,
                    italic,
                    underline,
                    color: color.flatten(),
                }
            } else {
                RunFmt {
                    bold: false,
                    italic: false,
                    underline: false,
                    color: None,
                }
            }
        } else {
            let c = editor.cursor();
            match b.lines.get(c.line) {
                Some(line) if c.index > 0 => span_fmt(line.attrs_list(), c.index - 1),
                _ => RunFmt {
                    bold: false,
                    italic: false,
                    underline: false,
                    color: None,
                },
            }
        }
    })
}

/// Toggle bold / italic / underline across the current selection (word-processor
/// rule: if every selected character already has it, clear it; otherwise set
/// it). Other attributes and per-character boundaries are preserved. A caret
/// with no selection is a no-op — there is no pending-style buffer yet.
fn apply_char_style(d: &mut DocRuntime, fs: &mut FontSystem, toggle: CharToggle) {
    let font_name = d.font.name().to_string();
    let editor = d.editor.as_mut().expect("editor");
    if toggle_selection_style(editor, fs, &font_name, toggle) {
        d.revision = d.revision.wrapping_add(1);
    }
}

/// Set the text colour of the current selection (`Color::BLACK` clears back to
/// the default ink). A caret with no selection is a no-op.
fn apply_char_color(d: &mut DocRuntime, fs: &mut FontSystem, color: Color) {
    let font_name = d.font.name().to_string();
    let target = (color != Color::BLACK).then_some(color);
    let editor = d.editor.as_mut().expect("editor");
    if restyle_selection(editor, fs, &font_name, |mut f| {
        f.color = target;
        f
    }) {
        d.revision = d.revision.wrapping_add(1);
    }
}

/// Core of [`apply_char_style`], operating directly on the editor so the
/// keyboard handler can reuse it while it already holds the editor borrow.
fn toggle_selection_style(
    editor: &mut Editor<'static>,
    fs: &mut FontSystem,
    font_name: &str,
    toggle: CharToggle,
) -> bool {
    let Some((start, end)) = editor.selection_bounds() else {
        return false;
    };
    // Decide the target value once: clear only if every selected char has it.
    let (any, all_set) = editor.with_buffer(|b| {
        let mut any = false;
        let mut all_set = true;
        for li in start.line..=end.line {
            let Some(line) = b.lines.get(li) else {
                continue;
            };
            let text = line.text();
            let range = line_selection_range(li, start, end, text.len());
            let list = line.attrs_list();
            for (i, _) in text.char_indices() {
                if i < range.start || i >= range.end {
                    continue;
                }
                any = true;
                let f = span_fmt(list, i);
                let set = match toggle {
                    CharToggle::Bold => f.bold,
                    CharToggle::Italic => f.italic,
                    CharToggle::Underline => f.underline,
                };
                all_set &= set;
            }
        }
        (any, all_set)
    });
    if !any {
        return false;
    }
    let make = !all_set;
    restyle_selection(editor, fs, font_name, |mut f| {
        match toggle {
            CharToggle::Bold => f.bold = make,
            CharToggle::Italic => f.italic = make,
            CharToggle::Underline => f.underline = make,
        }
        f
    })
}

/// Rebuild the attr spans of every line the selection touches, mapping each
/// selected character's [`RunFmt`] through `map` and leaving everything else
/// intact. Returns whether any line actually changed.
fn restyle_selection(
    editor: &mut Editor<'static>,
    fs: &mut FontSystem,
    font_name: &str,
    map: impl Fn(RunFmt) -> RunFmt,
) -> bool {
    let Some((start, end)) = editor.selection_bounds() else {
        return false;
    };
    let mut changed = false;
    editor.with_buffer_mut(|b| {
        for li in start.line..=end.line {
            let Some(line) = b.lines.get(li) else {
                continue;
            };
            if line.text() == IMAGE_PLACEHOLDER {
                continue; // never restyle an image placeholder line
            }
            let text = line.text().to_string();
            let range = line_selection_range(li, start, end, text.len());
            let old = line.attrs_list();
            // Preserve the line's list kind (stored in the defaults metadata) so
            // colour/emphasis changes don't clear the bullet/number.
            let list_code = old.defaults().metadata;
            let base = Attrs::new()
                .family(Family::Name(font_name))
                .metadata(list_code);
            let mut list = AttrsList::new(&base);

            let mut seg: Option<(usize, RunFmt)> = None; // (start_byte, fmt)
            let flush = |list: &mut AttrsList, seg: (usize, RunFmt), stop: usize| {
                let (s, fmt) = seg;
                if fmt.is_styled() {
                    list.add_span(s..stop, &styled_attrs(font_name, fmt));
                }
            };
            for (i, _) in text.char_indices() {
                let mut fmt = span_fmt(old, i);
                if i >= range.start && i < range.end {
                    fmt = map(fmt);
                }
                match seg {
                    Some((_, f)) if f == fmt => {}
                    Some(prev) => {
                        flush(&mut list, prev, i);
                        seg = Some((i, fmt));
                    }
                    None => seg = Some((i, fmt)),
                }
            }
            if let Some(prev) = seg {
                flush(&mut list, prev, text.len());
            }
            if b.lines[li].set_attrs_list(list) {
                changed = true;
            }
        }
    });
    if changed {
        editor.shape_as_needed(fs, false);
    }
    changed
}

/// Horizontal offset (content px) of an image of width `w` in a column of width
/// `cw`, per its paragraph alignment. Mirrors the PDF/preview `align_offset`.
fn image_align_offset(align: ParagraphAlign, cw: f32, w: f32) -> f32 {
    match align {
        ParagraphAlign::Center => ((cw - w) * 0.5).max(0.0),
        ParagraphAlign::Right => (cw - w).max(0.0),
        _ => 0.0,
    }
}

/// The id of the image on the caret's line, if that line is an image block.
fn active_image_id(editor: &Editor<'static>) -> Option<usize> {
    let line = editor.cursor().line;
    editor.with_buffer(|b| {
        let line = b.lines.get(line)?;
        if line.text() == IMAGE_PLACEHOLDER {
            Some(line.attrs_list().get_span(0).metadata)
        } else {
            None
        }
    })
}

/// The image `(id, line)` whose displayed rectangle contains the content-space
/// point `(bx, by)` (buffer pixels, `by` continuous across pages), if any.
fn image_at(d: &DocRuntime, cw: f32, bx: f32, by: f32) -> Option<(usize, usize)> {
    let editor = d.editor.as_ref()?;
    editor.with_buffer(|b| {
        for run in b.layout_runs() {
            if run.text != IMAGE_PLACEHOLDER {
                continue;
            }
            if by < run.line_top || by > run.line_top + run.line_height {
                continue;
            }
            let Some(g) = run.glyphs.first() else {
                continue;
            };
            let id = g.metadata;
            let Some(block) = d.images.get(&id) else {
                continue;
            };
            let (w_px, _) = image_display_px(block, &d.setup);
            let ox = image_align_offset(block.align, cw, w_px);
            if bx >= ox && bx <= ox + w_px {
                return Some((id, run.line_i));
            }
        }
        None
    })
}

/// The page index a buffer line lays out on (`ch` = content height px).
fn line_page(editor: &Editor<'static>, line: usize, ch: f32) -> usize {
    editor.with_buffer(|b| {
        for run in b.layout_runs() {
            if run.line_i == line {
                return (run.line_top / ch).floor().max(0.0) as usize;
            }
        }
        0
    })
}

/// Focus an object from the Layers panel: place the caret on the text body's
/// first line, or on the nth image's line, and page to it. Selecting an image
/// line makes `active_image` pick it up so its on-page controls appear.
fn apply_focus(d: &mut DocRuntime, fs: &mut FontSystem, target: FlowTextFocus) {
    if let FlowTextFocus::FloatingImage(idx) = target {
        if let Some(block) = d.floating.get(idx) {
            d.page_index = block.page;
            d.selected_floating = Some(idx);
        }
        return;
    }
    d.selected_floating = None;
    let editor = d.editor.as_mut().expect("editor");
    let line = editor.with_buffer(|b| match target {
        FlowTextFocus::Text => b.lines.iter().position(|l| l.text() != IMAGE_PLACEHOLDER),
        FlowTextFocus::Image(ordinal) => {
            let mut n = 0;
            for (i, l) in b.lines.iter().enumerate() {
                if l.text() == IMAGE_PLACEHOLDER {
                    if n == ordinal {
                        return Some(i);
                    }
                    n += 1;
                }
            }
            None
        }
        FlowTextFocus::FloatingImage(_) => None, // handled above
    });
    let Some(line) = line else {
        return;
    };
    editor.set_selection(Selection::None);
    editor.set_cursor(Cursor::new(line, 0));
    editor.shape_as_needed(fs, false);
    let ch = d.setup.content_height_px(DPI);
    d.page_index = line_page(d.editor.as_ref().expect("editor"), line, ch);
}

/// The buffer line whose laid-out rows contain the content-space y `by`.
fn line_at_y(editor: &Editor<'static>, by: f32) -> Option<usize> {
    editor.with_buffer(|b| {
        for run in b.layout_runs() {
            if by >= run.line_top && by < run.line_top + run.line_height {
                return Some(run.line_i);
            }
        }
        None
    })
}

/// Displayed pixel size of an image block at the editor DPI, clamped so the
/// width never exceeds the text column (height follows the aspect ratio).
fn image_display_px(block: &ImageBlock, setup: &PageSetup) -> (f32, f32) {
    let cw = setup.content_width_px(DPI);
    let mut w = block.width_mm / 25.4 * DPI;
    let mut h = block.height_mm() / 25.4 * DPI;
    if w > cw && w > 0.0 {
        let s = cw / w;
        w = cw;
        h *= s;
    }
    (w.max(1.0), h.max(1.0))
}

/// Attrs for an image placeholder glyph: invisible ink, the image id in the
/// metadata, and a line height equal to the displayed image height so the line
/// reserves the picture's vertical space.
fn placeholder_attrs(font_name: &str, id: usize, h_px: f32) -> Attrs<'_> {
    Attrs::new()
        .family(Family::Name(font_name))
        .metadata(id)
        .color(CtColor::rgba(0, 0, 0, 0))
        .metrics(Metrics::new(8.0, h_px.max(1.0)))
}

/// Decode image bytes into an egui `ColorImage`, capping the longest side so a
/// large source photo does not become an oversized GPU texture.
fn decode_color_image(bytes: &[u8]) -> Option<egui::ColorImage> {
    const MAX_SIDE: u32 = 1600;
    let rgba = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = rgba.dimensions();
    let rgba = if w.max(h) > MAX_SIDE && w.max(h) > 0 {
        let scale = MAX_SIDE as f32 / w.max(h) as f32;
        image::imageops::resize(
            &rgba,
            (w as f32 * scale).round().max(1.0) as u32,
            (h as f32 * scale).round().max(1.0) as u32,
            image::imageops::FilterType::Triangle,
        )
    } else {
        rgba
    };
    let (w, h) = rgba.dimensions();
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        rgba.as_raw(),
    ))
}

/// The list code for a buffer line, stored in the line's `AttrsList` defaults
/// metadata. Unlike `BufferLine::set_metadata` (cleared by `split_off`/`append`
/// on every edit), the attrs defaults are preserved across edits — and copied to
/// the new line on Enter, so a list continues. Image placeholder lines never
/// count as lists (their defaults metadata holds the image id instead).
fn line_list_code(line: &cosmic_text::BufferLine) -> usize {
    if line.text() == IMAGE_PLACEHOLDER {
        return 0;
    }
    line.attrs_list().defaults().metadata & LIST_CODE_MASK
}

/// Rebuild `line`'s attr list so its defaults carry list `code` (low byte),
/// preserving the spacing bits, the family and any character spans.
fn set_line_list_code(line: &mut cosmic_text::BufferLine, code: usize) {
    let new_list = {
        let old = line.attrs_list();
        let meta = (old.defaults().metadata & !LIST_CODE_MASK) | (code & LIST_CODE_MASK);
        let new_defaults = old.defaults().metadata(meta);
        let mut nl = AttrsList::new(&new_defaults);
        for (range, attrs) in old.spans() {
            nl.add_span(range.clone(), &attrs.as_attrs());
        }
        nl
    };
    line.set_attrs_list(new_list);
}

/// Rebuild `line`'s attr list so its defaults carry the spacing `code` (high
/// bits), preserving the list byte, the family and any character spans.
fn set_line_spacing_code(line: &mut cosmic_text::BufferLine, code: usize) {
    let new_list = {
        let old = line.attrs_list();
        let meta = (old.defaults().metadata & LIST_CODE_MASK) | ((code & 0xFFFF) << SPACING_SHIFT);
        let new_defaults = old.defaults().metadata(meta);
        let mut nl = AttrsList::new(&new_defaults);
        for (range, attrs) in old.spans() {
            nl.add_span(range.clone(), &attrs.as_attrs());
        }
        nl
    };
    line.set_attrs_list(new_list);
}

/// After each shape, give every line that overrides its spacing a per-line line
/// height via `metrics_opt` on the defaults and every span (so even a fully
/// emphasised line keeps the override), and clear it from lines that inherit the
/// document default (which use the buffer's global metrics). Recomputed against
/// the current `base_px` so a global font-size change stays consistent. Runs
/// only when a line actually differs, so it does not force reshaping every frame.
fn refresh_line_spacing(editor: &mut Editor<'static>, fs: &mut FontSystem, base_px: f32) {
    let mut changed = false;
    editor.with_buffer_mut(|b| {
        for line in b.lines.iter_mut() {
            if line.text() == IMAGE_PLACEHOLDER {
                continue; // image lines carry their own placeholder metrics
            }
            let desired: Option<cosmic_text::CacheMetrics> = line_spacing_override(line)
                .map(|m| Metrics::new(base_px, (base_px * m).max(1.0)).into());
            if line.attrs_list().defaults().metrics_opt == desired {
                continue;
            }
            let new_list = {
                let old = line.attrs_list();
                let mut def = old.defaults();
                def.metrics_opt = desired;
                let mut nl = AttrsList::new(&def);
                for (range, attrs) in old.spans() {
                    let mut a = attrs.as_attrs();
                    a.metrics_opt = desired;
                    nl.add_span(range.clone(), &a);
                }
                nl
            };
            line.set_attrs_list(new_list);
            changed = true;
        }
    });
    if changed {
        editor.shape_as_needed(fs, false);
    }
}

/// Re-lay every list line at the narrowed (hanging-indent) width so `layout_runs`
/// reflects it. cosmic-text lays all lines at the buffer's global width; this
/// overrides just the list lines after each shape (image lines are excluded).
fn relayout_list_lines(editor: &mut Editor<'static>, fs: &mut FontSystem, content_w: f32) {
    let narrow = (content_w - list_indent_px()).max(1.0);
    editor.with_buffer_mut(|b| {
        let fss = b.metrics().font_size;
        let wrap = b.wrap();
        let ell = b.ellipsize();
        let mono = b.monospace_width();
        let tab = b.tab_width();
        let hint = b.hinting();
        for line in b.lines.iter_mut() {
            if line_list_code(line) != 0 {
                line.reset_layout();
                line.layout(fs, fss, Some(narrow), wrap, ell, mono, tab, hint);
            }
        }
    });
}

/// The list indent (px, rounded) applied to the line at buffer-y `by`, so a
/// click on a list line can be mapped back into the narrowed layout.
fn list_indent_at_y(editor: &Editor<'static>, by: i32) -> i32 {
    let y = by as f32;
    editor.with_buffer(|b| {
        for run in b.layout_runs() {
            if y >= run.line_top && y < run.line_top + run.line_height {
                return if line_list_code(&b.lines[run.line_i]) != 0 {
                    list_indent_px().round() as i32
                } else {
                    0
                };
            }
        }
        0
    })
}

/// The list kind currently on the caret's line (for the toolbar toggle state).
fn caret_list_kind(editor: &Editor<'static>) -> ListKind {
    let line = editor.cursor().line;
    editor.with_buffer(|b| {
        b.lines
            .get(line)
            .map(|l| list_kind_from_code(line_list_code(l)))
            .unwrap_or(ListKind::None)
    })
}

/// Toggle a list kind over every line the selection touches (or the caret line).
/// Toggling the kind that is already set clears it.
fn apply_list(d: &mut DocRuntime, fs: &mut FontSystem, kind: ListKind) {
    let content_w = d.setup.content_width_px(DPI);
    let editor = d.editor.as_mut().expect("editor");
    let (l0, l1) = match editor.selection_bounds() {
        Some((s, e)) => (s.line, e.line),
        None => {
            let c = editor.cursor();
            (c.line, c.line)
        }
    };
    let target = list_kind_to_code(kind);
    editor.with_buffer_mut(|b| {
        for i in l0..=l1 {
            if let Some(line) = b.lines.get_mut(i) {
                if line.text() == IMAGE_PLACEHOLDER {
                    continue; // images are never list items
                }
                let now = line_list_code(line);
                let next = if now == target { 0 } else { target };
                set_line_list_code(line, next);
                line.set_align(Some(Align::Left)); // lists read left-aligned
            }
        }
    });
    relayout_list_lines(editor, fs, content_w);
    d.revision = d.revision.wrapping_add(1);
}

/// True if buffer line `line_i` is an image placeholder line.
fn line_is_image(editor: &Editor<'static>, line_i: usize) -> bool {
    editor.with_buffer(|b| {
        b.lines
            .get(line_i)
            .is_some_and(|l| l.text() == IMAGE_PLACEHOLDER)
    })
}

/// Insert a picture as its own block at the caret. Stores the block under a new
/// id and inserts a placeholder line carrying that id, with a blank line after
/// so the caret lands on editable text.
fn insert_image_at_caret(d: &mut DocRuntime, fs: &mut FontSystem, image: PendingImage) {
    let cw_mm = d.setup.content_width_mm();
    let block = ImageBlock::inline(
        image.data,
        image.natural_w.max(1),
        image.natural_h.max(1),
        DEFAULT_IMAGE_WIDTH_MM.min(cw_mm),
        ParagraphAlign::Center,
    );
    insert_inline_image_block(d, fs, block);
}

/// Insert an existing image block as an inline paragraph at the caret.
fn insert_inline_image_block(d: &mut DocRuntime, fs: &mut FontSystem, mut block: ImageBlock) {
    block.wrap = ImageWrap::Inline;
    let id = d.next_image_id;
    d.next_image_id += 1;
    let (_, h_px) = image_display_px(&block, &d.setup);
    d.images.insert(id, block);

    let font_name = d.font.name().to_string();
    let editor = d.editor.as_mut().expect("editor");
    // Start the image on its own fresh line, then leave a clean line after it.
    editor.action(fs, Action::Motion(Motion::End));
    editor.insert_string("\n", None);
    let ph_attrs = placeholder_attrs(&font_name, id, h_px);
    editor.insert_string(IMAGE_PLACEHOLDER, Some(AttrsList::new(&ph_attrs)));
    // Center the image line and add a trailing empty text line for the caret.
    let img_line = editor.cursor().line;
    editor.with_buffer_mut(|b| {
        if let Some(line) = b.lines.get_mut(img_line) {
            line.set_align(Some(Align::Center));
        }
    });
    editor.insert_string("\n", None);
    editor.shape_as_needed(fs, false);
    d.revision = d.revision.wrapping_add(1);
}

/// Index of the buffer line holding the image block `id`, if present.
fn find_image_line(editor: &Editor<'static>, id: usize) -> Option<usize> {
    editor.with_buffer(|b| {
        b.lines.iter().position(|l| {
            l.text() == IMAGE_PLACEHOLDER && l.attrs_list().get_span(0).metadata == id
        })
    })
}

/// Set an image block's horizontal alignment within the text column.
fn apply_image_align(d: &mut DocRuntime, fs: &mut FontSystem, id: usize, align: ParagraphAlign) {
    if let Some(block) = d.images.get_mut(&id) {
        if block.align == align {
            return;
        }
        block.align = align;
    } else {
        return;
    }
    let ct_align = match align {
        ParagraphAlign::Center => Align::Center,
        ParagraphAlign::Right => Align::Right,
        _ => Align::Left,
    };
    let editor = d.editor.as_mut().expect("editor");
    if let Some(li) = find_image_line(editor, id) {
        editor.with_buffer_mut(|b| {
            if let Some(line) = b.lines.get_mut(li) {
                line.set_align(Some(ct_align));
            }
        });
    }
    editor.shape_as_needed(fs, false);
    d.revision = d.revision.wrapping_add(1);
}

/// Resize an image block by width (mm); height follows the aspect ratio. The
/// placeholder line's reserved height is updated so the layout re-flows.
fn apply_image_width(d: &mut DocRuntime, fs: &mut FontSystem, id: usize, width_mm: f32) {
    let max_mm = d.setup.content_width_mm();
    let width_mm = width_mm.clamp(10.0, max_mm.max(10.0));
    let h_px = {
        let Some(block) = d.images.get_mut(&id) else {
            return;
        };
        if (block.width_mm - width_mm).abs() < 0.05 {
            return;
        }
        block.width_mm = width_mm;
        image_display_px(block, &d.setup).1
    };
    let font_name = d.font.name().to_string();
    let editor = d.editor.as_mut().expect("editor");
    if let Some(li) = find_image_line(editor, id) {
        editor.with_buffer_mut(|b| {
            if let Some(line) = b.lines.get_mut(li) {
                line.set_attrs_list(AttrsList::new(&placeholder_attrs(&font_name, id, h_px)));
            }
        });
    }
    editor.shape_as_needed(fs, false);
    d.revision = d.revision.wrapping_add(1);
}

/// Move an image block up (`-1`) or down (`+1`) one paragraph by swapping lines.
fn apply_image_move(d: &mut DocRuntime, fs: &mut FontSystem, id: usize, dir: i32) {
    let editor = d.editor.as_mut().expect("editor");
    let Some(li) = find_image_line(editor, id) else {
        return;
    };
    let target = li as i32 + dir;
    let moved = editor.with_buffer_mut(|b| {
        if target < 0 || target as usize >= b.lines.len() {
            return false;
        }
        b.lines.swap(li, target as usize);
        b.lines[li].reset_layout();
        b.lines[target as usize].reset_layout();
        true
    });
    if moved {
        editor.set_selection(Selection::None);
        editor.set_cursor(Cursor::new(target as usize, 0));
        editor.shape_as_needed(fs, false);
        d.revision = d.revision.wrapping_add(1);
    }
}

/// Delete an image block: remove its placeholder line.
fn apply_image_delete(d: &mut DocRuntime, fs: &mut FontSystem, id: usize) {
    let editor = d.editor.as_mut().expect("editor");
    let Some(li) = find_image_line(editor, id) else {
        return;
    };
    editor.set_selection(Selection::None);
    editor.set_cursor(Cursor::new(li, 0));
    editor.action(fs, Action::Delete);
    d.images.remove(&id);
    editor.shape_as_needed(fs, false);
    d.revision = d.revision.wrapping_add(1);
}

// --- Floating images (Word-style "in front of text") -----------------------

/// Millimetres → editor pixels at the canonical DPI.
fn mm_to_px(mm: f32) -> f32 {
    mm / 25.4 * DPI
}

/// Drop cached floating-image textures (keyed by list index) after the floating
/// list is reordered or shortened, so indices no longer point at stale pictures.
fn clear_floating_textures(d: &mut DocRuntime) {
    d.image_tex.retain(|k, _| *k < FLOATING_TEX_BASE);
}

/// Ensure a texture exists for every floating image on `page_index`.
fn ensure_floating_textures(ctx: &egui::Context, d: &mut DocRuntime, page_index: usize) {
    for i in 0..d.floating.len() {
        let key = FLOATING_TEX_BASE + i;
        if d.floating[i].page == page_index && !d.image_tex.contains_key(&key) {
            let bytes = d.floating[i].data.clone();
            if let Some(ci) = decode_color_image(&bytes) {
                let tex = ctx.load_texture(
                    format!("iai_doc_float_{i}"),
                    ci,
                    egui::TextureOptions::LINEAR,
                );
                d.image_tex.insert(key, tex);
            }
        }
    }
}

/// Paint one floating image (texture already built), plus its transform frame
/// and corner handles when it is the selected image.
fn paint_floating_image(
    painter: &egui::Painter,
    d: &DocRuntime,
    i: usize,
    rect: egui::Rect,
    scale: f32,
) {
    let Some(block) = d.floating.get(i) else {
        return;
    };
    let key = FLOATING_TEX_BASE + i;
    let img_rect = egui::Rect::from_min_size(
        egui::pos2(
            rect.min.x + mm_to_px(block.x_mm) * scale,
            rect.min.y + mm_to_px(block.y_mm) * scale,
        ),
        egui::vec2(
            mm_to_px(block.width_mm) * scale,
            mm_to_px(block.height_mm()) * scale,
        ),
    );
    if let Some(tex) = d.image_tex.get(&key) {
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        painter.image(tex.id(), img_rect, uv, egui::Color32::WHITE);
    }
    if d.selected_floating == Some(i) {
        let accent = egui::Color32::from_rgb(60, 120, 240);
        painter.rect_stroke(
            img_rect,
            0.0,
            egui::Stroke::new(1.5, accent),
            egui::StrokeKind::Outside,
        );
        for c in [
            img_rect.left_top(),
            img_rect.right_top(),
            img_rect.right_bottom(),
            img_rect.left_bottom(),
        ] {
            let handle = egui::Rect::from_center_size(c, egui::vec2(8.0, 8.0));
            painter.rect_filled(handle, 1.0, egui::Color32::WHITE);
            painter.rect_stroke(
                handle,
                1.0,
                egui::Stroke::new(1.5, accent),
                egui::StrokeKind::Outside,
            );
        }
    }
}

/// A floating image's page rectangle in millimetres: `(x, y, w, h)`.
fn floating_rect_mm(block: &ImageBlock) -> (f32, f32, f32, f32) {
    (block.x_mm, block.y_mm, block.width_mm, block.height_mm())
}

fn point_in_block(block: &ImageBlock, pmm: (f32, f32)) -> bool {
    let (x, y, w, h) = floating_rect_mm(block);
    pmm.0 >= x && pmm.0 <= x + w && pmm.1 >= y && pmm.1 <= y + h
}

/// Which corner handle (0=TL,1=TR,2=BR,3=BL) is within `r` mm of `pmm`, if any.
fn hit_corner(block: &ImageBlock, pmm: (f32, f32), r: f32) -> Option<u8> {
    let (x, y, w, h) = floating_rect_mm(block);
    let corners = [(x, y), (x + w, y), (x + w, y + h), (x, y + h)];
    corners.iter().enumerate().find_map(|(i, (cx, cy))| {
        ((pmm.0 - cx).abs() <= r && (pmm.1 - cy).abs() <= r).then_some(i as u8)
    })
}

/// Resize a floating block by dragging `corner` to `pmm`, keeping the aspect
/// ratio and holding the opposite corner fixed.
fn resize_floating(block: &mut ImageBlock, corner: u8, pmm: (f32, f32)) {
    let (x, y, w, h) = floating_rect_mm(block);
    let min_w = 8.0;
    let aspect = if w > 0.0 { h / w } else { 1.0 };
    match corner {
        0 => {
            let (ax, ay) = (x + w, y + h);
            let nw = (ax - pmm.0).max(min_w);
            block.width_mm = nw;
            block.x_mm = ax - nw;
            block.y_mm = ay - nw * aspect;
        }
        1 => {
            let ay = y + h;
            let nw = (pmm.0 - x).max(min_w);
            block.width_mm = nw;
            block.y_mm = ay - nw * aspect;
        }
        3 => {
            let ax = x + w;
            let nw = (ax - pmm.0).max(min_w);
            block.width_mm = nw;
            block.x_mm = ax - nw;
        }
        _ => {
            block.width_mm = (pmm.0 - x).max(min_w);
        }
    }
}

/// Handle pointer input for floating images (select / move / resize). Returns
/// true when it consumed the pointer so text/inline handling is skipped.
fn handle_floating_pointer(
    d: &mut DocRuntime,
    response: &egui::Response,
    rect: egui::Rect,
    scale: f32,
    page_index: usize,
) -> bool {
    let Some(p) = response.interact_pointer_pos() else {
        if response.drag_stopped() {
            if d.float_drag.is_some() {
                d.revision = d.revision.wrapping_add(1);
            }
            d.float_drag = None;
        }
        return false;
    };
    let pmm = (
        (p.x - rect.min.x) / scale / DPI * 25.4,
        (p.y - rect.min.y) / scale / DPI * 25.4,
    );
    let handle_mm = 3.5;

    if response.drag_started() || response.clicked() {
        // A handle of the already-selected image starts a resize.
        if let Some(sel) = d.selected_floating {
            if let Some(block) = d.floating.get(sel) {
                if block.page == page_index {
                    if let Some(corner) = hit_corner(block, pmm, handle_mm) {
                        d.float_drag = Some(FloatDrag::Resize { corner });
                        return true;
                    }
                }
            }
        }
        // Otherwise select the top-most floating image under the pointer.
        let mut hit = None;
        for (i, block) in d.floating.iter().enumerate() {
            if block.page == page_index && point_in_block(block, pmm) {
                hit = Some(i);
            }
        }
        if let Some(i) = hit {
            d.selected_floating = Some(i);
            let block = &d.floating[i];
            d.float_drag = Some(FloatDrag::Move {
                dx_mm: pmm.0 - block.x_mm,
                dy_mm: pmm.1 - block.y_mm,
            });
            return true;
        }
        d.selected_floating = None;
        d.float_drag = None;
        return false;
    }
    if response.dragged() {
        if let (Some(sel), Some(drag)) = (d.selected_floating, d.float_drag) {
            if let Some(block) = d.floating.get_mut(sel) {
                match drag {
                    FloatDrag::Move { dx_mm, dy_mm } => {
                        block.x_mm = (pmm.0 - dx_mm).max(0.0);
                        block.y_mm = (pmm.1 - dy_mm).max(0.0);
                    }
                    FloatDrag::Resize { corner } => resize_floating(block, corner, pmm),
                }
                return true;
            }
        }
    }
    if response.drag_stopped() {
        if d.float_drag.is_some() {
            d.revision = d.revision.wrapping_add(1); // commit the move/resize to the model
        }
        d.float_drag = None;
    }
    false
}

/// Convert the inline image `id` into a floating image (`wrap`) at a default
/// position on the current page.
fn convert_inline_to_floating(d: &mut DocRuntime, fs: &mut FontSystem, id: usize, wrap: ImageWrap) {
    let Some(block) = d.images.get(&id).cloned() else {
        return;
    };
    {
        let editor = d.editor.as_mut().expect("editor");
        if let Some(li) = find_image_line(editor, id) {
            editor.set_selection(Selection::None);
            editor.set_cursor(Cursor::new(li, 0));
            editor.action(fs, Action::Delete);
        }
        editor.shape_as_needed(fs, false);
    }
    d.images.remove(&id);
    let mut fb = block;
    fb.wrap = if wrap == ImageWrap::Inline {
        ImageWrap::InFrontOfText
    } else {
        wrap
    };
    fb.page = d.page_index;
    fb.x_mm = d.setup.margins.left_mm + 10.0;
    fb.y_mm = d.setup.margins.top_mm + 10.0;
    d.floating.push(fb);
    d.selected_floating = Some(d.floating.len() - 1);
    d.revision = d.revision.wrapping_add(1);
}

/// Convert the floating image at `idx` back into an inline block at the caret.
fn convert_floating_to_inline(d: &mut DocRuntime, fs: &mut FontSystem, idx: usize) {
    if idx >= d.floating.len() {
        return;
    }
    let mut block = d.floating.remove(idx);
    d.selected_floating = None;
    clear_floating_textures(d);
    block.wrap = ImageWrap::Inline;
    let cw_mm = d.setup.content_width_mm();
    if block.width_mm > cw_mm {
        block.width_mm = cw_mm;
    }
    insert_inline_image_block(d, fs, block);
}

/// Rasterise one page of the editor buffer to opaque white RGBA at
/// `render_scale` device pixels per layout pixel. Returns the pixels and the
/// texture dimensions `(width, height)`. Rendering at the display resolution
/// (rather than a fixed 96 dpi bitmap that egui then upsamples) is what keeps
/// the on-page text crisp.
#[allow(clippy::too_many_arguments)]
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
    render_scale: f32,
    font_name: &str,
    font_pt: f32,
    line_spacing: f32,
) -> (Vec<u8>, usize, usize) {
    let pw = (page_w * render_scale).ceil().max(1.0) as usize;
    let ph = (page_h * render_scale).ceil().max(1.0) as usize;
    // Transparent background: the white paper is painted separately so a
    // `BehindText` floating image can show through the gaps between glyphs.
    let mut buf = vec![0u8; pw * ph * 4];
    let ink = CtColor::rgb(0x1a, 0x1a, 0x1a);
    let list_indent = list_indent_px();
    // Marker text per buffer line (numbered items count up, restart on interrupt).
    let markers: Vec<Option<String>> = editor.with_buffer(|b| {
        let mut out = Vec::with_capacity(b.lines.len());
        let mut num = 0usize;
        for line in &b.lines {
            out.push(match list_kind_from_code(line_list_code(line)) {
                ListKind::None => {
                    num = 0;
                    None
                }
                ListKind::Bullet => {
                    num = 0;
                    Some("•".to_string())
                }
                ListKind::Numbered => {
                    num += 1;
                    Some(format!("{num}."))
                }
            });
        }
        out
    });
    editor.with_buffer(|b| {
        let mut last_line: Option<usize> = None;
        for run in b.layout_runs() {
            let p = (run.line_top / ch).floor() as usize;
            let first_visual = Some(run.line_i) != last_line;
            last_line = Some(run.line_i);
            if p != page {
                continue;
            }
            // List lines are shifted right by the hanging indent.
            let is_list = line_list_code(&b.lines[run.line_i]) != 0;
            let ox = cx + if is_list { list_indent } else { 0.0 };
            let base_y = cy + run.line_y - page as f32 * ch;
            // Draw the list marker (same font + baseline as the body) in the gutter.
            if is_list && first_visual {
                if let Some(Some(text)) = markers.get(run.line_i) {
                    let mut mb = Buffer::new(fs, base_metrics(font_pt, line_spacing));
                    mb.set_size(Some(list_indent.max(1.0)), None);
                    mb.set_text(
                        text,
                        &Attrs::new().family(Family::Name(font_name)),
                        Shaping::Advanced,
                        None,
                    );
                    mb.shape_until_scroll(fs, false);
                    for mr in mb.layout_runs() {
                        for glyph in mr.glyphs {
                            let phys = glyph
                                .physical((cx * render_scale, base_y * render_scale), render_scale);
                            cache.with_pixels(fs, phys.cache_key, ink, |gx, gy, col| {
                                blend_px(&mut buf, pw, ph, phys.x + gx, phys.y + gy, col);
                            });
                        }
                    }
                }
            }
            for glyph in run.glyphs {
                let color = glyph.color_opt.unwrap_or(ink);
                let phys = glyph.physical((ox * render_scale, base_y * render_scale), render_scale);
                cache.with_pixels(fs, phys.cache_key, color, |gx, gy, col| {
                    blend_px(&mut buf, pw, ph, phys.x + gx, phys.y + gy, col);
                });
            }
            // Underline (from the shaped decoration spans). cosmic-text rasterises
            // only glyphs, so the line is drawn here at the font's own metrics.
            for span in run.decorations {
                let underline = span.data.text_decoration.underline;
                if underline == UnderlineStyle::None {
                    continue;
                }
                let Some(glyphs) = run.glyphs.get(span.glyph_range.clone()) else {
                    continue;
                };
                if glyphs.is_empty() {
                    continue;
                }
                let x_min = glyphs.iter().fold(f32::INFINITY, |m, g| m.min(g.x));
                let x_max = glyphs
                    .iter()
                    .fold(f32::NEG_INFINITY, |m, g| m.max(g.x + g.w));
                let width = x_max - x_min;
                if width <= 0.0 {
                    continue;
                }
                let fsz = span.font_size;
                let thickness = (span.data.underline_metrics.thickness * fsz).max(1.0);
                let col = span
                    .data
                    .text_decoration
                    .underline_color_opt
                    .or(span.color_opt)
                    .unwrap_or(ink);
                let uy = base_y - span.data.underline_metrics.offset * fsz;
                let x = (ox + x_min) * render_scale;
                let w = width * render_scale;
                let t = thickness * render_scale;
                fill_rect(&mut buf, pw, ph, x, uy * render_scale, w, t, col);
                if underline == UnderlineStyle::Double {
                    fill_rect(
                        &mut buf,
                        pw,
                        ph,
                        x,
                        (uy + thickness * 2.0) * render_scale,
                        w,
                        t,
                        col,
                    );
                }
            }
        }
    });
    (buf, pw, ph)
}

/// Fill an axis-aligned rectangle (device pixels) with a colour, used for
/// underline decorations.
fn fill_rect(buf: &mut [u8], w: usize, h: usize, x: f32, y: f32, rw: f32, rh: f32, color: CtColor) {
    let x0 = x.round() as i32;
    let y0 = y.round() as i32;
    let x1 = (x + rw).round() as i32;
    let y1 = (y + rh.max(1.0)).round() as i32;
    for py in y0..y1 {
        for px in x0..x1 {
            blend_px(buf, w, h, px, py, color);
        }
    }
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
    // Straight-alpha "over" compositing so the page texture can be transparent
    // where there is no ink (a `BehindText` image shows through those pixels).
    let da = buf[idx + 3] as f32 / 255.0;
    let out_a = a + da * (1.0 - a);
    if out_a <= 0.0 {
        return;
    }
    let src = [color.r() as f32, color.g() as f32, color.b() as f32];
    for (c, s) in src.iter().enumerate() {
        let d = buf[idx + c] as f32;
        buf[idx + c] = ((s * a + d * da * (1.0 - a)) / out_a)
            .round()
            .clamp(0.0, 255.0) as u8;
    }
    buf[idx + 3] = (out_a * 255.0).round() as u8;
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

/// Snapshot the interactive cosmic-text projection back into the canonical
/// engine-agnostic document model: one paragraph per buffer line, split into
/// runs wherever bold / italic changes. This is the single bridge between the
/// editor and the stored model — save, PDF export and reopen all read from it.
fn editor_document(d: &DocRuntime) -> TextDocument {
    let editor = d.editor.as_ref().expect("editor");
    let base_char = CharStyle {
        font: d.font.clone(),
        size_pt: d.font_pt,
        ..CharStyle::default()
    };

    let paragraphs = editor.with_buffer(|buffer| {
        buffer
            .lines
            .iter()
            .map(|line| {
                let align = match line.align().unwrap_or(Align::Left) {
                    Align::Center => ParagraphAlign::Center,
                    Align::Right | Align::End => ParagraphAlign::Right,
                    Align::Justified => ParagraphAlign::Justify,
                    _ => ParagraphAlign::Left,
                };
                let style = ParagraphStyle {
                    align,
                    line_spacing: line_spacing_override(line).unwrap_or(d.line_spacing),
                    list: list_kind_from_code(line_list_code(line)),
                    ..ParagraphStyle::default()
                };
                // An image line carries the picture id in the placeholder's
                // metadata; look the block up in the runtime store.
                if line.text() == IMAGE_PLACEHOLDER {
                    let id = line.attrs_list().get_span(0).metadata;
                    if let Some(block) = d.images.get(&id) {
                        return Paragraph {
                            image: Some(block.clone()),
                            style,
                            runs: Vec::new(),
                        };
                    }
                }
                Paragraph {
                    runs: runs_from_line(line, &base_char),
                    style,
                    image: None,
                }
            })
            .collect()
    });

    TextDocument {
        paragraphs,
        page: d.setup,
        default_char: base_char,
        default_para: ParagraphStyle {
            line_spacing: d.line_spacing,
            ..ParagraphStyle::default()
        },
        floating_images: d.floating.clone(),
    }
}

/// Split one buffer line into runs, coalescing consecutive characters that share
/// bold / italic / underline / colour. Font and size are uniform (`base`).
fn runs_from_line(line: &cosmic_text::BufferLine, base: &CharStyle) -> Vec<Run> {
    let text = line.text();
    let list = line.attrs_list();
    let mut runs: Vec<Run> = Vec::new();
    let mut cur: Option<(String, RunFmt)> = None;
    for (i, ch) in text.char_indices() {
        let fmt = span_fmt(list, i);
        match &mut cur {
            Some((s, f)) if *f == fmt => s.push(ch),
            _ => {
                if let Some((s, f)) = cur.take() {
                    runs.push(styled_run(s, f, base));
                }
                cur = Some((ch.to_string(), fmt));
            }
        }
    }
    if let Some((s, f)) = cur.take() {
        runs.push(styled_run(s, f, base));
    }
    runs
}

fn styled_run(text: String, fmt: RunFmt, base: &CharStyle) -> Run {
    Run::new(
        text,
        CharStyle {
            bold: fmt.bold,
            italic: fmt.italic,
            underline: fmt.underline,
            color: fmt.color.unwrap_or(base.color),
            ..base.clone()
        },
    )
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

    fn buffer_with(text: &str, fs: &mut FontSystem) -> Buffer {
        let mut buffer = Buffer::new(fs, base_metrics(13.0, DEFAULT_LINE_SPACING));
        buffer.set_size(Some(400.0), None);
        let base = Attrs::new().family(Family::Name("Times New Roman"));
        buffer.set_text(text, &base, Shaping::Advanced, None);
        buffer.shape_until_scroll(fs, false);
        buffer
    }

    #[test]
    fn runs_split_where_bold_span_begins_and_ends() {
        let mut fs = FontSystem::new();
        let mut buffer = buffer_with("Hello world", &mut fs);
        let base = Attrs::new().family(Family::Name("Times New Roman"));
        let mut list = AttrsList::new(&base);
        let bold = RunFmt {
            bold: true,
            italic: false,
            underline: false,
            color: None,
        };
        list.add_span(6..11, &styled_attrs("Times New Roman", bold)); // "world"
        buffer.lines[0].set_attrs_list(list);

        let runs = runs_from_line(&buffer.lines[0], &CharStyle::default());
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "Hello ");
        assert!(!runs[0].style.bold);
        assert_eq!(runs[1].text, "world");
        assert!(runs[1].style.bold && !runs[1].style.italic);
    }

    #[test]
    fn toggle_bold_over_selection_sets_then_clears_that_span_only() {
        let mut fs = FontSystem::new();
        let buffer = buffer_with("Xin chào Việt Nam", &mut fs);
        let mut editor = Editor::new(buffer);
        // Select "chào": "Xin " is 4 bytes, "chào" is 5 (à = 2 bytes) -> 4..9.
        editor.set_selection(Selection::Normal(Cursor::new(0, 4)));
        editor.set_cursor(Cursor::new(0, 9));

        assert!(toggle_selection_style(
            &mut editor,
            &mut fs,
            "Times New Roman",
            CharToggle::Bold
        ));
        let cur = current_fmt(&editor);
        assert!(cur.bold && !cur.italic);
        let bold: String = editor
            .with_buffer(|b| runs_from_line(&b.lines[0], &CharStyle::default()))
            .into_iter()
            .filter(|r| r.style.bold)
            .map(|r| r.text)
            .collect();
        assert_eq!(bold, "chào", "only the selected word is bold");

        // Toggling again clears it (every selected char was already bold).
        assert!(toggle_selection_style(
            &mut editor,
            &mut fs,
            "Times New Roman",
            CharToggle::Bold
        ));
        assert!(editor
            .with_buffer(|b| runs_from_line(&b.lines[0], &CharStyle::default()))
            .iter()
            .all(|r| !r.style.bold));
    }

    #[test]
    fn line_spacing_and_list_code_coexist_in_metadata() {
        let mut fs = FontSystem::new();
        let mut buffer = buffer_with("một dòng", &mut fs);
        let line = &mut buffer.lines[0];
        set_line_list_code(line, list_kind_to_code(ListKind::Numbered));
        set_line_spacing_code(line, spacing_to_code(1.5));
        assert_eq!(line_list_code(line), 2);
        assert_eq!(line_spacing_override(line), Some(1.5));
        // Changing the list kind must not disturb the spacing override.
        set_line_list_code(line, list_kind_to_code(ListKind::Bullet));
        assert_eq!(line_list_code(line), 1);
        assert_eq!(line_spacing_override(line), Some(1.5));
        // Clearing spacing (code 0) leaves the list intact.
        set_line_spacing_code(line, 0);
        assert_eq!(line_spacing_override(line), None);
        assert_eq!(line_list_code(line), 1);
    }

    #[test]
    fn custom_line_spacing_sets_per_line_metrics_only_where_overridden() {
        let mut fs = FontSystem::new();
        let buffer = buffer_with("dòng một\ndòng hai", &mut fs);
        let mut editor = Editor::new(buffer);
        let base_px = 13.0 * DPI / 72.0;
        editor.with_buffer_mut(|b| set_line_spacing_code(&mut b.lines[0], spacing_to_code(2.0)));
        refresh_line_spacing(&mut editor, &mut fs, base_px);
        editor.with_buffer(|b| {
            let m0 = b.lines[0]
                .attrs_list()
                .defaults()
                .metrics_opt
                .expect("line 0 has a per-line metric");
            let lh = Metrics::from(m0).line_height;
            assert!((lh - base_px * 2.0).abs() < 0.5, "line height follows 2.0×");
            assert!(
                b.lines[1].attrs_list().defaults().metrics_opt.is_none(),
                "the default-spacing line keeps the global metrics"
            );
        });
    }

    #[test]
    fn bold_and_italic_are_independent_on_the_same_span() {
        let mut fs = FontSystem::new();
        let buffer = buffer_with("alpha beta", &mut fs);
        let mut editor = Editor::new(buffer);
        editor.set_selection(Selection::Normal(Cursor::new(0, 6))); // "beta"
        editor.set_cursor(Cursor::new(0, 10));

        toggle_selection_style(&mut editor, &mut fs, "Times New Roman", CharToggle::Bold);
        toggle_selection_style(&mut editor, &mut fs, "Times New Roman", CharToggle::Italic);
        let styled = editor
            .with_buffer(|b| runs_from_line(&b.lines[0], &CharStyle::default()))
            .into_iter()
            .find(|r| r.text == "beta")
            .expect("styled run");
        assert!(styled.style.bold && styled.style.italic);

        // Clearing bold must leave italic intact.
        toggle_selection_style(&mut editor, &mut fs, "Times New Roman", CharToggle::Bold);
        let styled = editor
            .with_buffer(|b| runs_from_line(&b.lines[0], &CharStyle::default()))
            .into_iter()
            .find(|r| r.text == "beta")
            .expect("styled run");
        assert!(!styled.style.bold && styled.style.italic);
    }

    #[test]
    fn underline_toggles_and_survives_the_model_round_trip() {
        let mut fs = FontSystem::new();
        let buffer = buffer_with("alpha beta", &mut fs);
        let mut editor = Editor::new(buffer);
        editor.set_selection(Selection::Normal(Cursor::new(0, 6))); // "beta"
        editor.set_cursor(Cursor::new(0, 10));

        assert!(toggle_selection_style(
            &mut editor,
            &mut fs,
            "Times New Roman",
            CharToggle::Underline
        ));
        let styled = editor
            .with_buffer(|b| runs_from_line(&b.lines[0], &CharStyle::default()))
            .into_iter()
            .find(|r| r.text == "beta")
            .expect("styled run");
        assert!(styled.style.underline && !styled.style.bold);
        assert!(current_fmt(&editor).underline);
    }

    #[test]
    fn colour_applies_to_selection_and_black_clears_it() {
        let mut fs = FontSystem::new();
        let buffer = buffer_with("alpha beta", &mut fs);
        let mut editor = Editor::new(buffer);
        editor.set_selection(Selection::Normal(Cursor::new(0, 6))); // "beta"
        editor.set_cursor(Cursor::new(0, 10));

        let red = Color::new(220, 30, 30, 255);
        assert!(restyle_selection(
            &mut editor,
            &mut fs,
            "Times New Roman",
            |mut f| {
                f.color = Some(red);
                f
            }
        ));
        let styled = editor
            .with_buffer(|b| runs_from_line(&b.lines[0], &CharStyle::default()))
            .into_iter()
            .find(|r| r.text == "beta")
            .expect("coloured run");
        assert_eq!(styled.style.color, red);

        // Setting the colour span back to None returns "beta" to the default,
        // so the whole line coalesces to one plain run again.
        assert!(restyle_selection(
            &mut editor,
            &mut fs,
            "Times New Roman",
            |mut f| {
                f.color = None;
                f
            }
        ));
        let runs = editor.with_buffer(|b| runs_from_line(&b.lines[0], &CharStyle::default()));
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].style.color, Color::BLACK);
    }

    #[test]
    fn image_paragraph_round_trips_through_the_editor() {
        let mut fs = FontSystem::new();
        let mut d = DocRuntime::default();
        let block = ImageBlock::inline(vec![9, 8, 7, 6], 200, 100, 40.0, ParagraphAlign::Center);
        let mut doc = TextDocument::from_plain_text("Trước ảnh");
        doc.paragraphs.push(Paragraph::image(block.clone()));
        doc.paragraphs
            .push(Paragraph::plain("Sau ảnh", CharStyle::default()));
        let view = FlowTextViewModel {
            document: std::sync::Arc::new(doc),
            revision: 1,
            active_page: 0,
            page_count: 1,
        };
        sync_runtime(&mut d, &mut fs, &view);

        // The middle buffer line is the image placeholder.
        assert!(line_is_image(d.editor.as_ref().unwrap(), 1));
        assert!(!line_is_image(d.editor.as_ref().unwrap(), 0));

        // Reading the editor back reproduces the image paragraph unchanged.
        let back = editor_document(&d);
        assert_eq!(back.paragraphs.len(), 3);
        assert!(back.paragraphs[0].image.is_none());
        assert_eq!(back.paragraphs[1].image.as_ref(), Some(&block));
        assert!(back.paragraphs[2].image.is_none());
    }

    #[test]
    fn list_kind_round_trips_and_toggle_sets_it() {
        let mut fs = FontSystem::new();
        let mut d = DocRuntime::default();
        let mut doc = TextDocument::from_plain_text("Mở đầu");
        let mut p1 = Paragraph::plain("Một", CharStyle::default());
        p1.style.list = ListKind::Numbered;
        let mut p2 = Paragraph::plain("Hai", CharStyle::default());
        p2.style.list = ListKind::Bullet;
        doc.paragraphs.push(p1);
        doc.paragraphs.push(p2);
        let view = FlowTextViewModel {
            document: std::sync::Arc::new(doc),
            revision: 1,
            active_page: 0,
            page_count: 1,
        };
        sync_runtime(&mut d, &mut fs, &view);

        let back = editor_document(&d);
        assert_eq!(back.paragraphs[0].style.list, ListKind::None);
        assert_eq!(back.paragraphs[1].style.list, ListKind::Numbered);
        assert_eq!(back.paragraphs[2].style.list, ListKind::Bullet);

        // Toggling a bullet on the first (caret) line, then off again.
        d.editor.as_mut().unwrap().set_cursor(Cursor::new(0, 0));
        apply_list(&mut d, &mut fs, ListKind::Bullet);
        assert_eq!(
            editor_document(&d).paragraphs[0].style.list,
            ListKind::Bullet
        );
        apply_list(&mut d, &mut fs, ListKind::Bullet);
        assert_eq!(editor_document(&d).paragraphs[0].style.list, ListKind::None);
    }

    #[test]
    fn list_kind_survives_typing_and_enter_continues_it() {
        let mut fs = FontSystem::new();
        let mut d = DocRuntime::default();
        let mut p = Paragraph::plain("Mục", CharStyle::default());
        p.style.list = ListKind::Bullet;
        let view = FlowTextViewModel {
            document: std::sync::Arc::new(TextDocument {
                paragraphs: vec![p],
                ..Default::default()
            }),
            revision: 1,
            active_page: 0,
            page_count: 1,
        };
        sync_runtime(&mut d, &mut fs, &view);

        // Typing on the list line must NOT drop the bullet (the reported bug).
        {
            let editor = d.editor.as_mut().unwrap();
            editor.action(&mut fs, Action::Motion(Motion::End));
            editor.insert_string(" thêm", None);
            editor.shape_as_needed(&mut fs, false);
        }
        assert_eq!(
            editor_document(&d).paragraphs[0].style.list,
            ListKind::Bullet,
            "list kind must survive typing"
        );

        // Enter starts a new paragraph that continues the list.
        {
            let editor = d.editor.as_mut().unwrap();
            editor.action(&mut fs, Action::Enter);
            editor.insert_string("Mục hai", None);
            editor.shape_as_needed(&mut fs, false);
        }
        let back = editor_document(&d);
        assert_eq!(back.paragraphs.len(), 2);
        assert_eq!(back.paragraphs[0].style.list, ListKind::Bullet);
        assert_eq!(
            back.paragraphs[1].style.list,
            ListKind::Bullet,
            "Enter continues the list"
        );
    }

    #[test]
    fn colour_and_emphasis_keep_the_list_number() {
        let mut fs = FontSystem::new();
        let mut d = DocRuntime::default();
        let mut p = Paragraph::plain("alpha beta", CharStyle::default());
        p.style.list = ListKind::Numbered;
        let view = FlowTextViewModel {
            document: std::sync::Arc::new(TextDocument {
                paragraphs: vec![p],
                ..Default::default()
            }),
            revision: 1,
            active_page: 0,
            page_count: 1,
        };
        sync_runtime(&mut d, &mut fs, &view);
        {
            let e = d.editor.as_mut().unwrap();
            e.set_selection(Selection::Normal(Cursor::new(0, 0)));
            e.set_cursor(Cursor::new(0, 10));
        }
        let red = Color::new(220, 30, 30, 255);
        apply_char_color(&mut d, &mut fs, red);
        let back = editor_document(&d);
        assert_eq!(
            back.paragraphs[0].style.list,
            ListKind::Numbered,
            "colouring must not drop the list number"
        );
        assert!(back.paragraphs[0].runs.iter().any(|r| r.style.color == red));

        // Bold (the other restyle path) must also keep the list.
        apply_char_style(&mut d, &mut fs, CharToggle::Bold);
        assert_eq!(
            editor_document(&d).paragraphs[0].style.list,
            ListKind::Numbered
        );
    }

    #[test]
    fn per_line_narrow_layout_override_reflows_that_line() {
        let mut fs = FontSystem::new();
        let mut buffer = Buffer::new(&mut fs, base_metrics(13.0, DEFAULT_LINE_SPACING));
        buffer.set_size(Some(400.0), None);
        let long = "từ ".repeat(80);
        buffer.set_text(
            &long,
            &Attrs::new().family(Family::Name("Times New Roman")),
            Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut fs, false);
        let wide = buffer.layout_runs().count();

        // Re-lay line 0 at a much narrower width; layout_runs must reflect it.
        let fss = buffer.metrics().font_size;
        let wrap = buffer.wrap();
        let ell = buffer.ellipsize();
        let mono = buffer.monospace_width();
        let tab = buffer.tab_width();
        let hint = buffer.hinting();
        {
            let line = &mut buffer.lines[0];
            line.reset_layout();
            line.layout(&mut fs, fss, Some(120.0), wrap, ell, mono, tab, hint);
        }
        let narrow = buffer.layout_runs().count();
        assert!(
            narrow > wide,
            "narrow override should wrap into more visual lines: {narrow} vs {wide}"
        );
    }

    #[test]
    fn caret_with_no_selection_does_not_toggle() {
        let mut fs = FontSystem::new();
        let buffer = buffer_with("plain", &mut fs);
        let mut editor = Editor::new(buffer);
        editor.set_selection(Selection::None);
        editor.set_cursor(Cursor::new(0, 2));
        assert!(!toggle_selection_style(
            &mut editor,
            &mut fs,
            "Times New Roman",
            CharToggle::Bold
        ));
    }

    #[test]
    fn render_page_rasterises_editor_text() {
        let mut fs = FontSystem::new();
        let mut sc = SwashCache::new();
        let setup = PageSetup::default();
        let (cx, cy, _, ch) = setup.content_rect_px(DPI);
        let page_w = setup.paper.width_px(DPI);
        let page_h = setup.paper.height_px(DPI);

        let mut buffer = Buffer::new(&mut fs, base_metrics(13.0, DEFAULT_LINE_SPACING));
        buffer.set_size(Some(setup.content_width_px(DPI)), None);
        buffer.set_text(
            "Cộng hòa xã hội chủ nghĩa Việt Nam",
            &Attrs::new().family(Family::Name("Times New Roman")),
            Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut fs, false);
        let editor = Editor::new(buffer);

        // At 2x device resolution the texture is 2x larger on each side and the
        // reported dimensions match the buffer length.
        let render_scale = 2.0;
        let (px, tw, th) = render_page(
            &editor,
            &mut fs,
            &mut sc,
            0,
            cx,
            cy,
            ch,
            page_w,
            page_h,
            render_scale,
            "Times New Roman",
            13.0,
            DEFAULT_LINE_SPACING,
        );
        assert_eq!(tw, (page_w * render_scale).ceil() as usize);
        assert_eq!(th, (page_h * render_scale).ceil() as usize);
        assert_eq!(px.len(), tw * th * 4);
        // Ink is present as pixels with coverage (alpha) so it composites over
        // the separately-painted white paper.
        assert!(
            px.chunks_exact(4).any(|p| p[3] > 0 && p[0] < 200),
            "expected rasterised ink on the page"
        );
        // The background stays transparent so a BehindText image can show through.
        let corner = &px[0..4];
        assert_eq!(corner[3], 0, "the page background must be transparent");
    }
}

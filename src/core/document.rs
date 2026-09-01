// A Document is the user's unit of work (one open file).
// Wraps Canvas + file metadata. The app holds Vec<Document>, not Canvas directly.
//
// DocumentId is threaded through ToolCtx and EventBus from the start so the
// plugin API never assumes a single document — avoiding a future breaking change.

use crate::core::canvas::Canvas;
use crate::core::page::Page;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DocumentId(pub u32);

/// The backing content hosted by one application document/tab.
///
/// `FlowText` is deliberately additive for the first integration slice: the
/// existing `canvas` field remains as a tiny compatibility shell so image code
/// does not need a risky repository-wide enum migration. Text pages themselves
/// are never materialised as canvases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DocumentKind {
    #[default]
    Canvas,
    FlowText,
}

/// Persistent, lightweight state for a flowing-text document. Layout/editor
/// caches live outside this core type; this is the canonical content used by
/// dirty tracking, save/open and export.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowTextDocumentState {
    document: std::sync::Arc<crate::core::text_document::TextDocument>,
    revision: u64,
    saved_revision: u64,
    /// Session-only page selected in the shared page bar.
    active_page: usize,
    /// Derived by layout; never causes the document to become dirty.
    layout_page_count: usize,
}

impl Default for FlowTextDocumentState {
    fn default() -> Self {
        Self::new(crate::core::text_document::TextDocument::new())
    }
}

impl FlowTextDocumentState {
    pub fn new(document: crate::core::text_document::TextDocument) -> Self {
        Self {
            document: std::sync::Arc::new(document),
            revision: 0,
            saved_revision: 0,
            active_page: 0,
            layout_page_count: 1,
        }
    }

    pub fn document(&self) -> &crate::core::text_document::TextDocument {
        &self.document
    }

    /// Cheap immutable snapshot for the UI read contract. Long documents are
    /// not cloned every frame; mutation uses `Arc::make_mut` below.
    pub fn document_arc(&self) -> std::sync::Arc<crate::core::text_document::TextDocument> {
        self.document.clone()
    }

    /// Mutate canonical text content and mark the document dirty. Callers that
    /// only change caret, selection, view or derived layout must use the
    /// dedicated session-state methods instead.
    pub fn document_mut(&mut self) -> &mut crate::core::text_document::TextDocument {
        self.revision = self.revision.wrapping_add(1);
        std::sync::Arc::make_mut(&mut self.document)
    }

    pub fn replace_document(&mut self, document: crate::core::text_document::TextDocument) {
        if self.document.as_ref() != &document {
            self.document = std::sync::Arc::new(document);
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }

    pub fn mark_saved(&mut self) {
        self.saved_revision = self.revision;
    }

    pub fn active_page(&self) -> usize {
        self.active_page
            .min(self.layout_page_count.saturating_sub(1))
    }

    pub fn set_active_page(&mut self, page: usize) {
        self.active_page = page.min(self.layout_page_count.saturating_sub(1));
    }

    pub fn page_count(&self) -> usize {
        self.layout_page_count.max(1)
    }

    pub fn set_layout_page_count(&mut self, count: usize) {
        self.layout_page_count = count.max(1);
        self.active_page = self.active_page.min(self.layout_page_count - 1);
    }
}

/// Orientation of a ruler guide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuideOrientation {
    /// A horizontal line spanning the canvas at a fixed Y (dragged from the top ruler).
    Horizontal,
    /// A vertical line spanning the canvas at a fixed X (dragged from the left ruler).
    Vertical,
}

/// A single ruler guide. `pos` is the canvas-space coordinate of the line
/// (Y for `Horizontal`, X for `Vertical`).
#[derive(Debug, Clone, Copy)]
pub struct Guide {
    pub orientation: GuideOrientation,
    pub pos: f32,
}

impl DocumentId {
    #[allow(dead_code)]
    pub const NONE: DocumentId = DocumentId(0);
}

/// Marks a document as one page of an imported PDF. Pages of the same PDF share
/// a `group_id`; the navigator strip uses it to page between the sibling tabs.
#[derive(Debug, Clone)]
pub struct PdfPageRef {
    pub group_id: u32,
    pub source: PathBuf,
    pub index: usize,
    pub count: usize,
    /// Import resolution requested by the user. Clean pages can be discarded
    /// from RAM and rasterized from `source` again at this resolution.
    pub requested_dpi: f32,
    /// False while this document is a lightweight lazy-load placeholder.
    pub loaded: bool,
    /// Baseline of the rasterized source layer. If it remains untouched and all
    /// later layers use overlay-safe compositing, export can keep the original
    /// PDF page vector and append only those added layers.
    pub original_width: u32,
    pub original_height: u32,
    pub original_dpi: f32,
    pub original_layer_id: u32,
    pub original_tiles_fingerprint: u64,
}

pub struct PdfCachedPage {
    pub canvas: Canvas,
    pub reference: PdfPageRef,
    pub saved_zoom: f32,
    pub saved_offset_x: f32,
    pub saved_offset_y: f32,
}

/// One non-destructive rectangle painted over every page of a multi-page PDF.
/// Coordinates are normalized in the rendered page's top-left coordinate space,
/// so the same corner is covered even when pages have different pixel sizes.
#[derive(Debug, Clone, PartialEq)]
pub struct PdfGlobalClear {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub color: [u8; 4],
}

impl PdfGlobalClear {
    pub fn from_selection(
        selection: &mut crate::core::selection::Selection,
        canvas_width: u32,
        canvas_height: u32,
        mut color: [u8; 4],
    ) -> Result<Self, String> {
        if !selection.active || canvas_width == 0 || canvas_height == 0 {
            return Err("Hãy tạo vùng chọn chữ nhật trước".to_string());
        }
        selection.refresh_bbox();
        let (bx0, by0, bx1, by1) = selection.bounding_box_cached();
        let x0 = (bx0.floor() as i32).clamp(0, canvas_width as i32) as u32;
        let y0 = (by0.floor() as i32).clamp(0, canvas_height as i32) as u32;
        let x1 = (bx1.ceil() as i32).clamp(0, canvas_width as i32) as u32;
        let y1 = (by1.ceil() as i32).clamp(0, canvas_height as i32) as u32;
        if x1 <= x0 || y1 <= y0 {
            return Err("Vùng chọn nằm ngoài trang".to_string());
        }

        // A global edit must be predictable. A lasso/feathered mask cannot be
        // represented by the compact vector rectangle used for 1,000-page PDFs.
        for y in y0..y1 {
            for x in x0..x1 {
                if selection.sample(x, y) < 0.999 {
                    return Err(
                        "Xóa mọi trang hiện hỗ trợ vùng chọn chữ nhật, Feather = 0".to_string()
                    );
                }
            }
        }

        color[3] = 255;
        Ok(Self {
            x0: x0 as f32 / canvas_width as f32,
            y0: y0 as f32 / canvas_height as f32,
            x1: x1 as f32 / canvas_width as f32,
            y1: y1 as f32 / canvas_height as f32,
            color,
        })
    }

    pub fn pixel_rect(&self, width: u32, height: u32) -> (u32, u32, u32, u32) {
        let x0 = (self.x0.clamp(0.0, 1.0) * width as f32).floor() as u32;
        let y0 = (self.y0.clamp(0.0, 1.0) * height as f32).floor() as u32;
        let x1 = (self.x1.clamp(0.0, 1.0) * width as f32).ceil() as u32;
        let y1 = (self.y1.clamp(0.0, 1.0) * height as f32).ceil() as u32;
        (x0.min(width), y0.min(height), x1.min(width), y1.min(height))
    }
}

pub struct PdfGlobalOverlayCache {
    pub width: u32,
    pub height: u32,
    pub operation_count: usize,
    pub stack: crate::core::layer::LayerStack,
}

pub struct PdfDocumentState {
    pub source: PathBuf,
    /// Materialized embedded PDF used by self-contained `.iai` projects.
    /// Runtime-only; project files keep serializing `source` as the original link.
    pub embedded_source: Option<PathBuf>,
    pub page_count: usize,
    pub selected_pages: Vec<usize>,
    /// Saved checkpoint for page removal/reordering. Keeping this separate from
    /// the active canvas avoids marking a clean source page as raster-edited.
    pub selected_pages_saved: Vec<usize>,
    /// Optional tab names keyed by physical source-page index. The key follows
    /// the page when `selected_pages` is reordered.
    pub page_names: std::collections::BTreeMap<usize, String>,
    pub page_names_saved: std::collections::BTreeMap<usize, String>,
    pub requested_dpi: f32,
    pub active_page: usize,
    /// The active page differs from a clean render of the source (has edits).
    /// Sticky across saves — drives caching-on-navigation and hybrid export.
    ///
    /// NOT derivable from a saved checkpoint: "has ever been edited" survives a
    /// save, whereas dirty is cleared by one. Reconciled from the active
    /// canvas's dirty state by [`Document::reconcile_pdf_page_modified`].
    pub active_page_modified: bool,
    pub edited_pages: std::collections::HashMap<usize, PdfCachedPage>,
    /// Compact, document-level covers applied to every source page. Unlike
    /// `edited_pages`, this stays O(number of operations), not O(page count).
    pub global_clears: Vec<PdfGlobalClear>,
    pub global_clears_saved: Vec<PdfGlobalClear>,
    pub global_clears_redo: Vec<PdfGlobalClear>,
    pub global_overlay_cache: Option<PdfGlobalOverlayCache>,
}

impl PdfDocumentState {
    pub fn effective_source(&self) -> &Path {
        self.embedded_source.as_deref().unwrap_or(&self.source)
    }

    /// Any cached (inactive) page holding edits not yet written to the project
    /// file. The active page lives on [`Document::canvas`], so the whole-document
    /// answer is [`Document::is_modified`].
    pub fn cached_pages_dirty(&self) -> bool {
        self.selected_pages != self.selected_pages_saved
            || self.page_names != self.page_names_saved
            || self.global_clears != self.global_clears_saved
            || self
                .edited_pages
                .values()
                .any(|page| page.canvas.is_dirty())
    }

    pub fn page_display_name(&self, position: usize) -> String {
        self.selected_pages
            .get(position)
            .and_then(|source_index| self.page_names.get(source_index))
            .filter(|name| !name.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| format!("Trang {}", position + 1))
    }

    pub fn display_names(&self) -> Vec<String> {
        (0..self.selected_pages.len())
            .map(|position| self.page_display_name(position))
            .collect()
    }

    pub fn set_page_name(&mut self, position: usize, name: Option<String>) {
        let Some(&source_index) = self.selected_pages.get(position) else {
            return;
        };
        match name.filter(|name| !name.trim().is_empty()) {
            Some(name) => {
                self.page_names.insert(source_index, name);
            }
            None => {
                self.page_names.remove(&source_index);
            }
        }
    }

    pub fn move_page(&mut self, from: usize, to: usize) {
        if from >= self.selected_pages.len() || to >= self.selected_pages.len() || from == to {
            return;
        }
        let page = self.selected_pages.remove(from);
        self.selected_pages.insert(to, page);
    }

    /// Allocate an id outside the physical page range of the source PDF. Such
    /// ids always have a materialized canvas in `edited_pages` and represent a
    /// blank/image/imported-PDF page inserted by the user.
    pub fn allocate_inserted_page_id(&self) -> Option<usize> {
        self.selected_pages
            .iter()
            .copied()
            .chain(self.edited_pages.keys().copied())
            .max()
            .unwrap_or_else(|| self.page_count.saturating_sub(1))
            .max(self.page_count.saturating_sub(1))
            .checked_add(1)
    }

    /// Remove a visible page and its cached edit/name. The active canvas must be
    /// switched away by the caller before removing its physical source index.
    pub fn remove_page(&mut self, position: usize) -> Option<usize> {
        if self.selected_pages.len() <= 1 || position >= self.selected_pages.len() {
            return None;
        }
        let source_index = self.selected_pages.remove(position);
        self.edited_pages.remove(&source_index);
        self.page_names.remove(&source_index);
        Some(source_index)
    }

    pub fn rebuild_global_overlay(&mut self, width: u32, height: u32) {
        if self.global_clears.is_empty() || width == 0 || height == 0 {
            self.global_overlay_cache = None;
            return;
        }
        if self.global_overlay_cache.as_ref().is_some_and(|cache| {
            cache.width == width
                && cache.height == height
                && cache.operation_count == self.global_clears.len()
        }) {
            return;
        }

        let mut stack = crate::core::layer::LayerStack::new(width, height);
        stack.layers.clear();
        for (index, operation) in self.global_clears.iter().enumerate() {
            let (x0, y0, x1, y1) = operation.pixel_rect(width, height);
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            let w = x1 - x0;
            let h = y1 - y0;
            let Some(len) = crate::core::canvas::Canvas::checked_rgba_len(w, h) else {
                continue;
            };
            let mut rgba = vec![0u8; len];
            for pixel in rgba.chunks_exact_mut(4) {
                pixel.copy_from_slice(&operation.color);
            }
            let mut layer = crate::core::layer::Layer::new(
                index as u32,
                "Xóa vùng trên mọi trang PDF",
                width,
                height,
            );
            layer.tiles.write_region(x0, y0, w, h, &rgba);
            layer.locked = true;
            layer.selected = false;
            stack.layers.push(layer);
        }
        stack.active_idx = 0;
        stack.set_next_id(stack.layers.len() as u32);
        self.global_overlay_cache = (!stack.layers.is_empty()).then_some(PdfGlobalOverlayCache {
            width,
            height,
            operation_count: self.global_clears.len(),
            stack,
        });
    }
}

impl PdfPageRef {
    pub fn record_canvas_baseline(&mut self, canvas: &Canvas) {
        self.original_width = canvas.width;
        self.original_height = canvas.height;
        self.original_dpi = canvas.metadata.resolution_ppi;
        if let Some(layer) = canvas.layer_stack.layers.first() {
            self.original_layer_id = layer.id;
            self.original_tiles_fingerprint = layer.tiles.revision_fingerprint();
        }
    }

    /// True when the base (page 0) raster layer is byte-for-byte the imported
    /// PDF page: same size/DPI and an untouched raster layer with default
    /// compositing. When true, edits live purely in the layers above, so export
    /// can keep the original vector page and overlay only those.
    pub fn base_is_pristine(&self, canvas: &Canvas) -> bool {
        use crate::core::layer::{BlendMode, LayerType};

        if !self.loaded
            || canvas.width != self.original_width
            || canvas.height != self.original_height
            || (canvas.metadata.resolution_ppi - self.original_dpi).abs() > 0.01
        {
            return false;
        }
        let Some(base) = canvas.layer_stack.layers.first() else {
            return false;
        };
        base.id == self.original_layer_id
            && base.tiles.revision_fingerprint() == self.original_tiles_fingerprint
            && matches!(base.layer_type, LayerType::Raster)
            && base.visible
            && (base.opacity - 1.0).abs() <= f32::EPSILON
            && base.blend_mode == BlendMode::Normal
            && base.mask.is_none()
            && base.offset == (0, 0)
            && base.parent_id.is_none()
    }

    /// Force [`base_is_pristine`] to report `false` even though the baseline was
    /// just (re)recorded. Used when loading a project whose base layer was known
    /// to be edited at save time, so overlay export correctly falls back to raster.
    pub fn mark_base_dirty(&mut self) {
        self.original_tiles_fingerprint = self.original_tiles_fingerprint.wrapping_add(1);
    }

    pub fn safe_overlay_rgba(&self, canvas: &Canvas) -> Option<Vec<u8>> {
        self.safe_overlay_pdf_parts(canvas).map(|(rgba, _)| rgba)
    }

    /// Split edits above a pristine imported-PDF base into a transparent raster
    /// overlay and native PDF vectors. Promoted Path layers are omitted from the
    /// raster overlay so their cached anti-aliasing cannot leave a jagged twin.
    pub fn safe_overlay_pdf_parts(
        &self,
        canvas: &Canvas,
    ) -> Option<(Vec<u8>, Vec<crate::core::print::PdfVectorObject>)> {
        use crate::core::layer::{BlendMode, LayerType};

        if !self.base_is_pristine(canvas) {
            return None;
        }
        if canvas.layer_stack.layers.iter().skip(1).any(|layer| {
            layer.blend_mode != BlendMode::Normal
                || matches!(layer.layer_type, LayerType::Adjustment(_))
        }) {
            return None;
        }
        let mut overlay_stack = canvas.layer_stack.clone();
        overlay_stack.layers.remove(0);
        overlay_stack.active_idx = overlay_stack
            .active_idx
            .saturating_sub(1)
            .min(overlay_stack.layers.len().saturating_sub(1));
        let mut overlay_canvas = Canvas::new(canvas.width, canvas.height);
        overlay_canvas.layer_stack = overlay_stack;
        overlay_canvas.metadata = canvas.metadata.clone();
        let selection = crate::core::print::collect_pdf_vectors(&overlay_canvas);
        let rgba = crate::core::print::pdf_raster_base(&overlay_canvas, &selection);
        Some((rgba, selection.objects))
    }
}

pub struct Document {
    pub id: DocumentId,
    pub kind: DocumentKind,
    /// Present exactly when `kind == FlowText`. This is the canonical flowing
    /// text model; the 1x1 `canvas` remains dormant compatibility storage.
    pub flow_text: Option<FlowTextDocumentState>,
    pub canvas: Canvas,
    pub path: Option<PathBuf>,
    pub file_modified_at: Option<SystemTime>,
    pub title: String,
    /// View state saved when the user switches to another tab.
    /// zoom = 0.0 means "never been viewed" → triggers fit_to_screen on first show.
    pub saved_zoom: f32,
    pub saved_offset_x: f32,
    pub saved_offset_y: f32,
    /// Ruler guides for this document (canvas-space). Session-only.
    pub guides: Vec<Guide>,
    /// Set when this document is one page of an imported PDF (drives the navigator).
    pub pdf_page: Option<PdfPageRef>,
    /// Multi-page PDF session. Only the active page occupies `canvas`; edited
    /// inactive pages live here, while clean pages are rendered again on demand.
    pub pdf_document: Option<PdfDocumentState>,
    /// Formatted EXIF line ("ISO … · f/… · …") for a RAW import, shown in the
    /// Develop window. Session-only — parsed by the RAW preview worker.
    pub raw_exif: Option<String>,
    /// Which page the page-tab bar is focused on. Index into the page list; the
    /// active page's content lives in [`Self::canvas`].
    pub active_artboard: usize,
    /// Multi-page (Corel/Excel-style) document: one independent [`Canvas`] per
    /// page. EMPTY = a plain single-page document (the one page is `canvas`). When
    /// non-empty, its length is the page count and the slot at `active_artboard`
    /// is `None` — that page is currently checked out into `canvas`; every other
    /// slot holds its page's content. Pages are separate canvases (not regions of
    /// one giant canvas), so a document scales to many pages and shows one at a
    /// time, and an empty page costs almost nothing.
    pub pages: Vec<Option<Canvas>>,
    /// The shared master (background) page composited beneath every page that
    /// opts in (`CanvasMetadata::use_master`). Edit once, applies to all pages.
    /// `None` = no master (the common case; behaviour is unchanged). While the
    /// user is editing the master ([`Self::editing_master`]) this is `None` — the
    /// master is checked out into [`Self::canvas`], exactly like an active page.
    pub master: Option<Box<Canvas>>,
    /// True while the master page is checked out into [`Self::canvas`] for
    /// editing; the real active page is parked in `pages[active_artboard]`.
    pub editing_master: bool,
    /// Memory Milestone M1: this is a RAW document whose heavy full-resolution
    /// buffers (tiles + `develop_source`) have been evicted to a lightweight
    /// thumbnail because it is not the active image. `canvas` holds only the
    /// thumbnail; the RAW at `path` is re-decoded on demand when the user
    /// activates it (the swap-in goes through the existing preview→full path).
    /// `false` for every ordinary document. Session-only, never serialized.
    pub deferred_raw: bool,
}

impl Document {
    /// Charge every resident buffer this document owns to `report`, under a
    /// label derived from `owner` (Memory Milestone M0). Covers the active
    /// canvas plus the multi-page multipliers the plan flags: parked artboard
    /// `pages`, the shared `master`, and any edited PDF pages held in memory.
    pub fn account_memory(&self, report: &mut crate::core::mem_report::MemReport, owner: &str) {
        self.canvas.account_memory(report, owner);
        for page in self.pages.iter().flatten() {
            page.account_memory(report, owner);
        }
        if let Some(master) = &self.master {
            master.account_memory(report, owner);
        }
        if let Some(pdf) = &self.pdf_document {
            for cached in pdf.edited_pages.values() {
                cached.canvas.account_memory(report, owner);
            }
        }
    }

    /// Total resident logical bytes this document owns — Memory Milestone M0.
    pub fn estimated_resident_bytes(&self) -> u64 {
        let mut report = crate::core::mem_report::MemReport::new();
        self.account_memory(&mut report, "");
        report.total()
    }

    pub fn new(id: DocumentId, width: u32, height: u32) -> Self {
        Self {
            id,
            kind: DocumentKind::Canvas,
            flow_text: None,
            canvas: Canvas::new(width, height),
            path: None,
            file_modified_at: None,
            title: "Untitled".to_string(),
            saved_zoom: 0.0,
            saved_offset_x: 0.0,
            saved_offset_y: 0.0,
            guides: Vec::new(),
            pdf_page: None,
            pdf_document: None,
            raw_exif: None,
            active_artboard: 0,
            pages: Vec::new(),
            master: None,
            editing_master: false,
            deferred_raw: false,
        }
    }

    pub fn from_canvas(id: DocumentId, canvas: Canvas, path: Option<PathBuf>) -> Self {
        let title = path
            .as_ref()
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .unwrap_or("Untitled")
            .to_string();
        Self {
            id,
            kind: DocumentKind::Canvas,
            flow_text: None,
            canvas,
            file_modified_at: path.as_deref().and_then(file_modified_at),
            path,
            title,
            saved_zoom: 0.0,
            saved_offset_x: 0.0,
            saved_offset_y: 0.0,
            guides: Vec::new(),
            pdf_page: None,
            pdf_document: None,
            raw_exif: None,
            active_artboard: 0,
            pages: Vec::new(),
            master: None,
            editing_master: false,
            deferred_raw: false,
        }
    }

    /// Create a lightweight flowing-text document without allocating an A4
    /// raster canvas. The 1x1 canvas only preserves the current app invariant
    /// while the viewport/input code is migrated by document kind.
    pub fn new_flow_text(id: DocumentId) -> Self {
        let mut document = Self::new(id, 1, 1);
        document.kind = DocumentKind::FlowText;
        document.flow_text = Some(FlowTextDocumentState::default());
        document.title = "Tài liệu chưa đặt tên".to_string();
        // Document-surface zoom is a fit-relative multiplier; avoid running the
        // canvas first-view fit calculation against the 1x1 compatibility shell.
        document.saved_zoom = 1.0;
        document
    }

    pub fn is_flow_text(&self) -> bool {
        self.kind == DocumentKind::FlowText
    }

    /// The artboards this document renders, never empty — see
    /// [`Canvas::effective_artboards`]. The container lives on the canvas metadata
    /// (so it persists and, later, undoes with the rest of the canvas); these are
    /// thin conveniences for app code that holds a `Document`.
    pub fn effective_artboards(&self) -> Vec<Page> {
        self.canvas.effective_artboards()
    }

    /// How many artboards the document has — always at least one (the implicit
    /// page).
    pub fn artboard_count(&self) -> usize {
        self.canvas.artboard_count()
    }

    /// Whether the document carries explicit artboards (a real multi-page job)
    /// rather than the single implicit page derived from the canvas.
    pub fn has_explicit_artboards(&self) -> bool {
        self.canvas.has_explicit_artboards()
    }

    /// Short display name shown in the tab bar.
    pub fn tab_title(&self) -> &str {
        if let Some(path) = &self.path {
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&self.title)
        } else {
            &self.title
        }
    }

    /// Unsaved changes anywhere in this document (the active canvas, or any
    /// cached PDF page).
    ///
    /// Derived from each canvas's saved checkpoint — never assigned. The old
    /// design carried a hand-set `is_modified` bool on both `Document` and
    /// `App`, which had to be kept in sync by ~100 call sites and still got
    /// undo-back-to-saved wrong.
    pub fn is_modified(&self) -> bool {
        self.flow_text.as_ref().is_some_and(|text| text.is_dirty())
            || self.canvas.is_dirty()
            || self.pages.iter().flatten().any(|c| c.is_dirty())
            || self.master.as_ref().is_some_and(|m| m.is_dirty())
            || self
                .pdf_document
                .as_ref()
                .is_some_and(|pdf| pdf.cached_pages_dirty())
    }

    /// Unsaved changes on the page the user is looking at.
    pub fn active_is_modified(&self) -> bool {
        self.flow_text.as_ref().is_some_and(|text| text.is_dirty()) || self.canvas.is_dirty()
    }

    /// Anchor every canvas in this document to "clean". Called after a
    /// successful write; keeps the page canvases and their sticky
    /// differs-from-original state intact.
    pub fn mark_saved(&mut self) {
        if let Some(text) = self.flow_text.as_mut() {
            text.mark_saved();
        }
        self.canvas.mark_saved();
        for page in self.pages.iter_mut().flatten() {
            page.mark_saved();
        }
        if let Some(master) = self.master.as_mut() {
            master.mark_saved();
        }
        if let Some(pdf) = self.pdf_document.as_mut() {
            pdf.selected_pages_saved = pdf.selected_pages.clone();
            pdf.page_names_saved = pdf.page_names.clone();
            pdf.global_clears_saved = pdf.global_clears.clone();
            for page in pdf.edited_pages.values_mut() {
                page.canvas.mark_saved();
            }
        }
    }

    /// Number of pages in this document — always at least one (a plain document
    /// has an empty `pages` list and one page, its `canvas`).
    pub fn page_count(&self) -> usize {
        self.flow_text
            .as_ref()
            .map_or_else(|| self.pages.len().max(1), |text| text.page_count())
    }

    /// A blank page the same size / DPI / print-setup as the active page.
    fn blank_like_active(&self) -> Canvas {
        let mut c = Canvas::new(self.canvas.width, self.canvas.height);
        c.metadata.resolution_ppi = self.canvas.metadata.resolution_ppi;
        c.metadata.page_bleed_px = self.canvas.metadata.page_bleed_px;
        c.metadata.page_margin_px = self.canvas.metadata.page_margin_px;
        c
    }

    /// Append a blank page (same size as the active one) and make it active.
    /// Returns the new page's index. The first call materialises the current
    /// single page into the list, so a plain document becomes a two-page one.
    pub fn add_blank_page(&mut self) -> usize {
        if self.editing_master {
            self.exit_master_edit();
        }
        if self.pages.is_empty() {
            // Slot for the page currently checked out into `canvas`.
            self.pages.push(None);
            self.active_artboard = 0;
        }
        let blank = self.blank_like_active();
        self.pages.push(Some(blank));
        let new_index = self.pages.len() - 1;
        self.switch_page(new_index);
        new_index
    }

    /// Switch the active page to `index` by swapping the checked-out canvas with
    /// the stored one. No-op when already active, out of range, or single-page.
    pub fn switch_page(&mut self, index: usize) {
        if self.editing_master {
            self.exit_master_edit();
        }
        if self.pages.is_empty() || index >= self.pages.len() || index == self.active_artboard {
            return;
        }
        let Some(target) = self.pages[index].take() else {
            return; // the target slot is unexpectedly empty; leave state untouched
        };
        let old = self.active_artboard;
        let current = std::mem::replace(&mut self.canvas, target);
        self.pages[old] = Some(current);
        self.active_artboard = index;
    }

    /// Delete page `index`, keeping at least one page. Deleting the last remaining
    /// extra page collapses the document back to a plain single-page one. When the
    /// active page is removed, a neighbour becomes active; the caller must resync
    /// the view because the checked-out canvas may change. No-op on a single-page
    /// document or an out-of-range index.
    pub fn remove_page(&mut self, index: usize) {
        if self.editing_master {
            self.exit_master_edit();
        }
        if self.pages.len() <= 1 || index >= self.pages.len() {
            return;
        }
        // Park the checked-out active canvas back into its slot so `pages` is a
        // whole list (every slot `Some`) before the structural edit.
        let parked = std::mem::replace(&mut self.canvas, Canvas::new(1, 1));
        self.pages[self.active_artboard] = Some(parked);
        self.pages.remove(index);

        // Down to one page → collapse to a plain document (empty `pages`).
        if self.pages.len() == 1 {
            self.canvas = self.pages.remove(0).unwrap_or_else(|| Canvas::new(1, 1));
            self.active_artboard = 0;
            return;
        }

        // Track where the active page landed, then re-check it out.
        let mut new_active = self.active_artboard;
        if index < new_active {
            new_active -= 1;
        } else if index == new_active {
            new_active = index.min(self.pages.len() - 1);
        }
        self.active_artboard = new_active;
        if let Some(canvas) = self.pages[new_active].take() {
            self.canvas = canvas;
        }
    }

    /// Reorder pages: move the page at `from` to position `to` (tab drag / the
    /// context-menu "move left / right"). The active page keeps its content — only
    /// tab order and the active index change, so no view resync is needed. No-op
    /// when out of range or `from == to`.
    pub fn move_page(&mut self, from: usize, to: usize) {
        if self.editing_master {
            self.exit_master_edit();
        }
        let n = self.pages.len();
        if n < 2 || from >= n || to >= n || from == to {
            return;
        }
        let parked = std::mem::replace(&mut self.canvas, Canvas::new(1, 1));
        self.pages[self.active_artboard] = Some(parked);
        let page = self.pages.remove(from);
        self.pages.insert(to, page);

        // Follow the active page through the remove+insert shift.
        let mut active = self.active_artboard;
        if active == from {
            active = to;
        } else {
            if from < active {
                active -= 1;
            }
            if to <= active {
                active += 1;
            }
        }
        self.active_artboard = active.min(self.pages.len() - 1);
        if let Some(canvas) = self.pages[self.active_artboard].take() {
            self.canvas = canvas;
        }
    }

    /// Set (or clear, with `None`) the custom tab name of page `index`. The name
    /// lives on that page's canvas metadata, so it survives reorder and save.
    pub fn set_page_name(&mut self, index: usize, name: Option<String>) {
        let name = name.filter(|s| !s.trim().is_empty());
        if self.pages.is_empty() {
            if index == 0 {
                self.canvas.metadata.page_name = name;
            }
            return;
        }
        if index == self.active_artboard && !self.editing_master {
            self.canvas.metadata.page_name = name;
        } else if let Some(Some(canvas)) = self.pages.get_mut(index) {
            canvas.metadata.page_name = name;
        }
    }

    /// Display name for page `index`: the custom name if set, else "Trang N".
    pub fn page_display_name(&self, index: usize) -> String {
        let custom = if self.pages.is_empty() {
            (index == 0)
                .then(|| self.canvas.metadata.page_name.clone())
                .flatten()
        } else if index == self.active_artboard && !self.editing_master {
            self.canvas.metadata.page_name.clone()
        } else {
            self.pages
                .get(index)
                .and_then(|slot| slot.as_ref())
                .and_then(|canvas| canvas.metadata.page_name.clone())
        };
        custom
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| format!("Trang {}", index + 1))
    }

    /// Tab labels for every page, in order (drives the page-tab bar).
    pub fn page_names(&self) -> Vec<String> {
        (0..self.page_count())
            .map(|i| self.page_display_name(i))
            .collect()
    }

    /// Every page's canvas in tab order — the checked-out active canvas plus the
    /// stored inactive ones. One entry for a plain single-page document. Used by
    /// multi-page save / export so a page is never missed.
    pub fn all_page_canvases(&self) -> Vec<&Canvas> {
        if self.pages.is_empty() {
            return vec![&self.canvas];
        }
        (0..self.pages.len())
            .map(|i| {
                // While editing the master, the active slot holds the real page
                // (parked) and `canvas` holds the master — so the active slot maps
                // to `canvas` only when NOT editing the master.
                if i == self.active_artboard && !self.editing_master {
                    &self.canvas
                } else {
                    self.pages[i].as_ref().unwrap_or(&self.canvas)
                }
            })
            .collect()
    }

    // ── Master (shared background) page ────────────────────────────────────────

    /// The document has a master page — whether stored or currently checked out
    /// into `canvas` for editing.
    pub fn has_master(&self) -> bool {
        self.master.is_some() || self.editing_master
    }

    /// The master canvas, wherever it currently lives: checked out into `canvas`
    /// while editing, otherwise the stored `master`. `None` when there is none.
    pub fn master_canvas(&self) -> Option<&Canvas> {
        if self.editing_master {
            Some(&self.canvas)
        } else {
            self.master.as_deref()
        }
    }

    /// The master to composite BENEATH the active page, or `None` when there is no
    /// master, the active page opts out, or the master itself is being edited
    /// (it renders alone then).
    pub fn active_master_backdrop(&self) -> Option<&Canvas> {
        if self.editing_master {
            return None;
        }
        let master = self.master.as_deref()?;
        self.canvas.metadata.use_master.then_some(master)
    }

    /// The master to composite beneath page `index` (export / thumbnails), honoring
    /// that page's opt-out. `None` while editing the master or when absent.
    pub fn master_backdrop_for(&self, index: usize) -> Option<&Canvas> {
        if self.editing_master {
            return None;
        }
        let master = self.master.as_deref()?;
        let uses = if self.pages.is_empty() {
            index == 0 && self.canvas.metadata.use_master
        } else if index == self.active_artboard {
            self.canvas.metadata.use_master
        } else {
            self.pages
                .get(index)
                .and_then(|s| s.as_ref())
                .is_some_and(|c| c.metadata.use_master)
        };
        uses.then_some(master)
    }

    /// Create a blank master page (matching the active page's size / DPI / print
    /// setup) if the document has none. No-op if one already exists or is being
    /// edited. Materialises the single-page document into the multi-page list so
    /// the master persists through the artboard-document save path.
    pub fn ensure_master(&mut self) {
        if self.has_master() {
            return;
        }
        if self.pages.is_empty() {
            self.pages.push(None);
            self.active_artboard = 0;
        }
        let mut master = self.blank_like_active();
        master.metadata.page_name = Some("Trang nền".to_string());
        self.master = Some(Box::new(master));
    }

    /// Remove the master page. Exits master-edit first so the real page is restored
    /// into `canvas`. No-op when there is no master.
    pub fn remove_master(&mut self) {
        if self.editing_master {
            self.exit_master_edit();
        }
        self.master = None;
    }

    /// Check the master out into `canvas` for editing, parking the active page in
    /// its slot. Requires a master and a multi-page list. No-op if already editing.
    pub fn enter_master_edit(&mut self) {
        if self.editing_master || self.pages.is_empty() {
            return;
        }
        let Some(master) = self.master.take() else {
            return;
        };
        let active_page = std::mem::replace(&mut self.canvas, *master);
        self.pages[self.active_artboard] = Some(active_page);
        self.editing_master = true;
    }

    /// Park the master back and restore the active page into `canvas`. No-op when
    /// not editing the master.
    pub fn exit_master_edit(&mut self) {
        if !self.editing_master {
            return;
        }
        let master = std::mem::replace(&mut self.canvas, Canvas::new(1, 1));
        self.master = Some(Box::new(master));
        self.editing_master = false;
        if let Some(canvas) = self
            .pages
            .get_mut(self.active_artboard)
            .and_then(|s| s.take())
        {
            self.canvas = canvas;
        }
    }

    /// A throwaway canvas for rendering / exporting page `index` with its master
    /// composited beneath, or `None` when the page has no master backdrop (callers
    /// use the page canvas directly). The result shares tile data with the
    /// originals via the layer clones, so it is cheap; it must not be edited or
    /// saved. CMYK pages are skipped for now (the merged canvas is RGB) — the
    /// master simply doesn't render under a CMYK page.
    pub fn page_render_canvas(&self, index: usize) -> Option<Canvas> {
        let master = self.master_backdrop_for(index)?;
        let page = *self.all_page_canvases().get(index)?;
        if page.is_cmyk() || master.is_cmyk() {
            return None;
        }
        // This canvas is consumed only by export. Avoid allocating and clearing
        // a page-sized flat buffer/selection on the UI thread.
        let mut merged = page.export_snapshot();
        merged.layer_stack = page.layer_stack.with_backdrop(&master.layer_stack);
        merged.metadata = page.metadata.clone();
        merged.color_space = page.color_space;
        Some(merged)
    }

    /// Set (or clear) whether page `index` shows the master beneath it.
    pub fn set_page_use_master(&mut self, index: usize, on: bool) {
        if self.pages.is_empty() {
            if index == 0 {
                self.canvas.metadata.use_master = on;
            }
            return;
        }
        if index == self.active_artboard && !self.editing_master {
            self.canvas.metadata.use_master = on;
        } else if let Some(Some(canvas)) = self.pages.get_mut(index) {
            canvas.metadata.use_master = on;
        }
    }

    /// Latch the sticky "this PDF page has been edited" flag from the active
    /// canvas's dirty state. Sticky and save-surviving, so it cannot simply be
    /// derived; call it wherever the active page's dirty state may have changed.
    pub fn reconcile_pdf_page_modified(&mut self) {
        let dirty = self.canvas.is_dirty();
        if let Some(pdf) = self.pdf_document.as_mut() {
            pdf.active_page_modified |= dirty;
        }
        self.rebuild_pdf_global_overlay();
    }

    pub fn rebuild_pdf_global_overlay(&mut self) {
        let (width, height) = (self.canvas.width, self.canvas.height);
        if let Some(pdf) = self.pdf_document.as_mut() {
            pdf.rebuild_global_overlay(width, height);
        }
    }

    pub fn active_pdf_global_overlay(&self) -> Option<&crate::core::layer::LayerStack> {
        self.pdf_document
            .as_ref()?
            .global_overlay_cache
            .as_ref()
            .filter(|cache| cache.width == self.canvas.width && cache.height == self.canvas.height)
            .map(|cache| &cache.stack)
    }

    /// Return the pixels added above an untouched imported PDF base layer.
    /// `None` means the edit can affect/remove the vector background and the
    /// page must be rasterized. The checks are deliberately conservative.
    pub fn pdf_safe_overlay_rgba(&self) -> Option<Vec<u8>> {
        let page = self.pdf_page.as_ref()?;
        page.safe_overlay_rgba(&self.canvas)
    }
}

pub fn file_modified_at(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

pub fn disambiguated_tab_titles(docs: &[Document]) -> Vec<String> {
    let base_titles: Vec<String> = docs.iter().map(|d| d.tab_title().to_string()).collect();
    let mut counts = std::collections::HashMap::<String, usize>::new();
    for title in &base_titles {
        *counts.entry(title.clone()).or_default() += 1;
    }

    let mut seen = std::collections::HashMap::<String, usize>::new();
    base_titles
        .into_iter()
        .map(|title| {
            if counts.get(&title).copied().unwrap_or(0) <= 1 {
                return title;
            }
            let n = seen.entry(title.clone()).or_default();
            *n += 1;
            if *n == 1 {
                title
            } else {
                format!("{title} ({n})")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        disambiguated_tab_titles, Document, DocumentId, DocumentKind, PdfDocumentState,
        PdfGlobalClear, PdfPageRef,
    };
    use crate::core::canvas::Canvas;
    use std::path::PathBuf;

    #[test]
    fn flow_text_document_is_lightweight_and_clean_at_creation() {
        let doc = Document::new_flow_text(DocumentId(7));
        assert_eq!(doc.kind, DocumentKind::FlowText);
        assert!(doc.is_flow_text());
        assert_eq!((doc.canvas.width, doc.canvas.height), (1, 1));
        assert!(
            doc.pages.is_empty(),
            "text pages must not materialise canvases"
        );
        assert_eq!(doc.page_count(), 1);
        assert!(!doc.is_modified());
    }

    #[test]
    fn flow_text_revision_participates_in_document_dirty_tracking() {
        let mut doc = Document::new_flow_text(DocumentId(8));
        let text = doc.flow_text.as_mut().expect("flow text state");
        text.replace_document(crate::core::text_document::TextDocument::from_plain_text(
            "Hợp đồng thuê nhà",
        ));
        assert!(doc.is_modified());

        doc.mark_saved();
        assert!(!doc.is_modified());

        doc.flow_text
            .as_mut()
            .expect("flow text state")
            .document_mut()
            .page
            .margins
            .left_mm = 35.0;
        assert!(doc.is_modified());
    }

    #[test]
    fn derived_flow_text_paging_does_not_dirty_content_and_clamps_navigation() {
        let mut doc = Document::new_flow_text(DocumentId(9));
        let text = doc.flow_text.as_mut().expect("flow text state");
        text.set_layout_page_count(12);
        text.set_active_page(11);
        assert_eq!(text.active_page(), 11);
        assert_eq!(doc.page_count(), 12);
        assert!(!doc.is_modified());

        let text = doc.flow_text.as_mut().expect("flow text state");
        text.set_layout_page_count(3);
        assert_eq!(text.active_page(), 2);
        assert_eq!(doc.page_count(), 3);
        assert!(!doc.is_modified());
    }

    #[test]
    fn duplicate_tab_titles_get_suffixes() {
        let docs = vec![
            Document::from_canvas(
                DocumentId(1),
                crate::core::canvas::Canvas::new(1, 1),
                Some(PathBuf::from("a/photo.png")),
            ),
            Document::from_canvas(
                DocumentId(2),
                crate::core::canvas::Canvas::new(1, 1),
                Some(PathBuf::from("a/photo.png")),
            ),
            Document::from_canvas(
                DocumentId(3),
                crate::core::canvas::Canvas::new(1, 1),
                Some(PathBuf::from("b/photo.png")),
            ),
        ];

        assert_eq!(
            disambiguated_tab_titles(&docs),
            vec!["photo.png", "photo.png (2)", "photo.png (3)"]
        );
    }

    #[test]
    fn pdf_page_metadata_renames_reorders_and_removes_without_touching_pixels() {
        let mut pdf = PdfDocumentState {
            source: PathBuf::from("book.pdf"),
            embedded_source: None,
            page_count: 8,
            selected_pages: vec![1, 3, 6],
            selected_pages_saved: vec![1, 3, 6],
            page_names: std::collections::BTreeMap::new(),
            page_names_saved: std::collections::BTreeMap::new(),
            requested_dpi: 144.0,
            active_page: 1,
            active_page_modified: false,
            edited_pages: std::collections::HashMap::new(),
            global_clears: Vec::new(),
            global_clears_saved: Vec::new(),
            global_clears_redo: Vec::new(),
            global_overlay_cache: None,
        };

        assert_eq!(pdf.display_names(), vec!["Trang 1", "Trang 2", "Trang 3"]);
        assert!(!pdf.cached_pages_dirty());
        pdf.set_page_name(1, Some("Minh họa".to_string()));
        pdf.move_page(1, 0);
        assert_eq!(pdf.selected_pages, vec![3, 1, 6]);
        assert_eq!(pdf.display_names(), vec!["Minh họa", "Trang 2", "Trang 3"]);
        assert!(pdf.cached_pages_dirty());

        assert_eq!(pdf.remove_page(2), Some(6));
        assert_eq!(pdf.selected_pages, vec![3, 1]);
        assert_eq!(pdf.allocate_inserted_page_id(), Some(8));
        assert_eq!(pdf.remove_page(1), Some(1));
        assert_eq!(
            pdf.remove_page(0),
            None,
            "the final visible page is retained"
        );
    }

    #[test]
    fn pdf_overlay_is_allowed_only_while_source_layer_is_untouched() {
        let canvas = crate::core::canvas::Canvas::from_rgba(vec![255; 4 * 4 * 4], 4, 4);
        let mut document = Document::from_canvas(DocumentId(1), canvas, None);
        let mut page = PdfPageRef {
            group_id: 1,
            source: PathBuf::from("source.pdf"),
            index: 0,
            count: 1,
            requested_dpi: 72.0,
            loaded: true,
            original_width: 0,
            original_height: 0,
            original_dpi: 72.0,
            original_layer_id: 0,
            original_tiles_fingerprint: 0,
        };
        page.record_canvas_baseline(&document.canvas);
        document.pdf_page = Some(page);

        let overlay_index = document.canvas.add_layer();
        document.canvas.layer_stack.layers[overlay_index]
            .tiles
            .write_region(1, 1, 1, 1, &[255, 0, 0, 255]);
        let overlay = document.pdf_safe_overlay_rgba().expect("safe overlay");
        assert!(overlay
            .chunks_exact(4)
            .any(|pixel| pixel == [255, 0, 0, 255]));

        document.canvas.layer_stack.layers[0]
            .tiles
            .write_region(0, 0, 1, 1, &[0, 0, 0, 255]);
        assert!(document.pdf_safe_overlay_rgba().is_none());
    }

    /// Dirty is per-canvas, so two open documents cannot leak dirt into each
    /// other. The old design kept one live bool on `App` plus a per-`Document`
    /// copy synced on tab switch, which is exactly where they drifted apart.
    #[test]
    fn multi_document_dirty_state_is_independent() {
        let mut a = Document::new(DocumentId(1), 4, 4);
        let mut b = Document::new(DocumentId(2), 4, 4);
        a.mark_saved();
        b.mark_saved();
        assert!(!a.is_modified());
        assert!(!b.is_modified());

        // A real edit on `a` only.
        a.canvas.deselect();
        assert!(a.is_modified(), "the edited document is dirty");
        assert!(!b.is_modified(), "its neighbour must stay clean");

        // Saving `a` must not disturb `b`, and vice versa.
        b.canvas.deselect();
        a.mark_saved();
        assert!(!a.is_modified());
        assert!(
            b.is_modified(),
            "saving one document must not clean another"
        );
    }

    #[test]
    fn saving_a_document_then_undoing_reports_dirty_again() {
        let mut doc = Document::new(DocumentId(1), 4, 4);
        doc.canvas.deselect();
        doc.mark_saved();
        assert!(!doc.is_modified());

        doc.canvas.undo();
        assert!(
            doc.is_modified(),
            "undoing away from the saved state must report unsaved changes"
        );
        doc.canvas.redo();
        assert!(
            !doc.is_modified(),
            "redoing back onto the saved state must report clean"
        );
    }

    #[test]
    fn new_document_has_one_implicit_artboard() {
        use crate::core::geometry::Rect;
        use crate::core::page::PageId;
        let doc = Document::new(DocumentId(1), 800, 600);
        assert!(!doc.has_explicit_artboards());
        assert_eq!(doc.artboard_count(), 1);
        let boards = doc.effective_artboards();
        assert_eq!(boards.len(), 1);
        assert_eq!(boards[0].id, PageId::IMPLICIT);
        assert_eq!(boards[0].rect(), Rect::new(0.0, 0.0, 800.0, 600.0));
        assert_eq!((boards[0].bleed, boards[0].margin), (0.0, 0.0));
        assert!(boards[0].background.is_none());
    }

    #[test]
    fn implicit_artboard_follows_the_canvas_size() {
        use crate::core::geometry::Rect;
        // Derived, not stored: two differently sized documents each report their
        // own canvas as the single implicit artboard — no copy to desync.
        let small = Document::new(DocumentId(1), 100, 100);
        let big = Document::new(DocumentId(2), 1920, 1080);
        assert_eq!(
            small.effective_artboards()[0].rect(),
            Rect::new(0.0, 0.0, 100.0, 100.0)
        );
        assert_eq!(
            big.effective_artboards()[0].rect(),
            Rect::new(0.0, 0.0, 1920.0, 1080.0)
        );
    }

    #[test]
    fn pdf_global_clear_captures_and_scales_a_hard_rectangle() {
        let mut canvas = Canvas::new(100, 80);
        canvas.select_rect(0, 0, 20, 16);
        let clear = PdfGlobalClear::from_selection(
            &mut canvas.selection,
            canvas.width,
            canvas.height,
            [250, 249, 248, 120],
        )
        .unwrap();
        assert_eq!(clear.color, [250, 249, 248, 255]);
        assert_eq!(clear.pixel_rect(200, 160), (0, 0, 40, 32));
    }

    #[test]
    fn explicit_artboards_override_the_implicit_page() {
        use crate::core::geometry::Point;
        use crate::core::page::{Page, PageId};
        let mut doc = Document::new(DocumentId(1), 400, 400);
        doc.canvas.metadata.artboards = vec![
            Page::implicit(400, 400),
            Page {
                id: PageId(1),
                origin: Point::new(420.0, 0.0),
                size: (400.0, 400.0),
                bleed: 0.0,
                margin: 0.0,
                background: None,
            },
        ];
        assert!(doc.has_explicit_artboards());
        assert_eq!(doc.artboard_count(), 2);
        let boards = doc.effective_artboards();
        assert_eq!(boards.len(), 2);
        assert_eq!(boards[1].id, PageId(1));
        assert_eq!(boards[1].origin, Point::new(420.0, 0.0));
    }

    #[test]
    fn page_setup_command_sets_bleed_and_margin_then_undoes() {
        // The undoable page-setup path: bleed/margin land on the implicit page,
        // survive undo/redo, and route through the canvas history gate.
        let mut doc = Document::new(DocumentId(1), 100, 100);
        assert_eq!(doc.effective_artboards()[0].bleed, 0.0);
        doc.mark_saved();

        doc.canvas
            .execute(
                Box::new(crate::core::command::PageSetupCommand::new(
                    "Page setup",
                    8.0,
                    5.0,
                    Vec::new(),
                )),
                crate::core::gateway::ChangeKind::LayerStructure,
            )
            .expect("page setup applies");
        assert_eq!(doc.canvas.metadata.page_bleed_px, 8.0);
        assert_eq!(doc.effective_artboards()[0].bleed, 8.0);
        assert_eq!(doc.effective_artboards()[0].margin, 5.0);
        assert!(
            doc.is_modified(),
            "a page-setup edit marks the document dirty"
        );

        doc.canvas.undo();
        assert_eq!(doc.canvas.metadata.page_bleed_px, 0.0);
        assert_eq!(doc.effective_artboards()[0].bleed, 0.0);

        doc.canvas.redo();
        assert_eq!(doc.canvas.metadata.page_bleed_px, 8.0);
        assert_eq!(doc.effective_artboards()[0].bleed, 8.0);
    }

    #[test]
    fn add_and_switch_pages_swap_the_active_canvas() {
        // Each page is an independent canvas; switching swaps the checked-out one.
        let mut doc = Document::new(DocumentId(1), 100, 80);
        assert_eq!(doc.page_count(), 1);
        doc.canvas.metadata.title = "P0".to_string();

        let idx1 = doc.add_blank_page();
        assert_eq!(idx1, 1);
        assert_eq!(doc.page_count(), 2);
        assert_eq!(doc.active_artboard, 1);
        doc.canvas.metadata.title = "P1".to_string();

        doc.switch_page(0);
        assert_eq!(doc.active_artboard, 0);
        assert_eq!(doc.canvas.metadata.title, "P0", "page 0 checked back in");
        assert!(doc.pages[0].is_none(), "active slot is empty (checked out)");
        assert!(doc.pages[1].is_some(), "inactive page 1 is stored");

        doc.switch_page(1);
        assert_eq!(doc.canvas.metadata.title, "P1");
        assert!(doc.pages[1].is_none());
        assert!(doc.pages[0].is_some());

        // Out-of-range / same-page switches are no-ops.
        doc.switch_page(9);
        assert_eq!(doc.active_artboard, 1);
        doc.switch_page(1);
        assert_eq!(doc.active_artboard, 1);
    }

    #[test]
    fn remove_page_keeps_at_least_one_and_tracks_active() {
        let mut doc = Document::new(DocumentId(1), 100, 80);
        doc.set_page_name(0, Some("A".into()));
        doc.add_blank_page();
        doc.set_page_name(1, Some("B".into()));
        doc.add_blank_page();
        doc.set_page_name(2, Some("C".into()));
        assert_eq!(doc.page_names(), vec!["A", "B", "C"]);

        // Remove a page before the active one → active index shifts down but the
        // same page (C) stays checked out.
        doc.switch_page(2);
        doc.remove_page(0);
        assert_eq!(doc.page_names(), vec!["B", "C"]);
        assert_eq!(doc.active_artboard, 1);
        assert_eq!(doc.page_display_name(doc.active_artboard), "C");

        // Remove the active page → collapse back to a plain single-page document
        // showing the surviving neighbour.
        doc.remove_page(1);
        assert_eq!(doc.page_count(), 1);
        assert!(doc.pages.is_empty(), "single page → plain document");
        assert_eq!(doc.page_display_name(0), "B");

        // The sole page cannot be removed.
        doc.remove_page(0);
        assert_eq!(doc.page_count(), 1);
    }

    #[test]
    fn move_page_reorders_and_active_follows() {
        let mut doc = Document::new(DocumentId(2), 50, 50);
        doc.set_page_name(0, Some("A".into()));
        doc.add_blank_page();
        doc.set_page_name(1, Some("B".into()));
        doc.add_blank_page();
        doc.set_page_name(2, Some("C".into()));

        // Active is C (index 2); move it to the front.
        doc.move_page(2, 0);
        assert_eq!(doc.page_names(), vec!["C", "A", "B"]);
        assert_eq!(doc.active_artboard, 0);
        assert_eq!(doc.page_display_name(doc.active_artboard), "C");

        // Move a non-active page across the active one; active index follows.
        doc.switch_page(1); // active = A
        doc.move_page(2, 0); // B to front → [B, C, A]
        assert_eq!(doc.page_names(), vec!["B", "C", "A"]);
        assert_eq!(doc.active_artboard, 2);
        assert_eq!(doc.page_display_name(doc.active_artboard), "A");
    }

    #[test]
    fn all_page_canvases_returns_every_page_in_order() {
        let mut doc = Document::new(DocumentId(4), 10, 10);
        assert_eq!(doc.all_page_canvases().len(), 1, "single page");
        doc.set_page_name(0, Some("A".into()));
        doc.add_blank_page();
        doc.set_page_name(1, Some("B".into()));
        doc.add_blank_page();
        doc.set_page_name(2, Some("C".into()));
        doc.switch_page(1); // active = B; A and C are stored
        let names: Vec<String> = doc
            .all_page_canvases()
            .iter()
            .map(|c| c.metadata.page_name.clone().unwrap_or_default())
            .collect();
        assert_eq!(
            names,
            vec!["A", "B", "C"],
            "active canvas sits at its index"
        );
    }

    #[test]
    fn page_names_default_to_positional_and_ride_the_canvas() {
        let mut doc = Document::new(DocumentId(3), 20, 20);
        assert_eq!(doc.page_names(), vec!["Trang 1"]);
        doc.set_page_name(0, Some("Bìa".into()));
        assert_eq!(doc.page_names(), vec!["Bìa"]);
        // Blank name reverts to the positional label.
        doc.set_page_name(0, Some("   ".into()));
        assert_eq!(doc.page_names(), vec!["Trang 1"]);

        doc.add_blank_page();
        doc.set_page_name(1, Some("Ruột".into()));
        assert_eq!(doc.page_names(), vec!["Trang 1", "Ruột"]);
        // The custom name travels with its canvas through a reorder.
        doc.move_page(1, 0);
        assert_eq!(doc.page_names(), vec!["Ruột", "Trang 2"]);
    }

    #[test]
    fn ensure_master_creates_one_and_materialises_pages() {
        let mut doc = Document::new(DocumentId(1), 120, 90);
        assert!(!doc.has_master());
        doc.ensure_master();
        assert!(doc.has_master());
        assert!(doc.master.is_some());
        assert!(!doc.editing_master);
        // Master matches the page size and became a proper multi-page container.
        let m = doc.master_canvas().unwrap();
        assert_eq!((m.width, m.height), (120, 90));
        assert_eq!(
            doc.pages.len(),
            1,
            "single-page doc materialised for saving"
        );
        // Idempotent.
        doc.ensure_master();
        assert_eq!(doc.pages.len(), 1);
    }

    #[test]
    fn edit_master_checks_it_out_and_restores_the_active_page() {
        let mut doc = Document::new(DocumentId(1), 60, 60);
        doc.canvas.metadata.title = "P0".into();
        doc.add_blank_page();
        doc.canvas.metadata.title = "P1".into();
        doc.ensure_master();

        doc.enter_master_edit();
        assert!(doc.editing_master);
        assert_eq!(
            doc.master_canvas().unwrap().metadata.page_name.as_deref(),
            Some("Trang nền")
        );
        assert!(doc.master.is_none(), "master is checked out into canvas");
        assert!(
            doc.pages[doc.active_artboard].is_some(),
            "the real active page is parked while editing the master"
        );
        // Page labels still report the real pages, not the master.
        assert_eq!(doc.all_page_canvases().len(), 2);

        doc.exit_master_edit();
        assert!(!doc.editing_master);
        assert!(doc.master.is_some());
        assert_eq!(doc.canvas.metadata.title, "P1", "active page restored");
    }

    #[test]
    fn active_master_backdrop_respects_per_page_opt_out() {
        let mut doc = Document::new(DocumentId(1), 40, 40);
        doc.add_blank_page(); // page 1 active
        doc.ensure_master();
        // Default: the active page uses the master.
        assert!(doc.active_master_backdrop().is_some());
        // Opt the active page out.
        doc.set_page_use_master(doc.active_artboard, false);
        assert!(doc.active_master_backdrop().is_none());
        // A page without a master never has a backdrop.
        doc.remove_master();
        assert!(!doc.has_master());
        assert!(doc.active_master_backdrop().is_none());
    }

    #[test]
    fn switching_pages_exits_master_edit_safely() {
        let mut doc = Document::new(DocumentId(1), 30, 30);
        doc.canvas.metadata.title = "P0".into();
        doc.add_blank_page();
        doc.canvas.metadata.title = "P1".into();
        doc.ensure_master();
        doc.enter_master_edit();
        assert!(doc.editing_master);
        // A page switch while editing the master must first restore the page.
        doc.switch_page(0);
        assert!(!doc.editing_master);
        assert_eq!(doc.active_artboard, 0);
        assert_eq!(doc.canvas.metadata.title, "P0");
        assert!(doc.master.is_some());
    }

    #[test]
    fn master_dirty_marks_the_document_modified() {
        let mut doc = Document::new(DocumentId(1), 20, 20);
        doc.add_blank_page();
        doc.ensure_master();
        doc.mark_saved();
        assert!(!doc.is_modified());
        // Dirty the master directly (as an edit would).
        doc.master.as_mut().unwrap().deselect();
        assert!(doc.is_modified(), "a master edit marks the document dirty");
        doc.mark_saved();
        assert!(!doc.is_modified());
    }
}

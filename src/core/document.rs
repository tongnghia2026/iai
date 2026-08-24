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

pub struct PdfDocumentState {
    pub source: PathBuf,
    /// Materialized embedded PDF used by self-contained `.iai` projects.
    /// Runtime-only; project files keep serializing `source` as the original link.
    pub embedded_source: Option<PathBuf>,
    pub page_count: usize,
    pub selected_pages: Vec<usize>,
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
}

impl PdfDocumentState {
    pub fn effective_source(&self) -> &Path {
        self.embedded_source.as_deref().unwrap_or(&self.source)
    }

    /// Any cached (inactive) page holding edits not yet written to the project
    /// file. The active page lives on [`Document::canvas`], so the whole-document
    /// answer is [`Document::is_modified`].
    pub fn cached_pages_dirty(&self) -> bool {
        self.edited_pages
            .values()
            .any(|page| page.canvas.is_dirty())
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
}

impl Document {
    pub fn new(id: DocumentId, width: u32, height: u32) -> Self {
        Self {
            id,
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
        }
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
        self.canvas.is_dirty()
            || self.pages.iter().flatten().any(|c| c.is_dirty())
            || self
                .pdf_document
                .as_ref()
                .is_some_and(|pdf| pdf.cached_pages_dirty())
    }

    /// Unsaved changes on the page the user is looking at.
    pub fn active_is_modified(&self) -> bool {
        self.canvas.is_dirty()
    }

    /// Anchor every canvas in this document to "clean". Called after a
    /// successful write; keeps the page canvases and their sticky
    /// differs-from-original state intact.
    pub fn mark_saved(&mut self) {
        self.canvas.mark_saved();
        for page in self.pages.iter_mut().flatten() {
            page.mark_saved();
        }
        if let Some(pdf) = self.pdf_document.as_mut() {
            for page in pdf.edited_pages.values_mut() {
                page.canvas.mark_saved();
            }
        }
    }

    /// Number of pages in this document — always at least one (a plain document
    /// has an empty `pages` list and one page, its `canvas`).
    pub fn page_count(&self) -> usize {
        self.pages.len().max(1)
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
        if index == self.active_artboard {
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
        } else if index == self.active_artboard {
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

    /// Latch the sticky "this PDF page has been edited" flag from the active
    /// canvas's dirty state. Sticky and save-surviving, so it cannot simply be
    /// derived; call it wherever the active page's dirty state may have changed.
    pub fn reconcile_pdf_page_modified(&mut self) {
        let dirty = self.canvas.is_dirty();
        if let Some(pdf) = self.pdf_document.as_mut() {
            pdf.active_page_modified |= dirty;
        }
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
    use super::{disambiguated_tab_titles, Document, DocumentId, PdfPageRef};
    use std::path::PathBuf;

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
    fn add_artboard_grows_canvas_and_undoes_as_one_step() {
        // Mirrors App::add_artboard: canvas.resize + the artboard-list change
        // nested in ONE undo group, so a single undo reverts BOTH (size + list).
        let mut doc = Document::new(DocumentId(1), 100, 80);
        assert_eq!(doc.effective_artboards().len(), 1);
        let (boards, w2, h2) = crate::core::page::append_artboard_in_row(&[], 100, 80, 0.0, 0.0);

        doc.canvas.begin_undo_group("Add artboard");
        assert!(doc.canvas.resize(w2, h2));
        doc.canvas
            .execute(
                Box::new(crate::core::command::PageSetupCommand::new(
                    "Add artboard",
                    0.0,
                    0.0,
                    boards,
                )),
                crate::core::gateway::ChangeKind::LayerStructure,
            )
            .expect("add artboard applies");
        doc.canvas.end_undo_group();
        assert_eq!(doc.canvas.width, w2, "canvas grew to hold both artboards");
        assert_eq!(doc.canvas.height, h2);
        assert_eq!(doc.effective_artboards().len(), 2);

        doc.canvas.undo();
        assert_eq!(doc.canvas.width, 100, "one undo reverts size AND the list");
        assert!(doc.canvas.metadata.artboards.is_empty());
        assert_eq!(doc.effective_artboards().len(), 1);

        doc.canvas.redo();
        assert_eq!(doc.canvas.width, w2);
        assert_eq!(doc.effective_artboards().len(), 2);
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
}

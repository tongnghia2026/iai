//! PDF: the pages of a document, or of every open document, in one file.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use iai_canvas::canvas::Canvas;
use iai_canvas::document::Document;
use iai_canvas::print::{PrintMarks, build_snapshots_pdf, prepare_pdf_pixels};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::not_enabled;
use crate::{
    Apply, CmdError, Command, CommandMeta, Effect, ErrorCode, Job, Plan, Session, Validate,
};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PdfParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// Where to write the PDF.
    #[schemars(length(min = 1, max = 4096))]
    pub path: String,
    /// The pages to write, 0 being the first; every page when omitted.
    #[schemars(length(min = 1, max = 10000))]
    pub pages: Option<Vec<usize>>,
    /// Every open document instead, tab by tab, all their pages (text
    /// documents are left out).
    #[serde(default)]
    pub all_open_documents: bool,
    /// Lower the resolution of pages above it (smaller file); never raised.
    #[schemars(range(min = 36, max = 2400))]
    pub dpi: Option<u32>,
    /// Corner crop marks for the print shop.
    #[serde(default)]
    pub crop_marks: bool,
    #[serde(default)]
    pub registration_marks: bool,
    /// Bleed: how far the trim sits inside the picture's edge, in mm.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 20.0))]
    pub bleed_mm: f32,
    /// Embed the sRGB colour profile.
    #[serde(default)]
    pub embed_icc: bool,
}

impl Validate for PdfParams {
    fn validate(&self) -> Result<(), CmdError> {
        if self.all_open_documents && (self.doc.is_some() || self.pages.is_some()) {
            return Err(CmdError::bad_param(
                "allOpenDocuments",
                "to be left out with `doc` or `pages`",
            ));
        }
        let ends_pdf = Path::new(&self.path)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("pdf"));
        if !ends_pdf {
            return Err(CmdError::bad_param("path", "a file ending in .pdf"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PdfWritten {
    pub path: String,
    pub pages: usize,
    pub bytes: u64,
    /// How the pages were made: `raster`, `text` or `hybrid` (an opened PDF
    /// whose untouched pages are copied as they were).
    pub kind: String,
}

static PDF: CommandMeta = CommandMeta {
    id: "export.pdf",
    label_vi: "Xuất PDF",
    label_en: "Export PDF",
    doc: "Write pages to one PDF: a document's (all, or `pages`), or every open document's \
          (`allOpenDocuments`, e.g. many scanned photos into one file). Each page keeps its \
          size on paper; shapes and text stay sharp. Optional print marks and lower dpi. \
          Writes a file; prints nothing.",
    effects: &[Effect::WriteFiles],
    featured: true,
};

pub struct ExportPdf;

/// What the worker writes, taken from the documents when the call is made.
enum Source {
    /// Page canvases; with the count of documents they come from when every
    /// open document is combined.
    Pages(Vec<Canvas>, Option<usize>),
    Text {
        document: Arc<iai_doc::text_document::TextDocument>,
        pages: Vec<usize>,
    },
    Hybrid(Box<HybridSource>),
}

struct HybridSource {
    source: PathBuf,
    clean_whole: bool,
    global_edits: Vec<iai_canvas::document::PdfGlobalEdit>,
    snapshots: Vec<(usize, Option<(Canvas, iai_canvas::document::PdfPageRef)>)>,
}

/// Page `i` of `doc`, ready to write: merged over its master page, if it has
/// one.
fn page_snapshot(doc: &Document, i: usize) -> Option<Canvas> {
    if let Some(merged) = doc.page_render_canvas(i) {
        return Some(merged);
    }
    doc.all_page_canvases().get(i).map(|c| c.export_snapshot())
}

fn chosen(pages: &Option<Vec<usize>>, count: usize) -> Result<Vec<usize>, CmdError> {
    match pages {
        None => Ok((0..count).collect()),
        Some(list) => {
            if let Some(bad) = list.iter().find(|&&p| p >= count) {
                return Err(CmdError::new(
                    ErrorCode::NotFound,
                    format!("no page {bad}: the document has {count} (the first is 0)"),
                )
                .vi(format!("Không có trang {}", bad.saturating_add(1))));
            }
            Ok(list.clone())
        }
    }
}

fn hybrid_source(doc: &Document, positions: &[usize]) -> Result<HybridSource, CmdError> {
    let pdf = doc
        .pdf_document
        .as_ref()
        .ok_or_else(|| CmdError::internal("not a PDF document"))?;
    let selected: Vec<usize> = positions
        .iter()
        .filter_map(|&p| pdf.selected_pages.get(p).copied())
        .collect();
    if selected.len() != positions.len() || selected.is_empty() {
        return Err(CmdError::bad_param("pages", "pages of this PDF"));
    }
    let clean_whole = !doc.is_modified()
        && selected.len() == pdf.page_count
        && selected.iter().copied().eq(0..pdf.page_count);
    let global_edits = pdf.global_edits.clone();
    let mut snapshots = Vec::with_capacity(selected.len());
    for &page in &selected {
        let mut edited = if page == pdf.active_page && pdf.active_page_modified {
            doc.pdf_page
                .as_ref()
                .map(|reference| (doc.canvas.export_snapshot(), reference.clone()))
        } else {
            pdf.edited_pages
                .get(&page)
                .map(|cached| (cached.canvas.export_snapshot(), cached.reference.clone()))
        };
        if let Some((canvas, _)) = edited.as_mut()
            && let Some(overlay) = iai_canvas::document::build_pdf_global_overlay_stack(
                &global_edits,
                page,
                canvas.width,
                canvas.height,
            )
        {
            canvas.layer_stack = canvas.layer_stack.with_overlay(&overlay);
        }
        snapshots.push((page, edited));
    }
    Ok(HybridSource {
        source: pdf.effective_source().to_path_buf(),
        clean_whole: clean_whole && global_edits.is_empty(),
        global_edits,
        snapshots,
    })
}

/// The bytes of a PDF made from an opened PDF: pages not edited are copied
/// as they are (still vector), edited ones are laid over or replaced. With
/// them, how many pages were each (vector, overlay, raster); none when the
/// original was copied whole.
fn hybrid_bytes(
    h: &HybridSource,
    target_dpi: u32,
) -> Result<(Vec<u8>, Option<(usize, usize, usize)>), String> {
    const MAX_PIXELS: u64 = 50_000_000;
    if h.clean_whole {
        let bytes = std::fs::read(&h.source).map_err(|e| format!("Lỗi đọc PDF gốc: {e}"))?;
        return Ok((bytes, None));
    }
    let compatibility =
        iai_formats::pdf::hybrid_overlay_compatibility(&h.source).unwrap_or_default();
    let baked: Vec<usize> = h
        .snapshots
        .iter()
        .filter_map(|(page, edited)| edited.is_some().then_some(*page))
        .collect();
    let make_pages = |allow_overlay: bool| {
        let mut pages = Vec::with_capacity(h.snapshots.len());
        for (page, edited) in &h.snapshots {
            let content = if let Some((canvas, reference)) = edited {
                let overlay = (allow_overlay && compatibility.get(*page).copied().unwrap_or(false))
                    .then(|| reference.safe_overlay_pdf_parts(canvas))
                    .flatten();
                if let Some((rgba, vectors)) = overlay {
                    iai_formats::pdf::HybridPageContent::Overlay {
                        rgba,
                        vectors,
                        width: canvas.width,
                        height: canvas.height,
                        dpi: canvas.metadata.resolution_ppi,
                    }
                } else {
                    let rgba = canvas.export_flat_up_to(MAX_PIXELS).ok_or_else(|| {
                        format!(
                            "Trang PDF {} quá lớn để xuất an toàn",
                            page.saturating_add(1)
                        )
                    })?;
                    let (rgba, width, height, dpi) = prepare_pdf_pixels(
                        rgba,
                        canvas.width,
                        canvas.height,
                        canvas.metadata.resolution_ppi,
                        target_dpi,
                    )?;
                    iai_formats::pdf::HybridPageContent::Raster {
                        rgba,
                        width,
                        height,
                        dpi,
                    }
                }
            } else {
                iai_formats::pdf::HybridPageContent::Original
            };
            pages.push(iai_formats::pdf::HybridPage {
                source_index: *page,
                content,
            });
        }
        Ok::<_, String>(pages)
    };
    let build = |pages: Vec<iai_formats::pdf::HybridPage>| {
        iai_formats::pdf::build_hybrid_pdf_with_global_edits_baked(
            &h.source,
            &pages,
            &h.global_edits,
            &baked,
        )
    };
    let hybrid = make_pages(true)
        .and_then(build)
        .or_else(|_| make_pages(false).and_then(build))
        .map_err(|e| format!("Lỗi tạo PDF: {e}"))?;
    let counts = (
        hybrid.vector_pages,
        hybrid.overlay_pages,
        hybrid.raster_pages,
    );
    Ok((hybrid.bytes, Some(counts)))
}

impl Command for ExportPdf {
    type Params = PdfParams;
    type Output = PdfWritten;
    fn meta(&self) -> &'static CommandMeta {
        &PDF
    }

    fn plan(&self, s: &mut Session<'_>, p: PdfParams) -> Result<Plan<PdfWritten>, CmdError> {
        let source = if p.all_open_documents {
            let mut pages = Vec::new();
            let mut docs = 0usize;
            for doc in s.workspace().documents() {
                // A flowing-text tab has no raster page to place; leave it out
                // rather than inserting its 1×1 compatibility canvas.
                if doc.is_flow_text() || doc.deferred_raw {
                    continue;
                }
                let before = pages.len();
                for i in 0..doc.all_page_canvases().len() {
                    if let Some(page) = page_snapshot(doc, i) {
                        pages.push(page);
                    }
                }
                docs = docs.saturating_add(usize::from(pages.len() > before));
            }
            if pages.is_empty() {
                return Err(not_enabled(
                    "no open document has pages to write",
                    "Không có tài liệu nào để gộp PDF",
                ));
            }
            Source::Pages(pages, Some(docs))
        } else {
            let doc = s.doc_id(p.doc)?;
            let d = s.document(Some(doc))?;
            if d.deferred_raw {
                s.canvas(Some(doc))?;
            }
            if let Some(flow) = &d.flow_text {
                let document = flow.document_arc().ok_or_else(|| {
                    not_enabled(
                        "this text document is written to PDF from the app window",
                        "Văn bản này được xuất PDF trong cửa sổ app",
                    )
                })?;
                let pages = chosen(&p.pages, d.page_count())?;
                Source::Text { document, pages }
            } else if let Some(pdf) = &d.pdf_document {
                let positions = chosen(&p.pages, pdf.selected_pages.len().max(1))?;
                Source::Hybrid(Box::new(hybrid_source(d, &positions)?))
            } else {
                let chosen = chosen(&p.pages, d.page_count())?;
                let mut pages = Vec::with_capacity(chosen.len());
                for i in chosen {
                    pages.push(page_snapshot(d, i).ok_or_else(|| {
                        CmdError::new(ErrorCode::NotFound, format!("no page {i}"))
                    })?);
                }
                Source::Pages(pages, None)
            }
        };
        let path = PathBuf::from(&p.path);
        let marks = PrintMarks {
            bleed_mm: p.bleed_mm,
            crop_marks: p.crop_marks,
            registration_marks: p.registration_marks,
        };
        let icc = p.embed_icc.then(iai_color::cms::srgb_icc_bytes);
        let dpi = p.dpi.unwrap_or(0);
        let label_vi = match &source {
            Source::Pages(_, Some(docs)) => format!("Đang gộp {docs} tài liệu thành PDF"),
            Source::Pages(pages, None) => format!("Đang xuất PDF {} trang", pages.len()),
            Source::Text { pages, .. } => format!("Đang xuất PDF văn bản {} trang", pages.len()),
            Source::Hybrid(h) => format!("Đang xuất PDF {} trang", h.snapshots.len()),
        };
        Ok(Plan::Job(Job {
            doc: None,
            label_vi,
            work: Box::new(move |cx| {
                cx.progress(0.05, "Đang dựng PDF…");
                let fail = |e: String| CmdError::new(ErrorCode::Io, e.clone()).vi(e);
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let (count, kind) = match &source {
                    Source::Pages(pages, _) => (pages.len(), "raster"),
                    Source::Text { pages, .. } => (pages.len(), "text"),
                    Source::Hybrid(h) => (h.snapshots.len(), "hybrid"),
                };
                if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                    std::fs::create_dir_all(dir)?;
                }
                let said = match source {
                    Source::Pages(pages, docs) => {
                        let bytes = build_snapshots_pdf(pages, dpi, marks, icc).map_err(fail)?;
                        iai_platform::atomic_file::write_bytes_atomic(&path, &bytes)
                            .map_err(|e| fail(format!("Lỗi ghi PDF: {e}")))?;
                        match docs {
                            Some(docs) => {
                                format!("Đã gộp {docs} tài liệu ({count} trang) thành PDF: {name}")
                            }
                            None => format!("Đã xuất PDF {count} trang: {name}"),
                        }
                    }
                    Source::Text { document, pages } => {
                        let mut fonts = cosmic_text::FontSystem::new();
                        let layout = iai_text::text_layout::DocumentLayout::build(
                            document.as_ref(),
                            96.0,
                            &mut fonts,
                        );
                        layout
                            .write_text_pdf_pages(&mut fonts, &path, &pages)
                            .map_err(fail)?;
                        format!("Đã xuất PDF văn bản {count} trang: {name}")
                    }
                    Source::Hybrid(h) => {
                        let (bytes, counts) = hybrid_bytes(&h, dpi).map_err(fail)?;
                        let what = if counts.is_some() {
                            "Lỗi ghi PDF"
                        } else {
                            "Lỗi sao chép PDF gốc"
                        };
                        iai_platform::atomic_file::write_bytes_atomic(&path, &bytes)
                            .map_err(|e| fail(format!("{what}: {e}")))?;
                        match counts {
                            Some((v, o, r)) => {
                                format!("Đã xuất PDF hybrid ({v} vector, {o} overlay, {r} raster)")
                            }
                            None => "Đã xuất PDF vector gốc".to_string(),
                        }
                    }
                };
                cx.check_cancelled()?;
                let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                Ok(Box::new(move |s: &mut Session<'_>| {
                    s.notice(said, format!("PDF written, {count} pages: {name}"));
                    Ok(PdfWritten {
                        path: path.display().to_string(),
                        pages: count,
                        bytes,
                        kind: kind.to_string(),
                    })
                }) as Apply<PdfWritten>)
            }),
        }))
    }
}

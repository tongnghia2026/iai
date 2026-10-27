//! Files: a new blank document, open, save, export a picture.

use std::path::{Path, PathBuf};

use iai_canvas::canvas::{Canvas, MAX_DIMENSION};
use iai_canvas::document::{Document, FlowTextBacking, FlowTextDocumentState, file_modified_at};
use iai_formats::iai::IaiLoad;
use iai_formats::pdf::{PdfPages, PdfRenderPlan};
use iai_formats::{ExportOptions, FormatRegistry};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{not_enabled, parse_color};
use crate::{
    Apply, CmdError, Command, CommandMeta, Effect, EngineEvent, ErrorCode, Job, JobCtx, Plan,
    Session, Validate,
};

/// A document as the file commands report it.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Opened {
    pub doc: u32,
    pub title: String,
    pub width_px: u32,
    pub height_px: u32,
    pub dpi: f32,
    pub pages: usize,
}

fn opened(d: &Document) -> Opened {
    Opened {
        doc: d.id.0,
        title: d.tab_title().to_string(),
        width_px: d.canvas.width,
        height_px: d.canvas.height,
        dpi: d.canvas.metadata.resolution_ppi,
        pages: d.page_count(),
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NewParams {
    #[schemars(range(min = 1, max = 30000))]
    pub width_px: u32,
    #[schemars(range(min = 1, max = 30000))]
    pub height_px: u32,
    /// Pixels per inch: with the size in pixels, the size on paper.
    #[serde(default = "dpi_72")]
    #[schemars(range(min = 1.0, max = 9600.0))]
    pub dpi: f32,
    /// "white" (default), "black", "transparent" or a colour "#RRGGBB".
    pub background: Option<String>,
    /// The tab's name.
    #[schemars(length(min = 1, max = 200))]
    pub title: Option<String>,
    /// Start it in CMYK (the built-in Generic space) instead of RGB, for
    /// print work.
    #[serde(default)]
    pub cmyk: bool,
}

fn dpi_72() -> f32 {
    72.0
}

impl Validate for NewParams {
    fn validate(&self) -> Result<(), CmdError> {
        if let Some(b) = &self.background
            && !matches!(b.as_str(), "white" | "black" | "transparent")
        {
            parse_color("background", b)?;
        }
        Ok(())
    }
}

static NEW: CommandMeta = CommandMeta {
    id: "file.new",
    label_vi: "Tạo ảnh mới",
    label_en: "New Document",
    doc: "Open a new blank document in a tab of its own and show it, as File ▸ New does.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct NewDocument;

impl Command for NewDocument {
    type Params = NewParams;
    type Output = Opened;
    fn meta(&self) -> &'static CommandMeta {
        &NEW
    }
    fn plan(&self, s: &mut Session<'_>, p: NewParams) -> Result<Plan<Opened>, CmdError> {
        let (w, h) = (p.width_px, p.height_px);
        let mut canvas = Canvas::new(w, h);
        let background = p.background.as_deref().unwrap_or("white");
        if let Some(bg) = canvas.layer_stack.layers.first_mut() {
            match background {
                "white" => bg.is_background = true,
                "black" => {
                    bg.tiles = iai_raster::tile::TileMap::new_black(w, h);
                    bg.is_background = true;
                }
                "transparent" => {
                    bg.tiles = iai_raster::tile::TileMap::new(w, h);
                    bg.name = "Layer 1".to_string();
                    bg.is_background = false;
                    bg.locked = false;
                }
                colour => {
                    let [r, g, b, a] = parse_color("background", colour)?;
                    bg.tiles = iai_raster::tile::TileMap::new_solid(w, h, r, g, b, a);
                    bg.is_background = true;
                }
            }
        }
        canvas.flatten_full();
        canvas.metadata.resolution_ppi = p.dpi;
        if p.cmyk {
            // Made RGB, then laid on the ink planes (the empty history cleared).
            let _ = canvas.convert_to_cmyk(iai_canvas::canvas::CmykProfile::Naive);
        }
        let id = s.new_document_id();
        let mut doc = Document::from_canvas(id, canvas, None);
        if let Some(title) = p.title {
            doc.title = title;
        }
        doc.mark_saved();
        let out = opened(&doc);
        s.add_document(doc);
        s.notice(format!("Ảnh mới {w}×{h}"), format!("New document {w}×{h}"));
        Ok(Plan::Done(out))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OpenParams {
    /// The file: an image (JPEG, PNG, TIFF, WebP, BMP, PSD, camera RAW), a
    /// PDF (its pages, rasterised) or an iAi document (.iai).
    #[schemars(length(min = 1, max = 4096))]
    pub path: String,
    /// A PDF only: the pages to open, 0 being the first. They open in the
    /// PDF's order, each once; every page when omitted.
    #[schemars(length(min = 1, max = 10000))]
    pub pages: Option<Vec<usize>>,
    /// A PDF only: the resolution its pages are rendered at, in pixels per
    /// inch; 300 when omitted. Lower only where a page would be too large.
    #[schemars(range(min = 18, max = 600))]
    pub dpi: Option<u32>,
}

impl Validate for OpenParams {
    fn validate(&self) -> Result<(), CmdError> {
        if iai_formats::pdf::is_pdf_path(Path::new(&self.path)) {
            return Ok(());
        }
        if self.pages.is_some() {
            return Err(CmdError::bad_param("pages", "to be left out: not a PDF"));
        }
        if self.dpi.is_some() {
            return Err(CmdError::bad_param("dpi", "to be left out: not a PDF"));
        }
        Ok(())
    }
}

/// Image endings `file.list` reads the pixel size of.
const LISTED_IMAGES: [&str; 8] = ["jpg", "jpeg", "png", "tif", "tiff", "webp", "bmp", "gif"];
/// The most files one listing names.
const MAX_LISTED: usize = 2000;

static LIST: CommandMeta = CommandMeta {
    id: "file.list",
    label_vi: "Xem file trong thư mục",
    label_en: "List Files",
    doc: "The files and sub-folders of a folder (inside the folders granted): each file's name, \
          full path and size, and an image's size in pixels; by name. `ext` keeps only those \
          endings (`[\"jpg\", \"png\"]`). Use it to find what to open; it changes nothing.",
    effects: &[Effect::ReadFiles],
    featured: true,
};

pub struct ListFiles;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListFilesParams {
    /// The folder.
    #[schemars(length(min = 1, max = 4096))]
    pub path: String,
    /// Only files with these endings, without the dot, any case; every file when left out.
    #[serde(default)]
    pub ext: Vec<String>,
}
impl Validate for ListFilesParams {}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListedFile {
    pub name: String,
    pub path: String,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width_px: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height_px: Option<u32>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Listing {
    pub folder: String,
    pub files: Vec<ListedFile>,
    pub folders: Vec<String>,
    /// Files past the first 2000, not named.
    pub more: usize,
}

impl Command for ListFiles {
    type Params = ListFilesParams;
    type Output = Listing;

    fn meta(&self) -> &'static CommandMeta {
        &LIST
    }

    fn plan(&self, _: &mut Session<'_>, p: ListFilesParams) -> Result<Plan<Listing>, CmdError> {
        let dir = PathBuf::from(&p.path);
        let read = std::fs::read_dir(&dir).map_err(|e| {
            CmdError::new(
                ErrorCode::NotFound,
                format!("no folder {}: {e}", dir.display()),
            )
            .vi(format!("Không có thư mục {}", dir.display()))
        })?;
        let want: Vec<String> = p
            .ext
            .iter()
            .map(|e| e.trim_start_matches('.').to_lowercase())
            .collect();
        let mut files = Vec::new();
        let mut folders = Vec::new();
        for entry in read.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            match entry.file_type() {
                Ok(t) if t.is_dir() => folders.push(name),
                Ok(t) if t.is_file() => {
                    let ext = Path::new(&name)
                        .extension()
                        .map(|e| e.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    if want.is_empty() || want.contains(&ext) {
                        files.push((name, entry.path(), ext));
                    }
                }
                _ => {}
            }
        }
        files.sort_by_key(|(name, ..)| name.to_lowercase());
        folders.sort_by_key(|n| n.to_lowercase());
        let more = files.len().saturating_sub(MAX_LISTED);
        files.truncate(MAX_LISTED);
        let files = files
            .into_iter()
            .map(|(name, path, ext)| {
                let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                let size = LISTED_IMAGES
                    .contains(&ext.as_str())
                    .then(|| image::image_dimensions(&path).ok())
                    .flatten();
                ListedFile {
                    name,
                    path: path.display().to_string(),
                    bytes,
                    width_px: size.map(|s| s.0),
                    height_px: size.map(|s| s.1),
                }
            })
            .collect();
        Ok(Plan::Done(Listing {
            folder: dir.display().to_string(),
            files,
            folders,
            more,
        }))
    }
}

static OPEN: CommandMeta = CommandMeta {
    id: "file.open",
    label_vi: "Mở file",
    label_en: "Open File",
    doc: "Open a file as a new document (tab) and show it. A PDF opens as one document with a \
          page per PDF page (`pages` picks some, `dpi` sets the resolution, 300 by default), as \
          an untitled picture: `file.save` it as .iai or `export.pdf` it. Pages that would not \
          fit in memory together are refused (`too_large`, with the call that fits). Large, \
          RAW and many-page files take seconds: `background: true` gives a job id.",
    effects: &[Effect::Edit, Effect::ReadFiles],
    featured: true,
};

pub struct OpenFile;

/// What to make of a PDF: which pages, at what resolution.
pub(super) struct PdfChoice {
    pub(super) pages: Option<Vec<usize>>,
    pub(super) dpi: Option<u32>,
}

/// What a file decodes to, before it is a document.
pub(super) enum Decoded {
    Pages {
        pages: Vec<Canvas>,
        active: usize,
        master: Option<Box<Canvas>>,
    },
    Text(FlowTextBacking),
}

fn decode(path: &Path) -> Result<Decoded, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if ext == "iai" {
        return match iai_formats::iai::load(path)? {
            IaiLoad::Canvas(canvas) => Ok(Decoded::Pages {
                pages: vec![canvas],
                active: 0,
                master: None,
            }),
            IaiLoad::ArtboardDoc(doc) => Ok(Decoded::Pages {
                active: doc.active_page,
                pages: doc.pages,
                master: doc.master.map(Box::new),
            }),
            IaiLoad::FlowTextDocument(backing) => Ok(Decoded::Text(backing)),
            IaiLoad::PdfProject(_) => {
                Err("an iAi PDF project opens in the app window (File > Open)".to_string())
            }
        };
    }
    let pages = FormatRegistry::new().import_many(path)?;
    if pages.is_empty() {
        return Err("the file holds no image".to_string());
    }
    Ok(Decoded::Pages {
        pages,
        active: 0,
        master: None,
    })
}

/// The pages of a PDF, rendered one at a time; refused before any is
/// rendered when together they come to more than `limit` pixels.
pub(super) fn decode_pdf(
    path: &Path,
    choice: &PdfChoice,
    limit: u64,
    cx: &JobCtx,
) -> Result<Decoded, CmdError> {
    let pdf = PdfPages::open(path).map_err(|e| unreadable(path, e))?;
    let count = pdf.page_count();
    let wanted: Vec<usize> = match &choice.pages {
        Some(list) => {
            if let Some(bad) = list.iter().find(|&&p| p >= count) {
                return Err(CmdError::new(
                    ErrorCode::NotFound,
                    format!("no page {bad}: the PDF has {count} (the first is 0)"),
                )
                .vi(format!("Không có trang {}", bad.saturating_add(1))));
            }
            list.clone()
        }
        None => (0..count).collect(),
    };
    let dpi = choice.dpi.map(|d| d as f32);
    let plan = pdf.plan(&wanted, dpi).map_err(|e| unreadable(path, e))?;
    if plan.pixels > limit {
        return Err(too_large_to_open(path, &pdf, &plan, dpi, limit));
    }
    let total = plan.pages.len();
    let mut renders = pdf.renders(&plan);
    let mut pages = Vec::with_capacity(total);
    for done in 0..total {
        cx.check_cancelled()?;
        cx.progress(
            0.1 + 0.85 * done as f32 / total as f32,
            format!("Đang dựng trang {}/{total}…", done.saturating_add(1)),
        );
        let (_, canvas) = renders
            .next()
            .ok_or_else(|| CmdError::internal("a PDF page was not rendered"))?
            .map_err(|e| unreadable(path, e))?;
        pages.push(canvas);
    }
    Ok(Decoded::Pages {
        pages,
        active: 0,
        master: None,
    })
}

/// Pages that would not fit in memory together: what would, as numbers and
/// as the call to make (the first pages that fit).
fn too_large_to_open(
    path: &Path,
    pdf: &PdfPages,
    plan: &PdfRenderPlan,
    dpi: Option<f32>,
    limit: u64,
) -> CmdError {
    let fits = |pages: &[usize], dpi: Option<f32>| {
        pdf.plan(pages, dpi)
            .is_ok_and(|fewer| fewer.pixels <= limit)
    };
    // The longest run of the asked pages, from the first, that fits.
    let (mut lo, mut hi) = (0usize, plan.pages.len());
    while lo < hi {
        let mid = lo.saturating_add(hi.saturating_sub(lo).div_ceil(2));
        if plan.pages.get(..mid).is_some_and(|pages| fits(pages, dpi)) {
            lo = mid;
        } else {
            hi = mid.saturating_sub(1);
        }
    }
    let first: &[usize] = plan.pages.get(..lo).unwrap_or_default();
    // The resolution at which every asked page fits: pixels grow with its square.
    let mut all_at = (plan.dpi * (limit as f32 / plan.pixels as f32).sqrt()).floor() as u32;
    while all_at >= 18 && !fits(&plan.pages, Some(all_at as f32)) {
        all_at = all_at.saturating_sub(1);
    }
    let n = plan.pages.len();
    let mp = |pixels: u64| pixels / 1_000_000;
    let mut message = format!(
        "{n} pages at {:.0} dpi come to {} MP, more than this machine holds open at once \
         ({} MP): open fewer `pages` (the first {} fit)",
        plan.dpi,
        mp(plan.pixels),
        mp(limit),
        first.len()
    );
    if all_at >= 18 {
        message.push_str(&format!(" or a lower `dpi` (all fit at {all_at})"));
    }
    let mut error = CmdError::new(ErrorCode::TooLarge, message).vi(format!(
        "{}: {n} trang ở {:.0} dpi quá lớn để mở cùng lúc — hãy mở ít trang hơn hoặc giảm dpi",
        file_name(path),
        plan.dpi
    ));
    if !first.is_empty() {
        let mut args = serde_json::Map::new();
        args.insert("path".into(), path.display().to_string().into());
        args.insert("pages".into(), first.into());
        if let Some(dpi) = dpi {
            args.insert("dpi".into(), (dpi as u32).into());
        }
        error = error.with_hint(serde_json::json!({"command": "file.open", "args": args}));
    }
    error
}

fn unreadable(path: &Path, e: String) -> CmdError {
    CmdError::new(ErrorCode::Io, format!("{}: {e}", path.display()))
        .vi(format!("Không mở được {}: {e}", file_name(path)))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

impl Command for OpenFile {
    type Params = OpenParams;
    type Output = Opened;
    fn meta(&self) -> &'static CommandMeta {
        &OPEN
    }
    fn plan(&self, _: &mut Session<'_>, p: OpenParams) -> Result<Plan<Opened>, CmdError> {
        let path = PathBuf::from(&p.path);
        if !path.is_file() {
            return Err(CmdError::new(
                ErrorCode::NotFound,
                format!("no file at {}", path.display()),
            )
            .vi(format!("Không có file {}", file_name(&path))));
        }
        let pdf = iai_formats::pdf::is_pdf_path(&path).then_some(PdfChoice {
            pages: p.pages,
            dpi: p.dpi,
        });
        Ok(Plan::Job(Job {
            doc: None,
            label_vi: format!("Mở {}", file_name(&path)),
            work: Box::new(move |cx| {
                cx.progress(0.1, format!("Đang đọc {}…", file_name(&path)));
                let decoded = match &pdf {
                    Some(choice) => decode_pdf(
                        &path,
                        choice,
                        iai_platform::hw::open_pages_pixel_limit(),
                        cx,
                    )?,
                    None => decode(&path).map_err(|e| unreadable(&path, e))?,
                };
                cx.check_cancelled()?;
                Ok(Box::new(move |s: &mut Session<'_>| {
                    let id = s.new_document_id();
                    let mut doc = match decoded {
                        Decoded::Text(backing) => {
                            let mut doc = Document::new_flow_text(id);
                            doc.flow_text = Some(FlowTextDocumentState::from_backing(backing));
                            doc
                        }
                        Decoded::Pages {
                            pages,
                            active,
                            master,
                        } => {
                            let mut slots: Vec<Option<Canvas>> =
                                pages.into_iter().map(Some).collect();
                            let active = active.min(slots.len().saturating_sub(1));
                            let canvas = slots
                                .get_mut(active)
                                .and_then(Option::take)
                                .ok_or_else(|| CmdError::internal("no page"))?;
                            let mut doc = Document::from_canvas(id, canvas, None);
                            if slots.len() > 1 {
                                doc.pages = slots;
                                doc.active_artboard = active;
                            }
                            doc.master = master;
                            doc
                        }
                    };
                    doc.title = path
                        .file_stem()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "Untitled".to_string());
                    // A rasterised PDF is not its file: saving it in place
                    // would replace the PDF (all its pages) with these.
                    if pdf.is_none() {
                        doc.file_modified_at = file_modified_at(&path);
                        doc.path = Some(path.clone());
                    }
                    doc.mark_saved();
                    let out = opened(&doc);
                    s.add_document(doc);
                    s.notice(
                        format!("Đã mở {}", file_name(&path)),
                        format!("Opened {}", file_name(&path)),
                    );
                    Ok(out)
                }) as Apply<Opened>)
            }),
        }))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SaveParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// Where to write; the document's own file when omitted. A document of
    /// several pages, or of text, is written as .iai only.
    #[schemars(length(min = 1, max = 4096))]
    pub path: Option<String>,
}
impl Validate for SaveParams {}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Saved {
    pub doc: u32,
    pub path: String,
    pub bytes: u64,
}

static SAVE: CommandMeta = CommandMeta {
    id: "file.save",
    label_vi: "Lưu",
    label_en: "Save",
    doc: "Write a document to its file, or to `path` (then it becomes its file). .iai keeps \
          layers and pages; an image format (.png, .jpg, .tif, .webp) flattens them. Use \
          `file.export` for a copy that leaves the document's file alone.",
    effects: &[Effect::WriteFiles],
    featured: true,
};

pub struct SaveFile;

/// What a save writes, detached from the live document.
enum Content {
    Image(Canvas),
    Pages {
        pages: Vec<Canvas>,
        active: usize,
        master: Option<Canvas>,
    },
    Text(FlowTextBacking),
}

fn detached(canvas: &Canvas) -> Canvas {
    let mut snapshot = canvas.export_snapshot();
    snapshot.channels.alpha = canvas.channels.alpha.clone();
    snapshot
}

fn write(content: &Content, path: &Path, opts: &ExportOptions) -> Result<(), String> {
    match content {
        Content::Image(canvas) => FormatRegistry::new().export(canvas, path, opts),
        Content::Pages {
            pages,
            active,
            master,
        } => {
            let refs: Vec<&Canvas> = pages.iter().collect();
            iai_formats::iai::write_artboard_doc(path, &refs, *active, master.as_ref())
        }
        Content::Text(FlowTextBacking::Legacy(document)) => {
            iai_formats::iai::write_flow_text_doc(path, document.as_ref())
        }
        Content::Text(FlowTextBacking::CanvasEditor(document)) => {
            iai_formats::iai::write_canvas_editor_doc(path, document)
        }
    }
}

fn has_writer(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| iai_formats::ExportFormat::from_extension(e).is_some())
}

impl Command for SaveFile {
    type Params = SaveParams;
    type Output = Saved;
    fn meta(&self) -> &'static CommandMeta {
        &SAVE
    }
    fn plan(&self, s: &mut Session<'_>, p: SaveParams) -> Result<Plan<Saved>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let d = s.document(Some(doc))?;
        let path = match (&p.path, &d.path) {
            (Some(path), _) => PathBuf::from(path),
            (None, Some(path)) => path.clone(),
            (None, None) => {
                return Err(CmdError::bad_param(
                    "path",
                    "a file name: the document has never been saved",
                ));
            }
        };
        let is_iai = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("iai"));
        if d.pdf_document.is_some() {
            return Err(not_enabled(
                "a PDF opened page by page is saved from the app window",
                "Dự án PDF được lưu trong cửa sổ app",
            ));
        }
        let content = if let Some(flow) = &d.flow_text {
            Content::Text(flow.backing().clone())
        } else if !d.pages.is_empty() {
            let pages = d.all_page_canvases().into_iter().map(detached).collect();
            Content::Pages {
                pages,
                active: d.active_artboard,
                master: d.master_canvas().map(detached),
            }
        } else {
            if !is_iai && !has_writer(&path) {
                return Err(CmdError::bad_param(
                    "path",
                    "a file ending in .iai, .png, .jpg, .tif, .webp, .bmp or .pdf",
                ));
            }
            Content::Image(detached(&d.canvas))
        };
        if !matches!(content, Content::Image(_)) && !is_iai {
            return Err(CmdError::bad_param(
                "path",
                "a .iai file: this document has several pages or is text",
            ));
        }
        let marks: Vec<_> = std::iter::once(d.canvas.history_checkpoint()).collect();
        Ok(Plan::Job(Job {
            doc: Some(doc),
            label_vi: format!("Lưu {}", file_name(&path)),
            work: Box::new(move |cx| {
                cx.progress(0.1, format!("Đang lưu {}…", file_name(&path)));
                if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                    std::fs::create_dir_all(dir)?;
                }
                write(&content, &path, &ExportOptions::default()).map_err(|e| {
                    CmdError::new(ErrorCode::Io, format!("{}: {e}", path.display()))
                        .vi(format!("Không lưu được {}: {e}", file_name(&path)))
                })?;
                let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                Ok(Box::new(move |s: &mut Session<'_>| {
                    let d = s.document_mut(Some(doc))?;
                    d.path = Some(path.clone());
                    d.file_modified_at = file_modified_at(&path);
                    // Saved only if nothing changed while it was written.
                    if marks.first() == Some(&d.canvas.history_checkpoint()) {
                        d.mark_saved();
                    }
                    let shown = path.display().to_string();
                    s.emit(EngineEvent::Saved {
                        doc,
                        path: shown.clone(),
                    });
                    s.notice(
                        format!("Đã lưu {}", file_name(&path)),
                        format!("Saved {}", file_name(&path)),
                    );
                    Ok(Saved {
                        doc,
                        path: shown,
                        bytes,
                    })
                }) as Apply<Saved>)
            }),
        }))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExportParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// Where to write; the format comes from the ending: .png, .jpg, .tif,
    /// .webp, .bmp.
    #[schemars(length(min = 1, max = 4096))]
    pub path: String,
    /// Fit the longer side to this many pixels (only ever smaller).
    #[schemars(range(min = 16, max = 30000))]
    pub long_edge_px: Option<u32>,
    /// Sharpen for the output size, 0 (off) to 100.
    #[serde(default)]
    #[schemars(range(min = 0, max = 100))]
    pub sharpen: u8,
    /// Embed the colour profile (default).
    #[serde(default = "yes")]
    pub embed_icc: bool,
}

fn yes() -> bool {
    true
}

impl Validate for ExportParams {}

static EXPORT: CommandMeta = CommandMeta {
    id: "file.export",
    label_vi: "Xuất ảnh",
    label_en: "Export Image",
    doc: "Write the shown page, flattened, to an image file (.png, .jpg, .tif, .webp, .bmp), \
          optionally made smaller and sharpened for the web. The document is left as it is. For \
          PDF use `export.pdf`.",
    effects: &[Effect::WriteFiles],
    featured: false,
};

pub struct ExportImage;

impl Command for ExportImage {
    type Params = ExportParams;
    type Output = Saved;
    fn meta(&self) -> &'static CommandMeta {
        &EXPORT
    }
    fn plan(&self, s: &mut Session<'_>, p: ExportParams) -> Result<Plan<Saved>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let path = PathBuf::from(&p.path);
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if !matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "tif" | "tiff" | "webp" | "bmp"
        ) {
            return Err(CmdError::bad_param(
                "path",
                "a file ending in .png, .jpg, .tif, .webp or .bmp",
            ));
        }
        let canvas = detached(s.canvas(Some(doc))?);
        if canvas.width > MAX_DIMENSION || canvas.height > MAX_DIMENSION {
            return Err(CmdError::new(
                ErrorCode::TooLarge,
                "the page is too large to export",
            ));
        }
        let opts = ExportOptions {
            embed_icc: p.embed_icc,
            resize_long_edge: p.long_edge_px,
            output_sharpen: p.sharpen.min(100),
            ..ExportOptions::default()
        };
        Ok(Plan::Job(Job {
            doc: None,
            label_vi: format!("Xuất {}", file_name(&path)),
            work: Box::new(move |cx| {
                cx.progress(0.1, format!("Đang xuất {}…", file_name(&path)));
                if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                    std::fs::create_dir_all(dir)?;
                }
                FormatRegistry::new()
                    .export(&canvas, &path, &opts)
                    .map_err(|e| {
                        CmdError::new(ErrorCode::Io, format!("{}: {e}", path.display()))
                            .vi(format!("Không xuất được {}: {e}", file_name(&path)))
                    })?;
                let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                Ok(Box::new(move |s: &mut Session<'_>| {
                    s.notice(
                        format!("Đã xuất {}", file_name(&path)),
                        format!("Exported {}", file_name(&path)),
                    );
                    Ok(Saved {
                        doc,
                        path: path.display().to_string(),
                        bytes,
                    })
                }) as Apply<Saved>)
            }),
        }))
    }
}

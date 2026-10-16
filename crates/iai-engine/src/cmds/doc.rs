//! Reading the open documents: what is open, what one holds; showing one.

use iai_canvas::canvas::Canvas;
use iai_canvas::document::Document;
use iai_doc::layer::LayerType;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{Blend, Bounds, DocParams};
use crate::{CmdError, Command, CommandMeta, Effect, Plan, Session, Validate};

/// One open document in `doc.list`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocSummary {
    pub doc: u32,
    pub title: String,
    /// The file it was opened from or saved to.
    pub path: Option<String>,
    /// `canvas` (layers of pixels and shapes) or `text` (a flowing text document).
    pub kind: String,
    pub width_px: u32,
    pub height_px: u32,
    pub pages: usize,
    pub shown: bool,
    /// Has changes not saved to its file.
    pub modified: bool,
    pub revision: u64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocList {
    pub docs: Vec<DocSummary>,
    /// The document shown, if any.
    pub shown: Option<u32>,
}

fn page_count(d: &Document) -> usize {
    match &d.pdf_document {
        Some(pdf) => pdf.selected_pages.len().max(1),
        None => d.page_count(),
    }
}

static LIST: CommandMeta = CommandMeta {
    id: "doc.list",
    label_vi: "Danh sách tài liệu",
    label_en: "List Documents",
    doc: "The open documents (tabs): id, title, file, size in pixels, page count, and which is shown. \
          Every other command names a document by this id (`doc`).",
    effects: &[Effect::Query],
    featured: false,
};

pub struct ListDocs;

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoParams {}
impl Validate for NoParams {}

impl Command for ListDocs {
    type Params = NoParams;
    type Output = DocList;

    fn meta(&self) -> &'static CommandMeta {
        &LIST
    }

    fn plan(&self, s: &mut Session<'_>, _: NoParams) -> Result<Plan<DocList>, CmdError> {
        let shown = s.active_id();
        let ids: Vec<u32> = s.workspace().documents().iter().map(|d| d.id.0).collect();
        let mut docs = Vec::with_capacity(ids.len());
        for id in ids {
            let revision = s.revision(id);
            let Some(d) = s.workspace().documents().iter().find(|d| d.id.0 == id) else {
                continue;
            };
            let mut one = summary_of(d, shown == Some(id));
            one.revision = revision;
            docs.push(one);
        }
        Ok(Plan::Done(DocList { docs, shown }))
    }
}

fn summary_of(d: &Document, shown: bool) -> DocSummary {
    DocSummary {
        doc: d.id.0,
        title: d.tab_title().to_string(),
        path: d.path.as_ref().map(|p| p.display().to_string()),
        kind: if d.is_flow_text() { "text" } else { "canvas" }.to_string(),
        width_px: d.canvas.width,
        height_px: d.canvas.height,
        pages: page_count(d),
        shown,
        modified: d.is_modified(),
        revision: 0,
    }
}

/// One layer as `doc.inspect` reports it, top of the stack first.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LayerInfo {
    pub id: u32,
    pub name: String,
    /// `raster`, `text`, `vector`, `group`, `adjustment` or `smart_object`.
    pub kind: String,
    pub visible: bool,
    pub locked: bool,
    /// 0 to 1.
    pub opacity: f32,
    pub blend: Blend,
    /// Where its pixels lie on the page.
    pub bounds: Bounds,
    /// The group holding it.
    pub parent: Option<u32>,
    pub is_background: bool,
    pub has_mask: bool,
    /// The text of a text layer.
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    pub index: usize,
    pub name: String,
    pub shown: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SelectionInfo {
    pub active: bool,
    pub bounds: Option<Bounds>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocInfo {
    #[serde(flatten)]
    pub summary: DocSummary,
    /// Pixels per inch: with the size in pixels, the size on paper.
    pub dpi: f32,
    pub width_mm: f32,
    pub height_mm: f32,
    /// The shown page's index (0 is the first).
    pub page: usize,
    pub page_list: Vec<PageInfo>,
    /// Layers of the shown page, top first.
    pub layers: Vec<LayerInfo>,
    pub active_layer: Option<u32>,
    pub selection: SelectionInfo,
    pub can_undo: bool,
    pub can_redo: bool,
}

fn layer_kind(t: &LayerType) -> &'static str {
    match t {
        LayerType::Raster => "raster",
        LayerType::Adjustment(_) => "adjustment",
        LayerType::Group => "group",
        LayerType::Text(_) => "text",
        LayerType::Vector(_) => "vector",
        LayerType::SmartObject => "smart_object",
    }
}

pub(crate) fn layer_infos(c: &Canvas) -> Vec<LayerInfo> {
    c.layer_stack
        .layers
        .iter()
        .rev()
        .map(|l| LayerInfo {
            id: l.id,
            name: l.name.clone(),
            kind: layer_kind(&l.layer_type).to_string(),
            visible: l.visible,
            locked: l.locked,
            opacity: l.opacity,
            blend: Blend::from_core(l.blend_mode),
            bounds: Bounds {
                x: l.offset.0,
                y: l.offset.1,
                w: l.width,
                h: l.height,
            },
            parent: l.parent_id,
            is_background: l.is_background,
            has_mask: l.mask.is_some(),
            text: match &l.layer_type {
                LayerType::Text(t) => Some(t.content.clone()),
                _ => None,
            },
        })
        .collect()
}

static INSPECT: CommandMeta = CommandMeta {
    id: "doc.inspect",
    label_vi: "Xem tài liệu",
    label_en: "Inspect Document",
    doc: "Everything an agent needs before editing a document: size in pixels and millimetres, \
          dpi, pages, the layers of the shown page (top first, with ids, bounds, opacity, blend, \
          text), the active layer, the selection's bounds, and the revision to pass as \
          `expectRevision`.",
    effects: &[Effect::Query],
    featured: true,
};

pub struct Inspect;

impl Command for Inspect {
    type Params = DocParams;
    type Output = DocInfo;

    fn meta(&self) -> &'static CommandMeta {
        &INSPECT
    }

    fn plan(&self, s: &mut Session<'_>, p: DocParams) -> Result<Plan<DocInfo>, CmdError> {
        let id = s.doc_id(p.doc)?;
        let revision = s.revision(id);
        let shown = s.active_id() == Some(id);
        let d = s.document(Some(id))?;
        let c = &d.canvas;
        let dpi = c.metadata.resolution_ppi.max(1.0);
        let mm = |px: u32| px as f32 / dpi * 25.4;
        let page = match &d.pdf_document {
            Some(pdf) => pdf
                .selected_pages
                .iter()
                .position(|&x| x == pdf.active_page)
                .unwrap_or(0),
            None => d.active_artboard,
        };
        let names: Vec<String> = match &d.pdf_document {
            Some(pdf) => pdf.display_names(),
            None => (0..d.page_count())
                .map(|i| d.page_display_name(i))
                .collect(),
        };
        let page_list = names
            .into_iter()
            .enumerate()
            .map(|(index, name)| PageInfo {
                index,
                name,
                shown: index == page,
            })
            .collect();
        let selection = SelectionInfo {
            active: c.selection.active,
            bounds: if c.selection.active {
                let (x0, y0, x1, y1) = c.selection.bounding_box_cached();
                Bounds::from_corners(x0, y0, x1, y1)
            } else {
                None
            },
        };
        let mut summary = summary_of(d, shown);
        summary.revision = revision;
        let info = DocInfo {
            summary,
            dpi,
            width_mm: mm(c.width),
            height_mm: mm(c.height),
            page,
            page_list,
            layers: if d.is_flow_text() {
                Vec::new()
            } else {
                layer_infos(c)
            },
            active_layer: c
                .layer_stack
                .layers
                .get(c.layer_stack.active_idx)
                .map(|l| l.id),
            selection,
            can_undo: c.can_undo(),
            can_redo: c.can_redo(),
        };
        Ok(Plan::Done(info))
    }
}

static SHOW: CommandMeta = CommandMeta {
    id: "doc.show",
    label_vi: "Hiện tài liệu",
    label_en: "Show Document",
    doc: "Bring a document's tab to the front. Commands that leave out `doc` work on it.",
    effects: &[Effect::Query],
    featured: false,
};

pub struct Show;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ShowParams {
    /// Document id (see `doc.list`).
    pub doc: u32,
}
impl Validate for ShowParams {}

impl Command for Show {
    type Params = ShowParams;
    type Output = super::common::Changed;

    fn meta(&self) -> &'static CommandMeta {
        &SHOW
    }

    fn plan(
        &self,
        s: &mut Session<'_>,
        p: ShowParams,
    ) -> Result<Plan<super::common::Changed>, CmdError> {
        s.show(p.doc)?;
        Ok(Plan::Done(super::common::Changed { doc: p.doc }))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PreviewParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The longer side of the picture, in pixels (it is never enlarged).
    #[serde(default = "edge_1024")]
    #[schemars(range(min = 16, max = 2048))]
    pub max_edge: u32,
}

fn edge_1024() -> u32 {
    1024
}

impl Validate for PreviewParams {}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub doc: u32,
    /// The picture's size.
    pub width_px: u32,
    pub height_px: u32,
    /// The page's own size.
    pub page_width_px: u32,
    pub page_height_px: u32,
    /// The picture as PNG, base64.
    pub png_base64: String,
}

static PREVIEW: CommandMeta = CommandMeta {
    id: "doc.preview",
    label_vi: "Ảnh xem trước",
    label_en: "Preview",
    doc: "A small PNG of what the shown page of a document looks like, every layer flattened, to \
          look at the result. For checks by numbers, `analyze.region` is exact and cheaper.",
    effects: &[Effect::Query],
    featured: false,
};

pub struct RenderPreview;

impl Command for RenderPreview {
    type Params = PreviewParams;
    type Output = Preview;

    fn meta(&self) -> &'static CommandMeta {
        &PREVIEW
    }

    fn plan(&self, s: &mut Session<'_>, p: PreviewParams) -> Result<Plan<Preview>, CmdError> {
        use base64::Engine as _;
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas(Some(doc))?;
        let (w, h) = (canvas.width, canvas.height);
        let image = image::RgbaImage::from_raw(w, h, canvas.flatten_for_export())
            .ok_or_else(|| CmdError::internal("the page did not flatten to its size"))?;
        let edge = p.max_edge.clamp(16, 2048);
        let scale = (edge as f32 / w.max(h).max(1) as f32).min(1.0);
        let (pw, ph) = (
            ((w as f32 * scale).round() as u32).max(1),
            ((h as f32 * scale).round() as u32).max(1),
        );
        let small = if (pw, ph) == (w, h) {
            image
        } else {
            image::imageops::resize(&image, pw, ph, image::imageops::FilterType::Triangle)
        };
        let mut png = Vec::new();
        small
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| CmdError::internal(format!("PNG: {e}")))?;
        Ok(Plan::Done(Preview {
            doc,
            width_px: pw,
            height_px: ph,
            page_width_px: w,
            page_height_px: h,
            png_base64: base64::engine::general_purpose::STANDARD.encode(&png),
        }))
    }
}

//! Pages of a document: add one, show one.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{DocParams, not_enabled};
use crate::{CmdError, Command, CommandMeta, Effect, EngineEvent, Plan, Session, Validate};

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Paged {
    pub doc: u32,
    /// The page shown now (0 is the first).
    pub page: usize,
    pub pages: usize,
}

/// Pages of `doc` are this engine's to change: not a PDF opened page by page,
/// not a text document.
fn own_pages(s: &Session<'_>, doc: u32) -> Result<(), CmdError> {
    let d = s.document(Some(doc))?;
    if d.pdf_document.is_some() {
        return Err(not_enabled(
            "the pages of an opened PDF are changed from its page bar",
            "Trang của PDF đang mở được đổi ở thanh trang",
        ));
    }
    if d.is_flow_text() {
        return Err(not_enabled(
            "a text document lays its pages out itself",
            "Tài liệu văn bản tự chia trang",
        ));
    }
    if d.deferred_raw {
        s.canvas(Some(doc))?;
    }
    Ok(())
}

static ADD: CommandMeta = CommandMeta {
    id: "page.add",
    label_vi: "Thêm trang",
    label_en: "Add Page",
    doc: "Add a blank page the size of the shown one, after the last, and show it.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct AddPage;

impl Command for AddPage {
    type Params = DocParams;
    type Output = Paged;
    fn meta(&self) -> &'static CommandMeta {
        &ADD
    }
    fn enabled(&self, s: &Session<'_>, p: &DocParams) -> Result<(), CmdError> {
        own_pages(s, s.doc_id(p.doc)?)
    }
    fn plan(&self, s: &mut Session<'_>, p: DocParams) -> Result<Plan<Paged>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let d = s.document_mut(Some(doc))?;
        let page = d.add_blank_page();
        let pages = d.page_count();
        s.emit(EngineEvent::PageShown { doc });
        s.notice(
            format!("Đã thêm trang {} / {pages}", page.saturating_add(1)),
            format!("Page {} of {pages} added", page.saturating_add(1)),
        );
        Ok(Plan::Done(Paged { doc, page, pages }))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SelectPageParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The page to show: 0 is the first (see `doc.inspect`'s `pageList`).
    #[schemars(range(min = 0, max = 100000))]
    pub page: usize,
}
impl Validate for SelectPageParams {}

static SELECT: CommandMeta = CommandMeta {
    id: "page.select",
    label_vi: "Chuyển trang",
    label_en: "Show Page",
    doc: "Show another page of a document. Layer and selection commands work on the page shown.",
    effects: &[Effect::Query],
    featured: false,
};

pub struct SelectPage;

impl Command for SelectPage {
    type Params = SelectPageParams;
    type Output = Paged;
    fn meta(&self) -> &'static CommandMeta {
        &SELECT
    }
    fn enabled(&self, s: &Session<'_>, p: &SelectPageParams) -> Result<(), CmdError> {
        own_pages(s, s.doc_id(p.doc)?)
    }
    fn plan(&self, s: &mut Session<'_>, p: SelectPageParams) -> Result<Plan<Paged>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let d = s.document_mut(Some(doc))?;
        let pages = d.page_count();
        if p.page >= pages {
            return Err(crate::CmdError::new(
                crate::ErrorCode::NotFound,
                format!(
                    "no page {} (the document has {pages}; the first is 0)",
                    p.page
                ),
            )
            .vi(format!("Không có trang {}", p.page.saturating_add(1))));
        }
        if p.page != d.active_artboard {
            d.switch_page(p.page);
            s.emit(EngineEvent::PageShown { doc });
            let n = p.page.saturating_add(1);
            s.notice(format!("Trang {n}"), format!("Page {n}"));
        }
        Ok(Plan::Done(Paged {
            doc,
            page: p.page,
            pages,
        }))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OnePageParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The page: 0 is the first.
    #[schemars(range(min = 0, max = 100000))]
    pub page: usize,
}
impl Validate for OnePageParams {}

fn page_of(pages: usize, page: usize) -> Result<(), CmdError> {
    if page < pages {
        Ok(())
    } else {
        Err(crate::CmdError::new(
            crate::ErrorCode::NotFound,
            format!("no page {page} (the document has {pages}; the first is 0)"),
        )
        .vi(format!("Không có trang {}", page.saturating_add(1))))
    }
}

static DELETE: CommandMeta = CommandMeta {
    id: "page.delete",
    label_vi: "Xóa trang",
    label_en: "Delete Page",
    doc: "Delete a page of a document (the page bar's Delete); one page always stays. When the \
          page shown goes, a neighbour is shown.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct DeletePage;

impl Command for DeletePage {
    type Params = OnePageParams;
    type Output = Paged;
    fn meta(&self) -> &'static CommandMeta {
        &DELETE
    }
    fn enabled(&self, s: &Session<'_>, p: &OnePageParams) -> Result<(), CmdError> {
        own_pages(s, s.doc_id(p.doc)?)
    }
    fn plan(&self, s: &mut Session<'_>, p: OnePageParams) -> Result<Plan<Paged>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let d = s.document_mut(Some(doc))?;
        page_of(d.page_count(), p.page)?;
        if d.page_count() <= 1 {
            return Err(not_enabled(
                "a document keeps at least one page",
                "Tài liệu phải còn ít nhất một trang",
            ));
        }
        d.remove_page(p.page);
        let (page, pages) = (d.active_artboard, d.page_count());
        s.emit(EngineEvent::PageShown { doc });
        s.notice(
            format!("Đã xoá trang · còn {pages}"),
            format!("Page deleted; {pages} left"),
        );
        Ok(Plan::Done(Paged { doc, page, pages }))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MovePageParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The page to move (0 is the first) and where it goes.
    #[schemars(range(min = 0, max = 100000))]
    pub from: usize,
    #[schemars(range(min = 0, max = 100000))]
    pub to: usize,
}
impl Validate for MovePageParams {}

static MOVE: CommandMeta = CommandMeta {
    id: "page.move",
    label_vi: "Đổi thứ tự trang",
    label_en: "Move Page",
    doc: "Move a page to another place in the document's order; what each page holds stays.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct MovePage;

impl Command for MovePage {
    type Params = MovePageParams;
    type Output = Paged;
    fn meta(&self) -> &'static CommandMeta {
        &MOVE
    }
    fn enabled(&self, s: &Session<'_>, p: &MovePageParams) -> Result<(), CmdError> {
        own_pages(s, s.doc_id(p.doc)?)
    }
    fn plan(&self, s: &mut Session<'_>, p: MovePageParams) -> Result<Plan<Paged>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let d = s.document_mut(Some(doc))?;
        let pages = d.page_count();
        page_of(pages, p.from)?;
        page_of(pages, p.to)?;
        d.move_page(p.from, p.to);
        let page = d.active_artboard;
        Ok(Plan::Done(Paged { doc, page, pages }))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RenamePageParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The page: 0 is the first.
    #[schemars(range(min = 0, max = 100000))]
    pub page: usize,
    /// Its name; empty gives it back its number ("Trang N").
    #[schemars(length(max = 200))]
    pub name: String,
}
impl Validate for RenamePageParams {}

static RENAME: CommandMeta = CommandMeta {
    id: "page.rename",
    label_vi: "Đổi tên trang",
    label_en: "Rename Page",
    doc: "Give a page a name of its own on the page bar; an empty name gives it back its number.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct RenamePage;

impl Command for RenamePage {
    type Params = RenamePageParams;
    type Output = Paged;
    fn meta(&self) -> &'static CommandMeta {
        &RENAME
    }
    fn enabled(&self, s: &Session<'_>, p: &RenamePageParams) -> Result<(), CmdError> {
        own_pages(s, s.doc_id(p.doc)?)
    }
    fn plan(&self, s: &mut Session<'_>, p: RenamePageParams) -> Result<Plan<Paged>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let d = s.document_mut(Some(doc))?;
        let pages = d.page_count();
        page_of(pages, p.page)?;
        let name = p.name.trim().to_string();
        let numbered = format!("Trang {}", p.page.saturating_add(1));
        let custom = (!name.is_empty() && name != numbered).then_some(name);
        d.set_page_name(p.page, custom);
        let page = d.active_artboard;
        Ok(Plan::Done(Paged { doc, page, pages }))
    }
}

/// What `page.master` does with the shared master page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MasterOp {
    /// Make it (from the shown page) if there is none, and edit it.
    Edit,
    /// Finish editing it: the page shown before is back.
    Done,
    /// Delete it.
    Delete,
    /// Show it beneath `page`, or not.
    Show,
    Hide,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MasterParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    pub op: MasterOp,
    /// For show / hide: the page (0 is the first).
    #[schemars(range(min = 0, max = 100000))]
    pub page: Option<usize>,
}
impl Validate for MasterParams {
    fn validate(&self) -> Result<(), CmdError> {
        if matches!(self.op, MasterOp::Show | MasterOp::Hide) && self.page.is_none() {
            return Err(CmdError::bad_param("page", "which page, for show / hide"));
        }
        Ok(())
    }
}

static MASTER: CommandMeta = CommandMeta {
    id: "page.master",
    label_vi: "Trang nền",
    label_en: "Master Page",
    doc: "The shared master page drawn beneath every page that shows it (Page ▸ Master): edit \
          it (made from the shown page the first time), finish editing, delete it, or show / \
          hide it beneath one page.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct Master;

impl Command for Master {
    type Params = MasterParams;
    type Output = Paged;
    fn meta(&self) -> &'static CommandMeta {
        &MASTER
    }
    fn enabled(&self, s: &Session<'_>, p: &MasterParams) -> Result<(), CmdError> {
        own_pages(s, s.doc_id(p.doc)?)
    }
    fn plan(&self, s: &mut Session<'_>, p: MasterParams) -> Result<Plan<Paged>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let d = s.document_mut(Some(doc))?;
        let pages = d.page_count();
        // Checking the master out or in changes the canvas shown; the rest
        // only changes what is drawn beneath the page shown.
        let editing = d.editing_master;
        let (vi, en, shown) = match p.op {
            MasterOp::Edit => {
                d.ensure_master();
                d.enter_master_edit();
                (
                    "Đang chỉnh TRANG NỀN (hiện dưới mọi trang) — bấm lại để xong",
                    "Editing the master page",
                    true,
                )
            }
            MasterOp::Done => {
                d.exit_master_edit();
                ("Đã xong trang nền", "Master page done", editing)
            }
            MasterOp::Delete => {
                if !d.has_master() {
                    return Err(not_enabled(
                        "the document has no master page",
                        "Tài liệu chưa có trang nền",
                    ));
                }
                d.remove_master();
                ("Đã xoá trang nền", "Master page deleted", editing)
            }
            MasterOp::Show | MasterOp::Hide => {
                let page = p.page.unwrap_or_default();
                page_of(pages, page)?;
                d.set_page_use_master(page, p.op == MasterOp::Show);
                if page != d.active_artboard || editing {
                    let page = d.active_artboard;
                    return Ok(Plan::Done(Paged { doc, page, pages }));
                }
                ("", "", false)
            }
        };
        let page = d.active_artboard;
        s.emit(if shown {
            EngineEvent::PageShown { doc }
        } else {
            EngineEvent::Structure { doc }
        });
        if !vi.is_empty() {
            s.notice(vi, en);
        }
        Ok(Plan::Done(Paged { doc, page, pages }))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SetupParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The bleed drawn round the page, in mm (0 for none).
    #[schemars(range(min = 0.0, max = 100.0))]
    pub bleed_mm: Option<f32>,
    /// The safe margin drawn inside the page, in mm (0 for none).
    #[schemars(range(min = 0.0, max = 100.0))]
    pub margin_mm: Option<f32>,
}
impl Validate for SetupParams {
    fn validate(&self) -> Result<(), CmdError> {
        if self.bleed_mm.is_none() && self.margin_mm.is_none() {
            return Err(CmdError::bad_param("(params)", "bleedMm or marginMm"));
        }
        Ok(())
    }
}

static SETUP: CommandMeta = CommandMeta {
    id: "page.setup",
    label_vi: "Lề trang",
    label_en: "Page Setup",
    doc: "The page's bleed and safe margin in mm, drawn as guides on the page (Page ▸ Bleed, \
          Safe margin). One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct Setup;

impl Command for Setup {
    type Params = SetupParams;
    type Output = Paged;
    fn meta(&self) -> &'static CommandMeta {
        &SETUP
    }
    fn plan(&self, s: &mut Session<'_>, p: SetupParams) -> Result<Plan<Paged>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let (page, pages) = {
            let d = s.document(Some(doc))?;
            (d.active_artboard, d.page_count())
        };
        let canvas = s.canvas_mut(Some(doc))?;
        let dpi = canvas.metadata.resolution_ppi.max(1.0);
        let px = |mm: f32| mm.max(0.0) / 25.4 * dpi;
        let (bleed, margin, artboards) = {
            let m = &canvas.metadata;
            (m.page_bleed_px, m.page_margin_px, m.artboards.clone())
        };
        let bleed_px = p.bleed_mm.map_or(bleed, px);
        let margin_px = p.margin_mm.map_or(margin, px);
        if (bleed_px - bleed).abs() >= 1e-3 || (margin_px - margin).abs() >= 1e-3 {
            canvas
                .execute(
                    Box::new(iai_history::command::PageSetupCommand::new(
                        "Page setup",
                        bleed_px,
                        margin_px,
                        artboards,
                    )),
                    iai_history::gateway::ChangeKind::LayerStructure,
                )
                .map_err(|e| crate::CmdError::internal(e.message))?;
            s.emit(EngineEvent::Structure { doc });
            let line = format!(
                "Trang: bleed {:.0}mm · lề an toàn {:.0}mm",
                bleed_px / dpi * 25.4,
                margin_px / dpi * 25.4
            );
            s.notice(line.clone(), line);
        }
        Ok(Plan::Done(Paged { doc, page, pages }))
    }
}

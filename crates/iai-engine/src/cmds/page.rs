//! Pages of a document: add one, show one.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{not_enabled, DocParams};
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

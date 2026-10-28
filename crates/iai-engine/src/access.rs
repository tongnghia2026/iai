//! How a command finds what its params name: documents and layers by id,
//! the shown one when a param is left out.

use iai_canvas::canvas::Canvas;
use iai_canvas::document::{Document, DocumentId};

use crate::events::EngineEvent;
use crate::session::{Session, no_document, no_such_document};
use crate::{CmdError, ErrorCode};

impl Session<'_> {
    /// The id of `doc`, or of the document shown when `None`.
    pub fn doc_id(&self, doc: Option<u32>) -> Result<u32, CmdError> {
        match doc {
            Some(d) if self.find(d).is_some() => Ok(d),
            Some(d) => Err(no_such_document(d)),
            None => self.active_id().ok_or_else(no_document),
        }
    }

    pub fn document(&self, doc: Option<u32>) -> Result<&Document, CmdError> {
        let id = self.doc_id(doc)?;
        self.find(id).ok_or_else(|| no_such_document(id))
    }

    pub fn document_mut(&mut self, doc: Option<u32>) -> Result<&mut Document, CmdError> {
        let id = self.doc_id(doc)?;
        self.ws
            .documents_mut()
            .iter_mut()
            .find(|d| d.id.0 == id)
            .ok_or_else(|| no_such_document(id))
    }

    /// The page shown of `doc`: what layer and selection commands work on.
    pub fn canvas(&self, doc: Option<u32>) -> Result<&Canvas, CmdError> {
        let d = self.document(doc)?;
        editable(d)?;
        Ok(&d.canvas)
    }

    pub fn canvas_mut(&mut self, doc: Option<u32>) -> Result<&mut Canvas, CmdError> {
        let d = self.document_mut(doc)?;
        editable(d)?;
        Ok(&mut d.canvas)
    }

    /// A new document id.
    pub fn new_document_id(&mut self) -> DocumentId {
        self.ws.new_document_id()
    }

    /// Open `doc` as a new tab, shown; its id.
    pub fn add_document(&mut self, doc: Document) -> u32 {
        let id = doc.id.0;
        self.ws.add(doc);
        self.emit(EngineEvent::Opened { doc: id });
        id
    }

    /// Add `doc` as a new tab behind the one shown; its id.
    pub fn add_document_behind(&mut self, doc: Document) -> u32 {
        let id = doc.id.0;
        self.ws.add_behind(doc);
        self.emit(EngineEvent::Opened { doc: id });
        id
    }

    /// Close document `doc`; the id of the one shown after.
    pub fn close_document(&mut self, doc: u32) -> Result<Option<u32>, CmdError> {
        let index = self.index_of(doc).ok_or_else(|| no_such_document(doc))?;
        self.ws.remove(index);
        let shown = self.active_id();
        if let Some(shown) = shown {
            self.emit(EngineEvent::Shown { doc: shown });
            self.emit(EngineEvent::Structure { doc: shown });
        }
        Ok(shown)
    }

    /// Show document `doc`.
    pub fn show(&mut self, doc: u32) -> Result<(), CmdError> {
        let index = self.index_of(doc).ok_or_else(|| no_such_document(doc))?;
        if self.ws.active() != Some(index) {
            self.ws.set_active(index);
            self.emit(EngineEvent::Shown { doc });
        }
        Ok(())
    }
}

/// Whether the shown page of `doc` holds pixels a command can work on.
fn editable(doc: &Document) -> Result<(), CmdError> {
    if doc.is_flow_text() {
        return Err(CmdError::new(
            ErrorCode::NotEnabled,
            "this is a text document; it has no layers",
        )
        .vi("Đây là tài liệu văn bản, không có layer"));
    }
    if doc.deferred_raw {
        return Err(CmdError::new(
            ErrorCode::NotEnabled,
            "this RAW photo is not loaded at full size; show it first",
        )
        .vi("Ảnh RAW này chưa nạp đủ, hãy mở nó ra trước")
        .with_hint(serde_json::json!({"command": "doc.show", "args": {"doc": doc.id.0}})));
    }
    Ok(())
}

/// The index in `canvas`'s stack of layer `id`, or of the active layer.
pub fn layer_index(canvas: &Canvas, layer: Option<u32>) -> Result<usize, CmdError> {
    let stack = &canvas.layer_stack;
    match layer {
        Some(id) => stack.layers.iter().position(|l| l.id == id).ok_or_else(|| {
            CmdError::new(ErrorCode::NotFound, format!("no layer {id}"))
                .vi(format!("Không có layer {id}"))
        }),
        None if stack.active_idx < stack.layers.len() => Ok(stack.active_idx),
        None => Err(CmdError::new(ErrorCode::NotFound, "no active layer").vi("Chưa chọn layer")),
    }
}

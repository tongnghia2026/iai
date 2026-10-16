//! Where the open documents live. The GUI keeps its tabs in the app and
//! lends them to the engine for each call; without a window (scripts, tests,
//! an agent working on files) [`HeadlessWorkspace`] holds them.

use iai_canvas::document::{Document, DocumentId};

/// The open documents, as the engine sees them.
pub trait Workspace {
    fn documents(&self) -> &[Document];
    fn documents_mut(&mut self) -> &mut [Document];
    /// The document shown (the active tab); `None` when none is open.
    fn active(&self) -> Option<usize>;
    /// Show the document at `index`.
    fn set_active(&mut self, index: usize);
    /// A new id, never used before in this session.
    fn new_document_id(&mut self) -> DocumentId;
    /// Add `doc` as a new tab and show it; its index.
    fn add(&mut self, doc: Document) -> usize;
    /// Close the document at `index` without asking about unsaved work.
    fn remove(&mut self, index: usize) -> Option<Document>;
}

/// The documents of a session without a window.
#[derive(Default)]
pub struct HeadlessWorkspace {
    docs: Vec<Document>,
    active: Option<usize>,
    next_id: u32,
}

impl HeadlessWorkspace {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Workspace for HeadlessWorkspace {
    fn documents(&self) -> &[Document] {
        &self.docs
    }

    fn documents_mut(&mut self) -> &mut [Document] {
        &mut self.docs
    }

    fn active(&self) -> Option<usize> {
        self.active.filter(|&i| i < self.docs.len())
    }

    fn set_active(&mut self, index: usize) {
        if index < self.docs.len() {
            self.active = Some(index);
        }
    }

    fn new_document_id(&mut self) -> DocumentId {
        self.next_id = self.next_id.saturating_add(1);
        DocumentId(self.next_id)
    }

    fn add(&mut self, doc: Document) -> usize {
        self.docs.push(doc);
        let index = self.docs.len().saturating_sub(1);
        self.active = Some(index);
        index
    }

    fn remove(&mut self, index: usize) -> Option<Document> {
        if index >= self.docs.len() {
            return None;
        }
        let doc = self.docs.remove(index);
        self.active = match self.active {
            _ if self.docs.is_empty() => None,
            Some(a) if a > index => Some(a.saturating_sub(1)),
            Some(a) => Some(a.min(self.docs.len().saturating_sub(1))),
            None => None,
        };
        Some(doc)
    }
}

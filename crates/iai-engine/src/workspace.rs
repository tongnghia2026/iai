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
    /// Add `doc` as a new tab behind the one shown; its index.
    fn add_behind(&mut self, doc: Document) -> usize {
        let shown = self.active();
        let index = self.add(doc);
        if let Some(shown) = shown
            && shown != index
        {
            self.set_active(shown);
        }
        index
    }
}

/// The open documents, the one shown, and the order they were last used in:
/// the window's tabs, and a session's without a window (scripts, tests, an
/// agent working on files). The engine owns this type; the window keeps one
/// and lends it, with what only a window has, for each call.
pub struct Tabs {
    pub documents: Vec<Document>,
    /// Index of the document shown. In the window it always indexes into
    /// `documents` (the window never runs without a tab); without a window
    /// it means nothing while `documents` is empty.
    pub active_doc_idx: usize,
    /// Document ids, most recently used first: closing the tab shown returns
    /// to the front-most one left rather than to its neighbour.
    pub doc_mru: Vec<DocumentId>,
    /// The id the next new document gets.
    pub next_doc_id: u32,
    /// The window's rule: closing the last tab leaves a blank page.
    keeps_one: bool,
}

/// The documents of a session without a window.
pub type HeadlessWorkspace = Tabs;

impl Tabs {
    /// No document yet: a session without a window.
    pub fn new() -> Self {
        Self {
            documents: Vec::new(),
            active_doc_idx: 0,
            doc_mru: Vec::new(),
            next_doc_id: 1,
            keeps_one: false,
        }
    }

    /// The window's tabs: one blank page of `width` x `height`, which stays
    /// when every other tab is closed.
    pub fn window(width: u32, height: u32) -> Self {
        let first = DocumentId(1);
        Self {
            documents: vec![Document::new(first, width, height)],
            active_doc_idx: 0,
            doc_mru: vec![first],
            next_doc_id: 2,
            keeps_one: true,
        }
    }

    /// The tab of the document with this id, while it is still open.
    pub fn index_of(&self, id: DocumentId) -> Option<usize> {
        self.documents.iter().position(|d| d.id == id)
    }

    /// The document with this id, while it is still open.
    pub fn by_id(&self, id: DocumentId) -> Option<&Document> {
        self.documents.iter().find(|d| d.id == id)
    }

    /// The document with this id, to change it, while it is still open.
    pub fn by_id_mut(&mut self, id: DocumentId) -> Option<&mut Document> {
        self.documents.iter_mut().find(|d| d.id == id)
    }
}

impl Default for Tabs {
    fn default() -> Self {
        Self::new()
    }
}

impl Workspace for Tabs {
    fn documents(&self) -> &[Document] {
        &self.documents
    }

    fn documents_mut(&mut self) -> &mut [Document] {
        &mut self.documents
    }

    fn active(&self) -> Option<usize> {
        Some(self.active_doc_idx).filter(|&i| i < self.documents.len())
    }

    fn set_active(&mut self, index: usize) {
        if index < self.documents.len() {
            self.active_doc_idx = index;
        }
    }

    fn new_document_id(&mut self) -> DocumentId {
        let id = DocumentId(self.next_doc_id);
        self.next_doc_id = self.next_doc_id.saturating_add(1);
        id
    }

    fn add(&mut self, doc: Document) -> usize {
        self.documents.push(doc);
        self.active_doc_idx = self.documents.len().saturating_sub(1);
        self.active_doc_idx
    }

    fn remove(&mut self, index: usize) -> Option<Document> {
        if index >= self.documents.len() {
            return None;
        }
        let doc = if self.keeps_one && self.documents.len() == 1 {
            let id = self.new_document_id();
            let blank = Document::new(
                id,
                iai_canvas::canvas::DEFAULT_W,
                iai_canvas::canvas::DEFAULT_H,
            );
            std::mem::replace(self.documents.get_mut(index)?, blank)
        } else {
            self.documents.remove(index)
        };
        let len = self.documents.len();
        if self.active_doc_idx > index || self.active_doc_idx >= len {
            self.active_doc_idx = self
                .active_doc_idx
                .saturating_sub(1)
                .min(len.saturating_sub(1));
        }
        Some(doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_keeps_a_tab_and_a_session_without_one_does_not() {
        let mut window = Tabs::window(40, 30);
        let first = window.documents[0].id;
        assert!(window.remove(0).is_some());
        assert_eq!(window.documents.len(), 1);
        assert_ne!(window.documents[0].id, first);
        assert_eq!(window.active(), Some(0));

        let mut headless = Tabs::new();
        assert_eq!(headless.active(), None);
        let id = headless.new_document_id();
        assert_eq!(id, DocumentId(1));
        headless.add(Document::new(id, 10, 10));
        let id = headless.new_document_id();
        headless.add(Document::new(id, 10, 10));
        assert_eq!(headless.active(), Some(1));
        headless.remove(1);
        assert_eq!(headless.active(), Some(0));
        headless.remove(0);
        assert!(headless.documents.is_empty());
        assert_eq!(headless.active(), None);
    }
}

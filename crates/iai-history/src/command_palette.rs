//! The undo step that edits a document's palette.

use crate::command::{Command, EditContext};
use iai_doc::palette::{DocumentSwatch, validate_palette};

/// Replace the complete small palette. At the documented 256-entry ceiling this
/// is both simpler and cheaper than a family of index commands, and makes undo
/// robust against add/remove/rename reordering.
pub struct ReplaceDocumentPalette {
    new_palette: Vec<DocumentSwatch>,
    old_palette: Option<Vec<DocumentSwatch>>,
}

impl ReplaceDocumentPalette {
    pub fn new(new_palette: Vec<DocumentSwatch>) -> Self {
        Self {
            new_palette,
            old_palette: None,
        }
    }
}

impl Command for ReplaceDocumentPalette {
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        validate_palette(&self.new_palette)?;
        let metadata = ctx
            .metadata
            .as_deref_mut()
            .ok_or("document metadata unavailable")?;
        self.old_palette = Some(metadata.swatches.clone());
        metadata.swatches = self.new_palette.clone();
        Ok(())
    }

    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        let old = self.old_palette.as_ref().ok_or("nothing to undo")?;
        let metadata = ctx
            .metadata
            .as_deref_mut()
            .ok_or("document metadata unavailable")?;
        metadata.swatches = old.clone();
        Ok(())
    }

    fn label(&self) -> &str {
        "Edit Document Palette"
    }

    fn memory_bytes(&self) -> usize {
        let bytes = |palette: &[DocumentSwatch]| {
            palette
                .iter()
                .map(|swatch| std::mem::size_of::<DocumentSwatch>() + swatch.name.capacity())
                .sum::<usize>()
        };
        bytes(&self.new_palette) + self.old_palette.as_deref().map_or(0, bytes)
    }
}

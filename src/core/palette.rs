//! Document-owned process-colour swatches.
//!
//! The palette belongs to the canvas/document rather than the UI session so it
//! survives `.iai` round-trips and follows a document when tabs are switched.
//! Mutations use [`ReplaceDocumentPalette`], keeping add/remove/rename as one
//! normal undo step through the canvas gateway.

use crate::core::command::{Command, EditContext};
use crate::core::vector::color::ColorValue;

pub const MAX_DOCUMENT_SWATCHES: usize = 256;
pub const MAX_SWATCH_NAME_BYTES: usize = 96;

#[derive(Debug, Clone, PartialEq)]
pub struct DocumentSwatch {
    pub name: String,
    pub color: ColorValue,
}

impl DocumentSwatch {
    pub fn new(name: impl Into<String>, color: ColorValue) -> Self {
        Self {
            name: name.into(),
            color,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("swatch name cannot be empty".into());
        }
        if name.len() > MAX_SWATCH_NAME_BYTES {
            return Err(format!("swatch name exceeds {MAX_SWATCH_NAME_BYTES} bytes"));
        }
        self.color.validate()
    }
}

pub fn validate_palette(swatches: &[DocumentSwatch]) -> Result<(), String> {
    if swatches.len() > MAX_DOCUMENT_SWATCHES {
        return Err(format!(
            "document palette exceeds {MAX_DOCUMENT_SWATCHES} swatches"
        ));
    }
    for swatch in swatches {
        swatch.validate()?;
    }
    Ok(())
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::Canvas;
    use crate::core::gateway::ChangeKind;

    #[test]
    fn palette_command_roundtrips_through_undo_redo() {
        let mut canvas = Canvas::new(20, 20);
        let red = DocumentSwatch::new("Process Red", ColorValue::rgb(1.0, 0.0, 0.0));
        canvas
            .execute(
                Box::new(ReplaceDocumentPalette::new(vec![red.clone()])),
                ChangeKind::DocumentMetadata,
            )
            .unwrap();
        assert_eq!(canvas.metadata.swatches, vec![red.clone()]);
        assert!(canvas.undo().is_some());
        assert!(canvas.metadata.swatches.is_empty());
        assert!(canvas.redo().is_some());
        assert_eq!(canvas.metadata.swatches, vec![red]);
    }

    #[test]
    fn palette_validation_rejects_empty_and_non_finite_entries() {
        assert!(DocumentSwatch::new("", ColorValue::BLACK)
            .validate()
            .is_err());
        assert!(
            DocumentSwatch::new("Bad", ColorValue::rgb(f32::NAN, 0.0, 0.0))
                .validate()
                .is_err()
        );
    }
}

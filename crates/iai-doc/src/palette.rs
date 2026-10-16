//! Document-owned process-colour swatches.
//!
//! The palette belongs to the canvas/document rather than the UI session so it
//! survives `.iai` round-trips and follows a document when tabs are switched.
//! Mutations use [`ReplaceDocumentPalette`], keeping add/remove/rename as one
//! normal undo step through the canvas gateway.

use crate::vector::color::ColorValue;

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

#[cfg(test)]
mod tests {
    use super::*;

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

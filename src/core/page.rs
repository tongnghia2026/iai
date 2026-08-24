#![allow(dead_code)]
//! Page / Artboard identity (foundation contract #10, Mục 3.11 / 3.13).
//!
//! At MVP there is exactly ONE implicit page, [`PageId::IMPLICIT`], whose
//! descriptor equals the single canvas (origin `(0,0)`, size = canvas, no
//! bleed/margin). Every [`crate::core::layer::Layer`] carries a `page_id`, so
//! multi-page / Artboard support later is a purely ADDITIVE extension: the
//! coordinate model is already page-relative abstract `f32`
//! (see [`crate::core::vector::object`]), so no object is re-homed when real pages
//! arrive. The multi-page CONTAINER (a `pages` list on the document + its `.iai`
//! envelope) is the one reserved additive slot — see `docs/ADR_PAGE_OWNERSHIP.md`.
//!
//! Ownership lives on the Layer, not on `VectorObjectData`: a page groups layers
//! of every kind (raster/text/shape/path), so page membership must be uniform
//! across layer types rather than vector-only.

use crate::core::geometry::{Point, Rect};

/// Which page / artboard a layer belongs to. An opaque id like `Layer::id`;
/// [`PageId::IMPLICIT`] (`0`) is the single MVP page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PageId(pub u32);

impl PageId {
    /// The one page that exists before multi-page is built. `Default` for `PageId`
    /// resolves to this, so a defaulted layer lands on the implicit page.
    pub const IMPLICIT: PageId = PageId(0);
}

/// A page / artboard placed in document-space. `origin + size` make it serve BOTH
/// Affinity-style Artboards (a region of a larger document) and Publisher-style
/// pages (an independently rendered canvas). Bleed / margin / background are print
/// intents. This is a reserved contract type: the container that holds many pages
/// is added additively when the feature is built.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub id: PageId,
    /// Top-left of the page in document-space.
    pub origin: Point,
    /// `(width, height)` in document units.
    pub size: (f32, f32),
    /// Uniform bleed in document units (`0` at MVP). Per-side bleed can be added
    /// additively if a job ever needs asymmetric bleed.
    pub bleed: f32,
    /// Uniform safety margin in document units (`0` at MVP).
    pub margin: f32,
    /// Straight sRGB page background, or `None` for transparent / paper.
    pub background: Option<[u8; 4]>,
}

impl Page {
    /// The single MVP page: covers the whole canvas at the document origin with no
    /// bleed / margin and no explicit background. page-space == canvas-space, so
    /// nothing about today's one-canvas behaviour changes.
    pub fn implicit(width: u32, height: u32) -> Self {
        Self {
            id: PageId::IMPLICIT,
            origin: Point::new(0.0, 0.0),
            size: (width as f32, height as f32),
            bleed: 0.0,
            margin: 0.0,
            background: None,
        }
    }

    /// The page rectangle in document-space (`origin + size`).
    pub fn rect(&self) -> Rect {
        Rect::new(self.origin.x, self.origin.y, self.size.0, self.size.1)
    }

    /// Reject non-finite or negative geometry so a page from an untrusted file
    /// can't poison layout maths.
    pub fn validate(&self) -> Result<(), String> {
        let finite = self.origin.x.is_finite()
            && self.origin.y.is_finite()
            && self.size.0.is_finite()
            && self.size.1.is_finite()
            && self.bleed.is_finite()
            && self.margin.is_finite();
        if !finite {
            return Err("page has non-finite geometry".into());
        }
        if self.size.0 < 0.0 || self.size.1 < 0.0 || self.bleed < 0.0 || self.margin < 0.0 {
            return Err("page size / bleed / margin must be >= 0".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_page_id_is_the_implicit_page() {
        assert_eq!(PageId::default(), PageId::IMPLICIT);
        assert_eq!(PageId::IMPLICIT.0, 0);
    }

    #[test]
    fn implicit_page_matches_the_canvas() {
        let p = Page::implicit(1200, 800);
        assert_eq!(p.id, PageId::IMPLICIT);
        assert_eq!(p.rect(), Rect::new(0.0, 0.0, 1200.0, 800.0));
        assert_eq!((p.bleed, p.margin), (0.0, 0.0));
        assert!(p.background.is_none());
        assert!(p.validate().is_ok());
    }

    #[test]
    fn validate_rejects_bad_geometry() {
        let mut p = Page::implicit(100, 100);
        p.bleed = -1.0;
        assert!(p.validate().is_err());
        let mut p = Page::implicit(100, 100);
        p.size.0 = f32::NAN;
        assert!(p.validate().is_err());
    }
}

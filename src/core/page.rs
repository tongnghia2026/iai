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

/// Workspace gap between artboards, as a fraction of the reference page width,
/// with a floor so a tiny page still gets a visible separation.
fn row_gap(reference_w: u32) -> f32 {
    (reference_w as f32 * 0.06).max(24.0)
}

/// Plan an artboard appended to the RIGHT of `existing` (same size as the last
/// page, one [`row_gap`] apart). When `existing` is empty, the single implicit
/// page equal to the canvas is materialised first — so a plain one-page document
/// becomes a real two-artboard job. Returns `(all artboards including the new
/// one, new canvas width, new canvas height)`, the canvas grown just enough to
/// enclose every trim rect (artboards are regions of one shared canvas). New
/// pages inherit the current default `bleed` / `margin`.
pub fn append_artboard_in_row(
    existing: &[Page],
    canvas_w: u32,
    canvas_h: u32,
    bleed: f32,
    margin: f32,
) -> (Vec<Page>, u32, u32) {
    let mut boards: Vec<Page> = if existing.is_empty() {
        let mut p = Page::implicit(canvas_w, canvas_h);
        p.bleed = bleed.max(0.0);
        p.margin = margin.max(0.0);
        vec![p]
    } else {
        existing.to_vec()
    };
    let last = boards
        .last()
        .expect("non-empty after the implicit fallback");
    let new_size = last.size;
    let right_edge = boards
        .iter()
        .map(|b| b.origin.x + b.size.0)
        .fold(0.0_f32, f32::max);
    let new_id = PageId(
        boards
            .iter()
            .map(|b| b.id.0)
            .max()
            .unwrap_or(0)
            .wrapping_add(1),
    );
    boards.push(Page {
        id: new_id,
        origin: Point::new(right_edge + row_gap(canvas_w), 0.0),
        size: new_size,
        bleed: bleed.max(0.0),
        margin: margin.max(0.0),
        background: None,
    });
    let max_x = boards
        .iter()
        .map(|b| b.origin.x + b.size.0)
        .fold(0.0_f32, f32::max);
    let max_y = boards
        .iter()
        .map(|b| b.origin.y + b.size.1)
        .fold(0.0_f32, f32::max);
    let w2 = (canvas_w as f32).max(max_x).ceil().max(1.0) as u32;
    let h2 = (canvas_h as f32).max(max_y).ceil().max(1.0) as u32;
    (boards, w2, h2)
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

    #[test]
    fn append_materialises_the_implicit_page_then_adds_a_second() {
        let (boards, w2, h2) = append_artboard_in_row(&[], 100, 80, 4.0, 3.0);
        assert_eq!(boards.len(), 2);
        assert_eq!(boards[0].id, PageId::IMPLICIT);
        assert_eq!(boards[0].rect(), Rect::new(0.0, 0.0, 100.0, 80.0));
        assert_eq!((boards[0].bleed, boards[0].margin), (4.0, 3.0));
        assert!(
            boards[1].origin.x >= 100.0,
            "the new page sits to the right of the first"
        );
        assert_eq!(boards[1].size, (100.0, 80.0));
        assert_eq!((boards[1].bleed, boards[1].margin), (4.0, 3.0));
        assert!(w2 > 100, "canvas grew to hold both artboards");
        assert_eq!(h2, 80, "same-height row keeps the height");
        // Every page must enclose within the new canvas.
        assert!(boards[1].origin.x + boards[1].size.0 <= w2 as f32 + 0.5);
    }

    #[test]
    fn append_extends_an_existing_row() {
        let existing = vec![
            Page::implicit(50, 50),
            Page {
                id: PageId(1),
                origin: Point::new(70.0, 0.0),
                size: (50.0, 50.0),
                bleed: 0.0,
                margin: 0.0,
                background: None,
            },
        ];
        let (boards, w2, _) = append_artboard_in_row(&existing, 120, 50, 0.0, 0.0);
        assert_eq!(boards.len(), 3);
        assert_eq!(boards[2].id, PageId(2), "id is max + 1");
        assert!(
            boards[2].origin.x >= 120.0,
            "appended past the rightmost page"
        );
        assert!(w2 as f32 >= boards[2].origin.x + 50.0 - 0.5);
    }
}

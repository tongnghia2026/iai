#![allow(dead_code)]
//! Vector fill/outline style (foundation task T2.2).
//!
//! Corel's model treats Fill and Outline as two independent properties (Mục 2.2 /
//! 2.3); this mirrors that. Code says `stroke` where the UI says "Outline
//! (Stroke)" — one documented mapping, no ambiguity. Overprint is a separate
//! flag on each of fill and outline (Mục 3.5). Gradient/pattern paints and dash
//! arrays are deliberately absent here — they arrive as ADDITIVE enum variants /
//! fields in Phase 6 without touching this contract (Mục 3.11).
//!
//! Fill *rule* (NonZero/EvenOdd) is geometry, so it stays on `PathData`
//! (path.rs), not here.

use crate::core::vector::color::ColorValue;

/// How a stroke's ends are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineCap {
    #[default]
    Butt,
    Round,
    Square,
}

/// How a stroke's corners are joined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

/// What paints a fill or an outline. `None` is Corel's explicit "no fill / no
/// outline" — distinct from a fully transparent colour.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Paint {
    #[default]
    None,
    Solid(ColorValue),
    // Gradient(..) / Pattern(..): Phase 6 — additive variants, no migration.
}

impl Paint {
    pub fn is_visible(self) -> bool {
        match self {
            Paint::None => false,
            Paint::Solid(c) => c.alpha() > 0.0,
        }
    }

    pub fn color(self) -> Option<ColorValue> {
        match self {
            Paint::None => None,
            Paint::Solid(c) => Some(c),
        }
    }
}

/// Outline geometry (Corel "Outline"): width plus cap/join/miter. Dash is added
/// later. `width` is in object-local units; `Paint::None` — not `width == 0` — is
/// the canonical "no outline" signal, but a zero width is tolerated and treated
/// as invisible.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrokeStyle {
    pub width: f32,
    pub cap: LineCap,
    pub join: LineJoin,
    pub miter_limit: f32,
}

impl Default for StrokeStyle {
    fn default() -> Self {
        Self {
            width: 1.0,
            cap: LineCap::default(),
            join: LineJoin::default(),
            miter_limit: 4.0,
        }
    }
}

/// The complete appearance of a vector object: fill, outline and object opacity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VectorStyle {
    pub fill: Paint,
    /// Overprint the fill on separations (leave underlying inks, don't knock out).
    pub fill_overprint: bool,
    pub stroke: Paint,
    pub stroke_style: StrokeStyle,
    /// Overprint the outline, independent of the fill.
    pub stroke_overprint: bool,
    /// Object opacity in `[0,1]`; multiplies each paint's own alpha at raster time.
    pub opacity: f32,
}

impl Default for VectorStyle {
    /// A freshly-drawn path is a solid black fill with no outline — the common
    /// case for sign/advert artwork, and always visible.
    fn default() -> Self {
        Self {
            fill: Paint::Solid(ColorValue::BLACK),
            fill_overprint: false,
            stroke: Paint::None,
            stroke_style: StrokeStyle::default(),
            stroke_overprint: false,
            opacity: 1.0,
        }
    }
}

impl VectorStyle {
    /// Solid fill, no outline.
    pub fn filled(color: ColorValue) -> Self {
        Self {
            fill: Paint::Solid(color),
            stroke: Paint::None,
            ..Self::default()
        }
    }

    /// Outline only, no fill.
    pub fn stroked(color: ColorValue, width: f32) -> Self {
        Self {
            fill: Paint::None,
            stroke: Paint::Solid(color),
            stroke_style: StrokeStyle {
                width,
                ..StrokeStyle::default()
            },
            ..Self::default()
        }
    }

    /// Effective outline width, or 0 when there is no visible outline.
    pub fn effective_stroke_width(self) -> f32 {
        if self.stroke.is_visible() {
            self.stroke_style.width.max(0.0)
        } else {
            0.0
        }
    }

    /// Opacity in range, stroke width/miter finite, and both paints valid.
    pub fn validate(self) -> Result<(), String> {
        if !self.opacity.is_finite() || !(0.0..=1.0).contains(&self.opacity) {
            return Err(format!("opacity {} out of [0,1]", self.opacity));
        }
        if !self.stroke_style.width.is_finite() || self.stroke_style.width < 0.0 {
            return Err("stroke width must be finite and >= 0".into());
        }
        if !self.stroke_style.miter_limit.is_finite() || self.stroke_style.miter_limit < 1.0 {
            return Err("miter limit must be finite and >= 1".into());
        }
        for p in [self.fill, self.stroke] {
            if let Paint::Solid(c) = p {
                c.validate()?;
            }
        }
        Ok(())
    }

    /// Adapter from the legacy raster [`crate::core::shape::ShapeData`] fields, so
    /// Shape can migrate onto the shared style WITHOUT a big-bang rewrite (Bước 2
    /// DoD). Takes the raw fields rather than `&ShapeData` to keep the vector core
    /// free of any dependency on the shape module. The current mapping: a fill is
    /// visible only when `fill` is set and the colour is not fully transparent; an
    /// outline is visible only when width > 0 and its colour is not transparent.
    pub fn from_shape_fields(
        fill: bool,
        fill_color: [u8; 4],
        stroke_width: f32,
        stroke_color: [u8; 4],
    ) -> Self {
        let fill_paint = if fill && fill_color[3] > 0 {
            Paint::Solid(ColorValue::from_rgba8(fill_color))
        } else {
            Paint::None
        };
        let stroke_paint = if stroke_width > 0.0 && stroke_color[3] > 0 {
            Paint::Solid(ColorValue::from_rgba8(stroke_color))
        } else {
            Paint::None
        };
        Self {
            fill: fill_paint,
            fill_overprint: false,
            stroke: stroke_paint,
            stroke_style: StrokeStyle {
                width: stroke_width.max(0.0),
                ..StrokeStyle::default()
            },
            stroke_overprint: false,
            opacity: 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_visible_black_fill() {
        let s = VectorStyle::default();
        assert!(s.fill.is_visible());
        assert!(!s.stroke.is_visible());
        assert_eq!(s.opacity, 1.0);
        assert_eq!(s.stroke_style.miter_limit, 4.0);
        assert!(s.validate().is_ok());
    }

    #[test]
    fn constructors_and_effective_width() {
        let f = VectorStyle::filled(ColorValue::WHITE);
        assert!(f.fill.is_visible() && !f.stroke.is_visible());
        assert_eq!(f.effective_stroke_width(), 0.0);

        let s = VectorStyle::stroked(ColorValue::BLACK, 3.0);
        assert!(!s.fill.is_visible() && s.stroke.is_visible());
        assert_eq!(s.effective_stroke_width(), 3.0);
    }

    #[test]
    fn shape_adapter_maps_fill_and_outline() {
        // Filled red, 2px blue outline.
        let s = VectorStyle::from_shape_fields(true, [255, 0, 0, 255], 2.0, [0, 0, 255, 255]);
        assert_eq!(
            s.fill.color(),
            Some(ColorValue::from_rgba8([255, 0, 0, 255]))
        );
        assert_eq!(s.effective_stroke_width(), 2.0);

        // No fill, no outline (zero width).
        let none = VectorStyle::from_shape_fields(false, [0, 0, 0, 0], 0.0, [0, 0, 0, 0]);
        assert!(!none.fill.is_visible());
        assert!(!none.stroke.is_visible());

        // Transparent fill colour counts as no fill even when the flag is set.
        let ghost = VectorStyle::from_shape_fields(true, [10, 20, 30, 0], 0.0, [0, 0, 0, 0]);
        assert!(!ghost.fill.is_visible());
    }

    #[test]
    fn validate_catches_bad_values() {
        let mut s = VectorStyle::default();
        s.opacity = 1.5;
        assert!(s.validate().is_err());

        let mut s = VectorStyle::default();
        s.stroke_style.width = f32::NAN;
        assert!(s.validate().is_err());

        let mut s = VectorStyle::default();
        s.stroke_style.miter_limit = 0.5;
        assert!(s.validate().is_err());
    }
}

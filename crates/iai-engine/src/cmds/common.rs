//! Param and output types several commands share.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{CmdError, ErrorCode};

/// A rectangle in document pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RectPx {
    #[schemars(range(min = -100000, max = 100000))]
    pub x: i32,
    #[schemars(range(min = -100000, max = 100000))]
    pub y: i32,
    #[schemars(range(min = 1, max = 100000))]
    pub w: u32,
    #[schemars(range(min = 1, max = 100000))]
    pub h: u32,
}

impl RectPx {
    /// The part of it inside a `width × height` page, as `(x0, y0, x1, y1)`;
    /// `None` when nothing of it is inside.
    pub fn clipped(&self, width: u32, height: u32) -> Option<(u32, u32, u32, u32)> {
        let x0 = i64::from(self.x).clamp(0, i64::from(width));
        let y0 = i64::from(self.y).clamp(0, i64::from(height));
        let x1 = i64::from(self.x)
            .saturating_add(i64::from(self.w))
            .clamp(0, i64::from(width));
        let y1 = i64::from(self.y)
            .saturating_add(i64::from(self.h))
            .clamp(0, i64::from(height));
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some((
            u32::try_from(x0).ok()?,
            u32::try_from(y0).ok()?,
            u32::try_from(x1).ok()?,
            u32::try_from(y1).ok()?,
        ))
    }
}

/// Where something lies, in document pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Bounds {
    /// From `(x0, y0, x1, y1)` as the selection reports it.
    pub fn from_corners(x0: f32, y0: f32, x1: f32, y1: f32) -> Option<Self> {
        let w = (x1 - x0).round();
        let h = (y1 - y0).round();
        if !(w >= 1.0 && h >= 1.0) {
            return None;
        }
        Some(Self {
            x: x0.round() as i32,
            y: y0.round() as i32,
            w: w as u32,
            h: h as u32,
        })
    }
}

/// `"#RRGGBB"` or `"#RRGGBBAA"` as RGBA.
pub fn parse_color(field: &str, text: &str) -> Result<[u8; 4], CmdError> {
    let bad = || {
        CmdError::bad_param(field, "a colour as \"#RRGGBB\" or \"#RRGGBBAA\"")
            .vi(format!("Màu `{field}` phải có dạng #RRGGBB"))
    };
    let hex = text.strip_prefix('#').ok_or_else(bad)?;
    if !(hex.len() == 6 || hex.len() == 8) || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(bad());
    }
    let byte = |i: usize| {
        hex.get(i..i.saturating_add(2))
            .and_then(|s| u8::from_str_radix(s, 16).ok())
            .ok_or_else(bad)
    };
    let alpha = if hex.len() == 8 { byte(6)? } else { 255 };
    Ok([byte(0)?, byte(2)?, byte(4)?, alpha])
}

/// RGBA as `"#RRGGBB"` (or `"#RRGGBBAA"` when not opaque).
pub fn color_text(rgba: [u8; 4]) -> String {
    let [r, g, b, a] = rgba;
    if a == 255 {
        format!("#{r:02X}{g:02X}{b:02X}")
    } else {
        format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
    }
}

/// How a new selection combines with the one there is.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SelectMode {
    /// It replaces the selection (default).
    #[default]
    Replace,
    Add,
    Subtract,
    Intersect,
}

impl SelectMode {
    pub fn to_core(self) -> iai_doc::selection::SelectionMode {
        use iai_doc::selection::SelectionMode as M;
        match self {
            SelectMode::Replace => M::New,
            SelectMode::Add => M::Add,
            SelectMode::Subtract => M::Subtract,
            SelectMode::Intersect => M::Intersect,
        }
    }
}

/// A layer's blend mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Blend {
    Normal,
    Dissolve,
    Multiply,
    ColorBurn,
    Darken,
    Screen,
    ColorDodge,
    Lighten,
    Overlay,
    SoftLight,
    HardLight,
    LinearLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl Blend {
    pub fn to_core(self) -> iai_raster::blend::BlendMode {
        use iai_raster::blend::BlendMode as B;
        match self {
            Blend::Normal => B::Normal,
            Blend::Dissolve => B::Dissolve,
            Blend::Multiply => B::Multiply,
            Blend::ColorBurn => B::ColorBurn,
            Blend::Darken => B::Darken,
            Blend::Screen => B::Screen,
            Blend::ColorDodge => B::ColorDodge,
            Blend::Lighten => B::Lighten,
            Blend::Overlay => B::Overlay,
            Blend::SoftLight => B::SoftLight,
            Blend::HardLight => B::HardLight,
            Blend::LinearLight => B::LinearLight,
            Blend::Difference => B::Difference,
            Blend::Exclusion => B::Exclusion,
            Blend::Hue => B::Hue,
            Blend::Saturation => B::Saturation,
            Blend::Color => B::Color,
            Blend::Luminosity => B::Luminosity,
        }
    }

    pub fn from_core(mode: iai_raster::blend::BlendMode) -> Self {
        use iai_raster::blend::BlendMode as B;
        match mode {
            B::Normal => Blend::Normal,
            B::Dissolve => Blend::Dissolve,
            B::Multiply => Blend::Multiply,
            B::ColorBurn => Blend::ColorBurn,
            B::Darken => Blend::Darken,
            B::Screen => Blend::Screen,
            B::ColorDodge => Blend::ColorDodge,
            B::Lighten => Blend::Lighten,
            B::Overlay => Blend::Overlay,
            B::SoftLight => Blend::SoftLight,
            B::HardLight => Blend::HardLight,
            B::LinearLight => Blend::LinearLight,
            B::Difference => Blend::Difference,
            B::Exclusion => Blend::Exclusion,
            B::Hue => Blend::Hue,
            B::Saturation => Blend::Saturation,
            B::Color => Blend::Color,
            B::Luminosity => Blend::Luminosity,
        }
    }
}

/// A command that has nothing to do on this document.
pub fn not_enabled(en: impl Into<String>, vi: impl Into<String>) -> CmdError {
    CmdError::new(ErrorCode::NotEnabled, en).vi(vi)
}

/// Params of commands that only name a document.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DocParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
}

impl crate::Validate for DocParams {}

/// What commands that change a document answer, when there is nothing more
/// to say.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Changed {
    pub doc: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_read_and_write_back() {
        assert_eq!(parse_color("c", "#0090FF").unwrap(), [0, 0x90, 0xFF, 255]);
        assert_eq!(
            parse_color("c", "#0090ff80").unwrap(),
            [0, 0x90, 0xFF, 0x80]
        );
        assert_eq!(color_text([0, 0x90, 0xFF, 255]), "#0090FF");
        assert_eq!(color_text([1, 2, 3, 4]), "#01020304");
        for bad in ["0090FF", "#0090F", "#0090FG", "#", "", "#ééé"] {
            assert_eq!(
                parse_color("c", bad).unwrap_err().code,
                ErrorCode::BadParams,
                "{bad}"
            );
        }
    }

    #[test]
    fn a_rectangle_is_clipped_to_the_page() {
        let r = RectPx {
            x: -5,
            y: 2,
            w: 10,
            h: 100,
        };
        assert_eq!(r.clipped(8, 8), Some((0, 2, 5, 8)));
        let out = RectPx {
            x: 20,
            y: 0,
            w: 4,
            h: 4,
        };
        assert_eq!(out.clipped(8, 8), None);
    }
}

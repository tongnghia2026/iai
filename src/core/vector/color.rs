#![allow(dead_code)]
//! Vector-object colour (foundation task T2.1).
//!
//! A single [`ColorValue`] shared by every vector fill/outline. It is an enum so
//! print-specific kinds can be ADDED later (a `Spot` variant with swatch
//! identity, Mục 3.5) WITHOUT migrating existing objects — the plan forbids both
//! an "RGBA-only style that pretends to support CMYK" and a schema that forces a
//! full migration when spot/overprint arrive (Mục 3.5 / 3.12 / 8).
//!
//! Conversions deliberately reuse the document colour pipeline in [`crate::core::cms`]
//! rather than growing a second RGB↔CMYK path (a named piece of debt to avoid):
//! the naive built-in transform for a profile-less preview, and a [`CmykConverter`]
//! for the document's ICC profile when producing ink planes. CMYK ink is stored
//! and emitted verbatim (never round-tripped through RGB) so pure-black / rich-black
//! intent survives to separations.

use crate::core::cms::{naive_cmyk_to_rgb, CmykConverter};

/// Quantise a `[0,1]` float channel to 8-bit with rounding.
#[inline]
fn q(x: f32) -> u8 {
    (x.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Maximum bytes stored for a spot colorant name. Keeping the name inline (not a
/// heap `String`) is what lets [`ColorValue`] stay `Copy`, which the whole vector
/// pipeline relies on. Real ink names (Pantone etc.) fit comfortably; longer
/// names are truncated on a char boundary.
pub const SPOT_NAME_CAP: usize = 31;

/// A fixed-capacity, `Copy` colorant name for a spot ink. It is the identity of
/// the separation plate the ink prints on.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SpotName {
    bytes: [u8; SPOT_NAME_CAP],
    len: u8,
}

impl SpotName {
    /// Build from a string, trimming and truncating to [`SPOT_NAME_CAP`] bytes on
    /// a UTF-8 char boundary.
    pub fn new(name: &str) -> Self {
        let trimmed = name.trim();
        let mut end = trimmed.len().min(SPOT_NAME_CAP);
        while end > 0 && !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        let src = &trimmed.as_bytes()[..end];
        let mut bytes = [0u8; SPOT_NAME_CAP];
        bytes[..src.len()].copy_from_slice(src);
        Self {
            bytes,
            len: src.len() as u8,
        }
    }

    pub fn as_str(&self) -> &str {
        // Constructed only from valid UTF-8 slices, so this never fails.
        std::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("")
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl std::fmt::Debug for SpotName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

/// A colour at the vector-object level. `a` is the paint's own alpha; object
/// opacity (which multiplies this) lives separately on `VectorStyle`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColorValue {
    /// Device / sRGB, channels and alpha in `[0,1]`.
    Rgb { r: f32, g: f32, b: f32, a: f32 },
    /// Process CMYK ink fractions in `[0,1]` under the DOCUMENT's CMYK profile.
    /// `k` is explicit (not derived from CMY) so pure/rich black is preserved.
    Cmyk {
        c: f32,
        m: f32,
        y: f32,
        k: f32,
        a: f32,
    },
    /// A named spot ink printed on its OWN separation plate. `tint` is the ink
    /// coverage in `[0,1]` (a screen percentage); `alt` is the process-CMYK bytes
    /// that approximate the ink at full strength for on-screen preview and for
    /// flattening when the spot can't be kept as a plate. The name is the plate
    /// identity carried into the PDF `Separation` colour space.
    Spot {
        name: SpotName,
        tint: f32,
        alt: [u8; 4],
        a: f32,
    },
}

impl Default for ColorValue {
    fn default() -> Self {
        ColorValue::BLACK
    }
}

impl ColorValue {
    pub const BLACK: ColorValue = ColorValue::Rgb {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };
    pub const WHITE: ColorValue = ColorValue::Rgb {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    };
    pub const TRANSPARENT: ColorValue = ColorValue::Rgb {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };

    #[inline]
    pub fn rgb(r: f32, g: f32, b: f32) -> Self {
        ColorValue::Rgb { r, g, b, a: 1.0 }
    }
    #[inline]
    pub fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        ColorValue::Rgb { r, g, b, a }
    }
    #[inline]
    pub fn cmyk(c: f32, m: f32, y: f32, k: f32) -> Self {
        ColorValue::Cmyk { c, m, y, k, a: 1.0 }
    }
    #[inline]
    pub fn cmyka(c: f32, m: f32, y: f32, k: f32, a: f32) -> Self {
        ColorValue::Cmyk { c, m, y, k, a }
    }

    /// A spot ink: a named plate at `tint` coverage, with a process-CMYK bytes
    /// approximation for preview / flattening.
    #[inline]
    pub fn spot(name: &str, alt_cmyk: [u8; 4], tint: f32) -> Self {
        ColorValue::Spot {
            name: SpotName::new(name),
            tint: tint.clamp(0.0, 1.0),
            alt: alt_cmyk,
            a: 1.0,
        }
    }

    pub fn is_spot(self) -> bool {
        matches!(self, ColorValue::Spot { .. })
    }

    pub fn spot_name(self) -> Option<SpotName> {
        match self {
            ColorValue::Spot { name, .. } => Some(name),
            _ => None,
        }
    }

    pub fn spot_tint(self) -> Option<f32> {
        match self {
            ColorValue::Spot { tint, .. } => Some(tint),
            _ => None,
        }
    }

    /// From an existing 8-bit sRGB colour (the format the raster pipeline uses).
    #[inline]
    pub fn from_rgba8(rgba: [u8; 4]) -> Self {
        ColorValue::Rgb {
            r: rgba[0] as f32 / 255.0,
            g: rgba[1] as f32 / 255.0,
            b: rgba[2] as f32 / 255.0,
            a: rgba[3] as f32 / 255.0,
        }
    }

    #[inline]
    pub fn alpha(self) -> f32 {
        match self {
            ColorValue::Rgb { a, .. } | ColorValue::Cmyk { a, .. } | ColorValue::Spot { a, .. } => {
                a
            }
        }
    }

    #[must_use]
    pub fn with_alpha(self, a: f32) -> Self {
        match self {
            ColorValue::Rgb { r, g, b, .. } => ColorValue::Rgb { r, g, b, a },
            ColorValue::Cmyk { c, m, y, k, .. } => ColorValue::Cmyk { c, m, y, k, a },
            ColorValue::Spot {
                name, tint, alt, ..
            } => ColorValue::Spot { name, tint, alt, a },
        }
    }

    pub fn is_cmyk(self) -> bool {
        matches!(self, ColorValue::Cmyk { .. })
    }

    /// Every channel is finite and within `[0,1]`.
    pub fn validate(self) -> Result<(), String> {
        // RGB pads its 4th slot with the (already-listed) alpha so both arms are
        // 5 valid channels — no sentinel value to reason about. A spot's `alt` is
        // byte data (always valid), so only its tint and alpha are checked here.
        let chans = match self {
            ColorValue::Rgb { r, g, b, a } => [r, g, b, a, a],
            ColorValue::Cmyk { c, m, y, k, a } => [c, m, y, k, a],
            ColorValue::Spot { tint, a, .. } => [tint, a, a, a, a],
        };
        for v in chans {
            if !v.is_finite() {
                return Err("non-finite colour channel".into());
            }
            if !(0.0..=1.0001).contains(&v) {
                return Err(format!("colour channel {v} out of [0,1]"));
            }
        }
        if let ColorValue::Spot { name, .. } = self {
            if name.is_empty() {
                return Err("spot colour needs a plate name".into());
            }
        }
        Ok(())
    }

    // ── Profile-less preview (display / RGB mirror) ──────────────────────────

    /// Preview RGBA for display and the sRGB mirror of a CMYK document. CMYK uses
    /// the profile-less naive transform; the accurate mirror for a CMYK doc goes
    /// through [`ColorValue::to_rgba8_profiled`].
    pub fn to_rgba8(self) -> [u8; 4] {
        match self {
            ColorValue::Rgb { r, g, b, a } => [q(r), q(g), q(b), q(a)],
            ColorValue::Cmyk { c, m, y, k, a } => {
                let rgb = naive_cmyk_to_rgb([q(c), q(m), q(y), q(k)]);
                [rgb[0], rgb[1], rgb[2], q(a)]
            }
            ColorValue::Spot { tint, alt, a, .. } => {
                let rgb = naive_cmyk_to_rgb(scale_ink(alt, tint));
                [rgb[0], rgb[1], rgb[2], q(a)]
            }
        }
    }

    /// Straight-alpha RGBA as floats in `[0,1]` (naive preview for CMYK).
    pub fn to_rgb_f32(self) -> (f32, f32, f32, f32) {
        let [r, g, b, a] = self.to_rgba8();
        (
            r as f32 / 255.0,
            g as f32 / 255.0,
            b as f32 / 255.0,
            a as f32 / 255.0,
        )
    }

    // ── Profile-aware (document ICC / ink planes) ────────────────────────────

    /// CMYK ink bytes for this colour under the document converter. A CMYK value
    /// is emitted verbatim (K preserved); an RGB value is separated via `conv`.
    pub fn to_cmyk8(self, conv: &CmykConverter) -> [u8; 4] {
        match self {
            ColorValue::Cmyk { c, m, y, k, .. } => [q(c), q(m), q(y), q(k)],
            ColorValue::Rgb { r, g, b, .. } => conv.rgb_to_cmyk_one([q(r), q(g), q(b)]),
            // A spot flattened to process is its CMYK approximation at this tint.
            ColorValue::Spot { tint, alt, .. } => scale_ink(alt, tint),
        }
    }

    /// Accurate display RGBA under the document converter (CMYK routed through the
    /// ICC profile instead of the naive transform); alpha is carried through.
    pub fn to_rgba8_profiled(self, conv: &CmykConverter) -> [u8; 4] {
        match self {
            ColorValue::Rgb { r, g, b, a } => [q(r), q(g), q(b), q(a)],
            ColorValue::Cmyk { c, m, y, k, a } => {
                let rgb = conv.cmyk_to_rgb_one([q(c), q(m), q(y), q(k)]);
                [rgb[0], rgb[1], rgb[2], q(a)]
            }
            ColorValue::Spot { tint, alt, a, .. } => {
                let rgb = conv.cmyk_to_rgb_one(scale_ink(alt, tint));
                [rgb[0], rgb[1], rgb[2], q(a)]
            }
        }
    }
}

/// Scale process-ink bytes by a `[0,1]` tint (a spot screen percentage): full
/// tint keeps the ink, zero tint is bare paper.
#[inline]
fn scale_ink(alt: [u8; 4], tint: f32) -> [u8; 4] {
    let t = tint.clamp(0.0, 1.0);
    [
        (alt[0] as f32 * t).round() as u8,
        (alt[1] as f32 * t).round() as u8,
        (alt[2] as f32 * t).round() as u8,
        (alt[3] as f32 * t).round() as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::cms::naive_rgb_to_cmyk;

    #[test]
    fn rgb_roundtrips_through_bytes() {
        let c = ColorValue::from_rgba8([12, 200, 255, 128]);
        assert_eq!(c.to_rgba8(), [12, 200, 255, 128]);
    }

    #[test]
    fn cmyk_ink_is_emitted_verbatim() {
        let c = ColorValue::cmyk(0.1, 0.2, 0.3, 0.4);
        // Verbatim ink, not round-tripped through RGB.
        assert_eq!(
            c.to_cmyk8(&CmykConverter::Naive),
            [q(0.1), q(0.2), q(0.3), q(0.4)]
        );
    }

    #[test]
    fn pure_black_stays_pure_k() {
        // Rich-black avoidance: K-only in stays K-only out (no CMY contamination).
        let k = ColorValue::cmyk(0.0, 0.0, 0.0, 1.0);
        assert_eq!(k.to_cmyk8(&CmykConverter::Naive), [0, 0, 0, 255]);
        assert_eq!(k.to_rgba8(), [0, 0, 0, 255]);
    }

    #[test]
    fn rgb_separates_via_converter_single_source() {
        // The vector colour must reuse cms's converter, not a private formula.
        let red = ColorValue::rgb(1.0, 0.0, 0.0);
        assert_eq!(
            red.to_cmyk8(&CmykConverter::Naive),
            naive_rgb_to_cmyk([255, 0, 0])
        );
    }

    #[test]
    fn cmyk_preview_matches_naive_decode() {
        let c = ColorValue::cmyk(0.0, 1.0, 1.0, 0.0); // red-ish
        let rgb = naive_cmyk_to_rgb([0, 255, 255, 0]);
        assert_eq!(c.to_rgba8(), [rgb[0], rgb[1], rgb[2], 255]);
    }

    #[test]
    fn validate_rejects_bad_channels() {
        assert!(ColorValue::rgb(0.5, 0.5, 0.5).validate().is_ok());
        assert!(ColorValue::rgba(f32::NAN, 0.0, 0.0, 1.0)
            .validate()
            .is_err());
        assert!(ColorValue::rgba(2.0, 0.0, 0.0, 1.0).validate().is_err());
        assert!(ColorValue::cmyk(0.0, 0.0, 0.0, 1.0).validate().is_ok());
    }

    #[test]
    fn with_alpha_and_helpers() {
        let c = ColorValue::WHITE.with_alpha(0.5);
        assert_eq!(c.alpha(), 0.5);
        assert!(!c.is_cmyk());
        assert!(ColorValue::cmyk(0.0, 0.0, 0.0, 1.0).is_cmyk());
    }

    #[test]
    fn spot_name_truncates_on_char_boundary_and_trims() {
        assert_eq!(SpotName::new("  PANTONE 185 C  ").as_str(), "PANTONE 185 C");
        // Multi-byte tail is cut cleanly, never mid-codepoint.
        let long = "é".repeat(40); // 2 bytes each
        let name = SpotName::new(&long);
        assert!(name.as_str().len() <= SPOT_NAME_CAP);
        assert!(std::str::from_utf8(name.as_str().as_bytes()).is_ok());
    }

    #[test]
    fn spot_previews_via_its_process_alternate_scaled_by_tint() {
        // Full-tint magenta-ish spot (alt = M+Y ink).
        let full = ColorValue::spot("PANTONE Warm Red C", [0, 255, 255, 0], 1.0);
        assert!(full.is_spot() && !full.is_cmyk());
        assert_eq!(
            full.spot_name().map(|n| n.as_str().to_string()).as_deref(),
            Some("PANTONE Warm Red C")
        );
        assert_eq!(full.to_cmyk8(&CmykConverter::Naive), [0, 255, 255, 0]);
        assert_eq!(full.to_rgba8(), {
            let rgb = naive_cmyk_to_rgb([0, 255, 255, 0]);
            [rgb[0], rgb[1], rgb[2], 255]
        });

        // Half tint halves the ink coverage.
        let half = ColorValue::spot("PANTONE Warm Red C", [0, 255, 255, 0], 0.5);
        assert_eq!(half.to_cmyk8(&CmykConverter::Naive), [0, 128, 128, 0]);
    }

    #[test]
    fn spot_validates_tint_and_requires_a_name() {
        assert!(ColorValue::spot("Silver", [0, 0, 0, 40], 1.0)
            .validate()
            .is_ok());
        // Empty plate name is rejected.
        let nameless = ColorValue::Spot {
            name: SpotName::new(""),
            tint: 1.0,
            alt: [0, 0, 0, 0],
            a: 1.0,
        };
        assert!(nameless.validate().is_err());
    }
}

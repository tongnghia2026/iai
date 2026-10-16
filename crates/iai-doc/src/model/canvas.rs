//! What a canvas page is, as data: its metadata, colour-mode tags, the dirty
//! rectangle and the size limits of a flat pixel buffer. Below the layers,
//! the history and the canvas, which all use it.

/// Above this many pixels a canvas keeps no flat composited buffer.
pub const LARGE_CANVAS_PIXELS: u64 = 25_000_000;

#[inline]
pub fn pixel_count(width: u32, height: u32) -> Option<u64> {
    (width as u64).checked_mul(height as u64)
}

/// Bytes of an RGBA8 buffer of this size, `None` when it overflows.
#[inline]
pub fn checked_rgba_len(width: u32, height: u32) -> Option<usize> {
    pixel_count(width, height)?
        .checked_mul(4)
        .and_then(|n| usize::try_from(n).ok())
}

/// Whether a canvas of this size keeps a flat composited buffer.
#[inline]
pub fn fits_flat_buffer(width: u32, height: u32) -> bool {
    pixel_count(width, height).is_some_and(|n| n <= LARGE_CANVAS_PIXELS)
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CanvasMetadata {
    pub title: String,
    pub author: String,
    pub description: String,
    pub tags: Vec<String>,
    pub resolution_ppi: f32,
    /// Name of the ICC profile an imported file was converted *from* into the
    /// sRGB working space (empty when the source was untagged or already sRGB).
    /// Display-only — shown in the Info panel.
    pub source_profile: String,
    /// Scene-linear Develop storage primaries. Older documents default to the
    /// legacy linear-sRGB compatibility path.
    pub develop_working_space: iai_color::working_color::WorkingColorSpace,
    /// Version of the scene color-pipeline contract stored by `.iai`.
    pub color_pipeline_version: u16,
    /// Decode-time RAW look that produced the baked document pixels. `None` for
    /// non-RAW documents and legacy `.iai` files that predate this provenance.
    /// The scene master itself remains transient; this tag prevents saved work
    /// from losing which versioned input recipe created it.
    pub raw_render_recipe: Option<RawRenderRecipeVersion>,
    /// Named process colours owned by this document. Kept out of global UI
    /// preferences so each `.iai` project carries its production palette.
    pub swatches: Vec<crate::palette::DocumentSwatch>,
    /// Artboards (print pages) placed in document space — the multi-page
    /// container reserved by `docs/ADR_PAGE_OWNERSHIP.md`. EMPTY means a single
    /// implicit artboard equal to the canvas (page-space == canvas-space), so a
    /// one-page document stores nothing and round-trips byte-identically. Homed on
    /// the canvas metadata (not `Document`) so it rides the existing `.iai` canvas
    /// serialisation and, later, the canvas undo / dirty gate.
    pub artboards: Vec<crate::page::Page>,
    /// Default page bleed, in document units (canvas pixels), applied to the single
    /// implicit page. `0` = none. Kept on the canvas so it follows a resize and
    /// persists; explicit artboards carry their own bleed instead.
    pub page_bleed_px: f32,
    /// Default page safe-margin, in document units (canvas pixels), applied to the
    /// implicit page. `0` = none.
    pub page_margin_px: f32,
    /// Custom name shown on this page's tab in a multi-page (artboard) document.
    /// `None` (or blank) falls back to the positional "Trang N" label. Rides on the
    /// canvas so it travels with the page through reorder / swap and persists with
    /// the rest of the canvas metadata.
    pub page_name: Option<String>,
    /// Whether this page shows the document's shared master (background) page
    /// beneath its own layers. `true` by default so a new page inherits the
    /// master; a page can opt out. Only meaningful in a multi-page document that
    /// has a master — see [`crate::core::document::Document::master`]. Persisted
    /// only when `false`, so default pages stay byte-identical.
    pub use_master: bool,
}

impl Default for CanvasMetadata {
    fn default() -> Self {
        Self {
            title: "Untitled".to_string(),
            author: String::new(),
            description: String::new(),
            tags: Vec::new(),
            resolution_ppi: 72.0,
            source_profile: String::new(),
            develop_working_space: iai_color::working_color::WorkingColorSpace::LinearSrgb,
            color_pipeline_version: 1,
            raw_render_recipe: None,
            swatches: Vec::new(),
            artboards: Vec::new(),
            page_bleed_px: 0.0,
            page_margin_px: 0.0,
            page_name: None,
            use_master: true,
        }
    }
}

/// Version of the decode-time RAW master recipe.
///
/// `LegacyBaked1` is the exact look that shipped before the Q1 technical/creative
/// split. `TechnicalNeutral2` is an opt-in audit recipe which removes the
/// global colour-NR, capture-sharpen, brightness, warmth, and chroma taste
/// stages. Keeping this in provenance makes A/B output explainable instead of
/// depending on an unrecorded collection of environment overrides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawRenderRecipeVersion {
    LegacyBaked1,
    TechnicalNeutral2,
    /// Shipping default from Q2/Q4: the legacy baked look plus the band-passed
    /// capture-sharpen ("nét") and, when no camera profile resolves, the
    /// preview-safe embedded-JPEG colour fit (matrix + bounded exposure/chroma,
    /// no RGB histogram curves) instead of the full curve fit.
    NaturalV3,
}

impl RawRenderRecipeVersion {
    pub const fn name(self) -> &'static str {
        match self {
            Self::LegacyBaked1 => "legacy-baked-v1",
            Self::TechnicalNeutral2 => "technical-neutral-v2",
            Self::NaturalV3 => "natural-v3",
        }
    }

    /// Parse the stable manifest/provenance spelling. Unknown future recipes
    /// stay `None` so older builds can open the baked pixels without silently
    /// claiming a different RAW decode contract.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "legacy-baked-v1" => Some(Self::LegacyBaked1),
            "technical-neutral-v2" => Some(Self::TechnicalNeutral2),
            "natural-v3" => Some(Self::NaturalV3),
            _ => None,
        }
    }
}

/// Document colour-space tag. Working/display space is `SRGB`; the other
/// variants are convert/export targets handled through [`crate::core::cms`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[allow(dead_code)]
pub enum ColorSpace {
    #[default]
    SRGB,
    LinearRGB,
    AdobeRGB,
    DisplayP3,
    ProPhoto,
}

/// The document's assigned ICC profile. `data` empty means untagged (treated as
/// the sRGB working space); when present it is the raw ICC bytes embedded on
/// export. Populated on import by [`crate::core::cms`] / the format importers.
#[derive(Debug, Clone, Default)]
pub struct IccProfile {
    pub name: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[allow(dead_code)]
pub enum BitDepth {
    #[default]
    Eight,
    Sixteen,
}

/// Document colour mode. In `Rgb` (the default) the tiles' RGBA bytes are the
/// ground truth. In `Cmyk` every layer tile carries CMYK8 ink planes as ground
/// truth and its RGBA bytes are the profile projection of that ink (display /
/// composite mirror — alpha stays the real layer alpha).
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ColorMode {
    #[default]
    Rgb,
    Cmyk(CmykProfile),
}

/// The ink↔RGB conversion space of a CMYK document.
#[derive(Debug, Clone, PartialEq)]
pub enum CmykProfile {
    /// Built-in "Generic CMYK (naive)" max-K GCR: zero-setup and exactly
    /// invertible (RGB→CMYK→RGB lossless), but NOT a print-accurate rendering.
    Naive,
    /// ICC CMYK device profile; `data` = raw ICC bytes (embedded in .iai).
    Icc { name: String, data: Vec<u8> },
}

impl CmykProfile {
    pub fn display_name(&self) -> &str {
        match self {
            CmykProfile::Naive => "Generic CMYK (naive)",
            CmykProfile::Icc { name, .. } => name,
        }
    }

    /// Build the two-way converter for this space. `None` only for a corrupt /
    /// non-CMYK ICC payload.
    pub fn converter(&self) -> Option<iai_color::cms::CmykConverter> {
        match self {
            CmykProfile::Naive => Some(iai_color::cms::CmykConverter::Naive),
            CmykProfile::Icc { data, .. } => {
                iai_color::cms::CmykConverter::from_icc_bytes(data, iai_color::cms::DEFAULT_INTENT)
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DirtyRegion {
    pub active: bool,
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl DirtyRegion {
    pub fn expand(&mut self, x0: u32, y0: u32, x1: u32, y1: u32) {
        if !self.active {
            self.x0 = x0;
            self.y0 = y0;
            self.x1 = x1;
            self.y1 = y1;
            self.active = true;
        } else {
            if x0 < self.x0 {
                self.x0 = x0;
            }
            if y0 < self.y0 {
                self.y0 = y0;
            }
            if x1 > self.x1 {
                self.x1 = x1;
            }
            if y1 > self.y1 {
                self.y1 = y1;
            }
        }
    }

    pub fn expand_full(&mut self, w: u32, h: u32) {
        self.expand(0, 0, w, h);
    }

    pub fn clear(&mut self) {
        self.active = false;
    }

    pub fn to_rect(&self) -> Option<(u32, u32, u32, u32)> {
        if self.active {
            Some((self.x0, self.y0, self.x1, self.y1))
        } else {
            None
        }
    }

    /// Convert canvas-pixel dirty region to a viewport scissor rect `(x, y, width, height)`.
    ///
    /// `offset_x / offset_y` — screen-pixel position of the canvas top-left corner
    ///   (i.e. `self.view.offset_x / offset_y`).
    /// `zoom` — current zoom factor (canvas pixel → screen pixel scale).
    ///
    /// Returns `None` when the region is inactive or entirely off-screen.
    /// The returned rect is already clamped to `[0, screen_w) × [0, screen_h)`.
    #[allow(dead_code)]
    pub fn as_screen_rect(
        &self,
        offset_x: f32,
        offset_y: f32,
        zoom: f32,
        screen_w: u32,
        screen_h: u32,
    ) -> Option<(u32, u32, u32, u32)> {
        if !self.active {
            return None;
        }
        let sx0 = (self.x0 as f32 * zoom + offset_x).floor().max(0.0) as u32;
        let sy0 = (self.y0 as f32 * zoom + offset_y).floor().max(0.0) as u32;
        let sx1 = (self.x1 as f32 * zoom + offset_x)
            .ceil()
            .min(screen_w as f32) as u32;
        let sy1 = (self.y1 as f32 * zoom + offset_y)
            .ceil()
            .min(screen_h as f32) as u32;
        if sx1 <= sx0 || sy1 <= sy0 {
            return None;
        }
        Some((sx0, sy0, sx1 - sx0, sy1 - sy0))
    }
}

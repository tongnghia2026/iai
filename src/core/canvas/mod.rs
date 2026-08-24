// Main document model — Canvas, History, metadata.
//

mod clip_ops;
mod clip_render;
mod color_mode;
mod geometry_ops;
mod history_gate;
mod layer_ops;
mod raster_ops;
mod selection_ops;

use history_gate::HistoryGate;

use super::gateway::{ChangeError, ChangeKind, ChangeOutcome};
use super::layer::{Layer, LayerStack};
pub use super::selection::Selection;

pub use super::layer::BlendMode;

pub const DEFAULT_W: u32 = 1920;
pub const DEFAULT_H: u32 = 1080;

/// Maximum width/height (in pixels) a canvas may reach. Enforced both on import
/// (guards against crafted or corrupt files declaring enormous dimensions that
/// would OOM the process during decode) and by the canvas resize/crop paths.
///
/// Lives in the raster core, not in `file_io`: the canvas itself is what upholds
/// the limit, and `core` must not depend on the I/O layer.
pub const MAX_DIMENSION: u32 = 30_000;

/// Where a selection stroke sits relative to the selection edge (like the standard
/// Edit ▸ Stroke "Location").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StrokeLocation {
    Inside,
    #[default]
    Center,
    Outside,
}

/// Parameters for stroking the active selection (Edit ▸ Stroke dialog).
#[derive(Debug, Clone, Copy)]
pub struct StrokeParams {
    /// Straight-alpha RGBA8 stroke color (alpha here is the color's own alpha).
    pub color: [u8; 4],
    /// Stroke width in pixels (>= 1).
    pub width: u32,
    pub location: StrokeLocation,
    /// Overall stroke opacity 0.0..=1.0, multiplied onto the color alpha.
    pub opacity: f32,
}

impl Default for StrokeParams {
    fn default() -> Self {
        Self {
            color: [0, 0, 0, 255],
            width: 1,
            location: StrokeLocation::Inside,
            opacity: 1.0,
        }
    }
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
    pub develop_working_space: crate::core::working_color::WorkingColorSpace,
    /// Version of the scene color-pipeline contract stored by `.iai`.
    pub color_pipeline_version: u16,
    /// Named process colours owned by this document. Kept out of global UI
    /// preferences so each `.iai` project carries its production palette.
    pub swatches: Vec<crate::core::palette::DocumentSwatch>,
    /// Artboards (print pages) placed in document space — the multi-page
    /// container reserved by `docs/ADR_PAGE_OWNERSHIP.md`. EMPTY means a single
    /// implicit artboard equal to the canvas (page-space == canvas-space), so a
    /// one-page document stores nothing and round-trips byte-identically. Homed on
    /// the canvas metadata (not `Document`) so it rides the existing `.iai` canvas
    /// serialisation and, later, the canvas undo / dirty gate.
    pub artboards: Vec<crate::core::page::Page>,
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
            develop_working_space: crate::core::working_color::WorkingColorSpace::LinearSrgb,
            color_pipeline_version: 1,
            swatches: Vec::new(),
            artboards: Vec::new(),
            page_bleed_px: 0.0,
            page_margin_px: 0.0,
            page_name: None,
        }
    }
}

impl Canvas {
    /// The artboards this canvas renders, never empty: the explicit
    /// [`CanvasMetadata::artboards`] when set, otherwise the single implicit
    /// artboard equal to the canvas (origin `(0,0)`, canvas size, no bleed /
    /// margin). Derived fresh, so the implicit artboard can never desync from a
    /// canvas resize.
    pub fn effective_artboards(&self) -> Vec<crate::core::page::Page> {
        if self.metadata.artboards.is_empty() {
            let mut page = crate::core::page::Page::implicit(self.width, self.height);
            page.bleed = self.metadata.page_bleed_px.max(0.0);
            page.margin = self.metadata.page_margin_px.max(0.0);
            vec![page]
        } else {
            self.metadata.artboards.clone()
        }
    }

    /// How many artboards the canvas has — always at least one (the implicit
    /// page). O(1); prefer over `effective_artboards().len()`.
    pub fn artboard_count(&self) -> usize {
        self.metadata.artboards.len().max(1)
    }

    /// Whether the canvas carries explicit artboards (a real multi-page job)
    /// rather than the single implicit page derived from its size.
    pub fn has_explicit_artboards(&self) -> bool {
        !self.metadata.artboards.is_empty()
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
    pub fn converter(&self) -> Option<crate::core::cms::CmykConverter> {
        match self {
            CmykProfile::Naive => Some(crate::core::cms::CmykConverter::Naive),
            CmykProfile::Icc { data, .. } => crate::core::cms::CmykConverter::from_icc_bytes(
                data,
                crate::core::cms::DEFAULT_INTENT,
            ),
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

pub struct Canvas {
    pub width: u32,
    pub height: u32,
    /// Composited pixel buffer (RGBA).
    /// May be stale when `pixels_stale = true` — call `ensure_pixels()` before reading.
    pub pixels: Vec<u8>,
    /// True when `pixels` needs a full CPU re-flatten (e.g. after undo/redo or layer op).
    /// The GPU compositor updates independently via tile atlas; `pixels` is only used by
    /// CPU tools (eyedropper, fill sample_merged) and export. Setting this flag defers
    /// the expensive flatten until those paths actually need it.
    pub pixels_stale: bool,
    pub layer_stack: LayerStack,
    pub layer_revision: u64,
    /// Selection.bounding_box() uses the cached bbox in selection.rs.
    pub selection: Selection,
    pub metadata: CanvasMetadata,
    pub color_space: ColorSpace,
    /// RGB (default) or CMYK-with-ink-planes. See [`ColorMode`].
    pub color_mode: ColorMode,
    #[allow(dead_code)]
    pub bit_depth: BitDepth,
    /// Document ICC profile (empty = untagged/sRGB). Set on import; embedded on
    /// export when "Embed Color Profile" is enabled.
    pub icc_profile: IccProfile,
    pub dirty: DirtyRegion,
    pub stroke_dirty: DirtyRegion,
    /// Phase 1: snapshot history. Phase 2: migrate sang CommandHistory.
    pub pending_stroke: Option<crate::core::command::DeltaSnapshot>,
    pub pending_stroke_name: String,
    /// The undo/redo history, sealed inside [`HistoryGate`] so it can only be
    /// appended to via the gateway. Every persistent change goes through
    /// [`record`](Self::record)/[`record_as`](Self::record_as)/[`execute`](Self::execute),
    /// so history, the saved checkpoint and invalidation cannot drift apart —
    /// and the compiler, not a convention, enforces it. See core::gateway.
    cmd_history: HistoryGate,
    /// Pre-computed Lab + Sobel cache for Smart Select tool.
    /// Lazily computed once per layer revision; invalidated when layer content changes.
    pub edge_cache: Option<Box<super::selection::EdgeCache>>,
    /// Linear scene-referred master from a RAW decode (unclamped f16). Present
    /// only while the document can still enter a scene-referred Develop
    /// session; dropped on Develop commit/cancel to free memory. Never
    /// serialized into .iai.
    pub develop_source: Option<std::sync::Arc<super::develop_scene::SceneSource>>,
    /// Channels panel state: write mask, viewed channel, saved alpha channels.
    pub channels: super::channels::ChannelsState,
    /// In-progress Brush/Eraser stroke into an alpha channel (view=Alpha);
    /// finalized into an AlphaPlanePaintCommand by `end_stroke`.
    pub pending_alpha_stroke: Option<super::channels::PendingAlphaStroke>,
    /// Region of the viewed alpha plane painted since the last GPU upload.
    pub plane_dirty: DirtyRegion,
    /// Combined fingerprint of PowerClip geometry (content offsets/sizes + frame
    /// shapes) at the last [`refresh_clip_masks`](Self::refresh_clip_masks) bake.
    /// Lets the per-event refit skip when nothing clip-relevant changed. Derived
    /// state; never serialized.
    pub clip_fp: u64,
}

/// Shortest distance from point `(px,py)` to segment `a–b`. Used by
/// `Canvas::stroke_polyline`.
fn dist_point_segment(px: f32, py: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (ax, ay) = a;
    let (bx, by) = b;
    let dx = bx - ax;
    let dy = by - ay;
    let len2 = dx * dx + dy * dy;
    let t = if len2 <= f32::EPSILON {
        0.0
    } else {
        (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
    };
    let cx = ax + t * dx;
    let cy = ay + t * dy;
    (px - cx).hypot(py - cy)
}

#[allow(dead_code)]
impl Canvas {
    pub const LARGE_CANVAS_PIXELS: u64 = 25_000_000;

    #[inline]
    pub fn pixel_count(width: u32, height: u32) -> Option<u64> {
        (width as u64).checked_mul(height as u64)
    }

    #[inline]
    pub fn checked_rgba_len(width: u32, height: u32) -> Option<usize> {
        Self::pixel_count(width, height)?
            .checked_mul(4)
            .and_then(|n| usize::try_from(n).ok())
    }

    #[inline]
    pub fn fits_flat_buffer(width: u32, height: u32) -> bool {
        Self::pixel_count(width, height).is_some_and(|n| n <= Self::LARGE_CANVAS_PIXELS)
    }

    #[inline]
    pub fn guarded_flat_rgba_len(width: u32, height: u32) -> Option<usize> {
        if Self::fits_flat_buffer(width, height) {
            Self::checked_rgba_len(width, height)
        } else {
            None
        }
    }

    pub fn new(width: u32, height: u32) -> Self {
        let layer_stack = LayerStack::new(width, height);
        let pixels = if Self::fits_flat_buffer(width, height) {
            layer_stack.flatten(width, height)
        } else {
            Vec::new()
        };
        let selection = Selection::new(width, height);
        Self {
            width,
            height,
            pixels,
            pixels_stale: false,
            layer_stack,
            layer_revision: 1,
            selection,
            metadata: CanvasMetadata::default(),
            color_space: ColorSpace::SRGB,
            color_mode: ColorMode::Rgb,
            bit_depth: BitDepth::Eight,
            icc_profile: IccProfile::default(),
            dirty: DirtyRegion::default(),
            stroke_dirty: DirtyRegion::default(),
            pending_stroke: None,
            pending_stroke_name: String::new(),
            cmd_history: HistoryGate::new(),
            edge_cache: None,
            develop_source: None,
            channels: super::channels::ChannelsState::default(),
            pending_alpha_stroke: None,
            plane_dirty: DirtyRegion::default(),
            clip_fp: 0,
        }
    }

    pub fn new_default() -> Self {
        Self::new(DEFAULT_W, DEFAULT_H)
    }

    pub fn new_blank(width: u32, height: u32) -> Self {
        Self::new(width, height)
    }

    pub fn from_rgba(pixels: Vec<u8>, width: u32, height: u32) -> Self {
        let has_transparency = pixels.chunks_exact(4).any(|pixel| pixel[3] < u8::MAX);
        let tiles = crate::core::tile::TileMap::from_rgba(&pixels, width, height);
        let flat_pixels = if Self::fits_flat_buffer(width, height) {
            pixels
        } else {
            Vec::new()
        };
        let mut layer_stack = LayerStack::new(width, height);
        layer_stack.layers[0].tiles = tiles;
        if has_transparency {
            let layer = &mut layer_stack.layers[0];
            layer.name = "Layer 1".to_string();
            layer.is_background = false;
            layer.locked = false;
        }

        let mut dirty = DirtyRegion::default();
        dirty.expand_full(width, height);
        Self {
            width,
            height,
            pixels: flat_pixels,
            pixels_stale: false,
            layer_stack,
            layer_revision: 1,
            selection: Selection::new(width, height),
            metadata: CanvasMetadata::default(),
            color_space: ColorSpace::SRGB,
            color_mode: ColorMode::Rgb,
            bit_depth: BitDepth::Eight,
            icc_profile: IccProfile::default(),
            dirty,
            stroke_dirty: DirtyRegion::default(),
            pending_stroke: None,
            pending_stroke_name: String::new(),
            cmd_history: HistoryGate::new(),
            edge_cache: None,
            develop_source: None,
            channels: super::channels::ChannelsState::default(),
            pending_alpha_stroke: None,
            plane_dirty: DirtyRegion::default(),
            clip_fp: 0,
        }
    }

    /// Build a 16-bit document from a 16-bit RGBA buffer (`width*height*4`
    /// samples). The background layer keeps the 16-bit master; `bit_depth` is set
    /// to `Sixteen`. Display/tools still read the 8-bit mirror.
    pub fn from_rgba16(px16: Vec<u16>, width: u32, height: u32) -> Self {
        let has_transparency = px16.chunks_exact(4).any(|pixel| pixel[3] < u16::MAX);
        let tiles = crate::core::tile::TileMap::from_rgba16(&px16, width, height);
        let flat_pixels = if Self::fits_flat_buffer(width, height) {
            px16.iter().map(|&v| (v >> 8) as u8).collect()
        } else {
            Vec::new()
        };
        let mut layer_stack = LayerStack::new(width, height);
        layer_stack.layers[0].tiles = tiles;
        if has_transparency {
            let layer = &mut layer_stack.layers[0];
            layer.name = "Layer 1".to_string();
            layer.is_background = false;
            layer.locked = false;
        }

        let mut dirty = DirtyRegion::default();
        dirty.expand_full(width, height);
        Self {
            width,
            height,
            pixels: flat_pixels,
            pixels_stale: false,
            layer_stack,
            layer_revision: 1,
            selection: Selection::new(width, height),
            metadata: CanvasMetadata::default(),
            color_space: ColorSpace::SRGB,
            color_mode: ColorMode::Rgb,
            bit_depth: BitDepth::Sixteen,
            icc_profile: IccProfile::default(),
            dirty,
            stroke_dirty: DirtyRegion::default(),
            pending_stroke: None,
            pending_stroke_name: String::new(),
            cmd_history: HistoryGate::new(),
            edge_cache: None,
            develop_source: None,
            channels: super::channels::ChannelsState::default(),
            pending_alpha_stroke: None,
            plane_dirty: DirtyRegion::default(),
            clip_fp: 0,
        }
    }

    pub fn begin_stroke(&mut self, action_name: &str) {
        if matches!(
            self.channels.view,
            crate::core::channels::ChannelView::Alpha(_)
        ) {
            if self.pending_stroke.is_some() || self.pending_alpha_stroke.is_some() {
                self.end_stroke();
            }
            self.pending_stroke_name = action_name.to_string();
            self.stroke_dirty.clear();
            return;
        }

        if self.layer_stack.layers.is_empty() {
            return;
        }
        // Finalize any stroke that was never closed (tool switched / focus lost
        // before the mouse-release fired). Without this, the new "before" snapshot
        // overwrites the orphaned one and that paint becomes un-undoable.
        if self.pending_stroke.is_some() || self.pending_alpha_stroke.is_some() {
            self.end_stroke();
        }
        self.layer_stack.normalize_active_idx();
        let layer = self.layer_stack.active_layer();
        let target = layer.paint_target;
        let has_mask = layer.mask.is_some();
        let snap = crate::core::command::DeltaSnapshot::capture_before(
            match target {
                crate::core::layer::PaintTarget::Pixels => &layer.tiles,
                crate::core::layer::PaintTarget::Mask => {
                    if has_mask {
                        &layer.mask.as_ref().unwrap().tiles
                    } else {
                        &layer.tiles
                    }
                }
            },
            layer.id,
            if target == crate::core::layer::PaintTarget::Mask && !has_mask {
                crate::core::layer::PaintTarget::Pixels
            } else {
                target
            },
        );
        self.pending_stroke = Some(snap);
        self.pending_stroke_name = action_name.to_string();
        self.stroke_dirty.clear();
    }

    /// Mark a changed pixel region — needs recomposite and GPU upload.
    /// Called by Tool.on_drag() after each dab.
    pub fn mark_dirty(&mut self, x0: u32, y0: u32, x1: u32, y1: u32) {
        self.dirty.expand(x0, y0, x1, y1);
        self.stroke_dirty.expand(x0, y0, x1, y1);
    }

    /// Mark a changed region of the alpha-channel plane currently displayed
    /// by the Channels panel.
    pub fn mark_plane_dirty(&mut self, x0: u32, y0: u32, x1: u32, y1: u32) {
        self.plane_dirty.expand(x0, y0, x1, y1);
    }

    /// Called at the end of a stroke for cleanup. Called by Tool.on_release().
    ///
    /// Resolves the "after" tiles by the SNAPSHOT's `layer_id` (not the current
    /// active layer): if the active layer changed mid-stroke — e.g. the stroke was
    /// finalized late by `begin_stroke`'s auto-flush or a focus-loss handler — we
    /// must still pair before/after on the same layer or the undo entry corrupts.
    pub fn end_stroke(&mut self) {
        if let Some(mut snap) = self.pending_stroke.take() {
            let name = std::mem::take(&mut self.pending_stroke_name);
            let target = snap.target;
            let is_16bit = self.bit_depth == BitDepth::Sixteen;
            if let Some(layer) = self
                .layer_stack
                .layers
                .iter_mut()
                .find(|l| l.id == snap.layer_id)
            {
                // Painting writes 8-bit tiles, and `get_tile_mut` drops the
                // 16-bit master on every touched tile. On a 16-bit document that
                // would flip `has_hdr()` to false for the whole layer (one 8-bit
                // tile fails the all()-check), silently degrading every later
                // filter/adjustment/export to 8-bit. Rebuild the masters from the
                // pre-stroke snapshot so only the actually-painted pixels drop to
                // 8-bit precision and the rest of each touched tile stays 16-bit.
                // Preserve masters when the document is in 16-bit mode OR the
                // layer actually carried masters before this edit — e.g. a 16-bit
                // `.iai` reopened before its mode flag was restored. has_hdr() is
                // the honest per-layer check; the mode flag can lag behind it.
                // (For 8-bit documents the first master-less tile short-circuits
                // has_hdr() to false, so this stays cheap.)
                if target == crate::core::layer::PaintTarget::Pixels
                    && (is_16bit || snap.before_tiles.has_hdr())
                {
                    layer.tiles.repromote_after_paint(&snap.before_tiles);
                }
                let has_mask = layer.mask.is_some();
                snap.capture_after(match target {
                    crate::core::layer::PaintTarget::Pixels => &layer.tiles,
                    crate::core::layer::PaintTarget::Mask => {
                        if has_mask {
                            &layer.mask.as_ref().unwrap().tiles
                        } else {
                            &layer.tiles
                        }
                    }
                });
                if snap.has_changes() {
                    self.record_as(
                        Box::new(crate::core::command::PaintCommand::new(&name, snap)),
                        ChangeKind::LayerPixels,
                    );
                }
            }
        }

        // Finalize an alpha-plane stroke (Channels panel): crop before/after
        // to the stroke bbox and push the undo command.
        if let Some(pending) = self.pending_alpha_stroke.take() {
            if let (Some((x0, y0, x1, y1)), Some(idx)) =
                (pending.bbox, self.channels.alpha_index_of(pending.alpha_id))
            {
                let ch = &self.channels.alpha[idx];
                if x1 > x0 && y1 > y0 && pending.before.len() == ch.mask.len() {
                    let crop = |data: &[u8]| {
                        let mut out = Vec::with_capacity(((x1 - x0) * (y1 - y0)) as usize);
                        for y in y0..y1 {
                            let row = (y as usize) * (ch.width as usize);
                            out.extend_from_slice(&data[row + x0 as usize..row + x1 as usize]);
                        }
                        out
                    };
                    let before = crop(&pending.before);
                    let after = crop(&ch.mask);
                    if before != after {
                        self.record_as(
                            Box::new(crate::core::command::AlphaPlanePaintCommand::new(
                                "Channel Paint",
                                pending.alpha_id,
                                (x0, y0, x1, y1),
                                before,
                                after,
                            )),
                            ChangeKind::LayerPixels,
                        );
                    }
                }
            }
        }
        self.layer_revision += 1;
    }

    /// Undo one step. Returns the SAME typed outcome an `execute`/`record` of a
    /// structural change returns, so undo can never be invalidated differently
    /// from the edit it reverses (the crop-undo-skips-GPU-resize class of bug).
    /// `None` means there was nothing to undo.
    pub fn undo(&mut self) -> Option<ChangeOutcome> {
        let undone = {
            let mut ctx = crate::core::command::EditContext::new(
                &mut self.layer_stack,
                &mut self.width,
                &mut self.height,
                Some(&mut self.selection),
            )
            .with_channels(&mut self.channels)
            .with_metadata(&mut self.metadata);
            self.cmd_history.undo(&mut ctx)
        };
        if !undone {
            return None;
        }
        self.layer_revision += 1;
        self.reconcile_selection_dims();
        self.reconcile_path_ink();
        self.flatten_full();
        Some(self.history_nav_outcome())
    }

    /// Redo one step. Mirrors [`undo`](Self::undo), including its outcome.
    pub fn redo(&mut self) -> Option<ChangeOutcome> {
        let redone = {
            let mut ctx = crate::core::command::EditContext::new(
                &mut self.layer_stack,
                &mut self.width,
                &mut self.height,
                Some(&mut self.selection),
            )
            .with_channels(&mut self.channels)
            .with_metadata(&mut self.metadata);
            self.cmd_history.redo(&mut ctx)
        };
        if !redone {
            return None;
        }
        self.layer_revision += 1;
        self.reconcile_selection_dims();
        self.reconcile_path_ink();
        self.flatten_full();
        Some(self.history_nav_outcome())
    }

    /// Invalidation for a history step. A stack entry can hold any mix of
    /// pixel/structure/selection changes (a CompoundCommand especially), so
    /// stepping through history always claims the widest invalidation — exactly
    /// what the app already did by firing LayerStructureChanged + SelectionChanged
    /// after every undo/redo.
    fn history_nav_outcome(&self) -> ChangeOutcome {
        let mut out = ChangeKind::LayerStructure
            .outcome(self.cmd_history.revision(), self.cmd_history.is_dirty());
        out.content_changed = true;
        out.selection_changed = true;
        out
    }

    /// Undoing/redoing a canvas-size change (crop, resize, rotate) restores the
    /// canvas dimensions but the commands do not carry the selection, so its
    /// mask can be left at the other size — every consumer indexes it by
    /// canvas-sized coordinates. Keep the dimensions consistent (mask CONTENT
    /// across a size change is not restored; matching PS, the selection is
    /// simply dropped/cleared by the resize).
    fn reconcile_selection_dims(&mut self) {
        if self.selection.width != self.width || self.selection.height != self.height {
            self.selection.resize(self.width, self.height);
        }
    }

    pub fn can_undo(&self) -> bool {
        self.cmd_history.undo_count() > 0
    }
    pub fn can_redo(&self) -> bool {
        self.cmd_history.redo_count() > 0
    }

    /// Record an already-applied change in history — the single door for
    /// persistent document mutation.
    ///
    /// Most commands in this codebase are *records* of a mutation that already
    /// happened (`capture_before` → mutate → `capture_after`); this is their
    /// entry point. Use [`execute`](Self::execute) for commands that still need
    /// to run.
    ///
    /// Pushing bumps the history revision and moves the saved checkpoint, so the
    /// History panel and the dirty indicator update together and cannot drift.
    pub fn record(&mut self, cmd: Box<dyn crate::core::command::Command>) -> ChangeOutcome {
        let kind = ChangeKind::LayerStructure;
        self.record_as(cmd, kind)
    }

    /// [`record`](Self::record) with an explicit change kind, so the caller can
    /// say "pixels only" or "selection only" and get the cheaper invalidation.
    pub fn record_as(
        &mut self,
        cmd: Box<dyn crate::core::command::Command>,
        kind: ChangeKind,
    ) -> ChangeOutcome {
        let (revision, is_dirty) = self.cmd_history.record(cmd);
        kind.outcome(revision, is_dirty)
    }

    /// Run a command and record it ONLY if it succeeds.
    ///
    /// A command that fails leaves the document untouched and never enters
    /// history, so a failed operation can't strand a half-applied state or a
    /// phantom undo step.
    pub fn execute(
        &mut self,
        mut cmd: Box<dyn crate::core::command::Command>,
        kind: ChangeKind,
    ) -> Result<ChangeOutcome, ChangeError> {
        let result = {
            let mut ctx = crate::core::command::EditContext::new(
                &mut self.layer_stack,
                &mut self.width,
                &mut self.height,
                Some(&mut self.selection),
            )
            .with_channels(&mut self.channels)
            .with_metadata(&mut self.metadata);
            cmd.execute(&mut ctx)
        };
        match result {
            Ok(()) => {
                self.reconcile_path_ink();
                Ok(self.record_as(cmd, kind))
            }
            Err(message) => Err(ChangeError { message }),
        }
    }

    /// Re-derive CMYK ink planes for Path layers from their (freshly rasterised)
    /// RGB mirror. A vector command rebuilds the mirror deterministically from
    /// its model (`command_vector`); on a CMYK document the ink planes must
    /// follow so separations/export and the baked `.iai` fallback stay correct.
    /// The ICC converter lives here on the canvas, not in an `EditContext`, so
    /// this runs after the gateway rather than inside the command. No-op on RGB
    /// documents (one bool check) and on CMYK documents with no Path layers.
    ///
    /// Only layers whose raster was actually re-rasterized are re-encoded: a
    /// wholesale `apply_object_to_layer` replaces a layer's tiles with ink-less
    /// ones ([`TileMap::needs_ink_encode`]), while every *other* vector layer
    /// still carries valid ink and is skipped. This keeps a single-layer edit
    /// (node drag, style scrub, transform commit) from re-running the O(area) ICC
    /// transform over every vector layer in the document — the reconcile used to
    /// rebuild the ink of the whole stack on each edit, which was a major part of
    /// the CMYK drag lag.
    pub(crate) fn reconcile_path_ink(&mut self) {
        use crate::core::layer::LayerType;
        if !self.is_cmyk() {
            return;
        }
        let any_stale =
            self.layer_stack.layers.iter().any(|l| {
                matches!(l.layer_type, LayerType::Vector(_)) && l.tiles.needs_ink_encode()
            });
        if !any_stale {
            return;
        }
        let Some(conv) = self.cmyk_converter() else {
            return;
        };
        for layer in &mut self.layer_stack.layers {
            if matches!(layer.layer_type, LayerType::Vector(_)) && layer.tiles.needs_ink_encode() {
                layer.tiles.encode_ink_from_mirror(&conv);
            }
        }
    }

    /// Re-derive every Path layer's raster cache from its model (Bước 5 / T5.2).
    /// The model is the source of truth; the cache must be rebuildable from it
    /// (Mục 3.8). Called after loading an `.iai` so a reopened document renders
    /// from the model rather than trusting the baked PNG fallback, and as a
    /// recovery path when a cache is missing/corrupt. No-op when there are no
    /// Path layers.
    pub fn rebuild_path_caches(&mut self) {
        use crate::core::layer::LayerType;
        let mut any = false;
        for layer in &mut self.layer_stack.layers {
            if matches!(
                layer.layer_type,
                LayerType::Vector(crate::core::vector::object::VectorGeometry::Path(_))
            ) {
                // Folds a saved Move-tool drag (offset ≠ model) back into the
                // model AND re-derives the raster from the model.
                crate::core::command_vector::rebuild_path_cache_from_model(layer);
                any = true;
            } else if let LayerType::Vector(
                crate::core::vector::object::VectorGeometry::Primitive(shape),
            ) = &layer.layer_type
            {
                let shape = shape.clone();
                if let Some(raster) = shape.render() {
                    layer.tiles = crate::core::tile::TileMap::from_rgba(
                        &raster.rgba,
                        raster.width,
                        raster.height,
                    );
                    layer.width = raster.width;
                    layer.height = raster.height;
                    any = true;
                }
            }
        }
        if any {
            self.reconcile_path_ink();
            self.layer_revision += 1;
        }
    }

    /// Monotonic stamp of the history stacks. The History panel keys its cache
    /// off this, so it cannot go stale.
    pub fn history_revision(&self) -> u64 {
        self.cmd_history.revision()
    }

    /// True when this canvas holds edits not present in the file on disk.
    /// Derived from the history's saved checkpoint — never assigned.
    pub fn is_dirty(&self) -> bool {
        self.cmd_history.is_dirty()
    }

    /// Anchor "clean" to the current state, after a successful save or load.
    pub fn mark_saved(&mut self) {
        self.cmd_history.mark_saved();
    }

    /// The current state can no longer be proven equal to the file on disk
    /// (e.g. content mutated outside the command system).
    pub fn mark_dirty_unconditionally(&mut self) {
        self.cmd_history.mark_saved_state_unreachable();
    }

    pub fn undo_count(&self) -> usize {
        self.cmd_history.undo_count()
    }
    pub fn redo_count(&self) -> usize {
        self.cmd_history.redo_count()
    }
    pub fn history_entries(&self) -> Vec<crate::core::command::HistoryEntry> {
        self.cmd_history.history_entries()
    }

    /// Begin an undo group (GIMP-style): every op in the group becomes one undo entry.
    /// Nested begin_undo_group is safe — only the outermost group is used.
    pub fn begin_undo_group(&mut self, label: &str) {
        self.cmd_history.begin_group(label);
    }

    /// End the undo group. Merge all its commands into one atomic entry.
    pub fn end_undo_group(&mut self) {
        self.cmd_history.end_group();
    }

    /// Discard all undo/redo history. Used when a change makes prior deltas
    /// meaningless (e.g. a colour-mode or bit-depth conversion). Routed through
    /// the gate like every other history touch, so no submodule reaches the
    /// history field directly.
    pub fn reset_history(&mut self) {
        self.cmd_history.clear();
    }

    /// Recomposite the whole canvas (after undo/redo or layer structure changes).
    ///
    /// Defers the expensive CPU flatten: sets `pixels_stale = true` and `dirty` flag
    /// so the GPU compositor refreshes, but does NOT rebuild `pixels` immediately.
    /// Call `ensure_pixels()` before reading `pixels` (eyedropper, fill, export).
    ///
    /// Large canvas (> LARGE_CANVAS_PIXELS): same behaviour — skip full flatten.
    pub fn flatten_full(&mut self) {
        self.dirty.expand_full(self.width, self.height);
        if !Self::fits_flat_buffer(self.width, self.height) {
            return;
        }
        self.pixels_stale = true;
    }

    /// Ensure `pixels` is up-to-date. Call before any code that reads `canvas.pixels`
    /// (eyedropper color sampling, fill tool sample_merged, PNG/JPEG export).
    pub fn ensure_pixels(&mut self) {
        if !self.pixels_stale {
            return;
        }
        if !Self::fits_flat_buffer(self.width, self.height) {
            return;
        }
        self.pixels = self.layer_stack.flatten(self.width, self.height);
        self.pixels_stale = false;
    }

    /// Recomposite only the dirty region (after a brush stroke — much faster).
    /// Called after mark_dirty() and before render.
    pub fn flatten_dirty(&mut self) {
        if let Some((x0, y0, x1, y1)) = self.dirty.to_rect() {
            self.layer_stack.flatten_region(
                &mut self.pixels,
                self.width,
                self.height,
                x0,
                y0,
                x1.saturating_sub(x0),
                y1.saturating_sub(y0),
            );
        }
    }

    pub fn flatten_all(&mut self) {
        let mut _cmd = crate::core::command::LayerStructureCommand::capture_before(
            "Flatten Image",
            &self.layer_stack,
            self.width,
            self.height,
        );
        if self.bit_depth == BitDepth::Sixteen {
            self.layer_stack.merge_all16(self.width, self.height);
        } else {
            self.layer_stack.merge_all(self.width, self.height);
        }
        _cmd.capture_after(&self.layer_stack, self.width, self.height);
        self.record_as(Box::new(_cmd), ChangeKind::LayerStructure);
        self.flatten_full();
    }

    pub fn active_layer(&self) -> &Layer {
        self.layer_stack.active_layer()
    }

    pub fn active_layer_mut(&mut self) -> &mut Layer {
        self.layer_stack.active_layer_mut()
    }

    pub fn active_idx(&self) -> usize {
        self.layer_stack.active_idx
    }

    pub fn set_active_idx(&mut self, idx: usize) {
        if idx < self.layer_stack.layers.len() {
            self.layer_stack.active_idx = idx;
        }
    }

    /// Get composited pixels for export.
    pub fn export_pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Flatten that doesn't modify self (for export).
    pub fn flatten_for_export(&self) -> Vec<u8> {
        self.layer_stack.flatten(self.width, self.height)
    }

    pub fn layer_count(&self) -> usize {
        self.layer_stack.layers.len()
    }
}

#[cfg(test)]
mod hdr_adjust_tests {
    use super::*;
    use crate::core::layer::AdjustmentType;

    #[test]
    fn destructive_adjustment_on_16bit_doc_stays_16bit() {
        let (w, h) = (4u32, 4u32);
        let mut px16 = vec![0u16; (w * h * 4) as usize];
        for (i, v) in px16.iter_mut().enumerate() {
            *v = if i % 4 == 3 {
                65535
            } else {
                ((i as u32 * 800 + 137) & 0xFFFF) as u16
            };
        }
        let mut canvas = Canvas::from_rgba16(px16, w, h);
        assert_eq!(canvas.bit_depth, BitDepth::Sixteen);

        let ok = canvas.apply_adjustment_to_active_layer(AdjustmentType::Invert);
        assert!(ok, "adjustment should apply to the background layer");
        // The layer must remain 16-bit (HDR master preserved through the bake).
        assert_eq!(canvas.bit_depth, BitDepth::Sixteen);
        let out = canvas
            .export_flat16_samples()
            .expect("16-bit master preserved after adjustment");
        // Invert at 16-bit: a low-byte-bearing sample stays low-byte-bearing
        // (would be lost if the bake had dropped to 8-bit).
        assert!(out.iter().any(|&v| v & 0xFF != 0), "precision survived");
    }

    #[test]
    fn paint_stroke_keeps_16bit_and_preserves_untouched_precision() {
        let (w, h) = (4u32, 4u32);
        // Distinct 16-bit values with non-zero low bytes so an 8-bit degradation
        // of untouched pixels would be detectable.
        let mut px16 = vec![0u16; (w * h * 4) as usize];
        for (i, v) in px16.iter_mut().enumerate() {
            *v = if i % 4 == 3 {
                65535
            } else {
                (i as u32 * 1234 + 777) as u16
            };
        }
        let mut canvas = Canvas::from_rgba16(px16, w, h);
        assert!(canvas.layer_stack.layers[0].tiles.has_hdr());
        let untouched_r = canvas.layer_stack.layers[0].tiles.get_pixel16(0, 0).0;
        assert_ne!(
            untouched_r % 257,
            0,
            "test value must not be 8-bit-representable"
        );

        canvas.begin_stroke("Brush Stroke");
        canvas.layer_stack.layers[0]
            .tiles
            .set_pixel(2, 2, 255, 0, 0, 255);
        assert!(
            !canvas.layer_stack.layers[0].tiles.has_hdr(),
            "an 8-bit write drops the touched tile's master mid-stroke"
        );
        canvas.end_stroke();

        let tiles = &canvas.layer_stack.layers[0].tiles;
        assert!(
            tiles.has_hdr(),
            "end_stroke rebuilds the master so the layer stays 16-bit"
        );
        // Painted pixel is embedded as the 8-bit brush colour (v*257).
        assert_eq!(tiles.get_pixel16(2, 2), (255 * 257, 0, 0, 65535));
        // An untouched pixel in the SAME tile keeps its exact original 16-bit value.
        assert_eq!(tiles.get_pixel16(0, 0).0, untouched_r);
    }

    #[test]
    fn stroke_selection_paints_locked_background() {
        let (w, h) = (5u32, 5u32);
        let mut canvas = Canvas::from_rgba(vec![255u8; (w * h * 4) as usize], w, h);
        canvas.layer_stack.layers[0].locked = true;
        canvas.layer_stack.layers[0].is_background = true;
        assert!(canvas.layer_stack.layers[0].locked);
        assert!(canvas.layer_stack.layers[0].is_background);

        canvas.selection.select_rect(1, 1, 4, 4);
        canvas.stroke_selection(StrokeParams {
            color: [255, 0, 0, 255],
            width: 1,
            location: StrokeLocation::Inside,
            opacity: 1.0,
        });

        let out = canvas.layer_stack.layers[0]
            .tiles
            .extract_region(0, 0, w, h);
        let px = |x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            &out[i..i + 4]
        };
        assert_eq!(px(1, 1), &[255, 0, 0, 255]);
        assert_eq!(px(2, 2), &[255, 255, 255, 255]);
    }

    #[test]
    fn stroke_selection_paints_unlocked_layer_zero() {
        let (w, h) = (5u32, 5u32);
        let mut canvas = Canvas::from_rgba(vec![255u8; (w * h * 4) as usize], w, h);
        canvas.layer_stack.layers[0].name = "Layer 0".to_string();
        canvas.layer_stack.layers[0].locked = false;
        canvas.layer_stack.layers[0].is_background = false;

        canvas.selection.select_rect(1, 1, 4, 4);
        canvas.stroke_selection(StrokeParams {
            color: [255, 0, 0, 255],
            width: 1,
            location: StrokeLocation::Inside,
            opacity: 1.0,
        });

        let out = canvas.layer_stack.layers[0]
            .tiles
            .extract_region(0, 0, w, h);
        let px = |x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            &out[i..i + 4]
        };
        assert_eq!(px(1, 1), &[255, 0, 0, 255]);
        assert_eq!(px(2, 2), &[255, 255, 255, 255]);
    }

    #[test]
    fn stroke_selection_paints_16bit_background() {
        let (w, h) = (5u32, 5u32);
        let mut px16 = vec![65535u16; (w * h * 4) as usize];
        for i in (0..px16.len()).step_by(4) {
            px16[i] = 65535;
            px16[i + 1] = 65535;
            px16[i + 2] = 65535;
            px16[i + 3] = 65535;
        }
        let mut canvas = Canvas::from_rgba16(px16, w, h);
        canvas.layer_stack.layers[0].locked = true;
        canvas.layer_stack.layers[0].is_background = true;

        canvas.selection.select_rect(1, 1, 4, 4);
        canvas.stroke_selection(StrokeParams {
            color: [255, 0, 0, 255],
            width: 1,
            location: StrokeLocation::Inside,
            opacity: 1.0,
        });

        let tiles = &canvas.layer_stack.layers[0].tiles;
        assert!(tiles.has_hdr());
        assert_eq!(tiles.get_pixel16(1, 1), (65535, 0, 0, 65535));
        assert_eq!(tiles.get_pixel16(2, 2), (65535, 65535, 65535, 65535));
    }

    #[test]
    fn stroke_selection_full_canvas_keeps_requested_inside_width() {
        fn assert_width(width: u32) {
            let (w, h) = (13u32, 13u32);
            let mut canvas = Canvas::from_rgba(vec![255u8; (w * h * 4) as usize], w, h);
            canvas.layer_stack.layers[0].locked = true;
            canvas.layer_stack.layers[0].is_background = true;

            canvas.selection.select_all();
            canvas.stroke_selection(StrokeParams {
                color: [255, 0, 0, 255],
                width,
                ..StrokeParams::default()
            });

            let out = canvas.layer_stack.layers[0]
                .tiles
                .extract_region(0, 0, w, h);
            let px = |x: u32, y: u32| {
                let i = ((y * w + x) * 4) as usize;
                &out[i..i + 4]
            };
            let mid_x = w / 2;
            for y in 0..width {
                assert_eq!(px(mid_x, y), &[255, 0, 0, 255], "row {y} should be stroke");
            }
            assert_eq!(
                px(mid_x, width),
                &[255, 255, 255, 255],
                "row {width} should be inside the unstroked area"
            );
        }

        assert_width(3);
        assert_width(5);
    }

    #[test]
    fn export_flat16_applies_mask_and_opacity() {
        let (w, h) = (2u32, 2u32);
        let px16 = vec![65535u16; (w * h * 4) as usize];
        let mut canvas = Canvas::from_rgba16(px16, w, h);
        {
            let layer = &mut canvas.layer_stack.layers[0];
            layer.opacity = 0.5;
            let mut mask = crate::core::layer::LayerMask::new_white(w, h);
            mask.tiles.set_pixel(0, 0, 0, 0, 0, 255);
            layer.mask = Some(mask);
        }

        let out = canvas.export_flat16_samples().expect("16-bit export");

        assert_eq!(out.len(), (w * h * 4) as usize);
        assert_eq!(out[3], 0, "black-masked pixel exports transparent");
        let a = out[7] as i32; // pixel (1,0): white mask, 50% opacity
        assert!((a - 32768).abs() <= 1, "opacity applied to alpha, got {a}");
        assert_eq!(out[4], 65535, "RGB stays straight (unpremultiplied)");
    }

    #[test]
    fn export_flat16_composites_multiple_layers_losslessly() {
        let (w, h) = (2u32, 2u32);
        // Opaque bottom with non-8-bit-representable 16-bit values.
        let mut bottom = vec![0u16; (w * h * 4) as usize];
        for (i, v) in bottom.iter_mut().enumerate() {
            *v = if i % 4 == 3 {
                65535
            } else {
                (i as u32 * 3000 + 511) as u16
            };
        }
        let mut canvas = Canvas::from_rgba16(bottom.clone(), w, h);
        // A fully transparent layer on top must leave the 16-bit bottom untouched.
        let top = canvas.layer_stack.add_layer(w, h);
        canvas.layer_stack.active_idx = top;

        let out = canvas.export_flat16_samples().expect("16-bit export");
        assert_eq!(
            out, bottom,
            "transparent top preserves the 16-bit bottom exactly"
        );
        assert!(
            out.iter().any(|&v| v & 0xFF != 0),
            "sub-8-bit precision survives the composite"
        );
    }

    #[test]
    fn flatten_image_16bit_stays_16bit() {
        let (w, h) = (2u32, 2u32);
        let mut px = vec![0u16; (w * h * 4) as usize];
        for (i, v) in px.iter_mut().enumerate() {
            *v = if i % 4 == 3 {
                65535
            } else {
                (i as u32 * 4000 + 321) as u16
            };
        }
        let mut canvas = Canvas::from_rgba16(px, w, h);

        canvas.flatten_all();

        assert_eq!(canvas.layer_stack.layers.len(), 1);
        let tiles = &canvas.layer_stack.layers[0].tiles;
        assert!(
            tiles.has_hdr(),
            "Flatten Image on a 16-bit doc stays 16-bit"
        );
        // A single opaque layer bakes to itself (white background invisible under
        // full alpha), with its exact 16-bit values intact.
        assert_eq!(tiles.get_pixel16(0, 0), (321, 4321, 8321, 65535));
    }

    #[test]
    fn export_flat16_respects_layer_offset() {
        let (w, h) = (2u32, 1u32);
        let px16 = vec![65535u16; (w * h * 4) as usize];
        let mut canvas = Canvas::from_rgba16(px16, w, h);
        canvas.layer_stack.layers[0].offset = (1, 0);

        let out = canvas.export_flat16_samples().expect("16-bit export");

        assert_eq!(out.len(), (w * h * 4) as usize);
        assert_eq!(out[3], 0, "vacated pixel is transparent");
        assert_eq!(out[7], 65535, "layer pixel lands at its offset");
    }

    #[test]
    fn flip_horizontal_works_on_large_canvas() {
        // A >25M px canvas keeps no flat buffer (Viewport Streaming); flip must
        // still work purely through the tile path, not the composite buffer.
        let (w, h) = (6000u32, 5000u32); // 30 MP
        assert!(!Canvas::fits_flat_buffer(w, h), "test needs a large canvas");
        let mut canvas = Canvas::new(w, h);
        assert!(
            canvas.pixels.is_empty(),
            "large canvas starts with no flat buffer"
        );

        canvas.layer_stack.layers[0]
            .tiles
            .set_pixel(1, 2, 10, 20, 30, 255);
        canvas.flip_horizontal();

        assert_eq!(
            (canvas.width, canvas.height),
            (w, h),
            "size unchanged by flip"
        );
        // x = 1 mirrors to w - 1 - 1 on the same row.
        assert_eq!(
            canvas.layer_stack.layers[0].tiles.get_pixel(w - 2, 2),
            (10, 20, 30, 255),
            "flip mirrored the pixel via tiles"
        );
        assert!(canvas.pixels.is_empty(), "still no flat buffer after flip");
    }

    #[test]
    fn resize_extend_and_shrink_on_large_canvas() {
        // >25M px canvas keeps no flat buffer; tile-native resize must extend and
        // shrink through the chunked blit, preserving content.
        let (w, h) = (6000u32, 5000u32); // 30 MP
        assert!(!Canvas::fits_flat_buffer(w, h));
        let mut canvas = Canvas::new(w, h);
        assert!(canvas.pixels.is_empty());
        canvas.layer_stack.layers[0]
            .tiles
            .set_pixel(10, 20, 1, 2, 3, 255);

        assert!(canvas.resize(7000, 5500)); // extend → 38.5 MP
        assert_eq!((canvas.width, canvas.height), (7000, 5500));
        assert_eq!(canvas.layer_stack.layers[0].width, 7000);
        assert!(canvas.pixels.is_empty(), "no flat buffer after extend");
        assert_eq!(
            canvas.layer_stack.layers[0].tiles.get_pixel(10, 20),
            (1, 2, 3, 255),
            "content survived the extend"
        );

        assert!(canvas.resize(5200, 5000)); // shrink → 26 MP (still large)
        assert_eq!((canvas.width, canvas.height), (5200, 5000));
        assert!(canvas.pixels.is_empty(), "no flat buffer after shrink");
        assert_eq!(
            canvas.layer_stack.layers[0].tiles.get_pixel(10, 20),
            (1, 2, 3, 255),
            "in-bounds content survived the shrink"
        );
    }

    #[test]
    fn crop_on_large_canvas() {
        // Tile-native straight crop (delete_cropped) on a >25M px canvas.
        let (w, h) = (6000u32, 5000u32);
        let mut canvas = Canvas::new(w, h);
        canvas.layer_stack.layers[0]
            .tiles
            .set_pixel(300, 400, 9, 8, 7, 255);

        assert!(canvas.crop(256, 384, 1024, 768, true));
        assert_eq!((canvas.width, canvas.height), (1024, 768));
        assert!(canvas.pixels.is_empty(), "large crop keeps no flat buffer");
        assert_eq!(canvas.layer_stack.layers[0].offset, (0, 0));
        assert_eq!(
            canvas.layer_stack.layers[0]
                .tiles
                .get_pixel(300 - 256, 400 - 384),
            (9, 8, 7, 255),
            "cropped content lands at the shifted position"
        );
    }

    #[test]
    fn rgba_import_with_alpha_creates_a_normal_layer() {
        let rgba = vec![10, 20, 30, 255, 40, 50, 60, 0];
        let canvas = Canvas::from_rgba(rgba, 2, 1);
        let layer = &canvas.layer_stack.layers[0];

        assert_eq!(layer.name, "Layer 1");
        assert!(!layer.is_background);
        assert!(!layer.locked);
        assert_eq!(layer.tiles.get_pixel(1, 0), (40, 50, 60, 0));
    }

    #[test]
    fn rgba16_import_with_alpha_creates_a_normal_layer() {
        let rgba = vec![1000, 2000, 3000, 65535, 4000, 5000, 6000, 0];
        let canvas = Canvas::from_rgba16(rgba, 2, 1);
        let layer = &canvas.layer_stack.layers[0];

        assert_eq!(layer.name, "Layer 1");
        assert!(!layer.is_background);
        assert!(!layer.locked);
        assert_eq!(layer.tiles.get_pixel16(1, 0), (4000, 5000, 6000, 0));
    }

    #[test]
    fn cmyk_plate_preview_mask_displays_ink_density() {
        let mut canvas = Canvas::from_rgba(
            vec![
                255, 0, 0, 255, // red: M/Y ink in the naive CMYK space
                0, 0, 0, 255, // black: K ink
            ],
            2,
            1,
        );
        assert!(canvas.cmyk_plate_preview_mask(0).is_none());

        canvas
            .convert_to_cmyk(CmykProfile::Naive)
            .expect("convert to CMYK");

        let cyan = canvas.cmyk_plate_preview_mask(0).unwrap();
        let magenta = canvas.cmyk_plate_preview_mask(1).unwrap();
        let black = canvas.cmyk_plate_preview_mask(3).unwrap();

        assert_eq!(
            cyan,
            vec![255, 255],
            "red and black carry no cyan in naive CMYK"
        );
        assert_eq!(magenta, vec![0, 255], "red's magenta plate is dark");
        assert_eq!(black, vec![255, 0], "black's K plate is dark");
        assert!(canvas.cmyk_plate_preview_mask(4).is_none());
    }

    #[test]
    fn cmyk_levels_adjust_ink_and_reproject_mirror() {
        use crate::core::layer::{AdjustmentType, LevelsParams};
        // Red opaque + fully transparent pixel; naive CMYK red = M=255,Y=255.
        let mut canvas = Canvas::from_rgba(vec![255, 0, 0, 255, 0, 0, 0, 0], 2, 1);
        canvas
            .convert_to_cmyk(CmykProfile::Naive)
            .expect("convert to CMYK");

        // Slot 1 = Magenta on a CMYK doc: cap output at 128.
        let mut channels = [LevelsParams::default(); 4];
        channels[1].out_white = 128;
        assert!(canvas.apply_adjustment_to_active_layer(AdjustmentType::Levels { channels }));

        let mut ink = [0u8; 8];
        canvas.layer_stack.layers[0]
            .tiles
            .extract_ink_region_into(0, 0, 2, 1, &mut ink);
        assert_eq!(ink[0], 0, "cyan untouched");
        assert_eq!(ink[1], 128, "magenta levelled 255 -> 128");
        assert_eq!(ink[2], 255, "yellow untouched");
        assert_eq!(&ink[4..8], &[0, 0, 0, 0], "transparent pixel keeps no ink");

        // Mirror re-projected from the edited ink: naive G = 1 - M.
        let (r, g, b, a) = canvas.layer_stack.layers[0].tiles.get_pixel(0, 0);
        assert_eq!(a, 255, "alpha untouched");
        assert_eq!(r, 255);
        assert!(
            (g as i32 - 127).abs() <= 1,
            "G tracks the new magenta ({g})"
        );
        assert_eq!(b, 0);
        // Ink ground truth and mirror stay consistent (naive roundtrip exact).
        let conv = canvas.cmyk_converter().unwrap();
        let mut rgb = [[0u8; 3]; 1];
        conv.cmyk_to_rgb_slice(&[[ink[0], ink[1], ink[2], ink[3]]], &mut rgb);
        assert_eq!((rgb[0][0], rgb[0][1], rgb[0][2]), (r, g, b));
    }

    #[test]
    fn cmyk_rejects_rgb_space_adjustments() {
        use crate::core::layer::AdjustmentType;
        let mut canvas = Canvas::from_rgba(vec![255, 0, 0, 255], 1, 1);
        canvas
            .convert_to_cmyk(CmykProfile::Naive)
            .expect("convert to CMYK");
        let before = canvas.layer_stack.layers[0].tiles.clone();
        assert!(
            !canvas.apply_adjustment_to_active_layer(AdjustmentType::HueSaturation {
                hue: 40.0,
                saturation: 20.0,
                lightness: 0.0,
            }),
            "RGB-space adjustments must be refused on a CMYK document"
        );
        assert_eq!(
            canvas.layer_stack.layers[0].tiles.get_pixel(0, 0),
            before.get_pixel(0, 0),
            "pixels untouched after the refusal"
        );
    }

    /// `reconcile_path_ink` must re-derive ink ONLY for layers whose raster was
    /// just rebuilt (ink-less tiles), leaving every other vector layer — and its
    /// tile revisions — untouched. This is the anti-regression for the old
    /// blanket reconcile that re-ran the O(area) ICC transform over every vector
    /// layer on each edit (a large part of the CMYK drag lag).
    #[test]
    fn reconcile_path_ink_only_reencodes_rebuilt_layers() {
        use crate::core::command_vector::{apply_object_to_layer, CreatePathLayer};
        use crate::core::gateway::ChangeKind;
        use crate::core::geometry::Point;
        use crate::core::layer::LayerType;
        use crate::core::vector::affine::AffineTransform;
        use crate::core::vector::color::ColorValue;
        use crate::core::vector::object::{VectorGeometry, VectorObjectData};
        use crate::core::vector::path::{Contour, FillRule, Node};
        use crate::core::vector::style::VectorStyle;

        fn square_obj(side: f32, at: (f32, f32)) -> VectorObjectData {
            let path = crate::core::vector::path::PathData::new(
                vec![Contour::new(
                    vec![
                        Node::sharp(Point::new(0.0, 0.0)),
                        Node::sharp(Point::new(side, 0.0)),
                        Node::sharp(Point::new(side, side)),
                        Node::sharp(Point::new(0.0, side)),
                    ],
                    true,
                )],
                FillRule::NonZero,
            );
            VectorObjectData::new(
                path,
                VectorStyle::filled(ColorValue::rgb(1.0, 0.0, 0.0)),
                AffineTransform::translate(at.0, at.1),
            )
        }

        let mut canvas = Canvas::new(300, 300);
        canvas
            .convert_to_cmyk(CmykProfile::Naive)
            .expect("convert to CMYK");
        // Two Path layers, both created through the gateway → both fully inked.
        canvas
            .execute(
                Box::new(CreatePathLayer::new(square_obj(50.0, (20.0, 20.0)), "A")),
                ChangeKind::LayerStructure,
            )
            .expect("create A");
        canvas
            .execute(
                Box::new(CreatePathLayer::new(square_obj(50.0, (150.0, 150.0)), "B")),
                ChangeKind::LayerStructure,
            )
            .expect("create B");
        let path_ids: Vec<u32> = canvas
            .layer_stack
            .layers
            .iter()
            .filter(|l| matches!(l.layer_type, LayerType::Vector(VectorGeometry::Path(_))))
            .map(|l| l.id)
            .collect();
        assert_eq!(path_ids.len(), 2, "two path layers");
        let (id_a, id_b) = (path_ids[0], path_ids[1]);
        let fp = |c: &Canvas, id: u32| {
            c.layer_stack
                .layers
                .iter()
                .find(|l| l.id == id)
                .unwrap()
                .tiles
                .revision_fingerprint()
        };
        let has_ink = |c: &Canvas, id: u32| {
            c.layer_stack
                .layers
                .iter()
                .find(|l| l.id == id)
                .unwrap()
                .tiles
                .has_any_ink()
        };
        assert!(has_ink(&canvas, id_a) && has_ink(&canvas, id_b));
        let b_before = fp(&canvas, id_b);

        // Rebuild ONLY layer A's raster (like a live preview / node commit does).
        // This strips A's ink; B is untouched and still inked.
        {
            let idx = canvas
                .layer_stack
                .layers
                .iter()
                .position(|l| l.id == id_a)
                .unwrap();
            apply_object_to_layer(
                &mut canvas.layer_stack.layers[idx],
                square_obj(80.0, (20.0, 20.0)),
            );
        }
        assert!(
            canvas
                .layer_stack
                .layers
                .iter()
                .find(|l| l.id == id_a)
                .unwrap()
                .tiles
                .needs_ink_encode(),
            "A was rebuilt ink-less"
        );

        canvas.reconcile_path_ink();

        assert!(has_ink(&canvas, id_a), "A re-inked");
        assert!(
            !canvas
                .layer_stack
                .layers
                .iter()
                .find(|l| l.id == id_a)
                .unwrap()
                .tiles
                .needs_ink_encode(),
            "A fully inked after reconcile"
        );
        assert_eq!(
            fp(&canvas, id_b),
            b_before,
            "layer B was not re-encoded (its tile revisions are untouched)"
        );
    }

    #[test]
    fn flatten_ink_reads_ink_planes_directly() {
        // Red opaque + fully transparent; naive CMYK red = M=255, Y=255.
        let mut canvas = Canvas::from_rgba(vec![255, 0, 0, 255, 0, 0, 0, 0], 2, 1);
        assert!(canvas.flatten_ink().is_none(), "RGB documents carry no ink");
        canvas
            .convert_to_cmyk(CmykProfile::Naive)
            .expect("convert to CMYK");

        let ink = canvas
            .flatten_ink()
            .expect("flat raster stack is ink-exact");
        assert_eq!(
            &ink[..4],
            &[0, 255, 255, 0],
            "red = M+Y ink, straight from the plane"
        );
        assert_eq!(
            &ink[4..],
            &[0, 0, 0, 0],
            "transparent pixel stays paper (no ink)"
        );

        // Layer opacity scales ink coverage exactly like the CMYK brush model.
        canvas.layer_stack.layers[0].opacity = 0.5;
        let ink = canvas.flatten_ink().expect("opacity alone keeps ink-exact");
        assert_eq!(&ink[..4], &[0, 128, 128, 0], "half-opacity halves the ink");
    }

    #[test]
    fn flatten_ink_refuses_non_ink_exact_stacks() {
        use crate::core::tile::TilePos;

        let mut canvas = Canvas::from_rgba(vec![255, 0, 0, 255], 1, 1);
        canvas
            .convert_to_cmyk(CmykProfile::Naive)
            .expect("convert to CMYK");
        assert!(canvas.flatten_ink().is_some());

        // Non-Normal blend modes composite in RGB.
        canvas.layer_stack.layers[0].blend_mode = crate::core::layer::BlendMode::Multiply;
        assert!(canvas.flatten_ink().is_none(), "blend mode disqualifies");
        canvas.layer_stack.layers[0].blend_mode = crate::core::layer::BlendMode::Normal;
        assert!(canvas.flatten_ink().is_some());

        // Grouped layers composite in RGB (isolation, group opacity).
        canvas.layer_stack.layers[0].parent_id = Some(99);
        assert!(
            canvas.flatten_ink().is_none(),
            "group membership disqualifies"
        );
        canvas.layer_stack.layers[0].parent_id = None;

        // A painted pixel whose tile lost its ink plane (an RGB mutation went
        // through get_tile_mut) means the plates would lie.
        canvas.layer_stack.layers[0]
            .tiles
            .get_tile_mut(TilePos { x: 0, y: 0 });
        assert!(
            canvas.flatten_ink().is_none(),
            "painted pixel without ink disqualifies"
        );
    }

    #[test]
    fn mode_conversions_reset_channel_selection() {
        let mut canvas = Canvas::from_rgba(vec![255, 0, 0, 255], 1, 1);
        canvas.channels.select_color(0, false);

        canvas
            .convert_to_cmyk(CmykProfile::Naive)
            .expect("convert to CMYK");
        assert_eq!(
            canvas.channels.view,
            crate::core::channels::ChannelView::Composite
        );
        assert!(canvas.channels.is_default_write());

        canvas.channels.select_channel_n(3, false, 4);
        canvas.convert_to_rgb_mode();
        assert_eq!(
            canvas.channels.view,
            crate::core::channels::ChannelView::Composite
        );
        assert!(canvas.channels.is_default_write());
    }

    #[test]
    fn expanding_crop_uses_the_current_background_color() {
        let mut canvas = Canvas::new(2, 2);
        let background = [12, 34, 56, 255];

        assert!(canvas.crop_with_background(-1, -1, 4, 4, true, background));
        assert_eq!((canvas.width, canvas.height), (4, 4));
        assert_eq!(canvas.layer_stack.layers.len(), 1);
        assert_eq!(
            canvas.layer_stack.layers[0].tiles.get_pixel(0, 0),
            (12, 34, 56, 255)
        );
        assert_eq!(
            canvas.layer_stack.layers[0].tiles.get_pixel(1, 1),
            (255, 255, 255, 255)
        );
    }

    #[test]
    fn expanding_transparent_document_keeps_the_png_layer() {
        let mut canvas = Canvas::from_rgba(vec![255, 0, 0, 255, 0, 0, 0, 0], 2, 1);
        let background = [12, 34, 56, 255];

        assert!(canvas.crop_with_background(-1, -1, 4, 3, true, background));
        assert_eq!(canvas.layer_stack.layers.len(), 2);

        let background_layer = &canvas.layer_stack.layers[0];
        let png_layer = &canvas.layer_stack.layers[1];
        assert!(background_layer.is_background);
        assert_eq!(background_layer.name, "Background");
        assert!(!png_layer.is_background);
        assert_eq!(png_layer.name, "Layer 1");
        assert_eq!(png_layer.tiles.get_pixel(1, 1), (255, 0, 0, 255));
        assert_eq!(png_layer.tiles.get_pixel(2, 1), (0, 0, 0, 0));
        assert_eq!(background_layer.tiles.get_pixel(0, 0), (12, 34, 56, 255));

        canvas.undo();
        assert_eq!((canvas.width, canvas.height), (2, 1));
        assert_eq!(canvas.layer_stack.layers.len(), 1);
        assert!(!canvas.layer_stack.layers[0].is_background);

        canvas.redo();
        assert_eq!((canvas.width, canvas.height), (4, 3));
        assert_eq!(canvas.layer_stack.layers.len(), 2);
        assert!(canvas.layer_stack.layers[0].is_background);
        assert!(!canvas.layer_stack.layers[1].is_background);
    }

    #[test]
    fn transformed_expanding_crop_uses_the_background_color() {
        let mut canvas = Canvas::new(2, 2);
        let background = [12, 34, 56, 255];

        assert!(canvas.crop_transformed_with_background(
            1.0, 1.0, 6.0, 6.0, 6, 6, 0.0, 0.0, 0.0, true, background,
        ));
        assert_eq!(
            canvas.layer_stack.layers[0].tiles.get_pixel(0, 0),
            (12, 34, 56, 255)
        );
        assert_eq!(
            canvas.layer_stack.layers[0].tiles.get_pixel(2, 2),
            (255, 255, 255, 255)
        );
    }

    #[test]
    fn undo_after_crop_restores_canvas_and_selection_dimensions() {
        // The crop command restores the layers and canvas size on undo, but it
        // does not carry the selection — undo/redo must still leave the
        // selection mask sized to the canvas, or every mask consumer indexes
        // out of bounds (the "Ctrl+Z after crop draws garbage" family).
        let mut canvas = Canvas::new(800, 600);
        assert!(canvas.crop(100, 50, 300, 200, true));
        assert_eq!((canvas.width, canvas.height), (300, 200));
        assert_eq!(
            (canvas.selection.width, canvas.selection.height),
            (300, 200)
        );

        canvas.undo();
        assert_eq!((canvas.width, canvas.height), (800, 600));
        assert_eq!(
            (canvas.selection.width, canvas.selection.height),
            (800, 600),
            "undo left the selection at the cropped size"
        );

        canvas.redo();
        assert_eq!((canvas.width, canvas.height), (300, 200));
        assert_eq!(
            (canvas.selection.width, canvas.selection.height),
            (300, 200),
            "redo left the selection at the pre-crop size"
        );
    }

    #[test]
    fn flatten16_keeps_precision_small() {
        // Chunked merge_all16 must match the reference flatten over white and keep
        // sub-8-bit precision on a single opaque layer.
        let (w, h) = (2u32, 2u32);
        let mut px16 = vec![0u16; (w * h * 4) as usize];
        for (i, v) in px16.iter_mut().enumerate() {
            *v = if i % 4 == 3 {
                65535
            } else {
                (i as u32 * 5000 + 321) as u16
            };
        }
        let mut canvas = Canvas::from_rgba16(px16, w, h);
        canvas.flatten_all(); // 16-bit doc → merge_all16
        let tiles = &canvas.layer_stack.layers[0].tiles;
        assert!(tiles.has_hdr(), "flatten stays 16-bit");
        let (r, g, b, a) = tiles.get_pixel16(0, 0);
        // Opaque over white = the layer's own colour (alpha full).
        assert!((r as i32 - 321).abs() <= 2, "r={r}");
        assert!((g as i32 - 5321).abs() <= 2, "g={g}");
        assert!((b as i32 - 10321).abs() <= 2, "b={b}");
        assert_eq!(a, 65535);
        assert!(r % 257 != 0, "sub-8-bit precision preserved");
    }

    #[test]
    fn edit_preserves_masters_on_hdr_layer_even_in_8bit_mode() {
        // A 16-bit .iai reopened before its mode flag is restored: the layer has
        // masters but bit_depth is Eight. An 8-bit edit under a stroke must still
        // keep the untouched pixels at 16-bit — repromote is gated on has_hdr(),
        // not only on the document mode flag.
        use crate::core::tile::TileMap;
        let (w, h) = (16u32, 16u32);
        let mut px16 = vec![0u16; (w * h * 4) as usize];
        for p in 0..(w * h) as usize {
            px16[p * 4] = 300;
            px16[p * 4 + 1] = 40000;
            px16[p * 4 + 2] = 12345;
            px16[p * 4 + 3] = 65535;
        }
        let mut canvas = Canvas::new(w, h);
        canvas.bit_depth = BitDepth::Eight; // deliberately NOT 16-bit mode
        canvas.layer_stack.layers[0].tiles = TileMap::from_rgba16(&px16, w, h);
        assert!(canvas.layer_stack.layers[0].tiles.has_hdr());

        // 8-bit edit of a 2x2 corner under a stroke; write_region drops the
        // touched tile's master.
        canvas.begin_stroke("Edit");
        canvas.layer_stack.layers[0]
            .tiles
            .write_region(0, 0, 2, 2, &[0u8, 0, 0, 255].repeat(4));
        canvas.end_stroke();

        let tiles = &canvas.layer_stack.layers[0].tiles;
        assert!(
            tiles.has_hdr(),
            "masters lost after an 8-bit edit on an hdr layer in 8-bit mode"
        );
        assert_eq!(
            tiles.get_pixel16(5, 5),
            (300, 40000, 12345, 65535),
            "untouched 16-bit pixel not preserved through the edit"
        );
    }

    #[test]
    fn crop_preserves_16bit_master() {
        // Cropping a 16-bit layer (a very common RAW operation) must keep the
        // masters, not rebuild the tiles at 8 bits.
        let (w, h) = (32u32, 32u32);
        let mut px16 = vec![0u16; (w * h * 4) as usize];
        for p in 0..(w * h) as usize {
            px16[p * 4] = 300;
            px16[p * 4 + 1] = 40000;
            px16[p * 4 + 2] = 12345;
            px16[p * 4 + 3] = 65535;
        }
        let mut canvas = Canvas::from_rgba16(px16, w, h);
        assert!(canvas.layer_stack.layers[0].tiles.has_hdr());

        assert!(canvas.crop(8, 8, 16, 16, false));
        assert_eq!((canvas.width, canvas.height), (16, 16));

        let tiles = &canvas.layer_stack.layers[0].tiles;
        assert!(tiles.has_hdr(), "crop dropped the 16-bit master");
        assert_eq!(
            tiles.get_pixel16(4, 4),
            (300, 40000, 12345, 65535),
            "crop quantized the 16-bit values"
        );
    }

    #[test]
    fn resize_preserves_16bit_master() {
        // Resampling (resize / rotate-by-angle / perspective) a 16-bit layer must
        // keep the master. A uniform sub-8-bit field (300 is not a v*257) resamples
        // to the same value, so the result proves precision survived the bilinear.
        let (w, h) = (40u32, 40u32);
        let mut px16 = vec![0u16; (w * h * 4) as usize];
        for p in 0..(w * h) as usize {
            px16[p * 4] = 300;
            px16[p * 4 + 1] = 40000;
            px16[p * 4 + 2] = 12345;
            px16[p * 4 + 3] = 65535;
        }
        let mut canvas = Canvas::from_rgba16(px16, w, h);
        assert!(canvas.layer_stack.layers[0].tiles.has_hdr());

        assert!(canvas.resize_image(20, 20, 300.0));
        assert_eq!((canvas.width, canvas.height), (20, 20));

        let tiles = &canvas.layer_stack.layers[0].tiles;
        assert!(tiles.has_hdr(), "resize dropped the 16-bit master");
        let (r, g, b, a) = tiles.get_pixel16(10, 10);
        assert!((r as i32 - 300).abs() <= 1, "r={r}");
        assert!((g as i32 - 40000).abs() <= 1, "g={g}");
        assert!((b as i32 - 12345).abs() <= 1, "b={b}");
        assert_eq!(a, 65535);
        assert!(
            r % 257 != 0 || g % 257 != 0,
            "sub-8-bit precision preserved (not an 8-bit up-convert)"
        );
    }

    #[test]
    fn filter_style_commit_preserves_16bit_outside_the_change() {
        // A filter / Smart Fill commit: 16-bit before, 8-bit after. Pixels the op
        // left unchanged must keep their 16-bit master (only the changed region
        // becomes 8-bit-sourced), so a selection-limited op does not collapse the
        // whole layer to 8-bit.
        use crate::core::tile::TileMap;
        let (w, h) = (16u32, 16u32);
        let mut px16 = vec![0u16; (w * h * 4) as usize];
        for p in 0..(w * h) as usize {
            px16[p * 4] = 300;
            px16[p * 4 + 1] = 40000;
            px16[p * 4 + 2] = 12345;
            px16[p * 4 + 3] = 65535;
        }
        let mut canvas = Canvas::from_rgba16(px16, w, h);
        let lid = canvas.layer_stack.layers[0].id;
        let before = canvas.layer_stack.layers[0].tiles.clone();
        assert!(before.has_hdr());

        // "after" = an 8-bit copy of the layer with one corner pixel changed.
        let mut after8 = before.flatten();
        after8[0] = 0;
        after8[1] = 0;
        after8[2] = 0;
        after8[3] = 255;
        let after = TileMap::from_rgba(&after8, w, h);
        assert!(!after.has_hdr());

        assert!(canvas.commit_layer_tiles_change(lid, before, after, "Filter"));

        let tiles = &canvas.layer_stack.layers[0].tiles;
        assert!(tiles.has_hdr(), "commit dropped the 16-bit master");
        assert_eq!(
            tiles.get_pixel16(5, 5),
            (300, 40000, 12345, 65535),
            "unchanged pixel lost its 16-bit precision"
        );
    }

    #[test]
    fn flatten_16bit_on_large_canvas() {
        // The RAW case: Flatten Image on a >25M px 16-bit doc must chunk (no
        // canvas-sized f32 buffer) and stay 16-bit.
        let (w, h) = (5001u32, 5001u32); // 25.01 MP, > 25M
        assert!(!Canvas::fits_flat_buffer(w, h));
        let mut canvas = Canvas::new(w, h);
        canvas.bit_depth = BitDepth::Sixteen;
        for layer in &mut canvas.layer_stack.layers {
            layer.tiles.promote_to_hdr();
        }
        canvas.flatten_all(); // → chunked merge_all16
        let tiles = &canvas.layer_stack.layers[0].tiles;
        assert!(
            tiles.has_hdr(),
            "16-bit flatten on a large canvas stays 16-bit"
        );
        assert_eq!(canvas.layer_stack.layers.len(), 1);
        assert_eq!(
            tiles.get_pixel16(5000, 5000),
            (65535, 65535, 65535, 65535),
            "white background flattens to opaque white at full precision"
        );
    }

    #[test]
    fn image_size_on_large_canvas() {
        // Image Size (resample) on a >25M px canvas must chunk (no canvas-sized
        // buffer) and stay under Viewport Streaming when the output is also large.
        let (w, h) = (6000u32, 5000u32); // 30M px
        assert!(!Canvas::fits_flat_buffer(w, h));
        let mut canvas = Canvas::new(w, h); // white background
        canvas.layer_stack.layers[0]
            .tiles
            .set_pixel(0, 0, 10, 20, 30, 255);

        assert!(canvas.resize_image(5001, 5001, 300.0)); // 25.01M, still large
        assert_eq!((canvas.width, canvas.height), (5001, 5001));
        assert_eq!(canvas.metadata.resolution_ppi, 300.0);
        assert!(
            canvas.pixels.is_empty(),
            "large resize keeps no flat buffer"
        );
        assert_eq!(
            canvas.layer_stack.layers[0].tiles.get_pixel(2500, 2500),
            (255, 255, 255, 255),
            "uniform white background resamples to white"
        );
    }

    #[test]
    fn rotated_crop_on_large_canvas() {
        // Rotated crop (resample) on a >25M px canvas must chunk. A 0-rad rotation
        // is a plain centered resample; a large output stays streamed.
        let (w, h) = (6000u32, 5000u32);
        assert!(!Canvas::fits_flat_buffer(w, h));
        let mut canvas = Canvas::new(w, h);
        assert!(canvas.crop_rotated(3000.0, 2500.0, 5001.0, 5001.0, 5001, 5001, 0.0, false));
        assert_eq!((canvas.width, canvas.height), (5001, 5001));
        assert!(
            canvas.pixels.is_empty(),
            "large rotated crop keeps no flat buffer"
        );
        assert_eq!(canvas.layer_stack.layers[0].offset, (0, 0));
        assert_eq!(
            canvas.layer_stack.layers[0].tiles.get_pixel(2500, 2500),
            (255, 255, 255, 255),
            "white background maps to white"
        );
    }
}

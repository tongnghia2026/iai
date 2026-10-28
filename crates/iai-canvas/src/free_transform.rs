//! Free Transform (Ctrl+T) without the window: a session's state, the
//! layers it holds as they were, and the commit that resamples them (one undo
//! step). The window's tool drags the handles and shows the preview; the
//! `layer.free_transform` command builds the same session from the document
//! and commits it, so the tool and an agent give the same pixels.
//!
//! Bbox:   Uses TIGHT content bounds (non-transparent pixels) for handles/overlay.
//!         Full layer bounds are kept in LayerOrigState for pixel sampling.
//! Multi:  All selected non-locked layers are transformed together (union bbox).
//! Commit: CPU resample per layer → new TileMaps; one undo command.

use iai_doc::layer::{Layer, LayerType};
use iai_doc::shape::ShapeData;
use iai_raster::tile::TileMap;
use iai_text::text::{TextData, rasterize_placed};
use iai_vector::vector::object::{VectorGeometry, VectorObjectData};
use rayon::prelude::*;

use crate::canvas::Canvas;

pub const MIN_LIVE_SCALE: f32 = 0.02;
pub const TRANSFORM_EPS: f32 = 0.001;

/// Which of the 8 edge/corner handles (or center) is being dragged.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TransformHandle {
    TopLeft,
    TopCenter,
    TopRight,
    MiddleLeft,
    MiddleRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
    Center,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TransformMode {
    #[default]
    Free,
    Skew,
    Distort,
    Perspective,
}

/// Snapshot of one layer's state before a free transform.
#[derive(Clone)]
pub struct LayerOrigState {
    pub layer_id: u32,
    pub layer_idx: usize,
    pub layer_type: iai_doc::layer::LayerType,
    pub tiles: iai_raster::tile::TileMap,
    pub mask: Option<iai_doc::layer::LayerMask>,
    pub offset: (i32, i32),
    pub width: u32,
    pub height: u32,
    pub content_offset: (i32, i32),
    pub content_w: u32,
    pub content_h: u32,
    /// Original-source canvas to the layer's canvas placement at the start of
    /// this session. Identity for the first transform; accumulated for later
    /// raster transforms so commits never resample an already filtered result.
    pub source_to_current: [f32; 9],
    /// The layer as it is, when these are the source pixels kept from an
    /// earlier commit rather than the layer's own: what undo gives back.
    pub before: Option<Box<LayerOrigState>>,
}

#[derive(Clone)]
pub struct RasterTransformLineage {
    pub source: LayerOrigState,
    pub source_to_current: [f32; 9],
    pub rendered_fingerprint: u64,
    /// Where the rendered pixels were placed. A later plain move changes only
    /// the layer offset, so the next session adds that shift to
    /// `source_to_current` instead of rendering at the old place.
    pub rendered_offset: (i32, i32),
    /// The mask the commit produced. Painting the mask afterwards leaves the
    /// pixel fingerprint alone, so this is what tells the stored source's mask
    /// is out of date.
    pub rendered_mask_fingerprint: Option<u64>,
}

/// All state for an ongoing Ctrl+T free-transform session.
/// Lives in App and is None when no transform is active.
#[derive(Clone)]
pub struct TransformState {
    pub layer_states: Vec<LayerOrigState>,
    pub preview_layer_states: Vec<LayerOrigState>,
    #[allow(dead_code)]
    pub layer_idx: usize,
    #[allow(dead_code)]
    pub layer_id: u32,
    pub orig_offset: (i32, i32),
    pub orig_w: u32,
    pub orig_h: u32,
    pub scale_x: f32,
    pub scale_y: f32,
    pub angle_deg: f32,
    pub translate_x: f32,
    pub translate_y: f32,
    pub pivot_cx: f32,
    pub pivot_cy: f32,
    pub drag_handle: Option<Option<TransformHandle>>,
    pub drag_start_cx: f32,
    pub drag_start_cy: f32,
    pub drag_start_sx: f32,
    pub drag_start_sy: f32,
    pub drag_start_angle: f32,
    pub drag_start_tx: f32,
    pub drag_start_ty: f32,
    /// Destination corners in TL, TR, BR, BL order for non-affine modes.
    pub quad: Option<[(f32, f32); 4]>,
    pub drag_start_quad: [(f32, f32); 4],
    pub mode: TransformMode,
}

pub struct TransformCommitLayer {
    pub layer_id: u32,
    pub layer_type: iai_doc::layer::LayerType,
    pub tiles: iai_raster::tile::TileMap,
    pub mask: Option<iai_doc::layer::LayerMask>,
    pub width: u32,
    pub height: u32,
    pub offset: (i32, i32),
    pub lineage: Option<RasterTransformLineage>,
}

pub struct TransformCommitResult {
    pub doc_id: crate::document::DocumentId,
    pub command: iai_history::command::FreeTransformCommand,
    pub layers: Vec<TransformCommitLayer>,
    /// A Text layer had to give up its editable type because the gesture (skew,
    /// a distort on rotated text, a size past the font limits) cannot be
    /// described by `TextData`. The status bar says so instead of letting the
    /// user discover it when the Type tool refuses to reopen the layer.
    pub rasterized_text: bool,
}

impl TransformState {
    /// A path is among the layers: Free mode only (skew, distort and
    /// perspective would turn it to pixels).
    pub fn has_path(&self) -> bool {
        self.layer_states
            .iter()
            .any(|ls| matches!(ls.layer_type, LayerType::Vector(VectorGeometry::Path(_))))
    }

    /// Forward-transform a canvas point through the current transform.
    pub fn transform_point(&self, cx: f32, cy: f32) -> (f32, f32) {
        if let Some(quad) = self.quad {
            let w = self.orig_w.max(1) as f32;
            let h = self.orig_h.max(1) as f32;
            let u = (cx - self.orig_offset.0 as f32) / w;
            let v = (cy - self.orig_offset.1 as f32) / h;
            if let Some(map) = iai_geom::geometry::Homography::square_to_quad(
                quad.map(|(x, y)| iai_geom::geometry::Point::new(x, y)),
            ) {
                let p = map.apply(u, v);
                return (p.x, p.y);
            }
        }
        let rad = self.angle_deg.to_radians();
        let c = rad.cos();
        let s = rad.sin();
        let dx = cx - self.pivot_cx;
        let dy = cy - self.pivot_cy;
        let nx = self.pivot_cx + self.translate_x + c * self.scale_x * dx - s * self.scale_y * dy;
        let ny = self.pivot_cy + self.translate_y + s * self.scale_x * dx + c * self.scale_y * dy;
        (nx, ny)
    }

    /// Inverse map from destination canvas coordinates back to original canvas.
    pub fn inverse_canvas_point(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        if let Some(quad) = self.quad {
            let map = iai_geom::geometry::Homography::square_to_quad(
                quad.map(|(x, y)| iai_geom::geometry::Point::new(x, y)),
            )?;
            let uv = map.inverse()?.apply(x, y);
            return Some((
                self.orig_offset.0 as f32 + uv.x * self.orig_w as f32,
                self.orig_offset.1 as f32 + uv.y * self.orig_h as f32,
            ));
        }
        let (a, b, c, d) = self.inv_matrix();
        let dx = x - (self.pivot_cx + self.translate_x);
        let dy = y - (self.pivot_cy + self.translate_y);
        Some((
            self.pivot_cx + a * dx + b * dy,
            self.pivot_cy + c * dx + d * dy,
        ))
    }

    /// Inverse canvas-to-canvas homography used by GPU preview and CPU commit.
    pub fn inverse_homography(&self) -> Option<[f32; 9]> {
        let src = [
            iai_geom::geometry::Point::new(self.orig_offset.0 as f32, self.orig_offset.1 as f32),
            iai_geom::geometry::Point::new(
                self.orig_offset.0 as f32 + self.orig_w as f32,
                self.orig_offset.1 as f32,
            ),
            iai_geom::geometry::Point::new(
                self.orig_offset.0 as f32 + self.orig_w as f32,
                self.orig_offset.1 as f32 + self.orig_h as f32,
            ),
            iai_geom::geometry::Point::new(
                self.orig_offset.0 as f32,
                self.orig_offset.1 as f32 + self.orig_h as f32,
            ),
        ];
        let dst = if let Some(q) = self.quad {
            q.map(|(x, y)| iai_geom::geometry::Point::new(x, y))
        } else {
            src.map(|p| {
                let (x, y) = self.transform_point(p.x, p.y);
                iai_geom::geometry::Point::new(x, y)
            })
        };
        let unit_to_src = iai_geom::geometry::Homography::square_to_quad(src)?;
        let dst_to_unit = iai_geom::geometry::Homography::square_to_quad(dst)?.inverse()?;
        let a = dst_to_unit.m;
        let b = unit_to_src.m;
        let mut out = [0.0; 9];
        for r in 0..3 {
            for c in 0..3 {
                out[r * 3 + c] = (0..3).map(|k| b[r * 3 + k] * a[k * 3 + c]).sum();
            }
        }
        Some(out)
    }

    /// Inverse matrix coefficients used by the GPU shader.
    /// inv = M⁻¹ where M = [[cos*sx, -sin*sy], [sin*sx, cos*sy]].
    pub fn inv_matrix(&self) -> (f32, f32, f32, f32) {
        let rad = self.angle_deg.to_radians();
        let c = rad.cos();
        let s = rad.sin();
        let sx = if self.scale_x.abs() < 1e-6 {
            1e-6_f32
        } else {
            self.scale_x
        };
        let sy = if self.scale_y.abs() < 1e-6 {
            1e-6_f32
        } else {
            self.scale_y
        };
        (c / sx, s / sx, -s / sy, c / sy)
    }

    /// Positions of the 8 handles in canvas space.
    /// Order: TL, TC, TR, ML, MR, BL, BC, BR.
    pub fn handle_positions(&self) -> [(f32, f32); 8] {
        if let Some([tl, tr, br, bl]) = self.quad {
            let mid = |a: (f32, f32), b: (f32, f32)| ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5);
            return [
                tl,
                mid(tl, tr),
                tr,
                mid(tl, bl),
                mid(tr, br),
                bl,
                mid(bl, br),
                br,
            ];
        }
        let ox = self.orig_offset.0 as f32;
        let oy = self.orig_offset.1 as f32;
        let w = self.orig_w as f32;
        let h = self.orig_h as f32;
        [
            self.transform_point(ox, oy),
            self.transform_point(ox + w / 2.0, oy),
            self.transform_point(ox + w, oy),
            self.transform_point(ox, oy + h / 2.0),
            self.transform_point(ox + w, oy + h / 2.0),
            self.transform_point(ox, oy + h),
            self.transform_point(ox + w / 2.0, oy + h),
            self.transform_point(ox + w, oy + h),
        ]
    }

    /// Bounding box corners in canvas space (TL, TR, BL, BR).
    pub fn corners(&self) -> [(f32, f32); 4] {
        let ox = self.orig_offset.0 as f32;
        let oy = self.orig_offset.1 as f32;
        let w = self.orig_w as f32;
        let h = self.orig_h as f32;
        [
            self.transform_point(ox, oy),
            self.transform_point(ox + w, oy),
            self.transform_point(ox, oy + h),
            self.transform_point(ox + w, oy + h),
        ]
    }

    /// Local coords (relative to pivot) for each handle — used in scale math.
    pub fn handle_local(&self, handle: TransformHandle) -> (f32, f32) {
        let hw = self.orig_w as f32 / 2.0;
        let hh = self.orig_h as f32 / 2.0;
        match handle {
            TransformHandle::TopLeft => (-hw, -hh),
            TransformHandle::TopCenter => (0.0, -hh),
            TransformHandle::TopRight => (hw, -hh),
            TransformHandle::MiddleLeft => (-hw, 0.0),
            TransformHandle::MiddleRight => (hw, 0.0),
            TransformHandle::BottomLeft => (-hw, hh),
            TransformHandle::BottomCenter => (0.0, hh),
            TransformHandle::BottomRight => (hw, hh),
            TransformHandle::Center => (0.0, 0.0),
        }
    }
}

pub fn projective_handle_indices(h: TransformHandle) -> &'static [usize] {
    match h {
        TransformHandle::TopLeft => &[0],
        TransformHandle::TopCenter => &[0, 1],
        TransformHandle::TopRight => &[1],
        TransformHandle::MiddleLeft => &[0, 3],
        TransformHandle::MiddleRight => &[1, 2],
        TransformHandle::BottomLeft => &[3],
        TransformHandle::BottomCenter => &[3, 2],
        TransformHandle::BottomRight => &[2],
        TransformHandle::Center => &[0, 1, 2, 3],
    }
}

pub fn dragged_projective_quad(
    start: [(f32, f32); 4],
    h: TransformHandle,
    mode: TransformMode,
    dx: f32,
    dy: f32,
) -> [(f32, f32); 4] {
    let mut q = start;
    if h == TransformHandle::Center {
        for p in &mut q {
            p.0 += dx;
            p.1 += dy;
        }
        return q;
    }
    match mode {
        TransformMode::Skew => {
            let horizontal = matches!(
                h,
                TransformHandle::TopLeft
                    | TransformHandle::TopCenter
                    | TransformHandle::TopRight
                    | TransformHandle::BottomLeft
                    | TransformHandle::BottomCenter
                    | TransformHandle::BottomRight
            );
            for &i in projective_handle_indices(h) {
                if horizontal {
                    q[i].0 += dx;
                } else {
                    q[i].1 += dy;
                }
            }
        }
        TransformMode::Distort => {
            for &i in projective_handle_indices(h) {
                q[i].0 += dx;
                q[i].1 += dy;
            }
        }
        TransformMode::Perspective => {
            for &i in projective_handle_indices(h) {
                q[i].0 += dx;
                q[i].1 += dy;
            }
            // Moving a corner also moves its two adjacent corners in the
            // opposite direction on the corresponding axis, producing the
            // symmetric trapezoid expected from Perspective mode.
            let corner = match h {
                TransformHandle::TopLeft => Some((0, 3, 1)),
                TransformHandle::TopRight => Some((1, 2, 0)),
                TransformHandle::BottomRight => Some((2, 1, 3)),
                TransformHandle::BottomLeft => Some((3, 0, 2)),
                _ => None,
            };
            if let Some((_i, vertical_neighbor, horizontal_neighbor)) = corner {
                q[vertical_neighbor].0 -= dx;
                q[horizontal_neighbor].1 -= dy;
            }
        }
        TransformMode::Free => {}
    }
    q
}

pub fn transform_quad_about_center(quad: &mut [(f32, f32); 4], sx: f32, sy: f32, angle_deg: f32) {
    let center = (
        quad.iter().map(|p| p.0).sum::<f32>() * 0.25,
        quad.iter().map(|p| p.1).sum::<f32>() * 0.25,
    );
    let (s, c) = angle_deg.to_radians().sin_cos();
    for p in quad {
        let x = (p.0 - center.0) * sx;
        let y = (p.1 - center.1) * sy;
        p.0 = center.0 + c * x - s * y;
        p.1 = center.1 + s * x + c * y;
    }
}

pub fn clamp_live_scale(value: f32, drag_start: f32) -> f32 {
    let sign = if drag_start < 0.0 { -1.0 } else { 1.0 };
    (value * sign).max(MIN_LIVE_SCALE) * sign
}

/// Shift-constrained translation for a Free Transform center drag. The axis is
/// chosen from the pointer's displacement since press, so pressing or releasing
/// Shift midway through a drag remains predictable and does not accumulate
/// off-axis movement.
pub fn constrained_move_delta(dx: f32, dy: f32, shift: bool) -> (f32, f32) {
    if !shift {
        (dx, dy)
    } else if dx.abs() >= dy.abs() {
        (dx, 0.0)
    } else {
        (0.0, dy)
    }
}

/// Uniform scale ratio whose transformed corner is closest to the pointer.
/// `along_*` is the pointer in the transform's unrotated coordinate system.
/// When scaling around the opposite corner, measure from that fixed corner;
/// measuring from the pivot and translating afterwards applies the pointer
/// movement twice and makes the handle outrun the cursor.
pub fn corner_drag_ratio(
    along_x: f32,
    along_y: f32,
    lx: f32,
    ly: f32,
    start_sx: f32,
    start_sy: f32,
    from_center: bool,
) -> f32 {
    let (anchor_lx, anchor_ly) = if from_center { (0.0, 0.0) } else { (-lx, -ly) };
    let base_x = start_sx * (lx - anchor_lx);
    let base_y = start_sy * (ly - anchor_ly);
    let target_x = along_x - start_sx * anchor_lx;
    let target_y = along_y - start_sy * anchor_ly;
    let denom = base_x * base_x + base_y * base_y;
    if denom <= f32::EPSILON {
        1.0
    } else {
        (target_x * base_x + target_y * base_y) / denom
    }
}

pub fn transformed_content_bounds(
    ts: &TransformState,
    ls: &LayerOrigState,
) -> Option<(i32, i32, u32, u32)> {
    let cx0 = ls.content_offset.0 as f32;
    let cy0 = ls.content_offset.1 as f32;
    let cx1 = cx0 + ls.content_w as f32;
    let cy1 = cy0 + ls.content_h as f32;

    let final_map = mat3_mul(transform_forward_homography(ts)?, ls.source_to_current);
    let map = |x, y| {
        let p = iai_geom::geometry::Homography { m: final_map }.apply(x, y);
        (p.x, p.y)
    };
    let corners = [map(cx0, cy0), map(cx1, cy0), map(cx0, cy1), map(cx1, cy1)];
    let min_cx = corners
        .iter()
        .map(|(x, _)| *x)
        .fold(f32::INFINITY, f32::min);
    let max_cx = corners
        .iter()
        .map(|(x, _)| *x)
        .fold(f32::NEG_INFINITY, f32::max);
    let min_cy = corners
        .iter()
        .map(|(_, y)| *y)
        .fold(f32::INFINITY, f32::min);
    let max_cy = corners
        .iter()
        .map(|(_, y)| *y)
        .fold(f32::NEG_INFINITY, f32::max);

    if !min_cx.is_finite() || !max_cx.is_finite() || !min_cy.is_finite() || !max_cy.is_finite() {
        return None;
    }

    // Matrix composition can leave an exact integer edge a few ULPs outside
    // (e.g. rotate 20°, then -20° -> 28.000002). Snap that numerical noise
    // before floor/ceil or a reversible transform grows a transparent row/col.
    const BOUNDS_EPS: f32 = 1.0e-4;
    let floor_x = (min_cx + BOUNDS_EPS).floor();
    let floor_y = (min_cy + BOUNDS_EPS).floor();
    let width = ((max_cx - BOUNDS_EPS).ceil() - floor_x).max(1.0);
    let height = ((max_cy - BOUNDS_EPS).ceil() - floor_y).max(1.0);

    if floor_x < i32::MIN as f32
        || floor_x > i32::MAX as f32
        || floor_y < i32::MIN as f32
        || floor_y > i32::MAX as f32
        || width > u32::MAX as f32
        || height > u32::MAX as f32
    {
        return None;
    }

    Some((floor_x as i32, floor_y as i32, width as u32, height as u32))
}

pub const MAT3_IDENTITY: [f32; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

pub fn mat3_mul(a: [f32; 9], b: [f32; 9]) -> [f32; 9] {
    let mut out = [0.0; 9];
    for r in 0..3 {
        for c in 0..3 {
            out[r * 3 + c] = (0..3).map(|k| a[r * 3 + k] * b[k * 3 + c]).sum();
        }
    }
    out
}

pub fn tile_content_fingerprint(tiles: &TileMap) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut entries: Vec<_> = tiles.tiles.iter().collect();
    entries.sort_unstable_by_key(|(pos, _)| (pos.x, pos.y));
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tiles.width.hash(&mut hasher);
    tiles.height.hash(&mut hasher);
    for (pos, tile) in entries {
        pos.x.hash(&mut hasher);
        pos.y.hash(&mut hasher);
        tile.pixels.hash(&mut hasher);
    }
    hasher.finish()
}

pub fn transform_forward_homography(ts: &TransformState) -> Option<[f32; 9]> {
    iai_geom::geometry::Homography {
        m: ts.inverse_homography()?,
    }
    .inverse()
    .map(|h| h.m)
}

pub fn translate_only(ts: &TransformState) -> Option<(i32, i32)> {
    if ts.quad.is_some() {
        return None;
    }
    let angle = ts.angle_deg.rem_euclid(360.0);
    let angle_is_zero = angle <= TRANSFORM_EPS || (360.0 - angle) <= TRANSFORM_EPS;
    if !angle_is_zero
        || (ts.scale_x - 1.0).abs() > TRANSFORM_EPS
        || (ts.scale_y - 1.0).abs() > TRANSFORM_EPS
        || !ts.translate_x.is_finite()
        || !ts.translate_y.is_finite()
    {
        return None;
    }

    let dx = ts.translate_x.round();
    let dy = ts.translate_y.round();
    if dx < i32::MIN as f32 || dx > i32::MAX as f32 || dy < i32::MIN as f32 || dy > i32::MAX as f32
    {
        return None;
    }
    Some((dx as i32, dy as i32))
}

pub fn text_scales_are_uniform(sx: f32, sy: f32) -> bool {
    (sx - sy).abs() <= sx.max(sy).max(1.0) * 0.02
}

/// A value the transform wanted, and the value the editable-text limits allow.
/// Any real difference means the committed type can no longer describe what the
/// preview showed, so the caller must keep the resampled pixels instead.
pub fn clamped_or_bail(value: f32, lo: f32, hi: f32) -> Option<f32> {
    let clamped = value.clamp(lo, hi);
    ((clamped - value).abs() <= value.abs().max(1.0) * 1.0e-3).then_some(clamped)
}

/// Scale a `TextData` by the factors expressed in ITS OWN axes. `None` when a
/// limit (font size, tracking, stretch) would silently cut the result short —
/// the layer then stays the resampled raster, which still matches the preview.
pub fn scaled_text_data(td: &TextData, sx: f32, sy: f32) -> Option<TextData> {
    let mut out = td.clone();
    let sx = sx.abs();
    let sy = sy.abs();
    let font_scale = if text_scales_are_uniform(sx, sy) {
        (sx + sy) * 0.5
    } else {
        sy
    };
    out.font_px = clamped_or_bail(
        out.font_px * font_scale,
        iai_text::text::MIN_EDITABLE_FONT_PX,
        iai_text::text::MAX_EDITABLE_FONT_PX,
    )?;
    // Font size carries the vertical scale; retain the independent horizontal
    // component so reopening the Type tool reproduces the transformed glyphs.
    out.stretch_x = clamped_or_bail(
        out.stretch_x * sx / font_scale.max(TRANSFORM_EPS),
        0.01,
        100.0,
    )?;
    out.tracking_px = clamped_or_bail(out.tracking_px * font_scale, -200.0, 500.0)?;
    for gs in &mut out.glyph_styles {
        gs.font_px = clamped_or_bail(
            gs.font_px * font_scale,
            iai_text::text::MIN_EDITABLE_FONT_PX,
            iai_text::text::MAX_EDITABLE_FONT_PX,
        )?;
    }
    Some(out)
}

/// Whether the text's own axes are parallel to the canvas axes, so a
/// non-uniform canvas scale can be split into font size (vertical) and
/// `stretch_x` (horizontal). 180° counts: it negates both axes, which the flip
/// flags absorb.
pub fn text_axes_are_canvas_aligned(rotation_deg: f32) -> bool {
    let a = rotation_deg.rem_euclid(180.0);
    a <= TRANSFORM_EPS || (180.0 - a) <= TRANSFORM_EPS
}

pub fn transformed_text_data(td: &TextData, ts: &TransformState) -> Option<TextData> {
    if ts.quad.is_some() {
        return None;
    }
    if !ts.scale_x.is_finite()
        || !ts.scale_y.is_finite()
        || !ts.angle_deg.is_finite()
        || ts.scale_x.abs() <= TRANSFORM_EPS
        || ts.scale_y.abs() <= TRANSFORM_EPS
    {
        return None;
    }

    // A non-uniform scale acts along the CANVAS axes, but font size and
    // stretch_x act along the text's own baseline. Those only agree while the
    // text is upright, so distorting already-rotated type would reshape the
    // glyphs along the wrong axis — keep the resampled pixels instead.
    if !text_scales_are_uniform(ts.scale_x.abs(), ts.scale_y.abs())
        && !text_axes_are_canvas_aligned(td.rotation_deg)
    {
        return None;
    }

    let mut out = scaled_text_data(td, ts.scale_x.abs(), ts.scale_y.abs())?;
    let sx_neg = ts.scale_x < 0.0;
    let sy_neg = ts.scale_y < 0.0;
    if sx_neg == sy_neg {
        out.rotation_deg = (td.rotation_deg + ts.angle_deg).rem_euclid(360.0);
    } else {
        out.rotation_deg = (ts.angle_deg - td.rotation_deg).rem_euclid(360.0);
    }
    if sx_neg {
        out.flip_x = !out.flip_x;
    }
    if sy_neg {
        out.flip_y = !out.flip_y;
    }
    Some(out)
}

/// Canvas position of a Text layer's upright raster origin — the single point
/// its rotation, flips and stretch pivot around. Recovered from the layer's own
/// pixels (like `text_edit_origin_for_layer` does when Type reopens), so a
/// bitmap that drifted from its `TextData` still anchors where it is drawn.
pub fn text_layer_origin(td: &TextData, tiles: &TileMap, offset: (i32, i32)) -> Option<(f32, f32)> {
    let (placed, delta) = rasterize_placed(td)?;
    let placed_tiles = TileMap::from_rgba(&placed.rgba, placed.width, placed.height);
    let (placed_min_x, placed_min_y, _, _) = placed_tiles.content_bounds()?;
    let (layer_min_x, layer_min_y, _, _) = tiles.content_bounds()?;
    Some((
        (offset.0 + layer_min_x - delta.0 - placed_min_x) as f32,
        (offset.1 + layer_min_y - delta.1 - placed_min_y) as f32,
    ))
}

/// Re-render transformed type crisply from its outlines and place it at the
/// transform's image of the old raster origin.
///
/// The gesture maps the text's whole local frame, so its origin simply travels
/// with it — `M(origin)`. Centring the fresh ink inside the transformed
/// bounding box instead (what this used to do) only agrees for ink that fills
/// that box: rotate a ragged or centred block and the committed text landed
/// several pixels off the preview.
pub fn rasterized_text_layer_at_origin(
    td: &TextData,
    origin: (f32, f32),
) -> Option<(TileMap, u32, u32, (i32, i32))> {
    if !origin.0.is_finite() || !origin.1.is_finite() {
        return None;
    }
    let (raster, delta) = rasterize_placed(td)?;
    let tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
    let ox = origin.0.round().clamp(i32::MIN as f32, i32::MAX as f32) as i32;
    let oy = origin.1.round().clamp(i32::MIN as f32, i32::MAX as f32) as i32;
    let offset = (ox.saturating_add(delta.0), oy.saturating_add(delta.1));
    Some((tiles, raster.width, raster.height, offset))
}

/// Whether the rotation keeps an axis-aligned box axis-aligned (a multiple of
/// 90°). Flips arrive as negative scales and are harmless for the symmetric
/// Rectangle/Ellipse geometry.
pub fn axis_preserving_angle(angle_deg: f32) -> bool {
    let a = angle_deg.rem_euclid(90.0);
    a <= TRANSFORM_EPS || (90.0 - a) <= TRANSFORM_EPS
}

/// Map a Shape layer's geometry through the transform: the canvas-space span of
/// the transformed shape plus its scaled corner radius and stroke width. `None`
/// when the shape can't represent the transform (a rotated rectangle/ellipse)
/// and the layer must fall back to a plain raster. Lines survive any rotation —
/// their two endpoints are simply mapped.
pub fn transformed_shape_span(
    sd: &ShapeData,
    ls: &LayerOrigState,
    ts: &TransformState,
) -> Option<(f32, f32, f32, f32, f32, f32)> {
    if ts.quad.is_some() {
        return None;
    }
    if !ts.scale_x.is_finite()
        || !ts.scale_y.is_finite()
        || !ts.angle_deg.is_finite()
        || ts.scale_x.abs() <= TRANSFORM_EPS
        || ts.scale_y.abs() <= TRANSFORM_EPS
    {
        return None;
    }
    // Linear part of the gesture, R(angle)·S(sx, sy). A shape that cannot
    // redraw the mapped picture from a mapped box (a rotated rectangle, an
    // odd-sided polygon turned on its side, a gradient that would stay put)
    // must leave ShapeData, or the commit re-renders it upright.
    let (sin, cos) = ts.angle_deg.to_radians().sin_cos();
    if !sd.survives_linear_map(
        cos * ts.scale_x,
        sin * ts.scale_x,
        -sin * ts.scale_y,
        cos * ts.scale_y,
    ) {
        return None;
    }
    let (cx0, cy0, cx1, cy1) = sd.canvas_span(ls.offset);
    let (nx0, ny0) = ts.transform_point(cx0, cy0);
    let (nx1, ny1) = ts.transform_point(cx1, cy1);
    if !(nx0.is_finite() && ny0.is_finite() && nx1.is_finite() && ny1.is_finite()) {
        return None;
    }
    let sx = ts.scale_x.abs();
    let sy = ts.scale_y.abs();
    let radius = (sd.corner_radius * sx.min(sy)).max(0.0);
    let stroke = (sd.stroke_width() * (sx * sy).sqrt()).max(0.0);
    Some((nx0, ny0, nx1, ny1, radius, stroke))
}

/// Convert a parametric Shape to an editable Path when an affine rotation can
/// no longer be represented by ShapeData's axis-aligned span. The geometry and
/// paint remain vector; only the primitive-specific controls are relinquished.
/// Free mode's gesture (scale and turn about the pivot, then move) as the
/// affine map of the page it is: what a vector object's transform is
/// composed with. `None` for a distort or a perspective, which no affine is.
pub fn gesture_affine(ts: &TransformState) -> Option<iai_vector::vector::affine::AffineTransform> {
    if ts.quad.is_some()
        || !ts.scale_x.is_finite()
        || !ts.scale_y.is_finite()
        || !ts.angle_deg.is_finite()
        || ts.scale_x.abs() <= TRANSFORM_EPS
        || ts.scale_y.abs() <= TRANSFORM_EPS
    {
        return None;
    }
    use iai_vector::vector::affine::AffineTransform;
    let pivot = AffineTransform::translate(ts.pivot_cx, ts.pivot_cy);
    let unpivot = AffineTransform::translate(-ts.pivot_cx, -ts.pivot_cy);
    Some(
        AffineTransform::translate(ts.translate_x, ts.translate_y)
            .then(&pivot)
            .then(&AffineTransform::rotate(ts.angle_deg.to_radians()))
            .then(&AffineTransform::scale(ts.scale_x, ts.scale_y))
            .then(&unpivot),
    )
}

/// A path layer after the gesture: its model composed with the gesture's
/// affine (where a Move-tool drag left the layer, not where the model last
/// rendered), rendered afresh, so it stays a sharp, editable path. Its line
/// keeps its width, as the Move tool's path handles leave it. `None` for a
/// distort or a perspective.
pub fn path_after_gesture(
    object: &VectorObjectData,
    ls: &LayerOrigState,
    ts: &TransformState,
) -> Option<(VectorObjectData, TileMap, u32, u32, (i32, i32))> {
    use iai_vector::vector::affine::AffineTransform;
    let gesture = gesture_affine(ts)?;
    let mut object = object.clone();
    if let Some((origin, _, _)) = iai_vector::vector::raster::raster_geometry(&object) {
        let dx = (ls.offset.0 - origin.0) as f32;
        let dy = (ls.offset.1 - origin.1) as f32;
        if dx != 0.0 || dy != 0.0 {
            object.transform = AffineTransform::translate(dx, dy).then(&object.transform);
        }
    }
    object.transform = gesture.then(&object.transform);
    let raster = iai_vector::vector::raster::rasterize(&object)?;
    let tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
    Some((object, tiles, raster.width, raster.height, raster.offset))
}

pub fn rotated_shape_as_path(
    sd: &ShapeData,
    ls: &LayerOrigState,
    ts: &TransformState,
) -> Option<(VectorObjectData, TileMap, u32, u32, (i32, i32))> {
    let gesture = gesture_affine(ts)?;
    let mut object = sd.to_vector_object(ls.offset);
    object.transform = gesture.then(&object.transform);
    object.style.stroke_style.width =
        (object.style.stroke_style.width * (ts.scale_x * ts.scale_y).abs().sqrt()).max(0.0);
    let raster = iai_vector::vector::raster::rasterize(&object)?;
    let tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
    Some((object, tiles, raster.width, raster.height, raster.offset))
}

/// Rebuild a Shape layer at its transformed span and re-render it crisply from
/// the vector (the analogue of `rasterized_text_layer_at`).
pub fn rasterized_shape_layer_at(
    sd: &ShapeData,
    span: (f32, f32, f32, f32),
    radius: f32,
    stroke: f32,
) -> Option<(ShapeData, TileMap, u32, u32, (i32, i32))> {
    let mut style = sd.style;
    style.stroke_style.width = stroke;
    let (mut next, off) = ShapeData::from_canvas_span_with_style(
        sd.kind, span.0, span.1, span.2, span.3, radius, style,
    );
    // `from_canvas_span_with_style` returns defaults for these; carry the source
    // shape's own values so a transform never resets the corner style, polygon
    // sides or star inner-radius.
    next.corner_type = sd.corner_type;
    next.sides = sd.sides;
    next.star_inner = sd.star_inner;
    let w = (span.2 - span.0).abs();
    let h = (span.3 - span.1).abs();
    next.corner_radius = next.corner_radius.min(w * 0.5).min(h * 0.5).max(0.0);
    let raster = next.render()?;
    let tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
    Some((next, tiles, raster.width, raster.height, off))
}

pub fn transformable_layer(layer: &Layer) -> bool {
    !layer.locked
        && !layer.is_background
        && matches!(
            layer.layer_type,
            LayerType::Raster
                | LayerType::Vector(VectorGeometry::Path(_))
                | LayerType::Text(_)
                | LayerType::Vector(VectorGeometry::Primitive(_))
                | LayerType::SmartObject
        )
}

pub fn layer_orig_state(layer: &Layer, layer_idx: usize) -> LayerOrigState {
    orig_state_with_bounds(layer, layer_idx, layer.tiles.content_bounds())
}

/// `None` for a layer without one visible pixel. It has nothing to transform,
/// and its raster rectangle (a fresh layer is canvas-sized) must not stretch
/// the box every other selected layer is scaled and turned about.
pub fn inked_layer_orig_state(layer: &Layer, layer_idx: usize) -> Option<LayerOrigState> {
    let bounds = layer.tiles.content_bounds()?;
    Some(orig_state_with_bounds(layer, layer_idx, Some(bounds)))
}

pub fn orig_state_with_bounds(
    layer: &Layer,
    layer_idx: usize,
    content_bounds: Option<(i32, i32, i32, i32)>,
) -> LayerOrigState {
    let (c_ox, c_oy, c_w, c_h) = match content_bounds {
        Some((x0, y0, x1, y1)) => (
            layer.offset.0 + x0,
            layer.offset.1 + y0,
            (x1 - x0).max(1) as u32,
            (y1 - y0).max(1) as u32,
        ),
        None => (
            layer.offset.0,
            layer.offset.1,
            layer.width.max(1),
            layer.height.max(1),
        ),
    };
    LayerOrigState {
        layer_id: layer.id,
        layer_idx,
        layer_type: layer.layer_type.clone(),
        tiles: layer.tiles.clone(),
        mask: layer.mask.clone(),
        offset: layer.offset,
        width: layer.width,
        height: layer.height,
        content_offset: (c_ox, c_oy),
        content_w: c_w,
        content_h: c_h,
        source_to_current: MAT3_IDENTITY,
        before: None,
    }
}

pub fn bake_transform_commit(
    doc_id: crate::document::DocumentId,
    ts: TransformState,
    interpolation: iai_geom::geometry::InterpolationMode,
) -> Result<TransformCommitResult, String> {
    for ls in &ts.layer_states {
        let Some((_, _, new_w, new_h)) = transformed_content_bounds(&ts, ls) else {
            return Err("Transform bi huy: kich thuoc output khong hop le".to_string());
        };
        if Canvas::checked_rgba_len(new_w, new_h).is_none() {
            return Err("Transform bi huy: layer output vuot gioi han bo nho".to_string());
        }
    }

    let mut cmd = iai_history::command::FreeTransformCommand::new("Free Transform");
    let mut updates = Vec::with_capacity(ts.layer_states.len());
    let mut rasterized_text = false;

    for ls in &ts.layer_states {
        let Some((new_ox, new_oy, new_w, new_h)) = transformed_content_bounds(&ts, ls) else {
            continue;
        };
        let mut after_layer_type = ls.layer_type.clone();
        let mut crisp_vector_layer = None;
        if let LayerType::Text(td) = &ls.layer_type {
            match transformed_text_data(td, &ts) {
                Some(next_td) => {
                    // Masked type keeps the resampled raster (its mask follows
                    // the same affine), so only the metadata is carried over.
                    if ls.mask.is_none() {
                        crisp_vector_layer = text_layer_origin(td, &ls.tiles, ls.offset)
                            .zip(transform_forward_homography(&ts))
                            .and_then(|(origin, forward)| {
                                let moved = iai_geom::geometry::Homography {
                                    m: mat3_mul(forward, ls.source_to_current),
                                }
                                .apply(origin.0, origin.1);
                                rasterized_text_layer_at_origin(&next_td, (moved.x, moved.y))
                            });
                    }
                    after_layer_type = LayerType::Text(next_td);
                }
                None => {
                    // The gesture cannot be described by editable type (skew, a
                    // distort on rotated text, a size past the font limits).
                    // Keep the exact pixels the preview showed and say so.
                    after_layer_type = LayerType::Raster;
                    rasterized_text = true;
                }
            }
        }
        if let LayerType::Vector(VectorGeometry::Path(object)) = &ls.layer_type {
            match path_after_gesture(object, ls, &ts) {
                Some((next, tiles, w, h, off)) => {
                    after_layer_type = LayerType::Vector(VectorGeometry::Path(next));
                    crisp_vector_layer = Some((tiles, w, h, off));
                }
                // A distort cannot keep it a path: its pixels are resampled.
                None => after_layer_type = LayerType::Raster,
            }
        }
        if let LayerType::Vector(VectorGeometry::Primitive(sd)) = &ls.layer_type {
            match transformed_shape_span(sd, ls, &ts) {
                Some((nx0, ny0, nx1, ny1, radius, stroke)) => {
                    let crisp = if ls.mask.is_none() {
                        rasterized_shape_layer_at(sd, (nx0, ny0, nx1, ny1), radius, stroke)
                    } else {
                        None
                    };
                    if let Some((next_sd, tiles, w, h, off)) = crisp {
                        after_layer_type = LayerType::Vector(VectorGeometry::Primitive(next_sd));
                        crisp_vector_layer = Some((tiles, w, h, off));
                    } else if ls.mask.is_some() {
                        // The mask was resampled to the transformed content
                        // bounds, so keep the resampled raster and re-anchor the
                        // geometry to those bounds — handles keep matching what
                        // is on screen and later edits render at the new size.
                        let mut next = sd.clone();
                        next.x0 = nx0 - new_ox as f32;
                        next.y0 = ny0 - new_oy as f32;
                        next.x1 = nx1 - new_ox as f32;
                        next.y1 = ny1 - new_oy as f32;
                        next.corner_radius = radius;
                        next.style.stroke_style.width = stroke;
                        after_layer_type = LayerType::Vector(VectorGeometry::Primitive(next));
                    } else {
                        after_layer_type = LayerType::Raster;
                    }
                }
                None => {
                    if let Some((object, tiles, w, h, off)) = rotated_shape_as_path(sd, ls, &ts) {
                        after_layer_type = LayerType::Vector(VectorGeometry::Path(object));
                        crisp_vector_layer = Some((tiles, w, h, off));
                    } else {
                        after_layer_type = LayerType::Raster;
                    }
                }
            }
        }

        let orig_ox = ls.offset.0 as f32;
        let orig_oy = ls.offset.1 as f32;
        let orig_w = ls.width as f32;
        let orig_h = ls.height as f32;
        let final_source_to_canvas = mat3_mul(
            transform_forward_homography(&ts)
                .ok_or_else(|| "Transform bi huy: ma tran khong hop le".to_string())?,
            ls.source_to_current,
        );
        let canvas_to_source = iai_geom::geometry::Homography {
            m: final_source_to_canvas,
        }
        .inverse()
        .ok_or_else(|| "Transform bi huy: ma tran khong the dao".to_string())?;

        let Some(pixel_len) = Canvas::checked_rgba_len(new_w, new_h) else {
            continue;
        };
        // Type and shapes that re-render crisply from their own geometry throw
        // this resample away; skipping it is the bulk of the wait between Enter
        // and the committed layer appearing.
        let mut pixels = vec![
            0u8;
            if crisp_vector_layer.is_some() {
                0
            } else {
                pixel_len
            }
        ];
        let src_tiles = &ls.tiles;
        // A rotated/projective raster needs one bilinear coverage ramp at its
        // boundary. Do not add a second supersampling filter: that widens a
        // Photoshop-style one-pixel edge into two visibly different alpha steps.
        let filter_transform_edge = ts.quad.is_some() || !axis_preserving_angle(ts.angle_deg);
        pixels
            .par_chunks_mut((new_w * 4) as usize)
            .enumerate()
            .for_each(|(py, row)| {
                // Transform pixel centres. Canvas-space layer bounds describe
                // pixel edges, whereas TileMap samplers use integer pixel-centre
                // coordinates.
                let canvas_y = py as f32 + new_oy as f32 + 0.5;
                for px in 0..new_w as usize {
                    let canvas_x = px as f32 + new_ox as f32 + 0.5;
                    let src = canvas_to_source.apply(canvas_x, canvas_y);
                    let (src_x, src_y) = (src.x, src.y);
                    let edge_x = src_x - orig_ox;
                    let edge_y = src_y - orig_oy;
                    if edge_x < 0.0 || edge_y < 0.0 || edge_x >= orig_w || edge_y >= orig_h {
                        continue;
                    }
                    let lx = edge_x - 0.5;
                    let ly = edge_y - 0.5;
                    let (r, g, b, a) = match interpolation {
                        iai_geom::geometry::InterpolationMode::Bilinear => {
                            // The continuous source rectangle extends to
                            // `width/height`, while pixel centres end at
                            // `width-1/height-1`. Clamp only after the rectangle
                            // inclusion test above so bilinear filtering does not
                            // mix a valid edge pixel with transparent out-of-bounds
                            // samples and create a wide faded fringe after scaling.
                            if filter_transform_edge {
                                let filtered = src_tiles.sample_bilinear(lx, ly);
                                let clamped = src_tiles.sample_bilinear(
                                    lx.clamp(0.0, (orig_w - 1.0).max(0.0)),
                                    ly.clamp(0.0, (orig_h - 1.0).max(0.0)),
                                );
                                if filtered.3 == 0 || clamped.3 == 0 {
                                    filtered
                                } else {
                                    // Bilinear's transparent-border contribution is
                                    // a coverage term. Sharpen that term independently
                                    // from the source alpha: the inner fringe becomes
                                    // opaque while the outer fringe carries the single
                                    // AA ramp, matching Photoshop's transformed edge.
                                    let coverage = filtered.3 as f32 / clamped.3 as f32;
                                    let sharp_coverage = (coverage * 2.0).min(1.0);
                                    (
                                        filtered.0,
                                        filtered.1,
                                        filtered.2,
                                        (clamped.3 as f32 * sharp_coverage)
                                            .round()
                                            .clamp(0.0, 255.0)
                                            as u8,
                                    )
                                }
                            } else {
                                src_tiles.sample_bilinear(
                                    lx.clamp(0.0, (orig_w - 1.0).max(0.0)),
                                    ly.clamp(0.0, (orig_h - 1.0).max(0.0)),
                                )
                            }
                        }
                        iai_geom::geometry::InterpolationMode::NearestNeighbor => {
                            src_tiles.sample_nearest(lx, ly)
                        }
                    };
                    if a == 0 {
                        continue;
                    }
                    let i = px * 4;
                    row[i] = r;
                    row[i + 1] = g;
                    row[i + 2] = b;
                    row[i + 3] = a;
                }
            });

        // The layer mask lives in layer-local space, so it must follow the same
        // affine as the pixels or it keeps cutting the layer at its old position.
        let (mask_w, mask_h, mask_ox, mask_oy) = match &crisp_vector_layer {
            Some((_, w, h, (ox, oy))) => (*w, *h, *ox, *oy),
            None => (new_w, new_h, new_ox, new_oy),
        };
        let mask_len = Canvas::checked_rgba_len(mask_w, mask_h).unwrap_or(0);
        let new_mask = ls.mask.as_ref().map(|mask| {
            let mw = mask.width.max(1) as f32;
            let mh = mask.height.max(1) as f32;
            let mask_tiles = &mask.tiles;
            let mut mask_pixels = vec![0u8; mask_len];
            mask_pixels
                .par_chunks_mut((mask_w.max(1) * 4) as usize)
                .enumerate()
                .for_each(|(py, row)| {
                    let canvas_y = py as f32 + mask_oy as f32 + 0.5;
                    for px in 0..mask_w as usize {
                        let canvas_x = px as f32 + mask_ox as f32 + 0.5;
                        let src = canvas_to_source.apply(canvas_x, canvas_y);
                        let (src_x, src_y) = (src.x, src.y);
                        // Clamp-to-edge: pixels mapping outside the source mask
                        // take the nearest edge value instead of a hard reveal.
                        let lx = (src_x - orig_ox - 0.5).clamp(0.0, mw - 1.0);
                        let ly = (src_y - orig_oy - 0.5).clamp(0.0, mh - 1.0);
                        let g = match interpolation {
                            iai_geom::geometry::InterpolationMode::Bilinear => {
                                mask_tiles.sample_bilinear(lx, ly).0
                            }
                            iai_geom::geometry::InterpolationMode::NearestNeighbor => {
                                mask_tiles.sample_nearest(lx, ly).0
                            }
                        };
                        let i = px * 4;
                        row[i] = g;
                        row[i + 1] = g;
                        row[i + 2] = g;
                        row[i + 3] = 255;
                    }
                });
            iai_doc::layer::LayerMask {
                tiles: iai_raster::tile::TileMap::from_rgba(&mask_pixels, mask_w, mask_h),
                width: mask_w,
                height: mask_h,
                enabled: mask.enabled,
                inverted: mask.inverted,
                // Re-derived on the next clip re-bake for PowerClip content.
                bake_offset: (0, 0),
                bake_frame_offset: (0, 0),
            }
        });

        let mut new_tiles = if pixels.is_empty() {
            iai_raster::tile::TileMap::new(new_w, new_h)
        } else {
            iai_raster::tile::TileMap::from_rgba(&pixels, new_w, new_h)
        };
        let mut out_w = new_w;
        let mut out_h = new_h;
        let mut out_offset = (new_ox, new_oy);
        if let Some((tiles, width, height, offset)) = crisp_vector_layer {
            new_tiles = tiles;
            out_w = width;
            out_h = height;
            out_offset = offset;
        }
        let undo = ls.before.as_deref().unwrap_or(ls);
        cmd.add_layer(
            ls.layer_id,
            undo.layer_type.clone(),
            undo.tiles.clone(),
            undo.mask.clone(),
            undo.width,
            undo.height,
            undo.offset,
            after_layer_type.clone(),
            new_tiles.clone(),
            new_mask.clone(),
            out_w,
            out_h,
            out_offset,
        );
        let lineage = matches!(after_layer_type, LayerType::Raster).then(|| {
            let mut source = ls.clone();
            source.source_to_current = MAT3_IDENTITY;
            source.before = None;
            RasterTransformLineage {
                source,
                source_to_current: final_source_to_canvas,
                rendered_fingerprint: tile_content_fingerprint(&new_tiles),
                rendered_offset: out_offset,
                rendered_mask_fingerprint: new_mask
                    .as_ref()
                    .map(|m| tile_content_fingerprint(&m.tiles)),
            }
        });
        updates.push(TransformCommitLayer {
            layer_id: ls.layer_id,
            layer_type: after_layer_type,
            tiles: new_tiles,
            mask: new_mask,
            width: out_w,
            height: out_h,
            offset: out_offset,
            lineage,
        });
    }

    Ok(TransformCommitResult {
        doc_id,
        command: cmd,
        layers: updates,
        rasterized_text,
    })
}

/// Why a Free Transform session cannot start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoSession {
    /// No layer that can be transformed is picked (or active).
    NothingToTransform,
}

/// The session Ctrl+T starts on `canvas`: the layers `picked` names, or the
/// picked layers (the active one when none is), a group standing for its
/// members, each as the last commit resampled it from (`lineage`, which
/// drops what no longer holds). The box is the layers' content, or `bounds`
/// (top-left, bottom-right) when the Move tool's box is the one shown. The
/// transform starts as none.
pub fn session(
    canvas: &Canvas,
    lineage: &mut std::collections::HashMap<u32, RasterTransformLineage>,
    picked: Option<&[u32]>,
    bounds: Option<((f32, f32), (f32, f32))>,
) -> Result<TransformState, NoSession> {
    let stack = &canvas.layer_stack;
    let active_idx = stack.active_idx;
    let movable = |l: &Layer| !l.locked && !l.is_background;
    let candidates: Vec<usize> = match picked {
        Some(ids) => stack
            .layers
            .iter()
            .enumerate()
            .filter(|(_, l)| ids.contains(&l.id) && movable(l))
            .map(|(i, _)| i)
            .collect(),
        None => {
            let sel: Vec<usize> = stack
                .layers
                .iter()
                .enumerate()
                .filter(|(_, l)| l.selected && movable(l))
                .map(|(i, _)| i)
                .collect();
            if sel.is_empty() {
                match stack.layers.get(active_idx) {
                    Some(l) if movable(l) => vec![active_idx],
                    _ => Vec::new(),
                }
            } else {
                sel
            }
        }
    };
    if candidates.is_empty() {
        return Err(NoSession::NothingToTransform);
    }

    let mut target_indices = Vec::new();
    let mut seen_targets = std::collections::HashSet::new();
    let mut preview_layer_states = Vec::new();
    for idx in candidates {
        let Some(layer) = stack.layers.get(idx) else {
            continue;
        };
        if layer.is_group() {
            preview_layer_states.push(layer_orig_state(layer, idx));
            for member_idx in stack.group_member_range(idx) {
                let Some(member) = stack.layers.get(member_idx) else {
                    continue;
                };
                if transformable_layer(member) && seen_targets.insert(member_idx) {
                    target_indices.push(member_idx);
                }
            }
        } else if transformable_layer(layer) && seen_targets.insert(idx) {
            target_indices.push(idx);
        }
    }

    let mut layer_states: Vec<LayerOrigState> = Vec::with_capacity(target_indices.len());
    for pass in 0..2 {
        for &idx in &target_indices {
            let is_primary = idx == active_idx;
            if (pass == 0) != is_primary {
                continue;
            }
            if let Some(ls) = inked_layer_orig_state(&stack.layers[idx], idx) {
                layer_states.push(ls);
            }
        }
    }
    if layer_states.is_empty() {
        return Err(NoSession::NothingToTransform);
    }

    let (u_x0, u_y0, u_x1, u_y1) = match bounds {
        Some((top_left, bottom_right)) => (
            top_left.0 as i32,
            top_left.1 as i32,
            bottom_right.0 as i32,
            bottom_right.1 as i32,
        ),
        None => layer_states.iter().fold(
            (i32::MAX, i32::MAX, i32::MIN, i32::MIN),
            |(x0, y0, x1, y1), ls| {
                (
                    x0.min(ls.content_offset.0),
                    y0.min(ls.content_offset.1),
                    x1.max(ls.content_offset.0 + ls.content_w as i32),
                    y1.max(ls.content_offset.1 + ls.content_h as i32),
                )
            },
        ),
    };
    let orig_offset = (u_x0, u_y0);
    let orig_w = (u_x1 - u_x0).max(1) as u32;
    let orig_h = (u_y1 - u_y0).max(1) as u32;
    let pivot_cx = u_x0 as f32 + orig_w as f32 / 2.0;
    let pivot_cy = u_y0 as f32 + orig_h as f32 / 2.0;

    // Reuse the untouched raster source from preceding Free Transform
    // commits. The current layer bounds above remain the UI box for this
    // session, while the worker receives the original pixels plus the
    // accumulated source→current mapping. Any intervening pixel or mask edit
    // changes a fingerprint and automatically breaks the lineage.
    for ls in &mut layer_states {
        if !matches!(ls.layer_type, LayerType::Raster) {
            continue;
        }
        let current_fp = tile_content_fingerprint(&ls.tiles);
        let current_mask_fp = ls.mask.as_ref().map(|m| tile_content_fingerprint(&m.tiles));
        match lineage.get(&ls.layer_id) {
            Some(kept)
                if kept.rendered_fingerprint == current_fp
                    && kept.rendered_mask_fingerprint == current_mask_fp =>
            {
                // A plain move since that commit (Move tool, arrow keys,
                // Align, a drag-only Free Transform) shifted the offset but
                // not the pixels. Carry the shift, or this commit would
                // render the layer back where the last one left it.
                let dx = (ls.offset.0 - kept.rendered_offset.0) as f32;
                let dy = (ls.offset.1 - kept.rendered_offset.1) as f32;
                let shift = [1.0, 0.0, dx, 0.0, 1.0, dy, 0.0, 0.0, 1.0];
                let layer_idx = ls.layer_idx;
                let source_to_current = mat3_mul(shift, kept.source_to_current);
                let current = std::mem::replace(ls, kept.source.clone());
                ls.layer_idx = layer_idx;
                ls.source_to_current = source_to_current;
                ls.before = Some(Box::new(current));
            }
            Some(_) => {
                lineage.remove(&ls.layer_id);
            }
            None => {}
        }
    }

    let layer_id = layer_states[0].layer_id;
    let layer_idx = layer_states[0].layer_idx;
    Ok(TransformState {
        layer_states,
        preview_layer_states,
        layer_idx,
        layer_id,
        orig_offset,
        orig_w,
        orig_h,
        scale_x: 1.0,
        scale_y: 1.0,
        angle_deg: 0.0,
        translate_x: 0.0,
        translate_y: 0.0,
        pivot_cx,
        pivot_cy,
        drag_handle: None,
        drag_start_cx: 0.0,
        drag_start_cy: 0.0,
        drag_start_sx: 1.0,
        drag_start_sy: 1.0,
        drag_start_angle: 0.0,
        drag_start_tx: 0.0,
        drag_start_ty: 0.0,
        quad: None,
        drag_start_quad: [(0.0, 0.0); 4],
        mode: TransformMode::Free,
    })
}

/// A session that only moves its layers by whole pixels: move them and
/// record the step. `None` when it does more than that; `Some(false)` when it
/// does nothing at all.
pub fn move_only(canvas: &mut Canvas, ts: &TransformState) -> Option<bool> {
    let (dx, dy) = translate_only(ts)?;
    if dx == 0 && dy == 0 {
        return Some(false);
    }
    let layer_ids: Vec<u32> = ts.layer_states.iter().map(|ls| ls.layer_id).collect();
    for layer in canvas.layer_stack.layers.iter_mut() {
        if layer_ids.contains(&layer.id) {
            layer.offset.0 = layer.offset.0.saturating_add(dx);
            layer.offset.1 = layer.offset.1.saturating_add(dy);
        }
    }
    let cmd = iai_history::command::TranslateLayerCommand::from_applied_move(
        layer_ids,
        dx,
        dy,
        &canvas.layer_stack,
    );
    canvas.record(Box::new(cmd));
    Some(true)
}

/// Lay a finished commit's layers into `canvas`, keep their sources in
/// `lineage`, and record the step.
pub fn land(
    canvas: &mut Canvas,
    lineage: &mut std::collections::HashMap<u32, RasterTransformLineage>,
    result: TransformCommitResult,
) {
    for update in result.layers {
        match update.lineage {
            Some(kept) => {
                lineage.insert(update.layer_id, kept);
            }
            None => {
                lineage.remove(&update.layer_id);
            }
        }
        if let Some(layer) = canvas
            .layer_stack
            .layers
            .iter_mut()
            .find(|l| l.id == update.layer_id)
        {
            layer.tiles = update.tiles;
            layer.mask = update.mask;
            layer.width = update.width;
            layer.height = update.height;
            layer.offset = update.offset;
            layer.layer_type = update.layer_type;
        }
    }
    // A path drawn afresh needs its ink again on a CMYK page.
    canvas.reconcile_path_ink();
    canvas.record(Box::new(result.command));
}

#[cfg(test)]
mod tests {
    use super::*;
    use iai_doc::shape::ShapeKind;

    /// A turned shape is drawn again as a path in a frame of its own; its
    /// mask is laid into that same frame (it used to be resampled to the
    /// transformed content box, and sat apart from the shape).
    #[test]
    fn a_turned_shape_keeps_its_mask_in_its_own_frame() {
        let (sd, off) = ShapeData::from_canvas_span(
            ShapeKind::Rectangle,
            20.0,
            20.0,
            120.0,
            80.0,
            0.0,
            true,
            [200, 30, 30, 255],
            0.0,
            [0, 0, 0, 0],
        );
        let raster = sd.render().expect("the shape renders");
        let mut layer = Layer::new(7, "Shape", raster.width, raster.height);
        layer.tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
        layer.offset = off;
        layer.layer_type = LayerType::Vector(VectorGeometry::Primitive(sd));
        layer.add_mask(true);
        let ls = inked_layer_orig_state(&layer, 1).expect("inked");
        let (x0, y0) = (ls.content_offset.0, ls.content_offset.1);
        let (w, h) = (ls.content_w, ls.content_h);
        let ts = TransformState {
            layer_states: vec![ls],
            preview_layer_states: Vec::new(),
            layer_idx: 1,
            layer_id: 7,
            orig_offset: (x0, y0),
            orig_w: w,
            orig_h: h,
            scale_x: 1.0,
            scale_y: 1.0,
            angle_deg: 30.0,
            translate_x: 0.0,
            translate_y: 0.0,
            pivot_cx: x0 as f32 + w as f32 / 2.0,
            pivot_cy: y0 as f32 + h as f32 / 2.0,
            drag_handle: None,
            drag_start_cx: 0.0,
            drag_start_cy: 0.0,
            drag_start_sx: 1.0,
            drag_start_sy: 1.0,
            drag_start_angle: 0.0,
            drag_start_tx: 0.0,
            drag_start_ty: 0.0,
            quad: None,
            drag_start_quad: [(0.0, 0.0); 4],
            mode: TransformMode::Free,
        };
        let result = bake_transform_commit(
            crate::document::DocumentId(1),
            ts,
            iai_geom::geometry::InterpolationMode::Bilinear,
        )
        .expect("the bake");
        let out = &result.layers[0];
        assert!(matches!(
            out.layer_type,
            LayerType::Vector(VectorGeometry::Path(_))
        ));
        let mask = out.mask.as_ref().expect("the mask goes with it");
        assert_eq!((mask.width, mask.height), (out.width, out.height));
    }
}

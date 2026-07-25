// Free Transform (Ctrl+T) — begin / commit / cancel / drag logic.
//
// Bbox:   Uses TIGHT content bounds (non-transparent pixels) for handles/overlay.
//         Full layer bounds are kept in LayerOrigState for pixel sampling.
// Multi:  All selected non-locked layers are transformed together (union bbox).
// GPU:    Primary layer (index 0) gets live inverse-matrix preview shader.
// Commit: CPU bilinear resample per layer → new TileMaps; one undo command.

use super::state::{
    App, LayerOrigState, TransformCommitLayer, TransformCommitResult, TransformHandle,
    TransformState,
};
use crate::app::render::CanvasEvent;
use crate::core::canvas::Canvas;
use crate::core::document::GuideOrientation;
use crate::core::layer::{Layer, LayerType};
use crate::core::shape::{ShapeData, ShapeKind};
use crate::core::snapping::{best_snap, SnapKind, SnapLine, SNAP_THRESHOLD_PX};
use crate::core::text::{rasterize_placed, TextData};
use crate::core::tile::TileMap;
use crate::core::vector::object::VectorGeometry;
use crate::gpu::compositor::TransformPreviewUniform;
use rayon::prelude::*;

const HANDLE_RADIUS: f32 = 8.0;
const ROTATE_ZONE: f32 = 24.0;
const MIN_LIVE_SCALE: f32 = 0.02;
const TRANSFORM_EPS: f32 = 0.001;

fn projective_handle_indices(h: TransformHandle) -> &'static [usize] {
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

fn dragged_projective_quad(
    start: [(f32, f32); 4],
    h: TransformHandle,
    mode: crate::app::state::TransformMode,
    dx: f32,
    dy: f32,
) -> [(f32, f32); 4] {
    use crate::app::state::TransformMode;
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

fn transform_quad_about_center(quad: &mut [(f32, f32); 4], sx: f32, sy: f32, angle_deg: f32) {
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

fn clamp_live_scale(value: f32, drag_start: f32) -> f32 {
    let sign = if drag_start < 0.0 { -1.0 } else { 1.0 };
    (value * sign).max(MIN_LIVE_SCALE) * sign
}

/// Uniform scale ratio whose transformed corner is closest to the pointer.
/// `along_*` is the pointer in the transform's unrotated coordinate system.
/// When scaling around the opposite corner, measure from that fixed corner;
/// measuring from the pivot and translating afterwards applies the pointer
/// movement twice and makes the handle outrun the cursor.
fn corner_drag_ratio(
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

fn transformed_content_bounds(
    ts: &TransformState,
    ls: &LayerOrigState,
) -> Option<(i32, i32, u32, u32)> {
    let cx0 = ls.content_offset.0 as f32;
    let cy0 = ls.content_offset.1 as f32;
    let cx1 = cx0 + ls.content_w as f32;
    let cy1 = cy0 + ls.content_h as f32;

    let corners = [
        ts.transform_point(cx0, cy0),
        ts.transform_point(cx1, cy0),
        ts.transform_point(cx0, cy1),
        ts.transform_point(cx1, cy1),
    ];
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

    let floor_x = min_cx.floor();
    let floor_y = min_cy.floor();
    let width = (max_cx.ceil() - floor_x).max(1.0);
    let height = (max_cy.ceil() - floor_y).max(1.0);

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

fn translate_only(ts: &TransformState) -> Option<(i32, i32)> {
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

fn axis_aligned_positive_text_scale(ts: &TransformState) -> Option<(f32, f32)> {
    if ts.quad.is_some() {
        return None;
    }
    let angle = ts.angle_deg.rem_euclid(360.0);
    let angle_is_zero = angle <= TRANSFORM_EPS || (360.0 - angle) <= TRANSFORM_EPS;
    if !angle_is_zero
        || !ts.scale_x.is_finite()
        || !ts.scale_y.is_finite()
        || ts.scale_x <= 0.0
        || ts.scale_y <= 0.0
    {
        return None;
    }
    Some((ts.scale_x, ts.scale_y))
}

fn text_scales_are_uniform(sx: f32, sy: f32) -> bool {
    (sx - sy).abs() <= sx.max(sy).max(1.0) * 0.02
}

fn scaled_text_data(td: &TextData, sx: f32, sy: f32) -> TextData {
    let mut out = td.clone();
    let sx = sx.abs();
    let sy = sy.abs();
    let font_scale = if text_scales_are_uniform(sx, sy) {
        (sx + sy) * 0.5
    } else {
        sy
    };
    out.font_px = (out.font_px * font_scale).clamp(4.0, 1600.0);
    // Font size carries the vertical scale; retain the independent horizontal
    // component so reopening the Type tool reproduces the transformed glyphs.
    out.stretch_x = (out.stretch_x * sx / font_scale.max(TRANSFORM_EPS)).clamp(0.01, 100.0);
    out.tracking_px = (out.tracking_px * font_scale).clamp(-200.0, 500.0);
    for gs in &mut out.glyph_styles {
        gs.font_px = (gs.font_px * font_scale).clamp(4.0, 1600.0);
    }
    out
}

fn transformed_text_data(td: &TextData, ts: &TransformState) -> Option<TextData> {
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

    let mut out = scaled_text_data(td, ts.scale_x.abs(), ts.scale_y.abs());
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

fn rasterized_text_layer_at(
    td: &TextData,
    content_origin: (i32, i32),
) -> Option<(TileMap, u32, u32, (i32, i32))> {
    let (raster, delta) = rasterize_placed(td)?;
    let tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
    let placed_origin = (
        content_origin.0.saturating_add(delta.0),
        content_origin.1.saturating_add(delta.1),
    );
    let offset = tiles
        .content_bounds()
        .map(|(min_x, min_y, _, _)| {
            (
                placed_origin.0.saturating_sub(min_x),
                placed_origin.1.saturating_sub(min_y),
            )
        })
        .unwrap_or(placed_origin);
    Some((tiles, raster.width, raster.height, offset))
}

/// Whether the rotation keeps an axis-aligned box axis-aligned (a multiple of
/// 90°). Flips arrive as negative scales and are harmless for the symmetric
/// Rectangle/Ellipse geometry.
fn axis_preserving_angle(angle_deg: f32) -> bool {
    let a = angle_deg.rem_euclid(90.0);
    a <= TRANSFORM_EPS || (90.0 - a) <= TRANSFORM_EPS
}

/// Map a Shape layer's geometry through the transform: the canvas-space span of
/// the transformed shape plus its scaled corner radius and stroke width. `None`
/// when the shape can't represent the transform (a rotated rectangle/ellipse)
/// and the layer must fall back to a plain raster. Lines survive any rotation —
/// their two endpoints are simply mapped.
fn transformed_shape_span(
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
    if sd.kind != ShapeKind::Line && !axis_preserving_angle(ts.angle_deg) {
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

/// Rebuild a Shape layer at its transformed span and re-render it crisply from
/// the vector (the analogue of `rasterized_text_layer_at`).
fn rasterized_shape_layer_at(
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
    let w = (span.2 - span.0).abs();
    let h = (span.3 - span.1).abs();
    next.corner_radius = next.corner_radius.min(w * 0.5).min(h * 0.5).max(0.0);
    let raster = next.render()?;
    let tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
    Some((next, tiles, raster.width, raster.height, off))
}

fn transformable_layer(layer: &Layer) -> bool {
    !layer.locked
        && !layer.is_background
        && matches!(
            layer.layer_type,
            LayerType::Raster
                | LayerType::Text(_)
                | LayerType::Vector(VectorGeometry::Primitive(_))
                | LayerType::SmartObject
        )
}

fn layer_orig_state(layer: &Layer, layer_idx: usize) -> LayerOrigState {
    let (c_ox, c_oy, c_w, c_h) = match layer.tiles.content_bounds() {
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
    }
}

fn bake_transform_commit(
    doc_id: crate::core::document::DocumentId,
    ts: TransformState,
    interpolation: crate::core::geometry::InterpolationMode,
) -> Result<TransformCommitResult, String> {
    for ls in &ts.layer_states {
        let Some((_, _, new_w, new_h)) = transformed_content_bounds(&ts, ls) else {
            return Err("Transform bi huy: kich thuoc output khong hop le".to_string());
        };
        if Canvas::checked_rgba_len(new_w, new_h).is_none() {
            return Err("Transform bi huy: layer output vuot gioi han bo nho".to_string());
        }
    }

    let mut cmd = crate::core::command::FreeTransformCommand::new("Free Transform");
    let mut updates = Vec::with_capacity(ts.layer_states.len());

    for ls in &ts.layer_states {
        let Some((new_ox, new_oy, new_w, new_h)) = transformed_content_bounds(&ts, ls) else {
            continue;
        };
        let mut after_layer_type = ls.layer_type.clone();
        let mut crisp_vector_layer = None;
        if let LayerType::Text(td) = &ls.layer_type {
            if let Some((sx, sy)) = axis_aligned_positive_text_scale(&ts) {
                let next_td = scaled_text_data(td, sx, sy);
                after_layer_type = LayerType::Text(next_td.clone());
                if text_scales_are_uniform(sx, sy) && ls.mask.is_none() {
                    crisp_vector_layer = rasterized_text_layer_at(&next_td, (new_ox, new_oy));
                }
            } else if let Some(next_td) = transformed_text_data(td, &ts) {
                after_layer_type = LayerType::Text(next_td.clone());
                if ls.mask.is_none() {
                    crisp_vector_layer = rasterized_text_layer_at(&next_td, (new_ox, new_oy));
                }
            } else {
                after_layer_type = LayerType::Raster;
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
                None => after_layer_type = LayerType::Raster,
            }
        }

        let orig_ox = ls.offset.0 as f32;
        let orig_oy = ls.offset.1 as f32;
        let orig_w = ls.width as f32;
        let orig_h = ls.height as f32;

        let Some(pixel_len) = Canvas::checked_rgba_len(new_w, new_h) else {
            continue;
        };
        let mut pixels = vec![0u8; pixel_len];
        let src_tiles = &ls.tiles;
        pixels
            .par_chunks_mut((new_w * 4) as usize)
            .enumerate()
            .for_each(|(py, row)| {
                let canvas_y = py as f32 + new_oy as f32;
                for px in 0..new_w as usize {
                    let canvas_x = px as f32 + new_ox as f32;
                    let Some((src_x, src_y)) = ts.inverse_canvas_point(canvas_x, canvas_y) else {
                        continue;
                    };
                    let lx = src_x - orig_ox;
                    let ly = src_y - orig_oy;
                    if lx < 0.0 || ly < 0.0 || lx >= orig_w || ly >= orig_h {
                        continue;
                    }
                    let (r, g, b, a) = match interpolation {
                        crate::core::geometry::InterpolationMode::Bilinear => {
                            src_tiles.sample_bilinear(lx, ly)
                        }
                        crate::core::geometry::InterpolationMode::NearestNeighbor => {
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
        let new_mask = ls.mask.as_ref().map(|mask| {
            let mw = mask.width.max(1) as f32;
            let mh = mask.height.max(1) as f32;
            let mask_tiles = &mask.tiles;
            let mut mask_pixels = vec![0u8; pixel_len];
            mask_pixels
                .par_chunks_mut((new_w * 4) as usize)
                .enumerate()
                .for_each(|(py, row)| {
                    let canvas_y = py as f32 + new_oy as f32;
                    for px in 0..new_w as usize {
                        let canvas_x = px as f32 + new_ox as f32;
                        let Some((src_x, src_y)) = ts.inverse_canvas_point(canvas_x, canvas_y)
                        else {
                            continue;
                        };
                        // Clamp-to-edge: pixels mapping outside the source mask
                        // take the nearest edge value instead of a hard reveal.
                        let lx = (src_x - orig_ox).clamp(0.0, mw - 1.0);
                        let ly = (src_y - orig_oy).clamp(0.0, mh - 1.0);
                        let g = match interpolation {
                            crate::core::geometry::InterpolationMode::Bilinear => {
                                mask_tiles.sample_bilinear(lx, ly).0
                            }
                            crate::core::geometry::InterpolationMode::NearestNeighbor => {
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
            crate::core::layer::LayerMask {
                tiles: crate::core::tile::TileMap::from_rgba(&mask_pixels, new_w, new_h),
                width: new_w,
                height: new_h,
                enabled: mask.enabled,
                inverted: mask.inverted,
            }
        });

        let mut new_tiles = crate::core::tile::TileMap::from_rgba(&pixels, new_w, new_h);
        let mut out_w = new_w;
        let mut out_h = new_h;
        let mut out_offset = (new_ox, new_oy);
        if let Some((tiles, width, height, offset)) = crisp_vector_layer {
            new_tiles = tiles;
            out_w = width;
            out_h = height;
            out_offset = offset;
        }
        cmd.add_layer(
            ls.layer_id,
            ls.layer_type.clone(),
            ls.tiles.clone(),
            ls.mask.clone(),
            ls.width,
            ls.height,
            ls.offset,
            after_layer_type.clone(),
            new_tiles.clone(),
            new_mask.clone(),
            out_w,
            out_h,
            out_offset,
        );
        updates.push(TransformCommitLayer {
            layer_id: ls.layer_id,
            layer_type: after_layer_type,
            tiles: new_tiles,
            mask: new_mask,
            width: out_w,
            height: out_h,
            offset: out_offset,
        });
    }

    Ok(TransformCommitResult {
        doc_id,
        command: cmd,
        layers: updates,
    })
}

impl App {
    pub fn begin_transform(&mut self) {
        if self.edit.transform_state.is_some() {
            return;
        }
        if self.edit.text_edit.is_some() {
            self.commit_text_edit();
        }
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let active_idx = canvas.layer_stack.active_idx;

        let candidates: Vec<usize> = {
            let sel: Vec<usize> = canvas
                .layer_stack
                .layers
                .iter()
                .enumerate()
                .filter(|(_, l)| l.selected && !l.locked && !l.is_background)
                .map(|(i, _)| i)
                .collect();
            if sel.is_empty() {
                match canvas.layer_stack.layers.get(active_idx) {
                    Some(l) if !l.locked && !l.is_background => vec![active_idx],
                    _ => return,
                }
            } else {
                sel
            }
        };

        // Path geometry owns an affine model and must never enter the raster
        // Free Transform worker. Scale/rotate it with the Move-tool handles;
        // skew/projective requires an explicit Rasterize decision. A mixed
        // selection is blocked as a whole instead of silently transforming only
        // the raster members and tearing the selection apart.
        let selection_contains_path =
            candidates.iter().any(|&idx| {
                canvas.layer_stack.layers.get(idx).is_some_and(|layer| {
                    matches!(layer.layer_type, LayerType::Vector(VectorGeometry::Path(_)))
                        || (layer.is_group()
                            && canvas
                                .layer_stack
                                .group_member_range(idx)
                                .any(|member_idx| {
                                    canvas.layer_stack.layers.get(member_idx).is_some_and(
                                        |member| {
                                            matches!(
                                                member.layer_type,
                                                LayerType::Vector(VectorGeometry::Path(_))
                                            )
                                        },
                                    )
                                }))
                })
            });
        if selection_contains_path {
            self.shell.status_msg =
                "Path uses affine Move handles; rasterize explicitly for skew/projective"
                    .to_string();
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return;
        }

        let mut target_indices = Vec::new();
        let mut seen_targets = std::collections::HashSet::new();
        let mut preview_layer_states = Vec::new();
        for idx in candidates {
            let Some(layer) = canvas.layer_stack.layers.get(idx) else {
                continue;
            };
            if layer.is_group() {
                preview_layer_states.push(layer_orig_state(layer, idx));
                for member_idx in canvas.layer_stack.group_member_range(idx) {
                    let Some(member) = canvas.layer_stack.layers.get(member_idx) else {
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
                let l = &canvas.layer_stack.layers[idx];
                layer_states.push(layer_orig_state(l, idx));
            }
        }
        if layer_states.is_empty() {
            return;
        }

        let (u_x0, u_y0, u_x1, u_y1) = layer_states.iter().fold(
            (i32::MAX, i32::MAX, i32::MIN, i32::MIN),
            |(x0, y0, x1, y1), ls| {
                (
                    x0.min(ls.content_offset.0),
                    y0.min(ls.content_offset.1),
                    x1.max(ls.content_offset.0 + ls.content_w as i32),
                    y1.max(ls.content_offset.1 + ls.content_h as i32),
                )
            },
        );
        let orig_offset = (u_x0, u_y0);
        let orig_w = (u_x1 - u_x0).max(1) as u32;
        let orig_h = (u_y1 - u_y0).max(1) as u32;
        let pivot_cx = u_x0 as f32 + orig_w as f32 / 2.0;
        let pivot_cy = u_y0 as f32 + orig_h as f32 / 2.0;

        let layer_id = layer_states[0].layer_id;
        let layer_idx = layer_states[0].layer_idx;

        self.edit.transform_state = Some(TransformState {
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
            mode: crate::app::state::TransformMode::Free,
        });

        self.update_transform_preview();
        self.edit.tools.select(crate::tools::ToolId::Transform);
        let n = self
            .edit
            .transform_state
            .as_ref()
            .map(|ts| ts.layer_states.len())
            .unwrap_or(0);
        self.shell.status_msg = if n > 1 {
            format!("Free Transform ({n} layers) — corner=proportional • Shift+edge=distort • Alt=from-center • Enter=commit • Esc=cancel")
        } else {
            "Free Transform — corner=proportional • Shift+edge=distort • Alt=from-center • Enter/Esc".to_string()
        };
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    pub fn cancel_transform(&mut self) {
        if self.edit.transform_state.is_none() {
            return;
        }
        self.edit.transform_state = None;
        self.edit.transform_snap_guides.clear();
        self.clear_transform_preview();
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.edit.tools.select(crate::tools::ToolId::Move);
        self.shell.status_msg = "Transform cancelled".to_string();
    }

    /// Undo while Free Transform is live: revert the pending transform to
    /// identity instead of popping the document history underneath the
    /// session. Returns true when a transform session consumed the undo.
    pub fn transform_undo_pending(&mut self) -> bool {
        let Some(ts) = self.edit.transform_state.as_mut() else {
            return false;
        };
        let identity = ts.scale_x == 1.0
            && ts.scale_y == 1.0
            && ts.angle_deg == 0.0
            && ts.translate_x == 0.0
            && ts.translate_y == 0.0
            && ts.quad.is_none();
        if !identity {
            ts.scale_x = 1.0;
            ts.scale_y = 1.0;
            ts.angle_deg = 0.0;
            ts.translate_x = 0.0;
            ts.translate_y = 0.0;
            ts.drag_handle = None;
            ts.quad = None;
            ts.mode = crate::app::state::TransformMode::Free;
            self.update_transform_preview();
            self.recomposite_visible();
            self.shell.status_msg = "Transform reset".to_string();
        }
        true
    }

    pub fn commit_transform(&mut self) {
        let ts = match self.edit.transform_state.as_ref() {
            Some(t) => t.clone(),
            None => return,
        };

        if let Some((dx, dy)) = translate_only(&ts) {
            let Some(ts) = self.edit.transform_state.take() else {
                return;
            };
            self.edit.transform_snap_guides.clear();
            self.clear_transform_preview();

            if dx == 0 && dy == 0 {
                self.sync_compositor_viewport();
                self.recomposite();
                self.edit.tools.select(crate::tools::ToolId::Move);
                self.shell.status_msg = "Transform unchanged".to_string();
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
                return;
            }

            let layer_ids: Vec<u32> = ts.layer_states.iter().map(|ls| ls.layer_id).collect();
            {
                let layers = &mut self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers;
                for layer in layers.iter_mut() {
                    if layer_ids.contains(&layer.id) {
                        layer.offset.0 = layer.offset.0.saturating_add(dx);
                        layer.offset.1 = layer.offset.1.saturating_add(dy);
                    }
                }
            }
            let cmd = crate::core::command::TranslateLayerCommand::from_applied_move(
                layer_ids,
                dx,
                dy,
                &self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack,
            );
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .record(Box::new(cmd));
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            self.edit.tools.select(crate::tools::ToolId::Move);
            self.shell.status_msg = "Transform applied".to_string();
            return;
        }

        let Some(ts) = self.edit.transform_state.take() else {
            return;
        };
        self.edit.transform_snap_guides.clear();
        let doc_id = self.docs.documents[self.docs.active_doc_idx].id;
        let interpolation = self.shell.ui.transform_interpolation;
        let (tx, rx) = std::sync::mpsc::channel();
        self.edit.pending_transform_commit = Some(rx);
        self.edit.tools.select(crate::tools::ToolId::Move);
        self.shell.status_msg = "Applying transform...".to_string();
        rayon::spawn(move || {
            let result = bake_transform_commit(doc_id, ts, interpolation);
            let _ = tx.send(result);
        });
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    pub fn poll_transform_commit(&mut self) {
        let Some(rx) = self.edit.pending_transform_commit.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(result)) => {
                self.clear_transform_preview();
                if self.docs.documents[self.docs.active_doc_idx].id == result.doc_id {
                    for update in result.layers {
                        if let Some(layer) = self.docs.documents[self.docs.active_doc_idx]
                            .canvas
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
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .record(Box::new(result.command));
                    self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
                    self.shell.status_msg = "Transform applied".to_string();
                    if self.edit.warp_after_transform_commit {
                        self.edit.warp_after_transform_commit = false;
                        self.begin_warp();
                    }
                } else {
                    self.edit.warp_after_transform_commit = false;
                    self.shell.status_msg = "Transform finished for another document".to_string();
                }
            }
            Ok(Err(err)) => {
                self.edit.warp_after_transform_commit = false;
                self.clear_transform_preview();
                self.recomposite();
                self.shell.status_msg = err;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.edit.pending_transform_commit = Some(rx);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.edit.warp_after_transform_commit = false;
                self.clear_transform_preview();
                self.recomposite();
                self.shell.status_msg = "Transform worker stopped".to_string();
            }
        }
    }

    pub fn update_transform_preview(&mut self) {
        let previews: Vec<TransformPreviewUniform> = if let Some(ts) = &self.edit.transform_state {
            let Some(inv_m) = ts.inverse_homography() else {
                return;
            };
            ts.layer_states
                .iter()
                .chain(ts.preview_layer_states.iter())
                .map(|ls| TransformPreviewUniform {
                    layer_id: ls.layer_id,
                    inv_m,
                    orig_ox: ls.offset.0 as f32,
                    orig_oy: ls.offset.1 as f32,
                })
                .collect()
        } else {
            Vec::new()
        };

        if let Some(gpu) = &mut self.win.gpu {
            gpu.compositor.transform_previews = previews;
        }
    }

    fn clear_transform_preview(&mut self) {
        if let Some(gpu) = &mut self.win.gpu {
            gpu.compositor.transform_previews.clear();
        }
    }

    pub fn transform_on_press(
        &mut self,
        canvas_x: f32,
        canvas_y: f32,
        screen_x: f32,
        screen_y: f32,
    ) {
        let Some(ts) = self.edit.transform_state.as_mut() else {
            return;
        };
        let zoom = self.edit.view.zoom;
        let vox = self.edit.view.offset_x;
        let voy = self.edit.view.offset_y;
        let c2s = |cx: f32, cy: f32| (cx * zoom + vox, cy * zoom + voy);

        let handles = ts.handle_positions();
        let order = [
            TransformHandle::TopLeft,
            TransformHandle::TopCenter,
            TransformHandle::TopRight,
            TransformHandle::MiddleLeft,
            TransformHandle::MiddleRight,
            TransformHandle::BottomLeft,
            TransformHandle::BottomCenter,
            TransformHandle::BottomRight,
        ];
        for (i, &h) in order.iter().enumerate() {
            let (hcx, hcy) = handles[i];
            let (shx, shy) = c2s(hcx, hcy);
            if ((screen_x - shx).powi(2) + (screen_y - shy).powi(2)).sqrt() < HANDLE_RADIUS {
                ts.drag_handle = Some(Some(h));
                ts.drag_start_cx = canvas_x;
                ts.drag_start_cy = canvas_y;
                ts.drag_start_sx = ts.scale_x;
                ts.drag_start_sy = ts.scale_y;
                ts.drag_start_angle = ts.angle_deg;
                ts.drag_start_tx = ts.translate_x;
                ts.drag_start_ty = ts.translate_y;
                if let Some(q) = ts.quad {
                    ts.drag_start_quad = q;
                }
                return;
            }
        }

        let Some((orig_cx, orig_cy)) = ts.inverse_canvas_point(canvas_x, canvas_y) else {
            return;
        };
        let ox0 = ts.orig_offset.0 as f32;
        let oy0 = ts.orig_offset.1 as f32;
        let ox1 = ox0 + ts.orig_w as f32;
        let oy1 = oy0 + ts.orig_h as f32;
        let inside_bbox = orig_cx >= ox0 && orig_cx <= ox1 && orig_cy >= oy0 && orig_cy <= oy1;

        if inside_bbox {
            ts.drag_handle = Some(Some(TransformHandle::Center));
            ts.drag_start_cx = canvas_x;
            ts.drag_start_cy = canvas_y;
            ts.drag_start_tx = ts.translate_x;
            ts.drag_start_ty = ts.translate_y;
            if let Some(q) = ts.quad {
                ts.drag_start_quad = q;
            }
            return;
        }

        if ts.mode != crate::app::state::TransformMode::Free {
            return;
        }

        let rz = ROTATE_ZONE / zoom;
        let in_rotate = orig_cx >= ox0 - rz
            && orig_cx <= ox1 + rz
            && orig_cy >= oy0 - rz
            && orig_cy <= oy1 + rz;
        if in_rotate {
            ts.drag_handle = Some(None);
            ts.drag_start_cx = canvas_x;
            ts.drag_start_cy = canvas_y;
            ts.drag_start_angle = ts.angle_deg;
            ts.drag_start_tx = ts.translate_x;
            ts.drag_start_ty = ts.translate_y;
        }
    }

    /// Scale behavior:
    /// - Corner handles (TL/TR/BL/BR): always proportional, Shift has no effect.
    /// - Middle handles (TC/BC/ML/MR): proportional by default; Shift = distort along that axis only.
    /// - Rotate: Shift = snap to 15° steps.
    /// `alt` = scale from center pivot (no translate adjustment); otherwise opposite anchor is fixed.
    /// Build per-axis snap targets for Free Transform: canvas edges/center, ruler
    /// guides, and the content edges/centers of visible layers NOT being
    /// transformed.
    fn build_transform_snap_targets(&self) -> (Vec<(f32, SnapKind)>, Vec<(f32, SnapKind)>) {
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let w = canvas.width as f32;
        let h = canvas.height as f32;
        let mut tx = vec![
            (0.0, SnapKind::CanvasEdge),
            (w, SnapKind::CanvasEdge),
            (w * 0.5, SnapKind::CanvasCenter),
        ];
        let mut ty = vec![
            (0.0, SnapKind::CanvasEdge),
            (h, SnapKind::CanvasEdge),
            (h * 0.5, SnapKind::CanvasCenter),
        ];

        let xform_ids: Vec<u32> = self
            .edit
            .transform_state
            .as_ref()
            .map(|ts| ts.layer_states.iter().map(|l| l.layer_id).collect())
            .unwrap_or_default();

        for layer in canvas.layer_stack.layers.iter() {
            if !layer.visible || xform_ids.contains(&layer.id) {
                continue;
            }
            if layer.width == 0 || layer.height == 0 {
                continue;
            }
            let lx0 = layer.offset.0 as f32;
            let ly0 = layer.offset.1 as f32;
            let lx1 = lx0 + layer.width as f32;
            let ly1 = ly0 + layer.height as f32;
            tx.push((lx0, SnapKind::LayerEdge));
            tx.push((lx1, SnapKind::LayerEdge));
            tx.push(((lx0 + lx1) * 0.5, SnapKind::LayerCenter));
            ty.push((ly0, SnapKind::LayerEdge));
            ty.push((ly1, SnapKind::LayerEdge));
            ty.push(((ly0 + ly1) * 0.5, SnapKind::LayerCenter));
        }

        for g in &self.docs.documents[self.docs.active_doc_idx].guides {
            match g.orientation {
                GuideOrientation::Vertical => tx.push((g.pos, SnapKind::Guide)),
                GuideOrientation::Horizontal => ty.push((g.pos, SnapKind::Guide)),
            }
        }
        (tx, ty)
    }

    pub fn transform_on_drag(&mut self, canvas_x: f32, canvas_y: f32, shift: bool, alt: bool) {
        // Snap inputs need &self → gather before borrowing transform_state mutably.
        let snap_enabled = self.shell.ui.snap_enabled;
        let zoom = self.edit.view.zoom;
        let needs_snap_targets = snap_enabled
            && self
                .edit
                .transform_state
                .as_ref()
                .and_then(|ts| ts.drag_handle)
                .flatten()
                .is_some();
        let (tx_targets, ty_targets) = if needs_snap_targets {
            self.build_transform_snap_targets()
        } else {
            (Vec::new(), Vec::new())
        };
        let threshold = SNAP_THRESHOLD_PX / zoom.max(1e-4);
        self.edit.transform_snap_guides.clear();
        let mut guides: Vec<SnapLine> = Vec::new();

        let projective_dragged = {
            let Some(ts) = self.edit.transform_state.as_mut() else {
                return;
            };
            if ts.mode == crate::app::state::TransformMode::Free || ts.quad.is_none() {
                false
            } else if let Some(Some(h)) = ts.drag_handle {
                let candidate = dragged_projective_quad(
                    ts.drag_start_quad,
                    h,
                    ts.mode,
                    canvas_x - ts.drag_start_cx,
                    canvas_y - ts.drag_start_cy,
                );
                let valid = crate::core::geometry::Homography::square_to_quad(
                    candidate.map(|(x, y)| crate::core::geometry::Point::new(x, y)),
                )
                .and_then(|m| m.inverse())
                .is_some();
                if valid {
                    ts.quad = Some(candidate);
                }
                true
            } else {
                false
            }
        };
        if projective_dragged {
            self.update_transform_preview();
            self.request_interactive_recompose();
            return;
        }

        {
            let Some(ts) = self.edit.transform_state.as_mut() else {
                return;
            };
            // Snapping is only meaningful while the box is axis-aligned.
            let angle0 = {
                let a = ts.angle_deg.rem_euclid(360.0);
                a < 0.5 || a > 359.5
            };

            match ts.drag_handle {
                None => {
                    return;
                }
                Some(None) => {
                    let eff_cx = ts.pivot_cx + ts.drag_start_tx;
                    let eff_cy = ts.pivot_cy + ts.drag_start_ty;
                    let start_a = (ts.drag_start_cy - eff_cy).atan2(ts.drag_start_cx - eff_cx);
                    let cur_a = (canvas_y - eff_cy).atan2(canvas_x - eff_cx);
                    let new_angle = ts.drag_start_angle + (cur_a - start_a).to_degrees();
                    ts.angle_deg = if shift {
                        (new_angle / 15.0).round() * 15.0
                    } else {
                        new_angle
                    };
                }
                Some(Some(TransformHandle::Center)) => {
                    ts.translate_x = ts.drag_start_tx + (canvas_x - ts.drag_start_cx);
                    ts.translate_y = ts.drag_start_ty + (canvas_y - ts.drag_start_cy);

                    // Snap the moving bbox edges/center to targets (like the Move tool).
                    if snap_enabled && angle0 {
                        let c = ts.corners();
                        let l = c.iter().map(|p| p.0).fold(f32::INFINITY, f32::min);
                        let r = c.iter().map(|p| p.0).fold(f32::NEG_INFINITY, f32::max);
                        let t = c.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
                        let b = c.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max);
                        if let Some((adj, pos, kind)) =
                            best_snap(&[l, (l + r) * 0.5, r], &tx_targets, threshold)
                        {
                            ts.translate_x += adj;
                            guides.push(SnapLine {
                                vertical: true,
                                pos,
                                kind,
                            });
                        }
                        if let Some((adj, pos, kind)) =
                            best_snap(&[t, (t + b) * 0.5, b], &ty_targets, threshold)
                        {
                            ts.translate_y += adj;
                            guides.push(SnapLine {
                                vertical: false,
                                pos,
                                kind,
                            });
                        }
                    }
                }
                Some(Some(h)) => {
                    let rad = ts.drag_start_angle.to_radians();
                    let cos_a = rad.cos();
                    let sin_a = rad.sin();
                    let (lx, ly) = ts.handle_local(h);
                    let target_x = canvas_x - ts.pivot_cx - ts.drag_start_tx;
                    let target_y = canvas_y - ts.pivot_cy - ts.drag_start_ty;
                    let along_x = cos_a * target_x + sin_a * target_y;
                    let along_y = -sin_a * target_x + cos_a * target_y;

                    let upd_x = lx.abs() > 1.0;
                    let upd_y = ly.abs() > 1.0;

                    // Non-Alt drags keep the opposite handle fixed. Compute each
                    // axis from that anchor, not from the center pivot; translation
                    // below then preserves the anchor without doubling the drag.
                    let anchor_lx = if alt { 0.0 } else { -lx };
                    let anchor_ly = if alt { 0.0 } else { -ly };

                    let raw_sx = if upd_x {
                        (along_x - ts.drag_start_sx * anchor_lx) / (lx - anchor_lx)
                    } else {
                        ts.drag_start_sx
                    };
                    let raw_sy = if upd_y {
                        (along_y - ts.drag_start_sy * anchor_ly) / (ly - anchor_ly)
                    } else {
                        ts.drag_start_sy
                    };

                    match (upd_x, upd_y) {
                        (true, true) => {
                            // Project onto the original corner diagonal. Besides
                            // keeping the handle with the cursor, this avoids the
                            // x/y dominance switch that caused visible jitter.
                            let ratio = corner_drag_ratio(
                                along_x,
                                along_y,
                                lx,
                                ly,
                                ts.drag_start_sx,
                                ts.drag_start_sy,
                                alt,
                            );
                            ts.scale_x = ts.drag_start_sx * ratio;
                            ts.scale_y = ts.drag_start_sy * ratio;
                        }
                        (true, false) => {
                            ts.scale_x = raw_sx;
                            if shift {
                            } else {
                                ts.scale_y = ts.drag_start_sy * (raw_sx / ts.drag_start_sx);
                            }
                        }
                        (false, true) => {
                            ts.scale_y = raw_sy;
                            if shift {
                            } else {
                                ts.scale_x = ts.drag_start_sx * (raw_sy / ts.drag_start_sy);
                            }
                        }
                        _ => {}
                    }

                    ts.scale_x = clamp_live_scale(ts.scale_x, ts.drag_start_sx);
                    ts.scale_y = clamp_live_scale(ts.scale_y, ts.drag_start_sy);

                    if !alt {
                        let opp_lx = -lx;
                        let opp_ly = -ly;
                        let sx_new = ts.scale_x;
                        let sy_new = ts.scale_y;
                        ts.translate_x = ts.drag_start_tx
                            + cos_a * (ts.drag_start_sx - sx_new) * opp_lx
                            - sin_a * (ts.drag_start_sy - sy_new) * opp_ly;
                        ts.translate_y = ts.drag_start_ty
                            + sin_a * (ts.drag_start_sx - sx_new) * opp_lx
                            + cos_a * (ts.drag_start_sy - sy_new) * opp_ly;
                    }

                    // Snap pass: nudge the scale so the *actual* moving edge lands on a
                    // target (canvas edge/center, guide, other layer), keeping the same
                    // fixed anchor. Operates on the real edge position — accurate &
                    // responsive — instead of the cursor (which doesn't equal the edge).
                    if snap_enabled && angle0 {
                        let pcx = ts.pivot_cx;
                        let pcy = ts.pivot_cy;
                        let ox0 = ts.orig_offset.0 as f32;
                        let oy0 = ts.orig_offset.1 as f32;
                        let ox1 = ox0 + ts.orig_w as f32;
                        let oy1 = oy0 + ts.orig_h as f32;

                        let move_right = matches!(
                            h,
                            TransformHandle::TopRight
                                | TransformHandle::MiddleRight
                                | TransformHandle::BottomRight
                        );
                        let move_left = matches!(
                            h,
                            TransformHandle::TopLeft
                                | TransformHandle::MiddleLeft
                                | TransformHandle::BottomLeft
                        );
                        let move_top = matches!(
                            h,
                            TransformHandle::TopLeft
                                | TransformHandle::TopCenter
                                | TransformHandle::TopRight
                        );
                        let move_bottom = matches!(
                            h,
                            TransformHandle::BottomLeft
                                | TransformHandle::BottomCenter
                                | TransformHandle::BottomRight
                        );
                        let is_corner = matches!(
                            h,
                            TransformHandle::TopLeft
                                | TransformHandle::TopRight
                                | TransformHandle::BottomLeft
                                | TransformHandle::BottomRight
                        );
                        // Corners are always proportional; middle handles are proportional
                        // unless Shift (distort one axis).
                        let proportional = is_corner || !shift;

                        // Fixed anchor per axis: the opposite edge (far-side anchor) when
                        // this axis is dragged and not Alt, otherwise the center.
                        let ax_orig = if (move_left || move_right) && !alt {
                            if move_right {
                                ox0
                            } else {
                                ox1
                            }
                        } else {
                            pcx
                        };
                        let ay_orig = if (move_top || move_bottom) && !alt {
                            if move_bottom {
                                oy0
                            } else {
                                oy1
                            }
                        } else {
                            pcy
                        };
                        let a_x = pcx + ts.translate_x + ts.scale_x * (ax_orig - pcx);
                        let a_y = pcy + ts.translate_y + ts.scale_y * (ay_orig - pcy);

                        // cand = (dist, ratio, target_pos, kind)
                        let cand_x = if move_left || move_right {
                            let mo = if move_right { ox1 } else { ox0 };
                            let e = pcx + ts.translate_x + ts.scale_x * (mo - pcx);
                            best_snap(&[e], &tx_targets, threshold).and_then(|(adj, pos, kind)| {
                                let denom = e - a_x;
                                (denom.abs() > 1e-3)
                                    .then(|| (adj.abs(), (pos - a_x) / denom, pos, kind))
                            })
                        } else {
                            None
                        };
                        let cand_y = if move_top || move_bottom {
                            let mo = if move_bottom { oy1 } else { oy0 };
                            let e = pcy + ts.translate_y + ts.scale_y * (mo - pcy);
                            best_snap(&[e], &ty_targets, threshold).and_then(|(adj, pos, kind)| {
                                let denom = e - a_y;
                                (denom.abs() > 1e-3)
                                    .then(|| (adj.abs(), (pos - a_y) / denom, pos, kind))
                            })
                        } else {
                            None
                        };

                        let chosen = match (cand_x, cand_y) {
                            (Some(cx), Some(cy)) => {
                                if cx.0 <= cy.0 {
                                    Some((true, cx))
                                } else {
                                    Some((false, cy))
                                }
                            }
                            (Some(cx), None) => Some((true, cx)),
                            (None, Some(cy)) => Some((false, cy)),
                            (None, None) => None,
                        };

                        if let Some((is_x, (_dist, ratio, pos, kind))) = chosen {
                            if proportional {
                                ts.scale_x *= ratio;
                                ts.scale_y *= ratio;
                            } else if is_x {
                                ts.scale_x *= ratio;
                            } else {
                                ts.scale_y *= ratio;
                            }
                            if proportional || is_x {
                                ts.translate_x = a_x - pcx - ts.scale_x * (ax_orig - pcx);
                            }
                            if proportional || !is_x {
                                ts.translate_y = a_y - pcy - ts.scale_y * (ay_orig - pcy);
                            }
                            guides.push(SnapLine {
                                vertical: is_x,
                                pos,
                                kind,
                            });
                        }
                    }
                }
            }
        }

        self.edit.transform_snap_guides = guides;
        self.update_transform_preview();
        self.request_interactive_recompose();
    }

    pub fn transform_on_release(&mut self) {
        if let Some(ts) = self.edit.transform_state.as_mut() {
            ts.drag_handle = None;
        }
        self.edit.transform_snap_guides.clear();
    }

    pub fn transform_set_scale_x(&mut self, v: f32) {
        if let Some(ts) = self.edit.transform_state.as_mut() {
            let next = if v < 0.0 {
                v.min(-MIN_LIVE_SCALE)
            } else {
                v.max(MIN_LIVE_SCALE)
            };
            if let Some(q) = ts.quad.as_mut() {
                transform_quad_about_center(q, next / ts.scale_x, 1.0, 0.0);
            }
            ts.scale_x = next;
        }
        self.update_transform_preview();
        self.recomposite_visible();
    }
    pub fn transform_set_scale_y(&mut self, v: f32) {
        if let Some(ts) = self.edit.transform_state.as_mut() {
            let next = if v < 0.0 {
                v.min(-MIN_LIVE_SCALE)
            } else {
                v.max(MIN_LIVE_SCALE)
            };
            if let Some(q) = ts.quad.as_mut() {
                transform_quad_about_center(q, 1.0, next / ts.scale_y, 0.0);
            }
            ts.scale_y = next;
        }
        self.update_transform_preview();
        self.recomposite_visible();
    }
    pub fn transform_set_angle(&mut self, deg: f32) {
        if let Some(ts) = self.edit.transform_state.as_mut() {
            if let Some(q) = ts.quad.as_mut() {
                transform_quad_about_center(q, 1.0, 1.0, deg - ts.angle_deg);
            }
            ts.angle_deg = deg;
        }
        self.update_transform_preview();
        self.recomposite_visible();
    }
    pub fn transform_set_translate_x(&mut self, v: f32) {
        if let Some(ts) = self.edit.transform_state.as_mut() {
            if let Some(q) = ts.quad.as_mut() {
                let dx = v - ts.translate_x;
                for p in q {
                    p.0 += dx;
                }
            }
            ts.translate_x = v;
        }
        self.update_transform_preview();
        self.recomposite_visible();
    }
    pub fn transform_set_translate_y(&mut self, v: f32) {
        if let Some(ts) = self.edit.transform_state.as_mut() {
            if let Some(q) = ts.quad.as_mut() {
                let dy = v - ts.translate_y;
                for p in q {
                    p.1 += dy;
                }
            }
            ts.translate_y = v;
        }
        self.update_transform_preview();
        self.recomposite_visible();
    }

    /// Restore the live transform to the state in which this session began.
    pub fn transform_reset(&mut self) {
        let Some(ts) = self.edit.transform_state.as_mut() else {
            return;
        };
        ts.scale_x = 1.0;
        ts.scale_y = 1.0;
        ts.angle_deg = 0.0;
        ts.translate_x = 0.0;
        ts.translate_y = 0.0;
        ts.drag_handle = None;
        ts.quad = None;
        ts.mode = crate::app::state::TransformMode::Free;
        self.edit.transform_snap_guides.clear();
        self.update_transform_preview();
        self.recomposite_visible();
        self.shell.status_msg = "Free Transform reset".to_string();
    }

    pub fn transform_set_mode(&mut self, mode: crate::app::state::TransformMode) {
        let Some(ts) = self.edit.transform_state.as_mut() else {
            return;
        };
        if mode != crate::app::state::TransformMode::Free && ts.quad.is_none() {
            let c = ts.corners();
            ts.quad = Some([c[0], c[1], c[3], c[2]]);
        }
        ts.mode = mode;
        ts.drag_handle = None;
        self.edit.transform_snap_guides.clear();
        self.update_transform_preview();
        self.recomposite_visible();
        self.shell.status_msg = format!("Free Transform — {mode:?}");
    }

    /// Warp uses the existing mesh editor. The pending transform is committed
    /// first when necessary; an unchanged transform can switch immediately.
    pub fn transform_start_warp(&mut self) {
        let unchanged = self.edit.transform_state.as_ref().is_some_and(|ts| {
            ts.quad.is_none()
                && ts.scale_x == 1.0
                && ts.scale_y == 1.0
                && ts.angle_deg == 0.0
                && ts.translate_x == 0.0
                && ts.translate_y == 0.0
        });
        if unchanged {
            self.cancel_transform();
            self.begin_warp();
        } else {
            self.edit.warp_after_transform_commit = true;
            self.commit_transform();
            if self.edit.pending_transform_commit.is_none() {
                self.edit.warp_after_transform_commit = false;
                self.begin_warp();
            }
        }
    }

    pub fn transform_cursor_hint(&self) -> u8 {
        let Some(ts) = &self.edit.transform_state else {
            return 0;
        };
        let zoom = self.edit.view.zoom;
        let vox = self.edit.view.offset_x;
        let voy = self.edit.view.offset_y;
        let mx = self.edit.input.mouse_x;
        let my = self.edit.input.mouse_y;
        let c2s = |cx: f32, cy: f32| -> (f32, f32) { (cx * zoom + vox, cy * zoom + voy) };

        let handles = ts.handle_positions();
        for (i, &(hcx, hcy)) in handles.iter().enumerate() {
            let (shx, shy) = c2s(hcx, hcy);
            if ((mx - shx).powi(2) + (my - shy).powi(2)).sqrt() < HANDLE_RADIUS {
                let local_dirs = [
                    (1.0, 1.0),  // TL
                    (0.0, 1.0),  // TC
                    (-1.0, 1.0), // TR
                    (1.0, 0.0),  // ML
                    (1.0, 0.0),  // MR
                    (-1.0, 1.0), // BL
                    (0.0, 1.0),  // BC
                    (1.0, 1.0),  // BR
                ];
                let (lx, ly) = local_dirs[i];
                let sx = lx * ts.scale_x.signum();
                let sy = ly * ts.scale_y.signum();
                let rad = ts.angle_deg.to_radians();
                let c = rad.cos();
                let s = rad.sin();
                let screen_dx = c * sx - s * sy;
                let screen_dy = s * sx + c * sy;

                let mut angle = screen_dy.atan2(screen_dx).to_degrees();
                while angle < 0.0 {
                    angle += 180.0;
                }
                while angle >= 180.0 {
                    angle -= 180.0;
                }

                let idx = ((angle / 45.0).round() as i32).rem_euclid(4);
                return 10 + idx as u8;
            }
        }

        let cx = (mx - vox) / zoom;
        let cy = (my - voy) / zoom;
        let Some((orig_cx, orig_cy)) = ts.inverse_canvas_point(cx, cy) else {
            return 0;
        };
        let ox0 = ts.orig_offset.0 as f32;
        let oy0 = ts.orig_offset.1 as f32;
        let ox1 = ox0 + ts.orig_w as f32;
        let oy1 = oy0 + ts.orig_h as f32;
        if orig_cx >= ox0 && orig_cx <= ox1 && orig_cy >= oy0 && orig_cy <= oy1 {
            return 0;
        }

        let rz = ROTATE_ZONE / zoom;
        if orig_cx >= ox0 - rz && orig_cx <= ox1 + rz && orig_cy >= oy0 - rz && orig_cy <= oy1 + rz
        {
            return 1;
        }

        0
    }

    pub fn transform_flip_horizontal(&mut self) {
        if let Some(ts) = self.edit.transform_state.as_mut() {
            if let Some(q) = ts.quad.as_mut() {
                transform_quad_about_center(q, -1.0, 1.0, 0.0);
            }
            ts.scale_x *= -1.0;
        }
        self.update_transform_preview();
        self.recomposite_visible();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
    pub fn transform_flip_vertical(&mut self) {
        if let Some(ts) = self.edit.transform_state.as_mut() {
            if let Some(q) = ts.quad.as_mut() {
                transform_quad_about_center(q, 1.0, -1.0, 0.0);
            }
            ts.scale_y *= -1.0;
        }
        self.update_transform_preview();
        self.recomposite_visible();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
    pub fn transform_rotate_90cw(&mut self) {
        if let Some(ts) = self.edit.transform_state.as_mut() {
            if let Some(q) = ts.quad.as_mut() {
                transform_quad_about_center(q, 1.0, 1.0, 90.0);
            }
            ts.angle_deg += 90.0;
        }
        self.update_transform_preview();
        self.recomposite_visible();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
    pub fn transform_rotate_90ccw(&mut self) {
        if let Some(ts) = self.edit.transform_state.as_mut() {
            if let Some(q) = ts.quad.as_mut() {
                transform_quad_about_center(q, 1.0, 1.0, -90.0);
            }
            ts.angle_deg -= 90.0;
        }
        self.update_transform_preview();
        self.recomposite_visible();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
    pub fn transform_rotate_180(&mut self) {
        if let Some(ts) = self.edit.transform_state.as_mut() {
            if let Some(q) = ts.quad.as_mut() {
                transform_quad_about_center(q, 1.0, 1.0, 180.0);
            }
            ts.angle_deg += 180.0;
        }
        self.update_transform_preview();
        self.recomposite_visible();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_transform_corner_tracks_pointer_without_doubling_drag() {
        let mut app = crate::app::state::App::new();
        app.shell.ui.snap_enabled = false;
        app.edit.transform_state = Some(TransformState {
            layer_states: Vec::new(),
            preview_layer_states: Vec::new(),
            layer_idx: 0,
            layer_id: 1,
            orig_offset: (0, 0),
            orig_w: 100,
            orig_h: 100,
            scale_x: 1.0,
            scale_y: 1.0,
            angle_deg: 0.0,
            translate_x: 0.0,
            translate_y: 0.0,
            pivot_cx: 50.0,
            pivot_cy: 50.0,
            drag_handle: Some(Some(TransformHandle::TopLeft)),
            drag_start_cx: 0.0,
            drag_start_cy: 0.0,
            drag_start_sx: 1.0,
            drag_start_sy: 1.0,
            drag_start_angle: 0.0,
            drag_start_tx: 0.0,
            drag_start_ty: 0.0,
            quad: None,
            drag_start_quad: [(0.0, 0.0); 4],
            mode: crate::app::state::TransformMode::Free,
        });

        app.transform_on_drag(-10.0, -10.0, false, false);

        let handles = app
            .edit
            .transform_state
            .as_ref()
            .expect("transform remains active")
            .handle_positions();
        assert!((handles[0].0 + 10.0).abs() < 0.001);
        assert!((handles[0].1 + 10.0).abs() < 0.001);
        assert!((handles[7].0 - 100.0).abs() < 0.001);
        assert!((handles[7].1 - 100.0).abs() < 0.001);
    }

    #[test]
    fn undo_during_free_transform_reverts_pending_only() {
        let mut app = crate::app::state::App::new();
        let undo_before = app.docs.documents[0].canvas.undo_count();
        app.edit.transform_state = Some(TransformState {
            layer_states: Vec::new(),
            preview_layer_states: Vec::new(),
            layer_idx: 0,
            layer_id: 1,
            orig_offset: (0, 0),
            orig_w: 4,
            orig_h: 4,
            scale_x: 2.0,
            scale_y: 0.5,
            angle_deg: 45.0,
            translate_x: 10.0,
            translate_y: -3.0,
            pivot_cx: 2.0,
            pivot_cy: 2.0,
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
            mode: crate::app::state::TransformMode::Free,
        });

        assert!(app.transform_undo_pending(), "session consumes the undo");
        let ts = app
            .edit
            .transform_state
            .as_ref()
            .expect("session stays open");
        assert_eq!(
            (
                ts.scale_x,
                ts.scale_y,
                ts.angle_deg,
                ts.translate_x,
                ts.translate_y
            ),
            (1.0, 1.0, 0.0, 0.0, 0.0),
            "pending transform reverts to identity"
        );
        assert!(
            app.transform_undo_pending(),
            "undo stays consumed at identity"
        );
        assert_eq!(
            app.docs.documents[0].canvas.undo_count(),
            undo_before,
            "document history is untouched"
        );
    }
    use crate::core::command::{Command, EditContext};
    use crate::core::document::DocumentId;
    use crate::core::layer::{LayerMask, LayerStack};
    use crate::core::text::{rasterize, TextData, TextFontFamily};
    use crate::core::tile::TileMap;

    #[test]
    fn bake_resamples_mask_with_layer() {
        // 2x2 opaque layer; mask: left column black, right column white.
        let pixels = [255u8, 0, 0, 255].repeat(4);
        let mask_px: Vec<u8> = [
            [0u8, 0, 0, 255],
            [255, 255, 255, 255],
            [0, 0, 0, 255],
            [255, 255, 255, 255],
        ]
        .concat();
        let ls = LayerOrigState {
            layer_id: 7,
            layer_idx: 0,
            layer_type: LayerType::Raster,
            tiles: TileMap::from_rgba(&pixels, 2, 2),
            mask: Some(LayerMask {
                tiles: TileMap::from_rgba(&mask_px, 2, 2),
                width: 2,
                height: 2,
                enabled: true,
                inverted: false,
            }),
            offset: (0, 0),
            width: 2,
            height: 2,
            content_offset: (0, 0),
            content_w: 2,
            content_h: 2,
        };
        let ts = TransformState {
            layer_states: vec![ls],
            preview_layer_states: Vec::new(),
            layer_idx: 0,
            layer_id: 7,
            orig_offset: (0, 0),
            orig_w: 2,
            orig_h: 2,
            scale_x: 2.0,
            scale_y: 2.0,
            angle_deg: 0.0,
            translate_x: 0.0,
            translate_y: 0.0,
            pivot_cx: 1.0,
            pivot_cy: 1.0,
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
            mode: crate::app::state::TransformMode::Free,
        };

        let result = bake_transform_commit(
            DocumentId(1),
            ts,
            crate::core::geometry::InterpolationMode::Bilinear,
        )
        .expect("bake succeeds");

        assert_eq!(result.layers.len(), 1);
        let layer = &result.layers[0];
        assert_eq!((layer.width, layer.height), (4, 4));
        let mask = layer.mask.as_ref().expect("mask survives the transform");
        assert_eq!((mask.width, mask.height), (4, 4));
        assert!(mask.enabled);
        // 2x scale keeps left side black, right side white.
        assert!(mask.tiles.get_pixel(0, 1).0 < 10, "left edge stays black");
        assert!(mask.tiles.get_pixel(3, 1).0 > 245, "right edge stays white");
    }

    #[test]
    fn text_scale_updates_editable_font_metadata() {
        let td = TextData {
            content: "Scale".to_string(),
            font_family: TextFontFamily::DejaVuSans,
            font_px: 24.0,
            ..TextData::default()
        };
        let Some(raster) = rasterize(&td) else {
            return;
        };
        let tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
        let Some((min_x, min_y, max_x, max_y)) = tiles.content_bounds() else {
            return;
        };
        let offset = (12, 17);
        let content_w = (max_x - min_x).max(1) as u32;
        let content_h = (max_y - min_y).max(1) as u32;
        let content_offset = (offset.0 + min_x, offset.1 + min_y);
        let ls = LayerOrigState {
            layer_id: 7,
            layer_idx: 0,
            layer_type: LayerType::Text(td.clone()),
            tiles: tiles.clone(),
            mask: None,
            offset,
            width: raster.width,
            height: raster.height,
            content_offset,
            content_w,
            content_h,
        };
        let ts = TransformState {
            layer_states: vec![ls],
            preview_layer_states: Vec::new(),
            layer_idx: 0,
            layer_id: 7,
            orig_offset: content_offset,
            orig_w: content_w,
            orig_h: content_h,
            scale_x: 2.0,
            scale_y: 1.0,
            angle_deg: 0.0,
            translate_x: 0.0,
            translate_y: 0.0,
            pivot_cx: content_offset.0 as f32 + content_w as f32 * 0.5,
            pivot_cy: content_offset.1 as f32 + content_h as f32 * 0.5,
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
            mode: crate::app::state::TransformMode::Free,
        };

        let result = bake_transform_commit(
            DocumentId(1),
            ts,
            crate::core::geometry::InterpolationMode::Bilinear,
        )
        .expect("bake succeeds");

        let LayerType::Text(after_td) = &result.layers[0].layer_type else {
            panic!("scaled text stays editable");
        };
        assert!((after_td.font_px - 24.0).abs() < 0.01);
        assert!((after_td.stretch_x - 2.0).abs() < 0.01);
        assert!(result.layers[0].width > raster.width);

        let mut stack = LayerStack::new(160, 120);
        let idx = stack.add_layer(160, 120);
        stack.layers[idx].id = 7;
        stack.layers[idx].layer_type = LayerType::Text(td.clone());
        stack.layers[idx].tiles = tiles;
        stack.layers[idx].offset = offset;
        stack.layers[idx].width = raster.width;
        stack.layers[idx].height = raster.height;

        let mut command = result.command;
        let mut canvas_w = 160;
        let mut canvas_h = 120;
        let mut ctx = EditContext::new(&mut stack, &mut canvas_w, &mut canvas_h, None);
        command.execute(&mut ctx).expect("redo applies");
        let LayerType::Text(redo_td) = &ctx.layers.layers[idx].layer_type else {
            panic!("redo keeps text layer");
        };
        assert!((redo_td.font_px - 24.0).abs() < 0.01);
        assert!((redo_td.stretch_x - 2.0).abs() < 0.01);

        command.undo(&mut ctx).expect("undo applies");
        let LayerType::Text(undo_td) = &ctx.layers.layers[idx].layer_type else {
            panic!("undo keeps text layer");
        };
        assert!((undo_td.font_px - 24.0).abs() < 0.01);
        assert!((undo_td.stretch_x - 1.0).abs() < 0.01);
    }

    #[test]
    fn text_rotation_keeps_editable_text_metadata() {
        let td = TextData {
            content: "Turn".to_string(),
            font_family: TextFontFamily::DejaVuSans,
            font_px: 24.0,
            ..TextData::default()
        };
        let Some(raster) = rasterize(&td) else {
            return;
        };
        let tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
        let Some((min_x, min_y, max_x, max_y)) = tiles.content_bounds() else {
            return;
        };
        let offset = (10, 10);
        let content_w = (max_x - min_x).max(1) as u32;
        let content_h = (max_y - min_y).max(1) as u32;
        let content_offset = (offset.0 + min_x, offset.1 + min_y);
        let ls = LayerOrigState {
            layer_id: 3,
            layer_idx: 0,
            layer_type: LayerType::Text(td.clone()),
            tiles,
            mask: None,
            offset,
            width: raster.width,
            height: raster.height,
            content_offset,
            content_w,
            content_h,
        };
        let ts = TransformState {
            layer_states: vec![ls],
            preview_layer_states: Vec::new(),
            layer_idx: 0,
            layer_id: 3,
            orig_offset: content_offset,
            orig_w: content_w,
            orig_h: content_h,
            scale_x: 1.0,
            scale_y: 1.0,
            angle_deg: 30.0,
            translate_x: 0.0,
            translate_y: 0.0,
            pivot_cx: content_offset.0 as f32 + content_w as f32 * 0.5,
            pivot_cy: content_offset.1 as f32 + content_h as f32 * 0.5,
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
            mode: crate::app::state::TransformMode::Free,
        };

        let result = bake_transform_commit(
            DocumentId(1),
            ts,
            crate::core::geometry::InterpolationMode::Bilinear,
        )
        .expect("bake succeeds");

        let LayerType::Text(after_td) = &result.layers[0].layer_type else {
            panic!("rotated text stays editable");
        };
        assert!((after_td.rotation_deg - 30.0).abs() < 0.01);
    }

    #[test]
    fn text_flip_keeps_editable_text_metadata() {
        let mut td = TextData {
            content: "Flip".to_string(),
            font_family: TextFontFamily::DejaVuSans,
            font_px: 24.0,
            ..TextData::default()
        };
        td.glyph_styles = td
            .content
            .chars()
            .map(|_| crate::core::text::GlyphStyle {
                color: [20, 120, 220, 255],
                font_px: 30.0,
                font_family: td.font_family.clone(),
                bold: td.bold,
                italic: td.italic,
                underline: td.underline,
            })
            .collect();
        let Some(raster) = rasterize(&td) else {
            return;
        };
        let tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
        let Some((min_x, min_y, max_x, max_y)) = tiles.content_bounds() else {
            return;
        };
        let offset = (10, 10);
        let content_w = (max_x - min_x).max(1) as u32;
        let content_h = (max_y - min_y).max(1) as u32;
        let content_offset = (offset.0 + min_x, offset.1 + min_y);
        let ls = LayerOrigState {
            layer_id: 4,
            layer_idx: 0,
            layer_type: LayerType::Text(td.clone()),
            tiles,
            mask: None,
            offset,
            width: raster.width,
            height: raster.height,
            content_offset,
            content_w,
            content_h,
        };
        let ts = TransformState {
            layer_states: vec![ls],
            preview_layer_states: Vec::new(),
            layer_idx: 0,
            layer_id: 4,
            orig_offset: content_offset,
            orig_w: content_w,
            orig_h: content_h,
            scale_x: -1.0,
            scale_y: 1.0,
            angle_deg: 0.0,
            translate_x: 0.0,
            translate_y: 0.0,
            pivot_cx: content_offset.0 as f32 + content_w as f32 * 0.5,
            pivot_cy: content_offset.1 as f32 + content_h as f32 * 0.5,
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
            mode: crate::app::state::TransformMode::Free,
        };

        let result = bake_transform_commit(
            DocumentId(1),
            ts,
            crate::core::geometry::InterpolationMode::Bilinear,
        )
        .expect("bake succeeds");

        let LayerType::Text(after_td) = &result.layers[0].layer_type else {
            panic!("flipped text stays editable");
        };
        assert!(after_td.flip_x);
        assert!(!after_td.flip_y);
        assert_eq!(after_td.glyph_styles, td.glyph_styles);
    }

    /// Build a Shape layer's LayerOrigState from a canvas span, mirroring how
    /// shape_ops creates the layer (render → tiles at the span's offset).
    fn shape_orig_state(kind: ShapeKind, layer_id: u32) -> (ShapeData, LayerOrigState) {
        let (sd, off) = ShapeData::from_canvas_span(
            kind,
            10.0,
            10.0,
            50.0,
            40.0,
            6.0,
            true,
            [200, 30, 30, 255],
            2.0,
            [0, 0, 0, 255],
        );
        let raster = sd.render().expect("shape renders");
        let tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
        let (min_x, min_y, max_x, max_y) = tiles.content_bounds().expect("has content");
        let content_offset = (off.0 + min_x, off.1 + min_y);
        let content_w = (max_x - min_x).max(1) as u32;
        let content_h = (max_y - min_y).max(1) as u32;
        let ls = LayerOrigState {
            layer_id,
            layer_idx: 0,
            layer_type: LayerType::Vector(VectorGeometry::Primitive(sd.clone())),
            tiles,
            mask: None,
            offset: off,
            width: raster.width,
            height: raster.height,
            content_offset,
            content_w,
            content_h,
        };
        (sd, ls)
    }

    fn shape_transform_state(ls: LayerOrigState, sx: f32, sy: f32, angle: f32) -> TransformState {
        let orig_offset = ls.content_offset;
        let orig_w = ls.content_w;
        let orig_h = ls.content_h;
        TransformState {
            layer_states: vec![ls],
            preview_layer_states: Vec::new(),
            layer_idx: 0,
            layer_id: 9,
            orig_offset,
            orig_w,
            orig_h,
            scale_x: sx,
            scale_y: sy,
            angle_deg: angle,
            translate_x: 0.0,
            translate_y: 0.0,
            pivot_cx: orig_offset.0 as f32 + orig_w as f32 * 0.5,
            pivot_cy: orig_offset.1 as f32 + orig_h as f32 * 0.5,
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
            mode: crate::app::state::TransformMode::Free,
        }
    }

    #[test]
    fn shape_scale_updates_geometry_and_rerenders_crisp() {
        let (sd, ls) = shape_orig_state(ShapeKind::Rectangle, 9);
        let ts = shape_transform_state(ls, 2.0, 2.0, 0.0);
        let result = bake_transform_commit(
            DocumentId(1),
            ts,
            crate::core::geometry::InterpolationMode::Bilinear,
        )
        .expect("bake succeeds");

        let layer = &result.layers[0];
        let LayerType::Vector(VectorGeometry::Primitive(after)) = &layer.layer_type else {
            panic!("scaled shape stays editable");
        };
        // The 40×30 span doubles; geometry must follow the raster instead of
        // snapping back to the original size on the next handle/radius edit.
        let (x0, y0, x1, y1) = after.canvas_span(layer.offset);
        assert!(((x1 - x0).abs() - 80.0).abs() <= 1.0, "span w {}", x1 - x0);
        assert!(((y1 - y0).abs() - 60.0).abs() <= 1.0, "span h {}", y1 - y0);
        assert!((after.corner_radius - 2.0 * sd.corner_radius).abs() <= 0.5);
        assert!((after.stroke_width() - 2.0 * sd.stroke_width()).abs() <= 0.1);
        // Crisp vector re-render, not a blurry resample: raster covers the span.
        assert!(layer.width as f32 >= 80.0 && layer.height as f32 >= 60.0);
    }

    #[test]
    fn shape_rotation_rasterizes_rect_but_keeps_line() {
        let (_, rect_ls) = shape_orig_state(ShapeKind::Rectangle, 9);
        let ts = shape_transform_state(rect_ls, 1.0, 1.0, 45.0);
        let result = bake_transform_commit(
            DocumentId(1),
            ts,
            crate::core::geometry::InterpolationMode::Bilinear,
        )
        .expect("bake succeeds");
        assert!(
            matches!(result.layers[0].layer_type, LayerType::Raster),
            "a rotated rectangle can't stay a live axis-aligned shape"
        );

        let (line_sd, line_ls) = shape_orig_state(ShapeKind::Line, 9);
        let offset = line_ls.offset;
        let ts = shape_transform_state(line_ls, 1.0, 1.0, 45.0);
        let expect_p0 =
            ts.transform_point(line_sd.x0 + offset.0 as f32, line_sd.y0 + offset.1 as f32);
        let expect_p1 =
            ts.transform_point(line_sd.x1 + offset.0 as f32, line_sd.y1 + offset.1 as f32);
        let result = bake_transform_commit(
            DocumentId(1),
            ts,
            crate::core::geometry::InterpolationMode::Bilinear,
        )
        .expect("bake succeeds");
        let layer = &result.layers[0];
        let LayerType::Vector(VectorGeometry::Primitive(after)) = &layer.layer_type else {
            panic!("a rotated line stays a live shape");
        };
        assert_eq!(after.kind, ShapeKind::Line);
        let (x0, y0, x1, y1) = after.canvas_span(layer.offset);
        assert!((x0 - expect_p0.0).abs() <= 1.0 && (y0 - expect_p0.1).abs() <= 1.0);
        assert!((x1 - expect_p1.0).abs() <= 1.0 && (y1 - expect_p1.1).abs() <= 1.0);
    }

    #[test]
    fn shape_scale_with_mask_keeps_geometry_in_sync_with_resample() {
        let (_, mut ls) = shape_orig_state(ShapeKind::Rectangle, 9);
        // All-white enabled mask so the resample path is taken but nothing hides.
        let mw = ls.width;
        let mh = ls.height;
        let white = vec![255u8; (mw * mh * 4) as usize];
        ls.mask = Some(LayerMask {
            tiles: TileMap::from_rgba(&white, mw, mh),
            width: mw,
            height: mh,
            enabled: true,
            inverted: false,
        });
        let sd = match &ls.layer_type {
            LayerType::Vector(VectorGeometry::Primitive(sd)) => sd.clone(),
            _ => unreachable!(),
        };
        let ts = shape_transform_state(ls, 2.0, 2.0, 0.0);
        let result = bake_transform_commit(
            DocumentId(1),
            ts,
            crate::core::geometry::InterpolationMode::Bilinear,
        )
        .expect("bake succeeds");
        let layer = &result.layers[0];
        let LayerType::Vector(VectorGeometry::Primitive(after)) = &layer.layer_type else {
            panic!("masked shape keeps editable geometry");
        };
        let (x0, y0, x1, y1) = after.canvas_span(layer.offset);
        assert!(((x1 - x0).abs() - 80.0).abs() <= 1.0);
        assert!(((y1 - y0).abs() - 60.0).abs() <= 1.0);
        assert!((after.corner_radius - 2.0 * sd.corner_radius).abs() <= 0.5);
        assert!(layer.mask.is_some(), "mask survives");
    }

    #[test]
    fn skew_moves_a_whole_edge_and_keeps_a_parallelogram() {
        let q = [(0.0, 0.0), (100.0, 0.0), (100.0, 80.0), (0.0, 80.0)];
        let out = dragged_projective_quad(
            q,
            TransformHandle::TopCenter,
            crate::app::state::TransformMode::Skew,
            15.0,
            9.0,
        );
        assert_eq!(out[0], (15.0, 0.0));
        assert_eq!(out[1], (115.0, 0.0));
        assert_eq!(out[2], q[2]);
        assert_eq!(out[3], q[3]);
    }

    #[test]
    fn distort_moves_only_the_selected_corner() {
        let q = [(0.0, 0.0), (100.0, 0.0), (100.0, 80.0), (0.0, 80.0)];
        let out = dragged_projective_quad(
            q,
            TransformHandle::TopRight,
            crate::app::state::TransformMode::Distort,
            -12.0,
            7.0,
        );
        assert_eq!(out[1], (88.0, 7.0));
        assert_eq!(out[0], q[0]);
        assert_eq!(out[2], q[2]);
        assert_eq!(out[3], q[3]);
    }

    #[test]
    fn perspective_corner_drag_moves_adjacent_edges_symmetrically() {
        let q = [(0.0, 0.0), (100.0, 0.0), (100.0, 80.0), (0.0, 80.0)];
        let out = dragged_projective_quad(
            q,
            TransformHandle::TopLeft,
            crate::app::state::TransformMode::Perspective,
            10.0,
            5.0,
        );
        assert_eq!(out[0], (10.0, 5.0));
        assert_eq!(out[3], (-10.0, 80.0));
        assert_eq!(out[1], (100.0, -5.0));
    }
}

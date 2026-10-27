//! PowerClip: objects placed inside a vector frame so they are clipped to its
//! shape, taken out again, and arranged in it (Center / Fit / Fill / Stretch).
//! The clip RELATION lives in `Layer::clip_parent_id`; the visible clip is a
//! managed mask baked by `Canvas::refresh_clip_masks`. These are the edits
//! alone: the caller records them as one undo step.

use crate::canvas::Canvas;
use iai_doc::layer::{Layer, LayerStack, LayerType};
use iai_vector::command_vector::{apply_object_to_layer, fold_offset_into_model};
use iai_vector::vector::affine::AffineTransform;
use iai_vector::vector::object::VectorGeometry;

/// How PowerClip content is positioned / scaled inside its frame (Corel-style).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Keep the content's size, centre it in the frame.
    Center,
    /// Scale proportionally so the whole content fits inside the frame, centred.
    Fit,
    /// Scale proportionally so the content covers the frame (overflow clipped).
    Fill,
    /// Scale X and Y independently so the content exactly fills the frame.
    Stretch,
}

/// A layer that can take part in a PowerClip: selected, not the background,
/// not locked, not a group.
fn is_clip_eligible(layer: &Layer) -> bool {
    layer.selected && !layer.locked && !layer.is_background && !layer.is_group()
}

fn is_vector(layer: &Layer) -> bool {
    matches!(layer.layer_type, LayerType::Vector(_))
}

/// The layer's opaque content rectangle in canvas space (`x, y, w, h`), or `None`
/// when it has no ink. Frame and content bounds are both taken this way, so the
/// arrange maths is uniform across raster photos and rendered vector shapes.
fn content_rect(layer: &Layer) -> Option<(f32, f32, f32, f32)> {
    let (x0, y0, x1, y1) = layer.tiles.content_bounds()?;
    let (w, h) = ((x1 - x0) as f32, (y1 - y0) as f32);
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    Some((
        layer.offset.0 as f32 + x0 as f32,
        layer.offset.1 as f32 + y0 as f32,
        w,
        h,
    ))
}

/// Scale a content layer by `(sx, sy)` about its content origin `(cx, cy)` and put
/// that origin at `(tx, ty)`. A Path stays crisp (the affine folds into its vector
/// geometry, then the raster is re-derived); any other layer resamples its pixels.
fn arrange_content_layer(layer: &mut Layer, cx: f32, cy: f32, sx: f32, sy: f32, tx: f32, ty: f32) {
    if let LayerType::Vector(VectorGeometry::Path(object)) = &layer.layer_type {
        // Canvas→canvas map: scale about (cx,cy) then translate the origin to (tx,ty).
        let a = AffineTransform {
            a: sx,
            b: 0.0,
            c: 0.0,
            d: sy,
            e: tx - cx * sx,
            f: ty - cy * sy,
        };
        let mut o = object.clone();
        o.transform = a.then(&o.transform);
        apply_object_to_layer(layer, o);
        return;
    }
    // Raster / text / primitive content: resample the whole tilemap and re-anchor
    // its opaque origin at the target.
    let (bx0, by0) = layer
        .tiles
        .content_bounds()
        .map(|(x0, y0, _, _)| (x0 as f32, y0 as f32))
        .unwrap_or((0.0, 0.0));
    let nw = ((layer.width as f32) * sx).round().max(1.0) as u32;
    let nh = ((layer.height as f32) * sy).round().max(1.0) as u32;
    layer.tiles = Canvas::resample_tilemap(&layer.tiles, nw, nh);
    layer.width = nw;
    layer.height = nh;
    layer.offset = (
        (tx - bx0 * sx).round() as i32,
        (ty - by0 * sy).round() as i32,
    );
}

/// The frame (the top-most selected vector) and its content (every other
/// eligible selected layer), or `None` when the selection can't form a clip.
pub fn frame_and_content(stack: &LayerStack) -> Option<(usize, Vec<usize>)> {
    let selected: Vec<usize> = stack
        .layers
        .iter()
        .enumerate()
        .filter(|(_, l)| is_clip_eligible(l))
        .map(|(i, _)| i)
        .collect();
    // The container is usually the shape drawn on top.
    let frame = *selected
        .iter()
        .rev()
        .find(|&&i| is_vector(&stack.layers[i]))?;
    let content: Vec<usize> = selected.into_iter().filter(|&i| i != frame).collect();
    if content.is_empty() {
        return None;
    }
    Some((frame, content))
}

/// Whether a selected layer is PowerClip content (Extract / arrange apply).
pub fn has_selected_content(stack: &LayerStack) -> bool {
    stack
        .layers
        .iter()
        .any(|l| l.selected && l.clip_parent_id.is_some())
}

/// Place the selected content inside the selected vector frame: move it
/// directly above the frame, link it as the frame's clip content (picked, the
/// top one active) and bake the clip. How many layers went in; `None` when the
/// selection has no frame and content.
pub fn place(canvas: &mut Canvas) -> Option<usize> {
    let (frame_idx, content_idx) = frame_and_content(&canvas.layer_stack)?;
    let stack = &mut canvas.layer_stack;
    let frame_id = stack.layers[frame_idx].id;
    let frame_parent = stack.layers[frame_idx].parent_id;
    let count = content_idx.len();

    // Lift the content layers out (highest index first so lower ones stay
    // valid), preserving their relative order.
    let mut desc = content_idx;
    desc.sort_unstable_by(|a, b| b.cmp(a));
    let mut content: Vec<Layer> = desc.iter().map(|&i| stack.layers.remove(i)).collect();
    content.reverse();

    // Re-insert directly above the frame, linked as its clip content.
    let fi = stack
        .layers
        .iter()
        .position(|l| l.id == frame_id)
        .unwrap_or(0);
    for (k, mut layer) in content.into_iter().enumerate() {
        layer.clip_parent_id = Some(frame_id);
        layer.parent_id = frame_parent;
        stack.layers.insert(fi + 1 + k, layer);
    }
    stack.active_idx = (fi + count).min(stack.layers.len().saturating_sub(1));
    for (i, l) in stack.layers.iter_mut().enumerate() {
        l.selected = (fi + 1..=fi + count).contains(&i);
    }
    // Baked now so the undo snapshot is self-contained.
    canvas.refresh_clip_masks();
    Some(count)
}

/// Take the selected clip content out of its frame: drop the clip link and the
/// managed clip mask, leaving each object free again. How many came out.
pub fn extract(canvas: &mut Canvas) -> usize {
    let mut freed = 0;
    for layer in &mut canvas.layer_stack.layers {
        if layer.selected && layer.clip_parent_id.is_some() {
            layer.clip_parent_id = None;
            // The mask was the managed clip.
            layer.mask = None;
            layer.mask_active = false;
            freed += 1;
        }
    }
    if freed > 0 {
        canvas.clip_fp = 0; // rebake the clips that remain
        canvas.refresh_clip_masks();
    }
    freed
}

/// Position / scale each selected PowerClip content layer inside its own
/// frame. How many were arranged.
pub fn arrange(canvas: &mut Canvas, fit: Fit) -> usize {
    let stack = &mut canvas.layer_stack;
    let targets: Vec<(usize, u32)> = stack
        .layers
        .iter()
        .enumerate()
        .filter(|(_, l)| l.selected)
        .filter_map(|(i, l)| l.clip_parent_id.map(|frame_id| (i, frame_id)))
        .collect();
    let mut arranged = 0;
    for (content_idx, frame_id) in targets {
        // The frame's on-canvas extent (its rendered shape).
        let Some((fx, fy, fw, fh)) = stack
            .layers
            .iter()
            .find(|l| l.id == frame_id)
            .and_then(content_rect)
        else {
            continue;
        };
        // A Path may carry a pending Move offset not yet folded into its model;
        // fold it first so the geometry we transform is at the true position.
        if matches!(
            stack.layers[content_idx].layer_type,
            LayerType::Vector(VectorGeometry::Path(_))
        ) {
            fold_offset_into_model(&mut stack.layers[content_idx]);
        }
        let Some((cx, cy, cw, ch)) = content_rect(&stack.layers[content_idx]) else {
            continue;
        };
        let (sx, sy) = match fit {
            Fit::Center => (1.0, 1.0),
            Fit::Fit => {
                let s = (fw / cw).min(fh / ch);
                (s, s)
            }
            Fit::Fill => {
                let s = (fw / cw).max(fh / ch);
                (s, s)
            }
            Fit::Stretch => (fw / cw, fh / ch),
        };
        let (tw, th) = (cw * sx, ch * sy);
        let (tx, ty) = match fit {
            Fit::Stretch => (fx, fy),
            _ => (fx + (fw - tw) * 0.5, fy + (fh - th) * 0.5),
        };
        arrange_content_layer(&mut stack.layers[content_idx], cx, cy, sx, sy, tx, ty);
        arranged += 1;
    }
    if arranged > 0 {
        canvas.clip_fp = 0;
        canvas.refresh_clip_masks();
    }
    arranged
}

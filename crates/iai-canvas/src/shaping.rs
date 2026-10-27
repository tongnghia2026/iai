//! Shaping vector layers: Weld / Trim / Intersect / Simplify on the selected
//! vector layers, and a shape or a text layer turned into editable curves.
//! Each is one undo step; what it says goes to the status line, and a refusal
//! leaves the document as it was.
//!
//! Coordinate spaces: each vector layer is reduced to a canvas-space `PathData`
//! (primitive → `from_shape`, Path → model in layer space folded with any
//! offset drift). A result is stored on a Path layer with an identity
//! transform, so its raster origin (`Layer::offset`) is derived straight from
//! the canvas-space geometry — the delta-0 invariant every other vector op
//! relies on.

use crate::canvas::Canvas;
use crate::canvas::text_layer::layer_name_from;
use iai_doc::layer::{Layer, LayerType};
use iai_doc::shape::ShapeKind;
use iai_history::command::LayerStructureCommand;
use iai_vector::command_vector::apply_object_to_layer;
use iai_vector::vector::affine::AffineTransform;
use iai_vector::vector::boolean::{BooleanOp, boolean_many, intersect_overlaps};
use iai_vector::vector::brush::BrushStroke;
use iai_vector::vector::color::ColorValue;
use iai_vector::vector::from_shape;
use iai_vector::vector::object::{VectorGeometry, VectorObjectData};
use iai_vector::vector::path::PathData;
use iai_vector::vector::raster;
use iai_vector::vector::style::VectorStyle;

/// A vector layer eligible for a boolean: selected, unlocked, not the background,
/// and holding either a parametric primitive or a Bézier path.
pub fn is_boolean_target(layer: &Layer) -> bool {
    layer.selected
        && !layer.locked
        && !layer.is_background
        && matches!(layer.layer_type, LayerType::Vector(_))
}

/// Reduce one vector layer to a canvas-space fill path plus its style. Returns
/// `None` for non-vector layers.
pub fn layer_canvas_path(layer: &Layer) -> Option<(PathData, VectorStyle)> {
    match &layer.layer_type {
        LayerType::Vector(VectorGeometry::Path(obj)) => {
            // The model lives in layer space; `Layer::offset` is normally its
            // derived raster origin (delta-0). Fold any residual drag drift so the
            // path we compose is in true canvas space.
            let mut p = obj.path_in_layer_space();
            if let Some((origin, _, _)) = raster::raster_geometry(obj) {
                let dx = (layer.offset.0 - origin.0) as f32;
                let dy = (layer.offset.1 - origin.1) as f32;
                if dx != 0.0 || dy != 0.0 {
                    p.transform(&AffineTransform::translate(dx, dy));
                }
            }
            Some((p, obj.style))
        }
        LayerType::Vector(VectorGeometry::Primitive(shape)) => {
            let (x0, y0, x1, y1) = shape.canvas_span(layer.offset);
            let path = match shape.kind {
                ShapeKind::Rectangle => {
                    from_shape::rect_path(x0, y0, x1, y1, shape.effective_radius())
                }
                ShapeKind::Ellipse => from_shape::ellipse_path(x0, y0, x1, y1),
                ShapeKind::Line => from_shape::line_path(x0, y0, x1, y1),
                ShapeKind::Polygon => from_shape::polygon_path(x0, y0, x1, y1, shape.sides),
                ShapeKind::Star => {
                    from_shape::star_path(x0, y0, x1, y1, shape.sides, shape.star_inner)
                }
            };
            Some((path, shape.style))
        }
        _ => None,
    }
}

/// Like [`layer_canvas_path`] but also returns the object's Vector Brush (its
/// width scaled into canvas units) when the layer is a brush stroke, so an
/// open-stroke cut can preserve the variable width. `None` for non-vector layers.
pub fn layer_canvas_object(layer: &Layer) -> Option<(PathData, VectorStyle, Option<BrushStroke>)> {
    let (path, style) = layer_canvas_path(layer)?;
    let brush = match &layer.layer_type {
        LayerType::Vector(VectorGeometry::Path(obj)) => obj.brush.as_ref().map(|b| BrushStroke {
            base_width: b.base_width * obj.transform_scale(),
            profile: b.profile.clone(),
            cap: b.cap,
        }),
        _ => None,
    };
    Some((path, style, brush))
}

/// The status line's name of `op`, CorelDRAW's.
pub fn boolean_name(op: BooleanOp) -> &'static str {
    match op {
        BooleanOp::Union => "Weld",
        BooleanOp::Intersect => "Intersect",
        BooleanOp::Difference => "Trim",
        BooleanOp::Exclude => "Simplify",
    }
}

/// Apply a boolean/shaping operation to the selected vector layers, as one
/// undo step. CorelDRAW-compatible layer policy:
///   Weld (Union)  → merge every source into ONE result (sources removed).
///   Trim (Diff)   → bottom target minus uppers; sources stay unchanged.
///   Intersect     → KEEP every source; add the overlap as a NEW object on top.
///   Simplify      → preserve the visible stack: each lower object loses the
///                   area hidden by selected objects above it.
pub fn boolean_selected(canvas: &mut Canvas, op: BooleanOp) -> Result<String, String> {
    // Selected vector layers bottom-to-top: (layer index, canvas-space path,
    // style). Index 0 is the bottom layer.
    let items: Vec<(usize, PathData, VectorStyle)> = canvas
        .layer_stack
        .layers
        .iter()
        .enumerate()
        .filter(|(_, l)| is_boolean_target(l))
        .filter_map(|(i, l)| layer_canvas_path(l).map(|(p, s)| (i, p, s)))
        .collect();
    if items.len() < 2 {
        return Err("Select at least 2 vector objects to shape".to_string());
    }
    let indices: Vec<usize> = items.iter().map(|(i, _, _)| *i).collect();
    let paths: Vec<&PathData> = items.iter().map(|(_, p, _)| p).collect();
    let target_idx = indices[0];
    // Merge results inherit the bottom (target) object's style.
    let target_style = items[0].2;
    let name = boolean_name(op);

    // Compute the geometry up front so an empty/failed result aborts cleanly,
    // before any layer is touched.
    enum Plan {
        /// Remove all sources, insert one result at the bottom (Weld).
        Merge(VectorObjectData),
        /// Replace only the bottom target. `None` means it was fully trimmed.
        ReplaceTarget(Option<VectorObjectData>),
        /// Keep all sources, add one result on top (Intersect).
        AddOnTop(VectorObjectData),
        /// Replace each source's geometry in place; `None` = fully consumed,
        /// remove that layer (Simplify). The top selected layer is untouched.
        PerLayer(Vec<(usize, Option<VectorObjectData>)>),
    }
    let new_obj =
        |p: PathData, s: VectorStyle| VectorObjectData::new(p, s, AffineTransform::IDENTITY);

    let plan = match op {
        BooleanOp::Union => match boolean_many(&paths, op).filter(|r| !r.contours.is_empty()) {
            Some(r) => Plan::Merge(new_obj(r, target_style)),
            None => return Err("Empty result".to_string()),
        },
        BooleanOp::Difference => {
            let result = boolean_many(&paths, op)
                .filter(|r| !r.contours.is_empty())
                .map(|r| new_obj(r, target_style));
            Plan::ReplaceTarget(result)
        }
        BooleanOp::Intersect => match intersect_overlaps(&paths).filter(|r| !r.contours.is_empty())
        {
            Some(r) => Plan::AddOnTop(new_obj(r, target_style)),
            None => return Err("No intersection".to_string()),
        },
        BooleanOp::Exclude => {
            // Corel Simplify: top objects act as cutters for objects below.
            // The top selected object stays untouched.
            let mut per: Vec<(usize, Option<VectorObjectData>)> =
                Vec::with_capacity(items.len().saturating_sub(1));
            for (k, (idx, _p, style)) in
                items.iter().enumerate().take(items.len().saturating_sub(1))
            {
                let mut refs: Vec<&PathData> = vec![paths[k]];
                refs.extend(paths.iter().skip(k + 1).copied());
                let r =
                    boolean_many(&refs, BooleanOp::Difference).filter(|r| !r.contours.is_empty());
                match r {
                    Some(r) => per.push((*idx, Some(new_obj(r, *style)))),
                    None => per.push((*idx, None)),
                }
            }
            Plan::PerLayer(per)
        }
    };

    let valid = match &plan {
        Plan::Merge(o) | Plan::AddOnTop(o) => o.validate().is_ok(),
        Plan::ReplaceTarget(o) => o.as_ref().is_none_or(|o| o.validate().is_ok()),
        Plan::PerLayer(v) => v
            .iter()
            .all(|(_, o)| o.as_ref().is_none_or(|o| o.validate().is_ok())),
    };
    if !valid {
        return Err("Invalid result".to_string());
    }

    let (cw, ch) = (canvas.width, canvas.height);
    let before = LayerStructureCommand::capture_before(name, &canvas.layer_stack, cw, ch);

    let mut removed_empty = 0usize;
    match plan {
        Plan::Merge(object) => {
            let new_id = canvas.layer_stack.next_id();
            canvas.layer_stack.set_next_id(new_id + 1);
            let mut new_layer = canvas.layer_stack.layers[target_idx].clone();
            new_layer.id = new_id;
            new_layer.name = name.to_string();
            new_layer.mask = None;
            apply_object_to_layer(&mut new_layer, object);
            // Remove sources highest-index-first so lower indices stay valid.
            let mut desc = indices.clone();
            desc.sort_unstable_by(|a, b| b.cmp(a));
            for &i in &desc {
                canvas.layer_stack.layers.remove(i);
            }
            let insert_at = target_idx.min(canvas.layer_stack.layers.len());
            canvas.layer_stack.layers.insert(insert_at, new_layer);
            canvas.layer_stack.active_idx = insert_at;
            for (i, l) in canvas.layer_stack.layers.iter_mut().enumerate() {
                l.selected = i == insert_at;
            }
        }
        Plan::AddOnTop(object) => {
            let new_id = canvas.layer_stack.next_id();
            canvas.layer_stack.set_next_id(new_id + 1);
            let mut new_layer = canvas.layer_stack.layers[target_idx].clone();
            new_layer.id = new_id;
            new_layer.name = name.to_string();
            new_layer.mask = None;
            apply_object_to_layer(&mut new_layer, object);
            let top = *indices.last().unwrap_or(&target_idx);
            let insert_at = (top + 1).min(canvas.layer_stack.layers.len());
            canvas.layer_stack.layers.insert(insert_at, new_layer);
            canvas.layer_stack.active_idx = insert_at;
            for (i, l) in canvas.layer_stack.layers.iter_mut().enumerate() {
                l.selected = i == insert_at;
            }
        }
        Plan::ReplaceTarget(object) => {
            if let Some(object) = object {
                apply_object_to_layer(&mut canvas.layer_stack.layers[target_idx], object);
                canvas.layer_stack.active_idx = target_idx;
            } else {
                canvas.layer_stack.layers.remove(target_idx);
                removed_empty = 1;
                canvas.layer_stack.active_idx =
                    target_idx.min(canvas.layer_stack.layers.len().saturating_sub(1));
            }
            let active = canvas.layer_stack.active_idx;
            for (i, layer) in canvas.layer_stack.layers.iter_mut().enumerate() {
                layer.selected = i == active;
            }
        }
        Plan::PerLayer(per) => {
            // In-place edits keep indices stable; collect the empties and drop
            // them afterwards (highest-first). Edited layers stay selected.
            let mut empties: Vec<usize> = Vec::new();
            for (idx, obj) in per {
                match obj {
                    Some(o) => apply_object_to_layer(&mut canvas.layer_stack.layers[idx], o),
                    None => empties.push(idx),
                }
            }
            empties.sort_unstable_by(|a, b| b.cmp(a));
            for &i in &empties {
                canvas.layer_stack.layers.remove(i);
            }
            removed_empty = empties.len();
            if let Some(pos) = canvas.layer_stack.layers.iter().position(|l| l.selected) {
                canvas.layer_stack.active_idx = pos;
            } else {
                canvas.layer_stack.active_idx = canvas
                    .layer_stack
                    .active_idx
                    .min(canvas.layer_stack.layers.len().saturating_sub(1));
            }
        }
    }

    // CMYK: re-derive ink planes for the new Path raster(s) from the RGB mirror.
    canvas.reconcile_path_ink();
    canvas.layer_revision += 1;

    let mut cmd = before;
    cmd.capture_after(&canvas.layer_stack, cw, ch);
    canvas.record(Box::new(cmd));

    Ok(match op {
        BooleanOp::Union => format!("Welded {} vector objects", indices.len()),
        BooleanOp::Difference => {
            if removed_empty == 0 {
                format!(
                    "Trimmed the target with {} source(s) — sources kept",
                    indices.len() - 1
                )
            } else {
                "Target fully trimmed away — sources kept".to_string()
            }
        }
        BooleanOp::Intersect => "Created the intersection (originals kept)".to_string(),
        BooleanOp::Exclude => format!(
            "Simplified the overlap — {} shapes remain",
            indices.len() - removed_empty
        ),
    })
}

/// Convert the Shape layer at `idx` into an editable vector Path layer
/// ("Convert to Curves"). The primitive geometry becomes a `PathData`
/// (rectangle/rounded-rect/ellipse → closed curves, line → open), the
/// fill/stroke carry over, and the layer is swapped IN PLACE so its id /
/// stacking order / opacity / blend / mask are kept. One structural undo
/// restores the Shape.
pub fn shape_to_curves(canvas: &mut Canvas, idx: usize) -> Result<String, String> {
    let Some((data, offset)) =
        canvas
            .layer_stack
            .layers
            .get(idx)
            .and_then(|l| match &l.layer_type {
                LayerType::Vector(VectorGeometry::Primitive(d)) => Some((d.clone(), l.offset)),
                _ => None,
            })
    else {
        return Err("Not a shape layer".to_string());
    };
    let (x0, y0, x1, y1) = data.canvas_span(offset);
    // Shape geometry is layer-local; convert to canvas space so the Path lands
    // in the same place (its transform is then identity — delta-0 invariant).
    let path = match data.kind {
        ShapeKind::Rectangle => {
            let radius = data.effective_radius();
            from_shape::rect_path_corners(x0, y0, x1, y1, [radius; 4], [data.corner_type; 4])
        }
        ShapeKind::Ellipse => from_shape::ellipse_path(x0, y0, x1, y1),
        ShapeKind::Line => from_shape::line_path(x0, y0, x1, y1),
        ShapeKind::Polygon => from_shape::polygon_path(x0, y0, x1, y1, data.sides),
        ShapeKind::Star => from_shape::star_path(x0, y0, x1, y1, data.sides, data.star_inner),
    };
    let object = VectorObjectData::new(path, data.style, AffineTransform::IDENTITY);

    let (cw, ch) = (canvas.width, canvas.height);
    let before =
        LayerStructureCommand::capture_before("Convert to Curves", &canvas.layer_stack, cw, ch);
    if let Some(layer) = canvas.layer_stack.layers.get_mut(idx) {
        apply_object_to_layer(layer, object);
    }
    // CMYK: re-derive ink planes for the new Path raster from its RGB mirror.
    canvas.reconcile_path_ink();
    canvas.layer_revision += 1;

    let mut cmd = before;
    cmd.capture_after(&canvas.layer_stack, cw, ch);
    canvas.record(Box::new(cmd));
    Ok("Converted the shape to curves (Path)".to_string())
}

/// Convert the Text layer at `idx` into vector Path layers: one per colour
/// run, the first in the text layer's place, the rest just above it with
/// the same compositing. Every glyph outline lands exactly where the
/// rasterized text sat. One structural undo restores the Text layer.
///
/// The existing layer mask is preserved on the in-place Path layer, just like
/// Shape-to-Curves; the vector opacity carries the layer's own
/// `Layer::opacity`, while `TextData::opacity` is folded into each object's
/// `VectorStyle::opacity` by [`iai_text::text::text_to_curves`].
pub fn text_to_curves(canvas: &mut Canvas, idx: usize) -> Result<String, String> {
    // Snapshot the text layer's data + the properties extra colour layers
    // inherit.
    let (td, layer_offset, blend, layer_opacity, visible, parent_id, page_id) = {
        let Some(layer) = canvas.layer_stack.layers.get(idx) else {
            return Err("Not a text layer".to_string());
        };
        let LayerType::Text(td) = &layer.layer_type else {
            return Err("Not a text layer".to_string());
        };
        (
            td.clone(),
            layer.offset,
            layer.blend_mode,
            layer.opacity,
            layer.visible,
            layer.parent_id,
            layer.page_id,
        )
    };
    let td_opacity = td.opacity.clamp(0.0, 1.0);

    // Build one validated vector object per colour group (skip any the parser
    // limits would reject, so a pathological glyph count can't poison a file).
    let mut objects: Vec<VectorObjectData> = Vec::new();
    for grp in iai_text::text::text_to_curves(&td, layer_offset) {
        let style = VectorStyle {
            opacity: td_opacity,
            ..VectorStyle::filled(ColorValue::from_rgba8(grp.color))
        };
        let obj = VectorObjectData::new(grp.path, style, AffineTransform::IDENTITY);
        if obj.validate().is_ok() {
            objects.push(obj);
        }
    }
    let Some(first) = objects.first().cloned() else {
        return Err("No text to convert to curves".to_string());
    };

    let (cw, ch) = (canvas.width, canvas.height);
    let before =
        LayerStructureCommand::capture_before("Text to Curves", &canvas.layer_stack, cw, ch);

    // First colour reuses the text layer slot in place.
    if let Some(layer) = canvas.layer_stack.layers.get_mut(idx) {
        apply_object_to_layer(layer, first);
    }
    canvas.layer_stack.active_idx = idx;

    // Extra colours: fresh Path layers just above, inheriting the text layer's
    // compositing so the stack looks identical.
    for obj in objects.iter().skip(1) {
        let new_idx = canvas.layer_stack.add_layer(cw, ch);
        let layer = &mut canvas.layer_stack.layers[new_idx];
        apply_object_to_layer(layer, obj.clone());
        layer.blend_mode = blend;
        layer.opacity = layer_opacity;
        layer.visible = visible;
        layer.parent_id = parent_id;
        layer.page_id = page_id;
        layer.name = layer_name_from(&td.content);
    }

    // CMYK: re-derive ink planes for the new Path rasters from their RGB mirror.
    canvas.reconcile_path_ink();
    canvas.layer_revision += 1;

    // Select the converted layer so the Move box shows without an extra click.
    // Do this before capturing the after-state so redo restores the exact
    // selection produced by the original conversion.
    for l in &mut canvas.layer_stack.layers {
        l.selected = false;
    }
    let active = canvas
        .layer_stack
        .active_idx
        .min(canvas.layer_stack.layers.len().saturating_sub(1));
    if let Some(l) = canvas.layer_stack.layers.get_mut(active) {
        l.selected = true;
    }

    let mut cmd = before;
    cmd.capture_after(&canvas.layer_stack, cw, ch);
    canvas.record(Box::new(cmd));
    Ok("Converted text to curves (Path)".to_string())
}

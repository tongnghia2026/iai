//! Text set along a vector layer's outline stays on it: when the outline
//! changes (node edit, move, transform, undo) the type layer takes the new
//! path, as CorelDRAW keeps Fit Text To Path live. The link is
//! `TextPath::source`; `TextPath::source_sig` says which outline the points
//! were taken from, so a frame where nothing moved costs one hash per link.

use crate::canvas::Canvas;
use crate::canvas::text_layer::rasterize_into_layer;
use crate::shaping::layer_canvas_path;
use iai_doc::layer::{Layer, LayerType};
use std::hash::{Hash, Hasher};

/// Fingerprint of the outline text follows on `layer`: its first contour in
/// canvas space. `None` when the layer has no vector outline.
pub fn path_signature(layer: &Layer) -> Option<u64> {
    let (path, _) = layer_canvas_path(layer)?;
    let c = path.contours.first()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    c.closed.hash(&mut h);
    for n in &c.nodes {
        for p in [Some(n.anchor), n.in_handle, n.out_handle] {
            match p {
                Some(p) => {
                    p.x.to_bits().hash(&mut h);
                    p.y.to_bits().hash(&mut h);
                }
                None => 0xffu8.hash(&mut h),
            }
        }
    }
    Some(h.finish())
}

/// The outline text follows on `layer`, flattened, canvas pixels, and
/// whether it is closed. `None` without a line or shape of 2+ points.
pub fn text_path_points(layer: &Layer) -> Option<(Vec<[f32; 2]>, bool)> {
    let (path, _) = layer_canvas_path(layer)?;
    let contour = path.contours.first()?;
    let one = iai_doc::vector::path::PathData::new(
        vec![contour.clone()],
        iai_doc::vector::path::FillRule::NonZero,
    );
    let pts = iai_doc::vector::flatten::flatten_path(&one, 0.25)
        .into_iter()
        .next()
        .unwrap_or_default();
    (pts.len() >= 2).then(|| (pts.iter().map(|q| [q.x, q.y]).collect(), contour.closed))
}

/// The upright origin of a type layer on the page (its offset less the
/// placement delta), the space `TextPath::points` are in.
pub fn text_anchor(layer: &Layer) -> Option<(i32, i32)> {
    let LayerType::Text(td) = &layer.layer_type else {
        return None;
    };
    let delta = iai_text::text::rasterize_placed(td).map_or((0, 0), |(_, d)| d);
    Some((
        layer.offset.0.saturating_sub(delta.0),
        layer.offset.1.saturating_sub(delta.1),
    ))
}

/// Moves every type layer whose source outline changed onto it. Returns
/// whether any did; their old and new bounds are marked dirty. A deleted
/// source leaves the text where it was.
pub fn follow_text_paths(canvas: &mut Canvas) -> bool {
    let layers = &canvas.layer_stack.layers;
    let mut todo: Vec<(usize, Vec<[f32; 2]>, bool, u64)> = Vec::new();
    for (i, l) in layers.iter().enumerate() {
        let LayerType::Text(td) = &l.layer_type else {
            continue;
        };
        let Some(src) = td.path.as_ref().and_then(|p| p.source) else {
            continue;
        };
        let Some(source) = layers.iter().find(|s| s.id == src) else {
            continue;
        };
        let Some(sig) = path_signature(source) else {
            continue;
        };
        if td.path.as_ref().is_some_and(|p| p.source_sig == sig) {
            continue;
        }
        if let Some((pts, closed)) = text_path_points(source) {
            todo.push((i, pts, closed, sig));
        }
    }
    if todo.is_empty() {
        return false;
    }
    for (i, pts, closed, sig) in todo {
        let Some(layer) = canvas.layer_stack.layers.get(i) else {
            continue;
        };
        let (Some(anchor), LayerType::Text(td)) = (text_anchor(layer), &layer.layer_type) else {
            continue;
        };
        let old = (layer.offset.0, layer.offset.1, layer.width, layer.height);
        let mut td = td.clone();
        if let Some(p) = td.path.as_mut() {
            p.points = pts
                .iter()
                .map(|q| [q[0] - anchor.0 as f32, q[1] - anchor.1 as f32])
                .collect();
            p.closed = closed;
            p.source_sig = sig;
        }
        rasterize_into_layer(canvas, i, anchor, td);
        let new = {
            let l = &canvas.layer_stack.layers[i];
            (l.offset.0, l.offset.1, l.width, l.height)
        };
        canvas.mark_dirty_layer_bounds(old.0, old.1, old.2, old.3);
        canvas.mark_dirty_layer_bounds(new.0, new.1, new.2, new.3);
    }
    canvas.layer_revision = canvas.layer_revision.saturating_add(1);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use iai_doc::model::text::{TextData, TextPath};
    use iai_vector::command_vector::apply_object_to_layer;
    use iai_vector::vector::affine::AffineTransform;
    use iai_vector::vector::color::ColorValue;
    use iai_vector::vector::from_shape;
    use iai_vector::vector::object::VectorObjectData;
    use iai_vector::vector::style::VectorStyle;

    #[test]
    fn text_follows_its_outline_when_it_moves() {
        let mut canvas = Canvas::new(400, 300);
        let src = canvas.layer_stack.next_id();
        canvas.layer_stack.set_next_id(src + 1);
        let mut line = Layer::new(src, "Đường", 1, 1);
        apply_object_to_layer(
            &mut line,
            VectorObjectData::new(
                from_shape::line_path(20.0, 150.0, 380.0, 150.0),
                VectorStyle::filled(ColorValue::from_rgba8([0, 0, 0, 255])),
                AffineTransform::IDENTITY,
            ),
        );
        let sig = path_signature(&line).expect("sig");
        let (points, closed) = text_path_points(&line).expect("points");
        canvas.layer_stack.layers.push(line);
        let li = canvas.layer_stack.layers.len() - 1;
        let td = TextData {
            content: "ABC".into(),
            font_px: 30.0,
            path: Some(TextPath {
                points,
                closed,
                start: 0.5,
                offset: 0.0,
                flip: false,
                source: Some(src),
                source_sig: sig,
            }),
            ..TextData::default()
        };
        let tid = canvas.layer_stack.next_id();
        canvas.layer_stack.set_next_id(tid + 1);
        canvas.layer_stack.layers.push(Layer::new(tid, "Chữ", 1, 1));
        let ti = canvas.layer_stack.layers.len() - 1;
        rasterize_into_layer(&mut canvas, ti, (0, 0), td);
        assert!(!follow_text_paths(&mut canvas), "nothing moved yet");
        let y0 = canvas.layer_stack.layers[ti].offset.1;
        canvas.layer_stack.layers[li].offset.1 += 60;
        assert!(follow_text_paths(&mut canvas));
        let y1 = canvas.layer_stack.layers[ti].offset.1;
        assert!((y1 - y0 - 60).abs() <= 1, "text moved {} px", y1 - y0);
        assert!(!follow_text_paths(&mut canvas), "settled");
    }
}

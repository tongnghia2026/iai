//! The designer's object tools beyond Shaping, CorelDRAW's words: Contour
//! (parallel outlines outward or inward), Convert Outline to Object, Combine
//! and Break Apart (Align / Distribute live in `iai_vector::arrange`). Each
//! is one undo step on the layers named by id; a refusal leaves the document
//! as it was.
//!
//! Results are Path layers with an identity transform in canvas space, the
//! same delta-0 invariant [`crate::shaping`] keeps.

use crate::canvas::Canvas;
use crate::shaping::layer_canvas_path;
use iai_doc::layer::{Layer, LayerType};
use iai_history::command::LayerStructureCommand;
use iai_vector::command_vector::apply_object_to_layer;
use iai_vector::vector::affine::AffineTransform;
use iai_vector::vector::color::ColorValue;
use iai_vector::vector::contour;
use iai_vector::vector::object::{VectorGeometry, VectorObjectData};
use iai_vector::vector::path::PathData;
use iai_vector::vector::style::{LineJoin, Paint, VectorStyle};

fn index_of(canvas: &Canvas, id: u32) -> Result<usize, String> {
    canvas
        .layer_stack
        .layers
        .iter()
        .position(|l| l.id == id)
        .ok_or_else(|| format!("No layer {id}"))
}

fn editable(l: &Layer) -> bool {
    !l.locked && !l.is_background
}

/// One undo step around `f`, which changes the layer stack.
fn step<T>(
    canvas: &mut Canvas,
    name: &str,
    f: impl FnOnce(&mut Canvas) -> Result<T, String>,
) -> Result<T, String> {
    let (cw, ch) = (canvas.width, canvas.height);
    let mut cmd = LayerStructureCommand::capture_before(name, &canvas.layer_stack, cw, ch);
    let out = f(canvas)?;
    canvas.reconcile_path_ink();
    canvas.layer_revision += 1;
    cmd.capture_after(&canvas.layer_stack, cw, ch);
    canvas.record(Box::new(cmd));
    Ok(out)
}

/// A new layer like `like` (its group, blend, opacity) holding `object`.
fn new_layer_like(canvas: &mut Canvas, like: usize, object: VectorObjectData, name: &str) -> Layer {
    let id = canvas.layer_stack.next_id();
    canvas.layer_stack.set_next_id(id + 1);
    let mut layer = canvas.layer_stack.layers[like].clone();
    layer.id = id;
    layer.name = name.to_string();
    layer.mask = None;
    layer.selected = false;
    layer.clip_parent_id = None;
    apply_object_to_layer(&mut layer, object);
    layer
}

/// A vector layer's path and style; a type layer's letters as one filled
/// path in its colour (so text gets contours too: outlined lettering).
fn shape_or_text_path(layer: &Layer) -> Option<(PathData, VectorStyle)> {
    if let LayerType::Text(td) = &layer.layer_type {
        let groups = iai_text::text::text_to_curves(td, layer.offset);
        let colour = groups.first()?.color;
        let contours = groups.into_iter().flat_map(|g| g.path.contours).collect();
        let path = PathData::new(contours, iai_vector::vector::path::FillRule::NonZero);
        return Some((path, VectorStyle::filled(ColorValue::from_rgba8(colour))));
    }
    layer_canvas_path(layer)
}

/// Stroke width of a vector layer in canvas units.
fn canvas_stroke_width(layer: &Layer, style: &VectorStyle) -> f32 {
    let scale = match &layer.layer_type {
        LayerType::Vector(VectorGeometry::Path(o)) => o.transform_scale(),
        _ => 1.0,
    };
    style.stroke_style.width * scale
}

/// What Contour makes of each step.
pub struct ContourSpec {
    /// Pixels per step: outward when positive, inward when negative.
    pub distance: f32,
    pub steps: u32,
    pub join: LineJoin,
    /// Fill of each step, in turn (cycled); the object's own fill when empty.
    pub colors: Vec<[u8; 4]>,
    /// Keep the object's outline on the contours too.
    pub keep_outline: bool,
}

/// Contour: around (or inside) each layer, `steps` outlines `distance`
/// apart, each a filled layer — outward ones under it, the widest lowest;
/// inward ones above it. Returns the new layers.
pub fn contour(canvas: &mut Canvas, ids: &[u32], spec: &ContourSpec) -> Result<Vec<u32>, String> {
    if ids.is_empty() {
        return Err("Pick a vector layer".to_string());
    }
    let mut work: Vec<(u32, Vec<(PathData, VectorStyle, String)>)> = Vec::new();
    for &id in ids {
        let idx = index_of(canvas, id)?;
        let layer = &canvas.layer_stack.layers[idx];
        let (path, style) = shape_or_text_path(layer)
            .ok_or_else(|| format!("«{}» is not a vector layer", layer.name))?;
        let mut made = Vec::new();
        for k in 1..=spec.steps.max(1) {
            let d = spec.distance * k as f32;
            let Some(p) = contour::offset_path(&path, d, spec.join, 4.0) else {
                break;
            };
            let mut st = style;
            if let Some(c) = spec.colors.get((k as usize - 1) % spec.colors.len().max(1)) {
                st.fill = Paint::Solid(ColorValue::from_rgba8(*c));
            } else if matches!(st.fill, Paint::None) {
                // An outline-only object: its contours take the outline's paint.
                st.fill = st.stroke;
            }
            if !spec.keep_outline {
                st.stroke = Paint::None;
            }
            made.push((p, st, format!("Viền {k}: {}", layer.name)));
        }
        if made.is_empty() {
            return Err(format!(
                "«{}» has no room for an inward contour that far",
                layer.name
            ));
        }
        work.push((id, made));
    }
    step(canvas, "Contour", |canvas| {
        let mut out = Vec::new();
        for (id, made) in work {
            let outward = spec.distance > 0.0;
            // Outward: widest first so it ends lowest; each goes in under
            // the object. Inward: smallest last, on top.
            let order: Vec<_> = if outward {
                made.into_iter().rev().collect()
            } else {
                made
            };
            let mut last = id;
            for (p, st, name) in order {
                let idx = index_of(canvas, if outward { id } else { last })?;
                let object = VectorObjectData::new(p, st, AffineTransform::IDENTITY);
                object.validate().map_err(|e| e.to_string())?;
                let layer = new_layer_like(canvas, idx, object, &name);
                out.push(layer.id);
                last = layer.id;
                // Outward: just under the object, above the earlier (wider)
                // ones. Inward: just above the previous one.
                let at = if outward { idx } else { idx + 1 };
                canvas.layer_stack.layers.insert(at, layer);
            }
        }
        Ok(out)
    })
}

/// Convert Outline to Object: each layer's outline becomes a filled layer
/// of its own just above it, painted with the outline's paint; the object
/// keeps only its fill (or goes, when it had none). Returns the new layers.
pub fn outline_to_object(canvas: &mut Canvas, ids: &[u32]) -> Result<Vec<u32>, String> {
    let mut work = Vec::new();
    for &id in ids {
        let idx = index_of(canvas, id)?;
        let layer = &canvas.layer_stack.layers[idx];
        if !editable(layer) {
            return Err(format!("«{}» is locked", layer.name));
        }
        let (path, style) = layer_canvas_path(layer)
            .ok_or_else(|| format!("«{}» is not a vector layer", layer.name))?;
        if matches!(style.stroke, Paint::None) {
            return Err(format!("«{}» has no outline", layer.name));
        }
        let width = canvas_stroke_width(layer, &style);
        let s = style.stroke_style;
        let shape = contour::outline_to_path(&path, width, s.cap, s.join, s.miter_limit)
            .ok_or_else(|| format!("«{}»'s outline is too thin", layer.name))?;
        let mut st = style;
        st.fill = style.stroke;
        st.stroke = Paint::None;
        let mut rest = style;
        rest.stroke = Paint::None;
        let keep = !matches!(style.fill, Paint::None);
        work.push((
            id,
            shape,
            st,
            path,
            rest,
            keep,
            format!("Viền: {}", layer.name),
        ));
    }
    step(canvas, "Convert Outline to Object", |canvas| {
        let mut out = Vec::new();
        for (id, shape, st, path, rest, keep, name) in work {
            let idx = index_of(canvas, id)?;
            let object = VectorObjectData::new(shape, st, AffineTransform::IDENTITY);
            object.validate().map_err(|e| e.to_string())?;
            let layer = new_layer_like(canvas, idx, object, &name);
            out.push(layer.id);
            canvas.layer_stack.layers.insert(idx + 1, layer);
            if keep {
                let fill = VectorObjectData::new(path, rest, AffineTransform::IDENTITY);
                apply_object_to_layer(&mut canvas.layer_stack.layers[idx], fill);
            } else {
                canvas.layer_stack.layers.remove(idx);
            }
        }
        Ok(out)
    })
}

/// Combine: the layers become one path (overlaps cut holes), with the
/// bottom one's style and place. Returns it.
pub fn combine(canvas: &mut Canvas, ids: &[u32]) -> Result<u32, String> {
    if ids.len() < 2 {
        return Err("Pick two or more vector layers".to_string());
    }
    let mut rows: Vec<usize> = ids
        .iter()
        .map(|id| index_of(canvas, *id))
        .collect::<Result<_, _>>()?;
    rows.sort_unstable();
    let mut paths = Vec::new();
    for &i in &rows {
        let l = &canvas.layer_stack.layers[i];
        if !editable(l) {
            return Err(format!("«{}» is locked", l.name));
        }
        paths.push(
            layer_canvas_path(l).ok_or_else(|| format!("«{}» is not a vector layer", l.name))?,
        );
    }
    let refs: Vec<&PathData> = paths.iter().map(|(p, _)| p).collect();
    let one = contour::combine(&refs);
    let style = paths[0].1;
    step(canvas, "Combine", |canvas| {
        let object = VectorObjectData::new(one, style, AffineTransform::IDENTITY);
        object.validate().map_err(|e| e.to_string())?;
        let bottom = rows[0];
        let name = canvas.layer_stack.layers[bottom].name.clone();
        let layer = new_layer_like(canvas, bottom, object, &name);
        let id = layer.id;
        for &i in rows.iter().rev() {
            canvas.layer_stack.layers.remove(i);
        }
        canvas.layer_stack.layers.insert(bottom, layer);
        canvas.layer_stack.active_idx = bottom;
        Ok(id)
    })
}

/// Break Apart: each separate piece of the layer on a layer of its own (its
/// holes kept with it), in reading order, where the layer was. Returns them.
pub fn break_apart(canvas: &mut Canvas, id: u32) -> Result<Vec<u32>, String> {
    let idx = index_of(canvas, id)?;
    let l = &canvas.layer_stack.layers[idx];
    if !editable(l) {
        return Err(format!("«{}» is locked", l.name));
    }
    let (path, style) =
        layer_canvas_path(l).ok_or_else(|| format!("«{}» is not a vector layer", l.name))?;
    let parts = contour::break_apart(&path);
    if parts.len() < 2 {
        return Err(format!("«{}» is already one piece", l.name));
    }
    let name = l.name.clone();
    step(canvas, "Break Apart", |canvas| {
        let mut out = Vec::new();
        let mut layers = Vec::new();
        for (k, p) in parts.into_iter().enumerate() {
            let object = VectorObjectData::new(p, style, AffineTransform::IDENTITY);
            object.validate().map_err(|e| e.to_string())?;
            let layer = new_layer_like(canvas, idx, object, &format!("{name} {}", k + 1));
            out.push(layer.id);
            layers.push(layer);
        }
        canvas.layer_stack.layers.remove(idx);
        // The first piece lowest, so reading order runs up the panel.
        for (k, layer) in layers.into_iter().enumerate() {
            canvas.layer_stack.layers.insert(idx + k, layer);
        }
        canvas.layer_stack.active_idx = idx;
        Ok(out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use iai_vector::vector::flatten::flatten_path;
    use iai_vector::vector::from_shape;

    /// What a vector layer's outline covers, half its stroke added.
    fn ink_bounds(layer: &Layer) -> Option<(f32, f32, f32, f32)> {
        let (p, st) = layer_canvas_path(layer)?;
        let pad = if matches!(st.stroke, Paint::None) {
            0.0
        } else {
            canvas_stroke_width(layer, &st) * 0.5
        };
        let b = flatten_path(&p, 0.25)
            .iter()
            .flatten()
            .fold((f32::MAX, f32::MAX, f32::MIN, f32::MIN), |b, q| {
                (b.0.min(q.x), b.1.min(q.y), b.2.max(q.x), b.3.max(q.y))
            });
        Some((b.0 - pad, b.1 - pad, b.2 + pad, b.3 + pad))
    }

    fn add(canvas: &mut Canvas, name: &str, path: PathData, style: VectorStyle) -> u32 {
        let id = canvas.layer_stack.next_id();
        canvas.layer_stack.set_next_id(id + 1);
        let mut layer = Layer::new(id, name, 1, 1);
        apply_object_to_layer(
            &mut layer,
            VectorObjectData::new(path, style, AffineTransform::IDENTITY),
        );
        canvas.layer_stack.layers.push(layer);
        id
    }

    fn red() -> VectorStyle {
        VectorStyle::filled(ColorValue::from_rgba8([255, 0, 0, 255]))
    }

    fn names(canvas: &Canvas) -> Vec<String> {
        canvas
            .layer_stack
            .layers
            .iter()
            .map(|l| l.name.clone())
            .collect()
    }

    #[test]
    fn contours_stack_under_the_object_and_undo_in_one_step() {
        let mut canvas = Canvas::new(300, 300);
        let n0 = canvas.layer_stack.layers.len();
        let id = add(
            &mut canvas,
            "Chữ",
            from_shape::rect_path(100.0, 100.0, 200.0, 150.0, 0.0),
            red(),
        );
        let made = contour(
            &mut canvas,
            &[id],
            &ContourSpec {
                distance: 6.0,
                steps: 2,
                join: LineJoin::Round,
                colors: vec![[255, 255, 255, 255], [0, 0, 255, 255]],
                keep_outline: false,
            },
        )
        .expect("contour");
        assert_eq!(made.len(), 2);
        let n = names(&canvas);
        let at = |s: &str| n.iter().position(|x| x == s).expect(s);
        assert!(
            at("Viền 2: Chữ") < at("Viền 1: Chữ") && at("Viền 1: Chữ") < at("Chữ"),
            "{n:?}"
        );
        let wide = &canvas.layer_stack.layers[at("Viền 2: Chữ")];
        let (x0, y0, x1, _) = ink_bounds(wide).expect("b");
        assert!((x0 - 88.0).abs() < 0.7 && (y0 - 88.0).abs() < 0.7 && (x1 - 212.0).abs() < 0.7);
        canvas.undo();
        assert_eq!(canvas.layer_stack.layers.len(), n0 + 1);
    }

    #[test]
    fn combined_layers_break_apart_again() {
        let mut canvas = Canvas::new(200, 200);
        let a = add(
            &mut canvas,
            "a",
            from_shape::rect_path(10.0, 10.0, 50.0, 50.0, 0.0),
            red(),
        );
        let b = add(
            &mut canvas,
            "b",
            from_shape::ellipse_path(100.0, 10.0, 140.0, 50.0),
            red(),
        );
        let one = combine(&mut canvas, &[a, b]).expect("combine");
        assert_eq!(
            names(&canvas)
                .iter()
                .filter(|n| *n == "a" || *n == "b")
                .count(),
            1
        );
        let parts = break_apart(&mut canvas, one).expect("apart");
        assert_eq!(parts.len(), 2);
        assert!(names(&canvas).contains(&"a 1".to_string()));
    }

    #[test]
    fn an_outline_becomes_its_own_filled_layer() {
        let mut canvas = Canvas::new(200, 200);
        let mut st = red();
        st.stroke = Paint::Solid(ColorValue::from_rgba8([0, 0, 0, 255]));
        st.stroke_style.width = 8.0;
        let id = add(
            &mut canvas,
            "Ô",
            from_shape::rect_path(50.0, 50.0, 150.0, 150.0, 0.0),
            st,
        );
        let made = outline_to_object(&mut canvas, &[id]).expect("outline");
        assert_eq!(made.len(), 1);
        let l = &canvas.layer_stack.layers[index_of(&canvas, made[0]).expect("i")];
        let (x0, _, x1, _) = ink_bounds(l).expect("b");
        assert!(
            (x0 - 46.0).abs() < 0.7 && (x1 - 154.0).abs() < 0.7,
            "{x0} {x1}"
        );
        let src = &canvas.layer_stack.layers[index_of(&canvas, id).expect("i")];
        let (_, s) = layer_canvas_path(src).expect("p");
        assert!(matches!(s.stroke, Paint::None));
    }
}

#[cfg(test)]
mod text_tests {
    use super::*;

    #[test]
    fn lettering_gets_contours_round_its_letters() {
        if iai_text::text::font().is_none() {
            return;
        }
        let mut canvas = Canvas::new(300, 120);
        let id = canvas.layer_stack.next_id();
        canvas.layer_stack.set_next_id(id + 1);
        let mut layer = Layer::new(id, "SALE", 1, 1);
        layer.offset = (20, 20);
        layer.layer_type = LayerType::Text(iai_doc::model::text::TextData {
            content: "SALE".to_string(),
            font_px: 60.0,
            color: [255, 0, 0, 255],
            ..Default::default()
        });
        canvas.layer_stack.layers.push(layer);
        let made = contour(
            &mut canvas,
            &[id],
            &ContourSpec {
                distance: 4.0,
                steps: 2,
                join: LineJoin::Round,
                colors: vec![[255, 255, 255, 255], [0, 0, 0, 255]],
                keep_outline: false,
            },
        )
        .expect("contour");
        assert_eq!(made.len(), 2);
    }
}

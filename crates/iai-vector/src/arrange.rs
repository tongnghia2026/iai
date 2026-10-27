//! Align and distribute layers (the Move tool's Align / Distribute buttons):
//! what moves where, worked out on the stack, then applied in one step; and
//! step-and-repeat copies.

use crate::command_vector::{apply_object_to_layer, fold_offset_into_model};
use crate::vector::affine::AffineTransform;
use crate::vector::object::VectorGeometry;
use iai_doc::layer::{Layer, LayerStack, LayerType};
use iai_geom::geometry::Point;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    HorizontalCenter,
    Right,
    Top,
    VerticalCenter,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Distribute {
    HorizontalCenters,
    VerticalCenters,
}

/// Layers that move together, and by how much.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shift {
    pub members: Vec<usize>,
    pub dx: i32,
    pub dy: i32,
}

fn movable(layer: &Layer) -> bool {
    !layer.locked
        && !layer.is_background
        && matches!(
            layer.layer_type,
            LayerType::Raster | LayerType::Text(_) | LayerType::Vector(_) | LayerType::SmartObject
        )
}

fn bounds(layer: &Layer) -> Option<(i32, i32, i32, i32)> {
    let (x0, y0, x1, y1) = layer.tiles.content_bounds()?;
    let (ox, oy) = layer.offset;
    Some((x0 + ox, y0 + oy, x1 + ox, y1 + oy))
}

/// Union of the members' content bounds, in canvas coordinates.
fn union(stack: &LayerStack, members: &[usize]) -> Option<(i32, i32, i32, i32)> {
    members
        .iter()
        .filter_map(|&m| stack.layers.get(m).and_then(bounds))
        .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
}

/// What `align` moves: the selected layers (a selected group as one, its
/// members moving together), or the active one when none is selected. One
/// target aligns to the canvas, several to the box round them all.
pub fn align(stack: &LayerStack, width: u32, height: u32, align: Align) -> Vec<Shift> {
    let n = stack.layers.len();
    let mut consumed = vec![false; n];
    let mut targets: Vec<((i32, i32, i32, i32), Vec<usize>)> = Vec::new();

    // Outermost group first (a header sits above its members, so higher index
    // = more enclosing): an outer group takes all it holds, and a group inside
    // it is then not moved a second time.
    for idx in (0..n).rev() {
        if consumed[idx] || !(stack.layers[idx].is_group() && stack.layers[idx].selected) {
            continue;
        }
        let members: Vec<usize> = stack.group_member_range(idx).collect();
        for &m in &members {
            consumed[m] = true;
        }
        consumed[idx] = true;
        if let Some(b) = union(stack, &members) {
            targets.push((b, members));
        }
    }
    for (idx, layer) in stack.layers.iter().enumerate() {
        if !consumed[idx]
            && layer.selected
            && movable(layer)
            && let Some(b) = bounds(layer)
        {
            targets.push((b, vec![idx]));
        }
    }
    // Nothing selected: the active object (its group, or the layer).
    if targets.is_empty() {
        let idx = stack.active_idx;
        match stack.layers.get(idx) {
            Some(l) if l.is_group() => {
                let members: Vec<usize> = stack.group_member_range(idx).collect();
                if let Some(b) = union(stack, &members) {
                    targets.push((b, members));
                }
            }
            Some(l) if movable(l) => {
                if let Some(b) = bounds(l) {
                    targets.push((b, vec![idx]));
                }
            }
            _ => {}
        }
    }

    let (rx0, ry0, rx1, ry1) = if targets.len() > 1 {
        targets
            .iter()
            .map(|(b, _)| *b)
            .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
            .unwrap_or_default()
    } else {
        (0, 0, width as i32, height as i32)
    };
    let centre = |a: i32, b: i32| (a + b) as f32 * 0.5;
    targets
        .into_iter()
        .filter_map(|((x0, y0, x1, y1), members)| {
            let (dx, dy) = match align {
                Align::Left => (rx0 - x0, 0),
                Align::HorizontalCenter => ((centre(rx0, rx1) - centre(x0, x1)).round() as i32, 0),
                Align::Right => (rx1 - x1, 0),
                Align::Top => (0, ry0 - y0),
                Align::VerticalCenter => (0, (centre(ry0, ry1) - centre(y0, y1)).round() as i32),
                Align::Bottom => (0, ry1 - y1),
            };
            (dx != 0 || dy != 0).then_some(Shift { members, dx, dy })
        })
        .collect()
}

/// What `distribute` moves: of three or more selected movable layers, the
/// inner ones, to equal centre-to-centre steps; the outermost two stay.
pub fn distribute(stack: &LayerStack, mode: Distribute) -> Vec<Shift> {
    let mut objects: Vec<(usize, f32)> = stack
        .layers
        .iter()
        .enumerate()
        .filter(|(_, l)| l.selected && movable(l))
        .filter_map(|(idx, l)| {
            let (x0, y0, x1, y1) = bounds(l)?;
            let c = match mode {
                Distribute::HorizontalCenters => (x0 + x1) as f32 * 0.5,
                Distribute::VerticalCenters => (y0 + y1) as f32 * 0.5,
            };
            Some((idx, c))
        })
        .collect();
    if objects.len() < 3 {
        return Vec::new();
    }
    objects.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    let first = objects[0].1;
    let last = objects[objects.len() - 1].1;
    let step = (last - first) / (objects.len() - 1) as f32;
    let inner = objects.len() - 2;
    objects
        .into_iter()
        .enumerate()
        .skip(1)
        .take(inner)
        .filter_map(|(order, (idx, current))| {
            let d = (first + step * order as f32 - current).round() as i32;
            (d != 0).then(|| match mode {
                Distribute::HorizontalCenters => Shift {
                    members: vec![idx],
                    dx: d,
                    dy: 0,
                },
                Distribute::VerticalCenters => Shift {
                    members: vec![idx],
                    dx: 0,
                    dy: d,
                },
            })
        })
        .collect()
}

/// Step and repeat: copy each selected movable layer and carry the copy by
/// the canvas-space affine `m`. A path copy takes `m` exactly into its model
/// (`new = M ∘ orig`); any other copy only follows where `m` takes its centre,
/// its pixels not turned. The copies become the selection, the top one
/// active, so doing it again goes on from them (a row, a ring, a spiral).
/// How many were copied.
pub fn step_copy(stack: &mut LayerStack, m: AffineTransform) -> usize {
    let rows: Vec<usize> = stack
        .layers
        .iter()
        .enumerate()
        .filter(|(_, l)| l.selected && movable(l) && !l.is_group())
        .map(|(idx, _)| idx)
        .collect();
    if rows.is_empty() || !m.is_finite() {
        return 0;
    }
    for layer in &mut stack.layers {
        layer.selected = false;
    }
    let mut copies = Vec::with_capacity(rows.len());
    // From the top down, so the rows below stay where they were.
    for &idx in rows.iter().rev() {
        let at = stack.duplicate_layer(idx);
        let layer = &mut stack.layers[at];
        match &layer.layer_type {
            LayerType::Vector(VectorGeometry::Path(obj)) => {
                let mut o = obj.clone();
                o.transform = m.then(&obj.transform);
                if o.transform.is_finite() {
                    apply_object_to_layer(layer, o);
                }
            }
            _ => {
                let cx = layer.offset.0 as f32 + layer.width as f32 * 0.5;
                let cy = layer.offset.1 as f32 + layer.height as f32 * 0.5;
                let p = m.apply_point(Point::new(cx, cy));
                layer.offset.0 += (p.x - cx).round() as i32;
                layer.offset.1 += (p.y - cy).round() as i32;
            }
        }
        layer.selected = true;
        copies.push(layer.id);
    }
    if let Some(top) = copies
        .first()
        .and_then(|id| stack.layers.iter().position(|l| l.id == *id))
    {
        stack.active_idx = top;
    }
    copies.len()
}

/// Move the layers. A path's place is its model (the offset only follows
/// it), so the move is folded back into the model.
pub fn apply(stack: &mut LayerStack, shifts: &[Shift]) {
    for shift in shifts {
        for &m in &shift.members {
            let Some(layer) = stack.layers.get_mut(m) else {
                continue;
            };
            layer.offset.0 += shift.dx;
            layer.offset.1 += shift.dy;
            if matches!(layer.layer_type, LayerType::Vector(VectorGeometry::Path(_))) {
                fold_offset_into_model(layer);
            }
        }
    }
}

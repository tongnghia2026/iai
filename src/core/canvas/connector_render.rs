//! Live dynamic-connector rerouting (vector gap #8).
//!
//! A connector Path layer whose end(s) are stuck to shapes (see
//! [`crate::core::connector`]) has its visible path DERIVED from those anchors and
//! the targets' current positions. This rebuilds it whenever a connected shape
//! moves or resizes — the same "recompute on the structural recomposite,
//! fingerprint-gated, outside undo" pattern the PowerClip clip mask uses. A
//! connector with no anchors is a plain static arrow and costs nothing here.

use super::Canvas;
use crate::core::layer::LayerType;
use crate::core::vector::affine::AffineTransform;
use crate::core::vector::from_shape::{elbow_connector_path, ConnectorRoute};
use crate::core::vector::object::{VectorGeometry, VectorObjectData};
use crate::core::vector::path::{Contour, FillRule, Node, PathData};

impl Canvas {
    /// Whether any layer is an attached (dynamic) connector. A cheap gate so the
    /// common document pays nothing.
    pub fn has_connectors(&self) -> bool {
        self.layer_stack
            .layers
            .iter()
            .any(|l| l.connector.is_some_and(|c| c.is_attached()))
    }

    /// Fingerprint of everything a reroute depends on: each connector's route and
    /// the position/size/coverage of the shapes its ends stick to. Excludes the
    /// connector's OWN path (that is the output), so a manual edit of a free end
    /// does not re-trigger a reroute.
    fn connector_state_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let target_sig = |id: u32, h: &mut std::collections::hash_map::DefaultHasher| match self
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == id)
        {
            Some(t) => {
                t.offset.hash(h);
                t.width.hash(h);
                t.height.hash(h);
                t.tiles.revision_fingerprint().hash(h);
            }
            None => (u32::MAX, id).hash(h),
        };
        for layer in &self.layer_stack.layers {
            let Some(binding) = layer.connector.filter(|c| c.is_attached()) else {
                continue;
            };
            layer.id.hash(&mut h);
            binding.route.hash(&mut h);
            for anchor in [binding.start, binding.end] {
                match anchor {
                    Some(a) => {
                        a.layer_id.hash(&mut h);
                        a.fx.to_bits().hash(&mut h);
                        a.fy.to_bits().hash(&mut h);
                        target_sig(a.layer_id, &mut h);
                    }
                    None => 0u8.hash(&mut h),
                }
            }
        }
        h.finish()
    }

    /// Reroute every attached connector from its anchors and its targets' current
    /// positions. No-op (`false`) when nothing is connected or nothing a connector
    /// depends on changed since the last reroute. Rebuilds derived path/raster
    /// directly (outside history) and marks the changed regions dirty so the next
    /// composite shows the new route.
    pub fn refresh_connectors(&mut self) -> bool {
        if !self.has_connectors() {
            self.connector_fp = 0;
            return false;
        }
        let fp = self.connector_state_fingerprint();
        if fp == self.connector_fp {
            return false;
        }
        self.connector_fp = fp;

        // Read pass: compute each connector's new object without holding a borrow
        // across the target lookups and the later mutation.
        let mut updates: Vec<(usize, VectorObjectData, (i32, i32, u32, u32))> = Vec::new();
        for i in 0..self.layer_stack.layers.len() {
            let layer = &self.layer_stack.layers[i];
            let Some(binding) = layer.connector.filter(|c| c.is_attached()) else {
                continue;
            };
            let LayerType::Vector(VectorGeometry::Path(object)) = &layer.layer_type else {
                continue;
            };
            // A dynamic connector is one open contour; skip anything else.
            let Some(contour) = object.path.contours.first() else {
                continue;
            };
            let (Some(first), Some(last)) = (contour.nodes.first(), contour.nodes.last()) else {
                continue;
            };
            // Current endpoints in canvas space (a free end keeps its position).
            let cur_start = object.transform.apply_point(first.anchor);
            let cur_end = object.transform.apply_point(last.anchor);
            let resolve = |anchor: Option<crate::core::connector::ConnectorAnchor>,
                           fallback: crate::core::geometry::Point| {
                match anchor.and_then(|a| {
                    self.layer_stack
                        .layers
                        .iter()
                        .find(|l| l.id == a.layer_id)
                        .and_then(|t| t.canvas_content_rect())
                        .map(|rect| a.resolve(rect))
                }) {
                    Some((x, y)) => crate::core::geometry::Point::new(x, y),
                    None => fallback,
                }
            };
            let new_start = resolve(binding.start, cur_start);
            let new_end = resolve(binding.end, cur_end);

            // Regenerate in canvas space, then fold back through the object's
            // transform so it rides that transform like the original.
            let canvas_path = elbow_connector_path(
                new_start.x,
                new_start.y,
                new_end.x,
                new_end.y,
                ConnectorRoute::from_u8(binding.route),
            );
            let local_path = match object.transform.inverse() {
                Some(inv) => transform_path(&canvas_path, &inv),
                None => canvas_path,
            };
            if paths_equal(&object.path, &local_path) {
                continue;
            }
            let mut next = object.clone();
            next.path = local_path;
            let old_bounds = (layer.offset.0, layer.offset.1, layer.width, layer.height);
            updates.push((i, next, old_bounds));
        }

        if updates.is_empty() {
            return false;
        }
        for (i, object, old) in updates {
            crate::core::command_vector::apply_object_to_layer(
                &mut self.layer_stack.layers[i],
                object,
            );
            let new = {
                let l = &self.layer_stack.layers[i];
                (l.offset.0, l.offset.1, l.width, l.height)
            };
            self.mark_dirty_layer_bounds(old.0, old.1, old.2, old.3);
            self.mark_dirty_layer_bounds(new.0, new.1, new.2, new.3);
        }
        true
    }
}

/// Map every node anchor of a straight-segment path through `t` (connectors have
/// no curve handles, so only anchors need transforming).
fn transform_path(path: &PathData, t: &AffineTransform) -> PathData {
    PathData::new(
        path.contours
            .iter()
            .map(|c| {
                Contour::new(
                    c.nodes
                        .iter()
                        .map(|n| Node::sharp(t.apply_point(n.anchor)))
                        .collect(),
                    c.closed,
                )
            })
            .collect(),
        FillRule::NonZero,
    )
}

/// Whether two connector paths have the same anchors (so a reroute would be a
/// no-op). Cheap: connectors are a handful of nodes.
fn paths_equal(a: &PathData, b: &PathData) -> bool {
    if a.contours.len() != b.contours.len() {
        return false;
    }
    a.contours.iter().zip(&b.contours).all(|(ca, cb)| {
        ca.nodes.len() == cb.nodes.len()
            && ca.nodes.iter().zip(&cb.nodes).all(|(na, nb)| {
                (na.anchor.x - nb.anchor.x).abs() < 1e-3 && (na.anchor.y - nb.anchor.y).abs() < 1e-3
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::connector::{ConnectorAnchor, ConnectorBinding};
    use crate::core::tile::TileMap;
    use crate::core::vector::affine::AffineTransform;
    use crate::core::vector::style::VectorStyle;

    /// Add an opaque `w×h` box at `(ox,oy)`; return its layer id.
    fn add_box(canvas: &mut Canvas, ox: i32, oy: i32, w: u32, h: u32) -> u32 {
        let idx = canvas.layer_stack.add_layer(canvas.width, canvas.height);
        let l = &mut canvas.layer_stack.layers[idx];
        l.tiles = TileMap::from_rgba(&vec![255u8; (w * h * 4) as usize], w, h);
        l.width = w;
        l.height = h;
        l.offset = (ox, oy);
        l.id
    }

    /// Canvas-space endpoints of the connector layer (transform is identity).
    fn endpoints(canvas: &Canvas, id: u32) -> ((f32, f32), (f32, f32)) {
        let l = canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == id)
            .unwrap();
        let LayerType::Vector(VectorGeometry::Path(o)) = &l.layer_type else {
            panic!("not a path");
        };
        let c = &o.path.contours[0];
        let s = o.transform.apply_point(c.nodes.first().unwrap().anchor);
        let e = o.transform.apply_point(c.nodes.last().unwrap().anchor);
        ((s.x, s.y), (e.x, e.y))
    }

    #[test]
    fn connector_reroutes_when_a_linked_box_moves() {
        let mut canvas = Canvas::new(300, 300);
        let a = add_box(&mut canvas, 10, 10, 20, 20); // centre (20,20)
        let b = add_box(&mut canvas, 100, 100, 20, 20); // centre (110,110)

        // Connector layer: a straight line drawn OFF the box centres, both ends
        // bound to the boxes so the first refresh snaps them on.
        let idx = canvas.layer_stack.add_layer(300, 300);
        let path = elbow_connector_path(5.0, 5.0, 50.0, 60.0, ConnectorRoute::Straight);
        let obj = VectorObjectData::new(path, VectorStyle::default(), AffineTransform::IDENTITY);
        crate::core::command_vector::apply_object_to_layer(
            &mut canvas.layer_stack.layers[idx],
            obj,
        );
        let conn = canvas.layer_stack.layers[idx].id;
        canvas.layer_stack.layers[idx].connector = Some(ConnectorBinding {
            start: Some(ConnectorAnchor {
                layer_id: a,
                fx: 0.5,
                fy: 0.5,
            }),
            end: Some(ConnectorAnchor {
                layer_id: b,
                fx: 0.5,
                fy: 0.5,
            }),
            route: 0,
        });

        assert!(canvas.has_connectors());
        assert!(canvas.refresh_connectors(), "first refresh snaps the ends");
        let (s, e) = endpoints(&canvas, conn);
        assert!(
            (s.0 - 20.0).abs() < 0.6 && (s.1 - 20.0).abs() < 0.6,
            "start on A: {s:?}"
        );
        assert!(
            (e.0 - 110.0).abs() < 0.6 && (e.1 - 110.0).abs() < 0.6,
            "end on B: {e:?}"
        );

        // Nothing moved → the next refresh is a gated no-op.
        assert!(!canvas.refresh_connectors(), "gated when nothing moved");

        // Move box B; the connector's end must follow it.
        let bi = canvas
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == b)
            .unwrap();
        canvas.layer_stack.layers[bi].offset = (200, 200); // centre (210,210)
        assert!(canvas.refresh_connectors(), "box moved → reroute");
        let (s2, e2) = endpoints(&canvas, conn);
        assert!((s2.0 - 20.0).abs() < 0.6, "start still on A");
        assert!(
            (e2.0 - 210.0).abs() < 0.6 && (e2.1 - 210.0).abs() < 0.6,
            "end followed B to {e2:?}"
        );
    }

    #[test]
    fn no_connectors_is_a_cheap_noop() {
        let mut canvas = Canvas::new(32, 32);
        canvas.layer_stack.add_layer(32, 32);
        assert!(!canvas.has_connectors());
        assert!(!canvas.refresh_connectors());
    }
}

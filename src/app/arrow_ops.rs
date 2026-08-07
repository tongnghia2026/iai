use super::render::CanvasEvent;
use super::state::App;
use crate::core::command_vector::{CreatePathLayer, ReplacePathGeometry};
use crate::core::gateway::ChangeKind;
use crate::core::layer::LayerType;
use crate::core::vector::affine::AffineTransform;
use crate::core::vector::from_shape::{elbow_connector_path, ConnectorRoute};
use crate::core::vector::object::{VectorGeometry, VectorObjectData};
use crate::core::vector::path::{Contour, FillRule, Node, PathData};
use crate::core::vector::style::ArrowHead;

impl App {
    /// Settings represented by the selected Arrow layer, if the active layer is
    /// one of the open, arrow-headed Paths created by this tool.
    pub fn active_arrow_settings(&self) -> Option<(f32, u8, u8)> {
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let layer = canvas
            .layer_stack
            .layers
            .get(canvas.layer_stack.active_idx)?;
        if !layer.selected || !layer.visible || layer.locked || layer.is_background {
            return None;
        }
        let LayerType::Vector(VectorGeometry::Path(object)) = &layer.layer_type else {
            return None;
        };
        if object.style.stroke_style.end_arrow.kind == ArrowHead::None {
            return None;
        }
        let contour = object.path.contours.as_slice();
        let [contour] = contour else { return None };
        if contour.closed || !(2..=4).contains(&contour.nodes.len()) {
            return None;
        }
        let route = match contour.nodes.len() {
            2 => 0,
            3 => {
                let start = contour.nodes[0].anchor;
                let bend = contour.nodes[1].anchor;
                if (bend.y - start.y).abs() <= 0.001 {
                    1
                } else {
                    2
                }
            }
            4 => 3,
            _ => unreachable!(),
        };
        Some((
            object.style.stroke_style.width,
            object.style.stroke_style.end_arrow.kind.to_u8(),
            route,
        ))
    }

    /// Re-route the selected Arrow while preserving its endpoints, style and
    /// object transform. The gateway records the geometry replacement for undo.
    pub fn set_active_arrow_route(&mut self, route: u8) {
        if self.active_arrow_settings().is_none() {
            return;
        }
        self.path_style_commit();
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let idx = canvas.layer_stack.active_idx;
        let layer = &mut canvas.layer_stack.layers[idx];
        crate::core::command_vector::fold_offset_into_model(layer);
        let LayerType::Vector(VectorGeometry::Path(object)) = &layer.layer_type else {
            return;
        };
        let Some(contour) = object.path.contours.first() else {
            return;
        };
        let (Some(start), Some(end)) = (contour.nodes.first(), contour.nodes.last()) else {
            return;
        };
        let path = elbow_connector_path(
            start.anchor.x,
            start.anchor.y,
            end.anchor.x,
            end.anchor.y,
            ConnectorRoute::from_u8(route),
        );
        let layer_id = layer.id;
        if canvas
            .execute(
                Box::new(ReplacePathGeometry::new(layer_id, path)),
                ChangeKind::LayerStructure,
            )
            .is_err()
        {
            return;
        }
        canvas.reconcile_path_ink();
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }

    pub fn commit_arrow(&mut self) {
        use crate::tools::arrow::{MODE_BRANCH, MODE_TREE};
        match self.edit.tools.arrow().mode {
            MODE_BRANCH => self.commit_arrow_branch(),
            MODE_TREE => self.commit_arrow_tree(),
            _ => self.commit_arrow_single(),
        }
    }

    /// Add `object` as a new, selected Path layer and repaint. Returns its id.
    fn add_arrow_layer(&mut self, object: VectorObjectData, name: &'static str) -> Option<u32> {
        let doc_idx = self.docs.active_doc_idx;
        let canvas = &mut self.docs.documents[doc_idx].canvas;
        if canvas
            .execute(
                Box::new(CreatePathLayer::new(object, name)),
                ChangeKind::LayerStructure,
            )
            .is_err()
        {
            return None;
        }
        let new_idx = canvas.layer_stack.active_idx;
        for layer in &mut canvas.layer_stack.layers {
            layer.selected = false;
        }
        let new_id = canvas.layer_stack.layers.get_mut(new_idx).map(|layer| {
            layer.selected = true;
            layer.id
        });
        canvas.reconcile_path_ink();
        canvas.layer_revision += 1;
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        new_id
    }

    /// One drag → one standalone arrow / connector layer (the classic behaviour).
    fn commit_arrow_single(&mut self) {
        let fg = self.edit.fg_color;
        let Some(object) = self.edit.tools.arrow_mut().take_arrow_object(fg) else {
            return;
        };
        if self.add_arrow_layer(object, "Arrow").is_some() {
            self.shell.status_msg = "Arrow / connector created".to_string();
        }
    }

    /// One drag → a whole org-chart connector (bar + N down-arrows + parent stub)
    /// as a single editable layer.
    fn commit_arrow_tree(&mut self) {
        let fg = self.edit.fg_color;
        let Some(object) = self.edit.tools.arrow_mut().take_tree_object(fg) else {
            return;
        };
        if self.add_arrow_layer(object, "Tree connector").is_some() {
            self.shell.status_msg = "Tree connector created".to_string();
        }
    }

    /// Branch (multi-arrow) mode: the first drag lays a straight trunk line; each
    /// later drag adds a sub-arrow as a new contour on the SAME layer, so a trunk
    /// with several radiating arrows is one editable object. The branch's start
    /// point is wherever the drag began — snapping already pulls it onto the trunk
    /// when the user starts on it (see the vector snap feature).
    fn commit_arrow_branch(&mut self) {
        let fg = self.edit.fg_color;
        let style = self.edit.tools.arrow().make_style(fg);
        let Some((start, end)) = self.edit.tools.arrow_mut().take_straight_segment() else {
            return;
        };
        let doc_idx = self.docs.active_doc_idx;

        // Append to the current trunk if it still exists as a Path layer.
        if let Some(layer_id) = self.edit.arrow_multi_layer {
            let canvas = &mut self.docs.documents[doc_idx].canvas;
            let existing = canvas
                .layer_stack
                .layers
                .iter()
                .find(|l| l.id == layer_id)
                .and_then(|l| match &l.layer_type {
                    LayerType::Vector(VectorGeometry::Path(obj)) => Some(obj.clone()),
                    _ => None,
                });
            if let Some(obj) = existing {
                // Map the canvas-space branch into the object's local frame so it
                // rides the object's transform like every other contour.
                let (a, b) = match obj.transform.inverse() {
                    Some(inv) => (inv.apply_point(start), inv.apply_point(end)),
                    None => (start, end),
                };
                let mut new_path = obj.path.clone();
                new_path
                    .contours
                    .push(Contour::new(vec![Node::sharp(a), Node::sharp(b)], false));
                if canvas
                    .execute(
                        Box::new(ReplacePathGeometry::new(layer_id, new_path)),
                        ChangeKind::LayerStructure,
                    )
                    .is_ok()
                {
                    canvas.reconcile_path_ink();
                    self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
                    self.shell.status_msg = "Sub-arrow added".to_string();
                }
                return;
            }
            // Trunk was deleted / is no longer a Path — fall through to a fresh one.
            self.edit.arrow_multi_layer = None;
        }

        // No trunk yet: create the base line as a new Path layer and remember it.
        let object = VectorObjectData::new(
            PathData::new(
                vec![Contour::new(
                    vec![Node::sharp(start), Node::sharp(end)],
                    false,
                )],
                FillRule::NonZero,
            ),
            style,
            AffineTransform::IDENTITY,
        );
        self.edit.arrow_multi_layer = self.add_arrow_layer(object, "Arrow");
        self.shell.status_msg = "Trunk drawn — drag again from it to add sub-arrows".to_string();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::Canvas;
    use crate::tools::{PointerEvent, ToolCtx, ToolId};

    /// Drive one Arrow gesture (press → drag → release) then commit it, exactly as
    /// the pointer handler does.
    fn arrow_drag(app: &mut App, x0: f32, y0: f32, x1: f32, y1: f32) {
        {
            let mut ctx = ToolCtx::new(
                &mut app.docs.documents[0],
                app.edit.fg_color,
                app.edit.bg_color,
                1.0,
                0.0,
                0.0,
            );
            let _ = app.edit.tools.on_press(PointerEvent::new(x0, y0), &mut ctx);
            let _ = app.edit.tools.on_drag(PointerEvent::new(x1, y1), &mut ctx);
            let _ = app
                .edit
                .tools
                .on_release(PointerEvent::new(x1, y1), &mut ctx);
        }
        app.commit_arrow();
    }

    fn multi_layer_contours(app: &App) -> usize {
        let id = app.edit.arrow_multi_layer.expect("a trunk layer");
        let canvas = &app.docs.documents[0].canvas;
        let layer = canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == id)
            .expect("trunk layer present");
        match &layer.layer_type {
            LayerType::Vector(VectorGeometry::Path(o)) => o.path.contours.len(),
            _ => 0,
        }
    }

    fn vector_layer_count(app: &App) -> usize {
        app.docs.documents[0]
            .canvas
            .layer_stack
            .layers
            .iter()
            .filter(|l| matches!(l.layer_type, LayerType::Vector(VectorGeometry::Path(_))))
            .count()
    }

    fn arrow_app() -> App {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(400, 400);
        app.edit.tools.select(ToolId::Arrow);
        app.edit.view.zoom = 1.0;
        app.edit.view.offset_x = 0.0;
        app.edit.view.offset_y = 0.0;
        // select() resets the branch group; keep it None for the first drag.
        app.edit.arrow_multi_layer = None;
        app
    }

    #[test]
    fn branch_mode_keeps_sub_arrows_on_one_layer() {
        let mut app = arrow_app();
        app.edit.tools.arrow_mut().mode = crate::tools::arrow::MODE_BRANCH;

        // Trunk.
        arrow_drag(&mut app, 100.0, 100.0, 100.0, 300.0);
        assert_eq!(vector_layer_count(&app), 1, "trunk is one layer");
        assert_eq!(multi_layer_contours(&app), 1, "trunk is one contour");

        // Two sub-arrows starting on the trunk → same layer, more contours.
        arrow_drag(&mut app, 100.0, 300.0, 180.0, 360.0);
        arrow_drag(&mut app, 100.0, 300.0, 20.0, 360.0);
        assert_eq!(vector_layer_count(&app), 1, "still one layer");
        assert_eq!(multi_layer_contours(&app), 3, "trunk + 2 branches");
    }

    #[test]
    fn single_mode_makes_a_layer_per_drag() {
        let mut app = arrow_app();
        assert!(!app.edit.tools.arrow().is_branch());
        arrow_drag(&mut app, 10.0, 10.0, 60.0, 10.0);
        arrow_drag(&mut app, 10.0, 40.0, 60.0, 40.0);
        assert_eq!(vector_layer_count(&app), 2, "each drag is its own arrow");
        assert!(app.edit.arrow_multi_layer.is_none());
    }

    #[test]
    fn finishing_the_group_starts_a_new_trunk() {
        let mut app = arrow_app();
        app.edit.tools.arrow_mut().mode = crate::tools::arrow::MODE_BRANCH;
        arrow_drag(&mut app, 100.0, 100.0, 100.0, 300.0);
        let first = app.edit.arrow_multi_layer;
        // Esc / Enter / re-selecting the tool ends the group.
        app.edit.arrow_multi_layer = None;
        arrow_drag(&mut app, 200.0, 100.0, 200.0, 300.0);
        assert_ne!(app.edit.arrow_multi_layer, first, "a fresh trunk layer");
        assert_eq!(vector_layer_count(&app), 2, "two independent trunks");
    }

    #[test]
    fn tree_mode_generates_one_connector_layer() {
        let mut app = arrow_app();
        app.edit.tools.arrow_mut().mode = crate::tools::arrow::MODE_TREE;
        app.edit.tools.arrow_mut().tree_count = 4;
        // One drag of the layout box.
        arrow_drag(&mut app, 40.0, 40.0, 360.0, 160.0);
        assert_eq!(vector_layer_count(&app), 1, "one connector layer");
        // bar + stub + 4 drops = 6 contours.
        let canvas = &app.docs.documents[0].canvas;
        let obj = canvas
            .layer_stack
            .layers
            .iter()
            .find_map(|l| match &l.layer_type {
                LayerType::Vector(VectorGeometry::Path(o)) => Some(o),
                _ => None,
            })
            .expect("a path layer");
        assert_eq!(obj.path.contours.len(), 6);
        // Tree mode doesn't accumulate like branch mode.
        assert!(app.edit.arrow_multi_layer.is_none());
    }
}

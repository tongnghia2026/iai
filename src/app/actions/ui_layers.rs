//! apply_ui_actions handlers: the layer panel — add/remove/select, locks,
//! opacity/blend, ordering, merges, groups, masks and paint target.
//! Split out of actions.rs (phase 2).

use crate::app::render::CanvasEvent;
use crate::app::state::App;
use crate::ui::UiActions;

impl App {
    pub(super) fn handle_layer_actions(&mut self, actions: &mut UiActions) {
        macro_rules! undoable_layer_op {
            ($name:expr, $block:block) => {{
                let (cw, ch) = {
                    let d = &self.docs.documents[self.docs.active_doc_idx];
                    (d.canvas.width, d.canvas.height)
                };
                let mut cmd = crate::core::command::LayerStructureCommand::capture_before(
                    $name,
                    &self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack,
                    cw,
                    ch,
                );
                $block;
                cmd.capture_after(
                    &self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack,
                    cw,
                    ch,
                );
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .record(Box::new(cmd));
            }};
        }

        if actions.layers.add_layer {
            undoable_layer_op!("Add Layer", {
                let (cw, ch) = {
                    let d = &self.docs.documents[self.docs.active_doc_idx];
                    (d.canvas.width, d.canvas.height)
                };
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .add_layer(cw, ch);
            });
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        }
        if let Some(adj) = actions.layers.add_adjustment_layer.take() {
            // Adjustment layers composite in RGB on the mirrors — the ink
            // planes would no longer be what prints/exports, so refuse in CMYK.
            if self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .is_cmyk()
            {
                self.shell.status_msg = "Adjustment layer chưa hỗ trợ ở chế độ CMYK".to_string();
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            } else {
                undoable_layer_op!("Add Adjustment Layer", {
                    let (cw, ch) = {
                        let d = &self.docs.documents[self.docs.active_doc_idx];
                        (d.canvas.width, d.canvas.height)
                    };
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .add_adjustment_layer(adj, cw, ch);
                });
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            }
        }
        if let Some(idx) = actions.layers.duplicate_layer.take() {
            let is_grp = self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack
                .layers
                .get(idx)
                .map_or(false, |l| l.is_group());
            undoable_layer_op!("Duplicate Layer", {
                let ls = &mut self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack;
                if is_grp {
                    ls.duplicate_group(idx);
                } else {
                    ls.duplicate_layer(idx);
                }
            });
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        }
        if actions.layers.layer_via_copy {
            let active = self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack
                .active_idx;
            if self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack
                .layers
                .get(active)
                .map_or(false, |l| l.is_group())
            {
                undoable_layer_op!("Duplicate Group", {
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .duplicate_group(active);
                });
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            } else {
                let had_selection = self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .selection
                    .active;
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .begin_undo_group("Layer via Copy");
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_via_copy();
                if had_selection {
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .deselect();
                }
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .end_undo_group();
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
                if had_selection {
                    self.apply_canvas_event(CanvasEvent::SelectionChanged);
                }
            }
        }
        if let Some(idx) = actions.layers.rasterize_layer.take() {
            let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
            if let Some(id) = canvas.layer_stack.layers.get(idx).map(|l| l.id) {
                if canvas
                    .execute(
                        Box::new(crate::core::command_vector::RasterizeVectorLayer::new(id)),
                        crate::core::gateway::ChangeKind::LayerStructure,
                    )
                    .is_ok()
                {
                    self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
                }
            }
        }
        if let Some(idx) = actions.layers.convert_to_curves.take() {
            self.convert_shape_to_path(idx);
        }
        if let Some(idx) = actions.layers.text_to_curves.take() {
            self.text_to_curves(idx);
        }
        if let Some(op) = actions.layers.boolean_op.take() {
            self.apply_boolean(op);
        }
        if actions.layers.powerclip_place {
            self.powerclip_place();
        }
        if actions.layers.powerclip_release {
            self.powerclip_release();
        }
        if let Some(mode) = actions.layers.powerclip_arrange.take() {
            self.powerclip_arrange(mode);
        }
        if std::mem::take(&mut actions.layers.powerclip_edit_contents) {
            self.powerclip_edit_contents();
        }
        if actions.layers.toggle_clipping_mask {
            self.toggle_clipping_mask();
        }
        if let Some(idx) = actions.layers.remove_layer.take() {
            if self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .delete_layer_at(idx)
            {
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            }
        }
        if let Some((idx, ctrl, shift)) = actions.layers.select_layer.take() {
            if ctrl {
                if idx
                    < self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .layers
                        .len()
                {
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .layers[idx]
                        .selected ^= true;
                    if self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .layers[idx]
                        .selected
                    {
                        self.docs.documents[self.docs.active_doc_idx]
                            .canvas
                            .layer_stack
                            .active_idx = idx;
                    }
                }
            } else if shift {
                let current_idx = self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .active_idx;
                let min_idx = current_idx.min(idx);
                let max_idx = current_idx.max(idx).min(
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .layers
                        .len()
                        - 1,
                );
                for i in min_idx..=max_idx {
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .layers[i]
                        .selected = true;
                }
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .active_idx = idx;
            } else {
                for l in &mut self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers
                {
                    l.selected = false;
                }
                if idx
                    < self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .layers
                        .len()
                {
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .layers[idx]
                        .selected = true;
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .active_idx = idx;
                }
            }
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_revision += 1;
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if let Some(idx) = actions.layers.load_layer_selection.take() {
            if self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .load_layer_alpha_selection(idx)
            {
                self.apply_canvas_event(CanvasEvent::SelectionChanged);
                self.shell.status_msg = "Loaded layer transparency as selection".to_string();
            } else {
                self.shell.status_msg = "Layer has no pixels to select".to_string();
            }
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if let Some((idx, name)) = actions.doc.rename_confirmed.take() {
            undoable_layer_op!("Rename Layer", {
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .rename_layer(idx, &name);
            });
        }
        if let Some(idx) = actions.layers.unlock_background_layer.take() {
            let unlocked = self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack
                .layers
                .get(idx)
                .map_or(false, |l| l.is_background);
            if unlocked {
                undoable_layer_op!("Layer From Background", {
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .unlock_background_layer(idx);
                });
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
                self.shell.status_msg = "Background converted to Layer 0".to_string();
            }
        }
        if let Some(idx) = actions.layers.toggle_visible.take() {
            if idx
                < self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers
                    .len()
            {
                undoable_layer_op!("Toggle Visibility", {
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .layers[idx]
                        .visible = !self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .layers[idx]
                        .visible;
                });
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            }
        }
        if let Some(idx) = actions.layers.toggle_lock.take() {
            if idx
                < self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers
                    .len()
            {
                undoable_layer_op!("Toggle Lock", {
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .layers[idx]
                        .locked = !self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .layers[idx]
                        .locked;
                });
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_revision += 1;
            }
        }
        if let Some(idx) = actions.layers.toggle_lock_alpha.take() {
            if idx
                < self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers
                    .len()
            {
                undoable_layer_op!("Toggle Lock Alpha", {
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .layers[idx]
                        .lock_alpha = !self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .layers[idx]
                        .lock_alpha;
                });
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_revision += 1;
            }
        }
        if let Some((idx, op)) = actions.layers.set_opacity.take() {
            if idx
                < self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers
                    .len()
            {
                if self.edit.pending_opacity_cmd.is_none() {
                    let (cw, ch) = {
                        let d = &self.docs.documents[self.docs.active_doc_idx];
                        (d.canvas.width, d.canvas.height)
                    };
                    self.edit.pending_opacity_cmd =
                        Some(crate::core::command::LayerStructureCommand::capture_before(
                            "Set Opacity",
                            &self.docs.documents[self.docs.active_doc_idx]
                                .canvas
                                .layer_stack,
                            cw,
                            ch,
                        ));
                }
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers[idx]
                    .opacity = op;
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_revision += 1;
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            }
        }
        if actions.layers.set_opacity_commit {
            if let Some(mut cmd) = self.edit.pending_opacity_cmd.take() {
                let (cw, ch) = {
                    let d = &self.docs.documents[self.docs.active_doc_idx];
                    (d.canvas.width, d.canvas.height)
                };
                cmd.capture_after(
                    &self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack,
                    cw,
                    ch,
                );
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .record(Box::new(cmd));
            }
        }
        if let Some((idx, mode)) = actions.layers.set_blend_mode.take() {
            if idx
                < self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers
                    .len()
            {
                undoable_layer_op!("Set Blend Mode", {
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .layers[idx]
                        .blend_mode = mode;
                });
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_revision += 1;
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            }
        }
        if let Some(idx) = actions.layers.move_layer_up.take() {
            undoable_layer_op!("Move Layer Up", {
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .move_layer_up(idx);
            });
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        }
        if let Some(idx) = actions.layers.move_layer_down.take() {
            undoable_layer_op!("Move Layer Down", {
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .move_layer_down(idx);
            });
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        }
        if let Some((src, dst)) = actions.layers.move_layer_to.take() {
            undoable_layer_op!("Move Layer", {
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .drag_layer_to(src, dst);
            });
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        }
        if actions.layers.merge_selected {
            let is_large = self.win.gpu.as_ref().map_or(false, |g| g.is_large_canvas);
            if is_large {
                self.shell.status_msg =
                    "Merge không hỗ trợ canvas > 25M pixels (Viewport Streaming mode)".to_string();
            } else if self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .merge_selected()
            {
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            }
        }
        if let Some(idx) = actions.layers.merge_down.take() {
            // merge_down is tile-native (256-px chunks) — runs under Viewport Streaming.
            undoable_layer_op!("Merge Down", {
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .merge_down(idx);
            });
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        }
        if actions.layers.merge_all {
            // Flatten Image is tile-native (chunked 8- and 16-bit) — runs under
            // Viewport Streaming.
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .flatten_all();
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        }
        if let Some(idx) = actions.layers.merge_group.take() {
            // Merge Group is tile-native (per-chunk subtree composite + opacity/mask
            // bake), so it runs under Viewport Streaming with no >25M px gate.
            if self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack
                .layers
                .get(idx)
                .map_or(false, |l| l.is_group())
            {
                let (mcw, mch) = {
                    let d = &self.docs.documents[self.docs.active_doc_idx];
                    (d.canvas.width, d.canvas.height)
                };
                undoable_layer_op!("Merge Group", {
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .merge_group(idx, mcw, mch);
                });
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
                self.shell.status_msg = "Merged group".to_string();
            }
        }
        if actions.layers.merge_visible {
            // Merge Visible is tile-native (chunked flatten into tiles), so it runs
            // under Viewport Streaming with no >25M px gate.
            let (mcw, mch) = {
                let d = &self.docs.documents[self.docs.active_doc_idx];
                (d.canvas.width, d.canvas.height)
            };
            undoable_layer_op!("Stamp Visible", {
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .merge_visible(mcw, mch);
            });
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            self.shell.status_msg = "Stamped visible layers".to_string();
        }
        if actions.layers.group_selected {
            self.do_group_selected();
        }
        if let Some(idx) = actions.layers.ungroup.take() {
            self.do_ungroup(idx);
        }
        if let Some(idx) = actions.layers.toggle_group_expanded.take() {
            if let Some(l) = self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack
                .layers
                .get_mut(idx)
            {
                if l.is_group() {
                    l.expanded = !l.expanded;
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_revision += 1;
                    if let Some(w) = &self.win.window {
                        w.request_redraw();
                    }
                }
            }
        }
        if let Some(idx) = actions.layers.add_mask_white.take() {
            if idx
                < self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers
                    .len()
            {
                undoable_layer_op!("Add Layer Mask", {
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .layers[idx]
                        .add_mask(true);
                });
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            }
        }
        if let Some(idx) = actions.layers.add_mask_black.take() {
            if idx
                < self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers
                    .len()
            {
                undoable_layer_op!("Add Hide-All Layer Mask", {
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .layers[idx]
                        .add_mask(false);
                });
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            }
        }
        if let Some(idx) = actions.layers.apply_mask.take() {
            if idx
                < self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers
                    .len()
            {
                undoable_layer_op!("Apply Layer Mask", {
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .apply_layer_mask(idx);
                });
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            }
        }
        if let Some(idx) = actions.layers.delete_mask.take() {
            if idx
                < self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers
                    .len()
            {
                undoable_layer_op!("Delete Layer Mask", {
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .layers[idx]
                        .delete_mask();
                });
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            }
        }
        if let Some(idx) = actions.layers.toggle_mask_enabled.take() {
            if idx
                < self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers
                    .len()
            {
                undoable_layer_op!("Toggle Layer Mask", {
                    if let Some(mask) = self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .layers[idx]
                        .mask
                        .as_mut()
                    {
                        mask.enabled = !mask.enabled;
                    }
                });
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            }
        }
        if let Some(idx) = actions.layers.toggle_mask_link.take() {
            if let Some(layer) = self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack
                .layers
                .get_mut(idx)
            {
                layer.mask_linked = !layer.mask_linked;
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_revision += 1;
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
        }
        if let Some(idx) = actions.layers.invert_active.take() {
            self.do_invert_active(idx);
        }
        if let Some((idx, target)) = actions.layers.set_paint_target.take() {
            if idx
                < self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers
                    .len()
            {
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers[idx]
                    .paint_target = target;
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_revision += 1;
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
        }
    }
}

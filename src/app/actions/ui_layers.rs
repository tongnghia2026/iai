//! apply_ui_actions handlers: the layer panel — add/remove/select, locks,
//! opacity/blend, ordering, merges, groups, masks and paint target.
//! Split out of actions.rs (phase 2).

use super::MERGE_REFUSED_MSG;
use crate::app::render::CanvasEvent;
use crate::app::state::App;
use crate::ui::UiActions;

impl App {
    pub(super) fn handle_layer_actions(&mut self, actions: &mut UiActions) {
        macro_rules! undoable_layer_op {
            ($name:expr, $block:block) => {{
                let (cw, ch) = {
                    let d = self.docs.active();
                    (d.canvas.width, d.canvas.height)
                };
                let mut cmd = crate::core::command::LayerStructureCommand::capture_before(
                    $name,
                    &self.docs.active().canvas.layer_stack,
                    cw,
                    ch,
                );
                $block;
                cmd.capture_after(&self.docs.active().canvas.layer_stack, cw, ch);
                self.docs.active_mut().canvas.record(Box::new(cmd));
            }};
        }

        // Like `undoable_layer_op!`, but the block yields whether anything
        // changed; a no-op records no history step (so Ctrl+Z never looks dead).
        macro_rules! undoable_layer_op_if {
            ($name:expr, $block:block) => {{
                let (cw, ch) = {
                    let d = self.docs.active();
                    (d.canvas.width, d.canvas.height)
                };
                let mut cmd = crate::core::command::LayerStructureCommand::capture_before(
                    $name,
                    &self.docs.active().canvas.layer_stack,
                    cw,
                    ch,
                );
                let changed: bool = $block;
                if changed {
                    cmd.capture_after(&self.docs.active().canvas.layer_stack, cw, ch);
                    self.docs.active_mut().canvas.record(Box::new(cmd));
                }
                changed
            }};
        }

        if actions.layers.add_layer {
            undoable_layer_op!("Add Layer", {
                let (cw, ch) = {
                    let d = self.docs.active();
                    (d.canvas.width, d.canvas.height)
                };
                let ls = &mut self.docs.active_mut().canvas.layer_stack;
                // Select ONLY the new layer so the next copy/merge/group acts on
                // it, not on a stale prior selection (e.g. a group folder that
                // was left selected and hidden).
                let new_idx = ls.add_layer(cw, ch);
                for (i, l) in ls.layers.iter_mut().enumerate() {
                    l.selected = i == new_idx;
                }
            });
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        }
        if let Some(adj) = actions.layers.add_adjustment_layer.take() {
            // Adjustment layers composite in RGB on the mirrors — the ink
            // planes would no longer be what prints/exports, so refuse in CMYK.
            if self.docs.active().canvas.is_cmyk() {
                self.shell.status_msg = "Adjustment layer chưa hỗ trợ ở chế độ CMYK".to_string();
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            } else {
                undoable_layer_op!("Add Adjustment Layer", {
                    let (cw, ch) = {
                        let d = self.docs.active();
                        (d.canvas.width, d.canvas.height)
                    };
                    let ls = &mut self.docs.active_mut().canvas.layer_stack;
                    let new_idx = ls.add_adjustment_layer(adj, cw, ch);
                    for (i, l) in ls.layers.iter_mut().enumerate() {
                        l.selected = i == new_idx;
                    }
                });
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            }
        }
        if let Some(idx) = actions.layers.duplicate_layer.take() {
            undoable_layer_op!("Duplicate Layer", {
                let ls = &mut self.docs.active_mut().canvas.layer_stack;
                // A folder is duplicated with its contents, alone or as part
                // of a multi-selection.
                if !ls.duplicate_selected_from(idx) {
                    let copy = ls.duplicate_node(idx);
                    ls.select_only(copy);
                }
            });
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        }
        if actions.layers.layer_via_copy {
            if self.docs.active_mut().canvas.duplicate_active_group() {
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            } else {
                let had_selection = self.docs.active().canvas.selection.active;
                self.docs
                    .active_mut()
                    .canvas
                    .begin_undo_group("Layer via Copy");
                self.docs.active_mut().canvas.layer_via_copy();
                if had_selection {
                    self.docs.active_mut().canvas.deselect();
                }
                self.docs.active_mut().canvas.end_undo_group();
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
                if had_selection {
                    self.apply_canvas_event(CanvasEvent::SelectionChanged);
                }
            }
        }
        if let Some(idx) = actions.layers.rasterize_layer.take() {
            let canvas = &mut self.docs.active_mut().canvas;
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
        if std::mem::take(&mut actions.layers.powerclip_cancel_editing) {
            self.powerclip_cancel_editing();
        }
        if actions.layers.toggle_clipping_mask {
            self.toggle_clipping_mask();
        }
        if let Some(idx) = actions.layers.remove_layer.take() {
            if self.docs.active_mut().canvas.delete_layer_at(idx) {
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            }
        }
        if let Some((idx, ctrl, shift)) = actions.layers.select_layer.take() {
            if ctrl {
                self.docs
                    .active_mut()
                    .canvas
                    .layer_stack
                    .toggle_layer_selected(idx);
            } else if shift {
                let ls = &mut self.docs.active_mut().canvas.layer_stack;
                let current_idx = ls.active_idx;
                let last = ls.layers.len().saturating_sub(1);
                let min_idx = current_idx.min(idx);
                let max_idx = current_idx.max(idx).min(last);
                // Shift selects a contiguous range, but never a layer hidden
                // inside a collapsed group folder: the user can't see it in the
                // panel, and pulling it into a later merge would corrupt the
                // group.
                let picks: Vec<usize> = (min_idx..=max_idx)
                    .filter(|&i| !ls.is_collapsed_hidden(i))
                    .collect();
                for i in picks {
                    ls.layers[i].selected = true;
                }
                ls.active_idx = idx;
            } else {
                for l in &mut self.docs.active_mut().canvas.layer_stack.layers {
                    l.selected = false;
                }
                if idx < self.docs.active().canvas.layer_stack.layers.len() {
                    self.docs.active_mut().canvas.layer_stack.layers[idx].selected = true;
                    self.docs.active_mut().canvas.layer_stack.active_idx = idx;
                }
            }
            self.docs.active_mut().canvas.layer_revision += 1;
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if let Some(idx) = actions.layers.load_layer_selection.take() {
            if self
                .docs
                .active_mut()
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
                self.docs
                    .active_mut()
                    .canvas
                    .layer_stack
                    .rename_layer(idx, &name);
            });
            // The panel's names are cached on layer_revision.
            self.docs.active_mut().canvas.layer_revision += 1;
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if let Some(idx) = actions.layers.unlock_background_layer.take() {
            let unlocked = self
                .docs
                .active()
                .canvas
                .layer_stack
                .layers
                .get(idx)
                .is_some_and(|l| l.is_background);
            if unlocked {
                undoable_layer_op!("Layer From Background", {
                    self.docs
                        .active_mut()
                        .canvas
                        .layer_stack
                        .unlock_background_layer(idx);
                });
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
                self.shell.status_msg = "Background converted to Layer 0".to_string();
            }
        }
        if let Some(idx) = actions.layers.toggle_visible.take() {
            if idx < self.docs.active().canvas.layer_stack.layers.len() {
                undoable_layer_op!("Toggle Visibility", {
                    self.docs.active_mut().canvas.layer_stack.layers[idx].visible =
                        !self.docs.active().canvas.layer_stack.layers[idx].visible;
                });
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            }
        }
        if let Some(idx) = actions.layers.toggle_lock.take() {
            if idx < self.docs.active().canvas.layer_stack.layers.len() {
                undoable_layer_op!("Toggle Lock", {
                    self.docs.active_mut().canvas.layer_stack.layers[idx].locked =
                        !self.docs.active().canvas.layer_stack.layers[idx].locked;
                });
                self.docs.active_mut().canvas.layer_revision += 1;
            }
        }
        if let Some(idx) = actions.layers.toggle_lock_alpha.take() {
            if idx < self.docs.active().canvas.layer_stack.layers.len() {
                undoable_layer_op!("Toggle Lock Alpha", {
                    self.docs.active_mut().canvas.layer_stack.layers[idx].lock_alpha =
                        !self.docs.active().canvas.layer_stack.layers[idx].lock_alpha;
                });
                self.docs.active_mut().canvas.layer_revision += 1;
            }
        }
        if let Some((idx, op)) = actions.layers.set_opacity.take() {
            if idx < self.docs.active().canvas.layer_stack.layers.len() {
                if self.edit.pending_opacity_cmd.is_none() {
                    let (cw, ch) = {
                        let d = self.docs.active();
                        (d.canvas.width, d.canvas.height)
                    };
                    self.edit.pending_opacity_cmd =
                        Some(crate::core::command::LayerStructureCommand::capture_before(
                            "Set Opacity",
                            &self.docs.active().canvas.layer_stack,
                            cw,
                            ch,
                        ));
                }
                self.docs.active_mut().canvas.layer_stack.layers[idx].opacity = op;
                self.docs.active_mut().canvas.layer_revision += 1;
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            }
        }
        if actions.layers.set_opacity_commit {
            if let Some(mut cmd) = self.edit.pending_opacity_cmd.take() {
                let (cw, ch) = {
                    let d = self.docs.active();
                    (d.canvas.width, d.canvas.height)
                };
                cmd.capture_after(&self.docs.active().canvas.layer_stack, cw, ch);
                self.docs.active_mut().canvas.record(Box::new(cmd));
            }
        }
        if let Some((idx, mode)) = actions.layers.set_blend_mode.take() {
            if idx < self.docs.active().canvas.layer_stack.layers.len() {
                undoable_layer_op!("Set Blend Mode", {
                    self.docs.active_mut().canvas.layer_stack.layers[idx].blend_mode = mode;
                });
                self.docs.active_mut().canvas.layer_revision += 1;
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            }
        }
        if let Some(idx) = actions.layers.move_layer_up.take() {
            if undoable_layer_op_if!("Move Layer Up", {
                self.docs.active_mut().canvas.layer_stack.move_layer_up(idx)
            }) {
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            }
        }
        if let Some(idx) = actions.layers.move_layer_down.take() {
            if undoable_layer_op_if!("Move Layer Down", {
                self.docs
                    .active_mut()
                    .canvas
                    .layer_stack
                    .move_layer_down(idx)
            }) {
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            }
        }
        if let Some((src, dst)) = actions.layers.move_layer_to.take() {
            if undoable_layer_op_if!("Move Layer", {
                self.docs
                    .active_mut()
                    .canvas
                    .layer_stack
                    .drag_layer_to(src, dst)
            }) {
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            }
        }
        if actions.layers.merge_selected {
            // Merge Selected / Merge Down are tile-native (256-px chunks), so they
            // run under Viewport Streaming too.
            if self.docs.active_mut().canvas.merge_selected() {
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            } else {
                self.shell.status_msg = MERGE_REFUSED_MSG.to_string();
            }
        }
        if let Some(idx) = actions.layers.merge_down.take() {
            if self.docs.active_mut().canvas.merge_down(idx) {
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            } else {
                self.shell.status_msg = MERGE_REFUSED_MSG.to_string();
            }
        }
        if actions.layers.merge_all {
            let _ = self.run_now("layer.flatten");
        }
        if let Some(idx) = actions.layers.merge_group.take() {
            // Merge Group is tile-native (per-chunk subtree composite + opacity/mask
            // bake), so it runs under Viewport Streaming with no >25M px gate.
            if self
                .docs
                .active()
                .canvas
                .layer_stack
                .layers
                .get(idx)
                .is_some_and(|l| l.is_group())
                && self.docs.active_mut().canvas.merge_group(idx)
            {
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
                self.shell.status_msg = "Merged group".to_string();
            }
        }
        if actions.layers.merge_visible {
            // Merge Visible is tile-native (chunked flatten into tiles), so it runs
            // under Viewport Streaming with no >25M px gate. The Canvas wrapper
            // picks the 16-bit composite on a 16-bit document, like Ctrl+Shift+E.
            if self.docs.active_mut().canvas.merge_visible() {
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
                self.shell.status_msg = "Stamped visible layers".to_string();
            }
        }
        if actions.layers.group_selected {
            self.do_group_selected();
        }
        if let Some(idx) = actions.layers.ungroup.take() {
            self.do_ungroup(idx);
        }
        if let Some(idx) = actions.layers.toggle_group_expanded.take() {
            if let Some(l) = self
                .docs
                .active_mut()
                .canvas
                .layer_stack
                .layers
                .get_mut(idx)
            {
                if l.is_group() {
                    l.expanded = !l.expanded;
                    self.docs.active_mut().canvas.layer_revision += 1;
                    if let Some(w) = &self.win.window {
                        w.request_redraw();
                    }
                }
            }
        }
        if let Some(idx) = actions.layers.add_mask_white.take() {
            if idx < self.docs.active().canvas.layer_stack.layers.len() {
                undoable_layer_op!("Add Layer Mask", {
                    self.docs.active_mut().canvas.layer_stack.layers[idx].add_mask(true);
                });
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            }
        }
        if let Some(idx) = actions.layers.add_mask_black.take() {
            if idx < self.docs.active().canvas.layer_stack.layers.len() {
                undoable_layer_op!("Add Hide-All Layer Mask", {
                    self.docs.active_mut().canvas.layer_stack.layers[idx].add_mask(false);
                });
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            }
        }
        if let Some(idx) = actions.layers.apply_mask.take() {
            if idx < self.docs.active().canvas.layer_stack.layers.len() {
                if undoable_layer_op_if!("Apply Layer Mask", {
                    self.docs
                        .active_mut()
                        .canvas
                        .layer_stack
                        .apply_layer_mask(idx)
                }) {
                    self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
                } else {
                    self.shell.status_msg =
                        "Không áp dụng được mask: nhóm có chữ, vector hoặc adjustment".to_string();
                }
            }
        }
        if let Some(idx) = actions.layers.delete_mask.take() {
            if idx < self.docs.active().canvas.layer_stack.layers.len() {
                undoable_layer_op!("Delete Layer Mask", {
                    self.docs.active_mut().canvas.layer_stack.layers[idx].delete_mask();
                });
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            }
        }
        if let Some(idx) = actions.layers.toggle_mask_enabled.take() {
            if idx < self.docs.active().canvas.layer_stack.layers.len() {
                undoable_layer_op!("Toggle Layer Mask", {
                    if let Some(mask) = self.docs.active_mut().canvas.layer_stack.layers[idx]
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
            if let Some(layer) = self
                .docs
                .active_mut()
                .canvas
                .layer_stack
                .layers
                .get_mut(idx)
            {
                layer.mask_linked = !layer.mask_linked;
                self.docs.active_mut().canvas.layer_revision += 1;
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
        }
        if let Some(idx) = actions.layers.invert_active.take() {
            self.do_invert_active(idx);
        }
        if let Some((idx, target)) = actions.layers.set_paint_target.take() {
            if idx < self.docs.active().canvas.layer_stack.layers.len() {
                self.docs.active_mut().canvas.layer_stack.layers[idx].paint_target = target;
                self.docs.active_mut().canvas.layer_revision += 1;
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
        }
    }
}

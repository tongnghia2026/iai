//! apply_ui_actions handlers: the layer panel — add/remove/select, locks,
//! opacity/blend, ordering, merges, groups, masks and paint target.
//! Split out of actions.rs (phase 2).

use crate::app::render::CanvasEvent;
use crate::app::state::App;
use crate::ui::UiActions;

impl App {
    pub(super) fn handle_layer_actions(&mut self, actions: &mut UiActions) {
        macro_rules! undoable_layer_op {
            ($name:expr_2021, $block:block) => {{
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
            ($name:expr_2021, $block:block) => {{
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
        if std::mem::take(&mut actions.layers.powerclip_edit_contents) {
            self.powerclip_edit_contents();
        }
        if std::mem::take(&mut actions.layers.powerclip_cancel_editing) {
            self.powerclip_cancel_editing();
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
        if let Some((idx, op)) = actions.layers.set_opacity.take()
            && idx < self.docs.active().canvas.layer_stack.layers.len()
        {
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
        if actions.layers.set_opacity_commit
            && let Some(mut cmd) = self.edit.pending_opacity_cmd.take()
        {
            let (cw, ch) = {
                let d = self.docs.active();
                (d.canvas.width, d.canvas.height)
            };
            cmd.capture_after(&self.docs.active().canvas.layer_stack, cw, ch);
            self.docs.active_mut().canvas.record(Box::new(cmd));
        }
        if let Some((src, dst)) = actions.layers.move_layer_to.take()
            && undoable_layer_op_if!("Move Layer", {
                self.docs
                    .active_mut()
                    .canvas
                    .layer_stack
                    .drag_layer_to(src, dst)
            })
        {
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        }
        if let Some(idx) = actions.layers.toggle_group_expanded.take()
            && let Some(l) = self
                .docs
                .active_mut()
                .canvas
                .layer_stack
                .layers
                .get_mut(idx)
            && l.is_group()
        {
            l.expanded = !l.expanded;
            self.docs.active_mut().canvas.layer_revision += 1;
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if let Some((idx, target)) = actions.layers.set_paint_target.take()
            && idx < self.docs.active().canvas.layer_stack.layers.len()
        {
            self.docs.active_mut().canvas.layer_stack.layers[idx].paint_target = target;
            self.docs.active_mut().canvas.layer_revision += 1;
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
    }
}

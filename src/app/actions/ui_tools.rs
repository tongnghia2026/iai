//! apply_ui_actions handlers: text tool, transform/crop lifecycle and all
//! per-tool option fields. Split out of actions.rs (phase 2).

use crate::app::state::App;
use crate::ui::UiActions;
use winit::event_loop::ActiveEventLoop;

impl App {
    pub(super) fn handle_text_actions(&mut self, actions: &mut UiActions) {
        if self.edit.text_edit.is_some()
            && self.edit.tools.active_id() != crate::tools::ToolId::Text
        {
            self.commit_text_edit();
        }

        if let Some(buf) = actions.tool.text_buffer.take() {
            self.update_text_buffer(buf);
        }
        if let Some((selection, caret)) = actions.tool.text_cursor.take() {
            self.update_text_cursor_cache(selection, caret);
        }
        if let Some(px) = actions.tool.set_text_font_px.take() {
            let px = px.clamp(4.0, 1600.0);
            // Apply to the selection (or pending caret) before changing the
            // default, so unselected characters keep their prior size.
            if self.edit.text_edit.is_some() {
                self.apply_text_style_to_selection(None, Some(px), None, None, None, None);
            }
            self.edit.text_font_px = px;
            // An explicit choice wins over canvas-sized auto defaults.
            self.edit.text_font_px_auto = false;
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if let Some(family) = actions.tool.preview_text_font_family.take() {
            self.preview_text_font_family(family);
        }
        if let Some(family) = actions.tool.set_text_font_family.take() {
            self.commit_text_font_family(family);
        }
        if actions.tool.cancel_text_font_family_preview {
            self.cancel_text_font_family_preview();
        }
        if let Some(color) = actions.tool.set_text_color.take() {
            if self.edit.text_edit.is_some() {
                self.apply_text_style_to_selection(Some(color), None, None, None, None, None);
            }
            self.edit.text_color = color;
            self.edit.fg_color = color;
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if let Some(v) = actions.tool.set_text_bold.take() {
            if self.edit.text_edit.is_some() {
                self.apply_text_style_to_selection(None, None, None, Some(v), None, None);
            }
            self.edit.text_bold = v;
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if let Some(v) = actions.tool.set_text_italic.take() {
            if self.edit.text_edit.is_some() {
                self.apply_text_style_to_selection(None, None, None, None, Some(v), None);
            }
            self.edit.text_italic = v;
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if let Some(v) = actions.tool.set_text_underline.take() {
            if self.edit.text_edit.is_some() {
                self.apply_text_style_to_selection(None, None, None, None, None, Some(v));
            }
            self.edit.text_underline = v;
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if let Some(v) = actions.tool.set_text_line_height.take() {
            self.edit.text_line_height = v.clamp(0.5, 4.0);
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if let Some(v) = actions.tool.set_text_tracking_px.take() {
            self.edit.text_tracking_px = v.clamp(-200.0, 500.0);
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if let Some(v) = actions.tool.set_text_opacity.take() {
            self.edit.text_opacity = v.clamp(0.0, 1.0);
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if let Some(origin) = actions.tool.text_move_origin.take() {
            if let Some(session) = &mut self.edit.text_edit {
                session.origin = origin;
                self.edit.text_focus_pending = false;
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
        }
        if let Some(a) = actions.tool.set_text_align.take() {
            self.edit.text_align = a;
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if let Some((cx, cy)) = actions.tool.text_canvas_click.take() {
            self.text_tool_click(cx, cy);
        }
        if actions.tool.text_commit {
            if !(self.shell.ui.show_paint_color_dialog
                && self.shell.ui.paint_color_dialog_target == 2)
            {
                self.commit_text_edit();
            }
        }
        if actions.tool.text_cancel {
            self.cancel_text_edit();
        }
        if let Some(idx) = actions.tool.edit_text_layer.take() {
            self.begin_edit_text_layer(idx);
        }
        if actions.tool.consume_text_focus {
            self.edit.text_focus_pending = false;
        }
        self.edit.text_drag_hovered = actions.tool.text_drag_handle_hovered;
        self.edit.text_panel_hovered = actions.tool.text_panel_hovered;
    }

    pub(super) fn handle_transform_crop_actions(
        &mut self,
        actions: &mut UiActions,
        event_loop: &ActiveEventLoop,
    ) {
        if actions.tool.transform_cancel {
            self.cancel_transform();
            self.sync_cursor(event_loop);
        }
        if actions.tool.transform_commit {
            self.commit_transform();
            self.sync_cursor(event_loop);
        }
        if actions.tool.start_transform {
            if self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .is_cmyk()
            {
                self.shell.status_msg = "Free Transform is not available in CMYK mode".to_string();
            } else {
                self.begin_transform();
                self.sync_cursor(event_loop);
            }
        }
        if let Some(v) = actions.tool.set_transform_scale_x.take() {
            self.transform_set_scale_x(v);
        }
        if let Some(v) = actions.tool.set_transform_scale_y.take() {
            self.transform_set_scale_y(v);
        }
        if let Some(v) = actions.tool.set_transform_angle.take() {
            self.transform_set_angle(v);
        }
        if let Some(v) = actions.tool.set_transform_tx.take() {
            self.transform_set_translate_x(v);
        }
        if let Some(v) = actions.tool.set_transform_ty.take() {
            self.transform_set_translate_y(v);
        }
        let ctx_menu_consumed = actions.tool.transform_flip_h
            || actions.tool.transform_flip_v
            || actions.tool.transform_rot_90cw
            || actions.tool.transform_rot_90ccw
            || actions.tool.transform_rot_180
            || actions.tool.transform_reset
            || actions.tool.set_transform_mode.is_some()
            || actions.tool.transform_warp
            || actions.tool.transform_ctx_menu_close
            || actions.tool.transform_commit
            || actions.tool.transform_cancel;
        if actions.tool.transform_flip_h {
            self.transform_flip_horizontal();
        }
        if actions.tool.transform_flip_v {
            self.transform_flip_vertical();
        }
        if actions.tool.transform_rot_90cw {
            self.transform_rotate_90cw();
        }
        if actions.tool.transform_rot_90ccw {
            self.transform_rotate_90ccw();
        }
        if actions.tool.transform_rot_180 {
            self.transform_rotate_180();
        }
        if actions.tool.transform_reset {
            self.transform_reset();
        }
        if let Some(mode) = actions.tool.set_transform_mode.take() {
            self.transform_set_mode(mode);
        }
        if actions.tool.transform_warp {
            self.transform_start_warp();
        }
        if ctx_menu_consumed {
            self.edit.transform_ctx_menu_pos = None;
        }

        let sel_menu_consumed = actions.sel.selection_ctx_menu_close
            || actions.sel.select_all
            || actions.sel.deselect
            || actions.sel.invert_selection
            || actions.sel.show_feather_dialog.is_some()
            || actions.sel.selection_grow.is_some()
            || actions.sel.selection_shrink.is_some()
            || actions.sel.selection_smooth.is_some()
            || actions.sel.selection_border.is_some()
            || actions.sel.open_modify_dialog.is_some()
            || actions.sel.close_modify_dialog
            || actions.doc.fill_foreground
            || actions.sel.show_stroke_dialog.is_some()
            || actions.dialogs.smart_fill_fill
            || actions.layers.layer_via_copy
            || actions.doc.copy
            || actions.doc.cut
            || actions.doc.paste
            || actions.tool.start_transform;
        if sel_menu_consumed {
            self.edit.selection_ctx_menu_pos = None;
        }

        if actions.tool.crop_cancel {
            self.edit.tools.active_on_cancel();
            self.update_crop_preview();
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if actions.tool.crop_confirm {
            if self.edit.tools.active_id() == crate::tools::ToolId::PerspectiveCrop {
                self.commit_active_perspective_crop();
            } else {
                self.commit_active_crop();
            }
        }
        if let Some(w) = actions.tool.set_persp_out_w.take() {
            let cw = self.docs.documents[self.docs.active_doc_idx].canvas.width as f32;
            let ch = self.docs.documents[self.docs.active_doc_idx].canvas.height as f32;
            let t = self.edit.tools.perspective_crop_mut();
            t.set_manual_width(w, cw, ch);
        }
        if let Some(h) = actions.tool.set_persp_out_h.take() {
            let cw = self.docs.documents[self.docs.active_doc_idx].canvas.width as f32;
            let ch = self.docs.documents[self.docs.active_doc_idx].canvas.height as f32;
            let t = self.edit.tools.perspective_crop_mut();
            t.set_manual_height(h, cw, ch);
        }
        if actions.tool.clear_persp_size {
            self.edit.tools.perspective_crop_mut().clear_manual_size();
        }
        if let Some(u) = actions.tool.set_persp_unit.take() {
            let cw = self.docs.documents[self.docs.active_doc_idx].canvas.width as f32;
            let ch = self.docs.documents[self.docs.active_doc_idx].canvas.height as f32;
            let t = self.edit.tools.perspective_crop_mut();
            t.unit = u;
            t.sync_manual_pixels(cw, ch);
        }
        if let Some(d) = actions.tool.set_persp_dpi.take() {
            let cw = self.docs.documents[self.docs.active_doc_idx].canvas.width as f32;
            let ch = self.docs.documents[self.docs.active_doc_idx].canvas.height as f32;
            let t = self.edit.tools.perspective_crop_mut();
            t.dpi = d.clamp(1.0, 10000.0);
            t.sync_manual_pixels(cw, ch);
        }
    }

    /// Current colour a paint-colour dialog `target` edits (used to seed the
    /// dialog and as the Cancel baseline). Mirrors [`Self::set_paint_color`].
    fn paint_dialog_current_color(&self, target: u8) -> [u8; 4] {
        let path_style = self.active_path_style_vm();
        match target {
            1 => self.edit.bg_color,
            2 => self.edit.text_color,
            3 => self.edit.tools.shape().fill_color,
            4 => self.edit.tools.shape().stroke_color,
            5 => path_style.map_or([0, 0, 0, 255], |s| s.fill_color),
            6 => path_style.map_or([0, 0, 0, 255], |s| s.stroke_color),
            7 => path_style.map_or([255, 255, 255, 255], |s| s.fill_end_color),
            8..=15 => path_style.map_or([0, 0, 0, 255], |s| {
                s.gradient_stop_colors[(target - 8) as usize]
            }),
            _ => self.edit.tools.brush().settings.color,
        }
    }

    /// Open the paint-colour dialog for `target`, showing `color` in the picker
    /// while `original` is the value Cancel restores.
    fn open_paint_color_dialog_seeded(&mut self, target: u8, color: [u8; 4], original: [u8; 4]) {
        self.shell.ui.show_paint_color_dialog = true;
        self.shell.ui.paint_color_dialog_target = target;
        self.shell.ui.paint_color_dialog_color = color;
        self.shell.ui.paint_color_dialog_original = original;
        self.shell.ui.paint_color_dialog_center_next = true;
    }

    pub(super) fn handle_tool_color_actions(
        &mut self,
        actions: &mut UiActions,
        event_loop: &ActiveEventLoop,
    ) {
        if let Some(id) = actions.tool.select_tool.take() {
            if !self.is_tool_modal_active() {
                self.edit.tools.select(id);
                if id == crate::tools::ToolId::Text {
                    self.shell.ui.show_text_panel = true;
                }
                if id == crate::tools::ToolId::Crop {
                    let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
                    let dpi = canvas.metadata.resolution_ppi;
                    self.edit.tools.crop_mut().sync_dpi_on_activate(dpi);
                }
                if id == crate::tools::ToolId::PerspectiveCrop {
                    // No auto frame — wait for the user to pick the four corners.
                    let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
                    let dpi = canvas.metadata.resolution_ppi;
                    let cw = canvas.width as f32;
                    let ch = canvas.height as f32;
                    let t = self.edit.tools.perspective_crop_mut();
                    // Only adopt the image's DPI when no manual output size is set.
                    // Once the user has typed a size, freeze DPI so a cm/inch value
                    // doesn't jump when re-entering the tool on a different-DPI image.
                    if t.manual_size.is_none() {
                        t.dpi = dpi;
                    }
                    t.sync_manual_pixels(cw, ch);
                    t.begin_placing();
                }
                self.sync_cursor(event_loop);
            }
        }
        if let Some(s) = actions.tool.brush_size.take() {
            if self.edit.tools.active_id() == crate::tools::ToolId::Eraser {
                self.edit.tools.eraser_mut().size = s.clamp(1.0, 5000.0);
            } else {
                self.edit.tools.brush_mut().settings.size = s.clamp(1.0, 5000.0);
            }
            self.win.last_cursor_radius = 0;
            self.sync_cursor(event_loop);
        }
        if let Some(h) = actions.tool.brush_hardness.take() {
            let h = h.clamp(0.0, 1.0);
            if self.edit.tools.active_id() == crate::tools::ToolId::Eraser {
                self.edit.tools.eraser_mut().hardness = h;
            } else {
                let b = self.edit.tools.brush_mut();
                b.settings.hardness = h;
                b.preset_idx = crate::tools::brush::PRESET_CUSTOM;
            }
        }
        if let Some(o) = actions.tool.brush_opacity.take() {
            let o = o.clamp(0.0, 1.0);
            if self.edit.tools.active_id() == crate::tools::ToolId::Eraser {
                self.edit.tools.eraser_mut().opacity = o;
            } else {
                self.edit.tools.brush_mut().settings.opacity = o;
            }
        }
        if let Some(s) = actions.tool.brush_spacing.take() {
            if self.edit.tools.active_id() == crate::tools::ToolId::Eraser {
                let e = self.edit.tools.eraser_mut();
                e.spacing = s;
                e.preset_idx = crate::tools::brush::PRESET_CUSTOM;
            } else {
                let b = self.edit.tools.brush_mut();
                b.settings.spacing = s;
                b.preset_idx = crate::tools::brush::PRESET_CUSTOM;
            }
        }
        if let Some(s) = actions.tool.brush_smoothing.take() {
            if self.edit.tools.active_id() == crate::tools::ToolId::Eraser {
                self.edit.tools.eraser_mut().smoothing = s.clamp(0.0, 1.0);
            } else {
                self.edit.tools.brush_mut().settings.smoothing = s.clamp(0.0, 1.0);
            }
        }
        if let Some(f) = actions.tool.brush_flow.take() {
            if self.edit.tools.active_id() == crate::tools::ToolId::Eraser {
                let e = self.edit.tools.eraser_mut();
                e.flow = f.clamp(0.0, 1.0);
                e.preset_idx = crate::tools::brush::PRESET_CUSTOM;
            } else {
                let b = self.edit.tools.brush_mut();
                b.settings.flow = f.clamp(0.0, 1.0);
                b.preset_idx = crate::tools::brush::PRESET_CUSTOM;
            }
        }
        if let Some(idx) = actions.tool.select_brush_preset.take() {
            if self.edit.tools.active_id() == crate::tools::ToolId::Eraser {
                self.edit.tools.eraser_mut().apply_preset(idx);
            } else {
                self.edit.tools.brush_mut().apply_preset(idx);
            }
        }
        if actions.tool.brush_popup_close {
            self.edit.brush_popup_pos = None;
        }
        if let Some(c) = actions.tool.brush_color.take() {
            self.set_paint_color(0, c);
        }
        if let Some(c) = actions.tool.bg_color.take() {
            self.set_paint_color(1, c);
        }
        if let Some(target) = actions.dialogs.open_paint_color_dialog.take() {
            let target = match target {
                1..=15 => target,
                _ => 0,
            };
            let color = self.paint_dialog_current_color(target);
            self.open_paint_color_dialog_seeded(target, color, color);
        }
        // Palette right-click "Adjust colour…": seed the picker with the chosen
        // swatch but keep the target's real colour as the Cancel baseline.
        if let Some((target, seed)) = actions.dialogs.open_paint_color_dialog_with.take() {
            let target = match target {
                1..=15 => target,
                _ => 0,
            };
            let original = self.paint_dialog_current_color(target);
            self.open_paint_color_dialog_seeded(target, seed, original);
        }
        if actions.dialogs.paint_color_dialog_centered {
            self.shell.ui.paint_color_dialog_center_next = false;
        }
        if let Some(live_preview) = actions.dialogs.set_paint_color_dialog_live_preview.take() {
            self.shell.ui.paint_color_dialog_live_preview = live_preview;
            let color = if live_preview {
                self.shell.ui.paint_color_dialog_color
            } else {
                self.shell.ui.paint_color_dialog_original
            };
            self.set_paint_color(self.shell.ui.paint_color_dialog_target, color);
        }
        if let Some(color) = actions.dialogs.set_paint_color_dialog_color.take() {
            self.shell.ui.paint_color_dialog_color = color;
            if self.shell.ui.paint_color_dialog_live_preview {
                self.set_paint_color(self.shell.ui.paint_color_dialog_target, color);
            }
        }
        if actions.dialogs.paint_color_dialog_default {
            let color = if self.shell.ui.paint_color_dialog_target == 1 {
                [255, 255, 255, 255]
            } else {
                [0, 0, 0, 255]
            };
            self.shell.ui.paint_color_dialog_color = color;
            if self.shell.ui.paint_color_dialog_live_preview {
                self.set_paint_color(self.shell.ui.paint_color_dialog_target, color);
            }
        }
        if actions.dialogs.paint_color_dialog_ok {
            let target = self.shell.ui.paint_color_dialog_target;
            let color = self.shell.ui.paint_color_dialog_color;
            self.shell.ui.show_paint_color_dialog = false;
            self.shell.ui.paint_color_dialog_center_next = false;
            self.set_paint_color(target, color);
            // A Path colour was previewed live; commit the single undo step.
            if (5..=15).contains(&target) {
                self.path_style_commit();
            }
        }
        if actions.dialogs.paint_color_dialog_cancel {
            let target = self.shell.ui.paint_color_dialog_target;
            let color = self.shell.ui.paint_color_dialog_original;
            self.shell.ui.show_paint_color_dialog = false;
            self.shell.ui.paint_color_dialog_color = color;
            self.shell.ui.paint_color_dialog_center_next = false;
            self.set_paint_color(target, color);
            // Restore the baseline and record nothing (final == baseline).
            if (5..=15).contains(&target) {
                self.path_style_commit();
            }
        }
    }

    pub(super) fn handle_tool_options_actions(
        &mut self,
        actions: &mut UiActions,
        event_loop: &ActiveEventLoop,
    ) {
        if let Some((axis, reference)) = actions.tool.node_align.take() {
            self.node_align(axis, reference);
        }
        if std::mem::take(&mut actions.tool.node_break) {
            self.node_break_at_selected();
        }
        if std::mem::take(&mut actions.tool.node_join) {
            self.node_join_selected();
        }
        if let Some(m) = actions.tool.set_pen_mode.take() {
            self.edit.tools.pen_mut().mode = crate::tools::pen::PenMode::from_u8(m);
        }
        if let Some(w) = actions.tool.set_pen_stroke_width.take() {
            self.edit.tools.pen_mut().stroke_width = w.clamp(1.0, 1000.0);
        }
        if let Some(w) = actions.tool.set_vector_brush_width.take() {
            self.edit.tools.vector_brush_mut().width = w.clamp(0.5, 1000.0);
        }
        if let Some(s) = actions.tool.set_vector_brush_smoothing.take() {
            self.edit.tools.vector_brush_mut().smoothing = s.clamp(0.0, 0.95);
        }
        if let Some(p) = actions.tool.set_vector_brush_pressure.take() {
            self.edit.tools.vector_brush_mut().pressure = p;
        }
        if let Some(v) = actions.tool.set_vector_brush_velocity.take() {
            self.edit.tools.vector_brush_mut().velocity = v;
        }
        if std::mem::take(&mut actions.tool.expand_vector_brush) {
            self.expand_active_brush_stroke();
        }
        if let Some(k) = actions.tool.set_shape_kind.take() {
            self.edit.tools.shape_mut().kind = crate::tools::shape::ShapeKind::from_u8(k);
        }
        if let Some(f) = actions.tool.set_shape_fill.take() {
            self.edit.tools.shape_mut().fill = f;
            self.update_selected_shape_style(true, false);
        }
        if let Some(w) = actions.tool.set_shape_stroke_width.take() {
            self.edit.tools.shape_mut().stroke_width = w.clamp(0.0, 1000.0);
            self.update_selected_shape_style(false, true);
        }
        if let Some(r) = actions.tool.set_shape_corner_radius.take() {
            self.edit.tools.shape_mut().corner_radius = r.clamp(0.0, 5000.0);
            self.update_selected_shape_style(false, false);
        }
        if let Some(n) = actions.tool.set_shape_sides.take() {
            self.edit.tools.shape_mut().sides = n.clamp(3, 100);
            self.update_selected_shape_style(false, false);
        }
        if let Some(f) = actions.tool.set_shape_star_inner.take() {
            self.edit.tools.shape_mut().star_inner = f.clamp(0.05, 0.95);
            self.update_selected_shape_style(false, false);
        }
        // Path layer Fill/Outline (Move / Node options bar).
        if let Some(on) = actions.tool.set_path_fill_enabled.take() {
            self.path_set_fill_enabled(on);
        }
        if let Some(on) = actions.tool.set_path_stroke_enabled.take() {
            self.path_set_stroke_enabled(on);
        }
        if let Some(on) = actions.tool.set_path_fill_overprint.take() {
            self.path_set_fill_overprint(on);
            if on {
                self.shell.status_msg =
                    "Overprint Fill enabled; PDF preserves the artwork via raster fallback"
                        .to_string();
            }
        }
        if let Some(on) = actions.tool.set_path_stroke_overprint.take() {
            self.path_set_stroke_overprint(on);
            if on {
                self.shell.status_msg =
                    "Overprint Outline enabled; PDF preserves the artwork via raster fallback"
                        .to_string();
            }
        }
        if let Some(color) = actions.tool.apply_palette_fill.take() {
            if !self.path_apply_palette_fill(color) {
                let rgba = color.to_rgba8();
                self.set_paint_color(0, rgba);
                self.shell.status_msg = "No Path selected; colour set as Foreground".to_string();
            }
        }
        if let Some(color) = actions.tool.apply_palette_outline.take() {
            if !self.path_apply_palette_outline(color) {
                let rgba = color.to_rgba8();
                self.set_paint_color(1, rgba);
                self.shell.status_msg = "No Path selected; colour set as Background".to_string();
            }
        }
        if std::mem::take(&mut actions.tool.clear_palette_fill) && !self.path_clear_palette_fill() {
            self.shell.status_msg = "Select a Path to remove its Fill".to_string();
        }
        if std::mem::take(&mut actions.tool.clear_palette_outline)
            && !self.path_clear_palette_outline()
        {
            self.shell.status_msg = "Select a Path to remove its Outline".to_string();
        }
        if let Some(color) = actions.tool.add_document_swatch.take() {
            self.add_document_swatch(color);
        }
        if let Some((index, name)) = actions.tool.rename_document_swatch.take() {
            self.rename_document_swatch(index, name);
        }
        if let Some(index) = actions.tool.remove_document_swatch.take() {
            self.remove_document_swatch(index);
        }
        if let Some(w) = actions.tool.set_path_stroke_width.take() {
            self.path_set_stroke_width(w);
        }
        if let Some(kind) = actions.tool.set_path_fill_kind.take() {
            self.path_set_fill_kind(kind);
        }
        if let Some((index, offset)) = actions.tool.set_path_gradient_stop_offset.take() {
            self.path_set_gradient_stop_offset(index, offset);
        }
        if std::mem::take(&mut actions.tool.add_path_gradient_stop) {
            self.path_add_gradient_stop();
        }
        if let Some(index) = actions.tool.remove_path_gradient_stop.take() {
            self.path_remove_gradient_stop(index);
        }
        if let Some(kind) = actions.tool.set_path_dash_kind.take() {
            self.path_set_dash_kind(kind);
        }
        if let Some((values, len)) = actions.tool.set_path_dash_values.take() {
            self.path_set_dash_values(values, len);
        }
        if let Some(offset) = actions.tool.set_path_dash_offset.take() {
            self.path_set_dash_offset(offset);
        }
        if actions.tool.commit_path_style {
            self.path_style_commit();
        }
        if actions.tool.swap_colors {
            std::mem::swap(
                &mut self.edit.tools.brush_mut().settings.color,
                &mut self.edit.bg_color,
            );
            self.edit.fg_color = self.edit.tools.brush().settings.color;
            self.edit.tools.fill_mut().color = self.edit.tools.brush().settings.color;
            self.edit.tools.eraser_mut().bg_color = self.edit.bg_color;
        }
        if let Some(s) = actions.tool.set_clone_size.take() {
            self.edit.tools.clone_like_mut().size = s.clamp(1.0, 5000.0);
            self.win.last_cursor_radius = 0;
            self.sync_cursor(event_loop);
        }
        if let Some(h) = actions.tool.set_clone_hardness.take() {
            self.edit.tools.clone_like_mut().hardness = h.clamp(0.0, 1.0);
        }
        if let Some(o) = actions.tool.set_clone_opacity.take() {
            self.edit.tools.clone_like_mut().opacity = o.clamp(0.0, 1.0);
        }
        if let Some(s) = actions.tool.set_clone_spacing.take() {
            self.edit.tools.clone_like_mut().spacing = s.clamp(0.01, 2.0);
        }
        if let Some(i) = actions.tool.set_clone_preset.take() {
            if let Some(p) = crate::tools::brush::BRUSH_PRESETS.get(i) {
                let t = self.edit.tools.clone_like_mut();
                t.hardness = p.hardness;
                t.spacing = p.spacing;
            }
        }
        if let Some(v) = actions.tool.set_clone_aligned.take() {
            self.edit.tools.clone_like_mut().aligned = v;
        }
        if let Some(v) = actions.tool.set_clone_sample_merged.take() {
            self.edit.tools.clone_like_mut().sample_merged = v;
        }
        if let Some(v) = actions.tool.set_clone_smart_fill.take() {
            self.edit.tools.clone_like_mut().smart_fill = v;
        }

        if let Some(s) = actions.tool.set_smudge_size.take() {
            self.edit.tools.smudge_mut().size = s.clamp(1.0, 5000.0);
            self.win.last_cursor_radius = 0;
            self.sync_cursor(event_loop);
        }
        if let Some(h) = actions.tool.set_smudge_hardness.take() {
            self.edit.tools.smudge_mut().hardness = h.clamp(0.0, 1.0);
        }
        if let Some(v) = actions.tool.set_smudge_strength.take() {
            self.edit.tools.smudge_mut().strength = v.clamp(0.0, 1.0);
        }
        if let Some(v) = actions.tool.set_smudge_finger_painting.take() {
            self.edit.tools.smudge_mut().finger_painting = v;
        }
        if let Some(i) = actions.tool.set_smudge_preset.take() {
            if let Some(p) = crate::tools::brush::BRUSH_PRESETS.get(i) {
                let t = self.edit.tools.smudge_mut();
                t.hardness = p.hardness;
                t.spacing = p.spacing;
            }
        }
        if let Some(s) = actions.tool.set_dodge_size.take() {
            self.edit.tools.dodge_burn_mut().size = s.clamp(1.0, 5000.0);
            self.win.last_cursor_radius = 0;
            self.sync_cursor(event_loop);
        }
        if let Some(h) = actions.tool.set_dodge_hardness.take() {
            self.edit.tools.dodge_burn_mut().hardness = h.clamp(0.0, 1.0);
        }
        if let Some(v) = actions.tool.set_dodge_exposure.take() {
            self.edit.tools.dodge_burn_mut().exposure = v.clamp(0.0, 1.0);
        }
        if let Some(v) = actions.tool.set_dodge_range.take() {
            self.edit.tools.dodge_burn_mut().range =
                crate::tools::dodge_burn::ToneRange::from_u8(v);
        }
        if let Some(v) = actions.tool.set_dodge_protect_tones.take() {
            self.edit.tools.dodge_burn_mut().protect_tones = v;
        }
        if let Some(i) = actions.tool.set_dodge_preset.take() {
            if let Some(p) = crate::tools::brush::BRUSH_PRESETS.get(i) {
                let t = self.edit.tools.dodge_burn_mut();
                t.hardness = p.hardness;
                t.spacing = p.spacing;
            }
        }
        if let Some(v) = actions.tool.set_patch_mode.take() {
            self.edit.tools.patch_mut().mode = crate::tools::patch::PatchMode::from_u8(v);
        }

        if let Some(m) = actions.tool.set_crop_mode.take() {
            let new_mode = match m {
                0 => crate::tools::crop::CropMode::Free,
                1 => crate::tools::crop::CropMode::Ratio,
                _ => crate::tools::crop::CropMode::FixedSize,
            };
            let had_selection = self.edit.tools.crop().has_selection();
            self.edit.tools.crop_mut().mode = new_mode;
            if had_selection && new_mode == crate::tools::crop::CropMode::FixedSize {
                let cw = self.docs.documents[self.docs.active_doc_idx].canvas.width;
                let ch = self.docs.documents[self.docs.active_doc_idx].canvas.height;
                let c = self.edit.tools.crop_mut();
                c.init_bounds(cw, ch);
                if let Some(win) = &self.win.window {
                    win.request_redraw();
                }
            }
        }
        if let Some(w) = actions.tool.set_crop_ratio_w.take() {
            self.edit.tools.crop_mut().ratio_w = w;
        }
        if let Some(h) = actions.tool.set_crop_ratio_h.take() {
            self.edit.tools.crop_mut().ratio_h = h;
        }
        if let Some(u) = actions.tool.set_crop_unit.take() {
            let cw = self.docs.documents[self.docs.active_doc_idx].canvas.width as f32;
            let ch = self.docs.documents[self.docs.active_doc_idx].canvas.height as f32;
            let new_unit = match u {
                0 => crate::core::units::Unit::Pixels,
                1 => crate::core::units::Unit::Centimeters,
                2 => crate::core::units::Unit::Millimeters,
                3 => crate::core::units::Unit::Inches,
                4 => crate::core::units::Unit::Points,
                5 => crate::core::units::Unit::Picas,
                _ => crate::core::units::Unit::Percent,
            };
            self.edit
                .tools
                .crop_mut()
                .change_display_unit(new_unit, cw, ch);
            if let Some(win) = &self.win.window {
                win.request_redraw();
            }
        }
        if let Some(d) = actions.tool.set_crop_dpi.take() {
            let cw = self.docs.documents[self.docs.active_doc_idx].canvas.width as f32;
            let ch = self.docs.documents[self.docs.active_doc_idx].canvas.height as f32;
            let c = self.edit.tools.crop_mut();
            let had_selection = c.has_selection();
            let displayed =
                if c.mode != crate::tools::crop::CropMode::FixedSize && c.has_selection() {
                    Some((
                        crate::core::units::from_pixels(
                            (c.crop_x1 - c.crop_x0).abs(),
                            c.unit,
                            c.dpi,
                            cw,
                        ),
                        crate::core::units::from_pixels(
                            (c.crop_y1 - c.crop_y0).abs(),
                            c.unit,
                            c.dpi,
                            ch,
                        ),
                    ))
                } else {
                    None
                };
            c.set_user_dpi(d);
            if had_selection && c.mode == crate::tools::crop::CropMode::FixedSize {
                c.init_bounds(cw as u32, ch as u32);
            } else if let Some((w, h)) = displayed {
                let w_px = crate::core::units::to_pixels(w, c.unit, c.dpi, cw).max(1.0);
                let h_px = crate::core::units::to_pixels(h, c.unit, c.dpi, ch).max(1.0);
                let x0 = c.crop_x0.min(c.crop_x1);
                let y0 = c.crop_y0.min(c.crop_y1);
                c.crop_x0 = x0;
                c.crop_y0 = y0;
                c.crop_x1 = x0 + w_px;
                c.crop_y1 = y0 + h_px;
            }
            if let Some(win) = &self.win.window {
                win.request_redraw();
            }
        }
        if let Some(value) = actions.tool.set_crop_w_value.take() {
            let cw = self.docs.documents[self.docs.active_doc_idx].canvas.width as f32;
            let ch = self.docs.documents[self.docs.active_doc_idx].canvas.height as f32;
            let c = self.edit.tools.crop_mut();
            let had_selection = c.has_selection();
            c.fixed_w = value.max(0.0);
            if had_selection && c.mode == crate::tools::crop::CropMode::FixedSize {
                c.init_bounds(cw as u32, ch as u32);
            } else if had_selection {
                let w_px = crate::core::units::to_pixels(value, c.unit, c.dpi, cw).max(1.0);
                let x0 = c.crop_x0.min(c.crop_x1);
                c.crop_x0 = x0;
                c.crop_x1 = x0 + w_px;
            }
            if let Some(win) = &self.win.window {
                win.request_redraw();
            }
        }
        if let Some(value) = actions.tool.set_crop_h_value.take() {
            let cw = self.docs.documents[self.docs.active_doc_idx].canvas.width as f32;
            let ch = self.docs.documents[self.docs.active_doc_idx].canvas.height as f32;
            let c = self.edit.tools.crop_mut();
            let had_selection = c.has_selection();
            c.fixed_h = value.max(0.0);
            if had_selection && c.mode == crate::tools::crop::CropMode::FixedSize {
                c.init_bounds(cw as u32, ch as u32);
            } else if had_selection {
                let h_px = crate::core::units::to_pixels(value, c.unit, c.dpi, ch).max(1.0);
                let y0 = c.crop_y0.min(c.crop_y1);
                c.crop_y0 = y0;
                c.crop_y1 = y0 + h_px;
            }
            if let Some(win) = &self.win.window {
                win.request_redraw();
            }
        }
        if let Some(w_px) = actions.tool.set_crop_w_px.take() {
            let cw = self.docs.documents[self.docs.active_doc_idx].canvas.width as f32;
            let ch = self.docs.documents[self.docs.active_doc_idx].canvas.height as f32;
            let c = self.edit.tools.crop_mut();
            let had_selection = c.has_selection();
            c.fixed_w = crate::core::units::from_pixels(w_px.max(1.0), c.unit, c.dpi, cw);
            if had_selection && c.mode == crate::tools::crop::CropMode::FixedSize {
                c.init_bounds(cw as u32, ch as u32);
            } else if had_selection {
                let x0 = c.crop_x0.min(c.crop_x1);
                c.crop_x0 = x0;
                c.crop_x1 = x0 + w_px.max(1.0);
            }
            if let Some(win) = &self.win.window {
                win.request_redraw();
            }
        }
        if let Some(h_px) = actions.tool.set_crop_h_px.take() {
            let cw = self.docs.documents[self.docs.active_doc_idx].canvas.width as f32;
            let ch = self.docs.documents[self.docs.active_doc_idx].canvas.height as f32;
            let c = self.edit.tools.crop_mut();
            let had_selection = c.has_selection();
            c.fixed_h = crate::core::units::from_pixels(h_px.max(1.0), c.unit, c.dpi, ch);
            if had_selection && c.mode == crate::tools::crop::CropMode::FixedSize {
                c.init_bounds(cw as u32, ch as u32);
            } else if had_selection {
                let y0 = c.crop_y0.min(c.crop_y1);
                c.crop_y0 = y0;
                c.crop_y1 = y0 + h_px.max(1.0);
            }
            if let Some(win) = &self.win.window {
                win.request_redraw();
            }
        }
        if let Some(o) = actions.tool.set_crop_overlay.take() {
            use crate::tools::crop::CropOverlay;
            self.edit.tools.crop_mut().overlay = match o {
                0 => CropOverlay::None,
                1 => CropOverlay::RuleOfThirds,
                2 => CropOverlay::Grid,
                3 => CropOverlay::GoldenRatio,
                _ => CropOverlay::None,
            };
        }
        if actions.tool.swap_crop_wh {
            let c = self.edit.tools.crop_mut();
            std::mem::swap(&mut c.fixed_w, &mut c.fixed_h);
            std::mem::swap(&mut c.ratio_w, &mut c.ratio_h);
            if c.has_selection() {
                let bcx = (c.crop_x0 + c.crop_x1) * 0.5;
                let bcy = (c.crop_y0 + c.crop_y1) * 0.5;
                let hw = (c.crop_x1 - c.crop_x0).abs() * 0.5;
                let hh = (c.crop_y1 - c.crop_y0).abs() * 0.5;
                c.crop_x0 = bcx - hh;
                c.crop_x1 = bcx + hh;
                c.crop_y0 = bcy - hw;
                c.crop_y1 = bcy + hw;
            }
            if let Some(win) = &self.win.window {
                win.request_redraw();
            }
        }
        if let Some(s) = actions.tool.set_wand_brush_size.take() {
            self.edit.tools.wand_mut().brush_size = s.clamp(1.0, 2000.0);
            self.win.last_cursor_radius = 0;
            self.sync_cursor(event_loop);
        }
        if let Some(t) = actions.tool.set_wand_tolerance.take() {
            self.edit.tools.wand_mut().tolerance = t;
        }
        if let Some(e) = actions.tool.set_wand_edge_sensitivity.take() {
            self.edit.tools.wand_mut().edge_sensitivity = e;
        }
        if let Some(c) = actions.tool.set_wand_contiguous.take() {
            self.edit.tools.wand_mut().contiguous = c;
        }
        if let Some(a) = actions.tool.set_wand_anti_alias.take() {
            self.edit.tools.wand_mut().anti_alias = a;
        }
        if let Some(m) = actions.tool.set_wand_sample_merged.take() {
            self.edit.tools.wand_mut().sample_merged = m;
        }
        if let Some(model) = actions.sel.set_select_subject_model.take() {
            if self.jobs.select_subject.set_selected_model(model) {
                self.shell.status_msg = self.jobs.select_subject.status_text();
            } else {
                self.shell.status_msg =
                    "Cannot switch Select Subject model while it is busy".to_string();
            }
        }
        if let Some(t) = actions.tool.set_fill_tolerance.take() {
            self.edit.tools.fill_mut().tolerance = t;
        }
        if let Some(c) = actions.tool.set_fill_contiguous.take() {
            self.edit.tools.fill_mut().contiguous = c;
        }
        if let Some(a) = actions.tool.set_fill_anti_alias.take() {
            self.edit.tools.fill_mut().anti_alias = a;
        }
        if let Some(m) = actions.tool.set_fill_all_layers.take() {
            self.edit.tools.fill_mut().sample_merged = m;
        }

        if let Some(t) = actions.tool.set_gradient_type.take() {
            let handled_by_vector =
                self.active_gradient_mode() == 2 && self.vector_gradient_set_kind(t);
            if !handled_by_vector {
                self.edit.tools.gradient_mut().gradient_type = match t {
                    0 => crate::tools::gradient::GradientType::Linear,
                    1 => crate::tools::gradient::GradientType::Radial,
                    2 => crate::tools::gradient::GradientType::Angle,
                    3 => crate::tools::gradient::GradientType::Reflected,
                    _ => crate::tools::gradient::GradientType::Diamond,
                };
            }
        }
        if let Some(o) = actions.tool.set_gradient_opacity.take() {
            self.edit.tools.gradient_mut().opacity = o;
        }
        if let Some(r) = actions.tool.set_gradient_reverse.take() {
            if self.active_gradient_mode() == 2 {
                if r {
                    self.vector_gradient_reverse();
                }
            } else {
                self.edit.tools.gradient_mut().reverse = r;
            }
        }
        if let Some(d) = actions.tool.set_gradient_dither.take() {
            self.edit.tools.gradient_mut().dither = d;
        }
        if let Some(stops) = actions.tool.set_gradient_stops.take() {
            if self.active_gradient_mode() != 2 || !self.vector_gradient_set_stops(stops.clone()) {
                let g = &mut self.edit.tools.gradient_mut().gradient;
                g.name = "Custom".to_string();
                g.stops = stops
                    .into_iter()
                    .map(|(position, color)| crate::tools::gradient::GradientStop {
                        position,
                        color,
                    })
                    .collect();
            }
        }
        if let Some(v) = actions.tool.toggle_gradient_editor.take() {
            self.shell.ui.show_gradient_editor = v;
            if !v && self.active_gradient_mode() == 2 {
                self.path_style_commit();
            }
        }

        if let Some(s) = actions.tool.set_eyedropper_sample.take() {
            self.edit.tools.eyedropper_mut().sample_size = match s {
                0 => crate::tools::eyedropper::SampleSize::Point,
                1 => crate::tools::eyedropper::SampleSize::Average3x3,
                2 => crate::tools::eyedropper::SampleSize::Average5x5,
                _ => crate::tools::eyedropper::SampleSize::Average11x11,
            };
        }
        if let Some(m) = actions.tool.set_eyedropper_sample_merged.take() {
            self.edit.tools.eyedropper_mut().sample_merged = m;
        }

        if let Some(a) = actions.tool.set_move_auto_select.take() {
            self.edit.tools.move_tool_mut().auto_select = a;
        }
        if let Some(s) = actions.tool.set_move_show_transform.take() {
            self.edit.tools.move_tool_mut().show_transform = s;
        }
        if let Some(align) = actions.layers.align_layers.take() {
            self.align_selected_layers_to_canvas(align);
        }
        if let Some(distribute) = actions.layers.distribute_layers.take() {
            self.distribute_selected_layers(distribute);
        }
        if actions.layers.duplicate_selected_step {
            self.duplicate_selected_with_step((10, 10));
        }
        if actions.layers.repeat_duplicate_step {
            let delta = self.edit.tools.move_tool().last_duplicate_delta;
            if let Some(delta) = delta {
                self.duplicate_selected_with_step(delta);
            } else {
                self.shell.status_msg = "No duplicate step to repeat".to_string();
            }
        }
        if let Some(action) = actions.tool.move_transform.take() {
            self.apply_move_transform_action(action);
            self.sync_cursor(event_loop);
        }
    }
}

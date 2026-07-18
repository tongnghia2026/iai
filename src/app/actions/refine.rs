//! Refine Selection panel: overlay texture, live preview and the
//! commit/output modes. Split out of app/actions.rs.

use crate::app::render::CanvasEvent;
use crate::app::state::App;

impl App {
    /// Rebuild `refine_overlay_tex` when the mask changes (called every frame in Overlay mode).
    ///
    /// The texture encodes the selection mask as RGBA: selected pixels are transparent,
    /// unselected pixels are a red/pink tint — matching the standard Overlay view mode.
    /// Texture is capped at 2048×2048 so it is fast even on large canvases.
    pub fn update_refine_overlay_tex(&mut self) {
        if !self.edit.show_refine_panel {
            return;
        }
        if self.edit.refine_view_mode != crate::ui::refine_select::RefineViewMode::Overlay {
            return;
        }

        let mask_rev = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .selection
            .mask_revision;
        if self.edit.refine_overlay_mask_rev == mask_rev && self.edit.refine_overlay_tex.is_some() {
            return;
        }

        let canvas_w = self.docs.documents[self.docs.active_doc_idx].canvas.width as usize;
        let canvas_h = self.docs.documents[self.docs.active_doc_idx].canvas.height as usize;
        if canvas_w == 0 || canvas_h == 0 {
            return;
        }

        let mask = &self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .selection
            .mask;

        const MAX_TEX: usize = 2048;
        let tex_w = canvas_w.min(MAX_TEX);
        let tex_h = canvas_h.min(MAX_TEX);
        let scale_x = canvas_w as f32 / tex_w as f32;
        let scale_y = canvas_h as f32 / tex_h as f32;

        let mut pixels = vec![egui::Color32::TRANSPARENT; tex_w * tex_h];
        let [r, g, b, tint_alpha] = self.edit.refine_overlay_color;

        for ty in 0..tex_h {
            let cy = ((ty as f32 * scale_y) as usize).min(canvas_h - 1);
            let row_base = cy * canvas_w;
            let out_base = ty * tex_w;
            for tx in 0..tex_w {
                let cx = ((tx as f32 * scale_x) as usize).min(canvas_w - 1);
                let mask_val = mask[row_base + cx];
                let alpha = (255u32 - mask_val as u32) * tint_alpha as u32 / 255;
                if alpha > 0 {
                    pixels[out_base + tx] =
                        egui::Color32::from_rgba_unmultiplied(r, g, b, alpha as u8);
                }
            }
        }

        let image = egui::ColorImage {
            size: [tex_w, tex_h],
            pixels,
            source_size: egui::Vec2::new(tex_w as f32, tex_h as f32),
        };
        let tex =
            self.win
                .egui_ctx
                .load_texture("sam_overlay", image, egui::TextureOptions::LINEAR);
        self.edit.refine_overlay_tex = Some(tex);
        self.edit.refine_overlay_mask_rev = mask_rev;
    }

    /// Open the refine panel — snapshot current selection for cancel/preview.
    /// Automatically switches to the Refine Brush tool while panel is open.
    pub fn open_refine_panel(&mut self) {
        if !self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .selection
            .active
        {
            return;
        }
        self.edit.refine_snapshot = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .selection
            .mask
            .clone();
        self.edit.refine_feather = 0.0;
        self.edit.refine_smooth = 0;
        self.edit.refine_smart_radius = 0.0;
        self.edit.refine_shift_edge = 0.0;
        self.edit.refine_contrast = 0.0;
        self.edit.refine_decontaminate = false;
        self.edit.refine_decontaminate_amount = 0.5;
        self.edit.refine_dirty = false;
        self.edit.show_refine_panel = true;
        self.shell.ui.show_refine_color_dialog = false;
        self.shell.ui.refine_color_dialog_center_next = false;
        self.edit.refine_view_mode = crate::ui::refine_select::RefineViewMode::Overlay;
        self.edit.refine_overlay_tex = None;
        self.edit.refine_overlay_mask_rev = u64::MAX;
        self.edit.refine_output_mode = crate::ui::refine_select::RefineOutputMode::Selection;
        self.edit.refine_prev_tool = self.edit.tools.active_id();
        self.edit.tools.select(crate::tools::ToolId::RefineBrush);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Apply refine parameters to snapshot and update selection preview.
    pub fn apply_refine_preview(&mut self) {
        use crate::core::selection::{blur_mask, smart_radius_mask};

        let canvas_w = self.docs.documents[self.docs.active_doc_idx].canvas.width as usize;
        let canvas_h = self.docs.documents[self.docs.active_doc_idx].canvas.height as usize;

        let mut mask = self.edit.refine_snapshot.clone();

        if self.edit.refine_smart_radius > 0.5 {
            smart_radius_mask(&mut mask, canvas_w, canvas_h, self.edit.refine_smart_radius);
        }

        if self.edit.refine_shift_edge.abs() > 0.5 {
            let shift_px = ((self.edit.refine_shift_edge.abs() / 100.0) * 30.0) as u32;
            if shift_px > 0 {
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .selection
                    .mask = mask;
                if self.edit.refine_shift_edge > 0.0 {
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .selection
                        .grow(shift_px);
                } else {
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .selection
                        .shrink(shift_px);
                }
                mask = self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .selection
                    .mask
                    .clone();
            }
        }

        if self.edit.refine_smooth > 0 {
            let r = (self.edit.refine_smooth as f32 / 5.0).max(1.0);
            blur_mask(&mut mask, canvas_w, canvas_h, r);
        }

        if self.edit.refine_feather > 0.5 {
            blur_mask(&mut mask, canvas_w, canvas_h, self.edit.refine_feather);
        }

        if self.edit.refine_contrast > 0.5 {
            let factor = 1.0 + self.edit.refine_contrast / 50.0;
            for v in &mut mask {
                let f = *v as f32 / 255.0;
                let boosted = 0.5 + (f - 0.5) * factor;
                *v = (boosted.clamp(0.0, 1.0) * 255.0) as u8;
            }
        }

        let sel = &mut self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .selection;
        sel.mask = mask;
        sel.active = sel.mask.iter().any(|&v| v > 0);
        sel.mask_revision += 1;
        sel.mark_bbox_dirty();

        self.apply_canvas_event(CanvasEvent::SelectionChanged);
        self.edit.refine_dirty = false;
    }

    /// Cancel refine panel — restore original mask and the previous tool.
    pub fn cancel_refine_panel(&mut self) {
        if !self.edit.refine_snapshot.is_empty() {
            let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
            canvas.selection.mask = self.edit.refine_snapshot.clone();
            canvas.selection.active = canvas.selection.mask.iter().any(|&v| v > 0);
            canvas.selection.mask_revision += 1;
            canvas.selection.mark_bbox_dirty();
            self.apply_canvas_event(CanvasEvent::SelectionChanged);
        }
        self.edit.show_refine_panel = false;
        self.shell.ui.show_refine_color_dialog = false;
        self.shell.ui.refine_color_dialog_center_next = false;
        self.edit.refine_view_mode = crate::ui::refine_select::RefineViewMode::Overlay;
        self.edit.refine_output_mode = crate::ui::refine_select::RefineOutputMode::Selection;
        self.edit.refine_overlay_tex = None;
        self.edit.refine_overlay_mask_rev = u64::MAX;
        self.edit.refine_snapshot.clear();
        self.edit.tools.select(self.edit.refine_prev_tool);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Commit refine: apply output mode then close panel.
    pub fn commit_refine_panel(&mut self) {
        if !self.edit.refine_snapshot.is_empty() {
            let after_mask = self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .selection
                .mask
                .clone();
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .selection
                .mask = self.edit.refine_snapshot.clone();
            let mut cmd = crate::core::command::SelectionCommand::capture_before(
                "Refine Selection",
                &self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .selection,
            );
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .selection
                .mask = after_mask;
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .selection
                .active = self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .selection
                .mask
                .iter()
                .any(|&v| v > 0);
            cmd.capture_after(
                &self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .selection,
            );
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .record(Box::new(cmd));
        }

        match self.edit.refine_output_mode {
            crate::ui::refine_select::RefineOutputMode::Selection => {}
            crate::ui::refine_select::RefineOutputMode::LayerMask => {
                self.output_refine_as_layer_mask();
            }
            crate::ui::refine_select::RefineOutputMode::NewLayer => {
                self.output_refine_as_new_layer(false);
            }
            crate::ui::refine_select::RefineOutputMode::NewLayerWithMask => {
                self.output_refine_as_new_layer(true);
            }
        }

        self.edit.show_refine_panel = false;
        self.shell.ui.show_refine_color_dialog = false;
        self.shell.ui.refine_color_dialog_center_next = false;
        self.edit.refine_view_mode = crate::ui::refine_select::RefineViewMode::Overlay;
        self.edit.refine_output_mode = crate::ui::refine_select::RefineOutputMode::Selection;
        self.edit.refine_overlay_tex = None;
        self.edit.refine_overlay_mask_rev = u64::MAX;
        self.edit.refine_snapshot.clear();
        self.edit.tools.select(self.edit.refine_prev_tool);
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Output to Layer Mask: apply current selection mask as a mask on the active layer.
    pub(super) fn output_refine_as_layer_mask(&mut self) {
        let idx = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .active_idx;
        let canvas_w = self.docs.documents[self.docs.active_doc_idx].canvas.width;
        let canvas_h = self.docs.documents[self.docs.active_doc_idx].canvas.height;
        let sel_mask = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .selection
            .mask
            .clone();
        if sel_mask.is_empty() {
            return;
        }

        let Some(rgba_len) = crate::core::canvas::Canvas::guarded_flat_rgba_len(canvas_w, canvas_h)
        else {
            self.shell.status_msg =
                "Khong the tao layer mask: canvas vuot gioi han buffer phang".to_string();
            return;
        };
        let Some(mask_len) = crate::core::canvas::Canvas::pixel_count(canvas_w, canvas_h)
            .and_then(|n| usize::try_from(n).ok())
        else {
            return;
        };
        if sel_mask.len() < mask_len {
            self.shell.status_msg =
                "Khong the tao layer mask: selection mask khong hop le".to_string();
            return;
        }
        let mut rgba = vec![0u8; rgba_len];
        for (i, &m) in sel_mask.iter().take(mask_len).enumerate() {
            let base = i * 4;
            rgba[base] = m;
            rgba[base + 1] = m;
            rgba[base + 2] = m;
            rgba[base + 3] = 255;
        }
        let tiles = crate::core::tile::TileMap::from_rgba(&rgba, canvas_w, canvas_h);
        let mask = crate::core::layer::LayerMask {
            tiles,
            width: canvas_w,
            height: canvas_h,
            enabled: true,
            inverted: false,
        };

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
                .mask = Some(mask);
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_revision += 1;
        }
    }

    /// Output to New Layer (with_mask=false) or New Layer with Mask (with_mask=true).
    /// Duplicates the active layer; if !with_mask, clips pixels to selection.
    /// If with_mask, attaches the selection mask as a layer mask.
    pub(super) fn output_refine_as_new_layer(&mut self, with_mask: bool) {
        let idx = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .active_idx;
        let canvas_w = self.docs.documents[self.docs.active_doc_idx].canvas.width;
        let canvas_h = self.docs.documents[self.docs.active_doc_idx].canvas.height;
        let sel_mask = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .selection
            .mask
            .clone();

        let new_idx = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .duplicate_layer(idx);
        self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_revision += 1;

        if with_mask {
            if !sel_mask.is_empty() {
                let Some(rgba_len) =
                    crate::core::canvas::Canvas::guarded_flat_rgba_len(canvas_w, canvas_h)
                else {
                    self.shell.status_msg =
                        "Khong the tao layer mask: canvas vuot gioi han buffer phang".to_string();
                    return;
                };
                let Some(mask_len) = crate::core::canvas::Canvas::pixel_count(canvas_w, canvas_h)
                    .and_then(|n| usize::try_from(n).ok())
                else {
                    return;
                };
                if sel_mask.len() < mask_len {
                    self.shell.status_msg =
                        "Khong the tao layer mask: selection mask khong hop le".to_string();
                    return;
                }
                let mut rgba = vec![0u8; rgba_len];
                for (i, &m) in sel_mask.iter().take(mask_len).enumerate() {
                    let b = i * 4;
                    rgba[b] = m;
                    rgba[b + 1] = m;
                    rgba[b + 2] = m;
                    rgba[b + 3] = 255;
                }
                let tiles = crate::core::tile::TileMap::from_rgba(&rgba, canvas_w, canvas_h);
                let mask = crate::core::layer::LayerMask {
                    tiles,
                    width: canvas_w,
                    height: canvas_h,
                    enabled: true,
                    inverted: false,
                };
                if let Some(layer) = self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers
                    .get_mut(new_idx)
                {
                    layer.mask = Some(mask);
                }
            }
        } else {
            if !sel_mask.is_empty() {
                let layer = &mut self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers[new_idx];
                let lw = layer.width;
                let lh = layer.height;
                let lox = layer.offset.0;
                let loy = layer.offset.1;
                for y in 0..lh {
                    for x in 0..lw {
                        let cx = x as i32 + lox;
                        let cy = y as i32 + loy;
                        let m = if cx >= 0
                            && cy >= 0
                            && (cx as u32) < canvas_w
                            && (cy as u32) < canvas_h
                        {
                            sel_mask[(cy as u32 * canvas_w + cx as u32) as usize]
                        } else {
                            0
                        };
                        if m == 255 {
                            continue;
                        }
                        let (r, g, b, a) = layer.tiles.get_pixel(x, y);
                        let new_a = (a as u32 * m as u32 / 255) as u8;
                        layer.tiles.set_pixel(x, y, r, g, b, new_a);
                    }
                }
            }
        }

        if self.edit.refine_decontaminate && !sel_mask.is_empty() {
            self.decontaminate_refine_layer(
                new_idx,
                &sel_mask,
                canvas_w,
                canvas_h,
                self.edit.refine_decontaminate_amount,
            );
        }
    }

    pub(super) fn decontaminate_refine_layer(
        &mut self,
        layer_idx: usize,
        sel_mask: &[u8],
        canvas_w: u32,
        canvas_h: u32,
        amount: f32,
    ) {
        let amount = amount.clamp(0.0, 1.0);
        if amount <= 0.001 || canvas_w == 0 || canvas_h == 0 {
            return;
        }
        let Some(mask_len) = crate::core::canvas::Canvas::pixel_count(canvas_w, canvas_h)
            .and_then(|n| usize::try_from(n).ok())
        else {
            return;
        };
        if sel_mask.len() < mask_len {
            return;
        }

        let Some(layer) = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .layers
            .get_mut(layer_idx)
        else {
            return;
        };

        let source_tiles = layer.tiles.clone();
        let lw = layer.width;
        let lh = layer.height;
        let lox = layer.offset.0;
        let loy = layer.offset.1;
        const SEARCH_R: i32 = 8;

        let mask_at = |cx: i32, cy: i32| -> u8 {
            if cx >= 0 && cy >= 0 && (cx as u32) < canvas_w && (cy as u32) < canvas_h {
                sel_mask[(cy as u32 * canvas_w + cx as u32) as usize]
            } else {
                0
            }
        };

        for y in 0..lh {
            for x in 0..lw {
                let cx = x as i32 + lox;
                let cy = y as i32 + loy;
                let m = mask_at(cx, cy);
                if m == 0 {
                    continue;
                }
                let near_edge = m < 255
                    || mask_at(cx - 1, cy) < 245
                    || mask_at(cx + 1, cy) < 245
                    || mask_at(cx, cy - 1) < 245
                    || mask_at(cx, cy + 1) < 245;
                if !near_edge {
                    continue;
                }

                let mut sum = [0u32; 3];
                let mut count = 0u32;
                for sy in -SEARCH_R..=SEARCH_R {
                    for sx in -SEARCH_R..=SEARCH_R {
                        if sx * sx + sy * sy > SEARCH_R * SEARCH_R {
                            continue;
                        }
                        let scx = cx + sx;
                        let scy = cy + sy;
                        if mask_at(scx, scy) < 220 {
                            continue;
                        }
                        let lx = scx - lox;
                        let ly = scy - loy;
                        if lx < 0 || ly < 0 || lx >= lw as i32 || ly >= lh as i32 {
                            continue;
                        }
                        let (r, g, b, a) = source_tiles.get_pixel(lx as u32, ly as u32);
                        if a == 0 {
                            continue;
                        }
                        sum[0] += r as u32;
                        sum[1] += g as u32;
                        sum[2] += b as u32;
                        count += 1;
                    }
                }
                if count == 0 {
                    continue;
                }

                let (r, g, b, a) = source_tiles.get_pixel(x, y);
                if a == 0 {
                    continue;
                }
                let fg = [
                    (sum[0] / count) as f32,
                    (sum[1] / count) as f32,
                    (sum[2] / count) as f32,
                ];
                let edge_strength = 1.0 - (m as f32 / 255.0);
                let mix = (amount * edge_strength.sqrt()).clamp(0.0, 1.0);
                let nr = (r as f32 * (1.0 - mix) + fg[0] * mix).round() as u8;
                let ng = (g as f32 * (1.0 - mix) + fg[1] * mix).round() as u8;
                let nb = (b as f32 * (1.0 - mix) + fg[2] * mix).round() as u8;
                layer.tiles.set_pixel(x, y, nr, ng, nb, a);
            }
        }
    }
}

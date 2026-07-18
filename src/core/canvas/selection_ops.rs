//! Selection operations: shape/path/wand selects, modify ops, snapshots,
//! selection-aware clears and moves. Selection edits enter history like any
//! other command.

// Main document model — Canvas, History, metadata.
//

use super::*;
use crate::core::gateway::ChangeKind;

impl Canvas {
    /// bounding_box() in selection.rs is cached.
    pub fn selection_bbox(&mut self) -> (f32, f32, f32, f32) {
        self.selection.bounding_box()
    }

    /// Snapshot the current selection (for live preview in tools).
    /// A tool saves the snapshot before dragging and restores it before each preview step.
    /// Not written to cmd_history — this is temporary state within a stroke.
    pub fn snapshot_selection(&self) -> crate::core::selection::SelectionSnapshot {
        self.selection.snapshot()
    }

    /// Restore the selection from a snapshot (no history).
    pub fn restore_selection_snapshot(&mut self, snap: &crate::core::selection::SelectionSnapshot) {
        self.selection.restore_snapshot(snap);
    }

    pub fn select_rect(&mut self, x0: u32, y0: u32, x1: u32, y1: u32) {
        let mut cmd =
            crate::core::command::SelectionCommand::capture_before("Select Rect", &self.selection);
        self.selection.select_rect(x0, y0, x1, y1);
        cmd.capture_after(&self.selection);
        self.record_as(Box::new(cmd), ChangeKind::Selection);
    }

    pub fn select_all(&mut self) {
        let mut cmd =
            crate::core::command::SelectionCommand::capture_before("Select All", &self.selection);
        self.selection.select_all();
        cmd.capture_after(&self.selection);
        self.record_as(Box::new(cmd), ChangeKind::Selection);
    }

    pub fn deselect(&mut self) {
        let mut cmd =
            crate::core::command::SelectionCommand::capture_before("Deselect", &self.selection);
        self.selection.deselect();
        cmd.capture_after(&self.selection);
        self.record_as(Box::new(cmd), ChangeKind::Selection);
    }

    pub fn invert_selection(&mut self) {
        let mut cmd = crate::core::command::SelectionCommand::capture_before(
            "Invert Selection",
            &self.selection,
        );
        self.selection.invert();
        cmd.capture_after(&self.selection);
        self.record_as(Box::new(cmd), ChangeKind::Selection);
    }

    pub fn select_ellipse_mode(
        &mut self,
        x0: u32,
        y0: u32,
        x1: u32,
        y1: u32,
        mode: crate::core::selection::SelectionMode,
        feather: f32,
        anti_alias: bool,
    ) {
        let mut cmd = crate::core::command::SelectionCommand::capture_before(
            "Select Ellipse",
            &self.selection,
        );
        self.selection
            .select_ellipse_mode(x0, y0, x1, y1, mode, feather, anti_alias);
        cmd.capture_after(&self.selection);
        self.record_as(Box::new(cmd), ChangeKind::Selection);
        self.dirty.expand_full(self.width, self.height);
    }

    pub fn select_rect_mode(
        &mut self,
        x0: u32,
        y0: u32,
        x1: u32,
        y1: u32,
        mode: crate::core::selection::SelectionMode,
        feather: f32,
        _anti_alias: bool,
    ) {
        let mut cmd =
            crate::core::command::SelectionCommand::capture_before("Select Rect", &self.selection);
        let new_mask = {
            let Some(mask_len) =
                Self::pixel_count(self.width, self.height).and_then(|n| usize::try_from(n).ok())
            else {
                return;
            };
            let mut m = vec![0u8; mask_len];
            if feather == 0.0 {
                let x1c = x1.min(self.width);
                let y1c = y1.min(self.height);
                for y in y0..y1c {
                    for x in x0..x1c {
                        m[(y * self.width + x) as usize] = 255;
                    }
                }
            } else {
                let rx0 = x0 as f32;
                let ry0 = y0 as f32;
                let rx1 = x1 as f32;
                let ry1 = y1 as f32;

                let pad = feather.ceil() as u32;
                let fx0 = x0.saturating_sub(pad);
                let fy0 = y0.saturating_sub(pad);
                let fx1 = (x1 + pad).min(self.width);
                let fy1 = (y1 + pad).min(self.height);

                for y in fy0..fy1 {
                    for x in fx0..fx1 {
                        let px = x as f32 + 0.5;
                        let py = y as f32 + 0.5;

                        let dx = (rx0 - px).max(0.0).max(px - rx1);
                        let dy = (ry0 - py).max(0.0).max(py - ry1);
                        let dist = (dx * dx + dy * dy).sqrt();

                        if dist <= feather {
                            let alpha = (1.0 - dist / feather).clamp(0.0, 1.0);
                            m[(y * self.width + x) as usize] = (alpha * 255.0) as u8;
                        }
                    }
                }
            }
            m
        };
        self.selection.apply_with_mode(new_mask, mode);
        cmd.capture_after(&self.selection);
        self.record_as(Box::new(cmd), ChangeKind::Selection);
        self.dirty.expand_full(self.width, self.height);
    }

    pub fn select_path_mode(
        &mut self,
        points: &[(f32, f32)],
        mode: crate::core::selection::SelectionMode,
    ) {
        let mut cmd =
            crate::core::command::SelectionCommand::capture_before("Select Path", &self.selection);
        let new_mask = self.selection.build_path_mask(points);
        self.selection.apply_with_mode(new_mask, mode);
        cmd.capture_after(&self.selection);
        self.record_as(Box::new(cmd), ChangeKind::Selection);
        self.dirty.expand_full(self.width, self.height);
    }

    pub fn select_polygon_mode(
        &mut self,
        points: &[(f32, f32)],
        mode: crate::core::selection::SelectionMode,
        feather: f32,
        _anti_alias: bool,
    ) {
        let mut cmd = crate::core::command::SelectionCommand::capture_before(
            "Select Polygon",
            &self.selection,
        );
        let mut new_mask = self.selection.build_polygon_mask(points);
        if feather > 0.0 {
            crate::core::selection::blur_mask(
                &mut new_mask,
                self.width as usize,
                self.height as usize,
                feather,
            );
        }
        self.selection.apply_with_mode(new_mask, mode);
        cmd.capture_after(&self.selection);
        self.record_as(Box::new(cmd), ChangeKind::Selection);
        self.dirty.expand_full(self.width, self.height);
    }

    pub fn feather_selection(&mut self, radius: f32) {
        let mut cmd = crate::core::command::SelectionCommand::capture_before(
            "Feather Selection",
            &self.selection,
        );
        self.selection.feather(radius);
        cmd.capture_after(&self.selection);
        self.record_as(Box::new(cmd), ChangeKind::Selection);
        self.dirty.expand_full(self.width, self.height);
    }

    pub fn grow_selection(&mut self, pixels: u32) {
        let mut cmd = crate::core::command::SelectionCommand::capture_before(
            "Grow Selection",
            &self.selection,
        );
        self.selection.grow(pixels);
        cmd.capture_after(&self.selection);
        self.record_as(Box::new(cmd), ChangeKind::Selection);
        self.dirty.expand_full(self.width, self.height);
    }

    pub fn shrink_selection(&mut self, pixels: u32) {
        let mut cmd = crate::core::command::SelectionCommand::capture_before(
            "Shrink Selection",
            &self.selection,
        );
        self.selection.shrink(pixels);
        cmd.capture_after(&self.selection);
        self.record_as(Box::new(cmd), ChangeKind::Selection);
        self.dirty.expand_full(self.width, self.height);
    }

    pub fn smooth_selection(&mut self, radius: f32) {
        let mut cmd = crate::core::command::SelectionCommand::capture_before(
            "Smooth Selection",
            &self.selection,
        );
        self.selection.smooth(radius);
        cmd.capture_after(&self.selection);
        self.record_as(Box::new(cmd), ChangeKind::Selection);
        self.dirty.expand_full(self.width, self.height);
    }

    pub fn border_selection(&mut self, width: u32) {
        let mut cmd = crate::core::command::SelectionCommand::capture_before(
            "Border Selection",
            &self.selection,
        );
        self.selection.border(width);
        cmd.capture_after(&self.selection);
        self.record_as(Box::new(cmd), ChangeKind::Selection);
        self.dirty.expand_full(self.width, self.height);
    }

    /// Load a layer's pixel transparency (alpha) as the active selection — like
    /// the standard Ctrl+click on a layer thumbnail. Returns false and leaves the
    /// selection unchanged when the layer has no opaque pixels (e.g. an empty,
    /// group, or adjustment layer). Undoable.
    pub fn load_layer_alpha_selection(&mut self, layer_idx: usize) -> bool {
        let cw = self.width;
        let ch = self.height;
        let mask_len = self.selection.mask.len();
        if mask_len != (cw as usize) * (ch as usize) {
            return false;
        }
        let Some(layer) = self.layer_stack.layers.get(layer_idx) else {
            return false;
        };

        let mut mask = vec![0u8; mask_len];
        let ox = layer.offset.0;
        let oy = layer.offset.1;
        let ts = crate::core::tile::TILE_SIZE as i32;
        let mut any = false;
        for (pos, tile) in layer.tiles.tiles.iter() {
            let base_cx = pos.x * ts + ox;
            let base_cy = pos.y * ts + oy;
            for ty in 0..ts {
                let cy = base_cy + ty;
                if cy < 0 || cy >= ch as i32 {
                    continue;
                }
                let row = cy as usize * cw as usize;
                for tx in 0..ts {
                    let cx = base_cx + tx;
                    if cx < 0 || cx >= cw as i32 {
                        continue;
                    }
                    let a = tile.pixels[((ty * ts + tx) * 4 + 3) as usize];
                    if a > 0 {
                        mask[row + cx as usize] = a;
                        any = true;
                    }
                }
            }
        }

        if !any {
            return false;
        }

        let mut cmd = crate::core::command::SelectionCommand::capture_before(
            "Load Layer Selection",
            &self.selection,
        );
        self.selection
            .apply_with_mode(mask, crate::core::selection::SelectionMode::New);
        cmd.capture_after(&self.selection);
        self.record_as(Box::new(cmd), ChangeKind::Selection);
        self.dirty.expand_full(self.width, self.height);
        true
    }

    /// Wand: flood-fill select from (sx, sy) with options.
    pub fn select_wand(
        &mut self,
        sx: u32,
        sy: u32,
        tolerance: u8,
        edge_sensitivity: u8,
        contiguous: bool,
        anti_alias: bool,
        feather: f32,
        sample_merged: bool,
        mode: crate::core::selection::SelectionMode,
    ) {
        if self.layer_stack.layers.is_empty() {
            return;
        }
        self.layer_stack.normalize_active_idx();
        let pixels: Vec<u8> = if sample_merged {
            self.ensure_pixels();
            self.pixels.clone()
        } else {
            let idx = self.layer_stack.active_idx;
            self.layer_stack.layers[idx]
                .tiles
                .extract_region(0, 0, self.width, self.height)
        };

        let raw_mask = crate::core::selection::flood_fill_mask(
            &pixels,
            self.width,
            self.height,
            sx,
            sy,
            tolerance,
            edge_sensitivity,
            contiguous,
            anti_alias,
        );

        let mut cmd =
            crate::core::command::SelectionCommand::capture_before("Wand", &self.selection);
        self.selection.apply_with_mode(raw_mask, mode);
        if feather > 0.0 {
            self.selection.feather(feather);
        }
        cmd.capture_after(&self.selection);
        self.record_as(Box::new(cmd), ChangeKind::Selection);
    }

    /// Smart Select stamp — SLIC superpixel BFS with Lab color + Sobel edge stop.
    ///
    /// Subtract mode: fast circle erase (no edge cache needed).
    /// Add/New: lazy-compute EdgeCache (Lab + Sobel, ~15-30ms, once per layer revision),
    ///          then pixel BFS per stamp (<5ms).
    /// Cache is invalidated automatically when layer_revision changes.
    pub fn smart_select_brush_stamp(
        &mut self,
        cx: f32,
        cy: f32,
        brush_radius: f32,
        tolerance: u8,
        edge_sensitivity: u8,
        mode: crate::core::selection::SelectionMode,
        sample_merged: bool,
    ) {
        use crate::core::selection::{
            compute_sobel, pixels_to_lab, smart_select_stamp_region, EdgeCache,
        };

        let w = self.width;
        let h = self.height;
        if w == 0 || h == 0 {
            return;
        }

        if matches!(mode, crate::core::selection::SelectionMode::Subtract) {
            self.paint_selection_brush(cx, cy, brush_radius, mode);
            return;
        }

        if self.layer_stack.layers.is_empty() {
            return;
        }
        self.layer_stack.normalize_active_idx();
        let active_idx = self.layer_stack.active_idx;
        let curr_rev = self.layer_revision;

        let needs_recompute = self.edge_cache.as_ref().map_or(true, |c| {
            c.layer_idx != active_idx
                || c.layer_revision != curr_rev
                || c.sample_merged != sample_merged
        });

        if needs_recompute {
            let pixels: Vec<u8> = if sample_merged {
                self.ensure_pixels();
                self.pixels.clone()
            } else {
                self.layer_stack.layers[active_idx]
                    .tiles
                    .extract_region(0, 0, w, h)
            };

            let lab = pixels_to_lab(&pixels, w, h);
            let sobel = compute_sobel(&pixels, w, h);

            self.edge_cache = Some(Box::new(EdgeCache {
                lab,
                sobel,
                width: w,
                height: h,
                layer_idx: active_idx,
                layer_revision: curr_rev,
                sample_merged,
            }));
        }

        let sx = cx.round().clamp(0.0, (w - 1) as f32) as u32;
        let sy = cy.round().clamp(0.0, (h - 1) as f32) as u32;

        let stamp = smart_select_stamp_region(
            self.edge_cache.as_ref().unwrap(),
            sx,
            sy,
            brush_radius,
            tolerance,
            edge_sensitivity,
        );

        let mask = &mut self.selection.mask;
        let canvas_w = w as usize;
        let stamp_has_any = stamp.mask.iter().any(|&v| v > 0);
        match mode {
            crate::core::selection::SelectionMode::Subtract => unreachable!(),
            crate::core::selection::SelectionMode::Intersect => {
                for (i, v) in mask.iter_mut().enumerate() {
                    let x = i % canvas_w;
                    let y = i / canvas_w;
                    if x < stamp.x0
                        || y < stamp.y0
                        || x >= stamp.x0 + stamp.width
                        || y >= stamp.y0 + stamp.height
                    {
                        *v = 0;
                        continue;
                    }
                    let sx = x - stamp.x0;
                    let sy = y - stamp.y0;
                    if stamp.mask[sy * stamp.width + sx] == 0 {
                        *v = 0;
                    }
                }
                self.selection.active = crate::core::selection::mask_has_any(mask);
            }
            _ => {
                for y in 0..stamp.height {
                    let src = y * stamp.width;
                    let dst = (stamp.y0 + y) * canvas_w + stamp.x0;
                    for x in 0..stamp.width {
                        if stamp.mask[src + x] > 0 {
                            mask[dst + x] = 255;
                        }
                    }
                }
                if stamp_has_any {
                    self.selection.active = true;
                } else {
                    self.selection.active = crate::core::selection::mask_has_any(mask);
                }
            }
        }
        self.selection.mask_revision += 1;
        self.selection.mark_bbox_dirty();
    }

    /// Paint the selection with a solid circular brush (color-agnostic).
    /// Add: paint 255; Subtract: paint 0.
    /// Doesn't push undo — the caller manages it.
    pub fn paint_selection_brush(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        mode: crate::core::selection::SelectionMode,
    ) {
        let w = self.width;
        let h = self.height;
        if w == 0 || h == 0 {
            return;
        }

        let x0 = ((cx - radius).floor() as i32).max(0) as u32;
        let y0 = ((cy - radius).floor() as i32).max(0) as u32;
        let x1 = ((cx + radius).ceil() as i32 + 1).min(w as i32) as u32;
        let y1 = ((cy + radius).ceil() as i32 + 1).min(h as i32) as u32;

        let mask = &mut self.selection.mask;
        let r2 = radius * radius;
        let subtract = matches!(mode, crate::core::selection::SelectionMode::Subtract);

        for y in y0..y1 {
            for x in x0..x1 {
                let dx = x as f32 - cx;
                let dy = y as f32 - cy;
                if dx * dx + dy * dy > r2 {
                    continue;
                }
                let i = (y * w + x) as usize;
                if subtract {
                    mask[i] = 0;
                } else {
                    mask[i] = 255;
                }
            }
        }
        self.selection.active = crate::core::selection::mask_has_any(mask);
        self.selection.mask_revision += 1;
        self.selection.mark_bbox_dirty();
    }

    /// Refine Brush stamp — intelligent alpha matting for selection edges.
    ///
    /// Uses the EdgeCache (Lab + Sobel) from Quick Select; recomputes when stale.
    /// Smart mode: color-based alpha matting — pixels similar to FG get alpha ≈ 1,
    ///             pixels similar to BG get alpha ≈ 0. Great for hair/fur detail.
    /// Add / Subtract: soft-edge brush that force-includes / force-excludes pixels.
    pub fn refine_edge_stamp(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        hardness: f32,
        mode: crate::core::selection::RefineBrushMode,
        sample_merged: bool,
    ) {
        use crate::core::selection::{compute_sobel, pixels_to_lab, refine_edge_stamp, EdgeCache};

        let w = self.width;
        let h = self.height;
        if w == 0 || h == 0 {
            return;
        }

        if !matches!(mode, crate::core::selection::RefineBrushMode::Smart) {
            let mask = &mut self.selection.mask;
            let icx = cx as i32;
            let icy = cy as i32;
            let ir = radius.ceil() as i32;

            #[inline]
            fn falloff(dist: f32, radius: f32, hardness: f32) -> f32 {
                let t = (dist / radius).clamp(0.0, 1.0);
                let soft_edge = 1.0 - hardness;
                if soft_edge < 0.01 {
                    if t < 1.0 {
                        1.0
                    } else {
                        0.0
                    }
                } else {
                    let fs = 1.0 - soft_edge;
                    if t <= fs {
                        1.0
                    } else {
                        let ft = (t - fs) / soft_edge;
                        1.0 - ft * ft
                    }
                }
            }

            for dy in -ir..=ir {
                for dx in -ir..=ir {
                    let px = icx + dx;
                    let py = icy + dy;
                    if px < 0 || py < 0 || px >= w as i32 || py >= h as i32 {
                        continue;
                    }
                    let dist = ((dx * dx + dy * dy) as f32).sqrt();
                    if dist > radius {
                        continue;
                    }
                    let weight = falloff(dist, radius, hardness);
                    let i = (py as u32 * w + px as u32) as usize;
                    match mode {
                        crate::core::selection::RefineBrushMode::Add => {
                            let v = (weight * 255.0) as u8;
                            mask[i] = mask[i].max(v);
                        }
                        crate::core::selection::RefineBrushMode::Subtract => {
                            mask[i] = (mask[i] as f32 * (1.0 - weight)).round() as u8;
                        }
                        _ => {}
                    }
                }
            }
            self.selection.active = crate::core::selection::mask_has_any(mask);
            self.selection.mask_revision += 1;
            self.selection.mark_bbox_dirty();
            return;
        }

        if self.layer_stack.layers.is_empty() {
            return;
        }
        self.layer_stack.normalize_active_idx();
        let active_idx = self.layer_stack.active_idx;
        let curr_rev = self.layer_revision;

        let needs_recompute = self.edge_cache.as_ref().map_or(true, |c| {
            c.layer_idx != active_idx
                || c.layer_revision != curr_rev
                || c.sample_merged != sample_merged
        });

        if needs_recompute {
            let pixels: Vec<u8> = if sample_merged {
                self.ensure_pixels();
                self.pixels.clone()
            } else {
                self.layer_stack.layers[active_idx]
                    .tiles
                    .extract_region(0, 0, w, h)
            };
            let lab = pixels_to_lab(&pixels, w, h);
            let sobel = compute_sobel(&pixels, w, h);
            self.edge_cache = Some(Box::new(EdgeCache {
                lab,
                sobel,
                width: w,
                height: h,
                layer_idx: active_idx,
                layer_revision: curr_rev,
                sample_merged,
            }));
        }

        let cache = self.edge_cache.as_ref().unwrap();
        let lab = &cache.lab;
        let sobel = &cache.sobel;
        refine_edge_stamp(
            lab,
            sobel,
            &mut self.selection.mask,
            w,
            h,
            cx,
            cy,
            radius,
            hardness,
            mode,
        );
        self.selection.active = crate::core::selection::mask_has_any(&self.selection.mask);
        self.selection.mask_revision += 1;
        self.selection.mark_bbox_dirty();
    }

    /// Erase the active selection's pixels on the active raster layer (Edit → Cut).
    /// Alpha inside the selection is driven toward 0, feathered by the selection
    /// mask. Undoable as one "Cut" stroke. Returns `false` (no-op) when there is no
    /// active selection or the layer is locked.
    pub fn clear_selection(&mut self) -> bool {
        if !self.selection.active {
            return false;
        }
        if self.active_layer().locked && !self.active_layer().is_background {
            return false;
        }

        let ox = self.active_layer().offset.0;
        let oy = self.active_layer().offset.1;
        let layer_w = self.active_layer().width as i32;
        let layer_h = self.active_layer().height as i32;

        self.selection.refresh_bbox();
        let bbox = self.selection.bounding_box();
        let (sel_x0, sel_y0, sel_x1, sel_y1) =
            (bbox.0 as i32, bbox.1 as i32, bbox.2 as i32, bbox.3 as i32);

        let fill_x0 = sel_x0.max(ox).max(0) as u32;
        let fill_y0 = sel_y0.max(oy).max(0) as u32;
        let fill_x1 = sel_x1.min(ox + layer_w).min(self.width as i32).max(0) as u32;
        let fill_y1 = sel_y1.min(oy + layer_h).min(self.height as i32).max(0) as u32;
        if fill_x1 <= fill_x0 || fill_y1 <= fill_y0 {
            return false;
        }
        let fill_w = fill_x1 - fill_x0;
        let fill_h = fill_y1 - fill_y0;
        let lx0 = (fill_x0 as i32 - ox) as u32;
        let ly0 = (fill_y0 as i32 - oy) as u32;

        let Some(region_len) = Self::checked_rgba_len(fill_w, fill_h) else {
            return false;
        };
        self.begin_stroke("Cut");

        let mut region = self
            .active_layer()
            .get_paint_tiles()
            .map(|tiles| tiles.extract_region(lx0, ly0, fill_w, fill_h))
            .unwrap_or_else(|| vec![0u8; region_len]);

        for y in 0..fill_h {
            for x in 0..fill_w {
                let cx = fill_x0 + x;
                let cy = fill_y0 + y;
                let sel_a = self.selection.sample(cx, cy);
                if sel_a < 0.001 {
                    continue;
                }
                let i = ((y * fill_w + x) * 4) as usize;
                let new_a = (region[i + 3] as f32 * (1.0 - sel_a)).round() as u8;
                region[i + 3] = new_a;
                if new_a == 0 {
                    region[i] = 0;
                    region[i + 1] = 0;
                    region[i + 2] = 0;
                }
            }
        }

        self.active_layer_mut()
            .update_tiles_region(lx0, ly0, fill_w, fill_h, &region);
        self.end_stroke();
        self.dirty.expand_full(self.width, self.height);
        true
    }

    /// Move tool — moves the active layer's pixel CONTENT by the selection
    /// (standard raster editors behavior), instead of just moving the marquee frame.
    ///
    /// During the drag, `selection.offset` is moved live (previewing the marquee at
    /// the destination). This is called on_release: cut pixels at the ORIGINAL position
    /// (dest − delta) and paste them at the current marquee position, recording one undo
    /// group of PaintCommand (tiles) + TranslateSelectionCommand (offset) → Ctrl+Z restores
    /// both pixels and marquee.
    ///
    /// Returns `false` if the layer is locked / not raster (caller falls back to moving
    /// only the marquee). Content whose destination is offscreen isn't moved (kept at the
    /// original position) — safe, no silent data loss.
    pub fn move_selected_content(&mut self, dx: i32, dy: i32) -> bool {
        if (dx == 0 && dy == 0) || !self.selection.active {
            return false;
        }
        self.layer_stack.normalize_active_idx();
        let idx = self.layer_stack.active_idx;
        {
            let l = &self.layer_stack.layers[idx];
            if (l.locked && !l.is_background) || !l.is_raster() {
                return false;
            }
        }

        self.selection.refresh_bbox();
        let (bx0, by0, bx1, by1) = self.selection.bounding_box_cached();
        let dest_x0 = (bx0 as i32).clamp(0, self.width as i32);
        let dest_y0 = (by0 as i32).clamp(0, self.height as i32);
        let dest_x1 = (bx1 as i32).clamp(0, self.width as i32);
        let dest_y1 = (by1 as i32).clamp(0, self.height as i32);
        if dest_x1 <= dest_x0 || dest_y1 <= dest_y0 {
            return false;
        }

        let layer_id = self.layer_stack.layers[idx].id;
        let ox = self.layer_stack.layers[idx].offset.0;
        let oy = self.layer_stack.layers[idx].offset.1;
        let lw = self.layer_stack.layers[idx].width as i32;
        let lh = self.layer_stack.layers[idx].height as i32;
        let before = self.layer_stack.layers[idx].tiles.clone();

        let mut lifted: Vec<(i32, i32, u8, u8, u8, f32)> = Vec::new();
        let mut cleared: Vec<(i32, i32, f32)> = Vec::new();
        {
            let tiles = &self.layer_stack.layers[idx].tiles;
            let read = |cx: i32, cy: i32| -> (u8, u8, u8, u8) {
                let lx = cx - ox;
                let ly = cy - oy;
                if lx < 0 || ly < 0 || lx >= lw || ly >= lh {
                    (0, 0, 0, 0)
                } else {
                    tiles.get_pixel(lx as u32, ly as u32)
                }
            };
            for cy in dest_y0..dest_y1 {
                for cx in dest_x0..dest_x1 {
                    let w = self.selection.sample(cx as u32, cy as u32);
                    if w <= 0.001 {
                        continue;
                    }
                    let scx = cx - dx;
                    let scy = cy - dy;
                    let (r, g, b, a) = read(scx, scy);
                    let la = (a as f32 / 255.0) * w;
                    if la > 0.0 {
                        lifted.push((cx, cy, r, g, b, la));
                    }
                    cleared.push((scx, scy, 1.0 - w));
                }
            }
        }

        {
            let tiles = &mut self.layer_stack.layers[idx].tiles;
            for &(scx, scy, keep) in &cleared {
                let lx = scx - ox;
                let ly = scy - oy;
                if lx < 0 || ly < 0 || lx >= lw || ly >= lh {
                    continue;
                }
                let (r, g, b, a) = tiles.get_pixel(lx as u32, ly as u32);
                let na = (a as f32 * keep).round() as u8;
                if na == 0 {
                    tiles.set_pixel(lx as u32, ly as u32, 0, 0, 0, 0);
                } else {
                    tiles.set_pixel(lx as u32, ly as u32, r, g, b, na);
                }
            }
            for &(cx, cy, r, g, b, la) in &lifted {
                let lx = cx - ox;
                let ly = cy - oy;
                if lx < 0 || ly < 0 || lx >= lw || ly >= lh {
                    continue;
                }
                let (dr, dg, db, da_u) = tiles.get_pixel(lx as u32, ly as u32);
                let da = da_u as f32 / 255.0;
                let out_a = la + da * (1.0 - la);
                if out_a <= 0.0001 {
                    tiles.set_pixel(lx as u32, ly as u32, 0, 0, 0, 0);
                    continue;
                }
                let blend = |s: u8, d: u8| -> u8 {
                    ((s as f32 * la + d as f32 * da * (1.0 - la)) / out_a)
                        .round()
                        .clamp(0.0, 255.0) as u8
                };
                tiles.set_pixel(
                    lx as u32,
                    ly as u32,
                    blend(r, dr),
                    blend(g, dg),
                    blend(b, db),
                    (out_a * 255.0).round().clamp(0.0, 255.0) as u8,
                );
            }
        }

        let after = self.layer_stack.layers[idx].tiles.clone();

        let mut snap = crate::core::command::DeltaSnapshot::capture_before(
            &before,
            layer_id,
            crate::core::layer::PaintTarget::Pixels,
        );
        snap.after_tiles = after;
        self.begin_undo_group("Move Selection Content");
        self.record_as(
            Box::new(crate::core::command::PaintCommand::new(
                "Move Selection Content",
                snap,
            )),
            ChangeKind::LayerPixels,
        );
        let sel_cmd = crate::core::command::TranslateSelectionCommand::from_applied_move(
            &self.selection,
            dx,
            dy,
        );
        self.record_as(Box::new(sel_cmd), ChangeKind::Selection);
        self.end_undo_group();

        self.layer_revision += 1;
        self.dirty.expand_full(self.width, self.height);
        self.flatten_full();
        true
    }
}

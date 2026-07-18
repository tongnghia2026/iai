//! Raster mutations on layer pixels: fills, strokes, patch/heal, destructive
//! adjustments/filters and their preview/commit tile plumbing.

// Main document model — Canvas, History, metadata.
//

use super::*;
use crate::core::gateway::ChangeKind;
use crate::core::layer::AdjustmentType;

impl Canvas {
    pub(crate) fn build_adjusted_layer_tiles(
        &self,
        source_tiles: &crate::core::tile::TileMap,
        layer_w: u32,
        layer_h: u32,
        ox: i32,
        oy: i32,
        adj: &AdjustmentType,
    ) -> crate::core::tile::TileMap {
        let selection = self.selection.clone();

        // 16-bit path: a layer that still holds its 16-bit master is adjusted at
        // full precision (no banding) and stays 16-bit.
        if source_tiles.has_hdr() {
            let mut px16 = source_tiles.flatten16();
            if selection.active {
                for y in 0..layer_h {
                    for x in 0..layer_w {
                        let cx = ox + x as i32;
                        let cy = oy + y as i32;
                        if cx < 0 || cy < 0 || cx >= self.width as i32 || cy >= self.height as i32 {
                            continue;
                        }
                        let sel_a = selection.sample(cx as u32, cy as u32);
                        if sel_a <= 0.001 {
                            continue;
                        }
                        let i = ((y * layer_w + x) * 4) as usize;
                        if i + 3 >= px16.len() {
                            continue;
                        }
                        let old = [px16[i], px16[i + 1], px16[i + 2], px16[i + 3]];
                        let (nr, ng, nb, na) = adj.apply_pixel16(old[0], old[1], old[2], old[3]);
                        if sel_a >= 0.999 {
                            px16[i] = nr;
                            px16[i + 1] = ng;
                            px16[i + 2] = nb;
                            px16[i + 3] = na;
                        } else {
                            let inv = 1.0 - sel_a;
                            px16[i] = (nr as f32 * sel_a + old[0] as f32 * inv).round() as u16;
                            px16[i + 1] = (ng as f32 * sel_a + old[1] as f32 * inv).round() as u16;
                            px16[i + 2] = (nb as f32 * sel_a + old[2] as f32 * inv).round() as u16;
                            px16[i + 3] = (na as f32 * sel_a + old[3] as f32 * inv).round() as u16;
                        }
                    }
                }
            } else {
                adj.apply_to_pixels16(&mut px16);
            }
            return crate::core::tile::TileMap::from_rgba16(&px16, layer_w, layer_h);
        }

        let mut pixels = source_tiles.flatten();

        if selection.active {
            for y in 0..layer_h {
                for x in 0..layer_w {
                    let cx = ox + x as i32;
                    let cy = oy + y as i32;
                    if cx < 0 || cy < 0 || cx >= self.width as i32 || cy >= self.height as i32 {
                        continue;
                    }
                    let sel_a = selection.sample(cx as u32, cy as u32);
                    if sel_a <= 0.001 {
                        continue;
                    }

                    let i = ((y * layer_w + x) * 4) as usize;
                    if i + 3 >= pixels.len() {
                        continue;
                    }

                    let old = [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]];
                    let (nr, ng, nb, na) = adj.apply_pixel(old[0], old[1], old[2], old[3]);
                    if sel_a >= 0.999 {
                        pixels[i] = nr;
                        pixels[i + 1] = ng;
                        pixels[i + 2] = nb;
                        pixels[i + 3] = na;
                    } else {
                        let inv = 1.0 - sel_a;
                        pixels[i] = (nr as f32 * sel_a + old[0] as f32 * inv).round() as u8;
                        pixels[i + 1] = (ng as f32 * sel_a + old[1] as f32 * inv).round() as u8;
                        pixels[i + 2] = (nb as f32 * sel_a + old[2] as f32 * inv).round() as u8;
                        pixels[i + 3] = (na as f32 * sel_a + old[3] as f32 * inv).round() as u8;
                    }
                }
            }
        } else {
            adj.apply_to_pixels(&mut pixels);
        }

        crate::core::tile::TileMap::from_rgba(&pixels, layer_w, layer_h)
    }

    /// Like `build_adjusted_layer_tiles` but for neighbourhood filters: flatten
    /// the source, run the filter over the whole layer buffer, then (if a
    /// selection is active) blend the filtered result back over the original by
    /// the selection alpha so only the masked region changes.
    pub fn build_filtered_layer_tiles_with_selection(
        source_tiles: &crate::core::tile::TileMap,
        layer_w: u32,
        layer_h: u32,
        ox: i32,
        oy: i32,
        filter: &crate::core::filters::FilterType,
        selection: &crate::core::selection::Selection,
        canvas_w: u32,
        canvas_h: u32,
    ) -> crate::core::tile::TileMap {
        // 16-bit path: a layer that still holds its 16-bit master is filtered at
        // full precision (no banding) and stays 16-bit, mirroring the adjustment
        // path above.
        if source_tiles.has_hdr() {
            let original = source_tiles.flatten16();
            let mut filtered = original.clone();
            filter.apply16(&mut filtered, layer_w, layer_h);

            let result = if selection.active {
                let mut out = original.clone();
                for y in 0..layer_h {
                    for x in 0..layer_w {
                        let cx = ox + x as i32;
                        let cy = oy + y as i32;
                        if cx < 0 || cy < 0 || cx >= canvas_w as i32 || cy >= canvas_h as i32 {
                            continue;
                        }
                        let sel_a = selection.sample(cx as u32, cy as u32);
                        if sel_a <= 0.001 {
                            continue;
                        }
                        let i = ((y * layer_w + x) * 4) as usize;
                        if i + 3 >= out.len() {
                            continue;
                        }
                        if sel_a >= 0.999 {
                            out[i..i + 4].copy_from_slice(&filtered[i..i + 4]);
                        } else {
                            let inv = 1.0 - sel_a;
                            for c in 0..4 {
                                out[i + c] = (filtered[i + c] as f32 * sel_a
                                    + original[i + c] as f32 * inv)
                                    .round() as u16;
                            }
                        }
                    }
                }
                out
            } else {
                filtered
            };

            return crate::core::tile::TileMap::from_rgba16(&result, layer_w, layer_h);
        }

        let original = source_tiles.flatten();
        let mut filtered = original.clone();
        filter.apply(&mut filtered, layer_w, layer_h);

        let result = if selection.active {
            let mut out = original.clone();
            for y in 0..layer_h {
                for x in 0..layer_w {
                    let cx = ox + x as i32;
                    let cy = oy + y as i32;
                    if cx < 0 || cy < 0 || cx >= canvas_w as i32 || cy >= canvas_h as i32 {
                        continue;
                    }
                    let sel_a = selection.sample(cx as u32, cy as u32);
                    if sel_a <= 0.001 {
                        continue;
                    }
                    let i = ((y * layer_w + x) * 4) as usize;
                    if i + 3 >= out.len() {
                        continue;
                    }
                    if sel_a >= 0.999 {
                        out[i] = filtered[i];
                        out[i + 1] = filtered[i + 1];
                        out[i + 2] = filtered[i + 2];
                        out[i + 3] = filtered[i + 3];
                    } else {
                        let inv = 1.0 - sel_a;
                        for c in 0..4 {
                            out[i + c] = (filtered[i + c] as f32 * sel_a
                                + original[i + c] as f32 * inv)
                                .round() as u8;
                        }
                    }
                }
            }
            out
        } else {
            filtered
        };

        crate::core::tile::TileMap::from_rgba(&result, layer_w, layer_h)
    }

    pub(crate) fn build_filtered_layer_tiles(
        &self,
        source_tiles: &crate::core::tile::TileMap,
        layer_w: u32,
        layer_h: u32,
        ox: i32,
        oy: i32,
        filter: &crate::core::filters::FilterType,
    ) -> crate::core::tile::TileMap {
        Self::build_filtered_layer_tiles_with_selection(
            source_tiles,
            layer_w,
            layer_h,
            ox,
            oy,
            filter,
            &self.selection,
            self.width,
            self.height,
        )
    }

    /// Apply `filter` to the layer's pixels for a live preview (overwrites the
    /// layer tiles from `source_tiles`). Mirrors `preview_adjustment_on_layer`;
    /// commit/cancel reuse `commit_layer_tiles_change` / `restore_layer_tiles`.
    pub fn preview_filter_on_layer(
        &mut self,
        layer_id: u32,
        source_tiles: &crate::core::tile::TileMap,
        filter: &crate::core::filters::FilterType,
    ) -> bool {
        let Some(idx) = self
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == layer_id)
        else {
            return false;
        };
        let layer = &self.layer_stack.layers[idx];
        if (!layer.is_background && layer.locked) || !layer.is_raster() {
            return false;
        }

        let (layer_w, layer_h, ox, oy) =
            (layer.width, layer.height, layer.offset.0, layer.offset.1);
        let filtered =
            self.build_filtered_layer_tiles(source_tiles, layer_w, layer_h, ox, oy, filter);
        self.layer_stack.layers[idx].tiles = filtered;
        self.layer_revision += 1;
        self.dirty.expand_full(self.width, self.height);
        self.flatten_full();
        true
    }

    /// CMYK twin of `build_adjusted_layer_tiles`: applies an ink-native
    /// adjustment (Levels/Curves, channel slots `[C, M, Y, K]`) to the layer's
    /// ink planes and re-projects the RGB mirror. `None` when the adjustment
    /// has no ink meaning or the canvas has no CMYK converter — callers must
    /// refuse rather than fall back to the RGB path, which would orphan the
    /// ink ground truth.
    pub(crate) fn build_ink_adjusted_layer_tiles(
        &self,
        source_tiles: &crate::core::tile::TileMap,
        ox: i32,
        oy: i32,
        adj: &AdjustmentType,
    ) -> Option<crate::core::tile::TileMap> {
        let conv = self.cmyk_converter()?;
        let luts = adj.ink_luts()?;
        let selection = &self.selection;
        let (cw, ch) = (self.width as i64, self.height as i64);
        let mut tiles = source_tiles.clone();
        tiles.apply_ink_luts(&luts, &conv, |lx, ly| {
            if !selection.active {
                return 1.0;
            }
            let cx = ox as i64 + lx;
            let cy = oy as i64 + ly;
            if cx < 0 || cy < 0 || cx >= cw || cy >= ch {
                return 0.0;
            }
            selection.sample(cx as u32, cy as u32)
        });
        Some(tiles)
    }

    pub fn preview_adjustment_on_layer(
        &mut self,
        layer_id: u32,
        source_tiles: &crate::core::tile::TileMap,
        adj: &AdjustmentType,
    ) -> bool {
        let Some(idx) = self
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == layer_id)
        else {
            return false;
        };
        let layer = &self.layer_stack.layers[idx];
        if (!layer.is_background && layer.locked) || !layer.is_raster() {
            return false;
        }

        let (layer_w, layer_h, ox, oy) =
            (layer.width, layer.height, layer.offset.0, layer.offset.1);
        let adjusted = if self.is_cmyk() {
            match self.build_ink_adjusted_layer_tiles(source_tiles, ox, oy, adj) {
                Some(t) => t,
                None => return false,
            }
        } else {
            self.build_adjusted_layer_tiles(source_tiles, layer_w, layer_h, ox, oy, adj)
        };
        self.layer_stack.layers[idx].tiles = adjusted;
        self.layer_revision += 1;
        self.dirty.expand_full(self.width, self.height);
        self.flatten_full();
        true
    }

    pub fn restore_layer_tiles(
        &mut self,
        layer_id: u32,
        tiles: crate::core::tile::TileMap,
    ) -> bool {
        let Some(idx) = self
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == layer_id)
        else {
            return false;
        };
        self.layer_stack.layers[idx].tiles = tiles;
        self.layer_revision += 1;
        self.dirty.expand_full(self.width, self.height);
        self.flatten_full();
        true
    }

    pub fn preview_layer_tiles(
        &mut self,
        layer_id: u32,
        tiles: crate::core::tile::TileMap,
    ) -> bool {
        let Some(idx) = self
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == layer_id)
        else {
            return false;
        };
        self.layer_stack.layers[idx].tiles = tiles;
        self.layer_revision += 1;
        self.dirty.expand_full(self.width, self.height);
        self.pixels_stale = true;
        true
    }

    pub fn commit_layer_tiles_change(
        &mut self,
        layer_id: u32,
        before_tiles: crate::core::tile::TileMap,
        after_tiles: crate::core::tile::TileMap,
        label: &str,
    ) -> bool {
        let Some(idx) = self
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == layer_id)
        else {
            return false;
        };

        self.layer_stack.layers[idx].tiles = before_tiles;
        let mut cmd = crate::core::command::LayerStructureCommand::capture_before(
            label,
            &self.layer_stack,
            self.width,
            self.height,
        );
        self.layer_stack.layers[idx].tiles = after_tiles;
        cmd.capture_after(&self.layer_stack, self.width, self.height);
        self.record_as(Box::new(cmd), ChangeKind::LayerPixels);
        self.layer_revision += 1;
        self.dirty.expand_full(self.width, self.height);
        self.flatten_full();
        true
    }

    /// Smart Fill: replace the pixels of the active raster layer that fall
    /// inside the current selection with texture synthesised from the rest of the
    /// layer (multi-scale PatchMatch). Undoable; reuses `commit_layer_tiles_change`.
    /// Returns false when there is no usable selection or the layer is unsuitable.
    pub fn smart_fill_fill(&mut self, allow_ai: bool) -> bool {
        self.layer_stack.normalize_active_idx();
        if self.layer_stack.layers.is_empty() || !self.selection.active {
            return false;
        }
        let idx = self.layer_stack.active_idx;
        let (lw, lh, ox, oy) = {
            let l = &self.layer_stack.layers[idx];
            (l.width as usize, l.height as usize, l.offset.0, l.offset.1)
        };
        if lw == 0 || lh == 0 {
            return false;
        }

        let mut hole = vec![false; lw * lh];
        let mut any = false;
        for y in 0..lh {
            for x in 0..lw {
                let cx = ox + x as i32;
                let cy = oy + y as i32;
                if cx < 0 || cy < 0 || cx >= self.width as i32 || cy >= self.height as i32 {
                    continue;
                }
                if self.selection.sample(cx as u32, cy as u32) > 0.5 {
                    hole[y * lw + x] = true;
                    any = true;
                }
            }
        }
        if !any {
            return false;
        }
        self.smart_fill_fill_hole(hole, allow_ai, false)
    }

    /// Smart fill an explicit layer-local hole mask (`len` must equal the
    /// active layer's `width*height`; `true` = pixel to synthesise) on the active
    /// raster layer. Shared by the selection-based fill and the Repair Brush's
    /// content-aware mode. Undoable; false when the layer is unsuitable / no-op.
    /// `allow_ai` lets the explicit (selection) fill try the LaMa ONNX inpainter
    /// when its model is downloaded, falling back to PatchMatch otherwise. The
    /// interactive Repair Brush passes `false` so each stroke stays instant.
    /// `seamless` (Repair Brush) blends the synthesised region into its
    /// surroundings so there's no hard seam / wrong-colour stroke.
    pub fn smart_fill_fill_hole(
        &mut self,
        hole: Vec<bool>,
        allow_ai: bool,
        seamless: bool,
    ) -> bool {
        self.layer_stack.normalize_active_idx();
        if self.layer_stack.layers.is_empty() {
            return false;
        }
        let idx = self.layer_stack.active_idx;
        {
            let layer = &self.layer_stack.layers[idx];
            if (!layer.is_background && layer.locked) || !layer.is_raster() {
                return false;
            }
        }
        let layer_id = self.layer_stack.layers[idx].id;
        let (lw, lh) = {
            let l = &self.layer_stack.layers[idx];
            (l.width as usize, l.height as usize)
        };
        if lw == 0 || lh == 0 || hole.len() < lw * lh || !hole.iter().any(|&b| b) {
            return false;
        }

        let before_tiles = self.layer_stack.layers[idx].tiles.clone();
        let mut buf = before_tiles.flatten();
        if buf.len() < lw * lh * 4 {
            return false;
        }
        let used_ai = allow_ai && crate::core::lama::inpaint(&mut buf, lw, lh, &hole);
        if !used_ai {
            let ok = if seamless {
                crate::core::smart_fill::fill_seamless(&mut buf, lw, lh, &hole)
            } else {
                crate::core::smart_fill::fill(&mut buf, lw, lh, &hole)
            };
            if !ok {
                return false;
            }
        }
        let label = if used_ai {
            "Smart Fill (AI)"
        } else {
            "Smart Fill"
        };
        let after_tiles = crate::core::tile::TileMap::from_rgba(&buf, lw as u32, lh as u32);
        self.commit_layer_tiles_change(layer_id, before_tiles, after_tiles, label)
    }

    /// Apply the Patch tool's Source mode. The active selection stays at the
    /// DESTINATION; `(dx, dy)` is the drag delta, so the SOURCE is the destination
    /// shifted by `(dx, dy)`. Seamless-clone source→destination on the active raster
    /// layer (`smart_fill::seamless_clone_region`). Undoable. Returns false when
    /// unsuitable / no-op.
    pub fn patch_clone(&mut self, dx: i32, dy: i32) -> bool {
        self.layer_stack.normalize_active_idx();
        if self.layer_stack.layers.is_empty() || !self.selection.active {
            return false;
        }
        if dx == 0 && dy == 0 {
            return false;
        }
        let idx = self.layer_stack.active_idx;
        {
            let l = &self.layer_stack.layers[idx];
            if (!l.is_background && l.locked) || !l.is_raster() {
                return false;
            }
        }

        let layer_id = self.layer_stack.layers[idx].id;
        let (lw, lh, ox, oy) = {
            let l = &self.layer_stack.layers[idx];
            (l.width as usize, l.height as usize, l.offset.0, l.offset.1)
        };
        if lw == 0 || lh == 0 {
            return false;
        }

        let mut mask = vec![0f32; lw * lh];
        let mut any = false;
        for y in 0..lh {
            for x in 0..lw {
                let cx = ox + x as i32;
                let cy = oy + y as i32;
                if cx < 0 || cy < 0 || cx >= self.width as i32 || cy >= self.height as i32 {
                    continue;
                }
                let s = self.selection.sample(cx as u32, cy as u32);
                if s > 0.004 {
                    mask[y * lw + x] = s;
                    any = true;
                }
            }
        }
        if !any {
            return false;
        }

        let before_tiles = self.layer_stack.layers[idx].tiles.clone();
        let mut buf = before_tiles.flatten();
        if buf.len() < lw * lh * 4 {
            return false;
        }
        let changed =
            crate::core::smart_fill::seamless_clone_region(&mut buf, lw, lh, &mask, dx, dy);
        if !changed {
            return false;
        }
        let after_tiles = crate::core::tile::TileMap::from_rgba(&buf, lw as u32, lh as u32);
        self.commit_layer_tiles_change(layer_id, before_tiles, after_tiles, "Patch")
    }

    /// Live Patch source preview: blend the source (the active layer sampled from
    /// `base` at the selection shifted by `(dx, dy)`) into the selection region of
    /// the active layer. `base` is a clean pre-drag snapshot of the layer so
    /// overlapping source/dest can't feed back. The selection itself does NOT move —
    /// only the previewed pixels update. Non-undoable; marks the region dirty.
    pub fn patch_preview(&mut self, base: &crate::core::tile::TileMap, dx: i32, dy: i32) {
        self.patch_preview_region(base, Some((dx, dy)));
    }

    /// Remove a Patch live preview by restoring the selection region from `base`.
    pub fn patch_preview_clear(&mut self, base: &crate::core::tile::TileMap) {
        self.patch_preview_region(base, None);
    }

    pub(crate) fn patch_preview_region(
        &mut self,
        base: &crate::core::tile::TileMap,
        shift: Option<(i32, i32)>,
    ) {
        self.layer_stack.normalize_active_idx();
        if self.layer_stack.layers.is_empty() || !self.selection.active {
            return;
        }
        let idx = self.layer_stack.active_idx;
        {
            let l = &self.layer_stack.layers[idx];
            if (!l.is_background && l.locked) || !l.is_raster() {
                return;
            }
        }
        let (lw, lh, ox, oy) = {
            let l = &self.layer_stack.layers[idx];
            (l.width as i32, l.height as i32, l.offset.0, l.offset.1)
        };

        // Tight selection bbox (canvas coords) → layer-local, clamped. Cached, so it
        // only recomputes once for a stationary selection.
        self.selection.refresh_bbox();
        let (bx0, by0, bx1, by1) = self.selection.bounding_box_cached();
        let lx0 = (bx0 as i32 - ox).clamp(0, lw);
        let ly0 = (by0 as i32 - oy).clamp(0, lh);
        let lx1 = (bx1 as i32 - ox + 1).clamp(0, lw);
        let ly1 = (by1 as i32 - oy + 1).clamp(0, lh);
        if lx1 <= lx0 || ly1 <= ly0 {
            return;
        }

        // Selection is read through a raw pointer so the mutable tiles borrow below
        // doesn't conflict (disjoint Canvas fields) — same pattern the brush uses.
        let sel_ptr: *const crate::core::selection::Selection = &self.selection;
        let tiles = &mut self.layer_stack.layers[idx].tiles;

        for ly in ly0..ly1 {
            for lx in lx0..lx1 {
                let cx = (lx + ox) as u32;
                let cy = (ly + oy) as u32;
                let m = unsafe { (*sel_ptr).sample(cx, cy) };
                if m <= 0.004 {
                    continue;
                }
                let (br, bg, bb, ba) = base.get_pixel(lx as u32, ly as u32);
                let (sr, sg, sb, sa) = match shift {
                    Some((dx, dy)) => {
                        let srx = lx + dx;
                        let sry = ly + dy;
                        if srx >= 0 && sry >= 0 && srx < lw && sry < lh {
                            base.get_pixel(srx as u32, sry as u32)
                        } else {
                            (br, bg, bb, ba)
                        }
                    }
                    None => (br, bg, bb, ba),
                };
                let mix = |b: u8, s: u8| {
                    ((b as f32) * (1.0 - m) + (s as f32) * m)
                        .round()
                        .clamp(0.0, 255.0) as u8
                };
                tiles.set_pixel(
                    lx as u32,
                    ly as u32,
                    mix(br, sr),
                    mix(bg, sg),
                    mix(bb, sb),
                    mix(ba, sa),
                );
            }
        }

        let cx0 = (lx0 + ox).max(0) as u32;
        let cy0 = (ly0 + oy).max(0) as u32;
        let cx1 = ((lx1 + ox) as u32).min(self.width);
        let cy1 = ((ly1 + oy) as u32).min(self.height);
        self.mark_dirty(cx0, cy0, cx1, cy1);
    }

    pub fn apply_adjustment_to_active_layer(&mut self, adj: AdjustmentType) -> bool {
        self.layer_stack.normalize_active_idx();
        if self.layer_stack.layers.is_empty() {
            return false;
        }

        let idx = self.layer_stack.active_idx;
        let layer = &self.layer_stack.layers[idx];
        if (!layer.is_background && layer.locked) || !layer.is_raster() {
            return false;
        }
        // A CMYK document only accepts ink-native adjustments; RGB-space math
        // would edit the mirror and orphan the ink ground truth.
        if self.is_cmyk() && !adj.is_ink_native() {
            return false;
        }

        let label = adj.name().to_string();
        let mut cmd = crate::core::command::LayerStructureCommand::capture_before(
            &label,
            &self.layer_stack,
            self.width,
            self.height,
        );

        let (layer_w, layer_h, ox, oy) = {
            let layer = &self.layer_stack.layers[idx];
            (layer.width, layer.height, layer.offset.0, layer.offset.1)
        };
        let source_tiles = self.layer_stack.layers[idx].tiles.clone();
        self.layer_stack.layers[idx].tiles = if self.is_cmyk() {
            match self.build_ink_adjusted_layer_tiles(&source_tiles, ox, oy, &adj) {
                Some(t) => t,
                None => return false,
            }
        } else {
            self.build_adjusted_layer_tiles(&source_tiles, layer_w, layer_h, ox, oy, &adj)
        };
        cmd.capture_after(&self.layer_stack, self.width, self.height);
        self.record_as(Box::new(cmd), ChangeKind::LayerPixels);
        self.layer_revision += 1;
        self.dirty.expand_full(self.width, self.height);
        self.flatten_full();
        true
    }

    /// Mark a dirty region covering a layer's canvas-space footprint.
    ///
    /// Tiles are always in layer-local space (0,0 = layer's top-left).
    /// All visual movement must go through `layer.offset`, not `TileMap::translate()`.
    /// This helper computes the dirty rect from (ox, oy, lw, lh) clamped to canvas bounds.
    ///
    /// Call BEFORE and AFTER changing the offset so the union of old + new positions is
    /// recomposited (avoids a ghost at the old position or missing pixels at the new one).
    pub fn mark_dirty_layer_bounds(&mut self, ox: i32, oy: i32, lw: u32, lh: u32) {
        let x0 = ox.max(0) as u32;
        let y0 = oy.max(0) as u32;
        let x1 = (ox + lw as i32).min(self.width as i32).max(0) as u32;
        let y1 = (oy + lh as i32).min(self.height as i32).max(0) as u32;
        if x1 > x0 && y1 > y0 {
            self.mark_dirty(x0, y0, x1, y1);
        }
    }

    pub fn fill_solid_color(&mut self, color: [u8; 4]) {
        if self.active_layer().locked && !self.active_layer().is_background {
            return;
        }

        let lock_alpha = self.active_layer().lock_alpha;
        let ox = self.active_layer().offset.0;
        let oy = self.active_layer().offset.1;
        let layer_w = self.active_layer().width as i32;
        let layer_h = self.active_layer().height as i32;

        let (sel_x0, sel_y0, sel_x1, sel_y1): (i32, i32, i32, i32) = if self.selection.active {
            let bbox = self.selection.bounding_box();
            (bbox.0 as i32, bbox.1 as i32, bbox.2 as i32, bbox.3 as i32)
        } else {
            (0, 0, self.width as i32, self.height as i32)
        };

        let fill_x0 = sel_x0.max(ox).max(0) as u32;
        let fill_y0 = sel_y0.max(oy).max(0) as u32;
        let fill_x1 = sel_x1.min(ox + layer_w).min(self.width as i32).max(0) as u32;
        let fill_y1 = sel_y1.min(oy + layer_h).min(self.height as i32).max(0) as u32;

        if fill_x1 <= fill_x0 || fill_y1 <= fill_y0 {
            return;
        }
        let fill_w = fill_x1 - fill_x0;
        let fill_h = fill_y1 - fill_y0;

        let lx0 = (fill_x0 as i32 - ox) as u32;
        let ly0 = (fill_y0 as i32 - oy) as u32;

        if !self.selection.active && color[3] == 255 && !lock_alpha {
            self.begin_stroke("Fill");

            let [r, g, b, a] = color;
            let ts = crate::core::tile::TILE_SIZE;
            let lx1 = lx0 + fill_w;
            let ly1 = ly0 + fill_h;
            let tx0 = lx0 / ts;
            let ty0 = ly0 / ts;
            let tx1 = (lx1 - 1) / ts;
            let ty1 = (ly1 - 1) / ts;

            if let Some(tiles) = self.active_layer_mut().get_paint_tiles_mut() {
                for ty in ty0..=ty1 {
                    for tx in tx0..=tx1 {
                        let pos = crate::core::tile::TilePos {
                            x: tx as i32,
                            y: ty as i32,
                        };
                        let tile = tiles.get_tile_mut(pos);

                        let tile_lx0 = (tx * ts).max(lx0);
                        let tile_lx1 = ((tx + 1) * ts).min(lx1);
                        let tile_ly0 = (ty * ts).max(ly0);
                        let tile_ly1 = ((ty + 1) * ts).min(ly1);

                        let px0 = tile_lx0 - tx * ts;
                        let py0 = tile_ly0 - ty * ts;
                        let pw = tile_lx1 - tile_lx0;
                        let ph = tile_ly1 - tile_ly0;

                        if pw == ts && ph == ts {
                            for chunk in tile.pixels.chunks_exact_mut(4) {
                                chunk[0] = r;
                                chunk[1] = g;
                                chunk[2] = b;
                                chunk[3] = a;
                            }
                        } else {
                            for row in 0..ph {
                                let base = (((py0 + row) * ts + px0) * 4) as usize;
                                for col in 0..pw {
                                    let i = base + (col * 4) as usize;
                                    tile.pixels[i] = r;
                                    tile.pixels[i + 1] = g;
                                    tile.pixels[i + 2] = b;
                                    tile.pixels[i + 3] = a;
                                }
                            }
                        }
                    }
                }
            }

            self.end_stroke();
            self.dirty.expand_full(self.width, self.height);
            return;
        }

        let Some(region_len) = Self::checked_rgba_len(fill_w, fill_h) else {
            return;
        };
        self.begin_stroke("Fill");

        let mut region = self
            .active_layer()
            .get_paint_tiles()
            .map(|tiles| tiles.extract_region(lx0, ly0, fill_w, fill_h))
            .unwrap_or_else(|| vec![0u8; region_len]);

        for y in 0..fill_h {
            for x in 0..fill_w {
                let cx = fill_x0 + x;
                let cy = fill_y0 + y;

                let sel_a = if self.selection.active {
                    self.selection.sample(cx, cy)
                } else {
                    1.0
                };

                if sel_a < 0.001 {
                    continue;
                }

                let i = ((y * fill_w + x) * 4) as usize;

                let dst_a = region[i + 3] as f32 / 255.0;

                if lock_alpha && dst_a < 0.001 {
                    continue;
                }

                let src_r = color[0] as f32 / 255.0;
                let src_g = color[1] as f32 / 255.0;
                let src_b = color[2] as f32 / 255.0;
                let src_a = (color[3] as f32 / 255.0) * sel_a;

                if lock_alpha {
                    let w = src_a.min(dst_a) / dst_a;
                    let dst_r = region[i] as f32 / 255.0;
                    let dst_g = region[i + 1] as f32 / 255.0;
                    let dst_b = region[i + 2] as f32 / 255.0;
                    region[i] = ((src_r * w + dst_r * (1.0 - w)) * 255.0).round() as u8;
                    region[i + 1] = ((src_g * w + dst_g * (1.0 - w)) * 255.0).round() as u8;
                    region[i + 2] = ((src_b * w + dst_b * (1.0 - w)) * 255.0).round() as u8;
                } else {
                    let out_a = src_a + dst_a * (1.0 - src_a);
                    if out_a > 0.001 {
                        let dst_r = region[i] as f32 / 255.0;
                        let dst_g = region[i + 1] as f32 / 255.0;
                        let dst_b = region[i + 2] as f32 / 255.0;
                        region[i] = ((src_r * src_a + dst_r * dst_a * (1.0 - src_a)) / out_a
                            * 255.0)
                            .round() as u8;
                        region[i + 1] = ((src_g * src_a + dst_g * dst_a * (1.0 - src_a)) / out_a
                            * 255.0)
                            .round() as u8;
                        region[i + 2] = ((src_b * src_a + dst_b * dst_a * (1.0 - src_a)) / out_a
                            * 255.0)
                            .round() as u8;
                        region[i + 3] = (out_a * 255.0).round() as u8;
                        if region[i + 3] == 0 {
                            region[i] = 0;
                            region[i + 1] = 0;
                            region[i + 2] = 0;
                        }
                    } else {
                        region[i] = 0;
                        region[i + 1] = 0;
                        region[i + 2] = 0;
                        region[i + 3] = 0;
                    }
                }
            }
        }

        self.active_layer_mut()
            .update_tiles_region(lx0, ly0, fill_w, fill_h, &region);
        self.end_stroke();
        self.dirty.expand_full(self.width, self.height);
    }

    /// Composite a solid `color` (straight-alpha RGBA8) into the active raster
    /// layer through a per-pixel coverage buffer `cov` (`bw*bh`, values 0..=1)
    /// anchored at canvas-space box (`bx0`,`by0`). One undoable stroke named
    /// `label`. Honors layer offset / lock / lock_alpha and ignores the active
    /// selection — the coverage buffer *is* the mask. Shared by `fill_polygon`
    /// and `stroke_polyline`.
    pub(crate) fn composite_solid_coverage(
        &mut self,
        label: &str,
        color: [u8; 4],
        bx0: u32,
        by0: u32,
        bw: u32,
        bh: u32,
        cov: &[f32],
    ) {
        if self.active_layer().locked && !self.active_layer().is_background {
            return;
        }
        if bw == 0 || bh == 0 || cov.len() < (bw * bh) as usize {
            return;
        }

        let lock_alpha = self.active_layer().lock_alpha;
        let ox = self.active_layer().offset.0;
        let oy = self.active_layer().offset.1;
        let layer_w = self.active_layer().width as i32;
        let layer_h = self.active_layer().height as i32;

        // Clamp the box to the layer (and canvas) bounds in canvas space.
        let fx0 = (bx0 as i32).max(ox).max(0);
        let fy0 = (by0 as i32).max(oy).max(0);
        let fx1 = ((bx0 + bw) as i32).min(ox + layer_w).min(self.width as i32);
        let fy1 = ((by0 + bh) as i32)
            .min(oy + layer_h)
            .min(self.height as i32);
        if fx1 <= fx0 || fy1 <= fy0 {
            return;
        }
        let fw = (fx1 - fx0) as u32;
        let fh = (fy1 - fy0) as u32;
        let lx0 = (fx0 - ox) as u32;
        let ly0 = (fy0 - oy) as u32;
        // Offset from the coverage box origin to the (clamped) region origin.
        let cov_dx = (fx0 - bx0 as i32) as u32;
        let cov_dy = (fy0 - by0 as i32) as u32;

        let Some(region_len) = Self::checked_rgba_len(fw, fh) else {
            return;
        };
        self.begin_stroke(label);

        let mut region = self
            .active_layer()
            .get_paint_tiles()
            .map(|tiles| tiles.extract_region(lx0, ly0, fw, fh))
            .unwrap_or_else(|| vec![0u8; region_len]);

        let src_r = color[0] as f32 / 255.0;
        let src_g = color[1] as f32 / 255.0;
        let src_b = color[2] as f32 / 255.0;
        let base_a = color[3] as f32 / 255.0;

        for y in 0..fh {
            for x in 0..fw {
                let cov_a = cov[((cov_dy + y) * bw + cov_dx + x) as usize].clamp(0.0, 1.0);
                if cov_a < 0.001 {
                    continue;
                }
                let i = ((y * fw + x) * 4) as usize;
                let dst_a = region[i + 3] as f32 / 255.0;
                if lock_alpha && dst_a < 0.001 {
                    continue;
                }
                let src_a = base_a * cov_a;

                if lock_alpha {
                    let w = src_a.min(dst_a) / dst_a;
                    let dst_r = region[i] as f32 / 255.0;
                    let dst_g = region[i + 1] as f32 / 255.0;
                    let dst_b = region[i + 2] as f32 / 255.0;
                    region[i] = ((src_r * w + dst_r * (1.0 - w)) * 255.0).round() as u8;
                    region[i + 1] = ((src_g * w + dst_g * (1.0 - w)) * 255.0).round() as u8;
                    region[i + 2] = ((src_b * w + dst_b * (1.0 - w)) * 255.0).round() as u8;
                } else {
                    let out_a = src_a + dst_a * (1.0 - src_a);
                    if out_a > 0.001 {
                        let dst_r = region[i] as f32 / 255.0;
                        let dst_g = region[i + 1] as f32 / 255.0;
                        let dst_b = region[i + 2] as f32 / 255.0;
                        region[i] = ((src_r * src_a + dst_r * dst_a * (1.0 - src_a)) / out_a
                            * 255.0)
                            .round() as u8;
                        region[i + 1] = ((src_g * src_a + dst_g * dst_a * (1.0 - src_a)) / out_a
                            * 255.0)
                            .round() as u8;
                        region[i + 2] = ((src_b * src_a + dst_b * dst_a * (1.0 - src_a)) / out_a
                            * 255.0)
                            .round() as u8;
                        region[i + 3] = (out_a * 255.0).round() as u8;
                    }
                }
            }
        }

        self.active_layer_mut()
            .update_tiles_region(lx0, ly0, fw, fh, &region);
        self.end_stroke();
        self.dirty.expand_full(self.width, self.height);
    }

    /// Stroke the active selection edge onto the active raster layer (Edit ▸
    /// Stroke). The band is the difference between a grown and a shrunk copy of
    /// the selection mask, sized/placed by `params.location`; it is composited as
    /// a coverage buffer so the selection itself does NOT clip the result (the
    /// band *is* the mask). One undoable "Stroke" command. No-op without an
    /// active selection or with width 0.
    pub fn stroke_selection(&mut self, params: StrokeParams) {
        if !self.selection.active || params.width == 0 {
            return;
        }

        // How far to grow (outward edge) and shrink (inward edge) from the
        // original boundary so the band straddles it per the chosen location.
        let (grow_px, shrink_px) = match params.location {
            StrokeLocation::Inside => (0, params.width),
            StrokeLocation::Outside => (params.width, 0),
            StrokeLocation::Center => {
                let out = params.width / 2;
                (out, params.width - out)
            }
        };

        let mut outer = self.selection.clone();
        if grow_px > 0 {
            outer.grow(grow_px);
        }
        let mut inner = self.selection.clone();
        if shrink_px > 0 {
            inner.shrink(shrink_px);
        }

        // Band box = grown selection's bbox, clamped to the canvas.
        let (bx0f, by0f, bx1f, by1f) = outer.bounding_box();
        let bx0 = bx0f.clamp(0.0, self.width as f32) as u32;
        let by0 = by0f.clamp(0.0, self.height as f32) as u32;
        let bx1 = bx1f.clamp(0.0, self.width as f32) as u32;
        let by1 = by1f.clamp(0.0, self.height as f32) as u32;
        if bx1 <= bx0 || by1 <= by0 {
            return;
        }
        let bw = bx1 - bx0;
        let bh = by1 - by0;

        if Self::checked_rgba_len(bw, bh).is_none() {
            return;
        }
        let mut cov = vec![0f32; (bw * bh) as usize];
        for y in 0..bh {
            for x in 0..bw {
                let cx = bx0 + x;
                let cy = by0 + y;
                let band = (outer.sample(cx, cy) - inner.sample(cx, cy)).clamp(0.0, 1.0);
                cov[(y * bw + x) as usize] = band;
            }
        }

        let a = (params.color[3] as f32 * params.opacity.clamp(0.0, 1.0)).round() as u8;
        let color = [params.color[0], params.color[1], params.color[2], a];
        self.composite_solid_coverage("Stroke", color, bx0, by0, bw, bh, &cov);
    }

    /// Fill the closed polygon `points` (canvas space) on the active layer with
    /// `color`, optionally `feather`ed. One undoable "Fill Path" stroke; does not
    /// touch the active selection (Pen tool "Path → Fill"). No-op for < 3 points.
    pub fn fill_polygon(&mut self, points: &[(f32, f32)], color: [u8; 4], feather: f32) {
        if points.len() < 3 {
            return;
        }
        let mut mask = self.selection.build_polygon_mask(points);
        if feather > 0.0 {
            crate::core::selection::blur_mask(
                &mut mask,
                self.width as usize,
                self.height as usize,
                feather,
            );
        }

        let pad = feather.max(0.0).ceil() as i32 + 1;
        let (mut minx, mut miny, mut maxx, mut maxy) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &(x, y) in points {
            minx = minx.min(x);
            miny = miny.min(y);
            maxx = maxx.max(x);
            maxy = maxy.max(y);
        }
        let bx0 = (minx.floor() as i32 - pad).clamp(0, self.width as i32) as u32;
        let by0 = (miny.floor() as i32 - pad).clamp(0, self.height as i32) as u32;
        let bx1 = (maxx.ceil() as i32 + pad).clamp(0, self.width as i32) as u32;
        let by1 = (maxy.ceil() as i32 + pad).clamp(0, self.height as i32) as u32;
        if bx1 <= bx0 || by1 <= by0 {
            return;
        }
        let bw = bx1 - bx0;
        let bh = by1 - by0;

        let mut cov = vec![0.0f32; (bw * bh) as usize];
        for y in 0..bh {
            for x in 0..bw {
                let cx = bx0 + x;
                let cy = by0 + y;
                cov[(y * bw + x) as usize] = mask[(cy * self.width + cx) as usize] as f32 / 255.0;
            }
        }
        self.composite_solid_coverage("Fill Path", color, bx0, by0, bw, bh, &cov);
    }

    /// Stroke the polyline `points` (canvas space) on the active layer with `color`
    /// at `width` px, with ~1px anti-aliased edges. `closed` joins the last point
    /// back to the first. One undoable "Stroke Path" stroke (Pen tool "Path →
    /// Stroke"). No-op for < 2 points or non-positive width.
    pub fn stroke_polyline(
        &mut self,
        points: &[(f32, f32)],
        closed: bool,
        color: [u8; 4],
        width: f32,
    ) {
        if points.len() < 2 || width <= 0.0 {
            return;
        }
        let half = width * 0.5;
        let pad = half.ceil() as i32 + 1;
        let (mut minx, mut miny, mut maxx, mut maxy) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &(x, y) in points {
            minx = minx.min(x);
            miny = miny.min(y);
            maxx = maxx.max(x);
            maxy = maxy.max(y);
        }
        let bx0 = (minx.floor() as i32 - pad).clamp(0, self.width as i32) as u32;
        let by0 = (miny.floor() as i32 - pad).clamp(0, self.height as i32) as u32;
        let bx1 = (maxx.ceil() as i32 + pad).clamp(0, self.width as i32) as u32;
        let by1 = (maxy.ceil() as i32 + pad).clamp(0, self.height as i32) as u32;
        if bx1 <= bx0 || by1 <= by0 {
            return;
        }
        let bw = bx1 - bx0;
        let bh = by1 - by0;

        let seg_count = if closed {
            points.len()
        } else {
            points.len() - 1
        };
        let mut cov = vec![0.0f32; (bw * bh) as usize];
        for y in 0..bh {
            for x in 0..bw {
                let px = bx0 as f32 + x as f32 + 0.5;
                let py = by0 as f32 + y as f32 + 0.5;
                let mut dmin = f32::MAX;
                for s in 0..seg_count {
                    let a = points[s];
                    let b = points[(s + 1) % points.len()];
                    dmin = dmin.min(dist_point_segment(px, py, a, b));
                }
                let c = (half + 0.5 - dmin).clamp(0.0, 1.0);
                if c > 0.0 {
                    cov[(y * bw + x) as usize] = c;
                }
            }
        }
        self.composite_solid_coverage("Stroke Path", color, bx0, by0, bw, bh, &cov);
    }

    /// Repair Brush (content-aware): heal blemishes inside the soft brush
    /// `mask` (layer-local coverage, len == active layer `w*h`).
    pub fn heal_skin(&mut self, mask: Vec<f32>) -> bool {
        self.layer_stack.normalize_active_idx();
        if self.layer_stack.layers.is_empty() {
            return false;
        }
        let idx = self.layer_stack.active_idx;
        {
            let layer = &self.layer_stack.layers[idx];
            if (!layer.is_background && layer.locked) || !layer.is_raster() {
                return false;
            }
        }
        let layer_id = self.layer_stack.layers[idx].id;
        let (lw, lh) = {
            let l = &self.layer_stack.layers[idx];
            (l.width as usize, l.height as usize)
        };
        if lw == 0 || lh == 0 || mask.len() < lw * lh {
            return false;
        }

        let (mut x0, mut y0, mut x1, mut y1) = (lw, lh, 0usize, 0usize);
        let mut any = false;
        for y in 0..lh {
            for x in 0..lw {
                if mask[y * lw + x] > 0.004 {
                    any = true;
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
        }
        if !any {
            return false;
        }

        let before_tiles = self.layer_stack.layers[idx].tiles.clone();
        let mut buf = before_tiles.flatten();
        if buf.len() < lw * lh * 4 {
            return false;
        }

        let changed = crate::core::smart_fill::fill_soft(&mut buf, lw, lh, &mask);

        if !changed {
            return false;
        }

        let after_tiles = crate::core::tile::TileMap::from_rgba(&buf, lw as u32, lh as u32);
        self.commit_layer_tiles_change(layer_id, before_tiles, after_tiles, "Smart Heal")
    }
}

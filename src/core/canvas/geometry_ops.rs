//! Canvas geometry: crop (straight/rotated/transformed/perspective), flips,
//! 90-degree rotates, canvas/image resize and the tile resampling behind them.
//! Every size change goes through the app's apply_canvas_event path.

// Main document model — Canvas, History, metadata.
//

use super::*;
use crate::core::gateway::ChangeKind;

impl Canvas {
    pub(crate) fn add_crop_background_if_missing(
        &mut self,
        width: u32,
        height: u32,
        color: [u8; 4],
    ) {
        if self
            .layer_stack
            .layers
            .iter()
            .any(|layer| layer.is_background)
        {
            return;
        }

        let mut id = self.layer_stack.next_id();
        while self.layer_stack.layers.iter().any(|layer| layer.id == id) {
            id = id.saturating_add(1);
        }
        self.layer_stack.set_next_id(id.saturating_add(1));

        let [r, g, b, a] = color;
        let mut layer = crate::core::layer::Layer::new(id, "Background", width, height);
        layer.tiles = crate::core::tile::TileMap::new_solid(width, height, r, g, b, a);
        layer.is_background = true;
        layer.locked = true;
        self.layer_stack.layers.insert(0, layer);
        self.layer_stack.active_idx = self.layer_stack.active_idx.saturating_add(1);
    }

    pub fn crop(&mut self, x: i32, y: i32, w: u32, h: u32, delete_cropped: bool) -> bool {
        self.crop_impl(x, y, w, h, delete_cropped, None)
    }

    pub fn crop_with_background(
        &mut self,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        delete_cropped: bool,
        background: [u8; 4],
    ) -> bool {
        self.crop_impl(x, y, w, h, delete_cropped, Some(background))
    }

    pub(crate) fn crop_impl(
        &mut self,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        delete_cropped: bool,
        background: Option<[u8; 4]>,
    ) -> bool {
        let max = MAX_DIMENSION;
        if w == 0 || h == 0 || w > max || h > max {
            return false;
        }
        let expands_canvas = x < 0
            || y < 0
            || x as i64 + w as i64 > self.width as i64
            || y as i64 + h as i64 > self.height as i64;

        let mut cmd = crate::core::command::LayerStructureCommand::capture_before(
            "Crop",
            &self.layer_stack,
            self.width,
            self.height,
        );

        for layer in &mut self.layer_stack.layers {
            let ox = layer.offset.0;
            let oy = layer.offset.1;

            if delete_cropped {
                let layer_fill = background.filter(|_| layer.is_background);
                let (new_tiles, new_w, new_h) = Self::crop_tilemap(
                    &layer.tiles,
                    layer.width,
                    layer.height,
                    x,
                    y,
                    w,
                    h,
                    ox,
                    oy,
                    layer_fill,
                );
                layer.tiles = new_tiles;
                layer.width = new_w;
                layer.height = new_h;
                layer.offset = (0, 0);

                if let Some(mask) = &mut layer.mask {
                    let (mt, mw, mh) = Self::crop_tilemap(
                        &mask.tiles,
                        mask.width,
                        mask.height,
                        x,
                        y,
                        w,
                        h,
                        ox,
                        oy,
                        background.map(|_| [255, 255, 255, 255]),
                    );
                    mask.tiles = mt;
                    mask.width = mw;
                    mask.height = mh;
                }
            } else {
                layer.offset = (ox - x, oy - y);
            }
        }

        if expands_canvas {
            if let Some(color) = background {
                self.add_crop_background_if_missing(w, h, color);
            }
        }

        self.width = w;
        self.height = h;
        self.selection.resize(w, h);
        cmd.capture_after(&self.layer_stack, self.width, self.height);
        self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
        self.flatten_full();
        true
    }

    /// Rotated crop: extract a `(w × h)` rectangle centered at `(cx, cy)` that
    /// is tilted by `angle_rad` radians (CW positive, same sign as `CropTool::rotation`).
    ///
    /// Each output pixel `(u, v)` is filled by back-projecting the un-rotated offset
    /// `(u - w/2, v - h/2)` through a CW rotation by `-angle_rad` to obtain the
    /// source canvas position, then sampling with bilinear interpolation.
    ///
    /// A `LayerStructureCommand` is pushed for undo.
    pub fn crop_rotated(
        &mut self,
        cx: f32,
        cy: f32,
        src_w: f32,
        src_h: f32,
        out_w: u32,
        out_h: u32,
        angle_rad: f32,
        _delete_cropped: bool,
    ) -> bool {
        use crate::core::command::LayerStructureCommand;

        if out_w == 0 || out_h == 0 {
            return false;
        }
        let max = MAX_DIMENSION;
        if out_w > max || out_h > max {
            return false;
        }
        let mut cmd = LayerStructureCommand::capture_before(
            "Crop/Resample",
            &self.layer_stack,
            self.width,
            self.height,
        );

        let scale_x = src_w / out_w as f32;
        let scale_y = src_h / out_h as f32;

        let hw = out_w as f32 * 0.5;
        let hh = out_h as f32 * 0.5;

        let cos_rot = angle_rad.cos();
        let sin_rot = angle_rad.sin();

        for layer in &mut self.layer_stack.layers {
            let ox = layer.offset.0 as f32;
            let oy = layer.offset.1 as f32;
            // Back-project each output-pixel centre through a CW rotation by
            // `-angle_rad` to the source; tile-native chunked resample so rotated
            // Crop works under Viewport Streaming.
            let map = |u: f32, v: f32| -> (f32, f32) {
                let lx = (u - hw) * scale_x;
                let ly = (v - hh) * scale_y;
                let src_cx = lx * cos_rot - ly * sin_rot + cx;
                let src_cy = lx * sin_rot + ly * cos_rot + cy;
                (src_cx - ox, src_cy - oy)
            };
            layer.tiles = Self::resample_into_tiles(&layer.tiles, out_w, out_h, &map, None);
            layer.width = out_w;
            layer.height = out_h;
            layer.offset = (0, 0);

            if let Some(mask) = &mut layer.mask {
                mask.tiles = Self::resample_into_tiles(&mask.tiles, out_w, out_h, &map, None);
                mask.width = out_w;
                mask.height = out_h;
            }
        }

        self.width = out_w;
        self.height = out_h;
        self.selection.resize(out_w, out_h);
        cmd.capture_after(&self.layer_stack, self.width, self.height);
        self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
        self.flatten_full();
        true
    }

    /// Crop a fixed viewport while the image underneath has been preview-transformed.
    /// This is the modern Crop-tool model: the crop box is an axis-aligned viewport,
    /// while the source image can be panned/rotated behind it before commit.
    pub fn crop_transformed(
        &mut self,
        cx: f32,
        cy: f32,
        viewport_w: f32,
        viewport_h: f32,
        out_w: u32,
        out_h: u32,
        image_tx: f32,
        image_ty: f32,
        angle_rad: f32,
        delete_cropped: bool,
    ) -> bool {
        self.crop_transformed_impl(
            cx,
            cy,
            viewport_w,
            viewport_h,
            out_w,
            out_h,
            image_tx,
            image_ty,
            angle_rad,
            delete_cropped,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn crop_transformed_with_background(
        &mut self,
        cx: f32,
        cy: f32,
        viewport_w: f32,
        viewport_h: f32,
        out_w: u32,
        out_h: u32,
        image_tx: f32,
        image_ty: f32,
        angle_rad: f32,
        delete_cropped: bool,
        background: [u8; 4],
    ) -> bool {
        self.crop_transformed_impl(
            cx,
            cy,
            viewport_w,
            viewport_h,
            out_w,
            out_h,
            image_tx,
            image_ty,
            angle_rad,
            delete_cropped,
            Some(background),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn crop_transformed_impl(
        &mut self,
        cx: f32,
        cy: f32,
        viewport_w: f32,
        viewport_h: f32,
        out_w: u32,
        out_h: u32,
        image_tx: f32,
        image_ty: f32,
        angle_rad: f32,
        _delete_cropped: bool,
        background: Option<[u8; 4]>,
    ) -> bool {
        use crate::core::command::LayerStructureCommand;

        if out_w == 0 || out_h == 0 {
            return false;
        }
        let max = MAX_DIMENSION;
        if out_w > max || out_h > max {
            return false;
        }
        let extends_canvas = cx - viewport_w * 0.5 < 0.0
            || cy - viewport_h * 0.5 < 0.0
            || cx + viewport_w * 0.5 > self.width as f32
            || cy + viewport_h * 0.5 > self.height as f32
            || image_tx.abs() > 0.001
            || image_ty.abs() > 0.001
            || angle_rad.abs() > 0.001;

        let mut cmd = LayerStructureCommand::capture_before(
            "Crop/Resample",
            &self.layer_stack,
            self.width,
            self.height,
        );

        let scale_x = viewport_w / out_w as f32;
        let scale_y = viewport_h / out_h as f32;
        let hw = out_w as f32 * 0.5;
        let hh = out_h as f32 * 0.5;
        let cos_inv = angle_rad.cos();
        let sin_inv = angle_rad.sin();
        let pivot_tx = cx + image_tx;
        let pivot_ty = cy + image_ty;

        for layer in &mut self.layer_stack.layers {
            let ox = layer.offset.0 as f32;
            let oy = layer.offset.1 as f32;
            let map = |u: f32, v: f32| -> (f32, f32) {
                let dest_x = (u - hw) * scale_x + cx;
                let dest_y = (v - hh) * scale_y + cy;
                let dx = dest_x - pivot_tx;
                let dy = dest_y - pivot_ty;
                let src_cx = dx * cos_inv + dy * sin_inv + cx;
                let src_cy = -dx * sin_inv + dy * cos_inv + cy;
                (src_cx - ox, src_cy - oy)
            };
            let layer_fill = background.filter(|_| layer.is_background);
            layer.tiles = Self::resample_into_tiles(&layer.tiles, out_w, out_h, &map, layer_fill);
            layer.width = out_w;
            layer.height = out_h;
            layer.offset = (0, 0);

            if let Some(mask) = &mut layer.mask {
                mask.tiles = Self::resample_into_tiles(
                    &mask.tiles,
                    out_w,
                    out_h,
                    &map,
                    background.map(|_| [255, 255, 255, 255]),
                );
                mask.width = out_w;
                mask.height = out_h;
            }
        }

        if extends_canvas {
            if let Some(color) = background {
                self.add_crop_background_if_missing(out_w, out_h, color);
            }
        }

        self.width = out_w;
        self.height = out_h;
        self.selection.resize(out_w, out_h);
        cmd.capture_after(&self.layer_stack, self.width, self.height);
        self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
        self.flatten_full();
        true
    }

    /// Rectify a dragged quadrilateral to an axis-aligned `out_w × out_h` image
    /// (Perspective Crop). `quad` is `[top-left, top-right, bottom-right,
    /// bottom-left]` in canvas space. Each output pixel is mapped through the
    /// unit-square→quad homography back to the source and bilinear-sampled, so
    /// perspective is corrected. Mirrors `crop_rotated`'s per-layer resample.
    pub fn crop_perspective(
        &mut self,
        quad: [(f32, f32); 4],
        out_w: u32,
        out_h: u32,
        _delete_cropped: bool,
    ) -> bool {
        use crate::core::command::LayerStructureCommand;
        use crate::core::geometry::{Homography, Point};

        if out_w == 0 || out_h == 0 {
            return false;
        }
        let max = MAX_DIMENSION;
        if out_w > max || out_h > max {
            return false;
        }
        let h = match Homography::square_to_quad([
            Point::new(quad[0].0, quad[0].1),
            Point::new(quad[1].0, quad[1].1),
            Point::new(quad[2].0, quad[2].1),
            Point::new(quad[3].0, quad[3].1),
        ]) {
            Some(h) => h,
            None => return false,
        };

        let mut cmd = LayerStructureCommand::capture_before(
            "Perspective Crop",
            &self.layer_stack,
            self.width,
            self.height,
        );

        let inv_w = 1.0 / out_w as f32;
        let inv_h = 1.0 / out_h as f32;

        for layer in &mut self.layer_stack.layers {
            let ox = layer.offset.0 as f32;
            let oy = layer.offset.1 as f32;
            // Map each output-pixel centre through the unit-square->quad homography
            // back to the source; tile-native chunked resample so perspective Crop
            // works under Viewport Streaming.
            let map = |u: f32, v: f32| -> (f32, f32) {
                let src = h.apply(u * inv_w, v * inv_h);
                (src.x - ox, src.y - oy)
            };
            layer.tiles = Self::resample_into_tiles(&layer.tiles, out_w, out_h, &map, None);
            layer.width = out_w;
            layer.height = out_h;
            layer.offset = (0, 0);

            if let Some(mask) = &mut layer.mask {
                mask.tiles = Self::resample_into_tiles(&mask.tiles, out_w, out_h, &map, None);
                mask.width = out_w;
                mask.height = out_h;
            }
        }

        self.width = out_w;
        self.height = out_h;
        self.selection.resize(out_w, out_h);
        cmd.capture_after(&self.layer_stack, self.width, self.height);
        self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
        self.flatten_full();
        true
    }

    /// Extract a (out_w × out_h) region from `src_tiles` into a new TileMap.
    /// Returns `(new_tilemap, out_w, out_h)`.
    /// The crop rectangle `(crop_x, crop_y, out_w, out_h)` is in canvas space.
    /// `offset_x/y` is the layer's canvas offset (tile_pos = canvas_pos - offset).
    pub(crate) fn crop_tilemap(
        src_tiles: &crate::core::tile::TileMap,
        src_w: u32,
        src_h: u32,
        crop_x: i32,
        crop_y: i32,
        out_w: u32,
        out_h: u32,
        offset_x: i32,
        offset_y: i32,
        fill: Option<[u8; 4]>,
    ) -> (crate::core::tile::TileMap, u32, u32) {
        let local_x = crop_x - offset_x;
        let local_y = crop_y - offset_y;

        let tile_x0 = local_x.clamp(0, src_w as i32) as u32;
        let tile_y0 = local_y.clamp(0, src_h as i32) as u32;
        let tile_x1 = (local_x + out_w as i32).clamp(0, src_w as i32) as u32;
        let tile_y1 = (local_y + out_h as i32).clamp(0, src_h as i32) as u32;

        // Tile-native (no canvas-sized buffer): copy the clamped source rect into a
        // fresh map at its destination offset. Works on Viewport-Streaming canvases.
        let mut new_tiles = match fill {
            Some([r, g, b, a]) => crate::core::tile::TileMap::new_solid(out_w, out_h, r, g, b, a),
            None => crate::core::tile::TileMap::new(out_w, out_h),
        };
        if tile_x1 > tile_x0 && tile_y1 > tile_y0 {
            let copy_w = tile_x1 - tile_x0;
            let copy_h = tile_y1 - tile_y0;
            let dest_x = (tile_x0 as i32 - local_x) as u32;
            let dest_y = (tile_y0 as i32 - local_y) as u32;
            new_tiles.blit_region_from(src_tiles, tile_x0, tile_y0, dest_x, dest_y, copy_w, copy_h);
        }
        new_tiles.bump_all_revisions();
        (new_tiles, out_w, out_h)
    }

    /// Resample `src` into a fresh tile-native `out_w × out_h` map in 256-px chunks
    /// (no canvas-sized buffer, so resampling crops work under Viewport Streaming).
    /// `map(u, v)` takes an output pixel CENTRE and returns the source-space
    /// coordinate to bilinear-sample from `src`.
    pub(crate) fn resample_into_tiles(
        src: &crate::core::tile::TileMap,
        out_w: u32,
        out_h: u32,
        map: impl Fn(f32, f32) -> (f32, f32) + Sync,
        background: Option<[u8; 4]>,
    ) -> crate::core::tile::TileMap {
        use rayon::prelude::*;
        let mut new_tiles = crate::core::tile::TileMap::new(out_w, out_h);
        let chunk = 256u32;
        let mut by = 0;
        while by < out_h {
            let ch = chunk.min(out_h - by);
            let mut bx = 0;
            while bx < out_w {
                let cw = chunk.min(out_w - bx);
                let mut buf = vec![0u8; (cw * ch * 4) as usize];
                buf.par_chunks_mut((cw * 4) as usize)
                    .enumerate()
                    .for_each(|(r, row)| {
                        let v = (by + r as u32) as f32 + 0.5;
                        for c in 0..cw as usize {
                            let u = (bx + c as u32) as f32 + 0.5;
                            let (sx, sy) = map(u, v);
                            let (mut rr, mut gg, mut bb, mut aa) = src.sample_bilinear(sx, sy);
                            if let Some([br, bg, bb_bg, ba]) = background {
                                let src_a = aa as f32 / 255.0;
                                let bg_a = ba as f32 / 255.0;
                                let out_a = src_a + bg_a * (1.0 - src_a);
                                if out_a > 0.0 {
                                    rr = ((rr as f32 * src_a + br as f32 * bg_a * (1.0 - src_a))
                                        / out_a)
                                        .round()
                                        .clamp(0.0, 255.0)
                                        as u8;
                                    gg = ((gg as f32 * src_a + bg as f32 * bg_a * (1.0 - src_a))
                                        / out_a)
                                        .round()
                                        .clamp(0.0, 255.0)
                                        as u8;
                                    bb = ((bb as f32 * src_a + bb_bg as f32 * bg_a * (1.0 - src_a))
                                        / out_a)
                                        .round()
                                        .clamp(0.0, 255.0)
                                        as u8;
                                }
                                aa = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
                            }
                            let idx = c * 4;
                            row[idx] = rr;
                            row[idx + 1] = gg;
                            row[idx + 2] = bb;
                            row[idx + 3] = aa;
                        }
                    });
                new_tiles.write_region(bx, by, cw, ch, &buf);
                bx += cw;
            }
            by += ch;
        }
        new_tiles.bump_all_revisions();
        new_tiles
    }

    pub fn flip_horizontal(&mut self) {
        use crate::core::command::{PixelTransformCommand, PixelTransformKind};
        let cmd = PixelTransformCommand::capture_before(
            "Flip Horizontal",
            PixelTransformKind::FlipH,
            &self.layer_stack,
            self.width,
            self.height,
            &self.selection,
        );
        let canvas_w = self.width as i32;
        for layer in &mut self.layer_stack.layers {
            let lw = layer.width as i32;
            let old_ox = layer.offset.0;
            layer.tiles = layer.tiles.flip_h();
            layer.offset.0 = canvas_w - old_ox - lw;
            if let Some(mask) = &mut layer.mask {
                PixelTransformCommand::transform_layer_mask(mask, &PixelTransformKind::FlipH);
            }
        }
        {
            let (nm, nw, nh) = PixelTransformCommand::transform_sel_mask_pub(
                &self.selection.mask,
                &PixelTransformKind::FlipH,
                self.selection.width,
                self.selection.height,
            );
            self.selection.mask = nm;
            self.selection.width = nw;
            self.selection.height = nh;
            self.selection.offset = (0, 0);
            self.selection.mask_revision += 1;
            self.selection.mark_bbox_dirty();
        }
        self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
        self.flatten_full();
    }

    pub fn flip_vertical(&mut self) {
        use crate::core::command::{PixelTransformCommand, PixelTransformKind};
        let cmd = PixelTransformCommand::capture_before(
            "Flip Vertical",
            PixelTransformKind::FlipV,
            &self.layer_stack,
            self.width,
            self.height,
            &self.selection,
        );
        let canvas_h = self.height as i32;
        for layer in &mut self.layer_stack.layers {
            let lh = layer.height as i32;
            let old_oy = layer.offset.1;
            layer.tiles = layer.tiles.flip_v();
            layer.offset.1 = canvas_h - old_oy - lh;
            if let Some(mask) = &mut layer.mask {
                PixelTransformCommand::transform_layer_mask(mask, &PixelTransformKind::FlipV);
            }
        }
        {
            let (nm, nw, nh) = PixelTransformCommand::transform_sel_mask_pub(
                &self.selection.mask,
                &PixelTransformKind::FlipV,
                self.selection.width,
                self.selection.height,
            );
            self.selection.mask = nm;
            self.selection.width = nw;
            self.selection.height = nh;
            self.selection.offset = (0, 0);
            self.selection.mask_revision += 1;
            self.selection.mark_bbox_dirty();
        }
        self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
        self.flatten_full();
    }

    pub fn rotate_90_cw(&mut self) {
        use crate::core::command::{PixelTransformCommand, PixelTransformKind};
        let cmd = PixelTransformCommand::capture_before(
            "Rotate 90 CW",
            PixelTransformKind::Rotate90CW,
            &self.layer_stack,
            self.width,
            self.height,
            &self.selection,
        );
        let canvas_h = self.height as i32;
        for layer in &mut self.layer_stack.layers {
            let old_lh = layer.height as i32;
            let old_ox = layer.offset.0;
            let old_oy = layer.offset.1;
            layer.tiles = layer.tiles.rotate_90_cw();
            std::mem::swap(&mut layer.width, &mut layer.height);
            layer.offset.0 = canvas_h - old_oy - old_lh;
            layer.offset.1 = old_ox;
            if let Some(mask) = &mut layer.mask {
                PixelTransformCommand::transform_layer_mask(mask, &PixelTransformKind::Rotate90CW);
            }
        }
        std::mem::swap(&mut self.width, &mut self.height);
        {
            let (nm, nw, nh) = PixelTransformCommand::transform_sel_mask_pub(
                &self.selection.mask,
                &PixelTransformKind::Rotate90CW,
                self.selection.width,
                self.selection.height,
            );
            self.selection.mask = nm;
            self.selection.width = nw;
            self.selection.height = nh;
            self.selection.offset = (0, 0);
            self.selection.mask_revision += 1;
            self.selection.mark_bbox_dirty();
        }
        self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
        self.flatten_full();
    }

    pub fn rotate_90_ccw(&mut self) {
        use crate::core::command::{PixelTransformCommand, PixelTransformKind};
        let cmd = PixelTransformCommand::capture_before(
            "Rotate 90 CCW",
            PixelTransformKind::Rotate90CCW,
            &self.layer_stack,
            self.width,
            self.height,
            &self.selection,
        );
        let canvas_w = self.width as i32;
        for layer in &mut self.layer_stack.layers {
            let old_lw = layer.width as i32;
            let old_ox = layer.offset.0;
            let old_oy = layer.offset.1;
            layer.tiles = layer.tiles.rotate_90_ccw();
            std::mem::swap(&mut layer.width, &mut layer.height);
            layer.offset.0 = old_oy;
            layer.offset.1 = canvas_w - old_ox - old_lw;
            if let Some(mask) = &mut layer.mask {
                PixelTransformCommand::transform_layer_mask(mask, &PixelTransformKind::Rotate90CCW);
            }
        }
        std::mem::swap(&mut self.width, &mut self.height);
        {
            let (nm, nw, nh) = PixelTransformCommand::transform_sel_mask_pub(
                &self.selection.mask,
                &PixelTransformKind::Rotate90CCW,
                self.selection.width,
                self.selection.height,
            );
            self.selection.mask = nm;
            self.selection.width = nw;
            self.selection.height = nh;
            self.selection.offset = (0, 0);
            self.selection.mask_revision += 1;
            self.selection.mark_bbox_dirty();
        }
        self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
        self.flatten_full();
    }

    pub fn resize(&mut self, new_w: u32, new_h: u32) -> bool {
        if new_w == self.width && new_h == self.height {
            return true;
        }
        if new_w == 0 || new_h == 0 {
            return false;
        }
        let max = MAX_DIMENSION;
        if new_w > max || new_h > max {
            return false;
        }

        self.begin_undo_group("Resize Canvas");

        if self.selection.active {
            let mut sel_cmd = crate::core::command::SelectionCommand::capture_before(
                "Resize Canvas",
                &self.selection,
            );
            self.selection.resize(new_w, new_h);
            sel_cmd.capture_after(&self.selection);
            self.record_as(Box::new(sel_cmd), ChangeKind::Selection);
        } else {
            self.selection.resize(new_w, new_h);
        }

        let resize_cmd = crate::core::command::ResizeCanvasCommand::capture_before(
            &self.layer_stack,
            self.width,
            self.height,
            new_w,
            new_h,
        );
        for layer in &mut self.layer_stack.layers {
            let copy_w = layer.width.min(new_w);
            let copy_h = layer.height.min(new_h);
            // Tile-native crop/extend anchored top-left: the extended area stays a
            // sparse transparent region (no canvas-sized buffer), so canvas Resize
            // works under Viewport Streaming.
            let mut new_tiles = crate::core::tile::TileMap::new(new_w, new_h);
            if copy_w > 0 && copy_h > 0 {
                new_tiles.blit_region_from(&layer.tiles, 0, 0, 0, 0, copy_w, copy_h);
            }
            layer.tiles = new_tiles;
            layer.width = new_w;
            layer.height = new_h;
            if let Some(mask) = &mut layer.mask {
                mask.resize_to(new_w, new_h);
            }
        }
        self.width = new_w;
        self.height = new_h;
        self.pixels = if Self::fits_flat_buffer(new_w, new_h) {
            Self::checked_rgba_len(new_w, new_h)
                .map(|len| vec![255u8; len])
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        self.record_as(Box::new(resize_cmd), ChangeKind::LayerStructure);

        self.end_undo_group();
        self.flatten_full();
        true
    }

    pub fn resize_image(&mut self, new_w: u32, new_h: u32, new_dpi: f32) -> bool {
        if new_w == 0 || new_h == 0 {
            return false;
        }
        // Tile-native chunked resample (no canvas-sized buffer), so Image Size runs
        // under Viewport Streaming. Only the per-dimension cap applies.
        let max = MAX_DIMENSION;
        if new_w > max || new_h > max {
            return false;
        }

        let sx = new_w as f32 / self.width.max(1) as f32;
        let sy = new_h as f32 / self.height.max(1) as f32;

        self.begin_undo_group("Image Size");

        if self.selection.active {
            let mut sel_cmd = crate::core::command::SelectionCommand::capture_before(
                "Image Size",
                &self.selection,
            );
            Self::resample_selection(&mut self.selection, new_w, new_h, sx, sy);
            sel_cmd.capture_after(&self.selection);
            self.record_as(Box::new(sel_cmd), ChangeKind::Selection);
        } else {
            self.selection.resize(new_w, new_h);
        }

        let mut cmd = crate::core::command::LayerStructureCommand::capture_before(
            "Image Size",
            &self.layer_stack,
            self.width,
            self.height,
        );

        for layer in &mut self.layer_stack.layers {
            let layer_new_w = ((layer.width as f32 * sx).round() as u32).max(1);
            let layer_new_h = ((layer.height as f32 * sy).round() as u32).max(1);
            layer.tiles = Self::resample_tilemap(&layer.tiles, layer_new_w, layer_new_h);
            layer.width = layer_new_w;
            layer.height = layer_new_h;
            layer.offset = (
                (layer.offset.0 as f32 * sx).round() as i32,
                (layer.offset.1 as f32 * sy).round() as i32,
            );

            if let Some(mask) = &mut layer.mask {
                let mask_new_w = ((mask.width as f32 * sx).round() as u32).max(1);
                let mask_new_h = ((mask.height as f32 * sy).round() as u32).max(1);
                mask.tiles = Self::resample_tilemap(&mask.tiles, mask_new_w, mask_new_h);
                mask.width = mask_new_w;
                mask.height = mask_new_h;
            }
        }

        self.width = new_w;
        self.height = new_h;
        self.metadata.resolution_ppi = new_dpi.max(1.0);
        self.pixels = if Self::fits_flat_buffer(new_w, new_h) {
            Self::checked_rgba_len(new_w, new_h)
                .map(|len| vec![255u8; len])
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        cmd.capture_after(&self.layer_stack, self.width, self.height);
        self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
        self.end_undo_group();
        self.flatten_full();
        true
    }

    /// Bilinear resample `src` to `new_w × new_h`, tile-native (256-px chunks, no
    /// canvas-sized buffer) so Image Size works under Viewport Streaming.
    pub(crate) fn resample_tilemap(
        src: &crate::core::tile::TileMap,
        new_w: u32,
        new_h: u32,
    ) -> crate::core::tile::TileMap {
        if src.width == new_w && src.height == new_h {
            return src.clone();
        }

        let scale_x = src.width.max(1) as f32 / new_w.max(1) as f32;
        let scale_y = src.height.max(1) as f32 / new_h.max(1) as f32;
        Self::resample_into_tiles(
            src,
            new_w,
            new_h,
            move |u, v| (u * scale_x - 0.5, v * scale_y - 0.5),
            None,
        )
    }

    pub(crate) fn resample_selection(
        selection: &mut crate::core::selection::Selection,
        new_w: u32,
        new_h: u32,
        sx: f32,
        sy: f32,
    ) {
        use rayon::prelude::*;

        let old_w = selection.width.max(1);
        let old_h = selection.height.max(1);
        let old_mask = selection.mask.clone();
        let old_offset = selection.offset;
        let scale_x = old_w as f32 / new_w.max(1) as f32;
        let scale_y = old_h as f32 / new_h.max(1) as f32;
        let mut mask = vec![0u8; (new_w as usize).saturating_mul(new_h as usize)];

        mask.par_chunks_mut(new_w as usize)
            .enumerate()
            .for_each(|(y, row)| {
                let src_y = ((y as f32 + 0.5) * scale_y - 0.5)
                    .round()
                    .clamp(0.0, (old_h - 1) as f32) as u32;
                for x in 0..new_w as usize {
                    let src_x = ((x as f32 + 0.5) * scale_x - 0.5)
                        .round()
                        .clamp(0.0, (old_w - 1) as f32) as u32;
                    row[x] = old_mask[(src_y * old_w + src_x) as usize];
                }
            });

        selection.mask = mask;
        selection.width = new_w;
        selection.height = new_h;
        selection.offset = (
            (old_offset.0 as f32 * sx).round() as i32,
            (old_offset.1 as f32 * sy).round() as i32,
        );
        selection.active = selection.mask.par_iter().any(|&v| v > 0);
        selection.mask_revision += 1;
        selection.mark_bbox_dirty();
    }
}

#![allow(dead_code)]
use super::brush::{soft_round_alpha, BrushTool, StrokeBuffer};
use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::canvas::Canvas;
use crate::core::tile::TileMap;

pub struct CloneTool {
    /// Tip diameter in canvas px (same meaning as the Brush size).
    pub size: f32,
    pub hardness: f32,
    pub opacity: f32,
    pub spacing: f32,
    pub aligned: bool,
    pub sample_merged: bool,
    /// Repair mode: transfer source TEXTURE but shift its colour so the patch's
    /// average matches the destination's → seamless blend (blemish removal).
    pub heal_mode: bool,
    /// Spot mode: no Alt+click needed — each stroke auto-samples a clean patch
    /// from just outside the brush and heals over the spot (1-click blemish fix).
    pub spot_mode: bool,
    /// Smart healing (Repair Brush only): instead of cloning one nearby
    /// patch, record the brushed region and let the App synthesise the fill from
    /// the surrounding texture (PatchMatch) on release.
    pub smart_fill: bool,

    source_canvas_x: f32,
    source_canvas_y: f32,
    has_source: bool,
    /// The current source was auto-picked by spot healing (no Alt+click).
    spot_auto: bool,

    offset_x: f32,
    offset_y: f32,
    offset_set: bool,

    stroke_start_x: f32,
    stroke_start_y: f32,

    flat_cache: Option<(u64, TileMap)>,
    stroke_source: Option<TileMap>,
    stroke_source_merged: bool,
    /// Per-stroke coverage + stroke-start snapshot for the plain Clone
    /// (opacity caps the stroke, like the Brush). None for healing / CMYK.
    stroke: Option<StrokeBuffer>,
    /// Distance to the next dab along the stroke path (plain Clone).
    walk_residual: f32,

    ca_recording: bool,
    ca_dirty: bool,
    /// Soft brush coverage (0..1, size/hardness/opacity baked in), max-accumulated
    /// over the stroke. Drained on release for the blemish-aware skin heal.
    ca_mask: Vec<f32>,
    ca_lw: u32,
    ca_lh: u32,
    ca_ox: i32,
    ca_oy: i32,
}

impl CloneTool {
    pub fn new() -> Self {
        Self {
            size: 60.0,
            hardness: 0.0,
            opacity: 1.0,
            spacing: 0.25,
            aligned: true,
            sample_merged: false,
            heal_mode: false,
            spot_mode: false,
            smart_fill: false,
            source_canvas_x: 0.0,
            source_canvas_y: 0.0,
            has_source: false,
            spot_auto: false,
            offset_x: 0.0,
            offset_y: 0.0,
            offset_set: false,
            stroke_start_x: 0.0,
            stroke_start_y: 0.0,
            flat_cache: None,
            stroke_source: None,
            stroke_source_merged: false,
            stroke: None,
            walk_residual: 0.0,
            ca_recording: false,
            ca_dirty: false,
            ca_mask: Vec::new(),
            ca_lw: 0,
            ca_lh: 0,
            ca_ox: 0,
            ca_oy: 0,
        }
    }

    pub fn is_smart_fill(&self) -> bool {
        self.smart_fill
    }

    /// Start recording a content-aware healing stroke. Captures the active raster
    /// layer's geometry; returns false if the layer can't be painted onto.
    fn ca_begin(&mut self, canvas: &Canvas) -> bool {
        if canvas.layer_stack.layers.is_empty() {
            return false;
        }
        let l = canvas.active_layer();
        if (!l.is_background && l.locked) || !l.is_raster() {
            return false;
        }
        self.ca_lw = l.width;
        self.ca_lh = l.height;
        self.ca_ox = l.offset.0;
        self.ca_oy = l.offset.1;
        self.ca_mask = vec![0.0; (l.width as usize) * (l.height as usize)];
        self.ca_recording = true;
        self.ca_dirty = false;
        true
    }

    /// Stamp a soft brush disc (size/hardness/opacity, pixel-center sampled) into
    /// the content-aware coverage mask, max-accumulated so overlapping dabs in one
    /// stroke don't harden the soft edge. Canvas coords.
    fn ca_dab(&mut self, cx: f32, cy: f32) {
        if !self.ca_recording {
            return;
        }
        let r = self.radius();
        let r2 = r * r;
        let opacity = self.opacity.clamp(0.0, 1.0);
        let lw = self.ca_lw as i32;
        let lh = self.ca_lh as i32;
        let lcx = cx - self.ca_ox as f32;
        let lcy = cy - self.ca_oy as f32;
        let x0 = ((lcx - r).floor() as i32).max(0);
        let y0 = ((lcy - r).floor() as i32).max(0);
        let x1 = ((lcx + r).ceil() as i32).min(lw);
        let y1 = ((lcy + r).ceil() as i32).min(lh);
        for y in y0..y1 {
            let dy = y as f32 + 0.5 - lcy;
            let dy2 = dy * dy;
            for x in x0..x1 {
                let dx = x as f32 + 0.5 - lcx;
                let d2 = dx * dx + dy2;
                if d2 > r2 {
                    continue;
                }
                let cov = soft_round_alpha(d2, r, self.hardness);
                let v = cov * opacity;
                let idx = (y as usize) * (self.ca_lw as usize) + x as usize;
                if v > self.ca_mask[idx] {
                    self.ca_mask[idx] = v;
                    self.ca_dirty = true;
                }
            }
        }
    }

    fn ca_segment(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) {
        let dist = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
        let spacing = (self.radius() * 0.25).max(0.5);
        let steps = ((dist / spacing).ceil() as u32).clamp(1, 4000);
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            self.ca_dab(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t);
        }
    }

    /// Drain the recorded content-aware stroke. Returns the layer-local hole mask
    /// (with its dimensions) if anything was painted; `None` otherwise. The App
    /// calls this after `on_release` to run the actual synthesis + undo commit.
    pub fn take_pending_ca(&mut self) -> Option<(Vec<f32>, u32, u32)> {
        self.ca_recording = false;
        if self.ca_dirty && self.ca_mask.len() == (self.ca_lw as usize) * (self.ca_lh as usize) {
            self.ca_dirty = false;
            Some((std::mem::take(&mut self.ca_mask), self.ca_lw, self.ca_lh))
        } else {
            self.ca_mask = Vec::new();
            self.ca_dirty = false;
            None
        }
    }

    #[inline]
    fn radius(&self) -> f32 {
        (self.size * 0.5).max(0.5)
    }

    /// Distance between dab centres for the plain Clone, as a fraction of the
    /// tip diameter (Photoshop's Spacing).
    #[inline]
    fn dab_spacing(&self) -> f32 {
        (self.size * self.spacing).max(0.5)
    }

    /// The plain Clone follows the Brush's stroke model. Healing keeps per-dab
    /// compositing: each membrane dab must blend against what earlier dabs in
    /// the same stroke already healed.
    #[inline]
    fn uses_stroke_model(&self) -> bool {
        !self.heal_mode && !self.spot_mode
    }

    /// Why a stroke can't start on the active layer, if it can't.
    fn paint_block_reason(&self, canvas: &Canvas) -> Option<&'static str> {
        if canvas.layer_stack.layers.is_empty() {
            return Some("Chưa có layer để Clone");
        }
        if matches!(
            canvas.channels.view,
            crate::core::channels::ChannelView::Alpha(_)
        ) {
            return Some("Clone không vẽ lên kênh Alpha — chọn lại kênh RGB");
        }
        let layer = canvas.active_layer();
        if layer.locked && !layer.is_background {
            return Some("Layer đang khoá");
        }
        if layer.get_paint_tiles().is_none() {
            return Some("Cần chọn một layer ảnh (raster) để Clone");
        }
        if !self.uses_stroke_model() && layer.paint_target == crate::core::layer::PaintTarget::Mask
        {
            return Some("Repair Brush không vẽ lên mask — bấm vào ảnh của layer");
        }
        None
    }

    fn stamp(&mut self, canvas: &mut Canvas, x: f32, y: f32) {
        if self.stroke.is_some() {
            self.stamp_dab_stroked(canvas, x, y);
        } else {
            self.stamp_dab(canvas, x, y);
        }
    }

    pub fn source(&self) -> Option<(f32, f32)> {
        if self.has_source {
            Some((self.source_canvas_x, self.source_canvas_y))
        } else {
            None
        }
    }

    /// Whole-pixel offset (destination − source) a stroke starting at
    /// `(dst_x, dst_y)` would use right now — drives the clone preview. Before
    /// the offset is locked (just after Alt+click) the stroke would start at the
    /// source point itself.
    pub fn preview_offset(&self, dst_x: f32, dst_y: f32) -> Option<(i32, i32)> {
        if !self.has_source || self.smart_fill {
            return None;
        }
        let (ox, oy) = if self.offset_set || self.stroke_source.is_some() {
            (self.offset_x, self.offset_y)
        } else {
            (dst_x - self.source_canvas_x, dst_y - self.source_canvas_y)
        };
        Some((ox.round() as i32, oy.round() as i32))
    }

    /// Canvas-space point being sampled while a stroke is in progress (the
    /// source crosshair Photoshop shows during painting).
    pub fn stroke_source_center(&self, dst_x: f32, dst_y: f32) -> Option<(f32, f32)> {
        (self.has_source && !self.spot_auto && self.stroke_source.is_some())
            .then(|| (dst_x - self.offset_x, dst_y - self.offset_y))
    }

    fn stamp_dab(&mut self, canvas: &mut Canvas, dst_cx: f32, dst_cy: f32) {
        if !self.has_source {
            return;
        }
        if canvas.layer_stack.layers.is_empty() {
            return;
        }
        canvas.layer_stack.normalize_active_idx();

        let r = self.radius().min(2000.0);
        let r2 = r * r;
        let opacity = self.opacity.clamp(0.0, 1.0);

        let dst_layer_idx = canvas.layer_stack.active_idx;
        let dst_ox = canvas.layer_stack.layers[dst_layer_idx].offset.0;
        let dst_oy = canvas.layer_stack.layers[dst_layer_idx].offset.1;
        let dst_lw = canvas.layer_stack.layers[dst_layer_idx].width;
        let dst_lh = canvas.layer_stack.layers[dst_layer_idx].height;

        let lcx = dst_cx - dst_ox as f32;
        let lcy = dst_cy - dst_oy as f32;

        let lx0 = ((lcx - r).floor().max(0.0) as u32).min(dst_lw);
        let ly0 = ((lcy - r).floor().max(0.0) as u32).min(dst_lh);
        let lx1 = ((lcx + r).ceil() as u32).min(dst_lw);
        let ly1 = ((lcy + r).ceil() as u32).min(dst_lh);

        if lx1 <= lx0 || ly1 <= ly0 {
            return;
        }

        let Some(src_pixels) = self.stroke_source.as_ref() else {
            return;
        };

        let tile_size = crate::core::tile::TILE_SIZE;
        let has_sel = canvas.selection.active;
        let sel_ptr: *const crate::core::selection::Selection = &canvas.selection;
        let lock_alpha = canvas.layer_stack.layers[dst_layer_idx].lock_alpha;
        // Channels-panel write gate: each enabled channel copies its own
        // plate from the source pixel; the others and alpha stay untouched.
        let channel_wm = canvas.channels.write_gate();

        let membrane = if self.heal_mode || self.spot_mode {
            Some(self.heal_membrane(canvas, dst_layer_idx, lx0, ly0, lx1, ly1, lcx, lcy, r, r2))
        } else {
            None
        };

        // CMYK: the source snapshot is RGB (the mirror); convert each sampled
        // (membrane-corrected) source colour to ink and blend into the dest
        // plane, then re-project the mirror. Exact for the naive space.
        let cmyk_conv = canvas.cmyk_converter();

        let tx0 = lx0 / tile_size;
        let ty0 = ly0 / tile_size;
        let tx1 = (lx1 - 1) / tile_size;
        let ty1 = (ly1 - 1) / tile_size;

        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                let pos = crate::core::tile::TilePos {
                    x: tx as i32,
                    y: ty as i32,
                };
                let tile_x0 = tx * tile_size;
                let tile_y0 = ty * tile_size;

                let px_min = tile_x0.max(lx0);
                let py_min = tile_y0.max(ly0);
                let px_max = (tile_x0 + tile_size).min(lx1);
                let py_max = (tile_y0 + tile_size).min(ly1);
                if px_max <= px_min || py_max <= py_min {
                    continue;
                }

                let dst_tile = if cmyk_conv.is_some() {
                    canvas.layer_stack.layers[dst_layer_idx]
                        .tiles
                        .get_tile_mut_ink(pos)
                } else {
                    canvas.layer_stack.layers[dst_layer_idx]
                        .tiles
                        .get_tile_mut(pos)
                };

                for py in py_min..py_max {
                    let dy = py as f32 + 0.5 - lcy;
                    let dy2 = dy * dy;
                    let row_off = (py - tile_y0) * tile_size;
                    let canvas_py = (py as i32 + dst_oy) as u32;

                    for px in px_min..px_max {
                        let dx = px as f32 + 0.5 - lcx;
                        let d2 = dx * dx + dy2;
                        if d2 > r2 {
                            continue;
                        }

                        let alpha = soft_round_alpha(d2, r, self.hardness);
                        let mut src_a_factor = alpha * opacity;
                        if src_a_factor < 0.001 {
                            continue;
                        }

                        if has_sel {
                            let canvas_px = (px as i32 + dst_ox) as u32;
                            unsafe {
                                src_a_factor *= (*sel_ptr).sample(canvas_px, canvas_py);
                            }
                            if src_a_factor < 0.001 {
                                continue;
                            }
                        }

                        let src_abs_x = (px as i32 + dst_ox) as f32 - self.offset_x;
                        let src_abs_y = (py as i32 + dst_oy) as f32 - self.offset_y;
                        if src_abs_x < 0.0 || src_abs_y < 0.0 {
                            continue;
                        }

                        let (sr, sg, sb, sa) = if self.stroke_source_merged {
                            src_pixels.get_pixel(src_abs_x as u32, src_abs_y as u32)
                        } else {
                            let slx = src_abs_x as i32 - dst_ox;
                            let sly = src_abs_y as i32 - dst_oy;
                            if slx < 0 || sly < 0 {
                                continue;
                            }
                            src_pixels.get_pixel(slx as u32, sly as u32)
                        };
                        if sa == 0 {
                            continue;
                        }

                        let col = px - tile_x0;
                        let i = ((row_off + col) * 4) as usize;

                        let src_alpha = (sa as f32 / 255.0) * src_a_factor;
                        let dst_a = dst_tile.pixels[i + 3] as f32 / 255.0;
                        if lock_alpha && dst_a < 0.001 {
                            continue;
                        }
                        let mut out_a = src_alpha + dst_a * (1.0 - src_alpha);
                        if lock_alpha {
                            out_a = dst_a;
                        }
                        if out_a < 0.001 {
                            continue;
                        }

                        let (cr, cg, cb) = match &membrane {
                            Some((c, mw, _)) => {
                                let mi = ((py - ly0) as usize * *mw + (px - lx0) as usize) * 3;
                                (c[mi], c[mi + 1], c[mi + 2])
                            }
                            None => (0.0, 0.0, 0.0),
                        };
                        let src_r = (sr as f32 / 255.0 + cr).clamp(0.0, 1.0);
                        let src_g = (sg as f32 / 255.0 + cg).clamp(0.0, 1.0);
                        let src_b = (sb as f32 / 255.0 + cb).clamp(0.0, 1.0);
                        if let Some(conv) = cmyk_conv.as_ref() {
                            let fink = conv.rgb_to_cmyk_one([
                                (src_r * 255.0).round() as u8,
                                (src_g * 255.0).round() as u8,
                                (src_b * 255.0).round() as u8,
                            ]);
                            let dw = dst_a * (1.0 - src_alpha);
                            let plane = dst_tile.ink.as_mut().expect("cmyk tile carries ink");
                            let mut ink = [plane[i], plane[i + 1], plane[i + 2], plane[i + 3]];
                            for c in 0..4 {
                                // Plate gate: only clone into enabled ink plates.
                                if let Some(wm) = channel_wm {
                                    if !wm[c] {
                                        continue;
                                    }
                                }
                                let d = plane[i + c] as f32 / 255.0;
                                let s = fink[c] as f32 / 255.0;
                                let v = if lock_alpha || channel_wm.is_some() {
                                    d + (s - d) * src_alpha
                                } else {
                                    (s * src_alpha + d * dw) / out_a
                                };
                                let v = (v * 255.0).round().clamp(0.0, 255.0) as u8;
                                plane[i + c] = v;
                                ink[c] = v;
                            }
                            let rgb = conv.cmyk_to_rgb_one(ink);
                            dst_tile.pixels[i] = rgb[0];
                            dst_tile.pixels[i + 1] = rgb[1];
                            dst_tile.pixels[i + 2] = rgb[2];
                            // Plate/lock-alpha edits keep alpha; a full ink clone
                            // updates it with the source-over result.
                            if channel_wm.is_none() {
                                dst_tile.pixels[i + 3] =
                                    (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
                            }
                            continue;
                        }
                        if let Some(wm) = channel_wm {
                            crate::core::tile::blend_masked(
                                &mut dst_tile.pixels[i..i + 4],
                                [src_r, src_g, src_b],
                                src_alpha,
                                wm,
                            );
                            continue;
                        }
                        let dst_r = dst_tile.pixels[i] as f32 / 255.0;
                        let dst_g = dst_tile.pixels[i + 1] as f32 / 255.0;
                        let dst_b = dst_tile.pixels[i + 2] as f32 / 255.0;

                        let inv = 1.0 - src_alpha;
                        let denom = src_alpha + dst_a * inv;
                        if denom < 0.001 {
                            continue;
                        }
                        // .round(): truncation drifts every overlapping dab
                        // toward black / transparent (dark blotches).
                        let to8 = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
                        dst_tile.pixels[i] = to8((src_r * src_alpha + dst_r * dst_a * inv) / denom);
                        dst_tile.pixels[i + 1] =
                            to8((src_g * src_alpha + dst_g * dst_a * inv) / denom);
                        dst_tile.pixels[i + 2] =
                            to8((src_b * src_alpha + dst_b * dst_a * inv) / denom);
                        dst_tile.pixels[i + 3] = to8(out_a);
                    }
                }
            }
        }

        let cx0 = (lx0 as i32 + dst_ox).max(0) as u32;
        let cy0 = (ly0 as i32 + dst_oy).max(0) as u32;
        let cx1 = ((lx1 as i32 + dst_ox) as u32).min(canvas.width);
        let cy1 = ((ly1 as i32 + dst_oy) as u32).min(canvas.height);
        canvas.mark_dirty(cx0, cy0, cx1, cy1);
    }

    fn stamp_segment(&mut self, canvas: &mut Canvas, x0: f32, y0: f32, x1: f32, y1: f32) {
        if !x0.is_finite() || !y0.is_finite() || !x1.is_finite() || !y1.is_finite() {
            return;
        }
        let dist = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
        let spacing = (self.radius() * self.spacing).max(0.5);
        let steps = ((dist / spacing).ceil() as u32).clamp(1, 1000);
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            self.stamp_dab(canvas, x0 + (x1 - x0) * t, y0 + (y1 - y0) * t);
        }
    }

    /// Plain Clone dab (Photoshop Clone Stamp model): brush coverage
    /// accumulates in the stroke buffer and each touched pixel is recomposited
    /// from its stroke-start value, so Opacity caps the whole stroke and dab
    /// overlap never builds past it. Writes the active paint target — a layer
    /// mask receives the source's gray value.
    fn stamp_dab_stroked(&mut self, canvas: &mut Canvas, dst_cx: f32, dst_cy: f32) {
        let r = self.radius().min(2500.0);
        let r2 = r * r;
        let opacity = self.opacity.clamp(0.0, 1.0);
        let hardness = self.hardness;
        let (off_x, off_y) = (self.offset_x, self.offset_y);
        let merged = self.stroke_source_merged;
        let (Some(stroke), Some(src)) = (self.stroke.as_mut(), self.stroke_source.as_ref()) else {
            return;
        };
        let idx = canvas.layer_stack.active_idx;
        let Some(layer) = canvas.layer_stack.layers.get(idx) else {
            return;
        };
        let (ox, oy) = layer.offset;
        let (lw, lh) = (layer.width, layer.height);
        let paint_mask = stroke.paint_mask;
        let lock_alpha = !paint_mask && layer.lock_alpha;
        let channel_wm = if paint_mask {
            None
        } else {
            canvas.channels.write_gate()
        };

        let lcx = dst_cx - ox as f32;
        let lcy = dst_cy - oy as f32;
        let lx0 = ((lcx - r).floor().max(0.0) as u32).min(lw);
        let ly0 = ((lcy - r).floor().max(0.0) as u32).min(lh);
        let lx1 = ((lcx + r).ceil().max(0.0) as u32).min(lw);
        let ly1 = ((lcy + r).ceil().max(0.0) as u32).min(lh);
        if lx1 <= lx0 || ly1 <= ly0 {
            return;
        }

        let selection = &canvas.selection;
        let has_sel = selection.active;
        let Some(tiles) = canvas.layer_stack.layers[idx].get_paint_tiles_mut() else {
            return;
        };
        let ts = crate::core::tile::TILE_SIZE;
        let tile_px = (ts * ts) as usize;
        let to8 = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;

        for ty in (ly0 / ts)..=((ly1 - 1) / ts) {
            for tx in (lx0 / ts)..=((lx1 - 1) / ts) {
                let tile_x0 = tx * ts;
                let tile_y0 = ty * ts;
                let px_min = tile_x0.max(lx0);
                let py_min = tile_y0.max(ly0);
                let px_max = (tile_x0 + ts).min(lx1);
                let py_max = (tile_y0 + ts).min(ly1);
                if px_max <= px_min || py_max <= py_min {
                    continue;
                }
                let pos = crate::core::tile::TilePos {
                    x: tx as i32,
                    y: ty as i32,
                };
                let base = stroke.base.tiles.get(&pos).cloned();
                let cov = stroke
                    .cov
                    .entry(pos)
                    .or_insert_with(|| vec![0.0f32; tile_px].into_boxed_slice());
                let dst = tiles.get_tile_mut(pos);

                for py in py_min..py_max {
                    let dy = py as f32 + 0.5 - lcy;
                    let dy2 = dy * dy;
                    let canvas_y = py as i32 + oy;
                    for px in px_min..px_max {
                        let dx = px as f32 + 0.5 - lcx;
                        let d2 = dx * dx + dy2;
                        if d2 > r2 {
                            continue;
                        }
                        let canvas_x = px as i32 + ox;
                        let mut a = soft_round_alpha(d2, r, hardness);
                        if has_sel {
                            if canvas_x < 0 || canvas_y < 0 {
                                continue;
                            }
                            a *= selection.sample(canvas_x as u32, canvas_y as u32);
                        }
                        if a < 0.001 {
                            continue;
                        }

                        // The offset is fixed for the whole stroke and the source
                        // is a stroke-start snapshot, so every dab over a pixel
                        // reads the same source colour — recompositing from the
                        // base is idempotent.
                        let (sr, sg, sb, sa) = if merged {
                            let sx = (canvas_x as f32 - off_x).floor();
                            let sy = (canvas_y as f32 - off_y).floor();
                            if sx < 0.0 || sy < 0.0 {
                                continue;
                            }
                            src.get_pixel(sx as u32, sy as u32)
                        } else {
                            let sx = (px as f32 - off_x).floor();
                            let sy = (py as f32 - off_y).floor();
                            if sx < 0.0 || sy < 0.0 {
                                continue;
                            }
                            src.get_pixel(sx as u32, sy as u32)
                        };

                        let local = ((py - tile_y0) * ts + (px - tile_x0)) as usize;
                        let c = &mut cov[local];
                        *c += a * (1.0 - *c);
                        let s = (*c * opacity).min(1.0);
                        let (br, bg, bb, ba) = base
                            .as_ref()
                            .map_or((0, 0, 0, 0), |t| t.get_pixel(px - tile_x0, py - tile_y0));
                        let i = local * 4;

                        if paint_mask {
                            // Masks store gray in RGB; an absent tile reads as
                            // black, which is a real mask value, not "no source".
                            let (v, eff) = if merged {
                                if sa == 0 {
                                    continue;
                                }
                                let l = (0.299 * sr as f32 + 0.587 * sg as f32 + 0.114 * sb as f32)
                                    / 255.0;
                                (l, s * sa as f32 / 255.0)
                            } else {
                                (sr as f32 / 255.0, s)
                            };
                            let b = br as f32 / 255.0;
                            let out = to8(b + (v - b) * eff);
                            dst.pixels[i] = out;
                            dst.pixels[i + 1] = out;
                            dst.pixels[i + 2] = out;
                            dst.pixels[i + 3] = 255;
                            continue;
                        }
                        if sa == 0 {
                            continue;
                        }

                        let eff = s * sa as f32 / 255.0;
                        if let Some(wm) = channel_wm {
                            let base_rgb = [br, bg, bb];
                            let src_rgb = [sr, sg, sb];
                            for ch in 0..3 {
                                dst.pixels[i + ch] = if wm[ch] {
                                    let b = base_rgb[ch] as f32 / 255.0;
                                    to8(b + (src_rgb[ch] as f32 / 255.0 - b) * eff)
                                } else {
                                    base_rgb[ch]
                                };
                            }
                            dst.pixels[i + 3] = ba;
                            continue;
                        }

                        let base_a = ba as f32 / 255.0;
                        if lock_alpha && base_a < 0.001 {
                            continue;
                        }
                        let dst_w = base_a * (1.0 - eff);
                        let out_a = eff + dst_w;
                        if out_a < 0.001 {
                            continue;
                        }
                        let mix = |s8: u8, b8: u8| {
                            to8((s8 as f32 / 255.0 * eff + b8 as f32 / 255.0 * dst_w) / out_a)
                        };
                        dst.pixels[i] = mix(sr, br);
                        dst.pixels[i + 1] = mix(sg, bg);
                        dst.pixels[i + 2] = mix(sb, bb);
                        dst.pixels[i + 3] = if lock_alpha { ba } else { to8(out_a) };
                    }
                }
            }
        }

        let cx0 = (lx0 as i32 + ox).max(0) as u32;
        let cy0 = (ly0 as i32 + oy).max(0) as u32;
        let cx1 = ((lx1 as i32 + ox).max(0) as u32).min(canvas.width);
        let cy1 = ((ly1 as i32 + oy).max(0) as u32).min(canvas.height);
        if cx1 > cx0 && cy1 > cy0 {
            canvas.mark_dirty(cx0, cy0, cx1, cy1);
        }
    }

    fn capture_source_for_stroke(&mut self, canvas: &mut Canvas) {
        if self.sample_merged {
            let rev = canvas.layer_revision;
            let needs_rebuild = self.flat_cache.as_ref().map_or(true, |(r, _)| *r != rev);
            if needs_rebuild {
                // Reuse the display composite when it is current instead of
                // re-flattening every layer at each stroke start.
                canvas.ensure_pixels();
                let (w, h) = (canvas.width, canvas.height);
                let tiles = if !canvas.pixels_stale
                    && canvas.pixels.len() == (w as usize) * (h as usize) * 4
                {
                    TileMap::from_rgba(&canvas.pixels, w, h)
                } else {
                    TileMap::from_rgba(&canvas.layer_stack.flatten(w, h), w, h)
                };
                self.flat_cache = Some((rev, tiles));
            }
            self.stroke_source = self.flat_cache.as_ref().map(|(_, tiles)| tiles.clone());
            self.stroke_source_merged = true;
        } else {
            // Sample what the stroke paints: the layer mask when it is the
            // paint target, else the layer pixels.
            let idx = canvas.layer_stack.active_idx;
            self.stroke_source = canvas
                .layer_stack
                .layers
                .get(idx)
                .and_then(|l| l.get_paint_tiles())
                .cloned();
            self.stroke_source_merged = false;
        }
    }

    /// Spot heal: choose a nearby clean source automatically. It compares the
    /// destination rim with candidate rims at several distances/directions, then
    /// prefers candidates whose color and texture match the surrounding skin while
    /// avoiding hard local contrast. Reads from the captured stroke snapshot, so
    /// earlier dabs in the same stroke cannot feed back into later samples.
    fn spot_pick_offset(&self, canvas: &Canvas, dst_cx: f32, dst_cy: f32) -> Option<(f32, f32)> {
        let src = self.stroke_source.as_ref()?;
        let r = (self.size * 0.5).max(1.0).min(2000.0);
        let merged = self.stroke_source_merged;

        let (ox, oy, lw, lh) = if canvas.layer_stack.layers.is_empty() {
            (0i32, 0i32, canvas.width, canvas.height)
        } else {
            let idx = canvas
                .layer_stack
                .active_idx
                .min(canvas.layer_stack.layers.len() - 1);
            let l = &canvas.layer_stack.layers[idx];
            if merged {
                (0, 0, canvas.width, canvas.height)
            } else {
                (l.offset.0, l.offset.1, l.width, l.height)
            }
        };

        let sample_rgb = |cx: f32, cy: f32| -> Option<[f32; 3]> {
            let (sx, sy) = if merged {
                (cx, cy)
            } else {
                (cx - ox as f32, cy - oy as f32)
            };
            if sx < 0.0 || sy < 0.0 {
                return None;
            }
            let (sxu, syu) = (sx as u32, sy as u32);
            if sxu >= lw || syu >= lh {
                return None;
            }
            let (pr, pg, pb, pa) = src.get_pixel(sxu, syu);
            if pa == 0 {
                return None;
            }
            Some([pr as f32, pg as f32, pb as f32])
        };

        let luma = |rgb: [f32; 3]| 0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2];
        let rgb_dist = |a: [f32; 3], b: [f32; 3]| {
            let dr = a[0] - b[0];
            let dg = a[1] - b[1];
            let db = a[2] - b[2];
            (dr * dr + dg * dg + db * db).sqrt()
        };
        let collect_stats = |cx: f32, cy: f32, probes: &[(f32, f32)]| -> Option<([f32; 3], f32)> {
            let mut sum = [0.0f32; 3];
            let mut lsum = 0.0f32;
            let mut lsum2 = 0.0f32;
            for &(dx, dy) in probes {
                let rgb = sample_rgb(cx + dx, cy + dy)?;
                let y = luma(rgb);
                sum[0] += rgb[0];
                sum[1] += rgb[1];
                sum[2] += rgb[2];
                lsum += y;
                lsum2 += y * y;
            }
            let n = probes.len() as f32;
            let mean = [sum[0] / n, sum[1] / n, sum[2] / n];
            let lum_mean = lsum / n;
            let var = (lsum2 / n - lum_mean * lum_mean).max(0.0);
            Some((mean, var.sqrt()))
        };

        // Score a candidate with the exact same relative probes that are visible
        // at the destination. A brush crossing the page edge must not make the
        // source require pixels for the clipped-away half of the dab.
        const DIRS: usize = 16;
        let mut ring_probes = Vec::with_capacity(DIRS * 2);
        for scale in [0.62f32, 0.92] {
            let rr = r * scale;
            for i in 0..DIRS {
                let a = std::f32::consts::TAU * i as f32 / DIRS as f32;
                let probe = (rr * a.cos(), rr * a.sin());
                if sample_rgb(dst_cx + probe.0, dst_cy + probe.1).is_some() {
                    ring_probes.push(probe);
                }
            }
        }
        if ring_probes.len() < 8 {
            return None;
        }

        let body_probe_candidates = [
            (0.0, 0.0),
            (0.45, 0.0),
            (-0.45, 0.0),
            (0.0, 0.45),
            (0.0, -0.45),
            (0.55, 0.55),
            (-0.55, 0.55),
            (0.55, -0.55),
            (-0.55, -0.55),
        ];
        let body_probes: Vec<(f32, f32)> = body_probe_candidates
            .into_iter()
            .map(|(x, y)| (x * r, y * r))
            .filter(|&(dx, dy)| sample_rgb(dst_cx + dx, dst_cy + dy).is_some())
            .collect();

        let (dst_rim_mean, dst_rim_std) = collect_stats(dst_cx, dst_cy, &ring_probes)?;
        let dst_body_std = if body_probes.len() >= 3 {
            collect_stats(dst_cx, dst_cy, &body_probes)?.1
        } else {
            dst_rim_std
        };

        // The scoring probes stop at 0.92r. Also require the candidate to
        // contain the complete part of the dab that survives edge clipping, so
        // the unprobed outer pixels cannot turn transparent at the page edge.
        let left = ox as f32;
        let top = oy as f32;
        let right = left + lw as f32;
        let bottom = top + lh as f32;
        let visible_dx0 = (-r).max(left - dst_cx);
        let visible_dy0 = (-r).max(top - dst_cy);
        let visible_dx1 = r.min(right - dst_cx);
        let visible_dy1 = r.min(bottom - dst_cy);
        let covers_visible_dab = |cx: f32, cy: f32| {
            cx + visible_dx0 >= left
                && cy + visible_dy0 >= top
                && cx + visible_dx1 < right
                && cy + visible_dy1 < bottom
        };

        let mut best_score = f32::MAX;
        let mut best_off: Option<(f32, f32)> = None;
        for radius_factor in [1.55f32, 2.15, 2.85, 3.65] {
            let dist = (r * radius_factor).max(r + 5.0);
            for i in 0..16 {
                let a = std::f32::consts::TAU * (i as f32 + 0.25 * radius_factor) / 16.0;
                let scx = dst_cx + dist * a.cos();
                let scy = dst_cy + dist * a.sin();
                if !covers_visible_dab(scx, scy) {
                    continue;
                }
                let Some((src_rim_mean, src_rim_std)) = collect_stats(scx, scy, &ring_probes)
                else {
                    continue;
                };
                let src_body_std = if body_probes.len() >= 3 {
                    let Some((_, std)) = collect_stats(scx, scy, &body_probes) else {
                        continue;
                    };
                    std
                } else {
                    src_rim_std
                };

                let color = rgb_dist(src_rim_mean, dst_rim_mean) / 255.0;
                let rim_texture = (src_rim_std - dst_rim_std).abs() / 255.0;
                let body_texture = (src_body_std - dst_body_std).abs() / 255.0;
                let smoothness = src_body_std / 255.0;
                let score = color * 2.35
                    + rim_texture * 0.75
                    + body_texture * 0.35
                    + smoothness * 0.42
                    + radius_factor * 0.025;
                if score < best_score {
                    best_score = score;
                    best_off = Some((dst_cx - scx, dst_cy - scy));
                }
            }
        }
        best_off
    }

    /// Build the seamless-clone correction field for a heal dab (Poisson membrane,
    /// Pérez et al. 2003). Returns a `w·h·3` buffer of per-pixel RGB corrections
    /// (in 0..1 space) plus `(w, h)`. The healed pixel is `source + correction`:
    /// on the brush rim the correction equals `(dst − src)` so the patch meets the
    /// destination EXACTLY (no seam); inside, the correction is the smooth harmonic
    /// (Laplace) interpolation of that rim mismatch, so the source's texture/detail
    /// is kept while its colour and brightness bend to match the surroundings.
    /// This is what makes healing look "smart" vs a flat colour offset.
    #[allow(clippy::too_many_arguments)]
    fn heal_membrane(
        &self,
        canvas: &Canvas,
        dst_layer_idx: usize,
        lx0: u32,
        ly0: u32,
        lx1: u32,
        ly1: u32,
        lcx: f32,
        lcy: f32,
        r: f32,
        r2: f32,
    ) -> (Vec<f32>, usize, usize) {
        let w = (lx1 - lx0) as usize;
        let h = (ly1 - ly0) as usize;
        let mut c = vec![0f32; w * h * 3];
        if w == 0 || h == 0 {
            return (c, w, h);
        }
        let Some(src) = self.stroke_source.as_ref() else {
            return (c, w, h);
        };
        let layer = &canvas.layer_stack.layers[dst_layer_idx];
        let dst_ox = layer.offset.0;
        let dst_oy = layer.offset.1;
        let dst_tiles = &layer.tiles;
        let merged = self.stroke_source_merged;
        let mut active = vec![false; w * h];
        let mut fixed = vec![false; w * h];
        let rim = (r - 1.5).max(0.0);
        let rim2 = rim * rim;

        for py in ly0..ly1 {
            let dy = py as f32 + 0.5 - lcy;
            let dy2 = dy * dy;
            for px in lx0..lx1 {
                let dx = px as f32 + 0.5 - lcx;
                let d2 = dx * dx + dy2;
                if d2 > r2 {
                    continue;
                }
                let sx = (px as i32 + dst_ox) as f32 - self.offset_x;
                let sy = (py as i32 + dst_oy) as f32 - self.offset_y;
                if sx < 0.0 || sy < 0.0 {
                    continue;
                }
                let (sr, sg, sb, sa) = if merged {
                    src.get_pixel(sx as u32, sy as u32)
                } else {
                    let slx = sx as i32 - dst_ox;
                    let sly = sy as i32 - dst_oy;
                    if slx < 0 || sly < 0 {
                        continue;
                    }
                    src.get_pixel(slx as u32, sly as u32)
                };
                if sa == 0 {
                    continue;
                }
                let (dr, dg, db, da) = dst_tiles.get_pixel(px, py);
                if da == 0 {
                    continue;
                }
                let idx = (py - ly0) as usize * w + (px - lx0) as usize;
                c[idx * 3] = (dr as f32 - sr as f32) / 255.0;
                c[idx * 3 + 1] = (dg as f32 - sg as f32) / 255.0;
                c[idx * 3 + 2] = (db as f32 - sb as f32) / 255.0;
                active[idx] = true;
                fixed[idx] = d2 >= rim2;
            }
        }

        for yy in 0..h {
            let row = yy * w;
            for xx in 0..w {
                let idx = row + xx;
                if !active[idx] || fixed[idx] {
                    continue;
                }
                fixed[idx] = xx == 0
                    || yy == 0
                    || xx + 1 >= w
                    || yy + 1 >= h
                    || !active[idx - 1]
                    || !active[idx + 1]
                    || !active[idx - w]
                    || !active[idx + w];
            }
        }

        crate::core::smart_fill::solve_harmonic_rgb(&mut c, &active, &fixed, w, h, 1.0);
        (c, w, h)
    }
}

impl Tool for CloneTool {
    fn id(&self) -> &'static str {
        "clone_tool"
    }
    fn name(&self) -> &str {
        "Clone"
    }
    fn shortcut(&self) -> Option<char> {
        Some('S')
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::Clone
    }
    fn paints(&self) -> bool {
        true
    }
    fn cursor_size(&self) -> f32 {
        self.radius()
    }

    fn on_press(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        if self.smart_fill {
            if event.alt {
                return ToolResponse::none();
            }
            if !self.ca_begin(ctx.canvas()) {
                return ToolResponse::none();
            }
            self.ca_dab(event.canvas_x, event.canvas_y);
            return ToolResponse::none();
        }

        if event.alt {
            self.source_canvas_x = event.canvas_x;
            self.source_canvas_y = event.canvas_y;
            self.has_source = true;
            self.spot_auto = false;
            self.offset_set = false;
            self.stroke_source = None;
            return ToolResponse::none();
        }

        self.stroke = None;
        if !self.spot_mode && !self.has_source {
            return ToolResponse::blocked("Alt+click để chọn điểm lấy mẫu trước khi Clone");
        }
        if let Some(msg) = self.paint_block_reason(ctx.canvas()) {
            return ToolResponse::blocked(msg);
        }

        if self.spot_mode && !self.has_source {
            self.capture_source_for_stroke(ctx.canvas_mut());
            let Some((ox, oy)) =
                self.spot_pick_offset(ctx.canvas(), event.canvas_x, event.canvas_y)
            else {
                self.stroke_source = None;
                return ToolResponse::none();
            };
            self.offset_x = ox;
            self.offset_y = oy;
            self.offset_set = true;
            self.has_source = true;
            self.spot_auto = true;
            ctx.canvas_mut().begin_stroke("Spot Heal");
            self.stroke_start_x = event.canvas_x;
            self.stroke_start_y = event.canvas_y;
            self.stamp_dab(ctx.canvas_mut(), event.canvas_x, event.canvas_y);
            return ToolResponse::repaint();
        }

        self.capture_source_for_stroke(ctx.canvas_mut());
        if self.stroke_source.is_none() {
            return ToolResponse::none();
        }
        ctx.canvas_mut().begin_stroke("Clone");

        if !self.aligned || !self.offset_set {
            self.offset_x = event.canvas_x - self.source_canvas_x;
            self.offset_y = event.canvas_y - self.source_canvas_y;
            if self.uses_stroke_model() {
                // Whole pixels: every source pixel is copied as-is (no
                // half-pixel ambiguity), which the preview reproduces exactly.
                self.offset_x = self.offset_x.round();
                self.offset_y = self.offset_y.round();
            }
            if !self.aligned {
                self.offset_set = false;
            } else {
                self.offset_set = true;
            }
        }

        self.stroke_start_x = event.canvas_x;
        self.stroke_start_y = event.canvas_y;

        if self.uses_stroke_model() {
            // CMYK layer pixels go through the direct ink dab (like the Brush);
            // everything else uses the stroke buffer.
            let canvas = ctx.canvas();
            let ink_pixels = canvas.is_cmyk()
                && canvas.active_layer().paint_target == crate::core::layer::PaintTarget::Pixels;
            if !ink_pixels {
                self.stroke = StrokeBuffer::begin(canvas);
            }
            self.walk_residual = self.dab_spacing();
        }
        self.stamp(ctx.canvas_mut(), event.canvas_x, event.canvas_y);
        ToolResponse::repaint()
    }

    fn on_drag(
        &mut self,
        event: PointerEvent,
        prev: &PointerEvent,
        ctx: &mut ToolCtx,
    ) -> ToolResponse {
        if self.smart_fill {
            self.ca_segment(prev.canvas_x, prev.canvas_y, event.canvas_x, event.canvas_y);
            return ToolResponse::none();
        }
        if !self.has_source || self.stroke_source.is_none() {
            return ToolResponse::none();
        }
        if self.uses_stroke_model() {
            // Walk the path at a fixed spacing, carrying the leftover distance
            // between events: dab density (and so the soft edge) no longer
            // depends on how fast the mouse moves.
            let mut residual = self.walk_residual;
            let canvas = ctx.canvas_mut();
            BrushTool::walk_dabs(
                self.dab_spacing(),
                prev.canvas_x,
                prev.canvas_y,
                event.canvas_x,
                event.canvas_y,
                &mut residual,
                |x, y| self.stamp(canvas, x, y),
            );
            self.walk_residual = residual;
            return ToolResponse::repaint();
        }
        if self.spot_mode {
            if let Some((ox, oy)) =
                self.spot_pick_offset(ctx.canvas(), event.canvas_x, event.canvas_y)
            {
                self.offset_x = ox;
                self.offset_y = oy;
            }
        }
        self.stamp_segment(
            ctx.canvas_mut(),
            prev.canvas_x,
            prev.canvas_y,
            event.canvas_x,
            event.canvas_y,
        );
        ToolResponse::repaint()
    }

    fn on_release(&mut self, _event: PointerEvent, _ctx: &mut ToolCtx) -> ToolResponse {
        if self.smart_fill {
            return ToolResponse::none();
        }
        self.stroke_source = None;
        self.stroke = None;
        if self.spot_mode {
            self.has_source = false;
            self.spot_auto = false;
        }
        ToolResponse::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spot_heal_finds_source_when_half_the_brush_crosses_page_edge() {
        let canvas = Canvas::new(128, 128);
        let mut tool = CloneTool::new();
        tool.size = 32.0;
        tool.stroke_source = Some(canvas.active_layer().tiles.clone());

        assert!(
            tool.spot_pick_offset(&canvas, 0.0, 64.0).is_some(),
            "left-edge click must still find a clean source"
        );
        assert!(
            tool.spot_pick_offset(&canvas, 64.0, 0.0).is_some(),
            "top-edge click must still find a clean source"
        );
    }

    #[test]
    fn spot_heal_uses_only_visible_footprint_on_narrow_page() {
        // No 80 px diameter source circle fits inside this 64 px-wide page.
        // The right half of a dab centred on the left edge does fit, however,
        // and an offset along the tall axis provides a complete source for it.
        let canvas = Canvas::new(64, 256);
        let mut tool = CloneTool::new();
        tool.size = 80.0;
        tool.stroke_source = Some(canvas.active_layer().tiles.clone());

        assert!(
            tool.spot_pick_offset(&canvas, 0.0, 128.0).is_some(),
            "clipped destination footprint must not require a full source circle"
        );
    }

    use crate::core::document::{Document, DocumentId};

    fn doc_filled(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Document {
        let mut px = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                px[i..i + 4].copy_from_slice(&f(x, y));
            }
        }
        Document::from_canvas(DocumentId(1), Canvas::from_rgba(px, w, h), None)
    }

    /// Alt+click `src`, then drag horizontally from `from` to `to_x` with a
    /// mouse event every `step` px, then release and close the undo step.
    fn clone_stroke(
        tool: &mut CloneTool,
        doc: &mut Document,
        src: (f32, f32),
        from: (f32, f32),
        to_x: f32,
        step: f32,
    ) {
        let mut ctx = ToolCtx::new(doc, [0, 0, 0, 255], [255; 4], 1.0, 0.0, 0.0);
        let mut alt = PointerEvent::new(src.0, src.1);
        alt.alt = true;
        tool.on_press(alt, &mut ctx);
        let mut prev = PointerEvent::new(from.0, from.1);
        tool.on_press(prev, &mut ctx);
        let mut x = from.0 + step;
        while x <= to_x {
            let ev = PointerEvent::new(x, from.1);
            tool.on_drag(ev, &prev, &mut ctx);
            prev = ev;
            x += step;
        }
        tool.on_release(prev, &mut ctx);
        ctx.canvas_mut().end_stroke();
    }

    fn red_left_white_right() -> Document {
        doc_filled(400, 256, |x, _| {
            if x < 200 {
                [255, 0, 0, 255]
            } else {
                [255, 255, 255, 255]
            }
        })
    }

    fn red_coverage(doc: &Document, x: u32, y: u32) -> f32 {
        let (_, g, _, _) = doc.canvas.active_layer().tiles.get_pixel(x, y);
        1.0 - g as f32 / 255.0
    }

    #[test]
    fn opacity_caps_one_stroke_and_stacks_across_strokes() {
        let mut doc = red_left_white_right();
        let mut tool = CloneTool::new();
        tool.hardness = 1.0;
        tool.opacity = 0.3;
        // A slow stroke (1 px per event) must still stop at 30 %.
        clone_stroke(
            &mut tool,
            &mut doc,
            (100.0, 128.0),
            (250.0, 128.0),
            340.0,
            1.0,
        );
        let one = red_coverage(&doc, 300, 128);
        assert!((one - 0.3).abs() < 0.02, "one stroke at 30% gave {one}");
        clone_stroke(
            &mut tool,
            &mut doc,
            (100.0, 128.0),
            (250.0, 128.0),
            340.0,
            1.0,
        );
        let two = red_coverage(&doc, 300, 128);
        assert!(
            (two - 0.51).abs() < 0.02,
            "second stroke should stack: {two}"
        );
    }

    #[test]
    fn soft_edge_does_not_depend_on_mouse_speed() {
        let edge_at = |step: f32| {
            let mut doc = red_left_white_right();
            let mut tool = CloneTool::new();
            tool.hardness = 0.0;
            clone_stroke(
                &mut tool,
                &mut doc,
                (100.0, 128.0),
                (250.0, 128.0),
                340.0,
                step,
            );
            red_coverage(&doc, 300, 128 + 24)
        };
        let (slow, fast) = (edge_at(1.0), edge_at(12.0));
        assert!(
            (slow - fast).abs() < 0.06,
            "soft edge changed with mouse speed: slow {slow}, fast {fast}"
        );
    }

    #[test]
    fn cloning_a_colour_over_itself_never_drifts() {
        let mut doc = doc_filled(400, 256, |_, _| [201, 147, 99, 255]);
        let mut tool = CloneTool::new();
        tool.opacity = 0.7;
        for y in [120.0, 124.0, 128.0, 132.0, 136.0] {
            clone_stroke(&mut tool, &mut doc, (100.0, y), (200.0, y), 350.0, 2.0);
        }
        for y in 80..180 {
            for x in 150..390 {
                assert_eq!(
                    doc.canvas.active_layer().tiles.get_pixel(x, y),
                    (201, 147, 99, 255),
                    "pixel ({x},{y}) drifted"
                );
            }
        }
    }

    #[test]
    fn sample_merged_clones_the_composite() {
        let mut doc = red_left_white_right();
        let mut tool = CloneTool::new();
        tool.hardness = 1.0;
        tool.sample_merged = true;
        clone_stroke(
            &mut tool,
            &mut doc,
            (100.0, 128.0),
            (250.0, 128.0),
            340.0,
            3.0,
        );
        assert!(red_coverage(&doc, 300, 128) > 0.99);
    }

    #[test]
    fn clone_on_layer_mask_edits_the_mask_and_undoes() {
        let mut doc = red_left_white_right();
        {
            let layer = &mut doc.canvas.layer_stack.layers[0];
            layer.add_mask(true);
            let mask = layer.mask.as_mut().unwrap();
            for y in 0..256 {
                for x in 0..200 {
                    mask.tiles.set_pixel(x, y, 0, 0, 0, 255);
                }
            }
        }
        let pixel_before = doc.canvas.active_layer().tiles.get_pixel(300, 128);
        let mut tool = CloneTool::new();
        tool.hardness = 1.0;
        clone_stroke(
            &mut tool,
            &mut doc,
            (100.0, 128.0),
            (250.0, 128.0),
            340.0,
            3.0,
        );

        let layer = doc.canvas.active_layer();
        assert_eq!(layer.mask.as_ref().unwrap().tiles.get_pixel(300, 128).0, 0);
        assert_eq!(
            layer.tiles.get_pixel(300, 128),
            pixel_before,
            "pixels must not change"
        );

        doc.canvas.undo();
        let mask = doc.canvas.active_layer().mask.as_ref().unwrap();
        assert_eq!(
            mask.tiles.get_pixel(300, 128).0,
            255,
            "undo must restore the mask"
        );
    }

    #[test]
    fn clone_without_source_explains_alt_click() {
        let mut doc = red_left_white_right();
        let mut tool = CloneTool::new();
        let mut ctx = ToolCtx::new(&mut doc, [0, 0, 0, 255], [255; 4], 1.0, 0.0, 0.0);
        let resp = tool.on_press(PointerEvent::new(300.0, 128.0), &mut ctx);
        assert!(resp.status.is_some());
    }
}

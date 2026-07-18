#![allow(dead_code)]
use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::canvas::Canvas;

#[derive(Debug, Clone)]
pub struct BrushSettings {
    pub size: f32,
    pub hardness: f32,
    pub opacity: f32,
    pub color: [u8; 4],
    pub spacing: f32,
    pub is_eraser: bool,
    /// Stroke smoothing strength: 0.0 = off, 1.0 = max (EMA factor).
    pub smoothing: f32,
    /// Per-dab paint amount (standard raster editors "Flow"): 0.0..1.0. Lower values build up
    /// gradually as overlapping dabs accumulate within a stroke (airbrush feel).
    pub flow: f32,
}

/// A named brush preset — a bundle of tip parameters selectable from the
/// right-click brush popup or the options-bar dropdown (like the standard
/// brush picker). Size / opacity / colour stay under the user's control.
#[derive(Clone, Debug)]
pub struct BrushPreset {
    pub name: &'static str,
    pub hardness: f32,
    pub spacing: f32,
    pub flow: f32,
}

/// Built-in basic brushes. Soft round tips use a full radial-gradient falloff;
/// hard round keeps a crisp anti-aliased edge.
pub const BRUSH_PRESETS: &[BrushPreset] = &[
    BrushPreset {
        name: "Soft Round",
        hardness: 0.0,
        spacing: 0.10,
        flow: 0.65,
    },
    BrushPreset {
        name: "Hard Round",
        hardness: 1.0,
        spacing: 0.10,
        flow: 1.0,
    },
    BrushPreset {
        name: "Medium Round",
        hardness: 0.5,
        spacing: 0.12,
        flow: 1.0,
    },
    BrushPreset {
        name: "Soft Low Flow",
        hardness: 0.0,
        spacing: 0.05,
        flow: 0.30,
    },
    BrushPreset {
        name: "Hard Edge Sketch",
        hardness: 0.9,
        spacing: 0.05,
        flow: 1.0,
    },
    BrushPreset {
        name: "Airbrush Soft",
        hardness: 0.0,
        spacing: 0.04,
        flow: 0.12,
    },
];

/// Sentinel for "no preset selected" (custom settings).
pub const PRESET_CUSTOM: usize = usize::MAX;

#[derive(Clone, Debug)]
pub struct BrushDab {
    pub cx: f32,
    pub cy: f32,
    pub radius: f32,
    pub hardness: f32,
    pub color: [f32; 4],
}

#[inline]
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0).max(0.0001)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Quintic smootherstep on [0,1]: zero 1st AND 2nd derivative at both ends.
#[inline]
fn smootherstep01(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

#[inline]
pub(crate) fn soft_round_alpha(dist2: f32, radius: f32, hardness: f32) -> f32 {
    if radius <= 0.0 {
        return 0.0;
    }

    let t = (dist2.sqrt() / radius).clamp(0.0, 1.0);
    let hardness = hardness.clamp(0.0, 1.0);
    if hardness >= 0.999 {
        // ~1px anti-aliasing band in absolute pixels: the old relative
        // 0.985..1 band collapsed to a fraction of a pixel on small tips
        // (jagged edges) and grew to several pixels on huge ones.
        let band = (1.0 / radius).min(1.0);
        return 1.0 - smoothstep(1.0 - band, 1.0, t);
    }

    let core = (hardness * 0.92).clamp(0.0, 0.92);
    if t <= core {
        1.0
    } else {
        // C2 bell falloff from the solid core to the rim: flat at both ends,
        // so there is no cone tip in the centre (old hardness-0 profile was
        // linear 1-t) and no visible ring where the dab meets the canvas.
        let ft = ((t - core) / (1.0 - core)).clamp(0.0, 1.0);
        1.0 - smootherstep01(ft)
    }
}

impl BrushSettings {
    /// Distance between dab centres along the stroke path, in canvas px.
    #[inline]
    pub fn dab_spacing(&self) -> f32 {
        (self.size * self.spacing).max(0.5)
    }
}

impl Default for BrushSettings {
    fn default() -> Self {
        Self {
            size: 20.0,
            hardness: 0.8,
            opacity: 1.0,
            color: [0, 0, 0, 255],
            spacing: 0.25,
            is_eraser: false,
            smoothing: 0.0,
            flow: 1.0,
        }
    }
}

/// Per-stroke paint state implementing the PS opacity/flow model: each dab
/// accumulates FLOW into this coverage buffer, and the stroke is composited
/// over the layer as it was at stroke start with OPACITY as a single ceiling.
/// Overlapping dabs inside one stroke therefore never exceed the stroke
/// opacity, while separate strokes still stack on each other.
pub struct StrokeBuffer {
    /// Distance to the next dab along the stroke path (walker state).
    pub residual: f32,
    /// Whether this stroke paints the layer mask (captured at stroke start —
    /// the target can't change mid-stroke).
    paint_mask: bool,
    /// Target tiles (layer pixels or mask) as they were when the stroke began.
    /// Arc clones: costs memory only for tiles the stroke actually touches.
    base: crate::core::tile::TileMap,
    /// Accumulated flow coverage per layer-local pixel, tile-keyed.
    cov: std::collections::HashMap<crate::core::tile::TilePos, Box<[f32]>>,
}

impl StrokeBuffer {
    /// Snapshot the active layer's paint target. Returns None when the layer
    /// has no paintable target (e.g. mask targeted but no mask attached).
    pub fn begin(canvas: &Canvas) -> Option<Self> {
        let idx = canvas.layer_stack.active_idx;
        let layer = canvas.layer_stack.layers.get(idx)?;
        let paint_mask = layer.paint_target == crate::core::layer::PaintTarget::Mask;
        let base = if paint_mask {
            layer.mask.as_ref()?.tiles.clone()
        } else {
            layer.tiles.clone()
        };
        Some(Self {
            residual: 0.0,
            paint_mask,
            base,
            cov: std::collections::HashMap::new(),
        })
    }
}

pub struct BrushTool {
    pub settings: BrushSettings,
    /// Index into [`BRUSH_PRESETS`] of the active preset, or [`PRESET_CUSTOM`]
    /// when the tip params were edited by hand.
    pub preset_idx: usize,
    pub pending_dabs: Vec<BrushDab>,
    pub stroke_dabs: Vec<BrushDab>,
    last_pos: (f32, f32),
    last_pressure: f32,
    /// EMA-smoothed position — updated on each drag event.
    smoothed_x: f32,
    smoothed_y: f32,
    /// Distance left until the next dab, carried across drag events
    /// (see [`Self::paint_cpu_stroke_segment`]).
    stroke_residual: f32,
    /// Per-stroke coverage buffer (PS opacity/flow model); Some while the
    /// pointer is down.
    stroke: Option<StrokeBuffer>,
}

impl BrushTool {
    pub fn new() -> Self {
        Self {
            settings: BrushSettings::default(),
            preset_idx: PRESET_CUSTOM,
            pending_dabs: Vec::new(),
            stroke_dabs: Vec::new(),
            last_pos: (0.0, 0.0),
            last_pressure: 0.0,
            smoothed_x: 0.0,
            smoothed_y: 0.0,
            stroke_residual: 0.0,
            stroke: None,
        }
    }

    /// Apply a built-in preset's tip parameters (hardness / spacing / flow).
    /// Size, opacity and colour are intentionally preserved.
    pub fn apply_preset(&mut self, idx: usize) {
        if let Some(p) = BRUSH_PRESETS.get(idx) {
            self.settings.hardness = p.hardness;
            self.settings.spacing = p.spacing;
            self.settings.flow = p.flow;
            self.preset_idx = idx;
        }
    }

    pub fn paint_dab(&mut self, canvas: &mut Canvas, cx: f32, cy: f32) {
        let r = (self.settings.size * 0.5).max(0.5).min(5000.0);

        let [br, bg, bb, ba] = self.settings.color;
        let color = [
            br as f32 / 255.0,
            bg as f32 / 255.0,
            bb as f32 / 255.0,
            (ba as f32 / 255.0) * self.settings.opacity,
        ];

        let dab = BrushDab {
            cx,
            cy,
            radius: r,
            hardness: self.settings.hardness,
            color,
        };
        self.pending_dabs.push(dab.clone());
        self.stroke_dabs.push(dab);

        let layer_idx = canvas.layer_stack.active_idx;
        let ox = canvas.layer_stack.layers[layer_idx].offset.0;
        let oy = canvas.layer_stack.layers[layer_idx].offset.1;
        let lw = canvas.layer_stack.layers[layer_idx].width;
        let lh = canvas.layer_stack.layers[layer_idx].height;

        let lx0 = ox.max(0) as u32;
        let ly0 = oy.max(0) as u32;
        let lx1 = (ox + lw as i32).clamp(0, canvas.width as i32) as u32;
        let ly1 = (oy + lh as i32).clamp(0, canvas.height as i32) as u32;

        let x0 = ((cx - r).floor().max(0.0) as u32).max(lx0);
        let y0 = ((cy - r).floor().max(0.0) as u32).max(ly0);
        let x1 = ((cx + r).ceil() as u32).min(canvas.width).min(lx1);
        let y1 = ((cy + r).ceil() as u32).min(canvas.height).min(ly1);

        if x1 > x0 && y1 > y0 {
            canvas.mark_dirty(x0, y0, x1, y1);
        }
    }
    /// Direct (unbuffered) dab: alpha = opacity × flow per dab, dabs composite
    /// straight onto the layer. Used by tools without a per-stroke opacity
    /// model (pencil) and by one-shot stamps.
    pub fn paint_cpu_dab(settings: &BrushSettings, canvas: &mut Canvas, cx: f32, cy: f32) {
        Self::paint_cpu_dab_impl(settings, canvas, cx, cy, None)
    }

    /// Buffered dab (PS model): flow accumulates in `stroke`'s coverage buffer
    /// and the result is composited from the stroke-start snapshot with the
    /// stroke opacity as ceiling. See [`StrokeBuffer`].
    pub fn paint_cpu_dab_stroked(
        settings: &BrushSettings,
        canvas: &mut Canvas,
        cx: f32,
        cy: f32,
        stroke: &mut StrokeBuffer,
    ) {
        Self::paint_cpu_dab_impl(settings, canvas, cx, cy, Some(stroke))
    }

    fn paint_cpu_dab_impl(
        settings: &BrushSettings,
        canvas: &mut Canvas,
        cx: f32,
        cy: f32,
        mut stroke: Option<&mut StrokeBuffer>,
    ) {
        if Self::paint_alpha_plane_dab(settings, canvas, cx, cy) {
            return;
        }

        // CMYK document: blend the colour's ink into the active layer's ink
        // planes (direct dab), then re-project the mirror. Skipped when the
        // stroke targets a grayscale layer mask (masks carry no ink). The
        // buffered/direct RGB machinery below is bypassed entirely.
        if canvas.is_cmyk() {
            let layer_idx = canvas.layer_stack.active_idx;
            let painting_mask = match &stroke {
                Some(s) => s.paint_mask,
                None => {
                    canvas.layer_stack.layers[layer_idx].paint_target
                        == crate::core::layer::PaintTarget::Mask
                }
            };
            if !painting_mask {
                Self::paint_ink_dab(settings, canvas, cx, cy);
                return;
            }
        }

        let r = (settings.size * 0.5).max(0.5).min(2000.0);
        let r2 = r * r;
        let opacity = settings.opacity.clamp(0.0, 1.0);
        let [br, bg, bb, ba] = settings.color;
        let ba_f = ba as f32 / 255.0;

        let layer_idx = canvas.layer_stack.active_idx;
        let ox = canvas.layer_stack.layers[layer_idx].offset.0;
        let oy = canvas.layer_stack.layers[layer_idx].offset.1;
        let lw = canvas.layer_stack.layers[layer_idx].width;
        let lh = canvas.layer_stack.layers[layer_idx].height;

        let lcx = cx - ox as f32;
        let lcy = cy - oy as f32;

        let lx0 = ((lcx - r).floor().max(0.0) as u32).min(lw);
        let ly0 = ((lcy - r).floor().max(0.0) as u32).min(lh);
        let lx1 = ((lcx + r).ceil() as u32).min(lw);
        let ly1 = ((lcy + r).ceil() as u32).min(lh);

        if lx1 <= lx0 || ly1 <= ly0 {
            return;
        }

        let buffered = stroke.is_some();
        // Buffered: opacity is a per-stroke ceiling applied at composite time,
        // so only flow feeds the per-dab accumulation. Direct: legacy per-dab
        // alpha = opacity × flow.
        let max_src_a = if buffered {
            ba_f * settings.flow.clamp(0.0, 1.0)
        } else {
            ba_f * opacity * settings.flow.clamp(0.0, 1.0)
        };
        if max_src_a < 0.001 || (buffered && opacity < 0.001) {
            return;
        }
        let src_r = br as f32 / 255.0;
        let src_g = bg as f32 / 255.0;
        let src_b = bb as f32 / 255.0;
        let mask_target = (src_r * 0.2126 + src_g * 0.7152 + src_b * 0.0722).clamp(0.0, 1.0);

        const FALLOFF_LUT_N: usize = 1024;
        let inv_r2 = if r2 > 0.0 { 1.0 / r2 } else { 0.0 };
        let lut_scale = (FALLOFF_LUT_N - 1) as f32;
        let mut falloff_lut = [0.0f32; FALLOFF_LUT_N];
        for (k, slot) in falloff_lut.iter_mut().enumerate() {
            let d2k = (k as f32 / lut_scale) * r2;
            *slot = soft_round_alpha(d2k, r, settings.hardness);
        }

        let tile_size = crate::core::tile::TILE_SIZE;

        let has_sel = canvas.selection.active;
        let sel_ptr: *const crate::core::selection::Selection = &canvas.selection;
        let paint_target = canvas.layer_stack.layers[layer_idx].paint_target;
        let paint_mask = match &stroke {
            Some(s) => s.paint_mask,
            None => paint_target == crate::core::layer::PaintTarget::Mask,
        };
        let lock_alpha = paint_target == crate::core::layer::PaintTarget::Pixels
            && canvas.layer_stack.layers[layer_idx].lock_alpha;
        // Channels-panel write gate: with a partial channel selection the dab
        // paints the colour's luma into the enabled channels only (alpha
        // untouched). Mask painting is grayscale and ignores the gate.
        let channel_wm = if paint_mask {
            None
        } else {
            canvas.channels.write_gate()
        };
        let Some(tiles) = canvas.layer_stack.layers[layer_idx].get_paint_tiles_mut() else {
            return;
        };

        let is_eraser = settings.is_eraser;

        let tx_start = lx0 / tile_size;
        let tx_end = lx1 / tile_size;
        let ty_start = ly0 / tile_size;
        let ty_end = ly1 / tile_size;
        let n_tiles = (((tx_end - tx_start) + 1) * ((ty_end - ty_start) + 1)) as usize;
        tiles.tiles.reserve(n_tiles);

        #[derive(Clone, Copy)]
        struct TileJob {
            ptr: usize,
            /// Coverage buffer for this tile (0 when unbuffered).
            cov_ptr: usize,
            /// Stroke-start snapshot tile (0 = tile was empty at stroke start).
            base_ptr: usize,
            tile_x0: u32,
            tile_y0: u32,
        }

        let tile_pixels = (tile_size * tile_size) as usize;
        let mut jobs: Vec<TileJob> = Vec::with_capacity(n_tiles);
        for ty in ty_start..=ty_end {
            let tile_y0 = ty * tile_size;
            let py_min = tile_y0.max(ly0);
            let py_max = (tile_y0 + tile_size).min(ly1);
            if py_max <= py_min {
                continue;
            }
            let ny = lcy.clamp(py_min as f32 + 0.5, py_max as f32 - 0.5);
            let ndy2 = (ny - lcy) * (ny - lcy);
            if ndy2 > r2 {
                continue;
            }
            for tx in tx_start..=tx_end {
                let tile_x0 = tx * tile_size;
                let px_min = tile_x0.max(lx0);
                let px_max = (tile_x0 + tile_size).min(lx1);
                if px_max <= px_min {
                    continue;
                }
                let nx = lcx.clamp(px_min as f32 + 0.5, px_max as f32 - 0.5);
                if (nx - lcx) * (nx - lcx) + ndy2 > r2 {
                    continue;
                }
                let pos = crate::core::tile::TilePos {
                    x: tx as i32,
                    y: ty as i32,
                };
                let tile = tiles.get_tile_mut(pos);
                let ptr = tile as *mut crate::core::tile::Tile as usize;
                let (cov_ptr, base_ptr) = match stroke.as_mut() {
                    Some(s) => {
                        let cov = s
                            .cov
                            .entry(pos)
                            .or_insert_with(|| vec![0.0f32; tile_pixels].into_boxed_slice());
                        let base_ptr = s
                            .base
                            .tiles
                            .get(&pos)
                            .map(|a| std::sync::Arc::as_ptr(a) as usize)
                            .unwrap_or(0);
                        (cov.as_mut_ptr() as usize, base_ptr)
                    }
                    None => (0, 0),
                };
                jobs.push(TileJob {
                    ptr,
                    cov_ptr,
                    base_ptr,
                    tile_x0,
                    tile_y0,
                });
            }
        }

        let sel_addr = sel_ptr as usize;

        let fill = |job: &TileJob| {
            let tile = unsafe { &mut *(job.ptr as *mut crate::core::tile::Tile) };
            let tile_x0 = job.tile_x0;
            let tile_y0 = job.tile_y0;
            let px_min = tile_x0.max(lx0);
            let py_min = tile_y0.max(ly0);
            let px_max = (tile_x0 + tile_size).min(lx1);
            let py_max = (tile_y0 + tile_size).min(ly1);
            if px_max <= px_min || py_max <= py_min {
                return;
            }
            unsafe {
                for py in py_min..py_max {
                    let dy = py as f32 + 0.5 - lcy;
                    let dy2 = dy * dy;
                    let row_offset = (py - tile_y0) * tile_size;
                    let canvas_py = (py as i32 + oy) as u32;

                    for px in px_min..px_max {
                        let dx = px as f32 + 0.5 - lcx;
                        let d2 = dx * dx + dy2;
                        if d2 > r2 {
                            continue;
                        }
                        let idx = ((d2 * inv_r2) * lut_scale) as usize;
                        let alpha = *falloff_lut.get_unchecked(idx.min(FALLOFF_LUT_N - 1));

                        let mut src_a = alpha * max_src_a;
                        if src_a < 0.001 {
                            continue;
                        }

                        if has_sel {
                            let canvas_px = (px as i32 + ox) as u32;
                            let sel = &*(sel_addr as *const crate::core::selection::Selection);
                            src_a *= sel.sample(canvas_px, canvas_py);
                            if src_a < 0.001 {
                                continue;
                            }
                        }

                        let col = px - tile_x0;
                        let i = ((row_offset + col) * 4) as usize;

                        if let Some(wm) = channel_wm {
                            // Partial channel selection: paint the colour's
                            // luma into the enabled channels (the eraser
                            // carries the background colour here). Alpha and
                            // the other channels stay — buffered dabs rewrite
                            // them from the stroke-start base, which keeps
                            // overlapping dabs idempotent.
                            if buffered {
                                let cov = &mut *((job.cov_ptr as *mut f32)
                                    .add((row_offset + col) as usize));
                                *cov += src_a * (1.0 - *cov);
                                let s = (*cov * opacity).clamp(0.0, 1.0);
                                let (b_r, b_g, b_b, b_a) = if job.base_ptr != 0 {
                                    (*(job.base_ptr as *const crate::core::tile::Tile))
                                        .get_pixel(col, py - tile_y0)
                                } else {
                                    (0, 0, 0, 0)
                                };
                                let base = [b_r, b_g, b_b];
                                for (c, &b8) in base.iter().enumerate() {
                                    let out = if wm[c] {
                                        let b = b8 as f32 / 255.0;
                                        ((b + (mask_target - b) * s) * 255.0).round() as u8
                                    } else {
                                        b8
                                    };
                                    *tile.pixels.get_unchecked_mut(i + c) = out;
                                }
                                *tile.pixels.get_unchecked_mut(i + 3) = b_a;
                            } else {
                                for c in 0..3 {
                                    if wm[c] {
                                        let d = *tile.pixels.get_unchecked(i + c) as f32 / 255.0;
                                        *tile.pixels.get_unchecked_mut(i + c) =
                                            ((d + (mask_target - d) * src_a) * 255.0).round() as u8;
                                    }
                                }
                            }
                            continue;
                        }

                        if buffered {
                            // PS model: accumulate flow into the stroke's
                            // coverage, then composite base ⊕ (color, cov ×
                            // opacity). Re-deriving from the stroke-start
                            // snapshot on every dab makes overlap idempotent —
                            // one stroke can never exceed its opacity.
                            let cov =
                                &mut *((job.cov_ptr as *mut f32).add((row_offset + col) as usize));
                            *cov += src_a * (1.0 - *cov);
                            let s = (*cov * opacity).clamp(0.0, 1.0);
                            let (b_r, b_g, b_b, b_a) = if job.base_ptr != 0 {
                                (*(job.base_ptr as *const crate::core::tile::Tile))
                                    .get_pixel(col, py - tile_y0)
                            } else {
                                (0, 0, 0, 0)
                            };

                            if paint_mask {
                                let bgray = b_r as f32 / 255.0;
                                let out = bgray + (mask_target - bgray) * s;
                                let out8 = (out * 255.0).round().clamp(0.0, 255.0) as u8;
                                *tile.pixels.get_unchecked_mut(i) = out8;
                                *tile.pixels.get_unchecked_mut(i + 1) = out8;
                                *tile.pixels.get_unchecked_mut(i + 2) = out8;
                                *tile.pixels.get_unchecked_mut(i + 3) = 255;
                            } else if is_eraser {
                                let base_a = b_a as f32 / 255.0;
                                let out_a = base_a * (1.0 - s);
                                if out_a < 0.001 {
                                    *tile.pixels.get_unchecked_mut(i) = 0;
                                    *tile.pixels.get_unchecked_mut(i + 1) = 0;
                                    *tile.pixels.get_unchecked_mut(i + 2) = 0;
                                    *tile.pixels.get_unchecked_mut(i + 3) = 0;
                                } else {
                                    *tile.pixels.get_unchecked_mut(i) = b_r;
                                    *tile.pixels.get_unchecked_mut(i + 1) = b_g;
                                    *tile.pixels.get_unchecked_mut(i + 2) = b_b;
                                    *tile.pixels.get_unchecked_mut(i + 3) =
                                        (out_a * 255.0).round() as u8;
                                }
                            } else {
                                let base_a = b_a as f32 / 255.0;
                                let dst_weight = base_a * (1.0 - s);
                                if lock_alpha {
                                    if base_a < 0.001 {
                                        continue;
                                    }
                                    let denom = s + dst_weight;
                                    if denom < 0.001 {
                                        continue;
                                    }
                                    let base_r = b_r as f32 / 255.0;
                                    let base_g = b_g as f32 / 255.0;
                                    let base_b = b_b as f32 / 255.0;
                                    let out_r = (src_r * s + base_r * dst_weight) / denom;
                                    let out_g = (src_g * s + base_g * dst_weight) / denom;
                                    let out_b = (src_b * s + base_b * dst_weight) / denom;
                                    *tile.pixels.get_unchecked_mut(i) =
                                        (out_r * 255.0).round() as u8;
                                    *tile.pixels.get_unchecked_mut(i + 1) =
                                        (out_g * 255.0).round() as u8;
                                    *tile.pixels.get_unchecked_mut(i + 2) =
                                        (out_b * 255.0).round() as u8;
                                    *tile.pixels.get_unchecked_mut(i + 3) = b_a;
                                } else {
                                    let out_a = s + dst_weight;
                                    if out_a < 0.001 {
                                        continue;
                                    }
                                    let base_r = b_r as f32 / 255.0;
                                    let base_g = b_g as f32 / 255.0;
                                    let base_b = b_b as f32 / 255.0;
                                    let out_r = (src_r * s + base_r * dst_weight) / out_a;
                                    let out_g = (src_g * s + base_g * dst_weight) / out_a;
                                    let out_b = (src_b * s + base_b * dst_weight) / out_a;
                                    *tile.pixels.get_unchecked_mut(i) =
                                        (out_r * 255.0).round() as u8;
                                    *tile.pixels.get_unchecked_mut(i + 1) =
                                        (out_g * 255.0).round() as u8;
                                    *tile.pixels.get_unchecked_mut(i + 2) =
                                        (out_b * 255.0).round() as u8;
                                    *tile.pixels.get_unchecked_mut(i + 3) =
                                        (out_a * 255.0).round() as u8;
                                }
                            }
                            continue;
                        }

                        if paint_mask {
                            // Standard source-over lerp so repeated strokes keep
                            // moving the mask toward the brush value (the old
                            // max/min cap froze low-opacity paint at target*a
                            // forever — soft brushes could never cover).
                            let dst = *tile.pixels.get_unchecked(i) as f32 / 255.0;
                            let out = dst + (mask_target - dst) * src_a;
                            let out8 = (out * 255.0).round().clamp(0.0, 255.0) as u8;
                            *tile.pixels.get_unchecked_mut(i) = out8;
                            *tile.pixels.get_unchecked_mut(i + 1) = out8;
                            *tile.pixels.get_unchecked_mut(i + 2) = out8;
                            *tile.pixels.get_unchecked_mut(i + 3) = 255;
                            continue;
                        }

                        if is_eraser {
                            let dst_a8 = *tile.pixels.get_unchecked(i + 3);
                            if dst_a8 == 0 {
                                continue;
                            }
                            let dst_a = dst_a8 as f32 / 255.0;
                            let out_a = (dst_a - src_a).clamp(0.0, 1.0);
                            if out_a < 0.001 {
                                *tile.pixels.get_unchecked_mut(i) = 0;
                                *tile.pixels.get_unchecked_mut(i + 1) = 0;
                                *tile.pixels.get_unchecked_mut(i + 2) = 0;
                                *tile.pixels.get_unchecked_mut(i + 3) = 0;
                            } else {
                                *tile.pixels.get_unchecked_mut(i + 3) =
                                    (out_a * 255.0).round() as u8;
                            }
                        } else {
                            let dst_a8 = *tile.pixels.get_unchecked(i + 3);

                            if src_a >= 0.999 && !lock_alpha {
                                *tile.pixels.get_unchecked_mut(i) = br;
                                *tile.pixels.get_unchecked_mut(i + 1) = bg;
                                *tile.pixels.get_unchecked_mut(i + 2) = bb;
                                *tile.pixels.get_unchecked_mut(i + 3) = 255;
                                continue;
                            }

                            let dst_a = dst_a8 as f32 / 255.0;
                            let inv_src_a = 1.0 - src_a;
                            let dst_weight = dst_a * inv_src_a;
                            let mut out_a = src_a + dst_weight;

                            if lock_alpha {
                                out_a = dst_a;
                            }

                            if out_a < 0.001 {
                                continue;
                            }

                            let dst_r8 = *tile.pixels.get_unchecked(i);
                            let dst_g8 = *tile.pixels.get_unchecked(i + 1);
                            let dst_b8 = *tile.pixels.get_unchecked(i + 2);

                            let dst_r = dst_r8 as f32 / 255.0;
                            let dst_g = dst_g8 as f32 / 255.0;
                            let dst_b = dst_b8 as f32 / 255.0;

                            let denom = src_a + dst_weight;
                            if denom < 0.001 {
                                continue;
                            }
                            let out_r = (src_r * src_a + dst_r * dst_weight) / denom;
                            let out_g = (src_g * src_a + dst_g * dst_weight) / denom;
                            let out_b = (src_b * src_a + dst_b * dst_weight) / denom;

                            // .round() — plain truncation drifts every blend
                            // toward black/transparent, leaving dark streaks.
                            *tile.pixels.get_unchecked_mut(i) = (out_r * 255.0).round() as u8;
                            *tile.pixels.get_unchecked_mut(i + 1) = (out_g * 255.0).round() as u8;
                            *tile.pixels.get_unchecked_mut(i + 2) = (out_b * 255.0).round() as u8;
                            *tile.pixels.get_unchecked_mut(i + 3) = (out_a * 255.0).round() as u8;
                        }
                    }
                }
            }
        };

        if jobs.len() >= 4 {
            use rayon::prelude::*;
            jobs.par_iter().for_each(|job| fill(job));
        } else {
            jobs.iter().for_each(|job| fill(job));
        }
        let canvas_x0 = (lx0 as i32 + ox).max(0) as u32;
        let canvas_y0 = (ly0 as i32 + oy).max(0) as u32;
        let canvas_x1 = ((lx1 as i32 + ox) as u32).min(canvas.width);
        let canvas_y1 = ((ly1 as i32 + oy) as u32).min(canvas.height);
        canvas.mark_dirty(canvas_x0, canvas_y0, canvas_x1, canvas_y1);
    }

    /// Paint one direct (unbuffered) ink dab onto the active layer of a CMYK
    /// document. The foreground colour is converted to ink once, blended into
    /// each pixel's CMYK plane by coverage (source-over on the mirror's alpha),
    /// and the touched pixels' RGB mirror is re-projected from the new ink so
    /// display/compositing stay correct. Erasers lower the mirror alpha and
    /// clear the ink where fully erased. `lock_alpha` paints only where the
    /// layer is already opaque and preserves alpha, matching the RGB brush.
    ///
    /// When the Channels panel selects a subset of C/M/Y/K plates (`write_gate`),
    /// the dab switches to *plate paint*: only the enabled ink channels move
    /// (toward the colour's ink, or toward 0 for the eraser) and alpha stays —
    /// mirroring the RGB channel-gate semantics. Undo rides the stroke's
    /// DeltaSnapshot (writes go through the tiles' COW).
    fn paint_ink_dab(settings: &BrushSettings, canvas: &mut Canvas, cx: f32, cy: f32) {
        let Some(conv) = canvas.cmyk_converter() else {
            return;
        };
        let plate_gate = canvas.channels.write_gate();
        let [br, bg, bb, ba] = settings.color;
        let fg_ink = conv.rgb_to_cmyk_one([br, bg, bb]);
        let is_eraser = settings.is_eraser;
        let max_src_a =
            (ba as f32 / 255.0) * settings.opacity.clamp(0.0, 1.0) * settings.flow.clamp(0.0, 1.0);
        if max_src_a < 0.001 {
            return;
        }

        let r = (settings.size * 0.5).max(0.5).min(2000.0);
        let r2 = r * r;

        let layer_idx = canvas.layer_stack.active_idx;
        let ox = canvas.layer_stack.layers[layer_idx].offset.0;
        let oy = canvas.layer_stack.layers[layer_idx].offset.1;
        let lw = canvas.layer_stack.layers[layer_idx].width;
        let lh = canvas.layer_stack.layers[layer_idx].height;
        let lock_alpha = canvas.layer_stack.layers[layer_idx].lock_alpha;

        let lcx = cx - ox as f32;
        let lcy = cy - oy as f32;
        let lx0 = ((lcx - r).floor().max(0.0) as u32).min(lw);
        let ly0 = ((lcy - r).floor().max(0.0) as u32).min(lh);
        let lx1 = ((lcx + r).ceil() as u32).min(lw);
        let ly1 = ((lcy + r).ceil() as u32).min(lh);
        if lx1 <= lx0 || ly1 <= ly0 {
            return;
        }

        const FALLOFF_LUT_N: usize = 1024;
        let inv_r2 = if r2 > 0.0 { 1.0 / r2 } else { 0.0 };
        let lut_scale = (FALLOFF_LUT_N - 1) as f32;
        let mut falloff_lut = [0.0f32; FALLOFF_LUT_N];
        for (k, slot) in falloff_lut.iter_mut().enumerate() {
            let d2k = (k as f32 / lut_scale) * r2;
            *slot = soft_round_alpha(d2k, r, settings.hardness);
        }

        let has_sel = canvas.selection.active;
        let sel_ptr: *const crate::core::selection::Selection = &canvas.selection;

        let tile_size = crate::core::tile::TILE_SIZE;
        let Some(tiles) = canvas.layer_stack.layers[layer_idx].get_paint_tiles_mut() else {
            return;
        };
        let tx_start = lx0 / tile_size;
        let tx_end = (lx1 - 1) / tile_size;
        let ty_start = ly0 / tile_size;
        let ty_end = (ly1 - 1) / tile_size;

        for ty in ty_start..=ty_end {
            let tile_y0 = ty * tile_size;
            for tx in tx_start..=tx_end {
                let tile_x0 = tx * tile_size;
                let px_min = tile_x0.max(lx0);
                let py_min = tile_y0.max(ly0);
                let px_max = (tile_x0 + tile_size).min(lx1);
                let py_max = (tile_y0 + tile_size).min(ly1);
                if px_max <= px_min || py_max <= py_min {
                    continue;
                }
                let pos = crate::core::tile::TilePos {
                    x: tx as i32,
                    y: ty as i32,
                };
                let crate::core::tile::Tile { ink, pixels, .. } = tiles.get_tile_mut_ink(pos);
                let plane = ink
                    .as_mut()
                    .expect("get_tile_mut_ink guarantees an ink plane");
                let mut touched: Vec<usize> = Vec::new();
                for py in py_min..py_max {
                    let dy = py as f32 + 0.5 - lcy;
                    let dy2 = dy * dy;
                    let row = (py - tile_y0) * tile_size;
                    for px in px_min..px_max {
                        let dx = px as f32 + 0.5 - lcx;
                        let d2 = dx * dx + dy2;
                        if d2 > r2 {
                            continue;
                        }
                        let li = ((d2 * inv_r2) * lut_scale) as usize;
                        let mut src_a = falloff_lut[li.min(FALLOFF_LUT_N - 1)] * max_src_a;
                        if src_a < 0.001 {
                            continue;
                        }
                        if has_sel {
                            let canvas_px = (px as i32 + ox) as u32;
                            let canvas_py = (py as i32 + oy) as u32;
                            let sel = unsafe { &*sel_ptr };
                            src_a *= sel.sample(canvas_px, canvas_py);
                            if src_a < 0.001 {
                                continue;
                            }
                        }
                        let i = ((row + (px - tile_x0)) * 4) as usize;
                        let dst_a = pixels[i + 3] as f32 / 255.0;
                        if let Some(wm) = plate_gate {
                            // Plate paint: only the enabled ink channels move
                            // (toward the colour's ink, or toward 0 to erase);
                            // alpha and the other plates are left as-is.
                            let target = if is_eraser { [0u8; 4] } else { fg_ink };
                            for c in 0..4 {
                                if wm[c] {
                                    let d = plane[i + c] as f32 / 255.0;
                                    let t = target[c] as f32 / 255.0;
                                    plane[i + c] = ((d + (t - d) * src_a) * 255.0).round() as u8;
                                }
                            }
                            touched.push(i);
                        } else if is_eraser {
                            let new_a = dst_a * (1.0 - src_a);
                            pixels[i + 3] = (new_a * 255.0).round() as u8;
                            if new_a < 0.001 {
                                plane[i] = 0;
                                plane[i + 1] = 0;
                                plane[i + 2] = 0;
                                plane[i + 3] = 0;
                            }
                            touched.push(i);
                        } else {
                            if lock_alpha && dst_a < 0.001 {
                                continue;
                            }
                            let new_a = if lock_alpha {
                                dst_a
                            } else {
                                src_a + dst_a * (1.0 - src_a)
                            };
                            if new_a < 0.001 {
                                continue;
                            }
                            let dw = dst_a * (1.0 - src_a);
                            for c in 0..4 {
                                let d = plane[i + c] as f32 / 255.0;
                                let s = fg_ink[c] as f32 / 255.0;
                                let out = (s * src_a + d * dw) / new_a;
                                plane[i + c] = (out * 255.0).round().clamp(0.0, 255.0) as u8;
                            }
                            pixels[i + 3] = (new_a * 255.0).round() as u8;
                            touched.push(i);
                        }
                    }
                }
                if !touched.is_empty() {
                    // Re-project the mirror RGB for the changed pixels in one
                    // converter call per tile (bounds the ICC transform cost).
                    let inks: Vec<[u8; 4]> = touched
                        .iter()
                        .map(|&i| [plane[i], plane[i + 1], plane[i + 2], plane[i + 3]])
                        .collect();
                    let mut rgb = vec![[0u8; 3]; inks.len()];
                    conv.cmyk_to_rgb_slice(&inks, &mut rgb);
                    for (k, &i) in touched.iter().enumerate() {
                        pixels[i] = rgb[k][0];
                        pixels[i + 1] = rgb[k][1];
                        pixels[i + 2] = rgb[k][2];
                    }
                }
            }
        }

        let canvas_x0 = (lx0 as i32 + ox).max(0) as u32;
        let canvas_y0 = (ly0 as i32 + oy).max(0) as u32;
        let canvas_x1 = ((lx1 as i32 + ox) as u32).min(canvas.width);
        let canvas_y1 = ((ly1 as i32 + oy) as u32).min(canvas.height);
        canvas.mark_dirty(canvas_x0, canvas_y0, canvas_x1, canvas_y1);
    }

    /// Paint directly into the viewed saved alpha channel. Returns true when
    /// the active Channels-panel view consumed the dab, even if no pixel changed.
    fn paint_alpha_plane_dab(
        settings: &BrushSettings,
        canvas: &mut Canvas,
        cx: f32,
        cy: f32,
    ) -> bool {
        let crate::core::channels::ChannelView::Alpha(alpha_id) = canvas.channels.view else {
            return false;
        };
        let Some(idx) = canvas.channels.alpha_index_of(alpha_id) else {
            return true;
        };

        let r = (settings.size * 0.5).max(0.5).min(2000.0);
        let r2 = r * r;
        let w = canvas.channels.alpha[idx].width;
        let h = canvas.channels.alpha[idx].height;
        if w == 0 || h == 0 {
            return true;
        }

        let x0 = ((cx - r).floor().max(0.0) as u32).min(w);
        let y0 = ((cy - r).floor().max(0.0) as u32).min(h);
        let x1 = ((cx + r).ceil() as u32).min(w);
        let y1 = ((cy + r).ceil() as u32).min(h);
        if x1 <= x0 || y1 <= y0 {
            return true;
        }

        let [br, bg, bb, ba] = settings.color;
        let max_src_a =
            (ba as f32 / 255.0) * settings.opacity.clamp(0.0, 1.0) * settings.flow.clamp(0.0, 1.0);
        if max_src_a < 0.001 {
            return true;
        }
        let target = ((br as f32 / 255.0) * 0.2126
            + (bg as f32 / 255.0) * 0.7152
            + (bb as f32 / 255.0) * 0.0722)
            .clamp(0.0, 1.0);

        if canvas.pending_alpha_stroke.as_ref().map(|p| p.alpha_id) != Some(alpha_id) {
            let before = canvas.channels.alpha[idx].mask.clone();
            canvas.pending_alpha_stroke = Some(crate::core::channels::PendingAlphaStroke {
                alpha_id,
                before,
                bbox: None,
            });
        }

        const FALLOFF_LUT_N: usize = 1024;
        let inv_r2 = if r2 > 0.0 { 1.0 / r2 } else { 0.0 };
        let lut_scale = (FALLOFF_LUT_N - 1) as f32;
        let mut falloff_lut = [0.0f32; FALLOFF_LUT_N];
        for (k, slot) in falloff_lut.iter_mut().enumerate() {
            let d2k = (k as f32 / lut_scale) * r2;
            *slot = soft_round_alpha(d2k, r, settings.hardness);
        }

        let has_sel = canvas.selection.active;
        let sel = &canvas.selection;
        let ch = &mut canvas.channels.alpha[idx];
        if ch.mask.len() < (w as usize).saturating_mul(h as usize) {
            return true;
        }

        let mut changed = false;
        for y in y0..y1 {
            let dy = y as f32 + 0.5 - cy;
            let dy2 = dy * dy;
            let row = y as usize * w as usize;
            for x in x0..x1 {
                let dx = x as f32 + 0.5 - cx;
                let d2 = dx * dx + dy2;
                if d2 > r2 {
                    continue;
                }
                let idx_lut = ((d2 * inv_r2) * lut_scale) as usize;
                let mut src_a = falloff_lut[idx_lut.min(FALLOFF_LUT_N - 1)] * max_src_a;
                if src_a < 0.001 {
                    continue;
                }
                if has_sel {
                    src_a *= sel.sample(x, y);
                    if src_a < 0.001 {
                        continue;
                    }
                }

                let i = row + x as usize;
                let dst = ch.mask[i] as f32 / 255.0;
                let out = dst + (target - dst) * src_a;
                let out8 = (out * 255.0).round().clamp(0.0, 255.0) as u8;
                if ch.mask[i] != out8 {
                    ch.mask[i] = out8;
                    changed = true;
                }
            }
        }

        if changed {
            ch.revision += 1;
            canvas.mark_plane_dirty(x0, y0, x1, y1);
            if let Some(pending) = canvas.pending_alpha_stroke.as_mut() {
                pending.bbox = Some(match pending.bbox {
                    Some((bx0, by0, bx1, by1)) => {
                        (bx0.min(x0), by0.min(y0), bx1.max(x1), by1.max(y1))
                    }
                    None => (x0, y0, x1, y1),
                });
            }
        }

        true
    }

    /// Walk a stroke segment placing dab centres every `spacing` px along the
    /// polyline; `residual` is the distance from the segment start to the next
    /// dab and carries the leftover walk into the following event, so dab
    /// density does not depend on how the OS chops the stroke into mouse
    /// events and segment joints are never stamped twice.
    fn walk_dabs(
        spacing_base: f32,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        residual: &mut f32,
        mut dab: impl FnMut(f32, f32),
    ) {
        if !x0.is_finite() || !y0.is_finite() || !x1.is_finite() || !y1.is_finite() {
            return;
        }
        let dx = x1 - x0;
        let dy = y1 - y0;
        let dist = (dx * dx + dy * dy).sqrt();
        if dist <= 0.0 {
            return;
        }
        // Safety cap (mirrors the old 1000-step clamp): stretch spacing on
        // enormous jumps instead of stamping tens of thousands of dabs.
        let spacing = spacing_base.max(dist / 1024.0);
        let mut t = residual.max(0.0);
        while t <= dist {
            let f = t / dist;
            dab(x0 + dx * f, y0 + dy * f);
            t += spacing;
        }
        *residual = t - dist;
    }

    /// Distance-walked segment in direct (unbuffered) dab mode. Initialise
    /// `residual` to `dab_spacing()` right after stamping the press dab.
    pub fn paint_cpu_stroke_segment(
        settings: &BrushSettings,
        canvas: &mut Canvas,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        residual: &mut f32,
    ) {
        Self::walk_dabs(settings.dab_spacing(), x0, y0, x1, y1, residual, |x, y| {
            Self::paint_cpu_dab(settings, canvas, x, y)
        });
    }

    /// Distance-walked segment in buffered (per-stroke opacity) mode; the
    /// walker residual lives in the stroke buffer.
    pub fn paint_cpu_stroke_segment_stroked(
        settings: &BrushSettings,
        canvas: &mut Canvas,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        stroke: &mut StrokeBuffer,
    ) {
        let mut residual = stroke.residual;
        Self::walk_dabs(
            settings.dab_spacing(),
            x0,
            y0,
            x1,
            y1,
            &mut residual,
            |x, y| Self::paint_cpu_dab_stroked(settings, canvas, x, y, stroke),
        );
        stroke.residual = residual;
    }

    pub fn paint_stroke_segment(
        &mut self,
        canvas: &mut Canvas,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
    ) {
        if !x0.is_finite() || !y0.is_finite() || !x1.is_finite() || !y1.is_finite() {
            return;
        }
        let dx = x1 - x0;
        let dy = y1 - y0;
        let dist = (dx * dx + dy * dy).sqrt();
        if dist <= 0.0 {
            return;
        }
        let spacing = self.settings.dab_spacing().max(dist / 1024.0);
        let mut t = self.stroke_residual.max(0.0);
        while t <= dist {
            let f = t / dist;
            self.paint_dab(canvas, x0 + dx * f, y0 + dy * f);
            t += spacing;
        }
        self.stroke_residual = t - dist;
    }
}

impl Tool for BrushTool {
    fn id(&self) -> &'static str {
        "brush"
    }
    fn name(&self) -> &str {
        "Brush"
    }
    fn shortcut(&self) -> Option<char> {
        Some('B')
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::Brush
    }
    fn paints(&self) -> bool {
        true
    }
    fn cursor_size(&self) -> f32 {
        self.settings.size * 0.5
    }

    fn on_press(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        ctx.canvas_mut().begin_stroke("Brush Stroke");
        let (cx, cy) = (event.canvas_x, event.canvas_y);
        self.smoothed_x = cx;
        self.smoothed_y = cy;
        let alpha_plane = matches!(
            ctx.canvas().channels.view,
            crate::core::channels::ChannelView::Alpha(_)
        );
        if alpha_plane {
            self.stroke = None;
            Self::paint_cpu_dab(&self.settings, ctx.canvas_mut(), cx, cy);
            self.stroke_residual = self.settings.dab_spacing();
        } else {
            self.stroke = StrokeBuffer::begin(ctx.canvas());
        }
        if let Some(stroke) = self.stroke.as_mut() {
            Self::paint_cpu_dab_stroked(&self.settings, ctx.canvas_mut(), cx, cy, stroke);
            stroke.residual = self.settings.dab_spacing();
        }
        ToolResponse::repaint()
    }

    fn on_drag(
        &mut self,
        event: PointerEvent,
        _prev: &PointerEvent,
        ctx: &mut ToolCtx,
    ) -> ToolResponse {
        let (raw_x, raw_y) = (event.canvas_x, event.canvas_y);

        let (x1, y1) = if self.settings.smoothing > 0.001 {
            let factor = 1.0 - self.settings.smoothing.clamp(0.0, 1.0) * 0.9;
            let sx = self.smoothed_x + factor * (raw_x - self.smoothed_x);
            let sy = self.smoothed_y + factor * (raw_y - self.smoothed_y);
            (sx, sy)
        } else {
            (raw_x, raw_y)
        };

        let (x0, y0) = (self.smoothed_x, self.smoothed_y);
        self.smoothed_x = x1;
        self.smoothed_y = y1;

        if let Some(stroke) = self.stroke.as_mut() {
            Self::paint_cpu_stroke_segment_stroked(
                &self.settings,
                ctx.canvas_mut(),
                x0,
                y0,
                x1,
                y1,
                stroke,
            );
        } else if matches!(
            ctx.canvas().channels.view,
            crate::core::channels::ChannelView::Alpha(_)
        ) {
            Self::paint_cpu_stroke_segment(
                &self.settings,
                ctx.canvas_mut(),
                x0,
                y0,
                x1,
                y1,
                &mut self.stroke_residual,
            );
        }
        ToolResponse::repaint()
    }

    fn on_release(&mut self, _event: PointerEvent, _ctx: &mut ToolCtx) -> ToolResponse {
        self.stroke_dabs.clear();
        self.pending_dabs.clear();
        self.stroke = None;
        ToolResponse::none()
    }
}

#[cfg(test)]
mod tests {
    use super::soft_round_alpha;
    use super::*;

    #[test]
    fn channel_gate_paints_luma_into_selected_channel_only() {
        let mut canvas = Canvas::new_blank(16, 16);
        // Gray background layer (128,128,128,255); write only Red.
        let idx = canvas.layer_stack.active_idx;
        if let Some(tiles) = canvas.layer_stack.layers[idx].get_paint_tiles_mut() {
            for y in 0..16 {
                for x in 0..16 {
                    tiles.set_pixel(x, y, 128, 128, 128, 255);
                }
            }
        }
        canvas.channels.select_color(0, false);

        let settings = BrushSettings {
            size: 8.0,
            hardness: 1.0,
            opacity: 1.0,
            flow: 1.0,
            color: [255, 255, 255, 255], // luma 255
            ..BrushSettings::default()
        };
        BrushTool::paint_cpu_dab(&settings, &mut canvas, 8.0, 8.0);
        let (r, g, b, a) = canvas.layer_stack.layers[idx].tiles.get_pixel(8, 8);
        assert_eq!(r, 255, "red plate painted to the colour's luma");
        assert_eq!((g, b, a), (128, 128, 255), "G/B/alpha untouched");
    }

    #[test]
    fn brush_paints_viewed_alpha_channel_and_undoes_as_one_stroke() {
        let mut canvas = Canvas::new_blank(8, 8);
        let alpha_idx = canvas
            .channels
            .add_alpha("Stored".into(), vec![0; 64], 8, 8);
        let alpha_id = canvas.channels.alpha[alpha_idx].id;
        canvas.channels.select_alpha(alpha_idx);

        let settings = BrushSettings {
            size: 4.0,
            hardness: 1.0,
            opacity: 1.0,
            flow: 1.0,
            color: [255, 255, 255, 255],
            ..BrushSettings::default()
        };
        BrushTool::paint_cpu_dab(&settings, &mut canvas, 4.0, 4.0);

        let center = 4 * 8 + 4;
        assert_eq!(canvas.channels.alpha[alpha_idx].mask[center], 255);
        assert_eq!(
            canvas.pending_alpha_stroke.as_ref().map(|p| p.alpha_id),
            Some(alpha_id)
        );
        assert!(canvas.plane_dirty.active);

        canvas.end_stroke();
        assert!(canvas.pending_alpha_stroke.is_none());
        assert_eq!(canvas.undo_count(), 1);

        canvas.undo();
        let idx = canvas.channels.alpha_index_of(alpha_id).unwrap();
        assert_eq!(canvas.channels.alpha[idx].mask[center], 0);

        canvas.redo();
        let idx = canvas.channels.alpha_index_of(alpha_id).unwrap();
        assert_eq!(canvas.channels.alpha[idx].mask[center], 255);
    }

    #[test]
    fn brush_paints_ink_on_cmyk_and_keeps_mirror_consistent() {
        use crate::core::cms::naive_cmyk_to_rgb;

        let mut canvas = Canvas::new_blank(16, 16);
        let idx = canvas.layer_stack.active_idx;
        if let Some(tiles) = canvas.layer_stack.layers[idx].get_paint_tiles_mut() {
            for y in 0..16 {
                for x in 0..16 {
                    tiles.set_pixel(x, y, 255, 255, 255, 255);
                }
            }
        }
        canvas
            .convert_to_cmyk(crate::core::canvas::CmykProfile::Naive)
            .expect("convert to CMYK");

        let settings = BrushSettings {
            size: 8.0,
            hardness: 1.0,
            opacity: 1.0,
            flow: 1.0,
            color: [255, 0, 0, 255], // red → high M,Y ink
            ..BrushSettings::default()
        };
        canvas.begin_stroke("Brush Stroke");
        BrushTool::paint_cpu_dab(&settings, &mut canvas, 8.0, 8.0);

        // Center ink is the painted red's ink; the mirror is its exact projection.
        let mut ink = [0u8; 4];
        canvas.layer_stack.layers[idx]
            .tiles
            .extract_ink_region_into(8, 8, 1, 1, &mut ink);
        assert_eq!(ink[0], 0, "red carries no cyan");
        assert!(ink[1] > 200 && ink[2] > 200, "red is mostly magenta+yellow");
        let (r, g, b, a) = canvas.layer_stack.layers[idx].tiles.get_pixel(8, 8);
        assert_eq!([r, g, b], naive_cmyk_to_rgb(ink), "mirror must project ink");
        assert_eq!(a, 255, "opaque paint over opaque stays opaque");

        // Whole dab keeps the ink/mirror invariant.
        for (py, px) in [(8u32, 8u32), (6, 6), (10, 10), (5, 9)] {
            let mut k = [0u8; 4];
            canvas.layer_stack.layers[idx]
                .tiles
                .extract_ink_region_into(px, py, 1, 1, &mut k);
            let (pr, pg, pb, pa) = canvas.layer_stack.layers[idx].tiles.get_pixel(px, py);
            if pa > 0 {
                assert_eq!([pr, pg, pb], naive_cmyk_to_rgb(k), "desync at {px},{py}");
            }
        }

        // Undo restores the white background (ink back to none).
        canvas.end_stroke();
        assert_eq!(canvas.undo_count(), 1);
        canvas.undo();
        let mut ink0 = [9u8; 4];
        canvas.layer_stack.layers[idx]
            .tiles
            .extract_ink_region_into(8, 8, 1, 1, &mut ink0);
        assert_eq!(ink0, [0, 0, 0, 0], "undo returns white (no ink)");
    }

    #[test]
    fn cmyk_plate_gate_paints_only_selected_ink_channel() {
        use crate::core::cms::naive_cmyk_to_rgb;

        let mut canvas = Canvas::new_blank(16, 16);
        let idx = canvas.layer_stack.active_idx;
        if let Some(tiles) = canvas.layer_stack.layers[idx].get_paint_tiles_mut() {
            for y in 0..16 {
                for x in 0..16 {
                    tiles.set_pixel(x, y, 255, 255, 255, 255);
                }
            }
        }
        canvas
            .convert_to_cmyk(crate::core::canvas::CmykProfile::Naive)
            .expect("convert to CMYK");
        // Select only the Cyan plate (slot 0 of 4).
        canvas.channels.select_channel_n(0, false, 4);

        let settings = BrushSettings {
            size: 8.0,
            hardness: 1.0,
            opacity: 1.0,
            flow: 1.0,
            color: [255, 0, 0, 255], // red → C=0, M=255, Y=255
            ..BrushSettings::default()
        };
        canvas.begin_stroke("Brush Stroke");
        BrushTool::paint_cpu_dab(&settings, &mut canvas, 8.0, 8.0);

        let mut ink = [0u8; 4];
        canvas.layer_stack.layers[idx]
            .tiles
            .extract_ink_region_into(8, 8, 1, 1, &mut ink);
        // Cyan moved toward red's C (=0); M/Y/K plates and alpha untouched (white).
        assert_eq!(ink, [0, 0, 0, 0], "only Cyan is gated and red's C is 0");
        let (r, g, b, a) = canvas.layer_stack.layers[idx].tiles.get_pixel(8, 8);
        assert_eq!(
            [r, g, b],
            naive_cmyk_to_rgb(ink),
            "mirror projects gated ink"
        );
        assert_eq!(a, 255, "plate paint never changes alpha");

        // Now select Magenta and paint: only M rises, C/Y/K stay.
        canvas.channels.select_channel_n(1, false, 4);
        BrushTool::paint_cpu_dab(&settings, &mut canvas, 8.0, 8.0);
        canvas.layer_stack.layers[idx]
            .tiles
            .extract_ink_region_into(8, 8, 1, 1, &mut ink);
        assert!(ink[1] > 200, "Magenta plate rose toward red's M");
        assert_eq!((ink[0], ink[2], ink[3]), (0, 0, 0), "C/Y/K untouched");
    }

    #[test]
    fn channel_gate_buffered_stroke_is_idempotent_and_capped() {
        let mut canvas = Canvas::new_blank(16, 16);
        let idx = canvas.layer_stack.active_idx;
        if let Some(tiles) = canvas.layer_stack.layers[idx].get_paint_tiles_mut() {
            for y in 0..16 {
                for x in 0..16 {
                    tiles.set_pixel(x, y, 100, 100, 100, 255);
                }
            }
        }
        canvas.channels.select_color(1, false); // Green only

        let settings = BrushSettings {
            size: 8.0,
            hardness: 1.0,
            opacity: 0.5,
            flow: 1.0,
            color: [255, 255, 255, 255], // luma 255
            ..BrushSettings::default()
        };
        let mut stroke = StrokeBuffer::begin(&canvas).expect("paintable layer");
        BrushTool::paint_cpu_dab_stroked(&settings, &mut canvas, 8.0, 8.0, &mut stroke);
        let after_one = canvas.layer_stack.layers[idx].tiles.get_pixel(8, 8);
        for _ in 0..15 {
            BrushTool::paint_cpu_dab_stroked(&settings, &mut canvas, 8.0, 8.0, &mut stroke);
        }
        let after_many = canvas.layer_stack.layers[idx].tiles.get_pixel(8, 8);

        // 50% stroke ceiling: G = 100 + (255-100)*0.5 ≈ 177, R/B/alpha kept.
        assert_eq!(after_one, after_many, "overlapping dabs stay idempotent");
        let (r, g, b, a) = after_many;
        assert!(
            (176..=179).contains(&g),
            "green capped at stroke opacity, got {g}"
        );
        assert_eq!((r, b, a), (100, 100, 255), "R/B/alpha untouched");
    }

    #[test]
    fn channel_gate_eraser_writes_background_luma() {
        let mut canvas = Canvas::new_blank(16, 16);
        let idx = canvas.layer_stack.add_layer(16, 16);
        canvas.layer_stack.active_idx = idx;
        if let Some(tiles) = canvas.layer_stack.layers[idx].get_paint_tiles_mut() {
            for y in 0..16 {
                for x in 0..16 {
                    tiles.set_pixel(x, y, 40, 50, 60, 200);
                }
            }
        }
        canvas.channels.select_color(2, false); // Blue only

        // The eraser carries the background colour in `color` (see eraser.rs).
        let settings = BrushSettings {
            size: 8.0,
            hardness: 1.0,
            opacity: 1.0,
            flow: 1.0,
            color: [255, 255, 255, 255],
            is_eraser: true,
            ..BrushSettings::default()
        };
        BrushTool::paint_cpu_dab(&settings, &mut canvas, 8.0, 8.0);
        let (r, g, b, a) = canvas.layer_stack.layers[idx].tiles.get_pixel(8, 8);
        assert_eq!(b, 255, "blue plate erased to background luma");
        assert_eq!((r, g, a), (40, 50, 200), "other channels and alpha kept");
    }

    #[test]
    fn mask_paint_accumulates_across_low_flow_dabs() {
        use crate::core::layer::{LayerMask, PaintTarget};
        let mut canvas = Canvas::new_blank(16, 16);
        let idx = canvas.layer_stack.active_idx;
        canvas.layer_stack.layers[idx].mask = Some(LayerMask::new_white(16, 16));
        canvas.layer_stack.layers[idx].paint_target = PaintTarget::Mask;

        // Soft brush, low opacity/flow, painting black onto a white mask.
        let settings = BrushSettings {
            size: 8.0,
            hardness: 0.0,
            opacity: 0.5,
            flow: 0.5,
            color: [0, 0, 0, 255],
            ..BrushSettings::default()
        };
        let value = |canvas: &Canvas| {
            let mask = canvas.layer_stack.layers[idx].mask.as_ref().unwrap();
            mask.tiles.get_pixel(8, 8).0
        };

        let start = value(&canvas);
        BrushTool::paint_cpu_dab(&settings, &mut canvas, 8.0, 8.0);
        let after_one = value(&canvas);
        for _ in 0..20 {
            BrushTool::paint_cpu_dab(&settings, &mut canvas, 8.0, 8.0);
        }
        let after_many = value(&canvas);

        assert!(after_one < start, "first dab darkens the mask");
        assert!(
            after_many < after_one,
            "repeated dabs must keep building coverage ({after_many} !< {after_one})"
        );
        assert!(
            after_many <= 5,
            "soft low-flow paint eventually covers, got {after_many}"
        );
    }

    #[test]
    fn stroke_opacity_caps_within_one_stroke_but_stacks_across_strokes() {
        let mut canvas = Canvas::new_blank(16, 16);
        let idx = canvas.layer_stack.add_layer(16, 16); // transparent layer
        canvas.layer_stack.active_idx = idx;

        let settings = BrushSettings {
            size: 8.0,
            hardness: 1.0,
            opacity: 0.5,
            flow: 1.0,
            color: [255, 0, 0, 255],
            ..BrushSettings::default()
        };
        let alpha_at = |canvas: &Canvas| canvas.layer_stack.layers[idx].tiles.get_pixel(8, 8).3;

        let mut stroke = StrokeBuffer::begin(&canvas).expect("paintable layer");
        for _ in 0..15 {
            BrushTool::paint_cpu_dab_stroked(&settings, &mut canvas, 8.0, 8.0, &mut stroke);
        }
        drop(stroke);
        let one_stroke = alpha_at(&canvas);
        assert!(
            (126..=129).contains(&one_stroke),
            "overlapping dabs in ONE 50% stroke must cap at ~50% alpha, got {one_stroke}"
        );

        let mut stroke2 = StrokeBuffer::begin(&canvas).expect("paintable layer");
        for _ in 0..15 {
            BrushTool::paint_cpu_dab_stroked(&settings, &mut canvas, 8.0, 8.0, &mut stroke2);
        }
        let two_strokes = alpha_at(&canvas);
        assert!(
            two_strokes >= 187,
            "a SECOND 50% stroke must stack on the first, got {two_strokes}"
        );
    }

    #[test]
    fn dab_layout_is_independent_of_event_chopping() {
        let settings = BrushSettings {
            size: 8.0,
            hardness: 0.5,
            opacity: 0.6,
            flow: 0.5,
            ..BrushSettings::default()
        };
        // Paint the same straight path chopped into different event segments.
        let paint = |splits: &[(f32, f32)]| -> Vec<u8> {
            let mut canvas = Canvas::new_blank(64, 16);
            let mut prev = (4.0, 8.0);
            BrushTool::paint_cpu_dab(&settings, &mut canvas, prev.0, prev.1);
            let mut residual = settings.dab_spacing();
            for &(x, y) in splits {
                BrushTool::paint_cpu_stroke_segment(
                    &settings,
                    &mut canvas,
                    prev.0,
                    prev.1,
                    x,
                    y,
                    &mut residual,
                );
                prev = (x, y);
            }
            let idx = canvas.layer_stack.active_idx;
            canvas.layer_stack.layers[idx].tiles.flatten()
        };

        let one = paint(&[(60.0, 8.0)]);
        // Includes a segment shorter than the dab spacing (1.5 px < 2 px).
        let many = paint(&[(13.0, 8.0), (14.5, 8.0), (30.0, 8.0), (60.0, 8.0)]);

        assert_eq!(one.len(), many.len());
        let max_diff = one
            .iter()
            .zip(&many)
            .map(|(a, b)| (*a as i16 - *b as i16).unsigned_abs())
            .max()
            .unwrap_or(0);
        assert!(
            max_diff <= 2,
            "chopping the stroke into events changed the paint (max byte diff {max_diff})"
        );
    }

    #[test]
    fn soft_round_is_radial_gradient_without_hard_plateau() {
        let r = 100.0_f32;
        let center = soft_round_alpha(0.0, r, 0.0);
        let quarter = soft_round_alpha((r * 0.25).powi(2), r, 0.0);
        let half = soft_round_alpha((r * 0.5).powi(2), r, 0.0);
        let three_quarter = soft_round_alpha((r * 0.75).powi(2), r, 0.0);
        let edge = soft_round_alpha(r * r, r, 0.0);

        assert!(center > quarter);
        assert!(quarter > half);
        assert!(half > three_quarter);
        assert!(three_quarter > edge);
        assert!(
            (half - 0.5).abs() < 0.02,
            "half-radius should look like a smooth gradient"
        );
        assert_eq!(edge, 0.0);
    }

    #[test]
    fn hard_round_keeps_crisp_core_with_antialiased_edge() {
        let r = 100.0_f32;
        assert!(soft_round_alpha((r * 0.9).powi(2), r, 1.0) > 0.99);
        assert!(soft_round_alpha((r * 0.995).powi(2), r, 1.0) < 1.0);
        assert_eq!(soft_round_alpha(r * r, r, 1.0), 0.0);
    }
}

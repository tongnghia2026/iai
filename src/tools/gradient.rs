#![allow(dead_code)]
use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::canvas::Canvas;
use crate::core::tile::{Tile, TilePos, TILE_SIZE};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GradientType {
    Linear,
    Radial,
    Angle,
    Reflected,
    Diamond,
}

impl GradientType {
    pub fn name(&self) -> &str {
        match self {
            GradientType::Linear => "Linear",
            GradientType::Radial => "Radial",
            GradientType::Angle => "Angle",
            GradientType::Reflected => "Reflected",
            GradientType::Diamond => "Diamond",
        }
    }
    pub fn all() -> Vec<GradientType> {
        vec![
            GradientType::Linear,
            GradientType::Radial,
            GradientType::Angle,
            GradientType::Reflected,
            GradientType::Diamond,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GradientBlendMode {
    Normal,
    Dissolve,
    Behind,
    Clear,
}

#[derive(Debug, Clone)]
pub struct GradientStop {
    pub position: f32,
    pub color: [u8; 4],
}

#[derive(Debug, Clone)]
pub struct Gradient {
    pub name: String,
    pub stops: Vec<GradientStop>,
}

impl Gradient {
    pub fn foreground_to_background(fg: [u8; 4], bg: [u8; 4]) -> Self {
        Self {
            name: "Foreground to Background".to_string(),
            stops: vec![
                GradientStop {
                    position: 0.0,
                    color: fg,
                },
                GradientStop {
                    position: 1.0,
                    color: bg,
                },
            ],
        }
    }

    pub fn foreground_to_transparent(fg: [u8; 4]) -> Self {
        Self {
            name: "Foreground to Transparent".to_string(),
            stops: vec![
                GradientStop {
                    position: 0.0,
                    color: fg,
                },
                GradientStop {
                    position: 1.0,
                    color: [fg[0], fg[1], fg[2], 0],
                },
            ],
        }
    }

    pub fn sample(&self, t: f32) -> [u8; 4] {
        let mut stops = self.stops.clone();
        stops.sort_by(|a, b| {
            a.position
                .partial_cmp(&b.position)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Self::sample_sorted_stops(&stops, t)
    }

    fn sample_sorted_stops(stops: &[GradientStop], t: f32) -> [u8; 4] {
        if stops.is_empty() {
            return [0, 0, 0, 255];
        }
        if stops.len() == 1 {
            return stops[0].color;
        }

        let t = t.clamp(0.0, 1.0);
        let last = stops.len() - 1;
        if t <= stops[0].position {
            return stops[0].color;
        }
        if t >= stops[last].position {
            return stops[last].color;
        }

        for i in 0..last {
            let s0 = &stops[i];
            let s1 = &stops[i + 1];
            if t >= s0.position && t <= s1.position {
                let range = s1.position - s0.position;
                if range < 0.0001 {
                    return s1.color;
                }
                let local_t = (t - s0.position) / range;
                let inv = 1.0 - local_t;
                return [
                    (s0.color[0] as f32 * inv + s1.color[0] as f32 * local_t) as u8,
                    (s0.color[1] as f32 * inv + s1.color[1] as f32 * local_t) as u8,
                    (s0.color[2] as f32 * inv + s1.color[2] as f32 * local_t) as u8,
                    (s0.color[3] as f32 * inv + s1.color[3] as f32 * local_t) as u8,
                ];
            }
        }
        stops[last].color
    }
}

const GRADIENT_LUT_SIZE: usize = 4096;

pub struct GradientTool {
    pub gradient_type: GradientType,
    pub blend_mode: GradientBlendMode,
    pub opacity: f32,
    pub reverse: bool,
    pub dither: bool,
    pub gradient: Gradient,
    pub start_x: f32,
    pub start_y: f32,
    /// Current drag endpoint (canvas space) — drives the direction-guide overlay.
    pub cur_x: f32,
    pub cur_y: f32,
    pub is_dragging: bool,
}

impl GradientTool {
    pub fn new() -> Self {
        Self {
            gradient_type: GradientType::Linear,
            blend_mode: GradientBlendMode::Normal,
            opacity: 1.0,
            reverse: false,
            dither: false,
            gradient: Gradient::foreground_to_background([0, 0, 0, 255], [255, 255, 255, 255]),
            start_x: 0.0,
            start_y: 0.0,
            cur_x: 0.0,
            cur_y: 0.0,
            is_dragging: false,
        }
    }

    /// Start→current drag endpoints (canvas space) while dragging, for the
    /// direction-guide overlay. `None` when not dragging or zero-length.
    pub fn preview_line(&self) -> Option<[f32; 4]> {
        if !self.is_dragging {
            return None;
        }
        let dx = self.cur_x - self.start_x;
        let dy = self.cur_y - self.start_y;
        if dx * dx + dy * dy < 1.0 {
            return None;
        }
        Some([self.start_x, self.start_y, self.cur_x, self.cur_y])
    }

    fn apply_gradient(&self, canvas: &mut Canvas, ex: f32, ey: f32) {
        if !Canvas::fits_flat_buffer(canvas.width, canvas.height) {
            return;
        }
        let sx = self.start_x;
        let sy = self.start_y;
        let canvas_w = canvas.width;
        let canvas_h = canvas.height;
        canvas.layer_stack.normalize_active_idx();
        let active_idx = canvas.layer_stack.active_idx;
        let opacity = self.opacity;
        let selection_active = canvas.selection.active;
        let content_clip = if selection_active {
            None
        } else {
            canvas.layer_stack.layers.get(active_idx).and_then(|layer| {
                if layer.paint_target != crate::core::layer::PaintTarget::Pixels {
                    return None;
                }
                layer.tiles.content_bounds().map(|(x0, y0, x1, y1)| {
                    (
                        (layer.offset.0 + x0).clamp(0, canvas_w as i32),
                        (layer.offset.1 + y0).clamp(0, canvas_h as i32),
                        (layer.offset.0 + x1).clamp(0, canvas_w as i32),
                        (layer.offset.1 + y1).clamp(0, canvas_h as i32),
                    )
                })
            })
        };
        let (clip_x0, clip_y0, clip_x1, clip_y1) = if selection_active {
            let bbox = canvas.selection.bounding_box();
            (
                (bbox.0.floor() as i32).clamp(0, canvas_w as i32),
                (bbox.1.floor() as i32).clamp(0, canvas_h as i32),
                (bbox.2.ceil() as i32).clamp(0, canvas_w as i32),
                (bbox.3.ceil() as i32).clamp(0, canvas_h as i32),
            )
        } else if let Some(bounds) = content_clip {
            bounds
        } else {
            (0, 0, canvas_w as i32, canvas_h as i32)
        };
        let selection_sample = if selection_active {
            Some((
                canvas.selection.mask.clone(),
                canvas.selection.width,
                canvas.selection.height,
                canvas.selection.offset,
            ))
        } else {
            None
        };

        let can_paint = canvas
            .layer_stack
            .layers
            .get(active_idx)
            .map_or(false, |layer| {
                (!layer.locked || layer.is_background) && layer.get_paint_tiles().is_some()
            });
        if !can_paint {
            return;
        }

        let dx = ex - sx;
        let dy = ey - sy;
        let len2 = dx * dx + dy * dy;
        if len2 < 0.001 {
            return;
        }
        let len = len2.sqrt();
        let start_angle = dy.atan2(dx);
        let lut = self.build_lut();
        // CMYK: convert the gradient LUT to ink once; each pixel blends its
        // ink into the plane and re-projects the mirror.
        let cmyk_conv = canvas.cmyk_converter();
        let ink_lut: Option<Vec<[u8; 4]>> = cmyk_conv.as_ref().map(|c| {
            lut.iter()
                .map(|col| c.rgb_to_cmyk_one([col[0], col[1], col[2]]))
                .collect()
        });
        // Plate gate: a single-ink selection draws into only those plates.
        let plate_gate = if cmyk_conv.is_some() {
            canvas.channels.write_gate()
        } else {
            None
        };

        canvas.begin_stroke("Gradient");

        // Channels-panel write gate: each pixel blends its gradient colour's
        // luma into the enabled channels only (alpha untouched). Mask
        // gradients stay grayscale and skip the gate.
        let channel_wm = canvas.channels.write_gate();
        let layer = match canvas.layer_stack.layers.get_mut(active_idx) {
            Some(layer) => layer,
            None => {
                canvas.end_stroke();
                return;
            }
        };
        let channel_wm = if layer.paint_target == crate::core::layer::PaintTarget::Mask {
            None
        } else {
            channel_wm
        };
        let ox = layer.offset.0;
        let oy = layer.offset.1;
        let layer_w = layer.width;
        let layer_h = layer.height;
        let lock_alpha =
            layer.lock_alpha && layer.paint_target == crate::core::layer::PaintTarget::Pixels;
        let tiles = match layer.get_paint_tiles_mut() {
            Some(t) => t,
            None => {
                canvas.end_stroke();
                return;
            }
        };

        let lx0 = (clip_x0 - ox).clamp(0, layer_w as i32) as u32;
        let ly0 = (clip_y0 - oy).clamp(0, layer_h as i32) as u32;
        let lx1 = (clip_x1 - ox).clamp(0, layer_w as i32) as u32;
        let ly1 = (clip_y1 - oy).clamp(0, layer_h as i32) as u32;
        if lx1 <= lx0 || ly1 <= ly0 {
            canvas.end_stroke();
            return;
        }

        let tx0 = lx0 / TILE_SIZE;
        let ty0 = ly0 / TILE_SIZE;
        let tx1 = (lx1 + TILE_SIZE - 1) / TILE_SIZE;
        let ty1 = (ly1 + TILE_SIZE - 1) / TILE_SIZE;
        let fast_replace = opacity >= 0.999
            && lut.iter().all(|color| color[3] == 255)
            && !selection_active
            && !lock_alpha
            && !matches!(self.blend_mode, GradientBlendMode::Clear);

        for ty in ty0..ty1 {
            for tx in tx0..tx1 {
                let pos = TilePos {
                    x: tx as i32,
                    y: ty as i32,
                };
                let Some(tile) = (if lock_alpha {
                    tiles.tiles.get_mut(&pos)
                } else {
                    Some(
                        tiles
                            .tiles
                            .entry(pos)
                            .or_insert_with(|| Arc::new(Tile::new_empty())),
                    )
                }) else {
                    continue;
                };
                let tile = Arc::make_mut(tile);
                tile.revision += 1;

                let tile_lx0 = (tx * TILE_SIZE).max(lx0);
                let tile_ly0 = (ty * TILE_SIZE).max(ly0);
                let tile_lx1 = ((tx + 1) * TILE_SIZE).min(lx1);
                let tile_ly1 = ((ty + 1) * TILE_SIZE).min(ly1);

                for layer_y in tile_ly0..tile_ly1 {
                    let canvas_y = layer_y as i32 + oy;
                    let py = canvas_y as f32 + 0.5 - sy;
                    let row_base = ((layer_y - ty * TILE_SIZE) * TILE_SIZE * 4) as usize;
                    for layer_x in tile_lx0..tile_lx1 {
                        let canvas_x = layer_x as i32 + ox;
                        let px = canvas_x as f32 + 0.5 - sx;
                        let lut_idx = self.lut_index(px, py, dx, dy, len, len2, start_angle);
                        let color = lut[lut_idx];
                        let mut src_a = (color[3] as f32 / 255.0) * opacity;
                        if let Some((mask, sw, sh, offset)) = selection_sample.as_ref() {
                            let sx = canvas_x - offset.0;
                            let sy = canvas_y - offset.1;
                            if sx < 0 || sy < 0 || sx >= *sw as i32 || sy >= *sh as i32 {
                                continue;
                            }
                            let mi = (sy as u32 * *sw + sx as u32) as usize;
                            let Some(&m) = mask.get(mi) else {
                                continue;
                            };
                            src_a *= m as f32 / 255.0;
                        }
                        if src_a < 0.001 {
                            continue;
                        }

                        let col = ((layer_x - tx * TILE_SIZE) * 4) as usize;
                        let i = row_base + col;
                        if lock_alpha && tile.pixels[i + 3] == 0 {
                            continue;
                        }
                        if let (Some(conv), Some(ilut)) = (cmyk_conv.as_ref(), ink_lut.as_ref()) {
                            if tile.ink.is_none() {
                                tile.ink = Some(vec![0u8; crate::core::tile::TILE_BYTES]);
                            }
                            let fink = ilut[lut_idx];
                            let plane = tile.ink.as_mut().unwrap();
                            if let Some(wm) = plate_gate {
                                // Plate gradient: only enabled ink channels move.
                                let mut ink = [plane[i], plane[i + 1], plane[i + 2], plane[i + 3]];
                                for c in 0..4 {
                                    if wm[c] {
                                        let d = plane[i + c] as f32 / 255.0;
                                        let s = fink[c] as f32 / 255.0;
                                        let v = ((d + (s - d) * src_a) * 255.0).round() as u8;
                                        plane[i + c] = v;
                                        ink[c] = v;
                                    }
                                }
                                let rgb = conv.cmyk_to_rgb_one(ink);
                                tile.pixels[i] = rgb[0];
                                tile.pixels[i + 1] = rgb[1];
                                tile.pixels[i + 2] = rgb[2];
                                continue;
                            }
                            let dst_a = tile.pixels[i + 3] as f32 / 255.0;
                            if lock_alpha {
                                if dst_a < 0.001 {
                                    continue;
                                }
                                let w = src_a.min(dst_a) / dst_a;
                                let mut ink = [0u8; 4];
                                for c in 0..4 {
                                    let d = plane[i + c] as f32 / 255.0;
                                    let s = fink[c] as f32 / 255.0;
                                    let v =
                                        ((s * w + d * (1.0 - w)) * 255.0).round().clamp(0.0, 255.0)
                                            as u8;
                                    plane[i + c] = v;
                                    ink[c] = v;
                                }
                                let rgb = conv.cmyk_to_rgb_one(ink);
                                tile.pixels[i] = rgb[0];
                                tile.pixels[i + 1] = rgb[1];
                                tile.pixels[i + 2] = rgb[2];
                                continue;
                            }
                            let new_a = src_a + dst_a * (1.0 - src_a);
                            if new_a < 0.001 {
                                continue;
                            }
                            let dw = dst_a * (1.0 - src_a);
                            let mut ink = [0u8; 4];
                            for c in 0..4 {
                                let d = plane[i + c] as f32 / 255.0;
                                let s = fink[c] as f32 / 255.0;
                                let v = (((s * src_a + d * dw) / new_a) * 255.0)
                                    .round()
                                    .clamp(0.0, 255.0)
                                    as u8;
                                plane[i + c] = v;
                                ink[c] = v;
                            }
                            let rgb = conv.cmyk_to_rgb_one(ink);
                            tile.pixels[i] = rgb[0];
                            tile.pixels[i + 1] = rgb[1];
                            tile.pixels[i + 2] = rgb[2];
                            tile.pixels[i + 3] = (new_a * 255.0).round() as u8;
                            continue;
                        }
                        if let Some(wm) = channel_wm {
                            let luma = crate::core::tile::luma_u8(color[0], color[1], color[2])
                                as f32
                                / 255.0;
                            crate::core::tile::blend_masked(
                                &mut tile.pixels[i..i + 4],
                                [luma; 3],
                                src_a,
                                wm,
                            );
                            continue;
                        }
                        if fast_replace {
                            tile.pixels[i..i + 4].copy_from_slice(&color);
                            continue;
                        }

                        let dst_a = tile.pixels[i + 3] as f32 / 255.0;
                        if lock_alpha {
                            if dst_a < 0.001 {
                                continue;
                            }
                            let w = src_a.min(dst_a) / dst_a;
                            let src_r = color[0] as f32 / 255.0;
                            let src_g = color[1] as f32 / 255.0;
                            let src_b = color[2] as f32 / 255.0;
                            let dst_r = tile.pixels[i] as f32 / 255.0;
                            let dst_g = tile.pixels[i + 1] as f32 / 255.0;
                            let dst_b = tile.pixels[i + 2] as f32 / 255.0;
                            tile.pixels[i] = ((src_r * w + dst_r * (1.0 - w)) * 255.0)
                                .round()
                                .clamp(0.0, 255.0)
                                as u8;
                            tile.pixels[i + 1] = ((src_g * w + dst_g * (1.0 - w)) * 255.0)
                                .round()
                                .clamp(0.0, 255.0)
                                as u8;
                            tile.pixels[i + 2] = ((src_b * w + dst_b * (1.0 - w)) * 255.0)
                                .round()
                                .clamp(0.0, 255.0)
                                as u8;
                            continue;
                        }
                        let out_a = src_a + dst_a * (1.0 - src_a);
                        if out_a <= 0.001 {
                            continue;
                        }

                        let inv_src = 1.0 - src_a;
                        let src_r = color[0] as f32 / 255.0;
                        let src_g = color[1] as f32 / 255.0;
                        let src_b = color[2] as f32 / 255.0;
                        let dst_r = tile.pixels[i] as f32 / 255.0;
                        let dst_g = tile.pixels[i + 1] as f32 / 255.0;
                        let dst_b = tile.pixels[i + 2] as f32 / 255.0;

                        tile.pixels[i] =
                            ((src_r * src_a + dst_r * dst_a * inv_src) / out_a * 255.0) as u8;
                        tile.pixels[i + 1] =
                            ((src_g * src_a + dst_g * dst_a * inv_src) / out_a * 255.0) as u8;
                        tile.pixels[i + 2] =
                            ((src_b * src_a + dst_b * dst_a * inv_src) / out_a * 255.0) as u8;
                        tile.pixels[i + 3] = (out_a * 255.0) as u8;
                    }
                }
            }
        }

        canvas.mark_dirty(0, 0, canvas_w, canvas_h);
        canvas.end_stroke();
    }

    fn build_lut(&self) -> Vec<[u8; 4]> {
        let mut stops = self.gradient.stops.clone();
        stops.sort_by(|a, b| {
            a.position
                .partial_cmp(&b.position)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        (0..GRADIENT_LUT_SIZE)
            .map(|i| {
                let t = i as f32 / (GRADIENT_LUT_SIZE - 1) as f32;
                Gradient::sample_sorted_stops(&stops, t)
            })
            .collect()
    }

    #[inline]
    fn lut_index(
        &self,
        px: f32,
        py: f32,
        dx: f32,
        dy: f32,
        len: f32,
        len2: f32,
        start_angle: f32,
    ) -> usize {
        let t = match self.gradient_type {
            GradientType::Linear => (px * dx + py * dy) / len2,
            GradientType::Radial => ((px * px + py * py).sqrt()) / len,
            GradientType::Angle => {
                let angle = py.atan2(px);
                let diff = (angle - start_angle).rem_euclid(std::f32::consts::TAU);
                diff / std::f32::consts::TAU
            }
            GradientType::Reflected => ((px * dx + py * dy) / len2).abs(),
            GradientType::Diamond => {
                let proj_x = (px * dx + py * dy) / len;
                let proj_y = (px * (-dy) + py * dx) / len;
                (proj_x.abs() + proj_y.abs()) / len
            }
        };
        let t = (if self.reverse { 1.0 - t } else { t }).clamp(0.0, 1.0);
        (t * (GRADIENT_LUT_SIZE - 1) as f32).round() as usize
    }
}

impl Tool for GradientTool {
    fn id(&self) -> &'static str {
        "gradient"
    }
    fn name(&self) -> &str {
        "Gradient"
    }
    fn shortcut(&self) -> Option<char> {
        Some('G')
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::Gradient
    }
    fn paints(&self) -> bool {
        true
    }

    fn on_press(&mut self, event: PointerEvent, _ctx: &mut ToolCtx) -> ToolResponse {
        self.start_x = event.canvas_x;
        self.start_y = event.canvas_y;
        self.cur_x = event.canvas_x;
        self.cur_y = event.canvas_y;
        self.is_dragging = true;
        ToolResponse::none()
    }

    fn on_drag(
        &mut self,
        event: PointerEvent,
        _prev: &PointerEvent,
        _ctx: &mut ToolCtx,
    ) -> ToolResponse {
        // Shift constrains to 45° increments (standard raster editors).
        let (mut ex, mut ey) = (event.canvas_x, event.canvas_y);
        if event.shift {
            let dx = ex - self.start_x;
            let dy = ey - self.start_y;
            let len = (dx * dx + dy * dy).sqrt();
            if len > 0.0 {
                let ang = (dy.atan2(dx) / (std::f32::consts::PI / 4.0)).round()
                    * (std::f32::consts::PI / 4.0);
                ex = self.start_x + len * ang.cos();
                ey = self.start_y + len * ang.sin();
            }
        }
        self.cur_x = ex;
        self.cur_y = ey;
        ToolResponse::redraw()
    }

    fn on_release(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        let (mut ex, mut ey) = (event.canvas_x, event.canvas_y);
        if event.shift {
            let dx = ex - self.start_x;
            let dy = ey - self.start_y;
            let len = (dx * dx + dy * dy).sqrt();
            if len > 0.0 {
                let ang = (dy.atan2(dx) / (std::f32::consts::PI / 4.0)).round()
                    * (std::f32::consts::PI / 4.0);
                ex = self.start_x + len * ang.cos();
                ey = self.start_y + len * ang.sin();
            }
        }
        self.is_dragging = false;
        if !Canvas::fits_flat_buffer(ctx.canvas().width, ctx.canvas().height) {
            return ToolResponse::blocked(
                "Gradient không hỗ trợ canvas > 25M pixels (Viewport Streaming mode)",
            );
        }
        self.apply_gradient(ctx.canvas_mut(), ex, ey);
        ToolResponse::repaint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradient_samples_hold_edge_stop_colors() {
        let gradient = Gradient {
            name: "edge hold".to_string(),
            stops: vec![
                GradientStop {
                    position: 0.25,
                    color: [255, 0, 0, 255],
                },
                GradientStop {
                    position: 0.75,
                    color: [0, 0, 0, 255],
                },
            ],
        };

        assert_eq!(gradient.sample(0.0), [255, 0, 0, 255]);
        assert_eq!(gradient.sample(1.0), [0, 0, 0, 255]);
    }

    #[test]
    fn gradient_sampling_sorts_stops_before_interpolating() {
        let gradient = Gradient {
            name: "sorted".to_string(),
            stops: vec![
                GradientStop {
                    position: 1.0,
                    color: [255, 255, 255, 255],
                },
                GradientStop {
                    position: 0.0,
                    color: [0, 0, 0, 255],
                },
            ],
        };

        assert_eq!(gradient.sample(0.5), [127, 127, 127, 255]);
    }

    #[test]
    fn gradient_respects_lock_alpha_without_expanding_content_bounds() {
        let mut canvas = Canvas::new_blank(32, 32);
        let idx = canvas.layer_stack.add_layer(32, 32);
        let layer = &mut canvas.layer_stack.layers[idx];
        layer.lock_alpha = true;
        for y in 10..14 {
            for x in 10..14 {
                layer.tiles.set_pixel(x, y, 20, 30, 40, 128);
            }
        }
        assert_eq!(layer.tiles.content_bounds(), Some((10, 10, 14, 14)));

        let mut tool = GradientTool::new();
        tool.gradient = Gradient {
            name: "test".to_string(),
            stops: vec![
                GradientStop {
                    position: 0.0,
                    color: [255, 0, 0, 255],
                },
                GradientStop {
                    position: 1.0,
                    color: [0, 0, 255, 255],
                },
            ],
        };
        tool.start_x = 0.0;
        tool.start_y = 0.0;
        tool.apply_gradient(&mut canvas, 31.0, 0.0);

        let layer = &canvas.layer_stack.layers[idx];
        assert_eq!(layer.tiles.content_bounds(), Some((10, 10, 14, 14)));
        assert_eq!(layer.tiles.get_pixel(0, 0).3, 0);
        assert_eq!(layer.tiles.get_pixel(10, 10).3, 128);
        assert_ne!(layer.tiles.get_pixel(10, 10).0, 20);
    }

    #[test]
    fn gradient_defaults_to_existing_content_bounds_on_nonempty_layer() {
        let mut canvas = Canvas::new_blank(32, 32);
        let idx = canvas.layer_stack.add_layer(32, 32);
        let layer = &mut canvas.layer_stack.layers[idx];
        for y in 10..14 {
            for x in 10..14 {
                layer.tiles.set_pixel(x, y, 20, 30, 40, 255);
            }
        }
        assert_eq!(layer.tiles.content_bounds(), Some((10, 10, 14, 14)));

        let mut tool = GradientTool::new();
        tool.start_x = 0.0;
        tool.start_y = 0.0;
        tool.apply_gradient(&mut canvas, 31.0, 0.0);

        let layer = &canvas.layer_stack.layers[idx];
        assert_eq!(layer.tiles.content_bounds(), Some((10, 10, 14, 14)));
        assert_eq!(layer.tiles.get_pixel(0, 0).3, 0);
        assert_eq!(layer.tiles.get_pixel(10, 10).3, 255);
        assert_eq!(layer.tiles.get_pixel(13, 13).3, 255);
    }

    #[test]
    fn gradient_clips_to_active_selection() {
        let mut canvas = Canvas::new_blank(16, 16);
        let idx = canvas.layer_stack.add_layer(16, 16);
        canvas.selection.select_rect(4, 5, 9, 11);

        let mut tool = GradientTool::new();
        tool.start_x = 0.0;
        tool.start_y = 0.0;
        tool.apply_gradient(&mut canvas, 15.0, 0.0);

        let layer = &canvas.layer_stack.layers[idx];
        assert_eq!(layer.tiles.content_bounds(), Some((4, 5, 9, 11)));
        assert_eq!(layer.tiles.get_pixel(3, 5).3, 0);
        assert_eq!(layer.tiles.get_pixel(4, 5).3, 255);
        assert_eq!(layer.tiles.get_pixel(8, 10).3, 255);
        assert_eq!(layer.tiles.get_pixel(9, 10).3, 0);
    }
}

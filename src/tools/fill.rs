#![allow(dead_code)]
use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::canvas::Canvas;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FillMode {
    Foreground,
    Background,
    Pattern,
}

pub struct FillTool {
    pub tolerance: u8,
    pub anti_alias: bool,
    pub contiguous: bool,
    pub sample_merged: bool,
    pub fill_mode: FillMode,
    pub opacity: f32,
    pub color: [u8; 4],
}

impl FillTool {
    pub fn new() -> Self {
        Self {
            tolerance: 32,
            anti_alias: true,
            contiguous: true,
            sample_merged: false,
            fill_mode: FillMode::Foreground,
            opacity: 1.0,
            color: [0, 0, 0, 255],
        }
    }

    fn color_diff(a: [u8; 4], b: [u8; 4]) -> u32 {
        let dr = (a[0] as i32 - b[0] as i32).unsigned_abs();
        let dg = (a[1] as i32 - b[1] as i32).unsigned_abs();
        let db = (a[2] as i32 - b[2] as i32).unsigned_abs();
        let da = (a[3] as i32 - b[3] as i32).unsigned_abs();
        (dr + dg + db + da) / 4
    }

    fn get_canvas_pixel(canvas: &Canvas, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * canvas.width + x) * 4) as usize;
        if i + 3 < canvas.pixels.len() {
            [
                canvas.pixels[i],
                canvas.pixels[i + 1],
                canvas.pixels[i + 2],
                canvas.pixels[i + 3],
            ]
        } else {
            [0, 0, 0, 0]
        }
    }

    fn get_layer_pixel(canvas: &mut Canvas, active_idx: usize, x: u32, y: u32) -> [u8; 4] {
        if let Some(tiles) = canvas.layer_stack.layers[active_idx].get_paint_tiles_mut() {
            let (r, g, b, a) = tiles.get_pixel(x, y);
            [r, g, b, a]
        } else {
            [0, 0, 0, 0]
        }
    }

    pub fn flood_fill(
        &self,
        canvas: &mut Canvas,
        start_x: u32,
        start_y: u32,
        fill_color: [u8; 4],
        target_idxs: &[usize],
    ) {
        if start_x >= canvas.width || start_y >= canvas.height {
            return;
        }

        if !Canvas::fits_flat_buffer(canvas.width, canvas.height) {
            return;
        }

        let w = canvas.width;
        let h = canvas.height;
        canvas.layer_stack.normalize_active_idx();
        let active_idx = canvas.layer_stack.active_idx;
        let sample_merged = self.sample_merged;

        if sample_merged {
            canvas.ensure_pixels();
        }
        let src_pixels = if sample_merged {
            canvas.pixels.clone()
        } else {
            let Some(buf_len) = Canvas::guarded_flat_rgba_len(w, h) else {
                return;
            };
            let mut buf = vec![0u8; buf_len];
            canvas.layer_stack.layers[active_idx].blend_onto_region(&mut buf, w, 0, 0, w, h);
            buf
        };

        let get_px = |x: u32, y: u32| -> [u8; 4] {
            let i = ((y * w + x) * 4) as usize;
            if i + 3 < src_pixels.len() {
                [
                    src_pixels[i],
                    src_pixels[i + 1],
                    src_pixels[i + 2],
                    src_pixels[i + 3],
                ]
            } else {
                [0, 0, 0, 0]
            }
        };

        let target_color = get_px(start_x, start_y);
        if Self::color_diff(target_color, fill_color) == 0 {
            return;
        }

        if canvas.selection.active && !canvas.selection.is_selected(start_x, start_y) {
            return;
        }

        let tolerance = self.tolerance as u32;
        let mut mask = vec![false; w as usize * h as usize];

        let mut min_x: u32;
        let mut min_y: u32;
        let mut max_x: u32;
        let mut max_y: u32;

        if self.contiguous {
            min_x = start_x;
            min_y = start_y;
            max_x = start_x;
            max_y = start_y;

            let mut queue = std::collections::VecDeque::with_capacity(1024);
            queue.push_back((start_x, start_y));
            mask[(start_y * w + start_x) as usize] = true;

            while let Some((x, y)) = queue.pop_front() {
                macro_rules! try_neighbor {
                    ($nx:expr, $ny:expr) => {
                        let vi = ($ny * w + $nx) as usize;
                        if !mask[vi]
                            && Self::color_diff(target_color, get_px($nx, $ny)) <= tolerance
                            && (!canvas.selection.active || canvas.selection.is_selected($nx, $ny))
                        {
                            mask[vi] = true;
                            if $nx < min_x {
                                min_x = $nx;
                            }
                            if $nx > max_x {
                                max_x = $nx;
                            }
                            if $ny < min_y {
                                min_y = $ny;
                            }
                            if $ny > max_y {
                                max_y = $ny;
                            }
                            queue.push_back(($nx, $ny));
                        }
                    };
                }
                if x > 0 {
                    try_neighbor!(x - 1, y);
                }
                if x + 1 < w {
                    try_neighbor!(x + 1, y);
                }
                if y > 0 {
                    try_neighbor!(x, y - 1);
                }
                if y + 1 < h {
                    try_neighbor!(x, y + 1);
                }
            }
        } else {
            min_x = w;
            min_y = h;
            max_x = 0;
            max_y = 0;
            for y in 0..h {
                for x in 0..w {
                    if Self::color_diff(target_color, get_px(x, y)) <= tolerance
                        && (!canvas.selection.active || canvas.selection.is_selected(x, y))
                    {
                        mask[(y * w + x) as usize] = true;
                        if x < min_x {
                            min_x = x;
                        }
                        if x > max_x {
                            max_x = x;
                        }
                        if y < min_y {
                            min_y = y;
                        }
                        if y > max_y {
                            max_y = y;
                        }
                    }
                }
            }
            if min_x > max_x {
                return;
            }
        }

        let tile_size = crate::core::tile::TILE_SIZE;
        let opacity = self.opacity;
        let src_a = (fill_color[3] as f32 / 255.0) * opacity;

        if src_a < 0.001 {
            return;
        }
        let src_r = fill_color[0] as f32 / 255.0;
        let src_g = fill_color[1] as f32 / 255.0;
        let src_b = fill_color[2] as f32 / 255.0;

        if min_x > max_x || min_y > max_y {
            return;
        }

        // CMYK: fill blends the colour's ink into the plane (source-over on the
        // mirror alpha) and re-projects each filled pixel. Per-pixel projection
        // is fine here (fill is one-shot); an ICC profile makes a full-canvas
        // fill slow, the built-in naive space is instant.
        let cmyk_conv = canvas.cmyk_converter();
        let fg_ink = cmyk_conv
            .as_ref()
            .map(|c| c.rgb_to_cmyk_one([fill_color[0], fill_color[1], fill_color[2]]));
        // Plate gate: a single-ink selection fills only those plates (alpha kept).
        let plate_gate = if cmyk_conv.is_some() {
            canvas.channels.write_gate()
        } else {
            None
        };

        for &layer_idx in target_idxs {
            if layer_idx >= canvas.layer_stack.layers.len() {
                continue;
            }
            if canvas.layer_stack.layers[layer_idx].locked {
                continue;
            }

            let ox = canvas.layer_stack.layers[layer_idx].offset.0;
            let oy = canvas.layer_stack.layers[layer_idx].offset.1;
            let tile_w = canvas.layer_stack.layers[layer_idx].width as i32;
            let tile_h = canvas.layer_stack.layers[layer_idx].height as i32;

            // Channels-panel write gate: fill the colour's luma into the
            // enabled channels only (alpha untouched). Mask fills stay
            // grayscale and skip the gate.
            let channel_wm = if canvas.layer_stack.layers[layer_idx].paint_target
                == crate::core::layer::PaintTarget::Mask
            {
                None
            } else {
                canvas.channels.write_gate()
            };
            let fill_luma = crate::core::tile::luma_u8(fill_color[0], fill_color[1], fill_color[2])
                as f32
                / 255.0;

            let tiles = if let Some(t) = canvas.layer_stack.layers[layer_idx].get_paint_tiles_mut()
            {
                t
            } else {
                continue;
            };

            for y in min_y..=max_y {
                let layer_y = y as i32 - oy;
                if layer_y < 0 || layer_y >= tile_h {
                    continue;
                }
                let ty = layer_y.div_euclid(tile_size as i32);
                let py = layer_y.rem_euclid(tile_size as i32) as u32;
                let row_offset = py * tile_size;

                let mut x = min_x;
                while x <= max_x {
                    if !mask[(y * w + x) as usize] {
                        x += 1;
                        continue;
                    }

                    let layer_x = x as i32 - ox;
                    if layer_x < 0 {
                        x += 1;
                        continue;
                    }
                    if layer_x >= tile_w {
                        break;
                    }
                    let tx = layer_x.div_euclid(tile_size as i32);
                    let px = layer_x.rem_euclid(tile_size as i32) as u32;

                    let pos = crate::core::tile::TilePos { x: tx, y: ty };
                    let tile = if cmyk_conv.is_some() {
                        tiles.get_tile_mut_ink(pos)
                    } else {
                        tiles.get_tile_mut(pos)
                    };

                    let max_run = (tile_size - px).min(max_x - x + 1);
                    for i in 0..max_run {
                        let cx = x + i;
                        if !mask[(y * w + cx) as usize] {
                            continue;
                        }

                        let mut sa = src_a;
                        if canvas.selection.active {
                            sa *= canvas.selection.sample(cx, y);
                        }
                        if sa < 0.001 {
                            continue;
                        }

                        let di = (row_offset + px + i) as usize * 4;
                        if let (Some(conv), Some(fink)) = (cmyk_conv.as_ref(), fg_ink.as_ref()) {
                            let ink_plane = tile.ink.as_mut().expect("cmyk tile carries ink");
                            if let Some(wm) = plate_gate {
                                // Plate fill: only the enabled ink channels move;
                                // alpha and the other plates stay untouched.
                                let mut ink = [
                                    ink_plane[di],
                                    ink_plane[di + 1],
                                    ink_plane[di + 2],
                                    ink_plane[di + 3],
                                ];
                                for c in 0..4 {
                                    if wm[c] {
                                        let d = ink_plane[di + c] as f32 / 255.0;
                                        let s = fink[c] as f32 / 255.0;
                                        let v = ((d + (s - d) * sa) * 255.0).round() as u8;
                                        ink_plane[di + c] = v;
                                        ink[c] = v;
                                    }
                                }
                                let rgb = conv.cmyk_to_rgb_one(ink);
                                tile.pixels[di] = rgb[0];
                                tile.pixels[di + 1] = rgb[1];
                                tile.pixels[di + 2] = rgb[2];
                                continue;
                            }
                            let dst_a = tile.pixels[di + 3] as f32 / 255.0;
                            let new_a = sa + dst_a * (1.0 - sa);
                            if new_a < 0.001 {
                                continue;
                            }
                            let dw = dst_a * (1.0 - sa);
                            let mut ink = [0u8; 4];
                            for c in 0..4 {
                                let d = ink_plane[di + c] as f32 / 255.0;
                                let s = fink[c] as f32 / 255.0;
                                let v = (((s * sa + d * dw) / new_a) * 255.0)
                                    .round()
                                    .clamp(0.0, 255.0)
                                    as u8;
                                ink_plane[di + c] = v;
                                ink[c] = v;
                            }
                            let rgb = conv.cmyk_to_rgb_one(ink);
                            tile.pixels[di] = rgb[0];
                            tile.pixels[di + 1] = rgb[1];
                            tile.pixels[di + 2] = rgb[2];
                            tile.pixels[di + 3] = (new_a * 255.0).round() as u8;
                            continue;
                        }
                        if let Some(wm) = channel_wm {
                            crate::core::tile::blend_masked(
                                &mut tile.pixels[di..di + 4],
                                [fill_luma; 3],
                                sa,
                                wm,
                            );
                            continue;
                        }
                        let dst_r8 = tile.pixels[di];
                        let dst_g8 = tile.pixels[di + 1];
                        let dst_b8 = tile.pixels[di + 2];
                        let dst_a8 = tile.pixels[di + 3];

                        let dst_r = dst_r8 as f32 / 255.0;
                        let dst_g = dst_g8 as f32 / 255.0;
                        let dst_b = dst_b8 as f32 / 255.0;
                        let dst_a = dst_a8 as f32 / 255.0;

                        let inv_src_a = 1.0 - sa;
                        let dst_weight = dst_a * inv_src_a;
                        let out_a = sa + dst_weight;

                        let out_r = (src_r * sa + dst_r * dst_weight) / out_a;
                        let out_g = (src_g * sa + dst_g * dst_weight) / out_a;
                        let out_b = (src_b * sa + dst_b * dst_weight) / out_a;

                        tile.pixels[di] = (out_r * 255.0) as u8;
                        tile.pixels[di + 1] = (out_g * 255.0) as u8;
                        tile.pixels[di + 2] = (out_b * 255.0) as u8;
                        tile.pixels[di + 3] = (out_a * 255.0) as u8;
                    }
                    x += max_run;
                }
            }
        }

        canvas.mark_dirty(min_x, min_y, max_x + 1, max_y + 1);
    }
}

impl Tool for FillTool {
    fn id(&self) -> &'static str {
        "fill"
    }
    fn name(&self) -> &str {
        "Fill"
    }
    fn shortcut(&self) -> Option<char> {
        Some('F')
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::Fill
    }
    fn paints(&self) -> bool {
        true
    }

    fn on_press(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        let (cx, cy) = (event.canvas_x, event.canvas_y);
        {
            let canvas = ctx.canvas_mut();
            canvas.layer_stack.normalize_active_idx();
            if cx < 0.0 || cy < 0.0 || cx >= canvas.width as f32 || cy >= canvas.height as f32 {
                return ToolResponse::none();
            }
            if !Canvas::fits_flat_buffer(canvas.width, canvas.height) {
                return ToolResponse::blocked(
                    "Fill không hỗ trợ canvas > 25M pixels (Viewport Streaming mode)",
                );
            }
        }

        let target_idxs: Vec<usize> = {
            let canvas = ctx.canvas();
            let active = canvas.layer_stack.active_idx;
            let selected: Vec<usize> = canvas
                .layer_stack
                .layers
                .iter()
                .enumerate()
                .filter(|(_, l)| l.selected && !l.locked && l.get_paint_tiles().is_some())
                .map(|(i, _)| i)
                .collect();
            if selected.len() > 1 {
                selected
            } else {
                vec![active]
            }
        };

        let fill_color = self.color;

        if target_idxs.len() == 1 {
            ctx.canvas_mut().begin_stroke("Fill");
            self.flood_fill(
                ctx.canvas_mut(),
                cx as u32,
                cy as u32,
                fill_color,
                &target_idxs,
            );
        } else {
            let mut cmd = {
                let c = ctx.canvas();
                crate::core::command::LayerStructureCommand::capture_before(
                    "Fill (Multi-layer)",
                    &c.layer_stack,
                    c.width,
                    c.height,
                )
            };
            self.flood_fill(
                ctx.canvas_mut(),
                cx as u32,
                cy as u32,
                fill_color,
                &target_idxs,
            );
            {
                let c = ctx.canvas();
                cmd.capture_after(&c.layer_stack, c.width, c.height);
            }
            ctx.canvas_mut().record(Box::new(cmd));
        }

        ToolResponse::repaint()
    }

    fn on_drag(
        &mut self,
        _event: PointerEvent,
        _prev: &PointerEvent,
        _ctx: &mut ToolCtx,
    ) -> ToolResponse {
        ToolResponse::none()
    }

    fn on_release(&mut self, _event: PointerEvent, _ctx: &mut ToolCtx) -> ToolResponse {
        ToolResponse::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_gate_fill_blends_luma_and_keeps_alpha() {
        let mut canvas = Canvas::new_blank(16, 16);
        let idx = canvas.layer_stack.active_idx;
        if let Some(tiles) = canvas.layer_stack.layers[idx].get_paint_tiles_mut() {
            for y in 0..16 {
                for x in 0..16 {
                    tiles.set_pixel(x, y, 200, 60, 20, 180);
                }
            }
        }
        canvas.channels.select_color(0, false); // Red plate only

        let mut fill = FillTool::new();
        fill.tolerance = 255; // flood everything
        fill.opacity = 1.0;
        // Fill with black: luma 0 -> red plate goes to 0.
        fill.flood_fill(&mut canvas, 8, 8, [0, 0, 0, 255], &[idx]);

        let (r, g, b, a) = canvas.layer_stack.layers[idx].tiles.get_pixel(8, 8);
        assert_eq!(r, 0, "red plate filled with the colour's luma");
        assert_eq!((g, b, a), (60, 20, 180), "G/B/alpha untouched");
    }

    #[test]
    fn fill_writes_ink_on_cmyk_and_keeps_mirror() {
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

        let mut fill = FillTool::new();
        fill.tolerance = 255;
        fill.opacity = 1.0;
        canvas.begin_stroke("Fill");
        fill.flood_fill(&mut canvas, 8, 8, [0, 128, 255, 255], &[idx]);

        let mut ink = [0u8; 4];
        canvas.layer_stack.layers[idx]
            .tiles
            .extract_ink_region_into(8, 8, 1, 1, &mut ink);
        assert_eq!(ink, crate::core::cms::naive_rgb_to_cmyk([0, 128, 255]));
        let (r, g, b, a) = canvas.layer_stack.layers[idx].tiles.get_pixel(8, 8);
        assert_eq!([r, g, b], naive_cmyk_to_rgb(ink), "mirror must project ink");
        assert_eq!(a, 255);

        canvas.end_stroke();
        canvas.undo();
        let mut ink0 = [9u8; 4];
        canvas.layer_stack.layers[idx]
            .tiles
            .extract_ink_region_into(8, 8, 1, 1, &mut ink0);
        assert_eq!(ink0, [0, 0, 0, 0], "undo restores white (no ink)");
    }
}

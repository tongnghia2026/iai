#![allow(dead_code)]
use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::canvas::Canvas;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SampleSize {
    Point,
    Average3x3,
    Average5x5,
    Average11x11,
}

impl SampleSize {
    pub fn name(&self) -> &str {
        match self {
            SampleSize::Point => "Point Sample",
            SampleSize::Average3x3 => "3x3 Average",
            SampleSize::Average5x5 => "5x5 Average",
            SampleSize::Average11x11 => "11x11 Average",
        }
    }
    pub fn radius(&self) -> u32 {
        match self {
            SampleSize::Point => 0,
            SampleSize::Average3x3 => 1,
            SampleSize::Average5x5 => 2,
            SampleSize::Average11x11 => 5,
        }
    }
}

pub struct EyedropperTool {
    pub sample_size: SampleSize,
    pub sample_merged: bool,
    pub picked_color: Option<[u8; 4]>,
    /// Session palette built from explicit canvas picks (oldest to newest).
    pub picked_colors: Vec<[u8; 4]>,
}

impl EyedropperTool {
    pub fn new() -> Self {
        Self {
            sample_size: SampleSize::Point,
            sample_merged: true,
            picked_color: None,
            picked_colors: Vec::new(),
        }
    }

    pub fn remember_color(&mut self, color: [u8; 4]) {
        self.picked_color = Some(color);
        if self.picked_colors.last().copied() == Some(color) {
            return;
        }
        const MAX_PICKED_COLORS: usize = 24;
        if self.picked_colors.len() == MAX_PICKED_COLORS {
            self.picked_colors.remove(0);
        }
        self.picked_colors.push(color);
    }

    pub fn sample(&self, canvas: &Canvas, cx: u32, cy: u32) -> [u8; 4] {
        let r = self.sample_size.radius();

        let get_px = |x: u32, y: u32| -> [u8; 4] {
            if self.sample_merged {
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
            } else {
                let active_layer = &canvas.layer_stack.layers[canvas.layer_stack.active_idx];
                let ox = active_layer.offset.0;
                let oy = active_layer.offset.1;
                let layer_x = x as i32 - ox;
                let layer_y = y as i32 - oy;
                if layer_x < 0 || layer_y < 0 {
                    return [0, 0, 0, 0];
                }

                if let Some(tiles) = active_layer.get_paint_tiles() {
                    let (lr, lg, lb, la) = tiles.get_pixel(layer_x as u32, layer_y as u32);
                    [lr, lg, lb, la]
                } else {
                    [0, 0, 0, 0]
                }
            }
        };

        if r == 0 {
            return get_px(cx, cy);
        }

        let mut sum = [0u32; 4];
        let mut count = 0u32;
        let x0 = cx.saturating_sub(r);
        let y0 = cy.saturating_sub(r);
        let x1 = (cx + r + 1).min(canvas.width);
        let y1 = (cy + r + 1).min(canvas.height);

        for y in y0..y1 {
            for x in x0..x1 {
                let c = get_px(x, y);
                sum[0] += c[0] as u32;
                sum[1] += c[1] as u32;
                sum[2] += c[2] as u32;
                sum[3] += c[3] as u32;
                count += 1;
            }
        }

        if count == 0 {
            return [0, 0, 0, 255];
        }
        [
            (sum[0] / count) as u8,
            (sum[1] / count) as u8,
            (sum[2] / count) as u8,
            (sum[3] / count) as u8,
        ]
    }
}

impl Tool for EyedropperTool {
    fn id(&self) -> &'static str {
        "eyedropper"
    }
    fn name(&self) -> &str {
        "Eyedropper"
    }
    fn shortcut(&self) -> Option<char> {
        Some('I')
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::Eyedropper
    }

    fn on_press(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        let (cx, cy) = (event.canvas_x, event.canvas_y);
        let w = ctx.canvas().width as f32;
        let h = ctx.canvas().height as f32;
        if cx < 0.0 || cy < 0.0 || cx >= w || cy >= h {
            return ToolResponse::none();
        }
        ctx.canvas_mut().ensure_pixels();
        self.picked_color = Some(self.sample(ctx.canvas(), cx as u32, cy as u32));
        ToolResponse::none()
    }

    fn on_drag(
        &mut self,
        event: PointerEvent,
        _prev: &PointerEvent,
        ctx: &mut ToolCtx,
    ) -> ToolResponse {
        let (cx, cy) = (event.canvas_x, event.canvas_y);
        let w = ctx.canvas().width as f32;
        let h = ctx.canvas().height as f32;
        if cx < 0.0 || cy < 0.0 || cx >= w || cy >= h {
            return ToolResponse::none();
        }
        ctx.canvas_mut().ensure_pixels();
        self.picked_color = Some(self.sample(ctx.canvas(), cx as u32, cy as u32));
        ToolResponse::none()
    }

    fn on_release(&mut self, _event: PointerEvent, _ctx: &mut ToolCtx) -> ToolResponse {
        ToolResponse::none()
    }
}

#[cfg(test)]
mod tests {
    use super::EyedropperTool;

    #[test]
    fn picked_palette_accumulates_and_skips_consecutive_duplicates() {
        let mut tool = EyedropperTool::new();
        let red = [255, 0, 0, 255];
        let blue = [0, 0, 255, 255];
        tool.remember_color(red);
        tool.remember_color(red);
        tool.remember_color(blue);
        assert_eq!(tool.picked_colors, vec![red, blue]);
        assert_eq!(tool.picked_color, Some(blue));
    }
}

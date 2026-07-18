//! Smudge drags pixels across the active layer. Unlike Brush, it does not paint a
//! fixed colour over the canvas; each dab pulls texture from the previous dab
//! position into the current dab footprint.

use super::scrub;
use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::tile::TileMap;

pub struct SmudgeTool {
    pub size: f32,
    pub hardness: f32,
    /// 0..1. How strongly each dab pulls the previous texture into its footprint.
    pub strength: f32,
    pub spacing: f32,
    /// standard raster editors "Finger Painting": start the smear with the foreground colour
    /// instead of only the colour already under the cursor.
    pub finger_painting: bool,

    finger: Option<[f32; 4]>,
    last: (f32, f32),
}

fn sample_tile_bilinear(tiles: &TileMap, x: f32, y: f32) -> [f32; 4] {
    if !x.is_finite() || !y.is_finite() || x < 0.0 || y < 0.0 {
        return [0.0; 4];
    }

    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let weights = [
        (1.0 - tx) * (1.0 - ty),
        tx * (1.0 - ty),
        (1.0 - tx) * ty,
        tx * ty,
    ];
    let coords = [(x0, y0), (x0 + 1, y0), (x0, y0 + 1), (x0 + 1, y0 + 1)];

    let mut alpha = 0.0_f32;
    let mut premul = [0.0_f32; 3];
    for ((sx, sy), weight) in coords.into_iter().zip(weights) {
        if sx < 0 || sy < 0 {
            continue;
        }
        let (r, g, b, a) = tiles.get_pixel(sx as u32, sy as u32);
        let a = a as f32 / 255.0;
        alpha += a * weight;
        premul[0] += r as f32 / 255.0 * a * weight;
        premul[1] += g as f32 / 255.0 * a * weight;
        premul[2] += b as f32 / 255.0 * a * weight;
    }

    if alpha <= f32::EPSILON {
        return [0.0; 4];
    }

    [
        (premul[0] / alpha).clamp(0.0, 1.0),
        (premul[1] / alpha).clamp(0.0, 1.0),
        (premul[2] / alpha).clamp(0.0, 1.0),
        alpha.clamp(0.0, 1.0),
    ]
}

fn blend_toward_rgba(dst: &mut [u8; 4], src: [f32; 4], amount: f32) {
    let amount = amount.clamp(0.0, 1.0);
    if amount <= 0.0 {
        return;
    }

    let da = dst[3] as f32 / 255.0;
    let dpm = [
        dst[0] as f32 / 255.0 * da,
        dst[1] as f32 / 255.0 * da,
        dst[2] as f32 / 255.0 * da,
    ];
    let sa = src[3].clamp(0.0, 1.0);
    let spm = [src[0] * sa, src[1] * sa, src[2] * sa];

    let out_a = da + (sa - da) * amount;
    let out_pm = [
        dpm[0] + (spm[0] - dpm[0]) * amount,
        dpm[1] + (spm[1] - dpm[1]) * amount,
        dpm[2] + (spm[2] - dpm[2]) * amount,
    ];

    if out_a <= f32::EPSILON {
        *dst = [0, 0, 0, 0];
        return;
    }

    dst[0] = ((out_pm[0] / out_a).clamp(0.0, 1.0) * 255.0).round() as u8;
    dst[1] = ((out_pm[1] / out_a).clamp(0.0, 1.0) * 255.0).round() as u8;
    dst[2] = ((out_pm[2] / out_a).clamp(0.0, 1.0) * 255.0).round() as u8;
    dst[3] = (out_a.clamp(0.0, 1.0) * 255.0).round() as u8;
}

fn mix_rgba(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let t = t.clamp(0.0, 1.0);
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
        a[3] + (b[3] - a[3]) * t,
    ]
}

impl SmudgeTool {
    pub fn new() -> Self {
        Self {
            size: 40.0,
            hardness: 0.0,
            strength: 0.5,
            spacing: 0.1,
            finger_painting: false,
            finger: None,
            last: (0.0, 0.0),
        }
    }

    fn smudge_dab(&mut self, ctx: &mut ToolCtx, prev_x: f32, prev_y: f32, cx: f32, cy: f32) {
        if ctx.canvas().layer_stack.layers.is_empty() {
            return;
        }
        let active_idx = ctx.canvas().layer_stack.active_idx;
        let Some(layer) = ctx.canvas().layer_stack.layers.get(active_idx) else {
            return;
        };
        if (!layer.is_background && layer.locked) || !layer.is_raster() {
            return;
        }

        let source_tiles = layer.tiles.clone();
        let layer_offset = layer.offset;
        let strength = self.strength.clamp(0.0, 1.0);
        let pull_dx = cx - prev_x;
        let pull_dy = cy - prev_y;
        let finger = self.finger;

        scrub::for_each_dab_pixel(
            ctx.canvas_mut(),
            cx,
            cy,
            self.size * 0.5,
            self.hardness,
            strength,
            |px, py, cov, dst| {
                let src_x = px as f32 + 0.5 - pull_dx;
                let src_y = py as f32 + 0.5 - pull_dy;
                let mut src = sample_tile_bilinear(&source_tiles, src_x, src_y);
                if let Some(finger) = finger {
                    src = mix_rgba(finger, src, 1.0 - strength);
                }
                blend_toward_rgba(dst, src, cov);
            },
        );

        if let Some(finger) = self.finger {
            let center = sample_tile_bilinear(
                &source_tiles,
                cx - layer_offset.0 as f32,
                cy - layer_offset.1 as f32,
            );
            self.finger = Some(mix_rgba(center, finger, strength));
        }
    }
}

impl Default for SmudgeTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for SmudgeTool {
    fn id(&self) -> &'static str {
        "smudge"
    }
    fn name(&self) -> &str {
        "Smudge"
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::Smudge
    }
    fn paints(&self) -> bool {
        true
    }
    fn cursor_size(&self) -> f32 {
        self.size * 0.5
    }

    fn on_press(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        ctx.canvas_mut().begin_stroke("Smudge");
        self.last = (event.canvas_x, event.canvas_y);
        self.finger = if self.finger_painting {
            let fg = ctx.fg_color;
            Some([
                fg[0] as f32 / 255.0,
                fg[1] as f32 / 255.0,
                fg[2] as f32 / 255.0,
                1.0,
            ])
        } else {
            None
        };
        ToolResponse::repaint()
    }

    fn on_drag(
        &mut self,
        event: PointerEvent,
        _prev: &PointerEvent,
        ctx: &mut ToolCtx,
    ) -> ToolResponse {
        let (x0, y0) = self.last;
        let (x1, y1) = (event.canvas_x, event.canvas_y);
        let mut pts: Vec<(f32, f32)> = Vec::new();
        scrub::dab_segment(x0, y0, x1, y1, self.size, self.spacing, |x, y| {
            pts.push((x, y))
        });

        let mut prev = (x0, y0);
        for (x, y) in pts {
            let dist2 = (x - prev.0).powi(2) + (y - prev.1).powi(2);
            if dist2 > 0.0001 {
                self.smudge_dab(ctx, prev.0, prev.1, x, y);
                prev = (x, y);
            }
        }

        self.last = (x1, y1);
        ToolResponse::repaint()
    }

    fn on_release(&mut self, _event: PointerEvent, _ctx: &mut ToolCtx) -> ToolResponse {
        self.finger = None;
        ToolResponse::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tile::TileMap;

    #[test]
    fn bilinear_sample_keeps_transparent_edges_from_darkening_colour() {
        let mut tiles = TileMap::new(2, 1);
        tiles.set_pixel(0, 0, 255, 0, 0, 255);
        tiles.set_pixel(1, 0, 0, 0, 0, 0);

        let c = sample_tile_bilinear(&tiles, 0.5, 0.0);
        assert!(c[0] > 0.99, "red should stay red across alpha edge: {c:?}");
        assert!(
            c[3] > 0.45 && c[3] < 0.55,
            "alpha should interpolate: {c:?}"
        );
    }

    #[test]
    fn blend_toward_uses_premultiplied_alpha() {
        let mut dst = [0, 0, 255, 255];
        blend_toward_rgba(&mut dst, [1.0, 0.0, 0.0, 0.5], 0.5);

        assert!(dst[0] > 0, "source red should contribute");
        assert!(dst[2] > 0, "destination blue should remain");
        assert!(dst[3] > 180, "alpha should not collapse");
    }
}

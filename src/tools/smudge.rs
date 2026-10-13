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

/// Bilinear read of a layer mask's gray value (red byte) at layer-local `(x, y)`,
/// pixel centres on integers. Clamp-to-edge like `LayerMask::sample`; missing
/// tiles read as black, which is what an unpainted hide-all mask holds.
fn sample_mask_bilinear(tiles: &TileMap, w: u32, h: u32, x: f32, y: f32) -> f32 {
    if w == 0 || h == 0 || !x.is_finite() || !y.is_finite() {
        return 0.0;
    }
    let x = x.clamp(0.0, (w - 1) as f32);
    let y = y.clamp(0.0, (h - 1) as f32);
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let v = |px: u32, py: u32| tiles.get_pixel(px, py).0 as f32 / 255.0;
    let top = v(x0, y0) + (v(x1, y0) - v(x0, y0)) * tx;
    let bottom = v(x0, y1) + (v(x1, y1) - v(x0, y1)) * tx;
    top + (bottom - top) * ty
}

fn luma(c: [f32; 4]) -> f32 {
    (c[0] * 0.2126 + c[1] * 0.7152 + c[2] * 0.0722).clamp(0.0, 1.0)
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
        if (!layer.is_background && layer.locked) || !scrub::can_scrub(layer) {
            return;
        }
        let layer_offset = layer.offset;
        let strength = self.strength.clamp(0.0, 1.0);
        let pull_dx = cx - prev_x;
        let pull_dy = cy - prev_y;
        let finger = self.finger;

        // A mask is smeared as its own gray values, never the layer's colours.
        if layer.paint_target == crate::core::layer::PaintTarget::Mask {
            let Some(mask) = layer.mask.as_ref() else {
                return;
            };
            let (mw, mh) = (mask.width, mask.height);
            let source_tiles = mask.tiles.clone();
            let finger_v = finger.map(luma);
            scrub::for_each_dab_pixel(
                ctx.canvas_mut(),
                cx,
                cy,
                self.size * 0.5,
                self.hardness,
                strength,
                |px, py, cov, dst| {
                    let mut src = sample_mask_bilinear(
                        &source_tiles,
                        mw,
                        mh,
                        px as f32 - pull_dx,
                        py as f32 - pull_dy,
                    );
                    if let Some(f) = finger_v {
                        src = f + (src - f) * (1.0 - strength);
                    }
                    let d = dst[0] as f32 / 255.0;
                    let v = ((d + (src - d) * cov) * 255.0).round().clamp(0.0, 255.0) as u8;
                    *dst = [v, v, v, 255];
                },
            );
            if let Some(f) = finger_v {
                let center = sample_mask_bilinear(
                    &source_tiles,
                    mw,
                    mh,
                    cx - 0.5 - layer_offset.0 as f32,
                    cy - 0.5 - layer_offset.1 as f32,
                );
                let v = center + (f - center) * strength;
                self.finger = Some([v, v, v, 1.0]);
            }
            return;
        }

        let source_tiles = layer.tiles.clone();

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
    fn tip_hardness(&self) -> Option<f32> {
        Some(self.hardness)
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
    use crate::core::canvas::Canvas;
    use crate::core::document::{Document, DocumentId};
    use crate::core::tile::TileMap;

    fn black_doc_with_white_mask() -> Document {
        let px = [0u8, 0, 0, 255].repeat(64 * 64);
        let mut doc = Document::from_canvas(DocumentId(1), Canvas::from_rgba(px, 64, 64), None);
        let idx = doc.canvas.layer_stack.active_idx;
        doc.canvas.layer_stack.layers[idx].add_mask(true);
        doc
    }

    fn drag(tool: &mut dyn Tool, doc: &mut Document, y: f32, xs: std::ops::Range<i32>) {
        let mut ctx = ToolCtx::new(doc, [0, 0, 0, 255], [255; 4], 1.0, 0.0, 0.0);
        let mut prev = PointerEvent::new(xs.start as f32, y);
        tool.on_press(prev, &mut ctx);
        for x in xs {
            let ev = PointerEvent::new(x as f32, y);
            tool.on_drag(ev, &prev, &mut ctx);
            prev = ev;
        }
        tool.on_release(prev, &mut ctx);
    }

    #[test]
    fn smudging_a_mask_never_pulls_in_the_layer_colours() {
        let mut doc = black_doc_with_white_mask();
        drag(&mut SmudgeTool::new(), &mut doc, 32.0, 20..44);
        let layer = doc.canvas.layer_stack.active_layer();
        let mask = layer.mask.as_ref().unwrap();
        assert_eq!(mask.tiles.get_pixel(32, 32), (255, 255, 255, 255));
        assert_eq!(layer.tiles.get_pixel(32, 32), (0, 0, 0, 255));
    }

    #[test]
    fn smudge_drags_a_mask_edge_along_the_stroke() {
        let mut doc = black_doc_with_white_mask();
        let idx = doc.canvas.layer_stack.active_idx;
        let mask = doc.canvas.layer_stack.layers[idx].mask.as_mut().unwrap();
        for y in 0..64 {
            for x in 32..64 {
                mask.tiles.set_pixel(x, y, 0, 0, 0, 255);
            }
        }
        // Drag from the revealed half into the hidden half.
        drag(&mut SmudgeTool::new(), &mut doc, 32.0, 24..44);
        let mask = doc.canvas.layer_stack.layers[idx].mask.as_ref().unwrap();
        let (v, g, b, a) = mask.tiles.get_pixel(36, 32);
        assert!(v > 40, "white is pushed past the edge, got {v}");
        assert_eq!((g, b, a), (v, v, 255), "mask stays opaque gray");
        assert_eq!(
            mask.tiles.get_pixel(36, 5).0,
            0,
            "outside the stroke untouched"
        );
    }

    #[test]
    fn smudge_and_dodge_edit_a_group_mask() {
        use crate::core::layer::Layer;
        let mut doc = black_doc_with_white_mask();
        let stack = &mut doc.canvas.layer_stack;
        let mut part = Layer::from_rgba(9, "part", [255, 0, 0, 255].repeat(64 * 64), 64, 64);
        part.selected = true;
        stack.layers.push(part);
        stack.set_next_id(10);
        let group = stack.create_group_from_selected(64, 64).unwrap();
        stack.layers[group].add_mask(true);
        stack.active_idx = group;
        let mask = stack.layers[group].mask.as_mut().unwrap();
        for y in 0..64 {
            for x in 0..64 {
                mask.tiles.set_pixel(x, y, 150, 150, 150, 255);
            }
        }

        drag(
            &mut crate::tools::dodge_burn::DodgeBurnTool::dodge(),
            &mut doc,
            32.0,
            20..44,
        );
        let v = |doc: &Document| {
            let mask = doc.canvas.layer_stack.layers[group].mask.as_ref().unwrap();
            mask.tiles.get_pixel(32, 32).0
        };
        assert!(v(&doc) > 150, "dodge lightens the folder's mask");

        let mask = doc.canvas.layer_stack.layers[group].mask.as_mut().unwrap();
        for x in 40..64 {
            for y in 0..64 {
                mask.tiles.set_pixel(x, y, 0, 0, 0, 255);
            }
        }
        drag(&mut SmudgeTool::new(), &mut doc, 32.0, 30..48);
        let mask = doc.canvas.layer_stack.layers[group].mask.as_ref().unwrap();
        assert!(
            mask.tiles.get_pixel(42, 32).0 > 30,
            "smudge moves the folder's mask edge"
        );
    }

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

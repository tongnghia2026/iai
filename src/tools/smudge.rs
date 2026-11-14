//! Smudge drags the picture the way a finger drags wet paint.
//!
//! The tip carries paint of its own: a tip-sized picture that travels with the
//! pointer. All along the stroke the two trade: the carried paint takes up a
//! little of the layer under it and lays a little of itself back down. The
//! Strength sets the balance. At 100 % what the press picked up is carried to
//! the end of the stroke; at 0 % nothing moves.
//!
//! The trade runs every pixel of travel (every 1/128 of a wide tip) and in
//! floating point, on a copy of the layer around the tip. Copying the layer
//! from one dab behind, a tenth of the tip away, drew a dragged edge as a
//! staircase with steps that wide, and 8-bit pixels cannot hold the small
//! amount a single step moves.

use rayon::prelude::*;

use super::scrub;
use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::canvas::Canvas;
use crate::core::layer::PaintTarget;
use crate::core::selection::Selection;

/// The widest tip Smudge works with: the carried paint and the working copy
/// of the layer take about 40 bytes per pixel of the tip.
pub const MAX_SIZE: f32 = 2500.0;

/// How often paint changes hands while the tip crosses its own width, at low
/// strength. Few, long rides fade a smear out; many short ones push the
/// picture along as a block. Photoshop at its default 25 % spacing is 4.
const TURNS: f32 = 5.0;
/// The same at full strength, where it only sets how closely the layer
/// follows the carried paint: within 1/20 of the tip.
const TURNS_MAX: f32 = 20.0;
/// The most paint that changes hands in one step along the stroke. More
/// shows as ridges across a wide tip's smear.
const STEP_SHARE: f32 = 1.0 / 16.0;
/// Tips at least this many cells wide trade on all cores.
const PARALLEL_FROM: usize = 128;

pub struct SmudgeTool {
    pub size: f32,
    pub hardness: f32,
    /// 0..1. The share of the picture under the tip that travels with it.
    pub strength: f32,
    /// Photoshop's "Finger Painting": start the smear with the foreground
    /// colour instead of the colour under the cursor.
    pub finger_painting: bool,

    stroke: Option<Stroke>,
    /// Where the pointer was at the last event.
    last: (f32, f32),
    /// Distance to the next step along the stroke (see `scrub::dab_path`).
    carry: f32,
}

/// The round tip as a grid of cells one layer pixel wide, the centre cell at
/// the pointer.
struct Tip {
    /// Cells from the centre cell to the rim; the grid is `n` × `n`.
    half: i32,
    n: usize,
    /// Per cell, the share of the layer that becomes carried paint in a step.
    lay: Vec<f32>,
    /// Per cell, the share of the carried paint that becomes layer in a step.
    pick: Vec<f32>,
    /// Per row, the cells `start..end` that reach the layer (`n..0`: none).
    rows: Vec<(usize, usize)>,
    /// Distance between two steps along the stroke.
    step: f32,
}

impl Tip {
    /// `centre` is where the pointer sits inside the centre cell, -0.5..0.5.
    fn new(size: f32, hardness: f32, strength: f32, centre: (f32, f32)) -> Self {
        let radius = (size * 0.5).max(0.5);
        let diameter = radius * 2.0;
        // One ring of cells beyond the rim: a layer pixel reads the four
        // cells around it.
        let half = (radius + 0.5).ceil() as i32 + 1;
        let n = (2 * half + 1) as usize;

        // A cell that should carry the share `c` of the picture lays paint
        // down `c / (1 - c)` times as often as it takes paint up. The picture
        // then travels `c / (1 - c)` pixels for every pixel the tip leaves
        // behind, as it does when each dab copies the share `c` from one dab
        // back.
        let lay_turns = |c: f32| {
            if c >= 1.0 {
                TURNS_MAX
            } else {
                (TURNS * c / (1.0 - c)).min(TURNS_MAX)
            }
        };
        let step = (diameter * STEP_SHARE / lay_turns(strength).max(8.0)).max(1.0);

        let mut lay = vec![0.0f32; n * n];
        let mut pick = vec![0.0f32; n * n];
        let mut inked = vec![(n, 0usize); n];
        lay.par_chunks_mut(n)
            .zip(pick.par_chunks_mut(n))
            .zip(inked.par_iter_mut())
            .enumerate()
            .for_each(|(j, ((lay, pick), inked))| {
                let dy = (j as i32 - half) as f32 - centre.1;
                for i in 0..n {
                    let dx = (i as i32 - half) as f32 - centre.0;
                    let d2 = dx * dx + dy * dy;
                    let cover = if d2 > radius * radius {
                        0.0
                    } else {
                        scrub::dab_coverage(d2, radius, hardness)
                    };
                    let c = strength * cover;
                    let lay_t = lay_turns(c);
                    let pick_t = if c > 0.0 {
                        lay_t * (1.0 - c) / c
                    } else {
                        TURNS
                    };
                    lay[i] = 1.0 - (-lay_t / diameter * step).exp();
                    pick[i] = 1.0 - (-pick_t / diameter * step).exp();
                    if c > 0.0 {
                        inked.0 = inked.0.min(i);
                        inked.1 = inked.1.max(i + 1);
                    }
                }
            });
        // A layer pixel reads the four cells around it, so the neighbours of
        // an inked cell count too.
        let rows = (0..n)
            .map(|j| {
                let near = &inked[j.saturating_sub(1)..=(j + 1).min(n - 1)];
                let a = near.iter().map(|r| r.0).min().unwrap_or(n);
                let b = near.iter().map(|r| r.1).max().unwrap_or(0);
                if a < b {
                    (a.saturating_sub(1), (b + 1).min(n))
                } else {
                    (n, 0)
                }
            })
            .collect();

        Self {
            half,
            n,
            lay,
            pick,
            rows,
            step,
        }
    }
}

/// A floating-point, premultiplied copy of the layer around the tip.
struct Window {
    x0: i32,
    y0: i32,
    w: usize,
    h: usize,
    px: Vec<[f32; 4]>,
    /// The layer-local rect (`x0, y0, x1, y1`, end exclusive) the layer has
    /// yet to take back.
    dirty: Option<[i32; 4]>,
    /// The layer no longer holds what the copy does (a channel was gated).
    stale: bool,
}

impl Window {
    fn holds(&self, x0: i32, y0: i32, x1: i32, y1: i32) -> bool {
        !self.stale
            && x0 >= self.x0
            && y0 >= self.y0
            && x1 <= self.x0 + self.w as i32
            && y1 <= self.y0 + self.h as i32
    }
}

/// One press-to-release drag.
struct Stroke {
    layer_id: u32,
    target: PaintTarget,
    offset: (i32, i32),
    lock_alpha: bool,
    tip: Tip,
    /// The paint the tip carries, premultiplied, one entry per tip cell.
    paint: Vec<[f32; 4]>,
    win: Window,
    /// The press, in canvas space, and the layer pixel its centre cell sat
    /// on. The cells start out on the pixel grid and leave it only by as much
    /// as the pointer has travelled, so a drag along an axis never blurs
    /// across it.
    press: (f32, f32),
    origin: (f32, f32),
}

fn to_paint(b: &[u8], gray: bool) -> [f32; 4] {
    const K: f32 = 1.0 / 255.0;
    if gray {
        let v = b[0] as f32 * K;
        [v, v, v, 1.0]
    } else {
        let a = b[3] as f32 * K;
        [
            b[0] as f32 * K * a,
            b[1] as f32 * K * a,
            b[2] as f32 * K * a,
            a,
        ]
    }
}

fn to_bytes(p: [f32; 4]) -> [u8; 4] {
    let a = p[3].clamp(0.0, 1.0);
    let a8 = (a * 255.0).round();
    if a8 < 1.0 {
        return [0; 4];
    }
    let q = |v: f32| ((v / a).clamp(0.0, 1.0) * 255.0).round() as u8;
    [q(p[0]), q(p[1]), q(p[2]), a8 as u8]
}

fn luma(c: [u8; 4]) -> f32 {
    (c[0] as f32 * 0.2126 + c[1] as f32 * 0.7152 + c[2] as f32 * 0.0722) / 255.0
}

/// The four weights of a bilinear read `fx, fy` into a cell.
fn corners(fx: f32, fy: f32) -> [f32; 4] {
    [
        (1.0 - fx) * (1.0 - fy),
        fx * (1.0 - fy),
        (1.0 - fx) * fy,
        fx * fy,
    ]
}

impl Stroke {
    fn begin(tool: &SmudgeTool, canvas: &mut Canvas, x: f32, y: f32, fg: [u8; 4]) -> Option<Self> {
        let strength = tool.strength.clamp(0.0, 1.0);
        if strength <= 0.0 {
            return None;
        }
        let (offset, _) = scrub::paint_frame(canvas)?;
        let layer = canvas.layer_stack.active_layer();
        let target = layer.paint_target;
        let gx = x - offset.0 as f32 - 0.5;
        let gy = y - offset.1 as f32 - 0.5;
        let origin = (gx.round(), gy.round());
        let tip = Tip::new(
            tool.size.min(MAX_SIZE),
            tool.hardness.clamp(0.0, 1.0),
            strength,
            (gx - origin.0, gy - origin.1),
        );
        let mut stroke = Self {
            layer_id: layer.id,
            target,
            offset,
            lock_alpha: target == PaintTarget::Pixels && layer.lock_alpha,
            paint: vec![[0.0; 4]; tip.n * tip.n],
            tip,
            win: Window {
                x0: 0,
                y0: 0,
                w: 0,
                h: 0,
                px: Vec::new(),
                dirty: None,
                stale: true,
            },
            press: (x, y),
            origin,
        };
        let (ix, iy) = (origin.0 as i32, origin.1 as i32);
        if !stroke.load_window(canvas, ix, iy) {
            return None;
        }
        if tool.finger_painting {
            let colour = if target == PaintTarget::Mask {
                let v = luma(fg);
                [v, v, v, 1.0]
            } else {
                to_paint(&[fg[0], fg[1], fg[2], 255], false)
            };
            stroke.paint.fill(colour);
        } else {
            stroke.pick_up(ix, iy, 0.0, 0.0, true);
        }
        Some(stroke)
    }

    /// Whether the layer the stroke began on is still the one to paint.
    fn still_on(&self, canvas: &mut Canvas) -> bool {
        if scrub::paint_frame(canvas).is_none() {
            return false;
        }
        let layer = canvas.layer_stack.active_layer();
        layer.id == self.layer_id && layer.paint_target == self.target
    }

    /// Copy the layer around the tip whose centre cell is at `(ix, iy)`.
    /// Past the layer's edge the edge pixel repeats, so a drag in from
    /// outside pulls the edge colour in, not a hole.
    fn load_window(&mut self, canvas: &Canvas, ix: i32, iy: i32) -> bool {
        let layer = canvas.layer_stack.active_layer();
        let Some(tiles) = layer.get_paint_tiles() else {
            return false;
        };
        let lw = layer.width.min(tiles.width) as i32;
        let lh = layer.height.min(tiles.height) as i32;
        if lw <= 0 || lh <= 0 {
            return false;
        }
        let gray = self.target == PaintTarget::Mask;
        let half = self.tip.half;
        // Room to travel before the next copy.
        let pad = half.clamp(16, 128);
        let (x0, y0) = (ix - half - pad, iy - half - pad);
        let side = (2 * (half + pad) + 2) as usize;

        let (sx0, sy0) = (x0.clamp(0, lw - 1), y0.clamp(0, lh - 1));
        let sx1 = (x0 + side as i32 - 1).clamp(0, lw - 1);
        let sy1 = (y0 + side as i32 - 1).clamp(0, lh - 1);
        let sw = (sx1 - sx0 + 1) as usize;
        let bytes = tiles.extract_region(sx0 as u32, sy0 as u32, sw as u32, (sy1 - sy0 + 1) as u32);
        let cols: Vec<usize> = (0..side as i32)
            .map(|i| ((x0 + i).clamp(sx0, sx1) - sx0) as usize * 4)
            .collect();

        let win = &mut self.win;
        win.px.clear();
        win.px.resize(side * side, [0.0; 4]);
        win.px
            .par_chunks_mut(side)
            .enumerate()
            .for_each(|(j, row)| {
                let sy = ((y0 + j as i32).clamp(sy0, sy1) - sy0) as usize;
                let src = &bytes[sy * sw * 4..(sy + 1) * sw * 4];
                for (dst, &c) in row.iter_mut().zip(&cols) {
                    *dst = to_paint(&src[c..c + 4], gray);
                }
            });
        (win.x0, win.y0, win.w, win.h) = (x0, y0, side, side);
        win.dirty = None;
        win.stale = false;
        true
    }

    /// Hand the changed pixels back to the layer.
    fn flush(&mut self, canvas: &mut Canvas) {
        let Some([x0, y0, x1, y1]) = self.win.dirty.take() else {
            return;
        };
        let win = &self.win;
        scrub::for_each_rect_pixel(
            canvas,
            x0.max(0) as u32,
            y0.max(0) as u32,
            x1.max(0) as u32,
            y1.max(0) as u32,
            |_, px, py, dst| {
                let i = (py as i32 - win.y0) as usize * win.w + (px as i32 - win.x0) as usize;
                let new = to_bytes(win.px[i]);
                if new == *dst || (new[3] == 0 && dst[3] == 0) {
                    return false;
                }
                *dst = new;
                true
            },
        );
        // A gated channel kept its old value in the layer, not in the copy.
        if self.target == PaintTarget::Pixels && canvas.channels.write_gate().is_some() {
            self.win.stale = true;
        }
    }

    /// One step of the trade with the pointer at canvas `(x, y)`.
    fn step_to(&mut self, canvas: &mut Canvas, x: f32, y: f32) {
        let gx = self.origin.0 + (x - self.press.0);
        let gy = self.origin.1 + (y - self.press.1);
        if !gx.is_finite() || !gy.is_finite() {
            return;
        }
        let (ix, iy) = (gx.floor() as i32, gy.floor() as i32);
        let (fx, fy) = (gx - ix as f32, gy - iy as f32);
        let half = self.tip.half;
        if !self
            .win
            .holds(ix - half, iy - half, ix + half + 2, iy + half + 2)
        {
            self.flush(canvas);
            if !self.load_window(canvas, ix, iy) {
                return;
            }
        }
        self.pick_up(ix, iy, fx, fy, false);
        let selection = canvas.selection.active.then_some(&canvas.selection);
        self.lay_down(ix, iy, fx, fy, selection);
    }

    /// The carried paint takes up the layer under it: each cell its own
    /// share, or all of it (`whole`, on the press).
    fn pick_up(&mut self, ix: i32, iy: i32, fx: f32, fy: f32, whole: bool) {
        let tip = &self.tip;
        let win = &self.win;
        let n = tip.n;
        let bx = (ix - tip.half - win.x0) as usize;
        let by = (iy - tip.half - win.y0) as usize;
        let k = corners(fx, fy);
        let row = |(j, paint): (usize, &mut [[f32; 4]])| {
            let (a, b) = tip.rows[j];
            if a >= b {
                return;
            }
            let top = &win.px[(by + j) * win.w + bx..][..n + 1];
            let bottom = &win.px[(by + j + 1) * win.w + bx..][..n + 1];
            let pick = &tip.pick[j * n..(j + 1) * n];
            for i in a..b {
                let share = if whole { 1.0 } else { pick[i] };
                if share <= 0.0 {
                    continue;
                }
                let p = &mut paint[i];
                for c in 0..4 {
                    let under = k[0] * top[i][c]
                        + k[1] * top[i + 1][c]
                        + k[2] * bottom[i][c]
                        + k[3] * bottom[i + 1][c];
                    p[c] += share * (under - p[c]);
                }
            }
        };
        if n >= PARALLEL_FROM {
            self.paint.par_chunks_mut(n).enumerate().for_each(row);
        } else {
            self.paint.chunks_mut(n).enumerate().for_each(row);
        }
    }

    /// The layer under the tip takes up the carried paint, inside the
    /// selection only.
    fn lay_down(&mut self, ix: i32, iy: i32, fx: f32, fy: f32, selection: Option<&Selection>) {
        let tip = &self.tip;
        let paint = &self.paint;
        let win = &mut self.win;
        let n = tip.n;
        let (ox, oy) = self.offset;
        let lock_alpha = self.lock_alpha;
        // The layer pixel under the tip's first cell. The pixel one right and
        // one below of it is the first with four cells around it.
        let (lx, ly) = (ix - tip.half, iy - tip.half);
        let bx = (lx - win.x0) as usize;
        let by = (ly - win.y0) as usize;
        // A pixel sits `1 - fx` past the cell left of it.
        let k = corners(1.0 - fx, 1.0 - fy);
        let width = win.w;
        let row = |(j, pixels): (usize, &mut [[f32; 4]])| {
            let (a0, b0) = tip.rows[j];
            let (a1, b1) = tip.rows[j + 1];
            let (a, b) = (a0.min(a1), b0.max(b1).min(n - 1));
            if a >= b {
                return;
            }
            let lay0 = &tip.lay[j * n..(j + 1) * n];
            let lay1 = &tip.lay[(j + 1) * n..(j + 2) * n];
            let top = &paint[j * n..(j + 1) * n];
            let bottom = &paint[(j + 1) * n..(j + 2) * n];
            let pixels = &mut pixels[bx + 1..bx + n];
            for i in a..b {
                let mut share =
                    k[0] * lay0[i] + k[1] * lay0[i + 1] + k[2] * lay1[i] + k[3] * lay1[i + 1];
                if share <= 0.0 {
                    continue;
                }
                if let Some(sel) = selection {
                    let cx = (lx + 1 + i as i32 + ox) as u32;
                    let cy = (ly + 1 + j as i32 + oy) as u32;
                    share *= sel.sample(cx, cy);
                    if share <= 0.0 {
                        continue;
                    }
                }
                let old = pixels[i];
                let mut new = [0.0f32; 4];
                for c in 0..4 {
                    let carried = k[0] * top[i][c]
                        + k[1] * top[i + 1][c]
                        + k[2] * bottom[i][c]
                        + k[3] * bottom[i + 1][c];
                    new[c] = old[c] + share * (carried - old[c]);
                }
                if lock_alpha {
                    // Colour moves, coverage stays: clear paint changes nothing.
                    if old[3] <= 0.0 || new[3] <= 1e-6 {
                        continue;
                    }
                    let keep = old[3] / new[3];
                    new = [new[0] * keep, new[1] * keep, new[2] * keep, old[3]];
                }
                pixels[i] = new;
            }
        };
        let rows = &mut win.px[(by + 1) * width..(by + n) * width];
        if n >= PARALLEL_FROM {
            rows.par_chunks_mut(width).enumerate().for_each(row);
        } else {
            rows.chunks_mut(width).enumerate().for_each(row);
        }

        let r = [lx + 1, ly + 1, lx + n as i32, ly + n as i32];
        win.dirty = Some(match win.dirty {
            Some(d) => [
                d[0].min(r[0]),
                d[1].min(r[1]),
                d[2].max(r[2]),
                d[3].max(r[3]),
            ],
            None => r,
        });
    }
}

impl SmudgeTool {
    pub fn new() -> Self {
        Self {
            size: 40.0,
            hardness: 0.0,
            strength: 0.5,
            finger_painting: false,
            stroke: None,
            last: (0.0, 0.0),
            carry: 0.0,
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
        self.size.min(MAX_SIZE) * 0.5
    }
    fn tip_hardness(&self) -> Option<f32> {
        Some(self.hardness)
    }

    fn on_press(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        ctx.canvas_mut().begin_stroke("Smudge");
        self.last = (event.canvas_x, event.canvas_y);
        let fg = ctx.fg_color;
        self.stroke = Stroke::begin(self, ctx.canvas_mut(), event.canvas_x, event.canvas_y, fg);
        // The press only picks up: the first trade is one step along.
        self.carry = self.stroke.as_ref().map_or(0.0, |s| s.tip.step);
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
        self.last = (x1, y1);
        let canvas = ctx.canvas_mut();
        let Some(stroke) = self.stroke.as_mut().filter(|s| s.still_on(canvas)) else {
            self.stroke = None;
            return ToolResponse::none();
        };
        let step = stroke.tip.step;
        scrub::dab_path(x0, y0, x1, y1, step, 1.0, &mut self.carry, |x, y| {
            stroke.step_to(canvas, x, y)
        });
        stroke.flush(canvas);
        ToolResponse::repaint()
    }

    fn on_release(&mut self, _event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        if let Some(mut stroke) = self.stroke.take() {
            stroke.flush(ctx.canvas_mut());
        }
        ToolResponse::none()
    }

    fn on_cancel(&mut self) {
        self.stroke = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::Canvas;
    use crate::core::document::{Document, DocumentId};

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

    /// A 64×64 document drawn by `paint(x, y) -> rgba`.
    fn doc_of(paint: impl Fn(u32, u32) -> [u8; 4]) -> Document {
        let mut px = Vec::with_capacity(64 * 64 * 4);
        for y in 0..64 {
            for x in 0..64 {
                px.extend_from_slice(&paint(x, y));
            }
        }
        Document::from_canvas(DocumentId(1), Canvas::from_rgba(px, 64, 64), None)
    }

    /// Drag along y from `from` to `to` in `events` equal steps.
    fn drag_in(tool: &mut dyn Tool, doc: &mut Document, y: f32, from: f32, to: f32, events: u32) {
        let mut ctx = ToolCtx::new(doc, [0, 0, 0, 255], [255; 4], 1.0, 0.0, 0.0);
        let mut prev = PointerEvent::new(from, y);
        tool.on_press(prev, &mut ctx);
        for i in 1..=events {
            let ev = PointerEvent::new(from + (to - from) * i as f32 / events as f32, y);
            tool.on_drag(ev, &prev, &mut ctx);
            prev = ev;
        }
        tool.on_release(prev, &mut ctx);
    }

    /// A `w`×`h` document drawn by `paint(x, y) -> rgba`.
    fn doc_sized(w: u32, h: u32, paint: impl Fn(u32, u32) -> [u8; 4]) -> Document {
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                px.extend_from_slice(&paint(x, y));
            }
        }
        Document::from_canvas(DocumentId(1), Canvas::from_rgba(px, w, h), None)
    }

    /// Drag in a straight line from `from` to `to` in `events` equal steps.
    fn drag_line(
        tool: &mut dyn Tool,
        doc: &mut Document,
        from: (f32, f32),
        to: (f32, f32),
        events: u32,
    ) {
        let mut ctx = ToolCtx::new(doc, [0, 0, 0, 255], [255; 4], 1.0, 0.0, 0.0);
        let mut prev = PointerEvent::new(from.0, from.1);
        tool.on_press(prev, &mut ctx);
        for i in 1..=events {
            let t = i as f32 / events as f32;
            let ev = PointerEvent::new(from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
            tool.on_drag(ev, &prev, &mut ctx);
            prev = ev;
        }
        tool.on_release(prev, &mut ctx);
    }

    /// The largest jump and the largest kink (second difference) of the red
    /// channel along `points`: a staircase has kinks as tall as its steps.
    fn roughness(doc: &Document, points: impl Iterator<Item = (u32, u32)>) -> (i32, i32) {
        let layer = doc.canvas.layer_stack.active_layer();
        let v: Vec<i32> = points
            .map(|(x, y)| layer.tiles.get_pixel(x, y).0 as i32)
            .collect();
        let jump = v.windows(2).map(|w| (w[1] - w[0]).abs()).max().unwrap_or(0);
        let kink = v
            .windows(3)
            .map(|w| (w[2] - 2 * w[1] + w[0]).abs())
            .max()
            .unwrap_or(0);
        (jump, kink)
    }

    /// Dragged along a level edge, Smudge leaves the edge where it is
    /// (14/11: each dab moved the picture half a pixel up-left).
    #[test]
    fn smudge_along_an_edge_does_not_move_it_across() {
        let mut doc = doc_of(|_, y| if y < 32 { [0, 0, 0, 255] } else { [255; 4] });
        drag_in(&mut SmudgeTool::new(), &mut doc, 32.0, 12.0, 52.0, 20);
        let layer = doc.canvas.layer_stack.active_layer();
        for x in 24..40 {
            assert!(layer.tiles.get_pixel(x, 31).0 < 8, "row 31 at {x}");
            assert!(layer.tiles.get_pixel(x, 32).0 > 247, "row 32 at {x}");
        }
    }

    /// Dragged in from the page's edge, Smudge does not hole the Background.
    #[test]
    fn smudge_from_the_edge_keeps_the_background_opaque() {
        let mut doc = doc_of(|x, _| [200, (x * 4) as u8, 0, 255]);
        assert!(doc.canvas.layer_stack.active_layer().is_background);
        drag_in(&mut SmudgeTool::new(), &mut doc, 32.0, 0.0, 30.0, 10);
        let layer = doc.canvas.layer_stack.active_layer();
        for x in 0..64 {
            for y in 0..64 {
                assert_eq!(layer.tiles.get_pixel(x, y).3, 255, "({x}, {y})");
            }
        }
    }

    /// The same drag in 5 or in 50 events comes out the same, for Smudge
    /// and Dodge (14/11: a slow drag dabbed far more often).
    #[test]
    fn a_drag_is_the_same_in_few_or_many_events() {
        let paint = |x: u32, y: u32| [(x * 4) as u8, (y * 4) as u8, 128, 255];
        let tools: [fn() -> Box<dyn Tool>; 2] = [
            || Box::new(SmudgeTool::new()),
            || Box::new(crate::tools::dodge_burn::DodgeBurnTool::dodge()),
        ];
        for make in tools {
            let mut few = doc_of(paint);
            let mut many = doc_of(paint);
            drag_in(make().as_mut(), &mut few, 32.0, 10.0, 50.0, 5);
            drag_in(make().as_mut(), &mut many, 32.0, 10.0, 50.0, 50);
            let a = few.canvas.layer_stack.active_layer();
            let b = many.canvas.layer_stack.active_layer();
            let mut worst = 0i32;
            for x in 0..64 {
                for y in 0..64 {
                    let (p, q) = (a.tiles.get_pixel(x, y), b.tiles.get_pixel(x, y));
                    for (u, v) in [(p.0, q.0), (p.1, q.1), (p.2, q.2)] {
                        worst = worst.max((u as i32 - v as i32).abs());
                    }
                }
            }
            assert!(worst <= 2, "{}: differs by {worst}", make().name());
        }
    }

    /// With "Lock transparent pixels" Smudge keeps every pixel's coverage.
    #[test]
    fn smudge_keeps_locked_transparency() {
        let mut doc = doc_of(|x, _| if x < 32 { [255, 0, 0, 255] } else { [0; 4] });
        let idx = doc.canvas.layer_stack.active_idx;
        let layer = &mut doc.canvas.layer_stack.layers[idx];
        layer.is_background = false;
        layer.lock_alpha = true;
        drag_in(&mut SmudgeTool::new(), &mut doc, 32.0, 50.0, 10.0, 20);
        let layer = doc.canvas.layer_stack.active_layer();
        for x in 0..64 {
            let want = if x < 32 { 255 } else { 0 };
            assert_eq!(layer.tiles.get_pixel(x, 32).3, want, "alpha at {x}");
        }
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

    /// A dragged edge fades out smoothly. Each dab used to copy the layer from
    /// a tenth of the tip back, and the fade was a staircase with steps that
    /// wide, 41 to 55 levels tall.
    #[test]
    fn a_dragged_edge_fades_without_steps() {
        // The default tip, a wide one, one that steps more than a pixel at a
        // time, and a hard one. The last number is the length of the drag.
        let tips = [
            (40.0, 0.0, 600.0),
            (200.0, 0.0, 600.0),
            (300.0, 0.0, 300.0),
            (80.0, 1.0, 600.0),
        ];
        for (size, hardness, travel) in tips {
            let mut doc = doc_sized(
                900,
                340,
                |x, _| {
                    if x < 250 { [0, 0, 0, 255] } else { [255; 4] }
                },
            );
            let mut tool = SmudgeTool::new();
            tool.size = size;
            tool.hardness = hardness;
            let events = (travel / 5.0) as u32;
            drag_line(
                &mut tool,
                &mut doc,
                (200.0, 170.0),
                (200.0 + travel, 170.0),
                events,
            );
            let end = 200 + travel as u32;
            let (jump, kink) = roughness(&doc, (252..end).map(|x| (x, 170)));
            assert!(
                jump <= 12 && kink <= 4,
                "size {size}, hardness {hardness}: jump {jump}, kink {kink}"
            );
            let layer = doc.canvas.layer_stack.active_layer();
            let near = layer.tiles.get_pixel(254, 170).0;
            assert!(
                near < 128,
                "size {size}: black is pulled over the edge, got {near}"
            );
        }
    }

    /// The same along a slanted stroke, where the paint travels between pixels.
    #[test]
    fn a_slanted_drag_fades_without_steps() {
        let mut doc = doc_sized(
            700,
            500,
            |x, _| {
                if x < 150 { [0, 0, 0, 255] } else { [255; 4] }
            },
        );
        let mut tool = SmudgeTool::new();
        tool.size = 80.0;
        let (from, to) = ((100.0f32, 100.0f32), (600.0f32, 390.0f32));
        drag_line(&mut tool, &mut doc, from, to, 100);
        let along = (153..600u32).map(|x| {
            let t = (x as f32 - from.0) / (to.0 - from.0);
            (x, (from.1 + (to.1 - from.1) * t).round() as u32)
        });
        let (jump, kink) = roughness(&doc, along);
        assert!(jump <= 12 && kink <= 5, "jump {jump}, kink {kink}");
    }

    /// At 100 % the paint picked up on the press reaches the end of the
    /// stroke; at 50 % it has long faded out.
    #[test]
    fn full_strength_carries_the_press_to_the_end() {
        let end_of_a_drag_at = |strength: f32| {
            let mut doc = doc_sized(
                400,
                100,
                |x, _| {
                    if x < 60 { [200, 30, 30, 255] } else { [255; 4] }
                },
            );
            let mut tool = SmudgeTool::new();
            tool.size = 30.0;
            tool.hardness = 1.0;
            tool.strength = strength;
            drag_line(&mut tool, &mut doc, (40.0, 50.0), (340.0, 50.0), 60);
            doc.canvas
                .layer_stack
                .active_layer()
                .tiles
                .get_pixel(340, 50)
        };
        let (r, g, b, a) = end_of_a_drag_at(1.0);
        assert!(
            (197..=203).contains(&r) && (27..=33).contains(&g) && (27..=33).contains(&b),
            "the red of the press arrives whole, got {r} {g} {b}"
        );
        assert_eq!(a, 255);
        let (_, g, _, _) = end_of_a_drag_at(0.5);
        assert!(g > 250, "at half strength the far end stays white, got {g}");
    }

    /// "Finger Painting" starts the smear with the foreground colour.
    #[test]
    fn finger_painting_starts_with_the_foreground_colour() {
        let mut doc = doc_sized(300, 100, |_, _| [255; 4]);
        let mut tool = SmudgeTool::new();
        tool.finger_painting = true;
        // `drag_line` paints with a black foreground.
        drag_line(&mut tool, &mut doc, (50.0, 50.0), (250.0, 50.0), 40);
        let layer = doc.canvas.layer_stack.active_layer();
        let (start, end) = (
            layer.tiles.get_pixel(60, 50).0,
            layer.tiles.get_pixel(245, 50).0,
        );
        assert!(start < 200, "black is laid down at the start, got {start}");
        assert!(end > 245, "and has run out by the end, got {end}");
    }

    /// Pulled over clear pixels, a colour thins out but keeps its hue.
    #[test]
    fn smudge_into_clear_pixels_does_not_darken_the_colour() {
        let mut doc = doc_of(|x, _| if x < 32 { [255, 0, 0, 255] } else { [0; 4] });
        let idx = doc.canvas.layer_stack.active_idx;
        doc.canvas.layer_stack.layers[idx].is_background = false;
        drag_in(&mut SmudgeTool::new(), &mut doc, 32.0, 14.0, 54.0, 20);
        let layer = doc.canvas.layer_stack.active_layer();
        for x in 0..64 {
            let (r, g, b, a) = layer.tiles.get_pixel(x, 32);
            if a > 0 {
                assert!(r >= 250 && g <= 2 && b <= 2, "at {x}: {r} {g} {b} {a}");
            }
        }
        let pulled = layer.tiles.get_pixel(36, 32).3;
        assert!(
            pulled > 40,
            "red is pulled past the edge, got alpha {pulled}"
        );
    }

    /// Inside a selection Smudge still picks paint up from outside it, and
    /// lays none down there.
    #[test]
    fn smudge_lays_paint_down_inside_the_selection_only() {
        let mut doc = doc_of(|x, _| if x < 32 { [0, 0, 0, 255] } else { [255; 4] });
        doc.canvas.selection.select_rect(32, 0, 64, 64);
        drag_in(&mut SmudgeTool::new(), &mut doc, 32.0, 16.0, 50.0, 20);
        let layer = doc.canvas.layer_stack.active_layer();
        for x in 0..32 {
            assert_eq!(layer.tiles.get_pixel(x, 32), (0, 0, 0, 255), "at {x}");
        }
        let pulled = layer.tiles.get_pixel(35, 32).0;
        assert!(
            pulled < 200,
            "black from outside is pulled in, got {pulled}"
        );
    }

    /// The smallest tip, a press off the page and a layer smaller than the
    /// page all smudge without touching what lies outside the layer.
    #[test]
    fn a_tiny_tip_and_a_press_off_the_layer_are_safe() {
        let mut doc = doc_of(|x, _| [(x * 4) as u8, 0, 0, 255]);
        let mut tool = SmudgeTool::new();
        tool.size = 1.0;
        drag_in(&mut tool, &mut doc, 32.0, 10.0, 50.0, 20);

        tool.size = 40.0;
        drag_line(&mut tool, &mut doc, (-60.0, -45.0), (30.0, 30.0), 30);
        drag_line(&mut tool, &mut doc, (40.0, 40.0), (150.0, 90.0), 30);
        let idx = doc.canvas.layer_stack.active_idx;
        let layer = &mut doc.canvas.layer_stack.layers[idx];
        for x in 0..64 {
            assert_eq!(layer.tiles.get_pixel(x, 20).3, 255, "at {x}");
        }

        layer.is_background = false;
        layer.offset = (20, 10);
        drag_line(&mut tool, &mut doc, (0.0, 0.0), (90.0, 80.0), 30);
        let layer = doc.canvas.layer_stack.active_layer();
        assert_eq!((layer.width, layer.height), (64, 64));
    }
}

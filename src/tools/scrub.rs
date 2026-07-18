//! Shared brush-dab rasteriser for the "scrub" tool family — Smudge / Dodge /
//! Burn — i.e. tools that read-modify-write existing destination pixels along a
//! stroke (unlike Brush, which writes a fixed colour, and Clone, which copies a
//! source). Brush and Clone keep their own perf-tuned paths (LUT + rayon); this
//! engine stays deliberately simple (single-threaded, read-modify-write) so the
//! three new tools share one copy of the geometry / coverage / selection / bounds /
//! dirty-marking boilerplate instead of triplicating it.

use crate::core::canvas::Canvas;
use crate::core::tile::{TilePos, TILE_SIZE};

/// Soft-round brush coverage at squared distance `dist2` for a tip of `radius`
/// and `hardness` (0 = soft, 1 = hard). Matches the Brush tip falloff so the
/// scrub tools feel identical to the paint brush.
#[inline]
pub fn dab_coverage(dist2: f32, radius: f32, hardness: f32) -> f32 {
    super::brush::soft_round_alpha(dist2, radius, hardness)
}

/// Read the active layer's pixel at canvas-space `(cx, cy)` as linearised 0..1
/// RGBA, or `None` if outside the layer / unpaintable. Used by Smudge to pick up
/// colour without going through the mutable dab path.
pub fn sample_active_pixel(canvas: &Canvas, cx: f32, cy: f32) -> Option<[f32; 4]> {
    if canvas.layer_stack.layers.is_empty() {
        return None;
    }
    let l = canvas.active_layer();
    if !l.is_raster() {
        return None;
    }
    let lx = cx.floor() as i32 - l.offset.0;
    let ly = cy.floor() as i32 - l.offset.1;
    if lx < 0 || ly < 0 || lx >= l.width as i32 || ly >= l.height as i32 {
        return None;
    }
    let (r, g, b, a) = l.tiles.get_pixel(lx as u32, ly as u32);
    Some([
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    ])
}

/// Visit every active-layer pixel a brush dab centred at canvas-space `(cx, cy)`
/// covers, calling `f(px, py, coverage, dst)` where `px/py` are LAYER-LOCAL pixel
/// coordinates, `coverage` is the soft-round alpha already multiplied by `opacity`
/// and the selection mask, and `dst` is the pixel's mutable RGBA (read-modify-write
/// in place). Returns `false` (no-op) when the active layer can't be painted
/// (locked non-background, non-raster, empty, or the dab misses the layer).
/// Marks the touched canvas region dirty.
pub fn for_each_dab_pixel<F>(
    canvas: &mut Canvas,
    cx: f32,
    cy: f32,
    radius: f32,
    hardness: f32,
    opacity: f32,
    mut f: F,
) -> bool
where
    F: FnMut(u32, u32, f32, &mut [u8; 4]),
{
    if canvas.layer_stack.layers.is_empty() {
        return false;
    }
    canvas.layer_stack.normalize_active_idx();
    let idx = canvas.layer_stack.active_idx;
    {
        let l = &canvas.layer_stack.layers[idx];
        if (!l.is_background && l.locked) || !l.is_raster() {
            return false;
        }
    }

    let r = radius.max(0.5).min(5000.0);
    let r2 = r * r;
    let opacity = opacity.clamp(0.0, 1.0);

    let ox = canvas.layer_stack.layers[idx].offset.0;
    let oy = canvas.layer_stack.layers[idx].offset.1;
    let lw = canvas.layer_stack.layers[idx].width;
    let lh = canvas.layer_stack.layers[idx].height;

    let lcx = cx - ox as f32;
    let lcy = cy - oy as f32;

    let lx0 = ((lcx - r).floor().max(0.0) as u32).min(lw);
    let ly0 = ((lcy - r).floor().max(0.0) as u32).min(lh);
    let lx1 = ((lcx + r).ceil() as u32).min(lw);
    let ly1 = ((lcy + r).ceil() as u32).min(lh);
    if lx1 <= lx0 || ly1 <= ly0 {
        return false;
    }

    // Selection is read through a raw pointer so the mutable `get_paint_tiles_mut`
    // borrow below doesn't conflict — they touch disjoint Canvas fields. Same
    // pattern the Brush hot path uses.
    let has_sel = canvas.selection.active;
    let sel_ptr: *const crate::core::selection::Selection = &canvas.selection;

    let tile_size = TILE_SIZE;
    // Channels-panel write gate: keep only the enabled channels of the
    // computed result (mask painting is grayscale and ignores the gate).
    let channel_wm =
        if canvas.layer_stack.layers[idx].paint_target == crate::core::layer::PaintTarget::Mask {
            None
        } else {
            canvas.channels.write_gate()
        };
    let Some(tiles) = canvas.layer_stack.layers[idx].get_paint_tiles_mut() else {
        return false;
    };

    let mut touched = false;
    for ty in (ly0 / tile_size)..=(ly1 / tile_size) {
        for tx in (lx0 / tile_size)..=(lx1 / tile_size) {
            let tile_x0 = tx * tile_size;
            let tile_y0 = ty * tile_size;
            let px_min = tile_x0.max(lx0);
            let py_min = tile_y0.max(ly0);
            let px_max = (tile_x0 + tile_size).min(lx1);
            let py_max = (tile_y0 + tile_size).min(ly1);
            if px_max <= px_min || py_max <= py_min {
                continue;
            }
            let tile = tiles.get_tile_mut(TilePos {
                x: tx as i32,
                y: ty as i32,
            });
            for py in py_min..py_max {
                let dy = py as f32 + 0.5 - lcy;
                let dy2 = dy * dy;
                let row_off = (py - tile_y0) * tile_size;
                for px in px_min..px_max {
                    let dx = px as f32 + 0.5 - lcx;
                    let d2 = dx * dx + dy2;
                    if d2 > r2 {
                        continue;
                    }
                    let mut cov = dab_coverage(d2, r, hardness) * opacity;
                    if cov < 0.001 {
                        continue;
                    }
                    if has_sel {
                        let canvas_px = (px as i32 + ox) as u32;
                        let canvas_py = (py as i32 + oy) as u32;
                        cov *= unsafe { (*sel_ptr).sample(canvas_px, canvas_py) };
                        if cov < 0.001 {
                            continue;
                        }
                    }
                    let i = ((row_off + (px - tile_x0)) * 4) as usize;
                    let before = [
                        tile.pixels[i],
                        tile.pixels[i + 1],
                        tile.pixels[i + 2],
                        tile.pixels[i + 3],
                    ];
                    let mut dst = before;
                    f(px, py, cov, &mut dst);
                    if let Some(wm) = channel_wm {
                        crate::core::tile::apply_write_mask(before, &mut dst, wm);
                    }
                    tile.pixels[i] = dst[0];
                    tile.pixels[i + 1] = dst[1];
                    tile.pixels[i + 2] = dst[2];
                    tile.pixels[i + 3] = dst[3];
                    touched = true;
                }
            }
        }
    }

    let cx0 = (lx0 as i32 + ox).max(0) as u32;
    let cy0 = (ly0 as i32 + oy).max(0) as u32;
    let cx1 = ((lx1 as i32 + ox) as u32).min(canvas.width);
    let cy1 = ((ly1 as i32 + oy) as u32).min(canvas.height);
    canvas.mark_dirty(cx0, cy0, cx1, cy1);
    touched
}

/// Walk a stroke segment from `(x0,y0)` to `(x1,y1)`, calling `dab(x, y)` at every
/// spaced step. `spacing` is a fraction of the brush `diameter` (Brush convention).
pub fn dab_segment<F>(x0: f32, y0: f32, x1: f32, y1: f32, diameter: f32, spacing: f32, mut dab: F)
where
    F: FnMut(f32, f32),
{
    if !x0.is_finite() || !y0.is_finite() || !x1.is_finite() || !y1.is_finite() {
        return;
    }
    let dist = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
    let step = (diameter * spacing).max(0.5);
    let steps = ((dist / step).ceil() as u32).clamp(1, 4000);
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        dab(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t);
    }
}

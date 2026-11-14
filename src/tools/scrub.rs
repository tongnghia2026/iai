//! Shared brush-dab rasteriser for the "scrub" tool family — Smudge / Dodge /
//! Burn — i.e. tools that read-modify-write existing destination pixels along a
//! stroke (unlike Brush, which writes a fixed colour, and Clone, which copies a
//! source). Brush and Clone keep their own perf-tuned paths (LUT + rayon); this
//! engine stays deliberately simple (single-threaded, read-modify-write) so the
//! three new tools share one copy of the geometry / coverage / selection / bounds /
//! dirty-marking boilerplate instead of triplicating it.

use crate::core::canvas::Canvas;
use crate::core::layer::{Layer, PaintTarget};
use crate::core::selection::Selection;
use crate::core::tile::{TILE_SIZE, TilePos};

/// Soft-round brush coverage at squared distance `dist2` for a tip of `radius`
/// and `hardness` (0 = soft, 1 = hard). Matches the Brush tip falloff so the
/// scrub tools feel identical to the paint brush.
#[inline]
pub fn dab_coverage(dist2: f32, radius: f32, hardness: f32) -> f32 {
    super::brush::soft_round_alpha(dist2, radius, hardness)
}

/// Whether the scrub tools can edit `layer`'s paint target: raster pixels, or
/// the grayscale mask of any layer kind (folder, text, shape, adjustment...).
pub fn can_scrub(layer: &Layer) -> bool {
    match layer.paint_target {
        PaintTarget::Pixels => layer.is_raster(),
        PaintTarget::Mask => layer.mask.is_some(),
    }
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

/// The active layer's offset and size, when the scrub tools may paint it:
/// `None` for an empty stack, a locked layer, or a paint target they can't edit.
pub fn paint_frame(canvas: &mut Canvas) -> Option<((i32, i32), (u32, u32))> {
    if canvas.layer_stack.layers.is_empty() {
        return None;
    }
    canvas.layer_stack.normalize_active_idx();
    let l = &canvas.layer_stack.layers[canvas.layer_stack.active_idx];
    if (!l.is_background && l.locked) || !can_scrub(l) {
        return None;
    }
    Some((l.offset, (l.width, l.height)))
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
    let Some(((ox, oy), (lw, lh))) = paint_frame(canvas) else {
        return false;
    };

    let r = radius.max(0.5).min(5000.0);
    let r2 = r * r;
    let opacity = opacity.clamp(0.0, 1.0);

    let lcx = cx - ox as f32;
    let lcy = cy - oy as f32;

    let lx0 = ((lcx - r).floor().max(0.0) as u32).min(lw);
    let ly0 = ((lcy - r).floor().max(0.0) as u32).min(lh);
    let lx1 = ((lcx + r).ceil() as u32).min(lw);
    let ly1 = ((lcy + r).ceil() as u32).min(lh);

    for_each_rect_pixel(canvas, lx0, ly0, lx1, ly1, |sel, px, py, dst| {
        let dx = px as f32 + 0.5 - lcx;
        let dy = py as f32 + 0.5 - lcy;
        let d2 = dx * dx + dy * dy;
        if d2 > r2 {
            return false;
        }
        let mut cov = dab_coverage(d2, r, hardness) * opacity;
        if cov < 0.001 {
            return false;
        }
        if sel.active {
            cov *= sel.sample((px as i32 + ox) as u32, (py as i32 + oy) as u32);
            if cov < 0.001 {
                return false;
            }
        }
        f(px, py, cov, dst);
        true
    })
}

/// Visit every active-layer pixel of the layer-local rect `lx0..lx1` ×
/// `ly0..ly1` (clipped to the layer), calling `f(selection, px, py, dst)` with
/// the pixel's RGBA to change in place; `f` returns whether to keep the change.
/// Every kept change follows the rules the scrub tools share: mask texels stay
/// opaque gray, "Lock transparent pixels" keeps coverage, and the Channels
/// panel gates the colour channels. Returns `false` when the layer can't be
/// painted or nothing was written. Marks the rect dirty.
pub fn for_each_rect_pixel<F>(
    canvas: &mut Canvas,
    lx0: u32,
    ly0: u32,
    lx1: u32,
    ly1: u32,
    mut f: F,
) -> bool
where
    F: FnMut(&Selection, u32, u32, &mut [u8; 4]) -> bool,
{
    let Some(((ox, oy), (lw, lh))) = paint_frame(canvas) else {
        return false;
    };
    let (lx1, ly1) = (lx1.min(lw), ly1.min(lh));
    if lx1 <= lx0 || ly1 <= ly0 {
        return false;
    }
    let idx = canvas.layer_stack.active_idx;

    // The selection and the layer's tiles are disjoint Canvas fields: both can
    // be borrowed at once.
    let sel = &canvas.selection;

    let tile_size = TILE_SIZE;
    // Channels-panel write gate: keep only the enabled channels of the
    // computed result (mask painting is grayscale and ignores the gate).
    let paint_mask = canvas.layer_stack.layers[idx].paint_target == PaintTarget::Mask;
    // "Lock transparent pixels": the tools change colour, never coverage.
    let lock_alpha = !paint_mask && canvas.layer_stack.layers[idx].lock_alpha;
    let channel_wm = if paint_mask {
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
                let row_off = (py - tile_y0) * tile_size;
                for px in px_min..px_max {
                    let i = ((row_off + (px - tile_x0)) * 4) as usize;
                    let before = [
                        tile.pixels[i],
                        tile.pixels[i + 1],
                        tile.pixels[i + 2],
                        tile.pixels[i + 3],
                    ];
                    let mut dst = before;
                    if !f(sel, px, py, &mut dst) {
                        continue;
                    }
                    if paint_mask {
                        // Mask texels are opaque gray, as Brush writes them.
                        dst = [dst[0], dst[0], dst[0], 255];
                    } else if lock_alpha {
                        if before[3] == 0 {
                            continue;
                        }
                        dst[3] = before[3];
                    }
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
    let cx1 = ((lx1 as i32 + ox).max(0) as u32).min(canvas.width);
    let cy1 = ((ly1 as i32 + oy).max(0) as u32).min(canvas.height);
    canvas.mark_dirty(cx0, cy0, cx1, cy1);
    touched
}

/// Walk a stroke segment from `(x0,y0)` to `(x1,y1)`, calling `dab(x, y)` every
/// `spacing` × `diameter` along the WHOLE stroke (Brush convention): `carry` is
/// the distance from the segment start to the next dab and is kept between
/// pointer events. Each event used to dab both its ends, so a slow drag (many
/// short events) dabbed far more often and came out much stronger (14/11).
/// Start a stroke with `carry = 0.0` to dab its first point.
pub fn dab_path<F>(
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    diameter: f32,
    spacing: f32,
    carry: &mut f32,
    mut dab: F,
) where
    F: FnMut(f32, f32),
{
    if !x0.is_finite() || !y0.is_finite() || !x1.is_finite() || !y1.is_finite() {
        return;
    }
    let dist = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
    let step = dab_step(diameter, spacing);
    let mut t = carry.max(0.0);
    let mut n = 0u32;
    while t <= dist && n < 4000 {
        let f = if dist > 0.0 { t / dist } else { 0.0 };
        dab(x0 + (x1 - x0) * f, y0 + (y1 - y0) * f);
        t += step;
        n += 1;
    }
    *carry = t - dist;
}

/// The distance between two dabs of a tip `diameter` wide at `spacing`.
pub fn dab_step(diameter: f32, spacing: f32) -> f32 {
    (diameter * spacing).max(0.5)
}

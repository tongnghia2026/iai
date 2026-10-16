//! Writing a selection into the layers: as a layer mask, or cut into a
//! copy of the layer (Refine Selection's outputs, the clipboard's copy of a
//! selected part, the engine's background removal).

use super::Canvas;

/// Where [`output_selection`] writes the selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionOutput {
    /// The active layer's mask.
    LayerMask,
    /// A copy of the active layer with what is not selected cleared.
    NewLayer,
    /// A copy of the active layer with the selection as its mask.
    NewLayerWithMask,
}

/// Mask one copied layer's tiles by the active selection: pixels outside the
/// selection (or outside the canvas) lose their alpha, in both the 8-bit
/// mirror and the 16-bit master when present. Tiles left fully transparent
/// are dropped, so the copied block's tile bounding box tracks the selected
/// content instead of the source layer's full extent.
pub fn mask_copied_layer_by_selection(
    layer: &mut iai_doc::layer::Layer,
    selection: &iai_doc::selection::Selection,
    canvas_w: u32,
    canvas_h: u32,
) {
    use iai_raster::tile::TILE_SIZE;
    let (ox, oy) = layer.offset;
    let positions: Vec<iai_raster::tile::TilePos> = layer.tiles.tiles.keys().copied().collect();
    for pos in positions {
        let tile_x0 = pos.x * TILE_SIZE as i32;
        let tile_y0 = pos.y * TILE_SIZE as i32;
        if let Some(tile) = layer.tiles.tiles.get_mut(&pos) {
            let tile = std::sync::Arc::make_mut(tile);
            for ty in 0..TILE_SIZE {
                for tx in 0..TILE_SIZE {
                    // Canvas coords account for the layer offset.
                    let cx = tile_x0 + tx as i32 + ox;
                    let cy = tile_y0 + ty as i32 + oy;
                    let i = ((ty * TILE_SIZE + tx) * 4) as usize;
                    let sel =
                        if cx >= 0 && cy >= 0 && (cx as u32) < canvas_w && (cy as u32) < canvas_h {
                            selection.sample(cx as u32, cy as u32)
                        } else {
                            0.0
                        };
                    if sel < 1.0 {
                        let a = tile.pixels[i + 3];
                        tile.pixels[i + 3] = (a as f32 * sel) as u8;
                        if let Some(p16) = tile.pixels16.as_mut() {
                            let a16 = p16[i + 3];
                            p16[i + 3] = (a16 as f32 * sel) as u16;
                        }
                    }
                }
            }
        }
    }
    layer
        .tiles
        .tiles
        .retain(|_, tile| tile_has_visible_alpha(tile));
}

/// True when any pixel in the tile has non-zero alpha. Checks the 16-bit
/// master when present: a faint 16-bit alpha can round to zero in the 8-bit
/// mirror and must still survive the copy.
pub fn tile_has_visible_alpha(tile: &iai_raster::tile::Tile) -> bool {
    if let Some(p16) = &tile.pixels16 {
        p16.chunks_exact(4).any(|px| px[3] != 0)
    } else {
        tile.pixels.chunks_exact(4).any(|px| px[3] != 0)
    }
}

/// Layer mask (in the layer's own pixel space) revealing `sel`.
pub fn layer_mask_from_selection(
    sel: &iai_doc::selection::Selection,
    layer: &iai_doc::layer::Layer,
    cw: u32,
    ch: u32,
) -> Option<iai_doc::layer::LayerMask> {
    use rayon::prelude::*;
    let (lw, lh) = (layer.width, layer.height);
    let len = Canvas::guarded_flat_rgba_len(lw, lh)?;
    let (ox, oy) = layer.offset;
    let mut rgba = vec![0u8; len];
    rgba.par_chunks_mut(lw as usize * 4)
        .enumerate()
        .for_each(|(ly, row)| {
            let cy = ly as i64 + oy as i64;
            for (lx, px) in row.chunks_exact_mut(4).enumerate() {
                let cx = lx as i64 + ox as i64;
                let m = if cx >= 0 && cy >= 0 && cx < cw as i64 && cy < ch as i64 {
                    sel.mask[cy as usize * cw as usize + cx as usize]
                } else {
                    0
                };
                px.copy_from_slice(&[m, m, m, 255]);
            }
        });
    Some(iai_doc::layer::LayerMask {
        tiles: iai_raster::tile::TileMap::from_rgba(&rgba, lw, lh),
        width: lw,
        height: lh,
        enabled: true,
        inverted: false,
        bake_offset: (0, 0),
        bake_frame_offset: (0, 0),
    })
}

pub fn install_mask(layer: &mut iai_doc::layer::Layer, mask: iai_doc::layer::LayerMask) {
    layer.mask = Some(mask);
    layer.mask_active = true;
    layer.mask_linked = true;
    layer.paint_target = iai_doc::layer::PaintTarget::Mask;
}

/// Write `sel` to the active layer of `canvas`: as its layer mask, or as a
/// copy of it above, cut out or masked (the original is hidden, as Photoshop
/// does). Records no history: the caller makes it one undo step. Returns
/// false when nothing could be written.
pub fn output_selection(
    canvas: &mut Canvas,
    sel: &iai_doc::selection::Selection,
    mode: SelectionOutput,
    decontaminate: Option<f32>,
) -> bool {
    let (cw, ch) = (canvas.width, canvas.height);
    if sel.mask.len() < cw as usize * ch as usize {
        return false;
    }
    canvas.layer_stack.normalize_active_idx();
    let idx = canvas.layer_stack.active_idx;
    let Some(src) = canvas.layer_stack.layers.get(idx) else {
        return false;
    };
    match mode {
        SelectionOutput::LayerMask => {
            let Some(mask) = layer_mask_from_selection(sel, src, cw, ch) else {
                return false;
            };
            install_mask(&mut canvas.layer_stack.layers[idx], mask);
            true
        }
        SelectionOutput::NewLayer | SelectionOutput::NewLayerWithMask => {
            // Cutting the pixels needs a raster layer; others keep a mask.
            let with_mask = mode == SelectionOutput::NewLayerWithMask || !src.is_raster();
            let new_idx = canvas.layer_stack.duplicate_layer(idx);
            if new_idx == idx {
                return false;
            }
            let layer = &mut canvas.layer_stack.layers[new_idx];
            if with_mask {
                let Some(mask) = layer_mask_from_selection(sel, layer, cw, ch) else {
                    return false;
                };
                install_mask(layer, mask);
            } else {
                mask_copied_layer_by_selection(layer, sel, cw, ch);
            }
            if let Some(amount) = decontaminate {
                decontaminate_layer(layer, sel, cw, ch, amount);
            }
            canvas.layer_stack.layers[idx].visible = false;
            canvas.layer_stack.active_idx = new_idx;
            true
        }
    }
}

/// Decontaminate Colors: edge pixels take on the colour of nearby fully
/// selected pixels, the more the softer the selection is there.
fn decontaminate_layer(
    layer: &mut iai_doc::layer::Layer,
    sel: &iai_doc::selection::Selection,
    cw: u32,
    ch: u32,
    amount: f32,
) {
    use iai_raster::tile::TILE_SIZE;
    const SEARCH_R: i64 = 8;
    let amount = amount.clamp(0.0, 1.0);
    if amount <= 0.001 {
        return;
    }
    let (cw, ch) = (cw as i64, ch as i64);
    let mask_at = |x: i64, y: i64| -> u8 {
        if x >= 0 && y >= 0 && x < cw && y < ch {
            sel.mask[(y * cw + x) as usize]
        } else {
            0
        }
    };
    let (ox, oy) = (layer.offset.0 as i64, layer.offset.1 as i64);
    let source = layer.tiles.clone();
    let (lw, lh) = (layer.width as i64, layer.height as i64);
    let positions: Vec<_> = layer.tiles.tiles.keys().copied().collect();
    for pos in positions {
        let Some(tile) = layer.tiles.tiles.get_mut(&pos) else {
            continue;
        };
        let tile = std::sync::Arc::make_mut(tile);
        let mut touched = false;
        for ty in 0..TILE_SIZE as i64 {
            for tx in 0..TILE_SIZE as i64 {
                let (lx, ly) = (
                    pos.x as i64 * TILE_SIZE as i64 + tx,
                    pos.y as i64 * TILE_SIZE as i64 + ty,
                );
                let (cx, cy) = (lx + ox, ly + oy);
                let m = mask_at(cx, cy);
                let i = ((ty * TILE_SIZE as i64 + tx) * 4) as usize;
                if m == 0 || tile.pixels[i + 3] == 0 {
                    continue;
                }
                let near_edge = m < 255
                    || mask_at(cx - 1, cy) < 245
                    || mask_at(cx + 1, cy) < 245
                    || mask_at(cx, cy - 1) < 245
                    || mask_at(cx, cy + 1) < 245;
                if !near_edge {
                    continue;
                }
                let mut sum = [0u64; 3];
                let mut count = 0u64;
                for sy in -SEARCH_R..=SEARCH_R {
                    for sx in -SEARCH_R..=SEARCH_R {
                        if sx * sx + sy * sy > SEARCH_R * SEARCH_R
                            || mask_at(cx + sx, cy + sy) < 220
                        {
                            continue;
                        }
                        let (sx, sy) = (lx + sx, ly + sy);
                        if sx < 0 || sy < 0 || sx >= lw || sy >= lh {
                            continue;
                        }
                        let (r, g, b, a) = source.get_pixel16(sx as u32, sy as u32);
                        if a == 0 {
                            continue;
                        }
                        sum[0] += r as u64;
                        sum[1] += g as u64;
                        sum[2] += b as u64;
                        count += 1;
                    }
                }
                if count == 0 {
                    continue;
                }
                let mix = (amount * (1.0 - m as f32 / 255.0).sqrt()).clamp(0.0, 1.0);
                let (r, g, b, _) = source.get_pixel16(lx as u32, ly as u32);
                let cur = [r, g, b];
                for c in 0..3 {
                    let fg = sum[c] as f32 / count as f32;
                    let v16 = (cur[c] as f32 * (1.0 - mix) + fg * mix)
                        .round()
                        .clamp(0.0, 65535.0);
                    tile.pixels[i + c] = (v16 / 257.0).round() as u8;
                    if let Some(p16) = tile.pixels16.as_mut() {
                        p16[i + c] = v16 as u16;
                    }
                }
                touched = true;
            }
        }
        if touched {
            tile.revision = iai_raster::tile::next_tile_revision();
        }
    }
}

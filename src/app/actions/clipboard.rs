//! Internal layer clipboard: copy / cut / paste. Split out of app/actions.rs.
//! Copy also mirrors a flattened crop of the copied block onto the OS clipboard
//! (via `app::os_clipboard`), so in-app copies paste into other applications and
//! paste can tell a fresher external copy from iai's own write.

use crate::app::os_clipboard::{self, OsClipboardImage};
use crate::app::render::CanvasEvent;
use crate::app::state::App;
use crate::core::layer::Layer;

fn read_os_clipboard_image() -> Result<Option<OsClipboardImage>, String> {
    os_clipboard::read_image()
}

fn centered_layer_offset(canvas_w: u32, canvas_h: u32, layer_w: u32, layer_h: u32) -> (i32, i32) {
    (
        ((canvas_w as i64 - layer_w as i64) / 2) as i32,
        ((canvas_h as i64 - layer_h as i64) / 2) as i32,
    )
}

/// Mask one copied layer's tiles by the active selection: pixels outside the
/// selection (or outside the canvas) lose their alpha, in both the 8-bit
/// mirror and the 16-bit master when present. Tiles left fully transparent
/// are dropped, so the copied block's tile bounding box tracks the selected
/// content instead of the source layer's full extent.
fn mask_copied_layer_by_selection(
    layer: &mut Layer,
    selection: &crate::core::selection::Selection,
    canvas_w: u32,
    canvas_h: u32,
) {
    use crate::core::tile::TILE_SIZE;
    let (ox, oy) = layer.offset;
    let positions: Vec<crate::core::tile::TilePos> = layer.tiles.tiles.keys().copied().collect();
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
fn tile_has_visible_alpha(tile: &crate::core::tile::Tile) -> bool {
    if let Some(p16) = &tile.pixels16 {
        p16.chunks_exact(4).any(|px| px[3] != 0)
    } else {
        tile.pixels.chunks_exact(4).any(|px| px[3] != 0)
    }
}

/// Flatten a copied block into a tightly-cropped RGBA image for the OS
/// clipboard. Reuses the real stack compositor (blend modes, masks, isolated
/// groups) on a temporary stack whose offsets are shifted so the buffer only
/// covers the block's tile bbox clamped to the canvas, then crops to the exact
/// pixel content. Returns `None` when the block has no visible pixels in range
/// (or is too large to mirror onto the OS clipboard).
fn os_clipboard_block_image(
    block: &[Layer],
    canvas_w: u32,
    canvas_h: u32,
) -> Option<(u32, u32, Vec<u8>)> {
    use crate::core::tile::TILE_SIZE;

    let mut bbox: Option<(i32, i32, i32, i32)> = None;
    for layer in block {
        let (ox, oy) = layer.offset;
        for pos in layer.tiles.tiles.keys() {
            let x0 = pos.x * TILE_SIZE as i32 + ox;
            let y0 = pos.y * TILE_SIZE as i32 + oy;
            let (x1, y1) = (x0 + TILE_SIZE as i32, y0 + TILE_SIZE as i32);
            bbox = Some(match bbox {
                None => (x0, y0, x1, y1),
                Some((mx0, my0, mx1, my1)) => (mx0.min(x0), my0.min(y0), mx1.max(x1), my1.max(y1)),
            });
        }
    }
    let (bx0, by0, bx1, by1) = bbox?;
    // What-you-see semantics: clip to the canvas like every other output path.
    let x0 = bx0.max(0);
    let y0 = by0.max(0);
    let x1 = bx1.min(canvas_w as i32);
    let y1 = by1.min(canvas_h as i32);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (bw, bh) = ((x1 - x0) as u32, (y1 - y0) as u32);
    if bw as u64 * bh as u64 > 64_000_000 {
        return None; // keep OS clipboard writes bounded (~256MB RGBA)
    }

    // Temporary stack shifted to the buffer origin. Masks sample in layer-local
    // space and adjustment masks map through the offset, so the shift is exact
    // (same trick as the chunked flatten). Orphaned parent links are cleared so
    // visibility checks don't chase group ids outside the block.
    let ids: std::collections::HashSet<u32> = block.iter().map(|l| l.id).collect();
    let mut layers: Vec<Layer> = block.to_vec();
    for l in layers.iter_mut() {
        l.offset.0 -= x0;
        l.offset.1 -= y0;
        if l.parent_id.is_some_and(|p| !ids.contains(&p)) {
            l.parent_id = None;
        }
    }
    let next_id = layers.iter().map(|l| l.id).max().unwrap_or(0) + 1;
    let mut stack = crate::core::layer::LayerStack::new(1, 1);
    stack.layers = layers;
    stack.active_idx = 0;
    stack.set_next_id(next_id);

    let out = stack.flatten(bw, bh);
    let (cw, ch, mut rgba) = crop_to_alpha(bw, bh, out)?;
    flatten_onto_white(&mut rgba);
    Some((cw, ch, rgba))
}

/// Composite a straight-alpha RGBA buffer onto opaque white, in place, forcing
/// every pixel fully opaque.
///
/// The OS clipboard's DIB / bitmap formats are read by applications that ignore
/// the alpha channel (Windows Paint, many chat clients, Office "paste as
/// picture"). Those render a copied object's transparent surround — and its
/// see-through parts — as solid BLACK, so an in-app copy pasted into another
/// program came out as a black box (the reported "can't copy out to other apps"
/// bug). Flattening onto white first makes the copy paste correctly everywhere.
///
/// This only touches the flattened mirror sent to the *system* clipboard. iAi's
/// own paste keeps using the internal layer clipboard, so on-canvas transparency,
/// blend modes and layer structure are unaffected.
fn flatten_onto_white(rgba: &mut [u8]) {
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a == 255 {
            continue;
        }
        for c in px.iter_mut().take(3) {
            // straight-alpha over white: c·a/255 + 255·(255−a)/255, rounded.
            *c = ((*c as u32 * a + 255 * (255 - a) + 127) / 255) as u8;
        }
        px[3] = 255;
    }
}

/// Crop an RGBA buffer to the bounding box of its non-transparent pixels.
fn crop_to_alpha(w: u32, h: u32, rgba: Vec<u8>) -> Option<(u32, u32, Vec<u8>)> {
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for y in 0..h {
        let row = (y * w * 4) as usize;
        for x in 0..w {
            if rgba[row + (x * 4) as usize + 3] != 0 {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    if min_x == u32::MAX {
        return None;
    }
    let (cw, ch) = (max_x - min_x + 1, max_y - min_y + 1);
    if (cw, ch) == (w, h) {
        return Some((w, h, rgba));
    }
    let mut out = Vec::with_capacity((cw * ch * 4) as usize);
    for y in min_y..=max_y {
        let start = ((y * w + min_x) * 4) as usize;
        out.extend_from_slice(&rgba[start..start + (cw * 4) as usize]);
    }
    Some((cw, ch, out))
}

/// Shift needed to bring a pasted block back into view, given its
/// tile-granularity bounding box in canvas coords. A box that already
/// intersects the canvas keeps its position — paste preserves the copied
/// coordinates — so only content that would land entirely outside the canvas
/// (e.g. pasted into a smaller document) is moved: clamped to the nearest
/// edge, or centred when larger than the canvas.
fn paste_rescue_delta(
    (min_x, min_y, max_x, max_y): (i32, i32, i32, i32),
    canvas_w: u32,
    canvas_h: u32,
) -> (i32, i32) {
    let (w, h) = (canvas_w as i32, canvas_h as i32);
    if min_x < w && max_x > 0 && min_y < h && max_y > 0 {
        return (0, 0);
    }
    let (box_w, box_h) = (max_x - min_x, max_y - min_y);
    let target_x = if box_w <= w {
        min_x.clamp(0, w - box_w)
    } else {
        (w - box_w) / 2
    };
    let target_y = if box_h <= h {
        min_y.clamp(0, h - box_h)
    } else {
        (h - box_h) / 2
    };
    (target_x - min_x, target_y - min_y)
}

impl App {
    pub fn open_new_canvas_dialog_with_clipboard_hint(&mut self) {
        self.shell.ui.show_new_dialog = true;
        self.shell.ui.new_dpi = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .metadata
            .resolution_ppi;
        self.edit.clipboard_image_new_doc_hint = None;

        match read_os_clipboard_image() {
            Ok(Some(image)) => {
                self.shell.ui.new_w_input = image.width as f32;
                self.shell.ui.new_h_input = image.height as f32;
                self.shell.ui.new_unit = crate::core::units::Unit::Pixels;
                self.edit.clipboard_image_new_doc_hint = Some((image.width, image.height));
                self.shell.status_msg = format!(
                    "New canvas size from clipboard image: {}x{} px",
                    image.width, image.height
                );
            }
            Ok(None) => {}
            Err(err) => {
                self.shell.status_msg = err;
            }
        }
    }

    /// Copy selected layer(s) into the internal clipboard.
    /// - If multiple layers are selected (Shift-click), copies all of them.
    /// - If only one is selected (normal click), copies just that one.
    /// - A selected group folder carries its whole subtree (children + nested
    ///   folders) so "copy group" pastes back as a real folder, not an empty one.
    /// - Each raster layer's tiles are masked by the active selection if one exists.
    ///
    /// Whole `Layer`s are stored (offset, opacity, blend, mask, visibility and the
    /// `parent_id` group links), so paste reconstructs the layout and structure.
    /// The clipboard is shared across all open documents.
    pub fn do_copy(&mut self) {
        let canvas_w = self.docs.documents[self.docs.active_doc_idx].canvas.width;
        let canvas_h = self.docs.documents[self.docs.active_doc_idx].canvas.height;
        let active_idx = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .active_idx;

        // Seed indices (selected, or the active layer), expanded to include the full
        // subtree of any selected group folder.
        let indices: Vec<usize> = {
            let ls = &self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack;
            let selected: Vec<usize> = ls
                .layers
                .iter()
                .enumerate()
                .filter(|(_, l)| l.selected)
                .map(|(i, _)| i)
                .collect();
            let seeds = if selected.is_empty() {
                vec![active_idx]
            } else {
                selected
            };
            let mut set: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
            for &i in &seeds {
                set.insert(i);
                if ls.layers.get(i).is_some_and(|l| l.is_group()) {
                    for j in ls.group_member_range(i) {
                        set.insert(j);
                    }
                }
            }
            set.into_iter().collect()
        };

        // Clone the whole layers (in bottom→top stack order).
        let mut block: Vec<crate::core::layer::Layer> = {
            let layers = &self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack
                .layers;
            indices
                .iter()
                .filter_map(|&i| layers.get(i).cloned())
                .collect()
        };
        if block.is_empty() {
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return;
        }

        // Mask each raster layer's pixels by the active selection (region copy).
        // Group/adjustment headers have no pixels, so they are skipped.
        let has_sel = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .selection
            .active;
        if has_sel {
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .selection
                .refresh_bbox();
            let selection = &self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .selection;
            for layer in block.iter_mut() {
                if !layer.is_raster() {
                    continue;
                }
                mask_copied_layer_by_selection(layer, selection, canvas_w, canvas_h);
            }
        }

        let count = block.len();
        // Mirror a flattened crop onto the OS clipboard so the copy also pastes
        // into other applications; remember its hash so paste can tell our own
        // write apart from a fresher external copy.
        let os_sync = os_clipboard_block_image(&block, canvas_w, canvas_h);
        self.edit.os_clipboard_written =
            os_sync.and_then(|(bw, bh, rgba)| os_clipboard::write_image(bw, bh, &rgba).ok());
        self.edit.clipboard = Some(block);
        self.edit.clipboard_image_new_doc_hint = None;
        let os_note = if self.edit.os_clipboard_written.is_some() {
            " (+ system clipboard)"
        } else {
            ""
        };
        self.shell.status_msg = if count == 1 {
            format!("Copied 1 layer{os_note}")
        } else {
            format!("Copied {count} layers{os_note}")
        };
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Edit → Cut: copy the active selection to the clipboard, then erase those
    /// pixels from the active layer as one undo step. With no selection it degrades
    /// to a plain copy (nothing is erased).
    pub fn do_cut(&mut self) {
        self.do_copy();
        if self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .clear_selection()
        {
            self.shell.status_msg = "Cut selection".to_string();
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Paste the clipboard as new layer(s) on top of the stack, keeping each layer's
    /// copied position, properties and group structure. Undoable. Works across
    /// documents — the clipboard is app-level.
    pub fn do_paste(&mut self) {
        match read_os_clipboard_image() {
            Ok(Some(image)) if self.should_paste_os_clipboard_image(&image) => {
                self.paste_os_clipboard_image(image);
                return;
            }
            Ok(_) => {}
            Err(err) if self.edit.clipboard.is_none() => {
                self.shell.status_msg = err;
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
                return;
            }
            Err(_) => {}
        }

        let Some(mut block) = self.edit.clipboard.clone() else {
            self.shell.status_msg = "Clipboard empty".to_string();
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return;
        };

        let w = self.docs.documents[self.docs.active_doc_idx].canvas.width;
        let h = self.docs.documents[self.docs.active_doc_idx].canvas.height;
        let count = block.len();

        // Tile-granularity bounding box of the block's pixel content. Text and
        // shape layers keep rasterised tiles too, so every layer is scanned;
        // group/adjustment headers have no tiles and contribute nothing.
        let mut bbox: Option<(i32, i32, i32, i32)> = None;
        for layer in block.iter() {
            let (ox, oy) = layer.offset;
            for pos in layer.tiles.tiles.keys() {
                let x0 = pos.x * crate::core::tile::TILE_SIZE as i32 + ox;
                let y0 = pos.y * crate::core::tile::TILE_SIZE as i32 + oy;
                let x1 = x0 + crate::core::tile::TILE_SIZE as i32;
                let y1 = y0 + crate::core::tile::TILE_SIZE as i32;
                bbox = Some(match bbox {
                    None => (x0, y0, x1, y1),
                    Some((mx0, my0, mx1, my1)) => {
                        (mx0.min(x0), my0.min(y0), mx1.max(x1), my1.max(y1))
                    }
                });
            }
        }
        if let Some(bbox) = bbox {
            let (dx, dy) = paste_rescue_delta(bbox, w, h);
            if (dx, dy) != (0, 0) {
                for layer in block.iter_mut() {
                    layer.offset.0 += dx;
                    layer.offset.1 += dy;
                }
            }
        }

        let mut cmd = crate::core::command::LayerStructureCommand::capture_before(
            "Paste",
            &self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack,
            w,
            h,
        );

        self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .paste_layers(&block);

        // Fresh atlas revisions so the pasted tiles (and masks) upload.
        for l in self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .layers
            .iter_mut()
            .filter(|l| l.selected)
        {
            l.tiles.bump_all_revisions();
            if let Some(m) = l.mask.as_mut() {
                m.tiles.bump_all_revisions();
            }
        }

        cmd.capture_after(
            &self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack,
            w,
            h,
        );
        self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .record(Box::new(cmd));
        self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_revision += 1;

        self.upload_full();
        self.upload_selection_mask();

        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.shell.status_msg = if count == 1 {
            "Pasted as new layer".to_string()
        } else {
            format!("Pasted {} layers", count)
        };
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Whether Ctrl+V should paste the OS clipboard image instead of the internal
    /// layer clipboard. The internal clipboard wins only while the OS clipboard
    /// still holds what iai itself wrote (same content hash) — as soon as another
    /// app copies something newer, that image wins.
    fn should_paste_os_clipboard_image(&self, image: &OsClipboardImage) -> bool {
        if self.edit.clipboard.is_none() {
            return true;
        }
        if self.edit.clipboard_image_new_doc_hint == Some((image.width, image.height)) {
            return true;
        }
        match self.edit.os_clipboard_written {
            Some(written) => {
                os_clipboard::image_hash(image.width, image.height, &image.pixels) != written
            }
            // iai never wrote (or the write failed): can't tell which is newer,
            // keep the historical behaviour of preferring the internal copy.
            None => false,
        }
    }

    fn paste_os_clipboard_image(&mut self, image: OsClipboardImage) {
        let w = self.docs.documents[self.docs.active_doc_idx].canvas.width;
        let h = self.docs.documents[self.docs.active_doc_idx].canvas.height;

        let mut layer = Layer::from_rgba(
            0,
            "Clipboard Image",
            image.pixels,
            image.width,
            image.height,
        );
        layer.offset = centered_layer_offset(w, h, image.width, image.height);
        let block = vec![layer];

        let mut cmd = crate::core::command::LayerStructureCommand::capture_before(
            "Paste Image",
            &self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack,
            w,
            h,
        );

        self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .paste_layers(&block);

        for l in self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .layers
            .iter_mut()
            .filter(|l| l.selected)
        {
            l.tiles.bump_all_revisions();
        }

        cmd.capture_after(
            &self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack,
            w,
            h,
        );
        self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .record(Box::new(cmd));
        self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_revision += 1;

        self.upload_full();
        self.upload_selection_mask();

        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.edit.clipboard_image_new_doc_hint = None;
        self.shell.status_msg =
            format!("Pasted clipboard image {}x{} px", image.width, image.height);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::selection::Selection;
    use crate::core::tile::{Tile, TILE_BYTES, TILE_PIXELS, TILE_SIZE};

    #[test]
    fn paste_rescue_keeps_intersecting_box_in_place() {
        // Full-canvas layer whose tile extent rounds up past the canvas edge
        // (1000x800 canvas -> 1024x832 of tiles) must NOT be shifted.
        assert_eq!(paste_rescue_delta((0, 0, 1024, 832), 1000, 800), (0, 0));
        // Partially visible content keeps its copied position too.
        assert_eq!(paste_rescue_delta((-100, -100, 50, 50), 1000, 800), (0, 0));
        assert_eq!(
            paste_rescue_delta((900, 700, 1200, 1000), 1000, 800),
            (0, 0)
        );
    }

    #[test]
    fn paste_rescue_clamps_fully_offscreen_box_to_nearest_edge() {
        // 256x256 box entirely right of a 1000x800 canvas: pulled back to the
        // right edge, vertical position (already valid) untouched.
        assert_eq!(
            paste_rescue_delta((1500, 100, 1756, 356), 1000, 800),
            (744 - 1500, 0)
        );
        // Entirely above-left: clamped to the top-left corner.
        assert_eq!(
            paste_rescue_delta((-600, -500, -344, -244), 1000, 800),
            (600, 500)
        );
    }

    #[test]
    fn paste_rescue_centres_offscreen_box_larger_than_canvas() {
        // 2000x1500 box fully offscreen on a 1000x800 canvas: centred.
        assert_eq!(
            paste_rescue_delta((2000, 900, 4000, 2400), 1000, 800),
            (-500 - 2000, -350 - 900)
        );
    }

    fn selection_with_rect(w: u32, h: u32, x0: u32, y0: u32, x1: u32, y1: u32) -> Selection {
        let mut sel = Selection::new(w, h);
        sel.active = true;
        for y in y0..y1 {
            for x in x0..x1 {
                sel.mask[(y * w + x) as usize] = 255;
            }
        }
        sel
    }

    #[test]
    fn masked_copy_drops_fully_transparent_tiles() {
        // 512x512 opaque layer = 2x2 tiles; selection covers only the top-left
        // 100x100 corner, so three tiles become empty and must be dropped.
        let layer_px = vec![255u8; (512 * 512 * 4) as usize];
        let mut layer = Layer::from_rgba(1, "copy", layer_px, 512, 512);
        let sel = selection_with_rect(512, 512, 0, 0, 100, 100);
        mask_copied_layer_by_selection(&mut layer, &sel, 512, 512);

        assert_eq!(layer.tiles.tiles.len(), 1);
        let tile = layer
            .tiles
            .tiles
            .get(&crate::core::tile::TilePos { x: 0, y: 0 })
            .expect("top-left tile survives");
        let idx = |x: u32, y: u32| ((y * TILE_SIZE + x) * 4 + 3) as usize;
        assert_eq!(tile.pixels[idx(50, 50)], 255, "inside selection kept");
        assert_eq!(tile.pixels[idx(150, 150)], 0, "outside selection cleared");
    }

    #[test]
    fn masked_copy_also_masks_16bit_master() {
        let layer_px = vec![255u8; (256 * 256 * 4) as usize];
        let mut layer = Layer::from_rgba(1, "copy16", layer_px, 256, 256);
        for tile in layer.tiles.tiles.values_mut() {
            let tile = std::sync::Arc::make_mut(tile);
            tile.pixels16 = Some(vec![0xFFFFu16; TILE_PIXELS * 4]);
        }
        // Selection covers the left half only.
        let sel = selection_with_rect(256, 256, 0, 0, 128, 256);
        mask_copied_layer_by_selection(&mut layer, &sel, 256, 256);

        let tile = layer
            .tiles
            .tiles
            .get(&crate::core::tile::TilePos { x: 0, y: 0 })
            .expect("tile survives");
        let p16 = tile.pixels16.as_ref().expect("16-bit master kept");
        let idx = |x: u32, y: u32| ((y * TILE_SIZE + x) * 4 + 3) as usize;
        assert_eq!(p16[idx(10, 10)], 0xFFFF, "16-bit alpha kept in selection");
        assert_eq!(p16[idx(200, 10)], 0, "16-bit alpha cleared outside");
        assert_eq!(tile.pixels[idx(200, 10)], 0, "8-bit mirror cleared too");
    }

    #[test]
    fn block_image_crops_to_exact_content() {
        // 10x10 red layer at offset (5,7): the OS-clipboard mirror must come out
        // exactly 10x10 red, not padded to the tile bbox.
        let mut layer = Layer::from_rgba(1, "red", vec![255, 0, 0, 255].repeat(100), 10, 10);
        layer.offset = (5, 7);
        let (w, h, rgba) = os_clipboard_block_image(&[layer], 100, 80).expect("has content");
        assert_eq!((w, h), (10, 10));
        assert_eq!(&rgba[..4], &[255, 0, 0, 255]);
        assert_eq!(&rgba[rgba.len() - 4..], &[255, 0, 0, 255]);
    }

    #[test]
    fn block_image_composites_stack_order_and_opacity() {
        let red = Layer::from_rgba(1, "red", vec![255, 0, 0, 255].repeat(64), 8, 8);
        let mut green = Layer::from_rgba(2, "green", vec![0, 255, 0, 255].repeat(64), 8, 8);
        green.opacity = 0.5;
        let (w, h, rgba) = os_clipboard_block_image(&[red, green], 8, 8).expect("has content");
        assert_eq!((w, h), (8, 8));
        // 50% green over red: both channels near 127.
        assert!((rgba[0] as i32 - 127).abs() <= 2, "r={}", rgba[0]);
        assert!((rgba[1] as i32 - 127).abs() <= 2, "g={}", rgba[1]);
        assert_eq!(rgba[3], 255);
    }

    #[test]
    fn flatten_onto_white_fills_transparent_and_blends_edges() {
        // Three pixels: opaque red (kept), fully transparent (→ white), and a
        // half-opaque blue (→ blended halfway to white).
        let mut rgba = vec![
            255, 0, 0, 255, // opaque red
            0, 0, 0, 0, // fully transparent (rgb must not leak through as black)
            0, 0, 255, 128, // 50% blue
        ];
        flatten_onto_white(&mut rgba);
        assert_eq!(&rgba[0..4], &[255, 0, 0, 255], "opaque pixel untouched");
        assert_eq!(
            &rgba[4..8],
            &[255, 255, 255, 255],
            "transparent surround becomes opaque white, not black"
        );
        assert_eq!(rgba[11], 255, "blended pixel is now opaque");
        assert!((rgba[8] as i32 - 128).abs() <= 1, "r≈128: {}", rgba[8]);
        assert!((rgba[9] as i32 - 128).abs() <= 1, "g≈128: {}", rgba[9]);
        assert_eq!(rgba[10], 255, "blue channel stays saturated over white");
    }

    #[test]
    fn os_clipboard_mirror_is_opaque_over_white() {
        // A red disc-ish blob with a transparent surround: the OS-clipboard
        // mirror must come out fully opaque (no alpha, no black surround).
        let mut layer = Layer::from_rgba(1, "blob", vec![0u8; 16 * 16 * 4], 16, 16);
        // Paint a small opaque red square in the middle, leave the rest clear.
        for tile in layer.tiles.tiles.values_mut() {
            let tile = std::sync::Arc::make_mut(tile);
            for y in 4..12 {
                for x in 4..12 {
                    let i = ((y * TILE_SIZE + x) * 4) as usize;
                    tile.pixels[i] = 255;
                    tile.pixels[i + 3] = 255;
                }
            }
        }
        let (_, _, rgba) = os_clipboard_block_image(&[layer], 16, 16).expect("has content");
        assert!(
            rgba.chunks_exact(4).all(|px| px[3] == 255),
            "every mirrored pixel must be opaque"
        );
    }

    #[test]
    fn crop_to_alpha_trims_transparent_border() {
        // 6x6 buffer with a single opaque pixel at (2,3).
        let mut rgba = vec![0u8; 6 * 6 * 4];
        let i = ((3 * 6 + 2) * 4) as usize;
        rgba[i..i + 4].copy_from_slice(&[9, 9, 9, 255]);
        let (w, h, out) = crop_to_alpha(6, 6, rgba).expect("has content");
        assert_eq!((w, h), (1, 1));
        assert_eq!(out, vec![9, 9, 9, 255]);
        assert!(crop_to_alpha(4, 4, vec![0u8; 64]).is_none());
    }

    #[test]
    fn tile_prune_respects_faint_16bit_alpha() {
        // 8-bit mirror fully transparent, but the 16-bit master still holds a
        // faint alpha: the tile must survive the prune.
        let mut p16 = vec![0u16; TILE_PIXELS * 4];
        p16[3] = 1;
        let faint = Tile {
            pixels: vec![0u8; TILE_BYTES],
            pixels16: Some(p16),
            ink: None,
            revision: 0,
        };
        assert!(tile_has_visible_alpha(&faint));

        let empty = Tile {
            pixels: vec![0u8; TILE_BYTES],
            pixels16: None,
            ink: None,
            revision: 0,
        };
        assert!(!tile_has_visible_alpha(&empty));
    }
}

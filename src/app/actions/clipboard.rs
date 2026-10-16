//! Internal layer clipboard: copy / cut / paste. Split out of app/actions.rs.
//! Copy also mirrors a flattened crop of the copied block onto the OS clipboard
//! (via `app::os_clipboard`), so in-app copies paste into other applications and
//! paste can tell a fresher external copy from iai's own write.

use crate::app::os_clipboard::{self, OsClipboardImage};
use crate::app::render::CanvasEvent;
use crate::app::state::App;
use crate::core::canvas::selection_output::mask_copied_layer_by_selection;
use crate::core::layer::Layer;

/// The page the New Canvas dialog opens on with no image copied: A4, in cm,
/// at a print resolution.
const NEW_CANVAS_CM: (f32, f32) = (21.0, 29.7);
const NEW_CANVAS_DPI: f32 = 300.0;

fn read_os_clipboard_image() -> Result<Option<OsClipboardImage>, String> {
    os_clipboard::read_image()
}

fn centered_layer_offset(canvas_w: u32, canvas_h: u32, layer_w: u32, layer_h: u32) -> (i32, i32) {
    (
        ((canvas_w as i64 - layer_w as i64) / 2) as i32,
        ((canvas_h as i64 - layer_h as i64) / 2) as i32,
    )
}

/// Flatten a copied block into a tightly-cropped RGBA image for the OS
/// clipboard. Reuses the real stack compositor (blend modes, masks, isolated
/// groups) on a temporary stack whose offsets are shifted so the buffer only
/// covers the block's tile bbox clamped to the canvas, then crops to the exact
/// pixel content. Returns that content's top-left corner on the source canvas
/// with the image, or `None` when the block has no visible pixels in range
/// (or is too large to mirror onto the OS clipboard).
fn os_clipboard_block_image(
    block: &[Layer],
    canvas_w: u32,
    canvas_h: u32,
) -> Option<((i32, i32), u32, u32, Vec<u8>)> {
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
    // Carry the whole copied block, including content that extends past the
    // canvas. An upscale places a layer LARGER than the canvas
    // (centred with a negative offset), so clamping the mirror to the canvas
    // used to drop everything outside the original frame — copying that layer
    // and pasting it into a new page then lost the upscaled border. The block's
    // own content bbox keeps it whole; `crop_to_alpha` below still trims empty
    // margins, so layers that sit within the canvas are unaffected.
    let x0 = bx0;
    let y0 = by0;
    let x1 = bx1;
    let y1 = by1;
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let _ = (canvas_w, canvas_h);
    let (bw, bh) = ((x1 - x0) as u32, (y1 - y0) as u32);
    if bw as u64 * bh as u64 > 64_000_000 {
        return None; // keep OS clipboard writes bounded (~256MB RGBA)
    }

    // Temporary stack shifted to the buffer origin. Masks sample in layer-local
    // space and adjustment masks map through the offset, so the shift is exact
    // (same trick as the chunked flatten); a folder's mask lives in canvas
    // space and is moved with it. Orphaned parent links are cleared so
    // visibility checks don't chase group ids outside the block.
    let ids: std::collections::HashSet<u32> = block.iter().map(|l| l.id).collect();
    let mut layers: Vec<Layer> = block.to_vec();
    place_block(&mut layers, (-x0, -y0), bw, bh);
    for l in layers.iter_mut() {
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
    let ((cx, cy), cw, ch, mut rgba) = crop_to_alpha(bw, bh, out)?;
    flatten_onto_white(&mut rgba);
    Some(((x0 + cx as i32, y0 + cy as i32), cw, ch, rgba))
}

/// Move a copied block by `(dx, dy)` for its landing on a `canvas_w`×`canvas_h`
/// canvas. A folder holds no pixels and its mask lives in canvas space, so the
/// folder is re-cut to that canvas and its mask follows the move.
fn place_block(block: &mut [Layer], (dx, dy): (i32, i32), canvas_w: u32, canvas_h: u32) {
    for layer in block.iter_mut() {
        if !layer.is_group() {
            layer.offset.0 += dx;
            layer.offset.1 += dy;
            continue;
        }
        if let Some(mask) = layer.mask.as_mut() {
            if (dx, dy) != (0, 0) || (mask.width, mask.height) != (canvas_w, canvas_h) {
                *mask = mask.moved_onto(dx, dy, canvas_w, canvas_h);
            }
        }
        layer.tiles = crate::core::tile::TileMap::new(canvas_w, canvas_h);
        layer.width = canvas_w;
        layer.height = canvas_h;
        layer.offset = (0, 0);
    }
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
/// Returns the box's top-left corner in the buffer, its size and its pixels.
fn crop_to_alpha(w: u32, h: u32, rgba: Vec<u8>) -> Option<((u32, u32), u32, u32, Vec<u8>)> {
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
        return Some(((0, 0), w, h, rgba));
    }
    let mut out = Vec::with_capacity((cw * ch * 4) as usize);
    for y in min_y..=max_y {
        let start = ((y * w + min_x) * 4) as usize;
        out.extend_from_slice(&rgba[start..start + (cw * 4) as usize]);
    }
    Some(((min_x, min_y), cw, ch, out))
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

/// How far a copied block moves when it is pasted on a `w`×`h` canvas. On a
/// canvas of the size it was copied from it keeps its place, brought back into
/// view if it would land out of sight. On any other canvas the copied
/// coordinates mean nothing (a small photo pasted on a sheet landed in the
/// sheet's corner), so its content is centred, the way an image pasted from
/// another application is. `content` is the block's visible content on its
/// source canvas (`x, y, w, h`); `tile_bbox` (`x0, y0, x1, y1`) stands in for
/// it when that is not known.
fn paste_delta(
    source_canvas: Option<(u32, u32)>,
    content: Option<(i32, i32, u32, u32)>,
    tile_bbox: Option<(i32, i32, i32, i32)>,
    w: u32,
    h: u32,
) -> (i32, i32) {
    if source_canvas.is_none_or(|size| size == (w, h)) {
        return tile_bbox
            .map(|bbox| paste_rescue_delta(bbox, w, h))
            .unwrap_or((0, 0));
    }
    let content = content
        .or_else(|| tile_bbox.map(|(x0, y0, x1, y1)| (x0, y0, (x1 - x0) as u32, (y1 - y0) as u32)));
    let Some((x, y, content_w, content_h)) = content else {
        return (0, 0);
    };
    let (cx, cy) = centered_layer_offset(w, h, content_w, content_h);
    (cx - x, cy - y)
}

/// The rule behind `App::should_paste_os_clipboard_image`. `written` is the
/// hash of what iai last put on the OS clipboard, `mirror_size` the size of
/// the picture a copy mirrored there, `new_doc_hint` the size the New dialog
/// took from the OS clipboard image.
fn prefer_os_clipboard_image(
    has_internal: bool,
    written: Option<u64>,
    mirror_size: Option<(u32, u32)>,
    new_doc_hint: Option<(u32, u32)>,
    image: &OsClipboardImage,
) -> bool {
    if !has_internal {
        return true;
    }
    let size = (image.width, image.height);
    let iai_wrote_it = written.is_some_and(|written| {
        os_clipboard::image_hash(image.width, image.height, &image.pixels) == written
    });
    // The flattened mirror of the copied layers never wins over those layers,
    // not even when the New dialog took its size from it: a copied folder
    // must arrive as a folder.
    if iai_wrote_it && mirror_size == Some(size) {
        return false;
    }
    if new_doc_hint == Some(size) {
        return true;
    }
    // iai never wrote (or the write failed): can't tell which is newer, keep
    // the historical preference for the internal copy.
    written.is_some() && !iai_wrote_it
}

/// What the New Canvas dialog opens on (width, height, resolution, unit): an
/// A4 page at 300 dpi in cm, or the copied `image` in px, at its own
/// resolution. When iai made that image (`written` its hash, `mirror_size`
/// the size of the copy it mirrored), that is the resolution of the document
/// it was copied from; else the one the clipboard records (`image_ppi`), or
/// with none recorded the one an image file without any opens at.
fn new_canvas_start(
    image: Option<&OsClipboardImage>,
    written: Option<u64>,
    mirror_size: Option<(u32, u32)>,
    source_ppi: Option<f32>,
    image_ppi: Option<f32>,
) -> (f32, f32, f32, crate::core::units::Unit) {
    use crate::core::units::Unit;
    let Some(image) = image else {
        let (w, h) = NEW_CANVAS_CM;
        return (w, h, NEW_CANVAS_DPI, Unit::Centimeters);
    };
    let size = (image.width, image.height);
    let own = mirror_size == Some(size)
        && written.is_some_and(|written| {
            os_clipboard::image_hash(image.width, image.height, &image.pixels) == written
        });
    let ppi = source_ppi
        .filter(|_| own)
        .or(image_ppi)
        .unwrap_or_else(|| crate::core::canvas::CanvasMetadata::default().resolution_ppi);
    (size.0 as f32, size.1 as f32, ppi, Unit::Pixels)
}

impl App {
    pub fn open_new_canvas_dialog_with_clipboard_hint(&mut self) {
        self.shell.ui.show_new_dialog = true;
        let image = match read_os_clipboard_image() {
            Ok(image) => image,
            Err(err) => {
                self.shell.status_msg = err;
                None
            }
        };
        let ui = &mut self.shell.ui;
        (ui.new_w_input, ui.new_h_input, ui.new_dpi, ui.new_unit) = new_canvas_start(
            image.as_ref(),
            self.edit.os_clipboard_written,
            self.edit.clipboard_mirror_rect.map(|(_, _, w, h)| (w, h)),
            self.edit.clipboard_source_ppi,
            image.as_ref().and_then(|_| os_clipboard::read_image_ppi()),
        );
        self.edit.clipboard_image_new_doc_hint = image.as_ref().map(|i| (i.width, i.height));
        if let Some(image) = image {
            self.shell.status_msg = format!(
                "New canvas size from clipboard image: {}x{} px",
                image.width, image.height
            );
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
        let canvas_w = self.docs.active().canvas.width;
        let canvas_h = self.docs.active().canvas.height;
        let active_idx = self.docs.active().canvas.layer_stack.active_idx;

        // Seed indices (selected, or the active layer), expanded to include the full
        // subtree of any selected group folder.
        let indices: Vec<usize> = {
            let ls = &self.docs.active().canvas.layer_stack;
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
            let layers = &self.docs.active().canvas.layer_stack.layers;
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
        let has_sel = self.docs.active().canvas.selection.active;
        if has_sel {
            self.docs.active_mut().canvas.selection.refresh_bbox();
            let selection = &self.docs.active().canvas.selection;
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
        self.edit.clipboard_mirror_rect =
            os_sync.as_ref().map(|&((x, y), bw, bh, _)| (x, y, bw, bh));
        self.edit.clipboard_source_canvas = Some((canvas_w, canvas_h));
        self.edit.clipboard_source_ppi = Some(self.docs.active().canvas.metadata.resolution_ppi);
        self.edit.os_clipboard_written =
            os_sync.and_then(|(_, bw, bh, rgba)| os_clipboard::write_image(bw, bh, &rgba).ok());
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
        if self.docs.active_mut().canvas.clear_selection() {
            self.shell.status_msg = "Cut selection".to_string();
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Paste the clipboard as new layer(s) on top of the stack, keeping each layer's
    /// properties and group structure, and its copied position on a canvas of the
    /// size it came from (centred on any other, see `paste_delta`). Undoable.
    /// Works across documents — the clipboard is app-level.
    pub fn do_paste(&mut self) {
        let t0 = std::time::Instant::now();
        match read_os_clipboard_image() {
            Ok(Some(image)) if self.should_paste_os_clipboard_image(&image) => {
                let t_read = t0.elapsed().as_millis();
                self.paste_os_clipboard_image(image);
                // Diagnostic: clipboard read + layer insert + upload on the UI thread.
                let ms = t0.elapsed().as_millis();
                if ms >= 30 {
                    let text = format!("paste clipboard image {ms} ms (read {t_read} ms)");
                    eprintln!("iai[perf]: {text}");
                    crate::diag::note("perf", &text);
                }
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

        let w = self.docs.active().canvas.width;
        let h = self.docs.active().canvas.height;
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
        // The New dialog's hint was for this paste; it must not outlive it.
        self.edit.clipboard_image_new_doc_hint = None;
        let delta = paste_delta(
            self.edit.clipboard_source_canvas,
            self.edit.clipboard_mirror_rect,
            bbox,
            w,
            h,
        );
        place_block(&mut block, delta, w, h);

        let mut cmd = crate::core::command::LayerStructureCommand::capture_before(
            "Paste",
            &self.docs.active().canvas.layer_stack,
            w,
            h,
        );

        self.docs
            .active_mut()
            .canvas
            .layer_stack
            .paste_layers(&block);

        // Fresh atlas revisions so the pasted tiles (and masks) upload.
        for l in self
            .docs
            .active_mut()
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

        cmd.capture_after(&self.docs.active().canvas.layer_stack, w, h);
        self.docs.active_mut().canvas.record(Box::new(cmd));
        self.docs.active_mut().canvas.layer_revision += 1;

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
        prefer_os_clipboard_image(
            self.edit.clipboard.is_some(),
            self.edit.os_clipboard_written,
            self.edit.clipboard_mirror_rect.map(|(_, _, w, h)| (w, h)),
            self.edit.clipboard_image_new_doc_hint,
            image,
        )
    }

    fn paste_os_clipboard_image(&mut self, image: OsClipboardImage) {
        let w = self.docs.active().canvas.width;
        let h = self.docs.active().canvas.height;

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
            &self.docs.active().canvas.layer_stack,
            w,
            h,
        );

        self.docs
            .active_mut()
            .canvas
            .layer_stack
            .paste_layers(&block);

        for l in self
            .docs
            .active_mut()
            .canvas
            .layer_stack
            .layers
            .iter_mut()
            .filter(|l| l.selected)
        {
            l.tiles.bump_all_revisions();
        }

        cmd.capture_after(&self.docs.active().canvas.layer_stack, w, h);
        self.docs.active_mut().canvas.record(Box::new(cmd));
        self.docs.active_mut().canvas.layer_revision += 1;

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
        let (origin, w, h, rgba) =
            os_clipboard_block_image(&[layer], 100, 80).expect("has content");
        assert_eq!(origin, (5, 7), "where the content sat on the canvas");
        assert_eq!((w, h), (10, 10));
        assert_eq!(&rgba[..4], &[255, 0, 0, 255]);
        assert_eq!(&rgba[rgba.len() - 4..], &[255, 0, 0, 255]);
    }

    #[test]
    fn block_image_keeps_content_past_the_canvas() {
        // An upscaled layer is LARGER than the canvas and centred
        // with a negative offset. The OS-clipboard mirror must carry the whole
        // layer, not just the part inside the original 100x100 frame.
        let mut layer = Layer::from_rgba(
            1,
            "upscaled",
            [7u8, 7, 7, 255].repeat((300 * 300) as usize),
            300,
            300,
        );
        layer.offset = (-100, -100);
        let (origin, w, h, rgba) =
            os_clipboard_block_image(&[layer], 100, 100).expect("has content");
        assert_eq!(origin, (-100, -100));
        assert_eq!(
            (w, h),
            (300, 300),
            "full layer survives, not clipped to canvas"
        );
        assert_eq!(&rgba[..4], &[7, 7, 7, 255]);
    }

    #[test]
    fn block_image_composites_stack_order_and_opacity() {
        let red = Layer::from_rgba(1, "red", vec![255, 0, 0, 255].repeat(64), 8, 8);
        let mut green = Layer::from_rgba(2, "green", vec![0, 255, 0, 255].repeat(64), 8, 8);
        green.opacity = 0.5;
        let (_, w, h, rgba) = os_clipboard_block_image(&[red, green], 8, 8).expect("has content");
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
        let (_, _, _, rgba) = os_clipboard_block_image(&[layer], 16, 16).expect("has content");
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
        let (origin, w, h, out) = crop_to_alpha(6, 6, rgba).expect("has content");
        assert_eq!(origin, (2, 3));
        assert_eq!((w, h), (1, 1));
        assert_eq!(out, vec![9, 9, 9, 255]);
        assert!(crop_to_alpha(4, 4, vec![0u8; 64]).is_none());
    }

    fn os_image(width: u32, height: u32, fill: u8) -> OsClipboardImage {
        OsClipboardImage {
            width,
            height,
            pixels: vec![fill; (width * height * 4) as usize],
        }
    }

    #[test]
    fn the_new_dialog_opens_on_a4_or_on_the_image_just_copied() {
        use crate::core::units::Unit;
        // Nothing copied: an A4 page at 300 dpi, in cm (2480 × 3508 px).
        let a4 = new_canvas_start(None, None, None, Some(600.0), None);
        assert_eq!(a4, (21.0, 29.7, 300.0, Unit::Centimeters));
        let px = |v: f32| crate::ui::dialogs::new_canvas_pixels(v, a4.3, a4.2);
        assert_eq!((px(a4.0), px(a4.1)), (2480, 3508));
        // An ID photo copied in iai: its size in px, at its own 600 dpi.
        let mirror = os_image(661, 898, 7);
        let written = Some(os_clipboard::image_hash(661, 898, &mirror.pixels));
        let start = new_canvas_start(Some(&mirror), written, Some((661, 898)), Some(600.0), None);
        assert_eq!(start, (661.0, 898.0, 600.0, Unit::Pixels));
        // An image copied in another app: its size in px, at the resolution
        // the clipboard records...
        let other = os_image(661, 898, 9);
        let from = |image_ppi| {
            new_canvas_start(
                Some(&other),
                written,
                Some((661, 898)),
                Some(600.0),
                image_ppi,
            )
        };
        assert_eq!(from(Some(96.0)), (661.0, 898.0, 96.0, Unit::Pixels));
        // ...and with none, as an image file without one opens: 72.
        assert_eq!(from(None), (661.0, 898.0, 72.0, Unit::Pixels));
    }

    #[test]
    fn snap_on_in_preferences_snaps_from_the_first_drag() {
        let app = App::new();
        // Tests read no prefs.json: the settings are the defaults.
        assert!(app.shell.settings.snap_default && app.shell.ui.snap_enabled);
        let tools = &app.edit.tools;
        assert!(tools.move_tool().snap_enabled);
        assert!(tools.arrow().snap_enabled && tools.pen().snap_enabled);
    }

    #[test]
    fn own_copy_is_pasted_as_layers_even_after_the_new_dialog_took_its_size() {
        // Ctrl+C, Ctrl+N (the dialog reads the size of iai's own mirror),
        // Ctrl+V: the layers must win, or a copied folder arrives flattened.
        let mirror = os_image(20, 10, 7);
        let written = Some(os_clipboard::image_hash(20, 10, &mirror.pixels));
        let size = Some((20, 10));
        assert!(!prefer_os_clipboard_image(
            true, written, size, size, &mirror
        ));
        assert!(!prefer_os_clipboard_image(
            true, written, size, None, &mirror
        ));
    }

    #[test]
    fn an_image_iai_sent_elsewhere_is_not_the_copied_layers() {
        // iai put an image on the OS clipboard for the browser, not as the
        // mirror of a copy. Ctrl+V keeps the layers copied earlier, but a New
        // canvas sized from that image is still followed by that image.
        let sent = os_image(20, 10, 7);
        let written = Some(os_clipboard::image_hash(20, 10, &sent.pixels));
        let older_copy = Some((64, 64));
        assert!(!prefer_os_clipboard_image(
            true, written, older_copy, None, &sent
        ));
        assert!(prefer_os_clipboard_image(
            true,
            written,
            older_copy,
            Some((20, 10)),
            &sent
        ));
    }

    #[test]
    fn an_image_from_another_app_still_wins() {
        let external = os_image(20, 10, 9);
        let written = Some(os_clipboard::image_hash(
            20,
            10,
            &os_image(20, 10, 7).pixels,
        ));
        let size = Some((20, 10));
        // Newer than iai's own write, even at the very same size.
        assert!(prefer_os_clipboard_image(
            true, written, size, None, &external
        ));
        // The New dialog took its size from it.
        assert!(prefer_os_clipboard_image(true, None, None, size, &external));
        // Nothing copied in iai at all.
        assert!(prefer_os_clipboard_image(
            false, None, None, None, &external
        ));
        // iai never wrote and there is no hint: the internal copy is kept.
        assert!(!prefer_os_clipboard_image(
            true, None, None, None, &external
        ));
    }

    #[test]
    fn a_block_pasted_on_another_size_of_canvas_is_centred() {
        // A 1004x650 card copied whole, pasted on an A4 sheet: every paste
        // lands in the middle of the sheet, not in its top-left corner.
        let card = Some((1004, 650));
        let content = Some((0, 0, 1004, 650));
        let tiles = Some((0, 0, 1024, 768));
        assert_eq!(
            paste_delta(card, content, tiles, 2480, 3508),
            ((2480 - 1004) / 2, (3508 - 650) / 2)
        );
        // Content that sat away from the corner is centred all the same.
        assert_eq!(
            paste_delta(Some((600, 400)), Some((300, 120, 200, 100)), None, 400, 300),
            (100 - 300, 100 - 120)
        );
        // A canvas of exactly the content's size takes it corner to corner.
        assert_eq!(
            paste_delta(Some((600, 400)), Some((300, 120, 200, 100)), None, 200, 100),
            (-300, -120)
        );
        // Nothing visible was mirrored: the tile box stands in for the content.
        assert_eq!(
            paste_delta(card, None, Some((256, 0, 512, 256)), 2480, 3508),
            ((2480 - 256) / 2 - 256, (3508 - 256) / 2)
        );
    }

    #[test]
    fn a_block_pasted_on_a_canvas_of_its_own_size_keeps_its_place() {
        let size = Some((1000, 800));
        let content = Some((700, 500, 50, 50));
        let tiles = Some((512, 256, 768, 768));
        assert_eq!(paste_delta(size, content, tiles, 1000, 800), (0, 0));
        // Out of sight there (the canvas was cropped since): brought back.
        assert_eq!(
            paste_delta(size, None, Some((1500, 100, 1756, 356)), 1000, 800),
            (744 - 1500, 0)
        );
        // Where the block came from is not known: the copied coordinates.
        assert_eq!(paste_delta(None, content, tiles, 2480, 3508), (0, 0));
    }

    /// Two layers in a folder, well away from the canvas corner, copied the way
    /// `do_copy` does it: the folder's members, then its header.
    fn copied_folder() -> Vec<Layer> {
        use crate::core::layer::LayerStack;
        let mut stack = LayerStack::new(600, 400);
        let mut red = Layer::from_rgba(1, "red", [255, 0, 0, 255].repeat(40 * 30), 40, 30);
        red.offset = (300, 120);
        red.selected = true;
        let mut blue = Layer::from_rgba(2, "blue", [0, 0, 255, 255].repeat(20 * 20), 20, 20);
        blue.offset = (350, 160);
        blue.selected = true;
        stack.layers.push(red);
        stack.layers.push(blue);
        stack.set_next_id(3);
        let header = stack
            .create_group_from_selected(600, 400)
            .expect("two layers group");
        stack.layers[stack.group_member_range(header).start..=header].to_vec()
    }

    #[test]
    fn a_copied_folder_pasted_on_a_canvas_of_its_size_is_the_same_picture() {
        use crate::core::layer::LayerStack;
        let block = copied_folder();
        let (origin, mw, mh, mirror) =
            os_clipboard_block_image(&block, 600, 400).expect("has content");
        assert_eq!((origin, mw, mh), ((300, 120), 70, 60));

        // New canvas of the mirror's size, then the paste.
        let mut placed = block.clone();
        let delta = paste_delta(
            Some((600, 400)),
            Some((origin.0, origin.1, mw, mh)),
            None,
            mw,
            mh,
        );
        assert_eq!(delta, (-300, -120));
        place_block(&mut placed, delta, mw, mh);
        let mut dst = LayerStack::new(mw, mh);
        dst.paste_layers(&placed);

        // Still a folder holding its two layers, not one flat image.
        let header = dst
            .layers
            .iter()
            .find(|l| l.is_group())
            .expect("the folder came across");
        assert_eq!((header.width, header.height), (mw, mh));
        let inside = dst
            .layers
            .iter()
            .filter(|l| l.parent_id == Some(header.id))
            .count();
        assert_eq!(inside, 2, "both layers are in the pasted folder");
        // And it shows exactly what the copy showed (the mirror is on white,
        // the new canvas has a white Background).
        assert_eq!(dst.flatten(mw, mh), mirror);
    }

    #[test]
    fn a_folder_mask_follows_the_block_to_its_new_canvas() {
        // The folder hides its left half (canvas x < 335) on the source canvas.
        let mut block = copied_folder();
        let header = block.last_mut().expect("header is last");
        header.add_mask(true);
        let mask = header.mask.as_mut().expect("mask added");
        mask.tiles
            .write_region(0, 0, 335, 400, &[0u8, 0, 0, 255].repeat(335 * 400));

        // The mirror already honours the mask where the content is.
        let (origin, mw, mh, mirror) =
            os_clipboard_block_image(&block, 600, 400).expect("has content");
        assert_eq!((origin, mw, mh), ((335, 120), 35, 60));

        place_block(&mut block, (-origin.0, -origin.1), mw, mh);
        let header = block.last().expect("header is last");
        let mask = header.mask.as_ref().expect("mask kept");
        assert_eq!((mask.width, mask.height), (mw, mh));
        assert_eq!(mask.sample(0, 0), 1.0, "the revealed half came along");

        let mut dst = crate::core::layer::LayerStack::new(mw, mh);
        dst.paste_layers(&block);
        assert_eq!(dst.flatten(mw, mh), mirror);
    }

    #[test]
    fn tile_prune_respects_faint_16bit_alpha() {
        use crate::core::canvas::selection_output::tile_has_visible_alpha;
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

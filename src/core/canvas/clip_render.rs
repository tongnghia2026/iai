//! Live PowerClip rendering (Bước 6, feature layer).
//!
//! The clipping *relation* is `Layer::clip_parent_id` (see [`super::clip_ops`]);
//! this turns that relation into something visible. Rather than teach the GPU
//! compositor to sample a second layer's alpha (a large shader change), we bake
//! the frame's coverage into a *managed raster mask* on each content layer. The
//! existing mask path — which both the GPU shader and the CPU flatten already
//! honour — then clips the content for free, with no shader changes.
//!
//! The mask is DERIVED state, like the vector raster cache: it is rebuilt from
//! the frame whenever geometry changes (called on the structural recomposite
//! path and on load) and is never the source of truth. Because it is baked into
//! the content layer's OWN local space, moving the content re-samples the frame
//! at the content's new position on the next refresh, so the clip stays pinned
//! to the frame's shape.

use rayon::prelude::*;

use super::Canvas;
use crate::core::layer::{Layer, LayerMask};
use crate::core::tile::TileMap;

impl Canvas {
    /// Whether any layer is PowerClip content (has a clip frame). A cheap gate so
    /// the common no-clip document pays nothing.
    pub fn has_clip_content(&self) -> bool {
        self.layer_stack
            .layers
            .iter()
            .any(|l| l.clip_parent_id.is_some())
    }

    /// Fingerprint of the clip-relevant geometry: each content layer's identity/
    /// offset/size and its frame's identity/offset/size/coverage. Content PIXELS
    /// are intentionally excluded — painting inside a clip does not move the clip.
    fn clip_state_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for layer in &self.layer_stack.layers {
            let Some(fid) = layer.clip_parent_id else {
                continue;
            };
            layer.id.hash(&mut h);
            layer.offset.hash(&mut h);
            layer.width.hash(&mut h);
            layer.height.hash(&mut h);
            if let Some(frame) = self.layer_stack.layers.iter().find(|l| l.id == fid) {
                frame.id.hash(&mut h);
                frame.offset.hash(&mut h);
                frame.width.hash(&mut h);
                frame.height.hash(&mut h);
                frame.tiles.revision_fingerprint().hash(&mut h);
            }
        }
        h.finish()
    }

    /// Rebuild the managed clip mask on every PowerClip content layer from its
    /// frame's current coverage. No-op (returns `false`) when nothing is clipped
    /// or nothing clip-relevant changed since the last bake. Mutates masks
    /// directly — this is derived state, outside history.
    pub fn refresh_clip_masks(&mut self) -> bool {
        if !self.has_clip_content() {
            self.clip_fp = 0;
            return false;
        }
        let fp = self.clip_state_fingerprint();
        if fp == self.clip_fp {
            return false;
        }
        self.clip_fp = fp;
        // Read pass: bake each content layer's mask against its frame. Collect
        // first so we never hold an immutable borrow (the frame) while mutating a
        // content layer.
        let mut baked: Vec<(usize, LayerMask)> = Vec::new();
        for (i, layer) in self.layer_stack.layers.iter().enumerate() {
            let Some(fid) = layer.clip_parent_id else {
                continue;
            };
            let Some(frame) = self.layer_stack.layers.iter().find(|l| l.id == fid) else {
                continue;
            };
            baked.push((
                i,
                bake_clip_mask(frame, layer.offset, layer.width, layer.height),
            ));
        }
        let any = !baked.is_empty();
        for (i, mask) in baked {
            self.layer_stack.layers[i].mask = Some(mask);
        }
        any
    }
}

/// Build a layer-local mask whose value at `(lx, ly)` is the frame's coverage
/// (alpha) at the same CANVAS position, so the content shows only inside the
/// frame's shape. Areas outside the frame are hidden.
fn bake_clip_mask(frame: &Layer, content_offset: (i32, i32), cw: u32, ch: u32) -> LayerMask {
    let (fw, fh) = (frame.width, frame.height);
    let (fox, foy) = frame.offset;

    // Extract the frame's alpha once into a flat buffer so the (usually larger)
    // content loop is plain array indexing rather than a tile lookup per pixel.
    // Row-parallel: get_pixel is an immutable tile lookup, so rows are independent.
    let mut frame_alpha = vec![0u8; (fw as usize) * (fh as usize)];
    frame_alpha
        .par_chunks_mut(fw as usize)
        .enumerate()
        .for_each(|(y, row)| {
            let y = y as u32;
            for x in 0..fw {
                row[x as usize] = frame.tiles.get_pixel(x, y).3;
            }
        });

    // Fill row-parallel: baking a full-image-sized mask on one thread was the
    // ~2s stall before the next drag (CPU pinned at ~one core). Rows are
    // independent and only READ the shared frame_alpha, so rayon splits it across
    // all cores. Rows wholly outside the frame short-circuit to transparent.
    let mut buf = vec![0u8; (cw as usize) * (ch as usize) * 4];
    let row_bytes = (cw as usize) * 4;
    buf.par_chunks_mut(row_bytes)
        .enumerate()
        .for_each(|(ly, row)| {
            let fy = content_offset.1 + ly as i32 - foy;
            if fy < 0 || (fy as u32) >= fh {
                return; // outside the frame vertically → row stays transparent
            }
            let frame_row = (fy as usize) * (fw as usize);
            for lx in 0..cw as usize {
                let fx = content_offset.0 + lx as i32 - fox;
                let a = if fx >= 0 && (fx as u32) < fw {
                    frame_alpha[frame_row + fx as usize]
                } else {
                    0
                };
                let idx = lx * 4;
                row[idx] = a;
                row[idx + 1] = a;
                row[idx + 2] = a;
                row[idx + 3] = 255;
            }
        });

    LayerMask {
        tiles: TileMap::from_rgba(&buf, cw, ch),
        width: cw,
        height: ch,
        enabled: true,
        inverted: false,
        // Remember where the content AND its frame sat when this mask was baked.
        // The live compositor pins the clip with
        //   shift = (content.offset − bake_offset) − (frame.offset − bake_frame_offset)
        // so dragging either the image or the frame keeps the clip live with no
        // per-frame re-bake (that per-frame re-bake was the Move lag).
        bake_offset: content_offset,
        bake_frame_offset: (fox, foy),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tile::TileMap;

    /// A frame layer: opaque disc-ish square in the middle of a 40×40 canvas.
    fn make_frame(stack_w: u32, stack_h: u32) -> (Canvas, u32, u32) {
        let mut canvas = Canvas::new(stack_w, stack_h);
        // Frame: a 20×20 opaque square placed at (10,10).
        let frame_idx = canvas.layer_stack.add_layer(stack_w, stack_h);
        let opaque = vec![255u8; 20 * 20 * 4];
        {
            let f = &mut canvas.layer_stack.layers[frame_idx];
            f.tiles = TileMap::from_rgba(&opaque, 20, 20);
            f.width = 20;
            f.height = 20;
            f.offset = (10, 10);
        }
        let frame_id = canvas.layer_stack.layers[frame_idx].id;

        // Content: a full-canvas opaque layer.
        let content_idx = canvas.layer_stack.add_layer(stack_w, stack_h);
        let full = vec![255u8; (stack_w * stack_h * 4) as usize];
        {
            let c = &mut canvas.layer_stack.layers[content_idx];
            c.tiles = TileMap::from_rgba(&full, stack_w, stack_h);
            c.width = stack_w;
            c.height = stack_h;
            c.offset = (0, 0);
        }
        let content_id = canvas.layer_stack.layers[content_idx].id;
        canvas
            .layer_stack
            .attach_clipped_child(content_id, frame_id)
            .unwrap();
        (canvas, frame_id, content_id)
    }

    #[test]
    fn clip_mask_reveals_only_inside_the_frame() {
        let (mut canvas, _frame_id, content_id) = make_frame(40, 40);
        assert!(canvas.refresh_clip_masks(), "something was clipped");
        let content = canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == content_id)
            .unwrap();
        let mask = content.mask.as_ref().expect("clip mask baked");
        // Inside the frame square (15,15) → revealed.
        assert!(mask.sample(15, 15) > 0.9, "inside frame revealed");
        // Outside the frame (2,2) and (35,35) → hidden.
        assert!(mask.sample(2, 2) < 0.1, "top-left hidden");
        assert!(mask.sample(35, 35) < 0.1, "bottom-right hidden");
        // Frame edge boundary: (9,9) just outside, (10,10) just inside.
        assert!(mask.sample(9, 9) < 0.1);
        assert!(mask.sample(10, 10) > 0.9);
    }

    #[test]
    fn no_clip_content_is_a_cheap_noop() {
        let mut canvas = Canvas::new(16, 16);
        canvas.layer_stack.add_layer(16, 16);
        assert!(!canvas.has_clip_content());
        assert!(!canvas.refresh_clip_masks());
    }

    #[test]
    fn clip_mask_records_bake_offsets_for_live_pin() {
        // The live compositor pins a dragged clip by sampling the (unmoved) mask
        // at layer_local + shift, where
        //   shift = (content.offset − bake_offset) − (frame.offset − bake_frame_offset).
        // So the bake must stamp BOTH the content's and the frame's offset; a later
        // move that skips the re-bake then yields the exact live shift.
        let (mut canvas, frame_id, content_id) = make_frame(40, 40);
        canvas.refresh_clip_masks();
        let ci = canvas
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == content_id)
            .unwrap();
        let fi = canvas
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == frame_id)
            .unwrap();
        {
            let mask = canvas.layer_stack.layers[ci].mask.as_ref().unwrap();
            assert_eq!(mask.bake_offset, (0, 0), "content offset at bake");
            assert_eq!(mask.bake_frame_offset, (10, 10), "frame offset at bake");
        }

        // Mirror the compositor's shift formula.
        let shift = |canvas: &Canvas| {
            let m = canvas.layer_stack.layers[ci].mask.as_ref().unwrap();
            let c = canvas.layer_stack.layers[ci].offset;
            let f = canvas.layer_stack.layers[fi].offset;
            (
                (c.0 - m.bake_offset.0) - (f.0 - m.bake_frame_offset.0),
                (c.1 - m.bake_offset.1) - (f.1 - m.bake_frame_offset.1),
            )
        };

        // Drag the CONTENT (frame fixed), no re-bake: shift == drag delta.
        canvas.layer_stack.layers[ci].offset = (7, -3);
        assert_eq!(shift(&canvas), (7, -3), "content drag pins by +delta");

        // Drag the FRAME (content back at 0,0), no re-bake: shift == −frame delta,
        // so the clip window follows the frame instead of lagging behind.
        canvas.layer_stack.layers[ci].offset = (0, 0);
        canvas.layer_stack.layers[fi].offset = (15, 12);
        assert_eq!(shift(&canvas), (-5, -2), "frame drag pins by −delta");

        // Re-baking at the new positions resets the shift to zero.
        canvas.refresh_clip_masks();
        assert_eq!(shift(&canvas), (0, 0), "fresh bake ⇒ no shift");
    }

    #[test]
    fn moving_content_repins_clip_to_the_frame() {
        // After shifting the content, a refresh must re-pin the visible region to
        // the frame's fixed canvas position, not drag it along with the content.
        let (mut canvas, _fid, content_id) = make_frame(40, 40);
        canvas.refresh_clip_masks();
        // Shift content right by 10px.
        let ci = canvas
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == content_id)
            .unwrap();
        canvas.layer_stack.layers[ci].offset = (10, 0);
        canvas.refresh_clip_masks();
        let mask = canvas.layer_stack.layers[ci].mask.as_ref().unwrap();
        // Frame covers canvas x∈[10,30). Content now starts at x=10, so local
        // lx=0 maps to canvas x=10 (inside) and lx=25 maps to x=35 (outside).
        assert!(
            mask.sample(0, 15) > 0.9,
            "content-local 0 is inside frame now"
        );
        assert!(
            mask.sample(25, 15) < 0.1,
            "content-local 25 is outside frame"
        );
    }
}

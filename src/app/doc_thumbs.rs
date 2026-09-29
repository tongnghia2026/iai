//! Hover previews for the document tab strip and its overflow list: one small
//! composite per open document, rendered on a worker thread and re-rendered
//! only when the document's content key changes.

use crate::core::document::{Document, DocumentId};
use crate::core::layer::LayerStack;
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};

/// Longest side of a preview, in pixels.
const DOC_THUMB_MAX: u32 = 256;
/// Checkerboard cell (preview pixels) shown under transparent areas.
const CHECKER: u32 = 6;

struct Job {
    id: DocumentId,
    key: u64,
    stack: LayerStack,
    width: u32,
    height: u32,
}

type JobResult = (DocumentId, u64, Option<egui::ColorImage>);

#[derive(Default)]
pub struct DocThumbs {
    tex: HashMap<DocumentId, (u64, egui::TextureHandle)>,
    in_flight: HashMap<DocumentId, u64>,
    /// Keys that rendered nothing, so an empty document is not retried every frame.
    failed: HashMap<DocumentId, u64>,
    req_tx: Option<Sender<Job>>,
    res_rx: Option<Receiver<JobResult>>,
}

impl DocThumbs {
    fn ensure_worker(&mut self) {
        if self.req_tx.is_some() {
            return;
        }
        let (req_tx, req_rx) = std::sync::mpsc::channel::<Job>();
        let (res_tx, res_rx) = std::sync::mpsc::channel::<JobResult>();
        let spawned = std::thread::Builder::new()
            .name("doc-thumbs".into())
            .spawn(move || {
                while let Ok(mut job) = req_rx.recv() {
                    let image = render_preview(&mut job.stack, job.width, job.height);
                    if res_tx.send((job.id, job.key, image)).is_err() {
                        break;
                    }
                }
            });
        if spawned.is_ok() {
            self.req_tx = Some(req_tx);
            self.res_rx = Some(res_rx);
        }
    }

    /// Queue a fresh preview of `doc` unless the resident one is current or one
    /// is already rendering. Returns `true` when work was queued.
    pub fn request(&mut self, doc: &Document) -> bool {
        if doc.is_flow_text() || self.in_flight.contains_key(&doc.id) {
            return false;
        }
        let key = content_key(doc);
        if self.tex.get(&doc.id).is_some_and(|(k, _)| *k == key)
            || self.failed.get(&doc.id) == Some(&key)
        {
            return false;
        }
        self.ensure_worker();
        let Some(tx) = &self.req_tx else {
            return false;
        };
        let job = Job {
            id: doc.id,
            key,
            stack: doc.canvas.layer_stack.clone(),
            width: doc.canvas.width,
            height: doc.canvas.height,
        };
        if tx.send(job).is_err() {
            self.req_tx = None;
            self.res_rx = None;
            return false;
        }
        self.in_flight.insert(doc.id, key);
        true
    }

    /// Upload finished previews and drop those of closed documents.
    pub fn poll(&mut self, ctx: &egui::Context, documents: &[Document]) {
        let mut ready = Vec::new();
        if let Some(rx) = &self.res_rx {
            while let Ok(result) = rx.try_recv() {
                ready.push(result);
            }
        }
        let live = |id: &DocumentId| documents.iter().any(|doc| doc.id == *id);
        for (id, key, image) in ready {
            self.in_flight.remove(&id);
            if !live(&id) {
                continue;
            }
            match image {
                Some(image) => {
                    let handle = ctx.load_texture(
                        format!("doc_thumb_{}", id.0),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.tex.insert(id, (key, handle));
                }
                None => {
                    self.failed.insert(id, key);
                }
            }
        }
        self.tex.retain(|id, _| live(id));
        self.failed.retain(|id, _| live(id));
        if !self.in_flight.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(60));
        }
    }

    /// The resident preview of `id` (possibly one edit behind while a fresh
    /// one renders).
    pub fn get(&self, id: DocumentId) -> Option<(egui::TextureId, egui::Vec2)> {
        self.tex
            .get(&id)
            .map(|(_, handle)| (handle.id(), handle.size_vec2()))
    }
}

/// Order-independent content key of a tile map: every tile's position and
/// revision plus the map size and `salt`. No pixel reads; never 0.
pub(in crate::app) fn tiles_content_key(
    tiles: &crate::core::tile::TileMap,
    w: u32,
    h: u32,
    salt: u64,
) -> u64 {
    let mut acc = 0u64;
    for (pos, tile) in &tiles.tiles {
        let p = ((pos.x as u32 as u64) << 32) | pos.y as u32 as u64;
        acc = acc.wrapping_add(mix(p ^ mix(tile.revision)));
    }
    let size = ((w as u64) << 32) | h as u64;
    let key = mix(acc ^ mix(size) ^ mix(tiles.tiles.len() as u64 ^ salt.rotate_left(17)));
    key.max(1)
}

fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// What the preview depends on: canvas size, the structural revision and each
/// layer's pixels, mask and compositing state.
fn content_key(doc: &Document) -> u64 {
    let canvas = &doc.canvas;
    let mut key = mix(((canvas.width as u64) << 32) | canvas.height as u64);
    key = mix(key ^ canvas.layer_revision);
    for layer in &canvas.layer_stack.layers {
        let state = (layer.id as u64) << 32
            | (layer.visible as u64) << 31
            | (layer.blend_mode as u64) << 16
            | (layer.opacity.to_bits() >> 16) as u64;
        key = mix(key ^ state);
        key = mix(key ^ (((layer.offset.0 as u32 as u64) << 32) | layer.offset.1 as u32 as u64));
        key = mix(key ^ tiles_content_key(&layer.tiles, layer.width, layer.height, 0));
        if let Some(mask) = &layer.mask {
            let salt = (mask.enabled as u64) << 1 | mask.inverted as u64;
            key = mix(key ^ tiles_content_key(&mask.tiles, mask.width, mask.height, salt));
        }
    }
    key.max(1)
}

/// Preview size: longest side at most `max_dim`, aspect kept, never upscaled.
fn preview_size(w: u32, h: u32, max_dim: u32) -> (u32, u32) {
    let longest = w.max(h).max(1);
    if longest <= max_dim {
        return (w.max(1), h.max(1));
    }
    let scale = max_dim as f64 / longest as f64;
    (
        ((w as f64 * scale).round() as u32).max(1),
        ((h as f64 * scale).round() as u32).max(1),
    )
}

/// Composite one source row per preview row and box-average across each
/// preview pixel's span, over a light checkerboard. Only the sampled rows are
/// ever composited, so a huge document costs a few hundred rows.
fn render_preview(stack: &mut LayerStack, w: u32, h: u32) -> Option<egui::ColorImage> {
    if w == 0 || h == 0 {
        return None;
    }
    let (tw, th) = preview_size(w, h, DOC_THUMB_MAX);
    let mut out = vec![0u8; (tw * th * 4) as usize];
    for ty in 0..th {
        let sy = (((ty as u64 * 2 + 1) * h as u64) / (th as u64 * 2)).min(h as u64 - 1) as u32;
        let row = stack.flatten_band(w, h, sy, 1);
        if row.len() < (w * 4) as usize {
            return None;
        }
        for tx in 0..tw {
            let x0 = (tx as u64 * w as u64 / tw as u64) as u32;
            let x1 = (((tx as u64 + 1) * w as u64 / tw as u64) as u32).clamp(x0 + 1, w);
            let span = x1 - x0;
            let n = span.min(8);
            let mut acc = [0f32; 4];
            for k in 0..n {
                let sx = x0 + (2 * k + 1) * span / (2 * n);
                let i = (sx * 4) as usize;
                let a = row[i + 3] as f32 / 255.0;
                acc[0] += row[i] as f32 * a;
                acc[1] += row[i + 1] as f32 * a;
                acc[2] += row[i + 2] as f32 * a;
                acc[3] += a;
            }
            let inv_n = 1.0 / n as f32;
            let alpha = acc[3] * inv_n;
            let checker = if (tx / CHECKER + ty / CHECKER) % 2 == 0 {
                255.0
            } else {
                226.0
            };
            let o = ((ty * tw + tx) * 4) as usize;
            for c in 0..3 {
                out[o + c] = (acc[c] * inv_n + checker * (1.0 - alpha))
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
            out[o + 3] = 255;
        }
    }
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [tw as usize, th as usize],
        &out,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::Canvas;

    fn solid(w: u32, h: u32, rgba: [u8; 4]) -> Document {
        let canvas = Canvas::from_rgba(rgba.repeat((w * h) as usize), w, h);
        Document::from_canvas(DocumentId(7), canvas, None)
    }

    #[test]
    fn preview_fits_the_longest_side_and_keeps_colour() {
        let mut doc = solid(1200, 300, [200, 40, 10, 255]);
        let image =
            render_preview(&mut doc.canvas.layer_stack, 1200, 300).expect("preview renders");
        assert_eq!(image.size, [256, 64]);
        let px = image.pixels[64 * 20 + 100];
        assert_eq!((px.r(), px.g(), px.b()), (200, 40, 10));
    }

    #[test]
    fn small_documents_are_not_upscaled() {
        assert_eq!(preview_size(90, 40, DOC_THUMB_MAX), (90, 40));
        assert_eq!(preview_size(4000, 6000, DOC_THUMB_MAX), (171, 256));
    }

    #[test]
    fn a_requested_preview_lands_once_and_is_reused() {
        let ctx = egui::Context::default();
        let docs = vec![solid(300, 200, [10, 120, 250, 255])];
        let mut thumbs = DocThumbs::default();
        assert!(thumbs.request(&docs[0]));
        assert!(
            !thumbs.request(&docs[0]),
            "one render in flight per document"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while thumbs.get(docs[0].id).is_none() {
            assert!(std::time::Instant::now() < deadline, "preview never landed");
            std::thread::sleep(std::time::Duration::from_millis(5));
            thumbs.poll(&ctx, &docs);
        }
        assert_eq!(thumbs.get(docs[0].id).unwrap().1, egui::vec2(256.0, 171.0));
        assert!(
            !thumbs.request(&docs[0]),
            "unchanged document is not re-rendered"
        );

        thumbs.poll(&ctx, &[]);
        assert!(
            thumbs.get(docs[0].id).is_none(),
            "closed documents drop their preview"
        );
    }

    #[test]
    fn content_key_follows_edits_but_not_idle_frames() {
        let mut doc = solid(64, 64, [0, 0, 0, 255]);
        let before = content_key(&doc);
        assert_eq!(before, content_key(&doc));
        let idx = doc.canvas.layer_stack.active_idx;
        doc.canvas.layer_stack.layers[idx]
            .tiles
            .set_pixel(5, 5, 255, 255, 255, 255);
        assert_ne!(before, content_key(&doc), "a pixel edit changes the key");
        let edited = content_key(&doc);
        doc.canvas.layer_stack.layers[idx].visible = false;
        assert_ne!(edited, content_key(&doc), "hiding a layer changes the key");
    }
}

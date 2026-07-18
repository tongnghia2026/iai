//! Filter previews: GPU shader path, proxy-resolution CPU jobs and the
//! live canvas preview plumbing. Split out of app/actions.rs.

use crate::app::render::CanvasEvent;
use crate::app::state::App;
use crate::core::filters::FilterType;

const FILTER_PROXY_MAX_DIM: u32 = 512;

fn tile_maps_equal(a: &crate::core::tile::TileMap, b: &crate::core::tile::TileMap) -> bool {
    if a.width != b.width || a.height != b.height || a.tiles.len() != b.tiles.len() {
        return false;
    }
    a.tiles.iter().all(|(pos, tile_a)| {
        b.tiles
            .get(pos)
            .is_some_and(|tile_b| tile_a.pixels == tile_b.pixels)
    })
}

fn build_filter_proxy_source(
    tiles: &crate::core::tile::TileMap,
    layer_w: u32,
    layer_h: u32,
) -> (Vec<u8>, u32, u32, f32) {
    let src_w = layer_w.min(tiles.width);
    let src_h = layer_h.min(tiles.height);
    if src_w == 0 || src_h == 0 {
        return (vec![0, 0, 0, 0], 1, 1, 1.0);
    }

    let longest = src_w.max(src_h);
    let scale = (longest as f32 / FILTER_PROXY_MAX_DIM as f32).max(1.0);
    let proxy_w = ((src_w as f32 / scale).ceil() as u32).max(1);
    let proxy_h = ((src_h as f32 / scale).ceil() as u32).max(1);
    let mut pixels = vec![0u8; (proxy_w * proxy_h * 4) as usize];

    for py in 0..proxy_h {
        let sy =
            (((py as f32 + 0.5) * src_h as f32 / proxy_h as f32).floor() as u32).min(src_h - 1);
        for px in 0..proxy_w {
            let sx =
                (((px as f32 + 0.5) * src_w as f32 / proxy_w as f32).floor() as u32).min(src_w - 1);
            let (r, g, b, a) = tiles.get_pixel(sx, sy);
            let i = ((py * proxy_w + px) * 4) as usize;
            pixels[i] = r;
            pixels[i + 1] = g;
            pixels[i + 2] = b;
            pixels[i + 3] = a;
        }
    }

    (pixels, proxy_w, proxy_h, scale)
}

fn scale_filter_for_proxy(filter: FilterType, scale: f32) -> FilterType {
    let scale = scale.max(1.0);
    match filter {
        FilterType::GaussianBlur { radius } => FilterType::GaussianBlur {
            radius: radius / scale,
        },
        FilterType::Sharpen { amount, radius } => FilterType::Sharpen {
            amount,
            radius: radius / scale,
        },
        FilterType::HighPass { radius } => FilterType::HighPass {
            radius: radius / scale,
        },
        FilterType::Pixelate { cell } => FilterType::Pixelate {
            cell: (cell / scale).max(1.0),
        },
        other => other,
    }
}

fn filter_canvas_preview_prefers_cpu(filter: FilterType) -> bool {
    matches!(
        filter,
        FilterType::GaussianBlur { .. }
            | FilterType::Sharpen { .. }
            | FilterType::HighPass { .. }
            | FilterType::ReduceNoise { .. }
    )
}
impl App {
    pub(crate) fn begin_filter_preview(
        &mut self,
        filter: crate::core::filters::FilterType,
    ) -> bool {
        self.cancel_filter_preview();
        self.shell.ui.show_adjustment_dialog = false;
        self.cancel_adjustment_preview();
        self.cancel_adjustment_layer_edit();
        self.abandon_develop_session();

        let doc_id = self.docs.documents[self.docs.active_doc_idx].id;
        let preview = {
            let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
            canvas.layer_stack.normalize_active_idx();
            if canvas.layer_stack.layers.is_empty() {
                return false;
            }
            let idx = canvas.layer_stack.active_idx;
            let layer = &canvas.layer_stack.layers[idx];
            if (!layer.is_background && layer.locked) || !layer.is_raster() {
                return false;
            }
            let original_tiles = layer.tiles.clone();
            let (proxy_original, proxy_w, proxy_h, proxy_scale) =
                build_filter_proxy_source(&original_tiles, layer.width, layer.height);
            let proxy_original_preview =
                std::sync::Arc::new(egui::ColorImage::from_rgba_unmultiplied(
                    [proxy_w as usize, proxy_h as usize],
                    &proxy_original,
                ));
            crate::app::state::FilterPreviewSession {
                doc_id,
                layer_id: layer.id,
                original_tiles,
                job_id: 0,
                processing: false,
                gpu_preview_active: false,
                cpu_preview_active: false,
                pending_filter: None,
                last_preview_filter: None,
                rx: None,
                proxy_original,
                proxy_original_preview,
                proxy_w,
                proxy_h,
                proxy_scale,
                proxy_preview: None,
                proxy_filter: None,
            }
        };

        self.shell.filter_preview = Some(preview);
        self.shell.ui.filter_dialog = filter;
        // Canvas (full-image) preview is OFF by default — on large images it can
        // lag. The dialog's downscaled thumbnail always updates regardless; the
        // user opts into the live canvas preview via the "Preview" checkbox.
        self.shell.ui.filter_preview_enabled = false;
        self.shell.ui.show_filter_dialog = true;
        self.update_filter_proxy_preview(filter);
        self.update_filter_canvas_preview(filter);
        true
    }

    pub(super) fn clear_filter_gpu_preview(&mut self) {
        if let Some(gpu) = &mut self.win.gpu {
            gpu.compositor.preview_filter = None;
        }
    }

    pub(super) fn apply_filter_preview_gpu(
        &mut self,
        filter: crate::core::filters::FilterType,
    ) -> bool {
        if self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .selection
            .active
        {
            self.clear_filter_gpu_preview();
            return false;
        }
        if self.win.gpu.is_none() {
            return false;
        }
        let (layer_id, original_tiles, needs_restore) = {
            let Some(preview) = self.shell.filter_preview.as_mut() else {
                return false;
            };
            if self.docs.documents[self.docs.active_doc_idx].id != preview.doc_id {
                return false;
            }
            preview.job_id = preview.job_id.wrapping_add(1);
            preview.processing = false;
            preview.pending_filter = None;
            preview.rx = None;
            preview.last_preview_filter = Some(filter);
            preview.gpu_preview_active = true;
            let needs_restore = preview.cpu_preview_active;
            preview.cpu_preview_active = false;
            (
                preview.layer_id,
                preview.original_tiles.clone(),
                needs_restore,
            )
        };
        if let Some(gpu) = &mut self.win.gpu {
            gpu.compositor.preview_filter =
                Some(crate::gpu::compositor::FilterGpuPreview { layer_id, filter });
        }
        if needs_restore {
            let restored = self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .preview_layer_tiles(layer_id, original_tiles);
            if restored {
                self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
            }
        }
        self.recomposite_visible();
        true
    }

    pub(super) fn update_filter_canvas_preview(
        &mut self,
        filter: crate::core::filters::FilterType,
    ) -> bool {
        if !self.shell.ui.filter_preview_enabled {
            return false;
        }
        if self.win.gpu.is_some()
            && !self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .selection
                .active
            && !filter_canvas_preview_prefers_cpu(filter)
        {
            self.apply_filter_preview_gpu(filter)
        } else {
            self.spawn_filter_preview_job(filter)
        }
    }

    pub(super) fn set_filter_preview_enabled(&mut self, enabled: bool) -> bool {
        self.shell.ui.filter_preview_enabled = enabled;
        if enabled {
            let filter = self.shell.ui.filter_dialog;
            let proxy_changed = self.update_filter_proxy_preview(filter);
            let canvas_changed = self.update_filter_canvas_preview(filter);
            if (proxy_changed || canvas_changed) && self.win.window.is_some() {
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            return true;
        }

        self.clear_filter_gpu_preview();
        let Some(preview) = self.shell.filter_preview.as_mut() else {
            return false;
        };
        preview.job_id = preview.job_id.wrapping_add(1);
        preview.processing = false;
        preview.pending_filter = None;
        preview.rx = None;
        preview.gpu_preview_active = false;
        preview.cpu_preview_active = false;
        preview.last_preview_filter = None;

        if self.docs.documents[self.docs.active_doc_idx].id == preview.doc_id {
            let restored = self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .preview_layer_tiles(preview.layer_id, preview.original_tiles.clone());
            if restored {
                self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
            }
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        true
    }

    pub(super) fn spawn_filter_preview_job(
        &mut self,
        filter: crate::core::filters::FilterType,
    ) -> bool {
        let had_gpu_preview = self
            .shell
            .filter_preview
            .as_ref()
            .is_some_and(|preview| preview.gpu_preview_active);
        self.clear_filter_gpu_preview();
        if had_gpu_preview {
            self.recomposite_visible();
        }
        let Some(preview) = &mut self.shell.filter_preview else {
            return false;
        };
        if self.docs.documents[self.docs.active_doc_idx].id != preview.doc_id {
            return false;
        }
        if preview.cpu_preview_active
            && !preview.processing
            && preview.pending_filter.is_none()
            && preview.last_preview_filter == Some(filter)
        {
            return true;
        }
        if preview.processing {
            preview.pending_filter = Some(filter);
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return true;
        }

        preview.job_id = preview.job_id.wrapping_add(1);
        preview.pending_filter = None;
        preview.last_preview_filter = Some(filter);
        preview.gpu_preview_active = false;

        let layer_id = preview.layer_id;
        let job_id = preview.job_id;
        let source_tiles = preview.original_tiles.clone();
        let (layer_w, layer_h, ox, oy, selection, canvas_w, canvas_h) = {
            let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
            let Some(layer) = canvas.layer_stack.layers.iter().find(|l| l.id == layer_id) else {
                return false;
            };
            (
                layer.width,
                layer.height,
                layer.offset.0,
                layer.offset.1,
                canvas.selection.clone(),
                canvas.width,
                canvas.height,
            )
        };
        let (tx, rx) = std::sync::mpsc::channel();
        preview.processing = true;
        preview.rx = Some(rx);

        rayon::spawn(move || {
            let tiles = crate::core::canvas::Canvas::build_filtered_layer_tiles_with_selection(
                &source_tiles,
                layer_w,
                layer_h,
                ox,
                oy,
                &filter,
                &selection,
                canvas_w,
                canvas_h,
            );
            let _ = tx.send(crate::app::state::FilterPreviewResult {
                job_id,
                filter,
                tiles,
            });
        });

        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        true
    }

    pub(super) fn poll_filter_preview(&mut self) {
        let result = {
            let Some(preview) = &mut self.shell.filter_preview else {
                return;
            };
            let Some(rx) = preview.rx.take() else {
                return;
            };
            match rx.try_recv() {
                Ok(result) => {
                    preview.processing = false;
                    Some(result)
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    preview.rx = Some(rx);
                    None
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    preview.processing = false;
                    None
                }
            }
        };

        let Some(result) = result else {
            return;
        };

        let mut pending = None;
        let mut applied = false;
        if let Some(preview) = &mut self.shell.filter_preview {
            if result.job_id == preview.job_id
                && self.docs.documents[self.docs.active_doc_idx].id == preview.doc_id
            {
                let layer_id = preview.layer_id;
                let restored = self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .preview_layer_tiles(layer_id, result.tiles);
                if restored {
                    preview.last_preview_filter = Some(result.filter);
                    preview.cpu_preview_active = true;
                    preview.gpu_preview_active = false;
                    applied = true;
                }
                pending = preview.pending_filter.take();
            }
        }

        if applied {
            self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if let Some(filter) = pending {
            self.spawn_filter_preview_job(filter);
        }
    }

    pub(crate) fn update_filter_proxy_preview(
        &mut self,
        filter: crate::core::filters::FilterType,
    ) -> bool {
        let Some(preview) = self.shell.filter_preview.as_mut() else {
            return false;
        };
        if preview.proxy_filter == Some(filter) && preview.proxy_preview.is_some() {
            return false;
        }
        if preview.proxy_original.is_empty() || preview.proxy_w == 0 || preview.proxy_h == 0 {
            return false;
        }

        let mut pixels = preview.proxy_original.clone();
        let proxy_filter = scale_filter_for_proxy(filter, preview.proxy_scale);
        proxy_filter.apply(&mut pixels, preview.proxy_w, preview.proxy_h);
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [preview.proxy_w as usize, preview.proxy_h as usize],
            &pixels,
        );
        preview.proxy_preview = Some(std::sync::Arc::new(image));
        preview.proxy_filter = Some(filter);
        true
    }

    pub(crate) fn commit_filter_preview(
        &mut self,
        filter: &crate::core::filters::FilterType,
    ) -> bool {
        self.clear_filter_gpu_preview();
        let Some(preview) = self.shell.filter_preview.take() else {
            return false;
        };
        if self.docs.documents[self.docs.active_doc_idx].id != preview.doc_id {
            return false;
        }

        let Some((layer_w, layer_h, ox, oy, selection, canvas_w, canvas_h, current_tiles)) = ({
            let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
            canvas
                .layer_stack
                .layers
                .iter()
                .find(|l| l.id == preview.layer_id)
                .map(|layer| {
                    (
                        layer.width,
                        layer.height,
                        layer.offset.0,
                        layer.offset.1,
                        canvas.selection.clone(),
                        canvas.width,
                        canvas.height,
                        layer.tiles.clone(),
                    )
                })
        }) else {
            return false;
        };

        let preview_matches_filter = preview.cpu_preview_active
            && !preview.processing
            && preview.pending_filter.is_none()
            && preview.last_preview_filter == Some(*filter);
        let after_tiles = if preview_matches_filter {
            current_tiles
        } else {
            crate::core::canvas::Canvas::build_filtered_layer_tiles_with_selection(
                &preview.original_tiles,
                layer_w,
                layer_h,
                ox,
                oy,
                filter,
                &selection,
                canvas_w,
                canvas_h,
            )
        };
        let changed = !tile_maps_equal(&after_tiles, &preview.original_tiles);
        let ok = {
            let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
            if changed {
                canvas.commit_layer_tiles_change(
                    preview.layer_id,
                    preview.original_tiles,
                    after_tiles,
                    filter.name(),
                )
            } else {
                canvas.restore_layer_tiles(preview.layer_id, preview.original_tiles)
            }
        };

        if ok {
            self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
        }
        ok && changed
    }

    pub(crate) fn cancel_filter_preview(&mut self) {
        self.clear_filter_gpu_preview();
        let Some(preview) = self.shell.filter_preview.take() else {
            return;
        };
        if self.docs.documents[self.docs.active_doc_idx].id == preview.doc_id {
            let layer_id = preview.layer_id;
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .restore_layer_tiles(layer_id, preview.original_tiles);
            self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
    }
}

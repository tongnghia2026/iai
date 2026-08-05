//! Per-frame UiData snapshot for the egui layer: collect_ui_data plus the
//! print/clone/layer/mask thumbnail builders it uses. Split out of
//! actions.rs (phase 3).

use crate::app::state::App;
use crate::ui::{
    AiViewModel, ChannelsViewModel, ChromeViewModel, DevelopViewModel, DialogViewModel,
    DocumentViewModel, LayerViewModel, PrintViewModel, SelectionViewModel, ToolViewModel,
    TransformOverlayData, UiData,
};

const PRINT_PREVIEW_MAX_DIM: u32 = 768;
const PANEL_THUMB: usize = 64;
const CHANNEL_THUMB: usize = PANEL_THUMB;

fn web_ai_label(site: &str) -> &'static str {
    match site {
        "chatgpt" => "ChatGPT Web",
        "gemini" => "Gemini Web",
        _ => "AI Web",
    }
}

fn bilinear_rgba_sample<F>(x: f32, y: f32, mut sample: F) -> [u8; 4]
where
    F: FnMut(i32, i32) -> [u8; 4],
{
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;

    let c00 = sample(x0, y0);
    let c10 = sample(x0 + 1, y0);
    let c01 = sample(x0, y0 + 1);
    let c11 = sample(x0 + 1, y0 + 1);

    let weights = [
        (1.0 - tx) * (1.0 - ty),
        tx * (1.0 - ty),
        (1.0 - tx) * ty,
        tx * ty,
    ];
    let colors = [c00, c10, c01, c11];
    let mut alpha = 0.0_f32;
    let mut premul = [0.0_f32; 3];
    for (color, weight) in colors.iter().zip(weights) {
        let a = color[3] as f32 / 255.0;
        alpha += a * weight;
        premul[0] += color[0] as f32 * a * weight;
        premul[1] += color[1] as f32 * a * weight;
        premul[2] += color[2] as f32 * a * weight;
    }

    if alpha <= f32::EPSILON {
        return [0, 0, 0, 0];
    }

    [
        (premul[0] / alpha).round().clamp(0.0, 255.0) as u8,
        (premul[1] / alpha).round().clamp(0.0, 255.0) as u8,
        (premul[2] / alpha).round().clamp(0.0, 255.0) as u8,
        (alpha * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

impl App {
    fn build_print_preview_thumbnail(&mut self) -> Option<std::sync::Arc<egui::ColorImage>> {
        if !self.shell.ui.show_print_dialog {
            return None;
        }

        let doc_id = self.docs.documents[self.docs.active_doc_idx].id.0;
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let layer_revision = canvas.layer_revision;
        let (w, h) = (canvas.width, canvas.height);

        if self.shell.ui_data_cache.print_preview_doc_id == doc_id
            && self.shell.ui_data_cache.print_preview_layer_revision == layer_revision
            && self.shell.ui_data_cache.print_preview_w == w
            && self.shell.ui_data_cache.print_preview_h == h
        {
            return self.shell.ui_data_cache.print_preview_image.clone();
        }

        if w == 0 || h == 0 {
            return None;
        }

        let longest = w.max(h);
        let scale = (longest as f32 / PRINT_PREVIEW_MAX_DIM as f32).max(1.0);
        let thumb_w = ((w as f32 / scale).ceil() as u32).max(1);
        let thumb_h = ((h as f32 / scale).ceil() as u32).max(1);
        let mut out = vec![255_u8; (thumb_w * thumb_h * 4) as usize];

        // Nearest-sample onto white from one source row per thumbnail row. Small
        // canvases read the cached flat pixels; past the flat-buffer cap each
        // sampled row is composited on demand (Viewport Streaming), so the print
        // dialog previews any size.
        let mut sample_rows: Box<dyn FnMut(u32) -> Option<Vec<u8>> + '_> =
            if crate::core::canvas::Canvas::fits_flat_buffer(w, h) {
                let expected_len = crate::core::canvas::Canvas::checked_rgba_len(w, h)?;
                let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
                canvas.ensure_pixels();
                if canvas.pixels_stale || canvas.pixels.len() < expected_len {
                    return None;
                }
                let pixels = &canvas.pixels;
                Box::new(move |sy: u32| {
                    let start = ((sy * w) * 4) as usize;
                    Some(pixels[start..start + (w * 4) as usize].to_vec())
                })
            } else {
                let mut stack = self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .clone();
                Box::new(move |sy: u32| {
                    let band = stack.flatten_band(w, h, sy, 1);
                    (band.len() >= (w * 4) as usize).then_some(band)
                })
            };

        for ty in 0..thumb_h {
            let sy = (((ty as f32 + 0.5) * h as f32 / thumb_h as f32).floor() as u32).min(h - 1);
            let row = sample_rows(sy)?;
            for tx in 0..thumb_w {
                let sx =
                    (((tx as f32 + 0.5) * w as f32 / thumb_w as f32).floor() as u32).min(w - 1);
                let src = (sx * 4) as usize;
                let dst = ((ty * thumb_w + tx) * 4) as usize;
                let a = row[src + 3] as u16;
                let inv = 255 - a;
                out[dst] = ((row[src] as u16 * a + 255 * inv) / 255) as u8;
                out[dst + 1] = ((row[src + 1] as u16 * a + 255 * inv) / 255) as u8;
                out[dst + 2] = ((row[src + 2] as u16 * a + 255 * inv) / 255) as u8;
                out[dst + 3] = 255;
            }
        }
        drop(sample_rows);

        let image = std::sync::Arc::new(egui::ColorImage::from_rgba_unmultiplied(
            [thumb_w as usize, thumb_h as usize],
            &out,
        ));
        self.shell.ui_data_cache.print_preview_doc_id = doc_id;
        self.shell.ui_data_cache.print_preview_layer_revision = layer_revision;
        self.shell.ui_data_cache.print_preview_w = w;
        self.shell.ui_data_cache.print_preview_h = h;
        self.shell.ui_data_cache.print_preview_image = Some(image.clone());
        Some(image)
    }

    fn build_clone_source_thumbnail(
        &mut self,
    ) -> Option<std::sync::Arc<crate::ui::CloneSourcePreview>> {
        if !matches!(
            self.edit.tools.active_id(),
            crate::tools::ToolId::Clone | crate::tools::ToolId::Repair
        ) {
            return None;
        }
        if self.edit.input.alt_held
            || self.edit.input.alt_right_dragging
            || self.edit.input.was_over_ui
        {
            return None;
        }

        let zoom = self.edit.view.zoom.max(0.0001);
        let dst_x = (self.edit.input.mouse_x - self.edit.view.offset_x) / zoom;
        let dst_y = (self.edit.input.mouse_y - self.edit.view.offset_y) / zoom;
        let canvas_w = self.docs.documents[self.docs.active_doc_idx].canvas.width as f32;
        let canvas_h = self.docs.documents[self.docs.active_doc_idx].canvas.height as f32;
        if dst_x < 0.0 || dst_y < 0.0 || dst_x >= canvas_w || dst_y >= canvas_h {
            return None;
        }

        let (source_x, source_y, brush_size, hardness, opacity, sample_merged) = {
            let tool = self.edit.tools.clone_like();
            let (source_x, source_y) = tool.preview_source_center(dst_x, dst_y)?;
            (
                source_x,
                source_y,
                tool.size,
                tool.hardness,
                tool.opacity,
                tool.sample_merged,
            )
        };

        let radius = brush_size.max(0.5);
        let diameter = radius * 2.0;
        let preview_size =
            (diameter.round() as usize).clamp(1, crate::ui::CLONE_SOURCE_PREVIEW_MAX_SIZE);
        let sample_scale = diameter / preview_size as f32;
        let hard_r = (radius * hardness.clamp(0.0, 1.0)).min(radius);
        let feather = (radius - hard_r).max(0.001);
        let opacity = opacity.clamp(0.0, 1.0);
        let mut out = vec![0_u8; preview_size * preview_size * 4];
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        if canvas.width == 0 || canvas.height == 0 {
            return None;
        }

        if sample_merged {
            canvas.ensure_pixels();
        } else if canvas.layer_stack.layers.is_empty() {
            return None;
        }

        for y in 0..preview_size {
            for x in 0..preview_size {
                let dx = (x as f32 + 0.5) * sample_scale - radius;
                let dy = (y as f32 + 0.5) * sample_scale - radius;
                let d2 = dx * dx + dy * dy;
                let coverage = if d2 > radius * radius {
                    0.0
                } else {
                    let d = d2.sqrt();
                    if d <= hard_r {
                        1.0
                    } else {
                        let t = ((d - hard_r) / feather).clamp(0.0, 1.0);
                        1.0 - t * t * (3.0 - 2.0 * t)
                    }
                };
                let sx = source_x + dx;
                let sy = source_y + dy;
                let rgba = if sample_merged {
                    bilinear_rgba_sample(sx, sy, |px, py| {
                        if px < 0 || py < 0 {
                            return [0, 0, 0, 0];
                        }
                        let px = px as u32;
                        let py = py as u32;
                        if px >= canvas.width || py >= canvas.height {
                            [0, 0, 0, 0]
                        } else {
                            let i = ((py * canvas.width + px) * 4) as usize;
                            if i + 3 < canvas.pixels.len() {
                                [
                                    canvas.pixels[i],
                                    canvas.pixels[i + 1],
                                    canvas.pixels[i + 2],
                                    canvas.pixels[i + 3],
                                ]
                            } else {
                                [0, 0, 0, 0]
                            }
                        }
                    })
                } else {
                    let idx = canvas
                        .layer_stack
                        .active_idx
                        .min(canvas.layer_stack.layers.len().saturating_sub(1));
                    let layer = &canvas.layer_stack.layers[idx];
                    if let Some(tiles) = layer.get_paint_tiles() {
                        bilinear_rgba_sample(sx, sy, |px, py| {
                            let lx = px - layer.offset.0;
                            let ly = py - layer.offset.1;
                            if lx < 0 || ly < 0 {
                                [0, 0, 0, 0]
                            } else {
                                let (r, g, b, a) = tiles.get_pixel(lx as u32, ly as u32);
                                [r, g, b, a]
                            }
                        })
                    } else {
                        [0, 0, 0, 0]
                    }
                };

                let i = (y * preview_size + x) * 4;
                out[i] = rgba[0];
                out[i + 1] = rgba[1];
                out[i + 2] = rgba[2];
                out[i + 3] = ((rgba[3] as f32 * coverage * opacity)
                    .round()
                    .clamp(0.0, 255.0)) as u8;
            }
        }

        Some(std::sync::Arc::new(crate::ui::CloneSourcePreview {
            width: preview_size,
            height: preview_size,
            pixels: out,
        }))
    }

    pub fn collect_ui_data(&mut self) -> UiData {
        self.poll_printer_refresh();

        // Welcome screen: recent files + their thumbnails (uploads ready
        // thumbnails and requests missing ones on the main egui context). Only
        // while the welcome screen shows, so the editor path pays nothing.
        let welcome_recent = if self.shell.ui.show_welcome {
            self.welcome_recent_view()
        } else {
            Vec::new()
        };

        // Library grid: the chosen folder's images + their thumbnails. Built only
        // while the Library browser shows, so the editor path pays nothing.
        let library = if self.shell.ui.show_library {
            self.library_grid_view()
        } else {
            crate::ui::LibraryViewModel::default()
        };

        // Reveal the active layer in the Layers panel when the selection changes:
        // expand its collapsed ancestor folders and flag a scroll-to-active for
        // this frame. Detected centrally so every selection path (panel click,
        // canvas auto-select, scripted) triggers it. Keyed on the layer id (not
        // index) so a reorder that shifts indices doesn't spuriously re-reveal.
        let (doc_id_reveal, active_id, active_idx_reveal) = {
            let d = &self.docs.documents[self.docs.active_doc_idx];
            let ls = &d.canvas.layer_stack;
            (
                d.id.0,
                ls.layers.get(ls.active_idx).map(|l| l.id),
                ls.active_idx,
            )
        };
        let reveal_key = active_id.map(|id| (doc_id_reveal, id));
        let scroll_layers_to_active =
            if reveal_key.is_some() && reveal_key != self.shell.ui_data_cache.reveal_last_active {
                self.shell.ui_data_cache.reveal_last_active = reveal_key;
                if self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .expand_collapsed_ancestors(active_idx_reveal)
                {
                    // The row was hidden under a collapsed folder; bump so the
                    // layer-cache block below rebuilds with it expanded/visible.
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_revision += 1;
                }
                true
            } else {
                false
            };

        // The command history bumps its own revision on every stack mutation,
        // so this key can't go stale (the old hand-bumped canvas counter was
        // missed by several push sites — crop among them — freezing the panel).
        let history_rev = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .history_revision();
        let layer_rev = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_revision;
        let doc_id = self.docs.documents[self.docs.active_doc_idx].id.0;
        let force_layer_thumb_refresh = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .pending_stroke
            .is_some();

        if self.shell.ui_data_cache.history_revision != history_rev
            || self.shell.ui_data_cache.ui_cache_doc_id != doc_id
        {
            self.shell.ui_data_cache.history_entries = std::sync::Arc::new(
                self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .history_entries(),
            );
            self.shell.ui_data_cache.history_revision = history_rev;
        }

        if self.shell.ui_data_cache.layer_revision != layer_rev
            || self.shell.ui_data_cache.ui_cache_doc_id != doc_id
            || force_layer_thumb_refresh
        {
            let layers = &self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack
                .layers;
            self.shell.ui_data_cache.layer_names =
                std::sync::Arc::new(layers.iter().map(|l| l.name.clone()).collect());
            self.shell.ui_data_cache.layer_visibles =
                std::sync::Arc::new(layers.iter().map(|l| l.visible).collect());
            self.shell.ui_data_cache.layer_opacities =
                std::sync::Arc::new(layers.iter().map(|l| l.opacity).collect());
            self.shell.ui_data_cache.layer_blend_modes =
                std::sync::Arc::new(layers.iter().map(|l| l.blend_mode).collect());
            self.shell.ui_data_cache.layer_locked =
                std::sync::Arc::new(layers.iter().map(|l| l.locked).collect());
            // A clipped layer's mask IS the managed clip — hide it from the panel
            // so it reads as a plain linked layer (Photoshop shows no mask there).
            self.shell.ui_data_cache.layer_has_mask = std::sync::Arc::new(
                layers
                    .iter()
                    .map(|l| l.mask.is_some() && l.clip_parent_id.is_none())
                    .collect(),
            );
            self.shell.ui_data_cache.layer_is_clipped =
                std::sync::Arc::new(layers.iter().map(|l| l.clip_parent_id.is_some()).collect());
            {
                let base_ids: std::collections::HashSet<u32> =
                    layers.iter().filter_map(|l| l.clip_parent_id).collect();
                self.shell.ui_data_cache.layer_is_clip_base =
                    std::sync::Arc::new(layers.iter().map(|l| base_ids.contains(&l.id)).collect());
            }
            self.shell.ui_data_cache.layer_mask_enabled = std::sync::Arc::new(
                layers
                    .iter()
                    .map(|l| l.mask.as_ref().map(|m| m.enabled).unwrap_or(false))
                    .collect(),
            );
            self.shell.ui_data_cache.layer_paint_targets =
                std::sync::Arc::new(layers.iter().map(|l| l.paint_target).collect());
            self.shell.ui_data_cache.layer_mask_linked =
                std::sync::Arc::new(layers.iter().map(|l| l.mask_linked).collect());
            self.shell.ui_data_cache.layer_types = std::sync::Arc::new(
                layers
                    .iter()
                    .map(|layer| layer_ui_type(&layer.layer_type).to_string())
                    .collect(),
            );
            self.shell.ui_data_cache.layer_is_background =
                std::sync::Arc::new(layers.iter().map(|l| l.is_background).collect());
            self.shell.ui_data_cache.layer_lock_alpha =
                std::sync::Arc::new(layers.iter().map(|l| l.lock_alpha).collect());
            self.shell.ui_data_cache.layer_selected =
                std::sync::Arc::new(layers.iter().map(|l| l.selected).collect());
            self.shell.ui_data_cache.layer_thumbnails = std::sync::Arc::new(
                layers
                    .iter()
                    .map(build_layer_thumbnail_rgba)
                    .collect::<Vec<_>>(),
            );
            self.shell.ui_data_cache.layer_mask_thumbnails = std::sync::Arc::new(
                layers
                    .iter()
                    .map(build_layer_mask_thumbnail_rgba)
                    .collect::<Vec<_>>(),
            );
            let ls = &self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack;
            self.shell.ui_data_cache.layer_depths =
                std::sync::Arc::new((0..ls.layers.len()).map(|i| ls.depth_of(i)).collect());
            self.shell.ui_data_cache.layer_expanded =
                std::sync::Arc::new(ls.layers.iter().map(|l| l.expanded).collect());
            self.shell.ui_data_cache.layer_collapsed_hidden = std::sync::Arc::new(
                (0..ls.layers.len())
                    .map(|i| ls.is_collapsed_hidden(i))
                    .collect(),
            );
            self.shell.ui_data_cache.layer_revision = layer_rev;
            self.shell.ui_data_cache.ui_cache_doc_id = doc_id;
        }

        // Channels panel thumbnails. The colour plates sample the flat
        // composite buffer, which may need a full CPU flatten — so they only
        // rebuild while the panel is open, at most every few hundred ms, and
        // never during an in-flight stroke (revision is bumped on commit).
        // Docs past the flat-buffer cap get no colour plates (rows still work).
        if self.shell.ui.show_channels_panel {
            const CHANNEL_THUMB_MIN_INTERVAL: std::time::Duration =
                std::time::Duration::from_millis(600);
            let doc_changed = self.shell.ui_data_cache.channel_thumbs_doc_id != doc_id;
            let needs_color =
                doc_changed || self.shell.ui_data_cache.channel_thumbs_layer_revision != layer_rev;
            let throttled = !doc_changed
                && self
                    .shell
                    .ui_data_cache
                    .channel_thumbs_built_at
                    .is_some_and(|t| t.elapsed() < CHANNEL_THUMB_MIN_INTERVAL);
            if needs_color && !throttled {
                let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
                canvas.ensure_pixels();
                let (w, h) = (canvas.width, canvas.height);
                if !canvas.pixels_stale
                    && crate::core::canvas::Canvas::checked_rgba_len(w, h)
                        .is_some_and(|len| canvas.pixels.len() >= len)
                {
                    self.shell.ui_data_cache.channel_thumbnails =
                        std::sync::Arc::new(if canvas.is_cmyk() {
                            build_channel_thumbnails_cmyk(canvas)
                        } else {
                            build_channel_thumbnails_rgba(&canvas.pixels, w, h)
                        });
                } else if doc_changed {
                    self.shell.ui_data_cache.channel_thumbnails = std::sync::Arc::new(Vec::new());
                }
                self.shell.ui_data_cache.channel_thumbs_layer_revision = layer_rev;
                self.shell.ui_data_cache.channel_thumbs_built_at = Some(std::time::Instant::now());
            }

            let channels = &self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .channels;
            let alpha_key = channels.alpha.iter().enumerate().fold(0u64, |acc, (i, a)| {
                acc.rotate_left(9) ^ ((a.id as u64) << 24) ^ a.revision ^ (i as u64)
            }) ^ ((channels.alpha.len() as u64) << 48);
            if doc_changed || self.shell.ui_data_cache.channel_thumbs_alpha_key != alpha_key {
                self.shell.ui_data_cache.alpha_thumbnails = std::sync::Arc::new(
                    channels
                        .alpha
                        .iter()
                        .map(build_alpha_thumbnail_rgba)
                        .collect(),
                );
                self.shell.ui_data_cache.alpha_channel_names = std::sync::Arc::new(
                    channels
                        .alpha
                        .iter()
                        .map(|a| (a.id, a.name.clone()))
                        .collect(),
                );
                self.shell.ui_data_cache.channel_thumbs_alpha_key = alpha_key;
            }
            self.shell.ui_data_cache.channel_thumbs_doc_id = doc_id;
        }

        let (
            brush_size,
            brush_hardness,
            brush_opacity,
            brush_spacing,
            brush_flow,
            brush_smoothing,
            brush_preset_idx,
        ) = if self.edit.tools.active_id() == crate::tools::ToolId::Eraser {
            let e = self.edit.tools.eraser();
            (
                e.size,
                e.hardness,
                e.opacity,
                e.spacing,
                e.flow,
                e.smoothing,
                e.preset_idx,
            )
        } else {
            let b = self.edit.tools.brush();
            (
                b.settings.size,
                b.settings.hardness,
                b.settings.opacity,
                b.settings.spacing,
                b.settings.flow,
                b.settings.smoothing,
                b.preset_idx,
            )
        };

        let clone_source_thumbnail = self.build_clone_source_thumbnail();
        let print_preview_image = self.build_print_preview_thumbnail();
        let doc_ai_busy = std::sync::Arc::new(
            self.docs
                .documents
                .iter()
                .map(|doc| {
                    let doc_id = doc.id.0;
                    if let Some(job) = self.jobs.ai_engine.job_for_doc(doc_id) {
                        let label = match job.provider {
                            crate::core::ai::AiProvider::Gemini => "Gemini API",
                            crate::core::ai::AiProvider::OpenAi => "OpenAI API",
                        };
                        return Some(crate::ui::DocAiBusy {
                            label: label.to_string(),
                            elapsed_secs: Some(job.started.elapsed().as_secs()),
                            queue_pos: None,
                        });
                    }
                    if self
                        .jobs
                        .ext
                        .origin
                        .is_some_and(|origin| origin.doc_id == doc_id)
                    {
                        let site = self.jobs.ext.awaiting_site.as_deref().unwrap_or("web");
                        return Some(crate::ui::DocAiBusy {
                            label: web_ai_label(site).to_string(),
                            elapsed_secs: self
                                .jobs
                                .ext
                                .awaiting_started()
                                .map(|started| started.elapsed().as_secs()),
                            queue_pos: None,
                        });
                    }
                    self.jobs
                        .ext
                        .queued_for_doc(doc_id)
                        .map(|job| crate::ui::DocAiBusy {
                            label: web_ai_label(&job.site).to_string(),
                            elapsed_secs: None,
                            queue_pos: self.jobs.ext.queued_pos(doc_id),
                        })
                })
                .collect(),
        );

        UiData {
            doc: DocumentViewModel {
                canvas_w: self.docs.documents[self.docs.active_doc_idx].canvas.width,
                canvas_h: self.docs.documents[self.docs.active_doc_idx].canvas.height,
                canvas_dpi: self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .metadata
                    .resolution_ppi,
                canvas_unit: self.shell.canvas_unit,
                doc_profile_name: {
                    let p = &self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .icc_profile;
                    if p.name.is_empty() {
                        "sRGB IEC61966-2.1".to_string()
                    } else {
                        p.name.clone()
                    }
                },
                canvas_bit_depth: match self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .bit_depth
                {
                    crate::core::canvas::BitDepth::Sixteen => 16,
                    _ => 8,
                },
                is_cmyk: self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .is_cmyk(),
                cmyk_profile_name: match &self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .color_mode
                {
                    crate::core::canvas::ColorMode::Cmyk(p) => p.display_name().to_string(),
                    crate::core::canvas::ColorMode::Rgb => String::new(),
                },
                export_embed_icc: self.shell.ui.export_embed_icc,
                swatches: std::sync::Arc::new(
                    self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .metadata
                        .swatches
                        .clone(),
                ),
                zoom: self.edit.view.zoom,
                offset_x: self.edit.view.offset_x,
                offset_y: self.edit.view.offset_y,
                undo_count: self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .undo_count(),
                redo_count: self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .redo_count(),
                history_entries: self.shell.ui_data_cache.history_entries.clone(),
                current_file: self
                    .docs
                    .current_file
                    .as_ref()
                    .and_then(|p| p.to_str())
                    .map(|s| s.to_string()),
                is_modified: self.is_modified(),
                has_doc: !self.has_only_welcome_placeholder(),
                pdf_nav: self
                    .docs
                    .documents
                    .get(self.docs.active_doc_idx)
                    .and_then(|doc| {
                        let page = doc.pdf_page.as_ref()?;
                        let (index, count) =
                            doc.pdf_document
                                .as_ref()
                                .map_or((page.index, page.count), |pdf| {
                                    (
                                        pdf.selected_pages
                                            .iter()
                                            .position(|&index| index == pdf.active_page)
                                            .unwrap_or(0),
                                        pdf.selected_pages.len(),
                                    )
                                });
                        Some(crate::ui::PdfNavData {
                            index,
                            count,
                            source_name: page
                                .source
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("PDF")
                                .to_string(),
                        })
                    }),
                doc_count: self.docs.documents.len(),
                active_doc_idx: self.docs.active_doc_idx,
                doc_titles: std::sync::Arc::new(crate::core::document::disambiguated_tab_titles(
                    &self.docs.documents,
                )),
                doc_modified: std::sync::Arc::new(
                    self.docs
                        .documents
                        .iter()
                        .map(|d| d.is_modified())
                        .collect(),
                ),
                doc_ai_busy,
            },
            layers: LayerViewModel {
                layer_count: self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers
                    .len(),
                active_layer_idx: self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .active_idx,
                layer_names: self.shell.ui_data_cache.layer_names.clone(),
                layer_visibles: self.shell.ui_data_cache.layer_visibles.clone(),
                layer_opacities: self.shell.ui_data_cache.layer_opacities.clone(),
                layer_blend_modes: self.shell.ui_data_cache.layer_blend_modes.clone(),
                layer_locked: self.shell.ui_data_cache.layer_locked.clone(),
                layer_has_mask: self.shell.ui_data_cache.layer_has_mask.clone(),
                layer_mask_enabled: self.shell.ui_data_cache.layer_mask_enabled.clone(),
                layer_paint_targets: self.shell.ui_data_cache.layer_paint_targets.clone(),
                layer_mask_linked: self.shell.ui_data_cache.layer_mask_linked.clone(),
                layer_types: self.shell.ui_data_cache.layer_types.clone(),
                layer_is_background: self.shell.ui_data_cache.layer_is_background.clone(),
                layer_lock_alpha: self.shell.ui_data_cache.layer_lock_alpha.clone(),
                layer_selected: self.shell.ui_data_cache.layer_selected.clone(),
                layer_is_clipped: self.shell.ui_data_cache.layer_is_clipped.clone(),
                layer_is_clip_base: self.shell.ui_data_cache.layer_is_clip_base.clone(),
                layer_thumbnails: self.shell.ui_data_cache.layer_thumbnails.clone(),
                layer_mask_thumbnails: self.shell.ui_data_cache.layer_mask_thumbnails.clone(),
                layer_depths: self.shell.ui_data_cache.layer_depths.clone(),
                layer_expanded: self.shell.ui_data_cache.layer_expanded.clone(),
                layer_collapsed_hidden: self.shell.ui_data_cache.layer_collapsed_hidden.clone(),
                scroll_layers_to_active,
            },
            tool: ToolViewModel {
                active_tool: self.edit.tools.active_id(),
                tool_group_preferences: std::sync::Arc::new(self.edit.tools.group_preferences()),
                text_editing: self.edit.text_edit.is_some(),
                text_buffer: self
                    .edit
                    .text_edit
                    .as_ref()
                    .map(|s| s.buffer.clone())
                    .unwrap_or_default(),
                text_overlay_pos: self
                    .edit
                    .text_edit
                    .as_ref()
                    .map(|s| {
                        (
                            s.origin.0 as f32 * self.edit.view.zoom + self.edit.view.offset_x,
                            s.origin.1 as f32 * self.edit.view.zoom + self.edit.view.offset_y,
                        )
                    })
                    .unwrap_or((0.0, 0.0)),
                text_overlay_origin: self
                    .edit
                    .text_edit
                    .as_ref()
                    .map(|s| s.origin)
                    .unwrap_or((0, 0)),
                text_font_px: self.edit.text_font_px,
                text_font_family: self.edit.text_font_family.clone(),
                text_color: self.edit.text_color,
                text_align: self.edit.text_align,
                text_bold: self.edit.text_bold,
                text_italic: self.edit.text_italic,
                text_underline: self.edit.text_underline,
                text_line_height: self.edit.text_line_height,
                text_tracking_px: self.edit.text_tracking_px,
                text_opacity: self.edit.text_opacity,
                text_rotation_deg: self
                    .edit
                    .text_edit
                    .as_ref()
                    .map(|s| s.rotation_deg)
                    .unwrap_or(0.0),
                text_stretch_x: self
                    .edit
                    .text_edit
                    .as_ref()
                    .map(|s| s.stretch_x)
                    .unwrap_or(1.0),
                text_flip_x: self
                    .edit
                    .text_edit
                    .as_ref()
                    .map(|s| s.flip_x)
                    .unwrap_or(false),
                text_flip_y: self
                    .edit
                    .text_edit
                    .as_ref()
                    .map(|s| s.flip_y)
                    .unwrap_or(false),
                text_glyph_styles: std::sync::Arc::new(
                    self.edit
                        .text_edit
                        .as_ref()
                        .map(|s| s.glyph_styles.clone())
                        .unwrap_or_default(),
                ),
                text_selection: self
                    .edit
                    .text_edit
                    .as_ref()
                    .and_then(|s| s.selection.as_ref().cloned()),
                text_caret: self.edit.text_edit.as_ref().and_then(|s| s.caret),
                text_focus_pending: self.edit.text_focus_pending,
                text_font_available: crate::core::text::font_available(),
                text_font_registered: self
                    .edit
                    .text_fonts_registered
                    .contains(&self.edit.text_font_family.egui_family_name()),
                brush_size,
                brush_hardness,
                brush_opacity,
                brush_spacing,
                brush_smoothing,
                brush_flow,
                brush_preset_idx,
                brush_popup_pos: self.edit.brush_popup_pos,
                brush_color: self.edit.tools.brush().settings.color,
                brush_ink: self.ink_split_of(self.edit.tools.brush().settings.color),
                bg_color: self.edit.bg_color,
                crop_mode: self.edit.tools.crop().mode as u8,
                crop_ratio_w: self.edit.tools.crop().ratio_w,
                crop_ratio_h: self.edit.tools.crop().ratio_h,
                crop_overlay: {
                    use crate::tools::crop::CropOverlay;
                    match self.edit.tools.crop().overlay {
                        CropOverlay::None => 0,
                        CropOverlay::RuleOfThirds => 1,
                        CropOverlay::Grid => 2,
                        CropOverlay::GoldenRatio => 3,
                        _ => 0,
                    }
                },
                crop_unit: self.edit.tools.crop().unit as u8,
                crop_dpi: self.edit.tools.crop().dpi,
                crop_w_display: {
                    let c = self.edit.tools.crop();
                    if c.mode == crate::tools::crop::CropMode::FixedSize {
                        c.fixed_w
                    } else if c.has_selection() {
                        let w_px = (c.crop_x1 - c.crop_x0).abs();
                        crate::core::units::from_pixels(
                            w_px,
                            c.unit,
                            c.dpi,
                            self.docs.documents[self.docs.active_doc_idx].canvas.width as f32,
                        )
                    } else {
                        0.0
                    }
                },
                crop_h_display: {
                    let c = self.edit.tools.crop();
                    if c.mode == crate::tools::crop::CropMode::FixedSize {
                        c.fixed_h
                    } else if c.has_selection() {
                        let h_px = (c.crop_y1 - c.crop_y0).abs();
                        crate::core::units::from_pixels(
                            h_px,
                            c.unit,
                            c.dpi,
                            self.docs.documents[self.docs.active_doc_idx].canvas.height as f32,
                        )
                    } else {
                        0.0
                    }
                },
                fill_tolerance: self.edit.tools.fill().tolerance,
                fill_contiguous: self.edit.tools.fill().contiguous,
                fill_anti_alias: self.edit.tools.fill().anti_alias,
                fill_all_layers: self.edit.tools.fill().sample_merged,
                gradient_mode: self.active_gradient_mode(),
                gradient_type: self.active_gradient_ui_type(),
                gradient_opacity: self.edit.tools.gradient().opacity,
                gradient_reverse: self.edit.tools.gradient().reverse,
                gradient_dither: self.edit.tools.gradient().dither,
                gradient_stops: self.active_gradient_ui_stops(),
                show_gradient_editor: self.shell.ui.show_gradient_editor,
                eyedropper_sample: self.edit.tools.eyedropper().sample_size as u8,
                eyedropper_sample_merged: self.edit.tools.eyedropper().sample_merged,
                move_auto_select: self.edit.tools.move_tool().auto_select,
                move_show_transform: self.edit.tools.move_tool().show_transform,
                clone_size: self.edit.tools.clone_like().size,
                clone_hardness: self.edit.tools.clone_like().hardness,
                clone_opacity: self.edit.tools.clone_like().opacity,
                clone_spacing: self.edit.tools.clone_like().spacing,
                clone_aligned: self.edit.tools.clone_like().aligned,
                clone_sample_merged: self.edit.tools.clone_like().sample_merged,
                clone_smart_fill: self.edit.tools.clone_like().smart_fill,
                clone_source_thumbnail,
                smudge_size: self.edit.tools.smudge().size,
                smudge_hardness: self.edit.tools.smudge().hardness,
                smudge_strength: self.edit.tools.smudge().strength,
                smudge_finger_painting: self.edit.tools.smudge().finger_painting,
                dodge_size: self.edit.tools.dodge_burn().size,
                dodge_hardness: self.edit.tools.dodge_burn().hardness,
                dodge_exposure: self.edit.tools.dodge_burn().exposure,
                dodge_range: self.edit.tools.dodge_burn().range.to_u8(),
                dodge_protect_tones: self.edit.tools.dodge_burn().protect_tones,
                patch_mode: self.edit.tools.patch().mode.to_u8(),
                transform_interpolation: self.shell.ui.transform_interpolation,
                wand_brush_size: self.edit.tools.wand().brush_size,
                wand_tolerance: self.edit.tools.wand().tolerance,
                wand_edge_sensitivity: self.edit.tools.wand().edge_sensitivity,
                wand_contiguous: self.edit.tools.wand().contiguous,
                wand_anti_alias: self.edit.tools.wand().anti_alias,
                wand_sample_merged: self.edit.tools.wand().sample_merged,
                pen_mode: self.edit.tools.pen().mode.to_u8(),
                pen_stroke_width: self.edit.tools.pen().stroke_width,
                vector_brush_width: self.edit.tools.vector_brush().width,
                vector_brush_color: self.edit.fg_color,
                vector_brush_smoothing: self.edit.tools.vector_brush().smoothing,
                vector_brush_pressure: self.edit.tools.vector_brush().pressure,
                vector_brush_velocity: self.edit.tools.vector_brush().velocity,
                vector_brush_path: if self.edit.tools.active_id()
                    == crate::tools::ToolId::VectorBrush
                {
                    self.edit.tools.vector_brush().preview_points()
                } else {
                    Vec::new()
                },
                vector_brush_can_expand: self.active_brush_layer_id().is_some(),
                shape_kind: self.edit.tools.shape().kind.to_u8(),
                shape_fill: self.edit.tools.shape().fill,
                shape_fill_color: self.edit.tools.shape().fill_color,
                shape_stroke_width: self.edit.tools.shape().stroke_width,
                shape_stroke_color: self.edit.tools.shape().stroke_color,
                shape_corner_radius: self.edit.tools.shape().corner_radius,
                shape_corner_type: self.edit.tools.shape().corner_type.to_u8(),
                shape_sides: self.edit.tools.shape().sides,
                shape_star_inner: self.edit.tools.shape().star_inner,
                shape_preview: if self.edit.tools.active_id() == crate::tools::ToolId::Shape {
                    self.edit.tools.shape().preview_rect()
                } else {
                    None
                },
                // Editing handles for the active Shape layer, hidden while a new
                // shape is being rubber-banded.
                shape_overlay: if self.edit.tools.active_id() == crate::tools::ToolId::Shape
                    && !self.edit.tools.shape().is_dragging()
                {
                    self.active_shape_overlay()
                        .map(|(span, kind, radius, handles)| crate::ui::ShapeOverlay {
                            span,
                            kind,
                            radius,
                            handles,
                            dragging: self.shape_drag_active() || self.shape_style_scrub_active(),
                        })
                } else {
                    None
                },
                path_display: self.active_path_display(),
                // On-canvas node editing overlay for the active Path (Node tool).
                node_overlay: self.active_node_overlay(),
                path_gradient_overlay: self.active_path_gradient_overlay(),
                // Fill/Outline of the active Path (options bar under Move / Node).
                // Kept available outside Move/Node as well: the document Palette
                // applies Fill/Outline to the selected editable Path.
                path_style: self.active_path_style_vm(),
                gradient_preview: if self.edit.tools.active_id() == crate::tools::ToolId::Gradient {
                    self.edit.tools.gradient().preview_line()
                } else {
                    None
                },
                crop_rect: {
                    let c = self.edit.tools.crop();
                    if self.edit.tools.active_id() == crate::tools::ToolId::Crop
                        && c.has_selection()
                    {
                        Some([c.crop_x0, c.crop_y0, c.crop_x1, c.crop_y1])
                    } else {
                        None
                    }
                },
                crop_rotation: 0.0,
                persp_crop_quad: if self.edit.tools.active_id()
                    == crate::tools::ToolId::PerspectiveCrop
                    && self.edit.tools.perspective_crop().has_quad()
                {
                    Some(self.edit.tools.perspective_crop().corners)
                } else {
                    None
                },
                persp_crop_preview: if self.edit.tools.active_id()
                    == crate::tools::ToolId::PerspectiveCrop
                    && self.edit.tools.perspective_crop().is_placing()
                {
                    if let Some(rect) = self.edit.tools.perspective_crop().sweep_preview() {
                        // Rubber-band rectangle being swept → preview it as a full quad.
                        rect.to_vec()
                    } else {
                        let mut pts = self.edit.tools.perspective_crop().placing_points().to_vec();
                        if !pts.is_empty() {
                            // Append the live cursor as the tentative next corner so the
                            // rubber-band line / forming grid track the pointer in real time.
                            let ev = self.tool_event();
                            pts.push((ev.canvas_x, ev.canvas_y));
                        }
                        pts
                    }
                } else {
                    Vec::new()
                },
                persp_w_display: {
                    let pc = self.edit.tools.perspective_crop();
                    let cw = self.docs.documents[self.docs.active_doc_idx].canvas.width as f32;
                    let ch = self.docs.documents[self.docs.active_doc_idx].canvas.height as f32;
                    pc.display_values(cw, ch).0
                },
                persp_h_display: {
                    let pc = self.edit.tools.perspective_crop();
                    let cw = self.docs.documents[self.docs.active_doc_idx].canvas.width as f32;
                    let ch = self.docs.documents[self.docs.active_doc_idx].canvas.height as f32;
                    pc.display_values(cw, ch).1
                },
                persp_unit: self.edit.tools.perspective_crop().unit,
                persp_dpi: self.edit.tools.perspective_crop().dpi,
                persp_size_manual: self.edit.tools.perspective_crop().manual_size.is_some(),
                persp_has_quad: self.edit.tools.perspective_crop().has_quad(),
                pen_path: if self.edit.tools.active_id() == crate::tools::ToolId::Pen {
                    // Only the placed anchors — no rubber-band segment to the cursor.
                    self.edit.tools.pen().preview_points()
                } else {
                    Vec::new()
                },
                pen_anchors: if self.edit.tools.active_id() == crate::tools::ToolId::Pen {
                    self.edit
                        .tools
                        .pen()
                        .anchors()
                        .iter()
                        .map(|a| a.pt)
                        .collect()
                } else {
                    Vec::new()
                },
                pen_handles: if self.edit.tools.active_id() == crate::tools::ToolId::Pen {
                    self.edit.tools.pen().handle_segments()
                } else {
                    Vec::new()
                },
                pen_closed: self.edit.tools.active_id() == crate::tools::ToolId::Pen
                    && self.edit.tools.pen().is_closed(),
                crop_cursor_hint: self.crop_cursor_hint(),
                transform_overlay: self
                    .edit
                    .transform_state
                    .as_ref()
                    .map(|ts| {
                        let corners = ts.corners();
                        let handles = ts.handle_positions();
                        let center = ts.transform_point(ts.pivot_cx, ts.pivot_cy);
                        TransformOverlayData {
                            corners,
                            handles,
                            center,
                            // Free Transform has no relocatable pivot: marker on centre.
                            pivot: center,
                            pivot_snap_label: None,
                        }
                    })
                    // No modal Free Transform: the Move tool's active Path shows
                    // the same oriented box for on-canvas scale/rotate.
                    .or_else(|| {
                        self.active_path_transform_box()
                            .map(|b| TransformOverlayData {
                                corners: b.corners,
                                handles: b.handles,
                                center: b.center,
                                pivot: b.pivot,
                                // Snap label shown only while the pivot is dragged.
                                pivot_snap_label: if self.edit.path_pivot_dragging {
                                    self.edit.path_pivot_snap
                                } else {
                                    None
                                },
                            })
                    }),
                transform_scale_x: self
                    .edit
                    .transform_state
                    .as_ref()
                    .map(|ts| ts.scale_x)
                    .unwrap_or(1.0),
                transform_scale_y: self
                    .edit
                    .transform_state
                    .as_ref()
                    .map(|ts| ts.scale_y)
                    .unwrap_or(1.0),
                transform_angle: self
                    .edit
                    .transform_state
                    .as_ref()
                    .map(|ts| ts.angle_deg)
                    .unwrap_or(0.0),
                transform_tx: self
                    .edit
                    .transform_state
                    .as_ref()
                    .map(|ts| ts.translate_x)
                    .unwrap_or(0.0),
                transform_ty: self
                    .edit
                    .transform_state
                    .as_ref()
                    .map(|ts| ts.translate_y)
                    .unwrap_or(0.0),
                transform_cursor_hint: self.transform_cursor_hint(),
                transform_ctx_menu_pos: self.edit.transform_ctx_menu_pos,
            },
            sel: SelectionViewModel {
                selection_mode: self.edit.selection_mode,
                lasso_preview: if self.edit.tools.active_id() == crate::tools::ToolId::PolygonLasso
                {
                    let mut pts = self.edit.tools.polygon_lasso().preview_points().to_vec();
                    if !pts.is_empty() {
                        let ev = self.tool_event();
                        pts.push((ev.canvas_x, ev.canvas_y));
                    }
                    pts
                } else if self.edit.tools.active_id() == crate::tools::ToolId::Patch {
                    self.edit.tools.patch().preview_points().to_vec()
                } else {
                    self.edit.tools.lasso().preview_points().to_vec()
                },
                rect_sel_preview: self
                    .edit
                    .tools
                    .move_tool()
                    .preview_rect()
                    .or_else(|| self.edit.tools.selection_rect().preview_rect()),
                ellipse_sel_preview: self.edit.tools.selection_ellipse().preview_ellipse(),
                selection_ctx_menu_pos: self.edit.selection_ctx_menu_pos,
                has_selection: self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .selection
                    .active,
                select_subject_busy: self.jobs.select_subject.is_busy(),
                select_subject_status_msg: self.jobs.select_subject.status_text(),
                select_subject_model: self.jobs.select_subject.selected_model(),
                show_refine_panel: self.edit.show_refine_panel,
                refine_feather: self.edit.refine_feather,
                refine_smooth: self.edit.refine_smooth,
                refine_smart_radius: self.edit.refine_smart_radius,
                refine_shift_edge: self.edit.refine_shift_edge,
                refine_contrast: self.edit.refine_contrast,
                refine_decontaminate: self.edit.refine_decontaminate,
                refine_decontaminate_amount: self.edit.refine_decontaminate_amount,
                refine_brush_size: self.edit.tools.refine_brush().size,
                refine_brush_hardness: self.edit.tools.refine_brush().hardness,
                refine_brush_mode: self.edit.tools.refine_brush().mode,
                refine_view_mode: self.edit.refine_view_mode,
                refine_overlay_color: self.edit.refine_overlay_color,
                show_refine_color_dialog: self.shell.ui.show_refine_color_dialog,
                refine_color_dialog_color: self.shell.ui.refine_color_dialog_color,
                refine_color_dialog_original: self.shell.ui.refine_color_dialog_original,
                refine_color_dialog_live_preview: self.shell.ui.refine_color_dialog_live_preview,
                refine_color_dialog_center_next: self.shell.ui.refine_color_dialog_center_next,
                refine_output_mode: self.edit.refine_output_mode,
                refine_overlay_tex: self.edit.refine_overlay_tex.as_ref().map(|t| t.id()),
            },
            develop: DevelopViewModel {
                show_develop_dialog: self.shell.ui.show_develop_dialog,
                develop_in_window: self.win.develop_window.is_some(),
                develop_settings: self.shell.ui.develop_settings.clone(),
                develop_histogram: self.dev.develop_histogram.clone(),
                develop_mode: {
                    let active_id = self.docs.documents[self.docs.active_doc_idx].id;
                    self.dev
                        .develop_session
                        .iter()
                        .any(|e| e.doc == active_id && e.transient)
                },
                develop_sections_open: self.dev.develop_sections_open,
                develop_exif: self.docs.documents[self.docs.active_doc_idx]
                    .raw_exif
                    .clone(),
                develop_readout: self.dev.develop_readout,
                develop_auto_available: self
                    .dev
                    .develop_preview
                    .as_ref()
                    .is_some_and(|p| p.scene.is_some() && !p.histogram_proxy.is_empty()),
                develop_local_selected: self.shell.ui.develop_local_selected,
                develop_local_arm: self.shell.ui.develop_local_arm.map(|(k, _)| k),
                develop_local_overlay: self.build_develop_local_overlay(),
                develop_presets: self.shell.develop_presets.clone(),
            },
            print: PrintViewModel {
                proof_enabled: self.shell.proof_enabled,
                proof_gamut_warn: self.shell.proof_gamut_warn,
                proof_target_label: self.shell.proof_target.label(),
                display_cms_enabled: self.shell.display_cms_enabled,
                display_profile_name: self.shell.display_profile_name.clone(),
                show_print_dialog: self.shell.ui.show_print_dialog,
                print_layout: self.shell.print_layout,
                print_printers: std::sync::Arc::new(self.shell.print_printers.clone()),
                print_selected_printer: self.shell.print_selected_printer.clone(),
                print_copies: self.shell.print_copies,
                print_refreshing: self.jobs.pending_printer_refresh.is_some(),
                print_preview_image,
                print_printer_profile_name: self.shell.print_printer_profile_name.clone(),
            },
            dialogs: DialogViewModel {
                paint_dialog_ink: if self.shell.ui.show_paint_color_dialog {
                    self.ink_split_of(self.shell.ui.paint_color_dialog_color)
                } else {
                    None
                },
                show_new_dialog: self.shell.ui.show_new_dialog,
                show_resize_dialog: self.shell.ui.show_resize_dialog,
                show_image_size_dialog: self.shell.ui.show_image_size_dialog,
                show_rename_dialog: self.shell.ui.show_rename_dialog,
                show_export_dialog: self.shell.ui.show_export_dialog,
                show_preferences: self.shell.ui.show_preferences,
                show_adjustment_dialog: self.shell.ui.show_adjustment_dialog,
                adjustment_dialog: self.shell.ui.adjustment_dialog.clone(),
                adjustment_preview_enabled: self.shell.ui.adjustment_preview_enabled,
                adjustment_options: self.shell.ui.adjustment_options,
                adj_eyedropper: self.shell.ui.adj_eyedropper,
                levels_histogram: self
                    .shell
                    .adjustment_preview
                    .as_ref()
                    .map(|preview| preview.levels_histogram.clone())
                    .unwrap_or_else(|| std::sync::Arc::new([[0; 256]; 4])),
                show_warp_dialog: self.shell.ui.show_warp_dialog,
                warp_params: self.shell.ui.warp_params,
                warp_resizing: self.edit.input.warp_resizing,
                warp_resize_anchor: {
                    // Physical px → egui points (egui ppp == window scale factor here).
                    let ppp = self
                        .win
                        .window
                        .as_ref()
                        .map(|w| w.scale_factor() as f32)
                        .unwrap_or(1.0)
                        .max(0.0001);
                    (
                        self.edit.input.alt_drag_start_x / ppp,
                        self.edit.input.alt_drag_start_y / ppp,
                    )
                },
                warp_freeze: self.edit.warp_state.as_ref().and_then(|s| {
                    s.mesh.freeze_alpha().map(|(gw, gh, alpha)| {
                        std::sync::Arc::new(crate::ui::WarpFreezeView {
                            gw,
                            gh,
                            alpha,
                            layer_x: s.layer_offset.0 as f32,
                            layer_y: s.layer_offset.1 as f32,
                            layer_w: s.layer_w as f32,
                            layer_h: s.layer_h as f32,
                        })
                    })
                }),
                show_filter_dialog: self.shell.ui.show_filter_dialog,
                filter_dialog: self.shell.ui.filter_dialog,
                filter_proxy_original: self
                    .shell
                    .filter_preview
                    .as_ref()
                    .map(|preview| preview.proxy_original_preview.clone()),
                filter_proxy_preview: self
                    .shell
                    .filter_preview
                    .as_ref()
                    .and_then(|preview| preview.proxy_preview.clone()),
                filter_preview_processing: self
                    .shell
                    .filter_preview
                    .as_ref()
                    .is_some_and(|preview| preview.processing),
                filter_preview_enabled: self.shell.ui.filter_preview_enabled,
                show_smart_fill_dialog: self.shell.ui.show_smart_fill_dialog,
                lama_available: crate::core::lama::is_available(),
                lama_status_msg: crate::core::lama::status_text().unwrap_or_default(),
                show_exit_dialog: self.shell.ui.show_exit_dialog,
                show_close_dialog: self.shell.ui.show_close_dialog,
                show_reload_file_dialog: self.jobs.pending_reload_prompt.is_some(),
                reload_file_name: self
                    .jobs
                    .pending_reload_prompt
                    .as_ref()
                    .and_then(|p| p.path.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("file")
                    .to_string(),
                reload_will_discard_changes: self
                    .jobs
                    .pending_reload_prompt
                    .as_ref()
                    .and_then(|p| self.docs.documents.get(p.doc_idx))
                    .is_some_and(|d| d.is_modified()),
                show_pdf_import_dialog: self.jobs.pending_pdf_prompt.is_some(),
                show_cmyk_convert_dialog: self.shell.ui.show_cmyk_convert_dialog,
                cmyk_convert_use_icc: self.shell.ui.cmyk_convert_use_icc,
                cmyk_convert_icc_name: self
                    .shell
                    .ui
                    .cmyk_convert_icc
                    .as_ref()
                    .map(|(n, _)| n.clone()),
                pdf_import_file_name: self
                    .jobs
                    .pending_pdf_prompt
                    .as_ref()
                    .and_then(|p| p.path.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("PDF")
                    .to_string(),
                pdf_import_path_key: self
                    .jobs
                    .pending_pdf_prompt
                    .as_ref()
                    .map(|p| p.path.to_string_lossy().to_string())
                    .unwrap_or_default(),
                pdf_import_page_count: self
                    .jobs
                    .pending_pdf_prompt
                    .as_ref()
                    .map_or(0, |p| p.page_count),
                pdf_import_page_dims: self
                    .jobs
                    .pending_pdf_prompt
                    .as_ref()
                    .map(|p| p.page_dims.clone())
                    .unwrap_or_default(),
                show_feather_dialog: self.shell.ui.show_feather_dialog,
                show_modify_dialog: self.shell.ui.show_modify_dialog,
                show_stroke_dialog: self.shell.ui.show_stroke_dialog,
                new_w_input: self.shell.ui.new_w_input,
                new_h_input: self.shell.ui.new_h_input,
                new_dpi: self.shell.ui.new_dpi,
                new_bg_color: self.shell.ui.new_bg_color,
                new_name: self.shell.ui.new_name.clone(),
                new_unit: self.shell.ui.new_unit,
                rename_idx: self.shell.ui.rename_idx,
                rename_text: self.shell.ui.rename_text.clone(),
                export_format: self.shell.ui.export_format.clone(),
                show_paint_color_dialog: self.shell.ui.show_paint_color_dialog,
                paint_color_dialog_target: self.shell.ui.paint_color_dialog_target,
                paint_color_dialog_color: self.shell.ui.paint_color_dialog_color,
                paint_color_dialog_original: self.shell.ui.paint_color_dialog_original,
                paint_color_dialog_live_preview: self.shell.ui.paint_color_dialog_live_preview,
                paint_color_dialog_center_next: self.shell.ui.paint_color_dialog_center_next,
                user_presets: self.shell.user_presets.clone(),
                adjustment_presets: self.shell.adjustment_presets.clone(),
                show_preset_dialog: self.shell.ui.show_preset_dialog,
                show_delete_preset_dialog: self.shell.ui.show_delete_preset_dialog,
                preset_dialog_name: self.shell.ui.preset_dialog_name.clone(),
                preset_dialog_w: self.shell.ui.preset_dialog_w,
                preset_dialog_h: self.shell.ui.preset_dialog_h,
                preset_dialog_unit: self.shell.ui.preset_dialog_unit.clone(),
                preset_dialog_dpi: self.shell.ui.preset_dialog_dpi,
            },
            chrome: ChromeViewModel {
                status_msg: self.shell.status_msg.clone(),
                show_welcome: self.shell.ui.show_welcome,
                show_library: self.shell.ui.show_library,
                theme_mode: self.shell.ui.theme_mode,
                show_color_panel: self.shell.ui.show_color_panel,
                show_text_panel: self.shell.ui.show_text_panel,
                show_layer_panel: self.shell.ui.show_layer_panel,
                show_history_panel: self.shell.ui.show_history_panel,
                show_info_panel: self.shell.ui.show_info_panel,
                show_channels_panel: self.shell.ui.show_channels_panel,
                show_rulers: self.shell.ui.show_rulers,
                show_guides: self.shell.ui.show_guides,
                lock_guides: self.shell.ui.lock_guides,
                snap_enabled: self.shell.ui.snap_enabled,
                guides: self.docs.documents[self.docs.active_doc_idx].guides.clone(),
                guide_preview: match self.edit.guide_op {
                    Some(crate::app::state::GuideOp::Create { orientation, pos }) => {
                        Some((orientation, pos))
                    }
                    _ => None,
                },
                hovered_guide: self.active_hover_guide(),
                snap_guides: if self.edit.transform_state.is_some() {
                    self.edit.transform_snap_guides.clone()
                } else if self.edit.tools.active_id() == crate::tools::ToolId::Move {
                    self.edit.tools.move_tool().snap_guides.clone()
                } else {
                    Vec::new()
                },
                toolbar_w: self.shell.toolbar_w,
                panel_r_w: self.shell.panel_r_w,
                is_tool_modal: self.modal_lock_active(),
                modal_flash: self
                    .shell
                    .ui
                    .modal_flash_until
                    .is_some_and(|until| std::time::Instant::now() < until),
            },
            channels: ChannelsViewModel {
                channels_write_mask: self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .channels
                    .write_mask,
                channel_view: self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .channels
                    .view,
                alpha_channels: self.shell.ui_data_cache.alpha_channel_names.clone(),
                active_alpha_channel: self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .channels
                    .active_alpha,
                channel_thumbnails: self.shell.ui_data_cache.channel_thumbnails.clone(),
                alpha_thumbnails: self.shell.ui_data_cache.alpha_thumbnails.clone(),
            },
            ai: AiViewModel {
                show_ai_panel: self.shell.ui.show_ai_panel,
                ai: self.shell.ui.ai.clone(),
                ai_status: self.shell.ui.ai_status.clone(),
                ai_history: self.jobs.ai_engine.settings.history.clone(),
                ext_connected: self.jobs.ext.connected,
                ext_queue_len: self.jobs.ext.queue_len(),
                ext_status: self.jobs.ext.status.clone(),
                ext_log: self.jobs.ext.log.iter().cloned().collect(),
                ext_token: self.jobs.ext.token.clone(),
            },
            welcome: crate::ui::WelcomeViewModel {
                recent: welcome_recent,
            },
            library,
        }
    }

    /// CMYK ink split of an RGB colour through the active document's converter
    /// — `None` on an RGB document. Feeds the Info-panel and colour-picker
    /// ink readouts.
    fn ink_split_of(&self, color: [u8; 4]) -> Option<[u8; 4]> {
        let conv = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .cmyk_converter()?;
        let mut ink = [[0u8; 4]; 1];
        conv.rgb_to_cmyk_slice(&[[color[0], color[1], color[2]]], &mut ink);
        Some(ink[0])
    }
}

/// Stable UI-facing layer kind. Do not derive this from `Debug`: unified
/// `LayerType::Vector` deliberately contains both parametric Shape and curve
/// Path geometry, but the Layer menu needs to distinguish them for
/// Convert-to-Curves and Rasterize.
fn layer_ui_type(layer_type: &crate::core::layer::LayerType) -> &'static str {
    use crate::core::layer::LayerType;
    use crate::core::vector::object::VectorGeometry;
    match layer_type {
        LayerType::Raster => "Raster",
        LayerType::Adjustment(_) => "Adjustment",
        LayerType::Text(_) => "Text",
        LayerType::Group => "Group",
        LayerType::SmartObject => "SmartObject",
        LayerType::Vector(VectorGeometry::Primitive(_)) => "Shape",
        LayerType::Vector(VectorGeometry::Path(_)) => "Path",
    }
}

fn build_layer_thumbnail_rgba(layer: &crate::core::layer::Layer) -> Vec<u8> {
    let mut out = vec![0_u8; PANEL_THUMB * PANEL_THUMB * 4];
    if layer.width == 0 || layer.height == 0 {
        return out;
    }

    let (draw_w, draw_h, off_x, off_y) =
        thumbnail_fit(layer.width as usize, layer.height as usize, PANEL_THUMB);
    for y in 0..draw_h {
        for x in 0..draw_w {
            let sx = ((x as f32 + 0.5) / draw_w as f32 * layer.width as f32) - 0.5;
            let sy = ((y as f32 + 0.5) / draw_h as f32 * layer.height as f32) - 0.5;
            let [r, g, b, a] = bilinear_rgba_sample(sx, sy, |px, py| {
                let px = px.clamp(0, layer.width.saturating_sub(1) as i32) as u32;
                let py = py.clamp(0, layer.height.saturating_sub(1) as i32) as u32;
                let (r, g, b, a) = layer.tiles.get_pixel(px, py);
                [r, g, b, a]
            });
            let i = ((off_y + y) * PANEL_THUMB + off_x + x) * 4;
            out[i] = r;
            out[i + 1] = g;
            out[i + 2] = b;
            out[i + 3] = a;
        }
    }
    out
}

/// Sample the flat composite into the Channels panel plates: one RGBA
/// composite thumbnail plus three grayscale plates (R, G, B), all panel-sized,
/// in a single pass.
fn build_channel_thumbnails_rgba(pixels: &[u8], w: u32, h: u32) -> Vec<Vec<u8>> {
    let mut out = vec![blank_channel_thumbnail_rgba(); 4];
    if w == 0 || h == 0 || pixels.len() < (w as usize) * (h as usize) * 4 {
        return out;
    }
    let (draw_w, draw_h, off_x, off_y) = thumbnail_fit(w as usize, h as usize, CHANNEL_THUMB);
    for y in 0..draw_h {
        for x in 0..draw_w {
            let sx = ((x as f32 + 0.5) / draw_w as f32 * w as f32) - 0.5;
            let sy = ((y as f32 + 0.5) / draw_h as f32 * h as f32) - 0.5;
            let rgba = bilinear_rgba_sample(sx, sy, |px, py| {
                let px = px.clamp(0, w.saturating_sub(1) as i32) as usize;
                let py = py.clamp(0, h.saturating_sub(1) as i32) as usize;
                let si = (py * w as usize + px) * 4;
                [pixels[si], pixels[si + 1], pixels[si + 2], pixels[si + 3]]
            });
            let i = ((off_y + y) * CHANNEL_THUMB + off_x + x) * 4;
            out[0][i..i + 4].copy_from_slice(&rgba);
            for c in 0..3 {
                let v = rgba[c];
                let plate = &mut out[c + 1];
                plate[i] = v;
                plate[i + 1] = v;
                plate[i + 2] = v;
                plate[i + 3] = 255;
            }
        }
    }
    out
}

/// Channels panel thumbnails for a CMYK document: composite RGB mirror plus
/// C/M/Y/K ink-density plates (paper white, ink dark).
fn build_channel_thumbnails_cmyk(canvas: &crate::core::canvas::Canvas) -> Vec<Vec<u8>> {
    let mut out = Vec::with_capacity(5);
    let rgb = build_channel_thumbnails_rgba(&canvas.pixels, canvas.width, canvas.height);
    out.push(
        rgb.into_iter()
            .next()
            .unwrap_or_else(blank_channel_thumbnail_rgba),
    );
    for channel in 0..4 {
        let thumb = canvas
            .cmyk_plate_preview_mask(channel)
            .map(|mask| build_grayscale_thumbnail_rgba(&mask, canvas.width, canvas.height))
            .unwrap_or_else(blank_channel_thumbnail_rgba);
        out.push(thumb);
    }
    out
}

fn blank_channel_thumbnail_rgba() -> Vec<u8> {
    vec![0_u8; CHANNEL_THUMB * CHANNEL_THUMB * 4]
}

fn build_grayscale_thumbnail_rgba(mask: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut out = blank_channel_thumbnail_rgba();
    let (w, h) = (w as usize, h as usize);
    if w == 0 || h == 0 || mask.len() < w * h {
        return out;
    }
    let (draw_w, draw_h, off_x, off_y) = thumbnail_fit(w, h, CHANNEL_THUMB);
    for y in 0..draw_h {
        for x in 0..draw_w {
            let sx = ((x as f32 + 0.5) / draw_w as f32 * w as f32) - 0.5;
            let sy = ((y as f32 + 0.5) / draw_h as f32 * h as f32) - 0.5;
            let [v, _, _, _] = bilinear_rgba_sample(sx, sy, |px, py| {
                let px = px.clamp(0, w.saturating_sub(1) as i32) as usize;
                let py = py.clamp(0, h.saturating_sub(1) as i32) as usize;
                let v = mask[py * w + px];
                [v, v, v, 255]
            });
            let i = ((off_y + y) * CHANNEL_THUMB + off_x + x) * 4;
            out[i] = v;
            out[i + 1] = v;
            out[i + 2] = v;
            out[i + 3] = 255;
        }
    }
    out
}

/// One alpha channel's mask as a panel-sized grayscale thumbnail.
fn build_alpha_thumbnail_rgba(channel: &crate::core::channels::AlphaChannel) -> Vec<u8> {
    build_grayscale_thumbnail_rgba(&channel.mask, channel.width, channel.height)
}

fn build_layer_mask_thumbnail_rgba(layer: &crate::core::layer::Layer) -> Vec<u8> {
    let mut out = vec![0_u8; PANEL_THUMB * PANEL_THUMB * 4];
    let Some(mask) = layer.mask.as_ref() else {
        return Vec::new();
    };
    if mask.width == 0 || mask.height == 0 {
        return out;
    }

    let (draw_w, draw_h, off_x, off_y) =
        thumbnail_fit(mask.width as usize, mask.height as usize, PANEL_THUMB);
    for y in 0..draw_h {
        for x in 0..draw_w {
            let sx = ((x as f32 + 0.5) / draw_w as f32 * mask.width as f32) - 0.5;
            let sy = ((y as f32 + 0.5) / draw_h as f32 * mask.height as f32) - 0.5;
            let [v, _, _, _] = bilinear_rgba_sample(sx, sy, |px, py| {
                let px = px.clamp(0, mask.width.saturating_sub(1) as i32) as u32;
                let py = py.clamp(0, mask.height.saturating_sub(1) as i32) as u32;
                let v = (mask.sample(px, py) * 255.0).round().clamp(0.0, 255.0) as u8;
                [v, v, v, 255]
            });
            let i = ((off_y + y) * PANEL_THUMB + off_x + x) * 4;
            out[i] = v;
            out[i + 1] = v;
            out[i + 2] = v;
            out[i + 3] = 255;
        }
    }
    out
}

fn thumbnail_fit(src_w: usize, src_h: usize, thumb: usize) -> (usize, usize, usize, usize) {
    if src_w == 0 || src_h == 0 || thumb == 0 {
        return (0, 0, 0, 0);
    }

    let scale = (thumb as f32 / src_w as f32).min(thumb as f32 / src_h as f32);
    let draw_w = ((src_w as f32 * scale).round() as usize).clamp(1, thumb);
    let draw_h = ((src_h as f32 * scale).round() as usize).clamp(1, thumb);
    let off_x = (thumb - draw_w) / 2;
    let off_y = (thumb - draw_h) / 2;
    (draw_w, draw_h, off_x, off_y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::layer::{Layer, LayerMask};
    use crate::core::shape::{ShapeData, ShapeKind};
    use crate::core::vector::object::{VectorGeometry, VectorObjectData};

    #[test]
    fn layer_thumbnail_preserves_wide_aspect_ratio() {
        let mut layer = Layer::new(1, "wide", 48, 24);
        for y in 0..layer.height {
            for x in 0..layer.width {
                layer.tiles.set_pixel(x, y, 255, 0, 0, 255);
            }
        }

        let thumb = build_layer_thumbnail_rgba(&layer);
        assert_eq!(thumb.len(), PANEL_THUMB * PANEL_THUMB * 4);
        let alpha_at = |x: usize, y: usize| thumb[(y * PANEL_THUMB + x) * 4 + 3];

        assert_eq!(alpha_at(32, 15), 0);
        assert_eq!(alpha_at(32, 16), 255);
        assert_eq!(alpha_at(32, 47), 255);
        assert_eq!(alpha_at(32, 48), 0);
    }

    #[test]
    fn mask_thumbnail_preserves_tall_aspect_ratio() {
        let mut layer = Layer::new(1, "tall-mask", 24, 48);
        layer.mask = Some(LayerMask::new_white(24, 48));

        let thumb = build_layer_mask_thumbnail_rgba(&layer);
        assert_eq!(thumb.len(), PANEL_THUMB * PANEL_THUMB * 4);
        let alpha_at = |x: usize, y: usize| thumb[(y * PANEL_THUMB + x) * 4 + 3];

        assert_eq!(alpha_at(15, 32), 0);
        assert_eq!(alpha_at(16, 32), 255);
        assert_eq!(alpha_at(47, 32), 255);
        assert_eq!(alpha_at(48, 32), 0);
    }

    #[test]
    fn unified_vector_geometry_keeps_shape_and_path_ui_kinds() {
        let (shape, _) = ShapeData::from_canvas_span(
            ShapeKind::Star,
            10.0,
            10.0,
            90.0,
            90.0,
            0.0,
            true,
            [0, 0, 0, 255],
            0.0,
            [0, 0, 0, 255],
        );
        assert_eq!(
            layer_ui_type(&crate::core::layer::LayerType::Vector(
                VectorGeometry::Primitive(shape)
            )),
            "Shape"
        );
        assert_eq!(
            layer_ui_type(&crate::core::layer::LayerType::Vector(
                VectorGeometry::Path(VectorObjectData::default())
            )),
            "Path"
        );
    }
}

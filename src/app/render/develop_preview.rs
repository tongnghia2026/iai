//! Building and flushing the Develop window's GPU preview.

use crate::app::state::App;

/// RAW colour runs per pixel in the scene shader, like its CPU commit. Its only
/// colour proxy is Develop3's guided mixer control planes; the display-domain
/// RGB proxies (mode 1) belong to Identity sessions and on RAW re-applied
/// Saturation/Vibrance on top of the scene shader.
fn raw_color_runs_per_pixel(settings: &crate::core::develop::DevelopSettings) -> bool {
    settings.has_color() && !crate::core::develop::guided_mixer_active(settings)
}

/// Pixel budget of the GPU-resident Detail plane (about a 2K viewport at
/// 100 %); a larger view samples every `downsample` layer px instead.
const GPU_DETAIL_MAX_PLANE_PX: u64 = 4_000_000;

/// Plan the GPU Detail plane over the visible layer rect `[lx0, lx1) × [ly0,
/// ly1)`: one texel per `downsample` layer px — never coarser than the
/// display samples the layer unless the budget forces it — plus Detail's
/// apron on every side, with the origin snapped to the texel grid so a pan
/// keeps the sampling phase. Returns `(origin_x, origin_y, end_x, end_y,
/// downsample)`.
fn plan_gpu_detail_region(
    (lx0, ly0, lx1, ly1): (u32, u32, u32, u32),
    src_w: u32,
    src_h: u32,
    zoom: f32,
    budget: u64,
) -> (u32, u32, u32, u32, u32) {
    let mut ds = if zoom >= 1.0 {
        1
    } else {
        ((1.0 / zoom.max(1e-4)).floor() as u32).max(1)
    };
    loop {
        let pad = crate::core::develop::DETAIL_HALO as u32 * ds;
        let ox = lx0.saturating_sub(pad) / ds * ds;
        let oy = ly0.saturating_sub(pad) / ds * ds;
        let ex = lx1.saturating_add(pad).min(src_w).max(ox + 1);
        let ey = ly1.saturating_add(pad).min(src_h).max(oy + 1);
        let pw = (ex - ox).div_ceil(ds) as u64;
        let ph = (ey - oy).div_ceil(ds) as u64;
        if pw * ph <= budget || ds >= 64 {
            return (ox, oy, ex, ey, ds);
        }
        ds += 1;
    }
}

/// `settings` with its Detail sliders taken from `from`.
fn with_detail_of(
    settings: &crate::core::develop::DevelopSettings,
    from: &crate::core::develop::DevelopSettings,
) -> crate::core::develop::DevelopSettings {
    crate::core::develop::DevelopSettings {
        sharpening: from.sharpening,
        sharpen_radius: from.sharpen_radius,
        sharpen_detail: from.sharpen_detail,
        sharpen_masking: from.sharpen_masking,
        noise_reduction: from.noise_reduction,
        noise_reduction_detail: from.noise_reduction_detail,
        noise_reduction_contrast: from.noise_reduction_contrast,
        color_noise_reduction: from.color_noise_reduction,
        color_noise_detail: from.color_noise_detail,
        color_noise_smoothness: from.color_noise_smoothness,
        ..settings.clone()
    }
}

fn run_native_gpu_detail(
    gpu: &crate::gpu::GpuState,
    pixels: &mut Vec<[f32; 3]>,
    w: usize,
    h: usize,
    settings: &crate::core::develop::DevelopSettings,
    linear: bool,
    luma_coeff: [f32; 3],
) {
    let params = crate::gpu::detail_gpu::DetailWorkingParams::from_settings(settings);
    let mut rgb = Vec::with_capacity(pixels.len() * 3);
    for pixel in pixels.iter() {
        rgb.extend_from_slice(pixel);
    }
    let detailed = crate::gpu::detail_gpu::run_detail_tiled_with_runtime(
        &gpu.detail_runtime,
        &gpu.device,
        &gpu.queue,
        &rgb,
        w as u32,
        h as u32,
        &params,
        linear,
        luma_coeff,
    );
    for (pixel, rgb) in pixels.iter_mut().zip(detailed.chunks_exact(3)) {
        *pixel = [rgb[0], rgb[1], rgb[2]];
    }
}

impl App {
    /// Screen rect `(x0, y0, x1, y1)` (physical px), view offset and zoom the
    /// Develop preview is displayed at: the Develop window's own viewport while
    /// it is open, else the main canvas view.
    fn develop_display_view(&mut self) -> Option<((f32, f32, f32, f32), (f32, f32), f32)> {
        if self.win.develop_window.is_some() {
            self.develop_resolve_fit();
            let rect = self.develop_viewport_rect()?;
            return Some((rect, self.dev.develop_view_off, self.dev.develop_view_zoom));
        }
        let (sx, sy, sw, sh) = self.canvas_screen_clip()?;
        Some((
            (sx as f32, sy as f32, (sx + sw) as f32, (sy + sh) as f32),
            (self.edit.view.offset_x, self.edit.view.offset_y),
            self.edit.view.zoom,
        ))
    }

    fn develop_detail_view_sig(&mut self) -> Option<[u32; 7]> {
        let ((x0, y0, x1, y1), (ox, oy), zoom) = self.develop_display_view()?;
        Some([
            x0.to_bits(),
            y0.to_bits(),
            x1.to_bits(),
            y1.to_bits(),
            ox.to_bits(),
            oy.to_bits(),
            zoom.to_bits(),
        ])
    }

    /// The GPU Detail plane request for what the Develop view currently shows.
    fn plan_develop_detail(
        &mut self,
        layer_id: u32,
        raw_scene: bool,
        scene_tone: Option<&crate::core::develop_scene::SceneToneData>,
    ) -> Option<crate::gpu::compositor::DevelopDetailGpu> {
        let (src_w, src_h) = {
            let preview = self.dev.develop_preview.as_ref()?;
            (preview.original_tiles.width, preview.original_tiles.height)
        };
        let layer_offset = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == layer_id)
            .map(|l| l.offset)
            .unwrap_or((0, 0));
        let ((x0, y0, x1, y1), (off_x, off_y), zoom) = self.develop_display_view()?;
        let zoom = zoom.max(0.0001);
        let to_layer = |sx: f32, off: f32, layer_off: i32| (sx - off) / zoom - layer_off as f32;
        let lx0 = to_layer(x0, off_x, layer_offset.0)
            .floor()
            .clamp(0.0, src_w as f32) as u32;
        let ly0 = to_layer(y0, off_y, layer_offset.1)
            .floor()
            .clamp(0.0, src_h as f32) as u32;
        let lx1 = to_layer(x1, off_x, layer_offset.0)
            .ceil()
            .clamp(0.0, src_w as f32) as u32;
        let ly1 = to_layer(y1, off_y, layer_offset.1)
            .ceil()
            .clamp(0.0, src_h as f32) as u32;
        if lx1 <= lx0 || ly1 <= ly0 {
            return None;
        }
        let (origin_x, origin_y, end_x, end_y, downsample) = plan_gpu_detail_region(
            (lx0, ly0, lx1, ly1),
            src_w,
            src_h,
            zoom,
            GPU_DETAIL_MAX_PLANE_PX,
        );
        let luma_coeff = if raw_scene {
            scene_tone?.working_space.render_luminance_coefficients()
        } else {
            [0.2126, 0.7152, 0.0722]
        };
        Some(crate::gpu::compositor::DevelopDetailGpu {
            origin_x,
            origin_y,
            end_x,
            end_y,
            downsample,
            linear: raw_scene,
            luma_coeff,
        })
    }

    pub fn flush_develop_gpu_preview(&mut self) {
        // Mode A zoom/pan is a re-blit only; move the GPU Detail plane along
        // with the view it was planned for.
        if let Some(planned) = self.dev.develop_detail_view {
            if self.develop_detail_view_sig() != Some(planned) {
                self.dev.develop_gpu_preview_dirty = true;
            }
        }
        if !self.dev.develop_gpu_preview_dirty {
            return;
        }

        if !self.dev.develop_gpu_preview_immediate {
            if let Some(last) = self.dev.develop_gpu_recompose_last {
                let interval = self.dev.develop_gpu_recompose_cost.mul_f32(1.25).clamp(
                    std::time::Duration::from_millis(24),
                    std::time::Duration::from_millis(140),
                );
                let elapsed = last.elapsed();
                if elapsed < interval {
                    let deadline = std::time::Instant::now() + (interval - elapsed);
                    self.win.egui_repaint_deadline = Some(
                        self.win
                            .egui_repaint_deadline
                            .map_or(deadline, |d| d.min(deadline)),
                    );
                    return;
                }
            }
        }

        self.dev.develop_gpu_preview_dirty = false;
        self.dev.develop_gpu_preview_immediate = false;
        let start = std::time::Instant::now();
        self.recomposite();
        let end = std::time::Instant::now();
        self.dev.develop_gpu_recompose_cost = end.duration_since(start);
        self.dev.develop_gpu_recompose_last = Some(end);
    }

    /// Build the Develop GPU-preview payload for this frame (or `None` when the
    /// preview is inactive / selection-gated / no GPU). The expensive region proxies
    /// (which depend only on the tone stage) are cached in `develop_proxy_cache`
    /// and reused across a Colour/Shadows drag; only the cheap `adjusted` proxy is
    /// recomputed each frame.
    pub(in crate::app) fn build_develop_gpu_preview(
        &mut self,
    ) -> Option<crate::gpu::compositor::DevelopGpuPreview> {
        use crate::core::develop;

        self.dev.develop_detail_view = None;
        if self.win.gpu.is_none() {
            self.dev.develop_proxy_cache = None;
            return None;
        }
        let (layer_id, gpu_active, doc_matches) = match &self.dev.develop_preview {
            Some(p) => (
                p.layer_id,
                p.gpu_preview_active,
                p.doc_id == self.docs.documents[self.docs.active_doc_idx].id,
            ),
            None => {
                self.dev.develop_proxy_cache = None;
                return None;
            }
        };
        let active = gpu_active
            && doc_matches
            && self.shell.ui.show_develop_dialog
            && !self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .selection
                .active
            && !self.shell.ui.develop_settings.is_neutral();
        if !active {
            self.dev.develop_proxy_cache = None;
            return None;
        }

        let settings = self.shell.ui.develop_settings.clone();
        // Scene-referred (RAW) session: proxies are built from the linear f16
        // master through the scene chain; legacy sessions keep the display path.
        let scene = self
            .dev
            .develop_preview
            .as_ref()
            .and_then(|p| p.scene.clone());
        let scene_tone = scene
            .as_ref()
            .map(|sc| crate::core::develop_scene::build_scene_tone_for_scene(&settings, sc));
        // RAW colours in the scene chain; an Identity (JPEG/PNG) scene commits
        // Colour/Effects/Detail/Local through the display-domain bake, so its
        // proxies follow that chain instead.
        let raw_scene = scene
            .as_ref()
            .is_some_and(|sc| sc.look == crate::core::develop_scene::BaseLook::Raw);
        // Detail and Local reuse the same CPU kernels on a reduced-resolution
        // viewport proxy. They must not suppress the colour preview — the old
        // `&& !need_detail` gate made every Colour/Mixer edit vanish as soon as
        // a Detail slider was touched.
        // RAW colour always runs through the scene shader. Interaction may
        // reduce the sampled scene texture's resolution, but must never swap
        // to the old chroma-reconstruction model while the pointer is held:
        // changing models on release was the visible brightness/chroma jump.
        let linear_scene_color = raw_scene && raw_color_runs_per_pixel(&settings);
        // Detail on the GPU (mode 5): the compositor evaluates the ordinary
        // shader chain into a plane and runs the Detail kernels on it, so no
        // CPU proxy is needed. Spatial Effects/Locals still need the CPU chain
        // ahead of Detail, and a software adapter runs compute slower than
        // the CPU does.
        let software_adapter = self.win.gpu.as_ref().is_some_and(|g| g.software_adapter);
        let gpu_detail = settings.has_detail()
            && !settings.has_locals()
            && !settings.has_spatial_effects()
            && settings.vignette.abs() <= 0.001
            && !software_adapter
            && scene.as_ref().is_some_and(|sc| {
                self.win
                    .gpu
                    .as_ref()
                    .is_some_and(|g| g.compositor.scene_fits_texture(sc))
            });
        let cpu_detail = settings.has_detail() && !gpu_detail;
        let needs_spatial_proxy = settings.texture.abs() > 0.001
            || settings.clarity.abs() > 0.001
            || settings.dehaze.abs() > 0.001
            || settings.vignette.abs() > 0.001
            || cpu_detail
            || settings.has_locals();
        let need_fast = needs_spatial_proxy;
        let need_color = settings.has_color() && !linear_scene_color && !need_fast;
        // The fast low-res proxy carries the complete chain whenever a spatial
        // stage needs neighbourhood pixels. Its tail samples the same regional
        // luma/E proxy as the shader-only path, so stacking Shadows/Highlights
        // with Detail does not silently switch local adaptation to global tone.
        // Spatial Effects, Detail, and Local all run through this viewport proxy.
        // Detail requests a five-tap anti-aliased base below, avoiding the thin-edge
        // aliasing that made the earlier point-sampled attempt bead magenta/cyan.
        // Region-luma (regional Shadows/Highlights/Whites/Blacks adaptation) is
        // built under both Colour and fast spatial paths; each composes local
        // tone before the remaining stages.
        let need_local = settings.has_local_tone();
        let tone = (scene.is_none() && develop::tone_is_active(&settings))
            .then(|| develop::build_tone_data(&settings));
        let viewport_key = if need_fast || need_color {
            let preview = self
                .dev
                .develop_preview
                .as_ref()
                .expect("checked Some above");
            let src_w = preview.original_tiles.width;
            let src_h = preview.original_tiles.height;
            let layer_offset = self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack
                .layers
                .iter()
                .find(|l| l.id == layer_id)
                .map(|l| l.offset)
                .unwrap_or((0, 0));
            let (sx, sy, sw, sh) = self.canvas_screen_clip().unwrap_or_else(|| {
                self.win
                    .window
                    .as_ref()
                    .map(|w| {
                        let sz = w.inner_size();
                        (0, 0, sz.width.max(1), sz.height.max(1))
                    })
                    .unwrap_or((0, 0, 1, 1))
            });
            let zoom = self.edit.view.zoom.max(0.0001);
            let lx0 = ((sx as f32 - self.edit.view.offset_x) / zoom - layer_offset.0 as f32)
                .floor()
                .clamp(0.0, src_w as f32) as u32;
            let ly0 = ((sy as f32 - self.edit.view.offset_y) / zoom - layer_offset.1 as f32)
                .floor()
                .clamp(0.0, src_h as f32) as u32;
            let lx1 = (((sx + sw) as f32 - self.edit.view.offset_x) / zoom - layer_offset.0 as f32)
                .ceil()
                .clamp(0.0, src_w as f32) as u32;
            let ly1 = (((sy + sh) as f32 - self.edit.view.offset_y) / zoom - layer_offset.1 as f32)
                .ceil()
                .clamp(0.0, src_h as f32) as u32;
            let mut rw = lx1.saturating_sub(lx0).max(1);
            let mut rh = ly1.saturating_sub(ly0).max(1);
            let downsample = if cpu_detail {
                // Native-resolution viewport Detail is the WYSIWYG path. Bound
                // the uploaded RGB proxy by both the adapter's storage-binding
                // limit and a CPU-latency ceiling; zoomed-out views keep the
                // existing reduced proxy until the compositor-native capture
                // path can cover them without a giant host allocation.
                let storage_pixels = self
                    .win
                    .gpu
                    .as_ref()
                    .map(|gpu| {
                        (gpu.device.limits().max_storage_buffer_binding_size as u64 / 12)
                            .saturating_mul(4)
                            / 5
                    })
                    .unwrap_or(0)
                    .min(4_000_000);
                let apron = 2 * develop::DETAIL_HALO as u64;
                let padded_pixels = (rw as u64 + apron).saturating_mul(rh as u64 + apron);
                if padded_pixels <= storage_pixels {
                    1
                } else {
                    develop::detail_preview_downsample(rw, rh)
                }
            } else {
                develop::fast_preview_downsample(rw, rh)
            } as u32;
            // The apron must cover Detail's widest dependency so the viewport
            // crop matches the whole-image commit up to its visible edge.
            let min_pad = if cpu_detail {
                develop::DETAIL_HALO as u32
            } else {
                64
            };
            let pad = downsample.saturating_mul(4).max(min_pad);
            let ox = lx0.saturating_sub(pad).min(src_w.saturating_sub(1));
            let oy = ly0.saturating_sub(pad).min(src_h.saturating_sub(1));
            let ex = lx1.saturating_add(pad).min(src_w).max(ox.saturating_add(1));
            let ey = ly1.saturating_add(pad).min(src_h).max(oy.saturating_add(1));
            rw = ex.saturating_sub(ox).max(1);
            rh = ey.saturating_sub(oy).max(1);
            Some((ox, oy, rw, rh, downsample, src_w, src_h))
        } else {
            None
        };

        // Every cached base is tone-INDEPENDENT — tone/WB/Exposure are re-applied
        // per frame from it — so a slider drag never invalidates the cache; only a
        // layer or viewport (zoom/pan) change does.
        let region_matches = |f: &crate::app::state::DevelopRegionCache| {
            viewport_key.is_some_and(|(ox, oy, rw, rh, downsample, src_w, src_h)| {
                f.origin_x == ox
                    && f.origin_y == oy
                    && f.source_w == src_w
                    && f.source_h == src_h
                    && f.downsample == downsample
                    && (f.w as u32) == rw.div_ceil(downsample.max(1))
                    && (f.h as u32) == rh.div_ceil(downsample.max(1))
            })
        };

        // `cache_exact` = the bases cover the CURRENT viewport; `cache_usable` =
        // the bases exist for this layer but may be stale geometry (the viewport
        // moved since they were built).
        let cache_exact = self.dev.develop_proxy_cache.as_ref().is_some_and(|c| {
            c.layer_id == layer_id
                && (!need_color || c.color_region.as_ref().is_some_and(region_matches))
                && (!need_local || c.region_luma_base.is_some())
                && (!need_fast || c.fast_region.as_ref().is_some_and(region_matches))
        });
        let cache_usable = self.dev.develop_proxy_cache.as_ref().is_some_and(|c| {
            c.layer_id == layer_id
                && (!need_color || c.color_region.is_some())
                && (!need_local || c.region_luma_base.is_some())
                && (!need_fast || c.fast_region.is_some())
        });

        // A zoom/pan drag changes the viewport key on every recompose; rebuilding
        // the full-region-read bases at that cadence is what made zooming a large
        // RAW lag with the Mixer engaged. While a usable (right layer, stale
        // geometry) cache exists, reuse it and defer the rebuild until the last
        // rebuild's cost has cleared — the shader clamps to the stale proxy's
        // coverage, so newly-revealed edges are briefly approximate and the
        // trailing recompose below swaps in the exact bases once the view rests.
        let mut throttle = false;
        if cache_usable && !cache_exact {
            if let Some(last) = self.dev.develop_proxy_last {
                // Generous interval: the stale proxy looks fine while the view is
                // in motion, so rebuild sparsely mid-gesture and let the trailing
                // recompose land the exact bases when the view rests.
                let interval = self.dev.develop_proxy_cost.mul_f32(3.0).clamp(
                    std::time::Duration::from_millis(48),
                    std::time::Duration::from_millis(400),
                );
                let elapsed = last.elapsed();
                if elapsed < interval {
                    let deadline = std::time::Instant::now() + (interval - elapsed);
                    self.win.egui_repaint_deadline = Some(
                        self.win
                            .egui_repaint_deadline
                            .map_or(deadline, |d| d.min(deadline)),
                    );
                    // A pure view change doesn't re-enter this path on its own
                    // once the gesture stops — flag the preview dirty so
                    // `flush_develop_gpu_preview` recomposes after the deadline
                    // and the stale proxies get replaced.
                    self.dev.develop_gpu_preview_dirty = true;
                    throttle = true;
                }
            }
        }

        if !cache_exact && !throttle {
            let start = std::time::Instant::now();
            let reusable_cache = self
                .dev
                .develop_proxy_cache
                .as_ref()
                .filter(|c| c.layer_id == layer_id);
            // The colour base is tone-INDEPENDENT now, so it is reusable whenever it
            // covers the current viewport — no tone_sig gate.
            let reusable_color_region = reusable_cache.and_then(|c| {
                c.color_region
                    .as_ref()
                    .filter(|r| region_matches(r))
                    .cloned()
            });
            let reusable_region_luma_base = reusable_cache.and_then(|c| c.region_luma_base.clone());
            let luma_base_carried = reusable_region_luma_base.is_some();
            let reusable_fast_region = reusable_cache.and_then(|c| {
                c.fast_region
                    .as_ref()
                    .filter(|r| region_matches(r))
                    .cloned()
            });
            let (color_region, region_luma_base, fast_region) = {
                let src = &self
                    .dev
                    .develop_preview
                    .as_ref()
                    .expect("checked Some above")
                    .original_tiles;
                let color_region = if need_color && reusable_color_region.is_none() {
                    let (ox, oy, rw, rh, downsample, src_w, src_h) =
                        viewport_key.expect("need_color builds a viewport key");
                    // Cache only the tone-INDEPENDENT box-average (same de-blocking base
                    // the commit uses). Tone + guided low-pass + colour are applied per
                    // frame from this base (see below), so a Tone/Curve drag tracks the
                    // fresh tone instead of reusing a stale-tone colour region.
                    let (region, w, h) = match &scene {
                        Some(sc) => crate::core::develop_scene::build_scene_color_base_box(
                            sc,
                            ox,
                            oy,
                            rw,
                            rh,
                            downsample as usize,
                        ),
                        None => {
                            develop::build_color_base_box(src, ox, oy, rw, rh, downsample as usize)
                        }
                    };
                    Some(crate::app::state::DevelopRegionCache {
                        region: std::sync::Arc::new(region),
                        w,
                        h,
                        origin_x: ox,
                        origin_y: oy,
                        source_w: src_w,
                        source_h: src_h,
                        downsample,
                    })
                } else {
                    reusable_color_region
                };
                let fast_region = if need_fast && reusable_fast_region.is_none() {
                    let (ox, oy, rw, rh, downsample, src_w, src_h) =
                        viewport_key.expect("need_fast builds a viewport key");
                    let (region, w, h) = match &scene {
                        Some(sc) => crate::core::develop_scene::build_scene_fast_base(
                            sc,
                            ox,
                            oy,
                            rw,
                            rh,
                            downsample as usize,
                            cpu_detail,
                        ),
                        None => develop::build_fast_preview_region(
                            src,
                            &None,
                            ox,
                            oy,
                            rw,
                            rh,
                            downsample as usize,
                            cpu_detail,
                        ),
                    };
                    Some(crate::app::state::DevelopRegionCache {
                        region: std::sync::Arc::new(region),
                        w,
                        h,
                        origin_x: ox,
                        origin_y: oy,
                        source_w: src_w,
                        source_h: src_h,
                        downsample,
                    })
                } else {
                    reusable_fast_region
                };
                // Cache only the tone-INDEPENDENT full-image block average; WB+Exposure
                // + guided low-pass are applied per frame (see below), so an Exposure/WB
                // drag no longer rebuilds this full-image proxy.
                let region_luma_base = if need_local && reusable_region_luma_base.is_none() {
                    let (base, w, h) = match &scene {
                        Some(sc) => crate::core::develop_scene::build_scene_region_base(
                            sc,
                            develop::TONE_DOWNSAMPLE,
                        ),
                        None => develop::build_region_luma_base(src, develop::TONE_DOWNSAMPLE),
                    };
                    Some(crate::app::state::DevelopRegionCache {
                        region: std::sync::Arc::new(base),
                        w,
                        h,
                        origin_x: 0,
                        origin_y: 0,
                        source_w: src.width,
                        source_h: src.height,
                        downsample: develop::TONE_DOWNSAMPLE as u32,
                    })
                } else {
                    reusable_region_luma_base
                };
                (color_region, region_luma_base, fast_region)
            };
            // The local-tone base is viewport-independent: when it survives a
            // viewport rebuild its finished E-plane is still valid — carry that
            // memo over so a zoom/pan doesn't re-run the guided filter (the
            // priciest per-frame stage on a large RAW).
            let (region_luma_sig, region_luma) = if luma_base_carried {
                self.dev
                    .develop_proxy_cache
                    .as_ref()
                    .map(|c| (c.region_luma_sig, c.region_luma.clone()))
                    .unwrap_or(([0; 3], None))
            } else {
                // Filled/memoised by the block just below (keyed on WB+Exposure).
                ([0; 3], None)
            };
            self.dev.develop_proxy_cache = Some(crate::app::state::DevelopProxyCache {
                layer_id,
                region_luma_base,
                region_luma_sig,
                region_luma,
                color_region,
                fast_region,
                finished_color: None,
                finished_settings: None,
                identity_fast: Default::default(),
                raw_fast: Default::default(),
            });
            self.dev.develop_proxy_cost = start.elapsed();
            self.dev.develop_proxy_last = Some(std::time::Instant::now());
        }

        // Finish the local-adaptation base luma from the cached raw base, memoised on
        // WB+Exposure (its only inputs). So an Exposure/WB drag recomputes it every
        // frame — cheap (no full-image read) and UNthrottled, so it never lags the
        // per-pixel tone (that lag was the Exposure "nhảy loạn") — while a
        // Shadows/Contrast/Curve drag leaves WB+Exposure untouched and reuses it.
        if need_local {
            let wb_ev_sig = [
                settings.temperature.to_bits(),
                settings.tint.to_bits(),
                settings.exposure.to_bits(),
            ];
            let cache = self.dev.develop_proxy_cache.as_mut().unwrap();
            if cache.region_luma.is_none() || cache.region_luma_sig != wb_ev_sig {
                let base = cache.region_luma_base.as_ref().unwrap();
                let (bw, bh, bds) = (base.w, base.h, base.downsample);
                let data = match &scene_tone {
                    Some(st) => crate::core::develop_scene::finish_region_e(
                        &base.region,
                        bw,
                        bh,
                        st,
                        bds as usize,
                    ),
                    None => {
                        let t = tone.as_ref().expect("local tone implies tone active");
                        develop::finish_region_luma(&base.region, bw, bh, t, bds as usize)
                    }
                };
                cache.region_luma = Some(crate::gpu::compositor::RegionLumaProxy {
                    data: std::sync::Arc::new(data),
                    w: bw,
                    h: bh,
                    downsample: bds,
                });
                cache.region_luma_sig = wb_ev_sig;
            }
        }

        let (mut identity_stages, mut raw_stages) = {
            let cache = self.dev.develop_proxy_cache.as_mut().unwrap();
            (
                std::mem::take(&mut cache.identity_fast),
                std::mem::take(&mut cache.raw_fast),
            )
        };
        let cache = self.dev.develop_proxy_cache.as_ref().unwrap();
        // A pure view recompose (zoom/pan — settings untouched) reuses the
        // finished proxies outright; the per-frame tails below only re-run when
        // a slider actually moved. The memo is cleared on every base rebuild, so
        // a hit always refers to the bases currently in the cache.
        // With GPU Detail the proxies are the shader-path ones, which never read
        // the Detail sliders: a Detail drag keeps them.
        let finished_ok = cache
            .finished_color
            .as_ref()
            .is_some_and(|c| !(gpu_detail && c.fast_preview))
            && cache.finished_settings.as_ref().is_some_and(|s| {
                if gpu_detail {
                    s.same_image_effect(&with_detail_of(&settings, s))
                } else {
                    s.same_image_effect(&settings)
                }
            });
        let color = if !(need_color || need_fast) {
            None
        } else if finished_ok {
            cache.finished_color.clone()
        } else {
            let built = if need_color {
                let color_region = cache.color_region.as_ref().unwrap();
                // Apply the CURRENT tone to the cached raw base, then colour — so the
                // preview's tone base tracks the shader's per-pixel tone every frame.
                // Only a RAW scene applies guided control planes per pixel.
                let toned_samples = scene_tone.as_ref().filter(|_| raw_scene).map(|st| {
                    crate::core::develop_scene::tone_scene_color_samples(&color_region.region, st)
                });
                let region = std::sync::Arc::new(match &scene_tone {
                    Some(st) => crate::core::develop_scene::tone_lowpass_scene_region(
                        &color_region.region,
                        color_region.w,
                        color_region.h,
                        st,
                        color_region.downsample as usize,
                    ),
                    None => develop::tone_lowpass_color_region(
                        &color_region.region,
                        color_region.w,
                        color_region.h,
                        &tone,
                        color_region.downsample as usize,
                    ),
                });
                let guided_controls = toned_samples.as_ref().and_then(|samples| {
                    develop::guided_mixer_controls(
                        samples,
                        &settings,
                        color_region.w,
                        color_region.h,
                    )
                });
                let uses_guided_controls = guided_controls.is_some();
                let adjusted = guided_controls.unwrap_or_else(|| {
                    develop::apply_color_to_region(
                        &region,
                        &settings,
                        color_region.w,
                        color_region.h,
                    )
                });
                crate::gpu::compositor::ColorProxies {
                    region,
                    adjusted: std::sync::Arc::new(adjusted),
                    w: color_region.w,
                    h: color_region.h,
                    origin_x: color_region.origin_x,
                    origin_y: color_region.origin_y,
                    downsample: color_region.downsample,
                    fast_preview: false,
                    guided_controls: uses_guided_controls,
                    exact_detail: false,
                }
            } else {
                let fast = cache.fast_region.as_ref().unwrap();
                let exact_detail = cpu_detail && fast.downsample == 1;
                // A software adapter emulates compute on the CPU far slower
                // than the native Detail kernels.
                let gpu_exact = exact_detail && !software_adapter;
                let regional = cache
                    .region_luma
                    .as_ref()
                    .map(|r| (r.data.as_slice(), r.w, r.h, r.downsample));
                let (region, adjusted) = match &scene_tone {
                    Some(st) if !raw_scene => {
                        let gpu = self.win.gpu.as_ref().expect("GPU preview checked above");
                        crate::core::develop_scene::identity_fast_region_develop_staged(
                            &mut identity_stages,
                            &fast.region,
                            st,
                            &settings,
                            regional,
                            fast.w,
                            fast.h,
                            fast.origin_x,
                            fast.origin_y,
                            fast.source_w,
                            fast.source_h,
                            fast.downsample,
                            |pixels| {
                                if gpu_exact {
                                    run_native_gpu_detail(
                                        gpu,
                                        pixels,
                                        fast.w,
                                        fast.h,
                                        &settings,
                                        false,
                                        [0.2126, 0.7152, 0.0722],
                                    );
                                } else {
                                    develop::apply_detail_to_display_buffer(
                                        pixels,
                                        fast.w,
                                        fast.h,
                                        &settings,
                                        fast.downsample.max(1),
                                    );
                                }
                            },
                        )
                    }
                    Some(st) => {
                        let gpu = self.win.gpu.as_ref().expect("GPU preview checked above");
                        crate::core::develop_scene::scene_fast_region_develop_staged(
                            &mut raw_stages,
                            &fast.region,
                            st,
                            &settings,
                            regional,
                            fast.w,
                            fast.h,
                            fast.origin_x,
                            fast.origin_y,
                            fast.source_w,
                            fast.source_h,
                            fast.downsample,
                            |working, working_space| {
                                if gpu_exact {
                                    run_native_gpu_detail(
                                        gpu,
                                        working,
                                        fast.w,
                                        fast.h,
                                        &settings,
                                        true,
                                        working_space.render_luminance_coefficients(),
                                    );
                                } else {
                                    develop::apply_detail_to_working_buffer_in_space(
                                        working,
                                        fast.w,
                                        fast.h,
                                        &settings,
                                        working_space,
                                        fast.downsample.max(1),
                                    );
                                }
                            },
                        )
                    }
                    None if exact_detail => {
                        let gpu = self.win.gpu.as_ref().expect("GPU preview checked above");
                        let region = fast.region.clone();
                        let adjusted = develop::apply_fast_preview_to_region_with_detail(
                            &region,
                            &settings,
                            regional,
                            fast.w,
                            fast.h,
                            fast.origin_x,
                            fast.origin_y,
                            fast.source_w,
                            fast.source_h,
                            fast.downsample,
                            |pixels| {
                                if gpu_exact {
                                    run_native_gpu_detail(
                                        gpu,
                                        pixels,
                                        fast.w,
                                        fast.h,
                                        &settings,
                                        false,
                                        [0.2126, 0.7152, 0.0722],
                                    );
                                } else {
                                    develop::apply_detail_to_display_buffer(
                                        pixels, fast.w, fast.h, &settings, 1,
                                    );
                                }
                            },
                        );
                        (region, adjusted)
                    }
                    None => {
                        let region = fast.region.clone();
                        let adjusted = develop::apply_fast_preview_to_region(
                            &region,
                            &settings,
                            regional,
                            fast.w,
                            fast.h,
                            fast.origin_x,
                            fast.origin_y,
                            fast.source_w,
                            fast.source_h,
                            fast.downsample,
                        );
                        (region, adjusted)
                    }
                };
                crate::gpu::compositor::ColorProxies {
                    region,
                    adjusted: std::sync::Arc::new(adjusted),
                    w: fast.w,
                    h: fast.h,
                    origin_x: fast.origin_x,
                    origin_y: fast.origin_y,
                    downsample: fast.downsample,
                    fast_preview: true,
                    guided_controls: false,
                    exact_detail,
                }
            };
            let cache = self.dev.develop_proxy_cache.as_mut().unwrap();
            cache.identity_fast = std::mem::take(&mut identity_stages);
            cache.raw_fast = std::mem::take(&mut raw_stages);
            cache.finished_color = Some(built.clone());
            cache.finished_settings = Some(settings.clone());
            Some(built)
        };
        let region_luma = if need_local {
            self.dev
                .develop_proxy_cache
                .as_ref()
                .unwrap()
                .region_luma
                .clone()
        } else {
            None
        };

        let detail = if gpu_detail {
            self.plan_develop_detail(layer_id, raw_scene, scene_tone.as_ref())
        } else {
            None
        };
        if detail.is_some() {
            self.dev.develop_detail_view = self.develop_detail_view_sig();
        }

        Some(crate::gpu::compositor::DevelopGpuPreview {
            layer_id,
            settings,
            region_luma,
            color,
            scene,
            detail,
        })
    }
}

#[cfg(test)]
mod gpu_detail_region_tests {
    use super::plan_gpu_detail_region;
    use crate::core::develop::DETAIL_HALO;

    #[test]
    fn plane_covers_the_view_plus_apron_at_display_density() {
        let halo = DETAIL_HALO as u32;
        // 100 %: native density, apron on every side, clipped at the image.
        let (ox, oy, ex, ey, ds) =
            plan_gpu_detail_region((500, 10, 2100, 1010), 6000, 4000, 1.0, 4_000_000);
        assert_eq!(ds, 1);
        assert_eq!((ox, oy, ex, ey), (500 - halo, 0, 2100 + halo, 1010 + halo));

        // 25 %: one texel per 4 px, origin on the texel grid, apron scaled.
        let (ox, oy, ex, ey, ds) =
            plan_gpu_detail_region((1003, 777, 6000, 4000), 6000, 4000, 0.25, 4_000_000);
        assert_eq!(ds, 4);
        assert_eq!((ox % 4, oy % 4), (0, 0));
        assert!(ox + 4 * halo <= 1003 && oy + 4 * halo <= 777);
        assert_eq!((ex, ey), (6000, 4000));

        // A view larger than the budget coarsens instead of overflowing it.
        let (ox, oy, ex, ey, ds) =
            plan_gpu_detail_region((0, 0, 3840, 2160), 8000, 6000, 1.0, 4_000_000);
        assert!(ds >= 2);
        let pixels = ((ex - ox).div_ceil(ds) as u64) * ((ey - oy).div_ceil(ds) as u64);
        assert!(pixels <= 4_000_000);
    }
}

#[cfg(test)]
mod phase6_native_interaction_tests {
    use super::raw_color_runs_per_pixel;
    use crate::core::develop::{DevelopEngineVersion, DevelopSettings};

    #[test]
    fn raw_color_uses_one_per_pixel_model_for_drag_and_release() {
        let mut s = DevelopSettings::default();
        s.mixer_saturation[0] = 50.0;
        s.develop_engine_version = DevelopEngineVersion::Scene1;
        assert!(raw_color_runs_per_pixel(&s));
        s.develop_engine_version = DevelopEngineVersion::Develop3;
        assert!(!raw_color_runs_per_pixel(&s));
        s.mixer_saturation = [0.0; crate::core::develop::MIXER_BANDS];
        assert!(!raw_color_runs_per_pixel(&s));
        // Global Saturation alone has no guided planes: it must stay in the
        // scene shader rather than fall back to the display-domain proxies.
        s.saturation = 40.0;
        assert!(raw_color_runs_per_pixel(&s));
    }
}

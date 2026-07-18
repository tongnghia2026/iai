//! Building and flushing the Develop window's GPU preview.

use crate::app::state::App;

impl App {
    pub fn flush_develop_gpu_preview(&mut self) {
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
            .map(|sc| crate::core::develop_scene::build_scene_tone_for(&settings, sc.look));
        // Detail (Sharpening / Noise Reduction) is independent of the colour proxy: it
        // is full-resolution and previewed on commit only (see the note below). It must
        // NOT suppress the colour preview — the old `&& !need_detail` here made every
        // Colour/Mixer edit vanish (preview snapped back to the untouched image) the
        // moment a Detail slider was touched.
        let need_color = settings.has_color();
        // The fast (point-sampled, low-res) proxy carries tone+effects on a downsampled
        // buffer; only the spatial Effects/Detail stages actually need it. Tone is
        // applied EXACTLY per-pixel by the shader — global via the tone LUT, and
        // local-adaptation (Shadows/Highlights/Whites/Blacks) via the `region_luma`
        // proxy + local LUT below — matching the per-pixel CPU commit. Routing tone
        // through the fast proxy (the old `tone_is_active` term here) made the light
        // preview coarse and dropped local adaptation, so it diverged from the commit
        // (the "sáng/tối" preview↔commit jump). Keep tone out of the fast path.
        // Detail (Sharpening / Noise Reduction) is NOT previewed via the proxy — it is
        // full-resolution and the proxy version beaded thin edges (see
        // apply_fast_preview_to_region); it applies on commit. So only the Effects
        // group drives the fast proxy here.
        let need_fast = !need_color
            && (settings.texture.abs() > 0.001
                || settings.clarity.abs() > 0.001
                || settings.dehaze.abs() > 0.001
                || settings.vignette.abs() > 0.001);
        // Region-luma (regional Shadows/Highlights/Whites/Blacks adaptation) is now
        // built under the Colour Mixer too, not only for tone-only edits — the shader
        // composes local tone THEN colour, so H/S/W/B stay regional when the Mixer
        // engages (no jump). Still skipped on the fast-effects path (it bakes tone
        // into its own proxy and returns before the local stage).
        let need_local = !need_fast && settings.has_local_tone();
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
            let downsample = develop::fast_preview_downsample(rw, rh) as u32;
            let pad = downsample.saturating_mul(4).max(64);
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
                        ),
                        None => develop::build_fast_preview_region(
                            src,
                            &None,
                            ox,
                            oy,
                            rw,
                            rh,
                            downsample as usize,
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

        let cache = self.dev.develop_proxy_cache.as_ref().unwrap();
        // A pure view recompose (zoom/pan — settings untouched) reuses the
        // finished proxies outright; the per-frame tails below only re-run when
        // a slider actually moved. The memo is cleared on every base rebuild, so
        // a hit always refers to the bases currently in the cache.
        let finished_ok = cache.finished_color.is_some()
            && cache
                .finished_settings
                .as_ref()
                .is_some_and(|s| s.same_image_effect(&settings));
        let color = if !(need_color || need_fast) {
            None
        } else if finished_ok {
            cache.finished_color.clone()
        } else {
            let built = if need_color {
                let color_region = cache.color_region.as_ref().unwrap();
                // Apply the CURRENT tone to the cached raw base, then colour — so the
                // preview's tone base tracks the shader's per-pixel tone every frame.
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
                let adjusted = develop::apply_color_to_region(
                    &region,
                    &settings,
                    color_region.w,
                    color_region.h,
                );
                crate::gpu::compositor::ColorProxies {
                    region,
                    adjusted: std::sync::Arc::new(adjusted),
                    w: color_region.w,
                    h: color_region.h,
                    origin_x: color_region.origin_x,
                    origin_y: color_region.origin_y,
                    downsample: color_region.downsample,
                    fast_preview: false,
                }
            } else {
                let fast = cache.fast_region.as_ref().unwrap();
                // Scene sessions: the cached base is LINEAR scene; run the scene
                // chain to display, then re-apply only the leftover (stripped)
                // stages — tone is already inside the chain.
                let (region, effective_settings) = match &scene_tone {
                    Some(st) => (
                        std::sync::Arc::new(crate::core::develop_scene::scene_fast_region_display(
                            &fast.region,
                            st,
                        )),
                        crate::core::develop_scene::strip_scene_handled(&settings),
                    ),
                    None => (fast.region.clone(), settings.clone()),
                };
                let adjusted = develop::apply_fast_preview_to_region(
                    &region,
                    &effective_settings,
                    fast.w,
                    fast.h,
                    fast.origin_x,
                    fast.origin_y,
                    fast.source_w,
                    fast.source_h,
                    fast.downsample,
                );
                crate::gpu::compositor::ColorProxies {
                    region,
                    adjusted: std::sync::Arc::new(adjusted),
                    w: fast.w,
                    h: fast.h,
                    origin_x: fast.origin_x,
                    origin_y: fast.origin_y,
                    downsample: fast.downsample,
                    fast_preview: true,
                }
            };
            let cache = self.dev.develop_proxy_cache.as_mut().unwrap();
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

        Some(crate::gpu::compositor::DevelopGpuPreview {
            layer_id,
            settings,
            region_luma,
            color,
            scene,
        })
    }
}

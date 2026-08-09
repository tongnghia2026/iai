//! Develop (RAW pre-editor) preview and commit pipeline. Split out of
//! app/actions.rs.

use crate::app::render::CanvasEvent;
use crate::app::state::App;
use crate::core::document::DocumentId;

/// Minimum spacing between live-histogram re-bins during a slider drag.
const DEVELOP_HISTOGRAM_REBIN: std::time::Duration = std::time::Duration::from_millis(80);

/// RAM guard for linearizing a non-RAW layer into an f16 Identity scene master
/// (8 bytes/px, held for the whole Develop session). 16384² — the worst case
/// the old GPU-texture gate already admitted (2 GiB); anything larger falls
/// back to the legacy display-domain engine.
const SCENE_IDENTITY_MAX_PIXELS: u64 = 16384 * 16384;

impl App {
    /// Re-bin the live histogram from the cached source proxy through `settings`.
    fn rebin_develop_histogram(&mut self, settings: &crate::core::develop::DevelopSettings) {
        let Some(preview) = &self.dev.develop_preview else {
            return;
        };
        self.dev.develop_histogram = Some(std::sync::Arc::new(match &preview.scene {
            Some(scene) => crate::core::develop_scene::histogram_rgbl_scene(
                &preview.histogram_proxy,
                settings,
                scene.look,
            ),
            None => crate::core::develop::histogram_rgbl(&preview.histogram_proxy, settings),
        }));
        self.dev.develop_histogram_at = Some(std::time::Instant::now());
        self.dev.develop_histogram_stale = false;
    }

    /// Trailing half of the histogram throttle: once the re-bin interval has
    /// cleared, land the resting settings' histogram (runs every UI frame).
    pub(super) fn flush_due_develop_histogram(&mut self) {
        if !self.dev.develop_histogram_stale || self.dev.develop_preview.is_none() {
            return;
        }
        let due = self
            .dev
            .develop_histogram_at
            .map_or(true, |t| t.elapsed() >= DEVELOP_HISTOGRAM_REBIN);
        if due {
            let settings = self.shell.ui.develop_settings.clone();
            self.rebin_develop_histogram(&settings);
        }
    }

    pub(super) fn clear_develop_gpu_commit_state(&mut self) {
        self.dev.develop_gpu_preview_dirty = false;
        self.dev.develop_gpu_preview_immediate = false;
        self.dev.develop_proxy_cache = None;
        self.dev.develop_proxy_last = None;
        if let Some(gpu) = &mut self.win.gpu {
            gpu.compositor.develop_preview = None;
            gpu.compositor.ping_initialized = false;
            gpu.compositor.last_result_is_ping = false;
        }
    }

    /// Enter the RAW Develop stage for freshly-decoded RAW documents (slice
    /// R2 / D3). Queued by the loaders and drained the next frame (after the
    /// load attaches) so the GPU document is fully wired before the live
    /// preview starts. The first RAW opens the Develop window on itself; later
    /// decodes of a multi-open batch join the session filmstrip WITHOUT
    /// stealing the active image (they land over seconds while the user may
    /// already be editing — click the thumbnail to switch).
    pub(crate) fn enter_pending_develop(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
    ) {
        // Mid-"Open Image" bake the session is sealed; hold the queue — the
        // decode enters a fresh session once the commit finishes. Likewise
        // while the old window is retiring (open_develop_window would no-op
        // and the id would be lost from the queue).
        if self.dev.develop_bake_all.is_some() || self.win.retiring_develop_window.is_some() {
            return;
        }
        while !self.dev.pending_develop.is_empty() {
            let id = self.dev.pending_develop.remove(0);
            let Some(idx) = self.docs.documents.iter().position(|d| d.id == id) else {
                continue;
            };
            if self.win.develop_window.is_some() {
                self.develop_session_push(id, true);
                let title = self.develop_window_title();
                if let Some(w) = &self.win.develop_window {
                    w.set_title(&title);
                    w.request_redraw();
                }
            } else {
                if self.docs.active_doc_idx != idx {
                    self.switch_to_doc(idx);
                }
                // Marking the entry transient makes Cancel discard the
                // document (pre-editor flow: Open Image is what creates it).
                // If the session can't start (no raster layer — not expected
                // for a decoded RAW) the document just opens as a normal image.
                self.open_develop_window(event_loop);
                if self.dev.develop_preview.is_some() {
                    self.develop_session_mark_transient(id);
                }
            }
        }
    }

    /// Record `doc` as part of the Develop session (no-op if already present).
    pub(crate) fn develop_session_push(&mut self, doc: DocumentId, transient: bool) {
        if !self.dev.develop_session.iter().any(|e| e.doc == doc) {
            self.dev
                .develop_session
                .push(crate::app::state::DevelopSessionEntry {
                    doc,
                    transient,
                    settings: crate::core::develop::DevelopSettings::default(),
                });
        }
    }

    /// Mark `doc`'s session entry as a transient RAW import (Cancel closes it).
    pub(crate) fn develop_session_mark_transient(&mut self, doc: DocumentId) {
        if let Some(e) = self.dev.develop_session.iter_mut().find(|e| e.doc == doc) {
            e.transient = true;
        }
    }

    /// Save the active image's live settings back into its session entry
    /// (called before switching filmstrip images or committing the session).
    pub(crate) fn develop_session_save_active_settings(&mut self) {
        let active = self.docs.documents[self.docs.active_doc_idx].id;
        let settings = self.shell.ui.develop_settings.clone();
        if let Some(e) = self
            .dev
            .develop_session
            .iter_mut()
            .find(|e| e.doc == active)
        {
            e.settings = settings;
        }
    }

    /// Make another filmstrip image the active one: save the current image's
    /// settings, restore the source tiles (cancel the live preview BEFORE the
    /// document switch — after it the cancel's doc guard would skip the
    /// restore), then start a session on the target with its saved settings.
    pub(crate) fn develop_session_activate(&mut self, doc: DocumentId) {
        if self.docs.documents[self.docs.active_doc_idx].id == doc {
            return;
        }
        let Some(idx) = self.docs.documents.iter().position(|d| d.id == doc) else {
            return;
        };
        let Some(settings) = self
            .dev
            .develop_session
            .iter()
            .find(|e| e.doc == doc)
            .map(|e| e.settings.clone())
        else {
            return;
        };
        self.develop_session_save_active_settings();
        self.cancel_develop_preview();
        self.switch_to_doc(idx);
        self.begin_develop_preview(settings);
        self.dev.develop_view_fit = true;
        self.dev.develop_composited_view = None;
        let title = self.develop_window_title();
        if let Some(w) = &self.win.develop_window {
            w.set_title(&title);
            w.request_redraw();
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// End the Develop session on Cancel: close every transient RAW document
    /// (nothing is left open — the pre-editor flow where Commit is what creates
    /// the document) and forget the session.
    pub(crate) fn discard_develop_session_docs(&mut self) {
        let ids: Vec<DocumentId> = self
            .dev
            .develop_session
            .iter()
            .filter(|e| e.transient)
            .map(|e| e.doc)
            .collect();
        for (key, id) in &self.jobs.raw_preview_docs {
            if ids.contains(id) {
                self.jobs.cancelled_raw_loads.insert(key.clone());
            }
        }
        self.jobs.raw_preview_docs.retain(|_, id| !ids.contains(id));
        self.jobs
            .raw_preview_failures
            .retain(|id, _| !ids.contains(id));
        self.dev.develop_session.clear();
        for id in ids {
            if let Some(idx) = self.docs.documents.iter().position(|d| d.id == id) {
                self.close_doc_confirmed(idx);
            }
        }
    }

    /// Drop a Develop session without touching its documents — used when
    /// another modal takes over while the fallback in-canvas dialog is up (the
    /// documents stay open; only the session bookkeeping is abandoned).
    pub(crate) fn abandon_develop_session(&mut self) {
        self.dev.develop_commit_after_refine = false;
        self.shell.ui.show_develop_dialog = false;
        self.cancel_develop_preview();
        self.dev.develop_session.clear();
    }

    /// Start the next queued "Open Image" bake on a worker (no-op while one is
    /// already in flight). Unusable images — closed document, locked or
    /// non-raster layer — are skipped. The source rules mirror
    /// `begin_develop_preview`; the identity linearize for non-RAW layers runs
    /// on the worker, off the UI thread.
    pub(crate) fn develop_bake_all_start_next(&mut self) {
        // The active image's live preview survives the commit (so the edited
        // look stays on screen through the bake). Its layer tiles may hold
        // CPU-preview pixels, so ITS bake must source the pristine tiles the
        // preview state kept aside.
        let preview_pristine = self
            .dev
            .develop_preview
            .as_ref()
            .map(|p| (p.doc_id, p.layer_id, p.original_tiles.clone()));
        loop {
            let Some(state) = &mut self.dev.develop_bake_all else {
                return;
            };
            if state.rx.is_some() {
                return;
            }
            let Some((doc_id, settings)) = state.pending.pop_front() else {
                return;
            };

            let Some(doc) = self.docs.documents.iter().find(|d| d.id == doc_id) else {
                continue;
            };
            let canvas = &doc.canvas;
            if canvas.layer_stack.layers.is_empty() {
                continue;
            }
            let idx = canvas
                .layer_stack
                .active_idx
                .min(canvas.layer_stack.layers.len() - 1);
            let layer = &canvas.layer_stack.layers[idx];
            if (!layer.is_background && layer.locked) || !layer.is_raster() {
                continue;
            }
            let layer_id = layer.id;
            let original_tiles = match &preview_pristine {
                Some((pdoc, player, tiles)) if *pdoc == doc_id && *player == layer_id => {
                    tiles.clone()
                }
                _ => layer.tiles.clone(),
            };
            let scene = if layer.is_background {
                canvas.develop_source.clone()
            } else {
                None
            };
            let identity_fits =
                (layer.width as u64) * (layer.height as u64) <= SCENE_IDENTITY_MAX_PIXELS;
            let selection = if canvas.selection.active {
                Some(crate::core::develop::DevelopSelection {
                    selection: std::sync::Arc::new(canvas.selection.clone()),
                    layer_offset: layer.offset,
                })
            } else {
                None
            };

            let (tx, rx) = std::sync::mpsc::channel();
            rayon::spawn(move || {
                let scene = scene.or_else(|| {
                    identity_fits.then(|| {
                        std::sync::Arc::new(
                            crate::core::develop_scene::SceneSource::from_display_tiles(
                                &original_tiles,
                            ),
                        )
                    })
                });
                let mut tiles = match &scene {
                    Some(sc) => {
                        crate::core::develop_scene::apply_scene_to_tilemap(sc, &settings, selection)
                    }
                    None => crate::core::develop::apply_to_tilemap_direct(
                        &original_tiles,
                        &settings,
                        selection,
                    ),
                };
                tiles.bump_all_revisions();
                let _ = tx.send(crate::app::state::DevelopBakeAllResult {
                    doc: doc_id,
                    layer_id,
                    original_tiles,
                    tiles,
                });
            });
            if let Some(state) = &mut self.dev.develop_bake_all {
                state.rx = Some(rx);
            }
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            if let Some(w) = &self.win.develop_window {
                w.request_redraw();
            }
            return;
        }
    }

    /// Land a finished "Open Image" bake as an undoable tiles change on its
    /// document, start the next one, and — when the queue drains — finish the
    /// whole commit (teardown + status). Pumped from both windows' redraws.
    pub(crate) fn poll_develop_bake_all(&mut self) {
        if self.dev.develop_bake_all.is_none() {
            return;
        }
        let result = self
            .dev
            .develop_bake_all
            .as_ref()
            .and_then(|s| s.rx.as_ref())
            .and_then(|rx| rx.try_recv().ok());
        if let Some(res) = result {
            if let Some(state) = &mut self.dev.develop_bake_all {
                state.rx = None;
                state.done += 1;
            }
            // The live preview kept the edited image on screen through this
            // bake; its baked tiles land right below, so drop it WITHOUT the
            // restore a cancel would do (which would flash the original and
            // then be overwritten anyway).
            if self
                .dev
                .develop_preview
                .as_ref()
                .is_some_and(|p| p.doc_id == res.doc)
            {
                self.dev.develop_preview = None;
                self.clear_develop_gpu_commit_state();
            }
            if let Some(idx) = self.docs.documents.iter().position(|d| d.id == res.doc) {
                let canvas = &mut self.docs.documents[idx].canvas;
                // Commit = baked (PTS semantics): the scene master is consumed.
                canvas.develop_source = None;
                if canvas.commit_layer_tiles_change(
                    res.layer_id,
                    res.original_tiles,
                    res.tiles,
                    "Develop",
                ) {
                    if idx == self.docs.active_doc_idx {
                        self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
                    }
                }
            }
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            if let Some(w) = &self.win.develop_window {
                w.request_redraw();
            }
        }
        if self
            .dev
            .develop_bake_all
            .as_ref()
            .map_or(false, |s| s.rx.is_none())
        {
            self.develop_bake_all_start_next();
        }
        let finished = self
            .dev
            .develop_bake_all
            .as_ref()
            .map_or(false, |s| s.rx.is_none() && s.pending.is_empty());
        if finished {
            let st = self
                .dev
                .develop_bake_all
                .take()
                .expect("checked Some above");
            self.finish_develop_bake_all(st.total_images, st.done, st.single_raw);
        }
    }

    pub(crate) fn begin_develop_preview(
        &mut self,
        settings: crate::core::develop::DevelopSettings,
    ) -> bool {
        self.cancel_develop_preview();
        self.dev.develop_readout = None;
        self.dev.develop_gpu_preview_dirty = false;
        self.dev.develop_gpu_preview_immediate = false;
        self.dev.develop_gpu_recompose_last = None;
        self.dev.develop_gpu_recompose_cost = std::time::Duration::ZERO;
        self.shell.ui.show_adjustment_dialog = false;
        self.cancel_adjustment_preview();
        self.cancel_adjustment_layer_edit();
        self.shell.ui.show_filter_dialog = false;
        self.cancel_filter_preview();

        let doc_id = self.docs.documents[self.docs.active_doc_idx].id;
        // A RAW placeholder (embedded-JPEG preview, decode still running or
        // failed) only DISPLAYS in the Develop window — controls are locked and
        // update_develop_preview is gated. Skip building the f16 identity
        // master for it (8 bytes/px + a full-image pass, discarded the moment
        // the real decode replaces the canvas and this preview restarts).
        let decode_placeholder = self.jobs.raw_preview_docs.values().any(|id| *id == doc_id)
            || self.jobs.raw_preview_failures.contains_key(&doc_id);
        // Commit means APPLIED (PTS semantics): every session starts from the
        // layer's current pixels with neutral sliders. Keeping the previous
        // settings is a preset's job, not the panel's.
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
            // A RAW document carries its linear scene master: the session then
            // runs the scene-referred chain. The master belongs to the whole
            // (single-layer) decoded document, so only the background layer
            // may claim it. Every OTHER raster layer (JPEG/PNG/16-bit TIFF,
            // or extra layers of a RAW doc) is linearized into an
            // Identity-look scene on session open, so it runs the SAME
            // linear-light chain — CAT16 WB, ×2^EV exposure, EV-domain tone
            // equalizer — instead of the legacy gamma-domain engine, whose
            // additive luma masks cut the harsh, non-smooth Light transitions
            // on JPEGs. Neutral semantics hold: the identity tone map
            // reproduces the source exactly on [0,1].
            let scene = if layer.is_background {
                canvas.develop_source.clone()
            } else {
                None
            };
            // Linearize regardless of GPU texture limits, so the scene-referred
            // chain is THE tone engine: a master too big for a device texture
            // (scene_fits_texture) simply previews through the full-res CPU
            // bake — the same trade the RAW path already makes — and without a
            // GPU the CPU bake ran anyway. The only guard is RAM: the f16
            // master costs 8 bytes/px and lives for the whole session, so
            // absurdly large layers (beyond the old 16384² texture worst case)
            // keep the legacy display-domain engine as an escape hatch.
            let identity_fits = !decode_placeholder
                && (layer.width as u64) * (layer.height as u64) <= SCENE_IDENTITY_MAX_PIXELS;
            let scene = scene.or_else(|| {
                identity_fits.then(|| {
                    std::sync::Arc::new(
                        crate::core::develop_scene::SceneSource::from_display_tiles(
                            &original_tiles,
                        ),
                    )
                })
            });
            let histogram_proxy = match &scene {
                Some(sc) => {
                    std::sync::Arc::new(crate::core::develop_scene::build_scene_histogram_proxy(sc))
                }
                None => std::sync::Arc::new(crate::core::develop::build_histogram_proxy(
                    &original_tiles,
                )),
            };
            crate::app::state::DevelopPreviewState {
                doc_id,
                layer_id: layer.id,
                original_tiles,
                scene,
                histogram_proxy,
                job_id: 0,
                processing: false,
                gpu_preview_active: false,
                pending_settings: None,
                last_preview_settings: crate::core::develop::DevelopSettings::default(),
                rx: None,
                detail_refine_at: None,
                detail_refine_waiting_for_release: false,
                detail_refine_settings: None,
            }
        };

        self.dev.develop_preview = Some(preview);
        self.rebin_develop_histogram(&settings);
        self.shell.ui.develop_settings = settings.clone();
        self.shell.ui.show_develop_dialog = true;
        if settings.is_neutral() {
            true
        } else {
            self.update_develop_preview(settings)
        }
    }

    pub(crate) fn update_develop_preview(
        &mut self,
        settings: crate::core::develop::DevelopSettings,
    ) -> bool {
        let active_id = self.docs.documents[self.docs.active_doc_idx].id;
        if self
            .jobs
            .raw_preview_docs
            .values()
            .any(|id| *id == active_id)
            || self.jobs.raw_preview_failures.contains_key(&active_id)
        {
            return false;
        }
        {
            let Some(preview) = &mut self.dev.develop_preview else {
                return false;
            };
            if self.docs.documents[self.docs.active_doc_idx].id != preview.doc_id {
                return false;
            }

            if settings.same_image_effect(&preview.last_preview_settings) {
                preview.pending_settings = None;
                return true;
            }
        }

        // Live histogram: re-bin the cached source proxy through the new settings
        // so the curve-editor backdrop follows the sliders. Throttled — binning
        // costs a few ms and a drag lands here every tick; the trailing flush in
        // `poll_develop_preview` re-bins the resting value.
        let due = self
            .dev
            .develop_histogram_at
            .map_or(true, |t| t.elapsed() >= DEVELOP_HISTOGRAM_REBIN);
        if due {
            self.rebin_develop_histogram(&settings);
        } else {
            self.dev.develop_histogram_stale = true;
            let deadline = self.dev.develop_histogram_at.expect("checked Some above")
                + DEVELOP_HISTOGRAM_REBIN;
            self.win.egui_repaint_deadline = Some(
                self.win
                    .egui_repaint_deadline
                    .map_or(deadline, |d| d.min(deadline)),
            );
        }

        // The whole Develop panel previews on the GPU when there is no selection.
        // Local-tone and Colour feed the shader region proxies (built in recomposite)
        // and Effects run per-pixel in the shader, so the live preview matches the
        // CPU bake. Selection-active edits and local-adjustment masks take the CPU
        // path (the shader neither blends the selection mask nor evaluates local
        // masks — the CPU path IS the commit bake, so preview = commit).
        // A scene (RAW) session additionally requires the f16 master to fit a
        // device texture; oversized masters fall back to the CPU bake.
        let scene_for_gpu = self
            .dev
            .develop_preview
            .as_ref()
            .expect("checked Some above")
            .scene
            .clone();
        let scene_fits_gpu = match (&self.win.gpu, &scene_for_gpu) {
            (Some(gpu), Some(sc)) => gpu.compositor.scene_fits_texture(sc),
            _ => true,
        };
        // A layer inside an isolated group is pre-flattened on the CPU for
        // rendering, so the shader-overlay preview (keyed to its id) never shows —
        // bake into the tiles instead, exactly like the selection-active case.
        let layer_id = self
            .dev
            .develop_preview
            .as_ref()
            .expect("checked Some above")
            .layer_id;
        let in_isolated_group = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .layer_in_isolated_group(layer_id);
        if self.win.gpu.is_some()
            && scene_fits_gpu
            && !self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .selection
                .active
            && !settings.has_locals()
            && !in_isolated_group
        {
            return self.apply_develop_preview_gpu(settings);
        }

        let preview = self
            .dev
            .develop_preview
            .as_mut()
            .expect("checked Some above");
        if preview.processing {
            preview.pending_settings = Some(settings);
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return true;
        }

        self.spawn_develop_preview_job(settings)
    }

    pub(super) fn apply_develop_preview_gpu(
        &mut self,
        settings: crate::core::develop::DevelopSettings,
    ) -> bool {
        // Detail has no realtime GPU implementation, so it alone receives a
        // quiet-period CPU refine. Tone/WB/Colour must never launch a full RAW
        // bake after every mouse release: that worker saturates Rayon and makes
        // the next slider wait. Open Image separately requests one exact bake.
        const DETAIL_REFINE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);
        let (layer_id, original_tiles, needs_restore, immediate_eligible) = {
            let Some(preview) = &mut self.dev.develop_preview else {
                return false;
            };
            if self.docs.documents[self.docs.active_doc_idx].id != preview.doc_id {
                return false;
            }

            // Immediate (no throttle) when the recompose needs no proxy rebuild: a
            // Colour-mixer-only tweak reuses the cached tone proxy, a Temperature/Tint
            // drag is a per-pixel shader WB stage over cheap downsampled proxies, AND a
            // global-tone edit (Contrast/Exposure/…) is now a pure per-pixel GPU
            // recompose. All are cheap, so throttling them only made dragging
            // step/stutter (WB stepped in "batches" whenever Colour/local-tone/Effects
            // were also engaged, dropping it out of the two cases below).
            let immediate_eligible = settings
                .differs_only_color_mixer(&preview.last_preview_settings)
                || settings.differs_only_white_balance(&preview.last_preview_settings)
                || settings.preview_proxy_free();
            preview.job_id = preview.job_id.wrapping_add(1);
            preview.pending_settings = None;
            // Keep an already-running receiver alive. Rayon work cannot be
            // cancelled, so dropping it here used to make `processing` look
            // false and allowed another full-image refine to start while the
            // stale one was still consuming CPU. The bumped job id makes its
            // result harmless; polling it is what releases the single-flight
            // gate before the newest settled settings are baked.

            if settings.has_detail() {
                preview.detail_refine_at = if preview.detail_refine_waiting_for_release {
                    None
                } else {
                    Some(std::time::Instant::now() + DETAIL_REFINE_DEBOUNCE)
                };
                preview.detail_refine_settings = Some(settings.clone());
            } else {
                preview.detail_refine_at = None;
                preview.detail_refine_settings = None;
            }

            (
                preview.layer_id,
                preview.original_tiles.clone(),
                !preview.gpu_preview_active && !preview.last_preview_settings.is_neutral(),
                immediate_eligible,
            )
        };

        if needs_restore {
            let restored = self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .preview_layer_tiles(layer_id, original_tiles);
            if !restored {
                return false;
            }
            if let Some(preview) = &mut self.dev.develop_preview {
                preview.last_preview_settings = settings;
                preview.gpu_preview_active = !preview.last_preview_settings.is_neutral();
            }
            self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
        } else {
            if let Some(preview) = &mut self.dev.develop_preview {
                preview.last_preview_settings = settings;
                preview.gpu_preview_active = !preview.last_preview_settings.is_neutral();
            }
            self.dev.develop_gpu_preview_immediate = if self.dev.develop_gpu_preview_dirty {
                self.dev.develop_gpu_preview_immediate && immediate_eligible
            } else {
                immediate_eligible
            };
            self.dev.develop_gpu_preview_dirty = true;
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }

        true
    }

    /// Keep expensive settled rendering out of an active slider drag. On
    /// release, arm a short debounce so the last interactive GPU frame paints
    /// first and only the final settings receive a full-resolution bake.
    pub(crate) fn set_develop_controls_pointer_down(&mut self, down: bool) {
        let Some(preview) = &mut self.dev.develop_preview else {
            return;
        };
        let was_down = preview.detail_refine_waiting_for_release;
        preview.detail_refine_waiting_for_release = down;
        if down {
            preview.detail_refine_at = None;
        } else if was_down && preview.detail_refine_settings.is_some() {
            preview.detail_refine_at =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(120));
        }
        if was_down != down {
            // Pointer state switches RAW colour between the old fast chroma
            // reconstruction path (down) and exact per-pixel scene colour
            // (released). Recompose even when the slider value itself did not
            // change on this event.
            self.dev.develop_gpu_preview_dirty = true;
            self.dev.develop_gpu_preview_immediate = true;
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            if let Some(w) = &self.win.develop_window {
                w.request_redraw();
            }
        }
    }

    pub(super) fn spawn_develop_preview_job(
        &mut self,
        settings: crate::core::develop::DevelopSettings,
    ) -> bool {
        let Some(preview) = &mut self.dev.develop_preview else {
            return false;
        };
        if self.docs.documents[self.docs.active_doc_idx].id != preview.doc_id {
            return false;
        }

        preview.job_id = preview.job_id.wrapping_add(1);
        preview.pending_settings = None;
        preview.last_preview_settings = settings.clone();
        // A full CPU bake includes Detail, so it supersedes any pending refine
        // (otherwise a stale refine could fire later and overwrite this bake).
        preview.detail_refine_at = None;
        preview.detail_refine_settings = None;
        // gpu_preview_active is left as-is while the bake runs: when this job
        // refines a live GPU preview (Detail), the shader keeps showing the
        // current settings instead of flashing back to the untouched source.
        // The flag drops when the result lands (poll_develop_preview).

        if settings.is_neutral() {
            preview.processing = false;
            preview.rx = None;
            preview.gpu_preview_active = false;
            let restored = self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .preview_layer_tiles(preview.layer_id, preview.original_tiles.clone());
            if restored {
                self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
            return restored;
        }

        let layer_id = preview.layer_id;
        let job_id = preview.job_id;
        let source_tiles = preview.original_tiles.clone();
        let selection = {
            let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
            let Some(layer) = canvas.layer_stack.layers.iter().find(|l| l.id == layer_id) else {
                return false;
            };
            if canvas.selection.active {
                Some(crate::core::develop::DevelopSelection {
                    selection: std::sync::Arc::new(canvas.selection.clone()),
                    layer_offset: layer.offset,
                })
            } else {
                None
            }
        };
        let scene = preview.scene.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        preview.processing = true;
        preview.rx = Some(rx);

        rayon::spawn(move || {
            let tiles = match &scene {
                Some(sc) => {
                    crate::core::develop_scene::apply_scene_to_tilemap(sc, &settings, selection)
                }
                None => crate::core::develop::apply_to_tilemap_direct(
                    &source_tiles,
                    &settings,
                    selection,
                ),
            };
            let _ = tx.send(crate::app::state::DevelopPreviewResult {
                job_id,
                settings,
                tiles,
            });
        });

        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        true
    }

    /// Fire the debounced commit-quality settled bake scheduled by
    /// `apply_develop_preview_gpu`. Waits out an in-flight job instead of
    /// stacking a second full-image bake on top of it.
    pub(super) fn fire_due_detail_refine(&mut self) {
        let settings = {
            let Some(preview) = &mut self.dev.develop_preview else {
                return;
            };
            let Some(at) = preview.detail_refine_at else {
                return;
            };
            if self.docs.documents[self.docs.active_doc_idx].id != preview.doc_id {
                preview.detail_refine_at = None;
                preview.detail_refine_settings = None;
                return;
            }
            if std::time::Instant::now() < at || preview.processing {
                return;
            }
            preview.detail_refine_at = None;
            let Some(settings) = preview.detail_refine_settings.take() else {
                return;
            };
            settings
        };
        self.spawn_develop_preview_job(settings);
    }

    /// Canvas coords → the Develop layer's normalized mask space
    /// (x/(w−1), y/(h−1)); values outside [0,1] are legal (a gradient handle
    /// may sit past the image edge).
    pub(super) fn develop_local_norm_coords(
        &self,
        canvas_x: f32,
        canvas_y: f32,
    ) -> Option<(f32, f32)> {
        let preview = self.dev.develop_preview.as_ref()?;
        if self.docs.documents[self.docs.active_doc_idx].id != preview.doc_id {
            return None;
        }
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let layer = canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == preview.layer_id)?;
        let inv_w = 1.0 / (preview.original_tiles.width.max(2) - 1) as f32;
        let inv_h = 1.0 / (preview.original_tiles.height.max(2) - 1) as f32;
        Some((
            (canvas_x - layer.offset.0 as f32) * inv_w,
            (canvas_y - layer.offset.1 as f32) * inv_h,
        ))
    }

    /// Screen-space outline of the selected local mask for the main window's
    /// canvas overlay (None when Develop is closed or nothing is selected).
    pub(super) fn build_develop_local_overlay(&self) -> Option<crate::ui::DevelopLocalOverlay> {
        self.develop_local_overlay_for_view(
            self.edit.view.offset_x,
            self.edit.view.offset_y,
            self.edit.view.zoom,
            1.0,
        )
    }

    /// Outline of the selected local mask under an arbitrary view transform
    /// (`canvas px × zoom + offset`, then ÷ `pixels_per_point` into egui
    /// points). The Develop window passes its own view; the main window passes
    /// its view with ppp 1 (its view is already in egui coordinates).
    pub(crate) fn develop_local_overlay_for_view(
        &self,
        offset_x: f32,
        offset_y: f32,
        zoom: f32,
        pixels_per_point: f32,
    ) -> Option<crate::ui::DevelopLocalOverlay> {
        if !self.shell.ui.show_develop_dialog {
            return None;
        }
        let idx = self.shell.ui.develop_local_selected?;
        let local = self.shell.ui.develop_settings.locals.get(idx)?;
        let preview = self.dev.develop_preview.as_ref()?;
        if self.docs.documents[self.docs.active_doc_idx].id != preview.doc_id {
            return None;
        }
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let layer = canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == preview.layer_id)?;
        let w = (preview.original_tiles.width.max(2) - 1) as f32;
        let h = (preview.original_tiles.height.max(2) - 1) as f32;
        let ppp = pixels_per_point.max(1e-6);
        let to_screen = |nx: f32, ny: f32| {
            let cx = nx * w + layer.offset.0 as f32;
            let cy = ny * h + layer.offset.1 as f32;
            ((cx * zoom + offset_x) / ppp, (cy * zoom + offset_y) / ppp)
        };
        Some(match local.shape {
            crate::core::develop::LocalMaskShape::Linear { x0, y0, x1, y1 } => {
                crate::ui::DevelopLocalOverlay::Linear {
                    p0: to_screen(x0, y0),
                    p1: to_screen(x1, y1),
                }
            }
            crate::core::develop::LocalMaskShape::Radial { cx, cy, rx, ry, .. } => {
                let c = to_screen(cx, cy);
                crate::ui::DevelopLocalOverlay::Radial {
                    cx: c.0,
                    cy: c.1,
                    rx: rx * w * zoom / ppp,
                    ry: ry * h * zoom / ppp,
                }
            }
        })
    }

    /// Main-window entry: canvas coords come from the pointer via the main view.
    pub(crate) fn develop_local_pointer_down(&mut self) {
        let ev = self.tool_event();
        self.develop_local_pointer_down_at(ev.canvas_x, ev.canvas_y);
    }

    pub(crate) fn develop_local_pointer_down_at(&mut self, canvas_x: f32, canvas_y: f32) {
        let Some((kind, target)) = self.shell.ui.develop_local_arm else {
            return;
        };
        if !self.shell.ui.show_develop_dialog {
            return;
        }
        let Some((nx, ny)) = self.develop_local_norm_coords(canvas_x, canvas_y) else {
            return;
        };
        self.dev.develop_local_drag = Some((nx, ny));
        let shape = crate::core::develop::LocalMaskShape::from_drag(kind, nx, ny, nx, ny);
        let locals = &mut self.shell.ui.develop_settings.locals;
        let idx = match target {
            Some(i) if i < locals.len() => {
                locals[i].shape = shape;
                i
            }
            _ => {
                locals.push(crate::core::develop::LocalAdjustment {
                    shape,
                    settings: Default::default(),
                });
                locals.len() - 1
            }
        };
        self.shell.ui.develop_local_arm = Some((kind, Some(idx)));
        self.shell.ui.develop_local_selected = Some(idx);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Main-window entry: canvas coords come from the pointer via the main view.
    pub(crate) fn develop_local_pointer_drag(&mut self) {
        let ev = self.tool_event();
        self.develop_local_pointer_drag_at(ev.canvas_x, ev.canvas_y);
    }

    pub(crate) fn develop_local_pointer_drag_at(&mut self, canvas_x: f32, canvas_y: f32) {
        let Some((sx, sy)) = self.dev.develop_local_drag else {
            return;
        };
        let Some((kind, Some(idx))) = self.shell.ui.develop_local_arm else {
            return;
        };
        let Some((nx, ny)) = self.develop_local_norm_coords(canvas_x, canvas_y) else {
            return;
        };
        match self.shell.ui.develop_settings.locals.get_mut(idx) {
            Some(l) => {
                l.shape = crate::core::develop::LocalMaskShape::from_drag(kind, sx, sy, nx, ny)
            }
            None => return,
        }
        // Geometry only re-bakes when the mask carries live sliders — a fresh
        // mask is neutral, so during placement the overlay alone tracks the drag.
        let settings = self.shell.ui.develop_settings.clone();
        if settings.has_locals() {
            self.update_develop_preview(settings);
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    pub(crate) fn develop_local_pointer_up(&mut self) {
        if self.dev.develop_local_drag.take().is_none() {
            return;
        }
        // A click without a drag leaves a zero-length linear gradient (full
        // weight everywhere); give it a sensible downward run instead.
        if let Some((_, Some(idx))) = self.shell.ui.develop_local_arm {
            if let Some(l) = self.shell.ui.develop_settings.locals.get_mut(idx) {
                if let crate::core::develop::LocalMaskShape::Linear { x0, y0, x1, y1 } =
                    &mut l.shape
                {
                    if (*x1 - *x0).powi(2) + (*y1 - *y0).powi(2) < 1e-6 {
                        *x1 = *x0;
                        *y1 = *y0 + 0.25;
                    }
                }
            }
        }
        self.shell.ui.develop_local_arm = None;
        let settings = self.shell.ui.develop_settings.clone();
        if settings.has_locals() {
            self.update_develop_preview(settings);
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    pub(crate) fn poll_develop_preview(&mut self) {
        self.fire_due_detail_refine();
        self.flush_due_develop_histogram();
        let result = {
            let Some(preview) = &mut self.dev.develop_preview else {
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
        if let Some(preview) = &mut self.dev.develop_preview {
            if result.job_id == preview.job_id
                && self.docs.documents[self.docs.active_doc_idx].id == preview.doc_id
            {
                let layer_id = preview.layer_id;
                let restored = self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .preview_layer_tiles(layer_id, result.tiles);
                if restored {
                    preview.last_preview_settings = result.settings;
                    preview.gpu_preview_active = false;
                    applied = true;
                }
                pending = preview.pending_settings.take();
            }
        }

        if applied {
            self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if let Some(settings) = pending {
            self.spawn_develop_preview_job(settings);
        }
    }

    pub(crate) fn apply_develop_settings_sync(
        &mut self,
        settings: crate::core::develop::DevelopSettings,
    ) -> bool {
        let Some(preview) = &mut self.dev.develop_preview else {
            return false;
        };
        if self.docs.documents[self.docs.active_doc_idx].id != preview.doc_id {
            return false;
        }

        if !preview.gpu_preview_active
            && !preview.processing
            && preview.pending_settings.is_none()
            && !settings.is_neutral()
            && preview.last_preview_settings.same_image_effect(&settings)
        {
            return true;
        }

        preview.job_id = preview.job_id.wrapping_add(1);
        preview.processing = false;
        preview.gpu_preview_active = false;
        preview.pending_settings = None;
        preview.rx = None;

        let layer_id = preview.layer_id;
        let source_tiles = preview.original_tiles.clone();
        let selection = {
            let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
            let Some(layer) = canvas.layer_stack.layers.iter().find(|l| l.id == layer_id) else {
                return false;
            };
            if canvas.selection.active {
                Some(crate::core::develop::DevelopSelection {
                    selection: std::sync::Arc::new(canvas.selection.clone()),
                    layer_offset: layer.offset,
                })
            } else {
                None
            }
        };
        let scene = self
            .dev
            .develop_preview
            .as_ref()
            .and_then(|p| p.scene.clone());
        let tiles = if settings.is_neutral() {
            source_tiles
        } else {
            match &scene {
                Some(sc) => {
                    crate::core::develop_scene::apply_scene_to_tilemap(sc, &settings, selection)
                }
                None => crate::core::develop::apply_to_tilemap_direct(
                    &source_tiles,
                    &settings,
                    selection,
                ),
            }
        };
        let ok = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .restore_layer_tiles(layer_id, tiles);
        if ok {
            self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
        }
        ok
    }

    pub(crate) fn commit_develop_preview(&mut self) -> bool {
        self.dev.develop_local_drag = None;
        self.shell.ui.develop_local_arm = None;
        self.shell.ui.develop_local_selected = None;
        let Some(preview) = self.dev.develop_preview.take() else {
            return false;
        };
        self.clear_develop_gpu_commit_state();
        if self.docs.documents[self.docs.active_doc_idx].id != preview.doc_id {
            return false;
        }

        let ok = {
            let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
            // Commit = baked (PTS semantics): the scene master is consumed —
            // reopening Develop on the committed image starts neutral on the
            // baked pixels, and the f16 master's memory is released.
            canvas.develop_source = None;
            let Some(idx) = canvas
                .layer_stack
                .layers
                .iter()
                .position(|l| l.id == preview.layer_id)
            else {
                return false;
            };
            let mut after_tiles = canvas.layer_stack.layers[idx].tiles.clone();
            after_tiles.bump_all_revisions();
            canvas.commit_layer_tiles_change(
                preview.layer_id,
                preview.original_tiles,
                after_tiles,
                "Develop",
            )
        };

        if ok {
            self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
        }
        ok
    }

    pub(crate) fn cancel_develop_preview(&mut self) {
        self.dev.develop_histogram = None;
        self.dev.develop_histogram_at = None;
        self.dev.develop_histogram_stale = false;
        self.dev.develop_local_drag = None;
        self.shell.ui.develop_local_arm = None;
        self.shell.ui.develop_local_selected = None;
        let Some(preview) = self.dev.develop_preview.take() else {
            return;
        };
        self.clear_develop_gpu_commit_state();
        if self.docs.documents[self.docs.active_doc_idx].id == preview.doc_id {
            let restored = self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .restore_layer_tiles(preview.layer_id, preview.original_tiles);
            if restored {
                self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
                if let Some(w) = &self.win.window {
                    w.request_redraw();
                }
            }
        }
    }
}

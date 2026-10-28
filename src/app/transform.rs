// Free Transform (Ctrl+T) — begin / commit / cancel / drag logic.
//
// Bbox:   Uses TIGHT content bounds (non-transparent pixels) for handles/overlay.
//         Full layer bounds are kept in LayerOrigState for pixel sampling.
// Multi:  All selected non-locked layers are transformed together (union bbox).
// GPU:    Primary layer (index 0) gets live inverse-matrix preview shader.
// Commit: CPU bilinear resample per layer → new TileMaps; one undo command.

use super::state::{App, TransformHandle};
use crate::app::render::CanvasEvent;
use crate::core::document::GuideOrientation;
use crate::core::free_transform::*;
use crate::core::layer::LayerType;
use crate::core::snapping::{SNAP_THRESHOLD_PX, SnapKind, SnapLine, best_snap};
use crate::core::vector::object::VectorGeometry;
use crate::gpu::compositor::TransformPreviewUniform;

const HANDLE_RADIUS: f32 = 8.0;
const ROTATE_ZONE: f32 = 24.0;

impl App {
    /// Axis-aligned selection box shown by Move for selected vector primitives
    /// before a transform session is started. Path objects have their own
    /// oriented affine box; pixel layers only get a box through Ctrl+T.
    pub fn move_selection_transform_box(
        &self,
    ) -> Option<([(f32, f32); 4], [(f32, f32); 8], (f32, f32))> {
        if self.edit.tools.active_id() != crate::tools::ToolId::Move
            || self.edit.transform_state.is_some()
            || self.edit.input.painting
        {
            return None;
        }
        // The path box has a single object, or paths alone; any other pick
        // with a drawn object in it (shapes, paths, type, pictures among
        // them) gets this one box around all of it, as CorelDRAW frames a
        // selection. Pictures alone get a box through Ctrl+T only.
        if self.active_path_transform_box().is_some() {
            return None;
        }
        let canvas = &self.docs.active().canvas;
        let stack = &canvas.layer_stack;
        let picked: Vec<usize> = stack
            .layers
            .iter()
            .enumerate()
            .filter(|(_, l)| l.selected && !l.locked && !l.is_background)
            .map(|(i, _)| i)
            .collect();
        // A group stands for its members.
        let mut targets: Vec<usize> = Vec::new();
        for &i in &picked {
            if stack.layers[i].is_group() {
                targets.extend(stack.group_member_range(i));
            } else {
                targets.push(i);
            }
        }
        // Nothing drawn on a layer (a fresh one) does not stretch the box.
        targets.retain(|&i| {
            stack.layers.get(i).is_some_and(|l| {
                l.visible
                    && crate::core::free_transform::transformable_layer(l)
                    && !l.tiles.tiles.is_empty()
            })
        });
        let drawn = |l: &crate::core::layer::Layer| {
            matches!(l.layer_type, LayerType::Vector(_) | LayerType::Text(_))
        };
        if targets.is_empty() || !targets.iter().any(|&i| drawn(&stack.layers[i])) {
            return None;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &i in &targets {
            let l = &stack.layers[i];
            // A path by its geometry (its raster may be padded); the rest by
            // their raster, which the transform session's box is built on.
            let b = match &l.layer_type {
                LayerType::Vector(VectorGeometry::Path(_)) => self
                    .path_canvas_bounds_of(i)
                    .map(|r| (r.x, r.y, r.x + r.w, r.y + r.h)),
                _ => Some((
                    l.offset.0 as f32,
                    l.offset.1 as f32,
                    (l.offset.0 + l.width as i32) as f32,
                    (l.offset.1 + l.height as i32) as f32,
                )),
            };
            if let Some((bx0, by0, bx1, by1)) = b {
                x0 = x0.min(bx0.floor());
                y0 = y0.min(by0.floor());
                x1 = x1.max(bx1.ceil());
                y1 = y1.max(by1.ceil());
            }
        }
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
        Some((
            // Transform overlays consistently use TL, TR, BL, BR. Keeping that
            // order is important: the painter joins 0-1-3-2-0; returning the
            // usual clockwise TL,TR,BR,BL order draws an accidental X.
            [(x0, y0), (x1, y0), (x0, y1), (x1, y1)],
            [
                (x0, y0),
                (cx, y0),
                (x1, y0),
                (x0, cy),
                (x1, cy),
                (x0, y1),
                (cx, y1),
                (x1, y1),
            ],
            (cx, cy),
        ))
    }

    /// True when the pointer is on a Move selection handle or rotation ring.
    pub fn move_selection_transform_hit(&self, sx: f32, sy: f32) -> bool {
        let (corners, handles, center) = match self.move_selection_transform_box() {
            Some(v) => v,
            None => return false,
        };
        let zoom = self.edit.view.zoom;
        let ox = self.edit.view.offset_x;
        let oy = self.edit.view.offset_y;
        let screen = |p: (f32, f32)| (p.0 * zoom + ox, p.1 * zoom + oy);
        let dist =
            |a: (f32, f32), b: (f32, f32)| ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt();
        if handles
            .iter()
            .any(|&p| dist((sx, sy), screen(p)) <= HANDLE_RADIUS)
        {
            return true;
        }
        let cs = screen(center);
        corners.iter().any(|&p| {
            let ps = screen(p);
            let d = dist((sx, sy), ps);
            d > HANDLE_RADIUS && d <= ROTATE_ZONE && dist((sx, sy), cs) >= dist(ps, cs) - 2.0
        })
    }

    pub fn move_selection_transform_cursor_hint(&self, sx: f32, sy: f32) -> u8 {
        let (corners, handles, center) = match self.move_selection_transform_box() {
            Some(v) => v,
            None => return 0,
        };
        let zoom = self.edit.view.zoom;
        let ox = self.edit.view.offset_x;
        let oy = self.edit.view.offset_y;
        let screen = |p: (f32, f32)| (p.0 * zoom + ox, p.1 * zoom + oy);
        let dist =
            |a: (f32, f32), b: (f32, f32)| ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt();
        let hints = [2, 4, 3, 5, 5, 3, 4, 2];
        if let Some((i, _)) = handles
            .iter()
            .enumerate()
            .map(|(i, &p)| (i, dist((sx, sy), screen(p))))
            .filter(|(_, d)| *d <= HANDLE_RADIUS)
            .min_by(|a, b| a.1.total_cmp(&b.1))
        {
            return hints[i];
        }
        let cs = screen(center);
        if corners.iter().any(|&p| {
            let ps = screen(p);
            let d = dist((sx, sy), ps);
            d > HANDLE_RADIUS && d <= ROTATE_ZONE && dist((sx, sy), cs) >= dist(ps, cs) - 2.0
        }) {
            return 6;
        }
        0
    }

    pub fn begin_transform(&mut self) {
        let t0 = std::time::Instant::now();
        self.begin_transform_for_tool(true);
        // Diagnostic: Ctrl+T setup runs on the UI thread.
        let ms = t0.elapsed().as_millis();
        if ms >= 30 {
            eprintln!("iai[perf]: begin_transform {ms} ms");
            crate::diag::note("perf", &format!("begin_transform {ms} ms"));
        }
    }

    /// Start the existing transform engine without leaving Move.  This is used
    /// by the Move tool's on-canvas handles; Ctrl+T still selects Free Transform.
    pub fn begin_move_transform(&mut self) {
        self.begin_transform_for_tool(false);
    }

    fn begin_transform_for_tool(&mut self, select_transform_tool: bool) {
        if self.edit.transform_state.is_some() {
            return;
        }
        if self.edit.text_edit.is_some() {
            self.commit_text_edit();
        }
        // Move's handles are drawn on the layers' raster rectangles. The press
        // that starts this session is hit-tested against the session's own box,
        // so it has to be that same box: a shape's content sits a few pixels
        // inside its padded raster, and a press on the drawn corner would miss
        // the content corner and start a rotation instead of a scale.
        let move_box = if select_transform_tool {
            None
        } else {
            self.move_selection_transform_box()
                .map(|(corners, _, _)| (corners[0], corners[3]))
        };
        let doc = self.docs.active_mut();
        let session = crate::core::free_transform::session(
            &doc.canvas,
            &mut doc.transform_lineage,
            None,
            move_box,
        );
        self.edit.transform_state = match session {
            Ok(ts) => Some(ts),
            Err(NoSession::NothingToTransform) => return,
        };

        self.update_transform_preview();
        if select_transform_tool {
            self.edit.tools.select(crate::tools::ToolId::Transform);
        }
        let n = self
            .edit
            .transform_state
            .as_ref()
            .map(|ts| ts.layer_states.len())
            .unwrap_or(0);
        self.shell.status_msg = if !select_transform_tool && n > 1 {
            format!(
                "Move transform ({n} layers) — drag handles to scale, drag outside a corner to rotate"
            )
        } else if !select_transform_tool {
            "Move transform — drag handles to scale, drag outside a corner to rotate".to_string()
        } else if n > 1 {
            format!(
                "Free Transform ({n} layers) — corner=proportional • Shift+edge=distort • Alt=from-center • Enter=commit • Esc=cancel"
            )
        } else {
            "Free Transform — corner=proportional • Shift+edge=distort • Alt=from-center • Enter/Esc".to_string()
        };
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    pub fn cancel_transform(&mut self) {
        if self.edit.transform_state.is_none() {
            return;
        }
        self.edit.transform_state = None;
        self.edit.transform_snap_guides.clear();
        self.clear_transform_preview();
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.edit.tools.select(crate::tools::ToolId::Move);
        self.shell.status_msg = "Transform cancelled".to_string();
    }

    /// Undo while Free Transform is live: revert the pending transform to
    /// identity instead of popping the document history underneath the
    /// session. Returns true when a transform session consumed the undo.
    pub fn transform_undo_pending(&mut self) -> bool {
        let Some(ts) = self.edit.transform_state.as_mut() else {
            return false;
        };
        let identity = ts.scale_x == 1.0
            && ts.scale_y == 1.0
            && ts.angle_deg == 0.0
            && ts.translate_x == 0.0
            && ts.translate_y == 0.0
            && ts.quad.is_none();
        if !identity {
            ts.scale_x = 1.0;
            ts.scale_y = 1.0;
            ts.angle_deg = 0.0;
            ts.translate_x = 0.0;
            ts.translate_y = 0.0;
            ts.drag_handle = None;
            ts.quad = None;
            ts.mode = crate::app::state::TransformMode::Free;
            self.update_transform_preview();
            self.recomposite_visible();
            self.shell.status_msg = "Transform reset".to_string();
        }
        true
    }

    /// Commit the session with `layer.free_transform`, the command an agent
    /// runs: a plain move at once, anything else as a job the frame loop takes
    /// in (`on_free_transform_finished`).
    pub fn commit_transform(&mut self) {
        let Some(ts) = self.edit.transform_state.take() else {
            return;
        };
        self.edit.transform_snap_guides.clear();
        let interpolation = match self.shell.ui.transform_interpolation {
            crate::core::geometry::InterpolationMode::Bilinear => "bilinear",
            crate::core::geometry::InterpolationMode::NearestNeighbor => "nearest",
        };
        let (x0, y0) = (ts.orig_offset.0 as f32, ts.orig_offset.1 as f32);
        let mut args = serde_json::json!({
            "doc": self.docs.active().id.0,
            "layers": ts.layer_states.iter().map(|ls| ls.layer_id).collect::<Vec<_>>(),
            "scaleX": ts.scale_x,
            "scaleY": ts.scale_y,
            "angle": ts.angle_deg,
            "dx": ts.translate_x,
            "dy": ts.translate_y,
            "pivot": [ts.pivot_cx, ts.pivot_cy],
            "bounds": [x0, y0, x0 + ts.orig_w as f32, y0 + ts.orig_h as f32],
            "resample": interpolation,
        });
        if let Some(quad) = ts.quad {
            args["corners"] = quad.map(|(x, y)| [x, y]).to_vec().into();
        }
        self.edit.tools.select(crate::tools::ToolId::Move);
        match self.start_command("layer.free_transform", args) {
            Some(iai_engine::Reply::Started { job }) => {
                self.edit.pending_transform_commit = Some(job);
                self.shell.status_msg = "Applying transform...".to_string();
            }
            Some(iai_engine::Reply::Done { .. }) => {
                // A plain move, or none: no resample to wait for.
                self.clear_transform_preview();
                self.sync_compositor_viewport();
                self.recomposite();
                self.edit.warp_after_transform_commit = false;
            }
            None => {
                self.edit.warp_after_transform_commit = false;
                self.clear_transform_preview();
                self.recomposite();
            }
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Take in a finished commit (the frame loop does this as it polls the
    /// engine); kept for callers that wait on `pending_transform_commit`.
    pub fn poll_transform_commit(&mut self) {
        if self.edit.pending_transform_commit.is_some() {
            self.poll_engine();
        }
    }

    /// The commit's job has ended: the preview goes before its result lands.
    /// Left on, the redraw of the landed layers would draw them through the
    /// preview's transform once more, and that picture stayed on screen until
    /// the next redraw.
    pub(in crate::app) fn before_transform_lands(
        &mut self,
        ended: &[(iai_engine::JobId, &'static str)],
    ) {
        let ours = self.edit.pending_transform_commit;
        if ended.iter().any(|(job, _)| Some(*job) == ours) {
            self.clear_transform_preview();
        }
    }

    /// `layer.free_transform` ended: the preview gives way to the result, and
    /// a Warp asked for meanwhile starts.
    pub(in crate::app) fn on_free_transform_finished(
        &mut self,
        job: iai_engine::JobId,
        error: Option<&iai_engine::CmdError>,
    ) {
        if self.edit.pending_transform_commit != Some(job) {
            return;
        }
        self.edit.pending_transform_commit = None;
        self.clear_transform_preview();
        if error.is_some() {
            self.edit.warp_after_transform_commit = false;
            self.recomposite();
            return;
        }
        if std::mem::take(&mut self.edit.warp_after_transform_commit) {
            self.begin_warp();
        }
    }

    pub fn update_transform_preview(&mut self) {
        // A clip mask is derived from the child/base geometry. Make sure Free
        // Transform starts every preview frame from the current derived mask,
        // rather than carrying a stale bake into the GPU's fixed-frame sampling
        // path. Unlike Move, the transform itself lives only in the preview
        // matrix, so the fingerprint gate makes subsequent drag frames no-ops.
        // Both affine and projective previews below then read the same fresh mask.
        let clip_mask_changed = self.docs.active_mut().canvas.refresh_clip_masks();
        if clip_mask_changed {
            let canvas = &mut self.docs.active_mut().canvas;
            let clip_bounds: Vec<(i32, i32, u32, u32)> = canvas
                .layer_stack
                .layers
                .iter()
                .filter(|l| l.clip_parent_id.is_some())
                .map(|l| (l.offset.0, l.offset.1, l.width, l.height))
                .collect();
            for (ox, oy, lw, lh) in clip_bounds {
                canvas.mark_dirty_layer_bounds(ox, oy, lw, lh);
            }
        }

        let previews: Vec<TransformPreviewUniform> = if let Some(ts) = &self.edit.transform_state {
            let Some(inv_m) = ts.inverse_homography() else {
                return;
            };
            let canvas = &self.docs.active().canvas;
            ts.layer_states
                .iter()
                .chain(ts.preview_layer_states.iter())
                .filter_map(|ls| {
                    // `ls` may hold the retained pre-transform source for CPU
                    // commit. GPU preview, however, samples the raster currently
                    // resident in the layer atlas, so its texture origin must
                    // come from the live layer rather than the retained source.
                    let layer = canvas
                        .layer_stack
                        .layers
                        .iter()
                        .find(|layer| layer.id == ls.layer_id)?;
                    Some(TransformPreviewUniform {
                        layer_id: ls.layer_id,
                        inv_m,
                        orig_ox: layer.offset.0 as f32,
                        orig_oy: layer.offset.1 as f32,
                    })
                })
                .collect()
        } else {
            Vec::new()
        };

        if let Some(gpu) = &mut self.win.gpu {
            gpu.compositor.transform_previews = previews;
        }
    }

    fn clear_transform_preview(&mut self) {
        if let Some(gpu) = &mut self.win.gpu {
            gpu.compositor.transform_previews.clear();
        }
    }

    pub fn transform_on_press(
        &mut self,
        canvas_x: f32,
        canvas_y: f32,
        screen_x: f32,
        screen_y: f32,
    ) {
        let Some(ts) = self.edit.transform_state.as_mut() else {
            return;
        };
        let zoom = self.edit.view.zoom;
        let vox = self.edit.view.offset_x;
        let voy = self.edit.view.offset_y;
        let c2s = |cx: f32, cy: f32| (cx * zoom + vox, cy * zoom + voy);

        let handles = ts.handle_positions();
        let order = [
            TransformHandle::TopLeft,
            TransformHandle::TopCenter,
            TransformHandle::TopRight,
            TransformHandle::MiddleLeft,
            TransformHandle::MiddleRight,
            TransformHandle::BottomLeft,
            TransformHandle::BottomCenter,
            TransformHandle::BottomRight,
        ];
        for (i, &h) in order.iter().enumerate() {
            let (hcx, hcy) = handles[i];
            let (shx, shy) = c2s(hcx, hcy);
            if ((screen_x - shx).powi(2) + (screen_y - shy).powi(2)).sqrt() < HANDLE_RADIUS {
                ts.drag_handle = Some(Some(h));
                ts.drag_start_cx = canvas_x;
                ts.drag_start_cy = canvas_y;
                ts.drag_start_sx = ts.scale_x;
                ts.drag_start_sy = ts.scale_y;
                ts.drag_start_angle = ts.angle_deg;
                ts.drag_start_tx = ts.translate_x;
                ts.drag_start_ty = ts.translate_y;
                if let Some(q) = ts.quad {
                    ts.drag_start_quad = q;
                }
                return;
            }
        }

        let Some((orig_cx, orig_cy)) = ts.inverse_canvas_point(canvas_x, canvas_y) else {
            return;
        };
        let ox0 = ts.orig_offset.0 as f32;
        let oy0 = ts.orig_offset.1 as f32;
        let ox1 = ox0 + ts.orig_w as f32;
        let oy1 = oy0 + ts.orig_h as f32;
        let inside_bbox = orig_cx >= ox0 && orig_cx <= ox1 && orig_cy >= oy0 && orig_cy <= oy1;

        if inside_bbox {
            ts.drag_handle = Some(Some(TransformHandle::Center));
            ts.drag_start_cx = canvas_x;
            ts.drag_start_cy = canvas_y;
            ts.drag_start_tx = ts.translate_x;
            ts.drag_start_ty = ts.translate_y;
            if let Some(q) = ts.quad {
                ts.drag_start_quad = q;
            }
            return;
        }

        if ts.mode != crate::app::state::TransformMode::Free {
            return;
        }

        let rz = ROTATE_ZONE / zoom;
        let in_rotate = orig_cx >= ox0 - rz
            && orig_cx <= ox1 + rz
            && orig_cy >= oy0 - rz
            && orig_cy <= oy1 + rz;
        if in_rotate {
            ts.drag_handle = Some(None);
            ts.drag_start_cx = canvas_x;
            ts.drag_start_cy = canvas_y;
            ts.drag_start_angle = ts.angle_deg;
            ts.drag_start_tx = ts.translate_x;
            ts.drag_start_ty = ts.translate_y;
        }
    }

    /// Scale behavior:
    /// - Corner handles (TL/TR/BL/BR): always proportional, Shift has no effect.
    /// - Middle handles (TC/BC/ML/MR): proportional by default; Shift = distort along that axis only.
    /// - Rotate: Shift = snap to 15° steps.
    ///
    /// `alt` = scale from center pivot (no translate adjustment); otherwise opposite anchor is fixed.
    ///
    /// Build per-axis snap targets for Free Transform: canvas edges/center, ruler
    /// guides, and the content edges/centers of visible layers NOT being
    /// transformed.
    fn build_transform_snap_targets(&self) -> (Vec<(f32, SnapKind)>, Vec<(f32, SnapKind)>) {
        let canvas = &self.docs.active().canvas;
        let w = canvas.width as f32;
        let h = canvas.height as f32;
        let mut tx = vec![
            (0.0, SnapKind::CanvasEdge),
            (w, SnapKind::CanvasEdge),
            (w * 0.5, SnapKind::CanvasCenter),
        ];
        let mut ty = vec![
            (0.0, SnapKind::CanvasEdge),
            (h, SnapKind::CanvasEdge),
            (h * 0.5, SnapKind::CanvasCenter),
        ];

        let xform_ids: Vec<u32> = self
            .edit
            .transform_state
            .as_ref()
            .map(|ts| ts.layer_states.iter().map(|l| l.layer_id).collect())
            .unwrap_or_default();

        for layer in canvas.layer_stack.layers.iter() {
            if !layer.visible || xform_ids.contains(&layer.id) {
                continue;
            }
            // An empty (or group) layer's rectangle is not anything on screen.
            if layer.width == 0 || layer.height == 0 || !layer.has_renderable_content() {
                continue;
            }
            let lx0 = layer.offset.0 as f32;
            let ly0 = layer.offset.1 as f32;
            let lx1 = lx0 + layer.width as f32;
            let ly1 = ly0 + layer.height as f32;
            tx.push((lx0, SnapKind::LayerEdge));
            tx.push((lx1, SnapKind::LayerEdge));
            tx.push(((lx0 + lx1) * 0.5, SnapKind::LayerCenter));
            ty.push((ly0, SnapKind::LayerEdge));
            ty.push((ly1, SnapKind::LayerEdge));
            ty.push(((ly0 + ly1) * 0.5, SnapKind::LayerCenter));
        }

        for g in &self.docs.active().guides {
            match g.orientation {
                GuideOrientation::Vertical => tx.push((g.pos, SnapKind::Guide)),
                GuideOrientation::Horizontal => ty.push((g.pos, SnapKind::Guide)),
            }
        }
        (tx, ty)
    }

    pub fn transform_on_drag(&mut self, canvas_x: f32, canvas_y: f32, shift: bool, alt: bool) {
        // Snap inputs need &self → gather before borrowing transform_state mutably.
        let snap_enabled = self.shell.ui.snap_enabled;
        let zoom = self.edit.view.zoom;
        let needs_snap_targets = snap_enabled
            && self
                .edit
                .transform_state
                .as_ref()
                .and_then(|ts| ts.drag_handle)
                .flatten()
                .is_some();
        let (tx_targets, ty_targets) = if needs_snap_targets {
            self.build_transform_snap_targets()
        } else {
            (Vec::new(), Vec::new())
        };
        let threshold = SNAP_THRESHOLD_PX / zoom.max(1e-4);
        self.edit.transform_snap_guides.clear();
        let mut guides: Vec<SnapLine> = Vec::new();

        let projective_dragged = {
            let Some(ts) = self.edit.transform_state.as_mut() else {
                return;
            };
            if ts.mode == crate::app::state::TransformMode::Free || ts.quad.is_none() {
                false
            } else if let Some(Some(h)) = ts.drag_handle {
                let (dx, dy) = constrained_move_delta(
                    canvas_x - ts.drag_start_cx,
                    canvas_y - ts.drag_start_cy,
                    shift && h == TransformHandle::Center,
                );
                let candidate = dragged_projective_quad(ts.drag_start_quad, h, ts.mode, dx, dy);
                let valid = crate::core::geometry::Homography::square_to_quad(
                    candidate.map(|(x, y)| crate::core::geometry::Point::new(x, y)),
                )
                .and_then(|m| m.inverse())
                .is_some();
                if valid {
                    ts.quad = Some(candidate);
                }
                true
            } else {
                false
            }
        };
        if projective_dragged {
            self.update_transform_preview();
            self.request_interactive_recompose();
            return;
        }

        {
            let Some(ts) = self.edit.transform_state.as_mut() else {
                return;
            };
            // Snapping is only meaningful while the box is axis-aligned.
            // Written out: unlike `!(0.5..=359.5).contains(&a)` it is false for NaN.
            #[allow(clippy::manual_range_contains)]
            let angle0 = {
                let a = ts.angle_deg.rem_euclid(360.0);
                a < 0.5 || a > 359.5
            };

            match ts.drag_handle {
                None => {
                    return;
                }
                Some(None) => {
                    let eff_cx = ts.pivot_cx + ts.drag_start_tx;
                    let eff_cy = ts.pivot_cy + ts.drag_start_ty;
                    let start_a = (ts.drag_start_cy - eff_cy).atan2(ts.drag_start_cx - eff_cx);
                    let cur_a = (canvas_y - eff_cy).atan2(canvas_x - eff_cx);
                    let new_angle = ts.drag_start_angle + (cur_a - start_a).to_degrees();
                    ts.angle_deg = if shift {
                        (new_angle / 15.0).round() * 15.0
                    } else {
                        new_angle
                    };
                }
                Some(Some(TransformHandle::Center)) => {
                    let raw_dx = canvas_x - ts.drag_start_cx;
                    let raw_dy = canvas_y - ts.drag_start_cy;
                    let (dx, dy) = constrained_move_delta(raw_dx, raw_dy, shift);
                    ts.translate_x = ts.drag_start_tx + dx;
                    ts.translate_y = ts.drag_start_ty + dy;
                    let moving_horizontally = raw_dx.abs() >= raw_dy.abs();

                    // Snap the moving bbox edges/center to targets (like the Move tool).
                    if snap_enabled && angle0 {
                        let c = ts.corners();
                        let l = c.iter().map(|p| p.0).fold(f32::INFINITY, f32::min);
                        let r = c.iter().map(|p| p.0).fold(f32::NEG_INFINITY, f32::max);
                        let t = c.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
                        let b = c.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max);
                        // With Shift held, only snap along the chosen movement
                        // axis. Snapping the other axis would reintroduce the
                        // diagonal drift that the constraint is meant to remove.
                        if (!shift || moving_horizontally)
                            && let Some((adj, pos, kind)) =
                                best_snap(&[l, (l + r) * 0.5, r], &tx_targets, threshold)
                        {
                            ts.translate_x += adj;
                            guides.push(SnapLine {
                                vertical: true,
                                pos,
                                kind,
                            });
                        }
                        if (!shift || !moving_horizontally)
                            && let Some((adj, pos, kind)) =
                                best_snap(&[t, (t + b) * 0.5, b], &ty_targets, threshold)
                        {
                            ts.translate_y += adj;
                            guides.push(SnapLine {
                                vertical: false,
                                pos,
                                kind,
                            });
                        }
                    }
                }
                Some(Some(h)) => {
                    let rad = ts.drag_start_angle.to_radians();
                    let cos_a = rad.cos();
                    let sin_a = rad.sin();
                    let (lx, ly) = ts.handle_local(h);
                    let target_x = canvas_x - ts.pivot_cx - ts.drag_start_tx;
                    let target_y = canvas_y - ts.pivot_cy - ts.drag_start_ty;
                    let along_x = cos_a * target_x + sin_a * target_y;
                    let along_y = -sin_a * target_x + cos_a * target_y;

                    let upd_x = lx.abs() > 1.0;
                    let upd_y = ly.abs() > 1.0;

                    // Non-Alt drags keep the opposite handle fixed. Compute each
                    // axis from that anchor, not from the center pivot; translation
                    // below then preserves the anchor without doubling the drag.
                    let anchor_lx = if alt { 0.0 } else { -lx };
                    let anchor_ly = if alt { 0.0 } else { -ly };

                    let raw_sx = if upd_x {
                        (along_x - ts.drag_start_sx * anchor_lx) / (lx - anchor_lx)
                    } else {
                        ts.drag_start_sx
                    };
                    let raw_sy = if upd_y {
                        (along_y - ts.drag_start_sy * anchor_ly) / (ly - anchor_ly)
                    } else {
                        ts.drag_start_sy
                    };

                    match (upd_x, upd_y) {
                        (true, true) => {
                            // Project onto the original corner diagonal. Besides
                            // keeping the handle with the cursor, this avoids the
                            // x/y dominance switch that caused visible jitter.
                            let ratio = corner_drag_ratio(
                                along_x,
                                along_y,
                                lx,
                                ly,
                                ts.drag_start_sx,
                                ts.drag_start_sy,
                                alt,
                            );
                            ts.scale_x = ts.drag_start_sx * ratio;
                            ts.scale_y = ts.drag_start_sy * ratio;
                        }
                        (true, false) => {
                            ts.scale_x = raw_sx;
                            if shift {
                            } else {
                                ts.scale_y = ts.drag_start_sy * (raw_sx / ts.drag_start_sx);
                            }
                        }
                        (false, true) => {
                            ts.scale_y = raw_sy;
                            if shift {
                            } else {
                                ts.scale_x = ts.drag_start_sx * (raw_sy / ts.drag_start_sy);
                            }
                        }
                        _ => {}
                    }

                    ts.scale_x = clamp_live_scale(ts.scale_x, ts.drag_start_sx);
                    ts.scale_y = clamp_live_scale(ts.scale_y, ts.drag_start_sy);

                    if !alt {
                        let opp_lx = -lx;
                        let opp_ly = -ly;
                        let sx_new = ts.scale_x;
                        let sy_new = ts.scale_y;
                        ts.translate_x = ts.drag_start_tx
                            + cos_a * (ts.drag_start_sx - sx_new) * opp_lx
                            - sin_a * (ts.drag_start_sy - sy_new) * opp_ly;
                        ts.translate_y = ts.drag_start_ty
                            + sin_a * (ts.drag_start_sx - sx_new) * opp_lx
                            + cos_a * (ts.drag_start_sy - sy_new) * opp_ly;
                    }

                    // Snap pass: nudge the scale so the *actual* moving edge lands on a
                    // target (canvas edge/center, guide, other layer), keeping the same
                    // fixed anchor. Operates on the real edge position — accurate &
                    // responsive — instead of the cursor (which doesn't equal the edge).
                    if snap_enabled && angle0 {
                        let pcx = ts.pivot_cx;
                        let pcy = ts.pivot_cy;
                        let ox0 = ts.orig_offset.0 as f32;
                        let oy0 = ts.orig_offset.1 as f32;
                        let ox1 = ox0 + ts.orig_w as f32;
                        let oy1 = oy0 + ts.orig_h as f32;

                        let move_right = matches!(
                            h,
                            TransformHandle::TopRight
                                | TransformHandle::MiddleRight
                                | TransformHandle::BottomRight
                        );
                        let move_left = matches!(
                            h,
                            TransformHandle::TopLeft
                                | TransformHandle::MiddleLeft
                                | TransformHandle::BottomLeft
                        );
                        let move_top = matches!(
                            h,
                            TransformHandle::TopLeft
                                | TransformHandle::TopCenter
                                | TransformHandle::TopRight
                        );
                        let move_bottom = matches!(
                            h,
                            TransformHandle::BottomLeft
                                | TransformHandle::BottomCenter
                                | TransformHandle::BottomRight
                        );
                        let is_corner = matches!(
                            h,
                            TransformHandle::TopLeft
                                | TransformHandle::TopRight
                                | TransformHandle::BottomLeft
                                | TransformHandle::BottomRight
                        );
                        // Corners are always proportional; middle handles are proportional
                        // unless Shift (distort one axis).
                        let proportional = is_corner || !shift;

                        // Fixed anchor per axis: the opposite edge (far-side anchor) when
                        // this axis is dragged and not Alt, otherwise the center.
                        let ax_orig = if (move_left || move_right) && !alt {
                            if move_right { ox0 } else { ox1 }
                        } else {
                            pcx
                        };
                        let ay_orig = if (move_top || move_bottom) && !alt {
                            if move_bottom { oy0 } else { oy1 }
                        } else {
                            pcy
                        };
                        let a_x = pcx + ts.translate_x + ts.scale_x * (ax_orig - pcx);
                        let a_y = pcy + ts.translate_y + ts.scale_y * (ay_orig - pcy);

                        // cand = (dist, ratio, target_pos, kind)
                        let cand_x = if move_left || move_right {
                            let mo = if move_right { ox1 } else { ox0 };
                            let e = pcx + ts.translate_x + ts.scale_x * (mo - pcx);
                            best_snap(&[e], &tx_targets, threshold).and_then(|(adj, pos, kind)| {
                                let denom = e - a_x;
                                (denom.abs() > 1e-3)
                                    .then(|| (adj.abs(), (pos - a_x) / denom, pos, kind))
                            })
                        } else {
                            None
                        };
                        let cand_y = if move_top || move_bottom {
                            let mo = if move_bottom { oy1 } else { oy0 };
                            let e = pcy + ts.translate_y + ts.scale_y * (mo - pcy);
                            best_snap(&[e], &ty_targets, threshold).and_then(|(adj, pos, kind)| {
                                let denom = e - a_y;
                                (denom.abs() > 1e-3)
                                    .then(|| (adj.abs(), (pos - a_y) / denom, pos, kind))
                            })
                        } else {
                            None
                        };

                        let chosen = match (cand_x, cand_y) {
                            (Some(cx), Some(cy)) => {
                                if cx.0 <= cy.0 {
                                    Some((true, cx))
                                } else {
                                    Some((false, cy))
                                }
                            }
                            (Some(cx), None) => Some((true, cx)),
                            (None, Some(cy)) => Some((false, cy)),
                            (None, None) => None,
                        };

                        if let Some((is_x, (_dist, ratio, pos, kind))) = chosen {
                            if proportional {
                                ts.scale_x *= ratio;
                                ts.scale_y *= ratio;
                            } else if is_x {
                                ts.scale_x *= ratio;
                            } else {
                                ts.scale_y *= ratio;
                            }
                            if proportional || is_x {
                                ts.translate_x = a_x - pcx - ts.scale_x * (ax_orig - pcx);
                            }
                            if proportional || !is_x {
                                ts.translate_y = a_y - pcy - ts.scale_y * (ay_orig - pcy);
                            }
                            guides.push(SnapLine {
                                vertical: is_x,
                                pos,
                                kind,
                            });
                        }
                    }
                }
            }
        }

        self.edit.transform_snap_guides = guides;
        self.update_transform_preview();
        self.request_interactive_recompose();
    }

    pub fn transform_on_release(&mut self) {
        if let Some(ts) = self.edit.transform_state.as_mut() {
            ts.drag_handle = None;
        }
        self.edit.transform_snap_guides.clear();
        // Move-tool transforms are direct manipulation, not a modal Ctrl+T
        // session. Commit one undoable operation as soon as the gesture ends;
        // the idle selection box is rebuilt from the resulting layer bounds.
        if self.edit.tools.active_id() == crate::tools::ToolId::Move {
            self.commit_transform();
        }
    }

    pub fn transform_set_scale_x(&mut self, v: f32) {
        if let Some(ts) = self.edit.transform_state.as_mut() {
            let next = if v < 0.0 {
                v.min(-MIN_LIVE_SCALE)
            } else {
                v.max(MIN_LIVE_SCALE)
            };
            if let Some(q) = ts.quad.as_mut() {
                transform_quad_about_center(q, next / ts.scale_x, 1.0, 0.0);
            }
            ts.scale_x = next;
        }
        self.update_transform_preview();
        self.recomposite_visible();
    }
    pub fn transform_set_scale_y(&mut self, v: f32) {
        if let Some(ts) = self.edit.transform_state.as_mut() {
            let next = if v < 0.0 {
                v.min(-MIN_LIVE_SCALE)
            } else {
                v.max(MIN_LIVE_SCALE)
            };
            if let Some(q) = ts.quad.as_mut() {
                transform_quad_about_center(q, 1.0, next / ts.scale_y, 0.0);
            }
            ts.scale_y = next;
        }
        self.update_transform_preview();
        self.recomposite_visible();
    }
    pub fn transform_set_angle(&mut self, deg: f32) {
        if let Some(ts) = self.edit.transform_state.as_mut() {
            if let Some(q) = ts.quad.as_mut() {
                transform_quad_about_center(q, 1.0, 1.0, deg - ts.angle_deg);
            }
            ts.angle_deg = deg;
        }
        self.update_transform_preview();
        self.recomposite_visible();
    }
    pub fn transform_set_translate_x(&mut self, v: f32) {
        if let Some(ts) = self.edit.transform_state.as_mut() {
            if let Some(q) = ts.quad.as_mut() {
                let dx = v - ts.translate_x;
                for p in q {
                    p.0 += dx;
                }
            }
            ts.translate_x = v;
        }
        self.update_transform_preview();
        self.recomposite_visible();
    }
    pub fn transform_set_translate_y(&mut self, v: f32) {
        if let Some(ts) = self.edit.transform_state.as_mut() {
            if let Some(q) = ts.quad.as_mut() {
                let dy = v - ts.translate_y;
                for p in q {
                    p.1 += dy;
                }
            }
            ts.translate_y = v;
        }
        self.update_transform_preview();
        self.recomposite_visible();
    }

    /// Restore the live transform to the state in which this session began.
    pub fn transform_reset(&mut self) {
        let Some(ts) = self.edit.transform_state.as_mut() else {
            return;
        };
        ts.scale_x = 1.0;
        ts.scale_y = 1.0;
        ts.angle_deg = 0.0;
        ts.translate_x = 0.0;
        ts.translate_y = 0.0;
        ts.drag_handle = None;
        ts.quad = None;
        ts.mode = crate::app::state::TransformMode::Free;
        self.edit.transform_snap_guides.clear();
        self.update_transform_preview();
        self.recomposite_visible();
        self.shell.status_msg = "Free Transform reset".to_string();
    }

    pub fn transform_set_mode(&mut self, mode: crate::app::state::TransformMode) {
        let Some(ts) = self.edit.transform_state.as_mut() else {
            return;
        };
        // A path stays a path only under scale, turn and move.
        if mode != crate::app::state::TransformMode::Free && ts.has_path() {
            self.shell.status_msg = "Có đường path trong vùng chọn: Skew / Distort / Perspective \
                 sẽ biến path thành ảnh — Rasterize path trước nếu muốn kéo méo"
                .to_string();
            return;
        }
        if mode != crate::app::state::TransformMode::Free && ts.quad.is_none() {
            let c = ts.corners();
            ts.quad = Some([c[0], c[1], c[3], c[2]]);
        }
        ts.mode = mode;
        ts.drag_handle = None;
        self.edit.transform_snap_guides.clear();
        self.update_transform_preview();
        self.recomposite_visible();
        self.shell.status_msg = format!("Free Transform — {mode:?}");
    }

    /// Warp uses the existing mesh editor. The pending transform is committed
    /// first when necessary; an unchanged transform can switch immediately.
    pub fn transform_start_warp(&mut self) {
        let unchanged = self.edit.transform_state.as_ref().is_some_and(|ts| {
            ts.quad.is_none()
                && ts.scale_x == 1.0
                && ts.scale_y == 1.0
                && ts.angle_deg == 0.0
                && ts.translate_x == 0.0
                && ts.translate_y == 0.0
        });
        if unchanged {
            self.cancel_transform();
            self.begin_warp();
        } else {
            self.edit.warp_after_transform_commit = true;
            self.commit_transform();
            if self.edit.pending_transform_commit.is_none() {
                self.edit.warp_after_transform_commit = false;
                self.begin_warp();
            }
        }
    }

    pub fn transform_cursor_hint(&self) -> u8 {
        let Some(ts) = &self.edit.transform_state else {
            return 0;
        };
        let zoom = self.edit.view.zoom;
        let vox = self.edit.view.offset_x;
        let voy = self.edit.view.offset_y;
        let mx = self.edit.input.mouse_x;
        let my = self.edit.input.mouse_y;
        let c2s = |cx: f32, cy: f32| -> (f32, f32) { (cx * zoom + vox, cy * zoom + voy) };

        let handles = ts.handle_positions();
        for (i, &(hcx, hcy)) in handles.iter().enumerate() {
            let (shx, shy) = c2s(hcx, hcy);
            if ((mx - shx).powi(2) + (my - shy).powi(2)).sqrt() < HANDLE_RADIUS {
                let local_dirs = [
                    (1.0, 1.0),  // TL
                    (0.0, 1.0),  // TC
                    (-1.0, 1.0), // TR
                    (1.0, 0.0),  // ML
                    (1.0, 0.0),  // MR
                    (-1.0, 1.0), // BL
                    (0.0, 1.0),  // BC
                    (1.0, 1.0),  // BR
                ];
                let (lx, ly) = local_dirs[i];
                let sx = lx * ts.scale_x.signum();
                let sy = ly * ts.scale_y.signum();
                let rad = ts.angle_deg.to_radians();
                let c = rad.cos();
                let s = rad.sin();
                let screen_dx = c * sx - s * sy;
                let screen_dy = s * sx + c * sy;

                let mut angle = screen_dy.atan2(screen_dx).to_degrees();
                while angle < 0.0 {
                    angle += 180.0;
                }
                while angle >= 180.0 {
                    angle -= 180.0;
                }

                let idx = ((angle / 45.0).round() as i32).rem_euclid(4);
                return 10 + idx as u8;
            }
        }

        let cx = (mx - vox) / zoom;
        let cy = (my - voy) / zoom;
        let Some((orig_cx, orig_cy)) = ts.inverse_canvas_point(cx, cy) else {
            return 0;
        };
        let ox0 = ts.orig_offset.0 as f32;
        let oy0 = ts.orig_offset.1 as f32;
        let ox1 = ox0 + ts.orig_w as f32;
        let oy1 = oy0 + ts.orig_h as f32;
        if orig_cx >= ox0 && orig_cx <= ox1 && orig_cy >= oy0 && orig_cy <= oy1 {
            return 0;
        }

        let rz = ROTATE_ZONE / zoom;
        if orig_cx >= ox0 - rz && orig_cx <= ox1 + rz && orig_cy >= oy0 - rz && orig_cy <= oy1 + rz
        {
            return 1;
        }

        0
    }

    pub fn transform_flip_horizontal(&mut self) {
        if let Some(ts) = self.edit.transform_state.as_mut() {
            if let Some(q) = ts.quad.as_mut() {
                transform_quad_about_center(q, -1.0, 1.0, 0.0);
            }
            ts.scale_x *= -1.0;
        }
        self.update_transform_preview();
        self.recomposite_visible();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
    pub fn transform_flip_vertical(&mut self) {
        if let Some(ts) = self.edit.transform_state.as_mut() {
            if let Some(q) = ts.quad.as_mut() {
                transform_quad_about_center(q, 1.0, -1.0, 0.0);
            }
            ts.scale_y *= -1.0;
        }
        self.update_transform_preview();
        self.recomposite_visible();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
    pub fn transform_rotate_90cw(&mut self) {
        if let Some(ts) = self.edit.transform_state.as_mut() {
            if let Some(q) = ts.quad.as_mut() {
                transform_quad_about_center(q, 1.0, 1.0, 90.0);
            }
            ts.angle_deg += 90.0;
        }
        self.update_transform_preview();
        self.recomposite_visible();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
    pub fn transform_rotate_90ccw(&mut self) {
        if let Some(ts) = self.edit.transform_state.as_mut() {
            if let Some(q) = ts.quad.as_mut() {
                transform_quad_about_center(q, 1.0, 1.0, -90.0);
            }
            ts.angle_deg -= 90.0;
        }
        self.update_transform_preview();
        self.recomposite_visible();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
    pub fn transform_rotate_180(&mut self) {
        if let Some(ts) = self.edit.transform_state.as_mut() {
            if let Some(q) = ts.quad.as_mut() {
                transform_quad_about_center(q, 1.0, 1.0, 180.0);
            }
            ts.angle_deg += 180.0;
        }
        self.update_transform_preview();
        self.recomposite_visible();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::Canvas;
    use crate::core::shape::ShapeData;
    use crate::core::shape::ShapeKind;
    use crate::core::text::rasterize_placed;

    #[test]
    fn shift_constrains_transform_move_to_horizontal_axis() {
        assert_eq!(constrained_move_delta(24.0, 7.0, true), (24.0, 0.0));
        assert_eq!(constrained_move_delta(-24.0, 7.0, true), (-24.0, 0.0));
    }

    #[test]
    fn shift_constrains_transform_move_to_vertical_axis() {
        assert_eq!(constrained_move_delta(7.0, 24.0, true), (0.0, 24.0));
        assert_eq!(constrained_move_delta(7.0, -24.0, true), (0.0, -24.0));
    }

    #[test]
    fn transform_move_remains_free_without_shift() {
        assert_eq!(constrained_move_delta(24.0, 7.0, false), (24.0, 7.0));
    }

    #[test]
    fn free_transform_corner_tracks_pointer_without_doubling_drag() {
        let mut app = crate::app::state::App::new();
        app.shell.ui.snap_enabled = false;
        app.edit.transform_state = Some(TransformState {
            layer_states: Vec::new(),
            preview_layer_states: Vec::new(),
            layer_idx: 0,
            layer_id: 1,
            orig_offset: (0, 0),
            orig_w: 100,
            orig_h: 100,
            scale_x: 1.0,
            scale_y: 1.0,
            angle_deg: 0.0,
            translate_x: 0.0,
            translate_y: 0.0,
            pivot_cx: 50.0,
            pivot_cy: 50.0,
            drag_handle: Some(Some(TransformHandle::TopLeft)),
            drag_start_cx: 0.0,
            drag_start_cy: 0.0,
            drag_start_sx: 1.0,
            drag_start_sy: 1.0,
            drag_start_angle: 0.0,
            drag_start_tx: 0.0,
            drag_start_ty: 0.0,
            quad: None,
            drag_start_quad: [(0.0, 0.0); 4],
            mode: crate::app::state::TransformMode::Free,
        });

        app.transform_on_drag(-10.0, -10.0, false, false);

        let handles = app
            .edit
            .transform_state
            .as_ref()
            .expect("transform remains active")
            .handle_positions();
        assert!((handles[0].0 + 10.0).abs() < 0.001);
        assert!((handles[0].1 + 10.0).abs() < 0.001);
        assert!((handles[7].0 - 100.0).abs() < 0.001);
        assert!((handles[7].1 - 100.0).abs() < 0.001);
    }

    #[test]
    fn undo_during_free_transform_reverts_pending_only() {
        let mut app = crate::app::state::App::new();
        let undo_before = app.docs.documents[0].canvas.undo_count();
        app.edit.transform_state = Some(TransformState {
            layer_states: Vec::new(),
            preview_layer_states: Vec::new(),
            layer_idx: 0,
            layer_id: 1,
            orig_offset: (0, 0),
            orig_w: 4,
            orig_h: 4,
            scale_x: 2.0,
            scale_y: 0.5,
            angle_deg: 45.0,
            translate_x: 10.0,
            translate_y: -3.0,
            pivot_cx: 2.0,
            pivot_cy: 2.0,
            drag_handle: None,
            drag_start_cx: 0.0,
            drag_start_cy: 0.0,
            drag_start_sx: 1.0,
            drag_start_sy: 1.0,
            drag_start_angle: 0.0,
            drag_start_tx: 0.0,
            drag_start_ty: 0.0,
            quad: None,
            drag_start_quad: [(0.0, 0.0); 4],
            mode: crate::app::state::TransformMode::Free,
        });

        assert!(app.transform_undo_pending(), "session consumes the undo");
        let ts = app
            .edit
            .transform_state
            .as_ref()
            .expect("session stays open");
        assert_eq!(
            (
                ts.scale_x,
                ts.scale_y,
                ts.angle_deg,
                ts.translate_x,
                ts.translate_y
            ),
            (1.0, 1.0, 0.0, 0.0, 0.0),
            "pending transform reverts to identity"
        );
        assert!(
            app.transform_undo_pending(),
            "undo stays consumed at identity"
        );
        assert_eq!(
            app.docs.documents[0].canvas.undo_count(),
            undo_before,
            "document history is untouched"
        );
    }
    use crate::core::command::{Command, EditContext};
    use crate::core::document::DocumentId;
    use crate::core::layer::{LayerMask, LayerStack};
    use crate::core::text::{TextData, TextFontFamily, rasterize};
    use crate::core::tile::TileMap;

    #[test]
    fn bake_resamples_mask_with_layer() {
        // 2x2 opaque layer; mask: left column black, right column white.
        let pixels = [255u8, 0, 0, 255].repeat(4);
        let mask_px: Vec<u8> = [
            [0u8, 0, 0, 255],
            [255, 255, 255, 255],
            [0, 0, 0, 255],
            [255, 255, 255, 255],
        ]
        .concat();
        let ls = LayerOrigState {
            layer_id: 7,
            layer_idx: 0,
            layer_type: LayerType::Raster,
            tiles: TileMap::from_rgba(&pixels, 2, 2),
            mask: Some(LayerMask {
                tiles: TileMap::from_rgba(&mask_px, 2, 2),
                width: 2,
                height: 2,
                enabled: true,
                inverted: false,
                bake_offset: (0, 0),
                bake_frame_offset: (0, 0),
            }),
            offset: (0, 0),
            width: 2,
            height: 2,
            content_offset: (0, 0),
            content_w: 2,
            content_h: 2,
            source_to_current: MAT3_IDENTITY,
            before: None,
        };
        let ts = TransformState {
            layer_states: vec![ls],
            preview_layer_states: Vec::new(),
            layer_idx: 0,
            layer_id: 7,
            orig_offset: (0, 0),
            orig_w: 2,
            orig_h: 2,
            scale_x: 2.0,
            scale_y: 2.0,
            angle_deg: 0.0,
            translate_x: 0.0,
            translate_y: 0.0,
            pivot_cx: 1.0,
            pivot_cy: 1.0,
            drag_handle: None,
            drag_start_cx: 0.0,
            drag_start_cy: 0.0,
            drag_start_sx: 1.0,
            drag_start_sy: 1.0,
            drag_start_angle: 0.0,
            drag_start_tx: 0.0,
            drag_start_ty: 0.0,
            quad: None,
            drag_start_quad: [(0.0, 0.0); 4],
            mode: crate::app::state::TransformMode::Free,
        };

        let result = bake_transform_commit(
            DocumentId(1),
            ts,
            crate::core::geometry::InterpolationMode::Bilinear,
        )
        .expect("bake succeeds");

        assert_eq!(result.layers.len(), 1);
        let layer = &result.layers[0];
        assert_eq!((layer.width, layer.height), (4, 4));
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(
                    layer.tiles.get_pixel(x, y),
                    (255, 0, 0, 255),
                    "opaque source edge must not fade at ({x},{y})"
                );
            }
        }
        let mask = layer.mask.as_ref().expect("mask survives the transform");
        assert_eq!((mask.width, mask.height), (4, 4));
        assert!(mask.enabled);
        // 2x scale keeps left side black, right side white.
        assert!(mask.tiles.get_pixel(0, 1).0 < 10, "left edge stays black");
        assert!(mask.tiles.get_pixel(3, 1).0 > 245, "right edge stays white");
    }

    #[test]
    fn repeated_raster_transforms_render_from_the_original_source() {
        let pixels = [0u8, 180, 80, 255].repeat(64);
        let source = LayerOrigState {
            layer_id: 17,
            layer_idx: 0,
            layer_type: LayerType::Raster,
            tiles: TileMap::from_rgba(&pixels, 8, 8),
            mask: None,
            offset: (20, 20),
            width: 8,
            height: 8,
            content_offset: (20, 20),
            content_w: 8,
            content_h: 8,
            source_to_current: MAT3_IDENTITY,
            before: None,
        };
        let make_state = |ls: LayerOrigState, offset, w, h, angle| TransformState {
            layer_states: vec![ls],
            preview_layer_states: Vec::new(),
            layer_idx: 0,
            layer_id: 17,
            orig_offset: offset,
            orig_w: w,
            orig_h: h,
            scale_x: 1.0,
            scale_y: 1.0,
            angle_deg: angle,
            translate_x: 0.0,
            translate_y: 0.0,
            pivot_cx: offset.0 as f32 + w as f32 * 0.5,
            pivot_cy: offset.1 as f32 + h as f32 * 0.5,
            drag_handle: None,
            drag_start_cx: 0.0,
            drag_start_cy: 0.0,
            drag_start_sx: 1.0,
            drag_start_sy: 1.0,
            drag_start_angle: 0.0,
            drag_start_tx: 0.0,
            drag_start_ty: 0.0,
            quad: None,
            drag_start_quad: [(0.0, 0.0); 4],
            mode: crate::app::state::TransformMode::Free,
        };

        let first = bake_transform_commit(
            DocumentId(1),
            make_state(source, (20, 20), 8, 8, 20.0),
            crate::core::geometry::InterpolationMode::Bilinear,
        )
        .unwrap();
        let first_layer = &first.layers[0];
        let lineage = first_layer.lineage.clone().expect("raster keeps source");
        let mut second_source = lineage.source;
        second_source.source_to_current = lineage.source_to_current;
        let second = bake_transform_commit(
            DocumentId(1),
            make_state(
                second_source,
                first_layer.offset,
                first_layer.width,
                first_layer.height,
                -20.0,
            ),
            crate::core::geometry::InterpolationMode::Bilinear,
        )
        .unwrap();
        let restored = &second.layers[0];
        assert_eq!((restored.width, restored.height), (8, 8));
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(restored.tiles.get_pixel(x, y), (0, 180, 80, 255));
            }
        }
    }

    #[test]
    fn text_scale_updates_editable_font_metadata() {
        let td = TextData {
            content: "Scale".to_string(),
            font_family: TextFontFamily::DejaVuSans,
            font_px: 24.0,
            ..TextData::default()
        };
        let Some(raster) = rasterize(&td) else {
            return;
        };
        let tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
        let Some((min_x, min_y, max_x, max_y)) = tiles.content_bounds() else {
            return;
        };
        let offset = (12, 17);
        let content_w = (max_x - min_x).max(1) as u32;
        let content_h = (max_y - min_y).max(1) as u32;
        let content_offset = (offset.0 + min_x, offset.1 + min_y);
        let ls = LayerOrigState {
            layer_id: 7,
            layer_idx: 0,
            layer_type: LayerType::Text(td.clone()),
            tiles: tiles.clone(),
            mask: None,
            offset,
            width: raster.width,
            height: raster.height,
            content_offset,
            content_w,
            content_h,
            source_to_current: MAT3_IDENTITY,
            before: None,
        };
        let ts = TransformState {
            layer_states: vec![ls],
            preview_layer_states: Vec::new(),
            layer_idx: 0,
            layer_id: 7,
            orig_offset: content_offset,
            orig_w: content_w,
            orig_h: content_h,
            scale_x: 2.0,
            scale_y: 1.0,
            angle_deg: 0.0,
            translate_x: 0.0,
            translate_y: 0.0,
            pivot_cx: content_offset.0 as f32 + content_w as f32 * 0.5,
            pivot_cy: content_offset.1 as f32 + content_h as f32 * 0.5,
            drag_handle: None,
            drag_start_cx: 0.0,
            drag_start_cy: 0.0,
            drag_start_sx: 1.0,
            drag_start_sy: 1.0,
            drag_start_angle: 0.0,
            drag_start_tx: 0.0,
            drag_start_ty: 0.0,
            quad: None,
            drag_start_quad: [(0.0, 0.0); 4],
            mode: crate::app::state::TransformMode::Free,
        };

        let target_bounds =
            transformed_content_bounds(&ts, &ts.layer_states[0]).expect("scaled bounds are valid");
        let gesture = ts.clone();
        let result = bake_transform_commit(
            DocumentId(1),
            ts,
            crate::core::geometry::InterpolationMode::Bilinear,
        )
        .expect("bake succeeds");

        let LayerType::Text(after_td) = &result.layers[0].layer_type else {
            panic!("scaled text stays editable");
        };
        assert!((after_td.font_px - 24.0).abs() < 0.01);
        assert!((after_td.stretch_x - 2.0).abs() < 0.01);
        assert!(result.layers[0].width > raster.width);

        // The committed pixels must be the exact raster described by TextData.
        // Type mode blanks these pixels and reconstructs them from the metadata;
        // a mismatch here is the shrink-on-edit regression.
        let (rebuilt, _) = rasterize_placed(after_td).expect("metadata rerasterizes");
        let rebuilt_tiles = TileMap::from_rgba(&rebuilt.rgba, rebuilt.width, rebuilt.height);
        assert_eq!(result.layers[0].width, rebuilt.width);
        assert_eq!(result.layers[0].height, rebuilt.height);
        assert_eq!(
            tile_content_fingerprint(&result.layers[0].tiles),
            tile_content_fingerprint(&rebuilt_tiles)
        );

        assert_text_placed_at_mapped_origin(&td, &tiles, offset, &gesture, &result.layers[0]);

        // An axis-aligned scale maps the ink box onto the destination box, so
        // the committed ink must land there too — only font hinting separates
        // the re-laid glyphs from a plain resample of the old bitmap.
        let (min_x, min_y, max_x, max_y) = result.layers[0]
            .tiles
            .content_bounds()
            .expect("committed text has ink");
        let actual_cx = result.layers[0].offset.0 as f64 + (min_x + max_x) as f64 * 0.5;
        let actual_cy = result.layers[0].offset.1 as f64 + (min_y + max_y) as f64 * 0.5;
        let target_cx = target_bounds.0 as f64 + target_bounds.2 as f64 * 0.5;
        let target_cy = target_bounds.1 as f64 + target_bounds.3 as f64 * 0.5;
        assert!((actual_cx - target_cx).abs() <= 2.0);
        assert!((actual_cy - target_cy).abs() <= 2.0);

        let mut stack = LayerStack::new(160, 120);
        let idx = stack.add_layer(160, 120);
        stack.layers[idx].id = 7;
        stack.layers[idx].layer_type = LayerType::Text(td.clone());
        stack.layers[idx].tiles = tiles;
        stack.layers[idx].offset = offset;
        stack.layers[idx].width = raster.width;
        stack.layers[idx].height = raster.height;

        let mut command = result.command;
        let mut canvas_w = 160;
        let mut canvas_h = 120;
        let mut ctx = EditContext::new(&mut stack, &mut canvas_w, &mut canvas_h, None);
        command.execute(&mut ctx).expect("redo applies");
        let LayerType::Text(redo_td) = &ctx.layers.layers[idx].layer_type else {
            panic!("redo keeps text layer");
        };
        assert!((redo_td.font_px - 24.0).abs() < 0.01);
        assert!((redo_td.stretch_x - 2.0).abs() < 0.01);

        command.undo(&mut ctx).expect("undo applies");
        let LayerType::Text(undo_td) = &ctx.layers.layers[idx].layer_type else {
            panic!("undo keeps text layer");
        };
        assert!((undo_td.font_px - 24.0).abs() < 0.01);
        assert!((undo_td.stretch_x - 1.0).abs() < 0.01);
    }

    #[test]
    fn large_text_scale_does_not_snap_to_the_old_font_limit() {
        let td = TextData {
            content: "A".to_string(),
            font_px: 32.0,
            ..TextData::default()
        };
        let scaled = scaled_text_data(&td, 64.0, 64.0).expect("2048px is within the font limits");
        assert!((scaled.font_px - 2048.0).abs() < 0.01);
        assert!((scaled.stretch_x - 1.0).abs() < 0.01);
    }

    #[test]
    fn text_scale_past_the_font_limit_gives_up_the_editable_metadata() {
        let td = TextData {
            content: "A".to_string(),
            font_px: 32.0,
            ..TextData::default()
        };
        // 32 × 200 is far past MAX_EDITABLE_FONT_PX. Silently clamping used to
        // commit type much smaller than the preview showed.
        assert!(scaled_text_data(&td, 200.0, 200.0).is_none());
        // ...and the same below the minimum size.
        assert!(scaled_text_data(&td, 0.05, 0.05).is_none());
    }

    #[test]
    fn distorting_rotated_text_gives_up_the_editable_metadata() {
        let td = TextData {
            content: "Skew".to_string(),
            font_px: 24.0,
            rotation_deg: 30.0,
            ..TextData::default()
        };
        let mut ts = identity_text_transform_state();
        // A non-uniform scale runs along the canvas axes; font size and
        // stretch_x run along the text's baseline. They only agree upright.
        ts.scale_x = 2.0;
        ts.scale_y = 1.0;
        assert!(transformed_text_data(&td, &ts).is_none());

        // The same distort on upright text stays editable.
        let upright = TextData {
            rotation_deg: 0.0,
            ..td.clone()
        };
        assert!(transformed_text_data(&upright, &ts).is_some());

        // So does a uniform scale on the rotated block.
        ts.scale_y = 2.0;
        assert!(transformed_text_data(&td, &ts).is_some());
    }

    /// A do-nothing `TransformState` for the text-metadata unit tests above
    /// (they exercise `transformed_text_data`, never the bake).
    fn identity_text_transform_state() -> TransformState {
        TransformState {
            layer_states: Vec::new(),
            preview_layer_states: Vec::new(),
            layer_idx: 0,
            layer_id: 1,
            orig_offset: (0, 0),
            orig_w: 1,
            orig_h: 1,
            scale_x: 1.0,
            scale_y: 1.0,
            angle_deg: 0.0,
            translate_x: 0.0,
            translate_y: 0.0,
            pivot_cx: 0.0,
            pivot_cy: 0.0,
            drag_handle: None,
            drag_start_cx: 0.0,
            drag_start_cy: 0.0,
            drag_start_sx: 1.0,
            drag_start_sy: 1.0,
            drag_start_angle: 0.0,
            drag_start_tx: 0.0,
            drag_start_ty: 0.0,
            quad: None,
            drag_start_quad: [(0.0, 0.0); 4],
            mode: crate::app::state::TransformMode::Free,
        }
    }

    /// The committed type must sit at the transform's image of the block's old
    /// origin — the point its rotation, flips and stretch pivot around.
    /// Centring the fresh ink inside the transformed bounding box instead
    /// drifts whenever the ink does not fill that box (ragged or centred
    /// paragraphs, tall descenders), which is the visible jump on Enter.
    fn assert_text_placed_at_mapped_origin(
        before: &TextData,
        before_tiles: &TileMap,
        before_offset: (i32, i32),
        gesture: &TransformState,
        committed: &crate::app::state::TransformCommitLayer,
    ) {
        let LayerType::Text(after_td) = &committed.layer_type else {
            panic!("committed layer is no longer editable type");
        };
        let origin =
            text_layer_origin(before, before_tiles, before_offset).expect("origin recovers");
        let forward = transform_forward_homography(gesture).expect("affine gesture");
        let moved = crate::core::geometry::Homography { m: forward }.apply(origin.0, origin.1);
        let (_, delta) = rasterize_placed(after_td).expect("committed type rerasterizes");
        assert_eq!(
            committed.offset,
            (
                moved.x.round() as i32 + delta.0,
                moved.y.round() as i32 + delta.1,
            )
        );
    }

    #[test]
    fn text_rotation_keeps_editable_text_metadata() {
        let td = TextData {
            content: "Turn".to_string(),
            font_family: TextFontFamily::DejaVuSans,
            font_px: 24.0,
            ..TextData::default()
        };
        let Some(raster) = rasterize(&td) else {
            return;
        };
        let tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
        let Some((min_x, min_y, max_x, max_y)) = tiles.content_bounds() else {
            return;
        };
        let offset = (10, 10);
        let content_w = (max_x - min_x).max(1) as u32;
        let content_h = (max_y - min_y).max(1) as u32;
        let content_offset = (offset.0 + min_x, offset.1 + min_y);
        let ls = LayerOrigState {
            layer_id: 3,
            layer_idx: 0,
            layer_type: LayerType::Text(td.clone()),
            tiles,
            mask: None,
            offset,
            width: raster.width,
            height: raster.height,
            content_offset,
            content_w,
            content_h,
            source_to_current: MAT3_IDENTITY,
            before: None,
        };
        let ts = TransformState {
            layer_states: vec![ls],
            preview_layer_states: Vec::new(),
            layer_idx: 0,
            layer_id: 3,
            orig_offset: content_offset,
            orig_w: content_w,
            orig_h: content_h,
            scale_x: 1.0,
            scale_y: 1.0,
            angle_deg: 30.0,
            translate_x: 0.0,
            translate_y: 0.0,
            pivot_cx: content_offset.0 as f32 + content_w as f32 * 0.5,
            pivot_cy: content_offset.1 as f32 + content_h as f32 * 0.5,
            drag_handle: None,
            drag_start_cx: 0.0,
            drag_start_cy: 0.0,
            drag_start_sx: 1.0,
            drag_start_sy: 1.0,
            drag_start_angle: 0.0,
            drag_start_tx: 0.0,
            drag_start_ty: 0.0,
            quad: None,
            drag_start_quad: [(0.0, 0.0); 4],
            mode: crate::app::state::TransformMode::Free,
        };

        let source_tiles = ts.layer_states[0].tiles.clone();
        let gesture = ts.clone();
        let result = bake_transform_commit(
            DocumentId(1),
            ts,
            crate::core::geometry::InterpolationMode::Bilinear,
        )
        .expect("bake succeeds");

        let LayerType::Text(after_td) = &result.layers[0].layer_type else {
            panic!("rotated text stays editable");
        };
        assert!((after_td.rotation_deg - 30.0).abs() < 0.01);
        assert_text_placed_at_mapped_origin(
            &td,
            &source_tiles,
            offset,
            &gesture,
            &result.layers[0],
        );
    }

    #[test]
    fn text_flip_keeps_editable_text_metadata() {
        let mut td = TextData {
            content: "Flip".to_string(),
            font_family: TextFontFamily::DejaVuSans,
            font_px: 24.0,
            ..TextData::default()
        };
        td.glyph_styles = td
            .content
            .chars()
            .map(|_| crate::core::text::GlyphStyle {
                color: [20, 120, 220, 255],
                font_px: 30.0,
                font_family: td.font_family.clone(),
                bold: td.bold,
                italic: td.italic,
                underline: td.underline,
            })
            .collect();
        let Some(raster) = rasterize(&td) else {
            return;
        };
        let tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
        let Some((min_x, min_y, max_x, max_y)) = tiles.content_bounds() else {
            return;
        };
        let offset = (10, 10);
        let content_w = (max_x - min_x).max(1) as u32;
        let content_h = (max_y - min_y).max(1) as u32;
        let content_offset = (offset.0 + min_x, offset.1 + min_y);
        let ls = LayerOrigState {
            layer_id: 4,
            layer_idx: 0,
            layer_type: LayerType::Text(td.clone()),
            tiles,
            mask: None,
            offset,
            width: raster.width,
            height: raster.height,
            content_offset,
            content_w,
            content_h,
            source_to_current: MAT3_IDENTITY,
            before: None,
        };
        let ts = TransformState {
            layer_states: vec![ls],
            preview_layer_states: Vec::new(),
            layer_idx: 0,
            layer_id: 4,
            orig_offset: content_offset,
            orig_w: content_w,
            orig_h: content_h,
            scale_x: -1.0,
            scale_y: 1.0,
            angle_deg: 0.0,
            translate_x: 0.0,
            translate_y: 0.0,
            pivot_cx: content_offset.0 as f32 + content_w as f32 * 0.5,
            pivot_cy: content_offset.1 as f32 + content_h as f32 * 0.5,
            drag_handle: None,
            drag_start_cx: 0.0,
            drag_start_cy: 0.0,
            drag_start_sx: 1.0,
            drag_start_sy: 1.0,
            drag_start_angle: 0.0,
            drag_start_tx: 0.0,
            drag_start_ty: 0.0,
            quad: None,
            drag_start_quad: [(0.0, 0.0); 4],
            mode: crate::app::state::TransformMode::Free,
        };

        let result = bake_transform_commit(
            DocumentId(1),
            ts,
            crate::core::geometry::InterpolationMode::Bilinear,
        )
        .expect("bake succeeds");

        let LayerType::Text(after_td) = &result.layers[0].layer_type else {
            panic!("flipped text stays editable");
        };
        assert!(after_td.flip_x);
        assert!(!after_td.flip_y);
        assert_eq!(after_td.glyph_styles, td.glyph_styles);
    }

    /// Build a Shape layer's LayerOrigState from a canvas span, mirroring how
    /// shape_ops creates the layer (render → tiles at the span's offset).
    fn shape_orig_state(kind: ShapeKind, layer_id: u32) -> (ShapeData, LayerOrigState) {
        let (sd, off) = ShapeData::from_canvas_span(
            kind,
            10.0,
            10.0,
            50.0,
            40.0,
            6.0,
            true,
            [200, 30, 30, 255],
            2.0,
            [0, 0, 0, 255],
        );
        let raster = sd.render().expect("shape renders");
        let tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
        let (min_x, min_y, max_x, max_y) = tiles.content_bounds().expect("has content");
        let content_offset = (off.0 + min_x, off.1 + min_y);
        let content_w = (max_x - min_x).max(1) as u32;
        let content_h = (max_y - min_y).max(1) as u32;
        let ls = LayerOrigState {
            layer_id,
            layer_idx: 0,
            layer_type: LayerType::Vector(VectorGeometry::Primitive(sd.clone())),
            tiles,
            mask: None,
            offset: off,
            width: raster.width,
            height: raster.height,
            content_offset,
            content_w,
            content_h,
            source_to_current: MAT3_IDENTITY,
            before: None,
        };
        (sd, ls)
    }

    fn shape_transform_state(ls: LayerOrigState, sx: f32, sy: f32, angle: f32) -> TransformState {
        let orig_offset = ls.content_offset;
        let orig_w = ls.content_w;
        let orig_h = ls.content_h;
        TransformState {
            layer_states: vec![ls],
            preview_layer_states: Vec::new(),
            layer_idx: 0,
            layer_id: 9,
            orig_offset,
            orig_w,
            orig_h,
            scale_x: sx,
            scale_y: sy,
            angle_deg: angle,
            translate_x: 0.0,
            translate_y: 0.0,
            pivot_cx: orig_offset.0 as f32 + orig_w as f32 * 0.5,
            pivot_cy: orig_offset.1 as f32 + orig_h as f32 * 0.5,
            drag_handle: None,
            drag_start_cx: 0.0,
            drag_start_cy: 0.0,
            drag_start_sx: 1.0,
            drag_start_sy: 1.0,
            drag_start_angle: 0.0,
            drag_start_tx: 0.0,
            drag_start_ty: 0.0,
            quad: None,
            drag_start_quad: [(0.0, 0.0); 4],
            mode: crate::app::state::TransformMode::Free,
        }
    }

    #[test]
    fn shape_scale_updates_geometry_and_rerenders_crisp() {
        let (sd, ls) = shape_orig_state(ShapeKind::Rectangle, 9);
        let ts = shape_transform_state(ls, 2.0, 2.0, 0.0);
        let result = bake_transform_commit(
            DocumentId(1),
            ts,
            crate::core::geometry::InterpolationMode::Bilinear,
        )
        .expect("bake succeeds");

        let layer = &result.layers[0];
        let LayerType::Vector(VectorGeometry::Primitive(after)) = &layer.layer_type else {
            panic!("scaled shape stays editable");
        };
        // The 40×30 span doubles; geometry must follow the raster instead of
        // snapping back to the original size on the next handle/radius edit.
        let (x0, y0, x1, y1) = after.canvas_span(layer.offset);
        assert!(((x1 - x0).abs() - 80.0).abs() <= 1.0, "span w {}", x1 - x0);
        assert!(((y1 - y0).abs() - 60.0).abs() <= 1.0, "span h {}", y1 - y0);
        assert!((after.corner_radius - 2.0 * sd.corner_radius).abs() <= 0.5);
        assert!((after.stroke_width() - 2.0 * sd.stroke_width()).abs() <= 0.1);
        // Crisp vector re-render, not a blurry resample: raster covers the span.
        assert!(layer.width as f32 >= 80.0 && layer.height as f32 >= 60.0);
    }

    #[test]
    fn shape_rotation_converts_rect_to_path_but_keeps_line() {
        let (_, rect_ls) = shape_orig_state(ShapeKind::Rectangle, 9);
        let ts = shape_transform_state(rect_ls, 1.0, 1.0, 45.0);
        let result = bake_transform_commit(
            DocumentId(1),
            ts,
            crate::core::geometry::InterpolationMode::Bilinear,
        )
        .expect("bake succeeds");
        assert!(
            matches!(
                result.layers[0].layer_type,
                LayerType::Vector(VectorGeometry::Path(_))
            ),
            "a rotated rectangle becomes editable curves, never pixels"
        );

        let (line_sd, line_ls) = shape_orig_state(ShapeKind::Line, 9);
        let offset = line_ls.offset;
        let ts = shape_transform_state(line_ls, 1.0, 1.0, 45.0);
        let expect_p0 =
            ts.transform_point(line_sd.x0 + offset.0 as f32, line_sd.y0 + offset.1 as f32);
        let expect_p1 =
            ts.transform_point(line_sd.x1 + offset.0 as f32, line_sd.y1 + offset.1 as f32);
        let result = bake_transform_commit(
            DocumentId(1),
            ts,
            crate::core::geometry::InterpolationMode::Bilinear,
        )
        .expect("bake succeeds");
        let layer = &result.layers[0];
        let LayerType::Vector(VectorGeometry::Primitive(after)) = &layer.layer_type else {
            panic!("a rotated line stays a live shape");
        };
        assert_eq!(after.kind, ShapeKind::Line);
        let (x0, y0, x1, y1) = after.canvas_span(layer.offset);
        assert!((x0 - expect_p0.0).abs() <= 1.0 && (y0 - expect_p0.1).abs() <= 1.0);
        assert!((x1 - expect_p1.0).abs() <= 1.0 && (y1 - expect_p1.1).abs() <= 1.0);
    }

    #[test]
    fn shape_scale_with_mask_keeps_geometry_in_sync_with_resample() {
        let (_, mut ls) = shape_orig_state(ShapeKind::Rectangle, 9);
        // All-white enabled mask so the resample path is taken but nothing hides.
        let mw = ls.width;
        let mh = ls.height;
        let white = vec![255u8; (mw * mh * 4) as usize];
        ls.mask = Some(LayerMask {
            tiles: TileMap::from_rgba(&white, mw, mh),
            width: mw,
            height: mh,
            enabled: true,
            inverted: false,
            bake_offset: (0, 0),
            bake_frame_offset: (0, 0),
        });
        let sd = match &ls.layer_type {
            LayerType::Vector(VectorGeometry::Primitive(sd)) => sd.clone(),
            _ => unreachable!(),
        };
        let ts = shape_transform_state(ls, 2.0, 2.0, 0.0);
        let result = bake_transform_commit(
            DocumentId(1),
            ts,
            crate::core::geometry::InterpolationMode::Bilinear,
        )
        .expect("bake succeeds");
        let layer = &result.layers[0];
        let LayerType::Vector(VectorGeometry::Primitive(after)) = &layer.layer_type else {
            panic!("masked shape keeps editable geometry");
        };
        let (x0, y0, x1, y1) = after.canvas_span(layer.offset);
        assert!(((x1 - x0).abs() - 80.0).abs() <= 1.0);
        assert!(((y1 - y0).abs() - 60.0).abs() <= 1.0);
        assert!((after.corner_radius - 2.0 * sd.corner_radius).abs() <= 0.5);
        assert!(layer.mask.is_some(), "mask survives");
    }

    #[test]
    fn skew_moves_a_whole_edge_and_keeps_a_parallelogram() {
        let q = [(0.0, 0.0), (100.0, 0.0), (100.0, 80.0), (0.0, 80.0)];
        let out = dragged_projective_quad(
            q,
            TransformHandle::TopCenter,
            crate::app::state::TransformMode::Skew,
            15.0,
            9.0,
        );
        assert_eq!(out[0], (15.0, 0.0));
        assert_eq!(out[1], (115.0, 0.0));
        assert_eq!(out[2], q[2]);
        assert_eq!(out[3], q[3]);
    }

    #[test]
    fn distort_moves_only_the_selected_corner() {
        let q = [(0.0, 0.0), (100.0, 0.0), (100.0, 80.0), (0.0, 80.0)];
        let out = dragged_projective_quad(
            q,
            TransformHandle::TopRight,
            crate::app::state::TransformMode::Distort,
            -12.0,
            7.0,
        );
        assert_eq!(out[1], (88.0, 7.0));
        assert_eq!(out[0], q[0]);
        assert_eq!(out[2], q[2]);
        assert_eq!(out[3], q[3]);
    }

    #[test]
    fn perspective_corner_drag_moves_adjacent_edges_symmetrically() {
        let q = [(0.0, 0.0), (100.0, 0.0), (100.0, 80.0), (0.0, 80.0)];
        let out = dragged_projective_quad(
            q,
            TransformHandle::TopLeft,
            crate::app::state::TransformMode::Perspective,
            10.0,
            5.0,
        );
        assert_eq!(out[0], (10.0, 5.0));
        assert_eq!(out[3], (-10.0, 80.0));
        assert_eq!(out[1], (100.0, -5.0));
    }

    // --- Multi-layer audit, 19/10: a transform after a plain move, polygons on
    // their side, and the Move-handle box of a shape. ---

    fn app_with_selected_boxes(
        boxes: &[(i32, i32, u32, u32)],
    ) -> (crate::app::state::App, Vec<u32>) {
        let mut app = crate::app::state::App::new();
        app.docs.documents[0].canvas = Canvas::new(600, 600);
        let canvas = &mut app.docs.documents[0].canvas;
        for layer in &mut canvas.layer_stack.layers {
            layer.selected = false;
        }
        let mut ids = Vec::new();
        for (i, &(x, y, w, h)) in boxes.iter().enumerate() {
            let idx = canvas.layer_stack.add_layer(w, h);
            let mut px = vec![0u8; (w * h * 4) as usize];
            for (n, chunk) in px.chunks_exact_mut(4).enumerate() {
                let column = n as u32 % w;
                let green = if column == 0 { 0 } else { 200 };
                chunk.copy_from_slice(&[60 + 40 * i as u8, green, 90, 255]);
            }
            let layer = &mut canvas.layer_stack.layers[idx];
            layer.tiles = TileMap::from_rgba(&px, w, h);
            layer.offset = (x, y);
            layer.selected = true;
            ids.push(layer.id);
            canvas.layer_stack.active_idx = idx;
        }
        (app, ids)
    }

    fn finish_transform_commit(app: &mut crate::app::state::App) {
        let started = std::time::Instant::now();
        while app.edit.pending_transform_commit.is_some() {
            app.poll_transform_commit();
            assert!(
                started.elapsed() < std::time::Duration::from_secs(60),
                "transform bake never landed"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// Canvas-space content box `(x0, y0, x1, y1)` of the layer with `id`.
    fn content_box(app: &crate::app::state::App, id: u32) -> (i32, i32, i32, i32) {
        let layer = app.docs.documents[0]
            .canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == id)
            .expect("layer");
        let (x0, y0, x1, y1) = layer.tiles.content_bounds().expect("content");
        let (ox, oy) = layer.offset;
        (ox + x0, oy + y0, ox + x1, oy + y1)
    }

    #[test]
    fn transform_after_a_plain_move_keeps_layers_where_they_were_moved() {
        // Scale two photos (the commit remembers their untouched source), move
        // them with the arrow keys and with a drag-only Free Transform, then turn
        // them half way round together. Each must land mirrored about the moved
        // pair's centre — not back where it sat before the moves.
        let (mut app, ids) = app_with_selected_boxes(&[(50, 60, 40, 20), (150, 100, 30, 30)]);
        app.begin_transform();
        app.transform_set_scale_x(1.5);
        app.transform_set_scale_y(1.5);
        app.commit_transform();
        finish_transform_commit(&mut app);

        assert!(app.docs.documents[0].canvas.nudge_selected_layers(40, 25));
        app.begin_transform();
        app.transform_set_translate_x(-15.0);
        app.transform_set_translate_y(30.0);
        app.commit_transform();
        finish_transform_commit(&mut app);

        let moved: Vec<_> = ids.iter().map(|&id| content_box(&app, id)).collect();
        let cx2 =
            moved.iter().map(|b| b.0).min().unwrap() + moved.iter().map(|b| b.2).max().unwrap();
        let cy2 =
            moved.iter().map(|b| b.1).min().unwrap() + moved.iter().map(|b| b.3).max().unwrap();

        app.begin_transform();
        app.transform_set_angle(180.0);
        app.commit_transform();
        finish_transform_commit(&mut app);

        for (&id, before) in ids.iter().zip(&moved) {
            let after = content_box(&app, id);
            // A half turn about c maps [x0, x1] to [2c - x1, 2c - x0].
            let expected = (
                cx2 - before.2,
                cy2 - before.3,
                cx2 - before.0,
                cy2 - before.1,
            );
            let off = [
                after.0 - expected.0,
                after.1 - expected.1,
                after.2 - expected.2,
                after.3 - expected.3,
            ];
            assert!(
                off.iter().all(|d| d.abs() <= 1),
                "layer {id} landed at {after:?}, expected {expected:?} (moved box {before:?})"
            );
        }
    }

    #[test]
    fn mask_painted_after_a_transform_survives_the_next_one() {
        let (mut app, ids) = app_with_selected_boxes(&[(50, 60, 40, 20)]);
        let flat_mask = |w: u32, h: u32, v: u8| LayerMask {
            tiles: TileMap::from_rgba(&[v, v, v, 255].repeat((w * h) as usize), w, h),
            width: w,
            height: h,
            enabled: true,
            inverted: false,
            bake_offset: (0, 0),
            bake_frame_offset: (0, 0),
        };
        let id = ids[0];
        let find = |app: &crate::app::state::App| {
            app.docs.documents[0]
                .canvas
                .layer_stack
                .layers
                .iter()
                .position(|l| l.id == id)
                .expect("layer")
        };
        let idx = find(&app);
        app.docs.documents[0].canvas.layer_stack.layers[idx].mask = Some(flat_mask(40, 20, 255));
        app.begin_transform();
        app.transform_set_scale_x(2.0);
        app.transform_set_scale_y(2.0);
        app.commit_transform();
        finish_transform_commit(&mut app);

        // Paint the whole mask black: pixels untouched, mask changed.
        let idx = find(&app);
        let layer = &mut app.docs.documents[0].canvas.layer_stack.layers[idx];
        let (mw, mh) = {
            let mask = layer.mask.as_ref().expect("mask follows the transform");
            (mask.width, mask.height)
        };
        layer.mask = Some(flat_mask(mw, mh, 0));

        app.begin_transform();
        app.transform_set_angle(180.0);
        app.commit_transform();
        finish_transform_commit(&mut app);

        let idx = find(&app);
        let mask = app.docs.documents[0].canvas.layer_stack.layers[idx]
            .mask
            .clone()
            .expect("mask kept");
        for y in 0..mask.height {
            for x in 0..mask.width {
                assert!(
                    mask.tiles.get_pixel(x, y).0 < 10,
                    "the painted mask came back white at ({x},{y})"
                );
            }
        }
    }

    #[test]
    fn polygon_turned_on_its_side_leaves_shapedata_instead_of_snapping_upright() {
        let bake = |ts| {
            bake_transform_commit(
                DocumentId(1),
                ts,
                crate::core::geometry::InterpolationMode::Bilinear,
            )
            .expect("bake succeeds")
        };
        let (pentagon, ls) = shape_orig_state(ShapeKind::Polygon, 9);
        assert_eq!(pentagon.sides, 5);
        for (sx, sy, angle) in [(1.0, 1.0, 90.0), (1.0, 1.0, 180.0), (1.0, -1.0, 0.0)] {
            let result = bake(shape_transform_state(ls.clone(), sx, sy, angle));
            assert!(
                matches!(
                    result.layers[0].layer_type,
                    LayerType::Vector(VectorGeometry::Path(_))
                ),
                "a pentagon at {angle} deg / scale ({sx},{sy}) cannot stay a pointing-up ShapeData"
            );
        }
        // Mirrored across the vertical axis it looks the same: stays a live shape.
        let result = bake(shape_transform_state(ls, -1.0, 1.0, 0.0));
        assert!(matches!(
            result.layers[0].layer_type,
            LayerType::Vector(VectorGeometry::Primitive(_))
        ));
        // A rectangle at a quarter turn is still a rectangle.
        let (_, rect) = shape_orig_state(ShapeKind::Rectangle, 9);
        let result = bake(shape_transform_state(rect, 1.0, 1.0, 90.0));
        assert!(matches!(
            result.layers[0].layer_type,
            LayerType::Vector(VectorGeometry::Primitive(_))
        ));
    }

    #[test]
    fn move_handle_on_a_thick_outline_corner_scales_instead_of_rotating() {
        let mut app = crate::app::state::App::new();
        app.docs.documents[0].canvas = Canvas::new(300, 300);
        let (sd, off) = ShapeData::from_canvas_span(
            ShapeKind::Rectangle,
            60.0,
            60.0,
            160.0,
            120.0,
            0.0,
            true,
            [200, 30, 30, 255],
            14.0,
            [0, 0, 0, 255],
        );
        let raster = sd.render().expect("shape renders");
        {
            let canvas = &mut app.docs.documents[0].canvas;
            let idx = canvas.layer_stack.add_layer(raster.width, raster.height);
            for layer in &mut canvas.layer_stack.layers {
                layer.selected = false;
            }
            let layer = &mut canvas.layer_stack.layers[idx];
            layer.tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
            layer.offset = off;
            layer.layer_type = LayerType::Vector(VectorGeometry::Primitive(sd));
            layer.selected = true;
            canvas.layer_stack.active_idx = idx;
            // A second shape picked with it, down and to the right: the Move
            // box frames both (one alone has the path box), its top-left
            // corner the thick shape's.
            let second = canvas.layer_stack.layers[idx].duplicate(9001);
            let mut second = second;
            second.offset = (off.0 + 300, off.1 + 300);
            second.selected = true;
            canvas.layer_stack.layers.push(second);
        }
        app.edit.tools.select(crate::tools::ToolId::Move);
        app.edit.view.zoom = 4.0;
        app.edit.view.offset_x = 0.0;
        app.edit.view.offset_y = 0.0;

        let (corners, _, _) = app
            .move_selection_transform_box()
            .expect("Move shows a box");
        let (cx, cy) = corners[0];
        let (sx, sy) = (cx * 4.0, cy * 4.0);
        assert!(app.move_selection_transform_hit(sx, sy));
        app.begin_move_transform();
        app.transform_on_press(cx, cy, sx, sy);
        let ts = app.edit.transform_state.as_ref().expect("session started");
        assert_eq!(
            ts.drag_handle,
            Some(Some(TransformHandle::TopLeft)),
            "the drawn corner must grab the corner handle, not the rotate ring"
        );
    }

    #[test]
    fn an_empty_layer_in_the_selection_does_not_stretch_the_transform_box() {
        let (mut app, _) = app_with_selected_boxes(&[(50, 60, 40, 20), (150, 100, 30, 30)]);
        {
            // A fresh layer: canvas-sized, not one visible pixel.
            let canvas = &mut app.docs.documents[0].canvas;
            let idx = canvas.layer_stack.add_layer(600, 600);
            canvas.layer_stack.layers[idx].selected = true;
        }
        app.begin_transform();
        let ts = app.edit.transform_state.as_ref().expect("session started");
        assert_eq!(
            (ts.orig_offset, ts.orig_w, ts.orig_h),
            ((50, 60), 130, 70),
            "the box wraps the two photos, not the empty layer's whole canvas"
        );
        assert_eq!(ts.layer_states.len(), 2);
    }
}

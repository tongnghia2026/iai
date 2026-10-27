//! Direct edit operations: crop commits, layer align, Move-tool transform
//! actions, guides, invert and group/ungroup. Split out of app/actions.rs.

use crate::app::render::CanvasEvent;
use crate::app::state::App;
use crate::core::document::GuideOrientation;
use crate::core::snapping::{SNAP_THRESHOLD_PX, SnapKind, snap_1d};
use crate::core::vector::object::VectorGeometry;
use crate::ui::MoveTransformAction;

/// The canvas map of a Move-tool rotate/flip of the union box `[ux0,uy0]`
/// sized `union_w × union_h` — exactly the map [`transformed_layer_placement`]
/// applies to every layer rectangle (entries 0/±1, integer translation). Every
/// selected layer turns about this one centre: a Path's model, a Text or Shape
/// model and the raster layers alike, so a mixed selection keeps its layout.
/// Screen space has y down, so a positive rotation reads clockwise.
fn move_action_turn(
    action: MoveTransformAction,
    ux0: i32,
    uy0: i32,
    union_w: i32,
    union_h: i32,
) -> crate::core::vector::affine::AffineTransform {
    use crate::core::vector::affine::AffineTransform;
    let sx = (union_w - union_h) / 2;
    let sy = (union_h - union_w) / 2;
    let (a, b, c, d, e, f) = match action {
        MoveTransformAction::Rotate90Cw => (0, 1, -1, 0, ux0 + uy0 + union_h + sx, uy0 - ux0 + sy),
        MoveTransformAction::Rotate90Ccw => (0, -1, 1, 0, ux0 - uy0 + sx, ux0 + uy0 + union_w + sy),
        MoveTransformAction::Rotate180 => (-1, 0, 0, -1, 2 * ux0 + union_w, 2 * uy0 + union_h),
        MoveTransformAction::FlipHorizontal => (-1, 0, 0, 1, 2 * ux0 + union_w, 0),
        MoveTransformAction::FlipVertical => (1, 0, 0, -1, 0, 2 * uy0 + union_h),
    };
    AffineTransform {
        a: a as f32,
        b: b as f32,
        c: c as f32,
        d: d as f32,
        e: e as f32,
        f: f as f32,
    }
}

/// New `(offset_x, offset_y, width, height)` for a layer flipped/rotated within the
/// group union box `[ux0,uy0]` sized `union_w × union_h`, keeping the union's
/// CENTRE fixed. Rotating a non-square box only swaps its dimensions, so anchoring
/// the result at the union's top-left (as the old code did) drifted the centre and
/// made the content jump; the `(union_w - union_h)/2` shift re-centres it.
#[allow(clippy::too_many_arguments)]
fn transformed_layer_placement(
    action: MoveTransformAction,
    ux0: i32,
    uy0: i32,
    union_w: i32,
    union_h: i32,
    ox: i32,
    oy: i32,
    w: i32,
    h: i32,
) -> (i32, i32, i32, i32) {
    let rx = ox - ux0;
    let ry = oy - uy0;
    // Centre offset when the 90° rotation swaps the box's width/height.
    let sx = (union_w - union_h) / 2;
    let sy = (union_h - union_w) / 2;
    match action {
        MoveTransformAction::Rotate90Ccw => (ux0 + ry + sx, uy0 + union_w - rx - w + sy, h, w),
        MoveTransformAction::Rotate90Cw => (ux0 + union_h - ry - h + sx, uy0 + rx + sy, h, w),
        MoveTransformAction::Rotate180 => (ux0 + union_w - rx - w, uy0 + union_h - ry - h, w, h),
        MoveTransformAction::FlipHorizontal => (ux0 + union_w - rx - w, oy, w, h),
        MoveTransformAction::FlipVertical => (ox, uy0 + union_h - ry - h, w, h),
    }
}

/// Canvas-space box of a layer's visible pixels. An empty layer has none: its
/// raster rectangle (a fresh layer is canvas-sized) must not widen the union
/// the selection turns about.
fn layer_transform_bounds(layer: &crate::core::layer::Layer) -> Option<(i32, i32, i32, i32)> {
    layer.tiles.content_bounds().map(|(x0, y0, x1, y1)| {
        (
            layer.offset.0 + x0,
            layer.offset.1 + y0,
            layer.offset.0 + x1,
            layer.offset.1 + y1,
        )
    })
}

fn is_full_canvas_background(
    layer: &crate::core::layer::Layer,
    canvas_w: u32,
    canvas_h: u32,
) -> bool {
    layer.is_background
        && layer.offset == (0, 0)
        && layer.width == canvas_w
        && layer.height == canvas_h
}

fn crop_preview_background_color(
    crop_active: bool,
    has_background_layer: bool,
    background_color: [u8; 4],
) -> Option<[u8; 4]> {
    (crop_active && has_background_layer).then_some(background_color)
}

impl App {
    /// Commit the active Crop tool. Crop is tile-native (chunked blit/resample), so
    /// it runs under Viewport Streaming; the canvas crop methods only reject output
    /// past MAX_DIMENSION. Returns true if the crop was committed.
    pub fn commit_active_crop(&mut self) -> bool {
        // Crop is tile-native now (chunked blit for straight crop, chunked resample
        // for rotated/scaled), so it runs under Viewport Streaming with no >25M px
        // gate. The canvas crop methods still reject dimensions past MAX_DIMENSION.
        if let Some(gpu) = &mut self.win.gpu {
            gpu.compositor.crop_preview = None;
            gpu.compositor.crop_preview_background = None;
        }
        let began = std::time::Instant::now();
        // The frame as an `image.crop` call: the crop an agent would make,
        // through the same door. A PNG with alpha has no Background layer, so
        // what the crop adds there stays clear instead of taking the
        // toolbar's background colour.
        let call = {
            let canvas = &self.docs.active().canvas;
            let background = canvas
                .layer_stack
                .layers
                .iter()
                .any(|layer| layer.is_background)
                .then_some(self.edit.bg_color);
            self.edit
                .tools
                .crop()
                .engine_call(canvas.width, canvas.height, background)
        };
        match call {
            Some(args) => {
                if self.run_with("image.crop", args).is_err() {
                    // Refused (the status line says why): the frame stays.
                    return false;
                }
                self.edit.tools.crop_mut().cancel();
            }
            None => {
                self.edit.tools.crop_mut().cancel();
                return false;
            }
        }
        // The command's events did the canvas-size bookkeeping (GPU texture
        // resize, compositor invalidation, re-fit, recomposite) through
        // apply_canvas_event, the SAME path undo/redo take. The crop
        // resamples on this thread; a commit by Enter runs inside the key
        // handler, where no frame timing sees it. Keep what it cost.
        let total = began.elapsed();
        if total.as_millis() >= 30 {
            let canvas = &self.docs.active().canvas;
            crate::diag::note(
                "perf",
                &format!(
                    "crop committed in {} ms (pixels and screen) to {}x{}",
                    total.as_millis(),
                    canvas.width,
                    canvas.height
                ),
            );
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        true
    }

    pub fn update_crop_preview(&mut self) {
        let crop_active = self.edit.tools.active_id() == crate::tools::ToolId::Crop
            && self.edit.tools.crop().has_selection();
        let preview = if crop_active {
            let c = self.edit.tools.crop();
            if c.image_is_moved() {
                let canvas = &self.docs.active().canvas;
                let (pivot_x, pivot_y) = c.box_center();
                let cos = c.rotation.cos();
                let sin = c.rotation.sin();
                Some(crate::gpu::compositor::CropPreviewUniform {
                    inv_a: cos,
                    inv_b: sin,
                    inv_c: -sin,
                    inv_d: cos,
                    pivot_x,
                    pivot_y,
                    tx: c.image_tx,
                    ty: c.image_ty,
                    frame: [
                        c.crop_x0.min(c.crop_x1),
                        c.crop_y0.min(c.crop_y1),
                        c.crop_x0.max(c.crop_x1),
                        c.crop_y0.max(c.crop_y1),
                    ],
                    canvas_w: canvas.width as f32,
                    canvas_h: canvas.height as f32,
                    shade: 1.0 - crate::tools::crop::SHADE_ALPHA as f32 / 255.0,
                })
            } else {
                None
            }
        } else {
            None
        };
        // Match CropTool::on_confirm exactly: documents carrying a Background
        // layer fill newly exposed crop pixels with the current toolbar
        // background colour; alpha documents retain transparency/checkerboard.
        let has_background_layer = self
            .docs
            .active()
            .canvas
            .layer_stack
            .layers
            .iter()
            .any(|layer| layer.is_background);
        let preview_background =
            crop_preview_background_color(crop_active, has_background_layer, self.edit.bg_color);

        if let Some(gpu) = &mut self.win.gpu {
            gpu.compositor.crop_preview = preview;
            gpu.compositor.crop_preview_background = preview_background;
        }
        self.recomposite();
    }

    /// Commit the Perspective Crop: rectify the quad to a rectangle. Mirrors
    /// `commit_active_crop`'s post-resize bookkeeping (GPU texture, uniforms,
    /// fit-to-screen) since the canvas dimensions change.
    pub fn commit_active_perspective_crop(&mut self) -> bool {
        // Perspective Crop is tile-native now (chunked homography resample), so it
        // runs under Viewport Streaming with no >25M px gate. crop_perspective still
        // rejects an output past MAX_DIMENSION.
        if self
            .edit
            .tools
            .perspective_crop()
            .prospective_output_size()
            .is_none()
        {
            return false;
        }
        {
            let mut ctx = crate::extension::tool::ToolCtx::new(
                self.docs.active_mut(),
                self.edit.fg_color,
                self.edit.bg_color,
                self.edit.view.zoom,
                self.edit.view.offset_x,
                self.edit.view.offset_y,
            );
            self.edit.tools.active_on_confirm(&mut ctx);
        }
        // Stamp the chosen output DPI onto the rectified image.
        let persp_dpi = self.edit.tools.perspective_crop().dpi;
        self.docs.active_mut().canvas.metadata.resolution_ppi = persp_dpi;
        // Same centralised size-change path as commit_active_crop / undo.
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.apply_canvas_event(CanvasEvent::SelectionChanged);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        true
    }

    /// Duplicate the selected movable objects and carry each copy by the
    /// CANVAS-space affine `m` (`layer.step_copy`). The copies become the
    /// selection, so repeated calls build a step-and-repeat chain (row / ring /
    /// spiral); `m` is kept as the step the Repeat button replays.
    pub(in crate::app) fn duplicate_selected_with_transform(
        &mut self,
        m: crate::core::vector::affine::AffineTransform,
    ) -> bool {
        if !m.is_finite() {
            return false;
        }
        let matrix = [m.a, m.b, m.c, m.d, m.e, m.f];
        let ok = self
            .run_with("layer.step_copy", serde_json::json!({ "matrix": matrix }))
            .is_ok();
        if ok {
            self.edit.last_repeat_transform = Some(m);
        }
        ok
    }

    /// Duplicate the selection translated by `delta` — the Duplicate button and
    /// the "+" key. Records the translation as the repeatable step.
    pub(in crate::app) fn duplicate_selected_with_step(&mut self, delta: (i32, i32)) -> bool {
        let m =
            crate::core::vector::affine::AffineTransform::translate(delta.0 as f32, delta.1 as f32);
        let ok = self.duplicate_selected_with_transform(m);
        if ok {
            self.edit.tools.move_tool_mut().last_duplicate_delta = Some(delta);
        }
        ok
    }

    /// Replay the last captured duplicate-transform step (Repeat button / Ctrl+D):
    /// duplicate the current selection and re-apply the same move/rotate/scale, so
    /// each press extends the pattern from the previous copy.
    pub(in crate::app) fn repeat_last_step(&mut self) -> bool {
        match self.edit.last_repeat_transform {
            Some(m) => self.duplicate_selected_with_transform(m),
            None => {
                self.shell.status_msg =
                    "No step to repeat — duplicate, or Alt/Ctrl-drag a copy first".to_string();
                false
            }
        }
    }

    pub(super) fn apply_move_transform_action(&mut self, action: MoveTransformAction) {
        use crate::core::canvas::model_turn as turn_model;
        use crate::core::layer::{Layer, LayerMask, LayerType};

        fn can_transform(layer: &Layer) -> bool {
            (!layer.locked || layer.is_background)
                && matches!(
                    layer.layer_type,
                    LayerType::Raster
                        | LayerType::Text(_)
                        | LayerType::Vector(_)
                        | LayerType::SmartObject
                )
        }

        fn transform_mask(mask: &mut LayerMask, action: MoveTransformAction) {
            match action {
                MoveTransformAction::Rotate90Ccw => {
                    mask.tiles = mask.tiles.rotate_90_ccw();
                    std::mem::swap(&mut mask.width, &mut mask.height);
                }
                MoveTransformAction::Rotate90Cw => {
                    mask.tiles = mask.tiles.rotate_90_cw();
                    std::mem::swap(&mut mask.width, &mut mask.height);
                }
                MoveTransformAction::Rotate180 => {
                    mask.tiles = mask.tiles.flip_h().flip_v();
                }
                MoveTransformAction::FlipHorizontal => {
                    mask.tiles = mask.tiles.flip_h();
                }
                MoveTransformAction::FlipVertical => {
                    mask.tiles = mask.tiles.flip_v();
                }
            }
        }

        let label = match action {
            MoveTransformAction::Rotate90Ccw => "Rotate Layers 90 CCW",
            MoveTransformAction::Rotate90Cw => "Rotate Layers 90 CW",
            MoveTransformAction::Rotate180 => "Rotate Layers 180",
            MoveTransformAction::FlipHorizontal => "Flip Layers Horizontal",
            MoveTransformAction::FlipVertical => "Flip Layers Vertical",
        };

        let mut resized_canvas = false;
        let applied = {
            let canvas = &mut self.docs.active_mut().canvas;
            let mut targets = Vec::new();
            let mut seen = std::collections::HashSet::new();

            let selected: Vec<usize> = canvas
                .layer_stack
                .layers
                .iter()
                .enumerate()
                .filter(|(_, layer)| layer.selected)
                .map(|(idx, _)| idx)
                .collect();
            let candidates = if selected.is_empty() {
                vec![canvas.layer_stack.active_idx]
            } else {
                selected
            };

            for idx in candidates {
                let Some(layer) = canvas.layer_stack.layers.get(idx) else {
                    continue;
                };
                if layer.is_group() {
                    for member_idx in canvas.layer_stack.group_member_range(idx) {
                        let Some(member) = canvas.layer_stack.layers.get(member_idx) else {
                            continue;
                        };
                        if can_transform(member) && seen.insert(member_idx) {
                            targets.push(member_idx);
                        }
                    }
                } else if can_transform(layer) && seen.insert(idx) {
                    targets.push(idx);
                }
            }

            if targets.is_empty() {
                false
            } else {
                let has_full_canvas_background = targets.iter().any(|&idx| {
                    canvas.layer_stack.layers.get(idx).is_some_and(|layer| {
                        is_full_canvas_background(layer, canvas.width, canvas.height)
                    })
                });

                if has_full_canvas_background {
                    match action {
                        MoveTransformAction::Rotate90Ccw => {
                            canvas.rotate_90_ccw();
                            resized_canvas = true;
                        }
                        MoveTransformAction::Rotate90Cw => {
                            canvas.rotate_90_cw();
                            resized_canvas = true;
                        }
                        MoveTransformAction::Rotate180 => {
                            canvas.begin_undo_group("Rotate 180");
                            canvas.flip_horizontal();
                            canvas.flip_vertical();
                            canvas.end_undo_group();
                        }
                        MoveTransformAction::FlipHorizontal => canvas.flip_horizontal(),
                        MoveTransformAction::FlipVertical => canvas.flip_vertical(),
                    }
                    true
                } else if let Some((ux0, uy0, ux1, uy1)) = targets
                    .iter()
                    .filter_map(|&idx| layer_transform_bounds(&canvas.layer_stack.layers[idx]))
                    .fold(None::<(i32, i32, i32, i32)>, |acc, (lx0, ly0, lx1, ly1)| {
                        Some(match acc {
                            Some((x0, y0, x1, y1)) => {
                                (x0.min(lx0), y0.min(ly0), x1.max(lx1), y1.max(ly1))
                            }
                            None => (lx0, ly0, lx1, ly1),
                        })
                    })
                {
                    let union_w = ux1 - ux0;
                    let union_h = uy1 - uy0;
                    if union_w <= 0 || union_h <= 0 {
                        false
                    } else {
                        let mut cmd = crate::core::command::LayerStructureCommand::capture_before(
                            label,
                            &canvas.layer_stack,
                            canvas.width,
                            canvas.height,
                        );

                        let turn = move_action_turn(action, ux0, uy0, union_w, union_h);
                        for idx in targets {
                            let layer = &mut canvas.layer_stack.layers[idx];
                            // Text, Shape and Path layers re-render from their
                            // model, so the model turns with the pixels or the
                            // next edit snaps the layer back.
                            let source =
                                turn_model::has_turnable_model(layer).then(|| layer.clone());
                            // Vector Path: rotate/flip the MODEL and re-derive the
                            // raster, so the object stays editable and survives
                            // rebuild_path_caches on reload. Never bake the tiles
                            // (Mục 3.2).
                            if matches!(
                                layer.layer_type,
                                LayerType::Vector(VectorGeometry::Path(_))
                            ) {
                                if let Some(source) = &source {
                                    turn_model::turn_layer_model(layer, source, &turn);
                                }
                                if let Some(mask) = &mut layer.mask {
                                    transform_mask(mask, action);
                                }
                                continue;
                            }
                            let (nox, noy, nw, nh) = transformed_layer_placement(
                                action,
                                ux0,
                                uy0,
                                union_w,
                                union_h,
                                layer.offset.0,
                                layer.offset.1,
                                layer.width as i32,
                                layer.height as i32,
                            );
                            match action {
                                MoveTransformAction::Rotate90Ccw => {
                                    layer.tiles = layer.tiles.rotate_90_ccw();
                                }
                                MoveTransformAction::Rotate90Cw => {
                                    layer.tiles = layer.tiles.rotate_90_cw();
                                }
                                MoveTransformAction::Rotate180 => {
                                    layer.tiles = layer.tiles.flip_h().flip_v();
                                }
                                MoveTransformAction::FlipHorizontal => {
                                    layer.tiles = layer.tiles.flip_h();
                                }
                                MoveTransformAction::FlipVertical => {
                                    layer.tiles = layer.tiles.flip_v();
                                }
                            }
                            layer.width = nw as u32;
                            layer.height = nh as u32;
                            layer.offset = (nox, noy);

                            if let Some(mask) = &mut layer.mask {
                                transform_mask(mask, action);
                            }
                            if let Some(source) = &source {
                                turn_model::turn_layer_model(layer, source, &turn);
                            }
                        }

                        cmd.capture_after(&canvas.layer_stack, canvas.width, canvas.height);
                        canvas.record(Box::new(cmd));
                        canvas.flatten_full();
                        true
                    }
                } else {
                    false
                }
            }
        };

        if !applied {
            self.shell.status_msg = "No transformable layer selected".to_string();
            return;
        }

        if resized_canvas {
            let (w, h) = {
                let canvas = &self.docs.active().canvas;
                (canvas.width, canvas.height)
            };
            if let Some(gpu) = &mut self.win.gpu {
                gpu.resize_canvas_texture(w, h);
            }
            self.push_canvas_uniforms();
            self.fit_canvas_to_screen();
        }

        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.apply_canvas_event(CanvasEvent::SelectionChanged);
        self.shell.status_msg = match action {
            MoveTransformAction::Rotate90Ccw => "Rotated selection 90 deg counter-clockwise",
            MoveTransformAction::Rotate90Cw => "Rotated selection 90 deg clockwise",
            MoveTransformAction::Rotate180 => "Rotated selection 180 deg",
            MoveTransformAction::FlipHorizontal => "Flipped selection horizontally",
            MoveTransformAction::FlipVertical => "Flipped selection vertically",
        }
        .to_string();
    }

    /// Snap a guide coordinate (along its own axis) to the canvas edges/center and
    /// other parallel guides. Falls back to integer-pixel rounding when snapping is
    /// off or nothing is in range. `exclude` skips a guide by index (used when
    /// moving an existing one so it doesn't snap to itself).
    pub fn snap_guide_pos(
        &self,
        orientation: GuideOrientation,
        pos: f32,
        exclude: Option<usize>,
    ) -> f32 {
        let canvas = &self.docs.active().canvas;
        let dim = match orientation {
            GuideOrientation::Horizontal => canvas.height as f32,
            GuideOrientation::Vertical => canvas.width as f32,
        };
        if !self.shell.ui.snap_enabled {
            return pos.round();
        }
        let threshold = (SNAP_THRESHOLD_PX / self.edit.view.zoom).max(0.5);
        let mut targets: Vec<(f32, SnapKind)> = vec![
            (0.0, SnapKind::CanvasEdge),
            (dim, SnapKind::CanvasEdge),
            (dim * 0.5, SnapKind::CanvasCenter),
        ];
        for (i, g) in self.docs.active().guides.iter().enumerate() {
            if Some(i) == exclude {
                continue;
            }
            if g.orientation == orientation {
                targets.push((g.pos, SnapKind::Guide));
            }
        }
        match snap_1d(pos, &targets, threshold) {
            Some(s) => s.value,
            None => pos.round(),
        }
    }

    /// Ctrl+I — invert. Inverts the mask if editing a mask, otherwise the RGB
    /// pixels of the (unlocked) raster layer. Undoable.
    pub fn do_invert_active(&mut self, idx: usize) {
        // The command Ctrl+I is: the mask when it is painted on, else the
        // pixels.
        let id = self
            .docs
            .active()
            .canvas
            .layer_stack
            .layers
            .get(idx)
            .map(|l| l.id);
        if let Some(id) = id {
            let _ = self.run_with("layer.invert", serde_json::json!({"layer": id}));
        }
    }

    /// Ctrl+G — wrap the selected (or active) layers in a new group folder.
    /// Undoable. No-op (with a status hint) when only the background qualifies.
    pub fn do_group_selected(&mut self) {
        let _ = self.run_now("layer.group");
    }

    /// Ctrl+Shift+G — dissolve the group at `idx` (children rise to the group's
    /// parent level). Undoable. No-op when `idx` is not a group.
    pub fn do_ungroup(&mut self, idx: usize) {
        let id = self
            .docs
            .active()
            .canvas
            .layer_stack
            .layers
            .get(idx)
            .map(|l| l.id);
        match id {
            Some(id) => {
                let _ = self.run_with("layer.ungroup", serde_json::json!({"layer": id}));
            }
            None => self.shell.status_msg = "Layer hiện tại không phải nhóm".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{crop_preview_background_color, transformed_layer_placement};
    use crate::app::state::App;
    use crate::core::canvas::Canvas;
    use crate::core::tile::TileMap;
    use crate::core::vector::object::VectorGeometry;
    use crate::ui::{LayerAlign, LayerDistribute, MoveTransformAction};

    /// The Crop tool's frame committed by the window (through `image.crop`)
    /// and by the tool straight on the canvas: the same page, pixel for
    /// pixel — straight, turned with the picture moved, to a print size, and
    /// past the edge onto a Background.
    #[test]
    fn the_crop_tool_through_the_command_crops_as_the_tool_itself_did() {
        use crate::core::units::Unit;
        use crate::tools::crop::{CropMode, CropTool};
        let (w, h) = (300u32, 200u32);
        let rgba: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                [
                    (x * 255 / w) as u8,
                    (y * 255 / h) as u8,
                    ((x ^ y) & 0xFF) as u8,
                    255,
                ]
            })
            .collect();
        type Setup = fn(&mut CropTool);
        let cases: [(&str, Setup); 4] = [
            ("straight", |t| {
                t.mode = CropMode::Free;
                (t.crop_x0, t.crop_y0, t.crop_x1, t.crop_y1) = (20.4, 10.6, 180.2, 150.0);
            }),
            ("turned and moved", |t| {
                t.mode = CropMode::Free;
                (t.crop_x0, t.crop_y0, t.crop_x1, t.crop_y1) = (40.0, 30.0, 220.5, 170.25);
                (t.rotation, t.image_tx, t.image_ty) = (0.2137, 7.5, -3.25);
            }),
            ("to a print size", |t| {
                t.mode = CropMode::FixedSize;
                (t.fixed_w, t.fixed_h, t.unit, t.dpi) = (2.0, 3.0, Unit::Centimeters, 300.0);
                (t.crop_x0, t.crop_y0, t.crop_x1, t.crop_y1) = (50.0, 20.0, 150.0, 170.0);
            }),
            ("past the edge", |t| {
                t.mode = CropMode::Free;
                (t.crop_x0, t.crop_y0, t.crop_x1, t.crop_y1) = (-30.0, -20.0, 260.0, 230.0);
            }),
        ];
        for (name, setup) in cases {
            let mut app = App::new();
            app.shell.ui.show_welcome = false;
            app.docs.documents[0].canvas = Canvas::from_rgba(rgba.clone(), w, h);
            app.edit.bg_color = [12, 200, 90, 255];
            app.edit.tools.select(crate::tools::ToolId::Crop);
            setup(app.edit.tools.crop_mut());

            let mut by_hand = Canvas::from_rgba(rgba.clone(), w, h);
            let mut tool = CropTool::default();
            setup(&mut tool);
            tool.commit(&mut by_hand, Some(app.edit.bg_color));

            assert!(app.commit_active_crop(), "{name}");
            let by_window = &app.docs.documents[0].canvas;
            assert_eq!(
                (by_window.width, by_window.height),
                (by_hand.width, by_hand.height),
                "{name}"
            );
            assert_eq!(
                by_window.metadata.resolution_ppi, by_hand.metadata.resolution_ppi,
                "{name}"
            );
            let pixels = |c: &Canvas| -> Vec<Vec<u8>> {
                c.layer_stack
                    .layers
                    .iter()
                    .map(|l| l.flatten_tiles())
                    .collect()
            };
            assert!(
                pixels(by_window) == pixels(&by_hand),
                "{name}: pixels differ"
            );
            assert!(!app.edit.tools.crop().has_selection(), "{name}: frame left");
        }
    }

    #[test]
    fn crop_preview_uses_commit_background_only_for_background_documents() {
        let bg = [240, 230, 220, 255];
        assert_eq!(crop_preview_background_color(true, true, bg), Some(bg));
        assert_eq!(crop_preview_background_color(true, false, bg), None);
        assert_eq!(crop_preview_background_color(false, true, bg), None);
    }

    /// Apply the placement to every layer and return the transformed union box.
    fn transformed_union(
        action: MoveTransformAction,
        ux0: i32,
        uy0: i32,
        uw: i32,
        uh: i32,
        layers: &[(i32, i32, i32, i32)],
    ) -> (i32, i32, i32, i32) {
        let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for &(ox, oy, w, h) in layers {
            let (a, b, nw, nh) =
                transformed_layer_placement(action, ux0, uy0, uw, uh, ox, oy, w, h);
            x0 = x0.min(a);
            y0 = y0.min(b);
            x1 = x1.max(a + nw);
            y1 = y1.max(b + nh);
        }
        (x0, y0, x1, y1)
    }

    #[test]
    fn transform_keeps_union_centre() {
        // Non-square union (0,0)-(400,200), two layers tiling it. Every flip/rotate
        // must leave the union CENTRE where it was (content stays put, no jump).
        let (ux0, uy0, uw, uh) = (0, 0, 400, 200);
        let layers = [(0, 0, 400, 100), (0, 100, 400, 100)];
        // centre*2 (avoids /2 rounding) of the original union.
        let (cx2, cy2) = (2 * ux0 + uw, 2 * uy0 + uh);
        for action in [
            MoveTransformAction::Rotate90Cw,
            MoveTransformAction::Rotate90Ccw,
            MoveTransformAction::Rotate180,
            MoveTransformAction::FlipHorizontal,
            MoveTransformAction::FlipVertical,
        ] {
            let (x0, y0, x1, y1) = transformed_union(action, ux0, uy0, uw, uh, &layers);
            assert_eq!(x0 + x1, cx2, "{action:?} moved the x-centre");
            assert_eq!(y0 + y1, cy2, "{action:?} moved the y-centre");
        }
    }

    fn app_with_padded_selected_layer() -> (App, i32, i32) {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(200, 160);

        let canvas = &mut app.docs.documents[0].canvas;
        let idx = canvas.layer_stack.add_layer(100, 100);
        canvas.layer_stack.layers[0].selected = false;
        canvas.layer_stack.active_idx = idx;

        let mut px = vec![0u8; 100 * 100 * 4];
        for y in 40..50 {
            for x in 30..50 {
                let i = ((y * 100 + x) * 4) as usize;
                px[i] = 220;
                px[i + 1] = 40;
                px[i + 2] = 30;
                px[i + 3] = 255;
            }
        }

        let layer = &mut canvas.layer_stack.layers[idx];
        layer.tiles = TileMap::from_rgba(&px, 100, 100);
        layer.offset = (20, 10);
        layer.selected = true;

        // Canvas-space centre*2 of the real, non-transparent content.
        (app, 120, 110)
    }

    #[test]
    fn move_transform_buttons_keep_padded_content_centre() {
        for action in [
            MoveTransformAction::Rotate90Cw,
            MoveTransformAction::Rotate90Ccw,
            MoveTransformAction::Rotate180,
            MoveTransformAction::FlipHorizontal,
            MoveTransformAction::FlipVertical,
        ] {
            let (mut app, cx2, cy2) = app_with_padded_selected_layer();
            app.apply_move_transform_action(action);

            let canvas = &app.docs.documents[0].canvas;
            let layer = &canvas.layer_stack.layers[canvas.layer_stack.active_idx];
            let (x0, y0, x1, y1) = layer
                .tiles
                .content_bounds()
                .expect("transformed layer should still have content");
            assert_eq!(
                layer.offset.0 * 2 + x0 + x1,
                cx2,
                "{action:?} moved the padded content x-centre"
            );
            assert_eq!(
                layer.offset.1 * 2 + y0 + y1,
                cy2,
                "{action:?} moved the padded content y-centre"
            );
        }
    }

    #[test]
    fn move_transform_background_rotates_the_canvas_with_other_layers_present() {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(600, 400);
        {
            let canvas = &mut app.docs.documents[0].canvas;
            let _extra = canvas.layer_stack.add_layer(80, 40);
            canvas.layer_stack.layers[0].selected = true;
            canvas.layer_stack.active_idx = 0;
        }

        app.apply_move_transform_action(MoveTransformAction::Rotate90Cw);

        let canvas = &app.docs.documents[0].canvas;
        let bg = &canvas.layer_stack.layers[0];
        assert_eq!((canvas.width, canvas.height), (400, 600));
        assert_eq!(bg.offset, (0, 0));
        assert_eq!((bg.width, bg.height), (400, 600));
    }

    fn rect_path_object(
        w: f32,
        h: f32,
        tx: f32,
        ty: f32,
    ) -> crate::core::vector::object::VectorObjectData {
        use crate::core::geometry::Point;
        use crate::core::vector::affine::AffineTransform;
        use crate::core::vector::object::VectorObjectData;
        use crate::core::vector::path::{Contour, FillRule, Node, PathData};
        use crate::core::vector::style::VectorStyle;
        let path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(w, 0.0)),
                    Node::sharp(Point::new(w, h)),
                    Node::sharp(Point::new(0.0, h)),
                ],
                true,
            )],
            FillRule::NonZero,
        );
        VectorObjectData::new(
            path,
            VectorStyle::default(),
            AffineTransform::translate(tx, ty),
        )
    }

    #[test]
    fn move_action_turn_is_the_map_every_layer_placement_uses() {
        use super::move_action_turn;
        use crate::core::geometry::Point;
        // Odd and even width/height differences, layers inside and around the
        // union (padding can reach past the content union).
        for (ux0, uy0, uw, uh) in [(10, 20, 400, 200), (-7, 3, 51, 120), (0, 0, 33, 33)] {
            for (ox, oy, w, h) in [
                (ux0, uy0, uw, uh),
                (ux0 + 5, uy0 + 9, 13, 7),
                (ux0 - 4, uy0 - 2, 30, 61),
            ] {
                for action in [
                    MoveTransformAction::Rotate90Cw,
                    MoveTransformAction::Rotate90Ccw,
                    MoveTransformAction::Rotate180,
                    MoveTransformAction::FlipHorizontal,
                    MoveTransformAction::FlipVertical,
                ] {
                    let turn = move_action_turn(action, ux0, uy0, uw, uh);
                    let a = turn.apply_point(Point::new(ox as f32, oy as f32));
                    let b = turn.apply_point(Point::new((ox + w) as f32, (oy + h) as f32));
                    let mapped = (
                        a.x.min(b.x) as i32,
                        a.y.min(b.y) as i32,
                        (a.x - b.x).abs() as i32,
                        (a.y - b.y).abs() as i32,
                    );
                    assert_eq!(
                        mapped,
                        transformed_layer_placement(action, ux0, uy0, uw, uh, ox, oy, w, h),
                        "{action:?} union {:?} layer {:?}",
                        (ux0, uy0, uw, uh),
                        (ox, oy, w, h)
                    );
                }
            }
        }
    }

    #[test]
    fn move_transform_keeps_path_editable_not_baked() {
        use crate::core::command_vector::CreatePathLayer;
        use crate::core::gateway::ChangeKind;
        use crate::core::layer::LayerType;

        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(200, 200);
        let id = {
            let canvas = &mut app.docs.documents[0].canvas;
            canvas
                .execute(
                    Box::new(CreatePathLayer::new(
                        rect_path_object(40.0, 20.0, 50.0, 60.0),
                        "Path 1",
                    )),
                    ChangeKind::LayerStructure,
                )
                .unwrap();
            let idx = canvas
                .layer_stack
                .layers
                .iter()
                .position(|l| matches!(l.layer_type, LayerType::Vector(VectorGeometry::Path(_))))
                .unwrap();
            canvas.layer_stack.layers[idx].selected = true;
            canvas.layer_stack.active_idx = idx;
            canvas.layer_stack.layers[idx].id
        };
        let (w0, h0) = {
            let l = find_layer(&app, id);
            (l.width as i32, l.height as i32)
        };

        app.apply_move_transform_action(MoveTransformAction::Rotate90Cw);

        let l = find_layer(&app, id);
        assert!(
            matches!(l.layer_type, LayerType::Vector(VectorGeometry::Path(_))),
            "Path stays an editable vector after rotate (tiles not baked to Raster)"
        );
        assert!(
            (l.width as i32 - h0).abs() <= 2 && (l.height as i32 - w0).abs() <= 2,
            "90° swapped the raster dims: {}x{} was {w0}x{h0}",
            l.width,
            l.height
        );
    }

    fn find_layer(app: &App, id: u32) -> &crate::core::layer::Layer {
        app.docs.documents[0]
            .canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == id)
            .unwrap()
    }

    fn add_selected_box(app: &mut App, x: i32, y: i32) -> u32 {
        let canvas = &mut app.docs.documents[0].canvas;
        let idx = canvas.layer_stack.add_layer(10, 10);
        let layer = &mut canvas.layer_stack.layers[idx];
        layer.tiles = TileMap::from_rgba(&vec![255; 10 * 10 * 4], 10, 10);
        layer.offset = (x, y);
        layer.selected = true;
        layer.id
    }

    #[test]
    fn distribute_horizontal_centres_keeps_outer_objects_and_undoes_once() {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(240, 160);
        app.docs.documents[0].canvas.layer_stack.layers[0].selected = false;
        let left = add_selected_box(&mut app, 10, 20);
        let middle = add_selected_box(&mut app, 50, 20);
        let right = add_selected_box(&mut app, 170, 20);

        assert!(
            app.run_with(
                "layer.distribute",
                LayerDistribute::HorizontalCenters.args()
            )
            .is_ok()
        );
        assert_eq!(find_layer(&app, left).offset.0, 10);
        assert_eq!(find_layer(&app, middle).offset.0, 90);
        assert_eq!(find_layer(&app, right).offset.0, 170);
        assert_eq!(app.docs.documents[0].canvas.undo_count(), 1);

        app.docs.documents[0]
            .canvas
            .undo()
            .expect("undo distribute");
        assert_eq!(find_layer(&app, middle).offset.0, 50);
    }

    #[test]
    fn align_path_folds_raster_offset_into_affine_model() {
        use crate::core::command_vector::CreatePathLayer;
        use crate::core::gateway::ChangeKind;
        use crate::core::layer::LayerType;

        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(200, 160);
        let (id, original) = {
            let canvas = &mut app.docs.documents[0].canvas;
            canvas.layer_stack.layers[0].selected = false;
            let object = rect_path_object(30.0, 20.0, 80.0, 50.0);
            let original = object.transform;
            canvas
                .execute(
                    Box::new(CreatePathLayer::new(object, "Aligned Path")),
                    ChangeKind::LayerStructure,
                )
                .unwrap();
            let idx = canvas
                .layer_stack
                .layers
                .iter()
                .position(|l| matches!(l.layer_type, LayerType::Vector(VectorGeometry::Path(_))))
                .unwrap();
            canvas.layer_stack.layers[idx].selected = true;
            canvas.layer_stack.active_idx = idx;
            (canvas.layer_stack.layers[idx].id, original)
        };
        // Ignore creation history: the assertion below concerns alignment only.
        app.docs.documents[0].canvas.mark_saved();
        let before_undo = app.docs.documents[0].canvas.undo_count();

        assert!(app.run_with("layer.align", LayerAlign::Left.args()).is_ok());
        let layer = find_layer(&app, id);
        let aligned = match &layer.layer_type {
            LayerType::Vector(VectorGeometry::Path(object)) => object.transform,
            _ => panic!("Path must stay editable"),
        };
        assert_ne!(aligned, original, "alignment must update the model");
        assert_eq!(
            layer.offset.0 + layer.tiles.content_bounds().unwrap().0,
            0,
            "visible content aligned to canvas left"
        );
        assert_eq!(
            app.docs.documents[0].canvas.undo_count(),
            before_undo + 1,
            "one alignment = one undo step"
        );
        app.docs.documents[0].canvas.undo().expect("undo align");
        let restored = match &find_layer(&app, id).layer_type {
            LayerType::Vector(VectorGeometry::Path(object)) => object.transform,
            _ => panic!("Path must stay editable"),
        };
        assert_eq!(restored, original);
    }

    #[test]
    fn align_moves_a_whole_group_together() {
        // Regression: selecting a group (its header) and aligning must shift all
        // of the group's members by the same delta — previously a group had no
        // tiles of its own, so it was skipped and alignment did nothing.
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(200, 160);
        let (id_a, id_b) = {
            let canvas = &mut app.docs.documents[0].canvas;
            canvas.layer_stack.layers[0].selected = false;

            let a = canvas.layer_stack.add_layer(20, 20);
            canvas.layer_stack.layers[a]
                .tiles
                .set_pixel(0, 0, 255, 0, 0, 255);
            canvas.layer_stack.layers[a].offset = (60, 40);
            canvas.layer_stack.layers[a].selected = true;
            let id_a = canvas.layer_stack.layers[a].id;

            let b = canvas.layer_stack.add_layer(20, 20);
            canvas.layer_stack.layers[b]
                .tiles
                .set_pixel(0, 0, 0, 0, 255, 255);
            canvas.layer_stack.layers[b].offset = (100, 90);
            canvas.layer_stack.layers[b].selected = true;
            let id_b = canvas.layer_stack.layers[b].id;

            let header = canvas
                .layer_stack
                .create_group_from_selected(200, 160)
                .expect("group created");
            // Select only the group header, as the panel does on a folder click.
            for l in canvas.layer_stack.layers.iter_mut() {
                l.selected = false;
            }
            canvas.layer_stack.layers[header].selected = true;
            canvas.layer_stack.active_idx = header;
            (id_a, id_b)
        };

        assert!(app.run_with("layer.align", LayerAlign::Left.args()).is_ok());

        // The union's left edge (min content x = 60) snaps to canvas x=0, so both
        // members shift by -60 and keep their relative placement and their y.
        assert_eq!(find_layer(&app, id_a).offset, (0, 40));
        assert_eq!(find_layer(&app, id_b).offset, (40, 90));
    }

    #[test]
    fn duplicate_step_repeats_delta_and_keeps_path_editable() {
        use crate::core::command_vector::CreatePathLayer;
        use crate::core::gateway::ChangeKind;
        use crate::core::layer::LayerType;

        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(300, 200);
        {
            let canvas = &mut app.docs.documents[0].canvas;
            canvas.layer_stack.layers[0].selected = false;
            canvas
                .execute(
                    Box::new(CreatePathLayer::new(
                        rect_path_object(20.0, 20.0, 40.0, 50.0),
                        "Step Path",
                    )),
                    ChangeKind::LayerStructure,
                )
                .unwrap();
            let idx = canvas
                .layer_stack
                .layers
                .iter()
                .position(|layer| {
                    matches!(layer.layer_type, LayerType::Vector(VectorGeometry::Path(_)))
                })
                .unwrap();
            canvas.layer_stack.layers[idx].selected = true;
            canvas.layer_stack.active_idx = idx;
        }

        assert!(app.duplicate_selected_with_step((25, 10)));
        assert!(app.duplicate_selected_with_step((25, 10)));
        let mut origins: Vec<(i32, i32)> = app.docs.documents[0]
            .canvas
            .layer_stack
            .layers
            .iter()
            .filter(|layer| matches!(layer.layer_type, LayerType::Vector(VectorGeometry::Path(_))))
            .map(|layer| layer.offset)
            .collect();
        origins.sort_unstable();
        assert_eq!(origins.len(), 3);
        assert_eq!(origins[1].0 - origins[0].0, 25);
        assert_eq!(origins[2].0 - origins[1].0, 25);
        assert_eq!(origins[1].1 - origins[0].1, 10);
        assert_eq!(origins[2].1 - origins[1].1, 10);
        assert!(
            app.docs.documents[0]
                .canvas
                .layer_stack
                .layers
                .iter()
                .filter(|layer| {
                    matches!(layer.layer_type, LayerType::Vector(VectorGeometry::Path(_)))
                })
                .all(|layer| {
                    matches!(layer.layer_type, LayerType::Vector(VectorGeometry::Path(_)))
                })
        );

        app.docs.documents[0]
            .canvas
            .undo()
            .expect("undo second step");
        assert_eq!(
            app.docs.documents[0]
                .canvas
                .layer_stack
                .layers
                .iter()
                .filter(|layer| {
                    matches!(layer.layer_type, LayerType::Vector(VectorGeometry::Path(_)))
                })
                .count(),
            2
        );
    }

    // --- Multi-layer audit, 19/10: Rotate/Flip buttons on Type, Shapes and
    // several Paths. ---

    fn content_size(layer: &crate::core::layer::Layer) -> (i32, i32) {
        let (x0, y0, x1, y1) = layer.tiles.content_bounds().expect("content");
        (x1 - x0, y1 - y0)
    }

    fn canvas_content_box(layer: &crate::core::layer::Layer) -> (i32, i32, i32, i32) {
        let (x0, y0, x1, y1) = layer.tiles.content_bounds().expect("content");
        let (ox, oy) = layer.offset;
        (ox + x0, oy + y0, ox + x1, oy + y1)
    }

    fn select_only(app: &mut App, ids: &[u32]) {
        let canvas = &mut app.docs.documents[0].canvas;
        for layer in &mut canvas.layer_stack.layers {
            layer.selected = ids.contains(&layer.id);
        }
        if let Some(idx) = canvas
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == ids[0])
        {
            canvas.layer_stack.active_idx = idx;
        }
    }

    #[test]
    fn move_rotate_and_flip_turn_the_text_model_with_its_pixels() {
        use crate::core::layer::LayerType;
        use crate::core::text::{TextData, TextFontFamily, rasterize_placed};

        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(400, 300);
        let td = TextData {
            content: "Hello".to_string(),
            font_family: TextFontFamily::DejaVuSans,
            font_px: 32.0,
            ..TextData::default()
        };
        let (raster, _) = rasterize_placed(&td).expect("text renders");
        let id = {
            let canvas = &mut app.docs.documents[0].canvas;
            let idx = canvas.layer_stack.add_layer(raster.width, raster.height);
            let layer = &mut canvas.layer_stack.layers[idx];
            layer.tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
            layer.offset = (100, 80);
            layer.layer_type = LayerType::Text(td);
            layer.id
        };
        select_only(&mut app, &[id]);

        for (action, rotation, flip_x) in [
            (MoveTransformAction::Rotate90Cw, 90.0, false),
            (MoveTransformAction::FlipHorizontal, 270.0, true),
            (MoveTransformAction::Rotate180, 90.0, true),
            (MoveTransformAction::FlipVertical, 90.0, false),
        ] {
            app.apply_move_transform_action(action);
            let layer = find_layer(&app, id);
            let LayerType::Text(td) = &layer.layer_type else {
                panic!("{action:?}: type stays editable");
            };
            assert!(
                (td.rotation_deg - rotation).abs() < 0.01 && td.flip_x == flip_x,
                "{action:?}: rotation {} flip_x {}",
                td.rotation_deg,
                td.flip_x
            );
            // Re-rendering the model must draw what is on screen, or the next
            // edit or transform flips the text back.
            let (fresh, _) = rasterize_placed(td).expect("text renders");
            let fresh = TileMap::from_rgba(&fresh.rgba, fresh.width, fresh.height);
            let (fx0, fy0, fx1, fy1) = fresh.content_bounds().expect("ink");
            let (w, h) = content_size(layer);
            assert!(
                ((fx1 - fx0) - w).abs() <= 2 && ((fy1 - fy0) - h).abs() <= 2,
                "{action:?}: model draws {}x{}, layer shows {w}x{h}",
                fx1 - fx0,
                fy1 - fy0
            );
        }
    }

    #[test]
    fn move_rotate_keeps_shapes_true_to_their_pixels() {
        use crate::core::layer::LayerType;
        use crate::core::shape::{ShapeData, ShapeKind};

        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(400, 300);
        let mut add_shape = |kind, sides| {
            let (mut sd, off) = ShapeData::from_canvas_span(
                kind,
                60.0,
                50.0,
                180.0,
                110.0,
                0.0,
                true,
                [200, 30, 30, 255],
                // Fill only: a Path draws sharp corners with mitred outlines.
                0.0,
                [0, 0, 0, 255],
            );
            sd.sides = sides;
            let raster = sd.render().expect("shape renders");
            let canvas = &mut app.docs.documents[0].canvas;
            let idx = canvas.layer_stack.add_layer(raster.width, raster.height);
            let layer = &mut canvas.layer_stack.layers[idx];
            layer.tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
            layer.offset = off;
            layer.layer_type = LayerType::Vector(VectorGeometry::Primitive(sd));
            layer.id
        };
        let rect = add_shape(ShapeKind::Rectangle, 5);
        let triangle = add_shape(ShapeKind::Polygon, 3);

        for id in [rect, triangle] {
            select_only(&mut app, &[id]);
            let before = canvas_content_box(find_layer(&app, id));
            app.apply_move_transform_action(MoveTransformAction::Rotate90Cw);
            let layer = find_layer(&app, id);
            let after = canvas_content_box(layer);
            assert!(
                ((after.2 - after.0) - (before.3 - before.1)).abs() <= 2
                    && ((after.3 - after.1) - (before.2 - before.0)).abs() <= 2,
                "a quarter turn swaps the box: {before:?} -> {after:?}"
            );
            match &layer.layer_type {
                LayerType::Vector(VectorGeometry::Primitive(sd)) => {
                    assert_eq!(id, rect, "only the rectangle can stay a ShapeData");
                    // The model's next render is the turned picture.
                    let fresh = sd.render().expect("shape renders");
                    let fresh = TileMap::from_rgba(&fresh.rgba, fresh.width, fresh.height);
                    let (fx0, fy0, fx1, fy1) = fresh.content_bounds().expect("ink");
                    let (w, h) = content_size(layer);
                    assert!(((fx1 - fx0) - w).abs() <= 1 && ((fy1 - fy0) - h).abs() <= 1);
                }
                LayerType::Vector(VectorGeometry::Path(_)) => {
                    assert_eq!(id, triangle, "a triangle on its side becomes curves");
                }
                _ => panic!("the shape stays vector"),
            }
        }
    }

    #[test]
    fn move_rotate_turns_several_paths_about_their_common_centre() {
        use crate::core::command_vector::CreatePathLayer;
        use crate::core::gateway::ChangeKind;
        use crate::core::layer::LayerType;

        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(400, 300);
        for (tx, name) in [(50.0, "A"), (150.0, "B")] {
            app.docs.documents[0]
                .canvas
                .execute(
                    Box::new(CreatePathLayer::new(
                        rect_path_object(40.0, 20.0, tx, 60.0),
                        name,
                    )),
                    ChangeKind::LayerStructure,
                )
                .unwrap();
        }
        let ids: Vec<u32> = app.docs.documents[0]
            .canvas
            .layer_stack
            .layers
            .iter()
            .filter(|l| matches!(l.layer_type, LayerType::Vector(VectorGeometry::Path(_))))
            .map(|l| l.id)
            .collect();
        assert_eq!(ids.len(), 2);
        select_only(&mut app, &ids);
        let centre = |app: &App, id| {
            let (x0, y0, x1, y1) = canvas_content_box(find_layer(app, id));
            ((x0 + x1) as f32 * 0.5, (y0 + y1) as f32 * 0.5)
        };
        let before: Vec<_> = ids.iter().map(|&id| centre(&app, id)).collect();
        let (ux, uy) = (
            (before[0].0 + before[1].0) * 0.5,
            (before[0].1 + before[1].1) * 0.5,
        );

        app.apply_move_transform_action(MoveTransformAction::Rotate90Cw);

        for (&id, b) in ids.iter().zip(&before) {
            // Clockwise on screen: (dx, dy) -> (-dy, dx) about the pair's centre.
            let expected = (ux - (b.1 - uy), uy + (b.0 - ux));
            let got = centre(&app, id);
            assert!(
                (got.0 - expected.0).abs() <= 1.5 && (got.1 - expected.1).abs() <= 1.5,
                "path {id} centre {got:?}, expected {expected:?}"
            );
        }
    }

    #[test]
    fn move_rotate_ignores_an_empty_layer_in_the_selection() {
        let (mut app, cx2, cy2) = app_with_padded_selected_layer();
        let photo = {
            let canvas = &mut app.docs.documents[0].canvas;
            let photo = canvas.layer_stack.layers[canvas.layer_stack.active_idx].id;
            // A fresh layer: canvas-sized, not one visible pixel.
            let idx = canvas.layer_stack.add_layer(200, 160);
            canvas.layer_stack.layers[idx].selected = true;
            photo
        };
        app.apply_move_transform_action(MoveTransformAction::Rotate90Cw);

        let layer = find_layer(&app, photo);
        let (x0, y0, x1, y1) = layer.tiles.content_bounds().expect("content");
        assert_eq!(
            (layer.offset.0 * 2 + x0 + x1, layer.offset.1 * 2 + y0 + y1),
            (cx2, cy2),
            "the photo turns in place, not about the empty layer's canvas centre"
        );
    }
}

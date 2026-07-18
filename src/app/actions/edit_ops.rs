//! Direct edit operations: crop commits, layer align, Move-tool transform
//! actions, guides, invert and group/ungroup. Split out of app/actions.rs.

use crate::app::render::CanvasEvent;
use crate::app::state::App;
use crate::core::document::GuideOrientation;
use crate::core::snapping::{snap_1d, SnapKind, SNAP_THRESHOLD_PX};
use crate::ui::{LayerAlign, MoveTransformAction};

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

fn layer_transform_bounds(layer: &crate::core::layer::Layer) -> Option<(i32, i32, i32, i32)> {
    layer
        .tiles
        .content_bounds()
        .map(|(x0, y0, x1, y1)| {
            (
                layer.offset.0 + x0,
                layer.offset.1 + y0,
                layer.offset.0 + x1,
                layer.offset.1 + y1,
            )
        })
        .or_else(|| {
            if layer.width == 0 || layer.height == 0 {
                None
            } else {
                Some((
                    layer.offset.0,
                    layer.offset.1,
                    layer.offset.0 + layer.width as i32,
                    layer.offset.1 + layer.height as i32,
                ))
            }
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
        }
        {
            let mut ctx = crate::extension::tool::ToolCtx::new(
                &mut self.docs.documents[self.docs.active_doc_idx],
                self.edit.fg_color,
                self.edit.bg_color,
                self.edit.view.zoom,
                self.edit.view.offset_x,
                self.edit.view.offset_y,
            );
            self.edit.tools.active_on_confirm(&mut ctx);
        }
        // ALL canvas-size bookkeeping (GPU texture resize, compositor
        // invalidation, re-fit, recomposite) lives in apply_canvas_event now —
        // the SAME path undo/redo take. The old inline copy here pre-called
        // resize_canvas_texture, which defeated that path's size-change
        // detection, so a straight crop that kept the same window size left
        // the compositor "initialized" and the screen stale until a tool
        // switch (the "2nd crop onward doesn't update" bug).
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.apply_canvas_event(CanvasEvent::SelectionChanged);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        true
    }

    pub fn update_crop_preview(&mut self) {
        let preview = if self.edit.tools.active_id() == crate::tools::ToolId::Crop
            && self.edit.tools.crop().has_selection()
        {
            let c = self.edit.tools.crop();
            if c.rotation.abs() > 0.001 || c.image_tx.abs() > 0.001 || c.image_ty.abs() > 0.001 {
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
                })
            } else {
                None
            }
        } else {
            None
        };

        if let Some(gpu) = &mut self.win.gpu {
            gpu.compositor.crop_preview = preview;
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
                &mut self.docs.documents[self.docs.active_doc_idx],
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
        self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .metadata
            .resolution_ppi = persp_dpi;
        // Same centralised size-change path as commit_active_crop / undo.
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.apply_canvas_event(CanvasEvent::SelectionChanged);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        true
    }

    pub(super) fn align_selected_layers_to_canvas(&mut self, align: LayerAlign) -> bool {
        use crate::core::layer::LayerType;

        let moved_count = {
            let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
            let can_align = |layer: &crate::core::layer::Layer| {
                !layer.locked
                    && !layer.is_background
                    && matches!(
                        layer.layer_type,
                        LayerType::Raster
                            | LayerType::Text(_)
                            | LayerType::Shape(_)
                            | LayerType::SmartObject
                    )
            };

            let mut indices: Vec<usize> = canvas
                .layer_stack
                .layers
                .iter()
                .enumerate()
                .filter(|(_, layer)| layer.selected && can_align(layer))
                .map(|(idx, _)| idx)
                .collect();

            if indices.is_empty() {
                let idx = canvas.layer_stack.active_idx;
                if canvas.layer_stack.layers.get(idx).is_some_and(can_align) {
                    indices.push(idx);
                }
            }

            if indices.is_empty() {
                0
            } else {
                let bounds: Vec<(usize, i32, i32, i32, i32)> = indices
                    .iter()
                    .filter_map(|&idx| {
                        canvas.layer_stack.layers[idx].tiles.content_bounds().map(
                            |(x0, y0, x1, y1)| {
                                let layer = &canvas.layer_stack.layers[idx];
                                (
                                    idx,
                                    x0 + layer.offset.0,
                                    y0 + layer.offset.1,
                                    x1 + layer.offset.0,
                                    y1 + layer.offset.1,
                                )
                            },
                        )
                    })
                    .collect();

                if bounds.is_empty() {
                    0
                } else {
                    let mut cmd = crate::core::command::LayerStructureCommand::capture_before(
                        "Align Layers",
                        &canvas.layer_stack,
                        canvas.width,
                        canvas.height,
                    );

                    let reference = if bounds.len() > 1 {
                        bounds.iter().fold(
                            (i32::MAX, i32::MAX, i32::MIN, i32::MIN),
                            |(rx0, ry0, rx1, ry1), &(_, x0, y0, x1, y1)| {
                                (rx0.min(x0), ry0.min(y0), rx1.max(x1), ry1.max(y1))
                            },
                        )
                    } else {
                        (0, 0, canvas.width as i32, canvas.height as i32)
                    };
                    let (rx0, ry0, rx1, ry1) = reference;
                    let mut moved = 0usize;

                    for (idx, x0, y0, x1, y1) in bounds {
                        let (dx, dy) = match align {
                            LayerAlign::Left => (rx0 - x0, 0),
                            LayerAlign::HorizontalCenter => {
                                let canvas_center = (rx0 + rx1) as f32 * 0.5;
                                let layer_center = (x0 + x1) as f32 * 0.5;
                                ((canvas_center - layer_center).round() as i32, 0)
                            }
                            LayerAlign::Right => (rx1 - x1, 0),
                            LayerAlign::Top => (0, ry0 - y0),
                            LayerAlign::VerticalCenter => {
                                let canvas_center = (ry0 + ry1) as f32 * 0.5;
                                let layer_center = (y0 + y1) as f32 * 0.5;
                                (0, (canvas_center - layer_center).round() as i32)
                            }
                            LayerAlign::Bottom => (0, ry1 - y1),
                        };

                        if dx == 0 && dy == 0 {
                            continue;
                        }

                        let layer = &mut canvas.layer_stack.layers[idx];
                        layer.offset.0 += dx;
                        layer.offset.1 += dy;
                        moved += 1;
                    }

                    if moved > 0 {
                        cmd.capture_after(&canvas.layer_stack, canvas.width, canvas.height);
                        canvas.record(Box::new(cmd));
                    }
                    moved
                }
            }
        };

        if moved_count == 0 {
            self.shell.status_msg = "No movable layer content to align".to_string();
            return false;
        }

        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.shell.status_msg = if moved_count == 1 {
            "Aligned 1 layer".to_string()
        } else {
            format!("Aligned {moved_count} layers")
        };
        true
    }

    pub(super) fn apply_move_transform_action(&mut self, action: MoveTransformAction) {
        use crate::core::layer::{Layer, LayerMask, LayerType};

        fn can_transform(layer: &Layer) -> bool {
            (!layer.locked || layer.is_background)
                && matches!(
                    layer.layer_type,
                    LayerType::Raster
                        | LayerType::Text(_)
                        | LayerType::Shape(_)
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
            let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
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

                        for idx in targets {
                            let layer = &mut canvas.layer_stack.layers[idx];
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
                let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
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
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
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
        for (i, g) in self.docs.documents[self.docs.active_doc_idx]
            .guides
            .iter()
            .enumerate()
        {
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
        use crate::core::layer::PaintTarget;
        let (cw, ch) = {
            let d = &self.docs.documents[self.docs.active_doc_idx];
            (d.canvas.width, d.canvas.height)
        };
        let layers = &self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .layers;
        let Some(layer) = layers.get(idx) else {
            return;
        };

        let invert_mask = layer.paint_target == PaintTarget::Mask && layer.mask.is_some();
        if !invert_mask && (!layer.is_raster() || layer.locked) {
            self.shell.status_msg =
                "Invert cần layer raster (không khoá) hoặc một mask".to_string();
            return;
        }
        // Pixel invert edits the RGB mirror only and would orphan the ink
        // ground truth. Masks carry no ink, so mask invert stays available.
        if !invert_mask
            && self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .is_cmyk()
        {
            self.shell.status_msg =
                "Invert chưa hỗ trợ ở chế độ CMYK (dùng Curves đảo từng kênh mực)".to_string();
            return;
        }

        let label = if invert_mask { "Invert Mask" } else { "Invert" };
        let mut cmd = crate::core::command::LayerStructureCommand::capture_before(
            label,
            &self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack,
            cw,
            ch,
        );
        {
            let layer = &mut self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack
                .layers[idx];
            if invert_mask {
                if let Some(mask) = layer.mask.as_mut() {
                    mask.invert_tiles();
                }
            } else {
                layer.invert_pixels();
            }
        }
        cmd.capture_after(
            &self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack,
            cw,
            ch,
        );
        self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .record(Box::new(cmd));

        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.shell.status_msg = format!("{label} {}", egui_phosphor::regular::CHECK);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Ctrl+G — wrap the selected (or active) layers in a new group folder.
    /// Undoable. No-op (with a status hint) when only the background qualifies.
    pub fn do_group_selected(&mut self) {
        let (cw, ch) = {
            let d = &self.docs.documents[self.docs.active_doc_idx];
            (d.canvas.width, d.canvas.height)
        };
        let can_group = {
            let ls = &self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack;
            ls.layers.iter().any(|l| l.selected && !l.is_background)
                || ls
                    .layers
                    .get(ls.active_idx)
                    .map_or(false, |l| !l.is_background)
        };
        if !can_group {
            self.shell.status_msg = "Không có layer nào để nhóm".to_string();
            return;
        }

        let mut cmd = crate::core::command::LayerStructureCommand::capture_before(
            "Group Layers",
            &self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack,
            cw,
            ch,
        );
        self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .create_group_from_selected(cw, ch);
        cmd.capture_after(
            &self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack,
            cw,
            ch,
        );
        self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .record(Box::new(cmd));

        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.shell.status_msg = "Grouped layers".to_string();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Ctrl+Shift+G — dissolve the group at `idx` (children rise to the group's
    /// parent level). Undoable. No-op when `idx` is not a group.
    pub fn do_ungroup(&mut self, idx: usize) {
        let (cw, ch) = {
            let d = &self.docs.documents[self.docs.active_doc_idx];
            (d.canvas.width, d.canvas.height)
        };
        let is_grp = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .layers
            .get(idx)
            .map_or(false, |l| l.is_group());
        if !is_grp {
            self.shell.status_msg = "Layer hiện tại không phải nhóm".to_string();
            return;
        }

        let mut cmd = crate::core::command::LayerStructureCommand::capture_before(
            "Ungroup",
            &self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack,
            cw,
            ch,
        );
        self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .ungroup(idx);
        cmd.capture_after(
            &self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack,
            cw,
            ch,
        );
        self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .record(Box::new(cmd));

        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.shell.status_msg = "Ungrouped".to_string();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::transformed_layer_placement;
    use crate::app::state::App;
    use crate::core::canvas::Canvas;
    use crate::core::tile::TileMap;
    use crate::ui::MoveTransformAction;

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
}

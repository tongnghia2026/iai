use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::document::GuideOrientation;
use crate::core::snapping::{best_snap, SnapKind, SnapLine, SNAP_THRESHOLD_PX};

pub struct MoveTool {
    pub drag_start_x: f32,
    pub drag_start_y: f32,
    pub frac_x: f32,
    pub frac_y: f32,
    pub total_dx: i32,
    pub total_dy: i32,
    pub auto_select: bool,
    pub show_transform: bool,
    pub alt_duplicated: bool,
    pub is_moving_selection: bool,
    /// Tracks Alt held during the drag to duplicate on release.
    pub alt_held_during_drag: bool,
    pub marquee_dragging: bool,
    pub marquee_start_x: f32,
    pub marquee_start_y: f32,
    pub marquee_end_x: f32,
    pub marquee_end_y: f32,
    /// Structure command captured on Alt+drag duplicate, pushed in the same group
    /// as the TranslateLayerCommand on release so undo also removes the copy.
    pub pending_dup_cmd: Option<crate::core::command::LayerStructureCommand>,

    // --- Snapping (③) ---
    /// Master snap toggle (mirrors UiState.snap_enabled; set by the app each frame).
    pub snap_enabled: bool,
    /// Cursor canvas position at press — layer moves are tracked cumulatively from
    /// here so snapping/Shift can recompute the target each frame.
    press_cx: f32,
    press_cy: f32,
    /// Union content bbox (canvas coords) of the moving layers, captured at press.
    start_bbox: Option<(f32, f32, f32, f32)>,
    /// Snap targets along each axis (canvas edges/center, other layers, guides),
    /// captured at press (and after Alt-duplicate).
    snap_targets_x: Vec<(f32, SnapKind)>,
    snap_targets_y: Vec<(f32, SnapKind)>,
    /// Smart-guide lines to draw this frame; read by the app for the overlay.
    pub snap_guides: Vec<SnapLine>,
    snap_context_dirty: bool,
}

impl MoveTool {
    pub fn new() -> Self {
        Self {
            drag_start_x: 0.0,
            drag_start_y: 0.0,
            frac_x: 0.0,
            frac_y: 0.0,
            total_dx: 0,
            total_dy: 0,
            auto_select: true,
            show_transform: false,
            alt_duplicated: false,
            is_moving_selection: false,
            alt_held_during_drag: false,
            marquee_dragging: false,
            marquee_start_x: 0.0,
            marquee_start_y: 0.0,
            marquee_end_x: 0.0,
            marquee_end_y: 0.0,
            pending_dup_cmd: None,
            snap_enabled: true,
            press_cx: 0.0,
            press_cy: 0.0,
            start_bbox: None,
            snap_targets_x: Vec::new(),
            snap_targets_y: Vec::new(),
            snap_guides: Vec::new(),
            snap_context_dirty: false,
        }
    }

    /// Build the snap target lists + moving union bbox from the current layer
    /// stack: non-selected visible layers' content edges/centers, canvas
    /// edges/center, and the document's guides. Selected (moving) layers form the
    /// union bbox that will be snapped.
    fn build_snap_context(&mut self, ctx: &ToolCtx) {
        let canvas = ctx.canvas();
        let w = canvas.width as f32;
        let h = canvas.height as f32;

        let mut ux0 = f32::INFINITY;
        let mut uy0 = f32::INFINITY;
        let mut ux1 = f32::NEG_INFINITY;
        let mut uy1 = f32::NEG_INFINITY;
        let mut tx: Vec<(f32, SnapKind)> = vec![
            (0.0, SnapKind::CanvasEdge),
            (w, SnapKind::CanvasEdge),
            (w * 0.5, SnapKind::CanvasCenter),
        ];
        let mut ty: Vec<(f32, SnapKind)> = vec![
            (0.0, SnapKind::CanvasEdge),
            (h, SnapKind::CanvasEdge),
            (h * 0.5, SnapKind::CanvasCenter),
        ];

        for layer in canvas.layer_stack.layers.iter() {
            if layer.width == 0 || layer.height == 0 {
                continue;
            }
            let lx0 = layer.offset.0 as f32;
            let ly0 = layer.offset.1 as f32;
            let lx1 = lx0 + layer.width as f32;
            let ly1 = ly0 + layer.height as f32;

            if layer.selected && Self::can_move_layer(layer) {
                ux0 = ux0.min(lx0);
                uy0 = uy0.min(ly0);
                ux1 = ux1.max(lx1);
                uy1 = uy1.max(ly1);
            } else if layer.visible {
                tx.push((lx0, SnapKind::LayerEdge));
                tx.push((lx1, SnapKind::LayerEdge));
                tx.push(((lx0 + lx1) * 0.5, SnapKind::LayerCenter));
                ty.push((ly0, SnapKind::LayerEdge));
                ty.push((ly1, SnapKind::LayerEdge));
                ty.push(((ly0 + ly1) * 0.5, SnapKind::LayerCenter));
            }
        }

        for g in &ctx.document.guides {
            match g.orientation {
                GuideOrientation::Vertical => tx.push((g.pos, SnapKind::Guide)),
                GuideOrientation::Horizontal => ty.push((g.pos, SnapKind::Guide)),
            }
        }

        self.start_bbox = if ux0.is_finite() {
            Some((ux0, uy0, ux1, uy1))
        } else {
            None
        };
        self.snap_targets_x = tx;
        self.snap_targets_y = ty;
    }

    pub fn preview_rect(&self) -> Option<[f32; 4]> {
        if self.marquee_dragging {
            Some([
                self.marquee_start_x,
                self.marquee_start_y,
                self.marquee_end_x,
                self.marquee_end_y,
            ])
        } else {
            None
        }
    }

    fn can_move_layer(layer: &crate::core::layer::Layer) -> bool {
        use crate::core::layer::LayerType;

        !layer.locked
            && !layer.is_background
            && matches!(
                layer.layer_type,
                LayerType::Raster
                    | LayerType::Text(_)
                    | LayerType::Shape(_)
                    | LayerType::Path(_)
                    | LayerType::SmartObject
            )
    }
}

impl Tool for MoveTool {
    fn id(&self) -> &'static str {
        "move"
    }
    fn name(&self) -> &str {
        "Move"
    }
    fn shortcut(&self) -> Option<char> {
        Some('V')
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::Move
    }

    fn on_press(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        let (cx, cy) = (event.canvas_x, event.canvas_y);
        let canvas = ctx.canvas_mut();
        let mut changed_selection = false;

        if canvas.selection.active && canvas.selection.is_selected(cx as u32, cy as u32) {
            self.is_moving_selection = true;
        } else {
            self.is_moving_selection = false;
        }

        let do_auto_select = if event.ctrl {
            !self.auto_select
        } else {
            self.auto_select
        };
        if !self.is_moving_selection && do_auto_select {
            let x = cx as i32;
            let y = cy as i32;
            let mut hit_idx = None;
            for (idx, layer) in canvas.layer_stack.layers.iter().enumerate().rev() {
                if !layer.visible || !Self::can_move_layer(layer) {
                    continue;
                }
                let lx = x - layer.offset.0;
                let ly = y - layer.offset.1;
                if lx >= 0 && ly >= 0 && (lx as u32) < layer.width && (ly as u32) < layer.height {
                    let (_r, _g, _b, a) = layer.tiles.get_pixel(lx as u32, ly as u32);
                    if a > 0 {
                        hit_idx = Some(idx);
                        break;
                    }
                }
            }

            if let Some(idx) = hit_idx {
                if !canvas.layer_stack.layers[idx].selected {
                    for l in &mut canvas.layer_stack.layers {
                        l.selected = false;
                    }
                    canvas.layer_stack.layers[idx].selected = true;
                    canvas.layer_stack.active_idx = idx;
                    canvas.layer_revision += 1;
                    changed_selection = true;
                }
            } else {
                changed_selection = canvas.layer_stack.layers.iter().any(|l| l.selected);
                for l in &mut canvas.layer_stack.layers {
                    l.selected = false;
                }
                self.marquee_dragging = true;
                self.marquee_start_x = cx;
                self.marquee_start_y = cy;
                self.marquee_end_x = cx;
                self.marquee_end_y = cy;
            }
        }
        self.drag_start_x = event.screen_x;
        self.drag_start_y = event.screen_y;
        self.press_cx = event.canvas_x;
        self.press_cy = event.canvas_y;
        self.frac_x = 0.0;
        self.frac_y = 0.0;
        self.total_dx = 0;
        self.total_dy = 0;
        self.alt_duplicated = false;
        self.alt_held_during_drag = false;
        self.pending_dup_cmd = None;
        self.snap_guides.clear();
        self.start_bbox = None;
        self.snap_targets_x.clear();
        self.snap_targets_y.clear();
        self.snap_context_dirty = true;
        if changed_selection {
            ToolResponse::redraw()
        } else {
            ToolResponse::none()
        }
    }

    fn on_drag(
        &mut self,
        event: PointerEvent,
        prev: &PointerEvent,
        ctx: &mut ToolCtx,
    ) -> ToolResponse {
        if self.marquee_dragging {
            self.marquee_end_x = event.canvas_x;
            self.marquee_end_y = event.canvas_y;
            self.snap_guides.clear();
            return ToolResponse::redraw();
        }

        self.snap_guides.clear();

        // Moving the selection marquee content keeps the old incremental path
        // (no snapping).
        if self.is_moving_selection {
            let dx = {
                self.frac_x += event.canvas_x - prev.canvas_x;
                let d = self.frac_x.trunc() as i32;
                self.frac_x -= d as f32;
                d
            };
            let dy = {
                self.frac_y += event.canvas_y - prev.canvas_y;
                let d = self.frac_y.trunc() as i32;
                self.frac_y -= d as f32;
                d
            };
            if dx != 0 || dy != 0 {
                let canvas = ctx.canvas_mut();
                canvas.selection.offset.0 += dx;
                canvas.selection.offset.1 += dy;
                canvas.selection.mark_bbox_dirty();
                self.total_dx += dx;
                self.total_dy += dy;
                return ToolResponse::repaint();
            }
            return ToolResponse::none();
        }

        // --- Layer move: cumulative from the press point + Shift + snapping. ---
        let mut raw_dx = event.canvas_x - self.press_cx;
        let mut raw_dy = event.canvas_y - self.press_cy;

        // Shift = constrain to a straight line (lock the smaller axis).
        if event.shift {
            if raw_dx.abs() >= raw_dy.abs() {
                raw_dy = 0.0;
            } else {
                raw_dx = 0.0;
            }
        }
        let lock_x = event.shift && raw_dx == 0.0;
        let lock_y = event.shift && raw_dy == 0.0;

        let mut snap_dx = raw_dx;
        let mut snap_dy = raw_dy;
        if self.snap_enabled {
            if self.snap_context_dirty {
                self.build_snap_context(ctx);
                self.snap_context_dirty = false;
            }
            if let Some((x0, y0, x1, y1)) = self.start_bbox {
                let threshold = SNAP_THRESHOLD_PX / ctx.zoom.max(1e-4);
                if !lock_x {
                    let cands = [x0 + raw_dx, (x0 + x1) * 0.5 + raw_dx, x1 + raw_dx];
                    if let Some((adj, pos, kind)) =
                        best_snap(&cands, &self.snap_targets_x, threshold)
                    {
                        snap_dx = raw_dx + adj;
                        self.snap_guides.push(SnapLine {
                            vertical: true,
                            pos,
                            kind,
                        });
                    }
                }
                if !lock_y {
                    let cands = [y0 + raw_dy, (y0 + y1) * 0.5 + raw_dy, y1 + raw_dy];
                    if let Some((adj, pos, kind)) =
                        best_snap(&cands, &self.snap_targets_y, threshold)
                    {
                        snap_dy = raw_dy + adj;
                        self.snap_guides.push(SnapLine {
                            vertical: false,
                            pos,
                            kind,
                        });
                    }
                }
            }
        }

        let target_dx = snap_dx.round() as i32;
        let target_dy = snap_dy.round() as i32;
        let dx = target_dx - self.total_dx;
        let dy = target_dy - self.total_dy;

        if dx == 0 && dy == 0 {
            // Position unchanged, but the smart guides may need (re)drawing.
            return if self.snap_guides.is_empty() {
                ToolResponse::none()
            } else {
                ToolResponse::redraw()
            };
        }

        let mut did_dup = false;
        {
            let canvas = ctx.canvas_mut();

            if event.alt && !self.alt_duplicated {
                let to_duplicate: Vec<usize> = canvas
                    .layer_stack
                    .layers
                    .iter()
                    .enumerate()
                    .filter(|(_, l)| l.selected && Self::can_move_layer(l))
                    .map(|(i, _)| i)
                    .collect();

                if !to_duplicate.is_empty() {
                    let mut dup_cmd = crate::core::command::LayerStructureCommand::capture_before(
                        "Duplicate & Move",
                        &canvas.layer_stack,
                        canvas.width,
                        canvas.height,
                    );

                    for l in &mut canvas.layer_stack.layers {
                        l.selected = false;
                    }

                    for &idx in to_duplicate.iter().rev() {
                        let new_idx = canvas.layer_stack.duplicate_layer(idx);
                        canvas.layer_stack.layers[new_idx].selected = true;
                    }
                    dup_cmd.capture_after(&canvas.layer_stack, canvas.width, canvas.height);
                    self.pending_dup_cmd = Some(dup_cmd);
                    self.alt_duplicated = true;
                    did_dup = true;
                }
            }

            let has_movable = canvas
                .layer_stack
                .layers
                .iter()
                .any(|l| l.selected && Self::can_move_layer(l));

            if !has_movable {
                return ToolResponse::none();
            }

            let old_bounds: Vec<(i32, i32, u32, u32)> = canvas
                .layer_stack
                .layers
                .iter()
                .filter(|l| l.selected && Self::can_move_layer(l) && l.has_renderable_content())
                .map(|l| (l.offset.0, l.offset.1, l.width, l.height))
                .collect();

            for layer in canvas.layer_stack.layers.iter_mut() {
                if layer.selected && Self::can_move_layer(layer) {
                    layer.offset.0 += dx;
                    layer.offset.1 += dy;
                }
            }
            self.total_dx = target_dx;
            self.total_dy = target_dy;

            for (ox, oy, lw, lh) in old_bounds {
                canvas.mark_dirty_layer_bounds(ox, oy, lw, lh);
                canvas.mark_dirty_layer_bounds(ox + dx, oy + dy, lw, lh);
            }
        }

        // After an Alt-duplicate the originals become snap targets (and the copies
        // are the moving set) — rebuild so the copy can snap to the original.
        if did_dup {
            self.build_snap_context(ctx);
            self.snap_context_dirty = false;
        }

        ToolResponse::repaint()
    }

    fn on_release(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        if self.marquee_dragging {
            self.marquee_dragging = false;
            let canvas = ctx.canvas_mut();
            let x0 = self.marquee_start_x.min(self.marquee_end_x);
            let y0 = self.marquee_start_y.min(self.marquee_end_y);
            let x1 = self.marquee_start_x.max(self.marquee_end_x);
            let y1 = self.marquee_start_y.max(self.marquee_end_y);

            let mut hit_indices = Vec::new();

            for (idx, layer) in canvas.layer_stack.layers.iter().enumerate().rev() {
                if !layer.visible || !Self::can_move_layer(layer) {
                    continue;
                }
                let ox = layer.offset.0;
                let oy = layer.offset.1;

                let mut hit = false;
                for (pos, tile) in layer.tiles.tiles.iter() {
                    let tx0 = pos.x * crate::core::tile::TILE_SIZE as i32 + ox;
                    let ty0 = pos.y * crate::core::tile::TILE_SIZE as i32 + oy;
                    let tx1 = tx0 + crate::core::tile::TILE_SIZE as i32;
                    let ty1 = ty0 + crate::core::tile::TILE_SIZE as i32;

                    if tx0 < x1 as i32 && tx1 > x0 as i32 && ty0 < y1 as i32 && ty1 > y0 as i32 {
                        let start_x = (x0 as i32 - tx0).max(0);
                        let start_y = (y0 as i32 - ty0).max(0);
                        let end_x = (x1 as i32 - tx0).min(crate::core::tile::TILE_SIZE as i32);
                        let end_y = (y1 as i32 - ty0).min(crate::core::tile::TILE_SIZE as i32);

                        for i in start_y..end_y {
                            for j in start_x..end_x {
                                let a = tile.pixels[((i * crate::core::tile::TILE_SIZE as i32 + j)
                                    * 4
                                    + 3)
                                    as usize];
                                if a > 0 {
                                    hit = true;
                                    break;
                                }
                            }
                            if hit {
                                break;
                            }
                        }
                    }
                    if hit {
                        break;
                    }
                }

                if hit {
                    hit_indices.push(idx);
                }
            }

            if !hit_indices.is_empty() {
                for l in &mut canvas.layer_stack.layers {
                    l.selected = false;
                }
                for &idx in &hit_indices {
                    canvas.layer_stack.layers[idx].selected = true;
                }
                canvas.layer_stack.active_idx = hit_indices[0];
                canvas.layer_revision += 1;
            }
            return ToolResponse::repaint();
        }

        if self.is_moving_selection {
            self.is_moving_selection = false;
            if self.total_dx != 0 || self.total_dy != 0 {
                let canvas = ctx.canvas_mut();
                if !canvas.move_selected_content(self.total_dx, self.total_dy) {
                    let cmd = crate::core::command::TranslateSelectionCommand::from_applied_move(
                        &canvas.selection,
                        self.total_dx,
                        self.total_dy,
                    );
                    canvas.record(Box::new(cmd));
                }
            }
            return ToolResponse::repaint();
        }

        let _res = ToolResponse::none();

        let dup_cmd = self.pending_dup_cmd.take();
        let moved = self.total_dx != 0 || self.total_dy != 0;
        if dup_cmd.is_some() || moved {
            let canvas = ctx.canvas_mut();
            canvas.begin_undo_group("Duplicate & Move");
            if let Some(c) = dup_cmd {
                canvas.record(Box::new(c));
            }
            if moved {
                let mut layer_ids: Vec<u32> = canvas
                    .layer_stack
                    .layers
                    .iter()
                    .filter(|l| l.selected && Self::can_move_layer(l))
                    .map(|l| l.id)
                    .collect();
                if layer_ids.is_empty() {
                    if let Some(layer) =
                        canvas.layer_stack.layers.get(canvas.layer_stack.active_idx)
                    {
                        if Self::can_move_layer(layer) {
                            layer_ids.push(layer.id);
                        }
                    }
                }
                if !layer_ids.is_empty() {
                    let cmd = crate::core::command::TranslateLayerCommand::from_applied_move(
                        layer_ids,
                        self.total_dx,
                        self.total_dy,
                        &canvas.layer_stack,
                    );
                    canvas.record(Box::new(cmd));
                }
            }
            canvas.end_undo_group();
        }

        let _ = event;
        self.alt_held_during_drag = false;
        self.alt_duplicated = false;
        self.snap_guides.clear();
        self.start_bbox = None;
        self.snap_targets_x.clear();
        self.snap_targets_y.clear();
        self.snap_context_dirty = false;
        ToolResponse::none()
    }
}

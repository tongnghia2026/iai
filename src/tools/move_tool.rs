use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::document::GuideOrientation;
use crate::core::snapping::{best_snap, SnapKind, SnapLine, SNAP_THRESHOLD_PX};
use crate::core::vector::object::VectorGeometry;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConstrainedAxis {
    Horizontal,
    Vertical,
}

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
    /// Last committed duplicate translation. Repeating it duplicates the current
    /// selection by the same vector (Corel-style step-and-repeat).
    pub last_duplicate_delta: Option<(i32, i32)>,
    /// Set on the release that recorded an Alt+drag duplicate, so the app can
    /// mirror the translation into the unified repeatable step (`last_repeat_transform`).
    pub took_duplicate: bool,
    /// Consumed by App after a click (without a drag) on an already-selected vector.
    pub toggle_vector_transform_requested: bool,
    click_selected_vector: bool,

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
    /// Dominant axis chosen when Shift is first observed during a drag. Keep it
    /// stable until Shift is released so crossing the diagonal cannot make the
    /// moving layer group jump from horizontal to vertical (or vice versa).
    constrained_axis: Option<ConstrainedAxis>,
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
            last_duplicate_delta: None,
            took_duplicate: false,
            toggle_vector_transform_requested: false,
            click_selected_vector: false,
            snap_enabled: true,
            press_cx: 0.0,
            press_cy: 0.0,
            start_bbox: None,
            snap_targets_x: Vec::new(),
            snap_targets_y: Vec::new(),
            snap_guides: Vec::new(),
            snap_context_dirty: false,
            constrained_axis: None,
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

    /// Arm the second-click toggle from App-level vector-model hit testing.
    /// This supplements the raster-cache bounds check in `on_press`, which can
    /// be stale for a transformed Path before its asynchronous bake settles.
    pub fn arm_vector_transform_toggle(&mut self) {
        self.click_selected_vector = true;
    }

    fn can_move_layer(layer: &crate::core::layer::Layer) -> bool {
        use crate::core::layer::LayerType;

        !layer.locked
            && !layer.is_background
            && matches!(
                layer.layer_type,
                LayerType::Raster
                    | LayerType::Text(_)
                    | LayerType::Vector(_)
                    | LayerType::SmartObject
            )
    }

    /// True when a layer-local pixel is hidden by a clipping-mask / PowerClip
    /// frame. Such pixels are invisible on canvas, so a click or a marquee
    /// (including one drawn entirely outside the frame) must not select the layer
    /// through them. Only clip children are gated — an ordinary layer mask keeps
    /// its historic selectable-through-mask behaviour. `lx/ly` are layer-local.
    fn clip_hides(layer: &crate::core::layer::Layer, lx: u32, ly: u32) -> bool {
        layer.clip_parent_id.is_some()
            && layer
                .mask
                .as_ref()
                .is_some_and(|m| m.enabled && m.sample(lx, ly) < 0.5)
    }

    /// Canvas-space point `(x,y)` inside a layer's raster bounding box.
    fn point_in_layer_bounds(layer: &crate::core::layer::Layer, x: i32, y: i32) -> bool {
        let lx = x - layer.offset.0;
        let ly = y - layer.offset.1;
        lx >= 0 && ly >= 0 && (lx as u32) < layer.width && (ly as u32) < layer.height
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
        self.toggle_vector_transform_requested = false;
        self.click_selected_vector = canvas.layer_stack.layers.iter().any(|layer| {
            layer.selected
                && layer.visible
                && Self::can_move_layer(layer)
                && matches!(layer.layer_type, crate::core::layer::LayerType::Vector(_))
                && Self::point_in_layer_bounds(layer, cx as i32, cy as i32)
        });
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
                    if a > 0 && !Self::clip_hides(layer, lx as u32, ly as u32) {
                        hit_idx = Some(idx);
                        break;
                    }
                }
            }

            // CorelDRAW "treat as filled" for an already-SELECTED vector object:
            // when no opaque pixel sits under the cursor, still grab a selected,
            // movable Vector layer whose bounding box contains the press. This lets
            // a thin Vector Brush stroke (or an unfilled outline path) be dragged
            // from anywhere in its wide selection box, not only by clicking exactly
            // on the visible stroke. Opaque pixel hits are resolved FIRST above, so
            // this never steals a click that lands on another object.
            if hit_idx.is_none() {
                for (idx, layer) in canvas.layer_stack.layers.iter().enumerate().rev() {
                    if layer.selected
                        && layer.visible
                        && Self::can_move_layer(layer)
                        && matches!(layer.layer_type, crate::core::layer::LayerType::Vector(_))
                        && Self::point_in_layer_bounds(layer, x, y)
                    {
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
        self.constrained_axis = None;
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

        // Shift = constrain to the dominant axis chosen at the start of the
        // constraint. Do not recalculate it on every pointer event: otherwise a
        // small wobble across the diagonal makes the entire selection change
        // direction and visibly jump off the intended straight line.
        let (lock_x, lock_y) = if event.shift {
            let axis = *self.constrained_axis.get_or_insert_with(|| {
                if raw_dx.abs() >= raw_dy.abs() {
                    ConstrainedAxis::Horizontal
                } else {
                    ConstrainedAxis::Vertical
                }
            });
            match axis {
                ConstrainedAxis::Horizontal => {
                    raw_dy = 0.0;
                    (false, true)
                }
                ConstrainedAxis::Vertical => {
                    raw_dx = 0.0;
                    (true, false)
                }
            }
        } else {
            // Releasing Shift returns to free movement; pressing it again starts
            // a fresh constraint based on the pointer's current dominant axis.
            self.constrained_axis = None;
            (false, false)
        };

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

            // Keep a clipping-mask child's derived mask in the same coordinate
            // space as its moving image. The GPU live-pin preview deliberately
            // keeps the previous bake, but that can make the image disappear
            // where its source and mask tile maps diverge until mouse-up. Re-bake
            // after the offset changes so the normal zero-shift path is exact
            // throughout the drag. The fingerprint gate makes unrelated moves a
            // cheap no-op.
            let clip_mask_changed = canvas.refresh_clip_masks();

            for (ox, oy, lw, lh) in old_bounds {
                canvas.mark_dirty_layer_bounds(ox, oy, lw, lh);
                canvas.mark_dirty_layer_bounds(ox + dx, oy + dy, lw, lh);
            }
            if clip_mask_changed {
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
                                // A clip child's hidden pixels are invisible, so a
                                // marquee over them (e.g. drawn outside the frame)
                                // must not select the layer through them.
                                let lx = (pos.x * crate::core::tile::TILE_SIZE as i32 + j) as u32;
                                let ly = (pos.y * crate::core::tile::TILE_SIZE as i32 + i) as u32;
                                if a > 0 && !Self::clip_hides(layer, lx, ly) {
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
        let duplicated = dup_cmd.is_some();
        let moved = self.total_dx != 0 || self.total_dy != 0;
        self.toggle_vector_transform_requested = self.click_selected_vector
            && !moved
            && !duplicated
            && !event.shift
            && !event.ctrl
            && !event.alt;
        self.click_selected_vector = false;
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
                // A Path layer carries its position in the vector MODEL (the raster
                // is a cache), so fold the drag into the model transform via
                // ChangeVectorTransform — the object stays editable, the offset
                // and model never drift apart (so the on-canvas transform box is
                // always exact), and the position survives a reload. Everything
                // else keeps the relative TranslateLayerCommand.
                let (path_ids, other_ids): (Vec<u32>, Vec<u32>) =
                    layer_ids.into_iter().partition(|id| {
                        canvas
                            .layer_stack
                            .layers
                            .iter()
                            .find(|l| l.id == *id)
                            .is_some_and(|l| {
                                matches!(
                                    l.layer_type,
                                    crate::core::layer::LayerType::Vector(VectorGeometry::Path(_))
                                )
                            })
                    });
                if !other_ids.is_empty() {
                    let cmd = crate::core::command::TranslateLayerCommand::from_applied_move(
                        other_ids,
                        self.total_dx,
                        self.total_dy,
                        &canvas.layer_stack,
                    );
                    canvas.record(Box::new(cmd));
                }
                let (tdx, tdy) = (self.total_dx as f32, self.total_dy as f32);
                for id in path_ids {
                    let m0 = canvas
                        .layer_stack
                        .layers
                        .iter()
                        .find(|l| l.id == id)
                        .and_then(|l| match &l.layer_type {
                            crate::core::layer::LayerType::Vector(VectorGeometry::Path(o)) => {
                                Some(o.transform)
                            }
                            _ => None,
                        });
                    if let Some(m0) = m0 {
                        let new_t =
                            crate::core::vector::affine::AffineTransform::translate(tdx, tdy)
                                .then(&m0);
                        let _ = canvas.execute(
                            Box::new(crate::core::command_vector::ChangeVectorTransform::new(
                                id, new_t,
                            )),
                            crate::core::gateway::ChangeKind::LayerStructure,
                        );
                    }
                }
            }
            canvas.end_undo_group();
            if duplicated && moved {
                self.last_duplicate_delta = Some((self.total_dx, self.total_dy));
                self.took_duplicate = true;
            }
        }

        let _ = event;
        self.alt_held_during_drag = false;
        self.alt_duplicated = false;
        self.snap_guides.clear();
        self.start_bbox = None;
        self.snap_targets_x.clear();
        self.snap_targets_y.clear();
        self.snap_context_dirty = false;
        self.constrained_axis = None;
        ToolResponse::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::document::{Document, DocumentId};
    use crate::core::geometry::Point;
    use crate::core::layer::LayerType;
    use crate::core::vector::object::{VectorGeometry, VectorObjectData};
    use crate::core::vector::path::{Contour, FillRule, Node, PathData};

    fn ctx(doc: &mut Document) -> ToolCtx<'_> {
        ToolCtx::new(doc, [0, 0, 0, 255], [255, 255, 255, 255], 1.0, 0.0, 0.0)
    }

    /// A Vector Path layer occupying `[ox,ox+w)×[oy,oy+h)` with NO raster content
    /// (fully transparent) — mimicking a thin brush stroke whose wide bounding box
    /// is mostly empty, so a press in its interior misses the opaque-pixel pass.
    fn add_empty_vector_layer(
        doc: &mut Document,
        ox: i32,
        oy: i32,
        w: u32,
        h: u32,
        selected: bool,
    ) -> usize {
        let idx = doc.canvas.layer_stack.add_layer(w, h);
        let path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(w as f32, h as f32)),
                ],
                false,
            )],
            FillRule::NonZero,
        );
        let obj = VectorObjectData::from_path(path);
        let layer = &mut doc.canvas.layer_stack.layers[idx];
        layer.offset = (ox, oy);
        layer.width = w;
        layer.height = h;
        layer.layer_type = LayerType::Vector(VectorGeometry::Path(obj));
        layer.selected = selected;
        idx
    }

    #[test]
    fn selected_vector_grabs_from_empty_bbox_interior() {
        // The A-fix: a selected Vector Brush / unfilled path is draggable from
        // anywhere in its box, not only by clicking the visible stroke.
        let mut t = MoveTool::new();
        let mut doc = Document::new(DocumentId(1), 300, 300);
        let idx = add_empty_vector_layer(&mut doc, 50, 50, 100, 100, true);
        doc.canvas.layer_stack.active_idx = idx;

        {
            let mut c = ctx(&mut doc);
            // Deep inside the box, on transparent area (no stroke pixel here).
            t.on_press(PointerEvent::new(100.0, 100.0), &mut c);
        }
        assert!(
            !t.marquee_dragging,
            "a selected vector must be grabbed for moving, not marquee-selected"
        );
        assert!(
            doc.canvas.layer_stack.layers[idx].selected,
            "the selection is kept so the drag moves it"
        );
    }

    #[test]
    fn unselected_vector_box_still_marquees() {
        // The grab is scoped to the SELECTED object, so an unselected empty box
        // keeps the old behaviour (clears selection, starts a marquee).
        let mut t = MoveTool::new();
        let mut doc = Document::new(DocumentId(1), 300, 300);
        add_empty_vector_layer(&mut doc, 50, 50, 100, 100, false);

        {
            let mut c = ctx(&mut doc);
            t.on_press(PointerEvent::new(100.0, 100.0), &mut c);
        }
        assert!(
            t.marquee_dragging,
            "an unselected empty vector box must not be grabbed"
        );
    }

    #[test]
    fn shift_moves_multiple_selected_layers_on_one_axis() {
        let mut t = MoveTool::new();
        t.snap_enabled = false;
        let mut doc = Document::new(DocumentId(1), 400, 300);
        let first = add_empty_vector_layer(&mut doc, 40, 50, 80, 80, true);
        let second = add_empty_vector_layer(&mut doc, 70, 70, 80, 80, true);
        doc.canvas.layer_stack.active_idx = second;

        let press = PointerEvent::new(90.0, 90.0);
        let mut drag = PointerEvent::new(150.0, 105.0);
        drag.shift = true;
        {
            let mut c = ctx(&mut doc);
            t.on_press(press, &mut c);
            t.on_drag(drag, &press, &mut c);
        }

        assert_eq!(doc.canvas.layer_stack.layers[first].offset, (100, 50));
        assert_eq!(doc.canvas.layer_stack.layers[second].offset, (130, 70));
    }

    #[test]
    fn shift_keeps_the_initial_axis_for_the_whole_multi_layer_drag() {
        let mut t = MoveTool::new();
        t.snap_enabled = false;
        let mut doc = Document::new(DocumentId(1), 400, 300);
        let first = add_empty_vector_layer(&mut doc, 40, 50, 80, 80, true);
        let second = add_empty_vector_layer(&mut doc, 70, 70, 80, 80, true);
        doc.canvas.layer_stack.active_idx = second;

        let press = PointerEvent::new(90.0, 90.0);
        let mut horizontal_drag = PointerEvent::new(150.0, 105.0);
        horizontal_drag.shift = true;
        let mut crosses_diagonal = PointerEvent::new(155.0, 180.0);
        crosses_diagonal.shift = true;
        {
            let mut c = ctx(&mut doc);
            t.on_press(press, &mut c);
            t.on_drag(horizontal_drag, &press, &mut c);
            t.on_drag(crosses_diagonal, &horizontal_drag, &mut c);
        }

        // The first Shift-drag chose horizontal. Even though the later pointer
        // delta is vertically dominant, both selected layers stay on that line.
        assert_eq!(doc.canvas.layer_stack.layers[first].offset, (105, 50));
        assert_eq!(doc.canvas.layer_stack.layers[second].offset, (135, 70));
    }
}

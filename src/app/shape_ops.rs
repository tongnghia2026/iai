// Shape tool flow: create (and, later, edit) a vector Shape layer from the Shape
// tool's rubber-band drag. Mirrors text_ops.rs — the tool object (tools/shape.rs)
// only holds the draw settings + live preview; the real work (layer creation,
// rendering, undo) happens here at the App level so layer structure is undoable.

use super::render::CanvasEvent;
use super::state::{App, ShapeDragState};
use crate::core::command::LayerStructureCommand;
use crate::core::layer::LayerType;
use crate::core::shape::{ShapeData, ShapeHandle, ShapeKind};
use crate::core::tile::TileMap;
use crate::core::vector::object::VectorGeometry;

/// Screen-space grab radius for a handle.
const HANDLE_HIT_PX: f32 = 8.0;
/// Minimum screen inset of the corner-radius node from the top-left corner, so
/// it stays grabbable (and distinct from the corner handle) even at radius 0.
const RADIUS_NODE_INSET_PX: f32 = 16.0;

impl App {
    /// Create a new Shape layer from a canvas-space drag span, using the Shape
    /// tool's current style (kind, fill/stroke colours, width, corner radius).
    pub fn begin_new_shape(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) {
        let (kind, fill, fill_color, stroke_width, stroke_color, corner_radius, sides, star_inner) = {
            let s = self.edit.tools.shape();
            (
                s.kind,
                s.fill,
                s.fill_color,
                s.stroke_width,
                s.stroke_color,
                s.corner_radius,
                s.sides,
                s.star_inner,
            )
        };
        let (mut data, offset) = ShapeData::from_canvas_span(
            kind,
            x0,
            y0,
            x1,
            y1,
            corner_radius,
            fill,
            fill_color,
            stroke_width,
            stroke_color,
        );
        // Polygon/Star parameters live on the tool, not the span.
        data.sides = sides.clamp(3, 100);
        data.star_inner = star_inner.clamp(0.05, 0.95);
        let Some(raster) = data.render() else {
            return;
        };

        let (cw, ch) = {
            let d = &self.docs.documents[self.docs.active_doc_idx];
            (d.canvas.width, d.canvas.height)
        };
        let doc_idx = self.docs.active_doc_idx;
        let canvas = &mut self.docs.documents[doc_idx].canvas;
        let before = LayerStructureCommand::capture_before("Shape", &canvas.layer_stack, cw, ch);
        let idx = canvas.layer_stack.add_layer(cw, ch);

        let mut tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
        // Keep the layer ink-valid on CMYK documents (mirror → ink).
        if let Some(conv) = canvas.cmyk_converter() {
            tiles.encode_ink_from_mirror(&conv);
        }
        let layer = &mut canvas.layer_stack.layers[idx];
        layer.tiles = tiles;
        layer.width = raster.width;
        layer.height = raster.height;
        layer.offset = offset;
        layer.name = kind.label().to_string();
        layer.layer_type = LayerType::Vector(VectorGeometry::Primitive(data));

        let mut cmd = before;
        cmd.capture_after(&canvas.layer_stack, cw, ch);
        canvas.record(Box::new(cmd));
        canvas.layer_revision += 1;

        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// The active layer index if it is an editable Shape layer.
    fn active_shape_index(&self) -> Option<usize> {
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let idx = canvas.layer_stack.active_idx;
        let layer = canvas.layer_stack.layers.get(idx)?;
        if matches!(
            layer.layer_type,
            LayerType::Vector(VectorGeometry::Primitive(_))
        ) && layer.visible
            && !layer.locked
        {
            Some(idx)
        } else {
            None
        }
    }

    fn shape_at(&self, idx: usize) -> Option<(ShapeData, (i32, i32))> {
        let layer = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .layers
            .get(idx)?;
        if let LayerType::Vector(VectorGeometry::Primitive(d)) = &layer.layer_type {
            Some((d.clone(), layer.offset))
        } else {
            None
        }
    }

    /// Convert the Shape layer at `idx` into an editable vector Path layer
    /// ("Convert to Curves", Giai đoạn 4). The primitive geometry becomes a
    /// `PathData` (rectangle/rounded-rect/ellipse → closed curves, line → open),
    /// the fill/stroke carry over through the existing style adapter, and the
    /// layer is swapped IN PLACE so its id / stacking order / opacity / blend /
    /// mask are kept. One structural undo restores the Shape. Returns true when it
    /// converted.
    pub fn convert_shape_to_path(&mut self, idx: usize) -> bool {
        use crate::core::vector::affine::AffineTransform;
        use crate::core::vector::from_shape;
        use crate::core::vector::object::VectorObjectData;

        let (data, offset) = match self.shape_at(idx) {
            Some(v) => v,
            None => return false,
        };
        let (x0, y0, x1, y1) = data.canvas_span(offset);
        // Shape geometry is layer-local; convert to canvas space so the Path lands
        // in the same place (its transform is then identity — delta-0 invariant).
        let path = match data.kind {
            ShapeKind::Rectangle => from_shape::rect_path(x0, y0, x1, y1, data.effective_radius()),
            ShapeKind::Ellipse => from_shape::ellipse_path(x0, y0, x1, y1),
            ShapeKind::Line => from_shape::line_path(x0, y0, x1, y1),
            ShapeKind::Polygon => from_shape::polygon_path(x0, y0, x1, y1, data.sides),
            ShapeKind::Star => from_shape::star_path(x0, y0, x1, y1, data.sides, data.star_inner),
        };
        let object = VectorObjectData::new(path, data.style, AffineTransform::IDENTITY);

        let (cw, ch) = {
            let d = &self.docs.documents[self.docs.active_doc_idx];
            (d.canvas.width, d.canvas.height)
        };
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let before =
            LayerStructureCommand::capture_before("Convert to Curves", &canvas.layer_stack, cw, ch);
        {
            let Some(layer) = canvas.layer_stack.layers.get_mut(idx) else {
                return false;
            };
            crate::core::command_vector::apply_object_to_layer(layer, object);
        }
        // CMYK: re-derive ink planes for the new Path raster from its RGB mirror.
        canvas.reconcile_path_ink();
        canvas.layer_revision += 1;

        let mut cmd = before;
        cmd.capture_after(&canvas.layer_stack, cw, ch);
        canvas.record(Box::new(cmd));

        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.shell.status_msg = "Đã chuyển Shape thành đường cong (Path)".to_string();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        true
    }

    /// On-canvas editing overlay for the active Shape layer: the bounding-box
    /// span, the kind, the effective corner radius, and every handle position
    /// (incl. the zoom-dependent corner-radius node). Consumed by hit-testing
    /// and the UI overlay. During a handle drag this reflects the drag's
    /// pending geometry, so the overlay tracks the cursor even while raster
    /// bakes are throttled.
    /// Returns `(span[x0,y0,x1,y1], kind_u8, radius, handles[(handle_u8, cx, cy)])`.
    pub fn active_shape_overlay(&self) -> Option<([f32; 4], u8, f32, Vec<(u8, f32, f32)>)> {
        let idx = self.active_shape_index()?;
        let (mut data, offset) = match self.edit.shape_drag.as_ref().and_then(|d| {
            let layer = &self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack
                .layers[idx];
            (layer.id == d.layer_id)
                .then(|| d.pending.clone())
                .flatten()
        }) {
            Some(p) => p,
            None => self.shape_at(idx)?,
        };
        // A deferred options-bar radius scrub: the outline previews the tool's
        // target radius while the raster bake catches up.
        if self.edit.shape_style_pending.as_ref().is_some_and(|p| {
            p.dirty
                && p.doc_id == self.docs.documents[self.docs.active_doc_idx].id
                && p.layer_id
                    == self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .layer_stack
                        .layers[idx]
                        .id
        }) {
            data.corner_radius = self.edit.tools.shape().corner_radius;
        }
        let mut handles: Vec<(u8, f32, f32)> = data
            .handle_points(offset)
            .into_iter()
            .map(|(h, x, y)| (h.to_u8(), x, y))
            .collect();
        let (sx0, sy0, sx1, sy1) = data.canvas_span(offset);
        let mut radius = 0.0;
        if data.kind == ShapeKind::Rectangle {
            let minx = sx0.min(sx1);
            let miny = sy0.min(sy1);
            let maxx = sx0.max(sx1);
            let r = data.effective_radius();
            radius = r;
            let inset = (RADIUS_NODE_INSET_PX / self.edit.view.zoom).max(0.0);
            let node_x = (minx + r.max(inset)).min((minx + maxx) * 0.5);
            handles.push((ShapeHandle::Radius.to_u8(), node_x, miny));
        }
        Some(([sx0, sy0, sx1, sy1], data.kind.to_u8(), radius, handles))
    }

    /// Which handle of the active Shape layer is under `(cx,cy)`, if any.
    pub fn shape_handle_at(&self, cx: f32, cy: f32) -> Option<ShapeHandle> {
        let (_, _, _, handles) = self.active_shape_overlay()?;
        let thresh = HANDLE_HIT_PX / self.edit.view.zoom;
        let mut best: Option<(f32, u8)> = None;
        for (h, hx, hy) in handles {
            let d = ((cx - hx).powi(2) + (cy - hy).powi(2)).sqrt();
            if d <= thresh && best.map_or(true, |(bd, _)| d < bd) {
                best = Some((d, h));
            }
        }
        best.map(|(_, h)| ShapeHandle::from_u8(h))
    }

    /// Begin dragging the given handle of the active Shape layer.
    pub fn shape_begin_handle_drag(&mut self, handle: ShapeHandle) {
        let Some(idx) = self.active_shape_index() else {
            return;
        };
        let (cw, ch) = {
            let d = &self.docs.documents[self.docs.active_doc_idx];
            (d.canvas.width, d.canvas.height)
        };
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let layer_id = canvas.layer_stack.layers[idx].id;
        let before =
            LayerStructureCommand::capture_before("Edit Shape", &canvas.layer_stack, cw, ch);
        self.edit.shape_drag = Some(ShapeDragState {
            layer_id,
            handle,
            before_cmd: Some(before),
            changed: false,
            pending: None,
            last_bake: None,
            bake_cost_secs: 0.0,
        });
    }

    /// True while a Shape handle is being dragged.
    pub fn shape_drag_active(&self) -> bool {
        self.edit.shape_drag.is_some()
    }

    /// True while an options-bar style scrub has a bake still pending.
    pub fn shape_style_scrub_active(&self) -> bool {
        self.edit
            .shape_style_pending
            .as_ref()
            .is_some_and(|p| p.dirty)
    }

    /// Apply the in-progress handle drag to `(cx,cy)`. The new geometry is
    /// stored as the drag's pending target every move (the vector overlay
    /// tracks it at full frame rate); the CPU rasterization into the layer is
    /// throttled by its own measured cost so huge shapes can't stall the drag.
    pub fn shape_drag_update(&mut self, cx: f32, cy: f32) {
        let (handle, layer_id, pending) = match &self.edit.shape_drag {
            Some(d) => (d.handle, d.layer_id, d.pending.clone()),
            None => return,
        };
        // Resize from the live target, not the (possibly stale) baked layer.
        let (data, offset) = match pending {
            Some(p) => p,
            None => {
                let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
                let Some(layer) = canvas.layer_stack.layers.iter().find(|l| l.id == layer_id)
                else {
                    return;
                };
                let LayerType::Vector(VectorGeometry::Primitive(d)) = &layer.layer_type else {
                    return;
                };
                (d.clone(), layer.offset)
            }
        };
        let (new_data, new_offset) = data.resize_by_handle(offset, handle, cx, cy);
        if new_data == data && new_offset == offset {
            return;
        }
        let bake_due = {
            let Some(d) = self.edit.shape_drag.as_mut() else {
                return;
            };
            d.pending = Some((new_data.clone(), new_offset));
            d.changed = true;
            // Synchronous fallback (CMYK) only: keep the raster live when it's
            // cheap, back off to a ~33% duty cycle when a bake costs real time.
            let wait = (d.bake_cost_secs * 2.0).clamp(0.0, 2.0);
            d.last_bake
                .map_or(true, |t| t.elapsed().as_secs_f32() >= wait)
        };
        if !self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .is_cmyk()
        {
            // Off-thread bake: one worker job at a time is the natural
            // throttle; its completion (poll_shape_bake) chains the newest
            // pending target, so the UI thread never blocks on rasterization.
            if self.jobs.shape_bake.is_none() {
                let doc_id = self.docs.documents[self.docs.active_doc_idx].id;
                self.spawn_shape_bake(doc_id, layer_id, new_data, new_offset);
            } else if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        } else if bake_due {
            // CMYK documents bake synchronously (worker tiles carry no ink).
            self.shape_drag_bake();
        } else if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Kick a worker thread that rasterizes `data` and builds its `TileMap`.
    /// The result is picked up by `poll_shape_bake` on a later frame.
    fn spawn_shape_bake(
        &mut self,
        doc_id: crate::core::document::DocumentId,
        layer_id: u32,
        data: ShapeData,
        offset: (i32, i32),
    ) {
        let (tx, rx) = std::sync::mpsc::channel();
        let job = data.clone();
        std::thread::spawn(move || {
            let _ = tx.send(job.render().map(|r| {
                let tiles = TileMap::from_rgba(&r.rgba, r.width, r.height);
                (tiles, r.width, r.height)
            }));
        });
        self.jobs.shape_bake = Some(crate::app::state::ShapeBakeInFlight {
            doc_id,
            layer_id,
            data,
            offset,
            started: std::time::Instant::now(),
            rx,
        });
        self.arm_shape_bake_poll();
    }

    /// Guarantee an egui frame soon while a worker bake is in flight, so its
    /// result lands even when the pointer goes idle.
    fn arm_shape_bake_poll(&mut self) {
        let due = std::time::Instant::now() + std::time::Duration::from_millis(30);
        self.win.egui_repaint_deadline =
            Some(self.win.egui_repaint_deadline.map_or(due, |d| d.min(due)));
    }

    /// Poll the in-flight worker bake (called once per UI frame): swap a
    /// finished raster into its layer and chain the next bake from whatever
    /// target (drag pending / dirty style scrub) is newest by now.
    pub fn poll_shape_bake(&mut self) {
        let received = match &self.jobs.shape_bake {
            Some(job) => match job.rx.try_recv() {
                Ok(r) => r,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.arm_shape_bake_poll();
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
            },
            None => return,
        };
        // Apply while the job is still `Some` — flush_canvas's interactive
        // gates (defer flatten / partial recomposite) key off it.
        let mut cost = None;
        if let Some((tiles, w, h)) = received {
            let (doc_id, layer_id, data, offset, started) = {
                let j = self.jobs.shape_bake.as_ref().expect("checked above");
                (j.doc_id, j.layer_id, j.data.clone(), j.offset, j.started)
            };
            if self.apply_shape_bake_result(doc_id, layer_id, data, offset, tiles, w, h) {
                cost = Some((layer_id, started.elapsed().as_secs_f32()));
            }
        }
        self.jobs.shape_bake = None;
        if let Some((layer_id, secs)) = cost {
            let now = std::time::Instant::now();
            if let Some(d) = self.edit.shape_drag.as_mut() {
                if d.layer_id == layer_id {
                    d.bake_cost_secs = secs;
                    d.last_bake = Some(now);
                }
            }
            if let Some(p) = self.edit.shape_style_pending.as_mut() {
                if p.layer_id == layer_id {
                    p.bake_cost_secs = secs;
                    p.last_bake = Some(now);
                }
            }
        }
        self.chain_next_shape_bake();
    }

    /// Swap a finished worker raster into its layer. Returns false (dropping
    /// the result) when the target is no longer the active document's layer or
    /// the document turned CMYK (worker tiles carry no ink).
    fn apply_shape_bake_result(
        &mut self,
        doc_id: crate::core::document::DocumentId,
        layer_id: u32,
        data: ShapeData,
        offset: (i32, i32),
        tiles: TileMap,
        w: u32,
        h: u32,
    ) -> bool {
        if self.docs.documents[self.docs.active_doc_idx].id != doc_id {
            return false;
        }
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        if canvas.is_cmyk() {
            return false;
        }
        let Some(idx) = canvas
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == layer_id)
        else {
            return false;
        };
        let (old_off, old_w, old_h) = {
            let l = &canvas.layer_stack.layers[idx];
            if !matches!(
                l.layer_type,
                LayerType::Vector(VectorGeometry::Primitive(_))
            ) {
                return false;
            }
            (l.offset, l.width, l.height)
        };
        let layer = &mut canvas.layer_stack.layers[idx];
        layer.tiles = tiles;
        layer.width = w;
        layer.height = h;
        layer.offset = offset;
        layer.layer_type = LayerType::Vector(VectorGeometry::Primitive(data));
        canvas.layer_revision += 1;
        canvas.mark_dirty_layer_bounds(old_off.0, old_off.1, old_w, old_h);
        canvas.mark_dirty_layer_bounds(offset.0, offset.1, w, h);
        self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
        true
    }

    /// After a worker bake lands, immediately render the newest target: the
    /// drag's pending geometry if it moved on, else a dirty style scrub.
    fn chain_next_shape_bake(&mut self) {
        if let Some(d) = &self.edit.shape_drag {
            let layer_id = d.layer_id;
            if let Some((data, off)) = d.pending.clone() {
                let doc = &self.docs.documents[self.docs.active_doc_idx];
                if doc.canvas.is_cmyk() {
                    return;
                }
                let already = doc
                    .canvas
                    .layer_stack
                    .layers
                    .iter()
                    .find(|l| l.id == layer_id)
                    .is_some_and(|l| {
                        l.offset == off
                            && matches!(
                                &l.layer_type,
                                LayerType::Vector(VectorGeometry::Primitive(cur)) if *cur == data
                            )
                    });
                if !already {
                    let doc_id = doc.id;
                    self.spawn_shape_bake(doc_id, layer_id, data, off);
                }
            }
            return;
        }
        self.flush_pending_shape_style();
    }

    /// Rasterize the drag's pending geometry into the layer and refresh the
    /// screen through the brush-stroke path (dirty rect + partial recomposite,
    /// CPU flatten deferred — NOT the full flatten/upload LayerStructureChanged
    /// does; that full pass ran per mouse-move once and made drags crawl).
    fn shape_drag_bake(&mut self) {
        let (layer_id, target) = match &self.edit.shape_drag {
            Some(d) => (d.layer_id, d.pending.clone()),
            None => return,
        };
        let Some((new_data, new_offset)) = target else {
            return;
        };
        let bake_start = std::time::Instant::now();
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let Some(idx) = canvas
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == layer_id)
        else {
            return;
        };
        let (offset, old_w, old_h) = {
            let layer = &canvas.layer_stack.layers[idx];
            let LayerType::Vector(VectorGeometry::Primitive(d)) = &layer.layer_type else {
                return;
            };
            if *d == new_data && layer.offset == new_offset {
                return; // already baked
            }
            (layer.offset, layer.width, layer.height)
        };
        let Some(raster) = new_data.render() else {
            return;
        };
        let mut tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
        if let Some(conv) = canvas.cmyk_converter() {
            tiles.encode_ink_from_mirror(&conv);
        }
        let layer = &mut canvas.layer_stack.layers[idx];
        layer.tiles = tiles;
        layer.width = raster.width;
        layer.height = raster.height;
        layer.offset = new_offset;
        layer.layer_type = LayerType::Vector(VectorGeometry::Primitive(new_data));
        canvas.layer_revision += 1;
        canvas.mark_dirty_layer_bounds(offset.0, offset.1, old_w, old_h);
        canvas.mark_dirty_layer_bounds(new_offset.0, new_offset.1, raster.width, raster.height);
        self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
        if let Some(d) = self.edit.shape_drag.as_mut() {
            d.bake_cost_secs = bake_start.elapsed().as_secs_f32();
            d.last_bake = Some(std::time::Instant::now());
        }
    }

    /// Apply the Shape tool's current style (fill/stroke colours, stroke width,
    /// corner radius, fill on/off) to the active Shape layer, keeping the
    /// shape's kind and geometry. Called live from the options bar; re-renders
    /// without an undo entry (a lightweight preview edit). Scrubbing a
    /// DragValue emits a tick per frame, so like handle drags the CPU
    /// rasterization is throttled by its own measured cost — this just pins
    /// the target layer and lets `flush_pending_shape_style` decide.
    pub fn update_selected_shape_style(&mut self, apply_fill: bool, apply_stroke: bool) {
        let Some(idx) = self.active_shape_index() else {
            return;
        };
        let doc_id = self.docs.documents[self.docs.active_doc_idx].id;
        let layer_id = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .layers[idx]
            .id;
        match self.edit.shape_style_pending.as_mut() {
            Some(p) if p.doc_id == doc_id && p.layer_id == layer_id => {
                p.dirty = true;
                p.apply_fill |= apply_fill;
                p.apply_stroke |= apply_stroke;
            }
            _ => {
                self.edit.shape_style_pending = Some(crate::app::state::ShapeStylePending {
                    doc_id,
                    layer_id,
                    dirty: true,
                    apply_fill,
                    apply_stroke,
                    last_bake: None,
                    bake_cost_secs: 0.0,
                });
            }
        }
        self.flush_pending_shape_style();
    }

    /// Bake the pending options-bar style. On RGB documents the render runs on
    /// a worker thread (one job at a time; completion chains back here); the
    /// CMYK fallback keeps the synchronous cost throttle. Called every UI
    /// frame (apply_ui_actions) so the final scrub tick always lands even
    /// after the pointer goes idle.
    pub fn flush_pending_shape_style(&mut self) {
        let Some(p) = &self.edit.shape_style_pending else {
            return;
        };
        if !p.dirty {
            return;
        }
        if !self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .is_cmyk()
        {
            if self.jobs.shape_bake.is_some() || self.edit.shape_drag.is_some() {
                return; // the running job's completion chains us
            }
            let (doc_id, layer_id) = (p.doc_id, p.layer_id);
            let Some((idx, new_data, new_offset)) = self.styled_shape_target(doc_id, layer_id)
            else {
                self.edit.shape_style_pending = None;
                return;
            };
            let same = {
                let l = &self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .layer_stack
                    .layers[idx];
                l.offset == new_offset
                    && matches!(
                        &l.layer_type,
                        LayerType::Vector(VectorGeometry::Primitive(cur)) if *cur == new_data
                    )
            };
            if let Some(p) = self.edit.shape_style_pending.as_mut() {
                p.dirty = false; // consumed by the job (re-set by newer ticks)
                p.apply_fill = false;
                p.apply_stroke = false;
            }
            if !same {
                self.spawn_shape_bake(doc_id, layer_id, new_data, new_offset);
            }
            return;
        }
        let wait = (p.bake_cost_secs * 2.0).clamp(0.0, 2.0);
        let remaining = match p.last_bake {
            Some(t) => wait - t.elapsed().as_secs_f32(),
            None => 0.0,
        };
        if remaining <= 0.0 {
            self.shape_style_bake();
        } else {
            let due = std::time::Instant::now() + std::time::Duration::from_secs_f32(remaining);
            self.win.egui_repaint_deadline =
                Some(self.win.egui_repaint_deadline.map_or(due, |d| d.min(due)));
        }
    }

    /// The active Shape layer restyled with the tool's current options-bar
    /// values (geometry and kind kept). `None` drops a stale pin: the layer or
    /// document is no longer the active one.
    fn styled_shape_target(
        &self,
        doc_id: crate::core::document::DocumentId,
        layer_id: u32,
    ) -> Option<(usize, ShapeData, (i32, i32))> {
        let idx = self.active_shape_index()?;
        if self.docs.documents[self.docs.active_doc_idx].id != doc_id
            || self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack
                .layers[idx]
                .id
                != layer_id
        {
            return None;
        }
        let (data, offset) = self.shape_at(idx)?;
        let s = self.edit.tools.shape();
        let pending = self.edit.shape_style_pending.as_ref()?;
        let (x0, y0, x1, y1) = data.canvas_span(offset);
        let mut style = data.style;
        if pending.apply_fill {
            style.fill = if s.fill {
                crate::core::vector::style::Paint::Solid(
                    crate::core::vector::color::ColorValue::from_rgba8(s.fill_color),
                )
            } else {
                crate::core::vector::style::Paint::None
            };
        }
        if pending.apply_stroke {
            style.stroke_style.width = s.stroke_width.max(0.0);
            style.stroke = if s.stroke_width > 0.0 {
                crate::core::vector::style::Paint::Solid(
                    crate::core::vector::color::ColorValue::from_rgba8(s.stroke_color),
                )
            } else {
                crate::core::vector::style::Paint::None
            };
        }
        // Keep the shape's own kind — the options combo only affects new shapes.
        let (mut new_data, new_offset) = ShapeData::from_canvas_span_with_style(
            data.kind,
            x0,
            y0,
            x1,
            y1,
            s.corner_radius,
            style,
        );
        // Sides / inner-radius are live-editable like the corner radius.
        new_data.sides = s.sides.clamp(3, 100);
        new_data.star_inner = s.star_inner.clamp(0.05, 0.95);
        Some((idx, new_data, new_offset))
    }

    /// Re-render the pinned Shape layer with the tool's current style.
    fn shape_style_bake(&mut self) {
        let (doc_id, layer_id) = match &self.edit.shape_style_pending {
            Some(p) => (p.doc_id, p.layer_id),
            None => return,
        };
        // The pin must still point at the active shape layer of the active
        // document — the user may have switched since the scrub. Drop stale
        // pendings rather than styling whatever is active now.
        let Some(idx) = self.active_shape_index() else {
            self.edit.shape_style_pending = None;
            return;
        };
        if self.docs.documents[self.docs.active_doc_idx].id != doc_id
            || self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .layer_stack
                .layers[idx]
                .id
                != layer_id
        {
            self.edit.shape_style_pending = None;
            return;
        }
        let Some((idx, new_data, new_offset)) = self.styled_shape_target(doc_id, layer_id) else {
            self.edit.shape_style_pending = None;
            return;
        };
        let bake_start = std::time::Instant::now();
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let (data, offset, old_w, old_h) = {
            let l = &canvas.layer_stack.layers[idx];
            let LayerType::Vector(VectorGeometry::Primitive(d)) = &l.layer_type else {
                return;
            };
            (d.clone(), l.offset, l.width, l.height)
        };
        // Keep the shape's own kind — the options combo only affects new shapes.
        if new_data == data && new_offset == offset {
            if let Some(p) = self.edit.shape_style_pending.as_mut() {
                p.dirty = false;
                p.apply_fill = false;
                p.apply_stroke = false;
            }
            return;
        }
        let Some(raster) = new_data.render() else {
            // Degenerate size — nothing can apply; stop the retry loop.
            if let Some(p) = self.edit.shape_style_pending.as_mut() {
                p.dirty = false;
                p.apply_fill = false;
                p.apply_stroke = false;
            }
            return;
        };
        let mut tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
        if let Some(conv) = canvas.cmyk_converter() {
            tiles.encode_ink_from_mirror(&conv);
        }
        let layer = &mut canvas.layer_stack.layers[idx];
        layer.tiles = tiles;
        layer.width = raster.width;
        layer.height = raster.height;
        layer.offset = new_offset;
        layer.layer_type = LayerType::Vector(VectorGeometry::Primitive(new_data));
        canvas.layer_revision += 1;
        // Pixel-only change — dirty-rect flatten + recomposite, not the
        // full-canvas flatten/upload of LayerStructureChanged.
        canvas.mark_dirty_layer_bounds(offset.0, offset.1, old_w, old_h);
        canvas.mark_dirty_layer_bounds(new_offset.0, new_offset.1, raster.width, raster.height);
        self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
        if let Some(p) = self.edit.shape_style_pending.as_mut() {
            p.dirty = false;
            p.apply_fill = false;
            p.apply_stroke = false;
            p.bake_cost_secs = bake_start.elapsed().as_secs_f32();
            p.last_bake = Some(std::time::Instant::now());
        }
    }

    /// Finalize the handle drag, pushing an undo entry if geometry changed.
    pub fn shape_drag_finish(&mut self) {
        // Abandon any in-flight worker bake — the final geometry is baked
        // synchronously right here so the undo snapshot is exact (a late
        // worker result would be dropped by its identity checks anyway).
        if self.edit.shape_drag.is_some() {
            self.jobs.shape_bake = None;
        }
        // Bake the last pending geometry (a throttled drag usually ends
        // between bakes) before snapshotting the after-state.
        self.shape_drag_bake();
        let Some(drag) = self.edit.shape_drag.take() else {
            return;
        };
        let (cw, ch) = {
            let d = &self.docs.documents[self.docs.active_doc_idx];
            (d.canvas.width, d.canvas.height)
        };
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        if drag.changed {
            if let Some(mut cmd) = drag.before_cmd {
                cmd.capture_after(&canvas.layer_stack, cw, ch);
                canvas.record(Box::new(cmd));
            }
            // The drag streamed dirty-rect updates in Mode B with the CPU
            // flatten deferred; one full sync now restores Mode A, refreshes
            // the flat mirror, and recomposites from the committed state.
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::Canvas;

    /// App with one Shape layer (a 60×40 primitive at (20,30)) active.
    fn app_with_shape(kind: ShapeKind, radius: f32) -> (App, usize) {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(200, 200);
        let canvas = &mut app.docs.documents[0].canvas;
        let idx = canvas.layer_stack.add_layer(200, 200);
        let data = ShapeData {
            kind,
            x0: 20.0,
            y0: 30.0,
            x1: 80.0,
            y1: 70.0,
            corner_radius: radius,
            sides: 5,
            star_inner: 0.5,
            style: crate::core::vector::style::VectorStyle::from_shape_fields(
                true,
                [200, 40, 40, 255],
                3.0,
                [0, 0, 0, 255],
            ),
        };
        let layer = &mut canvas.layer_stack.layers[idx];
        layer.offset = (0, 0);
        layer.layer_type = LayerType::Vector(VectorGeometry::Primitive(data));
        canvas.layer_stack.active_idx = idx;
        (app, idx)
    }

    #[test]
    fn convert_rectangle_shape_to_editable_path_and_undo() {
        let (mut app, idx) = app_with_shape(ShapeKind::Rectangle, 0.0);
        assert!(app.convert_shape_to_path(idx));
        match &app.docs.documents[0].canvas.layer_stack.layers[idx].layer_type {
            LayerType::Vector(VectorGeometry::Path(o)) => {
                assert_eq!(o.path.contours[0].nodes.len(), 4);
                assert!(o.path.contours[0].closed);
                assert!(o.style.fill.is_visible(), "fill carried over");
            }
            _ => panic!("expected a Path layer after Convert to Curves"),
        }
        // The structural undo restores the Shape layer exactly.
        app.docs.documents[0].canvas.undo().expect("undo");
        assert!(matches!(
            app.docs.documents[0].canvas.layer_stack.layers[idx].layer_type,
            LayerType::Vector(VectorGeometry::Primitive(_))
        ));
    }

    #[test]
    fn convert_rounded_rectangle_yields_eight_nodes() {
        let (mut app, idx) = app_with_shape(ShapeKind::Rectangle, 10.0);
        assert!(app.convert_shape_to_path(idx));
        match &app.docs.documents[0].canvas.layer_stack.layers[idx].layer_type {
            LayerType::Vector(VectorGeometry::Path(o)) => {
                assert_eq!(o.path.contours[0].nodes.len(), 8)
            }
            _ => panic!("expected Path"),
        }
    }

    #[test]
    fn convert_ellipse_and_line_shapes() {
        let (mut app, idx) = app_with_shape(ShapeKind::Ellipse, 0.0);
        assert!(app.convert_shape_to_path(idx));
        match &app.docs.documents[0].canvas.layer_stack.layers[idx].layer_type {
            LayerType::Vector(VectorGeometry::Path(o)) => {
                assert_eq!(o.path.contours[0].nodes.len(), 4);
                assert!(o.path.contours[0].closed);
            }
            _ => panic!("ellipse → closed 4-node path"),
        }

        let (mut app2, idx2) = app_with_shape(ShapeKind::Line, 0.0);
        assert!(app2.convert_shape_to_path(idx2));
        match &app2.docs.documents[0].canvas.layer_stack.layers[idx2].layer_type {
            LayerType::Vector(VectorGeometry::Path(o)) => {
                assert_eq!(o.path.contours[0].nodes.len(), 2);
                assert!(!o.path.contours[0].closed, "line → open path");
            }
            _ => panic!("line → 2-node open path"),
        }
    }

    #[test]
    fn convert_refuses_a_non_shape_layer() {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(100, 100);
        // Layer 0 is the plain raster background — nothing to convert.
        assert!(!app.convert_shape_to_path(0));
    }

    #[test]
    fn geometry_only_shape_edit_preserves_vector_gradient() {
        use crate::core::vector::affine::AffineTransform;
        use crate::core::vector::color::ColorValue;
        use crate::core::vector::style::{Gradient, GradientKind, Paint};

        let (mut app, idx) = app_with_shape(ShapeKind::Rectangle, 0.0);
        let gradient = Gradient::two_color(
            GradientKind::Linear,
            ColorValue::cmyk(0.1, 0.2, 0.3, 0.4),
            ColorValue::rgb(0.9, 0.2, 0.1),
            AffineTransform::translate(20.0, 30.0).then(&AffineTransform::scale(60.0, 1.0)),
        );
        let layer = &mut app.docs.documents[0].canvas.layer_stack.layers[idx];
        let LayerType::Vector(VectorGeometry::Primitive(data)) = &mut layer.layer_type else {
            panic!("expected primitive");
        };
        data.style.fill = Paint::Gradient(gradient);

        app.edit.tools.shape_mut().corner_radius = 12.0;
        let doc_id = app.docs.documents[0].id;
        let layer_id = app.docs.documents[0].canvas.layer_stack.layers[idx].id;
        app.edit.shape_style_pending = Some(crate::app::state::ShapeStylePending {
            doc_id,
            layer_id,
            dirty: true,
            apply_fill: false,
            apply_stroke: false,
            last_bake: None,
            bake_cost_secs: 0.0,
        });
        let (_, target, _) = app
            .styled_shape_target(doc_id, layer_id)
            .expect("geometry target");
        assert_eq!(target.corner_radius, 12.0);
        assert_eq!(target.style.fill, Paint::Gradient(gradient));
    }

    #[test]
    fn convert_gradient_shape_to_curves_preserves_gradient_style() {
        use crate::core::vector::affine::AffineTransform;
        use crate::core::vector::color::ColorValue;
        use crate::core::vector::style::{Gradient, GradientKind, Paint};

        let (mut app, idx) = app_with_shape(ShapeKind::Star, 0.0);
        let gradient = Gradient::two_color(
            GradientKind::Radial,
            ColorValue::rgb(0.1, 0.4, 0.9),
            ColorValue::cmyk(0.0, 0.8, 0.2, 0.1),
            AffineTransform::translate(50.0, 50.0).then(&AffineTransform::scale(30.0, 30.0)),
        );
        let layer = &mut app.docs.documents[0].canvas.layer_stack.layers[idx];
        let LayerType::Vector(VectorGeometry::Primitive(shape)) = &mut layer.layer_type else {
            panic!("expected primitive");
        };
        shape.style.fill = Paint::Gradient(gradient);

        assert!(app.convert_shape_to_path(idx));
        let LayerType::Vector(VectorGeometry::Path(object)) =
            &app.docs.documents[0].canvas.layer_stack.layers[idx].layer_type
        else {
            panic!("expected Path after Convert to Curves");
        };
        assert_eq!(object.style.fill, Paint::Gradient(gradient));
        assert_eq!(object.path.contours[0].nodes.len(), 10);
    }
}

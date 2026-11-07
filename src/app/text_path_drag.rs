//! Dragging text along its path with the Move tool: a small handle sits
//! where the text stands on the path (its `start`); pulling it slides the
//! letters along, as Illustrator's and CorelDRAW's text-on-path handle does.
//! One undo step per drag.

use crate::app::render::CanvasEvent;
use crate::app::state::App;
use crate::core::layer::LayerType;

const HANDLE_HIT_PX: f32 = 9.0;

/// A drag of the handle in flight.
pub struct TextPathDrag {
    idx: usize,
    anchor: (i32, i32),
    cmd: crate::core::command::LayerStructureCommand,
}

impl App {
    /// The active type layer on a path, and its upright origin, while the
    /// Move tool is out and nothing else is transforming.
    fn text_on_path_layer(&self) -> Option<(usize, (i32, i32))> {
        if self.edit.tools.active_id() != crate::tools::ToolId::Move
            || self.edit.transform_state.is_some()
            || self.edit.text_edit.is_some()
        {
            return None;
        }
        let canvas = &self.docs.active().canvas;
        let idx = canvas.layer_stack.active_idx;
        let layer = canvas.layer_stack.layers.get(idx)?;
        match &layer.layer_type {
            LayerType::Text(td) if td.path.is_some() && layer.visible && !layer.locked => {
                Some((idx, iai_canvas::text_follow::text_anchor(layer)?))
            }
            _ => None,
        }
    }

    /// Where the handle is drawn, canvas pixels.
    pub(crate) fn text_path_handle(&self) -> Option<(f32, f32)> {
        let (idx, anchor) = self.text_on_path_layer()?;
        let LayerType::Text(td) = &self.docs.active().canvas.layer_stack.layers[idx].layer_type
        else {
            return None;
        };
        let (x, y) = iai_text::text::path_start_point(td)?;
        Some((x + anchor.0 as f32, y + anchor.1 as f32))
    }

    /// Whether the screen point (sx, sy) is on the handle.
    pub(crate) fn text_path_handle_hit(&self, sx: f32, sy: f32) -> bool {
        let Some((cx, cy)) = self.text_path_handle() else {
            return false;
        };
        let v = &self.edit.view;
        let (hx, hy) = (cx * v.zoom + v.offset_x, cy * v.zoom + v.offset_y);
        (hx - sx).hypot(hy - sy) <= HANDLE_HIT_PX
    }

    pub(crate) fn text_path_drag_begin(&mut self) {
        let Some((idx, anchor)) = self.text_on_path_layer() else {
            return;
        };
        let canvas = &self.docs.active().canvas;
        let cmd = crate::core::command::LayerStructureCommand::capture_before(
            "Move Text Along Path",
            &canvas.layer_stack,
            canvas.width,
            canvas.height,
        );
        self.edit.text_path_drag = Some(TextPathDrag { idx, anchor, cmd });
    }

    pub(crate) fn text_path_dragging(&self) -> bool {
        self.edit.text_path_drag.is_some()
    }

    /// Slide the text so it stands at the path point nearest (cx, cy).
    pub(crate) fn text_path_drag_update(&mut self, cx: f32, cy: f32) {
        let Some((idx, anchor)) = self.edit.text_path_drag.as_ref().map(|d| (d.idx, d.anchor))
        else {
            return;
        };
        let canvas = &mut self.docs.active_mut().canvas;
        let Some(layer) = canvas.layer_stack.layers.get(idx) else {
            return;
        };
        let LayerType::Text(td) = &layer.layer_type else {
            return;
        };
        let (x, y) = (cx - anchor.0 as f32, cy - anchor.1 as f32);
        let Some(start) = iai_text::text::path_start_near(td, x, y) else {
            return;
        };
        if td
            .path
            .as_ref()
            .is_some_and(|p| (p.start - start).abs() < 1e-5)
        {
            return;
        }
        let old = (layer.offset.0, layer.offset.1, layer.width, layer.height);
        let mut td = td.clone();
        if let Some(p) = td.path.as_mut() {
            p.start = start;
        }
        crate::core::canvas::text_layer::rasterize_into_layer(canvas, idx, anchor, td);
        let new = {
            let l = &canvas.layer_stack.layers[idx];
            (l.offset.0, l.offset.1, l.width, l.height)
        };
        canvas.mark_dirty_layer_bounds(old.0, old.1, old.2, old.3);
        canvas.mark_dirty_layer_bounds(new.0, new.1, new.2, new.3);
        self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
    }

    pub(crate) fn text_path_drag_finish(&mut self) {
        let Some(mut drag) = self.edit.text_path_drag.take() else {
            return;
        };
        let canvas = &mut self.docs.active_mut().canvas;
        drag.cmd
            .capture_after(&canvas.layer_stack, canvas.width, canvas.height);
        canvas.record(Box::new(drag.cmd));
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
    }
}

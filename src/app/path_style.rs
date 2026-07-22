//! Fill / Outline styling of the active Path layer (options-bar controls under
//! the Move and Node tools). The vector command [`ChangeVectorStyle`] already
//! exists; this is the App glue that reads the active Path's style for the UI and
//! applies edits through the gateway.
//!
//! Two kinds of edit:
//!   * discrete toggles (Fill on/off, Outline on/off) commit ONE
//!     `ChangeVectorStyle` immediately;
//!   * interactive scrubs (colour dialog drag, outline-width DragValue) preview
//!     live on the model without touching history, capture a baseline at the
//!     start, and commit ONE `ChangeVectorStyle` when the interaction ends —
//!     mirroring the opacity-slider pattern so a scrub is a single undo step.

use crate::app::render::CanvasEvent;
use crate::app::state::App;
use crate::core::layer::LayerType;
use crate::core::vector::color::ColorValue;
use crate::core::vector::object::VectorObjectData;
use crate::core::vector::style::{Paint, VectorStyle};

impl App {
    /// `(layer_id, current style)` of the active editable Path layer, or `None`.
    fn active_path_style(&self) -> Option<(u32, VectorStyle)> {
        let idx = self.active_path_layer()?;
        let layer = &self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .layers[idx];
        match &layer.layer_type {
            LayerType::Path(o) => Some((layer.id, o.style)),
            _ => None,
        }
    }

    /// Fill/outline snapshot for the options-bar controls, or `None` when the
    /// active layer isn't an editable Path.
    pub fn active_path_style_vm(&self) -> Option<crate::ui::PathStyleData> {
        let (_, style) = self.active_path_style()?;
        let color_of = |p: Paint, fallback: [u8; 4]| match p {
            Paint::Solid(c) => c.to_rgba8(),
            Paint::None => fallback,
        };
        Some(crate::ui::PathStyleData {
            fill_enabled: matches!(style.fill, Paint::Solid(_)),
            fill_color: color_of(style.fill, [0, 0, 0, 255]),
            stroke_enabled: matches!(style.stroke, Paint::Solid(_)),
            stroke_color: color_of(style.stroke, [0, 0, 0, 255]),
            stroke_width: style.stroke_style.width,
        })
    }

    /// Re-raster the active Path with `style`, WITHOUT recording history (a live
    /// preview). Dirty-rect invalidation only.
    fn preview_path_style(&mut self, layer_id: u32, style: VectorStyle) {
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let Some(idx) = canvas
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == layer_id)
        else {
            return;
        };
        let (old_off, old_w, old_h, obj) = {
            let layer = &canvas.layer_stack.layers[idx];
            let LayerType::Path(o) = &layer.layer_type else {
                return;
            };
            (layer.offset, layer.width, layer.height, o.clone())
        };
        let new_obj = VectorObjectData { style, ..obj };
        {
            let layer = &mut canvas.layer_stack.layers[idx];
            crate::core::command_vector::apply_object_to_layer(layer, new_obj);
        }
        canvas.layer_revision += 1;
        let (new_off, new_w, new_h) = {
            let l = &canvas.layer_stack.layers[idx];
            (l.offset, l.width, l.height)
        };
        canvas.mark_dirty_layer_bounds(old_off.0, old_off.1, old_w, old_h);
        canvas.mark_dirty_layer_bounds(new_off.0, new_off.1, new_w, new_h);
        self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
    }

    /// Live-preview `style` on the active Path WITHOUT recording history, the way
    /// a colour-dialog drag or width scrub should: the model style is updated
    /// immediately (cheap — so a following read/commit sees the latest), but the
    /// expensive fill re-raster is handed to the OFF-THREAD Path bake. The old
    /// path re-rastered synchronously on the UI thread on every colour tick, which
    /// stalled the picker on a big filled path ("fill màu lag"). Mirrors the
    /// live scale/rotate drag, which bakes off-thread for the same reason.
    fn preview_path_style_live(&mut self, layer_id: u32, style: VectorStyle) {
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let Some(idx) = canvas
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == layer_id)
        else {
            return;
        };
        let obj = {
            let layer = &mut canvas.layer_stack.layers[idx];
            let LayerType::Path(o) = &mut layer.layer_type else {
                return;
            };
            o.style = style;
            o.clone()
        };
        self.request_path_bake(layer_id, obj);
    }

    /// Capture the pre-edit style baseline for the active Path (once per
    /// interaction), so the scrub/dialog commits as one undo step.
    fn path_style_begin(&mut self) {
        let Some((id, style)) = self.active_path_style() else {
            return;
        };
        match self.edit.pending_path_style {
            Some((pid, _)) if pid == id => {}
            _ => self.edit.pending_path_style = Some((id, style)),
        }
    }

    /// Commit an interactive Path style edit: rewind the model to the captured
    /// baseline, then record ONE `ChangeVectorStyle` for the previewed style.
    pub fn path_style_commit(&mut self) {
        let Some((id, baseline)) = self.edit.pending_path_style.take() else {
            return;
        };
        // Drop any in-flight / queued live-preview bake: the final style is
        // rasterised synchronously below, so a late worker result would be stale
        // (same guard the transform commit uses).
        self.cancel_path_bake();
        let Some((cur_id, final_style)) = self.active_path_style() else {
            return;
        };
        if cur_id != id || final_style == baseline {
            // Nothing to record; make sure the model matches the baseline again.
            if cur_id == id && final_style != baseline {
                self.preview_path_style(id, baseline);
            }
            return;
        }
        // Rewind to baseline so the gateway captures the correct "before".
        self.preview_path_style(id, baseline);
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let _ = canvas.execute(
            Box::new(crate::core::command_vector::ChangeVectorStyle::new(
                id,
                final_style,
            )),
            crate::core::gateway::ChangeKind::LayerStructure,
        );
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Apply a discrete style change (Fill/Outline on-off) as ONE undo step.
    fn commit_path_style_change(&mut self, apply: impl FnOnce(&mut VectorStyle)) {
        let Some((id, mut style)) = self.active_path_style() else {
            return;
        };
        apply(&mut style);
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let _ = canvas.execute(
            Box::new(crate::core::command_vector::ChangeVectorStyle::new(
                id, style,
            )),
            crate::core::gateway::ChangeKind::LayerStructure,
        );
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Toggle the Path's fill on/off (discrete, one undo). Enabling uses black
    /// when the fill had no colour to fall back to.
    pub fn path_set_fill_enabled(&mut self, on: bool) {
        self.commit_path_style_change(|s| {
            if on {
                let c = match s.fill {
                    Paint::Solid(c) => c,
                    Paint::None => ColorValue::BLACK,
                };
                s.fill = Paint::Solid(c);
            } else {
                s.fill = Paint::None;
            }
        });
    }

    /// Toggle the Path's outline on/off (discrete, one undo). Enabling picks a
    /// sensible default width when the outline was zero-width.
    pub fn path_set_stroke_enabled(&mut self, on: bool) {
        self.commit_path_style_change(|s| {
            if on {
                let c = match s.stroke {
                    Paint::Solid(c) => c,
                    Paint::None => ColorValue::BLACK,
                };
                s.stroke = Paint::Solid(c);
                if s.stroke_style.width <= 0.0 {
                    s.stroke_style.width = 1.0;
                }
            } else {
                s.stroke = Paint::None;
            }
        });
    }

    /// Live-preview the Path's fill colour during a colour-dialog drag (call
    /// [`Self::path_style_commit`] on OK).
    pub fn path_set_fill_color(&mut self, rgba: [u8; 4]) {
        self.path_style_begin();
        if let Some((id, mut style)) = self.active_path_style() {
            style.fill = Paint::Solid(ColorValue::from_rgba8(rgba));
            self.preview_path_style_live(id, style);
        }
    }

    /// Live-preview the Path's outline colour during a colour-dialog drag.
    pub fn path_set_stroke_color(&mut self, rgba: [u8; 4]) {
        self.path_style_begin();
        if let Some((id, mut style)) = self.active_path_style() {
            style.stroke = Paint::Solid(ColorValue::from_rgba8(rgba));
            if style.stroke_style.width <= 0.0 {
                style.stroke_style.width = 1.0;
            }
            self.preview_path_style_live(id, style);
        }
    }

    /// Live-preview the Path's outline width during a DragValue scrub (call
    /// [`Self::path_style_commit`] when the scrub ends).
    pub fn path_set_stroke_width(&mut self, w: f32) {
        self.path_style_begin();
        if let Some((id, mut style)) = self.active_path_style() {
            style.stroke_style.width = w.clamp(0.0, 500.0);
            // A width scrub implies a visible outline.
            if w > 0.0 && matches!(style.stroke, Paint::None) {
                style.stroke = Paint::Solid(ColorValue::BLACK);
            }
            self.preview_path_style_live(id, style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::Canvas;
    use crate::core::command_vector::CreatePathLayer;
    use crate::core::gateway::ChangeKind;
    use crate::core::geometry::Point;
    use crate::core::vector::affine::AffineTransform;
    use crate::core::vector::path::{Contour, FillRule, Node, PathData};

    fn app_with_path() -> (App, u32) {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(200, 200);
        let path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(40.0, 0.0)),
                    Node::sharp(Point::new(40.0, 40.0)),
                    Node::sharp(Point::new(0.0, 40.0)),
                ],
                true,
            )],
            FillRule::NonZero,
        );
        let obj = VectorObjectData::new(
            path,
            VectorStyle::filled(ColorValue::rgb(1.0, 0.0, 0.0)),
            AffineTransform::translate(50.0, 50.0),
        );
        let id = {
            let canvas = &mut app.docs.documents[0].canvas;
            canvas
                .execute(
                    Box::new(CreatePathLayer::new(obj, "Path 1")),
                    ChangeKind::LayerStructure,
                )
                .unwrap();
            let idx = canvas
                .layer_stack
                .layers
                .iter()
                .position(|l| matches!(l.layer_type, LayerType::Path(_)))
                .unwrap();
            canvas.layer_stack.active_idx = idx;
            canvas.layer_stack.layers[idx].id
        };
        (app, id)
    }

    fn style(app: &App, id: u32) -> VectorStyle {
        match &app.docs.documents[0]
            .canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == id)
            .unwrap()
            .layer_type
        {
            LayerType::Path(o) => o.style,
            _ => panic!("not a path"),
        }
    }

    #[test]
    fn vm_reports_current_fill_and_outline() {
        let (app, _id) = app_with_path();
        let vm = app.active_path_style_vm().unwrap();
        assert!(vm.fill_enabled);
        assert_eq!(vm.fill_color, [255, 0, 0, 255]);
        assert!(!vm.stroke_enabled, "default path has no outline");
    }

    #[test]
    fn set_fill_color_is_one_undo_step() {
        let (mut app, id) = app_with_path();
        // Simulate a colour-dialog interaction: live preview then commit.
        app.path_set_fill_color([0, 128, 255, 255]);
        app.path_style_commit();
        assert_eq!(
            style(&app, id).fill,
            Paint::Solid(ColorValue::from_rgba8([0, 128, 255, 255]))
        );
        app.docs.documents[0].canvas.undo().expect("undo");
        assert_eq!(
            style(&app, id).fill,
            Paint::Solid(ColorValue::rgb(1.0, 0.0, 0.0)),
            "one undo restores the original fill"
        );
    }

    #[test]
    fn toggle_fill_off_and_undo() {
        let (mut app, id) = app_with_path();
        app.path_set_fill_enabled(false);
        assert_eq!(style(&app, id).fill, Paint::None);
        app.docs.documents[0].canvas.undo().expect("undo");
        assert!(matches!(style(&app, id).fill, Paint::Solid(_)));
    }

    #[test]
    fn stroke_width_scrub_commits_once() {
        let (mut app, id) = app_with_path();
        // A scrub: several live previews, then one commit on release.
        app.path_set_stroke_width(3.0);
        app.path_set_stroke_width(6.0);
        app.path_set_stroke_width(8.0);
        app.path_style_commit();
        let s = style(&app, id);
        assert!((s.stroke_style.width - 8.0).abs() < 1e-3);
        assert!(
            matches!(s.stroke, Paint::Solid(_)),
            "width implies an outline"
        );
        // A single undo returns to the original (no outline, default width).
        app.docs.documents[0].canvas.undo().expect("undo");
        assert!(matches!(style(&app, id).stroke, Paint::None));
    }

    #[test]
    fn cancelled_edit_records_nothing() {
        let (mut app, id) = app_with_path();
        let dirty_before = app.docs.documents[0].canvas.is_dirty();
        app.path_set_fill_color([10, 20, 30, 255]); // preview
                                                    // "Cancel": restore original, then commit sees no net change.
        app.path_set_fill_color([255, 0, 0, 255]);
        app.path_style_commit();
        assert_eq!(
            style(&app, id).fill,
            Paint::Solid(ColorValue::rgb(1.0, 0.0, 0.0))
        );
        assert_eq!(
            app.docs.documents[0].canvas.is_dirty(),
            dirty_before,
            "a no-net-change edit adds no history"
        );
    }
}

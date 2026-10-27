// PowerClip (Bước 6, feature layer): place object(s) inside a vector frame so
// they are clipped to its shape, extract them, arrange them in it — the
// `vector.powerclip` command (core: `iai_canvas::powerclip`). The clip re-fits
// automatically when a frame or its content moves — the render path calls
// `Canvas::refresh_clip_masks` on every pixel/structure change (it is
// fingerprint-gated, so idle painting costs nothing). Editing a frame's
// contents in place is a mode of the window and stays here.

use super::render::CanvasEvent;
use super::state::App;
pub use iai_canvas::powerclip::Fit as PowerClipFit;

impl App {
    /// Enable the "Place inside" command.
    pub fn can_powerclip_place(&self) -> bool {
        iai_canvas::powerclip::frame_and_content(&self.docs.active().canvas.layer_stack).is_some()
    }

    /// Enable "Extract" — any selected layer is clip content.
    pub fn can_powerclip_release(&self) -> bool {
        iai_canvas::powerclip::has_selected_content(&self.docs.active().canvas.layer_stack)
    }

    /// Place the selected content object(s) inside the selected vector frame.
    pub fn powerclip_place(&mut self) -> bool {
        self.run_with("vector.powerclip", serde_json::json!({"op": "place"}))
            .is_ok()
    }

    /// Extract the selected clip content from its frame.
    pub fn powerclip_release(&mut self) -> bool {
        self.run_with("vector.powerclip", serde_json::json!({"op": "extract"}))
            .is_ok()
    }

    /// Enable the arrange commands — any selected layer is PowerClip content.
    pub fn can_powerclip_arrange(&self) -> bool {
        self.can_powerclip_release()
    }

    /// Position / scale the selected PowerClip content inside its frame.
    pub fn powerclip_arrange(&mut self, mode: PowerClipFit) -> bool {
        let op = match mode {
            PowerClipFit::Center => "center",
            PowerClipFit::Fit => "fit",
            PowerClipFit::Fill => "fill",
            PowerClipFit::Stretch => "stretch",
        };
        self.run_with("vector.powerclip", serde_json::json!({"op": op}))
            .is_ok()
    }

    /// True while a PowerClip frame's contents are being edited in place.
    pub fn powerclip_editing_contents(&self) -> bool {
        self.docs.active().canvas.powerclip_edit_frame.is_some()
    }

    /// Enable "Edit Contents": a selected frame with content, a selected content
    /// layer, or an edit already in progress (to finish it).
    pub fn can_powerclip_edit_contents(&self) -> bool {
        if self.powerclip_editing_contents() {
            return true;
        }
        let stack = &self.docs.active().canvas.layer_stack;
        stack
            .layers
            .iter()
            .any(|l| l.selected && l.clip_parent_id.is_some())
            || stack.layers.iter().any(|frame| {
                frame.selected
                    && stack
                        .layers
                        .iter()
                        .any(|c| c.clip_parent_id == Some(frame.id))
            })
    }

    /// Toggle editing a PowerClip's contents IN PLACE. Entering unclips the
    /// selected frame's content (drops the managed clip mask and selects it) so it
    /// can be moved / scaled / painted freely against the frame; finishing
    /// re-bakes the clip so the content is clipped again at its new position. The
    /// clip relation and every content edit are preserved throughout; the unclip
    /// is derived state (outside history), like the clip mask itself.
    pub fn powerclip_edit_contents(&mut self) -> bool {
        let canvas = &mut self.docs.active_mut().canvas;

        // Already editing → finish: re-bake the clip and leave the mode.
        if canvas.powerclip_edit_frame.take().is_some() {
            self.edit.powerclip_edit_undo_mark = None;
            let canvas = &mut self.docs.active_mut().canvas;
            canvas.clip_fp = 0;
            canvas.refresh_clip_masks();
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            self.recomposite();
            self.shell.status_msg = "Đã xong sửa nội dung PowerClip".to_string();
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return true;
        }

        // Enter: resolve the frame from the selected content, else a selected frame.
        let frame_id = {
            let stack = &canvas.layer_stack;
            let from_content = stack
                .layers
                .iter()
                .find(|l| l.selected && l.clip_parent_id.is_some())
                .and_then(|l| l.clip_parent_id);
            let from_frame = stack
                .layers
                .iter()
                .find(|frame| {
                    frame.selected
                        && stack
                            .layers
                            .iter()
                            .any(|c| c.clip_parent_id == Some(frame.id))
                })
                .map(|f| f.id);
            from_content.or(from_frame)
        };
        let Some(frame_id) = frame_id else {
            self.shell.status_msg = "Chọn khung PowerClip hoặc nội dung của nó trước".to_string();
            return false;
        };

        canvas.powerclip_edit_frame = Some(frame_id);
        // Remember the undo depth so Cancel can revert every edit made in the
        // session (the mask changes here aren't commands, so this is stable).
        let undo_mark = canvas.undo_count();
        // Unclip every content layer of this frame and select the top one so tools
        // target the content. `refresh_clip_masks` now skips this frame, so the
        // masks stay off until the user finishes.
        let mut top_content = None;
        for (i, layer) in canvas.layer_stack.layers.iter_mut().enumerate() {
            if layer.clip_parent_id == Some(frame_id) {
                layer.mask = None;
                layer.mask_active = false;
                layer.selected = true;
                top_content = Some(i);
            } else {
                layer.selected = false;
            }
        }
        if let Some(i) = top_content {
            canvas.layer_stack.active_idx = i;
        }
        canvas.clip_fp = 0;
        self.edit.powerclip_edit_undo_mark = Some(undo_mark);
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.apply_canvas_event(CanvasEvent::SelectionChanged);
        self.recomposite();
        self.shell.status_msg =
            "Đang sửa nội dung PowerClip — di chuyển/sửa, xong bấm lại để đóng khung".to_string();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        true
    }

    /// Cancel PowerClip Edit-Contents: undo every content edit made during the
    /// session, then re-clip. No-op when not editing.
    pub fn powerclip_cancel_editing(&mut self) -> bool {
        if !self.powerclip_editing_contents() {
            return false;
        }
        self.sync_brush_gpu_to_cpu();
        let mark = self.edit.powerclip_edit_undo_mark.take().unwrap_or(0);
        // Revert every command recorded since editing began.
        while self.docs.active().canvas.undo_count() > mark {
            if self.docs.active_mut().canvas.undo().is_none() {
                break;
            }
        }
        let canvas = &mut self.docs.active_mut().canvas;
        canvas.powerclip_edit_frame = None;
        canvas.clip_fp = 0;
        canvas.refresh_clip_masks();
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.apply_canvas_event(CanvasEvent::SelectionChanged);
        self.recomposite();
        self.shell.status_msg = "Đã huỷ sửa nội dung PowerClip".to_string();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        true
    }

    /// Whether the active layer is currently clipped (Photoshop clipping mask /
    /// PowerClip content). Drives the menu label and shortcut toggle.
    pub fn active_is_clipped(&self) -> bool {
        let stack = &self.docs.active().canvas.layer_stack;
        stack
            .layers
            .get(stack.active_idx)
            .is_some_and(|l| l.clip_parent_id.is_some())
    }

    /// Photoshop-style clipping mask (Ctrl+Alt+G): clip the active layer to the
    /// layer directly below it — or, if that layer is itself clipped, join its
    /// clipping group (clip to the same base). Toggles: a clipped active layer is
    /// released instead. Works the same for vector and pixel layers because the
    /// clip engine (`refresh_clip_masks`) reads the base layer's coverage, whatever
    /// its type. One undo step.
    pub fn toggle_clipping_mask(&mut self) -> bool {
        // Ctrl+Alt+G: the command, on the active layer.
        self.run_now("layer.clip").is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::Canvas;
    use crate::core::layer::LayerType;
    use crate::core::shape::{ShapeData, ShapeKind};
    use crate::core::vector::object::VectorGeometry;
    use crate::core::vector::style::VectorStyle;

    fn rect_shape(x0: f32, y0: f32, x1: f32, y1: f32) -> ShapeData {
        ShapeData {
            kind: ShapeKind::Rectangle,
            x0,
            y0,
            x1,
            y1,
            corner_radius: 0.0,
            corner_type: crate::core::vector::from_shape::RectCorner::Round,
            sides: 5,
            star_inner: 0.5,
            style: VectorStyle::from_shape_fields(true, [40, 120, 220, 255], 0.0, [0, 0, 0, 0]),
        }
    }

    /// App with a full-canvas raster "photo" (bottom) and a rectangle frame on
    /// top, both selected.
    fn app_photo_and_frame() -> App {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(100, 100);
        let canvas = &mut app.docs.documents[0].canvas;

        // Photo: full-canvas raster.
        let photo = canvas.layer_stack.add_layer(100, 100);
        {
            let l = &mut canvas.layer_stack.layers[photo];
            l.tiles = crate::core::tile::TileMap::from_rgba(&vec![200u8; 100 * 100 * 4], 100, 100);
            l.selected = true;
        }
        // Frame: rectangle shape (30,30)-(70,70) on top. Real shape layers carry
        // a rendered raster of their fill; give the frame that opaque 40×40 raster
        // (at offset 30,30) so the clip bake — which reads the frame's coverage —
        // sees the shape.
        let frame = canvas.layer_stack.add_layer(100, 100);
        {
            let l = &mut canvas.layer_stack.layers[frame];
            l.layer_type = LayerType::Vector(VectorGeometry::Primitive(rect_shape(
                30.0, 30.0, 70.0, 70.0,
            )));
            l.tiles = crate::core::tile::TileMap::from_rgba(&vec![255u8; 40 * 40 * 4], 40, 40);
            l.width = 40;
            l.height = 40;
            l.offset = (30, 30);
            l.selected = true;
        }
        app
    }

    #[test]
    fn place_links_content_above_frame_and_clips() {
        let mut app = app_photo_and_frame();
        assert!(app.powerclip_place());
        let canvas = &app.docs.documents[0].canvas;
        // The photo now has a clip parent (the frame) and a baked clip mask.
        let content = canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.clip_parent_id.is_some())
            .expect("content is linked");
        let frame_id = content.clip_parent_id.unwrap();
        let mask = content.mask.as_ref().expect("clip mask baked");
        // Inside the frame rect (50,50) revealed; outside (5,5) hidden.
        assert!(mask.sample(50, 50) > 0.9, "inside frame revealed");
        assert!(mask.sample(5, 5) < 0.1, "outside frame hidden");
        // Content sits directly above its frame.
        let fi = canvas
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == frame_id)
            .unwrap();
        let ci = canvas
            .layer_stack
            .layers
            .iter()
            .position(|l| l.clip_parent_id == Some(frame_id))
            .unwrap();
        assert_eq!(ci, fi + 1, "content directly above frame");
    }

    #[test]
    fn place_then_undo_restores_free_layers() {
        let mut app = app_photo_and_frame();
        let before = app.docs.documents[0].canvas.layer_stack.layers.len();
        assert!(app.powerclip_place());
        app.docs.documents[0].canvas.undo().expect("undo");
        let canvas = &app.docs.documents[0].canvas;
        assert_eq!(canvas.layer_stack.layers.len(), before);
        assert!(
            canvas
                .layer_stack
                .layers
                .iter()
                .all(|l| l.clip_parent_id.is_none()),
            "undo unlinks the clip"
        );
    }

    #[test]
    fn release_frees_the_content() {
        let mut app = app_photo_and_frame();
        assert!(app.powerclip_place());
        // Select the clip content, then extract.
        {
            let stack = &mut app.docs.documents[0].canvas.layer_stack;
            for l in stack.layers.iter_mut() {
                l.selected = l.clip_parent_id.is_some();
            }
        }
        assert!(app.powerclip_release());
        let canvas = &app.docs.documents[0].canvas;
        assert!(
            canvas
                .layer_stack
                .layers
                .iter()
                .all(|l| l.clip_parent_id.is_none()),
            "content released"
        );
        assert!(
            canvas.layer_stack.layers.iter().all(|l| l.mask.is_none()),
            "managed clip mask removed"
        );
    }

    /// After Fit, the 100×100 photo is scaled down to the 40×40 frame and lands
    /// exactly on it (centred). After Center it keeps its size but re-centres.
    #[test]
    fn arrange_fit_scales_content_onto_the_frame() {
        let mut app = app_photo_and_frame();
        assert!(app.powerclip_place());
        // Select the clip content for the arrange.
        {
            let stack = &mut app.docs.documents[0].canvas.layer_stack;
            for l in stack.layers.iter_mut() {
                l.selected = l.clip_parent_id.is_some();
            }
        }
        assert!(app.can_powerclip_arrange());
        assert!(app.powerclip_arrange(PowerClipFit::Fit));

        let content = app.docs.documents[0]
            .canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.clip_parent_id.is_some())
            .expect("content still clipped");
        assert_eq!(
            (content.width, content.height),
            (40, 40),
            "content scaled to the frame size"
        );
        assert_eq!(content.offset, (30, 30), "content sits on the frame");
        assert!(content.mask.is_some(), "clip mask re-baked");
    }

    #[test]
    fn arrange_center_keeps_size_and_recentres() {
        let mut app = app_photo_and_frame();
        assert!(app.powerclip_place());
        {
            let stack = &mut app.docs.documents[0].canvas.layer_stack;
            for l in stack.layers.iter_mut() {
                l.selected = l.clip_parent_id.is_some();
            }
        }
        assert!(app.powerclip_arrange(PowerClipFit::Center));
        let content = app.docs.documents[0]
            .canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.clip_parent_id.is_some())
            .expect("content clipped");
        // 100×100 content centred on the 40×40 frame centred at (50,50).
        assert_eq!((content.width, content.height), (100, 100), "no scaling");
        assert_eq!(content.offset, (0, 0), "centre 50,50 = frame centre");

        // Undo restores the original placement (offset back at 0,0 either way, so
        // check the whole op is one undoable step by counting layers unchanged).
        app.docs.documents[0].canvas.undo().expect("undo arrange");
    }

    #[test]
    fn edit_contents_unclips_then_re_clips_in_place() {
        let mut app = app_photo_and_frame();
        assert!(app.powerclip_place());
        // The placed content is selected + clipped.
        assert!(app.can_powerclip_edit_contents());

        // Enter: content is unclipped (mask dropped) and the mode is active.
        assert!(app.powerclip_edit_contents());
        assert!(app.powerclip_editing_contents());
        {
            let canvas = &app.docs.documents[0].canvas;
            let content = canvas
                .layer_stack
                .layers
                .iter()
                .find(|l| l.clip_parent_id.is_some())
                .expect("still clip content");
            assert!(
                content.mask.is_none(),
                "content shown unclipped while editing"
            );
        }
        // A recomposite-time refresh must NOT re-clip the edited content.
        app.docs.documents[0].canvas.refresh_clip_masks();
        assert!(
            app.docs.documents[0]
                .canvas
                .layer_stack
                .layers
                .iter()
                .find(|l| l.clip_parent_id.is_some())
                .unwrap()
                .mask
                .is_none()
        );

        // Move the content while editing.
        {
            let canvas = &mut app.docs.documents[0].canvas;
            let ci = canvas
                .layer_stack
                .layers
                .iter()
                .position(|l| l.clip_parent_id.is_some())
                .unwrap();
            canvas.layer_stack.layers[ci].offset = (12, 0);
        }

        // Finish: the clip is re-baked at the new position.
        assert!(app.powerclip_edit_contents());
        assert!(!app.powerclip_editing_contents());
        let canvas = &app.docs.documents[0].canvas;
        let content = canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.clip_parent_id.is_some())
            .unwrap();
        assert!(content.mask.is_some(), "content re-clipped after finishing");
    }

    #[test]
    fn cancel_editing_reverts_session_edits_and_re_clips() {
        let mut app = app_photo_and_frame();
        assert!(app.powerclip_place());
        let after_place = app.docs.documents[0].canvas.undo_count();

        assert!(app.powerclip_edit_contents()); // enter
        assert!(app.powerclip_editing_contents());

        // An undoable edit during the session.
        app.docs.documents[0]
            .canvas
            .execute(
                Box::new(crate::core::command::PageSetupCommand::new(
                    "edit",
                    8.0,
                    5.0,
                    Vec::new(),
                )),
                crate::core::gateway::ChangeKind::LayerStructure,
            )
            .expect("edit applies");
        assert!(app.docs.documents[0].canvas.undo_count() > after_place);

        // Cancel reverts the session edit and re-clips.
        assert!(app.powerclip_cancel_editing());
        assert!(!app.powerclip_editing_contents());
        assert_eq!(
            app.docs.documents[0].canvas.undo_count(),
            after_place,
            "session edits undone back to the entry point"
        );
        assert_eq!(
            app.docs.documents[0].canvas.metadata.page_bleed_px, 0.0,
            "the edit was reverted"
        );
        let content = app.docs.documents[0]
            .canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.clip_parent_id.is_some())
            .unwrap();
        assert!(content.mask.is_some(), "content re-clipped after cancel");
    }

    #[test]
    fn right_click_menu_stays_available_after_editing_powerclip() {
        // Regression: after finishing Edit-Contents the selected layer is the
        // (raster) content, not a vector — the object menu must still be offered.
        let mut app = app_photo_and_frame();
        assert!(app.powerclip_place());
        assert!(app.powerclip_edit_contents()); // enter
        assert!(app.has_right_click_object(), "menu available while editing");
        assert!(app.powerclip_edit_contents()); // finish
        assert!(!app.powerclip_editing_contents());
        // The raster content is still selected and is clip content → menu shows.
        assert!(
            app.has_right_click_object(),
            "right-click menu still available on the clipped raster content"
        );
    }

    #[test]
    fn place_needs_a_frame_and_content() {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(50, 50);
        let canvas = &mut app.docs.documents[0].canvas;
        // Only one raster layer selected — no vector frame.
        let idx = canvas.layer_stack.add_layer(50, 50);
        canvas.layer_stack.layers[idx].selected = true;
        assert!(!app.can_powerclip_place());
        assert!(!app.powerclip_place());
    }

    /// App with a rectangle shape (below) and a full-canvas photo on top (active)
    /// — the Ctrl+Alt+G arrangement: clip the top layer to the one below.
    fn app_photo_over_shape() -> App {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(100, 100);
        let canvas = &mut app.docs.documents[0].canvas;
        // Shape frame first (ends up below).
        let frame = canvas.layer_stack.add_layer(100, 100);
        {
            let l = &mut canvas.layer_stack.layers[frame];
            l.layer_type = LayerType::Vector(VectorGeometry::Primitive(rect_shape(
                30.0, 30.0, 70.0, 70.0,
            )));
            l.tiles = crate::core::tile::TileMap::from_rgba(&vec![255u8; 40 * 40 * 4], 40, 40);
            l.width = 40;
            l.height = 40;
            l.offset = (30, 30);
        }
        // Photo on top, active.
        let photo = canvas.layer_stack.add_layer(100, 100);
        {
            let l = &mut canvas.layer_stack.layers[photo];
            l.tiles = crate::core::tile::TileMap::from_rgba(&vec![200u8; 100 * 100 * 4], 100, 100);
        }
        canvas.layer_stack.active_idx = photo;
        app
    }

    #[test]
    fn clipping_mask_clips_active_to_layer_below_and_toggles() {
        let mut app = app_photo_over_shape();
        assert!(app.active_is_clipped() == false);
        assert!(app.toggle_clipping_mask());
        assert!(app.active_is_clipped(), "active is now clipped");
        let canvas = &app.docs.documents[0].canvas;
        let active = &canvas.layer_stack.layers[canvas.layer_stack.active_idx];
        // Clipped to the shape below; masked to its 40×40 area.
        assert!(active.clip_parent_id.is_some());
        let mask = active.mask.as_ref().expect("clip mask baked");
        assert!(mask.sample(50, 50) > 0.9, "inside shape revealed");
        assert!(mask.sample(5, 5) < 0.1, "outside shape hidden");

        // Toggle again → released.
        assert!(app.toggle_clipping_mask());
        assert!(!app.active_is_clipped(), "toggled off");
        let canvas = &app.docs.documents[0].canvas;
        assert!(
            canvas.layer_stack.layers[canvas.layer_stack.active_idx]
                .mask
                .is_none()
        );
    }

    #[test]
    fn clipping_mask_undo_restores_free_layer() {
        let mut app = app_photo_over_shape();
        assert!(app.toggle_clipping_mask());
        app.docs.documents[0].canvas.undo().expect("undo");
        assert!(!app.active_is_clipped(), "undo unlinks the clip");
    }

    #[test]
    fn clipping_mask_needs_a_layer_below() {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(40, 40);
        // Only the background at index 0 is active — nothing below it.
        assert!(!app.toggle_clipping_mask());
    }
}

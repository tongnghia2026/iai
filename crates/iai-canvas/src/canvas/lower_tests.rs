//! Tests of lower crates' code that need the canvas to run on.

mod command_clip {
    use crate::canvas::Canvas;
    use iai_history::command_clip::*;
    use iai_history::gateway::ChangeKind;
    use iai_raster::tile::TileMap;

    #[test]
    fn create_child_and_first_pixels_are_one_undo_step() {
        let mut canvas = Canvas::new(32, 32);
        let frame_idx = canvas.layer_stack.add_layer(32, 32);
        let frame_id = canvas.layer_stack.layers[frame_idx].id;
        let pixels = vec![255; 32 * 32 * 4];
        let tiles = TileMap::from_rgba(&pixels, 32, 32);
        canvas
            .execute(
                Box::new(CreateClippedPixelChild::new(frame_id, 32, 32).with_initial_tiles(tiles)),
                ChangeKind::LayerStructure,
            )
            .unwrap();
        let child = canvas.layer_stack.active_layer();
        assert_eq!(child.clip_parent_id, Some(frame_id));
        assert!(!child.tiles.tiles.is_empty());
        assert!(canvas.undo().is_some());
        assert_eq!(canvas.layer_stack.layers.len(), 2);
        assert!(canvas.redo().is_some());
        assert_eq!(
            canvas.layer_stack.active_layer().clip_parent_id,
            Some(frame_id)
        );
    }

    #[test]
    fn mask_creation_and_release_are_undoable_gateway_commands() {
        let mut canvas = Canvas::new(16, 16);
        let frame_idx = canvas.layer_stack.add_layer(16, 16);
        let frame_id = canvas.layer_stack.layers[frame_idx].id;
        canvas
            .execute(
                Box::new(CreateClippedPixelChild::new(frame_id, 16, 16)),
                ChangeKind::LayerStructure,
            )
            .unwrap();
        let child_id = canvas.layer_stack.active_layer().id;
        canvas
            .execute(
                Box::new(CreateOrAttachRasterMask::new(frame_id, true)),
                ChangeKind::LayerStructure,
            )
            .unwrap();
        assert!(
            canvas
                .layer_stack
                .layers
                .iter()
                .find(|layer| layer.id == frame_id)
                .unwrap()
                .mask
                .is_some()
        );
        canvas
            .execute(
                Box::new(ReleaseClippedChild::new(child_id)),
                ChangeKind::LayerStructure,
            )
            .unwrap();
        assert_eq!(
            canvas
                .layer_stack
                .layers
                .iter()
                .find(|layer| layer.id == child_id)
                .unwrap()
                .clip_parent_id,
            None
        );
        canvas.undo();
        assert_eq!(
            canvas
                .layer_stack
                .layers
                .iter()
                .find(|layer| layer.id == child_id)
                .unwrap()
                .clip_parent_id,
            Some(frame_id)
        );
    }
}

mod command_palette {
    use crate::canvas::Canvas;
    use iai_doc::palette::DocumentSwatch;
    use iai_doc::vector::color::ColorValue;
    use iai_history::command_palette::*;
    use iai_history::gateway::ChangeKind;

    #[test]
    fn palette_command_roundtrips_through_undo_redo() {
        let mut canvas = Canvas::new(20, 20);
        let red = DocumentSwatch::new("Process Red", ColorValue::rgb(1.0, 0.0, 0.0));
        canvas
            .execute(
                Box::new(ReplaceDocumentPalette::new(vec![red.clone()])),
                ChangeKind::DocumentMetadata,
            )
            .unwrap();
        assert_eq!(canvas.metadata.swatches, vec![red.clone()]);
        assert!(canvas.undo().is_some());
        assert!(canvas.metadata.swatches.is_empty());
        assert!(canvas.redo().is_some());
        assert_eq!(canvas.metadata.swatches, vec![red]);
    }
}

mod gateway {
    use crate::canvas::Canvas;
    use iai_history::command::{Command, EditContext};
    use iai_history::gateway::*;

    struct FailingCommand;
    impl Command for FailingCommand {
        fn execute(&mut self, _ctx: &mut EditContext) -> Result<(), String> {
            Err("refused".to_string())
        }
        fn undo(&mut self, _ctx: &mut EditContext) -> Result<(), String> {
            Ok(())
        }
        fn label(&self) -> &str {
            "Failing"
        }
        fn memory_bytes(&self) -> usize {
            1
        }
    }

    fn canvas() -> Canvas {
        Canvas::from_rgba(vec![255; 4 * 4 * 4], 4, 4)
    }

    #[test]
    fn a_failed_command_neither_enters_history_nor_dirties() {
        let mut c = canvas();
        c.mark_saved();

        let err = c
            .execute(Box::new(FailingCommand), ChangeKind::LayerPixels)
            .unwrap_err();

        assert_eq!(err.message, "refused");
        assert_eq!(c.undo_count(), 0, "a failed command must not enter history");
        assert!(
            !c.is_dirty(),
            "a failed command must not dirty the document"
        );
    }

    #[test]
    fn undo_and_redo_report_the_same_invalidation_as_the_edit() {
        let mut c = canvas();
        // A real structural edit through the gateway.
        c.deselect();
        let edit_revision = c.history_revision();

        let undone = c.undo().expect("something to undo");
        assert_eq!(
            undone.composite,
            CompositeInvalidation::Full,
            "undo must invalidate at least as widely as the edit it reverses"
        );
        assert!(undone.layer_structure_changed && undone.selection_changed);
        assert_ne!(
            undone.history_revision, edit_revision,
            "undo must move the history revision so the panel cache refreshes"
        );

        let redone = c.redo().expect("something to redo");
        assert_eq!(redone.composite, undone.composite);
        assert_eq!(
            redone.layer_structure_changed,
            undone.layer_structure_changed
        );
        assert_eq!(redone.selection_changed, undone.selection_changed);
    }

    #[test]
    fn undo_with_empty_history_reports_nothing_to_do() {
        let mut c = canvas();
        assert!(c.undo().is_none());
        assert!(c.redo().is_none());
    }

    #[test]
    fn a_recorded_change_reports_dirty_and_a_fresh_revision() {
        let mut c = canvas();
        c.mark_saved();
        let before = c.history_revision();

        c.deselect();

        assert!(c.is_dirty());
        assert_ne!(c.history_revision(), before);
    }

    #[test]
    fn change_kinds_map_to_their_invalidation_and_all_persist() {
        for (kind, composite) in [
            (ChangeKind::LayerPixels, CompositeInvalidation::Region),
            (ChangeKind::LayerStructure, CompositeInvalidation::Full),
            (ChangeKind::Selection, CompositeInvalidation::None),
            (ChangeKind::DocumentMetadata, CompositeInvalidation::None),
        ] {
            let out = kind.outcome(7, true);
            assert_eq!(out.composite, composite);
            assert_eq!(out.persistence, PersistenceImpact::PersistentDocument);
            assert_eq!(out.history_revision, 7);
            assert!(out.is_dirty);
        }
    }
}

mod command_vector {
    use crate::canvas::Canvas;
    use iai_doc::layer::LayerType;
    use iai_doc::vector::color::ColorValue;
    use iai_doc::vector::object::{VectorGeometry, VectorObjectData};
    use iai_doc::vector::path::PathData;
    use iai_doc::vector::path::{Contour, FillRule, Node};
    use iai_doc::vector::style::VectorStyle;
    use iai_geom::geometry::Point;
    use iai_geom::vector::affine::AffineTransform;
    use iai_history::command::{Command, EditContext};
    use iai_history::gateway::ChangeKind;
    use iai_raster::tile::TileMap;
    use iai_vector::command_vector::*;

    fn square(side: f32) -> PathData {
        PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(side, 0.0)),
                    Node::sharp(Point::new(side, side)),
                    Node::sharp(Point::new(0.0, side)),
                ],
                true,
            )],
            FillRule::NonZero,
        )
    }

    fn obj(side: f32) -> VectorObjectData {
        VectorObjectData::new(
            square(side),
            VectorStyle::filled(ColorValue::rgb(1.0, 0.0, 0.0)),
            AffineTransform::translate(30.0, 30.0),
        )
    }

    fn canvas() -> Canvas {
        Canvas::from_rgba(vec![255; 4 * 100 * 100], 100, 100)
    }

    fn path_layer_count(c: &Canvas) -> usize {
        c.layer_stack
            .layers
            .iter()
            .filter(|l| matches!(l.layer_type, LayerType::Vector(VectorGeometry::Path(_))))
            .count()
    }

    #[test]
    fn create_then_undo_redo_round_trips() {
        let mut c = canvas();
        let before = c.layer_stack.layers.len();
        c.execute(
            Box::new(CreatePathLayer::new(obj(40.0), "Path 1")),
            ChangeKind::LayerStructure,
        )
        .expect("create");
        assert_eq!(path_layer_count(&c), 1);
        assert_eq!(c.layer_stack.layers.len(), before + 1);

        c.undo().expect("undo");
        assert_eq!(path_layer_count(&c), 0);
        assert_eq!(c.layer_stack.layers.len(), before);

        c.redo().expect("redo");
        assert_eq!(path_layer_count(&c), 1);
    }

    #[test]
    fn created_layer_has_raster_and_offset() {
        let mut c = canvas();
        c.execute(
            Box::new(CreatePathLayer::new(obj(40.0), "Path 1")),
            ChangeKind::LayerStructure,
        )
        .expect("create");
        let layer = c
            .layer_stack
            .layers
            .iter()
            .find(|l| matches!(l.layer_type, LayerType::Vector(VectorGeometry::Path(_))))
            .unwrap();
        assert!(!layer.tiles.tiles.is_empty(), "path has a raster cache");
        // Placed near the object's translated origin (30,30) minus the AA pad.
        assert!(layer.offset.0 <= 30 && layer.offset.0 >= 25);
    }

    #[test]
    fn change_style_undo_restores_model() {
        let mut c = canvas();
        let mut cmd = CreatePathLayer::new(obj(40.0), "Path 1");
        cmd.execute(&mut edit_ctx(&mut c)).unwrap();
        let id = cmd.created_id().unwrap();
        c.record(Box::new(cmd));

        let new_style = VectorStyle::stroked(ColorValue::BLACK, 3.0);
        c.execute(
            Box::new(ChangeVectorStyle::new(id, new_style)),
            ChangeKind::LayerStructure,
        )
        .expect("change style");
        assert_eq!(current_style(&c, id), new_style);

        c.undo().expect("undo");
        assert_eq!(current_style(&c, id).fill, obj(40.0).style.fill);
    }

    #[test]
    fn replace_geometry_and_transform_reraster() {
        let mut c = canvas();
        let mut cmd = CreatePathLayer::new(obj(40.0), "Path 1");
        cmd.execute(&mut edit_ctx(&mut c)).unwrap();
        let id = cmd.created_id().unwrap();
        c.record(Box::new(cmd));
        let off_before = current_offset(&c, id);

        // Move the object far right; the raster offset must follow.
        c.execute(
            Box::new(ChangeVectorTransform::new(
                id,
                AffineTransform::translate(70.0, 30.0),
            )),
            ChangeKind::LayerStructure,
        )
        .expect("transform");
        assert!(current_offset(&c, id).0 > off_before.0 + 20);

        c.undo().expect("undo");
        assert_eq!(current_offset(&c, id).0, off_before.0);

        // Replace geometry with a bigger square; validation passes, raster grows.
        let (w_before, _) = current_size(&c, id);
        c.execute(
            Box::new(ReplacePathGeometry::new(id, square(80.0))),
            ChangeKind::LayerStructure,
        )
        .expect("replace");
        assert!(current_size(&c, id).0 > w_before);
    }

    #[test]
    fn delete_then_undo_restores_layer() {
        let mut c = canvas();
        let mut cmd = CreatePathLayer::new(obj(40.0), "Path 1");
        cmd.execute(&mut edit_ctx(&mut c)).unwrap();
        let id = cmd.created_id().unwrap();
        c.record(Box::new(cmd));
        assert_eq!(path_layer_count(&c), 1);

        c.execute(
            Box::new(DeletePathLayer::new(id)),
            ChangeKind::LayerStructure,
        )
        .expect("delete");
        assert_eq!(path_layer_count(&c), 0);

        c.undo().expect("undo");
        assert_eq!(path_layer_count(&c), 1);
    }

    #[test]
    fn command_on_missing_layer_fails_without_history() {
        let mut c = canvas();
        c.mark_saved();
        let err = c
            .execute(
                Box::new(ChangeVectorStyle::new(999, VectorStyle::default())),
                ChangeKind::LayerStructure,
            )
            .unwrap_err();
        assert!(err.message.contains("not found"));
        assert!(!c.is_dirty(), "failed command must not dirty the document");
    }

    #[test]
    fn rasterize_converts_to_raster_and_undo_restores_model() {
        let mut c = canvas();
        let mut cmd = CreatePathLayer::new(obj(40.0), "Path 1");
        cmd.execute(&mut edit_ctx(&mut c)).unwrap();
        let id = cmd.created_id().unwrap();
        c.record(Box::new(cmd));
        let tiles_before = c
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == id)
            .unwrap()
            .tiles
            .tiles
            .len();

        c.execute(
            Box::new(RasterizeVectorLayer::new(id)),
            ChangeKind::LayerStructure,
        )
        .expect("rasterize");
        let l = c.layer_stack.layers.iter().find(|l| l.id == id).unwrap();
        assert!(
            matches!(l.layer_type, LayerType::Raster),
            "now a raster layer"
        );
        // The rendered tiles are kept verbatim (visually a no-op).
        assert_eq!(l.tiles.tiles.len(), tiles_before);

        c.undo().expect("undo");
        assert!(matches!(
            c.layer_stack
                .layers
                .iter()
                .find(|l| l.id == id)
                .unwrap()
                .layer_type,
            LayerType::Vector(VectorGeometry::Path(_))
        ));
    }

    #[test]
    fn path_layer_composites_into_flattened_canvas() {
        // A red path over the white background must show through the compositor:
        // this is the Bước 5 "raster cache feeds the existing compositor" check,
        // exercised on the CPU flatten path (the GPU atlas reads the same tiles).
        let mut c = canvas(); // white 100×100
        c.execute(
            Box::new(CreatePathLayer::new(obj(40.0), "Path 1")),
            ChangeKind::LayerStructure,
        )
        .expect("create");
        let flat = c.export_flat();
        let px = |x: usize, y: usize| {
            let i = (y * 100 + x) * 4;
            [flat[i], flat[i + 1], flat[i + 2]]
        };
        // Centre of the red square (object at translate(30,30), side 40 → ~50,50).
        let cen = px(50, 50);
        assert!(
            cen[0] > 200 && cen[1] < 60 && cen[2] < 60,
            "path fill composited, got {cen:?}"
        );
        // A corner still shows the white background.
        let corner = px(2, 2);
        assert!(
            corner[0] > 240 && corner[1] > 240 && corner[2] > 240,
            "background visible, got {corner:?}"
        );
    }

    #[test]
    fn drag_then_rebuild_preserves_position() {
        let mut c = canvas();
        let mut cmd = CreatePathLayer::new(obj(40.0), "Path 1");
        cmd.execute(&mut edit_ctx(&mut c)).unwrap();
        let id = cmd.created_id().unwrap();
        c.record(Box::new(cmd));
        let off0 = current_offset(&c, id);

        // Simulate a Move-tool drag: shift the layer offset (model untouched).
        c.layer_stack
            .layers
            .iter_mut()
            .find(|l| l.id == id)
            .unwrap()
            .offset = (off0.0 + 25, off0.1 - 10);

        // Reload path: rebuild folds the drag into the model, keeping the position.
        c.rebuild_path_caches();
        assert_eq!(
            current_offset(&c, id),
            (off0.0 + 25, off0.1 - 10),
            "dragged position survives rebuild"
        );
        // Now offset matches the model, so a further rebuild does not drift it.
        c.rebuild_path_caches();
        assert_eq!(
            current_offset(&c, id),
            (off0.0 + 25, off0.1 - 10),
            "fold is idempotent"
        );
    }

    /// A gesture press calls `fold_offset_into_model` on a layer whose offset
    /// already matches its model. That must NOT re-rasterize the cache — the
    /// old double O(area) render here was the first-click lag when rotating or
    /// scaling a page-sized gradient Path.
    #[test]
    fn in_sync_fold_skips_the_area_raster() {
        let mut c = canvas();
        let mut cmd = CreatePathLayer::new(obj(40.0), "Path 1");
        cmd.execute(&mut edit_ctx(&mut c)).unwrap();
        let id = cmd.created_id().unwrap();
        c.record(Box::new(cmd));
        let layer = c
            .layer_stack
            .layers
            .iter_mut()
            .find(|l| l.id == id)
            .unwrap();
        let before = layer.tiles.revision_fingerprint();

        fold_offset_into_model(layer);
        assert_eq!(
            layer.tiles.revision_fingerprint(),
            before,
            "in-sync fold must leave the raster cache untouched"
        );

        // With real drift the fold still runs: position is preserved and the
        // model transform absorbs the delta.
        layer.offset = (layer.offset.0 + 25, layer.offset.1 - 10);
        let dragged = layer.offset;
        fold_offset_into_model(layer);
        assert_eq!(layer.offset, dragged, "dragged position survives the fold");
        let LayerType::Vector(VectorGeometry::Path(o)) = &layer.layer_type else {
            panic!("still a path");
        };
        assert_eq!((o.transform.e, o.transform.f), (30.0 + 25.0, 30.0 - 10.0));
    }

    #[test]
    fn rebuild_path_caches_re_derives_tiles_from_model() {
        let mut c = canvas();
        let mut cmd = CreatePathLayer::new(obj(40.0), "Path 1");
        cmd.execute(&mut edit_ctx(&mut c)).unwrap();
        let id = cmd.created_id().unwrap();
        c.record(Box::new(cmd));

        // Simulate a missing/corrupt cache (e.g. a file with no baked PNG).
        c.layer_stack
            .layers
            .iter_mut()
            .find(|l| l.id == id)
            .unwrap()
            .tiles = TileMap::new(1, 1);

        c.rebuild_path_caches();
        let l = c.layer_stack.layers.iter().find(|l| l.id == id).unwrap();
        assert!(!l.tiles.tiles.is_empty(), "cache re-derived from the model");
    }

    #[test]
    fn expand_stroke_converts_brush_to_closed_path_and_undo_restores() {
        use iai_doc::vector::brush::BrushStroke;
        use iai_doc::vector::style::LineCap;

        // A brush stroke: open two-node centerline + uniform width.
        let centerline = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(40.0, 0.0)),
                ],
                false,
            )],
            FillRule::NonZero,
        );
        let brush_obj = VectorObjectData::new_brush(
            centerline,
            VectorStyle::filled(ColorValue::rgb(0.0, 0.0, 0.0)),
            AffineTransform::translate(20.0, 20.0),
            BrushStroke::uniform(10.0, LineCap::Round),
        );
        let mut c = canvas();
        let mut cmd = CreatePathLayer::new(brush_obj, "Brush 1");
        cmd.execute(&mut edit_ctx(&mut c)).unwrap();
        let id = cmd.created_id().unwrap();
        c.record(Box::new(cmd));

        c.execute(
            Box::new(ExpandVectorStroke::new(id)),
            ChangeKind::LayerStructure,
        )
        .expect("expand");
        // Now a closed, brush-less filled outline.
        let obj = match &c
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == id)
            .unwrap()
            .layer_type
        {
            LayerType::Vector(VectorGeometry::Path(o)) => o.clone(),
            _ => panic!("not a path"),
        };
        assert!(obj.brush.is_none(), "brush appearance is baked away");
        assert!(obj.path.contours[0].closed, "outline is closed");

        c.undo().expect("undo");
        let restored = match &c
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == id)
            .unwrap()
            .layer_type
        {
            LayerType::Vector(VectorGeometry::Path(o)) => o.clone(),
            _ => panic!("not a path"),
        };
        assert!(restored.brush.is_some(), "undo restores the brush stroke");
        assert!(
            !restored.path.contours[0].closed,
            "centerline is open again"
        );
    }

    #[test]
    fn expand_stroke_rejects_non_brush_layer() {
        let mut c = canvas();
        let mut cmd = CreatePathLayer::new(obj(40.0), "Path 1");
        cmd.execute(&mut edit_ctx(&mut c)).unwrap();
        let id = cmd.created_id().unwrap();
        c.record(Box::new(cmd));
        let err = c
            .execute(
                Box::new(ExpandVectorStroke::new(id)),
                ChangeKind::LayerStructure,
            )
            .unwrap_err();
        assert!(err.message.contains("not a brush stroke"));
    }

    // ── helpers ──
    fn edit_ctx(c: &mut Canvas) -> EditContext<'_> {
        EditContext::new(
            &mut c.layer_stack,
            &mut c.width,
            &mut c.height,
            Some(&mut c.selection),
        )
    }
    fn current_style(c: &Canvas, id: u32) -> VectorStyle {
        match &c
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == id)
            .unwrap()
            .layer_type
        {
            LayerType::Vector(VectorGeometry::Path(o)) => o.style,
            _ => panic!("not a path"),
        }
    }
    fn current_offset(c: &Canvas, id: u32) -> (i32, i32) {
        c.layer_stack
            .layers
            .iter()
            .find(|l| l.id == id)
            .unwrap()
            .offset
    }
    fn current_size(c: &Canvas, id: u32) -> (u32, u32) {
        let l = c.layer_stack.layers.iter().find(|l| l.id == id).unwrap();
        (l.width, l.height)
    }
}

mod smart_fill {
    //! The selection fill of a canvas, on the synthetic scene of
    //! core::smart_fill's quality tests.

    fn noise(i: usize, seed: u32) -> f32 {
        let mut x = (i as u32).wrapping_mul(2654435761) ^ seed;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        (x % 1000) as f32 / 1000.0 - 0.5
    }
    /// Sky-blue top half, grass-green bottom half, both with fine noise.
    fn two_backgrounds(w: usize, h: usize) -> Vec<u8> {
        let mut v = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let n = noise(i, 7) * 18.0 + noise(i / 3, 9) * 10.0;
                let c = if y < h / 2 {
                    [110.0 + n, 160.0 + n, 220.0 + n * 0.5]
                } else {
                    [60.0 + n * 1.5, 140.0 + n * 1.5, 50.0 + n]
                };
                for k in 0..3 {
                    v[i * 4 + k] = c[k].clamp(0.0, 255.0) as u8;
                }
                v[i * 4 + 3] = 255;
            }
        }
        v
    }
    /// Paint a red disc of radius `r` and return the hole (disc + 3 px).
    fn paint_disc(img: &mut [u8], w: usize, h: usize, c: (f32, f32), r: f32) -> Vec<bool> {
        let mut hole = vec![false; w * h];
        for y in 0..h {
            for x in 0..w {
                let d = ((x as f32 + 0.5 - c.0).powi(2) + (y as f32 + 0.5 - c.1).powi(2)).sqrt();
                if d < r + 3.0 {
                    hole[y * w + x] = true;
                }
                if d < r {
                    img[(y * w + x) * 4..][..3].copy_from_slice(&[200, 40, 40]);
                }
            }
        }
        hole
    }
    /// Mean |Laplacian| of luma inside the hole: fine-detail energy.
    fn detail(img: &[u8], hole: &[bool], w: usize) -> f64 {
        let l =
            |i: usize| (img[i * 4] as f64 + img[i * 4 + 1] as f64 + img[i * 4 + 2] as f64) / 3.0;
        let (mut acc, mut n) = (0f64, 0f64);
        for (i, &hh) in hole.iter().enumerate() {
            let (x, y) = (i % w, i / w);
            if !hh || x == 0 || y == 0 || x + 1 >= w || i + w >= hole.len() {
                continue;
            }
            acc += (4.0 * l(i) - l(i - 1) - l(i + 1) - l(i - w) - l(i + w)).abs();
            n += 1.0;
        }
        acc / n
    }
    fn rms(img: &[u8], truth: &[u8], hole: &[bool]) -> f64 {
        let (mut e, mut n) = (0f64, 0f64);
        for (i, _) in hole.iter().enumerate().filter(|&(_, &h)| h) {
            for c in 0..3 {
                let d = img[i * 4 + c] as f64 - truth[i * 4 + c] as f64;
                e += d * d;
                n += 1.0;
            }
        }
        (e / n).sqrt()
    }

    /// Needs the local LaMa model; run with `--ignored`: the selection fill
    /// (Edit ▸ Smart Fill) gives the hole the photo's grain, as the Repair
    /// Brush does, not the model's smooth 512 px one.
    #[test]
    #[ignore]
    fn ai_selection_fill_keeps_the_grain() {
        if !iai_ai::lama::is_available() {
            return;
        }
        let (w, h) = (480usize, 400usize);
        let truth = two_backgrounds(w, h);
        let mut img = truth.clone();
        let hole = paint_disc(&mut img, w, h, (240.0, 200.0), 70.0);
        let mut canvas = crate::canvas::Canvas::from_rgba(img, w as u32, h as u32);
        assert!(canvas.smart_fill_fill_hole(hole.clone(), true, false));
        let filled = canvas.active_layer().tiles.flatten();
        let (got, want) = (detail(&filled, &hole, w), detail(&truth, &hole, w));
        println!(
            "rms={:.1} detail={got:.1} (truth {want:.1})",
            rms(&filled, &truth, &hole)
        );
        assert!(got > want * 0.6, "{got} vs {want}");
    }
}

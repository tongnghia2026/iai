//! Canvas geometry: crop (straight/rotated/transformed/perspective), flips,
//! 90-degree rotates, canvas/image resize and the tile resampling behind them.
//! Every size change goes through the app's apply_canvas_event path.

// Main document model — Canvas, History, metadata.
//

use super::*;
use crate::core::gateway::ChangeKind;
use crate::core::layer::{Layer, LayerType};
use crate::core::tile::TileMap;
use crate::core::vector::affine::AffineTransform;
use crate::core::vector::object::VectorGeometry;

/// Translate editable geometry after an axis-aligned crop without baking it into
/// a canvas-sized raster. Text and primitive shapes store placement in the layer
/// offset; Path objects store it in their source-of-truth object transform.
fn translate_editable_layer(layer: &mut Layer, dx: i32, dy: i32) -> bool {
    match layer.layer_type.clone() {
        LayerType::Text(_) | LayerType::Vector(VectorGeometry::Primitive(_)) => {
            layer.offset.0 = layer.offset.0.saturating_add(dx);
            layer.offset.1 = layer.offset.1.saturating_add(dy);
            true
        }
        LayerType::Vector(VectorGeometry::Path(mut object)) => {
            object.transform =
                AffineTransform::translate(dx as f32, dy as f32).then(&object.transform);
            crate::core::command_vector::apply_object_to_layer(layer, object);
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod crop_regression_tests {
    use super::*;
    use crate::core::geometry::Point;
    use crate::core::shape::{ShapeData, ShapeKind};
    use crate::core::text::{rasterize_placed, TextData};
    use crate::core::vector::color::ColorValue;
    use crate::core::vector::object::VectorObjectData;
    use crate::core::vector::path::{Contour, FillRule, Node, PathData};
    use crate::core::vector::raster::raster_geometry;
    use crate::core::vector::style::VectorStyle;

    fn square_object(at: (f32, f32)) -> VectorObjectData {
        let path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(30.0, 0.0)),
                    Node::sharp(Point::new(30.0, 30.0)),
                    Node::sharp(Point::new(0.0, 30.0)),
                ],
                true,
            )],
            FillRule::NonZero,
        );
        VectorObjectData::new(
            path,
            VectorStyle::filled(ColorValue::rgb(1.0, 0.0, 0.0)),
            AffineTransform::translate(at.0, at.1),
        )
    }

    fn set_text_layer(layer: &mut Layer, origin: (i32, i32)) {
        let td = TextData {
            content: "Crop regression".to_string(),
            font_px: 24.0,
            ..TextData::default()
        };
        let (raster, delta) = rasterize_placed(&td).expect("text raster");
        layer.tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
        layer.width = raster.width;
        layer.height = raster.height;
        layer.offset = (origin.0 + delta.0, origin.1 + delta.1);
        layer.layer_type = LayerType::Text(td);
    }

    #[test]
    fn straight_crop_keeps_empty_layers_sparse() {
        let mut canvas = Canvas::new(257, 257);
        for _ in 1..1000 {
            canvas.layer_stack.add_layer(257, 257);
        }
        assert_eq!(
            canvas
                .layer_stack
                .layers
                .iter()
                .map(|layer| layer.tiles.tiles.len())
                .sum::<usize>(),
            4
        );

        assert!(canvas.crop(0, 0, 257, 257, true));
        assert_eq!(canvas.layer_stack.layers.len(), 1000);
        assert_eq!(
            canvas
                .layer_stack
                .layers
                .iter()
                .map(|layer| layer.tiles.tiles.len())
                .sum::<usize>(),
            4,
            "transparent layers must not become full-canvas tile maps"
        );
    }

    #[test]
    fn transformed_crop_skips_empty_layer_resampling() {
        let mut canvas = Canvas::new(257, 257);
        for _ in 1..1000 {
            canvas.layer_stack.add_layer(257, 257);
        }
        assert!(canvas.crop_transformed(128.5, 128.5, 257.0, 257.0, 257, 257, 0.0, 0.0, 0.0, true,));
        assert_eq!(canvas.layer_stack.layers.len(), 1000);
        assert_eq!(
            canvas
                .layer_stack
                .layers
                .iter()
                .skip(1)
                .map(|layer| layer.tiles.tiles.len())
                .sum::<usize>(),
            0
        );
    }

    #[test]
    fn straight_crop_translates_path_model_and_cache_together() {
        let mut canvas = Canvas::new(256, 256);
        crate::core::command_vector::apply_object_to_layer(
            &mut canvas.layer_stack.layers[0],
            square_object((100.0, 80.0)),
        );

        assert!(canvas.crop(64, 32, 128, 128, true));
        let layer = &canvas.layer_stack.layers[0];
        let LayerType::Vector(VectorGeometry::Path(object)) = &layer.layer_type else {
            panic!("path stopped being editable");
        };
        let (origin, width, height) = raster_geometry(object).expect("path geometry");
        assert_eq!(layer.offset, origin);
        assert_eq!((layer.width, layer.height), (width, height));
        let bounds = layer.tiles.content_bounds().expect("path pixels");
        assert!((layer.offset.0 + bounds.0 - 36).abs() <= 1);
        assert!((layer.offset.1 + bounds.1 - 48).abs() <= 1);

        let mut rebuilt = layer.clone();
        crate::core::command_vector::fold_offset_into_model(&mut rebuilt);
        assert_eq!(rebuilt.offset, layer.offset);
        assert_eq!(rebuilt.tiles.content_bounds(), layer.tiles.content_bounds());
        assert_eq!(rebuilt.layer_type, layer.layer_type);

        canvas.undo().expect("undo crop");
        let LayerType::Vector(VectorGeometry::Path(object)) =
            &canvas.layer_stack.layers[0].layer_type
        else {
            panic!("undo lost path model");
        };
        assert_eq!(raster_geometry(object).unwrap().0, (99, 79));
        canvas.redo().expect("redo crop");
        let layer = &canvas.layer_stack.layers[0];
        let LayerType::Vector(VectorGeometry::Path(object)) = &layer.layer_type else {
            panic!("redo lost path model");
        };
        assert_eq!(layer.offset, raster_geometry(object).unwrap().0);
    }

    #[test]
    fn straight_crop_keeps_text_and_primitive_editable_and_compact() {
        let mut canvas = Canvas::new(256, 256);
        set_text_layer(&mut canvas.layer_stack.layers[0], (100, 80));
        let text_before = canvas.layer_stack.layers[0].clone();

        let idx = canvas.layer_stack.add_layer(256, 256);
        let (shape, offset) = ShapeData::from_canvas_span(
            ShapeKind::Rectangle,
            120.0,
            90.0,
            160.0,
            130.0,
            0.0,
            true,
            [0, 0, 255, 255],
            0.0,
            [0, 0, 0, 0],
        );
        let raster = shape.render().expect("shape raster");
        let shape_layer = &mut canvas.layer_stack.layers[idx];
        shape_layer.tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
        shape_layer.width = raster.width;
        shape_layer.height = raster.height;
        shape_layer.offset = offset;
        shape_layer.layer_type = LayerType::Vector(VectorGeometry::Primitive(shape));
        let shape_before = shape_layer.clone();

        assert!(canvas.crop(64, 32, 128, 128, true));
        let text = &canvas.layer_stack.layers[0];
        assert!(matches!(text.layer_type, LayerType::Text(_)));
        assert_eq!(
            text.offset,
            (text_before.offset.0 - 64, text_before.offset.1 - 32)
        );
        assert_eq!(
            (text.width, text.height),
            (text_before.width, text_before.height)
        );

        let shape = &canvas.layer_stack.layers[idx];
        assert!(matches!(
            shape.layer_type,
            LayerType::Vector(VectorGeometry::Primitive(_))
        ));
        assert_eq!(
            shape.offset,
            (shape_before.offset.0 - 64, shape_before.offset.1 - 32)
        );
        assert_eq!(
            (shape.width, shape.height),
            (shape_before.width, shape_before.height)
        );
    }

    #[test]
    fn affine_crop_reconciles_path_and_text_models() {
        let mut canvas = Canvas::new(256, 256);
        crate::core::command_vector::apply_object_to_layer(
            &mut canvas.layer_stack.layers[0],
            square_object((100.0, 80.0)),
        );
        let text_idx = canvas.layer_stack.add_layer(256, 256);
        set_text_layer(&mut canvas.layer_stack.layers[text_idx], (110, 100));

        assert!(canvas.crop_transformed(128.0, 128.0, 128.0, 128.0, 128, 128, 0.0, 0.0, 0.0, true,));
        let path = &canvas.layer_stack.layers[0];
        let LayerType::Vector(VectorGeometry::Path(object)) = &path.layer_type else {
            panic!("affine crop rasterized a path");
        };
        assert_eq!(path.offset, raster_geometry(object).unwrap().0);
        let path_bounds = path.tiles.content_bounds().expect("affine path pixels");
        assert!((path.offset.0 + path_bounds.0 - 36).abs() <= 1);
        assert!((path.offset.1 + path_bounds.1 - 16).abs() <= 1);
        let text = &canvas.layer_stack.layers[text_idx];
        let LayerType::Text(td) = &text.layer_type else {
            panic!("axis-aligned affine crop rasterized text");
        };
        let text_origin = text_origin_for_layer(td, text);
        assert!((text_origin.0 - 46).abs() <= 1);
        assert!((text_origin.1 - 36).abs() <= 1);
    }

    #[test]
    fn rotated_crop_updates_path_model_in_output_coordinates() {
        let mut canvas = Canvas::new(256, 256);
        crate::core::command_vector::apply_object_to_layer(
            &mut canvas.layer_stack.layers[0],
            square_object((100.0, 80.0)),
        );
        assert!(canvas.crop_rotated(
            128.0,
            128.0,
            128.0,
            128.0,
            128,
            128,
            std::f32::consts::FRAC_PI_2,
            true,
        ));
        let layer = &canvas.layer_stack.layers[0];
        let LayerType::Vector(VectorGeometry::Path(object)) = &layer.layer_type else {
            panic!("rotated crop rasterized a path");
        };
        assert_eq!(layer.offset, raster_geometry(object).unwrap().0);
        let bounds = layer.tiles.content_bounds().expect("rotated path pixels");
        let canvas_bounds = (
            layer.offset.0 + bounds.0,
            layer.offset.1 + bounds.1,
            layer.offset.0 + bounds.2,
            layer.offset.1 + bounds.3,
        );
        assert!((canvas_bounds.0 - 16).abs() <= 1, "{canvas_bounds:?}");
        assert!((canvas_bounds.1 - 62).abs() <= 1, "{canvas_bounds:?}");
    }

    #[test]
    fn perspective_crop_explicitly_rasterizes_unrepresentable_models() {
        let mut canvas = Canvas::new(128, 128);
        crate::core::command_vector::apply_object_to_layer(
            &mut canvas.layer_stack.layers[0],
            square_object((30.0, 30.0)),
        );
        canvas
            .convert_to_cmyk(CmykProfile::Naive)
            .expect("CMYK conversion");
        assert!(canvas.crop_perspective(
            [(0.0, 0.0), (127.0, 4.0), (124.0, 127.0), (3.0, 124.0)],
            128,
            128,
            true,
        ));
        assert!(matches!(
            canvas.layer_stack.layers[0].layer_type,
            LayerType::Raster
        ));
        assert!(canvas.layer_stack.layers[0]
            .tiles
            .content_bounds()
            .is_some());
        assert!(
            canvas.layer_stack.layers[0].tiles.has_any_ink(),
            "projective rasterization must retain CMYK separations"
        );
    }
}

/// Recover the upright text anchor from the current derived raster. This is the
/// core equivalent of the Type tool's edit-origin recovery and lets a crop
/// transform the editable text model rather than only its cached pixels.
fn text_origin_for_layer(td: &crate::core::text::TextData, layer: &Layer) -> (i32, i32) {
    let Some((placed, delta)) = crate::core::text::rasterize_placed(td) else {
        return layer.offset;
    };
    let placed_tiles = TileMap::from_rgba(&placed.rgba, placed.width, placed.height);
    let Some((placed_min_x, placed_min_y, _, _)) = placed_tiles.content_bounds() else {
        return layer.offset;
    };
    let Some((layer_min_x, layer_min_y, _, _)) = layer.tiles.content_bounds() else {
        return layer.offset;
    };
    (
        layer
            .offset
            .0
            .saturating_add(layer_min_x)
            .saturating_sub(delta.0)
            .saturating_sub(placed_min_x),
        layer
            .offset
            .1
            .saturating_add(layer_min_y)
            .saturating_sub(delta.1)
            .saturating_sub(placed_min_y),
    )
}

/// Apply an affine canvas transform to editable text when its resulting basis is
/// still representable by TextData (rotation + horizontal stretch + font scale).
/// Anisotropic scale around already-rotated text can introduce shear; that case
/// returns false so the caller explicitly keeps the exact resampled raster.
fn apply_affine_to_text_layer(
    layer: &mut Layer,
    source: &Layer,
    td: &crate::core::text::TextData,
    transform: AffineTransform,
) -> bool {
    let angle = td.rotation_deg.to_radians();
    let (sin, cos) = angle.sin_cos();
    let fx = if td.flip_x { -1.0 } else { 1.0 };
    let fy = if td.flip_y { -1.0 } else { 1.0 };
    let stretch = td.stretch_x.max(0.001);

    // Columns of R(rotation) * Flip * Scale(stretch_x, 1), transformed by the
    // crop's linear part.
    let x0 = cos * fx * stretch;
    let y0 = sin * fx * stretch;
    let x1 = -sin * fy;
    let y1 = cos * fy;
    let nx0 = transform.a * x0 + transform.c * y0;
    let ny0 = transform.b * x0 + transform.d * y0;
    let nx1 = transform.a * x1 + transform.c * y1;
    let ny1 = transform.b * x1 + transform.d * y1;
    let sx = nx0.hypot(ny0);
    let sy = nx1.hypot(ny1);
    if !sx.is_finite() || !sy.is_finite() || sx <= 1e-5 || sy <= 1e-5 {
        return false;
    }
    let orthogonality = (nx0 * nx1 + ny0 * ny1).abs() / (sx * sy);
    if orthogonality > 0.002 {
        return false;
    }

    let mut next = td.clone();
    next.rotation_deg = ((ny0 * fx).atan2(nx0 * fx).to_degrees()).rem_euclid(360.0);
    next.font_px = (next.font_px * sy).clamp(4.0, 1600.0);
    next.tracking_px = (next.tracking_px * sy).clamp(-200.0, 500.0);
    next.stretch_x = (sx / sy).clamp(0.01, 100.0);
    for glyph in &mut next.glyph_styles {
        glyph.font_px = (glyph.font_px * sy).clamp(4.0, 1600.0);
    }

    let origin = text_origin_for_layer(td, source);
    let mapped = transform.apply_point(crate::core::geometry::Point::new(
        origin.0 as f32,
        origin.1 as f32,
    ));
    let mapped_origin = (mapped.x.round() as i32, mapped.y.round() as i32);
    let Some((raster, delta)) = crate::core::text::rasterize_placed(&next) else {
        return false;
    };
    layer.tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
    layer.width = raster.width;
    layer.height = raster.height;
    layer.offset = (
        mapped_origin.0.saturating_add(delta.0),
        mapped_origin.1.saturating_add(delta.1),
    );
    layer.layer_type = LayerType::Text(next);
    true
}

/// Reconcile an affine crop's resampled cache with the editable layer model.
/// A mask is tied to the old raster's local coordinate system, so masked editable
/// layers intentionally keep the exact rasterized result; unmasked layers can be
/// rebuilt compactly and remain editable.
fn reconcile_affine_layer_model(layer: &mut Layer, source: &Layer, transform: AffineTransform) {
    if source.mask.is_some() {
        if matches!(source.layer_type, LayerType::Text(_) | LayerType::Vector(_)) {
            layer.layer_type = LayerType::Raster;
        }
        return;
    }

    match source.layer_type.clone() {
        LayerType::Vector(VectorGeometry::Path(mut object)) => {
            object.transform = transform.then(&object.transform);
            let scale = transform.determinant().abs().sqrt();
            object.style.stroke_style.width = (object.style.stroke_style.width * scale).max(0.0);
            crate::core::command_vector::apply_object_to_layer(layer, object);
        }
        LayerType::Vector(VectorGeometry::Primitive(shape)) => {
            let mut object = shape.to_vector_object(source.offset);
            object.transform = transform.then(&object.transform);
            let scale = transform.determinant().abs().sqrt();
            object.style.stroke_style.width = (object.style.stroke_style.width * scale).max(0.0);
            crate::core::command_vector::apply_object_to_layer(layer, object);
        }
        LayerType::Text(td) => {
            if !apply_affine_to_text_layer(layer, source, &td, transform) {
                layer.layer_type = LayerType::Raster;
            }
        }
        _ => {}
    }
}

/// Perspective geometry cannot be represented by the affine-only vector/text
/// models. Keep the exact resampled result and make that conversion explicit so
/// no later model edit can silently replace it with stale pre-crop geometry.
fn rasterize_projective_layer_model(layer: &mut Layer) {
    if matches!(layer.layer_type, LayerType::Text(_) | LayerType::Vector(_)) {
        layer.layer_type = LayerType::Raster;
    }
}

/// Crop resampling rebuilds RGB mirrors for every layer kind, not only Paths.
/// Restore CMYK ink planes before the history snapshot so crop/undo/export keep
/// separations valid for raster, text and explicitly rasterized vector layers.
fn reconcile_crop_ink(canvas: &mut Canvas) {
    if !canvas.is_cmyk() {
        return;
    }
    let Some(converter) = canvas.cmyk_converter() else {
        return;
    };
    for layer in &mut canvas.layer_stack.layers {
        if layer.tiles.needs_ink_encode() {
            layer.tiles.encode_ink_from_mirror(&converter);
        }
    }
}

impl Canvas {
    pub(crate) fn add_crop_background_if_missing(
        &mut self,
        width: u32,
        height: u32,
        color: [u8; 4],
    ) {
        if self
            .layer_stack
            .layers
            .iter()
            .any(|layer| layer.is_background)
        {
            return;
        }

        let mut id = self.layer_stack.next_id();
        while self.layer_stack.layers.iter().any(|layer| layer.id == id) {
            id = id.saturating_add(1);
        }
        self.layer_stack.set_next_id(id.saturating_add(1));

        let [r, g, b, a] = color;
        let mut layer = crate::core::layer::Layer::new(id, "Background", width, height);
        layer.tiles = crate::core::tile::TileMap::new_solid(width, height, r, g, b, a);
        layer.is_background = true;
        layer.locked = true;
        self.layer_stack.layers.insert(0, layer);
        self.layer_stack.active_idx = self.layer_stack.active_idx.saturating_add(1);
    }

    pub fn crop(&mut self, x: i32, y: i32, w: u32, h: u32, delete_cropped: bool) -> bool {
        self.crop_impl(x, y, w, h, delete_cropped, None)
    }

    pub fn crop_with_background(
        &mut self,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        delete_cropped: bool,
        background: [u8; 4],
    ) -> bool {
        self.crop_impl(x, y, w, h, delete_cropped, Some(background))
    }

    pub(crate) fn crop_impl(
        &mut self,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        delete_cropped: bool,
        background: Option<[u8; 4]>,
    ) -> bool {
        let max = MAX_DIMENSION;
        if w == 0 || h == 0 || w > max || h > max {
            return false;
        }
        let expands_canvas = x < 0
            || y < 0
            || x as i64 + w as i64 > self.width as i64
            || y as i64 + h as i64 > self.height as i64;

        let mut cmd = crate::core::command::LayerStructureCommand::capture_before(
            "Crop",
            &self.layer_stack,
            self.width,
            self.height,
        );

        for layer in &mut self.layer_stack.layers {
            // Editable geometry stays compact and is translated in its own
            // source model. Cropping its raster to a canvas-sized cache would
            // both destroy sparsity and leave the model at the old coordinates.
            if translate_editable_layer(layer, -x, -y) {
                continue;
            }
            let ox = layer.offset.0;
            let oy = layer.offset.1;

            if delete_cropped {
                let layer_fill = background.filter(|_| layer.is_background);
                let (new_tiles, new_w, new_h) = Self::crop_tilemap(
                    &layer.tiles,
                    layer.width,
                    layer.height,
                    x,
                    y,
                    w,
                    h,
                    ox,
                    oy,
                    layer_fill,
                );
                layer.tiles = new_tiles;
                layer.width = new_w;
                layer.height = new_h;
                layer.offset = (0, 0);

                if let Some(mask) = &mut layer.mask {
                    let (mt, mw, mh) = Self::crop_tilemap(
                        &mask.tiles,
                        mask.width,
                        mask.height,
                        x,
                        y,
                        w,
                        h,
                        ox,
                        oy,
                        background.map(|_| [255, 255, 255, 255]),
                    );
                    mask.tiles = mt;
                    mask.width = mw;
                    mask.height = mh;
                }
            } else {
                layer.offset = (ox - x, oy - y);
            }
        }

        if expands_canvas {
            if let Some(color) = background {
                self.add_crop_background_if_missing(w, h, color);
            }
        }

        self.width = w;
        self.height = h;
        self.selection.resize(w, h);
        reconcile_crop_ink(self);
        cmd.capture_after(&self.layer_stack, self.width, self.height);
        self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
        self.flatten_full();
        true
    }

    /// Rotated crop: extract a `(w × h)` rectangle centered at `(cx, cy)` that
    /// is tilted by `angle_rad` radians (CW positive, same sign as `CropTool::rotation`).
    ///
    /// Each output pixel `(u, v)` is filled by back-projecting the un-rotated offset
    /// `(u - w/2, v - h/2)` through a CW rotation by `-angle_rad` to obtain the
    /// source canvas position, then sampling with bilinear interpolation.
    ///
    /// A `LayerStructureCommand` is pushed for undo.
    pub fn crop_rotated(
        &mut self,
        cx: f32,
        cy: f32,
        src_w: f32,
        src_h: f32,
        out_w: u32,
        out_h: u32,
        angle_rad: f32,
        _delete_cropped: bool,
    ) -> bool {
        use crate::core::command::LayerStructureCommand;

        if out_w == 0 || out_h == 0 {
            return false;
        }
        let max = MAX_DIMENSION;
        if out_w > max || out_h > max {
            return false;
        }
        let mut cmd = LayerStructureCommand::capture_before(
            "Crop/Resample",
            &self.layer_stack,
            self.width,
            self.height,
        );

        let scale_x = src_w / out_w as f32;
        let scale_y = src_h / out_h as f32;

        let hw = out_w as f32 * 0.5;
        let hh = out_h as f32 * 0.5;

        let cos_rot = angle_rad.cos();
        let sin_rot = angle_rad.sin();
        let forward = AffineTransform::translate(hw, hh)
            .then(&AffineTransform::scale(
                1.0 / scale_x.max(f32::EPSILON),
                1.0 / scale_y.max(f32::EPSILON),
            ))
            .then(&AffineTransform::rotate(-angle_rad))
            .then(&AffineTransform::translate(-cx, -cy));

        for layer in &mut self.layer_stack.layers {
            let source = layer.clone();
            let ox = layer.offset.0 as f32;
            let oy = layer.offset.1 as f32;
            // Back-project each output-pixel centre through a CW rotation by
            // `-angle_rad` to the source; tile-native chunked resample so rotated
            // Crop works under Viewport Streaming.
            let map = |u: f32, v: f32| -> (f32, f32) {
                let lx = (u - hw) * scale_x;
                let ly = (v - hh) * scale_y;
                let src_cx = lx * cos_rot - ly * sin_rot + cx;
                let src_cy = lx * sin_rot + ly * cos_rot + cy;
                (src_cx - ox, src_cy - oy)
            };
            layer.tiles = Self::resample_into_tiles(&layer.tiles, out_w, out_h, &map, None);
            layer.width = out_w;
            layer.height = out_h;
            layer.offset = (0, 0);

            if let Some(mask) = &mut layer.mask {
                mask.tiles = Self::resample_into_tiles(&mask.tiles, out_w, out_h, &map, None);
                mask.width = out_w;
                mask.height = out_h;
            }
            reconcile_affine_layer_model(layer, &source, forward);
        }

        self.width = out_w;
        self.height = out_h;
        self.selection.resize(out_w, out_h);
        reconcile_crop_ink(self);
        cmd.capture_after(&self.layer_stack, self.width, self.height);
        self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
        self.flatten_full();
        true
    }

    /// Crop a fixed viewport while the image underneath has been preview-transformed.
    /// This is the modern Crop-tool model: the crop box is an axis-aligned viewport,
    /// while the source image can be panned/rotated behind it before commit.
    pub fn crop_transformed(
        &mut self,
        cx: f32,
        cy: f32,
        viewport_w: f32,
        viewport_h: f32,
        out_w: u32,
        out_h: u32,
        image_tx: f32,
        image_ty: f32,
        angle_rad: f32,
        delete_cropped: bool,
    ) -> bool {
        self.crop_transformed_impl(
            cx,
            cy,
            viewport_w,
            viewport_h,
            out_w,
            out_h,
            image_tx,
            image_ty,
            angle_rad,
            delete_cropped,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn crop_transformed_with_background(
        &mut self,
        cx: f32,
        cy: f32,
        viewport_w: f32,
        viewport_h: f32,
        out_w: u32,
        out_h: u32,
        image_tx: f32,
        image_ty: f32,
        angle_rad: f32,
        delete_cropped: bool,
        background: [u8; 4],
    ) -> bool {
        self.crop_transformed_impl(
            cx,
            cy,
            viewport_w,
            viewport_h,
            out_w,
            out_h,
            image_tx,
            image_ty,
            angle_rad,
            delete_cropped,
            Some(background),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn crop_transformed_impl(
        &mut self,
        cx: f32,
        cy: f32,
        viewport_w: f32,
        viewport_h: f32,
        out_w: u32,
        out_h: u32,
        image_tx: f32,
        image_ty: f32,
        angle_rad: f32,
        _delete_cropped: bool,
        background: Option<[u8; 4]>,
    ) -> bool {
        use crate::core::command::LayerStructureCommand;

        if out_w == 0 || out_h == 0 {
            return false;
        }
        let max = MAX_DIMENSION;
        if out_w > max || out_h > max {
            return false;
        }
        let extends_canvas = cx - viewport_w * 0.5 < 0.0
            || cy - viewport_h * 0.5 < 0.0
            || cx + viewport_w * 0.5 > self.width as f32
            || cy + viewport_h * 0.5 > self.height as f32
            || image_tx.abs() > 0.001
            || image_ty.abs() > 0.001
            || angle_rad.abs() > 0.001;

        let mut cmd = LayerStructureCommand::capture_before(
            "Crop/Resample",
            &self.layer_stack,
            self.width,
            self.height,
        );

        let scale_x = viewport_w / out_w as f32;
        let scale_y = viewport_h / out_h as f32;
        let hw = out_w as f32 * 0.5;
        let hh = out_h as f32 * 0.5;
        let cos_inv = angle_rad.cos();
        let sin_inv = angle_rad.sin();
        let pivot_tx = cx + image_tx;
        let pivot_ty = cy + image_ty;
        let forward = AffineTransform::translate(hw, hh)
            .then(&AffineTransform::scale(
                1.0 / scale_x.max(f32::EPSILON),
                1.0 / scale_y.max(f32::EPSILON),
            ))
            .then(&AffineTransform::translate(image_tx, image_ty))
            .then(&AffineTransform::rotate(angle_rad))
            .then(&AffineTransform::translate(-cx, -cy));

        for layer in &mut self.layer_stack.layers {
            let source = layer.clone();
            let ox = layer.offset.0 as f32;
            let oy = layer.offset.1 as f32;
            let map = |u: f32, v: f32| -> (f32, f32) {
                let dest_x = (u - hw) * scale_x + cx;
                let dest_y = (v - hh) * scale_y + cy;
                let dx = dest_x - pivot_tx;
                let dy = dest_y - pivot_ty;
                let src_cx = dx * cos_inv + dy * sin_inv + cx;
                let src_cy = -dx * sin_inv + dy * cos_inv + cy;
                (src_cx - ox, src_cy - oy)
            };
            let layer_fill = background.filter(|_| layer.is_background);
            layer.tiles = Self::resample_into_tiles(&layer.tiles, out_w, out_h, &map, layer_fill);
            layer.width = out_w;
            layer.height = out_h;
            layer.offset = (0, 0);

            if let Some(mask) = &mut layer.mask {
                mask.tiles = Self::resample_into_tiles(
                    &mask.tiles,
                    out_w,
                    out_h,
                    &map,
                    background.map(|_| [255, 255, 255, 255]),
                );
                mask.width = out_w;
                mask.height = out_h;
            }
            reconcile_affine_layer_model(layer, &source, forward);
        }

        if extends_canvas {
            if let Some(color) = background {
                self.add_crop_background_if_missing(out_w, out_h, color);
            }
        }

        self.width = out_w;
        self.height = out_h;
        self.selection.resize(out_w, out_h);
        reconcile_crop_ink(self);
        cmd.capture_after(&self.layer_stack, self.width, self.height);
        self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
        self.flatten_full();
        true
    }

    /// Rectify a dragged quadrilateral to an axis-aligned `out_w × out_h` image
    /// (Perspective Crop). `quad` is `[top-left, top-right, bottom-right,
    /// bottom-left]` in canvas space. Each output pixel is mapped through the
    /// unit-square→quad homography back to the source and bilinear-sampled, so
    /// perspective is corrected. Mirrors `crop_rotated`'s per-layer resample.
    pub fn crop_perspective(
        &mut self,
        quad: [(f32, f32); 4],
        out_w: u32,
        out_h: u32,
        _delete_cropped: bool,
    ) -> bool {
        use crate::core::command::LayerStructureCommand;
        use crate::core::geometry::{Homography, Point};

        if out_w == 0 || out_h == 0 {
            return false;
        }
        let max = MAX_DIMENSION;
        if out_w > max || out_h > max {
            return false;
        }
        let h = match Homography::square_to_quad([
            Point::new(quad[0].0, quad[0].1),
            Point::new(quad[1].0, quad[1].1),
            Point::new(quad[2].0, quad[2].1),
            Point::new(quad[3].0, quad[3].1),
        ]) {
            Some(h) => h,
            None => return false,
        };

        let mut cmd = LayerStructureCommand::capture_before(
            "Perspective Crop",
            &self.layer_stack,
            self.width,
            self.height,
        );

        let inv_w = 1.0 / out_w as f32;
        let inv_h = 1.0 / out_h as f32;

        for layer in &mut self.layer_stack.layers {
            let ox = layer.offset.0 as f32;
            let oy = layer.offset.1 as f32;
            // Map each output-pixel centre through the unit-square->quad homography
            // back to the source; tile-native chunked resample so perspective Crop
            // works under Viewport Streaming.
            let map = |u: f32, v: f32| -> (f32, f32) {
                let src = h.apply(u * inv_w, v * inv_h);
                (src.x - ox, src.y - oy)
            };
            layer.tiles = Self::resample_into_tiles(&layer.tiles, out_w, out_h, &map, None);
            layer.width = out_w;
            layer.height = out_h;
            layer.offset = (0, 0);

            if let Some(mask) = &mut layer.mask {
                mask.tiles = Self::resample_into_tiles(&mask.tiles, out_w, out_h, &map, None);
                mask.width = out_w;
                mask.height = out_h;
            }
            rasterize_projective_layer_model(layer);
        }

        self.width = out_w;
        self.height = out_h;
        self.selection.resize(out_w, out_h);
        reconcile_crop_ink(self);
        cmd.capture_after(&self.layer_stack, self.width, self.height);
        self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
        self.flatten_full();
        true
    }

    /// Extract a (out_w × out_h) region from `src_tiles` into a new TileMap.
    /// Returns `(new_tilemap, out_w, out_h)`.
    /// The crop rectangle `(crop_x, crop_y, out_w, out_h)` is in canvas space.
    /// `offset_x/y` is the layer's canvas offset (tile_pos = canvas_pos - offset).
    pub(crate) fn crop_tilemap(
        src_tiles: &crate::core::tile::TileMap,
        src_w: u32,
        src_h: u32,
        crop_x: i32,
        crop_y: i32,
        out_w: u32,
        out_h: u32,
        offset_x: i32,
        offset_y: i32,
        fill: Option<[u8; 4]>,
    ) -> (crate::core::tile::TileMap, u32, u32) {
        if src_tiles.tiles.is_empty() && fill.is_none() {
            return (crate::core::tile::TileMap::new(out_w, out_h), out_w, out_h);
        }
        let local_x = crop_x - offset_x;
        let local_y = crop_y - offset_y;

        let tile_x0 = local_x.clamp(0, src_w as i32) as u32;
        let tile_y0 = local_y.clamp(0, src_h as i32) as u32;
        let tile_x1 = (local_x + out_w as i32).clamp(0, src_w as i32) as u32;
        let tile_y1 = (local_y + out_h as i32).clamp(0, src_h as i32) as u32;

        // Tile-native (no canvas-sized buffer): copy the clamped source rect into a
        // fresh map at its destination offset. Works on Viewport-Streaming canvases.
        let mut new_tiles = match fill {
            Some([r, g, b, a]) => crate::core::tile::TileMap::new_solid(out_w, out_h, r, g, b, a),
            None => crate::core::tile::TileMap::new(out_w, out_h),
        };
        if tile_x1 > tile_x0 && tile_y1 > tile_y0 {
            let copy_w = tile_x1 - tile_x0;
            let copy_h = tile_y1 - tile_y0;
            let dest_x = (tile_x0 as i32 - local_x) as u32;
            let dest_y = (tile_y0 as i32 - local_y) as u32;
            new_tiles.blit_region_from(src_tiles, tile_x0, tile_y0, dest_x, dest_y, copy_w, copy_h);
        }
        // A crop of a 16-bit layer stays 16-bit: the blit copied the overlapping
        // region's masters; up-convert any fill / transparent border (exact
        // `v*257`) so has_hdr() holds for the whole cropped result instead of a
        // single master-less border tile flipping it to false.
        if src_tiles.has_hdr() {
            new_tiles.promote_to_hdr();
        }
        new_tiles.bump_all_revisions();
        (new_tiles, out_w, out_h)
    }

    /// Resample `src` into a fresh tile-native `out_w × out_h` map in 256-px chunks
    /// (no canvas-sized buffer, so resampling crops work under Viewport Streaming).
    /// `map(u, v)` takes an output pixel CENTRE and returns the source-space
    /// coordinate to bilinear-sample from `src`.
    pub(crate) fn resample_into_tiles(
        src: &crate::core::tile::TileMap,
        out_w: u32,
        out_h: u32,
        map: impl Fn(f32, f32) -> (f32, f32) + Sync,
        background: Option<[u8; 4]>,
    ) -> crate::core::tile::TileMap {
        use rayon::prelude::*;
        let mut new_tiles = crate::core::tile::TileMap::new(out_w, out_h);
        // The common large-document case contains many empty/group/adjustment
        // layers. Sampling every output pixel for each empty sparse map makes a
        // transformed crop O(empty_layers * output_pixels) for no result.
        if src.tiles.is_empty() && background.is_none() {
            return new_tiles;
        }
        let chunk = 256u32;
        // Resample at 16 bits when the source carries a full master, so resize /
        // rotate-by-angle / perspective keep precision instead of quantizing.
        // write_region16 also refreshes each tile's 8-bit mirror.
        let src_hdr = src.has_hdr();
        let mut by = 0;
        while by < out_h {
            let ch = chunk.min(out_h - by);
            let mut bx = 0;
            while bx < out_w {
                let cw = chunk.min(out_w - bx);
                if src_hdr {
                    let mut buf = vec![0u16; (cw * ch * 4) as usize];
                    buf.par_chunks_mut((cw * 4) as usize)
                        .enumerate()
                        .for_each(|(r, row)| {
                            let v = (by + r as u32) as f32 + 0.5;
                            for c in 0..cw as usize {
                                let u = (bx + c as u32) as f32 + 0.5;
                                let (sx, sy) = map(u, v);
                                let (mut rr, mut gg, mut bb, mut aa) =
                                    src.sample_bilinear16(sx, sy);
                                if let Some([br, bg, bb_bg, ba]) = background {
                                    // Background is an 8-bit fill colour; lift to 16 bits.
                                    let (br, bg, bb_bg, ba) = (
                                        br as u16 * 257,
                                        bg as u16 * 257,
                                        bb_bg as u16 * 257,
                                        ba as u16 * 257,
                                    );
                                    let src_a = aa as f32 / 65535.0;
                                    let bg_a = ba as f32 / 65535.0;
                                    let out_a = src_a + bg_a * (1.0 - src_a);
                                    if out_a > 0.0 {
                                        rr = ((rr as f32 * src_a
                                            + br as f32 * bg_a * (1.0 - src_a))
                                            / out_a)
                                            .round()
                                            .clamp(0.0, 65535.0)
                                            as u16;
                                        gg = ((gg as f32 * src_a
                                            + bg as f32 * bg_a * (1.0 - src_a))
                                            / out_a)
                                            .round()
                                            .clamp(0.0, 65535.0)
                                            as u16;
                                        bb = ((bb as f32 * src_a
                                            + bb_bg as f32 * bg_a * (1.0 - src_a))
                                            / out_a)
                                            .round()
                                            .clamp(0.0, 65535.0)
                                            as u16;
                                    }
                                    aa = (out_a * 65535.0).round().clamp(0.0, 65535.0) as u16;
                                }
                                let idx = c * 4;
                                row[idx] = rr;
                                row[idx + 1] = gg;
                                row[idx + 2] = bb;
                                row[idx + 3] = aa;
                            }
                        });
                    new_tiles.write_region16(bx, by, cw, ch, &buf);
                } else {
                    let mut buf = vec![0u8; (cw * ch * 4) as usize];
                    buf.par_chunks_mut((cw * 4) as usize)
                        .enumerate()
                        .for_each(|(r, row)| {
                            let v = (by + r as u32) as f32 + 0.5;
                            for c in 0..cw as usize {
                                let u = (bx + c as u32) as f32 + 0.5;
                                let (sx, sy) = map(u, v);
                                let (mut rr, mut gg, mut bb, mut aa) = src.sample_bilinear(sx, sy);
                                if let Some([br, bg, bb_bg, ba]) = background {
                                    let src_a = aa as f32 / 255.0;
                                    let bg_a = ba as f32 / 255.0;
                                    let out_a = src_a + bg_a * (1.0 - src_a);
                                    if out_a > 0.0 {
                                        rr = ((rr as f32 * src_a
                                            + br as f32 * bg_a * (1.0 - src_a))
                                            / out_a)
                                            .round()
                                            .clamp(0.0, 255.0)
                                            as u8;
                                        gg = ((gg as f32 * src_a
                                            + bg as f32 * bg_a * (1.0 - src_a))
                                            / out_a)
                                            .round()
                                            .clamp(0.0, 255.0)
                                            as u8;
                                        bb = ((bb as f32 * src_a
                                            + bb_bg as f32 * bg_a * (1.0 - src_a))
                                            / out_a)
                                            .round()
                                            .clamp(0.0, 255.0)
                                            as u8;
                                    }
                                    aa = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
                                }
                                let idx = c * 4;
                                row[idx] = rr;
                                row[idx + 1] = gg;
                                row[idx + 2] = bb;
                                row[idx + 3] = aa;
                            }
                        });
                    new_tiles.write_region(bx, by, cw, ch, &buf);
                }
                bx += cw;
            }
            by += ch;
        }
        new_tiles.bump_all_revisions();
        new_tiles
    }

    pub fn flip_horizontal(&mut self) {
        use crate::core::command::{PixelTransformCommand, PixelTransformKind};
        let cmd = PixelTransformCommand::capture_before(
            "Flip Horizontal",
            PixelTransformKind::FlipH,
            &self.layer_stack,
            self.width,
            self.height,
            &self.selection,
        );
        let canvas_w = self.width as i32;
        for layer in &mut self.layer_stack.layers {
            let lw = layer.width as i32;
            let old_ox = layer.offset.0;
            layer.tiles = layer.tiles.flip_h();
            layer.offset.0 = canvas_w - old_ox - lw;
            if let Some(mask) = &mut layer.mask {
                PixelTransformCommand::transform_layer_mask(mask, &PixelTransformKind::FlipH);
            }
        }
        {
            let (nm, nw, nh) = PixelTransformCommand::transform_sel_mask_pub(
                &self.selection.mask,
                &PixelTransformKind::FlipH,
                self.selection.width,
                self.selection.height,
            );
            self.selection.mask = nm;
            self.selection.width = nw;
            self.selection.height = nh;
            self.selection.offset = (0, 0);
            self.selection.mask_revision += 1;
            self.selection.mark_bbox_dirty();
        }
        self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
        self.flatten_full();
    }

    pub fn flip_vertical(&mut self) {
        use crate::core::command::{PixelTransformCommand, PixelTransformKind};
        let cmd = PixelTransformCommand::capture_before(
            "Flip Vertical",
            PixelTransformKind::FlipV,
            &self.layer_stack,
            self.width,
            self.height,
            &self.selection,
        );
        let canvas_h = self.height as i32;
        for layer in &mut self.layer_stack.layers {
            let lh = layer.height as i32;
            let old_oy = layer.offset.1;
            layer.tiles = layer.tiles.flip_v();
            layer.offset.1 = canvas_h - old_oy - lh;
            if let Some(mask) = &mut layer.mask {
                PixelTransformCommand::transform_layer_mask(mask, &PixelTransformKind::FlipV);
            }
        }
        {
            let (nm, nw, nh) = PixelTransformCommand::transform_sel_mask_pub(
                &self.selection.mask,
                &PixelTransformKind::FlipV,
                self.selection.width,
                self.selection.height,
            );
            self.selection.mask = nm;
            self.selection.width = nw;
            self.selection.height = nh;
            self.selection.offset = (0, 0);
            self.selection.mask_revision += 1;
            self.selection.mark_bbox_dirty();
        }
        self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
        self.flatten_full();
    }

    pub fn rotate_90_cw(&mut self) {
        use crate::core::command::{PixelTransformCommand, PixelTransformKind};
        let cmd = PixelTransformCommand::capture_before(
            "Rotate 90 CW",
            PixelTransformKind::Rotate90CW,
            &self.layer_stack,
            self.width,
            self.height,
            &self.selection,
        );
        let canvas_h = self.height as i32;
        for layer in &mut self.layer_stack.layers {
            let old_lh = layer.height as i32;
            let old_ox = layer.offset.0;
            let old_oy = layer.offset.1;
            layer.tiles = layer.tiles.rotate_90_cw();
            std::mem::swap(&mut layer.width, &mut layer.height);
            layer.offset.0 = canvas_h - old_oy - old_lh;
            layer.offset.1 = old_ox;
            if let Some(mask) = &mut layer.mask {
                PixelTransformCommand::transform_layer_mask(mask, &PixelTransformKind::Rotate90CW);
            }
        }
        std::mem::swap(&mut self.width, &mut self.height);
        {
            let (nm, nw, nh) = PixelTransformCommand::transform_sel_mask_pub(
                &self.selection.mask,
                &PixelTransformKind::Rotate90CW,
                self.selection.width,
                self.selection.height,
            );
            self.selection.mask = nm;
            self.selection.width = nw;
            self.selection.height = nh;
            self.selection.offset = (0, 0);
            self.selection.mask_revision += 1;
            self.selection.mark_bbox_dirty();
        }
        self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
        self.flatten_full();
    }

    pub fn rotate_90_ccw(&mut self) {
        use crate::core::command::{PixelTransformCommand, PixelTransformKind};
        let cmd = PixelTransformCommand::capture_before(
            "Rotate 90 CCW",
            PixelTransformKind::Rotate90CCW,
            &self.layer_stack,
            self.width,
            self.height,
            &self.selection,
        );
        let canvas_w = self.width as i32;
        for layer in &mut self.layer_stack.layers {
            let old_lw = layer.width as i32;
            let old_ox = layer.offset.0;
            let old_oy = layer.offset.1;
            layer.tiles = layer.tiles.rotate_90_ccw();
            std::mem::swap(&mut layer.width, &mut layer.height);
            layer.offset.0 = old_oy;
            layer.offset.1 = canvas_w - old_ox - old_lw;
            if let Some(mask) = &mut layer.mask {
                PixelTransformCommand::transform_layer_mask(mask, &PixelTransformKind::Rotate90CCW);
            }
        }
        std::mem::swap(&mut self.width, &mut self.height);
        {
            let (nm, nw, nh) = PixelTransformCommand::transform_sel_mask_pub(
                &self.selection.mask,
                &PixelTransformKind::Rotate90CCW,
                self.selection.width,
                self.selection.height,
            );
            self.selection.mask = nm;
            self.selection.width = nw;
            self.selection.height = nh;
            self.selection.offset = (0, 0);
            self.selection.mask_revision += 1;
            self.selection.mark_bbox_dirty();
        }
        self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
        self.flatten_full();
    }

    pub fn resize(&mut self, new_w: u32, new_h: u32) -> bool {
        if new_w == self.width && new_h == self.height {
            return true;
        }
        if new_w == 0 || new_h == 0 {
            return false;
        }
        let max = MAX_DIMENSION;
        if new_w > max || new_h > max {
            return false;
        }

        self.begin_undo_group("Resize Canvas");

        if self.selection.active {
            let mut sel_cmd = crate::core::command::SelectionCommand::capture_before(
                "Resize Canvas",
                &self.selection,
            );
            self.selection.resize(new_w, new_h);
            sel_cmd.capture_after(&self.selection);
            self.record_as(Box::new(sel_cmd), ChangeKind::Selection);
        } else {
            self.selection.resize(new_w, new_h);
        }

        let resize_cmd = crate::core::command::ResizeCanvasCommand::capture_before(
            &self.layer_stack,
            self.width,
            self.height,
            new_w,
            new_h,
        );
        for layer in &mut self.layer_stack.layers {
            let copy_w = layer.width.min(new_w);
            let copy_h = layer.height.min(new_h);
            // Tile-native crop/extend anchored top-left: the extended area stays a
            // sparse transparent region (no canvas-sized buffer), so canvas Resize
            // works under Viewport Streaming.
            let mut new_tiles = crate::core::tile::TileMap::new(new_w, new_h);
            if copy_w > 0 && copy_h > 0 {
                new_tiles.blit_region_from(&layer.tiles, 0, 0, 0, 0, copy_w, copy_h);
            }
            layer.tiles = new_tiles;
            layer.width = new_w;
            layer.height = new_h;
            if let Some(mask) = &mut layer.mask {
                mask.resize_to(new_w, new_h);
            }
        }
        self.width = new_w;
        self.height = new_h;
        self.pixels = if Self::fits_flat_buffer(new_w, new_h) {
            Self::checked_rgba_len(new_w, new_h)
                .map(|len| vec![255u8; len])
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        self.record_as(Box::new(resize_cmd), ChangeKind::LayerStructure);

        self.end_undo_group();
        self.flatten_full();
        true
    }

    pub fn resize_image(&mut self, new_w: u32, new_h: u32, new_dpi: f32) -> bool {
        if new_w == 0 || new_h == 0 {
            return false;
        }
        // Tile-native chunked resample (no canvas-sized buffer), so Image Size runs
        // under Viewport Streaming. Only the per-dimension cap applies.
        let max = MAX_DIMENSION;
        if new_w > max || new_h > max {
            return false;
        }

        let sx = new_w as f32 / self.width.max(1) as f32;
        let sy = new_h as f32 / self.height.max(1) as f32;

        self.begin_undo_group("Image Size");

        if self.selection.active {
            let mut sel_cmd = crate::core::command::SelectionCommand::capture_before(
                "Image Size",
                &self.selection,
            );
            Self::resample_selection(&mut self.selection, new_w, new_h, sx, sy);
            sel_cmd.capture_after(&self.selection);
            self.record_as(Box::new(sel_cmd), ChangeKind::Selection);
        } else {
            self.selection.resize(new_w, new_h);
        }

        let mut cmd = crate::core::command::LayerStructureCommand::capture_before(
            "Image Size",
            &self.layer_stack,
            self.width,
            self.height,
        );

        for layer in &mut self.layer_stack.layers {
            let layer_new_w = ((layer.width as f32 * sx).round() as u32).max(1);
            let layer_new_h = ((layer.height as f32 * sy).round() as u32).max(1);
            layer.tiles = Self::resample_tilemap(&layer.tiles, layer_new_w, layer_new_h);
            layer.width = layer_new_w;
            layer.height = layer_new_h;
            layer.offset = (
                (layer.offset.0 as f32 * sx).round() as i32,
                (layer.offset.1 as f32 * sy).round() as i32,
            );

            if let Some(mask) = &mut layer.mask {
                let mask_new_w = ((mask.width as f32 * sx).round() as u32).max(1);
                let mask_new_h = ((mask.height as f32 * sy).round() as u32).max(1);
                mask.tiles = Self::resample_tilemap(&mask.tiles, mask_new_w, mask_new_h);
                mask.width = mask_new_w;
                mask.height = mask_new_h;
            }
        }

        self.width = new_w;
        self.height = new_h;
        self.metadata.resolution_ppi = new_dpi.max(1.0);
        self.pixels = if Self::fits_flat_buffer(new_w, new_h) {
            Self::checked_rgba_len(new_w, new_h)
                .map(|len| vec![255u8; len])
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        cmd.capture_after(&self.layer_stack, self.width, self.height);
        self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
        self.end_undo_group();
        self.flatten_full();
        true
    }

    /// Bilinear resample `src` to `new_w × new_h`, tile-native (256-px chunks, no
    /// canvas-sized buffer) so Image Size works under Viewport Streaming.
    pub(crate) fn resample_tilemap(
        src: &crate::core::tile::TileMap,
        new_w: u32,
        new_h: u32,
    ) -> crate::core::tile::TileMap {
        if src.width == new_w && src.height == new_h {
            return src.clone();
        }

        let scale_x = src.width.max(1) as f32 / new_w.max(1) as f32;
        let scale_y = src.height.max(1) as f32 / new_h.max(1) as f32;
        Self::resample_into_tiles(
            src,
            new_w,
            new_h,
            move |u, v| (u * scale_x - 0.5, v * scale_y - 0.5),
            None,
        )
    }

    pub(crate) fn resample_selection(
        selection: &mut crate::core::selection::Selection,
        new_w: u32,
        new_h: u32,
        sx: f32,
        sy: f32,
    ) {
        use rayon::prelude::*;

        let old_w = selection.width.max(1);
        let old_h = selection.height.max(1);
        let old_mask = selection.mask.clone();
        let old_offset = selection.offset;
        let scale_x = old_w as f32 / new_w.max(1) as f32;
        let scale_y = old_h as f32 / new_h.max(1) as f32;
        let mut mask = vec![0u8; (new_w as usize).saturating_mul(new_h as usize)];

        mask.par_chunks_mut(new_w as usize)
            .enumerate()
            .for_each(|(y, row)| {
                let src_y = ((y as f32 + 0.5) * scale_y - 0.5)
                    .round()
                    .clamp(0.0, (old_h - 1) as f32) as u32;
                for x in 0..new_w as usize {
                    let src_x = ((x as f32 + 0.5) * scale_x - 0.5)
                        .round()
                        .clamp(0.0, (old_w - 1) as f32) as u32;
                    row[x] = old_mask[(src_y * old_w + src_x) as usize];
                }
            });

        selection.mask = mask;
        selection.width = new_w;
        selection.height = new_h;
        selection.offset = (
            (old_offset.0 as f32 * sx).round() as i32,
            (old_offset.1 as f32 * sy).round() as i32,
        );
        selection.active = selection.mask.par_iter().any(|&v| v > 0);
        selection.mask_revision += 1;
        selection.mark_bbox_dirty();
    }
}

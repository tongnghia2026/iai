//! Canvas geometry: crop (straight/rotated/transformed/perspective), flips,
//! 90-degree rotates, canvas/image resize and the tile resampling behind them.
//! Every size change goes through the app's apply_canvas_event path.

// Main document model — Canvas, History, metadata.
//

use super::*;
use iai_doc::layer::{Layer, LayerType};
use iai_doc::vector::object::VectorGeometry;
use iai_geom::vector::affine::AffineTransform;
use iai_history::gateway::ChangeKind;
use iai_raster::tile::TileMap;

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
            iai_vector::command_vector::apply_object_to_layer(layer, object);
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod crop_regression_tests {
    use super::*;
    use iai_doc::shape::{ShapeData, ShapeKind};
    use iai_doc::vector::color::ColorValue;
    use iai_doc::vector::object::VectorObjectData;
    use iai_doc::vector::path::{Contour, FillRule, Node, PathData};
    use iai_doc::vector::style::VectorStyle;
    use iai_geom::geometry::Point;
    use iai_text::text::{TextData, rasterize_placed};
    use iai_vector::vector::raster::raster_geometry;

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
        assert!(
            canvas.crop_transformed(128.5, 128.5, 257.0, 257.0, 257, 257, 0.0, 0.0, 0.0, true,)
        );
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
        iai_vector::command_vector::apply_object_to_layer(
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
        iai_vector::command_vector::fold_offset_into_model(&mut rebuilt);
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
        iai_vector::command_vector::apply_object_to_layer(
            &mut canvas.layer_stack.layers[0],
            square_object((100.0, 80.0)),
        );
        let text_idx = canvas.layer_stack.add_layer(256, 256);
        set_text_layer(&mut canvas.layer_stack.layers[text_idx], (110, 100));

        assert!(
            canvas.crop_transformed(128.0, 128.0, 128.0, 128.0, 128, 128, 0.0, 0.0, 0.0, true,)
        );
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
        iai_vector::command_vector::apply_object_to_layer(
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
        iai_vector::command_vector::apply_object_to_layer(
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
        assert!(
            canvas.layer_stack.layers[0]
                .tiles
                .content_bounds()
                .is_some()
        );
        assert!(
            canvas.layer_stack.layers[0].tiles.has_any_ink(),
            "projective rasterization must retain CMYK separations"
        );
    }
}

/// Recover the upright text anchor from the current derived raster. This is the
/// core equivalent of the Type tool's edit-origin recovery and lets a crop
/// transform the editable text model rather than only its cached pixels.
fn text_origin_for_layer(td: &iai_text::text::TextData, layer: &Layer) -> (i32, i32) {
    let Some((placed, delta)) = iai_text::text::rasterize_placed(td) else {
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
    td: &iai_text::text::TextData,
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
    next.font_px = (next.font_px * sy).clamp(
        iai_text::text::MIN_EDITABLE_FONT_PX,
        iai_text::text::MAX_EDITABLE_FONT_PX,
    );
    next.tracking_px = (next.tracking_px * sy).clamp(-200.0, 500.0);
    next.stretch_x = (sx / sy).clamp(0.01, 100.0);
    for glyph in &mut next.glyph_styles {
        glyph.font_px = (glyph.font_px * sy).clamp(
            iai_text::text::MIN_EDITABLE_FONT_PX,
            iai_text::text::MAX_EDITABLE_FONT_PX,
        );
    }

    let origin = text_origin_for_layer(td, source);
    let mapped = transform.apply_point(iai_geom::geometry::Point::new(
        origin.0 as f32,
        origin.1 as f32,
    ));
    let mapped_origin = (mapped.x.round() as i32, mapped.y.round() as i32);
    let Some((raster, delta)) = iai_text::text::rasterize_placed(&next) else {
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
// The text arm's test rewrites the layer; it does not belong in a match guard.
#[allow(clippy::collapsible_match)]
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
            iai_vector::command_vector::apply_object_to_layer(layer, object);
        }
        LayerType::Vector(VectorGeometry::Primitive(shape)) => {
            let mut object = shape.to_vector_object(source.offset);
            object.transform = transform.then(&object.transform);
            let scale = transform.determinant().abs().sqrt();
            object.style.stroke_style.width = (object.style.stroke_style.width * scale).max(0.0);
            iai_vector::command_vector::apply_object_to_layer(layer, object);
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

/// The output pixels `[x0, y0, x1, y1]` a layer's tiles can reach when the
/// page is resampled through `forward` (page → output): the tiles' extent,
/// grown a source pixel for the bilinear reach, turned onto the output and
/// grown a pixel there for the footprint taps. `None` (no limit) when the map
/// has no tile or the frame is degenerate.
fn output_reach(
    tiles: &TileMap,
    offset: (i32, i32),
    forward: &AffineTransform,
    out_w: u32,
    out_h: u32,
) -> Option<[u32; 4]> {
    use iai_raster::tile::TILE_SIZE;
    let size = TILE_SIZE as i32;
    let mut keys = tiles.tiles.keys();
    let first = keys.next()?;
    let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.x, first.y);
    for pos in keys {
        x0 = x0.min(pos.x);
        y0 = y0.min(pos.y);
        x1 = x1.max(pos.x);
        y1 = y1.max(pos.y);
    }
    let left = (x0 * size + offset.0) as f32 - 1.0;
    let top = (y0 * size + offset.1) as f32 - 1.0;
    let right = ((x1 + 1) * size + offset.0) as f32 + 1.0;
    let bottom = ((y1 + 1) * size + offset.1) as f32 + 1.0;
    let on_output = forward.transform_bounds(iai_geom::geometry::Rect::new(
        left,
        top,
        right - left,
        bottom - top,
    ));
    if ![on_output.x, on_output.y, on_output.w, on_output.h]
        .iter()
        .all(|v| v.is_finite())
    {
        return None;
    }
    let clamp = |v: f32, max: u32| v.clamp(0.0, max as f32) as u32;
    Some([
        clamp((on_output.x - 1.0).floor(), out_w),
        clamp((on_output.y - 1.0).floor(), out_h),
        clamp((on_output.right() + 1.0).ceil(), out_w),
        clamp((on_output.bottom() + 1.0).ceil(), out_h),
    ])
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
        let mut layer = iai_doc::layer::Layer::new(id, "Background", width, height);
        layer.tiles = iai_raster::tile::TileMap::new_solid(width, height, r, g, b, a);
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

        let mut cmd = iai_history::command::LayerStructureCommand::capture_before(
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

        if expands_canvas && let Some(color) = background {
            self.add_crop_background_if_missing(w, h, color);
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
        use iai_history::command::LayerStructureCommand;

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
            layer.tiles = Self::resample_into_tiles(&layer.tiles, out_w, out_h, map, None);
            layer.width = out_w;
            layer.height = out_h;
            layer.offset = (0, 0);

            if let Some(mask) = &mut layer.mask {
                mask.tiles = Self::resample_into_tiles(&mask.tiles, out_w, out_h, map, None);
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
        use iai_history::command::LayerStructureCommand;

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
        let footprint = scale_x.max(scale_y);
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
            // A path or shape without a mask is drawn again from its model
            // below: resampling its pixels first would be thrown away.
            let redrawn = layer.mask.is_none()
                && matches!(
                    layer.layer_type,
                    LayerType::Vector(VectorGeometry::Path(_) | VectorGeometry::Primitive(_))
                );
            layer.tiles = if redrawn {
                TileMap::new(out_w, out_h)
            } else {
                Self::resample_footprint(
                    &layer.tiles,
                    out_w,
                    out_h,
                    map,
                    layer_fill,
                    footprint,
                    false,
                    output_reach(&layer.tiles, layer.offset, &forward, out_w, out_h),
                )
            };
            layer.width = out_w;
            layer.height = out_h;
            layer.offset = (0, 0);

            if let Some(mask) = &mut layer.mask {
                mask.tiles = Self::resample_footprint(
                    &mask.tiles,
                    out_w,
                    out_h,
                    map,
                    background.map(|_| [255, 255, 255, 255]),
                    footprint,
                    true,
                    output_reach(&mask.tiles, source.offset, &forward, out_w, out_h),
                );
                mask.width = out_w;
                mask.height = out_h;
            }
            reconcile_affine_layer_model(layer, &source, forward);
        }

        if extends_canvas && let Some(color) = background {
            self.add_crop_background_if_missing(out_w, out_h, color);
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
        use iai_geom::geometry::{Homography, Point};
        use iai_history::command::LayerStructureCommand;

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
            layer.tiles = Self::resample_into_tiles(&layer.tiles, out_w, out_h, map, None);
            layer.width = out_w;
            layer.height = out_h;
            layer.offset = (0, 0);

            if let Some(mask) = &mut layer.mask {
                mask.tiles = Self::resample_into_tiles(&mask.tiles, out_w, out_h, map, None);
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
        src_tiles: &iai_raster::tile::TileMap,
        src_w: u32,
        src_h: u32,
        crop_x: i32,
        crop_y: i32,
        out_w: u32,
        out_h: u32,
        offset_x: i32,
        offset_y: i32,
        fill: Option<[u8; 4]>,
    ) -> (iai_raster::tile::TileMap, u32, u32) {
        if src_tiles.tiles.is_empty() && fill.is_none() {
            return (iai_raster::tile::TileMap::new(out_w, out_h), out_w, out_h);
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
            Some([r, g, b, a]) => iai_raster::tile::TileMap::new_solid(out_w, out_h, r, g, b, a),
            None => iai_raster::tile::TileMap::new(out_w, out_h),
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
        src: &iai_raster::tile::TileMap,
        out_w: u32,
        out_h: u32,
        map: impl Fn(f32, f32) -> (f32, f32) + Sync,
        background: Option<[u8; 4]>,
    ) -> iai_raster::tile::TileMap {
        Self::resample_into_tiles_footprint(src, out_w, out_h, map, background, 1.0)
    }

    /// [`Self::resample_into_tiles`] for a reduction: `footprint` is how many
    /// source pixels one output pixel spans. Past 1.5 each output pixel
    /// averages a grid of bilinear taps over its footprint (premultiplied),
    /// so a big photo shrunk to print size stays crisp instead of aliasing.
    pub(crate) fn resample_into_tiles_footprint(
        src: &iai_raster::tile::TileMap,
        out_w: u32,
        out_h: u32,
        map: impl Fn(f32, f32) -> (f32, f32) + Sync,
        background: Option<[u8; 4]>,
        footprint: f32,
    ) -> iai_raster::tile::TileMap {
        Self::resample_footprint(src, out_w, out_h, map, background, footprint, false, None)
    }

    /// [`Self::resample_into_tiles_footprint`], and for a layer mask with
    /// `keep_edge`: an output pixel the source covers in part takes the
    /// source's own value there instead of a blend toward `background`. A
    /// mask blended toward white at its edge shows the layer's edge pixels
    /// through even where it hides them (a faint line along the photo's edge
    /// when a crop reaches past it).
    ///
    /// `reach` (`[x0, y0, x1, y1]` output pixels, see [`output_reach`]) is
    /// where the source can land: output tiles outside it are left out
    /// unsampled, as they would come out clear. Only without `background`.
    #[allow(clippy::too_many_arguments)]
    fn resample_footprint(
        src: &iai_raster::tile::TileMap,
        out_w: u32,
        out_h: u32,
        map: impl Fn(f32, f32) -> (f32, f32) + Sync,
        background: Option<[u8; 4]>,
        footprint: f32,
        keep_edge: bool,
        reach: Option<[u32; 4]>,
    ) -> iai_raster::tile::TileMap {
        let reach = reach.filter(|_| background.is_none());
        let outside = |x: u32, y: u32| {
            reach.is_some_and(|[x0, y0, x1, y1]| {
                let size = iai_raster::tile::TILE_SIZE;
                x >= x1 || y >= y1 || x + size <= x0 || y + size <= y0
            })
        };
        use iai_raster::fast_math::{round_u8, round_u16};
        use iai_raster::tile::{BilinearCursor, TILE_SIZE, Tile, TileGrid, TilePos};
        use rayon::prelude::*;
        // Per-pixel reads go through a dense index, not the map's hash.
        let grid = TileGrid::new(src);
        let taps = if footprint.is_finite() {
            footprint.round().clamp(1.0, 5.0) as usize
        } else {
            1
        };
        let offsets: Vec<f32> = (0..taps)
            .map(|i| (i as f32 + 0.5) / taps as f32 - 0.5)
            .collect();
        // One tap per output pixel (an enlargement): neighbours share their
        // source pixels, which the cursor keeps. Several taps jump between
        // source pixels, where it would only add work.
        let sample8 = |cur: &mut BilinearCursor, u: f32, v: f32| {
            if taps == 1 {
                let (sx, sy) = map(u, v);
                return cur.sample_bilinear(sx, sy);
            }
            let mut acc = [0.0f32; 4];
            for &dv in &offsets {
                for &du in &offsets {
                    let (sx, sy) = map(u + du, v + dv);
                    let (r, g, b, a) = grid.sample_bilinear(sx, sy);
                    let a = a as f32;
                    acc[0] += r as f32 * a;
                    acc[1] += g as f32 * a;
                    acc[2] += b as f32 * a;
                    acc[3] += a;
                }
            }
            if acc[3] <= 0.0 {
                return (0, 0, 0, 0);
            }
            let c = |v: f32| round_u8(v / acc[3]);
            let a = round_u8(acc[3] / (taps * taps) as f32);
            (c(acc[0]), c(acc[1]), c(acc[2]), a)
        };
        let sample16 = |u: f32, v: f32| -> (u16, u16, u16, u16) {
            if taps == 1 {
                let (sx, sy) = map(u, v);
                return grid.sample_bilinear16(sx, sy);
            }
            let mut acc = [0.0f64; 4];
            for &dv in &offsets {
                for &du in &offsets {
                    let (sx, sy) = map(u + du, v + dv);
                    let (r, g, b, a) = grid.sample_bilinear16(sx, sy);
                    let a = a as f64;
                    acc[0] += r as f64 * a;
                    acc[1] += g as f64 * a;
                    acc[2] += b as f64 * a;
                    acc[3] += a;
                }
            }
            if acc[3] <= 0.0 {
                return (0, 0, 0, 0);
            }
            let c = |v: f64| (v / acc[3]).round().clamp(0.0, 65535.0) as u16;
            let a = (acc[3] / (taps * taps) as f64).round().clamp(0.0, 65535.0) as u16;
            (c(acc[0]), c(acc[1]), c(acc[2]), a)
        };
        let mut new_tiles = iai_raster::tile::TileMap::new(out_w, out_h);
        // The common large-document case contains many empty/group/adjustment
        // layers. Sampling every output pixel for each empty sparse map makes a
        // transformed crop O(empty_layers * output_pixels) for no result.
        if src.tiles.is_empty() && background.is_none() {
            return new_tiles;
        }
        if reach.is_some_and(|[x0, y0, x1, y1]| x0 >= x1 || y0 >= y1) {
            return new_tiles;
        }
        if !src.has_hdr() {
            let pixel8 = |cur: &mut BilinearCursor, u: f32, v: f32| -> [u8; 4] {
                let (mut rr, mut gg, mut bb, mut aa) = sample8(cur, u, v);
                if keep_edge && aa > 0 {
                    aa = u8::MAX;
                }
                // The blend below gives exactly these for an opaque sample (the
                // sample itself) and for none over an opaque fill (the fill).
                match background {
                    Some(_) if aa == u8::MAX => return [rr, gg, bb, aa],
                    Some([br, bg, bb_bg, u8::MAX]) if aa == 0 => {
                        return [br, bg, bb_bg, u8::MAX];
                    }
                    _ => {}
                }
                if let Some([br, bg, bb_bg, ba]) = background {
                    let src_a = aa as f32 / 255.0;
                    let bg_a = ba as f32 / 255.0;
                    let out_a = src_a + bg_a * (1.0 - src_a);
                    if out_a > 0.0 {
                        rr = round_u8(
                            (rr as f32 * src_a + br as f32 * bg_a * (1.0 - src_a)) / out_a,
                        );
                        gg = round_u8(
                            (gg as f32 * src_a + bg as f32 * bg_a * (1.0 - src_a)) / out_a,
                        );
                        bb = round_u8(
                            (bb as f32 * src_a + bb_bg as f32 * bg_a * (1.0 - src_a)) / out_a,
                        );
                    }
                    aa = round_u8(out_a * 255.0);
                }
                [rr, gg, bb, aa]
            };
            // Each output tile is sampled straight into its own storage on one
            // worker; a tile left all transparent stays absent (sparse), as a
            // chunk written through write_region would.
            let cols = out_w.div_ceil(TILE_SIZE);
            let rows = out_h.div_ceil(TILE_SIZE);
            let built: Vec<(TilePos, Tile)> = (0..cols * rows)
                .into_par_iter()
                .filter_map(|i| {
                    let (tx, ty) = (i % cols, i / cols);
                    let (bx, by) = (tx * TILE_SIZE, ty * TILE_SIZE);
                    if outside(bx, by) {
                        return None;
                    }
                    let cw = TILE_SIZE.min(out_w - bx) as usize;
                    let ch = TILE_SIZE.min(out_h - by);
                    let mut tile = Tile::new_empty();
                    let mut cur = grid.cursor();
                    let stride = (TILE_SIZE * 4) as usize;
                    for (r, row) in tile
                        .pixels
                        .chunks_exact_mut(stride)
                        .take(ch as usize)
                        .enumerate()
                    {
                        let v = (by + r as u32) as f32 + 0.5;
                        for (c, px) in row.chunks_exact_mut(4).take(cw).enumerate() {
                            let u = (bx + c as u32) as f32 + 0.5;
                            px.copy_from_slice(&pixel8(&mut cur, u, v));
                        }
                    }
                    let pos = TilePos {
                        x: tx as i32,
                        y: ty as i32,
                    };
                    tile.pixels.iter().any(|&b| b != 0).then_some((pos, tile))
                })
                .collect();
            for (pos, tile) in built {
                new_tiles.tiles.insert(pos, std::sync::Arc::new(tile));
            }
            new_tiles.bump_all_revisions();
            return new_tiles;
        }
        // A source with a full 16-bit master resamples at 16 bits, so resize /
        // rotate-by-angle / perspective keep precision instead of quantizing.
        // write_region16 also refreshes each tile's 8-bit mirror.
        let chunk = 256u32;
        let mut by = 0;
        while by < out_h {
            let ch = chunk.min(out_h - by);
            let mut bx = 0;
            while bx < out_w {
                let cw = chunk.min(out_w - bx);
                if outside(bx, by) {
                    bx += cw;
                    continue;
                }
                let mut buf = vec![0u16; (cw * ch * 4) as usize];
                buf.par_chunks_mut((cw * 4) as usize)
                    .enumerate()
                    .for_each(|(r, row)| {
                        let v = (by + r as u32) as f32 + 0.5;
                        for c in 0..cw as usize {
                            let u = (bx + c as u32) as f32 + 0.5;
                            let (mut rr, mut gg, mut bb, mut aa) = sample16(u, v);
                            if keep_edge && aa > 0 {
                                aa = u16::MAX;
                            }
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
                                    rr = round_u16(
                                        (rr as f32 * src_a + br as f32 * bg_a * (1.0 - src_a))
                                            / out_a,
                                    );
                                    gg = round_u16(
                                        (gg as f32 * src_a + bg as f32 * bg_a * (1.0 - src_a))
                                            / out_a,
                                    );
                                    bb = round_u16(
                                        (bb as f32 * src_a + bb_bg as f32 * bg_a * (1.0 - src_a))
                                            / out_a,
                                    );
                                }
                                aa = round_u16(out_a * 65535.0);
                            }
                            let idx = c * 4;
                            row[idx] = rr;
                            row[idx + 1] = gg;
                            row[idx + 2] = bb;
                            row[idx + 3] = aa;
                        }
                    });
                new_tiles.write_region16(bx, by, cw, ch, &buf);
                bx += cw;
            }
            by += ch;
        }
        new_tiles.bump_all_revisions();
        new_tiles
    }

    pub fn flip_horizontal(&mut self) {
        use iai_history::command::{PixelTransformCommand, PixelTransformKind};
        let cmd = PixelTransformCommand::capture_before(
            "Flip Horizontal",
            PixelTransformKind::FlipH,
            &self.layer_stack,
            self.width,
            self.height,
            &self.selection,
        );
        let canvas_w = self.width as i32;
        let models = self.turnable_model_sources();
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
        let turn = AffineTransform {
            a: -1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: canvas_w as f32,
            f: 0.0,
        };
        self.record_turn_with_models("Flip Horizontal", Box::new(cmd), models, turn);
        self.flatten_full();
    }

    pub fn flip_vertical(&mut self) {
        use iai_history::command::{PixelTransformCommand, PixelTransformKind};
        let cmd = PixelTransformCommand::capture_before(
            "Flip Vertical",
            PixelTransformKind::FlipV,
            &self.layer_stack,
            self.width,
            self.height,
            &self.selection,
        );
        let canvas_h = self.height as i32;
        let models = self.turnable_model_sources();
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
        let turn = AffineTransform {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: -1.0,
            e: 0.0,
            f: canvas_h as f32,
        };
        self.record_turn_with_models("Flip Vertical", Box::new(cmd), models, turn);
        self.flatten_full();
    }

    pub fn rotate_90_cw(&mut self) {
        use iai_history::command::{PixelTransformCommand, PixelTransformKind};
        let cmd = PixelTransformCommand::capture_before(
            "Rotate 90 CW",
            PixelTransformKind::Rotate90CW,
            &self.layer_stack,
            self.width,
            self.height,
            &self.selection,
        );
        let canvas_h = self.height as i32;
        let models = self.turnable_model_sources();
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
        // (x, y) -> (H - y, x): a quarter turn clockwise on screen.
        let turn = AffineTransform {
            a: 0.0,
            b: 1.0,
            c: -1.0,
            d: 0.0,
            e: canvas_h as f32,
            f: 0.0,
        };
        self.record_turn_with_models("Rotate 90 CW", Box::new(cmd), models, turn);
        self.flatten_full();
    }

    pub fn rotate_90_ccw(&mut self) {
        use iai_history::command::{PixelTransformCommand, PixelTransformKind};
        let cmd = PixelTransformCommand::capture_before(
            "Rotate 90 CCW",
            PixelTransformKind::Rotate90CCW,
            &self.layer_stack,
            self.width,
            self.height,
            &self.selection,
        );
        let canvas_w = self.width as i32;
        let models = self.turnable_model_sources();
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
        // (x, y) -> (y, W - x): a quarter turn counter-clockwise on screen.
        let turn = AffineTransform {
            a: 0.0,
            b: -1.0,
            c: 1.0,
            d: 0.0,
            e: 0.0,
            f: canvas_w as f32,
        };
        self.record_turn_with_models("Rotate 90 CCW", Box::new(cmd), models, turn);
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
            let mut sel_cmd = iai_history::command::SelectionCommand::capture_before(
                "Resize Canvas",
                &self.selection,
            );
            self.selection.resize(new_w, new_h);
            sel_cmd.capture_after(&self.selection);
            self.record_as(Box::new(sel_cmd), ChangeKind::Selection);
        } else {
            self.selection.resize(new_w, new_h);
        }

        let resize_cmd = iai_history::command::ResizeCanvasCommand::capture_before(
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
            let mut new_tiles = iai_raster::tile::TileMap::new(new_w, new_h);
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
            let mut sel_cmd = iai_history::command::SelectionCommand::capture_before(
                "Image Size",
                &self.selection,
            );
            Self::resample_selection(&mut self.selection, new_w, new_h, sx, sy);
            sel_cmd.capture_after(&self.selection);
            self.record_as(Box::new(sel_cmd), ChangeKind::Selection);
        } else {
            self.selection.resize(new_w, new_h);
        }

        let mut cmd = iai_history::command::LayerStructureCommand::capture_before(
            "Image Size",
            &self.layer_stack,
            self.width,
            self.height,
        );

        // Paths, shapes and type are models in page pixels: they are scaled
        // and drawn again, not resampled, so they stay sharp and a later edit
        // of them keeps the new size.
        let scale = AffineTransform::scale(sx, sy);
        for layer in &mut self.layer_stack.layers {
            let source = layer.clone();
            let redrawn = layer.mask.is_none()
                && matches!(
                    layer.layer_type,
                    LayerType::Vector(VectorGeometry::Path(_) | VectorGeometry::Primitive(_))
                );
            let layer_new_w = ((layer.width as f32 * sx).round() as u32).max(1);
            let layer_new_h = ((layer.height as f32 * sy).round() as u32).max(1);
            layer.tiles = if redrawn {
                TileMap::new(layer_new_w, layer_new_h)
            } else {
                Self::resample_tilemap(&layer.tiles, layer_new_w, layer_new_h)
            };
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
            reconcile_affine_layer_model(layer, &source, scale);
        }

        self.width = new_w;
        self.height = new_h;
        self.metadata.resolution_ppi = new_dpi.max(1.0);
        // The flat composite is made again when it is next read.
        self.pixels = Vec::new();
        reconcile_crop_ink(self);
        cmd.capture_after(&self.layer_stack, self.width, self.height);
        self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
        self.end_undo_group();
        self.flatten_full();
        true
    }

    /// Bilinear resample `src` to `new_w × new_h`, tile-native (256-px chunks, no
    /// canvas-sized buffer) so Image Size works under Viewport Streaming.
    pub fn resample_tilemap(
        src: &iai_raster::tile::TileMap,
        new_w: u32,
        new_h: u32,
    ) -> iai_raster::tile::TileMap {
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
        selection: &mut iai_doc::selection::Selection,
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

#[cfg(test)]
mod page_change_tests {
    use super::*;
    use iai_doc::vector::color::ColorValue;
    use iai_doc::vector::object::VectorObjectData;
    use iai_doc::vector::path::{Contour, FillRule, Node, PathData};
    use iai_doc::vector::style::VectorStyle;
    use iai_geom::geometry::Point;

    /// A frame turned, moved and scaled down, as a crop to a print size.
    fn turned_frame() -> (AffineTransform, impl Fn(f32, f32) -> (f32, f32) + Sync, f32) {
        let (cx, cy, vw, vh, out_w, out_h) =
            (420.0f32, 300.0f32, 500.0f32, 380.0f32, 260u32, 200u32);
        let (tx, ty, angle) = (13.0f32, -7.5f32, 0.31f32);
        let (sx, sy) = (vw / out_w as f32, vh / out_h as f32);
        let (hw, hh) = (out_w as f32 * 0.5, out_h as f32 * 0.5);
        let forward = AffineTransform::translate(hw, hh)
            .then(&AffineTransform::scale(1.0 / sx, 1.0 / sy))
            .then(&AffineTransform::translate(tx, ty))
            .then(&AffineTransform::rotate(angle))
            .then(&AffineTransform::translate(-cx, -cy));
        let (cos, sin) = (angle.cos(), angle.sin());
        let map = move |u: f32, v: f32| {
            let (dx, dy) = ((u - hw) * sx + cx - cx - tx, (v - hh) * sy + cy - cy - ty);
            (dx * cos + dy * sin + cx, -dx * sin + dy * cos + cy)
        };
        (forward, map, sx.max(sy))
    }

    /// A blob of colour far from the page's middle, on a big layer.
    fn corner_blob(hdr: bool) -> TileMap {
        let (w, h) = (900u32, 700u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 380..470u32 {
            for x in 520..640u32 {
                let i = ((y * w + x) * 4) as usize;
                rgba[i..i + 4].copy_from_slice(&[(x % 256) as u8, (y % 256) as u8, 77, 200]);
            }
        }
        if hdr {
            let px16: Vec<u16> = rgba.iter().map(|&v| v as u16 * 257 + 3).collect();
            TileMap::from_rgba16(&px16, w, h)
        } else {
            TileMap::from_rgba(&rgba, w, h)
        }
    }

    #[test]
    fn the_reach_holds_every_pixel_the_layer_lands_on() {
        let (forward, map, footprint) = turned_frame();
        for hdr in [false, true] {
            for offset in [(0, 0), (-37, 22)] {
                let tiles = corner_blob(hdr);
                let shifted = |u: f32, v: f32| {
                    let (x, y) = map(u, v);
                    (x - offset.0 as f32, y - offset.1 as f32)
                };
                let reach = output_reach(&tiles, offset, &forward, 260, 200);
                assert!(reach.is_some());
                let all = Canvas::resample_footprint(
                    &tiles, 260, 200, shifted, None, footprint, false, None,
                );
                let within = Canvas::resample_footprint(
                    &tiles, 260, 200, shifted, None, footprint, false, reach,
                );
                let mut a = vec![0u8; 260 * 200 * 4];
                let mut b = vec![0u8; 260 * 200 * 4];
                all.flatten_tiles_region_into(0, 0, 260, 200, &mut a);
                within.flatten_tiles_region_into(0, 0, 260, 200, &mut b);
                assert!(a.iter().any(|&v| v != 0), "the blob lands on the frame");
                assert!(
                    a == b,
                    "hdr {hdr}, offset {offset:?}: the reach cut pixels off"
                );
                if hdr {
                    let mut a16 = vec![0u16; 260 * 200 * 4];
                    let mut b16 = vec![0u16; 260 * 200 * 4];
                    all.flatten16_region_into(0, 0, 260, 200, &mut a16);
                    within.flatten16_region_into(0, 0, 260, 200, &mut b16);
                    assert!(a16 == b16, "offset {offset:?}: 16-bit pixels differ");
                }
            }
        }
    }

    #[test]
    fn image_size_scales_a_path_and_keeps_it_a_path() {
        let mut canvas = Canvas::new(200, 100);
        let path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(40.0, 0.0)),
                    Node::sharp(Point::new(40.0, 20.0)),
                    Node::sharp(Point::new(0.0, 20.0)),
                ],
                true,
            )],
            FillRule::NonZero,
        );
        let object = VectorObjectData::new(
            path,
            VectorStyle::filled(ColorValue::rgb(0.0, 0.0, 1.0)),
            AffineTransform::translate(30.0, 20.0),
        );
        let idx = canvas.layer_stack.add_layer(200, 100);
        iai_vector::command_vector::apply_object_to_layer(
            &mut canvas.layer_stack.layers[idx],
            object,
        );
        assert!(canvas.resize_image(400, 200, 144.0));

        let layer = &canvas.layer_stack.layers[idx];
        let LayerType::Vector(VectorGeometry::Path(object)) = &layer.layer_type else {
            panic!("the path became pixels");
        };
        let bounds = object.layer_bounds(0.05).expect("path bounds");
        assert!((bounds.x - 60.0).abs() < 0.5 && (bounds.y - 40.0).abs() < 0.5);
        assert!((bounds.w - 80.0).abs() < 0.5 && (bounds.h - 40.0).abs() < 0.5);
        // What shows is the model drawn at the new size, not the old pixels
        // blown up: drawn again, it comes out the same.
        let mut redrawn = layer.clone();
        iai_vector::command_vector::apply_object_to_layer(&mut redrawn, object.clone());
        assert_eq!(redrawn.offset, layer.offset);
        assert_eq!(redrawn.flatten_tiles(), layer.flatten_tiles());

        canvas.undo().expect("undo Image Size");
        let LayerType::Vector(VectorGeometry::Path(object)) =
            &canvas.layer_stack.layers[idx].layer_type
        else {
            panic!("undo lost the path");
        };
        let bounds = object.layer_bounds(0.05).expect("path bounds");
        assert!((bounds.x - 30.0).abs() < 0.5 && (bounds.w - 40.0).abs() < 0.5);
    }

    #[test]
    fn a_change_made_on_a_copy_lands_with_its_undo_step() {
        let mut canvas = Canvas::new(900, 700);
        let idx = canvas.layer_stack.add_layer(900, 700);
        canvas.layer_stack.layers[idx].tiles = corner_blob(false);
        let before = canvas.layer_stack.layers[idx].flatten_tiles();
        let undo_steps = canvas.undo_count();

        let mut copy = canvas.page_work_copy();
        let stamp = canvas.page_stamp();
        assert!(copy.crop_transformed(560.0, 420.0, 400.0, 300.0, 200, 150, 0.0, 0.0, 0.2, true));
        assert_eq!(canvas.page_stamp(), stamp, "the copy is not the page");
        canvas.adopt_page_change(copy);

        assert_eq!((canvas.width, canvas.height), (200, 150));
        assert_eq!(canvas.undo_count(), undo_steps + 1, "one undo step");
        assert_ne!(canvas.page_stamp(), stamp);
        assert!(
            canvas.layer_stack.layers[idx]
                .flatten_tiles()
                .iter()
                .any(|&v| v != 0),
            "the blob is in the frame"
        );
        canvas.undo().expect("undo the crop");
        assert_eq!((canvas.width, canvas.height), (900, 700));
        assert_eq!(canvas.layer_stack.layers[idx].flatten_tiles(), before);
    }
}

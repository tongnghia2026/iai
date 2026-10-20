//! Editable layer models after an exact quarter turn or mirror.
//!
//! Rotate 90°/180° and Flip turn a layer's cached raster exactly (tile
//! rotation, integer offsets). Text, primitive shapes and Paths, however, are
//! re-rendered from their model on the next edit or transform, so the model has
//! to turn with the pixels — otherwise the layer snaps back to its old
//! orientation the moment anything re-renders it.

use super::*;
use iai_doc::layer::{Layer, LayerType};
use iai_doc::shape::ShapeData;
use iai_doc::vector::object::VectorGeometry;
use iai_geom::geometry::Point;
use iai_geom::vector::affine::AffineTransform;
use iai_history::gateway::ChangeKind;
use iai_text::text::TextData;

/// Rotation (degrees, clockwise on screen) of a quarter turn / mirror's linear
/// part once any mirror has been pulled out as a horizontal flip, plus whether
/// there was one. Entries of `turn` are expected to be exactly 0 or ±1.
fn quarter_turn_parts(turn: &AffineTransform) -> (f32, bool) {
    let mirrored = turn.a * turn.d - turn.b * turn.c < 0.0;
    // Undo the mirror (R·Fx·Fx = R) so the first column is R's.
    let (ca, cb) = if mirrored {
        (-turn.a, -turn.b)
    } else {
        (turn.a, turn.b)
    };
    let angle = if ca > 0.5 {
        0.0
    } else if ca < -0.5 {
        180.0
    } else if cb > 0.5 {
        90.0
    } else {
        270.0
    };
    (angle, mirrored)
}

/// Type turned by a quarter turn / mirror: same glyphs, new rotation and flips.
/// The rendered model is `R(rotation)·Flip`, so a mirror `R(θ)·Fx` reverses the
/// rotation and toggles the horizontal flip (the rule Free Transform uses).
pub fn turned_text_data(td: &TextData, turn: &AffineTransform) -> TextData {
    let (angle, mirrored) = quarter_turn_parts(turn);
    let mut next = td.clone();
    if mirrored {
        next.rotation_deg = (angle - td.rotation_deg).rem_euclid(360.0);
        next.flip_x = !next.flip_x;
    } else {
        next.rotation_deg = (td.rotation_deg + angle).rem_euclid(360.0);
    }
    next
}

/// A primitive shape whose box was carried by `turn`, expressed in the turned
/// layer's local space, or `None` when the shape's look does not survive the
/// turn (an odd-sided polygon on its side, a gradient that would stay put).
fn turned_shape_data(
    sd: &ShapeData,
    source_offset: (i32, i32),
    turned_offset: (i32, i32),
    turn: &AffineTransform,
) -> Option<ShapeData> {
    if !sd.survives_linear_map(turn.a, turn.b, turn.c, turn.d) {
        return None;
    }
    let (cx0, cy0, cx1, cy1) = sd.canvas_span(source_offset);
    let p0 = turn.apply_point(Point::new(cx0, cy0));
    let p1 = turn.apply_point(Point::new(cx1, cy1));
    let (ox, oy) = (turned_offset.0 as f32, turned_offset.1 as f32);
    let (mut x0, mut y0, mut x1, mut y1) = (p0.x - ox, p0.y - oy, p1.x - ox, p1.y - oy);
    if sd.kind.is_box_shape() {
        (x0, x1) = (x0.min(x1), x0.max(x1));
        (y0, y1) = (y0.min(y1), y0.max(y1));
    }
    Some(ShapeData {
        x0,
        y0,
        x1,
        y1,
        ..sd.clone()
    })
}

/// Bring an editable layer's model along with the canvas-space quarter turn or
/// mirror `turn`. `source` is the layer as it was before; `layer` already holds
/// the exactly turned tiles, offset, size and mask.
///
/// Type and shapes that survive keep that exact raster and only get their model
/// updated, so a mask stays aligned. A shape that cannot describe the turned
/// picture becomes a Path (or, under a mask tied to its raster, plain pixels);
/// a Path's model is turned and its raster re-derived from it.
pub fn turn_layer_model(layer: &mut Layer, source: &Layer, turn: &AffineTransform) {
    match &source.layer_type {
        LayerType::Text(td) => {
            layer.layer_type = LayerType::Text(turned_text_data(td, turn));
        }
        LayerType::Vector(VectorGeometry::Primitive(sd)) => {
            if let Some(next) = turned_shape_data(sd, source.offset, layer.offset, turn) {
                layer.layer_type = LayerType::Vector(VectorGeometry::Primitive(next));
            } else if layer.mask.is_some() {
                layer.layer_type = LayerType::Raster;
            } else {
                let mut object = sd.to_vector_object(source.offset);
                object.transform = turn.then(&object.transform);
                iai_vector::command_vector::apply_object_to_layer(layer, object);
            }
        }
        LayerType::Vector(VectorGeometry::Path(_)) => {
            // A pending Move-tool drag lives only in the offset; fold it in so
            // the turn starts from the displayed position.
            let mut synced = source.clone();
            iai_vector::command_vector::fold_offset_into_model(&mut synced);
            if let LayerType::Vector(VectorGeometry::Path(mut object)) = synced.layer_type {
                object.transform = turn.then(&object.transform);
                iai_vector::command_vector::apply_object_to_layer(layer, object);
            }
        }
        _ => {}
    }
}

/// Whether [`turn_layer_model`] has anything to do for this layer.
pub fn has_turnable_model(layer: &Layer) -> bool {
    matches!(layer.layer_type, LayerType::Text(_) | LayerType::Vector(_))
}

impl Canvas {
    /// Pre-turn copies of the layers whose model must follow a whole-canvas
    /// quarter turn or mirror.
    pub(crate) fn turnable_model_sources(&self) -> Vec<Layer> {
        self.layer_stack
            .layers
            .iter()
            .filter(|l| has_turnable_model(l))
            .cloned()
            .collect()
    }

    /// After the raster loop of a whole-canvas turn, turn every editable model
    /// in `sources` too and record that as its own step, grouped with the pixel
    /// step under `label` so one undo restores both.
    pub(crate) fn record_turn_with_models(
        &mut self,
        label: &str,
        pixel_step: Box<dyn iai_history::command::Command>,
        sources: Vec<Layer>,
        turn: AffineTransform,
    ) {
        let mut model_step = iai_history::command::FreeTransformCommand::new(label);
        let mut changed = false;
        for source in &sources {
            let Some(layer) = self
                .layer_stack
                .layers
                .iter_mut()
                .find(|l| l.id == source.id)
            else {
                continue;
            };
            let before = layer.clone();
            turn_layer_model(layer, source, &turn);
            model_step.add_layer(
                layer.id,
                before.layer_type,
                before.tiles,
                before.mask,
                before.width,
                before.height,
                before.offset,
                layer.layer_type.clone(),
                layer.tiles.clone(),
                layer.mask.clone(),
                layer.width,
                layer.height,
                layer.offset,
            );
            changed = true;
        }
        self.begin_undo_group(label);
        self.record_as(pixel_step, ChangeKind::LayerStructure);
        if changed {
            self.record_as(Box::new(model_step), ChangeKind::LayerStructure);
        }
        self.end_undo_group();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iai_doc::shape::ShapeKind;
    use iai_doc::vector::color::ColorValue;
    use iai_doc::vector::object::VectorObjectData;
    use iai_doc::vector::path::{Contour, FillRule, Node, PathData};
    use iai_doc::vector::style::VectorStyle;
    use iai_raster::tile::TileMap;
    use iai_text::text::rasterize_placed;

    fn ink_size(tiles: &TileMap) -> (i32, i32) {
        let (x0, y0, x1, y1) = tiles.content_bounds().expect("ink");
        (x1 - x0, y1 - y0)
    }

    fn layer(canvas: &Canvas, id: u32) -> &Layer {
        canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == id)
            .expect("layer")
    }

    fn add_text(canvas: &mut Canvas) -> u32 {
        let td = TextData {
            content: "Turn me".to_string(),
            font_px: 28.0,
            ..TextData::default()
        };
        let (raster, delta) = rasterize_placed(&td).expect("text raster");
        let idx = canvas.layer_stack.add_layer(raster.width, raster.height);
        let layer = &mut canvas.layer_stack.layers[idx];
        layer.tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
        layer.offset = (40 + delta.0, 50 + delta.1);
        layer.layer_type = LayerType::Text(td);
        layer.id
    }

    /// What re-rendering the layer's model would draw, as an ink size.
    fn model_ink_size(layer: &Layer) -> (i32, i32) {
        match &layer.layer_type {
            LayerType::Text(td) => {
                let (r, _) = rasterize_placed(td).expect("text raster");
                ink_size(&TileMap::from_rgba(&r.rgba, r.width, r.height))
            }
            LayerType::Vector(VectorGeometry::Primitive(sd)) => {
                let r = sd.render().expect("shape raster");
                ink_size(&TileMap::from_rgba(&r.rgba, r.width, r.height))
            }
            LayerType::Vector(VectorGeometry::Path(_)) => {
                let mut rebuilt = layer.clone();
                iai_vector::command_vector::rebuild_path_cache_from_model(&mut rebuilt);
                ink_size(&rebuilt.tiles)
            }
            _ => panic!("not an editable layer"),
        }
    }

    fn assert_model_matches(layer: &Layer, what: &str) {
        let shown = ink_size(&layer.tiles);
        let model = model_ink_size(layer);
        assert!(
            (shown.0 - model.0).abs() <= 2 && (shown.1 - model.1).abs() <= 2,
            "{what}: the model would draw {model:?} where the layer shows {shown:?}"
        );
    }

    #[test]
    fn image_rotate_turns_text_and_undo_redo_keep_model_and_pixels_together() {
        let mut canvas = Canvas::new(300, 200);
        let id = add_text(&mut canvas);
        let upright = ink_size(&layer(&canvas, id).tiles);

        canvas.rotate_90_cw();
        let turned = layer(&canvas, id);
        let LayerType::Text(td) = &turned.layer_type else {
            panic!("type stays editable");
        };
        assert!((td.rotation_deg - 90.0).abs() < 0.01);
        assert_eq!(ink_size(&turned.tiles), (upright.1, upright.0));
        assert_model_matches(turned, "after rotate");

        assert!(canvas.undo().is_some());
        let back = layer(&canvas, id);
        let LayerType::Text(td) = &back.layer_type else {
            panic!("type stays editable");
        };
        assert!(
            td.rotation_deg.abs() < 0.01,
            "undo restores the upright model"
        );
        assert_eq!(ink_size(&back.tiles), upright);
        assert_model_matches(back, "after undo");

        assert!(canvas.redo().is_some());
        let again = layer(&canvas, id);
        let LayerType::Text(td) = &again.layer_type else {
            panic!("type stays editable");
        };
        assert!(
            (td.rotation_deg - 90.0).abs() < 0.01,
            "redo turns the model again"
        );
        assert_model_matches(again, "after redo");
    }

    #[test]
    fn image_rotate_and_flip_keep_shapes_and_paths_true_to_their_pixels() {
        let mut canvas = Canvas::new(320, 240);
        let add_shape = |canvas: &mut Canvas, kind, sides| {
            let (mut sd, off) = ShapeData::from_canvas_span(
                kind,
                30.0,
                40.0,
                150.0,
                90.0,
                0.0,
                true,
                [200, 30, 30, 255],
                2.0,
                [0, 0, 0, 255],
            );
            sd.sides = sides;
            let r = sd.render().expect("shape raster");
            let idx = canvas.layer_stack.add_layer(r.width, r.height);
            let layer = &mut canvas.layer_stack.layers[idx];
            layer.tiles = TileMap::from_rgba(&r.rgba, r.width, r.height);
            layer.offset = off;
            layer.layer_type = LayerType::Vector(VectorGeometry::Primitive(sd));
            layer.id
        };
        let rect = add_shape(&mut canvas, ShapeKind::Rectangle, 5);
        let triangle = add_shape(&mut canvas, ShapeKind::Polygon, 3);
        let path = {
            let object = VectorObjectData::new(
                PathData::new(
                    vec![Contour::new(
                        vec![
                            Node::sharp(Point::new(0.0, 0.0)),
                            Node::sharp(Point::new(90.0, 0.0)),
                            Node::sharp(Point::new(0.0, 30.0)),
                        ],
                        true,
                    )],
                    FillRule::NonZero,
                ),
                VectorStyle::filled(ColorValue::rgb(0.0, 0.4, 1.0)),
                AffineTransform::translate(170.0, 120.0),
            );
            let idx = canvas.layer_stack.add_layer(1, 1);
            let layer = &mut canvas.layer_stack.layers[idx];
            iai_vector::command_vector::apply_object_to_layer(layer, object);
            layer.id
        };

        canvas.rotate_90_cw();
        for id in [rect, triangle, path] {
            assert_model_matches(layer(&canvas, id), "after rotate");
        }
        assert!(matches!(
            layer(&canvas, rect).layer_type,
            LayerType::Vector(VectorGeometry::Primitive(_))
        ));
        assert!(
            matches!(
                layer(&canvas, triangle).layer_type,
                LayerType::Vector(VectorGeometry::Path(_))
            ),
            "a triangle on its side cannot stay a pointing-up ShapeData"
        );

        canvas.flip_vertical();
        for id in [rect, triangle, path] {
            assert_model_matches(layer(&canvas, id), "after flip");
        }
        canvas.flip_horizontal();
        canvas.rotate_90_ccw();
        for id in [rect, triangle, path] {
            assert_model_matches(layer(&canvas, id), "after the round trip");
        }
    }
}

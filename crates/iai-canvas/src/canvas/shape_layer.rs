//! A new shape layer: a rectangle, ellipse, line, polygon or star that stays
//! editable (its geometry and style are kept; its pixels are made from them).

use iai_doc::layer::LayerType;
use iai_doc::shape::ShapeData;
use iai_doc::vector::object::VectorGeometry;
use iai_history::command::LayerStructureCommand;
use iai_raster::tile::TileMap;

use super::Canvas;

impl Canvas {
    /// Add `shape`, its raster at `offset`, as a new layer above the active
    /// one, made the active and only selected layer; one undo step. Its index,
    /// or `None` when the shape draws nothing.
    pub fn add_shape_layer(&mut self, shape: ShapeData, offset: (i32, i32)) -> Option<usize> {
        let raster = shape.render()?;
        let (cw, ch) = (self.width, self.height);
        let mut cmd = LayerStructureCommand::capture_before("Shape", &self.layer_stack, cw, ch);
        let idx = self.layer_stack.add_layer(cw, ch);

        let mut tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
        // Keep the layer ink-valid on CMYK documents (mirror → ink).
        if let Some(conv) = self.cmyk_converter() {
            tiles.encode_ink_from_mirror(&conv);
        }
        let layer = self.layer_stack.layers.get_mut(idx)?;
        layer.tiles = tiles;
        layer.width = raster.width;
        layer.height = raster.height;
        layer.offset = offset;
        layer.name = shape.kind.label().to_string();
        layer.layer_type = LayerType::Vector(VectorGeometry::Primitive(shape));

        cmd.capture_after(&self.layer_stack, cw, ch);
        self.record(Box::new(cmd));
        // The new shape alone is selected, so the Move tool's transform box
        // shows on it without another click.
        for (i, l) in self.layer_stack.layers.iter_mut().enumerate() {
            l.selected = i == idx;
        }
        self.layer_revision += 1;
        Some(idx)
    }
}

//! A text layer's pixels, made from its text.

use iai_doc::layer::LayerType;
use iai_doc::model::text::TextData;
use iai_raster::tile::TileMap;
use iai_text::text::rasterize_placed;

use super::Canvas;

/// Rasterize `td` and replace the layer's tiles with it. The text is written at
/// tile-local (0,0); `origin` is the upright text anchor, and rotated tiles
/// receive a bbox offset. Also updates the layer name from the content.
///
/// The tiles are raster-sized (not canvas-sized): a canvas-sized TileMap would
/// silently crop text extending past the canvas edge, and the transform-commit
/// path already produces raster-sized text layers — width/height must track
/// the tiles for hit-testing and .iai export.
pub fn rasterize_into_layer(canvas: &mut Canvas, idx: usize, origin: (i32, i32), td: TextData) {
    let (cw, ch) = (canvas.width, canvas.height);
    let (tiles, w, h, offset) = match rasterize_placed(&td) {
        Some((raster, delta)) => (
            TileMap::from_rgba(&raster.rgba, raster.width, raster.height),
            raster.width,
            raster.height,
            (
                origin.0.saturating_add(delta.0),
                origin.1.saturating_add(delta.1),
            ),
        ),
        None => (TileMap::new(cw, ch), cw, ch, origin),
    };
    let name = layer_name_from(&td.content);
    let layer = &mut canvas.layer_stack.layers[idx];
    layer.tiles = tiles;
    layer.width = w;
    layer.height = h;
    layer.offset = offset;
    layer.name = name;
    layer.layer_type = LayerType::Text(td);
}

/// First line of the text, trimmed and truncated, for the layer panel name.
pub fn layer_name_from(content: &str) -> String {
    let first = content.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        return "Text".to_string();
    }
    let truncated: String = first.chars().take(24).collect();
    truncated
}

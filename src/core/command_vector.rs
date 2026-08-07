//! Vector document commands (Bước 4 / T4.2), kept OUT of the already-large
//! `command.rs` per the pre-planned module layout (Mục 3.10).
//!
//! Every persistent Path change runs through one of these and is recorded via the
//! gateway (`Canvas::execute` → `HistoryGate`), never by pushing onto history
//! directly (Mục 3.9 / 8). The model ([`VectorObjectData`]) is the source of
//! truth; the `Layer::tiles` raster is a CACHE these commands re-derive from the
//! model in both `execute` and `undo` (Mục 3.2 / 3.7). Because the cache is
//! re-derived, node/style/transform commands store only the small model delta —
//! never a TileMap snapshot and never a whole-document clone (Mục 8, T4.2 DoD).
//!
//! CMYK note: the rasteriser emits the RGB mirror; the document's ink planes for
//! Path layers are re-derived by [`Canvas::reconcile_path_ink`] after the gateway
//! runs, since the ICC converter lives on the canvas, not in an `EditContext`.

use crate::core::command::{Command, EditContext};
use crate::core::layer::{Layer, LayerType};
use crate::core::tile::TileMap;
use crate::core::vector::affine::AffineTransform;
use crate::core::vector::object::{VectorGeometry, VectorObjectData};
use crate::core::vector::path::PathData;
use crate::core::vector::raster;
use crate::core::vector::style::VectorStyle;

/// Set a Path layer's model AND rebuild its raster cache from that model. The
/// raster (tiles/width/height/offset) is derived state, so this fully replaces it
/// — no cache is stored in history. On a CMYK document the RGB mirror written here
/// is turned into ink planes afterwards by [`Canvas::reconcile_path_ink`].
///
/// Shared with [`crate::core::canvas::Canvas::rebuild_path_caches`] (rebuild the
/// cache from the model on load) so both paths derive tiles identically.
pub(crate) fn apply_object_to_layer(layer: &mut Layer, object: VectorObjectData) {
    match raster::rasterize(&object) {
        Some(r) => {
            layer.tiles = TileMap::from_rgba(&r.rgba, r.width, r.height);
            layer.width = r.width;
            layer.height = r.height;
            layer.offset = r.offset;
        }
        None => {
            // No visible fill/outline: keep a valid but empty 1×1 raster.
            layer.tiles = TileMap::new(1, 1);
            layer.width = 1;
            layer.height = 1;
        }
    }
    layer.layer_type = LayerType::Vector(VectorGeometry::Path(object));
}

/// Reconcile a Path layer's `Layer::offset` INTO its model, then re-derive the
/// raster. A Move-tool drag shifts `offset` (the general layer-move path) without
/// touching the model, but the model is the source of truth for position and the
/// offset is otherwise DERIVED from it (`apply_object_to_layer` sets it to the
/// model's raster origin). This bakes any such drag into the model transform so
/// the two agree again — at the SAME on-canvas position — which is what lets a
/// dragged Path survive a reload and lets a later rotate pivot about the displayed
/// centre. A no-op offset (already in sync) still re-derives the raster from the
/// model, so this doubles as the cache-rebuild step. Called on load and before a
/// model edit that must respect a pending drag.
pub(crate) fn fold_offset_into_model(layer: &mut Layer) {
    fold_offset_impl(layer, false);
}

/// [`fold_offset_into_model`] that ALWAYS re-derives the raster from the model,
/// even when the offset is already in sync. Load-time cache rebuild uses this so
/// a reopened document renders from the model rather than trusting the baked
/// raster fallback; interactive gesture-begin call sites use the plain variant,
/// whose in-sync fast path skips the O(area) re-raster entirely.
pub(crate) fn rebuild_path_cache_from_model(layer: &mut Layer) {
    fold_offset_impl(layer, true);
}

fn fold_offset_impl(layer: &mut Layer, force_rebuild: bool) {
    let LayerType::Vector(VectorGeometry::Path(obj)) = &layer.layer_type else {
        return;
    };
    // The raster ORIGIN is a function of the flattened bounds alone —
    // `raster_geometry` computes it in O(nodes). The old code called
    // `rasterize` (O(area)) just to learn it, then `apply_object_to_layer`
    // rasterized AGAIN: two full CPU renders of e.g. a page-sized gradient on
    // every transform/gradient gesture press, the reported first-click lag.
    let Some((origin, rw, rh)) = raster::raster_geometry(obj) else {
        return;
    };
    let dx = (layer.offset.0 - origin.0) as f32;
    let dy = (layer.offset.1 - origin.1) as f32;
    if dx == 0.0 && dy == 0.0 && layer.width == rw && layer.height == rh && !force_rebuild {
        // Offset and cache dimensions already agree with the model — the cache
        // IS the derived raster; nothing to fold and nothing to rebuild.
        return;
    }
    let obj = obj.clone();
    let obj = if dx != 0.0 || dy != 0.0 {
        VectorObjectData {
            transform: AffineTransform::translate(dx, dy).then(&obj.transform),
            ..obj
        }
    } else {
        obj
    };
    apply_object_to_layer(layer, obj);
}

/// The current Path model on layer `id`, or an error if it is missing / not a
/// Path layer. Shared by the model-edit commands.
fn path_object(ctx: &EditContext, id: u32) -> Result<VectorObjectData, String> {
    let layer = ctx
        .layers
        .layers
        .iter()
        .find(|l| l.id == id)
        .ok_or_else(|| format!("Layer {id} not found"))?;
    match &layer.layer_type {
        LayerType::Vector(VectorGeometry::Path(obj)) => Ok(obj.clone()),
        _ => Err(format!("Layer {id} is not a Path layer")),
    }
}

fn set_path_object(ctx: &mut EditContext, id: u32, object: VectorObjectData) -> Result<(), String> {
    let layer = ctx
        .layers
        .layers
        .iter_mut()
        .find(|l| l.id == id)
        .ok_or_else(|| format!("Layer {id} not found"))?;
    apply_object_to_layer(layer, object);
    Ok(())
}

fn vector_style(ctx: &EditContext, id: u32) -> Result<VectorStyle, String> {
    let layer = ctx
        .layers
        .layers
        .iter()
        .find(|layer| layer.id == id)
        .ok_or_else(|| format!("Layer {id} not found"))?;
    match &layer.layer_type {
        LayerType::Vector(VectorGeometry::Path(object)) => Ok(object.style),
        LayerType::Vector(VectorGeometry::Primitive(shape)) => Ok(shape.style),
        _ => Err(format!("Layer {id} is not a Vector layer")),
    }
}

pub(crate) fn apply_style_to_layer(layer: &mut Layer, style: VectorStyle) -> Result<(), String> {
    match &layer.layer_type {
        LayerType::Vector(VectorGeometry::Path(object)) => {
            let mut object = object.clone();
            object.style = style;
            apply_object_to_layer(layer, object);
            Ok(())
        }
        LayerType::Vector(VectorGeometry::Primitive(shape)) => {
            let (x0, y0, x1, y1) = shape.canvas_span(layer.offset);
            let (mut next, offset) = crate::core::shape::ShapeData::from_canvas_span_with_style(
                shape.kind,
                x0,
                y0,
                x1,
                y1,
                shape.corner_radius,
                style,
            );
            // `from_canvas_span_with_style` fills these with defaults — the caller
            // MUST copy the shape's own values back or a style change (e.g. a
            // palette fill) silently resets the rectangle corner style, polygon
            // sides and star inner-radius.
            next.corner_type = shape.corner_type;
            next.sides = shape.sides;
            next.star_inner = shape.star_inner;
            let raster = next
                .render()
                .ok_or_else(|| "primitive vector rasterization failed".to_string())?;
            layer.tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
            layer.width = raster.width;
            layer.height = raster.height;
            layer.offset = offset;
            layer.layer_type = LayerType::Vector(VectorGeometry::Primitive(next));
            Ok(())
        }
        _ => Err(format!("Layer {} is not a Vector layer", layer.id)),
    }
}

fn set_vector_style(ctx: &mut EditContext, id: u32, style: VectorStyle) -> Result<(), String> {
    let layer = ctx
        .layers
        .layers
        .iter_mut()
        .find(|layer| layer.id == id)
        .ok_or_else(|| format!("Layer {id} not found"))?;
    apply_style_to_layer(layer, style)
}

// ── CreatePathLayer ──────────────────────────────────────────────────────────

/// Add a new Path layer above the active layer. Undo removes it.
pub struct CreatePathLayer {
    object: VectorObjectData,
    name: String,
    // Filled by `execute` for `undo`:
    created_id: Option<u32>,
    prev_active: usize,
}

impl CreatePathLayer {
    pub fn new(object: VectorObjectData, name: impl Into<String>) -> Self {
        Self {
            object,
            name: name.into(),
            created_id: None,
            prev_active: 0,
        }
    }

    /// The layer id assigned by the most recent `execute` (for the caller to
    /// select the new layer). `None` before execution.
    pub fn created_id(&self) -> Option<u32> {
        self.created_id
    }
}

impl Command for CreatePathLayer {
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        self.object.validate()?;
        self.prev_active = ctx.layers.active_idx;
        let (w, h) = (*ctx.canvas_width, *ctx.canvas_height);
        let idx = ctx.layers.add_layer(w, h);
        let layer = &mut ctx.layers.layers[idx];
        layer.name = self.name.clone();
        self.created_id = Some(layer.id);
        apply_object_to_layer(layer, self.object.clone());
        Ok(())
    }

    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        if let Some(id) = self.created_id {
            if let Some(pos) = ctx.layers.layers.iter().position(|l| l.id == id) {
                ctx.layers.layers.remove(pos);
            }
        }
        ctx.layers.active_idx = self
            .prev_active
            .min(ctx.layers.layers.len().saturating_sub(1));
        Ok(())
    }

    fn label(&self) -> &str {
        "Create Path"
    }

    fn memory_bytes(&self) -> usize {
        // Only the small model is retained; the raster lives on the live layer.
        std::mem::size_of::<VectorObjectData>() + self.object.path.total_nodes() * 40
    }
}

// ── DeletePathLayer ──────────────────────────────────────────────────────────

/// Remove a Path layer by id. Undo restores the whole layer at its old index.
pub struct DeletePathLayer {
    layer_id: u32,
    // Filled by `execute`: the removed layer and where it sat.
    removed: Option<(usize, Box<Layer>)>,
    prev_active: usize,
}

impl DeletePathLayer {
    pub fn new(layer_id: u32) -> Self {
        Self {
            layer_id,
            removed: None,
            prev_active: 0,
        }
    }
}

impl Command for DeletePathLayer {
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        let pos = ctx
            .layers
            .layers
            .iter()
            .position(|l| l.id == self.layer_id)
            .ok_or_else(|| format!("Layer {} not found", self.layer_id))?;
        if !matches!(
            ctx.layers.layers[pos].layer_type,
            LayerType::Vector(VectorGeometry::Path(_))
        ) {
            return Err(format!("Layer {} is not a Path layer", self.layer_id));
        }
        self.prev_active = ctx.layers.active_idx;
        let layer = ctx.layers.layers.remove(pos);
        self.removed = Some((pos, Box::new(layer)));
        ctx.layers.active_idx = ctx
            .layers
            .active_idx
            .min(ctx.layers.layers.len().saturating_sub(1));
        Ok(())
    }

    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        if let Some((pos, layer)) = self.removed.take() {
            let pos = pos.min(ctx.layers.layers.len());
            ctx.layers.layers.insert(pos, *layer);
            ctx.layers.active_idx = self.prev_active.min(ctx.layers.layers.len() - 1);
        }
        Ok(())
    }

    fn label(&self) -> &str {
        "Delete Path"
    }

    fn memory_bytes(&self) -> usize {
        self.removed
            .as_ref()
            .map_or(0, |(_, l)| l.tiles.tiles.len() * 64)
    }
}

// ── ReplacePathGeometry ──────────────────────────────────────────────────────

/// Replace a Path layer's geometry (used by the Node tool later), keeping its
/// style and transform. Stores only the before/after [`PathData`].
pub struct ReplacePathGeometry {
    layer_id: u32,
    new_path: PathData,
    old_path: Option<PathData>,
}

impl ReplacePathGeometry {
    pub fn new(layer_id: u32, new_path: PathData) -> Self {
        Self {
            layer_id,
            new_path,
            old_path: None,
        }
    }
}

impl Command for ReplacePathGeometry {
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        self.new_path.validate()?;
        let mut obj = path_object(ctx, self.layer_id)?;
        self.old_path = Some(std::mem::replace(&mut obj.path, self.new_path.clone()));
        set_path_object(ctx, self.layer_id, obj)
    }

    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        let old = self.old_path.clone().ok_or("nothing to undo")?;
        let mut obj = path_object(ctx, self.layer_id)?;
        obj.path = old;
        set_path_object(ctx, self.layer_id, obj)
    }

    fn label(&self) -> &str {
        "Edit Path"
    }

    fn memory_bytes(&self) -> usize {
        self.new_path.total_nodes() * 40
            + self.old_path.as_ref().map_or(0, |p| p.total_nodes() * 40)
    }
}

// ── ChangeVectorStyle ────────────────────────────────────────────────────────

/// Change a unified Vector layer's fill/outline/opacity, keeping either its
/// parametric primitive or Bézier geometry editable.
pub struct ChangeVectorStyle {
    layer_id: u32,
    new_style: VectorStyle,
    old_style: Option<VectorStyle>,
}

impl ChangeVectorStyle {
    pub fn new(layer_id: u32, new_style: VectorStyle) -> Self {
        Self {
            layer_id,
            new_style,
            old_style: None,
        }
    }

    /// History record for a style mutation whose final model/cache has already
    /// been applied by an interactive preview. This avoids rewinding and
    /// rasterizing the baseline just to let `execute` capture it.
    pub fn already_applied(layer_id: u32, old_style: VectorStyle, new_style: VectorStyle) -> Self {
        Self {
            layer_id,
            new_style,
            old_style: Some(old_style),
        }
    }
}

impl Command for ChangeVectorStyle {
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        self.new_style.validate()?;
        self.old_style = Some(vector_style(ctx, self.layer_id)?);
        set_vector_style(ctx, self.layer_id, self.new_style)
    }

    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        let old = self.old_style.ok_or("nothing to undo")?;
        set_vector_style(ctx, self.layer_id, old)
    }

    fn label(&self) -> &str {
        "Change Style"
    }
}

// ── ChangeVectorTransform ────────────────────────────────────────────────────

/// Change a Path layer's object transform (move/scale/rotate) without baking node
/// coordinates (Mục 3.4 / Giai đoạn 5 groundwork).
pub struct ChangeVectorTransform {
    layer_id: u32,
    new_transform: AffineTransform,
    old_transform: Option<AffineTransform>,
}

impl ChangeVectorTransform {
    pub fn new(layer_id: u32, new_transform: AffineTransform) -> Self {
        Self {
            layer_id,
            new_transform,
            old_transform: None,
        }
    }
}

impl Command for ChangeVectorTransform {
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        if !self.new_transform.is_finite() {
            return Err("transform is non-finite".into());
        }
        let mut obj = path_object(ctx, self.layer_id)?;
        self.old_transform = Some(obj.transform);
        obj.transform = self.new_transform;
        set_path_object(ctx, self.layer_id, obj)
    }

    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        let old = self.old_transform.ok_or("nothing to undo")?;
        let mut obj = path_object(ctx, self.layer_id)?;
        obj.transform = old;
        set_path_object(ctx, self.layer_id, obj)
    }

    fn label(&self) -> &str {
        "Transform Path"
    }
}

// ── RasterizeVectorLayer ─────────────────────────────────────────────────────

/// Bake a Path layer down to a plain Raster layer (Mục 3.7 / T5.3): the rendered
/// tiles are kept as-is (the raster is already the cache), only the vector model
/// is dropped so raster tools may paint directly. This is the explicit, opt-in
/// Rasterize policy — raster tools never silently rasterize a vector layer
/// (Mục 3.2). Undo restores the model; the tiles never change, so it is visually
/// a no-op in both directions.
pub struct RasterizeVectorLayer {
    layer_id: u32,
    model: Option<VectorObjectData>,
}

impl RasterizeVectorLayer {
    pub fn new(layer_id: u32) -> Self {
        Self {
            layer_id,
            model: None,
        }
    }
}

impl Command for RasterizeVectorLayer {
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        let layer = ctx
            .layers
            .layers
            .iter_mut()
            .find(|l| l.id == self.layer_id)
            .ok_or_else(|| format!("Layer {} not found", self.layer_id))?;
        match std::mem::replace(&mut layer.layer_type, LayerType::Raster) {
            LayerType::Vector(VectorGeometry::Path(obj)) => {
                self.model = Some(obj);
                Ok(())
            }
            other => {
                // Not a Path layer: put the type back and refuse.
                layer.layer_type = other;
                Err(format!("Layer {} is not a Path layer", self.layer_id))
            }
        }
    }

    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        let obj = self.model.clone().ok_or("nothing to undo")?;
        let layer = ctx
            .layers
            .layers
            .iter_mut()
            .find(|l| l.id == self.layer_id)
            .ok_or_else(|| format!("Layer {} not found", self.layer_id))?;
        layer.layer_type = LayerType::Vector(VectorGeometry::Path(obj));
        Ok(())
    }

    fn label(&self) -> &str {
        "Rasterize Path"
    }

    fn memory_bytes(&self) -> usize {
        self.model.as_ref().map_or(0, |o| o.path.total_nodes() * 40)
    }
}

// ── ExpandVectorStroke ───────────────────────────────────────────────────────

/// Expand a Vector Brush stroke into a closed-outline Path (Phase 6B "Expand
/// Stroke"): replaces the object's open centerline + brush appearance with a
/// closed filled outline a Boolean/fill can consume. The whole object changes
/// (geometry AND `brush` drop together), so undo restores the entire old object.
pub struct ExpandVectorStroke {
    layer_id: u32,
    old: Option<VectorObjectData>,
}

impl ExpandVectorStroke {
    pub fn new(layer_id: u32) -> Self {
        Self {
            layer_id,
            old: None,
        }
    }
}

impl Command for ExpandVectorStroke {
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        let obj = path_object(ctx, self.layer_id)?;
        let brush = obj
            .brush
            .clone()
            .ok_or_else(|| format!("Layer {} is not a brush stroke", self.layer_id))?;
        let outline = crate::core::vector::brush::expand_stroke(&obj.path, &brush)
            .ok_or_else(|| "brush stroke has no expandable outline".to_string())?;
        let new_obj = VectorObjectData {
            path: outline,
            style: obj.style,
            transform: obj.transform,
            brush: None,
        };
        new_obj.validate()?;
        self.old = Some(obj);
        set_path_object(ctx, self.layer_id, new_obj)
    }

    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        let old = self.old.clone().ok_or("nothing to undo")?;
        set_path_object(ctx, self.layer_id, old)
    }

    fn label(&self) -> &str {
        "Expand Stroke"
    }

    fn memory_bytes(&self) -> usize {
        self.old.as_ref().map_or(0, |o| o.path.total_nodes() * 40)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::Canvas;
    use crate::core::gateway::ChangeKind;
    use crate::core::geometry::Point;
    use crate::core::vector::color::ColorValue;
    use crate::core::vector::path::{Contour, FillRule, Node};

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
        use crate::core::vector::brush::BrushStroke;
        use crate::core::vector::style::LineCap;

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

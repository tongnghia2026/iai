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

use crate::vector::affine::AffineTransform;
use crate::vector::object::{VectorGeometry, VectorObjectData};
use crate::vector::path::PathData;
use crate::vector::raster;
use crate::vector::style::VectorStyle;
use iai_doc::layer::{Layer, LayerType};
use iai_history::command::{Command, EditContext};
use iai_raster::tile::TileMap;

/// Set a Path layer's model AND rebuild its raster cache from that model. The
/// raster (tiles/width/height/offset) is derived state, so this fully replaces it
/// — no cache is stored in history. On a CMYK document the RGB mirror written here
/// is turned into ink planes afterwards by [`Canvas::reconcile_path_ink`].
///
/// Shared with [`crate::core::canvas::Canvas::rebuild_path_caches`] (rebuild the
/// cache from the model on load) so both paths derive tiles identically.
pub fn apply_object_to_layer(layer: &mut Layer, object: VectorObjectData) {
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
pub fn fold_offset_into_model(layer: &mut Layer) {
    fold_offset_impl(layer, false);
}

/// [`fold_offset_into_model`] that ALWAYS re-derives the raster from the model,
/// even when the offset is already in sync. Load-time cache rebuild uses this so
/// a reopened document renders from the model rather than trusting the baked
/// raster fallback; interactive gesture-begin call sites use the plain variant,
/// whose in-sync fast path skips the O(area) re-raster entirely.
pub fn rebuild_path_cache_from_model(layer: &mut Layer) {
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

pub fn apply_style_to_layer(layer: &mut Layer, style: VectorStyle) -> Result<(), String> {
    match &layer.layer_type {
        LayerType::Vector(VectorGeometry::Path(object)) => {
            let mut object = object.clone();
            object.style = style;
            apply_object_to_layer(layer, object);
            Ok(())
        }
        LayerType::Vector(VectorGeometry::Primitive(shape)) => {
            let (x0, y0, x1, y1) = shape.canvas_span(layer.offset);
            let (mut next, offset) = iai_doc::shape::ShapeData::from_canvas_span_with_style(
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
    /// Optional dynamic-connector binding stamped onto the new layer in `execute`
    /// (so it survives redo, not just the initial create).
    connector: Option<iai_doc::connector::ConnectorBinding>,
    // Filled by `execute` for `undo`:
    created_id: Option<u32>,
    prev_active: usize,
}

impl CreatePathLayer {
    pub fn new(object: VectorObjectData, name: impl Into<String>) -> Self {
        Self {
            object,
            name: name.into(),
            connector: None,
            created_id: None,
            prev_active: 0,
        }
    }

    /// Stamp a dynamic-connector binding onto the created layer.
    pub fn with_connector(
        mut self,
        connector: Option<iai_doc::connector::ConnectorBinding>,
    ) -> Self {
        self.connector = connector;
        self
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
        layer.connector = self.connector;
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
        let outline = crate::vector::brush::expand_stroke(&obj.path, &brush)
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

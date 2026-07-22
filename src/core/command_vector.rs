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
use crate::core::vector::object::VectorObjectData;
use crate::core::vector::path::PathData;
use crate::core::vector::raster;
use crate::core::vector::style::VectorStyle;

/// Set a Path layer's model AND rebuild its raster cache from that model. The
/// raster (tiles/width/height/offset) is derived state, so this fully replaces it
/// — no cache is stored in history. On a CMYK document the RGB mirror written here
/// is turned into ink planes afterwards by [`Canvas::reconcile_path_ink`].
fn apply_object_to_layer(layer: &mut Layer, object: VectorObjectData) {
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
    layer.layer_type = LayerType::Path(object);
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
        LayerType::Path(obj) => Ok(obj.clone()),
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
        if !matches!(ctx.layers.layers[pos].layer_type, LayerType::Path(_)) {
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

/// Change a Path layer's fill/outline/opacity, keeping geometry and transform.
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
}

impl Command for ChangeVectorStyle {
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        self.new_style.validate()?;
        let mut obj = path_object(ctx, self.layer_id)?;
        self.old_style = Some(obj.style);
        obj.style = self.new_style;
        set_path_object(ctx, self.layer_id, obj)
    }

    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        let old = self.old_style.ok_or("nothing to undo")?;
        let mut obj = path_object(ctx, self.layer_id)?;
        obj.style = old;
        set_path_object(ctx, self.layer_id, obj)
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
            .filter(|l| matches!(l.layer_type, LayerType::Path(_)))
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
            .find(|l| matches!(l.layer_type, LayerType::Path(_)))
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
            LayerType::Path(o) => o.style,
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

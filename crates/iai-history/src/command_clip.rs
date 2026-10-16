//! Undoable clipping/attachment commands (PowerClip foundation T6.2).
//!
//! These are structural commands and deliberately enter history through
//! `Canvas::execute`. A first mask/paint payload can be supplied to the create
//! command so attachment creation and the first mark are one undo transaction.

use crate::command::{Command, EditContext};
use iai_doc::layer::{LayerStack, PaintTarget};
use iai_raster::tile::TileMap;

#[derive(Clone)]
struct StackState {
    stack: LayerStack,
}

impl StackState {
    fn capture(stack: &LayerStack) -> Self {
        Self {
            stack: stack.clone(),
        }
    }

    fn restore(&self, stack: &mut LayerStack) {
        *stack = self.stack.clone();
    }
}

pub struct CreateClippedPixelChild {
    frame_id: u32,
    width: u32,
    height: u32,
    initial_tiles: Option<TileMap>,
    before: Option<StackState>,
    after: Option<StackState>,
    created_id: Option<u32>,
}

impl CreateClippedPixelChild {
    pub fn new(frame_id: u32, width: u32, height: u32) -> Self {
        Self {
            frame_id,
            width,
            height,
            initial_tiles: None,
            before: None,
            after: None,
            created_id: None,
        }
    }

    /// Attach pixels produced by the first brush gesture atomically with layer
    /// creation. Undo then removes both the mark and the new child.
    pub fn with_initial_tiles(mut self, tiles: TileMap) -> Self {
        self.initial_tiles = Some(tiles);
        self
    }

    pub fn created_id(&self) -> Option<u32> {
        self.created_id
    }
}

impl Command for CreateClippedPixelChild {
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        if let Some(after) = &self.after {
            after.restore(ctx.layers);
            return Ok(());
        }
        let before = StackState::capture(ctx.layers);
        let id = ctx
            .layers
            .create_clipped_pixel_child(self.frame_id, self.width, self.height)?;
        if let Some(tiles) = &self.initial_tiles {
            let child = ctx
                .layers
                .layers
                .iter_mut()
                .find(|layer| layer.id == id)
                .ok_or_else(|| "Created PowerClip child disappeared".to_string())?;
            child.tiles = tiles.clone();
        }
        self.created_id = Some(id);
        self.before = Some(before);
        self.after = Some(StackState::capture(ctx.layers));
        Ok(())
    }

    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        self.before
            .as_ref()
            .ok_or_else(|| "PowerClip command was not executed".to_string())?
            .restore(ctx.layers);
        Ok(())
    }

    fn label(&self) -> &str {
        "Create Pixel Paint inside"
    }
}

pub struct ReleaseClippedChild {
    child_id: u32,
    before: Option<StackState>,
    after: Option<StackState>,
}

impl ReleaseClippedChild {
    pub fn new(child_id: u32) -> Self {
        Self {
            child_id,
            before: None,
            after: None,
        }
    }
}

impl Command for ReleaseClippedChild {
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        if let Some(after) = &self.after {
            after.restore(ctx.layers);
            return Ok(());
        }
        let before = StackState::capture(ctx.layers);
        ctx.layers.release_clipped_child(self.child_id)?;
        self.before = Some(before);
        self.after = Some(StackState::capture(ctx.layers));
        Ok(())
    }

    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        self.before
            .as_ref()
            .ok_or_else(|| "Release command was not executed".to_string())?
            .restore(ctx.layers);
        Ok(())
    }

    fn label(&self) -> &str {
        "Release PowerClip Content"
    }
}

pub struct CreateOrAttachRasterMask {
    layer_id: u32,
    white: bool,
    initial_tiles: Option<TileMap>,
    before: Option<StackState>,
    after: Option<StackState>,
}

impl CreateOrAttachRasterMask {
    pub fn new(layer_id: u32, white: bool) -> Self {
        Self {
            layer_id,
            white,
            initial_tiles: None,
            before: None,
            after: None,
        }
    }

    /// Supply the mask including the first eraser gesture. This keeps automatic
    /// mask creation and that gesture atomic in history.
    pub fn with_initial_tiles(mut self, tiles: TileMap) -> Self {
        self.initial_tiles = Some(tiles);
        self
    }
}

impl Command for CreateOrAttachRasterMask {
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        if let Some(after) = &self.after {
            after.restore(ctx.layers);
            return Ok(());
        }
        let before = StackState::capture(ctx.layers);
        let layer = ctx
            .layers
            .layers
            .iter_mut()
            .find(|layer| layer.id == self.layer_id)
            .ok_or_else(|| "Mask target layer not found".to_string())?;
        if layer.mask.is_none() {
            layer.add_mask(self.white);
        }
        if let Some(tiles) = &self.initial_tiles {
            let mask = layer
                .mask
                .as_mut()
                .ok_or_else(|| "Raster mask could not be created".to_string())?;
            mask.tiles = tiles.clone();
        }
        layer.mask_active = true;
        layer.paint_target = PaintTarget::Mask;
        self.before = Some(before);
        self.after = Some(StackState::capture(ctx.layers));
        Ok(())
    }

    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        self.before
            .as_ref()
            .ok_or_else(|| "Mask command was not executed".to_string())?
            .restore(ctx.layers);
        Ok(())
    }

    fn label(&self) -> &str {
        "Create or Attach Raster Mask"
    }
}

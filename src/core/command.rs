// Command pattern cho undo/redo.
// Key point: each Command stores only a DELTA (changed region), not the whole canvas.
//
// To add a new undo/redo command (Phase 2+):
//   1. Create a struct implementing the Command trait.
//   2. execute() applies the change and stores what's needed to undo.
//   3. undo() restores the stored data.
//   4. Record it through the gateway: canvas.record(Box::new(MyCommand { ... }))
//      (or record_as/execute) — cmd_history is sealed, there is no direct push.
//
// Phase 2: Migrate tool actions sang Command pattern.

use crate::core::layer::LayerStack;
use std::collections::VecDeque;

#[derive(Clone, Debug, PartialEq)]
pub struct HistoryEntry {
    pub label: String,
    pub is_current: bool,
    pub is_undone: bool,
}

pub struct EditContext<'a> {
    pub layers: &'a mut LayerStack,
    pub canvas_width: &'a mut u32,
    pub canvas_height: &'a mut u32,
    pub selection: Option<&'a mut crate::core::selection::Selection>,
    /// Channels-panel state (alpha channels). Only alpha-channel commands
    /// need it; other construction sites leave it `None`.
    pub channels: Option<&'a mut crate::core::channels::ChannelsState>,
    /// Document metadata is optional for low-level command tests that operate
    /// on a bare LayerStack. Canvas gateway contexts always provide it.
    pub metadata: Option<&'a mut crate::core::canvas::CanvasMetadata>,
}

impl<'a> EditContext<'a> {
    pub fn new(
        layers: &'a mut LayerStack,
        w: &'a mut u32,
        h: &'a mut u32,
        sel: Option<&'a mut crate::core::selection::Selection>,
    ) -> Self {
        Self {
            layers,
            canvas_width: w,
            canvas_height: h,
            selection: sel,
            channels: None,
            metadata: None,
        }
    }

    pub fn with_channels(mut self, channels: &'a mut crate::core::channels::ChannelsState) -> Self {
        self.channels = Some(channels);
        self
    }

    pub fn with_metadata(mut self, metadata: &'a mut crate::core::canvas::CanvasMetadata) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

pub trait Command: Send + Sync {
    /// Apply the change. Should store the prior state for undo.
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String>;

    /// Restore the state from before execute.
    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String>;

    /// Display name in the History panel.
    fn label(&self) -> &str;

    /// Estimated memory used (bytes). Used to evict when over budget.
    fn memory_bytes(&self) -> usize {
        0
    }

    /// Can this merge with the next command?
    /// E.g. consecutive brush dabs → merge into one undo entry.
    #[allow(dead_code)]
    fn can_merge_with(&self, _next: &dyn Command) -> bool {
        false
    }
}

pub struct DeltaSnapshot {
    pub layer_id: u32,
    pub target: crate::core::layer::PaintTarget,
    pub before_tiles: crate::core::tile::TileMap,
    pub after_tiles: crate::core::tile::TileMap,
}

impl DeltaSnapshot {
    /// Capture the layer's current pixels as "before" (Arc, O(1)).
    pub fn capture_before(
        layer_tiles: &crate::core::tile::TileMap,
        layer_id: u32,
        target: crate::core::layer::PaintTarget,
    ) -> Self {
        Self {
            layer_id,
            target,
            before_tiles: layer_tiles.clone(),
            after_tiles: layer_tiles.clone(),
        }
    }

    /// Store the "after" pixels once painting is done.
    pub fn capture_after(&mut self, layer_tiles: &crate::core::tile::TileMap) {
        self.after_tiles = layer_tiles.clone();
    }

    pub fn memory_bytes(&self) -> usize {
        // Changed tiles only, at their REAL size (a 16-bit tile carries a
        // second master buffer) — same marginal-cost accounting the eviction
        // budget assumes everywhere.
        let mut bytes = 0usize;
        for (pos, after_arc) in &self.after_tiles.tiles {
            if self.before_tiles.tiles.get(pos).map_or(true, |before_arc| {
                !std::sync::Arc::ptr_eq(after_arc, before_arc)
            }) {
                bytes += after_arc.byte_size();
            }
        }
        for (pos, before_arc) in &self.before_tiles.tiles {
            if !self.after_tiles.tiles.contains_key(pos) {
                bytes += before_arc.byte_size();
            }
        }
        bytes
    }

    pub fn has_changes(&self) -> bool {
        self.changed_tile_count() > 0
    }

    fn changed_tile_count(&self) -> usize {
        let mut changed = self
            .after_tiles
            .tiles
            .iter()
            .filter(|(pos, after_arc)| {
                self.before_tiles.tiles.get(pos).map_or(true, |before_arc| {
                    !std::sync::Arc::ptr_eq(after_arc, before_arc)
                })
            })
            .count();

        changed += self
            .before_tiles
            .tiles
            .keys()
            .filter(|pos| !self.after_tiles.tiles.contains_key(pos))
            .count();

        changed
    }

    /// Apply after_pixels to the layer (execute).
    pub fn apply_after(&self, layer_tiles: &mut crate::core::tile::TileMap) {
        *layer_tiles = self.after_tiles.clone();
        layer_tiles.bump_changed_revisions(&self.before_tiles);
    }

    /// Apply before_pixels to the layer (undo).
    pub fn apply_before(&self, layer_tiles: &mut crate::core::tile::TileMap) {
        *layer_tiles = self.before_tiles.clone();
        layer_tiles.bump_changed_revisions(&self.after_tiles);
    }
}

pub struct PaintCommand {
    pub label: String,
    pub snapshot: DeltaSnapshot,
}

impl PaintCommand {
    pub fn new(label: &str, snapshot: DeltaSnapshot) -> Self {
        Self {
            label: label.to_string(),
            snapshot,
        }
    }
}

impl Command for PaintCommand {
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        let id = self.snapshot.layer_id;
        let layer = ctx
            .layers
            .layers
            .iter_mut()
            .find(|l| l.id == id)
            .ok_or_else(|| format!("Layer {} not found", id))?;
        match self.snapshot.target {
            crate::core::layer::PaintTarget::Pixels => self.snapshot.apply_after(&mut layer.tiles),
            crate::core::layer::PaintTarget::Mask => {
                if let Some(mask) = &mut layer.mask {
                    self.snapshot.apply_after(&mut mask.tiles);
                }
            }
        }
        Ok(())
    }

    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        let id = self.snapshot.layer_id;
        let layer = ctx
            .layers
            .layers
            .iter_mut()
            .find(|l| l.id == id)
            .ok_or_else(|| format!("Layer {} not found", id))?;
        match self.snapshot.target {
            crate::core::layer::PaintTarget::Pixels => self.snapshot.apply_before(&mut layer.tiles),
            crate::core::layer::PaintTarget::Mask => {
                if let Some(mask) = &mut layer.mask {
                    self.snapshot.apply_before(&mut mask.tiles);
                }
            }
        }
        Ok(())
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn memory_bytes(&self) -> usize {
        self.snapshot.memory_bytes() + self.label.len()
    }

    fn can_merge_with(&self, next: &dyn Command) -> bool {
        next.label() == self.label()
    }
}

pub struct LayerStructureCommand {
    label: String,
    before_layers: Vec<crate::core::layer::Layer>,
    after_layers: Vec<crate::core::layer::Layer>,
    before_active: usize,
    after_active: usize,
    before_next_id: u32,
    after_next_id: u32,
    before_w: u32,
    before_h: u32,
    after_w: u32,
    after_h: u32,
}

impl LayerStructureCommand {
    pub fn capture_before(label: &str, stack: &LayerStack, w: u32, h: u32) -> Self {
        Self {
            label: label.to_string(),
            before_layers: stack.layers.clone(),
            after_layers: Vec::new(),
            before_active: stack.active_idx,
            after_active: 0,
            before_next_id: stack.next_id(),
            after_next_id: stack.next_id(),
            before_w: w,
            before_h: h,
            after_w: w,
            after_h: h,
        }
    }

    pub fn capture_after(&mut self, stack: &LayerStack, w: u32, h: u32) {
        self.after_layers = stack.layers.clone();
        self.after_active = stack.active_idx;
        self.after_next_id = stack.next_id();
        self.after_w = w;
        self.after_h = h;
    }
}

impl Command for LayerStructureCommand {
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        ctx.layers.layers = self.after_layers.clone();
        ctx.layers.active_idx = self.after_active;
        ctx.layers.set_next_id(self.after_next_id);
        *ctx.canvas_width = self.after_w;
        *ctx.canvas_height = self.after_h;
        Ok(())
    }

    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        ctx.layers.layers = self.before_layers.clone();
        ctx.layers.active_idx = self.before_active;
        ctx.layers.set_next_id(self.before_next_id);
        *ctx.canvas_width = self.before_w;
        *ctx.canvas_height = self.before_h;
        Ok(())
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn memory_bytes(&self) -> usize {
        // MARGINAL footprint: only tiles the command INTRODUCED (present in
        // `after` but not in `before`, by Arc identity). The `before` tiles are
        // the previous state's `after` — owned by the older history entry — and
        // unchanged tiles are shared with the live canvas, so counting the full
        // union here billed every structure command (crop, resize, reorder,
        // opacity…) at whole-document size and evicted the history down to a
        // handful of entries on a large photo.
        let mut before: std::collections::HashSet<*const crate::core::tile::Tile> =
            std::collections::HashSet::new();
        for l in self.before_layers.iter() {
            for arc in l.tiles.tiles.values() {
                before.insert(std::sync::Arc::as_ptr(arc));
            }
            if let Some(m) = &l.mask {
                for arc in m.tiles.tiles.values() {
                    before.insert(std::sync::Arc::as_ptr(arc));
                }
            }
        }
        let mut bytes = 0usize;
        let mut seen: std::collections::HashSet<*const crate::core::tile::Tile> =
            std::collections::HashSet::new();
        for l in self.after_layers.iter() {
            for arc in l.tiles.tiles.values() {
                let p = std::sync::Arc::as_ptr(arc);
                if !before.contains(&p) && seen.insert(p) {
                    bytes += arc.byte_size();
                }
            }
            if let Some(m) = &l.mask {
                for arc in m.tiles.tiles.values() {
                    let p = std::sync::Arc::as_ptr(arc);
                    if !before.contains(&p) && seen.insert(p) {
                        bytes += arc.byte_size();
                    }
                }
            }
        }
        bytes + (self.before_layers.len() + self.after_layers.len()) * 512
    }
}

fn encode_rle(data: &[u8]) -> Vec<(u8, u32)> {
    let mut out = Vec::new();
    if data.is_empty() {
        return out;
    }
    let mut current = data[0];
    let mut count = 1;
    for &b in &data[1..] {
        if b == current && count < u32::MAX {
            count += 1;
        } else {
            out.push((current, count));
            current = b;
            count = 1;
        }
    }
    out.push((current, count));
    out
}

fn decode_rle(rle: &[(u8, u32)], size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(size);
    for &(val, count) in rle {
        out.resize(out.len() + count as usize, val);
    }
    out
}

pub struct SelectionCommand {
    label: String,
    before: crate::core::selection::Selection,
    after: crate::core::selection::Selection,
    before_rle: Vec<(u8, u32)>,
    after_rle: Vec<(u8, u32)>,
}

impl SelectionCommand {
    pub fn capture_before(label: &str, sel: &crate::core::selection::Selection) -> Self {
        let mut before = sel.clone();
        let before_rle = encode_rle(&before.mask);
        before.mask.clear();
        before.mask.shrink_to_fit();

        Self {
            label: label.to_string(),
            before,
            after: sel.clone(),
            before_rle,
            after_rle: Vec::new(),
        }
    }

    pub fn capture_after(&mut self, sel: &crate::core::selection::Selection) {
        let mut after = sel.clone();
        self.after_rle = encode_rle(&after.mask);
        after.mask.clear();
        after.mask.shrink_to_fit();
        self.after = after;
    }
}

impl Command for SelectionCommand {
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        if let Some(sel) = ctx.selection.as_mut() {
            let mut decompressed = self.after.clone();
            let mask_len = (decompressed.width as u64)
                .checked_mul(decompressed.height as u64)
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| "Selection mask size overflow".to_string())?;
            decompressed.mask = decode_rle(&self.after_rle, mask_len);
            decompressed.mark_bbox_dirty();
            **sel = decompressed;
        }
        Ok(())
    }

    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        if let Some(sel) = ctx.selection.as_mut() {
            let mut decompressed = self.before.clone();
            let mask_len = (decompressed.width as u64)
                .checked_mul(decompressed.height as u64)
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| "Selection mask size overflow".to_string())?;
            decompressed.mask = decode_rle(&self.before_rle, mask_len);
            decompressed.mark_bbox_dirty();
            **sel = decompressed;
        }
        Ok(())
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn memory_bytes(&self) -> usize {
        self.before_rle.len() * 5 + self.after_rle.len() * 5 + 100
    }
}

/// One Channels-panel alpha-channel edit. Insert/Remove own the channel
/// (moved back and forth on undo/redo — no mask clone); Rename swaps names
/// only. Requires an `EditContext` built `with_channels`.
pub enum AlphaChannelOp {
    Insert {
        index: usize,
        channel: Option<crate::core::channels::AlphaChannel>,
    },
    Remove {
        index: usize,
        channel: Option<crate::core::channels::AlphaChannel>,
    },
    Rename {
        index: usize,
        name: String,
    },
}

pub struct AlphaChannelCommand {
    label: String,
    op: AlphaChannelOp,
}

impl AlphaChannelCommand {
    /// Insert `channel` at `index` when executed (New/Duplicate/Save Selection).
    pub fn insert(label: &str, index: usize, channel: crate::core::channels::AlphaChannel) -> Self {
        Self {
            label: label.to_string(),
            op: AlphaChannelOp::Insert {
                index,
                channel: Some(channel),
            },
        }
    }

    /// Remove the channel at `index` when executed.
    pub fn remove(label: &str, index: usize) -> Self {
        Self {
            label: label.to_string(),
            op: AlphaChannelOp::Remove {
                index,
                channel: None,
            },
        }
    }

    /// Rename the channel at `index` to `name` when executed.
    pub fn rename(label: &str, index: usize, name: String) -> Self {
        Self {
            label: label.to_string(),
            op: AlphaChannelOp::Rename { index, name },
        }
    }
}

impl Command for AlphaChannelCommand {
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        let channels = ctx
            .channels
            .as_mut()
            .ok_or_else(|| "Alpha-channel command needs a channels context".to_string())?;
        match &mut self.op {
            AlphaChannelOp::Insert { index, channel } => {
                let ch = channel
                    .take()
                    .ok_or_else(|| "Alpha channel already inserted".to_string())?;
                channels.insert_alpha(*index, ch);
            }
            AlphaChannelOp::Remove { index, channel } => {
                *channel = Some(
                    channels
                        .remove_alpha(*index)
                        .ok_or_else(|| "Alpha channel index out of range".to_string())?,
                );
            }
            AlphaChannelOp::Rename { index, name } => {
                let old = channels
                    .rename_alpha(*index, name.clone())
                    .ok_or_else(|| "Alpha channel index out of range".to_string())?;
                *name = old; // swap: next execute/undo restores it
            }
        }
        Ok(())
    }

    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        let channels = ctx
            .channels
            .as_mut()
            .ok_or_else(|| "Alpha-channel command needs a channels context".to_string())?;
        match &mut self.op {
            AlphaChannelOp::Insert { index, channel } => {
                *channel = Some(
                    channels
                        .remove_alpha(*index)
                        .ok_or_else(|| "Alpha channel index out of range".to_string())?,
                );
            }
            AlphaChannelOp::Remove { index, channel } => {
                let ch = channel
                    .take()
                    .ok_or_else(|| "Alpha channel not held for undo".to_string())?;
                channels.insert_alpha(*index, ch);
            }
            AlphaChannelOp::Rename { index, name } => {
                let old = channels
                    .rename_alpha(*index, name.clone())
                    .ok_or_else(|| "Alpha channel index out of range".to_string())?;
                *name = old;
            }
        }
        Ok(())
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn memory_bytes(&self) -> usize {
        let held = match &self.op {
            AlphaChannelOp::Insert { channel, .. } | AlphaChannelOp::Remove { channel, .. } => {
                channel.as_ref().map_or(0, |c| c.mask.len())
            }
            AlphaChannelOp::Rename { name, .. } => name.len(),
        };
        held + std::mem::size_of::<Self>()
    }
}

/// One Brush/Eraser stroke painted into an alpha channel (Channels panel),
/// stored as before/after crops of the stroke's bbox.
pub struct AlphaPlanePaintCommand {
    label: String,
    alpha_id: u32,
    /// (x0, y0, x1, y1), exclusive max, in channel coords.
    bbox: (u32, u32, u32, u32),
    before: Vec<u8>,
    after: Vec<u8>,
}

impl AlphaPlanePaintCommand {
    pub fn new(
        label: &str,
        alpha_id: u32,
        bbox: (u32, u32, u32, u32),
        before: Vec<u8>,
        after: Vec<u8>,
    ) -> Self {
        Self {
            label: label.to_string(),
            alpha_id,
            bbox,
            before,
            after,
        }
    }

    fn write_crop(
        alpha_id: u32,
        bbox: (u32, u32, u32, u32),
        channels: &mut crate::core::channels::ChannelsState,
        data: &[u8],
    ) {
        let Some(idx) = channels.alpha_index_of(alpha_id) else {
            return;
        };
        let ch = &mut channels.alpha[idx];
        let (x0, y0, x1, y1) = bbox;
        let crop_w = (x1 - x0) as usize;
        for (row, y) in (y0..y1.min(ch.height)).enumerate() {
            let src = row * crop_w;
            let dst = (y as usize) * (ch.width as usize) + x0 as usize;
            let n = crop_w.min((ch.width.saturating_sub(x0)) as usize);
            if src + n <= data.len() && dst + n <= ch.mask.len() {
                ch.mask[dst..dst + n].copy_from_slice(&data[src..src + n]);
            }
        }
        ch.revision += 1;
    }
}

impl Command for AlphaPlanePaintCommand {
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        let channels = ctx
            .channels
            .as_mut()
            .ok_or_else(|| "Alpha-plane command needs a channels context".to_string())?;
        Self::write_crop(self.alpha_id, self.bbox, channels, &self.after);
        Ok(())
    }

    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        let channels = ctx
            .channels
            .as_mut()
            .ok_or_else(|| "Alpha-plane command needs a channels context".to_string())?;
        Self::write_crop(self.alpha_id, self.bbox, channels, &self.before);
        Ok(())
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn memory_bytes(&self) -> usize {
        self.before.len() + self.after.len() + self.label.len()
    }
}

pub struct TranslateLayerCommand {
    offsets: Vec<(u32, (i32, i32), (i32, i32))>,
}

impl TranslateLayerCommand {
    pub fn from_applied_move(layer_ids: Vec<u32>, dx: i32, dy: i32, layers: &LayerStack) -> Self {
        let offsets = layers
            .layers
            .iter()
            .filter(|layer| layer_ids.contains(&layer.id))
            .map(|layer| {
                let after = layer.offset;
                let before = (after.0 - dx, after.1 - dy);
                (layer.id, before, after)
            })
            .collect();

        Self { offsets }
    }
}

impl Command for TranslateLayerCommand {
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        for (id, _before, after) in &self.offsets {
            if let Some(layer) = ctx.layers.layers.iter_mut().find(|l| l.id == *id) {
                layer.offset = *after;
            }
        }
        Ok(())
    }

    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        for (id, before, _after) in &self.offsets {
            if let Some(layer) = ctx.layers.layers.iter_mut().find(|l| l.id == *id) {
                layer.offset = *before;
            }
        }
        Ok(())
    }

    fn label(&self) -> &str {
        "Move Layers"
    }
    fn memory_bytes(&self) -> usize {
        32 + self.offsets.capacity() * 20
    }
}

pub struct TranslateSelectionCommand {
    before_offset: (i32, i32),
    after_offset: (i32, i32),
}

impl TranslateSelectionCommand {
    pub fn from_applied_move(
        selection: &crate::core::selection::Selection,
        dx: i32,
        dy: i32,
    ) -> Self {
        let after_offset = selection.offset;
        let before_offset = (after_offset.0 - dx, after_offset.1 - dy);
        Self {
            before_offset,
            after_offset,
        }
    }
}

impl Command for TranslateSelectionCommand {
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        if let Some(sel) = ctx.selection.as_mut() {
            sel.offset = self.after_offset;
            sel.mark_bbox_dirty();
        }
        Ok(())
    }

    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        if let Some(sel) = ctx.selection.as_mut() {
            sel.offset = self.before_offset;
            sel.mark_bbox_dirty();
        }
        Ok(())
    }

    fn label(&self) -> &str {
        "Move Selection"
    }
    fn memory_bytes(&self) -> usize {
        16
    }
}

pub struct CompoundCommand {
    label: String,
    pub commands: Vec<Box<dyn Command>>,
}

impl CompoundCommand {
    pub fn new(label: &str) -> Self {
        Self {
            label: label.to_string(),
            commands: Vec::new(),
        }
    }
}

impl Command for CompoundCommand {
    // Best-effort: a sub-command that can't resolve its target (e.g. a layer a
    // neighbouring structure change renamed/removed) must NOT abort the whole group
    // with `?`. Aborting mid-loop leaves the already-applied sub-commands done while
    // the history treats the compound as failed → canvas and undo stack desync.
    // Applying every sub-command and swallowing per-item errors keeps the group
    // atomic from the history's point of view.
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        for cmd in &mut self.commands {
            let _ = cmd.execute(ctx);
        }
        Ok(())
    }

    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        for cmd in self.commands.iter_mut().rev() {
            let _ = cmd.undo(ctx);
        }
        Ok(())
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn memory_bytes(&self) -> usize {
        self.commands.iter().map(|c| c.memory_bytes()).sum()
    }
}

/// Floor for history eviction: whatever the memory budget says, keep at least
/// this many undo steps (see `evict_if_needed`).
const MIN_KEPT_ENTRIES: usize = 6;

/// Identity of one edit on the undo stack, unique for the life of a history.
///
/// Distinct from [`CommandHistory::revision`], which is a monotonic *change
/// counter*: revision bumps on undo too, so it can never tell you that you have
/// returned to a state you saw before. An `EditId` names a *position* in the
/// edit graph, which is exactly what a saved checkpoint has to compare against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditId(u64);

/// One entry on a history stack: the command plus the identity of the state it
/// produces.
struct StackEntry {
    id: EditId,
    cmd: Box<dyn Command>,
}

/// Where the last successful save sits relative to the history stacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SavedPoint {
    /// Saved at the state reached after this edit. `None` = the initial, empty
    /// state (a fresh document, or one just opened from disk).
    At(Option<EditId>),
    /// The saved state can never be reached again: it was evicted from the
    /// history, or the history was cleared. The document reads dirty forever
    /// (until the next save), because we cannot prove it matches the file.
    Unreachable,
}

pub struct CommandHistory {
    undo_stack: VecDeque<StackEntry>,
    redo_stack: Vec<StackEntry>,
    total_memory_bytes: usize,
    /// Max memory for history (default 256MB).
    pub memory_budget_bytes: usize,
    /// Max entry count (fallback if the memory budget isn't precise enough).
    pub max_entries: usize,
    /// Bumped on every stack mutation (push/undo/redo/clear). The History
    /// panel's cache keys off this, so it can NEVER go stale — the old design
    /// asked every push site to bump a separate canvas counter by hand, and the
    /// sites that forgot (crop among them) left the panel frozen until some
    /// other tool's bump flushed all the missed entries at once.
    revision: u64,
    /// Source of [`EditId`]s. Never reused, so a diverged branch can never be
    /// mistaken for the branch a save happened on.
    next_edit_id: u64,
    /// Anchor for [`is_dirty`]. Dirty is *derived* from this, never assigned:
    /// the old design asked ~100 call sites to set a bool by hand, so undo
    /// back to a saved state still reported unsaved changes.
    saved_point: SavedPoint,

    group_depth: u32,
    pending_group: Option<(String, Vec<Box<dyn Command>>)>,
}

#[allow(dead_code)]
impl CommandHistory {
    pub fn new() -> Self {
        Self {
            undo_stack: VecDeque::new(),
            redo_stack: Vec::new(),
            total_memory_bytes: 0,
            // RAM/16 clamped to [128 MB, 1 GB] — a weak laptop keeps a smaller
            // undo footprint, a workstation keeps more history.
            memory_budget_bytes: crate::core::hw::history_budget_bytes(),
            max_entries: 100,
            revision: 1,
            next_edit_id: 1,
            // A brand-new document matches "no file" trivially; opening a file
            // re-checkpoints via mark_saved().
            saved_point: SavedPoint::At(None),
            group_depth: 0,
            pending_group: None,
        }
    }

    /// Monotonic stamp of the stacks' state — see the field doc.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn mint_edit_id(&mut self) -> EditId {
        let id = EditId(self.next_edit_id);
        self.next_edit_id += 1;
        id
    }

    /// Identity of the state the document is in right now: the edit on top of
    /// the undo stack, or `None` for the initial state.
    fn current_point(&self) -> Option<EditId> {
        self.undo_stack.back().map(|e| e.id)
    }

    /// True when the document has changes not present in the file on disk.
    ///
    /// Derived, never assigned, so all four cases fall out for free:
    /// save → edit → undo lands back on the saved point (clean); edit → save →
    /// undo leaves it (dirty) → redo returns to it (clean); and diverging from a
    /// redo branch mints a fresh [`EditId`], which can never equal the saved one.
    pub fn is_dirty(&self) -> bool {
        match self.saved_point {
            SavedPoint::Unreachable => true,
            SavedPoint::At(point) => point != self.current_point(),
        }
    }

    /// Anchor "clean" to the current state. Called after a successful save.
    pub fn mark_saved(&mut self) {
        self.saved_point = SavedPoint::At(self.current_point());
    }

    /// The current state can no longer be proven equal to the file on disk.
    /// Used when history is dropped out from under the document (e.g. a colour
    /// mode conversion that rewrites every layer), so it reads dirty until saved.
    pub fn mark_saved_state_unreachable(&mut self) {
        self.saved_point = SavedPoint::Unreachable;
    }

    /// Called when an entry is about to be dropped from the front of the undo
    /// stack. Once the saved edit — or the initial state, which *any* eviction
    /// strands — falls off, that state is gone for good and dirty must latch on.
    fn note_evicted(&mut self, evicted: EditId) {
        match self.saved_point {
            SavedPoint::At(Some(id)) if id == evicted => {
                self.saved_point = SavedPoint::Unreachable;
            }
            SavedPoint::At(None) => self.saved_point = SavedPoint::Unreachable,
            _ => {}
        }
    }

    /// Begin an undo group. Every push() until end_group() becomes one entry.
    /// Nested begin_group only increases depth (uses the outermost group's label).
    pub fn begin_group(&mut self, label: &str) {
        if self.group_depth == 0 {
            self.pending_group = Some((label.to_string(), Vec::new()));
        }
        self.group_depth += 1;
    }

    /// End the undo group. If it has exactly one command → unwrap (avoid overhead).
    /// If the group is empty → skip (push nothing).
    pub fn end_group(&mut self) {
        if self.group_depth == 0 {
            return;
        }
        self.group_depth -= 1;
        if self.group_depth == 0 {
            if let Some((label, commands)) = self.pending_group.take() {
                match commands.len() {
                    0 => {}
                    1 => {
                        let mut cmds = commands;
                        self.push_to_stack(cmds.remove(0));
                    }
                    _ => {
                        let mut compound = CompoundCommand::new(&label);
                        compound.commands = commands;
                        self.push_to_stack(Box::new(compound));
                    }
                }
            }
        }
    }

    /// Actually push onto undo_stack (bypasses grouping; internal use).
    fn push_to_stack(&mut self, cmd: Box<dyn Command>) {
        let mem = cmd.memory_bytes();
        // Dropping the redo branch destroys those states; a fresh EditId below
        // guarantees the new branch can never be mistaken for the saved one.
        self.redo_stack.clear();
        let id = self.mint_edit_id();
        self.undo_stack.push_back(StackEntry { id, cmd });
        self.total_memory_bytes += mem;
        self.revision += 1;
        self.evict_if_needed();
    }

    /// Push a new command. If inside a group → add to the group instead of the stack.
    pub fn push(&mut self, cmd: Box<dyn Command>) {
        if let Some((_, ref mut cmds)) = self.pending_group {
            cmds.push(cmd);
        } else {
            self.push_to_stack(cmd);
        }
    }

    /// Undo: pop from undo_stack, call undo(), push onto redo_stack.
    ///
    /// A command that fails to undo is put back untouched, so a failed step
    /// never leaves the document half-reverted.
    pub fn undo(&mut self, ctx: &mut EditContext) -> bool {
        if let Some(mut entry) = self.undo_stack.pop_back() {
            let mem = entry.cmd.memory_bytes();
            self.total_memory_bytes = self.total_memory_bytes.saturating_sub(mem);
            match entry.cmd.undo(ctx) {
                Ok(()) => {
                    self.redo_stack.push(entry);
                    self.revision += 1;
                    true
                }
                Err(_) => {
                    self.total_memory_bytes += mem;
                    self.undo_stack.push_back(entry);
                    false
                }
            }
        } else {
            false
        }
    }

    /// Redo: pop from redo_stack, call execute(), push onto undo_stack.
    ///
    /// The entry keeps its original [`EditId`], so redoing back onto a saved
    /// checkpoint reports clean again.
    pub fn redo(&mut self, ctx: &mut EditContext) -> bool {
        if let Some(mut entry) = self.redo_stack.pop() {
            match entry.cmd.execute(ctx) {
                Ok(()) => {
                    let mem = entry.cmd.memory_bytes();
                    self.undo_stack.push_back(entry);
                    self.total_memory_bytes += mem;
                    self.revision += 1;
                    self.evict_if_needed();
                    true
                }
                Err(_) => {
                    self.redo_stack.push(entry);
                    false
                }
            }
        } else {
            false
        }
    }

    /// Evict the oldest entries when over budget — but NEVER below
    /// [`MIN_KEPT_ENTRIES`]: one huge command (a crop rebuilds every layer's
    /// tiles, so it alone can exceed the whole budget) must not wipe the rest
    /// of the history. Running some hundred MB over budget beats stranding the
    /// user two steps from nothing.
    fn evict_if_needed(&mut self) {
        while (self.total_memory_bytes > self.memory_budget_bytes
            || self.undo_stack.len() > self.max_entries)
            && self.undo_stack.len() > MIN_KEPT_ENTRIES
        {
            if let Some(old) = self.undo_stack.pop_front() {
                self.total_memory_bytes = self
                    .total_memory_bytes
                    .saturating_sub(old.cmd.memory_bytes());
                self.note_evicted(old.id);
            }
        }
    }

    pub fn undo_count(&self) -> usize {
        self.undo_stack.len()
    }
    pub fn redo_count(&self) -> usize {
        self.redo_stack.len()
    }
    pub fn memory_mb(&self) -> f32 {
        self.total_memory_bytes as f32 / 1_048_576.0
    }

    pub fn history_entries(&self) -> Vec<HistoryEntry> {
        let mut entries = Vec::new();

        entries.push(HistoryEntry {
            label: "Initial State".to_string(),
            is_current: self.undo_stack.is_empty(),
            is_undone: false,
        });

        let undo_count = self.undo_stack.len();

        for (i, entry) in self.undo_stack.iter().enumerate() {
            entries.push(HistoryEntry {
                label: entry.cmd.label().to_string(),
                is_current: i == undo_count - 1,
                is_undone: false,
            });
        }

        for entry in self.redo_stack.iter().rev() {
            entries.push(HistoryEntry {
                label: entry.cmd.label().to_string(),
                is_current: false,
                is_undone: true,
            });
        }

        entries
    }

    /// Drop all history. The document is left dirty: clearing throws away the
    /// states we would compare a checkpoint against, so we can no longer prove
    /// the content matches the file. Callers that mean "this IS the clean
    /// baseline" (open/save) follow up with [`mark_saved`](Self::mark_saved).
    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.total_memory_bytes = 0;
        self.revision += 1;
        self.group_depth = 0;
        self.pending_group = None;
        self.saved_point = SavedPoint::Unreachable;
    }
}

impl Default for CommandHistory {
    fn default() -> Self {
        Self::new()
    }
}
pub enum PixelTransformKind {
    Rotate90CW,
    Rotate90CCW,
    FlipH,
    FlipV,
}

pub struct LayerTileSnapshot {
    pub layer_id: u32,
    pub before_tiles: crate::core::tile::TileMap,
    pub before_mask: Option<crate::core::layer::LayerMask>,
    pub before_width: u32,
    pub before_height: u32,
    pub before_offset: (i32, i32),
}

pub struct PixelTransformCommand {
    label: String,
    transform: PixelTransformKind,
    snapshots: Vec<LayerTileSnapshot>,
    before_sel: crate::core::selection::Selection,
    before_canvas_w: u32,
    before_canvas_h: u32,
    after_canvas_w: u32,
    after_canvas_h: u32,
}

impl PixelTransformCommand {
    pub fn capture_before(
        label: &str,
        transform: PixelTransformKind,
        stack: &crate::core::layer::LayerStack,
        canvas_w: u32,
        canvas_h: u32,
        sel: &crate::core::selection::Selection,
    ) -> Self {
        let snapshots = stack
            .layers
            .iter()
            .map(|l| LayerTileSnapshot {
                layer_id: l.id,
                before_tiles: l.tiles.clone(),
                before_mask: l.mask.clone(),
                before_width: l.width,
                before_height: l.height,
                before_offset: l.offset,
            })
            .collect();

        let (after_canvas_w, after_canvas_h) = match transform {
            PixelTransformKind::Rotate90CW | PixelTransformKind::Rotate90CCW => {
                (canvas_h, canvas_w)
            }
            PixelTransformKind::FlipH | PixelTransformKind::FlipV => (canvas_w, canvas_h),
        };

        Self {
            label: label.to_string(),
            transform,
            snapshots,
            before_sel: sel.clone(),
            before_canvas_w: canvas_w,
            before_canvas_h: canvas_h,
            after_canvas_w,
            after_canvas_h,
        }
    }

    pub fn transform_sel_mask_pub(
        mask: &[u8],
        transform: &PixelTransformKind,
        w: u32,
        h: u32,
    ) -> (Vec<u8>, u32, u32) {
        Self::transform_sel_mask(mask, transform, w, h)
    }

    /// Layer masks live in layer-local space, so a whole-image flip/rotate must
    /// apply the same tile transform to the mask (offsets are handled by the
    /// layer). Shared by the live canvas paths and this command's redo.
    pub fn transform_layer_mask(
        mask: &mut crate::core::layer::LayerMask,
        transform: &PixelTransformKind,
    ) {
        match transform {
            PixelTransformKind::FlipH => mask.tiles = mask.tiles.flip_h(),
            PixelTransformKind::FlipV => mask.tiles = mask.tiles.flip_v(),
            PixelTransformKind::Rotate90CW => {
                mask.tiles = mask.tiles.rotate_90_cw();
                std::mem::swap(&mut mask.width, &mut mask.height);
            }
            PixelTransformKind::Rotate90CCW => {
                mask.tiles = mask.tiles.rotate_90_ccw();
                std::mem::swap(&mut mask.width, &mut mask.height);
            }
        }
    }

    fn transform_sel_mask(
        mask: &[u8],
        transform: &PixelTransformKind,
        w: u32,
        h: u32,
    ) -> (Vec<u8>, u32, u32) {
        let w_u = w as usize;
        let h_u = h as usize;
        if mask.is_empty() || w_u * h_u != mask.len() {
            return (mask.to_vec(), w, h);
        }
        match transform {
            PixelTransformKind::FlipH => {
                let mut out = vec![0u8; mask.len()];
                for y in 0..h_u {
                    for x in 0..w_u {
                        out[y * w_u + x] = mask[y * w_u + (w_u - 1 - x)];
                    }
                }
                (out, w, h)
            }
            PixelTransformKind::FlipV => {
                let mut out = vec![0u8; mask.len()];
                for y in 0..h_u {
                    let src_y = h_u - 1 - y;
                    out[y * w_u..(y + 1) * w_u]
                        .copy_from_slice(&mask[src_y * w_u..(src_y + 1) * w_u]);
                }
                (out, w, h)
            }
            PixelTransformKind::Rotate90CW => {
                let mut out = vec![0u8; mask.len()];
                for y in 0..h_u {
                    for x in 0..w_u {
                        let dst_x = h_u - 1 - y;
                        let dst_y = x;
                        out[dst_y * h_u + dst_x] = mask[y * w_u + x];
                    }
                }
                (out, h, w)
            }
            PixelTransformKind::Rotate90CCW => {
                let mut out = vec![0u8; mask.len()];
                for y in 0..h_u {
                    for x in 0..w_u {
                        let dst_x = y;
                        let dst_y = w_u - 1 - x;
                        out[dst_y * h_u + dst_x] = mask[y * w_u + x];
                    }
                }
                (out, h, w)
            }
        }
    }

    fn reapply(&self, ctx: &mut EditContext) {
        let canvas_w = *ctx.canvas_width as i32;
        let canvas_h = *ctx.canvas_height as i32;
        for layer in &mut ctx.layers.layers {
            if !self.snapshots.iter().any(|s| s.layer_id == layer.id) {
                continue;
            }
            match self.transform {
                PixelTransformKind::FlipH => {
                    let lw = layer.width as i32;
                    let old_ox = layer.offset.0;
                    layer.tiles = layer.tiles.flip_h();
                    layer.offset.0 = canvas_w - old_ox - lw;
                }
                PixelTransformKind::FlipV => {
                    let lh = layer.height as i32;
                    let old_oy = layer.offset.1;
                    layer.tiles = layer.tiles.flip_v();
                    layer.offset.1 = canvas_h - old_oy - lh;
                }
                PixelTransformKind::Rotate90CW => {
                    let old_lh = layer.height as i32;
                    let old_ox = layer.offset.0;
                    let old_oy = layer.offset.1;
                    layer.tiles = layer.tiles.rotate_90_cw();
                    std::mem::swap(&mut layer.width, &mut layer.height);
                    layer.offset.0 = canvas_h - old_oy - old_lh;
                    layer.offset.1 = old_ox;
                }
                PixelTransformKind::Rotate90CCW => {
                    let old_lw = layer.width as i32;
                    let old_ox = layer.offset.0;
                    let old_oy = layer.offset.1;
                    layer.tiles = layer.tiles.rotate_90_ccw();
                    std::mem::swap(&mut layer.width, &mut layer.height);
                    layer.offset.0 = old_oy;
                    layer.offset.1 = canvas_w - old_ox - old_lw;
                }
            }
            if let Some(mask) = &mut layer.mask {
                Self::transform_layer_mask(mask, &self.transform);
            }
        }
        *ctx.canvas_width = self.after_canvas_w;
        *ctx.canvas_height = self.after_canvas_h;
    }

    fn apply_transform_to_sel(&self, ctx: &mut EditContext) {
        if let Some(sel) = ctx.selection.as_mut() {
            let (new_mask, new_w, new_h) =
                Self::transform_sel_mask(&sel.mask, &self.transform, sel.width, sel.height);
            sel.mask = new_mask;
            sel.width = new_w;
            sel.height = new_h;
            sel.offset = (0, 0);
            sel.mask_revision += 1;
            sel.mark_bbox_dirty();
        }
    }
}

impl Command for PixelTransformCommand {
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        self.reapply(ctx);
        self.apply_transform_to_sel(ctx);
        Ok(())
    }

    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        for snap in &self.snapshots {
            if let Some(layer) = ctx.layers.layers.iter_mut().find(|l| l.id == snap.layer_id) {
                layer.tiles = snap.before_tiles.clone();
                layer.mask = snap.before_mask.clone();
                layer.width = snap.before_width;
                layer.height = snap.before_height;
                layer.offset = snap.before_offset;
            }
        }
        *ctx.canvas_width = self.before_canvas_w;
        *ctx.canvas_height = self.before_canvas_h;
        if let Some(sel) = ctx.selection.as_mut() {
            **sel = self.before_sel.clone();
            sel.mark_bbox_dirty();
        }
        Ok(())
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn memory_bytes(&self) -> usize {
        let mut seen: std::collections::HashSet<*const crate::core::tile::Tile> =
            std::collections::HashSet::new();
        for snap in &self.snapshots {
            for arc in snap.before_tiles.tiles.values() {
                seen.insert(std::sync::Arc::as_ptr(arc));
            }
        }
        seen.len() * crate::core::tile::TILE_BYTES
            + self.snapshots.len() * 64
            + self.before_sel.mask.len()
    }
}

/// Per-layer state stored inside a FreeTransformCommand.
pub struct LayerTransformEntry {
    layer_id: u32,
    before_layer_type: crate::core::layer::LayerType,
    before_tiles: crate::core::tile::TileMap,
    before_mask: Option<crate::core::layer::LayerMask>,
    before_w: u32,
    before_h: u32,
    before_offset: (i32, i32),
    after_layer_type: crate::core::layer::LayerType,
    after_tiles: crate::core::tile::TileMap,
    after_mask: Option<crate::core::layer::LayerMask>,
    after_w: u32,
    after_h: u32,
    after_offset: (i32, i32),
}

/// Undo/redo record for Ctrl+T free transform commit.
/// Supports any number of layers (single or multi-select).
pub struct FreeTransformCommand {
    label: String,
    layers: Vec<LayerTransformEntry>,
}

impl FreeTransformCommand {
    pub fn new(label: &str) -> Self {
        Self {
            label: label.to_string(),
            layers: Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_layer(
        &mut self,
        layer_id: u32,
        before_layer_type: crate::core::layer::LayerType,
        before_tiles: crate::core::tile::TileMap,
        before_mask: Option<crate::core::layer::LayerMask>,
        before_w: u32,
        before_h: u32,
        before_offset: (i32, i32),
        after_layer_type: crate::core::layer::LayerType,
        after_tiles: crate::core::tile::TileMap,
        after_mask: Option<crate::core::layer::LayerMask>,
        after_w: u32,
        after_h: u32,
        after_offset: (i32, i32),
    ) {
        self.layers.push(LayerTransformEntry {
            layer_id,
            before_layer_type,
            before_tiles,
            before_mask,
            before_w,
            before_h,
            before_offset,
            after_layer_type,
            after_tiles,
            after_mask,
            after_w,
            after_h,
            after_offset,
        });
    }
}

impl Command for FreeTransformCommand {
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        for entry in &self.layers {
            if let Some(layer) = ctx
                .layers
                .layers
                .iter_mut()
                .find(|l| l.id == entry.layer_id)
            {
                layer.tiles = entry.after_tiles.clone();
                layer.mask = entry.after_mask.clone();
                layer.width = entry.after_w;
                layer.height = entry.after_h;
                layer.offset = entry.after_offset;
                layer.layer_type = entry.after_layer_type.clone();
            }
        }
        Ok(())
    }
    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        for entry in &self.layers {
            if let Some(layer) = ctx
                .layers
                .layers
                .iter_mut()
                .find(|l| l.id == entry.layer_id)
            {
                layer.tiles = entry.before_tiles.clone();
                layer.mask = entry.before_mask.clone();
                layer.width = entry.before_w;
                layer.height = entry.before_h;
                layer.offset = entry.before_offset;
                layer.layer_type = entry.before_layer_type.clone();
            }
        }
        Ok(())
    }
    fn label(&self) -> &str {
        &self.label
    }
    fn memory_bytes(&self) -> usize {
        let mut seen = std::collections::HashSet::new();
        for entry in &self.layers {
            for arc in entry
                .before_tiles
                .tiles
                .values()
                .chain(entry.after_tiles.tiles.values())
                .chain(
                    entry
                        .before_mask
                        .iter()
                        .flat_map(|m| m.tiles.tiles.values()),
                )
                .chain(entry.after_mask.iter().flat_map(|m| m.tiles.tiles.values()))
            {
                seen.insert(std::sync::Arc::as_ptr(arc));
            }
        }
        seen.len() * crate::core::tile::TILE_BYTES + 128
    }
}

pub struct ResizeCanvasCommand {
    snapshots: Vec<LayerTileSnapshot>,
    before_canvas_w: u32,
    before_canvas_h: u32,
    after_canvas_w: u32,
    after_canvas_h: u32,
}

impl ResizeCanvasCommand {
    pub fn capture_before(
        stack: &crate::core::layer::LayerStack,
        canvas_w: u32,
        canvas_h: u32,
        new_w: u32,
        new_h: u32,
    ) -> Self {
        let snapshots = stack
            .layers
            .iter()
            .map(|l| LayerTileSnapshot {
                layer_id: l.id,
                before_tiles: l.tiles.clone(),
                before_mask: l.mask.clone(),
                before_width: l.width,
                before_height: l.height,
                before_offset: l.offset,
            })
            .collect();
        Self {
            snapshots,
            before_canvas_w: canvas_w,
            before_canvas_h: canvas_h,
            after_canvas_w: new_w,
            after_canvas_h: new_h,
        }
    }

    fn reapply(&self, ctx: &mut EditContext) {
        let new_w = self.after_canvas_w;
        let new_h = self.after_canvas_h;
        for layer in &mut ctx.layers.layers {
            let copy_w = layer.width.min(new_w);
            let copy_h = layer.height.min(new_h);
            let sub = layer.tiles.extract_region(0, 0, copy_w, copy_h);
            layer.tiles = crate::core::tile::TileMap::from_rgba(&sub, new_w, new_h);
            layer.width = new_w;
            layer.height = new_h;
            if let Some(mask) = &mut layer.mask {
                mask.resize_to(new_w, new_h);
            }
        }
        *ctx.canvas_width = new_w;
        *ctx.canvas_height = new_h;
    }
}

impl Command for ResizeCanvasCommand {
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        self.reapply(ctx);
        Ok(())
    }

    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        for snap in &self.snapshots {
            if let Some(layer) = ctx.layers.layers.iter_mut().find(|l| l.id == snap.layer_id) {
                layer.tiles = snap.before_tiles.clone();
                layer.mask = snap.before_mask.clone();
                layer.width = snap.before_width;
                layer.height = snap.before_height;
            }
        }
        *ctx.canvas_width = self.before_canvas_w;
        *ctx.canvas_height = self.before_canvas_h;
        Ok(())
    }

    fn label(&self) -> &str {
        "Resize Canvas"
    }

    fn memory_bytes(&self) -> usize {
        let mut seen: std::collections::HashSet<*const crate::core::tile::Tile> =
            std::collections::HashSet::new();
        for snap in &self.snapshots {
            for arc in snap.before_tiles.tiles.values() {
                seen.insert(std::sync::Arc::as_ptr(arc));
            }
        }
        seen.len() * crate::core::tile::TILE_BYTES + self.snapshots.len() * 64
    }
}

/// Undoable change to a document's page setup — the default page bleed / margin
/// and the explicit artboard list, all held in [`crate::core::canvas::CanvasMetadata`].
/// Routing every artboard edit through one swap command keeps it on the canvas
/// history gate (Artboard plan, trap #1): it undoes and marks the document dirty
/// like any pixel edit. The prior snapshot is captured on the first `execute` and
/// restored on `undo`; a redo re-applies the new state.
pub struct PageSetupCommand {
    label: String,
    bleed_px: f32,
    margin_px: f32,
    artboards: Vec<crate::core::page::Page>,
    prev: Option<(f32, f32, Vec<crate::core::page::Page>)>,
}

impl PageSetupCommand {
    pub fn new(
        label: &str,
        bleed_px: f32,
        margin_px: f32,
        artboards: Vec<crate::core::page::Page>,
    ) -> Self {
        Self {
            label: label.to_string(),
            bleed_px,
            margin_px,
            artboards,
            prev: None,
        }
    }
}

impl Command for PageSetupCommand {
    fn execute(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        let meta = ctx
            .metadata
            .as_mut()
            .ok_or_else(|| "Page-setup command needs a metadata context".to_string())?;
        if self.prev.is_none() {
            self.prev = Some((
                meta.page_bleed_px,
                meta.page_margin_px,
                meta.artboards.clone(),
            ));
        }
        meta.page_bleed_px = self.bleed_px;
        meta.page_margin_px = self.margin_px;
        meta.artboards = self.artboards.clone();
        Ok(())
    }

    fn undo(&mut self, ctx: &mut EditContext) -> Result<(), String> {
        let meta = ctx
            .metadata
            .as_mut()
            .ok_or_else(|| "Page-setup command needs a metadata context".to_string())?;
        let (bleed, margin, artboards) = self
            .prev
            .clone()
            .ok_or_else(|| "Page-setup command has no prior state to undo".to_string())?;
        meta.page_bleed_px = bleed;
        meta.page_margin_px = margin;
        meta.artboards = artboards;
        Ok(())
    }

    fn label(&self) -> &str {
        &self.label
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paint_undo_refreshes_tile_revision_before_the_next_edit() {
        use crate::core::tile::TilePos;

        let before = crate::core::tile::TileMap::from_rgba(&[10, 20, 30, 255], 1, 1);
        let mut after = before.clone();
        after.get_tile_mut(TilePos { x: 0, y: 0 }).pixels[0] = 200;
        let stale_filled_revision = after.tiles.get(&TilePos { x: 0, y: 0 }).unwrap().revision;

        let snapshot = DeltaSnapshot {
            layer_id: 1,
            target: crate::core::layer::PaintTarget::Pixels,
            before_tiles: before,
            after_tiles: after,
        };
        let mut live = snapshot.after_tiles.clone();

        snapshot.apply_before(&mut live);
        let restored_revision = live.tiles.get(&TilePos { x: 0, y: 0 }).unwrap().revision;
        assert_ne!(restored_revision, stale_filled_revision);

        live.get_tile_mut(TilePos { x: 0, y: 0 }).pixels[1] = 210;
        let next_fill_revision = live.tiles.get(&TilePos { x: 0, y: 0 }).unwrap().revision;
        assert_ne!(next_fill_revision, stale_filled_revision);
    }

    struct FailingCommand;

    impl Command for FailingCommand {
        fn execute(&mut self, _ctx: &mut EditContext) -> Result<(), String> {
            Err("execute failed".to_string())
        }

        fn undo(&mut self, _ctx: &mut EditContext) -> Result<(), String> {
            Err("undo failed".to_string())
        }

        fn label(&self) -> &str {
            "Failing"
        }

        fn memory_bytes(&self) -> usize {
            7
        }
    }

    fn empty_context<'a>(
        stack: &'a mut LayerStack,
        width: &'a mut u32,
        height: &'a mut u32,
    ) -> EditContext<'a> {
        EditContext::new(stack, width, height, None)
    }

    /// Minimal command that succeeds and carries a label — enough to move the
    /// history between states in the checkpoint tests below.
    struct NoopCommand(&'static str);

    impl Command for NoopCommand {
        fn execute(&mut self, _ctx: &mut EditContext) -> Result<(), String> {
            Ok(())
        }
        fn undo(&mut self, _ctx: &mut EditContext) -> Result<(), String> {
            Ok(())
        }
        fn label(&self) -> &str {
            self.0
        }
        fn memory_bytes(&self) -> usize {
            1
        }
    }

    fn edit(history: &mut CommandHistory, label: &'static str) {
        history.push(Box::new(NoopCommand(label)));
    }

    // ── Saved-checkpoint dirty semantics ────────────────────────────────────
    // Dirty is derived from the saved point, never assigned. These pin the four
    // cases the old hand-assigned `is_modified` bool got wrong.

    #[test]
    fn fresh_history_is_clean() {
        assert!(!CommandHistory::new().is_dirty());
    }

    #[test]
    fn edit_dirties_and_save_cleans() {
        let mut h = CommandHistory::new();
        edit(&mut h, "Brush");
        assert!(h.is_dirty(), "an edit must dirty the document");
        h.mark_saved();
        assert!(!h.is_dirty(), "saving must clean it");
    }

    #[test]
    fn save_then_edit_then_undo_to_the_saved_point_is_clean() {
        let (mut stack, mut w, mut h_) = (LayerStack::new(4, 4), 4, 4);
        let mut history = CommandHistory::new();
        edit(&mut history, "Brush");
        history.mark_saved();

        edit(&mut history, "Fill");
        assert!(history.is_dirty());

        let mut ctx = empty_context(&mut stack, &mut w, &mut h_);
        assert!(history.undo(&mut ctx));
        assert!(
            !history.is_dirty(),
            "undo back onto the saved state must read clean"
        );
    }

    #[test]
    fn edit_then_save_then_undo_is_dirty_and_redo_is_clean_again() {
        let (mut stack, mut w, mut h_) = (LayerStack::new(4, 4), 4, 4);
        let mut history = CommandHistory::new();
        edit(&mut history, "Brush");
        history.mark_saved();

        let mut ctx = empty_context(&mut stack, &mut w, &mut h_);
        assert!(history.undo(&mut ctx));
        assert!(
            history.is_dirty(),
            "undoing away from the saved state must dirty"
        );

        assert!(history.redo(&mut ctx));
        assert!(
            !history.is_dirty(),
            "redo back onto the saved state must clean again"
        );
    }

    #[test]
    fn diverging_from_the_saved_branch_stays_dirty_forever() {
        let (mut stack, mut w, mut h_) = (LayerStack::new(4, 4), 4, 4);
        let mut history = CommandHistory::new();
        edit(&mut history, "Brush");
        history.mark_saved();

        let mut ctx = empty_context(&mut stack, &mut w, &mut h_);
        history.undo(&mut ctx);
        // New edit destroys the redo branch the save lived on.
        edit(&mut history, "Fill");
        assert!(history.is_dirty());

        // No amount of undo can return to the saved state — it no longer exists.
        history.undo(&mut ctx);
        assert!(
            history.is_dirty(),
            "the saved state was destroyed; it must never read clean again"
        );
    }

    #[test]
    fn evicting_the_saved_edit_latches_dirty() {
        let mut history = CommandHistory::new();
        history.max_entries = MIN_KEPT_ENTRIES;
        edit(&mut history, "Saved");
        history.mark_saved();
        assert!(!history.is_dirty());

        // Push until the saved entry falls off the front of the stack.
        for _ in 0..(MIN_KEPT_ENTRIES * 3) {
            edit(&mut history, "More");
        }
        assert!(
            history.is_dirty(),
            "once the saved edit is evicted the state is unreachable → dirty"
        );
    }

    #[test]
    fn failed_command_does_not_enter_history() {
        let mut history = CommandHistory::new();
        history.mark_saved();
        let before = history.undo_count();

        // A command that cannot execute must never be pushed; the gateway is
        // what enforces this, so assert the contract it relies on.
        let mut cmd = FailingCommand;
        let (mut stack, mut w, mut h_) = (LayerStack::new(4, 4), 4, 4);
        let mut ctx = empty_context(&mut stack, &mut w, &mut h_);
        assert!(cmd.execute(&mut ctx).is_err());

        assert_eq!(history.undo_count(), before);
        assert!(!history.is_dirty(), "a failed command must not dirty");
    }

    #[test]
    fn a_transaction_group_makes_exactly_one_undo_entry() {
        let mut history = CommandHistory::new();
        history.begin_group("Drag");
        for _ in 0..12 {
            edit(&mut history, "scrub tick");
        }
        history.end_group();
        assert_eq!(
            history.undo_count(),
            1,
            "a whole drag/scrub must collapse to one undo entry"
        );
    }

    #[test]
    fn an_empty_transaction_group_makes_no_entry_and_no_dirt() {
        let mut history = CommandHistory::new();
        history.mark_saved();
        history.begin_group("Cancelled preview");
        history.end_group();
        assert_eq!(history.undo_count(), 0);
        assert!(
            !history.is_dirty(),
            "a cancelled preview must leave no history and no dirt"
        );
    }

    #[test]
    fn undo_to_initial_state_after_saving_at_initial_state_is_clean() {
        let (mut stack, mut w, mut h_) = (LayerStack::new(4, 4), 4, 4);
        let mut history = CommandHistory::new();
        // Opening a file checkpoints at the initial, empty state.
        history.mark_saved();
        edit(&mut history, "Brush");
        assert!(history.is_dirty());

        let mut ctx = empty_context(&mut stack, &mut w, &mut h_);
        history.undo(&mut ctx);
        assert!(
            !history.is_dirty(),
            "undo back to the opened state must read clean"
        );
    }

    #[test]
    fn clearing_history_leaves_the_document_dirty() {
        let mut history = CommandHistory::new();
        edit(&mut history, "Brush");
        history.mark_saved();
        // A colour-mode conversion rewrites every layer and drops history.
        history.clear();
        assert!(
            history.is_dirty(),
            "with history gone we cannot prove the content matches the file"
        );
    }

    #[test]
    fn alpha_channel_command_round_trips_all_ops() {
        let mut channels = crate::core::channels::ChannelsState::default();
        let mut stack = LayerStack::new(4, 4);
        let mut width = 4;
        let mut height = 4;

        let id = channels.allocate_alpha_id();
        let channel = crate::core::channels::AlphaChannel {
            id,
            name: "Cut".into(),
            mask: vec![7u8; 16],
            width: 4,
            height: 4,
            revision: 0,
        };

        // Insert → undo → redo.
        let mut insert = AlphaChannelCommand::insert("New Channel", 0, channel);
        {
            let mut ctx = EditContext::new(&mut stack, &mut width, &mut height, None)
                .with_channels(&mut channels);
            insert.execute(&mut ctx).unwrap();
        }
        assert_eq!(channels.alpha.len(), 1);
        {
            let mut ctx = EditContext::new(&mut stack, &mut width, &mut height, None)
                .with_channels(&mut channels);
            insert.undo(&mut ctx).unwrap();
        }
        assert!(channels.alpha.is_empty());
        {
            let mut ctx = EditContext::new(&mut stack, &mut width, &mut height, None)
                .with_channels(&mut channels);
            insert.execute(&mut ctx).unwrap();
        }
        assert_eq!(channels.alpha[0].name, "Cut");
        assert_eq!(channels.alpha[0].id, id, "redo restores the same id");

        // Rename swaps back and forth and bumps the revision each time.
        let mut rename = AlphaChannelCommand::rename("Rename Channel", 0, "Edge".into());
        {
            let mut ctx = EditContext::new(&mut stack, &mut width, &mut height, None)
                .with_channels(&mut channels);
            rename.execute(&mut ctx).unwrap();
        }
        assert_eq!(channels.alpha[0].name, "Edge");
        {
            let mut ctx = EditContext::new(&mut stack, &mut width, &mut height, None)
                .with_channels(&mut channels);
            rename.undo(&mut ctx).unwrap();
        }
        assert_eq!(channels.alpha[0].name, "Cut");
        assert_eq!(channels.alpha[0].revision, 2);

        // Remove holds the channel for undo (mask preserved byte-exact).
        let mut remove = AlphaChannelCommand::remove("Delete Channel", 0);
        {
            let mut ctx = EditContext::new(&mut stack, &mut width, &mut height, None)
                .with_channels(&mut channels);
            remove.execute(&mut ctx).unwrap();
        }
        assert!(channels.alpha.is_empty());
        assert!(remove.memory_bytes() >= 16, "held mask counted in budget");
        {
            let mut ctx = EditContext::new(&mut stack, &mut width, &mut height, None)
                .with_channels(&mut channels);
            remove.undo(&mut ctx).unwrap();
        }
        assert_eq!(channels.alpha[0].mask, vec![7u8; 16]);

        // Without a channels context the command must refuse, not panic.
        let mut ctx = EditContext::new(&mut stack, &mut width, &mut height, None);
        assert!(AlphaChannelCommand::remove("x", 0)
            .execute(&mut ctx)
            .is_err());
    }

    #[test]
    fn alpha_plane_paint_command_writes_only_bbox_crop() {
        let mut channels = crate::core::channels::ChannelsState::default();
        let mut stack = LayerStack::new(4, 4);
        let mut width = 4;
        let mut height = 4;
        let id = channels.allocate_alpha_id();
        channels.insert_alpha(
            0,
            crate::core::channels::AlphaChannel {
                id,
                name: "Alpha".into(),
                mask: vec![0; 16],
                width: 4,
                height: 4,
                revision: 0,
            },
        );

        let mut cmd =
            AlphaPlanePaintCommand::new("Channel Paint", id, (1, 1, 3, 2), vec![0, 0], vec![9, 8]);
        {
            let mut ctx = EditContext::new(&mut stack, &mut width, &mut height, None)
                .with_channels(&mut channels);
            cmd.execute(&mut ctx).unwrap();
        }
        assert_eq!(channels.alpha[0].mask[5], 9);
        assert_eq!(channels.alpha[0].mask[6], 8);
        assert_eq!(channels.alpha[0].mask[4], 0);
        assert_eq!(channels.alpha[0].revision, 1);

        {
            let mut ctx = EditContext::new(&mut stack, &mut width, &mut height, None)
                .with_channels(&mut channels);
            cmd.undo(&mut ctx).unwrap();
        }
        assert_eq!(channels.alpha[0].mask[5], 0);
        assert_eq!(channels.alpha[0].mask[6], 0);
        assert_eq!(channels.alpha[0].revision, 2);
    }

    #[test]
    fn undo_failure_keeps_command_on_undo_stack() {
        let mut history = CommandHistory::new();
        history.push(Box::new(FailingCommand));
        let mut stack = LayerStack::new(1, 1);
        let mut width = 1;
        let mut height = 1;
        let mut ctx = empty_context(&mut stack, &mut width, &mut height);

        assert!(!history.undo(&mut ctx));
        assert_eq!(history.undo_count(), 1);
        assert_eq!(history.redo_count(), 0);
        assert_eq!(history.total_memory_bytes, 7);
    }

    #[test]
    fn redo_failure_keeps_command_on_redo_stack() {
        let mut history = CommandHistory::new();
        let id = history.mint_edit_id();
        history.redo_stack.push(StackEntry {
            id,
            cmd: Box::new(FailingCommand),
        });
        let mut stack = LayerStack::new(1, 1);
        let mut width = 1;
        let mut height = 1;
        let mut ctx = empty_context(&mut stack, &mut width, &mut height);

        assert!(!history.redo(&mut ctx));
        assert_eq!(history.undo_count(), 0);
        assert_eq!(history.redo_count(), 1);
    }

    #[test]
    fn delta_snapshot_counts_removed_tiles_as_change() {
        let before = crate::core::tile::TileMap::new_solid(1, 1, 255, 0, 0, 255);
        let after = crate::core::tile::TileMap::new(1, 1);
        let snap = DeltaSnapshot {
            layer_id: 1,
            target: crate::core::layer::PaintTarget::Pixels,
            before_tiles: before,
            after_tiles: after,
        };

        assert!(snap.has_changes());
        assert_eq!(snap.memory_bytes(), crate::core::tile::TILE_BYTES);
    }

    struct HugeCommand;

    impl Command for HugeCommand {
        fn execute(&mut self, _ctx: &mut EditContext) -> Result<(), String> {
            Ok(())
        }
        fn undo(&mut self, _ctx: &mut EditContext) -> Result<(), String> {
            Ok(())
        }
        fn label(&self) -> &str {
            "Huge"
        }
        fn memory_bytes(&self) -> usize {
            1 << 30 // 1 GB — alone over any budget
        }
    }

    #[test]
    fn eviction_never_drops_below_the_minimum_kept_entries() {
        // One over-budget command (e.g. a crop that rebuilt every layer) must
        // not evict the whole history down to itself.
        let mut history = CommandHistory::new();
        history.memory_budget_bytes = 64 * 1024 * 1024;
        for _ in 0..10 {
            history.push(Box::new(HugeCommand));
        }
        assert_eq!(
            history.undo_count(),
            MIN_KEPT_ENTRIES,
            "eviction floor not honoured"
        );
    }

    #[test]
    fn structure_command_bills_only_introduced_tiles() {
        let mut stack = LayerStack::new(512, 512);
        let idx = stack.add_layer(512, 512);

        // Structure-only change: every tile Arc is shared between before and
        // after, so the marginal cost is bookkeeping only. (The old union
        // accounting billed this at whole-document size, which evicted the
        // history down to a handful of entries on a large photo.)
        let mut cmd = LayerStructureCommand::capture_before("Reorder", &stack, 512, 512);
        cmd.capture_after(&stack, 512, 512);
        assert!(
            cmd.memory_bytes() < crate::core::tile::TILE_BYTES,
            "structure-only change billed pixel data: {}",
            cmd.memory_bytes()
        );

        // Pixel change: replacing one layer's tiles bills exactly the tile
        // DATA the command introduced — `new_solid` shares one Arc across all
        // four positions, so that's a single tile's bytes, far below the whole
        // stack the old union accounting would have billed (8 tiles).
        let mut cmd = LayerStructureCommand::capture_before("Fill", &stack, 512, 512);
        stack.layers[idx].tiles = crate::core::tile::TileMap::new_solid(512, 512, 10, 20, 30, 255);
        cmd.capture_after(&stack, 512, 512);
        let bytes = cmd.memory_bytes();
        let one_tile = crate::core::tile::TILE_BYTES;
        assert!(
            bytes >= one_tile && bytes < 2 * one_tile,
            "expected ~1 shared tile billed, got {bytes}"
        );
    }

    #[test]
    fn applied_layer_translate_uses_absolute_offsets_on_redo() {
        let mut stack = LayerStack::new(16, 16);
        let idx = stack.add_layer(16, 16);
        let id = stack.layers[idx].id;
        stack.layers[idx].offset = (8, 9);
        let mut width = 16;
        let mut height = 16;

        let mut cmd = TranslateLayerCommand::from_applied_move(vec![id], 5, 7, &stack);
        {
            let mut ctx = empty_context(&mut stack, &mut width, &mut height);
            cmd.undo(&mut ctx).unwrap();
        }
        assert_eq!(stack.layers[idx].offset, (3, 2));

        {
            let mut ctx = empty_context(&mut stack, &mut width, &mut height);
            cmd.execute(&mut ctx).unwrap();
            cmd.execute(&mut ctx).unwrap();
        }
        assert_eq!(stack.layers[idx].offset, (8, 9));
    }

    #[test]
    fn applied_selection_translate_uses_absolute_offsets_on_redo() {
        let mut stack = LayerStack::new(16, 16);
        let mut width = 16;
        let mut height = 16;
        let mut sel = crate::core::selection::Selection::new(16, 16);
        sel.offset = (5, 7);

        let mut cmd = TranslateSelectionCommand::from_applied_move(&sel, 5, 7);

        {
            let mut ctx = EditContext::new(&mut stack, &mut width, &mut height, Some(&mut sel));
            cmd.undo(&mut ctx).unwrap();
        }
        assert_eq!(sel.offset, (0, 0));

        {
            let mut ctx = EditContext::new(&mut stack, &mut width, &mut height, Some(&mut sel));
            cmd.execute(&mut ctx).unwrap();
            cmd.execute(&mut ctx).unwrap();
        }
        assert_eq!(sel.offset, (5, 7));
    }

    #[test]
    fn duplicate_and_move_redo_does_not_double_translate() {
        let mut history = CommandHistory::new();
        let mut stack = LayerStack::new(16, 16);
        let src_idx = stack.add_layer(16, 16);
        stack.layers[src_idx].selected = true;
        let mut width = 16;
        let mut height = 16;

        let mut structure =
            LayerStructureCommand::capture_before("Duplicate & Move", &stack, width, height);
        for layer in &mut stack.layers {
            layer.selected = false;
        }
        let dup_idx = stack.duplicate_layer(src_idx);
        let dup_id = stack.layers[dup_idx].id;
        stack.layers[dup_idx].selected = true;
        structure.capture_after(&stack, width, height);

        stack.layers[dup_idx].offset = (6, 0);
        let translate = TranslateLayerCommand::from_applied_move(vec![dup_id], 6, 0, &stack);

        history.begin_group("Duplicate & Move");
        history.push(Box::new(structure));
        history.push(Box::new(translate));
        history.end_group();

        {
            let mut ctx = empty_context(&mut stack, &mut width, &mut height);
            assert!(history.undo(&mut ctx));
        }
        assert_eq!(stack.layers.len(), 2);

        {
            let mut ctx = empty_context(&mut stack, &mut width, &mut height);
            assert!(history.redo(&mut ctx));
        }
        let dup = stack
            .layers
            .iter()
            .find(|layer| layer.id == dup_id)
            .unwrap();
        assert_eq!(dup.offset, (6, 0));
    }
}

//! Layer-structure commands: add/remove/duplicate/merge/reorder, adjustment
//! layers, per-layer flags. All record through the mutation gateway.

// Main document model — Canvas, History, metadata.
//

use super::*;
use iai_doc::layer::{AdjustmentType, Layer};
use iai_history::gateway::ChangeKind;

pub use iai_doc::layer::BlendMode;

impl Canvas {
    pub fn add_layer(&mut self) -> usize {
        self.layer_revision += 1;
        let mut _cmd = iai_history::command::LayerStructureCommand::capture_before(
            "Add Layer",
            &self.layer_stack,
            self.width,
            self.height,
        );
        let idx = self.layer_stack.add_layer(self.width, self.height);
        _cmd.capture_after(&self.layer_stack, self.width, self.height);
        self.record_as(Box::new(_cmd), ChangeKind::LayerStructure);
        self.flatten_full();
        idx
    }

    /// Delete the currently selected layers. Undoable.
    /// Logic:
    ///   1. Collect all selected layers.
    ///   2. If none → fall back to deleting the active layer.
    ///   3. Only push history if a layer was actually deleted.
    pub fn remove_active_layer(&mut self) -> bool {
        let mut to_remove: Vec<usize> = self
            .layer_stack
            .layers
            .iter()
            .enumerate()
            .filter(|(_, l)| l.selected)
            .map(|(i, _)| i)
            .collect();

        if to_remove.is_empty() {
            to_remove.push(self.layer_stack.active_idx);
        }
        let mut to_remove = self.layer_stack.with_group_contents(&to_remove);

        if to_remove.is_empty() {
            return false;
        }

        let mut _cmd = iai_history::command::LayerStructureCommand::capture_before(
            "Delete Layer",
            &self.layer_stack,
            self.width,
            self.height,
        );

        to_remove.sort_unstable_by(|a, b| b.cmp(a));
        let mut ok = false;
        for &idx in &to_remove {
            if self.layer_stack.remove_layer(idx) {
                ok = true;
            }
        }

        if ok {
            if let Some(active) = self.layer_stack.layers.get_mut(self.layer_stack.active_idx) {
                active.selected = true;
            }
            _cmd.capture_after(&self.layer_stack, self.width, self.height);
            self.record_as(Box::new(_cmd), ChangeKind::LayerStructure);
            self.flatten_full();
        }
        ok
    }

    /// Delete the layer at a specific idx (panel context menu / ✕ button). Undoable.
    /// If layer[idx] is selected → multi-delete all selected layers.
    /// If layer[idx] is not selected → single-delete only idx.
    pub fn delete_layer_at(&mut self, idx: usize) -> bool {
        let cannot_delete =
            idx >= self.layer_stack.layers.len() || self.layer_stack.layers.len() <= 1;
        if cannot_delete {
            return false;
        }

        let do_multi = self.layer_stack.layers.get(idx).is_some_and(|l| l.selected);
        let to_remove: Vec<usize> = if do_multi {
            self.layer_stack
                .layers
                .iter()
                .enumerate()
                .filter(|(_, l)| l.selected)
                .map(|(i, _)| i)
                .collect()
        } else {
            vec![idx]
        };
        let to_remove = self.layer_stack.with_group_contents(&to_remove);

        if to_remove.is_empty() {
            return false;
        }

        let mut cmd = iai_history::command::LayerStructureCommand::capture_before(
            "Delete Layer",
            &self.layer_stack,
            self.width,
            self.height,
        );
        let mut ok = false;
        let mut sorted = to_remove.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        for &i in &sorted {
            if self.layer_stack.remove_layer(i) {
                ok = true;
            }
        }

        if ok {
            if let Some(active) = self.layer_stack.layers.get_mut(self.layer_stack.active_idx) {
                active.selected = true;
            }
            cmd.capture_after(&self.layer_stack, self.width, self.height);
            self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
            self.flatten_full();
            self.layer_revision += 1;
        }
        ok
    }

    pub fn duplicate_active_layer(&mut self) -> usize {
        let mut _cmd = iai_history::command::LayerStructureCommand::capture_before(
            "Duplicate Layer",
            &self.layer_stack,
            self.width,
            self.height,
        );
        let idx = self
            .layer_stack
            .duplicate_layer(self.layer_stack.active_idx);
        _cmd.capture_after(&self.layer_stack, self.width, self.height);
        self.record_as(Box::new(_cmd), ChangeKind::LayerStructure);
        self.flatten_full();
        self.layer_revision += 1;
        idx
    }

    pub fn layer_via_copy(&mut self) -> usize {
        let mut _cmd = iai_history::command::LayerStructureCommand::capture_before(
            "Layer via Copy",
            &self.layer_stack,
            self.width,
            self.height,
        );

        // Whole objects: a selected folder is copied with its contents, and a
        // row inside it is not copied a second time on its own.
        let mut to_copy: Vec<usize> = self.layer_stack.selected_roots();

        if to_copy.is_empty() {
            to_copy.push(self.layer_stack.active_idx);
        }

        let mut last_new_idx = self.layer_stack.active_idx;

        for l in &mut self.layer_stack.layers {
            l.selected = false;
        }

        for &src_idx in to_copy.iter().rev() {
            if self
                .layer_stack
                .layers
                .get(src_idx)
                .is_some_and(|l| l.is_group())
            {
                let new_idx = self.layer_stack.duplicate_group(src_idx);
                self.layer_stack.layers[new_idx].selected = true;
                last_new_idx = new_idx;
                continue;
            }
            let new_idx = self.layer_stack.duplicate_layer(src_idx);
            self.layer_stack.layers[new_idx].is_background = false;
            self.layer_stack.layers[new_idx].selected = true;
            last_new_idx = new_idx;

            if self.selection.active {
                self.selection.refresh_bbox();
                let layer = &mut self.layer_stack.layers[new_idx];
                let ox = layer.offset.0;
                let oy = layer.offset.1;
                let bbox = self.selection.bounding_box_cached();
                let sel_x0 = bbox.0 as i32;
                let sel_y0 = bbox.1 as i32;
                let sel_x1 = bbox.2 as i32;
                let sel_y1 = bbox.3 as i32;

                layer.tiles.tiles.retain(|pos, _| {
                    let tx0 = pos.x * iai_raster::tile::TILE_SIZE as i32 + ox;
                    let ty0 = pos.y * iai_raster::tile::TILE_SIZE as i32 + oy;
                    let tx1 = tx0 + iai_raster::tile::TILE_SIZE as i32;
                    let ty1 = ty0 + iai_raster::tile::TILE_SIZE as i32;
                    !(tx1 <= sel_x0 || tx0 >= sel_x1 || ty1 <= sel_y0 || ty0 >= sel_y1)
                });

                let mut tiles_to_modify = Vec::new();
                for pos in layer.tiles.tiles.keys() {
                    tiles_to_modify.push(*pos);
                }

                let ts = iai_raster::tile::TILE_SIZE as i32;
                let cw = self.width as i32;
                let ch = self.height as i32;

                for pos in tiles_to_modify {
                    let tx0 = pos.x * ts + ox;
                    let ty0 = pos.y * ts + oy;
                    let tx1 = tx0 + ts - 1;

                    let tile_inside_bbox = tx0 >= sel_x0
                        && ty0 >= sel_y0
                        && (tx0 + ts) <= sel_x1
                        && (ty0 + ts) <= sel_y1;
                    if tile_inside_bbox {
                        let cx0 = tx0.clamp(0, cw - 1) as u32;
                        let cy0 = ty0.clamp(0, ch - 1) as u32;
                        let cx1 = tx1.clamp(0, cw - 1) as u32;
                        let cy1 = (ty0 + ts - 1).clamp(0, ch - 1) as u32;
                        if self.selection.sample(cx0, cy0) >= 1.0
                            && self.selection.sample(cx1, cy0) >= 1.0
                            && self.selection.sample(cx0, cy1) >= 1.0
                            && self.selection.sample(cx1, cy1) >= 1.0
                        {
                            continue;
                        }
                    }

                    let tile = layer.tiles.get_tile_mut(pos);
                    for i in 0..ts {
                        for j in 0..ts {
                            let cx = tx0 + j;
                            let cy = ty0 + i;
                            let idx = ((i * ts + j) * 4) as usize;
                            if cx >= 0 && cy >= 0 && cx < cw && cy < ch {
                                let sel = self.selection.sample(cx as u32, cy as u32);
                                if sel < 1.0 {
                                    let a = tile.pixels[idx + 3];
                                    tile.pixels[idx + 3] = (a as f32 * sel) as u8;
                                }
                            } else {
                                tile.pixels[idx + 3] = 0;
                            }
                        }
                    }
                }
            }
        }

        _cmd.capture_after(&self.layer_stack, self.width, self.height);
        self.record_as(Box::new(_cmd), ChangeKind::LayerStructure);
        self.flatten_full();
        self.layer_revision += 1;

        last_new_idx
    }

    /// Merge the layer at `idx` into the one below it. Undoable; records nothing
    /// when the merge is refused (group boundary, adjustment/locked below).
    pub fn merge_down(&mut self, idx: usize) -> bool {
        let mut _cmd = iai_history::command::LayerStructureCommand::capture_before(
            "Merge Down",
            &self.layer_stack,
            self.width,
            self.height,
        );
        let ok = self.layer_stack.merge_down(idx);
        if ok {
            self.ensure_16bit_layer_masters();
            _cmd.capture_after(&self.layer_stack, self.width, self.height);
            self.record_as(Box::new(_cmd), ChangeKind::LayerStructure);
            self.flatten_full();
        }
        ok
    }

    /// Ctrl+E: merge all selected layers into one.
    /// Returns true if anything changed.
    pub fn merge_selected(&mut self) -> bool {
        let mut cmd = iai_history::command::LayerStructureCommand::capture_before(
            "Merge Selected",
            &self.layer_stack,
            self.width,
            self.height,
        );
        let ok = self.layer_stack.merge_selected(self.width, self.height);
        if ok {
            self.ensure_16bit_layer_masters();
            cmd.capture_after(&self.layer_stack, self.width, self.height);
            self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
            self.flatten_full();
        }
        ok
    }

    /// Merge the folder at `idx` into one layer. Undoable; keeps a 16-bit
    /// document 16-bit.
    pub fn merge_group(&mut self, idx: usize) -> bool {
        let mut cmd = iai_history::command::LayerStructureCommand::capture_before(
            "Merge Group",
            &self.layer_stack,
            self.width,
            self.height,
        );
        let ok = self.layer_stack.merge_group(idx, self.width, self.height);
        if ok {
            self.ensure_16bit_layer_masters();
            cmd.capture_after(&self.layer_stack, self.width, self.height);
            self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
            self.flatten_full();
        }
        ok
    }

    /// Ctrl+J on a folder: duplicate the active group (header + children).
    /// Returns true if the active layer was a group and nothing else was
    /// selected with it; a mixed selection is left to `layer_via_copy`, which
    /// copies every selected object.
    pub fn duplicate_active_group(&mut self) -> bool {
        let idx = self.layer_stack.active_idx;
        if !self
            .layer_stack
            .layers
            .get(idx)
            .is_some_and(|l| l.is_group())
        {
            return false;
        }
        if self.layer_stack.selected_roots().iter().any(|&i| i != idx) {
            return false;
        }
        let mut cmd = iai_history::command::LayerStructureCommand::capture_before(
            "Duplicate Group",
            &self.layer_stack,
            self.width,
            self.height,
        );
        self.layer_stack.duplicate_group(idx);
        cmd.capture_after(&self.layer_stack, self.width, self.height);
        self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
        self.flatten_full();
        true
    }

    /// Ctrl+Shift+E: merge every eye-on layer into one. Returns true if changed.
    pub fn merge_visible(&mut self) -> bool {
        let mut cmd = iai_history::command::LayerStructureCommand::capture_before(
            "Merge Visible",
            &self.layer_stack,
            self.width,
            self.height,
        );
        let ok = if self.bit_depth == BitDepth::Sixteen {
            self.layer_stack.merge_visible16(self.width, self.height)
        } else {
            self.layer_stack.merge_visible(self.width, self.height)
        };
        if ok {
            cmd.capture_after(&self.layer_stack, self.width, self.height);
            self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
            self.flatten_full();
        }
        ok
    }

    pub fn add_adjustment_layer(&mut self, adj: AdjustmentType) -> usize {
        let mut _cmd = iai_history::command::LayerStructureCommand::capture_before(
            "Add Adjustment Layer",
            &self.layer_stack,
            self.width,
            self.height,
        );
        let idx = self
            .layer_stack
            .add_adjustment_layer(adj, self.width, self.height);
        _cmd.capture_after(&self.layer_stack, self.width, self.height);
        self.record_as(Box::new(_cmd), ChangeKind::LayerStructure);
        self.flatten_full();
        idx
    }

    pub fn set_layer_blend_mode(&mut self, idx: usize, mode: BlendMode) {
        self.layer_revision += 1;
        if idx < self.layer_stack.layers.len() {
            self.layer_stack.layers[idx].blend_mode = mode;
            self.flatten_full();
        }
    }

    pub fn set_layer_opacity(&mut self, idx: usize, opacity: f32) {
        self.layer_revision += 1;
        if idx < self.layer_stack.layers.len() {
            self.layer_stack.layers[idx].opacity = opacity.clamp(0.0, 1.0);
            self.dirty.expand_full(self.width, self.height);
        }
    }

    pub fn toggle_layer_visibility(&mut self, idx: usize) {
        if idx < self.layer_stack.layers.len() {
            self.layer_stack.layers[idx].visible = !self.layer_stack.layers[idx].visible;
            self.flatten_full();
        }
    }

    pub fn rename_layer(&mut self, idx: usize, name: &str) {
        self.layer_revision += 1;
        self.layer_stack.rename_layer(idx, name);
    }

    pub fn move_layer_up(&mut self, idx: usize) -> bool {
        self.layer_revision += 1;
        let mut _cmd = iai_history::command::LayerStructureCommand::capture_before(
            "Move Layer Up",
            &self.layer_stack,
            self.width,
            self.height,
        );
        let ok = self.layer_stack.move_layer_up(idx);
        if ok {
            _cmd.capture_after(&self.layer_stack, self.width, self.height);
            self.record_as(Box::new(_cmd), ChangeKind::LayerStructure);
            self.flatten_full();
        }
        ok
    }

    pub fn move_layer_down(&mut self, idx: usize) -> bool {
        self.layer_revision += 1;
        let mut _cmd = iai_history::command::LayerStructureCommand::capture_before(
            "Move Layer Down",
            &self.layer_stack,
            self.width,
            self.height,
        );
        let ok = self.layer_stack.move_layer_down(idx);
        if ok {
            _cmd.capture_after(&self.layer_stack, self.width, self.height);
            self.record_as(Box::new(_cmd), ChangeKind::LayerStructure);
            self.flatten_full();
        }
        ok
    }

    /// Nudge the selected (or active) movable layers by `(dx, dy)` canvas pixels —
    /// used by the Move tool's arrow-key shortcuts. One undoable "Move Layer"
    /// command per call. Returns false if nothing eligible moved.
    pub fn nudge_selected_layers(&mut self, dx: i32, dy: i32) -> bool {
        use iai_doc::layer::LayerType;
        if dx == 0 && dy == 0 {
            return false;
        }
        let movable = |l: &Layer| -> bool {
            !l.locked
                && !l.is_background
                && matches!(
                    l.layer_type,
                    LayerType::Raster
                        | LayerType::Text(_)
                        | LayerType::Vector(_)
                        | LayerType::SmartObject
                )
        };

        // A selected folder is nudged with everything in it.
        let mut ids: Vec<u32> = self
            .layer_stack
            .selected_with_group_contents()
            .into_iter()
            .map(|i| &self.layer_stack.layers[i])
            .filter(|l| movable(l))
            .map(|l| l.id)
            .collect();
        if ids.is_empty() {
            if let Some(l) = self.layer_stack.layers.get(self.layer_stack.active_idx) {
                if movable(l) {
                    ids.push(l.id);
                }
            }
        }
        if ids.is_empty() {
            return false;
        }

        let bounds: Vec<(i32, i32, u32, u32)> = self
            .layer_stack
            .layers
            .iter()
            .filter(|l| ids.contains(&l.id))
            .map(|l| (l.offset.0, l.offset.1, l.width, l.height))
            .collect();

        for l in self.layer_stack.layers.iter_mut() {
            if ids.contains(&l.id) {
                l.offset.0 += dx;
                l.offset.1 += dy;
            }
        }

        for (ox, oy, w, h) in bounds {
            self.mark_dirty_layer_bounds(ox, oy, w, h);
            self.mark_dirty_layer_bounds(ox + dx, oy + dy, w, h);
        }

        let cmd = iai_history::command::TranslateLayerCommand::from_applied_move(
            ids,
            dx,
            dy,
            &self.layer_stack,
        );
        self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
        self.layer_revision += 1;
        true
    }
}

#[cfg(test)]
mod folder_tests {
    use super::*;

    /// Background, then a folder holding "one" at (40, 50) and "two" at
    /// (150, 60), then "loose" on top. Only the folder's header row is
    /// selected, as right after Ctrl+G. Returns the header's index.
    fn canvas_with_folder() -> (Canvas, usize) {
        let mut canvas = Canvas::new(300, 300);
        let stack = &mut canvas.layer_stack;
        for (name, offset) in [("one", (40, 50)), ("two", (150, 60))] {
            let id = stack.next_id();
            let mut layer = Layer::from_rgba(id, name, [9u8, 9, 9, 255].repeat(100), 10, 10);
            layer.offset = offset;
            layer.selected = true;
            stack.layers.push(layer);
            stack.set_next_id(id + 1);
        }
        let header = stack
            .create_group_from_selected(300, 300)
            .expect("two layers group");
        let id = stack.next_id();
        let mut loose = Layer::from_rgba(id, "loose", [9u8, 9, 9, 255].repeat(100), 10, 10);
        loose.offset = (200, 200);
        stack.layers.push(loose);
        stack.set_next_id(id + 1);
        stack.active_idx = header;
        (canvas, header)
    }

    fn offset_of(canvas: &Canvas, name: &str) -> (i32, i32) {
        canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.name == name)
            .map(|l| l.offset)
            .expect("layer exists")
    }

    #[test]
    fn nudging_a_selected_folder_moves_everything_in_it() {
        let (mut canvas, _) = canvas_with_folder();
        assert!(canvas.nudge_selected_layers(3, -2));
        assert_eq!(offset_of(&canvas, "one"), (43, 48));
        assert_eq!(offset_of(&canvas, "two"), (153, 58));
        assert_eq!(offset_of(&canvas, "loose"), (200, 200), "not in the folder");

        canvas.undo();
        assert_eq!(offset_of(&canvas, "one"), (40, 50));
        assert_eq!(offset_of(&canvas, "two"), (150, 60));
    }

    #[test]
    fn layer_via_copy_takes_a_folder_and_a_loose_layer_together() {
        // Folder + another layer selected: Ctrl+J used to copy the folder
        // alone, or leave an empty copy of its header.
        let (mut canvas, header) = canvas_with_folder();
        let source_group = canvas.layer_stack.layers[header].id;
        let loose = canvas.layer_stack.layers.len() - 1;
        canvas.layer_stack.layers[loose].selected = true;

        assert!(
            !canvas.duplicate_active_group(),
            "a mixed selection is not the folder-only shortcut"
        );
        canvas.layer_via_copy();

        let layers = &canvas.layer_stack.layers;
        assert_eq!(
            layers.len(),
            9,
            "Background + (folder of two + loose) twice"
        );
        let copy = layers
            .iter()
            .find(|l| l.is_group() && l.id != source_group)
            .expect("the folder was copied");
        let inside = layers
            .iter()
            .filter(|l| l.parent_id == Some(copy.id))
            .count();
        assert_eq!(inside, 2, "the copied folder holds copies of both layers");
        let originals = layers
            .iter()
            .filter(|l| l.parent_id == Some(source_group))
            .count();
        assert_eq!(originals, 2, "nothing was copied into the source folder");
        let selected: Vec<&str> = layers
            .iter()
            .filter(|l| l.selected)
            .map(|l| l.name.as_str())
            .collect();
        assert_eq!(selected, vec!["Group copy", "loose copy"]);
    }

    #[test]
    fn layer_via_copy_on_a_folder_alone_duplicates_it() {
        let (mut canvas, _) = canvas_with_folder();
        assert!(canvas.duplicate_active_group());
        assert_eq!(canvas.layer_stack.layers.len(), 8);
    }
}

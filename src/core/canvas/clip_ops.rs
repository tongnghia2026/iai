//! Typed clipping/attachment relations for PowerClip foundation.
//!
//! Group membership remains `Layer::parent_id`; clipping uses the independent
//! `clip_parent_id` link so reorder and future nesting cannot confuse the two.

use super::LayerStack;
use crate::core::layer::LayerType;

impl LayerStack {
    /// Whether linking `child_id` inside `frame_id` would preserve a finite,
    /// acyclic clip graph.
    pub fn can_attach_clipped_child(&self, child_id: u32, frame_id: u32) -> bool {
        if child_id == frame_id {
            return false;
        }
        let Some(child) = self.layers.iter().find(|layer| layer.id == child_id) else {
            return false;
        };
        let Some(frame) = self.layers.iter().find(|layer| layer.id == frame_id) else {
            return false;
        };
        if child.is_group() || frame.is_group() || child.is_background {
            return false;
        }

        // Following the prospective frame chain must never arrive back at the
        // child. Missing links are rejected rather than silently accepted.
        let mut current = Some(frame.id);
        let mut visited = std::collections::HashSet::new();
        while let Some(id) = current {
            if id == child_id || !visited.insert(id) {
                return false;
            }
            let Some(layer) = self.layers.iter().find(|layer| layer.id == id) else {
                return false;
            };
            current = layer.clip_parent_id;
        }
        true
    }

    pub fn attach_clipped_child(&mut self, child_id: u32, frame_id: u32) -> Result<(), String> {
        if !self.can_attach_clipped_child(child_id, frame_id) {
            return Err("Invalid or cyclic PowerClip relation".to_string());
        }
        let child = self
            .layers
            .iter_mut()
            .find(|layer| layer.id == child_id)
            .ok_or_else(|| "PowerClip child layer not found".to_string())?;
        child.clip_parent_id = Some(frame_id);
        Ok(())
    }

    pub fn release_clipped_child(&mut self, child_id: u32) -> Result<u32, String> {
        let child = self
            .layers
            .iter_mut()
            .find(|layer| layer.id == child_id)
            .ok_or_else(|| "PowerClip child layer not found".to_string())?;
        child
            .clip_parent_id
            .take()
            .ok_or_else(|| "Layer is not inside a PowerClip frame".to_string())
    }

    /// Create an empty raster content layer immediately above `frame_id` and
    /// attach it in one model operation. Commands wrap this atomically for undo.
    pub fn create_clipped_pixel_child(
        &mut self,
        frame_id: u32,
        width: u32,
        height: u32,
    ) -> Result<u32, String> {
        let frame_idx = self
            .layers
            .iter()
            .position(|layer| layer.id == frame_id)
            .ok_or_else(|| "PowerClip frame layer not found".to_string())?;
        if matches!(self.layers[frame_idx].layer_type, LayerType::Group) {
            return Err("A group cannot be a PowerClip frame".to_string());
        }
        let group_parent = self.layers[frame_idx].parent_id;
        self.active_idx = frame_idx;
        let child_idx = self.add_layer(width, height);
        let child_id = self.layers[child_idx].id;
        self.layers[child_idx].name = "Pixel Paint inside".to_string();
        self.layers[child_idx].parent_id = group_parent;
        if let Err(error) = self.attach_clipped_child(child_id, frame_id) {
            self.layers.remove(child_idx);
            self.active_idx = frame_idx.min(self.layers.len().saturating_sub(1));
            return Err(error);
        }
        Ok(child_id)
    }

    /// Clear dangling/self/cyclic clip links after untrusted load or structural
    /// edits. Valid relations are never rewritten.
    pub fn repair_clip_relations(&mut self) {
        let snapshot: std::collections::HashMap<u32, (Option<u32>, bool, bool)> = self
            .layers
            .iter()
            .map(|layer| {
                (
                    layer.id,
                    (layer.clip_parent_id, layer.is_group(), layer.is_background),
                )
            })
            .collect();
        let ids: std::collections::HashSet<u32> = snapshot.keys().copied().collect();
        for layer in &mut self.layers {
            let Some(parent) = layer.clip_parent_id else {
                continue;
            };
            let mut current = Some(parent);
            let mut visited = std::collections::HashSet::new();
            visited.insert(layer.id);
            let parent_is_group = snapshot.get(&parent).is_some_and(|entry| entry.1);
            let mut valid = ids.contains(&parent)
                && !parent_is_group
                && !layer.is_group()
                && !layer.is_background;
            while valid {
                let Some(id) = current else {
                    break;
                };
                if !visited.insert(id) {
                    valid = false;
                    break;
                }
                current = snapshot.get(&id).and_then(|entry| entry.0);
            }
            if !valid {
                layer.clip_parent_id = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::core::layer::LayerStack;

    #[test]
    fn relation_is_distinct_from_group_membership_and_reorder_safe() {
        let mut stack = LayerStack::new(64, 64);
        let frame = stack.add_layer(64, 64);
        let frame_id = stack.layers[frame].id;
        let child_id = stack.create_clipped_pixel_child(frame_id, 64, 64).unwrap();
        let child = stack
            .layers
            .iter()
            .find(|layer| layer.id == child_id)
            .unwrap();
        assert_eq!(child.clip_parent_id, Some(frame_id));
        assert_eq!(child.parent_id, None);

        let child_idx = stack.layers.iter().position(|l| l.id == child_id).unwrap();
        assert!(stack.move_layer_to(child_idx, 0));
        assert_eq!(
            stack
                .layers
                .iter()
                .find(|layer| layer.id == child_id)
                .unwrap()
                .clip_parent_id,
            Some(frame_id)
        );
    }

    #[test]
    fn cycles_are_rejected_and_deleting_frame_releases_child() {
        let mut stack = LayerStack::new(64, 64);
        let a = stack.add_layer(64, 64);
        let aid = stack.layers[a].id;
        let b = stack.add_layer(64, 64);
        let bid = stack.layers[b].id;
        stack.attach_clipped_child(bid, aid).unwrap();
        assert!(stack.attach_clipped_child(aid, bid).is_err());

        let a_idx = stack.layers.iter().position(|l| l.id == aid).unwrap();
        assert!(stack.remove_layer(a_idx));
        assert_eq!(
            stack
                .layers
                .iter()
                .find(|l| l.id == bid)
                .unwrap()
                .clip_parent_id,
            None
        );
    }

    #[test]
    fn paste_remaps_internal_clip_links() {
        let mut source = LayerStack::new(64, 64);
        let frame = source.add_layer(64, 64);
        let frame_id = source.layers[frame].id;
        let child_id = source.create_clipped_pixel_child(frame_id, 64, 64).unwrap();
        let block = vec![
            source
                .layers
                .iter()
                .find(|l| l.id == frame_id)
                .unwrap()
                .clone(),
            source
                .layers
                .iter()
                .find(|l| l.id == child_id)
                .unwrap()
                .clone(),
        ];

        let mut target = LayerStack::new(64, 64);
        target.add_layer(64, 64);
        target.add_layer(64, 64);
        target.paste_layers(&block);
        let child = target.layers.last().unwrap();
        let frame = &target.layers[target.layers.len() - 2];
        assert_eq!(child.clip_parent_id, Some(frame.id));
        assert_ne!(frame.id, frame_id);
    }

    #[test]
    fn duplicate_group_remaps_clip_child_to_copied_frame() {
        let mut stack = LayerStack::new(64, 64);
        let frame_idx = stack.add_layer(64, 64);
        let frame_id = stack.layers[frame_idx].id;
        let child_id = stack.create_clipped_pixel_child(frame_id, 64, 64).unwrap();
        for layer in &mut stack.layers {
            layer.selected = layer.id == frame_id || layer.id == child_id;
        }
        let group_idx = stack.create_group_from_selected(64, 64).unwrap();
        let copy_group_idx = stack.duplicate_group(group_idx);
        let copy_range = stack.group_member_range(copy_group_idx);
        let copied: Vec<_> = stack.layers[copy_range].iter().collect();
        let copied_child = copied
            .iter()
            .find(|layer| layer.clip_parent_id.is_some())
            .unwrap();
        let copied_frame_id = copied_child.clip_parent_id.unwrap();
        assert_ne!(copied_frame_id, frame_id);
        assert!(copied.iter().any(|layer| layer.id == copied_frame_id));
    }
}

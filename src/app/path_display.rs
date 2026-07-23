//! Cached screen-resolution display raster for the active editable Path.

use crate::core::layer::{BlendMode, LayerType};
use crate::tools::ToolId;

use super::state::{PathDisplayCacheEntry, PathDisplayCacheKey};
use super::App;

impl App {
    pub(in crate::app) fn active_path_display(&mut self) -> Option<crate::ui::PathDisplayRaster> {
        let tool = self.edit.tools.active_id();
        if !matches!(tool, ToolId::Move | ToolId::Node | ToolId::Pen)
            || (tool == ToolId::Pen && !self.edit.tools.pen().is_empty())
            || self.edit.transform_state.is_some()
            || self.edit.path_transform.is_some()
        {
            return None;
        }
        let scale = crate::core::vector::display::zoom_bucket(self.edit.view.zoom)?;
        let doc = &self.docs.documents[self.docs.active_doc_idx];
        let stack = &doc.canvas.layer_stack;
        let active_idx = stack.active_idx;
        let layer = stack.layers.get(active_idx)?;
        let LayerType::Path(object) = &layer.layer_type else {
            return None;
        };
        let ancestors_are_plain = {
            let mut parent_id = layer.parent_id;
            let mut plain = true;
            while let Some(id) = parent_id {
                let Some(parent) = stack.layers.iter().find(|candidate| candidate.id == id) else {
                    plain = false;
                    break;
                };
                if !parent.visible
                    || parent.mask.is_some()
                    || (parent.opacity - 1.0).abs() > 1e-3
                    || parent.blend_mode != BlendMode::Normal
                {
                    plain = false;
                    break;
                }
                parent_id = parent.parent_id;
            }
            plain
        };
        let painted_layer_above = stack
            .layers
            .iter()
            .enumerate()
            .skip(active_idx + 1)
            .any(|(idx, candidate)| stack.is_effectively_visible(idx) && !candidate.is_group());
        if !stack.is_effectively_visible(active_idx)
            || painted_layer_above
            || layer.mask.is_some()
            || (layer.opacity - 1.0).abs() > 1e-3
            || layer.blend_mode != BlendMode::Normal
            || (object.style.opacity - 1.0).abs() > 1e-3
            || !ancestors_are_plain
        {
            return None;
        }
        let key = PathDisplayCacheKey {
            doc_id: doc.id.0,
            layer_id: layer.id,
            scale,
            layer_offset: layer.offset,
            object: object.clone(),
        };
        let cache_hit = self
            .shell
            .ui_data_cache
            .path_display
            .as_ref()
            .is_some_and(|entry| entry.key == key);
        if !cache_hit && self.edit.node_drag.is_none() {
            let raster = crate::core::vector::display::rasterize_for_display(object, scale)?;
            self.shell.ui_data_cache.path_display_serial =
                self.shell.ui_data_cache.path_display_serial.wrapping_add(1);
            let inv = 1.0 / scale as f32;
            let tiles = crate::core::vector::display::split_display_tiles(
                &raster.rgba,
                raster.width,
                raster.height,
            )
            .into_iter()
            .map(|tile| crate::ui::PathDisplayTile {
                rgba: std::sync::Arc::new(tile.rgba),
                x: tile.x,
                y: tile.y,
                width: tile.width,
                height: tile.height,
            })
            .collect();
            let display = crate::ui::PathDisplayRaster {
                cache_key: self.shell.ui_data_cache.path_display_serial,
                tiles: std::sync::Arc::new(tiles),
                canvas_x: layer.offset.0 as f32 + raster.offset.0 as f32 * inv,
                canvas_y: layer.offset.1 as f32 + raster.offset.1 as f32 * inv,
                canvas_w: raster.width as f32 * inv,
                canvas_h: raster.height as f32 * inv,
                raster_w: raster.width,
                raster_h: raster.height,
            };
            self.shell.ui_data_cache.path_display = Some(PathDisplayCacheEntry {
                key: key.clone(),
                display,
            });
        }
        self.shell
            .ui_data_cache
            .path_display
            .as_ref()
            .filter(|entry| entry.key == key)
            .map(|entry| entry.display.clone())
    }
}

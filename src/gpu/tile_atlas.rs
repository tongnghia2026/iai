#![allow(dead_code)]
use crate::core::tile::TilePos;
use std::collections::{HashMap, VecDeque};

pub const ATLAS_SLOT_SIZE: u32 = 256;
pub const ATLAS_GRID_W: u32 = 64;
pub const ATLAS_GRID_H: u32 = 64;
pub const ATLAS_SLOT_COUNT: usize = (ATLAS_GRID_W * ATLAS_GRID_H) as usize;
pub const TILE_ATLAS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

#[derive(Clone, Copy)]
pub struct AtlasSlot {
    pub layer_id: usize,
    pub pos: TilePos,
    pub revision: u64,
    pub last_used: u64,
}

pub struct TileAtlas {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub bind_group: wgpu::BindGroup,
    pub slots: Vec<Option<AtlasSlot>>,
    /// Slot indices that are genuinely unused — O(1) pop on cache miss.
    /// Separated from LRU so cache hits need zero queue manipulation.
    pub free_slots: VecDeque<usize>,
    pub mapping: HashMap<(usize, TilePos), usize>,
    pub frame: u64,
    pub grid_w: u32,
    pub grid_h: u32,
    pub slot_count: usize,
}

impl TileAtlas {
    /// `max_texture_dimension` — queried from `adapter.limits().max_texture_dimension_2d`.
    /// `scene_view` fills the group's third binding (the Develop scene master);
    /// pass the compositor's 1×1 dummy outside a RAW session.
    pub fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        scene_view: &wgpu::TextureView,
        max_texture_dimension: u32,
    ) -> Self {
        let slot = ATLAS_SLOT_SIZE;
        let max_dim = (ATLAS_GRID_W * slot)
            .min(max_texture_dimension.max(slot))
            .min(8192);
        let mut dim = (max_dim / slot).max(1) * slot;

        let texture = loop {
            let scope = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("TileAtlas"),
                size: wgpu::Extent3d {
                    width: dim,
                    height: dim,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: TILE_ATLAS_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let oom = pollster::block_on(scope.pop());
            if oom.is_none() || dim <= slot * 4 {
                break tex;
            }
            drop(tex);
            dim = (dim / 2 / slot).max(1) * slot;
        };

        let grid_w = dim / slot;
        let grid_h = dim / slot;
        let slot_count = (grid_w * grid_h) as usize;
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = Self::build_bind_group(device, layout, &view, sampler, scene_view);

        let mut free_slots = VecDeque::with_capacity(slot_count);
        for i in 0..slot_count {
            free_slots.push_back(i);
        }

        Self {
            texture,
            view,
            bind_group,
            slots: vec![None; slot_count],
            free_slots,
            mapping: HashMap::new(),
            frame: 0,
            grid_w,
            grid_h,
            slot_count,
        }
    }

    pub fn clear(&mut self) {
        self.slots.fill(None);
        self.free_slots.clear();
        self.free_slots.extend(0..self.slot_count);
        self.mapping.clear();
    }

    fn build_bind_group(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        atlas_view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
        scene_view: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("TileAtlas_bg"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(scene_view),
                },
            ],
        })
    }

    /// Swap the scene-master binding (session start/end) without touching the
    /// atlas texture or slot state.
    pub fn rebind_scene(
        &mut self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        scene_view: &wgpu::TextureView,
    ) {
        self.bind_group = Self::build_bind_group(device, layout, &self.view, sampler, scene_view);
    }

    /// Returns (slot_x, slot_y, needs_upload).
    ///
    /// Cache hit  → O(1): update last_used, no queue scan.
    /// Cache miss → O(1) if free slots remain; O(slot_count) eviction only when atlas full
    ///              (for A4 300 DPI + 10 layers ≈ 1400 tiles < 4096 slots → eviction never fires).
    pub fn get_or_allocate(
        &mut self,
        layer_id: usize,
        pos: TilePos,
        revision: u64,
    ) -> (u32, u32, bool) {
        self.frame += 1;

        let key = (layer_id, pos);

        if let Some(&slot_idx) = self.mapping.get(&key) {
            let slot = self.slots[slot_idx].as_mut().unwrap();
            slot.last_used = self.frame;

            let needs_upload = slot.revision != revision;
            if needs_upload {
                slot.revision = revision;
            }

            let slot_x = (slot_idx as u32) % self.grid_w;
            let slot_y = (slot_idx as u32) / self.grid_w;
            return (slot_x, slot_y, needs_upload);
        }

        let slot_idx = if let Some(free) = self.free_slots.pop_front() {
            free
        } else {
            self.evict_lru()
        };

        self.slots[slot_idx] = Some(AtlasSlot {
            layer_id,
            pos,
            revision,
            last_used: self.frame,
        });
        self.mapping.insert(key, slot_idx);

        let slot_x = (slot_idx as u32) % self.grid_w;
        let slot_y = (slot_idx as u32) / self.grid_w;
        (slot_x, slot_y, true)
    }

    /// Linear scan for the slot with the smallest last_used value.
    /// Only called when all 4096 slots are occupied — essentially never for typical canvases.
    fn evict_lru(&mut self) -> usize {
        let evict_idx = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.map(|s| (i, s.last_used)))
            .min_by_key(|&(_, lu)| lu)
            .map(|(i, _)| i)
            .unwrap_or(0);

        if let Some(old) = self.slots[evict_idx] {
            self.mapping.remove(&(old.layer_id, old.pos));
        }
        evict_idx
    }

    pub fn upload_tile(&self, queue: &wgpu::Queue, slot_x: u32, slot_y: u32, pixels: &[u8]) {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: slot_x * ATLAS_SLOT_SIZE,
                    y: slot_y * ATLAS_SLOT_SIZE,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(ATLAS_SLOT_SIZE * 4),
                rows_per_image: Some(ATLAS_SLOT_SIZE),
            },
            wgpu::Extent3d {
                width: ATLAS_SLOT_SIZE,
                height: ATLAS_SLOT_SIZE,
                depth_or_array_layers: 1,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tile_atlas_uploads_cpu_bytes_as_srgb_texture() {
        assert_eq!(
            super::TILE_ATLAS_FORMAT,
            wgpu::TextureFormat::Rgba8UnormSrgb
        );
    }
}

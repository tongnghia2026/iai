#![allow(dead_code)]
use crate::core::tile::TilePos;
use std::collections::{HashMap, VecDeque};
use wgpu::util::DeviceExt;

pub const ATLAS_SLOT_SIZE: u32 = 256;
pub const ATLAS_GRID_W: u32 = 64;
pub const ATLAS_GRID_H: u32 = 64;
pub const ATLAS_SLOT_COUNT: usize = (ATLAS_GRID_W * ATLAS_GRID_H) as usize;
pub const TILE_ATLAS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// Byte-space view of the same texels: the zoomed-out display filter and the
/// mip builder average stored sRGB bytes directly (Photoshop-style), with no
/// transfer-curve round trip per texel.
pub const TILE_ATLAS_RAW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// Mip levels kept per slot (256 → 8 px). Must match `ATLAS_MIP_LEVELS` in
/// compositor.wgsl.
pub const ATLAS_MIP_LEVELS: u32 = 6;

/// Per-upload mip-builder record: the texels of the slot that belong to the
/// layer (edge tiles are partial) and whether the slot holds a layer mask,
/// which averages its value directly instead of alpha-weighted colour.
pub fn slot_mip_info(valid_w: u32, valid_h: u32, is_mask: bool) -> u32 {
    let w = valid_w.clamp(1, ATLAS_SLOT_SIZE);
    let h = valid_h.clamp(1, ATLAS_SLOT_SIZE);
    w | (h << 16) | (u32::from(is_mask) << 31)
}

const MIP_SHADER: &str = r#"
struct MipLevel {
    level: u32,
    grid_w: u32,
    tile: u32,
    dim: u32,
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var<uniform> lv: MipLevel;
// (slot index, slot_mip_info) per slot to rebuild.
@group(0) @binding(2) var<storage, read> entries: array<vec2<u32>>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) @interpolate(flat) entry: u32,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, @builtin(instance_index) entry: u32) -> VsOut {
    let slot = entries[entry].x;
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let origin = vec2<f32>(f32(slot % lv.grid_w), f32(slot / lv.grid_w)) * f32(lv.tile);
    let ndc = (origin + corner * f32(lv.tile)) / f32(lv.dim) * 2.0 - 1.0;
    var out: VsOut;
    out.pos = vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
    out.entry = entry;
    return out;
}

// One destination texel = the 2×2 texels below it that lie inside the layer.
// Colour is alpha-weighted so transparent pixels don't darken edges; masks
// average their value as-is. Works on whole byte values and rounds half to
// even: float ties (x.5 arriving as x.4999…) would otherwise all round down
// and darken every level a little more than the one before.
@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let e = entries[in.entry];
    let slot = e.x;
    let info = e.y;
    let origin = vec2<u32>(slot % lv.grid_w, slot / lv.grid_w) * lv.tile;
    let local = vec2<u32>(in.pos.xy) - origin;
    let prev = lv.level - 1u;
    let round_up = (1u << prev) - 1u;
    let valid = vec2<u32>(
        ((info & 0x1FFu) + round_up) >> prev,
        (((info >> 16u) & 0x1FFu) + round_up) >> prev,
    );
    let is_mask = (info >> 31u) != 0u;
    var plain = vec3<f32>(0.0);
    var weighted = vec3<f32>(0.0);
    var alpha = 0.0;
    var n = 0.0;
    for (var dy = 0u; dy < 2u; dy = dy + 1u) {
        for (var dx = 0u; dx < 2u; dx = dx + 1u) {
            let c = local * 2u + vec2<u32>(dx, dy);
            if (c.x < valid.x && c.y < valid.y) {
                let t = round(textureLoad(src, vec2<i32>(origin * 2u + c), 0) * 255.0);
                plain = plain + t.rgb;
                weighted = weighted + t.rgb * t.a;
                alpha = alpha + t.a;
                n = n + 1.0;
            }
        }
    }
    if (n == 0.0) {
        return vec4<f32>(0.0);
    }
    // n is 1, 2 or 4: multiply by its exact reciprocal (GPU division is not
    // correctly rounded, which turns x.5 ties into x.4999…).
    let inv_n = select(select(1.0, 0.5, n == 2.0), 0.25, n == 4.0);
    var rgb = plain * inv_n;
    if (!is_mask && alpha != 255.0 * n) {
        rgb = select(vec3<f32>(0.0), weighted / alpha, alpha > 0.0);
    }
    return vec4<f32>(round(rgb), round(alpha * inv_n)) / 255.0;
}
"#;

/// GPU mip builder for the atlas. Levels are rebuilt per slot right after its
/// level-0 upload, in the same command stream as the passes that sample it.
struct MipBuilder {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    /// Byte-space single-level views, index = mip level.
    level_views: Vec<wgpu::TextureView>,
    /// Constant per-level parameters, index = destination level (0 unused).
    level_params: Vec<wgpu::Buffer>,
}

impl MipBuilder {
    fn new(device: &wgpu::Device, texture: &wgpu::Texture, dim: u32, grid_w: u32) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("atlas_mip_shader"),
            source: wgpu::ShaderSource::Wgsl(MIP_SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("atlas_mip_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("atlas_mip_pl"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("atlas_mip_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: TILE_ATLAS_RAW_FORMAT,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let level_views = (0..ATLAS_MIP_LEVELS)
            .map(|level| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("atlas_mip_level"),
                    format: Some(TILE_ATLAS_RAW_FORMAT),
                    base_mip_level: level,
                    mip_level_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        let level_params = (0..ATLAS_MIP_LEVELS)
            .map(|level| {
                let params = [
                    level,
                    grid_w,
                    ATLAS_SLOT_SIZE >> level,
                    (dim >> level).max(1),
                ];
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("atlas_mip_level_params"),
                    contents: bytemuck::cast_slice(&params),
                    usage: wgpu::BufferUsages::UNIFORM,
                })
            })
            .collect();
        Self {
            pipeline,
            layout,
            level_views,
            level_params,
        }
    }
}

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
    /// All mip levels, viewed as plain bytes (see [`TILE_ATLAS_RAW_FORMAT`]).
    pub raw_view: wgpu::TextureView,
    /// Bilinear sampler for explicit-level reads of `raw_view`.
    raw_sampler: wgpu::Sampler,
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
    mips: MipBuilder,
    /// Latest [`slot_mip_info`] per slot.
    mip_info: Vec<u32>,
    /// Slots whose mips are stale (uploaded since the last
    /// [`Self::encode_mips`]), each listed once.
    mip_stale: Vec<bool>,
    stale_slots: Vec<u32>,
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
                mip_level_count: ATLAS_MIP_LEVELS,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: TILE_ATLAS_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[TILE_ATLAS_RAW_FORMAT],
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
        let raw_view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("TileAtlas_raw"),
            format: Some(TILE_ATLAS_RAW_FORMAT),
            ..Default::default()
        });
        let raw_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("TileAtlas_raw_sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bind_group = Self::build_bind_group(
            device,
            layout,
            &view,
            sampler,
            scene_view,
            &raw_view,
            &raw_sampler,
        );
        let mips = MipBuilder::new(device, &texture, dim, grid_w);

        let mut free_slots = VecDeque::with_capacity(slot_count);
        for i in 0..slot_count {
            free_slots.push_back(i);
        }

        Self {
            texture,
            view,
            raw_view,
            raw_sampler,
            bind_group,
            slots: vec![None; slot_count],
            free_slots,
            mapping: HashMap::new(),
            frame: 0,
            grid_w,
            grid_h,
            slot_count,
            mips,
            mip_info: vec![0; slot_count],
            mip_stale: vec![false; slot_count],
            stale_slots: Vec::new(),
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
        raw_view: &wgpu::TextureView,
        raw_sampler: &wgpu::Sampler,
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
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(raw_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(raw_sampler),
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
        self.bind_group = Self::build_bind_group(
            device,
            layout,
            &self.view,
            sampler,
            scene_view,
            &self.raw_view,
            &self.raw_sampler,
        );
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

    /// Upload a slot's level 0 and mark its mips stale (`mip_info` from
    /// [`slot_mip_info`]). They are rebuilt by the next [`Self::encode_mips`],
    /// which must precede any pass that samples a level above 0.
    pub fn upload_tile(
        &mut self,
        queue: &wgpu::Queue,
        slot_x: u32,
        slot_y: u32,
        pixels: &[u8],
        mip_info: u32,
    ) {
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
        let slot = (slot_y * self.grid_w + slot_x) as usize;
        self.mip_info[slot] = mip_info;
        if !self.mip_stale[slot] {
            self.mip_stale[slot] = true;
            self.stale_slots.push(slot as u32);
        }
    }

    /// Record the mip rebuild of every stale slot. Level-0 uploads are staged
    /// by the queue ahead of the submission that carries `encoder`, so
    /// recording this before a zoomed-out pass reads fresh texels at every
    /// level. Slots never sampled zoomed out stay stale at no cost.
    pub fn encode_mips(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) {
        if self.stale_slots.is_empty() {
            return;
        }
        let entries: Vec<[u32; 2]> = self
            .stale_slots
            .iter()
            .map(|&slot| [slot, self.mip_info[slot as usize]])
            .collect();
        for &slot in &self.stale_slots {
            self.mip_stale[slot as usize] = false;
        }
        self.stale_slots.clear();

        let entry_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("atlas_mip_entries"),
            contents: bytemuck::cast_slice(&entries),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let count = entries.len() as u32;
        for level in 1..ATLAS_MIP_LEVELS as usize {
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("atlas_mip_bg"),
                layout: &self.mips.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(
                            &self.mips.level_views[level - 1],
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.mips.level_params[level].as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: entry_buf.as_entire_binding(),
                    },
                ],
            });
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("atlas_mip_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.mips.level_views[level],
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                ..Default::default()
            });
            rpass.set_pipeline(&self.mips.pipeline);
            rpass.set_bind_group(0, &bind_group, &[]);
            rpass.draw(0..4, 0..count);
        }
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

    #[test]
    fn slot_mip_info_packs_extent_and_mask_flag() {
        let info = super::slot_mip_info(200, 17, true);
        assert_eq!(info & 0x1FF, 200);
        assert_eq!((info >> 16) & 0x1FF, 17);
        assert_eq!(info >> 31, 1);
        assert_eq!(super::slot_mip_info(0, 999, false), 1 | (256 << 16));
    }

    #[test]
    fn mip_shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(super::MIP_SHADER).expect("parse");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("validate");
    }
}

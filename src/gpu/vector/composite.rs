//! Live-compositor stage: draw a run of eligible vector layers straight into the
//! ping/pong accumulator at the correct z-position (Phase 2/3).
//!
//! A run is rendered to an owned MSAA target, resolved to a premultiplied texture,
//! then composited over the current accumulator buffer into the other buffer with
//! a shader that mirrors `compositor.wgsl`'s straight-alpha sRGB Normal blend — so
//! the vector run behaves exactly like one raster layer in the ping/pong loop
//! (one parity flip).
//!
//! ## Phase 3 — cache GPU buffers, keep transforms uniform-only
//!
//! The geometry is tessellated *and uploaded to GPU vertex/index buffers* once per
//! (geometry fingerprint, zoom bucket) and kept in [`GpuMeshCache`]. Pan, zoom
//! (within a bucket), move, rotate and scale change only the per-draw uniform
//! (view + object transform + colour): they re-tessellate 0 meshes and re-upload
//! 0 buffers. Only a node/geometry edit changes the fingerprint, so only the
//! edited path's mesh is rebuilt. The cache has a byte budget and evicts
//! least-recently-used meshes, never dropping one the current frame needs.

use std::collections::{HashMap, HashSet};
use wgpu::util::DeviceExt;

use super::cache::{geometry_fingerprint, ByteLru};
use super::mesh::tessellate;
use super::renderer::{
    CanvasView, GpuMesh, GpuPaint, VectorDraw, VectorRenderer, VECTOR_SAMPLE_COUNT,
};
use crate::core::blend::BlendMode;
use crate::core::layer::LayerMask;
use crate::core::vector::affine::AffineTransform;
use crate::core::vector::object::VectorObjectData;
use crate::core::vector::raster::raster_geometry;

/// GPU mesh cache byte budget (source vertex/index bytes). A 500-layer flower
/// fixture tessellates to a few MB total, so this comfortably holds a stress
/// scene across several zoom buckets while still bounding memory.
const MESH_CACHE_BYTE_BUDGET: usize = 96 * 1024 * 1024;

struct Sized {
    width: u32,
    height: u32,
    msaa_view: wgpu::TextureView,
    run_view: wgpu::TextureView,
}

/// GPU-resident mesh cache keyed by `(geometry fingerprint, zoom bucket)`. Owns
/// the uploaded [`GpuMesh`]es; [`ByteLru`] holds the byte-budget/eviction policy.
struct GpuMeshCache {
    entries: HashMap<(u64, u32), GpuMesh>,
    lru: ByteLru<(u64, u32)>,
    /// Meshes tessellated (== uploaded) during the current frame.
    frame_tessellations: u32,
    frame_uploads: u32,
    /// Geometry keys whose tessellation has failed and been logged. An eligible
    /// layer that fails to tessellate is dropped from the draw list (it would
    /// otherwise vanish silently), so — like `missing_texture_logged` in
    /// `gpu/mod.rs` — this leaves one log line per geometry instead of per frame.
    failed_logged: HashSet<(u64, u32)>,
}

impl GpuMeshCache {
    fn new(byte_budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            lru: ByteLru::new(byte_budget),
            frame_tessellations: 0,
            frame_uploads: 0,
            failed_logged: HashSet::new(),
        }
    }

    fn begin_frame(&mut self) {
        self.frame_tessellations = 0;
        self.frame_uploads = 0;
    }

    /// Ensure `key` is resident, tessellating + uploading on a miss. `protect`
    /// guards the current frame's working set from eviction. Returns whether the
    /// mesh is present afterwards (a tessellation failure returns `false`).
    fn ensure(
        &mut self,
        device: &wgpu::Device,
        key: (u64, u32),
        object: &VectorObjectData,
        tolerance: f32,
        protect: &dyn Fn(&(u64, u32)) -> bool,
    ) -> bool {
        if self.entries.contains_key(&key) {
            self.lru.touch(&key);
            return true;
        }
        let mesh = match tessellate(object, tolerance) {
            Ok(mesh) => mesh,
            Err(_) => {
                if self.failed_logged.insert(key) {
                    eprintln!(
                        "gpu vector: tessellation failed for geometry {:#x} (zoom bucket {}), \
                         {} nodes — layer will not draw on the GPU vector path this frame",
                        key.0,
                        key.1,
                        object.path.total_nodes(),
                    );
                }
                return false;
            }
        };
        self.frame_tessellations += 1;
        let gpu = GpuMesh::upload(device, &mesh);
        self.frame_uploads += 1;
        let bytes = gpu.byte_len.max(1);
        self.entries.insert(key, gpu);
        for evicted in self.lru.insert(key, bytes, protect) {
            self.entries.remove(&evicted);
        }
        true
    }

    fn get(&self, key: &(u64, u32)) -> Option<&GpuMesh> {
        self.entries.get(key)
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.lru.clear();
        self.failed_logged.clear();
    }
}

pub struct VectorCompositeStage {
    renderer: VectorRenderer,
    composite_pipeline: wgpu::RenderPipeline,
    composite_bgl: wgpu::BindGroupLayout,
    sized: Option<Sized>,
    cache: GpuMeshCache,
    mask_cache: HashMap<u32, CachedMask>,
}

struct CachedMask {
    fingerprint: u64,
    width: u32,
    height: u32,
    inverted: bool,
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

#[derive(Clone, Copy)]
pub struct VectorMask<'a> {
    pub layer_id: u32,
    pub layer_offset: (i32, i32),
    /// Layer-local mask sample delta. Zero for ordinary masks; PowerClip uses
    /// `(content delta - frame delta)` to keep the clip pinned while either
    /// participant moves without forcing a mask re-bake every frame.
    pub sample_shift: (i32, i32),
    pub mask: &'a LayerMask,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MaskUniform {
    data: [f32; 12],
}

impl VectorCompositeStage {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let renderer = VectorRenderer::new(device, format, VECTOR_SAMPLE_COUNT);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vector_composite_shader"),
            source: wgpu::ShaderSource::Wgsl(super::VECTOR_COMPOSITE_SHADER.into()),
        });
        let composite_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vector_composite_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vector_composite_layout"),
            bind_group_layouts: &[Some(&composite_bgl)],
            immediate_size: 0,
        });
        let composite_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vector_composite_pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // The shader outputs the fully-blended result (it samples
                    // the dst itself), so no hardware blend.
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            renderer,
            composite_pipeline,
            composite_bgl,
            sized: None,
            cache: GpuMeshCache::new(MESH_CACHE_BYTE_BUDGET),
            mask_cache: HashMap::new(),
        }
    }

    /// Reset per-frame counters before a composite. Cached GPU meshes persist
    /// across frames; only the current-frame tessellation/upload counts reset.
    pub fn begin_frame(&mut self) {
        self.cache.begin_frame();
    }

    /// Drop every cached GPU mesh (e.g. when the flag is toggled off). Device loss
    /// already rebuilds the whole stage, so it does not need this.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.mask_cache.clear();
    }

    /// Meshes tessellated + uploaded during the last [`Self::composite_run`]. Zero
    /// on a pure pan/zoom(within bucket)/move/rotate/scale frame (Phase 3 budget).
    pub fn last_frame_tessellations(&self) -> u32 {
        self.cache.frame_tessellations
    }

    /// GPU vertex/index buffer uploads during the last composite (== tessellations).
    pub fn last_frame_uploads(&self) -> u32 {
        self.cache.frame_uploads
    }

    /// Cached GPU mesh count.
    pub fn cache_len(&self) -> usize {
        self.cache.lru.len()
    }

    /// Cached GPU mesh size in bytes (source vertex/index bytes).
    pub fn cache_bytes(&self) -> usize {
        self.cache.lru.bytes()
    }

    /// Total meshes evicted by the byte-budget LRU since creation.
    pub fn cache_evictions(&self) -> u64 {
        self.cache.lru.evictions()
    }

    fn ensure_size(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        if let Some(s) = &self.sized {
            if s.width == width && s.height == height {
                return;
            }
        }
        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let msaa = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vector_run_msaa"),
            size: extent,
            mip_level_count: 1,
            sample_count: self.renderer.sample_count(),
            dimension: wgpu::TextureDimension::D2,
            format: super::renderer::VECTOR_TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let run = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vector_run_resolve"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: super::renderer::VECTOR_TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        self.sized = Some(Sized {
            width,
            height,
            msaa_view: msaa.create_view(&wgpu::TextureViewDescriptor::default()),
            run_view: run.create_view(&wgpu::TextureViewDescriptor::default()),
        });
    }

    /// Render one run of eligible vector objects (in z-order) and composite it over
    /// `dst_read`, writing to `dst_write`. The caller flips ping/pong parity once
    /// per run (the composite pass writes every pixel of `dst_write`).
    #[allow(clippy::too_many_arguments)]
    pub fn composite_run(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        dst_read: &wgpu::TextureView,
        dst_write: &wgpu::TextureView,
        viewport_w: u32,
        viewport_h: u32,
        view_offset_x: f32,
        view_offset_y: f32,
        zoom: f32,
        // Each object plus its layer's live offset. A vector layer's geometry lives
        // in `object.transform` (canvas space) with `layer.offset` normally equal to
        // its raster origin; a live Move drag shifts `layer.offset` before it is
        // folded back into the model, so the difference is the drag delta and must
        // be applied so the GPU shape follows the pointer like the raster preview.
        objects: &[(&VectorObjectData, (i32, i32), f32)],
        mask: Option<VectorMask<'_>>,
        run_opacity: f32,
        blend_mode: BlendMode,
    ) {
        self.ensure_size(device, viewport_w, viewport_h);
        let bucket = zoom_bucket(zoom);

        // Pass A — geometry key + tessellation tolerance per object. The `touched`
        // set is this frame's working set; it protects those keys from eviction
        // while later cache misses insert (a mesh needed this frame is never
        // dropped mid-frame).
        let mut plan: Vec<((u64, u32), f32)> = Vec::with_capacity(objects.len());
        let mut touched: HashSet<(u64, u32)> = HashSet::with_capacity(objects.len());
        for (object, _, _) in objects {
            let obj_scale = (object.transform.determinant().abs().sqrt()).max(1e-3);
            let tol = (0.25 / (bucket as f32 * obj_scale)).clamp(0.02, 1.0);
            let key = (geometry_fingerprint(object), bucket);
            touched.insert(key);
            plan.push((key, tol));
        }

        // Pass B — make every mesh resident. A cache hit (pan/zoom-in-bucket/move/
        // rotate/scale) tessellates and uploads nothing; only a fingerprint change
        // (node/geometry/style edit) misses and rebuilds that one mesh.
        let protect = |k: &(u64, u32)| touched.contains(k);
        let mut resident: Vec<Option<(u64, u32)>> = Vec::with_capacity(objects.len());
        for ((object, _, _), (key, tol)) in objects.iter().zip(&plan) {
            let ok = self.cache.ensure(device, *key, object, *tol, &protect);
            resident.push(ok.then_some(*key));
        }

        // Pass C — build the draws from the resident cached GPU meshes.
        let draws: Vec<VectorDraw> = objects
            .iter()
            .zip(&resident)
            .filter_map(|((object, layer_offset, layer_opacity), key)| {
                let key = (*key)?;
                let mesh = self.cache.get(&key)?;
                // Drag drift: layer.offset − model raster origin (0 when settled).
                let drift = raster_geometry(object).map_or((0.0, 0.0), |(origin, _, _)| {
                    (
                        (layer_offset.0 - origin.0) as f32,
                        (layer_offset.1 - origin.1) as f32,
                    )
                });
                let object_to_canvas = if drift == (0.0, 0.0) {
                    object.transform
                } else {
                    AffineTransform::translate(drift.0, drift.1).then(&object.transform)
                };
                Some(VectorDraw {
                    mesh,
                    object_to_canvas,
                    fill: GpuPaint::from_model(object.style.fill),
                    stroke: if object.style.effective_stroke_width() > 0.0 {
                        GpuPaint::from_model(object.style.stroke)
                    } else {
                        None
                    },
                    opacity: object.style.opacity * *layer_opacity,
                })
            })
            .collect();

        let Some(sized) = &self.sized else {
            return;
        };
        let view = CanvasView {
            width: viewport_w,
            height: viewport_h,
            off_x: view_offset_x,
            off_y: view_offset_y,
            scale: zoom,
        };
        // Pass 1: draw the run to the owned MSAA target and resolve to run_view.
        self.renderer.encode_run(
            device,
            encoder,
            &sized.msaa_view,
            &sized.run_view,
            view,
            &draws,
        );
        let mask_view = if let Some(spec) = mask {
            let width = spec.mask.width.max(1);
            let height = spec.mask.height.max(1);
            let fingerprint = spec.mask.tiles.revision_fingerprint();
            let stale = self.mask_cache.get(&spec.layer_id).is_none_or(|cached| {
                cached.fingerprint != fingerprint
                    || cached.width != width
                    || cached.height != height
                    || cached.inverted != spec.mask.inverted
            });
            if stale {
                let mut bytes = vec![0u8; width as usize * height as usize];
                for y in 0..height {
                    for x in 0..width {
                        bytes[(y * width + x) as usize] =
                            (spec.mask.sample(x, y) * 255.0).round() as u8;
                    }
                }
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("vector_layer_mask"),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                queue.write_texture(
                    texture.as_image_copy(),
                    &bytes,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(width),
                        rows_per_image: Some(height),
                    },
                    wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                );
                let view = texture.create_view(&Default::default());
                self.mask_cache.insert(
                    spec.layer_id,
                    CachedMask {
                        fingerprint,
                        width,
                        height,
                        inverted: spec.mask.inverted,
                        _texture: texture,
                        view,
                    },
                );
            }
            &self.mask_cache.get(&spec.layer_id).unwrap().view
        } else {
            // Binding 2 is mandatory. The shader skips this texture when disabled.
            &sized.run_view
        };
        let mask_data = mask.map_or(
            [
                0.0,
                0.0,
                0.0,
                1.0,
                0.0,
                0.0,
                1.0,
                1.0,
                0.0,
                0.0,
                run_opacity,
                blend_mode_code(blend_mode) as f32,
            ],
            |spec| {
                [
                    1.0,
                    view_offset_x,
                    view_offset_y,
                    zoom,
                    spec.layer_offset.0 as f32,
                    spec.layer_offset.1 as f32,
                    spec.mask.width.max(1) as f32,
                    spec.mask.height.max(1) as f32,
                    spec.sample_shift.0 as f32,
                    spec.sample_shift.1 as f32,
                    run_opacity,
                    blend_mode_code(blend_mode) as f32,
                ]
            },
        );
        let mask_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vector_mask_uniform"),
            contents: bytemuck::bytes_of(&MaskUniform { data: mask_data }),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        // Pass 2: composite run_view over dst_read into dst_write.
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vector_composite_bg"),
            layout: &self.composite_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(dst_read),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&sized.run_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(mask_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: mask_buffer.as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("vector_composite_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dst_write,
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
            pass.set_pipeline(&self.composite_pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }

        // Publish this frame's cache activity + size for the hybrid-canvas telemetry
        // (no-op in release). The byte-budget LRU already bounds memory in `ensure`.
        super::telemetry::mesh_frame(
            self.cache.frame_tessellations,
            self.cache.frame_uploads,
            self.cache.lru.evictions(),
            self.cache.lru.bytes(),
            self.cache.lru.len(),
        );
    }
}

fn blend_mode_code(mode: BlendMode) -> u32 {
    match mode {
        BlendMode::Normal => 0,
        BlendMode::Multiply => 1,
        BlendMode::Screen => 2,
        BlendMode::Overlay => 3,
        BlendMode::Darken => 4,
        BlendMode::Lighten => 5,
        BlendMode::ColorDodge => 6,
        BlendMode::ColorBurn => 7,
        BlendMode::HardLight => 8,
        BlendMode::SoftLight => 9,
        BlendMode::Difference => 10,
        BlendMode::Exclusion => 11,
        BlendMode::Hue => 12,
        BlendMode::Saturation => 13,
        BlendMode::Color => 14,
        BlendMode::Luminosity => 15,
        BlendMode::LinearLight => 16,
        // Dissolve (17) is stochastic per-pixel and cannot match the CPU raster
        // reference across zoom/transform, so it stays a raster fallback (never
        // reaches here — see `eligibility::blend_mode_supported`).
        _ => 0,
    }
}

/// Zoom → mesh bucket: curves are re-tessellated finer as the run is magnified, so
/// they stay crisp, while pan and small zoom jitter reuse the same mesh. Powers of
/// two up to 16 mirror the display-raster buckets.
fn zoom_bucket(zoom: f32) -> u32 {
    let z = (zoom.max(1.0).min(16.0).ceil() as u32).max(1);
    z.next_power_of_two().min(16)
}

#[cfg(test)]
mod tests {
    use super::blend_mode_code;
    use crate::core::blend::BlendMode;
    use crate::gpu::vector::eligibility::blend_mode_supported;

    #[test]
    fn every_supported_blend_maps_to_a_distinct_gpu_code() {
        // A GPU-eligible non-Normal mode that fell through to code 0 would render
        // as Normal — the plan's forbidden "silent wrong render". Lock every
        // supported mode to a non-zero code and make sure none collide.
        let mut seen = std::collections::HashSet::new();
        for &mode in BlendMode::all() {
            if !blend_mode_supported(mode) {
                continue;
            }
            let code = blend_mode_code(mode);
            if mode == BlendMode::Normal {
                assert_eq!(code, 0, "Normal must stay code 0");
            } else {
                assert_ne!(code, 0, "{mode:?} is GPU-eligible but maps to Normal");
                assert!(seen.insert(code), "{mode:?} reuses blend code {code}");
            }
        }
    }

    #[test]
    fn non_separable_codes_match_the_compositor_shader() {
        // These must equal the switch arms in compositor.wgsl / vector_composite.wgsl.
        assert_eq!(blend_mode_code(BlendMode::Hue), 12);
        assert_eq!(blend_mode_code(BlendMode::Saturation), 13);
        assert_eq!(blend_mode_code(BlendMode::Color), 14);
        assert_eq!(blend_mode_code(BlendMode::Luminosity), 15);
    }
}

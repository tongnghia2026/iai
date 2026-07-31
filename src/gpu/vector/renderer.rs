//! Off-screen GPU renderer for tessellated vector meshes (Phase 1 POC).
//!
//! This is the native draw pass the hybrid canvas is built on. It is deliberately
//! self-contained: it owns a pipeline plus a per-size MSAA colour target and knows
//! how to draw a list of [`VectorMesh`] fills/strokes into either an off-screen
//! texture (proof-of-concept / snapshot tests) or, from Phase 2 on, a texture the
//! compositor supplies. It never touches the vector model or the raster twin.
//!
//! ## Colour space
//!
//! The compositor's canvas texture is `Rgba8UnormSrgb`. `ColorValue::Rgb` stores
//! sRGB-encoded channels (`to_rgba8` is a straight `round(v*255)`), so to make the
//! GPU write the *same* bytes the CPU rasteriser stores, the fill colour is passed
//! to the shader as linear = `srgb_to_linear(channel)`; the sRGB target then
//! re-encodes it to the identical byte on store. Anti-aliasing (MSAA coverage) is
//! therefore resolved in linear light, which is what the Phase 0 comparison metric
//! requires.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use super::mesh::{VectorMesh, VectorVertex};
use crate::core::vector::affine::AffineTransform;

/// The colour target format the hybrid canvas draws into. Matches
/// `GpuState::canvas_texture`.
pub const VECTOR_TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// MSAA sample count for the vector target. Phase 0/§10 fix this at 4× and resolve
/// to a sample-count-1 texture before it re-enters the compositor.
pub const VECTOR_SAMPLE_COUNT: u32 = 4;

/// Standard sRGB electro-optical transfer (sRGB byte space → linear light).
#[inline]
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Inverse sRGB transfer (linear light → sRGB byte space).
#[inline]
pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Convert a premultiplied-alpha sRGB buffer (what [`VectorRenderer::render_offscreen`]
/// returns) to straight alpha, in place. Division is done in linear light so an
/// AA-edge pixel un-premultiplies correctly; a fully-opaque interior is unchanged.
///
/// The production compositor's accumulator is **straight alpha** and blends in
/// sRGB space (see `compositor.wgsl`), so a resolved vector run must be converted
/// with this before it is composited as a run at its z-position. Phase 2 wiring
/// depends on this; interior pixels are byte-identical either way.
pub fn unpremultiply_srgb(rgba: &mut [u8]) {
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3] as f32 / 255.0;
        if a <= 0.0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
            continue;
        }
        for c in px.iter_mut().take(3) {
            let lin = srgb_to_linear(*c as f32 / 255.0) / a;
            *c = (linear_to_srgb(lin.min(1.0)) * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
    }
}

/// The layer→clip mapping for one render call: the mesh is drawn in *layer* space
/// (`object_to_canvas` maps object-local vertices to layer pixels) and this frame
/// maps a `width × height` window whose top-left sits at `(off_x, off_y)` in layer
/// space onto normalised device coordinates.
#[derive(Debug, Clone, Copy)]
pub struct CanvasView {
    pub width: u32,
    pub height: u32,
    pub off_x: f32,
    pub off_y: f32,
}

impl CanvasView {
    /// Column-major `mat3x3` mapping a layer point `(lx, ly, 1)` to clip space,
    /// with the Y axis flipped for wgpu's NDC. Pixel `(px, py)` centre `+0.5`
    /// lands on the same sample the CPU rasteriser reads, because a layer point at
    /// integer `lx` maps to framebuffer edge `lx - off_x`.
    fn canvas_to_clip(&self) -> [[f32; 4]; 3] {
        let w = self.width.max(1) as f32;
        let h = self.height.max(1) as f32;
        [
            [2.0 / w, 0.0, 0.0, 0.0],
            [0.0, -2.0 / h, 0.0, 0.0],
            [
                -2.0 * self.off_x / w - 1.0,
                2.0 * self.off_y / h + 1.0,
                1.0,
                0.0,
            ],
        ]
    }
}

/// One mesh + its object→layer transform + a solid colour, as handed to the
/// renderer for a single draw. Colour is the straight-alpha sRGB-byte value in
/// `[0,1]` (i.e. `ColorValue::Rgb` channels); the renderer performs the linear
/// conversion described in the module docs.
pub struct VectorDraw<'a> {
    pub mesh: &'a VectorMesh,
    pub object_to_canvas: AffineTransform,
    /// Fill colour, straight-alpha sRGB-byte space in `[0,1]`.
    pub fill: Option<[f32; 4]>,
    /// Stroke colour, straight-alpha sRGB-byte space in `[0,1]`.
    pub stroke: Option<[f32; 4]>,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DrawRaw {
    object_to_canvas: [[f32; 4]; 3],
    canvas_to_clip: [[f32; 4]; 3],
    color: [f32; 4],
}

fn affine_cols(m: &AffineTransform) -> [[f32; 4]; 3] {
    // x' = a*x + c*y + e ; y' = b*x + d*y + f  →  columns (a,b,0),(c,d,0),(e,f,1).
    [
        [m.a, m.b, 0.0, 0.0],
        [m.c, m.d, 0.0, 0.0],
        [m.e, m.f, 1.0, 0.0],
    ]
}

/// Owns the vector draw pipeline. One instance is valid for the lifetime of a GPU
/// device; it is rebuilt on device loss like every other pipeline.
pub struct VectorRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_stride: u32,
    sample_count: u32,
}

impl VectorRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, sample_count: u32) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vector_shader"),
            source: wgpu::ShaderSource::Wgsl(super::VECTOR_SHADER.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vector_draw_bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<DrawRaw>() as u64),
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vector_pipeline_layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vector_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<VectorVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x2,
                        offset: 0,
                        shader_location: 0,
                    }],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // Premultiplied source-over (the shader emits premultiplied
                    // colour). Blending on an sRGB target runs in linear light; a
                    // fully-covered opaque sample keeps the source colour exactly
                    // (interior == reference).
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // The fill/stroke tessellation emits both windings; never cull.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });
        let align = device.limits().min_uniform_buffer_offset_alignment.max(1);
        let size = std::mem::size_of::<DrawRaw>() as u32;
        let uniform_stride = size.div_ceil(align) * align;
        Self {
            pipeline,
            bind_group_layout,
            uniform_stride,
            sample_count,
        }
    }

    pub fn sample_count(&self) -> u32 {
        self.sample_count
    }

    /// Pack one uniform block per (draw, colour) into a single buffer and return
    /// the buffer plus the list of `(byte_offset, index_range)` to draw. Fill and
    /// stroke of one object become two blocks so each gets its own colour.
    fn build_uniforms(
        &self,
        device: &wgpu::Device,
        view: &CanvasView,
        draws: &[VectorDraw],
    ) -> Option<(wgpu::Buffer, Vec<DrawItem>)> {
        let clip = view.canvas_to_clip();
        let mut blob: Vec<u8> = Vec::new();
        let mut items: Vec<DrawItem> = Vec::new();
        let stride = self.uniform_stride as usize;
        let mut push = |color: [f32; 4], obj: &AffineTransform, range: std::ops::Range<u32>| {
            if range.is_empty() {
                return;
            }
            let raw = DrawRaw {
                object_to_canvas: affine_cols(obj),
                canvas_to_clip: clip,
                color: [
                    srgb_to_linear(color[0]),
                    srgb_to_linear(color[1]),
                    srgb_to_linear(color[2]),
                    color[3],
                ],
            };
            let offset = items.len() * stride;
            blob.resize(offset, 0);
            blob.extend_from_slice(bytemuck::bytes_of(&raw));
            items.push(DrawItem {
                offset: offset as u32,
                index_range: range,
            });
        };
        for draw in draws {
            if let Some(fill) = draw.fill {
                push(fill, &draw.object_to_canvas, draw.mesh.fill_range.clone());
            }
            if let Some(stroke) = draw.stroke {
                push(
                    stroke,
                    &draw.object_to_canvas,
                    draw.mesh.stroke_range.clone(),
                );
            }
        }
        if items.is_empty() {
            return None;
        }
        blob.resize(items.len() * stride, 0);
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vector_uniforms"),
            contents: &blob,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        Some((buffer, items))
    }

    /// Record every draw into an already-open render pass targeting a texture of
    /// this renderer's sample count. `mesh_buffers` must be built once per frame
    /// with [`Self::upload_meshes`] and index-aligned with `draws`. Used by the
    /// compositor integration (Phase 2) so the vector run draws straight into the
    /// accumulator attachment.
    pub fn record<'p>(
        &'p self,
        pass: &mut wgpu::RenderPass<'p>,
        bind_group: &'p wgpu::BindGroup,
        mesh_buffers: &'p [MeshBuffers],
        draws: &[VectorDraw],
        items: &[DrawItem],
    ) {
        pass.set_pipeline(&self.pipeline);
        // `items` interleaves fill-then-stroke per draw exactly as `build_uniforms`
        // produced them; walk the same order to pair each with its mesh buffer.
        let mut item = 0usize;
        for (draw, buffers) in draws.iter().zip(mesh_buffers) {
            pass.set_vertex_buffer(0, buffers.vertices.slice(..));
            pass.set_index_buffer(buffers.indices.slice(..), wgpu::IndexFormat::Uint32);
            for present in [draw.fill.is_some(), draw.stroke.is_some()] {
                if !present {
                    continue;
                }
                if let Some(it) = items.get(item) {
                    pass.set_bind_group(0, bind_group, &[it.offset]);
                    pass.draw_indexed(it.index_range.clone(), 0, 0..1);
                    item += 1;
                }
            }
        }
    }

    /// Upload the vertex/index buffers for one frame's draws.
    pub fn upload_meshes(&self, device: &wgpu::Device, draws: &[VectorDraw]) -> Vec<MeshBuffers> {
        draws
            .iter()
            .map(|draw| MeshBuffers::new(device, draw.mesh))
            .collect()
    }

    pub fn bind_group(&self, device: &wgpu::Device, buffer: &wgpu::Buffer) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vector_draw_bg"),
            layout: &self.bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(std::mem::size_of::<DrawRaw>() as u64),
                }),
            }],
        })
    }

    /// Proof-of-concept path: render `draws` into a fresh MSAA target, resolve to a
    /// sample-count-1 texture, read it back, and return tight straight-alpha RGBA
    /// (row-unpadded, `width * height * 4` bytes). Used by the local snapshot tests
    /// that compare against `core::vector::raster`.
    pub fn render_offscreen(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: CanvasView,
        draws: &[VectorDraw],
    ) -> Vec<u8> {
        let width = view.width.max(1);
        let height = view.height.max(1);
        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let msaa = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vector_msaa"),
            size: extent,
            mip_level_count: 1,
            sample_count: self.sample_count,
            dimension: wgpu::TextureDimension::D2,
            format: VECTOR_TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let resolve = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vector_resolve"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: VECTOR_TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let msaa_view = msaa.create_view(&wgpu::TextureViewDescriptor::default());
        let resolve_view = resolve.create_view(&wgpu::TextureViewDescriptor::default());

        let uniforms = self.build_uniforms(device, &view, draws);
        let mesh_buffers = self.upload_meshes(device, draws);
        // The bind group must outlive the render pass, so build it up front.
        let bound = uniforms
            .as_ref()
            .map(|(buffer, items)| (self.bind_group(device, buffer), items));

        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("vector_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &msaa_view,
                    resolve_target: Some(&resolve_view),
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                ..Default::default()
            });
            if let Some((bind_group, items)) = &bound {
                self.record(&mut pass, bind_group, &mesh_buffers, draws, items);
            }
        }

        // Copy the resolved texture into a padded readback buffer.
        let bytes_per_pixel = 4u32;
        let unpadded = width * bytes_per_pixel;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vector_readback"),
            size: (padded * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &resolve,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            extent,
        );
        queue.submit([encoder.finish()]);

        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::wait_indefinitely()).ok();
        let mapped = slice.get_mapped_range();
        let mut out = vec![0u8; (unpadded * height) as usize];
        for row in 0..height as usize {
            let src = row * padded as usize;
            let dst = row * unpadded as usize;
            out[dst..dst + unpadded as usize]
                .copy_from_slice(&mapped[src..src + unpadded as usize]);
        }
        drop(mapped);
        readback.unmap();
        out
    }
}

/// A uniform block's byte offset and the index range it draws.
pub struct DrawItem {
    pub offset: u32,
    pub index_range: std::ops::Range<u32>,
}

/// GPU vertex/index buffers for one mesh.
pub struct MeshBuffers {
    pub vertices: wgpu::Buffer,
    pub indices: wgpu::Buffer,
}

impl MeshBuffers {
    pub fn new(device: &wgpu::Device, mesh: &VectorMesh) -> Self {
        // An empty buffer is invalid; a 1-vertex placeholder keeps the slice call
        // legal for a mesh that is pure stroke or pure fill.
        let vertices = if mesh.vertices.is_empty() {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vector_vertices_empty"),
                contents: bytemuck::bytes_of(&VectorVertex { position: [0.0; 2] }),
                usage: wgpu::BufferUsages::VERTEX,
            })
        } else {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vector_vertices"),
                contents: bytemuck::cast_slice(&mesh.vertices),
                usage: wgpu::BufferUsages::VERTEX,
            })
        };
        let indices = if mesh.indices.is_empty() {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vector_indices_empty"),
                contents: bytemuck::bytes_of(&0u32),
                usage: wgpu::BufferUsages::INDEX,
            })
        } else {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vector_indices"),
                contents: bytemuck::cast_slice(&mesh.indices),
                usage: wgpu::BufferUsages::INDEX,
            })
        };
        Self { vertices, indices }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draw_uniform_is_16_byte_aligned() {
        // WGSL uniform blocks must be a multiple of 16 bytes; two padded mat3x3
        // (48 each) + a vec4 (16) = 112.
        assert_eq!(std::mem::size_of::<DrawRaw>(), 112);
        assert_eq!(std::mem::size_of::<DrawRaw>() % 16, 0);
    }

    #[test]
    fn srgb_transfer_round_trips() {
        for byte in [0u8, 1, 12, 128, 200, 255] {
            let x = byte as f32 / 255.0;
            let round = linear_to_srgb(srgb_to_linear(x));
            assert!(
                (round - x).abs() < 1e-4,
                "sRGB round-trip failed for {byte}"
            );
        }
    }

    #[test]
    fn unpremultiply_is_identity_when_opaque() {
        let mut px = [10u8, 200, 255, 255, 40, 40, 40, 255];
        let before = px;
        unpremultiply_srgb(&mut px);
        assert_eq!(px, before);
    }

    #[test]
    fn unpremultiply_recovers_straight_colour() {
        // Straight white at 50% alpha, premultiplied in linear light, is
        // sRGB-byte ~188. Un-premultiplying must recover ~255.
        let mut px = [188u8, 188, 188, 128];
        unpremultiply_srgb(&mut px);
        for c in 0..3 {
            assert!(
                px[c] >= 252,
                "channel {c} = {} not recovered to ~255",
                px[c]
            );
        }
        assert_eq!(px[3], 128, "alpha is preserved");
    }

    #[test]
    fn affine_columns_match_apply_point() {
        let m = AffineTransform::translate(5.0, -3.0).then(&AffineTransform::rotate(0.4));
        let cols = affine_cols(&m);
        // Column-major mat3 * (x,y,1): x' = c0.x*x + c1.x*y + c2.x, etc.
        let (x, y) = (2.0f32, 7.0f32);
        let px = cols[0][0] * x + cols[1][0] * y + cols[2][0];
        let py = cols[0][1] * x + cols[1][1] * y + cols[2][1];
        let p = m.apply_point(crate::core::geometry::Point::new(x, y));
        assert!((px - p.x).abs() < 1e-4 && (py - p.y).abs() < 1e-4);
    }
}

/// Request a headless GPU device (no surface) for local snapshot tests. Returns
/// `None` on a machine/CI with no usable adapter, so the caller can skip instead
/// of failing — GPU render snapshots are local/manual per the plan, never a CI
/// gate.
pub fn headless_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let mut idesc = wgpu::InstanceDescriptor::new_without_display_handle();
    idesc.backends = wgpu::Backends::all();
    let instance = wgpu::Instance::new(idesc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        compatible_surface: None,
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
    }))
    .ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("vector_headless_device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        ..Default::default()
    }))
    .ok()
}

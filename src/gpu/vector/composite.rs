//! Live-compositor stage: draw a run of eligible vector layers straight into the
//! ping/pong accumulator at the correct z-position (Phase 2).
//!
//! A run is rendered to an owned MSAA target, resolved to a premultiplied texture,
//! then composited over the current accumulator buffer into the other buffer with
//! a shader that mirrors `compositor.wgsl`'s straight-alpha sRGB Normal blend — so
//! the vector run behaves exactly like one raster layer in the ping/pong loop
//! (one parity flip). Meshes are cached per (geometry fingerprint, zoom bucket) so
//! pan/zoom/move never re-tessellate.

use std::collections::HashMap;

use super::cache::geometry_fingerprint;
use super::mesh::{tessellate, VectorMesh};
use super::renderer::{CanvasView, VectorDraw, VectorRenderer, VECTOR_SAMPLE_COUNT};
use crate::core::vector::affine::AffineTransform;
use crate::core::vector::color::ColorValue;
use crate::core::vector::object::VectorObjectData;
use crate::core::vector::raster::raster_geometry;
use crate::core::vector::style::Paint;

/// Cap on cached meshes; a full clear past it bounds memory (rare — the run only
/// ever holds the visible eligible layers).
const MESH_CACHE_CAP: usize = 4096;

struct Sized {
    width: u32,
    height: u32,
    msaa_view: wgpu::TextureView,
    run_view: wgpu::TextureView,
}

pub struct VectorCompositeStage {
    renderer: VectorRenderer,
    composite_pipeline: wgpu::RenderPipeline,
    composite_bgl: wgpu::BindGroupLayout,
    sized: Option<Sized>,
    /// (geometry fingerprint, zoom bucket) → tessellated mesh + frame last used.
    meshes: HashMap<(u64, u32), (VectorMesh, u64)>,
    frame: u64,
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
            meshes: HashMap::new(),
            frame: 0,
        }
    }

    /// Bump the frame counter once per composite so stale meshes can be pruned.
    pub fn begin_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
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
        objects: &[(&VectorObjectData, (i32, i32))],
    ) {
        self.ensure_size(device, viewport_w, viewport_h);
        let bucket = zoom_bucket(zoom);

        // Tessellate/refresh the cache first (mutable), then borrow it immutably
        // to build the draws so both borrows are disjoint from the renderer.
        let mut keys: Vec<Option<(u64, u32)>> = Vec::with_capacity(objects.len());
        for (object, _) in objects {
            let obj_scale = (object.transform.determinant().abs().sqrt()).max(1e-3);
            let tol = (0.25 / (bucket as f32 * obj_scale)).clamp(0.02, 1.0);
            let key = (geometry_fingerprint(object), bucket);
            if !self.meshes.contains_key(&key) {
                match tessellate(object, tol) {
                    Ok(mesh) => {
                        self.meshes.insert(key, (mesh, self.frame));
                    }
                    Err(_) => {
                        keys.push(None);
                        continue;
                    }
                }
            }
            if let Some(entry) = self.meshes.get_mut(&key) {
                entry.1 = self.frame;
            }
            keys.push(Some(key));
        }

        let draws: Vec<VectorDraw> = objects
            .iter()
            .zip(&keys)
            .filter_map(|((object, layer_offset), key)| {
                let key = (*key)?;
                let mesh = &self.meshes.get(&key)?.0;
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
                    fill: paint_rgba(object.style.fill),
                    stroke: if object.style.effective_stroke_width() > 0.0 {
                        paint_rgba(object.style.stroke)
                    } else {
                        None
                    },
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

        if self.meshes.len() > MESH_CACHE_CAP {
            let cur = self.frame;
            self.meshes.retain(|_, (_, f)| cur.wrapping_sub(*f) <= 1);
        }
    }
}

/// The straight-alpha sRGB `[0,1]` colour of a solid RGB paint, or `None`. Only
/// `Paint::Solid(Rgb)` reaches here — eligibility already rejected CMYK/gradient.
fn paint_rgba(paint: Paint) -> Option<[f32; 4]> {
    match paint {
        Paint::Solid(ColorValue::Rgb { r, g, b, a }) => Some([r, g, b, a]),
        _ => None,
    }
}

/// Zoom → mesh bucket: curves are re-tessellated finer as the run is magnified, so
/// they stay crisp, while pan and small zoom jitter reuse the same mesh. Powers of
/// two up to 16 mirror the display-raster buckets.
fn zoom_bucket(zoom: f32) -> u32 {
    let z = (zoom.max(1.0).min(16.0).ceil() as u32).max(1);
    z.next_power_of_two().min(16)
}

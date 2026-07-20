// GPU renderer — wgpu backend + egui integration
// Fully decoupled from app logic — only knows how to draw.

use crate::core::canvas::Canvas;
use bytemuck::{Pod, Zeroable};
use std::sync::Arc;
use winit::window::Window;

pub mod compositor;
pub mod tile_atlas;

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct CanvasUniforms {
    pub offset: [f32; 2],
    pub zoom: f32,
    /// 0.0 = Mode A (normal, GPU compositor, canvas-sized texture)
    /// 1.0 = Mode B (large canvas, CPU viewport composite, screen-sized texture fullscreen blit)
    pub vp_mode: f32,
    pub screen_size: [f32; 2],
    /// 1.0 = apply the soft-proof 3D LUT in the blit shader, 0.0 = pass-through.
    pub proof_enabled: f32,
    /// Channels panel plate view: 0 = composite, 1..3 = R/G/B as grayscale,
    /// 4 = saved alpha-channel plane as grayscale. Non-zero bypasses proof.
    pub channel_view: f32,
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct CursorUniforms {
    pub cursor_pos: [f32; 2],
    pub brush_size: f32,
    pub _pad: f32,
    pub screen_size: [f32; 2],
    pub _pad2: [f32; 2],
}

/// Selection outline — sends the selection rect's bbox (pixel coords in canvas space).
#[repr(C)]
#[derive(Debug, Copy, Clone, Pod, Zeroable)]
pub struct SelectionUniforms {
    /// [x0, y0, x1, y1] — pixel coords in canvas space (bbox to clip the vertex quad).
    pub rect: [f32; 4],
    pub offset: [f32; 2],
    pub zoom: f32,
    pub time: f32,
    pub screen_size: [f32; 2],
    pub canvas_size: [f32; 2],
    pub sel_offset: [f32; 2],
    pub _pad: [f32; 2],
}

/// Per-window render target: the swapchain surface, its configuration, and the
/// egui renderer that draws that window's UI. Everything else (device, queue,
/// pipelines, bind groups, canvas/selection textures, the compositor) is shared
/// across windows and lives on `GpuState`.
///
/// Extracting this is the first step toward a second OS window (the Develop
/// stage, kiểu Camera Raw). With one window it is a pure move: `GpuState` owns
/// exactly one `WindowSurface` (`main`) and every former `self.main.surface` /
/// `self.main.config` / `self.main.egui_renderer` now reads it — behaviour is identical.
pub struct WindowSurface {
    pub surface: wgpu::Surface<'static>,
    pub config: wgpu::SurfaceConfiguration,
    egui_renderer: egui_wgpu::Renderer,
}

impl WindowSurface {
    /// Reconfigure the swapchain after a window resize.
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(device, &self.config);
    }
}

#[allow(dead_code)]
pub struct GpuState {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// The main application window's render target.
    pub main: WindowSurface,
    /// The Develop stage's second OS window, when open (D1.2). Shares this
    /// device/queue/pipelines; only the surface + config + egui_renderer are
    /// its own. `None` whenever the Develop window is closed.
    pub develop: Option<WindowSurface>,
    canvas_texture: wgpu::Texture,
    canvas_view: wgpu::TextureView,
    canvas_override_active: bool,
    sampler: wgpu::Sampler,
    /// Soft-proof 3D LUT (sRGB→device→sRGB). Identity until a proof setup is
    /// applied; always bound so the blit bind group layout is constant.
    lut3d_tex: wgpu::Texture,
    lut3d_view: wgpu::TextureView,
    lut3d_sampler: wgpu::Sampler,
    canvas_uniform_buf: wgpu::Buffer,
    cursor_uniform_buf: wgpu::Buffer,
    selection_uniform_buf: wgpu::Buffer,
    render_pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    checker_pipeline: wgpu::RenderPipeline,
    checker_bind_group: wgpu::BindGroup,
    checker_bind_group_layout: wgpu::BindGroupLayout,
    cursor_pipeline: wgpu::RenderPipeline,
    cursor_bind_group: wgpu::BindGroup,
    selection_pipeline: wgpu::RenderPipeline,
    selection_bind_group: wgpu::BindGroup,
    selection_bind_group_layout: wgpu::BindGroupLayout,
    selection_mask_tex: wgpu::Texture,
    selection_mask_view: wgpu::TextureView,
    selection_mask_samp: wgpu::Sampler,
    selection_canvas_w: u32,
    selection_canvas_h: u32,
    /// Max texture size this GPU supports (from adapter.limits()).
    /// Used to validate canvas size before create_texture.
    pub max_texture_dimension: u32,
    /// Viewport streaming mode — enabled when the canvas exceeds max_texture_dimension.
    /// In this mode:
    ///   - canvas_texture = screen-sized (instead of canvas-sized)
    ///   - CPU composites pixels for the viewport and uploads to the GPU each dirty frame
    ///   - the GPU brush pipeline is disabled (CPU brush is used instead)
    pub is_large_canvas: bool,
    pub compositor: compositor::CompositorState,
    pub current_frame_is_ping: bool,
    /// Set by wgpu's device-lost callback (driver reset / TDR — e.g. after a
    /// heavy AI inference or an idle power cycle). Every resource on this
    /// device is invalid from that point; the App polls this each frame and
    /// rebuilds the whole GPU context instead of freezing on validation spam.
    pub device_lost: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Retained so a second OS window (the Develop stage) can build its own
    /// surface on the SAME device: `instance.create_surface(window2)` +
    /// `adapter` for its capabilities. The shared pipelines were compiled for
    /// `surface_format`, so window 2's config must use the same format (both
    /// windows sit on the same adapter, so its preferred sRGB format matches).
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    pub surface_format: wgpu::TextureFormat,
    /// The Develop window's own canvas-blit uniforms (its own offset/zoom/screen
    /// size), separate from `canvas_uniform_buf` so the two windows can show the
    /// canvas at independent views without clobbering each other.
    develop_canvas_uniform_buf: wgpu::Buffer,
}

#[allow(dead_code)]
impl GpuState {
    pub fn new(
        window: Arc<Window>,
        _pixels: &[u8],
        canvas_w: u32,
        canvas_h: u32,
    ) -> Result<Self, String> {
        let mut idesc = wgpu::InstanceDescriptor::new_without_display_handle();
        idesc.backends = wgpu::Backends::all();
        let instance = wgpu::Instance::new(idesc);

        let surface = instance
            .create_surface(Arc::clone(&window))
            .map_err(|e| format!("Không tạo được surface đồ hoạ: {e}"))?;

        let adapter = [
            (wgpu::PowerPreference::HighPerformance, false),
            (wgpu::PowerPreference::LowPower, false),
            (wgpu::PowerPreference::LowPower, true),
        ]
        .into_iter()
        .find_map(|(power_preference, force_fallback_adapter)| {
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                power_preference,
                force_fallback_adapter,
            }))
            .ok()
        })
        .ok_or_else(|| {
            "Không tìm thấy GPU tương thích (kể cả trình kết xuất phần mềm). \
             Hãy cập nhật driver card màn hình rồi mở lại IAI."
                .to_string()
        })?;

        let adapter_limits = adapter.limits();
        let max_tex = adapter_limits.max_texture_dimension_2d;

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("iai_device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits {
                max_texture_dimension_2d: max_tex,
                ..wgpu::Limits::default()
            },
            ..Default::default()
        }))
        .map_err(|e| format!("Không tạo được thiết bị GPU: {e}"))?;

        device.on_uncaptured_error(std::sync::Arc::new(|e: wgpu::Error| {
            crate::log_crash(&format!("wgpu uncaptured error: {e}"));
            eprintln!("wgpu uncaptured error: {e}");
        }));

        // Driver reset / TDR recovery: flag the loss and WAKE the event loop —
        // it usually happens while the app idles (GPU power cycle) or right
        // after a heavy AI inference, when no redraw would otherwise arrive to
        // notice it. `Destroyed` is the intentional drop during a rebuild.
        let device_lost = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let flag = std::sync::Arc::clone(&device_lost);
            let wake = std::sync::Arc::clone(&window);
            device.set_device_lost_callback(move |reason, msg| {
                if matches!(reason, wgpu::DeviceLostReason::Destroyed) {
                    return;
                }
                crate::log_crash(&format!("wgpu device LOST ({reason:?}): {msg}"));
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                wake.request_redraw();
            });
        }

        let size = window.inner_size();
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .find(|f| f.is_srgb())
            .copied()
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 1,
        };
        surface.configure(&device, &config);

        let canvas_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("canvas_tex"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let canvas_view = canvas_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let canvas_uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("canvas_uniforms"),
            size: std::mem::size_of::<CanvasUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let develop_canvas_uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("develop_canvas_uniforms"),
            size: std::mem::size_of::<CanvasUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let cursor_uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cursor_uniforms"),
            size: std::mem::size_of::<CursorUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let selection_uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("selection_uniforms"),
            size: std::mem::size_of::<SelectionUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let blend_alpha = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent::OVER,
        };

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("canvas_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Soft-proof 3D LUT + its sampler.
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let checker_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("checker_bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let cursor_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("cursor_bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let selection_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("selection_bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let selection_mask_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("selection_mask_tex"),
            size: wgpu::Extent3d {
                width: canvas_w,
                height: canvas_h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let selection_mask_view =
            selection_mask_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let selection_mask_samp = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("selection_mask_samp"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        // Soft-proof 3D LUT: starts as identity (no colour change), uploaded when
        // a proof setup is applied. Linear-filtered, clamped at the cube edges.
        let lut_size = crate::core::cms::PROOF_LUT_SIZE as u32;
        let lut3d_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("proof_lut3d"),
            size: wgpu::Extent3d {
                width: lut_size,
                height: lut_size,
                depth_or_array_layers: lut_size,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &lut3d_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &crate::core::cms::identity_lut(crate::core::cms::PROOF_LUT_SIZE),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(lut_size * 4),
                rows_per_image: Some(lut_size),
            },
            wgpu::Extent3d {
                width: lut_size,
                height: lut_size,
                depth_or_array_layers: lut_size,
            },
        );
        let lut3d_view = lut3d_tex.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D3),
            ..Default::default()
        });
        let lut3d_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("proof_lut_samp"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let bind_group = Self::make_canvas_bind_group(
            &device,
            &bind_group_layout,
            &canvas_view,
            &sampler,
            &canvas_uniform_buf,
            &lut3d_view,
            &lut3d_sampler,
        );
        let checker_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("checker_bg"),
            layout: &checker_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: canvas_uniform_buf.as_entire_binding(),
            }],
        });
        let cursor_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("cursor_bg"),
            layout: &cursor_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: cursor_uniform_buf.as_entire_binding(),
            }],
        });
        let selection_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("selection_bg"),
            layout: &selection_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: selection_uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&selection_mask_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&selection_mask_samp),
                },
            ],
        });

        let canvas_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("canvas_shader"),
            source: wgpu::ShaderSource::Wgsl(CANVAS_SHADER.into()),
        });
        let cursor_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cursor_shader"),
            source: wgpu::ShaderSource::Wgsl(CURSOR_SHADER.into()),
        });
        let checker_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("checker_shader"),
            source: wgpu::ShaderSource::Wgsl(CHECKER_SHADER.into()),
        });
        let selection_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("selection_shader"),
            source: wgpu::ShaderSource::Wgsl(SELECTION_SHADER.into()),
        });

        let canvas_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("canvas_pl"),
                bind_group_layouts: &[Some(&bind_group_layout)],
                immediate_size: 0,
            });
        let checker_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("checker_pl"),
                bind_group_layouts: &[Some(&checker_bind_group_layout)],
                immediate_size: 0,
            });
        let cursor_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("cursor_pl"),
                bind_group_layouts: &[Some(&cursor_bind_group_layout)],
                immediate_size: 0,
            });
        let selection_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("selection_pl"),
                bind_group_layouts: &[Some(&selection_bind_group_layout)],
                immediate_size: 0,
            });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("canvas_rp"),
            layout: Some(&canvas_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &canvas_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &canvas_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(blend_alpha),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
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

        let checker_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("checker_rp"),
            layout: Some(&checker_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &checker_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &checker_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
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

        let cursor_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cursor_rp"),
            layout: Some(&cursor_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &cursor_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &cursor_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(blend_alpha),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
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

        let selection_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("selection_rp"),
            layout: Some(&selection_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &selection_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &selection_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(blend_alpha),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
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

        let egui_renderer =
            egui_wgpu::Renderer::new(&device, format, egui_wgpu::RendererOptions::default());

        let compositor = compositor::CompositorState::new(&device, 1, 1, max_tex);

        let is_large_canvas = canvas_w > max_tex
            || canvas_h > max_tex
            || !Canvas::fits_flat_buffer(canvas_w, canvas_h);

        let main = WindowSurface {
            surface,
            config,
            egui_renderer,
        };

        Ok(Self {
            device,
            queue,
            main,
            develop: None,
            canvas_texture,
            canvas_view,
            canvas_override_active: false,
            sampler,
            lut3d_tex,
            lut3d_view,
            lut3d_sampler,
            canvas_uniform_buf,
            cursor_uniform_buf,
            selection_uniform_buf,
            render_pipeline,
            bind_group_layout,
            bind_group,
            checker_pipeline,
            checker_bind_group,
            checker_bind_group_layout,
            cursor_pipeline,
            cursor_bind_group,
            selection_pipeline,
            selection_bind_group,
            selection_bind_group_layout,
            selection_mask_tex,
            selection_mask_view,
            selection_mask_samp,
            selection_canvas_w: canvas_w,
            selection_canvas_h: canvas_h,
            max_texture_dimension: max_tex,
            is_large_canvas,
            compositor,
            current_frame_is_ping: true,
            device_lost,
            instance,
            adapter,
            surface_format: format,
            develop_canvas_uniform_buf,
        })
    }

    /// Build the Develop stage's second-window render target on THIS device
    /// (D1.2). Mirrors the surface/config/egui-renderer setup in `new()`, but
    /// reuses the shared `surface_format` so the shared pipelines stay valid.
    /// The caller supplies the freshly-created winit window; on success
    /// `self.develop` becomes `Some`. Idempotent: re-opening replaces the slot.
    pub fn open_develop_surface(&mut self, window: Arc<Window>) -> Result<(), String> {
        let surface = self
            .instance
            .create_surface(Arc::clone(&window))
            .map_err(|e| format!("Could not create the Develop window surface: {e}"))?;
        let caps = surface.get_capabilities(&self.adapter);
        // The shared pipelines were compiled for `surface_format`; only proceed
        // if this surface can present it (same adapter, so it should).
        if !caps.formats.contains(&self.surface_format) {
            return Err(format!(
                "The Develop window surface does not support format {:?}",
                self.surface_format
            ));
        }
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: self.surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 1,
        };
        surface.configure(&self.device, &config);
        let egui_renderer = egui_wgpu::Renderer::new(
            &self.device,
            self.surface_format,
            egui_wgpu::RendererOptions::default(),
        );
        self.develop = Some(WindowSurface {
            surface,
            config,
            egui_renderer,
        });
        Ok(())
    }

    /// Tear down the Develop window's render target (Develop closed). The winit
    /// window itself is dropped by the caller; this just releases the surface.
    pub fn close_develop_surface(&mut self) {
        self.develop = None;
    }

    fn make_canvas_bind_group(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
        uniform_buf: &wgpu::Buffer,
        lut_view: &wgpu::TextureView,
        lut_sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("canvas_bg"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(lut_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(lut_sampler),
                },
            ],
        })
    }

    /// Replace the soft-proof 3D LUT contents (RGBA8, `PROOF_LUT_SIZE`³). Pass an
    /// identity LUT to disable the visual effect. Display-only — never touches
    /// document pixels or export.
    pub fn upload_proof_lut(&self, data: &[u8]) {
        let n = crate::core::cms::PROOF_LUT_SIZE as u32;
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.lut3d_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(n * 4),
                rows_per_image: Some(n),
            },
            wgpu::Extent3d {
                width: n,
                height: n,
                depth_or_array_layers: n,
            },
        );
    }

    pub fn write_canvas_uniforms(&self, u: &CanvasUniforms) {
        self.queue
            .write_buffer(&self.canvas_uniform_buf, 0, bytemuck::bytes_of(u));
    }

    pub fn write_cursor_uniforms(&self, u: &CursorUniforms) {
        self.queue
            .write_buffer(&self.cursor_uniform_buf, 0, bytemuck::bytes_of(u));
    }

    pub fn write_selection_uniforms(&self, u: &SelectionUniforms) {
        self.queue
            .write_buffer(&self.selection_uniform_buf, 0, bytemuck::bytes_of(u));
    }

    fn ensure_canvas_texture_size(&mut self, w: u32, h: u32) {
        let w = w.max(1);
        let h = h.max(1);
        if self.canvas_texture.width() == w && self.canvas_texture.height() == h {
            return;
        }
        self.canvas_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("canvas_tex"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.rebuild_bind_group();
    }

    pub fn clear_canvas_override(&mut self) {
        self.canvas_override_active = false;
    }

    /// Upload a saved alpha channel as a grayscale display plane. Normal
    /// canvas-space rendering uses canvas-sized pixels; viewport streaming
    /// rasterizes the mask into the screen-sized display texture.
    pub fn upload_alpha_plane(
        &mut self,
        mask: &[u8],
        canvas_w: u32,
        canvas_h: u32,
        view_offset_x: f32,
        view_offset_y: f32,
        zoom: f32,
        dirty: Option<(u32, u32, u32, u32)>,
    ) {
        if canvas_w == 0 || canvas_h == 0 {
            return;
        }

        const ALIGN: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let align_row = |bytes: u32| ((bytes + ALIGN - 1) / ALIGN) * ALIGN;

        if self.compositor.canvas_space {
            self.ensure_canvas_texture_size(canvas_w, canvas_h);
            let (mut x0, mut y0, mut x1, mut y1) = dirty.unwrap_or((0, 0, canvas_w, canvas_h));
            x0 = x0.min(canvas_w);
            y0 = y0.min(canvas_h);
            x1 = x1.min(canvas_w).max(x0);
            y1 = y1.min(canvas_h).max(y0);
            let w = x1 - x0;
            let h = y1 - y0;
            if w == 0 || h == 0 {
                self.canvas_override_active = true;
                return;
            }

            let bytes_per_row = align_row(w * 4);
            let mut upload = vec![0u8; (bytes_per_row * h) as usize];
            for row in 0..h {
                let y = y0 + row;
                let dst_row = row as usize * bytes_per_row as usize;
                for col in 0..w {
                    let x = x0 + col;
                    let src = y as usize * canvas_w as usize + x as usize;
                    let v = mask.get(src).copied().unwrap_or(0);
                    let dst = dst_row + col as usize * 4;
                    upload[dst] = v;
                    upload[dst + 1] = v;
                    upload[dst + 2] = v;
                    upload[dst + 3] = 255;
                }
            }

            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.canvas_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: x0, y: y0, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &upload,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
        } else {
            let sw = self.main.config.width.max(1);
            let sh = self.main.config.height.max(1);
            if zoom <= 0.0 {
                return;
            }
            self.ensure_canvas_texture_size(sw, sh);
            let (sx0, sy0, sx1, sy1) = if let Some((x0, y0, x1, y1)) = dirty.filter(|_| zoom > 0.0)
            {
                let sx0 = (view_offset_x + x0 as f32 * zoom)
                    .floor()
                    .clamp(0.0, sw as f32);
                let sy0 = (view_offset_y + y0 as f32 * zoom)
                    .floor()
                    .clamp(0.0, sh as f32);
                let sx1 = (view_offset_x + x1 as f32 * zoom)
                    .ceil()
                    .clamp(0.0, sw as f32);
                let sy1 = (view_offset_y + y1 as f32 * zoom)
                    .ceil()
                    .clamp(0.0, sh as f32);
                (sx0 as u32, sy0 as u32, sx1 as u32, sy1 as u32)
            } else {
                (0, 0, sw, sh)
            };
            let w = sx1.saturating_sub(sx0);
            let h = sy1.saturating_sub(sy0);
            if w == 0 || h == 0 {
                self.canvas_override_active = true;
                return;
            }

            let bytes_per_row = align_row(w * 4);
            let mut upload = vec![0u8; (bytes_per_row * h) as usize];
            let inv_zoom = 1.0 / zoom;
            let cw = canvas_w as i32;
            let ch = canvas_h as i32;
            for row in 0..h {
                let sy = sy0 + row;
                let cy = ((sy as f32 - view_offset_y) * inv_zoom).floor() as i32;
                let dst_row = row as usize * bytes_per_row as usize;
                for col in 0..w {
                    let sx = sx0 + col;
                    let cx = ((sx as f32 - view_offset_x) * inv_zoom).floor() as i32;
                    let v = if cx >= 0 && cy >= 0 && cx < cw && cy < ch {
                        let src = cy as usize * canvas_w as usize + cx as usize;
                        mask.get(src).copied().unwrap_or(0)
                    } else {
                        0
                    };
                    let dst = dst_row + col as usize * 4;
                    upload[dst] = v;
                    upload[dst + 1] = v;
                    upload[dst + 2] = v;
                    upload[dst + 3] = 255;
                }
            }

            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.canvas_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: sx0,
                        y: sy0,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &upload,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
        }

        self.canvas_override_active = true;
    }

    /// Upload the selection mask to the GPU texture.
    ///
    /// When `is_large_canvas = false` (normal):
    ///   - Upload the whole canvas mask directly (~canvas size).
    ///
    /// Khi `is_large_canvas = true`:
    ///   - Texture is screen-sized (not canvas-sized).
    ///   - Rasterize the canvas mask to screen space: each screen pixel (sx, sy) →
    ///     canvas pixel cx = (sx − view_offset_x) / zoom, cy = (sy − view_offset_y) / zoom,
    ///     then subtract sel_offset to index into the mask buffer.
    ///   - Upload ~8 MB (1920×1080) instead of 139 MB+ (full canvas).
    ///
    /// `sel_offset_x/y`: selection translate offset from `Selection::offset`.
    pub fn update_selection_mask(
        &self,
        mask: &[u8],
        canvas_w: u32,
        canvas_h: u32,
        view_offset_x: f32,
        view_offset_y: f32,
        zoom: f32,
        sel_offset_x: i32,
        sel_offset_y: i32,
    ) {
        if canvas_w == 0 || canvas_h == 0 {
            return;
        }

        const ALIGN: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

        if self.is_large_canvas {
            let sw = self.main.config.width;
            let sh = self.main.config.height;
            if sw == 0 || sh == 0 || zoom <= 0.0 {
                return;
            }
            let Some(screen_len) = (sw as u64)
                .checked_mul(sh as u64)
                .and_then(|n| usize::try_from(n).ok())
            else {
                return;
            };

            let inv_zoom = 1.0 / zoom;
            let cw = canvas_w as i32;
            let ch = canvas_h as i32;

            let mut screen_mask = vec![0u8; screen_len];
            for sy in 0..sh {
                for sx in 0..sw {
                    let cx = ((sx as f32 - view_offset_x) * inv_zoom) as i32;
                    let cy = ((sy as f32 - view_offset_y) * inv_zoom) as i32;
                    let mx = cx - sel_offset_x;
                    let my = cy - sel_offset_y;
                    if mx >= 0 && my >= 0 && mx < cw && my < ch {
                        let mask_i = my as usize * canvas_w as usize + mx as usize;
                        if mask_i < mask.len() {
                            screen_mask[(sy * sw + sx) as usize] = mask[mask_i];
                        }
                    }
                }
            }

            let bytes_per_row = ((sw + ALIGN - 1) / ALIGN) * ALIGN;
            let upload: Vec<u8> = if bytes_per_row == sw {
                screen_mask
            } else {
                let Some(padded_len) = (bytes_per_row as u64)
                    .checked_mul(sh as u64)
                    .and_then(|n| usize::try_from(n).ok())
                else {
                    return;
                };
                let mut padded = vec![0u8; padded_len];
                for row in 0..sh as usize {
                    let src = &screen_mask[row * sw as usize..(row + 1) * sw as usize];
                    let dst = row * bytes_per_row as usize;
                    padded[dst..dst + sw as usize].copy_from_slice(src);
                }
                padded
            };
            self.queue.write_texture(
                self.selection_mask_tex.as_image_copy(),
                &upload,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(sh),
                },
                wgpu::Extent3d {
                    width: sw,
                    height: sh,
                    depth_or_array_layers: 1,
                },
            );
        } else {
            if canvas_w != self.selection_canvas_w || canvas_h != self.selection_canvas_h {
                return;
            }

            let w = canvas_w as usize;
            let h = canvas_h as usize;
            let Some(mask_len) = (canvas_w as u64)
                .checked_mul(canvas_h as u64)
                .and_then(|n| usize::try_from(n).ok())
            else {
                return;
            };
            if mask.len() < mask_len {
                return;
            }
            let bytes_per_row = ((canvas_w + ALIGN - 1) / ALIGN) * ALIGN;

            if bytes_per_row == canvas_w {
                self.queue.write_texture(
                    self.selection_mask_tex.as_image_copy(),
                    mask,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(bytes_per_row),
                        rows_per_image: Some(canvas_h),
                    },
                    wgpu::Extent3d {
                        width: canvas_w,
                        height: canvas_h,
                        depth_or_array_layers: 1,
                    },
                );
            } else {
                let Some(padded_len) = (bytes_per_row as u64)
                    .checked_mul(canvas_h as u64)
                    .and_then(|n| usize::try_from(n).ok())
                else {
                    return;
                };
                let mut padded = vec![0u8; padded_len];
                for row in 0..h {
                    let src = &mask[row * w..(row + 1) * w];
                    let dst = row * bytes_per_row as usize;
                    padded[dst..dst + w].copy_from_slice(src);
                }
                self.queue.write_texture(
                    self.selection_mask_tex.as_image_copy(),
                    &padded,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(bytes_per_row),
                        rows_per_image: Some(canvas_h),
                    },
                    wgpu::Extent3d {
                        width: canvas_w,
                        height: canvas_h,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }
    }

    /// Upload a partial region into the canvas texture (dirty rect).
    pub fn upload_patch(&self, pixels: &[u8], canvas_w: u32, x0: u32, y0: u32, x1: u32, y1: u32) {
        let tex_w = self.canvas_texture.width();
        let tex_h = self.canvas_texture.height();

        let x0 = x0.min(tex_w);
        let y0 = y0.min(tex_h);
        let x1 = x1.min(tex_w).max(x0);
        let y1 = y1.min(tex_h).max(y0);

        let w = x1 - x0;
        let h = y1 - y0;
        if w == 0 || h == 0 {
            return;
        }
        const ALIGN: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let bytes_per_row = (((w * 4) + ALIGN - 1) / ALIGN) * ALIGN;

        let mut patch = Vec::with_capacity((bytes_per_row * h) as usize);
        for row in y0..y1 {
            let start = ((row * canvas_w + x0) * 4) as usize;
            let end = start + (w * 4) as usize;
            if end <= pixels.len() {
                patch.extend_from_slice(&pixels[start..end]);
                patch.resize(patch.len() + (bytes_per_row - w * 4) as usize, 0);
            } else {
                patch.resize(patch.len() + bytes_per_row as usize, 0);
            }
        }
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.canvas_texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: x0, y: y0, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &patch,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Upload the whole canvas (used for undo/redo).
    pub fn upload_full(&self, pixels: &[u8], w: u32, h: u32) {
        self.queue.write_texture(
            self.canvas_texture.as_image_copy(),
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Rebuild the bind group after the texture view changes.
    pub fn rebuild_bind_group(&mut self) {
        self.canvas_view = self
            .canvas_texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.bind_group = Self::make_canvas_bind_group(
            &self.device,
            &self.bind_group_layout,
            &self.canvas_view,
            &self.sampler,
            &self.canvas_uniform_buf,
            &self.lut3d_view,
            &self.lut3d_sampler,
        );
        self.checker_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("checker_bg"),
            layout: &self.checker_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: self.canvas_uniform_buf.as_entire_binding(),
            }],
        });
    }

    /// Update the `is_large_canvas` flag and recreate the selection texture after rotate/resize.
    ///
    /// canvas_texture stays a 1×1 placeholder because the render pass reads from the compositor output.
    /// is_large_canvas = true when the canvas exceeds the GPU limit OR the CPU flat-buffer limit (25M px).
    pub fn resize_canvas_texture(&mut self, new_w: u32, new_h: u32) {
        let large = new_w > self.max_texture_dimension
            || new_h > self.max_texture_dimension
            || !Canvas::fits_flat_buffer(new_w, new_h);
        // No-op when nothing changed: this now runs on every
        // LayerStructureChanged (the size-change bookkeeping is centralised
        // there), not just on explicit crop/resize commits.
        if large == self.is_large_canvas
            && (new_w, new_h) == (self.selection_canvas_w, self.selection_canvas_h)
        {
            return;
        }
        self.is_large_canvas = large;
        self.recreate_selection_texture(new_w, new_h);
    }

    /// Canvas size the GPU-side canvas-dependent textures were last built for.
    pub fn canvas_texture_size(&self) -> (u32, u32) {
        (self.selection_canvas_w, self.selection_canvas_h)
    }

    /// Recreate the selection mask texture when the canvas changes size.
    ///
    /// When `is_large_canvas = true` (canvas exceeds max_texture_dimension):
    ///   - Create a SCREEN-sized texture (config.width × config.height), not canvas-sized.
    ///   - Avoids the "texture dimension exceeds limit" validation error → no crash.
    ///   - update_selection_mask() rasterizes the canvas mask to screen space on upload.
    ///
    /// When `is_large_canvas = false` (normal canvas):
    ///   - Create a canvas-sized texture, clamped to max_texture_dimension for safety.
    pub fn recreate_selection_texture(&mut self, canvas_w: u32, canvas_h: u32) {
        self.selection_canvas_w = canvas_w;
        self.selection_canvas_h = canvas_h;

        let (tex_w, tex_h) = if self.is_large_canvas {
            (
                self.main.config.width.max(1),
                self.main.config.height.max(1),
            )
        } else {
            (
                canvas_w.min(self.max_texture_dimension).max(1),
                canvas_h.min(self.max_texture_dimension).max(1),
            )
        };

        self.selection_mask_tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("selection_mask_tex"),
            size: wgpu::Extent3d {
                width: tex_w,
                height: tex_h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        self.selection_mask_view = self
            .selection_mask_tex
            .create_view(&wgpu::TextureViewDescriptor::default());

        self.selection_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("selection_bg"),
            layout: &self.selection_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.selection_uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&self.selection_mask_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.selection_mask_samp),
                },
            ],
        });
    }

    pub fn composite_layers(
        &mut self,
        layer_stack: &crate::core::layer::LayerStack,
        view_offset_x: f32,
        view_offset_y: f32,
        zoom: f32,
        dirty_rect: Option<(u32, u32, u32, u32)>,
        allow_backdrop_cache: bool,
    ) {
        let final_is_ping = self.compositor.composite_layers(
            &self.device,
            &self.queue,
            layer_stack,
            view_offset_x,
            view_offset_y,
            zoom,
            dirty_rect,
            allow_backdrop_cache,
        );

        self.current_frame_is_ping = final_is_ping;
    }
    /// `canvas_bg`: None = draw checkerboard (normal), Some([r,g,b,a]) = solid background color
    /// Used by Refine Selection "On Black" / "On White" view modes.
    /// `backdrop`: clear colour for the pasteboard around the canvas — themed
    /// (dark gray in Dark mode, light gray in Light mode). Ignored when
    /// `canvas_bg` is Some (that fills the whole viewport solid instead).
    /// `canvas_clip`: Some((x,y,w,h)) restricts the canvas quad to the canvas
    /// rectangle on screen (standard): layer content spilling outside the
    /// canvas (the black pasteboard) is NOT shown. None = draw fullscreen (old).
    /// `draw_canvas`: false skips the canvas quad entirely — used while the
    /// Develop window owns the Mode B compositor texture (it is baked for THAT
    /// window's view, so blitting it here would show garbage).
    ///
    /// Returns whether a frame was actually presented. `false` = the swapchain
    /// image could not be acquired (Outdated/Lost — e.g. after another OS
    /// window on the adapter is destroyed); the surface has been reconfigured
    /// and the CALLER must request another redraw, or the window keeps showing
    /// its last presented frame until some external event repaints it (the
    /// "stale until minimize/restore" bug).
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        _egui_ctx: &egui::Context,
        primitives: &[egui::ClippedPrimitive],
        textures_delta: &egui::TexturesDelta,
        pixels_per_point: f32,
        draw_selection: bool,
        canvas_bg: Option<[f64; 4]>,
        backdrop: [f64; 4],
        canvas_clip: Option<(u32, u32, u32, u32)>,
        draw_canvas: bool,
    ) -> bool {
        let surface_texture = match self.main.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.main.surface.configure(&self.device, &self.main.config);
                return false;
            }
            _ => return false,
        };

        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        for (id, delta) in &textures_delta.set {
            self.main
                .egui_renderer
                .update_texture(&self.device, &self.queue, *id, delta);
        }

        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.main.config.width, self.main.config.height],
            pixels_per_point,
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame_enc"),
            });

        self.main.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            primitives,
            &screen_descriptor,
        );

        let compositor_view = if self.current_frame_is_ping {
            &self.compositor.ping_view
        } else {
            &self.compositor.pong_view
        };
        let display_view = if self.canvas_override_active {
            &self.canvas_view
        } else {
            compositor_view
        };

        let current_canvas_bg = Self::make_canvas_bind_group(
            &self.device,
            &self.bind_group_layout,
            display_view,
            &self.sampler,
            &self.canvas_uniform_buf,
            &self.lut3d_view,
            &self.lut3d_sampler,
        );

        let bg = canvas_bg.unwrap_or(backdrop);
        let clear_color = wgpu::Color {
            r: bg[0],
            g: bg[1],
            b: bg[2],
            a: bg[3],
        };

        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                ..Default::default()
            });

            if canvas_bg.is_none() {
                // The checker shader is fullscreen and this scissor is the
                // authoritative visible canvas rectangle. Do not make the
                // shader infer canvas dimensions from unrelated uniform slots.
                if let Some((cx, cy, cw, ch)) = canvas_clip {
                    let cw = cw.min(self.main.config.width.saturating_sub(cx));
                    let ch = ch.min(self.main.config.height.saturating_sub(cy));
                    if cw > 0 && ch > 0 {
                        rpass.set_scissor_rect(cx, cy, cw, ch);
                        rpass.set_pipeline(&self.checker_pipeline);
                        rpass.set_bind_group(0, &self.checker_bind_group, &[]);
                        rpass.draw(0..4, 0..1);
                        rpass.set_scissor_rect(
                            0,
                            0,
                            self.main.config.width,
                            self.main.config.height,
                        );
                    }
                }
            }

            if draw_canvas {
                if let Some((cx, cy, cw, ch)) = canvas_clip {
                    let cw = cw.min(self.main.config.width.saturating_sub(cx));
                    let ch = ch.min(self.main.config.height.saturating_sub(cy));
                    if cw > 0 && ch > 0 {
                        rpass.set_scissor_rect(cx, cy, cw, ch);
                    }
                }
                rpass.set_pipeline(&self.render_pipeline);
                rpass.set_bind_group(0, &current_canvas_bg, &[]);
                rpass.draw(0..4, 0..1);
                if canvas_clip.is_some() {
                    rpass.set_scissor_rect(0, 0, self.main.config.width, self.main.config.height);
                }
            }

            if draw_selection {
                rpass.set_pipeline(&self.selection_pipeline);
                rpass.set_bind_group(0, &self.selection_bind_group, &[]);
                rpass.draw(0..4, 0..1);
            }

            rpass.set_pipeline(&self.cursor_pipeline);
            rpass.set_bind_group(0, &self.cursor_bind_group, &[]);
            rpass.draw(0..4, 0..1);

            let mut rpass_static = rpass.forget_lifetime();
            self.main
                .egui_renderer
                .render(&mut rpass_static, primitives, &screen_descriptor);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        surface_texture.present();

        for id in &textures_delta.free {
            self.main.egui_renderer.free_texture(id);
        }
        true
    }

    /// Render one frame of the Develop stage's second OS window: its egui UI
    /// over the canvas blit (uniforms supplied by the caller — Mode A positions
    /// the canvas-sized texture, Mode B fullscreen-blits the viewport texture
    /// baked for this window). No-op when the Develop window is closed.
    ///
    /// Like [`Self::render`], returns whether a frame was presented — on
    /// `false` the surface was reconfigured and the caller must request
    /// another redraw or the window keeps showing its previous frame.
    ///
    /// The `WindowSurface` is moved out of `self.develop` for the duration so
    /// every shared GPU field (`device`, `queue`, the pipelines) can be borrowed
    /// immutably alongside the `&mut` egui renderer without aliasing — `render()`
    /// only mutates its own window's `egui_renderer`, so nothing shared changes.
    pub fn render_develop(
        &mut self,
        primitives: &[egui::ClippedPrimitive],
        textures_delta: &egui::TexturesDelta,
        pixels_per_point: f32,
        backdrop: [f64; 4],
        canvas_uniforms: Option<CanvasUniforms>,
    ) -> bool {
        let Some(mut ws) = self.develop.take() else {
            return true; // Window closed: nothing to present, nothing to retry.
        };
        let presented = self.render_develop_inner(
            &mut ws,
            primitives,
            textures_delta,
            pixels_per_point,
            backdrop,
            canvas_uniforms,
        );
        // Put the surface back regardless of transient present failures.
        self.develop = Some(ws);
        presented
    }

    fn render_develop_inner(
        &self,
        ws: &mut WindowSurface,
        primitives: &[egui::ClippedPrimitive],
        textures_delta: &egui::TexturesDelta,
        pixels_per_point: f32,
        backdrop: [f64; 4],
        canvas_uniforms: Option<CanvasUniforms>,
    ) -> bool {
        let surface_texture = match ws.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                ws.surface.configure(&self.device, &ws.config);
                return false;
            }
            _ => return false,
        };

        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        for (id, delta) in &textures_delta.set {
            ws.egui_renderer
                .update_texture(&self.device, &self.queue, *id, delta);
        }

        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [ws.config.width, ws.config.height],
            pixels_per_point,
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("develop_frame_enc"),
            });

        ws.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            primitives,
            &screen_descriptor,
        );

        let clear_color = wgpu::Color {
            r: backdrop[0],
            g: backdrop[1],
            b: backdrop[2],
            a: backdrop[3],
        };

        // Canvas quad: sample the shared compositor output. Mode A positions the
        // canvas-sized texture by this window's offset/zoom; Mode B fullscreen-
        // blits the viewport-sized texture the compositor baked for this window.
        // `None` = nothing to show; just the dark backdrop.
        let canvas_bg = canvas_uniforms.map(|u| {
            self.queue
                .write_buffer(&self.develop_canvas_uniform_buf, 0, bytemuck::bytes_of(&u));
            let compositor_view = if self.current_frame_is_ping {
                &self.compositor.ping_view
            } else {
                &self.compositor.pong_view
            };
            let display_view = if self.canvas_override_active {
                &self.canvas_view
            } else {
                compositor_view
            };
            Self::make_canvas_bind_group(
                &self.device,
                &self.bind_group_layout,
                display_view,
                &self.sampler,
                &self.develop_canvas_uniform_buf,
                &self.lut3d_view,
                &self.lut3d_sampler,
            )
        });

        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("develop_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                ..Default::default()
            });

            if let Some(bg) = &canvas_bg {
                rpass.set_pipeline(&self.render_pipeline);
                rpass.set_bind_group(0, bg, &[]);
                rpass.draw(0..4, 0..1);
            }

            let mut rpass_static = rpass.forget_lifetime();
            ws.egui_renderer
                .render(&mut rpass_static, primitives, &screen_descriptor);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        surface_texture.present();

        for id in &textures_delta.free {
            ws.egui_renderer.free_texture(id);
        }
        true
    }
}

/// Checkerboard — drawn over the canvas area, pattern by real pixel coords.
const CHECKER_SHADER: &str = r#"
struct CanvasUniforms {
    offset:      vec2<f32>,
    zoom:        f32,
    vp_mode:     f32,
    screen_size: vec2<f32>,
    proof_enabled: f32,
    channel_view:  f32,
};

@group(0) @binding(0) var<uniform> u: CanvasUniforms;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// Fullscreen quad — clipped later in the fragment by canvas bounds.
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    let tex_size = vec2<f32>(
        // Derive canvas size back from NDC: map uv [0,1] to screen
        // Really we want quad = the exact canvas area on screen
        1.0, 1.0  // placeholder, tính trong fragment
    );
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    // NDC of the canvas's four corners
    // canvas corner pixel = u.offset + corner * canvas_pixel_size * u.zoom
    // But we don't know canvas size here → use a fullscreen quad, clip in the FS
    let ndc = corner * 2.0 - 1.0;
    var out: VsOut;
    out.pos = vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
    out.uv = corner;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Screen pixel position
    let sp = in.pos.xy;

    // The render pass scissors this fullscreen shader to the visible canvas.
    // Derive only the pattern coordinates here; canvas bounds do not belong in
    // the uniform and must never alias proof/channel display controls again.
    let canvas_px = (sp - u.offset) / u.zoom;

    // Checkerboard 8px
    let CELL: f32 = 8.0;
    let cx = floor(canvas_px.x / CELL);
    let cy = floor(canvas_px.y / CELL);
    let checker = (i32(cx) + i32(cy)) % 2;

    // Keep transparency clearly visible in both application themes.  The old
    // 0.03/0.02 values were almost black and made the checkerboard look like a
    // missing/solid canvas background, especially for transparent PNG files.
    let light = vec4<f32>(0.34, 0.34, 0.34, 1.0);
    let dark  = vec4<f32>(0.24, 0.24, 0.24, 1.0);

    if checker == 0 {
        return light;
    } else {
        return dark;
    }
}
"#;

const CANVAS_SHADER: &str = r#"
struct CanvasUniforms {
    offset:      vec2<f32>,
    zoom:        f32,
    // 0.0 = Mode A: normal GPU compositor path (canvas-sized texture, zoom/offset applied here)
    // 1.0 = Mode B: large canvas / viewport streaming (screen-sized texture, content pre-baked by CPU)
    vp_mode:     f32,
    screen_size: vec2<f32>,
    proof_enabled: f32,
    // Channels panel plate view: 0 = composite, 1..3 = R/G/B as grayscale,
    // 4 = saved alpha-channel plane (uploaded as grayscale).
    channel_view: f32,
};

@group(0) @binding(0) var canvas_tex: texture_2d<f32>;
@group(0) @binding(1) var canvas_samp: sampler;
@group(0) @binding(2) var<uniform> u: CanvasUniforms;
@group(0) @binding(3) var proof_lut: texture_3d<f32>;
@group(0) @binding(4) var proof_samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    var pixel_pos: vec2<f32>;
    if u.vp_mode > 0.5 {
        // Mode B: viewport texture covers the full screen: fullscreen blit, no transform.
        // Canvas data is pre-composited into screen pixels by CPU, including zoom/pan.
        pixel_pos = corner * u.screen_size;
    } else {
        // Mode A: canvas quad sized by texture dimensions, positioned with offset+zoom.
        let tex_size = vec2<f32>(textureDimensions(canvas_tex));
        pixel_pos = u.offset + corner * tex_size * u.zoom;
    }
    let ndc = pixel_pos / u.screen_size * 2.0 - 1.0;
    var out: VsOut;
    out.pos = vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
    out.uv = corner;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(canvas_tex, canvas_samp, in.uv);
    // Soft-proof: remap the displayed colour through the 3D LUT. Sampled
    // unconditionally and blended by proof_enabled (0/1) so the off path is a
    // bit-exact pass-through and there is no non-uniform control flow.
    // N must match core::cms::PROOF_LUT_SIZE.
    let n = 17.0;
    let rgb = clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    let coord = rgb * ((n - 1.0) / n) + (0.5 / n);
    let proofed = textureSample(proof_lut, proof_samp, coord).rgb;
    var out_rgb = mix(c.rgb, proofed, u.proof_enabled);
    // Single-channel plate (Channels panel): show one channel as grayscale.
    // Plates are document data, not display simulation; proof is bypassed.
    if u.channel_view > 0.5 {
        var v = c.r;
        if u.channel_view > 2.5 {
            v = c.b;
        } else if u.channel_view > 1.5 {
            v = c.g;
        }
        out_rgb = vec3<f32>(v);
    }
    return vec4<f32>(out_rgb, c.a);
}
"#;

const CURSOR_SHADER: &str = r#"
struct CursorUniforms {
    cursor_pos: vec2<f32>,
    brush_size: f32,
    _pad: f32,
    screen_size: vec2<f32>,
    _pad2: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: CursorUniforms;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) local: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let local = corner * 2.0 - 1.0;
    let r = u.brush_size + 2.0;
    let pixel = u.cursor_pos + local * r;
    let ndc = pixel / u.screen_size * 2.0 - 1.0;
    var out: VsOut;
    out.pos = vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
    out.local = local;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if u.brush_size < 0.5 { discard; }
    let dist = length(in.local) * (u.brush_size + 2.0);
    let r = u.brush_size;
    let black_diff = abs(dist - r);
    if black_diff < 1.5 {
        let a = 1.0 - black_diff / 1.5;
        return vec4<f32>(0.0, 0.0, 0.0, a * 0.9);
    }
    let white_diff = abs(dist - (r - 2.0));
    if white_diff < 1.0 {
        let a = 1.0 - white_diff;
        return vec4<f32>(1.0, 1.0, 1.0, a * 0.6);
    }
    return vec4<f32>(0.0, 0.0, 0.0, 0.0);
}
"#;

/// Marching-ants selection outline — animated border drawn around the selection rect.
const SELECTION_SHADER: &str = r#"
// ── Selection Marching Ants Shader ──────────────────────────────────────────
// Full-canvas quad ensures correct rendering for ANY selection shape.
// Screen-space edge detection handles zoom levels correctly.

struct SelectionUniforms {
    rect:        vec4<f32>,   // unused in vertex (kept for layout compat)
    offset:      vec2<f32>,   // canvas screen offset
    zoom:        f32,
    time:        f32,
    screen_size: vec2<f32>,
    canvas_size: vec2<f32>,
    sel_offset:  vec2<f32>,
    _pad:        vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: SelectionUniforms;
@group(0) @binding(1) var mask_tex:  texture_2d<f32>;
@group(0) @binding(2) var mask_samp: sampler;

struct VsOut { @builtin(position) pos: vec4<f32> };

fn mask_at(p: vec2<i32>) -> f32 {
    if p.x < 0 || p.y < 0 || p.x >= i32(u.canvas_size.x) || p.y >= i32(u.canvas_size.y) {
        return 0.0;
    }
    return textureLoad(mask_tex, p, 0).r;
}

fn mask_at_canvas(p: vec2<f32>) -> f32 {
    let fp = floor(p);
    return mask_at(vec2<i32>(i32(fp.x), i32(fp.y)));
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    let sx0 = u.offset.x;
    let sy0 = u.offset.y;
    let sx1 = u.offset.x + u.canvas_size.x * u.zoom;
    let sy1 = u.offset.y + u.canvas_size.y * u.zoom;
    let corners = array<vec2<f32>,4>(
        vec2(sx0,sy0), vec2(sx1,sy0), vec2(sx0,sy1), vec2(sx1,sy1)
    );
    let sp = corners[vi];
    let ndc = sp / u.screen_size * 2.0 - 1.0;
    return VsOut(vec4(ndc.x, -ndc.y, 0.0, 1.0));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let canvas_pos = (in.pos.xy - u.offset) / u.zoom;

    // Bbox optimization in canvas coordinates
    let bbox_min = u.rect.xy;
    let bbox_max = u.rect.zw;
    // expand by 2 pixels to allow edge detection on the outside
    if canvas_pos.x < bbox_min.x - 2.0 || canvas_pos.x > bbox_max.x + 2.0 ||
       canvas_pos.y < bbox_min.y - 2.0 || canvas_pos.y > bbox_max.y + 2.0 { discard; }

    // Exact pixel lookup. Using a screen-pixel-sized UV offset makes the sampled
    // neighbour jump multiple canvas pixels when zoomed out, which widens the
    // edge into 2-3 marching-ant rows. Integer mask coords keep it one edge.
    let mask_pos_f = canvas_pos - u.sel_offset;
    let mask_floor = floor(mask_pos_f);
    let mask_pos = vec2<i32>(i32(mask_floor.x), i32(mask_floor.y));

    let c = mask_at(mask_pos);
    var sr = mask_at(mask_pos + vec2<i32>(1, 0));
    var sl = mask_at(mask_pos - vec2<i32>(1, 0));
    var st = mask_at(mask_pos - vec2<i32>(0, 1));
    var sb = mask_at(mask_pos + vec2<i32>(0, 1));

    // Single-sided edge detection (inner edge only):
    // Mark a pixel only when IT is inside the selection (c >= 0.5)
    // AND has a neighbor outside (< 0.5). Avoids double-sided → 2px thick.
    // Result: a crisp 1px line just like GIMP.
    var edge_r = c >= 0.5 && sr < 0.5;
    var edge_l = c >= 0.5 && sl < 0.5;
    var edge_t = c >= 0.5 && st < 0.5;
    var edge_b = c >= 0.5 && sb < 0.5;

    if u.zoom < 1.0 {
        // One mask pixel is smaller than one screen pixel at fit zooms. Probe
        // exactly one screen pixel away in canvas space so the edge remains
        // visible without expanding into several marching-ant rows.
        let step = 1.0 / max(u.zoom, 0.0001);
        sr = mask_at_canvas(mask_pos_f + vec2<f32>( step, 0.0));
        sl = mask_at_canvas(mask_pos_f + vec2<f32>(-step, 0.0));
        st = mask_at_canvas(mask_pos_f + vec2<f32>(0.0, -step));
        sb = mask_at_canvas(mask_pos_f + vec2<f32>(0.0,  step));
        edge_r = c >= 0.5 && sr < 0.5;
        edge_l = c >= 0.5 && sl < 0.5;
        edge_t = c >= 0.5 && st < 0.5;
        edge_b = c >= 0.5 && sb < 0.5;
    }

    let is_edge = edge_r || edge_l || edge_t || edge_b;

    if !is_edge { discard; }

    // At high zoom (≥1) each canvas pixel covers many screen pixels.
    // Thinning: render only screen pixels near the edge to keep ~1.2 screen-px thickness.
    if u.zoom > 1.0 {
        let frac = fract(canvas_pos);  // [0,1) trong canvas pixel
        let dist_r = (1.0 - frac.x) * u.zoom;
        let dist_l = frac.x          * u.zoom;
        let dist_t = frac.y          * u.zoom;
        let dist_b = (1.0 - frac.y) * u.zoom;
        let W = 1.2;   // screen-px width của đường nét

        // Inner-edge only: check only its own edge side (c>=0.5 side)
        let near_edge = (edge_r && dist_r < W) || (edge_l && dist_l < W)
                     || (edge_t && dist_t < W) || (edge_b && dist_b < W);
        if !near_edge { discard; }
    }

    // March along the boundary direction (not diagonally):
    // dx_strength high → mask varies horizontally → vertical edge → march along Y
    // dy_strength high → mask varies vertically   → horizontal edge → march along X
    let vertical_edge = edge_r || edge_l;
    let march_coord = select(in.pos.x, in.pos.y, vertical_edge);
    let DASH: f32 = 4.0;   // 4px dash period (gần hơn với standard raster editors ~4px)
    let phase = (march_coord - u.time * 30.0) % (DASH * 2.0);
    let p = (phase + DASH * 2.0) % (DASH * 2.0);
    if p < DASH { return vec4(1.0, 1.0, 1.0, 1.0); }
    else        { return vec4(0.0, 0.0, 0.0, 1.0); }
}
"#;

#[cfg(test)]
mod shader_validation {
    fn validate(label: &str, src: &str) {
        let module = naga::front::wgsl::parse_str(src)
            .unwrap_or_else(|e| panic!("{label} failed to parse:\n{}", e.emit_to_string(src)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{label} failed to validate: {e:?}"));
    }

    // Guards the screen-blit shaders (incl. the soft-proof 3D-LUT sample added in
    // CANVAS_SHADER) — a broken blit shader is a black canvas at runtime.
    #[test]
    fn blit_shaders_are_valid_wgsl() {
        validate("checker", super::CHECKER_SHADER);
        validate("canvas", super::CANVAS_SHADER);
        validate("cursor", super::CURSOR_SHADER);
        validate("selection", super::SELECTION_SHADER);
    }

    #[test]
    fn checker_shader_does_not_alias_display_controls_as_canvas_size() {
        let shader = super::CHECKER_SHADER;
        assert!(!shader.contains("_pad2"));
        assert!(!shader.contains("discard"));
        assert!(shader.contains("proof_enabled"));
        assert!(shader.contains("channel_view"));
    }
}

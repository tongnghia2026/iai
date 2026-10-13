//! GPU port of `core::develop::guided_lowpass_plane` (self-guided low-pass of
//! one scalar plane) for the Develop mode-5 pre-pass, where it builds the
//! Effects' regional base luminance without a host round trip. See
//! `guided.wgsl`; parity with the CPU filter is locked by the test below.

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GuidedParams {
    w: u32,
    h: u32,
    n: u32,
    r: u32,
    groups_x: u32,
    src: u32,
    src2: u32,
    dst: u32,
    dst2: u32,
    out_off: u32,
    eps: f32,
    _pad: f32,
    // pad the whole struct to the 256-byte dynamic-uniform stride
    _tail: [u32; 52],
}

const STRIDE: u64 = 256;
/// Outputs per invocation of the sliding box passes (`RUN` in guided.wgsl).
const RUN: u32 = 16;
/// f32 planes in the pool: I plus five scratch planes.
const POOL_PLANES: u64 = 6;
const ENTRIES: &[&str] = &["load", "box_h", "box_v", "coeffs", "finish"];

fn entry(name: &str) -> usize {
    ENTRIES.iter().position(|&e| e == name).unwrap()
}

/// Cached pipelines for the guided low-pass.
pub struct GuidedGpuRuntime {
    bgl: wgpu::BindGroupLayout,
    pipelines: Vec<wgpu::ComputePipeline>,
}

/// Pooled storage reused across frames.
#[derive(Default)]
pub struct GuidedBuffers {
    pool: Option<wgpu::Buffer>,
    uniform: Option<wgpu::Buffer>,
}

fn ensure(
    slot: &mut Option<wgpu::Buffer>,
    device: &wgpu::Device,
    label: &str,
    size: u64,
    usage: wgpu::BufferUsages,
) {
    if slot.as_ref().is_some_and(|b| b.size() >= size) {
        return;
    }
    let size = size.div_ceil(1 << 20) * (1 << 20);
    *slot = Some(device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage,
        mapped_at_creation: false,
    }));
}

impl GuidedGpuRuntime {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("guided_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("guided.wgsl").into()),
        });
        let storage = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: false },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("guided_bgl"),
            entries: &[
                storage(0),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<GuidedParams>() as u64,
                        ),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                storage(3),
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("guided_pl"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pipelines = ENTRIES
            .iter()
            .map(|name| {
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("guided_pipe"),
                    layout: Some(&layout),
                    module: &shader,
                    entry_point: Some(name),
                    compilation_options: Default::default(),
                    cache: None,
                })
            })
            .collect();
        Self { bgl, pipelines }
    }

    /// Encode the guided low-pass of the `w×h` plane held in `src.r` (radius
    /// `r`, regulariser `eps`), writing the result packed into `out` from f32
    /// element `out_off`.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        buffers: &mut GuidedBuffers,
        src: &wgpu::TextureView,
        out: &wgpu::Buffer,
        out_off: u32,
        w: u32,
        h: u32,
        r: u32,
        eps: f32,
    ) {
        let n = w * h;
        if n == 0 {
            return;
        }
        let limit = device.limits().max_compute_workgroups_per_dimension.max(1);
        // Invocations per pass: one per pixel, or one per RUN-long box run.
        let groups = |items: u32| {
            let total = items.div_ceil(64).max(1);
            let gx = total.min(limit);
            (gx, total.div_ceil(gx))
        };
        let mut base: GuidedParams = bytemuck::Zeroable::zeroed();
        base.w = w;
        base.h = h;
        base.n = n;
        base.r = r;
        base.out_off = out_off;
        base.eps = eps;
        let plane = |k: u32| k * n;
        let (i, s1, s2, s3, s4, t) = (plane(0), plane(1), plane(2), plane(3), plane(4), plane(5));
        let pass = |name: &str, src: u32, src2: u32, dst: u32, dst2: u32| {
            let items = match name {
                "box_h" => w.div_ceil(RUN) * h,
                "box_v" => h.div_ceil(RUN) * w,
                _ => n,
            };
            let (gx, gy) = groups(items);
            let mut p = base;
            p.groups_x = gx;
            p.src = src;
            p.src2 = src2;
            p.dst = dst;
            p.dst2 = dst2;
            (entry(name), p, gy)
        };
        let passes = [
            pass("load", 0, 0, i, s1),
            pass("box_h", i, 0, t, 0),
            pass("box_v", t, 0, s2, 0),
            pass("box_h", s1, 0, t, 0),
            pass("box_v", t, 0, s3, 0),
            pass("coeffs", s2, s3, s1, s4),
            pass("box_h", s1, 0, t, 0),
            pass("box_v", t, 0, s2, 0),
            pass("box_h", s4, 0, t, 0),
            pass("box_v", t, 0, s3, 0),
            pass("finish", s2, s3, i, 0),
        ];
        ensure(
            &mut buffers.pool,
            device,
            "guided_pool",
            POOL_PLANES * n as u64 * 4,
            wgpu::BufferUsages::STORAGE,
        );
        ensure(
            &mut buffers.uniform,
            device,
            "guided_uniform",
            passes.len() as u64 * STRIDE,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let (Some(pool), Some(uniform)) = (&buffers.pool, &buffers.uniform) else {
            return;
        };
        let mut bytes = vec![0u8; passes.len() * STRIDE as usize];
        for (k, (_, p, _)) in passes.iter().enumerate() {
            let at = k * STRIDE as usize;
            bytes[at..at + std::mem::size_of::<GuidedParams>()]
                .copy_from_slice(bytemuck::bytes_of(p));
        }
        queue.write_buffer(uniform, 0, &bytes);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("guided_bg"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: pool.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: uniform,
                        offset: 0,
                        size: wgpu::BufferSize::new(std::mem::size_of::<GuidedParams>() as u64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(src),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: out.as_entire_binding(),
                },
            ],
        });
        let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("guided_pass"),
            timestamp_writes: None,
        });
        for (k, (pi, p, groups_y)) in passes.iter().enumerate() {
            cpass.set_pipeline(&self.pipelines[*pi]);
            cpass.set_bind_group(0, &bind_group, &[(k as u64 * STRIDE) as u32]);
            cpass.dispatch_workgroups(p.groups_x, *groups_y, 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_guided_lowpass_matches_cpu() {
        let Some((device, queue)) = crate::gpu::vector::renderer::headless_device() else {
            eprintln!("no headless GPU adapter; skipped");
            return;
        };
        let runtime = GuidedGpuRuntime::new(&device);
        for (w, h, r, eps) in [(97u32, 61u32, 6u32, 0.05f32), (180, 140, 24, 0.05)] {
            let plane: Vec<f32> = (0..w * h)
                .map(|i| {
                    let (x, y) = ((i % w) as f32, (i / w) as f32);
                    let step = if x > w as f32 * 0.4 { 0.7 } else { 0.2 };
                    step + 0.1 * (x * 0.37).sin() * (y * 0.21).cos()
                })
                .collect();
            let cpu = crate::core::develop::guided_lowpass_plane(
                &plane, w as usize, h as usize, r as usize, eps,
            );
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let row = w as usize * 16;
            let padded = row.div_ceil(256) * 256;
            let mut bytes = vec![0u8; padded * h as usize];
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let v = plane[y * w as usize + x];
                    bytes[y * padded + x * 16..y * padded + x * 16 + 4]
                        .copy_from_slice(&v.to_ne_bytes());
                }
            }
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded as u32),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            let out_off = 16u32;
            let size = (out_off + w * h) as u64 * 4;
            let out = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut buffers = GuidedBuffers::default();
            let mut encoder = device.create_command_encoder(&Default::default());
            runtime.encode(
                &device,
                &queue,
                &mut encoder,
                &mut buffers,
                &view,
                &out,
                out_off,
                w,
                h,
                r,
                eps,
            );
            encoder.copy_buffer_to_buffer(&out, 0, &readback, 0, size);
            queue.submit([encoder.finish()]);
            let slice = readback.slice(..);
            slice.map_async(wgpu::MapMode::Read, |_| {});
            device.poll(wgpu::PollType::wait_indefinitely()).ok();
            let gpu: Vec<f32> = bytemuck::cast_slice(&slice.get_mapped_range()).to_vec();
            readback.unmap();
            let d = cpu
                .iter()
                .zip(&gpu[out_off as usize..])
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            println!("GPU vs CPU guided lowpass ({w}x{h}, r {r}) max abs diff = {d:.7}");
            assert!(d < 1e-4, "GPU guided lowpass diverges: {d}");
        }
    }
}

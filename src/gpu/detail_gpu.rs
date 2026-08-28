//! GPU port of `core::develop::detail::process_detail_plane` (display domain,
//! luma path: Sharpening + Noise Reduction) so the live preview runs the same
//! Detail as the CPU commit. Built as a sequence of compute dispatches over one
//! pooled storage buffer; see `detail.wgsl`.
//!
//! This module is currently a self-contained, headless-testable core (see the
//! `gpu_detail_parity` integration test). Compositor integration and the
//! linear/scene domain + chroma-NR (`color_noise_reduction`) land next.

use wgpu::util::DeviceExt;

/// Slider values folded to working units — mirrors `detail::DetailParams::new`.
#[derive(Clone, Copy, Debug)]
pub struct DetailWorkingParams {
    pub amount: f32,
    pub sigma: f32,
    pub detail: f32,
    pub masking: f32,
    pub nr: f32,
    pub color_nr: f32,
}

impl DetailWorkingParams {
    /// Fold raw slider values (0..=100, radius 0.5..=3) the same way the CPU does.
    pub fn from_sliders(
        sharpening: f32,
        sharpen_radius: f32,
        sharpen_detail: f32,
        sharpen_masking: f32,
        noise_reduction: f32,
        color_noise_reduction: f32,
    ) -> Self {
        Self {
            amount: (sharpening / 100.0).clamp(0.0, 1.0) * 1.5,
            sigma: sharpen_radius.clamp(0.3, 3.0),
            detail: (sharpen_detail / 100.0).clamp(0.0, 1.0),
            masking: (sharpen_masking / 100.0).clamp(0.0, 1.0),
            nr: (noise_reduction / 100.0).clamp(0.0, 1.0),
            color_nr: (color_noise_reduction / 100.0).clamp(0.0, 1.0),
        }
    }

    fn level_gain(&self) -> [f32; 3] {
        let t = ((self.sigma - 0.3) / 2.7).clamp(0.0, 1.0);
        [1.0, 0.35 + 0.65 * t, 0.9 * t]
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PassParams {
    w: u32,
    h: u32,
    n: u32,
    level: u32,
    flags: u32,
    linear: u32,
    _pad0: u32,
    _pad1: u32,
    src_off: u32,
    dst_off: u32,
    a_off: u32,
    b_off: u32,
    img_off: u32,
    luma_off: u32,
    chroma_off: u32,
    res_off: u32,
    d0_off: u32,
    d1_off: u32,
    d2_off: u32,
    cavg_off: u32,
    cavgtmp_off: u32,
    _pad2: u32,
    _pad3: u32,
    _pad4: u32,
    amount: f32,
    sigma: f32,
    detail: f32,
    masking: f32,
    nr: f32,
    color_nr: f32,
    lg0: f32,
    lg1: f32,
    lg2: f32,
    _fpad0: f32,
    _fpad1: f32,
    _fpad2: f32,
    // pad the whole struct to the 256-byte dynamic-uniform stride
    _tail: [u32; 28],
}

const FLAG_H: u32 = 1;
const FLAG_EA: u32 = 2;
const STRIDE: u64 = 256;

/// Region base offsets (in f32 elements) inside the pooled buffer.
struct Layout {
    n: u32,
    img: u32,
    luma: u32,
    chroma: u32,
    r_a: u32,
    r_b: u32,
    tmp: u32,
    d0: u32,
    d1: u32,
    d2: u32,
    cavg: u32,
    cavgtmp: u32,
    total: u32,
}

impl Layout {
    fn new(w: u32, h: u32) -> Self {
        let n = w * h;
        Self {
            n,
            img: 0,
            luma: 3 * n,
            chroma: 4 * n,
            r_a: 7 * n,
            r_b: 8 * n,
            tmp: 9 * n,
            d0: 10 * n,
            d1: 11 * n,
            d2: 12 * n,
            cavg: 13 * n,
            cavgtmp: 16 * n,
            total: 19 * n,
        }
    }
}

const ENTRIES: &[&str] = &[
    "split",
    "atrous",
    "diff",
    "nr_garrote",
    "box_blur",
    "reconstruct",
    "sharpen",
    "combine",
];

/// Run the display-domain Detail (Sharpening + Noise Reduction) on the GPU and
/// return the RGB result (`3·w·h` f32). Chroma NR (`color_nr`) is not yet applied.
/// Builds pipelines per call — fine for tests; the compositor path will cache them.
pub fn run_detail_display(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    rgb: &[f32],
    w: u32,
    h: u32,
    p: DetailWorkingParams,
) -> Vec<f32> {
    let lay = Layout::new(w, h);
    let n = lay.n as usize;
    assert_eq!(rgb.len(), 3 * n, "rgb must be 3*w*h");

    // Pool buffer: upload input into the img region, zero the rest.
    let mut pool_init = vec![0f32; lay.total as usize];
    pool_init[lay.img as usize..lay.img as usize + 3 * n].copy_from_slice(rgb);
    let pool = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("detail_pool"),
        contents: bytemuck::cast_slice(&pool_init),
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_SRC
            | wgpu::BufferUsages::COPY_DST,
    });

    // Build the pass list (entry index into ENTRIES, PassParams).
    let lg = p.level_gain();
    let base = PassParams {
        w,
        h,
        n: lay.n,
        level: 0,
        flags: 0,
        linear: 0,
        _pad0: 0,
        _pad1: 0,
        src_off: 0,
        dst_off: 0,
        a_off: 0,
        b_off: 0,
        img_off: lay.img,
        luma_off: lay.luma,
        chroma_off: lay.chroma,
        res_off: lay.r_b,
        d0_off: lay.d0,
        d1_off: lay.d1,
        d2_off: lay.d2,
        cavg_off: lay.cavg,
        cavgtmp_off: lay.cavgtmp,
        _pad2: 0,
        _pad3: 0,
        _pad4: 0,
        amount: p.amount,
        sigma: p.sigma,
        detail: p.detail,
        masking: p.masking,
        nr: p.nr,
        color_nr: p.color_nr,
        lg0: lg[0],
        lg1: lg[1],
        lg2: lg[2],
        _fpad0: 0.0,
        _fpad1: 0.0,
        _fpad2: 0.0,
        _tail: [0; 28],
    };

    let entry = |name: &str| ENTRIES.iter().position(|&e| e == name).unwrap();
    let mut passes: Vec<(usize, PassParams)> = Vec::new();
    // split
    passes.push((entry("split"), base));

    let do_luma = p.nr > 0.001 || p.amount > 0.001;
    if do_luma {
        // decompose: residual ping-pong luma -> rB -> rA -> rB, all edge-aware.
        let smooth =
            |passes: &mut Vec<(usize, PassParams)>, src: u32, mid: u32, dst: u32, level: u32| {
                let mut ph = base;
                ph.level = level;
                ph.flags = FLAG_H | FLAG_EA;
                ph.src_off = src;
                ph.dst_off = mid;
                passes.push((entry("atrous"), ph));
                let mut pv = base;
                pv.level = level;
                pv.flags = FLAG_EA;
                pv.src_off = mid;
                pv.dst_off = dst;
                passes.push((entry("atrous"), pv));
            };
        let diff = |passes: &mut Vec<(usize, PassParams)>, a: u32, b: u32, dst: u32| {
            let mut pd = base;
            pd.a_off = a;
            pd.b_off = b;
            pd.dst_off = dst;
            passes.push((entry("diff"), pd));
        };
        // level 0: luma -> rB, d0 = luma - rB
        smooth(&mut passes, lay.luma, lay.tmp, lay.r_b, 0);
        diff(&mut passes, lay.luma, lay.r_b, lay.d0);
        // level 1: rB -> rA, d1 = rB - rA
        smooth(&mut passes, lay.r_b, lay.tmp, lay.r_a, 1);
        diff(&mut passes, lay.r_b, lay.r_a, lay.d1);
        // level 2: rA -> rB, d2 = rA - rB
        smooth(&mut passes, lay.r_a, lay.tmp, lay.r_b, 2);
        diff(&mut passes, lay.r_a, lay.r_b, lay.d2);
        // residual is rB (base.res_off already = rB)

        if p.nr > 0.001 {
            passes.push((entry("nr_garrote"), base));
        }
        if p.amount > 0.001 {
            let mut bh = base;
            bh.flags = FLAG_H;
            bh.src_off = lay.chroma;
            bh.dst_off = lay.cavgtmp;
            passes.push((entry("box_blur"), bh));
            let mut bv = base;
            bv.flags = 0;
            bv.src_off = lay.cavgtmp;
            bv.dst_off = lay.cavg;
            passes.push((entry("box_blur"), bv));
            passes.push((entry("sharpen"), base));
        } else {
            passes.push((entry("reconstruct"), base));
        }
    }
    // combine
    passes.push((entry("combine"), base));

    // Uniform buffer: one 256-byte-strided PassParams per pass.
    let uniform_bytes = passes.len() as u64 * STRIDE;
    let uniform = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("detail_uniform"),
        size: uniform_bytes,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    for (i, (_, pp)) in passes.iter().enumerate() {
        queue.write_buffer(&uniform, i as u64 * STRIDE, bytemuck::bytes_of(pp));
    }

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("detail_shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("detail.wgsl").into()),
    });
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("detail_bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(
                        std::mem::size_of::<PassParams>() as u64
                    ),
                },
                count: None,
            },
        ],
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("detail_bg"),
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: pool.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &uniform,
                    offset: 0,
                    size: wgpu::BufferSize::new(std::mem::size_of::<PassParams>() as u64),
                }),
            },
        ],
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("detail_pl"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let pipelines: Vec<wgpu::ComputePipeline> = ENTRIES
        .iter()
        .map(|name| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("detail_pipe"),
                layout: Some(&pl),
                module: &shader,
                entry_point: Some(name),
                compilation_options: Default::default(),
                cache: None,
            })
        })
        .collect();

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("detail_encoder"),
    });
    {
        let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("detail_pass"),
            timestamp_writes: None,
        });
        let groups = (lay.n + 63) / 64;
        for (i, (pi, _)) in passes.iter().enumerate() {
            cpass.set_pipeline(&pipelines[*pi]);
            cpass.set_bind_group(0, &bind_group, &[(i as u64 * STRIDE) as u32]);
            cpass.dispatch_workgroups(groups, 1, 1);
        }
    }

    // Read back the img region.
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("detail_readback"),
        size: (3 * n * std::mem::size_of::<f32>()) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_buffer_to_buffer(
        &pool,
        (lay.img as u64) * 4,
        &readback,
        0,
        (3 * n * std::mem::size_of::<f32>()) as u64,
    );
    queue.submit([encoder.finish()]);

    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).ok();
    let data = slice.get_mapped_range();
    let out: Vec<f32> = bytemuck::cast_slice(&data).to_vec();
    drop(data);
    readback.unmap();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Headless GPU Detail (Sharpening + Noise Reduction, display domain) must
    /// match the CPU `apply_detail_to_display_buffer` within a tight tolerance,
    /// so the future live preview equals the commit.
    #[test]
    fn gpu_detail_matches_cpu_display() {
        let Some((device, queue)) = crate::gpu::vector::renderer::headless_device() else {
            eprintln!("no headless GPU adapter; skipped");
            return;
        };
        let (w, h) = (40u32, 24u32);
        // Edges + hashed noise so Sharpening, the masking gate and NR all engage.
        let hash = |i: usize, s: u32| -> f32 {
            let mut x = (i as u32)
                .wrapping_mul(2_654_435_761)
                .wrapping_add(s)
                .wrapping_add(2_463_534_242);
            x ^= x >> 15;
            x = x.wrapping_mul(2_246_822_519);
            x ^= x >> 13;
            (x as f32 / u32::MAX as f32) * 2.0 - 1.0
        };
        let mut rgb = vec![0f32; (3 * w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) as usize;
                let step = if x > w / 2 { 0.62 } else { 0.30 };
                let base = (step + 0.03 * hash(i, 7)).clamp(0.0, 1.0);
                rgb[i * 3] = base;
                rgb[i * 3 + 1] = (base + 0.02 * hash(i, 11)).clamp(0.0, 1.0);
                rgb[i * 3 + 2] = (base + 0.02 * hash(i, 13)).clamp(0.0, 1.0);
            }
        }

        // CPU reference.
        let mut settings = crate::core::develop::DevelopSettings::default();
        settings.sharpening = 70.0;
        settings.noise_reduction = 40.0;
        settings.sharpen_masking = 30.0;
        let mut cpu: Vec<[f32; 3]> = (0..(w * h) as usize)
            .map(|i| [rgb[i * 3], rgb[i * 3 + 1], rgb[i * 3 + 2]])
            .collect();
        crate::core::develop::apply_detail_to_display_buffer(
            &mut cpu, w as usize, h as usize, &settings, 1,
        );

        // GPU.
        let params = DetailWorkingParams::from_sliders(70.0, 1.0, 25.0, 30.0, 40.0, 0.0);
        let gpu = run_detail_display(&device, &queue, &rgb, w, h, params);

        let mut max_diff = 0f32;
        for i in 0..(w * h) as usize {
            for c in 0..3 {
                let d = (cpu[i][c] - gpu[i * 3 + c]).abs();
                max_diff = max_diff.max(d);
            }
        }
        println!(
            "GPU vs CPU Detail max abs diff = {max_diff:.6} ({:.3}/255)",
            max_diff * 255.0
        );
        assert!(
            max_diff < 3e-3,
            "GPU Detail diverges from CPU: max abs diff {max_diff} ({:.2}/255)",
            max_diff * 255.0
        );
    }
}

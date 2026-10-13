//! GPU port of `core::develop::detail_core` — Camera Raw–style Sharpening,
//! Noise Reduction and Colour Noise Reduction on a gamma-encoded luma/chroma
//! split, in both the display and linear/scene working domains, so the live
//! preview runs the same Detail as the CPU commit. Built as a sequence of
//! compute dispatches over one pooled storage buffer; see `detail.wgsl`.
//! Parity is locked by the headless tests below.
//!
//! Shared core used by the live native-resolution compositor and parity probes.

use crate::core::develop::{DetailPlan, DevelopSettings, DETAIL_HALO, DETAIL_LEVELS};

/// Detail sliders folded once on the CPU; the GPU consumes exactly these
/// numbers, so preview and commit share one parameterisation.
#[derive(Clone, Debug)]
pub struct DetailWorkingParams {
    plan: DetailPlan,
}

impl DetailWorkingParams {
    /// Full-resolution plan for the given Develop settings.
    pub fn from_settings(settings: &DevelopSettings) -> Self {
        Self::from_settings_scaled(settings, 1)
    }

    /// Plan for a plane sampled every `downsample` source pixels — the same
    /// `preview_scale` the CPU Detail uses on a reduced preview proxy.
    pub fn from_settings_scaled(settings: &DevelopSettings, downsample: u32) -> Self {
        Self {
            plan: DetailPlan::new(settings, downsample.max(1) as f32),
        }
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
    groups_x: u32,
    radius: u32,
    src_off: u32,
    dst_off: u32,
    a_off: u32,
    b_off: u32,
    gsrc_off: u32,
    gdst_off: u32,
    img_off: u32,
    luma_off: u32,
    chroma_off: u32,
    acc_off: u32,
    ga_off: u32,
    gb_off: u32,
    gf_off: u32,
    gm_off: u32,
    lo_off: u32,
    hi_off: u32,
    sigma: f32,
    atten: f32,
    tau: f32,
    k: f32,
    k_fine: f32,
    mask_lo: f32,
    mask_hi: f32,
    halo_h: f32,
    lc0: f32,
    lc1: f32,
    lc2: f32,
    _pad: f32,
    tex_x0: u32,
    tex_y0: u32,
    crop_x: u32,
    crop_y: u32,
    core_w: u32,
    core_h: u32,
    out_x0: u32,
    out_y0: u32,
    out_w: u32,
    out_base: u32,
    // pad the whole struct to the 256-byte dynamic-uniform stride
    _tail: [u32; 18],
}

const FLAG_H: u32 = 1;
const FLAG_FIRST: u32 = 2;
const FLAG_FINE: u32 = 4;
const FLAG_MASK: u32 = 8;
const FLAG_HALO: u32 = 16;
const STRIDE: u64 = 256;
/// f32 values per pixel in the pooled buffer (19 used, one spare).
const POOL_PER_PIXEL: u64 = 20;

/// Region base offsets (in f32 elements) inside the pooled buffer. The scratch
/// region is reused by each stage in turn.
struct Layout {
    w: u32,
    h: u32,
    n: u32,
    img: u32,
    luma: u32,
    chroma: u32,
    scratch: u32,
    total: u32,
}

impl Layout {
    fn new(w: u32, h: u32) -> Self {
        let n = w * h;
        Self {
            w,
            h,
            n,
            img: 0,
            luma: 3 * n,
            chroma: 4 * n,
            scratch: 7 * n,
            total: 19 * n,
        }
    }
    /// Scratch plane `k` in single-channel units.
    fn s(&self, k: u32) -> u32 {
        self.scratch + k * self.n
    }
}

const ENTRIES: &[&str] = &[
    "split",
    "cspeck",
    "catrous",
    "caccum",
    "cfinish",
    "patrous",
    "laccum",
    "lfinish",
    "gauss",
    "minmax",
    "sharpen",
    "combine",
    "load_tex",
    "store_out",
];

fn entry(name: &str) -> usize {
    ENTRIES.iter().position(|&e| e == name).unwrap()
}

fn gauss_radius(sigma: f32) -> u32 {
    crate::core::develop::gauss_taps(sigma).len() as u32 / 2
}

/// Display-domain convenience wrapper (Rec.709 luma, clamp at the ends), matching
/// `apply_detail_to_display_buffer`.
pub fn run_detail_display(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    rgb: &[f32],
    w: u32,
    h: u32,
    p: &DetailWorkingParams,
) -> Vec<f32> {
    run_detail(device, queue, rgb, w, h, p, false, [0.2126, 0.7152, 0.0722])
}

/// Cached shader and compute pipelines for repeated live-preview runs.
pub struct DetailGpuRuntime {
    bgl: wgpu::BindGroupLayout,
    pipelines: Vec<wgpu::ComputePipeline>,
    /// Placeholders for the resident-plane bindings on host round-trip runs.
    dummy_src_view: wgpu::TextureView,
    dummy_out: wgpu::Buffer,
}

impl DetailGpuRuntime {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("detail_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("detail.wgsl").into()),
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
            label: Some("detail_bgl"),
            entries: &[
                storage(0),
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
        let dummy_src = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("detail_dummy_src"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: RESIDENT_SRC_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let dummy_src_view = dummy_src.create_view(&wgpu::TextureViewDescriptor::default());
        let dummy_out = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("detail_dummy_out"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("detail_pl"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pipelines = ENTRIES
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
        Self {
            bgl,
            pipelines,
            dummy_src_view,
            dummy_out,
        }
    }

    /// Run one bounded plane. Callers handling full-resolution photographs
    /// should use [`run_detail_tiled_with_runtime`] so storage stays bounded.
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rgb: &[f32],
        w: u32,
        h: u32,
        p: &DetailWorkingParams,
        linear: bool,
        luma_coeff: [f32; 3],
    ) -> Vec<f32> {
        run_detail_impl(self, device, queue, rgb, w, h, &p.plan, linear, luma_coeff)
    }
}

/// Run the full Detail pipeline on the GPU and return the RGB result
/// (`3·w·h` f32). `linear` selects the scene/working domain (encode on entry,
/// decode on exit) vs the display domain (clamp at the ends). This convenience
/// entry point builds a runtime for one call; live preview keeps a
/// [`DetailGpuRuntime`] and uses [`run_detail_tiled_with_runtime`].
#[allow(clippy::too_many_arguments)]
pub fn run_detail(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    rgb: &[f32],
    w: u32,
    h: u32,
    p: &DetailWorkingParams,
    linear: bool,
    luma_coeff: [f32; 3],
) -> Vec<f32> {
    let runtime = DetailGpuRuntime::new(device);
    runtime.run(device, queue, rgb, w, h, p, linear, luma_coeff)
}

/// The dispatch list for one plane: (entry index, uniforms).
fn build_passes(lay: &Layout, base: PassParams, p: &DetailPlan) -> Vec<(usize, PassParams)> {
    let mut passes: Vec<(usize, PassParams)> = vec![(entry("split"), base)];
    if p.cnr {
        // 3-channel scratch planes: two ping-pong levels, a temp, the accumulator.
        let (ca, cb, ct, cacc) = (lay.s(0), lay.s(3), lay.s(6), lay.s(9));
        let mut cur = lay.chroma;
        if p.cnr_speck > 0.0 {
            let mut sp = base;
            sp.src_off = lay.chroma;
            sp.dst_off = cb;
            sp.atten = p.cnr_speck;
            passes.push((entry("cspeck"), sp));
            cur = cb;
        }
        for lev in 0..DETAIL_LEVELS {
            let dst = if lev % 2 == 0 { ca } else { cb };
            let mut ph = base;
            ph.level = lev as u32;
            ph.flags = FLAG_H;
            ph.sigma = p.cnr_sigma[lev];
            ph.src_off = cur;
            ph.dst_off = ct;
            passes.push((entry("catrous"), ph));
            let mut pv = ph;
            pv.flags = 0;
            pv.src_off = ct;
            pv.dst_off = dst;
            passes.push((entry("catrous"), pv));
            let mut pa = base;
            pa.flags = if lev == 0 { FLAG_FIRST } else { 0 };
            pa.a_off = cur;
            pa.b_off = dst;
            pa.acc_off = cacc;
            pa.atten = p.cnr_a[lev];
            pa.tau = p.cnr_tau[lev].max(1e-6);
            passes.push((entry("caccum"), pa));
            cur = dst;
        }
        let mut pf = base;
        pf.a_off = cur;
        pf.acc_off = cacc;
        passes.push((entry("cfinish"), pf));
    }

    if p.lnr {
        let (la, lb, lt, lacc) = (lay.s(0), lay.s(1), lay.s(2), lay.s(3));
        let mut cur = lay.luma;
        for lev in 0..DETAIL_LEVELS {
            let dst = if lev % 2 == 0 { la } else { lb };
            let mut ph = base;
            ph.level = lev as u32;
            ph.flags = FLAG_H;
            ph.src_off = cur;
            ph.dst_off = lt;
            passes.push((entry("patrous"), ph));
            let mut pv = ph;
            pv.flags = 0;
            pv.src_off = lt;
            pv.dst_off = dst;
            passes.push((entry("patrous"), pv));
            let mut pa = base;
            pa.level = lev as u32;
            pa.flags = if lev == 0 { FLAG_FIRST } else { 0 };
            pa.a_off = cur;
            pa.b_off = dst;
            pa.acc_off = lacc;
            pa.atten = p.lnr_w[lev] * p.lnr_alpha;
            pa.tau = p.lnr_tau[lev].max(1e-6);
            pa.mask_lo = p.lnr_edge_lo;
            pa.mask_hi = p.lnr_edge_hi;
            passes.push((entry("laccum"), pa));
            cur = dst;
        }
        let mut pf = base;
        pf.a_off = cur;
        pf.acc_off = lacc;
        passes.push((entry("lfinish"), pf));
    }

    if p.sharpen {
        let (gpa, gpb, gpf, gpm, lo, hi, t1, t2) = (
            lay.s(0),
            lay.s(1),
            lay.s(2),
            lay.s(3),
            lay.s(4),
            lay.s(5),
            lay.s(6),
            lay.s(7),
        );
        let blur = |passes: &mut Vec<(usize, PassParams)>, sigma: f32, dst: u32| {
            let r = gauss_radius(sigma);
            let mut pv = base;
            pv.radius = r;
            pv.sigma = sigma.max(1e-6);
            pv.flags = 0;
            pv.src_off = lay.luma;
            pv.dst_off = if r == 0 { dst } else { t1 };
            passes.push((entry("gauss"), pv));
            if r > 0 {
                let mut ph = pv;
                ph.flags = FLAG_H;
                ph.src_off = t1;
                ph.dst_off = dst;
                passes.push((entry("gauss"), ph));
            }
        };
        blur(&mut passes, p.sigma_a, gpa);
        blur(&mut passes, p.sigma_b, gpb);
        let mut flags = 0;
        if p.k_fine > 0.0 {
            blur(&mut passes, p.sigma_fine, gpf);
            flags |= FLAG_FINE;
        }
        if p.mask {
            blur(&mut passes, p.mask_sigma, gpm);
            flags |= FLAG_MASK;
        }
        if p.halo {
            let mut mh = base;
            mh.radius = p.halo_r;
            mh.flags = FLAG_H;
            mh.a_off = lay.luma;
            mh.b_off = lay.luma;
            mh.dst_off = t1;
            mh.gdst_off = t2;
            passes.push((entry("minmax"), mh));
            let mut mv = mh;
            mv.flags = 0;
            mv.a_off = t1;
            mv.b_off = t2;
            mv.dst_off = lo;
            mv.gdst_off = hi;
            passes.push((entry("minmax"), mv));
            flags |= FLAG_HALO;
        }
        let mut ps = base;
        ps.flags = flags;
        ps.ga_off = gpa;
        ps.gb_off = gpb;
        ps.gf_off = gpf;
        ps.gm_off = gpm;
        ps.lo_off = lo;
        ps.hi_off = hi;
        ps.k = p.k;
        ps.k_fine = p.k_fine;
        ps.mask_lo = p.mask_lo;
        ps.mask_hi = p.mask_hi;
        ps.halo_h = p.halo_h;
        passes.push((entry("sharpen"), ps));
    }
    passes.push((entry("combine"), base));
    passes
}

fn base_params(lay: &Layout, groups_x: u32, linear: bool, luma_coeff: [f32; 3]) -> PassParams {
    let mut base: PassParams = bytemuck::Zeroable::zeroed();
    base.w = lay.w;
    base.h = lay.h;
    base.n = lay.n;
    base.linear = u32::from(linear);
    base.groups_x = groups_x;
    base.img_off = lay.img;
    base.luma_off = lay.luma;
    base.chroma_off = lay.chroma;
    base.sigma = 1.0;
    base.tau = 1.0;
    base.mask_hi = 1.0;
    base.halo_h = 1.0;
    base.lc0 = luma_coeff[0];
    base.lc1 = luma_coeff[1];
    base.lc2 = luma_coeff[2];
    base
}

fn detail_bind_group(
    runtime: &DetailGpuRuntime,
    device: &wgpu::Device,
    pool: &wgpu::Buffer,
    uniform: &wgpu::Buffer,
    src: &wgpu::TextureView,
    out: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("detail_bg"),
        layout: &runtime.bgl,
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
                    size: wgpu::BufferSize::new(std::mem::size_of::<PassParams>() as u64),
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
    })
}

/// Format of the resident input plane read by [`DetailGpuRuntime::encode_resident`]
/// (RGB in `.rgb`, full f32 so the plane matches a CPU buffer bit for bit).
pub const RESIDENT_SRC_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;

/// Largest pooled plane (pixels, apron included) one binding can hold.
fn max_plane_pixels(device: &wgpu::Device) -> u64 {
    let limits = device.limits();
    let bytes = limits
        .max_buffer_size
        .min(limits.max_storage_buffer_binding_size);
    (bytes.saturating_mul(9) / 10 / (POOL_PER_PIXEL * 4)).max(1)
}

/// One apron'd tile of a resident plane: the source window `[x0, x0+w) ×
/// [y0, y0+h)` whose `core` (at `crop` inside it) lands at `core_x/core_y`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ResidentTile {
    x0: u32,
    y0: u32,
    w: u32,
    h: u32,
    crop_x: u32,
    crop_y: u32,
    core_x: u32,
    core_y: u32,
    core_w: u32,
    core_h: u32,
}

/// Split a `w×h` plane into tiles whose pooled storage fits `max_plane`
/// pixels. The whole plane when it fits; else full-width strips (one apron
/// band per seam); square tiles only for planes too wide for a strip.
fn plan_resident_tiles(w: u32, h: u32, max_plane: u64, halo: u32) -> Vec<ResidentTile> {
    let (core_w, core_h) = if (w as u64) * (h as u64) <= max_plane {
        (w, h)
    } else {
        let strip_rows = max_plane / w.max(1) as u64;
        if strip_rows >= (2 * halo + 64) as u64 {
            (w, (strip_rows - 2 * halo as u64) as u32)
        } else {
            let edge = ((max_plane as f64).sqrt().floor() as u32)
                .saturating_sub(2 * halo)
                .max(1);
            (edge, edge)
        }
    };
    let mut tiles = Vec::new();
    for core_y in (0..h).step_by(core_h as usize) {
        let ch = core_h.min(h - core_y);
        for core_x in (0..w).step_by(core_w as usize) {
            let cw = core_w.min(w - core_x);
            // No synthetic padding past the real plane edge: every level clamps
            // there, exactly as one monolithic pass does.
            let x0 = core_x.saturating_sub(halo);
            let y0 = core_y.saturating_sub(halo);
            let x1 = (core_x + cw + halo).min(w);
            let y1 = (core_y + ch + halo).min(h);
            tiles.push(ResidentTile {
                x0,
                y0,
                w: x1 - x0,
                h: y1 - y0,
                crop_x: core_x - x0,
                crop_y: core_y - y0,
                core_x,
                core_y,
                core_w: cw,
                core_h: ch,
            });
        }
    }
    tiles
}

/// Pooled storage reused across [`DetailGpuRuntime::encode_resident`] frames.
#[derive(Default)]
pub struct DetailResidentBuffers {
    pool: Option<wgpu::Buffer>,
    uniform: Option<wgpu::Buffer>,
}

fn ensure_buffer(
    slot: &mut Option<wgpu::Buffer>,
    device: &wgpu::Device,
    label: &str,
    size: u64,
    usage: wgpu::BufferUsages,
) {
    if slot.as_ref().is_some_and(|b| b.size() >= size) {
        return;
    }
    // Grow in 1 MiB steps so a zoom/pan does not reallocate every frame.
    let size = size.div_ceil(1 << 20) * (1 << 20);
    *slot = Some(device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage,
        mapped_at_creation: false,
    }));
}

impl DetailGpuRuntime {
    /// Encode Detail over a GPU-resident plane: reads the `w×h` RGB of `src`
    /// (texel (0,0) = plane pixel (0,0)) and writes the result, packed
    /// `3·w·h` f32, into `out` from f32 element `out_base` on. No host upload
    /// or readback. `p = None` copies the plane through unchanged.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_resident(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        buffers: &mut DetailResidentBuffers,
        src: &wgpu::TextureView,
        out: &wgpu::Buffer,
        out_base: u32,
        w: u32,
        h: u32,
        p: Option<&DetailWorkingParams>,
        linear: bool,
        luma_coeff: [f32; 3],
    ) {
        self.encode_resident_with_budget(
            device,
            queue,
            encoder,
            buffers,
            src,
            out,
            out_base,
            w,
            h,
            p,
            linear,
            luma_coeff,
            max_plane_pixels(device),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn encode_resident_with_budget(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        buffers: &mut DetailResidentBuffers,
        src: &wgpu::TextureView,
        out: &wgpu::Buffer,
        out_base: u32,
        w: u32,
        h: u32,
        p: Option<&DetailWorkingParams>,
        linear: bool,
        luma_coeff: [f32; 3],
        max_plane: u64,
    ) {
        if w == 0 || h == 0 {
            return;
        }
        let dispatch_limit = device.limits().max_compute_workgroups_per_dimension.max(1);
        let mut passes: Vec<(usize, PassParams, u32, u32)> = Vec::new();
        let mut pool_floats = 0u64;
        for t in plan_resident_tiles(w, h, max_plane, DETAIL_HALO as u32) {
            let lay = Layout::new(t.w, t.h);
            pool_floats = pool_floats.max(lay.total as u64);
            let total_groups = lay.n.div_ceil(64);
            let groups_x = total_groups.min(dispatch_limit);
            let groups_y = total_groups.div_ceil(groups_x);
            let mut base = base_params(&lay, groups_x, linear, luma_coeff);
            base.tex_x0 = t.x0;
            base.tex_y0 = t.y0;
            base.crop_x = t.crop_x;
            base.crop_y = t.crop_y;
            base.core_w = t.core_w;
            base.core_h = t.core_h;
            base.out_x0 = t.core_x;
            base.out_y0 = t.core_y;
            base.out_w = w;
            base.out_base = out_base;
            passes.push((entry("load_tex"), base, groups_x, groups_y));
            if let Some(p) = p {
                passes.extend(
                    build_passes(&lay, base, &p.plan)
                        .into_iter()
                        .map(|(e, pp)| (e, pp, groups_x, groups_y)),
                );
            }
            passes.push((entry("store_out"), base, groups_x, groups_y));
        }

        ensure_buffer(
            &mut buffers.pool,
            device,
            "detail_resident_pool",
            pool_floats * 4,
            wgpu::BufferUsages::STORAGE,
        );
        ensure_buffer(
            &mut buffers.uniform,
            device,
            "detail_resident_uniform",
            passes.len() as u64 * STRIDE,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let (Some(pool), Some(uniform)) = (&buffers.pool, &buffers.uniform) else {
            return;
        };
        let mut bytes = vec![0u8; passes.len() * STRIDE as usize];
        for (i, (_, pp, _, _)) in passes.iter().enumerate() {
            let at = i * STRIDE as usize;
            bytes[at..at + std::mem::size_of::<PassParams>()]
                .copy_from_slice(bytemuck::bytes_of(pp));
        }
        queue.write_buffer(uniform, 0, &bytes);

        let bind_group = detail_bind_group(self, device, pool, uniform, src, out);
        let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("detail_resident_pass"),
            timestamp_writes: None,
        });
        for (i, (pi, _, groups_x, groups_y)) in passes.iter().enumerate() {
            cpass.set_pipeline(&self.pipelines[*pi]);
            cpass.set_bind_group(0, &bind_group, &[(i as u64 * STRIDE) as u32]);
            cpass.dispatch_workgroups(*groups_x, *groups_y, 1);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_detail_impl(
    runtime: &DetailGpuRuntime,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    rgb: &[f32],
    w: u32,
    h: u32,
    p: &DetailPlan,
    linear: bool,
    luma_coeff: [f32; 3],
) -> Vec<f32> {
    let lay = Layout::new(w, h);
    let n = lay.n as usize;
    assert_eq!(rgb.len(), 3 * n, "rgb must be 3*w*h");
    if n == 0 {
        return Vec::new();
    }
    let total_groups = lay.n.div_ceil(64);
    let dispatch_limit = device.limits().max_compute_workgroups_per_dimension.max(1);
    let groups_x = total_groups.min(dispatch_limit);
    let groups_y = total_groups.div_ceil(groups_x);
    assert!(
        groups_y <= dispatch_limit,
        "detail plane exceeds the device's 2-D dispatch capacity"
    );

    // Every scratch plane is fully written before its first read. Allocate the
    // pooled storage uninitialised and upload only the RGB image (3·N).
    let pool = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("detail_pool"),
        size: lay.total as u64 * std::mem::size_of::<f32>() as u64,
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_SRC
            | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(
        &pool,
        lay.img as u64 * std::mem::size_of::<f32>() as u64,
        bytemuck::cast_slice(rgb),
    );

    let base = base_params(&lay, groups_x, linear, luma_coeff);
    let passes = build_passes(&lay, base, p);

    // Uniform buffer: one 256-byte-strided PassParams per pass.
    let uniform = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("detail_uniform"),
        size: passes.len() as u64 * STRIDE,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    for (i, (_, pp)) in passes.iter().enumerate() {
        queue.write_buffer(&uniform, i as u64 * STRIDE, bytemuck::bytes_of(pp));
    }

    let bind_group = detail_bind_group(
        runtime,
        device,
        &pool,
        &uniform,
        &runtime.dummy_src_view,
        &runtime.dummy_out,
    );
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("detail_encoder"),
    });
    {
        let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("detail_pass"),
            timestamp_writes: None,
        });
        for (i, (pi, _)) in passes.iter().enumerate() {
            cpass.set_pipeline(&runtime.pipelines[*pi]);
            cpass.set_bind_group(0, &bind_group, &[(i as u64 * STRIDE) as u32]);
            cpass.dispatch_workgroups(groups_x, groups_y, 1);
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

// Large enough to use the adapter's storage-binding budget efficiently while
// keeping one transient host/pool allocation bounded.
const PREFERRED_TILE_CORE: u32 = 2048;

/// Full-resolution entry point used by live preview. The source is split into
/// apron'd tiles so the pooled working storage remains bounded even for a
/// 24–60 MP photograph. The apron covers the widest dependency chain of the
/// Detail core, so cropping it after each run matches one monolithic pass
/// without seams.
#[allow(clippy::too_many_arguments)]
pub fn run_detail_tiled_with_runtime(
    runtime: &DetailGpuRuntime,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    rgb: &[f32],
    w: u32,
    h: u32,
    p: &DetailWorkingParams,
    linear: bool,
    luma_coeff: [f32; 3],
) -> Vec<f32> {
    let max_plane_edge = (max_plane_pixels(device) as f64).sqrt().floor() as u32;
    let halo = DETAIL_HALO as u32;
    let core_edge = PREFERRED_TILE_CORE.min(max_plane_edge.saturating_sub(2 * halo).max(1));
    run_detail_tiled_with_core(
        runtime, device, queue, rgb, w, h, p, linear, luma_coeff, core_edge,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_detail_tiled_with_core(
    runtime: &DetailGpuRuntime,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    rgb: &[f32],
    w: u32,
    h: u32,
    p: &DetailWorkingParams,
    linear: bool,
    luma_coeff: [f32; 3],
    core_edge: u32,
) -> Vec<f32> {
    let n = (w as usize).saturating_mul(h as usize);
    assert_eq!(rgb.len(), 3 * n, "rgb must be 3*w*h");
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let halo = DETAIL_HALO as u32;
    let core_edge = core_edge.max(1);
    let mut out = vec![0.0f32; 3 * n];
    for core_y in (0..h).step_by(core_edge as usize) {
        let core_h = core_edge.min(h - core_y);
        for core_x in (0..w).step_by(core_edge as usize) {
            let core_w = core_edge.min(w - core_x);
            // Do not synthesize padding outside the full image: every à-trous
            // level clamps its intermediate plane at the real image edge, and
            // pre-extending source pixels would not be mathematically equal.
            let source_x0 = core_x.saturating_sub(halo);
            let source_y0 = core_y.saturating_sub(halo);
            let source_x1 = (core_x + core_w + halo).min(w);
            let source_y1 = (core_y + core_h + halo).min(h);
            let tile_w = source_x1 - source_x0;
            let tile_h = source_y1 - source_y0;
            let crop_x = core_x - source_x0;
            let crop_y = core_y - source_y0;
            let mut tile = vec![0.0f32; 3 * (tile_w * tile_h) as usize];
            for tile_y in 0..tile_h {
                let source_i = 3 * ((source_y0 + tile_y) * w + source_x0) as usize;
                let tile_i = 3 * (tile_y * tile_w) as usize;
                let len = 3 * tile_w as usize;
                tile[tile_i..tile_i + len].copy_from_slice(&rgb[source_i..source_i + len]);
            }

            let detailed = runtime.run(device, queue, &tile, tile_w, tile_h, p, linear, luma_coeff);
            for y in 0..core_h {
                let source_row = 3 * ((y + crop_y) * tile_w + crop_x) as usize;
                let dest_row = 3 * ((core_y + y) * w + core_x) as usize;
                let row_len = 3 * core_w as usize;
                out[dest_row..dest_row + row_len]
                    .copy_from_slice(&detailed[source_row..source_row + row_len]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Multiple WGPU adapters/devices mapping readbacks concurrently is flaky on
    // some Windows drivers. The production path is single-device; serialize
    // these headless parity probes so `cargo test --lib` is deterministic.
    static GPU_DETAIL_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn hash(i: usize, s: u32) -> f32 {
        let mut x = (i as u32)
            .wrapping_mul(2_654_435_761)
            .wrapping_add(s)
            .wrapping_add(2_463_534_242);
        x ^= x >> 15;
        x = x.wrapping_mul(2_246_822_519);
        x ^= x >> 13;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    /// Edges, texture and luma/chroma noise so every stage engages.
    fn test_image(w: u32, h: u32, linear: bool) -> Vec<f32> {
        let mut rgb = vec![0f32; (3 * w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) as usize;
                let step = if x > w / 2 { 0.62 } else { 0.22 };
                let tex = 0.02 * ((x as f32) * 2.1).sin();
                let base = step + tex + 0.03 * hash(i, 7);
                let px = [base + 0.03 * hash(i, 11), base, base + 0.025 * hash(i, 13)];
                for c in 0..3 {
                    rgb[i * 3 + c] = if linear {
                        (px[c].max(0.0)).powf(2.2) * 1.4
                    } else {
                        px[c].clamp(0.0, 1.0)
                    };
                }
            }
        }
        rgb
    }

    fn cases() -> Vec<DevelopSettings> {
        let base = DevelopSettings::default();
        vec![
            DevelopSettings {
                sharpening: 70.0,
                noise_reduction: 40.0,
                color_noise_reduction: 60.0,
                ..base.clone()
            },
            DevelopSettings {
                sharpening: 120.0,
                sharpen_radius: 2.2,
                sharpen_detail: 10.0,
                sharpen_masking: 30.0,
                noise_reduction: 20.0,
                noise_reduction_detail: 80.0,
                noise_reduction_contrast: 40.0,
                ..base.clone()
            },
            DevelopSettings {
                sharpening: 40.0,
                sharpen_detail: 80.0,
                color_noise_reduction: 25.0,
                color_noise_detail: 20.0,
                color_noise_smoothness: 90.0,
                ..base
            },
        ]
    }

    fn max_diff(cpu: &[[f32; 3]], gpu: &[f32]) -> f32 {
        cpu.iter()
            .enumerate()
            .flat_map(|(i, p)| (0..3).map(move |c| (p[c] - gpu[i * 3 + c]).abs()))
            .fold(0.0, f32::max)
    }

    /// Display domain: GPU must match `apply_detail_to_display_buffer`.
    #[test]
    fn gpu_detail_matches_cpu_display() {
        let _guard = GPU_DETAIL_TEST_LOCK.lock().expect("GPU Detail test lock");
        let Some((device, queue)) = crate::gpu::vector::renderer::headless_device() else {
            eprintln!("no headless GPU adapter; skipped");
            return;
        };
        let (w, h) = (48u32, 40u32);
        let rgb = test_image(w, h, false);
        for settings in cases() {
            let mut cpu: Vec<[f32; 3]> = rgb.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
            crate::core::develop::apply_detail_to_display_buffer(
                &mut cpu, w as usize, h as usize, &settings, 1,
            );
            let params = DetailWorkingParams::from_settings(&settings);
            let gpu = run_detail_display(&device, &queue, &rgb, w, h, &params);
            let d = max_diff(&cpu, &gpu);
            println!("GPU vs CPU display Detail max abs diff = {d:.7}");
            assert!(d < 2e-4, "GPU Detail diverges from CPU: {d}");
        }
    }

    /// Linear/scene domain (RAW): GPU must match
    /// `apply_detail_to_working_buffer_in_space` in the working colour space.
    #[test]
    fn gpu_detail_matches_cpu_linear_scene() {
        let _guard = GPU_DETAIL_TEST_LOCK.lock().expect("GPU Detail test lock");
        let Some((device, queue)) = crate::gpu::vector::renderer::headless_device() else {
            eprintln!("no headless GPU adapter; skipped");
            return;
        };
        use crate::core::working_color::WorkingColorSpace;
        let space = WorkingColorSpace::AcesCg;
        let coeff = space.render_luminance_coefficients();
        let (w, h) = (48u32, 40u32);
        let rgb = test_image(w, h, true);
        for settings in cases() {
            let mut cpu: Vec<[f32; 3]> = rgb.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
            crate::core::develop::apply_detail_to_working_buffer_in_space(
                &mut cpu, w as usize, h as usize, &settings, space, 1,
            );
            let params = DetailWorkingParams::from_settings(&settings);
            let gpu = run_detail(&device, &queue, &rgb, w, h, &params, true, coeff);
            let d = max_diff(&cpu, &gpu);
            println!("GPU vs CPU linear Detail max abs diff = {d:.7}");
            assert!(d < 5e-4, "GPU linear Detail diverges: {d}");
        }
    }

    #[test]
    fn gpu_detail_tiled_matches_monolithic_without_seams() {
        let _guard = GPU_DETAIL_TEST_LOCK.lock().expect("GPU Detail test lock");
        let Some((device, queue)) = crate::gpu::vector::renderer::headless_device() else {
            eprintln!("no headless GPU adapter; skipped");
            return;
        };
        let (w, h) = (230u32, 190u32);
        let rgb = test_image(w, h, false);
        let settings = &cases()[0];
        let params = DetailWorkingParams::from_settings(settings);
        let runtime = DetailGpuRuntime::new(&device);
        let coeff = [0.2126, 0.7152, 0.0722];
        let whole = runtime.run(&device, &queue, &rgb, w, h, &params, false, coeff);
        // A small core forces seams through smooth and edge regions; the apron
        // must make every cropped pixel identical.
        let tiled = run_detail_tiled_with_core(
            &runtime, &device, &queue, &rgb, w, h, &params, false, coeff, 60,
        );
        let d = whole
            .iter()
            .zip(&tiled)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        println!("GPU tiled vs monolithic Detail max abs diff = {d:.8}");
        assert!(d < 1e-6, "tiled GPU Detail has a seam: {d}");
    }

    /// Upload `rgb` as the resident input texture, run the resident path and
    /// read the packed output back (test-only round trip).
    #[allow(clippy::too_many_arguments)]
    fn run_resident(
        runtime: &DetailGpuRuntime,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rgb: &[f32],
        w: u32,
        h: u32,
        p: &DetailWorkingParams,
        linear: bool,
        coeff: [f32; 3],
        budget: Option<u64>,
    ) -> Vec<f32> {
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
            format: RESIDENT_SRC_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let row = w as usize * 16;
        let padded = row.div_ceil(256) * 256;
        let mut bytes = vec![0u8; padded * h as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let i = y * w as usize + x;
                let px = [rgb[i * 3], rgb[i * 3 + 1], rgb[i * 3 + 2], 1.0f32];
                bytes[y * padded + x * 16..y * padded + x * 16 + 16]
                    .copy_from_slice(bytemuck::cast_slice(&px));
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
        let size = (3 * w * h) as u64 * 4;
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
        let mut buffers = DetailResidentBuffers::default();
        let mut encoder = device.create_command_encoder(&Default::default());
        runtime.encode_resident_with_budget(
            device,
            queue,
            &mut encoder,
            &mut buffers,
            &view,
            &out,
            0,
            w,
            h,
            Some(p),
            linear,
            coeff,
            budget.unwrap_or_else(|| max_plane_pixels(device)),
        );
        encoder.copy_buffer_to_buffer(&out, 0, &readback, 0, size);
        queue.submit([encoder.finish()]);
        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::wait_indefinitely()).ok();
        let data: Vec<f32> = bytemuck::cast_slice(&slice.get_mapped_range()).to_vec();
        readback.unmap();
        data
    }

    #[test]
    fn resident_tiles_cover_the_plane_once() {
        for (w, h, budget) in [
            (230, 190, 1u64 << 30),
            (230, 600, 60_000),
            (230, 190, 30_000),
            (500, 90, 40_000),
        ] {
            let tiles = plan_resident_tiles(w, h, budget, DETAIL_HALO as u32);
            let mut hits = vec![0u8; (w * h) as usize];
            for t in &tiles {
                assert!((t.w as u64) * (t.h as u64) <= budget.max((w * h) as u64));
                for y in t.core_y..t.core_y + t.core_h {
                    for x in t.core_x..t.core_x + t.core_w {
                        hits[(y * w + x) as usize] += 1;
                    }
                }
                assert_eq!(t.core_x - t.x0, t.crop_x);
                assert_eq!(t.core_y - t.y0, t.crop_y);
            }
            assert!(hits.iter().all(|&n| n == 1), "{w}x{h} budget {budget}");
        }
    }

    /// The resident (texture in, buffer out) path must equal the CPU Detail in
    /// both domains and at a reduced preview scale, and its tiling must be
    /// seamless — strips and square tiles alike.
    #[test]
    fn gpu_detail_resident_matches_cpu_without_seams() {
        let _guard = GPU_DETAIL_TEST_LOCK.lock().expect("GPU Detail test lock");
        let Some((device, queue)) = crate::gpu::vector::renderer::headless_device() else {
            eprintln!("no headless GPU adapter; skipped");
            return;
        };
        let runtime = DetailGpuRuntime::new(&device);
        // Tall enough that a 60k-pixel budget plans full-width strips while a
        // 30k one falls back to square tiles.
        let (w, h) = (230u32, 600u32);
        let display = [0.2126, 0.7152, 0.0722];
        for scale in [1u32, 3] {
            let rgb = test_image(w, h, false);
            let settings = &cases()[0];
            let params = DetailWorkingParams::from_settings_scaled(settings, scale);
            let mut cpu: Vec<[f32; 3]> = rgb.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
            crate::core::develop::apply_detail_to_display_buffer(
                &mut cpu, w as usize, h as usize, settings, scale,
            );
            let whole = run_resident(
                &runtime, &device, &queue, &rgb, w, h, &params, false, display, None,
            );
            let d = max_diff(&cpu, &whole);
            println!("resident vs CPU display Detail (scale {scale}) max abs diff = {d:.7}");
            assert!(d < 2e-4, "resident GPU Detail diverges from CPU: {d}");
            for budget in [60_000u64, 30_000] {
                let tiled = run_resident(
                    &runtime,
                    &device,
                    &queue,
                    &rgb,
                    w,
                    h,
                    &params,
                    false,
                    display,
                    Some(budget),
                );
                let seam = whole
                    .iter()
                    .zip(&tiled)
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0f32, f32::max);
                println!("resident tiled ({budget} px) vs whole max abs diff = {seam:.8}");
                assert!(seam < 1e-6, "resident tiling has a seam: {seam}");
            }
        }

        use crate::core::working_color::WorkingColorSpace;
        let space = WorkingColorSpace::AcesCg;
        let coeff = space.render_luminance_coefficients();
        let rgb = test_image(w, h, true);
        let settings = &cases()[1];
        let params = DetailWorkingParams::from_settings(settings);
        let mut cpu: Vec<[f32; 3]> = rgb.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
        crate::core::develop::apply_detail_to_working_buffer_in_space(
            &mut cpu, w as usize, h as usize, settings, space, 1,
        );
        let gpu = run_resident(
            &runtime,
            &device,
            &queue,
            &rgb,
            w,
            h,
            &params,
            true,
            coeff,
            Some(50_000),
        );
        let d = max_diff(&cpu, &gpu);
        println!("resident vs CPU linear Detail max abs diff = {d:.7}");
        assert!(d < 5e-4, "resident linear GPU Detail diverges: {d}");
    }
}

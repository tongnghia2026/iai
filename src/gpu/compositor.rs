use crate::core::layer::{BlendMode, Layer, LayerStack};
use crate::core::tile::{TileMap, TilePos};
use crate::gpu::tile_atlas::TileAtlas;
use bytemuck::{Pod, Zeroable};

/// Deepest LOD proxy level built for a zoomed-out layer (`S = 2^level`). Level 8
/// reduces by 256×, enough to render a ~12k-px layer at zoom ≈ 0.004.
const PROXY_MAX_LEVEL: usize = 8;

/// Soft cap on total cached LOD-proxy pixel data. Beyond it the least-recently-
/// used proxy levels (never the one built this frame) are evicted. Sized to hold
/// a large single layer's proxy comfortably while bounding gigapixel / many-layer
/// growth — the whole point of building only the target level (not the chain).
const PROXY_CACHE_BYTES_CAP: usize = 256 * 1024 * 1024;

/// One cached LOD proxy: a layer (and its enabled mask) box-reduced by `2^level`,
/// keyed in `layer_proxies` by `(layer_id, level)`. Only the level actually
/// rendered is retained — the intermediate halvings used to build it are dropped
/// — and it is evicted by LRU under the byte cap, on a fingerprint change, or
/// when the layer leaves the visible set.
struct ProxyEntry {
    src_fp: u64,
    mask_fp: u64,
    tiles: TileMap,
    mask: Option<TileMap>,
    bytes: usize,
    last_used: u64,
}

/// An in-flight background proxy build. The downsample runs on a worker thread
/// (it is the expensive part — one full base read) while the layer keeps
/// rendering full-res; the result arrives over `rx`. `src_fp`/`mask_fp` pin the
/// content the build was started from, so a build finishing after the layer was
/// edited is detected as stale and discarded.
struct ProxyBuild {
    src_fp: u64,
    mask_fp: u64,
    rx: std::sync::mpsc::Receiver<(TileMap, Option<TileMap>)>,
}

/// Outcome of polling a pending [`ProxyBuild`] (kept separate so the immutable
/// borrow of the builds map ends before the map is mutated).
enum BuildPoll {
    Ready((TileMap, Option<TileMap>)),
    Building,
    /// Worker finished-and-gone (panicked) or built stale content → respawn.
    Respawn,
}

/// The per-frame decision to composite one layer from its LOD proxy instead of
/// its full-resolution tiles (built in a pre-pass, consumed in the layer loop).
/// Holds cloned `TileMap`s (cheap — `Arc` clones of a few hundred proxy tiles).
struct ProxyRender {
    level: u32,
    tiles: TileMap,
    mask: Option<TileMap>,
    pw: u32,
    ph: u32,
}

/// The compositor WGSL. Lives in `compositor.wgsl` so shader edits diff as
/// shader edits; embedded at compile time, and still validated by naga in
/// this module's tests plus the CPU/GPU parity tests in core.
pub const COMPOSITOR_SHADER: &str = include_str!("compositor.wgsl");

pub const ADJUSTMENT_SHADER: &str = r#"
struct CompositorUniforms {
    opacity:        f32,
    blend_mode:     u32,
    offset_x:       f32,
    offset_y:       f32,
    zoom:           f32,
    view_offset_x:  f32,
    view_offset_y:  f32,
    viewport_w:     f32,
    viewport_h:     f32,
    layer_tiles_w:  u32,
    layer_tiles_h:  u32,
    layer_w:        f32,
    layer_h:        f32,
    xform_active:   u32,
    xform_inv_a:    f32,
    xform_inv_b:    f32,
    xform_inv_c:    f32,
    xform_inv_d:    f32,
    xform_pivot_x:  f32,
    xform_pivot_y:  f32,
    xform_tx:       f32,
    xform_ty:       f32,
    xform_orig_ox:  f32,
    xform_orig_oy:  f32,
    xform_orig_w:   f32,
    xform_orig_h:   f32,
    mask_enabled:   u32,
    mask_inverted:  u32,
    adj_kind:       u32,
    adj_pad_a:      u32,
    adj_pad_b:      u32,
    adj_pad_c:      u32,
    adj_p:          array<vec4<f32>, 3>,
    lut:            array<vec4<f32>, 192>,
};

@group(0) @binding(0) var atlas_tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(1) @binding(0) var dst_tex: texture_2d<f32>;
@group(2) @binding(0) var<uniform> u: CompositorUniforms;
@group(2) @binding(1) var<storage, read> tile_map: array<i32>;
@group(2) @binding(2) var<storage, read> mask_tile_map: array<i32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var out: VsOut;
    let x = f32((vi << 1u) & 2u);
    let y = f32(vi & 2u);
    out.uv = vec2<f32>(x, y);
    out.pos = vec4<f32>(x * 2.0 - 1.0, y * -2.0 + 1.0, 0.0, 1.0);
    return out;
}

fn lum299(c: vec3<f32>) -> f32 { return dot(c, vec3(0.299, 0.587, 0.114)); }

fn rgb_to_hsl(c: vec3<f32>) -> vec3<f32> {
    let mx = max(max(c.r, c.g), c.b);
    let mn = min(min(c.r, c.g), c.b);
    let l  = (mx + mn) * 0.5;
    if (mx - mn < 0.0001) { return vec3(0.0, 0.0, l); }
    let d  = mx - mn;
    let s  = select(d / (2.0 - mx - mn), d / (mx + mn), l < 0.5);
    var h: f32;
    if (mx == c.r)      { h = (c.g - c.b) / d + select(6.0, 0.0, c.g >= c.b); }
    else if (mx == c.g) { h = (c.b - c.r) / d + 2.0; }
    else                { h = (c.r - c.g) / d + 4.0; }
    return vec3(h / 6.0, s, l);
}

fn hue_to_rgb(p: f32, q: f32, t_in: f32) -> f32 {
    var t = t_in;
    if (t < 0.0) { t += 1.0; }
    if (t > 1.0) { t -= 1.0; }
    if (t < 1.0/6.0) { return p + (q - p) * 6.0 * t; }
    if (t < 0.5)     { return q; }
    if (t < 2.0/3.0) { return p + (q - p) * (2.0/3.0 - t) * 6.0; }
    return p;
}

fn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {
    let h = hsl.x; let s = hsl.y; let l = hsl.z;
    if (s < 0.0001) { return vec3(l); }
    let q = select(l + s - l * s, l * (1.0 + s), l < 0.5);
    let p = 2.0 * l - q;
    return vec3(hue_to_rgb(p, q, h + 1.0/3.0),
                hue_to_rgb(p, q, h),
                hue_to_rgb(p, q, h - 1.0/3.0));
}

fn adj_chroma(c: vec3<f32>) -> f32 {
    return max(max(c.r, c.g), c.b) - min(min(c.r, c.g), c.b);
}

fn adj_smootherstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = clamp((x - edge0) / max(edge1 - edge0, 0.00001), 0.0, 1.0);
    return t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
}

fn adj_set_rgb_preserving_luma(candidate: vec3<f32>, target_luma_in: f32) -> vec3<f32> {
    let target_luma = clamp(target_luma_in, 0.0, 1.0);
    var c = candidate;
    let mn = min(min(c.r, c.g), c.b);
    if (mn < 0.0) {
        let anchor = max(target_luma, 0.035);
        let scale = clamp(anchor / max(anchor - mn, 0.00001), 0.0, 1.0);
        c = vec3(target_luma) + (c - vec3(target_luma)) * scale;
    }
    let mx = max(max(c.r, c.g), c.b);
    if (mx > 1.0) {
        let scale = clamp((1.0 - target_luma) / max(mx - target_luma, 0.00001), 0.0, 1.0);
        c = vec3(target_luma) + (c - vec3(target_luma)) * scale;
    }
    return clamp(c, vec3(0.0), vec3(1.0));
}

fn adj_chroma_headroom(d: vec3<f32>, l: f32) -> f32 {
    var room = 1e9;
    if (d.r > 1e-6) { room = min(room, (1.0 - l) / d.r - 1.0); } else if (d.r < -1e-6) { room = min(room, l / -d.r - 1.0); }
    if (d.g > 1e-6) { room = min(room, (1.0 - l) / d.g - 1.0); } else if (d.g < -1e-6) { room = min(room, l / -d.g - 1.0); }
    if (d.b > 1e-6) { room = min(room, (1.0 - l) / d.b - 1.0); } else if (d.b < -1e-6) { room = min(room, l / -d.b - 1.0); }
    return max(room, 0.0);
}

// Gamut-aware soft-knee saturation, mirror of CPU core::color::saturate_around_luma
// (and the camera-raw dev_scale_chroma_around_luma) so the Hue/Saturation preview
// matches the bake and never hard-clips into blocky over-saturation.
fn adj_scale_chroma_around_luma(c: vec3<f32>, factor_in: f32) -> vec3<f32> {
    let l = clamp(lum299(c), 0.0, 1.0);
    let protect = adj_smootherstep(0.035, 0.14, l) * (1.0 - adj_smootherstep(0.90, 0.99, l));
    let req = (clamp(factor_in, 0.0, 2.35) - 1.0) * protect;
    let d = c - vec3(l);
    var f = 1.0 + req;
    if (req > 0.0) {
        let room = adj_chroma_headroom(d, l);
        f = select(1.0, 1.0 + room * tanh(req / room), room > 1e-4);
    }
    return adj_set_rgb_preserving_luma(vec3(l) + d * f, l);
}

// The mask tile lives in the sRGB atlas, so the sampled value is linearized;
// invert with the exact sRGB transfer curve (not pow 1/2.2) to recover the
// stored byte and match the CPU, which reads the mask byte directly.
fn mask_value_from_sample(c: vec4<f32>) -> f32 {
    let l = clamp(c.r, 0.0, 1.0);
    if (l <= 0.0031308) {
        return l * 12.92;
    }
    return 1.055 * pow(l, 1.0 / 2.4) - 0.055;
}

// Clamp-to-edge, mirroring LayerMask::sample on the CPU: out-of-bounds
// coordinates take the nearest edge texel instead of hard-revealing.
fn sample_mask_nearest_i(lx: i32, ly: i32) -> f32 {
    if (u.mask_enabled == 0u) { return 1.0; }
    let ux = u32(clamp(lx, 0, max(i32(u.layer_w) - 1, 0)));
    let uy = u32(clamp(ly, 0, max(i32(u.layer_h) - 1, 0)));
    let tx = ux / 256u;
    let ty = uy / 256u;
    if (tx >= u.layer_tiles_w || ty >= u.layer_tiles_h) { return 1.0; }
    let tile_idx = ty * u.layer_tiles_w + tx;
    let slot = mask_tile_map[tile_idx];
    var value: f32 = 0.0;
    if (slot >= 0) {
        let slot_x = u32(slot & 0xFFFF);
        let slot_y = u32(slot >> 16u);
        let atlas_x = f32(slot_x * 256u + ux % 256u) + 0.5;
        let atlas_y = f32(slot_y * 256u + uy % 256u) + 0.5;
        let atlas_dim = vec2<f32>(textureDimensions(atlas_tex));
        value = mask_value_from_sample(textureSample(atlas_tex, samp, vec2<f32>(atlas_x / atlas_dim.x, atlas_y / atlas_dim.y)));
    }
    if (u.mask_inverted == 1u) { return 1.0 - value; }
    return value;
}

fn apply_adjustment(c_in: vec3<f32>) -> vec3<f32> {
    let c = clamp(c_in, vec3(0.0), vec3(1.0));
    let P0 = u.adj_p[0];
    let P1 = u.adj_p[1];
    let P2 = u.adj_p[2];
    switch u.adj_kind {
        case 1u: { // BrightnessContrast
            let bv = P0.x / 255.0;
            let cv = (P0.y + 100.0) / 100.0;
            return clamp((c + vec3(bv) - vec3(0.5)) * cv + vec3(0.5), vec3(0.0), vec3(1.0));
        }
        case 2u: { // HueSaturation
            var hsl = rgb_to_hsl(c);
            let nh = fract(hsl.x + P0.x / 360.0 + 1.0);
            let nl = clamp(hsl.z + P0.z / 100.0, 0.0, 1.0);
            var adjusted = hsl_to_rgb(vec3(nh, hsl.y, nl));
            let sf = P0.y / 100.0;
            if (abs(sf) > 0.001) {
                let chroma_gate = adj_smootherstep(0.035, 0.20, adj_chroma(adjusted));
                let factor = select(1.0 + sf, 1.0 + sf * 1.35 * chroma_gate, sf >= 0.0);
                adjusted = adj_scale_chroma_around_luma(adjusted, factor);
            }
            return adjusted;
        }
        case 3u: { // Levels
            let ib = P0.x / 255.0;
            let iw = P0.y / 255.0;
            let gamma = max(P0.z, 0.01);
            let ob = P0.w / 255.0;
            let ow = P1.x / 255.0;
            var v = clamp((c - vec3(ib)) / max(iw - ib, 0.001), vec3(0.0), vec3(1.0));
            v = pow(v, vec3(1.0 / gamma));
            return clamp(vec3(ob) + v * (ow - ob), vec3(0.0), vec3(1.0));
        }
        case 13u: { // Per-channel Levels/Curves: three 256-entry LUTs (R@0, G@256, B@512),
                    // each already composed as master(channel(v)) on the CPU bake.
            let ri = u32(c.r * 255.0 + 0.5);
            let gi = u32(c.g * 255.0 + 0.5) + 256u;
            let bi = u32(c.b * 255.0 + 0.5) + 512u;
            return vec3<f32>(
                u.lut[ri >> 2u][ri & 3u],
                u.lut[gi >> 2u][gi & 3u],
                u.lut[bi >> 2u][bi & 3u],
            );
        }
        case 5u: { // ColorBalance
            let orig_lum = lum299(c);
            let lw = orig_lum;
            let sw = max(1.0 - 2.0 * lw, 0.0);
            let hw = max(2.0 * lw - 1.0, 0.0);
            let mw = 1.0 - sw - hw;
            let shadows = P0.xyz;
            let midtones = P1.xyz;
            let highlights = P2.xyz;
            var nc = clamp(c + sw * shadows / 100.0 + mw * midtones / 100.0 + hw * highlights / 100.0, vec3(0.0), vec3(1.0));
            if (P0.w > 0.5) {
                let new_lum = lum299(nc);
                if (new_lum > 0.001) { nc = clamp(nc * (orig_lum / new_lum), vec3(0.0), vec3(1.0)); }
            }
            return nc;
        }
        case 6u: { // Vibrance
            let hsl = rgb_to_hsl(c);
            let boost = P0.x / 100.0 * (1.0 - hsl.y);
            let ns = clamp(hsl.y + boost + P0.y / 100.0, 0.0, 1.0);
            return hsl_to_rgb(vec3(hsl.x, ns, hsl.z));
        }
        case 7u: { // Exposure
            var v = c * pow(2.0, P0.x);
            v = v + vec3(P0.y);
            return clamp(pow(max(v, vec3(0.0)), vec3(1.0 / max(P0.z, 0.01))), vec3(0.0), vec3(1.0));
        }
        case 8u: { // Invert
            return vec3(1.0) - c;
        }
        case 9u: { // Threshold
            let v = select(0.0, 1.0, lum299(c) >= P0.x / 255.0);
            return vec3(v);
        }
        case 10u: { // Posterize
            let lv = max(P0.x, 2.0);
            return floor(c * lv) / (lv - 1.0);
        }
        case 11u: { // Desaturate
            return vec3(lum299(c));
        }
        case 12u: { // GradientMap: 64 RGB ramp samples packed in u.lut
            var t = lum299(c);
            if (P0.x > 0.5) {
                t = 1.0 - t;
            }
            if (P0.y > 0.5) {
                let h = fract(sin(dot(c, vec3(12.9898, 78.233, 37.719))) * 43758.5453);
                t = clamp(t + (h - 0.5) * (1.0 / 48.0), 0.0, 1.0);
            }
            let x = clamp(t, 0.0, 1.0) * 63.0;
            let i0 = u32(floor(x));
            let i1 = min(i0 + 1u, 63u);
            return mix(u.lut[i0].xyz, u.lut[i1].xyz, fract(x));
        }
        default: { return c; }
    }
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let dst = textureSample(dst_tex, samp, in.uv);

    let screen_x = in.uv.x * u.viewport_w;
    let screen_y = in.uv.y * u.viewport_h;
    let canvas_x = screen_x / u.zoom + u.view_offset_x;
    let canvas_y = screen_y / u.zoom + u.view_offset_y;

    let layer_x = canvas_x - u.offset_x;
    let layer_y = canvas_y - u.offset_y;
    if (layer_x < 0.0 || layer_y < 0.0 || layer_x >= u.layer_w || layer_y >= u.layer_h) {
        return dst;
    }

    let mask_a = sample_mask_nearest_i(i32(layer_x), i32(layer_y));
    let eff = u.opacity * mask_a;
    if (eff <= 0.0) { return dst; }

    // Keep adjustment math aligned with the CPU gamma-space apply path.
    let g = pow(clamp(dst.rgb, vec3(0.0), vec3(1.0)), vec3(1.0 / 2.2));
    let adj = apply_adjustment(g);
    let adj_lin = pow(clamp(adj, vec3(0.0), vec3(1.0)), vec3(2.2));
    let out_rgb = mix(dst.rgb, adj_lin, eff);
    return vec4<f32>(out_rgb, dst.a);
}
"#;

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct CompositorUniformsData {
    pub opacity: f32,
    pub blend_mode: u32,
    pub offset_x: f32,
    pub offset_y: f32,
    pub zoom: f32,
    pub view_offset_x: f32,
    pub view_offset_y: f32,
    pub viewport_w: f32,
    pub viewport_h: f32,
    pub layer_tiles_w: u32,
    pub layer_tiles_h: u32,
    pub layer_w: f32,
    pub layer_h: f32,
    pub xform_active: u32,
    pub xform_inv_a: f32,
    pub xform_inv_b: f32,
    pub xform_inv_c: f32,
    pub xform_inv_d: f32,
    pub xform_pivot_x: f32,
    pub xform_pivot_y: f32,
    pub xform_tx: f32,
    pub xform_ty: f32,
    pub xform_orig_ox: f32,
    pub xform_orig_oy: f32,
    pub xform_orig_w: f32,
    pub xform_orig_h: f32,
    pub mask_enabled: u32,
    pub mask_inverted: u32,
    pub adj_kind: u32,
    pub _adj_pad_a: u32,
    /// Live PowerClip / clipping-mask pin. Two i16 packed `(dx << 16) | dy`
    /// (masking off sign bits) = owning layer's `offset − mask.bake_offset`.
    /// The main compositor adds it to the mask sample coord so a clipped child
    /// stays pinned to its frame while dragged, with no per-frame mask re-bake.
    /// Zero for every non-clip layer (default, byte-identical to before).
    pub clip_shift_packed: u32,
    pub _adj_pad_c: u32,
    /// Three vec4 values = 12 floats. Mapping depends on `adjustment_to_gpu`.
    pub adj_p: [f32; 12],
    /// Gamma-space LUT bank, byte-equivalent to WGSL `array<vec4<f32>,192>`.
    /// Kind 13 (per-channel Levels/Curves) uses all three 256-entry planes
    /// (R at 0, G at 256, B at 512). Gradient Map and the Develop preview
    /// param buffer only use the first 256 slots.
    /// Offset 176, 16-byte aligned; total struct size is 3248 bytes.
    pub adj_lut: [f32; 768],
}

const _: () = assert!(std::mem::size_of::<CompositorUniformsData>() == 3248);
const _: () = assert!(std::mem::offset_of!(CompositorUniformsData, adj_lut) == 176);

/// Pack a clipping-mask / PowerClip child's live pin into the single spare
/// uniform slot. The compositor samples the (unmoved) clip mask at a canvas-fixed
/// coordinate `layer_local + (dx, dy)`, so a clipped image stays pinned to its
/// frame while it is moved OR free-transformed, with no per-frame re-bake.
///
/// The same 32 bits also carry the "this layer IS a clip child" flag: each i16
/// delta is stored biased by `0x8000`, so both halves are always ≥ 1 and the word
/// is never zero for a clip child. A value of exactly 0 therefore means "not a
/// clip child → no pin", which the shader fast-paths. Deltas clamp to ±32767 px
/// (in the current composite's pixel scale — full-res, or downscaled by the LOD
/// proxy level).
fn pack_clip_shift(dx: i32, dy: i32) -> u32 {
    let dx = (dx.clamp(-32767, 32767) + 0x8000) as u32;
    let dy = (dy.clamp(-32767, 32767) + 0x8000) as u32;
    (dx << 16) | (dy & 0xFFFF)
}

/// Converts `AdjustmentType` into shader kind, parameters, and LUT data.
/// kind=0 means the adjustment is CPU-only for flattening. Kind 13 uses the LUT
/// as three 256-sample channel planes (R@0, G@256, B@512), each pre-composed
/// as master(channel(v)); Gradient Map uses slots 0..256 as 64 packed RGB ramp
/// samples.
fn adjustment_to_gpu(adj: &crate::core::layer::AdjustmentType) -> (u32, [f32; 12], [f32; 768]) {
    use crate::core::layer::AdjustmentType as A;
    let mut p = [0.0f32; 12];
    let mut lut = [0.0f32; 768];
    let kind = match adj {
        A::BrightnessContrast {
            brightness,
            contrast,
        } => {
            p[0] = *brightness;
            p[1] = *contrast;
            1
        }
        A::HueSaturation {
            hue,
            saturation,
            lightness,
        } => {
            p[0] = *hue;
            p[1] = *saturation;
            p[2] = *lightness;
            2
        }
        A::Levels { channels } => {
            if channels[1..].iter().all(|ch| ch.is_identity()) {
                // Master-only: the analytic kind stays exact (no 8-bit LUT
                // quantization in the preview).
                p[0] = channels[0].in_black as f32;
                p[1] = channels[0].in_white as f32;
                p[2] = channels[0].gamma;
                p[3] = channels[0].out_black as f32;
                p[4] = channels[0].out_white as f32;
                3
            } else {
                // Per-channel: bake master∘channel into the three LUT planes.
                // Must match AdjustmentType::apply_pixel_norm's compose order.
                for ch in 0..3 {
                    for i in 0..256 {
                        let v = i as f32 / 255.0;
                        lut[ch * 256 + i] = crate::core::layer::levels_eval(
                            &channels[0],
                            crate::core::layer::levels_eval(&channels[ch + 1], v),
                        );
                    }
                }
                13
            }
        }
        A::ColorBalance {
            shadows,
            midtones,
            highlights,
            preserve_luminosity,
        } => {
            p[0] = shadows[0];
            p[1] = shadows[1];
            p[2] = shadows[2];
            p[3] = if *preserve_luminosity { 1.0 } else { 0.0 };
            p[4] = midtones[0];
            p[5] = midtones[1];
            p[6] = midtones[2];
            p[8] = highlights[0];
            p[9] = highlights[1];
            p[10] = highlights[2];
            5
        }
        A::Vibrance {
            vibrance,
            saturation,
        } => {
            p[0] = *vibrance;
            p[1] = *saturation;
            6
        }
        A::Exposure {
            exposure,
            offset,
            gamma,
        } => {
            p[0] = *exposure;
            p[1] = *offset;
            p[2] = *gamma;
            7
        }
        A::Invert => 8,
        A::Threshold { value } => {
            p[0] = *value as f32;
            9
        }
        A::Posterize { levels } => {
            p[0] = *levels as f32;
            10
        }
        A::Desaturate => 11,
        A::Curves { channels } => {
            // Bake master∘channel into the three LUT planes. Must match
            // AdjustmentType::apply_pixel_norm's compose order.
            for ch in 0..3 {
                for i in 0..256 {
                    let v = i as f32 / 255.0;
                    lut[ch * 256 + i] = crate::core::layer::curves_eval(
                        &channels[0],
                        crate::core::layer::curves_eval(&channels[ch + 1], v),
                    );
                }
            }
            13
        }
        A::GradientMap {
            stops,
            reverse,
            dither,
        } => {
            p[0] = if *reverse { 1.0 } else { 0.0 };
            p[1] = if *dither { 1.0 } else { 0.0 };
            for i in 0..64 {
                let t = i as f32 / 63.0;
                let c = crate::core::color::sample_gradient_stops(stops, t);
                let base = i * 4;
                lut[base] = c[0] as f32 / 255.0;
                lut[base + 1] = c[1] as f32 / 255.0;
                lut[base + 2] = c[2] as f32 / 255.0;
                lut[base + 3] = 1.0;
            }
            12
        }
        _ => 0,
    };
    (kind, p, lut)
}

fn filter_to_gpu(filter: &crate::core::filters::FilterType) -> (u32, [f32; 12]) {
    use crate::core::filters::FilterType as F;
    let mut p = [0.0f32; 12];
    let kind = match filter {
        F::GaussianBlur { radius } => {
            p[0] = *radius;
            30
        }
        F::Sharpen { amount, radius } => {
            p[0] = *amount;
            p[1] = *radius;
            31
        }
        F::HighPass { radius } => {
            p[0] = *radius;
            32
        }
        F::AddNoise {
            amount,
            monochromatic,
        } => {
            p[0] = *amount;
            p[1] = f32::from(*monochromatic);
            33
        }
        F::Pixelate { cell } => {
            p[0] = *cell;
            34
        }
        F::ReduceNoise { strength } => {
            p[0] = *strength;
            35
        }
    };
    (kind, p)
}

/// Pack the Develop settings for the GPU preview shader.
/// Returns `(adj_p, adj_lut, local_lut, tone_active)` for the camera-raw shader.
///
/// Global tone (no Highlights/Shadows/Whites/Blacks, or no region proxy): `adj_lut`
/// is the full tone curve and `adj_p[4]` (the local flag) is 0. Local-adaptation
/// path: `adj_lut` is the curve WITHOUT H/S/W/B, `local_lut` carries the H/S/W/B
/// offset, and `adj_p[1] = (1, downsample, proxy_w, proxy_h)` so the shader samples
/// `region` for the offset — matching `ToneData::apply_local` on the CPU bake.
/// Returns `(adj_p, adj_lut, local_lut, tone_active)`. `tone_active` (0/1) goes to
/// the uniform's pad and tells the shader whether to run the tone stage at all — it
/// mirrors the CPU `tone_is_active`, so a Colour-only edit skips the highlight
/// roll-off on both sides (no pop).
fn develop_to_gpu(
    settings: &crate::core::develop::DevelopSettings,
    region: Option<&RegionLumaProxy>,
    color: Option<&ColorProxies>,
) -> ([f32; 12], [f32; 256], [f32; 256], u32) {
    let tone = crate::core::develop::build_tone_data(settings);
    let tone_active = u32::from(crate::core::develop::tone_is_active(settings));
    let mut adj_p = [0.0f32; 12];
    adj_p[0] = tone.gains[0];
    adj_p[1] = tone.gains[1];
    adj_p[2] = tone.gains[2];
    adj_p[3] = tone.ev;

    // Colour active. adj_p[6]/[7] carry the colour proxy's origin; adj_p[2] its
    // grid. When local tone (H/S/W/B) is ALSO engaged, run the regional
    // adaptation too — the shader composes local tone THEN colour — so a
    // Shadows/Blacks region does not jump the moment the Mixer engages. The
    // region-luma proxy derives its dims from the layer size, so it no longer
    // needs adj_p[1].z/.w (reused here for the colour origin).
    if let Some(c) = color {
        if !c.region.is_empty() {
            adj_p[8] = if c.fast_preview { 2.0 } else { 1.0 };
            adj_p[9] = c.downsample.max(1) as f32;
            adj_p[10] = c.w as f32;
            adj_p[11] = c.h as f32;
            adj_p[6] = c.origin_x as f32;
            adj_p[7] = c.origin_y as f32;
            if c.fast_preview {
                return (adj_p, [0.0f32; 256], [0.0f32; 256], 0);
            }
            if tone.is_local {
                if let Some(r) = region.filter(|r| !r.data.is_empty()) {
                    adj_p[4] = 1.0;
                    adj_p[5] = r.downsample.max(1) as f32;
                    return (adj_p, tone.global_lut, tone.local_lut, tone_active);
                }
            }
            return (adj_p, tone.lut, [0.0f32; 256], tone_active);
        }
    }

    match (tone.is_local, region) {
        (true, Some(r)) if !r.data.is_empty() => {
            adj_p[4] = 1.0;
            adj_p[5] = r.downsample.max(1) as f32;
            adj_p[6] = r.w as f32;
            adj_p[7] = r.h as f32;
            (adj_p, tone.global_lut, tone.local_lut, tone_active)
        }
        _ => (adj_p, tone.lut, [0.0f32; 256], tone_active),
    }
}

/// Scene-session twin of [`develop_to_gpu`]: `adj_lut` carries the log2-indexed
/// sigmoid LUT, `adj_p[1].x` flags the tone-equalizer (its EV-offset table and
/// the CAT16 matrix travel through `dev_local_lut` / `dev_effects`, uploaded by
/// `upload_develop_proxies`), and the colour-proxy slots pack exactly like the
/// legacy path so `dev_color_proxy_at` / `dev_finish_colored` are shared.
fn develop_scene_to_gpu(
    settings: &crate::core::develop::DevelopSettings,
    look: crate::core::develop_scene::BaseLook,
    region: Option<&RegionLumaProxy>,
    color: Option<&ColorProxies>,
) -> ([f32; 12], [f32; 256]) {
    let tone = crate::core::develop_scene::build_scene_tone_for(settings, look);
    let mut adj_p = [0.0f32; 12];
    if let Some(c) = color {
        if !c.region.is_empty() {
            adj_p[8] = if c.fast_preview { 2.0 } else { 1.0 };
            adj_p[9] = c.downsample.max(1) as f32;
            adj_p[10] = c.w as f32;
            adj_p[11] = c.h as f32;
            adj_p[6] = c.origin_x as f32;
            adj_p[7] = c.origin_y as f32;
        }
    }
    if settings.has_local_tone() {
        if let Some(r) = region.filter(|r| !r.data.is_empty()) {
            adj_p[4] = 1.0;
            adj_p[5] = r.downsample.max(1) as f32;
        }
    }
    (adj_p, tone.lut)
}

/// Data set by App when a layer is in free-transform preview mode.
/// Stored in CompositorState so composite_layers() can read it.
#[derive(Clone)]
pub struct TransformPreviewUniform {
    pub layer_id: u32,
    /// Destination-canvas to original-canvas projective transform.
    pub inv_m: [f32; 9],
    pub orig_ox: f32,
    pub orig_oy: f32,
}

/// Global image transform used by the modern Crop preview. Unlike Free Transform,
/// it applies to every visible layer so the whole composed image moves behind the
/// fixed crop viewport.
#[derive(Clone)]
pub struct CropPreviewUniform {
    pub inv_a: f32,
    pub inv_b: f32,
    pub inv_c: f32,
    pub inv_d: f32,
    pub pivot_x: f32,
    pub pivot_y: f32,
    pub tx: f32,
    pub ty: f32,
}

/// Downsampled regional base-luma proxy for the GPU local-tone preview, built by
/// `build_region_luma_proxy` on the App side and uploaded to `dev_region_luma_buf`.
#[derive(Clone)]
pub struct RegionLumaProxy {
    pub data: std::sync::Arc<Vec<f32>>,
    pub w: usize,
    pub h: usize,
    pub downsample: u32,
}

/// The `region`/`adjusted` RGB proxies for the GPU colour preview, built by
/// `build_color_proxies` and uploaded to `dev_region_rgb_buf`/`dev_adjusted_rgb_buf`.
#[derive(Clone)]
pub struct ColorProxies {
    pub region: std::sync::Arc<Vec<[f32; 3]>>,
    pub adjusted: std::sync::Arc<Vec<[f32; 3]>>,
    pub w: usize,
    pub h: usize,
    pub origin_x: u32,
    pub origin_y: u32,
    pub downsample: u32,
    pub fast_preview: bool,
}

#[derive(Clone)]
pub struct DevelopGpuPreview {
    pub layer_id: u32,
    pub settings: crate::core::develop::DevelopSettings,
    /// Present only when Highlights/Shadows/Whites/Blacks are engaged AND Colour is
    /// not (Colour falls back to global tone, matching the CPU bake).
    /// Scene sessions: this is the regional exposure E plane (EV values).
    pub region_luma: Option<RegionLumaProxy>,
    /// Present when Vibrance/Saturation/Color Mixer is engaged.
    pub color: Option<ColorProxies>,
    /// Linear scene master for RAW sessions: uploaded once as an Rgba16Float
    /// texture; the shader then runs the scene-referred chain (u.adj_pad_c == 1)
    /// instead of the legacy atlas tone path.
    pub scene: Option<std::sync::Arc<crate::core::develop_scene::SceneSource>>,
}

#[derive(Clone)]
pub struct FilterGpuPreview {
    pub layer_id: u32,
    pub filter: crate::core::filters::FilterType,
}

pub struct CompositorState {
    pub ping_texture: wgpu::Texture,
    pub ping_view: wgpu::TextureView,
    pub ping_bg: wgpu::BindGroup,
    pong_texture: wgpu::Texture,
    pub pong_view: wgpu::TextureView,
    pub pong_bg: wgpu::BindGroup,

    pub tile_atlas: TileAtlas,
    pub uniform_buf: wgpu::Buffer,
    pub tile_map_buf: wgpu::Buffer,
    pub mask_tile_map_buf: wgpu::Buffer,
    pub uniform_bg: wgpu::BindGroup,
    layer_uniform_bufs: Vec<wgpu::Buffer>,
    layer_tile_map_bufs: Vec<wgpu::Buffer>,
    layer_mask_tile_map_bufs: Vec<wgpu::Buffer>,
    layer_uniform_bgs: Vec<wgpu::BindGroup>,
    uniform_bg_generation: u64,
    layer_pool_generation: u64,

    /// Develop local-tone preview storage: the downsampled regional base-luma
    /// proxy (grows on demand) and the 256-entry H/S/W/B luma-offset LUT. Bound on
    /// group 2 (bindings 3/4) for every draw; only read when the local path is on.
    dev_region_luma_buf: wgpu::Buffer,
    dev_local_lut_buf: wgpu::Buffer,
    /// Capacity (in f32 elements) of `dev_region_luma_buf`; the buffer + uniform_bg
    /// are rebuilt when a proxy needs more.
    dev_region_capacity: usize,
    dev_uploaded_region_luma: Option<std::sync::Arc<Vec<f32>>>,

    /// Develop colour preview: the `region`/`adjusted` RGB proxies (packed f32,
    /// 3 per pixel). Grow together; `dev_color_capacity` is their shared f32 capacity.
    dev_region_rgb_buf: wgpu::Buffer,
    dev_adjusted_rgb_buf: wgpu::Buffer,
    dev_color_capacity: usize,
    dev_uploaded_region_rgb: Option<std::sync::Arc<Vec<[f32; 3]>>>,
    dev_uploaded_adjusted_rgb: Option<std::sync::Arc<Vec<[f32; 3]>>>,

    /// Develop Effects: 4 raw slider values [texture, clarity, dehaze, vignette].
    dev_effects_buf: wgpu::Buffer,
    dev_rgb_curve_buf: wgpu::Buffer,

    /// Scene-referred Develop master texture (Rgba16Float, full layer) for RAW
    /// sessions; the 1×1 dummy is bound outside a session. `dev_scene_key` is
    /// the Arc pointer of the uploaded SceneSource (0 = none) so the upload
    /// happens once per session, not per frame.
    dev_scene_tex: Option<wgpu::Texture>,
    #[allow(dead_code)]
    dev_scene_dummy_tex: wgpu::Texture,
    dev_scene_dummy_view: wgpu::TextureView,
    dev_scene_key: usize,

    pipeline: wgpu::RenderPipeline,
    adjustment_pipeline: wgpu::RenderPipeline,
    clear_pipeline: wgpu::RenderPipeline,
    #[allow(dead_code)]
    bg_layout_src: wgpu::BindGroupLayout,
    bg_layout_dst: wgpu::BindGroupLayout,
    #[allow(dead_code)]
    bg_layout_uniform: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,

    pub viewport_w: u32,
    pub viewport_h: u32,
    /// Texture downscale factor for the ping/pong (1 = full). The shader still
    /// maps to the full logical `viewport_w/h`, so a value >1 composites the same
    /// view at a fraction of the resolution — a fast, low-res preview proxy used
    /// while interactively moving/transforming. The blit upscales it.
    pub render_scale: u32,
    max_texture_dimension: u32,

    /// Pre-allocated scratch buffer for tile_map_data, reused every frame to
    /// avoid per-layer heap allocations (~25 k allocs/sec eliminated at 60 fps).
    tile_map_scratch: Vec<i32>,
    mask_tile_map_scratch: Vec<i32>,

    /// True once ping has been cleared at least once.
    /// Guards the partial-composite path: LoadOp::Load on an uninitialised
    /// ping texture reads garbage, so we must use LoadOp::Clear first.
    /// Reset to false on viewport resize.
    pub ping_initialized: bool,

    /// Buffer holding the latest full-composite result: true=ping, false=pong.
    /// Partial composite uses this to start from the correct accumulator and end
    /// on the same buffer, preserving pixels outside the scissored dirty rect.
    pub last_result_is_ping: bool,

    /// Every layer currently inside a free-transform session gets one entry.
    /// Each entry carries the same inverse matrix but a different layer_id /
    /// orig_ox/oy/w/h.  Updated by App on every drag; cleared on exit.
    /// Using a Vec lets multi-layer transforms preview ALL selected layers live.
    pub transform_previews: Vec<TransformPreviewUniform>,

    pub crop_preview: Option<CropPreviewUniform>,

    pub develop_preview: Option<DevelopGpuPreview>,

    /// Live destructive-adjustment preview (Ctrl+L/M etc.): apply this
    /// `AdjustmentType` to the matching raster layer's own pixels in the shader,
    /// instead of CPU-baking it into the layer tiles each drag step. Cleared on
    /// commit/cancel. `None` = no preview.
    pub preview_adj: Option<(u32, crate::core::layer::AdjustmentType)>,

    /// Live destructive-filter preview: apply a fast shader approximation to the
    /// matching raster layer. The layer tiles stay pristine until OK/commit.
    pub preview_filter: Option<FilterGpuPreview>,

    /// Compositing space. `true` (Mode A): ping/pong are CANVAS-sized and layers
    /// composite 1:1 (offset 0 / zoom 1); the final blit applies zoom/pan, so a
    /// view-only change is a cheap re-blit (no recomposite) — used for canvases
    /// that fit in a texture. `false` (Mode B): ping/pong are viewport-sized and
    /// the view transform is baked in (zoom/pan recomposites) — large/streaming
    /// canvases. The App sizes the viewport + sets this via `sync_compositor_viewport`.
    pub canvas_space: bool,

    /// Backdrop cache ("below-active projection", à la Photoshop). Holds the
    /// accumulated composite of the visible layers *below* the active layer so a
    /// full recomposite (adjustment/opacity/blend/Develop drag on a layer near
    /// the top) can resume from the snapshot and only blend the active layer and
    /// those above, instead of re-blending the whole stack every frame.
    backdrop_texture: wgpu::Texture,
    /// True once `backdrop_texture` holds a snapshot consistent with `backdrop_*`.
    backdrop_valid: bool,
    /// Number of visible-composited layers baked into the snapshot (the cut
    /// point, in visible-layer order).
    backdrop_boundary: usize,
    /// Per-layer fingerprints of the snapshotted prefix `[0..boundary)`. A HIT
    /// requires the current prefix to hash-match this exactly (catches any
    /// content/prop/structure change below the cut).
    backdrop_sig: Vec<u64>,
    /// Viewport + view signature at snapshot time: `(viewport_w, viewport_h,
    /// render_scale, canvas_space, view_offset_x, view_offset_y, zoom)` (floats as
    /// bit patterns). A mismatch invalidates the snapshot. The view transform is
    /// included because Mode B *bakes* zoom/pan into the composite, so a snapshot
    /// taken at one view must not be resumed under another (would misalign the
    /// frozen prefix against the freshly-composited upper layers).
    backdrop_vp: (u32, u32, u32, bool, u32, u32, u32),
    /// The ping/pong parity (`current_dst_is_ping`) at the instant of the
    /// snapshot — i.e. which buffer held the frozen prefix. Recorded (rather than
    /// derived from `boundary % 2`) so the resume is exact even when some prefix
    /// layers were skipped (oversized) and thus did not flip parity.
    backdrop_dst_is_ping: bool,
    /// Which region of `backdrop_texture` actually holds valid prefix pixels.
    /// `None` = a full-viewport snapshot (from the full recomposite path), valid
    /// everywhere. `Some(rect)` = a partial snapshot taken during a scissored
    /// dirty-rect composite, valid only inside `rect` (Phase 3). A partial resume
    /// may reuse it only when its scissor ⊆ this region; a full resume needs a
    /// full snapshot (`None`).
    backdrop_scissor: Option<(u32, u32, u32, u32)>,

    /// Zoomed-out LOD proxies, keyed by `(layer_id, level)`. When a large single
    /// (or few) layer is zoomed out far enough that its visible full-res tiles
    /// would overflow the atlas (forcing per-frame banding + re-upload), it is
    /// composited from a downsampled proxy that fits the atlas and stays resident
    /// across pans. Rebuilt on content change; LRU-evicted under a byte cap; and
    /// pruned to the visible set each frame. Mode B, full-recompose only — see
    /// `plan_layer_proxy`. `proxy_frame` is the monotonic clock the LRU uses.
    layer_proxies: std::collections::HashMap<(u32, u32), ProxyEntry>,
    proxy_frame: u64,
    /// Layer id whose LOD-proxy builds are suspended this recompose: while a
    /// Shape layer is interactively re-baked, every bake changes its content
    /// fingerprint, so a downsample spawned for it is stale before it lands.
    /// The layer simply composites full-res until the edit settles.
    pub proxy_build_suspend: Option<u32>,
    /// In-flight background proxy builds, keyed like `layer_proxies`. A layer with
    /// a pending build renders full-res until the downsample lands; the App
    /// requests a repaint while any build is pending so the proxy engages even if
    /// the zoom gesture already stopped.
    proxy_builds: std::collections::HashMap<(u32, u32), ProxyBuild>,
    /// Hybrid-canvas GPU vector stage. `Some` only when `IAI_GPU_VECTOR_CANVAS` is
    /// on; `None` leaves the raster pipeline unchanged. See `gpu::vector::composite`.
    vector_stage: Option<crate::gpu::vector::composite::VectorCompositeStage>,
    /// Layer ids the last composite drew natively on the GPU (empty when the GPU
    /// vector path is inactive). Kept as frame telemetry/debug state; callers that
    /// plan the current frame must use `will_draw_vector_layer_on_gpu` instead.
    pub gpu_drawn_layer_ids: Vec<u32>,
}

impl CompositorState {
    pub fn can_gpu_isolate_opacity_groups(&self, stack: &LayerStack, allow_active: bool) -> bool {
        crate::gpu::vector::eligibility::stack_supports_gpu_opacity_groups(stack)
            && stack.layers.iter().enumerate().all(|(index, layer)| {
                let in_effected_group = layer.parent_id.is_some_and(|parent_id| {
                    stack.layers.iter().any(|group| {
                        group.id == parent_id
                            && group.is_group()
                            && (group.opacity < 0.999
                                || group.blend_mode != crate::core::blend::BlendMode::Normal
                                || group.mask.as_ref().is_some_and(|mask| mask.enabled))
                    })
                });
                !in_effected_group
                    || !stack.is_effectively_visible(index)
                    || self.will_draw_vector_layer_on_gpu(
                        layer,
                        stack,
                        index,
                        stack.active_idx,
                        allow_active,
                    )
            })
    }

    /// Whether `layer` will use the native GPU-vector path in the current
    /// compositor state. Keep this as the single policy entry point for both
    /// scene planning and `path_display`: consulting the ids drawn by the
    /// previous frame creates a feedback loop because the crisp overlay itself
    /// temporarily hides raster twins from the next composite.
    pub fn will_draw_vector_layer_on_gpu(
        &self,
        layer: &Layer,
        layer_stack: &LayerStack,
        stack_idx: usize,
        active_idx: usize,
        allow_active: bool,
    ) -> bool {
        self.vector_stage.is_some()
            && !self.canvas_space
            && self.render_scale == 1
            && self.crop_preview.is_none()
            && (stack_idx != active_idx || allow_active)
            && !self
                .transform_previews
                .iter()
                .any(|preview| preview.layer_id == layer.id)
            && matches!(
                crate::gpu::vector::eligibility::layer_eligibility_in_stack(
                    layer,
                    layer_stack,
                    true,
                ),
                crate::gpu::vector::eligibility::Eligibility::GpuVector
            )
    }

    pub fn new(
        device: &wgpu::Device,
        viewport_w: u32,
        viewport_h: u32,
        max_texture_dimension: u32,
    ) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let bg_layout_src = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("comp_src_layout"),
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
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // Scene-referred Develop master (Rgba16Float; 1×1 dummy outside
                // a RAW session). f16 textures are filterable in core WebGPU.
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let bg_layout_dst = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("comp_dst_layout"),
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
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let bg_layout_uniform = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("comp_uniform_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
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
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Develop local-tone preview: region-luma proxy + local-tone LUT.
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Develop colour preview: region + adjusted RGB proxies.
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Develop Effects: 4 raw slider values.
                wgpu::BindGroupLayoutEntry {
                    binding: 7,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Develop R/G/B point-curve LUTs: [flag, 3×256].
                wgpu::BindGroupLayoutEntry {
                    binding: 8,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Compositor Shader"),
            source: wgpu::ShaderSource::Wgsl(COMPOSITOR_SHADER.into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("comp_pipeline_layout"),
            bind_group_layouts: &[
                Some(&bg_layout_src),
                Some(&bg_layout_dst),
                Some(&bg_layout_uniform),
            ],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("comp_pipeline"),
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
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let adjustment_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Adjustment Shader"),
            source: wgpu::ShaderSource::Wgsl(ADJUSTMENT_SHADER.into()),
        });

        let adjustment_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("adjustment_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &adjustment_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &adjustment_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let clear_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Clear Shader"),
            source: wgpu::ShaderSource::Wgsl(
                r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
};
@vertex fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var out: VsOut;
    let x = f32((vi << 1u) & 2u);
    let y = f32(vi & 2u);
    out.pos = vec4<f32>(x * 2.0 - 1.0, y * -2.0 + 1.0, 0.0, 1.0);
    return out;
}
@fragment fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 0.0);
}
            "#
                .into(),
            ),
        });

        let clear_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("clear_pipeline"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &clear_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &clear_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // 1×1 Rgba16Float dummy bound as the scene master outside RAW sessions.
        let dev_scene_dummy_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dev_scene_dummy"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let dev_scene_dummy_view =
            dev_scene_dummy_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let tile_atlas = TileAtlas::new(
            device,
            &bg_layout_src,
            &sampler,
            &dev_scene_dummy_view,
            max_texture_dimension,
        );

        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Compositor Uniforms"),
            size: std::mem::size_of::<CompositorUniformsData>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let tile_map_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Tile Map Buffer"),
            size: 16384 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mask_tile_map_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Mask Tile Map Buffer"),
            size: 16384 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let dev_region_capacity = 4096usize;
        let dev_region_luma_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dev_region_luma_buf"),
            size: (dev_region_capacity * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let dev_local_lut_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dev_local_lut_buf"),
            size: 256 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let dev_color_capacity = 4096usize;
        let dev_region_rgb_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dev_region_rgb_buf"),
            size: (dev_color_capacity * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let dev_adjusted_rgb_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dev_adjusted_rgb_buf"),
            size: (dev_color_capacity * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let dev_effects_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dev_effects_buf"),
            // [texture, clarity, dehaze, vignette, mixer_gate_active,
            //  5..16 free (the mixer re-gate table lives in dev_rgb_curve),
            //  scene: CAT16·2^EV matrix 16..25, hue-preserve 25,
            //  display-curve flag 26, pad 27]
            size: 28 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let dev_rgb_curve_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dev_rgb_curve_buf"),
            // [active flag, R 256, G 256, B 256, scene display luma 256,
            //  mixer re-gate LUT 360 (1025..1385)]
            size: 1385 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let uniform_bg = Self::build_uniform_bg(
            device,
            &bg_layout_uniform,
            &uniform_buf,
            &tile_map_buf,
            &mask_tile_map_buf,
            &dev_region_luma_buf,
            &dev_local_lut_buf,
            &dev_region_rgb_buf,
            &dev_adjusted_rgb_buf,
            &dev_effects_buf,
            &dev_rgb_curve_buf,
        );

        let viewport_w = viewport_w.clamp(1, max_texture_dimension.max(1));
        let viewport_h = viewport_h.clamp(1, max_texture_dimension.max(1));

        let (ping_texture, ping_view, ping_bg) = Self::create_pingpong(
            device,
            viewport_w,
            viewport_h,
            &bg_layout_dst,
            &sampler,
            "ping",
        );
        let (pong_texture, pong_view, pong_bg) = Self::create_pingpong(
            device,
            viewport_w,
            viewport_h,
            &bg_layout_dst,
            &sampler,
            "pong",
        );
        let backdrop_texture = Self::create_backdrop(device, viewport_w, viewport_h);

        Self {
            ping_texture,
            ping_view,
            ping_bg,
            pong_texture,
            pong_view,
            pong_bg,
            tile_atlas,
            uniform_buf,
            tile_map_buf,
            mask_tile_map_buf,
            uniform_bg,
            layer_uniform_bufs: Vec::new(),
            layer_tile_map_bufs: Vec::new(),
            layer_mask_tile_map_bufs: Vec::new(),
            layer_uniform_bgs: Vec::new(),
            uniform_bg_generation: 0,
            layer_pool_generation: u64::MAX,
            dev_region_luma_buf,
            dev_local_lut_buf,
            dev_region_capacity,
            dev_uploaded_region_luma: None,
            dev_region_rgb_buf,
            dev_adjusted_rgb_buf,
            dev_color_capacity,
            dev_uploaded_region_rgb: None,
            dev_uploaded_adjusted_rgb: None,
            dev_effects_buf,
            dev_rgb_curve_buf,
            dev_scene_tex: None,
            dev_scene_dummy_tex,
            dev_scene_dummy_view,
            dev_scene_key: 0,
            pipeline,
            adjustment_pipeline,
            clear_pipeline,
            bg_layout_src,
            bg_layout_dst,
            bg_layout_uniform,
            sampler,
            viewport_w,
            viewport_h,
            render_scale: 1,
            max_texture_dimension: max_texture_dimension.max(1),
            tile_map_scratch: vec![0i32; 16384],
            mask_tile_map_scratch: vec![0i32; 16384],
            ping_initialized: false,
            last_result_is_ping: false,
            transform_previews: Vec::new(),
            crop_preview: None,
            develop_preview: None,
            preview_adj: None,
            preview_filter: None,
            canvas_space: false,
            backdrop_texture,
            backdrop_valid: false,
            backdrop_boundary: 0,
            backdrop_sig: Vec::new(),
            backdrop_vp: (viewport_w, viewport_h, 1, false, 0, 0, 0),
            backdrop_dst_is_ping: false,
            backdrop_scissor: None,
            layer_proxies: std::collections::HashMap::new(),
            proxy_frame: 0,
            proxy_build_suspend: None,
            proxy_builds: std::collections::HashMap::new(),
            // Built only when the hybrid-canvas flag is on; None keeps the raster
            // pipeline byte-for-byte unchanged. Rebuilt with the whole GpuState on
            // device loss.
            vector_stage: crate::gpu::vector::runtime_enabled().then(|| {
                crate::gpu::vector::composite::VectorCompositeStage::new(
                    device,
                    wgpu::TextureFormat::Rgba8UnormSrgb,
                )
            }),
            gpu_drawn_layer_ids: Vec::new(),
        }
    }

    /// (Re)build the group-2 bind group. Called at init and whenever
    /// `dev_region_luma_buf` is grown (a bind group pins its buffers by identity).
    #[allow(clippy::too_many_arguments)]
    fn build_uniform_bg(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        uniform_buf: &wgpu::Buffer,
        tile_map_buf: &wgpu::Buffer,
        mask_tile_map_buf: &wgpu::Buffer,
        dev_region_luma_buf: &wgpu::Buffer,
        dev_local_lut_buf: &wgpu::Buffer,
        dev_region_rgb_buf: &wgpu::Buffer,
        dev_adjusted_rgb_buf: &wgpu::Buffer,
        dev_effects_buf: &wgpu::Buffer,
        dev_rgb_curve_buf: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("comp_uniform_bg"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: tile_map_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: mask_tile_map_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: dev_region_luma_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: dev_local_lut_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: dev_region_rgb_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: dev_adjusted_rgb_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: dev_effects_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: dev_rgb_curve_buf.as_entire_binding(),
                },
            ],
        })
    }

    /// Rebuild group 2 from the current buffers (call after growing any CR buffer).
    fn rebuild_uniform_bg(&mut self, device: &wgpu::Device) {
        self.uniform_bg = Self::build_uniform_bg(
            device,
            &self.bg_layout_uniform,
            &self.uniform_buf,
            &self.tile_map_buf,
            &self.mask_tile_map_buf,
            &self.dev_region_luma_buf,
            &self.dev_local_lut_buf,
            &self.dev_region_rgb_buf,
            &self.dev_adjusted_rgb_buf,
            &self.dev_effects_buf,
            &self.dev_rgb_curve_buf,
        );
        self.uniform_bg_generation = self.uniform_bg_generation.wrapping_add(1);
    }

    fn ensure_layer_bind_pool(&mut self, device: &wgpu::Device, count: usize) {
        while self.layer_uniform_bufs.len() < count {
            let idx = self.layer_uniform_bufs.len();
            self.layer_uniform_bufs
                .push(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(&format!("comp_layer_uniform_buf_{idx}")),
                    size: std::mem::size_of::<CompositorUniformsData>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
            self.layer_tile_map_bufs
                .push(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(&format!("comp_layer_tile_map_buf_{idx}")),
                    size: (self.tile_map_scratch.len() * std::mem::size_of::<i32>()) as u64,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
            self.layer_mask_tile_map_bufs
                .push(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(&format!("comp_layer_mask_tile_map_buf_{idx}")),
                    size: (self.mask_tile_map_scratch.len() * std::mem::size_of::<i32>()) as u64,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
        }

        if self.layer_pool_generation == self.uniform_bg_generation
            && self.layer_uniform_bgs.len() >= self.layer_uniform_bufs.len()
        {
            return;
        }

        self.layer_uniform_bgs.clear();
        for idx in 0..self.layer_uniform_bufs.len() {
            self.layer_uniform_bgs.push(Self::build_uniform_bg(
                device,
                &self.bg_layout_uniform,
                &self.layer_uniform_bufs[idx],
                &self.layer_tile_map_bufs[idx],
                &self.layer_mask_tile_map_bufs[idx],
                &self.dev_region_luma_buf,
                &self.dev_local_lut_buf,
                &self.dev_region_rgb_buf,
                &self.dev_adjusted_rgb_buf,
                &self.dev_effects_buf,
                &self.dev_rgb_curve_buf,
            ));
        }
        self.layer_pool_generation = self.uniform_bg_generation;
    }

    /// Upload the Develop preview proxies for this frame. Local-tone (region luma
    /// + offset LUT) and Colour (region + adjusted RGB) are independent; each grows
    /// its buffer on demand and rebuilds uniform_bg. When both are engaged the
    /// region-luma proxy is still uploaded so the shader runs the regional H/S/W/B
    /// adaptation under the Mixer (matching the CPU bake — no Shadows/Blacks jump).
    /// Upload/drop the scene master texture when the session changes. The Arc
    /// pointer keys the upload, so a live drag costs nothing here.
    fn sync_develop_scene(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let want = self
            .develop_preview
            .as_ref()
            .and_then(|p| p.scene.clone())
            // Oversized masters fall back to the CPU-bake preview app-side;
            // never try to allocate a texture past the device limit.
            .filter(|s| {
                s.width.max(1) <= self.max_texture_dimension
                    && s.height.max(1) <= self.max_texture_dimension
            });
        let key = want
            .as_ref()
            .map(|s| std::sync::Arc::as_ptr(s) as usize)
            .unwrap_or(0);
        if key == self.dev_scene_key {
            return;
        }
        match want {
            Some(scene) => {
                let w = scene.width.max(1);
                let h = scene.height.max(1);
                let tex = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("dev_scene_tex"),
                    size: wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba16Float,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                // write_texture wants bytes_per_row % 256 == 0 (h > 1): upload
                // row-padded. One-time cost per session.
                let bpr = w as usize * 8;
                let padded = bpr.div_ceil(256) * 256;
                let src = bytemuck::cast_slice::<u16, u8>(&scene.half);
                if padded == bpr {
                    queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &tex,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        src,
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(bpr as u32),
                            rows_per_image: Some(h),
                        },
                        wgpu::Extent3d {
                            width: w,
                            height: h,
                            depth_or_array_layers: 1,
                        },
                    );
                } else {
                    let mut buf = vec![0u8; padded * h as usize];
                    for y in 0..h as usize {
                        buf[y * padded..y * padded + bpr]
                            .copy_from_slice(&src[y * bpr..(y + 1) * bpr]);
                    }
                    queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &tex,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        &buf,
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
                }
                let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
                self.tile_atlas
                    .rebind_scene(device, &self.bg_layout_src, &self.sampler, &view);
                self.dev_scene_tex = Some(tex);
            }
            None => {
                self.dev_scene_tex = None;
                self.tile_atlas.rebind_scene(
                    device,
                    &self.bg_layout_src,
                    &self.sampler,
                    &self.dev_scene_dummy_view,
                );
            }
        }
        self.dev_scene_key = key;
    }

    /// Whether the shader can run the scene chain for this preview (the master
    /// texture fits the device). The app gates the GPU path on this too.
    pub fn scene_fits_texture(&self, scene: &crate::core::develop_scene::SceneSource) -> bool {
        scene.width.max(1) <= self.max_texture_dimension
            && scene.height.max(1) <= self.max_texture_dimension
    }

    fn upload_develop_proxies(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        self.sync_develop_scene(device, queue);
        if self.develop_preview.is_none() {
            self.dev_uploaded_region_luma = None;
            self.dev_uploaded_region_rgb = None;
            self.dev_uploaded_adjusted_rgb = None;
            return;
        }

        let local_upload: Option<(std::sync::Arc<Vec<f32>>, [f32; 256])> =
            self.develop_preview.as_ref().and_then(|p| {
                let r = p.region_luma.as_ref()?;
                if r.data.is_empty() {
                    return None;
                }
                if let Some(scene) = &p.scene {
                    // Scene session: the offset table is the tone-equalizer's
                    // EV offsets over the shared EV axis.
                    if !p.settings.has_local_tone() {
                        return None;
                    }
                    let tone =
                        crate::core::develop_scene::build_scene_tone_for(&p.settings, scene.look);
                    return Some((r.data.clone(), tone.tone_eq));
                }
                let tone = crate::core::develop::build_tone_data(&p.settings);
                if !tone.is_local {
                    return None;
                }
                Some((r.data.clone(), tone.local_lut))
            });
        if let Some((region_data, local_lut)) = local_upload {
            if region_data.len() > self.dev_region_capacity {
                let cap = region_data.len().next_power_of_two();
                self.dev_region_luma_buf = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("dev_region_luma_buf"),
                    size: (cap * 4) as u64,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                self.dev_region_capacity = cap;
                self.dev_uploaded_region_luma = None;
                self.rebuild_uniform_bg(device);
            }
            let same_region = self
                .dev_uploaded_region_luma
                .as_ref()
                .is_some_and(|prev| std::sync::Arc::ptr_eq(prev, &region_data));
            if !same_region {
                queue.write_buffer(
                    &self.dev_region_luma_buf,
                    0,
                    bytemuck::cast_slice(&region_data),
                );
                self.dev_uploaded_region_luma = Some(region_data);
            }
            queue.write_buffer(&self.dev_local_lut_buf, 0, bytemuck::cast_slice(&local_lut));
        } else {
            self.dev_uploaded_region_luma = None;
        }

        let color_upload: Option<(std::sync::Arc<Vec<[f32; 3]>>, std::sync::Arc<Vec<[f32; 3]>>)> =
            self.develop_preview.as_ref().and_then(|p| {
                let c = p.color.as_ref()?;
                if c.region.is_empty() {
                    return None;
                }
                Some((c.region.clone(), c.adjusted.clone()))
            });
        if let Some((region, adjusted)) = color_upload {
            let len_f32 = region.len() * 3;
            if len_f32 > self.dev_color_capacity {
                let cap = len_f32.next_power_of_two();
                self.dev_region_rgb_buf = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("dev_region_rgb_buf"),
                    size: (cap * 4) as u64,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                self.dev_adjusted_rgb_buf = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("dev_adjusted_rgb_buf"),
                    size: (cap * 4) as u64,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                self.dev_color_capacity = cap;
                self.dev_uploaded_region_rgb = None;
                self.dev_uploaded_adjusted_rgb = None;
                self.rebuild_uniform_bg(device);
            }
            let same_region = self
                .dev_uploaded_region_rgb
                .as_ref()
                .is_some_and(|prev| std::sync::Arc::ptr_eq(prev, &region));
            if !same_region {
                queue.write_buffer(&self.dev_region_rgb_buf, 0, bytemuck::cast_slice(&region));
                self.dev_uploaded_region_rgb = Some(region);
            }
            let same_adjusted = self
                .dev_uploaded_adjusted_rgb
                .as_ref()
                .is_some_and(|prev| std::sync::Arc::ptr_eq(prev, &adjusted));
            if !same_adjusted {
                queue.write_buffer(
                    &self.dev_adjusted_rgb_buf,
                    0,
                    bytemuck::cast_slice(&adjusted),
                );
                self.dev_uploaded_adjusted_rgb = Some(adjusted);
            }
        } else {
            self.dev_uploaded_region_rgb = None;
            self.dev_uploaded_adjusted_rgb = None;
        }

        // Effects: 4 raw slider values (the shader eases + masks them per
        // pixel) + the mixer anti-bleed gate flag (the gate TABLE itself rides
        // in dev_rgb_curve[1025..1385] — the same curve-LUT machinery the CPU
        // plan gates with, so preview and bake gate identically). Scene
        // sessions append the CAT16·2^EV matrix, the hue-preserve blend and
        // the display-curve flag ([16..27]).
        if let Some(p) = self.develop_preview.as_ref() {
            let s = &p.settings;
            let mut effects = [0.0f32; 28];
            effects[0] = s.texture;
            effects[1] = s.clarity;
            effects[2] = s.dehaze;
            effects[3] = s.vignette;
            let mixer_gated = crate::core::develop::mixer_edit_mask(s).is_some();
            if mixer_gated {
                effects[4] = 1.0;
            }

            // R/G/B point curves: [flag, 3×256] — the exact tables the CPU
            // ToneData::apply_rgb_curves reads, so preview and bake match.
            // [769..1025] is the scene display luma curve; [1025..1385] the
            // mixer re-gate LUT.
            let mut rgb = vec![0.0f32; 1385];
            if let Some(luts) = crate::core::develop::rgb_curve_luts(s) {
                rgb[0] = 1.0;
                for (ch, lut) in luts.iter().enumerate() {
                    rgb[1 + ch * 256..1 + (ch + 1) * 256].copy_from_slice(lut);
                }
            }
            if mixer_gated {
                if let Some(curves) = crate::core::develop::build_mixer_curves_opt(s) {
                    rgb[1025..1385].copy_from_slice(&curves.gate);
                }
            }
            if let Some(scene) = &p.scene {
                let tone = crate::core::develop_scene::build_scene_tone_for(s, scene.look);
                for (i, row) in tone.wb_ev.iter().enumerate() {
                    effects[16 + i * 3] = row[0];
                    effects[16 + i * 3 + 1] = row[1];
                    effects[16 + i * 3 + 2] = row[2];
                }
                effects[25] = f32::from(tone.shadow_chroma_active);
                if let Some(display) = &tone.display {
                    effects[26] = 1.0;
                    rgb[769..1025].copy_from_slice(display.as_ref());
                }
            }
            queue.write_buffer(&self.dev_effects_buf, 0, bytemuck::cast_slice(&effects));
            queue.write_buffer(&self.dev_rgb_curve_buf, 0, bytemuck::cast_slice(&rgb));
        }
    }

    fn create_pingpong(
        device: &wgpu::Device,
        w: u32,
        h: u32,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        label: &str,
    ) -> (wgpu::Texture, wgpu::TextureView, wgpu::BindGroup) {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                // COPY_DST lets the backdrop cache restore a snapshot into
                // ping/pong via copy_texture_to_texture (resume path).
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&format!("{}_bg", label)),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        (tex, view, bg)
    }

    /// Backdrop-cache texture: a copy-only snapshot of the accumulated composite
    /// of the layers *below* the active layer. Same size/format as ping/pong, but
    /// never rendered into or sampled directly — only `copy_texture_to_texture`d
    /// to/from ping/pong, so it needs only COPY_SRC | COPY_DST.
    fn create_backdrop(device: &wgpu::Device, w: u32, h: u32) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("backdrop_cache"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    /// Set the logical viewport (`w`×`h`, used by the shader for the view
    /// transform) and a texture downscale `scale` (1 = full). The ping/pong are
    /// created at `w/scale × h/scale`; with `scale>1` the same view composites at
    /// a fraction of the resolution (cheap low-res preview), and the blit upscales.
    pub fn configure_viewport(&mut self, device: &wgpu::Device, w: u32, h: u32, scale: u32) {
        let w = w.clamp(1, self.max_texture_dimension);
        let h = h.clamp(1, self.max_texture_dimension);
        let scale = scale.max(1);
        if self.viewport_w == w && self.viewport_h == h && self.render_scale == scale {
            return;
        }
        self.viewport_w = w;
        self.viewport_h = h;
        self.render_scale = scale;
        let tw = w.div_ceil(scale).max(1);
        let th = h.div_ceil(scale).max(1);
        let (pt, pv, pbg) =
            Self::create_pingpong(device, tw, th, &self.bg_layout_dst, &self.sampler, "ping");
        self.ping_texture = pt;
        self.ping_view = pv;
        self.ping_bg = pbg;
        let (pot, pov, pobg) =
            Self::create_pingpong(device, tw, th, &self.bg_layout_dst, &self.sampler, "pong");
        self.pong_texture = pot;
        self.pong_view = pov;
        self.pong_bg = pobg;
        self.backdrop_texture = Self::create_backdrop(device, tw, th);
        self.backdrop_valid = false;
        self.backdrop_scissor = None;
        self.ping_initialized = false;
        self.last_result_is_ping = false;
    }

    fn blend_mode_to_u32(mode: &BlendMode) -> u32 {
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
            BlendMode::Dissolve => 17,
        }
    }

    fn mask_atlas_layer_id(layer_id: u32) -> usize {
        (layer_id as usize).wrapping_add(1usize << (usize::BITS - 1))
    }

    /// Atlas key namespace for a layer's LOD-proxy tiles. Bit 62 tags "proxy" and
    /// the level sits in bits 48.. so proxy levels never collide with each other,
    /// the full-res tiles (bit 62 clear), or masks (bit 63). `layer_id` occupies
    /// the low 32 bits.
    fn proxy_atlas_layer_id(layer_id: u32, level: u32) -> usize {
        (layer_id as usize) | ((level as usize) << 48) | (1usize << 62)
    }

    /// Atlas key namespace for a proxy layer's mask tiles (proxy tag + mask bit).
    fn proxy_mask_atlas_layer_id(layer_id: u32, level: u32) -> usize {
        Self::proxy_atlas_layer_id(layer_id, level) | (1usize << 63)
    }

    /// Build the level-`level` proxy (`S = 2^level`) by halving the base `level`
    /// times, discarding the intermediates so only the target level is retained
    /// (one base read of transient RAM, not a whole mip chain kept resident).
    fn build_proxy_level(
        base: &TileMap,
        base_mask: Option<&TileMap>,
        level: usize,
    ) -> (TileMap, Option<TileMap>) {
        let mut tiles = base.downsample_half();
        let mut mask = base_mask.map(TileMap::downsample_half);
        for _ in 1..level {
            tiles = tiles.downsample_half();
            mask = mask.map(|m| m.downsample_half());
        }
        (tiles, mask)
    }

    /// Approximate heap footprint of a proxy `TileMap` (8-bit mirror only).
    fn proxy_map_bytes(m: &TileMap) -> usize {
        m.tiles.len() * crate::core::tile::TILE_BYTES
    }

    /// Evict least-recently-used proxy levels until the cached total is under the
    /// byte cap. Entries used this frame (`last_used == proxy_frame`) are never
    /// evicted — they are needed right now — so if the current frame's live
    /// proxies alone exceed the cap it is transiently overshot rather than
    /// thrashing a proxy the same frame still needs.
    fn evict_proxies_over_cap(&mut self) {
        Self::evict_proxies(
            &mut self.layer_proxies,
            PROXY_CACHE_BYTES_CAP,
            self.proxy_frame,
        );
    }

    /// Pure LRU eviction over a proxy map (extracted so it is testable without a
    /// GPU `CompositorState`). See [`Self::evict_proxies_over_cap`].
    fn evict_proxies(
        map: &mut std::collections::HashMap<(u32, u32), ProxyEntry>,
        cap: usize,
        frame: u64,
    ) {
        let mut total: usize = map.values().map(|e| e.bytes).sum();
        while total > cap {
            let victim = map
                .iter()
                .filter(|(_, e)| e.last_used != frame)
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| *k);
            let Some(v) = victim else { break };
            if let Some(e) = map.remove(&v) {
                total -= e.bytes;
            }
        }
    }

    /// Decide whether `layer` should composite from a zoomed-out LOD proxy this
    /// frame, and if so build/refresh its chain and return the render plan. Only
    /// engages in Mode B, on a full recompose, for a plain raster layer with no
    /// active preview, and only when the layer's visible full-res tiles would
    /// overflow the atlas (i.e. banding + per-frame re-upload would otherwise
    /// thrash). The proxy level is chosen so it renders at ≤ 1:1 (sharp, no
    /// upsample) and its visible tile count stays a fraction of the viewport.
    #[allow(clippy::too_many_arguments)]
    fn plan_layer_proxy(
        &mut self,
        layer: &Layer,
        zoom: f32,
        view_offset_x: f32,
        view_offset_y: f32,
        use_partial: bool,
        slot_count: usize,
        vw: u32,
        vh: u32,
    ) -> Option<ProxyRender> {
        if self.canvas_space || use_partial || zoom <= 0.0 || zoom >= 1.0 {
            return None;
        }
        if matches!(
            layer.layer_type,
            crate::core::layer::LayerType::Adjustment(_)
        ) {
            return None;
        }
        let id = layer.id;
        let previewed = self
            .develop_preview
            .as_ref()
            .is_some_and(|p| p.layer_id == id)
            || self.preview_adj.as_ref().is_some_and(|(pid, _)| *pid == id)
            || self
                .preview_filter
                .as_ref()
                .is_some_and(|p| p.layer_id == id)
            || self.transform_previews.iter().any(|t| t.layer_id == id)
            || self.crop_preview.is_some();
        if previewed {
            return None;
        }
        let ltw = (layer.width + 255) / 256;
        let lth = (layer.height + 255) / 256;
        let total = (ltw * lth) as usize;
        // Empty (no grid) or oversized layers (skipped by the composite loop and
        // the parity precount alike) never take the proxy path.
        if total == 0 || total > 16384 {
            return None;
        }
        let band = (0.0, 0.0, vw as f32, vh as f32);
        let off = (layer.offset.0 as f32, layer.offset.1 as f32);
        let mask_enabled = layer.mask.as_ref().is_some_and(|m| m.enabled);
        let mut vis = Self::visible_tile_slots_for_rect(
            &layer.tiles,
            band,
            zoom,
            (view_offset_x, view_offset_y),
            off,
            layer.width,
            layer.height,
            ltw,
            lth,
        );
        if mask_enabled {
            if let Some(m) = layer.mask.as_ref() {
                vis += Self::visible_tile_slots_for_rect(
                    &m.tiles,
                    band,
                    zoom,
                    (view_offset_x, view_offset_y),
                    off,
                    layer.width,
                    layer.height,
                    ltw,
                    lth,
                );
            }
        }
        if vis <= slot_count {
            return None;
        }
        let level = ((1.0 / zoom).log2().floor() as i32).clamp(1, PROXY_MAX_LEVEL as i32) as usize;
        let key = (id, level as u32);
        let src_fp = layer.tiles.revision_fingerprint();
        let mask_ref = layer.mask.as_ref().filter(|m| m.enabled);
        let mask_fp = mask_ref.map_or(0, |m| m.tiles.revision_fingerprint());

        // 1. A cached proxy current with the layer's content → use it now.
        if self
            .layer_proxies
            .get(&key)
            .is_some_and(|e| e.src_fp == src_fp && e.mask_fp == mask_fp)
        {
            return self.take_proxy(key, level);
        }

        // 2. Poll a matching background build (the borrow of the map ends with
        // `poll`, so the map can be mutated in the match below).
        let poll = self.proxy_builds.get(&key).map(|b| {
            if b.src_fp == src_fp && b.mask_fp == mask_fp {
                match b.rx.try_recv() {
                    Ok(res) => BuildPoll::Ready(res),
                    Err(std::sync::mpsc::TryRecvError::Empty) => BuildPoll::Building,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => BuildPoll::Respawn,
                }
            } else {
                BuildPoll::Respawn // build was for now-stale content
            }
        });
        match poll {
            Some(BuildPoll::Ready((tiles, mask))) => {
                self.proxy_builds.remove(&key);
                let bytes =
                    Self::proxy_map_bytes(&tiles) + mask.as_ref().map_or(0, Self::proxy_map_bytes);
                self.layer_proxies.insert(
                    key,
                    ProxyEntry {
                        src_fp,
                        mask_fp,
                        tiles,
                        mask,
                        bytes,
                        last_used: self.proxy_frame,
                    },
                );
                self.evict_proxies_over_cap();
                return self.take_proxy(key, level);
            }
            Some(BuildPoll::Building) => return None, // still building → full-res this frame
            Some(BuildPoll::Respawn) => {
                self.proxy_builds.remove(&key);
            }
            None => {}
        }

        // 3. Spawn a background build (the downsample is the expensive part) and
        // render full-res until it lands. Cloning the base is cheap (Arc clones).
        // Suspended layers (mid-interactive-rebake) skip the spawn: the build
        // would be stale before it lands.
        if self.proxy_build_suspend == Some(id) {
            return None;
        }
        let base = layer.tiles.clone();
        let mask_base = mask_ref.map(|m| m.tiles.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(Self::build_proxy_level(&base, mask_base.as_ref(), level));
        });
        self.proxy_builds.insert(
            key,
            ProxyBuild {
                src_fp,
                mask_fp,
                rx,
            },
        );
        None
    }

    /// Clone the cached proxy at `key` into a `ProxyRender`, refreshing its LRU
    /// timestamp. `None` only if the entry is absent (just inserted → always Some).
    fn take_proxy(&mut self, key: (u32, u32), level: usize) -> Option<ProxyRender> {
        let entry = self.layer_proxies.get_mut(&key)?;
        entry.last_used = self.proxy_frame;
        let (pw, ph) = (entry.tiles.width, entry.tiles.height);
        Some(ProxyRender {
            level: level as u32,
            tiles: entry.tiles.clone(),
            mask: entry.mask.clone(),
            pw,
            ph,
        })
    }

    /// Whether any background proxy build is still running. The App polls this
    /// after compositing and requests a repaint while true, so a proxy that
    /// finishes after the zoom gesture stops still gets swapped in.
    pub fn has_pending_proxy_builds(&self) -> bool {
        !self.proxy_builds.is_empty()
    }

    /// The visible-composited layers (group folders and fully-transparent /
    /// hidden layers excluded), each tagged with its original stack index, plus
    /// the backdrop cut point: the number of those layers that sit *below* the
    /// active layer. Everything `[0..boundary)` is the frozen prefix the backdrop
    /// cache snapshots; `[boundary..]` (the active layer and above) is re-blended.
    fn visible_layers_and_boundary(layer_stack: &LayerStack) -> (Vec<(usize, &Layer)>, usize) {
        let visible: Vec<(usize, &Layer)> = layer_stack
            .layers
            .iter()
            .enumerate()
            .filter(|(i, l)| {
                !l.is_group()
                    && l.opacity > 0.001
                    && layer_stack.is_effectively_visible(*i)
                    && l.has_renderable_content()
            })
            .collect();
        let active_idx = layer_stack.active_idx;
        let boundary = visible.iter().take_while(|(i, _)| *i < active_idx).count();
        (visible, boundary)
    }

    /// Whether `inner` is fully contained in `outer` (both `(x, y, w, h)`). Used
    /// to decide if a partial-snapshot region still covers the current scissor.
    fn rect_contains(outer: (u32, u32, u32, u32), inner: (u32, u32, u32, u32)) -> bool {
        let (ox, oy, ow, oh) = outer;
        let (ix, iy, iw, ih) = inner;
        ix >= ox && iy >= oy && ix + iw <= ox + ow && iy + ih <= oy + oh
    }

    /// Cheap content+prop fingerprint of a single layer for the backdrop cache.
    /// Two layers with equal fingerprints composite identically; conversely any
    /// change that alters the composited result must change the fingerprint. Used
    /// to validate the cached prefix (layers below the active layer) each frame —
    /// a mismatch means the backdrop snapshot is stale and must be rebuilt.
    fn layer_fingerprint(layer: &crate::core::layer::Layer) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        layer.id.hash(&mut h);
        Self::blend_mode_to_u32(&layer.blend_mode).hash(&mut h);
        layer.opacity.to_bits().hash(&mut h);
        layer.offset.hash(&mut h);
        layer.visible.hash(&mut h);
        // Pixel content — also covers Raster/Text/Shape visual state, which is
        // rasterised into these tiles before compositing.
        layer.tiles.revision_fingerprint().hash(&mut h);
        match &layer.mask {
            Some(m) => {
                true.hash(&mut h);
                m.enabled.hash(&mut h);
                m.inverted.hash(&mut h);
                m.tiles.revision_fingerprint().hash(&mut h);
            }
            None => false.hash(&mut h),
        }
        // Adjustment layers are parametric (their tiles are unused); fold the GPU
        // parameters in so a slider change on a *below* adjustment invalidates.
        match &layer.layer_type {
            crate::core::layer::LayerType::Adjustment(adj) => {
                0u8.hash(&mut h);
                let (kind, params, lut) = adjustment_to_gpu(adj);
                kind.hash(&mut h);
                for v in params {
                    v.to_bits().hash(&mut h);
                }
                for v in lut {
                    v.to_bits().hash(&mut h);
                }
            }
            crate::core::layer::LayerType::Raster => 1u8.hash(&mut h),
            crate::core::layer::LayerType::Group => 2u8.hash(&mut h),
            crate::core::layer::LayerType::Text(_) => 3u8.hash(&mut h),
            crate::core::layer::LayerType::Vector(
                crate::core::vector::object::VectorGeometry::Primitive(_),
            ) => 4u8.hash(&mut h),
            crate::core::layer::LayerType::SmartObject => 5u8.hash(&mut h),
            // Path renders through its tiles cache (hashed above via the tile
            // revision fingerprint), like Shape/Text — the discriminant is enough.
            crate::core::layer::LayerType::Vector(
                crate::core::vector::object::VectorGeometry::Path(_),
            ) => 6u8.hash(&mut h),
        }
        h.finish()
    }

    /// Largest square output block (in viewport px) whose gathered tile grid — the
    /// layer's tiles plus, when present, its mask's tiles — is guaranteed to fit the
    /// atlas. A block spanning `S` layer px touches at most `floor(S/256) + 2` tiles
    /// per axis (a `ceil` and a `floor` straddle add one each), so capping the span at
    /// `(tiles_side - 1) * 256 - 1` bounds it at `tiles_side = floor(sqrt(budget))`
    /// tiles/axis → `budget` tiles total. `budget` is the per-source slot budget
    /// (halved when a mask doubles the uploads). Used to split a single oversized
    /// layer into per-block passes (see the band loop below).
    fn overflow_band_px(slot_count: usize, mask_present: bool, zoom: f32) -> u32 {
        let budget = (slot_count / (1 + usize::from(mask_present))).max(1);
        let tiles_side = (budget as f64).sqrt().floor() as u32;
        let span = ((tiles_side.max(2) - 1) * 256).saturating_sub(1);
        ((span as f32) * zoom).floor().max(1.0) as u32
    }

    /// Split `region` (`x, y, w, h` in viewport px) into a grid of disjoint sub-rects
    /// each no larger than `max_w × max_h`; their union is exactly `region`. Empty
    /// (`w == 0 || h == 0`) yields no blocks. Used to tile an oversized layer's output
    /// so each block's tiles fit the atlas.
    fn tile_region(
        region: (u32, u32, u32, u32),
        max_w: u32,
        max_h: u32,
    ) -> Vec<(u32, u32, u32, u32)> {
        let (rx, ry, rw, rh) = region;
        let max_w = max_w.max(1);
        let max_h = max_h.max(1);
        let mut out = Vec::new();
        let mut y = ry;
        while y < ry + rh {
            let bh = max_h.min(ry + rh - y);
            let mut x = rx;
            while x < rx + rw {
                let bw = max_w.min(rx + rw - x);
                out.push((x, y, bw, bh));
                x += bw;
            }
            y += bh;
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn tile_span_for_rect(
        band: (f32, f32, f32, f32),
        zoom: f32,
        view_offset: (f32, f32),
        layer_off: (f32, f32),
        layer_w: u32,
        layer_h: u32,
        layer_tiles_w: u32,
        layer_tiles_h: u32,
    ) -> Option<(u32, u32, u32, u32)> {
        if zoom <= 0.0 || layer_w == 0 || layer_h == 0 || layer_tiles_w == 0 || layer_tiles_h == 0 {
            return None;
        }

        let (src_sx0, src_sy0, src_sx1, src_sy1) = band;
        let layer_x0 = (src_sx0 / zoom + view_offset.0) - layer_off.0;
        let layer_y0 = (src_sy0 / zoom + view_offset.1) - layer_off.1;
        let layer_x1 = (src_sx1 / zoom + view_offset.0) - layer_off.0;
        let layer_y1 = (src_sy1 / zoom + view_offset.1) - layer_off.1;

        let px0 = layer_x0.floor().clamp(0.0, layer_w as f32) as u32;
        let py0 = layer_y0.floor().clamp(0.0, layer_h as f32) as u32;
        let px1 = layer_x1.ceil().clamp(0.0, layer_w as f32) as u32;
        let py1 = layer_y1.ceil().clamp(0.0, layer_h as f32) as u32;
        if px1 <= px0 || py1 <= py0 {
            return None;
        }

        let tx0 = (px0 / 256).min(layer_tiles_w - 1);
        let ty0 = (py0 / 256).min(layer_tiles_h - 1);
        let tx1 = ((px1 - 1) / 256).min(layer_tiles_w - 1);
        let ty1 = ((py1 - 1) / 256).min(layer_tiles_h - 1);
        Some((tx0, ty0, tx1, ty1))
    }

    fn unique_tile_slots_in_span(
        tiles: &crate::core::tile::TileMap,
        span: (u32, u32, u32, u32),
    ) -> usize {
        let (tx0, ty0, tx1, ty1) = span;
        let mut seen = std::collections::HashSet::new();
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                let pos = TilePos {
                    x: tx as i32,
                    y: ty as i32,
                };
                if let Some(tile) = tiles.tiles.get(&pos) {
                    seen.insert(std::sync::Arc::as_ptr(tile));
                }
            }
        }
        seen.len()
    }

    #[allow(clippy::too_many_arguments)]
    fn visible_tile_slots_for_rect(
        tiles: &crate::core::tile::TileMap,
        band: (f32, f32, f32, f32),
        zoom: f32,
        view_offset: (f32, f32),
        layer_off: (f32, f32),
        layer_w: u32,
        layer_h: u32,
        layer_tiles_w: u32,
        layer_tiles_h: u32,
    ) -> usize {
        Self::tile_span_for_rect(
            band,
            zoom,
            view_offset,
            layer_off,
            layer_w,
            layer_h,
            layer_tiles_w,
            layer_tiles_h,
        )
        .map_or(0, |span| Self::unique_tile_slots_in_span(tiles, span))
    }

    fn atlas_slot_for_tile(
        atlas: &mut TileAtlas,
        queue: &wgpu::Queue,
        slot_cache: &mut std::collections::HashMap<*const crate::core::tile::Tile, i32>,
        atlas_layer_id: usize,
        pos: TilePos,
        arc_tile: &std::sync::Arc<crate::core::tile::Tile>,
    ) -> i32 {
        let ptr = std::sync::Arc::as_ptr(arc_tile);
        if let Some(&slot) = slot_cache.get(&ptr) {
            return slot;
        }

        let (slot_x, slot_y, needs_upload) =
            atlas.get_or_allocate(atlas_layer_id, pos, arc_tile.revision);
        if needs_upload {
            atlas.upload_tile(queue, slot_x, slot_y, &arc_tile.pixels);
        }
        let slot = (slot_x | (slot_y << 16)) as i32;
        slot_cache.insert(ptr, slot);
        slot
    }

    /// Resolve one tile source (a layer's or its mask's `TileMap`) for a single
    /// output block into `scratch` (a per-tile atlas-slot map, `-1` = no tile),
    /// uploading any tiles not already resident. `scratch[..total_tiles]` is reset to
    /// `-1` first, then only the block's tiles are set — so a banded layer rewrites
    /// the full map per block with just that block's tiles resident.
    ///
    /// The scissor branch mirrors the original inline gather exactly; the
    /// `is_transform` branch iterates *all* tiles (a preview transform's inverse map
    /// isn't axis-aligned to the block) and is therefore only ever called with a
    /// single full-region block — it is not banded.
    #[allow(clippy::too_many_arguments)]
    fn gather_tiles_for_rect(
        atlas: &mut TileAtlas,
        queue: &wgpu::Queue,
        scratch: &mut [i32],
        atlas_layer_id: usize,
        tiles: &crate::core::tile::TileMap,
        total_tiles: usize,
        layer_tiles_w: u32,
        layer_tiles_h: u32,
        is_transform: bool,
        band: (f32, f32, f32, f32),
        zoom: f32,
        view_offset: (f32, f32),
        layer_off: (f32, f32),
        layer_w: u32,
        layer_h: u32,
    ) {
        scratch[..total_tiles].fill(-1);
        let mut slot_cache: std::collections::HashMap<*const crate::core::tile::Tile, i32> =
            std::collections::HashMap::new();
        if is_transform {
            for (pos, arc_tile) in &tiles.tiles {
                if pos.x < 0 || pos.y < 0 {
                    continue;
                }
                let tx = pos.x as u32;
                let ty = pos.y as u32;
                if tx >= layer_tiles_w || ty >= layer_tiles_h {
                    continue;
                }
                let idx = (ty * layer_tiles_w + tx) as usize;
                scratch[idx] = Self::atlas_slot_for_tile(
                    atlas,
                    queue,
                    &mut slot_cache,
                    atlas_layer_id,
                    *pos,
                    arc_tile,
                );
            }
            return;
        }

        let Some((tx0, ty0, tx1, ty1)) = Self::tile_span_for_rect(
            band,
            zoom,
            view_offset,
            layer_off,
            layer_w,
            layer_h,
            layer_tiles_w,
            layer_tiles_h,
        ) else {
            return;
        };

        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                let idx = (ty * layer_tiles_w + tx) as usize;
                let pos = TilePos {
                    x: tx as i32,
                    y: ty as i32,
                };
                if let Some(arc_tile) = tiles.tiles.get(&pos) {
                    scratch[idx] = Self::atlas_slot_for_tile(
                        atlas,
                        queue,
                        &mut slot_cache,
                        atlas_layer_id,
                        pos,
                        arc_tile,
                    );
                }
            }
        }
    }

    /// Whether `layer` will actually be blended (i.e. flip the ping-pong parity)
    /// in the composite loop this frame. This MUST mirror the loop's skip rules
    /// so the partial-path parity precount stays exact: oversized layers and
    /// raster/text/shape layers whose tiles miss the dirty band are skipped
    /// without flipping parity, while adjustment and transform/crop layers always
    /// draw. `band` is the shared dirty rect in viewport px (full viewport in the
    /// non-partial path).
    fn layer_draws_this_frame(
        &self,
        layer: &Layer,
        band: (f32, f32, f32, f32),
        view_offset_x: f32,
        view_offset_y: f32,
        zoom: f32,
    ) -> bool {
        let layer_tiles_w = (layer.width + 255) / 256;
        let layer_tiles_h = (layer.height + 255) / 256;
        let total_tiles = (layer_tiles_w * layer_tiles_h) as usize;
        if total_tiles > 16384 {
            return false;
        }
        let is_adjustment = matches!(
            layer.layer_type,
            crate::core::layer::LayerType::Adjustment(_)
        );
        let is_transform = self
            .transform_previews
            .iter()
            .any(|t| t.layer_id == layer.id)
            || self.crop_preview.is_some();
        if is_adjustment || is_transform {
            return true;
        }
        let layer_slots = Self::visible_tile_slots_for_rect(
            &layer.tiles,
            band,
            zoom,
            (view_offset_x, view_offset_y),
            (layer.offset.0 as f32, layer.offset.1 as f32),
            layer.width,
            layer.height,
            layer_tiles_w,
            layer_tiles_h,
        );
        if layer_slots > 0 {
            return true;
        }
        layer
            .mask
            .as_ref()
            .filter(|m| m.enabled)
            .is_some_and(|mask| {
                Self::visible_tile_slots_for_rect(
                    &mask.tiles,
                    band,
                    zoom,
                    (view_offset_x, view_offset_y),
                    (layer.offset.0 as f32, layer.offset.1 as f32),
                    layer.width,
                    layer.height,
                    layer_tiles_w,
                    layer_tiles_h,
                ) > 0
            })
    }

    pub fn composite_layers(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layer_stack: &LayerStack,
        view_offset_x: f32,
        view_offset_y: f32,
        zoom: f32,
        dirty_rect: Option<(u32, u32, u32, u32)>,
        allow_backdrop_cache: bool,
        allow_active_gpu_vector: bool,
    ) -> bool {
        let mut command_buffers: Vec<wgpu::CommandBuffer> = Vec::new();

        // Each entry keeps its original stack index; `boundary` is the backdrop
        // cut = the visible-composited layers *below* the active layer. The active
        // layer (and everything above) is re-blended every frame; the frozen
        // prefix below is served from `backdrop_texture`.
        let (visible_layers, boundary) = Self::visible_layers_and_boundary(layer_stack);
        let n_layers = visible_layers.len();
        let active_idx = layer_stack.active_idx;

        // Hybrid canvas (Phase 2): per visible layer, is it an eligible GPU vector
        // layer this frame? Only in the full viewport path (not canvas_space / not
        // low-res preview); a present vector run forces a full composite (no partial
        // dirty-rect, no backdrop cache) so the ping/pong parity stays trivial.
        // Crop transforms the whole stack per-layer (not by id), so a crop preview
        // disables the GPU vector path entirely for the frame.
        let gpu_eligible: Vec<bool> = visible_layers
            .iter()
            .map(|&(stack_idx, layer)| {
                // The ACTIVE layer is the one being edited: node/style/shape
                // drags update a pending raster preview (its tiles), not the
                // committed model the GPU reads, so keep it on the existing
                // raster + crisp-overlay path. Free-transform previews (which
                // can target several layers) are excluded by id. Non-active
                // static vector layers render natively on the GPU. A live
                // multi-Move of other selected layers is followed via the
                // offset drift correction in `composite_run`.
                self.will_draw_vector_layer_on_gpu(
                    layer,
                    layer_stack,
                    stack_idx,
                    active_idx,
                    allow_active_gpu_vector,
                )
            })
            .collect();
        let gpu_vector_active = gpu_eligible.iter().any(|&e| e);

        // Record exactly the layers this composite draws on the GPU, so
        // `path_display` can drop their redundant CPU crisp-overlay bake (Phase 8).
        // Cleared when the GPU path is inactive so a stale set never suppresses a
        // layer that is back on the raster path.
        self.gpu_drawn_layer_ids.clear();
        if gpu_vector_active {
            for (slot, &(_, layer)) in visible_layers.iter().enumerate() {
                if gpu_eligible[slot] {
                    self.gpu_drawn_layer_ids.push(layer.id);
                }
            }
        }

        let use_partial = dirty_rect.is_some()
            && self.ping_initialized
            && self.render_scale == 1
            && !gpu_vector_active;
        if let Some(stage) = self.vector_stage.as_mut() {
            if gpu_vector_active {
                stage.begin_frame();
            }
        }
        // A live preview (Develop / Ctrl+L·M / filter) targeting a layer inside
        // the frozen prefix would make the snapshot stale — its parameters live
        // in compositor state, not in the layer fingerprint. In practice previews
        // target the active layer (== boundary), never below; guard anyway.
        let preview_target_below = |id: u32| -> bool {
            layer_stack
                .layers
                .iter()
                .position(|l| l.id == id)
                .is_some_and(|idx| idx < active_idx)
        };
        let preview_conflict = self
            .develop_preview
            .as_ref()
            .is_some_and(|p| preview_target_below(p.layer_id))
            || self
                .preview_adj
                .as_ref()
                .is_some_and(|(pid, _)| preview_target_below(*pid))
            || self
                .preview_filter
                .as_ref()
                .is_some_and(|p| preview_target_below(p.layer_id))
            // A crop preview transforms the *whole* stack (applied per-layer, not
            // by id), so it would alter the frozen prefix too.
            || self.crop_preview.is_some()
            // A multi-layer free-transform can drag a layer that sits below the
            // active one; freezing it in the backdrop would show it un-transformed.
            || self
                .transform_previews
                .iter()
                .any(|t| preview_target_below(t.layer_id));
        let vp_sig = (
            self.viewport_w,
            self.viewport_h,
            self.render_scale,
            self.canvas_space,
            // Mode B bakes the view into the composite, so a view change must
            // invalidate the snapshot. Mode A always passes (0,0,1) here (the view
            // is applied at blit time), so this is a no-op for it.
            view_offset_x.to_bits(),
            view_offset_y.to_bits(),
            zoom.to_bits(),
        );
        // The clamped scissor sub-rect for the partial path (`None` when the rect
        // degenerates to empty, or in the full path). The backdrop cache keys its
        // partial-snapshot validity to this region.
        let clamped_scissor: Option<(u32, u32, u32, u32)> = if use_partial {
            dirty_rect.and_then(|(sx, sy, sw, sh)| {
                let sx = sx.min(self.viewport_w);
                let sy = sy.min(self.viewport_h);
                let sw = sw.min(self.viewport_w.saturating_sub(sx));
                let sh = sh.min(self.viewport_h.saturating_sub(sy));
                (sw > 0 && sh > 0).then_some((sx, sy, sw, sh))
            })
        } else {
            None
        };

        // Phase 3: the cache now serves the partial dirty-rect path too. A slider
        // drag (adjustment/opacity) recomposites only the visible region; the
        // frozen prefix is restored inside that scissor and the loop resumes at
        // the cut. `boundary` sits at the active layer, so the dragged layer (and
        // everything above) is still re-blended — only the layers below are served
        // from the snapshot.
        let cache_enabled = allow_backdrop_cache
            && self.render_scale == 1
            && boundary > 0
            && boundary < n_layers
            && !preview_conflict
            && !gpu_vector_active
            && (!use_partial || clamped_scissor.is_some());
        let cur_sig: Vec<u64> = if cache_enabled {
            visible_layers[..boundary]
                .iter()
                .map(|&(_, l)| Self::layer_fingerprint(l))
                .collect()
        } else {
            Vec::new()
        };
        // The snapshot must cover the region we resume into. A partial snapshot is
        // valid only inside its scissor (`backdrop_scissor = Some`); a full-path
        // snapshot covers the whole viewport (`None`). So a partial resume needs
        // its scissor ⊆ the snapshot's region, and a full resume needs a full
        // snapshot.
        let scissor_ok = match (use_partial, self.backdrop_scissor, clamped_scissor) {
            (false, snap, _) => snap.is_none(),
            (true, None, _) => true,
            (true, Some(snap), Some(cur)) => Self::rect_contains(snap, cur),
            (true, Some(_), None) => false,
        };
        let cache_hit = cache_enabled
            && self.backdrop_valid
            && self.ping_initialized
            && self.backdrop_boundary == boundary
            && self.backdrop_vp == vp_sig
            && self.backdrop_sig == cur_sig
            && scissor_ok;

        // Parity of the accumulator buffer the first blended layer reads. In the
        // partial path the final result must land back in `last_result_is_ping` so
        // the untouched out-of-scissor pixels stay the previous frame's composite.
        // The parity flips once per *blended* layer, so count only the layers that
        // will actually be drawn — those `[boundary..]` on a resume (else all).
        // `layer_draws_this_frame` mirrors the loop's skip rules (oversized layers,
        // and raster/text/shape layers whose tiles miss the dirty band); a drifting
        // count lands the result in the wrong ping/pong buffer, leaving the
        // out-of-scissor surround stale.
        let mut current_dst_is_ping = if use_partial {
            let band = match dirty_rect {
                Some((dx, dy, dw, dh)) => (
                    dx.min(self.viewport_w) as f32,
                    dy.min(self.viewport_h) as f32,
                    (dx + dw).min(self.viewport_w) as f32,
                    (dy + dh).min(self.viewport_h) as f32,
                ),
                None => (0.0, 0.0, self.viewport_w as f32, self.viewport_h as f32),
            };
            let range = if cache_hit {
                boundary..n_layers
            } else {
                0..n_layers
            };
            let blended = visible_layers[range]
                .iter()
                .filter(|(_, l)| {
                    self.layer_draws_this_frame(l, band, view_offset_x, view_offset_y, zoom)
                })
                .count();
            if blended % 2 == 0 {
                self.last_result_is_ping
            } else {
                !self.last_result_is_ping
            }
        } else {
            true
        };
        let mut first_layer = true;

        // On a partial resume the start buffer's scissor region is seeded with the
        // cached prefix (below) instead of cleared to transparent.
        if use_partial && !cache_hit {
            let (sx, sy, sw, sh) = dirty_rect.unwrap();
            let sx = sx.min(self.viewport_w);
            let sy = sy.min(self.viewport_h);
            let sw = sw.min(self.viewport_w.saturating_sub(sx));
            let sh = sh.min(self.viewport_h.saturating_sub(sy));
            if sw > 0 && sh > 0 {
                let clear_view = if current_dst_is_ping {
                    &self.ping_view
                } else {
                    &self.pong_view
                };
                let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("partial_clear_enc"),
                });
                {
                    let mut rpass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("partial_clear"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: clear_view,
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
                    rpass.set_pipeline(&self.clear_pipeline);
                    rpass.set_scissor_rect(sx, sy, sw, sh);
                    rpass.draw(0..3, 0..1);
                }
                command_buffers.push(enc.finish());
            }
        }

        let (src_sx0, src_sy0, src_sx1, src_sy1) = match (use_partial, dirty_rect) {
            (true, Some((dx, dy, dw, dh))) => (
                dx.min(self.viewport_w) as f32,
                dy.min(self.viewport_h) as f32,
                (dx + dw).min(self.viewport_w) as f32,
                (dy + dh).min(self.viewport_h) as f32,
            ),
            _ => (0.0, 0.0, self.viewport_w as f32, self.viewport_h as f32),
        };

        // Develop preview: upload the region proxies the per-layer shader reads
        // (the layer loop only points the uniform at these buffers). Buffers grow on
        // demand; growing rebuilds uniform_bg (a bind group pins its buffers).
        self.upload_develop_proxies(device, queue);
        self.ensure_layer_bind_pool(device, n_layers);

        // Zoomed-out LOD proxy pre-pass: decide, per visible layer, whether to
        // composite it from a downsampled proxy (large layer zoomed out far enough
        // to overflow the atlas), building/refreshing its chain now so the layer
        // loop below only reads cheap owned clones (no `&mut self` conflict with
        // the atlas). The proxy path only changes a layer's tile *source* and view
        // scale — never whether it draws — so the parity precount above (full-res)
        // stays exact. `plan_layer_proxy` returns `None` in Mode A / partial paths.
        self.proxy_frame = self.proxy_frame.wrapping_add(1);
        let slot_count = self.tile_atlas.slot_count;
        let (vw, vh) = (self.viewport_w, self.viewport_h);
        // Layers below the cut are served from the frozen backdrop on a cache hit,
        // so they aren't recomposited and never need (or should spend a build on)
        // a proxy.
        let frozen = if cache_hit { boundary } else { 0 };
        let proxy_plans: Vec<Option<ProxyRender>> = visible_layers
            .iter()
            .enumerate()
            .map(|(slot, &(_idx, layer))| {
                if slot < frozen {
                    return None;
                }
                self.plan_layer_proxy(
                    layer,
                    zoom,
                    view_offset_x,
                    view_offset_y,
                    use_partial,
                    slot_count,
                    vw,
                    vh,
                )
            })
            .collect();
        // Keep the proxy cache + in-flight builds bounded to the layers on screen.
        let visible_ids: std::collections::HashSet<u32> =
            visible_layers.iter().map(|&(_, l)| l.id).collect();
        self.layer_proxies
            .retain(|(lid, _), _| visible_ids.contains(lid));
        self.proxy_builds
            .retain(|(lid, _), _| visible_ids.contains(lid));

        // Backdrop resume: seed the accumulator with the cached prefix and start
        // the layer loop at the cut, instead of re-blending [0..boundary).
        if cache_hit {
            // Full resume: restore the exact parity captured at snapshot time so
            // layer `boundary` reads the buffer we copy the backdrop into (skipped
            // oversized layers don't flip parity), and copy the whole backdrop.
            // Partial resume: keep the parity that lands the result back in
            // `last_result_is_ping` (already set above) and copy only the scissor
            // sub-rect, so the out-of-scissor pixels of the result buffer stay the
            // previous frame's composite.
            let copy_region = if use_partial {
                clamped_scissor
            } else {
                current_dst_is_ping = self.backdrop_dst_is_ping;
                None
            };
            let dst_tex = if current_dst_is_ping {
                &self.ping_texture
            } else {
                &self.pong_texture
            };
            let (ox, oy, cw, ch) = match copy_region {
                Some((sx, sy, sw, sh)) => (sx, sy, sw, sh),
                None => (
                    0,
                    0,
                    self.backdrop_texture.width(),
                    self.backdrop_texture.height(),
                ),
            };
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("backdrop_restore"),
            });
            enc.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.backdrop_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: ox, y: oy, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: dst_tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: ox, y: oy, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: cw,
                    height: ch,
                    depth_or_array_layers: 1,
                },
            );
            command_buffers.push(enc.finish());
            first_layer = false;
        }
        let start_slot = if cache_hit { boundary } else { 0 };

        // Atlas-overflow guard. The tile atlas holds a fixed number of 256² slots.
        // When the tiles visible this frame (across all layers) exceed that — e.g.
        // a large multi-layer canvas zoomed out, where every tile of every layer
        // is on screen at once — a later layer's tile upload would evict a slot an
        // earlier, not-yet-submitted layer pass still samples, scrambling the
        // output (and dropping the background). To prevent that we submit the
        // pending passes before a batch of layers would overflow the atlas: once
        // their passes have run, their slots are safe to evict/reuse. `batch_touched`
        // counts the tiles charged to the current (unsubmitted) batch.
        let atlas_slot_count = self.tile_atlas.slot_count;
        let mut batch_touched: usize = 0;

        for (layer_slot, &(_stack_idx, layer)) in visible_layers.iter().enumerate() {
            // Resume: the frozen prefix is already in the accumulator.
            if layer_slot < start_slot {
                continue;
            }
            // Hybrid canvas: an eligible GPU vector layer is drawn natively by the
            // vector run, not by the raster tile pass (its one representation — no
            // halo). Handle the whole contiguous run at its first layer, then skip
            // the members here. A run participates in the ping/pong as one step.
            if gpu_eligible[layer_slot] {
                let layer_has_mask = layer.mask.as_ref().is_some_and(|mask| mask.enabled);
                let previous_joins = layer_slot > 0
                    && gpu_eligible[layer_slot - 1]
                    && visible_layers[layer_slot - 1].1.parent_id == layer.parent_id
                    && !layer_has_mask
                    && !visible_layers[layer_slot - 1]
                        .1
                        .mask
                        .as_ref()
                        .is_some_and(|mask| mask.enabled);
                let is_run_start = !previous_joins;
                if is_run_start {
                    use crate::core::layer::LayerType;
                    use crate::core::vector::object::VectorGeometry;
                    let mut run_end = layer_slot;
                    if layer_has_mask {
                        run_end += 1;
                    } else {
                        while run_end < n_layers
                            && gpu_eligible[run_end]
                            && visible_layers[run_end].1.parent_id == layer.parent_id
                            && !visible_layers[run_end]
                                .1
                                .mask
                                .as_ref()
                                .is_some_and(|mask| mask.enabled)
                        {
                            run_end += 1;
                        }
                    }
                    // Phase 6: a Primitive is drawn by converting it to the same
                    // `PathData` its raster twin uses (`ShapeData::to_vector_object`).
                    // The converted objects are owned temporaries, so they are built
                    // into `converted` *first* (capacity reserved so references never
                    // dangle), then borrowed alongside the Path objects in z-order.
                    let prim_count = (layer_slot..run_end)
                        .filter(|&j| {
                            matches!(
                                &visible_layers[j].1.layer_type,
                                LayerType::Vector(VectorGeometry::Primitive(_))
                            )
                        })
                        .count();
                    let mut converted: Vec<crate::core::vector::object::VectorObjectData> =
                        Vec::with_capacity(prim_count);
                    for j in layer_slot..run_end {
                        if let LayerType::Vector(VectorGeometry::Primitive(shape)) =
                            &visible_layers[j].1.layer_type
                        {
                            converted.push(shape.to_vector_object(visible_layers[j].1.offset));
                        }
                    }
                    let mut objects: Vec<(
                        &crate::core::vector::object::VectorObjectData,
                        (i32, i32),
                        f32,
                    )> = Vec::with_capacity(run_end - layer_slot);
                    let mut ci = 0usize;
                    for j in layer_slot..run_end {
                        let run_layer = visible_layers[j].1;
                        match &run_layer.layer_type {
                            LayerType::Vector(VectorGeometry::Path(obj)) => {
                                objects.push((obj, run_layer.offset, run_layer.opacity));
                            }
                            LayerType::Vector(VectorGeometry::Primitive(_)) => {
                                // The position is already baked into the converted
                                // object's transform, so pass its own raster origin:
                                // the drag-drift correction becomes a no-op and the
                                // shape draws exactly where its raster twin sat.
                                let obj = &converted[ci];
                                let origin = crate::core::vector::raster::raster_geometry(obj)
                                    .map(|(o, _, _)| o)
                                    .unwrap_or(run_layer.offset);
                                objects.push((obj, origin, run_layer.opacity));
                                ci += 1;
                            }
                            _ => {}
                        }
                    }
                    if !objects.is_empty() {
                        let run_group = layer.parent_id.and_then(|parent_id| {
                            layer_stack
                                .layers
                                .iter()
                                .find(|candidate| candidate.id == parent_id)
                        });
                        let run_opacity = run_group.map_or(1.0, |group| group.opacity);
                        let run_blend_mode =
                            run_group.map_or(layer.blend_mode, |group| group.blend_mode);
                        let run_mask_owner = run_group.unwrap_or(layer);
                        let vw = self.viewport_w;
                        let vh = self.viewport_h;
                        let ping_v = self.ping_view.clone();
                        let pong_v = self.pong_view.clone();
                        let (dst_read, dst_write) = if current_dst_is_ping {
                            (&ping_v, &pong_v)
                        } else {
                            (&pong_v, &ping_v)
                        };
                        let mut enc =
                            device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("vector_run_enc"),
                            });
                        // First drawn content: seed its background (dst_read)
                        // transparent, mirroring the raster first-layer clear.
                        if first_layer && !use_partial {
                            let rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                                label: Some("vector_run_clear"),
                                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                    view: dst_read,
                                    resolve_target: None,
                                    depth_slice: None,
                                    ops: wgpu::Operations {
                                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                        store: wgpu::StoreOp::Store,
                                    },
                                })],
                                depth_stencil_attachment: None,
                                ..Default::default()
                            });
                            drop(rp);
                            self.ping_initialized = true;
                        }
                        first_layer = false;
                        if let Some(stage) = self.vector_stage.as_mut() {
                            stage.composite_run(
                                device,
                                queue,
                                &mut enc,
                                dst_read,
                                dst_write,
                                vw,
                                vh,
                                view_offset_x,
                                view_offset_y,
                                zoom,
                                &objects,
                                run_mask_owner
                                    .mask
                                    .as_ref()
                                    .filter(|mask| mask.enabled)
                                    .map(|mask| {
                                        crate::gpu::vector::composite::VectorMask {
                                            layer_id: run_mask_owner.id,
                                            // Group masks use canvas coordinates in
                                            // the CPU synthetic-layer reference.
                                            layer_offset: run_group
                                                .map_or(layer.offset, |_| (0, 0)),
                                            sample_shift: if run_group.is_some() {
                                                (0, 0)
                                            } else {
                                                layer.clip_parent_id.map_or((0, 0), |frame_id| {
                                                    let frame_now = layer_stack
                                                        .layers
                                                        .iter()
                                                        .find(|candidate| candidate.id == frame_id)
                                                        .map(|frame| frame.offset)
                                                        .unwrap_or(mask.bake_frame_offset);
                                                    (
                                                        (layer.offset.0 - mask.bake_offset.0)
                                                            - (frame_now.0
                                                                - mask.bake_frame_offset.0),
                                                        (layer.offset.1 - mask.bake_offset.1)
                                                            - (frame_now.1
                                                                - mask.bake_frame_offset.1),
                                                    )
                                                })
                                            },
                                            mask,
                                        }
                                    }),
                                run_opacity,
                                run_blend_mode,
                            );
                        }
                        command_buffers.push(enc.finish());
                        // The run is one ping/pong step: flip parity exactly once.
                        current_dst_is_ping = !current_dst_is_ping;
                    }
                }
                continue;
            }
            // Snapshot: capture the accumulated prefix [0..boundary) the moment
            // before the active layer is blended over it, so a later interactive
            // frame can resume from here.
            if cache_enabled && !cache_hit && layer_slot == boundary {
                let src_tex = if current_dst_is_ping {
                    &self.ping_texture
                } else {
                    &self.pong_texture
                };
                // In the partial path only the scissor region of the accumulator
                // holds valid prefix pixels, so snapshot just that sub-rect (and
                // record it as the snapshot's validity region below).
                let (ox, oy, cw, ch) = match clamped_scissor {
                    Some((sx, sy, sw, sh)) if use_partial => (sx, sy, sw, sh),
                    _ => (
                        0,
                        0,
                        self.backdrop_texture.width(),
                        self.backdrop_texture.height(),
                    ),
                };
                let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("backdrop_snapshot"),
                });
                enc.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: src_tex,
                        mip_level: 0,
                        origin: wgpu::Origin3d { x: ox, y: oy, z: 0 },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture: &self.backdrop_texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d { x: ox, y: oy, z: 0 },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width: cw,
                        height: ch,
                        depth_or_array_layers: 1,
                    },
                );
                command_buffers.push(enc.finish());
                // Record which buffer holds the prefix so the next resume restores
                // the identical parity (skipped oversized layers don't flip it).
                self.backdrop_dst_is_ping = current_dst_is_ping;
            }

            let layer_tiles_w = (layer.width + 255) / 256;
            let layer_tiles_h = (layer.height + 255) / 256;
            let total_tiles = (layer_tiles_w * layer_tiles_h) as usize;

            if total_tiles > 16384 {
                continue;
            }

            let mask_present = layer.mask.as_ref().is_some_and(|m| m.enabled);

            // Effective tile source, dimensions, atlas namespace and view scale
            // for this layer. Normally the layer's own full-res tiles; when a LOD
            // proxy is engaged, the downsampled proxy plus a view rescaled by
            // `S = 2^level` (zoom·S, view_offset/S, offset/S, layer_w/h/tiles at
            // proxy resolution). The shader computes `layer_x = screen_x/zoom' +
            // view' − off'`, which for the proxy equals `layer_x_full / S` — the
            // exact proxy-local coordinate — with `dev_local = layer_x/layer_w`
            // unchanged, so no shader change is needed.
            let proxy = proxy_plans[layer_slot].as_ref();
            let (
                eff_tiles,
                eff_mask_tiles,
                eff_w,
                eff_h,
                eff_tiles_w,
                eff_tiles_h,
                eff_total_tiles,
                eff_zoom,
                eff_view_off,
                eff_off,
                eff_atlas_id,
                eff_mask_atlas_id,
            ): (
                &TileMap,
                Option<&TileMap>,
                u32,
                u32,
                u32,
                u32,
                usize,
                f32,
                (f32, f32),
                (f32, f32),
                usize,
                usize,
            ) = if let Some(p) = proxy {
                let s = (1u32 << p.level) as f32;
                let etw = (p.pw + 255) / 256;
                let eth = (p.ph + 255) / 256;
                (
                    &p.tiles,
                    p.mask.as_ref(),
                    p.pw,
                    p.ph,
                    etw,
                    eth,
                    (etw * eth) as usize,
                    zoom * s,
                    (view_offset_x / s, view_offset_y / s),
                    (layer.offset.0 as f32 / s, layer.offset.1 as f32 / s),
                    Self::proxy_atlas_layer_id(layer.id, p.level),
                    Self::proxy_mask_atlas_layer_id(layer.id, p.level),
                )
            } else {
                (
                    &layer.tiles,
                    layer.mask.as_ref().filter(|m| m.enabled).map(|m| &m.tiles),
                    layer.width,
                    layer.height,
                    layer_tiles_w,
                    layer_tiles_h,
                    total_tiles,
                    zoom,
                    (view_offset_x, view_offset_y),
                    (layer.offset.0 as f32, layer.offset.1 as f32),
                    layer.id as usize,
                    Self::mask_atlas_layer_id(layer.id),
                )
            };

            let tp = self
                .transform_previews
                .iter()
                .find(|t| t.layer_id == layer.id);
            let cp = self.crop_preview.as_ref();
            let is_transform = tp.is_some() || cp.is_some();

            let (
                xform_active,
                inv_a,
                inv_b,
                inv_c,
                inv_d,
                pivot_x,
                pivot_y,
                xform_tx,
                xform_ty,
                orig_ox,
                orig_oy,
                orig_w,
                orig_h,
            ) = if let Some(t) = tp {
                (
                    2u32, t.inv_m[0], t.inv_m[1], t.inv_m[2], t.inv_m[3], t.inv_m[4], t.inv_m[5],
                    t.inv_m[6], t.inv_m[7], t.inv_m[8], t.orig_ox, t.orig_oy, 0.0,
                )
            } else if let Some(crop) = cp {
                (
                    1u32,
                    crop.inv_a,
                    crop.inv_b,
                    crop.inv_c,
                    crop.inv_d,
                    crop.pivot_x,
                    crop.pivot_y,
                    crop.tx,
                    crop.ty,
                    layer.offset.0 as f32,
                    layer.offset.1 as f32,
                    layer.width as f32,
                    layer.height as f32,
                )
            } else {
                (
                    0u32, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
                )
            };

            let is_adjustment_layer = matches!(
                layer.layer_type,
                crate::core::layer::LayerType::Adjustment(_)
            );
            let (mut adj_kind, mut adj_p, mut adj_lut) = match &layer.layer_type {
                crate::core::layer::LayerType::Adjustment(adj) => adjustment_to_gpu(adj),
                _ => (0u32, [0.0f32; 12], [0.0f32; 768]),
            };
            // Develop: whether the tone stage runs (drives the shader's pad flag).
            let mut dev_tone_active = 0u32;
            // Scene-referred session flag (u.adj_pad_c): the shader samples the
            // f16 scene master and runs the scene chain instead of the atlas tone.
            let mut dev_scene_flag = 0u32;
            if !is_adjustment_layer {
                if let Some(preview) = &self.develop_preview {
                    if preview.layer_id == layer.id {
                        adj_kind = 20;
                        if let (Some(scene), true) = (&preview.scene, self.dev_scene_key != 0) {
                            let (p, lut) = develop_scene_to_gpu(
                                &preview.settings,
                                scene.look,
                                preview.region_luma.as_ref(),
                                preview.color.as_ref(),
                            );
                            adj_p = p;
                            adj_lut[..256].copy_from_slice(&lut);
                            dev_tone_active = 1;
                            dev_scene_flag = 1;
                        } else {
                            let (p, lut, _local_lut, tone_active) = develop_to_gpu(
                                &preview.settings,
                                preview.region_luma.as_ref(),
                                preview.color.as_ref(),
                            );
                            adj_p = p;
                            adj_lut[..256].copy_from_slice(&lut);
                            dev_tone_active = tone_active;
                        }
                    }
                } else if let Some((pid, adj)) = &self.preview_adj {
                    // Live Ctrl+L/M preview: the layer shader applies kinds 1..=13
                    // to this raster layer's own pixels (mutually exclusive with
                    // Develop, which the app cancels before starting one).
                    if *pid == layer.id {
                        let (k, p, lut) = adjustment_to_gpu(adj);
                        adj_kind = k;
                        adj_p = p;
                        adj_lut = lut;
                    }
                } else if let Some(preview) = &self.preview_filter {
                    if preview.layer_id == layer.id {
                        let (k, p) = filter_to_gpu(&preview.filter);
                        adj_kind = k;
                        adj_p = p;
                    }
                }
            }

            // Live clip pin: a clipping-mask / PowerClip child skips its clip
            // re-bake while dragged/transformed (the per-frame re-bake was the Move
            // lag), so its mask still sits at the bake position. Feed the shader the
            // pin delta (Δcontent − Δframe) so it samples the mask at the
            // canvas-fixed spot instead — the image stays clipped inside the frame
            // live, through Move AND Free Transform. Naturally zero shift when the
            // mask is fresh; the bias encoding still flags it as a clip child so the
            // transform branches pin too.
            let clip_shift_eff: Option<(i32, i32)> =
                match (layer.clip_parent_id, layer.mask.as_ref()) {
                    (Some(frame_id), Some(m)) if m.enabled => {
                        // Frame's CURRENT offset (live during a frame drag). If the
                        // frame isn't in this composited stack (rare region composite),
                        // fall back to its bake offset → Δframe = 0 → content-only pin.
                        let frame_now = layer_stack
                            .layers
                            .iter()
                            .find(|l| l.id == frame_id)
                            .map(|f| f.offset)
                            .unwrap_or(m.bake_frame_offset);
                        let dx = (layer.offset.0 - m.bake_offset.0)
                            - (frame_now.0 - m.bake_frame_offset.0);
                        let dy = (layer.offset.1 - m.bake_offset.1)
                            - (frame_now.1 - m.bake_frame_offset.1);
                        // The normal (non-transform) path composites through a LOD proxy
                        // when one is engaged, so its layer_x is downscaled — scale the
                        // pin to match. The transform branches sample at full-res canvas
                        // coordinates, so they keep the full-res shift.
                        let scale = if xform_active == 0 {
                            proxy.map_or(1, |p| 1i32 << p.level)
                        } else {
                            1
                        };
                        Some((dx / scale, dy / scale))
                    }
                    _ => None,
                };
            let clip_shift_packed = clip_shift_eff.map_or(0, |(dx, dy)| pack_clip_shift(dx, dy));
            // The shader samples the clip mask at `layer_local + shift`, so the mask
            // tiles it reads are those the layer would show if it sat at
            // `offset − shift`. Gather the mask against THAT offset, or the shifted
            // sample lands on a tile that was never uploaded (slot -1 = hidden) and
            // the base bleeds through the moving image. (Transform passes gather the
            // whole tileset, so they are unaffected.)
            let eff_off_mask = match clip_shift_eff {
                Some((sx, sy)) => (eff_off.0 - sx as f32, eff_off.1 - sy as f32),
                None => eff_off,
            };

            let uniform = CompositorUniformsData {
                opacity: layer.opacity,
                blend_mode: Self::blend_mode_to_u32(&layer.blend_mode),
                offset_x: eff_off.0,
                offset_y: eff_off.1,
                zoom: eff_zoom,
                view_offset_x: eff_view_off.0,
                view_offset_y: eff_view_off.1,
                viewport_w: self.viewport_w as f32,
                viewport_h: self.viewport_h as f32,
                layer_tiles_w: eff_tiles_w,
                layer_tiles_h: eff_tiles_h,
                layer_w: eff_w as f32,
                layer_h: eff_h as f32,
                xform_active,
                xform_inv_a: inv_a,
                xform_inv_b: inv_b,
                xform_inv_c: inv_c,
                xform_inv_d: inv_d,
                xform_pivot_x: pivot_x,
                xform_pivot_y: pivot_y,
                xform_tx,
                xform_ty,
                xform_orig_ox: orig_ox,
                xform_orig_oy: orig_oy,
                xform_orig_w: orig_w,
                xform_orig_h: orig_h,
                mask_enabled: u32::from(mask_present),
                mask_inverted: u32::from(layer.mask.as_ref().map(|m| m.inverted).unwrap_or(false)),
                adj_kind,
                _adj_pad_a: dev_tone_active,
                clip_shift_packed,
                _adj_pad_c: dev_scene_flag,
                adj_p,
                adj_lut,
            };
            queue.write_buffer(
                &self.layer_uniform_bufs[layer_slot],
                0,
                bytemuck::bytes_of(&uniform),
            );

            // Full-region scissor: the clamped dirty rect on the partial path, or
            // none for a full recompose (the fullscreen pass writes every pixel).
            let scissor = if use_partial {
                dirty_rect.and_then(|(sx, sy, sw, sh)| {
                    let sx = sx.min(self.viewport_w);
                    let sy = sy.min(self.viewport_h);
                    let sw = sw.min(self.viewport_w.saturating_sub(sx));
                    let sh = sh.min(self.viewport_h.saturating_sub(sy));
                    if sw > 0 && sh > 0 {
                        Some((sx, sy, sw, sh))
                    } else {
                        None
                    }
                })
            } else {
                None
            };

            // Output region this layer's pass covers (partial → clamped dirty rect,
            // else full viewport). `src_*` are u32 casts, so this is integer-exact.
            let region = (
                src_sx0 as u32,
                src_sy0 as u32,
                (src_sx1 - src_sx0) as u32,
                (src_sy1 - src_sy0) as u32,
            );
            // Tiles this layer (+ mask) makes visible this frame. When that exceeds
            // the atlas, a single render pass would evict slots it still samples
            // (scramble); split the output into blocks that each fit. Transform
            // previews gather every tile regardless of the scissor, so they are never
            // banded — kept as one full-region pass (a pre-existing, rare gap).
            let visible_tiles = if is_transform || region.2 == 0 || region.3 == 0 {
                0
            } else {
                let band = (src_sx0, src_sy0, src_sx1, src_sy1);
                let layer_slots = Self::visible_tile_slots_for_rect(
                    eff_tiles,
                    band,
                    eff_zoom,
                    eff_view_off,
                    eff_off,
                    eff_w,
                    eff_h,
                    eff_tiles_w,
                    eff_tiles_h,
                );
                let mask_slots = eff_mask_tiles.map_or(0, |mask_tiles| {
                    Self::visible_tile_slots_for_rect(
                        mask_tiles,
                        band,
                        eff_zoom,
                        eff_view_off,
                        eff_off_mask,
                        eff_w,
                        eff_h,
                        eff_tiles_w,
                        eff_tiles_h,
                    )
                });
                layer_slots + mask_slots
            };
            if !is_adjustment_layer && !is_transform && visible_tiles == 0 {
                continue;
            }
            // Overflow-guard charge. Transform/crop passes gather the whole tileset
            // regardless of the band (they are never banded), so `visible_tiles`
            // (0 for them) under-counts the atlas slots they touch. Charge the full
            // grid so a transform layer still flushes the prior batch before its
            // uploads can evict slots those pending passes sample.
            let budget_tiles = if is_transform {
                eff_total_tiles * (1 + usize::from(mask_present))
            } else {
                visible_tiles
            };
            if batch_touched > 0
                && batch_touched + budget_tiles > atlas_slot_count
                && !command_buffers.is_empty()
            {
                queue.submit(command_buffers.drain(..));
                batch_touched = 0;
            }
            batch_touched += budget_tiles;

            let needs_banding = visible_tiles > atlas_slot_count;
            let bands = if needs_banding {
                let m = Self::overflow_band_px(atlas_slot_count, mask_present, zoom);
                Self::tile_region(region, m, m)
            } else {
                vec![region]
            };
            let is_banded = bands.len() > 1;
            // Banded: flush the boundary snapshot + prior layers so their atlas slots
            // are evict-safe before the first block uploads (the sub-layer analogue
            // of the multi-layer flush above).
            if is_banded && !command_buffers.is_empty() {
                queue.submit(command_buffers.drain(..));
                batch_touched = 0;
            }

            for &(bx, by, bw, bh) in &bands {
                let band_src = (bx as f32, by as f32, (bx + bw) as f32, (by + bh) as f32);
                Self::gather_tiles_for_rect(
                    &mut self.tile_atlas,
                    queue,
                    &mut self.tile_map_scratch,
                    eff_atlas_id,
                    eff_tiles,
                    eff_total_tiles,
                    eff_tiles_w,
                    eff_tiles_h,
                    is_transform,
                    band_src,
                    eff_zoom,
                    eff_view_off,
                    eff_off,
                    eff_w,
                    eff_h,
                );
                if let Some(mask_tiles) = eff_mask_tiles {
                    Self::gather_tiles_for_rect(
                        &mut self.tile_atlas,
                        queue,
                        &mut self.mask_tile_map_scratch,
                        eff_mask_atlas_id,
                        mask_tiles,
                        eff_total_tiles,
                        eff_tiles_w,
                        eff_tiles_h,
                        is_transform,
                        band_src,
                        eff_zoom,
                        eff_view_off,
                        eff_off_mask,
                        eff_w,
                        eff_h,
                    );
                } else {
                    self.mask_tile_map_scratch[..eff_total_tiles].fill(-1);
                }

                queue.write_buffer(
                    &self.layer_tile_map_bufs[layer_slot],
                    0,
                    bytemuck::cast_slice(&self.tile_map_scratch[..eff_total_tiles]),
                );
                queue.write_buffer(
                    &self.layer_mask_tile_map_bufs[layer_slot],
                    0,
                    bytemuck::cast_slice(&self.mask_tile_map_scratch[..eff_total_tiles]),
                );

                let (dst_view_render, dst_bg_read) = if current_dst_is_ping {
                    (&self.pong_view, &self.ping_bg)
                } else {
                    (&self.ping_view, &self.pong_bg)
                };
                // A banded layer scissors each pass to its block; the non-banded path
                // keeps the original scissor (dirty rect, or none).
                let band_scissor = if is_banded {
                    Some((bx, by, bw, bh))
                } else {
                    scissor
                };

                let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("layer_comp_enc"),
                });

                if first_layer && !use_partial {
                    let rpass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("full_clear_ping"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &self.ping_view,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        ..Default::default()
                    });
                    drop(rpass);
                    self.ping_initialized = true;
                }
                first_layer = false;

                {
                    let mut rpass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("composite_pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: dst_view_render,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                // `first_layer` is already false here (the
                                // full-clear pass above flipped it), and the pass
                                // draws a fullscreen triangle that writes every
                                // pixel — Load is correct for both full and
                                // partial composites.
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        ..Default::default()
                    });

                    if let Some((sx, sy, sw, sh)) = band_scissor {
                        rpass.set_scissor_rect(sx, sy, sw, sh);
                    }

                    if is_adjustment_layer && adj_kind > 0 {
                        rpass.set_pipeline(&self.adjustment_pipeline);
                    } else {
                        rpass.set_pipeline(&self.pipeline);
                    }
                    rpass.set_bind_group(0, &self.tile_atlas.bind_group, &[]);
                    rpass.set_bind_group(1, dst_bg_read, &[]);
                    rpass.set_bind_group(2, &self.layer_uniform_bgs[layer_slot], &[]);
                    rpass.draw(0..3, 0..1);
                }

                command_buffers.push(enc.finish());

                // Banded: submit now so this block's slots are evict-safe before the
                // next block uploads (each tile-map write flushes at its own submit,
                // after the previous block's pass has read the buffer).
                if is_banded {
                    queue.submit(command_buffers.drain(..));
                }
            }
            if is_banded {
                batch_touched = 0;
            }

            // Parity flips once per layer: all bands share src/dst and tile disjoint
            // output regions, so together they equal a single full-layer pass.
            current_dst_is_ping = !current_dst_is_ping;
        }

        if first_layer && !use_partial {
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("empty_clear"),
            });
            {
                let rpass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("empty_clear_ping"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.ping_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    ..Default::default()
                });
                drop(rpass);
            }
            queue.submit(std::iter::once(enc.finish()));
            self.ping_initialized = true;
        }

        // Commit the snapshot's validity metadata (the pixels + parity were
        // captured inside the loop). On a hit nothing was re-snapshotted, so the
        // existing metadata still describes the backdrop.
        if cache_enabled && !cache_hit {
            self.backdrop_valid = true;
            self.backdrop_boundary = boundary;
            self.backdrop_sig = cur_sig;
            self.backdrop_vp = vp_sig;
            // A partial snapshot is valid only inside its scissor; a full-path
            // snapshot covers the whole viewport.
            self.backdrop_scissor = if use_partial { clamped_scissor } else { None };
        }

        if !command_buffers.is_empty() {
            queue.submit(command_buffers.drain(..));
        }

        self.last_result_is_ping = current_dst_is_ping;
        current_dst_is_ping
    }
}

#[cfg(test)]
mod shader_tests {
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

    #[test]
    fn shaders_are_valid_wgsl() {
        validate("compositor", super::COMPOSITOR_SHADER);
        validate("adjustment", super::ADJUSTMENT_SHADER);
    }

    /// Mirror of the shader's `clip_is_child()` + `clip_shift()`: 0 means "not a
    /// clip child"; otherwise each half is the i16 delta biased by 0x8000. Keep in
    /// lockstep — a clip mask drifting off its frame would be this encoding
    /// breaking, not the shader.
    fn unpack_clip_shift(packed: u32) -> Option<(i32, i32)> {
        if packed == 0 {
            return None;
        }
        let dx = (packed >> 16) as i32 - 0x8000;
        let dy = (packed & 0xFFFF) as i32 - 0x8000;
        Some((dx, dy))
    }

    #[test]
    fn clip_shift_packs_and_unpacks_signed_deltas() {
        for (dx, dy) in [
            (0, 0),
            (1, -1),
            (37, 512),
            (-250, -3000),
            (32767, -32767),
            (-32767, 32767),
        ] {
            let packed = super::pack_clip_shift(dx, dy);
            // A clip child is ALWAYS flagged non-zero, even at zero shift, so the
            // transform branches know to pin (a zero word means "not a clip child").
            assert_ne!(packed, 0, "clip child must stay flagged for ({dx}, {dy})");
            assert_eq!(
                unpack_clip_shift(packed),
                Some((dx, dy)),
                "round-trip failed for ({dx}, {dy})"
            );
        }
        // Exactly 0 is the reserved "not a clip child" value.
        assert_eq!(unpack_clip_shift(0), None);
        // Out-of-range deltas clamp instead of wrapping to a bogus small shift.
        assert_eq!(
            unpack_clip_shift(super::pack_clip_shift(90_000, -90_000)),
            Some((32767, -32767))
        );
    }

    #[test]
    fn compositor_converts_srgb_texture_samples_for_byte_space_edits() {
        let shader = super::COMPOSITOR_SHADER;
        assert!(
            shader.contains("let srgb = dev_linear_to_srgb(src.rgb);"),
            "sRGB atlas samples must be converted back to byte-space before Develop/adjustment math"
        );
        assert!(
            shader.contains("src = vec4<f32>(dev_srgb_to_linear"),
            "edited byte-space colours must return to linear before sRGB render-target output"
        );
        assert!(
            shader.contains("let src_rgb = dev_linear_to_srgb(src.rgb);")
                && shader.contains("let dst_rgb = dev_linear_to_srgb(dst.rgb);"),
            "layer blending must run in the RGB document space, not on decoded linear samples"
        );
    }

    #[test]
    fn dissolve_has_a_distinct_gpu_mode() {
        assert_eq!(
            super::CompositorState::blend_mode_to_u32(&crate::core::layer::BlendMode::Dissolve),
            17
        );
        assert!(super::COMPOSITOR_SHADER.contains("dissolve_hash("));
    }

    #[test]
    fn master_only_levels_stays_analytic_kind3() {
        let mut channels = [crate::core::layer::LevelsParams::default(); 4];
        channels[0].in_black = 30;
        channels[0].gamma = 1.4;
        let (kind, p, _) =
            super::adjustment_to_gpu(&crate::core::layer::AdjustmentType::Levels { channels });
        assert_eq!(kind, 3, "master-only Levels keeps the exact analytic path");
        assert_eq!(p[0], 30.0);
    }

    #[test]
    fn per_channel_levels_lut_matches_cpu_apply() {
        let mut channels = [crate::core::layer::LevelsParams::default(); 4];
        channels[0].gamma = 1.3;
        channels[1].in_black = 40;
        channels[2].out_white = 220;
        channels[3].in_white = 200;
        let adj = crate::core::layer::AdjustmentType::Levels { channels };

        let (kind, _, lut) = super::adjustment_to_gpu(&adj);
        assert_eq!(kind, 13);

        // Every LUT sample must reproduce the CPU per-pixel result exactly
        // (both run levels_eval master∘channel on the same f32 input).
        for i in 0..256u32 {
            let v = i as u8;
            let (r, g, b, _) = adj.apply_pixel(v, v, v, 255);
            let lr = (lut[i as usize] * 255.0).round() as u8;
            let lg = (lut[256 + i as usize] * 255.0).round() as u8;
            let lb = (lut[512 + i as usize] * 255.0).round() as u8;
            assert_eq!((lr, lg, lb), (r, g, b), "levels LUT diverged at {i}");
        }
    }

    #[test]
    fn per_channel_curves_lut_matches_cpu_apply() {
        let mut channels: [Vec<(f32, f32)>; 4] =
            std::array::from_fn(|_| crate::core::layer::identity_curve());
        channels[0] = vec![(0.0, 0.0), (0.5, 0.42), (1.0, 1.0)];
        channels[2] = vec![(0.0, 0.05), (0.6, 0.7), (1.0, 0.95)];
        let adj = crate::core::layer::AdjustmentType::Curves { channels };

        let (kind, _, lut) = super::adjustment_to_gpu(&adj);
        assert_eq!(kind, 13);

        for i in 0..256u32 {
            let v = i as u8;
            let (r, g, b, _) = adj.apply_pixel(v, v, v, 255);
            let lr = (lut[i as usize] * 255.0).round() as u8;
            let lg = (lut[256 + i as usize] * 255.0).round() as u8;
            let lb = (lut[512 + i as usize] * 255.0).round() as u8;
            assert_eq!((lr, lg, lb), (r, g, b), "curves LUT diverged at {i}");
        }
    }

    #[test]
    fn both_shaders_carry_kind13_and_768_lut() {
        for shader in [super::COMPOSITOR_SHADER, super::ADJUSTMENT_SHADER] {
            assert!(
                shader.contains("array<vec4<f32>, 192>"),
                "shader LUT bank must hold 3×256 floats"
            );
            assert!(shader.contains("case 13u:"), "kind 13 missing");
            assert!(
                !shader.contains("// Curves: 256-entry tone LUT"),
                "dead single-LUT Curves kind (old case 4u in apply_adjustment) must stay deleted"
            );
        }
    }

    #[test]
    fn gradient_map_adjustment_is_gpu_backed() {
        let adj = crate::core::layer::AdjustmentType::GradientMap {
            stops: vec![(0.0, [255, 0, 0]), (1.0, [0, 0, 255])],
            reverse: true,
            dither: false,
        };

        let (kind, params, lut) = super::adjustment_to_gpu(&adj);

        assert_eq!(kind, 12);
        assert_eq!(params[0], 1.0);
        assert_eq!(params[1], 0.0);
        assert_eq!(&lut[0..4], &[1.0, 0.0, 0.0, 1.0]);
        assert_eq!(&lut[252..256], &[0.0, 0.0, 1.0, 1.0]);
    }
}

/// Backdrop-cache logic that doesn't need a GPU device: the cut-point
/// computation and the per-layer fingerprint's change-detection. The
/// snapshot/resume pixel path (parity, copy_texture_to_texture) still needs a
/// live device and is verified by GUI smoke-testing.
#[cfg(test)]
mod backdrop_tests {
    use super::CompositorState;
    use crate::core::layer::{AdjustmentType, BlendMode, Layer, LayerStack};
    use crate::core::tile::TileMap;

    #[test]
    fn boundary_counts_visible_layers_below_active() {
        let mut stack = LayerStack::new(4, 4); // idx0 = Background
        let l1 = stack.add_layer(4, 4); // idx1
        stack.layers[l1].tiles = TileMap::new_solid(4, 4, 255, 0, 0, 255);
        let l2 = stack.add_layer(4, 4); // idx2
        stack.layers[l2].tiles = TileMap::new_solid(4, 4, 0, 255, 0, 255);
        let l3 = stack.add_layer(4, 4); // idx3
        stack.layers[l3].tiles = TileMap::new_solid(4, 4, 0, 0, 255, 255);

        stack.active_idx = 3;
        assert_eq!(CompositorState::visible_layers_and_boundary(&stack).1, 3);

        stack.active_idx = 1;
        assert_eq!(CompositorState::visible_layers_and_boundary(&stack).1, 1);

        stack.active_idx = 0;
        assert_eq!(CompositorState::visible_layers_and_boundary(&stack).1, 0);

        // A hidden layer below the active one drops out of the frozen prefix.
        stack.layers[0].visible = false;
        stack.active_idx = 2;
        assert_eq!(CompositorState::visible_layers_and_boundary(&stack).1, 1);
    }

    #[test]
    fn boundary_skips_empty_raster_layers() {
        let mut stack = LayerStack::new(4, 4); // Background is renderable.
        stack.add_layer(4, 4); // Empty raster layer.

        stack.active_idx = 1;
        let (visible, boundary) = CompositorState::visible_layers_and_boundary(&stack);

        assert_eq!(visible.len(), 1);
        assert_eq!(boundary, 1);
    }

    #[test]
    fn solid_tilemap_counts_as_one_visible_atlas_slot() {
        let tiles = TileMap::new_solid(1024, 1024, 255, 255, 255, 255);
        let slots = CompositorState::visible_tile_slots_for_rect(
            &tiles,
            (0.0, 0.0, 512.0, 512.0),
            0.5,
            (0.0, 0.0),
            (0.0, 0.0),
            1024,
            1024,
            4,
            4,
        );

        assert_eq!(slots, 1);
    }

    #[test]
    fn tiles_outside_band_yield_zero_slots() {
        // A layer whose content sits far outside the dirty band contributes no
        // atlas slots. The compositor's `visible_tiles == 0` skip relies on this,
        // and the partial-path parity precount must exclude such layers to keep
        // the ping-pong count exact.
        let tiles = TileMap::new_solid(512, 512, 255, 255, 255, 255);
        let slots = CompositorState::visible_tile_slots_for_rect(
            &tiles,
            (0.0, 0.0, 100.0, 100.0),
            1.0,
            (0.0, 0.0),
            (2000.0, 2000.0),
            512,
            512,
            2,
            2,
        );

        assert_eq!(slots, 0);
    }

    #[test]
    fn proxy_atlas_ids_never_collide() {
        // Proxy tile keys must not alias full-res tiles, masks, other proxy
        // levels, or proxy masks — else the atlas would serve stale pixels across
        // the zoom-in/zoom-out boundary.
        let full = 7usize;
        let mask = CompositorState::mask_atlas_layer_id(7);
        let p1 = CompositorState::proxy_atlas_layer_id(7, 1);
        let p2 = CompositorState::proxy_atlas_layer_id(7, 2);
        let pm1 = CompositorState::proxy_mask_atlas_layer_id(7, 1);
        let ids = [full, mask, p1, p2, pm1];
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                assert_ne!(ids[i], ids[j], "atlas id {i} vs {j} collide");
            }
        }
        // Different layers stay distinct at the same level.
        assert_ne!(
            CompositorState::proxy_atlas_layer_id(7, 3),
            CompositorState::proxy_atlas_layer_id(8, 3)
        );
    }

    #[test]
    fn proxy_rescale_selects_same_screen_tiles_as_full_res() {
        // The LOD trick: composite a proxy reduced by S with the view rescaled
        // (zoom·S, view/S, off/S) and the shader maps each output pixel to
        // `layer_x_full / S`. So `tile_span_for_rect` on the proxy must pick
        // exactly the proxy tiles covering the same screen band as the full-res
        // tiles cover — i.e. each span endpoint scaled down by S.
        let s = 4u32; // level 2
        for &band in &[
            (0.0f32, 0.0f32, 1024.0f32, 1024.0f32),
            (256.0, 256.0, 768.0, 768.0),
        ] {
            let full = CompositorState::tile_span_for_rect(
                band,
                0.25,
                (0.0, 0.0),
                (0.0, 0.0),
                4096,
                4096,
                16,
                16,
            )
            .expect("full span");
            let proxy = CompositorState::tile_span_for_rect(
                band,
                0.25 * s as f32,
                (0.0, 0.0),
                (0.0, 0.0),
                4096 / s,
                4096 / s,
                16 / s,
                16 / s,
            )
            .expect("proxy span");
            assert_eq!(proxy.0, full.0 / s, "tx0 for band {band:?}");
            assert_eq!(proxy.1, full.1 / s, "ty0 for band {band:?}");
            assert_eq!(proxy.2, full.2 / s, "tx1 for band {band:?}");
            assert_eq!(proxy.3, full.3 / s, "ty1 for band {band:?}");
        }
    }

    #[test]
    fn build_proxy_level_reduces_dims_by_power_of_two() {
        let base = TileMap::new_solid(4096, 2048, 200, 100, 50, 255);
        // Level 3 → S = 8.
        let (tiles, mask) = CompositorState::build_proxy_level(&base, None, 3);
        assert_eq!((tiles.width, tiles.height), (512, 256));
        assert!(mask.is_none());
        // Uniform colour survives the chained box filter.
        assert_eq!(tiles.get_pixel(0, 0), (200, 100, 50, 255));
    }

    #[test]
    fn proxy_lru_evicts_oldest_and_protects_current_frame() {
        use super::ProxyEntry;
        use std::collections::HashMap;

        let mk = |bytes: usize, last_used: u64| ProxyEntry {
            src_fp: 0,
            mask_fp: 0,
            tiles: TileMap::new(1, 1),
            mask: None,
            bytes,
            last_used,
        };
        let mut map: HashMap<(u32, u32), ProxyEntry> = HashMap::new();
        map.insert((1, 1), mk(40, 1)); // oldest
        map.insert((2, 1), mk(40, 2));
        map.insert((3, 1), mk(40, 9)); // current frame
        map.insert((4, 1), mk(40, 9)); // current frame

        // Cap 100 with 160 cached → must drop the two oldest non-current entries.
        CompositorState::evict_proxies(&mut map, 100, 9);
        assert!(!map.contains_key(&(1, 1)), "oldest evicted first");
        assert!(!map.contains_key(&(2, 1)), "next-oldest evicted");
        assert!(map.contains_key(&(3, 1)) && map.contains_key(&(4, 1)));
        assert!(map.values().map(|e| e.bytes).sum::<usize>() <= 100);
    }

    #[test]
    fn proxy_lru_overshoots_rather_than_evict_live_frame() {
        use super::ProxyEntry;
        use std::collections::HashMap;

        let mk = |bytes: usize, last_used: u64| ProxyEntry {
            src_fp: 0,
            mask_fp: 0,
            tiles: TileMap::new(1, 1),
            mask: None,
            bytes,
            last_used,
        };
        let mut map: HashMap<(u32, u32), ProxyEntry> = HashMap::new();
        map.insert((1, 1), mk(200, 5));
        map.insert((2, 1), mk(200, 5));
        // Both live this frame and together exceed the cap: nothing evictable, so
        // the cache transiently overshoots instead of dropping a needed proxy.
        CompositorState::evict_proxies(&mut map, 100, 5);
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn fingerprint_stable_when_unchanged() {
        let base = Layer::new(1, "a", 8, 8);
        assert_eq!(
            CompositorState::layer_fingerprint(&base),
            CompositorState::layer_fingerprint(&base.clone())
        );
    }

    #[test]
    fn fingerprint_detects_property_edits() {
        let base = Layer::new(1, "a", 8, 8);
        let fp = CompositorState::layer_fingerprint(&base);

        let mut op = base.clone();
        op.opacity = 0.5;
        assert_ne!(fp, CompositorState::layer_fingerprint(&op));

        let mut bl = base.clone();
        bl.blend_mode = BlendMode::Multiply;
        assert_ne!(fp, CompositorState::layer_fingerprint(&bl));

        let mut off = base.clone();
        off.offset = (3, -2);
        assert_ne!(fp, CompositorState::layer_fingerprint(&off));

        let mut vis = base.clone();
        vis.visible = false;
        assert_ne!(fp, CompositorState::layer_fingerprint(&vis));
    }

    #[test]
    fn fingerprint_detects_adjustment_param_change() {
        // Adjustment layers carry no tiles, so params must be folded in directly.
        let a0 = Layer::new_adjustment(
            2,
            AdjustmentType::BrightnessContrast {
                brightness: 0.0,
                contrast: 0.0,
            },
            8,
            8,
        );
        let a1 = Layer::new_adjustment(
            2,
            AdjustmentType::BrightnessContrast {
                brightness: 20.0,
                contrast: 0.0,
            },
            8,
            8,
        );
        assert_ne!(
            CompositorState::layer_fingerprint(&a0),
            CompositorState::layer_fingerprint(&a1)
        );
    }

    #[test]
    fn fingerprint_detects_pixel_edit_via_revision_bump() {
        // Pixel edits bump tile revisions; the fingerprint keys on revisions, so
        // a bump (what any real edit does) must change it.
        let mut px = Layer::from_rgba(3, "c", vec![128u8; 8 * 8 * 4], 8, 8);
        let before = CompositorState::layer_fingerprint(&px);
        px.tiles.bump_all_revisions();
        assert_ne!(before, CompositorState::layer_fingerprint(&px));
    }

    #[test]
    fn rect_contains_covers_the_partial_snapshot_cases() {
        // A partial snapshot (`outer`) may only serve a resume whose scissor
        // (`inner`) is fully inside it — the Phase 3 `scissor_ok` predicate.
        let outer = (10u32, 20u32, 100u32, 80u32); // x∈[10,110), y∈[20,100)

        // Identical region and a strictly-interior region are covered.
        assert!(CompositorState::rect_contains(outer, outer));
        assert!(CompositorState::rect_contains(outer, (30, 40, 20, 20)));
        // Flush against the far corner (inner right/bottom == outer right/bottom).
        assert!(CompositorState::rect_contains(outer, (60, 60, 50, 40)));

        // Origin before the snapshot, or extent past its right/bottom edge, misses.
        assert!(!CompositorState::rect_contains(outer, (5, 40, 20, 20)));
        assert!(!CompositorState::rect_contains(outer, (30, 15, 20, 20)));
        assert!(!CompositorState::rect_contains(outer, (100, 40, 20, 20)));
        assert!(!CompositorState::rect_contains(outer, (30, 90, 20, 20)));
        // An inner larger than the outer in either axis is never contained.
        assert!(!CompositorState::rect_contains(outer, (10, 20, 101, 80)));
    }

    #[test]
    fn tile_region_tiles_cover_exactly_once() {
        let region = (3u32, 5u32, 40u32, 30u32);
        let (rx, ry, rw, rh) = region;
        for &(mw, mh) in &[(13u32, 9u32), (1, 1), (40, 30), (100, 100), (7, 50)] {
            let blocks = CompositorState::tile_region(region, mw, mh);
            let mut covered = vec![0u32; (rw * rh) as usize];
            for &(bx, by, bw, bh) in &blocks {
                assert!(bw >= 1 && bh >= 1, "empty block for max {mw}x{mh}");
                assert!(bw <= mw && bh <= mh, "block exceeds max {mw}x{mh}");
                assert!(
                    bx >= rx && by >= ry && bx + bw <= rx + rw && by + bh <= ry + rh,
                    "block escapes region for max {mw}x{mh}"
                );
                for yy in by..by + bh {
                    for xx in bx..bx + bw {
                        covered[((yy - ry) * rw + (xx - rx)) as usize] += 1;
                    }
                }
            }
            assert!(
                covered.iter().all(|&c| c == 1),
                "every pixel covered exactly once for max {mw}x{mh}"
            );
        }
    }

    #[test]
    fn tile_region_empty_region_yields_no_blocks() {
        assert!(CompositorState::tile_region((0, 0, 0, 10), 8, 8).is_empty());
        assert!(CompositorState::tile_region((0, 0, 10, 0), 8, 8).is_empty());
    }

    #[test]
    fn overflow_band_px_blocks_fit_atlas() {
        // A block sized by overflow_band_px must gather no more atlas slots (layer +
        // mask) than the atlas holds, at any sub-tile offset — the guarantee that
        // makes tiled multi-pass banding correct. `tiles_in_span` mirrors the gather
        // bounds (floor on the near edge, ceil on the far edge) without the layer-size
        // clamp, i.e. the upper bound on tiles a block's pass can touch.
        fn tiles_in_span(a: f32, len: f32) -> u32 {
            let t0 = (a.floor().max(0.0) as u32) / 256;
            let t1 = ((a + len).ceil().max(0.0) as u32) / 256;
            t1 - t0 + 1
        }
        let offsets = [
            0.0f32, 0.1, 0.5, 0.9, 127.3, 255.9, 256.0, 256.1, 383.7, 511.9,
        ];
        for &slot_count in &[16usize, 64, 256, 512, 1024] {
            for &mask in &[false, true] {
                for &zoom in &[1.0f32, 0.5, 0.2, 0.1, 0.05, 0.033] {
                    let m = CompositorState::overflow_band_px(slot_count, mask, zoom);
                    assert!(m >= 1, "band px must be >= 1");
                    let span = m as f32 / zoom;
                    for &ax in &offsets {
                        for &ay in &offsets {
                            let tiles = tiles_in_span(ax, span) as usize
                                * tiles_in_span(ay, span) as usize
                                * (1 + usize::from(mask));
                            assert!(
                                tiles <= slot_count,
                                "block {m}px @zoom {zoom} mask {mask} slots {slot_count}: \
                                 {tiles} tiles > atlas"
                            );
                        }
                    }
                }
            }
        }
    }
}

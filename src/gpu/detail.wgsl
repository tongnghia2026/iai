// GPU Detail — port of `core::develop::detail_core` (Camera Raw–style
// Sharpening, Luminance NR and Colour NR on a gamma-encoded luma/chroma split),
// so the live preview runs the same Detail as the commit.
//
// One pooled storage buffer `pool` holds every working plane at a fixed f32
// offset (in elements); each pass reads/writes regions of it by offset carried
// in the per-dispatch uniform `P`. Multi-tap / multi-scale work is a sequence
// of dispatches in one compute pass (read-after-write barriers between
// dispatches), mirroring the CPU's sequential planes.

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
    // GPU-resident tiles: source texel of tile pixel (0,0), the tile's core
    // (crop offset + size) and where that core lands in the output plane.
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
};

@group(0) @binding(0) var<storage, read_write> pool: array<f32>;
@group(0) @binding(1) var<uniform> P: PassParams;
// GPU-resident input plane (RGB in .rgb) and packed RGB output plane; host
// round-trip runs bind 1×1 placeholders.
@group(0) @binding(2) var src_tex: texture_2d<f32>;
@group(0) @binding(3) var<storage, read_write> out_rgb: array<f32>;

const FLAG_H: u32 = 1u;
const FLAG_FIRST: u32 = 2u;
const FLAG_FINE: u32 = 4u;
const FLAG_MASK: u32 = 8u;
const FLAG_HALO: u32 = 16u;

// Mirrors of the `detail_core` constants used per pixel.
const SH_LIMIT: f32 = 0.12;
const SH_SHADOW_KNEE: f32 = 0.276;
const SH_SHADOW_POW: f32 = 1.51;
const SH_HIGHLIGHT_CUT: f32 = 0.505;
const SH_HALO_MARGIN: f32 = 0.090;
const NR_HIGHLIGHT_CUT: f32 = 0.5;
const CNR_HIGHLIGHT_CUT: f32 = 0.25;
const CNR_SPECK_SPREAD: f32 = 1.5;
const CNR_SPECK_FLOOR: f32 = 0.01;
const CNR_SPECK_WIDTH: f32 = 0.03;

fn linear_index(gid: vec3<u32>) -> u32 {
    return gid.y * P.groups_x * 64u + gid.x;
}

fn smooth01(x: f32) -> f32 {
    let t = clamp(x, 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn b3(t: i32) -> f32 {
    switch t {
        case 0, 4: { return 1.0 / 16.0; }
        case 1, 3: { return 4.0 / 16.0; }
        default: { return 6.0 / 16.0; }
    }
}

fn encode_channel(v: f32) -> f32 {
    let a = abs(v);
    var e: f32;
    if (a <= 0.0031308) {
        e = 12.92 * a;
    } else {
        e = 1.055 * pow(a, 1.0 / 2.4) - 0.055;
    }
    return select(e, -e, v < 0.0);
}

fn decode_channel(e: f32) -> f32 {
    let a = abs(e);
    var v: f32;
    if (a <= 0.04045) {
        v = a / 12.92;
    } else {
        v = pow((a + 0.055) / 1.055, 2.4);
    }
    return select(v, -v, e < 0.0);
}

// Tap index along the pass axis at hole offset `o`, edge-clamped.
fn tap(i: u32, o: i32, horizontal: bool) -> u32 {
    let w = i32(P.w);
    let h = i32(P.h);
    let x = i32(i % P.w);
    let y = i32(i / P.w);
    if (horizontal) {
        return u32(y * w + clamp(x + o, 0, w - 1));
    }
    return u32(clamp(y + o, 0, h - 1) * w + x);
}

// Resident input: copy this tile's texels into the pool's RGB image plane.
@compute @workgroup_size(64)
fn load_tex(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = linear_index(gid);
    if (i >= P.n) { return; }
    let x = i % P.w;
    let y = i / P.w;
    let c = textureLoad(src_tex, vec2<i32>(i32(P.tex_x0 + x), i32(P.tex_y0 + y)), 0);
    pool[P.img_off + i * 3u] = c.r;
    pool[P.img_off + i * 3u + 1u] = c.g;
    pool[P.img_off + i * 3u + 2u] = c.b;
}

// Resident output: write this tile's core (apron cropped) into the plane.
@compute @workgroup_size(64)
fn store_out(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = linear_index(gid);
    if (i >= P.n) { return; }
    let x = i % P.w;
    let y = i / P.w;
    if (x < P.crop_x || y < P.crop_y) { return; }
    let cx = x - P.crop_x;
    let cy = y - P.crop_y;
    if (cx >= P.core_w || cy >= P.core_h) { return; }
    let o = P.out_base + ((P.out_y0 + cy) * P.out_w + P.out_x0 + cx) * 3u;
    out_rgb[o] = pool[P.img_off + i * 3u];
    out_rgb[o + 1u] = pool[P.img_off + i * 3u + 1u];
    out_rgb[o + 2u] = pool[P.img_off + i * 3u + 2u];
}

// RGB → gamma-encoded luma + chroma offsets.
@compute @workgroup_size(64)
fn split(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = linear_index(gid);
    if (i >= P.n) { return; }
    var r = pool[P.img_off + i * 3u];
    var g = pool[P.img_off + i * 3u + 1u];
    var b = pool[P.img_off + i * 3u + 2u];
    if (P.linear != 0u) {
        r = encode_channel(r);
        g = encode_channel(g);
        b = encode_channel(b);
    }
    let l = P.lc0 * r + P.lc1 * g + P.lc2 * b;
    pool[P.luma_off + i] = l;
    pool[P.chroma_off + i * 3u] = r - l;
    pool[P.chroma_off + i * 3u + 1u] = g - l;
    pool[P.chroma_off + i * 3u + 2u] = b - l;
}

fn chroma_at(off: u32, j: u32) -> vec3<f32> {
    return vec3<f32>(pool[off + j * 3u], pool[off + j * 3u + 1u], pool[off + j * 3u + 2u]);
}

// Isolated chroma outlier → 8-neighbour mean, weighted by `atten` (src → dst).
@compute @workgroup_size(64)
fn cspeck(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = linear_index(gid);
    if (i >= P.n) { return; }
    let w = i32(P.w);
    let h = i32(P.h);
    let x = i32(i % P.w);
    let y = i32(i / P.w);
    let c = chroma_at(P.src_off, i);
    var mean = vec3<f32>(0.0);
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            if (dx == 0 && dy == 0) { continue; }
            let j = u32(clamp(y + dy, 0, h - 1) * w + clamp(x + dx, 0, w - 1));
            mean = mean + chroma_at(P.src_off, j) / 8.0;
        }
    }
    var spread = 0.0;
    var dmin = 3.4e38;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            if (dx == 0 && dy == 0) { continue; }
            let j = u32(clamp(y + dy, 0, h - 1) * w + clamp(x + dx, 0, w - 1));
            let v = chroma_at(P.src_off, j);
            let dm = v - mean;
            spread = spread + dot(dm, dm) / 8.0;
            let dc = v - c;
            dmin = min(dmin, dot(dc, dc));
        }
    }
    let excess = sqrt(dmin) - CNR_SPECK_SPREAD * sqrt(spread) - CNR_SPECK_FLOOR;
    let t = P.atten * smooth01(excess / CNR_SPECK_WIDTH);
    let o = c + t * (mean - c);
    pool[P.dst_off + i * 3u] = o.x;
    pool[P.dst_off + i * 3u + 1u] = o.y;
    pool[P.dst_off + i * 3u + 2u] = o.z;
}

// Joint-chroma à-trous pass (3-channel planes), range-weighted by the chroma
// vector distance from the centre.
@compute @workgroup_size(64)
fn catrous(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = linear_index(gid);
    if (i >= P.n) { return; }
    let horizontal = (P.flags & FLAG_H) != 0u;
    let step = 1 << P.level;
    let inv = -0.5 / (P.sigma * P.sigma);
    let c = vec3<f32>(pool[P.src_off + i * 3u], pool[P.src_off + i * 3u + 1u], pool[P.src_off + i * 3u + 2u]);
    var acc = vec3<f32>(0.0);
    var ws = 0.0;
    for (var t = 0; t < 5; t = t + 1) {
        let j = tap(i, (t - 2) * step, horizontal);
        let v = vec3<f32>(pool[P.src_off + j * 3u], pool[P.src_off + j * 3u + 1u], pool[P.src_off + j * 3u + 2u]);
        let d = v - c;
        let wt = b3(t) * exp(dot(d, d) * inv);
        acc = acc + v * wt;
        ws = ws + wt;
    }
    let o = acc / max(ws, 1e-12);
    pool[P.dst_off + i * 3u] = o.x;
    pool[P.dst_off + i * 3u + 1u] = o.y;
    pool[P.dst_off + i * 3u + 2u] = o.z;
}

// Colour-NR level accumulate: acc (+)= d·keep, d = a − b (3-channel).
@compute @workgroup_size(64)
fn caccum(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = linear_index(gid);
    if (i >= P.n) { return; }
    let d = vec3<f32>(
        pool[P.a_off + i * 3u] - pool[P.b_off + i * 3u],
        pool[P.a_off + i * 3u + 1u] - pool[P.b_off + i * 3u + 1u],
        pool[P.a_off + i * 3u + 2u] - pool[P.b_off + i * 3u + 2u],
    );
    var keep = 1.0;
    if (P.atten > 0.0) {
        let m = sqrt(dot(d, d)) / P.tau;
        let m2 = m * m;
        let prot = 1.0 / (1.0 + m2 * m2);
        let taper = 1.0 - CNR_HIGHLIGHT_CUT * smooth01((pool[P.luma_off + i] - 0.55) / 0.4);
        keep = 1.0 - min(P.atten * prot * taper, 1.0);
    }
    for (var ch = 0u; ch < 3u; ch = ch + 1u) {
        let prev = select(pool[P.acc_off + i * 3u + ch], 0.0, (P.flags & FLAG_FIRST) != 0u);
        pool[P.acc_off + i * 3u + ch] = prev + d[ch] * keep;
    }
}

// chroma = residual (a) + accumulated detail.
@compute @workgroup_size(64)
fn cfinish(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = linear_index(gid);
    if (i >= P.n) { return; }
    for (var ch = 0u; ch < 3u; ch = ch + 1u) {
        pool[P.chroma_off + i * 3u + ch] = pool[P.a_off + i * 3u + ch] + pool[P.acc_off + i * 3u + ch];
    }
}

// Plain à-trous B3 pass on one plane (horizontal with FLAG_H).
@compute @workgroup_size(64)
fn patrous(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = linear_index(gid);
    if (i >= P.n) { return; }
    let horizontal = (P.flags & FLAG_H) != 0u;
    let step = 1 << P.level;
    var acc = 0.0;
    for (var t = 0; t < 5; t = t + 1) {
        acc = acc + pool[P.src_off + tap(i, (t - 2) * step, horizontal)] * b3(t);
    }
    pool[P.dst_off + i] = acc;
}

// Luma-NR level accumulate: acc (+)= d·(1 − min(atten·taper·gate·prot, 1)),
// d = a − b; the gate fades the shrink where the smoothed plane b has real
// contrast, the protection where the coefficient itself is large.
@compute @workgroup_size(64)
fn laccum(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = linear_index(gid);
    if (i >= P.n) { return; }
    let st = 1u << P.level;
    let w = P.w;
    let h = P.h;
    let x = i % w;
    let y = i / w;
    let xp = y * w + min(x + st, w - 1u);
    let xm = y * w + select(0u, x - st, x >= st);
    let yp = min(y + st, h - 1u) * w + x;
    let ym = select(0u, y - st, y >= st) * w + x;
    let gx = (pool[P.b_off + xp] - pool[P.b_off + xm]) * 0.5;
    let gy = (pool[P.b_off + yp] - pool[P.b_off + ym]) * 0.5;
    let m = sqrt(gx * gx + gy * gy);
    let gate = 1.0 - smooth01((m - P.mask_lo) / max(P.mask_hi - P.mask_lo, 1e-9));
    let d = pool[P.a_off + i] - pool[P.b_off + i];
    let r = abs(d) / P.tau;
    let r2 = r * r;
    let prot = 1.0 / (1.0 + r2 * r2);
    let taper = 1.0 - NR_HIGHLIGHT_CUT * smooth01((pool[P.luma_off + i] - 0.75) / 0.15);
    let v = d * (1.0 - min(P.atten * taper * gate * prot, 1.0));
    let prev = select(pool[P.acc_off + i], 0.0, (P.flags & FLAG_FIRST) != 0u);
    pool[P.acc_off + i] = prev + v;
}

@compute @workgroup_size(64)
fn lfinish(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = linear_index(gid);
    if (i >= P.n) { return; }
    pool[P.luma_off + i] = pool[P.a_off + i] + pool[P.acc_off + i];
}

// One separable Gaussian pass (vertical, or horizontal with FLAG_H), radius
// `P.radius`, weights normalised in-shader exactly as the CPU taps.
@compute @workgroup_size(64)
fn gauss(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = linear_index(gid);
    if (i >= P.n) { return; }
    let horizontal = (P.flags & FLAG_H) != 0u;
    let r = i32(P.radius);
    var sum = 0.0;
    for (var x = -r; x <= r; x = x + 1) {
        sum = sum + exp(-0.5 * f32(x * x) / (P.sigma * P.sigma));
    }
    var acc = 0.0;
    for (var x = -r; x <= r; x = x + 1) {
        let k = exp(-0.5 * f32(x * x) / (P.sigma * P.sigma)) / sum;
        acc = acc + pool[P.src_off + tap(i, x, horizontal)] * k;
    }
    pool[P.dst_off + i] = acc;
}

// Separable local min (a → dst) and max (b → gdst) over ±radius.
@compute @workgroup_size(64)
fn minmax(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = linear_index(gid);
    if (i >= P.n) { return; }
    let horizontal = (P.flags & FLAG_H) != 0u;
    let r = i32(P.radius);
    var lo = 3.4e38;
    var hi = -3.4e38;
    for (var x = -r; x <= r; x = x + 1) {
        let j = tap(i, x, horizontal);
        lo = min(lo, pool[P.a_off + j]);
        hi = max(hi, pool[P.b_off + j]);
    }
    pool[P.dst_off + i] = lo;
    pool[P.gdst_off + i] = hi;
}

// Final sharpening: DoG boost (+ fine band), tone fade, edge mask, tanh limit,
// halo clamp. Writes the new luma in place (reads only this pixel of luma).
@compute @workgroup_size(64)
fn sharpen(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = linear_index(gid);
    if (i >= P.n) { return; }
    let l = pool[P.luma_off + i];
    var delta = P.k * (pool[P.ga_off + i] - pool[P.gb_off + i]);
    if ((P.flags & FLAG_FINE) != 0u) {
        delta = delta + P.k_fine * (l - pool[P.gf_off + i]);
    }
    let tone = pow(clamp(l / SH_SHADOW_KNEE, 0.0, 1.0), SH_SHADOW_POW)
        * (1.0 - SH_HIGHLIGHT_CUT * smooth01((l - 0.75) / 0.30));
    delta = delta * tone;
    if ((P.flags & FLAG_MASK) != 0u) {
        let w = P.w;
        let h = P.h;
        let x = i % w;
        let y = i / w;
        let m = P.gm_off;
        var gx = 0.0;
        if (w >= 2u) {
            if (x == 0u) {
                gx = pool[m + i + 1u] - pool[m + i];
            } else if (x == w - 1u) {
                gx = pool[m + i] - pool[m + i - 1u];
            } else {
                gx = (pool[m + i + 1u] - pool[m + i - 1u]) * 0.5;
            }
        }
        var gy = 0.0;
        if (h >= 2u) {
            if (y == 0u) {
                gy = pool[m + i + w] - pool[m + i];
            } else if (y == h - 1u) {
                gy = pool[m + i] - pool[m + i - w];
            } else {
                gy = (pool[m + i + w] - pool[m + i - w]) * 0.5;
            }
        }
        let g = sqrt(gx * gx + gy * gy);
        delta = delta * smooth01((g - P.mask_lo) / max(P.mask_hi - P.mask_lo, 1e-9));
    }
    delta = SH_LIMIT * tanh(delta / SH_LIMIT);
    var u = l + delta;
    if ((P.flags & FLAG_HALO) != 0u) {
        let lo = pool[P.lo_off + i];
        let hi = pool[P.hi_off + i];
        let mg = SH_HALO_MARGIN * (hi - lo);
        let c = clamp(u, lo - mg, hi + mg);
        u = c + P.halo_h * (u - c);
    }
    pool[P.luma_off + i] = u;
}

// Luma + chroma → RGB (decoded to linear on the scene path, clamped on the
// display path).
@compute @workgroup_size(64)
fn combine(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = linear_index(gid);
    if (i >= P.n) { return; }
    var l = pool[P.luma_off + i];
    if (P.linear != 0u) {
        l = max(l, 0.0);
    }
    for (var c = 0u; c < 3u; c = c + 1u) {
        var v = l + pool[P.chroma_off + i * 3u + c];
        if (P.linear != 0u) {
            v = decode_channel(v);
        } else {
            v = clamp(v, 0.0, 1.0);
        }
        pool[P.img_off + i * 3u + c] = v;
    }
}

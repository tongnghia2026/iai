// GPU Detail — à-trous sharpen / noise-reduction port of the CPU
// `core::develop::detail::process_detail_plane`, so the live preview runs the
// SAME Detail as the commit and matches it pixel-for-pixel.
//
// One pooled storage buffer `pool` holds every working plane at a fixed f32
// offset (in elements); each pass reads/writes regions of it by offset carried
// in the per-dispatch uniform `P`. Multi-tap / multi-scale work is done as a
// sequence of dispatches in one compute pass (WebGPU inserts read-after-write
// barriers between dispatches), mirroring the CPU's sequential planes exactly.
//
// Display-domain path only for now (linear==0): luma = Rec.709, chroma = rgb −
// luma, both clamped to [0,1] at the ends — matching `apply_detail_to_display_buffer`.

struct PassParams {
    w: u32,
    h: u32,
    n: u32,
    level: u32,
    flags: u32,      // bit0 = horizontal pass, bit1 = edge-aware
    linear: u32,
    chan: u32,       // chroma channel (0..2) for the chroma-NR passes
    groups_x: u32,  // number of dispatched workgroups in X (for 2-D linearisation)
    // generic buffer offsets (in f32 elements)
    src_off: u32,
    dst_off: u32,
    a_off: u32,
    b_off: u32,
    // named region offsets
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
    // detail params
    amount: f32,
    sigma: f32,
    detail: f32,
    masking: f32,
    nr: f32,
    color_nr: f32,
    lg0: f32,
    lg1: f32,
    lg2: f32,
    lc0: f32,     // working-space luma coefficients
    lc1: f32,
    lc2: f32,
};

@group(0) @binding(0) var<storage, read_write> pool: array<f32>;
@group(0) @binding(1) var<uniform> P: PassParams;

const RANGE_SIGMA: f32 = 0.12;
const SHARPEN_KNEE: f32 = 0.04;
const SHARPEN_LIMIT: f32 = 0.35;
const MASK_GRAD_FULL: f32 = 0.035;
const NR_LUMA_THRESH: f32 = 0.08;
const NR_LEVEL_DECAY: f32 = 0.5;
const NR_SHADOW_MID: f32 = 0.5;
const NR_LUMA_SHADOW_GAIN: f32 = 1.5;
const NR_CHROMA_SHADOW_GAIN: f32 = 1.2;
// Chroma-NR per-level attenuation at a full slider (finest killed hardest).
const CHROMA_NR_ATTEN: array<f32, 3> = array<f32, 3>(1.0, 0.85, 0.6);

fn linear_index(gid: vec3<u32>) -> u32 {
    return gid.y * P.groups_x * 64u + gid.x;
}

fn luminance(r: f32, g: f32, b: f32) -> f32 {
    return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

fn smootherstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = clamp((x - edge0) / (edge1 - edge0), 0.0, 1.0);
    return t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
}

fn nr_shadow_weight(brightness: f32, gain: f32) -> f32 {
    return 1.0 + gain * (1.0 - smootherstep(0.0, NR_SHADOW_MID, clamp(brightness, 0.0, 1.0)));
}

// Split RGB into luminance + chroma offsets (display domain).
@compute @workgroup_size(64)
fn split(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = linear_index(gid);
    if (i >= P.n) { return; }
    let r = pool[P.img_off + i * 3u];
    let g = pool[P.img_off + i * 3u + 1u];
    let b = pool[P.img_off + i * 3u + 2u];
    let y = P.lc0 * r + P.lc1 * g + P.lc2 * b;
    var l: f32;
    if (P.linear == 0u) {
        l = clamp(y, 0.0, 1.0);
    } else {
        l = max(y, 0.0);
    }
    pool[P.luma_off + i] = l;
    pool[P.chroma_off + i * 3u] = r - l;
    pool[P.chroma_off + i * 3u + 1u] = g - l;
    pool[P.chroma_off + i * 3u + 2u] = b - l;
}

// One separable à-trous B3-spline pass (horizontal or vertical) at hole spacing
// 1<<level, edge-clamped, optionally range-weighted (edge-aware). src/dst are
// single-plane offsets.
@compute @workgroup_size(64)
fn atrous(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = linear_index(gid);
    if (i >= P.n) { return; }
    let w = i32(P.w);
    let h = i32(P.h);
    let x = i32(i % P.w);
    let y = i32(i / P.w);
    let step = 1 << P.level;
    let horizontal = (P.flags & 1u) != 0u;
    let edge_aware = (P.flags & 2u) != 0u;
    let centre = pool[P.src_off + i];
    let inv_s2 = 1.0 / (RANGE_SIGMA * RANGE_SIGMA);
    var kern = array<f32, 5>(1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0);
    var acc = 0.0;
    var wsum = 0.0;
    for (var t = 0; t < 5; t = t + 1) {
        let o = (t - 2) * step;
        var idx: u32;
        if (horizontal) {
            let sx = clamp(x + o, 0, w - 1);
            idx = u32(y * w + sx);
        } else {
            let sy = clamp(y + o, 0, h - 1);
            idx = u32(sy * w + x);
        }
        let v = pool[P.src_off + idx];
        var wt = kern[t];
        if (edge_aware) {
            let d = v - centre;
            wt = kern[t] / (1.0 + d * d * inv_s2);
        }
        acc = acc + v * wt;
        wsum = wsum + wt;
    }
    pool[P.dst_off + i] = acc / max(wsum, 1e-9);
}

// Detail coefficient dst = a − b.
@compute @workgroup_size(64)
fn diff(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = linear_index(gid);
    if (i >= P.n) { return; }
    pool[P.dst_off + i] = pool[P.a_off + i] - pool[P.b_off + i];
}

// Copy one chroma channel (P.chan) out to a contiguous scratch plane so the
// à-trous kernels can decompose it.
@compute @workgroup_size(64)
fn extract_channel(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = linear_index(gid);
    if (i >= P.n) { return; }
    pool[P.dst_off + i] = pool[P.chroma_off + i * 3u + P.chan];
}

// Chroma NR recombine: chroma[chan] = residual + Σ detail_j·(1 − atten_j), with a
// tone-adaptive (luma-shadow) attenuation. Mirrors the CPU chroma-NR recombine.
@compute @workgroup_size(64)
fn chroma_recombine(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = linear_index(gid);
    if (i >= P.n) { return; }
    let shadow_w = nr_shadow_weight(pool[P.luma_off + i], NR_CHROMA_SHADOW_GAIN);
    var v = pool[P.res_off + i];
    var dd = array<f32, 3>(pool[P.d0_off + i], pool[P.d1_off + i], pool[P.d2_off + i]);
    for (var j = 0; j < 3; j = j + 1) {
        let atten = min(P.color_nr * CHROMA_NR_ATTEN[j] * shadow_w, 1.0);
        v = v + dd[j] * (1.0 - atten);
    }
    pool[P.chroma_off + i * 3u + P.chan] = v;
}

// Non-negative garrote luma NR over the three detail levels, shadow-boosted
// threshold using the residual brightness.
@compute @workgroup_size(64)
fn nr_garrote(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = linear_index(gid);
    if (i >= P.n) { return; }
    let sw = nr_shadow_weight(pool[P.res_off + i], NR_LUMA_SHADOW_GAIN);
    let base = P.nr * NR_LUMA_THRESH;
    garrote_at(P.d0_off + i, base * sw);
    garrote_at(P.d1_off + i, base * NR_LEVEL_DECAY * sw);
    garrote_at(P.d2_off + i, base * NR_LEVEL_DECAY * NR_LEVEL_DECAY * sw);
}

fn garrote_at(idx: u32, t: f32) {
    let v = pool[idx];
    let a = abs(v);
    if (a <= t) {
        pool[idx] = 0.0;
    } else {
        pool[idx] = v * (1.0 - (t * t) / (a * a));
    }
}

// Box blur radius 1, edge-clamped window average — one separable pass over the
// 3-channel chroma plane. flags bit0 = horizontal.
@compute @workgroup_size(64)
fn box_blur(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = linear_index(gid);
    if (i >= P.n) { return; }
    let w = i32(P.w);
    let h = i32(P.h);
    let x = i32(i % P.w);
    let y = i32(i / P.w);
    let horizontal = (P.flags & 1u) != 0u;
    for (var c = 0u; c < 3u; c = c + 1u) {
        var lo: i32;
        var hi: i32;
        if (horizontal) {
            lo = max(x - 1, 0);
            hi = min(x + 1, w - 1);
        } else {
            lo = max(y - 1, 0);
            hi = min(y + 1, h - 1);
        }
        var sum = 0.0;
        for (var p = lo; p <= hi; p = p + 1) {
            var idx: u32;
            if (horizontal) {
                idx = u32(y * w + p);
            } else {
                idx = u32(p * w + x);
            }
            sum = sum + pool[P.src_off + idx * 3u + c];
        }
        pool[P.dst_off + i * 3u + c] = sum / f32(hi - lo + 1);
    }
}

// NR-only reconstruction: luma = residual + Σ (shrunk) details.
@compute @workgroup_size(64)
fn reconstruct(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = linear_index(gid);
    if (i >= P.n) { return; }
    let base = pool[P.res_off + i] + pool[P.d0_off + i] + pool[P.d1_off + i] + pool[P.d2_off + i];
    if (P.linear == 0u) {
        pool[P.luma_off + i] = clamp(base, 0.0, 1.0);
    } else {
        pool[P.luma_off + i] = max(base, 0.0);
    }
}

// Sharpen: boost the (denoised) detail levels, tanh-limited, masking-gated, plus
// the chroma de-fringe pull. Writes the new luma and the pulled chroma.
@compute @workgroup_size(64)
fn sharpen(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = linear_index(gid);
    if (i >= P.n) { return; }
    let w = i32(P.w);
    let h = i32(P.h);
    let x = i32(i % P.w);
    let y = i32(i / P.w);

    let dv0 = pool[P.d0_off + i];
    let dv1 = pool[P.d1_off + i];
    let dv2 = pool[P.d2_off + i];
    var lg = array<f32, 3>(P.lg0, P.lg1, P.lg2);
    var dvs = array<f32, 3>(dv0, dv1, dv2);
    var delta = 0.0;
    var edge_mag = 0.0;
    for (var j = 0; j < 3; j = j + 1) {
        let dv = dvs[j];
        if (j < 2) { edge_mag = edge_mag + abs(dv); }
        let weight = P.detail + (1.0 - P.detail) * smootherstep(0.0, SHARPEN_KNEE, abs(dv));
        delta = delta + P.amount * lg[j] * weight * dv;
    }

    var mask = 1.0;
    if (P.masking > 0.001) {
        let xl = pool[P.res_off + u32(y * w + max(x - 1, 0))];
        let xr = pool[P.res_off + u32(y * w + min(x + 1, w - 1))];
        let yt = pool[P.res_off + u32(max(y - 1, 0) * w + x)];
        let yb = pool[P.res_off + u32(min(y + 1, h - 1) * w + x)];
        let gx = (xr - xl) * 0.5;
        let gy = (yb - yt) * 0.5;
        let gmag = sqrt(gx * gx + gy * gy);
        let tm = P.masking * MASK_GRAD_FULL;
        mask = smootherstep(tm * 0.5, tm * 1.5, gmag);
    }

    delta = SHARPEN_LIMIT * tanh(delta * mask / SHARPEN_LIMIT);
    let base = pool[P.res_off + i] + dv0 + dv1 + dv2;
    var lout = base + delta;
    if (P.linear == 0u) {
        lout = clamp(lout, 0.0, 1.0);
    } else {
        lout = max(lout, 0.0);
    }
    pool[P.luma_off + i] = lout;

    let edge_gate = smootherstep(0.006, 0.055, edge_mag);
    let fr = min(P.amount * edge_gate * 0.4, 0.6) * mask;
    if (fr > 0.001) {
        for (var c = 0u; c < 3u; c = c + 1u) {
            let cur = pool[P.chroma_off + i * 3u + c];
            let cav = pool[P.cavg_off + i * 3u + c];
            pool[P.chroma_off + i * 3u + c] = cur + (cav - cur) * fr;
        }
    }
}

// Recombine luminance + chroma back to RGB (display clamp at the ends).
@compute @workgroup_size(64)
fn combine(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = linear_index(gid);
    if (i >= P.n) { return; }
    let l = pool[P.luma_off + i];
    for (var c = 0u; c < 3u; c = c + 1u) {
        var v = l + pool[P.chroma_off + i * 3u + c];
        if (P.linear == 0u) {
            v = clamp(v, 0.0, 1.0);
        }
        pool[P.img_off + i * 3u + c] = v;
    }
}

// GPU port of the self-guided low-pass `guided_lowpass_plane` (core::develop::
// spatial): count-normalised box means of I and I², a = var/(var+eps),
// b = (1-a)·mean, box means of a and b, q = mean_a·I + mean_b. One pooled
// storage buffer holds every plane at a fixed f32 offset; the uniform `G`
// carries the offsets of the pass (256-byte dynamic stride).

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
};

@group(0) @binding(0) var<storage, read_write> pool: array<f32>;
@group(0) @binding(1) var<uniform> G: GuidedParams;
// Source plane in `.r` (texel (x, y) = plane pixel (x, y)).
@group(0) @binding(2) var src_tex: texture_2d<f32>;
// Destination buffer of the filtered plane (packed from `out_off`).
@group(0) @binding(3) var<storage, read_write> out_buf: array<f32>;

fn gidx(gid: vec3<u32>) -> u32 {
    return gid.y * G.groups_x * 64u + gid.x;
}

// I and I² from the source texture.
@compute @workgroup_size(64)
fn load(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gidx(gid);
    if (i >= G.n) { return; }
    let v = textureLoad(src_tex, vec2<i32>(i32(i % G.w), i32(i / G.w)), 0).r;
    pool[G.dst + i] = v;
    pool[G.dst2 + i] = v * v;
}

// Horizontal count-normalised box mean (radius r) of plane `src` → `dst`.
@compute @workgroup_size(64)
fn box_h(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gidx(gid);
    if (i >= G.n) { return; }
    let x = i32(i % G.w);
    let row = i - u32(x);
    let x0 = max(x - i32(G.r), 0);
    let x1 = min(x + i32(G.r), i32(G.w) - 1);
    var acc = 0.0;
    for (var t = x0; t <= x1; t = t + 1) {
        acc = acc + pool[G.src + row + u32(t)];
    }
    pool[G.dst + i] = acc / f32(x1 - x0 + 1);
}

// Vertical count-normalised box mean (radius r) of plane `src` → `dst`.
@compute @workgroup_size(64)
fn box_v(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gidx(gid);
    if (i >= G.n) { return; }
    let x = i % G.w;
    let y = i32(i / G.w);
    let y0 = max(y - i32(G.r), 0);
    let y1 = min(y + i32(G.r), i32(G.h) - 1);
    var acc = 0.0;
    for (var t = y0; t <= y1; t = t + 1) {
        acc = acc + pool[G.src + u32(t) * G.w + x];
    }
    pool[G.dst + i] = acc / f32(y1 - y0 + 1);
}

// a (→ dst) and b (→ dst2) from mean I (src) and mean I² (src2).
@compute @workgroup_size(64)
fn coeffs(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gidx(gid);
    if (i >= G.n) { return; }
    let m = pool[G.src + i];
    let var_ = max(pool[G.src2 + i] - m * m, 0.0);
    let a = var_ / (var_ + G.eps);
    pool[G.dst + i] = a;
    pool[G.dst2 + i] = (1.0 - a) * m;
}

// q = mean_a (src) · I (dst) + mean_b (src2), written to the output buffer.
@compute @workgroup_size(64)
fn finish(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gidx(gid);
    if (i >= G.n) { return; }
    out_buf[G.out_off + i] = pool[G.src + i] * pool[G.dst + i] + pool[G.src2 + i];
}

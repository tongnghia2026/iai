// Composite one resolved vector run (premultiplied) over the accumulator
// (straight alpha), matching `compositor.wgsl`'s Normal blend (mode 0): the
// source-over runs in sRGB byte space, and the straight-alpha result is written
// back. `textureLoad` on the sRGB textures returns linear samples, so this
// converts to byte space, blends, and returns to linear for the sRGB target.

@group(0) @binding(0) var dst_tex: texture_2d<f32>;
@group(0) @binding(1) var vec_tex: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // Fullscreen triangle: (-1,-1), (3,-1), (-1,3).
    var out: VsOut;
    let x = select(-1.0, 3.0, vi == 1u);
    let y = select(-1.0, 3.0, vi == 2u);
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    return out;
}

fn to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((max(c, vec3<f32>(0.0)) + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let coord = vec2<i32>(pos.xy);
    let dst = textureLoad(dst_tex, coord, 0); // linear, straight alpha
    let vpm = textureLoad(vec_tex, coord, 0); // linear, premultiplied
    let sa = vpm.a;
    if (sa <= 0.00001) {
        return dst;
    }
    let v_lin = vpm.rgb / sa; // straight, linear
    let src_rgb = to_srgb(v_lin); // byte space
    let dst_rgb = to_srgb(dst.rgb);
    let da = dst.a;
    let out_a = sa + da * (1.0 - sa);
    if (out_a <= 0.00001) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    let out_rgb = (sa * src_rgb + da * (1.0 - sa) * dst_rgb) / out_a;
    return vec4<f32>(to_linear(out_rgb), out_a);
}

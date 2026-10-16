// Composite one resolved vector run (premultiplied) over the accumulator
// (straight alpha), matching `compositor.wgsl`'s Normal blend (mode 0): the
// source-over runs in sRGB byte space, and the straight-alpha result is written
// back. `textureLoad` on the sRGB textures returns linear samples, so this
// converts to byte space, blends, and returns to linear for the sRGB target.

@group(0) @binding(0) var dst_tex: texture_2d<f32>;
@group(0) @binding(1) var vec_tex: texture_2d<f32>;
@group(0) @binding(2) var mask_tex: texture_2d<f32>;

struct MaskUniform {
    // enabled, view_offset_x, view_offset_y, zoom,
    // layer_offset_x, layer_offset_y, mask_width, mask_height.
    data0: vec4<f32>,
    data1: vec4<f32>,
    // mask sample shift (PowerClip live pin), isolated-run opacity, padding.
    data2: vec4<f32>,
};
@group(0) @binding(3) var<uniform> mask_u: MaskUniform;

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

fn color_dodge(s: f32, d: f32) -> f32 {
    if d <= 0.0 { return 0.0; }
    if s >= 1.0 { return 1.0; }
    return min(1.0, d / (1.0 - s));
}

fn color_burn(s: f32, d: f32) -> f32 {
    if d >= 1.0 { return 1.0; }
    if s <= 0.0 { return 0.0; }
    return 1.0 - min(1.0, (1.0 - d) / s);
}

// PDF/W3C non-separable blend helpers, byte-identical to `compositor.wgsl` and
// the CPU reference `core::blend`: legacy blend-space luminosity (0.30/0.59/0.11)
// and saturation = max - min, NOT an HSL conversion.
fn lum(c: vec3<f32>) -> f32 { return dot(c, vec3(0.30, 0.59, 0.11)); }
fn sat(c: vec3<f32>) -> f32 {
    return max(max(c.r, c.g), c.b) - min(min(c.r, c.g), c.b);
}

fn set_lum(c: vec3<f32>, l: f32) -> vec3<f32> {
    let d = l - lum(c);
    var r = c + d;
    let mn = min(min(r.r, r.g), r.b);
    let mx = max(max(r.r, r.g), r.b);
    if (mn < 0.0) {
        let ll = lum(r);
        r = ll + ((r - ll) * ll / (ll - mn));
    }
    if (mx > 1.0) {
        let ll = lum(r);
        r = ll + ((r - ll) * (1.0 - ll) / (mx - ll));
    }
    return r;
}

fn set_sat(c: vec3<f32>, s: f32) -> vec3<f32> {
    let range = sat(c);
    if (range <= 0.0) {
        return vec3<f32>(0.0);
    }

    var r = vec3<f32>(0.0);
    if (c.r <= c.g) {
        if (c.g <= c.b) {
            r.g = (c.g - c.r) * s / range;
            r.b = s;
        } else if (c.r <= c.b) {
            r.b = (c.b - c.r) * s / range;
            r.g = s;
        } else {
            r.r = (c.r - c.b) * s / range;
            r.g = s;
        }
    } else if (c.r <= c.b) {
        r.r = (c.r - c.g) * s / range;
        r.b = s;
    } else if (c.g <= c.b) {
        r.b = (c.b - c.g) * s / range;
        r.r = s;
    } else {
        r.g = (c.g - c.b) * s / range;
        r.r = s;
    }
    return r;
}

fn blend_comp(mode: u32, s: vec3<f32>, d: vec3<f32>) -> vec3<f32> {
    switch mode {
        case 1u: { return s * d; }
        case 2u: { return s + d - s * d; }
        case 3u: {
            return mix(2.0 * s * d, 1.0 - 2.0 * (1.0 - s) * (1.0 - d), step(vec3(0.5), d));
        }
        case 4u: { return min(s, d); }
        case 5u: { return max(s, d); }
        case 6u: {
            return vec3(color_dodge(s.r, d.r), color_dodge(s.g, d.g), color_dodge(s.b, d.b));
        }
        case 7u: {
            return vec3(color_burn(s.r, d.r), color_burn(s.g, d.g), color_burn(s.b, d.b));
        }
        case 8u: {
            return mix(2.0 * s * d, 1.0 - 2.0 * (1.0 - s) * (1.0 - d), step(vec3(0.5), s));
        }
        case 9u: {
            let g = mix(sqrt(d), ((16.0 * d - 12.0) * d + 4.0) * d, step(d, vec3(0.25)));
            return mix(d - (1.0 - 2.0 * s) * d * (1.0 - d), d + (2.0 * s - 1.0) * (g - d), step(vec3(0.5), s));
        }
        case 10u: { return abs(s - d); }
        case 11u: { return s + d - 2.0 * s * d; }
        case 12u: { return set_lum(set_sat(s, sat(d)), lum(d)); }
        case 13u: { return set_lum(set_sat(d, sat(s)), lum(d)); }
        case 14u: { return set_lum(s, lum(d)); }
        case 15u: { return set_lum(d, lum(s)); }
        case 16u: { return clamp(d + 2.0 * s - 1.0, vec3(0.0), vec3(1.0)); }
        default: { return s; }
    }
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let coord = vec2<i32>(pos.xy);
    let dst = textureLoad(dst_tex, coord, 0); // linear, straight alpha
    let vpm = textureLoad(vec_tex, coord, 0); // linear, premultiplied
    var mask_a = 1.0;
    if (mask_u.data0.x > 0.5) {
        let canvas = pos.xy / mask_u.data0.w + mask_u.data0.yz;
        let local = floor(canvas - mask_u.data1.xy + mask_u.data2.xy);
        let upper = vec2<i32>(max(mask_u.data1.zw - vec2<f32>(1.0), vec2<f32>(0.0)));
        let coord_mask = clamp(vec2<i32>(local), vec2<i32>(0), upper);
        mask_a = textureLoad(mask_tex, coord_mask, 0).r;
    }
    let sa = vpm.a * mask_a * mask_u.data2.z;
    if (sa <= 0.00001) {
        return dst;
    }
    let v_lin = vpm.rgb / max(vpm.a, 0.00001); // straight, linear
    let src_rgb = to_srgb(v_lin); // byte space
    let dst_rgb = to_srgb(dst.rgb);
    let blend_mode = u32(mask_u.data2.w + 0.5);
    let da = dst.a;
    let out_a = sa + da * (1.0 - sa);
    if (out_a <= 0.00001) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    var out_rgb_pre = sa * src_rgb + da * (1.0 - sa) * dst_rgb;
    if blend_mode != 0u {
        let blended = blend_comp(blend_mode, src_rgb, dst_rgb);
        out_rgb_pre = (sa * da) * blended
            + (sa * (1.0 - da)) * src_rgb
            + (da * (1.0 - sa)) * dst_rgb;
    }
    let out_rgb = out_rgb_pre / out_a;
    return vec4<f32>(to_linear(out_rgb), out_a);
}

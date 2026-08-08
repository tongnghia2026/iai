
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
    // Free-transform preview (xform_active=1 for the layer being transformed)
    xform_active:   u32,
    xform_inv_a:    f32,  // inverse matrix: [inv_a  inv_b]
    xform_inv_b:    f32,  //                 [inv_c  inv_d]
    xform_inv_c:    f32,
    xform_inv_d:    f32,
    xform_pivot_x:  f32,  // fixed pivot (original layer center in canvas space)
    xform_pivot_y:  f32,
    xform_tx:       f32,  // additional translation (from center-handle drag)
    xform_ty:       f32,
    xform_orig_ox:  f32,  // original layer.offset for local-coord lookup
    xform_orig_oy:  f32,
    xform_orig_w:   f32,  // original layer dimensions (bounds check)
    xform_orig_h:   f32,
    mask_enabled:   u32,
    mask_inverted:  u32,
    adj_kind:       u32,
    adj_pad_a:      u32,
    clip_shift_packed: u32,  // live PowerClip pin: two i16 (dx<<16)|dy = offset − mask bake offset
    adj_pad_c:      u32,
    adj_p:          array<vec4<f32>, 3>,
    lut:            array<vec4<f32>, 192>,
};

@group(0) @binding(0) var atlas_tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
// Scene-referred Develop master (RAW sessions, u.adj_pad_c == 1u): the full
// layer as UNCLAMPED linear f16 RGBA. A 1×1 dummy is bound outside a session.
@group(0) @binding(2) var dev_scene_tex: texture_2d<f32>;

@group(1) @binding(0) var dst_tex: texture_2d<f32>;

@group(2) @binding(0) var<uniform> u: CompositorUniforms;
@group(2) @binding(1) var<storage, read> tile_map: array<i32>;
@group(2) @binding(2) var<storage, read> mask_tile_map: array<i32>;
// Develop local-tone preview (adj_kind==20, adj_p[1].x==1): a downsampled
// regional base-luma proxy (Shadows/Highlights local adaptation) plus the
// Highlights/Shadows/Whites/Blacks luma-offset LUT. Read only when the local
// path is active; otherwise they hold harmless leftover data.
@group(2) @binding(3) var<storage, read> dev_region_luma: array<f32>;
@group(2) @binding(4) var<storage, read> dev_local_lut: array<f32>;
// Develop colour preview (adj_kind==20, adj_p[2].x==1): the tone-mapped layer's
// edge-aware regional low-pass and that region after the Vibrance/Saturation/Mixer
// transform, each `pw*ph*3` packed f32 (RGB). The shader recombines them with the
// pixel's own detail (dev_finish_colored), matching the CPU `finish_colored_pixel`.
@group(2) @binding(5) var<storage, read> dev_region_rgb: array<f32>;
@group(2) @binding(6) var<storage, read> dev_adjusted_rgb: array<f32>;
// Develop Effects: raw [texture, clarity, dehaze, vignette] slider values.
// Applied last in develop_apply; Clarity/Dehaze are spatial and take their
// regional base from the colour region proxy (see dev_effects_stage).
@group(2) @binding(7) var<storage, read> dev_effects: array<f32>;
// Develop per-channel R/G/B point-curve LUTs: [active_flag, R 256, G 256,
// B 256]. Applied right after the tone stage, mirroring the CPU
// `ToneData::apply_rgb_curves` (same tables, both interpolated).
@group(2) @binding(8) var<storage, read> dev_rgb_curve: array<f32>;

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

// PDF/W3C non-separable blend helpers. These deliberately use the legacy
// blend-space luminosity rather than HSL or display-referred Rec.709 luma.
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

fn color_dodge(s: f32, d: f32) -> f32 {
    if (d <= 0.0) { return 0.0; }
    if (s >= 1.0) { return 1.0; }
    return min(1.0, d / (1.0 - s));
}

fn color_burn(s: f32, d: f32) -> f32 {
    if (d >= 1.0) { return 1.0; }
    if (s <= 0.0) { return 0.0; }
    return 1.0 - min(1.0, (1.0 - d) / s);
}

fn dissolve_hash(x: u32, y: u32) -> f32 {
    var h = x * 0x9e3779b9u + y * 0x517cc1b7u;
    h = h ^ (h >> 16u);
    h = h * 0x45d9f3b7u;
    h = h ^ (h >> 16u);
    return f32(h) / 4294967296.0;
}

fn rgb_to_hsl(c: vec3<f32>) -> vec3<f32> {
    let mx = max(max(c.r, c.g), c.b);
    let mn = min(min(c.r, c.g), c.b);
    let l  = (mx + mn) * 0.5;
    let d  = mx - mn;
    if (d < 0.0001) { return vec3(0.0, 0.0, l); }
    let s  = select(d / (2.0 - mx - mn), d / (mx + mn), l < 0.5);
    var h: f32;
    if (mx == c.r)      { h = (c.g - c.b) / d + select(6.0, 0.0, c.g >= c.b); }
    else if (mx == c.g) { h = (c.b - c.r) / d + 2.0; }
    else               { h = (c.r - c.g) / d + 4.0; }
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
    if (s == 0.0) { return vec3(l); }
    let q = select(l + s - l * s, l * (1.0 + s), l < 0.5);
    let p = 2.0 * l - q;
    return vec3(hue_to_rgb(p, q, h + 1.0/3.0),
                hue_to_rgb(p, q, h),
                hue_to_rgb(p, q, h - 1.0/3.0));
}

fn blend_comp(mode: u32, s: vec3<f32>, d: vec3<f32>) -> vec3<f32> {
    switch mode {
        case 0u:  { return s; }
        case 1u:  { return s * d; }
        case 2u:  { return s + d - s * d; }
        case 3u:  {
            let is_l = step(vec3(0.5), d);
            return mix(2.0*s*d, 1.0 - 2.0*(1.0-s)*(1.0-d), is_l);
        }
        case 4u:  { return min(s, d); }
        case 5u:  { return max(s, d); }
        case 6u:  {
            return vec3<f32>(
                color_dodge(s.r, d.r),
                color_dodge(s.g, d.g),
                color_dodge(s.b, d.b)
            );
        }
        case 7u:  {
            return vec3<f32>(
                color_burn(s.r, d.r),
                color_burn(s.g, d.g),
                color_burn(s.b, d.b)
            );
        }
        case 8u:  {
            let is_l = step(vec3(0.5), s);
            return mix(2.0*s*d, 1.0 - 2.0*(1.0-s)*(1.0-d), is_l);
        }
        case 9u:  {
            let g = mix(sqrt(d), ((16.0*d - 12.0)*d + 4.0)*d, step(d, vec3(0.25)));
            return mix(d - (1.0 - 2.0*s)*d*(1.0-d), d + (2.0*s-1.0)*(g-d), step(vec3(0.5), s));
        }
        case 10u: { return abs(s - d); }
        case 11u: { return s + d - 2.0*s*d; }
        case 12u: {
            return set_lum(set_sat(s, sat(d)), lum(d));
        }
        case 13u: {
            return set_lum(set_sat(d, sat(s)), lum(d));
        }
        case 14u: {
            return set_lum(s, lum(d));
        }
        case 15u: {
            return set_lum(d, lum(s));
        }
        case 16u: { return clamp(d + 2.0*s - 1.0, vec3(0.0), vec3(1.0)); }
        default:  { return s; }
    }
}

fn dev_param(i: u32) -> f32 {
    let v = u.lut[i / 4u];
    switch (i % 4u) {
        case 0u: { return v.x; }
        case 1u: { return v.y; }
        case 2u: { return v.z; }
        default: { return v.w; }
    }
}

// Per-stage gating: skip a whole adjustment block when its sliders sit at zero,
// matching the CPU DevelopPlan so the GPU preview equals the committed result.
fn dev_active(v: f32) -> bool {
    return abs(v) > 0.001;
}

fn dev_has_mixer() -> bool {
    for (var i = 0u; i < 8u; i = i + 1u) {
        if (dev_active(dev_param(20u + i)) || dev_active(dev_param(28u + i)) || dev_active(dev_param(36u + i))) {
            return true;
        }
    }
    return false;
}

fn dev_eased(value: f32) -> f32 {
    // Linear slider response (mirrors CPU control_to_unit); dropped the old
    // pow(1.18) ease that made small drags feel dead.
    return clamp(value / 200.0, -1.0, 1.0);
}

fn dev_smootherstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = clamp((x - edge0) / max(edge1 - edge0, 0.00001), 0.0, 1.0);
    return t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
}

fn dev_bell(x: f32, center: f32, width: f32) -> f32 {
    return dev_smootherstep(0.0, 1.0, 1.0 - abs(x - center) / max(width, 0.00001));
}

fn dev_range_mask(x: f32, low: f32, low_full: f32, high_full: f32, high: f32) -> f32 {
    return dev_smootherstep(low, low_full, x) * (1.0 - dev_smootherstep(high_full, high, x));
}

fn dev_linear_to_srgb_channel(v_in: f32) -> f32 {
    let v = clamp(v_in, 0.0, 1.0);
    if (v <= 0.0031308) {
        return v * 12.92;
    }
    return 1.055 * pow(v, 1.0 / 2.4) - 0.055;
}

fn dev_srgb_to_linear_channel(v_in: f32) -> f32 {
    let v = clamp(v_in, 0.0, 1.0);
    if (v <= 0.04045) {
        return v / 12.92;
    }
    return pow((v + 0.055) / 1.055, 2.4);
}

fn dev_linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        dev_linear_to_srgb_channel(c.r),
        dev_linear_to_srgb_channel(c.g),
        dev_linear_to_srgb_channel(c.b)
    );
}

fn dev_srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        dev_srgb_to_linear_channel(c.r),
        dev_srgb_to_linear_channel(c.g),
        dev_srgb_to_linear_channel(c.b)
    );
}

fn dev_luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3(0.2126, 0.7152, 0.0722));
}

// Rec.709 luminance on LINEAR values, used by the white-balance/exposure stage.
fn dev_luma_lin(c: vec3<f32>) -> f32 {
    return dot(c, vec3(0.2126, 0.7152, 0.0722));
}

fn dev_fit_linear_rgb_to_luma(c_in: vec3<f32>, target_luma_in: f32) -> vec3<f32> {
    let target_luma = clamp(target_luma_in, 0.0, 1.0);
    var c = c_in;
    let mn = min(min(c.r, c.g), c.b);
    if (mn < 0.0) {
        let anchor = max(target_luma, 0.0001);
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

fn dev_highlight_rolloff(x: f32) -> f32 {
    let knee = 0.75;
    if (x <= knee) {
        return x;
    }
    let t = (x - knee) / (1.0 - knee);
    return knee + (1.0 - knee) * t / (1.0 + t);
}

// Normalised tanh contrast sigmoid, mirrors CPU contrast_curve (CONTRAST_K = 2.4):
// strictly monotone through the endpoints, never crushes a tonal band to a single
// value. Positive steepens midtones; negative uses the exact inverse (atanh) so it
// flattens them instead of steepening (the sigmoid is symmetric in sign otherwise).
fn dev_contrast_curve(x_in: f32, amount: f32) -> f32 {
    let x = clamp(x_in, 0.0, 1.0);
    let a = clamp(amount, -1.0, 1.0);
    if (abs(a) < 1e-4) {
        return x;
    }
    let k = abs(a) * 3.0;
    let d = tanh(k * 0.5);
    if (a > 0.0) {
        return clamp(0.5 + tanh(k * (x - 0.5)) / (2.0 * d), 0.0, 1.0);
    }
    return clamp(0.5 + atanh((2.0 * x - 1.0) * d) / k, 0.0, 1.0);
}

// White balance in linear light: luma-preserving channel gains on the
// Temperature (blue↔yellow) and Tint (green↔magenta) axes. Mirrors CPU
// apply_white_balance.
fn dev_white_balance(c_in: vec3<f32>) -> vec3<f32> {
    let temp = dev_eased(dev_param(6u));
    let tint = dev_eased(dev_param(7u));
    let gr = (1.0 + 0.30 * temp) * (1.0 + 0.10 * tint);
    let gg = 1.0 - 0.20 * tint;
    let gb = (1.0 - 0.30 * temp) * (1.0 + 0.10 * tint);
    let l0 = dev_luma_lin(c_in);
    var c = vec3<f32>(max(c_in.r * gr, 0.0), max(c_in.g * gg, 0.0), max(c_in.b * gb, 0.0));
    let l1 = dev_luma_lin(c);
    if (l1 > 1e-6) {
        c = c * (l0 / l1);
    }
    return c;
}

// Exposure as a true linear-light multiply (×2^EV). Mirrors CPU
// apply_exposure_linear: ±50 slider → ±5 EV, soft highlight shoulder.
fn dev_exposure_linear(c: vec3<f32>, value: f32) -> vec3<f32> {
    let ev = clamp(value, -50.0, 50.0) / 50.0 * 5.0;
    if (abs(ev) <= 0.0001) {
        return c;
    }
    let factor = pow(2.0, ev);
    var lin = c * factor;
    let l0 = max(dev_luma_lin(lin), 0.0);
    let target_l = dev_highlight_rolloff(l0);
    if (l0 > 1e-6) {
        lin = lin * (target_l / l0);
    }
    return dev_fit_linear_rgb_to_luma(lin, target_l);
}

fn dev_chroma(c: vec3<f32>) -> f32 {
    return max(max(c.r, c.g), c.b) - min(min(c.r, c.g), c.b);
}

fn dev_shift(v: f32, delta: f32) -> f32 {
    let d = clamp(delta, -1.0, 1.0);
    if (d >= 0.0) {
        return mix(v, 1.0, d);
    }
    return mix(v, 0.0, -d);
}

// Mirror of CPU apply_tone_delta: clamp to +/-0.85 then lift with a matched 0.45
// falloff on both sides. Earlier the positive branch used 0.62 and skipped the
// clamp, so Contrast/Clarity/Dehaze/Texture/Vignette previewed brighter than the
// committed pixels.
fn dev_tone_delta(c: vec3<f32>, delta_in: f32) -> vec3<f32> {
    let delta = clamp(delta_in, -0.85, 0.85);
    let l = clamp(dev_luma(c), 0.0, 1.0);
    if (delta >= 0.0) {
        let amount = delta * pow(1.0 - l, 0.45);
        return vec3(dev_shift(c.r, amount), dev_shift(c.g, amount), dev_shift(c.b, amount));
    }
    let amount = -delta * pow(l, 0.45);
    return vec3(dev_shift(c.r, -amount), dev_shift(c.g, -amount), dev_shift(c.b, -amount));
}

fn dev_black_floor_gate(luma: f32) -> f32 {
    return dev_smootherstep(0.004, 0.055, luma);
}

fn dev_black_mask(luma: f32, chroma: f32) -> f32 {
    let tonal = 1.0 - dev_smootherstep(0.18, 0.42, luma);
    let color_protect =
        1.0 - dev_smootherstep(0.16, 0.38, chroma) * dev_smootherstep(0.12, 0.36, luma) * 0.45;
    return tonal * color_protect;
}

fn dev_shadow_mask(luma: f32, chroma: f32) -> f32 {
    let tonal = dev_range_mask(luma, 0.012, 0.09, 0.40, 0.68);
    let colored_shadow = dev_bell(luma, 0.28, 0.26) * dev_smootherstep(0.035, 0.16, chroma) * 0.28;
    let toe_detail = dev_range_mask(luma, 0.006, 0.035, 0.18, 0.34) * 0.52;
    return max(max(tonal, colored_shadow), toe_detail);
}

fn dev_exposure_shadow_mask(luma: f32) -> f32 {
    return dev_range_mask(luma, 0.004, 0.035, 0.32, 0.76);
}

fn dev_exposure_highlight_guard(luma: f32) -> f32 {
    return 1.0 - dev_smootherstep(0.68, 1.0, luma) * 0.55;
}

fn dev_tone_response(amount: f32) -> f32 {
    return sign(amount) * min(pow(abs(amount), 0.55), 1.0);
}

fn dev_darks_mask(luma: f32) -> f32 {
    return dev_bell(luma, 0.36, 0.30) * dev_smootherstep(0.075, 0.18, luma);
}

fn dev_curve_shadows_mask(luma: f32) -> f32 {
    return dev_range_mask(luma, 0.025, 0.12, 0.30, 0.54);
}

fn dev_highlight_mask(luma: f32) -> f32 {
    return dev_smootherstep(0.44, 0.82, luma);
}

fn dev_white_mask(luma: f32) -> f32 {
    return dev_smootherstep(0.70, 0.98, luma);
}

fn dev_apply_luma_delta(c: vec3<f32>, delta: f32) -> vec3<f32> {
    return clamp(c + vec3(delta), vec3(0.0), vec3(1.0));
}

fn dev_set_rgb_preserving_luma(candidate: vec3<f32>, target_luma_in: f32) -> vec3<f32> {
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

fn dev_luma_target_chroma_preserve_weight(luma: f32, chroma: f32, target_luma: f32) -> f32 {
    let signal = dev_smootherstep(0.018, 0.075, luma);
    let color = dev_smootherstep(0.030, 0.105, chroma);
    let highlight_room = 1.0 - dev_smootherstep(0.88, 0.98, target_luma);
    return signal * color * highlight_room * 0.88;
}

fn dev_apply_luma_target(c: vec3<f32>, target_luma: f32) -> vec3<f32> {
    // Chroma-aware luminance retarget (mirrors CPU apply_luma_target). Exposure
    // and white balance use a true multiply in the linear stage instead.
    let l = clamp(dev_luma(c), 0.0, 1.0);
    let dst_luma = clamp(target_luma, 0.0, 1.0);
    let lift = dst_luma - l;
    let chroma = dev_chroma(c);
    let chroma_gate = dev_smootherstep(0.04, 0.22, chroma);
    let tone_gate = dev_smootherstep(0.025, 0.16, l) * (1.0 - dev_smootherstep(0.86, 0.98, dst_luma));
    var chroma_factor = 1.0;
    if (lift > 0.0) {
        chroma_factor = 1.0 + lift * 1.20 * chroma_gate * tone_gate;
    }
    let additive = vec3(dst_luma) + (c - vec3(l)) * chroma_factor;
    let scaled = c * (dst_luma / max(l, 0.00001));
    let preserve = dev_luma_target_chroma_preserve_weight(l, chroma, dst_luma);
    return dev_set_rgb_preserving_luma(additive + (scaled - additive) * preserve, dst_luma);
}

// Largest extra chroma scale that keeps every channel in [0,1] (gamut hull along
// the constant-luma direction). Mirrors CPU core::color::chroma_headroom.
fn dev_chroma_headroom(d: vec3<f32>, l: f32) -> f32 {
    var room = 1e9;
    if (d.r > 1e-6) { room = min(room, (1.0 - l) / d.r - 1.0); } else if (d.r < -1e-6) { room = min(room, l / -d.r - 1.0); }
    if (d.g > 1e-6) { room = min(room, (1.0 - l) / d.g - 1.0); } else if (d.g < -1e-6) { room = min(room, l / -d.g - 1.0); }
    if (d.b > 1e-6) { room = min(room, (1.0 - l) / d.b - 1.0); } else if (d.b < -1e-6) { room = min(room, l / -d.b - 1.0); }
    return max(room, 0.0);
}

// Gamut-aware soft-knee saturation (mirrors CPU core::color::saturate_around_luma):
// tanh-compress a positive boost into the pixel's headroom so it asymptotes to the
// gamut hull instead of hard-clipping (which caused blocky over-saturation/hue
// shift when a band was pushed to the max). dev_set_rgb_preserving_luma is a no-op
// safety clamp here since the candidate is already in gamut.
fn dev_scale_chroma_around_luma(c: vec3<f32>, factor_in: f32) -> vec3<f32> {
    let l = clamp(dev_luma(c), 0.0, 1.0);
    let protect = dev_smootherstep(0.035, 0.14, l) * (1.0 - dev_smootherstep(0.90, 0.99, l));
    let req = (clamp(factor_in, 0.0, 2.35) - 1.0) * protect;
    let d = c - vec3(l);
    var f = 1.0 + req;
    if (req > 0.0) {
        let room = dev_chroma_headroom(d, l);
        f = select(1.0, 1.0 + room * tanh(req / room), room > 1e-4);
    }
    return dev_set_rgb_preserving_luma(vec3(l) + d * f, l);
}

fn dev_apply_blacks(c: vec3<f32>, amount: f32, luma: f32, chroma: f32) -> vec3<f32> {
    let mask = dev_black_mask(luma, chroma);
    if (amount >= 0.0) {
        let floor_gate = dev_black_floor_gate(luma);
        let upper_gate = 1.0 - dev_smootherstep(0.30, 0.56, luma);
        let lift = amount * 0.16 * mask * floor_gate * upper_gate;
        return dev_apply_luma_target(c, min(luma + lift, max(luma, 0.32)));
    }
    let darken = (-amount) * 0.86 * mask;
    return dev_apply_luma_target(c, luma * (1.0 - darken));
}

fn dev_apply_shadows(c: vec3<f32>, amount: f32, luma: f32, chroma: f32) -> vec3<f32> {
    let tone = dev_tone_response(amount);
    let mask = dev_shadow_mask(luma, chroma);
    if (tone >= 0.0) {
        let floor_gate = dev_black_floor_gate(luma);
        let lift = tone * 0.62 * mask * floor_gate * pow(1.0 - luma, 0.48);
        let cap = luma + 0.42 * pow(1.0 - luma, 0.70);
        return dev_apply_luma_target(c, min(luma + lift, cap));
    }
    let darken = (-tone) * 0.66 * mask * pow(luma, 0.52);
    return dev_apply_luma_target(c, luma - darken);
}

fn dev_apply_exposure(c: vec3<f32>, value: f32) -> vec3<f32> {
    let amount = clamp(clamp(value, -50.0, 50.0) / 50.0, -1.0, 1.0);
    if (abs(amount) <= 0.001) {
        return c;
    }
    let luma = clamp(dev_luma(c), 0.0, 1.0);
    if (amount > 0.0) {
        let factor = pow(2.0, amount * 0.92);
        let base = luma * (factor - 1.0) * dev_exposure_highlight_guard(luma);
        let toe = amount * 0.105 * dev_exposure_shadow_mask(luma);
        return dev_apply_luma_target(c, min(luma + base + toe, 0.92 + amount * 0.035));
    }
    let factor = pow(2.0, amount * 1.10);
    let darken = (luma * factor - luma) * (0.72 + 0.28 * dev_smootherstep(0.18, 0.88, luma));
    return dev_apply_luma_target(c, luma + darken);
}

fn dev_apply_highlights(c: vec3<f32>, amount: f32, luma: f32) -> vec3<f32> {
    let tone = dev_tone_response(amount);
    let mask = dev_highlight_mask(luma);
    if (tone >= 0.0) {
        let lift = tone * 0.82 * mask * pow(1.0 - luma, 0.50);
        let channel_cap = luma + (1.0 - max(max(c.r, c.g), c.b)) * 0.82;
        return dev_apply_luma_target(c, min(min(luma + lift, channel_cap), 0.94 + tone * 0.025));
    }
    let recover = (-tone) * 0.82 * mask * pow(luma, 0.58);
    return dev_apply_luma_target(c, max(luma - recover, 0.12));
}

fn dev_apply_whites(c: vec3<f32>, amount: f32, luma: f32) -> vec3<f32> {
    let mask = dev_white_mask(luma);
    if (amount >= 0.0) {
        let lift = amount * 0.34 * mask * pow(1.0 - luma, 0.52);
        return dev_apply_luma_target(c, luma + lift);
    }
    let recover = (-amount) * 0.38 * mask * pow(luma, 0.62);
    return dev_apply_luma_target(c, luma - recover);
}

fn dev_soft_contrast(c: vec3<f32>, amount_in: f32, strength: f32) -> vec3<f32> {
    let amount = clamp(amount_in * strength, -0.88, 0.96);
    if (abs(amount) <= 0.0001) {
        return c;
    }
    let l = clamp(dev_luma(c), 0.0, 1.0);
    let target_l = dev_contrast_curve(l, amount);
    let chroma_factor = clamp(1.0 + amount * 0.34, 0.66, 1.34);
    let cc = clamp(vec3(l) + (c - vec3(l)) * chroma_factor, vec3(0.0), vec3(1.0));
    return dev_apply_luma_target(cc, target_l);
}

// UCS-22 hue (radians in (-π, π]) of an sRGB colour. Mirrors the CPU
// core::ucs::ucs_hue_rad (whose chromaticity warp runs in f64 — the difference
// is orders of magnitude below the 1° gate-LUT resolution).
fn dev_ucs_hue(c: vec3<f32>) -> f32 {
    let lin = vec3<f32>(
        dev_srgb_to_linear_channel(c.r),
        dev_srgb_to_linear_channel(c.g),
        dev_srgb_to_linear_channel(c.b),
    );
    let big_x = dot(vec3<f32>(0.4124564, 0.3575761, 0.1804375), lin);
    let big_y = dot(vec3<f32>(0.2126729, 0.7151522, 0.0721750), lin);
    let big_z = dot(vec3<f32>(0.0193339, 0.1191920, 0.9503041), lin);
    let sum = max(big_x + big_y + big_z, 1e-10);
    let x = big_x / sum;
    let y = big_y / sum;
    var d = 0.318707282433486 * x + 2.16743692732158 * y + 0.291320554395942;
    if (abs(d) < 1e-10) { d = select(1e-10, -1e-10, d < 0.0); }
    let u = (-0.783941002840055 * x + 0.277512987809202 * y + 0.153836578598858) / d;
    let v = (0.745273540913283 * x - 0.205375866083878 * y - 0.165478376301988) / d;
    let us = 1.39656225667 * u / (abs(u) + 1.49217352929);
    let vs = 1.4513954287 * v / (abs(v) + 1.52488637914);
    let up = -1.124983854323892 * us - 0.980483721769325 * vs;
    let vp = 1.86323315098672 * us + 1.971853092390862 * vs;
    return atan2(vp, up);
}

// HSV saturation (delta/max) — the luma-normalised colour confidence the mixer
// weights by (high for navy/burgundy, ≈0 for greys). Mirrors CPU `hsv_saturation`.
fn dev_hsv_sat(c: vec3<f32>) -> f32 {
    let mx = max(c.r, max(c.g, c.b));
    let delta = mx - min(c.r, min(c.g, c.b));
    if (mx > 1e-4 && delta > 1e-6) {
        return delta / mx;
    }
    return 0.0;
}

// Logistic saturation weight; mirrors CPU `mixer_satweight` (MIXER_SAT_STEEP 24
// — gentle on purpose, see the CPU const: steeper flips 0↔1 across JPEG chroma
// blocks in dark regions and cuts hard square seams at hair/shadow boundaries).
fn dev_satweight(x: f32) -> f32 {
    return 1.0 / (1.0 + exp(-24.0 * x));
}

// Full colour-confidence weight: logistic HSV-saturation weight × the
// absolute-delta gate (rejects dark near-neutrals whose tiny delta inflates
// HSV saturation). Mirrors CPU `mixer_weight` (MIXER_DELTA 0.012..0.075).
fn dev_mixer_weight(c: vec3<f32>, shift: f32) -> f32 {
    let delta = max(c.r, max(c.g, c.b)) - min(c.r, min(c.g, c.b));
    return dev_satweight(dev_hsv_sat(c) - shift)
        * dev_smootherstep(0.012, 0.075, delta);
}

// ── Colour-Mixer selection model (GPU mirror) ───────────────────────────────
// The mixer adjustment itself runs on the CPU and is uploaded as the low-res
// `adjusted` proxy (dev_adjusted_rgb), so the curve model is GPU-parity-exact
// by construction — there is no separate GPU mixer to drift. The ONLY selection
// code that runs live on the GPU is the full-res spatial re-gate
// (dev_mixer_edit_affinity), which decides how much of the proxy delta reaches
// each pixel. Its gate table is BUILT on the CPU (the same Lagrange basis the
// mixer curves interpolate on — core/develop.rs `build_mixer_curves_opt`) and
// uploaded at dev_rgb_curve[1025..1385]; the shader only samples it at the
// pixel's UCS hue and applies the logistic saturation weight.

// Sample the unified tone curve (adj_lut / u.lut) at luminance l in [0,1].
// Linearly interpolates between the two nearest texels, mirroring the CPU
// `lut_lerp` used by the 16-bit commit (`ToneData::apply_interp`). A 16-bit
// document (e.g. a RAW) holds smooth gradients; a nearest-texel lookup posterizes
// them into 256 steps in the preview that then vanish on the interpolated commit
// (the "ánh sáng nhảy" preview↔commit jump) — interpolating here keeps them equal.
fn dev_lut(l: f32) -> f32 {
    let p = clamp(l, 0.0, 1.0) * 255.0;
    let i = u32(floor(p));
    let f = p - floor(p);
    let a = dev_param(min(i, 255u));
    let b = dev_param(min(i + 1u, 255u));
    return a + (b - a) * f;
}

// Sample the Highlights/Shadows/Whites/Blacks luma-OFFSET LUT at luminance l.
// Read at the REGIONAL luma (see dev_region_luma_at) so a dark/bright region is
// moved uniformly — the local-adaptation path. Interpolated to mirror the 16-bit
// commit (`apply_local_interp` → `lut_lerp`), avoiding a 256-step preview↔commit jump.
fn dev_local_lut_at(l: f32) -> f32 {
    let p = clamp(l, 0.0, 1.0) * 255.0;
    let i = u32(floor(p));
    let f = p - floor(p);
    let a = dev_local_lut[min(i, 255u)];
    let b = dev_local_lut[min(i + 1u, 255u)];
    return a + (b - a) * f;
}

// Bilinear sample of the downsampled regional base-luma proxy at layer-local
// pixel (lx, ly). adj_p[1] = (is_local, downsample s, proxy_w, proxy_h). The
// pixel→proxy mapping and edge clamp match the CPU `sample_plane_bilinear`, so the
// GPU preview reads the same field the bake builds.
fn dev_region_luma_at(lx: f32, ly: f32) -> f32 {
    // Region-luma proxy dims are DERIVED from the layer size + downsample (it is
    // always full-layer) rather than read from adj_p[1].z/.w — those two slots
    // carry the colour proxy's origin when Colour AND local tone run together.
    let s = max(u.adj_p[1].y, 1.0);
    let su = u32(s);
    let pw = (u32(u.layer_w) + su - 1u) / su;
    let ph = (u32(u.layer_h) + su - 1u) / su;
    if (pw == 0u || ph == 0u) { return 0.0; }
    let fx = clamp((lx + 0.5) / s - 0.5, 0.0, f32(pw - 1u));
    let fy = clamp((ly + 0.5) / s - 0.5, 0.0, f32(ph - 1u));
    let x0 = u32(floor(fx));
    let y0 = u32(floor(fy));
    let x1 = min(x0 + 1u, pw - 1u);
    let y1 = min(y0 + 1u, ph - 1u);
    let wx = fx - f32(x0);
    let wy = fy - f32(y0);
    let v00 = dev_region_luma[y0 * pw + x0];
    let v10 = dev_region_luma[y0 * pw + x1];
    let v01 = dev_region_luma[y1 * pw + x0];
    let v11 = dev_region_luma[y1 * pw + x1];
    let top = v00 + (v10 - v00) * wx;
    let bot = v01 + (v11 - v01) * wx;
    return top + (bot - top) * wy;
}

// ── Scene-referred Develop chain (RAW sessions, u.adj_pad_c == 1u) ──────────
// Mirrors core/develop_scene.rs exactly:
//   u.lut            = sigmoid over EV∈[-14,+6], display-linear output;
//   dev_local_lut    = tone-equalizer EV offsets over the same axis;
//   dev_region_luma  = regional exposure E plane (EV values, not [0,1]);
//   dev_effects[16..25] = CAT16·2^EV matrix (row-major),
//              [25] = hue-preserve blend, [26] = display-curve flag;
//   dev_rgb_curve[769..1025] = display-domain luma curve.

// Sigmoid via the log2-indexed LUT. CPU twin: SceneToneData::tone_map.
fn dev_scene_lut(v: f32) -> f32 {
    if (v <= 0.0) {
        return 0.0;
    }
    let ev = log2(max(v, 6.1035156e-5)); // 2^-14
    return dev_lut(clamp((ev + 14.0) / 20.0, 0.0, 1.0));
}

// Tone-equalizer EV offset at absolute exposure e. CPU twin: tone_eq_at.
fn dev_tone_eq_at(e: f32) -> f32 {
    return dev_local_lut_at(clamp((e + 14.0) / 20.0, 0.0, 1.0));
}

// Display-linear gamut resolution: pull chroma toward the clamped luminance
// just enough that every channel fits [0,1]. CPU twin: gamut_clip_chroma.
fn dev_gamut_clip_chroma(c: vec3<f32>) -> vec3<f32> {
    let y = clamp(dev_luma_lin(c), 0.0, 1.0);
    let d = c - vec3<f32>(y);
    var f = 1.0;
    if (d.r > 1e-6) { f = min(f, (1.0 - y) / d.r); } else if (d.r < -1e-6) { f = min(f, y / -d.r); }
    if (d.g > 1e-6) { f = min(f, (1.0 - y) / d.g); } else if (d.g < -1e-6) { f = min(f, y / -d.g); }
    if (d.b > 1e-6) { f = min(f, (1.0 - y) / d.b); } else if (d.b < -1e-6) { f = min(f, y / -d.b); }
    f = clamp(f, 0.0, 1.0);
    return clamp(vec3<f32>(y) + d * f, vec3(0.0), vec3(1.0));
}

// CPU twins: filmlike_clip / compress_highlight_chroma.
fn dev_filmlike_clip(c: vec3<f32>) -> vec3<f32> {
    let lo0 = min(min(c.r, c.g), c.b);
    let hi = max(max(c.r, c.g), c.b);
    if (hi <= 1.0) { return c; }
    if (lo0 >= 1.0 || hi - lo0 <= 1e-8) { return vec3<f32>(1.0); }
    let lo = max(lo0, 0.0);
    let scale = (1.0 - lo) / max(hi - lo, 1e-8);
    return vec3<f32>(lo) + (c - vec3<f32>(lo)) * scale;
}

fn dev_compress_highlight_chroma(c: vec3<f32>, mapped_n: f32, scene_n: f32) -> vec3<f32> {
    let ratio = mapped_n / max(scene_n, 1e-8);
    if (ratio >= 1.0) { return c; }
    let compression = dev_smootherstep(0.02, 0.80, 1.0 - ratio);
    let highlight = dev_smootherstep(0.55, 1.0, mapped_n);
    let amount = compression * highlight * 0.65;
    return mix(c, vec3<f32>(mapped_n), amount);
}

// Interpolated display-domain luma curve (curve_* sliders + point curve).
fn dev_display_lum_at(v: f32) -> f32 {
    let x = clamp(v, 0.0, 1.0) * 255.0;
    let i0 = u32(floor(x));
    let i1 = min(i0 + 1u, 255u);
    let t = x - f32(i0);
    return mix(dev_rgb_curve[769u + i0], dev_rgb_curve[769u + i1], t);
}

// The full scene chain: linear scene RGB → display-referred gamma sRGB.
// CPU twin: SceneToneData::scene_to_display (region_e from the proxy).
fn dev_scene_display(scene_rgb: vec3<f32>, local: vec2<f32>) -> vec3<f32> {
    let m0 = vec3<f32>(dev_effects[16], dev_effects[17], dev_effects[18]);
    let m1 = vec3<f32>(dev_effects[19], dev_effects[20], dev_effects[21]);
    let m2 = vec3<f32>(dev_effects[22], dev_effects[23], dev_effects[24]);
    var v = vec3<f32>(dot(m0, scene_rgb), dot(m1, scene_rgb), dot(m2, scene_rgb));
    if (u.adj_p[1].x > 0.5) {
        let e = dev_region_luma_at(local.x * u.layer_w, local.y * u.layer_h);
        v = v * exp2(dev_tone_eq_at(e));
    }
    // Per-channel sigmoid blended toward the max-RGB ratio path.
    let pc = vec3<f32>(dev_scene_lut(v.r), dev_scene_lut(v.g), dev_scene_lut(v.b));
    var outc = pc;
    let n = max(max(v.r, v.g), v.b);
    if (n > 1e-8) {
        let mapped_n = dev_scene_lut(n);
        let hw = dev_smootherstep(0.5, 1.0, mapped_n);
        let blend = 0.90 + (0.20 - 0.90) * hw;
        outc = mix(pc, v * (mapped_n / n), blend);
        outc = dev_compress_highlight_chroma(outc, mapped_n, n);
    }
    outc = dev_gamut_clip_chroma(dev_filmlike_clip(outc));
    var g = dev_linear_to_srgb(clamp(outc, vec3(0.0), vec3(1.0)));
    if (dev_effects[26] > 0.5) {
        let l = clamp(dev_luma(g), 0.0, 1.0);
        g = dev_apply_luma_target(g, dev_display_lum_at(l));
    }
    return clamp(g, vec3(0.0), vec3(1.0));
}

struct CrColorProxy {
    region: vec3<f32>,
    adjusted: vec3<f32>,
};

// Bilinear sample of the colour `region`/`adjusted` proxies at layer-local pixel
// (lx, ly). adj_p[2] = (color_on, downsample s, proxy_w, proxy_h). Both proxies
// share the same grid, so the weights are computed once. Mirrors the CPU
// `upsample_bilinear` mapping so preview and bake read the same field.
fn dev_color_proxy_at(lx: f32, ly: f32) -> CrColorProxy {
    let pw = u32(u.adj_p[2].z);
    let ph = u32(u.adj_p[2].w);
    var out: CrColorProxy;
    if (pw == 0u || ph == 0u) {
        out.region = vec3<f32>(0.0);
        out.adjusted = vec3<f32>(0.0);
        return out;
    }
    let origin = vec2<f32>(u.adj_p[1].z, u.adj_p[1].w);
    let s = max(u.adj_p[2].y, 1.0);
    let fx = clamp((lx - origin.x + 0.5) / s - 0.5, 0.0, f32(pw - 1u));
    let fy = clamp((ly - origin.y + 0.5) / s - 0.5, 0.0, f32(ph - 1u));
    let x0 = u32(floor(fx));
    let y0 = u32(floor(fy));
    let x1 = min(x0 + 1u, pw - 1u);
    let y1 = min(y0 + 1u, ph - 1u);
    let wx = fx - f32(x0);
    let wy = fy - f32(y0);
    let i00 = (y0 * pw + x0) * 3u;
    let i10 = (y0 * pw + x1) * 3u;
    let i01 = (y1 * pw + x0) * 3u;
    let i11 = (y1 * pw + x1) * 3u;
    let r00 = vec3<f32>(dev_region_rgb[i00], dev_region_rgb[i00 + 1u], dev_region_rgb[i00 + 2u]);
    let r10 = vec3<f32>(dev_region_rgb[i10], dev_region_rgb[i10 + 1u], dev_region_rgb[i10 + 2u]);
    let r01 = vec3<f32>(dev_region_rgb[i01], dev_region_rgb[i01 + 1u], dev_region_rgb[i01 + 2u]);
    let r11 = vec3<f32>(dev_region_rgb[i11], dev_region_rgb[i11 + 1u], dev_region_rgb[i11 + 2u]);
    out.region = mix(mix(r00, r10, wx), mix(r01, r11, wx), wy);
    let a00 = vec3<f32>(dev_adjusted_rgb[i00], dev_adjusted_rgb[i00 + 1u], dev_adjusted_rgb[i00 + 2u]);
    let a10 = vec3<f32>(dev_adjusted_rgb[i10], dev_adjusted_rgb[i10 + 1u], dev_adjusted_rgb[i10 + 2u]);
    let a01 = vec3<f32>(dev_adjusted_rgb[i01], dev_adjusted_rgb[i01 + 1u], dev_adjusted_rgb[i01 + 2u]);
    let a11 = vec3<f32>(dev_adjusted_rgb[i11], dev_adjusted_rgb[i11 + 1u], dev_adjusted_rgb[i11 + 2u]);
    out.adjusted = mix(mix(a00, a10, wx), mix(a01, a11, wx), wy);
    return out;
}

// Membership of colour `c` in the EDITED bands' hues: the CPU-built gate LUT
// (dev_rgb_curve[1025..1385], 1°/entry, periodic) sampled at the pixel's UCS
// hue, times the logistic saturation weight (midpoint 0.06 — mirrors CPU
// MIXER_SAT_SHIFT). Exact mirror of CPU `band_affinity`, down to the LUT wrap.
fn dev_band_affinity(c: vec3<f32>) -> f32 {
    let h = dev_ucs_hue(c);
    let t = fract((h + 3.14159265) / 6.28318531) * 360.0;
    let i = min(u32(t), 359u);
    let f = t - f32(i);
    let a = dev_rgb_curve[1025u + i];
    let b = dev_rgb_curve[1025u + ((i + 1u) % 360u)];
    let gate = a + (b - a) * f;
    let w = dev_mixer_weight(c, 0.06);
    return clamp(gate * w, 0.0, 1.0);
}

// Full-res spatial MEMBERSHIP indicator for mixer-band edits (active when
// dev_effects[4] > 0.5): once the pixel belongs to an edited hue the gate opens
// fully — the knees sit below the curve's response onset so in-between hues
// (partial responders) receive their proxy delta whole, matching the commit.
// A nearly-pure edited-band region still carries specks/neutrals inside; a
// boundary mix shuts the gate. Mirrors CPU `mixer_edit_affinity`
// (REGATE_LO 0.02, REGATE_HI 0.12). Reads the SMOOTH region (guided low-pass) —
// "prefilter chromaticity": band membership follows the
// neighbourhood hue so it can't flip pixel to pixel across a soft edge or JPEG
// chroma block, and the edit tapers across a boundary. A uniform object's
// low-pass equals its pixels, so its interior is unchanged.
fn dev_mixer_edit_affinity(region: vec3<f32>) -> f32 {
    return dev_smootherstep(0.02, 0.12, dev_band_affinity(region));
}

// Recombine the toned pixel with its (colour-)adjusted region plus re-added detail:
// luminance detail in full, chroma detail attenuated, and the colour change gated
// by the REGIONAL (guided low-pass) chroma-rescued luma and band membership, so
// the edit TAPERS across a colour/tone boundary instead of a hard 1-px edge; the
// per-pixel detail below keeps interior sharpness. The gate is applied a SINGLE
// time. Mirrors CPU `finish_colored_pixel_f32` (CHROMA_DETAIL_KEEP 0.5,
// SHADOW_COLOR_RESCUE 0.60).
fn dev_finish_colored(toned: vec3<f32>, region: vec3<f32>, adjusted: vec3<f32>) -> vec3<f32> {
    let d = toned - region;
    let dl = dev_luma(d);
    // PER-PIXEL shadow/neutral fade (pixel's OWN luma/chroma): protects a thin
    // dark strand lying over a bright coloured region from being lifted toward
    // white — it averages out of the region, so a region-based fade would lift
    // it. Mass boundaries stay smooth via the edge suppression baked into the
    // adjusted proxy. Mirrors CPU `finish_colored_pixel_f32`.
    let lt = clamp(dev_luma(toned), 0.0, 1.0);
    let ct = dev_chroma(toned);
    var gate = dev_smootherstep(0.0, 0.32, lt + ct * 0.60);
    if (dev_effects[4] > 0.5) {
        gate = gate * dev_mixer_edit_affinity(region);
    }
    let dlv = vec3<f32>(dl, dl, dl);
    // Adaptive chroma-detail keep: small deviations (JPEG chroma blocks) stay
    // attenuated at 0.5, big ones (real colour boundaries) keep their colour.
    // Reconstruct the FULL adjusted region + detail, then blend toward the pixel
    // by the gate ONCE. Gate → 0 returns the exact toned pixel (bit-faithful).
    let cd = d - dlv;
    let keep = 0.5 + 0.5 * dev_smootherstep(0.08, 0.22, length(cd));
    let full = clamp(adjusted + dlv + cd * keep, vec3(0.0), vec3(1.0));
    let toned_c = clamp(toned, vec3(0.0), vec3(1.0));
    return clamp(toned_c + (full - toned_c) * gate, vec3(0.0), vec3(1.0));
}

// Develop Effects. `base_l` is the pixel's edge-aware regional luminance after
// tone (dev_luma of the colour region proxy on the colour path) — it makes
// Clarity a real local-contrast op and Dehaze a veil removal, matching the CPU
// `apply_effects` (CLARITY_GAIN 2.2 / CLARITY_LIMIT 0.28 / DEHAZE 0.7, 0.25).
// dev_effects = raw [texture, clarity, dehaze, vignette].
fn dev_effects_stage(c_in: vec3<f32>, local: vec2<f32>, base_l: f32) -> vec3<f32> {
    var c = c_in;
    let texture_raw = dev_effects[0];
    let clarity_raw = dev_effects[1];
    let dehaze_raw = dev_effects[2];
    let vignette_raw = dev_effects[3];
    if (dev_active(texture_raw)) {
        let l = clamp(dev_luma(c), 0.0, 1.0);
        let mid = dev_bell(l, 0.5, 0.56);
        let texture = dev_eased(texture_raw) * (200.0 / 300.0);
        c = dev_soft_contrast(c, clamp(texture * mid * 0.55, -0.62, 0.78), 0.85);
    }
    if (dev_active(clarity_raw)) {
        let l = clamp(dev_luma(c), 0.0, 1.0);
        let base = clamp(base_l, 0.0, 1.0);
        let k = dev_eased(clarity_raw) * (200.0 / 180.0) * 2.2;
        let mid = dev_bell(base, 0.5, 0.56);
        let boost = k * mid * (l - base);
        let delta = 0.28 * tanh(boost / 0.28);
        c = dev_tone_delta(c, delta);
    }
    if (dev_active(dehaze_raw)) {
        let base = clamp(base_l, 0.0, 1.0);
        let d = clamp(dev_eased(dehaze_raw) * (200.0 / 160.0), -1.0, 1.0);
        if (d > 0.0) {
            let veil = dev_smootherstep(0.25, 0.95, base);
            let t = max(1.0 - d * veil * 0.7, 0.25);
            let a = 1.0 - t;
            c = (c - vec3<f32>(a, a, a)) / t;
        } else {
            let m = -d * 0.45 * dev_smootherstep(0.10, 0.90, base);
            c = c * (1.0 - m) + vec3<f32>(m, m, m);
        }
    }
    if (dev_active(vignette_raw)) {
        let vignette = dev_eased(vignette_raw);
        let n = local - vec2<f32>(0.5, 0.5);
        let edge = clamp(length(n) / 0.707, 0.0, 1.0);
        let amount = dev_smootherstep(0.18, 1.0, edge) * abs(vignette) * 0.42;
        let delta = select(amount, -amount, vignette > 0.0);
        c = dev_tone_delta(c, delta);
    }
    return clamp(c, vec3(0.0), vec3(1.0));
}

// Develop tone stage: White Balance + Exposure as per-channel linear gains,
// then the tone curve on luminance, then chroma re-applied around the new luma.
// adj_p[0] = (gain_r, gain_g, gain_b, ev). Mirrors CPU ToneData::apply / apply_local.
// When adj_p[1].x==1 the local-adaptation path runs: the global curve (u.lut, here
// WITHOUT Highlights/Shadows) is read at the pixel luma and the H/S/W/B offset is
// read at the REGIONAL luma. When adj_p[2].x==1 the colour stage recombines the
// region/adjusted proxies. Both match the CPU bake, so there is no pop on commit.
// Interleaved-gradient ordered dither in [0,1) — cheap, seam-free, no array.
fn dev_ign(p: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(dot(p, vec2<f32>(0.06711056, 0.00583715))));
}

// Detail preservation for the local-adaptation lift. Mirrors the CPU
// `local_detail_boost` exactly (EPS 0.02, MAX 2.2, strength 0.5, keep band
// 0.10–0.35).
fn dev_local_detail_boost(l: f32, base: f32, offset: f32) -> f32 {
    if (abs(offset) <= 1e-4) {
        return 0.0;
    }
    let base_new = clamp(base + offset, 0.0, 1.0);
    var ratio: f32;
    if (offset > 0.0) {
        ratio = (base_new + 0.02) / (base + 0.02);
    } else {
        ratio = (1.0 - base_new + 0.02) / (1.0 - base + 0.02);
    }
    let gain = 1.0 + (clamp(ratio, 1.0, 2.2) - 1.0) * 0.5;
    let d = l - base;
    let texture_w = 1.0 - dev_smootherstep(0.10, 0.35, abs(d));
    return d * (gain - 1.0) * texture_w;
}

// Interpolated lookup into one channel of the R/G/B point-curve tables.
fn dev_rgb_curve_at(ch: u32, v: f32) -> f32 {
    let x = clamp(v, 0.0, 1.0) * 255.0;
    let i0 = u32(floor(x));
    let i1 = min(i0 + 1u, 255u);
    let t = x - f32(i0);
    let base = 1u + ch * 256u;
    return mix(dev_rgb_curve[base + i0], dev_rgb_curve[base + i1], t);
}

fn develop_apply(srgb_in: vec3<f32>, local: vec2<f32>) -> vec3<f32> {
    var toned: vec3<f32>;
    if (u.adj_pad_c == 1u) {
        // Scene-referred session: ignore the 8-bit atlas value entirely and run
        // the full scene chain from the f16 linear master (no input dither
        // needed — the source is not quantized). textureSampleLevel is legal in
        // non-uniform control flow. Display curves are inside the chain, so the
        // rgb-point-curve block below is skipped for this path (scene sessions
        // fold them into dev_scene_display's display stage instead).
        let scene_rgb = textureSampleLevel(dev_scene_tex, samp, local, 0.0).rgb;
        toned = dev_scene_display(scene_rgb, local);
    } else {
    // Tone stage (skipped when inactive, so a Colour-only edit does not roll off
    // highlights — matching the CPU bake's `tone = None`).
    toned = clamp(srgb_in, vec3(0.0), vec3(1.0));
    // The preview atlas is 8-bit, so a 16-bit source (e.g. a RAW) loses its smooth
    // gradients here; a hard tone push then shows 256-step banding that the 16-bit
    // commit doesn't have. Add a sub-LSB ordered dither (~±0.5 of an 8-bit step,
    // per channel) BEFORE the curve so the banding dissolves into imperceptible
    // noise, matching the smooth commit. Cost is one fract-hash per channel.
    let dpx = vec2<f32>(local.x * u.layer_w, local.y * u.layer_h);
    let dither = vec3<f32>(
        dev_ign(dpx) - 0.5,
        dev_ign(dpx + vec2<f32>(11.3, 7.1)) - 0.5,
        dev_ign(dpx + vec2<f32>(23.7, 19.5)) - 0.5,
    ) * (1.0 / 255.0);
    toned = clamp(toned + dither, vec3(0.0), vec3(1.0));
    if (u.adj_pad_a == 1u) {
        let gp = u.adj_p[0];
        let gain = vec3<f32>(gp.x, gp.y, gp.z);
        let ev = gp.w;
        var lin = dev_srgb_to_linear(toned) * gain * ev;
        let l0_ro = max(dev_luma_lin(lin), 0.0);
        let target_ro = dev_highlight_rolloff(l0_ro);
        if (l0_ro > 1e-6) {
            lin = lin * (target_ro / l0_ro);
        }
        lin = dev_fit_linear_rgb_to_luma(lin, target_ro);
        let rgb = clamp(dev_linear_to_srgb(lin), vec3(0.0), vec3(1.0));
        let l = clamp(dev_luma(rgb), 0.0, 1.0);
        var target_l = dev_lut(l);
        if (u.adj_p[1].x > 0.5) {
            let region_l = dev_region_luma_at(local.x * u.layer_w, local.y * u.layer_h);
            let offset = dev_local_lut_at(region_l);
            target_l = clamp(
                dev_lut(l) + offset + dev_local_detail_boost(l, region_l, offset),
                0.0, 1.0,
            );
        }
        toned = dev_apply_luma_target(rgb, target_l);
    }
    }
    // Per-channel point curves, after the luma tone stage — mirrors the CPU
    // ToneData::apply* order exactly. Scene sessions apply them inside
    // dev_scene_display's display stage instead (dev_rgb_curve[0] is 0 there
    // only when the curves are identity; when set, applying here matches the
    // CPU scene chain's apply_display_curves order: luma curve, then R/G/B).
    if (dev_rgb_curve[0] > 0.5) {
        toned = vec3<f32>(
            dev_rgb_curve_at(0u, toned.r),
            dev_rgb_curve_at(1u, toned.g),
            dev_rgb_curve_at(2u, toned.b),
        );
    }
    if (u.adj_p[2].x > 0.5) {
        let cp = dev_color_proxy_at(local.x * u.layer_w, local.y * u.layer_h);
        toned = dev_finish_colored(toned, cp.region, cp.adjusted);
        if (u.adj_p[2].x > 1.5) {
            return clamp(toned, vec3(0.0), vec3(1.0));
        }
        // Colour path: the toned region low-pass doubles as the spatial
        // effects' base, exactly like the CPU `finish_colored_pixel_f32`.
        return dev_effects_stage(toned, local, dev_luma(cp.region));
    }
    // No colour and no fast proxy → the Effects sliders are all zero here (they
    // route through the fast proxy); the base only feeds inert branches.
    return dev_effects_stage(toned, local, clamp(dev_luma(toned), 0.0, 1.0));
}

// Bayer-8 threshold (0..1), identical matrix to the CPU `bayer8` so the GPU
// preview dither matches the committed bake's character.
fn dev_bayer8(x: u32, y: u32) -> f32 {
    var b = array<f32, 64>(
        0.0,  32.0, 8.0,  40.0, 2.0,  34.0, 10.0, 42.0,
        48.0, 16.0, 56.0, 24.0, 50.0, 18.0, 58.0, 26.0,
        12.0, 44.0, 4.0,  36.0, 14.0, 46.0, 6.0,  38.0,
        60.0, 28.0, 52.0, 20.0, 62.0, 30.0, 54.0, 22.0,
        3.0,  35.0, 11.0, 43.0, 1.0,  33.0, 9.0,  41.0,
        51.0, 19.0, 59.0, 27.0, 49.0, 17.0, 57.0, 25.0,
        15.0, 47.0, 7.0,  39.0, 13.0, 45.0, 5.0,  37.0,
        63.0, 31.0, 55.0, 23.0, 61.0, 29.0, 53.0, 21.0,
    );
    let i = (y & 7u) * 8u + (x & 7u);
    return (b[i] + 0.5) / 64.0;
}

// Ordered (Bayer-8) dither in 8-bit sRGB space, mirroring the CPU bake's
// `quantize_dither`. The compositor stores into an Rgba8 sRGB target, so a smooth
// Develop colour gradient (the saturation-boosted regional low-pass) would
// posterize into visible bands without this — the bake dithers, the live preview
// did not, hence the "striping only in preview" artifact. Keyed to framebuffer
// pixel coords so the pattern stays a crisp 1px at any zoom; the per-channel
// offset stops R/G/B sharing one pattern (which would dither luma only and leave
// chroma banding).
fn dev_dither_srgb(c: vec3<f32>, frag: vec2<f32>) -> vec3<f32> {
    let px = u32(frag.x);
    let py = u32(frag.y);
    let d = vec3<f32>(
        dev_bayer8(px,      py     ) - 0.5,
        dev_bayer8(px + 2u, py + 5u) - 0.5,
        dev_bayer8(px + 5u, py + 2u) - 0.5,
    ) * (1.0 / 255.0);
    return clamp(c + d, vec3<f32>(0.0), vec3<f32>(1.0));
}

// ── Tile-atlas helpers ───────────────────────────────────────────────────────

fn sample_tile_nearest_i(lx: i32, ly: i32) -> vec4<f32> {
    if (lx < 0 || ly < 0) { return vec4<f32>(0.0); }
    let ux = u32(lx);
    let uy = u32(ly);
    let tx = ux / 256u;
    let ty = uy / 256u;
    if (tx >= u.layer_tiles_w || ty >= u.layer_tiles_h) { return vec4<f32>(0.0); }
    let tile_idx = ty * u.layer_tiles_w + tx;
    let slot = tile_map[tile_idx];
    if (slot < 0) { return vec4<f32>(0.0); }
    let slot_x = u32(slot & 0xFFFF);
    let slot_y = u32(slot >> 16u);
    let atlas_x = f32(slot_x * 256u + ux % 256u) + 0.5;
    let atlas_y = f32(slot_y * 256u + uy % 256u) + 0.5;
    let atlas_dim = vec2<f32>(textureDimensions(atlas_tex));
    return textureSample(atlas_tex, samp, vec2<f32>(atlas_x / atlas_dim.x, atlas_y / atlas_dim.y));
}

fn sample_tile_nearest_clamped_i(lx: i32, ly: i32) -> vec4<f32> {
    let max_x = max(u.layer_w - 1.0, 0.0);
    let max_y = max(u.layer_h - 1.0, 0.0);
    return sample_tile_nearest_i(
        clamp(lx, 0, i32(max_x)),
        clamp(ly, 0, i32(max_y)),
    );
}

fn sample_tile_bilinear_clamped(lx: f32, ly: f32) -> vec4<f32> {
    let ix = i32(floor(lx));
    let iy = i32(floor(ly));
    let fx = fract(lx);
    let fy = fract(ly);
    let c00 = sample_tile_nearest_clamped_i(ix,     iy    );
    let c10 = sample_tile_nearest_clamped_i(ix + 1, iy    );
    let c01 = sample_tile_nearest_clamped_i(ix,     iy + 1);
    let c11 = sample_tile_nearest_clamped_i(ix + 1, iy + 1);
    return mix(mix(c00, c10, fx), mix(c01, c11, fx), fy);
}

fn sample_tile_bilinear(lx: f32, ly: f32) -> vec4<f32> {
    let ix = i32(floor(lx));
    let iy = i32(floor(ly));
    let fx = fract(lx);
    let fy = fract(ly);
    let c00 = sample_tile_nearest_i(ix,     iy    );
    let c10 = sample_tile_nearest_i(ix + 1, iy    );
    let c01 = sample_tile_nearest_i(ix,     iy + 1);
    let c11 = sample_tile_nearest_i(ix + 1, iy + 1);
    return mix(mix(c00, c10, fx), mix(c01, c11, fx), fy);
}

fn filter_weighted_sample(lx: f32, ly: f32, weight: f32) -> vec4<f32> {
    let c = sample_tile_bilinear_clamped(lx, ly);
    return vec4<f32>(c.rgb * c.a * weight, c.a * weight);
}

fn filter_preview_blur(lx: f32, ly: f32, radius_in: f32) -> vec4<f32> {
    let radius = clamp(radius_in, 0.0, 250.0);
    if (radius <= 0.01) {
        return sample_tile_bilinear_clamped(lx, ly);
    }

    let sigma = max(radius, 0.5);
    let support = sigma * 2.75;
    let golden_angle = 2.39996323;
    let sample_count = 72.0;

    var acc = vec4<f32>(0.0);
    var total_w = 0.0;

    for (var i: i32 = 0; i < 72; i = i + 1) {
        let fi = f32(i);
        let t = (fi + 0.5) / sample_count;
        let d = sqrt(t) * support;
        let a = fi * golden_angle;
        let w = exp(-0.5 * (d * d) / (sigma * sigma));
        acc += filter_weighted_sample(lx + cos(a) * d, ly + sin(a) * d, w);
        total_w += w;
    }

    let out_a = acc.a / max(total_w, 0.00001);
    if (acc.a <= 0.00001) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(acc.rgb / acc.a, out_a);
}

fn filter_noise_hash(p: vec2<f32>, c: f32) -> f32 {
    return fract(sin(dot(p + vec2<f32>(c * 17.13, c * 3.71), vec2<f32>(12.9898, 78.233))) * 43758.5453);
}

fn apply_filter_preview(src_in: vec4<f32>, lx: f32, ly: f32) -> vec4<f32> {
    let P0 = u.adj_p[0];
    switch u.adj_kind {
        case 30u: { // Gaussian Blur
            return filter_preview_blur(lx, ly, P0.x);
        }
        case 31u: { // Sharpen / Unsharp preview
            let amount = P0.x;
            let blur = filter_preview_blur(lx, ly, P0.y);
            return vec4<f32>(clamp(src_in.rgb + (src_in.rgb - blur.rgb) * amount, vec3(0.0), vec3(1.0)), src_in.a);
        }
        case 32u: { // High Pass
            let blur = filter_preview_blur(lx, ly, P0.x);
            return vec4<f32>(clamp(src_in.rgb - blur.rgb + vec3<f32>(0.5), vec3(0.0), vec3(1.0)), src_in.a);
        }
        case 33u: { // Add Noise
            let amount = clamp(P0.x / 100.0, 0.0, 1.0);
            let mono = P0.y > 0.5;
            let mag = amount;
            let n0 = (filter_noise_hash(vec2<f32>(lx, ly), 0.0) - 0.5) * 2.0 * mag;
            if (mono) {
                return vec4<f32>(clamp(src_in.rgb + vec3<f32>(n0), vec3(0.0), vec3(1.0)), src_in.a);
            }
            let n1 = (filter_noise_hash(vec2<f32>(lx, ly), 1.0) - 0.5) * 2.0 * mag;
            let n2 = (filter_noise_hash(vec2<f32>(lx, ly), 2.0) - 0.5) * 2.0 * mag;
            return vec4<f32>(clamp(src_in.rgb + vec3<f32>(n0, n1, n2), vec3(0.0), vec3(1.0)), src_in.a);
        }
        case 34u: { // Pixelate
            let cell = max(P0.x, 1.0);
            let cx = floor(lx / cell) * cell + cell * 0.5;
            let cy = floor(ly / cell) * cell + cell * 0.5;
            return sample_tile_bilinear_clamped(cx, cy);
        }
        case 35u: { // Reduce Noise approximation
            let s = clamp(P0.x / 100.0, 0.0, 1.0);
            let blur = filter_preview_blur(lx, ly, 0.8 + s * 2.2);
            let edge = clamp(length(src_in.rgb - blur.rgb) / 0.18, 0.0, 1.0);
            let mix_a = s * (1.0 - edge);
            return vec4<f32>(mix(src_in.rgb, blur.rgb, mix_a), src_in.a);
        }
        default: {
            return src_in;
        }
    }
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
    if (u.mask_inverted == 1u) {
        return 1.0 - value;
    }
    return value;
}

fn sample_mask_bilinear(lx: f32, ly: f32) -> f32 {
    if (u.mask_enabled == 0u) { return 1.0; }
    let ix = i32(floor(lx));
    let iy = i32(floor(ly));
    let fx = fract(lx);
    let fy = fract(ly);
    let c00 = sample_mask_nearest_i(ix,     iy    );
    let c10 = sample_mask_nearest_i(ix + 1, iy    );
    let c01 = sample_mask_nearest_i(ix,     iy + 1);
    let c11 = sample_mask_nearest_i(ix + 1, iy + 1);
    return mix(mix(c00, c10, fx), mix(c01, c11, fx), fy);
}

// Live clipping-mask / PowerClip pin. `clip_shift_packed` is 0 for a non-clip
// layer, and for a clip child holds two i16 pin deltas biased by 0x8000 (so the
// word is never 0). `clip_is_child` gates the pin; `clip_shift` unpacks it.
fn clip_is_child() -> bool { return u.clip_shift_packed != 0u; }
fn clip_shift() -> vec2<i32> {
    return vec2<i32>(
        i32(u.clip_shift_packed >> 16u) - 0x8000,
        i32(u.clip_shift_packed & 0xFFFFu) - 0x8000,
    );
}
// Sample the clip mask at a canvas-fixed layer-local coord. Out of the frame's
// baked footprint reads as hidden (0) rather than edge-clamped, so a frame that
// touches the content edge can't smear when the image is dragged/transformed.
fn clip_mask_at(lx: i32, ly: i32, lw: f32, lh: f32) -> f32 {
    if (lx < 0 || ly < 0 || lx >= i32(lw) || ly >= i32(lh)) { return 0.0; }
    return sample_mask_nearest_i(lx, ly);
}

// ── Per-layer adjustment preview (adj_kind 1..=13) ────────────────────────────
// These functions are a VERBATIM copy of the ones in ADJUSTMENT_SHADER so the
// live Ctrl+L/M preview (applied to a raster layer's own pixels here) matches
// the adjustment-LAYER pipeline and the CPU bake exactly. Keep both copies and
// core::layer::AdjustmentType::apply_pixel in sync.
fn lum299(c: vec3<f32>) -> f32 { return dot(c, vec3(0.299, 0.587, 0.114)); }

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

// ── Fragment shader ──────────────────────────────────────────────────────────
// NOTE: the Detail stage (wavelet sharpening / NR) has NO live GPU mirror — it
// previews through the debounced CPU refine bake, which runs the real commit
// pipeline (zero parity risk).

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let dst = textureSample(dst_tex, samp, in.uv);

    let screen_x = in.uv.x * u.viewport_w;
    let screen_y = in.uv.y * u.viewport_h;

    let canvas_x = screen_x / u.zoom + u.view_offset_x;
    let canvas_y = screen_y / u.zoom + u.view_offset_y;

    var src: vec4<f32>;
    var mask_a: f32 = 1.0;
    var dev_local: vec2<f32> = vec2<f32>(0.5, 0.5);
    var filter_lx: f32 = 0.0;
    var filter_ly: f32 = 0.0;

    if (u.xform_active == 2u) {
        // Free Transform uses a full inverse homography. The existing twelve
        // transform slots are packed as m00..m22, layer origin x/y, spare.
        let w = u.xform_tx * canvas_x + u.xform_ty * canvas_y + u.xform_orig_ox;
        if (abs(w) < 0.000001) {
            return dst;
        }
        let src_canvas_x = (u.xform_inv_a * canvas_x + u.xform_inv_b * canvas_y + u.xform_inv_c) / w;
        let src_canvas_y = (u.xform_inv_d * canvas_x + u.xform_pivot_x * canvas_y + u.xform_pivot_y) / w;
        let slx = src_canvas_x - u.xform_orig_oy;
        let sly = src_canvas_y - u.xform_orig_w;
        if (slx < 0.0 || sly < 0.0 || slx >= u.layer_w || sly >= u.layer_h) {
            return dst;
        }
        src = sample_tile_bilinear(slx, sly);
        // A clip child's frame stays fixed in canvas space while the image is
        // transformed, so sample the clip mask at the UN-transformed canvas
        // position (layer origin is packed in xform_orig_oy/xform_orig_w here), not
        // the rotated/scaled layer position. Non-clip masks transform with the layer.
        if (clip_is_child()) {
            let sh = clip_shift();
            mask_a = clip_mask_at(
                i32(canvas_x - u.xform_orig_oy) + sh.x,
                i32(canvas_y - u.xform_orig_w) + sh.y,
                u.layer_w, u.layer_h);
        } else {
            mask_a = sample_mask_bilinear(slx, sly);
        }
        dev_local = vec2<f32>(slx / max(u.layer_w, 1.0), sly / max(u.layer_h, 1.0));
        filter_lx = slx;
        filter_ly = sly;
    } else if (u.xform_active == 1u) {
        // ── Free-transform preview path ────────────────────────────────────
        // Inverse affine transform: output canvas pos to original layer-local pos.
        //   Forward: new_pos = pivot + translate + M * (orig_canvas - pivot)
        //   Inverse: (orig_canvas - pivot) = M_inv * (new_pos - pivot - translate)
        let dx = canvas_x - (u.xform_pivot_x + u.xform_tx);
        let dy = canvas_y - (u.xform_pivot_y + u.xform_ty);
        let ex = u.xform_inv_a * dx + u.xform_inv_b * dy;
        let ey = u.xform_inv_c * dx + u.xform_inv_d * dy;
        let slx = ex + u.xform_pivot_x - u.xform_orig_ox;
        let sly = ey + u.xform_pivot_y - u.xform_orig_oy;
        if (slx < 0.0 || sly < 0.0 || slx >= u.xform_orig_w || sly >= u.xform_orig_h) {
            return dst;
        }
        src = sample_tile_bilinear(slx, sly);
        // Clip child: sample the fixed frame at the un-transformed canvas position
        // (layer origin is xform_orig_ox/xform_orig_oy in this branch).
        if (clip_is_child()) {
            let sh = clip_shift();
            mask_a = clip_mask_at(
                i32(canvas_x - u.xform_orig_ox) + sh.x,
                i32(canvas_y - u.xform_orig_oy) + sh.y,
                u.xform_orig_w, u.xform_orig_h);
        } else {
            mask_a = sample_mask_bilinear(slx, sly);
        }
        dev_local = vec2<f32>(slx / max(u.xform_orig_w, 1.0), sly / max(u.xform_orig_h, 1.0));
        filter_lx = slx;
        filter_ly = sly;
    } else {
        // ── Normal (no transform) path ─────────────────────────────────────
        let layer_x = canvas_x - u.offset_x;
        let layer_y = canvas_y - u.offset_y;
        dev_local = vec2<f32>(layer_x / max(u.layer_w, 1.0), layer_y / max(u.layer_h, 1.0));

        if (layer_x < 0.0 || layer_y < 0.0 || layer_x >= u.layer_w || layer_y >= u.layer_h) {
            return dst;
        }

        let tile_x = u32(layer_x) / 256u;
        let tile_y = u32(layer_y) / 256u;

        if (tile_x >= u.layer_tiles_w || tile_y >= u.layer_tiles_h) {
            return dst;
        }

        let tile_idx = tile_y * u.layer_tiles_w + tile_x;
        let slot = tile_map[tile_idx];

        if (slot < 0) {
            return dst;
        }

        let slot_x = u32(slot & 0xFFFF);
        let slot_y = u32(slot >> 16u);

        let atlas_x = f32(slot_x * 256u) + (layer_x % 256.0);
        let atlas_y = f32(slot_y * 256u) + (layer_y % 256.0);

        let atlas_dim = vec2<f32>(textureDimensions(atlas_tex));
        let atlas_uv = vec2<f32>(atlas_x / atlas_dim.x, atlas_y / atlas_dim.y);
        src = textureSample(atlas_tex, samp, atlas_uv);
        // Live PowerClip / clipping-mask pin (Move path): sample the unmoved clip
        // mask at the canvas-fixed spot so a dragged clipped image stays inside its
        // frame. No-op for non-clip layers (clip_is_child false).
        if (clip_is_child()) {
            let sh = clip_shift();
            mask_a = clip_mask_at(i32(layer_x) + sh.x, i32(layer_y) + sh.y, u.layer_w, u.layer_h);
        } else {
            mask_a = sample_mask_nearest_i(i32(layer_x), i32(layer_y));
        }
        filter_lx = layer_x;
        filter_ly = layer_y;
    }

    if (u.adj_kind == 20u) {
        let srgb = dev_linear_to_srgb(src.rgb);
        let cr = develop_apply(srgb, dev_local);
        src = vec4<f32>(dev_srgb_to_linear(dev_dither_srgb(cr, in.pos.xy)), src.a);
    } else if (u.adj_kind >= 1u && u.adj_kind <= 13u) {
        // Live Ctrl+L/M preview on this raster layer's own pixels. Use the exact
        // sRGB curve (not pow 2.2): src is linear (Rgba8UnormSrgb atlas), so
        // dev_linear_to_srgb recovers the byte-space value the CPU bake operates
        // on — making this preview match the committed result.
        let srgb = dev_linear_to_srgb(src.rgb);
        let adj = apply_adjustment(srgb);
        src = vec4<f32>(dev_srgb_to_linear(adj), src.a);
    } else if (u.adj_kind >= 30u && u.adj_kind <= 35u) {
        src = apply_filter_preview(src, filter_lx, filter_ly);
    }

    var sa = src.a * u.opacity * mask_a;
    if (u.blend_mode == 17u) {
        let threshold = dissolve_hash(
            u32(max(floor(canvas_x), 0.0)),
            u32(max(floor(canvas_y), 0.0))
        );
        if (threshold >= sa) {
            return dst;
        }
        sa = 1.0;
    }
    if (sa <= 0.00001) {
        return dst;
    }
    let da = dst.a;
    let out_a = sa + da * (1.0 - sa);

    if (out_a == 0.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }

    // Photoshop's default setting blends directly in the RGB document space.
    // The atlas and ping-pong textures are sRGB formats, so texture sampling has
    // already decoded them to linear. Convert both operands back to sRGB for the
    // blend/composite math, then return to linear for the sRGB render target.
    let src_rgb = dev_linear_to_srgb(src.rgb);
    let dst_rgb = dev_linear_to_srgb(dst.rgb);

    if (u.blend_mode == 0u || u.blend_mode == 17u) {
        let out_rgb_pre = sa * src_rgb + da * (1.0 - sa) * dst_rgb;
        return vec4<f32>(dev_srgb_to_linear(out_rgb_pre / out_a), out_a);
    }

    let Cr = blend_comp(u.blend_mode, src_rgb, dst_rgb);
    let out_rgb_pre = (sa * da) * Cr
        + (sa * (1.0 - da)) * src_rgb
        + (da * (1.0 - sa)) * dst_rgb;
    let out_rgb = out_rgb_pre / out_a;

    return vec4<f32>(dev_srgb_to_linear(out_rgb), out_a);
}

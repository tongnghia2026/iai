struct GradientStop {
    color: vec4<f32>,
    params: vec4<f32>,
}

struct DrawUniforms {
    object_to_canvas: mat3x3<f32>,
    canvas_to_clip: mat3x3<f32>,
    object_to_gradient: mat3x3<f32>,
    paint_meta: vec4<u32>,
    stops: array<GradientStop, 8>,
}

@group(0) @binding(0) var<uniform> draw: DrawUniforms;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) object_position: vec2<f32>,
}

@vertex
fn vs_main(@location(0) position: vec2<f32>) -> VertexOut {
    var out: VertexOut;
    let canvas = draw.object_to_canvas * vec3<f32>(position, 1.0);
    let clip = draw.canvas_to_clip * canvas;
    out.position = vec4<f32>(clip.xy, 0.0, 1.0);
    out.object_position = position;
    return out;
}

fn srgb_to_linear_channel(c: f32) -> f32 {
    if c <= 0.04045 {
        return c / 12.92;
    }
    return pow((c + 0.055) / 1.055, 2.4);
}

fn sample_paint(object_position: vec2<f32>) -> vec4<f32> {
    let kind = draw.paint_meta.x;
    let count = max(draw.paint_meta.y, 1u);
    var color = draw.stops[0].color;
    if kind != 0u {
        let gp = draw.object_to_gradient * vec3<f32>(object_position, 1.0);
        var t = gp.x;
        if kind == 2u {
            t = length(gp.xy);
        }
        t = clamp(t, 0.0, 1.0);
        var right = count - 1u;
        for (var i = 0u; i < 8u; i = i + 1u) {
            if i < count && draw.stops[i].params.x >= t {
                right = i;
                break;
            }
        }
        if right == 0u {
            color = draw.stops[0].color;
        } else {
            let a = draw.stops[right - 1u];
            let b = draw.stops[right];
            let mix_t = clamp((t - a.params.x) / max(b.params.x - a.params.x, 0.000001), 0.0, 1.0);
            color = mix(a.color, b.color, mix_t);
        }
    }
    return vec4<f32>(
        srgb_to_linear_channel(color.r),
        srgb_to_linear_channel(color.g),
        srgb_to_linear_channel(color.b),
        color.a,
    );
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let color = sample_paint(in.object_position);
    return vec4<f32>(color.rgb * color.a, color.a);
}

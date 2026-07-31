struct DrawUniforms {
    object_to_canvas: mat3x3<f32>,
    canvas_to_clip: mat3x3<f32>,
    color: vec4<f32>,
}

@group(0) @binding(0) var<uniform> draw: DrawUniforms;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
}

@vertex
fn vs_main(@location(0) position: vec2<f32>) -> VertexOut {
    var out: VertexOut;
    let canvas = draw.object_to_canvas * vec3<f32>(position, 1.0);
    let clip = draw.canvas_to_clip * canvas;
    out.position = vec4<f32>(clip.xy, 0.0, 1.0);
    return out;
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    // Premultiplied-alpha output: MSAA resolve over a transparent clear averages
    // covered and uncovered samples, so an edge pixel is inherently premultiplied
    // by its coverage. Emitting premultiplied colour makes the whole target a
    // consistent premultiplied surface and lets overlapping shapes in one run
    // source-over correctly. A fully covered opaque sample keeps `color.rgb`.
    return vec4<f32>(draw.color.rgb * draw.color.a, draw.color.a);
}

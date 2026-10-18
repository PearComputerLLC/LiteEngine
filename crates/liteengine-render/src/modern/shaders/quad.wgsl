// Textured quad shader: background images, 2D sprites, 3D billboards and
// the speech-bubble/glyph overlay all draw through this pipeline.
// Vertex format (per vertex, 8 floats): x, y, u, v, r, g, b, a

struct QuadVertexInput {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
}

@group(0) @binding(0) var<uniform> ortho: mat4x4<f32>;

struct QuadOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
}

@vertex
fn vs_main(in: QuadVertexInput) -> QuadOutput {
    var out: QuadOutput;
    out.clip_pos = ortho * vec4<f32>(in.pos, 0.0, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: QuadOutput) -> @location(0) vec4<f32> {
    let t = textureSample(quad_tex, quad_sampler, in.uv);
    return vec4<f32>(in.color.rgb, in.color.a) * vec4<f32>(t.rgb, t.a);
}

@group(1) @binding(0) var quad_tex: texture_2d<f32>;
@group(1) @binding(1) var quad_sampler: sampler;

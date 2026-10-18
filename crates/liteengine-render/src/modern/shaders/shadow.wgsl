// Shadow depth pass: renders meshes into the sun's orthographic depth map.

struct VertexInput {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
}

@group(0) @binding(0) var<uniform> light_vp: mat4x4<f32>;
@group(1) @binding(0) var<uniform> model: mat4x4<f32>;

struct ShadowOutput {
    @builtin(position) clip_pos: vec4<f32>,
}

@vertex
fn vs_main(in: VertexInput) -> ShadowOutput {
    var out: ShadowOutput;
    out.clip_pos = light_vp * (model * vec4<f32>(in.pos, 1.0));
    return out;
}

// PBR mesh shader (Cook-Torrance GGX, sun + shadow map, hemispheric IBL).
// G-buffer outputs enable the screen-space RT pass (SSR) when raytracing
// mode is enabled: normals+view depth (target 1), metallic/roughness (target 2).
// NOTE: `common` (structs + BRDF helpers) is prepended by the renderer.

struct VertexOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) shadow_pos: vec4<f32>,
    @location(4) clip_w: f32,
}

struct VertexInput {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
}

@group(0) @binding(0) var<uniform> camera: Camera;
@group(0) @binding(1) var<uniform> sun: Sun;
@group(0) @binding(2) var<uniform> env: Env;
@group(0) @binding(3) var<uniform> light_mat: LightMatrix;
@group(1) @binding(0) var<uniform> model: ModelData;
@group(1) @binding(1) var mesh_tex: texture_2d<f32>;
@group(1) @binding(2) var mesh_sampler: sampler;
@group(1) @binding(3) var shadow_map: texture_depth_2d;
@group(1) @binding(4) var shadow_cmp: sampler_comparison;

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let wp = model.model * vec4<f32>(in.pos, 1.0);
    out.world_pos = wp.xyz;
    out.clip_pos = camera.view_proj * wp;
    // Uniform scale assumed: rotate the normal by the model matrix.
    out.normal = normalize((model.model * vec4<f32>(in.normal, 0.0)).xyz);
    out.uv = in.uv;
    out.shadow_pos = light_mat.light_vp * wp;
    out.clip_w = out.clip_pos.w;
    return out;
}

fn sample_shadow_soft(uv: vec2<f32>, depth: f32) -> f32 {
    let texel = 1.0 / 2048.0;
    var sum = textureSampleCompareLevel(shadow_map, shadow_cmp, uv, depth);
    let taps = 8.0;
    var count = 1.0;
    for (var i: i32 = 0; i < 8; i = i + 1) {
        // Golden-hour spiral taps (poisson-ish, constant offsets).
        let a = f32(i) * 2.399963;
        let r = (0.5 + f32(i) * 0.35) * texel * 2.2;
        let o = vec2<f32>(cos(a), sin(a)) * r;
        sum = sum + textureSampleCompareLevel(shadow_map, shadow_cmp, uv + o, depth);
        count = count + 1.0;
    }
    return sum / count;
}

struct MeshFragmentOutput {
    @location(0) color: vec4<f32>,
    @location(1) normal_depth: vec4<f32>,
    @location(2) material: vec4<f32>,
}

@fragment
fn fs_main(in: VertexOutput) -> MeshFragmentOutput {
    var out: MeshFragmentOutput;
    var albedo = model.material_color.rgb;
    var alpha = model.material_color.a;
    if model.params.z > 0.5 {
        let t = textureSample(mesh_tex, mesh_sampler, in.uv);
        albedo = albedo * t.rgb;
        alpha = alpha * t.a;
    }
    let n = normalize(in.normal);
    let v = normalize(camera.pos.xyz - in.world_pos);
    let l = -sun.dir.xyz;
    let h = normalize(v + l);
    let metallic = model.params.x;
    let roughness = clamp(model.params.y, 0.03, 1.0);

    // Cook-Torrance specular.
    let f0 = mix(vec3<f32>(0.04, 0.04, 0.04), albedo, metallic);
    let ndotl = max(dot(n, l), 0.0);
    let ndf = distribution_ggx(n, h, roughness);
    let geom = geometry_smith(n, v, l, roughness);
    let fres = fresnel_schlick(max(dot(h, v), 0.0), f0);

    let ndf_geom = ndf * geom;
    let denom = 4.0 * max(dot(n, v), 0.0) * ndotl + 0.0001;
    let specular = (ndf_geom / denom) * fres;
    let kd = (vec3<f32>(1.0, 1.0, 1.0) - fres) * (1.0 - metallic);

    // Shadow (soft 9-tap PCF).
    var shadow = 1.0;
    let sp = in.shadow_pos;
    if sp.w > 0.0 {
        let uv = sp.xy / sp.w;
        if uv.x > 0.0 && uv.x < 1.0 && uv.y > 0.0 && uv.y < 1.0 {
            let bias = 0.0018;
            shadow = sample_shadow_soft(uv, sp.z - bias);
        }
    }

    let sun_light = sun.color_intensity.rgb * sun.color_intensity.a;
    var color = kd * albedo * ambient_ibl(n, env) * env.params.x;
    color = color + (kd * albedo + specular) * sun_light * ndotl * shadow;
    // Environment reflection rim (IBL approximation of mirror contribution).
    color = color + env_color(reflect(-v, n), env) * specular * 2.0;

    out.color = vec4<f32>(color, alpha);
    out.normal_depth = vec4<f32>(n, in.clip_w);
    out.material = vec4<f32>(metallic, roughness, 0.0, 0.0);
    return out;
}

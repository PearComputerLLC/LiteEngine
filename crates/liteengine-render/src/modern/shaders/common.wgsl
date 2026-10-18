// Common WGSL types shared by LiteEngine's modern (wgpu) pipelines.

struct Camera {
    view_proj: mat4x4<f32>,
    inv_vp: mat4x4<f32>,
    pos: vec4<f32>,          // xyz = camera world position
}

struct Sun {
    dir: vec4<f32>,          // xyz = direction the light travels (normalized)
    color_intensity: vec4<f32>, // rgb = color, a = intensity
}

struct Env {
    sky_top: vec4<f32>,
    sky_horizon: vec4<f32>,
    ground: vec4<f32>,
    params: vec4<f32>,       // x = ambient strength
}

struct ModelData {
    model: mat4x4<f32>,
    material_color: vec4<f32>, // rgb = base color, a = alpha
    params: vec4<f32>,         // x = metallic, y = roughness, z = textured flag, w = unused
}

struct LightMatrix {
    light_vp: mat4x4<f32>,
}

struct PresentParams {
    screen: vec4<f32>,   // x = width, y = height, z = rt_on, w = shadow_soft
    sun_dir: vec4<f32>,
    sky_top: vec4<f32>,
    sky_horizon: vec4<f32>,
    ground: vec4<f32>,
    cam_pos: vec4<f32>,
}

const PI: f32 = 3.141592653589793;

// Schlick Fresnel
fn fresnel_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    return f0 + (vec3<f32>(1.0, 1.0, 1.0) - f0) * pow(1.0 - cos_theta, 5.0);
}

// GGX/Trowbridge-Reitz normal distribution
fn distribution_ggx(n: vec3<f32>, h: vec3<f32>, roughness: f32) -> f32 {
    let a = roughness * roughness;
    let a2 = a * a;
    let ndoth = max(dot(n, h), 0.0);
    let d = ndoth * ndoth * (a2 - 1.0) + 1.0;
    return a2 / max(PI * d * d, 0.0001);
}

// Smith geometry term (direct lighting)
fn geometry_smith(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, roughness: f32) -> f32 {
    let k = (roughness + 1.0) * (roughness + 1.0) / 8.0;
    let ndotv = max(dot(n, v), 0.0);
    let ndotl = max(dot(n, l), 0.0);
    let g1v = ndotv / (ndotv * (1.0 - k) + k);
    let g1l = ndotl / (ndotl * (1.0 - k) + k);
    return g1v * g1l;
}

// Hemispheric ambient (IBL proxy): sky above, horizon at eye level, ground below.
fn ambient_ibl(n: vec3<f32>, env: Env) -> vec3<f32> {
    let up = vec3<f32>(0.0, 1.0, 0.0);
    let t = clamp(dot(n, up) * 0.5 + 0.5, 0.0, 1.0);
    let sky = mix(env.sky_horizon.rgb, env.sky_top.rgb, t * t);
    return mix(env.ground.rgb, sky, clamp(dot(n, up) * 4.0 + 0.5, 0.0, 1.0));
}

// ACES filmic tonemap (Narkowicz approximation)
fn aces_tonemap(x: vec3<f32>) -> vec3<f32> {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((x * (a * x + b)) / (x * (c * x + d) + e), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn env_color(dir: vec3<f32>, env: Env) -> vec3<f32> {
    if dir.y < 0.0 {
        return env.ground.rgb * 0.9;
    }
    let t = clamp(dir.y * 0.5 + 0.5, 0.0, 1.0);
    return mix(env.sky_horizon.rgb, env.sky_top.rgb, t * t);
}

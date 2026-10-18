// Present pass: tonemaps the HDR scene. When raytracing mode is enabled it
// also performs screen-space reflections (SSR) by marching the reflected
// ray against the G-buffer view depth, plus cheap screen-space AO.

struct FragmentOut {
    @builtin(position) frag_coord: vec4<f32>,
}

struct PresentInput {
    @builtin(position) frag_coord: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> PresentInput {
    var out: PresentInput;
    // Fullscreen triangle.
    let x = f32(i32(vi & 1u) * 4 - 1);
    let y = f32(i32(vi >> 1u) * 4 - 1);
    out.frag_coord = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, 1.0 - (y + 1.0) * 0.5);
    return out;
}

@group(0) @binding(0) var hdr_tex: texture_2d<f32>;
@group(0) @binding(1) var gbuf_nd: texture_2d<f32>;  // rgb = normal, a = clip w (view depth proxy)
@group(0) @binding(2) var gbuf_mat: texture_2d<f32>; // x = metallic, y = roughness
@group(0) @binding(3) var pp_sampler: sampler;
@group(0) @binding(4) var<uniform> params: PresentParams;

fn linearize_depth(ndc_z: f32, near: f32, far: f32) -> f32 {
    return (2.0 * near * far) / (far + near - ndc_z * (far - near));
}

// Reconstruct view-space position from uv + clip-space w stored in gbuffer.
// We store clip w in gbuf_nd.a; view pos = inv_vp * ndc * w, then divide.
fn view_pos_from_ndc(ndc: vec3<f32>, w: f32) -> vec3<f32> {
    let p = params_inv_vp() * vec4<f32>(ndc * w, w);
    return p.xyz / p.w;
}

@group(0) @binding(5) var<uniform> inv_vp: mat4x4<f32>;

fn params_inv_vp() -> mat4x4<f32> {
    return inv_vp;
}

fn screen_uv_from_world(wp: vec3<f32>, screen: vec2<f32>) -> vec4<f32> {
    let clip = vec4<f32>(wp, 1.0) * params_vp(); // row-major trick avoided: use transpose
    return clip;
}

@group(0) @binding(6) var<uniform> vp: mat4x4<f32>;

fn params_vp() -> mat4x4<f32> {
    return vp;
}

@fragment
fn fs_main(in: PresentInput) -> @location(0) vec4<f32> {
    let hdr = textureSampleLevel(hdr_tex, pp_sampler, in.uv, 0.0);
    let nd_tex = textureSampleLevel(gbuf_nd, pp_sampler, in.uv, 0.0);
    let mat_tex = textureSampleLevel(gbuf_mat, pp_sampler, in.uv, 0.0);
    let metallic = mat_tex.r;
    let roughness = mat_tex.g;

    var color = hdr.rgb;
    let rt_on = params.screen.z > 0.5;

    if rt_on && metallic > 0.08 {
        // Screen-space reflection march.
        let normal = normalize(nd_tex.xyz);
        let clip_w = nd_tex.a;
        let ndc = vec3<f32>(in.uv.x * 2.0 - 1.0, 1.0 - in.uv.y * 2.0, hdr.a);
        let vpos = view_pos_from_ndc(ndc, clip_w);
        let view_dir = normalize(vpos - params.cam_pos.xyz);
        let refl_dir = reflect(view_dir, normal);

        var hit = vec3<f32>(0.0);
        var found = false;
        var ray_p = vpos + refl_dir * 0.15;
        for (var i: i32 = 0; i < 40; i = i + 1) {
            ray_p = ray_p + refl_dir * (0.12 + f32(i) * 0.02);
            let cp = vp * vec4<f32>(ray_p, 1.0);
            if cp.w <= 0.0 {
                break;
            }
            let suv = vec2<f32>(cp.x / cp.w * 0.5 + 0.5, 0.5 - cp.y / cp.w * 0.5);
            if suv.x < 0.0 || suv.x > 1.0 || suv.y < 0.0 || suv.y > 1.0 {
                break;
            }
            let scene_w = textureSampleLevel(gbuf_nd, pp_sampler, suv, 0.0).a;
            let z = cp.w;
            if scene_w > 0.0 && z > scene_w && z - scene_w < 0.6 {
                hit = textureSampleLevel(hdr_tex, pp_sampler, suv, 0.0).rgb;
                found = true;
                break;
            }
        }

        let env_refl = env_color(reflect(view_dir, normal) * -1.0,
            Env(params.sky_top, params.sky_horizon, params.ground, vec4<f32>(0.0)));
        var refl = env_refl;
        if found {
            refl = hit;
        }
        let k = clamp(metallic * (1.0 - roughness * 0.75), 0.0, 1.0);
        color = mix(color, refl, k * 0.75);
    }

    if rt_on {
        // Cheap screen-space AO: compare depth with a ring of neighbors.
        let texel = 2.0 / params.screen.xy;
        var occ = 0.0;
        for (var i: i32 = 0; i < 6; i = i + 1) {
            let a = f32(i) * 1.047;
            let o = vec2<f32>(cos(a), sin(a)) * texel * (1.0 + f32(i / 2));
            let nd = textureSampleLevel(gbuf_nd, pp_sampler, clamp(in.uv + o, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0).a;
            let center = nd_tex.a;
            if center > 0.0 && nd > center * 1.02 {
                occ = occ + 1.0;
            }
        }
        let ao = 1.0 - occ / 6.0 * 0.35;
        color = color * ao;
    }

    let ldr = aces_tonemap(color);
    // sRGB-ish gamma.
    let srgb = pow(ldr, vec3<f32>(1.0 / 2.2));
    return vec4<f32>(srgb, 1.0);
}

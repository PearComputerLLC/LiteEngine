//! Hybrid software path tracer (legacy "raytracing mode").
//!
//! Design goals: AAA-flavored lighting (sun shadows, GI bounces, glossy
//! reflections, emissives, gradient-sky environment) on a 2010-class CPU.
//! Techniques, in order of importance:
//!
//! 1. **Reduced internal resolution** (`quality` tiers: 20% / 30% / 45% of
//!    the window) — the single biggest win.
//! 2. **Hybrid composite** — the raster base stays crisp; the path-traced
//!    buffer is composited through a feathered coverage mask, so expensive
//!    rays are only spent where RT geometry exists.
//! 3. **Temporal accumulation** with an adaptive blend (noise melts away for
//!    static shots; ghosts are bounded in dynamic ones).
//! 4. **Edge-aware 3x3 bilateral denoise** on the accumulated buffer.
//! 5. **Sharpened upscale + ACES tonemap** back to window resolution.
//! 6. Row-striped multithreading over `available_parallelism` cores.
//!
//! Scene proxies: RT spheres/planes (from `rt.add_sphere`/`add_plane`) plus
//! every visible 3D sprite billboard as a textured, screen-aligned quad with
//! alpha cutout. 2D sprites stay in the crisp raster layer above.

use crate::fb::pack_rgb;
use liteengine_core::scene::{Camera, Vec3};
use liteengine_core::snapshot::RtParams;
use liteengine_core::snapshot::{BillboardDraw, TextureData};
use std::sync::Arc;

struct BillQuad {
    pos: Vec3,
    right: Vec3,
    up: Vec3,
    half_w: f32,
    half_h: f32,
    tex: Option<Arc<TextureData>>,
    tint: [f32; 3],
}

pub struct PathTracer {
    /// Accumulated color at internal resolution.
    acc: Vec<[f32; 3]>,
    /// 255 where any RT geometry was hit (feathered at composite time).
    coverage: Vec<u8>,
    w: usize,
    h: usize,
    frame_idx: u64,
    last_camera: Option<[u8; 32]>,
    denoise_enabled: bool,
}

impl PathTracer {
    pub fn new() -> PathTracer {
        PathTracer {
            acc: Vec::new(),
            coverage: Vec::new(),
            w: 0,
            h: 0,
            frame_idx: 0,
            last_camera: None,
            denoise_enabled: true,
        }
    }

    pub fn set_denoise(&mut self, on: bool) {
        self.denoise_enabled = on;
    }

    fn ensure(&mut self, w: usize, h: usize, camera: &Camera) {
        if self.w != w || self.h != h {
            self.acc = vec![[0.0; 3]; w * h];
            self.coverage = vec![0; w * h];
            self.w = w;
            self.h = h;
            self.frame_idx = 0;
        }
        // Reset accumulation when the camera moves (cheap temporal guard).
        let cam_key = camera_hash(camera);
        if self.last_camera != Some(cam_key) {
            self.frame_idx = 0;
            self.last_camera = Some(cam_key);
        }
    }

    /// Trace one frame of the RT scene into the accumulation buffer.
    pub fn render(
        &mut self,
        params: &RtParams,
        billboards: &[BillboardDraw],
        win_w: usize,
        win_h: usize,
        threads: usize,
    ) {
        let scale = params.scale.clamp(0.05, 1.0);
        let w = ((win_w as f32 * scale) as usize).max(32);
        let h = ((win_h as f32 * scale) as usize).max(24);
        self.ensure(w, h, &params.camera);

        // Camera basis (screen-aligned billboards share it).
        let cam = params.camera;
        let fwd = cam.forward();
        let up_world = Vec3::new(0.0, 1.0, 0.0);
        let right = fwd.cross(up_world).normalize();
        let up = right.cross(fwd);

        let quads: Vec<BillQuad> = billboards
            .iter()
            .map(|b| BillQuad {
                pos: b.pos,
                right,
                up,
                half_w: b.width / 2.0,
                half_h: b.height / 2.0,
                tex: b.texture.clone(),
                tint: b.tint,
            })
            .collect();

        let scene = RtSceneRef {
            params,
            quads: &quads,
        };

        let samples = params.quality.samples();
        let bounces = params.bounces;
        let frame = self.frame_idx;

        // Pre-split into disjoint per-thread chunks so the borrow checker
        // sees non-overlapping &mut regions.
        let chunk = h.div_ceil(threads).max(1);
        let acc_chunks: Vec<&mut [[f32; 3]]> =
            self.acc.chunks_mut(chunk * w).collect();
        let cov_chunks: Vec<&mut [u8]> = self.coverage.chunks_mut(chunk * w).collect();

        std::thread::scope(|scope| {
            for (t, (acc_slice, cov_slice)) in
                acc_chunks.into_iter().zip(cov_chunks).enumerate()
            {
                let y0 = t * chunk;
                let y1 = (y0 + acc_slice.len() / w).min(h);
                let w_loc = w;
                let scene = &scene;
                scope.spawn(move || {
                    for ly in 0..(y1 - y0) {
                        let y = y0 + ly;
                        for x in 0..w_loc {
                            let mut sum = [0.0f32; 3];
                            let mut hit_any = false;
                            for s in 0..samples {
                                let (u, v) = jitter(x, y, w, h, s, frame);
                                let ray = camera_ray(&cam, &right, &up, u, v);
                                let (col, hit) =
                                    trace(&ray, scene, bounces, x as u32, y as u32, s, frame);
                                sum[0] += col[0];
                                sum[1] += col[1];
                                sum[2] += col[2];
                                hit_any |= hit;
                            }
                            let inv = 1.0 / samples as f32;
                            let mut s = [sum[0] * inv, sum[1] * inv, sum[2] * inv];
                            for c in &mut s {
                                *c = c.clamp(0.0, 4.0);
                            }
                            // Adaptive temporal blend: converges for static
                            // shots, stays responsive when things move.
                            let idx = ly * w_loc + x;
                            let n = frame + 1;
                            let alpha = (1.0 / n as f32).max(scene.params.temporal_floor);
                            let prev = acc_slice[idx];
                            acc_slice[idx] = [
                                prev[0] + (s[0] - prev[0]) * alpha,
                                prev[1] + (s[1] - prev[1]) * alpha,
                                prev[2] + (s[2] - prev[2]) * alpha,
                            ];
                            cov_slice[idx] = if hit_any { 255 } else { 0 };
                        }
                    }
                });
            }
        });

        self.frame_idx += 1;
    }

    /// Composite the (denoised, upscaled) RT buffer over the raster base.
    pub fn composite(&self, fb: &mut crate::fb::FrameBuf) {
        if self.w == 0 || self.h == 0 {
            return;
        }
        let mut denoised;
        let src: &[[f32; 3]] = if self.denoise_enabled {
            denoised = self.acc.clone();
            denoise_bilateral(&self.acc, &mut denoised, self.w, self.h);
            &denoised
        } else {
            denoised = Vec::new();
            &self.acc
        };
        // Feather the coverage mask (1px box blur) for soft edges.
        let mut soft_cov = vec![0u8; self.coverage.len()];
        blur_coverage(&self.coverage, &mut soft_cov, self.w, self.h);

        for y in 0..fb.h {
            let ty = (((y as f32 + 0.5) / fb.h as f32) * self.h as f32)
                .floor()
                .min((self.h - 1) as f32) as usize;
            for x in 0..fb.w {
                let tx = (((x as f32 + 0.5) / fb.w as f32) * self.w as f32)
                    .floor()
                    .min((self.w - 1) as f32) as usize;
                let idx = ty * self.w + tx;
                let cov = soft_cov[idx] as f32 / 255.0;
                if cov <= 0.01 {
                    continue; // keep crisp raster base
                }
                let c = src[idx];
                // Sharpen + ACES tonemap the RT contribution.
                let col = [
                    aces((c[0] * 1.08 - 0.02).clamp(0.0, 1.0)),
                    aces((c[1] * 1.08 - 0.02).clamp(0.0, 1.0)),
                    aces((c[2] * 1.08 - 0.02).clamp(0.0, 1.0)),
                ];
                let rt = pack_rgb(col);
                let dst = fb.px[y * fb.w + x];
                let sa = (cov * 255.0) as u32;
                let s = (sa << 24) | (rt & 0x00ffffff);
                fb.px[y * fb.w + x] = crate::fb::blend(dst, s);
            }
        }
    }
}

struct RtSceneRef<'a> {
    params: &'a RtParams,
    quads: &'a [BillQuad],
}

#[derive(Clone, Copy)]
struct Ray {
    origin: Vec3,
    dir: Vec3,
}

#[inline]
fn camera_ray(cam: &Camera, right: &Vec3, up: &Vec3, u: f32, v: f32) -> Ray {
    let focal = 1.0 / (cam.fov_y_deg.to_radians() * 0.5).tan();
    let ndc_x = u * 2.0 - 1.0;
    let ndc_y = 1.0 - v * 2.0;
    let dir = cam
        .forward()
        .scale(focal)
        .add(right.scale(ndc_x))
        .add(up.scale(ndc_y))
        .normalize();
    Ray {
        origin: cam.pos,
        dir,
    }
}

struct Hit {
    point: Vec3,
    normal: Vec3,
    albedo: [f32; 3],
    metallic: f32,
    roughness: f32,
    emissive: f32,
}

fn intersect(scene: &RtSceneRef, ray: &Ray) -> Option<Hit> {
    let mut best: Option<Hit> = None;
    let mut best_t = f32::INFINITY;

    // Spheres.
    for s in &scene.params.scene.spheres {
        if let Some(t) = hit_sphere(ray, s.center, s.radius) {
            if t < best_t {
                best_t = t;
                let p = ray.origin.add(ray.dir.scale(t));
                best = Some(Hit {
                    point: p,
                    normal: p.sub(s.center).scale(1.0 / s.radius),
                    albedo: s.color,
                    metallic: s.metallic,
                    roughness: s.roughness,
                    emissive: s.emissive,
                });
            }
        }
    }

    // Planes.
    for p in &scene.params.scene.planes {
        let denom = p.normal.dot(ray.dir);
        if denom.abs() < 1e-5 {
            continue;
        }
        let t = p.point.sub(ray.origin).dot(p.normal) / denom;
        if t > 1e-4 && t < best_t {
            best_t = t;
            let point = ray.origin.add(ray.dir.scale(t));
            let mut color = p.color;
            if p.checkered {
                let cx = point.x.floor() as i32;
                let cz = point.z.floor() as i32;
                if (cx + cz) % 2 == 0 {
                    color = [color[0] * 0.55, color[1] * 0.55, color[2] * 0.55];
                }
            }
            best = Some(Hit {
                point,
                normal: if p.normal.dot(ray.dir) < 0.0 {
                    p.normal
                } else {
                    p.normal.scale(-1.0)
                },
                albedo: color,
                metallic: p.metallic,
                roughness: p.roughness,
                emissive: 0.0,
            });
        }
    }

    // Billboard quads (screen-aligned, textured, alpha cutout).
    for q in scene.quads {
        let n = q.right.cross(q.up).normalize();
        let dn = n.dot(ray.dir);
        if dn.abs() < 1e-5 {
            continue;
        }
        let t = q.pos.sub(ray.origin).dot(n) / dn;
        if t <= 1e-4 || t >= best_t {
            continue;
        }
        let p = ray.origin.add(ray.dir.scale(t));
        let rel = p.sub(q.pos);
        let lu = rel.dot(q.right);
        let lv = rel.dot(q.up);
        if lu.abs() > q.half_w || lv.abs() > q.half_h {
            continue;
        }
        let albedo = match &q.tex {
            Some(tex) => {
                let u = (lu / q.half_w * 0.5 + 0.5).clamp(0.0, 0.9999);
                let v = (0.5 - lv / q.half_h).clamp(0.0, 0.9999);
                let c = sample_tex(tex, u, v);
                if c[3] < 0.5 {
                    continue; // transparent texel: ray passes through
                }
                [
                    c[0] * q.tint[0],
                    c[1] * q.tint[1],
                    c[2] * q.tint[2],
                ]
            }
            None => [0.85, 0.3, 0.3],
        };
        best_t = t;
        best = Some(Hit {
            point: p,
            normal: if dn < 0.0 { n } else { n.scale(-1.0) },
            albedo,
            metallic: 0.0,
            roughness: 0.8,
            emissive: 0.0,
        });
    }

    best
}

#[inline]
fn hit_sphere(ray: &Ray, center: Vec3, radius: f32) -> Option<f32> {
    let oc = ray.origin.sub(center);
    let b = oc.dot(ray.dir);
    let c = oc.dot(oc) - radius * radius;
    let disc = b * b - c;
    if disc < 0.0 {
        return None;
    }
    let sq = disc.sqrt();
    let t0 = -b - sq;
    let t1 = -b + sq;
    if t0 > 1e-4 {
        Some(t0)
    } else if t1 > 1e-4 {
        Some(t1)
    } else {
        None
    }
}

fn trace(
    ray: &Ray,
    scene: &RtSceneRef,
    bounces: u32,
    px: u32,
    py: u32,
    sample: u32,
    frame: u64,
) -> ([f32; 3], bool) {
    let mut color = [0.0f32; 3];
    let mut throughput = [1.0f32; 3];
    let mut ray = *ray;
    let mut hit_any = false;

    for bounce in 0..=bounces {
        let hit = match intersect(scene, &ray) {
            Some(h) => h,
            None => {
                let env = env_color(scene.params, ray.dir);
                color[0] += throughput[0] * env[0];
                color[1] += throughput[1] * env[1];
                color[2] += throughput[2] * env[2];
                break;
            }
        };
        hit_any = true;

        // Emissive.
        if hit.emissive > 0.0 {
            color[0] += throughput[0] * hit.albedo[0] * hit.emissive;
            color[1] += throughput[1] * hit.albedo[1] * hit.emissive;
            color[2] += throughput[2] * hit.albedo[2] * hit.emissive;
        }

        // Direct sun with shadow ray.
        let sun = scene.params.sun;
        let to_sun = sun.dir.scale(-1.0);
        let ndotl = hit.normal.dot(to_sun).max(0.0);
        if ndotl > 0.0 {
            let shadow_ray = Ray {
                origin: hit.point.add(hit.normal.scale(1e-3)),
                dir: to_sun,
            };
            if !shadowed(&shadow_ray, scene) {
                let k = sun.intensity * ndotl;
                let diff = 1.0 - hit.metallic * 0.8;
                color[0] += throughput[0] * hit.albedo[0] * sun.color[0] * k * diff;
                color[1] += throughput[1] * hit.albedo[1] * sun.color[1] * k * diff;
                color[2] += throughput[2] * hit.albedo[2] * sun.color[2] * k * diff;
            }
        }

        // Ambient (hemispheric env proxy) — cheap IBL.
        let amb = 0.5 + 0.5 * hit.normal.y;
        let sky = mix3(scene.params.env.sky_horizon, scene.params.env.sky_top, amb);
        let amb_col = if hit.normal.y >= 0.0 {
            sky
        } else {
            scene.params.env.ground
        };
        let amb_k = 0.22 * (1.0 - hit.metallic);
        color[0] += throughput[0] * hit.albedo[0] * amb_col[0] * amb_k;
        color[1] += throughput[1] * hit.albedo[1] * amb_col[1] * amb_k;
        color[2] += throughput[2] * hit.albedo[2] * amb_col[2] * amb_k;

        // Secondary rays: glossy reflection for metals, one diffuse GI
        // bounce for the rest.
        if bounce < bounces {
            if hit.metallic > 0.05 {
                let r = perturb(
                    ray.dir.reflect(hit.normal).normalize(),
                    hit.roughness,
                    px,
                    py,
                    sample,
                    frame,
                    bounce,
                );
                ray = Ray {
                    origin: hit.point.add(hit.normal.scale(1e-3)),
                    dir: r,
                };
                let f0 = mix3([1.0; 3], hit.albedo, hit.metallic * 0.9);
                let k = 1.0 - hit.roughness * 0.6;
                throughput[0] *= f0[0] * k;
                throughput[1] *= f0[1] * k;
                throughput[2] *= f0[2] * k;
            } else {
                // Cosine-weighted hemisphere bounce (1-sample GI).
                let d = cosine_hemi(hit.normal, px, py, sample, frame, bounce);
                ray = Ray {
                    origin: hit.point.add(hit.normal.scale(1e-3)),
                    dir: d,
                };
                throughput[0] *= hit.albedo[0] * 0.6;
                throughput[1] *= hit.albedo[1] * 0.6;
                throughput[2] *= hit.albedo[2] * 0.6;
            }
            // Russian roulette after the first bounce.
            if bounce >= 1 {
                let p = throughput[0]
                    .max(throughput[1])
                    .max(throughput[2])
                    .min(0.95);
                if p < 0.05 || rand01(px, py, frame, 77 + sample) > p {
                    break;
                }
                throughput[0] /= p;
                throughput[1] /= p;
                throughput[2] /= p;
            }
        } else {
            break;
        }
    }

    (color, hit_any)
}

fn shadowed(ray: &Ray, scene: &RtSceneRef) -> bool {
    // Spheres + planes only (billboards are "cards", they don't cast).
    for s in &scene.params.scene.spheres {
        if hit_sphere(ray, s.center, s.radius).is_some() {
            return true;
        }
    }
    for p in &scene.params.scene.planes {
        let denom = p.normal.dot(ray.dir);
        if denom.abs() < 1e-5 {
            continue;
        }
        let t = p.point.sub(ray.origin).dot(p.normal) / denom;
        if t > 1e-3 {
            return true;
        }
    }
    false
}

fn env_color(params: &RtParams, dir: Vec3) -> [f32; 3] {
    if dir.y < 0.0 {
        let g = params.env.ground;
        return [g[0] * 0.9, g[1] * 0.9, g[2] * 0.9];
    }
    let t = (dir.y * 0.5 + 0.5).clamp(0.0, 1.0);
    mix3(params.env.sky_horizon, params.env.sky_top, t * t)
}

#[inline]
fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

// ------------------------------------------------------------------ rng utils

#[inline]
fn rng(x: u32, y: u32, frame: u64, salt: u32) -> u32 {
    let mut h = x
        .wrapping_mul(0x9E37_79B9)
        ^ y.wrapping_mul(0x85EB_CA6B)
        ^ (frame as u32).wrapping_mul(0xC2B2_AE35)
        ^ salt.wrapping_mul(0x27D4_EB2F);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    h
}

#[inline]
fn rand01(x: u32, y: u32, frame: u64, salt: u32) -> f32 {
    rng(x, y, frame, salt) as f32 / u32::MAX as f32
}

#[inline]
fn jitter(x: usize, y: usize, w: usize, h: usize, s: u32, frame: u64) -> (f32, f32) {
    let jx = rand01(x as u32, y as u32, frame, 11 + s);
    let jy = rand01(x as u32, y as u32, frame, 23 + s);
    (
        (x as f32 + jx) / w as f32,
        (y as f32 + jy) / h as f32,
    )
}

fn perturb(
    dir: Vec3,
    roughness: f32,
    px: u32,
    py: u32,
    s: u32,
    frame: u64,
    bounce: u32,
) -> Vec3 {
    let dir = dir.normalize();
    if roughness < 0.02 {
        return dir;
    }
    let a = rand01(px, py, frame, 31 + s + bounce * 7) * std::f32::consts::TAU;
    let r = rand01(px, py, frame, 47 + s + bounce * 7) * roughness;
    let t = dir
        .cross(Vec3::new(0.0, 1.0, 0.0))
        .normalize();
    let t = if t.length() < 1e-4 {
        Vec3::new(1.0, 0.0, 0.0)
    } else {
        t
    };
    let b = dir.cross(t);
    dir.add(t.scale(r * a.cos()))
        .add(b.scale(r * a.sin()))
        .normalize()
}

fn cosine_hemi(n: Vec3, px: u32, py: u32, s: u32, frame: u64, bounce: u32) -> Vec3 {
    let u1 = rand01(px, py, frame, 53 + s + bounce * 7).max(1e-6);
    let u2 = rand01(px, py, frame, 61 + s + bounce * 7);
    let r = u1.sqrt();
    let theta = u2 * std::f32::consts::TAU;
    let n = n.normalize();
    let t = n
        .cross(Vec3::new(0.0, 1.0, 0.0))
        .normalize();
    let t = if t.length() < 1e-4 {
        Vec3::new(1.0, 0.0, 0.0)
    } else {
        t
    };
    let b = n.cross(t);
    n.add(t.scale(r * theta.cos()))
        .add(b.scale(r * theta.sin()))
        .normalize()
}

#[inline]
fn camera_hash(cam: &Camera) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, v) in [
        cam.pos.x,
        cam.pos.y,
        cam.pos.z,
        cam.look_at.x,
        cam.look_at.y,
        cam.look_at.z,
        cam.fov_y_deg,
    ]
    .iter()
    .enumerate()
    {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_bits().to_le_bytes());
    }
    out
}

fn sample_tex(tex: &TextureData, u: f32, v: f32) -> [f32; 4] {
    let x = ((u * (tex.width as f32 - 1.0)).round() as u32).min(tex.width - 1);
    let y = ((v * (tex.height as f32 - 1.0)).round() as u32).min(tex.height - 1);
    let i = (y as usize * tex.width as usize + x as usize) * 4;
    [
        tex.rgba[i] as f32 / 255.0,
        tex.rgba[i + 1] as f32 / 255.0,
        tex.rgba[i + 2] as f32 / 255.0,
        tex.rgba[i + 3] as f32 / 255.0,
    ]
}

/// Edge-aware 3x3 bilateral denoise (weights on color distance).
fn denoise_bilateral(src: &[[f32; 3]], dst: &mut [[f32; 3]], w: usize, h: usize) {
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let c = &src[i];
            let mut sum = [0.0f32; 3];
            let mut wsum = 0.0f32;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let nx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
                    let ny = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
                    let n = &src[ny * w + nx];
                    let dist = (c[0] - n[0]).powi(2)
                        + (c[1] - n[1]).powi(2)
                        + (c[2] - n[2]).powi(2);
                    let weight = (-dist * 12.0).exp();
                    sum[0] += n[0] * weight;
                    sum[1] += n[1] * weight;
                    sum[2] += n[2] * weight;
                    wsum += weight;
                }
            }
            dst[i] = [sum[0] / wsum, sum[1] / wsum, sum[2] / wsum];
        }
    }
}

/// 1px box blur of the coverage mask (feathering for the composite).
fn blur_coverage(src: &[u8], dst: &mut [u8], w: usize, h: usize) {
    for y in 0..h {
        for x in 0..w {
            let mut sum = 0u32;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let nx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
                    let ny = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
                    sum += src[ny * w + nx] as u32;
                }
            }
            dst[y * w + x] = (sum / 9) as u8;
        }
    }
}

/// ACES filmic approximation (Narkowicz).
#[inline]
fn aces(x: f32) -> f32 {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    ((x * (a * x + b)) / (x * (c * x + d) + e)).clamp(0.0, 1.0)
}

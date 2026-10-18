//! Software triangle rasterizer for 3D model costumes (legacy path).
//!
//! A compact z-buffered, flat-shaded rasterizer — the mesh counterpart of
//! `soft3d`'s billboards. It reuses the exact [`Camera::project`] math the
//! path tracer and billboards share, so model costumes, billboards and RT
//! geometry all agree on screen positions.
//!
//! Shading: Lambert diffuse against the scene sun plus a hemispheric
//! ambient term from the environment colors, tinted by the sprite color.

use crate::fb::{blend, pack_rgb, FrameBuf};
use liteengine_core::scene::{Camera, EnvColors, SunLight, Vec3};
use liteengine_core::snapshot::{ModelDraw, ModelData};

/// Per-frame depth buffer (reused across frames by the renderer).
pub struct DepthBuf {
    pub w: usize,
    pub h: usize,
    pub z: Vec<f32>,
}

impl DepthBuf {
    pub fn new() -> DepthBuf {
        DepthBuf { w: 0, h: 0, z: Vec::new() }
    }

    pub fn resize(&mut self, w: usize, h: usize) {
        if self.w != w || self.h != h {
            self.w = w;
            self.h = h;
            self.z = vec![f32::INFINITY; w * h];
        } else {
            self.z.iter_mut().for_each(|v| *v = f32::INFINITY);
        }
    }
}

impl Default for DepthBuf {
    fn default() -> Self {
        Self::new()
    }
}

/// Lighting inputs resolved from the frame by the renderer.
#[derive(Clone, Copy)]
pub struct MeshLight {
    pub sun: SunLight,
    pub env: EnvColors,
}

impl MeshLight {
    pub fn resolve(sun: Option<SunLight>, env: Option<EnvColors>) -> MeshLight {
        MeshLight {
            sun: sun.unwrap_or_default(),
            env: env.unwrap_or_default(),
        }
    }
}

struct Xform {
    m: [[f32; 3]; 3], // rotation (column-major-ish rows as [row][col])
}

impl Xform {
    /// XYZ euler in degrees → rotation matrix R = Rz·Ry·Rx.
    fn from_euler_deg(rx: f32, ry: f32, rz: f32) -> Xform {
        let (ax, ay, az) = (
            rx.to_radians(),
            ry.to_radians(),
            rz.to_radians(),
        );
        let (cx, sx) = (ax.cos(), ax.sin());
        let (cy, sy) = (ay.cos(), ay.sin());
        let (cz, sz) = (az.cos(), az.sin());
        // Rz * Ry * Rx
        let m = [
            [
                cz * cy,
                cz * sy * sx - sz * cx,
                cz * sy * cx + sz * sx,
            ],
            [
                sz * cy,
                sz * sy * sx + cz * cx,
                sz * sy * cx - cz * sx,
            ],
            [
                -sy,
                cy * sx,
                cy * cx,
            ],
        ];
        Xform { m }
    }

    fn rot(&self, v: Vec3) -> Vec3 {
        Vec3::new(
            self.m[0][0] * v.x + self.m[0][1] * v.y + self.m[0][2] * v.z,
            self.m[1][0] * v.x + self.m[1][1] * v.y + self.m[1][2] * v.z,
            self.m[2][0] * v.x + self.m[2][1] * v.y + self.m[2][2] * v.z,
        )
    }
}

/// Project every vertex once, then rasterize with barycentric coverage.
pub fn draw_models(
    fb: &mut FrameBuf,
    depth: &mut DepthBuf,
    models: &[ModelDraw],
    cam: &Camera,
    light: &MeshLight,
) {
    if models.is_empty() {
        return;
    }
    depth.resize(fb.w, fb.h);

    let (w, h) = (fb.w as f32, fb.h as f32);
    let f = cam.forward();
    let world_up = Vec3::new(0.0, 1.0, 0.0);
    let right = f.cross(world_up).normalize();
    let up = right.cross(f);
    let focal = (h * 0.5) / (cam.fov_y_deg.to_radians() * 0.5).tan();
    let sun_dir = light.sun.dir.normalize();
    let sun_col = light.sun.color;
    let sun_k = light.sun.intensity;
    // Hemispheric ambient: sky above, ground below (matches the wgpu look).
    let amb = light.env.sky_horizon;
    let amb_k = 0.45;

    for md in models {
        let xf = Xform::from_euler_deg(md.rot.x, md.rot.y, md.rot.z);
        let s = md.scale.max(0.0001);
        let data: &ModelData = &md.model;

        // Transform vertices to view space once.
        let nv = data.vertex_count();
        if nv == 0 || data.indices.is_empty() {
            continue;
        }
        let mut sx = vec![0.0f32; nv];
        let mut sy = vec![0.0f32; nv];
        let mut sz = vec![0.0f32; nv]; // view depth along forward
        let mut infront = vec![false; nv];
        let stride = ModelData::STRIDE;
        for v in 0..nv {
            let o = v * stride;
            let local = Vec3::new(data.vertices[o], data.vertices[o + 1], data.vertices[o + 2]);
            let world = xf.rot(local).scale(s).add(md.pos);
            let rel = world.sub(cam.pos);
            let z = rel.dot(f);
            if z > 0.05 {
                infront[v] = true;
                sx[v] = w * 0.5 + rel.dot(right) * focal / z;
                sy[v] = h * 0.5 - rel.dot(up) * focal / z;
                sz[v] = z;
            }
        }

        for tri in data.indices.chunks_exact(3) {
            let (a, b, c) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
            if a >= nv || b >= nv || c >= nv {
                continue;
            }
            if !infront[a] || !infront[b] || !infront[c] {
                continue; // cheap near-plane clip: drop touching triangles
            }
            // Face normal in world space (from rotated vertex normals would
            // smooth; flat face normals read better at these model sizes).
            let oa = a * stride;
            let ob = b * stride;
            let oc = c * stride;
            let pa = Vec3::new(data.vertices[oa], data.vertices[oa + 1], data.vertices[oa + 2]);
            let pb = Vec3::new(data.vertices[ob], data.vertices[ob + 1], data.vertices[ob + 2]);
            let pc = Vec3::new(data.vertices[oc], data.vertices[oc + 1], data.vertices[oc + 2]);
            let e1 = pb.sub(pa);
            let e2 = pc.sub(pa);
            let n = xf.rot(e1.cross(e2)).normalize();
            let center = pa.add(pb).add(pc).scale(1.0 / 3.0).add(md.pos);
            let to_cam = cam.pos.sub(center);
            let facing = n.dot(to_cam);
            if !md.two_sided && facing <= 0.0 {
                continue; // back-face cull
            }

            // Lambert + hemispheric ambient, sprite-tinted.
            let mut lit = [0.0f32; 3];
            let ndl = sun_dir.scale(-1.0).dot(if facing >= 0.0 { n } else { n.scale(-1.0) }).max(0.0);
            for ch in 0..3 {
                lit[ch] = md.color[ch] * (amb_k * amb[ch] + sun_k * sun_col[ch] * ndl);
            }
            let argb = pack_rgb(lit);
            let alpha = ((md.alpha.clamp(0.0, 1.0) * 255.0) as u32) << 24
                | (argb & 0x00ffffff);

            // Screen-space bounding box.
            let min_x = sx[a].min(sx[b]).min(sx[c]).floor().max(0.0) as i32;
            let max_x = sx[a].max(sx[b]).max(sx[c]).ceil().min(w - 1.0) as i32;
            let min_y = sy[a].min(sy[b]).min(sy[c]).floor().max(0.0) as i32;
            let max_y = sy[a].max(sy[b]).max(sy[c]).ceil().min(h - 1.0) as i32;
            if min_x > max_x || min_y > max_y {
                continue;
            }

            // Edge functions.
            let (ax, ay) = (sx[a], sy[a]);
            let (bx, by) = (sx[b], sy[b]);
            let (cx, cy) = (sx[c], sy[c]);
            let area = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
            if area.abs() < 1e-6 {
                continue;
            }
            let inv_area = 1.0 / area;
            let flip = area < 0.0;
            for py in min_y..=max_y {
                let row = py as usize * depth.w;
                for px in min_x..=max_x {
                    let pxf = px as f32 + 0.5;
                    let pyf = py as f32 + 0.5;
                    let wa = ((bx - pxf) * (cy - pyf) - (by - pyf) * (cx - pxf)) * inv_area;
                    let wb = ((cx - pxf) * (ay - pyf) - (cy - pyf) * (ax - pxf)) * inv_area;
                    let wc = ((ax - pxf) * (by - pyf) - (ay - pyf) * (bx - pxf)) * inv_area;
                    // Interior when all three share the winding's sign.
                    let (wa, wb, wc) = if flip {
                        (-wa, -wb, -wc)
                    } else {
                        (wa, wb, wc)
                    };
                    if wa < 0.0 || wb < 0.0 || wc < 0.0 {
                        continue;
                    }
                    let z = wa * sz[a] + wb * sz[b] + wc * sz[c];
                    let di = row + px as usize;
                    if z <= depth.z[di] {
                        depth.z[di] = z;
                        let dst = fb.px[di];
                        fb.px[di] = blend(dst, alpha);
                    }
                }
            }
        }
    }
}

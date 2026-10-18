//! The modern (wgpu) rendering path.
//!
//! Selected automatically on Apple Silicon (macOS 11+), Windows 8/10/11 and
//! 64-bit x86_64/ARM64 Linux. Implements the full PBR look on modern GPUs:
//!
//! * metallic-roughness Cook-Torrance GGX materials,
//! * directional sun with a 2048² shadow map (9-tap PCF, softer in RT mode),
//! * hemispheric IBL-style ambience (sky/horizon/ground),
//! * **raytracing mode** = screen-space RT effects: SSR ray marching against
//!   the G-buffer plus cheap screen-space AO, layered over the PBR base,
//! * camera-facing 3D billboards (both sides textured) and 2D sprites drawn
//!   through a fast textured-quad pipeline,
//! * ACES tonemapping.
//!
//! Presentation deliberately reuses the same `Host` trait as the legacy path:
//! the final frame is copied back to CPU and handed to the OS window, keeping
//! ONE windowing/input layer for both paths (tiny dependency surface,
//! identical behavior on old and new OSes) at the cost of a readback — the
//! zero-copy surface-present path is planned for 1.1.
//!
//! WGSL sources live in `shaders/` and are concatenated at load time
//! (`common.wgsl` + pipeline code) because WGSL has no import statement.

use liteengine_core::host::{Host, Renderer};
use liteengine_core::scene::{Camera, EnvColors, SunLight, Vec3};
use liteengine_core::snapshot::Frame;
use std::collections::HashMap;

const COMMON_WGSL: &str = include_str!("shaders/common.wgsl");
const MESH_WGSL: &str = include_str!("shaders/mesh.wgsl");
const SHADOW_WGSL: &str = include_str!("shaders/shadow.wgsl");
const QUAD_WGSL: &str = include_str!("shaders/quad.wgsl");
const PRESENT_WGSL: &str = include_str!("shaders/present.wgsl");

/// Uniform offset map inside the shared "statics" buffer.
mod offsets {
    pub const CAMERA: u64 = 0; // 144 bytes
    pub const SUN: u64 = 160; // 32
    pub const ENV: u64 = 192; // 64
    pub const LIGHT: u64 = 256; // 64
    pub const STATICS_SIZE: u64 = 384;

    pub const PRESENT_PARAMS: u64 = 0; // 96
    pub const INV_VP: u64 = 96; // 64
    pub const VP: u64 = 160; // 64
    pub const PRESENT_SIZE: u64 = 256;
}

const UNIFORM_ALIGN: u64 = 256; // dynamic-offset alignment

// ------------------------------------------------------------------ math

/// Column-major 4x4 matrix (matches WGSL mat4x4 memory layout).
#[derive(Clone, Copy, Debug)]
pub struct Mat4 {
    pub c: [[f32; 4]; 4],
}

impl Mat4 {
    pub const IDENTITY: Mat4 = Mat4 {
        c: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    };

    pub fn mul(&self, b: &Mat4) -> Mat4 {
        let a = &self.c;
        let bb = &b.c;
        let mut m = [[0.0f32; 4]; 4];
        for col in 0..4 {
            for row in 0..4 {
                m[col][row] = a[0][row] * bb[col][0]
                    + a[1][row] * bb[col][1]
                    + a[2][row] * bb[col][2]
                    + a[3][row] * bb[col][3];
            }
        }
        Mat4 { c: m }
    }

    pub fn look_at_rh(eye: Vec3, center: Vec3, up: Vec3) -> Mat4 {
        let f = (center.sub(eye)).normalize();
        let s = f.cross(up).normalize();
        let u = s.cross(f);
        Mat4 {
            c: [
                [s.x, u.x, -f.x, 0.0],
                [s.y, u.y, -f.y, 0.0],
                [s.z, u.z, -f.z, 0.0],
                [-s.dot(eye), -u.dot(eye), f.dot(eye), 1.0],
            ],
        }
    }

    pub fn perspective_rh_zo(fov_y_deg: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
        let f = 1.0 / (fov_y_deg.to_radians() * 0.5).tan();
        Mat4 {
            c: [
                [f / aspect, 0.0, 0.0, 0.0],
                [0.0, f, 0.0, 0.0],
                [0.0, 0.0, far / (near - far), -1.0],
                [0.0, 0.0, (far * near) / (near - far), 0.0],
            ],
        }
    }

    pub fn ortho_rh_zo(l: f32, r: f32, b: f32, t: f32, n: f32, f: f32) -> Mat4 {
        Mat4 {
            c: [
                [2.0 / (r - l), 0.0, 0.0, 0.0],
                [0.0, 2.0 / (t - b), 0.0, 0.0],
                [0.0, 0.0, -1.0 / (f - n), 0.0],
                [-(r + l) / (r - l), -(t + b) / (t - b), -n / (f - n), 1.0],
            ],
        }
    }

    /// Translation * rotation(ZYX euler) * scale, column-major.
    pub fn trs(pos: Vec3, rot_deg: Vec3, scale: Vec3) -> Mat4 {
        let (rx, ry, rz) = (
            rot_deg.x.to_radians(),
            rot_deg.y.to_radians(),
            rot_deg.z.to_radians(),
        );
        let (cx, sx) = rx.sin_cos();
        let (cy, sy) = ry.sin_cos();
        let (cz, sz) = rz.sin_cos();
        let r00 = cz * cy;
        let r01 = cz * sy * sx - sz * cx;
        let r02 = cz * sy * cx + sz * sx;
        let r10 = sz * cy;
        let r11 = sz * sy * sx + cz * cx;
        let r12 = sz * sy * cx - cz * sx;
        let r20 = -sy;
        let r21 = cy * sx;
        let r22 = cy * cx;
        Mat4 {
            c: [
                [r00 * scale.x, r10 * scale.x, r20 * scale.x, 0.0],
                [r01 * scale.y, r11 * scale.y, r21 * scale.y, 0.0],
                [r02 * scale.z, r12 * scale.z, r22 * scale.z, 0.0],
                [pos.x, pos.y, pos.z, 1.0],
            ],
        }
    }

    pub fn to_bytes(&self) -> [u8; 64] {
        let mut out = [0u8; 64];
        for (i, col) in self.c.iter().enumerate() {
            for (j, v) in col.iter().enumerate() {
                out[(i * 4 + j) * 4..(i * 4 + j) * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
        out
    }
}

impl Mat4 {
    pub fn inverse(&self) -> Mat4 {
        // Standard adjugate method (gluInvertMatrix), row-major internally.
        let m = |r: usize, c: usize| self.c[c][r];
        let mut inv = [0.0f32; 16];
        inv[0] = m(1,1)*m(2,2)*m(3,3) - m(1,1)*m(2,3)*m(3,2) - m(2,1)*m(1,2)*m(3,3) + m(2,1)*m(1,3)*m(3,2) + m(3,1)*m(1,2)*m(2,3) - m(3,1)*m(1,3)*m(2,2);
        inv[4] = -m(1,0)*m(2,2)*m(3,3) + m(1,0)*m(2,3)*m(3,2) + m(2,0)*m(1,2)*m(3,3) - m(2,0)*m(1,3)*m(3,2) - m(3,0)*m(1,2)*m(2,3) + m(3,0)*m(1,3)*m(2,2);
        inv[8] = m(1,0)*m(2,1)*m(3,3) - m(1,0)*m(2,3)*m(3,1) - m(2,0)*m(1,1)*m(3,3) + m(2,0)*m(1,3)*m(3,1) + m(3,0)*m(1,1)*m(2,3) - m(3,0)*m(1,3)*m(2,1);
        inv[12] = -m(1,0)*m(2,1)*m(3,2) + m(1,0)*m(2,2)*m(3,1) + m(2,0)*m(1,1)*m(3,2) - m(2,0)*m(1,2)*m(3,1) - m(3,0)*m(1,1)*m(2,2) + m(3,0)*m(1,2)*m(2,1);
        inv[1] = -m(0,1)*m(2,2)*m(3,3) + m(0,1)*m(2,3)*m(3,2) + m(2,1)*m(0,2)*m(3,3) - m(2,1)*m(0,3)*m(3,2) - m(3,1)*m(0,2)*m(2,3) + m(3,1)*m(0,3)*m(2,2);
        inv[5] = m(0,0)*m(2,2)*m(3,3) - m(0,0)*m(2,3)*m(3,2) - m(2,0)*m(0,2)*m(3,3) + m(2,0)*m(0,3)*m(3,2) + m(3,0)*m(0,2)*m(2,3) - m(3,0)*m(0,3)*m(2,2);
        inv[9] = -m(0,0)*m(2,1)*m(3,3) + m(0,0)*m(2,3)*m(3,1) + m(2,0)*m(0,1)*m(3,3) - m(2,0)*m(0,3)*m(3,1) - m(3,0)*m(0,1)*m(2,3) + m(3,0)*m(0,3)*m(2,1);
        inv[13] = m(0,0)*m(2,1)*m(3,2) - m(0,0)*m(2,2)*m(3,1) - m(2,0)*m(0,1)*m(3,2) + m(2,0)*m(0,2)*m(3,1) + m(3,0)*m(0,1)*m(2,2) - m(3,0)*m(0,2)*m(2,1);
        inv[2] = m(0,1)*m(1,2)*m(3,3) - m(0,1)*m(1,3)*m(3,2) - m(1,1)*m(0,2)*m(3,3) + m(1,1)*m(0,3)*m(3,2) + m(3,1)*m(0,2)*m(1,3) - m(3,1)*m(0,3)*m(1,2);
        inv[6] = -m(0,0)*m(1,2)*m(3,3) + m(0,0)*m(1,3)*m(3,2) + m(1,0)*m(0,2)*m(3,3) - m(1,0)*m(0,3)*m(3,2) - m(3,0)*m(0,2)*m(1,3) + m(3,0)*m(0,3)*m(1,2);
        inv[10] = m(0,0)*m(1,1)*m(3,3) - m(0,0)*m(1,3)*m(3,1) - m(1,0)*m(0,1)*m(3,3) + m(1,0)*m(0,3)*m(3,1) + m(3,0)*m(0,1)*m(1,3) - m(3,0)*m(0,3)*m(1,1);
        inv[14] = -m(0,0)*m(1,1)*m(3,2) + m(0,0)*m(1,2)*m(3,1) + m(1,0)*m(0,1)*m(3,2) - m(1,0)*m(0,2)*m(3,1) - m(3,0)*m(0,1)*m(1,2) + m(3,0)*m(0,2)*m(1,1);
        inv[3] = -m(0,1)*m(1,2)*m(2,3) + m(0,1)*m(1,3)*m(2,2) + m(1,1)*m(0,2)*m(2,3) - m(1,1)*m(0,3)*m(2,2) - m(2,1)*m(0,2)*m(1,3) + m(2,1)*m(0,3)*m(1,2);
        inv[7] = m(0,0)*m(1,2)*m(2,3) - m(0,0)*m(1,3)*m(2,2) - m(1,0)*m(0,2)*m(2,3) + m(1,0)*m(0,3)*m(2,2) + m(2,0)*m(0,2)*m(1,3) - m(2,0)*m(0,3)*m(1,2);
        inv[11] = -m(0,0)*m(1,1)*m(2,3) + m(0,0)*m(1,3)*m(2,1) + m(1,0)*m(0,1)*m(2,3) - m(1,0)*m(0,3)*m(2,1) - m(2,0)*m(0,1)*m(1,3) + m(2,0)*m(0,3)*m(1,1);
        inv[15] = m(0,0)*m(1,1)*m(2,2) - m(0,0)*m(1,2)*m(2,1) - m(1,0)*m(0,1)*m(2,2) + m(1,0)*m(0,2)*m(2,1) + m(2,0)*m(0,1)*m(1,2) - m(2,0)*m(0,2)*m(1,1);
        let det = m(0,0)*inv[0] + m(0,1)*inv[4] + m(0,2)*inv[8] + m(0,3)*inv[12];
        if det.abs() < 1e-10 {
            return Mat4::IDENTITY;
        }
        let idet = 1.0 / det;
        let mut out = Mat4::IDENTITY;
        for col in 0..4 {
            for row in 0..4 {
                out.c[col][row] = inv[row * 4 + col] * idet;
            }
        }
        out
    }
}

fn f32b(vals: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vals.len() * 4);
    for v in vals {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

// ------------------------------------------------------------------ meshes

struct MeshData {
    vertices: Vec<f32>, // pos3 normal3 uv2
    indices: Vec<u16>,
}

fn gen_mesh(kind: &str) -> MeshData {
    match kind {
        "plane" => MeshData {
            vertices: vec![
                -12.0, 0.0, -12.0, 0.0, 1.0, 0.0, 0.0, 0.0,
                12.0, 0.0, -12.0, 0.0, 1.0, 0.0, 1.0, 0.0,
                12.0, 0.0, 12.0, 0.0, 1.0, 0.0, 1.0, 1.0,
                -12.0, 0.0, 12.0, 0.0, 1.0, 0.0, 0.0, 1.0,
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
        },
        "sphere" => {
            let mut v = Vec::new();
            let mut idx = Vec::new();
            let stacks = 16u32;
            let slices = 24u32;
            for i in 0..=stacks {
                let phi = std::f32::consts::PI * i as f32 / stacks as f32;
                for j in 0..=slices {
                    let theta = 2.0 * std::f32::consts::PI * j as f32 / slices as f32;
                    let x = phi.sin() * theta.cos();
                    let y = phi.cos();
                    let z = phi.sin() * theta.sin();
                    v.extend_from_slice(&[
                        x, y, z, x, y, z,
                        j as f32 / slices as f32,
                        i as f32 / stacks as f32,
                    ]);
                }
            }
            let stride = slices + 1;
            for i in 0..stacks {
                for j in 0..slices {
                    let a = i * stride + j;
                    let b = a + stride;
                    idx.extend_from_slice(&[a as u16, b as u16, (a + 1) as u16]);
                    idx.extend_from_slice(&[b as u16, (b + 1) as u16, (a + 1) as u16]);
                }
            }
            MeshData { vertices: v, indices: idx }
        }
        _ => {
            // cube
            let s = 0.5f32;
            let mut v = Vec::new();
            let mut idx = Vec::new();
            let normals: [[f32; 3]; 6] = [
                [1.0, 0.0, 0.0],
                [-1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, -1.0, 0.0],
                [0.0, 0.0, 1.0],
                [0.0, 0.0, -1.0],
            ];
            for n in &normals {
                let t: [f32; 3] = if n[0].abs() > 0.5 {
                    [0.0, 0.0, 1.0]
                } else {
                    [1.0, 0.0, 0.0]
                };
                let b = [
                    n[1] * t[2] - n[2] * t[1],
                    n[2] * t[0] - n[0] * t[2],
                    n[0] * t[1] - n[1] * t[0],
                ];
                let base = v.len() as u16 / 8;
                let corners: [[f32; 2]; 4] =
                    [[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]];
                for (ci, c) in corners.iter().enumerate() {
                    v.extend_from_slice(&[
                        n[0] * s + t[0] * s * c[0] + b[0] * s * c[1],
                        n[1] * s + t[1] * s * c[0] + b[1] * s * c[1],
                        n[2] * s + t[2] * s * c[0] + b[2] * s * c[1],
                        n[0],
                        n[1],
                        n[2],
                        if ci == 1 || ci == 2 { 1.0 } else { 0.0 },
                        if ci >= 2 { 1.0 } else { 0.0 },
                    ]);
                }
                idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
            }
            MeshData { vertices: v, indices: idx }
        }
    }
}

// ------------------------------------------------------------------ renderer

pub struct GpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    width: u32,
    height: u32,

    hdr_view: wgpu::TextureView,
    nd_view: wgpu::TextureView,
    mat_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    screen_view: wgpu::TextureView,
    screen_tex: wgpu::Texture,
    shadow_view: wgpu::TextureView,
    readback: wgpu::Buffer,

    mesh_pipe: wgpu::RenderPipeline,
    shadow_pipe: wgpu::RenderPipeline,
    quad_pipe: wgpu::RenderPipeline,
    present_pipe: wgpu::RenderPipeline,

    statics_buf: wgpu::Buffer,
    present_buf: wgpu::Buffer,
    model_buf: wgpu::Buffer,
    shadow_model_buf: wgpu::Buffer,
    ortho_buf: wgpu::Buffer,
    quad_buf: wgpu::Buffer,
    quad_buf_cap: u64,

    statics_bind: wgpu::BindGroup,
    present_bind: wgpu::BindGroup,
    quad_ortho_bind: wgpu::BindGroup,
    shadow_light_bind: wgpu::BindGroup,

    mesh_model_bgl: wgpu::BindGroupLayout,
    quad_tex_bgl: wgpu::BindGroupLayout,
    shadow_model_bgl: wgpu::BindGroupLayout,

    linear_sampler: wgpu::Sampler,
    cmp_sampler: wgpu::Sampler,

    white_view: wgpu::TextureView,
    meshes: HashMap<String, (wgpu::Buffer, wgpu::Buffer, u32)>,
    /// GPU uploads of streamed model costumes, keyed by `Arc::as_ptr`.
    model_meshes: HashMap<usize, (wgpu::Buffer, wgpu::Buffer, u32)>,
    /// Number of UNIFORM_ALIGN slots currently available in `model_buf`.
    model_slots: u64,
    /// Cached sprite textures keyed by TextureData identity pointer.
    sprite_binds: HashMap<usize, wgpu::BindGroup>,
    atlas_bind: wgpu::BindGroup,
}

impl GpuRenderer {
    pub fn new() -> Result<GpuRenderer, String> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok_or("no wgpu adapter found")?;
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("liteengine-gpu"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
            },
            None,
        ))
        .map_err(|e| e.to_string())?;

        let mesh_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mesh"),
            source: wgpu::ShaderSource::Wgsl((COMMON_WGSL.to_owned() + MESH_WGSL).into()),
        });
        let shadow_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shadow"),
            source: wgpu::ShaderSource::Wgsl((COMMON_WGSL.to_owned() + SHADOW_WGSL).into()),
        });
        let quad_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("quad"),
            source: wgpu::ShaderSource::Wgsl((COMMON_WGSL.to_owned() + QUAD_WGSL).into()),
        });
        let present_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("present"),
            source: wgpu::ShaderSource::Wgsl((COMMON_WGSL.to_owned() + PRESENT_WGSL).into()),
        });

        // ---- bind group layouts -------------------------------------------
        let statics_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("statics"),
            entries: &[
                // camera (vs+fs)
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(offsets::CAMERA + 144),
                    },
                    count: None,
                },
                // sun (fs)
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(32),
                    },
                    count: None,
                },
                // env (fs)
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(64),
                    },
                    count: None,
                },
                // light matrix (vs)
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(64),
                    },
                    count: None,
                },
                // shadow map + cmp sampler (fs)
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ],
        });
        let mesh_model_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mesh-model"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(96),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let shadow_light_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadow-light"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(64),
                },
                count: None,
            }],
        });
        let shadow_model_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadow-model"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(64),
                },
                count: None,
            }],
        });
        let quad_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("quad-tex"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let ortho_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("quad-ortho"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(64),
                },
                count: None,
            }],
        });
        let present_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("present"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(96),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(64),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(64),
                    },
                    count: None,
                },
            ],
        });

        // NOTE: quad.wgsl expects ortho at group0/binding0 and texture at group1.
        // mesh.wgsl expects statics at group0 and model/tex/sampler at group1
        // and shadow map at group2 — adjust the WGSL group numbers instead of
        // creating extra layouts (done via source patching below).

        // ---- vertex layouts ------------------------------------------------
        let mesh_vbl = wgpu::VertexBufferLayout {
            array_stride: 32,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 0, shader_location: 0 },
                wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 12, shader_location: 1 },
                wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 24, shader_location: 2 },
            ],
        };
        let quad_vbl = wgpu::VertexBufferLayout {
            array_stride: 32,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 0, shader_location: 0 },
                wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 8, shader_location: 1 },
                wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 16, shader_location: 2 },
            ],
        };

        let mesh_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("pbr-mesh"),
            layout: Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[&statics_bgl, &mesh_model_bgl],
                push_constant_ranges: &[],
            })),
            vertex: wgpu::VertexState {
                module: &mesh_module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[mesh_vbl.clone()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &mesh_module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[
                    Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba16Float,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    }),
                    Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba16Float,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    }),
                    Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba16Float,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    }),
                ],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });

        let shadow_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow"),
            layout: Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[&shadow_light_bgl, &shadow_model_bgl],
                push_constant_ranges: &[],
            })),
            vertex: wgpu::VertexState {
                module: &shadow_module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[mesh_vbl],
            },
            fragment: None,
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Front),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: Default::default(),
                bias: wgpu::DepthBiasState {
                    constant: 2,
                    slope_scale: 2.0,
                    clamp: 0.0,
                },
            }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });

        let quad_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("quad"),
            layout: Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[&ortho_bgl, &quad_bgl],
                push_constant_ranges: &[],
            })),
            vertex: wgpu::VertexState {
                module: &quad_module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[quad_vbl],
            },
            fragment: Some(wgpu::FragmentState {
                module: &quad_module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });

        let present_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("present"),
            layout: Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[&present_bgl],
                push_constant_ranges: &[],
            })),
            vertex: wgpu::VertexState {
                module: &present_module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &present_module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });

        // ---- buffers -------------------------------------------------------
        let mk_buf = |size: u64, label: &str| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let statics_buf = mk_buf(offsets::STATICS_SIZE, "statics");
        let present_buf = mk_buf(offsets::PRESENT_SIZE, "present-params");
        let model_buf = mk_buf(UNIFORM_ALIGN * 64, "model");
        let shadow_model_buf = mk_buf(64, "shadow-model");
        let ortho_buf = mk_buf(64, "ortho");
        let quad_buf_cap = 4u64 * 1024 * 1024;
        let quad_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quad-verts"),
            size: quad_buf_cap,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (1920u32 * 4).div_ceil(256) as u64 * 256 * 1080,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let linear_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let cmp_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });

        // Bind groups referencing placeholder textures; real ones arrive on
        // the first resize() call.
        let dummy = Self::make_color_tex(&device, &queue, 1, 1, [255, 255, 255, 255]);
        let dummy_view = dummy.create_view(&Default::default());

        let statics_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("statics-bind"),
            layout: &statics_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &statics_buf,
                        offset: offsets::CAMERA,
                        size: wgpu::BufferSize::new(144),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &statics_buf,
                        offset: offsets::SUN,
                        size: wgpu::BufferSize::new(32),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &statics_buf,
                        offset: offsets::ENV,
                        size: wgpu::BufferSize::new(64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &statics_buf,
                        offset: offsets::LIGHT,
                        size: wgpu::BufferSize::new(64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&dummy_view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(&cmp_sampler),
                },
            ],
        });
        let present_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("present-bind"),
            layout: &present_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&dummy_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&dummy_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&dummy_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&linear_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &present_buf,
                        offset: offsets::PRESENT_PARAMS,
                        size: wgpu::BufferSize::new(96),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &present_buf,
                        offset: offsets::INV_VP,
                        size: wgpu::BufferSize::new(64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &present_buf,
                        offset: offsets::VP,
                        size: wgpu::BufferSize::new(64),
                    }),
                },
            ],
        });
        let quad_ortho_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ortho-bind"),
            layout: &ortho_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &ortho_buf,
                    offset: 0,
                    size: None,
                }),
            }],
        });
        let shadow_light_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shadow-light-bind"),
            layout: &shadow_light_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &statics_buf,
                    offset: offsets::LIGHT,
                    size: wgpu::BufferSize::new(64),
                }),
            }],
        });
        let atlas_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atlas-bind"),
            layout: &quad_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&dummy_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&linear_sampler),
                },
            ],
        });

        Ok(GpuRenderer {
            device,
            queue,
            width: 0,
            height: 0,
            hdr_view: dummy_view.clone(),
            nd_view: dummy_view.clone(),
            mat_view: dummy_view.clone(),
            depth_view: dummy_view.clone(),
            screen_view: dummy_view.clone(),
            screen_tex: dummy.clone(),
            shadow_view: dummy_view.clone(),
            readback,
            mesh_pipe,
            shadow_pipe,
            quad_pipe,
            present_pipe,
            statics_buf,
            present_buf,
            model_buf,
            shadow_model_buf,
            ortho_buf,
            quad_buf,
            quad_buf_cap,
            statics_bind,
            present_bind,
            quad_ortho_bind,
            shadow_light_bind,
            mesh_model_bgl,
            quad_tex_bgl: quad_bgl,
            shadow_model_bgl,
            linear_sampler,
            cmp_sampler,
            white_view: dummy_view,
            meshes: HashMap::new(),
            model_meshes: HashMap::new(),
            model_slots: 64,
            sprite_binds: HashMap::new(),
            atlas_bind,
        })
    }

    fn make_color_tex(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        w: u32,
        h: u32,
        color: [u8; 4],
    ) -> wgpu::Texture {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let data = vec![color; (w.max(1) * h.max(1)) as usize];
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            {
                let mut flat = Vec::with_capacity(data.len() * 4);
                for c in &data {
                    flat.extend_from_slice(c);
                }
                flat
            }
            .as_slice(),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(w.max(1) * 4),
                rows_per_image: Some(h.max(1)),
            },
            wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
        );
        tex
    }

    fn ensure_targets(&mut self, w: u32, h: u32) {
        if self.width == w && self.height == h {
            return;
        }
        self.width = w;
        self.height = h;
        let size = wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 };
        let mk = |format: wgpu::TextureFormat, usage, label: &str| {
            self.device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        self.hdr_view = mk(
            wgpu::TextureFormat::Rgba16Float,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            "hdr",
        );
        self.nd_view = mk(
            wgpu::TextureFormat::Rgba16Float,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            "gbuf-nd",
        );
        self.mat_view = mk(
            wgpu::TextureFormat::Rgba16Float,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            "gbuf-mat",
        );
        self.depth_view = mk(
            wgpu::TextureFormat::Depth32Float,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
            "depth",
        );
        self.shadow_view = mk(
            wgpu::TextureFormat::Depth32Float,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
            "shadow-map",
        );
        let screen_tex = self
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("screen"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
        self.screen_view = screen_tex.create_view(&Default::default());
        self.screen_tex = screen_tex;

        // Present bind group with real targets.
        self.present_bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("present-bind"),
            layout: &self.present_pipe.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.hdr_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&self.nd_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&self.mat_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.linear_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.present_buf,
                        offset: offsets::PRESENT_PARAMS,
                        size: wgpu::BufferSize::new(96),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.present_buf,
                        offset: offsets::INV_VP,
                        size: wgpu::BufferSize::new(64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.present_buf,
                        offset: offsets::VP,
                        size: wgpu::BufferSize::new(64),
                    }),
                },
            ],
        });

        // Shadow compare bind group with the real shadow map.
        self.statics_bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("statics-bind"),
            layout: &self.mesh_pipe.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.statics_buf,
                        offset: offsets::CAMERA,
                        size: wgpu::BufferSize::new(144),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.statics_buf,
                        offset: offsets::SUN,
                        size: wgpu::BufferSize::new(32),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.statics_buf,
                        offset: offsets::ENV,
                        size: wgpu::BufferSize::new(64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.statics_buf,
                        offset: offsets::LIGHT,
                        size: wgpu::BufferSize::new(64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&self.shadow_view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(&self.cmp_sampler),
                },
            ],
        });
    }
}

/// Byte-slice helper (avoids bytemuck dependency).
fn bytemuck_bytes(data: &[u8]) -> &[u8] {
    data
}

// ------------------------------------------------------------------ frame rendering

impl GpuRenderer {
    fn get_mesh(&mut self, kind: &str) -> (wgpu::Buffer, wgpu::Buffer, u32) {
        if let Some(m) = self.meshes.get(kind) {
            return (m.0.clone(), m.1.clone(), m.2);
        }
        let data = gen_mesh(kind);
        let (vbuf, ibuf, count) = self.upload_mesh_data(&data.vertices, &data.indices);
        self.meshes
            .insert(kind.to_string(), (vbuf.clone(), ibuf.clone(), count));
        (vbuf, ibuf, count)
    }

    /// Upload or fetch a cached GPU mesh for a streamed model costume.
    fn get_model_mesh(
        &mut self,
        model: &std::sync::Arc<liteengine_core::snapshot::ModelData>,
    ) -> (wgpu::Buffer, wgpu::Buffer, u32) {
        let key = std::sync::Arc::as_ptr(model) as usize;
        if let Some(m) = self.model_meshes.get(&key) {
            return (m.0.clone(), m.1.clone(), m.2);
        }
        let (vbuf, ibuf, count) =
            self.upload_mesh_data(&model.vertices, &model.indices);
        self.model_meshes.insert(key, (vbuf.clone(), ibuf.clone(), count));
        (vbuf, ibuf, count)
    }

    fn upload_mesh_data(&self, vertices: &[f32], indices: &[u16]) -> (wgpu::Buffer, wgpu::Buffer, u32) {
        let vbuf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mesh-v"),
            size: (vertices.len() * 4) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue
            .write_buffer(&vbuf, 0, f32b(vertices).as_slice());
        let ibuf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mesh-i"),
            size: (indices.len() * 2).max(4) as u64,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut ib = Vec::with_capacity(indices.len() * 2);
        for i in indices {
            ib.extend_from_slice(&i.to_le_bytes());
        }
        self.queue.write_buffer(&ibuf, 0, &ib);
        (vbuf, ibuf, indices.len() as u32)
    }

    fn upload_mesh_model(&self, mesh: &liteengine_core::scene::PbrMesh, slot: usize) -> wgpu::BindGroup {
        let model = Mat4::trs(mesh.pos, mesh.rot, mesh.scale);
        let mut data = model.to_bytes().to_vec();
        data.extend_from_slice(f32b(&[
            mesh.color[0], mesh.color[1], mesh.color[2], 1.0,
        ]).as_slice());
        data.extend_from_slice(f32b(&[
            mesh.metallic,
            mesh.roughness,
            if mesh.texture.is_some() { 1.0 } else { 0.0 },
            0.0,
        ]).as_slice());
        data.resize(96, 0);
        let offset = (slot as u64) * UNIFORM_ALIGN;
        self.queue.write_buffer(&self.model_buf, offset, &data);

        // Untextured meshes sample the white texture.
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mesh-model"),
            layout: &self.mesh_model_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.model_buf,
                        offset,
                        size: wgpu::BufferSize::new(96),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&self.white_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.linear_sampler),
                },
            ],
        })
    }

    fn write_statics(
        &self,
        camera: &Camera,
        sun: &SunLight,
        env: &EnvColors,
        aspect: f32,
    ) -> (Mat4, Mat4) {
        let view = Mat4::look_at_rh(camera.pos, camera.look_at, Vec3::new(0.0, 1.0, 0.0));
        let proj = Mat4::perspective_rh_zo(camera.fov_y_deg, aspect, 0.1, 200.0);
        let vp = proj.mul(&view);
        let inv_vp = vp.inverse();

        // Shadow camera: ortho box around the origin along the sun direction.
        let dir = sun.dir.normalize();
        let eye = Vec3::ZERO.sub(dir.scale(25.0));
        let light_view = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::new(0.0, 1.0, 0.0));
        let light_proj = Mat4::ortho_rh_zo(-14.0, 14.0, -14.0, 14.0, 1.0, 60.0);
        let light_vp = light_proj.mul(&light_view);

        let mut statics = Vec::new();
        statics.extend_from_slice(&vp.to_bytes());
        statics.extend_from_slice(&inv_vp.to_bytes());
        statics.extend_from_slice(&f32b(&[
            camera.pos.x, camera.pos.y, camera.pos.z, 1.0,
        ]));
        statics.resize(offsets::SUN as usize, 0);
        statics.extend_from_slice(&f32b(&[dir.x, dir.y, dir.z, 0.0]));
        statics.extend_from_slice(&f32b(&[
            sun.color[0], sun.color[1], sun.color[2], sun.intensity,
        ]));
        statics.resize(offsets::ENV as usize, 0);
        statics.extend_from_slice(&f32b(&[
            env.sky_top[0], env.sky_top[1], env.sky_top[2], 1.0,
        ]));
        statics.extend_from_slice(&f32b(&[
            env.sky_horizon[0], env.sky_horizon[1], env.sky_horizon[2], 1.0,
        ]));
        statics.extend_from_slice(&f32b(&[
            env.ground[0], env.ground[1], env.ground[2], 1.0,
        ]));
        statics.extend_from_slice(&f32b(&[0.9, 0.0, 0.0, 0.0])); // ambient strength
        statics.resize(offsets::LIGHT as usize, 0);
        statics.extend_from_slice(&light_vp.to_bytes());
        self.queue.write_buffer(&self.statics_buf, 0, &statics);

        // Present params.
        let mut pp = Vec::new();
        pp.extend_from_slice(&f32b(&[
            self.width as f32,
            self.height as f32,
            1.0, // rt_on — updated per frame below
            0.0,
        ]));
        pp.extend_from_slice(&f32b(&[dir.x, dir.y, dir.z, 0.0]));
        pp.extend_from_slice(&f32b(&[env.sky_top[0], env.sky_top[1], env.sky_top[2], 1.0]));
        pp.extend_from_slice(&f32b(&[env.sky_horizon[0], env.sky_horizon[1], env.sky_horizon[2], 1.0]));
        pp.extend_from_slice(&f32b(&[env.ground[0], env.ground[1], env.ground[2], 1.0]));
        pp.extend_from_slice(&f32b(&[camera.pos.x, camera.pos.y, camera.pos.z, 1.0]));
        self.queue.write_buffer(&self.present_buf, offsets::PRESENT_PARAMS, &pp);
        self.queue.write_buffer(&self.present_buf, offsets::INV_VP, &inv_vp.to_bytes());
        self.queue.write_buffer(&self.present_buf, offsets::VP, &vp.to_bytes());

        (vp, inv_vp)
    }
}

impl Renderer for GpuRenderer {
    fn name(&self) -> &'static str {
        "modern-wgpu"
    }

    fn render_frame(&mut self, frame: &Frame, host: &mut dyn Host) -> Result<(), String> {
        let (w, h) = host.size();
        self.ensure_targets(w as u32, h as u32);

        let camera = frame.pbr_camera.unwrap_or_default();
        let sun = frame.pbr_sun.unwrap_or_default();
        let env = frame.pbr_env.unwrap_or_default();

        let (vp, _inv_vp) = self.write_statics(&camera, &sun, &env, w as f32 / h as f32);
        let _ = vp;

        // Per-frame rt flag lives at params.screen.z.
        let rt_on = if frame.rt.is_some() { 1.0 } else { 0.0 };
        self.queue.write_buffer(
            &self.present_buf,
            offsets::PRESENT_PARAMS + 8,
            f32b(&[rt_on]).as_slice(),
        );

        // Shadow model matrix (identity for all meshes; per-mesh model is in
        // a separate buffer bound by the shadow pipeline).
        let identity = Mat4::IDENTITY.to_bytes();
        self.queue.write_buffer(&self.shadow_model_buf, 0, &identity);

        // Per-instance uniforms: grow model_buf when this frame needs more
        // slots than are currently allocated (pbr meshes + model costumes).
        let needed = (frame.pbr_meshes.len() + frame.models.len()) as u64;
        if needed > self.model_slots {
            let new_slots = (needed * 2).max(64);
            self.model_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("model"),
                size: UNIFORM_ALIGN * new_slots,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.model_slots = new_slots;
        }

        let mut meshes: Vec<(wgpu::BindGroup, wgpu::Buffer, wgpu::Buffer, u32)> = Vec::new();
        let mut slot = 0usize;
        for mesh in &frame.pbr_meshes {
            let builtin = matches!(mesh.mesh.as_str(), "plane" | "cube" | "sphere");
            if !builtin && mesh.model.is_none() {
                continue; // still streaming
            }
            let bind = self.upload_mesh_model(mesh, slot);
            let (vb, ib, count) = match &mesh.model {
                Some(m) => self.get_model_mesh(m),
                None => self.get_mesh(&mesh.mesh),
            };
            meshes.push((bind, vb, ib, count));
            slot += 1;
        }
        // 3D model costumes from sprites render through the same PBR pipe.
        for md in &frame.models {
            let synth = liteengine_core::scene::PbrMesh {
                name: format!("model-costume-{slot}"),
                mesh: "model".into(),
                model: None,
                pos: md.pos,
                rot: md.rot,
                scale: Vec3::new(md.scale, md.scale, md.scale),
                color: md.color,
                metallic: 0.05,
                roughness: 0.6,
                texture: None,
            };
            let bind = self.upload_mesh_model(&synth, slot);
            let (vb, ib, count) = self.get_model_mesh(&md.model);
            meshes.push((bind, vb, ib, count));
            slot += 1;
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });

        // 1. Shadow pass.
        if !meshes.is_empty() {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shadow"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.shadow_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.shadow_pipe);
            pass.set_bind_group(0, &self.shadow_light_bind, &[]);
            for (i, (_, vb, ib, count)) in meshes.iter().enumerate() {
                let model_bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("shadow-model"),
                    layout: &self.shadow_model_bgl,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &self.model_buf,
                            offset: i as u64 * UNIFORM_ALIGN,
                            size: wgpu::BufferSize::new(64),
                        }),
                    }],
                });
                pass.set_bind_group(1, &model_bind, &[]);
                pass.set_vertex_buffer(0, vb.slice(..));
                pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint16);
                pass.draw_indexed(0..*count, 0, 0..1);
            }
        }

        // 2. Main PBR pass into HDR + G-buffer.
        {
            let bg = frame.bg_color;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pbr-main"),
                color_attachments: &[
                    Some(wgpu::RenderPassColorAttachment {
                        view: &self.hdr_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: bg[0] as f64,
                                g: bg[1] as f64,
                                b: bg[2] as f64,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                    Some(wgpu::RenderPassColorAttachment {
                        view: &self.nd_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                    Some(wgpu::RenderPassColorAttachment {
                        view: &self.mat_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                ],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.mesh_pipe);
            pass.set_bind_group(0, &self.statics_bind, &[]);
            for (i, (bind, vb, ib, count)) in meshes.iter().enumerate() {
                pass.set_bind_group(1, bind, &[(i as u64 * UNIFORM_ALIGN) as u32]);
                pass.set_vertex_buffer(0, vb.slice(..));
                pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint16);
                pass.draw_indexed(0..*count, 0, 0..1);
            }
        }

        // 3. Present pass: tonemap (+SSR/AO when raytracing is enabled).
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("present"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.screen_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.present_pipe);
            pass.set_bind_group(0, &self.present_bind, &[]);
            pass.draw(0..3, 0..1);
        }

        // 4. Readback.
        let bytes_per_row = (w as u32 * 4).div_ceil(256) * 256;
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &self.screen_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &self.readback,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(h as u32),
                },
            },
            wgpu::Extent3d { width: w as u32, height: h as u32, depth_or_array_layers: 1 },
        );
        self.queue.submit(Some(encoder.finish()));

        // Map and convert RGBA8 -> ARGB u32 for the host.
        let slice = self.readback.slice(..(bytes_per_row as u64 * h as u64));
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        let _ = self.device.poll(wgpu::Maintain::Wait);
        rx.recv()
            .map_err(|_| "readback channel closed".to_string())?
            .map_err(|e| e.to_string())?;
        let data = slice.get_mapped_range();
        let mut argb = Vec::with_capacity(w * h);
        for y in 0..h {
            let row = &data[y * bytes_per_row as usize..y * bytes_per_row as usize + w * 4];
            for px in row.chunks_exact(4) {
                // Rgba8Unorm bytes -> 0xAARRGGBB.
                argb.push(0xff000000u32
                    | ((px[0] as u32) << 16)
                    | ((px[1] as u32) << 8)
                    | px[2] as u32);
            }
        }
        drop(data);
        self.readback.unmap();

        // CPU overlay layer: background image (when purely 2D), billboards,
        // 2D sprites and speech bubbles — pixel-identical to the legacy path.
        let mut fb = crate::fb::FrameBuf::new(w, h);
        fb.px.copy_from_slice(&argb);
        let no_meshes = meshes.is_empty();
        if no_meshes {
            if let Some(bg) = &frame.background {
                fb.blit_cover(bg);
            }
        }
        let cam = frame
            .rt
            .as_ref()
            .map(|r| r.camera)
            .or(frame.pbr_camera)
            .unwrap_or_default();
        crate::soft3d::draw_billboards(&mut fb, &frame.billboards, &cam);
        crate::soft2d::draw_sprites(&mut fb, &frame.sprites);
        for b in &frame.bubbles {
            crate::speech::draw_bubble(&mut fb, b);
        }
        for m in &frame.monitors {
            crate::monitor::draw_monitor(&mut fb, m);
        }
        host.present(&fb.px, fb.w, fb.h)
    }
}

//! Frame snapshots — the serializable render inputs handed to a backend.
//!
//! The runtime builds a [`Frame`] every tick from the live [`Scene`](crate::scene::Scene)
//! plus streamed textures, so renderers never touch Lua or the scheduler.

use crate::scene::{Camera, EnvColors, MonitorKind, RtQuality, RtScene, SunLight};
use std::sync::Arc;

/// Decoded RGBA8 texture, shared between the asset store and renderers.
#[derive(Debug, Clone)]
pub struct TextureData {
    pub width: u32,
    pub height: u32,
    /// RGBA8, row-major, top-left origin.
    pub rgba: Vec<u8>,
}

impl TextureData {
    /// Average edge colors used to generate the streaming placeholder.
    pub fn placeholder(width: u32, height: u32) -> TextureData {
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let checker = ((x / 8) + (y / 8)) % 2 == 0;
                let v = if checker { 190 } else { 120 };
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
        }
        TextureData {
            width,
            height,
            rgba,
        }
    }
}

/// A drawable 2D sprite for this frame.
#[derive(Debug, Clone)]
pub struct SpriteDraw {
    pub texture: Option<Arc<TextureData>>,
    /// Screen-space centre in pixels (already transformed from stage space).
    pub cx: f32,
    pub cy: f32,
    pub layer: i32,
    pub dir_deg: f32,
    pub scale: f32,
    pub alpha: f32,
    pub tint: [f32; 3],
    /// Draw a placeholder quad when the texture is still streaming.
    pub placeholder: bool,
    pub name: String,
    /// On-screen footprint in pixels (for bubble anchoring).
    pub foot_w: f32,
    pub foot_h: f32,
}

/// A camera-facing 3D billboard (2D costume displayed in the 3D world).
#[derive(Debug, Clone)]
pub struct BillboardDraw {
    pub texture: Option<Arc<TextureData>>,
    pub pos: crate::scene::Vec3,
    /// World-space width/height of the quad.
    pub width: f32,
    pub height: f32,
    pub alpha: f32,
    pub tint: [f32; 3],
    pub both_sides: bool,
    pub placeholder: bool,
}

/// A speech bubble request (2D or anchored to a 3D sprite via screen-space anchor).
#[derive(Debug, Clone)]
pub struct BubbleDraw {
    pub text: String,
    pub think: bool,
    /// Screen-space anchor (top-right of the sprite footprint).
    pub anchor_x: f32,
    pub anchor_y: f32,
}

/// Decoded triangle mesh for 3D model costumes (`.obj` / `.glb` / `.gltf`).
///
/// Interleaved `pos3 normal3 uv2` vertices and `u16` indices — the same
/// layout the wgpu mesh pipeline already consumes, so decoded models upload
/// straight into both renderers.
#[derive(Debug, Clone, Default)]
pub struct ModelData {
    /// pos3 normal3 uv2, 8 floats per vertex.
    pub vertices: Vec<f32>,
    pub indices: Vec<u16>,
}

impl ModelData {
    pub const STRIDE: usize = 8;

    pub fn vertex_count(&self) -> usize {
        self.vertices.len() / Self::STRIDE
    }

    pub fn index_count(&self) -> usize {
        self.indices.len()
    }

    /// Resident-memory estimate used by the streaming LRU budget.
    pub fn byte_size(&self) -> usize {
        self.vertices.len() * 4 + self.indices.len() * 2
    }

    /// Compute smooth per-vertex normals (area-weighted average of adjacent
    /// faces) — used when a model ships without `vn` records.
    pub fn compute_normals(&mut self) {
        let n_verts = self.vertex_count();
        if n_verts == 0 {
            return;
        }
        let mut acc = vec![0.0f32; n_verts * 3];
        for tri in self.indices.chunks_exact(3) {
            let (a, b, c) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
            if a >= n_verts || b >= n_verts || c >= n_verts {
                continue;
            }
            let px = |i: usize| (self.vertices[i * Self::STRIDE], self.vertices[i * Self::STRIDE + 1], self.vertices[i * Self::STRIDE + 2]);
            let (ax, ay, az) = px(a);
            let (bx, by, bz) = px(b);
            let (cx, cy, cz) = px(c);
            let e1 = [bx - ax, by - ay, bz - az];
            let e2 = [cx - ax, cy - ay, cz - az];
            let nx = e1[1] * e2[2] - e1[2] * e2[1];
            let ny = e1[2] * e2[0] - e1[0] * e2[2];
            let nz = e1[0] * e2[1] - e1[1] * e2[0];
            for &vi in &[a, b, c] {
                acc[vi * 3] += nx;
                acc[vi * 3 + 1] += ny;
                acc[vi * 3 + 2] += nz;
            }
        }
        for v in 0..n_verts {
            let (nx, ny, nz) = (acc[v * 3], acc[v * 3 + 1], acc[v * 3 + 2]);
            let len = (nx * nx + ny * ny + nz * nz).sqrt();
            let o = v * Self::STRIDE + 3;
            if len > 1e-9 {
                self.vertices[o] = nx / len;
                self.vertices[o + 1] = ny / len;
                self.vertices[o + 2] = nz / len;
            } else {
                self.vertices[o] = 0.0;
                self.vertices[o + 1] = 1.0;
                self.vertices[o + 2] = 0.0;
            }
        }
    }
}

/// A 3D sprite rendered as a real mesh (model costume), both render paths.
#[derive(Debug, Clone)]
pub struct ModelDraw {
    pub model: Arc<ModelData>,
    pub pos: crate::scene::Vec3,
    /// Euler rotation in degrees (XYZ order).
    pub rot: crate::scene::Vec3,
    pub scale: f32,
    /// Flat base colour (sprite tint).
    pub color: [f32; 3],
    pub alpha: f32,
    /// When false, back-faces are culled (legacy rasterizer + wgpu).
    pub two_sided: bool,
}

/// A Scratch-style monitor snapshot for the overlay layer.
#[derive(Debug, Clone)]
pub struct MonitorDraw {
    pub kind: MonitorKind,
    pub label: String,
    /// Variable monitors: formatted value text.
    pub value: String,
    /// List monitors: the visible window of items (already sliced).
    pub items: Vec<String>,
    /// List monitors: index of the first visible item (0-based, absolute).
    pub first_index: usize,
    /// List monitors: total item count.
    pub total: usize,
    /// Screen-space top-left corner in pixels.
    pub x: f32,
    pub y: f32,
    pub large: bool,
}

/// Raytracing parameters for this frame (legacy hybrid path).
#[derive(Debug, Clone)]
pub struct RtParams {
    pub quality: RtQuality,
    /// Internal resolution scale (0.05..1.0).
    pub scale: f32,
    pub bounces: u32,
    pub denoise: bool,
    /// Include 3D sprite billboards as quads in the traced scene.
    pub include_billboards: bool,
    /// Minimum temporal blend per frame: 0.35 when the scene is animating
    /// (bounded ghosting), ~0.03 when static (long accumulation).
    pub temporal_floor: f32,
    pub sun: SunLight,
    pub env: EnvColors,
    pub scene: RtScene,
    pub camera: Camera,
}

/// Everything a renderer needs for one frame.
#[derive(Debug, Clone)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub bg_color: [f32; 3],
    pub background: Option<Arc<TextureData>>,
    pub sprites: Vec<SpriteDraw>,
    pub billboards: Vec<BillboardDraw>,
    pub bubbles: Vec<BubbleDraw>,
    /// 3D model costumes (real meshes, both render paths).
    pub models: Vec<ModelDraw>,
    /// Scratch-style monitors (topmost overlay).
    pub monitors: Vec<MonitorDraw>,
    pub rt: Option<RtParams>,
    /// PBR data (used by the modern wgpu path; ignored by the legacy path).
    pub pbr_camera: Option<Camera>,
    pub pbr_sun: Option<SunLight>,
    pub pbr_env: Option<EnvColors>,
    pub pbr_meshes: Vec<crate::scene::PbrMesh>,
}

impl Frame {
    pub fn empty(width: usize, height: usize) -> Frame {
        Frame {
            width,
            height,
            bg_color: [0.12, 0.14, 0.18],
            background: None,
            sprites: Vec::new(),
            billboards: Vec::new(),
            bubbles: Vec::new(),
            models: Vec::new(),
            monitors: Vec::new(),
            rt: None,
            pbr_camera: None,
            pbr_sun: None,
            pbr_env: None,
            pbr_meshes: Vec::new(),
        }
    }
}

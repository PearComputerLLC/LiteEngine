//! Frame snapshots — the serializable render inputs handed to a backend.
//!
//! The runtime builds a [`Frame`] every tick from the live [`Scene`](crate::scene::Scene)
//! plus streamed textures, so renderers never touch Lua or the scheduler.

use crate::scene::{Camera, EnvColors, RtQuality, RtScene, SunLight};
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
            rt: None,
            pbr_camera: None,
            pbr_sun: None,
            pbr_env: None,
            pbr_meshes: Vec::new(),
        }
    }
}

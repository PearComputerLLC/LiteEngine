//! Scene model: sprites, cameras, and the ray-trace primitive scene.
//!
//! Coordinates follow Scratch conventions: the stage origin `(0, 0)` is the
//! centre of the window, `+x` is right, `+y` is **up**. 3D coordinates use the
//! right-handed convention (camera looks down `-z` by default).

/// Minimal 3-component vector used across core and renderers.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const ZERO: Vec3 = Vec3 { x: 0.0, y: 0.0, z: 0.0 };

    pub fn new(x: f32, y: f32, z: f32) -> Self {
        Vec3 { x, y, z }
    }

    pub fn add(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }

    pub fn sub(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }

    pub fn scale(self, s: f32) -> Vec3 {
        Vec3::new(self.x * s, self.y * s, self.z * s)
    }

    pub fn dot(self, o: Vec3) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }

    pub fn cross(self, o: Vec3) -> Vec3 {
        Vec3::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }

    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }

    pub fn normalize(self) -> Vec3 {
        let l = self.length();
        if l > 1e-6 {
            self.scale(1.0 / l)
        } else {
            Vec3::ZERO
        }
    }

    pub fn reflect(self, n: Vec3) -> Vec3 {
        self.sub(n.scale(2.0 * self.dot(n)))
    }
}

/// A speech-bubble state attached to a sprite.
#[derive(Debug, Clone)]
pub struct SayState {
    pub message: String,
    pub think: bool,
    /// Engine time (seconds) at which the bubble disappears.
    pub until: f64,
}

/// A sprite: the Scratch-anything of LiteEngine.
///
/// A sprite is 2D by default. Setting `in3d = true` turns it into a
/// camera-facing 3D billboard textured on **both sides**, which is how 2D
/// costumes are displayed inside 3D scenes.
#[derive(Debug, Clone)]
pub struct Sprite {
    pub id: u32,
    pub name: String,
    /// Asset path of the current costume (streamed by the asset store).
    pub costume: Option<String>,
    pub visible: bool,
    /// Stage-space position (centre origin, +y up), in pixels.
    pub x: f32,
    pub y: f32,
    /// Painter's-algorithm layer (higher = drawn on top).
    pub layer: i32,
    /// Facing direction in degrees (0 = up, clockwise, Scratch-style).
    pub direction: f32,
    /// Size as a percentage (100 = natural size).
    pub size: f32,
    pub alpha: f32,
    /// Optional multiply tint, linear 0..1 per channel.
    pub tint: [f32; 3],
    /// When true the sprite lives in the 3D world at `pos3` as a billboard.
    pub in3d: bool,
    pub pos3: Vec3,
    /// Billboard rendering with texture on both sides (default true for 3D).
    pub billboard_both_sides: bool,
    pub say: Option<SayState>,
    /// Estimated costume pixel size (set when the texture streams in).
    pub tex_w: u32,
    pub tex_h: u32,
}

impl Sprite {
    pub fn new(id: u32, name: &str) -> Sprite {
        Sprite {
            id,
            name: name.to_string(),
            costume: None,
            visible: true,
            x: 0.0,
            y: 0.0,
            layer: 0,
            direction: 90.0, // Scratch default: pointing right
            size: 100.0,
            alpha: 1.0,
            tint: [1.0, 1.0, 1.0],
            in3d: false,
            pos3: Vec3::ZERO,
            billboard_both_sides: true,
            say: None,
            tex_w: 0,
            tex_h: 0,
        }
    }

    /// Effective on-screen footprint in pixels (0 when costume unknown).
    pub fn footprint(&self) -> (f32, f32) {
        let s = self.size / 100.0;
        (self.tex_w as f32 * s, self.tex_h as f32 * s)
    }
}

/// Perspective camera shared by the 3D billboard renderer, the path tracer
/// and the wgpu PBR path.
#[derive(Debug, Clone, Copy)]
pub struct Camera {
    pub pos: Vec3,
    pub look_at: Vec3,
    pub fov_y_deg: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            pos: Vec3::new(0.0, 1.0, 5.0),
            look_at: Vec3::ZERO,
            fov_y_deg: 60.0,
        }
    }
}

impl Camera {
    /// Forward (view) direction.
    pub fn forward(&self) -> Vec3 {
        (self.look_at.sub(self.pos)).normalize()
    }

    /// Project a world point to screen-space pixels `(x, y, depth)`.
    /// `y` grows downward; returns `None` behind the near plane.
    pub fn project(&self, p: Vec3, width: f32, height: f32) -> Option<(f32, f32, f32)> {
        let f = self.forward();
        let up_world = Vec3::new(0.0, 1.0, 0.0);
        let right = f.cross(up_world).normalize();
        let up = right.cross(f);
        let rel = p.sub(self.pos);
        let z = rel.dot(f);
        if z <= 0.05 {
            return None;
        }
        let focal = (height * 0.5) / (self.fov_y_deg.to_radians() * 0.5).tan();
        let sx = width * 0.5 + rel.dot(right) * focal / z;
        let sy = height * 0.5 - rel.dot(up) * focal / z;
        Some((sx, sy, z))
    }
}

/// Directional sun light used by the RT scene and the PBR path.
#[derive(Debug, Clone, Copy)]
pub struct SunLight {
    pub dir: Vec3,
    pub color: [f32; 3],
    pub intensity: f32,
}

impl Default for SunLight {
    fn default() -> Self {
        SunLight {
            dir: Vec3::new(-0.5, -1.0, -0.3).normalize(),
            color: [1.0, 0.98, 0.94],
            intensity: 1.15,
        }
    }
}

/// Gradient environment (procedural sky) used as IBL proxy and RT miss color.
#[derive(Debug, Clone, Copy)]
pub struct EnvColors {
    pub sky_top: [f32; 3],
    pub sky_horizon: [f32; 3],
    pub ground: [f32; 3],
}

impl Default for EnvColors {
    fn default() -> Self {
        EnvColors {
            sky_top: [0.20, 0.42, 0.85],
            sky_horizon: [0.74, 0.86, 0.98],
            ground: [0.28, 0.26, 0.24],
        }
    }
}

/// A sphere primitive in the ray-trace scene.
#[derive(Debug, Clone)]
pub struct RtSphere {
    pub name: String,
    pub center: Vec3,
    pub radius: f32,
    pub color: [f32; 3],
    pub metallic: f32,
    pub roughness: f32,
    pub emissive: f32,
}

/// An infinite axis-aligned plane in the ray-trace scene (`axis` = plane normal).
#[derive(Debug, Clone)]
pub struct RtPlane {
    pub name: String,
    pub point: Vec3,
    pub normal: Vec3,
    pub color: [f32; 3],
    pub checkered: bool,
    pub metallic: f32,
    pub roughness: f32,
}

/// Full ray-trace scene state, editable from Lua via the `rt` table.
#[derive(Debug, Clone)]
pub struct RtScene {
    pub spheres: Vec<RtSphere>,
    pub planes: Vec<RtPlane>,
    pub sun: SunLight,
    pub env: EnvColors,
}

impl Default for RtScene {
    fn default() -> Self {
        RtScene {
            spheres: Vec::new(),
            planes: Vec::new(),
            sun: SunLight::default(),
            env: EnvColors::default(),
        }
    }
}

/// Quality tiers for raytracing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtQuality {
    Low,
    Medium,
    High,
}

impl RtQuality {
    pub fn parse(s: &str) -> RtQuality {
        match s.to_ascii_lowercase().as_str() {
            "low" => RtQuality::Low,
            "high" => RtQuality::High,
            _ => RtQuality::Medium,
        }
    }

    /// Jittered samples per pixel per frame.
    pub fn samples(self) -> u32 {
        match self {
            RtQuality::Low => 1,
            RtQuality::Medium => 2,
            RtQuality::High => 4,
        }
    }

    /// Default internal resolution scale (fraction of window resolution).
    pub fn default_scale(self) -> f32 {
        match self {
            RtQuality::Low => 0.20,
            RtQuality::Medium => 0.30,
            RtQuality::High => 0.45,
        }
    }
}

/// Optional PBR mesh instance (wgpu path; legacy path ignores meshes).
#[derive(Debug, Clone)]
pub struct PbrMesh {
    pub name: String,
    /// Built-in generator: "plane" | "cube" | "sphere" (or an .obj path).
    pub mesh: String,
    pub pos: Vec3,
    /// Euler rotation in degrees.
    pub rot: Vec3,
    pub scale: Vec3,
    pub color: [f32; 3],
    pub metallic: f32,
    pub roughness: f32,
    pub texture: Option<String>,
}

/// PBR scene state, editable from Lua via the `pbr3d` table (modern path).
#[derive(Debug, Clone, Default)]
pub struct PbrScene {
    pub camera: Option<Camera>,
    pub sun: Option<SunLight>,
    pub env: Option<EnvColors>,
    pub meshes: Vec<PbrMesh>,
}

/// The whole mutable scene state owned by the runtime.
#[derive(Debug, Clone, Default)]
pub struct Scene {
    pub sprites: Vec<Sprite>,
    pub next_sprite_id: u32,
    pub background_color: [f32; 3],
    pub background_image: Option<String>,
    pub camera: Camera,
    pub rt: RtScene,
    pub pbr: PbrScene,
}

impl Scene {
    pub fn new_sprite(&mut self, name: &str) -> &mut Sprite {
        let id = self.next_sprite_id;
        self.next_sprite_id += 1;
        self.sprites.push(Sprite::new(id, name));
        self.sprites.last_mut().unwrap()
    }

    pub fn sprite_by_id(&self, id: u32) -> Option<&Sprite> {
        self.sprites.iter().find(|s| s.id == id)
    }

    pub fn sprite_by_id_mut(&mut self, id: u32) -> Option<&mut Sprite> {
        self.sprites.iter_mut().find(|s| s.id == id)
    }

    pub fn sprite_by_name(&self, name: &str) -> Option<&Sprite> {
        self.sprites.iter().find(|s| s.name == name)
    }

    pub fn remove_sprite(&mut self, id: u32) {
        self.sprites.retain(|s| s.id != id);
    }
}

/// Parse a `#rrggbb` hex string into linear-ish 0..1 floats.
pub fn parse_color(s: &str) -> Option<[f32; 3]> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0])
}

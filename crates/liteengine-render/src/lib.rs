//! The legacy software renderer — the custom optimized rendering path.
//!
//! Frame pipeline (all CPU, no GPU drivers, runs on a 2010 MacBook Air):
//!
//! 1. background fill / streamed background image,
//! 2. **hybrid raytracing** (when enabled): software path tracer renders the
//!    RT scene at a reduced internal resolution with temporal accumulation,
//!    an edge-aware denoiser, and a sharpened bilinear upscale; the result is
//!    composited over the raster base using a feathered coverage mask,
//! 3. 3D billboards (camera-facing quads, texture on both sides),
//! 4. 2D sprites (rotated, scaled, tinted, alpha-blended, painter order),
//! 5. speech bubbles (`say`/`think`) anchored top-right of each sprite.

pub mod fb;
pub mod font;
pub mod monitor;
pub mod pathtrace;
pub mod soft2d;
pub mod soft3d;
pub mod softmesh;
pub mod speech;
pub mod window;

#[cfg(feature = "modern")]
pub mod modern;

use fb::FrameBuf;
use liteengine_core::host::{Host, Renderer};
use liteengine_core::snapshot::Frame;
use pathtrace::PathTracer;

/// The software renderer used on the legacy path (and in headless/CI runs).
pub struct SoftRenderer {
    fb: FrameBuf,
    /// Internal-resolution RT accumulator (allocated lazily).
    tracer: PathTracer,
    /// Depth buffer for 3D model costumes (reused across frames).
    depth: softmesh::DepthBuf,
    /// Render thread budget: usually `available_parallelism`.
    pub threads: usize,
}

impl SoftRenderer {
    pub fn new() -> SoftRenderer {
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        SoftRenderer {
            fb: FrameBuf::new(64, 64),
            tracer: PathTracer::new(),
            depth: softmesh::DepthBuf::new(),
            threads,
        }
    }
}

impl Default for SoftRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderer for SoftRenderer {
    fn name(&self) -> &'static str {
        "legacy-software"
    }

    fn render_frame(&mut self, frame: &Frame, host: &mut dyn Host) -> Result<(), String> {
        let (w, h) = host.size();
        self.fb.resize(w, h);

        // 1. Background.
        self.fb.clear_rgb(frame.bg_color);
        if let Some(bg) = &frame.background {
            self.fb.blit_cover(bg);
        }

        // 2. Hybrid raytracing composite (optional).
        if let Some(rt) = &frame.rt {
            self.tracer.set_denoise(rt.denoise);
            self.tracer
                .render(rt, &frame.billboards, frame.width, frame.height, self.threads);
            self.tracer.composite(&mut self.fb);
        }

        // 3. 3D billboards (crisp raster layer on top of the RT base).
        let cam = frame
            .rt
            .as_ref()
            .map(|r| r.camera)
            .or(frame.pbr_camera)
            .unwrap_or_default();
        soft3d::draw_billboards(&mut self.fb, &frame.billboards, &cam);

        // 3b. 3D model costumes (z-buffered mesh rasterizer).
        if !frame.models.is_empty() {
            let light = softmesh::MeshLight::resolve(
                frame.rt.as_ref().map(|r| r.sun).or(frame.pbr_sun),
                frame.rt.as_ref().map(|r| r.env).or(frame.pbr_env),
            );
            softmesh::draw_models(&mut self.fb, &mut self.depth, &frame.models, &cam, &light);
        }

        // 4. 2D sprites (already sorted by layer by the runtime).
        soft2d::draw_sprites(&mut self.fb, &frame.sprites);

        // 5. Speech bubbles.
        for b in &frame.bubbles {
            speech::draw_bubble(&mut self.fb, b);
        }

        // 6. Scratch-style monitors (topmost overlay).
        for m in &frame.monitors {
            monitor::draw_monitor(&mut self.fb, m);
        }

        host.present(&self.fb.px, self.fb.w, self.fb.h)
    }
}

/// Convenience: save the last headless frame as a PNG (used by `--screenshot`).
pub fn save_png(path: &str, argb: &[u32], w: usize, h: usize) -> Result<(), String> {
    let mut rgba = Vec::with_capacity(argb.len() * 4);
    for p in argb {
        let r = ((p >> 16) & 0xff) as u8;
        let g = ((p >> 8) & 0xff) as u8;
        let b = (p & 0xff) as u8;
        rgba.extend_from_slice(&[r, g, b, 255]);
    }
    image::save_buffer(path, &rgba, w as u32, h as u32, image::ColorType::Rgba8)
        .map_err(|e| e.to_string())
}

/// PNG encode/decode roundtrip used by `liteengine doctor`.
pub fn png_probe(_bytes: &[u8]) -> bool {
    // Synthesize a small image, encode it to PNG in memory, decode it back.
    let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([200, 30, 30, 255]));
    let mut png_bytes = std::io::Cursor::new(Vec::new());
    if image::DynamicImage::ImageRgba8(img)
        .write_to(&mut png_bytes, image::ImageFormat::Png)
        .is_err()
    {
        return false;
    }
    image::load_from_memory(png_bytes.get_ref()).is_ok()
}

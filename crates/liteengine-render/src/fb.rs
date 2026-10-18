//! CPU framebuffer with the drawing primitives used by the software path.
//!
//! Pixel format: `0xAARRGGBB` (alpha 255 = opaque), row-major, top-left origin.

use liteengine_core::snapshot::TextureData;

#[derive(Clone)]
pub struct FrameBuf {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u32>,
}

impl FrameBuf {
    pub fn new(w: usize, h: usize) -> FrameBuf {
        FrameBuf {
            w,
            h,
            px: vec![0xff000000; w * h],
        }
    }

    pub fn resize(&mut self, w: usize, h: usize) {
        if self.w != w || self.h != h || self.px.is_empty() {
            self.w = w.max(1);
            self.h = h.max(1);
            self.px = vec![0xff000000; self.w * self.h];
        }
    }

    pub fn clear_rgb(&mut self, rgb: [f32; 3]) {
        let p = pack_rgb(rgb);
        self.px.iter_mut().for_each(|v| *v = p);
    }

    #[inline]
    pub fn blend_px(&mut self, x: i32, y: i32, argb: u32) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        let idx = y as usize * self.w + x as usize;
        self.px[idx] = blend(self.px[idx], argb);
    }

    #[inline]
    pub fn set_px(&mut self, x: i32, y: i32, argb: u32) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        self.px[y as usize * self.w + x as usize] = argb;
    }

    pub fn fill_rect(&mut self, x0: i32, y0: i32, w: i32, h: i32, argb: u32) {
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                self.blend_px(x, y, argb);
            }
        }
    }

    /// Anti-aliased horizontal line (used by bubble tails).
    pub fn draw_line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, argb: u32, thickness: i32) {
        let dx = (x1 - x0) as f32;
        let dy = (y1 - y0) as f32;
        let steps = dx.abs().max(dy.abs()).ceil() as i32;
        for i in 0..=steps {
            let t = if steps == 0 { 0.0 } else { i as f32 / steps as f32 };
            let x = x0 as f32 + dx * t;
            let y = y0 as f32 + dy * t;
            for oy in -(thickness / 2)..=(thickness / 2) {
                for ox in -(thickness / 2)..=(thickness / 2) {
                    if ox * ox + oy * oy <= (thickness / 2).pow(2) {
                        self.blend_px(x as i32 + ox, y as i32 + oy, argb);
                    }
                }
            }
        }
    }

    /// Filled rounded rectangle with an outline.
    pub fn fill_round_rect(
        &mut self,
        x0: i32,
        y0: i32,
        w: i32,
        h: i32,
        radius: i32,
        fill: u32,
        outline: Option<u32>,
    ) {
        let r = radius.min(w / 2).min(h / 2);
        for y in 0..h {
            for x in 0..w {
                // Distance to the rounded-corner "super-rect".
                let cx = (x - r).max(0).min(w - 1 - r) as i32;
                let cy = (y - r).max(0).min(h - 1 - r) as i32;
                let in_corner_x = x < r || x >= w - r;
                let in_corner_y = y < r || y >= h - r;
                let inside = if in_corner_x && in_corner_y {
                    let dx = (x - if x < r { r } else { w - 1 - r }) as f32;
                    let dy = (y - if y < r { r } else { h - 1 - r }) as f32;
                    dx * dx + dy * dy <= (r * r) as f32
                } else {
                    cx >= 0 || cy >= 0 || true
                };
                if inside {
                    self.blend_px(x0 + x, y0 + y, fill);
                }
            }
        }
        if let Some(oline) = outline {
            // Outline: draw the border pixels where the neighbor is outside.
            for y in 0..h {
                for x in 0..w {
                    let on_border = x == 0 || y == 0 || x == w - 1 || y == h - 1;
                    let near_corner = (x < r || x >= w - r) && (y < r || y >= h - r);
                    if on_border && !near_corner {
                        self.blend_px(x0 + x, y0 + y, oline);
                    }
                }
            }
            // Corner arcs: sample along each corner circle.
            for cx_corner in [r, w - 1 - r] {
                for cy_corner in [r, h - 1 - r] {
                    for a in 0..360 {
                        let rad = (a as f32).to_radians();
                        let x = cx_corner as f32 + (r as f32 - 0.5) * rad.cos();
                        let y = cy_corner as f32 + (r as f32 - 0.5) * rad.sin();
                        let px = x0 + x as i32;
                        let py = y0 + y as i32;
                        let inside_x = (px - x0) >= 0 && (px - x0) < w;
                        let inside_y = (py - y0) >= 0 && (py - y0) < h;
                        if inside_x && inside_y {
                            self.blend_px(px, py, oline);
                        }
                    }
                }
            }
        }
    }

    pub fn fill_circle(&mut self, cx: f32, cy: f32, r: f32, argb: u32) {
        let r2 = r * r;
        for y in (cy as i32 - r as i32 - 1)..=(cy as i32 + r as i32 + 1) {
            for x in (cx as i32 - r as i32 - 1)..=(cx as i32 + r as i32 + 1) {
                let dx = x as f32 + 0.5 - cx;
                let dy = y as f32 + 0.5 - cy;
                if dx * dx + dy * dy <= r2 {
                    self.blend_px(x, y, argb);
                }
            }
        }
    }

    /// Blit a texture stretched to `dw x dh` at `dx,dy` (no rotation).
    pub fn blit_scaled(&mut self, tex: &TextureData, dx: f32, dy: f32, dw: f32, dh: f32) {
        if dw < 1.0 || dh < 1.0 {
            return;
        }
        let x0 = dx.floor() as i32;
        let y0 = dy.floor() as i32;
        for py in 0..dh as i32 {
            for px_i in 0..dw as i32 {
                let u = (px_i as f32 + 0.5) / dw;
                let v = (py as f32 + 0.5) / dh;
                let c = sample_bilinear(tex, u, v);
                self.blend_px(x0 + px_i, y0 + py, c);
            }
        }
    }

    /// Blit a texture covering the whole framebuffer (background image).
    pub fn blit_cover(&mut self, tex: &TextureData) {
        let scale = (self.w as f32 / tex.width as f32)
            .max(self.h as f32 / tex.height as f32);
        let dw = tex.width as f32 * scale;
        let dh = tex.height as f32 * scale;
        let dx = (self.w as f32 - dw) / 2.0;
        let dy = (self.h as f32 - dh) / 2.0;
        self.blit_scaled(tex, dx, dy, dw, dh);
    }

    /// Draw an 8x8-font string, scaled. Returns the pixel width.
    pub fn draw_text(&mut self, x: i32, y: i32, scale: i32, color: u32, text: &str) -> i32 {
        let mut cx = x;
        for ch in text.chars() {
            if let Some(idx) = crate::font::glyph_index(ch as u8) {
                let glyph = crate::font::FONT[idx];
                for row in 0..8usize {
                    let bits = glyph[row];
                    for col in 0..8usize {
                        if bits & (1 << col) != 0 {
                            for sy in 0..scale {
                                for sx in 0..scale {
                                    self.blend_px(
                                        cx + (col as i32) * scale + sx,
                                        y + (row as i32) * scale + sy,
                                        color,
                                    );
                                }
                            }
                        }
                    }
                }
            }
            cx += 6 * scale; // 8px advance would look wide; 6px is classic
            if ch == ' ' {
                cx += scale;
            }
        }
        cx - x
    }

    pub fn text_width(&self, text: &str, scale: i32) -> i32 {
        let mut w = 0;
        for ch in text.chars() {
            w += if ch == ' ' { 7 } else { 6 } * scale;
        }
        w
    }

    /// Word-wrap `text` to `max_w` pixels at `scale`; returns lines.
    pub fn wrap_text(text: &str, max_w: i32, scale: i32) -> Vec<String> {
        let mut lines = Vec::new();
        for para in text.split('\n') {
            let mut cur = String::new();
            for word in para.split(' ') {
                let attempt = if cur.is_empty() {
                    word.to_string()
                } else {
                    format!("{cur} {word}")
                };
                let w = {
                    let mut tw = 0;
                    for ch in attempt.chars() {
                        tw += if ch == ' ' { 7 } else { 6 } * scale;
                    }
                    tw
                };
                if w > max_w && !cur.is_empty() {
                    lines.push(cur);
                    cur = word.to_string();
                } else {
                    cur = attempt;
                }
            }
            lines.push(cur);
        }
        lines
    }
}

/// Pack 0..1 rgb floats into opaque ARGB.
#[inline]
pub fn pack_rgb(rgb: [f32; 3]) -> u32 {
    let r = (rgb[0].clamp(0.0, 1.0) * 255.0) as u32;
    let g = (rgb[1].clamp(0.0, 1.0) * 255.0) as u32;
    let b = (rgb[2].clamp(0.0, 1.0) * 255.0) as u32;
    0xff000000 | (r << 16) | (g << 8) | b
}

/// Pack 0..1 rgb + alpha 0..1 into ARGB.
#[inline]
pub fn pack_rgba(rgb: [f32; 3], a: f32) -> u32 {
    let p = pack_rgb(rgb);
    let a = (a.clamp(0.0, 1.0) * 255.0) as u32;
    (a << 24) | (p & 0x00ffffff)
}

/// Bilinear sample of a texture; returns premultiplied-style ARGB.
#[inline]
pub fn sample_bilinear(tex: &TextureData, u: f32, v: f32) -> u32 {
    let x = u.clamp(0.0, 1.0) * (tex.width as f32 - 1.0);
    let y = v.clamp(0.0, 1.0) * (tex.height as f32 - 1.0);
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = (x0 + 1).min(tex.width - 1);
    let y1 = (y0 + 1).min(tex.height - 1);
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let c00 = texel(tex, x0, y0);
    let c10 = texel(tex, x1, y0);
    let c01 = texel(tex, x0, y1);
    let c11 = texel(tex, x1, y1);
    let mix = |a: u32, b: u32, t: f32| (a as f32 + (b as f32 - a as f32) * t) as u32;
    let ch = |c: u32, s: u32| (c >> s) & 0xff;
    let blend2 = |a: u32, b: u32, t: f32, s: u32| mix(ch(a, s), ch(b, s), t);
    let top = [
        blend2(c00, c10, fx, 16),
        blend2(c00, c10, fx, 8),
        blend2(c00, c10, fx, 0),
    ];
    let bot = [
        blend2(c01, c11, fx, 16),
        blend2(c01, c11, fx, 8),
        blend2(c01, c11, fx, 0),
    ];
    let a = mix(
        (c00 >> 24) & 0xff,
        (c10 >> 24) & 0xff,
        fx,
    );
    let a2 = mix(
        (c01 >> 24) & 0xff,
        (c11 >> 24) & 0xff,
        fx,
    );
    let alpha = mix(a, a2, fy);
    let r = mix(top[0], bot[0], fy);
    let g = mix(top[1], bot[1], fy);
    let b = mix(top[2], bot[2], fy);
    (alpha << 24) | (r << 16) | (g << 8) | b
}

#[inline]
fn texel(tex: &TextureData, x: u32, y: u32) -> u32 {
    let i = (y as usize * tex.width as usize + x as usize) * 4;
    let d = &tex.rgba;
    if i + 3 < d.len() {
        ((d[i + 3] as u32) << 24) | ((d[i] as u32) << 16) | ((d[i + 1] as u32) << 8) | d[i + 2] as u32
    } else {
        0
    }
}

/// Alpha blend `src` over `dst`.
#[inline]
pub fn blend(dst: u32, src: u32) -> u32 {
    let sa = ((src >> 24) & 0xff) as u32;
    if sa == 255 {
        return src;
    }
    if sa == 0 {
        return dst;
    }
    let da = 255 - sa;
    let ch = |c: u32, s: u32| ((c >> s) & 0xff) as u32;
    let mix = |a: u32, b: u32| ((a * sa + b * da) / 255) as u32;
    let r = mix(ch(src, 16), ch(dst, 16));
    let g = mix(ch(src, 8), ch(dst, 8));
    let b = mix(ch(src, 0), ch(dst, 0));
    0xff000000 | (r << 16) | (g << 8) | b
}

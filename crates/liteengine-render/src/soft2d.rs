//! 2D sprite rasterization: rotation + scale blits with bilinear sampling,
//! tint and alpha, drawn in painter order (runtime pre-sorts by layer).

use crate::fb::{blend, pack_rgb, sample_bilinear, FrameBuf};
use liteengine_core::snapshot::{SpriteDraw, TextureData};

/// Draw a rotated/scaled sprite. Inverse-maps every destination pixel inside
/// the sprite's axis-aligned bounding box back to texture space, so there is
/// no hole artifact at any angle.
fn draw_sprite_rot(fb: &mut FrameBuf, s: &SpriteDraw, tex: &TextureData) {
    let rad = s.dir_deg.to_radians();
    let (sin, cos) = rad.sin_cos();

    let scale = s.scale.max(0.01);
    let sw = tex.width as f32 * scale;
    let sh = tex.height as f32 * scale;

    // Half-extents of the rotated sprite (bounding box).
    let hx = (sw * cos.abs() + sh * sin.abs()) / 2.0;
    let hy = (sw * sin.abs() + sh * cos.abs()) / 2.0;

    let cx = s.cx;
    let cy = s.cy;
    let x0 = (cx - hx).floor() as i32;
    let y0 = (cy - hy).floor() as i32;
    let x1 = (cx + hx).ceil() as i32;
    let y1 = (cy + hy).ceil() as i32;

    let alpha = s.alpha.clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return;
    }
    let tint = s.tint;

    for py in y0..y1 {
        for px in x0..x1 {
            // Inverse rotate (px,py) around sprite centre into sprite space.
            let ox = px as f32 + 0.5 - cx;
            let oy = py as f32 + 0.5 - cy;
            let u_local = ox * cos + oy * sin; // along sprite width
            let v_local = -ox * sin + oy * cos; // along sprite height
            if u_local.abs() > sw / 2.0 || v_local.abs() > sh / 2.0 {
                continue;
            }
            let u = (u_local / sw + 0.5).clamp(0.0, 0.9999);
            let v = (v_local / sh + 0.5).clamp(0.0, 0.9999);
            let c = sample_bilinear(tex, u, v);
            // Apply tint + alpha.
            let r = (((c >> 16) & 0xff) as f32 * tint[0]) as u32;
            let g = (((c >> 8) & 0xff) as f32 * tint[1]) as u32;
            let b = ((c & 0xff) as f32 * tint[2]) as u32;
            let a = (((c >> 24) & 0xff) as f32 * alpha) as u32;
            let argb = (a << 24) | (r.min(255) << 16) | (g.min(255) << 8) | b.min(255);
            let dst_x = px;
            let dst_y = py;
            if dst_x >= 0 && dst_y >= 0 && (dst_x as usize) < fb.w && (dst_y as usize) < fb.h {
                let idx = dst_y as usize * fb.w + dst_x as usize;
                fb.px[idx] = blend(fb.px[idx], argb);
            }
        }
    }
}

/// Tinted translucent placeholder (checkerboard) for still-streaming sprites.
fn draw_placeholder(fb: &mut FrameBuf, s: &SpriteDraw) {
    let size = 64.0 * s.scale;
    let x0 = (s.cx - size / 2.0) as i32;
    let y0 = (s.cy - size / 2.0) as i32;
    let color = pack_rgb([0.45, 0.47, 0.5]);
    let color2 = pack_rgb([0.72, 0.74, 0.77]);
    for y in 0..size as i32 {
        for x in 0..size as i32 {
            let checker = ((x / 8) + (y / 8)) % 2 == 0;
            let c = if checker { color } else { color2 };
            let argb = (0x99u32 << 24) | (c & 0xffffff);
            fb.blend_px(x0 + x, y0 + y, argb);
        }
    }
}

/// Draw all 2D sprites in the given order.
pub fn draw_sprites(fb: &mut FrameBuf, sprites: &[SpriteDraw]) {
    for s in sprites {
        if let Some(tex) = &s.texture {
            draw_sprite_rot(fb, s, tex);
        } else if s.placeholder {
            draw_placeholder(fb, s);
        }
    }
}

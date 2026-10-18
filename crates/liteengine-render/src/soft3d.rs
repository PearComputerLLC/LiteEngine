//! 3D billboard rasterization.
//!
//! Sprites carrying a 2D costume inside the 3D world are drawn as
//! camera-facing quads textured on **both sides**: there is no backface
//! culling, and when the camera sees the quad from behind the texture is
//! mirrored horizontally so text/figures still read correctly.

use crate::fb::{blend, sample_bilinear, FrameBuf};
use liteengine_core::scene::{Camera, Vec3};
use liteengine_core::snapshot::BillboardDraw;

struct Basis {
    right: Vec3,
    up: Vec3,
    fwd: Vec3,
}

fn camera_basis(cam: &Camera) -> Basis {
    let fwd = cam.forward();
    let up_world = Vec3::new(0.0, 1.0, 0.0);
    let right = fwd.cross(up_world).normalize();
    let up = right.cross(fwd);
    Basis { right, up, fwd }
}

/// Draw all billboards, far-to-near (painter's algorithm).
pub fn draw_billboards(fb: &mut FrameBuf, billboards: &[BillboardDraw], cam: &Camera) {
    let basis = camera_basis(cam);

    // Compute depth for sorting.
    let mut order: Vec<(usize, f32)> = billboards
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let rel = b.pos.sub(cam.pos);
            (i, rel.dot(basis.fwd))
        })
        .collect();
    order.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let focal = (fb.h as f32 * 0.5) / (cam.fov_y_deg.to_radians() * 0.5).tan();

    for (i, depth) in order {
        let b = &billboards[i];
        if depth <= 0.05 {
            continue; // behind camera
        }
        let rel = b.pos.sub(cam.pos);
        let x_cam = rel.dot(basis.right);
        let y_cam = rel.dot(basis.up);
        let sx = fb.w as f32 * 0.5 + x_cam * focal / depth;
        let sy = fb.h as f32 * 0.5 - y_cam * focal / depth;
        let screen_w = b.width * focal / depth;
        let screen_h = b.height * focal / depth;
        if screen_w < 1.0 || screen_h < 1.0 {
            continue;
        }

        // Determine facing side: sign of the right-axis component of the
        // view direction relative to the billboard plane. Screen-aligned
        // billboards always face the camera; for plane-aligned billboards
        // (added later) this mirrors the texture on the back side.
        let facing_back = rel.dot(basis.right) < 0.0;

        let x0 = (sx - screen_w / 2.0).floor() as i32;
        let y0 = (sy - screen_h / 2.0).floor() as i32;
        let alpha = b.alpha.clamp(0.0, 1.0);

        for py in 0..screen_h as i32 {
            for px in 0..screen_w as i32 {
                let mut u = (px as f32 + 0.5) / screen_w;
                if facing_back && b.both_sides {
                    u = 1.0 - u; // mirror on the back side
                }
                let v = (py as f32 + 0.5) / screen_h;
                let argb = match &b.texture {
                    Some(tex) => {
                        let c = sample_bilinear(tex, u, v);
                        let r = (((c >> 16) & 0xff) as f32 * b.tint[0]) as u32;
                        let g = (((c >> 8) & 0xff) as f32 * b.tint[1]) as u32;
                        let bl = ((c & 0xff) as f32 * b.tint[2]) as u32;
                        let a = (((c >> 24) & 0xff) as f32 * alpha) as u32;
                        (a.min(255) << 24)
                            | (r.min(255) << 16)
                            | (g.min(255) << 8)
                            | bl.min(255)
                    }
                    None => {
                        if !b.placeholder {
                            continue;
                        }
                        let checker = (((px / 8) as i32 + (py / 8) as i32) % 2) == 0;
                        let v0 = if checker { 190u32 } else { 120 };
                        (0x99u32 << 24) | (v0 << 16) | (v0 << 8) | v0
                    }
                };
                let dx = x0 + px;
                let dy = y0 + py;
                if dx >= 0 && dy >= 0 && (dx as usize) < fb.w && (dy as usize) < fb.h {
                    let idx = dy as usize * fb.w + dx as usize;
                    fb.px[idx] = blend(fb.px[idx], argb);
                }
            }
        }
    }
}

//! Speech bubbles (`say` / `think`), Scratch-style.
//!
//! Bubbles are anchored to the **top-right** of the sprite footprint: the
//! bubble box sits above-right of the anchor and a tail (or thought dots)
//! points back down-left to the sprite. They are screen-space billboards,
//! so they work identically for 2D and 3D sprites.

use crate::fb::FrameBuf;
use liteengine_core::snapshot::BubbleDraw;

const MAX_WIDTH: i32 = 230;
const SCALE: i32 = 2;
const PADDING: i32 = 8;
const RADIUS: i32 = 8;

const WHITE: u32 = 0xffffffff;
const OUTLINE: u32 = 0xff20242c;
const TEXT: u32 = 0xff20242c;

pub fn draw_bubble(fb: &mut FrameBuf, b: &BubbleDraw) {
    let lines = FrameBuf::wrap_text(&b.text, MAX_WIDTH, SCALE);
    if lines.is_empty() {
        return;
    }
    let text_w = lines
        .iter()
        .map(|l| fb.text_width(l, SCALE))
        .max()
        .unwrap_or(0);
    let line_h = 8 * SCALE + 3;
    let bw = text_w + PADDING * 2;
    let bh = lines.len() as i32 * line_h + PADDING * 2 - 3;

    // Bubble placement: up-right of the anchor, clamped to the screen.
    let mut bx = b.anchor_x as i32 + 14;
    let mut by = b.anchor_y as i32 - bh - 26;
    bx = bx.clamp(4, fb.w as i32 - bw - 4);
    by = by.clamp(4, fb.h as i32 - bh - 4);

    // Bubble shadow (cheap depth cue).
    fb.fill_round_rect(bx + 2, by + 2, bw, bh, RADIUS, 0x40000000, None);
    // Bubble body.
    fb.fill_round_rect(bx, by, bw, bh, RADIUS, WHITE, Some(OUTLINE));

    if b.think {
        // Thought dots leading back to the anchor.
        let mut t = 0.0;
        while t < 1.0 {
            let x = bx as f32 + (b.anchor_x - bx as f32) * t;
            let y = (by + bh) as f32 + (b.anchor_y - (by + bh) as f32) * t;
            let r = 1.5 + 2.5 * t;
            fb.fill_circle(x, y, r, OUTLINE);
            t += 0.22;
        }
    } else {
        // Speech tail: two lines forming a pointer to the anchor.
        let tip_x = b.anchor_x as i32;
        let tip_y = b.anchor_y as i32;
        let base_x = bx + bw / 3;
        let base_y = by + bh;
        fb.draw_line(base_x, base_y - 1, tip_x, tip_y, WHITE, 3);
        fb.draw_line(base_x + 4, base_y - 1, tip_x, tip_y, OUTLINE, 1);
        fb.draw_line(base_x, base_y - 1, base_x + 4, base_y - 1, OUTLINE, 1);
        // Re-cover the seam so the tail looks connected.
        fb.fill_round_rect(bx, by, bw, bh, RADIUS, WHITE, Some(OUTLINE));
        fb.draw_line(base_x + 1, base_y - 2, tip_x - 1, tip_y + 1, WHITE, 2);
    }

    // Text.
    for (i, line) in lines.iter().enumerate() {
        fb.draw_text(
            bx + PADDING,
            by + PADDING - 2 + i as i32 * line_h,
            SCALE,
            TEXT,
            line,
        );
    }
}

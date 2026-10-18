//! Scratch-style monitors — variable watchers and list watchers.
//!
//! Drawn on the CPU overlay layer of **both** render paths (the wgpu path
//! composites them after readback, exactly like speech bubbles), so the
//! look is pixel-identical everywhere:
//!
//! * variable monitor: translucent white panel, dark label, orange value box
//!   (`large` style: a single big orange readout),
//! * list monitor: white card with an orange title bar, indexed rows,
//!   separators, and an `(N items)` footer.

use crate::fb::FrameBuf;
use liteengine_core::scene::MonitorKind;
use liteengine_core::snapshot::MonitorDraw;

const PANEL: u32 = 0xe9ffffff; // translucent white card
const ORANGE: u32 = 0xffff8c1a; // Scratch value/category orange
const TITLE: u32 = 0xffcf7500; // darker orange for the title bar bottom edge
const TEXT_DARK: u32 = 0xff575e75; // Scratch text-on-light
const TEXT_WHITE: u32 = 0xffffffff;
const SEPARATOR: u32 = 0x3099aabb;
const ROW_ALT: u32 = 0x14b8c4d8;
const OUTLINE: u32 = 0x4020242c;

const SCALE: i32 = 2;
const PAD: i32 = 7;
const RADIUS: i32 = 5;
const MIN_W: i32 = 64;

pub fn draw_monitor(fb: &mut FrameBuf, m: &MonitorDraw) {
    match m.kind {
        MonitorKind::Variable => draw_variable(fb, m),
        MonitorKind::List => draw_list(fb, m),
    }
}

// ---------------------------------------------------------------- variable

fn draw_variable(fb: &mut FrameBuf, m: &MonitorDraw) {
    if m.large {
        // Large readout: one big orange box, value only.
        let scale = 3;
        let vw = m.value.chars().count() as i32 * 6 * scale + PAD * 2;
        let vh = 8 * scale + PAD * 2;
        let x = m.x as i32;
        let y = m.y as i32;
        fb.fill_round_rect(x + 2, y + 2, vw, vh, RADIUS, 0x50000000, None);
        fb.fill_round_rect(x, y, vw, vh, RADIUS, ORANGE, Some(OUTLINE));
        fb.draw_text(x + PAD, y + PAD, scale, TEXT_WHITE, &m.value);
        return;
    }

    // Normal readout: [white panel: label + orange value box].
    let scale = SCALE;
    let label_w = fb.text_width(&m.label, scale);
    let value_w = (m.value.chars().count() as i32 * 6 * scale + 12).max(28);
    let line_h = 8 * scale;
    let pw = (label_w + value_w + PAD * 3).max(MIN_W);
    let ph = line_h + PAD * 2 - 2;
    let x = m.x as i32;
    let y = m.y as i32;

    fb.fill_round_rect(x + 2, y + 2, pw, ph, RADIUS, 0x40000000, None);
    fb.fill_round_rect(x, y, pw, ph, RADIUS, PANEL, Some(OUTLINE));
    let text_y = y + PAD - 2;
    fb.draw_text(x + PAD, text_y, scale, TEXT_DARK, &m.label);
    // Value box flush against the panel's right edge.
    let vx = x + pw - value_w - PAD / 2;
    let vy = y + 3;
    let vh = ph - 6;
    fb.fill_round_rect(vx, vy, value_w, vh, 3, ORANGE, None);
    // Center the value in the box, truncated if it overflows.
    let max_chars = (value_w - 8) as usize / (6 * scale as usize).max(1);
    let shown: String = if m.value.chars().count() > max_chars {
        m.value.chars().take(max_chars.saturating_sub(1)).collect::<String>() + "…"
    } else {
        m.value.clone()
    };
    let tw = fb.text_width(&shown, scale);
    let tx = vx + (value_w - tw) / 2;
    let ty = vy + (vh - line_h) / 2;
    fb.draw_text(tx, ty, scale, TEXT_WHITE, &shown);
}

// -------------------------------------------------------------------- list

fn draw_list(fb: &mut FrameBuf, m: &MonitorDraw) {
    let scale = SCALE;
    let row_h = 8 * scale + 4;
    let idx_w = 2 * 6 * scale; // two-digit index column
    let n_rows = m.items.len().max(1) as i32;

    // Widest row decides the card width.
    let mut content_w = fb.text_width(&m.label, scale) + PAD * 2;
    for (i, item) in m.items.iter().enumerate() {
        let row_text = format!("{} {}", i + 1 + m.first_index, item);
        let w = fb.text_width(&row_text, scale) + PAD * 3;
        content_w = content_w.max(w);
    }
    content_w = content_w.max(MIN_W).min(320);
    let title_h = 8 * scale + 10;
    let footer_h = 8 * scale + 8;
    let ph = title_h + n_rows * row_h + footer_h;
    let x = m.x as i32;
    let y = m.y as i32;

    fb.fill_round_rect(x + 2, y + 2, content_w, ph, RADIUS, 0x40000000, None);
    fb.fill_round_rect(x, y, content_w, ph, RADIUS, PANEL, Some(OUTLINE));

    // Title bar (orange) with the watched name.
    fb.fill_round_rect(x, y, content_w, title_h + RADIUS, RADIUS, ORANGE, None);
    fb.fill_rect(x, y + title_h / 2, content_w, title_h - title_h / 2 + 1, ORANGE);
    fb.draw_text(x + PAD, y + 5, scale, TEXT_WHITE, &m.label);

    // Rows.
    let list_top = y + title_h;
    if m.items.is_empty() {
        fb.draw_text(x + PAD, list_top + 4, scale, TEXT_DARK, "(empty)");
    }
    for (i, item) in m.items.iter().enumerate() {
        let ry = list_top + i as i32 * row_h;
        if i % 2 == 1 {
            fb.fill_rect(x + 1, ry, content_w - 2, row_h, ROW_ALT);
        }
        let idx = format!("{:2}", i + 1 + m.first_index);
        fb.draw_text(x + PAD, ry + 2, scale, TEXT_DARK, &idx);
        fb.draw_text(x + PAD + idx_w + 8, ry + 2, scale, TEXT_DARK, item);
        if i + 1 < m.items.len() {
            fb.fill_rect(x + 3, ry + row_h - 1, content_w - 6, 1, SEPARATOR);
        }
    }

    // Footer with the total count.
    let footer_y = list_top + n_rows * row_h;
    fb.fill_rect(x + 1, footer_y, content_w - 2, 1, SEPARATOR);
    let footer = if m.total == 1 { "(1 item)".to_string() } else { format!("({} items)", m.total) };
    fb.draw_text(x + PAD, footer_y + 3, scale, TEXT_DARK, &footer);
    let _ = TITLE;
}

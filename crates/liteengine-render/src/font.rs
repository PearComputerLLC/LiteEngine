//! Bitmap font facade over the generated 8x8 glyph table.

#[path = "font_data.rs"]
mod font_data;

pub use font_data::{glyph_index, FONT};

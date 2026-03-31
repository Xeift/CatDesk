#![allow(dead_code)]
// Type definitions
use image::{Rgba, RgbaImage};

pub type RGBA = Rgba<u8>;
pub type Image = RgbaImage;
pub type Color = (u8, u8, u8, u8);

// Helper to create RGBA
pub fn rgba(r: u8, g: u8, b: u8, a: u8) -> RGBA {
    Rgba([r, g, b, a])
}

pub fn color_to_rgba(c: Color) -> RGBA {
    rgba(c.0, c.1, c.2, c.3)
}

pub fn rgba_to_color(c: RGBA) -> Color {
    (c[0], c[1], c[2], c[3])
}

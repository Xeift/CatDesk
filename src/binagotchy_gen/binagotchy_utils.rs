#![allow(dead_code)]
// Utility functions for image manipulation
use super::types::{Color, color_to_rgba};
use image::{Rgba, RgbaImage};
use rand::Rng;

pub fn rint<R: Rng>(rng: &mut R, a: i32, b: i32) -> i32 {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    rng.gen_range(lo..=hi)
}

pub fn shift_mask(mask: &RgbaImage, dx: i32, dy: i32) -> RgbaImage {
    let (w, h) = mask.dimensions();
    let mut out = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));

    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let nx = x + dx;
            let ny = y + dy;
            if nx >= 0 && nx < w as i32 && ny >= 0 && ny < h as i32 {
                let pixel = mask.get_pixel(x as u32, y as u32);
                out.put_pixel(nx as u32, ny as u32, *pixel);
            }
        }
    }
    out
}

pub fn dilate(mask: &RgbaImage, radius: i32) -> RgbaImage {
    let mut out = mask.clone();
    let mut base = mask.clone();

    for _ in 0..radius {
        let mut acc = out.clone();
        let directions = [
            (-1, 0),
            (1, 0),
            (0, -1),
            (0, 1),
            (-1, -1),
            (-1, 1),
            (1, -1),
            (1, 1),
        ];

        for (dx, dy) in directions.iter() {
            let shifted = shift_mask(&base, *dx, *dy);
            acc = lighter(&acc, &shifted);
        }
        out = acc.clone();
        base = out.clone();
    }
    out
}

pub fn lighter(a: &RgbaImage, b: &RgbaImage) -> RgbaImage {
    let (w, h) = a.dimensions();
    let mut out = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));

    for y in 0..h {
        for x in 0..w {
            let pa = a.get_pixel(x, y);
            let pb = b.get_pixel(x, y);
            let pixel = Rgba([
                pa[0].max(pb[0]),
                pa[1].max(pb[1]),
                pa[2].max(pb[2]),
                pa[3].max(pb[3]),
            ]);
            out.put_pixel(x, y, pixel);
        }
    }
    out
}

pub fn subtract(a: &RgbaImage, b: &RgbaImage) -> RgbaImage {
    let (w, h) = a.dimensions();
    let mut out = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));

    for y in 0..h {
        for x in 0..w {
            let pa = a.get_pixel(x, y);
            let pb = b.get_pixel(x, y);
            // For grayscale mask operations: subtract RGB but keep alpha at 255
            let pixel = Rgba([
                pa[0].saturating_sub(pb[0]),
                pa[1].saturating_sub(pb[1]),
                pa[2].saturating_sub(pb[2]),
                255, // Keep alpha at 255 for grayscale masks
            ]);
            out.put_pixel(x, y, pixel);
        }
    }
    out
}

pub fn outline_ring(mask: &RgbaImage) -> RgbaImage {
    let dilated = dilate(mask, 1);
    subtract(&dilated, mask)
}

pub fn upscale_nearest(img: &RgbaImage, s: u32) -> RgbaImage {
    let (w, h) = img.dimensions();
    let mut out = RgbaImage::from_pixel(w * s, h * s, Rgba([0, 0, 0, 0]));

    for y in 0..h * s {
        for x in 0..w * s {
            let src_x = x / s;
            let src_y = y / s;
            let pixel = img.get_pixel(src_x, src_y);
            out.put_pixel(x, y, *pixel);
        }
    }
    out
}

pub fn scale_factor(canvas: i32) -> f32 {
    if canvas > 0 {
        canvas as f32 / 32.0
    } else {
        1.0
    }
}

pub fn s(v: i32, scale: f32) -> i32 {
    (v as f32 * scale).round() as i32
}

pub fn stable_seed(text: &str, base_seed: u64) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    let hash = hasher.finish();
    hash.wrapping_add(base_seed) & 0xFFFFFFFF
}

pub fn composite_clip(overlay: &RgbaImage, mask: &RgbaImage) -> RgbaImage {
    let (w, h) = overlay.dimensions();
    let mut out = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));

    for y in 0..h {
        for x in 0..w {
            let m = mask.get_pixel(x, y)[0];
            if m > 0 {
                out.put_pixel(x, y, *overlay.get_pixel(x, y));
            }
        }
    }
    out
}

pub fn lighten(c: Color, amt: i32) -> Color {
    (
        (c.0 as i32 + amt).min(255) as u8,
        (c.1 as i32 + amt).min(255) as u8,
        (c.2 as i32 + amt).min(255) as u8,
        c.3,
    )
}

pub fn darken(c: Color, amt: i32) -> Color {
    (
        (c.0 as i32 - amt).max(0) as u8,
        (c.1 as i32 - amt).max(0) as u8,
        (c.2 as i32 - amt).max(0) as u8,
        c.3,
    )
}

pub fn luminance(c: Color) -> f32 {
    0.2126 * c.0 as f32 + 0.7152 * c.1 as f32 + 0.0722 * c.2 as f32
}

pub fn blend_rgb(a: Color, b: Color, t: f32) -> Color {
    let t = t.max(0.0).min(1.0);
    (
        ((a.0 as f32 * (1.0 - t) + b.0 as f32 * t).round() as u8),
        ((a.1 as f32 * (1.0 - t) + b.1 as f32 * t).round() as u8),
        ((a.2 as f32 * (1.0 - t) + b.2 as f32 * t).round() as u8),
        a.3,
    )
}

pub fn with_alpha(c: Color, alpha: i32) -> Color {
    (c.0, c.1, c.2, alpha.max(0).min(255) as u8)
}

pub fn tone_match_patch(
    patch: Color,
    base: Color,
    max_delta_lum: f32,
    min_lum: f32,
    max_lum: f32,
    alpha: i32,
) -> Color {
    let base_l = luminance(base);
    let mut p = patch;

    // Blend toward base if contrast too high
    let p_l = luminance(p);
    let delta = (p_l - base_l).abs();
    if delta > max_delta_lum {
        let keep = max_delta_lum / delta.max(1e-6);
        let t = 1.0 - keep;
        p = blend_rgb(p, base, t);
    }

    // Clamp extremes
    let p_l = luminance(p);
    if p_l < min_lum {
        p = lighten(p, (min_lum - p_l) as i32);
    } else if p_l > max_lum {
        p = darken(p, (p_l - max_lum) as i32);
    }

    with_alpha(p, alpha)
}

pub fn pick_patch_color<R: Rng>(
    rng: &mut R,
    base: Color,
    palette: &[Color],
    max_pick_delta: f32,
    tone_max_delta: f32,
    alpha: i32,
) -> Color {
    let base_l = luminance(base);
    let candidates: Vec<Color> = palette
        .iter()
        .filter(|c| (luminance(**c) - base_l).abs() <= max_pick_delta)
        .copied()
        .collect();

    let c = if candidates.is_empty() {
        palette[rng.gen_range(0..palette.len())]
    } else {
        candidates[rng.gen_range(0..candidates.len())]
    };

    tone_match_patch(c, base, tone_max_delta, 35.0, 235.0, alpha)
}

pub fn pt(img: &mut RgbaImage, x: i32, y: i32, c: Color) {
    let (w, h) = img.dimensions();
    if x >= 0 && x < w as i32 && y >= 0 && y < h as i32 {
        let x = x as u32;
        let y = y as u32;

        let alpha = c.3 as f32 / 255.0;
        if alpha >= 0.99 {
            // Optimization for opaque pixels
            img.put_pixel(x, y, color_to_rgba(c));
        } else {
            let p_old = img.get_pixel(x, y);
            let inv_alpha = 1.0 - alpha;

            let r = (c.0 as f32 * alpha + p_old[0] as f32 * inv_alpha).round() as u8;
            let g = (c.1 as f32 * alpha + p_old[1] as f32 * inv_alpha).round() as u8;
            let b = (c.2 as f32 * alpha + p_old[2] as f32 * inv_alpha).round() as u8;

            // Standard 'over' operator for alpha
            let a_old = p_old[3] as f32 / 255.0;
            let a_new = alpha + a_old * inv_alpha;
            let a = (a_new * 255.0).round().min(255.0) as u8;

            img.put_pixel(x, y, Rgba([r, g, b, a]));
        }
    }
}

pub fn line(img: &mut RgbaImage, x0: i32, y0: i32, x1: i32, y1: i32, c: Color) {
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let mut x = x0;
    let mut y = y0;

    loop {
        pt(img, x, y, c);
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

pub fn mask_from_ascii(rows: &[&str]) -> RgbaImage {
    let h = rows.len() as u32;
    let w = rows.get(0).map(|r| r.len()).unwrap_or(0) as u32;
    let mut m = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));

    for (y, row) in rows.iter().enumerate() {
        for (x, ch) in row.chars().enumerate() {
            if ch == '#' {
                m.put_pixel(x as u32, y as u32, Rgba([255, 255, 255, 255]));
            }
        }
    }
    m
}

pub fn alpha_composite(base: &RgbaImage, overlay: &RgbaImage) -> RgbaImage {
    let (w, h) = base.dimensions();
    let mut out = base.clone();

    for y in 0..h {
        for x in 0..w {
            let bg = base.get_pixel(x, y);
            let fg = overlay.get_pixel(x, y);

            let alpha_fg = fg[3] as f32 / 255.0;
            let alpha_bg = bg[3] as f32 / 255.0;
            let alpha_out = alpha_fg + alpha_bg * (1.0 - alpha_fg);

            if alpha_out > 0.0 {
                let r = ((fg[0] as f32 * alpha_fg + bg[0] as f32 * alpha_bg * (1.0 - alpha_fg))
                    / alpha_out) as u8;
                let g = ((fg[1] as f32 * alpha_fg + bg[1] as f32 * alpha_bg * (1.0 - alpha_fg))
                    / alpha_out) as u8;
                let b = ((fg[2] as f32 * alpha_fg + bg[2] as f32 * alpha_bg * (1.0 - alpha_fg))
                    / alpha_out) as u8;
                out.put_pixel(x, y, Rgba([r, g, b, (alpha_out * 255.0) as u8]));
            }
        }
    }
    out
}

// Low-level Pixel Text and Fish Drawing
pub fn pixel_text_size(text: &str, scale: i32) -> (i32, i32) {
    if text.is_empty() {
        return (0, 0);
    }
    let glyph_h = 5;
    let mut width = 0;
    for (i, ch) in text.chars().enumerate() {
        if let Some(glyph) = get_glyph(ch) {
            let gw = glyph[0].len() as i32;
            width += gw;
            if i != text.len() - 1 {
                width += 1;
            }
        }
    }
    (width * scale, glyph_h * scale)
}

pub fn draw_pixel_text(img: &mut RgbaImage, x: i32, y: i32, text: &str, color: Color, scale: i32) {
    let mut cx = x;
    for (i, ch) in text.chars().enumerate() {
        if let Some(glyph) = get_glyph(ch) {
            let gw = glyph[0].len() as i32;
            for (gy, row) in glyph.iter().enumerate() {
                for (gx, bit) in row.chars().enumerate() {
                    if bit == '1' {
                        let x0 = cx + gx as i32 * scale;
                        let y0 = y + gy as i32 * scale;
                        if scale == 1 {
                            pt(img, x0, y0, color);
                        } else {
                            draw_rect(img, x0, y0, x0 + scale - 1, y0 + scale - 1, color);
                        }
                    }
                }
            }
            if i != text.len() - 1 {
                cx += (gw + 1) * scale;
            } else {
                cx += gw * scale;
            }
        }
    }
}

pub fn draw_pixel_fish(img: &mut RgbaImage, x: i32, y: i32, scale: i32) {
    let fish_pattern = [
        "..bbb...t",
        ".bbhbb.tt",
        "bbbhhbttt",
        ".bbhbb.tt",
        "..bbb...t",
    ];

    let body = (255, 200, 80, 255);
    let highlight = (255, 225, 120, 255);
    let tail = (255, 165, 70, 255);
    let eye = (20, 20, 20, 255);

    for (yy, row) in fish_pattern.iter().enumerate() {
        for (xx, bit) in row.chars().enumerate() {
            let c = match bit {
                'b' => Some(body),
                'h' => Some(highlight),
                't' => Some(tail),
                _ => None,
            };

            if let Some(col) = c {
                let x0 = x + xx as i32 * scale;
                let y0 = y + yy as i32 * scale;
                if scale == 1 {
                    pt(img, x0, y0, col);
                } else {
                    draw_rect(img, x0, y0, x0 + scale - 1, y0 + scale - 1, col);
                }
            }
        }
    }

    let eye_x = x + 2 * scale;
    let eye_y = y + 1 * scale;
    if scale == 1 {
        pt(img, eye_x, eye_y, eye);
    } else {
        draw_rect(img, eye_x, eye_y, eye_x + scale - 1, eye_y + scale - 1, eye);
    }
}

pub fn get_glyph(ch: char) -> Option<&'static [&'static str]> {
    match ch {
        'B' => Some(&["111", "101", "111", "101", "111"]),
        'U' => Some(&["101", "101", "101", "101", "111"]),
        'Y' => Some(&["101", "101", "010", "010", "010"]),
        'S' => Some(&["111", "100", "111", "001", "111"]),
        'E' => Some(&["111", "100", "111", "100", "111"]),
        'L' => Some(&["100", "100", "100", "100", "111"]),
        '0' => Some(&["111", "101", "101", "101", "111"]),
        '1' => Some(&["010", "110", "010", "010", "111"]),
        '2' => Some(&["111", "001", "111", "100", "111"]),
        '3' => Some(&["111", "001", "111", "001", "111"]),
        '4' => Some(&["101", "101", "111", "001", "001"]),
        '5' => Some(&["111", "100", "111", "001", "111"]),
        '6' => Some(&["111", "100", "111", "101", "111"]),
        '7' => Some(&["111", "001", "001", "010", "010"]),
        '8' => Some(&["111", "101", "111", "101", "111"]),
        '9' => Some(&["111", "101", "111", "001", "111"]),
        '.' => Some(&["00000", "0", "0", "0", "1"]), // Padded implicitly or handled by len
        ' ' => Some(&["0", "0", "0", "0", "0"]),
        _ => None,
    }
}

// Drawing helper functions
pub fn draw_rect(img: &mut RgbaImage, x0: i32, y0: i32, x1: i32, y1: i32, color: Color) {
    for y in y0..=y1 {
        for x in x0..=x1 {
            pt(img, x, y, color);
        }
    }
}

pub fn draw_ellipse(img: &mut RgbaImage, x0: i32, y0: i32, x1: i32, y1: i32, color: Color) {
    let min_x = x0.min(x1);
    let max_x = x0.max(x1);
    let min_y = y0.min(y1);
    let max_y = y0.max(y1);

    let width = (max_x - min_x) as f32 + 1.0;
    let height = (max_y - min_y) as f32 + 1.0;

    let cx = min_x as f32 + width / 2.0;
    let cy = min_y as f32 + height / 2.0;
    let rx = width / 2.0;
    let ry = height / 2.0;

    if rx <= 0.0 || ry <= 0.0 {
        return;
    }

    // Optimization for integer circles (odd width/height) to match draw_ellipse_outline
    // This ensures fill matches outline exactly for the ETH headwear
    let w_i = (max_x - min_x) + 1;
    let h_i = (max_y - min_y) + 1;
    if w_i == h_i && w_i % 2 != 0 {
        let r = w_i / 2;
        let cx = min_x + r;
        let cy = min_y + r;

        if r == 7 {
            // Shrink inner fill to avoid white leaks at diagonal outline corners
            let spans = [7, 7, 7, 6, 6, 4, 3, 1];
            for (y_off, &x_span) in spans.iter().enumerate() {
                let y = y_off as i32;
                let x = x_span as i32;
                for dx in -x..=x {
                    pt(img, cx + dx, cy + y, color);
                    pt(img, cx + dx, cy - y, color);
                }
            }
            return;
        }

        if r == 8 {
            // Shrink fill slightly to ensure it stays strictly inside the outline (User-verified fix)
            let spans = [7, 7, 7, 6, 6, 5, 4, 3, 1];
            for (y_off, &x_span) in spans.iter().enumerate() {
                let y = y_off as i32;
                let x = x_span as i32;
                for dx in -x..=x {
                    pt(img, cx + dx, cy + y, color);
                    pt(img, cx + dx, cy - y, color);
                }
            }
            return;
        }

        // Bresenham Scanline Fill for perfect match with outline (fallback)
        let mut x = 0;
        let mut y = r;
        let mut d = 3 - 2 * r;

        let mut draw_line = |y_offset: i32, x_limit: i32| {
            for dx in -x_limit..=x_limit {
                pt(img, cx + dx, cy + y_offset, color);
            }
        };

        while y >= x {
            // Draw horizontal spans for the current octant symmetries
            draw_line(y, x);
            draw_line(-y, x);
            draw_line(x, y);
            draw_line(-x, y);

            x += 1;
            if d > 0 {
                y -= 1;
                d = d + 4 * (x - y) + 10;
            } else {
                d = d + 4 * x + 6;
            }
        }
        return;
    }

    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let dx = (x as f32 + 0.5) - cx;
            let dy = (y as f32 + 0.5) - cy;
            // Ellipse equation: (dx/rx)^2 + (dy/ry)^2 <= 1
            if (dx / rx).powi(2) + (dy / ry).powi(2) <= 1.0 {
                pt(img, x, y, color);
            }
        }
    }
}

pub fn draw_circle(img: &mut RgbaImage, cx: i32, cy: i32, radius: i32, color: Color) {
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            if dx * dx + dy * dy <= radius * radius {
                pt(img, cx + dx, cy + dy, color);
            }
        }
    }
}

pub fn draw_triangle(
    img: &mut RgbaImage,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    x2: i32,
    y2: i32,
    color: Color,
) {
    let mut points = vec![(x0, y0), (x1, y1), (x2, y2)];
    points.sort_by_key(|p| p.1);

    let (x0, y0) = points[0];
    let (x1, y1) = points[1];
    let (x2, y2) = points[2];

    for y in y0..=y2 {
        let (x_start, x_end) = if y <= y1 {
            let t = if y1 == y0 {
                0.0
            } else {
                (y - y0) as f32 / (y1 - y0) as f32
            };
            let xa = x0 + ((x1 - x0) as f32 * t) as i32;
            let xb = x0 + ((x2 - x0) as f32 * ((y - y0) as f32 / (y2 - y0).max(1) as f32)) as i32;
            (xa.min(xb), xa.max(xb))
        } else {
            let t = if y2 == y1 {
                0.0
            } else {
                (y - y1) as f32 / (y2 - y1) as f32
            };
            let xa = x1 + ((x2 - x1) as f32 * t) as i32;
            let xb = x0 + ((x2 - x0) as f32 * ((y - y0) as f32 / (y2 - y0).max(1) as f32)) as i32;
            (xa.min(xb), xa.max(xb))
        };

        for x in x_start..=x_end {
            pt(img, x, y, color);
        }
    }
}

pub fn draw_ellipse_outline(img: &mut RgbaImage, x0: i32, y0: i32, x1: i32, y1: i32, color: Color) {
    let min_x = x0.min(x1);
    let max_x = x0.max(x1);
    let min_y = y0.min(y1);
    let max_y = y0.max(y1);

    let width = (max_x - min_x) + 1;
    let height = (max_y - min_y) + 1;
    let rx = width / 2;
    let ry = height / 2;
    let cx = min_x + rx;
    let cy = min_y + ry;

    if rx < 0 || ry < 0 {
        return;
    }
    if width <= 2 || height <= 2 {
        draw_ellipse(img, x0, y0, x1, y1, color);
        return;
    }

    if rx == ry && width == height {
        // PIL-matching lookup for radius 8 (ETH Coin)
        if rx == 8 {
            let spans = [8, 8, 8, 7, 7, 6, 5, 4, 2];
            let mut put = |px, py| pt(img, px, py, color);

            for y in 0..spans.len() {
                let curr_x = spans[y];
                // Determine how far inwards we need to draw to connect to the next row visually
                let next_x = if y + 1 < spans.len() {
                    spans[y + 1]
                } else {
                    curr_x
                };

                // If next row jumps in (e.g. 4 -> 2), draw intermediate pixels (e.g. 4 and 3)
                // For last row, just draw itself.
                let lower_bound = next_x + 1;
                let draw_start = if lower_bound > curr_x {
                    curr_x
                } else {
                    lower_bound
                };

                for x_fill in draw_start..=curr_x {
                    let y = y as i32;
                    let x = x_fill as i32;

                    put(cx - x, cy - y);
                    put(cx + x, cy - y);
                    put(cx - x, cy + y);
                    put(cx + x, cy + y);
                }
            }
            return;
        }

        // Bresenham's Circle Algorithm for other sizes
        let r = rx;
        let mut x = 0;
        let mut y = r;
        let mut d = 3 - 2 * r;
        // ... (rest of function)
        let mut put = |x: i32, y: i32| {
            pt(img, x, y, color);
        };

        while y >= x {
            put(cx + x, cy + y);
            put(cx - x, cy + y);
            put(cx + x, cy - y);
            put(cx - x, cy - y);
            put(cx + y, cy + x);
            put(cx - y, cy + x);
            put(cx + y, cy - x);
            put(cx - y, cy - x);

            x += 1;
            if d > 0 {
                y -= 1;
                d = d + 4 * (x - y) + 10;
            } else {
                d = d + 4 * x + 6;
            }
        }
    } else {
        // Float fallback for ellipse outline
        let rx_f = (width as f32) / 2.0;
        let ry_f = (height as f32) / 2.0;
        let cx_f = min_x as f32 + rx_f;
        let cy_f = min_y as f32 + ry_f;

        // Limits based on r+0.5
        let rx_out = rx_f + 0.5;
        let ry_out = ry_f + 0.5;
        // Inner radius for outline 1px thickness
        let rx_in = (rx_f - 0.5).max(0.0);
        let ry_in = (ry_f - 0.5).max(0.0);

        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let dx = (x as f32 + 0.5) - cx_f;
                let dy = (y as f32 + 0.5) - cy_f;

                let dist_out = (dx / rx_out).powi(2) + (dy / ry_out).powi(2);
                if dist_out <= 1.0 {
                    let dist_in = if rx_in > 0.0 && ry_in > 0.0 {
                        (dx / rx_in).powi(2) + (dy / ry_in).powi(2)
                    } else {
                        2.0
                    };

                    if dist_in > 1.0 {
                        pt(img, x, y, color);
                    }
                }
            }
        }
    }
}

pub fn draw_polygon(img: &mut RgbaImage, points: &[(i32, i32)], color: Color) {
    if points.len() < 3 {
        return;
    }

    // PIL-style polygon filling:
    // 1. No wireframe outline (scanline only).
    // 2. Scanline logic that matches PIL rasterization.

    let min_y = points.iter().map(|p| p.1).min().unwrap();
    let max_y = points.iter().map(|p| p.1).max().unwrap();

    for y in min_y..=max_y {
        let mut intersections = Vec::new();

        for i in 0..points.len() {
            let (x0, y0) = points[i];
            let (x1, y1) = points[(i + 1) % points.len()];

            // Check for intersection with scanline y
            // We consider the edge to span [min_y, max_y).
            // But we must handle the case where a vertex lies exactly on y.

            let (py0, py1, px0, px1) = if y0 < y1 {
                (y0, y1, x0, x1)
            } else {
                (y1, y0, x1, x0)
            };

            // Standard scanline rule: Include top y, exclude bottom y.
            // Helps avoid double counting.
            // Exception: Horizontal lines? Handled by not intersecting (py0 == py1).
            if y >= py0 && y < py1 {
                let t = (y - py0) as f32 / (py1 - py0) as f32;
                let x = px0 as f32 + (px1 - px0) as f32 * t;
                intersections.push(x);
            }
        }

        intersections.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        for chunk in intersections.chunks(2) {
            if chunk.len() == 2 {
                let x_start_f = chunk[0];
                let x_end_f = chunk[1];

                let x_start = x_start_f.round() as i32;
                let mut x_end = x_end_f.round() as i32;

                // PIL behavior heuristic:
                // If intersection is close to .0 (integer), it implies inclusivity of that edge pixel.
                // Especially for the right edge.
                if (x_end_f - x_end_f.round()).abs() < 0.001 {
                    x_end += 1;
                }

                // For left edge, standard round seems sufficient (1.5 -> 2, 1.0 -> 1).

                // Special case handling for single point tip
                if x_start == x_end && (x_start_f - x_end_f).abs() < 0.001 {
                    // Should we draw 1 pixel?
                    // (2.0, 2.0) -> 2..3 -> yes.
                    // (2.0) -> round 2 -> end 3 -> 2..3. Yes.
                }

                if x_start < x_end {
                    for x in x_start..x_end {
                        pt(img, x, y, color);
                    }
                }
            }
        }
    }
}

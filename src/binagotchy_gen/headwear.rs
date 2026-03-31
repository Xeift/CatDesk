// Complete headwear rendering module
use super::binagotchy_utils::*;
use super::constants::*;
use super::types::*;
use image::RgbaImage;
use rand::Rng;

pub fn render_headwear_layer<R: Rng>(
    canvas: u32,
    head_box: (i32, i32, i32, i32),
    rng: &mut R,
    headwear: &str,
    accent: Color,
) -> RgbaImage {
    let mut headwear_layer =
        RgbaImage::from_pixel(canvas, canvas + HEADROOM_TOP as u32, rgba(0, 0, 0, 0));

    let head_box_hw = (
        head_box.0,
        head_box.1 + HEADROOM_TOP,
        head_box.2,
        head_box.3 + HEADROOM_TOP,
    );

    draw_headwear(
        &mut headwear_layer,
        head_box_hw,
        rng,
        headwear,
        accent,
        canvas,
    );

    headwear_layer
}

fn draw_headwear<R: Rng>(
    img: &mut RgbaImage,
    head_box: (i32, i32, i32, i32),
    mut _rng: &mut R,
    headwear: &str,
    accent: Color,
    canvas: u32,
) {
    if headwear == "none" {
        return;
    }

    let (w, h) = (img.width() as i32, img.height() as i32);
    let scale = scale_factor(canvas as i32);
    let (x0, y0, x1, _) = head_box;
    let cx = super::character::face_center_x(canvas);

    match headwear {
        "crown" => draw_crown(img, cx, y0, scale),
        "halo" => draw_halo(img, cx, y0, scale),
        "bucket_hat" => draw_bucket_hat(img, x0, x1, y0, scale),
        "top_hat" => draw_top_hat(img, x0, x1, y0, scale),
        "tiara" => draw_tiara(img, cx, y0, scale),
        "flower_crown" => draw_flower_crown(img, cx, y0, scale),
        "sprout" => draw_sprout(img, cx, y0, w, h, scale),
        "apple" => draw_apple(img, cx, y0, w, h, scale),
        "antenna" => draw_antenna(img, cx, y0, w, h, accent, scale),
        "ice_cream" => draw_ice_cream(img, cx, y0, scale),
        "bubble_milk_tea" => draw_bubble_milk_tea(img, cx, y0, w, h, scale),
        "temple_cap" => draw_temple_cap(img, cx, y0, scale),
        _ => {}
    }
}

fn draw_crown(img: &mut RgbaImage, cx: i32, y0: i32, scale: f32) {
    // Rich gold colors
    let gold = (245, 210, 90, 255);
    let gold_dark = (210, 175, 65, 255);
    let gold_deep = (180, 145, 45, 255);

    // Colorful gems
    let ruby = (220, 50, 70, 255);
    let sapphire = (50, 100, 220, 255);

    let base_y = (y0 - s(3, scale)).max(0); // Moved up to accommodate taller band
    let band_h = s(3, scale).max(3); // Taller band for more presence

    // Draw band
    draw_rect(
        img,
        cx - s(7, scale),
        base_y,
        cx + s(7, scale),
        base_y + band_h,
        gold,
    );
    draw_rect(
        img,
        cx - s(7, scale),
        base_y + band_h - s(1, scale),
        cx + s(7, scale),
        base_y + band_h,
        gold_dark,
    );

    // Small decorative line on band
    let band_mid_y = base_y + band_h / 2;
    for x in -s(6, scale)..=s(6, scale) {
        if x % s(2, scale) == 0 {
            pt(img, cx + x, band_mid_y, gold_deep);
        }
    }

    // Draw 3 SHORT triangular spikes on the TALL base
    let spikes = [
        (-s(5, scale), s(3, scale), ruby), // Left - SHORT spike
        (0, s(4, scale), sapphire),        // Center - slightly taller but still short
        (s(5, scale), s(3, scale), ruby),  // Right - SHORT spike
    ];

    for (dx, height, gem_color) in spikes.iter() {
        // Draw spike
        draw_triangle(
            img,
            cx + dx - s(2, scale),
            base_y,
            cx + dx + s(2, scale),
            base_y,
            cx + dx,
            base_y - height,
            gold,
        );

        // Add gem at tip - make it bigger and more visible
        let gem_size = s(2, scale).max(2);
        for gy in 0..gem_size {
            for gx in 0..gem_size {
                let gem_x = cx + dx - gem_size / 2 + gx;
                let gem_y = base_y - height + s(1, scale) + gy;
                if gx == 0 && gy == 0 {
                    pt(img, gem_x, gem_y, lighten(*gem_color, 100));
                } else {
                    pt(img, gem_x, gem_y, *gem_color);
                }
            }
        }
    }
}

fn draw_halo(img: &mut RgbaImage, cx: i32, y0: i32, scale: f32) {
    let ring = (255, 235, 160, 200);
    // Python uses y0 - 9*scale for center y
    let cy = y0 - s(9, scale);
    let rx = s(6, scale);
    let ry = s(2, scale);

    draw_ellipse_outline(img, cx - rx, cy - ry, cx + rx, cy + ry, ring);
}

fn draw_bucket_hat(img: &mut RgbaImage, x0: i32, x1: i32, y0: i32, scale: f32) {
    let offset_x = -s(3, scale);
    let offset_y = -s(6, scale);
    let hx0 = x0 + offset_x;
    let hx1 = x1 + offset_x;
    let hy0 = y0 + offset_y;
    let brim_y = hy0 + s(1, scale);

    let hat_base = (120, 140, 110, 255);
    let brim = (90, 110, 85, 255);

    draw_rect(
        img,
        hx0 - s(1, scale),
        brim_y,
        hx1 + s(1, scale),
        brim_y + s(2, scale),
        brim,
    );

    let top_y = (hy0 - s(4, scale)).max(0);
    draw_rect(
        img,
        hx0 + s(2, scale),
        top_y,
        hx1 - s(2, scale),
        brim_y,
        hat_base,
    );
}

fn draw_top_hat(img: &mut RgbaImage, x0: i32, x1: i32, y0: i32, scale: f32) {
    let offset_x = -s(3, scale);
    let offset_y = -s(7, scale);
    let hx0 = x0 + offset_x;
    let hx1 = x1 + offset_x;
    let hy0 = y0 + offset_y;
    let brim_y = hy0 + s(1, scale);

    let hat_black = (20, 20, 20, 255);
    let hat_shadow = (10, 10, 10, 255);
    let hat_highlight = (35, 35, 35, 255);
    let hat_band = (45, 45, 45, 255);
    let hat_band_highlight = (55, 55, 55, 255);

    draw_rect(
        img,
        hx0 - s(1, scale),
        brim_y,
        hx1 + s(1, scale),
        brim_y + s(2, scale),
        hat_shadow,
    );

    let top_y = (hy0 - s(10, scale)).max(0);
    draw_rect(
        img,
        hx0 + s(3, scale),
        top_y,
        hx1 - s(3, scale),
        brim_y,
        hat_black,
    );
    draw_rect(
        img,
        hx0 + s(3, scale),
        top_y,
        hx0 + s(4, scale),
        brim_y,
        hat_highlight,
    );
    draw_rect(
        img,
        hx1 - s(4, scale),
        top_y,
        hx1 - s(3, scale),
        brim_y,
        hat_shadow,
    );
    draw_rect(
        img,
        hx0 + s(3, scale),
        brim_y - s(3, scale),
        hx1 - s(3, scale),
        brim_y - s(1, scale),
        hat_band,
    );
    draw_rect(
        img,
        hx0 + s(3, scale),
        brim_y - s(3, scale),
        hx0 + s(4, scale),
        brim_y - s(1, scale),
        hat_band_highlight,
    );
}

fn draw_tiara(img: &mut RgbaImage, cx: i32, y0: i32, scale: f32) {
    let silver = (220, 230, 240, 255);
    let gem = (120, 180, 255, 255);

    let base_y = (y0 - s(1, scale) - s(3, scale)).max(0);
    draw_rect(
        img,
        cx - s(6, scale),
        base_y,
        cx + s(6, scale),
        base_y + s(1, scale),
        silver,
    );

    let peaks = [
        (-s(4, scale), s(2, scale)),
        (0, s(3, scale)),
        (s(4, scale), s(2, scale)),
    ];

    for (dx, height) in peaks.iter() {
        draw_triangle(
            img,
            cx + dx - s(1, scale),
            base_y,
            cx + dx + s(1, scale),
            base_y,
            cx + dx,
            base_y - height,
            silver,
        );
        pt(img, cx + dx, base_y - height, gem);
    }
}

fn draw_flower_crown(img: &mut RgbaImage, cx: i32, y0: i32, scale: f32) {
    let petal = (245, 170, 200, 255);
    let center = (240, 220, 120, 255);
    let row_y = (y0 - s(2, scale)).max(0);

    for dx in [-s(5, scale), 0, s(5, scale)] {
        draw_ellipse(
            img,
            cx + dx - s(2, scale),
            row_y - s(1, scale),
            cx + dx + s(2, scale),
            row_y + s(1, scale),
            petal,
        );
        pt(img, cx + dx, row_y, center);
    }
}

fn draw_sprout(img: &mut RgbaImage, cx: i32, y0: i32, _w: i32, _h: i32, scale: f32) {
    let stem = (85, 150, 90, 255);
    let leaf = (120, 210, 140, 255);
    let leaf_shadow = (95, 175, 115, 255);
    let leaf_highlight = (165, 235, 175, 255);

    let base_y = y0 - s(1, scale);
    let stem_h = s(4, scale).max(3);
    let tip_y = base_y - stem_h;

    line(img, cx, base_y, cx, tip_y, stem);

    let leaf_dx = s(4, scale).max(3);
    let leaf_rx = s(2, scale).max(1);
    let leaf_ry = s(2, scale).max(1);

    let left_y = tip_y - s(1, scale);
    let right_y = tip_y + s(1, scale);
    let left_cx = cx - leaf_dx;
    let right_cx = cx + leaf_dx;

    line(img, cx, tip_y, left_cx, left_y, stem);
    line(img, cx, tip_y, right_cx, right_y, stem);

    // Draw base leaves
    draw_ellipse(
        img,
        left_cx - leaf_rx,
        left_y - leaf_ry,
        left_cx + leaf_rx,
        left_y + leaf_ry,
        leaf,
    );
    draw_ellipse(
        img,
        right_cx - leaf_rx,
        right_y - leaf_ry,
        right_cx + leaf_rx,
        right_y + leaf_ry,
        leaf,
    );

    // Draw leaf highlights
    draw_ellipse(
        img,
        left_cx - leaf_rx + s(1, scale),
        left_y - leaf_ry + s(1, scale),
        left_cx + leaf_rx - s(1, scale),
        left_y + leaf_ry,
        leaf_highlight,
    );
    draw_ellipse(
        img,
        right_cx - leaf_rx + s(1, scale),
        right_y - leaf_ry + s(1, scale),
        right_cx + leaf_rx - s(1, scale),
        right_y + leaf_ry,
        leaf_highlight,
    );

    // Draw shadows
    draw_ellipse(
        img,
        left_cx - leaf_rx,
        left_y - leaf_ry + s(1, scale),
        left_cx + leaf_rx - s(1, scale),
        left_y + leaf_ry - s(1, scale),
        leaf_shadow,
    );
    draw_ellipse(
        img,
        right_cx - leaf_rx,
        right_y - leaf_ry + s(1, scale),
        right_cx + leaf_rx - s(1, scale),
        right_y + leaf_ry - s(1, scale),
        leaf_shadow,
    );

    pt(img, cx, tip_y, lighten(leaf, 30));
}

fn draw_apple(img: &mut RgbaImage, cx: i32, y0: i32, _w: i32, _h: i32, scale: f32) {
    let apple = (210, 65, 70, 255);
    let apple_shadow = (170, 45, 55, 255);
    let apple_highlight = (235, 120, 120, 255);
    let stem_color = (90, 70, 50, 255);
    let leaf = (120, 200, 120, 255);

    let center_y = y0 - s(6, scale);
    let lobe_dx = s(3, scale).max(2);
    let lobe_rx = s(3, scale).max(2);
    let lobe_ry = s(4, scale).max(3);
    let bottom_rx = s(4, scale).max(3);
    let bottom_ry = s(6, scale).max(4);

    // Draw three lobes for apple shape
    draw_ellipse(
        img,
        cx - lobe_dx - lobe_rx,
        center_y - lobe_ry,
        cx - lobe_dx + lobe_rx,
        center_y + lobe_ry,
        apple,
    );
    draw_ellipse(
        img,
        cx + lobe_dx - lobe_rx,
        center_y - lobe_ry,
        cx + lobe_dx + lobe_rx,
        center_y + lobe_ry,
        apple,
    );
    draw_ellipse(
        img,
        cx - bottom_rx,
        center_y - lobe_ry + s(3, scale),
        cx + bottom_rx,
        center_y + bottom_ry,
        apple,
    );

    // Add highlights and shadows
    pt(img, cx, center_y - lobe_ry + s(1, scale), apple_shadow);
    draw_ellipse(
        img,
        cx - lobe_dx - s(1, scale),
        center_y - lobe_ry + s(1, scale),
        cx - lobe_dx + s(1, scale),
        center_y - lobe_ry + s(3, scale),
        apple_highlight,
    );
    draw_ellipse(
        img,
        cx + s(1, scale),
        center_y + s(2, scale),
        cx + bottom_rx,
        center_y + bottom_ry - s(1, scale),
        apple_shadow,
    );

    let apple_top = center_y - lobe_ry;
    let stem_top = apple_top - s(1, scale);

    let leaf_rx = s(3, scale).max(2);
    let leaf_ry = s(2, scale).max(1);
    let leaf_cx = cx + s(2, scale);
    let leaf_cy = apple_top - s(1, scale);

    // Python implementation: create leaf with shift effect (lines 864-877)
    let leaf_w = leaf_rx * 2 + 1;
    let leaf_h = leaf_ry * 2 + 1;

    // Step 1: Draw base ellipse (PROPER ellipse formula)
    let mut leaf_img: Vec<Option<Color>> = vec![None; (leaf_w * leaf_h) as usize];
    for yy in 0..leaf_h {
        for xx in 0..leaf_w {
            let dx = xx - leaf_rx;
            let dy = yy - leaf_ry;
            // Proper ellipse equation: (dx/rx)^2 + (dy/ry)^2 <= 1
            // In integer form: dx^2 * ry^2 + dy^2 * rx^2 <= rx^2 * ry^2
            if dx * dx * leaf_ry * leaf_ry + dy * dy * leaf_rx * leaf_rx
                <= leaf_rx * leaf_rx * leaf_ry * leaf_ry
            {
                leaf_img[(yy * leaf_w + xx) as usize] = Some(leaf);
            }
        }
    }

    // Step 2: Draw inner highlight ellipse (1,1 to w-1,h-1)
    for yy in 1..leaf_h {
        for xx in 1..leaf_w {
            if xx >= leaf_w - 1 || yy >= leaf_h - 1 {
                continue;
            }
            let dx = (xx - leaf_rx) as f32;
            let dy = (yy - leaf_ry) as f32;
            let rx_inner = (leaf_rx - 1) as f32;
            let ry_inner = (leaf_ry - 1) as f32;
            if rx_inner > 0.0
                && ry_inner > 0.0
                && (dx * dx) / (rx_inner * rx_inner) + (dy * dy) / (ry_inner * ry_inner) <= 1.0
            {
                leaf_img[(yy * leaf_w + xx) as usize] = Some(lighten(leaf, 25));
            }
        }
    }

    // Step 3: Apply shift - paste with shift effect
    for yy in 0..leaf_h {
        for xx in 0..leaf_w {
            if let Some(color) = leaf_img[(yy * leaf_w + xx) as usize] {
                let ny = if xx == 0 && yy + 1 < leaf_h {
                    yy + 1
                } else {
                    yy
                };
                let world_x = leaf_cx - leaf_rx + xx;
                let world_y = leaf_cy - leaf_ry + ny;
                pt(img, world_x, world_y, color);
            }
        }
    }

    // Draw stem and connectors (Python lines 880-883)
    line(img, cx, apple_top, cx, stem_top, stem_color);
    let connector_x = leaf_cx - s(1, scale);
    line(img, cx, stem_top, connector_x, leaf_cy, stem_color);
    line(
        img,
        leaf_cx - s(1, scale),
        leaf_cy,
        leaf_cx + s(1, scale),
        leaf_cy - s(1, scale),
        stem_color,
    );
}

fn draw_antenna(
    img: &mut RgbaImage,
    cx: i32,
    y0: i32,
    _w: i32,
    _h: i32,
    accent: Color,
    scale: f32,
) {
    let stalk_color = darken(accent, 20);
    let top_y = (y0 - s(7, scale)).max(0);

    line(img, cx, y0 - s(2, scale), cx, top_y, stalk_color);
    draw_ellipse(
        img,
        cx - s(1, scale),
        top_y - s(1, scale),
        cx + s(1, scale),
        top_y + s(1, scale),
        lighten(accent, 35),
    );
}

fn draw_ice_cream(img: &mut RgbaImage, cx: i32, y0: i32, scale: f32) {
    let cone_base = (210, 160, 110, 255);
    let cone_dark = (180, 135, 95, 255);
    let cream1 = (255, 182, 193, 255);
    let cream1_shadow = (240, 150, 165, 255);
    let cream1_highlight = (255, 220, 230, 255);
    let cream2 = (255, 240, 200, 255);
    let cream2_shadow = (235, 210, 170, 255);
    let cream2_highlight = (255, 250, 230, 255);

    let cone_width = s(3, scale);
    let cone_height = s(9, scale);
    let cone_top_y = (y0 - s(20, scale)).max(0);
    let cone_bottom_y = cone_top_y + cone_height;
    let tilt_offset = s(1, scale);

    // Draw cone
    draw_triangle(
        img,
        cx + tilt_offset,
        cone_top_y,
        cx - cone_width,
        cone_bottom_y,
        cx + cone_width,
        cone_bottom_y,
        cone_base,
    );

    // Add waffle pattern
    for i in 1..5 {
        let y = cone_top_y + i * s(2, scale);
        if y < cone_bottom_y - 1 {
            let progress = (y - cone_top_y) as f32 / cone_height.max(1) as f32;
            let half_width = (cone_width as f32 * progress) as i32;
            let x_center = cx + (tilt_offset as f32 * (1.0 - progress)) as i32;

            if half_width > 1 {
                line(
                    img,
                    x_center - half_width,
                    y,
                    x_center + half_width,
                    y,
                    cone_dark,
                );

                if i % 2 == 0 && half_width > 2 {
                    let y_next = y + s(1, scale);
                    line(
                        img,
                        x_center - half_width + 1,
                        y,
                        x_center,
                        y_next,
                        cone_dark,
                    );
                    line(
                        img,
                        x_center,
                        y,
                        x_center + half_width - 1,
                        y_next,
                        cone_dark,
                    );
                }
            }
        }
    }

    // Draw scoops
    let scoop1_rx = s(3, scale);
    let scoop1_ry = s(3, scale);
    let scoop1_cy = cone_bottom_y + scoop1_ry - s(1, scale);

    draw_ellipse(
        img,
        cx - scoop1_rx,
        scoop1_cy - scoop1_ry,
        cx + scoop1_rx,
        scoop1_cy + scoop1_ry,
        cream1,
    );
    draw_ellipse(
        img,
        cx - scoop1_rx + s(1, scale),
        scoop1_cy - scoop1_ry + s(1, scale),
        cx - s(1, scale),
        scoop1_cy - scoop1_ry + s(3, scale),
        cream1_highlight,
    );
    draw_ellipse(
        img,
        cx + s(1, scale),
        scoop1_cy + s(1, scale),
        cx + scoop1_rx - s(1, scale),
        scoop1_cy + scoop1_ry - s(1, scale),
        cream1_shadow,
    );

    let scoop2_rx = s(3, scale);
    let scoop2_ry = s(3, scale);
    let scoop2_cy = scoop1_cy + scoop1_ry + scoop2_ry - s(2, scale);

    draw_ellipse(
        img,
        cx - scoop2_rx,
        scoop2_cy - scoop2_ry,
        cx + scoop2_rx,
        scoop2_cy + scoop2_ry,
        cream2,
    );
    draw_ellipse(
        img,
        cx - scoop2_rx + s(1, scale),
        scoop2_cy - scoop2_ry + s(1, scale),
        cx - s(1, scale),
        scoop2_cy - scoop2_ry + s(3, scale),
        cream2_highlight,
    );
    draw_ellipse(
        img,
        cx + s(1, scale),
        scoop2_cy + s(1, scale),
        cx + scoop2_rx - s(1, scale),
        scoop2_cy + scoop2_ry - s(1, scale),
        cream2_shadow,
    );
}

fn draw_bubble_milk_tea(img: &mut RgbaImage, cx: i32, y0: i32, _w: i32, _h: i32, scale: f32) {
    let cup = (225, 190, 140, 255);
    let cup_shadow = (195, 155, 115, 255);
    let cup_highlight = (240, 210, 170, 255);
    let lid = (230, 225, 220, 255);
    let straw = (115, 180, 190, 255);
    let boba = (70, 55, 45, 255);
    let boba_highlight = (95, 75, 65, 255);
    let boba_shadow = darken(boba, 12);

    let cup_h = s(10, scale).max(8);
    let top_half = s(6, scale).max(5);
    let bottom_half = s(5, scale).max(5);

    let cup_bottom_y = (y0 - s(1, scale)).max(0);
    let cup_top_y = (cup_bottom_y - cup_h).max(0);

    // Draw cup as trapezoid
    draw_polygon(
        img,
        &[
            (cx - bottom_half, cup_bottom_y),
            (cx + bottom_half, cup_bottom_y),
            (cx + top_half, cup_top_y),
            (cx - top_half, cup_top_y),
        ],
        cup,
    );

    // Add highlight
    let highlight_w = s(1, scale).max(1);
    let highlight_x = cx - top_half + s(1, scale);
    draw_rect(
        img,
        highlight_x,
        cup_top_y + s(1, scale),
        highlight_x + highlight_w,
        cup_bottom_y - s(2, scale),
        cup_highlight,
    );

    // Add bottom shadow
    draw_rect(
        img,
        cx - bottom_half + s(1, scale),
        cup_bottom_y - s(2, scale),
        cx + bottom_half - s(1, scale),
        cup_bottom_y - s(1, scale),
        cup_shadow,
    );

    // Draw boba pearls
    let pearl_r = s(1, scale).max(1);
    let pearl_row_y = cup_bottom_y - s(2, scale).max(1);
    let pearl_spacing = s(4, scale).max(4).min(bottom_half - pearl_r);
    let mid_spacing = (pearl_r + 1).max(pearl_spacing / 2).max(2);

    let pearl_offsets = [
        (-pearl_spacing, 0),
        (-mid_spacing, s(1, scale)),
        (0, s(1, scale) - s(2, scale)),
        (pearl_spacing, 0),
    ];

    for (dx, dy) in pearl_offsets.iter() {
        let px = cx + dx;
        let py = pearl_row_y + dy;

        if pearl_r <= 1 {
            draw_rect(img, px - 1, py - 1, px, py, boba);
            pt(img, px - 1, py - 1, boba_highlight);
            pt(img, px, py, boba_shadow);
        } else {
            draw_ellipse(
                img,
                px - pearl_r,
                py - pearl_r,
                px + pearl_r,
                py + pearl_r,
                boba,
            );
            let hl = pearl_r.min(s(1, scale).max(1));
            draw_ellipse(
                img,
                px - pearl_r + 1,
                py - pearl_r + 1,
                px - pearl_r + hl,
                py - pearl_r + hl,
                boba_highlight,
            );
            pt(img, px + pearl_r - 1, py + pearl_r - 1, boba_shadow);
        }
    }

    // Draw lid
    draw_rect(
        img,
        cx - top_half - s(1, scale),
        cup_top_y - s(1, scale),
        cx + top_half + s(1, scale),
        cup_top_y,
        lid,
    );

    // Draw straw
    let straw_base_x = cx + s(2, scale);
    let straw_base_y = cup_top_y - s(1, scale);
    let straw_top_x = straw_base_x + s(1, scale);
    let straw_top_y = (straw_base_y - s(6, scale)).max(0);
    line(
        img,
        straw_base_x,
        straw_base_y,
        straw_top_x,
        straw_top_y,
        straw,
    );
}

fn draw_temple_cap(img: &mut RgbaImage, cx: i32, y0: i32, scale: f32) {
    let cap_base = (25, 45, 95, 255);
    let cap_text = (240, 210, 80, 255);
    let outline = (0, 0, 0, 255);

    // Temple cap grid pattern
    let cap_grid = vec![
        "...............................",
        "...............................",
        "...............................",
        ".................KK............",
        ".............KKKKKKKK..........",
        "...........KKBBBBBBBBKK........",
        "..........KBBBBBBBBBBBBKK......",
        ".........KBBBYYBBBBYYYBBBK.....",
        ".........KBBBYYYBBBBYYBBBK.....",
        ".........KBBBBYYBBBYYBBBBBK....",
        "........KBYBYBBBYBYBBBYYBBK....",
        "........KBBYYBBBBBYBBBBYBBK....",
        "........KBYYBBBBYYYBBBBYYBK....",
        "........KKKKKKBBBBBBBBBBBBK....",
        "......KKBBBBBBKKKKBBBBBBBK.....",
        ".....KBBBBBBBBBBBBKKKKBBBK.....",
        "....KKKKKBBBBBBBBBBBK.KKK......",
        "....KBBK.KBBBBBBBBBK...........",
        ".....KK...KBBBBBBBK............",
        "...........KKBB BKK.............",
        ".............KKK...............",
        "...............................",
        "...............................",
        "...............................",
    ];

    let gw = cap_grid[0].len() as i32;
    let gh = cap_grid.len() as i32;
    let scale_px = s(1, scale).max(1);

    // Calculate final position matching Python exactly
    let cap_w = gw * scale_px;
    let cap_h = gh * scale_px;
    let cap_left = cx - cap_w / 2 - s(2, scale) + s(2, scale); // Move left 1px (back to original)
    let cap_bottom = y0 - s(1, scale) + s(4, scale) + 1; // Move up 1px (from +2 back to +1)
    let cap_top = cap_bottom - cap_h + 1;

    for (gy, row) in cap_grid.iter().enumerate() {
        for (gx, ch) in row.chars().enumerate() {
            let color = match ch {
                'K' => Some(outline),
                'B' => {
                    // Add texture
                    let tex_light = lighten(cap_base, 14);
                    let tex_dark = darken(cap_base, 14);
                    if (gx as i32 + gy as i32) % 5 == 0 {
                        Some(tex_light)
                    } else if (gx as i32 + gy as i32) % 5 == 3 {
                        Some(tex_dark)
                    } else {
                        Some(cap_base)
                    }
                }
                'Y' => {
                    let text_light = lighten(cap_text, 12);
                    let text_dark = darken(cap_text, 18);
                    if (gx as i32 + gy as i32 * 2) % 5 == 0 {
                        Some(text_light)
                    } else if (gx as i32 + gy as i32 * 2) % 5 == 2 {
                        Some(text_dark)
                    } else {
                        Some(cap_text)
                    }
                }
                _ => None,
            };

            if let Some(c) = color {
                for dy in 0..scale_px {
                    for dx in 0..scale_px {
                        let px = cap_left + gx as i32 * scale_px + dx;
                        let py = cap_top + gy as i32 * scale_px + dy;
                        pt(img, px, py, c);
                    }
                }
            }
        }
    }
}

// Helper functions
fn draw_triangle(
    img: &mut RgbaImage,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    x2: i32,
    y2: i32,
    color: Color,
) {
    // Simple scanline triangle fill
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

fn draw_ellipse_outline(img: &mut RgbaImage, x0: i32, y0: i32, x1: i32, y1: i32, color: Color) {
    let cx = (x0 + x1) / 2;
    let cy = (y0 + y1) / 2;
    let rx = ((x1 - x0) / 2).abs();
    let ry = ((y1 - y0) / 2).abs();

    if rx == 0 && ry == 0 {
        pt(img, cx, cy, color);
        return;
    }

    // Bresenham-style ellipse algorithm for connected outline
    // This ensures all pixels are connected like PIL's outline parameter
    let mut x = 0;
    let mut y = ry;

    let rx2 = rx * rx;
    let ry2 = ry * ry;

    // Draw 4-way symmetric points
    let mut plot4 = |x: i32, y: i32| {
        pt(img, cx + x, cy + y, color);
        pt(img, cx - x, cy + y, color);
        pt(img, cx + x, cy - y, color);
        pt(img, cx - x, cy - y, color);
    };

    // Region 1: slope < -1
    let mut d1 = ry2 - (rx2 * ry) + (rx2 / 4);
    let mut dx = 2 * ry2 * x;
    let mut dy = 2 * rx2 * y;

    while dx < dy {
        plot4(x, y);
        x += 1;
        dx += 2 * ry2;

        if d1 < 0 {
            d1 += dx + ry2;
        } else {
            y -= 1;
            dy -= 2 * rx2;
            d1 += dx - dy + ry2;
        }
    }

    // Region 2: slope > -1
    let mut d2 = ry2 * (x * x + x) + rx2 * (y - 1) * (y - 1) - rx2 * ry2;

    while y >= 0 {
        plot4(x, y);
        y -= 1;
        dy -= 2 * rx2;

        if d2 > 0 {
            d2 += rx2 - dy;
        } else {
            x += 1;
            dx += 2 * ry2;
            d2 += dx - dy + rx2;
        }
    }
}

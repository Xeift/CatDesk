#![allow(dead_code)]
// Complete character generation module - ported from v29_generator.py
use super::binagotchy_utils::*;
use super::constants::*;
use super::types::*;
use image::RgbaImage;
use rand::Rng;
use rand::seq::SliceRandom;

pub fn render_base_sprite<R: Rng>(
    canvas: u32,
    rng: &mut R,
    eyes_pref: &str,
    eye_openness: f32,
    tail_state: i32,
) -> (RgbaImage, Color, String) {
    // Pick fur color
    let fur_idx = weighted_choice(rng, &FUR_TRAITS.iter().map(|t| t.1).collect::<Vec<_>>());
    let fur_color = FUR_TRAITS[fur_idx].2;

    // Get mask and head box
    let (mask, head_box) = mask_sitting_cat(canvas, rng, tail_state);

    // Create outline ring
    let ring = outline_ring(&mask);

    // Start with transparent background (NOT filled with fur color!)
    let mut img = RgbaImage::from_pixel(canvas, canvas, rgba(0, 0, 0, 0));

    // Apply outline
    for y in 0..canvas {
        for x in 0..canvas {
            if ring.get_pixel(x, y)[0] > 0 {
                img.put_pixel(x, y, color_to_rgba(OUTLINE));
            }
        }
    }

    // Apply base fur color within mask
    for y in 0..canvas {
        for x in 0..canvas {
            if mask.get_pixel(x, y)[0] > 0 {
                img.put_pixel(x, y, color_to_rgba(fur_color));
            }
        }
    }

    // Add patterns
    add_pattern_all(&mut img, &mask, head_box, rng, fur_color);

    // Apply micro shading
    apply_micro_shading(&mut img, &mask, fur_color, rng);

    // Draw face features using ONLY normal/animated eyes logic
    let eye_color_name = draw_face_features(
        &mut img,
        canvas,
        rng,
        eyes_pref,
        head_box,
        eye_openness,
    );

    (img, fur_color, eye_color_name)
}

pub fn mask_sitting_cat<R: Rng>(
    canvas: u32,
    _rng: &mut R,
    tail_state: i32,
) -> (RgbaImage, (i32, i32, i32, i32)) {
    let mask_ascii = match tail_state {
        1 => SITTING_CAT_MASK_TAIL_UP,
        2 => SITTING_CAT_MASK_TAIL_DOWN,
        _ => SITTING_CAT_MASK_32,
    };
    let base_mask = mask_from_ascii(mask_ascii);

    let mask = if canvas == 32 {
        base_mask
    } else {
        image::imageops::resize(
            &base_mask,
            canvas,
            canvas,
            image::imageops::FilterType::Nearest,
        )
    };

    let sx = canvas as f32 / 32.0;
    let (x0, y0, x1, y1) = SITTING_CAT_HEAD_BOX_32;
    let head_box = (
        (x0 as f32 * sx).round() as i32,
        (y0 as f32 * sx).round() as i32,
        (x1 as f32 * sx).round() as i32,
        (y1 as f32 * sx).round() as i32,
    );

    (mask, head_box)
}

pub(super) fn face_center_x(canvas: u32) -> i32 {
    let sx = canvas as f32 / 32.0;
    (SITTING_CAT_FACE_CENTER_X_32 as f32 * sx).round() as i32
}

pub fn resolve_eye_mode<R: Rng>(rng: &mut R, eye_pref: &str) -> String {
    if eye_pref != "random" {
        return eye_pref.to_string();
    }

    let idx = weighted_choice(rng, EYE_MODE_WEIGHTS);
    EYE_MODES[idx].to_string()
}

pub fn resolve_headwear<R: Rng>(rng: &mut R, headwear_pref: &str) -> String {
    if headwear_pref != "random" {
        return headwear_pref.to_string();
    }

    let weights: Vec<f32> = HEADWEAR_TRAITS.iter().map(|t| t.1).collect();
    let idx = weighted_choice(rng, &weights);
    HEADWEAR_TRAITS[idx].0.to_string()
}

fn weighted_choice<R: Rng>(rng: &mut R, weights: &[f32]) -> usize {
    let total: f64 = weights.iter().map(|&w| w as f64).sum();
    let mut value = rng.gen_range(0.0..total);

    for (i, &weight) in weights.iter().enumerate() {
        value -= weight as f64;
        if value <= 0.0 {
            return i;
        }
    }
    weights.len() - 1
}

fn fill_with_mask(img: &mut RgbaImage, mask: &RgbaImage, color: Color) {
    let (w, h) = img.dimensions();
    for y in 0..h {
        for x in 0..w {
            if mask.get_pixel(x, y)[0] > 0 {
                img.put_pixel(x, y, color_to_rgba(color));
            }
        }
    }
}

fn add_pattern_all<R: Rng>(
    img: &mut RgbaImage,
    mask: &RgbaImage,
    head_box: (i32, i32, i32, i32),
    rng: &mut R,
    base: Color,
) {
    add_pattern_face_subtle(img, mask, rng, base);
    add_pattern_coat_subtle(img, mask, head_box, rng, base);
}

fn add_pattern_face_subtle<R: Rng>(
    img: &mut RgbaImage,
    mask: &RgbaImage,
    rng: &mut R,
    base: Color,
) {
    let mode = *choose_weighted(&["tabby", "patch", "none"], rng, |item| match *item {
        "tabby" => 0.60,
        "patch" => 0.25,
        _ => 0.15,
    })
    .unwrap();

    let mut overlay = RgbaImage::from_pixel(img.width(), img.height(), rgba(0, 0, 0, 0));

    if mode == "tabby" {
        let stripe = darken(base, 45);
        // Draw tabby stripes on face
        line(&mut overlay, 15, 10, 15, 11, stripe);
        line(&mut overlay, 14, 10, 15, 11, stripe);
        line(&mut overlay, 16, 10, 15, 11, stripe);
        pt(&mut overlay, 12, 15, stripe);
        pt(&mut overlay, 11, 16, stripe);
        pt(&mut overlay, 19, 15, stripe);
        pt(&mut overlay, 20, 16, stripe);
    } else if mode == "patch" {
        let patch_color = pick_patch_color(rng, base, PATCH_COLORS, 85.0, 60.0, 210);
        let place = *["earL", "earR", "cheekL", "cheekR"].choose(rng).unwrap();

        let (x0, y0) = match place {
            "earL" => (11, 7),
            "earR" => (18, 7),
            "cheekL" => (9, 14),
            _ => (21, 14),
        };

        let base_l = luminance(base);
        let (ww, hh) = if base_l >= 150.0 {
            (
                *choose_weighted(&[3, 4], rng, |&x| if x == 3 { 0.6 } else { 0.4 }).unwrap(),
                *choose_weighted(&[3, 4], rng, |&x| if x == 3 { 0.6 } else { 0.4 }).unwrap(),
            )
        } else {
            (
                *choose_weighted(&[3, 4, 5], rng, |&x| match x {
                    3 => 0.55,
                    4 => 0.35,
                    _ => 0.10,
                })
                .unwrap(),
                *choose_weighted(&[3, 4, 5], rng, |&x| match x {
                    3 => 0.55,
                    4 => 0.35,
                    _ => 0.10,
                })
                .unwrap(),
            )
        };

        draw_ellipse(&mut overlay, x0, y0, x0 + ww, y0 + hh, patch_color);

        if rng.gen_bool(0.40) {
            let speck = tone_match_patch(
                darken(with_alpha(patch_color, 255), 18),
                base,
                55.0,
                35.0,
                235.0,
                210,
            );
            pt(
                &mut overlay,
                x0 + rng.gen_range(0..=ww),
                y0 + rng.gen_range(0..=hh),
                speck,
            );
        }
    }

    let clipped = composite_clip(&overlay, mask);
    *img = alpha_composite(img, &clipped);
}

fn add_pattern_coat_subtle<R: Rng>(
    img: &mut RgbaImage,
    mask: &RgbaImage,
    head_box: (i32, i32, i32, i32),
    rng: &mut R,
    base: Color,
) {
    let (w, h) = (img.width() as i32, img.height() as i32);
    let mut overlay = RgbaImage::from_pixel(w as u32, h as u32, rgba(0, 0, 0, 0));

    let lum = luminance(base);
    let accent = if lum < 95.0 {
        lighten(base, 35)
    } else {
        darken(base, 50)
    };
    let accent2 = if lum < 95.0 {
        lighten(base, 22)
    } else {
        darken(base, 35)
    };

    let (_x0, _y0, _x1, y1) = head_box;
    let body_y0 = (y1 - 1).max(0);

    // Add specks - Use Hash Noise Map for stability
    let seed_base = rng.next_u64();
    for y in body_y0..(h - 2) {
        for x in 0..w {
            // Apply prob 0.02 (approx 12-20 specks in region)
            // Area approx 20x16 = 320 px. 0.02 * 320 = 6.4. Wait, 12-20 is high density.
            // Python used loop count 12-20 with retries. Mask area is small.
            // Let's use 0.05 prob.
            if stable_seed(&format!("speck_{}_{}", x, y), seed_base) % 100 < 5 {
                if mask.get_pixel(x as u32, y as u32)[0] > 0 {
                    pt(&mut overlay, x, y, accent2);
                    // Cluster chance
                    if stable_seed(&format!("cluster_{}_{}", x, y), seed_base) % 100 < 35 {
                        if x + 1 < w && mask.get_pixel((x + 1) as u32, y as u32)[0] > 0 {
                            pt(&mut overlay, x + 1, y, accent2);
                        }
                    }
                }
            }
        }
    }

    // Add stripes - Randomized but independent of mask
    if rng.gen_bool(0.75) {
        let stripe_n = rint(rng, 2, 4);
        for _ in 0..stripe_n {
            let y = rint(rng, (body_y0 + 2).min(h - 1), (body_y0 + 2).max(h - 6));
            let x = rint(rng, 8, 8.max(w - 10));
            // Just draw line. Mask will clip it later.
            let length = rint(rng, 4, 6);
            let dx = if rng.gen_bool(0.5) { 1 } else { -1 };
            let x2 = x + dx * length;
            let y2 = y + length / 2;
            line(&mut overlay, x, y, x2, y2, accent);
        }
    }

    // Add tail stripes - Using coordinate scan based on y to ensure stability
    if rng.gen_bool(0.65) {
        let ys: Vec<i32> = (0..2)
            .map(|_| rint(rng, (body_y0 + 6).min(h - 1), (body_y0 + 6).max(h - 4)))
            .collect();
        for &yy in &ys {
            for xx in (w - 11).max(0)..(w - 2) {
                // Check mask to apply paint, but RNG was already consumed for Ys
                if mask.get_pixel(xx as u32, yy as u32)[0] > 0 {
                    pt(&mut overlay, xx, yy, accent);
                }
            }
        }
    }

    // Add body patch - Single attempt, robust to mask
    if rng.gen_bool(0.22) {
        let patch_color = pick_patch_color(rng, base, PATCH_COLORS, 90.0, 60.0, 210);
        let px = rint(rng, 10, 16);
        let py = rint(rng, (body_y0 + 4).min(h - 4), (body_y0 + 4).max(h - 4));
        // Draw ellipse blindly, let compositing clip it
        draw_ellipse(&mut overlay, px, py, px + 3, py + 3, patch_color);
    }

    let clipped = composite_clip(&overlay, mask);
    *img = alpha_composite(img, &clipped);
}

fn apply_micro_shading<R: Rng>(img: &mut RgbaImage, mask: &RgbaImage, base: Color, rng: &mut R) {
    let mut overlay = RgbaImage::from_pixel(img.width(), img.height(), rgba(0, 0, 0, 0));

    let hx = rint(rng, 10, 14);
    let hy = rint(rng, 10, 13);
    draw_rect(&mut overlay, hx, hy, hx + 1, hy + 1, lighten(base, 18));

    let sx = rint(rng, 16, 20);
    let sy = rint(rng, 16, 20);
    draw_rect(&mut overlay, sx, sy, sx + 1, sy + 1, darken(base, 20));

    let clipped = composite_clip(&overlay, mask);
    *img = alpha_composite(img, &clipped);
}

fn draw_face_features<R: Rng>(
    img: &mut RgbaImage,
    canvas: u32,
    rng: &mut R,
    _eyes_pref: &str,
    head_box: (i32, i32, i32, i32),
    eye_openness: f32,
) -> String {
    let (_, y0, _, _y1) = head_box;
    let cx = face_center_x(canvas);
    let _ = rint(rng, 4, 5); // Consume RNG
    let eye_dx = 4; // Fixed to 4px
    let eye_y = y0 + rint(rng, 2, 3);
    let mouth_y = eye_y + 7;

    // Pick random eye color
    let color_idx = rng.gen_range(0..EYE_TRAITS.len());
    let (color_name, eye_color) = EYE_TRAITS[color_idx];

    // Draw eyes using animation system
    use crate::binagotchy_gen::eye_animation::draw_animated_eyes;
    draw_animated_eyes(img, cx, eye_y, eye_dx, eye_color, eye_openness);

    // Draw nose
    pt(img, cx, mouth_y - 2, NOSE);
    pt(img, cx - 1, mouth_y - 1, OUTLINE);
    pt(img, cx + 1, mouth_y - 1, OUTLINE);
    pt(img, cx, mouth_y, OUTLINE);

    // Draw whiskers
    draw_whiskers(img, cx, mouth_y);

    color_name.to_string()
}

fn draw_whiskers(img: &mut RgbaImage, cx: i32, mouth_y: i32) {
    let contact_dx = 6;
    let whisker_len = 5;
    let y_upper = mouth_y - 2;
    let y_lower = mouth_y;

    for s in [-1, 1] {
        let x_contact = cx + s * contact_dx;

        pt(img, x_contact, y_upper, OUTLINE);
        line(
            img,
            x_contact + s * 1,
            y_upper - 1,
            x_contact + s * (1 + whisker_len),
            y_upper - 2,
            OUTLINE,
        );

        pt(img, x_contact, y_lower, OUTLINE);
        line(
            img,
            x_contact + s * 1,
            y_lower + 1,
            x_contact + s * (1 + whisker_len),
            y_lower + 2,
            OUTLINE,
        );
    }
}

fn draw_eyes<R: Rng>(
    img: &mut RgbaImage,
    cx: i32,
    eye_y: i32,
    eye_dx: i32,
    rng: &mut R,
    eye_mode: &str,
) {
    let eye_c = EYES[rng.gen_range(0..EYES.len())];
    let heart_fill = (225, 80, 110, 255);
    let heart_highlight = (255, 180, 200, 255);

    match eye_mode {
        "pixel" => {
            for ex in [cx - eye_dx, cx + eye_dx] {
                pt(img, ex, eye_y + 1, EYE_LN);
            }
        }
        "chibi_black" => {
            for ex in [cx - eye_dx, cx + eye_dx] {
                for yy in 0..2 {
                    pt(img, ex, eye_y + yy, EYE_LN);
                    pt(img, ex + 1, eye_y + yy, EYE_LN);
                }
                pt(img, ex, eye_y, WHITE);
            }
        }
        "closed_u" => {
            for ex in [cx - eye_dx, cx + eye_dx] {
                if ex < cx {
                    line(img, ex - 1, eye_y - 1, ex + 1, eye_y - 2, OUTLINE);
                } else {
                    line(img, ex - 1, eye_y - 2, ex + 1, eye_y - 1, OUTLINE);
                }

                pt(img, ex - 1, eye_y + 1, EYE_LN);
                pt(img, ex, eye_y + 2, EYE_LN);
                pt(img, ex + 1, eye_y + 2, EYE_LN);
                pt(img, ex + 2, eye_y + 1, EYE_LN);
            }
        }
        "sleepy" => {
            for ex in [cx - eye_dx, cx + eye_dx] {
                line(img, ex, eye_y + 1, ex + 2, eye_y + 1, EYE_LN);
            }
        }
        "heart" => {
            for ex in [cx - eye_dx, cx + eye_dx] {
                draw_heart(img, ex, eye_y, heart_fill, heart_highlight);
            }
        }
        _ => {
            // normal
            for ex in [cx - eye_dx, cx + eye_dx] {
                for yy in 0..3 {
                    pt(img, ex, eye_y + yy, EYE_LN);
                    pt(img, ex + 1, eye_y + yy, EYE_LN);
                }
                pt(img, ex, eye_y, WHITE);
                pt(img, ex, eye_y + 2, eye_c);
            }
        }
    }
}

fn draw_heart(img: &mut RgbaImage, xc: i32, yc: i32, fill: Color, highlight: Color) {
    let pattern = [".h.f.", "hffff", "fffff", ".fff.", "..f.."];

    let h0 = pattern.len() as i32;
    let w0 = pattern[0].len() as i32;
    let x0 = xc - w0 / 2;
    let y0 = yc - h0 / 2;

    for (yy, row) in pattern.iter().enumerate() {
        for (xx, ch) in row.chars().enumerate() {
            match ch {
                'f' => pt(img, x0 + xx as i32, y0 + yy as i32, fill),
                'h' => pt(img, x0 + xx as i32, y0 + yy as i32, highlight),
                _ => {}
            }
        }
    }
}

// Helper: choose from weighted slices using standard rand
// use rand::distributions::WeightedIndex;

fn choose_weighted<'a, T, R, F>(slice: &'a [T], rng: &mut R, weight_fn: F) -> Option<&'a T>
where
    R: Rng,
    F: Fn(&T) -> f32,
{
    let weights: Vec<f32> = slice.iter().map(|item| weight_fn(item)).collect();
    if weights.is_empty() {
        return None;
    }
    let idx = weighted_choice(rng, &weights);
    Some(&slice[idx])
}
